// Client: HTTP calls to chat.deepseek.com API + completion flow
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{json, Value};

use crate::pow::solve_pow;

pub const DEEPSEEK_API: &str = "https://chat.deepseek.com/api/v0";

/// Timeout mỗi lần đọc socket (DS_READ_TIMEOUT_SECS, mặc định 120s).
/// Phải > STALL_SECS của stream để lỗi stall vẫn được báo qua StreamEv::Truncated thay vì transport error.
fn read_timeout_secs() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        crate::settings::uint("DS_READ_TIMEOUT_SECS")
    })
}

/// Giải PoW trên blocking pool: đây là CPU-bound (có thể vài trăm ms) — chạy thẳng trong
/// async fn sẽ chặn tokio worker và làm chậm mọi request khác đang cùng runtime.
async fn solve_pow_async(challenge_hex: String, salt: String, expire_at: u64, difficulty: u64) -> Option<u64> {
    tokio::task::spawn_blocking(move || solve_pow(&challenge_hex, &salt, expire_at, difficulty))
        .await
        .ok()
        .flatten()
}

/// Bộ nhớ PoW tải sẵn theo token: Pending = đang tải (tránh tải trùng), Ready = header dùng được tới hạn.
enum PowSlot { Pending, Ready(std::time::Instant, String) }
const POW_MAX_AGE_SECS: u64 = 240; // challenge sống 300s — chừa biên

