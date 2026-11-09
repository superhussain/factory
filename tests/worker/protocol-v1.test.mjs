import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { loadProduction, memoryBucket, serverEnv } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const fixture = JSON.parse(
  await readFile(new URL("../fixtures/protocol-v1.json", import.meta.url)),
);
const project = "---\nrepo: example/sample\nbase: main\ntitle: Sample\n---\n";
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const tag = (bytes) => '"' + sha(bytes) + '"';
const snapshot = (bucket) =>
  [...bucket.objects].map(([key, object]) => [
    key,
    {
      ...object,
      bytes: Buffer.from(object.bytes),
      httpMetadata: { ...object.httpMetadata },
      customMetadata: { ...object.customMetadata },
      options: {
        ...object.options,
        ...(object.options.onlyIf && { onlyIf: Object.fromEntries(object.options.onlyIf) }),
      },
    },
  ]);

function png(width = 1, height = 1, length = 29) {
  const bytes = Buffer.alloc(length);
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]).copy(bytes);
  bytes.writeUInt32BE(13, 8);
  bytes.write("IHDR", 12);
  bytes.writeUInt32BE(width, 16);
  bytes.writeUInt32BE(height, 20);
  return bytes;
}

function fixtureBody(name) {
  if (name === "invalid-utf8") return Buffer.from([255]);
  if (name === "png") return png();
  if (name === "png-truncated") return png().subarray(0, 20);
  if (name === "png-wrong-chunk") {
    const bytes = png();
    bytes.write("IDAT", 12);
    return bytes;
  }
  if (name === "png-too-wide") return png(4097, 1);
  if (name === "png-too-many-pixels") return png(4096, 4096);
  if (name === "text-overflow") return Buffer.alloc((1 << 20) + 1, 120);
  if (name === "image-overflow") return Buffer.alloc((5 << 20) + 1, 120);
  if (name === "text-stream-overflow" || name === "image-stream-overflow") {
    const max = name.startsWith("text") ? 1 << 20 : 5 << 20;
    return new ReadableStream({
      start(controller) {
        controller.enqueue(Buffer.alloc(max, 120));
        controller.enqueue(Buffer.from("x"));
        controller.close();
      },
    });
  }
  throw new Error("unknown body fixture " + name);
}

async function setup() {
  const bucket = memoryBucket();
  for (const [key, body] of [
    ["board.json", '{"jobs":[]}'],
    ["backlog.md", "# Backlog\n"],
    ["projects/sample.md", project],
    ["project-documents/sample/notes.md", "# Notes\n"],
    ["jobs/sample/001/build.md", "# Build\n"],
  ])
    await bucket.put(key, body);
  bucket.calls.length = 0;
  return {
    bucket,
    env: { ...serverEnv, BUCKET: bucket },
    request(path, method = "GET", body, headers = {}) {
      return serveFactory(
        new Request(serverEnv.FACTORY_ORIGIN + path, {
          method,
          headers,
          ...(body !== undefined && { body, duplex: "half" }),
        }),
        this.env,
        { verified: true },
      );
    },
  };
}

function calls(bucket) {
  return bucket.calls.map(([op, key, options]) => ({
    op,
    ...(op === "list" ? { options: key } : { key }),
    ...(op === "put" && {
      options: {
        ...options,
        ...(options.onlyIf && { onlyIf: Object.fromEntries(options.onlyIf) }),
      },
    }),
  }));
}

for (const item of fixture.cases) {
  test(item.id, async () => {
    const s = await setup();
    const before = snapshot(s.bucket);
    const body = item.body?.fixture ? fixtureBody(item.body.fixture) : (item.body ?? undefined);
    const response = await s.request(item.path, item.method, body, item.headers);
    assert.equal(response.status, item.expected.status);
    assert.equal(await response.text(), item.expected.body);
    for (const [name, value] of Object.entries(item.expected.headers))
      assert.equal(response.headers.get(name), value, name);
    assert.deepEqual(calls(s.bucket), item.expected.storage);
    if (item.method === "PUT" && response.status !== 200)
      assert.deepEqual(snapshot(s.bucket), before);
  });
}

