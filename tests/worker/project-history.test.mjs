import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test, { mock } from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "operator@example.test" };
const device = { verified: true, kind: "service" };
const hex = (text) => createHash("sha256").update(text).digest("hex");
const config = "---\nrepo: example/sample\nbase: main\ntitle: Sample\n---\n";
const v1 = "# Notes\nRun pnpm test.\n";
const v2 = "# Notes\nRun pnpm test.\nThen deploy.\n";
const uploaded = new Date("2026-11-10T09:00:00.000Z");
const notes = "/api/project-documents/sample/notes.md";
const history = "/api/project-history/sample/notes.md";

// The running example: project sample with notes.md version 1 written before versioning.
function setup() {
  const bucket = memoryBucket();
  const get = bucket.get;
  // R2 objects from get() expose arrayBuffer() and uploaded; the shared memory bucket does not.
  bucket.get = async (key) => {
    const obj = await get(key);
    return obj && { ...obj, arrayBuffer: async () => obj.bytes };
  };
  for (const [key, text] of [
    ["projects/sample.md", config],
    ["project-documents/sample/notes.md", v1],
  ])
    bucket.objects.set(key, {
      key,
      bytes: Buffer.from(text),
      httpEtag: `"${hex(text)}"`,
      uploaded,
    });
  bucket.calls.length = 0;
  const send = (path, { method = "GET", body, headers = {}, principal = person } = {}) =>
    serveFactory(
      new Request("http://local" + path, {
        method,
        headers: method === "PUT" ? { "Content-Type": "text/markdown", ...headers } : headers,
        ...(method === "PUT" && { body, duplex: "half" }),
      }),
      { BUCKET: bucket },
      principal,
    );
  const put = (path, body, headers, principal = person) =>
    send(path, { method: "PUT", body, headers, principal });
  const puts = () => bucket.calls.filter(([op]) => op === "put");
  return { bucket, send, put, puts };
}

const saveAt = async (iso, fn) => {
  mock.timers.enable({ apis: ["Date"], now: Date.parse(iso) });
  try {
    return await fn();
  } finally {
    mock.timers.reset();
  }
};

test("the running example keeps every version with author and summary", async () => {
  const s = setup();
  const second = await saveAt("2026-11-12T18:40:00.000Z", () =>
    s.put(notes, v2, {
      "If-Match": `"${hex(v1)}"`,
      "X-Project-Summary": encodeURIComponent("Add the deploy step"),
    }),
  );
  assert.equal(second.status, 200);
  assert.equal(await second.text(), "ok");
  assert.equal(second.headers.get("ETag"), `"${hex(v2)}"`);
  assert.equal(second.headers.get("X-Project-Version"), "2");
  assert.equal(second.headers.get("X-Project-History"), null);
  // The legacy version 1 is copied first, then version 2 is written and copied.
  assert.deepEqual(
    s.puts().map(([, key, options]) => [key, options.customMetadata]),
    [
      [
        "project-history/sample/notes.md/000001.md",
        { version: "1", saved_at: "2026-11-10T09:00:00.000Z", sha256: hex(v1) },
      ],
      [
        "project-documents/sample/notes.md",
        {
          version: "2",
          saved_at: "2026-11-12T18:40:00.000Z",
          sha256: hex(v2),
          saved_by: "operator@example.test",
          summary: "Add the deploy step",
        },
      ],
      [
        "project-history/sample/notes.md/000002.md",
        {
          version: "2",
          saved_at: "2026-11-12T18:40:00.000Z",
          sha256: hex(v2),
          saved_by: "operator@example.test",
          summary: "Add the deploy step",
        },
      ],
    ],
  );
  for (const [, , options] of s.puts())
    assert.equal(options.httpMetadata.contentType, "text/markdown; charset=utf-8");

  const list = await s.send(history);
  assert.equal(list.status, 200);
  assert.equal(list.headers.get("Content-Type"), "application/json; charset=utf-8");
  assert.deepEqual(await list.json(), [
    {
      version: 2,
      saved_by: "operator@example.test",
      saved_at: "2026-11-12T18:40:00.000Z",
      sha256: hex(v2),
      summary: "Add the deploy step",
      history_copy: true,
    },
    {
      version: 1,
      saved_at: "2026-11-10T09:00:00.000Z",
      sha256: hex(v1),
      summary: "",
      history_copy: true,
    },
  ]);

  const old = await s.send(history + "/1");
  assert.equal(old.status, 200);
  assert.equal(await old.text(), v1);
  assert.equal(old.headers.get("Content-Type"), "text/markdown; charset=utf-8");
  assert.equal(old.headers.get("X-Project-Version"), "1");

  const third = await s.put(notes, v1, {
    "If-Match": second.headers.get("ETag"),
    "X-Project-Summary": encodeURIComponent("Undo the deploy step"),
  });
  assert.equal(third.status, 200);
  assert.equal(third.headers.get("X-Project-Version"), "3");
  const after = await (await s.send(history)).json();
  assert.deepEqual(
    after.map((v) => [v.version, v.summary]),
    [
      [3, "Undo the deploy step"],
      [2, "Add the deploy step"],
      [1, ""],
    ],
  );
  assert.equal(await (await s.send(history + "/3")).text(), v1);
  assert.equal(await (await s.send(history + "/2")).text(), v2);

  // A device reads the list without saved_by.
  const seen = await (await s.send(history, { principal: device })).json();
  assert.ok(seen.every((v) => !("saved_by" in v)));
});