fn pow_slots() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, PowSlot>> {
    static S: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, PowSlot>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn pow_cache_take(token: &str) -> Option<String> {
    let mut m = pow_slots();
    match m.remove(token) {
        Some(PowSlot::Ready(t, h)) if t.elapsed().as_secs() < POW_MAX_AGE_SECS => { eprintln!("[pow] dùng bản tải sẵn"); Some(h) }
        Some(PowSlot::Pending) => { m.insert(token.to_string(), PowSlot::Pending); None }
        _ => None,
    }
}

/// true nếu được quyền tải (chưa có Pending/Ready còn tươi).
fn pow_cache_claim(token: &str) -> bool {
    let mut m = pow_slots();
    match m.get(token) {
        Some(PowSlot::Pending) => false,
        Some(PowSlot::Ready(t, _)) if t.elapsed().as_secs() < POW_MAX_AGE_SECS => false,
        _ => { m.insert(token.to_string(), PowSlot::Pending); true }
    }
}

fn pow_cache_put(token: &str, header: String) { pow_slots().insert(token.to_string(), PowSlot::Ready(std::time::Instant::now(), header)); }
fn pow_cache_release(token: &str) { let mut m = pow_slots(); if matches!(m.get(token), Some(PowSlot::Pending)) { m.remove(token); } }

/// Event từ stream DS — server push thẳng ra SSE OpenAI
pub enum StreamEv {
    Thinking(String),
    Response(String),
    MsgId(i64),         // response_message_id
    Usage(u64),         // accumulated_token_usage thật từ web (prompt + reasoning + output)
    Truncated(String),  // stream bị cắt giữa chừng (stall/lỗi mạng) — gửi ngay trước Done
    Done,               // stream kết thúc (payload SSE đầy đủ không còn ai dùng)
}


#[derive(Clone)]
pub struct DsClient {
    http: reqwest::Client,
    token: String,
    cookies: String,
    /// ổn định theo từng acc (web thật giữ 1 device id cố định / trình duyệt)
    device_id: String,
}

/// UUID-dạng ổn định dẫn xuất từ token (FNV-1a 128 bit giả lập bằng 2 lần băm) — mỗi acc một "thiết bị".
fn device_id_for(token: &str) -> String {
    let h = |seed: u64| token.bytes().fold(seed, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    let (a, b) = (h(0xcbf29ce484222325), h(0x84222325cbf29ce4));
    format!("{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}", (a >> 32) as u32, (a >> 16) as u16, a as u16 & 0xfff, (b >> 52) as u16 & 0xfff, b & 0xffff_ffff_ffff)
}

/// Lệch múi giờ tính bằng giây như web (UTC+7 → 25200). Đặt DS_TZ_OFFSET để đổi.
fn tz_offset_secs() -> String {
    crate::settings::get("DS_TZ_OFFSET")
}

impl DsClient {
    pub fn new(token: String, cookies: String) -> Result<Self> {
        // Một reqwest::Client (connection pool) cho mỗi acc: tái dùng TCP/TLS/HTTP2 giữa các request thay vì
        // bắt tay lại mỗi lần, và mỗi acc vẫn có kết nối riêng như một trình duyệt riêng.
        static POOL: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, reqwest::Client>>> = std::sync::OnceLock::new();
        let pool = POOL.get_or_init(Default::default);
        let cached = pool.lock().unwrap_or_else(|e| e.into_inner()).get(&token).cloned();
        let http = match cached {
            Some(c) => c,
            None => {
                let c = reqwest::Client::builder()
                    .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36")
                    // Chặn treo vô hạn: DS chập chờn mà không có timeout thì request giữ Lease mãi → cạn pool.
                    // KHÔNG dùng `.timeout()` toàn cục: nó tính cả thời gian đọc body, sẽ giết stream chat dài.
                    // `connect_timeout` chặn lúc bắt tay TCP/TLS; `read_timeout` chặn từng lần đọc socket
                    // (gồm cả lúc chờ header) nên stream vẫn sống miễn DS gửi byte đều đặn.
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .read_timeout(std::time::Duration::from_secs(read_timeout_secs()))
                    .pool_idle_timeout(std::time::Duration::from_secs(60))
                    .build()?;
                pool.lock().unwrap_or_else(|e| e.into_inner()).insert(token.clone(), c.clone());
                c
            }
        };
        let device_id = device_id_for(&token);
        Ok(Self { http, token, cookies, device_id })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", DEEPSEEK_API, path))
            .header("authorization", format!("Bearer {}", self.token))
            .header("cookie", &self.cookies)
            .header("origin", "https://chat.deepseek.com")
            .header("referer", "https://chat.deepseek.com/")
            // Bộ header web thật (2.7.0) gửi kèm mọi call /api/v0 — thiếu là dấu hiệu "không phải trình duyệt"
            .header("accept", "*/*")
            .header("accept-language", "vi,en-US;q=0.9,en;q=0.8")
            .header("sec-ch-ua", "\"Google Chrome\";v=\"155\", \"Chromium\";v=\"155\", \"Not(A:Brand\";v=\"24\"")
            .header("sec-ch-ua-mobile", "?0")
            .header("sec-ch-ua-platform", "\"Linux\"")
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-origin")
            .header("x-client-platform", "web")
            .header("x-client-version", "2.7.0")
            .header("x-client-locale", "vi")
            .header("x-client-bundle-id", "com.deepseek.chat")
            .header("x-client-timezone-offset", tz_offset_secs())
            .header("x-device-model", "")
            .header("x-device-id", &self.device_id)
    }

    pub async fn users_current(&self) -> Result<Value> {
        let v: Value = self
            .req(reqwest::Method::GET, "/users/current")
            .send()
            .await?
            .json()
            .await?;
        Ok(v)
    }

    pub async fn create_session(&self) -> Result<String> {
        let v: Value = self
            .req(reqwest::Method::POST, "/chat_session/create")
            .json(&json!({}))
            .send()
            .await?
            .json()
            .await?;
        Self::check_auth(&v)?;
        Ok(v["data"]["biz_data"]["chat_session"]["id"]
            .as_str()
            .context("no session id")?
            .to_string())
    }

    /// Xoá session web (best-effort — endpoint chưa kiểm chứng trên mọi phiên bản web).
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let v: Value = self
            .req(reqwest::Method::POST, "/chat_session/delete")
            .json(&json!({ "chat_session_id": session_id }))
            .send()
            .await?
            .json()
            .await?;
        if v["code"].as_i64().unwrap_or(0) != 0 {
            anyhow::bail!("delete_session: {}", v);
        }
        Ok(())
    }

    /// Lỗi xác thực của DS web: HTTP 200 nhưng `code` 40003 ("invalid token") — phải đăng nhập lại, không retry mù.
    fn check_auth(v: &Value) -> Result<()> {
        if v["code"].as_i64() == Some(40003) {
            anyhow::bail!("AUTH_INVALID: {}", v["msg"].as_str().unwrap_or("invalid token"));
        }
        Ok(())
    }

    async fn fetch_pow_challenge_for(&self, target_path: &str) -> Result<Value> {
        let v: Value = self
            .req(reqwest::Method::POST, "/chat/create_pow_challenge")
            .json(&json!({ "target_path": target_path }))
            .send()
            .await?
            .json()
            .await?;
        Self::check_auth(&v)?;
        Ok(v["data"]["biz_data"]["challenge"].clone())
    }

    async fn fetch_pow_challenge(&self) -> Result<Value> {
        self.fetch_pow_challenge_for("/api/v0/chat/completion").await
    }

    /// PoW cho /chat/completion: dùng bản đã tải sẵn nếu còn tươi (challenge sống 300s), không thì giải mới.
    async fn completion_pow(&self) -> Result<String> {
        if let Some(h) = pow_cache_take(&self.token) {
            return Ok(h);
        }
        self.pow_header("/api/v0/chat/completion").await
    }

    /// Tải + giải sẵn PoW cho request kế tiếp của acc này, chạy nền trong lúc stream hiện tại đang chạy.
    /// Mỗi challenge chỉ dùng một lần. DS_POW_PREFETCH=0 để tắt.
    fn prefetch_pow(&self) {
        if !crate::settings::boolean("DS_POW_PREFETCH") || !pow_cache_claim(&self.token) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            match me.pow_header("/api/v0/chat/completion").await {
                Ok(h) => pow_cache_put(&me.token, h),
                Err(_) => pow_cache_release(&me.token),
            }
        });
    }

    /// Lấy challenge cho `target_path`, giải PoW, trả header `x-ds-pow-response` (base64).
    async fn pow_header(&self, target_path: &str) -> Result<String> {
        let ch = self.fetch_pow_challenge_for(target_path).await?;
        let challenge_hex = ch["challenge"].as_str().context("no challenge")?.to_string();
        let salt = ch["salt"].as_str().context("no salt")?.to_string();
        let expire_at = ch["expire_at"].as_u64().context("no expire_at")?;
        let difficulty = ch["difficulty"].as_u64().context("no difficulty")?;
        let signature = ch["signature"].as_str().context("no signature")?.to_string();
        let t0 = std::time::Instant::now();
        let answer = solve_pow_async(challenge_hex.clone(), salt.clone(), expire_at, difficulty).await
            .context("PoW solve failed within difficulty bound")?;
        eprintln!("[pow] difficulty={} answer={} solve_ms={}", difficulty, answer, t0.elapsed().as_millis());
        let payload = json!({
            "algorithm": ch["algorithm"], "challenge": challenge_hex, "salt": salt,
            "answer": answer, "signature": signature, "target_path": ch["target_path"],
        });
        Ok(base64::engine::general_purpose::STANDARD.encode(payload.to_string()))
    }

    /// Upload ảnh/file lên DS web (multipart + PoW riêng cho upload_file), chờ xử lý xong, trả file id.
    pub async fn upload_file(&self, bytes: Vec<u8>, filename: &str, mime: &str) -> Result<String> {
        let pow = self.pow_header("/api/v0/file/upload_file").await?;
        let size = bytes.len();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime)?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let v: Value = self
            .req(reqwest::Method::POST, "/file/upload_file")
            .header("x-ds-pow-response", pow)
            .header("x-file-size", size.to_string())
            .header("x-model-type", "default")
            .multipart(form)
            .send()
            .await?
            .json()
            .await?;
        Self::check_auth(&v)?;
        let id = v["data"]["biz_data"]["id"].as_str()
            .with_context(|| format!("upload_file thất bại: {}", v))?.to_string();
        // PENDING → SUCCESS (~2s với ảnh nhỏ)
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let f: Value = self
                .req(reqwest::Method::GET, &format!("/file/fetch_files?file_ids={}", id))
                .send().await?.json().await?;
            let st = f["data"]["biz_data"]["files"][0]["status"].as_str().unwrap_or("");
            match st {
                "SUCCESS" => return Ok(id),
                "FAILED" | "ERROR" | "CONTENT_EMPTY" | "UNSAFE" => {
                    anyhow::bail!("DS từ chối file: status={} err={}", st, f["data"]["biz_data"]["files"][0]["error_code"])
                }
                _ => {}
            }
        }
        anyhow::bail!("upload_file quá thời gian chờ xử lý ({})", id)
    }

    /// Xác thực nhẹ: token còn sống không (khác với cookie WAF).
    pub async fn token_alive(&self) -> Result<bool> {
        let v = self.users_current().await?;
        Ok(v["code"].as_i64() == Some(0))
    }

    /// Full flow: challenge -> solve -> completion (SSE). Returns (session_id, sse, last_message_id).
    /// Khi parent_message_id = Some(n) thì chat TIẾP trong cùng thread (không tạo conversation mới).
    pub async fn chat(
        &self,
        prompt: &str,
        session_id: Option<&str>,
        parent_message_id: Option<i64>,
        thinking: bool,
        search: bool,
    ) -> Result<(String, String, Option<i64>)> {
        let sid = match session_id {
            Some(s) => s.to_string(),
            None => self.create_session().await?,
        };

        let ch = self.fetch_pow_challenge().await?;
        let challenge_hex = ch["challenge"].as_str().context("no challenge")?.to_string();
        let salt = ch["salt"].as_str().context("no salt")?.to_string();
        let expire_at = ch["expire_at"].as_u64().context("no expire_at")?;
        let difficulty = ch["difficulty"].as_u64().context("no difficulty")?;
        let signature = ch["signature"].as_str().context("no signature")?.to_string();

        let t0 = std::time::Instant::now();
        let answer = solve_pow_async(challenge_hex.clone(), salt.clone(), expire_at, difficulty).await
            .context("PoW solve failed within difficulty bound")?;
        let solve_ms = t0.elapsed().as_millis();

        let pow_payload = json!({
            "algorithm": ch["algorithm"],
            "challenge": challenge_hex,
            "salt": salt,
            "answer": answer,
            "signature": signature,
            "target_path": ch["target_path"],
        });
        let pow_header = base64::engine::general_purpose::STANDARD.encode(pow_payload.to_string());

        eprintln!(
            "[pow] difficulty={} answer={} solve_ms={}",
            difficulty, answer, solve_ms
        );

        let resp = self
            .req(reqwest::Method::POST, "/chat/completion")
            .header("x-ds-pow-response", pow_header)
            .json(&json!({
                "chat_session_id": sid,
                "parent_message_id": parent_message_id,
                "ref_file_ids": [],
                "thinking_enabled": thinking,
                "search_enabled": search,
                "prompt": prompt,
            }))
            .send()
            .await?;

        let status = resp.status();
        let full = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!("completion {} -> {}", status, String::from_utf8_lossy(&full));
        }
        let sse = String::from_utf8_lossy(&full).to_string();

        // debug: dump toàn bộ SSE raw (env DS_DEBUG_SSE=1)
        if crate::settings::boolean("DS_DEBUG_SSE") {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
                .open("/tmp/ds_debug_sse.log") {
                use std::io::Write;
                let _ = writeln!(f, "===== {} =====\n{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), sse);
            }
        }

        // Lấy response_message_id từ event `ready` để lần sau nối tiếp thread
        let mut last_msg_id = None;
        for line in sse.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if let Some(rid) = v.get("response_message_id").and_then(|x| x.as_i64()) {
                        last_msg_id = Some(rid);
                    }
                }
            }
            // event: ready line chứa data trên dòng kế — regex đơn giản qua header ready
            if line.starts_with("event: ready") {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: {\"request_message_id\"") {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    last_msg_id = v.get("response_message_id").and_then(|x| x.as_i64());
                }
            }
        }
        Ok((sid, sse, last_msg_id))
    }

