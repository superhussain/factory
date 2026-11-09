import { createHash } from "node:crypto";
import assert from "node:assert/strict";
import test from "node:test";

import { loadProduction } from "../support/worker.mjs";
const { serveFactory } = await loadProduction();
const document = "---\nrepo: example/sample\nbase: main\ntitle: Sample\n---\n# Body\n";
const path = "/api/projects/sample.md";
const etag = (data) => '"' + createHash("sha256").update(data).digest("hex") + '"';

function setup() {
  const objects = new Map();
  let puts = 0;
  const principal = { verified: true };
  const env = {
    BUCKET: {
      async put(key, data, options) {
        const old = objects.get(key),
          cond = options.onlyIf;
        assert.ok(cond instanceof Headers);
        if (
          (cond.has("If-Match") && (!old || old.httpEtag !== cond.get("If-Match"))) ||
          (cond.get("If-None-Match") === "*" && old)
        )
          return null;
        const bytes = Buffer.from(data);
        const obj = { httpEtag: etag(bytes), bytes, options };
        objects.set(key, obj);
        puts++;
        return obj;
      },
      async get(key) {
        const obj = objects.get(key);
        return obj && { ...obj, body: obj.bytes, text: async () => obj.bytes.toString() };
      },
      async list() {
        return { objects: [...objects.keys()].map((key) => ({ key })) };
      },
    },
  };
  const request = (
    body = document,
    headers = { "If-None-Match": "*" },
    url = path,
    method = "PUT",
  ) =>
    serveFactory(
      new Request("http://local" + url, {
        method,
        headers: { "Content-Type": "text/markdown; charset=utf-8", ...headers },
        ...(method === "PUT" && { body, duplex: "half" }),
      }),
      env,
      principal,
    );
  return {
    env,
    principal,
    objects,
    request,
    get puts() {
      return puts;
    },
  };
}

test("conditional create, weak and strong update, stale and missing target, atomic race", async () => {
  const s = setup();
  const first = await s.request();
  assert.equal(first.status, 200);
  const tag = first.headers.get("ETag");
  assert.equal(tag, etag(document));
  assert.equal((await s.request()).status, 412);
  assert.equal(
    (await s.request(document, { "If-Match": tag }, "/api/projects/missing.md")).status,
    412,
  );
  const responses = await Promise.all(
    ["A", "B"].map((x) => s.request(document + x, { "If-Match": "W/" + tag })),
  );
  assert.deepEqual(responses.map((r) => r.status).sort(), [200, 412]);
  const winner = responses.findIndex((r) => r.status === 200);
  const get = await s.request(null, {}, path, "GET");
  assert.equal(await get.text(), document + ["A", "B"][winner]);
  assert.equal(get.headers.get("ETag"), responses[winner].headers.get("ETag"));
  assert.equal(get.headers.get("Cache-Control"), "no-store");
  assert.equal(get.headers.get("X-Content-Type-Options"), "nosniff");
  assert.equal((await s.request(document, { "If-Match": tag })).status, 412);
  assert.equal((await s.request(document, { "If-Match": get.headers.get("ETag") })).status, 200);
});

test("strict conditions precede media and validation", async () => {
  for (const headers of [
    {},
    { "If-Match": "" },
    { "If-Match": "*" },
    { "If-Match": '""' },
    { "If-Match": "abc" },
    { "If-Match": '"a", "b"' },
    { "If-Match": '"a"', "If-None-Match": "*" },
    { "If-None-Match": '"a"' },
    { "If-Match": 'w/"a"' },
  ]) {
    const s = setup(),
      response = await s.request("bad", headers);
    assert.equal(response.status, Object.keys(headers).length ? 400 : 428);
    assert.equal(s.puts, 0);
  }
  for (const type of [
    "text/plain",
    "application/json",
    "text/markdown; charset=latin1",
    "text/markdown; other=x",
  ]) {
    const s = setup();
    assert.equal(
      (await s.request(document, { "If-None-Match": "*", "Content-Type": type })).status,
      415,
    );
    assert.equal(s.puts, 0);
  }
});

