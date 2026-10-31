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
        Some(PowSlot::Ready(t, h)) if t.elapsed().as_secs() < POW_MAX_AGE_SECS => { crate::elog!("[pow] dùng bản tải sẵn"); Some(h) }
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
fn pow_cache_release_any(token: &str) { pow_slots().remove(token); }
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
    http: wreq::Client,
    token: String,
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

/// DS từ chối chính FILE (không phải acc): ảnh quá lớn, định dạng lạ, nội dung bị đánh dấu unsafe,
/// hoặc server trả 50404. Loại lỗi này lặp lại y hệt trên mọi acc, nên đổi acc là vô ích.
#[derive(Debug)]
pub struct FileRejected {
    pub status: String,
    pub code: String,
}

impl std::fmt::Display for FileRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DS từ chối file: status={} err={}", self.status, self.code)
    }
}

impl std::error::Error for FileRejected {}

/// Nhãn nhận dạng lỗi file-bị-từ-chối khi nó đã bị bọc trong chuỗi `anyhow` (mất kiểu cụ thể).
/// Dùng chung một chuỗi với `Display` ở trên để hai bên không lệch nhau.
pub const FILE_REJECTED_TAG: &str = "DS từ chối file:";

/// Lỗi của một lần upload: đã bị DS từ chối vì bản thân file (không phải vì acc).
pub fn is_file_rejected(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<FileRejected>().is_some())
        || format!("{:#}", e).contains(FILE_REJECTED_TAG)
}

/// Mã lỗi mà DS trả về khi từ chối chính file. 50404 là mã thực tế gặp trên /file/upload_file
/// (ảnh chụp màn hình 720x1600, PNG bình thường vẫn bị trả về mã này — DS chặn theo nội dung/kích thước
/// chứ không theo mime). Danh sách để mở rộng khi gặp thêm mã; mã lạ vẫn rơi về nhánh "lỗi acc".
pub fn is_file_upload_rejection(code: &str) -> bool {
    matches!(code, "50404" | "50403" | "50405" | "40011" | "40012" | "UNSAFE" | "CONTENT_EMPTY")
}

/// Giá trị `x-hif-leim` / `x-hif-dliq` web lấy từ hif-*.deepseek.com/query (không token) rồi cache theo `x-hif-ttl`
/// (mặc định 600s); web gắn chúng vào mọi request SSE (`addSSEHeader`) nên bản thiếu chúng khác hẳn trình duyệt.
/// Body của /chat/completion đúng thứ tự key và đủ trường như web gửi (serde_json::json! sẽ xếp key theo alphabet).
#[derive(serde::Serialize)]
struct CompletionBody<'a> {
    chat_session_id: &'a str,
    parent_message_id: Option<i64>,
    model_type: &'static str,
    prompt: &'a str,
    ref_file_ids: &'a [String],
    thinking_enabled: bool,
    search_enabled: bool,
    action: Option<()>,
    preempt: bool,
}

impl<'a> CompletionBody<'a> {
    fn new(sid: &'a str, parent: Option<i64>, prompt: &'a str, refs: &'a [String], thinking: bool, search: bool) -> Self {
        Self { chat_session_id: sid, parent_message_id: parent, model_type: "default", prompt, ref_file_ids: refs, thinking_enabled: thinking, search_enabled: search, action: None, preempt: false }
    }
    fn into_body(self) -> Vec<u8> { serde_json::to_vec(&self).unwrap_or_default() }
}

struct Slot { client: wreq::Client, jar: std::sync::Arc<wreq::cookie::Jar>, seeded: std::sync::Mutex<std::collections::HashMap<String, String>> }

fn slot_pool() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Slot>>> {
    static POOL: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Slot>>>> = std::sync::OnceLock::new();
    POOL.get_or_init(Default::default)
}

/// Cookie hiện có trong jar của acc (gồm cả những cái DS vừa đặt qua Set-Cookie) để ghi lại vào store.
pub fn jar_snapshot(token: &str) -> Option<std::collections::BTreeMap<String, String>> {
    let slot = slot_pool().lock().unwrap_or_else(|e| e.into_inner()).get(token).cloned()?;
    let m: std::collections::BTreeMap<String, String> = slot.jar.get_all().map(|c| (c.name().to_string(), c.value().to_string())).collect();
    if m.is_empty() { None } else { Some(m) }
}

/// Một giá trị HIF: còn hiệu lực tới `until`; lỗi thì thử lại theo backoff 1s,2s,4s… tối đa 600s như poller của web.
struct HifSlot {
    value: Option<String>,
    until: std::time::Instant,
    retry_at: std::time::Instant,
    fails: u32,
}

