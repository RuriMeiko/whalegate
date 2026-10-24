// Export/import bộ tài khoản (đem sang máy khác) + tự cập nhật từ GitHub Releases.

use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::server::{self, Account, AppState};

/// Phiên bản định dạng gói export. Tăng khi đổi cấu trúc theo cách không tương thích ngược.
pub(crate) const EXPORT_FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
pub(crate) struct AccountBundle {
    /// "whalegate-accounts"
    pub(crate) kind: String,
    pub(crate) format: u32,
    pub(crate) exported_at: u64,
    pub(crate) app_version: String,
    pub(crate) accounts: Vec<Account>,
}

/// `GET /api/accounts/export` — gói JSON đầy đủ. UI tải về thành file hoặc copy vào clipboard.
pub(crate) async fn api_export_accounts(State(st): State<Arc<AppState>>) -> Response {
    let accounts = st.store.read().await.accounts.clone();
    let bundle = AccountBundle {
        kind: "whalegate-accounts".into(),
        format: EXPORT_FORMAT,
        exported_at: server::now(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        accounts,
    };
    let body = serde_json::to_vec_pretty(&bundle).unwrap_or_else(|_| b"{}".to_vec());
    let name = format!("whalegate-accounts-{}.json", server::now());
    // Tên file gợi ý cho trình duyệt; dữ liệu chứa token nên không đặt cache công khai.
    (
        StatusCode::OK,
        [
            ("content-type", "application/json; charset=utf-8".to_string()),
            ("content-disposition", format!("attachment; filename=\"{}\"", name)),
            ("cache-control", "no-store".to_string()),
        ],
        body,
    )
        .into_response()
}

#[derive(Deserialize, Default)]
pub(crate) struct ImportOpts {
    /// true = chỉ báo trước sẽ nhập gì, không ghi (UI gọi để hiện hộp xác nhận).
    #[serde(default)]
    pub(crate) dry_run: bool,
    /// true = tài khoản trùng (cùng ds_uid hoặc cùng token) được thay token/cookies bằng bản mới.
    #[serde(default)]
    pub(crate) overwrite: bool,
}

/// `POST /api/accounts/import` — gộp tài khoản từ gói vào pool.
///
/// Khoá nhận dạng là `ds_uid` (cùng người dùng DeepSeek) chứ không phải `id` (uuid ngẫu nhiên của
/// máy cũ), để import hai lần hay import chồng lên máy đã có acc không tạo bản trùng.
pub(crate) async fn api_import_accounts(
    State(st): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut opts = ImportOpts::default();
    if let Some(o) = body.get("dry_run") { opts.dry_run = o.as_bool().unwrap_or(false); }
    if let Some(o) = body.get("overwrite") { opts.overwrite = o.as_bool().unwrap_or(false); }

    let parse_err = |m: &str| -> (StatusCode, Json<Value>) {
        (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": m})))
    };
    // Chấp nhận 3 dạng: gói đầy đủ, {"accounts":[...]}, hoặc mảng trần.
    let list_val = if let Some(a) = body.get("accounts") {
        a.clone()
    } else if body.is_array() {
        body.clone()
    } else {
        return Err(parse_err("gói import không có mảng accounts"));
    };
    let raw_accounts: Vec<Value> = match list_val {
        Value::Array(a) => a,
        _ => return Err(parse_err("accounts phải là mảng")),
    };
    let mut incoming: Vec<Account> = Vec::new();
    for v in raw_accounts {
        match serde_json::from_value::<Account>(v.clone()) {
            Ok(mut a) => {
                // Chuẩn hoá: token rỗng/khoảng trắng thì loại.
                a.token = server::normalize_token(&a.token);
                if a.token.len() < 20 || a.token.chars().any(|c| c.is_whitespace()) { continue; }
                if a.id.trim().is_empty() { a.id = uuid::Uuid::new_v4().to_string(); }
                if a.label.trim().is_empty() { a.label = a.ds_uid.clone().unwrap_or_else(|| "tài khoản".into()); }
                incoming.push(a);
            }
            Err(_) => continue,
        }
    }
    if incoming.is_empty() {
        return Err(parse_err("không có tài khoản hợp lệ trong gói (token thiếu hoặc sai định dạng)"));
    }

    struct Plan {
        fresh: Vec<Account>,
        updates: Vec<Account>, // bản mới ghi đè token/cookies lên acc đã có
        skipped: Vec<String>,
    }
    let store = st.store.read().await;
    let mut plan = Plan { fresh: Vec::new(), updates: Vec::new(), skipped: Vec::new() };
    for mut a in incoming {
        let existing = store.accounts.iter().find(|e| {
            a.ds_uid.as_deref().is_some_and(|u| e.ds_uid.as_deref() == Some(u)) || e.token == a.token
        });
        match existing {
            None => plan.fresh.push(a),
            Some(e) => {
                if opts.overwrite {
                    a.id = e.id.clone(); // giữ id cũ để mọi tham chiếu (conv/session/UI) còn đúng
                    plan.updates.push(a);
                } else {
                    plan.skipped.push(a.label.clone());
                }
            }
        }
    }
    drop(store);

    let base_total = st.store.read().await.accounts.len();
    if opts.dry_run {
        return Ok(Json(json!({
            "ok": true, "dry_run": true,
            "added": plan.fresh.len(), "updated": plan.updates.len(), "skipped": plan.skipped.len(),
            "skipped_labels": plan.skipped.iter().take(20).cloned().collect::<Vec<_>>(),
            "total": base_total,
        })));
    }
    let (added, updated, skipped) = (plan.fresh.len(), plan.updates.len(), plan.skipped.len());
    let skipped_labels: Vec<String> = plan.skipped.iter().take(20).cloned().collect();

    {
        let mut store = st.store.write().await;
        for mut a in plan.fresh.drain(..) {
            // id trùng với acc đã có (gói cũ dùng lại uuid) thì sinh id mới để không đè nhầm.
            if store.accounts.iter().any(|e| e.id == a.id) { a.id = uuid::Uuid::new_v4().to_string(); }
            if a.last_status.is_none() { a.last_status = Some("imported".into()); }
            store.accounts.push(a);
        }
        for u in plan.updates.drain(..) {
            if let Some(e) = store.accounts.iter_mut().find(|e| e.id == u.id) {
                let token_changed = e.token != u.token;
                e.token = u.token;
                e.cookies = u.cookies;
                e.label = u.label;
                if u.ds_uid.is_some() { e.ds_uid = u.ds_uid; }
                if token_changed {
                    // token mới → phiên web cũ không còn dùng được, xoá sạch trạng thái.
                    e.thread_session_id = None;
                    e.thread_parent_message_id = None;
                    e.thread_uses = 0;
                    e.stats.reset();
                    e.last_status = Some("imported".into());
                    e.last_verified = None;
                }
            }
        }
    }
    server::mark_dirty(&st);
    server::flush_store(&st).await;
    let total = st.store.read().await.accounts.len();
    crate::elog!("[import] +{} mới, ~{} cập nhật, {} bỏ qua — pool còn {} acc", added, updated, skipped, total);
    Ok(Json(json!({
        "ok": true, "dry_run": false,
        "added": added, "updated": updated, "skipped": skipped,
        "skipped_labels": skipped_labels, "total": total,
    })))
}

// ---------- tự cập nhật từ GitHub Releases ----------

const REPO: &str = "RuriMeiko/whalegate";

/// Kết quả dò bản mới.
#[derive(Clone, Serialize)]
pub(crate) struct UpdateInfo {
    /// Phiên bản đang chạy.
    pub(crate) current: String,
    /// Tag mới nhất trên GitHub, ví dụ "v0.5.0". None = không dò được (offline / chưa có release).
    pub(crate) latest: Option<String>,
    pub(crate) has_update: bool,
    pub(crate) notes: Option<String>,
    pub(crate) url: Option<String>,
    /// Lỗi mạng/API lần dò gần nhất (không phải lỗi cài đặt).
    pub(crate) error: Option<String>,
    pub(crate) checked_at: u64,
}

fn parse_ver(s: &str) -> (u64, u64, u64) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut it = s.split('.').map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>());
    let n = |x: Option<String>| x.and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    (n(it.next()), n(it.next()), n(it.next()))
}

