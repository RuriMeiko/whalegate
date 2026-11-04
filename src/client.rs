// Client: HTTP calls to chat.deepseek.com API + completion flow
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{json, Value};

use crate::pow::solve_pow;

pub const DEEPSEEK_API: &str = "https://chat.deepseek.com/api/v0";

/// Test chạy với server giả cục bộ: đổi base URL ở đây; bản chạy thật luôn dùng DEEPSEEK_API.
#[cfg(test)]
static TEST_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
fn api_base() -> String {
    #[cfg(test)]
    if let Some(b) = TEST_BASE.lock().unwrap_or_else(|e| e.into_inner()).clone() { return b; }
    DEEPSEEK_API.to_string()
}

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
    /// x-client-timezone-offset / accept-language của acc: acc bookmarklet mang giá trị thật của trình duyệt gốc
    tz_offset: String,
    accept_language: String,
    /// `shumei_device_id` web gửi kèm completion/continue: "B" + giải mã cookie `.thumbcache_*` (SDK Shumei đặt, đổi mỗi lần tải trang)
    shumei_id: Option<String>,
}

/// UUID-dạng ổn định dẫn xuất từ token (FNV-1a 128 bit giả lập bằng 2 lần băm) — mỗi acc một "thiết bị".
fn device_id_for(token: &str) -> String {
    let h = |seed: u64| token.bytes().fold(seed, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3));
    let (a, b) = (h(0xcbf29ce484222325), h(0x84222325cbf29ce4));
    format!("{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}", (a >> 32) as u32, (a >> 16) as u16, a as u16 & 0xfff, (b >> 52) as u16 & 0xfff, b & 0xffff_ffff_ffff)
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
    /// Web luôn gửi trường này (cuối body); chưa có thì `null` như khi SDK chưa kịp trả mã.
    shumei_device_id: Option<&'a str>,
}

impl<'a> CompletionBody<'a> {
    fn new(sid: &'a str, parent: Option<i64>, prompt: &'a str, refs: &'a [String], thinking: bool, search: bool, shumei: Option<&'a str>) -> Self {
        Self { chat_session_id: sid, parent_message_id: parent, model_type: "default", prompt, ref_file_ids: refs, thinking_enabled: thinking, search_enabled: search, action: None, preempt: false, shumei_device_id: shumei }
    }
    fn into_body(self) -> Vec<u8> { serde_json::to_vec(&self).unwrap_or_default() }
}

/// Chọn proxy đầu ra cho token này, dựa trên vân tay của acc.
///
/// Chế độ:
///   off    → không proxy, không bao giờ chạm file ghim
///   auto   → có proxy thì dùng; pool rỗng thì đi thẳng (có log)
///   strict → pool rỗng/sai cấu hình thì BÁO LỖI: thà acc không chạy được còn hơn chạy với IP thật
///
/// Trả (proxy để gắn vào client, chuỗi gốc để so cache).
fn pick_proxy(token: &str) -> Result<(Option<wreq::Proxy>, Option<String>)> {
    let pool = crate::proxypool::snapshot();
    if pool.mode == crate::proxypool::Mode::Off {
        return Ok((None, None));
    }
    if !pool.problems.is_empty() {
        for p in &pool.problems {
            crate::elog!("[proxy] cấu hình lỗi: {}", p);
        }
    }
    // ghim theo SEED của vân tay, không theo token: acc bị xoá rồi thêm lại (token mới) vẫn giữ nguyên IP
    let fp_seed = crate::fingerprint::seed_for(token);
    let entry = crate::proxypool::proxy_for(&fp_seed, &pool);
    let Some(entry) = entry else {
        return match pool.mode {
            crate::proxypool::Mode::Strict => Err(anyhow::anyhow!(
                "chế độ proxy=strict nhưng không có proxy nào dùng được ({})",
                if pool.problems.is_empty() { "danh sách rỗng".to_string() } else { pool.problems.join("; ") }
            )),
            _ => {
                crate::elog!("[proxy] acc {} không có proxy — đi thẳng (chế độ auto)", &fp_seed[..8.min(fp_seed.len())]);
                Ok((None, None))
            }
        };
    };
    crate::elog!("[proxy] vân tay {} → {}", &fp_seed[..8.min(fp_seed.len())], entry.masked());
    let p = entry.to_wreq()?;
    Ok((Some(p), Some(entry.raw)))
}

/// Dựng client mới cho token: chọn proxy (ghim theo vân tay) rồi trả (Proxy, chuỗi gốc).
/// Tách khỏi `pick_proxy` để chỗ cache và chỗ build dùng CÙNG một quyết định.
fn resolve_proxy(token: &str) -> Result<(Option<wreq::Proxy>, Option<String>)> {
    pick_proxy(token)
}

struct Slot {
    client: wreq::Client,
    jar: std::sync::Arc<wreq::cookie::Jar>,
    seeded: std::sync::Mutex<std::collections::HashMap<String, String>>,
    /// proxy đầu ra đã dùng để dựng client này (None = đi thẳng). Khoá pool là token, nên phải
    /// nhớ proxy để phát hiện ghim đổi mà dựng lại — nếu không, đổi proxy cho acc sẽ không có tác dụng
    /// cho tới khi restart.
    proxy: Option<String>,
}

fn slot_pool() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Slot>>> {
    static POOL: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Slot>>>> = std::sync::OnceLock::new();
    POOL.get_or_init(Default::default)
}

/// Bỏ client (kết nối + jar) đã dựng cho token: vân tay của token vừa đổi/gỡ thì lần sau phải dựng lại theo vân tay mới.
pub fn forget(token: &str) {
    slot_pool().lock().unwrap_or_else(|e| e.into_inner()).remove(token);
}