impl HifSlot {
    fn empty() -> Self {
        let now = std::time::Instant::now();
        Self { value: None, until: now, retry_at: now, fails: 0 }
    }
    fn due(&self) -> bool {
        let now = std::time::Instant::now();
        (self.value.is_none() || now >= self.until) && now >= self.retry_at
    }
    fn ok(&mut self, v: String, ttl: std::time::Duration) {
        let now = std::time::Instant::now();
        self.value = Some(v);
        self.until = now + ttl;
        self.retry_at = now;
        self.fails = 0;
    }
    fn fail(&mut self) {
        // web chỉ gửi giá trị còn hiệu lực: hết hạn mà chưa làm mới được thì bỏ
        if std::time::Instant::now() >= self.until { self.value = None; }
        self.fails = self.fails.saturating_add(1);
        let secs = (1u64 << self.fails.min(10)).min(600);
        self.retry_at = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    }
    fn current(&self) -> Option<String> {
        if std::time::Instant::now() < self.until { self.value.clone() } else { None }
    }
}

struct HifEntry { leim: HifSlot, dliq: HifSlot }

fn hif_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, HifEntry>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, HifEntry>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

impl DsClient {
    /// Lấy một giá trị HIF đúng như poller của web: preflight CORS (OPTIONS — server không đặt max-age nên lần nào cũng
    /// có), rồi GET không token kèm bộ header x-client-*, qua đúng client (đúng vân tay TLS) của acc. Timeout 3s như web.
    async fn fetch_hif(&self, host: &str) -> Option<(String, std::time::Duration)> {
        let url = format!("https://{}.deepseek.com/query", host);
        let t3 = std::time::Duration::from_secs(3);
        let _ = self.http
            .request(wreq::Method::OPTIONS, &url)
            .header("accept", "*/*")
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "x-client-bundle-id,x-client-locale,x-client-platform,x-client-timezone-offset,x-client-version")
            .header("origin", "https://chat.deepseek.com")
            .header("referer", "https://chat.deepseek.com/")
            .header("accept-language", "vi,en-US;q=0.9,en;q=0.8")
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-site")
            .timeout(t3)
            .send().await.ok()?;
        let resp = self.http
            .get(&url)
            .header("accept", "*/*")
            .header("accept-language", "vi,en-US;q=0.9,en;q=0.8")
            .header("origin", "https://chat.deepseek.com")
            .header("referer", "https://chat.deepseek.com/")
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-site")
            .header("x-client-bundle-id", "com.deepseek.chat")
            .header("x-client-locale", "vi")
            .header("x-client-platform", "web")
            .header("x-client-timezone-offset", tz_offset_secs())
            .header("x-client-version", "2.7.0")
            .timeout(t3)
            .send().await.ok()?;
        let ttl = resp.headers().get("x-hif-ttl").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).unwrap_or(600);
        let v: Value = resp.json().await.ok()?;
        if v["data"]["biz_code"].as_i64() != Some(0) { return None; }
        let value = v["data"]["biz_data"]["value"].as_str()?.to_string();
        Some((value, std::time::Duration::from_secs(ttl.max(30))))
    }

    /// Làm tươi HIF của acc: mỗi giá trị theo hạn và backoff riêng. hif-dliq nằm trên CDN khác hif-leim và có mạng/DNS
    /// không với tới được (Chrome báo ERR_NAME_NOT_RESOLVED) — khi đó web không gắn x-hif-dliq, ta cũng bỏ qua rồi thử lại.
    async fn ensure_hif(&self) {
        let (need_l, need_d) = {
            let mut c = hif_cache().lock().unwrap_or_else(|e| e.into_inner());
            let e = c.entry(self.token.clone()).or_insert_with(|| HifEntry { leim: HifSlot::empty(), dliq: HifSlot::empty() });
            (e.leim.due(), e.dliq.due())
        };
        if !need_l && !need_d { return; }
        let (l, d) = tokio::join!(async { if need_l { Some(self.fetch_hif("hif-leim").await) } else { None } },
                                  async { if need_d { Some(self.fetch_hif("hif-dliq").await) } else { None } });
        let mut c = hif_cache().lock().unwrap_or_else(|e| e.into_inner());
        let e = c.entry(self.token.clone()).or_insert_with(|| HifEntry { leim: HifSlot::empty(), dliq: HifSlot::empty() });
        if let Some(r) = l { match r { Some((v, ttl)) => e.leim.ok(v, ttl), None => e.leim.fail() } }
        if let Some(r) = d { match r { Some((v, ttl)) => e.dliq.ok(v, ttl), None => e.dliq.fail() } }
    }

    /// Thêm `x-hif-leim` / `x-hif-dliq` vào request SSE (chỉ cái nào đang còn hiệu lực, như web).
    async fn with_hif(&self, rb: wreq::RequestBuilder) -> wreq::RequestBuilder {
        self.ensure_hif().await;
        let (l, d) = hif_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&self.token).map(|h| (h.leim.current(), h.dliq.current())).unwrap_or((None, None));
        let rb = match l { Some(v) => rb.header("x-hif-leim", v), None => rb };
        match d { Some(v) => rb.header("x-hif-dliq", v), None => rb }
    }

    pub fn new(token: String, cookies: String) -> Result<Self> {
        // Một wreq::Client (connection pool + cookie jar) cho mỗi acc: tái dùng TCP/TLS/HTTP2 giữa các request thay vì
        // bắt tay lại mỗi lần, và mỗi acc vẫn có kết nối + cookie riêng như một trình duyệt riêng.
        let pool = slot_pool();
        let cached = pool.lock().unwrap_or_else(|e| e.into_inner()).get(&token).cloned();
        let slot = match cached {
            Some(c) => c,
            None => {
                let jar = std::sync::Arc::new(wreq::cookie::Jar::default());
                let client = wreq::Client::builder()
                    // Dấu vân tay mạng của acc: TLS/HTTP2/UA/sec-ch-ua đều do emulation đặt, khớp một bản trình duyệt thật
                    .emulation(crate::fingerprint::Fingerprint::for_token(&token).emulation())
                    // Jar nhận Set-Cookie của DS (ds_session_id, WAF…) và tự gửi lại như trình duyệt
                    .cookie_provider(jar.clone())
                    // Chặn treo vô hạn: DS chập chờn mà không có timeout thì request giữ Lease mãi → cạn pool.
                    // KHÔNG dùng `.timeout()` toàn cục: nó tính cả thời gian đọc body, sẽ giết stream chat dài.
                    // `connect_timeout` chặn lúc bắt tay TCP/TLS; `read_timeout` chặn từng lần đọc socket
                    // (gồm cả lúc chờ header) nên stream vẫn sống miễn DS gửi byte đều đặn.
                    .connect_timeout(std::time::Duration::from_secs(15))
                    .read_timeout(std::time::Duration::from_secs(read_timeout_secs()))
                    .pool_idle_timeout(std::time::Duration::from_secs(60))
                    .build()?;
                let slot = std::sync::Arc::new(Slot { client, jar, seeded: Default::default() });
                pool.lock().unwrap_or_else(|e| e.into_inner()).insert(token.clone(), slot.clone());
                slot
            }
        };
        // Nạp cookie từ store vào jar chỉ khi giá trị store ĐỔI so với lần nạp trước (Chrome vừa làm mới):
        // nếu không đổi thì giữ bản trong jar, có thể đã được Set-Cookie của DS cập nhật mới hơn.
        {
            let url: wreq::Uri = "https://chat.deepseek.com/".parse().expect("static url");
            let mut seeded = slot.seeded.lock().unwrap_or_else(|e| e.into_inner());
            for kv in cookies.split(';') {
                let Some((k, v)) = kv.trim().split_once('=') else { continue };
                if k.is_empty() || seeded.get(k).map(|old| old == v).unwrap_or(false) { continue; }
                slot.jar.add(format!("{}={}; Path=/; Secure", k, v), &url);
                seeded.insert(k.to_string(), v.to_string());
            }
        }
        let http = slot.client.clone();
        let device_id = crate::fingerprint::device_for(&token).unwrap_or_else(|| device_id_for(&crate::fingerprint::seed_for(&token)));
        Ok(Self { http, token, device_id })
    }

    fn req(&self, method: wreq::Method, path: &str) -> wreq::RequestBuilder {
        self.req_to(method, format!("{}{}", DEEPSEEK_API, path))
    }

    /// Bộ header của một call API web; tách URL ra để test soi được request đi ra dây.
    fn req_to(&self, method: wreq::Method, url: String) -> wreq::RequestBuilder {
        self.http
            .request(method, url)
            .header("authorization", format!("Bearer {}", self.token))
            .header("origin", "https://chat.deepseek.com")
            .header("referer", "https://chat.deepseek.com/")
            // Bộ header web thật (2.7.0) gửi kèm mọi call /api/v0 — thiếu là dấu hiệu "không phải trình duyệt"
            .header("accept", "*/*")
            .header("accept-language", "vi,en-US;q=0.9,en;q=0.8")
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
            .req(wreq::Method::GET, "/users/current")
            .send()
            .await?
            .json()
            .await?;
        Ok(v)
    }

    pub async fn create_session(&self) -> Result<String> {
        let v: Value = self
            .req(wreq::Method::POST, "/chat_session/create")
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
            .req(wreq::Method::POST, "/chat_session/delete")
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
            .req(wreq::Method::POST, "/chat/create_pow_challenge")
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
        if crate::settings::boolean("DS_POW_PREFETCH") {
            if let Some(h) = pow_cache_take(&self.token) {
                return Ok(h);
            }
        } else {
            pow_cache_release_any(&self.token); // tắt dự đoán: bỏ bản còn sót, giải mới như thường
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
        crate::elog!("[pow] difficulty={} answer={} solve_ms={}", difficulty, answer, t0.elapsed().as_millis());
        let payload = json!({
            "algorithm": ch["algorithm"], "challenge": challenge_hex, "salt": salt,
            "answer": answer, "signature": signature, "target_path": ch["target_path"],
        });
        Ok(base64::engine::general_purpose::STANDARD.encode(payload.to_string()))
    }

    /// Upload ảnh/file lên DS web (multipart + PoW riêng cho upload_file), chờ xử lý xong, trả file id.
    ///
    /// Lỗi upload được gắn nhãn `FileRejected` để tầng chat phân biệt được "DS không nhận file này"
    /// với "acc hỏng": file bị từ chối (50404, UNSAFE, CONTENT_EMPTY…) là thuộc tính của FILE,
    /// không phải của acc — thử acc khác chỉ tốn thời gian và đốt hạn mức.
    pub async fn upload_file(&self, bytes: Vec<u8>, filename: &str, mime: &str) -> Result<String> {
        let pow = self.pow_header("/api/v0/file/upload_file").await?;
        let size = bytes.len();
        let part = wreq::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(mime)?;
        let form = wreq::multipart::Form::new().part("file", part);
        let v: Value = self
            .req(wreq::Method::POST, "/file/upload_file")
            .header("x-ds-pow-response", pow)
            .header("x-file-size", size.to_string())
            .header("x-model-type", "default")
            .multipart(form)
            .send()
            .await?
            .json()
            .await?;
        Self::check_auth(&v)?;
        let Some(id) = v["data"]["biz_data"]["id"].as_str().map(|s| s.to_string()) else {
            // Không có id: DS trả lỗi ngay ở bước upload. 50404 và họ hàng là lỗi của FILE
            // (ảnh quá lớn/định dạng lạ/nội dung bị chặn) — cùng một file sẽ hỏng y hệt ở acc khác.
            let mut code = v["data"]["biz_data"]["error_code"].to_string().trim_matches('"').to_string();
            if code == "null" { code = v["code"].to_string().trim_matches('"').to_string(); }
            if is_file_upload_rejection(&code) {
                return Err(anyhow::Error::new(FileRejected { status: "FAILED".into(), code }));
            }
            anyhow::bail!("upload_file thất bại: {}", v)
        };
        // PENDING → SUCCESS (~2s với ảnh nhỏ)
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let f: Value = self
                .req(wreq::Method::GET, &format!("/file/fetch_files?file_ids={}", id))
                .send().await?.json().await?;
            let st = f["data"]["biz_data"]["files"][0]["status"].as_str().unwrap_or("");
            match st {
                "SUCCESS" => return Ok(id),
                "FAILED" | "ERROR" | "CONTENT_EMPTY" | "UNSAFE" => {
                    let file = &f["data"]["biz_data"]["files"][0];
                    return Err(anyhow::Error::new(FileRejected {
                        status: st.to_string(),
                        code: file["error_code"].to_string().trim_matches('"').to_string(),
                    }));
                }
                _ => {}
            }
        }
        anyhow::bail!("upload_file quá thời gian chờ xử lý ({})", id)
    }

    /// Xác thực nhẹ: token còn sống không (khác với cookie WAF).
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

        crate::elog!(
            "[pow] difficulty={} answer={} solve_ms={}",
            difficulty, answer, solve_ms
        );

        let rb = self.with_hif(self.req(wreq::Method::POST, "/chat/completion").header("x-ds-pow-response", pow_header)).await;
        let resp = rb
            // web gửi completion từ trang của chính session đó
            .header("referer", format!("https://chat.deepseek.com/a/chat/s/{}", sid))
            .header("content-type", "application/json")
            .body(CompletionBody::new(&sid, parent_message_id, prompt, &[], thinking, search).into_body())
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
                .open(std::env::temp_dir().join("ds_debug_sse.log")) {
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

    let rb = self.with_hif(self.req(wreq::Method::POST, "/chat/completion").header("x-ds-pow-response", pow_header)).await;
    let resp = rb
        .header("referer", format!("https://chat.deepseek.com/a/chat/s/{}", sid))
        .header("content-type", "application/json")
        .body(CompletionBody::new(&sid, parent_message_id, prompt, ref_file_ids, thinking, search).into_body())
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
        // giữ phần đầu SSE thô: stream không có RESPONSE thì ghi ra samples.jsonl để biết DS thật sự trả gì
        const RAW_KEEP: usize = 256 * 1024;
        let mut raw_head = String::new();
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
                Err(_) => { crate::elog!("[ds-stall] không có dữ liệu {}s (sau {}ms, {} bytes) — bỏ stream", STALL_SECS, t0.elapsed().as_millis(), total_bytes); truncated = Some(format!("DeepSeek ngừng gửi dữ liệu {}s", STALL_SECS)); break; }
                Ok(None) => break,
                Ok(Some(Err(e))) => { crate::elog!("[ds-stream-err] {} (sau {}ms, {} bytes)", e, t0.elapsed().as_millis(), total_bytes); truncated = Some(format!("stream lỗi giữa chừng: {}", e)); break; }
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
                if raw_head.len() < RAW_KEEP { raw_head.push_str(&line); raw_head.push('\n'); }
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };

                // mở fragment mới: báo nội dung ban đầu và đổi fragment active
                let open_frags = |arr: &Vec<Value>, cur_type: &mut String, has_content: &mut bool| {
                    for f in arr {
                        *cur_type = f["type"].as_str().unwrap_or("").to_string();
                        if *cur_type == "RESPONSE" {
                            *has_content = true;
                            if let Some(c) = f["content"].as_str() {
                                let _ = tx.send(StreamEv::Response(c.to_string()));
                            }
                        } else if *cur_type == "THINK" {
                            if let Some(c) = f["content"].as_str() {
                                let _ = tx.send(StreamEv::Thinking(c.to_string()));
                            }
                        }
                    }
                };

                // token thật: {"p":"response","o":"BATCH","v":[{"p":"accumulated_token_usage","v":346},...]}
                // BATCH cũng có thể mở fragment: khi có ảnh, web trả READ_LINK trước rồi mới
                // {"p":"response","o":"BATCH","v":[{"p":"fragments","v":[{"type":"RESPONSE",...}]}]}
                if let Some(arr) = v.get("v").and_then(|x| x.as_array()) {
                    for it in arr {
                        match it.get("p").and_then(|p| p.as_str()) {
                            Some("accumulated_token_usage") => {
                                if let Some(n) = it.get("v").and_then(|x| x.as_u64()) { let _ = tx.send(StreamEv::Usage(n)); }
                            }
                            Some("fragments") => {
                                if let Some(frags) = it.get("v").and_then(|x| x.as_array()) {
                                    open_frags(frags, &mut cur_type, &mut has_content);
                                }
                            }
                            _ => {}
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
                            open_frags(arr, &mut cur_type, &mut has_content);
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
        crate::elog!("[ds-timing] first_byte={}ms total={}ms bytes={} sse_chars={}", first_byte_ms.unwrap_or(0), t0.elapsed().as_millis(), total_bytes, sse_chars);
        if !has_content {
            crate::elog!("[ds-stream] stream không có fragment RESPONSE nào (nghi throttle/rỗng)");
            use std::io::Write;
            let path = std::path::Path::new(&crate::server::creds_dir()).join("samples.jsonl");
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let _ = writeln!(f, "{}", json!({"t": t, "kind": "no-response", "raw": raw_head}));
            }
        }
        if let Some(r) = truncated { let _ = tx.send(StreamEv::Truncated(r)); }
        let _ = tx.send(StreamEv::Done);
    });

    Ok((sid, rx))
}

}


