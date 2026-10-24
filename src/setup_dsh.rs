// `whalegate setup-dsh`: cấu hình dsh (Linux/macOS/Windows) dùng gateway — thay script Python cũ,
// không cần python3/PyYAML. Idempotent; luôn backup `cordis.patch.yml.bak.<giờ>` trước khi sửa.
use anyhow::{bail, Context, Result};
use serde_yaml::{Mapping, Value};
use std::path::PathBuf;

const HEADER: &str = "# Your patch layer for this dsh profile, applied after every bundle layer:\n\
# a top-level YAML array of loader patch entries (id-targeted config\n\
# overrides, disables, and insert lists; `!!js` expressions allowed).\n";

const USAGE: &str = "usage: whalegate setup-dsh [--profile <dir>] [--url <base_url>] [--preset ptc|standard] [--context <tokens>]\n\
  --profile  dsh profile dir (default: $DSH_HOME/profiles/web, else ~/.dsh/profiles/web)\n\
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

pub(crate) fn default_profile() -> PathBuf {
    dsh_home().join("profiles").join("web")
}

pub(crate) fn default_base_url() -> String {
    let port = std::env::var("DS_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(crate::DEFAULT_PORT);
    format!("http://127.0.0.1:{}/v1", port)
}

/// Cửa sổ ngữ cảnh báo cho dsh. dsh tự nén khi lịch sử chạm 80% cửa sổ (đếm token theo cách của nó); việc đó phải
/// xảy ra TRƯỚC khi prompt chạm DS_PROMPT_MAX_CHARS của gateway — vượt ngưỡng đó gateway cắt lịch sử thô (mất ngữ cảnh
/// không có tóm tắt). Tính theo trường hợp xấu nhất ~4 ký tự/token (văn bản tiếng Anh) và chừa 10% cho spec tools/DSML:
/// mặc định 1,2M ký tự → 337K token. Không vượt DS_CONTEXT_TOKENS (giới hạn đo được của web).
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
    let profile = default_profile();
    let file = profile.join("cordis.patch.yml");
    let doc: Option<Value> = std::fs::read_to_string(&file).ok().and_then(|s| serde_yaml::from_str(&s).ok());
    let prov = doc.as_ref()
        .and_then(|d| d.as_sequence())
        .and_then(|s| s.iter().find(|e| e.get("id").and_then(|v| v.as_str()) == Some("llm-pi-ai")))
        .and_then(|e| e.get("config")).and_then(|c| c.get("providers")).and_then(|p| p.get(PROVIDER));
    let base = prov.and_then(|p| p.get("baseURL")).and_then(|b| b.as_str()).map(|s| s.to_string());
    let ctx = prov.and_then(|p| p.get("models")).and_then(|m| m.get(0)).and_then(|m| m.get("contextWindow")).and_then(|c| c.as_u64());
    serde_json::json!({
        "dsh_home": dsh_home().to_string_lossy(),
        "profile": profile.to_string_lossy(),
        "found": profile.is_dir(),
        "configured": base.as_deref() == Some(default_base_url().as_str()),
        "base_url": base,
        "context_window": ctx,
        "expected": {"base_url": default_base_url(), "context_window": safe_context_window()},
    })
}

pub fn run(args: &[String]) -> Result<()> {
    let mut profile: Option<PathBuf> = None;
    let mut o = Opts::defaults();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().with_context(|| format!("{} needs a value\n{}", a, USAGE));
        match a.as_str() {
            "--profile" => profile = Some(PathBuf::from(val()?)),
            "--url" => o.base = val()?,
            "--preset" => o.preset = val()?,
            "--context" => o.ctx = val()?.parse().context("--context must be a number")?,
            "-h" | "--help" => { println!("{}", USAGE); return Ok(()); }
            other => bail!("unknown argument: {}\n{}", other, USAGE),
        }
    }
    let profile = profile.unwrap_or_else(default_profile);
    let r = configure(&profile, &o)?;
    if let Some(b) = &r.backup { println!("backup: {}", b.display()); }
    println!("OK: {}{}", r.file.display(), if r.backup.is_none() && !r.created { " (already up to date)" } else { "" });
    println!("  provider {} → {}  (model deepseek-web, context {} tokens, preset {})", PROVIDER, o.base, o.ctx, o.preset);
    println!("Restart dsh and pick \"DeepSeek Web (whalegate, free)\".");
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

    #[test]
    fn stamp_shape() {
        let s = chrono_stamp();
        assert_eq!(s.len(), 15);
        assert!(s.starts_with("20"));
    }
}
