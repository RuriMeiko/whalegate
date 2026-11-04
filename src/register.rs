// Đăng ký acc DeepSeek KHÔNG cần trình duyệt: gửi OTP email + register.
//
// Wire format đảo từ main.d1d163b7e4.js (web 2.7.0):
//   - Body của create_email_verification_code / register là JSON mã hoá:
//     AES-128-CBC (key 16B + IV 16B random, PKCS7) — IV KHÔNG kèm trong body.
//     x-request-encryption-key = base64( RSA-OAEP-SHA256(pubkey "production", key || iv) ).
//   - x-ds-guest-pow-response = base64({"salt","answer"}) — guest PoW dùng cùng
//     DeepSeekHashV1 với chat (nonce ∈ [0, difficulty), prefix salt_expireAt_),
//     đúng như header thật web đã gửi (diff 20 → answer 11; diff 160000 → answer 102394).
//   - Header bộ x-client-* y hệt web; cookie ds_session_id lấy từ /favicon.ico như acc login.
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use cbc::cipher::{block_padding::Pkcs7, KeyIvInit};
use rsa::pkcs8::DecodePublicKey;
use rsa::rand_core::RngCore;
use rsa::RsaPublicKey;
use sha2::Sha256;

/// Public key "production" của web (eL.production trong bundle).
const PUB_B64: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0RLAhx1TOJhAQBHS1LHXlnkfOg86O+uhsmKWNNkmugVrRqO6cl13SnjBs2otRYZGJd50xpBG4We2i6/oCJCwxA/49YZfIwaeDmTx+WBIN7qrwhwP7vGCi6Osesmn/Ddgdye6mifYkRpRMonQjGY6VPaUMTw1jjOAIKph5YZn0bDVmGLvs5eH6CoQYHM5/Z1Nfhj36VL28eZi3elJHoz/L6Tzi2wY1rCYqYHfepq3Ysy8y5Y5BeFJXN60RYLlbs+MRC+2kyJqcfApUnn1O/W2om9dhxc6Rc5dqas1YB4X4CWCttrKk/suhpIx9zpQY9kqtotbe5zweYaKv0rCVeqx8wIDAQAB";

const API: &str = "https://chat.deepseek.com/api/v0";
const ORIGIN: &str = "https://chat.deepseek.com";

type AesCbcEnc = cbc::Encryptor<aes::Aes128>;

/// Trạng thái 2 pha (gửi OTP → nhập OTP): giữ "máy" + cookie giữa hai lần chạy CLI.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct RegState {
    email: String,
    did: String,
    fp_seed: String,
    cookies: BTreeMap<String, String>,
    /// device_id thật từ SMSdk.getDeviceId() (Shumei fingerprint) — blob base64 dài, KHÔNG phải UUID
    device_blob: Option<String>,
    /// token user nếu register thành công
    token: Option<String>,
    ds_uid: Option<String>,
}

fn state_path() -> std::path::PathBuf {
    std::path::Path::new(&crate::server::data_dir()).join("register_state.json")
}

fn load_state() -> RegState {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(s: &RegState) -> Result<()> {
    let p = state_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, serde_json::to_vec_pretty(s)?)?;
    Ok(())
}

struct Session {
    http: wreq::Client,
    jar: std::sync::Arc<wreq::cookie::Jar>,
    state: RegState,
}

/// Dựng client với vân tay Chrome thật (cùng cơ chế DsClient): emulation wreq đặt
/// TLS/HTTP2/UA/sec-ch-ua, jar giữ ds_session_id + cookie WAF.
fn build_session(mut st: RegState) -> Result<Session> {
    if st.fp_seed.is_empty() {
        st.fp_seed = uuid::Uuid::new_v4().to_string();
    }
    if st.did.is_empty() {
        st.did = uuid::Uuid::new_v4().to_string();
    }
    let fp = crate::fingerprint::Fingerprint::for_token(&st.fp_seed);
    let jar = std::sync::Arc::new(wreq::cookie::Jar::default());
    let mut builder = wreq::Client::builder()
        .emulation(fp.emulation())
        .cookie_provider(jar.clone())
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30));
    if let Some(u) = crate::egress::proxy_url() { builder = builder.proxy(wreq::Proxy::all(&u)?); }
    let client = builder.build()?;
    // nạp cookie đã lưu từ pha trước
    let url: wreq::Uri = format!("{}/", ORIGIN).parse().expect("static url");
    for (k, v) in &st.cookies {
        jar.add(format!("{}={}; Path=/; Secure", k, v), &url);
    }
    Ok(Session { http: client, jar, state: st })
}

impl Session {
    /// Cookie jar hiện tại (gồm cả Set-Cookie server vừa đặt) để lưu lại state.
    fn snapshot_cookies(&self) -> BTreeMap<String, String> {
        self.jar.get_all().map(|c| (c.name().to_string(), c.value().to_string())).collect()
    }

