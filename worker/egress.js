// whalegate Egress Worker — cổng ra: whalegate gọi DeepSeek QUA Worker này nên DeepSeek thấy IP của Cloudflare thay vì IP nhà.
// Khác ds-proxy.js (đứng TRƯỚC whalegate): file này đứng SAU whalegate, giữa whalegate và chat.deepseek.com.
//
// Secret (đặt bằng `wrangler secret put EGRESS_KEY` hoặc worker/deploy-egress.sh):
//   EGRESS_KEY : khoá whalegate gửi trong header x-wg-key — không có khoá thì Worker từ chối (không thành proxy mở)
//
// Quy tắc:
//   - mặc định chuyển tới https://chat.deepseek.com + đúng path/query của request
//   - header x-wg-host chọn host khác nhưng CHỈ nhận deepseek.com và *.deepseek.com (vd hif-leim.deepseek.com)
//   - header x-wg-*, cf-*, x-forwarded-*, x-real-ip bị bỏ trước khi gọi DeepSeek
//   - Set-Cookie của DeepSeek được bỏ thuộc tính Domain để cookie jar của whalegate (host = Worker) nhận được
//   - GET /__tunnel?host=H&port=P (WebSocket): đường hầm TCP thô tới H:P bằng cloudflare:sockets — TLS đi nguyên vẹn đầu-cuối
//     giữa Chrome/whalegate và DeepSeek, Worker chỉ thấy bytes mã hoá. whalegate chạy proxy CONNECT cục bộ nối vào đây.
//   - GET /__health (không khoá) và GET /__ip (cần khoá) để kiểm tra Worker và xem IP đầu ra thật

const DEFAULT_HOST = "chat.deepseek.com";
const DROP_REQ = /^(x-wg-|cf-|x-forwarded-|x-real-ip$|forwarded$|host$|connection$|content-length$|accept-encoding$)/i;
const DROP_RESP = /^(content-encoding|content-length|transfer-encoding|connection)$/i;

export function allowedHost(h) {
  return typeof h === "string" && /^[a-z0-9.-]+$/i.test(h) && (h === "deepseek.com" || h.endsWith(".deepseek.com"));
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/__health") return json({ ok: true, t: Date.now() });

    if (!env.EGRESS_KEY || !(await safeEqual(request.headers.get("x-wg-key") || "", env.EGRESS_KEY))) {
      return json({ error: "unauthorized" }, 401);
    }

    if (url.pathname === "/__tunnel") return tunnel(request, url);

    if (url.pathname === "/__ip") {
      // IP mà DeepSeek sẽ thấy: hỏi một dịch vụ bên ngoài từ chính Worker
      try {
        const r = await fetch("https://api.ipify.org?format=json");
        return json({ egress: await r.json(), colo: request.cf && request.cf.colo });
      } catch (e) {
        return json({ error: String(e) }, 502);
      }
    }

    const host = request.headers.get("x-wg-host") || DEFAULT_HOST;
    if (!allowedHost(host)) return json({ error: "host not allowed", host }, 403);

    const headers = new Headers();
    for (const [k, v] of request.headers) if (!DROP_REQ.test(k)) headers.set(k, v);

    const init = { method: request.method, headers, redirect: "manual" };
    if (request.method !== "GET" && request.method !== "HEAD") init.body = request.body;

    let up;
    try {
      up = await fetch(`https://${host}${url.pathname}${url.search}`, init);
    } catch (e) {
      return json({ error: "upstream fetch failed", detail: String(e) }, 502);
    }

    const out = new Headers();
    for (const [k, v] of up.headers) {
      if (DROP_RESP.test(k) || k.toLowerCase() === "set-cookie") continue;
      out.set(k, v);
    }
    const cookies = typeof up.headers.getSetCookie === "function" ? up.headers.getSetCookie() : [];
    for (const c of cookies) out.append("set-cookie", stripDomain(c));
    return new Response(up.body, { status: up.status, statusText: up.statusText, headers: out });
  },
};

// cổng gửi thư không mở (tránh bị dùng làm relay spam); còn lại tuỳ trình duyệt
const BLOCKED_PORTS = new Set([25, 465, 587, 2525]);
export function tunnelTarget(url) {
  const host = url.searchParams.get("host") || "";
  const port = parseInt(url.searchParams.get("port") || "443", 10);
  if (!/^[a-z0-9.\-]+$/i.test(host) && !/^[0-9a-f:]+$/i.test(host)) return null;
  if (!(port >= 1 && port <= 65535) || BLOCKED_PORTS.has(port)) return null;
  return { host, port };
}

async function tunnel(request, url) {
  if ((request.headers.get("upgrade") || "").toLowerCase() !== "websocket") return json({ error: "websocket required" }, 426);
  const t = tunnelTarget(url);
  if (!t) return json({ error: "bad target" }, 400);
  const { connect } = await import("cloudflare:sockets");
  let sock;
  try { sock = connect({ hostname: t.host, port: t.port }); } catch (e) { return json({ error: "connect failed", detail: String(e) }, 502); }
  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  server.accept();
  const writer = sock.writable.getWriter();
  const end = () => { try { sock.close(); } catch (e) {} };
  server.addEventListener("message", (e) => {
    const chunk = typeof e.data === "string" ? new TextEncoder().encode(e.data) : new Uint8Array(e.data);
    writer.write(chunk).catch(() => { try { server.close(1011, "write"); } catch (x) {} end(); });
  });
  server.addEventListener("close", () => { writer.close().catch(() => {}); end(); });
  server.addEventListener("error", end);
  sock.readable.pipeTo(new WritableStream({
    write(chunk) { server.send(chunk); },
    close() { try { server.close(1000, "eof"); } catch (e) {} },
    abort() { try { server.close(1011, "upstream"); } catch (e) {} },
  })).catch(() => { try { server.close(1011, "upstream"); } catch (e) {} });
  return new Response(null, { status: 101, webSocket: client });
}

export function stripDomain(cookie) {
  return cookie.split(";").filter((p, i) => i === 0 || !/^\s*domain\s*=/i.test(p)).join(";");
}

async function safeEqual(a, b) {
  const enc = new TextEncoder();
  const [ha, hb] = await Promise.all([crypto.subtle.digest("SHA-256", enc.encode(a)), crypto.subtle.digest("SHA-256", enc.encode(b))]);
  const x = new Uint8Array(ha), y = new Uint8Array(hb);
  let d = 0;
  for (let k = 0; k < x.length; k++) d |= x[k] ^ y[k];
  return d === 0;
}

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), { status, headers: { "content-type": "application/json" } });
}
