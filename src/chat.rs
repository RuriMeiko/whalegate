// Handler /v1/chat/completions.
// MỘT lõi duy nhất (`run_chat`): chọn acc, giữ khoá in-flight suốt vòng đời stream, guard markup tool-call,
// retry + chuyển sang acc khác khi lỗi/rỗng/cụt. Stream SSE và non-stream chỉ là hai cách tiêu thụ lõi này.
use crate::client::{DsClient, StreamEv};
use crate::server::*;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json, Response};
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

type ApiErr = (StatusCode, Json<Value>);

/// Tự nhả khoá in-flight khi bị drop (kể cả client ngắt kết nối giữa stream).
struct InFlightGuard(Arc<std::sync::atomic::AtomicU32>);
impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Release);
    }
}

/// Một acc đã được chiếm cho đến khi `Lease` bị drop.
struct Lease {
    acc_id: String,
    token: String,
    cookies: std::collections::BTreeMap<String, String>,
    thread_sid: Option<String>,
    _guard: InFlightGuard,
}

/// Sự kiện trung gian của lõi chat.
enum Out {
    Reasoning(String),
    Content(String),
    ToolCalls(Vec<DsmlToolCall>),
    Finish { reason: &'static str, prompt_tokens: i64, completion_tokens: i64 },
    Error(String),
}

struct ChatCtx {
    prompt: String,
    thinking: bool,
    search: bool,
    tools: Option<Vec<Value>>,
}

fn queue_wait_secs() -> u64 {
    std::env::var("DS_QUEUE_WAIT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(120)
}

/// Chọn acc ít bận nhất (rồi lâu chưa dùng nhất). `exclude` là loại trừ MỀM: bỏ qua các acc đó khi còn acc khác
/// trong pool; pool chỉ có đúng acc đó thì vẫn dùng. Chờ tới `deadline` rồi trả 429.
async fn acquire(st: &Arc<AppState>, exclude: &[String], deadline: Instant) -> Result<Lease, ApiErr> {
    loop {
        let t = now();
        let picked = {
            let store = st.store.read().await;
            let accs = &store.accounts;
            if accs.is_empty() {
                return Err((StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": "no account configured"}))));
            }
            let others_exist = accs.iter().any(|a| a.pool_enabled && !exclude.contains(&a.id));
            let in_flight = st.in_flight.lock().unwrap();
            let mut best: Option<((u32, u64), usize)> = None;
            for (i, a) in accs.iter().enumerate() {
                if !a.pool_enabled || (others_exist && exclude.contains(&a.id)) {
                    continue;
                }
                let conc = in_flight
                    .get(&a.id)
                    .map(|v| v.load(std::sync::atomic::Ordering::Acquire))
                    .unwrap_or(0);
                // >2 call đồng thời / acc dễ bị khoá (DS_MAX_CONC_PER_ACC)
                if conc < AccStats::max_conc() && a.stats.available(t) {
                    let key = (conc, a.stats.last_used_at);
                    if best.map(|(bk, _)| key < bk).unwrap_or(true) {
                        best = Some((key, i));
                    }
                }
            }
            drop(in_flight);
            best.map(|(_, i)| {
                let a = &accs[i];
                let sid = if a.thread_uses >= session_rotate_limit() { None } else { a.thread_session_id.clone() };
                (a.token.clone(), a.cookies.clone(), sid, a.id.clone())
            })
        };

        if let Some((token, cookies, thread_sid, acc_id)) = picked {
            let counter = {
                let mut map = st.in_flight.lock().unwrap();
                map.entry(acc_id.clone())
                    .or_insert_with(|| Arc::new(std::sync::atomic::AtomicU32::new(0)))
                    .clone()
            };
            if counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel) < AccStats::max_conc() {
                return Ok(Lease { acc_id, token, cookies, thread_sid, _guard: InFlightGuard(counter) });
            }
            counter.fetch_sub(1, std::sync::atomic::Ordering::Release); // thua race → chọn lại
        }
        if Instant::now() >= deadline {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": "pool exhausted: all accounts busy/cooling down", "retry_after_secs": 5})),
            ));
        }
        // jitter nhẹ cho đỡ đều đều
        tokio::time::sleep(Duration::from_millis(200 + (t % 500))).await;
    }
}

/// Ghi nhận 1 lượt gọi thực sự lên acc (tính vào burst).
async fn touch_acc(st: &Arc<AppState>, acc_id: &str) {
    let mut store = st.store.write().await;
    if let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) {
        a.stats.record_call(now());
    }
}

