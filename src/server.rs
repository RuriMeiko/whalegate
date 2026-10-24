// Axum server: UI quản lý account + OpenAI-compatible API


use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::client::DsClient;

/// File acc (DS_ACCOUNTS_PATH ghi đè, tiện chạy bản test với bộ acc riêng).
pub(crate) fn creds_dir() -> String {
    std::path::Path::new(&creds_path()).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into())
}

fn creds_path() -> String {
    std::env::var("DS_ACCOUNTS_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{}/.ds-harness/accounts.json", home)
    })
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Account {
    pub id: String,
    pub label: String,
    pub token: String,
    pub cookies: std::collections::BTreeMap<String, String>,
    pub added_at: u64,
    pub last_verified: Option<u64>,
    pub last_status: Option<String>,
    // Thread stateful: chat tiếp trong cùng conversation (không tạo mới mỗi request)
    pub thread_session_id: Option<String>,
    pub thread_parent_message_id: Option<i64>,
    // số lượt đã chạy trên thread_session_id hiện tại — quá DS_SESSION_ROTATE thì đổi session mới
    #[serde(default)]
    pub thread_uses: u32,
    // ---- Smart rotation metrics ----
    #[serde(default)]
    pub stats: AccStats,
    // Tách acc khỏi pool (acc chính dùng tay): true = tool được phép dùng
    #[serde(default = "default_true")]
    pub pool_enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AccStats {
    // Burst window: đếm call gần nhất; reset khi quá cooldown
    #[serde(default)]
    pub burst_count: u32,
    #[serde(default)]
    pub burst_window_start: u64, // unix secs
    // Acc bị throttle đến thời điểm này (unix secs) — không chọn trước đó
    #[serde(default)]
    pub cooldown_until: u64,
    // Lifetime counters
    #[serde(default)]
    pub total_ok: u64,
    #[serde(default)]
    pub total_empty: u64,
    #[serde(default)]
    pub total_error: u64,
    #[serde(default)]
    pub last_used_at: u64,
    // số lần hỏng/rỗng liên tiếp — cooldown tăng dần để acc xấu tự rời pool rồi tự quay lại
    #[serde(default)]
    pub fail_streak: u32,
}

impl AccStats {
    // Tunable qua env: DS_BURST_LIMIT, DS_BURST_WINDOW, DS_COOLDOWN
    pub fn burst_limit() -> u32 {
        static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        *V.get_or_init(|| {
            std::env::var("DS_BURST_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(6)
        })
    }
    pub fn burst_window() -> u64 {
        static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *V.get_or_init(|| {
            std::env::var("DS_BURST_WINDOW").ok().and_then(|v| v.parse().ok()).unwrap_or(30)
        })
    }
    pub fn cooldown() -> u64 {
        static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *V.get_or_init(|| {
            std::env::var("DS_COOLDOWN").ok().and_then(|v| v.parse().ok()).unwrap_or(10)
        })
    }
    // CAP concurrent call trên 1 acc (env DS_MAX_CONC_PER_ACC, default 2).
    // Cộng đồng report ban khi >2 call đồng thời/acc — 2 là ngưỡng an toàn.
    pub fn max_conc() -> u32 {
        static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        *V.get_or_init(|| {
            std::env::var("DS_MAX_CONC_PER_ACC").ok().and_then(|v| v.parse().ok()).unwrap_or(2)
        })
    }

    fn burst_active(&self, now: u64) -> bool {
        now.saturating_sub(self.burst_window_start) < Self::burst_window()
            && self.burst_count >= Self::burst_limit()
    }

    pub(crate) fn available(&self, now: u64) -> bool {
        now >= self.cooldown_until && !self.burst_active(now)
    }

    /// Xoá sạch trạng thái lỗi/cooldown/burst (nút Reset trên UI, hoặc sau khi đăng nhập lại).
    pub(crate) fn reset(&mut self) {
        self.fail_streak = 0;
        self.cooldown_until = 0;
        self.burst_count = 0;
        self.burst_window_start = 0;
    }

    pub(crate) fn mark_ok(&mut self) {
        self.fail_streak = 0;
        self.cooldown_until = 0;
    }

    /// Hỏng/rỗng: cooldown 15s, 30s, 60s … tối đa 10 phút (DS_FAIL_COOLDOWN_MAX).
    pub(crate) fn mark_fail(&mut self, now: u64) {
        self.fail_streak = self.fail_streak.saturating_add(1);
        let max: u64 = std::env::var("DS_FAIL_COOLDOWN_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(600);
        let cd = (15u64 << (self.fail_streak - 1).min(10)).min(max);
        self.cooldown_until = now + cd;
    }

    pub(crate) fn record_call(&mut self, now: u64) {
        if now.saturating_sub(self.burst_window_start) >= Self::burst_window() {
            self.burst_window_start = now;
            self.burst_count = 0;
        }
        self.burst_count += 1;
        self.last_used_at = now;
        if self.burst_count >= Self::burst_limit() {
            self.cooldown_until = now + Self::cooldown();
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Store {
    pub accounts: Vec<Account>,
}

pub struct Creds {
    pub token: String,
    pub cookies: std::collections::BTreeMap<String, String>,
}

impl Creds {
    pub fn cookie_string(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

pub fn load_creds() -> Result<Creds> {
    let store: Store = serde_json::from_str(&std::fs::read_to_string(creds_path())?)?;
    let acc = store
        .accounts
        .first()
        .ok_or_else(|| anyhow::anyhow!("no accounts in {}", creds_path()))?;
    Ok(Creds {
        token: acc.token.clone(),
        cookies: acc.cookies.clone(),
    })
}

/// Đọc store. File hỏng KHÔNG được nuốt im lặng: đổi tên thành `.corrupt-<ts>` để giữ lại
/// (có thể cứu token bằng tay) rồi mới trả store rỗng — nếu không, lần ghi kế tiếp sẽ
/// ghi đè vĩnh viễn file hỏng bằng `{"accounts":[]}`.
pub(crate) async fn load_store() -> Store {
    let path = creds_path();
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Store::default(),
        Err(e) => {
            eprintln!("[store] không đọc được {}: {} — dùng store rỗng", path, e);
            return Store::default();
        }
    };
    match serde_json::from_str::<Store>(&raw) {
        Ok(s) => s,
        Err(e) => {
            let bak = format!("{}.corrupt-{}", path, now());
            eprintln!("[store] {} hỏng ({}): giữ bản sao tại {} — dùng store rỗng", path, e, bak);
            if let Err(e2) = tokio::fs::rename(&path, &bak).await {
                eprintln!("[store] không đổi tên được file hỏng: {}", e2);
            }
            Store::default()
        }
    }
}

/// Ghi store xuống đĩa NGUYÊN TỬ: ghi file tạm cùng thư mục rồi `rename` (rename trên cùng
/// filesystem là atomic) → crash/đầy đĩa giữa chừng không bao giờ để lại accounts.json cụt.
async fn write_store_atomic(store: &Store) -> Result<()> {
    let path = std::path::PathBuf::from(creds_path());
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    tokio::fs::create_dir_all(&dir).await?;
    let json = serde_json::to_vec_pretty(store)?;
    let tmp = dir.join(format!(".accounts.json.tmp-{}", std::process::id()));
    {
        let mut f = tokio::fs::File::create(&tmp).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // đặt quyền trên file tạm TRƯỚC khi ghi dữ liệu → không có cửa sổ nào file chứa token ở 0644
            f.set_permissions(std::fs::Permissions::from_mode(0o600)).await?;
        }
        use tokio::io::AsyncWriteExt;
        f.write_all(&json).await?;
        f.sync_all().await?;
    }
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}

/// Ghi ngay (dùng cho thao tác quản trị: thêm/xoá acc, login, capture). Chỉ ghi nếu store
/// đã đổi so với bản ghi gần nhất, và không giữ lock của store trong lúc chờ I/O.
pub(crate) async fn flush_store(st: &Arc<AppState>) {
    let _serialize = st.save_lock.lock().await;
    let ver = st.store_ver.load(std::sync::atomic::Ordering::Acquire);
    if ver <= st.store_written.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let snap = st.store.read().await.clone();
    match write_store_atomic(&snap).await {
        Ok(()) => st.store_written.store(ver, std::sync::atomic::Ordering::Release),
        Err(e) => eprintln!("[store] ghi accounts.json lỗi: {:#}", e),
    }
}

/// Đánh dấu store đã đổi. Đường nóng (mỗi lượt chat) chỉ gọi hàm này: flusher nền gom
/// nhiều thay đổi trong ~1s rồi ghi một lần, thay vì 2-3 lần ghi file mỗi request.
pub(crate) fn mark_dirty(st: &Arc<AppState>) {
    st.store_ver.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    st.store_dirty.notify_one();
}

async fn store_flusher(st: Arc<AppState>) {
    loop {
        st.store_dirty.notified().await;
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        flush_store(&st).await;
    }
}

/// Một hội thoại đang nằm trong phiên web: gửi tiếp chỉ cần phần mới + parent_message_id.
#[derive(Clone)]
pub(crate) struct Conv {
    pub(crate) acc_id: String,
    pub(crate) sid: String,
    pub(crate) parent_id: i64,
    pub(crate) tools_hash: u64,
    pub(crate) chars: usize,
    pub(crate) tokens: u64,
}

pub(crate) struct AppState {
    pub(crate) store: RwLock<Store>,
    // Ghi store: serialize các lần ghi, đếm version để bỏ qua lần ghi không có thay đổi
    pub(crate) save_lock: tokio::sync::Mutex<()>,
    pub(crate) store_ver: std::sync::atomic::AtomicU64,
    pub(crate) store_written: std::sync::atomic::AtomicU64,
    pub(crate) store_dirty: tokio::sync::Notify,
    // acc_id -> đang xử lý 1 call (chống double-use burst khi concurrent)
    pub(crate) in_flight: std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::atomic::AtomicU32>>>,
    // đang canh 1 tab login (chặn double-start)
    pub(crate) login_watching: std::sync::Mutex<bool>,
    // (acc_id, sha256 ảnh) -> file id DS đã upload (id gắn theo acc nên cache theo acc)
    // hội thoại stateful: dấu vân tay prefix -> phiên web đang giữ ngữ cảnh
    pub(crate) convs: std::sync::Mutex<std::collections::HashMap<u64, Conv>>,
    // MỌI phiên web do harness tạo cho hội thoại: sid -> (acc, lần dùng cuối). Quét xoá theo TTL kể cả khi hội thoại bị bỏ dở.
    pub(crate) sessions: std::sync::Mutex<std::collections::HashMap<String, (String, u64)>>,
    pub(crate) stat_conv_hit: std::sync::atomic::AtomicU64,
    pub(crate) stat_conv_miss: std::sync::atomic::AtomicU64,
    pub(crate) file_cache: std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
}

// ---------- handlers ----------

async fn index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

#[derive(Deserialize)]
struct AddAccount {
    label: String,
    token: String,
    cookies: std::collections::BTreeMap<String, String>,
}

async fn api_add_account(
    State(st): State<Arc<AppState>>,
    Json(p): Json<AddAccount>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // verify token trước khi lưu
    let c = DsClient::new(p.token.clone(), cookie_string(&p.cookies))
        .map_err(into_err)?;
    let v = c.users_current().await.map_err(into_err)?;
    let ok = v["code"].as_i64() == Some(0);
    if !ok {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("token invalid: {}", v["msg"])})),
        ));
    }
    let email = v["data"]["biz_data"]["email"].as_str().unwrap_or("?").to_string();

    let mut store = st.store.write().await;
    // dedupe theo token
    if store.accounts.iter().any(|a| a.token == p.token) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error": "token already added"})),
        ));
    }
    let acc = Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: if p.label.is_empty() { email } else { p.label },
        token: p.token,
        cookies: p.cookies,
        added_at: now(),
        last_verified: Some(now()),
        last_status: Some("ok".into()),
        thread_session_id: None,
        thread_uses: 0,
        thread_parent_message_id: None,
        stats: Default::default(),
        pool_enabled: true,
    };
    let id = acc.id.clone();
    store.accounts.push(acc);
    drop(store);
    mark_dirty(&st);
    flush_store(&st).await;
    Ok(Json(json!({"ok": true, "id": id})))
}