/// Bỏ TẤT CẢ client đã dựng. Dùng khi cấu hình proxy đổi (danh sách/mode): mọi client cũ đang đi
/// theo quyết định cũ, phải dựng lại thì ghim mới mới có hiệu lực.
pub fn forget_all() {
    slot_pool().lock().unwrap_or_else(|e| e.into_inner()).clear();
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
            .header("accept-language", &self.accept_language)
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-site")
            .timeout(t3)
            .send().await.ok()?;
        let resp = self.http
            .get(&url)
            .header("accept", "*/*")
            .header("accept-language", &self.accept_language)
            .header("origin", "https://chat.deepseek.com")
            .header("referer", "https://chat.deepseek.com/")
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-site")
            .header("x-client-bundle-id", "com.deepseek.chat")
            .header("x-client-locale", "vi")
            .header("x-client-platform", "web")
            .header("x-client-timezone-offset", &self.tz_offset)
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
        #[cfg(test)]
        if TEST_BASE.lock().unwrap_or_else(|e| e.into_inner()).is_some() { return rb; } // test với server giả: không gọi hif thật
        self.ensure_hif().await;
        let (l, d) = hif_cache().lock().unwrap_or_else(|e| e.into_inner()).get(&self.token).map(|h| (h.leim.current(), h.dliq.current())).unwrap_or((None, None));
        let rb = match l { Some(v) => rb.header("x-hif-leim", v), None => rb };
        match d { Some(v) => rb.header("x-hif-dliq", v), None => rb }
    }

    pub fn new(token: String, cookies: String) -> Result<Self> {
        // Một wreq::Client (connection pool + cookie jar) cho mỗi acc: tái dùng TCP/TLS/HTTP2 giữa các request thay vì
        // bắt tay lại mỗi lần, và mỗi acc vẫn có kết nối + cookie riêng như một trình duyệt riêng.
        let pool = slot_pool();
        // Cache theo token, nhưng proxy thì ghim theo VÂN TAY: nếu ghim đổi (đổi danh sách proxy,
        // xoá file ghim, đổi mode) thì client cũ đang đi sai IP — phải dựng lại.
        let want_proxy = pick_proxy(&token)?.1;
        let cached = pool.lock().unwrap_or_else(|e| e.into_inner()).get(&token).cloned();
        let cached = cached.filter(|s| s.proxy == want_proxy);
        let slot = match cached {
            Some(c) => c,
            None => {
                let jar = std::sync::Arc::new(wreq::cookie::Jar::default());
                // IP đầu ra của acc: ghim theo VÂN TAY (không theo token) — hai acc cùng "máy" phải cùng IP,
                // hai acc khác máy phải khác IP. Xem src/proxypool.rs.
                let (proxy, proxy_raw) = resolve_proxy(&token)?;
                let mut builder = wreq::Client::builder()
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
                    .pool_idle_timeout(std::time::Duration::from_secs(60));
                if let Some(p) = proxy {
                    builder = builder.proxy(p);
                }
                let client = builder.build()?;
                let slot = std::sync::Arc::new(Slot { client, jar, seeded: Default::default(), proxy: proxy_raw });
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
        let fp = crate::fingerprint::Fingerprint::for_token(&token);
        let shumei_id = shumei_id_from_cookies(&cookies);
        Ok(Self { http, token, device_id, tz_offset: fp.tz_offset_header(), accept_language: fp.accept_language(), shumei_id })
    }

    /// Bảo đảm acc có `ds_session_id` như trình duyệt thật. Cookie này HttpOnly (bookmarklet không đọc được) và chỉ
    /// được server web của DS đặt ở response ngoài `/api` khi request chưa mang nó — trình duyệt nhận nó lúc tải
    /// favicon/trang đầu tiên. whalegate chỉ gọi `/api` nên tự làm đúng bước đó: một lượt `GET /favicon.ico` qua
    /// client của acc (cùng vân tay), jar nhận Set-Cookie. Trả giá trị cookie (đã có sẵn thì trả luôn, không gọi gì).
    pub async fn ensure_session_cookie(&self) -> Result<String> {
        let has = || jar_snapshot(&self.token).and_then(|m| m.get("ds_session_id").cloned());
        if let Some(v) = has() { return Ok(v); }
        let resp = self.http
            .get("https://chat.deepseek.com/favicon.ico")
            .header("accept", "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8")
            .header("accept-language", &self.accept_language)
            .header("referer", "https://chat.deepseek.com/")
            .header("sec-fetch-dest", "image")
            .header("sec-fetch-mode", "no-cors")
            .header("sec-fetch-site", "same-origin")
            .send()
            .await?;
        let status = resp.status();
        // AWS WAF chặn thì trả 202 + x-amzn-waf-action (không tới server web, không có cookie)
        let waf = resp.headers().get("x-amzn-waf-action").and_then(|v| v.to_str().ok()).map(str::to_string);
        let _ = resp.bytes().await;
        if let Some(v) = has() { return Ok(v); }
        match waf {
            Some(a) => anyhow::bail!("AWS WAF đòi {} khi lấy ds_session_id", a),
            None => anyhow::bail!("DS không đặt ds_session_id (favicon.ico -> {})", status),
        }
    }

    fn req(&self, method: wreq::Method, path: &str) -> wreq::RequestBuilder {
        self.req_to(method, format!("{}{}", api_base(), path))
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
            .header("accept-language", &self.accept_language)
            .header("sec-fetch-dest", "empty")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-origin")
            .header("x-client-platform", "web")
            .header("x-client-version", "2.7.0")
            .header("x-client-locale", "vi")
            .header("x-client-bundle-id", "com.deepseek.chat")
            .header("x-client-timezone-offset", &self.tz_offset)
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

    /// Những gì web làm khi mở trang rồi để tab đó: lúc tải làm một lượt (users/current, 4 scope settings, banner,
    /// check_device, telemetry, settings/report), sau đó mỗi ~300s nạp lại từng scope settings (lệch pha nhau), và mỗi
    /// scope có kho (main/model/provider) báo một telemetry `loadRemoteFeaturesSuccess` (→ ~1 gói / 100s). Đo trên tab
    /// web 2.7.0 để yên ~56 phút. Mỗi acc một vòng nền, tự dừng khi acc không dùng 30 phút. DS_WEB_BOOT=0 để tắt.
    pub fn web_boot_in_background(&self) {
        if std::env::var("DS_WEB_BOOT").map(|v| v == "0").unwrap_or(false) { return; }
        type Map = std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>;
        static USED: std::sync::OnceLock<Map> = std::sync::OnceLock::new();
        static RUNNING: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
        USED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
            .insert(self.token.clone(), std::time::Instant::now());
        {
            let mut r = RUNNING.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
            if !r.insert(self.token.clone()) { return; }
        }
        let me = self.clone();
        tokio::spawn(async move {
            let idle = |t: &str| USED.get().and_then(|m| m.lock().ok().and_then(|m| m.get(t).map(|i| i.elapsed().as_secs()))).unwrap_or(u64::MAX);
            match me.web_boot().await {
                Ok((uid, did, session)) => {
                    let rnd = |lo: u64, hi: u64| lo + (uuid::Uuid::new_v4().as_u128() % (hi - lo + 1) as u128) as u64;
                    'outer: loop {
                        // ~300s mỗi vòng, 4 scope rải đều, mỗi bước ±20%
                        let mut order = vec!["web_upgrade", "main", "provider", "model"];
                        let n = order.len();
                        for i in (1..n).rev() { order.swap(i, (uuid::Uuid::new_v4().as_u128() % (i as u128 + 1)) as usize); }
                        for scope in order {
                            tokio::time::sleep(std::time::Duration::from_secs(rnd(60, 90))).await;
                            if idle(&me.token) > 1800 { break 'outer; }
                            if let Err(e) = me.refresh_scope(scope, &uid, &did, &session).await { crate::elog!("[boot] refresh {scope}: {:#}", e); }
                        }
                    }
                }
                Err(e) => crate::elog!("[boot] {:#}", e),
            }
            RUNNING.get().map(|r| r.lock().unwrap_or_else(|e| e.into_inner()).remove(&me.token));
        });
    }

    fn feature_did(&self) -> String {
        // did của kho cấu hình từ xa (`__ds_remote_feature_did`): id riêng của web, KHÁC x-device-id; ổn định theo thiết bị
        let d = self.device_id.replace('-', "");
        let x = |i: usize| u64::from_str_radix(&d[i..i + 8], 16).unwrap_or(0).wrapping_mul(0x9e37_79b9).rotate_left(7);
        format!("{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}", x(0) as u32, x(8) as u16, x(16) as u16 & 0xfff, x(24) as u16 & 0xfff, (x(0) ^ x(16)) & 0xffff_ffff_ffff)
    }

    /// Một telemetry `loadRemoteFeaturesSuccess` cho kho `key` (gzip → AES-CBC → RSA-OAEP).
    async fn feature_telemetry(&self, key: &str, session: &str, uid: &str, did: &str) -> Result<()> {
        use std::io::Write;
        let fp = crate::fingerprint::Fingerprint::for_token(&self.token);
        let (w, h) = (fp.avail.0, fp.avail.1.saturating_sub(80));
        let body = serde_json::json!({
            "events": [{"event_name": "loadRemoteFeaturesSuccess", "event_params": {
                "__appVersion": "2.7.0", "__commitId": "26090614", "__runtimeSessionId": session,
                "storageKey": key, "__windowWidth": w, "__windowHeight": h, "__documentHidden": "false",
                "event_level": "info", "event_message": "成功加载并存储远程规则"}}],
            "extra": {"uid": uid, "ds_did": did}
        });
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(body.to_string().as_bytes())?;
        let (ct, ek) = crate::register::encrypt_bytes_with(&gz.finish()?, crate::register::TELEMETRY_PUB_B64)?;
        let r = self.req(wreq::Method::POST, "/telemetry")
            .header("content-type", "application/octet-stream")
            .header("x-ds-compression", "gzip")
            .header("x-request-encryption-key", ek)
            .body(ct).send().await?;
        let st = r.status();
        let txt = r.text().await.unwrap_or_default();
        crate::elog!("[boot] telemetry({key}) -> {st} {}", txt.chars().take(80).collect::<String>());
        Ok(())
    }

    /// Nạp lại một scope settings như web làm định kỳ; scope có kho thì báo telemetry sau đó.
    async fn refresh_scope(&self, scope: &str, uid: &str, did: &str, session: &str) -> Result<()> {
        let _ = self.req(wreq::Method::GET, &format!("/client/settings?did={}&scope={}", did, scope))
            .send().await?.bytes().await?;
        let key = match scope {
            "main" => "__ds_remote_feature_store",
            "model" => "__ds_remote_feature_store_model",
            "provider" => "__ds_remote_feature_store_provider",
            _ => return Ok(()),
        };
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        self.feature_telemetry(key, session, uid, did).await
    }

    async fn web_boot(&self) -> Result<(String, String, String)> {
        let pause = |ms: u64| tokio::time::sleep(std::time::Duration::from_millis(ms));
        let get = |path: String| async move {
            let v: Value = self.req(wreq::Method::GET, &path).send().await?.json().await.unwrap_or(Value::Null);
            Ok::<Value, anyhow::Error>(v)
        };
        let feat_did = self.feature_did();
        let session = format!("session_v0_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);

        // 1) như web lúc tải trang: users/current, rồi settings các scope
        let cur = self.users_current().await?;
        let uid = cur["data"]["biz_data"]["id"].as_str().unwrap_or("").to_string();
        let sso = cur["data"]["biz_data"]["id_profile"]["id"].as_str().unwrap_or("").to_string();
        let mut ids: Vec<i64> = Vec::new();
        let mut stored: Vec<&str> = Vec::new();
        for (scope, key) in [("main", Some("__ds_remote_feature_store")), ("model", Some("__ds_remote_feature_store_model")),
                             ("web_upgrade", None), ("provider", Some("__ds_remote_feature_store_provider"))] {
            let v = get(format!("/client/settings?did={}&scope={}", feat_did, scope)).await?;
            if let Some(m) = v["data"]["biz_data"]["settings"].as_object() {
                if let Some(k) = key { stored.push(k); for e in m.values() { if let Some(i) = e["id"].as_i64() { ids.push(i); } } }
            }
            pause(60).await;
        }
        // 2) danh sách chat, banner, kiểm tra thiết bị
        let _ = get("/chat_session/fetch_page?lte_cursor.pinned=false".into()).await?;
        let _ = get(format!("/client/settings?did={}&scope=banner", feat_did)).await?;
        pause(150).await;
        let _ = self.req(wreq::Method::POST, "/users/auth_token/check_device")
            .json(&serde_json::json!({"device_id": self.device_id, "device_model": ""})).send().await?.bytes().await;

        // 3) mỗi kho nạp xong, web báo một telemetry loadRemoteFeaturesSuccess (gzip → AES-CBC → RSA-OAEP)
        for key in &stored {
            self.feature_telemetry(key, &session, &uid, &feat_did).await?;
            pause(250).await;
        }

        // 4) báo các cấu hình đã nạp
        let r = self.req(wreq::Method::POST, "/client/settings/report")
            .json(&serde_json::json!({"settings_ids": ids, "did": feat_did, "sso_id": sso})).send().await?;
        let st = r.status();
        let txt = r.text().await.unwrap_or_default();
        crate::elog!("[boot] settings/report ({} ids) -> {st} {}", ids.len(), txt.chars().take(100).collect::<String>());
        Ok((uid, feat_did, session))
    }

    pub async fn create_session(&self) -> Result<String> {
        self.web_boot_in_background();
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

    /// Lỗi toàn cục của DS web (HTTP 200, `code` khác 0 ở gốc JSON) — cùng bảng mã web dùng
    /// (`GLOBAL_ERROR_BIZ_CODE`). Mỗi loại cần xử lý khác nhau nên gắn tiền tố riêng cho tầng trên phân loại.
    fn check_auth(v: &Value) -> Result<()> {
        if let Some(e) = classify_global_code(v) { return Err(e); }
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
            .body(CompletionBody::new(&sid, parent_message_id, prompt, &[], thinking, search, self.shumei_id.as_deref()).into_body())
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

    let t_req = std::time::Instant::now(); // mốc từ lúc nhận request (tạo session + PoW + chờ header đều tính)
    // Luồng nền "giống web" (settings/telemetry) tính acc đang dùng theo MỖI request, không chỉ lúc tạo session mới:
    // acc chỉ chạy nối tiếp trong phiên cũ vẫn phải được giữ vòng nền, và vòng chỉ dừng khi acc thật sự nghỉ.
    self.web_boot_in_background();
    let sid = match session_id {
        Some(s) => s.to_string(),
        None => self.create_session().await?,
    };

    let pow_header = self.completion_pow().await?;

    let rb = self.with_hif(self.req(wreq::Method::POST, "/chat/completion").header("x-ds-pow-response", pow_header)).await;
    let resp = rb
        .header("referer", format!("https://chat.deepseek.com/a/chat/s/{}", sid))
        .header("content-type", "application/json")
        .body(CompletionBody::new(&sid, parent_message_id, prompt, ref_file_ids, thinking, search, self.shumei_id.as_deref()).into_body())
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
    // Lỗi ở completion về dạng JSON thường (không phải SSE): token chết, đòi hCaptcha, PoW sai… Đọc hết rồi phân
    // loại — nếu để vòng SSE xử lý, các dòng này không có tiền tố `data:` nên bị bỏ qua và acc bị coi là "throttle".
    let is_json = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.starts_with("application/json"));
    if is_json {
        let body = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        if let Some(e) = classify_global_code(&v) { return Err(e); }
        anyhow::bail!("completion trả JSON thay vì stream: {}", body.chars().take(300).collect::<String>());
    }

    crate::logbuf::debug("completion_sent", json!({
        "sid": sid, "parent": parent_message_id, "prompt_chars": prompt.chars().count(), "refs": ref_file_ids.len(),
        "thinking": thinking, "search": search, "shumei_id": self.shumei_id.is_some(), "status": status.as_u16(),
        "ms_to_headers": t_req.elapsed().as_millis() as u64,
    }));
    let (tx, rx) = mpsc::unbounded_channel::<StreamEv>();
    let me = self.clone();
    let sid_task = sid.clone();
    tokio::spawn(async move {
        let t0 = t_req;
        let mut state = SseState::default();
        let mut stats = PumpStats::default();
        let end = pump(resp, &mut state, &tx, &mut stats, t0).await;
        let mut truncated: Option<String> = end.why();
        // Như web (autoResume): stream dứt khi đã có chữ mà chưa thấy `event: close` → xin server nối lại cùng tin nhắn
        // thay vì bỏ cả câu trả lời rồi gửi lại toàn bộ ngữ cảnh sang acc khác.
        if state.any_delta && !state.complete() && state.resp_mid.is_some() {
            crate::elog!("[ds-resume] stream dứt ({}) sau {} ký tự — nối lại cùng tin nhắn", end.why().unwrap_or_else(|| "kết thúc mà không có close".into()), state.emitted[0] + state.emitted[1]);
            match me.auto_resume(&sid_task, &mut state, &tx, &mut stats).await {
                Ok(()) => { truncated = None; }
                Err(why) => { crate::elog!("[ds-resume] không nối lại được: {}", why); if truncated.is_none() { truncated = Some(why); } }
            }
        }
        crate::logbuf::debug("stream_end", json!({
            "sid": sid_task, "end": end.why().unwrap_or_else(|| "eof".into()), "complete": state.complete(), "resumed": state.resume,
            "resp_mid": state.resp_mid, "forwarded_chars": {"response": state.emitted[0], "think": state.emitted[1]},
            "bytes": stats.total_bytes, "first_byte_ms": stats.first_byte_ms, "total_ms": t0.elapsed().as_millis() as u64,
            "truncated": truncated.is_some(),
            // đuôi SSE chỉ ghi khi có gì bất thường (đứt / không có close / không có chữ)
            "tail": if !state.complete() || truncated.is_some() || !state.has_content { json!(stats.tail) } else { Value::Null },
        }));
        crate::elog!("[ds-timing] first_byte={}ms total={}ms bytes={} sse_chars={}", stats.first_byte_ms.unwrap_or(0), t0.elapsed().as_millis(), stats.total_bytes, stats.sse_chars);
        if !state.has_content {
            crate::elog!("[ds-stream] stream không có fragment RESPONSE nào (nghi throttle/rỗng)");
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            crate::logbuf::append_jsonl("samples.jsonl", json!({"t": t, "kind": "no-response", "raw": stats.raw_head}).to_string(), None);
        }
        if let Some(r) = truncated { let _ = tx.send(StreamEv::Truncated(r)); }
        let _ = tx.send(StreamEv::Done);
    });

    Ok((sid, rx))
}

}


/// Giải mã phần trăm (`%3D` → `=`): giá trị cookie web lưu dạng đã mã hoá URL.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `shumei_device_id` của web = "B" + giải mã giá trị cookie `.thumbcache_<hash>` (đo trên web 2.7.0: body completion có
/// `shumei_device_id` trùng cookie đó đúng từng ký tự). SDK Shumei đổi nó mỗi lần tải trang nên không thể "lưu một lần":
/// lấy theo cookie hiện có của acc (Chrome làm mới cookie thì mã mới theo). Server không bắt buộc trường này (thử: bỏ,
/// `null`, mã cũ, mã rác đều qua) nhưng web luôn gửi nên gửi cho giống.
pub(crate) fn shumei_id_from_cookies(cookies: &str) -> Option<String> {
    cookies.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k.starts_with(".thumbcache_") && !v.is_empty()).then(|| format!("B{}", pct_decode(v)))
    })
}

