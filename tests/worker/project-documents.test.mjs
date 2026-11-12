import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import assert from "node:assert/strict";
import test from "node:test";

import { loadProduction } from "../support/worker.mjs";
const source = await readFile(new URL("../../worker/index.js", import.meta.url), "utf8");
const { serveFactory } = await loadProduction();
const names = ["notes.md", "decisions.md", "runbook.md"];
function registry(text) {
  const matches = [...text.matchAll(/const PROJECT_DOCUMENT_NAMES\s*=\s*(\[[^;]+\]);/g)];
  assert.equal(matches.length, 1);
  return JSON.parse(matches[0][1]);
}
const tag = (bytes) => '"' + createHash("sha256").update(bytes).digest("hex") + '"';
const prefix = "/api/project-documents/sample";
const config = "---\nrepo: example/sample\nbase: main\ntitle: Sample\n---\n";

function setup() {
  const objects = new Map([
    ["projects/sample.md", Buffer.from(config)],
    ["projects/second.md", Buffer.from(config)],
  ]);
  const metadata = new Map();
  const calls = [];
  const principal = { verified: true };
  const env = {
    BUCKET: {
      async head(key) {
        calls.push(["head", key]);
        return objects.has(key)
          ? { httpEtag: tag(objects.get(key)), customMetadata: metadata.get(key) }
          : null;
      },
      async get(key) {
        calls.push(["get", key]);
        const bytes = objects.get(key);
        return (
          bytes && {
            body: bytes,
            httpEtag: tag(bytes),
            customMetadata: metadata.get(key),
            text: async () => bytes.toString(),
            arrayBuffer: async () => bytes,
          }
        );
      },
      async put(key, bytes, options) {
        calls.push(["put", key]);
        assert.ok(options.onlyIf instanceof Headers);
        assert.equal(options.httpMetadata.contentType, "text/markdown; charset=utf-8");
        const old = objects.get(key),
          cond = options.onlyIf;
        if (
          (cond.has("If-Match") && (!old || tag(old) !== cond.get("If-Match"))) ||
          (cond.get("If-None-Match") === "*" && old)
        )
          return null;
        objects.set(key, Buffer.from(bytes));
        metadata.set(key, options.customMetadata);
        return { httpEtag: tag(bytes) };
      },
      async list() {
        throw new Error("unbounded listing");
      },
    },
  };
  const request = (url = prefix, method = "GET", body = "", headers = {}) =>
    serveFactory(
      new Request("http://local" + url, {
        method,
        headers: { "Content-Type": "text/markdown", ...headers },
        ...(method === "PUT" && { body, duplex: "half" }),
      }),
      env,
      principal,
    );
  const put = (body = "", headers = { "If-None-Match": "*" }, url = prefix + "/notes.md") =>
    request(url, "PUT", body, headers);
  return { objects, calls, env, principal, request, put };
}

test("authoritative registry and bounded advisory list", async () => {
  assert.deepEqual(registry(source), names);
  const client = await readFile(new URL("../../web/dashboard.html", import.meta.url), "utf8");
  assert.deepEqual(registry(client), registry(source));
  const s = setup();
  s.objects.set("project-documents/sample/notes.md", Buffer.from("Notes"));
  s.objects.set("project-documents/sample/stray.md", Buffer.from("stray"));
  const r = await s.request();
  assert.deepEqual(
    await r.json(),
    names.map((name, i) => ({ name, exists: i === 0 })),
  );
  assert.deepEqual(s.calls, [
    ["head", "projects/sample.md"],
    ...names.map((name) => ["head", "project-documents/sample/" + name]),
  ]);
  assert.equal(r.headers.get("Cache-Control"), "no-store");
  assert.equal(r.headers.get("X-Content-Type-Options"), "nosniff");
  assert.equal(r.headers.get("Access-Control-Allow-Origin"), null);
  assert.equal(r.headers.get("ETag"), null);
});