async fn api_list_accounts(State(st): State<Arc<AppState>>) -> Json<Value> {
    let store = st.store.read().await;
    let list: Vec<Value> = store
        .accounts
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "label": a.label,
                "added_at": a.added_at,
                "last_verified": a.last_verified,
                "last_status": a.last_status,
                "token_prefix": a.token.get(..8).unwrap_or("?"),
                "pool_enabled": a.pool_enabled,
            })
        })
        .collect();
    Json(json!({"accounts": list}))
}

// Bật/tắt acc trong pool: false = acc chính dùng tay, tool KHÔNG đụng
async fn api_toggle_pool(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let enabled = body
        .get("pool_enabled")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| {
            into_err(anyhow::anyhow!("thiếu field pool_enabled (bool)"))
        })?;
    let mut store = st.store.write().await;
    let acc = store
        .accounts
        .iter_mut()
        .find(|a| a.id == id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "không thấy account"})),
            )
        })?;
    acc.pool_enabled = enabled;
    drop(store);
    mark_dirty(&st);
    flush_store(&st).await;
    Ok(Json(json!({"ok": true, "id": id, "pool_enabled": enabled})))
}

async fn api_delete_account(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let removed = {
        let mut store = st.store.write().await;
        let before = store.accounts.len();
        store.accounts.retain(|a| a.id != id);
        before != store.accounts.len()
    };
    if removed {
        // acc đã xoá: dọn luôn slot in-flight + ảnh cache để không rò rỉ theo id chết
        st.in_flight.lk().remove(&id);
        st.file_cache.lk().retain(|(acc_id, _), _| acc_id != &id);
        mark_dirty(&st);
        flush_store(&st).await;
    }
    Json(json!({"ok": removed}))
}

// verify lại 1 account (health check + refresh trạng thái)
/// Reset acc: xoá lỗi/cooldown/burst + session web đang giữ (xoá luôn trên DS), rồi kiểm tra lại token.
async fn api_reset_account(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let acc = {
        let mut store = st.store.write().await;
        let Some(a) = store.accounts.iter_mut().find(|a| a.id == id) else {
            return Json(json!({"ok": false, "error": "not found"}));
        };
        a.stats.reset();
        let old = a.thread_session_id.take();
        a.thread_uses = 0;
        let snap = (a.token.clone(), cookie_string(&a.cookies));
        drop(store);
        mark_dirty(&st);
        flush_store(&st).await;
        (snap, old)
    };
    let ((token, cookies), old_sid) = acc;
    st.file_cache.lk().retain(|(acc_id, _), _| acc_id != &id);
    let alive = match DsClient::new(token, cookies) {
        Ok(c) => {
            if let Some(sid) = old_sid { let _ = c.delete_session(&sid).await; }
            c.token_alive().await.unwrap_or(false)
        }
        Err(_) => false,
    };
    {
        let mut store = st.store.write().await;
        if let Some(a) = store.accounts.iter_mut().find(|a| a.id == id) {
            a.last_verified = Some(now());
            a.last_status = Some(if alive { "ok (reset)".into() } else { "invalid token — cần login lại".into() });
        }
    }
    mark_dirty(&st);
    flush_store(&st).await;
    Json(json!({"ok": true, "token_alive": alive}))
}

async fn api_verify_account(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let acc = {
        let store = st.store.read().await;
        store.accounts.iter().find(|a| a.id == id).cloned()
    };
    let Some(acc) = acc else {
        return Json(json!({"ok": false, "error": "not found"}));
    };
    let c = match DsClient::new(acc.token.clone(), cookie_string(&acc.cookies)) {
        Ok(c) => c,
        Err(e) => return Json(json!({"ok": false, "error": e.to_string()})),
    };
    let v = c.users_current().await.unwrap_or_else(|_| json!({"code": -1}));
    let ok = v["code"].as_i64() == Some(0);
    {
        let mut store = st.store.write().await;
        if let Some(a) = store.accounts.iter_mut().find(|a| a.id == id) {
            a.last_verified = Some(now());
            a.last_status = Some(if ok { "ok".into() } else { format!("invalid: {}", v["msg"]) });
        }
    }
    mark_dirty(&st);
    flush_store(&st).await;
    Json(json!({"ok": ok, "msg": v["msg"]}))
}


// ---------- DSML tool-call parse (DS web trả tool call dạng text DSML) ----------
// Format: <｜｜DSML｜｜ calls> <｜｜DSML｜｜ invoke name="bash"> <｜｜DSML｜｜ parameter name="command" string="true">...value...</｜｜DSML｜｜ parameter> </｜｜DSML｜｜ invoke> </｜｜DSML｜｜ calls>
pub(crate) struct DsmlToolCall {
    pub(crate) name: String,
    pub(crate) arguments: String, // JSON object string
}


/// Parse XML-tag tool call: <toolname><param>value</param><param2>...</param2></toolname>
/// (format dsh agent dùng khi model không trả DSML chuẩn)
fn parse_xml_tool_call(text: &str) -> Option<(String, DsmlToolCall)> {
    // tìm tag mở đầu tiên dạng <name> nơi name là tên tool (không chứa space, không phải tag thông thường)
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_alphabetic() {
            // đọc tên tag
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            let tag = &text[start..j];
            // phải có </tag> và content chứa tag con (parameter) — tag HTML thật (p, div) bị loại bằng
            // điều kiện: có </tag> xuất hiện SAU, và giữa có ít nhất 1 tag con <x>...</x>
            if !tag.is_empty() {
                let close = format!("</{}>", tag);
                if let Some(close_pos) = text[j..].find(&close) {
                    let inner = &text[j..j + close_pos];
                    // tag con: <param>value</param>
                    let mut args = serde_json::Map::new();
                    let mut cur = inner;
                    let mut found_param = false;
                    while let Some(pp) = cur.find('<') {
                        let after = &cur[pp + 1..];
                        let Some(gt) = after.find('>') else { break };
                        let child = &after[..gt];
                        // child phải là tên param hợp lệ (không phải /, không có space)
                        if child.starts_with('/') || child.contains(' ') || child.is_empty() {
                            cur = &after[gt + 1..];
                            continue;
                        }
                        let cclose = format!("</{}>", child);
                        let Some(cend) = after[gt + 1..].find(&cclose) else { break };
                        let value = &after[gt + 1..gt + 1 + cend];
                        args.insert(child.to_string(), serde_json::Value::String(value.to_string()));
                        found_param = true;
                        cur = &after[gt + 1 + cend + cclose.len()..];
                    }
                    if found_param {
                        let before = text[..i].trim().to_string();
                        return Some((before, DsmlToolCall {
                            name: tag.to_string(),
                            arguments: serde_json::Value::Object(args).to_string(),
                                                    }));
                    }
                }
            }
        }
        i += 1;
    }
    None
}


/// CHUẨN HÓA TOÀN BỘ DSML về 1 format: '<｜｜DSML｜｜ X>' — model trộn prefix trong cùng 1 block
/// (<dsml invoke ...> nhưng close-tag '</｜｜DSML｜｜ parameter>') — normalize mọi prefix về một.
fn normalize_dsml(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' && i + 5 <= b.len() {
            // '</dsml' hoặc '<dsml' → '<｜｜DSML｜｜' / '</｜｜DSML｜｜'
            let is_close = b[i + 1] == b'/';
            let tag_start = if is_close { i + 2 } else { i + 1 };
            if text[tag_start..].starts_with("dsml") {
                if is_close {
                    out.push_str("</｜｜DSML｜｜");
                    i += 6; // '</dsml'
                } else {
                    out.push_str("<｜｜DSML｜｜");
                    i += 5; // '<dsml'
                }
                continue;
            }
        }
        // copy char
        let ch_len = text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    // biến thể không-space + colon: '<｜｜DSML｜｜:tool_calls>' / '<｜｜DSML｜｜tool_calls>' → chuẩn
    for tag in ["tool_calls", "tool_call", "invoke", "parameter", "bash", "calls", "function_call", "function_calls"] {
        for pre in ["", ":"] {
            let from = format!("<｜｜DSML｜｜{}{}>", pre, tag);
            let to = format!("<｜｜DSML｜｜ {}>", tag);
            while out.contains(&from) {
                out = out.replace(&from, &to);
            }
            let fromc = format!("</｜｜DSML｜｜{}{}>", pre, tag);
            let toc = format!("</｜｜DSML｜｜ {}>", tag);
            while out.contains(&fromc) {
                out = out.replace(&fromc, &toc);
            }
        }
        let from = format!("<｜｜DSML｜｜{}>", tag);
        let to = format!("<｜｜DSML｜｜ {}>", tag);
        while out.contains(&from) {
            out = out.replace(&from, &to);
        }
        // with space already — nothing
    }
    // close-tag variants: '</｜｜DSML｜｜tool_calls>' → '</｜｜DSML｜｜ tool_calls>'
    for tag in ["tool_calls", "tool_call", "invoke", "parameter", "bash", "calls", "function_call", "function_calls", "name", "arguments"] {
        let from = format!("</｜｜DSML｜｜{}>", tag);
        let to = format!("</｜｜DSML｜｜ {}>", tag);
        while out.contains(&from) {
            out = out.replace(&from, &to);
        }
        // ' tool_calls>' variants → ' calls>'
        let from2 = format!("<｜｜DSML｜｜ {}>", tag);
        if !matches!(tag, "tool_calls" | "function_calls" | "tool_call" | "function_call") {
            continue
        }
        let to2 = "<｜｜DSML｜｜ calls>".to_string();
        while out.contains(&from2) {
            out = out.replace(&from2, &to2);
        }
        let from3 = format!("</｜｜DSML｜｜ {}>", tag);
        let to3 = if tag == "tool_calls" || tag == "function_calls" || tag == "tool_call" || tag == "function_call" {
            "</｜｜DSML｜｜ calls>".to_string()
        } else {
            continue
        };
        while out.contains(&from3) {
            out = out.replace(&from3, &to3);
        }
    }
    out
}
/// Số byte đầu của buffer stream an toàn để flush ra client.
/// - đã thấy marker tool-call (DSML 1/2 gạch, <invoke, <bash...) → flush tới '<' mở marker, giữ phần sau
/// - không marker nhưng đuôi có '<' chưa có '>' (tag stream dở, hoặc "｜" rời) → giữ từ '<' đó
/// - còn lại → flush hết
const TOOL_MARKERS: [&str; 6] = ["<dsml", "<invoke", "<parameter", "<bash", "<tool", "<function"];

/// Vị trí đầu tiên của marker tool-call. "DSML" trần chỉ tính khi đứng ngay sau '<' (kèm tối đa vài thanh '｜'/'|'),
/// để văn xuôi nhắc tới "DSML" (vd bản tóm tắt compaction) không bị coi là tool-call hỏng.
fn find_tool_marker(buf: &str) -> Option<usize> {
    let plain = TOOL_MARKERS.iter().filter_map(|k| buf.find(k)).min();
    let mut dsml = None;
    for (i, _) in buf.match_indices("DSML") {
        let head = buf[..i].trim_end_matches(|c| c == '｜' || c == '|');
        if head.ends_with('<') { dsml = Some(head.len() - 1); break; }
    }
    match (plain, dsml) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) }
}

pub(crate) fn has_tool_marker(buf: &str) -> bool {
    find_tool_marker(buf).is_some()
}

pub(crate) fn safe_flush_len(buf: &str) -> usize {
    if let Some(m) = find_tool_marker(buf) {
        return buf[..m].rfind('<').unwrap_or(m);
    }
    match buf.rfind('<') {
        Some(i) if !buf[i..].contains('>') => i,
        _ => buf.len(),
    }
}

/// Số lượt tối đa trên 1 session web trước khi đổi (DS_SESSION_ROTATE, mặc định 30).
/// Prompt luôn gửi đủ lịch sử, session web không giữ ngữ cảnh → để nó phình vô hạn chỉ tổ chậm/nặng.
pub(crate) fn session_rotate_limit() -> u32 {
    std::env::var("DS_SESSION_ROTATE").ok().and_then(|v| v.parse().ok()).unwrap_or(30)
}

/// Ghi nhận 1 lượt dùng session `sid` cho acc. Session khác session đang lưu (do xoay vòng) → thay mới,
/// xoá session cũ sau 10 phút (request đang chạy dở trên session cũ còn kịp xong).
pub(crate) fn note_session_use(a: &mut Account, sid: &str) {
    match a.thread_session_id.as_deref() {
        Some(cur) if cur == sid => a.thread_uses += 1,
        old => {
            if let Some(old) = old.map(|o| o.to_string()) {
                let (token, cookies) = (a.token.clone(), cookie_string(&a.cookies));
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    if let Ok(c) = crate::client::DsClient::new(token, cookies) {
                        match c.delete_session(&old).await {
                            Ok(()) => eprintln!("[session] đã xoá session cũ {}", old),
                            Err(e) => eprintln!("[session] xoá session cũ {} lỗi: {}", old, e),
                        }
                    }
                });
            }
            a.thread_session_id = Some(sid.to_string());
            a.thread_uses = 1;
        }
    }
}