    /// ds_session_id như trình duyệt: GET /favicon.ico ngoài /api — server web đặt cookie ở response này.
    async fn ensure_session_cookie(&mut self) -> Result<()> {
        if self.state.cookies.contains_key("ds_session_id") {
            return Ok(());
        }
        let resp = self.http.get(format!("{}/favicon.ico", ORIGIN))
            .header("accept", "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8")
            .header("referer", format!("{}/", ORIGIN))
            .header("sec-fetch-dest", "image")
            .header("sec-fetch-mode", "no-cors")
            .header("sec-fetch-site", "same-origin")
            .send().await?;
        let waf = resp.headers().get("x-amzn-waf-action").and_then(|v| v.to_str().ok()).map(str::to_string);
        let status = resp.status();
        let _ = resp.bytes().await;
        self.state.cookies = self.snapshot_cookies();
        if self.state.cookies.contains_key("ds_session_id") {
            crate::elog!("[register] ds_session_id ok ({} cookie)", self.state.cookies.len());
            return Ok(());
        }
        anyhow::bail!(match waf {
            Some(a) => format!("AWS WAF đòi {} khi lấy ds_session_id (favicon -> {})", a, status),
            None => format!("DS không đặt ds_session_id (favicon -> {})", status),
        });
    }

    fn base_headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("origin", ORIGIN.into()),
            ("referer", format!("{}/sign_up", ORIGIN)),
            ("accept", "*/*".into()),
            ("accept-language", "en-US,en;q=0.9".into()),
            ("sec-fetch-dest", "empty".into()),
            ("sec-fetch-mode", "cors".into()),
            ("sec-fetch-site", "same-origin".into()),
            ("x-device-id", self.state.did.clone()),
            ("x-device-model", String::new()),
            ("x-client-bundle-id", "com.deepseek.chat".into()),
            ("x-client-platform", "web".into()),
            ("x-client-version", "2.7.0".into()),
            ("x-client-locale", "en_US".into()),
            ("x-client-timezone-offset", "25200".into()),
        ]
    }

    async fn post_json(&self, path: &str, body: &Value) -> Result<(u16, Value)> {
        let url = format!("{}{}", API, path);
        let mut rb = self.http.post(&url).json(body);
        for (k, v) in self.base_headers() {
            rb = rb.header(k, v);
        }
        let resp = rb.send().await?;
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.with_context(|| format!("{path} -> {status} (body không phải JSON)"))?;
        Ok((status, v))
    }

    /// Challenge + giải PoW cho endpoint guest (cùng thuật toán pow.rs của chat).
    async fn guest_pow(&self, target_path: &str) -> Result<String> {
        // server nhận path NGẮN dạng "/v0/users/..." (không tiền tố /api) như response challenge chỉ ra
        let short = target_path.trim_start_matches("/api");
        let (_, v) = self.post_json("/users/create_guest_challenge", &json!({ "target_path": short })).await?;
        let ch = &v["data"]["biz_data"]["guest_challenge"];
        if ch.is_null() {
            anyhow::bail!("create_guest_challenge trả: {}", serde_json::to_string(&v).unwrap_or_default());
        }
        let (challenge, salt, expire_at, difficulty) = (
            ch["challenge"].as_str().context("no challenge")?.to_string(),
            ch["salt"].as_str().context("no salt")?.to_string(),
            ch["expire_at"].as_u64().context("no expire_at")?,
            ch["difficulty"].as_u64().context("no difficulty")?,
        );
        let t0 = std::time::Instant::now();
        let answer = {
            let challenge = challenge.clone();
            let salt = salt.clone();
            tokio::task::spawn_blocking(move || crate::pow::solve_pow(&challenge, &salt, expire_at, difficulty))
                .await?
                .context("PoW giải không ra trong [0, difficulty)")?
        };
        crate::elog!("[register] pow difficulty={} answer={} solve_ms={}", difficulty, answer, t0.elapsed().as_millis());
        // web chỉ gửi {"salt","answer"} — mirror đúng
        let payload = json!({ "salt": salt, "answer": answer });
        Ok(base64::engine::general_purpose::STANDARD.encode(payload.to_string()))
    }
}

/// Mã hoá body như web: AES-128-CBC(key,iv) + RSA-OAEP(key||iv) → (ciphertext, encryption-key b64).
fn encrypt_body(obj: &Value) -> Result<(Vec<u8>, String)> {
    encrypt_bytes(obj.to_string().as_bytes())
}

/// Phần mã hoá thô (telemetry nén gzip trước rồi mới vào đây).
pub(crate) fn encrypt_bytes(msg: &[u8]) -> Result<(Vec<u8>, String)> {
    encrypt_bytes_with(msg, PUB_B64)
}

/// Khóa công khai của telemetry (cặp "production" thứ nhất trong main.js; register dùng cặp thứ hai ở trên).
pub(crate) const TELEMETRY_PUB_B64: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAo2wegFXo1m3LGL5mWHXe3s2+tuxutbxwRQRO3RJO6Xjengngpkqtq0CfkZVdQyMRIR+9nU5xr26swsEbnwG9keuW4ZF8iAPfTobFT5Dxo0Pkya2Q6i+3jBnnC+MdWlSWuW2pJIX0O+78kmB6ZtXxnf/KmUBa9s6ufFcisgnH1tdpbjwRvzcUz4+zDIneEL7fZlB+nzo258F/oYfqECPRRJRLgDzFYfDd5K22K3grhSTpIvdaYAUe4gyIz/DHLuo+fIeEeGeTcy2hp4pqaT6ctlh7iHDbcSXvy5wqmVwI4M8atyLcl0/osb7JErbu/nuTc8okGCKzt3D8RTwHa/Z2BQIDAQAB";

