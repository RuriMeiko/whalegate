// Handler /v1/chat/completions.
// MỘT lõi duy nhất (`run_chat`): chọn acc, giữ khoá in-flight suốt vòng đời stream, guard markup tool-call,
// retry + chuyển sang acc khác khi lỗi/rỗng/cụt. Stream SSE và non-stream chỉ là hai cách tiêu thụ lõi này.
use crate::client::{DsClient, StreamEv};
use crate::server::*;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json, Response};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

type ApiErr = (StatusCode, Json<Value>);

/// Tự nhả khoá in-flight khi bị drop (kể cả client ngắt kết nối giữa stream).
struct InFlightGuard(Arc<std::sync::atomic::AtomicU32>);
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Release);
    }
}

/// Một acc đã được chiếm cho đến khi `Lease` bị drop.
struct Lease {
    acc_id: String,
    token: String,
    cookies: std::collections::BTreeMap<String, String>,
    thread_sid: Option<String>,
    _guard: InFlightGuard,
}

/// Sự kiện trung gian của lõi chat.
enum Out {
    Reasoning(String),
    Content(String),
    ToolCalls(Vec<DsmlToolCall>, Vec<String>), // (calls, id đã cấp — client sẽ echo lại, dùng làm dấu vân tay)
    Finish { reason: &'static str, prompt_tokens: i64, completion_tokens: i64, cached_tokens: i64 },
    Error(String),
}

/// Ảnh đính kèm lấy từ messages (đã giải mã); upload lên DS theo từng acc khi cần.
pub(crate) struct Image {
    hash: String,
    bytes: Vec<u8>,
    mime: String,
}

/// Tiếp hội thoại đang nằm trong phiên web: chỉ gửi phần mới.
struct ContPlan {
    acc_id: String,
    sid: String,
    parent_id: i64,
    delta_prompt: String,
    delta_images: Vec<Image>,
    base_chars: usize,
    tokens: u64, // token đã có trong phiên web (accumulated_token_usage lượt trước)
}

/// Đăng ký hội thoại sau khi trả lời để request kế tiếp nối tiếp được.
struct RegPlan {
    pf_n: u64,
    tools_hash: u64,
}

struct ChatCtx {
    prompt: String,
    cont: Option<ContPlan>,
    reg: Option<RegPlan>,
    images: Vec<Image>,
    thinking: bool,
    search: bool,
    tools: Option<Vec<Value>>,
}

/// Lưu mẫu response lỗi (parse trượt / call sai schema) vào ~/.ds-harness/samples.jsonl để biến thành test sau này.
fn save_sample(kind: &str, raw: &str) {
    use std::io::Write;
    let path = std::path::Path::new(&creds_dir()).join("samples.jsonl");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", json!({"t": now(), "kind": kind, "raw": raw}));
    }
}

/// Ước lượng token theo loại ký tự (đã đo trên tokenizer của DS web: Anh ~4,1 ký tự/token, code ~2,5, tiếng Việt ~1,9).
/// Chỉ dùng khi web không trả số thật (`accumulated_token_usage`).
pub(crate) fn est_tokens(text: &str) -> u64 {
    let mut t = 0f64;
    for c in text.chars() {
        t += if c.is_ascii_alphanumeric() || c == ' ' { 0.23 } else if c.is_ascii() { 0.6 } else { 1.1 };
    }
    t.ceil() as u64
}

fn sha256_hex(bytes: &[u8]) -> String {
    // sha256 không có crate sẵn: dùng hash 128-bit nhẹ (đủ để khử trùng ảnh trong 1 phiên, không dùng cho bảo mật)
    use std::hash::{Hash, Hasher};
    let mut h1 = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h1);
    let mut h2 = std::collections::hash_map::DefaultHasher::new();
    (bytes.len(), bytes.iter().rev().take(64).collect::<Vec<_>>()).hash(&mut h2);
    format!("{:016x}{:016x}{}", h1.finish(), h2.finish(), bytes.len())
}

/// Kích thước tối đa cho một ảnh tải qua URL (DS_MAX_IMAGE_MB, mặc định 20MB).
fn max_image_bytes() -> usize {
    crate::settings::uint("DS_MAX_IMAGE_MB") as usize * 1024 * 1024
}

/// Cắt theo ký tự (không panic khi cắt giữa ký tự nhiều byte).
fn head_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// Chặn SSRF: chỉ cho http/https tới host công khai. Ảnh trong hội thoại là dữ liệu do client
/// (có thể bị prompt-injection điều khiển) nên không được phép chạm vào loopback/LAN/metadata.
fn url_is_public(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else { return false };
    let host_l = host.trim_end_matches('.').to_ascii_lowercase();
    if host_l == "localhost" || host_l.ends_with(".localhost") || host_l.ends_with(".local") || host_l.ends_with(".internal") {
        return false;
    }
    let ip = host_l.parse::<std::net::IpAddr>().ok().or_else(|| {
        // "[::1]" → "::1"
        host_l.strip_prefix('[').and_then(|h| h.strip_suffix(']')).and_then(|h| h.parse().ok())
    });
    match ip {
        Some(std::net::IpAddr::V4(v4)) => {
            !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_broadcast()
                || v4.is_unspecified() || v4.is_documentation() || v4.octets()[0] == 0)
        }
        Some(std::net::IpAddr::V6(v6)) => {
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local() || v6.is_unicast_link_local())
                // IPv4-mapped (::ffff:127.0.0.1)
                && !v6.to_ipv4_mapped().map(|v4| v4.is_private() || v4.is_loopback() || v4.is_link_local()).unwrap_or(false)
        }
        // hostname: không phân giải DNS để kiểm tra (tránh TOCTOU); chỉ chặn các tên nội bộ rõ ràng ở trên
        None => true,
    }
}

/// Key nhận dạng một phần ảnh — dùng để không decode/tải lại cùng một ảnh trong một request.
fn image_key(part: &Value) -> Option<String> {
    if let Some(src) = part.get("source") {
        if let Some(d) = src.get("data").and_then(|x| x.as_str()) {
            return Some(format!("b64:{}:{}", d.len(), head_chars(d, 64)));
        }
    }
    let url = part.pointer("/image_url/url").and_then(|x| x.as_str())
        .or_else(|| part.get("image_url").and_then(|x| x.as_str()))
        .or_else(|| part.get("url").and_then(|x| x.as_str()))?;
    Some(format!("url:{}", url))
}

