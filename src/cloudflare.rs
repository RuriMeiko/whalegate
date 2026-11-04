//! Cài Worker cổng ra (worker/egress.js) lên Cloudflare ngay từ dashboard: tự tìm hoặc tải Node.js, đăng nhập wrangler bằng
//! trình duyệt (người dùng chỉ bấm Allow), deploy Worker, đặt khoá, bật cổng ra và đo IP. Mọi bước chạy nền, dashboard hỏi
//! trạng thái qua `status()`.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const WRANGLER: &str = "wrangler@4.149.0";
const WORKER_NAME: &str = "ds-egress";
const EGRESS_JS: &str = include_str!("../worker/egress.js");
const NODE_VER: &str = "v22.12.0";

#[derive(Default, Clone)]
struct State {
    op: String,
    log: Vec<String>,
    login_url: String,
    error: String,
    logged_in: Option<bool>,
    email: String,
    accounts: Vec<(String, String)>,
    /// kết quả đo từng Worker, khoá = URL
    probes: std::collections::BTreeMap<String, Probe>,
}

#[derive(Default, Clone)]
struct Probe { ip: String, colo: String, ms: u64, at: u64 }

static ST: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = ST.lock().unwrap_or_else(|p| p.into_inner());
    f(g.get_or_insert_with(State::default))
}

fn say(line: &str) {
    let l = line.trim();
    if l.is_empty() { return; }
    crate::elog!("[cloudflare] {l}");
    with(|s| { s.log.push(l.chars().take(300).collect()); let n = s.log.len(); if n > 14 { s.log.drain(..n - 14); } });
}

fn now() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }
fn tools_dir() -> PathBuf { Path::new(&crate::server::data_dir()).join("tools") }
fn work_dir() -> PathBuf { Path::new(&crate::server::data_dir()).join("cf-worker") }

// ---------- tìm / cài Node.js ----------

fn exe(name: &str) -> String { if cfg!(windows) { format!("{name}.cmd") } else { name.to_string() } }

/// Thư mục bin của Node đã whalegate tự tải (nếu có).
fn own_node_bin() -> Option<PathBuf> {
    let d = std::fs::read_dir(tools_dir()).ok()?;
    for e in d.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("node-v") {
            let p = if cfg!(windows) { e.path() } else { e.path().join("bin") };
            if p.join(exe("npx")).exists() { return Some(p); }
        }
    }
    None
}

fn system_npx() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(exe("npx"))).find(|p| p.exists())
}

/// (đường dẫn npx, thư mục cần thêm vào PATH để script npx tìm được `node`)
fn find_npx() -> Option<(PathBuf, Option<PathBuf>)> {
    if std::env::var("DS_CF_FORCE_NODE").map(|v| v == "1").unwrap_or(false) {
        return own_node_bin().map(|b| (b.join(exe("npx")), Some(b)));
    }
    if let Some(p) = system_npx() { return Some((p, None)); }
    own_node_bin().map(|b| (b.join(exe("npx")), Some(b)))
}

pub(crate) fn node_target() -> Option<(String, &'static str)> {
    let plat = match std::env::consts::OS { "linux" => "linux", "macos" => "darwin", "windows" => "win", _ => return None };
    let arch = match std::env::consts::ARCH { "x86_64" => "x64", "aarch64" => "arm64", _ => return None };
    Some((format!("node-{NODE_VER}-{plat}-{arch}"), if plat == "win" { "zip" } else { "tar.gz" }))
}

pub(crate) fn sha_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, f) = l.split_once(char::is_whitespace)?;
        (f.trim() == file && h.len() == 64).then(|| h.to_lowercase())
    })
}