/// Kết quả dò "acc này có chat được không", tách bạch với "token còn hợp lệ không".
/// /users/current chỉ chứng minh token chưa bị từ chối — một acc bị throttle vẫn qua được,
/// nên verify phải đi tới tận /chat/completion mới biết thật.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Chat thật, có ít nhất một fragment RESPONSE.
    Alive,
    /// Token bị từ chối (40003/40002): phải đăng nhập lại.
    AuthDead,
    /// Token hợp lệ nhưng DeepSeek trả stream rỗng: đang bị throttle/hết hạn mức.
    Throttled,
    /// Lỗi khác (mạng, PoW, 5xx) — không kết luận được gì về acc.
    Unknown,
}

impl Probe {
    pub fn as_str(&self) -> &'static str {
        match self {
            Probe::Alive => "alive",
            Probe::AuthDead => "auth_dead",
            Probe::Throttled => "throttled",
            Probe::Unknown => "unknown",
        }
    }
}

impl DsClient {
    /// Chat thử một prompt tối thiểu để chứng minh acc dùng được. Tạo session riêng rồi xoá,
    /// nên không đụng tới thread stateful của acc và không để lại rác trên web.
    pub async fn probe_chat(&self) -> Probe {
        // 1) auth: rẻ nhất, và phân biệt được "token chết" với "throttle".
        match self.users_current().await {
            Ok(v) => match v["code"].as_i64() {
                Some(0) => {}
                Some(40003) | Some(40002) => return Probe::AuthDead,
                _ => return Probe::Unknown,
            },
            Err(_) => return Probe::Unknown,
        }
        // 2) chat thật. Prompt ngắn nhất có thể; thinking/search tắt để không tốn quota.
        let sid = match self.create_session().await {
            Ok(s) => s,
            Err(e) => return if format!("{:#}", e).contains("AUTH_INVALID") { Probe::AuthDead } else { Probe::Unknown },
        };
        let out = self.chat("hi", Some(&sid), None, false, false).await;
        // dọn session dù kết quả thế nào
        let _ = self.delete_session(&sid).await;
        match out {
            Ok((_, sse, _)) => {
                if sse_has_response(&sse) { Probe::Alive } else { Probe::Throttled }
            }
            Err(e) => {
                let msg = format!("{:#}", e);
                if msg.contains("AUTH_INVALID") { Probe::AuthDead }
                else if msg.contains("stream rỗng") { Probe::Throttled }
                else { Probe::Unknown }
            }
        }
    }
}

