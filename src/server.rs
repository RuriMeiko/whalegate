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
    std::env::var("DS_ACCOUNTS_PATH").unwrap_or_else(|_| format!("{}/accounts.json", data_dir()))
}

/// Thư mục dữ liệu ~/.whalegate. Lần đầu chạy sau khi đổi tên: chuyển nguyên ~/.ds-harness sang (account, cài đặt,
/// usage, profile Chrome login) và để lại symlink tên cũ cho script/công cụ ngoài còn trỏ vào đó.
pub(crate) fn data_dir() -> String {
    static DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    // test không được đụng (hay migrate) dữ liệu thật của người dùng
    #[cfg(test)]
    return std::env::temp_dir().join(format!("whalegate-test-{}", std::process::id())).to_string_lossy().into_owned();
    #[allow(unreachable_code)]
    DIR.get_or_init(|| {
        let home = std::path::PathBuf::from(home_dir());
        let (new, old) = (home.join(".whalegate"), home.join(".ds-harness"));
        let old_is_real_dir = std::fs::symlink_metadata(&old).map(|m| m.is_dir()).unwrap_or(false);
        if !new.exists() && old_is_real_dir {
            match std::fs::rename(&old, &new) {
                Ok(()) => {
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(&new, &old);
                    eprintln!("[data] moved {} → {}", old.display(), new.display());
                }
                Err(e) => {
                    eprintln!("[data] could not move {} ({}), keep using it", old.display(), e);
                    return old.to_string_lossy().into_owned();
                }
            }
        }
        new.to_string_lossy().into_owned()
    }).clone()
}

/// Thư mục home: HOME (Linux/macOS) hoặc USERPROFILE (Windows); không có thì thư mục tạm của hệ điều hành.
pub(crate) fn home_dir() -> String {
    std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned())
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
    /// id người dùng DeepSeek (từ users/current) — nhận ra cùng một người dù token khác nhau
    #[serde(default)]
    pub ds_uid: Option<String>,
    /// seed vân tay trình duyệt của acc (None = acc cũ, vân tay dẫn xuất từ token)
    #[serde(default)]
    pub fp_seed: Option<String>,
    /// device id thật của Chrome riêng của acc (x-device-id); None = dẫn xuất từ seed
    #[serde(default)]
    pub device_id: Option<String>,
    /// lần cuối lấy cookie thiết bị từ Chrome riêng của acc (unix giây)
    #[serde(default)]
    pub device_refreshed_at: Option<u64>,
    /// hồ sơ trình duyệt THẬT của acc thêm bằng bookmarklet (UA, client hints, màn hình, múi giờ, GPU…).
    /// Có profile = acc dùng đúng "máy" của người dùng, KHÔNG mở Chrome riêng, KHÔNG tự sinh vân tay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<crate::fingerprint::BrowserProfile>,
    /// DeepSeek huỷ token (40002/40003) lúc nào. Web không có luồng refresh token: token chết là chết hẳn, nên acc
    /// đứng ngoài lịch chọn tới khi có token mới (đăng nhập lại, bookmarklet, sửa token) hoặc verify thấy sống lại.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_dead_at: Option<u64>,
}

impl Account {
    pub(crate) fn set_auth_dead(&mut self) {
        if self.auth_dead_at.is_none() { self.auth_dead_at = Some(now()); }
        self.last_status = Some("DeepSeek đã đăng xuất token này (40003) — cần đăng nhập lại".into());
    }
    pub(crate) fn clear_auth_dead(&mut self) { self.auth_dead_at = None; }
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
    // số lượt trong giờ hiện tại (cửa sổ 3600s) — chạm DS_HOURLY_LIMIT thì acc nghỉ tới hết giờ
    #[serde(default)]
    pub hour_window_start: u64,
    #[serde(default)]
    pub hour_count: u32,
}

impl AccStats {
    // Tunable qua env: DS_BURST_LIMIT, DS_BURST_WINDOW, DS_COOLDOWN
    pub fn burst_limit() -> u32 { crate::settings::uint("DS_BURST_LIMIT") as u32 }
    pub fn burst_window() -> u64 { crate::settings::uint("DS_BURST_WINDOW") }
    pub fn cooldown() -> u64 { crate::settings::uint("DS_COOLDOWN") }
    // CAP concurrent call trên 1 acc (DS_MAX_CONC_PER_ACC, mặc định 2).
    // Cộng đồng report ban khi >2 call đồng thời/acc — 2 là ngưỡng an toàn.
    pub fn max_conc() -> u32 { crate::settings::uint("DS_MAX_CONC_PER_ACC") as u32 }

    fn burst_active(&self, now: u64) -> bool {
        now.saturating_sub(self.burst_window_start) < Self::burst_window()
            && self.burst_count >= Self::burst_limit()
    }

    // Trần lượt/giờ mỗi acc (DS_HOURLY_LIMIT, 0 = không giới hạn): dùng dồn dập liên tục là kiểu dễ bị khoá acc
    pub fn hourly_limit() -> u32 { crate::settings::uint("DS_HOURLY_LIMIT") as u32 }

    fn hourly_capped(&self, now: u64) -> bool {
        let lim = Self::hourly_limit();
        lim > 0 && now.saturating_sub(self.hour_window_start) < 3600 && self.hour_count >= lim
    }

    pub(crate) fn available(&self, now: u64) -> bool {
        now >= self.cooldown_until && !self.burst_active(now) && !self.hourly_capped(now)
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
        let max: u64 = crate::settings::uint("DS_FAIL_COOLDOWN_MAX");
        let cd = (15u64 << (self.fail_streak - 1).min(10)).min(max);
        self.cooldown_until = now + cd;
    }

    pub(crate) fn record_call(&mut self, now: u64) {
        if now.saturating_sub(self.burst_window_start) >= Self::burst_window() {
            self.burst_window_start = now;
            self.burst_count = 0;
        }
        self.burst_count += 1;
        if now.saturating_sub(self.hour_window_start) >= 3600 {
            self.hour_window_start = now;
            self.hour_count = 0;
        }
        self.hour_count += 1;
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
            crate::elog!("[store] không đọc được {}: {} — dùng store rỗng", path, e);
            return Store::default();
        }
    };
    match serde_json::from_str::<Store>(&raw) {
        Ok(s) => {
            for a in &s.accounts {
                crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
                crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
                crate::fingerprint::bind_profile(&a.token, a.browser.clone());
            }
            s
        }
        Err(e) => {
            let bak = format!("{}.corrupt-{}", path, now());
            crate::elog!("[store] {} hỏng ({}): giữ bản sao tại {} — dùng store rỗng", path, e, bak);
            if let Err(e2) = tokio::fs::rename(&path, &bak).await {
                crate::elog!("[store] không đổi tên được file hỏng: {}", e2);
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
        Err(e) => crate::elog!("[store] ghi accounts.json lỗi: {:#}", e),
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

// ---------- khởi động lại ----------

static RESTART_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RESTART_SINCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn boot_id() -> u64 {
    static B: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *B.get_or_init(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

fn total_in_flight(st: &AppState) -> u32 {
    st.in_flight.lk().values().map(|v| v.load(std::sync::atomic::Ordering::Acquire)).sum()
}

#[derive(Deserialize)]
struct RestartReq { mode: String }

/// mode: "idle" = chờ đến lúc không còn request nào rồi khởi động lại; "now" = hard reset ngay; "cancel" = huỷ lệnh chờ.
async fn api_restart(State(st): State<Arc<AppState>>, Json(r): Json<RestartReq>) -> (StatusCode, Json<Value>) {
    use std::sync::atomic::Ordering::SeqCst;
    match r.mode.as_str() {
        "idle" => {
            if !RESTART_PENDING.swap(true, SeqCst) {
                RESTART_SINCE.store(now(), SeqCst);
                tokio::spawn(restart_when_idle(st));
            }
        }
        "now" => {
            RESTART_PENDING.store(true, SeqCst);
            tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_millis(300)).await; do_restart(st).await; });
        }
        "cancel" => RESTART_PENDING.store(false, SeqCst),
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "mode phải là idle | now | cancel"}))),
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

async fn api_dsh_status() -> Json<Value> {
    Json(crate::setup_dsh::status())
}

#[derive(Deserialize, Default)]
struct DshReq {
    preset: Option<String>,
    /// Tên các profile cần cấu hình (desktop, web, headless, ...). Bỏ trống = tất cả profile đang có.
    profiles: Option<Vec<String>>,
    /// true = cấu hình mọi profile tìm thấy, kể cả khi profiles có giá trị.
    all: Option<bool>,
}

/// Nút "Cấu hình dsh": ghi provider whalegate vào các profile dsh của máy đang chạy gateway.
/// Mặc định ghi TẤT CẢ profile đang có — trên Windows/macOS người dùng Desktop app dùng profile
/// "desktop", không phải "web", nên chỉ ghi web là họ mở app lên vẫn không thấy gateway.
async fn api_dsh_setup(body: Option<Json<DshReq>>) -> Json<Value> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let mut o = crate::setup_dsh::Opts::defaults();
    if let Some(p) = req.preset.filter(|p| matches!(p.as_str(), "ptc" | "standard")) { o.preset = p; }

    let all = crate::setup_dsh::list_profiles();
    let wanted: Vec<std::path::PathBuf> = if req.all == Some(true) || req.profiles.is_none() {
        all.iter().map(|(_, d, _)| d.clone()).collect()
    } else {
        let names = req.profiles.unwrap_or_default();
        names.iter().filter_map(|n| {
            // chặn path traversal: chỉ nhận tên profile đơn giản
            if n.is_empty() || n.contains('/') || n.contains('\\') || n == ".." { return None; }
            all.iter().find(|(name, _, _)| name == n).map(|(_, d, _)| d.clone())
        }).collect()
    };
    if wanted.is_empty() {
        return Json(json!({
            "ok": false,
            "error": format!("không thấy profile dsh nào trong {} — mở dsh (hoặc app Desktop) một lần để nó tạo profile", crate::setup_dsh::profiles_root().display()),
        }));
    }

    let results = tokio::task::spawn_blocking(move || crate::setup_dsh::configure_many(&wanted, &o)).await;
    match results {
        Ok(list) => {
            let mut ok = Vec::new();
            let mut failed = Vec::new();
            for (path, res) in list {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                match res {
                    Ok(r) => {
                        crate::elog!("[dsh] cấu hình {} (backup: {:?})", r.file.display(), r.backup);
                        ok.push(json!({"profile": name, "file": r.file.to_string_lossy(), "changed": r.backup.is_some() || r.created}));
                    }
                    Err(e) => failed.push(json!({"profile": name, "error": format!("{:#}", e)})),
                }
            }
            let err = if ok.is_empty() {
                Some(failed.first().and_then(|f| f["error"].as_str()).unwrap_or("không cấu hình được profile nào").to_string())
            } else { None };
            Json(json!({
                "ok": !ok.is_empty(),
                "configured": ok,
                "failed": failed,
                "error": err,
            }))
        }
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

async fn api_restart_status(State(st): State<Arc<AppState>>) -> Json<Value> {
    use std::sync::atomic::Ordering::SeqCst;
    Json(json!({"pending": RESTART_PENDING.load(SeqCst), "since": RESTART_SINCE.load(SeqCst), "in_flight": total_in_flight(&st), "boot": boot_id()}))
}

async fn restart_when_idle(st: Arc<AppState>) {
    use std::sync::atomic::Ordering::SeqCst;
    crate::elog!("[restart] chờ hết request rồi khởi động lại");
    let mut quiet = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !RESTART_PENDING.load(SeqCst) { crate::elog!("[restart] đã huỷ"); return; }
        if total_in_flight(&st) == 0 { quiet += 1; if quiet >= 4 { break; } } else { quiet = 0; }
    }
    do_restart(st).await;
}

/// Ghi mọi thứ xuống đĩa, xoá các phiên web đang giữ (không để mồ côi trên DS) rồi exec lại chính binary này
/// (cùng PID nên systemd không thấy gì; cũng nạp luôn bản build mới nếu có). Lỗi exec → thoát, systemd khởi động lại.
pub(crate) async fn do_restart(st: Arc<AppState>) {
    crate::elog!("[restart] đang khởi động lại…");
    flush_store(&st).await;
    let _ = tokio::task::spawn_blocking(crate::usage::flush_blocking).await;
    let sess: Vec<(String, String)> = st.sessions.lk().iter().map(|(sid, (acc, _))| (sid.clone(), acc.clone())).collect();
    let mut jobs = Vec::new();
    for (sid, acc_id) in sess {
        let acc = { st.store.read().await.accounts.iter().find(|a| a.id == acc_id).map(|a| (a.token.clone(), cookie_string(&a.cookies))) };
        if let Some((tok, ck)) = acc {
            jobs.push(tokio::spawn(async move { if let Ok(c) = DsClient::new(tok, ck) { let _ = c.delete_session(&sid).await; } }));
        }
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(4), futures::future::join_all(jobs)).await;
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().trim_end_matches(" (deleted)").to_string()).unwrap_or_else(|_| "whalegate".into());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(exe).args(std::env::args().skip(1)).exec();
        crate::elog!("[restart] exec lỗi: {err} — thoát để systemd khởi động lại");
        std::process::exit(1);
    }
    // Windows: file .exe đang chạy bị khoá, nên nếu có script cập nhật thì giao cho nó —
    // nó chờ tiến trình này thoát, move /y binary mới đè lên rồi chạy lại.
    #[cfg(not(unix))]
    {
        let exe_path = std::path::PathBuf::from(&exe);
        let bat = exe_path.with_extension("update.bat");
        let staged = exe_path.with_extension("exe.new");
        // Chỉ giao cho script khi cả hai file còn đó: nếu user đã chạy script thủ công (hoặc một
        // lần update cũ bị bỏ dở), .bat có thể còn nhưng .exe.new đã mất — chạy nó sẽ lặp vô hạn.
        let launched = if bat.is_file() && staged.is_file() {
            crate::elog!("[restart] chạy {} để thay binary", bat.display());
            std::process::Command::new("cmd").arg("/c").arg(&bat).spawn().is_ok()
        } else {
            let _ = std::fs::remove_file(&bat);
            false
        };
        std::process::exit(0);
    }
}


async fn api_settings_get() -> Json<Value> {
    Json(crate::settings::snapshot())
}

