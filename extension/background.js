// Tiện ích whalegate: bấm biểu tượng trên tab chat.deepseek.com → gom token, device id, TOÀN BỘ cookie
// (kể cả HttpOnly như ds_session_id — chrome.cookies đọc được, JS trên trang thì không) và hồ sơ trình duyệt,
// rồi gửi thẳng vào whalegate. Địa chỉ dashboard được điền lúc tải tiện ích từ dashboard.
const WHALEGATE = "__WG_ORIGIN__";
const DS = "https://chat.deepseek.com/";

chrome.action.onClicked.addListener((tab) => addFromTab(tab));

async function addFromTab(tab) {
  if (!tab.url || !tab.url.startsWith(DS)) {
    return flash(tab.id, "!", "Mở tab chat.deepseek.com đã đăng nhập rồi bấm lại", false);
  }
  try {
    const [{ result: d }] = await chrome.scripting.executeScript({ target: { tabId: tab.id }, func: collect });
    if (!d || !d.token) throw new Error("tab này chưa đăng nhập DeepSeek");
    const all = await chrome.cookies.getAll({ url: DS });
    const cookies = Object.fromEntries(all.map((c) => [c.name, c.value]));
    if (d.device_id) d.browser.device_id = d.device_id;
    const r = await fetch(WHALEGATE + "/api/accounts", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token: d.token, cookies, browser: d.browser, label: "" }),
    });
    const res = await r.json().catch(() => ({}));
    if (!r.ok || res.error) throw new Error(res.error || "whalegate trả lỗi " + r.status);
    const who = res.label || "tài khoản";
    const msg = res.updated ? (res.token_changed ? "Đã thay token mới cho " : "Đã làm mới ") + who : "Đã thêm " + who;
    await flash(tab.id, "✓", msg + " (" + all.length + " cookie)", true);
  } catch (e) {
    const m = String((e && e.message) || e);
    await flash(tab.id, "!", /Failed to fetch|NetworkError/.test(m) ? "Không kết nối được whalegate ở " + WHALEGATE : m, false);
  }
}

// Báo kết quả: badge trên biểu tượng + một dòng nổi trên chính tab DeepSeek trong vài giây.
async function flash(tabId, badge, text, ok) {
  await chrome.action.setBadgeBackgroundColor({ tabId, color: ok ? "#2f5d3a" : "#a8321f" });
  await chrome.action.setBadgeText({ tabId, text: badge });
  await chrome.action.setTitle({ tabId, title: "whalegate: " + text });
  setTimeout(() => chrome.action.setBadgeText({ tabId, text: "" }), 6000);
  try {
    await chrome.scripting.executeScript({ target: { tabId }, func: toast, args: [text, ok] });
  } catch (e) {}
}

function toast(text, ok) {
  const el = document.createElement("div");
  el.textContent = "whalegate · " + text;
  el.style.cssText = "position:fixed;z-index:2147483647;right:16px;bottom:16px;max-width:360px;padding:10px 14px;" +
    "font:13px/1.4 ui-monospace,Menlo,Consolas,monospace;color:#1d1b17;background:#ebe4d3;" +
    "border:1px solid " + (ok ? "#2f5d3a" : "#a8321f") + ";box-shadow:4px 4px 0 #1d1b17";
  document.documentElement.appendChild(el);
  setTimeout(() => el.remove(), 5000);
}

// Chạy trong tab DeepSeek. Cùng bộ trường với bookmarklet trên dashboard (COLLECT_JS trong ui.html).
function collect() {
  let raw = null;
  try { raw = localStorage.getItem("userToken"); } catch (e) {}
  let tok = null;
  try { tok = JSON.parse(raw || "null"); } catch (e) { tok = raw; }
  const token = tok && typeof tok === "object" ? tok.value : tok;
  if (!token) return null;
  let gpu = [null, null];
  try {
    const gl = document.createElement("canvas").getContext("webgl");
    const dbg = gl && gl.getExtension("WEBGL_debug_renderer_info");
    if (dbg) gpu = [gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL), gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)];
  } catch (e) {}
  let tz = null;
  try { tz = Intl.DateTimeFormat().resolvedOptions().timeZone; } catch (e) {}
  let dev = null;
  try { dev = localStorage.getItem("deepseek-device-id:chat"); } catch (e) {}
  const nav = navigator;
  return {
    token,
    device_id: dev,
    browser: {
      user_agent: nav.userAgent,
      cores: nav.hardwareConcurrency || null,
      memory_gb: nav.deviceMemory || null,
      screen: [screen.width, screen.height],
      avail: [screen.availWidth, screen.availHeight],
      dpr: window.devicePixelRatio || 1,
      tz,
      tz_offset: -new Date().getTimezoneOffset() * 60,
      langs: (nav.languages && nav.languages.length ? nav.languages : [nav.language || "vi"]).slice(),
      gpu_vendor: gpu[0],
      gpu_renderer: gpu[1],
    },
  };
}