/// Giải mã 1 phần nội dung ảnh (OpenAI `image_url`, Anthropic `image`/base64, `input_image`) → bytes + mime.
async fn decode_image_part(part: &Value) -> Option<(Vec<u8>, String)> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let cap = max_image_bytes();
    // Anthropic: {type:"image", source:{type:"base64", media_type, data}}
    if let Some(src) = part.get("source") {
        if let (Some(mt), Some(d)) = (src.get("media_type").and_then(|x| x.as_str()), src.get("data").and_then(|x| x.as_str())) {
            if d.len() / 4 * 3 > cap {
                crate::elog!("[image] base64 vượt giới hạn {}MB — bỏ", cap / 1024 / 1024);
                return None;
            }
            return b64.decode(d.trim()).ok().map(|b| (b, mt.to_string()));
        }
    }
    let url = part.pointer("/image_url/url").and_then(|x| x.as_str())
        .or_else(|| part.get("image_url").and_then(|x| x.as_str()))
        .or_else(|| part.get("url").and_then(|x| x.as_str()))?;
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',')?;
        if data.len() / 4 * 3 > cap {
            crate::elog!("[image] data URL vượt giới hạn {}MB — bỏ", cap / 1024 / 1024);
            return None;
        }
        let mime = meta.split(';').next().unwrap_or("image/png").to_string();
        return b64.decode(data.trim()).ok().map(|b| (b, mime));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        let parsed = reqwest::Url::parse(url).ok()?;
        if !url_is_public(&parsed) {
            crate::elog!("[image] chặn URL nội bộ/không hợp lệ: {}", head_chars(url, 120));
            return None;
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            // mỗi bước redirect cũng phải qua kiểm tra SSRF (host công khai có thể 302 về 127.0.0.1)
            .redirect(reqwest::redirect::Policy::custom(|a| {
                if a.previous().len() >= 3 || !url_is_public(a.url()) { a.stop() } else { a.follow() }
            }))
            .build().ok()?;
        let r = client.get(parsed).send().await.ok()?;
        if !r.status().is_success() {
            return None;
        }
        let mime = r.headers().get("content-type").and_then(|v| v.to_str().ok())
            .unwrap_or("image/png").split(';').next().unwrap_or("image/png").to_string();
        // đọc theo chunk để cắt sớm: không tin Content-Length, không gom cả GB vào RAM
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = r.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.ok()?;
            if buf.len() + chunk.len() > cap {
                crate::elog!("[image] ảnh vượt giới hạn {}MB — bỏ", cap / 1024 / 1024);
                return None;
            }
            buf.extend_from_slice(&chunk);
        }
        return Some((buf, mime));
    }
    None
}

/// Mọi ảnh trong hội thoại (kể cả trong kết quả tool như read_image), khử trùng, giữ tối đa `max` ảnh mới nhất.
/// `cache` chia sẻ giữa các lần gọi trong CÙNG request để ảnh URL không bị tải lại 2 lần.
pub(crate) async fn collect_images_cached(
    messages: &[Value], max: usize, cache: &mut std::collections::HashMap<String, Option<(Vec<u8>, String)>>,
) -> Vec<Image> {
    let mut found: Vec<Image> = Vec::new();
    for m in messages {
        let Some(parts) = m.get("content").and_then(|c| c.as_array()) else { continue };
        let mut stack: Vec<&Value> = parts.iter().collect();
        while let Some(p) = stack.pop() {
            match p.get("type").and_then(|t| t.as_str()) {
                Some("image_url") | Some("image") | Some("input_image") => {
                    let key = image_key(p);
                    let decoded = match (&key, cache.get(key.as_deref().unwrap_or(""))) {
                        (Some(_), Some(v)) => v.clone(),
                        (Some(k), None) => {
                            let v = decode_image_part(p).await;
                            cache.insert(k.clone(), v.clone());
                            v
                        }
                        (None, _) => decode_image_part(p).await,
                    };
                    if let Some((bytes, mime)) = decoded {
                        let hash = sha256_hex(&bytes);
                        found.retain(|i| i.hash != hash); // ảnh lặp lại → coi như mới nhất
                        found.push(Image { hash, bytes, mime });
                    }
                }
                Some("tool_result") => {
                    if let Some(inner) = p.get("content").and_then(|c| c.as_array()) { stack.extend(inner.iter()); }
                }
                _ => {}
            }
        }
    }
    let n = found.len();
    if n > max { found.drain(..n - max); }
    found
}

fn ext_for(mime: &str) -> &'static str {
    match mime { "image/jpeg" | "image/jpg" => "jpg", "image/webp" => "webp", "image/gif" => "gif", "image/bmp" => "bmp", _ => "png" }
}

/// Mức suy nghĩ → bật DeepThink? "none"/"off" = Nhanh; mọi mức khác ("max", "high"…) = DeepThink;
/// "auto" = DeepThink nhưng được tắt ở lượt nối tiếp sau tool_result (xử lý ở handler).
/// Không chỉ định: model cũ `*-r1` / `thinking:true` → bật, còn lại theo DS_DEFAULT_EFFORT (mặc định none).
pub(crate) fn effort_to_thinking(effort: Option<&str>, legacy_r1: bool, thinking_flag: Option<bool>) -> bool {
    match effort.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
        Some("none") | Some("off") | Some("minimal") | Some("false") | Some("0") => false,
        Some("") | None => {
            if legacy_r1 || thinking_flag == Some(true) { return true; }
            let d = crate::settings::get("DS_DEFAULT_EFFORT").to_ascii_lowercase();
            !matches!(d.as_str(), "none" | "off" | "")
        }
        Some(_) => true,
    }
}

fn hash_pair(prev: u64, x: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    prev.hash(&mut h);
    x.hash(&mut h);
    h.finish()
}

/// Dấu vân tay 1 message: assistant chỉ tính id tool-call (khoan dung với việc client chuẩn hoá lại văn bản); còn lại tính nội dung.
fn msg_fp_str(m: &Value) -> String {
    let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
    if role == "assistant" {
        let ids: Vec<&str> = m.get("tool_calls").and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|tc| tc.get("id").and_then(|x| x.as_str())).collect()).unwrap_or_default();
        return format!("assistant|{}", ids.join(","));
    }
    format!("{}|{}|{}", role, m.get("tool_call_id").and_then(|x| x.as_str()).unwrap_or(""), render_content(m))
}

/// pf[k] = dấu vân tay của k message đầu (pf[0] là hạt giống).
pub(crate) fn prefix_fps(msgs: &[Value]) -> Vec<u64> {
    let mut v = vec![0x5eed_u64];
    for m in msgs {
        let next = hash_pair(*v.last().unwrap(), &msg_fp_str(m));
        v.push(next);
    }
    v
}

