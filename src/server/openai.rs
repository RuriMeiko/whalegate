//! API tương thích OpenAI: dựng prompt từ messages, render tool, thống kê, proxy đầu ra, /v1/models.

#[allow(unused_imports)]
use super::*;

// ---------- OpenAI-compatible API ----------

#[derive(Deserialize)]
pub(crate) struct ChatRequest {
    pub(crate) model: Option<String>,
    pub(crate) messages: Vec<Value>,
    pub(crate) stream: Option<bool>,
    // true = tạo conversation mới; false/mặc định = chat tiếp thread hiện có
    pub(crate) new_thread: Option<bool>,
    // Bật DeepThink (thinking) / Tìm kiếm thông minh (websearch). Mặc định cả hai OFF.
    pub(crate) thinking: Option<bool>,
    pub(crate) search: Option<bool>,
    // mức suy nghĩ kiểu OpenAI: "none" = Nhanh, "max"/khác = DeepThink
    #[serde(default)]
    pub(crate) reasoning_effort: Option<String>,
    #[serde(default)]
    pub(crate) reasoning: Option<Value>,
    // OpenAI tools schema từ client (dsh) — render thành hướng dẫn DSML cho model
    #[serde(default)]
    pub(crate) tools: Option<Vec<Value>>,
    // "auto" | "none" | "required" | {"type":"function","function":{"name":X}} (kiểu OpenAI)
    #[serde(default)]
    pub(crate) tool_choice: Option<Value>,
    #[serde(default)]
    pub(crate) parallel_tool_calls: Option<bool>,
}

// Render tool specs (OpenAI schema) thành hướng dẫn gọi DSML — NHẤN MẠNH: mọi parameter
// trong "required" PHẢI có mặt trong arguments (dsh schema-reject call thiếu description).
/// Format tool-call chuẩn — ĐÚNG format DS web tự dùng; hướng dẫn, lịch sử và parser đều bám format này
/// (trước đây hướng dẫn nói `<dsml:tool_call>{json}` còn lịch sử render DSML → model trộn nhiều biến thể).
pub(crate) const TOOL_FORMAT_GUIDE: &str = "# Tools\n\
To call tools, END your reply with exactly one block in this format (nothing after it):\n\
<｜｜DSML｜｜ calls>\n\
<｜｜DSML｜｜ invoke name=\"TOOL_NAME\">\n\
<｜｜DSML｜｜ parameter name=\"PARAM_NAME\" string=\"true\">VALUE</｜｜DSML｜｜ parameter>\n\
</｜｜DSML｜｜ invoke>\n\
</｜｜DSML｜｜ calls>\n\
Use string=\"true\" for text values; for numbers, booleans, arrays and objects use string=\"false\" with a JSON value. \
Several invoke elements may share one calls block. Include every REQUIRED parameter. \
Keep any text before the block short. If you need no tool, answer in plain text with no block.\n";

pub(crate) fn render_tools_spec(tools: &Option<Vec<Value>>) -> String {
    let Some(tl) = tools else { return String::new() };
    if tl.is_empty() { return String::new(); }
    let mut out = String::from(TOOL_FORMAT_GUIDE);
    out.push_str("\nAvailable tools:\n");
    for t in tl {
        let f = t.get("function").unwrap_or(t);
        let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let desc = f.get("description").and_then(|x| x.as_str()).unwrap_or("");
        out.push_str(&format!("- {}: {}\n", name, desc));
        if let Some(params) = f.get("parameters").and_then(|p| p.get("properties")).and_then(|p| p.as_object()) {
            let required: Vec<&str> = f.get("parameters")
                .and_then(|p| p.get("required")).and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            for (k, v) in params {
                let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("string");
                let en = v.get("enum").and_then(|e| e.as_array())
                    .map(|a| format!(" one of {}", a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("|")))
                    .unwrap_or_default();
                let req_mark = if required.contains(&k.as_str()) { ", REQUIRED" } else { "" };
                let pd = v.get("description").and_then(|x| x.as_str()).unwrap_or("");
                let def = v.get("default").map(|d| format!(" (default {})", d)).unwrap_or_default();
                out.push_str(&format!("  param {} ({}{}{}): {}{}\n", k, ty, en, req_mark, pd, def));
                // kiểu lồng (mảng object, object có properties, anyOf…): in schema rút gọn, không thì model đoán bừa hình dạng
                let nested = ["items", "properties", "anyOf", "oneOf", "allOf", "$ref"].iter().any(|key| v.get(*key).is_some());
                if nested {
                    let mut s = v.clone();
                    if let Some(o) = s.as_object_mut() { o.remove("description"); o.remove("default"); }
                    out.push_str(&format!("    schema: {}\n", s));
                }
            }
        }
    }
    out
}