pub(crate) fn encrypt_bytes_with(msg: &[u8], pub_b64: &str) -> Result<(Vec<u8>, String)> {
    use cbc::cipher::BlockModeEncrypt as _;
    let key: [u8; 16] = rand_bytes()?;
    let iv: [u8; 16] = rand_bytes()?;
    let buf = msg.to_vec();
    let mut out = vec![0u8; msg.len() + 16]; // đủ chỗ cho padding
    let enc = AesCbcEnc::new_from_slices(&key, &iv).expect("key/iv 16B");
    let ct = enc.encrypt_padded_b2b::<Pkcs7>(&buf, &mut out)
        .map_err(|e| anyhow::anyhow!("AES pad: {e}"))?;
    let pubk = RsaPublicKey::from_public_key_der(&base64::engine::general_purpose::STANDARD.decode(pub_b64)?)
        .context("parse RSA public key")?;
    let wrapped = pubk.encrypt(
        &mut rsa::rand_core::OsRng,
        rsa::Oaep::new_with_mgf_hash::<Sha256, Sha256>(),
        &[key.as_slice(), iv.as_slice()].concat(),
    )?;
    Ok((ct.to_vec(), base64::engine::general_purpose::STANDARD.encode(wrapped)))
}

fn rand_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    rsa::rand_core::OsRng.fill_bytes(&mut b);
    Ok(b)
}

/// POST endpoint guest có mã hoá body + PoW: returns parsed JSON.
async fn encrypted_guest_post(sess: &Session, path: &str, payload: &Value) -> Result<(u16, Value)> {
    let pow = sess.guest_pow(&format!("/api/v0/users/{path}")).await?;
    let (body, ek) = encrypt_body(payload)?;
    let url = format!("{}/users/{path}", API);
    let mut rb = sess.http.post(&url).body(body);
    for (k, v) in sess.base_headers() {
        rb = rb.header(k, v);
    }
    rb = rb.header("x-ds-guest-pow-response", pow)
        .header("x-request-encryption-key", ek);
    let resp = rb.send().await?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.with_context(|| format!("{path} -> {status} (body không phải JSON)"))?;
    Ok((status, v))
}

fn show(step: &str, v: &Value) {
    println!("[{step}] {}", serde_json::to_string(v).unwrap_or_default());
}

/// Pha 1: gửi OTP email.
///
/// `rid`/`region` là output của SDK Shumei (SMSdk) mà web nhét vào
/// `shumei_verification`. Chúng chỉ sinh được trong một phiên Chrome thật —
/// xem `docs/register.md` để lấy bằng DevTools (Network → create_email_verification_code
/// → payload đã giải mã). Không có chúng server trả biz_code 2 RECAPTCHA_VERIFY_FAILED.
async fn cmd_send_otp(email: &str, device_id: &str, rid: &str, region: &str) -> Result<()> {
    // rid/region phải đi cùng nhau: web chỉ gửi object khi SDK trả về đủ cặp. Một rid trần với region rỗng
    // (hay ngược lại) là payload hỏng — server trả biz_code 2 chứ không nói thiếu field nào.
    if rid.is_empty() && !region.is_empty() {
        anyhow::bail!("có region={:?} nhưng thiếu rid — shumei_verification phải có rid", region);
    }
    if !rid.is_empty() && region.is_empty() {
        crate::elog!("[register] shumei_verification không có region — gửi chuỗi rỗng (web cũng gửi vậy khi SDK không trả region)");
    }
    let mut sess = build_session(RegState { email: email.to_string(), ..Default::default() })?;
    sess.state.device_blob = Some(device_id.to_string());
    sess.ensure_session_cookie().await?;
    // shape y hệt bundle 2.7.0: shumei_verification luôn có mặt (null nếu rỗng)
    let shumei = if rid.is_empty() {
        Value::Null
    } else {
        json!({ "rid": rid, "region": region })
    };
    let payload = json!({
        "email": email,
        "turnstile_token": "",
        "locale": "en_US",
        "shumei_verification": shumei,
        "hcaptcha_token": Value::Null,
        "device_id": sess.state.device_blob.clone().unwrap_or_default(),
        "scenario": "register",
    });
    let (status, v) = encrypted_guest_post(&sess, "create_email_verification_code", &payload).await?;
    show("create_email_verification_code", &v);
    sess.state.cookies = sess.snapshot_cookies();
    let biz = v["data"]["biz_code"].as_i64().unwrap_or(-1);
    if status == 200 && biz == 0 {
        save_state(&sess.state)?;
        println!("OK — OTP đã gửi tới {email}. Chạy lại: whalegate register verify <otp> <password>");
    } else {
        anyhow::bail!("gửi OTP thất bại: HTTP {status} biz_code={biz} (server trả ở trên)");
    }
    Ok(())
}