test("job optional writes, strong/weak CAS, independent listings and sandbox image types", async () => {
  const s = await setup();
  const path = "/api/jobs/sample/001/build.md";
  const initial = await s.request(path);
  const stale = initial.headers.get("ETag");
  const blind = await s.request(path, "PUT", "blind");
  assert.equal(blind.status, 200);
  const before = snapshot(s.bucket);
  assert.equal((await s.request(path, "PUT", "stale", { "If-Match": stale })).status, 412);
  assert.deepEqual(snapshot(s.bucket), before);
  const responses = await Promise.all(
    ["A", "B"].map((body) =>
      s.request(path, "PUT", body, { "If-Match": "W/" + blind.headers.get("ETag") }),
    ),
  );
  assert.deepEqual(responses.map((r) => r.status).sort(), [200, 412]);
  const winner = responses.findIndex((r) => r.status === 200);
  const read = await s.request(path);
  assert.equal(await read.text(), ["A", "B"][winner]);
  assert.equal(read.headers.get("ETag"), responses[winner].headers.get("ETag"));
  assert.deepEqual(await (await s.request("/api/jobs/sample/001/")).json(), ["build.md"]);
  for (const query of ["?etags", "?etags=0", "?etags=other"]) {
    assert.deepEqual(await (await s.request("/api/jobs/sample/001/" + query)).json(), [
      { name: "build.md", etag: read.headers.get("ETag") },
    ]);
  }
  for (const [extension, type] of [
    ["png", "image/png"],
    ["jpg", "image/jpeg"],
    ["jpeg", "image/jpeg"],
    ["gif", "image/gif"],
    ["webp", "image/webp"],
  ]) {
    const target = `/api/jobs/sample/001/round-1/shot.${extension}`;
    const bytes = Buffer.from([0, 255, 1, 2]);
    assert.equal(
      (await s.request(target, "PUT", bytes, { "Content-Type": "text/html" })).status,
      200,
    );
    const image = await s.request(target.replace("/api", ""));
    assert.deepEqual(Buffer.from(await image.arrayBuffer()), bytes);
    assert.equal(image.headers.get("Content-Type"), type);
    assert.equal(image.headers.get("Content-Security-Policy"), "default-src 'none'; sandbox");
    assert.equal(s.bucket.objects.get(target.slice(5)).httpMetadata.contentType, type);
  }
});

