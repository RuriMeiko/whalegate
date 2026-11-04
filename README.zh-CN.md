<p align="center">
  <img src="docs/banner.svg" alt="whalegate — 通往 DeepSeek 网页版的 OpenAI 兼容网关" width="100%">
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust">
  <img alt="OpenAI compatible" src="https://img.shields.io/badge/API-OpenAI--compatible-412991">
  <img alt="Tool calling" src="https://img.shields.io/badge/tool__calls-native--style-2ea44f">
  <img alt="Vision" src="https://img.shields.io/badge/vision-images-blue">
  <img alt="License" src="https://img.shields.io/badge/license-MIT-lightgrey">
</p>

<p align="center"><a href="README.md">English</a> · <a href="README.vi.md">Tiếng Việt</a> · <b>简体中文</b></p>

**whalegate** 能把你自己的 `chat.deepseek.com` 网页会话变成标准的 OpenAI 风格 `/v1/chat/completions` 接口。它把多个账号放进一个账号池，出错时在账号之间自动切换，通过 SSE 流式输出，能读取图片，并在 OpenAI `tools` 与 DeepSeek 的 DSML 格式之间互相转换。Cline、Continue、LiteLLM、OpenAI SDK 和 `dsh` 等智能体可以像使用真正的 API 一样使用它。