/// Acc hỏng/rỗng/cụt: tăng bộ đếm + cooldown tăng dần. `penalize=false` cho lỗi do model (không phải acc).
async fn fail_acc(st: &Arc<AppState>, acc_id: &str, why: &str, penalize: bool) {
    let mut store = st.store.write().await;
    if let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) {
        if penalize {
            if why == "empty" {
                a.stats.total_empty += 1;
            } else {
                a.stats.total_error += 1;
            }
            a.stats.mark_fail(now());
            a.last_status = Some(if why == "empty" { "throttled".into() } else { "error".into() });
        }
    }
    let _ = save_store(&store).await;
}

async fn ok_acc(st: &Arc<AppState>, acc_id: &str, sid: &str) {
    let mut store = st.store.write().await;
    if let Some(a) = store.accounts.iter_mut().find(|a| a.id == acc_id) {
        note_session_use(a, sid);
        a.stats.total_ok += 1;
        a.stats.mark_ok();
        a.last_status = Some("ok".into());
    }
    let _ = save_store(&store).await;
}

/// Lõi: retry tối đa 3 lượt, mỗi lượt lỗi/rỗng/cụt thì đổi sang acc khác.
fn run_chat(st: Arc<AppState>, ctx: ChatCtx, lease0: Lease) -> impl Stream<Item = Out> {
    async_stream::stream! {
        let mut lease = Some(lease0);
        let mut failed: Vec<String> = Vec::new();
        let mut last_err: Option<String> = None;
        let mut finished = false;
        let prompt_tokens = (ctx.prompt.chars().count() as i64) / 4;

        for attempt in 0..3usize {
            if attempt > 0 {
                // failover: nhả acc hỏng rồi lấy acc khác (mềm)
                let prev = lease.take().map(|l| l.acc_id);
                let dl = Instant::now() + Duration::from_secs(queue_wait_secs().min(60));
                match acquire(&st, &failed, dl).await {
                    Ok(l) => {
                        if Some(&l.acc_id) == prev.as_ref() {
                            // pool chỉ còn đúng acc này → chờ DS hồi (rate-limit mềm ~8-10s)
                            tokio::time::sleep(Duration::from_secs(8)).await;
                        }
                        lease = Some(l);
                    }
                    Err((_, Json(e))) => {
                        last_err = Some(e.get("error").and_then(|x| x.as_str()).unwrap_or("hết acc khả dụng").to_string());
                        break;
                    }
                }
            }
            let l = lease.as_ref().unwrap();
            let acc_id = l.acc_id.clone();
            touch_acc(&st, &acc_id).await;

            let stream_res = match DsClient::new(l.token.clone(), cookie_string(&l.cookies)) {
                Ok(c) => c.chat_stream(&ctx.prompt, l.thread_sid.as_deref(), None, ctx.thinking, ctx.search).await,
                Err(e) => Err(e),
            };
            let (sid, mut rx) = match stream_res {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("[ds-error] attempt {} acc {}: {:#}", attempt, &acc_id[..8.min(acc_id.len())], e);
                    last_err = Some(format!("{:#}", e));
                    fail_acc(&st, &acc_id, "error", true).await;
                    failed.push(acc_id);
                    continue;
                }
            };

            let mut resp_text = String::new(); // phần content chưa flush (đuôi nghi là marker)
            let mut full_text = String::new();
            let mut yielded_len: usize = 0;
            let mut think_len: usize = 0;
            let mut think_buf = String::new();
            let mut think_dropped = false;
            let mut has_content = false;
            let mut truncated: Option<String> = None;
            let mut attempt_done = false;

            while let Some(ev) = rx.recv().await {
                match ev {
                    StreamEv::Thinking(t) => {
                        think_len += t.chars().count();
                        // model hay nháp khối tool-call trong lúc nghĩ → không để markup lọt ra client
                        if !think_dropped && !t.is_empty() {
                            think_buf.push_str(&t);
                            let cut = safe_flush_len(&think_buf);
                            if has_tool_marker(&think_buf) {
                                eprintln!("[toolcall-guard] DSML trong reasoning — cắt từ marker");
                                think_dropped = true;
                            }
                            if cut > 0 {
                                yield Out::Reasoning(think_buf[..cut].to_string());
                            }
                            if think_dropped { think_buf.clear(); } else { think_buf.drain(..cut); }
                        }
                    }
                    StreamEv::Response(t) => {
                        has_content = true;
                        resp_text.push_str(&t);
                        full_text.push_str(&t);
                        // chỉ flush phần chắc chắn là prose; giữ đuôi nghi marker / toàn bộ từ marker trở đi
                        let cut = safe_flush_len(&resp_text);
                        if cut > 0 {
                            let head: String = resp_text[..cut].to_string();
                            yielded_len += head.chars().count();
                            resp_text.drain(..cut);
                            yield Out::Content(head);
                        }
                    }
                    StreamEv::Truncated(why) => { truncated = Some(why); }
                    StreamEv::Done(_) => {
                        if !think_dropped && !think_buf.is_empty() {
                            yield Out::Reasoning(std::mem::take(&mut think_buf));
                        }
                        let completion_tokens = (full_text.chars().count() as i64 + think_len as i64) / 4;

                        if let Some((before, mut calls)) = parse_any_tool_calls(&full_text) {
                            // prose trước khối tool-call mà chưa stream (before đã trim)
                            let before_chars = before.chars().count();
                            if before_chars > yielded_len {
                                let rem: String = before.chars().skip(yielded_len).collect();
                                if !rem.is_empty() { yield Out::Content(rem); }
                            }
                            for c in calls.iter_mut() { fill_required_args(&ctx.tools, c); }
                            ok_acc(&st, &acc_id, &sid).await;
                            yield Out::ToolCalls(calls);
                            yield Out::Finish { reason: "tool_calls", prompt_tokens, completion_tokens };
                            finished = true;
                        } else if let Some(why) = truncated.clone() {
                            eprintln!("[ds-truncated] attempt {}: {}", attempt, why);
                            last_err = Some(why);
                            if attempt == 2 && has_content && !has_tool_marker(&full_text) {
                                // lượt cuối: trả phần đã có, báo 'length' để client biết bị cắt
                                if !resp_text.is_empty() { yield Out::Content(std::mem::take(&mut resp_text)); }
                                yield Out::Finish { reason: "length", prompt_tokens, completion_tokens };
                                finished = true;
                            } else {
                                fail_acc(&st, &acc_id, "error", true).await;
                                failed.push(acc_id.clone());
                            }
                        } else if !has_content {
                            last_err = Some("DeepSeek web trả stream rỗng (có thể bị throttle)".into());
                            fail_acc(&st, &acc_id, "empty", true).await;
                            failed.push(acc_id.clone());
                        } else if has_tool_marker(&full_text) {
                            // có marker tool-call nhưng parse trượt: log nguyên văn để vá, retry; lỗi do model nên không phạt acc
                            eprintln!("[toolcall-unparsed] attempt {}; raw={:?}", attempt, full_text);
                            if attempt == 2 {
                                // bỏ mảnh markup, không để lọt ra client
                                yield Out::Finish { reason: "stop", prompt_tokens, completion_tokens };
                                finished = true;
                            } else {
                                last_err = Some("tool-call không parse được".into());
                                fail_acc(&st, &acc_id, "unparsed", false).await;
                            }
                        } else {
                            if !resp_text.is_empty() { yield Out::Content(std::mem::take(&mut resp_text)); }
                            ok_acc(&st, &acc_id, &sid).await;
                            yield Out::Finish { reason: "stop", prompt_tokens, completion_tokens };
                            finished = true;
                        }
                        attempt_done = true;
                    }
                    _ => {}
                }
                if attempt_done { break; }
            }
            if finished { break; }
        }

        if !finished {
            let msg = last_err.unwrap_or_else(|| "DeepSeek web không trả dữ liệu".into());
            eprintln!("[ds-fail] {}", msg);
            yield Out::Error(msg);
        }
    }
}