test("walkthrough html: page names only, 1 MiB cap, frame loads only, sandbox CSP on both read paths", async () => {
  const s = await setup();
  const csp =
    "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; " +
    "style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; " +
    "frame-ancestors 'self'";
  const body = "<!doctype html><p>hi</p><script>1</script>";
  const pages = {
    "walkthrough.html": "spec",
    "build.walkthrough.html": "build",
    "round-1/review-ui.walkthrough.html": "round-1/review-ui",
  };
  const tags = {};
  for (const name of [
    "walkthrough.html",
    "build.walkthrough.html",
    "round-1/review-ui.walkthrough.html",
  ]) {
    const put = await s.request("/api/jobs/sample/001/" + name, "PUT", body, {
      "Content-Type": "image/png",
    });
    assert.equal(put.status, 200, name);
    tags[name] = put.headers.get("ETag");
    for (const path of ["/jobs/sample/001/" + name, "/api/jobs/sample/001/" + name]) {
      const r = await s.request(path, "GET", undefined, { "Sec-Fetch-Dest": "iframe" });
      assert.equal(r.status, 200, path);
      assert.equal(await r.text(), body);
      assert.equal(r.headers.get("Content-Type"), "text/html; charset=utf-8");
      assert.equal(r.headers.get("Content-Security-Policy"), csp);
      assert.equal(r.headers.get("Referrer-Policy"), "no-referrer");
      assert.equal(r.headers.get("X-Content-Type-Options"), "nosniff");
      assert.equal(r.headers.get("Cache-Control"), "no-store, no-transform");
      assert.equal(r.headers.get("ETag"), tags[name]);
      for (const headers of [
        { "Sec-Fetch-Dest": "document" },
        { "Sec-Fetch-Dest": "empty" },
        { "Sec-Fetch-Dest": "object" },
        { "Sec-Fetch-Dest": "embed" },
        {},
      ]) {
        const refused = await s.request(path, "GET", undefined, headers);
        assert.equal(refused.status, 403, path + " " + JSON.stringify(headers));
        const text = await refused.text();
        assert.ok(text.includes("<p>Open this walkthrough from its job in the dashboard.</p>"));
        assert.ok(
          text.includes(`<a href="/#001/${pages[name]}" target="_top">Open job 001</a>`),
          text,
        );
        assert.equal(refused.headers.get("Content-Type"), "text/html; charset=utf-8");
        assert.equal(
          refused.headers.get("Content-Security-Policy"),
          "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'",
        );
        assert.equal(refused.headers.get("Cache-Control"), "no-store");
      }
    }
  }
  const service = { verified: true, kind: "service", client_id: "device-1" };
  const pullPath = "/api/jobs/sample/001/walkthrough.html";
  const pull = await serveFactory(new Request(serverEnv.FACTORY_ORIGIN + pullPath), s.env, service);
  assert.equal(pull.status, 200);
  assert.equal(await pull.text(), body);
  assert.equal(pull.headers.get("Content-Security-Policy"), csp);
  assert.equal(pull.headers.get("Cache-Control"), "no-store, no-transform");
  assert.equal(pull.headers.get("ETag"), tags["walkthrough.html"]);
  const specPath = "/api/jobs/sample/001/spec.md";
  assert.equal((await s.request(specPath, "PUT", "# spec", { "If-None-Match": "*" })).status, 200);
  const spec = await s.request(specPath);
  assert.equal(spec.headers.get("Cache-Control"), "no-store");
  assert.ok(spec.headers.get("ETag"));
  const tab = await serveFactory(
    new Request(serverEnv.FACTORY_ORIGIN + pullPath, { headers: { "Sec-Fetch-Dest": "document" } }),
    s.env,
    service,
  );
  assert.equal(tab.status, 403);
  for (const path of [
    "/api/jobs/sample/001/round-1/walkthrough.html",
    "/api/jobs/sample/001/other.html",
    "/api/jobs/sample/001/walkthrough.htm",
    "/api/jobs/sample/001/a.b.walkthrough.html",
    "/api/jobs/sample/001/.walkthrough.html",
    "/api/jobs/sample/001/build.walkthrough.htm",
    "/api/jobs/sample/001/round-1/x/y.walkthrough.html",
  ]) {
    assert.equal((await s.request(path, "PUT", body)).status, 404, path);
  }
  assert.equal((await s.request("/jobs/sample/001/other.html")).status, 404);
  for (const name of ["walkthrough.html", "build.walkthrough.html"]) {
    const big = await s.request("/api/jobs/sample/001/" + name, "PUT", "x".repeat((1 << 20) + 1));
    assert.equal(big.status, 413, name);
    assert.equal(await big.text(), "body over 1 MiB");
  }
});

test("idea image exact digest, envelope metadata and idempotent create-only upload", async () => {
  const s = await setup();
  const bytes = png();
  const name = sha(bytes) + ".png";
  const path = "/api/idea-images/" + name;
  const headers = { "Content-Type": "image/png" };
  assert.equal((await s.request(path, "PUT", bytes, headers)).status, 200);
  const before = snapshot(s.bucket);
  assert.equal((await s.request(path, "PUT", bytes, headers)).status, 200);
  assert.deepEqual(snapshot(s.bucket), before);
  const stored = s.bucket.objects.get("idea-images/" + name);
  assert.deepEqual(stored.customMetadata, {
    sha256: sha(bytes),
    width: "1",
    height: "1",
    length: "29",
  });
  const puts = s.bucket.calls.filter(([op]) => op === "put");
  assert.equal(puts.length, 2);
  for (const [, key, options] of puts) {
    assert.equal(key, "idea-images/" + name);
    assert.equal(options.onlyIf.get("If-None-Match"), "*");
    assert.deepEqual(options.httpMetadata, { contentType: "image/png" });
  }
  const response = await s.request(path);
  assert.deepEqual(Buffer.from(await response.arrayBuffer()), bytes);
  assert.equal(response.headers.get("Content-Security-Policy"), "default-src 'none'; sandbox");
  assert.equal(response.headers.get("Content-Type"), "image/png");
});

