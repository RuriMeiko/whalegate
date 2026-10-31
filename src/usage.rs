// Đếm token đã dùng (theo giờ UTC, lưu đĩa) + bảng giá API DeepSeek rẻ nhất (lấy live) → chi phí tương đương,
// tách giờ cao điểm / thấp điểm, ước tính khoản tiết kiệm so với gọi API trả tiền.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, OnceLock, RwLock};

const PRICING_URL: &str = "https://api-docs.deepseek.com/quick_start/pricing";
const PRICING_REFRESH_SECS: u64 = 6 * 3600;
const RETAIN_HOURS: u64 = 24 * 400;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct Bucket {
    req: u64,
    miss: u64, // input không trúng cache
    hit: u64,  // input trúng cache (ngữ cảnh đã nằm sẵn trong phiên web)
    out: u64,  // output + suy nghĩ
}

/// USD / 1M token. Mỗi cặp = (thấp điểm, cao điểm).
#[derive(Clone, Serialize)]
pub struct Pricing {
    pub model: String,
    pub hit: (f64, f64),
    pub miss: (f64, f64),
    pub out: (f64, f64),
    pub source: String, // "live" | "fallback"
    pub fetched_at: u64,
}

impl Pricing {
    /// Giá niêm yết khi không lấy được trang giá (deepseek-flash, tại thời điểm viết).
    fn fallback() -> Self {
        Pricing { model: "deepseek-flash".into(), hit: (0.003, 0.006), miss: (0.15, 0.30), out: (0.6, 1.2), source: "fallback".into(), fetched_at: 0 }
    }
}

/// Độ trễ các request gần đây (toàn bộ thời gian từ lúc nhận đến lúc xong, gồm retry/chờ tài khoản).
fn recent() -> &'static Mutex<VecDeque<(u64, u64, bool)>> {
    static R: OnceLock<Mutex<VecDeque<(u64, u64, bool)>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(VecDeque::with_capacity(400)))
}

pub fn record_request(ms: u64, ok: bool) {
    let mut r = recent().lock().unwrap_or_else(|e| e.into_inner());
    if r.len() == 400 { r.pop_front(); }
    r.push_back((now_secs(), ms, ok));
}

/// p50/p90 (ms) của các request thành công trong `window_secs` gần nhất + số request lỗi.
fn latency_report(window_secs: u64) -> Value {
    let cut = now_secs().saturating_sub(window_secs);
    let r = recent().lock().unwrap_or_else(|e| e.into_inner());
    let mut ok: Vec<u64> = r.iter().filter(|(t, _, o)| *t >= cut && *o).map(|(_, ms, _)| *ms).collect();
    let err = r.iter().filter(|(t, _, o)| *t >= cut && !*o).count();
    ok.sort_unstable();
    let pct = |p: f64| if ok.is_empty() { Value::Null } else { json!(ok[(((ok.len() - 1) as f64) * p).round() as usize]) };
    json!({"n": ok.len() + err, "ok": ok.len(), "err": err, "p50_ms": pct(0.5), "p90_ms": pct(0.9), "window_secs": window_secs})
}

struct Store {
    buckets: BTreeMap<u64, Bucket>, // key = giờ UTC (epoch/3600)
    dirty: bool,
}

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Store { buckets: load_file(), dirty: false }))
}

fn pricing_cell() -> &'static RwLock<Pricing> {
    static P: OnceLock<RwLock<Pricing>> = OnceLock::new();
    P.get_or_init(|| RwLock::new(Pricing::fallback()))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn file_path() -> std::path::PathBuf {
    std::path::Path::new(&crate::server::creds_dir()).join("usage.json")
}

