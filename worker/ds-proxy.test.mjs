// Test Worker không cần Cloudflare: `node --test worker/`
// fetch toàn cục được thay bằng upstream giả ghi lại mọi request nó nhận.
import { test } from "node:test";
import assert from "node:assert/strict";
import worker from "./ds-proxy.js";

function fakeUpstreams(plan) {
  const calls = [];
  globalThis.fetch = async (url, init) => {
    const body = init.body === undefined ? undefined : new TextDecoder().decode(init.body);
    calls.push({ url, auth: init.headers.get("authorization"), body });
    const host = new URL(url).host;
    const r = plan[host];
    if (r === "down") throw new TypeError("connect failed");
    return new Response(r.body ?? "{}", { status: r.status, headers: { "content-type": r.type ?? "application/json", "x-secret": "upstream-only" } });
  };
  return calls;
}

const ENV = { UPSTREAMS: '["https://a.test","https://b.test/"]', WORKER_TOKEN: "client-secret", UPSTREAM_TOKEN: "ds-api-key" };
const req = (path, init = {}) => new Request("https://proxy.test" + path, {
  method: "POST", headers: { authorization: "Bearer client-secret", "content-type": "application/json" }, body: '{"model":"m"}', ...init,
});

test("sai hoặc thiếu token client → 401, không gọi upstream", async () => {
  const calls = fakeUpstreams({});
  for (const auth of [null, "Bearer wrong", "client-secret"]) {
    const h = auth ? { authorization: auth } : {};
    const r = await worker.fetch(req("/v1/chat/completions", { headers: h }), ENV);
    assert.equal(r.status, 401);
  }
  assert.equal(calls.length, 0);
});

test("token client không lộ sang upstream; upstream nhận UPSTREAM_TOKEN", async () => {
  const calls = fakeUpstreams({ "a.test": { status: 200 }, "b.test": { status: 200 } });
  const r = await worker.fetch(req("/v1/chat/completions"), ENV);
  assert.equal(r.status, 200);
  assert.equal(calls[0].auth, "Bearer ds-api-key");
  assert.equal(calls[0].body, '{"model":"m"}');
  // header nội bộ của upstream và URL upstream không về tới client
  assert.equal(r.headers.get("x-secret"), null);
  assert.match(r.headers.get("x-whalegate-upstream"), /^[01]$/);
});

test("upstream đầu lỗi → upstream sau vẫn nhận đủ body (body chỉ đọc một lần)", async () => {
  for (const first of ["down", { status: 503 }]) {
    const calls = fakeUpstreams({ "a.test": first, "b.test": { status: 200, body: '{"ok":1}' } });
    // lặp vài lần để chắc chắn có lượt bắt đầu từ a.test (chọn ngẫu nhiên)
    for (let k = 0; k < 8; k++) {
      const r = await worker.fetch(req("/v1/chat/completions"), ENV);
      assert.equal(r.status, 200);
      assert.equal(await r.text(), '{"ok":1}');
    }
    assert.ok(calls.some((c) => c.url.startsWith("https://a.test")));
    for (const c of calls) assert.equal(c.body, '{"model":"m"}');
  }
});

test("mọi upstream hết acc → trả nguyên lỗi của whalegate, không nuốt thành 502", async () => {
  const err = JSON.stringify({ error: { code: "accounts_logged_out" } });
  fakeUpstreams({ "a.test": { status: 503, body: err }, "b.test": { status: 503, body: err } });
  const r = await worker.fetch(req("/v1/chat/completions"), ENV);
  assert.equal(r.status, 503);
  assert.equal(JSON.parse(await r.text()).error.code, "accounts_logged_out");
});

test("SSE đi qua nguyên content-type; đường admin bị chặn; search được cho qua", async () => {
  fakeUpstreams({ "a.test": { status: 200, type: "text/event-stream", body: "data: {}\n\n" }, "b.test": { status: 200, type: "text/event-stream", body: "data: {}\n\n" } });
  const r = await worker.fetch(req("/v1/chat/completions"), ENV);
  assert.equal(r.headers.get("content-type"), "text/event-stream");
  assert.equal((await worker.fetch(req("/api/accounts"), ENV)).status, 404);
  assert.equal((await worker.fetch(req("/v1/search/messages"), ENV)).status, 200);
  assert.equal((await worker.fetch(req("/v1/models", { method: "GET", body: undefined }), ENV)).status, 200);
});