type Tx = tokio::sync::mpsc::UnboundedSender<StreamEv>;

/// DS ngừng gửi (không byte nào trong STALL_SECS) → coi là treo, thoát để thử nối lại / server retry thay vì chờ vô hạn
const STALL_SECS: u64 = 90;
/// Giữ phần đầu SSE thô: stream không có RESPONSE thì ghi ra samples.jsonl để biết DS thật sự trả gì
const RAW_KEEP: usize = 256 * 1024;

/// Nối đường dẫn vá: `join("response/fragments", "-1")` = `response/fragments/-1`; vế rỗng giữ nguyên gốc.
fn join_path(base: &str, rel: &str) -> String {
    if rel.is_empty() { base.to_string() } else if base.is_empty() { rel.to_string() } else { format!("{}/{}", base, rel) }
}

/// Trạng thái parse SSE của MỘT câu trả lời, giữ xuyên các lần nối lại (autoResume / nút "Tiếp tục" của web).
///
/// Giao thức web (đã đối chiếu với luồng thật, web 2.7.0): mỗi `data:` là một lệnh vá `{p, o, v}` lên tin nhắn trả lời.
/// - `p` (đường dẫn) và `o` (thao tác) thiếu thì kế thừa của event trước; `{"v":" chữ"}` trơn nghĩa là nối tiếp lệnh trước.
/// - `o` có thể là `APPEND` (nối chuỗi / thêm phần tử mảng), `SET`, hoặc `BATCH` (mảng lệnh con, đường dẫn con TƯƠNG ĐỐI,
///   lồng được nhiều tầng; lệnh con thiếu `o` là SET).
/// - event đầu là ảnh chụp `{"v":{"response":{…,"fragments":[…]}}}`.
///
/// `resume_stream` và `continue` gửi lại tin nhắn từ ĐẦU: ảnh chụp tích luỹ rồi mới tới delta (web `upsertAssistantMessage` event
/// đầu rồi áp patch). Vì vậy khi nối lại chỉ chuyển tiếp phần vượt quá số ký tự đã gửi, tính riêng cho RESPONSE và THINK.
#[derive(Default)]
struct SseState {
    /// loại của từng fragment theo thứ tự (THINK, RESPONSE, TOOL_SEARCH, …) để biết chữ nối vào đâu
    frag_types: Vec<String>,
    last_p: String,
    last_o: String,
    /// đã thấy fragment RESPONSE (kể cả rỗng) — dùng nhận diện stream rỗng/throttle
    has_content: bool,
    /// đã chuyển tiếp ít nhất một ký tự (RESPONSE hoặc THINK) — tương ứng `delta` của web
    any_delta: bool,
    sent_msg_id: bool,
    resp_mid: Option<i64>,
    /// đã thấy `event: close` (hoặc `finish`): câu trả lời kết thúc bình thường
    saw_close: bool,
    resume: bool,
    base: [usize; 2],
    recv: [usize; 2],
    emitted: [usize; 2],
}

