// Log trong bộ nhớ: vòng đệm các dòng gần nhất + kênh phát trực tiếp cho dashboard (/api/logs, /api/logs/stream).
// Mọi dòng vẫn ra stderr như cũ (journalctl). Dòng quá dài bị cắt để vòng đệm không phình.
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use tokio::sync::broadcast;

const CAP: usize = 3000;
const MAX_LINE: usize = 4000;

#[derive(Clone, serde::Serialize)]
pub struct Line {
    pub seq: u64,
    pub t: u64, // ms epoch
    pub line: String,
}

struct Inner { buf: VecDeque<Line>, next: u64 }

fn inner() -> &'static Mutex<Inner> {
    static I: OnceLock<Mutex<Inner>> = OnceLock::new();
    I.get_or_init(|| Mutex::new(Inner { buf: VecDeque::with_capacity(CAP), next: 1 }))
}

fn tx() -> &'static broadcast::Sender<Line> {
    static T: OnceLock<broadcast::Sender<Line>> = OnceLock::new();
    T.get_or_init(|| broadcast::channel(1024).0)
}

pub fn push(mut s: String) {
    if s.chars().count() > MAX_LINE {
        s = s.chars().take(MAX_LINE).collect::<String>() + "…";
    }
    let t = now_ms();
    let line = {
        let mut g = inner().lock().unwrap_or_else(|e| e.into_inner());
        let l = Line { seq: g.next, t, line: s };
        g.next += 1;
        if g.buf.len() == CAP { g.buf.pop_front(); }
        g.buf.push_back(l.clone());
        l
    };
    let _ = tx().send(line.clone()); // không có người xem thì bỏ qua
    file_log("log", format!("{} {}", fmt_ts(line.t).1, line.line));
}

/// Các dòng có seq > after (tối đa `limit` dòng cuối).
pub fn since(after: u64, limit: usize) -> Vec<Line> {
    let g = inner().lock().unwrap_or_else(|e| e.into_inner());
    let v: Vec<Line> = g.buf.iter().filter(|l| l.seq > after).cloned().collect();
    let skip = v.len().saturating_sub(limit);
    v.into_iter().skip(skip).collect()
}

pub fn subscribe() -> broadcast::Receiver<Line> { tx().subscribe() }

// ---------------------------------------------------------------------------------------------------------------------
// Lưu log ra file để debug về sau: `<dữ liệu>/logs/whalegate-YYYYMMDD.log` (mọi dòng log, như trong dashboard) và
// `debug-YYYYMMDD.jsonl` (sự kiện chi tiết: nối lại, đuôi SSE khi đứt, challenge…). Theo ngày (múi giờ DS_TZ_OFFSET), giữ
// KEEP_DAYS ngày, mỗi file tối đa FILE_CAP byte. Ghi bằng một thread riêng nên không chặn runtime. Không bao giờ ghi
// token/cookie — chỉ những gì elog! vốn đã in.
// ---------------------------------------------------------------------------------------------------------------------
const KEEP_DAYS: i64 = 7;
const FILE_CAP: u64 = 64 * 1024 * 1024;
const FILE_LINE_MAX: usize = 16_000;

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((yoe + era * 400) + (m <= 2) as i64, m, d)
}

/// (`YYYYMMDD`, `YYYY-MM-DD HH:MM:SS.mmm`) theo giờ địa phương cấu hình (DS_TZ_OFFSET).
pub fn fmt_ts(ms: u64) -> (String, String) {
    let local = ms as i64 / 1000 + crate::settings::int("DS_TZ_OFFSET");
    let (days, sod) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    (format!("{y:04}{m:02}{d:02}"), format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}", sod / 3600, sod % 3600 / 60, sod % 60, ms % 1000))
}

pub fn now_ms() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0) }

pub fn logs_dir() -> std::path::PathBuf { std::path::Path::new(&crate::server::creds_dir()).join("logs") }

