# register / login — đăng ký và đăng nhập acc DeepSeek từ CLI

## Đăng nhập tự động (không cần bấm gì)

```bash
whalegate register login <email> <mật-khẩu> [--headless] [--label <tên>]
```

Mở Chrome riêng (profile + vân tay mới), tự điền form `sign_in`, bấm "Đăng nhập",
chờ token, verify qua `users/current` rồi lưu acc vào store. Không dùng
`shumei_verification` — trang đăng nhập không có captcha Shumei, nên luồng này
không phụ thuộc `fp.min.js`.

- Không có phiên đồ hoạ (máy chủ, sandbox) → tự chọn `--headless`.
- Chrome không mở được cửa sổ vì SUID sandbox → tự chuyển sang headless.
- Đọc thẳng `biz_code` của `POST /api/v0/users/login` nên báo đúng lý do:
  sai mật khẩu, tài khoản bị khoá, `RISK_DEVICE_DETECTED`, `OTP_REQUIRED`…
- Mật khẩu chỉ dùng một lần trong tiến trình, không ghi vào store hay log.

Trên dashboard: hộp **Thêm tài khoản → Chrome riêng → Đăng nhập tự động**
(gọi `POST /api/login_auto`, chạy nền nên không treo UI).

Đổi thư mục dữ liệu bằng `WHALEGATE_DATA_DIR` khi chạy trong sandbox/container
(`~/.whalegate` có thể chỉ đọc).

## Đăng ký acc mới

Luồng này dùng cho **acc của chính bạn**: whalegate không tự sinh hay giả mạo
`shumei_verification` — nó chỉ đọc giá trị mà chính trang đã dùng.

### Hai nguồn khác nhau, đừng lẫn

Request `POST /api/v0/users/create_email_verification_code` mang hai thứ riêng biệt:

```json
{
  "email": "...", "locale": "vi", "scenario": "register",
  "device_id": "B<base64 ~89 ký tự>",
  "shumei_verification": { "rid": "...", "region": "VN" },
  "turnstile_token": "..."
}
```

| Giá trị | Nguồn thật |
|---|---|
| `device_id` | `SMSdk.getDeviceId()` — chuỗi `'B'+base64`. **Không phải** cookie `smidV2` (cookie là timestamp hex); gửi cookie thì server trả 200 + `biz_code 2` chứ không phải 422, nên rất dễ tưởng là đúng |
| `shumei_verification.rid` / `.region` | **Widget captcha Shumei** trả về sau khi người dùng giải: `showShumeiCaptcha().onSettled` → `{pass, rid}`. Region lấy từ `eR()` của SDK |
| `turnstile_token` | Cloudflare Turnstile widget |

### `deviceprofile/v4` KHÔNG chứa `rid`

```
POST https://fp-it-acc.portal101.cn/deviceprofile/v4
→ {"code":1100,"detail":{"c":0,"deviceId":"NpOy…==","t":0},"requestId":"1102…"}
```

`code: 1100` là **phản hồi bình thường** (cấp deviceId), không phải dấu hiệu bị
phát hiện — Chrome thường cũng nhận đúng như vậy. Response chỉ có `deviceId` và
`requestId`; **không có `rid`**. Đọc `rid` từ đây là lý do bản capture đầu tiên
luôn thất bại. Xem test `deviceprofile_response_has_no_rid`.

### Vì sao headless không đăng ký được

Bấm "Gửi mã" trong Chrome headless (kể cả trang thật, PoW thật, body mã hoá thật)
vẫn trả:

```json
{"code":0,"data":{"biz_code":2,"biz_msg":"RECAPTCHA_VERIFY_FAILED"}}
```

Vì widget `cf-turnstile` có trong DOM nhưng **không tạo được iframe challenge**
trong headless (`document.querySelectorAll('iframe') → 0`), nên không có
`turnstile_token`. Đây là giới hạn của headless, không phải thiếu tham số. Cần
Chrome có màn hình thật.

### Các bước

1. `whalegate register capture <email>` — mở Chrome thật của máy, bạn tự bấm "Gửi mã";
   tool cài hook `XMLHttpRequest.send`/`fetch` **trước** để giữ nguyên văn body JSON
   của request đăng ký (trên đường truyền nó đã bị AES+RSA nên CDP chỉ thấy byte rác),
   đọc `shumei_verification.rid/region` từ đó và `device_id` từ `SMSdk.getDeviceId()`.