/// Rút gọn lịch sử nếu prompt vượt ngân sách (DS_PROMPT_MAX_CHARS, mặc định 400K ký tự):
/// 1) cắt giữa các tool_result cũ, 2) bỏ các lượt cũ nhất (giữ system + tin user đầu).
pub(crate) fn fit_messages(messages: &[Value], max_chars: usize) -> Vec<Value> {
    let size = |m: &[Value]| messages_to_prompt(m).chars().count();
    let mut msgs = messages.to_vec();
    if size(&msgs) <= max_chars {
        return msgs;
    }
    let keep_from = msgs.len().saturating_sub(6);
    for i in 0..keep_from {
        if msgs[i].get("role").and_then(|r| r.as_str()) == Some("tool") {
            let text = render_content(&msgs[i]);
            let n = text.chars().count();
            if n > 2500 {
                let head: String = text.chars().take(1500).collect();
                let tail: String = text.chars().skip(n - 500).collect();
                msgs[i]["content"] = Value::String(format!("{}\n…[đã rút gọn {} ký tự]…\n{}", head, n - 2000, tail));
            }
        }
        if size(&msgs) <= max_chars { return msgs; }
    }
    let first_user = msgs.iter().position(|m| m.get("role").and_then(|r| r.as_str()) == Some("user")).unwrap_or(0);
    while size(&msgs) > max_chars && msgs.len() > 8 {
        let k = msgs.iter().enumerate()
            .position(|(i, m)| i > first_user && m.get("role").and_then(|r| r.as_str()) != Some("system"));
        let Some(k) = k else { break };
        msgs.remove(k);
        while k < msgs.len() && msgs[k].get("role").and_then(|r| r.as_str()) == Some("tool") {
            msgs.remove(k); // không để tool_result mồ côi
        }
    }
    msgs
}

