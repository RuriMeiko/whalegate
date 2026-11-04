/* Dashboard i18n: vi (gốc) / en / zh.
 *
 * Nguồn chữ trong ui.html và server vẫn là tiếng Việt; lớp này dịch lúc chạy:
 *   1. tra nguyên câu trong bảng DICT [vi, en, zh]
 *   2. câu có số/tên thì khớp luật trong RULES
 *   3. câu ghép bằng " · " hoặc " — " thì dịch từng vế
 * Không dịch: log (#logBody), release notes (#updNotes), nội dung do người dùng gõ, vùng có data-nt.
 * Thêm ngôn ngữ / thêm chữ: xem docs/i18n.md. */
(function () {
  'use strict';

  /* ---------- bảng dịch nguyên câu: [vi, en, zh] ---------- */
  const DICT = [
    /* thanh trên */
    ['cài đặt', 'settings', '设置'],
    ['Phiên bản đang chạy — ghi chú phát hành', 'Running version — release notes', '当前运行版本 — 发布说明'],
    ['Mã nguồn trên GitHub', 'Source code on GitHub', 'GitHub 源代码'],
    ['Ngôn ngữ', 'Language', '语言'],
    ['mất kết nối', 'disconnected', '连接断开'],
    /* băng cập nhật / băng đăng xuất */
    ['Có bản mới', 'New version available', '有新版本'],
    ['xem', 'view', '查看'],
    ['Cập nhật', 'Update', '更新'],
    ['Ẩn', 'Hide', '隐藏'],
    ['Đăng nhập lại: nút trên acc, hoặc bấm lại tiện ích/bookmarklet trên tab DeepSeek.',
      'To log in again: use the button on the account, or click the extension/bookmarklet again on the DeepSeek tab.',
      '重新登录：使用账号上的按钮，或在 DeepSeek 标签页再次点击扩展/书签脚本。'],
    /* ô số liệu */
    ['Token hôm nay', 'Tokens today', '今日 Token'],
    ['Tiết kiệm hôm nay', 'Saved today', '今日节省'],
    ['Độ trễ (1 giờ)', 'Latency (last hour)', '延迟（近 1 小时）'],
    ['Tài khoản', 'Accounts', '账号'],
    ['24 giờ qua', 'Last 24 hours', '过去 24 小时'],
    ['Chi phí nếu trả API', 'Cost at API prices', '按官方 API 计价的费用'],
    ['chưa có lượt nào', 'no requests yet', '暂无请求'],
    ['hôm nay', 'today', '今天'],
    ['7 ngày', '7 days', '7 天'],
    ['30 ngày', '30 days', '30 天'],
    ['giá dự phòng', 'fallback price', '备用价格'],
    ['Verify hết', 'Verify all', '全部验证'],
    ['Reset hết', 'Reset all', '全部重置'],
    ['+ Thêm', '+ Add', '+ 添加'],
    ['Chưa có tài khoản.', 'No accounts yet.', '暂无账号。'],
    ['Chưa có tài khoản', 'No accounts yet', '暂无账号'],
    /* thêm tài khoản */
    ['Thêm tài khoản', 'Add account', '添加账号'],
    ['Đóng', 'Close', '关闭'],
    ['Cách thêm', 'Ways to add', '添加方式'],
    ['Tiện ích', 'Extension', '浏览器扩展'],
    ['Bookmarklet', 'Bookmarklet', '书签脚本'],
    ['Chrome riêng', 'Dedicated Chrome', '专属 Chrome'],
    ['Dán token', 'Paste token', '粘贴 Token'],
    ['Đủ mọi cookie, kể cả HttpOnly. Cài một lần, sau đó bấm biểu tượng whalegate trên tab chat.deepseek.com.',
      'Captures every cookie, including HttpOnly. Install once, then click the whalegate icon on a chat.deepseek.com tab.',
      '获取全部 Cookie，包括 HttpOnly。只需安装一次，之后在 chat.deepseek.com 标签页点击 whalegate 图标即可。'],
    ['Tải tiện ích', 'Download extension', '下载扩展'],
    ['rồi giải nén', 'then unzip it', '然后解压'],
    ['→ bật Developer mode → Load unpacked → chọn thư mục vừa giải nén',
      '→ turn on Developer mode → Load unpacked → pick the folder you just unzipped',
      '→ 开启开发者模式 → 加载已解压的扩展程序 → 选择刚解压的文件夹'],
    ['Kéo lên thanh bookmark một lần, sau đó bấm trên tab chat.deepseek.com. Cookie HttpOnly (',
      'Drag it to the bookmarks bar once, then click it on a chat.deepseek.com tab. The HttpOnly cookie (',
      '拖到书签栏一次，之后在 chat.deepseek.com 标签页点击它。HttpOnly Cookie ('],
    [') do whalegate tự xin, khác với của trình duyệt.',
      ') is requested by whalegate itself and differs from the browser\'s.',
      ') 由 whalegate 自行申请，与浏览器中的不同。'],
    ['Thêm vào whalegate', 'Add to whalegate', '添加到 whalegate'],
    ['kéo lên bookmark', 'drag to bookmarks', '拖到书签栏'],
    ['Tự đăng nhập bằng email + mật khẩu trong Chrome có vân tay riêng — không cần bấm gì trong cửa sổ. Mật khẩu chỉ đi tới chat.deepseek.com, không lưu lại.',
      'Logs in automatically with email + password inside a Chrome that has its own fingerprint — no need to click anything in the window. The password only goes to chat.deepseek.com and is not stored.',
      '在拥有独立指纹的 Chrome 中使用邮箱和密码自动登录，无需在窗口里做任何操作。密码只会发送到 chat.deepseek.com，不会被保存。'],
    ['email', 'email', '邮箱'],
    ['Email', 'Email', '邮箱'],
    ['mật khẩu', 'password', '密码'],
    ['Mật khẩu', 'Password', '密码'],
    ['Đăng nhập tự động', 'Log in automatically', '自动登录'],
    ['Mở Chrome rồi bạn tự đăng nhập — nút bên dưới', 'Opens Chrome and you log in yourself — button below', '打开 Chrome 后由你自行登录 — 见下方按钮'],
    ['Mở cửa sổ tự đăng nhập', 'Open window and log in myself', '打开窗口手动登录'],
    ['Mở cửa sổ Chrome rồi bạn tự đăng nhập trong đó', 'Opens a Chrome window and you log in there yourself', '打开 Chrome 窗口，由你在其中自行登录'],
    ['Quét Chrome chính', 'Scan main Chrome', '扫描主 Chrome'],
    ['Lấy các tab chat.deepseek.com đang đăng nhập từ một Chrome mở với --remote-debugging-port=9333. Từ Chrome 136 cờ này bị bỏ qua ở profile mặc định (cần thêm --user-data-dir riêng); với Chrome hằng ngày hãy dùng tab Tiện ích.',
      'Picks up logged-in chat.deepseek.com tabs from a Chrome started with --remote-debugging-port=9333. Since Chrome 136 this flag is ignored on the default profile (add your own --user-data-dir); for your everyday Chrome use the Extension tab.',
      '从以 --remote-debugging-port=9333 启动的 Chrome 中获取已登录的 chat.deepseek.com 标签页。自 Chrome 136 起，默认配置文件会忽略该参数（需另加 --user-data-dir）；日常使用的 Chrome 请改用“浏览器扩展”标签页。'],
    ['Token', 'Token', 'Token'],
    ['giá trị localStorage.userToken, hoặc kết quả lệnh console', 'value of localStorage.userToken, or the output of the console command', 'localStorage.userToken 的值，或控制台命令的输出'],
    ['copy lệnh console', 'copy console command', '复制控制台命令'],
    ['Lệnh chạy trong Console của tab chat.deepseek.com, gom token kèm hồ sơ trình duyệt', 'Command to run in the Console of a chat.deepseek.com tab; collects the token together with the browser profile', '在 chat.deepseek.com 标签页的控制台中运行，收集 Token 及浏览器配置'],
    ['nhãn (tuỳ chọn)', 'label (optional)', '标签（可选）'],
    ['Nhãn', 'Label', '标签'],
    ['Thêm', 'Add', '添加'],
    ['đã copy', 'copied', '已复制'],
    ['copy', 'copy', '复制'],
    ['Đã copy lệnh — dán vào Console của tab chat.deepseek.com', 'Command copied — paste it into the Console of a chat.deepseek.com tab', '命令已复制 — 请粘贴到 chat.deepseek.com 标签页的控制台'],
    ['Dán token trước', 'Paste a token first', '请先粘贴 Token'],
    ['Nhập email và mật khẩu trước', 'Enter the email and password first', '请先输入邮箱和密码'],
    ['Đã mở cửa sổ Chrome riêng — đăng nhập hoặc đăng ký trong đó, xong tự lưu và tự đóng.', 'Opened a dedicated Chrome window — log in or sign up there; it saves and closes by itself when done.', '已打开专属 Chrome 窗口 — 请在其中登录或注册，完成后会自动保存并关闭。'],
    ['acc quay lại pool', 'account is back in the pool', '账号已回到池中'],
    ['acc đã vào pool', 'account is in the pool', '账号已加入池中'],
    ['giữ vân tay trình duyệt gốc', 'original browser fingerprint kept', '保留原浏览器指纹'],
    ['tài khoản', 'account', '账号'],
    /* export / import */
    ['Export', 'Export', '导出'],
    ['Import', 'Import', '导入'],
    ['Export tài khoản', 'Export accounts', '导出账号'],
    ['Import tài khoản', 'Import accounts', '导入账号'],
    ['Gói export chứa', 'The export bundle contains', '导出包包含账号池中所有账号的'],
    ['token và cookie', 'tokens and cookies', 'Token 和 Cookie'],
    ['của mọi tài khoản trong pool — coi như mật khẩu, chỉ chuyển qua kênh tin cậy.', 'of every account in the pool — treat it like a password and only move it over a trusted channel.', '— 请当作密码对待，只通过可信渠道传输。'],
    ['Chọn cách mang đi: tải file', 'Choose how to carry it: download a', '选择带走方式：下载'],
    ['hoặc copy JSON vào clipboard để dán thẳng vào máy kia.', 'file or copy the JSON to the clipboard and paste it straight into the other machine.', '文件，或把 JSON 复制到剪贴板，直接粘贴到另一台机器。'],
    ['Copy clipboard', 'Copy to clipboard', '复制到剪贴板'],
    ['Tải file', 'Download file', '下载文件'],
    ['Huỷ', 'Cancel', '取消'],
    ['Chọn file gói export, hoặc dán JSON vào ô dưới (dùng khi đã copy clipboard ở máy kia).', 'Pick an export file, or paste the JSON below (use this when you copied it to the clipboard on the other machine).', '选择导出文件，或把 JSON 粘贴到下方（在另一台机器上复制到剪贴板时使用）。'],
    ['hoặc [... ]', 'or [... ]', '或 [... ]'],
    ['Chưa có dữ liệu.', 'No data yet.', '暂无数据。'],
    ['Chưa có dữ liệu', 'No data yet', '暂无数据'],
    ['Ghi đè token/cookie của tài khoản đã có', 'Overwrite token/cookie of existing accounts', '覆盖已有账号的 Token/Cookie'],
    ['Không đọc được file', 'Could not read the file', '无法读取文件'],
    ['gói import phải là object hoặc mảng', 'the import bundle must be an object or an array', '导入包必须是对象或数组'],
    ['Import?', 'Import?', '导入？'],
    ['Đã copy gói export vào clipboard', 'Export bundle copied to the clipboard', '导出包已复制到剪贴板'],
    ['chưa có dữ liệu export', 'no export data yet', '尚无导出数据'],
    ['trình duyệt chặn clipboard — dùng "Tải file"', 'the browser blocked the clipboard — use "Download file"', '浏览器禁止访问剪贴板 — 请使用“下载文件”'],
    ['bỏ qua (đã có)', 'skipped (already present)', '跳过（已存在）'],
    /* cập nhật */
    ['Đang chạy', 'Running', '当前版本'],
    ['Mới nhất', 'Latest', '最新版'],
    ['Trạng thái', 'Status', '状态'],
    ['Lỗi', 'Error', '错误'],
    ['đang kiểm tra…', 'checking…', '检查中…'],
    ['bị đình chỉ', 'suspended', '已被封停'],
    ['DeepSeek đã đình chỉ tài khoản này (USER_IS_BANNED)', 'DeepSeek has suspended this account (USER_IS_BANNED)', 'DeepSeek 已封停该账号 (USER_IS_BANNED)'],
    ['không gọi được /api/update', 'could not call /api/update', '无法调用 /api/update'],
    ['có bản mới', 'update available', '有可用更新'],
    ['đã là bản mới nhất', 'already the latest version', '已是最新版本'],
    ['Đã mới nhất', 'Up to date', '已是最新'],
    ['Cập nhật tải bản dựng sẵn từ GitHub, kiểm tra SHA-256, thay binary rồi tự khởi động lại. Request đang chạy bị cắt — dsh sẽ tự thử lại.',
      'Update downloads the prebuilt release from GitHub, verifies its SHA-256, swaps the binary and restarts by itself. Running requests are cut — dsh retries automatically.',
      '更新会从 GitHub 下载预编译版本，校验 SHA-256，替换可执行文件并自动重启。正在运行的请求会被中断 — dsh 会自动重试。'],
    ['Dò lại', 'Check again', '重新检查'],
    ['Cập nhật & khởi động lại', 'Update & restart', '更新并重启'],
    ['Cập nhật?', 'Update?', '更新？'],
    ['Không có bản mới để cập nhật', 'There is no new version to update to', '没有可更新的新版本'],
    ['Đã cài bản mới nhưng chưa thấy server lên lại — tải lại trang sau ít giây', 'The new version is installed but the server has not come back — reload the page in a few seconds', '新版本已安装，但服务器尚未重新上线 — 请几秒后刷新页面'],
    /* cấu hình dsh */
    ['Cấu hình dsh', 'Configure dsh', '配置 dsh'],
    ['Ghi provider', 'Writes the provider', '将 provider'],
    ['vào các profile dsh trên máy này, đặt', 'into the dsh profiles on this machine and sets', '写入本机的 dsh profile，并把'],
    ['làm model mặc định. Mỗi profile được backup trước khi sửa.', 'as the default model. Every profile is backed up before it is edited.', '设为默认模型。每个 profile 修改前都会先备份。'],
    ['Profile tìm thấy', 'Profiles found', '找到的 Profile'],
    ['chọn hết', 'select all', '全选'],
    ['bỏ chọn', 'select none', '全不选'],
    ['Cửa sổ ngữ cảnh báo cho dsh được tính từ', 'The context window reported to dsh is derived from', '报告给 dsh 的上下文窗口取自'],
    ['Trần prompt', 'Prompt cap', 'Prompt 上限'],
    [': dsh tự nén ở 80% cửa sổ, nên luôn nén trước khi gateway phải cắt lịch sử.', ': dsh compacts at 80% of the window, so it always compacts before the gateway has to cut history.', '：dsh 会在窗口用到 80% 时自行压缩，因此总是在网关需要截断历史之前完成压缩。'],
    ['PTC — nhiều tool trong 1 lượt (khuyên dùng)', 'PTC — many tools in one turn (recommended)', 'PTC — 一轮内调用多个工具（推荐）'],
    ['standard — mỗi bước 1 tool', 'standard — one tool per step', 'standard — 每步一个工具'],
    ['Cấu hình', 'Configure', '配置'],
    ['đã trỏ về gateway này', 'already points to this gateway', '已指向此网关'],
    ['chưa cấu hình', 'not configured', '未配置'],
    ['Có profile desktop: chọn nó nếu bạn dùng app Desktop, chọn web nếu bạn dùng dsh trên terminal — hoặc cả hai.', 'A desktop profile exists: pick it if you use the Desktop app, pick web if you use dsh in the terminal — or both.', '存在 desktop profile：使用桌面应用请选它，在终端使用 dsh 请选 web，也可以两者都选。'],
    ['Chọn profile bạn thực sự dùng. dsh trên terminal thường là web.', 'Pick the profile you actually use. dsh in the terminal is usually web.', '请选择你实际使用的 profile。终端中的 dsh 通常是 web。'],
    ['Chọn ít nhất một profile', 'Pick at least one profile', '请至少选择一个 profile'],
    ['không cấu hình được', 'could not configure', '无法配置'],
    ['Các profile đã đúng, không cần đổi', 'The profiles are already correct, nothing to change', 'Profile 已正确，无需修改'],
    /* sửa tài khoản */
    ['Sửa tài khoản', 'Edit account', '编辑账号'],
    ['Token mới', 'New token', '新 Token'],
    ['(để trống = giữ token hiện tại)', '(leave empty = keep the current token)', '（留空 = 保留当前 Token）'],
    ['{"value":"…"} hoặc token trần', '{"value":"…"} or a bare token', '{"value":"…"} 或纯 Token'],
    ['ID DeepSeek chưa biết (verify để lấy)', 'DeepSeek ID unknown (verify to fetch it)', 'DeepSeek ID 未知（验证后可获取）'],
    ['Lưu', 'Save', '保存'],
    /* cài đặt */
    ['Cài đặt', 'Settings', '设置'],
    ['Đang tải…', 'Loading…', '加载中…'],
    ['Không có mục nào.', 'No items.', '没有设置项。'],
    ['đang đặt bằng biến môi trường', 'set through an environment variable', '已通过环境变量设置'],
    ['cần khởi động lại', 'needs a restart', '需要重启'],
    ['tắt', 'off', '关'],
    ['bật', 'on', '开'],
    ['TẮT', 'OFF', '关'],
    ['BẬT', 'ON', '开'],
    ['Bỏ thay đổi chưa lưu', 'Discard unsaved change', '放弃未保存的修改'],
    ['mọi thay đổi có hiệu lực ngay', 'every change takes effect immediately', '所有修改立即生效'],
    ['Đã lưu cài đặt', 'Settings saved', '设置已保存'],
    ['không lưu được', 'could not save', '无法保存'],
    ['Khởi động lại?', 'Restart?', '重启？'],
    ['Có mục chỉ áp dụng sau khi khởi động lại. Khởi động lại ngay khi rảnh?', 'Some items only apply after a restart. Restart as soon as idle?', '部分设置项需重启后才会生效。是否在空闲时立即重启？'],
    ['Khởi động lại', 'Restart', '重启'],
    ['Hội thoại', 'Conversation', '对话'],
    ['Tốc độ', 'Speed', '速度'],
    ['Pool', 'Pool', '账号池'],
    ['Mạng', 'Network', '网络'],
    ['Proxy', 'Proxy', '代理'],
    ['Stateful', 'Stateful', '有状态'],
    ['Nối tiếp phiên web, chỉ gửi phần mới', 'Continue the web session and send only what is new', '沿用网页会话，只发送新增内容'],
    ['Dự đoán PoW', 'PoW prefetch', 'PoW 预取'],
    ['Tải và giải sẵn cho request kế tiếp', 'Fetch and solve ahead for the next request', '为下一个请求提前获取并解算'],
    ['Mức suy nghĩ mặc định', 'Default thinking effort', '默认思考强度'],
    ['Khi client không chỉ định. none nhanh · auto nghĩ lượt người dùng · max luôn nghĩ', 'When the client does not specify. none is fast · auto thinks on user turns · max always thinks', '客户端未指定时使用。none 最快 · auto 在用户回合思考 · max 始终思考'],
    ['Cửa sổ ngữ cảnh', 'Context window', '上下文窗口'],
    ['Số báo cho client; web nhận ≥512K token', 'Number reported to clients; the web accepts ≥512K tokens', '报告给客户端的数值；网页端可接受 ≥512K Token'],
    ['Vượt thì cắt tool_result cũ rồi bỏ lượt cũ', 'When exceeded, old tool_result is trimmed and then old turns are dropped', '超出时先裁剪旧的 tool_result，再丢弃旧回合'],
    ['Đồng thời mỗi tài khoản', 'Concurrency per account', '单账号并发数'],
    ['Cao hơn 2 dễ bị DeepSeek giới hạn', 'Above 2 DeepSeek tends to rate-limit', '高于 2 容易被 DeepSeek 限速'],
    ['Lượt/giờ mỗi tài khoản', 'Requests/hour per account', '单账号每小时请求数'],
    ['Request/phút mỗi tài khoản', 'Requests/minute per account', '单账号每分钟请求数'],
    ['0 = tự học: DeepSeek báo tin nhắn quá thường xuyên thì tự hạ nhịp theo số request vừa gửi, nới lại sau 30 phút', '0 = auto-learn: when DeepSeek says messages are too frequent, slow down to the number of requests just sent, and relax again after 30 minutes', '0 = 自动学习：DeepSeek 提示消息过于频繁时，按刚发送的请求数自动降速，30 分钟后再放宽'],
    ['Chạm trần thì tài khoản nghỉ tới hết giờ · 0 = tắt', 'At the cap the account rests until the hour ends · 0 = off', '达到上限后账号休息到整点结束 · 0 = 关闭'],
    ['Làm mới thiết bị', 'Device refresh', '设备刷新'],
    ['Mở Chrome nền của từng acc để lấy cookie WAF mới · 0 = tắt', 'Opens each account\'s background Chrome to get fresh WAF cookies · 0 = off', '打开每个账号的后台 Chrome 获取新的 WAF Cookie · 0 = 关闭'],
    ['Múi giờ', 'Time zone', '时区'],
    ['Giây lệch UTC · +7 = 25200', 'Offset from UTC in seconds · +7 = 25200', '相对 UTC 的偏移秒数 · +7 = 25200'],
    ['IP đầu ra', 'Egress IP', '出口 IP'],
    ['off = đi thẳng · auto = có proxy thì dùng, chết thì đi thẳng · strict = proxy chết thì báo lỗi, không lộ IP thật', 'off = direct · auto = use a proxy if alive, else go direct · strict = fail if the proxy is dead, never expose the real IP', 'off = 直连 · auto = 有可用代理则使用，失效则直连 · strict = 代理失效则报错，绝不暴露真实 IP'],
    ['Danh sách proxy', 'Proxy list', '代理列表'],
    ['Mỗi dòng một proxy: scheme://[user:pass@]host:port (http | https | socks5 | socks5h)', 'One proxy per line: scheme://[user:pass@]host:port (http | https | socks5 | socks5h)', '每行一个代理：scheme://[user:pass@]host:port (http | https | socks5 | socks5h)'],
    ['giây', 'sec', '秒'],
    ['giờ', 'h', '小时'],
    ['lượt', 'req', '次'],
    ['lượt/phút', 'req/min', '次/分钟'],
    ['ký tự', 'chars', '字符'],
    ['request', 'requests', '请求'],
    ['cần bật hoặc tắt', 'must be on or off', '必须为开或关'],
    ['cần là số nguyên', 'must be an integer', '必须是整数'],
    /* hộp xác nhận chung */
    ['Verify?', 'Verify?', '验证？'],
    ['Reset?', 'Reset?', '重置？'],
    ['Xoá?', 'Delete?', '删除？'],
    ['Xoá', 'Delete', '删除'],
    ['Verify tất cả?', 'Verify all?', '全部验证？'],
    ['Reset tất cả?', 'Reset all?', '全部重置？'],
    ['Verify tất cả', 'Verify all', '全部验证'],
    ['Reset tất cả', 'Reset all', '全部重置'],
    ['Không đổi được', 'Could not change', '无法更改'],
    ['verify lỗi', 'verify failed', '验证出错'],
    ['Dùng trong pool', 'Use in pool', '加入账号池'],
    /* dòng tài khoản */
    ['sẵn sàng', 'ready', '就绪'],
    ['DS đã đăng xuất', 'logged out by DeepSeek', '已被 DeepSeek 登出'],
    ['cần đăng nhập lại', 'login required', '需要重新登录'],
    ['đã tắt', 'disabled', '已禁用'],
    ['lỗi', 'error', '错误'],
    ['trùng người dùng', 'duplicate user', '用户重复'],
    ['trình duyệt gốc', 'original browser', '原浏览器'],
    ['bấm lại tiện ích/bookmarklet trên tab DeepSeek', 'click the extension/bookmarklet again on the DeepSeek tab', '请在 DeepSeek 标签页再次点击扩展/书签脚本'],
    ['thiết bị', 'device', '设备'],
    ['mở Chrome', 'open Chrome', '打开 Chrome'],
    ['Địa chỉ Worker (nút Cloudflare trên đầu trang tự cài). Mọi request và mọi cửa sổ Chrome của acc đi qua đường hầm của Worker, DeepSeek thấy IP Cloudflare', 'Worker address (the Cloudflare button at the top installs it). Every request and every account Chrome window goes through the Worker tunnel; DeepSeek sees a Cloudflare IP', 'Worker 地址（顶部的 Cloudflare 按钮会自动安装）。所有请求和账号的 Chrome 窗口都经 Worker 隧道发出，DeepSeek 看到的是 Cloudflare IP'],
    ['Khoá bí mật của Worker (tự điền khi cài bằng nút Cloudflare)', 'Secret key of the Worker (filled in automatically by the Cloudflare button)', 'Worker 的密钥（用 Cloudflare 按钮安装时自动填入）'],
    ['Dùng Worker Cloudflare', 'Use the Cloudflare Worker', '使用 Cloudflare Worker'],
    ['Tắt để đi thẳng bằng IP nhà (hoặc proxy riêng của từng acc) mà không phải xoá Worker', 'Turn off to go direct with your home IP (or each account\'s own proxy) without deleting the Worker', '关闭后直接使用本机 IP（或各账号自己的代理），无需删除 Worker'],
    ['Hạn mức Worker/ngày', 'Worker quota / day', 'Worker 每日额度'],
    ['Mỗi kết nối qua Worker tính 1 request · Free 100.000/ngày, gói trả phí tính theo tháng nên đặt số bạn muốn theo dõi', 'Each connection through the Worker counts as 1 request · Free is 100,000/day; paid plans are monthly, so set the number you want to track', '每个经 Worker 的连接计 1 次请求 · 免费版 10 万/天，付费版按月计，请设置你想监控的数值'],
    ['Hạn mức request mỗi ngày của gói Cloudflare (Free = 100000)', 'Daily request quota of your Cloudflare plan (Free = 100000)', 'Cloudflare 套餐的每日请求额度（免费 = 100000）'],
    ['Cloudflare Worker — IP đầu ra', 'Cloudflare Worker — egress IP', 'Cloudflare Worker — 出口 IP'],
    ['Mọi request và cửa sổ Chrome của acc đi qua Worker, DeepSeek thấy IP Cloudflare thay vì IP nhà.', 'Every request and account Chrome window goes through the Worker; DeepSeek sees a Cloudflare IP instead of your home IP.', '所有请求和账号的 Chrome 窗口都经 Worker 发出，DeepSeek 看到的是 Cloudflare IP 而不是你的本机 IP。'],
    ['Cloudflare Worker: đổi IP đầu ra cho mọi acc', 'Cloudflare Worker: change the egress IP of every account', 'Cloudflare Worker：更换所有账号的出口 IP'],
    ['whalegate đã tự cài vào thư mục dữ liệu', 'installed by whalegate in its data folder', '由 whalegate 安装在数据目录中'],
    ['đã có trên máy', 'already on this machine', '本机已有'],
    ['đang kiểm tra…', 'checking…', '检查中…'],
    ['Đăng nhập Cloudflare', 'Cloudflare login', 'Cloudflare 登录'],
    ['đã đăng nhập', 'logged in', '已登录'],
    ['chưa đăng nhập — sẽ mở trình duyệt để bạn bấm Allow', 'not logged in — a browser opens so you can press Allow', '未登录 — 将打开浏览器让你点击 Allow'],
    ['chưa cài', 'not installed', '未安装'],
    ['Cài lại / cập nhật Worker', 'Reinstall / update Worker', '重新安装 / 更新 Worker'],
    ['Cài đặt & đăng nhập', 'Install & log in', '安装并登录'],
    ['Mở trang duyệt của Cloudflare', 'Open the Cloudflare approval page', '打开 Cloudflare 授权页面'],
    ['Trình duyệt chưa tự mở? ', 'Browser did not open? ', '浏览器没有自动打开？'],
    ['. Duyệt xong quay lại đây, tự chạy tiếp.', '. Approve, then come back — it continues by itself.', '。授权后回到这里，会自动继续。'],
    ['Gỡ Worker?', 'Remove the Worker?', '移除 Worker？'],
    ['Xoá Worker ds-egress khỏi Cloudflare và xoá khoá. Từ giờ mọi acc đi thẳng bằng IP nhà (hoặc proxy riêng của từng acc).', 'Deletes the ds-egress Worker from Cloudflare and its key. From now on every account goes direct with your home IP (or its own proxy).', '从 Cloudflare 删除 ds-egress Worker 并清除密钥。此后所有账号直接使用本机 IP（或各自的代理）。'],
    ['Gỡ', 'Remove', '移除'],
    ['gỡ', 'remove', '移除'],
    ['kiểm tra', 'check', '检查'],
    ['đang dùng', 'in use', '使用中'],
    ['đang tắt', 'off', '已关闭'],
    ['chưa đo IP — bấm “kiểm tra”', 'IP not measured yet — press “check”', '尚未检测 IP — 点击“检查”'],
    ['Số liệu do whalegate đếm các kết nối nó mở qua Worker (mỗi kết nối tính 1 request). Cloudflare tính hạn mức riêng — Free là 100.000 request/ngày, đặt lại lúc 00:00 UTC; số chính xác xem trên dash.cloudflare.com. Đổi hạn mức trong Cài đặt nếu bạn dùng gói trả phí.', 'Figures are counted by whalegate from the connections it opens through the Worker (each counts as 1 request). Cloudflare meters its own quota — Free is 100,000 requests/day, reset at 00:00 UTC; see dash.cloudflare.com for exact numbers. Change the quota in Settings if you are on a paid plan.', '数据由 whalegate 统计其经 Worker 建立的连接（每个计 1 次请求）。Cloudflare 有自己的额度计量 — 免费版每天 10 万次请求，UTC 00:00 重置；精确数字请见 dash.cloudflare.com。使用付费版请在设置中修改额度。'],
    ['Kiểm tra đăng nhập Cloudflare…', 'Checking the Cloudflare login…', '正在检查 Cloudflare 登录…'],
    ['Đang deploy Worker…', 'Deploying the Worker…', '正在部署 Worker…'],
    ['Đặt khoá bí mật cho Worker…', 'Setting the Worker secret key…', '正在设置 Worker 密钥…'],
    ['Chờ Worker sẵn sàng và đo IP đầu ra…', 'Waiting for the Worker and measuring the egress IP…', '等待 Worker 就绪并检测出口 IP…'],
    ['Xong. Mọi request và cửa sổ Chrome của acc giờ đi qua Worker.', 'Done. Every request and account Chrome window now goes through the Worker.', '完成。所有请求和账号的 Chrome 窗口现在都经 Worker 发出。'],
    ['Worker Cloudflare (IP đầu ra)', 'Cloudflare Worker (egress IP)', 'Cloudflare Worker（出口 IP）'],
    ['Địa chỉ Worker từ worker/deploy-egress.sh — gọi API/HIF/đăng ký HTTP qua Cloudflare. Chrome (đăng nhập, đăng ký, làm mới thiết bị) không đi qua được. Cần khởi động lại', 'Worker address from worker/deploy-egress.sh — API/HIF/HTTP sign-up calls go through Cloudflare. Chrome windows (login, sign-up, device refresh) cannot use it. Restart required', 'worker/deploy-egress.sh 输出的 Worker 地址 — API/HIF/HTTP 注册请求经 Cloudflare 发出。Chrome 窗口（登录、注册、刷新设备）无法使用。需重启'],
    ['Khoá Worker', 'Worker key', 'Worker 密钥'],
    ['Nội dung ~/.whalegate/egress-key (cần khởi động lại)', 'Contents of ~/.whalegate/egress-key (restart required)', '~/.whalegate/egress-key 的内容（需重启）'],
    ['Traffic nền như web', 'Web-like background traffic', '类网页后台流量'],
    ['Tắt nếu nghi DeepSeek đánh dấu acc vì các request nền này', 'Turn off if you suspect DeepSeek flags accounts for these background requests', '如怀疑 DeepSeek 因这些后台请求标记账号，请关闭'],
    ['đang mở…', 'opening…', '正在打开…'],
    ['Mở đúng Chrome của acc này (cùng phiên, vân tay, mật khẩu đã lưu) để bạn tự giải captcha hoặc thao tác; đóng cửa sổ thì phiên được lưu lại', 'Opens this account\'s own Chrome (same session, fingerprint and saved passwords) so you can solve the captcha or work by hand; closing the window saves the session', '打开该账号专属的 Chrome（相同会话、指纹和已保存的密码），供你手动过验证码或操作；关闭窗口后会话自动保存'],
    ['cùng phiên, cùng vân tay. Giải captcha / gửi thử một tin xong thì đóng cửa sổ, whalegate tự lưu phiên', 'same session, same fingerprint. Solve the captcha / send a test message, then close the window — whalegate saves the session', '相同会话、相同指纹。完成验证码 / 发送一条测试消息后关闭窗口，whalegate 会自动保存会话'],
    ['ok (đã mở Chrome)', 'ok (Chrome opened)', 'ok（已打开 Chrome）'],
    ['Làm mới cookie thiết bị ngay (Chrome nền của acc)', 'Refresh the device cookies now (the account\'s background Chrome)', '立即刷新设备 Cookie（账号的后台 Chrome）'],
    ['đăng nhập lại', 'log in again', '重新登录'],
    ['Mở Chrome riêng của acc này (giữ vân tay và phiên Google cũ) để bạn đăng nhập lại; xong tự thay token vào acc', 'Opens this account\'s dedicated Chrome (keeping its fingerprint and old Google session) so you can log in again; the token is swapped in automatically afterwards', '打开该账号的专属 Chrome（保留其指纹和旧的 Google 会话）供你重新登录；完成后会自动替换 Token'],
    ['sửa', 'edit', '编辑'],
    ['verify', 'verify', '验证'],
    ['reset', 'reset', '重置'],
    ['xoá', 'delete', '删除'],
    /* log */
    ['tất cả', 'all', '全部'],
    ['lọc…', 'filter…', '筛选…'],
    ['Lọc log', 'Filter log', '筛选日志'],
    ['tạm dừng', 'pause', '暂停'],
    ['tiếp tục', 'resume', '继续'],
    ['↓ mới', '↓ new', '↓ 最新'],
    ['○ nối lại…', '○ reconnecting…', '○ 重新连接中…'],
    ['Không copy được', 'Could not copy', '无法复制'],
    /* tab do bookmarklet mở */
    ['Tab này phải do bookmarklet mở từ chat.deepseek.com', 'This tab must be opened by the bookmarklet from chat.deepseek.com', '此标签页必须由 chat.deepseek.com 上的书签脚本打开'],
    ['Đang nhận dữ liệu từ chat.deepseek.com…', 'Receiving data from chat.deepseek.com…', '正在从 chat.deepseek.com 接收数据…'],
    ['Đang thêm tài khoản…', 'Adding the account…', '正在添加账号…'],
    ['Không nhận được dữ liệu từ chat.deepseek.com — thử bấm bookmarklet lại', 'No data received from chat.deepseek.com — try clicking the bookmarklet again', '未收到来自 chat.deepseek.com 的数据 — 请再次点击书签脚本'],
    ['Bookmarklet không gom được dữ liệu', 'The bookmarklet could not collect the data', '书签脚本无法收集数据'],

    ['Kéo lên thanh bookmark', 'Drag to the bookmarks bar', '拖到书签栏'],
    ['gửi về', 'sends to', '发送到'],
    ['Mở một cửa sổ Chrome riêng (profile và vân tay riêng cho tài khoản này). Bạn đăng nhập DeepSeek trong đó bằng cách nào cũng được — email, Google, mã OTP — hoặc đăng ký tài khoản mới ngay trong cửa sổ đó. whalegate bắt token và cookie qua CDP, lưu tài khoản rồi tự đóng cửa sổ.',
      'Opens a separate Chrome window with its own profile and fingerprint for this account. Log in to DeepSeek there any way you like — email, Google, OTP — or sign up for a new account right in that window. whalegate captures the token and cookies over CDP, saves the account and closes the window by itself.',
      '打开一个独立的 Chrome 窗口（该账号专属的配置文件和指纹）。你可以在其中用任意方式登录 DeepSeek — 邮箱、Google、验证码均可 — 也可以直接在该窗口里注册新账号。whalegate 会通过 CDP 获取 Token 和 Cookie，保存账号并自动关闭窗口。'],
    ['Mở cửa sổ riêng để đăng nhập / đăng ký', 'Open dedicated window to log in / sign up', '打开专属窗口登录 / 注册'],
    ['Dán token (legacy)', 'Paste token (legacy)', '粘贴 Token（旧方式）'],
    ['Mở cửa sổ Chrome riêng rồi bạn tự đăng nhập trong đó', 'Opens a dedicated Chrome window and you log in there yourself', '打开专属 Chrome 窗口，由你在其中自行登录'],
    ['Tự điền email + mật khẩu (kém ổn định)', 'Auto-fill email + password (less reliable)', '自动填写邮箱和密码（稳定性较差）'],
    ['Hiển thị', 'Display', '显示'],
    ['Tỷ giá USD → CNY', 'USD → CNY rate', '美元兑人民币汇率'],
    ['Tính bằng 0,01 ¥: 720 = 7,20 ¥ cho 1 USD. Dùng khi dashboard ở tiếng Trung', 'In units of 0.01 ¥: 720 = 7.20 ¥ per 1 USD. Used when the dashboard is in Chinese', '以 0.01 ¥ 为单位：720 = 1 美元兑 7.20 ¥。控制面板为中文时使用'],
    ['×0,01 ¥', '×0.01 ¥', '×0.01 ¥'],
    ['Tỷ giá USD → VND', 'USD → VND rate', '美元兑越南盾汇率'],
    ['Số đồng cho 1 USD. Dùng khi dashboard ở tiếng Việt', 'Dong per 1 USD. Used when the dashboard is in Vietnamese', '1 美元兑多少越南盾。控制面板为越南语时使用'],
    ['đồng', 'VND', '越南盾'],
    ['Log trực tiếp', 'Live log', '实时日志'],
    ['Khoảng thời gian', 'Time range', '时间范围'],
    ['Token 7 ngày', 'Tokens 7 days', '7 天 Token'],
    ['Token 30 ngày', 'Tokens 30 days', '30 天 Token'],
    ['Token toàn bộ', 'Tokens all time', '累计 Token'],
    ['Tiết kiệm 7 ngày', 'Saved 7 days', '7 天节省'],
    ['Tiết kiệm 30 ngày', 'Saved 30 days', '30 天节省'],
    ['Tiết kiệm toàn bộ', 'Saved all time', '累计节省'],
    ['7 ngày qua', 'Last 7 days', '过去 7 天'],
    ['30 ngày qua', 'Last 30 days', '过去 30 天'],
    ['Toàn bộ', 'All time', '全部'],
    ['export', 'export', '导出'],
    ['24 giờ', '24 h', '24 小时'],
    ['3 ngày', '3 days', '3 天'],
    ['Khoảng thời gian xuất', 'Export range', '导出范围'],
    ['Tải file debug: log + chi tiết nối lại / challenge / cuối luồng SSE (không có token hay cookie). Đã lưu sẵn ở thư mục logs/ trong thư mục dữ liệu.', 'Download a debug file: log + details of resumes / challenges / SSE tails (no tokens or cookies). Already saved in the logs/ folder of the data directory.', '下载调试文件：日志 + 续接 / 验证码 / SSE 尾部的详细记录（不含 Token 和 Cookie）。已保存在数据目录的 logs/ 文件夹中。'],
    ['Đang tạo file debug…', 'Preparing the debug file…', '正在生成调试文件…'],
    ['Đăng nhập bằng Chrome riêng', 'Logged in with a dedicated Chrome', '通过专属 Chrome 登录'],
    ['Quét từ Chrome chính', 'Scanned from the main Chrome', '从主 Chrome 扫描'],
    ['Thêm bằng bookmarklet', 'Added with the bookmarklet', '通过书签脚本添加'],
    ['Thêm bằng tiện ích', 'Added with the extension', '通过浏览器扩展添加'],
    ['Thêm bằng cách dán token', 'Added by pasting a token', '通过粘贴 Token 添加'],
    ['Trình duyệt gốc (bookmarklet hoặc tiện ích)', 'Original browser (bookmarklet or extension)', '原浏览器（书签脚本或扩展）'],
    ['Đăng ký bằng dòng lệnh (register)', 'Signed up from the command line (register)', '通过命令行注册（register）'],
    ['Sao lưu đầy đủ', 'Full backup', '完整备份'],
    ['Tài khoản + cài đặt + proxy + lịch sử dùng + profile Chrome riêng, một file .tar.gz', 'Accounts + settings + proxies + usage history + dedicated Chrome profiles in one .tar.gz file', '账号 + 设置 + 代理 + 使用历史 + 专属 Chrome 配置文件，打包成一个 .tar.gz'],
    ['): thêm cài đặt, danh sách và ghim proxy, lịch sử dùng và profile Chrome riêng của từng tài khoản — đủ để chuyển nguyên vẹn sang máy khác. Cookie trong profile Chrome gắn với hệ điều hành nên máy khác hệ điều hành có thể phải đăng nhập lại.', '): adds the settings, the proxy list and bindings, the usage history and the dedicated Chrome profile of every account — enough to move everything to another machine. Cookies inside a Chrome profile are tied to the operating system, so a machine with a different OS may need a new login.', '): 额外包含设置、代理列表与绑定、使用历史以及每个账号的专属 Chrome 配置文件 — 足以把一切完整迁移到另一台机器。Chrome 配置文件里的 Cookie 与操作系统绑定，不同操作系统的机器可能需要重新登录。'],
    ['Đang tạo bản sao lưu đầy đủ…', 'Preparing the full backup…', '正在生成完整备份…'],
    ['Chọn file gói export hoặc bản sao lưu đầy đủ (', 'Pick an export file or a full backup (', '选择导出文件或完整备份（'],
    ['), hoặc dán JSON vào ô dưới (dùng khi đã copy clipboard ở máy kia).', '), or paste the JSON below (use this when you copied it to the clipboard on the other machine).', '），或把 JSON 粘贴到下方（在另一台机器上复制到剪贴板时使用）。'],
    ['Ghi đè dữ liệu đã có (token/cookie, cài đặt, proxy, profile…)', 'Overwrite existing data (token/cookie, settings, proxies, profiles…)', '覆盖已有数据（Token/Cookie、设置、代理、配置文件…）'],
    ['Đang đọc bản sao lưu…', 'Reading the backup…', '正在读取备份…'],
    ['Khôi phục?', 'Restore?', '恢复？'],
    ['Khôi phục', 'Restore', '恢复'],
    ['Gộp bản sao lưu vào máy này: tài khoản, cài đặt, proxy, lịch sử dùng và profile Chrome riêng. Không xoá tài khoản nào đang có.', 'Merges the backup into this machine: accounts, settings, proxies, usage history and dedicated Chrome profiles. No existing account is deleted.', '将备份合并到本机：账号、设置、代理、使用历史和专属 Chrome 配置文件。不会删除任何现有账号。'],
    [' Dữ liệu đã có sẽ bị ghi đè.', ' Existing data will be overwritten.', ' 已有数据将被覆盖。'],
    ['Đã khôi phục bản sao lưu', 'Backup restored', '备份已恢复'],
    ['Đang có cửa sổ đăng nhập mở nên chưa khôi phục được profile Chrome', 'A login window is open, so the Chrome profiles could not be restored', '当前有登录窗口打开，暂时无法恢复 Chrome 配置文件'],
    ['Lọc log: tất cả hoặc chỉ lỗi', 'Filter the log: everything or errors only', '筛选日志：全部或仅错误'],
    /* alert() của bookmarklet / lệnh console (chạy ở tab khác, dịch lúc dựng mã) */
    ['chạy trên tab chat.deepseek.com', 'run this on a chat.deepseek.com tab', '请在 chat.deepseek.com 标签页中运行'],
    ['tab này chưa đăng nhập chat.deepseek.com (không thấy userToken).', 'this tab is not logged in to chat.deepseek.com (no userToken found).', '此标签页尚未登录 chat.deepseek.com（找不到 userToken）。'],
    ['bấm bookmarklet này trên tab chat.deepseek.com', 'click this bookmarklet on a chat.deepseek.com tab', '请在 chat.deepseek.com 标签页上点击此书签脚本'],
    ['trình duyệt chặn tab mới — cho phép popup rồi bấm lại', 'the browser blocked the new tab — allow pop-ups and click again', '浏览器拦截了新标签页 — 请允许弹出窗口后重试'],
    ['đăng nhập đúng tài khoản này, xong cửa sổ tự đóng và acc quay lại pool', 'log in to exactly this account; the window closes by itself and the account goes back to the pool', '请登录这个账号；完成后窗口会自动关闭，账号随即回到账号池'],
    /* lỗi do server trả về (transfer.rs / server.rs / chrome_capture.rs / proxypool.rs / settings.rs) */
    ['đang là bản mới nhất', 'already on the latest version', '已是最新版本'],
    ['không có bản dựng sẵn cho nền tảng này — build từ source', 'there is no prebuilt binary for this platform — build from source', '此平台没有预编译版本 — 请从源码构建'],
    ['checksum không khớp — huỷ cập nhật', 'checksum mismatch — update cancelled', '校验和不匹配 — 已取消更新'],
    ['zip chỉ dùng trên Windows', 'zip is only used on Windows', 'zip 仅用于 Windows'],
    ['release không có tag_name', 'the release has no tag_name', '该发布缺少 tag_name'],
    ['gói import không có mảng accounts', 'the import bundle has no accounts array', '导入包中没有 accounts 数组'],
    ['accounts phải là mảng', 'accounts must be an array', 'accounts 必须是数组'],
    ['không có tài khoản hợp lệ trong gói (token thiếu hoặc sai định dạng)', 'no valid account in the bundle (token missing or malformed)', '导入包中没有有效账号（缺少 Token 或格式错误）'],
    ['mode phải là idle | now | cancel', 'mode must be idle | now | cancel', 'mode 必须是 idle | now | cancel'],
    ['không cấu hình được profile nào', 'could not configure any profile', '没有任何 profile 配置成功'],
    ['không đọc được token — dán đúng giá trị localStorage.userToken', 'could not read the token — paste the value of localStorage.userToken', '无法读取 Token — 请粘贴 localStorage.userToken 的值'],
    ['không thấy account', 'account not found', '未找到账号'],
    ['không tìm thấy tài khoản', 'account not found', '未找到账号'],
    ['tài khoản đã bị xoá', 'the account has been deleted', '该账号已被删除'],
    ['đang có một cửa sổ đăng nhập mở — đợi nó xong rồi thử lại', 'a login window is already open — wait for it to finish and try again', '已有一个登录窗口打开 — 请等它完成后再试'],
    ['đang có một cửa sổ đăng nhập mở — đăng nhập xong hoặc đóng cửa sổ đó rồi thử lại', 'a login window is already open — finish logging in or close it, then try again', '已有一个登录窗口打开 — 请先完成登录或关闭它，然后重试'],
    ['acc này dùng trình duyệt gốc: đăng nhập lại trên trình duyệt đó rồi bấm tiện ích/bookmarklet trên tab chat.deepseek.com', 'this account uses the original browser: log in again there, then click the extension/bookmarklet on a chat.deepseek.com tab', '该账号使用原浏览器：请在那个浏览器中重新登录，然后在 chat.deepseek.com 标签页点击扩展/书签脚本'],
    ['tài khoản này dùng trình duyệt gốc — không tự làm mới thiết bị; bấm tiện ích/bookmarklet trên tab chat.deepseek.com của trình duyệt đó để làm mới', 'this account uses the original browser — its device cannot be refreshed here; click the extension/bookmarklet on a chat.deepseek.com tab of that browser to refresh it', '该账号使用原浏览器 — 无法在此刷新设备；请在那个浏览器的 chat.deepseek.com 标签页点击扩展/书签脚本来刷新'],
    ['tài khoản này không có thiết bị riêng (chỉ acc đăng nhập qua Chrome mới làm mới được)', 'this account has no device of its own (only accounts logged in through Chrome can be refreshed)', '该账号没有独立设备（只有通过 Chrome 登录的账号才能刷新）'],
    ['token của acc đổi trong lúc dựng thiết bị — bỏ kết quả', 'the account token changed while the device was being built — result discarded', '构建设备期间账号 Token 已变化 — 已丢弃结果'],
    ['admin chỉ cho phép từ localhost', 'admin is only allowed from localhost', '管理接口仅允许从 localhost 访问'],
    ['whalegate đang được truy cập từ ngoài máy mà chưa đặt DS_API_KEY — đặt DS_API_KEY trước khi mở ra Internet', 'whalegate is being reached from outside this machine but DS_API_KEY is not set — set DS_API_KEY before exposing it to the Internet', '正在从本机以外访问 whalegate，但尚未设置 DS_API_KEY — 对外网开放前请先设置 DS_API_KEY'],
    ['không tìm thấy tab chat.deepseek.com đã login — bấm \'Sinh link OAuth2\', chọn account Google, rồi bấm lại', 'no logged-in chat.deepseek.com tab found — click \'Generate OAuth2 link\', pick the Google account, then click again', '未找到已登录的 chat.deepseek.com 标签页 — 请点击“生成 OAuth2 链接”，选择 Google 账号后再点击一次'],
    ['không tìm được cổng CDP trống trong 40000–60000', 'could not find a free CDP port in 40000–60000', '在 40000–60000 范围内找不到空闲的 CDP 端口'],
    ['không tìm thấy Chrome/Chromium/Edge trên máy — cài Google Chrome hoặc đặt DS_CHROME_BIN', 'no Chrome/Chromium/Edge found on this machine — install Google Chrome or set DS_CHROME_BIN', '本机未找到 Chrome/Chromium/Edge — 请安装 Google Chrome 或设置 DS_CHROME_BIN'],
    ['tab chưa login (userToken rỗng)', 'the tab is not logged in (userToken is empty)', '标签页尚未登录（userToken 为空）'],
    ['cửa sổ đăng nhập đã bị đóng trước khi login xong', 'the login window was closed before login finished', '登录完成前登录窗口已被关闭'],
    ['trang đăng nhập không render form (thiếu input mật khẩu) sau 15s', 'the login page did not render the form (no password input) after 15s', '15 秒后登录页仍未渲染出表单（缺少密码输入框）'],
    ['không kết nối được Chrome CDP :9333', 'could not connect to Chrome CDP :9333', '无法连接 Chrome CDP :9333'],
    ['không có tab chat.deepseek.com để refresh cookies', 'no chat.deepseek.com tab to refresh cookies from', '没有可用于刷新 Cookie 的 chat.deepseek.com 标签页'],
    ['không áp được vân tay lên tab đầu trong 15s', 'could not apply the fingerprint to the first tab within 15s', '15 秒内无法将指纹应用到第一个标签页'],
    ['SDK Shumei không trả device id sau 20s', 'the Shumei SDK returned no device id after 20s', '20 秒后数美 SDK 仍未返回设备 ID'],
    ['trang không nhận token (đang có token khác)', 'the page did not accept the token (a different token is present)', '页面未接受该 Token（当前存在另一个 Token）'],
    ['không đọc được phiên từ trang sau 30s', 'could not read the session from the page after 30s', '30 秒后仍无法从页面读取会话'],
    ['dòng rỗng', 'empty line', '空行'],
    ['phần đăng nhập phải là user:pass', 'the credentials must be user:pass', '认证部分必须是 user:pass'],
    ['port 0 không dùng được', 'port 0 cannot be used', '端口 0 不可用'],
    ['chứa ký tự NUL', 'contains a NUL character', '包含 NUL 字符'],
    ['quá 64KB', 'larger than 64KB', '超过 64KB'],
  ];

  const NM = '((?:(?! — ).)+?)'; /* tên acc/nhãn: dừng trước " — " */
  const rx = (src) => new RegExp(src.replace(/\(\.\+\)/g, NM));

  /* ---------- luật cho câu có số / tên: [regex, en, zh] — $n giữ nguyên nhóm, %n dịch nhóm ---------- */
  const RULES = [
    /* số liệu */
    [/^(\d+) lượt$/, '$1 req', '$1 次'],
    [/^tháng này ~(.+)$/, 'this month ~$1', '本月约 $1'],
    [/^(\d+) lỗi$/, '$1 error(s)', '$1 个错误'],
    [/^(\d+) lỗi \((.*)\)$/, '$1 error(s) ($2)', '$1 个错误（$2）'],
    [/^lỗi (\d+)$/, 'error $1', '错误 $1'],
    [/^TB (.+)\/ngày$/, 'avg $1/day', '日均 $1'],
    [/^ok (\d+)$/, 'ok $1', '成功 $1'],
    [/^rỗng (\d+)$/, 'empty $1', '空响应 $1'],
    [/^chạy (\d+)$/, 'running $1', '运行中 $1'],
    [/^nghỉ (\d+)s$/, 'cooldown $1s', '冷却 $1 秒'],
    [/^Đăng nhập bị từ chối: (.+)$/, 'Login refused: $1', '登录被拒绝：$1'],
    [/^DeepSeek từ chối đăng nhập: (.+)$/, 'DeepSeek refused the login: $1', 'DeepSeek 拒绝了登录：$1'],
    [/^Đăng nhập nhầm: cửa sổ vào (.+) chứ không phải acc này — bấm đăng nhập lại và chọn đúng tài khoản$/, 'Wrong login: the window signed in as $1, not this account — log in again and pick the right one', '登录错误：窗口登录的是 $1，不是该账号 — 请重新登录并选择正确的账号'],
    [/^chưa có — whalegate sẽ tự tải (.+) \(~30 MB\) từ nodejs\.org và kiểm tra checksum$/, 'not found — whalegate will download $1 (~30 MB) from nodejs.org and verify its checksum', '未找到 — whalegate 将从 nodejs.org 下载 $1（约 30 MB）并校验'],
    [/^đang chạy: cài đặt…$/, 'running: install…', '正在运行：安装…'],
    [/^đang chạy: gỡ Worker…$/, 'running: remove Worker…', '正在运行：移除 Worker…'],
    [/^đang chạy: kiểm tra…$/, 'running: check…', '正在运行：检查…'],
    [/^Cloudflare Worker · (.+)$/, 'Cloudflare Worker · $1', 'Cloudflare Worker · $1'],
    [/^IP đầu ra (\S+)$/, 'egress IP $1', '出口 IP $1'],
    [/^trạm (\S+)$/, 'colo $1', '节点 $1'],
    [/^phản hồi (\d+) ms$/, 'response $1 ms', '响应 $1 毫秒'],
    [/^Hôm nay \(UTC\): (.+)$/, 'Today (UTC): $1', '今天（UTC）：$1'],
    [/^7 ngày: (.+)$/, '7 days: $1', '7 天：$1'],
    [/^30 ngày: (.+)$/, '30 days: $1', '30 天：$1'],
    [/^Đường hầm: (\d+) đang mở · (\d+) đã mở · (\d+) lỗi(?: · (\d+) đi thẳng \(đích nằm trên Cloudflare, vd hCaptcha\))?(?: · lỗi gần nhất: (.+))?$/, 'Tunnels: $1 open · $2 opened · $3 failed · $4 direct (target on Cloudflare, e.g. hCaptcha)', '隧道：$1 个已打开 · 共打开 $2 个 · $3 个失败 · $4 个直连（目标在 Cloudflare 上，如 hCaptcha）'],
    [/^Đang tải Node\.js (\S+) từ nodejs\.org \((.+)\)…$/, 'Downloading Node.js $1 from nodejs.org ($2)…', '正在从 nodejs.org 下载 Node.js $1（$2）…'],
    [/^Dùng account Cloudflare: (.+)$/, 'Using Cloudflare account: $1', '使用 Cloudflare 账号：$1'],
    [/^DeepSeek khoá chat tài khoản này \(muted\) tới (.+)$/, 'DeepSeek muted this account until $1', 'DeepSeek 已禁言该账号，至 $1'],
    [/^DeepSeek giới hạn tốc độ — nghỉ (\d+)s \(trần học được (\d+)\/phút\)$/, 'DeepSeek rate limit — resting $1s (learned cap $2/min)', 'DeepSeek 限速 — 休息 $1 秒（学到上限 $2 次/分钟）'],
    [/^đang chạy (\d+)\/(\d+)$/, 'running $1/$2', '运行中 $1/$2'],
    [/^đang chạy (.+)$/, 'running $1', '当前运行 $1'],
    [/^(\d+) phút trước$/, '$1 min ago', '$1 分钟前'],
    [/^(\d+) giờ trước$/, '$1 h ago', '$1 小时前'],
    [/^(\d+) ngày trước$/, '$1 d ago', '$1 天前'],
    [/^làm mới (.+)$/, 'refreshed %1', '刷新于 %1'],
    [/^mất token (.+)$/, 'token lost %1', 'Token 丢失于 %1'],
    [/^(\d+) tài khoản$/, '$1 accounts', '$1 个账号'],
    /* băng đỏ / băng cập nhật */
    [/^DeepSeek đã đăng xuất (\d+) acc$/, 'DeepSeek logged out $1 account(s)', 'DeepSeek 已让 $1 个账号登出'],
    [/^(\d+) acc cùng lúc \((.+), DS huỷ hàng loạt\)$/, '$1 accounts at once ($2, DS revoked in bulk)', '$1 个账号同时被登出（$2，DS 批量登出）'],
    [/^lần gần nhất (.+)$/, 'most recent $1', '最近一次 $1'],
    [/^Bản mới (.+)$/, 'New version $1', '新版本 $1'],
    [/^Cập nhật lên (.+)$/, 'Update to $1', '更新到 $1'],
    /* thêm / đăng nhập */
    [/^Đang đăng nhập (.+)…$/, 'Logging in $1…', '正在登录 $1…'],
    [rx('^Đã đăng nhập (.+)$'), 'Logged in $1', '已登录 $1'],
    [/^\+(\d+) mới, (\d+) làm mới$/, '+$1 new, $2 refreshed', '新增 $1 个，刷新 $2 个'],
    [rx('^Đã mở Chrome của (.+)$'), 'Opened the Chrome of $1', '已打开 $1 的 Chrome'],
    [/^đăng nhập đúng tài khoản này, xong cửa sổ tự đóng$/, 'log in to exactly this account; the window closes by itself when done', '请登录这个账号，完成后窗口会自动关闭'],
    [rx('^Đã thay token mới cho (.+)$'), 'Replaced the token of $1', '已更换 $1 的 Token'],
    [rx('^Đã làm mới cookie/thiết bị cho (.+)$'), 'Refreshed the cookies/device of $1', '已刷新 $1 的 Cookie/设备'],
    [rx('^Đã thêm (.+?)( \\(giữ vân tay trình duyệt gốc\\))?$'),
      (m, t) => 'Added ' + t(m[1]) + (m[2] ? ' (original browser fingerprint kept)' : ''),
      (m, t) => '已添加 ' + t(m[1]) + (m[2] ? '（保留原浏览器指纹）' : '')],
    [/^Đang làm mới thiết bị (.+)…$/, 'Refreshing the device of $1…', '正在刷新 $1 的设备…'],
    [/^(.+): đã làm mới cookie thiết bị$/, '$1: device cookies refreshed', '$1：设备 Cookie 已刷新'],
    [rx('^Đã (bật|tắt) (.+)$'), (m) => (m[1] === 'bật' ? 'Enabled ' : 'Disabled ') + m[2], (m) => (m[1] === 'bật' ? '已启用 ' : '已禁用 ') + m[2]],
    /* verify / reset / xoá */
    [/^Kiểm tra token VÀ chat thử một lượt tối thiểu với (.+)\. Một lượt chat thật là cách duy nhất biết acc có đang bị throttle không\.$/,
      'Checks the token AND runs a minimal test chat with $1. A real chat turn is the only way to know whether the account is being throttled.',
      '检查 Token，并对 $1 进行一次最小化的试聊。只有真实聊天一轮才能知道账号是否被限流。'],
    [/^Đang verify (.+) \(chat thử\)…$/, 'Verifying $1 (test chat)…', '正在验证 $1（试聊）…'],
    [/^(.+): chat được, đã hết cooldown$/, '$1: chat works, cooldown is over', '$1：可以聊天，冷却已结束'],
    [/^(.+): token còn sống nhưng DeepSeek đã khoá chat \(mute\) — chờ hết hạn khoá, đăng nhập lại không giúp được$/, '$1: the token is alive but DeepSeek has muted chat — wait for the mute to end; logging in again will not help', '$1：Token 有效，但 DeepSeek 已禁言聊天 — 等待禁言结束，重新登录无济于事'],
    [/^(.+): token còn sống nhưng chat trả rỗng — đang bị throttle$/, '$1: the token is alive but chat returns empty — throttled', '$1：Token 有效，但聊天返回空 — 正被限流'],
    [/^(.+): token bị từ chối — cần đăng nhập lại$/, '$1: the token was rejected — log in again', '$1：Token 被拒绝 — 需要重新登录'],
    [/^(.+): không kết luận được \(lỗi mạng\/PoW\?\)$/, '$1: inconclusive (network/PoW error?)', '$1：无法判断（网络/PoW 错误？）'],
    [/^Xoá lỗi, cooldown và phiên web đang giữ của (.+), rồi chat thử một lượt để xác nhận acc dùng được\. Hội thoại đang dùng nó sẽ gửi lại đủ ngữ cảnh\.$/,
      'Clears the error, cooldown and held web session of $1, then runs a test chat to confirm the account works. Conversations using it will resend their full context.',
      '清除 $1 的错误、冷却和持有的网页会话，然后试聊一次以确认账号可用。正在使用它的对话会重新发送完整上下文。'],
    [rx('^Đã reset (.+)$'), 'Reset $1', '已重置 $1'],
    [/^Xoá (.+) khỏi pool\. Không hoàn tác được\.$/, 'Removes $1 from the pool. This cannot be undone.', '将 $1 从账号池中删除。此操作无法撤销。'],
    [rx('^Đã xoá (.+)$'), 'Deleted $1', '已删除 $1'],
    [/^Kiểm tra token của (\d+) tài khoản với DeepSeek \(mỗi tài khoản một lượt gọi\)\.$/, 'Checks the tokens of $1 accounts against DeepSeek (one call per account).', '向 DeepSeek 检查 $1 个账号的 Token（每个账号调用一次）。'],
    [/^Xoá lỗi, cooldown và phiên web đang giữ của cả (\d+) tài khoản, rồi kiểm tra lại token\. Mọi hội thoại stateful đang chạy sẽ gửi lại đủ ngữ cảnh ở bước kế\.$/,
      'Clears the error, cooldown and held web session of all $1 accounts, then re-checks the tokens. Every running stateful conversation will resend its full context on the next step.',
      '清除全部 $1 个账号的错误、冷却和持有的网页会话，然后重新检查 Token。所有正在运行的有状态对话会在下一步重新发送完整上下文。'],
    [/^(Verify|Reset) (\d+)\/(\d+)…$/, '$1 $2/$3…', (m) => (m[1] === 'Verify' ? '验证' : '重置') + ' ' + m[2] + '/' + m[3] + '…'],
    [/^(Verify|Reset) xong: (\d+) ok$/, '$1 done: $2 ok', (m) => (m[1] === 'Verify' ? '验证' : '重置') + '完成：' + m[2] + ' 个成功'],
    /* sửa */
    [/^ID DeepSeek (.+)$/, 'DeepSeek ID $1', 'DeepSeek ID $1'],
    [/^Đã lưu (.*)$/, 'Saved $1', '已保存 $1'],
    [/^Đã lưu$/, 'Saved', '已保存'],
    /* cài đặt */
    [/^Không tải được cài đặt: (.+)$/, 'Could not load the settings: $1', '无法加载设置：$1'],
    [/^Bỏ thay đổi (.+)$/, 'Discard the change to %1', '放弃对“%1”的修改'],
    [/^(\d+) thay đổi chưa lưu$/, '$1 unsaved change(s)', '$1 项修改未保存'],
    [/^trong khoảng (.+)$/, 'within the range $1', '范围为 $1'],
    [/^một trong: (.+)$/, 'one of: $1', '可选值：$1'],
    /* dsh */
    [/^Chưa có profile dsh nào trong (.+) — cài dsh \(hoặc mở app Desktop\) một lần để nó tạo profile\.$/, 'No dsh profile in $1 yet — install dsh (or open the Desktop app) once so it creates one.', '$1 中还没有 dsh profile — 请安装 dsh（或打开桌面应用）一次以创建 profile。'],
    [/^Đã cấu hình (\d+) profile \((\d+) thay đổi\)$/, 'Configured $1 profile(s) ($2 changed)', '已配置 $1 个 profile（$2 个有变化）'],
    [/^lỗi: (.+)$/, 'error: $1', '错误：$1'],
    [/^(.*) — mở lại dsh \/ app Desktop rồi chọn "DeepSeek Web \(whalegate, free\)"$/,
      (m, t) => t(m[1]) + ' — reopen dsh / the Desktop app and pick "DeepSeek Web (whalegate, free)"',
      (m, t) => t(m[1]) + ' — 请重新打开 dsh / 桌面应用，然后选择“DeepSeek Web (whalegate, free)”'],
    /* export / import */
    [/^Không tạo được gói export: (.+)$/, 'Could not create the export bundle: $1', '无法生成导出包：$1'],
    [rx('^Đã tải (.+)$'), 'Downloaded $1', '已下载 $1'],
    [/^JSON không hợp lệ: (.+)$/, 'Invalid JSON: $1', 'JSON 无效：$1'],
    [/^Sẽ nhập: (\d+) mới$/, 'Will import: $1 new', '将导入：$1 个新增'],
    [/^(\d+) ghi đè$/, '$1 overwritten', '$1 个覆盖'],
    [/^(\d+) bỏ qua \(đã có\)$/, '$1 skipped (already present)', '$1 个跳过（已存在）'],
    [/^pool hiện (\d+) tài khoản\.$/, 'the pool now has $1 accounts.', '账号池现有 $1 个账号。'],
    [/^Gộp tài khoản từ gói vào pool(, ghi đè token\/cookie của tài khoản đã có)?\. Không xoá tài khoản nào đang có\.$/,
      (m) => 'Merges the accounts from the bundle into the pool' + (m[1] ? ', overwriting the token/cookie of existing accounts' : '') + '. No existing account is deleted.',
      (m) => '将导入包中的账号合并到账号池' + (m[1] ? '，并覆盖已有账号的 Token/Cookie' : '') + '。不会删除任何现有账号。'],
    [/^Đã import: \+(\d+) mới, ~(\d+) cập nhật, (\d+) bỏ qua$/, 'Imported: +$1 new, ~$2 updated, $3 skipped', '已导入：新增 $1，更新 $2，跳过 $3'],
    /* cập nhật */
    [/^Tải (.+), thay binary và khởi động lại gateway\. Request đang chạy bị cắt; cấu hình và tài khoản giữ nguyên\.$/, 'Downloads $1, swaps the binary and restarts the gateway. Running requests are cut; settings and accounts are kept.', '下载 $1，替换可执行文件并重启网关。正在运行的请求会被中断；设置和账号保持不变。'],
    [/^Đang tải bản (.+)…$/, 'Downloading $1…', '正在下载 $1…'],
    [/^Đã cài (.+) · đang khởi động lại…$/, 'Installed $1 · restarting…', '已安装 $1 · 正在重启…'],
    [/^cập nhật lỗi (\d+)$/, 'update failed $1', '更新失败 $1'],
    [/^Cập nhật lỗi: (.+)$/, 'Update failed: $1', '更新失败：$1'],
    [/^Không dò được bản mới: (.+)$/, 'Could not check for a new version: $1', '无法检查新版本：$1'],
    /* misc */
    [/^Kéo lên thanh bookmark · gửi về (.+)$/, 'Drag to the bookmarks bar · sends to $1', '拖到书签栏 · 发送到 $1'],
    [/^Đã copy (\d+) dòng$/, 'Copied $1 line(s)', '已复制 $1 行'],
    [/^(\{.*\}) hoặc (\[.*\])$/, '$1 or $2', '$1 或 $2'],
    [/^Tài khoản: (\d+) mới · (\d+) ghi đè · (\d+) bỏ qua \(đã có\)$/, 'Accounts: $1 new · $2 overwritten · $3 skipped (already present)', '账号：$1 个新增 · $2 个覆盖 · $3 个跳过（已存在）'],
    [/^Profile Chrome riêng: (\d+)(?: \(bỏ qua (\d+) đã có\))?$/, (m) => 'Dedicated Chrome profiles: ' + m[1] + (m[2] ? ' (' + m[2] + ' already present, skipped)' : ''), (m) => '专属 Chrome 配置文件：' + m[1] + (m[2] ? '（' + m[2] + ' 个已存在，已跳过）' : '')],
    [/^Cài đặt: (\d+)$/, 'Settings: $1', '设置：$1'],
    [/^Proxy: (\d+) ghim( \+ danh sách)?$/, (m) => 'Proxy: ' + m[1] + ' binding(s)' + (m[2] ? ' + list' : ''), (m) => '代理：' + m[1] + ' 个绑定' + (m[2] ? ' + 列表' : '')],
    [/^Lịch sử dùng: (\d+) giờ$/, 'Usage history: $1 hour(s)', '使用历史：$1 小时'],
    /* lỗi động từ server */
    [/^không dò được bản mới: (.+)$/i, 'could not check for a new version: $1', '无法检查新版本：$1'],
    [/^đọc JSON: (.+)$/, 'reading JSON: $1', '读取 JSON 出错：$1'],
    [/^GitHub trả HTTP (\d+)$/, 'GitHub returned HTTP $1', 'GitHub 返回 HTTP $1'],
    [/^SHA256SUMS\.txt không có (.+)$/, 'SHA256SUMS.txt has no entry for $1', 'SHA256SUMS.txt 中没有 $1'],
    [/^không thấy (.+) trong archive$/, '$1 not found in the archive', '压缩包中找不到 $1'],
    [/^không thấy profile dsh nào trong (.+) — mở dsh \(hoặc app Desktop\) một lần để nó tạo profile$/, 'no dsh profile found in $1 — open dsh (or the Desktop app) once so it creates one', '$1 中没有 dsh profile — 请打开 dsh（或桌面应用）一次以创建 profile'],
    [/^token không hợp lệ: (.+)$/, 'invalid token: $1', 'Token 无效：$1'],
    [/^không mở được Chrome: (.+)$/, 'could not open Chrome: $1', '无法打开 Chrome：$1'],
    [/^không spawn được Chrome \((.+)\)$/, 'could not launch Chrome ($1)', '无法启动 Chrome（$1）'],
    [/^Host không hợp lệ: (.+)$/, 'invalid Host: $1', 'Host 无效：$1'],
    [/^(.+): giá trị không hợp lệ$/, '$1: invalid value', '$1：值无效'],
    [/^hết (\d+)s mà chưa login xong trong tab$/, 'login did not finish in the tab within $1s', '$1 秒内标签页仍未完成登录'],
    [/^không bấm được nút đăng nhập: (.+)$/, 'could not click the login button: $1', '无法点击登录按钮：$1'],
    [/^tab (.+) không xuất hiện sau khi áp vân tay$/, 'the tab $1 did not appear after applying the fingerprint', '应用指纹后标签页 $1 没有出现'],
    [/^scheme '(.+)' không hỗ trợ \((.+)\)$/, "scheme '$1' is not supported ($2)", "不支持 scheme“$1”（$2）"],
    [/^dòng (\d+): (.+)$/, 'line $1: %2', '第 $1 行：%2'],
    [/^không đọc được (.+): (.+)$/, 'could not read $1: $2', '无法读取 $1：$2'],
    [/^trong khoảng (.+)$/, 'within the range $1', '范围为 $1'],
  ];

  const LANGS = {vi: 'Tiếng Việt', en: 'English', zh: '简体中文'};
  const ATTRS = ['placeholder', 'title', 'aria-label'];
  const SKIP = '#logBody,#updNotes,[data-nt],script,style,textarea,code';

  const idx = {en: new Map(), zh: new Map()};
  const norm = (s) => s.replace(/\s+/g, ' ').trim();
  for (const [vi, en, zh] of DICT) { idx.en.set(norm(vi), en); idx.zh.set(norm(vi), zh); }

  let LANG = 'vi';

  /* `$n` giữ nhóm n, nhưng nếu nhóm còn chữ Việt (lỗi lồng nhau: "ngữ cảnh: nguyên nhân") thì dịch tiếp; `%n` luôn dịch */
  const HAS_VI = /[àáảãạăằắẳẵặâầấẩẫậèéẻẽẹêềếểễệìíỉĩịòóỏõọôồốổỗộơờớởỡợùúủũụưừứửữựỳýỷỹỵđĐ]/;
  function fill(tpl, m) {
    return tpl.replace(/([$%])(\d)/g, (x, k, i) => {
      const g = m[i] || '';
      return k === '%' || HAS_VI.test(g) ? tr(g) : g;
    });
  }

  /* dịch một câu đã chuẩn hoá; không có bản dịch thì trả lại nguyên văn */
  function tr(s) {
    if (LANG === 'vi' || !s) return s;
    const hit = idx[LANG].get(s);
    if (hit != null) return hit;
    const col = LANG === 'en' ? 1 : 2;
    for (const r of RULES) {
      const m = s.match(r[0]);
      if (!m) continue;
      const t = r[col];
      return typeof t === 'function' ? t(m, tr) : fill(t, m);
    }
    for (const sep of [' · ', ' — ', ': ']) {
      if (!s.includes(sep)) continue;
      const parts = s.split(sep), out = parts.map(tr);
      if (out.some((p, i) => p !== parts[i])) return out.join(sep);
    }
    const p = s.match(/^([·—→]\s+)([\s\S]+)$/);
    if (p) { const rest = tr(p[2]); if (rest !== p[2]) return p[1] + rest; }
    return s;
  }

  /* giữ khoảng trắng đầu/cuối của text node */
  function trKeep(raw) {
    if (LANG === 'vi') return raw;
    const m = raw.match(/^(\s*)([\s\S]*?)(\s*)$/);
    if (!m[2]) return raw;
    const core = norm(m[2]), out = tr(core);
    return out === core ? raw : m[1] + out + m[3];
  }

  const skipped = (n) => { const e = n.nodeType === 1 ? n : n.parentElement; return !e || !!e.closest(SKIP); };

  function trText(n) {
    if (skipped(n)) return;
    const cur = n.nodeValue;
    const src = n.__o !== undefined && cur === n.__o ? n.__s : cur;
    const out = trKeep(src);
    n.__s = src; n.__o = out;
    if (out !== cur) n.nodeValue = out;
  }

  function trAttr(e, name) {
    if (e.nodeType !== 1 || e.closest('[data-nt],script,style')) return;
    const cur = e.getAttribute(name);
    if (cur == null) return;
    const st = e.__a || (e.__a = {});
    const rec = st[name];
    const src = rec && cur === rec.o ? rec.s : cur;
    const out = trKeep(src);
    st[name] = {s: src, o: out};
    if (out !== cur) e.setAttribute(name, out);
  }

  function walk(root) {
    if (!root) return;
    if (root.nodeType === 3) return trText(root);
    if (root.nodeType !== 1) return;
    /* thuộc tính (placeholder/title/aria-label) dịch cả với <textarea>, <code>…; chỉ nội dung con mới bị bỏ qua */
    for (const a of ATTRS) trAttr(root, a);
    if (root.closest(SKIP)) return;
    for (let c = root.firstChild; c; c = c.nextSibling) walk(c);
  }

  let busy = false;
  const mo = new MutationObserver((recs) => {
    if (LANG === 'vi' && !busy) return;
    for (const r of recs) {
      if (r.type === 'characterData') trText(r.target);
      else if (r.type === 'attributes') trAttr(r.target, r.attributeName);
      else r.addedNodes.forEach(walk);
    }
  });

  function detect() {
    let saved = null;
    try { saved = localStorage.getItem('wg.lang'); } catch (e) {}
    if (saved && LANGS[saved]) return saved;
    const nav = String((navigator.languages && navigator.languages[0]) || navigator.language || 'en').toLowerCase();
    return nav.startsWith('vi') ? 'vi' : nav.startsWith('zh') ? 'zh' : 'en';
  }

  function setLang(l, persist) {
    if (!LANGS[l]) l = 'en';
    LANG = l;
    if (persist !== false) { try { localStorage.setItem('wg.lang', l); } catch (e) {} }
    document.documentElement.lang = l === 'zh' ? 'zh-CN' : l;
    document.querySelectorAll('.lang [data-l]').forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.l === l)));
    busy = true; walk(document.body); busy = false;
    try { if (typeof refreshBookmarklet === 'function') refreshBookmarklet(); } catch (e) {}
    try { if (typeof onLangChange === 'function') onLangChange(); } catch (e) {}
  }

  function init() {
    mo.observe(document.body, {childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ATTRS});
    setLang(detect(), false);
  }

  window.WG_I18N = {setLang: (l) => setLang(l, true), get lang() { return LANG; }, tr, LANGS, DICT, RULES, _walk: walk, _setLangRaw: (l) => { LANG = l; }};
  if (document.body) init(); else document.addEventListener('DOMContentLoaded', init);
})();
