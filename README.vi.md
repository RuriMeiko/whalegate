<p align="center">
  <img src="docs/banner.svg" alt="whalegate — cổng OpenAI-compatible vào DeepSeek web" width="100%">
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust">
  <img alt="OpenAI compatible" src="https://img.shields.io/badge/API-OpenAI--compatible-412991">
  <img alt="Tool calling" src="https://img.shields.io/badge/tool__calls-native--style-2ea44f">
  <img alt="Vision" src="https://img.shields.io/badge/vision-images-blue">
  <img alt="License" src="https://img.shields.io/badge/license-MIT-lightgrey">
</p>

<p align="center"><a href="README.md">English</a> · <b>Tiếng Việt</b> · <a href="README.zh-CN.md">简体中文</a></p>

**whalegate** biến các phiên `chat.deepseek.com` của chính bạn thành một endpoint `/v1/chat/completions` chuẩn kiểu OpenAI. Nó gom nhiều tài khoản vào một pool, tự chuyển sang tài khoản khác khi lỗi, stream qua SSE, đọc được ảnh, và dịch `tools` của OpenAI qua lại với định dạng DSML của DeepSeek. Các agent như Cline, Continue, LiteLLM, SDK của OpenAI và `dsh` dùng được nó như API thật.

> [!WARNING]
> **Việc nghiêm túc hãy dùng API chính thức.** [platform.deepseek.com](https://platform.deepseek.com/) rẻ, có SLA và không đặt tài khoản của bạn vào rủi ro. whalegate sinh ra để học hỏi, nghiên cứu giao thức và tự động hoá cá nhân trên **tài khoản của chính bạn**. Tự động hoá ứng dụng web có thể vi phạm Điều khoản dịch vụ của DeepSeek và khiến tài khoản bị khoá. Đừng bán lại hay chia sẻ quyền truy cập. Bạn tự chịu rủi ro khi sử dụng ([MIT](LICENSE), không bảo hành).

---

<a id="contents"></a>
## Mục lục

- [Tính năng](#features)
- [Cách hoạt động](#how-it-works)
- [Bắt đầu nhanh](#quick-start)
- [Thêm tài khoản](#adding-accounts)
- [Dùng API](#using-the-api)
- [Gọi tool](#tool-calling)
- [Ảnh (vision)](#images-vision)
- [Mức suy nghĩ](#reasoning-effort)
- [Hội thoại stateful](#stateful-conversations)
- [Pool tài khoản & failover](#account-pool--failover)
- [IP đầu ra theo từng tài khoản (proxy)](#egress-ip-per-account-proxies)
- [Dashboard](#dashboard)
- [Chuyển tài khoản giữa các máy](#moving-accounts-between-machines)
- [Cập nhật](#updating)
- [Cấu hình](#configuration)
- [HTTP endpoints](#http-endpoints)
- [Dùng với dsh (chế độ PTC)](#using-with-dsh-ptc-mode)
- [Cloudflare Worker fan-out](#cloudflare-worker-fan-out-optional)
- [Cấu trúc dự án](#project-layout)
- [Hạn chế](#limitations)

---

<a id="features"></a>
## Tính năng

| | |
|---|---|
| 🔌 **Tương thích OpenAI** | `POST /v1/chat/completions` (stream và không stream) và `GET /v1/models`. Phản hồi theo đúng dạng OpenAI, gồm cả `usage` và `cached_tokens` |
| 🛠️ **Gọi tool** | Hỗ trợ `tools`, `tool_choice` (`auto` / `none` / `required` / chỉ định tên) và `parallel_tool_calls`. Tham số được kiểm tra theo JSON Schema của bạn, và gateway tự thử lại kèm lời nhắc sửa đúng chỗ lỗi trước khi một lệnh gọi hỏng kịp tới client |
| 🖼️ **Vision** | Nhận `image_url` (data URL hoặc http), `image` base64 kiểu Anthropic, và ảnh nằm trong kết quả tool. Ảnh được tải lên DeepSeek và cache theo từng tài khoản |
| 🧠 **DeepThink** | `reasoning_effort` ánh xạ sang `none` / `auto` / `max`. Phần suy luận được stream dưới dạng `reasoning_content` |
| 🐳 **Pool tài khoản** | Xếp lịch theo tài khoản ít bận nhất, có trần đồng thời cho mỗi tài khoản, bộ giới hạn burst và thời gian nghỉ tăng dần |
| 🔁 **Failover** | Lỗi, stream rỗng hoặc bị cắt cụt sẽ chuyển request sang tài khoản khác (tối đa 3 lần thử) |
| 🧵 **Stateful** | Tiếp tục đúng hội thoại web cũ và chỉ gửi phần tin nhắn mới, nên vòng lặp agent dài vẫn nhanh và rẻ |
| 🔐 **Proof-of-work** | Có sẵn bộ giải `DeepSeekHashV1`, kèm tuỳ chọn giải trước cho request kế tiếp |
| 🌐 **Lấy phiên đăng nhập** | Bấm một lần bookmarklet là thêm được tài khoản với vân tay của chính trình duyệt bạn, hoặc đăng nhập qua một Chrome riêng có vân tay riêng và cookie thiết bị tự làm mới |
| 📦 **Cài một dòng** | Binary tĩnh dựng sẵn cho Linux (x86_64/arm64), macOS và Windows. Trình cài đặt khởi động gateway mỗi lần đăng nhập và cấu hình luôn dsh |
| 🔄 **Tự cập nhật** | Dashboard kiểm tra GitHub Releases, bấm một lần là tải, kiểm tra checksum, cài và khởi động lại sang bản mới |
| 📦 **Tài khoản mang đi được** | Export pool ra file hoặc JSON trong clipboard và import sang máy khác; gộp lại bao nhiêu lần cũng cho cùng kết quả |
| 📊 **Dashboard** | Tài khoản, log trực tiếp, lượng token kèm "nếu trả API thì tốn bao nhiêu", độ trễ p50/p90, cài đặt và khởi động lại êm ái |
| 🌍 **Đa ngôn ngữ** | Dashboard có tiếng Việt, English và 简体中文 |

---

<a id="how-it-works"></a>
## Cách hoạt động

```mermaid
flowchart LR
    subgraph Clients
        A1[OpenAI SDK]
        A2[Cline / Continue]
        A3[dsh agent]
        A4[LiteLLM]
    end

    subgraph WG["whalegate (Rust · axum)"]
        direction TB
        R["/v1/chat/completions"]
        P["Prompt builder<br/>messages + tools → DSML"]
        S["Scheduler<br/>least-busy · cooldown · burst"]
        X["Output parser<br/>DSML → tool_calls · schema check"]
        POW["PoW solver<br/>DeepSeekHashV1"]
        R --> P --> S
        S --> POW
        X --> R
    end

    subgraph Pool["Account pool"]
        B1[(acc 1)]
        B2[(acc 2)]
        B3[(acc n)]
    end

    DS["chat.deepseek.com<br/>/api/v0"]

    A1 & A2 & A3 & A4 -->|OpenAI JSON| R
    S --> B1 & B2 & B3
    B1 & B2 & B3 -->|token + cookies| DS
    DS -->|SSE| X
```

### Vòng đời của một request

```mermaid
sequenceDiagram
    autonumber
    participant C as Client
    participant G as whalegate
    participant A as Account pool
    participant D as DeepSeek web

    C->>G: POST /v1/chat/completions (messages, tools, stream)
    G->>G: fit context, render tools spec + history as one prompt
    G->>G: fingerprint prefix → known web conversation?
    G->>A: lease an account (least busy, not cooling down)
    A-->>G: acc + session
    G->>D: create PoW challenge
    D-->>G: challenge
    G->>G: solve DeepSeekHashV1
    G->>D: completion (prompt or delta, parent_message_id, ref_file_ids)
    loop SSE
        D-->>G: thinking / response chunks
        G-->>C: reasoning_content / content deltas<br/>(DSML markup held back, never leaked)
    end
    D-->>G: done (+ accumulated token usage)
    G->>G: parse tool calls → validate against schema
    alt valid
        G-->>C: tool_calls + finish_reason + usage + [DONE]
    else invalid / unparsable
        G->>D: retry with a correction note (≤ 3 attempts, failover on errors)
    end
```

---

<a id="quick-start"></a>
## Bắt đầu nhanh

### Cài đặt (khuyên dùng)

**Linux / macOS**
```sh
curl -fsSL https://raw.githubusercontent.com/RuriMeiko/whalegate/main/install.sh | sh
```

**Windows** (PowerShell)
```powershell
irm https://raw.githubusercontent.com/RuriMeiko/whalegate/main/install.ps1 | iex
```

```mermaid
flowchart LR
    I["install script"] --> D["download binary<br/>for your OS/CPU<br/>+ verify SHA-256"]
    D --> S["start in background<br/>+ on every login"]
    S --> C{"dsh installed?"}
    C -- yes --> W["write dsh config<br/>(provider, model, PTC)"]
    C -- no --> O
    W --> O["open http://127.0.0.1:8790/<br/>→ add an account"]
```

| | Linux | macOS | Windows |
|---|---|---|---|
| Binary | `~/.local/bin/whalegate` | `~/.local/bin/whalegate` | `%LOCALAPPDATA%\whalegate\whalegate.exe` |
| Chạy bằng | unit `systemd --user` tên `whalegate` | launchd agent `io.github.rurimeiko.whalegate` | mục Startup ẩn |
| Log | `journalctl --user -u whalegate -f` | `~/Library/Logs/whalegate.log` | dashboard → **log** |

Các tuỳ chọn của trình cài đặt là biến môi trường: `WHALEGATE_VERSION=v0.4.0`, `WHALEGATE_PORT=8790`, `WHALEGATE_NO_SERVICE=1` (chỉ cài binary), `WHALEGATE_NO_DSH=1`. Chạy lại trình cài đặt là nâng cấp tại chỗ. Thích bấm chuột hơn? Tải một gói từ [Releases](https://github.com/RuriMeiko/whalegate/releases) rồi chạy `whalegate` (hoặc bấm đúp `whalegate.exe`).

### Build từ source

Cần Rust ≥ 1.85, cùng `cmake` và `clang` (wreq build BoringSSL; trên Windows cần thêm NASM).

```bash
git clone https://github.com/RuriMeiko/whalegate.git ~/whalegate
cd ~/whalegate
cargo build --release          # → target/release/whalegate
./target/release/whalegate     # dashboard: http://127.0.0.1:8790/

# dịch vụ nền cho bản source đặt ở ~/whalegate
mkdir -p ~/.config/systemd/user && cp deploy/whalegate.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now whalegate
```

### CLI

| Lệnh | Tác dụng |
|---|---|
| `whalegate` / `whalegate serve` | Chạy gateway (mặc định) |
| `whalegate setup-dsh [--all] [--preset ptc\|standard] [--profile <dir>]... [--url <base>] [--context <tokens>]` | Trỏ dsh về gateway này (`--all` = mọi profile trên máy) |
| `whalegate version` | In phiên bản |
| `whalegate whoami` | In user hiện tại của tài khoản đầu tiên trong kho |
| `whalegate chat "<prompt>"` | Gửi một prompt thô và in luồng SSE |
| `whalegate chatraw "<prompt>" <thinking> <search>` | Như trên, có thêm công tắc DeepThink và search, dùng để gỡ lỗi |

---

<a id="adding-accounts"></a>
## Thêm tài khoản

Tài khoản được lưu trong `~/.whalegate/accounts.json`, có thể đổi bằng `DS_ACCOUNTS_PATH`. Nâng cấp từ ds-harness thì `~/.ds-harness` được chuyển sang đây tự động. File này chứa **token phiên đang sống**: đừng bao giờ commit hay chia sẻ.

Với DeepSeek, mỗi tài khoản là một "máy": vân tay TLS/HTTP2 (một bản Chrome/Edge thật được wreq mô phỏng), user agent, device id, cookie, múi giờ và ngôn ngữ. Tất cả giữ cố định suốt vòng đời tài khoản, vì một phiên đang sống bỗng xuất hiện trên một máy khác chính là thứ bị đánh dấu. Có bốn cách thêm tài khoản (các tab trong **+ Thêm**, theo đúng thứ tự này), và chúng khác nhau ở chỗ tài khoản sống trên *máy nào*.

| | Máy mà tài khoản dùng | Làm mới cookie |
|---|---|---|
| **Chrome riêng** | Một Chrome riêng mới, có vân tay và profile tự sinh | Tự động, mỗi `DS_DEVICE_REFRESH_HOURS` (mặc định 6h) |
| **Bookmarklet** | Trình duyệt của bạn; `ds_session_id` là của riêng whalegate, không phải của trình duyệt | Bấm lại bookmarklet trên một tab DeepSeek |
| **Tiện ích** | Trình duyệt của bạn, kèm mọi cookie, cả HttpOnly | Bấm lại tiện ích trên một tab DeepSeek |
| **Dán token** (legacy) | Suy ra từ token (cố định, nhưng không phải trình duyệt thật) | Không có |

### Cách 1: Chrome riêng (khuyên dùng)

```mermaid
flowchart LR
    A["Dashboard → Chrome riêng:<br/>Mở cửa sổ riêng để đăng nhập / đăng ký"] --> B["New seed → new fingerprint<br/>+ profile ~/.whalegate/profiles/&lt;seed&gt;"]
    B --> C["Dedicated Chrome on a random<br/>CDP port, fingerprint applied to every tab"]
    C --> D["You sign in<br/>(Google / email / passkey)"]
    D --> E["Gateway watches the tab (up to 10 min)<br/>→ token + device id + cookies"]
    E --> F["Save, close Chrome<br/>(profile is kept)"]
    F --> G["Every DS_DEVICE_REFRESH_HOURS:<br/>reopen that Chrome headless → fresh cookies"]
```

- Đăng nhập **hoặc đăng ký** ngay trong cửa sổ đó. Tài khoản DeepSeek mới tinh cũng được: gateway chỉ chờ tab có token. Đây là cách khuyên dùng khi đăng nhập bằng mật khẩu, vì không có gì được gõ vào dashboard. Dashboard cũng có mục gập **Tự điền email + mật khẩu** để điền form hộ bạn; cách này kém ổn định hơn vì trang đăng nhập của DeepSeek hay đổi.
- Đóng cửa sổ trước khi đăng nhập xong thì việc đăng nhập bị huỷ. Mỗi lúc chỉ mở được một cửa sổ đăng nhập.
- Khi chạy dưới systemd, gateway lấy `DISPLAY` / `WAYLAND_DISPLAY` từ phiên người dùng để cửa sổ mở lên được.
- Nếu cùng một user DeepSeek đăng nhập lại, token của tài khoản đang có được thay chứ không tạo bản trùng. Tài khoản chuyển sang thiết bị Chrome mới, và hồ sơ bookmarklet cũ (nếu có) bị bỏ.
- Nút **thiết bị** trên một tài khoản làm mới cookie thiết bị của nó ngay lập tức.
- **Quét Chrome chính** (trong tab Chrome riêng) lấy mọi tab `chat.deepseek.com` đang đăng nhập từ một Chrome chạy với `--remote-debugging-port=9333`. Từ Chrome 136 cờ này bị bỏ qua ở profile mặc định, nên chỉ chạy được với `--user-data-dir` riêng; với Chrome dùng hằng ngày hãy dùng tiện ích.

### Cách 2: bookmarklet

```mermaid
sequenceDiagram
    participant DS as chat.deepseek.com tab<br/>(logged in)
    participant D as whalegate dashboard tab
    participant G as gateway
    Note over DS: you click the bookmark
    DS->>D: window.open(dashboard#bm)
    DS->>DS: read userToken, device id, cookies,<br/>UA, screen, GPU, timezone, languages
    D->>DS: postMessage "ready"
    DS->>D: postMessage(bundle)<br/>(only to the dashboard origin)
    Note over D: accepts only from its opener<br/>with origin chat.deepseek.com
    D->>G: POST /api/accounts {token, cookies, browser}
    G->>G: bind the browser's fingerprint
    G->>DS: verify (users/current) with that fingerprint
    G->>G: same token or same DeepSeek user already in pool?<br/>update that account : add a new one
    G-->>D: saved (or error, fingerprint restored)
```

1. Trên dashboard, mở **+ Thêm** rồi kéo **Thêm vào whalegate** lên thanh bookmark (chỉ một lần). Bookmarklet trỏ về đúng địa chỉ dashboard bạn đang mở, nên nếu whalegate chạy trên máy khác, hãy mở dashboard bằng địa chỉ của máy đó rồi mới kéo.
2. Trên một tab `chat.deepseek.com` đã đăng nhập, bấm bookmark. Một tab dashboard mở ra và tài khoản được thêm.

Gói dữ liệu không bao giờ đi qua URL, lịch sử trình duyệt hay mạng: nó đi từ tab sang tab bằng `postMessage`, và dashboard chỉ nhận từ tab `chat.deepseek.com` đã mở nó, nên trang khác không đẩy được tài khoản vào pool của bạn. Sau đó tài khoản dùng đúng máy của trình duyệt bạn: bản Chrome/Edge của wreq gần nhất với phiên bản trình duyệt (trình duyệt mới hơn bảng thì cả vân tay được kéo về bản gần nhất), hệ điều hành, device id, múi giờ và ngôn ngữ của bạn trong `x-client-timezone-offset` / `Accept-Language`. Gateway không bao giờ mở một Chrome riêng cho các tài khoản này. Cookie DeepSeek đặt trong phản hồi được giữ lại tự động, nhưng cookie WAF (`aws-waf-token`) chỉ trình duyệt thật mới làm mới được. Để làm mới, bấm lại bookmarklet trên một tab DeepSeek: tài khoản đã có trong pool được cập nhật tại chỗ (gộp cookie, làm mới thiết bị và hồ sơ trình duyệt) thay vì thêm hai lần. Nếu bạn đăng nhập lại và có token mới, cùng một cú bấm đó sẽ thay token mới vào tài khoản đang có.

Nếu bookmarklet không chạy được, bộ thu thập y hệt có ở **Dán token**: copy lệnh console, chạy trong DevTools console của tab DeepSeek rồi dán kết quả.

### Cách 3: tiện ích trình duyệt

Tiện ích làm đúng việc của bookmarklet, và đọc thêm được cookie HttpOnly như `ds_session_id`. JavaScript của trang không bao giờ đọc được những cookie đó, nhưng một tiện ích có quyền `cookies` thì đọc được, giống như CDP làm với đăng nhập Chrome.

> **Về `ds_session_id`.** Nó là `HttpOnly; Secure; SameSite=Strict` và không có hạn dùng. Web server của DeepSeek gán nó ở phản hồi đầu tiên ngoài `/api` (favicon hoặc một trang) khi request chưa mang theo; `/api/v0/*` thì không bao giờ gán. Đăng nhập bằng Google sẽ cấp cái mới, vì cookie strict không được gửi khi Google chuyển hướng về. Tài khoản thêm bằng bookmarklet hay dán token không đọc được giá trị của trình duyệt, nên whalegate tự lấy cái riêng theo cách trình duyệt vẫn làm: một lần `GET /favicon.ico` qua client của tài khoản lúc thêm, và một lần lúc khởi động với các tài khoản cũ còn thiếu.

1. Ở **+ Thêm → Tiện ích**, bấm **Tải tiện ích** rồi giải nén. Bản tải về đã trỏ sẵn đúng dashboard bạn đang mở.
2. Mở `chrome://extensions`, bật **Developer mode**, bấm **Load unpacked** và chọn thư mục `whalegate-extension` vừa giải nén.
3. Trên một tab `chat.deepseek.com` đã đăng nhập, bấm biểu tượng whalegate. Tài khoản được thêm (hoặc làm mới nếu đã có trong pool), kết quả hiện ngay trên tab DeepSeek.

Tiện ích chỉ đọc được cookie của `chat.deepseek.com` và chỉ nói chuyện với host của dashboard. Nó gửi token, device id, cookie và hồ sơ trình duyệt thẳng tới `/api/accounts`, cùng một request với bookmarklet.

### Cách 4: dán token (legacy)

Trên một tab `chat.deepseek.com` đã đăng nhập, mở DevTools → Console và chạy:

```js
copy(localStorage.userToken)
```

Dán kết quả vào **+ Thêm → Dán token**. Nhận mọi dạng sau: `{"value":"…","__version":"0"}`, chuỗi có nháy, `Bearer …`, hoặc token trần. Token được kiểm tra trước khi lưu, còn cookie là tuỳ chọn. Bạn cũng có thể thêm qua API, kèm hồ sơ `browser` nếu muốn (các trường của bookmarklet: `user_agent`, `cores`, `memory_gb`, `screen`, `avail`, `dpr`, `tz`, `tz_offset`, `langs`, `gpu_vendor`, `gpu_renderer`, `device_id`):

```bash
curl -X POST http://127.0.0.1:8790/api/accounts \
  -H 'content-type: application/json' -d '{"token":"<token>","label":"main"}'
```

---

<a id="using-the-api"></a>
## Dùng API

Chỉ có một model là `deepseek-web`. Các alias cũ `deepseek-web-r1` và `deepseek-web:max` vẫn được chấp nhận.

### curl

```bash
curl http://127.0.0.1:8790/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "deepseek-web",
    "messages": [{"role": "user", "content": "Giải thích borrow checker trong 3 câu."}],
    "stream": true
  }'
```

### OpenAI Python SDK

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8790/v1", api_key="gì-cũng-được-trừ-khi-đặt-DS_API_KEY")

resp = client.chat.completions.create(
    model="deepseek-web",
    reasoning_effort="max",                      # none | auto | max
    messages=[{"role": "user", "content": "Chứng minh √2 là số vô tỉ."}],
)
print(resp.choices[0].message.reasoning_content)  # vết suy luận DeepThink
print(resp.choices[0].message.content)
```

### Trường bổ sung của request

Với các SDK, truyền chúng qua `extra_body`.

| Trường | Kiểu | Mặc định | Ý nghĩa |
|---|---|---|---|
| `reasoning_effort` | string | `DS_DEFAULT_EFFORT` | `none` = nhanh, `auto` = chỉ nghĩ ở lượt của người dùng, `max` = luôn nghĩ. `reasoning.effort` cũng dùng được |
| `thinking` | bool | `false` | Công tắc DeepThink kiểu cũ |
| `search` | bool | `false` | Tìm kiếm web của DeepSeek |
| `new_thread` | bool | `false` | Mở hội thoại web mới thay vì tiếp tục cái khớp |

### Lưu ý về tương thích

- `usage` lấy từ `accumulated_token_usage` thật của ứng dụng web. Khi thiếu, dùng số ước lượng theo từng hệ chữ, sai lệch khoảng 25% so với số đo thực. `prompt_tokens_details.cached_tokens` tính phần ngữ cảnh đã có sẵn trong hội thoại web.
- Lỗi `429` kèm `Retry-After`. SSE gửi keep-alive mỗi 10 giây. Lỗi phía upstream trả về đối tượng `error` rõ ràng.
- Ứng dụng web không có tương đương cho `temperature`, `top_p`, `max_tokens`, `stop`, `response_format` hay `n > 1`, nên các tham số này bị bỏ qua.

---

<a id="tool-calling"></a>
## Gọi tool

DeepSeek web không có API tool. whalegate mô phỏng bằng cách render `tools` của OpenAI sang đúng định dạng khối **DSML** mà model vốn dùng, rồi parse ngược lại:

```mermaid
flowchart TB
    T["OpenAI tools[] + tool_choice"] --> R["Tools spec in the prompt<br/>(types, enums, REQUIRED, nested schemas, defaults)"]
    H["History: assistant tool_calls<br/>+ role:tool results"] --> R2["Re-rendered as canonical DSML<br/>+ tool_result (name) [id]: …"]
    R & R2 --> M(("DeepSeek<br/>web model"))
    M --> O["Streamed reply"]
    O --> G{"Tool marker?"}
    G -- no --> TXT["content (streamed live)"]
    G -- yes --> PARSE["Normalize & parse DSML<br/>(tolerant of the model's typos)"]
    PARSE --> V{"Valid vs JSON Schema?"}
    V -- yes --> OUT["tool_calls[]<br/>finish_reason: tool_calls"]
    V -- no --> FIX["Retry with the bad output<br/>+ exact error as a correction note"]
    FIX --> M
```

Model viết gì, và client nhận được gì:

<table>
<tr><th>Đầu ra của model (DSML)</th><th>Client nhận</th></tr>
<tr><td>

```text
Let me check the repo.
<｜｜DSML｜｜ calls>
<｜｜DSML｜｜ invoke name="bash">
<｜｜DSML｜｜ parameter name="command" string="true">ls -la</｜｜DSML｜｜ parameter>
<｜｜DSML｜｜ parameter name="timeout" string="false">30</｜｜DSML｜｜ parameter>
</｜｜DSML｜｜ invoke>
</｜｜DSML｜｜ calls>
```

</td><td>

```json
{
  "content": "Let me check the repo.",
  "tool_calls": [{
    "id": "call_9f2c41ab07de",
    "type": "function",
    "function": {
      "name": "bash",
      "arguments": "{\"command\":\"ls -la\",\"timeout\":30}"
    }
  }]
}
```

</td></tr>
</table>

### Điều gì làm nó cư xử như một API thật

- **Không báo nhầm.** Lệnh gọi chỉ được trích ra khi request thực sự có `tools`. Các định dạng dự phòng (JSON, kiểu XML) chỉ được tính khi nằm ở cuối câu trả lời và gọi tên một tool bạn đã khai báo. HTML trong câu trả lời, hay một ví dụ `<bash command="…"/>` trong đoạn văn, vẫn là văn bản.
- **Kiểm tra schema.** Kiểu, `enum`, `required`, `properties` / `items` lồng nhau, `anyOf` / `oneOf`. Giá trị cũng được ép về đúng kiểu đã khai báo: `"50"` thành `50`, và `123` thành `"123"` với tham số kiểu chuỗi.
- **Thử lại để sửa.** Một lệnh gọi hỏng không bao giờ bị thử lại mù quáng. Lần thử kế tiếp kèm chính đầu ra của model cộng với vấn đề cụ thể, ví dụ `todo: "items[0].status" must be one of "todo"|"done"`, để model sửa đúng chỗ đó.
- **`tool_choice`.** `none` trả văn bản thường. `required` và chỉ định tên sẽ buộc phải gọi, và thử lại nếu model trả lời bằng văn xuôi. Chỉ định một tool chưa khai báo trả `400 invalid_request_error`.
- **`parallel_tool_calls: false`** chỉ giữ lại một lệnh gọi.
- **Markup không bao giờ lộ ra.** DSML bị giữ lại khỏi `content`, và bị gỡ khỏi `reasoning_content` khi model nháp lệnh gọi lúc đang suy nghĩ.
- **Lịch sử đi vòng được.** Các lệnh gọi trước được render lại bằng DSML chuẩn. Kết quả tool rỗng hiện là `(no output)`, để model không chạy lại một lệnh `mkdir` im lặng.

### Ví dụ: vòng lặp agent

```python
tools = [{
    "type": "function",
    "function": {
        "name": "bash",
        "description": "Run a shell command",
        "parameters": {
            "type": "object",
            "properties": {"command": {"type": "string"}, "description": {"type": "string"}},
            "required": ["command", "description"],
        },
    },
}]

messages = [{"role": "user", "content": "Ổ / còn trống bao nhiêu?"}]
while True:
    r = client.chat.completions.create(model="deepseek-web", messages=messages,
                                       tools=tools, reasoning_effort="auto")
    msg = r.choices[0].message
    messages.append(msg)
    if not msg.tool_calls:
        print(msg.content)
        break
    for call in msg.tool_calls:
        output = run(call)                       # bộ thực thi của bạn
        messages.append({"role": "tool", "tool_call_id": call.id, "content": output})
```

---

<a id="images-vision"></a>
## Ảnh (vision)

```python
client.chat.completions.create(model="deepseek-web", messages=[{
    "role": "user",
    "content": [
        {"type": "text", "text": "Ảnh chụp màn hình này sai ở đâu?"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0..."}},
    ],
}])
```

- Nhận `image_url` (data URL hoặc URL `http(s)` công khai; địa chỉ riêng tư và nội bộ bị từ chối), `{"type":"image","source":{…}}` kiểu Anthropic, và ảnh trả về **trong kết quả tool**, chẳng hạn từ tool `read_image`.
- Mỗi ảnh được tải lên qua endpoint file của DeepSeek (có PoW riêng) và gắn vào bằng `ref_file_ids`. Việc tải lên được cache theo từng tài khoản bằng hash nội dung.
- Mỗi request gửi tối đa 4 ảnh khác nhau gần nhất. Trần dung lượng mỗi ảnh là `DS_MAX_IMAGE_MB` (mặc định 20).

---

<a id="reasoning-effort"></a>
## Mức suy nghĩ

| `reasoning_effort` | DeepThink | Phù hợp với |
|---|---|---|
| `none` (cũng nhận `off`, `minimal`) | không bao giờ | Trả lời nhanh và vòng lặp tool đơn giản |
| `auto` | bật ở lượt của người dùng, **tắt** ở các bước chỉ nối tiếp sau kết quả `tool` | Agent. Bước có suy nghĩ mất ~10–44 giây còn bước không suy nghĩ ~2 giây, nên vòng lặp tool nhanh hơn nhiều lần |
| `max` (mọi giá trị khác) | mọi bước | Bài toán khó |

Khi client không gửi gì, áp dụng `DS_DEFAULT_EFFORT` (mặc định `none`). Request tạo tiêu đề từ client không bao giờ suy nghĩ.

---

<a id="stateful-conversations"></a>
## Hội thoại stateful

Agent gửi lại toàn bộ lịch sử ở mỗi bước. whalegate lấy vân tay của từng tiền tố hội thoại cùng các id tool-call đã cấp. Khi request kế tiếp mở rộng một tiền tố đã biết, chỉ các tin nhắn **mới** được gửi vào cùng hội thoại web đó, dùng `parent_message_id`.

```mermaid
sequenceDiagram
    participant C as Client
    participant G as whalegate
    participant D as DeepSeek web

    C->>G: step 1 — [system, user]
    G->>D: full prompt (new web conversation)
    D-->>G: tool_calls (ids issued: call_a)
    G->>G: remember fingerprint(prefix + call_a) → (acc, session, msg_id)
    C->>G: step 2 — [system, user, assistant(call_a), tool(call_a)]
    G->>G: prefix matches ✔
    G->>D: delta only: tool_result(call_a) → parent = msg_id
    Note over G,D: same account, same session, cached_tokens reported
```

Gateway quay lại gửi đầy đủ ngữ cảnh, trên bất kỳ tài khoản nào, khi tiền tố không khớp, tài khoản đang giữ bận quá 4 giây, tool thay đổi, hoặc hội thoại vượt `DS_CONV_MAX_CHARS`. Hội thoại web nhàn rỗi bị xoá sau `DS_CONV_TTL`. Đặt `DS_STATEFUL=0` để luôn gửi đầy đủ.

---

<a id="account-pool--failover"></a>
## Pool tài khoản & failover

```mermaid
stateDiagram-v2
    [*] --> Ready
    Ready --> Busy: lease (≤ DS_MAX_CONC_PER_ACC in flight)
    Busy --> Ready: success
    Busy --> Cooldown: error / empty / truncated stream
    Ready --> Burst: > DS_BURST_LIMIT calls in DS_BURST_WINDOW
    Burst --> Ready: after DS_COOLDOWN
    Cooldown --> Ready: 15s → 30s → 60s … (cap DS_FAIL_COOLDOWN_MAX)
    Busy --> NeedsLogin: code 40003 (token expired)
    NeedsLogin --> Ready: re-login / new token
    Ready --> Disabled: pool switch OFF
    Disabled --> Ready: pool switch ON
```

- Mỗi request có tối đa **3 lần thử**. Lỗi từ upstream chuyển sang tài khoản khác. Lỗi của model (một lệnh gọi tool hỏng) thì thử lại kèm lời nhắc sửa và không phạt tài khoản.
- Khi không còn tài khoản rảnh, request chờ tối đa `DS_QUEUE_WAIT_SECS` rồi trả `429` kèm `Retry-After`.
- Một stream đứng im 90 giây hoặc đứt giữa chừng (đã nhận chữ mà chưa có sự kiện `close`) được **nối lại trên chính tài khoản đó** trước, y như web: `POST /chat/resume_stream` tối đa 5 giây (mỗi lần chờ 3 giây, nghỉ 1 giây giữa các lần), rồi bấm "Tiếp tục" bằng `POST /chat/continue` kèm `fallback_to_resume` sau một nhịp ngẫu nhiên nhẹ. Chỉ phần client chưa thấy mới được chuyển tiếp. Nếu vẫn không được thì mới coi là cụt và chuyển sang tài khoản khác.
- Khi DeepSeek đòi xác minh captcha (`CHALLENGE_REQUIRED`), tài khoản nghỉ và gateway chờ ngẫu nhiên 2–5 giây rồi mới thử tài khoản khác, thay vì phát lại ngay cùng payload.
- Lease đang chạy giữ suốt cả stream, kể cả khi client ngắt kết nối, nên trần đồng thời giữ đúng thật sự.

---

<a id="egress-ip-per-account-proxies"></a>
## IP đầu ra theo từng tài khoản (proxy)

DeepSeek nhìn một tài khoản như một cặp `(IP, vân tay)`. Khi nhiều tài khoản cùng đi ra từ một IP, chúng trông như một người, nên whalegate có thể đưa mỗi tài khoản qua một proxy khác nhau.

Việc gắn là **theo vân tay (seed), không theo token**: hai tài khoản là cùng một "máy" (cùng vân tay) luôn dùng chung một IP, còn hai máy khác nhau thì không bao giờ chung. Các liên kết nằm trong `~/.whalegate/proxy-bindings.json`, nên tài khoản giữ nguyên IP qua các lần khởi động lại, và vẫn giữ khi bạn xoá rồi thêm lại.

```
DS_PROXY_MODE=strict
DS_PROXY_LIST='socks5h://user:pass@203.0.113.7:1080
http://198.51.100.4:8080'
```

Hoặc đặt cùng các dòng đó vào `~/.whalegate/proxies.txt`, hay dán vào **Cài đặt → Proxy** trên dashboard (danh sách được kiểm tra từng dòng; một dòng sai là từ chối cả lần lưu, nên không bao giờ có chuyện một nửa số tài khoản âm thầm đi thẳng).

Cách một proxy được gán:

1. Lần đầu thấy một vân tay, nó nhận proxy đang gánh ít vân tay nhất — để pool dàn đều thay vì dồn hết vào mục đầu tiên.
2. Liên kết đó được ghi xuống đĩa. Các request sau từ cùng vân tay luôn dùng nó.
3. Nếu proxy đã gắn biến mất khỏi danh sách, liên kết bị bỏ và chọn cái mới, thay vì để tài khoản kẹt.

Các chế độ: `off` (đi thẳng, mặc định) · `auto` (có proxy thì dùng; pool hỏng thì ghi log và đi thẳng) · `strict` (không có proxy dùng được thì từ chối bắt đầu request — dành cho lúc lộ IP thật tệ hơn một lệnh gọi thất bại).

Nên dùng `socks5h` thay vì `socks5` khi có thể: nó để proxy phân giải DNS, nên việc tra tên `chat.deepseek.com` cũng không diễn ra trên máy của bạn.

`GET /api/proxy` hiện pool hiện tại và mọi liên kết; `POST /api/proxy` thay danh sách; `POST /api/proxy/unbind` với `{"fingerprint":"…"}` thả một liên kết để tài khoản đó nhận IP khác ở lần sau.

<a id="dashboard"></a>
## Dashboard

Mở `http://127.0.0.1:8790/`. Chỉ truy cập được từ loopback.

- **Tài khoản:** thêm, verify, reset, sửa nhãn hoặc token, bật/tắt trong pool, verify/reset hàng loạt và phát hiện user trùng. **Verify** không chỉ là kiểm tra xác thực: nó gửi một completion thật tối thiểu trên một phiên dùng một lần, vì tài khoản đang bị throttle vẫn qua được endpoint xác thực. Kết quả là alive, throttled, auth_dead hoặc unknown, và chỉ alive mới gỡ nhãn throttle.
- **Mức dùng:** token theo giờ (cache miss / cache hit / output) và cùng lưu lượng đó sẽ tốn bao nhiêu trên API chính thức, hiển thị bằng $ (English), ¥ (tiếng Trung) hoặc ₫ (tiếng Việt) theo tỷ giá ở **Cài đặt → Hiển thị**. Giá được lấy trực tiếp từ trang giá của DeepSeek mỗi 6 giờ, có tính giờ cao điểm, và có dự phóng cả tháng. Bấm vào nhãn **Token**, **Tiết kiệm** hoặc biểu đồ để chuyển giữa hôm nay / 7 ngày / 30 ngày / toàn bộ (lựa chọn được nhớ). Dữ liệu nằm trong `usage.json` cạnh `accounts.json`.
- **Độ trễ:** p50/p90 trong giờ qua, kèm số lỗi.
- **Log trực tiếp:** luồng SSE có lọc và tìm kiếm. Các tag hữu ích gồm `[toolcall-invalid]`, `[toolcall-unparsed]`, `[conv]`, `[ds-stall]` và `[ds-fail]`. Mọi dòng log cũng được lưu ra `~/.whalegate/logs/whalegate-YYYYMMDD.log` (giữ 7 ngày, tối đa 64 MB mỗi file), còn sự kiện chi tiết vào `debug-YYYYMMDD.jsonl`: từng lần nối lại / tiếp tục, vài dòng SSE cuối của stream bị đứt, lỗi tài khoản như `CHALLENGE_REQUIRED`. Nút **export** trong cửa sổ log tải về một gói debug (phiên bản, cài đặt, tóm tắt từng tài khoản, cả hai loại log trong 24 giờ đến 7 ngày). Trong đó không có token, cookie hay nhãn tài khoản.
- **Cài đặt:** chỉ có các nút đã kiểm chứng. Lưu sẽ đợi tới khi không còn request nào đang chạy, rồi tiến trình tự re-exec (cùng PID, nạp bản build mới) và trang tự tải lại. **Hard reset** khởi động lại ngay.
- **Export / import:** chuyển cả pool tài khoản sang máy khác bằng file JSON hoặc thẳng qua clipboard. Import gộp theo user id của DeepSeek nên chạy hai lần vẫn an toàn; nó không bao giờ xoá tài khoản bạn đang có.
- **Ngôn ngữ:** Tiếng Việt / English / 简体中文 bằng công tắc quả cầu (VI / EN / 中) ở cuối thanh đầu trang. Lần đầu nó theo ngôn ngữ trình duyệt và nhớ lựa chọn của bạn. Xem [docs/i18n.md](docs/i18n.md).
- **Cập nhật:** dashboard kiểm tra GitHub Releases khi tải trang và mỗi 30 phút. Có tag mới hơn thì hiện một băng thông báo; bấm một lần là tải binary dựng sẵn, kiểm tra SHA-256, thay tại chỗ và re-exec, rồi trang tự tải lại khi tiến trình mới lên.

---
<a id="moving-accounts-between-machines"></a>
## Chuyển tài khoản giữa các máy

Pool nằm trong `~/.whalegate/accounts.json`, file này chứa token phiên — chính file là thông tin xác thực. Có hai cách mang nó đi, đều làm từ dashboard:

- **Export → Tải file** tải về `whalegate-accounts-<ts>.json`; chép sang máy kia rồi dùng **Import → chọn file**.
- **Export → Copy clipboard** đặt cùng JSON đó vào clipboard (nếu trình duyệt chặn API clipboard bất đồng bộ thì dùng một textarea ẩn thay thế); dán vào hộp thoại Import trên máy đích.

Import là gộp chứ không thay thế. Tài khoản được khớp theo user id của DeepSeek trước, rồi theo token, nên import cùng một gói hai lần thì lần thứ hai không thêm gì. Không bật *ghi đè* thì tài khoản đã có bị bỏ qua; bật thì token và cookie của chúng được thay (và các phiên web cũ bị xoá). Không có gì bị xoá cả.

**Sao lưu đầy đủ (cả máy).** Nút **Sao lưu đầy đủ** trong hộp Export tải về một file `whalegate-backup-<giờ>.tar.gz` gồm mọi thứ ở trên cộng với cài đặt dashboard, danh sách proxy và ghim vân tay→proxy, lịch sử dùng, và phần định danh của từng profile Chrome riêng (localStorage chứa đăng nhập và mã thiết bị, IndexedDB, cookie, tuỳ chọn: vài trăm KB mỗi cái, không có cache). Chọn đúng file đó ở **Import** sẽ có bản xem trước (tài khoản, profile, cài đặt, proxy, lịch sử) trước khi ghi gì. Import là gộp: tài khoản khớp theo user id của DeepSeek, còn thứ gì máy này đã có (cài đặt, danh sách proxy, ghim, giờ dùng, thư mục profile) thì giữ nguyên trừ khi bạn tick *ghi đè*. Không bao giờ đụng vào profile khi đang có cửa sổ đăng nhập mở. Chrome mã hoá cơ sở dữ liệu cookie bằng khoá gắn với hệ điều hành, nên máy khác hệ điều hành (hoặc khác người dùng) có thể không giải mã được cookie trong profile; đăng nhập và mã thiết bị (localStorage) cùng cookie của chính tài khoản lưu trong `accounts.json` vẫn sang đủ, và lần làm mới thiết bị kế tiếp sẽ làm mới phần còn lại. Gói được kiểm tra khi đọc: đường dẫn ra ngoài thư mục dữ liệu, link và nội dung quá lớn đều bị từ chối. Qua HTTP: `GET /api/backup/export` và `POST /api/backup/import?dry_run=1&overwrite=1` với file làm thân request.

> [!WARNING]
> Gói export chứa token và cookie của mọi tài khoản. Ai có nó đều dùng được các phiên đó. Hãy chuyển qua kênh bạn tin cậy, xoá file sau khi xong, và đừng bao giờ commit — `accounts.json` và `*.session` đã nằm sẵn trong `.gitignore`.

---

<a id="updating"></a>
## Cập nhật

`GET /api/update` đọc `https://api.github.com/repos/RuriMeiko/whalegate/releases/latest`, so với `CARGO_PKG_VERSION` đang chạy và trả `{current, latest, has_update, notes, url}`.

`POST /api/update` sau đó:

1. tải `whalegate-<target>.tar.gz` (`.zip` trên Windows) và `SHA256SUMS.txt` của tag mới nhất,
2. kiểm tra SHA-256 (tự cài sẵn trong mã, không thêm dependency),
3. giải nén binary, chép cạnh bản đang chạy rồi `rename` vào chỗ — nguyên tử, và an toàn khi inode cũ vẫn đang thực thi,
4. trả lời request HTTP, rồi re-exec để binary mới tiếp quản cùng PID.

Dashboard kiểm tra khi tải trang và mỗi 30 phút, hiện băng thông báo khi có tag mới hơn, và tải lại trang khi `/api/restart` báo một boot id mới. Request đang chạy bị cắt; các agent như dsh sẽ thử lại.

Windows khoá file `.exe` đang chạy nên bước `rename` ở bước 3 không làm được. Thay vào đó trình cập nhật chép binary mới thành `whalegate.exe.new` và ghi một `whalegate.update.bat` cạnh nó; đường khởi động lại chạy script này, script đợi tiến trình cũ thoát, thay file bằng `move /y`, chạy lại ứng dụng rồi tự xoá. Script chỉ chạy khi cả hai file còn đó, nên một lần cập nhật bị bỏ dở không bao giờ chạy lại.

Nếu bạn cài bằng `install.sh` / `install.ps1` thì không cần làm gì thêm — binary được thay tại chỗ và cấu hình dịch vụ không bị đụng tới. Muốn cập nhật từ shell thì chạy lại trình cài đặt:

```sh
curl -fsSL https://raw.githubusercontent.com/RuriMeiko/whalegate/main/install.sh | sh
```

---

<a id="configuration"></a>
## Cấu hình

Thứ tự ưu tiên: giá trị đã lưu trên dashboard → biến môi trường `DS_*` → mặc định. Các dòng đánh dấu ⚙️ sửa được trên dashboard.

| Biến | Mặc định | Ý nghĩa |
|---|---|---|
| `DS_PORT` | `8790` | Cổng lắng nghe (luôn bind `127.0.0.1`) |
| `DS_API_KEY` | – | Nếu đặt, `/v1/*` yêu cầu `Authorization: Bearer <key>`. Bắt buộc trước khi `/v1/*` trả lời bất kỳ thứ gì từ ngoài máy |
| `DS_ACCOUNTS_PATH` | `~/.whalegate/accounts.json` | Kho tài khoản. Cài đặt, mức dùng và mẫu lỗi nằm cạnh nó |
| ⚙️ `DS_STATEFUL` | `1` | Tiếp tục hội thoại web và chỉ gửi phần delta |
| ⚙️ `DS_POW_PREFETCH` | `1` | Tải và giải trước PoW kế tiếp |
| ⚙️ `DS_DEFAULT_EFFORT` | `none` | Mức suy nghĩ khi client không chỉ định |
| ⚙️ `DS_CONTEXT_TOKENS` | `512000` | `context_length` báo trong `/v1/models`. Ứng dụng web nhận ≥512K, còn ~1.1M trả stream rỗng |
| ⚙️ `DS_PROMPT_MAX_CHARS` | `1822000` | Quá ngân sách: cắt kết quả tool cũ trước, rồi bỏ các lượt cũ nhất |
| ⚙️ `DS_MAX_CONC_PER_ACC` | `2` | Số request đang chạy trên mỗi tài khoản. Hơn 2 dễ bị giới hạn tốc độ |
| ⚙️ `DS_TZ_OFFSET` | `25200` | Độ lệch múi giờ (giây) gửi cho ứng dụng web và dùng cho thống kê theo ngày |
| ⚙️ `DS_FX_CNY` | `720` | Tỷ giá khi dashboard ở tiếng Trung, tính bằng 0,01 ¥ cho mỗi USD (`720` = 7,20 ¥). Chi phí được tính gốc bằng USD rồi đổi khi hiển thị |
| ⚙️ `DS_FX_VND` | `26000` | Tỷ giá khi dashboard ở tiếng Việt: số đồng cho mỗi USD |
| ⚙️ `DS_PROXY_MODE` | `off` | IP đầu ra theo tài khoản: `off` đi thẳng · `auto` dùng proxy khi có, không thì đi thẳng · `strict` báo lỗi thay vì lộ IP thật |
| ⚙️ `DS_PROXY_LIST` | – | Danh sách proxy, mỗi dòng một cái: `scheme://[user:pass@]host:port` với `http`, `https`, `socks5` hoặc `socks5h` |
| `DS_PROXY_FILE` | `~/.whalegate/proxies.txt` | File danh sách proxy (cùng định dạng, cho phép chú thích `#`) |
| `DS_CONV_TTL` | `1800` | Xoá hội thoại web nhàn rỗi sau chừng này giây |
| `DS_CONV_MAX_CHARS` | `1200000` | Kích thước tối đa của một hội thoại stateful |
| `DS_SESSION_ROTATE` | `30` | Số lượt trên một phiên dùng chung (không tool) trước khi đổi phiên |
| `DS_QUEUE_WAIT_SECS` | `120` | Thời gian chờ tài khoản rảnh trước khi trả `429` |
| `DS_FAIL_COOLDOWN_MAX` | `600` | Trần của thời gian nghỉ tăng dần khi lỗi |
| `DS_BURST_LIMIT` / `DS_BURST_WINDOW` / `DS_COOLDOWN` | `6` / `30` / `10` | Bộ giới hạn burst theo tài khoản |
| `DS_MAX_IMAGE_MB` | `20` | Dung lượng tối đa mỗi ảnh |
| `DS_READ_TIMEOUT_SECS` | `120` | Timeout đọc socket (cần khởi động lại) |
| `DS_CHROME_BIN` | tự động | Đường dẫn Chrome/Chromium dùng để đăng nhập |
| `DS_LOGIN_CDP_PORT` | ngẫu nhiên 40000–59999 | Ép dùng một cổng CDP cố định cho Chrome đăng nhập riêng |
| `DS_DEVICE_REFRESH_HOURS` | `6` | Mở lại Chrome của từng tài khoản đăng nhập Chrome ở nền để lấy cookie thiết bị mới (`0` = tắt) |
| `DS_CN_HOLIDAYS` | – | Ngày lễ Trung Quốc bổ sung (`YYYY-MM-DD,…`) để tính giá giờ cao điểm |
| `DS_DEBUG_SSE` | `0` | Ghi SSE thô vào `/tmp/ds_debug_sse.log` |

---

<a id="http-endpoints"></a>
## HTTP endpoints

| Method | Path | Mô tả |
|---|---|---|
| `POST` | `/v1/chat/completions` | Chat tương thích OpenAI |
| `GET` | `/v1/models` | Danh sách model (`context_length`, `reasoning_efforts`, modalities) |
| `GET` | `/` | Dashboard |
| `GET` / `POST` | `/api/accounts` | Liệt kê / thêm tài khoản (`{token, label?, cookies?, browser?}`) |
| `PATCH` / `DELETE` | `/api/accounts/{id}` | Sửa nhãn hoặc token / xoá |
| `POST` | `/api/accounts/{id}/verify` · `/reset` · `/pool` | Thăm dò bằng một chat thật (trả `probe: alive\|throttled\|auth_dead\|unknown`) / xoá lỗi và phiên rồi thăm dò / `{"pool_enabled": bool}` |
| `POST` | `/api/accounts/{id}/device` | Làm mới cookie thiết bị ngay (tài khoản đăng nhập Chrome) |
| `POST` | `/api/capture` · `/api/login_start` | Lấy các tab đã đăng nhập từ Chrome chính / mở Chrome đăng nhập riêng |
| `GET` | `/api/stats` · `/api/usage` | Trạng thái pool (đang chạy, thời gian nghỉ, tỉ lệ trúng stateful) / mức dùng, chi phí và độ trễ |
| `GET` | `/api/logs` · `/api/logs/stream` | Log dạng JSON / SSE |
| `GET` / `POST` | `/api/settings` | Đọc hoặc cập nhật cài đặt (`null` đặt lại một khoá) |
| `GET` / `POST` | `/api/proxy` | Pool proxy đầu ra và liên kết vân tay→proxy / thay danh sách (`{"list": "…"}`) |
| `POST` | `/api/proxy/unbind` | Thả một liên kết (`{"fingerprint": "…"}`) để tài khoản đó nhận IP mới |
| `GET` / `POST` | `/api/dsh` | Trạng thái dsh (liệt kê mọi profile tìm thấy) / ghi cấu hình dsh (`{"preset": "ptc"\|"standard", "profiles": ["desktop","web"]}`; bỏ `profiles` hoặc đặt `"all": true` cho mọi profile) |
| `GET` / `POST` | `/api/restart` | Trạng thái / `{"mode": "idle" \| "now" \| "cancel"}` |
| `GET` | `/api/accounts/export` | Tải cả pool dạng gói JSON (chứa token — coi như bí mật) |
| `POST` | `/api/accounts/import` | Gộp một gói hoặc `{accounts: [...]}`. `dry_run` để xem trước, `overwrite` thay token của user đã biết |
| `GET` / `POST` | `/api/backup/export` · `/api/backup/import` | Sao lưu đầy đủ một file `.tar.gz` (tài khoản, cài đặt, proxy, ghim, lịch sử dùng, profile Chrome) / gộp lại vào máy (`?dry_run=1` xem trước, `?overwrite=1` ghi đè cái đã có; thân = file) |
| `GET` / `POST` | `/api/update` | Kiểm tra bản phát hành mới nhất trên GitHub / tải, kiểm tra và cài rồi khởi động lại |

> [!IMPORTANT]
> `/` và `/api/*` chỉ trả lời **loopback**, kể cả khi cổng được mở ra ngoài. Một request đến từ loopback nhưng mang header của reverse proxy hay tunnel (`cf-connecting-ip`, `x-forwarded-for`, `forwarded`, …) được coi là từ bên ngoài, nên dashboard vẫn riêng tư phía sau tunnel. `/v1/*` chỉ trả lời request từ bên ngoài khi đã đặt `DS_API_KEY`.

---

<a id="using-with-dsh-ptc-mode"></a>
## Dùng với dsh (chế độ PTC)

dsh (`@deepseek-ai/dsh`, trên Linux, macOS và Windows) đọc danh sách model từ file patch của profile, không phải từ `/v1/models`. whalegate ghi file đó giúp bạn. Dùng một trong hai cách:

- **Dashboard → dsh** — liệt kê mọi profile trong `$DSH_HOME/profiles`, cho biết cái nào đã trỏ về đây, và ghi những cái bạn tick (chọn PTC hoặc standard), hoặc
- **`whalegate setup-dsh --all`** (`--preset standard` cho preset cổ điển, `--profile <dir>` lặp lại được nếu muốn chỉ định rõ). Các trình cài đặt tự chạy lệnh này khi có dsh.

**Profile nào?** dsh trên terminal dùng `web`. **DeepSeek Harness Desktop** (Windows/macOS) dùng `desktop`, một profile do ứng dụng Electron sở hữu — đó là lý do chỉ ghi `web` khiến người dùng Desktop không thấy model. Hãy thoát ứng dụng Desktop khi cấu hình rồi mở lại. Các profile khác (`headless`, `acp`, tên tuỳ ý) hoạt động tương tự.

Cách nào thì cấu hình cũng idempotent, sao lưu `cordis.patch.yml` thành `.bak.<timestamp>`, và không đụng tới phần còn lại của file:

```yaml
- id: llm-pi-ai
  config:
    providers:
      whalegate:
        api: openai-completions
        baseURL: http://127.0.0.1:8790/v1
        headers: { Authorization: Bearer whalegate-local }   # DS_API_KEY nếu bạn đã đặt
        compat: { supportsDeveloperRole: false, supportsReasoningEffort: true, supportsUsageInStreaming: true }
        models:
          - id: deepseek-web
            contextWindow: 512000        # suy ra, xem bên dưới
            maxTokens: 32768
            input: [text, image]
            reasoningEfforts: { off: none, low: auto, max: max }
- id: agent-default-model
  config: { provider: whalegate, model: deepseek-web }
- id: agent-preset-registry
  config: { selectedDefault: ptc }
```

- **Key được ghi thẳng vào `headers`.** Transport OpenAI của dsh từ chối gửi request nếu không có thông tin xác thực, kể cả tới một server cục bộ không cần key. Ghi sẵn như vậy thì không cần `apiKeyEnv` hay `.env`, và cũng sửa luôn lỗi `no credential for provider route`.
- **`contextWindow` được chọn để lịch sử không bao giờ tràn.** dsh nén ở 80% cửa sổ, còn whalegate phải tự cắt lịch sử khi prompt vượt `DS_PROMPT_MAX_CHARS` (1,822M ký tự). Giả định xấu nhất ~4 ký tự mỗi token cộng 10% dự phòng cho phần đặc tả tool, cửa sổ là `DS_PROMPT_MAX_CHARS × 0.9 ÷ 4 ÷ 0.8`, bị chặn bởi `DS_CONTEXT_TOKENS`. Mặc định ra ≈512K, nên dsh luôn tóm tắt trước khi gateway phải cắt bất cứ thứ gì. Đổi *Trần prompt* trên dashboard rồi chạy lại setup để theo kịp.

```mermaid
flowchart LR
    A["history grows"] --> B{"≥ 80% of contextWindow?<br/>(≈410K tokens)"}
    B -- "yes" --> C["dsh summarizes old turns<br/>(clean, lossless log)"]
    B -- "no" --> D["send as is"]
    C --> D
    D --> E{"prompt > DS_PROMPT_MAX_CHARS?"}
    E -- "never reached<br/>with the derived window" --> F["DeepSeek web"]
    E -. "only if misconfigured" .-> G["gateway trims old tool results / turns"]
```

**Vì sao là PTC (Programmatic Tool Calling)?** Thay vì một schema cho mỗi tool, model chỉ thấy một tool `run_code` cùng một SDK TypeScript được sinh ra, rồi viết một chương trình nối nhiều tool lại:

```mermaid
flowchart LR
    subgraph Standard["standard preset — 4 round trips"]
        direction TB
        s1[bash: ls] --> s2[read a.ts] --> s3[read b.ts] --> s4[bash: grep]
    end
    subgraph PTC["PTC preset — 1 round trip"]
        direction TB
        p1["run_code:<br/>const files = await tools.bash({command:'ls'})<br/>for (const f of …) await tools.read({path:f})<br/>return summary"]
    end
```

Chỉ những gì chương trình in ra hoặc trả về mới quay lại ngữ cảnh. Qua whalegate điều này nghĩa là ít request hơn hẳn tới DeepSeek web, tức ít bị throttle hơn và bớt độ trễ cố định của mỗi lượt. Đổi lại, cả bước đó dựa vào một tham số `code` duy nhất, nên một lỗi SDK làm hỏng cả chương trình.

---

<a id="cloudflare-worker-fan-out-optional"></a>
## Cloudflare Worker fan-out (tuỳ chọn)

`worker/ds-proxy.js` cho nhiều instance whalegate (máy hoặc VPS khác nhau) một địa chỉ công khai chung. Nó kiểm tra token của client, rải request theo vòng tròn, và chuyển sang instance kế khi một cái lỗi hoặc không còn tài khoản dùng được (`429`/`5xx`, kể cả `503 accounts_logged_out`). Nếu mọi instance đều lỗi, client nhận lỗi thật của instance cuối thay vì một `502` chung chung.

```mermaid
flowchart LR
    C[Clients] -->|Bearer WORKER_TOKEN| W["Cloudflare Worker"]
    W -->|Bearer UPSTREAM_TOKEN| H1["whalegate #1"]
    W -->|Bearer UPSTREAM_TOKEN| H2["whalegate #2"]
    W --> H3["whalegate #n"]
```

`WORKER_TOKEN` của client không bao giờ tới instance. Worker gửi `UPSTREAM_TOKEN` của riêng nó (chính là `DS_API_KEY` của các instance). Chỉ `/v1/chat/completions`, `/v1/search/messages` và `/v1/models` được chuyển tiếp. Giao diện quản trị và `/api/*` không bao giờ truy cập được qua Worker.

### 1. Cho mỗi instance truy cập được và khoá nó lại

Cloudflare cần một địa chỉ `https://` công khai cho mỗi instance, ví dụ một [Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/) tới `http://127.0.0.1:8790`. Hãy đặt `DS_API_KEY` trên mọi instance trước.

> Tunnel hay reverse proxy chạy cùng máy làm request đến từ `127.0.0.1`. whalegate nhận ra các header mà những proxy này thêm vào (`cf-connecting-ip`, `x-forwarded-for`, `forwarded`, …) và coi request đó là từ bên ngoài: dashboard và `/api/*` trả `403`, còn `/v1/*` trả `403` cho tới khi đặt `DS_API_KEY`. Token tài khoản của bạn không bao giờ lộ ra qua tunnel.

### 2. Tạo Cloudflare API token

Cloudflare dashboard → **My Profile → API Tokens → Create Token → mẫu "Edit Cloudflare Workers"**. Giới hạn trong tài khoản của bạn. Script không cần quyền nào khác.

### 3. Triển khai

```bash
CLOUDFLARE_API_TOKEN=<token từ bước 2> \
WG_UPSTREAMS=https://wg1.example.com,https://wg2.example.com \
WG_UPSTREAM_TOKEN=<DS_API_KEY đã đặt trên các instance> \
./worker/deploy.sh
```

Script kiểm tra token, triển khai Worker, đặt các secret, rồi thử địa chỉ vừa triển khai: `/health` trả lời, `/v1/models` bị từ chối khi không có token, `/api/accounts` bị chặn, và `/v1/models` kèm token tới được một instance. Nó in ra `base_url` cho client. Token của client được sinh ở lần chạy đầu và lưu trong `~/.whalegate/worker-token` (chỉ mình bạn đọc được); truyền `WG_WORKER_TOKEN` nếu muốn tự chọn. Đặt `CLOUDFLARE_ACCOUNT_ID` nếu token thấy nhiều hơn một tài khoản. Chạy lại bất cứ lúc nào để đổi danh sách instance.

Test của Worker chạy được mà không cần tài khoản Cloudflare: `node --test worker/ds-proxy.test.mjs`.

---

<a id="project-layout"></a>
## Cấu trúc dự án

```text
src/
├── main.rs            CLI: serve | whoami | chat | chatraw
├── chat.rs            /v1/chat/completions — một lõi (run_chat) cho stream & không stream,
│                      failover, thử lại để sửa, tool_choice, nối tiếp stateful
├── server.rs          router, pool tài khoản, API dashboard, parser DSML & kiểm tra schema,
│                      dựng prompt
├── client.rs          client HTTP + SSE cho chat.deepseek.com, phát hiện stream đứng
├── pow.rs             Keccak-f[1600] + proof-of-work DeepSeekHashV1
├── chrome_capture.rs  đăng nhập CDP, lấy tab, làm mới cookie
├── settings.rs        cài đặt dashboard (đã lưu → env → mặc định)
├── setup_dsh.rs       `whalegate setup-dsh` + nút dsh trên dashboard (ghi cấu hình đa nền tảng)
├── usage.rs           tính token, giá API trực tiếp, độ trễ
├── logbuf.rs          vòng log trong bộ nhớ cho màn hình log trực tiếp
├── ui.html            dashboard (nhúng vào binary)
└── i18n.js            bản dịch dashboard vi/en/zh (nhúng vào ui.html lúc khởi động)
deploy/                unit systemd cho bản source, wrapper cấu hình dsh
install.sh, install.ps1 trình cài đặt một dòng (Linux/macOS, Windows)
.github/workflows/     pipeline phát hành: build musl tĩnh, macOS, Windows → GitHub release
worker/                Cloudflare Worker fan-out
docs/                  tài nguyên README, release notes, hướng dẫn i18n
```

Đầu ra gọi tool bị lỗi được lưu ở `~/.whalegate/samples.jsonl`. Phát lại chúng với parser hiện tại bằng:

```bash
cargo test --release replay_samples -- --ignored --nocapture
```

Bản dịch tiếng Trung của dashboard xuất phát từ một userscript do [wang1048920402](https://github.com/wang1048920402) đóng góp ở [issue #3](https://github.com/RuriMeiko/whalegate/issues/3).

PoW `DeepSeekHashV1` là SHA3-256 bỏ vòng 0 của keccak-f, port 1:1 từ [CJackHwang/ds2api](https://github.com/CJackHwang/ds2api).

---

<a id="limitations"></a>
## Hạn chế

- DeepSeek web **không phải** là API. Độ trễ cao hơn, và endpoint hay hành vi có thể đổi hoặc bị chặn bất cứ lúc nào.
- Gọi tool là mô phỏng bằng prompt. Nó đã được gia cố (kiểm tra, thử lại để sửa, parser khoan dung), nhưng vẫn kém chính xác hơn API function-calling gốc.
- Các lệnh gọi tool tới cùng một cục khi model viết xong khối, không phải từng phần delta của tham số.
- Các tham số lấy mẫu (`temperature`, `top_p`, `max_tokens`, `stop`, `response_format`) không có tương đương trên web.
- Không có endpoint embeddings.
- Đăng nhập Chrome được kiểm thử trên Linux, macOS và Windows trong CI (`.github/workflows/ci.yml`): tìm Chrome, mở với vân tay của tài khoản, đóng sạch sẽ, mở lại cùng profile mà cookie còn nguyên. whalegate tìm Chrome ở các vị trí chuẩn của từng hệ điều hành (`/Applications` và `~/Applications` trên macOS; `%ProgramFiles%`, `%LOCALAPPDATA%` và App Paths trong registry trên Windows), rồi quay sang Chromium, Edge hoặc Brave. Đặt `DS_CHROME_BIN` để chọn trình duyệt khác.
- Binary phát hành không được ký mã, nên macOS Gatekeeper và Windows SmartScreen có thể hỏi một lần.

---

## Giấy phép

[MIT](LICENSE) © 2026 RuriMeiko. **Hãy dùng có trách nhiệm, và mua API chính thức nếu bạn cần độ tin cậy.**