fn chunk(id: &str, model: &str, delta: Value, finish: Value) -> Event {
    Event::default().data(
        json!({
            "id": id, "object": "chat.completion.chunk", "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]
        })
        .to_string(),
    )
}

pub(crate) async fn api_chat_completions(
    State(st): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Response, ApiErr> {
    let model_name = req.model.clone().unwrap_or_else(|| "deepseek-web".into());
    let stream = req.stream.unwrap_or(false);

    // yêu cầu đặt tên phiên của client: không cần thinking (đỡ 4-5s + slot acc)
    let is_title_req = req.tools.as_ref().map_or(true, |t| t.is_empty())
        && req.messages.iter().any(|m| render_content(m).contains("Create a concise title"));
    let thinking = (req.model.as_deref().map(|m| m.contains("r1")).unwrap_or(false) || req.thinking.unwrap_or(false))
        && !is_title_req;
    let search = req.search.unwrap_or(false);

    let max_chars: usize = std::env::var("DS_PROMPT_MAX_CHARS").ok().and_then(|v| v.parse().ok()).unwrap_or(400_000);
    let msgs = fit_messages(&req.messages, max_chars);
    let tools_hdr = render_tools_spec(&req.tools);
    let prompt = format!("{}\n{}\n", tools_hdr, messages_to_prompt(&msgs));
    eprintln!(
        "[req] model={} stream={} tools={} msgs={}→{} prompt_chars={} thinking={}",
        model_name, stream, req.tools.as_ref().map_or(0, |t| t.len()), req.messages.len(), msgs.len(),
        prompt.chars().count(), thinking
    );

    let deadline = Instant::now() + Duration::from_secs(queue_wait_secs());
    let lease = acquire(&st, &[], deadline).await?;
    let ctx = ChatCtx { prompt, thinking, search, tools: req.tools.clone() };
    let mut outs = Box::pin(run_chat(st, ctx, lease));
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());

    if stream {
        let body = async_stream::stream! {
            yield chunk(&id, &model_name, json!({"role": "assistant", "content": ""}), Value::Null);
            while let Some(o) = outs.next().await {
                match o {
                    Out::Reasoning(t) => yield chunk(&id, &model_name, json!({"reasoning_content": t}), Value::Null),
                    Out::Content(t) => yield chunk(&id, &model_name, json!({"content": t}), Value::Null),
                    Out::ToolCalls(calls) => {
                        for (i, c) in calls.iter().enumerate() {
                            yield chunk(&id, &model_name, json!({"tool_calls": [{
                                "index": i, "id": new_call_id(), "type": "function",
                                "function": {"name": c.name, "arguments": c.arguments}
                            }]}), Value::Null);
                        }
                    }
                    Out::Finish { reason, prompt_tokens, completion_tokens } => {
                        yield chunk(&id, &model_name, json!({}), json!(reason));
                        yield Event::default().data(json!({
                            "id": id, "object": "chat.completion.chunk", "model": model_name, "choices": [],
                            "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens,
                                      "total_tokens": prompt_tokens + completion_tokens,
                                      "prompt_tokens_details": {"cached_tokens": 0}}
                        }).to_string());
                    }
                    Out::Error(m) => {
                        yield Event::default().data(json!({"error": {"message": m, "type": "upstream_error", "code": "ds_upstream_failed"}}).to_string());
                    }
                }
            }
            yield Event::default().data("[DONE]".to_string());
        };
        return Ok(Sse::new(body.map(|e| Ok::<_, std::convert::Infallible>(e))).into_response());
    }

    // non-stream: gom các sự kiện của cùng một lõi
    let (mut content, mut reasoning) = (String::new(), String::new());
    let mut tool_calls: Vec<Value> = Vec::new();
    let (mut finish, mut pt, mut ct) = ("stop", 0i64, 0i64);
    while let Some(o) = outs.next().await {
        match o {
            Out::Reasoning(t) => reasoning.push_str(&t),
            Out::Content(t) => content.push_str(&t),
            Out::ToolCalls(calls) => {
                tool_calls = calls.iter().map(|c| json!({
                    "id": new_call_id(), "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments}
                })).collect();
            }
            Out::Finish { reason, prompt_tokens, completion_tokens } => { finish = reason; pt = prompt_tokens; ct = completion_tokens; }
            Out::Error(m) => {
                return Err((StatusCode::BAD_GATEWAY, Json(json!({"error": {"message": m, "type": "upstream_error", "code": "ds_upstream_failed"}}))));
            }
        }
    }
    let mut message = json!({"role": "assistant", "content": if content.is_empty() && !tool_calls.is_empty() { Value::Null } else { Value::String(content) }});
    if !reasoning.is_empty() { message["reasoning_content"] = Value::String(reasoning); }
    if !tool_calls.is_empty() { message["tool_calls"] = Value::Array(tool_calls); }
    Ok(Json(json!({
        "id": id, "object": "chat.completion", "model": model_name,
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct}
    })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_messages_shrinks_old_tool_results_then_drops_oldest() {
        let big = "x".repeat(20_000);
        let mut msgs = vec![json!({"role": "system", "content": "sys"}), json!({"role": "user", "content": "task"})];
        for i in 0..6 {
            msgs.push(json!({"role": "assistant", "content": "", "tool_calls": [{"id": format!("c{}", i), "type": "function", "function": {"name": "bash", "arguments": "{}"}}]}));
            msgs.push(json!({"role": "tool", "tool_call_id": format!("c{}", i), "content": big.clone()}));
        }
        msgs.push(json!({"role": "user", "content": "tiếp"}));
        let full = messages_to_prompt(&msgs).chars().count();
        let fitted = fit_messages(&msgs, full / 2);
        let after = messages_to_prompt(&fitted).chars().count();
        assert!(after <= full / 2 + 3000, "phải rút gọn: {} → {}", full, after);
        assert_eq!(fitted[0]["role"], "system");
        assert_eq!(fitted[1]["content"], "task"); // tin user đầu còn nguyên
        assert_eq!(fitted.last().unwrap()["content"], "tiếp"); // tin mới nhất còn nguyên
        // không có tool_result mồ côi (mỗi tool đứng sau assistant có tool_calls)
        for (i, m) in fitted.iter().enumerate() {
            if m["role"] == "tool" { assert!(fitted[i - 1]["role"] == "assistant" || fitted[i - 1]["role"] == "tool"); }
        }
        // ngân sách đủ lớn thì giữ nguyên
        assert_eq!(fit_messages(&msgs, full * 2).len(), msgs.len());
    }
}