/// Kết quả web search trong SSE của /chat/completion (search_enabled): fragment SEARCH nhận
/// `{"p":"response/fragments/-1/results","v":[{url,title,snippet,published_at,...}]}` (có khi nằm sẵn trong khối init).
/// Bỏ trùng theo url, giữ thứ tự xuất hiện.
pub(crate) fn sse_search_results(sse: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let push = |arr: &Vec<Value>, out: &mut Vec<Value>| {
        for r in arr {
            let Some(url) = r["url"].as_str().filter(|u| !u.is_empty()) else { continue };
            if out.iter().any(|o| o["url"].as_str() == Some(url)) { continue; }
            out.push(r.clone());
        }
    };
    for line in sse.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        if v["p"].as_str().is_some_and(|p| p.ends_with("/results")) {
            if let Some(arr) = v["v"].as_array() { push(arr, &mut out); }
            continue;
        }
        if let Some(frags) = v["v"]["response"]["fragments"].as_array() {
            for f in frags {
                if let Some(arr) = f["results"].as_array() { push(arr, &mut out); }
            }
        }
    }
    out
}

/// SSE của /chat/completion có fragment RESPONSE nào chưa? Dùng chung logic với chat_stream:
/// khối fragments mang type, các delta sau đó chỉ mang p/v.
pub(crate) fn sse_has_response(sse: &str) -> bool {
    let cur_type = String::new();
    for line in sse.lines() {
        let Some(data) = line.strip_prefix("data: ") else { continue };
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        // init object chứa fragments[]
        if let Some(frags) = v.get("v").and_then(|x| x.get("response")).and_then(|r| r.get("fragments")).and_then(|f| f.as_array()) {
            for f in frags {
                if f["type"].as_str() == Some("RESPONSE") { return true; }
            }
            continue;
        }
        // APPEND fragments, hoặc BATCH có mục {"p":"fragments","v":[...]}
        if let Some(arr) = v.get("v").and_then(|x| x.as_array()) {
            for f in arr {
                if f["type"].as_str() == Some("RESPONSE") { return true; }
                if f["p"].as_str() == Some("fragments") {
                    let nested = f["v"].as_array().map(|a| a.iter().any(|g| g["type"].as_str() == Some("RESPONSE")));
                    if nested == Some(true) { return true; }
                }
            }
            continue;
        }
        // delta mang path
        if let Some(p) = v.get("p").and_then(|p| p.as_str()) {
            if p.ends_with("/content") && cur_type == "RESPONSE" { return true; }
            continue;
        }
        // bare {"v":"text"} nối tiếp fragment đang mở
        if cur_type == "RESPONSE" && v.get("v").and_then(|x| x.as_str()).is_some() { return true; }
    }
    false
}


