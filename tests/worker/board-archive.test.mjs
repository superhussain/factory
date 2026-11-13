// The hourly archive pass, its read API and the board write guards that keep archived
// ids and fingerprints from coming back. Rows are the made-up running example.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory, default: worker } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const device = { verified: true, kind: "service" };
const NOW = new Date("2026-11-09T10:17:00Z");

const ROWS = [
  {
    id: "412-fix-login",
    project: "sample",
    title: "Fix login",
    stage: "merged",
    done: "2026-09-01T12:00:00Z",
    jira: "PLAN-9005",
    pr: "https://github.com/example/sample/pull/412",
    goal: "Login survives a token refresh.",
  },
  {
    id: "413-old-suggestion",
    project: "sample",
    title: "Old suggestion",
    stage: "closed",
    done: "2026-08-20T12:00:00Z",
    origin: { fingerprint: "fp-7c1e" },
  },
  {
    id: "414-retry-sync",
    project: "sample",
    title: "Retry sync",
    stage: "closed",
    done: "2026-08-25T12:00:00Z",
    backlog: { decision: "denied", notes: "Try a queue instead." },
  },
  {
    id: "415-add-export",
    project: "sample",
    title: "Add export",
    stage: "merged",
    done: "2026-11-05T12:00:00Z",
  },
  {
    id: "416-rate-limit",
    project: "sample",
    title: "Rate limit",
    stage: "merged",
    done: "2026-09-10T12:00:00Z",
  },
];

function setup({ archive = { mode: "age", n: 30 }, jobs = ROWS, extra = {}, tracer = null } = {}) {
  const bucket = memoryBucket();
  const env = { BUCKET: bucket };
  const seed = (key, value) =>
    bucket.objects.set(key, {
      key,
      bytes: Buffer.from(typeof value === "string" ? value : JSON.stringify(value)),
      httpEtag: `"${key}-seed"`,
    });
  if (archive !== null)
    seed(
      "pipeline/global.json",
      tracer === null
        ? { schema: 1, archive }
        : { schema: 1, archive, tracer: { enabled: tracer } },
    );
  seed("board.json", { updated: "2026-11-09T09:00:00Z", jobs, ...extra });
  const read = (key) => JSON.parse(bucket.objects.get(key).bytes.toString());
  const send = (path, { method = "GET", body, headers = {}, principal = person } = {}) =>
    serveFactory(
      new Request("http://local" + path, {
        method,
        headers,
        ...(body !== undefined && { body, duplex: "half" }),
      }),
      env,
      principal,
    );
  const run = () => {
    let pass;
    worker.scheduled({ scheduledTime: NOW.getTime() }, env, { waitUntil: (p) => (pass ??= p) });
    return pass;
  };
  return { bucket, env, seed, read, send, run };
}

const ids = (jobs) => jobs.map((j) => j.id);

test("an age pass moves 412, 413 and 416, archive first, and leaves a floor", async () => {
  const s = setup();
  assert.deepEqual(await s.run(), {
    status: "archived",
    moved: ["412-fix-login", "413-old-suggestion", "416-rate-limit"],
  });
  const puts = s.bucket.calls.filter(([op]) => op === "put").map(([, key]) => key);
  assert.deepEqual(puts, ["board-archive.json", "board.json"]);
  assert.deepEqual(ids(s.read("board-archive.json").jobs), [
    "416-rate-limit",
    "412-fix-login",
    "413-old-suggestion",
  ]);
  const board = s.read("board.json");
  assert.deepEqual(ids(board.jobs), ["414-retry-sync", "415-add-export"]);
  assert.deepEqual(board.archived, {
    max_id: 416,
    fingerprints: { "fp-7c1e": "413-old-suggestion" },
  });
  assert.equal(board.updated, "2026-11-09T09:00:00Z");
  assert.ok(s.bucket.objects.get("board.json").bytes.toString().endsWith("}\n"));
  // A second pass has nothing to move and writes nothing.
  const before = s.bucket.calls.length;
  assert.deepEqual(await s.run(), { status: "none", moved: [] });
  assert.equal(s.bucket.calls.slice(before).filter(([op]) => op === "put").length, 0);
});

