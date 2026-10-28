// `whalegate setup-dsh`: cấu hình dsh (Linux/macOS/Windows) dùng gateway — thay script Python cũ,
// không cần python3/PyYAML. Idempotent; luôn backup `cordis.patch.yml.bak.<giờ>` trước khi sửa.
use anyhow::{bail, Context, Result};
use serde_yaml::{Mapping, Value};
use std::path::PathBuf;

const HEADER: &str = "# Your patch layer for this dsh profile, applied after every bundle layer:\n\
# a top-level YAML array of loader patch entries (id-targeted config\n\
# overrides, disables, and insert lists; `!!js` expressions allowed).\n";

const USAGE: &str = "usage: whalegate setup-dsh [--all] [--profile <dir>]... [--url <base_url>] [--preset ptc|standard] [--context <tokens>]\n\
  --all      configure every dsh profile on this machine (desktop, web, headless, ...)\n\
  --profile  dsh profile dir; repeatable (default: $DSH_HOME/profiles/web, else ~/.dsh/profiles/web)\n\
  --url      gateway base URL (default: http://127.0.0.1:$DS_PORT/v1, port 8790)\n\
  --preset   agent preset selected for new sessions (default: ptc)\n\
  --context  context window reported to dsh (default: derived from DS_PROMPT_MAX_CHARS so dsh compacts before the gateway trims)";

/// Tên provider trong dsh. "ds-harness" là tên cũ — bị gỡ khi cấu hình lại.
const PROVIDER: &str = "whalegate";
const LEGACY_PROVIDER: &str = "ds-harness";

pub(crate) fn dsh_home() -> PathBuf {
    match std::env::var("DSH_HOME") {
        Ok(h) if !h.trim().is_empty() => PathBuf::from(h),
        _ => PathBuf::from(crate::server::home_dir()).join(".dsh"),
    }
}

/// Tên profile mặc định khi không chỉ định (dsh web).
pub(crate) const DEFAULT_PROFILE_NAME: &str = "web";

pub(crate) fn default_profile() -> PathBuf {
    profile_dir(DEFAULT_PROFILE_NAME)
}

pub(crate) fn profiles_root() -> PathBuf {
    dsh_home().join("profiles")
}

pub(crate) fn profile_dir(name: &str) -> PathBuf {
    profiles_root().join(name)
}