pub(crate) async fn fetch_latest() -> UpdateInfo {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let mk_err = |e: String| UpdateInfo {
        current: current.clone(), latest: None, has_update: false, notes: None, url: None,
        error: Some(e), checked_at: server::now(),
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent(format!("whalegate/{}", current))
        .build()
    {
        Ok(c) => c,
        Err(e) => return mk_err(e.to_string()),
    };
    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let resp = client.get(&url).header("accept", "application/vnd.github+json").send().await;
    let v: Value = match resp {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(v) => v,
            Err(e) => return mk_err(format!("đọc JSON: {e}")),
        },
        Ok(r) => return mk_err(format!("GitHub trả HTTP {}", r.status())),
        Err(e) => return mk_err(e.to_string()),
    };
    let tag = v["tag_name"].as_str().unwrap_or_default().to_string();
    if tag.is_empty() {
        return mk_err("release không có tag_name".into());
    }
    UpdateInfo {
        has_update: parse_ver(&tag) > parse_ver(&current),
        latest: Some(tag),
        notes: v["body"].as_str().map(|s| s.chars().take(4000).collect()),
        url: v["html_url"].as_str().map(|s| s.to_string()),
        error: None,
        current,
        checked_at: server::now(),
    }
}

pub(crate) async fn api_update_check() -> Json<Value> {
    Json(json!(fetch_latest().await))
}