/// Pipeline parse tool-call DUY NHẤT (cả stream lẫn non-stream): chuẩn hoá biến thể rồi thử lần lượt các parser.
pub(crate) fn parse_any_tool_calls(text: &str) -> Option<(String, Vec<DsmlToolCall>)> {
    let n = normalize_dsml(text);
    parse_dsml_tool_calls(&n)
        .or_else(|| parse_dsml_json_tool_calls(&n))
        .or_else(|| parse_dsml_nested_tool_calls(&n))
        .or_else(|| parse_xml_tool_call(&n).map(|(b, c)| (b, vec![c])))
}

/// id tool-call duy nhất — `call_0` lặp ở mọi step làm model/client nhầm result ↔ call.
pub(crate) fn new_call_id() -> String {
    format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

/// Suy tên tool cho call bị mất tên: tool có đủ mọi khoá args trong schema, ưu tiên tool thoả hết `required`.
pub(crate) fn infer_tool_name(tools: &Option<Vec<Value>>, call: &mut DsmlToolCall) {
    if !call.name.is_empty() { return; }
    let Some(tools) = tools else { return };
    let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&call.arguments) else { return };
    let mut best: Option<(usize, String)> = None;
    for f in tools.iter().filter_map(|t| t.get("function")) {
        let Some(name) = f.get("name").and_then(|n| n.as_str()) else { continue };
        let props = f.pointer("/parameters/properties").and_then(|p| p.as_object());
        let Some(props) = props else { continue };
        if !args.keys().all(|k| props.contains_key(k)) { continue; }
        let req_ok = f.pointer("/parameters/required").and_then(|r| r.as_array())
            .is_none_or(|r| r.iter().filter_map(|x| x.as_str()).all(|k| args.contains_key(k) || k == "description"));
        let score = args.len() * 2 + usize::from(req_ok) * 100;
        if best.as_ref().is_none_or(|(b, _)| score > *b) { best = Some((score, name.to_string())); }
    }
    if let Some((_, n)) = best { call.name = n; }
}

/// Call có hợp lệ theo schema không (tên có trong tools, đủ `required` kiểu string/số…). Lỗi → đừng gửi cho client.
pub(crate) fn call_is_valid(tools: &Option<Vec<Value>>, call: &DsmlToolCall) -> bool {
    let Some(tools) = tools else { return true };
    let Some(f) = tools.iter().filter_map(|t| t.get("function")).find(|f| f.get("name").and_then(|n| n.as_str()) == Some(call.name.as_str())) else { return false };
    let Ok(Value::Object(args)) = serde_json::from_str::<Value>(&call.arguments) else { return false };
    f.pointer("/parameters/required").and_then(|r| r.as_array())
        .is_none_or(|r| r.iter().filter_map(|x| x.as_str()).all(|k| args.contains_key(k)))
}

/// Điền các property string bắt buộc mà model bỏ sót (vd bash.description) theo schema tools của client,
/// thay vì để client trả "invalid arguments: missing required property".
pub(crate) fn fill_required_args(tools: &Option<Vec<Value>>, call: &mut DsmlToolCall) {
    let Some(tools) = tools else { return };
    let Some(params) = tools.iter()
        .filter_map(|t| t.get("function"))
        .find(|f| f.get("name").and_then(|n| n.as_str()) == Some(call.name.as_str()))
        .and_then(|f| f.get("parameters")) else { return };
    let required: Vec<Value> = params.get("required").and_then(|r| r.as_array()).cloned().unwrap_or_default();
    let Ok(Value::Object(mut args)) = serde_json::from_str::<Value>(&call.arguments) else { return };
    // ép kiểu theo schema: model viết mọi giá trị là chuỗi, schema đòi số/bool/mảng/object → client báo invalid arguments
    if let Some(props) = params.get("properties").and_then(|p| p.as_object()) {
        for (k, sch) in props {
            let ty = sch.get("type").and_then(|t| t.as_str()).unwrap_or("string");
            if !matches!(ty, "integer" | "number" | "boolean" | "array" | "object") { continue; }
            if let Some(Value::String(sv)) = args.get(k) {
                if let Ok(v) = serde_json::from_str::<Value>(sv.trim()) {
                    let ok = match ty {
                        "integer" => v.is_i64() || v.is_u64(),
                        "number" => v.is_number(),
                        "boolean" => v.is_boolean(),
                        "array" => v.is_array(),
                        _ => v.is_object(),
                    };
                    if ok { args.insert(k.clone(), v); }
                }
            }
        }
    }
    for r in required.iter().filter_map(|r| r.as_str()) {
        if args.contains_key(r) { continue; }
        let is_str = params.get("properties").and_then(|p| p.get(r))
            .and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("string");
        if !is_str { continue; }
        let fallback = if r == "description" {
            let cmd = args.get("command").or_else(|| args.get("file_path")).and_then(|v| v.as_str()).unwrap_or(&call.name);
            cmd.chars().take(60).collect::<String>()
        } else { continue };
        args.insert(r.to_string(), Value::String(fallback));
    }
    call.arguments = Value::Object(args).to_string();
}

/// Chèn space chuẩn sau mọi '<｜｜DSML｜｜' / '</｜｜DSML｜｜' dính liền tên tag
/// ('<｜｜DSML｜｜invoke id="call_0" name="bash">', '<｜｜DSML｜｜:parameter name=..>') —
/// biến thể có attr thêm (id=...) lọt khỏi các replace cố định bên dưới.
fn space_dsml_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut rest = text;
    while let Some(p) = rest.find("｜｜DSML｜｜") {
        let end = p + "｜｜DSML｜｜".len();
        out.push_str(&rest[..end]);
        rest = &rest[end..];
        let trimmed = rest.trim_start_matches([':', '_']);
        if trimmed.starts_with(|c: char| c.is_ascii_alphabetic()) {
            out.push(' ');
            rest = trimmed;
        }
    }
    out.push_str(rest);
    out
}

/// Chuẩn hoá số thanh dọc quanh 'DSML': model hay nhả '<｜DSML｜｜ calls>' / '</｜DSML｜｜ invoke>' (1-3 gạch) → luôn 2 gạch mỗi bên.
fn fix_dsml_bars(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(p) = rest.find("DSML") {
        let before = &rest[..p];
        let n1 = before.chars().rev().take_while(|c| *c == '｜').count();
        let after = &rest[p + 4..];
        let n2 = after.chars().take_while(|c| *c == '｜').count();
        if n1 == 0 && n2 == 0 {
            out.push_str(&rest[..p + 4]);
            rest = after;
            continue;
        }
        out.push_str(&before[..before.len() - n1 * '｜'.len_utf8()]);
        out.push_str("｜｜DSML｜｜");
        rest = &after[n2 * '｜'.len_utf8()..];
    }
    out.push_str(rest);
    out
}

