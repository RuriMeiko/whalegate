// Sao lưu / khôi phục NGUYÊN VẸN để chuyển whalegate sang máy khác: một file `.tar.gz` gồm
//   accounts.json         toàn bộ bản ghi tài khoản (token, cookie, vân tay, phương thức đăng nhập…) — cùng định dạng export
//   settings.json         cài đặt đã đổi trên dashboard
//   proxies.txt           danh sách proxy
//   proxy-bindings.json   ghim vân tay → proxy (để mỗi "máy" giữ nguyên IP)
//   usage.json            lịch sử dùng theo giờ
//   profiles/<seed>/…     phần cần thiết của profile Chrome riêng từng tài khoản (localStorage, IndexedDB, cookie, tuỳ chọn)
//   manifest.json         loại gói, phiên bản, số lượng
// Profile Chrome chỉ lấy phần định danh (vài trăm KB mỗi cái) chứ không lấy cache (hàng trăm MB).
// Khi đọc gói: chặn path traversal, link, và giới hạn dung lượng giải nén (chống zip bomb).

use anyhow::{bail, Context, Result};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::server::{self, Account, AppState};

pub(crate) const BACKUP_FORMAT: u32 = 1;
const MAX_TOTAL: u64 = 512 << 20; // tổng dung lượng sau giải nén
const MAX_FILE: u64 = 128 << 20;
const MAX_ENTRIES: usize = 100_000;

/// File lẻ của một profile Chrome đáng mang đi (đường dẫn tương đối, dấu `/`).
const PROFILE_FILES: &[&str] = &["Local State", "Default/Preferences", "Default/Cookies", "Default/Network/Cookies"];
/// Thư mục đáng mang đi: localStorage (userToken, mã thiết bị, Shumei), sessionStorage, IndexedDB.
const PROFILE_DIRS: &[&str] = &["Default/Local Storage", "Default/Session Storage", "Default/IndexedDB"];

/// Seed là tên thư mục profile: chỉ cho chữ, số, `-`, `_`, `.` (không bắt đầu bằng `.`) — chặn `..` và dấu `/`.
pub(crate) fn valid_seed(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let r = format!("{rel}/{name}");
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() { continue; }
        if ft.is_dir() { walk(&e.path(), &r, out); }
        else if ft.is_file() && name != "LOCK" && e.metadata().map(|m| m.len() <= MAX_FILE).unwrap_or(false) { out.push((r, e.path())); }
    }
}

/// Các file cần mang đi của một thư mục profile Chrome (theo danh sách cho phép ở trên).
pub(crate) fn profile_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for f in PROFILE_FILES {
        let p = root.join(f);
        if p.is_file() { out.push(((*f).to_string(), p)); }
    }
    for d in PROFILE_DIRS {
        let p = root.join(d);
        if p.is_dir() { walk(&p, d, &mut out); }
    }
    out
}

fn tar_add<W: std::io::Write>(tb: &mut tar::Builder<W>, name: &str, data: &[u8]) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o600);
    h.set_mtime(server::now());
    tb.append_data(&mut h, name, data).with_context(|| format!("thêm {name} vào gói"))
}