/// Pha 2: register bằng OTP + password, thành công thì in token và gợi ý thêm vào store.
async fn cmd_verify(otp: &str, password: &str) -> Result<()> {
    let st = load_state();
    if st.email.is_empty() {
        anyhow::bail!("chưa có state gửi OTP — chạy 'whalegate register send-otp <email>' trước");
    }
    let sess = build_session(st)?;
    let payload = json!({
        "locale": "en_US",
        "region": null,
        "payload": {
            "email": sess.state.email,
            "email_verification_code": otp,
            "password": password,
        },
        "device_id": sess.state.did,
        "os": "web",
    });
    let (status, v) = encrypted_guest_post(&sess, "register", &payload).await?;
    show("register", &v);
    let user = &v["data"]["biz_data"]["user"];
    let token = user["token"].as_str().map(str::to_string);
    match token {
        Some(tok) => {
            let mut s2 = sess.state.clone();
            s2.token = Some(tok.clone());
            s2.ds_uid = user["id"].as_str().map(str::to_string);
            s2.cookies = sess.snapshot_cookies();
            save_state(&s2)?;
            println!("REGISTER OK");
            println!("uid:   {}", s2.ds_uid.clone().unwrap_or_default());
            println!("token: {tok}");
            println!("(đã lưu ở {} — thêm vào store bằng 'whalegate register add')", state_path().display());
        }
        None => anyhow::bail!("register thất bại: HTTP {status} — server trả ở trên (OTP sai/hết hạn?)"),
    }
    Ok(())
}

/// Pha 3 (tuỳ chọn): đẩy acc vừa register vào accounts.json của whalegate.
async fn cmd_add(label: Option<&str>) -> Result<()> {
    let st = load_state();
    let Some(token) = st.token.clone() else {
        anyhow::bail!("chưa register xong — chạy verify trước");
    };
    let mut store = crate::server::load_store().await;
    if store.accounts.iter().any(|a| a.ds_uid == st.ds_uid) {
        anyhow::bail!("acc (ds_uid={}) đã có trong store", st.ds_uid.clone().unwrap_or_default());
    }
    let acc = crate::server::Account {
        id: uuid::Uuid::new_v4().to_string(),
        label: label.map(str::to_string).unwrap_or_else(|| format!("reg-{}", &st.email.split('@').next().unwrap_or("acc"))),
        token,
        cookies: st.cookies.clone(),
        added_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        last_verified: None,
        last_status: None,
        thread_session_id: None,
        thread_parent_message_id: None,
        thread_uses: 0,
        stats: Default::default(),
        pool_enabled: true,
        ds_uid: st.ds_uid.clone(),
        fp_seed: Some(st.fp_seed.clone()),
        device_id: Some(st.did.clone()),
        device_refreshed_at: None,
        browser: None,
        auth_dead_at: None,
        login_method: Some("register".into()),
    };
    store.accounts.push(acc);
    crate::server::write_store_atomic_pub(&store).await?;
    println!("đã thêm acc vào store ({} acc tổng)", store.accounts.len());
    Ok(())
}

// ---------- capture: bắt shumei_verification + device_id từ Chrome thật ----------

/// Bản ghi đã bắt được từ một phiên đăng ký thật.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug)]
pub struct ShumeiCapture {
    /// base64 blob ~89 ký tự — SMSdk.getDeviceId(), cũng là detail.deviceId của response
    pub device_id: String,
    /// id server Shumei cấp, dùng trong shumei_verification
    pub rid: String,
    pub region: String,
    /// thời điểm bắt (ms) — rid gắn với challenge nên hết hạn nhanh
    pub captured_at_ms: u128,
    /// URL trang lúc bắt, để phân biệt sign_up với sign_in
    pub page_url: String,
}

fn capture_path() -> std::path::PathBuf {
    std::path::Path::new(&crate::server::data_dir()).join("shumei_capture.json")
}

pub fn load_capture() -> Option<ShumeiCapture> {
    let s = std::fs::read_to_string(capture_path()).ok()?;
    serde_json::from_str(&s).ok()
}

fn save_capture(c: &ShumeiCapture) -> Result<()> {
    let p = capture_path();
    if let Some(dir) = p.parent() { std::fs::create_dir_all(dir)?; }
    std::fs::write(&p, serde_json::to_vec_pretty(c)?)?;
    Ok(())
}

/// Đọc bộ tham số đăng ký từ body JSON của `create_email_verification_code` (bản gốc, trước khi mã hoá).
///
/// Đây là nguồn ĐÚNG của cả ba giá trị:
///   - `shumei_verification.rid` / `shumei_verification.region` — do widget captcha Shumei trả về
///     sau khi người dùng giải (`showShumeiCaptcha().onSettled` → `{pass, rid}`).
///   - `device_id` — `SMSdk.getDeviceId()`, chuỗi `'B'+base64`.
///
/// Response `deviceprofile/v4` KHÔNG chứa `rid` (chỉ có `deviceId` + `requestId`), nên không
/// dùng nó để lấy `rid` được — nhầm chỗ này là lý do bản capture cũ luôn thất bại.
fn parse_reg_params(v: &Value) -> Option<(String, String, String)> {
    let sv = &v["shumei_verification"];
    let rid = sv["rid"].as_str().map(str::trim).unwrap_or("");
    if rid.is_empty() {
        return None;
    }
    let region = sv["region"]
        .as_str()
        .map(str::to_string)
        .or_else(|| sv["region"].as_u64().map(|n| n.to_string()))
        .unwrap_or_default();
    let device = v["device_id"].as_str().unwrap_or("").trim().to_string();
    if device.is_empty() {
        return None;
    }
    Some((device, rid.to_string(), region))
}