async fn install_node() -> Result<()> {
    let (stem, ext) = node_target().context("hệ điều hành/kiến trúc này chưa hỗ trợ tự tải Node.js — cài Node.js tay rồi bấm lại")?;
    let file = format!("{stem}.{ext}");
    let base = format!("https://nodejs.org/dist/{NODE_VER}");
    say(&format!("Đang tải Node.js {NODE_VER} từ nodejs.org ({file})…"));
    let c = reqwest::Client::builder().timeout(std::time::Duration::from_secs(600)).build()?;
    let sums = c.get(format!("{base}/SHASUMS256.txt")).send().await?.error_for_status()?.text().await?;
    let want = sha_for(&sums, &file).with_context(|| format!("SHASUMS256.txt không có {file}"))?;
    let bytes = c.get(format!("{base}/{file}")).send().await?.error_for_status()?.bytes().await?;
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if got != want { bail!("checksum Node.js không khớp (nhận {got}, cần {want}) — không cài"); }
    say("Đã xác minh checksum, đang giải nén…");
    let dir = tools_dir();
    std::fs::create_dir_all(&dir)?;
    let (d2, bytes2, ext2) = (dir.clone(), bytes.to_vec(), ext);
    tokio::task::spawn_blocking(move || -> Result<()> {
        if ext2 == "zip" {
            zip::ZipArchive::new(std::io::Cursor::new(bytes2))?.extract(&d2)?;
        } else {
            tar::Archive::new(flate2::read::GzDecoder::new(std::io::Cursor::new(bytes2))).unpack(&d2)?;
        }
        Ok(())
    }).await??;
    if own_node_bin().is_none() { bail!("giải nén xong nhưng không thấy npx"); }
    say("Đã cài Node.js vào thư mục dữ liệu của whalegate.");
    Ok(())
}

// ---------- chạy wrangler ----------

fn path_with(extra: &Option<PathBuf>) -> std::ffi::OsString {
    let mut v: Vec<PathBuf> = extra.iter().cloned().collect();
    if let Some(p) = std::env::var_os("PATH") { v.extend(std::env::split_paths(&p)); }
    std::env::join_paths(v).unwrap_or_default()
}

/// Chạy `npx wrangler <args>`; mỗi dòng đầu ra vào nhật ký. Trả (thành công, toàn bộ đầu ra).
async fn wrangler(args: &[&str], cwd: Option<&Path>, stdin: Option<&str>, secs: u64, account: Option<&str>) -> Result<(bool, String)> {
    let (npx, extra) = find_npx().context("chưa có Node.js (npx)")?;
    let mut cmd = tokio::process::Command::new(npx);
    cmd.arg("--yes").arg(WRANGLER).args(args)
        .env("PATH", path_with(&extra))
        .env("WRANGLER_SEND_METRICS", "false")
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .stdin(if stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .kill_on_drop(true);
    if let Some(d) = cwd { cmd.current_dir(d); }
    if let Some(a) = account { cmd.env("CLOUDFLARE_ACCOUNT_ID", a); }
    let mut child = cmd.spawn().context("không chạy được npx")?;
    if let (Some(text), Some(mut si)) = (stdin, child.stdin.take()) { si.write_all(text.as_bytes()).await?; drop(si); }
    let (out, err) = (child.stdout.take().context("stdout")?, child.stderr.take().context("stderr")?);
    let all = std::sync::Arc::new(Mutex::new(String::new()));
    let quiet = args.first() == Some(&"auth"); // `auth token` in ra token: không ghi vào nhật ký
    let pump = |r: Box<dyn tokio::io::AsyncRead + Unpin + Send>, all: std::sync::Arc<Mutex<String>>| async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Some(u) = parse_login_url(&l) { with(|s| s.login_url = u); }
            if !quiet && !l.contains("telemetry") && !l.contains('─') { say(&l); }
            all.lock().unwrap_or_else(|p| p.into_inner()).push_str(&format!("{l}\n"));
        }
    };
    let (a, b) = (pump(Box::new(out), all.clone()), pump(Box::new(err), all.clone()));
    let run = async { tokio::join!(a, b); child.wait().await };
    let status = match tokio::time::timeout(std::time::Duration::from_secs(secs), run).await {
        Ok(s) => s?,
        Err(_) => bail!("quá {secs}s chưa xong"),
    };
    let text = all.lock().unwrap_or_else(|p| p.into_inner()).clone();
    Ok((status.success(), text))
}

