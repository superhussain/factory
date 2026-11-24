// Actual local R2 document conditions over HTTP; never follows redirects. Runs when
// FACTORY_TEST_URL names an owned local Worker, as `node tools/local.mjs --check` sets it.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { beforeEach, describe, test } from "node:test";
import { request, strong, testUrl } from "./local-http.mjs";

const FIXTURES = new URL("../fixtures/", import.meta.url);
const PROJECTS = ["documents-sample", "documents-second"];

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

describe("project documents API", () => {
  // Each test skips on its own, so a run without the URL counts them as skipped.
  const skip = url ? false : "set FACTORY_TEST_URL";
  const get = (path) => request(url, path);
  const put = (path, body, condition) =>
    request(url, path, {
      method: "PUT",
      body,
      headers: { "Content-Type": "text/markdown; charset=utf-8", ...condition },
    });
  const path = (slug = "documents-sample", name = "notes.md") =>
    `/api/project-documents/${slug}/${name}`;
  const config = readFileSync(new URL("project-documents.md", FIXTURES));
  const notes = readFileSync(new URL("project-document-notes.md", FIXTURES));

  async function reset(p, body) {
    const current = await get(p);
    assert.ok([200, 404].includes(current.status));
    const condition =
      current.status === 200
        ? { "If-Match": strong(current.headers.get("ETag")) }
        : { "If-None-Match": "*" };
    const result = await put(p, body, condition);
    assert.equal(result.status, 200);
    assert.deepEqual((await get(p)).body, Buffer.from(body));
    return strong(result.headers.get("ETag"));
  }

  // Every write starts in the same tick, as the Python barrier released them together.
  const race = (writes) => Promise.all(writes.map((args) => put(...args)));

  async function assertWinner(p, bodies, results) {
    assert.deepEqual(results.map((r) => r.status).sort(), [200, 412]);
    const winner = results.findIndex((r) => r.status === 200);
    const got = await get(p);
    assert.equal(got.status, 200);
    assert.deepEqual(got.body, Buffer.from(bodies[winner]));
    assert.equal(strong(got.headers.get("ETag")), strong(results[winner].headers.get("ETag")));
  }

  beforeEach(async () => {
    for (const slug of PROJECTS) {
      await reset(`/api/projects/${slug}.md`, config);
      await reset(path(slug, "notes.md"), notes);
    }
  });

  test("create race", { skip }, async () => {
    const p = path(undefined, "decisions.md");
    assert.equal((await get(p)).status, 404, "complete runs require fresh job-local persistence");
    const bodies = ["Created A\n", "Created B\n"];
    await assertWinner(p, bodies, await race(bodies.map((b) => [p, b, { "If-None-Match": "*" }])));
  });

  test("independent names and projects", { skip }, async () => {
    const paths = [path(), path(undefined, "runbook.md"), path("documents-second")];
    const tags = [];
    for (const [i, p] of paths.entries()) tags.push(await reset(p, `Distinct base ${i}`));
    const bodies = ["Notes saved", "Runbook saved", "Second project saved"];
    const results = await race(paths.map((p, i) => [p, bodies[i], { "If-Match": tags[i] }]));
    assert.deepEqual(
      results.map((r) => r.status),
      [200, 200, 200],
    );
    for (const [i, p] of paths.entries()) {
      const got = await get(p);
      assert.deepEqual(got.body, Buffer.from(bodies[i]));
      assert.equal(strong(got.headers.get("ETag")), strong(results[i].headers.get("ETag")));
    }
    for (const slug of PROJECTS)
      assert.deepEqual((await get(`/api/projects/${slug}.md`)).body, config);
  });

  test("update race", { skip }, async () => {
    const p = path();
    const tag = strong((await get(p)).headers.get("ETag"));
    const bodies = ["Updated A\n", "Updated B\n"];
    await assertWinner(p, bodies, await race(bodies.map((b) => [p, b, { "If-Match": tag }])));
  });

  test("validation, listing and missing registration", { skip }, async () => {
    const p = path();
    const original = await get(p);
    const tag = strong(original.headers.get("ETag"));
    async function assertUnchanged() {
      const current = await get(p);
      assert.deepEqual(current.body, original.body);
      assert.equal(strong(current.headers.get("ETag")), tag);
    }
    for (const [headers, body, expected] of [
      [{}, "valid", 428],
      [{ "If-Match": "unquoted" }, "valid", 400],
      [{ "If-Match": tag, "Content-Type": "text/plain" }, "valid", 415],
      [{ "If-Match": tag, "Content-Type": "text/markdown" }, Buffer.from([0xff]), 400],
      [{ "If-Match": tag, "Content-Type": "text/markdown" }, "\0", 400],
      [{ "If-Match": tag, "Content-Type": "text/markdown" }, "x".repeat((1 << 20) + 1), 413],
      [{ "If-Match": '"stale"', "Content-Type": "text/markdown" }, "valid", 412],
      [{ "If-None-Match": "*", "Content-Type": "text/markdown" }, "valid", 412],
    ]) {
      const rejected = await request(url, p, { method: "PUT", body, headers });
      assert.equal(rejected.status, expected, JSON.stringify(headers));
      if (headers["If-Match"] === "unquoted")
        assert.equal(rejected.body.toString(), "invalid document write condition");
      await assertUnchanged();
    }
    const transport = await request(url, p, {
      method: "PUT",
      body: "valid",
      headers: { "If-Match": "", "Content-Type": "text/markdown" },
    });
    assert.ok([400, 428].includes(transport.status));
    assert.equal(
      transport.body.toString(),
      transport.status === 400
        ? "invalid document write condition"
        : 'PUT needs If-Match: "<etag>" or If-None-Match: *',
    );
    await assertUnchanged();
    for (const method of ["GET", "PUT"]) {
      const body = method === "PUT" ? "text" : undefined;
      const r = await request(url, path("documents-unregistered"), { method, body });
      assert.equal(r.status, 404);
    }
    assert.equal((await get("/api/project-documents/documents-unregistered")).status, 404);
    for (const name of ["config.md", "Notes.md", "notes.md/extra", "%6eotes.md"])
      assert.equal((await get(path(undefined, name))).status, 404, name);
    const listing = JSON.parse((await get("/api/project-documents/documents-sample")).body);
    assert.deepEqual(
      listing.map((item) => item.name),
      ["notes.md", "decisions.md", "runbook.md"],
    );
    for (const item of listing) {
      assert.deepEqual(Object.keys(item).sort(), ["exists", "name"]);
      assert.equal(typeof item.exists, "boolean");
    }
    const exact = Buffer.from("# Notes\r\nUnicode \u{1F408} <script>literal</script>\r\n");
    assert.equal((await put(p, exact, { "If-Match": tag })).status, 200);
    assert.deepEqual((await get(p)).body, exact);
  });
});