test("board image reference counts, UUIDs, authoritative checks and stored-reference exemption", async () => {
  const s = await setup();
  const path = "/api/board";
  const write = (jobs) =>
    s.request(path, "PUT", JSON.stringify({ jobs }), {
      "If-Match": s.bucket.objects.get("board.json").httpEtag,
    });
  const names = Array.from({ length: 9 }, (_, i) => i.toString(16).padStart(64, "0") + ".png");
  const invalid = [
    [{ capture_id: "not-uuid" }],
    [{ idea_images: [] }],
    [{ idea_images: names.slice(0, 5) }],
    [{ idea_images: [names[0], names[0]] }],
    [{ idea_images: ["BAD.png"] }],
    [{ idea_images: names[0] }],
    ...[null, 1, {}].map((idea_images) => [{ idea_images }]),
  ];
  for (const jobs of invalid) {
    const before = snapshot(s.bucket);
    assert.equal((await write(jobs)).status, 400);
    assert.deepEqual(snapshot(s.bucket), before);
  }
  assert.equal(
    await (await write([{ idea_images: [names[0]] }])).text(),
    `idea image ${names[0]} is not uploaded`,
  );
  await s.bucket.put("idea-images/" + names[0], png(), { customMetadata: { sha256: "wrong" } });
  assert.equal((await write([{ idea_images: [names[0]] }])).status, 400);
  for (const name of names)
    await s.bucket.put("idea-images/" + name, png(), {
      customMetadata: { sha256: name.slice(0, 64) },
    });
  assert.equal(
    await (
      await write([
        { idea_images: names.slice(0, 4) },
        { idea_images: names.slice(4, 8) },
        { idea_images: names.slice(8) },
      ])
    ).text(),
    "too many new idea images in one board write",
  );
  assert.equal(
    (
      await write([
        { capture_id: "12345678-1234-1234-1234-123456789abc", idea_images: names.slice(0, 4) },
      ])
    ).status,
    200,
  );
  s.bucket.objects.delete("idea-images/" + names[0]);
  s.bucket.calls.length = 0;
  assert.equal((await write([{ idea_images: [names[0]] }])).status, 200);
  assert.deepEqual(
    s.bucket.calls.map(([op, key]) => [op, key]),
    [
      ["get", "board.json"],
      ["put", "board.json"],
    ],
  );
});

test("text and binary exact caps, streamed rejection and nonmutation", async () => {
  const s = await setup();
  for (const [path, limit] of [
    ["/api/jobs/sample/001/full.md", 1 << 20],
    ["/api/jobs/sample/001/full.png", 5 << 20],
  ]) {
    const bytes = Buffer.alloc(limit, 120);
    const response = await s.request(path, "PUT", bytes);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("ETag"), tag(bytes));
    assert.deepEqual(Buffer.from(await (await s.request(path)).arrayBuffer()), bytes);
    const before = snapshot(s.bucket);
    for (const [body, headers] of [
      [Buffer.alloc(limit + 1), {}],
      ["x", { "Content-Length": String(limit + 1) }],
      [fixtureBody(limit === 1 << 20 ? "text-stream-overflow" : "image-stream-overflow"), {}],
    ]) {
      assert.equal((await s.request(path, "PUT", body, headers)).status, 413);
      assert.deepEqual(snapshot(s.bucket), before);
    }
  }
});