pub(crate) fn parse_login_url(line: &str) -> Option<String> {
    let i = line.find("https://dash.cloudflare.com/oauth2/auth")?;
    Some(line[i..].split_whitespace().next()?.to_string())
}

/// (đã đăng nhập, email, [(tên account, id)])
pub(crate) fn parse_whoami(text: &str) -> (bool, String, Vec<(String, String)>) {
    let logged = text.contains("You are logged in");
    let email = text.split("the email ").nth(1).and_then(|r| r.split_whitespace().next()).map(|e| e.trim_end_matches('.').to_string()).unwrap_or_default();
    let mut accs = Vec::new();
    for l in text.lines() {
        let cols: Vec<&str> = l.split('│').map(str::trim).filter(|c| !c.is_empty()).collect();
        if cols.len() == 2 && cols[1].len() == 32 && cols[1].chars().all(|c| c.is_ascii_hexdigit()) {
            accs.push((cols[0].to_string(), cols[1].to_string()));
        }
    }
    (logged, email, accs)
}

pub(crate) fn parse_workers_url(text: &str) -> Option<String> {
    text.split_whitespace().find(|w| w.starts_with("https://") && w.contains(".workers.dev")).map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_string())
}

async fn refresh_login() -> Result<bool> {
    let (_, text) = wrangler(&["whoami"], None, None, 90, None).await?;
    let (logged, email, accs) = parse_whoami(&text);
    with(|s| { s.logged_in = Some(logged); s.email = email; s.accounts = accs; });
    Ok(logged)
}

/// Đo từng Worker đang bật; trả số Worker trả lời được.
async fn probe_ip() -> usize {
    let mut good = 0;
    for (i, w) in crate::egress::workers().into_iter().enumerate() {
        let t = std::time::Instant::now();
        let c = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build() { Ok(c) => c, Err(_) => continue };
        // trạm Cloudflare phục vụ Worker (gọi HTTP thường)
        let mut colo = String::new();
        let mut worker_ok = false;
        if let Ok(resp) = c.get(format!("{}/__ip", w.url)).header("x-wg-key", &w.key).send().await {
            if let Ok(v) = resp.json::<Value>().await { colo = v["colo"].as_str().unwrap_or("").to_string(); worker_ok = v.get("egress").is_some(); }
        }
        // IP mà DeepSeek/Chrome thật sự thấy: đo qua chính đường hầm TCP (khác IP của lần gọi HTTP thường ở trên).
        // Dùng dịch vụ không nằm sau Cloudflare vì Worker không được mở socket tới IP Cloudflare.
        let mut ip = String::new();
        if let Some(p) = crate::egress::proxy_url_idx(i) {
            if let Ok(px) = reqwest::Proxy::all(&p) {
                if let Ok(pc) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(25)).proxy(px).build() {
                    if let Ok(r) = pc.get("https://checkip.amazonaws.com").send().await { ip = r.text().await.unwrap_or_default().trim().to_string(); }
                }
            }
        }
        if ip.is_empty() && !worker_ok { continue; }
        good += 1;
        with(|s| { s.probes.insert(w.url.clone(), Probe { ip, colo, ms: t.elapsed().as_millis() as u64, at: now() }); });
    }
    good
}

fn begin(op: &str) -> bool {
    with(|s| {
        if !s.op.is_empty() { return false; }
        s.op = op.into(); s.error.clear(); s.login_url.clear(); s.log.clear();
        true
    })
}

fn finish(res: Result<()>) {
    with(|s| {
        s.op.clear();
        match res { Ok(()) => s.error.clear(), Err(e) => s.error = format!("{e:#}") }
    });
    if let Some(e) = with(|s| (!s.error.is_empty()).then(|| s.error.clone())) { crate::elog!("[cloudflare] lỗi: {e}"); }
}

pub(crate) fn needs_subdomain(text: &str) -> bool { text.contains("register a workers.dev subdomain") }