impl SseState {
    fn complete(&self) -> bool { self.saw_close }

    /// Bắt đầu một luồng nối lại: mọi thứ nhận được tính từ đầu, chỉ phần vượt `base` mới chuyển tiếp.
    fn begin_resume(&mut self) {
        self.resume = true;
        self.base = self.emitted;
        self.recv = [0, 0];
        self.frag_types.clear();
        self.last_p.clear();
        self.last_o.clear();
    }

    fn emit(&mut self, think: bool, s: &str, tx: &Tx) {
        let k = think as usize;
        let out: String = if self.resume {
            let n = s.chars().count();
            let before = self.recv[k];
            self.recv[k] += n;
            let skip = self.base[k];
            if self.recv[k] <= skip { return; }
            s.chars().skip(skip.saturating_sub(before)).collect()
        } else {
            s.to_string()
        };
        let n = out.chars().count();
        self.emitted[k] += n;
        if n > 0 { self.any_delta = true; }
        let _ = tx.send(if think { StreamEv::Thinking(out) } else { StreamEv::Response(out) });
    }

    /// Đồng bộ tới một văn bản đầy đủ (tin nhắn nhận trọn từ server): chỉ gửi phần chưa gửi.
    fn sync_to(&mut self, think: bool, full: &str, tx: &Tx) {
        let k = think as usize;
        let n = full.chars().count();
        if n > self.emitted[k] {
            let tail: String = full.chars().skip(self.emitted[k]).collect();
            self.emitted[k] = n;
            self.any_delta = true;
            let _ = tx.send(if think { StreamEv::Thinking(tail) } else { StreamEv::Response(tail) });
        }
    }

    /// Chữ của một fragment loại `ty`: RESPONSE / THINK được chuyển tiếp, loại khác (tool, search…) bỏ qua.
    fn text_for(&mut self, ty: &str, c: &str, tx: &Tx) {
        match ty {
            "RESPONSE" => { self.has_content = true; self.emit(false, c, tx); }
            "THINK" => self.emit(true, c, tx),
            _ => {}
        }
    }

    /// Fragment mới (APPEND vào `fragments`): ghi nhận loại rồi chuyển tiếp nội dung ban đầu.
    fn open_frags(&mut self, arr: &[Value], tx: &Tx) {
        for f in arr {
            let ty = f["type"].as_str().unwrap_or("").to_string();
            self.frag_types.push(ty.clone());
            self.text_for(&ty, f["content"].as_str().unwrap_or(""), tx);
            // RESPONSE rỗng vẫn tính là đã mở (stream rỗng/throttle khác stream có fragment)
        }
    }

    /// Loại fragment mà đường dẫn `…/fragments/<idx>/…` trỏ tới (idx âm đếm từ cuối); không có idx thì lấy fragment cuối.
    fn type_at(&self, path: &str) -> String {
        let segs: Vec<&str> = path.split('/').collect();
        let n = self.frag_types.len() as i64;
        let idx = segs.iter().position(|s| *s == "fragments").and_then(|i| segs.get(i + 1)).and_then(|s| s.parse::<i64>().ok());
        let i = match idx { Some(i) if i < 0 => n + i, Some(i) => i, None => n - 1 };
        if i >= 0 && i < n { self.frag_types[i as usize].clone() } else { String::new() }
    }

    /// Một dòng SSE đã tách (không còn `\n`): `event: close|finish` hoặc `data: {…}`.
    fn handle_line(&mut self, line: &str, tx: &Tx) {
        if let Some(ev) = line.strip_prefix("event:") {
            if matches!(ev.trim(), "close" | "finish") { self.saw_close = true; }
            return;
        }
        let Some(data) = line.strip_prefix("data: ") else { return };
        let Ok(v) = serde_json::from_str::<Value>(data) else { return };
        self.feed(&v, tx);
    }

    fn feed(&mut self, v: &Value, tx: &Tx) {
        if !self.sent_msg_id {
            if let Some(rid) = v.get("response_message_id").and_then(|x| x.as_i64()) {
                let _ = tx.send(StreamEv::MsgId(rid));
                self.sent_msg_id = true;
                self.resp_mid = Some(rid);
            }
        }
        // ảnh chụp: fragments[] đủ nội dung đến lúc này. Luồng thường chỉ mang fragment đầu (lấy cái cuối);
        // khi nối lại là ảnh chụp tích luỹ → duyệt đủ mọi fragment
        if let Some(frags) = v.get("v").and_then(|x| x.get("response")).and_then(|r| r.get("fragments")).and_then(|f| f.as_array()) {
            self.frag_types = frags.iter().map(|f| f["type"].as_str().unwrap_or("").to_string()).collect();
            self.last_p.clear();
            self.last_o.clear();
            let pick: &[Value] = if self.resume { frags } else { frags.last().map(std::slice::from_ref).unwrap_or(&[]) };
            for f in pick {
                let ty = f["type"].as_str().unwrap_or("");
                self.text_for(ty, f["content"].as_str().unwrap_or(""), tx);
            }
            return;
        }
        if let Some(p) = v.get("p").and_then(|x| x.as_str()) { self.last_p = p.to_string(); }
        if let Some(o) = v.get("o").and_then(|x| x.as_str()) { self.last_o = o.to_string(); }
        let Some(val) = v.get("v") else { return };
        let (p, o) = (self.last_p.clone(), self.last_o.clone());
        self.apply(&p, &o, val, tx);
    }

    fn apply(&mut self, path: &str, op: &str, val: &Value, tx: &Tx) {
        match op {
            "BATCH" => {
                if let Some(arr) = val.as_array() {
                    for it in arr {
                        let rel = it.get("p").and_then(|x| x.as_str()).unwrap_or("");
                        let o = it.get("o").and_then(|x| x.as_str()).unwrap_or("SET");
                        if let Some(iv) = it.get("v") { self.apply(&join_path(path, rel), o, iv, tx); }
                    }
                }
            }
            "SET" => {
                // token thật: {"p":"accumulated_token_usage","v":346} (thường nằm trong BATCH)
                if path.ends_with("accumulated_token_usage") {
                    if let Some(n) = val.as_u64() { let _ = tx.send(StreamEv::Usage(n)); }
                }
            }
            // thiếu `o` hẳn (event trơn đầu tiên) coi như nối tiếp, như hành vi cũ
            "APPEND" | "" => self.append(path, val, tx),
            _ => {}
        }
    }

    fn append(&mut self, path: &str, val: &Value, tx: &Tx) {
        if path.ends_with("fragments") {
            if let Some(arr) = val.as_array() { self.open_frags(arr, tx); }
        } else if path.ends_with("content") || path.is_empty() {
            if let Some(c) = val.as_str() {
                let ty = self.type_at(path);
                self.text_for(&ty, c, tx);
            }
        }
    }
}

#[derive(Default)]
struct PumpStats { first_byte_ms: Option<u128>, total_bytes: usize, sse_chars: usize, raw_head: String, tail: std::collections::VecDeque<String> }

enum PumpEnd { Eof, Stall, Err(String) }

impl PumpEnd {
    fn why(&self) -> Option<String> {
        match self {
            PumpEnd::Eof => None,
            PumpEnd::Stall => Some(format!("DeepSeek ngừng gửi dữ liệu {}s", STALL_SECS)),
            PumpEnd::Err(e) => Some(format!("stream lỗi giữa chừng: {}", e)),
        }
    }
}

