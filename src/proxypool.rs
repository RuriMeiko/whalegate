// Proxy đầu ra cho từng acc: mỗi VÂN TAY đi ra DeepSeek qua một IP khác.
//
// Vì sao gắn theo vân tay chứ không theo acc: DS nhìn thấy (IP, vân tay) như một cặp danh tính. Hai acc
// khác token nhưng cùng "máy" (cùng vân tay) thì phải cùng một IP; hai acc khác máy thì phải khác IP.
// Ghim theo vân tay giữ đúng cả hai chiều đó, kể cả khi acc bị xoá rồi thêm lại.
//
// Cấu hình (ưu tiên override trên dashboard → biến môi trường → file):
//   DS_PROXY_LIST    danh sách proxy, mỗi dòng một cái: scheme://[user:pass@]host:port
//                    scheme: http | https | socks5 | socks5h
//   DS_PROXY_MODE    off | auto | strict      (mặc định "off")
//                    off    = không dùng proxy (như trước giờ)
//                    auto   = có proxy thì dùng; proxy chết thì đi thẳng, có log
//                    strict = acc đã ghim proxy mà proxy chết thì BÁO LỖI, không lộ IP thật
//   DS_PROXY_FILE    file proxy, mỗi dòng một cái (mặc định <data_dir>/proxies.txt)
//
// Ghim nằm trong <data_dir>/proxy-bindings.json: vân tay → proxy. Bền qua lần khởi động lại và qua
// việc xoá/thêm lại acc, nên IP của một "máy" không nhảy lung tung.
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Một proxy đã phân tích. Giữ nguyên dạng chuỗi gốc để log/dashboard hiển thị đúng cái người dùng nhập,
/// nhưng ẩn mật khẩu khi in ra.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyEntry {
    /// chuỗi gốc đã chuẩn hoá (dùng làm khoá ghim)
    pub raw: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub pass: Option<String>,
}

impl ProxyEntry {
    /// Phân tích một dòng cấu hình. Sai định dạng thì trả Err kèm lý do — KHÔNG im lặng bỏ qua,
    /// vì bỏ qua một proxy là âm thầm để acc đó lộ IP thật.
    pub fn parse(line: &str) -> Result<Self> {
        let raw = line.trim();
        if raw.is_empty() {
            anyhow::bail!("dòng rỗng");
        }
        let (scheme, rest) = raw.split_once("://").context("thiếu scheme:// (http, https, socks5, socks5h)")?;
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https" | "socks5" | "socks5h") {
            anyhow::bail!("scheme '{}' không hỗ trợ (http | https | socks5 | socks5h)", scheme);
        }
        // user:pass@host:port — tách ở '@' CUỐI CÙNG vì mật khẩu có thể chứa '@'
        let (auth, hostport) = match rest.rsplit_once('@') {
            Some((a, h)) => (Some(a), h),
            None => (None, rest),
        };
        let (user, pass) = match auth {
            Some(a) => {
                let (u, p) = a.split_once(':').context("phần đăng nhập phải là user:pass")?;
                if u.is_empty() {
                    anyhow::bail!("user rỗng");
                }
                (Some(percent_decode(u)), Some(percent_decode(p)))
            }
            None => (None, None),
        };
        // host:port — tách ở ':' CUỐI CÙNG để chịu được IPv6 dạng [::1]:8080
        let (host, port) = hostport.rsplit_once(':').context("thiếu :port")?;
        let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
        if host.is_empty() {
            anyhow::bail!("host rỗng");
        }
        let port: u16 = port.parse().with_context(|| format!("port '{}' không hợp lệ", port))?;
        if port == 0 {
            anyhow::bail!("port 0 không dùng được");
        }
        Ok(ProxyEntry { raw: raw.to_string(), scheme, host, port, user, pass })
    }

    /// Chuỗi để hiện cho người dùng: che mật khẩu.
    pub fn masked(&self) -> String {
        // host IPv6 phải bọc trong [] — parse đã bỏ ngoặc để lưu trữ sạch
        let h = self.host_display();
        match (&self.user, &self.pass) {
            (Some(u), Some(_)) => format!("{}://{}:***@{}:{}", self.scheme, u, h, self.port),
            (Some(u), None) => format!("{}://{}@{}:{}", self.scheme, u, h, self.port),
            _ => format!("{}://{}:{}", self.scheme, h, self.port),
        }
    }

    /// Host đúng dạng ghi vào URI: IPv6 bọc [].
    fn host_display(&self) -> String {
        if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() }
    }

    /// Dựng `wreq::Proxy` tương ứng. socks5h để proxy phân giải DNS, nhờ vậy DS không thấy
    /// truy vấn DNS đến từ máy thật.
    pub fn to_wreq(&self) -> Result<wreq::Proxy> {
        let uri = format!("{}://{}:{}", self.scheme, self.host_display(), self.port);
        let p = wreq::Proxy::all(uri).with_context(|| format!("proxy {} không dựng được", self.masked()))?;
        Ok(match (&self.user, &self.pass) {
            (Some(u), Some(pw)) => p.basic_auth(u, pw),
            _ => p,
        })
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Cấu hình proxy đã nạp, tính mỗi lần gọi `snapshot()` (đổi file là có hiệu lực ngay, không cần restart).
#[derive(Clone, Default)]
pub struct Pool {
    pub mode: Mode,
    pub entries: Vec<ProxyEntry>,
    /// lý do pool rỗng/sai — để dashboard hiện thay vì "không có proxy" chung chung
    pub problems: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    #[default]
    Off,
    Auto,
    Strict,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Mode::Auto,
            "strict" => Mode::Strict,
            _ => Mode::Off,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self { Mode::Off => "off", Mode::Auto => "auto", Mode::Strict => "strict" }
    }
}

