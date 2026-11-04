//! Handler quản trị: khởi động lại, DSH, settings, nhật ký, usage, trang dashboard.

#[allow(unused_imports)]
use super::*;

// ---------- handlers ----------

// ---------- khởi động lại ----------

pub(crate) static RESTART_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub(crate) static RESTART_SINCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) fn boot_id() -> u64 {
    static B: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *B.get_or_init(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0))
}

pub(crate) fn total_in_flight(st: &AppState) -> u32 {
    st.in_flight.lk().values().map(|v| v.load(std::sync::atomic::Ordering::Acquire)).sum()
}

#[derive(Deserialize)]
pub(crate) struct RestartReq { mode: String }

/// mode: "idle" = chờ đến lúc không còn request nào rồi khởi động lại; "now" = hard reset ngay; "cancel" = huỷ lệnh chờ.
pub(crate) async fn api_restart(State(st): State<Arc<AppState>>, Json(r): Json<RestartReq>) -> (StatusCode, Json<Value>) {
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

pub(crate) async fn api_dsh_status() -> Json<Value> {
    Json(crate::setup_dsh::status())
}

#[derive(Deserialize, Default)]
pub(crate) struct DshReq {
    preset: Option<String>,
    /// Tên các profile cần cấu hình (desktop, web, headless, ...). Bỏ trống = tất cả profile đang có.
    profiles: Option<Vec<String>>,
    /// true = cấu hình mọi profile tìm thấy, kể cả khi profiles có giá trị.
    all: Option<bool>,
}

/// Nút "Cấu hình dsh": ghi provider whalegate vào các profile dsh của máy đang chạy gateway.
/// Mặc định ghi TẤT CẢ profile đang có — trên Windows/macOS người dùng Desktop app dùng profile
/// "desktop", không phải "web", nên chỉ ghi web là họ mở app lên vẫn không thấy gateway.
pub(crate) async fn api_dsh_setup(body: Option<Json<DshReq>>) -> Json<Value> {
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

pub(crate) async fn api_restart_status(State(st): State<Arc<AppState>>) -> Json<Value> {
    use std::sync::atomic::Ordering::SeqCst;
    Json(json!({"pending": RESTART_PENDING.load(SeqCst), "since": RESTART_SINCE.load(SeqCst), "in_flight": total_in_flight(&st), "boot": boot_id()}))
}

pub(crate) async fn restart_when_idle(st: Arc<AppState>) {
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


pub(crate) async fn api_settings_get() -> Json<Value> {
    Json(crate::settings::snapshot())
}

/// Body: {"KEY": "value"} (nhiều khoá một lần) — value null hoặc "" = bỏ override, quay về env/mặc định.
pub(crate) async fn api_settings_set(Json(body): Json<std::collections::BTreeMap<String, Value>>) -> (StatusCode, Json<Value>) {
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
pub(crate) struct LogsQ { after: Option<u64>, limit: Option<usize> }

pub(crate) async fn api_logs(axum::extract::Query(q): axum::extract::Query<LogsQ>) -> Json<Value> {
    let lines = crate::logbuf::since(q.after.unwrap_or(0), q.limit.unwrap_or(500).min(3000));
    Json(json!({ "lines": lines }))
}

#[derive(Deserialize)]
pub(crate) struct ExportQ { hours: Option<u64> }

/// Phần đầu của gói debug: phiên bản, cài đặt (không bí mật) và tóm tắt từng acc — KHÔNG có token / cookie / nhãn.
pub(crate) async fn export_header(st: &Arc<AppState>) -> String {
    let now_ms = crate::logbuf::now_ms();
    let mut h = String::new();
    h.push_str(&format!("whalegate v{} {}/{}\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH));
    h.push_str(&format!("xuất lúc {} (DS_TZ_OFFSET={}s)\n", crate::logbuf::fmt_ts(now_ms).1, crate::settings::int("DS_TZ_OFFSET")));
    h.push_str(&format!("thư mục log: {}\n", crate::logbuf::logs_dir().display()));
    h.push_str("\n===== CÀI ĐẶT (không có bí mật) =====\n");
    for d in crate::settings::DEFS.iter().filter(|d| d.show) {
        let v = crate::settings::get(d.key);
        // danh sách proxy có thể chứa user:pass → chỉ ghi số dòng
        let shown = if d.key == "DS_PROXY_LIST" { format!("<{} dòng>", v.lines().filter(|l| !l.trim().is_empty()).count()) }
            else if d.key == "DS_EGRESS_WORKERS" { format!("<{} Worker, khoá đã ẩn>", crate::egress::parse_workers(&v).len()) }
            else { v };
        h.push_str(&format!("{}={}\n", d.key, shown));
    }
    h.push_str("\n===== CÀI ĐẶT ẨN / NÂNG CAO (không hiện trên giao diện; giá trị đang áp dụng) =====\n");
    for d in crate::settings::DEFS.iter().filter(|d| !d.show && d.key != "DS_PROXY_LIST" && d.key != "DS_EGRESS_KEY") {
        h.push_str(&format!("{}={}\n", d.key, crate::settings::get(d.key)));
    }
    h.push_str("\n===== TÀI KHOẢN (id 8 ký tự khớp với dòng log [route]; không token/cookie/nhãn) =====\n");
    let store = st.store.read().await;
    let t = now();
    for a in &store.accounts {
        let ck = cookie_string(&a.cookies);
        h.push_str(&format!(
            "{} | {} | status={} | nghỉ={}s | fail_streak={} | ok/rỗng/lỗi={}/{}/{} | {} | chrome-riêng={} | cookie .thumbcache={} | cookie ds_session_id={} | rate_limit: streak={} trần/phút={} gọi 60s={}\n",
            &a.id[..8.min(a.id.len())],
            if a.pool_enabled { "bật" } else { "tắt" },
            a.last_status.clone().unwrap_or_default().chars().take(80).collect::<String>(),
            a.stats.cooldown_until.saturating_sub(t), a.stats.fail_streak, a.stats.total_ok, a.stats.total_empty, a.stats.total_error,
            if a.auth_dead_at.is_some() { "DS ĐÃ ĐĂNG XUẤT" } else { "token ok" },
            if a.fp_seed.is_some() { "có" } else { "không" },
            if crate::client::shumei_id_from_cookies(&ck).is_some() { "có" } else { "không" },
            if a.cookies.contains_key("ds_session_id") { "có" } else { "không" },
            a.stats.rl_streak, a.stats.rpm_cap(t).map_or("-".to_string(), |c| c.to_string()), a.stats.calls_last_minute(t),
        ));
    }
    h
}

/// Tải gói debug: phần đầu + log và sự kiện chi tiết đã lưu ra file trong `?hours=` giờ gần nhất (mặc định 24, tối đa 7 ngày).
pub(crate) async fn api_logs_export(State(st): State<Arc<AppState>>, axum::extract::Query(q): axum::extract::Query<ExportQ>) -> Response {
    let hours = q.hours.unwrap_or(24).clamp(1, 24 * 7);
    let header = export_header(&st).await;
    let (log, dbg) = tokio::task::spawn_blocking(move || crate::logbuf::read_recent(hours, 12 << 20)).await.unwrap_or_default();
    let body = format!("{header}\n===== LOG ({hours} giờ gần nhất) =====\n{log}\n===== DEBUG CHI TIẾT (JSONL, mỗi dòng một sự kiện) =====\n{dbg}");
    let stamp: String = crate::logbuf::fmt_ts(crate::logbuf::now_ms()).1.chars().take(19).filter(|c| c.is_ascii_digit() || *c == ' ').collect::<String>().replace(' ', "-");
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
            (axum::http::header::CONTENT_DISPOSITION, format!("attachment; filename=\"whalegate-debug-{stamp}.txt\"")),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        body,
    ).into_response()
}

/// SSE: gửi backlog (`?backlog=N`, mặc định 300) rồi phát trực tiếp từng dòng mới.
pub(crate) async fn api_logs_stream(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>)
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

pub(crate) async fn api_usage() -> Json<Value> {
    Json(tokio::task::spawn_blocking(crate::usage::report).await.unwrap_or_else(|_| json!({})))
}

pub(crate) async fn index() -> Html<&'static str> {
    Html(index_html())
}

/// ui.html kèm phiên bản đang chạy (thay `__WG_VERSION__` một lần, giữ lại cho mọi lần tải trang).
pub(crate) fn index_html() -> &'static str {
    static HTML: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HTML.get_or_init(|| include_str!("../ui.html").replace("__WG_VERSION__", env!("CARGO_PKG_VERSION")).replace("/*__WG_I18N__*/", include_str!("../i18n.js")))
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

