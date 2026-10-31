// Chrome và DeepSeek qua CDP.
// - Chrome riêng của từng acc (profile + vân tay riêng, cổng CDP ngẫu nhiên):
//   * login: mở cửa sổ sign_in → user login kiểu gì cũng được → canh 1s/lần → thấy token → verify → lưu → tắt ngay.
//   * dán token: mở nền (headless), ghi token vào localStorage → DeepSeek sinh cookie + device id → lưu → tắt.
// - Chrome chính của user (cổng 9333): quét tab đã login, làm mới cookie định kỳ.
use anyhow::{bail, Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

pub struct CapturedSession {
    pub token: String,
    pub cookies: std::collections::BTreeMap<String, String>,
    pub email: Option<String>,
    /// id người dùng DeepSeek (ổn định giữa các lần đăng nhập, dùng để nhận ra cùng một người)
    pub uid: Option<String>,
    /// seed vân tay của acc (Some khi login trong Chrome riêng của acc; None khi bắt từ Chrome chính)
    pub fp_seed: Option<String>,
    /// `deepseek-device-id:chat` của Chrome đã login — định danh thiết bị mà cookie smidV2/WAF đã gắn vào
    pub device_id: Option<String>,
}

const MAIN_CDP_PORT: u16 = 9333;

fn cdp_http(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Cổng CDP cho Chrome riêng của acc: mỗi lần mở chọn ngẫu nhiên một cổng trống trong 40000–60000 (tránh các cổng
/// debug phổ biến như 9222/9333 mà tool khác hay dò). DS_LOGIN_CDP_PORT ép một cổng cố định nếu cần.
fn pick_cdp_port() -> Result<u16> {
    if let Some(p) = std::env::var("DS_LOGIN_CDP_PORT").ok().and_then(|v| v.parse().ok()) {
        return Ok(p);
    }
    for _ in 0..50 {
        let r = u16::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..2].try_into().unwrap_or([0, 0]));
        let port = 40000 + r % 20000;
        // bind thử rồi nhả: cổng trống thì Chrome lấy được ngay sau đó
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    bail!("không tìm được cổng CDP trống trong 40000–60000")
}

/// Tìm binary Chrome trên máy: DS_CHROME_BIN → chỗ cài mặc định của từng hệ → (Linux/macOS) tên lệnh trong PATH.
/// Ưu tiên Chrome, rồi mới tới Chromium/Edge/Brave (cùng nhân Chromium, CDP như nhau).
fn find_chrome_binary() -> Result<String> {
    if let Ok(bin) = std::env::var("DS_CHROME_BIN") {
        if !bin.trim().is_empty() { return Ok(bin); }
    }
    if let Some(p) = chrome_candidates().into_iter().find(|p| std::path::Path::new(p).is_file()) {
        return Ok(p);
    }
    #[cfg(windows)]
    if let Some(p) = chrome_from_registry() {
        return Ok(p);
    }
    // Windows không dò PATH: `chrome.exe --version` trên Windows không in phiên bản mà MỞ CỬA SỔ trình duyệt
    if !cfg!(windows) {
        for bin in ["google-chrome-stable", "google-chrome", "chromium-browser", "chromium", "microsoft-edge", "brave-browser"] {
            let ok = std::process::Command::new(bin)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return Ok(bin.to_string());
            }
        }
    }
    bail!("không tìm thấy Chrome/Chromium/Edge trên máy — cài Google Chrome hoặc đặt DS_CHROME_BIN")
}

/// Đường dẫn cài đặt mặc định, theo thứ tự ưu tiên, cho hệ đang chạy.
fn chrome_candidates() -> Vec<String> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        // cài cho mọi người (/Applications) hoặc chỉ cho user (~/Applications); binary nằm trong .app cùng tên
        let home = std::env::var("HOME").unwrap_or_default();
        for app in ["Google Chrome", "Chromium", "Microsoft Edge", "Brave Browser"] {
            for root in ["/Applications".to_string(), format!("{}/Applications", home)] {
                out.push(format!("{}/{}.app/Contents/MacOS/{}", root, app, app));
            }
        }
    } else if cfg!(windows) {
        // theo biến môi trường chứ không cố định ổ C: (Windows cài ở ổ khác, hoặc Chrome cài riêng cho user ở LOCALAPPDATA)
        let roots: Vec<String> = ["PROGRAMFILES", "PROGRAMFILES(X86)", "ProgramW6432", "LOCALAPPDATA"].iter()
            .filter_map(|k| std::env::var(k).ok()).filter(|v| !v.is_empty()).collect();
        for rel in [r"Google\Chrome\Application\chrome.exe", r"Chromium\Application\chrome.exe",
                    r"Microsoft\Edge\Application\msedge.exe", r"BraveSoftware\Brave-Browser\Application\brave.exe"] {
            for root in &roots {
                let p = format!(r"{}\{}", root.trim_end_matches('\\'), rel);
                if !out.contains(&p) { out.push(p); }
            }
        }
    }
    out
}