#[cfg(test)]
mod probe_tests {
    use super::*;

    /// Stream rỗng: chỉ có event ready/update nhưng không fragment RESPONSE nào — đây đúng là
    /// thứ một acc bị throttle trả về, và là lý do verify cũ báo sai.
    #[test]
    fn empty_stream_has_no_response() {
        let sse = concat!(
            "event: ready\n",
            "data: {\"request_message_id\":123,\"response_message_id\":456}\n\n",
            "event: update\n",
            "data: {\"v\":{\"response\":{\"fragments\":[]}}}\n\n",
            "event: close\n\n"
        );
        assert!(!sse_has_response(sse));
    }

    /// Fragment RESPONSE xuất hiện ngay trong khối init.
    #[test]
    fn response_fragment_in_init() {
        let sse = "data: {\"v\":{\"response\":{\"fragments\":[{\"type\":\"RESPONSE\",\"content\":\"hi\"}]}}}\n";
        assert!(sse_has_response(sse));
    }

    /// Web hay trả THINK trước rồi mới tới RESPONSE: phải nhận ra RESPONSE, và không nhầm THINK.
    #[test]
    fn think_only_is_not_response() {
        let sse = concat!(
            "data: {\"v\":{\"response\":{\"fragments\":[{\"type\":\"THINK\",\"content\":\"nghĩ\"}]}}}\n",
            "data: {\"p\":\"response/fragments/-1/content\",\"v\":\" thêm\"}\n"
        );
        assert!(!sse_has_response(sse));
        let with_resp = concat!(
            "data: {\"v\":{\"response\":{\"fragments\":[{\"type\":\"RESPONSE\",\"content\":\"x\"}]}}}\n",
            "data: {\"p\":\"response/fragments/-1/content\",\"v\":\" thêm\"}\n"
        );
        assert!(sse_has_response(with_resp));
    }