/// Ghim vân tay → proxy, lưu xuống đĩa để IP của một "máy" không đổi qua các lần restart.
fn bindings() -> &'static Mutex<HashMap<String, String>> {
    static B: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(load_bindings()))
}

fn bindings_path() -> std::path::PathBuf {
    std::path::Path::new(&crate::server::creds_dir()).join("proxy-bindings.json")
}

fn load_bindings() -> HashMap<String, String> {
    std::fs::read_to_string(bindings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_bindings(m: &HashMap<String, String>) {
    let p = bindings_path();
    let tmp = p.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string_pretty(m) {
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn proxies_file() -> std::path::PathBuf {
    let f = crate::settings::get("DS_PROXY_FILE");
    if f.trim().is_empty() { bindings_path().with_file_name("proxies.txt") } else { std::path::PathBuf::from(f.trim()) }
}

/// Cấu hình hiện hành: đọc lại mỗi lần gọi để sửa file/dashboard là có hiệu lực ngay.
pub fn snapshot() -> Pool {
    let mode = Mode::parse(&crate::settings::get("DS_PROXY_MODE"));
    if mode == Mode::Off {
        return Pool { mode, ..Default::default() };
    }
    let mut problems = Vec::new();
    let mut entries = Vec::new();

    // 1) danh sách trong settings (override dashboard hoặc env) — nhiều dòng hoặc phân cách bằng dấu phẩy
    let inline = crate::settings::get("DS_PROXY_LIST");
    for line in inline.split(['\n', ',']) {
        let line = line.trim();
        if line.is_empty() { continue; }
        match ProxyEntry::parse(line) {
            Ok(e) => entries.push(e),
            Err(err) => problems.push(format!("{}: {:#}", line, err)),
        }
    }

    // 2) file proxy, mỗi dòng một cái; dòng bắt đầu bằng # là chú thích
    let file = proxies_file();
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            for (n, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                match ProxyEntry::parse(line) {
                    Ok(e) => entries.push(e),
                    Err(err) => problems.push(format!("{}:{}: {:#}", file.display(), n + 1, err)),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if entries.is_empty() && mode != Mode::Off {
                problems.push(format!("chưa có proxy nào: đặt DS_PROXY_LIST hoặc tạo {}", file.display()));
            }
        }
        Err(e) => problems.push(format!("không đọc được {}: {}", file.display(), e)),
    }

    // trùng chuỗi thì bỏ, giữ thứ tự
    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| seen.insert(e.raw.clone()));

    Pool { mode, entries, problems }
}

/// Proxy đã ghim cho vân tay này. Chưa ghim → chọn proxy ít dùng nhất (đếm trên bảng ghim hiện có)
/// rồi ghim luôn, để hai vân tay khác nhau KHÔNG rơi vào cùng một IP khi pool còn chỗ.
pub fn proxy_for(fp_seed: &str, pool: &Pool) -> Option<ProxyEntry> {
    if pool.mode == Mode::Off || pool.entries.is_empty() {
        return None;
    }
    let mut m = bindings().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(raw) = m.get(fp_seed).cloned() {
        if let Some(e) = pool.entries.iter().find(|e| e.raw == raw) {
            return Some(e.clone());
        }
        // proxy đã ghim không còn trong danh sách: bỏ ghim cũ rồi chọn lại, tránh acc kẹt vĩnh viễn
        m.remove(fp_seed);
        save_bindings(&m);
    }
    // đếm số vân tay đang ghim vào từng proxy
    let mut load: HashMap<String, usize> = HashMap::new();
    for raw in m.values() {
        *load.entry(raw.clone()).or_insert(0) += 1;
    }
    let pick = pool.entries.iter().min_by_key(|e| load.get(&e.raw).copied().unwrap_or(0))?.clone();
    m.insert(fp_seed.to_string(), pick.raw.clone());
    save_bindings(&m);
    Some(pick)
}

/// Bỏ ghim của một vân tay (khi acc bị xoá hẳn hoặc người dùng muốn đổi IP).
pub fn unbind(fp_seed: &str) {
    let mut m = bindings().lock().unwrap_or_else(|e| e.into_inner());
    if m.remove(fp_seed).is_some() {
        save_bindings(&m);
    }
}

/// Bảng ghim hiện tại để dashboard hiển thị: (vân tay, proxy gốc).
pub fn snapshot_bindings() -> Vec<(String, String)> {
    bindings().lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(fp, raw)| (fp.clone(), raw.clone())).collect()
}

/// Ghi đè danh sách proxy (một dòng một proxy). Chỉ ghi khi MỌI dòng đều hợp lệ —
/// ghi một nửa danh sách là âm thầm làm acc mất proxy.
pub fn write_file(lines: &str) -> Result<usize> {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for (n, line) in lines.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        match ProxyEntry::parse(line) {
            Ok(e) => ok.push(e.raw),
            Err(e) => errs.push(format!("dòng {}: {:#}", n + 1, e)),
        }
    }
    if !errs.is_empty() {
        anyhow::bail!("{}", errs.join("; "));
    }
    let path = proxies_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let mut out = ok.join("\n");
    if !out.is_empty() { out.push('\n'); }
    std::fs::write(&path, out).with_context(|| format!("không ghi được {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(ok.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_schemes_auth_and_ipv6() {
        let p = ProxyEntry::parse("http://u:p%40ss@1.2.3.4:8080").unwrap();
        assert_eq!((p.scheme.as_str(), p.host.as_str(), p.port), ("http", "1.2.3.4", 8080));
        assert_eq!((p.user.as_deref(), p.pass.as_deref()), (Some("u"), Some("p@ss")));
        assert_eq!(p.masked(), "http://u:***@1.2.3.4:8080");

        let s = ProxyEntry::parse("socks5h://[::1]:1080").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("::1", 1080));
        assert!(s.user.is_none());
        assert!(s.to_wreq().is_ok(), "socks phải dựng được Proxy");
    }

    #[test]
    fn rejects_bad_lines_instead_of_skipping() {
        for bad in ["1.2.3.4:8080", "ftp://h:1", "http://h", "http://h:0", "http://h:abc", "http://:80", "socks5://u@h:1"] {
            assert!(ProxyEntry::parse(bad).is_err(), "phải từ chối: {}", bad);
        }
    }

    #[test]
    fn distributes_evenly_and_stays_stable() {
        // Bản sao thuật toán chọn trong `proxy_for`, chạy trên registry riêng để không đụng file ghim thật.
        // PHẢI có nhánh trả sớm khi seed đã ghim — thiếu nó là ghim bị ghi đè mỗi lần gọi, tức IP của
        // một "máy" nhảy loạn. Đây chính là chỗ test này canh.
        fn pick(seed: &str, entries: &[ProxyEntry], m: &mut HashMap<String, String>) -> String {
            if let Some(raw) = m.get(seed) {
                if entries.iter().any(|e| &e.raw == raw) {
                    return raw.clone();
                }
            }
            let mut load: HashMap<String, usize> = HashMap::new();
            for raw in m.values() { *load.entry(raw.clone()).or_insert(0) += 1; }
            let best = entries.iter().min_by_key(|e| load.get(&e.raw).copied().unwrap_or(0)).unwrap();
            m.insert(seed.to_string(), best.raw.clone());
            best.raw.clone()
        }

        let entries: Vec<ProxyEntry> = ["http://10.0.0.1:1", "http://10.0.0.2:2", "http://10.0.0.3:3"]
            .iter().map(|s| ProxyEntry::parse(s).unwrap()).collect();
        let mut m: HashMap<String, String> = HashMap::new();
        let seeds: Vec<String> = (0..6).map(|i| format!("seed-{i}")).collect();
        let assigned: Vec<String> = seeds.iter().map(|s| pick(s, &entries, &mut m)).collect();

        // 6 vân tay, 3 proxy → mỗi proxy đúng 2: hai "máy" khác nhau không dùng chung một IP khi còn chỗ
        for e in &entries {
            let n = assigned.iter().filter(|a| *a == &e.raw).count();
            assert_eq!(n, 2, "{} phải được 2 vân tay, thực tế {}", e.raw, n);
        }
        // ghim lại: cùng vân tay → cùng proxy (IP của một "máy" không nhảy)
        let again: Vec<String> = seeds.iter().map(|s| pick(s, &entries, &mut m)).collect();
        assert_eq!(assigned, again, "ghim phải ổn định qua các lần gọi");
    }

    #[test]
    fn mode_parses() {
        assert_eq!(Mode::parse("AUTO"), Mode::Auto);
        assert_eq!(Mode::parse("strict"), Mode::Strict);
        assert_eq!(Mode::parse(""), Mode::Off);
        assert_eq!(Mode::parse("bậy"), Mode::Off);
    }
}

