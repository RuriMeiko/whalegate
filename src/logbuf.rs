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
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let line = {
        let mut g = inner().lock().unwrap_or_else(|e| e.into_inner());
        let l = Line { seq: g.next, t, line: s };
        g.next += 1;
        if g.buf.len() == CAP { g.buf.pop_front(); }
        g.buf.push_back(l.clone());
        l
    };
    let _ = tx().send(line); // không có người xem thì bỏ qua
}

/// Các dòng có seq > after (tối đa `limit` dòng cuối).
pub fn since(after: u64, limit: usize) -> Vec<Line> {
    let g = inner().lock().unwrap_or_else(|e| e.into_inner());
    let v: Vec<Line> = g.buf.iter().filter(|l| l.seq > after).cloned().collect();
    let skip = v.len().saturating_sub(limit);
    v.into_iter().skip(skip).collect()
}

pub fn subscribe() -> broadcast::Receiver<Line> { tx().subscribe() }

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
