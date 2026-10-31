// whalegate Worker Proxy — một địa chỉ công khai đứng trước nhiều instance whalegate (nhiều máy/VPS):
// kiểm tra token của client, chia request xoay vòng, upstream lỗi/hết acc thì chuyển sang upstream khác.
//
// Secret (đặt bằng `wrangler secret put` hoặc worker/deploy.sh):
//   UPSTREAMS      : JSON array các instance whalegate, ví dụ ["https://wg1.example","https://wg2.example"]
//   WORKER_TOKEN   : bearer client phải gửi tới Worker (để không ai dùng chùa)
//   UPSTREAM_TOKEN : (tuỳ chọn) DS_API_KEY của các instance whalegate — Worker tự gắn khi gọi upstream,
//                    token của client KHÔNG bao giờ được chuyển tiếp

const JSON_HEADERS = { "content-type": "application/json" };

// đường đi whalegate phục vụ cho client (admin /api/* chỉ mở cho localhost, không cho qua)
const ROUTES = [
  ["POST", "/v1/chat/completions"],
  ["POST", "/v1/search/messages"],
  ["GET", "/v1/models"],
];

// upstream trả các mã này thì thử instance khác: lỗi máy chủ, quá tải, hoặc instance đó hết acc dùng được
const RETRY_STATUS = new Set([429, 500, 502, 503, 504]);

// header chuyển về client: đủ cho JSON và SSE, không lộ gì về upstream
const PASS_HEADERS = ["content-type", "cache-control", "retry-after"];

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (url.pathname === "/health") {
      return json({ ok: true, t: Date.now() });
    }

    if (env.WORKER_TOKEN) {
      const auth = request.headers.get("authorization") || "";
      if (!(await safeEqual(auth, "Bearer " + env.WORKER_TOKEN))) {
        return json({ error: "unauthorized" }, 401);
      }
    }

    if (!ROUTES.some(([m, p]) => m === request.method && p === url.pathname)) {
      return json({ error: "not found", path: url.pathname }, 404);
    }

    let upstreams = [];
    try { upstreams = JSON.parse(env.UPSTREAMS || "[]"); } catch (e) {}
    upstreams = upstreams.filter((u) => typeof u === "string" && /^https?:\/\//.test(u)).map((u) => u.replace(/\/+$/, ""));
    if (!upstreams.length) {
      return json({ error: "no upstreams configured (set UPSTREAMS)" }, 500);
    }

    // round-robin qua KV nếu có binding, không thì ngẫu nhiên — với vài upstream là đủ đều
    let start;
    if (env.ROBIN_KV) {
      const cur = parseInt((await env.ROBIN_KV.get("rr")) || "0", 10) || 0;
      start = cur % upstreams.length;
      await env.ROBIN_KV.put("rr", String(cur + 1));
    } else {
      start = Math.floor(Math.random() * upstreams.length);
    }

    // body chỉ đọc được một lần: đọc trước vòng thử lại để upstream thứ hai, thứ ba vẫn có body
    const body = request.method === "POST" ? await request.arrayBuffer() : undefined;

    const headers = new Headers();
    for (const h of ["content-type", "accept", "user-agent"]) {
      const v = request.headers.get(h);
      if (v) headers.set(h, v);
    }
    if (env.UPSTREAM_TOKEN) headers.set("authorization", "Bearer " + env.UPSTREAM_TOKEN);

    let last = null;
    for (let attempt = 0; attempt < Math.min(3, upstreams.length); attempt++) {
      const i = (start + attempt) % upstreams.length;
      try {
        const resp = await fetch(upstreams[i] + url.pathname, { method: request.method, headers, body });
        if (RETRY_STATUS.has(resp.status) && attempt + 1 < Math.min(3, upstreams.length)) {
          last = resp;
          continue;
        }
        return passThrough(resp, i);
      } catch (e) {
        continue;
      }
    }
    // hết lượt: trả nguyên lỗi của upstream cuối (ví dụ 503 accounts_logged_out) thay vì một câu chung chung
    if (last) return passThrough(last, -1);
    return json({ error: "all upstreams failed" }, 502);
  },
};

function passThrough(resp, i) {
  const out = new Headers();
  for (const h of PASS_HEADERS) {
    const v = resp.headers.get(h);
    if (v) out.set(h, v);
  }
  if (!out.has("content-type")) out.set("content-type", "application/json");
  // chỉ số upstream (không phải URL) để chẩn đoán phía client
  if (i >= 0) out.set("x-whalegate-upstream", String(i));
  return new Response(resp.body, { status: resp.status, headers: out });
}

// so sánh không lộ thời gian: băm cả hai rồi so từng byte
async function safeEqual(a, b) {
  const enc = new TextEncoder();
  const [ha, hb] = await Promise.all([crypto.subtle.digest("SHA-256", enc.encode(a)), crypto.subtle.digest("SHA-256", enc.encode(b))]);
  const x = new Uint8Array(ha), y = new Uint8Array(hb);
  let d = 0;
  for (let k = 0; k < x.length; k++) d |= x[k] ^ y[k];
  return d === 0;
}

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), { status, headers: JSON_HEADERS });
}