/// Body: {"KEY": "value"} (nhiều khoá một lần) — value null hoặc "" = bỏ override, quay về env/mặc định.
async fn api_settings_set(Json(body): Json<std::collections::BTreeMap<String, Value>>) -> (StatusCode, Json<Value>) {
    for (k, v) in &body {
        let r = match v {
            Value::Null => crate::settings::set(k, None),
            Value::String(s) if s.is_empty() => crate::settings::set(k, None),
            Value::String(s) => crate::settings::set(k, Some(s)),
            Value::Bool(x) => crate::settings::set(k, Some(if *x { "1" } else { "0" })),
            Value::Number(x) => crate::settings::set(k, Some(&x.to_string())),
            _ => Err(format!("{k}: giá trị không hợp lệ")),
        };
        if let Err(e) = r { return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": e}))); }
    }
    (StatusCode::OK, Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct LogsQ { after: Option<u64>, limit: Option<usize> }

async fn api_logs(axum::extract::Query(q): axum::extract::Query<LogsQ>) -> Json<Value> {
    let lines = crate::logbuf::since(q.after.unwrap_or(0), q.limit.unwrap_or(500).min(3000));
    Json(json!({ "lines": lines }))
}

/// SSE: gửi backlog (`?backlog=N`, mặc định 300) rồi phát trực tiếp từng dòng mới.
async fn api_logs_stream(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>)
    -> axum::response::sse::Sse<impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let backlog: usize = q.get("backlog").and_then(|v| v.parse().ok()).unwrap_or(300).min(3000);
    let mut rx = crate::logbuf::subscribe(); // đăng ký trước khi lấy backlog để không hụt dòng
    let first = crate::logbuf::since(0, backlog);
    let mut last = first.last().map(|l| l.seq).unwrap_or(0);
    let stream = async_stream::stream! {
        for l in first { yield Ok(Event::default().data(serde_json::to_string(&l).unwrap_or_default())); }
        loop {
            match rx.recv().await {
                Ok(l) => {
                    if l.seq <= last { continue; }
                    last = l.seq;
                    yield Ok(Event::default().data(serde_json::to_string(&l).unwrap_or_default()));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // người xem quá chậm: nạp lại từ vòng đệm
                    for l in crate::logbuf::since(last, 3000) { last = l.seq; yield Ok(Event::default().data(serde_json::to_string(&l).unwrap_or_default())); }
                }
                Err(_) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
}

async fn api_usage() -> Json<Value> {
    Json(tokio::task::spawn_blocking(crate::usage::report).await.unwrap_or_else(|_| json!({})))
}

async fn index() -> Html<&'static str> {
    Html(index_html())
}

/// ui.html kèm phiên bản đang chạy (thay `__WG_VERSION__` một lần, giữ lại cho mọi lần tải trang).
fn index_html() -> &'static str {
    static HTML: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HTML.get_or_init(|| include_str!("ui.html").replace("__WG_VERSION__", env!("CARGO_PKG_VERSION")))
}

#[derive(Deserialize)]
struct AddAccount {
    #[serde(default)]
    label: String,
    token: String,
    #[serde(default)]
    cookies: std::collections::BTreeMap<String, String>,
    /// Gói "Thêm vào whalegate" từ bookmarklet: vân tay thật của trình duyệt đang đăng nhập DS.
    /// Có gói này thì acc dùng đúng máy đó; không có thì như acc dán token cũ (vân tay dẫn xuất từ token).
    #[serde(default)]
    browser: Option<crate::fingerprint::BrowserProfile>,
}

/// Nhận token dán tay từ `localStorage.userToken`: JSON `{"value":"…","__version":"0"}`, chuỗi có nháy, `Bearer …` hoặc token trần.
pub(crate) fn normalize_token(raw: &str) -> String {
    let mut t = raw.trim().to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&t) {
        match v {
            Value::Object(o) => if let Some(x) = o.get("value").and_then(|x| x.as_str()) { t = x.to_string(); },
            Value::String(x) => t = x,
            _ => {}
        }
    }
    let t = t.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`').trim();
    t.strip_prefix("Bearer ").unwrap_or(t).trim().to_string()
}

/// Thêm acc hỏng giữa chừng: token đang thuộc acc nào trong pool thì gắn lại đúng vân tay của acc đó,
/// không thì gỡ hẳn. Client đã dựng theo vân tay tạm cũng bỏ để lần sau dựng lại cho đúng.
fn restore_binding(store: &Store, token: &str) {
    crate::fingerprint::unbind(token);
    if let Some(a) = store.accounts.iter().find(|a| a.token == token) {
        crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
        crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
        crate::fingerprint::bind_profile(&a.token, a.browser.clone());
    }
    crate::client::forget(token);
}

async fn api_add_account(
    State(st): State<Arc<AppState>>,
    Json(mut p): Json<AddAccount>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    p.token = normalize_token(&p.token);
    if p.token.len() < 20 || p.token.chars().any(|c| c.is_whitespace()) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": "không đọc được token — dán đúng giá trị localStorage.userToken"}))));
    }
    // Token đã có trong pool = bấm bookmarklet / dán lại để làm mới acc đó (cookie, thiết bị, gỡ "đã đăng xuất"),
    // không phải lỗi trùng. Cùng người dùng mà token khác (đăng nhập lại) thì thay token vào acc cũ — xem dưới.
    let (existing_seed, existing_sid) = st.store.read().await.accounts.iter().find(|a| a.token == p.token)
        .map(|a| (a.fp_seed.clone(), a.cookies.get("ds_session_id").cloned())).unzip();
    // acc đã có ds_session_id mà gói mới (bookmarklet) không đọc được nó: giữ cái cũ, đừng xin cái mới
    if let (false, Some(Some(sid))) = (p.cookies.contains_key("ds_session_id"), existing_sid) {
        p.cookies.insert("ds_session_id".into(), sid);
    }
    // Có hồ sơ trình duyệt thật (bookmarklet) → gắn TRƯỚC khi tạo client, để ngay request verify cũng đi
    // bằng đúng TLS/UA/device id của máy user. Seed lấy từ gói; thiếu thì giữ seed của acc cũ, không có nữa thì sinh mới.
    let mut browser = p.browser.take();
    let mut seed_generated = false;
    if let Some(b) = browser.as_mut() {
        if b.seed.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
            b.seed = existing_seed.clone().flatten();
            if b.seed.is_none() { b.seed = Some(crate::fingerprint::new_seed()); seed_generated = true; }
        }
        crate::fingerprint::bind_profile(&p.token, Some(b.clone()));
        crate::fingerprint::bind(&p.token, b.seed.as_deref());
        crate::fingerprint::bind_device(&p.token, b.device_id.as_deref());
        crate::client::forget(&p.token);
    }
    // verify token trước khi lưu; hỏng thì trả registry vân tay + client về như trước khi gắn
    let verified = async {
        let c = DsClient::new(p.token.clone(), cookie_string(&p.cookies)).map_err(into_err)?;
        let v = c.users_current().await.map_err(into_err)?;
        if v["code"].as_i64() != Some(0) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("token không hợp lệ: {}", v["msg"].as_str().unwrap_or("?"))})),
            ));
        }
        Ok((v, c))
    }.await;
    let (v, c) = match verified {
        Ok(x) => x,
        Err(e) => { restore_binding(&*st.store.read().await, &p.token); return Err(e); }
    };
    // bookmarklet / dán token không có ds_session_id (HttpOnly): lấy như trình duyệt lấy, qua chính client của acc
    if !p.cookies.contains_key("ds_session_id") {
        match c.ensure_session_cookie().await {
            Ok(sid) => { p.cookies.insert("ds_session_id".into(), sid); }
            Err(e) => crate::elog!("[account] chưa lấy được ds_session_id: {:#}", e),
        }
    }
    let email = v["data"]["biz_data"]["email"].as_str().unwrap_or("?").to_string();
    let uid = v["data"]["biz_data"]["id"].as_str().map(|s| s.to_string());

    let mut store = st.store.write().await;
    // acc cũ: cùng token, hoặc cùng người dùng DeepSeek (đăng nhập lại → token mới)
    let idx = store.accounts.iter().position(|a| a.token == p.token)
        .or_else(|| uid.as_ref().and_then(|u| store.accounts.iter().position(|a| a.ds_uid.as_deref() == Some(u.as_str()))));
    if let Some(i) = idx {
        let a = &mut store.accounts[i];
        // seed vừa sinh tạm cho lần verify: acc bookmarklet cũ đã có seed thì giữ seed đó, nhiễu canvas/audio khỏi đổi
        if let (true, Some(b)) = (seed_generated, browser.as_mut()) {
            if let Some(old) = a.browser.as_ref().and_then(|ob| ob.seed.clone()) { b.seed = Some(old); }
        }
        let old_token = a.token.clone();
        let token_changed = apply_readd(a, p.token.clone(), p.cookies, browser, uid, p.label.trim());
        // registry vân tay: token cũ bỏ, token mới gắn theo đúng acc (kể cả khi lần verify đã gắn tạm thứ khác)
        if token_changed { crate::fingerprint::unbind(&old_token); crate::client::forget(&old_token); }
        crate::fingerprint::unbind(&a.token);
        crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
        crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
        crate::fingerprint::bind_profile(&a.token, a.browser.clone());
        crate::client::forget(&a.token);
        let (id, label) = (a.id.clone(), a.label.clone());
        crate::elog!("[account] cập nhật {}{}", label, if token_changed { " (token mới)" } else { "" });
        drop(store);
        mark_dirty(&st);
        flush_store(&st).await;
        return Ok(Json(json!({"ok": true, "id": id, "label": label, "updated": true, "token_changed": token_changed})));
    }
    let label_out = if p.label.trim().is_empty() { email } else { p.label.trim().to_string() };
    let acc = Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: label_out.clone(),
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
        ds_uid: uid.clone(),
        fp_seed: browser.as_ref().and_then(|b| b.seed.clone()),
        device_id: browser.as_ref().and_then(|b| b.device_id.clone()),
        device_refreshed_at: None,
        browser,
        auth_dead_at: None,
    };
    let id = acc.id.clone();
    store.accounts.push(acc);
    drop(store);
    mark_dirty(&st);
    flush_store(&st).await;
    Ok(Json(json!({"ok": true, "id": id, "label": label_out, "updated": false})))
}

/// Gộp một lần thêm lại vào acc đã có. Trả true nếu token đổi.
/// - token đổi (đăng nhập lại): thay token, bỏ phiên web cũ, cookie thay hẳn (cookie phiên cũ không thuộc token mới);
///   vân tay giữ nguyên trừ khi gói mang hồ sơ trình duyệt mới.
/// - cùng token: cookie gộp (bookmarklet không đọc được cookie httpOnly, giữ những cái jar đã có).
/// - có hồ sơ trình duyệt: token đến từ chính trình duyệt đó, nên "máy" của acc từ giờ là trình duyệt đó.
fn apply_readd(
    a: &mut Account,
    token: String,
    cookies: std::collections::BTreeMap<String, String>,
    browser: Option<crate::fingerprint::BrowserProfile>,
    uid: Option<String>,
    label: &str,
) -> bool {
    let token_changed = a.token != token;
    if token_changed {
        if a.fp_seed.is_none() { a.fp_seed = Some(crate::fingerprint::seed_for(&a.token)); }
        a.token = token;
        a.thread_session_id = None;
        a.thread_parent_message_id = None;
        a.thread_uses = 0;
        if !cookies.is_empty() { a.cookies = cookies; }
    } else {
        a.cookies.extend(cookies);
    }
    if let Some(b) = browser {
        a.fp_seed = b.seed.clone();
        a.device_id = b.device_id.clone();
        a.device_refreshed_at = None;
        a.browser = Some(b);
    }
    if !label.is_empty() { a.label = label.to_string(); }
    if uid.is_some() { a.ds_uid = uid; }
    a.clear_auth_dead();
    a.stats.reset();
    a.last_verified = Some(now());
    a.last_status = Some(if token_changed { "ok (token mới)" } else { "ok (cập nhật)" }.into());
    token_changed
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
                "uid": a.ds_uid,
                "device": a.fp_seed.is_some() && a.browser.is_none(),
                "browser": a.browser.is_some(),
                "device_refreshed_at": a.device_refreshed_at,
                "auth_dead_at": a.auth_dead_at,
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

#[derive(Deserialize)]
struct EditAccount {
    label: Option<String>,
    token: Option<String>,
}

/// Sửa tài khoản đã có: đổi nhãn và/hoặc thay token (kiểm tra token mới với DeepSeek, không trùng tài khoản khác).
/// Đổi token thì xoá lỗi/cooldown và mọi phiên web gắn với tài khoản này.
async fn api_edit_account(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(p): Json<EditAccount>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let bad = |code: StatusCode, msg: &str| Err((code, Json(json!({"error": msg}))));
    let new_token = p.token.as_deref().map(normalize_token).filter(|t| !t.is_empty());
    let new_label = p.label.as_deref().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    if new_token.is_none() && new_label.is_none() {
        return bad(StatusCode::BAD_REQUEST, "không có gì để sửa");
    }
    let (old_token, cookies) = {
        let store = st.store.read().await;
        let Some(a) = store.accounts.iter().find(|a| a.id == id) else { return bad(StatusCode::NOT_FOUND, "không tìm thấy tài khoản"); };
        (a.token.clone(), a.cookies.clone())
    };
    let mut verified_email = None;
    let mut verified_uid: Option<String> = None;
    if let Some(t) = new_token.as_ref().filter(|t| **t != old_token) {
        if t.len() < 20 || t.chars().any(|c| c.is_whitespace()) {
            return bad(StatusCode::BAD_REQUEST, "không đọc được token — dán đúng giá trị localStorage.userToken");
        }
        let c = DsClient::new(t.clone(), cookie_string(&cookies)).map_err(into_err)?;
        let v = c.users_current().await.map_err(into_err)?;
        if v["code"].as_i64() != Some(0) {
            return bad(StatusCode::BAD_REQUEST, &format!("token không hợp lệ: {}", v["msg"].as_str().unwrap_or("?")));
        }
        verified_email = v["data"]["biz_data"]["email"].as_str().map(|e| e.to_string());
        verified_uid = v["data"]["biz_data"]["id"].as_str().map(|e| e.to_string());
    }
    let old_session = {
        let mut store = st.store.write().await;
        if let Some(t) = new_token.as_ref() {
            if store.accounts.iter().any(|a| a.id != id && a.token == *t) {
                return bad(StatusCode::CONFLICT, "token này đã thuộc tài khoản khác trong pool");
            }
            if let Some(u) = verified_uid.as_ref() {
                if let Some(d) = store.accounts.iter().find(|a| a.id != id && a.ds_uid.as_deref() == Some(u.as_str())) {
                    return bad(StatusCode::CONFLICT, &format!("token này thuộc người dùng đã có trong pool ({})", d.label));
                }
            }
        }
        let Some(a) = store.accounts.iter_mut().find(|a| a.id == id) else { return bad(StatusCode::NOT_FOUND, "không tìm thấy tài khoản"); };
        let mut old_session = None;
        if let Some(l) = new_label.clone() { a.label = l; }
        if let Some(t) = new_token.clone().filter(|t| *t != a.token) {
            old_session = a.thread_session_id.take().map(|sid| (a.token.clone(), sid));
            // đổi token vẫn giữ nguyên vân tay: chốt seed vào acc (acc cũ: seed = token cũ) rồi gắn sang token mới
            if a.fp_seed.is_none() { a.fp_seed = Some(crate::fingerprint::seed_for(&a.token)); }
            a.token = t;
            a.clear_auth_dead();
            crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
            crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
            // acc thêm bằng bookmarklet: giữ đúng "máy" của trình duyệt gốc cho token mới
            crate::fingerprint::bind_profile(&a.token, a.browser.clone());
            if verified_uid.is_some() { a.ds_uid = verified_uid.clone(); }
            a.thread_uses = 0;
            a.thread_parent_message_id = None;
            a.stats.reset();
            a.last_verified = Some(now());
            a.last_status = Some("ok".into());
            if new_label.is_none() { if let Some(e) = verified_email.clone() { a.label = e; } }
        }
        old_session
    };
    if new_token.is_some_and(|t| t != old_token) {
        // phiên web cũ thuộc token cũ: bỏ khỏi sổ và xoá nếu còn được
        st.convs.lk().retain(|_, c| c.acc_id != id);
        st.sessions.lk().retain(|_, (acc, _)| *acc != id);
        st.file_cache.lk().retain(|(acc_id, _), _| acc_id != &id);
        if let Some((tok, sid)) = old_session {
            tokio::spawn(async move { if let Ok(c) = DsClient::new(tok, String::new()) { let _ = c.delete_session(&sid).await; } });
        }
    }
    mark_dirty(&st);
    flush_store(&st).await;
    let label = st.store.read().await.accounts.iter().find(|a| a.id == id).map(|a| a.label.clone()).unwrap_or_default();
    Ok(Json(json!({"ok": true, "label": label})))
}

async fn api_delete_account(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let (removed, profile_seed) = {
        let mut store = st.store.write().await;
        let before = store.accounts.len();
        let seed = store.accounts.iter().find(|a| a.id == id).and_then(|a| a.fp_seed.clone());
        store.accounts.retain(|a| a.id != id);
        (before != store.accounts.len(), seed)
    };
    if removed {
        // profile Chrome riêng của acc (hàng trăm MB) chỉ phục vụ acc này — xoá cùng lúc
        if let Some(seed) = profile_seed {
            let dir = crate::chrome_capture::account_profile_dir(&seed);
            tokio::task::spawn_blocking(move || { let _ = std::fs::remove_dir_all(dir); });
        }
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
    // Reset cũng phải chứng minh chat được, không chỉ token hợp lệ.
    let probe = match DsClient::new(token, cookies) {
        Ok(c) => {
            if let Some(sid) = old_sid { let _ = c.delete_session(&sid).await; }
            c.probe_chat().await
        }
        Err(_) => crate::client::Probe::Unknown,
    };
    let alive = probe == crate::client::Probe::Alive;
    {
        let mut store = st.store.write().await;
        if let Some(a) = store.accounts.iter_mut().find(|a| a.id == id) {
            a.last_verified = Some(now());
            a.last_status = Some(match probe {
                crate::client::Probe::Alive => "ok (reset)".into(),
                crate::client::Probe::Throttled => "throttled".into(),
                crate::client::Probe::AuthDead => "invalid token — cần login lại".into(),
                crate::client::Probe::Unknown => "reset: không kết luận được".into(),
            });
            match probe {
                crate::client::Probe::Alive => a.clear_auth_dead(),
                crate::client::Probe::AuthDead => a.set_auth_dead(),
                _ => {}
            }
        }
    }
    mark_dirty(&st);
    flush_store(&st).await;
    Json(json!({"ok": true, "token_alive": alive, "probe": probe.as_str()}))
}

/// Verify = auth + MỘT lượt chat thật. /users/current một mình không đủ: acc bị throttle vẫn
/// trả code 0 ở đó, nên verify kiểu cũ báo "ok" cho acc không chat được và xoá mất nhãn
/// "throttled" mà lần chat lỗi trước đã ghi.
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
    // Tra cứu user trước để lấy uid/email kể cả khi probe chat lỗi mạng.
    let users = c.users_current().await.unwrap_or_else(|_| json!({"code": -1}));
    let probe = c.probe_chat().await;
    let alive = probe == crate::client::Probe::Alive;
    {
        let mut store = st.store.write().await;
        if let Some(a) = store.accounts.iter_mut().find(|a| a.id == id) {
            a.last_verified = Some(now());
            match probe {
                crate::client::Probe::Alive => {
                    a.last_status = Some("ok".into());
                    // chat được thì hết cooldown: acc đã tự hồi
                    a.stats.reset();
                    a.clear_auth_dead();
                }
                crate::client::Probe::Throttled => {
                    // Giữ nguyên nhãn throttle, chỉ đẩy cooldown ra để scheduler né acc này.
                    a.last_status = Some("throttled".into());
                    a.stats.cooldown_until = now() + AccStats::cooldown();
                }
                crate::client::Probe::AuthDead => {
                    a.set_auth_dead();
                    a.stats.fail_streak = a.stats.fail_streak.max(5);
                }
                crate::client::Probe::Unknown => {
                    // Không kết luận được: giữ nhãn cũ thay vì ghi đè bằng "ok" giả.
                    if a.last_status.is_none() {
                        a.last_status = Some("verify: không kết luận được".into());
                    }
                }
            }
            if alive {
                if let Some(u) = users["data"]["biz_data"]["id"].as_str() { a.ds_uid = Some(u.to_string()); }
            }
        }
    }
    mark_dirty(&st);
    flush_store(&st).await;
    Json(json!({
        "ok": alive,
        "probe": probe.as_str(),
        "msg": users["msg"],
    }))
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
/// DSML "dịch sang tiếng Trung" mà model web đôi khi nhả (hay gặp sau lượt có ảnh):
///   <｜调用工具>\n<｜调用工具名称>NAME</｜调用工具名称>\n<｜调用工具参数名称>K</｜调用工具参数名称>\n
///   <｜调用工具参数值>V</｜调用工具参数值 | 调用工具参数>\n</｜调用工具>\n</｜调用工具>
/// → DSML chuẩn (giá trị luôn string="true"), bọc trong một khối calls. Thanh '｜' hay '|' đều nhận.
fn translate_cn_tool_tags(text: &str) -> String {
    const OPEN: &str = "<｜调用工具>";
    let t = text.replace("<|调用工具", "<｜调用工具").replace("</|调用工具", "</｜调用工具").replace("调用工具｜>", "调用工具>");
    let Some(first) = t.find(OPEN) else { return text.to_string() };
    let (head, mut rest) = t.split_at(first);
    let mut out = format!("{}<｜｜DSML｜｜ calls>\n", head);
    let tail;
    loop {
        let r = rest.trim_start();
        if let Some(r2) = r.strip_prefix(OPEN) {
            // <｜调用工具> <｜调用工具名称>NAME</｜调用工具名称>
            let r2 = r2.trim_start();
            let Some(after) = r2.strip_prefix("<｜调用工具名称>") else { rest = r2; continue };
            let Some(end) = after.find("</｜调用工具名称>") else { return text.to_string() };
            out.push_str(&format!("<｜｜DSML｜｜ invoke name=\"{}\">\n", after[..end].trim()));
            rest = &after[end + "</｜调用工具名称>".len()..];
        } else if let Some(after) = r.strip_prefix("<｜调用工具参数名称>") {
            let Some(end) = after.find("</｜调用工具参数名称>") else { return text.to_string() };
            let key = after[..end].trim();
            let v = after[end + "</｜调用工具参数名称>".len()..].trim_start();
            let Some(v) = v.strip_prefix("<｜调用工具参数值>") else { return text.to_string() };
            // giá trị kết thúc ở thẻ đóng gần nhất (model dùng lẫn 参数值 / 参数), hoặc thẻ mở/đóng kế tiếp
            let stops = ["</｜调用工具参数值>", "</｜调用工具参数>", "<｜调用工具参数名称>", "</｜调用工具>", OPEN];
            let (vend, stop) = stops.iter().filter_map(|s| v.find(s).map(|p| (p, *s))).min_by_key(|x| x.0).unwrap_or((v.len(), ""));
            out.push_str(&format!("<｜｜DSML｜｜ parameter name=\"{}\" string=\"true\">{}</｜｜DSML｜｜ parameter>\n", key, &v[..vend]));
            let consumed = if stop.starts_with("</｜调用工具参数") { stop.len() } else { 0 };
            rest = &v[vend + consumed..];
        } else if let Some(after) = r.strip_prefix("</｜调用工具>") {
            out.push_str("</｜｜DSML｜｜ invoke>\n");
            // thẻ đóng lặp (</调用工具> ×2) chỉ tính một lần
            let mut a = after.trim_start();
            while let Some(x) = a.strip_prefix("</｜调用工具>") { a = x.trim_start(); }
            rest = a;
        } else {
            tail = r;
            break;
        }
    }
    out.push_str("</｜｜DSML｜｜ calls>");
    if !tail.is_empty() { out.push('\n'); out.push_str(tail); }
    out
}

/// Model đôi khi nhả thanh thành "__DSML__": `<｜__DSML__｜__DSML__｜ parameter` (mở) và `</｜｜DSML｜｜｜__DSML__｜>` (đóng).
fn fix_underscore_dsml(text: &str) -> String {
    if !text.contains("__DSML__") && !text.contains('∣') { return text.to_string(); }
    text.replace('∣', "｜").replace("｜__DSML__｜__DSML__｜", "｜｜DSML｜｜").replace("｜__DSML__｜>", ">").replace("__DSML__", "DSML")
}

fn normalize_dsml(text: &str) -> String {
    let text = &fix_underscore_dsml(text);
    let translated;
    let text = if text.contains("调用工具") { translated = translate_cn_tool_tags(text); translated.as_str() } else { text };
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
/// Token đặc biệt MANG Ý ĐỊNH gọi tool theo chat template bản địa của DeepSeek (`<｜tool▁call▁begin｜>`,
/// `<｜tool_calls_begin｜>`, `<｜工具调用｜>`) — model web đôi khi lỡ nhả thay cho DSML. Các `<｜…｜>` khác
/// (`<｜end▁of▁sentence｜>`…) chỉ là văn bản, vd khi model đang giải thích về token.
const NATIVE_TOOL_MARKERS: [&str; 3] = ["<｜tool", "<｜工具", "<｜调用工具"];
const TOOL_MARKERS: [&str; 9] = ["<dsml", "<invoke", "<parameter", "<bash", "<tool", "<function", NATIVE_TOOL_MARKERS[0], NATIVE_TOOL_MARKERS[1], NATIVE_TOOL_MARKERS[2]];

/// Vị trí đầu tiên của marker tool-call. "DSML" trần chỉ tính khi dính markup: đứng sau '<' / thanh '｜' / "DS" (typo
/// "DSDSML") hoặc ngay trước thanh '｜'. Văn xuôi nhắc "DSML tool-call parsing" (vd bản tóm tắt compaction) thì không tính.
/// Vị trí `i` nằm trong code inline (số backtick đơn trước nó trên cùng dòng là lẻ): văn xuôi đang TRÍCH markup,
/// không phải gọi tool (vd "stray `</｜｜DSML｜｜ parameter>` inside new_string"). Khối ``` không tính ở đây.
fn in_inline_code(buf: &str, i: usize) -> bool {
    let mut start = buf[..i].rfind('\n').map_or(0, |p| p + 1).max(i.saturating_sub(4096));
    while !buf.is_char_boundary(start) { start += 1; }
    let mut odd = false;
    for (j, c) in buf[start..i].char_indices() { if is_inline_tick(buf, start + j, c) { odd = !odd; } }
    odd
}

/// Backtick mở/đóng code inline: bỏ fence ``` và tick thứ hai của cặp `` (đếm cặp như một dấu).
fn is_inline_tick(buf: &str, at: usize, c: char) -> bool {
    c == '`' && !buf[at..].starts_with("```") && !buf[..at].ends_with('`')
}

fn find_tool_marker(buf: &str) -> Option<usize> {
    // ứng viên theo thứ tự vị trí; độ chẵn lẻ backtick trên dòng tính dần một lượt (không đếm lại mỗi ứng viên —
    // văn xuôi một dòng dài toàn marker trích trong backtick từng là O(n²))
    let mut cands: Vec<usize> = TOOL_MARKERS.iter().flat_map(|k| buf.match_indices(k).map(|(i, _)| i)).collect();
    cands.sort_unstable();
    cands.dedup();
    let (mut pos, mut odd) = (0usize, false);
    let mut inline_at = |i: usize| -> bool {
        for (j, c) in buf[pos..i].char_indices() {
            if c == '\n' { odd = false } else if is_inline_tick(buf, pos + j, c) { odd = !odd }
        }
        pos = i;
        odd
    };
    let plain = cands.into_iter().find(|&i| !inline_at(i));
    let mut dsml = None;
    for (i, _) in buf.match_indices("DSML") {
        if plain.is_some_and(|p| p < i) { break; }
        if in_inline_code(buf, i) { continue; }
        let before = &buf[..i];
        let after = &buf[i + 4..];
        let glued_after = after.starts_with(['｜', '|', '∣']) || after.starts_with("__");
        let glued_before = before.ends_with(['<', '｜', '|', '∣']) || before.ends_with("DS") || before.ends_with("__");
        if glued_before || glued_after {
            // lùi về đầu cụm: bỏ "DS" typo, các thanh và '<' đứng trước
            let head = before.trim_end_matches("DS").trim_end_matches(['｜', '|', '∣', '_']);
            // phải có dạng THẺ: mở bằng '<'/'</', hoặc theo sau là tên thẻ. Văn xuôi nhắc "`｜｜DSML｜｜` fragments"
            // (vd bản tóm tắt compaction) không phải ý định gọi tool.
            let tagged = head.ends_with('<') || head.ends_with("</") || {
                let a = after.trim_start_matches(['｜', '|', '∣']).trim_start_matches([' ', ':', '_', '-']);
                [">", "calls", "call", "invoke", "parameter", "tool_call", "function_call"].iter().any(|w| a.starts_with(w))
            };
            if !tagged { continue; }
            dsml = Some(if head.ends_with('<') { head.len() - 1 } else { head.len() });
            break;
        }
    }
    match (plain, dsml) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) }
}

pub(crate) fn has_tool_marker(buf: &str) -> bool {
    find_tool_marker(buf).is_some()
}

/// Đuôi `<…` dài hơn thế này mà vẫn chưa thành marker thì không phải marker (marker dài nhất cần ~20 byte để nhận ra):
/// thả ra luôn — không thì văn xuôi kiểu `x < y` rồi một đoạn dài không có '>' bị giữ mãi và quét lại mỗi chunk.
const MAX_PENDING_TAG: usize = 48;

pub(crate) fn safe_flush_len(buf: &str) -> usize {
    if let Some(m) = find_tool_marker(buf) {
        // lùi qua '</' hoặc '<' dính ngay trước marker (find_tool_marker có thể dừng ở '/' của thẻ đóng)
        let head = &buf[..m];
        let head = head.strip_suffix('/').unwrap_or(head);
        let cut = head.strip_suffix('<').map_or(m, |h| h.len());
        return cut - dangling_bar_tag(&buf[..cut]).len();
    }
    // đuôi có thể là ĐẦU một marker chưa đủ ký tự để nhận ra (kể cả khi chunk cắt ngang marker):
    // '<…' / thanh '｜…' / '|…' chưa gặp '>' trong MAX_PENDING_TAG byte cuối, hoặc "D"/"DS"/"DSM" ở cuối.
    let mut win = buf.len().saturating_sub(MAX_PENDING_TAG);
    while !buf.is_char_boundary(win) { win += 1; }
    let from = buf.rfind('>').map_or(0, |i| i + 1).max(win);
    let mut cut = buf[from..].find(['<', '｜', '|']).map_or(buf.len(), |p| from + p);
    for partial in ["DSM", "DS", "D"] {
        if buf[..cut].ends_with(partial) { cut -= partial.len(); break; }
    }
    cut
}

/// Số lượt tối đa trên 1 session web trước khi đổi (DS_SESSION_ROTATE, mặc định 30).
/// Prompt luôn gửi đủ lịch sử, session web không giữ ngữ cảnh → để nó phình vô hạn chỉ tổ chậm/nặng.
pub(crate) fn session_rotate_limit() -> u32 {
    crate::settings::uint("DS_SESSION_ROTATE") as u32
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
                            Ok(()) => crate::elog!("[session] đã xoá session cũ {}", old),
                            Err(e) => crate::elog!("[session] xoá session cũ {} lỗi: {}", old, e),
                        }
                    }
                });
            }
            a.thread_session_id = Some(sid.to_string());
            a.thread_uses = 1;
        }
    }
}