test("Off, an invalid setting and every keep rule leave the board alone", async (t) => {
  const warn = t.mock.method(console, "warn", () => {});
  for (const archive of [null, { mode: "age" }, { mode: "weeks", n: 3 }]) {
    const s = setup({ archive });
    assert.deepEqual(await s.run(), { status: "off", moved: [] });
    assert.equal(s.bucket.objects.has("board-archive.json"), false);
  }
  for (const doc of [
    "{",
    JSON.stringify({ schema: 1 }),
    JSON.stringify({ schema: 2, archive: {} }),
  ]) {
    const s = setup();
    s.seed("pipeline/global.json", doc);
    assert.deepEqual(await s.run(), { status: "off", moved: [] });
  }
  const old = { project: "sample", stage: "merged", done: "2026-01-01T00:00:00Z" };
  const kept = [
    { ...old, id: "501-building", stage: "build" },
    { ...old, id: "502-no-done", done: "soon" },
    { ...old, id: "503-claimed", claim: { device: "host-a" } },
    { ...old, id: "504-needs", needs: "a decision" },
    { ...old, id: "505-escalated", escalated: true },
    { ...old, id: "506-escalated-note", note: "Escalated: auth change" },
    { ...old, id: "507-automerge", automerge: true },
    { ...old, id: "508-queued", build: { state: "queued" } },
    { ...old, id: "509-running", build: { state: "running" } },
    {
      ...old,
      id: "510-deny-notes",
      stage: "closed",
      backlog: { decision: "denied", notes: "Read me." },
    },
    { ...old, id: "511-tracer", tracer: { follow: true } },
  ];
  const s = setup({ jobs: kept, tracer: true });
  assert.deepEqual(await s.run(), { status: "none", moved: [] });
  assert.deepEqual(ids(s.read("board.json").jobs), ids(kept));
  // Once the notes are read and the mirror exists, both may go.
  const done = setup({
    jobs: [
      { ...kept[9], backlog: { ...kept[9].backlog, notes_read: "2026-02-01" } },
      { ...kept[10], tracer: { follow: true, url: "https://tracer.example.test/1" } },
    ],
    tracer: true,
  });
  assert.deepEqual((await done.run()).moved, ["510-deny-notes", "511-tracer"]);
  assert.equal(warn.mock.calls.length, 0);
});

test("Tracer off lets a pending mirror go", async () => {
  const old = { project: "sample", stage: "merged", done: "2026-01-01T00:00:00Z" };
  const s = setup({ jobs: [{ ...old, id: "511-tracer", tracer: { follow: true } }] });
  s.seed("pipeline/global.json", {
    schema: 1,
    archive: { mode: "age", n: 30 },
    tracer: { enabled: false },
  });
  assert.deepEqual(await s.run(), { status: "archived", moved: ["511-tracer"] });
});

test("Tracer unset lets a pending mirror go", async () => {
  const old = { project: "sample", stage: "merged", done: "2026-01-01T00:00:00Z" };
  const s = setup({ jobs: [{ ...old, id: "511-tracer", tracer: { follow: true } }] });
  assert.deepEqual(await s.run(), { status: "archived", moved: ["511-tracer"] });
});

test("a count pass keeps the N newest finished rows", async () => {
  const s = setup({ archive: { mode: "count", n: 2 } });
  // Newest finished: 415, 416 stay. 412 and 413 move; 414 is kept by its unread notes.
  assert.deepEqual((await s.run()).moved, ["412-fix-login", "413-old-suggestion"]);
  assert.deepEqual(ids(s.read("board.json").jobs), [
    "414-retry-sync",
    "415-add-export",
    "416-rate-limit",
  ]);
  assert.equal(s.read("board.json").archived.max_id, 413);
});

