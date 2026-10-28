// Cấu hình chạy được: giá trị đổi ngay trên dashboard (lưu settings.json), thứ tự ưu tiên
// override đã lưu → biến môi trường DS_* → mặc định. Khoá nhạy cảm (DS_API_KEY, đường dẫn, cổng) không nằm ở đây.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind { Bool, Int, Choice }

pub struct Def {
    pub key: &'static str,
    pub group: &'static str,
    pub kind: Kind,
    pub default: &'static str,
    pub min: i64,
    pub max: i64,
    pub choices: &'static [&'static str],
    pub unit: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    /// chỉ các mục đã kiểm chứng hiệu lực mới hiện trên dashboard; mục ẩn vẫn đọc từ env/mặc định
    pub show: bool,
    /// false = chỉ có hiệu lực sau khi khởi động lại server
    pub live: bool,
}

const fn b(key: &'static str, group: &'static str, default: &'static str, label: &'static str) -> Def {
    Def { key, group, kind: Kind::Bool, default, min: 0, max: 1, choices: &[], unit: "", label, hint: "", show: false, live: true }
}
const fn n(key: &'static str, group: &'static str, default: &'static str, min: i64, max: i64, unit: &'static str, label: &'static str) -> Def {
    Def { key, group, kind: Kind::Int, default, min, max, choices: &[], unit, label, hint: "", show: false, live: true }
}
const fn restart(mut d: Def) -> Def { d.live = false; d }
/// Hiện trên dashboard kèm nhãn ngắn và gợi ý một dòng.
const fn shown(mut d: Def, label: &'static str, hint: &'static str) -> Def { d.show = true; d.label = label; d.hint = hint; d }

pub static DEFS: &[Def] = &[
    // hội thoại
    shown(b("DS_STATEFUL", "Hội thoại", "1", "Stateful: nối tiếp phiên web, chỉ gửi phần mới (tắt = luôn gửi đủ ngữ cảnh)"), "Stateful", "Nối tiếp phiên web, chỉ gửi phần mới"),
    n("DS_CONV_TTL", "Hội thoại", "1800", 30, 86400, "giây", "Xoá phiên web nhàn rỗi sau"),
    n("DS_CONV_MAX_CHARS", "Hội thoại", "1200000", 10000, 5000000, "ký tự", "Trần ngữ cảnh một phiên stateful"),
    n("DS_SESSION_ROTATE", "Hội thoại", "30", 1, 1000, "lượt", "Đổi phiên dùng chung (không tool) sau"),
    // nhanh
    shown(b("DS_POW_PREFETCH", "Tốc độ", "1", "Dự đoán PoW: tải + giải sẵn cho request kế tiếp"), "Dự đoán PoW", "Tải và giải sẵn cho request kế tiếp"),
    Def { key: "DS_DEFAULT_EFFORT", group: "Tốc độ", kind: Kind::Choice, default: "none", min: 0, max: 0, choices: &["none", "auto", "max"], unit: "", label: "Mức suy nghĩ mặc định", hint: "Khi client không chỉ định. none nhanh · auto nghĩ lượt người dùng · max luôn nghĩ", show: true, live: true },
    shown(n("DS_CONTEXT_TOKENS", "Tốc độ", "512000", 8000, 1000000, "token", "Cửa sổ ngữ cảnh báo cho client (/v1/models)"), "Cửa sổ ngữ cảnh", "Số báo cho client; web nhận ≥512K token"),
    shown(n("DS_PROMPT_MAX_CHARS", "Tốc độ", "1822000", 10000, 5000000, "ký tự", "Vượt thì cắt tool_result cũ rồi bỏ lượt cũ"), "Trần prompt", "Vượt thì cắt tool_result cũ rồi bỏ lượt cũ"),
    n("DS_MAX_IMAGE_MB", "Tốc độ", "20", 1, 100, "MB", "Ảnh tối đa mỗi tấm"),
    // pool
    shown(n("DS_MAX_CONC_PER_ACC", "Pool", "2", 1, 6, "request", "Đồng thời tối đa trên một tài khoản"), "Đồng thời mỗi tài khoản", "Cao hơn 2 dễ bị DeepSeek giới hạn"),
    shown(n("DS_HOURLY_LIMIT", "Pool", "60", 0, 1000, "lượt", "Lượt tối đa mỗi giờ trên một tài khoản (0 = không giới hạn)"), "Lượt/giờ mỗi tài khoản", "Chạm trần thì tài khoản nghỉ tới hết giờ · 0 = tắt"),
    n("DS_QUEUE_WAIT_SECS", "Pool", "120", 1, 1800, "giây", "Chờ tài khoản rảnh tối đa"),
    n("DS_FAIL_COOLDOWN_MAX", "Pool", "600", 15, 7200, "giây", "Nghỉ tối đa khi tài khoản lỗi liên tiếp"),
    n("DS_BURST_LIMIT", "Pool", "6", 1, 100, "lượt", "Giới hạn burst mỗi tài khoản"),
    n("DS_BURST_WINDOW", "Pool", "30", 1, 600, "giây", "Cửa sổ burst"),
    n("DS_COOLDOWN", "Pool", "10", 0, 600, "giây", "Nghỉ sau khi chạm burst"),
    // mạng
    shown(n("DS_TZ_OFFSET", "Mạng", "25200", -43200, 50400, "giây", "Lệch múi giờ gửi cho web và tính ngày (UTC+7 = 25200)"), "Múi giờ", "Giây lệch UTC · +7 = 25200"),
    restart(n("DS_READ_TIMEOUT_SECS", "Mạng", "120", 30, 900, "giây", "Timeout mỗi lần đọc socket (cần restart)")),
    b("DS_DEBUG_SSE", "Mạng", "0", "Ghi SSE thô vào /tmp/ds_debug_sse.log (chỉ để gỡ lỗi)"),
];

