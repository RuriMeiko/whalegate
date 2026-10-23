// Client: HTTP calls to chat.deepseek.com API + completion flow
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::{json, Value};

use crate::pow::solve_pow;

pub const DEEPSEEK_API: &str = "https://chat.deepseek.com/api/v0";

/// Event từ stream DS — server push thẳng ra SSE OpenAI
pub enum StreamEv {
    Thinking(String),
    Response(String),
    Ready(String),      // chat_session_id
    MsgId(i64),         // response_message_id
    RawHasContent(bool),// SSE có fragment nào không (rate-limit detect)
    Done(String),       // full SSE text (để parse tool-calls cuối)
}


pub struct DsClient {
    http: reqwest::Client,
    token: String,
    cookies: String,
}

impl DsClient {
    pub fn new(token: String, cookies: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36")
            .build()?;
        Ok(Self { http, token, cookies })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", DEEPSEEK_API, path))
            .header("authorization", format!("Bearer {}", self.token))
            .header("cookie", &self.cookies)
            .header("origin", "https://chat.deepseek.com")
            .header("referer", "https://chat.deepseek.com/")
    }

    pub async fn users_current(&self) -> Result<Value> {
        let v: Value = self
            .req(reqwest::Method::GET, "/users/current")
            .send()
            .await?
            .json()
            .await?;
        Ok(v)
    }

    pub async fn create_session(&self) -> Result<String> {
        let v: Value = self
            .req(reqwest::Method::POST, "/chat_session/create")
            .json(&json!({}))
            .send()
            .await?
            .json()
            .await?;
        Ok(v["data"]["biz_data"]["chat_session"]["id"]
            .as_str()
            .context("no session id")?
            .to_string())
    }

    pub async fn history_messages(&self, session_id: &str) -> Result<Value> {
        let v: Value = self
            .req(reqwest::Method::GET, &format!("/chat/history_messages?chat_session_id={}", session_id))
            .send()
            .await?
            .json()
            .await?;
        Ok(v)
    }

    async fn fetch_pow_challenge(&self) -> Result<Value> {
        let v: Value = self
            .req(reqwest::Method::POST, "/chat/create_pow_challenge")
            .json(&json!({ "target_path": "/api/v0/chat/completion" }))
            .send()
            .await?
            .json()
            .await?;
        Ok(v["data"]["biz_data"]["challenge"].clone())
    }