/// Bắt bộ tham số đăng ký từ một tab đang mở, đúng nguồn thật của từng giá trị:
///
/// - `device_id` = `SMSdk.getDeviceId()` — chuỗi `'B'+base64` (~89 ký tự), **không phải** cookie
///   `smidV2` (cookie là timestamp hex). Gửi nhầm cookie thì server trả 200 + `biz_code 2`,
///   không phải 422, nên rất dễ tưởng là đúng.
/// - `rid`/`region` = `shumei_verification` trong **request gửi đi**
///   `create_email_verification_code`. Response `deviceprofile/v4` **không** chứa `rid`: nó chỉ
///   cấp `deviceId`, còn `rid` do widget captcha Shumei trả về sau khi người dùng giải.
///
/// Vì vậy hàm này nghe `Network.requestWillBeSent` (request gửi đi) thay vì response của Shumei.
/// Body request đã mã hoá nên không đọc được nội dung — nhưng `rid` nằm ở đó nên phải lấy từ
/// chính trang: hook `XMLHttpRequest.send`/`fetch` trước khi bấm, ghi lại body JSON gốc vào
/// `sessionStorage`, rồi đọc ra. Cách này không phụ thuộc việc body có bị mã hoá trên đường truyền.
async fn watch_shumei_in_tab(ws_url: String, timeout_secs: u64) -> Result<ShumeiCapture> {
    let mut conn = crate::chrome_capture::CdpConn::connect(&ws_url).await?;
    // Hook phải cài TRƯỚC khi người dùng bấm "Gửi mã": giữ nguyên văn body JSON của request
    // đăng ký, vì trên đường truyền nó đã bị AES+RSA nên CDP chỉ thấy byte rác.
    let hook = r#"(() => {
        if (window.__wgRegHook) return 'already';
        window.__wgRegHook = true;
        const WANT = '/create_email_verification_code';
        const record = (url, body) => {
            try {
                if (String(url).indexOf(WANT) < 0 || typeof body !== 'string') return;
                const v = JSON.parse(body);
                const sv = v && v.shumei_verification;
                sessionStorage.setItem('__wg_reg_params', JSON.stringify({
                    rid: (sv && sv.rid) || '',
                    region: (sv && sv.region) || '',
                    device_id: v.device_id || '',
                    turnstile: !!v.turnstile_token,
                }));
            } catch (e) {}
        };
        const OS = XMLHttpRequest.prototype.send;
        XMLHttpRequest.prototype.send = function (b) { try { record(this.__u || '', b); } catch (e) {} return OS.apply(this, arguments); };
        const OO = XMLHttpRequest.prototype.open;
        XMLHttpRequest.prototype.open = function (m, u) { this.__u = String(u); return OO.apply(this, arguments); };
        const OF = window.fetch;
        window.fetch = function (i, o) {
            try { record(typeof i === 'string' ? i : (i && i.url) || '', o && o.body); } catch (e) {}
            return OF.apply(this, arguments);
        };
        return 'installed';
    })()"#;
    conn.call("Runtime.evaluate", json!({"expression": hook, "returnByValue": true})).await?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("hết {timeout_secs}s mà chưa thấy request đăng ký mang shumei_verification");
        }
        // device_id lấy thẳng từ SDK (đúng giá trị client gửi), rid/region từ body đã hook
        let probe = r#"(() => {
            let dev = '';
            try { if (window.SMSdk && window.SMSdk.getDeviceId) dev = window.SMSdk.getDeviceId() || ''; } catch (e) {}
            const raw = sessionStorage.getItem('__wg_reg_params');
            let p = null;
            try { p = raw ? JSON.parse(raw) : null; } catch (e) {}
            return JSON.stringify({device: dev, params: p, url: location.href});
        })()"#;
        let res = conn.call("Runtime.evaluate", json!({"expression": probe, "returnByValue": true})).await;
        if let Ok(r) = res {
            if let Some(text) = r["result"]["value"].as_str() {
                if let Ok(v) = serde_json::from_str::<Value>(text) {
                    // body gốc đã hook nằm trong sessionStorage; device_id lấy từ SDK
                    if let Some((device_id, rid, region)) = v["params"].as_object().and_then(|_| {
                        let mut body = v["params"].clone();
                        body["device_id"] = v["device"].clone();
                        parse_reg_params(&body)
                    }) {
                        let cap = ShumeiCapture {
                            device_id, rid, region,
                            captured_at_ms: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
                            page_url: v["url"].as_str().unwrap_or("").to_string(),
                        };
                        save_capture(&cap)?;
                        return Ok(cap);
                    }
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Mở Chrome thật của máy này ở trang đăng ký, chờ bạn tự bấm "Gửi mã", bắt lấy
/// deviceprofile/v4 response, rồi in ra lệnh send-otp đã điền sẵn.
///
/// Không dùng `launch_account_chrome` vì hàm đó khoá cổng CDP login và áp vân tay
/// giả — ở đây cần đúng trình duyệt thật của máy, đúng IP, đúng profile người dùng,
/// nếu không Shumei chấm điểm fingerprint và trả rid không dùng được.
async fn cmd_capture(email: Option<&str>) -> Result<()> {
    let url = format!("{}/sign_up", ORIGIN);
    let bin = crate::chrome_capture::find_chrome_binary()?;
    let port = crate::chrome_capture::pick_cdp_port()?;
    // Profile riêng, không đụng profile Chrome đang chạy của bạn (Chrome từ chối mở
    // --user-data-dir đã bị process khác giữ).
    let profile = std::path::Path::new(&crate::server::data_dir()).join("capture-profile");
    std::fs::create_dir_all(&profile)?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg(&url)
        .args(crate::egress::chrome_args())
        .stdout(std::process::Stdio::null());
    // stderr vào file: Chrome chết lúc khởi động (thiếu màn hình, profile bị khoá) thì lý do nằm ở đây
    let err_log = profile.join("chrome-stderr.log");
    cmd.stderr(std::fs::File::create(&err_log).map(std::process::Stdio::from).unwrap_or_else(|_| std::process::Stdio::null()));
    for (k, v) in crate::chrome_capture::gui_env() { cmd.env(k, v); }
    let mut child = cmd.spawn().context(format!("không spawn được Chrome ({bin})"))?;

    let result = async {
        // chờ CDP lên (Chrome mới mở mất 1–3s)
        let mut ws = None;
        for _ in 0..40 {
            if let Ok(pages) = crate::chrome_capture::list_pages(port).await {
                if let Some(u) = pages.iter()
                    .find(|p| p["type"] == "page" && p["url"].as_str().map(|s| s.contains("deepseek.com")).unwrap_or(false))
                    .and_then(|p| p["webSocketDebuggerUrl"].as_str())
                {
                    ws = Some(u.to_string());
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let ws = match ws {
            Some(w) => w,
            None => {
                // Chrome thoát ngay hoặc không mở được cửa sổ: đọc stderr thay vì báo chung chung
                let tail: Vec<String> = std::fs::read_to_string(&err_log).unwrap_or_default()
                    .lines().filter(|l| !l.trim().is_empty()).map(|l| l.to_string()).collect();
                let tail = tail[tail.len().saturating_sub(3)..].join(" | ");
                let why = if tail.is_empty() {
                    format!("CDP :{port} không có tab DeepSeek sau 20s")
                } else {
                    format!("Chrome không mở được trang: {tail}")
                };
                anyhow::bail!("{}", why);
            }
        };

        println!("Chrome đã mở {url}");
        println!("  1. Nhập email{} và bấm \"Gửi mã\" trong cửa sổ đó.", email.map(|e| format!(" ({e})")).unwrap_or_default());
        println!("  2. Đừng đóng cửa sổ — tool đang nghe response deviceprofile/v4 (tối đa 180s).");
        let cap = watch_shumei_in_tab(ws, 180).await?;

        println!();
        println!("Bắt được shumei_verification:");
        println!("  device_id: {}", cap.device_id);
        println!("  rid:       {}", cap.rid);
        println!("  region:    {}", if cap.region.is_empty() { "(trống — server không trả; send-otp sẽ gửi chuỗi rỗng)" } else { &cap.region });
        println!("  lưu tại:   {}", capture_path().display());
        println!();
        println!("Bước tiếp theo:");
        if let Some(e) = email {
            println!("  whalegate register send-otp {e} '{}' '{}' '{}'", cap.device_id, cap.rid, cap.region);
        } else {
            println!("  whalegate register send-otp <email> '{}' '{}' '{}'", cap.device_id, cap.rid, cap.region);
        }
        Ok(())
    }.await;

    // đóng Chrome: Browser.close để nó ghi profile sạch, kill nếu không chịu
    if let Ok(ws) = crate::chrome_capture::browser_ws_url(port).await {
        if let Ok(mut c) = crate::chrome_capture::CdpConn::connect(&ws).await {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), c.call("Browser.close", json!({}))).await;
        }
    }
    for _ in 0..30 {
        if let Ok(Some(_)) = child.try_wait() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

// ---------- login: tự động đăng nhập acc có sẵn ----------

/// Tự đăng nhập bằng email + mật khẩu trong Chrome riêng của một seed mới, rồi lưu acc vào store.
///
/// Dùng `launch_account_chrome` (headed nếu có màn hình, headless nếu không) để có profile + vân tay
/// riêng cho acc; form do `submit_login_form` điền và bấm; token do `watch_login_tab` chờ và xác minh
/// qua `users/current` — cùng đường với luồng login thủ công trên dashboard, chỉ khác là không cần
/// người bấm.
async fn cmd_login(email: &str, password: &str, label: Option<&str>, force_headless: bool) -> Result<()> {
    let seed = crate::fingerprint::new_seed();
    let fp = crate::fingerprint::Fingerprint::from_seed(&seed);
    // Không có màn hình (máy chủ, systemd trước phiên đồ hoạ, sandbox) thì headless — form vẫn chạy
    // vì toàn bộ thao tác đi qua CDP, không cần cửa sổ thật.
    let headless = force_headless || !crate::chrome_capture::has_display();
    crate::elog!("[login] {} — {} seed={} headless={}", email, fp.user_agent(), &seed[..8.min(seed.len())], headless);
    if std::env::var("WG_LOGIN_DEBUG").is_ok() {
        eprintln!("[debug] data_dir={} profile={} chrome={:?}",
            crate::server::data_dir(),
            crate::chrome_capture::account_profile_dir(&seed),
            crate::chrome_capture::find_chrome_binary());
    }
    println!("mở Chrome{} cho acc mới…", if headless { " (headless)" } else { "" });
    let (browser, ws_url) = crate::chrome_capture::launch_account_chrome(&fp, "https://chat.deepseek.com/sign_in", headless)
        .await
        .context("không mở được Chrome")?;

    let result = async {
        // Trang sign_in cần vài giây nạp JS; submit_login_form tự poll tới khi form sẵn sàng.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        // Hook phải cài TRƯỚC khi bấm: bắt nguyên văn biz_code/biz_msg của /users/login, vì toast
        // lỗi trên trang tự tắt sau vài giây nên đọc DOM lúc timeout thường không còn gì.
        let hooked = crate::chrome_capture::install_login_response_hook(&ws_url).await.is_ok();
        println!("điền form và bấm Đăng nhập…");
        if let Err(e) = crate::chrome_capture::submit_login_form(&ws_url, email, password).await {
            // Trang có thể chưa kịp render: thử lại một lần sau 3s thay vì bỏ cuộc
            crate::elog!("[login] lần 1 lỗi: {:#} — thử lại", e);
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            crate::chrome_capture::submit_login_form(&ws_url, email, password).await?;
        }
        match crate::chrome_capture::watch_login_tab(ws_url.clone(), 90, browser.port, &seed).await {
            Ok(s) => Ok(s),
            Err(e) => {
                if hooked {
                    if let Some((code, msg)) = crate::chrome_capture::login_result(&ws_url).await {
                        let why = match (code, msg.as_str()) {
                            (2, _) => "sai email hoặc mật khẩu".to_string(),
                            (10, _) => "tài khoản đã bị khoá".to_string(),
                            (11, _) => "DeepSeek đánh dấu thiết bị này là rủi ro (RISK_DEVICE_DETECTED)".to_string(),
                            (12, _) => "số điện thoại bị hạn chế".to_string(),
                            (13, _) => "cần chữ ký thiết bị (SIGNATURE_REQUIRED)".to_string(),
                            (14, _) => "tài khoản bật xác thực 2 lớp (OTP_REQUIRED) — chưa hỗ trợ".to_string(),
                            (0, _) => "server nhận đăng nhập nhưng không cấp token".to_string(),
                            _ => format!("server từ chối (biz_code {code}{})", if msg.is_empty() { String::new() } else { format!(", {msg}") }),
                        };
                        anyhow::bail!("đăng nhập thất bại: {why}");
                    }
                }
                Err(e)
            }
        }
    }.await;

    browser.close().await;
    let session = result?;
    println!("đăng nhập OK: {} (uid {})", session.email.clone().unwrap_or_else(|| email.to_string()), session.uid.clone().unwrap_or_default());

    // Lưu vào store: dedupe theo token rồi theo uid, giống login_flow của dashboard.
    let mut store = crate::server::load_store().await;
    let now = crate::server::now();
    let existing = store.accounts.iter().position(|a| a.token == session.token)
        .or_else(|| session.uid.as_ref().and_then(|u| store.accounts.iter().position(|a| a.ds_uid.as_deref() == Some(u.as_str()))));
    match existing {
        Some(i) => {
            let a = &mut store.accounts[i];
            a.token = session.token.clone();
            a.cookies = session.cookies.clone();
            a.last_verified = Some(now);
            a.last_status = Some("ok (login)".into());
            a.clear_auth_dead();
            a.fp_seed = session.fp_seed.clone();
            a.device_id = session.device_id.clone();
            a.device_refreshed_at = Some(now);
            a.browser = None;
            a.login_method = Some("chrome".into());
            a.ds_uid = a.ds_uid.clone().or_else(|| session.uid.clone());
            crate::fingerprint::bind(&a.token, a.fp_seed.as_deref());
            crate::fingerprint::bind_device(&a.token, a.device_id.as_deref());
            println!("cập nhật acc có sẵn: {}", a.label);
        }
        None => {
            let acc = crate::server::Account {
                id: uuid::Uuid::new_v4().to_string(),
                label: label.map(str::to_string).unwrap_or_else(|| session.email.clone().unwrap_or_else(|| email.to_string())),
                token: session.token.clone(),
                cookies: session.cookies.clone(),
                added_at: now,
                last_verified: Some(now),
                last_status: Some("ok (login)".into()),
                thread_session_id: None,
                thread_parent_message_id: None,
                thread_uses: 0,
                stats: Default::default(),
                pool_enabled: true,
                ds_uid: session.uid.clone(),
                fp_seed: session.fp_seed.clone(),
                device_id: session.device_id.clone(),
                device_refreshed_at: Some(now),
                browser: None,
                auth_dead_at: None,
                login_method: Some("chrome".into()),
            };
            println!("thêm acc mới: {}", acc.label);
            store.accounts.push(acc);
        }
    }
    crate::server::write_store_atomic_pub(&store).await?;
    println!("đã lưu ({} acc trong store)", store.accounts.len());
    Ok(())
}
/// CLI: `whalegate register <capture [email] | send-otp <email> [<device_blob> <shumei_rid> <region>] | verify <otp> <password> | add [label]>`
///
/// `capture` mở Chrome thật của máy, nghe `POST fp-it-acc.portal101.cn/deviceprofile/v4`, lưu
/// `{device_id, rid, region, captured_at_ms}` vào ~/.whalegate/shumei_capture.json. `send-otp` không có
/// ba tham số thì đọc bản lưu đó (chỉ chấp nhận < 2 phút, vì rid gắn với challenge).
pub async fn run(args: &[String]) -> Result<()> {
    match args.first().map(|s| s.as_str()) {
        // Bắt shumei_verification + device_id thẳng từ Chrome thật của máy này. Không cần DevTools,
        // không cần dán tay — chỉ cần đăng ký trong cửa sổ Chrome vừa mở.
        Some("capture") => {
            let email = args.get(1).map(|s| s.as_str());
            cmd_capture(email).await
        }
        Some("login") => {
            let mut rest: Vec<&str> = Vec::new();
            let mut headless = false;
            let mut label: Option<&str> = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--headless" => headless = true,
                    "--label" => { i += 1; label = args.get(i).map(|s| s.as_str()); }
                    other => rest.push(other),
                }
                i += 1;
            }
            let email = rest.first().copied().context("thiếu email — whalegate login <email> <password> [--headless] [--label <tên>]")?;
            let password = rest.get(1).copied().context("thiếu mật khẩu — whalegate login <email> <password>")?;
            cmd_login(email, password, label, headless).await
        }
        Some("send-otp") => {
            let email = args.get(1).context("thiếu email — whalegate register send-otp <email> [<device_blob> <shumei_rid> <region>]")?;
            // Không truyền tay ba tham số thì dùng bản 'register capture' vừa lưu (nếu còn mới).
            // rid gắn với challenge nên quá ~2 phút là gần như chắc chắn bị từ chối.
            let saved = load_capture().filter(|c| {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                now.saturating_sub(c.captured_at_ms) < 120_000
            });
            let auto = args.get(2).is_none();
            let (blob, rid, region) = if auto {
                let c = saved.clone().context(
                    "không có shumei_verification đã bắt trong 2 phút qua — chạy 'whalegate register capture <email>' trước",
                )?;
                println!("[register] dùng bản capture lúc {} ({}s trước)",
                    c.rid,
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0).saturating_sub(c.captured_at_ms) / 1000);
                (c.device_id, c.rid, c.region)
            } else {
                (args[2].clone(), args[3].clone(), args[4].clone())
            };
            return cmd_send_otp(email, &blob, &rid, &region).await;
        }
        Some("verify") => {
            let otp = args.get(1).context("thiếu otp — whalegate register verify <otp> <password>")?;
            let pw = args.get(2).context("thiếu password")?;
            cmd_verify(otp, pw).await
        }
        Some("add") => cmd_add(args.get(2).map(|s| s.as_str())).await,
        _ => {
            eprintln!("usage: whalegate register login <email> <password> [--headless] [--label <tên>]");
            eprintln!("       whalegate register capture [email]");
            eprintln!("       whalegate register send-otp <email>                       # dùng bản capture < 2 phút");
            eprintln!("       whalegate register send-otp <email> <device_blob> <rid> <region>   # dán tay");
            eprintln!("       whalegate register verify <otp> <password>");
            eprintln!("       whalegate register add [label]");
            Ok(())
        }
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    /// Body thật của create_email_verification_code — nguồn đúng của cả rid lẫn device_id.
    #[test]
    fn parses_registration_body() {
        let v = json!({
            "email": "a@b.com", "locale": "vi", "scenario": "register",
            "device_id": "BW/HWVlly++AeCValSzywzmALQq4KvrNnAtScYol2RNJWFRsnYZ8N3inI+XlR",
            "shumei_verification": {"rid": "r-1", "region": "VN"}
        });
        let (d, r, g) = parse_reg_params(&v).unwrap();
        assert_eq!(r, "r-1");
        assert_eq!(g, "VN");
        assert!(d.starts_with("BW/"));
    }

    #[test]
    fn region_may_be_a_number() {
        let v = json!({"device_id": "Bxx", "shumei_verification": {"rid": "r", "region": 86}});
        assert_eq!(parse_reg_params(&v).unwrap().2, "86");
    }

    /// Thiếu rid thì KHÔNG dùng được: đó chính là lỗi của bản capture cũ (đọc rid từ
    /// response deviceprofile, nơi không hề có rid) nên luôn thất bại.
    #[test]
    fn rejects_missing_rid_or_device() {
        assert!(parse_reg_params(&json!({"device_id": "Bxx", "shumei_verification": {}})).is_none());
        assert!(parse_reg_params(&json!({"device_id": "Bxx"})).is_none());
        assert!(parse_reg_params(&json!({"shumei_verification": {"rid": "r"}})).is_none());
    }

    /// Response deviceprofile/v4: có deviceId + requestId nhưng KHÔNG có rid — chứng minh
    /// vì sao không thể lấy rid từ đây.
    #[test]
    fn deviceprofile_response_has_no_rid() {
        let v = json!({"code": 1100, "detail": {"c": 0, "deviceId": "NpOy5SGVRuogBL5lreWbL3JGoFCHfonf", "t": 0}, "requestId": "1102fcfbbe"});
        assert!(parse_reg_params(&v).is_none(), "deviceprofile không mang shumei_verification");
    }
}