fn tools_hash(tools: &Option<Vec<Value>>) -> u64 {
    hash_pair(7, &serde_json::to_string(tools).unwrap_or_default())
}

fn stateful_enabled() -> bool {
    crate::settings::boolean("DS_STATEFUL")
}

fn conv_max_chars() -> usize {
    crate::settings::uint("DS_CONV_MAX_CHARS") as usize
}

fn spawn_delete_session(acc: Option<(String, String)>, sid: String) {
    if let Some((tok, ck)) = acc {
        tokio::spawn(async move {
            if let Ok(c) = DsClient::new(tok, ck) {
                let _ = c.delete_session(&sid).await;
            }
        });
    }
}

fn too_many(msg: &str) -> ApiErr {
    (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error": {"message": msg, "type": "rate_limit_error", "code": "pool_exhausted"}, "retry_after_secs": 5})))
}

fn queue_wait_secs() -> u64 {
    crate::settings::uint("DS_QUEUE_WAIT_SECS")
}

/// Chọn acc ít bận nhất (rồi lâu chưa dùng nhất). `exclude` là loại trừ MỀM: bỏ qua các acc đó khi còn acc khác
/// trong pool; pool chỉ có đúng acc đó thì vẫn dùng. Chờ tới `deadline` rồi trả 429.
async fn acquire(st: &Arc<AppState>, exclude: &[String], only: Option<&str>, deadline: Instant) -> Result<Lease, ApiErr> {
    loop {
        let t = now();
        let picked = {
            let store = st.store.read().await;
            let accs = &store.accounts;
            if accs.is_empty() {
                return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "no account configured"}))));
            }
            let others_exist = accs.iter().any(|a| a.pool_enabled && !exclude.contains(&a.id));
            let in_flight = st.in_flight.lk();
            let mut best: Option<((u32, u64), usize)> = None;
            for (i, a) in accs.iter().enumerate() {
                if !a.pool_enabled || (others_exist && exclude.contains(&a.id)) || only.is_some_and(|o| o != a.id) {
                    continue;
                }
                let conc = in_flight
                    .get(&a.id)
                    .map(|v| v.load(std::sync::atomic::Ordering::Acquire))
                    .unwrap_or(0);
                // >2 call đồng thời / acc dễ bị khoá (DS_MAX_CONC_PER_ACC)
                if conc < AccStats::max_conc() && a.stats.available(t) {
                    let key = (conc, a.stats.last_used_at);
                    if best.map(|(bk, _)| key < bk).unwrap_or(true) {
                        best = Some((key, i));
                    }
                }
            }
            drop(in_flight);
            best.map(|(_, i)| {
                let a = &accs[i];
                let sid = if a.thread_uses >= session_rotate_limit() { None } else { a.thread_session_id.clone() };
                (a.token.clone(), a.cookies.clone(), sid, a.id.clone())
            })
        };

        if let Some((token, cookies, thread_sid, acc_id)) = picked {
            let counter = {
                let mut map = st.in_flight.lk();
                map.entry(acc_id.clone())
                    .or_insert_with(|| Arc::new(std::sync::atomic::AtomicU32::new(0)))
                    .clone()
            };
            if counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel) < AccStats::max_conc() {
                return Ok(Lease { acc_id, token, cookies, thread_sid, _guard: InFlightGuard(counter) });
            }
            counter.fetch_sub(1, std::sync::atomic::Ordering::Release); // thua race → chọn lại
        }
        if Instant::now() >= deadline {
            return Err(too_many("pool exhausted: all accounts busy/cooling down"));
        }
        // jitter nhẹ cho đỡ đều đều
        tokio::time::sleep(Duration::from_millis(200 + (t % 500))).await;
    }
}

/// Ghi nhận 1 lượt gọi thực sự lên acc (tính vào burst).
async fn touch_acc(st: &Arc<AppState>, acc_id: &str) {
    let mut store = st.store.write().await;
    if let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) {
        a.stats.record_call(now());
    }
    drop(store);
    mark_dirty(st);
}

/// Acc hỏng/rỗng/cụt: tăng bộ đếm + cooldown tăng dần. `penalize=false` cho lỗi do model (không phải acc).
async fn fail_acc(st: &Arc<AppState>, acc_id: &str, why: &str, penalize: bool) {
    let mut store = st.store.write().await;
    if let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) {
        if penalize {
            if why == "empty" {
                a.stats.total_empty += 1;
            } else {
                a.stats.total_error += 1;
            }
            a.stats.mark_fail(now());
            a.last_status = Some(if why == "empty" { "throttled".into() } else { "error".into() });
        }
    }
    drop(store);
    mark_dirty(st);
}

/// Token chết (code 40003): cách ly acc lâu, gắn nhãn cần đăng nhập lại; đăng nhập lại/Reset sẽ gỡ.
async fn mark_auth_invalid(st: &Arc<AppState>, acc_id: &str) {
    let mut store = st.store.write().await;
    if let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) {
        a.stats.total_error += 1;
        a.stats.fail_streak = a.stats.fail_streak.max(5);
        a.stats.cooldown_until = now() + 1800;
        a.last_status = Some("invalid token — cần login lại".into());
    }
    drop(store);
    mark_dirty(st);
}

async fn ok_acc(st: &Arc<AppState>, acc_id: &str, sid: &str, shared: bool) {
    let mut store = st.store.write().await;
    if let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) {
        if shared { note_session_use(a, sid); }
        a.stats.total_ok += 1;
        a.stats.mark_ok();
        a.last_status = Some("ok".into());
    }
    drop(store);
    mark_dirty(st);
}

impl ContPlan {
    fn acc_id_hint(&self) -> &str { &self.acc_id }
}

/// Ghi lại hội thoại vừa trả lời để request kế (cùng prefix + assistant này) nối tiếp được trong phiên web.
#[allow(clippy::too_many_arguments)]
fn register_conv(st: &Arc<AppState>, ctx: &ChatCtx, was_stateful: bool, acc_id: &str, sid: &str, msg_id: Option<i64>, ids: &[String], resp_chars: usize, tokens: u64) {
    let (Some(reg), Some(mid)) = (&ctx.reg, msg_id) else { return };
    let base = if was_stateful { ctx.cont.as_ref().map_or(0, |c| c.base_chars) } else { ctx.prompt.chars().count() };
    st.sessions.lk().insert(sid.to_string(), (acc_id.to_string(), now()));
    let key = hash_pair(reg.pf_n, &format!("assistant|{}", ids.join(",")));
    st.convs.lk().insert(key, Conv {
        acc_id: acc_id.to_string(), sid: sid.to_string(), parent_id: mid,
        tools_hash: reg.tools_hash, chars: base + resp_chars, tokens,
    });
}

