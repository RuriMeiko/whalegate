// Capture DeepSeek session qua CDP (port 9333) — Chrome của user.
// Flow login acc mới (1 nút):
//   mở tab DS sign_in (tab thường, profile chính) → user login kiểu gì cũng được
//   → server CANH đúng tab đó (poll 2s) → thấy token → verify → lưu → TỰ TẮT TAB.
//   Không cần bấm quét tabs.
// Fallback máy không có CDP: dán JSON {token, cookies} thủ công.
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

// Port CDP RIÊNG cho Chrome instance login (tránh đụng Chrome chính 9333)
pub fn login_cdp_port() -> u16 {
    static V: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("DS_LOGIN_CDP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(9777)
    })
}

fn login_cdp_http() -> String {
    format!("http://127.0.0.1:{}", login_cdp_port())
}

/// Profile login CỐ ĐỊNH (tái dùng mỗi lần login, KHÔNG xoá sau khi dùng) —
/// fingerprint ổn định qua các lần login để DS risk-engine không flag
/// "Current device environment error". Xoá tay bằng env DS_RESET_LOGIN_PROFILE=1.
pub fn login_profile_dir() -> String {
    std::env::var("DS_LOGIN_PROFILE").unwrap_or_else(|_| format!("{}/login-profile", crate::server::data_dir()))
}

/// Tìm binary Chrome trên máy: DS_CHROME_BIN → đường dẫn cài mặc định (macOS/Windows) → tên lệnh trong PATH.
fn find_chrome_binary() -> Result<String> {
    if let Ok(bin) = std::env::var("DS_CHROME_BIN") {
        if !bin.trim().is_empty() { return Ok(bin); }
    }
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let installed = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string(),
        "/Applications/Chromium.app/Contents/MacOS/Chromium".to_string(),
        r"C:\Program Files\Google\Chrome\Application\chrome.exe".to_string(),
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe".to_string(),
        format!(r"{}\Google\Chrome\Application\chrome.exe", local),
    ];
    if let Some(p) = installed.iter().find(|p| std::path::Path::new(p.as_str()).is_file()) {
        return Ok(p.clone());
    }
    for bin in [
        "google-chrome-stable",
        "google-chrome",
        "chromium-browser",
        "chromium",
        "chrome",
    ] {
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
    bail!("không tìm thấy Chrome/Chromium trên máy — cài google-chrome hoặc đặt DS_CHROME_BIN")
}