    /// Full flow: challenge -> solve -> completion (SSE). Returns (session_id, sse, last_message_id).
    /// Khi parent_message_id = Some(n) thì chat TIẾP trong cùng thread (không tạo conversation mới).
    pub async fn chat(
        &self,
        prompt: &str,
        session_id: Option<&str>,
        parent_message_id: Option<i64>,
        thinking: bool,
        search: bool,
    ) -> Result<(String, String, Option<i64>)> {
        let sid = match session_id {
            Some(s) => s.to_string(),
            None => self.create_session().await?,
        };

        let ch = self.fetch_pow_challenge().await?;
        let challenge_hex = ch["challenge"].as_str().context("no challenge")?.to_string();
        let salt = ch["salt"].as_str().context("no salt")?.to_string();
        let expire_at = ch["expire_at"].as_u64().context("no expire_at")?;
        let difficulty = ch["difficulty"].as_u64().context("no difficulty")?;
        let signature = ch["signature"].as_str().context("no signature")?.to_string();

        let t0 = std::time::Instant::now();
        let answer = solve_pow(&challenge_hex, &salt, expire_at, difficulty)
            .context("PoW solve failed within difficulty bound")?;
        let solve_ms = t0.elapsed().as_millis();

        let pow_payload = json!({
            "algorithm": ch["algorithm"],
            "challenge": challenge_hex,
            "salt": salt,
            "answer": answer,
            "signature": signature,
            "target_path": ch["target_path"],
        });
        let pow_header = base64::engine::general_purpose::STANDARD.encode(pow_payload.to_string());

        eprintln!(
            "[pow] difficulty={} answer={} solve_ms={}",
            difficulty, answer, solve_ms
        );

        let resp = self
            .req(reqwest::Method::POST, "/chat/completion")
            .header("x-ds-pow-response", pow_header)
            .json(&json!({
                "chat_session_id": sid,
                "parent_message_id": parent_message_id,
                "ref_file_ids": [],
                "thinking_enabled": thinking,
                "search_enabled": search,
                "prompt": prompt,
            }))
            .send()
            .await?;

        let status = resp.status();
        let full = resp.bytes().await?;
        if !status.is_success() {
            anyhow::bail!("completion {} -> {}", status, String::from_utf8_lossy(&full));
        }
        let sse = String::from_utf8_lossy(&full).to_string();

        // debug: dump toàn bộ SSE raw (env DS_DEBUG_SSE=1)
        if std::env::var("DS_DEBUG_SSE").as_deref() == Ok("1") {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
                .open("/tmp/ds_debug_sse.log") {
                use std::io::Write;
                let _ = writeln!(f, "===== {} =====\n{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), sse);
            }
        }

        // Lấy response_message_id từ event `ready` để lần sau nối tiếp thread
        let mut last_msg_id = None;
        for line in sse.lines() {
            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    if let Some(rid) = v.get("response_message_id").and_then(|x| x.as_i64()) {
                        last_msg_id = Some(rid);
                    }
                }
            }
            // event: ready line chứa data trên dòng kế — regex đơn giản qua header ready
            if line.starts_with("event: ready") {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: {\"request_message_id\"") {
                if let Ok(v) = serde_json::from_str::<Value>(data) {
                    last_msg_id = v.get("response_message_id").and_then(|x| x.as_i64());
                }
            }
        }
        Ok((sid, sse, last_msg_id))
    }

/// Stream real-time: trả receiver event; call non-blocking cho server pump.
pub async fn chat_stream(
    &self,
    prompt: &str,
    session_id: Option<&str>,
    parent_message_id: Option<i64>,
    thinking: bool,
    search: bool,
) -> Result<(String, tokio::sync::mpsc::UnboundedReceiver<StreamEv>)> {
    use tokio::sync::mpsc;
    use futures::StreamExt;

    let sid = match session_id {
        Some(s) => s.to_string(),
        None => self.create_session().await?,
    };

    let ch = self.fetch_pow_challenge().await?;
    let challenge_hex = ch["challenge"].as_str().context("no challenge")?.to_string();
    let salt = ch["salt"].as_str().context("no salt")?.to_string();
    let expire_at = ch["expire_at"].as_u64().context("no expire_at")?;
    let difficulty = ch["difficulty"].as_u64().context("no difficulty")?;
    let signature = ch["signature"].as_str().context("no signature")?.to_string();

    let t0 = std::time::Instant::now();
    let answer = solve_pow(&challenge_hex, &salt, expire_at, difficulty)
        .context("PoW solve failed within difficulty bound")?;
    eprintln!("[pow] difficulty={} answer={} solve_ms={}", difficulty, answer, t0.elapsed().as_millis());

    let pow_payload = json!({
        "algorithm": ch["algorithm"],
        "challenge": challenge_hex,
        "salt": salt,
        "answer": answer,
        "signature": signature,
        "target_path": ch["target_path"],
    });
    let pow_header = base64::engine::general_purpose::STANDARD.encode(pow_payload.to_string());

    let resp = self
        .req(reqwest::Method::POST, "/chat/completion")
        .header("x-ds-pow-response", pow_header)
        .json(&json!({
            "chat_session_id": sid,
            "parent_message_id": parent_message_id,
            "ref_file_ids": [],
            "thinking_enabled": thinking,
            "search_enabled": search,
            "prompt": prompt,
        }))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("completion {} -> {}", status, body);
    }