/// Đọc một phản hồi SSE tới hết (hoặc treo / lỗi), đẩy event cho `tx` qua `st`.
async fn pump(resp: wreq::Response, st: &mut SseState, tx: &Tx, stats: &mut PumpStats, t0: std::time::Instant) -> PumpEnd {
    use futures::StreamExt;
    let mut stream = std::pin::pin!(resp.bytes_stream());
    let mut buf = String::new();
    let mut raw: Vec<u8> = Vec::new(); // byte dư của ký tự UTF-8 bị cắt giữa 2 chunk (vd '｜' 3 byte)
    loop {
        let chunk = match tokio::time::timeout(std::time::Duration::from_secs(STALL_SECS), stream.next()).await {
            Err(_) => { crate::elog!("[ds-stall] không có dữ liệu {}s (sau {}ms, {} bytes) — bỏ stream", STALL_SECS, t0.elapsed().as_millis(), stats.total_bytes); return PumpEnd::Stall; }
            Ok(None) => return PumpEnd::Eof,
            Ok(Some(Err(e))) => { crate::elog!("[ds-stream-err] {} (sau {}ms, {} bytes)", e, t0.elapsed().as_millis(), stats.total_bytes); return PumpEnd::Err(e.to_string()); }
            Ok(Some(Ok(c))) => c,
        };
        stats.first_byte_ms.get_or_insert_with(|| t0.elapsed().as_millis());
        stats.total_bytes += chunk.len();
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
            if line.starts_with("data: ") {
                stats.sse_chars += line.chars().count() + 1;
                if stats.raw_head.len() < RAW_KEEP { stats.raw_head.push_str(&line); stats.raw_head.push('\n'); }
            }
            // đuôi SSE (25 dòng cuối, cắt gọn) để debug khi stream đứt giữa chừng
            if !line.is_empty() {
                if stats.tail.len() >= 25 { stats.tail.pop_front(); }
                stats.tail.push_back(line.chars().take(300).collect());
            }
            st.handle_line(&line, tx);
        }
    }
}

/// Tin nhắn "nhận trọn" từ server (mã 22 RESUME_MESSAGE_GOT_FULL_MESSAGE): lấy văn bản RESPONSE và THINK.
fn full_message_texts(v: &Value) -> Option<(String, String)> {
    fn find(v: &Value, depth: u8) -> Option<&Vec<Value>> {
        if let Some(a) = v.get("fragments").and_then(|x| x.as_array()) { return Some(a); }
        if depth == 0 { return None; }
        v.as_object()?.values().filter(|x| x.is_object()).find_map(|x| find(x, depth - 1))
    }
    let frags = find(&v["data"]["biz_data"], 3)?;
    let (mut resp, mut think) = (String::new(), String::new());
    for f in frags {
        let c = f["content"].as_str().unwrap_or("");
        match f["type"].as_str() { Some("RESPONSE") => resp.push_str(c), Some("THINK") => think.push_str(c), _ => {} }
    }
    Some((resp, think))
}

/// Một lần nối lại: kết quả cho vòng lặp ngoài.
enum Attempt { Complete, Retry, Stop(String) }

impl DsClient {
    /// Gọi một endpoint nối lại (`/chat/resume_stream` hoặc `/chat/continue`) rồi đọc nốt câu trả lời.
    /// Giống web: không PoW; chờ tối đa 3s để có phản hồi; lỗi JSON (không phải stream) thì dừng, riêng mã 22 là
    /// "server đã có trọn tin nhắn" nên lấy luôn nội dung.
    async fn resume_attempt(&self, path: &str, body: Value, sid: &str, st: &mut SseState, tx: &Tx, stats: &mut PumpStats) -> Attempt {
        let t0 = std::time::Instant::now();
        let before = st.emitted[0] + st.emitted[1];
        let mut meta = json!({});
        let r = self.resume_attempt_inner(path, body, sid, st, tx, stats, &mut meta).await;
        let (outcome, why) = match &r { Attempt::Complete => ("complete", String::new()), Attempt::Retry => ("retry", String::new()), Attempt::Stop(w) => ("stop", w.clone()) };
        crate::logbuf::debug("resume_attempt", json!({
            "path": path, "sid": sid, "outcome": outcome, "why": why, "ms": t0.elapsed().as_millis() as u64,
            "forwarded_chars": st.emitted[0] + st.emitted[1] - before, "received_chars": st.recv[0] + st.recv[1], "meta": meta,
        }));
        r
    }

    async fn resume_attempt_inner(&self, path: &str, body: Value, sid: &str, st: &mut SseState, tx: &Tx, stats: &mut PumpStats, meta: &mut Value) -> Attempt {
        let rb = self.with_hif(self.req(wreq::Method::POST, path)).await
            .header("referer", format!("https://chat.deepseek.com/a/chat/s/{}", sid))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).unwrap_or_default());
        let resp = match tokio::time::timeout(std::time::Duration::from_secs(3), rb.send()).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => { crate::elog!("[ds-resume] {} lỗi mạng: {}", path, e); return Attempt::Retry; }
            Err(_) => { crate::elog!("[ds-resume] {} không phản hồi trong 3s", path); return Attempt::Retry; }
        };
        let status = resp.status();
        meta["status"] = json!(status.as_u16());
        meta["content_type"] = json!(resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or(""));
        if !status.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            meta["body_head"] = json!(txt.chars().take(300).collect::<String>());
            return Attempt::Stop(format!("{} -> {} {}", path, status, txt.chars().take(160).collect::<String>()));
        }
        let is_json = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.starts_with("application/json"));
        if is_json {
            let txt = resp.text().await.unwrap_or_default();
            let v: Value = serde_json::from_str(&txt).unwrap_or(Value::Null);
            let code = v["data"]["biz_code"].as_i64();
            meta["biz_code"] = json!(code);
            meta["biz_msg"] = json!(v["data"]["biz_msg"]);
            if code != Some(22) { meta["body_head"] = json!(txt.chars().take(300).collect::<String>()); }
            if code == Some(22) {
                return match full_message_texts(&v) {
                    Some((r, t)) => {
                        st.sync_to(true, &t, tx);
                        st.sync_to(false, &r, tx);
                        st.saw_close = true;
                        Attempt::Complete
                    }
                    None => Attempt::Stop("server báo đã có trọn tin nhắn nhưng không đọc được nội dung".into()),
                };
            }
            if let Some(e) = classify_global_code(&v) { return Attempt::Stop(format!("{:#}", e)); }
            return Attempt::Stop(format!("{} trả JSON: {}", path, txt.chars().take(160).collect::<String>()));
        }
        st.begin_resume();
        let t0 = std::time::Instant::now();
        let end = pump(resp, st, tx, stats, t0).await;
        meta["stream_end"] = json!(end.why().unwrap_or_else(|| "eof".into()));
        meta["saw_close"] = json!(st.complete());
        if st.complete() { return Attempt::Complete; }
        // có chữ mới nhưng lại đứt: thử tiếp (vẫn trong hạn của vòng ngoài); chưa có gì thì nghỉ rồi hỏi lại
        crate::elog!("[ds-resume] {} dứt giữa chừng ({}), nhận {} ký tự", path, end.why().unwrap_or_else(|| "không có close".into()), st.recv[0] + st.recv[1]);
        Attempt::Retry
    }

    /// Y hệt web khi stream đứt: autoResume (`resume_stream`, tối đa 5s, nghỉ 1s giữa các lần), rồi nút "Tiếp tục"
    /// (`continue` với `fallback_to_resume`) sau một nhịp ngẫu nhiên nhẹ. Thành công → câu trả lời liền mạch, không đổi acc.
    async fn auto_resume(&self, sid: &str, st: &mut SseState, tx: &Tx, stats: &mut PumpStats) -> Result<(), String> {
        let t0 = std::time::Instant::now();
        let r = self.auto_resume_inner(sid, st, tx, stats).await;
        crate::logbuf::debug("auto_resume", json!({
            "sid": sid, "mid": st.resp_mid, "outcome": if r.is_ok() { "ok" } else { "failed" }, "why": r.as_ref().err(),
            "ms": t0.elapsed().as_millis() as u64, "forwarded_chars": {"response": st.emitted[0], "think": st.emitted[1]},
        }));
        r
    }

    async fn auto_resume_inner(&self, sid: &str, st: &mut SseState, tx: &Tx, stats: &mut PumpStats) -> Result<(), String> {
        let Some(mid) = st.resp_mid else { return Err("chưa biết id tin nhắn trả lời".into()) };
        let jitter = |lo: u64, span: u64| lo + (uuid::Uuid::new_v4().as_u128() % span as u128) as u64;
        let t_start = std::time::Instant::now();
        let mut tries = 0u32;
        while t_start.elapsed() < std::time::Duration::from_secs(5) && tries < 6 {
            tries += 1;
            match self.resume_attempt("/chat/resume_stream", json!({"chat_session_id": sid, "message_id": mid}), sid, st, tx, stats).await {
                Attempt::Complete => { crate::elog!("[ds-resume] nối lại được bằng resume_stream (lần {}), thêm {} ký tự", tries, st.recv[0] + st.recv[1]); return Ok(()); }
                Attempt::Stop(why) => return Err(why),
                Attempt::Retry => tokio::time::sleep(std::time::Duration::from_millis(jitter(1000, 300))).await,
            }
        }
        // web hiện nút "Tiếp tục": bấm hộ
        tokio::time::sleep(std::time::Duration::from_millis(jitter(800, 1000))).await;
        match self.resume_attempt("/chat/continue", json!({"chat_session_id": sid, "message_id": mid, "fallback_to_resume": true, "shumei_device_id": self.shumei_id}), sid, st, tx, stats).await {
            Attempt::Complete => { crate::elog!("[ds-resume] nối lại được bằng continue, thêm {} ký tự", st.recv[0] + st.recv[1]); Ok(()) }
            Attempt::Stop(why) => Err(why),
            Attempt::Retry => Err("resume_stream và continue đều không nối lại được".into()),
        }
    }
}