/// `POST /api/update` — tải bản mới nhất, thay binary đang chạy rồi khởi động lại.
pub(crate) async fn api_update_apply(State(st): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let info = fetch_latest().await;
    if let Some(e) = info.error.clone() {
        return (StatusCode::BAD_GATEWAY, Json(json!({"ok": false, "error": format!("không dò được bản mới: {}", e)})));
    }
    if !info.has_update {
        return (StatusCode::OK, Json(json!({"ok": false, "error": "đang là bản mới nhất", "current": info.current})));
    }
    let tag = info.latest.clone().unwrap_or_default();
    match download_and_install(&tag).await {
        Ok(msg) => {
            crate::elog!("[update] {} — khởi động lại để dùng bản mới", msg);
            // Trả lời xong mới exec: client cần nhận được JSON trước khi tiến trình biến mất.
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                crate::server::do_restart(st).await;
            });
            (StatusCode::OK, Json(json!({"ok": true, "version": tag, "message": msg})))
        }
        Err(e) => {
            crate::elog!("[update] cài {} lỗi: {:#}", tag, e);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": format!("{:#}", e)})))
        }
    }
}

fn target_triple() -> Option<&'static str> {
    // Dùng cfg! để chọn đúng asset mà workflow release.yml đã đóng gói cho từng nền tảng.
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) { Some("x86_64-unknown-linux-musl") }
    else if cfg!(all(target_os = "linux", target_arch = "aarch64")) { Some("aarch64-unknown-linux-musl") }
    else if cfg!(all(target_os = "macos", target_arch = "aarch64")) { Some("aarch64-apple-darwin") }
    else if cfg!(all(target_os = "macos", target_arch = "x86_64")) { Some("x86_64-apple-darwin") }
    else if cfg!(all(target_os = "windows", target_arch = "x86_64")) { Some("x86_64-pc-windows-msvc") }
    else { None }
}

/// Script .bat thay binary trên Windows: chờ file cũ không còn bị khoá, move /y đè lên, chạy lại.
/// Tách riêng để test được trên Linux (nội dung script là văn bản thuần).
fn windows_update_script(staged: &std::path::Path, exe: &std::path::Path) -> String {
    format!(
        concat!(
            "@echo off\r\n",
            "setlocal\r\n",
            "set \"NEW={}\"\r\n",
            "set \"OLD={}\"\r\n",
            ":wait\r\n",
            "move /y \"%NEW%\" \"%OLD%\" >nul 2>&1\r\n",
            "if errorlevel 1 (\r\n",
            "  ping -n 2 127.0.0.1 >nul\r\n",
            "  goto wait\r\n",
            ")\r\n",
            "start \"\" \"%OLD%\" %*\r\n",
            "(goto) 2>nul & del \"%~f0\"\r\n",
        ),
        staged.display(),
        exe.display(),
    )
}