/// Account mới chưa có subdomain workers.dev nên `wrangler deploy` đòi hỏi tương tác. Đăng ký qua API bằng token OAuth của wrangler.
async fn register_subdomain(acc_id: &str) -> Result<()> {
    let (ok, out) = wrangler(&["auth", "token"], None, None, 60, None).await?;
    let raw = out.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("").to_string();
    let token = if raw.starts_with('{') { serde_json::from_str::<Value>(&raw).ok().and_then(|v| v["token"].as_str().map(String::from)).unwrap_or_default() } else { raw };
    if !ok || token.is_empty() || token.contains(' ') { bail!("không lấy được token wrangler để đăng ký workers.dev"); }
    let http = reqwest::Client::new();
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{acc_id}/workers/subdomain");
    for _ in 0..4 {
        let name = format!("wg-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let r = http.put(&url).bearer_auth(&token).json(&json!({"subdomain": name})).send().await?;
        let ok = r.status().is_success();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if ok && body["success"].as_bool() == Some(true) { say(&format!("Đã đăng ký {name}.workers.dev")); return Ok(()); }
        say(&format!("Đăng ký {name} chưa được: {}", body["errors"][0]["message"].as_str().unwrap_or("lỗi không rõ")));
    }
    bail!("không đăng ký được workers.dev — mở dash.cloudflare.com → Workers & Pages để tạo thủ công rồi bấm lại")
}

async fn setup_flow(other_account: bool) -> Result<()> {
    if find_npx().is_none() {
        install_node().await?;
    }
    if other_account {
        say("Đăng xuất account Cloudflare hiện tại để đăng nhập account khác…");
        let _ = wrangler(&["logout"], None, None, 60, None).await?;
        with(|s| { s.logged_in = Some(false); s.email.clear(); s.accounts.clear(); });
    }
    say("Kiểm tra đăng nhập Cloudflare…");
    if !refresh_login().await? {
        say("Chưa đăng nhập — đã mở trình duyệt, bạn bấm Allow để cấp quyền cho wrangler.");
        let (ok, text) = wrangler(&["login"], None, None, 600, None).await?;
        if !ok && !text.contains("Successfully logged in") { bail!("đăng nhập Cloudflare không hoàn tất"); }
        with(|s| s.login_url.clear());
        if !refresh_login().await? { bail!("đăng nhập xong nhưng wrangler vẫn báo chưa đăng nhập"); }
    }
    let (acc_name, acc_id) = with(|s| s.accounts.first().cloned()).context("tài khoản Cloudflare không có account nào")?;
    say(&format!("Dùng account Cloudflare: {acc_name}"));

    let dir = work_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("egress.js"), EGRESS_JS)?;
    std::fs::write(dir.join("wrangler.toml"), format!("name = \"{WORKER_NAME}\"\nmain = \"egress.js\"\ncompatibility_date = \"2024-09-01\"\n\n[observability]\nenabled = false\n"))?;
    say("Đang deploy Worker…");
    let (mut ok, mut text) = wrangler(&["deploy", "--config", "wrangler.toml"], Some(&dir), None, 300, Some(&acc_id)).await?;
    if !ok && needs_subdomain(&text) {
        say("Account chưa có địa chỉ workers.dev — đang tự đăng ký…");
        register_subdomain(&acc_id).await?;
        say("Deploy lại Worker…");
        (ok, text) = wrangler(&["deploy", "--config", "wrangler.toml"], Some(&dir), None, 300, Some(&acc_id)).await?;
    }
    if !ok { bail!("deploy Worker thất bại — xem nhật ký"); }
    let url = parse_workers_url(&text).context("deploy xong nhưng không đọc được địa chỉ workers.dev")?;

    // mỗi account Cloudflare một Worker, mỗi Worker một khoá riêng; deploy lại thì giữ khoá cũ
    let mut list = crate::egress::all_workers();
    let key = list.iter().find(|w| w.url == url || w.tag == acc_id).map(|w| w.key.clone()).unwrap_or_else(new_key);
    say("Đặt khoá bí mật cho Worker…");
    let (ok, _) = wrangler(&["secret", "put", "EGRESS_KEY", "--config", "wrangler.toml"], Some(&dir), Some(&key), 120, Some(&acc_id)).await?;
    if !ok { bail!("đặt khoá Worker thất bại"); }

    list.retain(|w| w.url != url && w.tag != acc_id);
    list.push(crate::egress::Worker { url: url.clone(), key, tag: acc_id.clone() });
    if list.len() > crate::egress::MAX_WORKERS { bail!("tối đa {} Worker — gỡ bớt trước khi thêm", crate::egress::MAX_WORKERS); }
    crate::settings::set("DS_EGRESS_WORKERS", Some(&crate::egress::format_workers(&list))).map_err(|e| anyhow::anyhow!(e))?;
    crate::settings::forget("DS_EGRESS_URL");
    crate::settings::forget("DS_EGRESS_KEY");
    crate::settings::set("DS_EGRESS_ON", Some("1")).map_err(|e| anyhow::anyhow!(e))?;
    say("Chờ Worker sẵn sàng và đo IP đầu ra…");
    for _ in 0..12 {
        probe_ip().await;
        if with(|s| s.probes.get(&url).map(|p| !p.ip.is_empty()).unwrap_or(false)) { break; }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    if !with(|s| s.probes.get(&url).map(|p| !p.ip.is_empty()).unwrap_or(false)) { bail!("Worker đã deploy nhưng chưa trả lời — thử bấm “Kiểm tra” sau vài giây"); }
    say(&format!("Xong. Giờ có {} Worker; mỗi acc được ghim vào một Worker.", list.len()));
    Ok(())
}

fn new_key() -> String { format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()) }