test("a lost board race loses nothing, reads hide board ids, and the next pass reconciles", async (t) => {
  const warn = t.mock.method(console, "warn", () => {});
  const s = setup();
  const put = s.bucket.put;
  s.bucket.put = async (key, value, options) => {
    // A device writes the board between the archive write and the board write.
    if (key === "board.json") {
      const raced = { ...s.read("board.json"), updated: "2026-11-09T10:17:30Z" };
      await put("board.json", JSON.stringify(raced));
    }
    return put(key, value, options);
  };
  assert.deepEqual(await s.run(), { status: "conflict", moved: [] });
  assert.deepEqual(
    warn.mock.calls.map((c) => c.arguments),
    [
      [
        "archive pass conflict: board.json changed during the pass; rows stay on the board until the next pass",
      ],
    ],
  );
  s.bucket.put = put;
  assert.equal(s.read("board.json").jobs.length, 5);
  assert.equal(s.read("board-archive.json").jobs.length, 3);
  const page = await (await s.send("/api/board/archive")).json();
  assert.deepEqual(page, { jobs: [], page: 1, pages: 0, total: 0 });
  assert.equal((await s.send("/api/board/archive?id=412-fix-login")).status, 404);
  // The next hour moves them again without duplicating the stale copies.
  assert.equal((await s.run()).status, "archived");
  assert.deepEqual(ids(s.read("board-archive.json").jobs), [
    "416-rate-limit",
    "412-fix-login",
    "413-old-suggestion",
  ]);
  assert.equal(warn.mock.calls.length, 1);
  // A stale copy whose row stays on the board is dropped even when nothing moves.
  const stale = setup({ archive: { mode: "age", n: 1000 } });
  stale.seed("board-archive.json", { jobs: [ROWS[0]] });
  assert.deepEqual(await stale.run(), { status: "none", moved: [] });
  assert.deepEqual(stale.read("board-archive.json").jobs, []);
  assert.equal(stale.read("board.json").jobs.length, 5);
  assert.equal(warn.mock.calls.length, 1);
});

test("a refused archive write warns with its cause and moves nothing", async (t) => {
  const warn = t.mock.method(console, "warn", () => {});
  const bad = setup();
  bad.seed("board-archive.json", "{not json");
  const bytes = (s) => [...s.bucket.objects.values()].map((o) => o.bytes.toString());
  const before = bytes(bad);
  assert.deepEqual(await bad.run(), { status: "conflict", moved: [] });
  assert.deepEqual(
    warn.mock.calls.map((c) => c.arguments),
    [
      [
        "archive pass conflict: board-archive.json is not valid JSON or appeared during the pass; nothing moved",
      ],
    ],
  );
  assert.deepEqual(bytes(bad), before);

  warn.mock.resetCalls();
  const s = setup();
  s.seed("board-archive.json", { jobs: [] });
  const put = s.bucket.put;
  s.bucket.put = async (key, value, options) => {
    // Another writer replaces the archive between its read and the pass write.
    if (key === "board-archive.json") await put(key, JSON.stringify({ jobs: [] }));
    return put(key, value, options);
  };
  assert.deepEqual(await s.run(), { status: "conflict", moved: [] });
  assert.deepEqual(
    warn.mock.calls.map((c) => c.arguments),
    [["archive pass conflict: board-archive.json changed during the pass; nothing moved"]],
  );
});

test("the archive API serves id, page and q, and rejects bad queries", async () => {
  const s = setup();
  await s.run();
  for (const principal of [person, device]) {
    const one = await s.send("/api/board/archive?id=412-fix-login", { principal });
    assert.equal(one.status, 200);
    assert.match(one.headers.get("Content-Type"), /^application\/json/);
    assert.equal(one.headers.get("Cache-Control"), "no-store");
    assert.deepEqual(await one.json(), { jobs: [ROWS[0]] });
  }
  assert.equal((await s.send("/api/board/archive?id=414-retry-sync")).status, 404);
  assert.equal(await (await s.send("/api/board/archive?id=999-none")).text(), "not found");
  assert.deepEqual(await (await s.send("/api/board/archive?page=1")).json(), {
    jobs: [ROWS[4], ROWS[0], ROWS[1]],
    page: 1,
    pages: 1,
    total: 3,
  });
  assert.deepEqual((await (await s.send("/api/board/archive?page=2")).json()).jobs, []);
  assert.deepEqual(await (await s.send("/api/board/archive?q=LOGIN")).json(), {
    jobs: [ROWS[0]],
    total: 1,
  });
  assert.equal((await (await s.send("/api/board/archive?q=sample")).json()).total, 3);
  for (const query of ["plan-9005", "pull/412", "#412", "PR #412", "pr#412", "token refresh"])
    assert.deepEqual(
      await (await s.send("/api/board/archive?q=" + encodeURIComponent(query))).json(),
      { jobs: [ROWS[0]], total: 1 },
    );
  assert.deepEqual(await (await s.send("/api/board/archive?q=%2341")).json(), {
    jobs: [],
    total: 0,
  });
  for (const [query, text] of [
    ["page=0", "page: must be a whole number from 1"],
    ["page=1.5", "page: must be a whole number from 1"],
    ["page=", "page: must be a whole number from 1"],
    ["q=", "q: must be 1 to 200 characters"],
    ["q=" + "x".repeat(201), "q: must be 1 to 200 characters"],
    ["id=412-fix-login&q=login", "use one of id, page or q"],
    ["page=1&q=login", "use one of id, page or q"],
  ]) {
    const response = await s.send("/api/board/archive?" + query);
    assert.equal(response.status, 400, query);
    assert.equal(await response.text(), text);
  }
  assert.equal((await s.send("/api/board/archive", { method: "PUT", body: "{}" })).status, 404);
  assert.equal((await s.send("/api/board/archive", { method: "POST", body: "{}" })).status, 404);
  // Many rows page by 50.
  const many = setup({ archive: { mode: "count", n: 1 } });
  const old = (i) => ({
    id: `${600 + i}-old`,
    project: "sample",
    stage: "merged",
    done: new Date(Date.UTC(2026, 0, 1, 0, i)).toISOString(),
  });
  many.seed("board.json", { jobs: Array.from({ length: 121 }, (_, i) => old(i)) });
  await many.run();
  const second = await (await many.send("/api/board/archive?page=3")).json();
  assert.deepEqual([second.page, second.pages, second.total, second.jobs.length], [3, 3, 120, 20]);
});