test("simultaneous board claims preserve the single winner and exact ETag", async () => {
  const s = await setup();
  const etag = s.bucket.objects.get("board.json").httpEtag;
  const bodies = ["A", "B"].map((claim) => JSON.stringify({ jobs: [{ id: "001", claim }] }));
  const responses = await Promise.all(
    bodies.map((body) => s.request("/api/board", "PUT", body, { "If-Match": etag })),
  );
  assert.deepEqual(responses.map((r) => r.status).sort(), [200, 412]);
  const winner = responses.findIndex((r) => r.status === 200);
  const response = await s.request("/board.json");
  assert.equal(await response.text(), bodies[winner]);
  assert.equal(response.headers.get("ETag"), responses[winner].headers.get("ETag"));
});

test("a board write cannot change an existing row's id", async () => {
  const s = await setup();
  const LOGIN = { id: "412-fix-login", project: "sample", stage: "build" };
  const EXPORT = { id: "413-add-export", project: "sample", stage: "spec" };
  const seed = (text) => s.bucket.put("board.json", text);
  const write = (jobs, headers = {}) =>
    s.request("/api/board", "PUT", JSON.stringify({ jobs }), {
      "If-Match": s.bucket.objects.get("board.json").httpEtag,
      ...headers,
    });
  const stored = JSON.stringify({ jobs: [LOGIN, EXPORT] });
  const refusal = (id) =>
    `a board write cannot change a row's id: "${id}" is gone and a new row appeared. Nothing was written. Upgrade the factory skill, or drop and re-add the row in two writes.`;
  for (const [jobs, id] of [
    [[{ ...LOGIN, id: "412-fix-login 413-add-export" }, EXPORT], LOGIN.id],
    [[{ project: "sample", stage: "build" }, EXPORT], LOGIN.id],
    [[{ ...LOGIN, id: null }, EXPORT], LOGIN.id],
    [[{ ...LOGIN, id: EXPORT.id }, EXPORT], LOGIN.id],
    [[LOGIN, { ...EXPORT, id: "999-renamed" }], EXPORT.id],
  ]) {
    await seed(stored);
    const before = snapshot(s.bucket);
    const response = await write(jobs);
    assert.equal(response.status, 400);
    assert.equal(await response.text(), refusal(id));
    assert.deepEqual(snapshot(s.bucket), before);
  }
  {
    await seed(stored);
    const before = snapshot(s.bucket);
    const response = await write([{ ...LOGIN, id: "999-renamed" }, EXPORT], {
      "If-Match": "W/" + s.bucket.objects.get("board.json").httpEtag,
    });
    assert.equal(response.status, 400);
    assert.equal(await response.text(), refusal(LOGIN.id));
    assert.deepEqual(snapshot(s.bucket), before);
  }
  {
    const C1 = { id: "412-fix\u009blogin", project: "sample", stage: "build" };
    await seed(JSON.stringify({ jobs: [C1, EXPORT] }));
    const before = snapshot(s.bucket);
    const response = await write([{ ...C1, id: "999-renamed" }, EXPORT]);
    assert.equal(response.status, 400);
    const text = await response.text();
    assert.equal(text, refusal("412-fix\\u009blogin"));
    assert.ok(!/[\u0080-\u009f]/.test(text));
    assert.deepEqual(snapshot(s.bucket), before);
  }
  for (const [text, jobs] of [
    [stored, [{ ...LOGIN, stage: "review" }, EXPORT]],
    [stored, [EXPORT, LOGIN]],
    [stored, [LOGIN, EXPORT, { id: "414-new-row", stage: "idea" }]],
    [stored, [EXPORT]],
    [stored, []],
    [JSON.stringify({ jobs: [{ project: "sample" }] }), [LOGIN]],
    ["{", [LOGIN]],
  ]) {
    await seed(text);
    assert.equal((await write(jobs)).status, 200, JSON.stringify(jobs));
  }
  await seed(stored);
  const stale = { "If-Match": '"stale"' };
  assert.equal((await write([{ ...LOGIN, id: "999-renamed" }, EXPORT], stale)).status, 412);
  assert.equal(
    (
      await s.request("/api/board", "PUT", JSON.stringify({ jobs: [{ id: "999-renamed" }] }), {
        "If-None-Match": "*",
      })
    ).status,
    412,
  );
});