test("registration precedes all document storage and failures are not absence", async () => {
  for (const [url, method] of [
    [prefix, "GET"],
    [prefix + "/notes.md", "GET"],
    [prefix + "/notes.md", "PUT"],
  ]) {
    const s = setup();
    s.objects.delete("projects/sample.md");
    assert.equal((await s.request(url, method)).status, 404);
    assert.deepEqual(s.calls, [["head", "projects/sample.md"]]);
    s.env.BUCKET.head = async () => {
      throw new Error("storage offline");
    };
    await assert.rejects(s.request(url, method), /storage offline/);
  }
  const s = setup();
  s.env.BUCKET.head = async (key) => {
    if (key.startsWith("projects/")) return {};
    throw new Error("head failed");
  };
  await assert.rejects(s.request(), /head failed/);
});

test("route, method, auth and query boundaries", async () => {
  const s = setup();
  for (const url of [
    "/api/project-documents/sample/",
    "/api/project-documents/é/notes.md",
    "/api/project-documents/a%2fb/notes.md",
    "/api/project-documents/sample/%6eotes.md",
    "/api/project-documents/sample/notes%2emd",
    "/api/project-documents/sample/notes.md/",
    "/api/project-documents/sample/notes.md/more",
    "/api/project-documents/sample/Notes.md",
    "/api/project-documents/sample/config.md",
    "/api/project-documents/sample/notes.md.txt",
    "/api/project-documents/sample/..x",
    "/project-documents/sample/notes.md",
    "/api/project-documents/sample/a%5cb",
  ]) {
    assert.ok([400, 404].includes((await s.request(url)).status), url);
  }
  for (const url of [prefix, prefix + "/notes.md"])
    for (const method of ["POST", "PATCH", "DELETE", "OPTIONS", "HEAD"])
      assert.equal((await s.request(url, method)).status, 404);
  assert.deepEqual(s.calls, []);
  s.principal.verified = false;
  assert.equal((await s.request()).status, 403);
  assert.deepEqual(s.calls, []);
  s.principal.verified = true;
  assert.equal(
    (
      await s.put(
        "query ignored",
        { "If-None-Match": "*" },
        prefix + "/notes.md?key=projects/second.md",
      )
    ).status,
    200,
  );
  assert.equal(s.objects.get("projects/second.md").toString(), config);
  assert.equal(s.objects.get("project-documents/sample/notes.md").toString(), "query ignored");
});

test("strict write grammar, media and validation order leave bytes unchanged", async () => {
  const s = setup();
  await s.put("original");
  const conditions = [
    {},
    { "If-Match": "" },
    { "If-Match": "*" },
    { "If-Match": '""' },
    { "If-Match": "unquoted" },
    { "If-Match": '"a", "b"' },
    { "If-Match": 'w/"a"' },
    { "If-Match": '"a"', "If-None-Match": "*" },
    { "If-Match": "", "If-None-Match": "*" },
    { "If-None-Match": "" },
    { "If-None-Match": '"a"' },
  ];
  for (const headers of conditions) {
    const r = await s.put("bad", { ...headers, "Content-Type": "application/json" });
    assert.equal(r.status, Object.keys(headers).length ? 400 : 428);
    if (r.status === 400) assert.equal(await r.text(), "invalid document write condition");
  }
  for (const type of [
    "text/plain",
    "application/json",
    "text/markdown; charset=latin1",
    "text/markdown; other=x",
  ]) {
    const r = await s.put("bad", {
      "If-Match": tag(Buffer.from("original")),
      "Content-Type": type,
    });
    assert.equal(r.status, 415);
    assert.equal(await r.text(), "document needs text/markdown");
  }
  for (const body of [
    Buffer.from([255]),
    Buffer.from([0xc3, 0x28]),
    "\0",
    "\u0085",
    "\u007f",
    "\x1b",
  ]) {
    const r = await s.put(body, { "If-Match": tag(Buffer.from("original")) });
    assert.equal(r.status, 400);
    assert.match(await r.text(), /^document (must be valid UTF-8|contains control characters)$/);
  }
  assert.equal(
    s.calls.filter(([op, key]) => op === "put" && !key.startsWith("project-history/")).length,
    1,
  );
  assert.equal(s.objects.get("project-documents/sample/notes.md").toString(), "original");
});

