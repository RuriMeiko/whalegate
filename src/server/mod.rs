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
        // Cho phép đổi thư mục dữ liệu (test trong sandbox, chạy nhiều instance, đặt lên ổ khác).
        // Phải là đường dẫn tuyệt đối để không phụ thuộc cwd của tiến trình gọi.
        if let Ok(d) = std::env::var("WHALEGATE_DATA_DIR") {
            if !d.trim().is_empty() { return d; }
        }
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
    /// DeepSeek đã đình chỉ acc (đăng nhập trả USER_IS_BANNED / chat trả 40012): thời điểm phát hiện. Acc bị tắt khỏi pool,
    /// dashboard gạch ngang tên. Gỡ khi đăng nhập/verify lại thành công (`clear_auth_dead`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banned_at: Option<u64>,
    /// Cách acc được nối vào whalegate lần gần nhất: "chrome" (Chrome riêng) | "scan" (quét Chrome chính) | "bookmarklet" |
    /// "extension" | "paste" | "register" (đăng ký bằng CLI). None = acc cũ, suy ra từ dữ liệu có sẵn (xem `login_method_effective`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_method: Option<String>,
}

/// Chỉ nhận các giá trị đã biết từ client (tiện ích / bookmarklet / dán tay).
fn clean_via(v: &Option<String>) -> Option<String> {
    v.as_deref().map(str::trim).filter(|x| matches!(*x, "bookmarklet" | "extension" | "paste")).map(str::to_string)
}

impl Account {
    /// Phương thức hiển thị: giá trị đã lưu, hoặc suy ra cho acc cũ — có `browser` = trình duyệt gốc (bookmarklet hoặc
    /// tiện ích, không phân biệt được), có `fp_seed` = Chrome riêng, còn lại = dán token.
    pub(crate) fn login_method_effective(&self) -> &str {
        if let Some(m) = self.login_method.as_deref() { return m; }
        if self.browser.is_some() { "browser" } else if self.fp_seed.is_some() { "chrome" } else { "paste" }
    }

    pub(crate) fn set_auth_dead(&mut self) {
        if self.auth_dead_at.is_none() { self.auth_dead_at = Some(now()); }
        self.last_status = Some("DeepSeek đã đăng xuất token này (40003) — cần đăng nhập lại".into());
    }
    pub(crate) fn clear_auth_dead(&mut self) { self.auth_dead_at = None; self.banned_at = None; }
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
    // ---- rate limit của DeepSeek ("Tin nhắn quá thường xuyên") ----
    // số lần bị rate limit liên tiếp: cooldown 30s, 60s, 120s… tối đa 300s; một lượt thành công thì về 0
    #[serde(default)]
    pub rl_streak: u32,
    // trần request/phút học được từ lần bị chặn gần nhất (số request đã gửi trong 60s trước đó, trừ cái bị chặn); 0 = chưa học
    #[serde(default)]
    pub rl_cap: u32,
    #[serde(default)]
    pub rl_cap_at: u64,
    // thời điểm (giây) các call trong 60s gần nhất — chỉ trong bộ nhớ, để pace theo trần
    #[serde(skip)]
    pub recent: Vec<u64>,
}