/// '<｜｜DSML｜｜ file_path" string="true">' (mất 'parameter name="') → dạng chuẩn.
fn fix_mangled_param_tags(text: &str) -> String {
    const OPEN: &str = "<｜｜DSML｜｜ ";
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(p) = rest.find(OPEN) {
        out.push_str(&rest[..p + OPEN.len()]);
        let after = &rest[p + OPEN.len()..];
        let ident: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let tail = &after[ident.len()..];
        let is_tool_tag = matches!(ident.as_str(), "calls" | "invoke" | "parameter" | "tool_calls");
        if !ident.is_empty() && !is_tool_tag && tail.starts_with("\" string=") {
            out.push_str(&format!("parameter name=\"{}", ident));
            rest = tail;
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Xoá mảnh markup DSML còn sót khỏi phần prose.
fn strip_dsml_fragments(text: &str) -> String {
    let mut t = text.to_string();
    while let Some(p) = t.find("DSML") {
        let start = t[..p].rfind('<').unwrap_or(p);
        let end = t[p..].find('>').map(|e| p + e + 1).unwrap_or(t.len());
        t.replace_range(start..end, "");
    }
    t.trim().to_string()
}

pub(crate) fn parse_dsml_tool_calls(text: &str) -> Option<(String, Vec<DsmlToolCall>)> {
    // DS web đổi format theo phiên bản: '<｜｜DSML｜｜ calls>' / '<｜｜DSML｜｜tool_calls>' /
    // '<｜｜DSML｜｜ calls>' có space — normalize mọi biến thể về '<｜｜DSML｜｜ calls>' trước parse
    let mut norm: String = {
        let mut out = fix_mangled_param_tags(&space_dsml_tags(&fix_dsml_bars(text)))
            .replace("<｜｜DSML｜｜tool_calls>", "<｜｜DSML｜｜ calls>")
            .replace("</｜｜DSML｜｜tool_calls>", "</｜｜DSML｜｜ calls>")
            .replace("<｜｜DSML｜｜invoke name=", "<｜｜DSML｜｜ invoke name=")
            .replace("</｜｜DSML｜｜invoke>", "</｜｜DSML｜｜ invoke>")
            .replace("<｜｜DSML｜｜parameter name=", "<｜｜DSML｜｜ parameter name=")
            .replace("</｜｜DSML｜｜parameter>", "</｜｜DSML｜｜ parameter>");
        // variant: '</｜｜DSML｜｜ tool_calls>' có space lẻ
        if out.contains("</｜｜DSML｜｜ tool_calls>") {
            out = out.replace("</｜｜DSML｜｜ tool_calls>", "</｜｜DSML｜｜ calls>");
        }
        if out.contains("<｜｜DSML｜｜ tool_calls>") {
            out = out.replace("<｜｜DSML｜｜ tool_calls>", "<｜｜DSML｜｜ calls>");
        }
        out
    };
    // gộp open-tag 'calls' lặp liền nhau: '<｜｜DSML｜｜ calls><｜｜DSML｜｜ calls>' → 1
    let dup = "<｜｜DSML｜｜ calls><｜｜DSML｜｜ calls>";
    let single = "<｜｜DSML｜｜ calls>";
    while let Some(i) = norm.find(dup) {
        norm.replace_range(i..i + single.len(), "");
    }
    // model bỏ thẻ mở invoke (calls → parameter… → /invoke): chèn invoke không tên, tên suy ra từ schema sau
    if let Some(pp) = norm.find("<｜｜DSML｜｜ parameter") {
        let inv = norm.find("<｜｜DSML｜｜ invoke");
        if inv.is_none_or(|i| pp < i) && norm.contains("</｜｜DSML｜｜ invoke>") {
            norm.insert_str(pp, "<｜｜DSML｜｜ invoke name=\"\">");
        }
    }
    // thẻ <parameter name=...> trơn lẫn trong khối DSML → đưa về dạng DSML
    if norm.contains("<｜｜DSML｜｜ invoke") {
        norm = norm.replace("<parameter name=\"", "<｜｜DSML｜｜ parameter name=\"");
    }
    let text = &norm;
    // model đôi khi bỏ thẻ mở 'calls' (chỉ nhả các invoke) hoặc stream cụt trước thẻ đóng
    let start = text.find("<｜｜DSML｜｜ calls>").or_else(|| text.find("<｜｜DSML｜｜ invoke"))?;
    let close_calls = text.find("</｜｜DSML｜｜ calls>");
    let had_close = close_calls.is_some();
    let end_block = close_calls.unwrap_or(text.len());
    let before = strip_dsml_fragments(&text[..start]);
    let block = &text[start..end_block];

    let mut calls = Vec::new();
    // tách từng invoke — model nhiều khi chèn id attr: <invoke id="call_0" name="bash">
    // → tách bằng marker '<｜｜DSML｜｜ invoke' rồi tìm name= sau đó
    let mut rest = block;
    while let Some(p) = rest.find("<｜｜DSML｜｜ invoke") {
        let after_open = &rest[p + "<｜｜DSML｜｜ invoke".len()..];
        // tìm name="..." trong phần attr (sau '>' hoặc trước '>' — luôn trước '>' đầu tiên)
        let Some(gt) = after_open.find('>') else { break };
        let attrs = &after_open[..gt];
        // thiếu name= thì model hay để tên tool trong id="write" → dùng id
        let name_pos = attrs.find("name=\"").map(|i| i + "name=\"".len())
            .or_else(|| attrs.find("id=\"").map(|i| i + "id=\"".len()));
        let Some(np) = name_pos else { break };
        let after_name_start = &attrs[np..];
        let Some(name_end) = after_name_start.find("\"") else { break };
        let name = after_name_start[..name_end].to_string();
        // params bắt đầu sau '>' mở tag
        let after_name_full = &after_open[gt + 1..];
        // giới hạn trong invoke này — nếu không, param của các invoke sau bị gộp vào call đầu
        let inv_end = after_name_full.find("</｜｜DSML｜｜ invoke>");
        let after_name = match inv_end { Some(e) => &after_name_full[..e], None => after_name_full };

        // các parameter: name="..." string="true"
        let mut args = serde_json::Map::new();
        let mut cursor = after_name;
        while let Some(pp) = cursor.find("<｜｜DSML｜｜ parameter name=\"") {
            let after_p = &cursor[pp + "<｜｜DSML｜｜ parameter name=\"".len()..];
            let Some(pend) = after_p.find("\"") else { break };
            let pname = after_p[..pend].to_string();
            let after_pn = &after_p[pend + 1..];
            // value nằm giữa '>' và close-tag '</｜｜DSML｜｜ parameter...'
            // (model nhiều khi đè attr vào close-tag: '</｜｜DSML｜｜ parameter name="desc"...>')
            let Some(vstart) = after_pn.find('>') else { break };
            let value_part = &after_pn[vstart + 1..];
            let marker = "</｜｜DSML｜｜ parameter";
            let Some(vend) = value_part.find(marker) else { break };
            let mut value = value_part[..vend].to_string();
            // thẻ mở param lặp ('<parameter name="command">' rồi lại '<parameter name="command" string="true">giá trị') → lấy sau thẻ mở cuối
            while let Some(np) = value.find("<｜｜DSML｜｜ parameter") {
                match value[np..].find('>') {
                    Some(g) => value = value[np + g + 1..].to_string(),
                    None => break,
                }
            }
            let is_json = after_pn[..vstart].contains("string=\"false\"");
            // skip junk param rỗng (model nhiều khi emit <parameter name="bash"></parameter>)
            if !pname.is_empty() && !value.is_empty() {
                let v = if is_json { serde_json::from_str(value.trim()).unwrap_or(serde_json::Value::String(value)) } else { serde_json::Value::String(value) };
                args.insert(pname, v);
            }
            // advance qua '>' kế sau marker (đóng tag, bất kể attr đè vào)
            let after_close = &value_part[vend + marker.len()..];
            cursor = match after_close.find('>') {
                Some(i) => &after_close[i + 1..],
                None => after_close,
            };
        }
        // invoke chưa đóng + không có thẻ đóng calls = stream bị cụt → bỏ, tránh gọi tool với args dở
        if inv_end.is_none() && !had_close {
            break;
        }
        calls.push(DsmlToolCall {
            name,
            arguments: serde_json::Value::Object(args).to_string(),
                    });
        // tới hết invoke này
        if let Some(inv_end) = inv_end {
            rest = &after_name_full[inv_end + "</｜｜DSML｜｜ invoke>".len()..];
        } else {
            break;
        }
    }
    // model hay lặp y nguyên 1 invoke trong cùng batch (vd 2 lần read vite.config.js) → bỏ bản trùng
    let mut seen = std::collections::HashSet::new();
    calls.retain(|c| seen.insert((c.name.clone(), c.arguments.clone())));
    if calls.is_empty() {
        None
    } else {
        Some((before, calls))
    }
}


/// Variant 3 GENERIC: DS web shuffle separator liên tục:
///   <dsml:tool_call>{"name":"bash","arguments":{...}}</dsml:tool_call>
///   <dsml_tool_call>...</dsml_tool_call> / <dsml-tool-call>...</dsml-tool-call>
///   <dsml tool_call>... / <dsml:bash command="..." description="..."/>
/// Parser: mở thẻ bất kỳ '<dsml' + [:-_ ]* + tên-tag, body = JSON name/arguments
/// hoặc attribute command/description.
fn parse_dsml_json_tool_calls(text: &str) -> Option<(String, Vec<DsmlToolCall>)> {
    // tìm TẤT CẢ các mở thẻ '<dsml...' (không phân biệt separator)
    let mut open_pos: Option<usize> = None;
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'<'
            && (text[i + 1..].starts_with("dsml")
                || text[i + 1..].starts_with("bash")
                || text[i + 1..].starts_with("｜｜DSML｜｜"))
        {
            open_pos = Some(i);
            break;
        }
    }
    let start = open_pos?;
    let before = text[..start].trim().to_string();
    // tìm hết body: từ '<dsml' đến '}' đóng JSON hoặc '/>'
    let mut calls = Vec::new();
    let mut rest = &text[start..];
    while let Some(rp) = ["<dsml", "<bash", "<｜｜DSML｜｜"].iter().filter_map(|m| rest.find(m)).min() {
        let after = &rest[rp..];
        // body: sau '>' mở thẻ
        let Some(gt) = after.find('>') else { break };
        let tag_name_part = &after[..gt]; // vd "<dsml:tool_call" hoặc "<dsml tool_call" hoặc "<dsml:bash command=\".."
        // có attr ngay trong tag? (dạng <dsml:bash command="..." description="...">)
        let attr_call = {
            let cmd = tag_name_part.split("command=\"").nth(1)
                .and_then(|x| x.split('"').next()).map(|x| x.to_string());
            match cmd {
                Some(command) => {
                    let desc = tag_name_part.split("description=\"").nth(1)
                        .and_then(|x| x.split('"').next()).unwrap_or("").to_string();
                    let mut args = serde_json::Map::new();
                    args.insert("command".to_string(), serde_json::Value::String(command));
                    if !desc.is_empty() {
                        args.insert("description".to_string(), serde_json::Value::String(desc));
                    }
                    Some(DsmlToolCall {
                        name: "bash".to_string(),
                        arguments: serde_json::Value::Object(args).to_string(),
                                            })
                }
                None => None,
            }
        };
        if let Some(c) = attr_call {
            calls.push(c);
            // advance qua '>' của tag mở; thẻ đóng (nếu có) do vòng lặp sau xử lý
            rest = &after[gt + 1..];
            continue;
        }
        // JSON body sau tag mở — có thể là JSON trần HOẶC bọc trong <...arguments>{...}</...>
        let mut body_area = &after[gt + 1..];
        // nếu có tag <arguments>-like trước JSON → nhảy tới trong tag đó
        if let Some(ap) = body_area.find("arguments>") {
            // nếu '{' đầu tiên nằm SAU tag arguments-mở hơn '{' ngay đầu
            let brace_in_args = body_area[ap..].find('{').map(|x| ap + x);
            let brace_head = body_area.find('{');
            if let (Some(a), Some(h)) = (brace_in_args, brace_head) {
                if a > h { body_area = &body_area[h..]; }  // JSON trần trước — dùng luôn
                else { body_area = &body_area[a..]; }
            }
        }
        // vùng body có NHIỀU JSON object nối tiếp (mỗi dòng 1 call) hoặc 1 object — scan hết:
        let mut scan = body_area;
        let mut any = false;
        while let Some(br) = scan.find('{') {
            // match braces BỎ QUA nội dung JSON string (escape-aware) — command có thể chứa '{}'
            let json_str = {
                let b = &scan.as_bytes()[br..];
                let mut depth = 0i32;
                let mut in_str = false;
                let mut esc = false;
                let mut endj = None;
                for (i, ch) in b.iter().enumerate() {
                    if esc { esc = false; continue; }
                    match ch {
                        b'\\' if in_str => esc = true,
                        b'"' => in_str = !in_str,
                        b'{' if !in_str => depth += 1,
                        b'}' if !in_str => {
                            depth -= 1;
                            if depth == 0 { endj = Some(i + 1); break; }
                        }
                        _ => {}
                    }
                }
                endj.map(|e| scan[br..br + e].to_string())
            };
            let Some(json_str) = json_str else { break };
            // model DS emit JSON escape sai (vd '\;' trong shell command) — sửa trước khi parse
            let fixed: String = {
                let b = json_str.as_bytes();
                let mut out = Vec::with_capacity(b.len());
                let mut i = 0;
                while i < b.len() {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        let nx = b[i + 1];
                        let valid = matches!(nx, b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u');
                        if valid {
                            out.push(b[i]); out.push(nx);
                        } else {
                            out.push(nx); // bỏ backslash lỗi: '\;' → ';'
                        }
                        i += 2;
                    } else {
                        out.push(b[i]);
                        i += 1;
                    }
                }
                String::from_utf8_lossy(&out).into_owned()
            };
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&fixed) {
                // format gộp: {"tool_calls":[{name,arguments},...]} → expand
                if let Some(arr) = v.get("tool_calls").and_then(|x| x.as_array()) {
                    for item in arr {
                        let name = item.get("name").and_then(|x| x.as_str())
                            .or_else(|| item.get("tool").and_then(|x| x.as_str()))
                            .unwrap_or("bash").to_string();
                        let args = item.get("arguments").cloned()
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                        calls.push(DsmlToolCall {
                            name,
                            arguments: args.to_string(),
                                                    });
                        any = true;
                    }
                    continue;
                }
                // key biến thể: "name" hoặc "tool"; args ở "arguments" hoặc top-level command/description
                let name = v.get("name").and_then(|x| x.as_str())
                    .or_else(|| v.get("tool").and_then(|x| x.as_str()))
                    .unwrap_or("bash").to_string();
                let args = v.get("arguments").cloned()
                    .unwrap_or_else(|| {
                        let mut m = serde_json::Map::new();
                        if let Some(c) = v.get("command") { m.insert("command".into(), c.clone()); }
                        if let Some(d) = v.get("description") { m.insert("description".into(), d.clone()); }
                        serde_json::Value::Object(m)
                    });
                let has_args = args.as_object().map(|o| !o.is_empty()).unwrap_or(false);
                if has_args {
                    calls.push(DsmlToolCall {
                        name,
                        arguments: args.to_string(),
                                            });
                    any = true;
                }
            }
            scan = &scan[br + json_str.len()..];
        }
        if !any { break; }
        // advance qua close tag '</dsml...>' — tìm '>' kế tiếp
        match scan.find('>') {
            Some(cp) => rest = &scan[cp + 1..],
            None => break,
        }
    }
    if calls.is_empty() { None } else { Some((before, calls)) }
}

/// Variant 4 GENERIC nested: '<dsml:tool_call><dsml:invoke name="bash"><dsml:parameter name="k">v</dsml:parameter>...'
/// và '<dsml-tool name="bash"><parameter name="k">v</parameter>...</dsml-tool>'
/// Điểm chung: MỘT tag chứa attribute name="tool_name", sau đó các tag <...parameter name="k">value
fn parse_dsml_nested_tool_calls(text: &str) -> Option<(String, Vec<DsmlToolCall>)> {
    // GENERIC: quét text, tìm tool-call units theo nhiều hình thái:
    //  A) '<X invoke/tool/bash-tag name="TOOL" ...' rồi các tag parameter (name=K>v</tag>) — multi-call
    //  B) '<bash command="C" description="D"/>' — attr-style, name mặc định bash
    //  C) '<tool_name>X</tool_name><parameters><command>C</command><description>D</description></parameters>'
    let mut calls: Vec<DsmlToolCall> = Vec::new();
    let mut before_end = 0usize; // vị trí kết thúc text "before" (trước unit đầu tiên)
    let mut cursor = 0usize;
    let bytes_len = text.len();

    while cursor < bytes_len {
        let rest = &text[cursor..];
        // tìm tag mở kế tiếp
        let Some(lt) = rest.find('<') else { break };
        let Some(gt_rel) = rest[lt..].find('>') else { break };
        let gt = lt + gt_rel;
        let tag_full = &rest[lt..=gt]; // gồm '<' và '>'
        let tag_inner = &tag_full[1..tag_full.len() - 1]; // bỏ <> (chưa xử self-closing)

        let lower = tag_inner.to_ascii_lowercase();
        let is_close = tag_inner.starts_with('/');
        if is_close {
            cursor += gt + 1;
            continue;
        }
        // container tag (calls/tool_calls wrapper) — không phải unit: cố định before (text trước
        // container) rồi skip
        if lower.contains("calls") && !lower.contains("call=\"") {
            if calls.is_empty() {
                before_end = cursor + lt;
            }
            cursor += gt + 1;
            continue;
        }

        // CASE B: attr command= ngay trong tag (bash/ bất kỳ) — self-closing hoặc có body
        if let Some(_cmd_part) = lower.split("command=\"").nth(1) {
            // value trong ORIGINAL tag (lowercase chỉ để detect) — dùng text gốc
            let orig_cmd = tag_full.split("command=\"").nth(1)
                .and_then(|x| x.split('"').next()).unwrap_or("").to_string();
            if !orig_cmd.is_empty() {
                let desc = tag_full.split("description=\"").nth(1)
                    .and_then(|x| x.split('"').next()).unwrap_or("").to_string();
                let name = if lower.starts_with("bash") { "bash" } else {
                    // tên tag trước space/:
                    tag_inner.split([' ', ':', '-']).next().unwrap_or("bash")
                };
                let mut m = serde_json::Map::new();
                m.insert("command".into(), serde_json::Value::String(orig_cmd));
                if !desc.is_empty() {
                    m.insert("description".into(), serde_json::Value::String(desc));
                }
                if calls.is_empty() { before_end = cursor + lt; }
                calls.push(DsmlToolCall {
                    name: name.to_string(),
                    arguments: serde_json::Value::Object(m).to_string(),
                                    });
                cursor += gt + 1;
                continue;
            }
        }

        // CASE A/C: tag invoke/tool-name với name="TOOL" hoặc <tool_name>X</tool_name>
        let mut tool_name: Option<String> = None;
        if let Some(_np) = lower.find("name=\"") {
            // phải là tag invoke/tool (không phải parameter name=...)
            if lower.contains("invoke") || lower.contains("tool") && !lower.contains("parameter") {
                // value trong tag gốc
                let pos = {
                    // tìm vị trí name= trong tag_full (case-insensitive approx — tag gốc thường đúng case)
                    tag_full.find("name=\"").map(|x| x + 6).unwrap_or(0)
                };
                let after_q = &tag_full[pos..];
                tool_name = Some(after_q[..after_q.find('"').unwrap_or(0)].to_string());
            }
        }
        if tool_name.is_none() && lower.starts_with("tool_name") {
            // CASE C mở: <tool_name>X</tool_name>
            let after_gt = &rest[gt + 1..];
            if let Some(cl) = after_gt.find("</tool_name>") {
                tool_name = Some(after_gt[..cl].to_string());
                // sau đó: <parameters>...</parameters> hoặc tags command/description
                let rest2 = &after_gt[cl + "</tool_name>".len()..];
                // thu các <command>C</command> <description>D</description>
                let mut m = serde_json::Map::new();
                for key in ["command", "description"] {
                    let open_tag = format!("<{}>", key);
                    if let Some(kp) = rest2.find(&open_tag) {
                        let varea = &rest2[kp + open_tag.len()..];
                        if let Some(ve) = varea.find('<') {
                            m.insert(key.into(), serde_json::Value::String(varea[..ve].to_string()));
                        }
                    }
                }
                if !m.is_empty() {
                    if calls.is_empty() { before_end = cursor + lt; }
                    calls.push(DsmlToolCall {
                        name: tool_name.unwrap(),
                        arguments: serde_json::Value::Object(m).to_string(),
                                            });
                    cursor += gt + 1 + cl + "</tool_name>".len();
                    continue;
                }
            }
        }
        // CASE D: tag bash/invoke KHÔNG attr (vd '<dsml:bash>') — params từ child tags
        // <...command>C</...command> <...description>D</...description>
        let looks_tool = lower.starts_with("bash") || lower.contains("invoke")
            || (lower.starts_with("dsml") && !lower.contains("calls") && !lower.contains("tool_call"));
        if tool_name.is_none() && looks_tool {
            let after_tag = &rest[gt + 1..];
            let base: String = tag_inner.split([' ', ':']).next().unwrap_or("").to_string();
            let close_marker = format!("</{}", base);
            let body_len = after_tag.find(&close_marker).unwrap_or(after_tag.len());
            let _body = &after_tag[..body_len];
            let mut m = serde_json::Map::new();
            // value nằm GIỮA '>' và '<' close — close nằm NGOÀI body_len (body cắt trước close!)
            // → value area = after_tag (toàn bộ phần sau tag mở)
            for key in ["command", "description"] {
                let needle = format!("{}>", key);
                let mut pos = 0;
                while let Some(kp) = after_tag[pos..].find(&needle) {
                    let abs = pos + kp;
                    let pre_ok = abs > 0 && matches!(after_tag.as_bytes()[abs - 1], b':' | b'>' | b' ');
                    if pre_ok {
                        let varea = &after_tag[abs + needle.len()..];
                        if let Some(ve) = varea.find('<') {
                            let value = varea[..ve].to_string();
                            if !value.is_empty() {
                                m.insert(key.into(), serde_json::Value::String(value));
                            }
                        }
                        break;
                    }
                    pos = abs + needle.len();
                }
            }
            if !m.is_empty() {
                if calls.is_empty() { before_end = cursor + lt; }
                calls.push(DsmlToolCall {
                    name: "bash".to_string(),
                    arguments: serde_json::Value::Object(m).to_string(),
                                    });
                cursor += gt + 1 + body_len;
                continue;
            }
        }
        if let Some(tn) = tool_name {
            // CASE A: thu params từ tags phía sau: mọi 'name="K">value<' hoặc '<K>v</K>'
            let after_tag = &rest[gt + 1..];
            // body hết khi gặp tag đóng khớp (chứa '/' + cùng tên base)
            let base: String = tag_inner
                .split([' ', ':'])
                .next().unwrap_or("").to_string();
            let close_marker = format!("</{}", base);
            let body_len = after_tag.find(&close_marker).unwrap_or(after_tag.len());
            let body = &after_tag[..body_len];
            let mut m = serde_json::Map::new();
            // 1. attr style: name="K">v<
            let mut idx = 0;
            while let Some(np) = body[idx..].find("name=\"") {
                let abs = idx + np;
                let after = &body[abs + 6..];
                let Some(q) = after.find('"') else { break };
                let pname = after[..q].to_string();
                let rest2 = &after[q + 1..];
                let Some(g2) = rest2.find('>') else { break };
                let varea = &rest2[g2 + 1..];
                let Some(ev) = varea.find('<') else { break };
                let value = varea[..ev].to_string();
                if !pname.is_empty() && !value.is_empty() && pname != "name" {
                    m.insert(pname, serde_json::Value::String(value));
                }
                idx = abs + 6 + q + 1 + g2 + 1 + ev;
            }
            // 2. tag style: <command>v</command>
            for key in ["command", "description"] {
                if !m.contains_key(key) {
                    let open_tag = format!("<{}>", key);
                    if let Some(kp) = body.find(&open_tag) {
                        let varea = &body[kp + open_tag.len()..];
                        if let Some(ve) = varea.find('<') {
                            m.insert(key.into(), serde_json::Value::String(varea[..ve].to_string()));
                        }
                    }
                }
            }
            if !m.is_empty() {
                if calls.is_empty() { before_end = cursor + lt; }
                calls.push(DsmlToolCall {
                    name: tn,
                    arguments: serde_json::Value::Object(m).to_string(),
                                    });
                cursor += gt + 1 + body_len;
                continue;
            }
        }
        cursor += gt + 1;
    }

    if calls.is_empty() { None } else {
        Some((text[..before_end].trim().to_string(), calls))
    }
}

// ---------- OpenAI-compatible API ----------

#[derive(Deserialize)]
pub(crate) struct ChatRequest {
    pub(crate) model: Option<String>,
    pub(crate) messages: Vec<Value>,
    pub(crate) stream: Option<bool>,
    // true = tạo conversation mới; false/mặc định = chat tiếp thread hiện có
    pub(crate) new_thread: Option<bool>,
    // Bật DeepThink (thinking) / Tìm kiếm thông minh (websearch). Mặc định cả hai OFF.
    pub(crate) thinking: Option<bool>,
    pub(crate) search: Option<bool>,
    // mức suy nghĩ kiểu OpenAI: "none" = Nhanh, "max"/khác = DeepThink
    #[serde(default)]
    pub(crate) reasoning_effort: Option<String>,
    #[serde(default)]
    pub(crate) reasoning: Option<Value>,
    // OpenAI tools schema từ client (dsh) — render thành hướng dẫn DSML cho model
    #[serde(default)]
    pub(crate) tools: Option<Vec<Value>>,
}

// Render tool specs (OpenAI schema) thành hướng dẫn gọi DSML — NHẤN MẠNH: mọi parameter
// trong "required" PHẢI có mặt trong arguments (dsh schema-reject call thiếu description).
/// Format tool-call chuẩn — ĐÚNG format DS web tự dùng; hướng dẫn, lịch sử và parser đều bám format này
/// (trước đây hướng dẫn nói `<dsml:tool_call>{json}` còn lịch sử render DSML → model trộn nhiều biến thể).
const TOOL_FORMAT_GUIDE: &str = "# Tools\n\
To call tools, END your reply with exactly one block in this format (nothing after it):\n\
<｜｜DSML｜｜ calls>\n\
<｜｜DSML｜｜ invoke name=\"TOOL_NAME\">\n\
<｜｜DSML｜｜ parameter name=\"PARAM_NAME\" string=\"true\">VALUE</｜｜DSML｜｜ parameter>\n\
</｜｜DSML｜｜ invoke>\n\
</｜｜DSML｜｜ calls>\n\
Use string=\"true\" for text values; for numbers, booleans, arrays and objects use string=\"false\" with a JSON value. \
Several invoke elements may share one calls block. Include every REQUIRED parameter. \
Keep any text before the block short. If you need no tool, answer in plain text with no block.\n";

pub(crate) fn render_tools_spec(tools: &Option<Vec<Value>>) -> String {
    let Some(tl) = tools else { return String::new() };
    if tl.is_empty() { return String::new(); }
    let mut out = String::from(TOOL_FORMAT_GUIDE);
    out.push_str("\nAvailable tools:\n");
    for t in tl {
        let f = t.get("function").unwrap_or(t);
        let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let desc = f.get("description").and_then(|x| x.as_str()).unwrap_or("");
        out.push_str(&format!("- {}: {}\n", name, desc));
        if let Some(params) = f.get("parameters").and_then(|p| p.get("properties")).and_then(|p| p.as_object()) {
            let required: Vec<&str> = f.get("parameters")
                .and_then(|p| p.get("required")).and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            for (k, v) in params {
                let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("string");
                let en = v.get("enum").and_then(|e| e.as_array())
                    .map(|a| format!(" one of {}", a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("|")))
                    .unwrap_or_default();
                let req_mark = if required.contains(&k.as_str()) { ", REQUIRED" } else { "" };
                let pd = v.get("description").and_then(|x| x.as_str()).unwrap_or("");
                out.push_str(&format!("  param {} ({}{}{}): {}\n", k, ty, en, req_mark, pd));
            }
        }
    }
    out
}

pub(crate) fn render_content(m: &Value) -> String {
    // content có thể là string hoặc array of parts [{type:"text"|"tool_result"|"tool_call"...}]
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                match part.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                    // ảnh: nội dung thật được upload riêng, trong prompt chỉ để dấu vị trí
                    Some("image_url") | Some("image") | Some("input_image") => {
                        out.push_str("[hình ảnh đính kèm]\n");
                    }
                    // tool result: text lồng [{type:"text",text:"..."}]
                    Some("tool_result") => {
                        if let Some(inner) = part.get("content") {
                            match inner {
                                Value::String(s) => {
                                    out.push_str(s);
                                    out.push('\n');
                                }
                                Value::Array(items) => {
                                    for it in items {
                                        if let Some(t) = it.get("text").and_then(|x| x.as_str()) {
                                            out.push_str(t);
                                            out.push('\n');
                                        } else if matches!(it.get("type").and_then(|t| t.as_str()), Some("image_url") | Some("image") | Some("input_image")) {
                                            out.push_str("[hình ảnh đính kèm]\n");
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            out.trim_end().to_string()
        }
        _ => String::new(),
    }
}

fn render_tool_calls(m: &Value) -> String {
    // assistant message có tool_calls → render lại đúng format chuẩn để model thấy chính nó đã gọi gì
    let mut out = String::new();
    if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
        out.push_str("\n<｜｜DSML｜｜ calls>");
        for tc in tcs {
            let name = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("unknown");
            out.push_str(&format!("\n<｜｜DSML｜｜ invoke name=\"{}\">", name));
            if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")) {
                // arguments là JSON string — parse rồi render từng parameter
                let parsed: Option<Value> = args
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .or_else(|| args.as_object().map(|_| args.clone()));
                if let Some(obj) = parsed.and_then(|v| v.as_object().cloned()) {
                    for (k, v) in obj {
                        // chuỗi giữ nguyên (kể cả xuống dòng); kiểu khác ghi JSON + string="false"
                        let (flag, sv) = match &v {
                            Value::String(x) => ("true", x.clone()),
                            other => ("false", other.to_string()),
                        };
                        out.push_str(&format!(
                            "\n<｜｜DSML｜｜ parameter name=\"{}\" string=\"{}\">{}</｜｜DSML｜｜ parameter>",
                            k, flag, sv
                        ));
                    }
                }
            }
            out.push_str("\n</｜｜DSML｜｜ invoke>");
        }
        out.push_str("\n</｜｜DSML｜｜ calls>");
    }
    out
}

pub(crate) fn messages_to_prompt(messages: &[Value]) -> String {
    messages_to_prompt_from(messages, 0)
}

/// id tool-call → tên tool, dùng để gắn nhãn tool_result.
pub(crate) fn call_name_map(messages: &[Value]) -> std::collections::HashMap<String, String> {
    let mut call_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for m in messages {
        if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                if let (Some(id), Some(n)) = (tc.get("id").and_then(|x| x.as_str()), tc.pointer("/function/name").and_then(|x| x.as_str())) {
                    call_names.insert(id.to_string(), n.to_string());
                }
            }
        }
    }
    call_names
}

/// Render MỘT message thành đoạn prompt (None = bỏ qua). Tách riêng để `fit_messages`
/// tính kích thước từng message một lần thay vì render lại cả danh sách sau mỗi lần cắt.
pub(crate) fn render_msg(m: &Value, call_names: &std::collections::HashMap<String, String>) -> Option<String> {
    let role = m["role"].as_str().unwrap_or("user");
    match role {
        // assistant: content + (nếu có) DSML tool calls của chính nó
        "assistant" => {
            let text = render_content(m);
            let tcs = render_tool_calls(m);
            if text.is_empty() && tcs.is_empty() {
                None
            } else {
                Some(format!("assistant: {}{}", text, tcs))
            }
        }
        // tool result message
        "tool" => {
            let text = render_content(m);
            if text.is_empty() {
                None
            } else {
                // kèm tên tool + id để model khớp result ↔ call (tránh lặp call)
                let tcid = m["tool_call_id"].as_str().unwrap_or("");
                match call_names.get(tcid) {
                    Some(n) => Some(format!("tool_result ({}) [{}]: {}", n, tcid, text)),
                    None if tcid.is_empty() => Some(format!("tool_result: {}", text)),
                    None => Some(format!("tool_result [{}]: {}", tcid, text)),
                }
            }
        }
        _ => {
            let text = render_content(m);
            if text.is_empty() {
                None
            } else {
                Some(format!("{}: {}", role, text))
            }
        }
    }
}

/// Render từ message thứ `start` (nhãn tool_result vẫn tra tên tool trên toàn bộ lịch sử).
pub(crate) fn messages_to_prompt_from(messages: &[Value], start: usize) -> String {
    let call_names = call_name_map(messages);
    messages
        .iter()
        .skip(start)
        .filter_map(|m| render_msg(m, &call_names))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn api_stats(State(st): State<Arc<AppState>>) -> Json<Value> {
    let now = now();
    let store = st.store.read().await;
    let in_flight: std::collections::HashMap<String, u32> = st.in_flight.lk().iter()
        .map(|(k, v)| (k.clone(), v.load(std::sync::atomic::Ordering::Acquire))).collect();
    let accounts: Vec<Value> = store
        .accounts
        .iter()
        .map(|a| {
            json!({
                "in_flight": in_flight.get(&a.id).copied().unwrap_or(0),
                "fail_streak": a.stats.fail_streak,
                "id": a.id,
                "label": a.label,
                "available": a.stats.available(now),
                "cooldown_until": a.stats.cooldown_until,
                "cooldown_left_secs": a.stats.cooldown_until.saturating_sub(now),
                "burst_count": a.stats.burst_count,
                "burst_limit": AccStats::burst_limit(),
                "total_ok": a.stats.total_ok,
                "total_empty": a.stats.total_empty,
                "total_error": a.stats.total_error,
                "last_used_at": a.stats.last_used_at,
                "last_status": a.last_status,
                "pool_enabled": a.pool_enabled,
            })
        })
        .collect();
    let available = store.accounts.iter().filter(|a| a.pool_enabled && a.stats.available(now)).count();
    Json(json!({
        "pool": {
            "total": store.accounts.len(),
            "available": available,
            "capacity_now_rpm_hint": available as u64 * (AccStats::burst_limit() as u64),
            "max_conc_per_acc": AccStats::max_conc(),
            "stateful": {
                "hits": st.stat_conv_hit.load(std::sync::atomic::Ordering::Relaxed),
                "misses": st.stat_conv_miss.load(std::sync::atomic::Ordering::Relaxed),
                "active_conversations": st.convs.lk().len(),
                "open_sessions": st.sessions.lk().len(),
            },
        },
        "accounts": accounts,
        "now": now,
    }))
}

async fn api_models() -> Json<Value> {
    // MỘT model duy nhất; mức suy nghĩ chọn bằng `reasoning_effort`: "none" = Nhanh (flash), còn lại ("max"…) = DeepThink.
    Json(json!({
        "object": "list",
        "data": [{
            "id": "deepseek-web", "object": "model", "owned_by": "deepseek-harness",
            "input_modalities": ["text", "image"],
            "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
            "reasoning_efforts": ["none", "max"],
            "context_length": std::env::var("DS_CONTEXT_TOKENS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(500_000)
        }]
    }))
}

// Capture session từ Chrome đang login — nút 1 click trên UI
async fn api_capture_from_chrome(State(st): State<Arc<AppState>>) -> Json<Value> {
    // Quét TẤT CẢ tab DS trong Chrome → thêm acc mới, refresh acc cũ, dedupe theo token
    // Sau khi quét xong: AUTO RESET — dispose tab OAuth ẩn danh (nếu còn mở)
    let result = crate::chrome_capture::capture_all_ds_tabs().await;
    let out = match result {
        Ok(sessions) if sessions.is_empty() => Json(json!({
            "ok": false,
            "error": "không tìm thấy tab chat.deepseek.com đã login — bấm 'Sinh link OAuth2', chọn account Google, rồi bấm lại"
        })),
        Ok(sessions) => {
            let (added, refreshed, emails) = {
                let mut store = st.store.write().await;
                let mut added = 0usize;
                let mut refreshed = 0usize;
                let mut emails: Vec<String> = Vec::new();
                for s in sessions {
                    if let Some(a) = store.accounts.iter_mut().find(|a| a.token == s.token) {
                        a.cookies = s.cookies.clone();
                        a.last_verified = Some(now());
                        a.last_status = Some("ok (refreshed)".into());
                        refreshed += 1;
                    } else {
                        let acc = Account {
                            id: uuid::Uuid::new_v4().to_string(),
                            label: s.email.clone().unwrap_or_else(|| "chrome-captured".into()),
                            token: s.token.clone(),
                            cookies: s.cookies.clone(),
                            added_at: now(),
                            last_verified: Some(now()),
                            last_status: Some("ok".into()),
                            thread_session_id: None,
                            thread_uses: 0,
                            thread_parent_message_id: None,
                            stats: Default::default(),
                            pool_enabled: true,
                        };
                        emails.push(s.email.clone().unwrap_or_else(|| "unknown".into()));
                        store.accounts.push(acc);
                        added += 1;
                    }
                }
                (added, refreshed, emails)
            };
            mark_dirty(&st);
            flush_store(&st).await;
            Json(json!({
                "ok": true,
                "action": if added > 0 { "added" } else { "refreshed" },
                "added": added,
                "refreshed": refreshed,
                "email": emails.first(),
                "total_sessions_seen": added + refreshed
            }))
        }
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    };
    out
}

// BUILD PROFILE: nuôi profile login giống real user (dạo web, cache, cookies nền)
// Trả về ngay khi bắt đầu — chạy nền, UI hiện "đang build profile"
async fn api_build_profile(State(st): State<Arc<AppState>>) -> Json<Value> {
    {
        let mut watching = st.login_watching.lk();
        if *watching {
            return Json(json!({"ok": false, "error": "đang canh login/build — đợi xong rồi thử"}));
        }
        *watching = true;
    }
    let st2 = st.clone();
    tokio::spawn(async move {
        // 6s/site — đủ để cache + cookies nền
        let res = crate::chrome_capture::build_profile(6).await;
        let mut watching = st2.login_watching.lk();
        *watching = false;
        match res {
            Ok(msg) => eprintln!("[build-profile] {}", msg),
            Err(e) => eprintln!("[build-profile] lỗi: {}", e),
        }
    });
    Json(json!({
        "ok": true,
        "note": "đang build profile (dạo web thật + cache) — ~40s, xong sẽ báo trong log; sau đó bấm Login account mới"
    }))
}

// LOGIN 1 NÚT: mở tab DS thường → user login kiểu gì cũng được → server canh tab
// → thấy token → verify → LƯU → TỰ TẮT TAB. Không cần bấm gì thêm sau khi login.
// Chỉ 1 login đang canh tại 1 thời điểm (flag in-flight).
async fn api_login_start(State(st): State<Arc<AppState>>) -> Json<Value> {
    // chặn double-start
    {
        let mut watching = st.login_watching.lk();
        if *watching {
            return Json(json!({"ok": false, "error": "đang canh 1 tab login — đợi login xong hoặc 10 phút nữa thử lại"}));
        }
        *watching = true;
    }
    let st2 = st.clone();
    tokio::spawn(async move {
        let res = login_flow(st2.clone()).await;
        {
            let mut watching = st2.login_watching.lk();
            *watching = false;
        }
        if let Err(e) = res {
            eprintln!("[login-flow] lỗi: {}", e);
        }
    });
    Json(json!({
        "ok": true,
        "note": "tab DS đã mở — login trong đó, thấy login xong phần mềm TỰ lưu session + TỰ tắt tab"
    }))
}

async fn login_flow(st: Arc<AppState>) -> Result<()> {
    // FLOW CŨ (user thích): mở tab DS trong Chrome chính (CDP 9333) → canh → tự tắt tab.
    // Chrome chính = profile real user → không dính lỗi device environment.
    let (target_id, ws_url) = crate::chrome_capture::open_ds_login_tab().await?;
    eprintln!("[login-flow] tab login mở: {}", target_id);

    // canh login tới 10 phút
    let res = crate::chrome_capture::watch_login_tab(ws_url, 600).await;

    // tự tắt tab login (Chrome chính giữ nguyên)
    let _ = crate::chrome_capture::close_target(&target_id).await;
    eprintln!("[login-flow] tab đã đóng, session đã lưu");

    let session = res?;
    eprintln!("[login-flow] captured: {:?}", session.email);

    // lưu vào store (dedupe theo token — acc cũ chỉ refresh)
    {
        let mut store = st.store.write().await;
        if let Some(a) = store.accounts.iter_mut().find(|a| a.token == session.token) {
            a.cookies = session.cookies.clone();
            a.last_verified = Some(now());
            a.last_status = Some("ok (re-login)".into());
            a.stats.reset();
        } else {
            let acc = Account {
                id: uuid::Uuid::new_v4().to_string(),
                label: session.email.clone().unwrap_or_else(|| "login-tab".into()),
                token: session.token.clone(),
                cookies: session.cookies.clone(),
                added_at: now(),
                last_verified: Some(now()),
                last_status: Some("ok".into()),
                thread_session_id: None,
                thread_uses: 0,
                thread_parent_message_id: None,
                stats: Default::default(),
                pool_enabled: true,
            };
            store.accounts.push(acc);
        }
    }
    mark_dirty(&st);
    flush_store(&st).await;
    eprintln!("[login-flow] tab đã đóng, session đã lưu");
    Ok(())
}

// Capture session từ Chrome đang login — nút phụ (quét mọi tab)

// Refresh cookies cho toàn bộ account từ Chrome (chạy nền mỗi 5 phút)
async fn auto_refresh_loop(st: Arc<AppState>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        // (cookies, userToken hiện tại của Chrome chính)
        let (cookies, chrome_token) = match crate::chrome_capture::refresh_cookies_from_chrome().await
        {
            Ok((c, t)) if !c.is_empty() => (c, t),
            _ => continue, // Chrome tắt / không có tab — bỏ qua lần này
        };
        let refreshed = {
            let mut store = st.store.write().await;
            let mut refreshed = 0;
            for a in store.accounts.iter_mut() {
                // acc khớp token đang sống trong Chrome → refresh cookies
                if Some(&a.token) == chrome_token.as_ref() {
                    a.cookies = cookies.clone();
                    a.last_verified = Some(now());
                    a.last_status = Some("ok (refreshed)".into());
                    refreshed += 1;
                }
            }
            // token mới (user re-login acc khác bằng tay) mà chưa có trong pool: không đoán bừa
            // acc nào để gán — bỏ qua, chờ user bấm Capture/Login.
            if refreshed == 0 {
                if let Some(new_tok) = &chrome_token {
                    if !store.accounts.iter().any(|a| &a.token == new_tok) {
                        eprintln!("[auto-refresh] token mới trong Chrome chưa có trong pool — bỏ qua (dùng nút Capture nếu muốn thêm)");
                    }
                }
            }
            refreshed
        };
        if refreshed > 0 {
            mark_dirty(&st);
            flush_store(&st).await;
            eprintln!("[auto-refresh] cookies updated from chrome ({} acc)", refreshed);
        }
    }
}

// ---------- helpers ----------

pub(crate) fn cookie_string(cookies: &std::collections::BTreeMap<String, String>) -> String {
    cookies
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Khoá `std::sync::Mutex` chịu được poison: một panic khi đang giữ lock không được phép làm
/// mọi request sau panic theo. Dữ liệu được bảo vệ ở đây là map đếm/cache — luôn dùng được.
pub(crate) trait LockExt<T> {
    fn lk(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn lk(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub(crate) fn into_err(e: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": e.to_string()})),
    )
}

// ---------- router ----------

/// Chặn truy cập: UI và /api/* (quản trị acc, token/cookie) chỉ cho loopback dù server bị bind ra ngoài;
/// /v1/* yêu cầu `Authorization: Bearer <DS_API_KEY>` nếu đặt biến môi trường DS_API_KEY.
async fn access_guard(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    if (path == "/" || path.starts_with("/api/")) && !addr.ip().is_loopback() {
        return (StatusCode::FORBIDDEN, Json(json!({"error": "admin chỉ cho phép từ localhost"}))).into_response();
    }
    if path.starts_with("/v1/") {
        if let Ok(key) = std::env::var("DS_API_KEY") {
            if !key.is_empty() {
                let ok = req.headers().get("authorization").and_then(|v| v.to_str().ok())
                    .map(|v| v == format!("Bearer {}", key)).unwrap_or(false);
                if !ok {
                    return (StatusCode::UNAUTHORIZED, Json(json!({"error": "thiếu hoặc sai API key"}))).into_response();
                }
            }
        }
    }
    let mut resp = next.run(req).await;
    if resp.status() == StatusCode::TOO_MANY_REQUESTS && !resp.headers().contains_key("retry-after") {
        resp.headers_mut().insert("retry-after", axum::http::HeaderValue::from_static("5"));
    }
    resp
}

/// Dọn phiên web nhàn rỗi quá DS_CONV_TTL (mặc định 1800s): xoá trên DS, khỏi registry phiên và khỏi registry hội thoại.
async fn conv_sweeper(st: Arc<AppState>) {
    let ttl: u64 = std::env::var("DS_CONV_TTL").ok().and_then(|v| v.parse().ok()).unwrap_or(1800);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs((ttl / 4).clamp(3, 60))).await;
        let t = now();
        let expired: Vec<(String, String)> = {
            let mut m = st.sessions.lk();
            let ids: Vec<String> = m.iter().filter(|(_, (_, last))| t.saturating_sub(*last) > ttl).map(|(k, _)| k.clone()).collect();
            ids.into_iter().filter_map(|k| m.remove(&k).map(|(acc, _)| (k, acc))).collect()
        };
        if expired.is_empty() { continue; }
        st.convs.lk().retain(|_, c| !expired.iter().any(|(sid, _)| *sid == c.sid));
        for (sid, acc_id) in expired {
            let acc = { st.store.read().await.accounts.iter().find(|a| a.id == acc_id).map(|a| (a.token.clone(), cookie_string(&a.cookies))) };
            if let Some((tok, ck)) = acc {
                if let Ok(cl) = DsClient::new(tok, ck) {
                    match cl.delete_session(&sid).await {
                        Ok(()) => eprintln!("[conv] đã xoá phiên nhàn rỗi {}", &sid[..8.min(sid.len())]),
                        Err(e) => eprintln!("[conv] xoá phiên {} lỗi: {}", sid, e),
                    }
                }
            }
        }
    }
}

pub async fn run(port: u16) -> Result<()> {
    let store = load_store().await;
    let st = Arc::new(AppState {
        store: RwLock::new(store),
        save_lock: tokio::sync::Mutex::new(()),
        store_ver: std::sync::atomic::AtomicU64::new(0),
        store_written: std::sync::atomic::AtomicU64::new(0),
        store_dirty: tokio::sync::Notify::new(),
        in_flight: std::sync::Mutex::new(std::collections::HashMap::new()),
        login_watching: std::sync::Mutex::new(false),
        file_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        convs: std::sync::Mutex::new(std::collections::HashMap::new()),
        sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        stat_conv_hit: std::sync::atomic::AtomicU64::new(0),
        stat_conv_miss: std::sync::atomic::AtomicU64::new(0),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/api/accounts", get(api_list_accounts).post(api_add_account))
        .route("/api/accounts/{id}", axum::routing::delete(api_delete_account))
        .route("/api/accounts/{id}/verify", post(api_verify_account))
        .route("/api/accounts/{id}/pool", post(api_toggle_pool))
        .route("/api/accounts/{id}/reset", post(api_reset_account))
        .route("/api/capture", post(api_capture_from_chrome))
        .route("/api/login_start", post(api_login_start))
        .route("/api/build_profile", post(api_build_profile))
        .route("/api/stats", get(api_stats))
        .route("/v1/chat/completions", post(crate::chat::api_chat_completions))
        .route("/v1/models", get(api_models))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)) // 32MB — prompt dài (256K+ token)
        .layer(axum::middleware::from_fn(access_guard))
        .with_state(st.clone());

    // Background: dọn phiên nhàn rỗi, refresh cookies từ Chrome, ghi store gom lô
    tokio::spawn(conv_sweeper(st.clone()));
    tokio::spawn(store_flusher(st.clone()));
    tokio::spawn(auto_refresh_loop(st));

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    println!("deepseek-harness listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
    Ok(())
}

#[cfg(test)]
mod dsml_tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let text = "Để mình ngó qua repo.\n<｜｜DSML｜｜ calls> <｜｜DSML｜｜ invoke name=\"bash\"> <｜｜DSML｜｜ parameter name=\"command\" string=\"true\">pwd && ls -la</｜｜DSML｜｜ parameter> </｜｜DSML｜｜ invoke> </｜｜DSML｜｜ calls>";
        let (before, calls) = parse_dsml_tool_calls(text).expect("phải parse được");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert!(calls[0].arguments.contains("pwd && ls -la"));
        assert!(before.contains("ngó qua repo"));
        assert!(!before.contains("DSML"));
    }
}

#[cfg(test)]
mod xml_tests {
    use super::*;

    #[test]
    fn parse_dsml_close_tag_with_attrs() {
        // format lỗi thật từ log: close-tag param bị đè attr
        let text = "text\n\n<｜｜DSML｜｜ calls><｜｜DSML｜｜ invoke name=\"bash\"><｜｜DSML｜｜ parameter name=\"command\" string=\"true\">ls -la</｜｜DSML｜｜ parameter name=\"description\" string=\"true\">List</｜｜DSML｜｜ parameter></｜｜DSML｜｜ invoke></｜｜DSML｜｜ calls>";
        let r = parse_dsml_tool_calls(text);
        assert!(r.is_some(), "phải parse được malformed close-tag");
    }


    #[test]
    fn parse_dsml_glued_tags_with_id() {
        // format thật từ log 16:4x: tag dính liền + attr id, không space sau '｜｜'
        let text = "I'll dig in.\n\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜invoke id=\"call_0\" name=\"bash\">\n<｜｜DSML｜｜parameter name=\"command\" string=\"true\">du -sh /data</｜｜DSML｜｜parameter>\n</｜｜DSML｜｜invoke>\n</｜｜DSML｜｜ calls>";
        let (before, calls) = parse_dsml_tool_calls(text).expect("phải parse tag dính liền + id");
        assert_eq!(calls[0].name, "bash");
        assert!(calls[0].arguments.contains("du -sh /data"));
        assert!(!before.contains("DSML"));
    }


    #[test]
    fn safe_flush_holds_only_marker_tail() {
        assert!(has_tool_marker("nháp <｜DSML｜function_calls>") && !has_tool_marker("a < b"));
        assert!(!has_tool_marker("DSML tool-call parsing: normalize_dsml") && has_tool_marker("x <｜｜DSML｜｜ calls>"));
        assert_eq!(safe_flush_len("hello world"), 11);
        assert_eq!(safe_flush_len("a < b"), 2);
        let s = "I'll dig in.\n\n<｜DSML｜function_calls>";
        assert_eq!(&s[..safe_flush_len(s)], "I'll dig in.\n\n");
        let s = "xong. <｜";
        assert_eq!(&s[..safe_flush_len(s)], "xong. ");
        let s = "x\n<｜｜DSML｜｜ calls>";
        assert_eq!(&s[..safe_flush_len(s)], "x\n");
    }

    #[test]
    fn fill_required_description() {
        let tools = Some(vec![json!({"type":"function","function":{"name":"bash","parameters":{
            "required":["command","description"],
            "properties":{"command":{"type":"string"},"description":{"type":"string"}}}}})]);
        let mut c = DsmlToolCall { name: "bash".into(), arguments: "{\"command\":\"ls /data\"}".into() };
        fill_required_args(&tools, &mut c);
        let v: Value = serde_json::from_str(&c.arguments).unwrap();
        assert_eq!(v["description"], "ls /data");
    }

    #[test]
    fn parse_invokes_without_calls_open() {
        let text = "<｜｜DSML｜｜ invoke id=\"call_2\" name=\"read\"><｜｜DSML｜｜ parameter name=\"file_path\" string=\"true\">/a</｜｜DSML｜｜ parameter></｜｜DSML｜｜ invoke><｜｜DSML｜｜ invoke id=\"call_3\" name=\"read\"><｜｜DSML｜｜ parameter name=\"file_path\" string=\"true\">/b</｜｜DSML｜｜ parameter></｜｜DSML｜｜ invoke>";
        let (_, calls) = parse_dsml_tool_calls(text).expect("parse invoke trần");
        assert_eq!(calls.len(), 2);
        assert!(calls[0].arguments.contains("/a") && calls[1].arguments.contains("/b"), "args không được lẫn giữa các invoke");
        // cụt giữa chừng → bỏ invoke dở
        let cut = "<｜｜DSML｜｜ calls><｜｜DSML｜｜ invoke name=\"bash\"><｜｜DSML｜｜ parameter name=\"command\" string=\"true\">ls";
        assert!(parse_dsml_tool_calls(cut).is_none());
    }

    #[test]
    fn parse_invoke_id_as_name_and_plain_parameter() {
        // format thật từ log 16:5x: invoke id="write" (không name=) + <parameter> trơn
        let text = "<｜｜DSML｜｜ calls><｜｜DSML｜｜ invoke id=\"write\"><｜｜DSML｜｜ parameter name=\"file_path\" string=\"true\">/tmp/p.js</｜｜DSML｜｜ parameter>\n<parameter name=\"content\">(() => 1)()\n</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        let (_, calls) = parse_dsml_tool_calls(text).expect("parse id-as-name");
        assert_eq!(calls[0].name, "write");
        assert!(calls[0].arguments.contains("/tmp/p.js") && calls[0].arguments.contains("(() => 1)()"));
    }

    #[tokio::test]
    async fn session_rotation_bookkeeping() {
        let mut a: Account = serde_json::from_str(r#"{"id":"x","label":"x","token":"t","cookies":{},"added_at":0,"last_verified":null,"last_status":null,"thread_session_id":null,"thread_parent_message_id":null}"#).unwrap();
        note_session_use(&mut a, "s1");
        note_session_use(&mut a, "s1");
        assert_eq!((a.thread_session_id.as_deref(), a.thread_uses), (Some("s1"), 2));
        note_session_use(&mut a, "s2"); // xoay vòng: session mới thay thế, đếm lại
        assert_eq!((a.thread_session_id.as_deref(), a.thread_uses), (Some("s2"), 1));
    }

    #[test]
    fn typed_args_and_string_false() {
        let text = "<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"read\">\n<｜｜DSML｜｜ parameter name=\"file_path\" string=\"true\">/a</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"limit\" string=\"false\">50</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        let (_, calls) = parse_any_tool_calls(text).unwrap();
        let v: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(v["limit"], 50);
        // model quên string="false" → ép theo schema
        let tools = Some(vec![json!({"function":{"name":"read","parameters":{"properties":{"file_path":{"type":"string"},"limit":{"type":"integer"}}}}})]);
        let mut c = DsmlToolCall { name: "read".into(), arguments: "{\"file_path\":\"/a\",\"limit\":\"50\"}".into() };
        fill_required_args(&tools, &mut c);
        let v: Value = serde_json::from_str(&c.arguments).unwrap();
        assert_eq!(v["limit"], 50);
    }

    #[test]
    fn history_roundtrip_uses_canonical_format() {
        let msgs = vec![
            json!({"role":"user","content":"hi"}),
            json!({"role":"assistant","content":"ok","tool_calls":[{"id":"call_abc","type":"function","function":{"name":"write","arguments":"{\"file_path\":\"/x\",\"content\":\"a\\nb\",\"n\":3}"}}]}),
            json!({"role":"tool","tool_call_id":"call_abc","content":"done"}),
        ];
        let p = messages_to_prompt(&msgs);
        assert!(p.contains("tool_result (write) [call_abc]: done"));
        assert!(p.contains("a\nb"), "xuống dòng trong tham số phải được giữ");
        // model đọc lại lịch sử của chính nó phải parse được bằng đúng parser
        let (_, calls) = parse_any_tool_calls(&p).expect("lịch sử render phải parse lại được");
        let v: Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(v["content"], "a\nb");
        assert_eq!(v["n"], 3);
        assert!(!p.contains("system_note") && !p.contains("greet"));
    }

    #[test]
    fn call_ids_are_unique() {
        assert_ne!(new_call_id(), new_call_id());
    }

    #[test]
    fn fail_cooldown_escalates_and_resets() {
        let mut st = AccStats::default();
        st.mark_fail(1000); assert_eq!(st.cooldown_until, 1015);
        st.mark_fail(1000); assert_eq!(st.cooldown_until, 1030);
        st.mark_fail(1000); assert_eq!(st.cooldown_until, 1060);
        for _ in 0..20 { st.mark_fail(1000); }
        assert_eq!(st.cooldown_until, 1600); // trần 600s
        st.mark_ok();
        assert_eq!((st.fail_streak, st.cooldown_until), (0, 0));
    }

    #[test]
    fn parse_malformed_real_samples() {
        // 1) mở '<｜DSML｜｜ calls>' 1 gạch + đóng 1 gạch: không được để lọt vào 'before'
        let t1 = "Xong.\n<｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"bash\">\n<｜｜DSML｜｜ parameter name=\"command\" string=\"true\">ls</｜｜DSML｜｜ parameter>\n</｜DSML｜｜ invoke>\n</｜DSML｜｜ calls>";
        let (b, c) = parse_any_tool_calls(t1).unwrap();
        assert_eq!(c[0].name, "bash"); assert_eq!(b, "Xong.");
        // 2) thiếu invoke mở → tên rỗng, suy ra từ schema
        let t2 = "ok\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ parameter name=\"command\" string=\"true\">pwd</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"description\" string=\"true\">cwd</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        let (_, mut c) = parse_any_tool_calls(t2).unwrap();
        let tools = Some(vec![
            json!({"function":{"name":"write","parameters":{"properties":{"file_path":{},"content":{}},"required":["file_path","content"]}}}),
            json!({"function":{"name":"bash","parameters":{"properties":{"command":{},"description":{}},"required":["command","description"]}}})]);
        infer_tool_name(&tools, &mut c[0]);
        assert_eq!(c[0].name, "bash"); assert!(call_is_valid(&tools, &c[0]));
        // 3) thẻ param bị mất 'parameter name='
        let t3 = "<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"write\">\n<｜｜DSML｜｜ file_path\" string=\"true\">/a/.gitignore</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        let (_, c) = parse_any_tool_calls(t3).unwrap();
        assert!(c[0].arguments.contains("/a/.gitignore"));
        assert!(!call_is_valid(&tools, &c[0]), "thiếu content → không hợp lệ, để harness retry");
        // 4) param mở lặp
        let t4 = "<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ parameter name=\"command\">\n<｜｜DSML｜｜ parameter name=\"command\" string=\"true\">echo hi</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        let (_, c) = parse_any_tool_calls(t4).unwrap();
        let v: Value = serde_json::from_str(&c[0].arguments).unwrap();
        assert_eq!(v["command"], "echo hi");
    }

    #[test]
    fn parse_dsml_json_variants() {
        for tag in ["<dsml:tool_call>", "<dsml_tool_call>", "<dsml-tool-call>", "<dsml tool_call>"] {
            let text = format!("pre\n\n{}\n{{\"name\": \"bash\", \"arguments\": {{\"command\": \"ls\", \"description\": \"d\"}}}}\n</{}>", tag, tag.replace("<", "</"));
            let r = parse_dsml_json_tool_calls(&text);
            assert!(r.is_some(), "phải parse được variant {}", tag);
            let (_before, calls) = r.unwrap();
            assert_eq!(calls[0].name, "bash");
            assert!(calls[0].arguments.contains("ls"));
        }
    }

    #[test]
    fn parse_dsml_double_open_and_junk_param() {
        // format thật từ log 11:15: double open-tag + junk param rỗng name="bash"
        let text = "text\n\n<｜｜DSML｜｜ calls><｜｜DSML｜｜ calls><｜｜DSML｜｜ invoke id=\"call_0\" name=\"bash\"><｜｜DSML｜｜ parameter name=\"bash\" string=\"true\"></｜｜DSML｜｜ parameter><｜｜DSML｜｜ parameter name=\"command\" string=\"true\">du -sh /data</｜｜DSML｜｜ parameter><｜｜DSML｜｜ parameter name=\"description\" string=\"true\">Disk usage</｜｜DSML｜｜ parameter></｜｜DSML｜｜ invoke></｜｜DSML｜｜ calls>";
        let r = parse_dsml_tool_calls(text).expect("phải parse double-open + junk param");
        let (_before, calls) = r;
        assert_eq!(calls[0].name, "bash");
        let args: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert!(args.get("command").is_some());
        assert!(args.get("description").is_some());
        assert!(args.get("bash").is_none(), "junk param rỗng phải bị bỏ");
    }

    #[test]
    fn parse_dsml_json_multi_tool_calls() {
        // format thật từ log 11:17: 2 blocks <dsml:tool_call>{json}</dsml:tool_call>
        let text = " run two separate commands.\n\n<dsml:tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"du -h /data\", \"description\": \"du\"}}\n</dsml:tool_call>\n\n<dsml:tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"find . -size +100M -exec ls -lh {} \\;\", \"description\": \"big files\"}}\n</dsml:tool_call>";
        let r = parse_dsml_json_tool_calls(text);
        let r = r.expect("phải parse 2 tool_call JSON");
        assert_eq!(r.1.len(), 2, "phải ra 2 calls");
    }

    #[test]
    fn parse_dsml_multi_json_lines() {
        // format log 11:19: <dsml tool_calls> chứa 3 JSON lines key "tool"
        let text = "x\n\n<dsml tool_calls>\n{\"tool\": \"bash\", \"command\": \"ls /var\", \"description\": \"L\"}\n{\"tool\": \"bash\", \"command\": \"date\", \"description\": \"D\"}\n</dsml>";
        let r = parse_dsml_json_tool_calls(text).expect("phải parse multi-json-lines");
        assert_eq!(r.1.len(), 2);
        let args: serde_json::Value = serde_json::from_str(&r.1[0].arguments).unwrap();
        assert!(args.get("command").is_some());
    }

    #[test]
    fn parse_dsml_multi_name_json_lines() {
        // '<dsml tool_calls>' chứa 2 JSON lines key "name"
        let text = "x\n\n<dsml tool_calls>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"a\", \"description\": \"b\"}}\n{\"name\": \"bash\", \"arguments\": {\"command\": \"c\", \"description\": \"d\"}}\n</dsml tool_calls>";
        let r = parse_dsml_json_tool_calls(text).expect("phải parse");
        assert_eq!(r.1.len(), 2);
    }

    #[test]
    fn parse_nested_bash_self_closing_multi() {
        // format log 11:19: <dsml tool_calls>\n<bash command="X" description="Y" />\n<bash ... />
        let text = "x\n\n<dsml tool_calls>\n<bash command=\"ls /var\" description=\"List /\" />\n<bash command=\"date\" description=\"D\" />\n</dsml tool_calls>";
        let r = parse_dsml_nested_tool_calls(text).expect("phải parse bash self-closing");
        assert!(r.1.len() >= 2, "phải ra 2 calls, got {}", r.1.len());
        let args: serde_json::Value = serde_json::from_str(&r.1[0].arguments).unwrap();
        assert!(args.get("command").is_some() && args.get("description").is_some());
    }

    #[test]
    fn parse_nested_tool_name_parameters() {
        // <dsml tool_call>\n<tool_name>bash</tool_name>\n<parameters>\n<command>C</command>...
        let text = "x\n\n<dsml tool_call>\n<tool_name>bash</tool_name>\n<parameters>\n<command>ls /var</command>\n<description>List</description>\n</parameters>\n</dsml tool_call>";
        let r = parse_dsml_nested_tool_calls(text).expect("phải parse tool_name/parameters");
        assert_eq!(r.1[0].name, "bash");
        let args: serde_json::Value = serde_json::from_str(&r.1[0].arguments).unwrap();
        assert!(args.get("command").is_some());
    }

    #[test]
    fn parse_dsml_json_multi_blocks() {
        // log 11:20: 3 blocks liên tiếp '<dsml_tool_call>{json}</dsml_tool_call>' close-tag ngắn
        let text = "x\n\n<dsml_tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\", \"description\": \"L\"}}\n</dsml_tool_call>\n<dsml_tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"echo\", \"description\": \"E\"}}\n</dsml_tool_call>\n<dsml_tool_call>\n{\"name\": \"bash\", \"arguments\": {\"command\": \"date\", \"description\": \"D\"}}\n</dsml_tool_call>";
        let r = parse_dsml_json_tool_calls(text).expect("phải parse 3 blocks");
        assert_eq!(r.1.len(), 3, "got {}", r.1.len());
    }

    #[test]
    fn parse_dsml_bare_json_no_name_key() {
        // log 11:20: JSON bare {command, description} không có name key, close-tag '</dsml>'
        let text = "x\n\n<dsml-tool-call>\n{\"command\": \"ls /var\", \"description\": \"L\"}\n</dsml-tool-call>\n\n<dsml-tool-call>\n{\"command\": \"date\", \"description\": \"D\"}\n</dsml-tool-call>";
        let r = parse_dsml_json_tool_calls(text).expect("phải parse bare json");
        assert_eq!(r.1.len(), 2);
        let args: serde_json::Value = serde_json::from_str(&r.1[0].arguments).unwrap();
        assert_eq!(args["command"], "ls /var");
}
    #[test]
    fn parse_nested_bash_no_attr_child_tags() {
        // log 11:24: <dsml:bash>\n<dsml:command>C</dsml:command>\n<dsml:description>D</dsml:description>\n</dsml:bash>
        let text = "x\n\n<dsml:bash>\n<dsml:command>ls /var</dsml:command>\n<dsml:description>List</dsml:description>\n</dsml:bash>";
        let r = parse_dsml_nested_tool_calls(text).expect("phải parse case D");
        assert_eq!(r.1[0].name, "bash");
        let args: serde_json::Value = serde_json::from_str(&r.1[0].arguments).unwrap();
        assert_eq!(args["command"], "ls /var");
        assert!(args.get("description").is_some());
    }

    #[test]
    fn parse_xml_bash() {
        let text = "Để mình xem lại repo trước đã.\n\n<bash> <command>pwd && ls -la</command> <description>List workspace contents</description> </bash>";
        let (before, call) = parse_xml_tool_call(text).expect("phải parse được xml tool call");
        assert_eq!(call.name, "bash");
        assert!(call.arguments.contains("pwd && ls -la"));
        assert!(before.contains("repo"));
    }
}