/// Mã lỗi toàn cục của web DS (bảng `GLOBAL_ERROR_BIZ_CODE` trong bundle 2.7.0) → lỗi có tiền tố cho tầng trên.
/// None = không phải lỗi toàn cục (code 0 hoặc lỗi nghiệp vụ nằm trong `data.biz_code`).
pub(crate) fn classify_global_code(v: &Value) -> Option<anyhow::Error> {
    let code = v["code"].as_i64()?;
    let msg = v["msg"].as_str().unwrap_or("").to_string();
    let tag = match code {
        0 => return None,
        // web: onTokenInvalid → đăng xuất. Không có refresh token.
        40002 | 40003 => "AUTH_INVALID",
        // web: onIsBanned
        40012 => "USER_BANNED",
        // web: onIpAccessRestricted ("thao tác quá nhanh") — theo IP, không phải lỗi acc
        40029 => "IP_RESTRICTED",
        40300 | 40301 => "POW_INVALID",
        // web: business challenge = hCaptcha cho người thật giải, rồi gửi lại kèm x-ds-challenge-response
        40303 | 40304 => "CHALLENGE_REQUIRED",
        _ => return Some(anyhow::anyhow!("DS code {}: {}", code, msg)),
    };
    Some(anyhow::anyhow!("{}: {} ({})", tag, if msg.is_empty() { tag } else { &msg }, code))
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

    /// Gọi tls.peet.ws thật (cần mạng): `cargo test profile_is_stable -- --ignored --nocapture`
    #[test]
    #[ignore]
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
        let b = String::from_utf8(CompletionBody::new("s", None, "hi", &[], false, true, Some("BQWJD+x/==")).into_body()).unwrap();
        assert_eq!(b, r#"{"chat_session_id":"s","parent_message_id":null,"model_type":"default","prompt":"hi","ref_file_ids":[],"thinking_enabled":false,"search_enabled":true,"action":null,"preempt":false,"shumei_device_id":"BQWJD+x/=="}"#);
        // chưa có mã: web gửi null chứ không bỏ trường
        let n = String::from_utf8(CompletionBody::new("s", None, "hi", &[], false, true, None).into_body()).unwrap();
        assert!(n.ends_with(r#""preempt":false,"shumei_device_id":null}"#), "{n}");
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

#[cfg(test)]
mod session_cookie_tests {
    use super::*;

    /// Acc đã có ds_session_id (tiện ích, đăng nhập qua Chrome): trả luôn cookie đó, không gọi mạng.
    #[tokio::test]
    async fn existing_session_cookie_is_kept_without_request() {
        let c = DsClient::new("tok-sid-offline-0123456789abcdef".into(), "ds_session_id=abc123; smidV2=x".into()).unwrap();
        let t = std::time::Instant::now();
        assert_eq!(c.ensure_session_cookie().await.unwrap(), "abc123");
        assert!(t.elapsed() < std::time::Duration::from_millis(50), "không được gọi mạng khi đã có cookie");
    }

    /// Lấy ds_session_id thật từ DS như trình duyệt (cần mạng): `cargo test live_session_cookie -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_session_cookie_from_favicon() {
        let c = DsClient::new(format!("tok-sid-live-{}", uuid::Uuid::new_v4().simple()), String::new()).unwrap();
        let sid = c.ensure_session_cookie().await.expect("DS phải đặt ds_session_id");
        println!("ds_session_id = {}…", &sid[..8]);
        assert_eq!(sid.len(), 32);
        assert!(sid.chars().all(|ch| ch.is_ascii_hexdigit()));
        // lần hai: jar đã có, trả cùng giá trị
        assert_eq!(c.ensure_session_cookie().await.unwrap(), sid);
    }

    /// web_boot (users/current + client/settings + telemetry) trên acc đầu trong store:
    /// `cargo test live_web_boot -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_web_boot() {
        let (tok, cookies) = match std::env::var("WG_TEST_TOKEN") {
            Ok(t) => (t, std::env::var("WG_TEST_COOKIES").unwrap_or_default()),
            Err(_) => {
                let raw = std::fs::read_to_string(format!("{}/.whalegate/accounts.json", std::env::var("HOME").unwrap())).expect("store");
                let st: Value = serde_json::from_str(&raw).unwrap();
                let a = st["accounts"].as_array().unwrap().iter().find(|a| a["token"].as_str().is_some()).expect("acc có token");
                let ck = a["cookies"].as_object().map(|o| o.iter().map(|(k, v)| format!("{}={}", k, v.as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ")).unwrap_or_default();
                (a["token"].as_str().unwrap().to_string(), ck)
            }
        };
        let c = DsClient::new(tok, cookies).unwrap();
        c.web_boot().await.expect("web_boot");
        println!("web_boot xong");
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    fn collect(rx: &mut tokio::sync::mpsc::UnboundedReceiver<StreamEv>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            match e {
                StreamEv::Response(t) => out.push(format!("R:{t}")),
                StreamEv::Thinking(t) => out.push(format!("T:{t}")),
                StreamEv::MsgId(m) => out.push(format!("M:{m}")),
                _ => {}
            }
        }
        out
    }

    fn feed_all(st: &mut SseState, tx: &Tx, lines: &[&str]) { for l in lines { st.handle_line(l, tx); } }

    /// Stream thường: không có `event: close` thì chưa coi là xong, và biết id tin nhắn để nối lại.
    #[test]
    fn interrupted_stream_is_detected() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = SseState::default();
        feed_all(&mut st, &tx, &[
            r#"data: {"request_message_id":1,"response_message_id":7}"#,
            r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"Hel"}]}}}"#,
            r#"data: {"p":"response/fragments/-1/content","v":"lo"}"#,
        ]);
        assert!(st.any_delta && !st.complete());
        assert_eq!(st.resp_mid, Some(7));
        assert_eq!(collect(&mut rx), ["M:7", "R:Hel", "R:lo"]);
        feed_all(&mut st, &tx, &["event: close"]);
        assert!(st.complete());
    }

    /// Server gửi lại từ đầu (ảnh chụp tích luỹ + delta): chỉ phần chưa gửi được chuyển tiếp.
    #[test]
    fn resume_forwards_only_the_unsent_tail() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = SseState::default();
        feed_all(&mut st, &tx, &[
            r#"data: {"request_message_id":1,"response_message_id":7}"#,
            r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"Hel"}]}}}"#,
            r#"data: {"v":"lo"}"#,
        ]);
        let _ = collect(&mut rx);
        st.begin_resume();
        feed_all(&mut st, &tx, &[
            "event: ready",
            r#"data: {"request_message_id":1,"response_message_id":7}"#,
            r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"Hello wor"}]}}}"#,
            r#"data: {"v":"ld"}"#,
            "event: close",
        ]);
        assert_eq!(collect(&mut rx), ["R: wor", "R:ld"]);
        assert!(st.complete());
        assert_eq!(st.emitted[0], "Hello world".chars().count());
    }

    /// Ảnh chụp không dài hơn phần đã gửi thì không chuyển tiếp gì (không lặp chữ).
    #[test]
    fn resume_snapshot_not_longer_sends_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = SseState::default();
        feed_all(&mut st, &tx, &[
            r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"Xin chào"}]}}}"#,
        ]);
        let _ = collect(&mut rx);
        st.begin_resume();
        feed_all(&mut st, &tx, &[r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"Xin chào"}]}}}"#]);
        assert!(collect(&mut rx).is_empty());
    }

    /// THINK và RESPONSE đếm riêng; ảnh chụp có cả hai fragment.
    #[test]
    fn resume_counts_think_and_response_separately() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = SseState::default();
        feed_all(&mut st, &tx, &[
            r#"data: {"v":{"response":{"fragments":[{"type":"THINK","content":"hmm"}]}}}"#,
            r#"data: {"p":"response/fragments","o":"APPEND","v":[{"type":"RESPONSE","content":"ab"}]}"#,
        ]);
        let _ = collect(&mut rx);
        st.begin_resume();
        feed_all(&mut st, &tx, &[
            r#"data: {"v":{"response":{"fragments":[{"type":"THINK","content":"hmm ok"},{"type":"RESPONSE","content":"abcd"}]}}}"#,
        ]);
        assert_eq!(collect(&mut rx), ["T: ok", "R:cd"]);
    }

    /// Ký tự đa byte: đếm theo ký tự, không theo byte.
    #[test]
    fn resume_counts_chars_not_bytes() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = SseState::default();
        feed_all(&mut st, &tx, &[r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"你好"}]}}}"#]);
        let _ = collect(&mut rx);
        st.begin_resume();
        feed_all(&mut st, &tx, &[r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"你好世界"}]}}}"#]);
        assert_eq!(collect(&mut rx), ["R:世界"]);
    }

    /// Mã 22: server đã có trọn tin nhắn → lấy văn bản RESPONSE / THINK.
    #[test]
    fn full_message_is_extracted() {
        let v: Value = serde_json::from_str(r#"{"code":0,"data":{"biz_code":22,"biz_msg":"","biz_data":{"message_id":7,"fragments":[{"type":"THINK","content":"a"},{"type":"RESPONSE","content":"xy"},{"type":"RESPONSE","content":"z"}]}}}"#).unwrap();
        assert_eq!(full_message_texts(&v), Some(("xyz".into(), "a".into())));
        let nested: Value = serde_json::from_str(r#"{"data":{"biz_data":{"message":{"fragments":[{"type":"RESPONSE","content":"q"}]}}}}"#).unwrap();
        assert_eq!(full_message_texts(&nested), Some(("q".into(), String::new())));
        assert_eq!(full_message_texts(&serde_json::json!({"data":{"biz_data":{}}})), None);
    }

    /// Các dạng `BATCH` lồng nhau ghi lại từ luồng thật (web 2.7.0, có Search): trước đây chữ trong chúng bị rơi mất.
    #[test]
    fn real_batch_shapes_are_followed() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = SseState::default();
        feed_all(&mut st, &tx, &[
            r#"data: {"v":{"response":{"message_id":2,"fragments":[{"id":1,"type":"THINK","content":"a"}]}}}"#,
            r#"data: {"p":"response/fragments","o":"APPEND","v":[{"id":13,"type":"TOOL_SEARCH","status":"WIP","content":null,"queries":[],"results":[],"stage_id":3}]}"#,
            r#"data: {"p":"response","o":"BATCH","v":[{"p":"fragments","o":"APPEND","v":[{"id":14,"type":"TOOL_OPEN","status":"FINISHED","result":{},"stage_id":3}]}]}"#,
            // event trơn, v là mảng lệnh: kế thừa p=response, o=BATCH của event trước; content ở đây là SET lên TOOL_SEARCH
            r#"data: {"v":[{"p":"fragments/-2","o":"BATCH","v":[{"p":"content","v":"Found 24 web pages"},{"p":"results","v":[]}]}]}"#,
            r#"data: {"v":[{"p":"fragments","o":"APPEND","v":[{"id":16,"type":"THINK","content":"搜索","elapsed_secs":null}]},{"p":"has_pending_fragment","o":"SET","v":false}]}"#,
            // nối chữ vào THINK cuối rồi mở RESPONSE mới ngay trong một BATCH
            r#"data: {"p":"response/fragments","o":"BATCH","v":[{"p":"-1","o":"BATCH","v":[{"p":"content","o":"APPEND","v":"开始"},{"p":"elapsed_secs","o":"SET","v":2.4}]},{"p":"","o":"APPEND","v":[{"id":17,"type":"RESPONSE","content":"**Lịch sử","references":[],"stage_id":4}]}]}"#,
            // trích dẫn: chữ nằm trong BATCH trên fragment cuối
            r#"data: {"p":"response/fragments/-1","o":"BATCH","v":[{"p":"content","o":"APPEND","v":"[reference:0]. T"},{"p":"references","o":"SET","v":[{"id":10}]}]}"#,
            r#"data: {"p":"response/fragments/-1/content","o":"APPEND","v":" tiếp"}"#,
            r#"data: {"v":" nữa"}"#,
            // kết thúc: chữ cuối + token usage cùng một BATCH
            r#"data: {"p":"response","o":"BATCH","v":[{"p":"accumulated_token_usage","v":3548},{"p":"fragments/-1/content","o":"APPEND","v":" đó."},{"p":"quasi_status","o":"SET","v":"FINISHED"},{"p":"status","v":"FINISHED"}]}"#,
            "event: close",
        ]);
        let mut think = String::new(); let mut resp = String::new(); let mut usage = None;
        while let Ok(e) = rx.try_recv() { match e { StreamEv::Thinking(t) => think.push_str(&t), StreamEv::Response(t) => resp.push_str(&t), StreamEv::Usage(u) => usage = Some(u), _ => {} } }
        assert_eq!(think, "a搜索开始");
        assert_eq!(resp, "**Lịch sử[reference:0]. T tiếp nữa đó.");
        assert_eq!(usage, Some(3548));
        assert!(st.complete());
    }

    /// Phát lại một luồng `resume_stream` thật (bắt từ tab web) và so với tin nhắn cuối cùng server trả:
    /// `WG_SSE_FIXTURE=/đường/dẫn/fixture.json cargo test replay_real_resume -- --ignored --nocapture`
    /// fixture = {"raw": "<SSE thô>", "final": {"fragments": [...]}}. Cũng thử cắt giữa chừng rồi nối lại ở nhiều điểm.
    #[test]
    #[ignore]
    fn replay_real_resume() {
        let path = std::env::var("WG_SSE_FIXTURE").expect("đặt WG_SSE_FIXTURE");
        let fx: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let raw = fx["raw"].as_str().unwrap();
        let (mut want_t, mut want_r) = (String::new(), String::new());
        for f in fx["final"]["fragments"].as_array().unwrap() {
            let c = f["content"].as_str().unwrap_or("");
            match f["type"].as_str() { Some("THINK") => want_t.push_str(c), Some("RESPONSE") => want_r.push_str(c), _ => {} }
        }
        let lines: Vec<&str> = raw.lines().collect();
        println!("{} dòng, THINK {} ký tự, RESPONSE {} ký tự", lines.len(), want_t.chars().count(), want_r.chars().count());
        let run = |cut: Option<usize>| -> (String, String, bool) {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let mut st = SseState::default();
            st.begin_resume(); // luồng đầu: coi như một luồng nối bình thường từ đầu
            if let Some(k) = cut { for l in lines.iter().take(k) { st.handle_line(l, &tx); } st.begin_resume(); } // đứt ở dòng k rồi nối lại từ đầu
            for l in &lines { st.handle_line(l, &tx); }
            let (mut t, mut r) = (String::new(), String::new());
            while let Ok(e) = rx.try_recv() { match e { StreamEv::Thinking(x) => t.push_str(&x), StreamEv::Response(x) => r.push_str(&x), _ => {} } }
            (t, r, st.complete())
        };
        let (t, r, done) = run(None);
        assert!(done, "thiếu event close");
        assert_eq!(t, want_t, "THINK không khớp");
        assert_eq!(r, want_r, "RESPONSE không khớp");
        for k in [5usize, 20, 40, 75, 120, 200, 300, 380] {
            let (t, r, done) = run(Some(k));
            assert!(done);
            assert_eq!(t, want_t, "THINK lệch khi cắt ở dòng {k}");
            assert_eq!(r, want_r, "RESPONSE lệch khi cắt ở dòng {k}");
        }
    }

    /// sync_to chỉ gửi phần thiếu.
    #[test]
    fn sync_to_sends_missing_part() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = SseState::default();
        st.emit(false, "abc", &tx);
        let _ = collect(&mut rx);
        st.sync_to(false, "abcdef", &tx);
        st.sync_to(false, "abcdef", &tx);
        assert_eq!(collect(&mut rx), ["R:def"]);
    }
}