async fn remove_flow() -> Result<()> {
    let acc = with(|s| s.accounts.first().map(|a| a.1.clone()));
    let dir = work_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("wrangler.toml"), format!("name = \"{WORKER_NAME}\"\nmain = \"egress.js\"\ncompatibility_date = \"2024-09-01\"\n"))?;
    std::fs::write(dir.join("egress.js"), EGRESS_JS)?;
    say("Đang gỡ Worker của account đang đăng nhập khỏi Cloudflare…");
    let (ok, text) = wrangler(&["delete", "--config", "wrangler.toml", "--force"], Some(&dir), None, 120, acc.as_deref()).await?;
    if !ok && !text.contains("not found") && !text.contains("does not exist") { bail!("gỡ Worker thất bại — xem nhật ký"); }
    // bỏ đúng dòng của account này; Worker cũ chưa có tag mà chỉ còn một Worker duy nhất thì coi là của account này
    let mut list = crate::egress::all_workers();
    let before = list.len();
    let tag = acc.clone().unwrap_or_default();
    list.retain(|w| w.tag != tag || tag.is_empty());
    if list.len() == before && before == 1 && list[0].tag.is_empty() { list.clear(); }
    crate::settings::set("DS_EGRESS_WORKERS", Some(&crate::egress::format_workers(&list))).map_err(|e| anyhow::anyhow!(e))?;
    crate::settings::forget("DS_EGRESS_URL");
    crate::settings::forget("DS_EGRESS_KEY");
    with(|s| { let urls: Vec<String> = list.iter().map(|w| w.url.clone()).collect(); s.probes.retain(|k, _| urls.contains(k)); });
    say(&format!("Đã gỡ Worker. Còn {} Worker trong danh sách.", list.len()));
    Ok(())
}

/// `other_account`: đăng xuất wrangler rồi đăng nhập account Cloudflare khác để thêm Worker thứ hai.
pub fn start_setup(other_account: bool) -> Result<(), String> {
    if !begin("setup") { return Err("đang có một việc khác chạy".into()); }
    tokio::spawn(async move { let r = setup_flow(other_account).await; finish(r); });
    Ok(())
}

pub fn start_remove() -> Result<(), String> {
    if !begin("remove") { return Err("đang có một việc khác chạy".into()); }
    tokio::spawn(async { let r = remove_flow().await; finish(r); });
    Ok(())
}

