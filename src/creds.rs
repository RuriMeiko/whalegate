//! Email + mật khẩu của từng acc (khoá theo seed của Chrome riêng) để điền sẵn vào form đăng nhập lại.
//!
//! Chrome không chịu lưu mật khẩu của trang đăng nhập DeepSeek (SPA gửi fetch, không có bước "Lưu mật khẩu?" để bấm),
//! nên whalegate tự nhớ chính những gì bạn gõ trong cửa sổ đăng nhập của acc. Lưu ở `credentials.json` trong thư mục dữ
//! liệu (quyền 0600 trên Linux/macOS), KHÔNG nằm trong gói sao lưu và không bao giờ ra khỏi máy.
use std::collections::BTreeMap;

fn path() -> std::path::PathBuf {
    std::path::Path::new(&crate::server::data_dir()).join("credentials.json")
}

fn load() -> BTreeMap<String, (String, String)> {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn get(seed: &str) -> Option<(String, String)> {
    load().get(seed).filter(|(e, p)| !e.is_empty() && !p.is_empty()).cloned()
}

pub fn set(seed: &str, email: &str, password: &str) {
    if seed.is_empty() || email.is_empty() || password.is_empty() { return; }
    let mut m = load();
    if m.get(seed) == Some(&(email.to_string(), password.to_string())) { return; }
    m.insert(seed.to_string(), (email.to_string(), password.to_string()));
    let Ok(text) = serde_json::to_string(&m) else { return };
    let p = path();
    if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_err() { return; }
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)); }
    let _ = std::fs::rename(&tmp, &p);
}

pub fn remove(seed: &str) {
    let mut m = load();
    if m.remove(seed).is_some() {
        if let Ok(text) = serde_json::to_string(&m) { let _ = std::fs::write(path(), text); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_forgets_per_seed() {
        let seed = format!("seed-{}", uuid::Uuid::new_v4());
        assert_eq!(get(&seed), None);
        set(&seed, "a@b.c", "p\"w'\\1");
        assert_eq!(get(&seed), Some(("a@b.c".into(), "p\"w'\\1".into())), "ký tự đặc biệt giữ nguyên");
        set(&seed, "", "x"); // thiếu email → bỏ qua, không ghi đè
        assert_eq!(get(&seed).unwrap().1, "p\"w'\\1");
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(path()).unwrap().permissions().mode() & 0o777, 0o600); }
        remove(&seed);
        assert_eq!(get(&seed), None);
    }
}