pub(crate) fn render_content(m: &Value) -> String {
    // content có thể là string hoặc array of parts [{type:"text"|"tool_result"|"tool_call"...}]
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                match part.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                    // ảnh: nội dung thật được upload riêng, trong prompt chỉ để dấu vị trí
                    Some("image_url") | Some("image") | Some("input_image") => {
                        out.push_str("[hình ảnh đính kèm]\n");
                    }
                    // tool result: text lồng [{type:"text",text:"..."}]
                    Some("tool_result") => {
                        if let Some(inner) = part.get("content") {
                            match inner {
                                Value::String(s) => {
                                    out.push_str(s);
                                    out.push('\n');
                                }
                                Value::Array(items) => {
                                    for it in items {
                                        if let Some(t) = it.get("text").and_then(|x| x.as_str()) {
                                            out.push_str(t);
                                            out.push('\n');
                                        } else if matches!(it.get("type").and_then(|t| t.as_str()), Some("image_url") | Some("image") | Some("input_image")) {
                                            out.push_str("[hình ảnh đính kèm]\n");
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            out.trim_end().to_string()
        }
        _ => String::new(),
    }
}

pub(crate) fn render_tool_calls(m: &Value) -> String {
    // assistant message có tool_calls → render lại đúng format chuẩn để model thấy chính nó đã gọi gì
    let mut out = String::new();
    if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
        out.push_str("\n<｜｜DSML｜｜ calls>");
        for tc in tcs {
            let name = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("unknown");
            out.push_str(&format!("\n<｜｜DSML｜｜ invoke name=\"{}\">", name));
            if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")) {
                // arguments là JSON string — parse rồi render từng parameter
                let parsed: Option<Value> = args
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .or_else(|| args.as_object().map(|_| args.clone()));
                if let Some(obj) = parsed.and_then(|v| v.as_object().cloned()) {
                    for (k, v) in obj {
                        // chuỗi giữ nguyên (kể cả xuống dòng); kiểu khác ghi JSON + string="false"
                        let (flag, sv) = match &v {
                            Value::String(x) => ("true", x.clone()),
                            other => ("false", other.to_string()),
                        };
                        out.push_str(&format!(
                            "\n<｜｜DSML｜｜ parameter name=\"{}\" string=\"{}\">{}</｜｜DSML｜｜ parameter>",
                            k, flag, sv
                        ));
                    }
                }
            }
            out.push_str("\n</｜｜DSML｜｜ invoke>");
        }
        out.push_str("\n</｜｜DSML｜｜ calls>");
    }
    out
}

pub(crate) fn messages_to_prompt(messages: &[Value]) -> String {
    messages_to_prompt_from(messages, 0)
}

/// id tool-call → tên tool, dùng để gắn nhãn tool_result.
pub(crate) fn call_name_map(messages: &[Value]) -> std::collections::HashMap<String, String> {
    let mut call_names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for m in messages {
        if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                if let (Some(id), Some(n)) = (tc.get("id").and_then(|x| x.as_str()), tc.pointer("/function/name").and_then(|x| x.as_str())) {
                    call_names.insert(id.to_string(), n.to_string());
                }
            }
        }
    }
    call_names
}