> [!WARNING]
> **正式工作请使用官方 API。** [platform.deepseek.com](https://platform.deepseek.com/) 价格便宜、有 SLA，也不会让你的账号冒风险。whalegate 的用途是学习、协议研究，以及在**你自己的**账号上做个人自动化。对网页应用做自动化可能违反 DeepSeek 的服务条款，导致账号被封。请不要转售或共享访问权限。使用风险自负（[MIT](LICENSE)，不提供任何担保）。

---

<a id="contents"></a>
## 目录

- [功能特性](#features)
- [工作原理](#how-it-works)
- [快速开始](#quick-start)
- [添加账号](#adding-accounts)
- [使用 API](#using-the-api)
- [工具调用](#tool-calling)
- [图片（视觉）](#images-vision)
- [推理强度](#reasoning-effort)
- [有状态对话](#stateful-conversations)
- [账号池与故障转移](#account-pool--failover)
- [每个账号独立出口 IP（代理）](#egress-ip-per-account-proxies)
- [控制面板](#dashboard)
- [在机器之间迁移账号](#moving-accounts-between-machines)
- [更新](#updating)
- [配置](#configuration)
- [HTTP 接口](#http-endpoints)
- [配合 dsh 使用（PTC 模式）](#using-with-dsh-ptc-mode)
- [Cloudflare Worker 分流](#cloudflare-worker-fan-out-optional)
- [项目结构](#project-layout)
- [局限性](#limitations)

> 文中提到的界面按钮名称（如 **+ Thêm**、**Chrome riêng**）是控制面板的越南语原文。你可以在面板页首的 🌐 开关切换为简体中文，对应名称见括号内的说明。

---

<a id="features"></a>
## 功能特性

| | |
|---|---|
| 🔌 **OpenAI 兼容** | `POST /v1/chat/completions`（流式与非流式）和 `GET /v1/models`。响应采用 OpenAI 的结构，包括 `usage` 与 `cached_tokens` |
| 🛠️ **工具调用** | 支持 `tools`、`tool_choice`（`auto` / `none` / `required` / 指定名称）和 `parallel_tool_calls`。参数会按你的 JSON Schema 校验，网关会在错误的调用到达客户端之前，带着针对性的纠正提示自动重试 |
| 🖼️ **视觉** | 接受 `image_url`（data URL 或 http）、Anthropic 风格的 base64 `image`，以及工具结果中的图片。图片会上传到 DeepSeek，并按账号缓存 |
| 🧠 **DeepThink** | `reasoning_effort` 对应 `none` / `auto` / `max`。推理内容以 `reasoning_content` 流式输出 |
| 🐳 **账号池** | 按最空闲账号调度，带单账号并发上限、突发限流和逐级递增的冷却时间 |
| 🔁 **故障转移** | 出错、空响应或被截断的流，会把请求转给另一个账号（最多尝试 3 次） |
| 🧵 **有状态** | 延续同一个网页对话，只发送新增消息，因此很长的智能体循环依然又快又省 |
| 🔐 **工作量证明** | 内置 `DeepSeekHashV1` 求解器，可选为下一个请求提前预解 |
| 🌐 **会话获取** | 点一下书签脚本，就能带着你自己浏览器的指纹添加账号；也可以通过带独立指纹、设备 Cookie 自动刷新的专属 Chrome 登录 |
| 📦 **一行安装** | 为 Linux（x86_64/arm64）、macOS 和 Windows 提供预编译静态二进制。安装程序会在登录时启动网关，并顺带配置 dsh |
| 🔄 **自动更新** | 控制面板会检查 GitHub Releases，点一下即可下载、校验、安装并重启到新版本 |
| 📦 **账号可迁移** | 把账号池导出为文件或剪贴板中的 JSON，再导入到另一台机器；重复合并结果不变 |
| 📊 **控制面板** | 账号、实时日志、Token 用量（含“若走官方 API 要花多少钱”）、p50/p90 延迟，以及可平滑重启的设置 |
| 🌍 **多语言** | 控制面板支持 Tiếng Việt、English 和简体中文 |

---

<a id="how-it-works"></a>
## 工作原理

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

### 一个请求的生命周期

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
## 快速开始

### 安装（推荐）

**Linux / macOS**
```sh
curl -fsSL https://raw.githubusercontent.com/RuriMeiko/whalegate/main/install.sh | sh
```

**Windows**（PowerShell）
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
| 二进制文件 | `~/.local/bin/whalegate` | `~/.local/bin/whalegate` | `%LOCALAPPDATA%\whalegate\whalegate.exe` |
| 运行方式 | `systemd --user` 单元 `whalegate` | launchd 代理 `io.github.rurimeiko.whalegate` | 隐藏的启动项 |
| 日志 | `journalctl --user -u whalegate -f` | `~/Library/Logs/whalegate.log` | 控制面板 → **log** |

安装程序的选项通过环境变量设置：`WHALEGATE_VERSION=v0.4.0`、`WHALEGATE_PORT=8790`、`WHALEGATE_NO_SERVICE=1`（只装二进制）、`WHALEGATE_NO_DSH=1`。重新运行安装程序即原地升级。更喜欢点鼠标？从 [Releases](https://github.com/RuriMeiko/whalegate/releases) 下载压缩包，然后运行 `whalegate`（或双击 `whalegate.exe`）。

### 从源码构建

需要 Rust ≥ 1.85，以及 `cmake` 和 `clang`（wreq 会构建 BoringSSL；Windows 上还需要 NASM）。

```bash
git clone https://github.com/RuriMeiko/whalegate.git ~/whalegate
cd ~/whalegate
cargo build --release          # → target/release/whalegate
./target/release/whalegate     # 控制面板：http://127.0.0.1:8790/

# 为放在 ~/whalegate 的源码目录配置后台服务
mkdir -p ~/.config/systemd/user && cp deploy/whalegate.service ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now whalegate
```

### 命令行

| 命令 | 作用 |
|---|---|
| `whalegate` / `whalegate serve` | 运行网关（默认） |
| `whalegate setup-dsh [--all] [--preset ptc\|standard] [--profile <dir>]... [--url <base>] [--context <tokens>]` | 让 dsh 指向本网关（`--all` = 本机所有 profile） |
| `whalegate version` | 打印版本 |
| `whalegate whoami` | 打印第一个已存账号对应的当前用户 |
| `whalegate chat "<prompt>"` | 发送一条原始 prompt 并输出 SSE 流 |
| `whalegate chatraw "<prompt>" <thinking> <search>` | 同上，可切换 DeepThink 和搜索，用于调试 |

---

<a id="adding-accounts"></a>
## 添加账号

账号保存在 `~/.whalegate/accounts.json`，可用 `DS_ACCOUNTS_PATH` 覆盖。从 ds-harness 升级时，`~/.ds-harness` 会自动迁移到这里。该文件包含**有效的会话 Token**：切勿提交或分享。

在 DeepSeek 看来，每个账号就是一台“机器”：一份 TLS/HTTP2 指纹（由 wreq 模拟的真实 Chrome/Edge 版本）、User-Agent、设备 ID、Cookie、时区和语言。这些在账号的整个生命周期内都保持固定，因为一个正在使用的会话突然出现在另一台机器上，正是会被标记的情况。添加账号有四种方式（**+ Thêm**（+ 添加）里的标签页，按下面的顺序排列），区别在于账号落在*哪台机器*上。

| | 账号使用的机器 | Cookie 刷新 |
|---|---|---|
| **专属 Chrome** | 全新的专属 Chrome，带自动生成的指纹和配置文件 | 自动，每隔 `DS_DEVICE_REFRESH_HOURS`（默认 6 小时） |
| **书签脚本** | 你自己的浏览器；`ds_session_id` 是 whalegate 自己的，不是浏览器的 | 在 DeepSeek 标签页再次点击书签脚本 |
| **浏览器扩展** | 你自己的浏览器，包含所有 Cookie（含 HttpOnly） | 在 DeepSeek 标签页再次点击扩展 |
| **粘贴 Token**（旧方式） | 由 Token 推导（固定，但不是真实浏览器） | 无 |

### 方式 1：专属 Chrome（推荐）

```mermaid
flowchart LR
    A["Dashboard → Chrome riêng:<br/>Mở cửa sổ riêng để đăng nhập / đăng ký"] --> B["New seed → new fingerprint<br/>+ profile ~/.whalegate/profiles/&lt;seed&gt;"]
    B --> C["Dedicated Chrome on a random<br/>CDP port, fingerprint applied to every tab"]
    C --> D["You sign in<br/>(Google / email / passkey)"]
    D --> E["Gateway watches the tab (up to 10 min)<br/>→ token + device id + cookies"]
    E --> F["Save, close Chrome<br/>(profile is kept)"]
    F --> G["Every DS_DEVICE_REFRESH_HOURS:<br/>reopen that Chrome headless → fresh cookies"]
```

在 **+ Thêm → Chrome riêng**（添加 → 专属 Chrome）中点击 **Mở cửa sổ riêng để đăng nhập / đăng ký**（打开专属窗口登录 / 注册）。

- 直接在那个窗口里登录**或注册**。全新的 DeepSeek 账号同样可以：网关只是在等标签页拿到 Token。这是用密码登录时的推荐方式，因为不会在控制面板里输入任何内容。控制面板里还有一个折叠的 **Tự điền email + mật khẩu**（自动填写邮箱和密码）选项，可以替你填表；由于 DeepSeek 的登录页经常变动，这种方式稳定性较差。
- 登录完成前关闭窗口，登录就会取消。同一时间只能打开一个登录窗口。
- 在 systemd 下，网关会从用户会话中获取 `DISPLAY` / `WAYLAND_DISPLAY`，以便窗口能够打开。
- 如果同一个 DeepSeek 用户再次登录，已有账号的 Token 会被替换，而不是重复添加。账号会迁移到新的 Chrome 设备，原有的书签脚本配置会被丢弃。
- 账号上的 **thiết bị**（设备）按钮会立即刷新它的设备 Cookie。
- **Quét Chrome chính**（扫描主 Chrome，位于 Chrome riêng 标签页）会从以 `--remote-debugging-port=9333` 启动的 Chrome 中抓取所有已登录的 `chat.deepseek.com` 标签页。自 Chrome 136 起，默认配置文件会忽略该参数，因此只有搭配独立的 `--user-data-dir` 才有效；日常使用的 Chrome 请改用扩展。

### 方式 2：书签脚本

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

1. 在控制面板打开 **+ Thêm**（+ 添加），把 **Thêm vào whalegate**（添加到 whalegate）拖到书签栏（只需一次）。书签脚本指向你打开控制面板时使用的地址，所以如果 whalegate 运行在另一台机器上，请先用那台机器的地址打开控制面板，再进行拖拽。
2. 在已登录的 `chat.deepseek.com` 标签页上点击这个书签。会打开一个控制面板标签页，账号随即被添加。

数据包不会经过 URL、浏览器历史或网络：它通过 `postMessage` 在标签页之间直接传递，而控制面板只接受由 `chat.deepseek.com` 标签页（也就是打开它的那个）发来的数据，所以其他网站无法把账号塞进你的账号池。之后账号使用的就是你浏览器所在的那台机器：与你浏览器版本最接近的 wreq Chrome/Edge 版本（比表中更新的浏览器会把整套指纹对齐到最近的版本）、你的操作系统、你的设备 ID，以及你的时区和语言（体现在 `x-client-timezone-offset` / `Accept-Language` 中）。网关绝不会为这类账号另开 Chrome。DeepSeek 在响应中设置的 Cookie 会自动保留，但 WAF Cookie（`aws-waf-token`）只能由真实浏览器续期。要刷新它，请在 DeepSeek 标签页再次点击书签脚本：池中已有的账号会被原地更新（合并 Cookie、刷新设备和浏览器配置），而不是重复添加。如果你重新登录并拿到了新 Token，同样的一次点击会把新 Token 换进已有账号。

如果书签脚本无法运行，同样的采集器也在 **Dán token**（粘贴 Token）里提供：复制控制台命令，在 DeepSeek 标签页的 DevTools 控制台中运行，然后粘贴结果。

### 方式 3：浏览器扩展

扩展做的事和书签脚本一样，并且还能读取 HttpOnly Cookie（如 `ds_session_id`）。页面自己的 JavaScript 永远读不到这些 Cookie，但拥有 `cookies` 权限的扩展可以，就像 CDP 在 Chrome 登录中所做的那样。

> **关于 `ds_session_id`。** 它是 `HttpOnly; Secure; SameSite=Strict`，没有过期时间。当请求没有携带它时，DeepSeek 的网页服务器会在 `/api` 之外的第一个响应（favicon 或某个页面）里设置它；`/api/v0/*` 从不设置。使用 Google 登录会签发新的，因为 Google 跳转回来时不会发送 Strict Cookie。通过书签脚本或粘贴 Token 添加的账号读不到浏览器里的值，因此 whalegate 像浏览器一样自己获取：添加账号时通过该账号的客户端发一次 `GET /favicon.ico`，对缺少它的老账号则在启动时补一次。

1. 在 **+ Thêm → Tiện ích**（+ 添加 → 浏览器扩展）中，点击 **Tải tiện ích**（下载扩展）并解压。下载的扩展已经指向你打开的这个控制面板。
2. 打开 `chrome://extensions`，开启**开发者模式**，点击**加载已解压的扩展程序**，选择解压出来的 `whalegate-extension` 文件夹。
3. 在已登录的 `chat.deepseek.com` 标签页上点击 whalegate 图标。账号会被添加（若已在池中则刷新），结果显示在 DeepSeek 标签页上。

扩展只能读取 `chat.deepseek.com` 的 Cookie，也只与控制面板所在的主机通信。它把 Token、设备 ID、Cookie 和浏览器配置直接发送到 `/api/accounts`，与书签脚本发出的是同一个请求。

### 方式 4：粘贴 Token（旧方式）

在已登录的 `chat.deepseek.com` 标签页中，打开 DevTools → Console 并运行：

```js
copy(localStorage.userToken)
```

把结果粘贴到 **+ Thêm → Dán token (legacy)**（+ 添加 → 粘贴 Token（旧方式））。以下几种形式都能识别：`{"value":"…","__version":"0"}`、带引号的字符串、`Bearer …`，或纯 Token。保存前会先校验 Token，Cookie 则是可选的。你也可以通过 API 添加，必要时附带 `browser` 配置（即书签脚本的字段：`user_agent`、`cores`、`memory_gb`、`screen`、`avail`、`dpr`、`tz`、`tz_offset`、`langs`、`gpu_vendor`、`gpu_renderer`、`device_id`）：

```bash
curl -X POST http://127.0.0.1:8790/api/accounts \
  -H 'content-type: application/json' -d '{"token":"<token>","label":"main"}'
```

---

<a id="using-the-api"></a>
## 使用 API

唯一的模型是 `deepseek-web`。旧别名 `deepseek-web-r1` 和 `deepseek-web:max` 仍然可用。

### curl

```bash
curl http://127.0.0.1:8790/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "deepseek-web",
    "messages": [{"role": "user", "content": "用三句话解释 Rust 的借用检查器。"}],
    "stream": true
  }'
```

### OpenAI Python SDK

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8790/v1", api_key="未设置-DS_API_KEY-时随便填")

resp = client.chat.completions.create(
    model="deepseek-web",
    reasoning_effort="max",                      # none | auto | max
    messages=[{"role": "user", "content": "证明 √2 是无理数。"}],
)
print(resp.choices[0].message.reasoning_content)  # DeepThink 推理过程
print(resp.choices[0].message.content)
```

### 额外的请求字段

在各 SDK 中通过 `extra_body` 传入。

| 字段 | 类型 | 默认值 | 含义 |
|---|---|---|---|
| `reasoning_effort` | string | `DS_DEFAULT_EFFORT` | `none` = 快速，`auto` = 仅在用户回合思考，`max` = 始终思考。也支持 `reasoning.effort` |
| `thinking` | bool | `false` | DeepThink 的旧式开关 |
| `search` | bool | `false` | DeepSeek 网页搜索 |
| `new_thread` | bool | `false` | 新开一个网页对话，而不是延续匹配的那个 |

### 兼容性说明

- `usage` 来自网页应用真实的 `accumulated_token_usage`。缺失时使用按文字类型估算的值，与实测值相差约 25% 以内。`prompt_tokens_details.cached_tokens` 统计的是网页对话里已存在的那部分上下文。
- `429` 会带上 `Retry-After`。SSE 每 10 秒发送一次保活。上游失败会返回清晰的 `error` 对象。
- 网页应用没有与 `temperature`、`top_p`、`max_tokens`、`stop`、`response_format` 或 `n > 1` 对应的功能，因此这些参数会被忽略。

---

<a id="tool-calling"></a>
## 工具调用

DeepSeek 网页版没有工具 API。whalegate 的做法是把你的 OpenAI `tools` 渲染成模型本来就在用的 **DSML** 块格式，再把输出解析回来：

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

模型写出什么，以及你的客户端收到什么：

<table>
<tr><th>模型输出（DSML）</th><th>客户端收到</th></tr>
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

### 它为什么表现得像真正的 API

- **不误判。** 只有请求里确实带了 `tools`，才会提取调用。备用格式（JSON、类 XML）只有在位于回复末尾、且指向你声明过的工具时才算数。回答里的 HTML，或文字中举例的 `<bash command="…"/>`，仍然只是文本。
- **Schema 校验。** 类型、`enum`、`required`、嵌套的 `properties` / `items`、`anyOf` / `oneOf`。取值还会被转换成声明的类型：`"50"` 变成 `50`，字符串参数收到 `123` 会变成 `"123"`。
- **纠错重试。** 出错的调用从不盲目重试。下一次尝试会附上模型自己的输出和具体问题，例如 `todo: "items[0].status" must be one of "todo"|"done"`，让模型精确地改正这一处。
- **`tool_choice`。** `none` 返回纯文本。`required` 和指定名称会强制发起调用，若模型用文字作答就重试。指定一个未声明的工具会返回 `400 invalid_request_error`。
- **`parallel_tool_calls: false`** 只保留一个调用。
- **标记绝不外泄。** DSML 不会出现在 `content` 里；模型在思考时起草调用，也会从 `reasoning_content` 中剔除。
- **历史可往返。** 先前的调用会按规范的 DSML 重新渲染。空的工具结果显示为 `(no output)`，避免模型重复执行一个没有输出的 `mkdir`。

### 示例：智能体循环

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

messages = [{"role": "user", "content": "/ 分区还剩多少磁盘空间？"}]
while True:
    r = client.chat.completions.create(model="deepseek-web", messages=messages,
                                       tools=tools, reasoning_effort="auto")
    msg = r.choices[0].message
    messages.append(msg)
    if not msg.tool_calls:
        print(msg.content)
        break
    for call in msg.tool_calls:
        output = run(call)                       # 你自己的执行器
        messages.append({"role": "tool", "tool_call_id": call.id, "content": output})
```

---

<a id="images-vision"></a>
## 图片（视觉）

```python
client.chat.completions.create(model="deepseek-web", messages=[{
    "role": "user",
    "content": [
        {"type": "text", "text": "这张截图哪里不对？"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0..."}},
    ],
}])
```

- 接受 `image_url`（data URL 或公网 `http(s)` URL；私有和内部地址会被拒绝）、Anthropic 风格的 `{"type":"image","source":{…}}`，以及**工具结果中**返回的图片，例如 `read_image` 工具。
- 每张图片通过 DeepSeek 的文件接口上传（有独立的 PoW），并以 `ref_file_ids` 附加。上传结果按内容哈希在每个账号下缓存。
- 每个请求最多发送最近的 4 张不同图片。单张图片的大小上限为 `DS_MAX_IMAGE_MB`（默认 20）。

---

<a id="reasoning-effort"></a>
## 推理强度

| `reasoning_effort` | DeepThink | 适用场景 |
|---|---|---|
| `none`（也接受 `off`、`minimal`） | 从不 | 快速回答和简单的工具循环 |
| `auto` | 用户回合开启，仅接续 `tool` 结果的步骤**关闭** | 智能体。带思考的步骤约需 10–44 秒，不思考的步骤约 2 秒，因此工具循环会快上好几倍 |
| `max`（其他任意值） | 每一步 | 难题 |

客户端没有传值时，使用 `DS_DEFAULT_EFFORT`（默认 `none`）。客户端发来的生成标题请求从不思考。

---

<a id="stateful-conversations"></a>
## 有状态对话

智能体每一步都会重发完整历史。whalegate 会为每个对话前缀以及它签发过的 tool-call id 计算指纹。当下一个请求扩展了一个已知前缀时，只有**新**消息会通过 `parent_message_id` 发到同一个网页对话。

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

当前缀不匹配、持有该对话的账号繁忙超过 4 秒、工具发生变化，或对话增长超过 `DS_CONV_MAX_CHARS` 时，网关会改为发送完整上下文，并且可以在任意账号上进行。闲置的网页对话会在 `DS_CONV_TTL` 之后被删除。设置 `DS_STATEFUL=0` 可始终发送全部内容。

---

<a id="account-pool--failover"></a>
## 账号池与故障转移

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

- 每个请求最多有 **3 次尝试**。上游失败会换到另一个账号。模型自己的错误（错误的工具调用）会带着纠正提示重试，不会惩罚账号。
- 没有空闲账号时，请求最多等待 `DS_QUEUE_WAIT_SECS`，然后返回带 `Retry-After` 的 `429`。
- 流停滞 90 秒或中途中断（已收到部分文字但没有 `close` 事件）时，会像网页那样先在**同一个账号上续接**：`POST /chat/resume_stream` 最多 5 秒（每次等待 3 秒，两次之间停 1 秒），然后在短暂的随机停顿后相当于点击“继续”按钮，调用带 `fallback_to_resume` 的 `POST /chat/continue`。只会转发客户端尚未看到的部分。如果仍然失败，才视为被截断并切换到另一个账号。
- 当 DeepSeek 要求验证码（`CHALLENGE_REQUIRED`）时，该账号进入休息，网关会随机等待 2–5 秒再尝试其他账号，而不是立刻重放同一份请求。
- 进行中的租约会持续整个流的生命周期，即使客户端断开也一样，所以并发上限真正有效。

---

<a id="egress-ip-per-account-proxies"></a>
## 每个账号独立出口 IP（代理）

在 DeepSeek 眼里，一个账号就是一对 `(IP, 指纹)`。多个账号从同一个 IP 出去，看起来就像同一个人，所以 whalegate 可以让每个账号走不同的代理。

绑定的依据是**指纹种子，而不是 Token**：属于同一台“机器”（同一指纹）的两个账号始终共用一个 IP，而两台不同的机器绝不会共用。绑定关系保存在 `~/.whalegate/proxy-bindings.json`，所以账号在重启后保持 IP 不变，即使你删除后重新添加也一样。

```
DS_PROXY_MODE=strict
DS_PROXY_LIST='socks5h://user:pass@203.0.113.7:1080
http://198.51.100.4:8080'
```

也可以把同样的行放进 `~/.whalegate/proxies.txt`，或粘贴到控制面板的 **Cài đặt → IP đầu ra**（设置 → 出口 IP）中（列表会逐行校验；只要有一行有误，整次保存都会被拒绝，因此不会出现一半账号悄悄直连的情况）。

代理的分配方式：

1. 第一次见到某个指纹时，它会得到当前承载指纹最少的那个代理——让账号池均匀分布，而不是全堆在第一项上。
2. 这个绑定会写入磁盘。之后来自同一指纹的请求始终使用它。
3. 如果已绑定的代理从列表里消失，该绑定会被丢弃并重新选择，而不是让账号卡住。

模式：`off`（直连，默认）· `auto`（配置了代理就用；代理池不可用时记录日志并直连）· `strict`（没有可用代理时拒绝发起请求——适用于泄露真实 IP 比请求失败更糟的场景）。

能用 `socks5h` 就别用 `socks5`：它让代理来解析 DNS，这样对 `chat.deepseek.com` 的域名查询也不会发生在你的机器上。

`GET /api/proxy` 显示当前的代理池和所有绑定；`POST /api/proxy` 替换列表；带 `{"fingerprint":"…"}` 的 `POST /api/proxy/unbind` 会释放一个绑定，让该账号下次得到不同的 IP。

<a id="dashboard"></a>
### 用 Cloudflare Worker 作为出口路线

除了代理，账号还可以经由 **Cloudflare Worker** 出去：`worker/egress.js` 打开 TCP 隧道（TLS 仍是端到端，Worker 只能看到加密字节），DeepSeek 看到的是 Cloudflare 的 IP 而不是你的。点击控制面板顶部的 **Cloudflare**：whalegate 会自动查找或下载 Node.js（校验 checksum），运行 `wrangler login`（你只需点 *Allow*），在账号没有 `workers.dev` 地址时自动注册，部署 Worker 并测出它的 IP。每个账号的 Chrome 窗口（登录、注册、打开 Chrome、刷新设备）都使用同一条隧道。

可以在**多个 Cloudflare 账号**上各加一个 Worker（*+ 用另一个 Cloudflare 账号添加 Worker*，最多 8 个）。每个 DeepSeek 账号按指纹种子固定绑定到一个 Worker，IP 不会来回跳。弹窗按 Worker 分别显示 IP、节点和额度。

每个账号采用第一条适用的路线：**① 自有代理**（模式不是 `off` 且有可用代理）→ **② Cloudflare Worker**（已开启且至少有一个 Worker）→ **③ 直连**你的 IP。三者可同时使用。目标本身在 Cloudflare 上（hCaptcha）时一律直连，因为 Worker 连不上 Cloudflare 地址。账号的 Chrome 窗口目前只走 ② 或 ③，不走自有代理 ①。使用 Worker 须遵守 Cloudflare 的条款，请自行确认是否适用于你的用法。

设置（**Cài đặt → IP đầu ra**，即 设置 → 出口 IP）：`DS_EGRESS_ON`、`DS_EGRESS_WORKERS`（每行 `地址 密钥`，由按钮自动写入）、`DS_EGRESS_DAILY_LIMIT`（弹窗里显示的额度，按每个 Worker 计；免费版为每天 10 万）。

## 控制面板

打开 `http://127.0.0.1:8790/`。仅限本机回环访问。

- **账号：** 添加、验证、重置、编辑标签或 Token、在池中启用/停用、批量验证/重置，以及重复用户检测。**验证**不只是检查认证：它会在一次性会话上发送一条最小的真实补全请求，因为被限流的账号仍能通过认证接口。结果为 alive、throttled、auth_dead 或 unknown，只有 alive 才会清除限流标记。
- **用量：** 每小时的 Token（缓存未命中 / 缓存命中 / 输出），以及同样的流量在官方 API 上要花多少钱，按 **设置 → 显示** 中的汇率以 $（英文）、¥（中文）或 ₫（越南语）显示。价格每 6 小时从 DeepSeek 的定价页实时抓取，包含高峰时段，并提供整月的预测。点击 **Token**、**节省** 或图表的标题，可在 今天 / 7 天 / 30 天 / 全部 之间切换（选择会被记住）。数据保存在 `accounts.json` 旁边的 `usage.json` 中。
- **延迟：** 最近一小时的 p50/p90，以及错误数。
- **实时日志：** 带过滤和搜索的 SSE 流。常用标签包括 `[toolcall-invalid]`、`[toolcall-unparsed]`、`[conv]`、`[ds-stall]` 和 `[ds-fail]`。每一行日志也会保存到 `~/.whalegate/logs/whalegate-YYYYMMDD.log`（保留 7 天，每个文件最多 64 MB），详细事件则写入 `debug-YYYYMMDD.jsonl`：每次续接 / 继续的尝试、中断的流最后几行 SSE、`CHALLENGE_REQUIRED` 之类的账号错误。日志窗口里的 **export（导出）** 按钮会下载一个调试包（版本、设置、各账号摘要，以及最近 24 小时到 7 天的两种日志）。其中不含 Token、Cookie 或账号标签。
- **设置：** 只提供经过验证的选项。保存时会等到没有请求在处理，然后进程自行 re-exec（PID 不变，并载入新构建），页面随即重新加载。**Hard reset** 会立即重启。
- **导出 / 导入：** 把整个账号池作为 JSON 文件或直接通过剪贴板迁移到另一台机器。导入按 DeepSeek 用户 ID 合并，所以运行两次也是安全的；它绝不会删除你已有的账号。
- **语言：** 通过页首右端的地球开关（VI / EN / 中）在 Tiếng Việt / English / 简体中文 之间切换。首次访问会跟随浏览器语言，并记住你的选择。参见 [docs/i18n.md](docs/i18n.md)。
- **更新：** 控制面板在加载时以及每 30 分钟检查一次 GitHub Releases。出现更新的标签时会显示一个横幅；点一下即可下载预编译二进制、校验 SHA-256、原地替换并 re-exec，新进程启动后页面会自动重新加载。

---
<a id="moving-accounts-between-machines"></a>
## 在机器之间迁移账号

账号池保存在 `~/.whalegate/accounts.json`，里面是会话 Token——这个文件本身就是凭据。有两种带走它的方式，都在控制面板里完成：

- **Export → Tải file**（导出 → 下载文件）会下载 `whalegate-accounts-<ts>.json`；把它拷过去，在另一台机器上使用 **Import → chọn file**（导入 → 选择文件）。
- **Export → Copy clipboard**（导出 → 复制到剪贴板）把同样的 JSON 放进剪贴板（浏览器阻止异步剪贴板 API 时，会退回到隐藏的 textarea）；在目标机器上把它粘贴到导入对话框。

导入是合并而不是替换。账号先按 DeepSeek 用户 ID 匹配，再按 Token 匹配，所以同一个包导入两次，第二次不会添加任何东西。不勾选*覆盖*时，已存在的账号会被跳过；勾选后，它们的 Token 和 Cookie 会被替换（并清除其过期的网页会话）。任何账号都不会被删除。

**完整备份（整台机器）。** 导出对话框里的 **Sao lưu đầy đủ**（完整备份）按钮会下载一个 `whalegate-backup-<时间>.tar.gz`，除上述内容外还包含控制面板设置、代理列表与指纹→代理的绑定、使用历史，以及每个专属 Chrome 配置文件中的身份部分（含登录信息和设备 ID 的 localStorage、IndexedDB、Cookie、偏好设置：每个只有几百 KB，不含缓存）。在 **Import**（导入）里选同一个文件，会先显示预览（账号、配置文件、设置、代理、使用历史），确认后才会写入。导入是合并：账号按 DeepSeek 用户 ID 匹配，本机已有的内容（设置、代理列表、绑定、使用小时、配置文件夹）会保留，除非你勾选*覆盖*。有登录窗口打开时绝不会动配置文件。Chrome 用与操作系统绑定的密钥加密 Cookie 数据库，因此在不同操作系统（或不同用户）的机器上，配置文件里的 Cookie 可能无法解密；登录信息和设备 ID（localStorage）以及保存在 `accounts.json` 中的账号自身 Cookie 仍会完整到达，下一次设备刷新会更新其余部分。读取时会校验压缩包：指向数据目录之外的路径、链接和过大的内容都会被拒绝。通过 HTTP：`GET /api/backup/export` 和 `POST /api/backup/import?dry_run=1&overwrite=1`（请求体为该文件）。

> [!WARNING]
> 导出包包含每个账号的 Token 和 Cookie。持有它的人就能使用这些会话。请通过你信任的渠道传输，用完后删除文件，并且绝不要提交到仓库——`accounts.json` 和 `*.session` 已经在 `.gitignore` 里了。

---

<a id="updating"></a>
## 更新

`GET /api/update` 读取 `https://api.github.com/repos/RuriMeiko/whalegate/releases/latest`，与正在运行的 `CARGO_PKG_VERSION` 比较，并返回 `{current, latest, has_update, notes, url}`。

随后 `POST /api/update` 会：

1. 下载最新标签对应的 `whalegate-<target>.tar.gz`（Windows 上是 `.zip`）和 `SHA256SUMS.txt`，
2. 校验 SHA-256（在代码内部实现，不增加依赖），
3. 解压出二进制文件，复制到正在运行的文件旁边，再用 `rename` 替换到位——这是原子操作，并且在旧 inode 仍在执行时也是安全的，
4. 响应 HTTP 请求，然后 re-exec，让新的二进制接管同一个 PID。

控制面板在加载时以及每 30 分钟检查一次，出现更新的标签时显示横幅，并在 `/api/restart` 报告新的 boot id 后重新加载页面。进行中的请求会被中断；dsh 这类智能体会自行重试。

Windows 会锁定正在运行的 `.exe`，所以第 3 步的 rename 在那里行不通。更新程序改为把新二进制复制成 `whalegate.exe.new`，并在旁边写一个 `whalegate.update.bat`；重启流程会运行这个脚本，它等待旧进程退出，用 `move /y` 替换文件，重新启动应用并删除自身。脚本只有在两个文件都还在时才会运行，所以被放弃的更新绝不会重放。

如果你是通过 `install.sh` / `install.ps1` 安装的，就无需再做什么——二进制被原地替换，服务配置不受影响。若想在 shell 里更新，重新运行安装程序即可：

```sh
curl -fsSL https://raw.githubusercontent.com/RuriMeiko/whalegate/main/install.sh | sh
```

---

<a id="configuration"></a>
## 配置

优先级：控制面板中保存的值 → `DS_*` 环境变量 → 默认值。标有 ⚙️ 的行可以在控制面板中编辑。

| 变量 | 默认值 | 含义 |
|---|---|---|
| `DS_PORT` | `8790` | 监听端口（始终绑定 `127.0.0.1`） |
| `DS_API_KEY` | – | 设置后，`/v1/*` 需要 `Authorization: Bearer <key>`。`/v1/*` 要响应来自本机以外的任何请求，必须先设置它 |
| `DS_ACCOUNTS_PATH` | `~/.whalegate/accounts.json` | 账号存储。设置、用量和样本保存在它旁边 |
| ⚙️ `DS_STATEFUL` | `1` | 延续网页对话，只发送增量 |
| ⚙️ `DS_POW_PREFETCH` | `1` | 提前获取并解算下一个 PoW |
| ⚙️ `DS_DEFAULT_EFFORT` | `none` | 客户端未指定时的推理强度 |
| ⚙️ `DS_CONTEXT_TOKENS` | `256000` | `/v1/models` 中公布的 `context_length`。网页应用接受 ≥512K（约 1.1M 会返回空流），但 256K 能让 prompt 更小、更不显眼 |
| ⚙️ `DS_PROMPT_MAX_CHARS` | `912000` | 超出预算：先裁剪旧的工具结果，再丢弃最早的回合 |
| ⚙️ `DS_MAX_CONC_PER_ACC` | `2` | 每个账号同时处理的请求数。超过 2 往往会被限速 |
| ⚙️ `DS_TZ_OFFSET` | `25200` | 发送给网页应用并用于按日统计的时区偏移（秒） |
| ⚙️ `DS_FX_CNY` | `720` | 控制面板为中文时使用的汇率，单位为每美元 0.01 ¥（`720` = 7.20 ¥）。费用以美元计算，显示时再换算 |
| ⚙️ `DS_FX_VND` | `26000` | 控制面板为越南语时使用的汇率：每美元兑多少越南盾 |
| ⚙️ `DS_PROXY_MODE` | `off` | 每个账号的出口 IP：`off` 直连 · `auto` 有代理就用，否则直连 · `strict` 宁可失败也不泄露真实 IP |
| ⚙️ `DS_PROXY_LIST` | – | 代理列表，每行一个：`scheme://[user:pass@]host:port`，支持 `http`、`https`、`socks5` 或 `socks5h` |
| ⚙️ `DS_EGRESS_ON` | `1` | 使用 `DS_EGRESS_WORKERS` 中的 Cloudflare Worker（关闭则保留列表但不使用） |
| ⚙️ `DS_EGRESS_WORKERS` | – | Cloudflare Worker 列表，每行 `地址 密钥`（最多 8 个），由控制面板按钮自动写入；每个账号固定绑定一个 |
| ⚙️ `DS_EGRESS_DAILY_LIMIT` | `100000` | Cloudflare 弹窗中按每个 Worker 显示的每日请求额度（免费版 = 10 万） |
| `DS_PROXY_FILE` | `~/.whalegate/proxies.txt` | 代理列表文件（格式相同，允许 `#` 注释） |
| `DS_CONV_TTL` | `1800` | 闲置网页对话在这么多秒之后删除 |
| `DS_CONV_MAX_CHARS` | `1200000` | 单个有状态对话的最大体积 |
| `DS_SESSION_ROTATE` | `30` | 共用（无工具）会话轮换前的回合数 |
| `DS_QUEUE_WAIT_SECS` | `120` | 返回 `429` 之前等待空闲账号的时间 |
| `DS_FAIL_COOLDOWN_MAX` | `600` | 递增的失败冷却时间的上限 |
| `DS_BURST_LIMIT` / `DS_BURST_WINDOW` / `DS_COOLDOWN` | `6` / `30` / `10` | 每个账号的突发限流器 |
| `DS_MAX_IMAGE_MB` | `20` | 单张图片的最大体积 |
| `DS_READ_TIMEOUT_SECS` | `120` | 套接字读取超时（需要重启） |
| `DS_CHROME_BIN` | 自动 | 用于登录的 Chrome/Chromium 路径 |
| `DS_LOGIN_CDP_PORT` | 随机 40000–59999 | 为专属登录 Chrome 指定固定的 CDP 端口 |
| `DS_DEVICE_REFRESH_HOURS` | `6` | 在后台重新打开每个 Chrome 登录账号的 Chrome，以获取新的设备 Cookie（`0` = 关闭） |
| `DS_CN_HOLIDAYS` | – | 额外的中国节假日（`YYYY-MM-DD,…`），用于高峰价格计算 |
| `DS_DEBUG_SSE` | `0` | 把原始 SSE 写入 `/tmp/ds_debug_sse.log` |

---

<a id="http-endpoints"></a>
## HTTP 接口

| 方法 | 路径 | 说明 |
|---|---|---|
| `POST` | `/v1/chat/completions` | OpenAI 兼容的聊天 |
| `GET` | `/v1/models` | 模型列表（`context_length`、`reasoning_efforts`、模态） |
| `GET` | `/` | 控制面板 |
| `GET` / `POST` | `/api/accounts` | 列出 / 添加账号（`{token, label?, cookies?, browser?}`） |
| `PATCH` / `DELETE` | `/api/accounts/{id}` | 编辑标签或 Token / 删除 |
| `POST` | `/api/accounts/{id}/verify` · `/reset` · `/pool` | 用一次真实聊天探测（返回 `probe: alive\|throttled\|auth_dead\|unknown`）/ 清除错误和会话后再探测 / `{"pool_enabled": bool}` |
| `POST` | `/api/accounts/{id}/device` | 立即刷新设备 Cookie（Chrome 登录的账号） |
| `POST` | `/api/capture` · `/api/login_start` | 从你的主 Chrome 抓取已登录的标签页 / 打开专属登录 Chrome |
| `GET` | `/api/stats` · `/api/usage` | 账号池状态（处理中、冷却、有状态命中率）/ 用量、费用和延迟 |
| `GET` | `/api/logs` · `/api/logs/stream` | JSON 格式日志 / SSE |
| `GET` / `POST` | `/api/settings` | 读取或更新设置（`null` 表示重置某个键） |
| `GET` / `POST` | `/api/proxy` | 出口代理池与指纹→代理的绑定 / 替换列表（`{"list": "…"}`） |
| `POST` | `/api/proxy/unbind` | 释放一个绑定（`{"fingerprint": "…"}`），让该账号获得新的 IP |
| `GET` / `POST` | `/api/dsh` | dsh 状态（列出找到的所有 profile）/ 写入 dsh 配置（`{"preset": "ptc"\|"standard", "profiles": ["desktop","web"]}`；省略 `profiles` 或设置 `"all": true` 表示所有 profile） |
| `GET` / `POST` | `/api/restart` | 状态 / `{"mode": "idle" \| "now" \| "cancel"}` |
| `GET` | `/api/accounts/export` | 把整个账号池下载为 JSON 包（含 Token——当作机密对待） |
| `POST` | `/api/accounts/import` | 合并一个包或 `{accounts: [...]}`。`dry_run` 用于预览，`overwrite` 会替换已知用户的 Token |
| `GET` / `POST` | `/api/backup/export` · `/api/backup/import` | 完整备份为一个 `.tar.gz`（账号、设置、代理、绑定、使用历史、Chrome 配置文件）/ 合并回本机（`?dry_run=1` 预览，`?overwrite=1` 覆盖已有内容；请求体为该文件） |
| `GET` / `POST` | `/api/update` | 检查 GitHub 上最新的发布 / 下载、校验并安装，然后重启 |

> [!IMPORTANT]
> 即使端口对外开放，`/` 和 `/api/*` 也只响应**回环**请求。来自回环但带有反向代理或隧道请求头（`cf-connecting-ip`、`x-forwarded-for`、`forwarded` 等）的请求会被视为外部请求，因此控制面板在隧道后面同样保持私密。`/v1/*` 只有在设置了 `DS_API_KEY` 时才会响应外部请求。

---

<a id="using-with-dsh-ptc-mode"></a>
## 配合 dsh 使用（PTC 模式）

dsh（`@deepseek-ai/dsh`，支持 Linux、macOS 和 Windows）从 profile 的补丁文件读取模型列表，而不是从 `/v1/models`。whalegate 会替你写好这个文件。两种方式任选其一：

- **控制面板 → dsh**——它会列出 `$DSH_HOME/profiles` 下的所有 profile，显示哪些已经指向这里，并写入你勾选的那些（可选 PTC 或 standard），或者
- **`whalegate setup-dsh --all`**（`--preset standard` 使用经典预设，`--profile <dir>` 可重复使用以明确指定）。检测到 dsh 时，安装程序会自动运行它。

**用哪个 profile？** 终端里的 dsh 使用 `web`。**DeepSeek Harness Desktop**（Windows/macOS）使用 `desktop`，这是 Electron 应用自己管理的 profile——这就是为什么只写 `web` 会让桌面端用户看不到该模型。配置时请先退出桌面应用，配置完再重新打开。其他 profile（`headless`、`acp`、自定义名称）的用法相同。

无论哪种方式，配置都是幂等的，会把 `cordis.patch.yml` 备份为 `.bak.<timestamp>`，并且不会动文件里的其他内容：

```yaml
- id: llm-pi-ai
  config:
    providers:
      whalegate:
        api: openai-completions
        baseURL: http://127.0.0.1:8790/v1
        headers: { Authorization: Bearer whalegate-local }   # 如果设置了 DS_API_KEY 就填它
        compat: { supportsDeveloperRole: false, supportsReasoningEffort: true, supportsUsageInStreaming: true }
        models:
          - id: deepseek-web
            contextWindow: 256000        # 推导而来，见下文
            maxTokens: 32768
            input: [text, image]
            reasoningEfforts: { off: none, low: auto, max: max }
- id: agent-default-model
  config: { provider: whalegate, model: deepseek-web }
- id: agent-preset-registry
  config: { selectedDefault: ptc }
```

- **密钥直接写进 `headers`。** dsh 的 OpenAI 传输层在没有凭据时拒绝发送请求，哪怕对象是不需要密钥的本地服务器。直接写在里面，就不需要 `apiKeyEnv` 或 `.env`，同时也解决了 `no credential for provider route` 的报错。
- **`contextWindow` 的取值保证历史永远不会溢出。** dsh 在窗口用到 80% 时压缩，而当 prompt 超过 `DS_PROMPT_MAX_CHARS`（91.2 万字符）时，whalegate 必须自己裁剪历史。按最坏情况每个 Token 约 4 个字符，再给工具说明留出 10% 余量，窗口为 `DS_PROMPT_MAX_CHARS × 0.9 ÷ 4 ÷ 0.8`，并受 `DS_CONTEXT_TOKENS` 限制。默认得到约 256K，所以 dsh 总是先做摘要，网关无需裁剪任何内容。在控制面板中修改 *Trần prompt*（Prompt 上限）后，重新运行配置即可同步。

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

**为什么用 PTC（Programmatic Tool Calling，程序化工具调用）？** 模型看到的不是每个工具一个 schema，而是一个 `run_code` 工具加上一份生成的 TypeScript SDK，然后写一段程序把许多工具串起来：

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

只有程序打印或返回的内容才会回到上下文里。经过 whalegate，这意味着发往 DeepSeek 网页版的请求少得多，被限流的情况更少，每一轮固定的延迟也更低。代价是整个步骤都压在一个 `code` 参数上，因此 SDK 用错一处就会让整个程序失败。

---

<a id="cloudflare-worker-fan-out-optional"></a>
## Cloudflare Worker 分流（可选）

`worker/ds-proxy.js` 让多个 whalegate 实例（不同的机器或 VPS）共用一个公网地址。它校验客户端的 Token，按轮询分发请求，当某个实例出错或没有可用账号时（`429`/`5xx`，包括 `503 accounts_logged_out`）转到下一个实例。如果所有实例都失败，客户端拿到的是最后一个实例的真实错误，而不是笼统的 `502`。

```mermaid
flowchart LR
    C[Clients] -->|Bearer WORKER_TOKEN| W["Cloudflare Worker"]
    W -->|Bearer UPSTREAM_TOKEN| H1["whalegate #1"]
    W -->|Bearer UPSTREAM_TOKEN| H2["whalegate #2"]
    W --> H3["whalegate #n"]
```

客户端的 `WORKER_TOKEN` 绝不会到达实例。Worker 发送的是它自己的 `UPSTREAM_TOKEN`（即各实例的 `DS_API_KEY`）。只有 `/v1/chat/completions`、`/v1/search/messages` 和 `/v1/models` 会被转发。管理界面和 `/api/*` 绝不会通过 Worker 暴露。

### 1. 让每个实例可访问并加锁

Cloudflare 需要为每个实例提供一个公网 `https://` 地址，例如指向 `http://127.0.0.1:8790` 的 [Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)。请先在每个实例上设置 `DS_API_KEY`。

> 同一台机器上的隧道或反向代理会让请求看起来来自 `127.0.0.1`。whalegate 能识别这些代理添加的请求头（`cf-connecting-ip`、`x-forwarded-for`、`forwarded` 等），并把此类请求视为外部请求：控制面板和 `/api/*` 返回 `403`，而 `/v1/*` 在设置 `DS_API_KEY` 之前也返回 `403`。你的账号 Token 绝不会通过隧道暴露。

### 2. 创建 Cloudflare API Token

Cloudflare 控制台 → **My Profile → API Tokens → Create Token → “Edit Cloudflare Workers” 模板**。把它限制在你的账号内。脚本不需要其他权限。

### 3. 部署

```bash
CLOUDFLARE_API_TOKEN=<第 2 步得到的 token> \
WG_UPSTREAMS=https://wg1.example.com,https://wg2.example.com \
WG_UPSTREAM_TOKEN=<各实例上设置的 DS_API_KEY> \
./worker/deploy.sh
```

脚本会检查 Token、部署 Worker、设置密钥，然后测试已部署的地址：`/health` 能响应，不带 Token 的 `/v1/models` 被拒绝，`/api/accounts` 被拦截，带 Token 的 `/v1/models` 能到达某个实例。最后会打印给客户端用的 `base_url`。客户端 Token 在第一次运行时生成，保存在 `~/.whalegate/worker-token`（仅你本人可读）；如需自定义，传入 `WG_WORKER_TOKEN`。如果该 Token 能看到不止一个账号，请设置 `CLOUDFLARE_ACCOUNT_ID`。随时重新运行即可更改实例列表。

Worker 的测试无需 Cloudflare 账号即可运行：`node --test worker/ds-proxy.test.mjs`。

---

<a id="project-layout"></a>
## 项目结构

```text
src/
├── main.rs            命令行：serve | whoami | chat | chatraw
├── chat.rs            /v1/chat/completions — 流式与非流式共用一个核心（run_chat），
│                      故障转移、纠错重试、tool_choice、有状态延续
├── server.rs          路由、账号池、控制面板 API、DSML 解析器与 schema 校验、
│                      prompt 渲染
├── client.rs          chat.deepseek.com 的 HTTP + SSE 客户端，停滞检测
├── pow.rs             Keccak-f[1600] + DeepSeekHashV1 工作量证明
├── chrome_capture.rs  CDP 登录、标签页抓取、Cookie 刷新
├── settings.rs        控制面板设置（已保存 → 环境变量 → 默认值）
├── setup_dsh.rs       `whalegate setup-dsh` + 控制面板的 dsh 按钮（跨平台配置写入）
├── usage.rs           Token 统计、实时 API 价格、延迟
├── logbuf.rs          实时日志视图用的内存日志环
├── ui.html            控制面板（嵌入二进制）
└── i18n.js            控制面板翻译 vi/en/zh（启动时内联到 ui.html）
deploy/                源码部署用的 systemd 单元、dsh 配置包装脚本
install.sh, install.ps1 一行安装脚本（Linux/macOS、Windows）
.github/workflows/     发布流水线：静态 musl、macOS、Windows 构建 → GitHub release
worker/                Cloudflare Worker 分流
docs/                  README 资源、发布说明、i18n 指南
```

格式错误的工具调用输出会保存到 `~/.whalegate/samples.jsonl`。用当前的解析器重放它们：

```bash
cargo test --release replay_samples -- --ignored --nocapture
```

控制面板的中文翻译源自 [wang1048920402](https://github.com/wang1048920402) 在 [issue #3](https://github.com/RuriMeiko/whalegate/issues/3) 中贡献的用户脚本，感谢！

`DeepSeekHashV1` 工作量证明是去掉 keccak-f 第 0 轮的 SHA3-256，1:1 移植自 [CJackHwang/ds2api](https://github.com/CJackHwang/ds2api)。

---

<a id="limitations"></a>
## 局限性

- DeepSeek 网页版**不是** API。延迟更高，接口或行为可能随时变化或被封锁。
- 工具调用是通过 prompt 模拟的。它已经做了加固（校验、纠错重试、宽容的解析器），但仍不如原生函数调用 API 精确。
- 工具调用是在模型写完整个块之后一次性到达的，而不是以参数增量的形式流式输出。
- 采样参数（`temperature`、`top_p`、`max_tokens`、`stop`、`response_format`）在网页端没有对应功能。
- 没有 embeddings 接口。
- Chrome 登录在 CI 中已于 Linux、macOS 和 Windows 上测试（`.github/workflows/ci.yml`）：查找 Chrome、用账号的指纹打开、干净地关闭、再用同一个 profile 重新打开且 Cookie 完好。whalegate 会在各操作系统的标准位置查找 Chrome（macOS 上是 `/Applications` 和 `~/Applications`；Windows 上是 `%ProgramFiles%`、`%LOCALAPPDATA%` 和注册表中的 App Paths），找不到再退回到 Chromium、Edge 或 Brave。设置 `DS_CHROME_BIN` 可指定其他浏览器。
- 发布的二进制文件没有代码签名，因此 macOS Gatekeeper 和 Windows SmartScreen 可能会提示一次。

---

## 许可证

[MIT](LICENSE) © 2026 RuriMeiko。**请负责任地使用；如果你需要可靠性，请购买官方 API。**