/// Chrome tự đăng ký đường dẫn ở `App Paths` khi cài (kể cả cài vào chỗ lạ) — đọc bằng `reg query`, không mở cửa sổ.
#[cfg(windows)]
fn chrome_from_registry() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    for exe in ["chrome.exe", "msedge.exe"] {
        for hive in ["HKCU", "HKLM"] {
            let key = format!(r"{}\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}", hive, exe);
            let Ok(o) = std::process::Command::new("reg").args(["query", &key, "/ve"]).creation_flags(CREATE_NO_WINDOW).output() else { continue };
            if let Some(p) = parse_reg_default(&String::from_utf8_lossy(&o.stdout)) {
                if std::path::Path::new(&p).is_file() { return Some(p); }
            }
        }
    }
    None
}

/// Giá trị mặc định trong output `reg query … /ve`: dòng `    (Default)    REG_SZ    C:\…\chrome.exe`
/// (tên "(Default)" đổi theo ngôn ngữ Windows, nên chỉ dựa vào REG_SZ / REG_EXPAND_SZ).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_reg_default(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let (_, v) = l.split_once("REG_SZ").or_else(|| l.split_once("REG_EXPAND_SZ"))?;
        let v = v.trim().trim_matches('"');
        (!v.is_empty()).then(|| v.to_string())
    })
}

struct CdpConn {
    ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    next_id: u64,
}

impl CdpConn {
    async fn connect(url: &str) -> Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        Ok(Self { ws, next_id: 0 })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.ws
            .send(Message::Text(
                json!({"id": id, "method": method, "params": params}).to_string(),
            ))
            .await?;
        while let Some(msg) = self.ws.next().await {
            let msg = msg?;
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t)?;
                if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                    if let Some(err) = v.get("error") {
                        bail!("CDP {} error: {}", method, err);
                    }
                    return Ok(v["result"].clone());
                }
            }
        }
        bail!("CDP connection closed")
    }
}

async fn browser_ws_url(port: u16) -> Result<String> {
    let d: Value = reqwest::get(format!("{}/json/version", cdp_http(port)))
        .await?
        .json()
        .await
        .with_context(|| format!("không kết nối được Chrome CDP :{}", port))?;
    Ok(d["webSocketDebuggerUrl"]
        .as_str()
        .context("no browser ws")?
        .to_string())
}

async fn list_pages(port: u16) -> Result<Vec<Value>> {
    let body = reqwest::get(format!("{}/json/list", cdp_http(port))).await?.text().await?;
    Ok(serde_json::from_str(&body)?)
}

/// Đọc token + cookies từ 1 tab theo ws url. Err nếu tab chưa login / tab đóng.
pub async fn read_session_from_page(
    ws_url: &str,
) -> Result<(String, std::collections::BTreeMap<String, String>, Option<String>)> {
    let mut conn = CdpConn::connect(ws_url).await?;
    let expr = r#"(() => {
        const t = localStorage.getItem('userToken');
        if (!t) return {token: null, device: null};
        return {token: JSON.parse(t).value, device: localStorage.getItem('deepseek-device-id:chat')};
    })()"#;
    let res = conn
        .call("Runtime.evaluate", json!({"expression": expr, "returnByValue": true}))
        .await?;
    let value = &res["result"]["value"];
    let token = value["token"]
        .as_str()
        .context("tab chưa login (userToken rỗng)")?
        .to_string();
    let device_id = value["device"].as_str().filter(|d| !d.is_empty()).map(|d| d.to_string());
    // Network.getCookies thấy cả cookie httpOnly (ds_session_id) mà document.cookie không đọc được
    let ck = conn
        .call("Network.getCookies", json!({"urls": ["https://chat.deepseek.com"]}))
        .await?;
    let mut cookies = std::collections::BTreeMap::new();
    if let Some(arr) = ck["cookies"].as_array() {
        for c in arr {
            if let (Some(name), Some(v)) = (c["name"].as_str(), c["value"].as_str()) {
                cookies.insert(name.to_string(), v.to_string());
            }
        }
    }
    Ok((token, cookies, device_id))
}

async fn verify_and_build(
    token: String,
    cookies: std::collections::BTreeMap<String, String>,
    device_id: Option<String>,
    fp_seed: Option<&str>,
) -> Result<CapturedSession> {
    // gắn vân tay của acc TRƯỚC khi tạo client, để lần xác minh đầu đã đi bằng đúng "trình duyệt" của acc
    crate::fingerprint::bind(&token, fp_seed);
    crate::fingerprint::bind_device(&token, device_id.as_deref());
    let cookie_string = cookies
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("; ");
    let client = crate::client::DsClient::new(token.clone(), cookie_string)?;
    let me = client.users_current().await?;
    if me["code"].as_i64() != Some(0) {
        bail!("token không hợp lệ: {}", me["msg"]);
    }
    let email = me["data"]["biz_data"]["email"].as_str().map(|s| s.to_string());
    let uid = me["data"]["biz_data"]["id"].as_str().map(|s| s.to_string());
    Ok(CapturedSession { token, cookies, email, uid, fp_seed: fp_seed.map(|s| s.to_string()), device_id })
}