/// Lõi: retry tối đa 3 lượt, mỗi lượt lỗi/rỗng/cụt thì đổi sang acc khác.
fn run_chat(st: Arc<AppState>, ctx: ChatCtx, lease0: Lease) -> impl Stream<Item = Out> {
    async_stream::stream! {
        let t_req = Instant::now();
        let mut lease = Some(lease0);
        let mut failed: Vec<String> = Vec::new();
        let mut last_err: Option<String> = None;
        let mut finished = false;

        for attempt in 0..3usize {
            if attempt > 0 {
                // failover: nhả acc hỏng rồi lấy acc khác (mềm)
                let prev = lease.take().map(|l| l.acc_id);
                let dl = Instant::now() + Duration::from_secs(queue_wait_secs().min(60));
                match acquire(&st, &failed, None, dl).await {
                    Ok(l) => {
                        if Some(&l.acc_id) == prev.as_ref() {
                            // pool chỉ còn đúng acc này → chờ DS hồi (rate-limit mềm ~8-10s)
                            tokio::time::sleep(Duration::from_secs(8)).await;
                        }
                        lease = Some(l);
                    }
                    Err((_, Json(e))) => {
                        last_err = Some(e.get("error").and_then(|x| x.as_str()).unwrap_or("hết acc khả dụng").to_string());
                        break;
                    }
                }
            }
            let l = lease.as_ref().unwrap();
            let acc_id = l.acc_id.clone();
            touch_acc(&st, &acc_id).await;

            // lượt đầu + có hội thoại đang nằm trong phiên web → chỉ gửi phần mới nối vào parent
            let stateful_attempt = attempt == 0 && ctx.cont.is_some();
            let (prompt_used, images_used, sid_used, parent_used): (&str, &Vec<Image>, Option<String>, Option<i64>) = match (&ctx.cont, stateful_attempt) {
                (Some(cp), true) => (cp.delta_prompt.as_str(), &cp.delta_images, Some(cp.sid.clone()), Some(cp.parent_id)),
                // bắt đầu hội thoại stateful: phiên riêng (None → tạo mới); không dùng phiên chung của acc
                _ if ctx.reg.is_some() => (ctx.prompt.as_str(), &ctx.images, None, None),
                _ => (ctx.prompt.as_str(), &ctx.images, l.thread_sid.clone(), None),
            };
            let own_session = stateful_attempt || ctx.reg.is_some();
            // retry sau khi lượt nối tiếp hỏng: bỏ phiên cũ (ngữ cảnh không còn đáng tin)
            if attempt == 1 {
                if let Some(cp) = &ctx.cont {
                    let acc = { st.store.read().await.accounts.iter().find(|a| a.id == cp.acc_id_hint()).map(|a| (a.token.clone(), cookie_string(&a.cookies))) };
                    st.sessions.lk().remove(&cp.sid);
                    spawn_delete_session(acc, cp.sid.clone());
                }
            }
            let stream_res = match DsClient::new(l.token.clone(), cookie_string(&l.cookies)) {
                Ok(c) => {
                    // ảnh: upload (cache theo acc) rồi tham chiếu bằng ref_file_ids
                    let mut file_ids: Vec<String> = Vec::new();
                    let mut up_err: Option<anyhow::Error> = None;
                    for img in images_used {
                        let key = (acc_id.clone(), img.hash.clone());
                        let cached = st.file_cache.lk().get(&key).cloned();
                        if let Some(id) = cached { file_ids.push(id); continue; }
                        match c.upload_file(img.bytes.clone(), &format!("image-{}.{}", &img.hash[..8], ext_for(&img.mime)), &img.mime).await {
                            Ok(id) => { st.file_cache.lk().insert(key, id.clone()); file_ids.push(id); }
                            Err(e) => { up_err = Some(e); break; }
                        }
                    }
                    match up_err {
                        Some(e) => Err(e),
                        None => c.chat_stream(prompt_used, sid_used.as_deref(), parent_used, ctx.thinking, ctx.search, &file_ids).await,
                    }
                }
                Err(e) => Err(e),
            };
            let (sid, mut rx) = match stream_res {
                Ok(p) => {
                    if own_session { st.sessions.lk().insert(p.0.clone(), (acc_id.clone(), now())); }
                    p
                }
                Err(e) => {
                    crate::elog!("[ds-error] attempt {} acc {}: {:#}", attempt, &acc_id[..8.min(acc_id.len())], e);
                    last_err = Some(format!("{:#}", e));
                    if format!("{:#}", e).contains("AUTH_INVALID") {
                        mark_auth_invalid(&st, &acc_id).await;
                    } else {
                        fail_acc(&st, &acc_id, "error", true).await;
                    }
                    failed.push(acc_id);
                    continue;
                }
            };

            let mut resp_text = String::new(); // phần content chưa flush (đuôi nghi là marker)
            let mut full_text = String::new();
            let mut yielded_len: usize = 0;
            let mut think_est: u64 = 0;
            let prior_tokens: u64 = if stateful_attempt { ctx.cont.as_ref().map_or(0, |c| c.tokens) } else { 0 };
            let mut think_buf = String::new();
            let mut think_dropped = false;
            let mut has_content = false;
            let mut truncated: Option<String> = None;
            let mut msg_id: Option<i64> = None;
            let mut real_usage: Option<u64> = None;
            let mut attempt_done = false;

            while let Some(ev) = rx.recv().await {
                match ev {
                    StreamEv::Thinking(t) => {
                        think_est += est_tokens(&t);
                        // model hay nháp khối tool-call trong lúc nghĩ → không để markup lọt ra client
                        if !think_dropped && !t.is_empty() {
                            think_buf.push_str(&t);
                            let cut = safe_flush_len(&think_buf);
                            if has_tool_marker(&think_buf) {
                                crate::elog!("[toolcall-guard] DSML trong reasoning — cắt từ marker");
                                think_dropped = true;
                            }
                            if cut > 0 {
                                yield Out::Reasoning(think_buf[..cut].to_string());
                            }
                            if think_dropped { think_buf.clear(); } else { think_buf.drain(..cut); }
                        }
                    }
                    StreamEv::Response(t) => {
                        has_content = true;
                        resp_text.push_str(&t);
                        full_text.push_str(&t);
                        // chỉ flush phần chắc chắn là prose; giữ đuôi nghi marker / toàn bộ từ marker trở đi
                        let cut = safe_flush_len(&resp_text);
                        if cut > 0 {
                            let head: String = resp_text[..cut].to_string();
                            yielded_len += head.chars().count();
                            resp_text.drain(..cut);
                            yield Out::Content(head);
                        }
                    }
                    StreamEv::MsgId(m) => { msg_id = Some(m); }
                    StreamEv::Usage(u) => { real_usage = Some(u); crate::elog!("[usage] accumulated_token_usage={} (stateful={})", u, stateful_attempt); }
                    StreamEv::Truncated(why) => { truncated = Some(why); }
                    StreamEv::Done => {
                        if !think_dropped && !think_buf.is_empty() {
                            yield Out::Reasoning(std::mem::take(&mut think_buf));
                        }
                        // token: số thật của web (cộng dồn theo phiên) trừ phần output lượt này; không có thì ước lượng.
                        // Đo thực tế: accumulated_token_usage KHÔNG tính token suy nghĩ (think 2K hay 5K ký tự đều ra ~600),
                        // nên chỉ trừ output; còn completion_tokens báo ra vẫn gồm cả think (đúng nghĩa OpenAI/DeepSeek API).
                        let out_est = est_tokens(&full_text) as i64;
                        let completion_tokens = out_est + think_est as i64;
                        let prompt_tokens = match real_usage {
                            Some(u) => (u as i64 - out_est).max(1),
                            None => est_tokens(prompt_used) as i64 + prior_tokens as i64,
                        };
                        // phần ngữ cảnh đã nằm sẵn trong phiên web (nối tiếp) = cached
                        let cached_tokens = if stateful_attempt { (prior_tokens as i64).min(prompt_tokens) } else { 0 };

                        if let Some((before, mut calls)) = parse_any_tool_calls(&full_text) {
                            // prose trước khối tool-call mà chưa stream (before đã trim)
                            let before_chars = before.chars().count();
                            if before_chars > yielded_len {
                                let rem: String = before.chars().skip(yielded_len).collect();
                                if !rem.is_empty() { yield Out::Content(rem); }
                            }
                            for c in calls.iter_mut() { infer_tool_name(&ctx.tools, c); fill_required_args(&ctx.tools, c); }
                            // call sai schema (thiếu tham số bắt buộc/tool lạ): retry trong harness (2-3s) thay vì bắt client trả lỗi rồi model sửa (1 step)
                            let bad = calls.iter().find(|c| !call_is_valid(&ctx.tools, c)).map(|c| c.name.clone());
                            if let (Some(n), true) = (bad, attempt < 2) {
                                save_sample("invalid", &full_text);
                                crate::elog!("[toolcall-invalid] attempt {} tool={:?} args thiếu/không hợp lệ — retry; raw={:?}", attempt, n, full_text);
                                last_err = Some(format!("tool-call không hợp lệ ({})", n));
                                // lỗi do model: không phạt acc, NHƯNG phải loại acc khỏi lượt sau —
                                // chạy lại đúng acc + đúng prompt gần như chắc chắn ra lại đúng output hỏng
                                failed.push(acc_id.clone());
                            } else {
                            ok_acc(&st, &acc_id, &sid, !own_session).await;
                            let ids: Vec<String> = calls.iter().map(|_| new_call_id()).collect();
                            register_conv(&st, &ctx, stateful_attempt, &acc_id, &sid, msg_id, &ids, full_text.chars().count(), real_usage.unwrap_or(0));
                            yield Out::ToolCalls(calls, ids);
                            crate::usage::record(prompt_tokens.max(0) as u64, cached_tokens.max(0) as u64, completion_tokens.max(0) as u64);
                            crate::usage::record_request(t_req.elapsed().as_millis() as u64, true);
                            yield Out::Finish { reason: "tool_calls", prompt_tokens, completion_tokens, cached_tokens };
                            finished = true;
                            }
                        } else if let Some(why) = truncated.clone() {
                            crate::elog!("[ds-truncated] attempt {}: {}", attempt, why);
                            last_err = Some(why);
                            if attempt == 2 && has_content && !has_tool_marker(&full_text) {
                                // lượt cuối: trả phần đã có, báo 'length' để client biết bị cắt
                                if !resp_text.is_empty() { yield Out::Content(std::mem::take(&mut resp_text)); }
                                crate::usage::record(prompt_tokens.max(0) as u64, cached_tokens.max(0) as u64, completion_tokens.max(0) as u64);
                            crate::usage::record_request(t_req.elapsed().as_millis() as u64, true);
                            yield Out::Finish { reason: "length", prompt_tokens, completion_tokens, cached_tokens };
                                finished = true;
                            } else {
                                fail_acc(&st, &acc_id, "error", true).await;
                                failed.push(acc_id.clone());
                            }
                        } else if !has_content {
                            last_err = Some("DeepSeek web trả stream rỗng (có thể bị throttle)".into());
                            fail_acc(&st, &acc_id, "empty", true).await;
                            failed.push(acc_id.clone());
                        } else if has_tool_marker(&full_text) {
                            // có marker tool-call nhưng parse trượt: log nguyên văn để vá, retry; lỗi do model nên không phạt acc
                            save_sample("unparsed", &full_text);
                            crate::elog!("[toolcall-unparsed] attempt {}; raw={:?}", attempt, full_text);
                            // lượt cuối vẫn trượt → báo lỗi rõ (không trả completion rỗng làm client 'Stopped' im lặng)
                            last_err = Some("tool-call không parse được sau 3 lần thử".into());
                            fail_acc(&st, &acc_id, "unparsed", false).await;
                            // đổi acc cho lượt sau: lỗi do model nhưng cùng acc + cùng prompt dễ lặp lại y hệt
                            failed.push(acc_id.clone());
                        } else {
                            if !resp_text.is_empty() { yield Out::Content(std::mem::take(&mut resp_text)); }
                            ok_acc(&st, &acc_id, &sid, !own_session).await;
                            register_conv(&st, &ctx, stateful_attempt, &acc_id, &sid, msg_id, &[], full_text.chars().count(), real_usage.unwrap_or(0));
                            crate::usage::record(prompt_tokens.max(0) as u64, cached_tokens.max(0) as u64, completion_tokens.max(0) as u64);
                            crate::usage::record_request(t_req.elapsed().as_millis() as u64, true);
                            yield Out::Finish { reason: "stop", prompt_tokens, completion_tokens, cached_tokens };
                            finished = true;
                        }
                        attempt_done = true;
                    }
                }
                if attempt_done { break; }
            }
            if finished { break; }
        }

        if !finished {
            let msg = last_err.unwrap_or_else(|| "DeepSeek web không trả dữ liệu".into());
            crate::elog!("[ds-fail] {}", msg);
            crate::usage::record_request(t_req.elapsed().as_millis() as u64, false);
            yield Out::Error(msg);
        }
    }
}