fn overrides() -> &'static RwLock<HashMap<String, String>> {
    static O: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
    O.get_or_init(|| RwLock::new(load()))
}

fn file_path() -> std::path::PathBuf {
    std::path::Path::new(&crate::server::creds_dir()).join("settings.json")
}

fn load() -> HashMap<String, String> {
    let m: HashMap<String, String> = std::fs::read_to_string(file_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    // bỏ khoá lạ hoặc giá trị sai kiểu (file sửa tay)
    m.into_iter().filter(|(k, v)| find(k).is_some_and(|d| d.show && validate(d, v).is_ok())).collect()
}

fn find(key: &str) -> Option<&'static Def> { DEFS.iter().find(|d| d.key == key) }

fn validate(d: &Def, v: &str) -> Result<String, String> {
    let v = v.trim();
    match d.kind {
        Kind::Bool => match v { "1" | "true" | "on" => Ok("1".into()), "0" | "false" | "off" => Ok("0".into()), _ => Err("cần bật hoặc tắt".into()) },
        Kind::Int => {
            let x: i64 = v.parse().map_err(|_| "cần là số nguyên".to_string())?;
            if x < d.min || x > d.max { return Err(format!("trong khoảng {}–{}", d.min, d.max)); }
            Ok(x.to_string())
        }
        Kind::Choice => if d.choices.contains(&v) { Ok(v.to_string()) } else { Err(format!("một trong: {}", d.choices.join(", "))) },
    }
}

/// Giá trị hiệu lực dạng chuỗi: override → env → mặc định.
pub fn get(key: &str) -> String {
    if let Some(v) = overrides().read().unwrap_or_else(|e| e.into_inner()).get(key) { return v.clone(); }
    if let Ok(v) = std::env::var(key) { return v; }
    find(key).map(|d| d.default.to_string()).unwrap_or_default()
}

pub fn int(key: &str) -> i64 {
    get(key).parse().ok().or_else(|| find(key).and_then(|d| d.default.parse().ok())).unwrap_or(0)
}
pub fn uint(key: &str) -> u64 { int(key).max(0) as u64 }
pub fn boolean(key: &str) -> bool { !matches!(get(key).as_str(), "0" | "false" | "off" | "") }

/// Đặt giá trị (None = bỏ override, quay về env/mặc định) và ghi đĩa.
pub fn set(key: &str, value: Option<&str>) -> Result<(), String> {
    let d = find(key).filter(|d| d.show).ok_or_else(|| format!("khoá không hợp lệ: {key}"))?;
    {
        let mut m = overrides().write().unwrap_or_else(|e| e.into_inner());
        match value {
            Some(v) => { m.insert(key.to_string(), validate(d, v).map_err(|e| format!("{key}: {e}"))?); }
            None => { m.remove(key); }
        }
    }
    persist();
    Ok(())
}

fn persist() {
    let snap = overrides().read().unwrap_or_else(|e| e.into_inner()).clone();
    let p = file_path();
    let tmp = p.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string_pretty(&snap) {
        if std::fs::write(&tmp, s).is_ok() { let _ = std::fs::rename(&tmp, &p); }
    }
}

pub fn snapshot() -> Value {
    let ov = overrides().read().unwrap_or_else(|e| e.into_inner()).clone();
    let items: Vec<Value> = DEFS.iter().filter(|d| d.show).map(|d| {
        let from_env = std::env::var(d.key).is_ok();
        json!({
            "key": d.key, "group": d.group, "label": d.label, "hint": d.hint, "unit": d.unit, "live": d.live,
            "kind": match d.kind { Kind::Bool => "bool", Kind::Int => "int", Kind::Choice => "choice" },
            "min": d.min, "max": d.max, "choices": d.choices, "default": d.default,
            "value": get(d.key), "overridden": ov.contains_key(d.key), "env": from_env,
        })
    }).collect();
    json!({ "items": items })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_by_kind() {
        let st = find("DS_STATEFUL").unwrap();
        assert_eq!(validate(st, "off").unwrap(), "0");
        assert!(validate(st, "maybe").is_err());
        let ttl = find("DS_CONV_TTL").unwrap();
        assert!(validate(ttl, "10").is_err() && validate(ttl, "abc").is_err());
        assert_eq!(validate(ttl, " 600 ").unwrap(), "600");
        let e = find("DS_DEFAULT_EFFORT").unwrap();
        assert!(validate(e, "auto").is_ok() && validate(e, "high").is_err());
    }

    #[test]
    fn only_verified_settings_are_exposed() {
        let shown: Vec<&str> = DEFS.iter().filter(|d| d.show).map(|d| d.key).collect();
        assert_eq!(shown, ["DS_STATEFUL", "DS_POW_PREFETCH", "DS_DEFAULT_EFFORT", "DS_CONTEXT_TOKENS", "DS_PROMPT_MAX_CHARS", "DS_MAX_CONC_PER_ACC", "DS_HOURLY_LIMIT", "DS_TZ_OFFSET"]);
        assert!(set("DS_BURST_LIMIT", Some("5")).is_err(), "mục ẩn không được đặt qua API");
    }

    #[test]
    fn defaults_are_valid_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for d in DEFS {
            assert!(seen.insert(d.key), "trùng {}", d.key);
            assert!(validate(d, d.default).is_ok(), "mặc định sai: {}", d.key);
        }
    }
}