/// CANH 1 tab login: poll 1s/lần tới khi tab có token (login xong) hoặc hết giờ.
/// Verify token rồi trả session — caller tự đóng tab sau khi lưu.
pub async fn watch_login_tab(ws_url: String, timeout_secs: u64, port: u16, fp_seed: &str) -> Result<CapturedSession> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    // Tab sign_in đôi khi KHÔNG redirect (profile chính đã login, DS đứng yên ở form).
    // → poll MỌI tab DS trong Chrome: tab nào verify được session là lấy, ưu tiên tab watch.
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("hết {}s mà chưa login xong trong tab", timeout_secs);
        }
        // user tự đóng cửa sổ: dừng canh ngay, không giữ khoá login thêm tới hết 10 phút
        if reqwest::get(format!("{}/json/version", cdp_http(port))).await.is_err() {
            bail!("cửa sổ đăng nhập đã bị đóng trước khi login xong");
        }
        // 1) thử tab watch trước
        if let Ok((token, cookies, device)) = read_session_from_page(&ws_url).await {
            if let Ok(s) = verify_and_build(token, cookies, device, Some(fp_seed)).await {
                return Ok(s);
            }
        }
        // 2) quét mọi tab DS (user có thể login trong tab khác / sign_in đứng yên)
        if let Ok(sessions) = capture_ds_tabs_on(port, Some(fp_seed)).await {
            if let Some(s) = sessions.into_iter().next() {
                return Ok(s);
            }
        }
        // 1s: login xong là thấy gần như ngay, caller tắt cửa sổ liền sau khi xác minh
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Quét mọi tab DS đang mở (phụ — dùng khi user tự login trong tab riêng).
pub async fn capture_all_ds_tabs() -> Result<Vec<CapturedSession>> {
    capture_ds_tabs_on(MAIN_CDP_PORT, None).await
}

async fn capture_ds_tabs_on(port: u16, fp_seed: Option<&str>) -> Result<Vec<CapturedSession>> {
    let pages = list_pages(port).await.with_context(|| format!("không kết nối được Chrome CDP :{}", port))?;
    let mut out: Vec<CapturedSession> = Vec::new();
    for p in pages.iter().filter(|p| {
        p["type"] == "page"
            && p["url"].as_str().map(|u| u.contains("chat.deepseek.com")).unwrap_or(false)
    }) {
        let Some(ws_url) = p["webSocketDebuggerUrl"].as_str() else { continue };
        if let Ok((token, cookies, device)) = read_session_from_page(ws_url).await {
            if out.iter().any(|s| s.token == token) {
                continue; // cùng acc mở 2 tab
            }
            if let Ok(s) = verify_and_build(token, cookies, device, fp_seed).await {
                out.push(s);
            }
        }
    }
    Ok(out)
}

/// Refresh cookies từ tab DS trong Chrome (WAF token xoay — cần cập nhật định kỳ).
/// Refresh session từ Chrome chính: trả (cookies, userToken hiện tại nếu đọc được).
/// userToken từ localStorage — nếu acc re-login bằng tay thì token mới được bắt ở đây.
pub async fn refresh_cookies_from_chrome()
    -> Result<(std::collections::BTreeMap<String, String>, Option<String>)>
{
    let pages = list_pages(MAIN_CDP_PORT).await.context("không kết nối được Chrome CDP :9333")?;
    // ưu tiên tab /a/chat (đã login); bỏ tab sign_in đứng yên (token rỗng)
    let ds = pages
        .iter()
        .find(|p| {
            p["type"] == "page"
                && p["url"]
                    .as_str()
                    .map(|u| u.contains("chat.deepseek.com") && (u.contains("/a/chat") || u.contains("/a/chats")))
                    .unwrap_or(false)
        })
        .or_else(|| {
            pages.iter().find(|p| {
                p["type"] == "page"
                    && p["url"].as_str().map(|u| u.contains("chat.deepseek.com")).unwrap_or(false)
            })
        })
        .context("không có tab chat.deepseek.com để refresh cookies")?;
    let ws_url = ds["webSocketDebuggerUrl"].as_str().context("no ws url")?.to_string();
    let mut conn = CdpConn::connect(&ws_url).await?;
    let res = conn
        .call(
            "Network.getCookies",
            json!({"urls": ["https://chat.deepseek.com"]}),
        )
        .await?;
    let mut cookies = std::collections::BTreeMap::new();
    if let Some(arr) = res["cookies"].as_array() {
        for c in arr {
            if let (Some(name), Some(value)) = (c["name"].as_str(), c["value"].as_str()) {
                cookies.insert(name.to_string(), value.to_string());
            }
        }
    }
    // đọc userToken (format JSON wrapper {"value":..,"__version":..}) — parse value
    let tok = conn
        .call(
            "Runtime.evaluate",
            json!({"expression": r#"(JSON.parse(localStorage.getItem('userToken')||'{}').value||"")"#, "returnByValue": true}),
        )
        .await
        .ok()
        .and_then(|r| r["result"]["value"].as_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty());
    Ok((cookies, tok))
}


// ---------- CHROME RIÊNG CỦA TỪNG ACC: mỗi acc một profile + một vân tay ----------

pub struct AccountBrowser {
    child: std::process::Child,
    pub port: u16,
    injector: tokio::task::JoinHandle<()>,
    // giữ khoá cổng CDP login tới khi đóng: login tay và làm mới nền xếp hàng thay vì giẫm lên nhau
    _slot: tokio::sync::OwnedMutexGuard<()>,
}

/// Chỉ một Chrome riêng của acc chạy tại một thời điểm (chung cổng CDP login).
fn browser_slot() -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static S: std::sync::OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> = std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(()))).clone()
}

