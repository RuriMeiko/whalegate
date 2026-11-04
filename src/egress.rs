//! Cổng ra qua Cloudflare Worker (worker/egress.js): DeepSeek thấy IP của Cloudflare thay vì IP nhà.
//!
//! Cơ chế: whalegate chạy một proxy CONNECT cục bộ (127.0.0.1:cổng ngẫu nhiên). Mỗi CONNECT host:port được nối bằng WebSocket
//! tới `/__tunnel` của Worker, Worker mở socket TCP tới đích và chuyển bytes hai chiều. TLS đi nguyên vẹn đầu-cuối giữa
//! Chrome/whalegate và DeepSeek nên dấu vân tay TLS/HTTP2 vẫn là của Chrome giả lập — Worker chỉ thấy bytes mã hoá.
//! Cả client HTTP của whalegate lẫn mọi cửa sổ Chrome riêng (đăng nhập, đăng ký, mở Chrome, làm mới thiết bị) dùng proxy này.
use anyhow::{bail, Context, Result};
use futures::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

/// Mỗi Worker có một proxy cục bộ riêng (cổng riêng) để Chrome — vốn không gắn được thông tin đăng nhập vào proxy — vẫn chọn được Worker.
static PORTS: OnceLock<Vec<u16>> = OnceLock::new();
static ACTIVE: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static DIRECT: AtomicU64 = AtomicU64::new(0);
static LAST_ERR: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub const MAX_WORKERS: usize = 8;

/// Một Worker cổng ra. `tag` là id account Cloudflare đã deploy nó (để gỡ đúng Worker), có thể trống.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worker { pub url: String, pub key: String, pub tag: String }

/// Mỗi dòng: `địa-chỉ khoá [# tag]`. Bỏ dòng sai, trùng địa chỉ, và quá `MAX_WORKERS`.
pub(crate) fn parse_workers(text: &str) -> Vec<Worker> {
    let mut out: Vec<Worker> = Vec::new();
    for line in text.lines() {
        let (body, tag) = match line.split_once('#') { Some((b, t)) => (b, t.trim()), None => (line, "") };
        let mut it = body.split_whitespace();
        let (Some(u), Some(k)) = (it.next(), it.next()) else { continue };
        let u = u.trim_end_matches('/');
        if !(u.starts_with("https://") || u.starts_with("http://")) || out.iter().any(|w| w.url == u) { continue; }
        out.push(Worker { url: u.to_string(), key: k.to_string(), tag: tag.to_string() });
        if out.len() >= MAX_WORKERS { break; }
    }
    out
}

pub(crate) fn format_workers(ws: &[Worker]) -> String {
    ws.iter().map(|w| if w.tag.is_empty() { format!("{} {}", w.url, w.key) } else { format!("{} {} # {}", w.url, w.key, w.tag) }).collect::<Vec<_>>().join("\n")
}

/// Mọi Worker đã cấu hình, kể cả khi công tắc tổng đang tắt. Cấu hình cũ (một Worker ở DS_EGRESS_URL/KEY) vẫn được đọc.
pub fn all_workers() -> Vec<Worker> {
    let mut v = parse_workers(&crate::settings::get("DS_EGRESS_WORKERS"));
    let (u, k) = (crate::settings::get("DS_EGRESS_URL"), crate::settings::get("DS_EGRESS_KEY"));
    if !u.trim().is_empty() && !k.trim().is_empty() {
        let old = parse_workers(&format!("{} {}", u.trim(), k.trim()));
        if let Some(w) = old.into_iter().next() { if !v.iter().any(|x| x.url == w.url) { v.insert(0, w); v.truncate(MAX_WORKERS); } }
    }
    v
}

/// Các Worker đang có hiệu lực (công tắc tổng bật).
pub fn workers() -> Vec<Worker> {
    if crate::settings::boolean("DS_EGRESS_ON") { all_workers() } else { Vec::new() }
}

/// Worker của một acc, ghim cố định theo seed vân tay (FNV-1a) để IP không nhảy giữa các lần chạy.
pub(crate) fn worker_index(seed: &str, n: usize) -> usize {
    if n == 0 { return 0; }
    let mut h: u64 = 0xcbf29ce484222325;
    for b in seed.bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
    (h % n as u64) as usize
}