/// Mọi profile dsh đang có trên máy, kèm cờ "đã cấu hình whalegate chưa".
///
/// Trước đây chỉ ghi vào profiles/web, nên trên Windows/macOS người dùng Desktop app
/// (profile "desktop", do Electron quản lý) mở lên vẫn không thấy gateway. Giờ quét hết:
/// một profile được tính là tồn tại khi có package.json hoặc cordis.patch.yml.
pub(crate) fn list_profiles() -> Vec<(String, PathBuf, bool)> {
    let root = profiles_root();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else { return out };
    for e in rd.flatten() {
        let path = e.path();
        if !path.is_dir() { continue; }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n != "node_modules" => n.to_string(),
            _ => continue,
        };
        let initialized = path.join("package.json").is_file() || path.join("cordis.patch.yml").is_file();
        if !initialized { continue; }
        let configured = is_configured(&path);
        out.push((name, path, configured));
    }
    // desktop trước (nền tảng nào cũng có thể có), rồi web, rồi alphabetical
    out.sort_by(|a, b| {
        let rank = |n: &str| match n { "desktop" => 0, "web" => 1, _ => 2 };
        rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Profile này đã trỏ về gateway hiện tại chưa (đọc thẳng patch).
fn is_configured(profile: &std::path::Path) -> bool {
    let Ok(src) = std::fs::read_to_string(profile.join("cordis.patch.yml")) else { return false };
    let Ok(doc) = serde_yaml::from_str::<Value>(&src) else { return false };
    let Some(seq) = doc.as_sequence() else { return false };
    let base = default_base_url();
    seq.iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some("llm-pi-ai"))
        .and_then(|e| e.get("config"))
        .and_then(|c| c.get("providers"))
        .and_then(|p| p.get(PROVIDER))
        .and_then(|p| p.get("baseURL"))
        .and_then(|b| b.as_str())
        == Some(base.as_str())
}

pub(crate) fn default_base_url() -> String {
    let port = std::env::var("DS_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(crate::DEFAULT_PORT);
    format!("http://127.0.0.1:{}/v1", port)
}

/// Cửa sổ ngữ cảnh báo cho dsh. dsh tự nén khi lịch sử chạm 80% cửa sổ (đếm token theo cách của nó); việc đó phải
/// xảy ra TRƯỚC khi prompt chạm DS_PROMPT_MAX_CHARS của gateway — vượt ngưỡng đó gateway cắt lịch sử thô (mất ngữ cảnh
/// không có tóm tắt). Tính theo trường hợp xấu nhất ~4 ký tự/token (văn bản tiếng Anh) và chừa 10% cho spec tools/DSML:
/// mặc định 1,822M ký tự → 512K token. Không vượt DS_CONTEXT_TOKENS (giới hạn đo được của web).
pub(crate) fn safe_context_window() -> u64 {
    let max_chars = crate::settings::uint("DS_PROMPT_MAX_CHARS");
    let by_chars = max_chars * 9 / 10 / 4 * 10 / 8;
    by_chars.min(crate::settings::uint("DS_CONTEXT_TOKENS")) / 1000 * 1000
}

fn key(k: &str) -> Value { Value::String(k.to_string()) }

/// Mục `id` trong mảng patch (tạo mới nếu chưa có) → trả `config` dạng mapping.
fn entry_config<'a>(doc: &'a mut Vec<Value>, id: &str, name: &str) -> &'a mut Mapping {
    let pos = doc.iter().position(|e| e.get("id").and_then(|v| v.as_str()) == Some(id));
    let idx = match pos {
        Some(i) => i,
        None => {
            let mut m = Mapping::new();
            m.insert(key("id"), key(id));
            m.insert(key("name"), key(name));
            m.insert(key("config"), Value::Mapping(Mapping::new()));
            doc.push(Value::Mapping(m));
            doc.len() - 1
        }
    };
    let e = doc[idx].as_mapping_mut().expect("patch entry must be a mapping");
    if !matches!(e.get("config"), Some(Value::Mapping(_))) {
        e.insert(key("config"), Value::Mapping(Mapping::new()));
    }
    e.get_mut("config").and_then(|c| c.as_mapping_mut()).unwrap()
}

/// Key ghi thẳng vào header (không qua apiKeyEnv/.env): pi-ai bắt buộc có credential kể cả khi server không cần,
/// nên gateway không đặt DS_API_KEY thì dùng chuỗi giữ chỗ.
fn provider(base: &str, ctx: u64, api_key: &str) -> Value {
    let v = serde_json::json!({
        "displayName": "whalegate (DeepSeek web)",
        "api": "openai-completions",
        "baseURL": base,
        "headers": {"Authorization": format!("Bearer {}", api_key)},
        "timeoutMs": 600000,
        "compat": {"supportsDeveloperRole": false, "supportsReasoningEffort": true, "supportsUsageInStreaming": true},
        "models": [{
            "id": "deepseek-web", "name": "DeepSeek Web (whalegate, free)",
            "contextWindow": ctx, "maxTokens": 32768, "input": ["text", "image"],
            "reasoningEfforts": {"off": "none", "low": "auto", "max": "max"}
        }]
    });
    serde_yaml::to_value(v).expect("json → yaml")
}

pub(crate) struct Opts {
    pub base: String,
    pub ctx: u64,
    pub preset: String,
    pub api_key: String,
}

impl Opts {
    pub(crate) fn defaults() -> Opts {
        Opts {
            base: default_base_url(),
            ctx: safe_context_window(),
            preset: std::env::var("DSH_PRESET").unwrap_or_else(|_| "ptc".into()),
            api_key: std::env::var("DS_API_KEY").ok().filter(|k| !k.trim().is_empty()).unwrap_or_else(|| "whalegate-local".into()),
        }
    }
}