/// Dựng gói sao lưu. Trả (nội dung .tar.gz, tóm tắt).
pub(crate) fn build_backup(accounts: Vec<Account>) -> Result<(Vec<u8>, Value)> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tb = tar::Builder::new(gz);
    let n_acc = accounts.len();

    // seed của profile Chrome riêng: chỉ acc có fp_seed (đăng nhập qua Chrome riêng) và thư mục còn tồn tại
    let mut seeds: Vec<String> = accounts.iter().filter_map(|a| a.fp_seed.clone()).filter(|s| valid_seed(s)).collect();
    seeds.sort();
    seeds.dedup();

    let bundle = crate::transfer::AccountBundle {
        kind: "whalegate-accounts".into(),
        format: crate::transfer::EXPORT_FORMAT,
        exported_at: server::now(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        accounts,
    };
    tar_add(&mut tb, "accounts.json", &serde_json::to_vec_pretty(&bundle)?)?;

    let ov = crate::settings::overrides_map();
    if !ov.is_empty() { tar_add(&mut tb, "settings.json", &serde_json::to_vec_pretty(&ov)?)?; }
    if let Some(t) = crate::proxypool::proxies_file_text() { tar_add(&mut tb, "proxies.txt", t.as_bytes())?; }
    let bind = crate::proxypool::bindings_map();
    if !bind.is_empty() { tar_add(&mut tb, "proxy-bindings.json", &serde_json::to_vec_pretty(&bind)?)?; }
    let usage = crate::usage::export_json();
    if usage.len() > 2 { tar_add(&mut tb, "usage.json", usage.as_bytes())?; }

    let (mut n_prof, mut n_files) = (0usize, 0usize);
    for seed in &seeds {
        let root = PathBuf::from(crate::chrome_capture::account_profile_dir(seed));
        let files = profile_files(&root);
        if files.is_empty() { continue; }
        n_prof += 1;
        for (rel, abs) in files {
            let Ok(data) = std::fs::read(&abs) else { continue };
            tar_add(&mut tb, &format!("profiles/{seed}/{rel}"), &data)?;
            n_files += 1;
        }
    }

    let summary = json!({
        "accounts": n_acc, "settings": ov.len(), "proxy_bindings": bind.len(), "proxies": crate::proxypool::proxies_file_text().is_some(),
        "usage": usage.len() > 2, "profiles": n_prof, "profile_files": n_files,
    });
    let manifest = json!({
        "kind": "whalegate-backup", "format": BACKUP_FORMAT, "exported_at": server::now(), "app_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS, "arch": std::env::consts::ARCH, "contents": summary,
        "note": "Chứa token, cookie và dữ liệu đăng nhập của Chrome: coi như mật khẩu.",
    });
    tar_add(&mut tb, "manifest.json", &serde_json::to_vec_pretty(&manifest)?)?;
    let gz = tb.into_inner()?;
    Ok((gz.finish()?, summary))
}

/// Chuẩn hoá đường dẫn trong gói: chỉ thành phần thường, dấu `/`; chặn `..`, đường dẫn tuyệt đối, ký tự lạ.
fn clean_entry_path(p: &Path) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().context("tên file không phải UTF-8")?;
                if s.contains('\\') || s.contains('\0') { bail!("tên file không hợp lệ: {s:?}"); }
                parts.push(s.to_string());
            }
            Component::CurDir => {}
            _ => bail!("đường dẫn không an toàn trong gói: {}", p.display()),
        }
    }
    if parts.is_empty() { bail!("đường dẫn rỗng trong gói"); }
    Ok(parts.join("/"))
}

/// Đọc cả gói vào bộ nhớ (tên → nội dung), có giới hạn dung lượng giải nén.
pub(crate) fn read_backup(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let dec = flate2::read::GzDecoder::new(bytes).take(MAX_TOTAL + 1);
    let mut ar = tar::Archive::new(dec);
    let mut out = BTreeMap::new();
    let mut total = 0u64;
    for e in ar.entries().context("không đọc được gói (phải là .tar.gz của whalegate)")? {
        let mut e = e.context("gói hỏng")?;
        let et = e.header().entry_type();
        if et.is_dir() { continue; }
        if !et.is_file() { bail!("gói chứa mục không phải file thường (link/thiết bị) — từ chối"); }
        let size = e.header().size()?;
        if size > MAX_FILE { bail!("một file trong gói quá lớn ({} MB)", size >> 20); }
        total += size;
        if total > MAX_TOTAL { bail!("gói giải nén vượt {} MB", MAX_TOTAL >> 20); }
        let name = clean_entry_path(&e.path()?)?;
        let mut buf = Vec::with_capacity(size as usize);
        e.read_to_end(&mut buf)?;
        out.insert(name, buf);
        if out.len() > MAX_ENTRIES { bail!("gói có quá nhiều file"); }
    }
    Ok(out)
}