/// Rút gọn lịch sử nếu prompt vượt ngân sách (DS_PROMPT_MAX_CHARS, mặc định 1,2M ký tự):
/// 1) cắt giữa các tool_result cũ, 2) bỏ các lượt cũ nhất (giữ system + tin user đầu).
///
/// Chi phí O(n) thay vì O(n²): render từng message đúng một lần để lấy kích thước, sau đó
/// cập nhật tổng bằng phép trừ/cộng. Bản cũ gọi `messages_to_prompt()` lại toàn bộ danh sách
/// sau mỗi lần cắt (đo được 6.7ms cho prompt 485K ký tự, tăng bình phương theo độ dài).
pub(crate) fn fit_messages(messages: &[Value], max_chars: usize) -> Vec<Value> {
    let call_names = call_name_map(messages);
    let mut msgs = messages.to_vec();
    // rendered[i] = đoạn prompt của message i (None = message không đóng góp gì)
    let mut rendered: Vec<Option<String>> = msgs.iter().map(|m| render_msg(m, &call_names)).collect();
    // +1 cho '\n' nối giữa các đoạn (xấp xỉ; sai lệch ≤ vài chục ký tự không ảnh hưởng ngân sách)
    let mut total: usize = rendered.iter().flatten().map(|s| s.chars().count() + 1).sum();
    if total <= max_chars {
        return msgs;
    }

    // bước 1: rút gọn tool_result cũ (giữ nguyên 6 message cuối)
    let keep_from = msgs.len().saturating_sub(6);
    for i in 0..keep_from {
        if msgs[i].get("role").and_then(|r| r.as_str()) == Some("tool") {
            let text = render_content(&msgs[i]);
            let n = text.chars().count();
            if n > 2500 {
                let head: String = text.chars().take(1500).collect();
                let tail: String = text.chars().skip(n - 500).collect();
                msgs[i]["content"] = Value::String(format!("{}\n…[đã rút gọn {} ký tự]…\n{}", head, n - 2000, tail));
                let old = rendered[i].as_ref().map(|s| s.chars().count() + 1).unwrap_or(0);
                rendered[i] = render_msg(&msgs[i], &call_names);
                let new = rendered[i].as_ref().map(|s| s.chars().count() + 1).unwrap_or(0);
                total = total.saturating_sub(old).saturating_add(new);
            }
        }
        if total <= max_chars {
            return msgs;
        }
    }

    // bước 2: bỏ các lượt cũ nhất (không đụng system + tin user đầu), kèm tool_result mồ côi
    let first_user = msgs.iter().position(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")).unwrap_or(0);
    while total > max_chars && msgs.len() > 8 {
        let k = msgs.iter().enumerate()
            .position(|(i, m)| i > first_user && m.get("role").and_then(|r| r.as_str()) != Some("system"));
        let Some(k) = k else { break };
        total = total.saturating_sub(rendered[k].as_ref().map(|s| s.chars().count() + 1).unwrap_or(0));
        msgs.remove(k);
        rendered.remove(k);
        while k < msgs.len() && msgs[k].get("role").and_then(|r| r.as_str()) == Some("tool") {
            total = total.saturating_sub(rendered[k].as_ref().map(|s| s.chars().count() + 1).unwrap_or(0));
            msgs.remove(k); // không để tool_result mồ côi
            rendered.remove(k);
        }
    }
    msgs
}

fn chunk(id: &str, model: &str, delta: Value, finish: Value) -> Event {
    Event::default().data(
        json!({
            "id": id, "object": "chat.completion.chunk", "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
        })
        .to_string(),
    )
}

pub(crate) async fn api_chat_completions(
    State(st): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Response, ApiErr> {
    let model_name = req.model.clone().unwrap_or_else(|| "deepseek-web".into());
    let stream = req.stream.unwrap_or(false);

    // yêu cầu đặt tên phiên của client: không cần thinking (đỡ 4-5s + slot acc)
    let is_title_req = req.tools.as_ref().is_none_or(|t| t.is_empty())
        && req.messages.iter().any(|m| render_content(m).contains("Create a concise title"));
    let effort = req.reasoning_effort.clone()
        .or_else(|| req.reasoning.as_ref().and_then(|r| r.get("effort")).and_then(|e| e.as_str()).map(|s| s.to_string()))
        .or_else(|| req.model.as_deref().and_then(|m| m.split_once(':')).map(|(_, e)| e.to_string())); // "deepseek-web:max"
    // "auto": nghĩ ở lượt người dùng, tắt khi chỉ đang nối tiếp sau tool_result (vòng lặp công cụ).
    // Đo thực tế: bước có think 5–25K ký tự mất 10–44s, bước không think ~2s.
    let auto_effort = effort.as_deref().is_some_and(|e| e.trim().eq_ignore_ascii_case("auto"));
    let last_is_tool = req.messages.last().and_then(|m| m.get("role")).and_then(|r| r.as_str()) == Some("tool");
    let thinking = effort_to_thinking(effort.as_deref(), req.model.as_deref().is_some_and(|m| m.contains("r1")), req.thinking)
        && !(auto_effort && last_is_tool)
        && !is_title_req;
    let search = req.search.unwrap_or(false);

    let max_chars: usize = crate::settings::uint("DS_PROMPT_MAX_CHARS") as usize;
    let msgs = fit_messages(&req.messages, max_chars);
    let tools_hdr = render_tools_spec(&req.tools);
    let prompt = format!("{}\n{}\n", tools_hdr, messages_to_prompt(&msgs));
    crate::elog!(
        "[req] model={} stream={} tools={} msgs={}→{} prompt_chars={} thinking={}",
        model_name, stream, req.tools.as_ref().map_or(0, |t| t.len()), req.messages.len(), msgs.len(),
        prompt.chars().count(), thinking
    );

    // ---- STATEFUL: tìm hội thoại đang nằm trong phiên web khớp prefix của request này
    let n = req.messages.len();
    // `new_thread: true` = client muốn bắt đầu hội thoại mới → không nối tiếp phiên cũ,
    // và dọn luôn phiên khớp prefix (nếu có) để không rò rỉ session trên DS.
    let force_new = req.new_thread.unwrap_or(false);
    let stateful_on = stateful_enabled() && req.tools.as_ref().is_some_and(|t| !t.is_empty()) && !is_title_req;
    let pf = prefix_fps(&req.messages);
    let th = tools_hash(&req.tools);
    let mut cont: Option<ContPlan> = None;
    let mut lease: Option<Lease> = None;
    // cache ảnh dùng chung cho cả request: ảnh trong prefix không bị tải/giải mã lần thứ hai
    let mut img_cache: std::collections::HashMap<String, Option<(Vec<u8>, String)>> = std::collections::HashMap::new();
    if stateful_on && force_new {
        // dọn mọi hội thoại khớp prefix của request này (best-effort).
        // Lưu ý: guard của Mutex phải drop TRƯỚC `.await` → gom kết quả vào Vec trước.
        let stale: Vec<Conv> = {
            let mut convs = st.convs.lk();
            (1..n)
                .filter(|k| req.messages[k - 1].get("role").and_then(|r| r.as_str()) == Some("assistant"))
                .filter_map(|k| convs.remove(&pf[k]))
                .collect()
        };
        for conv in stale {
            crate::elog!("[conv] new_thread=true — bỏ phiên cũ {}", &conv.sid[..8.min(conv.sid.len())]);
            st.sessions.lk().remove(&conv.sid);
            let acc = { st.store.read().await.accounts.iter().find(|a| a.id == conv.acc_id).map(|a| (a.token.clone(), cookie_string(&a.cookies))) };
            spawn_delete_session(acc, conv.sid.clone());
        }
    }
    if stateful_on && !force_new {
        let hit = (1..n).rev()
            .filter(|k| req.messages[k - 1].get("role").and_then(|r| r.as_str()) == Some("assistant"))
            .find_map(|k| st.convs.lk().remove(&pf[k]).map(|c| (k, c)));
        if let Some((k, conv)) = hit {
            let acc_creds = { st.store.read().await.accounts.iter().find(|a| a.id == conv.acc_id).map(|a| (a.token.clone(), cookie_string(&a.cookies))) };
            let usable = acc_creds.is_some() && conv.tools_hash == th && conv.chars <= conv_max_chars();
            let delta = messages_to_prompt_from(&req.messages, k);
            if usable && !delta.trim().is_empty() {
                // cần đúng acc đã giữ phiên; bận quá 4s thì thôi, gửi đủ ở acc khác
                match acquire(&st, &[], Some(&conv.acc_id), Instant::now() + Duration::from_secs(4)).await {
                    Ok(l) => {
                        let delta_images = collect_images_cached(&req.messages[k..], 4, &mut img_cache).await;
                        st.stat_conv_hit.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        crate::elog!("[conv] nối tiếp phiên {} (+{} msg, delta {} ký tự, phiên đã {} ký tự)", &conv.sid[..8.min(conv.sid.len())], n - k, delta.chars().count(), conv.chars);
                        cont = Some(ContPlan { acc_id: conv.acc_id.clone(), sid: conv.sid.clone(), parent_id: conv.parent_id, delta_prompt: delta, delta_images, base_chars: conv.chars, tokens: conv.tokens });
                        lease = Some(l);
                    }
                    Err(_) => {
                        crate::elog!("[conv] acc giữ phiên đang bận — gửi đủ ngữ cảnh ở acc khác");
                        st.convs.lk().insert(pf[k], conv);
                    }
                }
            } else {
                crate::elog!("[conv] bỏ phiên (tools đổi / quá dài / acc mất) — gửi đủ ngữ cảnh");
                st.sessions.lk().remove(&conv.sid);
                spawn_delete_session(acc_creds, conv.sid.clone());
            }
        }
    }
    if stateful_on && cont.is_none() {
        st.stat_conv_miss.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n > 2 { crate::elog!("[conv] miss: {} msg, {} hội thoại đang giữ — gửi đủ ngữ cảnh", n, st.convs.lk().len()); }
    }
    let lease = match lease {
        Some(l) => l,
        None => acquire(&st, &[], None, Instant::now() + Duration::from_secs(queue_wait_secs())).await?,
    };
    let images = collect_images_cached(&msgs, 4, &mut img_cache).await;
    if !images.is_empty() { crate::elog!("[req] {} ảnh đính kèm", images.len()); }
    let reg = if stateful_on { Some(RegPlan { pf_n: pf[n], tools_hash: th }) } else { None };
    let ctx = ChatCtx { prompt, cont, reg, images, thinking, search, tools: req.tools.clone() };
    let mut outs = Box::pin(run_chat(st, ctx, lease));
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());

    if stream {
        let body = async_stream::stream! {
            yield chunk(&id, &model_name, json!({"role": "assistant", "content": ""}), Value::Null);
            while let Some(o) = outs.next().await {
                match o {
                    Out::Reasoning(t) => yield chunk(&id, &model_name, json!({"reasoning_content": t}), Value::Null),
                    Out::Content(t) => yield chunk(&id, &model_name, json!({"content": t}), Value::Null),
                    Out::ToolCalls(calls, ids) => {
                        for (i, c) in calls.iter().enumerate() {
                            yield chunk(&id, &model_name, json!({"tool_calls": [{
                                "index": i, "id": ids[i].clone(), "type": "function",
                                "function": {"name": c.name, "arguments": c.arguments}
                            }]}), Value::Null);
                        }
                    }
                    Out::Finish { reason, prompt_tokens, completion_tokens, cached_tokens } => {
                        yield chunk(&id, &model_name, json!({}), json!(reason));
                        yield Event::default().data(json!({
                            "id": id, "object": "chat.completion.chunk", "model": model_name, "choices": [],
                            "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens,
                                      "total_tokens": prompt_tokens + completion_tokens,
                                      "prompt_tokens_details": {"cached_tokens": cached_tokens}}
                        }).to_string());
                    }
                    Out::Error(m) => {
                        yield Event::default().data(json!({"error": {"message": m, "type": "upstream_error", "code": "ds_upstream_failed"}}).to_string());
                    }
                }
            }
            yield Event::default().data("[DONE]");
        };
        return Ok(Sse::new(body.map(Ok::<_, std::convert::Infallible>))
            .keep_alive(axum::response::sse::KeepAlive::new().interval(Duration::from_secs(10)).text("keep-alive"))
            .into_response());
    }

    // non-stream: gom các sự kiện của cùng một lõi
    let (mut content, mut reasoning) = (String::new(), String::new());
    let mut tool_calls: Vec<Value> = Vec::new();
    let (mut finish, mut pt, mut ct, mut cached) = ("stop", 0i64, 0i64, 0i64);
    while let Some(o) = outs.next().await {
        match o {
            Out::Reasoning(t) => reasoning.push_str(&t),
            Out::Content(t) => content.push_str(&t),
            Out::ToolCalls(calls, ids) => {
                tool_calls = calls.iter().zip(ids.iter()).map(|(c, tid)| json!({
                    "id": tid, "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments}
                })).collect();
            }
            Out::Finish { reason, prompt_tokens, completion_tokens, cached_tokens } => { finish = reason; pt = prompt_tokens; ct = completion_tokens; cached = cached_tokens; }
            Out::Error(m) => {
                return Err((StatusCode::BAD_GATEWAY, Json(json!({"error": {"message": m, "type": "upstream_error", "code": "ds_upstream_failed"}}))));
            }
        }
    }
    let mut message = json!({"role": "assistant", "content": if content.is_empty() && !tool_calls.is_empty() { Value::Null } else { Value::String(content) }});
    if !reasoning.is_empty() { message["reasoning_content"] = Value::String(reasoning); }
    if !tool_calls.is_empty() { message["tool_calls"] = Value::Array(tool_calls); }
    Ok(Json(json!({
        "id": id, "object": "chat.completion", "model": model_name,
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct, "prompt_tokens_details": {"cached_tokens": cached}}
    })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn fingerprint_chain_matches_next_request() {
        let m1 = vec![json!({"role": "system", "content": "s"}), json!({"role": "user", "content": "làm đi"})];
        let pf1 = prefix_fps(&m1);
        let key = hash_pair(pf1[2], "assistant|call_abc,call_def"); // lúc đăng ký sau khi trả lời
        let m2 = vec![
            m1[0].clone(), m1[1].clone(),
            // client echo lại assistant: văn bản có thể khác, id phải giống
            json!({"role": "assistant", "content": "  văn bản đã bị chuẩn hoá ", "tool_calls": [
                {"id": "call_abc", "type": "function", "function": {"name": "bash", "arguments": "{}"}},
                {"id": "call_def", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "call_abc", "content": "ok"}),
        ];
        let pf2 = prefix_fps(&m2);
        assert_eq!(pf2[3], key, "prefix + assistant phải khớp dấu vân tay đã đăng ký");
        // đổi nội dung user ở prefix → không khớp
        let mut m3 = m2.clone(); m3[1] = json!({"role": "user", "content": "làm khác"});
        assert_ne!(prefix_fps(&m3)[3], key);
        // trả lời không tool-call
        assert_eq!(prefix_fps(&[m1[0].clone(), m1[1].clone(), json!({"role": "assistant", "content": "xong"})])[3], hash_pair(pf1[2], "assistant|"));
    }

    #[test]
    fn token_estimator_close_to_measured() {
        // đo thật trên DS web (trừ ~35 token overhead hệ thống): en 413, vi 993, code 712
        let en = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        let vi = "Con cáo nâu nhanh nhẹn nhảy qua con chó lười biếng nằm dưới gốc cây. ".repeat(28);
        let code = "fn main() { let x: Vec<u32> = (0..10).collect(); println!(\"{:?}\", x); }\n".repeat(25);
        for (name, t, real) in [("en", en, 413f64), ("vi", vi, 993f64), ("code", code, 712f64)] {
            let e = est_tokens(&t) as f64;
            assert!((e / real - 1.0).abs() < 0.25, "{}: ước {} vs thật {}", name, e, real);
        }
    }

    #[test]
    fn effort_mapping() {
        assert!(!effort_to_thinking(Some("none"), false, None));
        assert!(effort_to_thinking(Some("max"), false, None));
        assert!(effort_to_thinking(Some("high"), false, None));
        assert!(!effort_to_thinking(None, false, None)); // mặc định Nhanh
        assert!(effort_to_thinking(None, true, None)); // alias cũ *-r1
        assert!(!effort_to_thinking(Some("none"), true, Some(true))); // effort tường minh thắng
    }

    #[test]
    fn fit_messages_shrinks_old_tool_results_then_drops_oldest() {
        let big = "x".repeat(20_000);
        let mut msgs = vec![json!({"role": "system", "content": "sys"}), json!({"role": "user", "content": "task"})];
        for i in 0..6 {
            msgs.push(json!({"role": "assistant", "content": "", "tool_calls": [{"id": format!("c{}", i), "type": "function", "function": {"name": "bash", "arguments": "{}"}}]}));
            msgs.push(json!({"role": "tool", "tool_call_id": format!("c{}", i), "content": big.clone()}));
        }
        msgs.push(json!({"role": "user", "content": "tiếp"}));
        let full = messages_to_prompt(&msgs).chars().count();
        let fitted = fit_messages(&msgs, full / 2);
        let after = messages_to_prompt(&fitted).chars().count();
        assert!(after <= full / 2 + 3000, "phải rút gọn: {} → {}", full, after);
        assert_eq!(fitted[0]["role"], "system");
        assert_eq!(fitted[1]["content"], "task"); // tin user đầu còn nguyên
        assert_eq!(fitted.last().unwrap()["content"], "tiếp"); // tin mới nhất còn nguyên
        // không có tool_result mồ côi (mỗi tool đứng sau assistant có tool_calls)
        for (i, m) in fitted.iter().enumerate() {
            if m["role"] == "tool" { assert!(fitted[i - 1]["role"] == "assistant" || fitted[i - 1]["role"] == "tool"); }
        }
        // ngân sách đủ lớn thì giữ nguyên
        assert_eq!(fit_messages(&msgs, full * 2).len(), msgs.len());
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    #[test]
    fn head_chars_is_char_safe() {
        assert_eq!(head_chars("ảnh-đẹp", 3), "ảnh");
        assert_eq!(head_chars("ab", 5), "ab");
    }
    #[test]
    fn ssrf_blocks_internal() {
        for u in ["http://127.0.0.1/x", "http://[::1]/", "http://192.168.1.5/", "http://169.254.169.254/", "file:///etc/passwd", "http://localhost:8790/"] {
            assert!(!url_is_public(&reqwest::Url::parse(u).unwrap()), "{u}");
        }
        assert!(url_is_public(&reqwest::Url::parse("https://example.com/a.png").unwrap()));
    }
}

#[cfg(test)]
mod auto_effort_tests {
    use super::*;
    #[test]
    fn auto_counts_as_thinking_before_tool_gating() {
        assert!(effort_to_thinking(Some("auto"), false, None));
    }
}
