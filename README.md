# ds-harness

**Proxy OpenAI-compatible chạy trên phiên DeepSeek web** — biến tài khoản chat.deepseek.com của bạn thành endpoint `/v1/chat/completions` chuẩn OpenAI, có pool nhiều account, failover, stream SSE và tool-calling.

> ⚠️ Đọc [Ưu tiên dùng API chính thức](#-ưu-tiên-dùng-api-chính-thức) và [Miễn trừ trách nhiệm](#️-miễn-trừ-trách-nhiệm--dùng-với-trách-nhiệm) trước khi dùng.

---

## Repo này làm gì?

DeepSeek web (`chat.deepseek.com`) không có API công khai miễn phí. Repo này dựng một cầu nối:

```
Client (OpenAI SDK, dsh, Cline, Continue, LiteLLM…)
        │  POST /v1/chat/completions   (đúng chuẩn OpenAI)
        ▼
   ds-harness (Rust, axum)  ── pool account ──┐
        │                                     │
        │  gọi API nội bộ của DS web           │  PoW solver + cookie + token
        ▼                                     │
   chat.deepseek.com/api/v0  ◄────────────────┘
```

Cụ thể nó làm được:

| Tính năng | Mô tả |
|---|---|
| **OpenAI-compatible** | `POST /v1/chat/completions` (stream & non-stream), `GET /v1/models` |
| **Pool nhiều account** | Thêm nhiều acc DS web, chia tải least-busy, giới hạn đồng thời mỗi acc |
| **Failover** | Lượt lỗi / rỗng / cụt stream → tự chuyển acc khác (tối đa 3 lượt), acc hỏng bị cooldown tăng dần |
| **Stream SSE** | Phát `delta` theo thời gian thực, có `reasoning_content` khi bật DeepThink |
| **Tool calling** | Dịch `tools` schema của OpenAI thành hướng dẫn DSML và parse tool-call trả về |
| **Quản lý acc qua UI** | Web UI tại `http://127.0.0.1:8790/`: thêm/xoá/verify acc, xem stats, bắt session từ Chrome |
| **Bắt session tự động** | Qua Chrome DevTools Protocol (CDP): login 1 lần, server tự bắt token + cookie |
| **Giải PoW** | Tự giải `DeepSeekHashV1` proof-of-work mà DS web yêu cầu trước mỗi request |
| **Giữ thread** | Chat tiếp trong cùng conversation của DS web, tự rotate session sau N lượt |
| **Worker proxy (tuỳ chọn)** | Cloudflare Worker xoay nhiều instance harness ở nhiều IP/VPS |

---

## 🚨 Ưu tiên dùng API chính thức

**Nếu bạn chỉ cần dùng DeepSeek cho công việc thật, hãy mua API chính thức trước:**

- **DeepSeek Platform**: <https://platform.deepseek.com/> — trả tiền theo token, có SLA, ổn định, không lo bị khoá acc, không vi phạm ToS.
- Hoặc dùng qua nhà cung cấp có API tương thích OpenAI (OpenRouter, Together, Groq…) nếu cần đa dạng model.

Repo này **không phải** sản phẩm thay thế API chính thức. Nó tồn tại cho mục đích **học tập, nghiên cứu giao thức và tự động hoá cá nhân** trên chính tài khoản của bạn.

Chi phí thực tế: API chính thức của DeepSeek rẻ hơn rất nhiều so với thời gian bạn bỏ ra để vận hành, debug và đối phó với việc acc bị khoá. **Hãy mua API nếu có thể.**

---

## ⚖️ Miễn trừ trách nhiệm — dùng với trách nhiệm

Khi dùng repo này, bạn tự chịu **toàn bộ** trách nhiệm:

1. **Tuân thủ Điều khoản dịch vụ** của DeepSeek. Việc dùng acc web qua proxy tự động có thể **vi phạm ToS** và dẫn đến **khoá tài khoản**. Bạn tự cân nhắc.
2. **Không dùng để bán lại, cho thuê, hoặc chia sẻ API** cho người khác khi chưa được phép — đây là hành vi có thể vi phạm pháp luật về cung cấp dịch vụ.
3. **Không dùng cho mục đích trái pháp luật**, gây hại, spam, lừa đảo, hay tạo nội dung bị cấm.
4. **Bảo mật dữ liệu**: file `~/.ds-harness/accounts.json` chứa **token và cookie phiên đăng nhập** — tương đương quyền truy cập tài khoản. Đừng commit, đừng chia sẻ, đừng đưa lên server công khai khi chưa bật `DS_API_KEY`.
5. **Không có bảo hành** — xem [LICENSE](LICENSE). Tác giả không chịu trách nhiệm cho bất kỳ thiệt hại nào (mất acc, mất dữ liệu, gián đoạn công việc…).

Tóm lại: **dùng trên acc của bạn, cho việc của bạn, và hiểu rõ rủi ro bạn đang nhận.**

---

## Cài đặt

### Yêu cầu

- Rust ≥ 1.75 (stable)
- Linux (đã test), macOS/Windows có thể chạy nhưng phần CDP cần chỉnh
- Chrome/Chromium nếu muốn bắt session tự động (không bắt buộc — có thể dán JSON thủ công)

### Build

```bash
git clone https://github.com/RuriMeiko/ds-harness.git
cd ds-harness
cargo build --release
```

Binary: `target/release/deepseek-harness`

### Chạy

```bash
DS_PORT=8790 ./target/release/deepseek-harness serve
```

Mở UI: <http://127.0.0.1:8790/>

### systemd --user (chạy nền)

```bash
mkdir -p ~/.config/systemd/user
cp deploy/ds-harness.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ds-harness
journalctl --user -u ds-harness -f
```

> File unit mặc định trỏ `WorkingDirectory=%h/ds-harness` — sửa lại nếu bạn clone chỗ khác.

---

## Thêm account

### Cách 1 — Tự động qua Chrome (khuyến nghị)

1. Bật Chrome với cổng CDP (nếu chưa):
   ```bash
   google-chrome --remote-debugging-port=9333
   ```
2. Vào UI → bấm **"➕ Login account mới"** — server tự mở tab DeepSeek trong Chrome.
3. Đăng nhập trong tab đó (Google / email / passkey — kiểu gì cũng được). Server canh tab (poll 2s), thấy token → verify → **lưu → tự đóng tab**, acc hiện ngay trong bảng.
4. Hoặc bấm **"🔍 Quét tabs & lưu session"** để bắt toàn bộ tab `chat.deepseek.com` đang đăng nhập sẵn.
5. Lần đầu nên bấm **"🏗️ Build profile"** (~40s): Chrome login tự dạo web thật để fingerprint trông tin cậy, đỡ lỗi `Current device environment error`. Profile được giữ nguyên giữa các lần login.

Server còn tự **refresh cookie từ Chrome mỗi 5 phút** cho acc khớp token đang sống.

### Cách 2 — Dán JSON thủ công

Trong DevTools của tab `chat.deepseek.com` đã login, chạy:

```js
JSON.stringify({
  token: JSON.parse(localStorage.getItem('userToken')).value,
  cookies: Object.fromEntries(document.cookie.split('; ').map(c => {
    const i = c.indexOf('='); return [c.slice(0, i), c.slice(i + 1)];
  }))
})
```

Dán kết quả vào ô **"Thêm account"** trên UI.

---

## Dùng API

### curl

```bash
curl http://127.0.0.1:8790/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "deepseek-web",
    "messages": [{"role": "user", "content": "Xin chào"}],
    "stream": false
  }'
```

### OpenAI Python SDK

```python
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:8790/v1",
    api_key="khong-can-neu-khong-dat-DS_API_KEY",
)

resp = client.chat.completions.create(
    model="deepseek-web-r1",      # hoặc "deepseek-web"
    messages=[{"role": "user", "content": "Giải thích borrow checker"}],
    extra_body={"thinking": True, "search": False},
)
print(resp.choices[0].message.content)
```

### Tham số mở rộng (ngoài chuẩn OpenAI)

| Field | Kiểu | Mặc định | Ý nghĩa |
|---|---|---|---|
| `thinking` | bool | `false` | Bật DeepThink (trả `reasoning_content`) |
| `search` | bool | `false` | Bật tìm kiếm thông minh của DS web |
| `new_thread` | bool | `false` | `true` = tạo conversation mới; `false` = chat tiếp thread hiện có |
| `tools` | array | – | Schema tool kiểu OpenAI; harness render thành hướng dẫn DSML và parse kết quả |

Model trả về tool-call sẽ nằm trong `choices[0].message.tool_calls` như bình thường.

---

## Biến môi trường

| Biến | Mặc định | Ý nghĩa |
|---|---|---|
| `DS_PORT` | `8787` | Cổng nghe (bind `127.0.0.1`) |
| `DS_STATEFUL` | 1 | nối tiếp hội thoại trong phiên web (chỉ gửi phần mới); 0 = luôn gửi đủ |
| `DS_CONTEXT_TOKENS` | `500000` | `context_length` báo cho client (web nhận được ≥500K token, đo thực tế; ~1,1M thì stream rỗng) |
| `DS_CONV_TTL` / `DS_CONV_MAX_CHARS` | 1800 / 1200000 | dọn phiên nhàn rỗi (s) / trần ngữ cảnh một phiên (ký tự) |
| `DS_DEFAULT_EFFORT` | none | mức suy nghĩ khi client không chỉ định |
| `DS_API_KEY` | (trống) | Đặt thì `/v1/*` yêu cầu `Authorization: Bearer <key>` |
| `DS_ACCOUNTS_PATH` | `$HOME/.ds-harness/accounts.json` | File lưu account |
| `DS_MAX_CONC_PER_ACC` | `2` | Số call đồng thời tối đa mỗi acc (>2 dễ bị khoá) |
| `DS_QUEUE_WAIT_SECS` | `120` | Chờ acc rảnh trước khi trả 429 |
| `DS_SESSION_ROTATE` | `30` | Số lượt/session web trước khi đổi session mới |
| `DS_FAIL_COOLDOWN_MAX` | `600` | Trần cooldown (giây) cho acc hỏng liên tiếp |
| `DS_PROMPT_MAX_CHARS` | `1200000` | Vượt ngân sách thì cắt giữa tool_result cũ, rồi bỏ lượt cũ nhất |
| `DS_BURST_LIMIT` / `DS_BURST_WINDOW` | `6` / `30` | Giới hạn burst mỗi acc trong cửa sổ giây |
| `DS_COOLDOWN` | `10` | Cooldown cơ bản (giây) sau lỗi |
| `DS_CHROME_BIN` | tự dò | Đường dẫn Chrome/Chromium |
| `DS_LOGIN_CDP_PORT` | `9777` | Cổng CDP riêng cho Chrome login |
| `DS_LOGIN_PROFILE` | `$HOME/.ds-harness/login-profile` | Profile Chrome cố định khi login (giữ fingerprint ổn định) |
| `DS_RESET_LOGIN_PROFILE` | – | `1` = xoá profile login trước khi mở |
| `DS_DEBUG_SSE` | – | Bật dump SSE thô để debug |

---

## HTTP endpoints

| Method | Path | Mô tả |
|---|---|---|
| `GET` | `/` | Web UI quản lý account |
| `GET` | `/api/accounts` | Danh sách acc |
| `POST` | `/api/accounts` | Thêm acc (`{token, cookies}`) |
| `DELETE` | `/api/accounts/{id}` | Xoá acc |
| `POST` | `/api/accounts/{id}/verify` | Verify token còn sống |
| `POST` | `/api/accounts/{id}/pool` | Bật/tắt acc trong pool |
| `POST` | `/api/capture` | Quét tab DS trong Chrome |
| `POST` | `/api/login_start` | Bắt đầu flow login tự động |
| `POST` | `/api/build_profile` | Nuôi profile Chrome giống user thật (warm-up) |
| `GET` | `/api/stats` | `in_flight`, `fail_streak`, cooldown từng acc |
| `POST` | `/v1/chat/completions` | OpenAI-compatible chat |
| `GET` | `/v1/models` | Danh sách model |

**Bảo mật**: `/` và `/api/*` **luôn chỉ cho loopback** (kể cả khi bind ra ngoài). `/v1/*` mở cho mọi IP nhưng nên đặt `DS_API_KEY`.

---

## Kiến trúc code

```
src/
├── main.rs            # CLI: serve | whoami | chat | chatraw
├── server.rs          # axum router, pool account, UI, parser DSML, dựng prompt
├── chat.rs            # /v1/chat/completions — một lõi run_chat cho cả stream & non-stream
├── client.rs          # HTTP + SSE tới chat.deepseek.com, event stream
├── chrome_capture.rs  # CDP: login, capture tab, refresh cookie
├── pow.rs             # Keccak-f[1600] + DeepSeekHashV1 proof-of-work
└── ui.html            # Web UI (nhúng vào binary)
worker/                # Cloudflare Worker proxy (tuỳ chọn)
deploy/                # systemd unit mẫu
```

### Ghi chú kỹ thuật đáng chú ý

- **Một đường parse tool-call duy nhất**: `parse_any_tool_calls` dùng chung cho stream và non-stream, xử lý cả DSML chuẩn lẫn biến thể XML mà model đôi khi sinh ra.
- **Guard markup**: chặn khối DSML lọt vào `content`/`reasoning_content` của client.
- **In-flight lease**: khoá giữ suốt vòng đời stream (kể cả khi client ngắt) → giới hạn đồng thời mỗi acc có hiệu lực thật.
- **Phát hiện stream cụt**: stall >90s hoặc lỗi mạng giữa chừng → `StreamEv::Truncated` → retry thay vì coi là xong.
- **PoW**: `DeepSeekHashV1` = SHA3-256 nhưng bỏ round 0 của keccak-f; port 1:1 từ [CJackHwang/ds2api](https://github.com/CJackHwang/ds2api).
- **Log chẩn đoán**: `[ds-timing]`, `[ds-stall]`, `[ds-stream-err]`, `[toolcall-unparsed]`, `[toolcall-guard]`, `[ds-fail]`.

---

## Cloudflare Worker proxy (tuỳ chọn)

`worker/ds-proxy.js` xoay request qua nhiều instance harness (nhiều IP/VPS), có rate-limit nhẹ và round-robin qua KV.

```bash
cd worker
npx wrangler deploy
npx wrangler secret put UPSTREAMS      # ["https://ds1.example","https://ds2.example"]
npx wrangler secret put WORKER_TOKEN   # bearer token cho client
```

---

## Giới hạn đã biết

- DeepSeek web **không phải** API — độ trễ cao hơn, có thể bị chặn/đổi endpoint bất cứ lúc nào.
- Tool-calling là **giả lập bằng prompt** (DSML), không phải native → độ chính xác thấp hơn API thật, model đôi khi sinh sai format.
- Bật >2 call đồng thời/acc dễ bị DS khoá tạm thời.
- Phần CDP phụ thuộc Chrome và chỉ ổn định trên Linux desktop.
- Không hỗ trợ vision/embedding.

---

## License

[MIT](LICENSE) © 2026 RuriMeiko

---

**Dùng với trách nhiệm. Ưu tiên mua API chính thức nếu bạn cần sự ổn định.**

## Model, ảnh, stateful
- **Một model**: `deepseek-web`. `reasoning_effort: none` = Nhanh (flash), `max` (hoặc mức khác) = DeepThink. Alias cũ `deepseek-web-r1` và hậu tố `deepseek-web:max` vẫn nhận.
- **Ảnh**: nhận `image_url` (data URL/http), `image`/base64 (kiểu Anthropic), kể cả trong kết quả tool. Ảnh được upload lên DS (`/file/upload_file`, PoW riêng) và gắn qua `ref_file_ids`; cache theo acc.
- **Stateful**: sau mỗi lượt trả lời harness ghi dấu vân tay của prefix + id tool-call đã cấp. Request kế tiếp có prefix khớp chỉ gửi phần mới, nối bằng `parent_message_id` vào đúng phiên/acc. Không khớp, acc bận, đổi tools hay phiên quá dài thì tự gửi đủ ngữ cảnh. Phiên nhàn rỗi bị xoá sau `DS_CONV_TTL`.
- **Token**: chuỗi tĩnh (không xoay); `code 40003` = hết hạn/đăng xuất → acc bị cách ly 30 phút, báo "cần login lại". Cookie `aws-waf-token` được làm mới từ Chrome mỗi 5 phút. Nút **Reset** xoá lỗi/cooldown/phiên và kiểm tra lại token.

## Token & tương thích API
- `usage` lấy từ `accumulated_token_usage` thật của web (cộng dồn theo phiên): `prompt_tokens` = tổng − output lượt này; `cached_tokens` = phần ngữ cảnh đã nằm sẵn trong phiên (nối tiếp stateful). Không có số thật thì ước lượng theo loại ký tự (`est_tokens`, sai lệch <25% so với đo thực: Anh ~4,1 ký tự/token, code ~2,5, tiếng Việt ~1,9).
- 429 có header `Retry-After`; SSE có keep-alive 10s; lỗi upstream trả `error` rõ ràng.
- **Không hỗ trợ** (web không có): `temperature`, `top_p`, `max_tokens`, `stop`, `tool_choice`, `response_format`, `n>1`. Chúng bị bỏ qua.
- `/api/stats` → `pool.stateful`: `hits`/`misses` (tỉ lệ nối tiếp phiên), số hội thoại và phiên đang mở.

## Cấu hình dsh

dsh đọc danh sách model từ file patch của profile, **không** tự lấy từ `/v1/models`, nên đổi harness xong phải cập nhật file này (nếu không dsh giữ `contextWindow` và model cũ).

Tự động (idempotent, có backup `.bak.<giờ>`):

```bash
deploy/setup-dsh.sh                       # profile ~/.dsh/profiles/web, harness http://127.0.0.1:8790/v1
deploy/setup-dsh.sh ~/.dsh/profiles/web http://127.0.0.1:8790/v1
DS_CONTEXT_TOKENS=300000 deploy/setup-dsh.sh   # đổi cửa sổ ngữ cảnh báo cho dsh
```

**Mức suy nghĩ:** `none` = Nhanh (flash); `max` = DeepThink mọi bước; `auto` = DeepThink ở lượt người dùng nhưng tắt khi chỉ nối tiếp sau kết quả công cụ (vòng lặp tool nhanh gấp nhiều lần: bước có think 5–25K ký tự mất 10–44s, bước không think ~2s). Trong dsh: off→none, low→auto, max→max.

Cần PyYAML. Script thêm provider `ds-harness` (1 model `deepseek-web`, nhận ảnh, ctx 500K, `reasoningEfforts` off→none, low→auto, max→max, usage trong stream) và đặt làm model mặc định. Mở lại dsh rồi chọn **DeepSeek Web (harness, free)**.

Làm tay: trong `~/.dsh/profiles/web/cordis.patch.yml`, mục `llm-pi-ai` → `providers.ds-harness.models` chỉ giữ:

```yaml
- id: deepseek-web
  name: DeepSeek Web (harness, free)
  contextWindow: 500000
  maxTokens: 32768
  input: [text, image]
  reasoningEfforts: {off: none, low: auto, max: max}
```

kèm `compat: {supportsDeveloperRole: false, supportsReasoningEffort: true, supportsUsageInStreaming: true}`, và `agent-default-model` → `model: deepseek-web`. Nếu harness đặt `DS_API_KEY`, khai báo biến môi trường `DS_HARNESS_API_KEY` trong `~/.dsh/.env`.

`contextWindow` nên ≤ `DS_CONTEXT_TOKENS` của harness (web nhận ≥500K token, đo thực tế; ~1,1M thì trả stream rỗng). Muốn dsh nén sớm hơn (model dài hay lú) thì hạ xuống 200–300K.

## Token, chi phí và giá API

Dashboard (`/`) đếm token mỗi lượt hoàn tất (input không trúng cache / trúng cache / output gồm cả suy nghĩ) theo giờ UTC, lưu `usage.json` cạnh `accounts.json` (giữ 400 ngày). Số liệu bắt đầu tính từ lúc server có tính năng này.

- Giá: lấy **live** từ `api-docs.deepseek.com/quick_start/pricing`, làm mới 6 giờ/lần, lấy model **rẻ nhất** mỗi hàng (hiện là `deepseek-flash`). Không tải được thì dùng bảng giá dự phòng và dashboard ghi rõ.
- Cao điểm theo trang giá: thứ 2–6, 01:00–04:00 và 06:00–10:00 UTC, trừ ngày nghỉ lễ Trung Quốc; giá cao điểm gấp đôi. Danh sách nghỉ lễ 2026 trong code là ước lượng; thêm ngày khác bằng `DS_CN_HOLIDAYS=2026-12-31,2027-01-01`.
- "Tiết kiệm" = chi phí nếu trả tiền API cho đúng lượng token đó. Cache miss tính đủ giá nên thiên về phía cao (API thật có cache theo prefix tự động).
- Dự toán tháng = bình quân ngày của tối đa 7 ngày gần nhất × 30.
- `GET /api/usage` trả toàn bộ số liệu dạng JSON.

## Cấu hình trên dashboard

Các giá trị `DS_*` hay chỉnh (stateful, dự đoán PoW, TTL phiên, trần ngữ cảnh, đồng thời mỗi acc, cooldown, múi giờ…) đổi được ngay trong mục **Cấu hình** của dashboard, có hiệu lực **không cần restart** (trừ khoản ghi "cần restart"). Hai nút ở đầu trang bật/tắt nhanh *stateful* và *dự đoán pow*.

- Lưu ở `settings.json` cạnh `accounts.json`; nút ↺ trả khoá về mặc định.
- Thứ tự ưu tiên: giá trị đã lưu trên dashboard → biến môi trường `DS_*` → mặc định.
- API: `GET /api/settings` (danh sách, kiểu, khoảng hợp lệ), `POST /api/settings` với `{"DS_STATEFUL":"0","DS_CONV_TTL":600}`; `null` = bỏ giá trị đã lưu. Chỉ gọi được từ loopback như mọi `/api/*`.
- `DS_API_KEY`, đường dẫn và cổng cố ý không nằm ở đây.

## Thêm tài khoản thủ công

Trên dashboard, mục **Thêm tài khoản thủ công**: mở tab đã đăng nhập chat.deepseek.com, F12 → Console → `copy(localStorage.userToken)`, dán vào ô rồi bấm Thêm. Nhận cả dạng `{"value":"…","__version":"0"}`, chuỗi có nháy, `Bearer …` hoặc token trần. Token được kiểm tra với DeepSeek trước khi lưu; trùng token thì báo đã có. Không cần cookie. Cũng gọi được bằng `POST /api/accounts` với `{"token":"…","label":"tuỳ chọn"}`.