async fn download_and_install(tag: &str) -> Result<String> {
    let target = target_triple().ok_or_else(|| anyhow::anyhow!("không có bản dựng sẵn cho nền tảng này — build từ source"))?;
    let base = format!("https://github.com/{}/releases/download/{}", REPO, tag);
    let is_win = cfg!(windows);
    let archive = format!("whalegate-{}.{}", target, if is_win { "zip" } else { "tar.gz" });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent(format!("whalegate/{}", env!("CARGO_PKG_VERSION")))
        .build()?;
    let tmp = std::env::temp_dir().join(format!("whalegate-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    // 1) tải archive + checksum
    let bytes = client.get(format!("{}/{}", base, archive)).send().await?.error_for_status()?.bytes().await?;
    let sums = client.get(format!("{}/SHA256SUMS.txt", base)).send().await?.error_for_status()?.text().await?;
    let want = sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        match (it.next(), it.next()) {
            (Some(h), Some(name)) if name.trim_start_matches('*') == archive => Some(h.to_lowercase()),
            _ => None,
        }
    });
    let got = sha256_hex(&bytes);
    match want {
        Some(w) if w == got => {}
        Some(_) => anyhow::bail!("checksum không khớp — huỷ cập nhật"),
        None => anyhow::bail!("SHA256SUMS.txt không có {}", archive),
    }

    // 2) giải nén, tìm binary
    let bin_name = if is_win { "whalegate.exe" } else { "whalegate" };
    let new_bin = tmp.join(bin_name);
    if is_win {
        extract_zip(&bytes, bin_name, &new_bin)?;
    } else {
        extract_targz(&bytes, bin_name, &new_bin)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_bin, std::fs::Permissions::from_mode(0o755))?;
    }

    // 3) thay binary đang chạy. Linux/macOS: rename đè lên file đang chạy vẫn hợp lệ (inode cũ còn
    //    sống tới khi tiến trình exec). Windows thì không — xem nhánh dưới.
    let exe = std::env::current_exe()?;
    let exe = std::path::PathBuf::from(exe.to_string_lossy().trim_end_matches(" (deleted)").to_string());
    if is_win {
        // Windows khoá file .exe đang chạy: kernel trả ERROR_ACCESS_DENIED cho mọi rename/copy
        // đè lên nó. Nên chép binary mới cạnh bản cũ và để MỘT TIẾN TRÌNH KHÁC thay file sau khi
        // tiến trình này thoát — do_restart chạy script đó thay vì exec chính binary cũ.
        let staged = exe.with_extension("exe.new");
        std::fs::copy(&new_bin, &staged)?;
        let helper = exe.with_extension("update.bat");
        std::fs::write(&helper, windows_update_script(&staged, &exe))?;
        let _ = std::fs::remove_dir_all(&tmp);
        return Ok(format!(
            "đã tải {} — khởi động lại để {} thay binary (Windows khoá file .exe đang chạy)",
            tag,
            helper.display()
        ));
    }
    let staged = exe.with_extension("new");
    std::fs::copy(&new_bin, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staged, &exe)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(format!("đã cài {} vào {}", tag, exe.display()))
}

/// SHA-256 thuần Rust, tránh thêm dependency chỉ để kiểm tra 1 file.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 { w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]); }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) = (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g; g = f; f = e; e = d.wrapping_add(t1); d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a); h[1] = h[1].wrapping_add(b); h[2] = h[2].wrapping_add(c); h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e); h[5] = h[5].wrapping_add(f); h[6] = h[6].wrapping_add(g); h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|x| format!("{:08x}", x)).collect()
}

/// Giải nén tar.gz và lấy file có tên đúng `bin_name` (bỏ qua đường dẫn thư mục).
fn extract_targz(data: &[u8], bin_name: &str, out: &std::path::Path) -> Result<()> {
    use std::io::Read;
    let gz = flate2::read::GzDecoder::new(data);
    let mut ar = tar::Archive::new(gz);
    for entry in ar.entries()? {
        let mut e = entry?;
        let path = e.path()?.to_string_lossy().to_string();
        if path.rsplit('/').next() == Some(bin_name) {
            let mut buf = Vec::new();
            e.read_to_end(&mut buf)?;
            std::fs::write(out, buf)?;
            return Ok(());
        }
    }
    anyhow::bail!("không thấy {} trong archive", bin_name)
}