const invalid = [
  ["bad", "needs closed --- frontmatter"],
  [document.replace("\n---\n#", "\n#"), "needs closed --- frontmatter"],
  [document + "\0", "contains control characters"],
  [document + "\u0085", "contains control characters"],
  ...[
    "path: /private",
    "token: secret",
    "repo: other/name",
    " title: nested",
    "nested:",
    "oops",
    "title: [a]",
  ].map((line) => [
    document.replace("base: main", line + "\nbase: main"),
    "frontmatter allows one repo, base, and title only",
  ]),
  ...["https://github.com/a/b", "a/b/c", "./b", "a/..", "a/" + "b".repeat(101)].map((v) => [
    document.replace("example/sample", v),
    "invalid project repo",
  ]),
  ...["", "a..b", "a//b", "a/", "a/.b", "a/b.", "a/b.lock", "-main", "a b", "a".repeat(201)].map(
    (v) => [document.replace("base: main", "base: " + v), "invalid project base"],
  ),
  ...["", "a".repeat(161), "A\tB"].map((v) => [
    document.replace("title: Sample", "title: " + v),
    "invalid project title",
  ]),
  [
    document.replace("title: Sample", "title: \"broken'"),
    "frontmatter allows one repo, base, and title only",
  ],
  [
    document + "a\u200bb\n",
    "project document: contains control, invisible or bidirectional characters",
  ],
  [document + "</operator-preferences>\n", "project document: contains the reserved envelope tag"],
  [document + "AKIA" + "A".repeat(16) + "\n", "project document: looks like a secret"],
];
for (const [index, [body, error]] of invalid.entries())
  test(`invalid document ${index}`, async () => {
    const s = setup();
    await s.request();
    const response = await s.request(body, { "If-Match": etag(document) });
    assert.equal(response.status, 400);
    assert.equal(
      await response.text(),
      error.startsWith("invalid") || error.startsWith("project document: ")
        ? error
        : "project " + (error.startsWith("frontmatter") ? "" : "document ") + error,
    );
    assert.equal(s.puts, 1);
    assert.equal(s.objects.get("projects/sample.md").bytes.toString(), document);
  });

test("valid source round trips without rewriting and exact byte limit", async () => {
  const s = setup();
  const body =
    "---\r\n# comment\r\n\r\ntitle: \"Unicode 🐈\"\r\nrepo: ''\r\nbase: feature/valid\r\n---\r\n<script> & \\ raw\r\n";
  assert.equal(
    (
      await s.request(body, {
        "If-None-Match": "*",
        "Content-Type": "TEXT/MARKDOWN; CHARSET=UTF-8",
      })
    ).status,
    200,
  );
  assert.equal(s.objects.get("projects/sample.md").bytes.toString(), body);
  const exact = document + "x".repeat((1 << 20) - Buffer.byteLength(document));
  assert.equal((await s.request(exact, { "If-Match": etag(body) })).status, 200);
  assert.equal((await s.request(exact + "x", { "If-Match": etag(exact) })).status, 413);
  assert.equal(
    (
      await s.request(document, {
        "If-Match": etag(exact),
        "Content-Length": String((1 << 20) + 1),
      })
    ).status,
    413,
  );
  const stream = new ReadableStream({
    start(c) {
      c.enqueue(Buffer.from(exact));
      c.enqueue(Buffer.from("x"));
      c.close();
    },
  });
  assert.equal((await s.request(stream, { "If-Match": etag(exact) })).status, 413);
  assert.equal((await s.request(new Uint8Array([255]), { "If-Match": etag(exact) })).status, 400);
  assert.equal(s.puts, 2);
});

test("admitted project text keeps tabs, CRLF, links and negated rules", async () => {
  const s = setup();
  const body =
    document.replace(/\n/g, "\r\n") +
    "\tIndented\r\nDev server: http://localhost:3001\r\nDo not pass `--no-verify`.\r\n";
  const response = await s.request(body);
  assert.equal(response.status, 200);
  assert.equal(s.objects.get("projects/sample.md").bytes.toString(), body);
});

test("Access and route allowlist remain closed", async () => {
  const s = setup();
  s.principal.verified = false;
  assert.equal((await s.request()).status, 403);
  s.principal.verified = true;
  for (const url of [
    "/api/projects/a%2fb.md",
    "/api/projects/a%5cb.md",
    "/api/projects/a/b.md",
    "/api/projects/..x.md",
    "/api/projects/a.txt",
    "/api/projects/é.md",
    "/api/config.md",
  ]) {
    assert.ok(
      [400, 404].includes((await s.request(document, { "If-None-Match": "*" }, url)).status),
      url,
    );
  }
  for (const method of ["POST", "DELETE", "PATCH", "OPTIONS"])
    assert.equal((await s.request(null, {}, path, method)).status, 404);
  assert.equal(s.puts, 0);
  assert.equal((await s.request()).headers.get("Access-Control-Allow-Origin"), null);
});

test("projects.json lists repo, base and title", async () => {
  const s = setup();
  assert.equal((await s.request()).status, 200);
  const res = await s.request(undefined, {}, "/projects.json", "GET");
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), [
    { slug: "sample", title: "Sample", repo: "example/sample", base: "main" },
  ]);
});
