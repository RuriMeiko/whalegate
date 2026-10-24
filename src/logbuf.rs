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
