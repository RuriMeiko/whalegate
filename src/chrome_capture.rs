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
}

const CDP_HTTP: &str = "http://127.0.0.1:9333";

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
    std::env::var("DS_LOGIN_PROFILE").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{}/.ds-harness/login-profile", home)
    })
}

/// Tìm binary Chrome trên máy.
fn find_chrome_binary() -> Result<String> {
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

/// SPAWN Chrome instance RIÊNG cho login:
/// - port CDP riêng (9777, config DS_LOGIN_CDP_PORT) — không đụng Chrome chính
/// - profile CỐ ĐỊNH (~/.ds-harness/login-profile) — fingerprint ổn định, tránh
///   "Current device environment error" của DS risk-engine
/// - mở thẳng trang sign_in DS
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

/// Tìm tab sign_in trong Chrome LOGIN instance (port riêng), trả ws_url.
pub async fn find_login_tab_ws() -> Result<String> {
    let body = reqwest::get(format!("{}/json/list", login_cdp_http()))
        .await?
        .text()
        .await?;
    let pages: Vec<Value> = serde_json::from_str(&body)?;
    let tab = pages
        .iter()
        .find(|p| {
            p["type"] == "page"
                && p["url"]
                    .as_str()
                    .map(|u| u.contains("chat.deepseek.com"))
                    .unwrap_or(false)
        })
        .context("không thấy tab chat.deepseek.com trong Chrome login")?;
    Ok(tab["webSocketDebuggerUrl"]
        .as_str()
        .context("no ws url")?
        .to_string())
}

// ---------- BUILD PROFILE: nuôi profile login giống real user ----------

const WARMUP_SITES: &[&str] = &[
    "https://www.wikipedia.org/",
    "https://news.ycombinator.com/",
    "https://www.bing.com/",
];

/// BUILD PROFILE (warm-up): mở Chrome login, dạo vài site phổ biến + vào trang DS
/// đọc nội dung (như user lướt web), đợi đủ lâu để:
/// - Chrome cache assets phổ biến (favicon, JS của các CDN)
/// - Google/DS set device-id cookies quan sát được (smidV2, ds_session_id)
/// - lịch sử + IndexedDB của DS tạo nền fingerprint "đã dùng máy"
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
                json!({"id": id, "method": method, "params": params}).to_string().into(),
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

async fn browser_ws_url() -> Result<String> {
    let d: Value = reqwest::get(format!("{}/json/version", CDP_HTTP))
        .await?
        .json()
        .await
        .context("không kết nối được Chrome CDP :9333")?;
    Ok(d["webSocketDebuggerUrl"]
        .as_str()
        .context("no browser ws")?
        .to_string())
}

async fn list_pages() -> Result<Vec<Value>> {
    let body = reqwest::get(format!("{}/json/list", CDP_HTTP)).await?.text().await?;
    Ok(serde_json::from_str(&body)?)
}

/// Mở tab thường (profile chính) tới trang sign_in DS — user login kiểu gì cũng
/// được (Google/email/passkey). Trả (target_id, ws_url) để canh + đóng.
pub async fn open_ds_login_tab() -> Result<(String, String)> {
    let mut conn = CdpConn::connect(&browser_ws_url().await?).await?;
    let tgt = conn
        .call(
            "Target.createTarget",
            json!({"url": "https://chat.deepseek.com/sign_in"}),
        )
        .await?;
    let target_id = tgt["targetId"].as_str().context("no target")?.to_string();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let ws_url = list_pages()
        .await
        .ok()
        .and_then(|pages| {
            pages
                .iter()
                .find(|p| p["id"].as_str() == Some(target_id.as_str()))
                .and_then(|p| p["webSocketDebuggerUrl"].as_str().map(|s| s.to_string()))
        })
        .context("tab mới không có ws url")?;
    Ok((target_id, ws_url))
}

/// Đóng 1 tab theo target_id.
pub async fn close_target(target_id: &str) -> Result<()> {
    let mut conn = CdpConn::connect(&browser_ws_url().await?).await?;
    let _ = conn
        .call("Target.closeTarget", json!({"targetId": target_id}))
        .await?;
    Ok(())
}

/// Đọc token + cookies từ 1 tab theo ws url. Err nếu tab chưa login / tab đóng.
pub async fn read_session_from_page(
    ws_url: &str,
) -> Result<(String, std::collections::BTreeMap<String, String>)> {
    let mut conn = CdpConn::connect(ws_url).await?;
    let expr = r#"(() => {
        const t = localStorage.getItem('userToken');
        if (!t) return {token: null, cookies: {}};
        return {
          token: JSON.parse(t).value,
          cookies: Object.fromEntries(document.cookie.split(';').map(c => {
            const i = c.indexOf('=');
            return i < 0 ? ['', ''] : [c.slice(0, i).trim(), c.slice(i + 1)];
          }).filter(([k]) => k)),
        };
    })()"#;
    let res = conn
        .call("Runtime.evaluate", json!({"expression": expr, "returnByValue": true}))
        .await?;
    let value = &res["result"]["value"];
    let token = value["token"]
        .as_str()
        .context("tab chưa login (userToken rỗng)")?
        .to_string();
    let mut cookies = std::collections::BTreeMap::new();
    if let Some(obj) = value["cookies"].as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                cookies.insert(k.clone(), s.to_string());
            }
        }
    }
    Ok((token, cookies))
}

async fn verify_and_build(
    token: String,
    cookies: std::collections::BTreeMap<String, String>,
) -> Result<CapturedSession> {
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
    Ok(CapturedSession { token, cookies, email })
}

/// CANH 1 tab login: poll 2s/lần tới khi tab có token (login xong) hoặc hết giờ.
/// Verify token rồi trả session — caller tự đóng tab sau khi lưu.
pub async fn watch_login_tab(ws_url: String, timeout_secs: u64) -> Result<CapturedSession> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    // Tab sign_in đôi khi KHÔNG redirect (profile chính đã login, DS đứng yên ở form).
    // → poll MỌI tab DS trong Chrome: tab nào verify được session là lấy, ưu tiên tab watch.
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("hết {}s mà chưa login xong trong tab", timeout_secs);
        }
        // 1) thử tab watch trước
        if let Ok((token, cookies)) = read_session_from_page(&ws_url).await {
            if let Ok(s) = verify_and_build(token, cookies).await {
                return Ok(s);
            }
        }
        // 2) quét mọi tab DS (user có thể login trong tab khác / sign_in đứng yên)
        if let Ok(sessions) = capture_all_ds_tabs().await {
            if let Some(s) = sessions.into_iter().next() {
                return Ok(s);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// Quét mọi tab DS đang mở (phụ — dùng khi user tự login trong tab riêng).
pub async fn capture_all_ds_tabs() -> Result<Vec<CapturedSession>> {
    let pages = list_pages().await.context("không kết nối được Chrome CDP :9333")?;
    let mut out: Vec<CapturedSession> = Vec::new();
    for p in pages.iter().filter(|p| {
        p["type"] == "page"
            && p["url"].as_str().map(|u| u.contains("chat.deepseek.com")).unwrap_or(false)
    }) {
        let Some(ws_url) = p["webSocketDebuggerUrl"].as_str() else { continue };
        if let Ok((token, cookies)) = read_session_from_page(ws_url).await {
            if out.iter().any(|s| s.token == token) {
                continue; // cùng acc mở 2 tab
            }
            if let Ok(s) = verify_and_build(token, cookies).await {
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
    let pages = list_pages().await.context("không kết nối được Chrome CDP :9333")?;
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