fn port_url(idx: usize) -> Option<String> { PORTS.get().and_then(|p| p.get(idx)).map(|p| format!("http://127.0.0.1:{p}")) }

/// Proxy cục bộ dẫn tới Worker số `idx` (dùng khi đo IP từng Worker).
pub fn proxy_url_idx(idx: usize) -> Option<String> {
    if idx >= workers().len() { return None; }
    port_url(idx)
}

/// `http://127.0.0.1:cổng` của proxy cục bộ dành cho acc có vân tay `seed`, khi có Worker đang bật.
pub fn proxy_url_for(seed: &str) -> Option<String> {
    let n = workers().len();
    if n == 0 { return None; }
    port_url(worker_index(seed, n))
}

/// Cờ Chrome: đi qua proxy cục bộ và chặn WebRTC lộ IP thật qua UDP.
pub fn chrome_args(seed: &str) -> Vec<String> {
    match proxy_url_for(seed) {
        Some(u) => vec![format!("--proxy-server={u}"), "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".into()],
        None => Vec::new(),
    }
}

/// Chuyển cấu hình một-Worker cũ (DS_EGRESS_URL/KEY) sang danh sách mới rồi xoá cấu hình cũ.
fn migrate_legacy() {
    let (u, k) = (crate::settings::get("DS_EGRESS_URL"), crate::settings::get("DS_EGRESS_KEY"));
    if u.trim().is_empty() && k.trim().is_empty() { return; }
    let all = all_workers();
    if crate::settings::set("DS_EGRESS_WORKERS", Some(&format_workers(&all))).is_ok() {
        crate::settings::forget("DS_EGRESS_URL");
        crate::settings::forget("DS_EGRESS_KEY");
    }
}

/// Mở các cổng proxy cục bộ (luôn mở, rẻ); dùng Worker nào do `workers()` quyết định mỗi lần có kết nối.
pub async fn start() {
    if PORTS.get().is_some() { return; }
    migrate_legacy();
    let mut listeners = Vec::new();
    for _ in 0..MAX_WORKERS {
        match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(l) => listeners.push(l),
            Err(e) => { crate::elog!("[egress] không mở được proxy cục bộ: {e}"); return; }
        }
    }
    let ports: Vec<u16> = listeners.iter().filter_map(|l| l.local_addr().ok().map(|a| a.port())).collect();
    if ports.len() != listeners.len() { return; }
    let _ = PORTS.set(ports);
    let tasks = listeners.into_iter().enumerate().map(|(idx, l)| async move {
        loop {
            let Ok((s, _)) = l.accept().await else { continue };
            tokio::spawn(async move {
                ACTIVE.fetch_add(1, Ordering::Relaxed);
                let r = handle(s, idx).await;
                ACTIVE.fetch_sub(1, Ordering::Relaxed);
                if let Err(e) = r {
                    FAILED.fetch_add(1, Ordering::Relaxed);
                    let msg = format!("{e:#}");
                    crate::elog!("[egress] {msg}");
                    *LAST_ERR.lock().unwrap_or_else(|p| p.into_inner()) = msg;
                }
            });
        }
    });
    futures::future::join_all(tasks).await;
}

fn parse_connect(head: &str) -> Result<(String, u16)> {
    let first = head.lines().next().unwrap_or("");
    let mut it = first.split_whitespace();
    let (m, target) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
    if !m.eq_ignore_ascii_case("CONNECT") { bail!("chỉ hỗ trợ CONNECT (nhận: {m})"); }
    let (h, p) = target.rsplit_once(':').context("CONNECT thiếu cổng")?;
    let host = h.trim_start_matches('[').trim_end_matches(']').to_string();
    let port: u16 = p.parse().context("cổng không hợp lệ")?;
    if host.is_empty() { bail!("CONNECT thiếu host"); }
    Ok((host, port))
}