/// Áp cấu hình vào nội dung patch (thuần, để test được).
pub(crate) fn apply(src: &str, o: &Opts) -> Result<String> {
    let mut doc: Vec<Value> = if src.trim().is_empty() {
        Vec::new()
    } else {
        match serde_yaml::from_str::<Value>(src).context("cordis.patch.yml is not valid YAML")? {
            Value::Sequence(s) => s,
            Value::Null => Vec::new(),
            _ => bail!("cordis.patch.yml must be a YAML array"),
        }
    };
    let providers = entry_config(&mut doc, "llm-pi-ai", "@deepseek-ai/dsh-llm-pi-ai")
        .entry(key("providers")).or_insert_with(|| Value::Mapping(Mapping::new()));
    if !providers.is_mapping() { *providers = Value::Mapping(Mapping::new()); }
    let providers = providers.as_mapping_mut().unwrap();
    providers.remove(LEGACY_PROVIDER);
    providers.insert(key(PROVIDER), provider(&o.base, o.ctx, &o.api_key));

    // web search của dsh mặc định gọi API chính thức (cần DEEPSEEK_API_KEY trả phí; key hỏng/hết tiền thì search lỗi
    // hoặc treo cả lượt) → trỏ về /v1/search của gateway, search bằng acc DeepSeek web. Key literal vì plugin đòi có key.
    let s = entry_config(&mut doc, "web-search-deepseek", "@deepseek-ai/dsh-web-search-deepseek");
    s.insert(key("baseURL"), key(&format!("{}/search", o.base.trim_end_matches('/'))));
    s.insert(key("apiKey"), key(&o.api_key));

    let m = entry_config(&mut doc, "agent-default-model", "@deepseek-ai/dsh-agent-default-model");
    m.insert(key("provider"), key(PROVIDER));
    m.insert(key("model"), key("deepseek-web"));

    // preset chọn sẵn cho phiên mới; `default` (preset dự phòng) giữ nguyên nếu đã có
    let r = entry_config(&mut doc, "agent-preset-registry", "@deepseek-ai/dsh-agent-preset-registry");
    r.entry(key("default")).or_insert_with(|| key("standard"));
    r.insert(key("selectedDefault"), key(&o.preset));

    Ok(format!("{}{}", HEADER, serde_yaml::to_string(&doc)?))
}

/// Kết quả cấu hình (CLI in ra, dashboard trả JSON).
pub(crate) struct Report {
    pub file: PathBuf,
    pub backup: Option<PathBuf>,
    pub created: bool,
}

/// Ghi cấu hình vào profile dsh (backup trước). Dùng chung cho CLI `setup-dsh` và nút trên dashboard.
pub(crate) fn configure(profile: &std::path::Path, o: &Opts) -> Result<Report> {
    if !profile.is_dir() {
        bail!(
            "no dsh profile at {} — install dsh and open it once so it creates the profile, or pass --profile <dir>",
            profile.display()
        );
    }
    let file = profile.join("cordis.patch.yml");
    let src = std::fs::read_to_string(&file).unwrap_or_default();
    let out = apply(&src, o)?;
    if out == src {
        return Ok(Report { file, backup: None, created: false });
    }
    let backup = if file.exists() {
        let bak = profile.join(format!("cordis.patch.yml.bak.{}", chrono_stamp()));
        std::fs::copy(&file, &bak).with_context(|| format!("could not back up {}", file.display()))?;
        Some(bak)
    } else {
        None
    };
    std::fs::write(&file, out).with_context(|| format!("could not write {}", file.display()))?;
    let created = backup.is_none();
    Ok(Report { file, backup, created })
}