/// Mọi parser, không lọc theo tools — chỉ còn dùng cho test/replay mẫu; runtime đi qua `extract_tool_calls`.
#[cfg(test)]
pub(crate) fn parse_any_tool_calls(text: &str) -> Option<(String, Vec<DsmlToolCall>)> {
    let n = normalize_dsml(text);
    parse_dsml_tool_calls(&n)
        .or_else(|| parse_dsml_json_tool_calls(&n))
        .or_else(|| parse_dsml_nested_tool_calls(&n))
        .or_else(|| parse_xml_tool_call(&n).map(|(b, c)| (b, vec![c])))
}

fn tool_names(tools: &[Value]) -> Vec<&str> {
    tools.iter().filter_map(|t| t.get("function").unwrap_or(t).get("name").and_then(|n| n.as_str())).collect()
}

/// Marker chắc chắn là ý định gọi tool (DSML / <invoke). Marker yếu (<bash, <tool, <function, <parameter)
/// hay gặp trong văn xuôi/code mẫu: parse trượt thì trả nguyên văn, không retry.
pub(crate) fn has_strong_tool_marker(buf: &str) -> bool {
    has_tool_marker(buf) && (buf.contains("DSML") || buf.contains("<dsml") || buf.contains("<invoke")
        || NATIVE_TOOL_MARKERS.iter().any(|m| buf.contains(m)))
}

