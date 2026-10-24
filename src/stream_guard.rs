// Bộ lọc stream content/reasoning khi request có tools: chỉ phát phần chắc chắn là văn xuôi, giữ lại markup
// tool-call. Tách khỏi run_chat để test được với đúng code chạy thật (fuzz chia chunk ngẫu nhiên, benchmark).
use crate::server::{has_strong_tool_marker, has_tool_marker, safe_flush_len};

/// Content: phát văn xuôi ngay, giữ từ marker tool-call trở đi tới khi stream xong.
pub(crate) struct ContentGuard {
    hold: bool,       // có tools → phải giữ markup; không có thì phát thẳng
    pending: String,  // chưa phát
    streamed: String, // đã phát — để tính phần văn xuôi còn thiếu khi khối tool-call được parse
    in_block: bool,   // đã gặp marker: giữ hết, khỏi quét lại buffer mỗi chunk (O(n²) với args lớn)
}

impl ContentGuard {
    pub(crate) fn new(hold: bool) -> Self {
        ContentGuard { hold, pending: String::new(), streamed: String::new(), in_block: false }
    }

    /// Nhận một chunk, trả phần được phép phát ngay.
    pub(crate) fn push(&mut self, t: &str) -> Option<String> {
        self.pending.push_str(t);
        if self.in_block { return None; }
        let cut = if self.hold { safe_flush_len(&self.pending) } else { self.pending.len() };
        let out = (cut > 0).then(|| {
            let head: String = self.pending.drain(..cut).collect();
            self.streamed.push_str(&head);
            head
        });
        self.in_block = self.hold && has_tool_marker(&self.pending);
        out
    }

    /// Khối tool-call đã parse: văn xuôi trước khối mà chưa phát. `before` là văn xuôi parser trả (đã trim, đã gỡ
    /// mảnh markup). Khớp theo NỘI DUNG chứ không đếm ký tự — đếm lệch khi `before` bị trim/gỡ mảnh làm mất chữ.
    pub(crate) fn remainder(&self, before: &str) -> String {
        let s = self.streamed.trim();
        if s.is_empty() { return before.to_string(); }
        match before.strip_prefix(s) {
            Some(rest) if self.streamed.ends_with(char::is_whitespace) => rest.trim_start().to_string(),
            Some(rest) => rest.to_string(),
            // văn xuôi đã phát không còn là tiền tố (parser gỡ mảnh nằm trong phần đã phát): đừng phát lặp/lệch
            None => String::new(),
        }
    }

    /// Không có tool-call: phần còn giữ là văn bản thường.
    pub(crate) fn take_rest(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        self.streamed.push_str(&rest);
        rest
    }
}

/// Reasoning: model hay nháp khối tool-call trong lúc nghĩ. Gặp marker MẠNH (DSML/<invoke/token bản địa) thì bỏ phần
/// suy nghĩ còn lại; marker yếu (`<function` khi đang bàn về code) chỉ tạm giữ, cuối lượt vẫn phát đủ.
pub(crate) struct ReasoningGuard {
    hold: bool,
    buf: String,
    held: bool,    // đang giữ vì marker yếu: chỉ quét phần mới để tìm marker mạnh
    dropped: bool,
}

impl ReasoningGuard {
    pub(crate) fn new(hold: bool) -> Self {
        ReasoningGuard { hold, buf: String::new(), held: false, dropped: false }
    }

    pub(crate) fn dropped(&self) -> bool { self.dropped }