test("board writes cannot change archived or reuse an archived id; a fresh write passes", async () => {
  const s = setup();
  await s.run();
  const board = s.read("board.json");
  const etag = s.bucket.objects.get("board.json").httpEtag;
  const write = (data) =>
    s.send("/api/board", {
      method: "PUT",
      body: JSON.stringify(data),
      headers: { "If-Match": etag },
      principal: device,
    });
  const snapshot = () => s.bucket.objects.get("board.json").httpEtag;
  for (const [data, text] of [
    [
      { updated: board.updated, jobs: board.jobs },
      "a board write cannot change the archived field. Nothing was written. Upgrade the factory skill.",
    ],
    [
      { ...board, archived: { ...board.archived, max_id: 0 } },
      "a board write cannot change the archived field. Nothing was written. Upgrade the factory skill.",
    ],
    [
      { ...board, jobs: [...board.jobs, { id: "416-x", project: "sample" }] },
      'a board write cannot add "416-x": ids up to 416 belong to archived jobs. Nothing was written. Upgrade the factory skill.',
    ],
  ]) {
    const response = await write(data);
    assert.equal(response.status, 400);
    assert.equal(await response.text(), text);
    assert.equal(snapshot(), etag);
  }
  const fresh = await write({
    ...board,
    jobs: [...board.jobs, { id: "417-new", project: "sample" }],
  });
  assert.equal(fresh.status, 200);
  assert.deepEqual(s.read("board.json").archived, board.archived);
  // A stale etag skips the guards and gets the usual 412 for the device to re-read.
  const stale = await s.send("/api/board", {
    method: "PUT",
    body: JSON.stringify({ jobs: [{ id: "416-x" }] }),
    headers: { "If-Match": etag },
    principal: device,
  });
  assert.equal(stale.status, 412);
});

test("an empty jobs write keeps the archived field and its floor", async () => {
  const s = setup();
  await s.run();
  const board = s.read("board.json");
  const etag = s.bucket.objects.get("board.json").httpEtag;
  const write = (data) =>
    s.send("/api/board", {
      method: "PUT",
      body: JSON.stringify(data),
      headers: { "If-Match": etag },
      principal: device,
    });
  const dropped = await write({ updated: board.updated, jobs: [] });
  assert.equal(dropped.status, 400);
  assert.equal(
    await dropped.text(),
    "a board write cannot change the archived field. Nothing was written. Upgrade the factory skill.",
  );
  assert.equal(s.bucket.objects.get("board.json").httpEtag, etag);
  const kept = await write({ ...board, jobs: [] });
  assert.equal(kept.status, 200);
  assert.deepEqual(s.read("board.json").archived, board.archived);
  assert.equal(s.read("board.json").archived.max_id, 416);
});

test("the scheduled handler runs the pass through waitUntil", async () => {
  const s = setup();
  const pending = [];
  worker.scheduled({ scheduledTime: NOW.getTime() }, s.env, { waitUntil: (p) => pending.push(p) });
  assert.equal(pending.length, 2);
  assert.equal((await pending[0]).status, "archived");
});