/// Render MỘT message thành đoạn prompt (None = bỏ qua). Tách riêng để `fit_messages`
/// tính kích thước từng message một lần thay vì render lại cả danh sách sau mỗi lần cắt.
pub(crate) fn render_msg(m: &Value, call_names: &std::collections::HashMap<String, String>) -> Option<String> {
    let role = m["role"].as_str().unwrap_or("user");
    match role {
        // assistant: content + (nếu có) DSML tool calls của chính nó
        "assistant" => {
            let text = render_content(m);
            let tcs = render_tool_calls(m);
            if text.is_empty() && tcs.is_empty() {
                None
            } else {
                Some(format!("assistant: {}{}", text, tcs))
            }
        }
        // tool result message
        "tool" => {
            // kết quả rỗng (mkdir, touch… thành công) vẫn phải hiện, không thì model tưởng chưa chạy và gọi lại
            let text = render_content(m);
            let text = if text.trim().is_empty() { "(no output)".to_string() } else { text };
            // kèm tên tool + id để model khớp result ↔ call (tránh lặp call)
            let tcid = m["tool_call_id"].as_str().unwrap_or("");
            match call_names.get(tcid) {
                Some(n) => Some(format!("tool_result ({}) [{}]: {}", n, tcid, text)),
                None if tcid.is_empty() => Some(format!("tool_result: {}", text)),
                None => Some(format!("tool_result [{}]: {}", tcid, text)),
            }
        }
        _ => {
            let text = render_content(m);
            if text.is_empty() {
                None
            } else {
                Some(format!("{}: {}", role, text))
            }
        }
    }
}

/// Render từ message thứ `start` (nhãn tool_result vẫn tra tên tool trên toàn bộ lịch sử).
pub(crate) fn messages_to_prompt_from(messages: &[Value], start: usize) -> String {
    let call_names = call_name_map(messages);
    messages
        .iter()
        .skip(start)
        .filter_map(|m| render_msg(m, &call_names))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) async fn api_stats(State(st): State<Arc<AppState>>) -> Json<Value> {
    let now = now();
    let store = st.store.read().await;
    let in_flight: std::collections::HashMap<String, u32> = st.in_flight.lk().iter()
        .map(|(k, v)| (k.clone(), v.load(std::sync::atomic::Ordering::Acquire))).collect();
    let accounts: Vec<Value> = store
        .accounts
        .iter()
        .map(|a| {
            json!({
                "in_flight": in_flight.get(&a.id).copied().unwrap_or(0),
                "fail_streak": a.stats.fail_streak,
                "id": a.id,
                "label": a.label,
                "rl_cap": a.stats.rpm_cap(now),
                "calls_last_minute": a.stats.calls_last_minute(now),
                "login_method": a.login_method_effective(),
                "login_method_known": a.login_method.is_some(),
                "available": a.auth_dead_at.is_none() && a.stats.available(now),
                "cooldown_until": a.stats.cooldown_until,
                "cooldown_left_secs": a.stats.cooldown_until.saturating_sub(now),
                "burst_count": a.stats.burst_count,
                "burst_limit": AccStats::burst_limit(),
                "total_ok": a.stats.total_ok,
                "total_empty": a.stats.total_empty,
                "total_error": a.stats.total_error,
                "last_used_at": a.stats.last_used_at,
                "last_status": a.last_status,
                "pool_enabled": a.pool_enabled,
                "uid": a.ds_uid,
                "device": a.fp_seed.is_some() && a.browser.is_none(),
                "browser": a.browser.is_some(),
                "device_refreshed_at": a.device_refreshed_at,
                "auth_dead_at": a.auth_dead_at,
                "banned": a.banned_at.is_some(),
                "dup": a.ds_uid.as_ref().is_some_and(|u| store.accounts.iter().filter(|b| b.ds_uid.as_ref() == Some(u)).count() > 1),
            })
        })
        .collect();
    let available = store.accounts.iter().filter(|a| a.pool_enabled && a.auth_dead_at.is_none() && a.stats.available(now)).count();
    // đợt đăng xuất gần nhất: các acc mất token trong cùng một cửa sổ với acc chết muộn nhất
    let latest_dead = store.accounts.iter().filter_map(|a| a.auth_dead_at).max();
    let logged_out_burst = latest_dead.map(|l| store.accounts.iter()
        .filter(|a| a.auth_dead_at.is_some_and(|d| l - d <= crate::chat::MASS_LOGOUT_WINDOW)).count()).unwrap_or(0);
    Json(json!({
        "pool": {
            "total": store.accounts.len(),
            "available": available,
            "logged_out": store.accounts.iter().filter(|a| a.auth_dead_at.is_some()).count(),
            "login_notice": live_login_notice(st.login_notice.lk().as_ref(), now),
            "logged_out_last_at": latest_dead,
            "logged_out_last_burst": logged_out_burst,
            "capacity_now_rpm_hint": available as u64 * (AccStats::burst_limit() as u64),
            "max_conc_per_acc": AccStats::max_conc(),
            "stateful": {
                "hits": st.stat_conv_hit.load(std::sync::atomic::Ordering::Relaxed),
                "misses": st.stat_conv_miss.load(std::sync::atomic::Ordering::Relaxed),
                "active_conversations": st.convs.lk().len(),
                "open_sessions": st.sessions.lk().len(),
            },
        },
        "accounts": accounts,
        "now": now,
        "boot": boot_id(),
    }))
}