/// Tool-call BẢN ĐỊA của DeepSeek (định dạng chat template của model, không phải DSML ta dạy):
///   <｜tool▁calls▁begin｜><｜tool▁call▁begin｜>function<｜tool▁sep｜>NAME\n```json\n{ARGS}\n```<｜tool▁call▁end｜>…
///   <｜tool▁call▁begin｜>NAME<｜tool▁sep｜>{ARGS}<｜tool▁call▁end｜>            (V3.1)
///   <｜工具调用｜>NAME\n{ARGS}                                                 (biến thể tiếng Trung)
/// Model web nhả dạng này khi trượt khỏi format (hay gặp ở bước không suy nghĩ ngay sau tool result).
/// Marker trơn không có tên/args (vòng lặp `<｜工具调用｜>` ×13) → None, để lõi retry kèm lời nhắc.
fn parse_native_tool_calls(text: &str) -> Option<(String, Vec<DsmlToolCall>)> {
    let t = text.replace('▁', "_");
    const BEGINS: [&str; 2] = ["<｜tool_call_begin｜>", "<｜工具调用｜>"];
    let first = BEGINS.iter().filter_map(|b| t.find(b)).min()?;
    let head = t[..first].replace("<｜tool_calls_begin｜>", "");
    let mut calls = Vec::new();
    let mut rest = &t[first..];
    // mỗi vòng tìm lại trên phần còn lại: giới hạn số call một lượt (thực tế ≤ vài chục)
    for _ in 0..64 {
        let Some((p, b)) = BEGINS.iter().filter_map(|b| rest.find(b).map(|p| (p, *b))).min_by_key(|x| x.0) else { break };
        let body_all = &rest[p + b.len()..];
        let end = BEGINS.iter().chain(["<｜tool_call_end｜>"].iter()).filter_map(|m| body_all.find(m)).min().unwrap_or(body_all.len());
        let body = body_all[..end].trim();
        rest = &body_all[end..];
        if body.is_empty() { continue; }
        // tên: trước <｜tool_sep｜> (bỏ "function"), hoặc ngay sau sep / dòng đầu
        let (name, after) = match body.split_once("<｜tool_sep｜>") {
            Some((pre, post)) => {
                let pre = pre.trim().trim_start_matches("function").trim();
                if pre.is_empty() {
                    let line = post.trim_start().lines().next().unwrap_or("").trim();
                    (line.to_string(), &post[post.find(line).map(|i| i + line.len()).unwrap_or(0)..])
                } else {
                    (pre.to_string(), post)
                }
            }
            None => {
                let line = body.lines().next().unwrap_or("").trim();
                (line.to_string(), &body[line.len().min(body.len())..])
            }
        };
        let name: String = name.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.').collect();
        let Some(args) = first_json_object(after) else { continue };
        if name.is_empty() { continue; }
        calls.push(DsmlToolCall { name, arguments: args.to_string() });
    }
    if calls.is_empty() { None } else { Some((strip_dsml_fragments(head.trim()), calls)) }
}

/// Object JSON hợp lệ đầu tiên trong chuỗi (bỏ qua ```json fence, ngoặc trong chuỗi JSON).
fn first_json_object(s: &str) -> Option<Value> {
    let mut from = 0;
    // mỗi lần thử quét tới cuối chuỗi: giới hạn số lần thử, không thì '{' x n là O(n²) (đo được 14s ở 130KB)
    for _ in 0..16 {
        let Some(off) = s[from..].find('{') else { break };
        let start = from + off;
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for (i, ch) in s[start..].char_indices() {
            if esc { esc = false; continue; }
            match ch {
                '\\' if in_str => esc = true,
                '"' => in_str = !in_str,
                '{' if !in_str => depth += 1,
                '}' if !in_str => {
                    depth -= 1;
                    if depth == 0 {
                        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(&s[start..start + i + 1]) { return Some(v); }
                        break;
                    }
                }
                _ => {}
            }
        }
        from = start + 1;
    }
    None
}

/// Tool-call từ output của model, CHỈ khi request có tools. DSML chuẩn nhận ở bất kỳ đâu; các dạng dự phòng
/// (JSON, nested, XML) chỉ nhận khi khối nằm cuối câu trả lời và mọi tên tool đều có trong `tools` —
/// tránh biến HTML/XML/ví dụ `<bash command=…/>` trong văn xuôi thành lệnh thật.
pub(crate) fn extract_tool_calls(text: &str, tools: &[Value]) -> Option<(String, Vec<DsmlToolCall>)> {
    let names = tool_names(tools);
    if names.is_empty() { return None; }
    let n = normalize_dsml(text);
    // DSML/bản địa chỉ khi có marker thật (marker trích trong code inline không tính)
    if has_tool_marker(&n) {
        if let Some(r) = parse_dsml_tool_calls(&n) { return Some(r); }
    }
    let tail = n.trim_end_matches(|c: char| c.is_whitespace() || c == '`');
    // định dạng bản địa cùng luật với dự phòng: khối phải kết thúc câu trả lời (dạng `<｜工具调用｜>NAME {json}` kết bằng '}')
    if tail.ends_with('>') || tail.ends_with('}') {
        if let Some(r) = parse_native_tool_calls(&n).filter(|(_, calls)| calls.iter().all(|c| names.contains(&c.name.as_str()))) {
            return Some(r);
        }
    }
    if !tail.ends_with('>') { return None; }
    parse_dsml_json_tool_calls(&n)
        .or_else(|| parse_dsml_nested_tool_calls(&n))
        .or_else(|| xml_tool_call_at_end(&n, &names))
        .filter(|(_, calls)| calls.iter().all(|c| names.contains(&c.name.as_str())))
}

/// Dạng XML `<tool_name><param>v</param></tool_name>` — chỉ xét thẻ mang tên tool đã khai báo, lấy lần xuất hiện
/// CUỐI (khối phải kết thúc câu trả lời). Quét mọi '<' như parse_xml_tool_call là O(n²) với HTML dày đặc.
fn xml_tool_call_at_end(text: &str, names: &[&str]) -> Option<(String, Vec<DsmlToolCall>)> {
    let p = names.iter().filter_map(|n| text.rfind(&format!("<{}>", n))).max()?;
    let (b, c) = parse_xml_tool_call(&text[p..])?;
    let before = format!("{} {}", text[..p].trim(), b).trim().to_string();
    Some((before, vec![c]))
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

/// Call có hợp lệ theo schema không (tên có trong tools, args khớp JSON Schema). Lỗi → đừng gửi cho client.
#[cfg(test)]
pub(crate) fn call_is_valid(tools: &Option<Vec<Value>>, call: &DsmlToolCall) -> bool {
    tools.as_ref().is_none_or(|t| call_problem(t, call).is_none())
}

/// Lỗi tool-call đã parse (None = hợp lệ). Thông điệp đưa thẳng vào lời nhắc sửa lỗi cho model khi retry.
pub(crate) enum CallProblem { UnknownTool, BadArgs(String) }

pub(crate) fn call_problem(tools: &[Value], call: &DsmlToolCall) -> Option<CallProblem> {
    let Some(f) = tools.iter().filter_map(|t| t.get("function")).find(|f| f.get("name").and_then(|n| n.as_str()) == Some(call.name.as_str())) else {
        return Some(CallProblem::UnknownTool);
    };
    let args = match serde_json::from_str::<Value>(&call.arguments) {
        Ok(v @ Value::Object(_)) => v,
        _ => return Some(CallProblem::BadArgs("arguments must be a JSON object".into())),
    };
    let Some(schema) = f.get("parameters") else { return None };
    let mut errs = Vec::new();
    check_schema(schema, &args, "", &mut errs);
    if errs.is_empty() { None } else { Some(CallProblem::BadArgs(errs.join("; "))) }
}

/// Validator JSON Schema tối giản (type/enum/required/properties/items/anyOf/oneOf) — đủ cho schema tool thường gặp.
/// Không xét additionalProperties: client nào cũng tự bỏ qua khoá thừa, bắt lỗi chỉ tổ tốn retry.
fn check_schema(sch: &Value, v: &Value, path: &str, errs: &mut Vec<String>) {
    if errs.len() >= 5 { return; }
    let at = if path.is_empty() { "arguments".to_string() } else { format!("\"{}\"", path) };
    if let Some(alts) = sch.get("anyOf").or_else(|| sch.get("oneOf")).and_then(|a| a.as_array()) {
        let ok = alts.iter().any(|a| { let mut e = Vec::new(); check_schema(a, v, path, &mut e); e.is_empty() });
        if !ok { errs.push(format!("{} matches none of the allowed shapes", at)); }
        return;
    }
    let type_ok = |t: &str| match t {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    };
    let types: Vec<&str> = match sch.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).collect(),
        _ => vec![],
    };
    if !types.is_empty() && !types.iter().any(|t| type_ok(t)) {
        errs.push(format!("{} must be {}", at, types.join(" or ")));
        return;
    }
    if let Some(en) = sch.get("enum").and_then(|e| e.as_array()) {
        if !en.contains(v) {
            errs.push(format!("{} must be one of {}", at, en.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("|")));
        }
    }
    if let Value::Object(obj) = v {
        for r in sch.get("required").and_then(|r| r.as_array()).into_iter().flatten().filter_map(|r| r.as_str()) {
            if !obj.contains_key(r) {
                let p = if path.is_empty() { r.to_string() } else { format!("{}.{}", path, r) };
                errs.push(format!("missing required \"{}\"", p));
            }
        }
        if let Some(props) = sch.get("properties").and_then(|p| p.as_object()) {
            for (k, x) in obj {
                if let Some(ps) = props.get(k) {
                    let p = if path.is_empty() { k.clone() } else { format!("{}.{}", path, k) };
                    check_schema(ps, x, &p, errs);
                }
            }
        }
    }
    if let (Value::Array(items), Some(isch)) = (v, sch.get("items")) {
        for (i, x) in items.iter().enumerate() {
            check_schema(isch, x, &format!("{}[{}]", path, i), errs);
        }
    }
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
            // ngược lại: string="false" cho tham số chuỗi (vd content: 123) → về chuỗi
            if ty == "string" {
                if let Some(v @ (Value::Number(_) | Value::Bool(_))) = args.get(k) {
                    let s = v.to_string();
                    args.insert(k.clone(), Value::String(s));
                }
                continue;
            }
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

/// Chuẩn hoá số thanh dọc quanh 'DSML': model hay nhả '<｜DSML｜｜ calls>' / '</｜DSML｜｜ invoke>' (1-3 gạch),
/// kể cả thanh ASCII hoặc trộn ('<｜||DSML||_call>', '</||DSML|| invoke>') → luôn 2 gạch toàn độ rộng mỗi bên.
fn fix_dsml_bars(text: &str) -> String {
    let is_bar = |c: &char| matches!(*c, '｜' | '|' | '∣');
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(p) = rest.find("DSML") {
        let before = &rest[..p];
        let b1: usize = before.chars().rev().take_while(is_bar).map(|c| c.len_utf8()).sum();
        let after = &rest[p + 4..];
        let b2: usize = after.chars().take_while(is_bar).map(|c| c.len_utf8()).sum();
        if b1 == 0 && b2 == 0 {
            out.push_str(&rest[..p + 4]);
            rest = after;
            continue;
        }
        out.push_str(&before[..before.len() - b1]);
        out.push_str("｜｜DSML｜｜");
        rest = &after[b2..];
    }
    out.push_str(rest);
    out
}

/// Thẻ đóng trần '</｜｜DSML｜｜>' (model bỏ chữ parameter/invoke/calls): suy ra loại từ các thẻ mở chưa đóng.
fn fix_bare_closers(text: &str) -> String {
    const BARE: &str = "</｜｜DSML｜｜>";
    if !text.contains(BARE) { return text.to_string(); }
    let (mut params, mut invokes) = (0i32, 0i32);
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let step;
        if rest.starts_with(BARE) {
            let name = if params > 0 { params -= 1; "parameter" } else if invokes > 0 { invokes -= 1; "invoke" } else { "calls" };
            out.push_str(&format!("</｜｜DSML｜｜ {}>", name));
            step = BARE.len();
        } else {
            for (tag, ctr) in [("<｜｜DSML｜｜ parameter", 0), ("</｜｜DSML｜｜ parameter>", 1), ("<｜｜DSML｜｜ invoke", 2), ("</｜｜DSML｜｜ invoke>", 3)] {
                if rest.starts_with(tag) {
                    match ctr { 0 => params += 1, 1 => params -= 1, 2 => invokes += 1, _ => invokes -= 1 }
                    break;
                }
            }
            let c = rest.chars().next().unwrap();
            out.push(c);
            step = c.len_utf8();
        }
        i += step;
    }
    out
}

