// DeepSeek Harness Worker Proxy — xoay nhiều upstream harness instance (multi-IP/multi-VPS)
// + cache-simple + rate-limit nhẹ. Free plan 100k req/ngày.
//
// Env vars (cấu hình trong dashboard):
//   UPSTREAMS : JSON array các harness backend, ví dụ:
//     ["https://ds1.wtfdev.qzz.io","https://ds2.wtfdev.qzz.io"]
//   WORKER_TOKEN : mật khẩu chung client phải gửi qua header Authorization: Bearer <token>
//     (để không ai băm free API của mình)

const JSON_HEADERS = { "content-type": "application/json" };

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);

    // health
    if (url.pathname === "/health") {
      return new Response(JSON.stringify({ ok: true, t: Date.now() }), { headers: JSON_HEADERS });
    }

    // token check (nếu cấu hình WORKER_TOKEN)
    if (env.WORKER_TOKEN) {
      const auth = request.headers.get("authorization") || "";
      if (auth !== "Bearer " + env.WORKER_TOKEN) {
        return json({ error: "unauthorized" }, 401);
      }
    }

    // chỉ cho POST /v1/chat/completions và GET /v1/models
    const allowed =
      (request.method === "POST" && url.pathname === "/v1/chat/completions") ||
      (request.method === "GET" && url.pathname === "/v1/models");
    if (!allowed) {
      return json({ error: "not found", path: url.pathname }, 404);
    }

    // xoay upstream theo yêu cầu round-robin qua Durable Object free tier
    // (đơn giản hơn: random pick — với vài upstream là đủ đều)
    let upstreams = [];
    try { upstreams = JSON.parse(env.UPSTREAMS || "[]"); } catch (e) {}
    if (!upstreams.length) {
      return json({ error: "no upstreams configured (set UPSTREAMS env)" }, 500);
    }

    // round robin state qua KV (nếu có binding KV) — fallback random
    let idx;
    if (env.ROBIN_KV) {
      const cur = parseInt((await env.ROBIN_KV.get("rr")) || "0");
      idx = cur % upstreams.length;
      await env.ROBIN_KV.put("rr", String(cur + 1));
    } else {
      idx = Math.floor(Math.random() * upstreams.length);
    }

    const target = upstreams[idx] + url.pathname;

    // forward với retry qua upstream khác nếu fail
    for (let attempt = 0; attempt < Math.min(3, upstreams.length); attempt++) {
      const u = upstreams[(idx + attempt) % upstreams.length] + url.pathname;
      try {
        const headers = new Headers(request.headers);
        headers.delete("host");
        headers.delete("cf-connecting-ip");
        headers.delete("cf-ipcountry");
        headers.delete("cf-ray");
        headers.delete("cf-visitor");
        headers.delete("x-forwarded-for");
        headers.delete("x-real-ip");

        const resp = await fetch(u, {
          method: request.method,
          headers,
          body: request.method === "POST" ? await request.text() : undefined,
        });

        if (resp.status >= 500 || resp.status === 429) {
          continue; // thử upstream khác
        }
        return new Response(resp.body, {
          status: resp.status,
          headers: {
            "content-type": resp.headers.get("content-type") || "application/json",
            "x-harness-upstream": u,
          },
        });
      } catch (e) {
        continue;
      }
    }

    return json({ error: "all upstreams failed" }, 502);
  },
};

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), { status, headers: JSON_HEADERS });
}