test("a device save of the project file is recorded as device", async () => {
  const s = setup();
  const r = await s.put(
    "/api/projects/sample.md",
    config + "# Body\n",
    {
      "If-Match": `"${hex(config)}"`,
    },
    device,
  );
  assert.equal(r.status, 200);
  assert.equal(r.headers.get("X-Project-Version"), "2");
  const list = await (await s.send("/api/project-history/sample/project.md")).json();
  assert.deepEqual(
    list.map((v) => [v.version, v.saved_by, v.history_copy]),
    [
      [2, "device", true],
      [1, undefined, true],
    ],
  );
  assert.equal(
    s.bucket.objects.get("project-history/sample/project.md/000002.md").bytes.toString(),
    config + "# Body\n",
  );
  const fresh = await s.put("/api/projects/fresh.md", config, { "If-None-Match": "*" });
  assert.equal(fresh.headers.get("X-Project-Version"), "1");
  assert.ok(s.bucket.objects.has("project-history/fresh/project.md/000001.md"));
});

test("an object written before deploy lists and serves as version 1", async () => {
  const s = setup();
  assert.deepEqual(await (await s.send(history)).json(), [
    { version: 1, saved_at: "2026-11-10T09:00:00.000Z", summary: "", history_copy: false },
  ]);
  const old = await s.send(history + "/1");
  assert.equal(await old.text(), v1);
  assert.equal(old.headers.get("X-Project-Version"), "1");
  assert.equal((await s.send(history + "/2")).status, 404);
  assert.deepEqual(await (await s.send("/api/project-history/sample/runbook.md")).json(), []);
  assert.equal(s.puts().length, 0);
});