/// Gửi một dòng cho thread ghi file. `kind`: "log" → .log, "debug" → .jsonl.
fn file_log(kind: &'static str, line: String) {
    use std::sync::mpsc::{channel, Sender};
    static W: OnceLock<Mutex<Sender<(&'static str, String)>>> = OnceLock::new();
    let w = W.get_or_init(|| {
        let (tx, rx) = channel::<(&'static str, String)>();
        std::thread::Builder::new()
            .name("log-file-writer".into())
            .spawn(move || {
                use std::io::Write;
                let dir = logs_dir();
                let _ = std::fs::create_dir_all(&dir);
                prune_old(&dir);
                let mut cur: Option<(String, &'static str, std::fs::File, u64)> = None;
                let mut last_prune_day = String::new();
                for (kind, mut line) in rx {
                    if line.chars().count() > FILE_LINE_MAX { line = line.chars().take(FILE_LINE_MAX).collect::<String>() + "…"; }
                    let (day, _) = fmt_ts(now_ms());
                    let ext = if kind == "debug" { "jsonl" } else { "log" };
                    let name = format!("{}-{}.{}", if kind == "debug" { "debug" } else { "whalegate" }, day, ext);
                    let path = dir.join(&name);
                    // một handle cho mỗi (loại, ngày); đổi ngày/loại thì mở lại
                    if !matches!(&cur, Some((d, k, _, _)) if *d == day && *k == kind) {
                        let _ = std::fs::create_dir_all(&dir);
                        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                            Ok(f) => { let len = f.metadata().map(|m| m.len()).unwrap_or(0); cur = Some((day.clone(), kind, f, len)); }
                            Err(_) => { cur = None; continue; }
                        }
                        if last_prune_day != day { prune_old(&dir); last_prune_day = day.clone(); }
                    }
                    if let Some((_, _, f, len)) = cur.as_mut() {
                        if *len >= FILE_CAP { continue; }
                        let bytes = format!("{}\n", line);
                        if f.write_all(bytes.as_bytes()).is_ok() { *len += bytes.len() as u64; }
                    }
                }
            })
            .expect("spawn log file writer thread");
        Mutex::new(tx)
    });
    let _ = w.lock().unwrap_or_else(|e| e.into_inner()).send((kind, line));
}

/// Xoá file log quá KEEP_DAYS ngày (theo ngày trong tên file).
fn prune_old(dir: &std::path::Path) {
    let (today, _) = fmt_ts(now_ms());
    let day_num = |s: &str| -> Option<i64> {
        let (y, m, d): (i64, i64, i64) = (s.get(0..4)?.parse().ok()?, s.get(4..6)?.parse().ok()?, s.get(6..8)?.parse().ok()?);
        // số ngày kể từ 1970 (thuật toán days_from_civil)
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        Some(era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468)
    };
    let Some(t) = day_num(&today) else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(date) = name.strip_prefix("whalegate-").or_else(|| name.strip_prefix("debug-")) else { continue };
        if let Some(d) = day_num(date) { if t - d >= KEEP_DAYS { let _ = std::fs::remove_file(e.path()); } }
    }
}

/// Sự kiện chi tiết cho debug: một dòng JSON `{"t","ts","kind",…data}` vào `debug-YYYYMMDD.jsonl`. Không hiện trong
/// dashboard (tránh nhiễu) và không ra stderr.
pub fn debug(kind: &str, data: serde_json::Value) {
    let t = now_ms();
    let mut v = serde_json::json!({"t": t, "ts": fmt_ts(t).1, "kind": kind});
    if let (Some(o), Some(d)) = (v.as_object_mut(), data.as_object()) { for (k, x) in d { o.insert(k.clone(), x.clone()); } }
    file_log("debug", v.to_string());
}

/// Đọc log trong `hours` giờ gần nhất từ file (cũ → mới): (dòng .log, dòng .jsonl). Mỗi phần tối đa `cap` byte (giữ phần mới).
pub fn read_recent(hours: u64, cap: usize) -> (String, String) {
    let dir = logs_dir();
    let now = now_ms();
    let cutoff_ms = now.saturating_sub(hours.max(1) * 3_600_000);
    let cutoff_ts = fmt_ts(cutoff_ms).1;
    let days: Vec<String> = (0..=(hours.max(1) / 24 + 1)).rev().map(|i| fmt_ts(now.saturating_sub(i * 86_400_000)).0).collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::new();
    let (mut log, mut dbg) = (String::new(), String::new());
    for d in days {
        if !seen.insert(d.clone()) { continue; }
        if let Ok(t) = std::fs::read_to_string(dir.join(format!("whalegate-{d}.log"))) {
            let mut keep = false;
            for l in t.lines() {
                // dòng mới bắt đầu bằng "YYYY-MM-DD HH:MM:SS.mmm "; dòng không có mốc (log nhiều dòng) theo dòng trước
                if l.len() > 24 && l.as_bytes().get(4) == Some(&b'-') && l.as_bytes().get(10) == Some(&b' ') { keep = &l[..23] >= cutoff_ts.as_str(); }
                if keep { log.push_str(l); log.push('\n'); }
            }
        }
        if let Ok(t) = std::fs::read_to_string(dir.join(format!("debug-{d}.jsonl"))) {
            for l in t.lines() {
                let ok = serde_json::from_str::<serde_json::Value>(l).ok().and_then(|v| v["t"].as_u64()).map(|x| x >= cutoff_ms).unwrap_or(true);
                if ok { dbg.push_str(l); dbg.push('\n'); }
            }
        }
    }
    let trim = |s: String| if s.len() > cap { let mut i = s.len() - cap; while !s.is_char_boundary(i) { i += 1; } format!("[… cắt bớt phần cũ, giữ {} byte cuối …]\n{}", cap, &s[i..]) } else { s };
    (trim(log), trim(dbg))
}

/// Như eprintln! nhưng đồng thời đẩy vào vòng đệm cho dashboard.
#[macro_export]
macro_rules! elog {
    ($($arg:tt)*) => {{
        let __s = format!($($arg)*);
        eprintln!("{}", __s);
        $crate::logbuf::push(__s);
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ring_keeps_order_and_since_filters() {
        for i in 0..5 { push(format!("dòng {i}")); }
        let all = since(0, 10_000);
        let last = all.last().unwrap().seq;
        assert!(all.windows(2).all(|w| w[0].seq < w[1].seq));
        push("mới".into());
        let n = since(last, 100);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].line, "mới");
        push("x".repeat(MAX_LINE + 50));
        assert!(since(0, 1)[0].line.chars().count() <= MAX_LINE + 1);
    }
}

/// Ghi một dòng JSONL vào `<thư mục dữ liệu>/<file>` mà không chặn luồng gọi: một thread ghi riêng nhận qua channel
/// (giữ đúng thứ tự dòng, I/O đĩa không nằm trên runtime async). `rotate_at`: file vượt cỡ này (byte) thì đổi tên
/// thành `<file>.1` (giữ một bản cũ) trước khi ghi tiếp.
pub fn append_jsonl(file: &'static str, line: String, rotate_at: Option<u64>) {
    use std::sync::mpsc::{channel, Sender};
    static W: OnceLock<Mutex<Sender<(&'static str, String, Option<u64>)>>> = OnceLock::new();
    let w = W.get_or_init(|| {
        let (tx, rx) = channel::<(&'static str, String, Option<u64>)>();
        std::thread::Builder::new()
            .name("jsonl-writer".into())
            .spawn(move || {
                use std::io::Write;
                for (file, line, rotate_at) in rx {
                    let path = std::path::Path::new(&crate::server::creds_dir()).join(file);
                    if let Some(max) = rotate_at {
                        if std::fs::metadata(&path).map(|m| m.len() > max).unwrap_or(false) {
                            let _ = std::fs::rename(&path, path.with_extension(format!("{}.1", path.extension().and_then(|e| e.to_str()).unwrap_or(""))));
                        }
                    }
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                        let _ = f.write_all(format!("{}\n", line).as_bytes());
                    }
                }
            })
            .expect("spawn jsonl writer thread");
        Mutex::new(tx)
    });
    let _ = w.lock().unwrap_or_else(|e| e.into_inner()).send((file, line, rotate_at));
}

#[cfg(test)]
mod jsonl_tests {
    use super::*;

    fn wait_for(path: &std::path::Path, lines: usize) -> Vec<String> {
        for _ in 0..200 {
            let v: Vec<String> = std::fs::read_to_string(path).unwrap_or_default().lines().map(|l| l.to_string()).collect();
            if v.len() >= lines { return v; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("writer thread không ghi đủ {} dòng vào {}", lines, path.display());
    }

    #[test]
    fn append_jsonl_keeps_order_and_rotates() {
        let dir = std::path::PathBuf::from(crate::server::creds_dir());
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("order-test.jsonl");
        let _ = std::fs::remove_file(&p);
        for i in 0..50 { append_jsonl("order-test.jsonl", format!("{{\"i\":{}}}", i), None); }
        let got = wait_for(&p, 50);
        assert_eq!(got, (0..50).map(|i| format!("{{\"i\":{}}}", i)).collect::<Vec<_>>());

        // vượt 10 byte → bản cũ thành .jsonl.1, file mới chỉ còn dòng vừa ghi
        append_jsonl("order-test.jsonl", "{\"after\":1}".into(), Some(10));
        let mut got = Vec::new();
        for _ in 0..200 {
            got = std::fs::read_to_string(&p).unwrap_or_default().lines().map(|l| l.to_string()).collect();
            if got.first().map(|l| l.contains("after")).unwrap_or(false) { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got, vec!["{\"after\":1}".to_string()]);
        assert_eq!(std::fs::read_to_string(dir.join("order-test.jsonl.1")).unwrap().lines().count(), 50);
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn timestamps_are_formatted_in_local_time() {
        // 2026-11-04 03:09:32.979 UTC, múi giờ mặc định +7 → 10:09:32.979
        std::env::remove_var("DS_TZ_OFFSET");
        let (day, ts) = fmt_ts(1_793_761_772_979);
        assert_eq!(day, "20261104");
        assert_eq!(ts, "2026-11-04 10:09:32.979");
        // ngoài biên: 1970-01-01 00:00 UTC → 07:00 giờ địa phương
        assert_eq!(fmt_ts(0).1, "1970-01-01 07:00:00.000");
    }

    #[test]
    fn debug_events_are_json_with_kind_and_time() {
        debug("unit_test_event", serde_json::json!({"x": 1, "note": "ổn"}));
        // thread ghi chạy nền: đợi file xuất hiện
        let (_, day) = (0, fmt_ts(now_ms()).0);
        let path = logs_dir().join(format!("debug-{day}.jsonl"));
        let mut found = None;
        for _ in 0..300 {
            if let Ok(t) = std::fs::read_to_string(&path) {
                if let Some(l) = t.lines().find(|l| l.contains("unit_test_event")) { found = Some(l.to_string()); break; }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let v: serde_json::Value = serde_json::from_str(&found.expect("không thấy dòng debug")).unwrap();
        assert_eq!(v["kind"], "unit_test_event");
        assert_eq!(v["x"], 1);
        assert!(v["ts"].as_str().unwrap().starts_with(&format!("{}-", &day[..4])));
    }

    #[test]
    fn log_lines_reach_the_daily_file_and_export_reads_them() {
        push("[unit] dòng thử ghi file".into());
        let mut ok = false;
        for _ in 0..300 {
            let (log, _) = read_recent(1, 1 << 20);
            if log.contains("[unit] dòng thử ghi file") { ok = true; break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ok, "export không đọc lại được dòng vừa ghi");
    }
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    #[test]
    fn old_files_are_pruned_recent_ones_kept() {
        let dir = std::env::temp_dir().join(format!("wg-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let today = fmt_ts(now_ms()).0;
        let yesterday = fmt_ts(now_ms() - 86_400_000).0;
        for n in ["whalegate-20200101.log", "debug-20200101.jsonl", &format!("whalegate-{today}.log"), &format!("debug-{yesterday}.jsonl"), "unrelated.txt"] {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        prune_old(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        left.sort();
        let mut want = vec![format!("debug-{yesterday}.jsonl"), format!("whalegate-{today}.log"), "unrelated.txt".to_string()];
        want.sort();
        assert_eq!(left, want);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
