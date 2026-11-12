// Installable dashboard routes: with a valid assertion, the service worker, manifest and
// icons answer with their exact types and the shared no-store, nosniff headers.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket, serverEnv } from "../support/worker.mjs";
import { generateKey, jwksFixture, sign } from "../support/jwks.mjs";
import { serviceWorker } from "../../worker/pwa.js";

const { default: worker } = await loadProduction();
const key = await generateKey("current");

async function signedIn(t, title = "Example Factory") {
  const f = jwksFixture(t, [key]);
  const env = {
    ...serverEnv,
    FACTORY_TITLE: title,
    ACCESS_ISSUER: f.config.issuer,
    BUCKET: memoryBucket(),
  };
  const headers = { "Cf-Access-Jwt-Assertion": await sign(key, f.claims()) };
  return (path, method = "GET") =>
    worker.fetch(new Request(serverEnv.FACTORY_ORIGIN + path, { method, headers }), env);
}

function assertHeaders(response, type) {
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("Content-Type"), type);
  assert.equal(response.headers.get("Cache-Control"), "no-store");
  assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
}

test("service worker script is the serialized self-invoking worker", async (t) => {
  const get = await signedIn(t);
  const response = await get("/sw.js");
  assertHeaders(response, "text/javascript; charset=utf-8");
  const body = await response.text();
  assert.match(body, /^\(function serviceWorker\(\) \{/);
  assert.match(body, /\}\)\(\);$/);
  assert.match(body, /"factory-v2"/);
  assert.doesNotMatch(body, /synthetic/);
  assert.equal((await get("/sw.js", "POST")).status, 404);
});

test("manifest carries the configured title and the three icons", async (t) => {
  const get = await signedIn(t, "Example Factory");
  const response = await get("/assets/manifest.webmanifest");
  assertHeaders(response, "application/manifest+json; charset=utf-8");
  const icon = (size, purpose) => ({
    src: `/assets/icon-${size}.png`,
    sizes: `${size}x${size}`,
    type: "image/png",
    purpose,
  });
  assert.deepEqual(await response.json(), {
    id: "/",
    name: "Example Factory",
    short_name: "Example Factory",
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#f9fafb",
    theme_color: "#f9fafb",
    icons: [icon(192, "any"), icon(512, "any"), icon(512, "maskable")],
  });
});

test("icons are PNGs with the declared IHDR size", async (t) => {
  const get = await signedIn(t);
  for (const [path, size] of [
    ["/assets/icon-192.png", 192],
    ["/assets/icon-512.png", 512],
    ["/assets/apple-touch-icon.png", 180],
  ]) {
    const response = await get(path);
    assertHeaders(response, "image/png");
    const bytes = Buffer.from(await response.arrayBuffer());
    assert.deepEqual([...bytes.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    assert.equal(bytes.subarray(12, 16).toString("latin1"), "IHDR");
    assert.equal(bytes.readUInt32BE(16), size);
    assert.equal(bytes.readUInt32BE(20), size);
    assert.equal((await get(path, "POST")).status, 404);
  }
});

test("dashboard and board.json carry the same deployed version", async (t) => {
  const f = jwksFixture(t, [key]);
  const bucket = memoryBucket();
  await bucket.put("board.json", '{"jobs":[]}');
  const env = { ...serverEnv, ACCESS_ISSUER: f.config.issuer, BUCKET: bucket };
  const headers = { "Cf-Access-Jwt-Assertion": await sign(key, f.claims()) };
  const get = (path) =>
    worker.fetch(new Request(serverEnv.FACTORY_ORIGIN + path, { headers }), env);
  const page = await get("/");
  const meta = (await page.text()).match(/<meta name="factory-version" content="([0-9a-f]{8})">/);
  assert.ok(meta);
  const board = await get("/board.json");
  assert.equal(board.status, 200);
  assert.equal(board.headers.get("X-Factory-Version"), meta[1]);
});

test("service worker leaves walkthrough.html to the network", async (t) => {
  const saved = Object.fromEntries(
    ["self", "location", "caches", "fetch"].map((k) => [k, globalThis[k]]),
  );
  t.after(() => Object.assign(globalThis, saved));
  const handlers = {};
  globalThis.self = {
    addEventListener: (type, fn) => (handlers[type] = fn),
    skipWaiting() {},
    clients: { claim: async () => {} },
  };
  globalThis.location = { origin: serverEnv.FACTORY_ORIGIN };
  globalThis.caches = {
    open: async () => ({ match: async () => {}, put: async () => {}, delete: async () => {} }),
    delete: async () => {},
    keys: async () => [],
  };
  globalThis.fetch = async () => new Response("x");
  serviceWorker();
  const calls = [];
  for (const [path, mode] of [
    ["/jobs/sample/001/walkthrough.html", "navigate"],
    ["/jobs/sample/001/round-1/review-ui.walkthrough.html", "navigate"],
    ["/jobs/sample/001/spec.md", "cors"],
  ]) {
    const url = serverEnv.FACTORY_ORIGIN + path;
    handlers.fetch({
      request: { method: "GET", url, mode },
      respondWith: (p) => {
        calls.push(url);
        p.catch(() => {});
      },
      waitUntil() {},
    });
  }
  assert.deepEqual(calls, [serverEnv.FACTORY_ORIGIN + "/jobs/sample/001/spec.md"]);
});
