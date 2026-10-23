# ds-harness

Proxy OpenAI-compatible (`/v1/chat/completions`, `/v1/models`) chạy trên các phiên DeepSeek web. UI quản lý acc: `http://127.0.0.1:8790/`.

## Chạy
`DS_PORT=8790 ./target/release/deepseek-harness serve` (mặc định 8787) — hoặc dùng `deploy/ds-harness.service` (systemd --user).

## Biến môi trường
| Biến | Mặc định | Ý nghĩa |
|---|---|---|
| `DS_PORT` | 8787 | cổng nghe (127.0.0.1) |
| `DS_MAX_CONC_PER_ACC` | 2 | call đồng thời tối đa / acc (>2 dễ bị khoá) |
| `DS_QUEUE_WAIT_SECS` | 120 | chờ acc rảnh trước khi trả 429 |
| `DS_SESSION_ROTATE` | 30 | số lượt / session web trước khi đổi session mới |
| `DS_FAIL_COOLDOWN_MAX` | 600 | trần cooldown (s) cho acc hỏng liên tiếp (15, 30, 60 …) |
| `DS_PROMPT_MAX_CHARS` | 400000 | vượt ngân sách thì cắt giữa tool_result cũ, rồi bỏ lượt cũ nhất |
| `DS_API_KEY` | (trống) | đặt thì `/v1/*` yêu cầu `Authorization: Bearer <key>`; UI và `/api/*` luôn chỉ cho localhost |
| `DS_ACCOUNTS_PATH` | ~/.ds-harness/accounts.json | file acc (tiện chạy bản test với bộ acc riêng) |
| `DS_BURST_LIMIT` / `DS_BURST_WINDOW` / `DS_COOLDOWN` | 6 / 30 / 10 | giới hạn burst / acc |

## Cấu trúc
- `src/chat.rs`: handler `/v1/chat/completions` — một lõi `run_chat` (chọn acc, retry, **failover sang acc khác**, guard markup); stream và non-stream cùng dùng.
- `src/server.rs`: acc/pool, UI, parser tool-call (DSML), dựng prompt. `src/client.rs`: HTTP + SSE của DS web. `src/chrome_capture.rs`: CDP.
- `/api/stats`: `in_flight`, `fail_streak`, cooldown từng acc.

## Thiết kế
- Prompt, lịch sử và parser đều dùng đúng format tool-call gốc của DS web (`<｜｜DSML｜｜ calls>…`), xem `TOOL_FORMAT_GUIDE`.
- Một đường parse tool-call duy nhất: `parse_any_tool_calls` (stream và non-stream).
- Guard chặn markup DSML lọt vào `content` lẫn `reasoning_content`; stream treo >90s hoặc lỗi giữa chừng được log (`[ds-stall]`, `[ds-stream-err]`) và retry.
- Mỗi lượt lỗi/rỗng/cụt thì đổi sang acc khác (tối đa 3 lượt); acc hỏng bị cách ly với cooldown tăng dần. Hết lượt trả `error` rõ cho client.
- Khoá in-flight giữ suốt vòng đời stream (kể cả khi client ngắt) nên giới hạn đồng thời / acc có hiệu lực.
- Stream cụt giữa chừng (stall/lỗi mạng) được phát hiện (`StreamEv::Truncated`) và retry, không coi là hoàn tất.
- Log chẩn đoán: `[ds-timing]`, `[toolcall-unparsed]`, `[toolcall-guard]`, `[ds-fail]`.