pub fn start_refresh() -> Result<(), String> {
    if !begin("refresh") { return Err("đang có một việc khác chạy".into()); }
    tokio::spawn(async {
        let r = async { if find_npx().is_some() { let _ = refresh_login().await; } probe_ip().await; Ok(()) }.await;
        finish(r);
    });
    Ok(())
}

/// Trạng thái cho dashboard.
pub fn status() -> Value {
    let s = with(|s| s.clone());
    let usage = crate::egress::stats();
    let on = crate::settings::boolean("DS_EGRESS_ON");
    let workers: Vec<Value> = crate::egress::all_workers().iter().map(|w| {
        let p = s.probes.get(&w.url).cloned().unwrap_or_default();
        let u = usage["workers"].as_array().and_then(|a| a.iter().find(|x| x["url"] == w.url.as_str())).cloned().unwrap_or(Value::Null);
        json!({
            "url": w.url, "tag": w.tag, "ip": p.ip, "colo": p.colo, "probe_ms": p.ms, "probed_at": p.at,
            "today": u["today"], "d7": u["d7"], "d30": u["d30"],
        })
    }).collect();
    json!({
        "node": find_npx().is_some(),
        "node_auto": own_node_bin().is_some(),
        "node_target": node_target().map(|t| t.0),
        "op": s.op, "log": s.log, "login_url": s.login_url, "error": s.error,
        "logged_in": s.logged_in, "email": s.email,
        "accounts": s.accounts.iter().map(|a| json!({"name": a.0, "id": a.1})).collect::<Vec<_>>(),
        "name": WORKER_NAME, "on": on, "max_workers": crate::egress::MAX_WORKERS,
        "workers": workers,
        "usage": usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_whoami_output() {
        let t = "👋 You are logged in with an OAuth Token, associated with the email user@example.com.\n┌─┬─┐\n│ Account Name │ Account ID │\n│ Example Account │ 0123456789abcdef0123456789abcdef │\n";
        let (l, e, a) = parse_whoami(t);
        assert!(l);
        assert_eq!(e, "user@example.com");
        assert_eq!(a, vec![("Example Account".to_string(), "0123456789abcdef0123456789abcdef".to_string())]);
        assert!(!parse_whoami("You are not authenticated. Please run `wrangler login`.").0);
    }

    #[test]
    fn finds_login_and_worker_urls() {
        assert_eq!(parse_login_url("Opening a link in your default browser: https://dash.cloudflare.com/oauth2/auth?a=1&b=2").as_deref(), Some("https://dash.cloudflare.com/oauth2/auth?a=1&b=2"));
        assert_eq!(parse_login_url("nothing here"), None);
        assert_eq!(parse_workers_url("Deployed ds-egress triggers (1s)\n  https://ds-egress.example.workers.dev\nCurrent Version ID: x").as_deref(), Some("https://ds-egress.example.workers.dev"));
        assert_eq!(parse_workers_url("no url"), None);
    }

    #[test]
    fn picks_the_right_node_checksum_and_target() {
        let sums = "aaaa  other.tar.gz\n".to_string() + &"b".repeat(64) + "  node-v22.12.0-linux-x64.tar.gz\n";
        assert_eq!(sha_for(&sums, "node-v22.12.0-linux-x64.tar.gz"), Some("b".repeat(64)));
        assert_eq!(sha_for(&sums, "node-v22.12.0-win-x64.zip"), None);
        let (stem, ext) = node_target().unwrap();
        assert!(stem.starts_with("node-v22.12.0-") && (ext == "tar.gz" || ext == "zip"));
    }

    #[test]
    fn spots_missing_subdomain() {
        assert!(needs_subdomain("ERROR You need to register a workers.dev subdomain before publishing"));
        assert!(!needs_subdomain("Deployed ds-egress"));
    }

    #[test]
    fn generated_keys_are_long_and_unique() {
        assert_eq!(new_key().len(), 64);
        assert_ne!(new_key(), new_key());
    }
}