/// Stream real-time: trả receiver event; call non-blocking cho server pump.
pub async fn chat_stream(
    &self,
    prompt: &str,
    session_id: Option<&str>,
    parent_message_id: Option<i64>,
    thinking: bool,
    search: bool,
    ref_file_ids: &[String],
) -> Result<(String, tokio::sync::mpsc::UnboundedReceiver<StreamEv>)> {
    use tokio::sync::mpsc;
    use futures::StreamExt;

    let t_req = std::time::Instant::now(); // mốc từ lúc nhận request (tạo session + PoW + chờ header đều tính)
    let sid = match session_id {
        Some(s) => s.to_string(),
        None => self.create_session().await?,
    };

    let pow_header = self.completion_pow().await?;

    let resp = self
        .req(reqwest::Method::POST, "/chat/completion")
        .header("x-ds-pow-response", pow_header)
        .json(&json!({
            "chat_session_id": sid,
            "parent_message_id": parent_message_id,
            "model_type": "default",
            "ref_file_ids": ref_file_ids,
            "thinking_enabled": thinking,
            "search_enabled": search,
            "action": null,
            "preempt": false,
            "prompt": prompt,
        }))
        .send()
        .await?;

    let status = resp.status();
    if status.is_success() {
        self.prefetch_pow(); // chạy nền trong lúc stream, phục vụ request kế tiếp của acc này
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("completion {} -> {}", status, body);
    }

    let (tx, rx) = mpsc::unbounded_channel::<StreamEv>();
    tokio::spawn(async move {
        let mut stream = std::pin::pin!(resp.bytes_stream());
        let mut buf = String::new();
        let mut sse_chars: usize = 0; // chỉ đếm, không giữ toàn bộ SSE trong RAM
        let mut cur_type = String::new();
        let mut has_content = false;
        let mut sent_msg_id = false;

        let t0 = t_req;
        let mut first_byte_ms: Option<u128> = None;
        let mut total_bytes: usize = 0;
        let mut truncated: Option<String> = None;
        let mut raw: Vec<u8> = Vec::new(); // byte dư của ký tự UTF-8 bị cắt giữa 2 chunk (vd '｜' 3 byte)
        // DS ngừng gửi (không byte nào trong STALL_SECS) → coi là treo, thoát để server retry thay vì chờ vô hạn
        const STALL_SECS: u64 = 90;
        loop {
            let chunk = match tokio::time::timeout(std::time::Duration::from_secs(STALL_SECS), stream.next()).await {
                Err(_) => { eprintln!("[ds-stall] không có dữ liệu {}s (sau {}ms, {} bytes) — bỏ stream", STALL_SECS, t0.elapsed().as_millis(), total_bytes); truncated = Some(format!("DeepSeek ngừng gửi dữ liệu {}s", STALL_SECS)); break; }
                Ok(None) => break,
                Ok(Some(Err(e))) => { eprintln!("[ds-stream-err] {} (sau {}ms, {} bytes)", e, t0.elapsed().as_millis(), total_bytes); truncated = Some(format!("stream lỗi giữa chừng: {}", e)); break; }
                Ok(Some(Ok(c))) => c,
            };
            first_byte_ms.get_or_insert_with(|| t0.elapsed().as_millis());
            total_bytes += chunk.len();
            raw.extend_from_slice(&chunk);
            match std::str::from_utf8(&raw) {
                Ok(t) => { buf.push_str(t); raw.clear(); }
                Err(e) => {
                    let ok = e.valid_up_to();
                    buf.push_str(std::str::from_utf8(&raw[..ok]).unwrap_or(""));
                    if e.error_len().is_none() {
                        raw.drain(..ok); // cuối chunk là ký tự dở — chờ chunk sau
                    } else {
                        buf.push_str(&String::from_utf8_lossy(&raw[ok..]));
                        raw.clear();
                    }
                }
            }
            // xử lý từng dòng hoàn chỉnh
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                let rest = buf[pos + 1..].to_string();
                buf = rest;
                let Some(data) = line.strip_prefix("data: ") else { continue };
                sse_chars += line.chars().count() + 1;
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };

                // token thật: {"p":"response","o":"BATCH","v":[{"p":"accumulated_token_usage","v":346},...]}
                if let Some(arr) = v.get("v").and_then(|x| x.as_array()) {
                    for it in arr {
                        if it.get("p").and_then(|p| p.as_str()) == Some("accumulated_token_usage") {
                            if let Some(n) = it.get("v").and_then(|x| x.as_u64()) { let _ = tx.send(StreamEv::Usage(n)); }
                        }
                    }
                }
                // msg id
                if !sent_msg_id {
                    if let Some(rid) = v.get("response_message_id").and_then(|x| x.as_i64()) {
                        let _ = tx.send(StreamEv::MsgId(rid));
                        sent_msg_id = true;
                    }
                }

                // init object: fragments array
                if let Some(frags) = v.get("v").and_then(|x| x.get("response")).and_then(|r| r.get("fragments")).and_then(|f| f.as_array()) {
                    if let Some(last) = frags.last() {
                        cur_type = last["type"].as_str().unwrap_or("").to_string();
                        if cur_type == "RESPONSE" {
                            has_content = true;
                            if let Some(c) = last["content"].as_str() {
                                let _ = tx.send(StreamEv::Response(c.to_string()));
                            }
                        } else if cur_type == "THINK" {
                            if let Some(c) = last["content"].as_str() {
                                let _ = tx.send(StreamEv::Thinking(c.to_string()));
                            }
                        }
                    }
                    continue;
                }
                // fragment mới APPEND
                if v.get("p").and_then(|p| p.as_str()) == Some("response/fragments") {
                    if v.get("o").and_then(|o| o.as_str()) == Some("APPEND") {
                        if let Some(arr) = v.get("v").and_then(|x| x.as_array()) {
                            for f in arr {
                                cur_type = f["type"].as_str().unwrap_or("").to_string();
                                if cur_type == "RESPONSE" {
                                    has_content = true;
                                    if let Some(c) = f["content"].as_str() {
                                        let _ = tx.send(StreamEv::Response(c.to_string()));
                                    }
                                } else if cur_type == "THINK" {
                                    if let Some(c) = f["content"].as_str() {
                                        let _ = tx.send(StreamEv::Thinking(c.to_string()));
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
                // append delta vào fragment active (có path)
                if let Some(p) = v.get("p").and_then(|p| p.as_str()) {
                    if p.ends_with("/content") {
                        if let Some(c) = v.get("v").and_then(|x| x.as_str()) {
                            if cur_type == "RESPONSE" {
                                has_content = true;
                                let _ = tx.send(StreamEv::Response(c.to_string()));
                            } else if cur_type == "THINK" {
                                let _ = tx.send(StreamEv::Thinking(c.to_string()));
                            }
                        }
                    }
                    continue;
                }
                // bare {"v":"text"} → continuation của fragment active (case web trả phần lớn chữ)
                if cur_type == "RESPONSE" {
                    if let Some(c) = v.get("v").and_then(|x| x.as_str()) {
                        has_content = true;
                        let _ = tx.send(StreamEv::Response(c.to_string()));
                    }
                } else if cur_type == "THINK" {
                    if let Some(c) = v.get("v").and_then(|x| x.as_str()) {
                        let _ = tx.send(StreamEv::Thinking(c.to_string()));
                    }
                }
            }
        }
        eprintln!("[ds-timing] first_byte={}ms total={}ms bytes={} sse_chars={}", first_byte_ms.unwrap_or(0), t0.elapsed().as_millis(), total_bytes, sse_chars);
        if !has_content {
            eprintln!("[ds-stream] stream không có fragment RESPONSE nào (nghi throttle/rỗng)");
        }
        if let Some(r) = truncated { let _ = tx.send(StreamEv::Truncated(r)); }
        let _ = tx.send(StreamEv::Done);
    });

    Ok((sid, rx))
}

}

#[cfg(test)]
mod header_tests {
    #[test]
    fn device_id_is_stable_uuid_shape() {
        let a = super::device_id_for("tok-a");
        assert_eq!(a, super::device_id_for("tok-a"));
        assert_ne!(a, super::device_id_for("tok-b"));
        assert_eq!(a.len(), 36);
        assert_eq!(a.matches('-').count(), 4);
    }
}