/// Trạng thái hiện tại cho dashboard: có dsh không, đã trỏ về gateway này chưa.
pub(crate) fn status() -> serde_json::Value {
    let profiles: Vec<serde_json::Value> = list_profiles().into_iter().map(|(name, path, configured)| {
        serde_json::json!({
            "name": name,
            "dir": path.to_string_lossy(),
            "configured": configured,
        })
    }).collect();
    let any_configured = profiles.iter().any(|p| p["configured"].as_bool() == Some(true));
    serde_json::json!({
        "dsh_home": dsh_home().to_string_lossy(),
        "profiles_root": profiles_root().to_string_lossy(),
        "profiles": profiles,
        // giữ các khoá cũ để client/script đời trước không vỡ
        "profile": default_profile().to_string_lossy(),
        "found": default_profile().is_dir(),
        "configured": any_configured,
        "base_url": if any_configured { Some(default_base_url()) } else { None },
        "expected": {"base_url": default_base_url(), "context_window": safe_context_window()},
    })
}

/// Cấu hình nhiều profile một lượt (dashboard chọn nhiều, hoặc CLI --all).
/// Trả về báo cáo từng profile; một profile lỗi không chặn các profile còn lại.
pub(crate) fn configure_many(profiles: &[PathBuf], o: &Opts) -> Vec<(PathBuf, Result<Report>)> {
    profiles.iter().map(|p| (p.clone(), configure(p, o))).collect()
}