/// `GET /api/backup/export` — tải bản sao lưu nguyên vẹn.
pub(crate) async fn api_backup_export(State(st): State<Arc<AppState>>) -> Response {
    let accounts = st.store.read().await.accounts.clone();
    match tokio::task::spawn_blocking(move || build_backup(accounts)).await {
        Ok(Ok((bytes, summary))) => {
            crate::elog!("[backup] xuất {} byte: {}", bytes.len(), summary);
            let stamp: String = crate::logbuf::fmt_ts(crate::logbuf::now_ms()).1.chars().take(19).filter(|c| c.is_ascii_digit() || *c == ' ').collect::<String>().replace(' ', "-");
            (
                StatusCode::OK,
                [
                    ("content-type", "application/gzip".to_string()),
                    ("content-disposition", format!("attachment; filename=\"whalegate-backup-{stamp}.tar.gz\"")),
                    ("cache-control", "no-store".to_string()),
                ],
                bytes,
            ).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": format!("{e:#}")}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok": false, "error": e.to_string()}))).into_response(),
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct BackupQ {
    dry_run: Option<String>,
    overwrite: Option<String>,
}

/// `?dry_run=1` hay `=true` đều được.
fn truthy(v: &Option<String>) -> bool { matches!(v.as_deref().map(str::trim), Some("1") | Some("true") | Some("yes") | Some("on")) }

fn bad(msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": msg.into()})))
}

/// `POST /api/backup/import?dry_run=1&overwrite=1` — thân là nội dung `.tar.gz`. Gộp, không xoá gì đang có:
/// tài khoản khớp theo `ds_uid`; cài đặt / proxy / ghim / giờ dùng / profile đã có thì giữ, trừ khi `overwrite`.
pub(crate) async fn api_backup_import(
    State(st): State<Arc<AppState>>,
    Query(q): Query<BackupQ>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (dry, overwrite) = (truthy(&q.dry_run), truthy(&q.overwrite));
    let files = tokio::task::spawn_blocking(move || read_backup(&body)).await
        .map_err(|e| bad(e.to_string()))?
        .map_err(|e| bad(format!("{e:#}")))?;

    let manifest: Value = files.get("manifest.json").and_then(|b| serde_json::from_slice(b).ok()).unwrap_or(Value::Null);
    if manifest["kind"] != "whalegate-backup" { return Err(bad("không phải bản sao lưu của whalegate (thiếu manifest.json)")); }
    if manifest["format"].as_u64().unwrap_or(0) > BACKUP_FORMAT as u64 {
        return Err(bad("bản sao lưu từ phiên bản whalegate mới hơn — hãy cập nhật whalegate rồi nhập lại"));
    }

    // 1) tài khoản — dùng đúng đường import sẵn có (khớp ds_uid, không tạo bản trùng)
    let acc_raw = files.get("accounts.json").ok_or_else(|| bad("gói không có accounts.json"))?;
    let acc_val: Value = serde_json::from_slice(acc_raw).map_err(|e| bad(format!("accounts.json hỏng: {e}")))?;
    let mut req = json!({"accounts": acc_val.get("accounts").cloned().unwrap_or(acc_val.clone()), "dry_run": dry, "overwrite": overwrite});
    if let Some(o) = req.as_object_mut() { o.insert("dry_run".into(), json!(dry)); }
    let accounts = crate::transfer::api_import_accounts(State(st.clone()), Json(req)).await?.0;

    // 2) cài đặt
    let (mut set_ok, mut set_skip) = (0, 0);
    if let Some(b) = files.get("settings.json") {
        if let Ok(m) = serde_json::from_slice::<HashMap<String, String>>(b) {
            (set_ok, set_skip) = crate::settings::import_overrides(&m, overwrite, dry);
        }
    }
    // 3) proxy: danh sách + ghim
    let mut proxies = false;
    let mut proxy_note = Value::Null;
    if let Some(b) = files.get("proxies.txt") {
        match crate::proxypool::import_proxies_file(&String::from_utf8_lossy(b), overwrite, dry) {
            Ok(w) => proxies = w,
            Err(e) => proxy_note = json!(format!("danh sách proxy trong gói không hợp lệ: {e:#}")),
        }
    }
    let mut bindings = 0;
    if let Some(b) = files.get("proxy-bindings.json") {
        if let Ok(m) = serde_json::from_slice::<HashMap<String, String>>(b) { bindings = crate::proxypool::import_bindings(&m, overwrite, dry); }
    }
    // 4) lịch sử dùng
    let usage_hours = files.get("usage.json").map(|b| crate::usage::import_json(&String::from_utf8_lossy(b), overwrite, dry)).unwrap_or(0);

    // 5) profile Chrome riêng: không động vào khi đang có cửa sổ đăng nhập, và không đè profile đã có trừ khi overwrite
    let mut by_seed: BTreeMap<String, Vec<(String, &Vec<u8>)>> = BTreeMap::new();
    for (name, data) in &files {
        let Some(rest) = name.strip_prefix("profiles/") else { continue };
        let Some((seed, rel)) = rest.split_once('/') else { continue };
        if !valid_seed(seed) { continue; }
        by_seed.entry(seed.to_string()).or_default().push((rel.to_string(), data));
    }
    let busy = *st.login_watching.lock().unwrap_or_else(|e| e.into_inner());
    let (mut restored, mut skipped, mut written) = (0usize, 0usize, 0usize);
    for (seed, list) in &by_seed {
        let root = PathBuf::from(crate::chrome_capture::account_profile_dir(seed));
        if busy || (root.join("Default").exists() && !overwrite) { skipped += 1; continue; }
        restored += 1;
        if dry { continue; }
        for (rel, data) in list {
            // rel đã được chuẩn hoá khi đọc gói; kiểm lại một lần nữa trước khi ghi
            if rel.split('/').any(|p| p.is_empty() || p == "..") { continue; }
            let dest = root.join(rel);
            if let Some(dir) = dest.parent() { let _ = std::fs::create_dir_all(dir); }
            let tmp = dest.with_extension("wg-tmp");
            if std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, &dest).is_ok() { written += 1; }
        }
    }

    crate::elog!("[backup] nhập{}: tài khoản {}, cài đặt +{}, ghim proxy +{}, giờ dùng +{}, profile {} (bỏ qua {}), {} file",
        if dry { " (xem trước)" } else { "" }, accounts, set_ok, bindings, usage_hours, restored, skipped, written);
    Ok(Json(json!({
        "ok": true, "dry_run": dry, "overwrite": overwrite, "app_version": manifest["app_version"], "os": manifest["os"],
        "accounts": accounts,
        "settings": {"applied": set_ok, "skipped": set_skip},
        "proxy": {"list_written": proxies, "bindings": bindings, "note": proxy_note},
        "usage": {"hours": usage_hours},
        "profiles": {"restored": restored, "skipped": skipped, "files": written, "busy": busy},
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tb = tar::Builder::new(gz);
        for (n, d) in entries { tar_add(&mut tb, n, d).unwrap(); }
        tb.into_inner().unwrap().finish().unwrap()
    }

    /// Tar có đường dẫn tuỳ ý (kể cả `..`) — Builder từ chối nên ghi thẳng vào header.
    fn raw_tar(name: &str, data: &[u8]) -> Vec<u8> {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o600);
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_cksum();
        let mut out = Vec::new();
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(data);
        out.resize(out.len() + (512 - data.len() % 512) % 512, 0);
        out.extend_from_slice(&[0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &out).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn archive_round_trips() {
        let b = make_tar(&[("accounts.json", b"{}"), ("profiles/abc/Default/Preferences", b"prefs"), ("./manifest.json", b"m")]);
        let m = read_backup(&b).unwrap();
        assert_eq!(m["accounts.json"], b"{}");
        assert_eq!(m["profiles/abc/Default/Preferences"], b"prefs");
        assert!(m.contains_key("manifest.json"), "tiền tố ./ phải được gỡ");
    }

    #[test]
    fn path_traversal_and_absolute_paths_are_rejected() {
        for bad in ["../evil", "a/../../evil", "/etc/passwd"] {
            let err = read_backup(&raw_tar(bad, b"x")).err().unwrap_or_else(|| panic!("{bad} phải bị từ chối"));
            assert!(format!("{err:#}").contains("không an toàn"), "{bad}: {err:#}");
        }
        assert!(read_backup(b"not a gzip").is_err());
    }

    #[test]
    fn seeds_cannot_escape_the_profiles_dir() {
        assert!(valid_seed("26d8b96d-eb01-4b58-8e3d-b6c09fbefffa"));
        assert!(valid_seed("shumei-a"));
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "../x", &"x".repeat(81)] { assert!(!valid_seed(bad), "{bad:?}"); }
    }

    #[test]
    fn only_identity_files_of_a_profile_are_taken() {
        let root = std::env::temp_dir().join(format!("wg-prof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |p: &str, d: &str| { let f = root.join(p); std::fs::create_dir_all(f.parent().unwrap()).unwrap(); std::fs::write(f, d).unwrap(); };
        mk("Local State", "ls");
        mk("Default/Preferences", "p");
        mk("Default/Network/Cookies", "c");
        mk("Default/Local Storage/leveldb/000003.log", "l");
        mk("Default/Local Storage/leveldb/LOCK", "");
        mk("Default/IndexedDB/https_chat.deepseek.com_0.indexeddb.leveldb/x", "i");
        mk("Default/Cache/Cache_Data/f_000001", "cache");
        mk("optimization_guide_model_store/big.bin", "model");
        mk("Default/History", "h");
        let mut got: Vec<String> = profile_files(&root).into_iter().map(|(r, _)| r).collect();
        got.sort();
        assert_eq!(got, [
            "Default/IndexedDB/https_chat.deepseek.com_0.indexeddb.leveldb/x", "Default/Local Storage/leveldb/000003.log",
            "Default/Network/Cookies", "Default/Preferences", "Local State",
        ]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_helpers_keep_local_values_unless_overwrite() {
        // cài đặt: khoá đã đổi ở máy này giữ nguyên; khoá hợp lệ mới thì áp dụng; khoá lạ / sai kiểu bị bỏ
        let _ = crate::settings::set("DS_FX_VND", Some("27000"));
        let incoming: HashMap<String, String> = [("DS_FX_VND", "30000"), ("DS_FX_CNY", "710"), ("DS_NOPE", "1"), ("DS_TZ_OFFSET", "abc")]
            .into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let (ok, skip) = crate::settings::import_overrides(&incoming, false, true);
        assert_eq!((ok, skip), (1, 3), "chỉ DS_FX_CNY là mới và hợp lệ");
        assert_eq!(crate::settings::get("DS_FX_VND"), "27000", "dry run không được ghi");
        let (ok, _) = crate::settings::import_overrides(&incoming, false, false);
        assert_eq!(ok, 1);
        assert_eq!(crate::settings::get("DS_FX_VND"), "27000");
        assert_eq!(crate::settings::get("DS_FX_CNY"), "710");
        let (ok, _) = crate::settings::import_overrides(&incoming, true, false);
        assert_eq!(ok, 2, "overwrite áp dụng cả khoá đã có");
        assert_eq!(crate::settings::get("DS_FX_VND"), "30000");
        let _ = crate::settings::set("DS_FX_VND", None);
        let _ = crate::settings::set("DS_FX_CNY", None);

        // lịch sử dùng: giờ đã có giữ nguyên
        let far = u64::MAX / 4;
        let js = format!("{{\"{far}\":{{\"req\":2,\"miss\":3,\"hit\":4,\"out\":5}}}}");
        assert_eq!(crate::usage::import_json(&js, false, true), 1);
        assert_eq!(crate::usage::import_json(&js, false, false), 1);
        assert_eq!(crate::usage::import_json(&js, false, false), 0, "nhập lần hai không thêm gì (idempotent)");
        assert_eq!(crate::usage::import_json(&js, true, false), 1);
        assert_eq!(crate::usage::import_json("not json", false, false), 0);

        // ghim proxy: vân tay đã có ghim giữ nguyên
        let m: HashMap<String, String> = [("seed-backup-test".to_string(), "http://1.2.3.4:8080".to_string())].into();
        assert_eq!(crate::proxypool::import_bindings(&m, false, true), 1);
        assert_eq!(crate::proxypool::import_bindings(&m, false, false), 1);
        assert_eq!(crate::proxypool::import_bindings(&m, false, false), 0);
        crate::proxypool::unbind("seed-backup-test");
    }

    #[test]
    fn proxy_list_in_a_backup_must_be_valid_as_a_whole() {
        assert!(crate::proxypool::import_proxies_file("http://1.2.3.4:8080\nftp://bad:1", true, true).is_err());
        assert!(matches!(crate::proxypool::import_proxies_file("# ghi chú\nsocks5h://u:p@1.2.3.4:1080", true, true), Ok(true)));
    }

    #[test]
    fn query_flags_accept_1_and_true() {
        assert!(truthy(&Some("1".into())) && truthy(&Some("true".into())) && truthy(&Some(" yes ".into())));
        assert!(!truthy(&Some("0".into())) && !truthy(&Some("false".into())) && !truthy(&None) && !truthy(&Some("".into())));
    }
}
