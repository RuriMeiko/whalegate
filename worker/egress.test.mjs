import test from "node:test";
import assert from "node:assert/strict";
import worker, { allowedHost, stripDomain, tunnelTarget } from "./egress.js";

const env = { EGRESS_KEY: "k" };
const req = (path, init = {}) => new Request("https://eg.example" + path, init);

function mockFetch(handler) {
  const calls = [];
  globalThis.fetch = async (url, init) => { calls.push({ url: String(url), init }); return handler(String(url), init); };
  return calls;
}

test("health is public, everything else needs the key", async () => {
  assert.equal((await worker.fetch(req("/__health"), env)).status, 200);
  assert.equal((await worker.fetch(req("/api/v0/users/current"), env)).status, 401);
  assert.equal((await worker.fetch(req("/api/v0/users/current", { headers: { "x-wg-key": "bad" } }), env)).status, 401);
  assert.equal((await worker.fetch(req("/api/v0/users/current", { headers: { "x-wg-key": "k" } }), {})).status, 401, "không có secret thì từ chối hết");
});

test("only deepseek hosts are reachable", () => {
  assert.ok(allowedHost("chat.deepseek.com") && allowedHost("hif-leim.deepseek.com") && allowedHost("deepseek.com"));
  for (const h of ["evil.com", "deepseek.com.evil.com", "xdeepseek.com", "a b.deepseek.com", "", "127.0.0.1"]) assert.equal(allowedHost(h), false, h);
});

test("forwards path, query, body and strips identifying headers", async () => {
  const calls = mockFetch(() => new Response("{}", { status: 200 }));
  const r = await worker.fetch(req("/api/v0/chat/completion?a=1", {
    method: "POST", body: '{"x":1}',
    headers: { "x-wg-key": "k", authorization: "Bearer T", "x-forwarded-for": "1.2.3.4", "cf-connecting-ip": "1.2.3.4", "content-type": "application/json", "x-device-id": "d" },
  }), env);
  assert.equal(r.status, 200);
  assert.equal(calls[0].url, "https://chat.deepseek.com/api/v0/chat/completion?a=1");
  const h = calls[0].init.headers;
  assert.equal(h.get("authorization"), "Bearer T");
  assert.equal(h.get("x-device-id"), "d");
  for (const k of ["x-wg-key", "x-forwarded-for", "cf-connecting-ip"]) assert.equal(h.get(k), null, k);
});

test("x-wg-host picks another deepseek host, rejects others", async () => {
  const calls = mockFetch(() => new Response("x"));
  await worker.fetch(req("/query", { headers: { "x-wg-key": "k", "x-wg-host": "hif-leim.deepseek.com" } }), env);
  assert.equal(calls[0].url, "https://hif-leim.deepseek.com/query");
  const bad = await worker.fetch(req("/query", { headers: { "x-wg-key": "k", "x-wg-host": "evil.com" } }), env);
  assert.equal(bad.status, 403);
});

test("Set-Cookie loses its Domain attribute", async () => {
  assert.equal(stripDomain("ds=1; Path=/; Domain=.deepseek.com; Secure; HttpOnly"), "ds=1; Path=/; Secure; HttpOnly");
  assert.equal(stripDomain("a=b"), "a=b");
  mockFetch(() => { const h = new Headers(); h.append("set-cookie", "s=1; Domain=chat.deepseek.com; Path=/"); h.append("set-cookie", "t=2; Domain=.deepseek.com"); return new Response("ok", { headers: h }); });
  const r = await worker.fetch(req("/favicon.ico", { headers: { "x-wg-key": "k" } }), env);
  assert.deepEqual(r.headers.getSetCookie(), ["s=1; Path=/", "t=2"]);
});

test("streams the body through and keeps status", async () => {
  mockFetch(() => new Response("event: ready\n\n", { status: 202, headers: { "content-type": "text/event-stream" } }));
  const r = await worker.fetch(req("/api/v0/chat/completion", { method: "POST", body: "{}", headers: { "x-wg-key": "k" } }), env);
  assert.equal(r.status, 202);
  assert.equal(r.headers.get("content-type"), "text/event-stream");
  assert.equal(await r.text(), "event: ready\n\n");
});

test("tunnel needs the key and a websocket upgrade, and validates the target", async () => {
  assert.equal((await worker.fetch(req("/__tunnel?host=chat.deepseek.com&port=443"), env)).status, 401);
  const noWs = await worker.fetch(req("/__tunnel?host=chat.deepseek.com&port=443", { headers: { "x-wg-key": "k" } }), env);
  assert.equal(noWs.status, 426);
  const u = (q) => new URL("https://eg.example/__tunnel?" + q);
  assert.deepEqual(tunnelTarget(u("host=chat.deepseek.com&port=443")), { host: "chat.deepseek.com", port: 443 });
  assert.deepEqual(tunnelTarget(u("host=example.com")), { host: "example.com", port: 443 }, "mặc định 443");
  for (const q of ["host=&port=443", "host=a b.com&port=443", "host=x.com&port=25", "host=x.com&port=0", "host=x.com&port=70000", "host=../x&port=443"]) assert.equal(tunnelTarget(u(q)), null, q);
});