2. `whalegate register send-otp <email>` — dùng bản capture < 2 phút.
3. `whalegate register verify <otp> <mật-khẩu>`
4. `whalegate register add [label]`

### `register capture` làm gì

```bash
whalegate register capture you@example.com
```

- Mở **Chrome thật của máy** (profile riêng ở `~/.whalegate/capture-profile`, không
  đụng profile đang chạy) tại `https://chat.deepseek.com/sign_up`.
- Cài hook `XMLHttpRequest.open/send` + `fetch` **trước** khi bạn bấm gì, để giữ
  nguyên văn body JSON của `create_email_verification_code` trong `sessionStorage`.
  Không đọc qua CDP body vì trên đường truyền body đã bị AES-128-CBC + RSA-OAEP.
- Poll tới khi thấy `shumei_verification.rid` và `SMSdk.getDeviceId()` cùng có,
  lưu `{device_id, rid, region, captured_at_ms, page_url}` vào
  `~/.whalegate/shumei_capture.json`.
- In lệnh `send-otp` đã điền sẵn, rồi đóng Chrome.

Trong lúc chờ, bạn tự nhập email và bấm **"Gửi mã"** trong cửa sổ Chrome đó (phải
giải captcha nếu nó hiện). Tool chờ tối đa 180s.

Sau đó:

```bash
# dùng bản capture vừa lưu (< 2 phút, vì rid gắn với challenge)
whalegate register send-otp you@example.com

# hoặc dán tay nếu muốn
whalegate register send-otp you@example.com '<device_id>' '<rid>' '<region>'
```

### Lấy tay bằng DevTools (dự phòng)

1. Mở Chrome, vào https://chat.deepseek.com/sign_up, nhập email.
2. Mở DevTools → Console **trước** khi bấm, dán đoạn hook sau:

```js
const OS = XMLHttpRequest.prototype.send, OO = XMLHttpRequest.prototype.open;
XMLHttpRequest.prototype.open = function (m, u) { this.__u = u; return OO.apply(this, arguments) };
XMLHttpRequest.prototype.send = function (b) {
  if (String(this.__u || '').includes('/create_email_verification_code')) console.log('BODY', b);
  return OS.apply(this, arguments)
};
```

3. Bấm "Gửi mã", giải captcha nếu hiện.
4. Console in ra body JSON — lấy `shumei_verification.rid`, `shumei_verification.region`
   và `device_id`.
5. Chạy `whalegate register send-otp you@example.com '<device_id>' '<rid>' '<region>'`.

Đừng lấy `rid` từ response `deviceprofile/v4`: response đó không có `rid`.

Lý do phải hook `send` chứ không đọc Network panel: payload của
`create_email_verification_code` bị mã hoá nhị phân (AES-128-CBC + RSA-OAEP) trên
đường truyền, nên DevTools chỉ thấy byte rác. Hook chạy **trong trang**, trước khi
mã hoá, nên thấy JSON gốc.

## Các bước sau khi có OTP

```bash
whalegate register verify <otp> <password>
whalegate register add [label]
```

## Ghi chú

- `rid` hết hạn nhanh (cùng phiên với challenge). `send-otp` từ chối bản capture
  cũ hơn 2 phút; nếu bị từ chối, chạy lại `capture`.
- `register` (bước xác nhận OTP) **không** cần `shumei_verification` — chỉ
  `create_email_verification_code` cần.
- `region` có thể vắng; `send-otp` chấp nhận và gửi chuỗi rỗng, giống web khi
  SDK không trả region. `rid` thì bắt buộc.
- Headless **không** đăng ký được: Turnstile không render iframe nên không có
  `turnstile_token`. Luồng login thì không bị ảnh hưởng (trang `sign_in` không có
  captcha) nên vẫn chạy headless bình thường.
- Phần còn lại (PoW DeepSeekHashV1, AES-128-CBC + RSA-OAEP-SHA256, header
  `x-client-*`, cookie `ds_session_id`) đã được `src/register.rs` xử lý.