/// IP thuộc dải của Cloudflare (https://www.cloudflare.com/ips/). Worker không được mở socket TCP tới các IP này
/// (hCaptcha, nhiều CDN/website nằm sau Cloudflare) nên đích như vậy phải đi thẳng, không qua đường hầm.
pub(crate) fn is_cloudflare_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    // https://www.cloudflare.com/ips-v4
    const V4: &[(&str, u32)] = &[
        ("173.245.48.0", 20), ("103.21.244.0", 22), ("103.22.200.0", 22), ("103.31.4.0", 22), ("141.101.64.0", 18),
        ("108.162.192.0", 18), ("190.93.240.0", 20), ("188.114.96.0", 20), ("197.234.240.0", 22), ("198.41.128.0", 17),
        ("162.158.0.0", 15), ("104.16.0.0", 13), ("104.24.0.0", 14), ("172.64.0.0", 13), ("131.0.72.0", 22),
    ];
    match ip {
        IpAddr::V4(v) => { let n = u32::from(v); V4.iter().any(|(net, bits)| { let m = !0u32 << (32 - bits); net.parse::<std::net::Ipv4Addr>().map(|a| n & m == u32::from(a) & m).unwrap_or(false) }) }
        IpAddr::V6(v) => {
            let s = v.segments();
            (s[0] == 0x2606 && s[1] == 0x4700) || (s[0] == 0x2803 && s[1] == 0xf800) || (s[0] == 0x2405 && (s[1] == 0xb500 || s[1] == 0x8100))
                || (s[0] == 0x2a06 && s[1] & 0xfff8 == 0x98c0) || (s[0] == 0x2c0f && s[1] == 0xf248)
        }
    }
}

fn ws_url(base: &str, host: &str, port: u16) -> String {
    let b = if let Some(r) = base.strip_prefix("https://") { format!("wss://{r}") } else { format!("ws://{}", base.trim_start_matches("http://")) };
    format!("{b}/__tunnel?host={host}&port={port}")
}

async fn handle(mut tcp: tokio::net::TcpStream, idx: usize) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 2048];
    let head_end = loop {
        let n = tcp.read(&mut tmp).await?;
        if n == 0 { return Ok(()); }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break i + 4; }
        if buf.len() > 16 * 1024 { bail!("header CONNECT quá dài"); }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let (host, port) = match parse_connect(&head) {
        Ok(x) => x,
        Err(_) => { let _ = tcp.write_all(b"HTTP/1.1 501 Not Implemented\r\n\r\n").await; return Ok(()); } // http:// thường (kiểm tra mạng của Chrome…): không hỗ trợ, không phải lỗi
    };
    let ws_list = workers();
    if ws_list.is_empty() {
        let _ = tcp.write_all(b"HTTP/1.1 503 Service Unavailable\r\n\r\n").await;
        bail!("cổng ra đang tắt");
    }
    let Worker { url: base, key, .. } = ws_list[idx % ws_list.len()].clone();
    // đích nằm trên IP Cloudflare (vd hCaptcha): Worker không nối tới được → đi thẳng từ máy này
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port)).await.map(|i| i.collect()).unwrap_or_default();
    if addrs.iter().any(|a| is_cloudflare_ip(a.ip())) {
        let mut up = match tokio::time::timeout(std::time::Duration::from_secs(15), tokio::net::TcpStream::connect(&addrs[..])).await {
            Ok(Ok(s)) => s,
            _ => { let _ = tcp.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await; bail!("không nối thẳng được tới {host}:{port}"); }
        };
        DIRECT.fetch_add(1, Ordering::Relaxed);
        tcp.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;
        if buf.len() > head_end { up.write_all(&buf[head_end..]).await?; }
        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut up).await;
        return Ok(());
    }
    let mut req = ws_url(&base, &host, port).into_client_request()?;
    req.headers_mut().insert("x-wg-key", key.parse()?);
    let ws = match tokio::time::timeout(std::time::Duration::from_secs(15), tokio_tungstenite::connect_async(req)).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => { let _ = tcp.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await; bail!("Worker từ chối đường hầm tới {host}:{port}: {e}"); }
        Err(_) => { let _ = tcp.write_all(b"HTTP/1.1 504 Gateway Timeout\r\n\r\n").await; bail!("Worker không trả lời khi mở đường hầm tới {host}:{port}"); }
    };
    TOTAL.fetch_add(1, Ordering::Relaxed);
    bump_usage(&base);
    tcp.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;

    let (mut wtx, mut wrx) = ws.split();
    // byte client gửi sớm sau header (hiếm) được chuyển luôn
    if buf.len() > head_end { wtx.send(Message::Binary(buf[head_end..].to_vec().into())).await?; }
    let (mut r, mut w) = tcp.into_split();
    let up = async {
        let mut b = vec![0u8; 16 * 1024];
        loop {
            let n = r.read(&mut b).await?;
            if n == 0 { break; }
            wtx.send(Message::Binary(b[..n].to_vec().into())).await?;
        }
        let _ = wtx.close().await;
        Ok::<(), anyhow::Error>(())
    };
    let down = async {
        while let Some(m) = wrx.next().await {
            match m? {
                Message::Binary(b) => w.write_all(&b).await?,
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = w.shutdown().await;
        Ok::<(), anyhow::Error>(())
    };
    // một chiều đóng thì chiều kia kết thúc luôn (đường hầm TCP không half-close được qua WebSocket)
    tokio::select! { r = up => r, r = down => r }
}

// ---------- số request đã gửi qua Worker (để so với hạn mức Cloudflare) ----------

fn usage_path() -> std::path::PathBuf { std::path::Path::new(&crate::server::data_dir()).join("egress-usage.json") }
fn day() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86400).unwrap_or(0) }