// ---------- proxy đầu ra (mỗi vân tay một IP) ----------

/// Trạng thái pool + bảng ghim: dashboard hiện đúng cái đang có hiệu lực, kèm lỗi cấu hình
/// (một dòng proxy sai định dạng phải nhìn thấy được, không im lặng bỏ qua).
pub(crate) async fn api_proxy_get() -> Json<Value> {
    let pool = crate::proxypool::snapshot();
    let bindings: Vec<Value> = crate::proxypool::snapshot_bindings()
        .into_iter()
        .map(|(fp, raw)| {
            let masked = crate::proxypool::ProxyEntry::parse(&raw).map(|e| e.masked()).unwrap_or(raw);
            json!({"fingerprint": fp, "proxy": masked})
        })
        .collect();
    Json(json!({
        "mode": pool.mode.as_str(),
        "entries": pool.entries.iter().map(|e| e.masked()).collect::<Vec<_>>(),
        "count": pool.entries.len(),
        "problems": pool.problems,
        "bindings": bindings,
    }))
}

#[derive(Deserialize)]
pub(crate) struct ProxySetReq {
    /// mỗi dòng một proxy; ghi đè toàn bộ danh sách
    list: String,
}

/// Ghi đè danh sách proxy. Chỉ ghi khi MỌI dòng hợp lệ — trả lỗi từng dòng để sửa.
pub(crate) async fn api_proxy_set(Json(r): Json<ProxySetReq>) -> (StatusCode, Json<Value>) {
    match crate::proxypool::write_file(&r.list) {
        Ok(n) => {
            // danh sách đổi → client cũ có thể đang ghim proxy không còn tồn tại: bỏ hết client đã dựng
            // để lần request sau dựng lại theo ghim mới
            crate::client::forget_all();
            crate::elog!("[proxy] đã ghi {} proxy", n);
            (StatusCode::OK, Json(json!({"ok": true, "count": n})))
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": format!("{:#}", e)}))),
    }
}

#[derive(Deserialize)]
pub(crate) struct ProxyUnbindReq { fingerprint: String }

/// Bỏ ghim của một vân tay → lần dùng sau sẽ được cấp proxy khác (đổi IP của acc đó).
pub(crate) async fn api_proxy_unbind(Json(r): Json<ProxyUnbindReq>) -> Json<Value> {
    crate::proxypool::unbind(&r.fingerprint);
    Json(json!({"ok": true}))
}

pub(crate) async fn api_models() -> Json<Value> {
    // MỘT model duy nhất; mức suy nghĩ chọn bằng `reasoning_effort`: "none" = Nhanh (flash), còn lại ("max"…) = DeepThink.
    Json(json!({
        "object": "list",
        "data": [{
            "id": "deepseek-web", "object": "model", "owned_by": "whalegate",
            "input_modalities": ["text", "image"],
            "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
            "reasoning_efforts": ["none", "auto", "max"],
            "context_length": crate::settings::uint("DS_CONTEXT_TOKENS")
        }]
    }))
}

