//! Đăng nhập acc: capture từ Chrome, login tự động/thủ công, thiết bị, tiện ích trình duyệt, vòng refresh.

#[allow(unused_imports)]
use super::*;

// Capture session từ Chrome đang login — nút 1 click trên UI
pub(crate) async fn api_capture_from_chrome(State(st): State<Arc<AppState>>) -> Json<Value> {
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
                        a.login_method = Some("scan".into());
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
                banned_at: None,
                            login_method: Some("scan".into()),
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
pub(crate) async fn api_login_start(State(st): State<Arc<AppState>>) -> Json<Value> {
    // Mỗi acc mới = một "máy" mới: seed riêng → profile Chrome riêng + vân tay riêng.
    start_login(st, crate::fingerprint::new_seed(), None, None).await
}
/// Đăng nhập TỰ ĐỘNG: nhận email + mật khẩu, mở Chrome riêng của một seed mới, tự điền form
/// `sign_in` và bấm nút, rồi chờ token như luồng login tay. Mật khẩu chỉ dùng một lần trong
/// request này — không ghi vào store, không log.
///
/// Trả về ngay khi Chrome mở xong; phần chờ token chạy nền để dashboard không bị treo 90s.
pub(crate) async fn api_login_auto(State(st): State<Arc<AppState>>, Json(body): Json<Value>) -> Json<Value> {
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let label = body["label"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if email.is_empty() || password.is_empty() {
        return Json(json!({"ok": false, "error": "thiếu email hoặc mật khẩu"}));
    }
    // chặn double-start: Chrome riêng khoá cổng CDP, hai luồng cùng mở sẽ tranh nhau
    {
        let mut watching = st.login_watching.lk();
        if *watching {
            return Json(json!({"ok": false, "error": "đang có một cửa sổ đăng nhập mở — đợi nó xong rồi thử lại"}));
        }
        *watching = true;
    }
    let seed = crate::fingerprint::new_seed();
    let fp = crate::fingerprint::Fingerprint::from_seed(&seed);
    // headless khi máy không có phiên đồ hoạ: form vẫn điều khiển được qua CDP
    let headless = !crate::chrome_capture::has_display();
    crate::elog!("[login-auto] {} seed={} headless={}", email, &seed[..8.min(seed.len())], headless);
    let (browser, ws_url) = match crate::chrome_capture::launch_account_chrome(&fp, "https://chat.deepseek.com/sign_in", headless).await {
        Ok(x) => x,
        Err(e) => {
            *st.login_watching.lk() = false;
            crate::elog!("[login-auto] lỗi mở Chrome: {:#}", e);
            return Json(json!({"ok": false, "error": format!("không mở được Chrome: {:#}", e)}));
        }
    };
    let st2 = st.clone();
    let em2 = email.clone();
    let label2 = label.clone();
    let reply_label = label.clone().unwrap_or_else(|| email.clone());
    tokio::spawn(async move {
        let res = login_auto_flow(&st2, seed, browser, ws_url, em2, password, label2).await;
        *st2.login_watching.lk() = false;
        match res {
            Ok(l) => crate::elog!("[login-auto] xong: {}", l),
            Err(e) => crate::elog!("[login-auto] lỗi: {:#}", e),
        }
    });
    Json(json!({"ok": true, "label": reply_label,
        "note": "đang tự đăng nhập trong Chrome riêng — xong acc tự vào pool"}))
}

/// Điền form, chờ token, rồi lưu acc — cùng đường với `login_flow` sau bước capture.
pub(crate) async fn login_auto_flow(
    st: &Arc<AppState>,
    seed: String,
    browser: crate::chrome_capture::AccountBrowser,
    ws_url: String,
    email: String,
    password: String,
    label: Option<String>,
) -> Result<String> {
    // trang sign_in cần vài giây nạp JS; submit_login_form tự poll tới khi form sẵn sàng
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    // hook TRƯỚC khi bấm: toast lỗi tự tắt, còn biz_msg của /users/login thì giữ được
    let hooked = crate::chrome_capture::install_login_response_hook(&ws_url).await.is_ok();
    if let Err(e) = crate::chrome_capture::submit_login_form(&ws_url, &email, &password).await {
        crate::elog!("[login-auto] lần 1 lỗi: {:#} — thử lại", e);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        crate::chrome_capture::submit_login_form(&ws_url, &email, &password).await?;
    }
    // mật khẩu không cần nữa: xoá khỏi bộ nhớ trước khi chờ (drop chuỗi gốc ở caller)
    drop(password);
    let res = crate::chrome_capture::watch_login_tab(ws_url.clone(), 120, browser.port, &seed).await;
    let session = match res {
        Ok(s) => s,
        Err(e) => {
            let why = if hooked {
                match crate::chrome_capture::login_result(&ws_url).await {
                    Some((2, _)) => "sai email hoặc mật khẩu".to_string(),
                    Some((10, _)) => "tài khoản đã bị khoá".to_string(),
                    Some((11, _)) => "DeepSeek đánh dấu thiết bị này là rủi ro (RISK_DEVICE_DETECTED)".to_string(),
                    Some((12, _)) => "số điện thoại bị hạn chế".to_string(),
                    Some((13, _)) => "cần chữ ký thiết bị (SIGNATURE_REQUIRED)".to_string(),
                    Some((14, _)) => "tài khoản bật xác thực 2 lớp (OTP_REQUIRED) — chưa hỗ trợ tự động".to_string(),
                    Some((0, _)) => "server nhận đăng nhập nhưng không cấp token".to_string(),
                    Some((c, m)) => format!("server từ chối (biz_code {c}{})", if m.is_empty() { String::new() } else { format!(", {m}") }),
                    None => format!("{e:#}"),
                }
            } else {
                format!("{e:#}")
            };
            browser.close().await;
            anyhow::bail!("{}", why);
        }
    };
    browser.close().await;
    crate::elog!("[login-auto] captured: {:?}", session.email);

    // lưu vào store — cùng quy tắc dedupe với login_flow: token, rồi uid
    let label_out = label.clone().or_else(|| session.email.clone()).unwrap_or_else(|| email.clone());
    {
        let mut store = st.store.write().await;
        if let Some(a) = store.accounts.iter_mut().find(|a| a.token == session.token) {
            a.cookies = session.cookies.clone();
            a.last_verified = Some(now());
            a.last_status = Some("ok (login-auto)".into());
            a.clear_auth_dead();
            if session.fp_seed.is_some() { adopt_chrome_device(a, &session); }
            if let Some(l) = &label { a.label = l.clone(); }
        } else if let Some(a) = session.uid.as_ref().and_then(|u| store.accounts.iter_mut().find(|a| a.ds_uid.as_deref() == Some(u.as_str()))) {
            if a.fp_seed.is_none() { a.fp_seed = Some(crate::fingerprint::seed_for(&a.token)); }
            a.token = session.token.clone();
            crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
            crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
            a.cookies = session.cookies.clone();
            a.last_verified = Some(now());
            a.last_status = Some("ok (login-auto)".into());
            a.clear_auth_dead();
            a.thread_session_id = None;
            if session.fp_seed.is_some() { adopt_chrome_device(a, &session); }
            if let Some(l) = &label { a.label = l.clone(); }
        } else {
            store.accounts.push(Account {
                id: uuid::Uuid::new_v4().to_string(),
                label: label_out.clone(),
                token: session.token.clone(),
                cookies: session.cookies.clone(),
                added_at: now(),
                last_verified: Some(now()),
                last_status: Some("ok (login-auto)".into()),
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
                banned_at: None,
                login_method: Some("chrome".into()),
            });
        }
    }
    mark_dirty(st);
    flush_store(st).await;
    Ok(label_out)
}


/// Tiện ích trình duyệt (`extension/`) nén sẵn để "Load unpacked": đọc được cả cookie HttpOnly mà bookmarklet
/// không đọc được. Địa chỉ dashboard lấy từ chính request tải về (giống bookmarklet), nên tiện ích gửi đúng về đây.
pub(crate) async fn api_extension_zip(headers: axum::http::HeaderMap) -> Response {
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

pub(crate) fn extension_zip(host: &str) -> Result<Vec<u8>> {
    use std::io::Write;
    // host đi thẳng vào manifest và JS: chỉ nhận host[:port] hợp lệ
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)) {
        anyhow::bail!("Host không hợp lệ: {}", host);
    }
    // match pattern của Chrome không mang cổng: http://127.0.0.1/* phủ mọi cổng của máy đó
    let hostname = if host.starts_with('[') { host.split(']').next().map(|h| format!("{}]", h)).unwrap_or_default() } else { host.split(':').next().unwrap_or(host).to_string() };
    let manifest = include_str!("../../extension/manifest.json").replace("__WG_HOST_PATTERN__", &format!("http://{}/*", hostname));
    let background = include_str!("../../extension/background.js").replace("__WG_ORIGIN__", &format!("http://{}", host));
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [("manifest.json", manifest), ("background.js", background)] {
        z.start_file(format!("whalegate-extension/{}", name), opt)?;
        z.write_all(body.as_bytes())?;
    }
    Ok(z.finish()?.into_inner())
}

/// Thông báo "DeepSeek từ chối đăng nhập" chỉ hiện trong chừng này giây: nó là sự kiện vừa xảy ra, không phải trạng thái lâu dài.
pub(crate) const LOGIN_NOTICE_TTL_SECS: u64 = 120;

pub(crate) fn live_login_notice(n: Option<&(u64, String)>, now: u64) -> Option<Value> {
    n.filter(|(t, _)| now.saturating_sub(*t) <= LOGIN_NOTICE_TTL_SECS).map(|(t, m)| json!({"at": t, "msg": m}))
}

/// Đăng nhập lại acc DS đã đăng xuất, mở đúng Chrome riêng của nó (profile `profiles/<seed>`, còn phiên Google)
/// với đúng vân tay cũ. Người dùng tự bấm đăng nhập trong cửa sổ — whalegate không tự điền gì. Đăng nhập xong,
/// `login_flow` nhận ra cùng người dùng qua uid và thay token vào chính acc này.
pub(crate) fn cf_result(r: Result<(), String>) -> Json<Value> {
    match r { Ok(()) => Json(json!({"ok": true})), Err(e) => Json(json!({"ok": false, "error": e})) }
}

pub(crate) async fn api_account_relogin(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let (acc, shared) = {
        let store = st.store.read().await;
        let acc = store.accounts.iter().find(|a| a.id == id).map(|a| (a.fp_seed.clone(), a.browser.is_some(), a.label.clone(), a.ds_uid.clone()));
        // profile này đang là "máy" của một acc KHÁC (vd lần đăng nhập nhầm trước đó đã gán nó cho người dùng khác)
        let shared = acc.as_ref().and_then(|a| a.0.clone()).is_some_and(|seed| store.accounts.iter().any(|o| o.id != id && o.fp_seed.as_deref() == Some(seed.as_str())));
        (acc, shared)
    };
    let Some((seed, has_browser, label, uid)) = acc else { return Json(json!({"ok": false, "error": "không tìm thấy tài khoản"})) };
    if has_browser {
        return Json(json!({"ok": false, "error": "acc này dùng trình duyệt gốc: đăng nhập lại trên trình duyệt đó rồi bấm tiện ích/bookmarklet trên tab chat.deepseek.com"}));
    }
    // acc dán token chưa từng có Chrome riêng: mở một máy mới — session cũ đã chết nên không có gì để "giữ máy".
    // Profile đang dùng chung với acc khác thì cũng mở máy mới: hai người dùng không thể chung một profile Chrome.
    let seed = if shared { None } else { seed }
        .filter(|s| std::path::Path::new(&crate::chrome_capture::account_profile_dir(s)).is_dir())
        .unwrap_or_else(crate::fingerprint::new_seed);
    crate::elog!("[login-flow] đăng nhập lại {} (profile {}{})", label, &seed[..8.min(seed.len())], if shared { ", profile cũ đang dùng chung với acc khác → máy mới" } else { "" });
    start_login(st, seed, uid, Some(id)).await
}

/// Nút "mở Chrome" trên dashboard: mở ĐÚNG Chrome của acc (profile `profiles/<seed>` với phiên, cookie, mật khẩu đã lưu,
/// vân tay cũ) ở chat.deepseek.com để bạn tự thao tác — giải hCaptcha, gửi thử một tin, đăng nhập lại… Cửa sổ giữ mở
/// tới khi bạn đóng; trong lúc đó whalegate theo dõi phiên, đóng xong thì lưu cookie/token mới vào acc và cho acc quay lại
/// pool (bỏ cooldown vì bị đòi xác minh).
pub(crate) async fn api_account_open_chrome(
    State(st): State<Arc<AppState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Json<Value> {
    let acc = st.store.read().await.accounts.iter().find(|a| a.id == id).map(|a| (a.fp_seed.clone(), a.browser.is_some(), a.label.clone(), a.auth_dead_at.is_some()));
    let Some((seed, has_browser, label, dead)) = acc else { return Json(json!({"ok": false, "error": "không tìm thấy tài khoản"})) };
    if has_browser {
        return Json(json!({"ok": false, "error": "acc này dùng trình duyệt gốc của bạn — mở chat.deepseek.com ngay trên trình duyệt đó"}));
    }
    // acc đã bị đăng xuất: "mở Chrome" làm đúng việc của "đăng nhập lại" (mở trang đăng nhập, tự điền, đăng nhập xong tự đóng)
    if dead { return api_account_relogin(State(st), axum::extract::Path(id)).await; }
    let Some(seed) = seed.filter(|s| std::path::Path::new(&crate::chrome_capture::account_profile_dir(s)).is_dir()) else {
        return Json(json!({"ok": false, "error": "acc này chưa có Chrome riêng (dán token) — dùng \"đăng nhập lại\" hoặc thêm lại bằng Chrome riêng"}));
    };
    {
        let mut watching = st.login_watching.lk();
        if *watching {
            return Json(json!({"ok": false, "error": "đang có một cửa sổ Chrome của whalegate mở — đóng cửa sổ đó rồi thử lại"}));
        }
        *watching = true;
    }
    let fp = crate::fingerprint::Fingerprint::from_seed(&seed);
    crate::elog!("[open-chrome] mở Chrome của {} (profile {}) — {} {} cores={}", label, &seed[..8.min(seed.len())], fp.user_agent(), fp.screen.0, fp.cores);
    let (browser, ws_url) = match crate::chrome_capture::launch_account_chrome(&fp, "https://chat.deepseek.com/", false).await {
        Ok(x) => x,
        Err(e) => {
            *st.login_watching.lk() = false;
            crate::elog!("[open-chrome] lỗi: {:#}", e);
            return Json(json!({"ok": false, "error": format!("không mở được Chrome: {:#}", e)}));
        }
    };
    let _ = ws_url;
    let st2 = st.clone();
    tokio::spawn(async move {
        let res = open_flow(st2.clone(), id, seed, browser).await;
        *st2.login_watching.lk() = false;
        if let Err(e) = res { crate::elog!("[open-chrome] lỗi: {:#}", e); }
    });
    Json(json!({"ok": true, "note": "đã mở Chrome của acc — làm gì cần làm (giải captcha, gửi thử một tin…) rồi đóng cửa sổ; whalegate tự lưu phiên"}))
}

/// Theo dõi cửa sổ Chrome do người dùng điều khiển tới khi họ đóng (tối đa 2 giờ), rồi áp phiên cuối cùng vào acc.
pub(crate) async fn open_flow(st: Arc<AppState>, acc_id: String, seed: String, browser: crate::chrome_capture::AccountBrowser) -> Result<()> {
    let port = browser.port;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2 * 3600);
    let mut last = None;
    let mut ticks = 0u32;
    loop {
        if tokio::time::Instant::now() >= deadline { crate::elog!("[open-chrome] quá 2 giờ — tự đóng cửa sổ"); break; }
        if !crate::chrome_capture::cdp_alive(port).await { break; }
        if ticks % 3 == 0 {
            if let Some(s) = crate::chrome_capture::peek_session(port).await { last = Some(s); }
        }
        ticks += 1;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    browser.close().await;
    let Some(raw) = last else {
        crate::elog!("[open-chrome] đã đóng — không có phiên DeepSeek đang đăng nhập trong cửa sổ, giữ nguyên acc");
        return Ok(());
    };
    let session = crate::chrome_capture::finish_session(raw, &seed).await?;
    let mut note = String::new();
    {
        let mut store = st.store.write().await;
        let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) else { return Ok(()) };
        let same_token = a.token == session.token;
        let same_user = session.uid.is_some() && session.uid == a.ds_uid;
        if !same_token && !same_user && !a.auth_dead_at.is_some() {
            crate::elog!("[open-chrome] cửa sổ đang đăng nhập người dùng khác ({:?}) — không đụng vào acc", session.email);
            return Ok(());
        }
        if !same_token {
            // đăng nhập lại: token mới của cùng người dùng, giữ nguyên vân tay
            a.token = session.token.clone();
            crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
            crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
            a.thread_session_id = None;
            note.push_str("token mới; ");
        }
        a.cookies = session.cookies.clone();
        if session.device_id.is_some() { a.device_id = session.device_id.clone(); crate::fingerprint::bind_device(&a.token, a.device_id.as_deref()); }
        if session.uid.is_some() && a.ds_uid.is_none() { a.ds_uid = session.uid.clone(); }
        a.last_verified = Some(now());
        if a.auth_dead_at.is_some() { a.clear_auth_dead(); a.stats.reset(); note.push_str("hết đăng xuất; "); }
        // bị đòi hCaptcha mà bạn đã xác minh trong cửa sổ: bỏ nghỉ để acc thử lại ngay (không chắc đã qua — nếu DS còn đòi
        // thì acc tự nghỉ lại 30 phút)
        if a.last_status.as_deref().is_some_and(|s| s.contains("hCaptcha")) {
            a.stats.cooldown_until = 0;
            a.stats.fail_streak = 0;
            note.push_str("bỏ nghỉ hCaptcha; ");
        }
        a.last_status = Some("ok (đã mở Chrome)".into());
        crate::elog!("[open-chrome] đã lưu phiên của {:?}: {}", session.email, note.trim_end_matches("; "));
    }
    mark_dirty(&st);
    flush_store(&st).await;
    Ok(())
}

pub(crate) async fn start_login(st: Arc<AppState>, seed: String, expect_uid: Option<String>, target_id: Option<String>) -> Json<Value> {
    *st.login_notice.lk() = None;
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
        let res = login_flow(st2.clone(), seed, browser, ws_url, expect_uid, target_id).await;
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
pub(crate) async fn give_device(st: &Arc<AppState>, id: &str) -> Result<String> {
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
pub(crate) async fn api_account_device(
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
pub(crate) async fn device_refresh_loop(st: Arc<AppState>) {
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
pub(crate) fn adopt_chrome_device(a: &mut Account, session: &crate::chrome_capture::CapturedSession) {
    a.fp_seed = session.fp_seed.clone();
    a.device_id = session.device_id.clone();
    a.device_refreshed_at = Some(now());
    a.browser = None;
    a.login_method = Some("chrome".into());
    crate::fingerprint::unbind(&a.token);
    crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
    crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
    crate::client::forget(&a.token);
}

/// Câu dễ hiểu cho mã từ chối của /users/login (mã như luồng đăng nhập tự động); lạ thì giữ nguyên thông điệp của DeepSeek.
pub(crate) fn login_refusal_text(code: i64, msg: &str) -> String {
    let known = match code {
        2 => Some("sai email hoặc mật khẩu"),
        // bắt từ DeepSeek thật: biz_code 10 + biz_msg USER_IS_BANNED, web hiện "Do vi phạm, tài khoản của bạn đã tạm thời bị đình chỉ"
        10 => Some("DeepSeek đã tạm thời đình chỉ tài khoản này (vi phạm) — đăng nhập lại không giúp được, cần chờ hoặc khiếu nại trên DeepSeek"),
        11 => Some("DeepSeek đánh dấu thiết bị này là rủi ro"),
        12 => Some("số điện thoại bị hạn chế"),
        13 => Some("cần chữ ký thiết bị"),
        14 => Some("tài khoản bật xác thực 2 lớp"),
        _ => None,
    };
    match (known, msg.trim()) {
        (Some(k), "") => k.to_string(),
        (Some(k), m) if m == "USER_IS_BANNED" => format!("{k} [{m}]"),
        (Some(k), m) => format!("{k} ({m})"),
        (None, "") => format!("biz_code {code}"),
        (None, m) => m.to_string(),
    }
}

pub(crate) async fn login_flow(st: Arc<AppState>, seed: String, browser: crate::chrome_capture::AccountBrowser, ws_url: String, expect_uid: Option<String>, target_id: Option<String>) -> Result<()> {
    // Điền sẵn email/mật khẩu đã nhớ (không bấm đăng nhập) và ghi lại những gì bạn gõ để lần sau khỏi gõ lại.
    let (known_email, known_pw) = {
        let saved = crate::creds::get(&seed);
        let email = saved.as_ref().map(|c| c.0.clone()).or_else(|| {
            let store = st.store.try_read().ok()?;
            store.accounts.iter().find(|a| a.fp_seed.as_deref() == Some(seed.as_str()) && a.label.contains('@') && !a.label.contains('*')).map(|a| a.label.clone())
        });
        (email, saved.map(|c| c.1))
    };
    let typed: std::sync::Arc<std::sync::Mutex<Option<(String, String)>>> = Default::default();
    let (ban_tx, ban_rx) = tokio::sync::oneshot::channel::<String>();
    let helper = {
        let mut ban_tx = Some(ban_tx);
        let (ws, typed, st_h, target_h) = (ws_url.clone(), typed.clone(), st.clone(), target_id.clone());
        tokio::spawn(async move {
            if let Err(e) = crate::chrome_capture::prepare_login_page(&ws, known_email.as_deref(), known_pw.as_deref()).await {
                crate::elog!("[login-flow] điền sẵn form lỗi: {:#}", e);
            }
            let mut last_notice = String::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if let Some(c) = crate::chrome_capture::read_typed_creds(&ws).await { *typed.lock().unwrap_or_else(|p| p.into_inner()) = Some(c); }
                // bắt phản hồi /users/login: DeepSeek từ chối (đình chỉ, sai mật khẩu, rủi ro thiết bị…) thì báo lên dashboard ngay
                let _ = crate::chrome_capture::install_login_response_hook(&ws).await;
                if let Some((code, msg)) = crate::chrome_capture::login_result(&ws).await {
                    if code != 0 {
                        let text = login_refusal_text(code, &msg);
                        if text != last_notice {
                            last_notice = text.clone();
                            crate::elog!("[login-flow] DeepSeek từ chối đăng nhập: {text} (biz_code {code})");
                            *st_h.login_notice.lk() = Some((now(), text.clone()));
                            // tài khoản bị đình chỉ: không còn gì để chờ trong cửa sổ này
                            if code == 10 { if let Some(tx) = ban_tx.take() { let _ = tx.send(text.clone()); } }
                            if let Some(tid) = &target_h {
                                let mut store = st_h.store.write().await;
                                if let Some(a) = store.accounts.iter_mut().find(|a| &a.id == tid) { a.last_status = Some(format!("Đăng nhập bị từ chối: {text}")); }
                                drop(store);
                                mark_dirty(&st_h);
                            }
                        }
                    }
                }
            }
        })
    };
    // canh login tới 10 phút, xong thì tắt Chrome (giữ profile)
    let res = tokio::select! {
        r = crate::chrome_capture::watch_login_tab(ws_url.clone(), 600, browser.port, &seed) => r,
        Ok(why) = ban_rx => Err(anyhow::anyhow!("BANNED:{why}")),
    };
    helper.abort();
    // DeepSeek từ chối vì tài khoản bị đình chỉ: đóng cửa sổ NGAY, đánh dấu acc (tắt khỏi pool, gạch ngang), không lưu gì
    if let Err(e) = &res {
        if let Some(why) = format!("{e:#}").strip_prefix("BANNED:") {
            let why = why.to_string();
            browser.close().await;
            if let Some(tid) = &target_id {
                let mut store = st.store.write().await;
                if let Some(a) = store.accounts.iter_mut().find(|a| &a.id == tid) {
                    a.banned_at = Some(now());
                    a.pool_enabled = false;
                    a.last_status = Some("DeepSeek đã đình chỉ tài khoản này (USER_IS_BANNED)".into());
                    crate::elog!("[login-flow] acc {} bị DeepSeek đình chỉ — đã đóng cửa sổ, tắt khỏi pool", &a.id[..8]);
                }
            }
            mark_dirty(&st);
            flush_store(&st).await;
            anyhow::bail!("tài khoản bị đình chỉ — {why}");
        }
    }
    // Đang đăng nhập lại một acc cụ thể mà phiên bắt được là của NGƯỜI DÙNG KHÁC: không lưu gì, không nhớ mật khẩu,
    // xoá phiên DeepSeek sai khỏi profile (nếu không lần mở sau tự vào lại tài khoản sai), báo lên dashboard.
    if let (Ok(s), Some(exp)) = (&res, &expect_uid) {
        if s.uid.as_deref().is_some_and(|u| u != exp) {
            let got = s.email.clone().unwrap_or_else(|| "tài khoản khác".into());
            if let Err(e) = crate::chrome_capture::clear_ds_site_data(&ws_url).await { crate::elog!("[login-flow] không xoá được phiên sai: {:#}", e); }
            browser.close().await;
            if let Some(tid) = &target_id {
                let mut store = st.store.write().await;
                if let Some(a) = store.accounts.iter_mut().find(|a| &a.id == tid) {
                    a.last_status = Some(format!("Đăng nhập nhầm: cửa sổ vào {} chứ không phải acc này — bấm đăng nhập lại và chọn đúng tài khoản", got));
                }
            }
            mark_dirty(&st);
            anyhow::bail!("đăng nhập nhầm tài khoản ({got}) — không lưu, đã xoá phiên sai khỏi profile");
        }
    }
    browser.close().await;
    if res.is_ok() {
        let c = typed.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some((e, p)) = c { crate::creds::set(&seed, &e, &p); crate::elog!("[login-flow] đã nhớ email + mật khẩu của acc để điền sẵn lần sau"); }
    }

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
                banned_at: None,
                login_method: Some("chrome".into()),
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
pub(crate) async fn auto_refresh_loop(st: Arc<AppState>) {
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
mod login_refusal_tests {
    use super::*;

    #[test]
    fn login_notice_expires() {
        let n = (1000u64, "x".to_string());
        assert!(live_login_notice(Some(&n), 1000).is_some());
        assert!(live_login_notice(Some(&n), 1000 + LOGIN_NOTICE_TTL_SECS).is_some());
        assert!(live_login_notice(Some(&n), 1000 + LOGIN_NOTICE_TTL_SECS + 1).is_none(), "F5 sau vài phút không hiện lại");
        assert!(live_login_notice(None, 5).is_none());
    }

    #[test]
    fn a_successful_login_lifts_the_suspension_mark() {
        let mut a: Account = serde_json::from_str(r#"{"id":"x","label":"l","token":"t","cookies":{},"added_at":1,"banned_at":5,"auth_dead_at":6,"pool_enabled":false}"#).unwrap();
        assert!(a.banned_at.is_some());
        a.clear_auth_dead();
        assert!(a.banned_at.is_none() && a.auth_dead_at.is_none());
        // acc cũ không có trường banned_at vẫn đọc được
        let old: Account = serde_json::from_str(r#"{"id":"x","label":"l","token":"t","cookies":{},"added_at":1}"#).unwrap();
        assert!(old.banned_at.is_none());
    }

    #[test]
    fn refusal_texts_are_readable() {
        assert_eq!(login_refusal_text(12, ""), "số điện thoại bị hạn chế");
        // phản hồi thật bắt được khi đăng nhập một tài khoản bị đình chỉ
        assert!(login_refusal_text(10, "USER_IS_BANNED").starts_with("DeepSeek đã tạm thời đình chỉ tài khoản này"));
        assert!(login_refusal_text(10, "USER_IS_BANNED").ends_with("[USER_IS_BANNED]"));
        assert_eq!(login_refusal_text(99, "Do vi phạm, tài khoản của bạn đã tạm thời bị đình chỉ."), "Do vi phạm, tài khoản của bạn đã tạm thời bị đình chỉ.");
        assert_eq!(login_refusal_text(99, ""), "biz_code 99");
    }
}