type Days = std::collections::BTreeMap<u64, u64>;
static DAYS: std::sync::Mutex<Option<std::collections::BTreeMap<String, Days>>> = std::sync::Mutex::new(None);
static LAST_SAVE: AtomicU64 = AtomicU64::new(0);

/// Số đường hầm mỗi ngày theo từng Worker (khoá = URL). File cũ (chỉ có ngày → số) được gán cho Worker đầu tiên.
fn load_days() -> std::collections::BTreeMap<String, Days> {
    let Ok(t) = std::fs::read_to_string(usage_path()) else { return Default::default() };
    if let Ok(m) = serde_json::from_str(&t) { return m; }
    let mut out = std::collections::BTreeMap::new();
    if let Ok(old) = serde_json::from_str::<Days>(&t) {
        out.insert(all_workers().first().map(|w| w.url.clone()).unwrap_or_default(), old);
    }
    out
}

fn with_days<R>(f: impl FnOnce(&mut std::collections::BTreeMap<String, Days>) -> R) -> R {
    let mut g = DAYS.lock().unwrap_or_else(|p| p.into_inner());
    let m = g.get_or_insert_with(load_days);
    f(m)
}

fn bump_usage(url: &str) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let snapshot = with_days(|m| {
        let d = day();
        let e = m.entry(url.to_string()).or_default();
        *e.entry(d).or_insert(0) += 1;
        for v in m.values_mut() { v.retain(|k, _| d.saturating_sub(*k) < 60); }
        if now.saturating_sub(LAST_SAVE.load(Ordering::Relaxed)) >= 10 { LAST_SAVE.store(now, Ordering::Relaxed); serde_json::to_string(m).ok() } else { None }
    });
    if let Some(s) = snapshot { let _ = std::fs::write(usage_path(), s); }
}

/// Đường hầm của một Worker (url) trong `n` ngày UTC gần nhất; `url = None` = cộng mọi Worker.
/// (Cloudflare đặt lại hạn mức Free lúc 00:00 UTC.)
pub fn used_days(url: Option<&str>, n: u64) -> u64 {
    let d = day();
    with_days(|m| m.iter().filter(|(u, _)| url.map_or(true, |x| x == u.as_str()))
        .flat_map(|(_, v)| v.iter()).filter(|(k, _)| d.saturating_sub(**k) < n).map(|(_, v)| *v).sum())
}