fn load_file() -> BTreeMap<u64, Bucket> {
    std::fs::read_to_string(file_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Ghi nhận một lượt hoàn tất. `prompt` đã gồm phần `cached`.
pub fn record(prompt: u64, cached: u64, completion: u64) {
    let h = now_secs() / 3600;
    let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
    let b = s.buckets.entry(h).or_default();
    let cached = cached.min(prompt);
    b.req += 1;
    b.hit += cached;
    b.miss += prompt - cached;
    b.out += completion;
    s.dirty = true;
}

/// Ghi đĩa ngay (dùng trước khi khởi động lại).
pub fn flush_blocking() { flush(); }

fn flush() {
    let (json, ok) = {
        let mut s = store().lock().unwrap_or_else(|e| e.into_inner());
        if !s.dirty { return; }
        let cut = (now_secs() / 3600).saturating_sub(RETAIN_HOURS);
        s.buckets.retain(|h, _| *h >= cut);
        s.dirty = false;
        (serde_json::to_string(&s.buckets).unwrap_or_default(), true)
    };
    if !ok { return; }
    let p = file_path();
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// Nền: ghi đĩa gom lô + làm mới bảng giá định kỳ.
pub fn spawn_background() {
    let _ = store(); // nạp file ngay
    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            tokio::task::spawn_blocking(flush).await.ok();
        }
    });
    tokio::spawn(async {
        loop {
            match fetch_pricing().await {
                Ok(p) => { crate::elog!("[pricing] {} hit {:?} miss {:?} out {:?}", p.model, p.hit, p.miss, p.out); *pricing_cell().write().unwrap_or_else(|e| e.into_inner()) = p; }
                Err(e) => crate::elog!("[pricing] không lấy được giá live ({e}) — dùng bảng giá hiện có"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(PRICING_REFRESH_SECS)).await;
        }
    });
}

// ---------- bảng giá live ----------

async fn fetch_pricing() -> anyhow::Result<Pricing> {
    // trang giá công khai: đi bằng một trình duyệt giả lập cố định (không gắn với acc nào), không còn UA Chrome tự khai
    let html = wreq::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .emulation(crate::fingerprint::Fingerprint::from_seed("whalegate-pricing").emulation())
        // trang giá giờ trả 302; wreq mặc định không đi theo redirect như reqwest
        .redirect(wreq::redirect::Policy::limited(5))
        .build()?
        .get(PRICING_URL).send().await?.error_for_status()?.text().await?;
    parse_pricing(&html, now_secs()).ok_or_else(|| anyhow::anyhow!("không đọc được bảng giá"))
}

/// HTML → các dòng chữ (bỏ script/style/thẻ, giải mã vài entity).
fn html_lines(html: &str) -> Vec<String> {
    let mut s = html.to_string();
    for (open, close) in [("<script", "</script>"), ("<style", "</style>")] {
        while let Some(a) = s.find(open) {
            match s[a..].find(close) { Some(b) => s.replace_range(a..a + b + close.len(), ""), None => { s.truncate(a); break; } }
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c { '<' => { in_tag = true; out.push('\n'); } '>' => in_tag = false, _ if !in_tag => out.push(c), _ => {} }
    }
    out.replace("&amp;", "&").replace("&nbsp;", " ")
        .lines().map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{200b}').to_string()).filter(|l| !l.is_empty()).collect()
}

/// Lấy giá RẺ NHẤT mỗi hàng (hit/miss/out × thấp/cao điểm) trong bảng "PRICING".
fn parse_pricing(html: &str, now: u64) -> Option<Pricing> {
    let lines = html_lines(html);
    let start = lines.iter().position(|l| l.eq_ignore_ascii_case("PRICING"))?;
    let (mut row, mut peak) = (usize::MAX, false);
    let mut vals: [[Vec<f64>; 2]; 3] = Default::default();
    for l in &lines[start..] {
        let u = l.to_ascii_uppercase();
        if u.starts_with("CONCURRENCY") { break; }
        if u.contains("(CACHE HIT)") { row = 0; }
        else if u.contains("(CACHE MISS)") { row = 1; }
        else if u.starts_with("1M OUTPUT") { row = 2; }
        else if u == "OFF-PEAK" { peak = false; }
        else if u == "PEAK" { peak = true; }
        else if let Some(n) = l.strip_prefix('$') {
            if row < 3 { if let Ok(v) = n.trim().parse::<f64>() { vals[row][peak as usize].push(v); } }
        }
    }
    let min = |v: &Vec<f64>| v.iter().cloned().fold(f64::INFINITY, f64::min);
    let pair = |r: usize| (min(&vals[r][0]), min(&vals[r][1]));
    let (hit, miss, out) = (pair(0), pair(1), pair(2));
    if [hit.0, hit.1, miss.0, miss.1, out.0, out.1].iter().any(|v| !v.is_finite()) { return None; }
    // tên model rẻ nhất = cột đầu ("deepseek-flash"); trang liệt kê model ngay sau chữ MODEL
    let model = lines.iter().position(|l| l == "MODEL").and_then(|i| lines.get(i + 1)).cloned().unwrap_or_else(|| "deepseek-flash".into());
    Some(Pricing { model, hit, miss, out, source: "live".into(), fetched_at: now })
}

// ---------- giờ cao điểm ----------

/// (năm, tháng, ngày) từ số ngày kể từ 1970-01-01.
pub(crate) fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Ngày nghỉ lễ Trung Quốc 2026 (ước lượng theo lịch công bố, có thể lệch) + DS_CN_HOLIDAYS="YYYY-MM-DD,…" để bổ sung.
fn is_cn_holiday(y: i64, m: u32, d: u32) -> bool {
    const R: [(u32, u32, u32, u32); 7] = [(1, 1, 1, 3), (2, 15, 2, 23), (4, 4, 4, 6), (5, 1, 5, 5), (6, 19, 6, 21), (9, 25, 9, 27), (10, 1, 10, 7)];
    if y == 2026 && R.iter().any(|&(m1, d1, m2, d2)| (m, d) >= (m1, d1) && (m, d) <= (m2, d2)) { return true; }
    let key = format!("{:04}-{:02}-{:02}", y, m, d);
    std::env::var("DS_CN_HOLIDAYS").map(|v| v.split(',').any(|x| x.trim() == key)).unwrap_or(false)
}

/// Giờ UTC `h` (epoch/3600) có phải cao điểm? Thứ 2–6, 01–04 và 06–10 UTC, trừ nghỉ lễ.
pub fn is_peak(hour_epoch: u64) -> bool {
    let days = (hour_epoch / 24) as i64;
    let hod = (hour_epoch % 24) as u32;
    let wd = (days + 4).rem_euclid(7); // 1970-01-01 là thứ 5 → 0=CN
    if wd == 0 || wd == 6 { return false; }
    if !((1..4).contains(&hod) || (6..10).contains(&hod)) { return false; }
    let (y, m, d) = civil(days);
    !is_cn_holiday(y, m, d)
}

// ---------- báo cáo ----------

fn cost(b: &Bucket, p: &Pricing, peak: bool) -> f64 {
    let i = peak as usize;
    let pick = |t: (f64, f64)| if i == 0 { t.0 } else { t.1 };
    (b.miss as f64 * pick(p.miss) + b.hit as f64 * pick(p.hit) + b.out as f64 * pick(p.out)) / 1e6
}

#[derive(Default)]
struct Agg { req: u64, miss: u64, hit: u64, out: u64, cost: f64, cost_peak: f64, tok_peak: u64, cost_if_off: f64 }

impl Agg {
    fn add(&mut self, b: &Bucket, p: &Pricing, peak: bool) {
        let c = cost(b, p, peak);
        self.req += b.req; self.miss += b.miss; self.hit += b.hit; self.out += b.out;
        self.cost += c;
        self.cost_if_off += cost(b, p, false);
        if peak { self.cost_peak += c; self.tok_peak += b.miss + b.hit + b.out; }
    }
    fn json(&self) -> Value {
        let tokens = self.miss + self.hit + self.out;
        json!({
            "req": self.req, "tokens": tokens, "input": self.miss + self.hit, "cached": self.hit, "output": self.out,
            "cost": self.cost, "cost_peak": self.cost_peak, "cost_offpeak": self.cost - self.cost_peak,
            "tokens_peak": self.tok_peak, "peak_surcharge": self.cost - self.cost_if_off,
        })
    }
}

pub fn report() -> Value {
    let now = now_secs();
    let tz: i64 = crate::settings::int("DS_TZ_OFFSET");
    let p = pricing_cell().read().unwrap_or_else(|e| e.into_inner()).clone();
    let s = store().lock().unwrap_or_else(|e| e.into_inner());
    let hnow = now / 3600;
    let local_day = |h: u64| ((h * 3600) as i64 + tz).div_euclid(86400);
    let today = local_day(hnow);

    let (mut a_today, mut a_7d, mut a_30d, mut a_all) = (Agg::default(), Agg::default(), Agg::default(), Agg::default());
    let mut days: BTreeMap<i64, Agg> = BTreeMap::new();
    for (&h, b) in s.buckets.iter() {
        let peak = is_peak(h);
        a_all.add(b, &p, peak);
        if h + 24 * 30 > hnow { a_30d.add(b, &p, peak); }
        if h + 24 * 7 > hnow { a_7d.add(b, &p, peak); }
        let d = local_day(h);
        if d == today { a_today.add(b, &p, peak); }
        if d + 14 > today { days.entry(d).or_default().add(b, &p, peak); }
    }

    // 24 giờ gần nhất (cũ → mới)
    let hours: Vec<Value> = (0..24).map(|i| {
        let h = hnow - 23 + i;
        let b = s.buckets.get(&h).copied().unwrap_or_default();
        json!({"t": h * 3600, "input": b.miss + b.hit, "cached": b.hit, "output": b.out, "peak": is_peak(h), "cost": cost(&b, &p, is_peak(h))})
    }).collect();

    // 14 ngày gần nhất
    let daily: Vec<Value> = (0..14).map(|i| {
        let d = today - 13 + i;
        let a = days.get(&d);
        json!({"t": d * 86400 - tz, "tokens": a.map(|a| a.miss + a.hit + a.out).unwrap_or(0), "cost": a.map(|a| a.cost).unwrap_or(0.0)})
    }).collect();

    // lịch giá 24 giờ tới + lần đổi giá kế tiếp
    let now_peak = is_peak(hnow);
    let schedule: Vec<Value> = (0..24).map(|i| json!({"t": (hnow + i) * 3600, "peak": is_peak(hnow + i)})).collect();
    let next_switch = (1..24 * 14).map(|i| hnow + i).find(|&h| is_peak(h) != now_peak).map(|h| h * 3600);

    // dự toán tháng: bình quân ngày trên tối đa 7 ngày gần nhất có dữ liệu
    let first = s.buckets.keys().next().copied().unwrap_or(hnow);
    let span_days = (((hnow.saturating_sub(first)) as f64 / 24.0).max(1.0)).min(7.0);
    let per_day_tokens = (a_7d.miss + a_7d.hit + a_7d.out) as f64 / span_days;
    let per_day_cost = a_7d.cost / span_days;
    let per_day_peak_share = if a_7d.cost > 0.0 { a_7d.cost_peak / a_7d.cost } else { 0.0 };

    json!({
        "pricing": p,
        "now": now, "tz": tz, "now_peak": now_peak, "next_switch": next_switch,
        "today": a_today.json(), "d7": a_7d.json(), "d30": a_30d.json(), "all": a_all.json(),
        "projection": {"days_basis": span_days, "tokens_per_day": per_day_tokens, "cost_per_day": per_day_cost,
                       "month_tokens": per_day_tokens * 30.0, "month_cost": per_day_cost * 30.0, "peak_cost_share": per_day_peak_share,
                       "month_cost_if_offpeak": a_7d.cost_if_off / span_days * 30.0},
        "hours": hours, "daily": daily, "schedule": schedule,
        "latency": latency_report(3600),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_windows_follow_pricing_page() {
        // 2026-10-26 là thứ 2 (không nghỉ lễ): 01:00 UTC cao điểm, 04:00 thấp, 06:00 cao, 10:00 thấp
        let day = 20752u64; // ngày kể từ epoch của 2026-10-26
        assert_eq!(civil(day as i64), (2026, 10, 26));
        assert!(is_peak(day * 24 + 1) && !is_peak(day * 24 + 4) && is_peak(day * 24 + 6) && is_peak(day * 24 + 9) && !is_peak(day * 24 + 10));
        // 2026-10-24 (thứ 7) và 10-25 (CN) luôn thấp điểm
        assert!(!is_peak((day - 2) * 24 + 7) && !is_peak((day - 1) * 24 + 7));
        // Quốc khánh 2026-10-05 (thứ 2) là ngày nghỉ → thấp điểm
        let nd = 20731u64;
        assert_eq!(civil(nd as i64), (2026, 10, 5));
        assert!(!is_peak(nd * 24 + 7));
    }

    #[test]
    fn parses_cheapest_prices_from_page() {
        let page = "<script>x</script><div>MODEL</div><div>deepseek-flash</div><div>deepseek-v4-pro</div>\
          <div>PRICING</div><div>1M INPUT TOKENS</div><div>(CACHE HIT)</div><div>OFF-PEAK</div><div>$0.003</div><div>$0.022</div>\
          <div>PEAK</div><div>$0.006</div><div>$0.044</div><div>1M INPUT TOKENS</div><div>(CACHE MISS)</div><div>OFF-PEAK</div>\
          <div>$0.15</div><div>$0.66</div><div>PEAK</div><div>$0.3</div><div>$1.32</div><div>1M OUTPUT TOKENS</div><div>OFF-PEAK</div>\
          <div>$0.6</div><div>$1.98</div><div>PEAK</div><div>$1.2</div><div>$3.96</div><div>Concurrency Limit</div><div>$9</div>";
        let p = parse_pricing(page, 1).expect("parse");
        assert_eq!((p.hit, p.miss, p.out), ((0.003, 0.006), (0.15, 0.30), (0.6, 1.2)));
        assert_eq!(p.model, "deepseek-flash");
    }

    #[test]
    fn latency_percentiles() {
        for ms in [1000, 2000, 3000, 4000, 10_000] { record_request(ms, true); }
        record_request(500, false);
        let l = latency_report(60);
        assert!(l["ok"].as_u64().unwrap() >= 5 && l["err"].as_u64().unwrap() >= 1);
        assert!(l["p50_ms"].as_u64().unwrap() >= 1000 && l["p90_ms"].as_u64().unwrap() <= 10_000);
    }

    #[test]
    fn cost_math() {
        let p = Pricing::fallback();
        let b = Bucket { req: 1, miss: 1_000_000, hit: 1_000_000, out: 1_000_000 };
        assert!((cost(&b, &p, false) - (0.15 + 0.003 + 0.6)).abs() < 1e-9);
        assert!((cost(&b, &p, true) - (0.30 + 0.006 + 1.2)).abs() < 1e-9);
    }
}
