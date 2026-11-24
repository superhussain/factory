// Local Worker project API over HTTP and through the factory binary; never follows redirects.
// Runs when FACTORY_TEST_URL names an owned local Worker, as `node tools/local.mjs --check` sets it.
// Two devices' pushes meet at a barrier proxy, so both send their PUT on the same base.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, test } from "node:test";
import { barrierProxy, factory, makeHome, request, strong, testUrl } from "./local-http.mjs";

const PROJECT = "/api/projects/settings-sample.md";
const SLUG = "settings-sample";

test("the target guard refuses every other origin before any request", (t) => {
  const saved = process.env.FACTORY_TEST_URL;
  t.after(() => {
    if (saved === undefined) delete process.env.FACTORY_TEST_URL;
    else process.env.FACTORY_TEST_URL = saved;
  });
  t.mock.method(globalThis, "fetch");
  for (const url of [
    "https://factory.example.test",
    "http://localhost:8220",
    "http://127.0.0.1:8220/",
    "http://127.0.0.1:8210",
    "http://user@127.0.0.1:8220",
    "http://127.0.0.1:8220?x=1",
    "http://127.0.0.1:8220#x",
    "http://127.0.0.1:8220/api",
  ]) {
    process.env.FACTORY_TEST_URL = url;
    assert.throws(testUrl, /owned loopback origin/, url);
  }
  assert.equal(globalThis.fetch.mock.callCount(), 0);
});

const url = testUrl();

describe("project API", () => {
  // Each test skips on its own, so a run without the URL counts them as skipped.
  const skip = url ? false : "set FACTORY_TEST_URL";
  const base = readFileSync(new URL("../fixtures/project-settings.md", import.meta.url));
  const more = (text) => Buffer.concat([base, Buffer.from(text)]);
  const get = () => request(url, PROJECT);
  const put = (body, tag) =>
    request(url, PROJECT, {
      method: "PUT",
      body,
      headers: { "Content-Type": "text/markdown; charset=utf-8", "If-Match": tag },
    });
  // The project files the binary keeps for a home: the document, its sync state, and the saved R2 copy.
  const files = (home) =>
    [`${SLUG}.md`, `${SLUG}.sync.json`, `${SLUG}.md.remote`].map((n) => join(home, "projects", n));
  let tag, dir;

  beforeEach(async () => {
    const current = await get();
    assert.ok([200, 404].includes(current.status));
    const condition =
      current.status === 200
        ? { "If-Match": strong(current.headers.get("ETag")) }
        : { "If-None-Match": "*" };
    const written = await request(url, PROJECT, {
      method: "PUT",
      body: base,
      headers: { "Content-Type": "text/markdown", ...condition },
    });
    assert.equal(written.status, 200);
    tag = strong(written.headers.get("ETag"));
    dir = mkdtempSync(join(tmpdir(), "project-api-"));
  });

  afterEach(() => rmSync(dir, { recursive: true, force: true }));

  async function home(name, origin = url) {
    const h = makeHome(dir, name, origin, { [SLUG]: join(dir, name) });
    assert.equal((await factory(h, "pull", SLUG)).code, 0);
    return h;
  }

  const push = async (h) => (await factory(h, "push", SLUG)).code;

  test("browser-like and device conflicts in both orders", { skip }, async () => {
    const h = await home("device");
    const [doc] = files(h);
    writeFileSync(doc, more("Device"));
    assert.equal((await put(more("Browser"), tag)).status, 200);
    assert.equal(await push(h), 6);
    assert.deepEqual(readFileSync(doc), more("Device"));
    writeFileSync(doc, base);
    assert.equal((await factory(h, "pull", SLUG)).code, 0);
    const browserTag = strong((await get()).headers.get("ETag"));
    writeFileSync(doc, more("Device second"));
    assert.equal(await push(h), 0);
    assert.equal((await put(more("Browser second"), browserTag)).status, 412);
    assert.deepEqual((await get()).body, more("Device second"));
  });

  test("real atomic race and readback", { skip }, async () => {
    const bodies = [more("A"), more("B")];
    const results = await Promise.all(bodies.map((body) => put(body, tag)));
    assert.deepEqual(results.map((r) => r.status).sort(), [200, 412]);
    const winner = results.findIndex((r) => r.status === 200);
    const got = await get();
    assert.equal(got.status, 200);
    assert.deepEqual(got.body, bodies[winner]);
    assert.equal(strong(got.headers.get("ETag")), strong(results[winner].headers.get("ETag")));
  });

  test("two-device race, conflict, merge and where", { skip }, async () => {
    const proxy = await barrierProxy(url, `PUT ${PROJECT}`);
    try {
      const homes = [await home("A", proxy.origin), await home("B", proxy.origin)];
      const drafts = [more("Device A"), more("Device B")];
      homes.forEach((h, i) => writeFileSync(files(h)[0], drafts[i]));
      const before = proxy.calls.length;
      const results = await Promise.all(homes.map(push));
      assert.deepEqual([...results].sort(), [0, 6]);
      assert.deepEqual(proxy.calls.slice(before), [`PUT ${PROJECT}`, `PUT ${PROJECT}`]);
      const winner = results.indexOf(0);
      const loser = results.indexOf(6);
      assert.deepEqual((await get()).body, drafts[winner]);
      const [doc, state, remote] = files(homes[loser]);
      assert.deepEqual(readFileSync(doc), drafts[loser]);
      assert.equal((await factory(homes[loser], "pull", SLUG)).code, 6);
      assert.deepEqual(readFileSync(remote), drafts[winner]);
      assert.deepEqual(readFileSync(doc), drafts[loser]);
      writeFileSync(doc, more("Merged explicitly"));
      unlinkSync(remote);
      assert.equal(await push(homes[loser]), 0);
      const where = await factory(homes[loser], "where", SLUG);
      assert.equal(where.code, 0, where.stderr);
      const got = JSON.parse(where.stdout);
      assert.deepEqual(readFileSync(got.project_file), (await get()).body);
      assert.equal(
        JSON.parse(readFileSync(state, "utf8")).etag,
        strong((await get()).headers.get("ETag")),
      );
    } finally {
      await proxy.close();
    }
  });

  test("validation and baseless stale writes leave bytes", { skip }, async () => {
    for (const [headers, body, status] of [
      [{}, base, 428],
      [{ "If-Match": '"a", "b"' }, base, 400],
      [{ "If-Match": tag, "Content-Type": "application/json" }, base, 415],
      [
        { "If-Match": tag, "Content-Type": "text/markdown" },
        Buffer.from(base.toString().replace("base: main", "path: /device\nbase: main")),
        400,
      ],
      [{ "If-Match": tag, "Content-Type": "text/markdown" }, Buffer.from([0xff]), 400],
      [{ "If-Match": tag, "Content-Type": "text/markdown" }, more("x".repeat(1 << 20)), 413],
      [{ "If-Match": '"stale"', "Content-Type": "text/markdown" }, base, 412],
      [{ "If-None-Match": "*", "Content-Type": "text/markdown" }, base, 412],
    ]) {
      const r = await request(url, PROJECT, { method: "PUT", body, headers });
      assert.equal(r.status, status, JSON.stringify(headers));
      const current = await get();
      assert.deepEqual(current.body, base);
      assert.equal(strong(current.headers.get("ETag")), tag);
    }
  });
});