impl AccountBrowser {
    /// Tắt Chrome, GIỮ profile (cookie/localStorage/IndexedDB của acc nằm ở đó). Xin Chrome tự đóng qua CDP
    /// `Browser.close` để nó kịp ghi cookie/phiên Google xuống profile — kill ngang (nhất là TerminateProcess trên
    /// Windows) có thể mất phần chưa ghi, mà profile này còn dùng lại cho "đăng nhập lại" và làm mới thiết bị.
    /// Trả true nếu Chrome tự đóng (không phải kill).
    pub async fn close(mut self) -> bool {
        self.injector.abort();
        if let Ok(Ok(ws)) = tokio::time::timeout(std::time::Duration::from_secs(2), browser_ws_url(self.port)).await {
            if let Ok(Ok(mut c)) = tokio::time::timeout(std::time::Duration::from_secs(2), CdpConn::connect(&ws)).await {
                // Chrome đóng kết nối ngay khi nhận lệnh, nên lỗi "connection closed" ở đây là bình thường
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.call("Browser.close", json!({}))).await;
            }
        }
        for _ in 0..30 {
            if let Ok(Some(_)) = self.child.try_wait() { return true; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        false
    }
}

/// Thư mục profile Chrome của một acc: cố định theo seed để lần sau mở lại đúng "máy" đó.
pub fn account_profile_dir(seed: &str) -> String {
    format!("{}/profiles/{}", crate::server::data_dir(), seed)
}

/// Biến môi trường màn hình cho Chrome có cửa sổ. Dịch vụ systemd thường khởi động trước phiên đồ hoạ nên tiến trình
/// không có DISPLAY/WAYLAND_DISPLAY (Chrome mở cửa sổ sẽ thoát ngay); lấy lại từ systemd user — phiên đồ hoạ import
/// chúng vào đó khi đăng nhập — không được thì dò socket Wayland/X11. Đã có sẵn trong môi trường thì không đụng.
fn gui_env() -> Vec<(String, String)> {
    if !cfg!(target_os = "linux") { return Vec::new(); }
    let has = |k: &str| std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false);
    if has("DISPLAY") || has("WAYLAND_DISPLAY") { return Vec::new(); }
    let mut out: Vec<(String, String)> = Vec::new();
    if let Ok(o) = std::process::Command::new("systemctl").args(["--user", "show-environment"]).output() {
        for l in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some((k, v)) = l.split_once('=') {
                if matches!(k, "DISPLAY" | "WAYLAND_DISPLAY" | "XAUTHORITY" | "XDG_SESSION_TYPE") && !v.is_empty() {
                    out.push((k.to_string(), v.to_string()));
                }
            }
        }
    }
    if !out.iter().any(|(k, _)| k == "DISPLAY" || k == "WAYLAND_DISPLAY") {
        let rt = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        if !rt.is_empty() && std::path::Path::new(&rt).join("wayland-0").exists() {
            out.push(("WAYLAND_DISPLAY".into(), "wayland-0".into()));
        }
        if let Some(n) = (0..10).find(|n| std::path::Path::new(&format!("/tmp/.X11-unix/X{}", n)).exists()) {
            out.push(("DISPLAY".into(), format!(":{}", n)));
        }
    }
    out
}

/// Không có màn hình nào để mở cửa sổ (máy chủ không giao diện).
fn gui_env_missing() -> bool {
    let has = |k: &str| std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false);
    !has("DISPLAY") && !has("WAYLAND_DISPLAY") && !gui_env().iter().any(|(k, _)| k == "DISPLAY" || k == "WAYLAND_DISPLAY")
}