/// SPAWN Chrome instance RIÊNG cho login: port CDP riêng (9777, config
/// DS_LOGIN_CDP_PORT) để không đụng Chrome chính; profile CỐ ĐỊNH
/// (~/.whalegate/login-profile) để fingerprint ổn định, tránh lỗi
/// "Current device environment error" của DS risk-engine; mở thẳng trang sign_in DS.
/// Trả (Child process, đường dẫn profile) — caller kill Chrome khi xong (GIỮ profile).
pub async fn spawn_login_chrome() -> Result<(std::process::Child, String)> {
    let bin = find_chrome_binary()?;
    // reset profile nếu user yêu cầu
    let profile = login_profile_dir();
    if std::env::var("DS_RESET_LOGIN_PROFILE").as_deref() == Ok("1") {
        let _ = std::fs::remove_dir_all(&profile);
    }
    let port = login_cdp_port();
    let mut child = std::process::Command::new(&bin)
        .arg(format!("--remote-debugging-port={}", port))
        .arg(format!("--user-data-dir={}", profile))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-features=Translate")
        .arg("https://chat.deepseek.com/sign_in")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context(format!("không spawn được Chrome ({})", bin))?;

    // chờ CDP của instance mới sẵn sàng (tối đa 20s)
    let mut ready = false;
    for _ in 0..40 {
        if let Ok(resp) = reqwest::get(format!("{}/json/version", login_cdp_http())).await {
            if resp.status().is_success() {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if !ready {
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&profile);
        bail!("Chrome login đã mở nhưng CDP :{} không sẵn sàng sau 20s", port);
    }
    Ok((child, profile))
}

// ---------- BUILD PROFILE: nuôi profile login giống real user ----------

const WARMUP_SITES: &[&str] = &[
    "https://www.wikipedia.org/",
    "https://news.ycombinator.com/",
    "https://www.bing.com/",
];

/// BUILD PROFILE (warm-up): mở Chrome login, dạo vài site phổ biến + vào trang DS
/// đọc nội dung (như user lướt web), đợi đủ lâu để Chrome cache assets phổ biến
/// (favicon, JS của các CDN), Google/DS set device-id cookies quan sát được
/// (smidV2, ds_session_id), và lịch sử + IndexedDB của DS tạo nền fingerprint "đã dùng máy".
/// Gọi 1 lần trước khi login acc đầu. Lần login sau dùng lại profile này.
pub async fn build_profile(warmup_secs: u64) -> Result<String> {
    let (mut child, profile) = spawn_login_chrome().await?;
    // đảm bảo Chrome load xong trang sign_in trước khi dạo
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    // dạo vài site phổ biến (mở tab, đợi, đóng) — tạo lịch sử browser thật
    let mut conn = CdpConn::connect(&browser_ws_url_for_login().await?).await?;
    for site in WARMUP_SITES {
        let tgt = conn
            .call(
                "Target.createTarget",
                json!({"url": site, "background": true}),
            )
            .await?;
        let tab_id = tgt["targetId"].as_str().unwrap_or("").to_string();
        // ở lại site 1 nhịp — như user đọc trang
        tokio::time::sleep(std::time::Duration::from_secs(warmup_secs.max(3))).await;
        if !tab_id.is_empty() {
            let _ = conn
                .call("Target.closeTarget", json!({"targetId": tab_id}))
                .await;
        }
    }

    // vào trang chính DS ở lại chút (để DS set device-id/fp cookies trước khi login)
    let tgt = conn
        .call(
            "Target.createTarget",
            json!({"url": "https://chat.deepseek.com/", "background": true}),
        )
        .await?;
    let tab_id = tgt["targetId"].as_str().unwrap_or("").to_string();
    tokio::time::sleep(std::time::Duration::from_secs(warmup_secs.max(3))).await;
    if !tab_id.is_empty() {
        let _ = conn
            .call("Target.closeTarget", json!({"targetId": tab_id}))
            .await;
    }

    // tắt Chrome, GIỮ profile
    let _ = child.kill();
    let _ = child.wait();
    Ok(format!(
        "profile warm xong: {} site + DS trang chính, tổng ~{}s — profile: {}",
        WARMUP_SITES.len() + 1,
        ((WARMUP_SITES.len() + 1) as u64) * warmup_secs.max(3),
        profile
    ))
}

async fn browser_ws_url_for_login() -> Result<String> {
    let d: Value = reqwest::get(format!("{}/json/version", login_cdp_http()))
        .await?
        .json()
        .await?;
    Ok(d["webSocketDebuggerUrl"]
        .as_str()
        .context("no browser ws (login)")?
        .to_string())
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

/// CANH 1 tab login: poll 2s/lần tới khi tab có token (login xong) hoặc hết giờ.
/// Verify token rồi trả session — caller tự đóng tab sau khi lưu.
pub async fn watch_login_tab(ws_url: String, timeout_secs: u64, port: u16, fp_seed: &str) -> Result<CapturedSession> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    // Tab sign_in đôi khi KHÔNG redirect (profile chính đã login, DS đứng yên ở form).
    // → poll MỌI tab DS trong Chrome: tab nào verify được session là lấy, ưu tiên tab watch.
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("hết {}s mà chưa login xong trong tab", timeout_secs);
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
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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
}

impl AccountBrowser {
    /// Tắt Chrome, GIỮ profile (cookie/localStorage/IndexedDB của acc nằm ở đó).
    pub fn close(mut self) {
        self.injector.abort();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Thư mục profile Chrome của một acc: cố định theo seed để lần sau mở lại đúng "máy" đó.
pub fn account_profile_dir(seed: &str) -> String {
    format!("{}/profiles/{}", crate::server::data_dir(), seed)
}

/// Mở Chrome riêng cho acc (profile riêng, cổng CDP login), áp vân tay lên MỌI tab/iframe mở ra
/// (UA + client hints, múi giờ, canvas/WebGL/audio/màn hình/phần cứng) rồi đưa tab đầu tới `start_url`.
/// Trả (browser, ws url của tab đầu).
pub async fn launch_account_chrome(fp: &crate::fingerprint::Fingerprint, start_url: &str) -> Result<(AccountBrowser, String)> {
    let port = login_cdp_port();
    if reqwest::get(format!("{}/json/version", cdp_http(port))).await.is_ok() {
        bail!("đang có một phiên Chrome login khác chạy trên cổng {} — đóng nó rồi thử lại", port);
    }
    let bin = find_chrome_binary()?;
    let profile = account_profile_dir(&fp.seed);
    std::fs::create_dir_all(&profile).ok();
    let (w, h) = fp.window_size();
    let mut child = std::process::Command::new(&bin)
        .arg(format!("--remote-debugging-port={}", port))
        .arg(format!("--user-data-dir={}", profile))
        .arg(format!("--window-size={},{}", w, h))
        .arg("--lang=vi-VN")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-features=Translate")
        .arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context(format!("không spawn được Chrome ({})", bin))?;
    let mut ready = false;
    for _ in 0..40 {
        if let Ok(resp) = reqwest::get(format!("{}/json/version", cdp_http(port))).await {
            if resp.status().is_success() { ready = true; break; }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    if !ready {
        let _ = child.kill();
        let _ = child.wait();
        bail!("Chrome của acc mở rồi nhưng CDP :{} không sẵn sàng sau 20s", port);
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
    let browser = AccountBrowser { child, port, injector };
    if tokio::time::timeout(std::time::Duration::from_secs(15), ready_rx).await.map_err(|_| ()).and_then(|r| r.map_err(|_| ())).is_err() {
        browser.close();
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
    browser.close();
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

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Cần Chrome + màn hình: `cargo test live_fp -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_fp_applied_in_real_chrome() {
        for seed in ["live-a", "live-b"] {
            let fp = crate::fingerprint::Fingerprint::from_seed(seed);
            let (b, ws) = launch_account_chrome(&fp, "https://chat.deepseek.com/sign_in").await.unwrap();
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
            b.close();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Trang soi bot trong Chrome có vân tay: `cargo test live_bot -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_bot_pages() {
        let fp = crate::fingerprint::Fingerprint::from_seed("live-a");
        let (b, ws) = launch_account_chrome(&fp, "https://bot.sannysoft.com/").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        let mut c = CdpConn::connect(&ws).await.unwrap();
        // mọi dòng: tên | kết quả | màu nền ô kết quả (đỏ/cam = fail/warn trên trang)
        let js = r#"JSON.stringify([...document.querySelectorAll('tr')].map(r=>{const t=r.querySelectorAll('td');return t.length<2?null:[t[0].innerText.trim().slice(0,40),t[1].innerText.replace(/\s+/g,' ').trim().slice(0,70),getComputedStyle(t[1]).backgroundColor,t[1].className]}).filter(Boolean))"#;
        let r = c.call("Runtime.evaluate", json!({"expression": js, "returnByValue": true})).await.unwrap();
        let rows: Vec<Vec<String>> = serde_json::from_str(r["result"]["value"].as_str().unwrap()).unwrap();
        for r in &rows { println!("ROW {} | {} | {} | {}", r[0], r[1], r[2], r[3]); }
        b.close();
    }

    /// WebGL params + font trong Chrome có vân tay: `cargo test live_gl_fonts -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_gl_fonts() {
        for seed in ["live-a", "live-b"] {
            let fp = crate::fingerprint::Fingerprint::from_seed(seed);
            let (b, ws) = launch_account_chrome(&fp, "https://example.com/").await.unwrap();
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
            b.close();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Thứ tự header Chrome thật gửi cho fetch() có header tuỳ biến: `cargo test live_wire_order -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_wire_order() {
        let fp = crate::fingerprint::Fingerprint::from_seed("live-a");
        let (b, ws) = launch_account_chrome(&fp, "https://tls.peet.ws/").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let mut c = CdpConn::connect(&ws).await.unwrap();
        let js = r#"fetch('/api/all',{method:'POST',credentials:'include',headers:{'authorization':'Bearer x','content-type':'application/json','x-client-platform':'web','x-client-version':'2.7.0','x-client-locale':'vi','x-client-bundle-id':'com.deepseek.chat','x-client-timezone-offset':'25200','x-device-model':'','x-device-id':'d','x-ds-pow-response':'p'},body:'{}'}).then(r=>r.json()).then(j=>JSON.stringify(j.http2.sent_frames.filter(f=>f.headers).flatMap(f=>f.headers.map(h=>h.split(': ')[0]))))"#;
        let r = c.call("Runtime.evaluate", json!({"expression": js, "returnByValue": true, "awaitPromise": true})).await.unwrap();
        println!("CHROME_ORDER {}", r["result"]["value"]);
        b.close();
    }
}