    /// Mẫu thật (search_enabled): kết quả nằm ở response/fragments/-1/results, có thể lặp url.
    #[test]
    fn search_results_from_sse() {
        let sse = concat!(
            "data: {\"v\":{\"response\":{\"fragments\":[{\"id\":2,\"type\":\"SEARCH\",\"status\":\"WIP\",\"content\":null,\"queries\":[{\"query\":\"q\"}],\"results\":[]}]}}}\n",
            "data: {\"p\":\"response/fragments/-1/results\",\"v\":[{\"url\":\"https://a\",\"title\":\"A\",\"snippet\":\"sa\",\"cite_index\":1,\"published_at\":1700000000},{\"url\":\"https://b\",\"title\":\"B\",\"snippet\":\"sb\",\"cite_index\":2,\"published_at\":null}]}\n",
            "data: {\"p\":\"response/fragments/-1/results\",\"v\":[{\"url\":\"https://a\",\"title\":\"A again\"}]}\n",
            "data: {\"p\":\"response\",\"o\":\"BATCH\",\"v\":[{\"p\":\"fragments\",\"o\":\"APPEND\",\"v\":[{\"id\":3,\"type\":\"RESPONSE\",\"content\":\"x\"}]}]}\n"
        );
        let r = sse_search_results(sse);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["title"], "A");
        assert_eq!(r[1]["url"], "https://b");
    }

    /// Có ảnh đính kèm: web mở READ_LINK trước, rồi RESPONSE nằm trong một BATCH.
    #[test]
    fn response_fragment_in_batch() {
        let sse = concat!(
            "data: {\"v\":{\"response\":{\"fragments\":[{\"id\":2,\"type\":\"READ_LINK\",\"status\":\"WIP\"}]}}}\n",
            "data: {\"p\":\"response\",\"o\":\"BATCH\",\"v\":[{\"p\":\"fragments\",\"v\":[{\"id\":3,\"type\":\"RESPONSE\",\"content\":\"<\"}]},{\"p\":\"has_pending_fragment\",\"v\":false}]}\n",
            "data: {\"p\":\"response/fragments/-1/content\",\"o\":\"APPEND\",\"v\":\"x\"}\n"
        );
        assert!(sse_has_response(sse));
    }

    /// Dòng rác / JSON hỏng không được làm panic hay báo nhầm.
    #[test]
    fn garbage_lines_are_ignored() {
        let sse = "event: ping\ndata: not json\n\ndata: {\"v\":42}\n";
        assert!(!sse_has_response(sse));
        assert!(!sse_has_response(""));
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

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    #[test]
    fn profile_is_stable_per_token_and_varies_across_tokens() {
        let ua = |t: &str| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let c = wreq::Client::builder().emulation(crate::fingerprint::Fingerprint::for_token(t).emulation()).build().unwrap();
                c.get("https://tls.peet.ws/api/all").send().await.unwrap().text().await.unwrap()
            })
        };
        let v: Value = serde_json::from_str(&ua("tok-a")).unwrap();
        let v2: Value = serde_json::from_str(&ua("tok-a")).unwrap();
        assert_eq!(v["tls"]["ja4"], v2["tls"]["ja4"]);
        for t in ["tok-a", "tok-b", "tok-c", "tok-d"] {
            let v: Value = serde_json::from_str(&ua(t)).unwrap();
            println!("{t}: {} | ja4={} | h2={}", v["http2"]["sent_frames"][0].is_object(), v["tls"]["ja4"], v["http2"]["akamai_fingerprint_hash"]);
            println!("   {} {}", v["user_agent"], v["http2"]["sent_frames"].as_array().map(|f| f.len()).unwrap_or(0));
        }
    }

    /// So request thật với Chrome: `cargo test wire_ -- --ignored --nocapture`
    #[test]
    fn completion_body_matches_web_key_order() {
        let b = String::from_utf8(CompletionBody::new("s", None, "hi", &[], false, true).into_body()).unwrap();
        assert_eq!(b, r#"{"chat_session_id":"s","parent_message_id":null,"model_type":"default","prompt":"hi","ref_file_ids":[],"thinking_enabled":false,"search_enabled":true,"action":null,"preempt":false}"#);
    }

    #[tokio::test]
    #[ignore]
    async fn wire_request_as_seen_by_server() {
        let c = DsClient::new("tok-wire".into(), "a=b".into()).unwrap();
        let v: Value = c.req_to(wreq::Method::POST, "https://tls.peet.ws/api/all".into())
            .header("x-ds-pow-response", "AAAA")
            .json(&json!({"chat_session_id":"x","prompt":"hi"}))
            .send().await.unwrap().json().await.unwrap();
        println!("ua={}", v["user_agent"]);
        println!("ja4={}", v["tls"]["ja4"]);
        println!("h2={}", v["http2"]["akamai_fingerprint"]);
        println!("headers={}", v["http2"]["sent_frames"].as_array().unwrap().iter().filter_map(|f| f["headers"].as_array()).flatten().map(|h| h.as_str().unwrap_or("").split(": ").next().unwrap_or("").to_string()).collect::<Vec<_>>().join(","));
        println!("priority_frame={}", v["http2"]["sent_frames"].as_array().unwrap().iter().map(|f| f["frame_type"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(","));
    }

    /// Chat thật qua client mới bằng acc đầu tiên trong store: `cargo test live_chat -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_chat_with_first_account() {
        let raw = std::fs::read_to_string(format!("{}/.whalegate/accounts.json", std::env::var("HOME").unwrap())).unwrap();
        let st: Value = serde_json::from_str(&raw).unwrap();
        let a = &st["accounts"][0];
        let cookies = a["cookies"].as_object().unwrap().iter().map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ");
        let c = DsClient::new(a["token"].as_str().unwrap().to_string(), cookies).unwrap();
        let ms = |t: std::time::Instant| t.elapsed().as_millis();
        for round in 1..=3 {
            let t = std::time::Instant::now();
            c.users_current().await.unwrap();
            let t_user = ms(t);
            let t = std::time::Instant::now();
            c.ensure_hif().await;
            let t_hif = ms(t);
            let t = std::time::Instant::now();
            let sid = c.create_session().await.unwrap();
            let t_sess = ms(t);
            let t = std::time::Instant::now();
            let pow = c.completion_pow().await.unwrap();
            let t_pow = ms(t);
            drop(pow);
            let t = std::time::Instant::now();
            let (sid2, mut rx) = c.chat_stream("ping, trả lời đúng 1 chữ: ok", Some(&sid), None, false, false, &[]).await.unwrap();
            let t_head = ms(t);
            let mut text = String::new();
            let mut first = None;
            while let Some(ev) = rx.recv().await {
                match ev { StreamEv::Response(t2) => { first.get_or_insert(ms(t)); text.push_str(&t2) }, StreamEv::Done => break, _ => {} }
            }
            println!("round{round}: users/current={t_user}ms hif={t_hif}ms create_session={t_sess}ms pow={t_pow}ms completion_headers={t_head}ms first_token={:?}ms total={}ms reply={:?}", first, ms(t), text);
            let _ = c.delete_session(&sid2).await;
        }
    }

    /// HTTP thuần (không Chrome) vào các trang soi TLS/HTTP2: `cargo test wire_sites -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn wire_sites() {
        for seed in ["tok-site-1", "tok-site-2"] {
            let fp = crate::fingerprint::Fingerprint::for_token(seed);
            let c = wreq::Client::builder().emulation(fp.emulation()).build().unwrap();
            let b: Value = c.get("https://tls.browserleaks.com/json").send().await.unwrap().json().await.unwrap();
            println!("{seed} want_ua={} \n  browserleaks: ja3n={} ja4={} akamai={} ua={}", fp.user_agent(), b["ja3n_hash"], b["ja4"], b["akamai_hash"], b["user_agent"]);
            let p: Value = c.get("https://tls.peet.ws/api/all").send().await.unwrap().json().await.unwrap();
            println!("  peet: ja4={} h2={} http={}", p["tls"]["ja4"], p["http2"]["akamai_fingerprint_hash"], p["http_version"]);
        }
    }

    /// Server có ép x-hif-* không: `cargo test live_hif -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_hif_enforcement() {
        let raw = std::fs::read_to_string(format!("{}/.whalegate/accounts.json", std::env::var("HOME").unwrap())).unwrap();
        let st: Value = serde_json::from_str(&raw).unwrap();
        let a = &st["accounts"][0];
        let cookies = a["cookies"].as_object().unwrap().iter().map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ");
        let tok = a["token"].as_str().unwrap().to_string();
        let c = DsClient::new(tok.clone(), cookies).unwrap();
        for (name, leim, dliq) in [("đúng", None, None), ("không có", Some(None), Some(None)), ("sai (rác)", Some(Some("AAAA=.BBBB")), Some(Some("CCCC=.DDDD")))] {
            {
                let mut m = hif_cache().lock().unwrap();
                m.remove(&tok);
                if let (Some(l), Some(d)) = (leim, dliq) {
                    let mut e = HifEntry { leim: HifSlot::empty(), dliq: HifSlot::empty() };
                    if let Some(x) = l { e.leim.ok(x.to_string(), std::time::Duration::from_secs(600)); }
                    if let Some(x) = d { e.dliq.ok(x.to_string(), std::time::Duration::from_secs(600)); }
                    e.leim.retry_at += std::time::Duration::from_secs(600);
                    e.dliq.retry_at += std::time::Duration::from_secs(600);
                    m.insert(tok.clone(), e);
                }
            }
            let t = std::time::Instant::now();
            match c.chat_stream("trả lời đúng 1 chữ: ok", None, None, false, false, &[]).await {
                Ok((sid, mut rx)) => {
                    let mut text = String::new();
                    while let Some(ev) = rx.recv().await { match ev { StreamEv::Response(t) => text.push_str(&t), StreamEv::Done => break, _ => {} } }
                    println!("HIF {name}: OK reply={:?} {}ms", text, t.elapsed().as_millis());
                    let _ = c.delete_session(&sid).await;
                }
                Err(e) => println!("HIF {name}: LỖI {}", format!("{e:#}").chars().take(300).collect::<String>()),
            }
        }
    }
}