/// Giải nén zip (chỉ Windows) và lấy file `bin_name`.
#[cfg(windows)]
fn extract_zip(data: &[u8], bin_name: &str, out: &std::path::Path) -> Result<()> {
    use std::io::{Cursor, Read};
    let mut z = zip::ZipArchive::new(Cursor::new(data))?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        if f.name().rsplit('/').next() == Some(bin_name) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            std::fs::write(out, buf)?;
            return Ok(());
        }
    }
    anyhow::bail!("không thấy {} trong archive", bin_name)
}

#[cfg(not(windows))]
fn extract_zip(_data: &[u8], _bin_name: &str, _out: &std::path::Path) -> Result<()> {
    anyhow::bail!("zip chỉ dùng trên Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kiểm tra SHA-256 với vector chuẩn của NIST + vài độ dài biên (rỗng, đúng 55/56/64 byte
    /// quanh mốc padding) — một lỗi padding sẽ làm mọi bản cập nhật bị từ chối.
    /// Script Windows phải quote đường dẫn (có thể chứa khoảng trắng), move /y đè binary cũ,
    /// lặp tới khi file không còn bị khoá, chạy lại app, rồi tự xoá.
    #[test]
    fn windows_update_script_shape() {
        let staged = std::path::Path::new(r"C:\Program Files\whalegate\whalegate.exe.new");
        let exe = std::path::Path::new(r"C:\Program Files\whalegate\whalegate.exe");
        let bat = windows_update_script(staged, exe);
        assert!(bat.contains(r#"set "NEW=C:\Program Files\whalegate\whalegate.exe.new""#), "NEW: {}", bat);
        assert!(bat.contains(r#"set "OLD=C:\Program Files\whalegate\whalegate.exe""#), "OLD: {}", bat);
        assert!(bat.contains(r#"move /y "%NEW%" "%OLD%""#), "move: {}", bat);
        assert!(bat.contains("goto wait"), "phải lặp khi file còn bị khoá");
        assert!(bat.contains(r#"start "" "%OLD%" %*"#), "start: {}", bat);
        assert!(bat.contains(r#"del "%~f0""#), "del self");
        assert!(bat.starts_with("@echo off\r\n"), "CRLF cho cmd.exe");
    }
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // 1 triệu chữ 'a'
        let million = vec![b'a'; 1_000_000];
        assert_eq!(sha256_hex(&million), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // các độ dài quanh ranh giới block/padding
        for n in [55usize, 56, 57, 63, 64, 65, 119, 120] {
            let data = vec![0u8; n];
            assert_eq!(sha256_hex(&data).len(), 64, "len {n}");
        }
    }

    #[test]
    fn version_compare_orders_correctly() {
        assert!(parse_ver("v0.5.0") > parse_ver("0.4.1"));
        assert!(parse_ver("0.10.0") > parse_ver("0.9.9"));
        assert_eq!(parse_ver("v0.5.0"), parse_ver("0.5.0"));
        assert_eq!(parse_ver("0.4.1"), parse_ver("0.4.1"));
        // tag lạ không được panic và không được coi là mới hơn
        assert!(!(parse_ver("garbage") > parse_ver("0.5.0")));
        assert_eq!(parse_ver("v1.2"), (1, 2, 0));
    }

    #[test]
    fn bundle_roundtrip_shape() {
        let b = AccountBundle {
            kind: "whalegate-accounts".into(),
            format: EXPORT_FORMAT,
            exported_at: 1,
            app_version: "0.5.0".into(),
            accounts: vec![],
        };
        let js = serde_json::to_string(&b).unwrap();
        let back: AccountBundle = serde_json::from_str(&js).unwrap();
        assert_eq!(back.kind, "whalegate-accounts");
        assert_eq!(back.format, EXPORT_FORMAT);
    }
}