pub fn stats() -> serde_json::Value {
    serde_json::json!({
        "ports": PORTS.get(),
        "active": ACTIVE.load(Ordering::Relaxed),
        "total": TOTAL.load(Ordering::Relaxed),
        "failed": FAILED.load(Ordering::Relaxed),
        "direct": DIRECT.load(Ordering::Relaxed),
        "last_error": LAST_ERR.lock().unwrap_or_else(|p| p.into_inner()).clone(),
        "today": used_days(None, 1), "d7": used_days(None, 7), "d30": used_days(None, 30),
        "workers": workers().iter().map(|w| serde_json::json!({"url": w.url, "today": used_days(Some(&w.url), 1), "d7": used_days(Some(&w.url), 7), "d30": used_days(Some(&w.url), 30)})).collect::<Vec<_>>(),
        "daily_limit": crate::settings::uint("DS_EGRESS_DAILY_LIMIT"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_connect_lines() {
        assert_eq!(parse_connect("CONNECT chat.deepseek.com:443 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap(), ("chat.deepseek.com".into(), 443));
        assert_eq!(parse_connect("connect [2001:db8::1]:8443 HTTP/1.1\r\n\r\n").unwrap(), ("2001:db8::1".into(), 8443));
        for bad in ["GET http://x/ HTTP/1.1\r\n\r\n", "CONNECT nohost HTTP/1.1\r\n\r\n", "CONNECT :443 HTTP/1.1\r\n\r\n", "CONNECT a:99999 HTTP/1.1\r\n\r\n"] {
            assert!(parse_connect(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cloudflare_ranges_are_detected() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        for c in ["104.19.230.21", "104.26.5.5", "172.67.1.1", "162.159.140.1", "108.162.200.1", "141.101.70.1", "188.114.97.3", "2606:4700::1", "2a06:98c0:3600::103"] {
            assert!(is_cloudflare_ip(ip(c)), "{c}");
        }
        for o in ["203.0.113.7", "8.8.8.8", "13.225.1.1", "52.1.1.1", "104.15.0.1", "172.63.255.255", "2001:db8::1", "2600:9000::1"] {
            assert!(!is_cloudflare_ip(ip(o)), "{o}");
        }
    }

    #[test]
    fn websocket_url_follows_the_worker_scheme() {
        assert_eq!(ws_url("https://w.example", "a.com", 443), "wss://w.example/__tunnel?host=a.com&port=443");
        assert_eq!(ws_url("http://127.0.0.1:8787", "a.com", 80), "ws://127.0.0.1:8787/__tunnel?host=a.com&port=80");
    }

    #[test]
    fn off_without_url_and_key() {
        assert!(workers().is_empty());
        assert!(proxy_url_for("seed").is_none());
        assert!(chrome_args("seed").is_empty());
    }

    #[test]
    fn parses_worker_lines() {
        let t = "https://a.example.workers.dev/ k1 # acc1\n\nbad line\nftp://x k\nhttps://a.example.workers.dev k2\nhttps://b.example.workers.dev k3";
        let w = parse_workers(t);
        assert_eq!(w.len(), 2, "dòng sai và trùng địa chỉ bị bỏ");
        assert_eq!((w[0].url.as_str(), w[0].key.as_str(), w[0].tag.as_str()), ("https://a.example.workers.dev", "k1", "acc1"));
        assert_eq!(w[1].tag, "");
        assert_eq!(parse_workers(&format_workers(&w)), w, "ghi ra rồi đọc lại phải giữ nguyên");
        let many: String = (0..20).map(|i| format!("https://w{i}.example.workers.dev k\n")).collect();
        assert_eq!(parse_workers(&many).len(), MAX_WORKERS);
    }

    #[test]
    fn accounts_are_pinned_to_a_worker() {
        for n in 1..=MAX_WORKERS {
            let i = worker_index("2216de35-a20a-4238-a004-94d67e2f9b27", n);
            assert!(i < n);
            assert_eq!(i, worker_index("2216de35-a20a-4238-a004-94d67e2f9b27", n), "cùng seed phải ra cùng Worker");
        }
        let spread: std::collections::HashSet<usize> = (0..64).map(|i| worker_index(&format!("seed-{i}"), 4)).collect();
        assert_eq!(spread.len(), 4, "các acc phải rải đều ra cả 4 Worker");
    }
}

#[cfg(test)]
mod tls_provider_tests {
    /// rustls phải có đúng một crypto provider dựng sẵn; thiếu thì `connect_async` tới wss:// panic trong task đường hầm.
    #[test]
    fn rustls_has_a_default_crypto_provider() {
        let cfg = std::panic::catch_unwind(|| {
            rustls::ClientConfig::builder().with_root_certificates(rustls::RootCertStore::empty()).with_no_client_auth()
        });
        assert!(cfg.is_ok(), "rustls không có crypto provider — thêm feature \"ring\" cho rustls trong Cargo.toml");
    }

    /// Đi qua đúng đường code thật: mở WebSocket TLS (wss://). Bị bỏ qua mặc định vì cần mạng.
    #[tokio::test]
    #[ignore]
    async fn wss_connect_does_not_panic() {
        let r = tokio_tungstenite::connect_async("wss://echo.websocket.org").await;
        assert!(r.is_ok(), "{:?}", r.err());
    }
}