    pub(crate) fn push(&mut self, t: &str) -> Option<String> {
        if !self.hold { return (!t.is_empty()).then(|| t.to_string()); }
        if self.dropped || t.is_empty() { return None; }
        let old_len = self.buf.len();
        self.buf.push_str(t);
        if self.held {
            // chỉ quét cửa sổ mới + phần chồng lấn để bắt marker vắt qua ranh giới chunk
            let mut from = old_len.saturating_sub(64);
            while !self.buf.is_char_boundary(from) { from -= 1; }
            if has_strong_tool_marker(&self.buf[from..]) {
                self.dropped = true;
                self.buf.clear();
            }
            return None;
        }
        let cut = safe_flush_len(&self.buf);
        if has_strong_tool_marker(&self.buf) {
            self.dropped = true;
            let head = self.buf[..cut].to_string();
            self.buf.clear();
            return (!head.is_empty()).then_some(head);
        }
        let head: String = self.buf.drain(..cut).collect();
        self.held = has_tool_marker(&self.buf);
        (!head.is_empty()).then_some(head)
    }

    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.dropped || self.buf.is_empty() { return None; }
        Some(std::mem::take(&mut self.buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::extract_tool_calls;
    use serde_json::{json, Value};

    /// xorshift64 — đủ cho fuzz, khỏi kéo crate rand.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
        fn below(&mut self, n: usize) -> usize { (self.next() % n as u64) as usize }
    }

    fn tools() -> Vec<Value> {
        ["run_code", "bash", "read", "write", "todo"].iter().map(|n| json!({"type":"function","function":{"name":n,
            "parameters":{"type":"object","properties":{"code":{"type":"string"},"command":{"type":"string"},
            "description":{"type":"string"},"file_path":{"type":"string"},"content":{"type":"string"}}}}})).collect()
    }

    fn dsml(name: &str, args: &[(&str, &str)]) -> String {
        let mut s = format!("<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"{}\">\n", name);
        for (k, v) in args { s += &format!("<｜｜DSML｜｜ parameter name=\"{}\" string=\"true\">{}</｜｜DSML｜｜ parameter>\n", k, v); }
        s + "</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>"
    }

    fn corpus() -> Vec<String> {
        let mut c: Vec<String> = vec![
            "Xin chào! Đây là câu trả lời bình thường, có dấu tiếng Việt và emoji 🐳.".into(),
            "Compare: if a < b then swap; otherwise keep. ".repeat(20) + "Done.",
            "Use <div><span>hi</span></div> and <br/> in HTML. Generics: Vec<u8>, Option<Box<T>>.".into(),
            "XML example:\n<config>\n  <function>foo</function>\n  <tool>x</tool>\n</config>\nThat's all.".into(),
            "DeepSeek ends turns with <｜end▁of▁sentence｜> and roles <｜User｜>/<｜Assistant｜>.".into(),
            "Talking about DSML tool-call parsing and the | pipe | character.".into(),
            "Args were corrupted by a stray `</｜｜DSML｜｜ parameter>` inside new_string; fixed by re-issuing.".into(),
            "| col a | col b |\n|---|---|\n| 1 | 2 |\n| x DS | DSM |".into(),
            format!("\n\nI'll check if a < b first.\n{}", dsml("bash", &[("command", "ls -la"), ("description", "list")])),
            format!("Đọc file trước.\n{}", dsml("read", &[("file_path", "/tmp/a.txt")])),
            format!("{}", dsml("write", &[("file_path", "/x.rs"), ("content", "fn main() { let v: Vec<u8> = vec![]; if 1 < 2 {} }")])),
            "<｜||DSML||_call>\n<｜｜DSML｜｜ invoke name=\"run_code\">\n<｜｜DSML｜｜ parameter name=\"code\" string=\"true\">x()</｜｜DSML｜｜ parameter>\n</｜｜DSML｜｜ invoke>\n</｜｜DSML｜｜ calls>".into(),
            "<||DSML||_call>\n<||DSML|| invoke name=\"bash\">\n<||DSML|| parameter name=\"command\" string=\"true\">pwd</||DSML|| parameter>\n</||DSML|| invoke>\n</||DSML||_call>".into(),
            "Running it.\n<｜tool▁calls▁begin｜><｜tool▁call▁begin｜>function<｜tool▁sep｜>bash\n```json\n{\"command\": \"echo {a}\"}\n```<｜tool▁call▁end｜><｜tool▁calls▁end｜>".into(),
            "<｜工具调用｜>\n".repeat(12) + "<｜工具调用｜>",
            "Example only: <｜tool▁call▁begin｜>bash<｜tool▁sep｜>{\"command\":\"rm -rf /\"}<｜tool▁call▁end｜> — never run that.".into(),
            "Lệnh mẫu: <bash command=\"rm -rf build\"/> trong tài liệu, đừng chạy.".into(),
            "Partial marker at the very end <｜｜DS".into(),
            "Unclosed < tag then a long tail ".to_string() + &"x".repeat(300),
        ];
        // mẫu lỗi thật đã lưu (nếu có trên máy này)
        let p = std::path::PathBuf::from(crate::server::home_dir()).join(".whalegate/samples.jsonl");
        for l in std::fs::read_to_string(p).unwrap_or_default().lines() {
            if let Some(raw) = serde_json::from_str::<Value>(l).ok().and_then(|v| v["raw"].as_str().map(|s| s.to_string())) { c.push(raw); }
        }
        c
    }

    #[derive(Debug, PartialEq)]
    enum Outcome { Calls(usize), Retry, Text }

    /// Đúng thứ tự xử lý của run_chat lúc Done: parse → (gọi tool | retry vì marker mạnh | văn bản).
    fn simulate(full: &str, tools: &[Value], chunks: &[usize]) -> (String, Outcome) {
        let mut g = ContentGuard::new(true);
        let mut out = String::new();
        let chars: Vec<char> = full.chars().collect();
        let (mut i, mut k) = (0, 0);
        while i < chars.len() {
            let n = chunks[k % chunks.len()].max(1).min(chars.len() - i);
            let piece: String = chars[i..i + n].iter().collect();
            if let Some(o) = g.push(&piece) { out += &o; }
            i += n; k += 1;
        }
        match extract_tool_calls(full, tools) {
            Some((before, calls)) => { out += &g.remainder(&before); (out, Outcome::Calls(calls.len())) }
            None if has_strong_tool_marker(full) => (out, Outcome::Retry),
            None => { out += &g.take_rest(); (out, Outcome::Text) }
        }
    }

    fn ws(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }
    /// markup gọi tool lộ ra client (chữ "DSML" trơn trong văn xuôi thì không tính)
    const LEAKS: [&str; 7] = ["｜DSML", "DSML｜", "|DSML", "DSML|", "<｜tool", "<｜工具", "<invoke"];

    #[test]
    fn streaming_matches_whole_and_never_leaks() {
        let tools = tools();
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let corpus = corpus();
        let mut stats = std::collections::HashMap::new();
        for full in &corpus {
            let (base, outcome) = simulate(full, &tools, &[usize::MAX]);
            *stats.entry(format!("{:?}", outcome).split('(').next().unwrap().to_string()).or_insert(0) += 1;
            // mẫu dài (bản tóm tắt hàng chục KB) chia ít lần hơn để bộ test vẫn nhanh
            for _ in 0..if full.len() > 4096 { 8 } else { 40 } {
                let chunks: Vec<usize> = (0..7).map(|_| 1 + rng.below(12)).collect();
                let (got, o2) = simulate(full, &tools, &chunks);
                assert_eq!(o2, outcome);
                assert_eq!(got, base, "kết quả phụ thuộc cách chia chunk {:?}\nraw={:?}", chunks, full);
            }
            match outcome {
                Outcome::Text => assert_eq!(base, *full, "văn bản thường phải ra nguyên vẹn"),
                Outcome::Calls(_) | Outcome::Retry => {
                    for l in LEAKS { assert!(!base.contains(l), "lộ {:?} ra client: {:?}\nraw={:?}", l, base, full); }
                    if let Outcome::Calls(_) = outcome {
                        let before = extract_tool_calls(full, &tools).unwrap().0;
                        assert_eq!(ws(&base), ws(&before), "mất/lặp chữ trước khối tool-call\nraw={:?}", full);
                    }
                }
            }
            // không có tools: luôn phát nguyên văn
            let mut g = ContentGuard::new(false);
            let mut out = g.push(full).unwrap_or_default();
            out += &g.take_rest();
            assert_eq!(out, *full);
        }
        eprintln!("corpus={} outcomes={:?}", corpus.len(), stats);
    }

    #[test]
    fn reasoning_guard_drops_only_strong_markup() {
        let mut rng = Rng(42);
        let cases = [
            ("Think: maybe use <function> in XML, and a < b. Then answer.", false),
            ("Let me draft:\n<｜｜DSML｜｜ calls>\n<｜｜DSML｜｜ invoke name=\"bash\">…", true),
            ("First <function> mention, long thoughts ".to_string().leak() as &str, false),
            ("weak <tool> then later <｜tool▁call▁begin｜>bash", true),
        ];
        for (text, strong) in cases {
            for _ in 0..30 {
                let mut g = ReasoningGuard::new(true);
                let chars: Vec<char> = text.chars().collect();
                let mut out = String::new();
                let mut i = 0;
                while i < chars.len() {
                    let n = (1 + rng.below(9)).min(chars.len() - i);
                    if let Some(r) = g.push(&chars[i..i + n].iter().collect::<String>()) { out += &r; }
                    i += n;
                }
                if let Some(r) = g.finish() { out += &r; }
                if strong {
                    assert!(g.dropped() && !out.contains("DSML") && !out.contains("<｜tool"), "{:?}", out);
                } else {
                    assert_eq!(out, text, "marker yếu không được làm mất reasoning");
                }
            }
        }
    }

    #[test]
    fn fuzz_no_panic() {
        const FRAG: [&str; 30] = ["<", ">", "</", "｜", "|", "DSML", "DS", " calls", "invoke", " name=\"", "\"", "parameter",
            " string=\"true\"", "string=\"false\"", "{", "}", "\"code\":", "bash", "run_code", "<｜tool▁call▁begin｜>",
            "<｜tool▁sep｜>", "<｜工具调用｜>", "```json\n", "\n", " ", "é", "🐳", "工", "<bash command=\"x\"/>", "/>"];
        let tools = tools();
        let mut rng = Rng(7);
        for _ in 0..4000 {
            let n = 1 + rng.below(40);
            let s: String = (0..n).map(|_| FRAG[rng.below(FRAG.len())]).collect();
            let _ = extract_tool_calls(&s, &tools);
            let _ = (safe_flush_len(&s), has_tool_marker(&s), has_strong_tool_marker(&s));
            let mut g = ContentGuard::new(true);
            let mut r = ReasoningGuard::new(true);
            for ch in s.chars() { let c = ch.to_string(); let _ = g.push(&c); let _ = r.push(&c); }
            let _ = r.finish();
        }
    }

    /// cargo test --release bench_ -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_stream_and_parse() {
        let tools = tools();
        let t = |label: &str, f: &mut dyn FnMut()| { let s = std::time::Instant::now(); f(); eprintln!("{:<58} {:>8.2} ms", label, s.elapsed().as_secs_f64() * 1e3); };
        let stream = |text: &str, chunk: usize| {
            let mut g = ContentGuard::new(true);
            let b = text.as_bytes();
            let mut i = 0;
            while i < b.len() {
                let mut j = (i + chunk).min(b.len());
                while !text.is_char_boundary(j) { j += 1; }
                let _ = g.push(&text[i..j]);
                i = j;
            }
        };
        let big_code = "fn f() { let v: Vec<u8> = vec![1, 2]; if a < b { x(); } }\n".repeat(18_000); // ~1 MB
        let big_call = format!("Writing the file.\n{}", dsml("write", &[("file_path", "/x.rs"), ("content", &big_code)]));
        let prose_lt = "a < b ".to_string() + &"lorem ipsum dolor sit amet ".repeat(40_000); // ~1 MB, '<' không có '>'
        let html = "<p>Hello <b>world</b> and <i>more</i></p>\n".repeat(25_000); // ~1 MB
        let think = "Considering <function> usage. ".to_string() + &"step by step reasoning text ".repeat(40_000);

        t("stream 1MB tool-call (8-byte chunks)", &mut || stream(&big_call, 8));
        t("parse  1MB tool-call (extract_tool_calls)", &mut || { assert!(extract_tool_calls(&big_call, &tools).is_some()); });
        t("stream 1MB prose with unclosed '<'", &mut || stream(&prose_lt, 8));
        t("stream 1MB HTML prose", &mut || stream(&html, 8));
        t("parse  1MB HTML prose (no call expected)", &mut || { assert!(extract_tool_calls(&html, &tools).is_none()); });
        // đầu vào độc hại ~130KB (≈ trần output 32K token của một lượt): không được có đường bậc hai
        let n = 130_000;
        let adversarial: Vec<(&str, String)> = vec![
            ("'{' x n after native marker", format!("<｜工具调用｜>bash\n{}}}", "{".repeat(n))),
            ("'{' x n in dsml json variant", format!("<dsml:tool_call>{}</dsml:tool_call>", "{".repeat(n))),
            ("'<a' x n (xml fallback)", "<a".repeat(n / 2) + ">"),
            ("'<b>' x n unclosed tags", "<b>".repeat(n / 3)),
            ("invoke opens x many", "<｜｜DSML｜｜ invoke name=\"bash\">".repeat(n / 30) + "</｜｜DSML｜｜ calls>"),
            ("parameter opens x many", "<｜｜DSML｜｜ calls><｜｜DSML｜｜ invoke name=\"bash\">".to_string() + &"<｜｜DSML｜｜ parameter name=\"a\" string=\"true\">".repeat(n / 45) + "</｜｜DSML｜｜ calls>"),
            ("bare closers x many", "<｜｜DSML｜｜ calls>".to_string() + &"</｜｜DSML｜｜>".repeat(n / 15)),
            ("'DSML|' x many", "<|DSML|".repeat(n / 7) + ">"),
            ("name=\" x many (nested parser)", "<x invoke name=\"".repeat(n / 16) + ">"),
            ("command=\" x many", "<bash command=\"".repeat(n / 16) + "/>"),
            ("tool_call_begin x many", "<｜tool▁call▁begin｜>".repeat(n / 20) + "}"),
            ("backticks + markers", "`<invoke` ".repeat(n / 10) + ">"),
        ];
        for (label, s) in &adversarial {
            t(&format!("adversarial parse: {}", label), &mut || { let _ = extract_tool_calls(s, &tools); });
            t(&format!("adversarial stream: {}", label), &mut || stream(s, 8));
        }
        t("reasoning 1MB with weak marker early", &mut || {
            let mut g = ReasoningGuard::new(true);
            let mut i = 0; let b = think.as_bytes();
            while i < b.len() { let j = (i + 8).min(b.len()); let _ = g.push(&think[i..j]); i = j; }
            assert!(g.finish().is_some());
        });
    }
}