/// Trần học được hết hạn sau chừng này giây không bị chặn nữa (DeepSeek có thể đã nới giới hạn).
const RL_CAP_TTL_SECS: u64 = 1800;

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

    /// Trần request/phút do người dùng đặt (DS_MAX_RPM); 0 = tự học từ các lần bị rate limit.
    pub fn max_rpm() -> u32 { crate::settings::uint("DS_MAX_RPM") as u32 }

    /// Trần đang áp dụng cho acc này: cài đặt thủ công, hoặc trần học được còn hạn.
    pub(crate) fn rpm_cap(&self, now: u64) -> Option<u32> {
        let m = Self::max_rpm();
        if m > 0 { return Some(m); }
        if self.rl_cap > 0 && now.saturating_sub(self.rl_cap_at) < RL_CAP_TTL_SECS { Some(self.rl_cap) } else { None }
    }

    pub(crate) fn calls_last_minute(&self, now: u64) -> u32 {
        self.recent.iter().filter(|t| now.saturating_sub(**t) < 60).count() as u32
    }

    fn rpm_capped(&self, now: u64) -> bool {
        self.rpm_cap(now).is_some_and(|c| self.calls_last_minute(now) >= c)
    }

    pub(crate) fn available(&self, now: u64) -> bool {
        now >= self.cooldown_until && !self.burst_active(now) && !self.hourly_capped(now) && !self.rpm_capped(now)
    }

    /// Bao nhiêu giây nữa acc dùng lại được (0 = đang dùng được). `u64::MAX` nếu phải đợi tới hết giờ (trần lượt/giờ).
    /// Dùng để lượt nối tiếp phiên chờ đúng acc giữ phiên thay vì bỏ phiên và gửi lại cả ngữ cảnh sang acc khác.
    pub(crate) fn wait_secs(&self, now: u64) -> u64 {
        if self.hourly_capped(now) { return u64::MAX; }
        let mut w = self.cooldown_until.saturating_sub(now);
        if self.burst_active(now) {
            w = w.max((self.burst_window_start + Self::burst_window()).saturating_sub(now));
        }
        if let Some(cap) = self.rpm_cap(now) {
            let live: Vec<u64> = self.recent.iter().copied().filter(|t| now.saturating_sub(*t) < 60).collect();
            if live.len() as u32 >= cap {
                // phải có (đang có - trần + 1) call trôi khỏi cửa sổ 60s: call thứ (đang có - trần) hết hạn sau 60s
                let idx = live.len() - cap as usize;
                w = w.max((live[idx] + 60).saturating_sub(now));
            }
        }
        w
    }

    /// DeepSeek báo rate limit: nghỉ 30s, 60s, 120s… (tối đa 300s) và học trần request/phút để lần sau tự giãn nhịp
    /// trước khi bị chặn. Trần chỉ được hạ trong thời hạn hiệu lực (không nới lên khi lại bị chặn). Trả (nghỉ giây, trần).
    pub(crate) fn note_rate_limit(&mut self, now: u64) -> (u64, u32) {
        self.rl_streak = self.rl_streak.saturating_add(1);
        let learned = self.calls_last_minute(now).saturating_sub(1).max(3);
        self.rl_cap = if self.rl_cap > 0 && now.saturating_sub(self.rl_cap_at) < RL_CAP_TTL_SECS { self.rl_cap.min(learned) } else { learned };
        self.rl_cap_at = now;
        let cd = (30u64 << (self.rl_streak - 1).min(4)).min(300);
        self.cooldown_until = now + cd;
        (cd, self.rl_cap)
    }

    /// Xoá sạch trạng thái lỗi/cooldown/burst (nút Reset trên UI, hoặc sau khi đăng nhập lại).
    pub(crate) fn reset(&mut self) {
        self.rl_streak = 0;
        self.rl_cap = 0;
        self.fail_streak = 0;
        self.cooldown_until = 0;
        self.burst_count = 0;
        self.burst_window_start = 0;
    }

    pub(crate) fn mark_ok(&mut self) {
        self.rl_streak = 0;
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
        self.recent.retain(|t| now.saturating_sub(*t) < 60);
        self.recent.push(now);
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
    // Bind vân tay của MỌI acc trước khi dựng client: client phân giải proxy/vân tay qua
    // fingerprint::seed_for(token), mà hàm đó chỉ biết seed nếu đã bind. Thiếu bước này thì acc
    // thêm bằng bookmarklet bị coi là "máy" khác (seed = token) → ghim proxy sai và emulation sai.
    for a in &store.accounts {
        crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
        crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
        crate::fingerprint::bind_profile(&a.token, a.browser.clone());
    }
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
pub(crate) async fn write_store_atomic_pub(store: &Store) -> Result<()> {
    write_store_atomic(store).await
}

async fn write_store_atomic(store: &Store) -> Result<()> {
    write_store_bytes(serde_json::to_vec_pretty(store)?).await
}

async fn write_store_bytes(json: Vec<u8>) -> Result<()> {
    let path = std::path::PathBuf::from(creds_path());
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    tokio::fs::create_dir_all(&dir).await?;
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
    // serialize ngay dưới read lock (không clone cả Store), nhả lock trước khi ghi file
    let json = match serde_json::to_vec_pretty(&*st.store.read().await) {
        Ok(j) => j,
        Err(e) => { crate::elog!("[store] serialize accounts.json lỗi: {:#}", e); return; }
    };
    match write_store_bytes(json).await {
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
    /// lý do DeepSeek từ chối lần đăng nhập gần nhất trong cửa sổ Chrome (thời điểm, nội dung) — dashboard hiện lên
    pub(crate) login_notice: std::sync::Mutex<Option<(u64, String)>>,
    // (acc_id, sha256 ảnh) -> file id DS đã upload (id gắn theo acc nên cache theo acc)
    // hội thoại stateful: dấu vân tay prefix -> phiên web đang giữ ngữ cảnh
    pub(crate) convs: std::sync::Mutex<std::collections::HashMap<u64, Conv>>,
    // MỌI phiên web do harness tạo cho hội thoại: sid -> (acc, lần dùng cuối). Quét xoá theo TTL kể cả khi hội thoại bị bỏ dở.
    pub(crate) sessions: std::sync::Mutex<std::collections::HashMap<String, (String, u64)>>,
    pub(crate) stat_conv_hit: std::sync::atomic::AtomicU64,
    pub(crate) stat_conv_miss: std::sync::atomic::AtomicU64,
    pub(crate) file_cache: std::sync::Mutex<std::collections::HashMap<(String, String), String>>,
}


mod accounts;
mod admin;
mod dsml;
mod login;
mod openai;
#[allow(unused_imports)] pub(crate) use accounts::*;
#[allow(unused_imports)] pub(crate) use admin::*;
#[allow(unused_imports)] pub(crate) use dsml::*;
#[allow(unused_imports)] pub(crate) use login::*;
#[allow(unused_imports)] pub(crate) use openai::*;

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
        login_notice: std::sync::Mutex::new(None),
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
        .route("/api/backup/export", get(crate::backup::api_backup_export))
        .route("/api/backup/import", post(crate::backup::api_backup_import).layer(axum::extract::DefaultBodyLimit::max(600 << 20)))
        .route("/api/accounts/{id}", axum::routing::delete(api_delete_account).patch(api_edit_account))
        .route("/api/accounts/{id}/verify", post(api_verify_account))
        .route("/api/accounts/{id}/pool", post(api_toggle_pool))
        .route("/api/accounts/{id}/reset", post(api_reset_account))
        .route("/api/accounts/{id}/device", post(api_account_device))
        .route("/api/cf", get(|| async { Json(crate::cloudflare::status()) }))
        .route("/api/cf/setup", post(|| async { cf_result(crate::cloudflare::start_setup(false)) }))
        .route("/api/cf/add", post(|| async { cf_result(crate::cloudflare::start_setup(true)) }))
        .route("/api/cf/remove", post(|| async { cf_result(crate::cloudflare::start_remove()) }))
        .route("/api/cf/refresh", post(|| async { cf_result(crate::cloudflare::start_refresh()) }))
        .route("/api/accounts/{id}/relogin", post(api_account_relogin))
        .route("/api/accounts/{id}/open_chrome", post(api_account_open_chrome))
        .route("/api/extension.zip", get(api_extension_zip))
        .route("/api/capture", post(api_capture_from_chrome))
        .route("/api/login_start", post(api_login_start))
        .route("/api/login_auto", post(api_login_auto))
        .route("/api/proxy", get(api_proxy_get).post(api_proxy_set))
        .route("/api/proxy/unbind", post(api_proxy_unbind))
        .route("/api/stats", get(api_stats))
        .route("/api/usage", get(api_usage))
        .route("/api/logs", get(api_logs))
        .route("/api/logs/stream", get(api_logs_stream))
        .route("/api/logs/export", get(api_logs_export))
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
    tokio::spawn(crate::egress::start());
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
mod rate_limit_tests {
    use super::*;

    #[test]
    fn wait_secs_tells_how_long_until_the_account_is_usable() {
        let mut a = AccStats::default();
        assert_eq!(a.wait_secs(1000), 0);
        // 6 call dồn trong 11s (đúng ca trong file debug): burst 6/30s chặn tới hết cửa sổ 30s, không phải 10s cooldown
        for t in [1000u64, 1002, 1004, 1007, 1009, 1011] { a.record_call(t); }
        assert!(!a.available(1012));
        assert_eq!(a.wait_secs(1012), 1000 + 30 - 1012, "đợi tới hết cửa sổ burst");
        assert!(a.available(1031) && a.wait_secs(1031) == 0);
        // cooldown dài (challenge/mute) thì báo đúng số giây còn lại
        let mut b = AccStats::default();
        b.cooldown_until = 5000;
        assert_eq!(b.wait_secs(4000), 1000);
        // trần request/phút
        let mut c = AccStats::default();
        c.rl_cap = 3; c.rl_cap_at = 1000;
        c.recent = vec![1000, 1010, 1020];
        assert_eq!(c.wait_secs(1030), 30, "call cũ nhất (1000) hết hạn ở 1060");
    }

    fn calls(a: &mut AccStats, start: u64, n: u32, gap: u64) -> u64 {
        let mut t = start;
        for _ in 0..n { a.record_call(t); t += gap; }
        t
    }

    #[test]
    fn rate_limit_is_not_a_failure_and_backs_off_exponentially() {
        let mut a = AccStats::default();
        let t = calls(&mut a, 1000, 11, 3); // 11 request trong ~30s
        let (cd1, cap1) = a.note_rate_limit(t);
        assert_eq!(cd1, 30);
        assert_eq!(a.fail_streak, 0, "rate limit không phải lỗi của acc");
        assert_eq!(cap1, 10, "học trần = số request đã gửi trừ cái bị chặn");
        assert!(!a.available(t + 29) && a.available(t + 31 + 60), "hết cooldown thì dùng lại được (khi cửa sổ 60s đã trống)");
        assert_eq!(a.note_rate_limit(t + 100).0, 60);
        assert_eq!(a.note_rate_limit(t + 200).0, 120);
        for i in 0..10 { a.note_rate_limit(t + 300 + i); }
        assert_eq!(a.cooldown_until - (t + 309), 300, "tối đa 300s");
    }

    #[test]
    fn learned_cap_spaces_out_calls_before_the_next_block() {
        let mut a = AccStats::default();
        let t = calls(&mut a, 1000, 11, 1); // 11 call dồn trong 10s, call cuối ở 1010, t = 1011
        assert_eq!(a.note_rate_limit(t).1, 10);
        assert!(!a.available(a.cooldown_until), "cooldown hết nhưng 60s gần nhất vẫn còn 11 call >= trần 10 → chờ");
        assert!(!a.available(1060), "còn đúng 10 call trong cửa sổ → vẫn chờ");
        assert!(a.available(1061), "call đầu trôi khỏi cửa sổ 60s (còn 9 < 10) → dùng lại");
        // gửi tới đúng trần thì chặn, dưới trần thì cho
        let mut b = AccStats::default();
        b.rl_cap = 5; b.rl_cap_at = 1000;
        let mut now = 1000;
        for _ in 0..4 { b.record_call(now); now += 1; }
        assert!(b.available(now));
        b.record_call(now);
        assert!(!b.available(now + 1), "đủ 5 call trong 60s");
    }

    #[test]
    fn learned_cap_only_goes_down_and_expires() {
        let mut a = AccStats::default();
        a.rl_cap = 8; a.rl_cap_at = 1000;
        let t = calls(&mut a, 1000, 20, 1);
        a.note_rate_limit(t);
        assert_eq!(a.rl_cap, 8, "không nới trần khi vừa bị chặn lại");
        a.rl_cap = 4; a.rl_cap_at = 1000;
        assert_eq!(a.rpm_cap(1000 + 100), Some(4));
        assert_eq!(a.rpm_cap(1000 + 1800), None, "trần hết hạn sau 30 phút để thử nới");
    }

    #[test]
    fn success_clears_streak_and_reset_clears_cap() {
        let mut a = AccStats::default();
        let t = calls(&mut a, 1000, 6, 2);
        a.note_rate_limit(t); a.note_rate_limit(t + 40);
        assert_eq!(a.rl_streak, 2);
        a.mark_ok();
        assert_eq!(a.rl_streak, 0);
        assert!(a.rl_cap >= 3, "mark_ok giữ trần đã học");
        a.reset();
        assert_eq!((a.rl_streak, a.rl_cap, a.cooldown_until), (0, 0, 0));
    }

    #[test]
    fn learned_cap_never_below_three() {
        let mut a = AccStats::default();
        let t = calls(&mut a, 1000, 1, 1);
        assert_eq!(a.note_rate_limit(t).1, 3);
    }
}

#[cfg(test)]
mod login_method_tests {
    use super::*;

    fn base() -> Account {
        serde_json::from_value(json!({"id": "x", "label": "x", "token": "t".repeat(24), "cookies": {}, "added_at": 0,
            "last_verified": null, "last_status": null, "thread_session_id": null, "thread_parent_message_id": null})).unwrap()
    }

    #[test]
    fn legacy_accounts_get_an_inferred_method() {
        let mut a = base();
        assert_eq!(a.login_method_effective(), "paste");
        a.fp_seed = Some("seed".into());
        assert_eq!(a.login_method_effective(), "chrome");
        a.browser = Some(Default::default());
        assert_eq!(a.login_method_effective(), "browser", "có hồ sơ trình duyệt gốc: bookmarklet hoặc tiện ích, chưa phân biệt được");
        a.login_method = Some("extension".into());
        assert_eq!(a.login_method_effective(), "extension", "giá trị đã lưu thắng suy luận");
    }

    #[test]
    fn method_survives_export_and_old_bundles_still_load() {
        let mut a = base();
        a.login_method = Some("bookmarklet".into());
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["login_method"], "bookmarklet");
        let back: Account = serde_json::from_value(v).unwrap();
        assert_eq!(back.login_method.as_deref(), Some("bookmarklet"));
        // acc chưa có phương thức không in trường (gói cũ y như trước)
        assert!(serde_json::to_value(base()).unwrap().get("login_method").is_none());
    }

    #[test]
    fn via_from_clients_is_whitelisted() {
        assert_eq!(clean_via(&Some(" extension ".into())).as_deref(), Some("extension"));
        assert_eq!(clean_via(&Some("bookmarklet".into())).as_deref(), Some("bookmarklet"));
        assert_eq!(clean_via(&Some("paste".into())).as_deref(), Some("paste"));
        assert_eq!(clean_via(&Some("chrome".into())), None, "chrome/scan/register do server tự đặt, client không được khai");
        assert_eq!(clean_via(&Some("<script>".into())), None);
        assert_eq!(clean_via(&None), None);
    }
}