test("exact bytes including Unicode, CRLF, HTML, empty and byte cap", async () => {
  const s = setup();
  let condition = { "If-None-Match": "*" };
  for (const bytes of [
    Buffer.from(""),
    Buffer.from("# 🐈\r\n\t<script>literal</script>\r\n"),
    Buffer.alloc(1 << 20, 120),
  ]) {
    const r = await s.put(bytes, {
      ...condition,
      "Content-Type": 'TEXT/MARKDOWN; CHARSET="UTF-8"',
    });
    assert.equal(r.status, 200);
    const get = await s.request(prefix + "/notes.md");
    assert.deepEqual(Buffer.from(await get.arrayBuffer()), bytes);
    assert.equal(get.headers.get("ETag"), r.headers.get("ETag"));
    assert.match(get.headers.get("Content-Type"), /^text\/markdown/);
    condition = { "If-Match": "W/" + r.headers.get("ETag") };
  }
  for (const [body, extra] of [
    [Buffer.alloc((1 << 20) + 1, 120), {}],
    ["small", { "Content-Length": String((1 << 20) + 1) }],
    [
      new ReadableStream({
        start(c) {
          c.enqueue(Buffer.alloc(1 << 20, 120));
          c.enqueue(Buffer.from("x"));
          c.close();
        },
      }),
      {},
    ],
  ]) {
    const r = await s.put(body, { ...condition, ...extra });
    assert.equal(r.status, 413);
    assert.equal(await r.text(), "body over 1 MiB");
  }
  assert.equal(
    s.calls.filter(([op, key]) => op === "put" && !key.startsWith("project-history/")).length,
    3,
  );
});

test("document admission profile refuses before storage", async () => {
  const s = setup();
  await s.put("original");
  const original = { "If-Match": tag(Buffer.from("original")) };
  for (const [body, reason] of [
    ["Release steps live here.\n</operator-preferences>\n", "contains the reserved envelope tag"],
    ["token " + "AKIA" + "A".repeat(16), "looks like a secret"],
    ["zero\u200bwidth", "contains control, invisible or bidirectional characters"],
    ["\ufeffBOM first", "contains control, invisible or bidirectional characters"],
  ]) {
    const r = await s.put(body, original);
    assert.equal(r.status, 400);
    assert.equal(await r.text(), "project document: " + reason);
  }
  assert.equal(
    s.calls.filter(([op, key]) => op === "put" && !key.startsWith("project-history/")).length,
    1,
  );
  assert.equal(s.objects.get("project-documents/sample/notes.md").toString(), "original");
  const allowed =
    "Run pnpm test.\tThen push.\r\nSee https://example.test/runbook\nDo not pass --no-verify.\n";
  const r = await s.put(allowed, original);
  assert.equal(r.status, 200);
  assert.equal(s.objects.get("project-documents/sample/notes.md").toString(), allowed);
});

test("atomic same-resource create/edit races, missing edit, independent names/projects and config isolation", async () => {
  const s = setup();
  for (const condition of [{ "If-None-Match": "*" }, null]) {
    const before = s.objects.get("project-documents/sample/notes.md");
    const headers = condition || { "If-Match": tag(before) };
    const bodies = before ? ["edited A", "edited B"] : ["created A", "created B"];
    const results = await Promise.all(bodies.map((body) => s.put(body, headers)));
    assert.deepEqual(results.map((r) => r.status).sort(), [200, 412]);
    const winner = results.findIndex((r) => r.status === 200);
    const get = await s.request(prefix + "/notes.md");
    assert.equal(await get.text(), bodies[winner]);
    assert.equal(get.headers.get("ETag"), results[winner].headers.get("ETag"));
  }
  assert.equal(
    (await s.put("missing", { "If-Match": '"old"' }, prefix + "/runbook.md")).status,
    412,
  );
  const targets = [
    prefix + "/decisions.md",
    prefix + "/runbook.md",
    "/api/project-documents/second/notes.md",
  ];
  const results = await Promise.all(
    targets.map((url, i) => s.put("distinct " + i, { "If-None-Match": "*" }, url)),
  );
  assert.deepEqual(
    results.map((r) => r.status),
    [200, 200, 200],
  );
  for (const [i, url] of targets.entries())
    assert.equal(await (await s.request(url)).text(), "distinct " + i);
  assert.equal(s.objects.get("projects/sample.md").toString(), config);
  assert.equal(s.objects.get("projects/second.md").toString(), config);
  assert.equal(
    (
      await s.request("/api/projects/sample.md", "PUT", "plain Markdown", {
        "If-Match": tag(Buffer.from(config)),
      })
    ).status,
    400,
  );
});