/// Mở Chrome riêng cho acc (profile riêng, cổng CDP login), áp vân tay lên MỌI tab/iframe mở ra
/// (UA + client hints, múi giờ, canvas/WebGL/audio/màn hình/phần cứng) rồi đưa tab đầu tới `start_url`.
/// Trả (browser, ws url của tab đầu).
pub async fn launch_account_chrome(fp: &crate::fingerprint::Fingerprint, start_url: &str, headless: bool) -> Result<(AccountBrowser, String)> {
    let slot = browser_slot().lock_owned().await;
    let port = pick_cdp_port()?;
    let bin = find_chrome_binary()?;
    let profile = account_profile_dir(&fp.seed);
    std::fs::create_dir_all(&profile).ok();
    let (w, h) = fp.window_size();
    let mut cmd = std::process::Command::new(&bin);
    if headless {
        // headless "new" là Chrome đầy đủ; UA "HeadlessChrome" bị vân tay ghi đè trên mọi tab
        cmd.arg("--headless=new");
    } else {
        for (k, v) in gui_env() { cmd.env(k, v); }
    }
    // stderr của Chrome vào file trong profile: Chrome chết lúc khởi động thì lý do nằm ở đây
    let err_log = std::path::Path::new(&profile).join("chrome-stderr.log");
    let stderr = std::fs::File::create(&err_log).map(std::process::Stdio::from).unwrap_or_else(|_| std::process::Stdio::null());
    let mut child = cmd
        .arg(format!("--remote-debugging-port={}", port))
        .arg(format!("--user-data-dir={}", profile))
        .arg(format!("--window-size={},{}", w, h))
        .arg("--lang=vi-VN")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-features=Translate")
        .arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .spawn()
        .context(format!("không spawn được Chrome ({})", bin))?;
    let mut ready = false;
    let mut exited = None;
    for _ in 0..40 {
        if let Ok(resp) = reqwest::get(format!("{}/json/version", cdp_http(port))).await {
            if resp.status().is_success() { ready = true; break; }
        }
        // Chrome thoát luôn (thiếu màn hình, profile bị khoá…) thì báo ngay, không đợi đủ 20s
        if let Ok(Some(st)) = child.try_wait() { exited = Some(st); break; }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if !ready {
        let _ = child.kill();
        let _ = child.wait();
        let tail: Vec<String> = std::fs::read_to_string(&err_log).unwrap_or_default().lines()
            .filter(|l| !l.trim().is_empty()).map(|l| l.to_string()).collect();
        let tail = tail[tail.len().saturating_sub(4)..].join(" | ");
        let why = match exited {
            Some(st) => format!("Chrome thoát ngay khi khởi động ({})", st),
            None => format!("CDP :{} không sẵn sàng sau 20s", port),
        };
        let hint = if !headless && cfg!(target_os = "linux") && gui_env_missing() {
            " — không tìm thấy màn hình (DISPLAY/WAYLAND_DISPLAY) để mở cửa sổ; dùng cách dán token"
        } else { "" };
        bail!("{}{}{}", why, if tail.is_empty() { String::new() } else { format!(": {}", tail) }, hint);
    }

    let ws = browser_ws_url(port).await?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let fp2 = fp.clone();
    let start = start_url.to_string();
    let injector = tokio::spawn(async move {
        if let Err(e) = run_injector(ws, fp2, start, ready_tx).await {
            crate::elog!("[fingerprint] injector dừng: {}", e);
        }
    });
    let browser = AccountBrowser { child, port, injector, _slot: slot };
    if tokio::time::timeout(std::time::Duration::from_secs(15), ready_rx).await.map_err(|_| ()).and_then(|r| r.map_err(|_| ())).is_err() {
        browser.close().await;
        bail!("không áp được vân tay lên tab đầu trong 15s");
    }

    // tab đầu đã được áp vân tay và điều hướng; lấy ws của nó (khớp theo host của URL khởi đầu)
    let host = start_url.split("//").nth(1).unwrap_or(start_url).split('/').next().unwrap_or("").to_string();
    for _ in 0..20 {
        if let Ok(pages) = list_pages(port).await {
            if let Some(u) = pages.iter().find(|p| p["type"] == "page" && p["url"].as_str().map(|u| u.contains(host.as_str())).unwrap_or(false)).and_then(|p| p["webSocketDebuggerUrl"].as_str()) {
                return Ok((browser, u.to_string()));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    browser.close().await;
    bail!("tab {} không xuất hiện sau khi áp vân tay", host)
}

/// Vòng lặp trên kết nối browser-level: mỗi target mới (tab, popup OAuth, iframe) bị giữ lại ở trạng thái
/// "chờ debugger", được áp vân tay, rồi mới cho chạy — nên script trang không bao giờ thấy giá trị thật.
async fn run_injector(ws_url: String, fp: crate::fingerprint::Fingerprint, start_url: String, ready: tokio::sync::oneshot::Sender<()>) -> Result<()> {
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    let mut id: u64 = 0;
    // build thông điệp CDP (chưa gửi) — gửi bằng ws.send ở chỗ gọi để không giữ mượn ws trong closure
    let mut cmd = move |method: &str, params: Value, session: Option<&str>| {
        id += 1;
        let mut m = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session { m["sessionId"] = json!(s); }
        Message::Text(m.to_string())
    };
    let auto = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
    ws.send(cmd("Target.setAutoAttach", auto.clone(), None)).await?;
    ws.send(cmd("Target.getTargets", json!({}), None)).await?;
    let mut ready = Some(ready);
    let mut navigated = false;
    let mut attached: std::collections::HashSet<String> = Default::default();
    while let Some(msg) = ws.next().await {
        let Message::Text(t) = msg? else { continue };
        let v: Value = serde_json::from_str(&t)?;
        // trang about:blank có sẵn lúc Chrome khởi động: attach tay vì nó có thể mở trước khi ta bật auto-attach
        if v["result"]["targetInfos"].is_array() {
            for ti in v["result"]["targetInfos"].as_array().unwrap() {
                if ti["type"] == "page" {
                    if let Some(tid) = ti["targetId"].as_str() {
                        ws.send(cmd("Target.attachToTarget", json!({"targetId": tid, "flatten": true}), None)).await?;
                    }
                }
            }
            continue;
        }
        if v["method"] != "Target.attachedToTarget" { continue; }
        let p = &v["params"];
        let (Some(sid), Some(ty)) = (p["sessionId"].as_str(), p["targetInfo"]["type"].as_str()) else { continue };
        if !attached.insert(sid.to_string()) { continue; }
        if matches!(ty, "page" | "iframe") {
            ws.send(cmd("Target.setAutoAttach", auto.clone(), Some(sid))).await?;
            for (m, params) in fp.cdp_commands() {
                ws.send(cmd(m, params, Some(sid))).await?;
            }
            if ty == "page" && !navigated {
                navigated = true;
                ws.send(cmd("Page.navigate", json!({"url": start_url}), Some(sid))).await?;
                if let Some(r) = ready.take() { let _ = r.send(()); }
            }
        }
        if p["waitingForDebugger"].as_bool().unwrap_or(false) {
            ws.send(cmd("Runtime.runIfWaitingForDebugger", json!({}), Some(sid))).await?;
        }
    }
    Ok(())
}

/// Biến một token thành thiết bị đầy đủ: mở Chrome riêng của seed (nền), ghi token vào localStorage như web lưu,
/// tải lại để DeepSeek sinh cookie thiết bị (smidV2, aws-waf-token, ds_session_id) và device id, rồi đọc ra.
/// Dùng cho acc thêm bằng token, nâng cấp acc cũ lên thiết bị riêng, và làm mới cookie định kỳ.
pub async fn materialize_device(fp: &crate::fingerprint::Fingerprint, token: &str) -> Result<CapturedSession> {
    let (browser, ws) = launch_account_chrome(fp, "https://chat.deepseek.com/", true).await?;
    let res = async {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let mut conn = CdpConn::connect(&ws).await?;
        let stored = json!({"value": token, "__version": "0"}).to_string();
        let set = format!(
            "(() => {{ const cur = (() => {{ try {{ return JSON.parse(localStorage.getItem('userToken') || '{{}}').value }} catch (e) {{ return null }} }})(); \
               if (cur === {tok}) return false; localStorage.setItem('userToken', {stored}); return true; }})()",
            tok = serde_json::to_string(token)?, stored = serde_json::to_string(&stored)?);
        let changed = conn.call("Runtime.evaluate", json!({"expression": set, "returnByValue": true})).await?;
        if changed["result"]["value"].as_bool().unwrap_or(false) {
            let _ = conn.call("Page.reload", json!({})).await;
        }
        // chờ trang chạy xong JS thiết bị: có smidV2 + ds_session_id thì đủ; WAF token có thể tới sau vài giây
        let mut last = None;
        for i in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if let Ok((t, cookies, device)) = read_session_from_page(&ws).await {
                if t != token { bail!("trang không nhận token (đang có token khác)"); }
                let full = cookies.contains_key("smidV2") && cookies.contains_key("ds_session_id")
                    && (cookies.contains_key("aws-waf-token") || i >= 15);
                last = Some((cookies, device));
                if full { break; }
            }
        }
        let (cookies, device) = last.context("không đọc được phiên từ trang sau 30s")?;
        verify_and_build(token.to_string(), cookies, device, Some(&fp.seed)).await
    }.await;
    browser.close().await;
    res
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Cần Chrome + màn hình: `cargo test live_fp -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_fp_applied_in_real_chrome() {
        for seed in ["live-a", "live-b"] {
            let fp = crate::fingerprint::Fingerprint::from_seed(seed);
            let (b, ws) = launch_account_chrome(&fp, "https://chat.deepseek.com/sign_in", false).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let mut c = CdpConn::connect(&ws).await.unwrap();
            let js = r#"(()=>{const c=document.createElement('canvas');c.width=200;c.height=50;const x=c.getContext('2d');x.font='16px Arial';x.fillText('whalegate',10,30);
              const g=document.createElement('canvas').getContext('webgl');
              return JSON.stringify({ua:navigator.userAgent,plat:navigator.platform,cores:navigator.hardwareConcurrency,mem:navigator.deviceMemory,
               scr:[screen.width,screen.height,devicePixelRatio],tz:Intl.DateTimeFormat().resolvedOptions().timeZone,
               gpu:g.getParameter(37446),canvas:c.toDataURL().slice(-40),brands:navigator.userAgentData.brands,
               ts:Function.prototype.toString.call(HTMLCanvasElement.prototype.toDataURL)})})()"#;
            let r = c.call("Runtime.evaluate", json!({"expression": js, "returnByValue": true})).await.unwrap();
            println!("{seed}: {}\n  expect ua={} gpu={} sec-ch-ua={}", r["result"]["value"], fp.user_agent(), fp.gpu_renderer, fp.sec_ch_ua());
            b.close().await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Trang soi bot trong Chrome có vân tay: `cargo test live_bot -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_bot_pages() {
        let fp = crate::fingerprint::Fingerprint::from_seed("live-a");
        let (b, ws) = launch_account_chrome(&fp, "https://bot.sannysoft.com/", false).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        let mut c = CdpConn::connect(&ws).await.unwrap();
        // mọi dòng: tên | kết quả | màu nền ô kết quả (đỏ/cam = fail/warn trên trang)
        let js = r#"JSON.stringify([...document.querySelectorAll('tr')].map(r=>{const t=r.querySelectorAll('td');return t.length<2?null:[t[0].innerText.trim().slice(0,40),t[1].innerText.replace(/\s+/g,' ').trim().slice(0,70),getComputedStyle(t[1]).backgroundColor,t[1].className]}).filter(Boolean))"#;
        let r = c.call("Runtime.evaluate", json!({"expression": js, "returnByValue": true})).await.unwrap();
        let rows: Vec<Vec<String>> = serde_json::from_str(r["result"]["value"].as_str().unwrap()).unwrap();
        for r in &rows { println!("ROW {} | {} | {} | {}", r[0], r[1], r[2], r[3]); }
        b.close().await;
    }

    /// WebGL params + font trong Chrome có vân tay: `cargo test live_gl_fonts -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_gl_fonts() {
        for seed in ["live-a", "live-b"] {
            let fp = crate::fingerprint::Fingerprint::from_seed(seed);
            let (b, ws) = launch_account_chrome(&fp, "https://example.com/", false).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let mut c = CdpConn::connect(&ws).await.unwrap();
            let js = r#"(()=>{const g=document.createElement('canvas').getContext('webgl');
              const w=(f)=>{const s=document.createElement('span');s.style.fontFamily=f+',monospace';s.style.fontSize='72px';s.style.position='absolute';s.textContent='mmmmmmmmmmlli';document.body.appendChild(s);const v=s.offsetWidth;s.remove();return v};
              const base=w('monospace');
              const fonts=['Segoe UI','Calibri','Helvetica Neue','Menlo','DejaVu Sans','Liberation Sans','Noto Sans','Ubuntu','Arial','Courier New'];
              return JSON.stringify({os:navigator.platform,maxTex:g.getParameter(3379),viewport:Array.from(g.getParameter(3386)),vtx:g.getParameter(36347),point:Array.from(g.getParameter(33901)),
                visible:fonts.filter(f=>w(f)!==base),check:fonts.filter(f=>document.fonts.check('16px "'+f+'"'))})})()"#;
            let r = c.call("Runtime.evaluate", json!({"expression": js, "returnByValue": true})).await.unwrap();
            println!("{seed} os={:?}: {}", fp.os, r["result"]["value"]);
            b.close().await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Thứ tự header Chrome thật gửi cho fetch() có header tuỳ biến: `cargo test live_wire_order -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_wire_order() {
        let fp = crate::fingerprint::Fingerprint::from_seed("live-a");
        let (b, ws) = launch_account_chrome(&fp, "https://tls.peet.ws/", false).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let mut c = CdpConn::connect(&ws).await.unwrap();
        let js = r#"fetch('/api/all',{method:'POST',credentials:'include',headers:{'authorization':'Bearer x','content-type':'application/json','x-client-platform':'web','x-client-version':'2.7.0','x-client-locale':'vi','x-client-bundle-id':'com.deepseek.chat','x-client-timezone-offset':'25200','x-device-model':'','x-device-id':'d','x-ds-pow-response':'p'},body:'{}'}).then(r=>r.json()).then(j=>JSON.stringify(j.http2.sent_frames.filter(f=>f.headers).flatMap(f=>f.headers.map(h=>h.split(': ')[0]))))"#;
        let r = c.call("Runtime.evaluate", json!({"expression": js, "returnByValue": true, "awaitPromise": true})).await.unwrap();
        println!("CHROME_ORDER {}", r["result"]["value"]);
        b.close().await;
    }

    /// Token → thiết bị (headless), dùng acc đầu trong store, profile tạm rồi xoá: `cargo test live_materialize -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_materialize() {
        let raw = std::fs::read_to_string(format!("{}/.whalegate/accounts.json", std::env::var("HOME").unwrap())).unwrap();
        let st: Value = serde_json::from_str(&raw).unwrap();
        let token = st["accounts"][0]["token"].as_str().unwrap().to_string();
        let fp = crate::fingerprint::Fingerprint::from_seed("live-mat");
        let t = std::time::Instant::now();
        let r = materialize_device(&fp, &token).await;
        match r {
            Ok(s) => println!("MAT ok {}s cookies={:?} device={} email={}", t.elapsed().as_secs(), s.cookies.keys().collect::<Vec<_>>(), s.device_id.is_some(), s.email.is_some()),
            Err(e) => println!("MAT err {}s {e:#}", t.elapsed().as_secs()),
        }
        let _ = std::fs::remove_dir_all(account_profile_dir("live-mat"));
    }
}

#[cfg(test)]
mod os_tests {
    use super::*;

    #[test]
    fn reg_query_default_value_any_locale() {
        let en = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\chrome.exe\r\n    (Default)    REG_SZ    C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe\r\n";
        assert_eq!(parse_reg_default(en).as_deref(), Some(r"C:\Program Files\Google\Chrome\Application\chrome.exe"));
        // Windows tiếng Việt: tên giá trị mặc định khác, chỉ dựa vào REG_SZ
        let vi = "    (Mặc định)    REG_SZ    D:\\Apps\\Chrome\\chrome.exe\r\n";
        assert_eq!(parse_reg_default(vi).as_deref(), Some(r"D:\Apps\Chrome\chrome.exe"));
        let quoted = "    (Default)    REG_EXPAND_SZ    \"C:\\x\\msedge.exe\"\r\n";
        assert_eq!(parse_reg_default(quoted).as_deref(), Some(r"C:\x\msedge.exe"));
        assert_eq!(parse_reg_default("ERROR: The system was unable to find the specified registry key or value."), None);
    }

    #[test]
    fn candidates_match_this_os() {
        let c = chrome_candidates();
        if cfg!(target_os = "macos") {
            assert_eq!(c[0], "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
            assert!(c.iter().any(|p| p.ends_with("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome") && !p.starts_with("/Applications")), "thiếu ~/Applications");
            assert!(c.iter().any(|p| p.contains("Microsoft Edge.app")));
        } else if cfg!(windows) {
            let pf = std::env::var("PROGRAMFILES").unwrap();
            assert_eq!(c[0], format!(r"{}\Google\Chrome\Application\chrome.exe", pf.trim_end_matches('\\')));
            assert!(c.iter().all(|p| !p.starts_with('\\')), "biến môi trường rỗng không được sinh đường dẫn tương đối: {c:?}");
            assert!(c.iter().any(|p| p.ends_with("msedge.exe")));
        } else {
            assert!(c.is_empty(), "Linux dò PATH, không có đường dẫn cố định");
        }
    }

    /// Chrome thật trên hệ đang chạy (CI chạy trên Linux/macOS/Windows):
    /// `cargo test chrome_os_ -- --ignored --nocapture`, thêm WG_TEST_HEADED=1 để thử cả cửa sổ thật.
    /// Tìm Chrome → mở với vân tay → vân tay có trên tab → đặt cookie → tắt êm → mở lại cùng profile → cookie còn.
    #[tokio::test]
    #[ignore]
    async fn chrome_os_launch_fingerprint_close_reopen() {
        println!("Chrome: {}", find_chrome_binary().expect("không tìm thấy Chrome"));
        let mut modes = vec![true];
        if std::env::var("WG_TEST_HEADED").map(|v| v == "1").unwrap_or(false) { modes.push(false); }
        for headless in modes {
            let seed = format!("ci-os-{}", if headless { "headless" } else { "headed" });
            let fp = crate::fingerprint::Fingerprint::from_seed(&seed);
            let _ = std::fs::remove_dir_all(account_profile_dir(&seed));
            let mark = uuid::Uuid::new_v4().simple().to_string();

            let (b, ws) = launch_account_chrome(&fp, "https://example.com/", headless).await.expect("mở Chrome");
            let mut c = CdpConn::connect(&ws).await.unwrap();
            let mut seen = Value::Null;
            for _ in 0..30 {
                let r = c.call("Runtime.evaluate", json!({"expression": "JSON.stringify({ua:navigator.userAgent,cores:navigator.hardwareConcurrency,w:screen.width,href:location.href,ready:document.readyState})", "returnByValue": true})).await.unwrap();
                seen = serde_json::from_str(r["result"]["value"].as_str().unwrap_or("null")).unwrap_or(Value::Null);
                if seen["href"].as_str().unwrap_or("").starts_with("https://example.com") && seen["ready"] == "complete" { break; }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            println!("[{}] tab: {}", if headless { "headless" } else { "headed" }, seen);
            assert_eq!(seen["ua"], fp.user_agent(), "UA chưa được áp");
            assert_eq!(seen["cores"], fp.cores);
            assert_eq!(seen["w"], fp.screen.0);
            c.call("Runtime.evaluate", json!({"expression": format!("document.cookie='wgci={mark}; max-age=3600; path=/'; document.cookie")})).await.unwrap();
            let t = std::time::Instant::now();
            let graceful = b.close().await;
            println!("  đóng {} sau {}ms", if graceful { "êm (Browser.close)" } else { "bằng kill" }, t.elapsed().as_millis());
            assert!(graceful, "Chrome không tự đóng — phải kill");

            let (b, ws) = launch_account_chrome(&fp, "https://example.com/", headless).await.expect("mở lại Chrome");
            let mut c = CdpConn::connect(&ws).await.unwrap();
            let ck = c.call("Network.getCookies", json!({"urls": ["https://example.com/"]})).await.unwrap();
            let kept = ck["cookies"].as_array().unwrap().iter().any(|k| k["name"] == "wgci" && k["value"] == mark.as_str());
            b.close().await;
            let _ = std::fs::remove_dir_all(account_profile_dir(&seed));
            assert!(kept, "cookie không còn sau khi mở lại profile: {}", ck);
            println!("  cookie còn sau khi mở lại profile ✓");
        }
    }
}