pub fn run(args: &[String]) -> Result<()> {
    // --profile lặp lại được; --all = mọi profile đang có trên máy (desktop, web, headless, ...).
    let mut profiles: Vec<PathBuf> = Vec::new();
    let mut all = false;
    let mut o = Opts::defaults();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().with_context(|| format!("{} needs a value
{}", a, USAGE));
        match a.as_str() {
            "--profile" => profiles.push(PathBuf::from(val()?)),
            "--all" => all = true,
            "--url" => o.base = val()?,
            "--preset" => o.preset = val()?,
            "--context" => o.ctx = val()?.parse().context("--context must be a number")?,
            "-h" | "--help" => { println!("{}", USAGE); return Ok(()); }
            other => bail!("unknown argument: {}
{}", other, USAGE),
        }
    }
    if all {
        for (_, dir, _) in list_profiles() {
            if !profiles.contains(&dir) { profiles.push(dir); }
        }
        if profiles.is_empty() {
            bail!("no dsh profile found under {} — open dsh once so it creates one", profiles_root().display());
        }
    }
    if profiles.is_empty() { profiles.push(default_profile()); }

    let mut failed = 0usize;
    for profile in &profiles {
        match configure(profile, &o) {
            Ok(r) => {
                if let Some(b) = &r.backup { println!("backup: {}", b.display()); }
                println!("OK: {}{}", r.file.display(), if r.backup.is_none() && !r.created { " (already up to date)" } else { "" });
            }
            Err(e) => {
                failed += 1;
                eprintln!("FAIL: {}: {:#}", profile.display(), e);
            }
        }
    }
    println!("  provider {} → {}  (model deepseek-web, context {} tokens, preset {})", PROVIDER, o.base, o.ctx, o.preset);
    println!("Restart dsh (and the Desktop app, if you use it) and pick \"DeepSeek Web (whalegate, free)\".");
    if failed > 0 { bail!("{} of {} profile(s) failed", failed, profiles.len()); }
    Ok(())
}

/// YYYYmmdd-HHMMSS theo UTC (không kéo thêm crate thời gian).
fn chrono_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_and_idempotent() {
        let o = Opts { base: "http://127.0.0.1:8790/v1".into(), ctx: 337000, preset: "ptc".into(), api_key: "whalegate-local".into() };
        let a = apply("", &o).unwrap();
        let b = apply(&a, &o).unwrap();
        assert_eq!(a, b, "chạy lại không được đổi gì");
        assert!(a.contains("selectedDefault: ptc") && a.contains("model: deepseek-web") && a.contains("low: auto"));
        assert!(a.contains("Authorization: Bearer whalegate-local") && !a.contains("apiKeyEnv"), "key ghi thẳng, không qua env");
    }

    #[test]
    fn keeps_other_providers_and_entries() {
        let src = "- id: llm-pi-ai\n  name: x\n  config:\n    providers:\n      other: {baseURL: http://o}\n      ds-harness: {baseURL: http://old}\n- id: ui-settings-general\n  config: {theme: dark}\n- id: agent-preset-registry\n  config: {default: minimal}\n";
        let o = Opts { base: "http://h/v1".into(), ctx: 300000, preset: "standard".into(), api_key: "k".into() };
        let out = apply(src, &o).unwrap();
        let v: Vec<Value> = serde_yaml::from_str(&out).unwrap();
        let prov = &v[0]["config"]["providers"];
        assert_eq!(prov["other"]["baseURL"], "http://o");
        assert!(prov.get("ds-harness").is_none(), "provider tên cũ phải bị gỡ");
        assert_eq!(prov["whalegate"]["baseURL"], "http://h/v1");
        assert_eq!(prov["whalegate"]["headers"]["Authorization"], "Bearer k");
        assert_eq!(prov["whalegate"]["models"][0]["contextWindow"], 300000);
        let search = v.iter().find(|e| e["id"] == "web-search-deepseek").expect("search entry");
        assert_eq!(search["config"]["baseURL"], "http://h/v1/search");
        assert_eq!(search["config"]["apiKey"], "k");
        assert_eq!(v[1]["config"]["theme"], "dark");
        assert_eq!(v[2]["config"]["default"], "minimal");
        assert_eq!(v[2]["config"]["selectedDefault"], "standard");
    }

    #[test]
    fn context_window_fits_prompt_budget() {
        // dsh nén ở 80% cửa sổ; ở 4 ký tự/token vẫn phải dưới 90% trần prompt của gateway
        let w = safe_context_window();
        assert!(w * 8 / 10 * 4 <= crate::settings::uint("DS_PROMPT_MAX_CHARS") * 9 / 10, "{}", w);
        assert!(w <= crate::settings::uint("DS_CONTEXT_TOKENS"));
    }


    /// Mọi profile đã init (có package.json hoặc cordis.patch.yml) phải được liệt kê,
    /// node_modules bị bỏ qua, và "desktop" đứng đầu để UI dễ thấy trên Windows/macOS.
    #[test]
    fn discovers_all_initialized_profiles() {
        let root = std::env::temp_dir().join(format!("whalegate-dsh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profs = root.join("profiles");
        for name in ["web", "desktop", "headless", "fresh", "node_modules"] {
            std::fs::create_dir_all(profs.join(name)).unwrap();
        }
        // web + desktop + headless: đã init; fresh: có thư mục nhưng chưa init
        for name in ["web", "desktop", "headless"] {
            std::fs::write(profs.join(name).join("package.json"), "{}").unwrap();
        }
        std::env::set_var("DSH_HOME", &root);
        let got: Vec<String> = list_profiles().into_iter().map(|(n, _, _)| n).collect();
        std::env::remove_var("DSH_HOME");
        let _ = std::fs::remove_dir_all(&root);
        // desktop trước, rồi web, rồi phần còn lại theo alphabet
        assert_eq!(got, vec!["desktop", "web", "headless"], "{:?}", got);
    }

    /// is_configured chỉ đúng khi baseURL trùng gateway hiện tại.
    #[test]
    fn configured_detection_compares_base_url() {
        let o = Opts { base: "http://127.0.0.1:8790/v1".into(), ctx: 300000, preset: "ptc".into(), api_key: "k".into() };
        let dir = std::env::temp_dir().join(format!("whalegate-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(dir.join("cordis.patch.yml"), apply("", &o).unwrap()).unwrap();
        let base = default_base_url();
        let same = base == "http://127.0.0.1:8790/v1";
        assert_eq!(is_configured(&dir), same, "base mặc định là {}", base);

        // trỏ cổng khác → không tính là đã cấu hình
        let other = Opts { base: "http://127.0.0.1:9999/v1".into(), ..o };
        std::fs::write(dir.join("cordis.patch.yml"), apply("", &other).unwrap()).unwrap();
        assert!(!is_configured(&dir));

        // patch hỏng → false, không panic
        std::fs::write(dir.join("cordis.patch.yml"), "not: [valid").unwrap();
        assert!(!is_configured(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stamp_shape() {
        let s = chrono_stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20"));
    }
}
