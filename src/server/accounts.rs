//! Handler quản lý acc: thêm / sửa / xoá / reset / kiểm tra, bật-tắt pool.

#[allow(unused_imports)]
use super::*;

#[derive(Deserialize)]
pub(crate) struct AddAccount {
    #[serde(default)]
    label: String,
    token: String,
    #[serde(default)]
    cookies: std::collections::BTreeMap<String, String>,
    /// Gói "Thêm vào whalegate" từ bookmarklet: vân tay thật của trình duyệt đang đăng nhập DS.
    /// Có gói này thì acc dùng đúng máy đó; không có thì như acc dán token cũ (vân tay dẫn xuất từ token).
    #[serde(default)]
    browser: Option<crate::fingerprint::BrowserProfile>,
    /// Cách thêm: "extension" | "bookmarklet" | "paste" (client tự khai; giá trị lạ bị bỏ)
    #[serde(default)]
    via: Option<String>,
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
pub(crate) fn restore_binding(store: &Store, token: &str) {
    crate::fingerprint::unbind(token);
    if let Some(a) = store.accounts.iter().find(|a| a.token == token) {
        crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
        crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
        crate::fingerprint::bind_profile(&a.token, a.browser.clone());
    }
    crate::client::forget(token);
}

pub(crate) async fn api_add_account(
    State(st): State<Arc<AppState>>,
    Json(mut p): Json<AddAccount>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    p.token = normalize_token(&p.token);
    let via = clean_via(&p.via);
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
        if via.is_some() { a.login_method = via.clone(); }
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
                banned_at: None,
        login_method: via,
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
pub(crate) fn apply_readd(
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

pub(crate) async fn api_list_accounts(State(st): State<Arc<AppState>>) -> Json<Value> {
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
pub(crate) async fn api_toggle_pool(
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
pub(crate) struct EditAccount {
    label: Option<String>,
    token: Option<String>,
}

/// Sửa tài khoản đã có: đổi nhãn và/hoặc thay token (kiểm tra token mới với DeepSeek, không trùng tài khoản khác).
/// Đổi token thì xoá lỗi/cooldown và mọi phiên web gắn với tài khoản này.
pub(crate) async fn api_edit_account(
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

pub(crate) async fn api_delete_account(
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
        *st.login_notice.lk() = None;
        // profile Chrome riêng của acc (hàng trăm MB) chỉ phục vụ acc này — xoá cùng lúc
        if let Some(seed) = profile_seed {
            crate::creds::remove(&seed);
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
pub(crate) async fn api_reset_account(
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
                crate::client::Probe::Muted(u) => crate::chat::mute_status(u),
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
pub(crate) async fn api_verify_account(
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
                crate::client::Probe::Muted(u) => {
                    a.last_status = Some(crate::chat::mute_status(u));
                    a.clear_auth_dead();
                    a.stats.cooldown_until = if u > now() { u } else { now() + 3600 };
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