    let (tx, rx) = mpsc::unbounded_channel::<StreamEv>();
    let sid2 = sid.clone();
    tokio::spawn(async move {
        let mut stream = std::pin::pin!(resp.bytes_stream());
        let mut buf = String::new();
        let mut full = String::new();
        let mut cur_type = String::new();
        let mut has_content = false;
        let mut sent_msg_id = false;

        let t0 = std::time::Instant::now();
        let mut first_byte_ms: Option<u128> = None;
        let mut total_bytes: usize = 0;
        let mut raw: Vec<u8> = Vec::new(); // byte dư của ký tự UTF-8 bị cắt giữa 2 chunk (vd '｜' 3 byte)
        // DS ngừng gửi (không byte nào trong STALL_SECS) → coi là treo, thoát để server retry thay vì chờ vô hạn
        const STALL_SECS: u64 = 90;
        loop {
            let chunk = match tokio::time::timeout(std::time::Duration::from_secs(STALL_SECS), stream.next()).await {
                Err(_) => { eprintln!("[ds-stall] không có dữ liệu {}s (sau {}ms, {} bytes) — bỏ stream", STALL_SECS, t0.elapsed().as_millis(), total_bytes); break; }
                Ok(None) => break,
                Ok(Some(Err(e))) => { eprintln!("[ds-stream-err] {} (sau {}ms, {} bytes)", e, t0.elapsed().as_millis(), total_bytes); break; }
                Ok(Some(Ok(c))) => c,
            };
            first_byte_ms.get_or_insert_with(|| t0.elapsed().as_millis());
            total_bytes += chunk.len();
            raw.extend_from_slice(&chunk);
            match std::str::from_utf8(&raw) {
                Ok(t) => { buf.push_str(t); raw.clear(); }
                Err(e) => {
                    let ok = e.valid_up_to();
                    buf.push_str(std::str::from_utf8(&raw[..ok]).unwrap_or(""));
                    if e.error_len().is_none() {
                        raw.drain(..ok); // cuối chunk là ký tự dở — chờ chunk sau
                    } else {
                        buf.push_str(&String::from_utf8_lossy(&raw[ok..]));
                        raw.clear();
                    }
                }
            }
            // xử lý từng dòng hoàn chỉnh
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                let rest = buf[pos + 1..].to_string();
                buf = rest;
                let Some(data) = line.strip_prefix("data: ") else { continue };
                full.push_str(&line); full.push('\n');
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };

                // msg id
                if !sent_msg_id {
                    if let Some(rid) = v.get("response_message_id").and_then(|x| x.as_i64()) {
                        let _ = tx.send(StreamEv::MsgId(rid));
                        sent_msg_id = true;
                    }
                }

                // init object: fragments array
                if let Some(frags) = v.get("v").and_then(|x| x.get("response")).and_then(|r| r.get("fragments")).and_then(|f| f.as_array()) {
                    if let Some(last) = frags.last() {
                        cur_type = last["type"].as_str().unwrap_or("").to_string();
                        if cur_type == "RESPONSE" {
                            has_content = true;
                            if let Some(c) = last["content"].as_str() {
                                let _ = tx.send(StreamEv::Response(c.to_string()));
                            }
                        } else if cur_type == "THINK" {
                            if let Some(c) = last["content"].as_str() {
                                let _ = tx.send(StreamEv::Thinking(c.to_string()));
                            }
                        }
                    }
                    continue;
                }
                // fragment mới APPEND
                if v.get("p").and_then(|p| p.as_str()) == Some("response/fragments") {
                    if v.get("o").and_then(|o| o.as_str()) == Some("APPEND") {
                        if let Some(arr) = v.get("v").and_then(|x| x.as_array()) {
                            for f in arr {
                                cur_type = f["type"].as_str().unwrap_or("").to_string();
                                if cur_type == "RESPONSE" {
                                    has_content = true;
                                    if let Some(c) = f["content"].as_str() {
                                        let _ = tx.send(StreamEv::Response(c.to_string()));
                                    }
                                } else if cur_type == "THINK" {
                                    if let Some(c) = f["content"].as_str() {
                                        let _ = tx.send(StreamEv::Thinking(c.to_string()));
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }
                // append delta vào fragment active (có path)
                if let Some(p) = v.get("p").and_then(|p| p.as_str()) {
                    if p.ends_with("/content") {
                        if let Some(c) = v.get("v").and_then(|x| x.as_str()) {
                            if cur_type == "RESPONSE" {
                                has_content = true;
                                let _ = tx.send(StreamEv::Response(c.to_string()));
                            } else if cur_type == "THINK" {
                                let _ = tx.send(StreamEv::Thinking(c.to_string()));
                            }
                        }
                    }
                    continue;
                }
                // bare {"v":"text"} → continuation của fragment active (case web trả phần lớn chữ)
                if cur_type == "RESPONSE" {
                    if let Some(c) = v.get("v").and_then(|x| x.as_str()) {
                        has_content = true;
                        let _ = tx.send(StreamEv::Response(c.to_string()));
                    }
                } else if cur_type == "THINK" {
                    if let Some(c) = v.get("v").and_then(|x| x.as_str()) {
                        let _ = tx.send(StreamEv::Thinking(c.to_string()));
                    }
                }
            }
        }
        eprintln!("[ds-timing] first_byte={}ms total={}ms bytes={} resp_chars={}", first_byte_ms.unwrap_or(0), t0.elapsed().as_millis(), total_bytes, full.chars().count());
        let _ = tx.send(StreamEv::RawHasContent(has_content));
        let _ = tx.send(StreamEv::Done(full));
        let _ = sid2;
    });

    Ok((sid, rx))
}

}