test("a failed history copy still saves and the next save heals it", async () => {
  const s = setup();
  const put = s.bucket.put;
  s.bucket.put = async (key, value, options) => {
    if (key.endsWith("/000002.md")) throw new Error("synthetic history failure");
    return put(key, value, options);
  };
  const warn = mock.method(console, "warn", () => {});
  const failed = await s.put(notes, v2, { "If-Match": `"${hex(v1)}"` });
  assert.deepEqual(
    warn.mock.calls.map((call) => call.arguments),
    [["project history copy failed: project-history/sample/notes.md/000002.md"]],
  );
  warn.mock.restore();
  assert.equal(failed.status, 200);
  assert.equal(failed.headers.get("X-Project-Version"), "2");
  assert.equal(failed.headers.get("X-Project-History"), "missing");
  s.bucket.put = put;
  assert.deepEqual(
    (await (await s.send(history)).json()).map((v) => [v.version, v.history_copy]),
    [
      [2, false],
      [1, true],
    ],
  );
  assert.equal(await (await s.send(history + "/2")).text(), v2);
  const healed = await s.put(notes, "# Notes\n", { "If-Match": failed.headers.get("ETag") });
  assert.equal(healed.headers.get("X-Project-Version"), "3");
  assert.equal(healed.headers.get("X-Project-History"), null);
  assert.equal(
    s.bucket.objects.get("project-history/sample/notes.md/000002.md").customMetadata.version,
    "2",
  );
  assert.ok(s.bucket.objects.has("project-history/sample/notes.md/000003.md"));
});

test("two saves against the same version race to one 200 and one 412", async () => {
  const s = setup();
  const results = await Promise.all(
    ["A", "B"].map((x) => s.put(notes, v1 + x, { "If-Match": `"${hex(v1)}"` })),
  );
  assert.deepEqual(results.map((r) => r.status).sort(), [200, 412]);
  assert.equal((await (await s.send(history)).json()).map((v) => v.version).join(), "2,1");
  assert.equal((await s.put(notes, "stale", { "If-Match": `"${hex(v1)}"` })).status, 412);
});

test("a bad summary is refused before anything is stored", async () => {
  const s = setup();
  for (const [summary, text] of [
    ["%E0%A4%A", "summary: must be percent-encoded UTF-8"],
    [encodeURIComponent("two\nlines"), "summary: must be one line of 1 to 120 characters"],
    ["x".repeat(121), "summary: must be one line of 1 to 120 characters"],
  ]) {
    const r = await s.put(notes, v2, { "If-Match": `"${hex(v1)}"`, "X-Project-Summary": summary });
    assert.equal(r.status, 400);
    assert.equal(await r.text(), text);
  }
  assert.equal(s.puts().length, 0);
  assert.equal(s.bucket.objects.get("project-documents/sample/notes.md").bytes.toString(), v1);
});

test("unregistered slugs, unknown names, versions and methods are 404", async () => {
  const s = setup();
  for (const path of [
    "/api/project-history/missing/notes.md",
    "/api/project-history/missing/notes.md/1",
    "/api/project-history/sample/config.md",
    "/api/project-history/sample/stray.md/1",
    "/api/project-history/sample/notes.md/0",
    "/api/project-history/sample/notes.md/9",
    "/api/project-history/sample/notes.md/",
    "/api/project-history/sample",
  ])
    assert.equal((await s.send(path)).status, 404, path);
  for (const method of ["PUT", "POST", "DELETE", "PATCH"])
    assert.equal((await s.send(history, { method, body: "x" })).status, 404, method);
  assert.equal(s.puts().length, 0);
});

test("a pipeline object without a version still saves as version 1, as before", async () => {
  const s = setup();
  const legacy = JSON.stringify({ schema: 1 });
  s.bucket.objects.set("pipeline/global.json", {
    key: "pipeline/global.json",
    bytes: Buffer.from(legacy),
    httpEtag: `"${hex(legacy)}"`,
    uploaded,
  });
  const body = JSON.stringify({ schema: 1, merge: "human" });
  const saved = await s.send("/api/pipeline/global", {
    method: "PUT",
    body,
    headers: { "Content-Type": "application/json", "If-Match": `"${hex(legacy)}"` },
  });
  assert.equal(saved.status, 200);
  assert.equal(saved.headers.get("X-Pipeline-Version"), "1");
  assert.deepEqual(
    s.puts().map(([, key]) => key),
    ["pipeline/global.json", "pipeline-history/global/000001.json"],
  );
});