#[cfg(test)]
mod resume_http_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Phản hồi giả cho từng đường dẫn, lấy lần lượt (cái cuối lặp lại).
    #[derive(Clone)]
    enum Reply { Sse(&'static str), Json(&'static str), Status(u16) }

    #[derive(Default)]
    struct Plan { by_path: Mutex<std::collections::HashMap<String, Vec<Reply>>>, hits: Mutex<Vec<String>> }

    async fn serve(plan: Arc<Plan>) -> String {
        use axum::{extract::State, http::{StatusCode, Uri}, response::{IntoResponse, Response}, routing::any, Router};
        async fn h(State(p): State<Arc<Plan>>, uri: Uri, body: String) -> Response {
            let path = uri.path().to_string();
            p.hits.lock().unwrap().push(format!("{} {}", path, body));
            let r = {
                let mut m = p.by_path.lock().unwrap();
                let v = m.get_mut(&path).expect("đường dẫn không có trong kịch bản");
                if v.len() > 1 { v.remove(0) } else { v[0].clone() }
            };
            match r {
                Reply::Sse(t) => ([("content-type", "text/event-stream")], t).into_response(),
                Reply::Json(t) => ([("content-type", "application/json")], t).into_response(),
                Reply::Status(c) => (StatusCode::from_u16(c).unwrap(), "boom").into_response(),
            }
        }
        let app = Router::new().fallback(any(h)).with_state(plan);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap(); });
        format!("http://{}", addr)
    }

    fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<StreamEv>) -> String {
        let mut s = String::new();
        while let Ok(e) = rx.try_recv() { if let StreamEv::Response(t) = e { s.push_str(&t); } }
        s
    }

    /// State như lúc stream đứt giữa chừng: đã chuyển tiếp "Hello", biết id tin nhắn trả lời.
    fn cut_state(tx: &Tx) -> SseState {
        let mut st = SseState::default();
        st.handle_line(r#"data: {"request_message_id":1,"response_message_id":7}"#, tx);
        st.handle_line(r#"data: {"v":{"response":{"fragments":[{"type":"RESPONSE","content":"Hello"}]}}}"#, tx);
        st
    }

    // TEST_BASE là biến toàn cục: các test dùng nó chạy lần lượt trong MỘT test để không giẫm lên nhau
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auto_resume_against_fake_server() {
        let client = DsClient::new("tok-offline-0123456789abcdef".into(), "ds_session_id=x".into()).unwrap();
        let sid = "sid-1";

        // 1) resume_stream trả SSE tích luỹ + close → chỉ phần thiếu được chuyển tiếp
        let plan = Arc::new(Plan::default());
        plan.by_path.lock().unwrap().insert("/chat/resume_stream".into(), vec![Reply::Sse(concat!(
            "event: ready\ndata: {\"request_message_id\":1,\"response_message_id\":7}\n\n",
            "data: {\"v\":{\"response\":{\"fragments\":[{\"type\":\"RESPONSE\",\"content\":\"Hello wor\"}]}}}\n\n",
            "data: {\"v\":\"ld\"}\n\n",
            "event: close\ndata: {}\n\n"))]);
        *TEST_BASE.lock().unwrap() = Some(serve(plan.clone()).await);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = cut_state(&tx);
        let _ = drain(&mut rx);
        let mut stats = PumpStats::default();
        let t = std::time::Instant::now();
        assert!(client.auto_resume(sid, &mut st, &tx, &mut stats).await.is_ok());
        assert_eq!(drain(&mut rx), " world");
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "lần đầu thành công thì không được chờ");
        let hits = plan.hits.lock().unwrap().clone();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].starts_with("/chat/resume_stream ") && hits[0].contains("\"message_id\":7") && hits[0].contains(sid), "{:?}", hits);

        // 2) server báo đã có trọn tin nhắn (mã 22) → lấy nội dung, không cần stream
        let plan = Arc::new(Plan::default());
        plan.by_path.lock().unwrap().insert("/chat/resume_stream".into(), vec![Reply::Json(
            r#"{"code":0,"data":{"biz_code":22,"biz_msg":"","biz_data":{"fragments":[{"type":"RESPONSE","content":"Hello world!"}]}}}"#)]);
        *TEST_BASE.lock().unwrap() = Some(serve(plan.clone()).await);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = cut_state(&tx);
        let _ = drain(&mut rx);
        assert!(client.auto_resume(sid, &mut st, &tx, &mut PumpStats::default()).await.is_ok());
        assert_eq!(drain(&mut rx), " world!");

        // 3) lỗi HTTP (hoặc JSON lạ) → dừng ngay, không đập liên tục
        let plan = Arc::new(Plan::default());
        plan.by_path.lock().unwrap().insert("/chat/resume_stream".into(), vec![Reply::Status(500)]);
        *TEST_BASE.lock().unwrap() = Some(serve(plan.clone()).await);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = cut_state(&tx);
        let _ = drain(&mut rx);
        let t = std::time::Instant::now();
        let r = client.auto_resume(sid, &mut st, &tx, &mut PumpStats::default()).await;
        assert!(r.is_err(), "{:?}", r);
        assert_eq!(plan.hits.lock().unwrap().len(), 1, "lỗi HTTP không được thử lại");
        assert!(t.elapsed() < std::time::Duration::from_secs(2));

        // 4) resume_stream cứ đứt không có close → hết ~5s thì bấm "continue", continue trả đủ
        let plan = Arc::new(Plan::default());
        plan.by_path.lock().unwrap().insert("/chat/resume_stream".into(), vec![Reply::Sse(
            "data: {\"v\":{\"response\":{\"fragments\":[{\"type\":\"RESPONSE\",\"content\":\"Hello w\"}]}}}\n\n")]);
        plan.by_path.lock().unwrap().insert("/chat/continue".into(), vec![Reply::Sse(concat!(
            "data: {\"v\":{\"response\":{\"fragments\":[{\"type\":\"RESPONSE\",\"content\":\"Hello world\"}]}}}\n\n",
            "event: close\n\n"))]);
        *TEST_BASE.lock().unwrap() = Some(serve(plan.clone()).await);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut st = cut_state(&tx);
        let _ = drain(&mut rx);
        let t = std::time::Instant::now();
        assert!(client.auto_resume(sid, &mut st, &tx, &mut PumpStats::default()).await.is_ok());
        let el = t.elapsed();
        assert!(el >= std::time::Duration::from_secs(4) && el < std::time::Duration::from_secs(9), "web: ~5s resume rồi continue, thực tế {:?}", el);
        // phần đã nhận ở các lần resume (" w") + phần còn lại của continue, không lặp
        assert_eq!(drain(&mut rx), " world");
        let hits = plan.hits.lock().unwrap().clone();
        assert!(hits.iter().any(|h| h.starts_with("/chat/continue ") && h.contains("\"fallback_to_resume\":true") && h.contains("\"message_id\":7")), "{:?}", hits);
        assert!(hits.iter().filter(|h| h.starts_with("/chat/resume_stream ")).count() >= 2);

        *TEST_BASE.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod pow_tool {
    use super::*;

    /// Công cụ thử nghiệm: giải một loạt thử thách PoW lấy từ trình duyệt thật rồi in header `x-ds-pow-response`.
    /// `WG_POW_IN=challenges.json WG_POW_OUT=headers.json cargo test pow_headers_from_file -- --ignored`
    /// challenges.json = [{"algorithm","challenge","salt","difficulty","expire_at","signature","target_path"}, ...]
    #[test]
    #[ignore]
    fn pow_headers_from_file() {
        let inp = std::env::var("WG_POW_IN").expect("WG_POW_IN");
        let out = std::env::var("WG_POW_OUT").expect("WG_POW_OUT");
        let list: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(inp).unwrap()).unwrap();
        let headers: Vec<String> = list.iter().map(|ch| {
            let challenge_hex = ch["challenge"].as_str().unwrap().to_string();
            let salt = ch["salt"].as_str().unwrap().to_string();
            let answer = solve_pow(&challenge_hex, &salt, ch["expire_at"].as_u64().unwrap(), ch["difficulty"].as_u64().unwrap()).expect("không giải được");
            let payload = json!({"algorithm": ch["algorithm"], "challenge": challenge_hex, "salt": salt, "answer": answer, "signature": ch["signature"], "target_path": ch["target_path"]});
            base64::engine::general_purpose::STANDARD.encode(payload.to_string())
        }).collect();
        std::fs::write(out, serde_json::to_string(&headers).unwrap()).unwrap();
    }
}

#[cfg(test)]
mod shumei_id_tests {
    use super::*;

    #[test]
    fn id_is_b_plus_decoded_thumbcache_cookie() {
        let ck = "aws-waf-token=w:1; smidV2=2026110abc; .thumbcache_6b2e5483f9d858d7c661c5e276b6a6ae=QWJD%2Bx%2F%3D%3D; ds_session_id=s";
        assert_eq!(shumei_id_from_cookies(ck).as_deref(), Some("BQWJD+x/=="));
        // không có cookie → None (completion sẽ gửi null)
        assert_eq!(shumei_id_from_cookies("a=b; smidV2=1"), None);
        assert_eq!(shumei_id_from_cookies(".thumbcache_x="), None);
        // %-sequence hỏng giữ nguyên, không panic
        assert_eq!(pct_decode("a%2"), "a%2");
        assert_eq!(pct_decode("%zz%41"), "%zzA");
    }

    #[test]
    fn client_takes_id_from_cookie_string() {
        let c = DsClient::new("tok-shumei-offline-0123456789".into(), "x=1; .thumbcache_ab=Zm9v%3D".into()).unwrap();
        assert_eq!(c.shumei_id.as_deref(), Some("BZm9v="));
    }
}