/// '<｜｜DSML｜｜ file_path" string="true">' (mất 'parameter name="') → dạng chuẩn.
/// Model hay kết khối bằng `</｜｜DSML｜｜>` rồi `<｜｜DSML｜｜ calls>` (thiếu '/') thay cho `</invoke></calls>`,
/// có khi viết thêm khối thứ hai y như vậy. Invoke đang mở (không còn parameter dở) mà gặp `calls` mở lại hoặc
/// một `invoke` mới → đóng invoke ở đó; `calls` mở lại thì bỏ. Chạy sau fix_bare_closers.
fn close_reopened_invokes(text: &str) -> String {
    const CALLS: &str = "<｜｜DSML｜｜ calls>";
    const INV: &str = "<｜｜DSML｜｜ invoke";
    const INV_CLOSE: &str = "</｜｜DSML｜｜ invoke>";
    const P_OPEN: &str = "<｜｜DSML｜｜ parameter";
    const P_CLOSE: &str = "</｜｜DSML｜｜ parameter";
    if text.matches(INV).count() <= text.matches(INV_CLOSE).count() && text.matches(CALLS).count() <= 1 {
        return text.to_string();
    }
    let (mut inv_open, mut param_open, mut seen_calls) = (false, false, false);
    let mut out = String::with_capacity(text.len() + 32);
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with(CALLS) {
            if inv_open && !param_open {
                out.push_str(INV_CLOSE);
                inv_open = false;
                i += CALLS.len();
                continue;
            }
            if seen_calls && !param_open { i += CALLS.len(); continue; }
            seen_calls = true;
        } else if rest.starts_with(INV) {
            if inv_open && !param_open { out.push_str(INV_CLOSE); out.push('\n'); }
            inv_open = true;
        } else if rest.starts_with(INV_CLOSE) {
            inv_open = false;
        } else if rest.starts_with(P_OPEN) {
            param_open = true;
        } else if rest.starts_with(P_CLOSE) {
            param_open = false;
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

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

/// Cắt mảnh thẻ DSML dính ở CUỐI giá trị tham số: '…app｜｜DSML｜｜ parameter>\n', '…｜｜DSML｜｜>'.
/// Chỉ khi markup là phần đuôi (sau nó chỉ còn tên thẻ, '>' và khoảng trắng) — nội dung file hợp lệ có nhắc tới
/// DSML ở giữa (vd ghi chính source của parser này) không bị đụng.
fn strip_trailing_dsml(v: &str) -> &str {
    let mut cur = v;
    loop {
        let t = cur.trim_end();
        let Some(p) = t.rfind("DSML") else { return cur };
        let tail = t[p + 4..].trim_start_matches(['｜', '|']);
        let tail_ok = tail.trim_end_matches('>').trim().chars().all(|c| c.is_ascii_alphabetic() || c == '_');
        let head = t[..p].trim_end_matches(['｜', '|']);
        if !tail_ok || head.len() == t[..p].len() { return cur; }
        cur = head.trim_end_matches('/').trim_end_matches('<');
    }
}

/// Xoá mảnh markup DSML còn sót khỏi phần prose.
fn strip_dsml_fragments(text: &str) -> String {
    let mut t = text.to_string();
    while let Some(p) = t.find("DSML") {
        let start = t[..p].rfind('<').unwrap_or(p);
        let end = t[p..].find('>').map(|e| p + e + 1).unwrap_or(t.len());
        t.replace_range(start..end, "");
    }
    let t = t.trim_end();
    let t = t[..t.len() - dangling_bar_tag(t).len()].trim();
    t[glued_lead_bar(t).unwrap_or(0)..].trim().to_string()
}

/// Câu trả lời mở đầu bằng '<｜' dính chữ (`<｜我是…`): số byte cần bỏ (khoảng trắng đầu + '<｜'), 0 nếu không phải,
/// None nếu chưa đủ chữ để biết. Marker thật (`<｜工具调用｜>`, `<｜调用工具>`, `<｜end▁of▁sentence｜>`) có
/// '>'/'｜' trong vài ký tự đầu hoặc mở bằng chữ ASCII → không đụng.
pub(crate) fn glued_lead_bar(s: &str) -> Option<usize> {
    let lead = s.trim_start();
    let Some(after) = lead.strip_prefix("<｜") else {
        // mới có '<' hoặc chưa có gì: chưa biết
        return if lead.is_empty() || lead == "<" { None } else { Some(0) };
    };
    let head: String = after.chars().take(12).collect();
    if !after.chars().next().map_or(true, |c| !c.is_ascii()) { return Some(0); }
    if head.contains(['>', '｜', '|']) { return Some(0); }
    if head.chars().count() < 12 && !head.contains(char::is_whitespace) { return None; }
    Some(s.len() - after.len())
}

/// Mẩu thẻ token đặc biệt bỏ dở ở cuối (`<｜我`, `<｜`): model mở rồi đổi ý, viết luôn khối tool-call.
/// Chỉ tính khi sau '<｜' không có '>' và không có khoảng trắng — văn xuôi kiểu "a <｜ b" giữ nguyên.
fn dangling_bar_tag(s: &str) -> &str {
    let t = s.trim_end();
    let Some(p) = t.rfind("<｜").or_else(|| t.rfind("<|")) else { return "" };
    let frag = &t[p..];
    if frag.contains('>') || frag[1..].chars().any(char::is_whitespace) || frag.chars().count() > 12 { return ""; }
    &s[p..]
}

pub(crate) fn parse_dsml_tool_calls(text: &str) -> Option<(String, Vec<DsmlToolCall>)> {
    let fixed = fix_underscore_dsml(text).replace("DSDSML", "DSML"); // typo lặp "DS" của model
    let text = fixed.as_str();
    // DS web đổi format theo phiên bản: '<｜｜DSML｜｜ calls>' / '<｜｜DSML｜｜tool_calls>' /
    // '<｜｜DSML｜｜ calls>' có space — normalize mọi biến thể về '<｜｜DSML｜｜ calls>' trước parse
    let mut norm: String = {
        let mut out = fix_mangled_param_tags(&close_reopened_invokes(&fix_bare_closers(&space_dsml_tags(&fix_dsml_bars(text)))))
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
        if inv.is_none_or(|i| pp < i) {
            let had_inv_close = norm.contains("</｜｜DSML｜｜ invoke>");
            norm.insert_str(pp, "<｜｜DSML｜｜ invoke name=\"\">");
            if !had_inv_close {
                // chưa có thẻ đóng invoke nào: đóng ngay trước </calls> (hoặc cuối chuỗi)
                let at = norm.find("</｜｜DSML｜｜ calls>").unwrap_or(norm.len());
                norm.insert_str(at, "</｜｜DSML｜｜ invoke>\n");
            }
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
            // thẻ đóng viết thiếu '</' ('…giá trị｜｜DSML｜｜ parameter>') → đuôi markup dính vào giá trị: cắt bỏ
            let value = strip_trailing_dsml(&value).to_string();
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
    // "auto" | "none" | "required" | {"type":"function","function":{"name":X}} (kiểu OpenAI)
    #[serde(default)]
    pub(crate) tool_choice: Option<Value>,
    #[serde(default)]
    pub(crate) parallel_tool_calls: Option<bool>,
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
                let def = v.get("default").map(|d| format!(" (default {})", d)).unwrap_or_default();
                out.push_str(&format!("  param {} ({}{}{}): {}{}\n", k, ty, en, req_mark, pd, def));
                // kiểu lồng (mảng object, object có properties, anyOf…): in schema rút gọn, không thì model đoán bừa hình dạng
                let nested = ["items", "properties", "anyOf", "oneOf", "allOf", "$ref"].iter().any(|key| v.get(*key).is_some());
                if nested {
                    let mut s = v.clone();
                    if let Some(o) = s.as_object_mut() { o.remove("description"); o.remove("default"); }
                    out.push_str(&format!("    schema: {}\n", s));
                }
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
            // kết quả rỗng (mkdir, touch… thành công) vẫn phải hiện, không thì model tưởng chưa chạy và gọi lại
            let text = render_content(m);
            let text = if text.trim().is_empty() { "(no output)".to_string() } else { text };
            // kèm tên tool + id để model khớp result ↔ call (tránh lặp call)
            let tcid = m["tool_call_id"].as_str().unwrap_or("");
            match call_names.get(tcid) {
                Some(n) => Some(format!("tool_result ({}) [{}]: {}", n, tcid, text)),
                None if tcid.is_empty() => Some(format!("tool_result: {}", text)),
                None => Some(format!("tool_result [{}]: {}", tcid, text)),
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
                "available": a.auth_dead_at.is_none() && a.stats.available(now),
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
                "uid": a.ds_uid,
                "device": a.fp_seed.is_some() && a.browser.is_none(),
                "browser": a.browser.is_some(),
                "device_refreshed_at": a.device_refreshed_at,
                "auth_dead_at": a.auth_dead_at,
                "dup": a.ds_uid.as_ref().is_some_and(|u| store.accounts.iter().filter(|b| b.ds_uid.as_ref() == Some(u)).count() > 1),
            })
        })
        .collect();
    let available = store.accounts.iter().filter(|a| a.pool_enabled && a.auth_dead_at.is_none() && a.stats.available(now)).count();
    // đợt đăng xuất gần nhất: các acc mất token trong cùng một cửa sổ với acc chết muộn nhất
    let latest_dead = store.accounts.iter().filter_map(|a| a.auth_dead_at).max();
    let logged_out_burst = latest_dead.map(|l| store.accounts.iter()
        .filter(|a| a.auth_dead_at.is_some_and(|d| l - d <= crate::chat::MASS_LOGOUT_WINDOW)).count()).unwrap_or(0);
    Json(json!({
        "pool": {
            "total": store.accounts.len(),
            "available": available,
            "logged_out": store.accounts.iter().filter(|a| a.auth_dead_at.is_some()).count(),
            "logged_out_last_at": latest_dead,
            "logged_out_last_burst": logged_out_burst,
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
        "boot": boot_id(),
    }))
}

async fn api_models() -> Json<Value> {
    // MỘT model duy nhất; mức suy nghĩ chọn bằng `reasoning_effort`: "none" = Nhanh (flash), còn lại ("max"…) = DeepThink.
    Json(json!({
        "object": "list",
        "data": [{
            "id": "deepseek-web", "object": "model", "owned_by": "whalegate",
            "input_modalities": ["text", "image"],
            "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
            "reasoning_efforts": ["none", "auto", "max"],
            "context_length": crate::settings::uint("DS_CONTEXT_TOKENS")
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
                    } else if let Some(a) = s.uid.as_ref().and_then(|u| store.accounts.iter_mut().find(|a| a.ds_uid.as_deref() == Some(u.as_str()))) {
                        // cùng người dùng đăng nhập lại (token mới): thay token thay vì thêm bản trùng
                        // đổi token vẫn giữ nguyên vân tay: chốt seed vào acc (acc cũ: seed = token cũ) rồi gắn sang token mới
                        if a.fp_seed.is_none() { a.fp_seed = Some(crate::fingerprint::seed_for(&a.token)); }
                        a.token = s.token.clone();
                        crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
                        crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
                        crate::fingerprint::bind_profile(&a.token, a.browser.clone());
                        a.cookies = s.cookies.clone();
                        a.last_verified = Some(now());
                        a.last_status = Some("ok (re-login)".into());
                        a.clear_auth_dead();
                        a.stats.reset();
                        a.thread_session_id = None;
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
                            ds_uid: s.uid.clone(),
                            fp_seed: s.fp_seed.clone(),
                            device_id: s.device_id.clone(),
                            device_refreshed_at: None,
                            browser: None,
                            auth_dead_at: None,
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

// LOGIN 1 NÚT: mở Chrome riêng của acc mới (profile + vân tay riêng) → user login kiểu gì cũng được → server canh tab
// → thấy token → verify → LƯU → TẮT CHROME (giữ profile). Không cần bấm gì thêm sau khi login.
// Chỉ 1 login đang canh tại 1 thời điểm (flag in-flight).
async fn api_login_start(State(st): State<Arc<AppState>>) -> Json<Value> {
    // Mỗi acc mới = một "máy" mới: seed riêng → profile Chrome riêng + vân tay riêng.
    start_login(st, crate::fingerprint::new_seed()).await
}

/// Tiện ích trình duyệt (`extension/`) nén sẵn để "Load unpacked": đọc được cả cookie HttpOnly mà bookmarklet
/// không đọc được. Địa chỉ dashboard lấy từ chính request tải về (giống bookmarklet), nên tiện ích gửi đúng về đây.
async fn api_extension_zip(headers: axum::http::HeaderMap) -> Response {
    let host = headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("127.0.0.1:8790");
    match extension_zip(host) {
        Ok(bytes) => (
            [
                (axum::http::header::CONTENT_TYPE, "application/zip"),
                (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\"whalegate-extension.zip\""),
            ],
            bytes,
        ).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": format!("{:#}", e)}))).into_response(),
    }
}

fn extension_zip(host: &str) -> Result<Vec<u8>> {
    use std::io::Write;
    // host đi thẳng vào manifest và JS: chỉ nhận host[:port] hợp lệ
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)) {
        anyhow::bail!("Host không hợp lệ: {}", host);
    }
    // match pattern của Chrome không mang cổng: http://127.0.0.1/* phủ mọi cổng của máy đó
    let hostname = if host.starts_with('[') { host.split(']').next().map(|h| format!("{}]", h)).unwrap_or_default() } else { host.split(':').next().unwrap_or(host).to_string() };
    let manifest = include_str!("../extension/manifest.json").replace("__WG_HOST_PATTERN__", &format!("http://{}/*", hostname));
    let background = include_str!("../extension/background.js").replace("__WG_ORIGIN__", &format!("http://{}", host));
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [("manifest.json", manifest), ("background.js", background)] {
        z.start_file(format!("whalegate-extension/{}", name), opt)?;
        z.write_all(body.as_bytes())?;
    }
    Ok(z.finish()?.into_inner())
}

/// Đăng nhập lại acc DS đã đăng xuất, mở đúng Chrome riêng của nó (profile `profiles/<seed>`, còn phiên Google)
/// với đúng vân tay cũ. Người dùng tự bấm đăng nhập trong cửa sổ — whalegate không tự điền gì. Đăng nhập xong,
/// `login_flow` nhận ra cùng người dùng qua uid và thay token vào chính acc này.
async fn api_account_relogin(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let acc = st.store.read().await.accounts.iter().find(|a| a.id == id).map(|a| (a.fp_seed.clone(), a.browser.is_some(), a.label.clone()));
    let Some((seed, has_browser, label)) = acc else { return Json(json!({"ok": false, "error": "không tìm thấy tài khoản"})) };
    if has_browser {
        return Json(json!({"ok": false, "error": "acc này dùng trình duyệt gốc: đăng nhập lại trên trình duyệt đó rồi bấm tiện ích/bookmarklet trên tab chat.deepseek.com"}));
    }
    // acc dán token chưa từng có Chrome riêng: mở một máy mới — session cũ đã chết nên không có gì để "giữ máy"
    let seed = seed
        .filter(|s| std::path::Path::new(&crate::chrome_capture::account_profile_dir(s)).is_dir())
        .unwrap_or_else(crate::fingerprint::new_seed);
    crate::elog!("[login-flow] đăng nhập lại {} (profile {})", label, &seed[..8.min(seed.len())]);
    start_login(st, seed).await
}

async fn start_login(st: Arc<AppState>, seed: String) -> Json<Value> {
    // chặn double-start
    {
        let mut watching = st.login_watching.lk();
        if *watching {
            return Json(json!({"ok": false, "error": "đang có một cửa sổ đăng nhập mở — đăng nhập xong hoặc đóng cửa sổ đó rồi thử lại"}));
        }
        *watching = true;
    }
    // Mở Chrome NGAY trong request để lỗi (thiếu màn hình, Chrome chết…) hiện lên dashboard thay vì chỉ nằm trong log.
    let fp = crate::fingerprint::Fingerprint::from_seed(&seed);
    crate::elog!("[login-flow] vân tay: {} {} cores={} gpu={}", fp.user_agent(), fp.screen.0, fp.cores, fp.gpu_renderer);
    let (browser, ws_url) = match crate::chrome_capture::launch_account_chrome(&fp, "https://chat.deepseek.com/sign_in", false).await {
        Ok(x) => x,
        Err(e) => {
            *st.login_watching.lk() = false;
            crate::elog!("[login-flow] lỗi: {:#}", e);
            return Json(json!({"ok": false, "error": format!("không mở được Chrome: {:#}", e)}));
        }
    };
    crate::elog!("[login-flow] Chrome riêng của acc đã mở (cổng {})", browser.port);
    let st2 = st.clone();
    tokio::spawn(async move {
        let res = login_flow(st2.clone(), seed, browser, ws_url).await;
        *st2.login_watching.lk() = false;
        if let Err(e) = res {
            crate::elog!("[login-flow] lỗi: {:#}", e);
        }
    });
    Json(json!({
        "ok": true,
        "note": "đã mở cửa sổ Chrome riêng — login trong đó, login xong phần mềm tự lưu và tự đóng cửa sổ"
    }))
}

/// Làm mới cookie thiết bị của acc có thiết bị riêng (login qua Chrome riêng): mở lại đúng Chrome/profile/vân tay
/// của acc ở chế độ nền, DeepSeek cấp cookie mới (WAF…), ghi vào acc. KHÔNG cấp thiết bị mới cho acc chưa có: một phiên
/// đang sống bỗng xuất hiện trên máy khác (UA/OS/device id/smidV2 khác) là tín hiệu dễ bị đánh dấu.
async fn give_device(st: &Arc<AppState>, id: &str) -> Result<String> {
    let (token, seed, has_browser) = {
        let store = st.store.read().await;
        let a = store.accounts.iter().find(|a| a.id == id).ok_or_else(|| anyhow::anyhow!("không tìm thấy tài khoản"))?;
        (a.token.clone(), a.fp_seed.clone(), a.browser.is_some())
    };
    // Acc thêm bằng bookmarklet đang sống trên đúng trình duyệt của user: dựng Chrome riêng cho nó là tự tạo
    // một "máy" khác (UA/GPU/device id khác) → đúng thứ dễ bị đánh dấu. Cookie của nó do chính user làm mới.
    if has_browser {
        anyhow::bail!("tài khoản này dùng trình duyệt gốc — không tự làm mới thiết bị; bấm tiện ích/bookmarklet trên tab chat.deepseek.com của trình duyệt đó để làm mới");
    }
    let seed = seed.ok_or_else(|| anyhow::anyhow!("tài khoản này không có thiết bị riêng (chỉ acc đăng nhập qua Chrome mới làm mới được)"))?;
    let fp = crate::fingerprint::Fingerprint::from_seed(&seed);
    let s = crate::chrome_capture::materialize_device(&fp, &token).await?;
    {
        let mut store = st.store.write().await;
        let a = store.accounts.iter_mut().find(|a| a.id == id).ok_or_else(|| anyhow::anyhow!("tài khoản đã bị xoá"))?;
        if a.token != token { anyhow::bail!("token của acc đổi trong lúc dựng thiết bị — bỏ kết quả"); }
        a.fp_seed = Some(seed);
        a.device_id = s.device_id.clone();
        a.cookies = s.cookies.clone();
        a.device_refreshed_at = Some(now());
        a.last_verified = Some(now());
        crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
        crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
    }
    mark_dirty(st);
    flush_store(st).await;
    Ok(fp.user_agent())
}

/// Nút "thiết bị" trên dashboard: làm mới cookie thiết bị của acc ngay.
async fn api_account_device(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    match give_device(&st, &id).await {
        Ok(ua) => Json(json!({"ok": true, "ua": ua})),
        Err(e) => Json(json!({"ok": false, "error": format!("{:#}", e)})),
    }
}

/// Làm mới cookie thiết bị (aws-waf-token hết hạn cần JS challenge trong trình duyệt) cho các acc có thiết bị riêng,
/// lần lượt từng acc, mỗi DS_DEVICE_REFRESH_HOURS giờ. 0 = tắt.
async fn device_refresh_loop(st: Arc<AppState>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        let hours = crate::settings::uint("DS_DEVICE_REFRESH_HOURS");
        if hours == 0 { continue; }
        let due: Vec<(String, String)> = {
            let store = st.store.read().await;
            store.accounts.iter()
                .filter(|a| a.browser.is_none() && a.fp_seed.is_some() && a.device_refreshed_at.map(|t| now().saturating_sub(t) >= hours * 3600).unwrap_or(true))
                .map(|a| (a.id.clone(), a.label.clone()))
                .collect()
        };
        for (id, label) in due {
            match give_device(&st, &id).await {
                Ok(_) => crate::elog!("[device] đã làm mới cookie thiết bị: {}", label),
                Err(e) => crate::elog!("[device] làm mới thiết bị {} lỗi: {:#}", label, e),
            }
        }
    }
}

/// Canh cửa sổ login đã mở tới khi đăng nhập xong (tối đa 10 phút), tắt Chrome (giữ profile), lưu acc.
/// Acc vừa đăng nhập trong Chrome riêng: từ giờ "máy" của nó là Chrome đó (seed + device id + cookie vừa sinh).
/// Bỏ hồ sơ bookmarklet nếu có, gắn lại registry và bỏ client cũ — giữ profile cũ là gửi cookie của máy này
/// bằng vân tay của máy kia.
fn adopt_chrome_device(a: &mut Account, session: &crate::chrome_capture::CapturedSession) {
    a.fp_seed = session.fp_seed.clone();
    a.device_id = session.device_id.clone();
    a.device_refreshed_at = Some(now());
    a.browser = None;
    crate::fingerprint::unbind(&a.token);
    crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
    crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
    crate::client::forget(&a.token);
}

async fn login_flow(st: Arc<AppState>, seed: String, browser: crate::chrome_capture::AccountBrowser, ws_url: String) -> Result<()> {
    // canh login tới 10 phút, xong thì tắt Chrome (giữ profile)
    let res = crate::chrome_capture::watch_login_tab(ws_url, 600, browser.port, &seed).await;
    browser.close().await;

    let session = res?;
    crate::elog!("[login-flow] captured: {:?}", session.email);

    // lưu vào store (dedupe theo token — acc cũ chỉ refresh)
    {
        let mut store = st.store.write().await;
        if let Some(a) = store.accounts.iter_mut().find(|a| a.token == session.token) {
            a.cookies = session.cookies.clone();
            a.last_verified = Some(now());
            a.last_status = Some("ok (re-login)".into());
            a.clear_auth_dead();
            a.stats.reset();
            if session.fp_seed.is_some() { adopt_chrome_device(a, &session); }
        } else if let Some(a) = session.uid.as_ref().and_then(|u| store.accounts.iter_mut().find(|a| a.ds_uid.as_deref() == Some(u.as_str()))) {
            // cùng người dùng đăng nhập lại (token mới): thay token thay vì thêm bản trùng
            // đổi token vẫn giữ nguyên vân tay: chốt seed vào acc (acc cũ: seed = token cũ) rồi gắn sang token mới
            if a.fp_seed.is_none() { a.fp_seed = Some(crate::fingerprint::seed_for(&a.token)); }
            a.token = session.token.clone();
            crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
            crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
            crate::fingerprint::bind_profile(&a.token, a.browser.clone());
            a.cookies = session.cookies.clone();
            a.last_verified = Some(now());
            a.last_status = Some("ok (re-login)".into());
            a.clear_auth_dead();
            a.stats.reset();
            a.thread_session_id = None;
            if session.fp_seed.is_some() { adopt_chrome_device(a, &session); }
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
                ds_uid: session.uid.clone(),
                fp_seed: session.fp_seed.clone(),
                device_id: session.device_id.clone(),
                device_refreshed_at: session.fp_seed.as_ref().map(|_| now()),
                browser: None,
                auth_dead_at: None,
            };
            store.accounts.push(acc);
        }
    }
    mark_dirty(&st);
    flush_store(&st).await;
    crate::elog!("[login-flow] tab đã đóng, session đã lưu");
    Ok(())
}

// Capture session từ Chrome đang login — nút phụ (quét mọi tab)

// Refresh cookies cho toàn bộ account từ Chrome (chạy nền mỗi 5 phút)
async fn auto_refresh_loop(st: Arc<AppState>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        // cookie mà DS đặt qua Set-Cookie (ds_session_id, WAF…) nằm trong jar của client → ghi lại vào store để sống qua khởi động lại
        {
            let mut changed = false;
            let mut store = st.store.write().await;
            for a in store.accounts.iter_mut() {
                if let Some(snap) = crate::client::jar_snapshot(&a.token) {
                    for (k, v) in snap { if a.cookies.get(&k) != Some(&v) { a.cookies.insert(k, v); changed = true; } }
                }
            }
            drop(store);
            if changed { mark_dirty(&st); }
        }
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
                        crate::elog!("[auto-refresh] token mới trong Chrome chưa có trong pool — bỏ qua (dùng nút Capture nếu muốn thêm)");
                    }
                }
            }
            refreshed
        };
        if refreshed > 0 {
            mark_dirty(&st);
            flush_store(&st).await;
            crate::elog!("[auto-refresh] cookies updated from chrome ({} acc)", refreshed);
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
    // Qua tunnel/reverse proxy cùng máy (cloudflared, nginx…) kết nối vẫn đến từ 127.0.0.1, nhưng người gửi thật ở
    // ngoài Internet: header proxy để lại là dấu hiệu. Không coi những request đó là "localhost".
    let local = addr.ip().is_loopback() && !is_proxied(req.headers());
    if (path == "/" || path.starts_with("/api/")) && !local {
        return (StatusCode::FORBIDDEN, Json(json!({"error": "admin chỉ cho phép từ localhost"}))).into_response();
    }
    if path.starts_with("/v1/") {
        let key = std::env::var("DS_API_KEY").unwrap_or_default();
        if !key.is_empty() {
            let ok = req.headers().get("authorization").and_then(|v| v.to_str().ok())
                .map(|v| v == format!("Bearer {}", key)).unwrap_or(false);
            if !ok {
                return (StatusCode::UNAUTHORIZED, Json(json!({"error": "thiếu hoặc sai API key"}))).into_response();
            }
        } else if !local {
            // mở /v1 ra Internet mà không có key = ai cũng dùng được pool acc của bạn
            return (StatusCode::FORBIDDEN, Json(json!({"error": "whalegate đang được truy cập từ ngoài máy mà chưa đặt DS_API_KEY — đặt DS_API_KEY trước khi mở ra Internet"}))).into_response();
        }
    }
    let mut resp = next.run(req).await;
    if resp.status() == StatusCode::TOO_MANY_REQUESTS && !resp.headers().contains_key("retry-after") {
        resp.headers_mut().insert("retry-after", axum::http::HeaderValue::from_static("5"));
    }
    resp
}

/// Request đi qua proxy/tunnel (Cloudflare Tunnel, nginx, Caddy…): các proxy này luôn gắn ít nhất một header sau.
fn is_proxied(h: &axum::http::HeaderMap) -> bool {
    ["cf-connecting-ip", "cf-ray", "x-forwarded-for", "x-real-ip", "forwarded", "x-forwarded-host"].iter().any(|k| h.contains_key(*k))
}

/// Tài khoản cũ chưa có id người dùng DeepSeek: hỏi users/current một lần (lần lượt, nhẹ) để nhận ra bản trùng.
async fn backfill_uids(st: Arc<AppState>) {
    let todo: Vec<(String, String, String)> = {
        let store = st.store.read().await;
        store.accounts.iter().filter(|a| a.ds_uid.is_none()).map(|a| (a.id.clone(), a.token.clone(), cookie_string(&a.cookies))).collect()
    };
    for (id, token, cookies) in todo {
        let Ok(c) = DsClient::new(token, cookies) else { continue };
        let Ok(v) = c.users_current().await else { continue };
        if v["code"].as_i64() != Some(0) { continue; }
        if let Some(u) = v["data"]["biz_data"]["id"].as_str() {
            if let Some(a) = st.store.write().await.accounts.iter_mut().find(|a| a.id == id) { a.ds_uid = Some(u.to_string()); }
            mark_dirty(&st);
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    crate::elog!("[uid] đã nhận id người dùng cho các tài khoản cũ");
}

/// Acc cũ (dán token, bookmarklet) chưa có ds_session_id: lấy một lần như trình duyệt lấy (xem
/// `DsClient::ensure_session_cookie`). Lần lượt, có nghỉ, bỏ qua acc DS đã đăng xuất.
async fn backfill_session_cookies(st: Arc<AppState>) {
    let todo: Vec<(String, String, String)> = {
        let store = st.store.read().await;
        store.accounts.iter().filter(|a| a.auth_dead_at.is_none() && !a.cookies.contains_key("ds_session_id"))
            .map(|a| (a.id.clone(), a.token.clone(), cookie_string(&a.cookies))).collect()
    };
    if todo.is_empty() { return; }
    let mut got = 0;
    for (id, token, cookies) in &todo {
        let Ok(c) = DsClient::new(token.clone(), cookies.clone()) else { continue };
        match c.ensure_session_cookie().await {
            Ok(sid) => {
                if let Some(a) = st.store.write().await.accounts.iter_mut().find(|a| &a.id == id) {
                    a.cookies.entry("ds_session_id".into()).or_insert(sid);
                    got += 1;
                }
                mark_dirty(&st);
            }
            Err(e) => crate::elog!("[cookie] {}: {:#}", &id[..8.min(id.len())], e),
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    crate::elog!("[cookie] đã lấy ds_session_id cho {}/{} acc còn thiếu", got, todo.len());
}

/// Dọn phiên web nhàn rỗi quá DS_CONV_TTL (mặc định 1800s): xoá trên DS, khỏi registry phiên và khỏi registry hội thoại.
async fn conv_sweeper(st: Arc<AppState>) {
    loop {
        let ttl: u64 = crate::settings::uint("DS_CONV_TTL"); // đọc mỗi vòng: đổi trên dashboard có hiệu lực ngay
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
                        Ok(()) => crate::elog!("[conv] đã xoá phiên nhàn rỗi {}", &sid[..8.min(sid.len())]),
                        Err(e) => crate::elog!("[conv] xoá phiên {} lỗi: {}", sid, e),
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
        .route("/api/accounts/export", get(crate::transfer::api_export_accounts))
        .route("/api/accounts/import", post(crate::transfer::api_import_accounts))
        .route("/api/accounts/{id}", axum::routing::delete(api_delete_account).patch(api_edit_account))
        .route("/api/accounts/{id}/verify", post(api_verify_account))
        .route("/api/accounts/{id}/pool", post(api_toggle_pool))
        .route("/api/accounts/{id}/reset", post(api_reset_account))
        .route("/api/accounts/{id}/device", post(api_account_device))
        .route("/api/accounts/{id}/relogin", post(api_account_relogin))
        .route("/api/extension.zip", get(api_extension_zip))
        .route("/api/capture", post(api_capture_from_chrome))
        .route("/api/login_start", post(api_login_start))
        .route("/api/stats", get(api_stats))
        .route("/api/usage", get(api_usage))
        .route("/api/logs", get(api_logs))
        .route("/api/logs/stream", get(api_logs_stream))
        .route("/api/settings", get(api_settings_get).post(api_settings_set))
        .route("/api/restart", get(api_restart_status).post(api_restart))
        .route("/api/dsh", get(api_dsh_status).post(api_dsh_setup))
        .route("/api/update", get(crate::transfer::api_update_check).post(crate::transfer::api_update_apply))
        .route("/v1/chat/completions", post(crate::chat::api_chat_completions))
        .route("/v1/search/messages", post(crate::chat::api_search_messages))
        .route("/v1/models", get(api_models))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)) // 32MB — prompt dài (256K+ token)
        .layer(axum::middleware::from_fn(access_guard))
        .with_state(st.clone());

    // Background: dọn phiên nhàn rỗi, refresh cookies từ Chrome, ghi store gom lô
    let _ = boot_id();
    crate::usage::spawn_background();
    tokio::spawn(backfill_uids(st.clone()));
    tokio::spawn(backfill_session_cookies(st.clone()));
    tokio::spawn(conv_sweeper(st.clone()));
    tokio::spawn(store_flusher(st.clone()));
    tokio::spawn(auto_refresh_loop(st.clone()));
    tokio::spawn(device_refresh_loop(st));

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    // thử lại vài giây: lúc tự khởi động lại trên Windows tiến trình cũ có thể chưa kịp nhả cổng
    let mut tries = 0;
    let listener = loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => break l,
            Err(e) if tries < 20 && e.kind() == std::io::ErrorKind::AddrInUse => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            Err(e) => return Err(anyhow::anyhow!("không mở được cổng {}: {} — đổi cổng bằng DS_PORT", port, e)),
        }
    };
    println!("whalegate listening on http://{} — open it in a browser to add accounts", addr);
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

    #[test]
    fn parse_underscore_dsml_bars() {
        // mẫu thật (dsh session 1a6065d5): thanh bị nhả thành "__DSML__", thiếu invoke, đóng bằng `｜__DSML__｜>`
        let text = "Giờ tôi dựng trang.\n\n<｜｜DSML｜｜ calls>\n<｜__DSML__｜__DSML__｜ parameter name=\"code\" string=\"true\">const files: Record<string, string> = {};</｜｜DSML｜｜ parameter>\n<｜__DSML__｜__DSML__｜ parameter name=\"description\" string=\"true\">Read</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜｜__DSML__｜>";
        assert!(has_strong_tool_marker(text));
        let tools = vec![json!({"type":"function","function":{"name":"run_code","parameters":{"type":"object",
            "properties":{"code":{"type":"string"},"description":{"type":"string"}},"required":["code","description"]}}})];
        let (before, calls) = extract_tool_calls(text, &tools).expect("phải parse được");
        assert_eq!(calls.len(), 1);
        assert!(calls[0].arguments.contains("Record<string, string>") && calls[0].arguments.contains("Read"), "{}", calls[0].arguments);
        assert!(!before.contains("DSML"));
        // không có thẻ mở calls, chỉ mở bằng parameter __DSML__ (step cuối của session)
        assert!(has_tool_marker("Xong.\n\n<｜__DSML__｜__DSML__｜ parameter name=\"code\">x"));
    }

    #[test]
    fn parse_underscore_dsml_closers_without_invoke() {
        // mẫu thật (dsh session 1a6065d5, turn 14): không có calls/invoke mở, đóng bằng `</｜__DSML__｜__DSML__｜>` rồi `… calls>`
        let text = "Đủ 11 phần.\n\n<｜__DSML__｜__DSML__｜ parameter name=\"code\" string=\"true\">const r = await tools.bash({ command: `echo hi` });</｜__DSML__｜__DSML__｜ parameter>\n<｜__DSML__｜__DSML__｜ parameter name=\"description\" string=\"true\">Spot-check</｜__DSML__｜__DSML__｜ parameter>\n</｜__DSML__｜__DSML__｜>\n</｜__DSML__｜__DSML__｜ calls>";
        let tools = vec![json!({"type":"function","function":{"name":"run_code","parameters":{"type":"object",
            "properties":{"code":{"type":"string"},"description":{"type":"string"}},"required":["code","description"]}}})];
        let (before, calls) = extract_tool_calls(text, &tools).expect("phải parse được");
        assert_eq!(calls.len(), 1);
        assert!(calls[0].arguments.contains("echo hi") && calls[0].arguments.contains("Spot-check"), "{}", calls[0].arguments);
        assert!(!before.contains("DSML") && before.contains("Đủ 11 phần"));
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
        assert!(has_tool_marker("<｜｜DSDSML｜｜ calls>") && has_tool_marker("a ｜DSML｜｜ invoke") && has_tool_marker("x DSML｜｜ calls>"));
        // nhắc tới DSML trong văn xuôi (không có dạng thẻ) không phải marker
        assert!(!has_tool_marker("mangled with literal `｜｜DSML｜｜` fragments") && !has_tool_marker("a ｜DSML｜｜ b"));
        assert_eq!(safe_flush_len("xin chào <｜｜DSDSML｜｜ calls>"), "xin chào ".len());
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

#[cfg(test)]
mod sample_replay {
    /// Chạy lại các mẫu hỏng đã lưu (~/.whalegate/samples.jsonl) qua parser hiện tại:
    ///   cargo test --release replay_samples -- --ignored --nocapture
    #[test]
    #[ignore]
    fn replay_samples() {
        let p = std::path::PathBuf::from(super::home_dir()).join(".whalegate/samples.jsonl");
        let (mut ok, mut bad) = (0, 0);
        for l in std::fs::read_to_string(p).unwrap_or_default().lines() {
            let v: serde_json::Value = match serde_json::from_str(l) { Ok(v) => v, Err(_) => continue };
            let raw = v["raw"].as_str().unwrap_or("");
            if !super::has_tool_marker(raw) { continue; }
            match super::parse_any_tool_calls(raw) {
                Some((_, calls)) if !calls.is_empty() => ok += 1,
                _ => { bad += 1; println!("FAIL {:?}", raw.chars().take(260).collect::<String>()); }
            }
        }
        println!("replay: ok={} fail={}", ok, bad);
    }
}

#[cfg(test)]
mod bare_close_tests {
    use super::*;
    #[test]
    fn no_invoke_and_bare_closers() {
        let t = "<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ parameter name=\"command\" string=\"true\">ls</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"description\" string=\"true\">d</｜｜DSML｜｜>\n</｜｜DSML｜｜ calls>";
        let (_, c) = parse_dsml_tool_calls(t).expect("parse");
        assert_eq!(c.len(), 1);
        assert!(c[0].arguments.contains("\"command\":\"ls\""));
    }
    #[test]
    fn dsdsml_typo_parses() {
        let t = "<｜｜DSDSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"bash\">\n<｜｜DSML｜｜ parameter name=\"command\" string=\"true\">ls</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        assert_eq!(parse_dsml_tool_calls(t).expect("parse").1[0].name, "bash");
    }
    #[test]
    fn mangled_command_with_bare_closers() {
        let t = "<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"bash\">\n<｜｜DSML｜｜ command\" string=\"true\">ls -la</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"description\" string=\"true\">x</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜>\n</｜｜DSML｜｜ calls>";
        let (_, c) = parse_dsml_tool_calls(t).expect("parse");
        assert_eq!(c[0].name, "bash");
        assert!(c[0].arguments.contains("ls -la"), "{}", c[0].arguments);
    }
}

#[cfg(test)]
mod token_paste_tests {
    use super::normalize_token;
    #[test]
    fn accepts_localstorage_shapes() {
        let t = "AbCdEf0123456789AbCdEf0123456789AbCdEf0123456789";
        assert_eq!(normalize_token(&format!("{{\"value\":\"{t}\",\"__version\":\"0\"}}")), t);
        assert_eq!(normalize_token(&format!("\"{t}\"")), t);
        assert_eq!(normalize_token(&format!("  Bearer {t}\n")), t);
        assert_eq!(normalize_token(t), t);
    }
}

#[cfg(test)]
mod edit_account_tests {
    #[test]
    fn edit_route_accepts_partial_body() {
        let e: super::EditAccount = serde_json::from_str(r#"{"label":"mới"}"#).unwrap();
        assert!(e.token.is_none() && e.label.as_deref() == Some("mới"));
    }
}

#[cfg(test)]
mod tool_gate_tests {
    use super::*;

    fn bash_tools() -> Vec<Value> {
        vec![json!({"type":"function","function":{"name":"bash","parameters":{"type":"object",
            "properties":{"command":{"type":"string"},"description":{"type":"string"}},"required":["command"]}}})]
    }

    #[test]
    fn prose_html_is_not_a_tool_call() {
        let t = bash_tools();
        assert!(extract_tool_calls("Dùng HTML: <div><span>hi</span></div> là xong.", &t).is_none());
        assert!(extract_tool_calls("Ví dụ XML:\n<config><port>8080</port></config>", &t).is_none(), "tag không phải tên tool");
        // ví dụ lệnh giữa văn xuôi: không được thành lệnh thật
        assert!(extract_tool_calls("Lệnh: <bash command=\"rm -rf build\"/> trong tài liệu", &t).is_none());
        // không có tools → không bao giờ có tool-call
        assert!(extract_tool_calls("<｜｜DSML｜｜ calls><｜｜DSML｜｜ invoke name=\"bash\"><｜｜DSML｜｜ parameter name=\"command\" string=\"true\">ls</｜｜DSML｜｜ parameter></｜｜DSML｜｜ invoke></｜｜DSML｜｜ calls>", &[]).is_none());
    }

    #[test]
    fn fallback_formats_still_parse_at_end() {
        let t = bash_tools();
        let (b, c) = extract_tool_calls("Xem repo.\n\n<bash> <command>pwd</command> </bash>", &t).expect("xml cuối câu");
        assert_eq!((b.as_str(), c[0].name.as_str()), ("Xem repo.", "bash"));
        assert!(extract_tool_calls("x\n<bash command=\"ls\" description=\"L\" />\n", &t).is_some());
        assert!(!has_strong_tool_marker("dùng <function> trong TS") && has_strong_tool_marker("<｜｜DSML｜｜ calls>"));
    }

    #[test]
    fn schema_validation_nested() {
        let tools = vec![json!({"function":{"name":"todo","parameters":{"type":"object","required":["items"],"properties":{
            "items":{"type":"array","items":{"type":"object","required":["text","status"],"properties":{
                "text":{"type":"string"},"status":{"enum":["todo","done"]}}}}}}}})];
        let call = |a: &str| DsmlToolCall { name: "todo".into(), arguments: a.into() };
        assert!(call_problem(&tools, &call(r#"{"items":[{"text":"a","status":"done"}]}"#)).is_none());
        let Some(CallProblem::BadArgs(e)) = call_problem(&tools, &call(r#"{"items":[{"text":"a","status":"later"},{"status":"todo"}]}"#)) else { panic!() };
        assert!(e.contains("items[0].status") && e.contains("missing required \"items[1].text\""), "{}", e);
        assert!(matches!(call_problem(&tools, &DsmlToolCall { name: "nope".into(), arguments: "{}".into() }), Some(CallProblem::UnknownTool)));
        let Some(CallProblem::BadArgs(e)) = call_problem(&tools, &call(r#"{"items":"x"}"#)) else { panic!() };
        assert!(e.contains("must be array"), "{}", e);
    }

    #[test]
    fn nested_schema_is_rendered() {
        let tools = Some(vec![json!({"function":{"name":"todo","parameters":{"properties":{
            "items":{"type":"array","description":"list","items":{"type":"object","properties":{"done":{"type":"boolean"}}}},
            "n":{"type":"integer","default":3}},"required":["items"]}}})]);
        let s = render_tools_spec(&tools);
        assert!(s.contains("schema: {\"items\":{\"properties\":{\"done\":{\"type\":\"boolean\"}}"), "{}", s);
        assert!(s.contains("(default 3)"));
    }

    #[test]
    fn string_param_coerced_back_to_string() {
        let tools = Some(vec![json!({"function":{"name":"write","parameters":{"properties":{"content":{"type":"string"}}}}})]);
        let mut c = DsmlToolCall { name: "write".into(), arguments: r#"{"content":123}"#.into() };
        fill_required_args(&tools, &mut c);
        assert_eq!(c.arguments, r#"{"content":"123"}"#);
    }

    #[test]
    fn empty_tool_result_is_kept() {
        let msgs = vec![
            json!({"role":"assistant","content":"","tool_calls":[{"id":"c1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"mkdir x\"}"}}]}),
            json!({"role":"tool","tool_call_id":"c1","content":""}),
        ];
        assert!(messages_to_prompt(&msgs).contains("tool_result (bash) [c1]: (no output)"));
    }
}

#[cfg(test)]
mod native_toolcall_tests {
    use super::*;

    fn tools() -> Vec<Value> {
        vec![json!({"type":"function","function":{"name":"run_code","parameters":{"type":"object",
            "properties":{"code":{"type":"string"},"description":{"type":"string"}},"required":["code","description"]}}})]
    }

    #[test]
    fn chinese_marker_loop_is_strong_but_unparsed() {
        // mẫu thật (dsh session 3615c2c3): model nhả 13 lần marker bản địa, không tên, không args
        let t = "<｜工具调用｜>\n".repeat(12) + "<｜工具调用｜>";
        assert!(has_strong_tool_marker(&t), "phải bị giữ lại, không stream ra client");
        assert!(extract_tool_calls(&t, &tools()).is_none(), "không có gì để gọi → để lõi retry kèm lời nhắc");
        assert_eq!(safe_flush_len(&t), 0);
    }

    #[test]
    fn native_v3_format_parses() {
        let t = "Reading it.\n<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>function<｜tool▁sep｜>run_code\n```json\n{\"code\": \"console.log(1)\", \"description\": \"print\"}\n```<｜tool▁call▁end｜><｜tool▁calls▁end｜>";
        let (before, c) = extract_tool_calls(t, &tools()).expect("parse native v3");
        assert_eq!(before, "Reading it.");
        assert_eq!(c[0].name, "run_code");
        assert!(c[0].arguments.contains("console.log(1)"));
    }

    #[test]
    fn other_special_tokens_and_examples_stay_text() {
        let t = tools();
        // giải thích về token đặc biệt: không giữ, không retry
        let prose = "DeepSeek ends each turn with <｜end▁of▁sentence｜> and marks roles with <｜User｜>.";
        assert!(!has_tool_marker(prose) && !has_strong_tool_marker(prose));
        assert_eq!(safe_flush_len(prose), prose.len());
        // ví dụ tool-call bản địa GIỮA văn xuôi: không thành lệnh thật
        let ex = "The native format looks like <｜tool▁call▁begin｜>run_code<｜tool▁sep｜>{\"code\":\"rm -rf /\",\"description\":\"x\"}<｜tool▁call▁end｜> — don't copy it.";
        assert!(extract_tool_calls(ex, &t).is_none());
        // tên tool lạ ở dạng bản địa: không nhận (để lõi retry kèm lời nhắc)
        let unk = "<｜tool▁call▁begin｜>nope<｜tool▁sep｜>{\"a\":1}<｜tool▁call▁end｜>";
        assert!(extract_tool_calls(unk, &t).is_none());
    }

    #[test]
    fn native_v31_and_chinese_with_args_parse() {
        let t = "<｜tool▁call▁begin｜>run_code<｜tool▁sep｜>{\"code\":\"a\",\"description\":\"b\"}<｜tool▁call▁end｜>";
        assert_eq!(extract_tool_calls(t, &tools()).unwrap().1[0].name, "run_code");
        let t = "<｜工具调用｜>run_code\n{\"code\":\"x({y:1})\",\"description\":\"d\"}";
        let (_, c) = extract_tool_calls(t, &tools()).unwrap();
        let v: Value = serde_json::from_str(&c[0].arguments).unwrap();
        assert_eq!(v["code"], "x({y:1})");
    }

    #[test]
    fn chinese_xml_tags_parse() {
        // mẫu thật (dsh session 20a53b8f, turn 3): thẻ đóng giá trị dùng lẫn 参数值/参数, </调用工具> lặp
        let t = "Danh sách lỗi đã rõ.\n\n<｜调用工具>\n<｜调用工具名称>run_code</｜调用工具名称>\n<｜调用工具参数名称>description</｜调用工具参数名称>\n<｜调用工具参数值>Fix footer</｜调用工具参数值>\n<｜调用工具参数名称>code</｜调用工具参数名称>\n<｜调用工具参数值>const old = `a {\n  b: c;\n}`;\nawait tools.edit({ old_string: old });\nconsole.log(\"x<y\");</｜调用工具参数>\n</｜调用工具>\n</｜调用工具>\n\n<｜调用工具>\n<｜调用工具名称>run_code</｜调用工具名称>\n<｜调用工具参数名称>description</｜调用工具参数名称>\n<｜调用工具参数值>Second</｜调用工具参数值>\n<｜调用工具参数名称>code</｜调用工具参数名称>\n<｜调用工具参数值>console.log(2);</｜调用工具参数>\n</｜调用工具>\n</｜调用工具>";
        assert!(has_strong_tool_marker(t));
        assert_eq!(&t[..safe_flush_len(t)], "Danh sách lỗi đã rõ.\n\n");
        let (before, c) = extract_tool_calls(t, &tools()).expect("parse chinese xml");
        assert_eq!(before, "Danh sách lỗi đã rõ.");
        assert_eq!(c.len(), 2);
        let v: Value = serde_json::from_str(&c[0].arguments).unwrap();
        assert_eq!(v["description"], "Fix footer");
        assert_eq!(v["code"], "const old = `a {\n  b: c;\n}`;\nawait tools.edit({ old_string: old });\nconsole.log(\"x<y\");");
        let v: Value = serde_json::from_str(&c[1].arguments).unwrap();
        assert_eq!(v["code"], "console.log(2);");
    }

    #[test]
    fn dangling_bar_tag_before_dsml_is_dropped() {
        // mẫu thật (dsh session 20a53b8f): "<｜我" lọt ra trước khối DSML hợp lệ
        let t = "<｜我<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"run_code\">\n<｜｜DSML｜｜ parameter name=\"code\" string=\"true\">x</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"description\" string=\"true\">d</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        assert_eq!(safe_flush_len(t), 0);
        let (before, c) = extract_tool_calls(t, &tools()).unwrap();
        assert_eq!(before, "");
        assert_eq!(c[0].name, "run_code");
        let t2 = "Done. <｜我\n<｜｜DSML｜｜ calls>";
        assert_eq!(&t2[..safe_flush_len(t2)], "Done. ");
        // văn xuôi có khoảng trắng sau '<｜' thì giữ
        assert_eq!(dangling_bar_tag("a <｜ b"), "");
    }

    #[test]
    fn bare_closer_then_reopened_calls_parses_both_blocks() {
        // mẫu thật (dsh session 20a53b8f, 21:27): tham số cuối đóng bằng '</｜｜DSML｜｜>' rồi 'calls' mở lại thay cho
        // '</invoke></calls>', hai khối liền. Trước đây khối 1 lộ ra thành chữ, khối 2 bị cắt ở '<<' của heredoc.
        let block = |code: &str, d: &str| format!("<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"run_code\">\n<｜｜DSML｜｜ parameter name=\"code\" string=\"true\">{}</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"description\" string=\"true\">{}</｜｜DSML｜｜>\n\n", code, d);
        let code2 = "const r = await tools.bash({ command: `cd /x && python3 - <<'PY'\nimport re\nprint(1)\nPY` });";
        let t = format!("Vẫn 1010px. Kiểm tra trực tiếp.\n\n{}{}<｜｜DSML｜｜ calls>", block("grep -n viewport index.html", "Check viewport"), block(code2, "Find wide elements"));
        let (before, c) = extract_tool_calls(&t, &tools()).expect("parse cả hai khối");
        assert_eq!(before, "Vẫn 1010px. Kiểm tra trực tiếp.");
        assert_eq!(c.len(), 2);
        let a: Value = serde_json::from_str(&c[0].arguments).unwrap();
        let b: Value = serde_json::from_str(&c[1].arguments).unwrap();
        assert_eq!(a["code"], "grep -n viewport index.html");
        assert_eq!(a["description"], "Check viewport");
        assert_eq!(b["code"], code2);
        assert_eq!(b["description"], "Find wide elements");
    }

    #[test]
    fn glued_lead_bar_rules() {
        assert_eq!(glued_lead_bar("<｜我是一名刚学 HTML/CSS/JS 的学生"), Some("<｜".len()));
        assert_eq!(glued_lead_bar("<｜我是"), None, "chưa đủ chữ: chờ");
        assert_eq!(glued_lead_bar("<｜工具调用｜>run_code"), Some(0));
        assert_eq!(glued_lead_bar("<｜调用工具>\n<｜调用工具名称>"), Some(0));
        assert_eq!(glued_lead_bar("<｜｜DSML｜｜ calls>"), Some(0));
        assert_eq!(glued_lead_bar("<｜end▁of▁sentence｜>"), Some(0));
        assert_eq!(glued_lead_bar("Xin chào"), Some(0));
    }
}


#[cfg(test)]
mod dsml_leak_tests {
    use super::*;

    fn tools() -> Vec<Value> {
        vec![json!({"type":"function","function":{"name":"run_code","parameters":{"type":"object",
            "properties":{"code":{"type":"string"},"description":{"type":"string"}},"required":["code","description"]}}})]
    }
    fn call(code_desc_tail: &str) -> String {
        format!("<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"run_code\">\n<｜｜DSML｜｜ parameter name=\"code\" string=\"true\">x()</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"description\" string=\"true\">{}\n</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>", code_desc_tail)
    }

    #[test]
    fn closing_tag_without_slash_does_not_leak_into_value() {
        // mẫu thật (dsh session c39df5ed): '…relaunch app｜｜DSML｜｜ parameter>' và '…requirements｜｜DSML｜｜>'
        for tail in ["Copy keychain｜｜DSML｜｜ parameter>", "Probe altool｜｜DSML｜｜>", "Probe｜DSML｜｜ parameter>"] {
            let (_, c) = extract_tool_calls(&call(tail), &tools()).unwrap();
            let v: Value = serde_json::from_str(&c[0].arguments).unwrap();
            assert!(!v["description"].as_str().unwrap().contains("DSML"), "{:?}", v["description"]);
        }
        // DSML ở GIỮA giá trị (nội dung hợp lệ) phải giữ nguyên
        assert_eq!(strip_trailing_dsml("a <｜｜DSML｜｜ calls> b"), "a <｜｜DSML｜｜ calls> b");
        assert_eq!(strip_trailing_dsml("plain"), "plain");
    }

    #[test]
    fn ascii_and_mixed_bars_parse() {
        // mẫu thật: '<｜||DSML||_call>' và '<||DSML||_call>'
        let t = "<||DSML||_call>\n<||DSML|| invoke name=\"run_code\">\n<||DSML|| parameter name=\"code\" string=\"true\">x()</||DSML|| parameter>\n<||DSML|| parameter name=\"description\" string=\"true\">d</||DSML|| parameter>\n</||DSML|| invoke>\n</||DSML||_call>";
        let (before, c) = extract_tool_calls(t, &tools()).expect("thanh ASCII phải parse được");
        assert_eq!((before.as_str(), c[0].name.as_str()), ("", "run_code"));
        let t = "<｜||DSML||_call>\n<｜｜DSML｜｜ invoke name=\"run_code\">\n<｜｜DSML｜｜ parameter name=\"code\" string=\"true\">x()</｜｜DSML｜｜ parameter>\n<｜｜DSML｜｜ parameter name=\"description\" string=\"true\">d</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>";
        assert!(extract_tool_calls(t, &tools()).is_some());
        assert_eq!(safe_flush_len(t), 0, "không lộ ra stream");
        // văn xuôi có ống '|' bình thường không bị đụng
        assert_eq!(fix_dsml_bars("a | b || c"), "a | b || c");
    }
}

#[cfg(test)]
mod hourly_limit_tests {
    use super::AccStats;

    #[test]
    fn hourly_cap_rests_account_until_window_ends() {
        assert_eq!(AccStats::hourly_limit(), 0, "mặc định tắt giới hạn");
        std::env::set_var("DS_HOURLY_LIMIT", "60");
        let lim = AccStats::hourly_limit();
        assert_eq!(lim, 60);
        let mut s = AccStats::default();
        let t0 = 1_000_000u64;
        // rải đều (né burst): mỗi lượt cách 40s, chưa tới trần thì vẫn rảnh
        for i in 0..lim as u64 - 1 { s.record_call(t0 + i * 40); assert!(s.available(t0 + i * 40 + 30)); }
        s.record_call(t0 + 3000);
        assert!(!s.available(t0 + 3010), "chạm trần: nghỉ");
        assert!(s.available(t0 + 3600), "hết giờ: rảnh lại");
        s.record_call(t0 + 3600);
        assert_eq!(s.hour_count, 1, "sang giờ mới thì đếm lại");
        std::env::remove_var("DS_HOURLY_LIMIT");
    }
}

#[cfg(test)]
mod readd_tests {
    use super::*;

    fn acc() -> Account {
        let mut a: Account = serde_json::from_value(json!({"id": "x", "label": "x@mail", "token": "old-token", "cookies": {"ds_session_id": "s1", "aws-waf-token": "w1"},
            "added_at": 0, "last_verified": null, "last_status": null, "thread_session_id": "sid", "thread_parent_message_id": 7})).unwrap();
        a.thread_uses = 3;
        a.set_auth_dead();
        a
    }

    #[test]
    fn same_token_merges_cookies_and_revives() {
        let mut a = acc();
        let changed = apply_readd(&mut a, "old-token".into(), [("aws-waf-token".to_string(), "w2".to_string())].into(), None, Some("u1".into()), "");
        assert!(!changed);
        // cookie httpOnly bookmarklet không đọc được (ds_session_id) phải còn nguyên
        assert_eq!(a.cookies["ds_session_id"], "s1");
        assert_eq!(a.cookies["aws-waf-token"], "w2");
        assert!(a.auth_dead_at.is_none());
        assert_eq!(a.thread_session_id.as_deref(), Some("sid"), "cùng token thì phiên web vẫn dùng tiếp được");
        assert_eq!(a.label, "x@mail");
    }

    #[test]
    fn new_token_replaces_session_but_keeps_machine() {
        let mut a = acc();
        let changed = apply_readd(&mut a, "new-token".into(), Default::default(), None, None, "");
        assert!(changed);
        assert_eq!(a.token, "new-token");
        // vân tay cũ dẫn xuất từ token cũ → chốt lại để token mới vẫn đi đúng "máy" đó
        assert_eq!(a.fp_seed.as_deref(), Some("old-token"));
        assert!(a.thread_session_id.is_none() && a.thread_parent_message_id.is_none() && a.thread_uses == 0);
        assert!(a.auth_dead_at.is_none());
    }

    #[test]
    fn browser_profile_moves_account_to_that_browser() {
        let mut a = acc();
        a.fp_seed = Some("chrome-seed".into());
        a.device_refreshed_at = Some(1);
        let b = crate::fingerprint::BrowserProfile { seed: Some("bm-seed".into()), device_id: Some("dev-bm".into()), ..Default::default() };
        apply_readd(&mut a, "new-token".into(), [("aws-waf-token".to_string(), "w9".to_string())].into(), Some(b), None, "nhãn mới");
        assert_eq!(a.fp_seed.as_deref(), Some("bm-seed"));
        assert_eq!(a.device_id.as_deref(), Some("dev-bm"));
        assert!(a.browser.is_some() && a.device_refreshed_at.is_none());
        // token mới: cookie thay hẳn, không mang cookie phiên của token cũ
        assert!(!a.cookies.contains_key("ds_session_id"));
        assert_eq!(a.label, "nhãn mới");
    }
}

#[cfg(test)]
mod proxy_guard_tests {
    use super::*;

    #[test]
    fn tunnel_requests_are_not_local() {
        let mut h = axum::http::HeaderMap::new();
        assert!(!is_proxied(&h), "trình duyệt trên chính máy này không gắn header proxy");
        // cloudflared gắn cf-connecting-ip/cf-ray; nginx/caddy gắn x-forwarded-for
        for k in ["cf-connecting-ip", "cf-ray", "x-forwarded-for", "x-real-ip", "forwarded"] {
            h.clear();
            h.insert(k, axum::http::HeaderValue::from_static("1.2.3.4"));
            assert!(is_proxied(&h), "{k}");
        }
    }
}

#[cfg(test)]
mod extension_tests {
    use super::*;

    #[test]
    fn extension_zip_points_at_the_dashboard() {
        let bytes = extension_zip("127.0.0.1:8790").unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut read = |n: &str| { let mut s = String::new(); std::io::Read::read_to_string(&mut z.by_name(n).unwrap(), &mut s).unwrap(); s };
        let m: Value = serde_json::from_str(&read("whalegate-extension/manifest.json")).unwrap();
        assert_eq!(m["host_permissions"][1], "http://127.0.0.1/*");
        assert!(m["permissions"].as_array().unwrap().iter().any(|p| p == "cookies"));
        let bg = read("whalegate-extension/background.js");
        assert!(bg.contains(r#"const WHALEGATE = "http://127.0.0.1:8790";"#));
        assert!(!bg.contains("__WG_"));
        // host lạ (chèn ký tự vào JS/manifest) bị từ chối
        assert!(extension_zip("evil\";alert(1)//").is_err());
        assert!(extension_zip("").is_err());
        let v6 = extension_zip("[::1]:8790").unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(v6)).unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut z.by_name("whalegate-extension/manifest.json").unwrap(), &mut s).unwrap();
        assert!(s.contains("http://[::1]/*"));
    }
}

#[cfg(test)]
mod version_ui_tests {
    #[test]
    fn dashboard_shows_running_version() {
        let html = super::index_html();
        assert!(!html.contains("__WG_VERSION__"), "còn chỗ trống chưa thay");
        let v = env!("CARGO_PKG_VERSION");
        assert!(html.contains(&format!(">v{}</a>", v)));
        assert!(html.contains(&format!("/releases/tag/v{}", v)));
    }
}
