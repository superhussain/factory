// The board rows the room keeps in SQLite: the first read seeds from R2's board.json and keeps
// its ETag, a PATCH writes only its rows, the alarm copies the board to R2, a full write copies
// it at once, and a board.json written to R2 from outside is adopted, with unflushed rows
// merged over it. Rows are the made-up running example around 2001-add-csv-export.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { syntheticBoard } from "../../tools/board-cpu.mjs";
import { loadProduction, memoryBucket, memoryRoom } from "../support/worker.mjs";

const { serveFactory, BoardEvents, default: worker } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const SEED = '"board.json-seed"';

const BUILD = { id: "1999-synthetic-build", project: "sample", stage: "build" };
const SPEC = { id: "2000-synthetic-spec", project: "sample", stage: "spec" };
const IDEA = { id: "2001-add-csv-export", project: "sample", stage: "idea", title: "CSV" };
const BOARD = { updated: "2026-11-19T11:00:00Z", jobs: [BUILD, SPEC], autonomy: { mode: "auto" } };

function setup(board = BOARD, text = JSON.stringify(board)) {
  const bucket = memoryBucket();
  if (board)
    bucket.objects.set("board.json", {
      key: "board.json",
      bytes: Buffer.from(text),
      httpEtag: SEED,
    });
  const notified = [];
  const env = { BUCKET: bucket };
  const room = memoryRoom(
    BoardEvents,
    env,
    async (input, init) => {
      notified.push(await new Request(input, init).text());
      return new Response(null, { status: 204 });
    },
    2_000_000,
  );
  env.BoardEvents = room.namespace;
  const send = (path, init = {}) =>
    serveFactory(new Request("http://local" + path, init), env, person, { waitUntil() {} });
  const patch = (body) => send("/api/board", { method: "PATCH", body: JSON.stringify(body) });
  const r2 = () => bucket.objects.get("board.json");
  return { bucket, env, room, notified, send, patch, r2 };
}

const boardPuts = (bucket) =>
  bucket.calls.filter(([op, key]) => op === "put" && key === "board.json").length;

test("the first read seeds from R2 and keeps its ETag; GET is R2's bytes until a row changes", async () => {
  // A board a client wrote with indentation and a trailing newline reads back byte for byte.
  const s = setup(BOARD, JSON.stringify(BOARD, null, 2) + "\n");
  const full = await s.send("/board.json");
  assert.equal(full.headers.get("ETag"), SEED);
  assert.equal(await full.text(), JSON.stringify(BOARD, null, 2) + "\n");
  const api = await s.send("/api/board");
  assert.equal(api.headers.get("ETag"), SEED);
  assert.equal(api.headers.get("Accept-Patch"), "application/json");
  assert.deepEqual(await api.json(), BOARD);
  const slim = await s.send("/board.json?view=slim");
  assert.equal(slim.headers.get("ETag"), SEED);
  assert.deepEqual(
    (await slim.json()).jobs.map((j) => j.id),
    [BUILD.id, SPEC.id],
  );
  assert.equal(s.room.alarm(), null, "a seed leaves nothing to copy");
});

test("a PATCH puts no board.json; the alarm copies the board to R2", async () => {
  const s = setup();
  const ok = await s.patch({
    updated: "2026-11-19T12:00:00Z",
    rows: [{ id: IDEA.id, expect: null, row: IDEA }],
  });
  assert.equal(ok.status, 200);
  const etag = ok.headers.get("ETag");
  assert.notEqual(etag, SEED);
  const expected = {
    updated: "2026-11-19T12:00:00Z",
    jobs: [BUILD, SPEC, IDEA],
    autonomy: { mode: "auto" },
  };
  assert.equal(boardPuts(s.bucket), 0);
  assert.equal(s.r2().httpEtag, SEED);
  assert.deepEqual(JSON.parse(s.notified[0]), {
    type: "rows",
    etag,
    prev: SEED,
    updated: "2026-11-19T12:00:00Z",
    rows: [IDEA],
    removed: [],
  });
  assert.ok(s.room.alarm() > Date.now());
  await s.room.runAlarm();
  assert.equal(s.r2().bytes.toString(), JSON.stringify(expected));
  // The copy changes R2's version, not the board's.
  const read = await s.send("/board.json");
  assert.equal(read.headers.get("ETag"), etag);
  assert.equal(await read.text(), JSON.stringify(expected));
  assert.equal(s.room.object.board().hasDirty(), false);
});

test("the first read after a write copies what it built; later reads stream R2's bytes", async (t) => {
  const s = setup();
  s.env.BOARD_DEBUG = "1";
  const log = t.mock.method(console, "log", () => {});
  // Each read runs in a room evicted since the last request, as in production.
  const read = async () => {
    s.room.restart();
    const response = await s.send("/board.json");
    return { etag: response.headers.get("ETag"), text: await response.text() };
  };
  const ok = await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  const etag = ok.headers.get("ETag");
  const built = await read();
  assert.equal(built.etag, etag);
  await s.room.settle();
  assert.equal(boardPuts(s.bucket), 1, "one copy, put from the bytes the read built");
  assert.equal(s.r2().bytes.toString(), built.text);
  assert.equal(s.room.object.board().state().verbatim, true);
  assert.equal(s.room.object.board().hasDirty(), false);
  const copied = await read();
  assert.deepEqual(copied, built);
  await s.room.settle();
  await s.room.runAlarm();
  assert.equal(boardPuts(s.bucket), 1, "neither the second read nor the alarm copies again");
  assert.deepEqual(
    log.mock.calls.map((c) => c.arguments[0]),
    [`board store: read the pages for ${etag}`, `board store: read R2's bytes for ${etag}`],
  );
  // A commit after the copy moves the board off R2's bytes until the next copy.
  await s.patch({ rows: [{ id: IDEA.id, expect: IDEA, row: { ...IDEA, title: "CSV 2" } }] });
  assert.equal(s.room.object.board().state().verbatim, false);
  assert.equal(JSON.parse((await read()).text).jobs[2].title, "CSV 2");
});

test("boardRead decodes each version once while the room stays in memory", async (t) => {
  const s = setup();
  s.env.BOARD_DEBUG = "1";
  const log = t.mock.method(console, "log", () => {});
  const ok = await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  const first = await s.room.stub.boardRead();
  assert.deepEqual(await s.room.stub.boardRead(), first);
  assert.equal(first.etag, ok.headers.get("ETag"));
  assert.deepEqual(JSON.parse(first.text).jobs, [BUILD, SPEC, IDEA]);
  assert.equal(log.mock.calls.length, 1, "the second read takes the decoded text");
  await s.room.settle();
});

test("a read whose R2 get sees another read's copy answers with the version it serves", async () => {
  const s = setup();
  await s.send("/board.json");
  // An evicted room has no memo, so its next read of the seeded board gets R2's bytes.
  s.room.restart();
  const get = s.bucket.get.bind(s.bucket);
  let release, entered;
  const gate = new Promise((resolve) => (release = resolve));
  const waiting = new Promise((resolve) => (entered = resolve));
  s.bucket.get = async (...args) => {
    s.bucket.get = get;
    entered();
    await gate;
    return get(...args);
  };
  const first = s.send("/board.json");
  await waiting;
  // A PATCH lands and a second read builds the new version and copies it, all during that get.
  const ok = await s.patch({
    updated: "2026-11-19T12:00:00Z",
    rows: [{ id: IDEA.id, expect: null, row: IDEA }],
  });
  const etag = ok.headers.get("ETag");
  const expected = { ...BOARD, updated: "2026-11-19T12:00:00Z", jobs: [BUILD, SPEC, IDEA] };
  assert.deepEqual(await (await s.send("/board.json")).json(), expected);
  await s.room.settle();
  assert.deepEqual(JSON.parse(s.r2().bytes), expected);
  release();
  const response = await first;
  assert.equal(response.headers.get("ETag"), etag);
  assert.deepEqual(await response.json(), expected);
});

test("a write during a read's copy leaves the board off R2's bytes until the next copy", async () => {
  const s = setup();
  const ok = await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  const put = s.bucket.put.bind(s.bucket);
  const renamed = { ...IDEA, title: "CSV 2" };
  s.bucket.put = async (...args) => {
    s.bucket.put = put;
    const again = await s.patch({ rows: [{ id: IDEA.id, expect: IDEA, row: renamed }] });
    assert.equal(again.status, 200);
    return put(...args);
  };
  const first = await s.send("/board.json");
  assert.equal(first.headers.get("ETag"), ok.headers.get("ETag"));
  await s.room.settle();
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [BUILD, SPEC, IDEA]);
  const board = s.room.object.board();
  assert.equal(board.state().verbatim, false);
  assert.equal(board.hasDirty(), true);
  assert.deepEqual((await (await s.send("/board.json")).json()).jobs, [BUILD, SPEC, renamed]);
  await s.room.settle();
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [BUILD, SPEC, renamed]);
  assert.equal(board.state().verbatim, true);
});

test("a PATCH during a copy stays unflushed until the next alarm", async () => {
  const s = setup();
  await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  const put = s.bucket.put.bind(s.bucket);
  let release;
  const gate = new Promise((resolve) => (release = resolve));
  s.bucket.put = async (...args) => (await gate, put(...args));
  const copying = s.room.runAlarm();
  const done = { ...BUILD, stage: "review" };
  assert.equal((await s.patch({ rows: [{ id: BUILD.id, expect: BUILD, row: done }] })).status, 200);
  release();
  await copying;
  s.bucket.put = put;
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [BUILD, SPEC, IDEA]);
  assert.equal(s.room.object.board().hasDirty(), true);
  await s.room.runAlarm();
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [done, SPEC, IDEA]);
  assert.equal(s.room.object.board().hasDirty(), false);
});

test("the version moving under a patch retries it on the new version", async () => {
  const s = setup();
  await s.send("/board.json");
  const commit = s.room.stub.boardCommit;
  let races = 1;
  s.room.stub.boardCommit = async (c) => {
    // A pickup host commits first.
    if (races-- > 0)
      await commit({
        base: c.base,
        changes: [{ id: BUILD.id, row: { ...BUILD, note: "host" }, add: false }],
        max: 1 << 24,
      });
    return commit(c);
  };
  const response = await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  assert.equal(response.status, 200);
  assert.deepEqual((await s.room.stored()).jobs, [{ ...BUILD, note: "host" }, SPEC, IDEA]);
});

test("an archive pass that loses to a PATCH moves nothing", async (t) => {
  const warn = t.mock.method(console, "warn", () => {});
  const old = { ...BUILD, stage: "merged", done: "2026-08-01T12:00:00Z" };
  const s = setup({ updated: "2026-11-09T09:00:00Z", jobs: [old, SPEC] });
  s.bucket.objects.set("pipeline/global.json", {
    key: "pipeline/global.json",
    bytes: Buffer.from(JSON.stringify({ schema: 1, archive: { mode: "age", n: 30 } })),
    httpEtag: '"p"',
  });
  const run = () => {
    let pass;
    worker.scheduled({ scheduledTime: Date.parse("2026-11-09T10:17:00Z") }, s.env, {
      waitUntil: (p) => (pass ??= p),
    });
    return pass;
  };
  const replace = s.room.stub.boardReplace;
  s.room.stub.boardReplace = async (...args) => {
    await s.patch({ rows: [{ id: SPEC.id, expect: SPEC, row: { ...SPEC, note: "device" } }] });
    return replace(...args);
  };
  assert.deepEqual(await run(), { status: "conflict", moved: [] });
  assert.equal(warn.mock.calls.length, 1);
  assert.deepEqual(
    (await s.room.stored()).jobs.map((j) => j.id),
    [old.id, SPEC.id],
  );
  s.room.stub.boardReplace = replace;
  assert.deepEqual(await run(), { status: "archived", moved: [old.id] });
  // A full write reaches R2 before it answers.
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [{ ...SPEC, note: "device" }]);
  assert.equal(JSON.parse(s.r2().bytes).archived.max_id, 1999);
  assert.deepEqual(await s.room.stored(), JSON.parse(s.r2().bytes));
});

test("a PUT copies to R2 before it answers and refuses a row over 1 MiB", async () => {
  const s = setup();
  const put = (jobs, etag) =>
    s.send("/api/board", {
      method: "PUT",
      headers: { "If-Match": etag },
      body: JSON.stringify({ ...BOARD, jobs }),
    });
  const big = await put([{ ...BUILD, idea: "x".repeat(1 << 20) }], SEED);
  assert.equal(big.status, 413);
  assert.equal(await big.text(), "a board row over 1 MiB cannot be stored. Nothing was written.");
  const ok = await put([BUILD], SEED);
  assert.equal(await ok.text(), "ok");
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [BUILD]);
  assert.equal(s.room.alarm(), null);
  assert.equal((await put([SPEC], SEED)).status, 412, "the seed's ETag is stale now");
  assert.equal((await put([BUILD, SPEC], ok.headers.get("ETag"))).status, 200);
});

test("a PUT reads back byte for byte, even with a top-level field past SQLite's value limit", async () => {
  const s = setup();
  // 8 MiB in one top-level field, as the cancel drill writes, and a trailing newline.
  const body = JSON.stringify({ ...BOARD, drillPad: Array(8192).fill("x".repeat(1024)) }) + "\n";
  const ok = await s.send("/api/board", { method: "PUT", headers: { "If-Match": SEED }, body });
  assert.equal(ok.status, 200);
  assert.equal(await (await s.send("/board.json")).text(), body);
  // A row change rebuilds the text from the rows, still with the large field.
  const patched = await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  assert.equal(patched.status, 200);
  const after = await (await s.send("/board.json")).json();
  assert.equal(after.drillPad.length, 8192);
  assert.deepEqual(
    after.jobs.map((j) => j.id),
    [...BOARD.jobs.map((j) => j.id), IDEA.id],
  );
});

test("a new row may name an upload that another stored row carries", async () => {
  const image = `${"a".repeat(64)}.png`;
  const board = { ...BOARD, jobs: [{ ...BUILD, idea_images: [image] }, SPEC] };
  const s = setup(board);
  // IDEA is new and BUILD is not fetched, so only the index finds the stored name; R2 has no
  // upload object, so a miss would refuse the write.
  const added = await s.patch({
    rows: [{ id: IDEA.id, expect: null, row: { ...IDEA, idea_images: [image] } }],
  });
  assert.equal(added.status, 200, await added.text());
});

test("a board.json written to R2 from outside is adopted with its ETag", async () => {
  const s = setup();
  await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  await s.room.runAlarm();
  const restored = { ...BOARD, jobs: [SPEC] };
  await s.bucket.put("board.json", JSON.stringify(restored));
  const read = await s.send("/board.json");
  assert.equal(read.headers.get("ETag"), s.r2().httpEtag);
  assert.deepEqual(await read.json(), restored);
});

test("unflushed rows merge over an outside write row by row; a row both changed keeps R2's", async (t) => {
  const warn = t.mock.method(console, "warn", () => {});
  const s = setup();
  const mine = { ...BUILD, note: "room" };
  await s.patch({
    updated: "2026-11-19T12:00:00Z",
    rows: [
      { id: BUILD.id, expect: BUILD, row: mine },
      { id: IDEA.id, expect: null, row: IDEA },
    ],
  });
  // A Worker from before the room writes R2 directly: SPEC moves on and BUILD is changed too.
  const theirs = {
    updated: "2026-11-19T12:30:00Z",
    jobs: [
      { ...BUILD, note: "old worker" },
      { ...SPEC, stage: "build" },
    ],
    autonomy: { mode: "auto" },
  };
  await s.bucket.put("board.json", JSON.stringify(theirs));
  const merged = await (await s.send("/board.json")).json();
  assert.deepEqual(merged, { ...theirs, jobs: [...theirs.jobs, IDEA] });
  assert.match(warn.mock.calls[0].arguments[0], /R2 kept "?1999-synthetic-build/);
  await s.room.runAlarm();
  assert.deepEqual(JSON.parse(s.r2().bytes), merged);
  assert.equal(s.room.object.board().hasDirty(), false);
});

test("an unreadable board.json is served as stored and refuses a PATCH", async () => {
  const s = setup(BOARD, "{not json");
  assert.equal(await (await s.send("/board.json")).text(), "{not json");
  const refused = await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  assert.equal(refused.status, 400);
});

// The PATCH budget at today's board and twice it, pinned as work units so it never flakes: the
// SQL rows the room reads, the bytes crossing RPC, and the Worker's JSON calls stay the same as
// the board doubles, and the PATCH neither reads nor writes board.json.
test("a one-row PATCH costs the same work at 2,300 and 4,600 rows", async () => {
  const cost = async (rows) => {
    const board = syntheticBoard(rows);
    // The board's first idea sits at another row at each size, so it gets one size at both.
    const active = board.jobs.find((j) => j.stage === "idea");
    active.idea = "x".repeat(1000);
    const s = setup(board);
    // The first PATCH seeds; the second is the one measured.
    const once = (stage) =>
      s.patch({
        rows: [
          {
            id: active.id,
            expect: { ...active, stage: stage[0] },
            row: { ...active, stage: stage[1] },
          },
        ],
      });
    assert.equal((await once(["idea", "spec"])).status, 200);
    const before = { ...s.room.work, calls: s.bucket.calls.length };
    const { parse, stringify } = JSON;
    const json = { parse: 0, stringify: 0 };
    JSON.parse = (...a) => (json.parse++, parse(...a));
    JSON.stringify = (...a) => (json.stringify++, stringify(...a));
    let response;
    try {
      response = await once(["spec", "build"]);
    } finally {
      JSON.parse = parse;
      JSON.stringify = stringify;
    }
    assert.equal(response.status, 200);
    const touched = s.bucket.calls
      .slice(before.calls)
      .filter(([op, key]) => key === "board.json" && op !== "head");
    assert.deepEqual(touched, [], "no board.json get or put");
    return {
      sqlRows: s.room.work.sqlRows - before.sqlRows,
      rpcBytes: s.room.work.rpcBytes - before.rpcBytes,
      json,
    };
  };
  const today = await cost(2300);
  const doubled = await cost(4600);
  assert.equal(doubled.sqlRows, today.sqlRows);
  assert.ok(
    Math.abs(doubled.rpcBytes - today.rpcBytes) < 200,
    `${today.rpcBytes} ${doubled.rpcBytes}`,
  );
  assert.deepEqual(doubled.json, today.json);
  assert.ok(doubled.sqlRows < 20, `SQL rows ${doubled.sqlRows}`);
  assert.ok(doubled.rpcBytes < 20000, `RPC bytes ${doubled.rpcBytes}`);
  assert.ok(doubled.json.stringify < 60, `JSON.stringify calls ${doubled.json.stringify}`);
});

test("a copy that lands before the room records it is the room's own after a restart", async (t) => {
  const warn = t.mock.method(console, "warn", () => {});
  const s = setup();
  const first = { ...BUILD, note: "first" };
  const second = { ...BUILD, note: "second" };
  await s.patch({ rows: [{ id: BUILD.id, expect: BUILD, row: first }] });
  const put = s.bucket.put.bind(s.bucket);
  s.bucket.put = async (key, value, options) => {
    // The row changes again while the copy is under way, and the room stops after R2 takes the
    // copy but before it records so.
    s.bucket.put = put;
    const again = await s.patch({ rows: [{ id: BUILD.id, expect: first, row: second }] });
    assert.equal(again.status, 200);
    await put(key, value, options);
    throw new Error("the room stopped");
  };
  await assert.rejects(s.room.runAlarm(), /the room stopped/);
  s.room.restart();
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [first, SPEC]);
  assert.deepEqual((await (await s.send("/board.json")).json()).jobs, [second, SPEC]);
  assert.equal(warn.mock.calls.length, 0, "the room's own copy is no outside write to merge");
  // That read built the board, so it copies it.
  await s.room.settle();
  assert.deepEqual(JSON.parse(s.r2().bytes).jobs, [second, SPEC]);
  assert.equal(s.room.object.board().hasDirty(), false);
});

test("a full write loses to an outside write that lands in R2 before its copy", async (t) => {
  t.mock.method(console, "warn", () => {});
  const s = setup();
  await s.send("/board.json");
  const outside = { ...BOARD, jobs: [BUILD, SPEC, IDEA] };
  const put = s.bucket.put.bind(s.bucket);
  s.bucket.put = async (key, value, options) => {
    // A Worker from before the room writes R2 between the room's check and its copy.
    s.bucket.put = put;
    await put("board.json", JSON.stringify(outside));
    return put(key, value, options);
  };
  const response = await s.send("/api/board", {
    method: "PUT",
    headers: { "If-Match": SEED },
    body: JSON.stringify({ ...BOARD, jobs: [BUILD] }),
  });
  assert.equal(response.status, 412);
  assert.deepEqual(await (await s.send("/board.json")).json(), outside);
  assert.deepEqual(JSON.parse(s.r2().bytes), outside);
});

// The slim body before the room built it: the Worker's own slimBoard over a board in R2 alone.
// Each text gets its own ETag, since the Worker keeps its last slim body by ETag.
async function workerSlim(text) {
  const bucket = memoryBucket();
  const httpEtag = '"' + createHash("sha256").update(text).digest("hex") + '"';
  bucket.objects.set("board.json", { key: "board.json", bytes: Buffer.from(text), httpEtag });
  const request = new Request("http://local/board.json?view=slim");
  return (await serveFactory(request, { BUCKET: bucket }, person)).text();
}

// Made-up boards of random shape: emoji, U+2028, duplicate, missing and object ids, rows that
// are not objects, projects named __proto__ and 123, unparseable done stamps, a top-level slim
// key, and rows big enough to fill several pages. Every write is a room commit, so rows the
// Worker would refuse are written too. Some updates grow a row to 100 to 300 KB, so pages pass
// MAX_PAGE and split; after every write each index row names its row's page, the state's last
// page is at least the highest, and ids() is the board's ids.
test("the room's slim body is byte for byte the Worker's slimBoard, across random boards and writes", async () => {
  let seed = 2284;
  const rand = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
  const pick = (list) => list[Math.floor(rand() * list.length)];
  const texts = ["plain", "emoji 🎉👩‍💻", "line sep end", 'quote " and \\', "é 中文", ""];
  const stamp = () =>
    new Date(Date.UTC(2026, 0, 1) + Math.floor(rand() * 400) * 3600000).toISOString();
  let n = 0;
  const row = (id = `${1000 + ++n}-random`) => {
    if (rand() < 0.03) return pick([null, "a string row", 7, [1, 2]]);
    const out = {
      id: rand() < 0.05 ? pick([null, 12, { nested: 1 }, undefined]) : id,
      stage: pick(["idea", "build", "merged", "merged", "merged", "closed", "Merged", undefined]),
      project: pick(["factory", "sample", "__proto__", "123", "", 7, undefined]),
      title: pick(texts),
      updated: rand() < 0.9 ? stamp() : undefined,
      done: pick([stamp(), stamp(), undefined, "not a date", 0]),
    };
    if (rand() < 0.04) out.idea = "x".repeat(60_000 + Math.floor(rand() * 90_000));
    return out;
  };
  const check = async (s, what) => {
    const full = await (await s.send("/board.json")).text();
    const slim = await s.send("/board.json?view=slim");
    assert.equal(slim.headers.get("ETag"), (await s.send("/board.json")).headers.get("ETag"));
    assert.ok(slim.headers.get("X-Factory-Version"));
    assert.equal(await slim.text(), await workerSlim(full), what);
    const store = s.room.object.board();
    const sql = (query, ...b) => store.sql.exec(query, ...b).toArray();
    // The index stores a non-string id as NULL.
    const key = (id) => (typeof id === "string" ? id : "\0null");
    const pages = sql("SELECT page, json FROM board_pages ORDER BY page");
    for (const p of pages) {
      assert.ok(Buffer.byteLength(p.json) <= 3 << 19, `${what} page ${p.page} size`);
      assert.deepEqual(
        sql("SELECT id FROM board_index WHERE page = ?", p.page)
          .map((r) => key(r.id))
          .sort(),
        JSON.parse(p.json)
          .map((j) => key(j?.id))
          .sort(),
        `${what} index of page ${p.page}`,
      );
    }
    if (pages.length) assert.ok(store.state().lastPage >= pages.at(-1).page, `${what} lastPage`);
    assert.deepEqual(
      new Set(await store.ids()),
      new Set(JSON.parse(full).jobs?.map?.((j) => j?.id) ?? []),
      `${what} ids`,
    );
  };
  let splits = 0;
  for (let b = 0; b < 40; b++) {
    const jobs = Array.from({ length: Math.floor(rand() * 70) }, () => row());
    // Some rows repeat an earlier id, as an unclean board can.
    for (const j of jobs)
      if (rand() < 0.05 && j?.constructor === Object && jobs[0]?.id) j.id = jobs[0].id;
    const board = { updated: stamp(), jobs, autonomy: { mode: "auto" } };
    if (rand() < 0.2) board.slim = { stale: true };
    if (rand() < 0.2) board["1"] = "an integer key";
    if (rand() < 0.2) board.note = pick(texts);
    const s = setup(board, JSON.stringify(board, null, rand() < 0.5 ? 2 : 0));
    await check(s, `board ${b}`);
    for (let w = 0; w < 8; w++) {
      const store = s.room.object.board();
      const split = store.split;
      store.split = (...args) => (splits++, split.apply(store, args));
      const ids = jobs.map((j) => j?.id).filter((id) => typeof id === "string");
      const changes = [];
      for (let k = Math.floor(rand() * 3); ids.length && k >= 0; k--) {
        const id = pick(ids);
        if (changes.some((c) => c.id === id)) continue;
        const next = rand() < 0.2 ? null : { ...row(id), id };
        if (next?.constructor === Object && rand() < 0.5)
          next.idea = "é".repeat(50_000 + Math.floor(rand() * 100_000));
        changes.push({ id, row: next, add: false });
      }
      if (rand() < 0.6) {
        const added = row();
        changes.push({ id: added?.id ?? `${++n}-x`, row: added, add: true });
      }
      if (!changes.length) continue;
      // Half the pages lose their meta, as before the column, so the commit's update, delete and
      // add each meet a page with none; the slim read after fills it in.
      store.sql.exec("UPDATE board_pages SET meta = NULL WHERE page % 2 = ?", w % 2);
      const out = await store.commit({ base: store.state().etag, changes, updated: stamp() });
      assert.ok(out.etag, JSON.stringify(out));
      await check(s, `board ${b} write ${w}`);
    }
    // A room from before the meta column fills it in on its first slim read.
    s.room.object.board().sql.exec("UPDATE board_pages SET meta = NULL");
    s.room.restart();
    await check(s, `board ${b} without meta`);
    const left = s.room.object.board().sql.exec("SELECT page FROM board_pages WHERE meta IS NULL");
    assert.equal(left.toArray().length, 0);
  }
  assert.ok(splits >= 20, `${splits} splits`);
});

// Ids 12 and "12", or null and "null", are different ids to slimBoard's per-stage count, and
// two rows without an id are the same one. Two {} ids are two ids, as two objects are.
const LOOKALIKE_IDS = JSON.stringify({
  jobs: [12, "12", null, "null", undefined, undefined, "undefined", {}, {}].map((id) => ({
    id,
    stage: "merged",
    project: "sample",
  })),
});

test("a slim read of an empty, jobless, unreadable or lookalike-id board is what the Worker sent", async () => {
  const texts = [
    '{"jobs":[]}',
    '{"updated":"2026-11-24T00:00:00Z"}',
    '{"jobs":"none"}',
    "{not json",
  ];
  for (const text of [...texts, LOOKALIKE_IDS]) {
    const s = setup(null);
    s.bucket.objects.set("board.json", {
      key: "board.json",
      bytes: Buffer.from(text),
      httpEtag: SEED,
    });
    const slim = await s.send("/board.json?view=slim");
    assert.equal(slim.status, 200);
    assert.equal(await slim.text(), await workerSlim(text), text);
  }
  assert.equal((await setup(null).send("/board.json?view=slim")).status, 404);
});

// The Worker's step of a slim read is one fetch of the room's bytes, passed through: no RPC, so
// no board text crosses to the Worker, and the same few JSON calls at 2,300 and 4,600 rows.
test("a slim read is one room fetch the Worker passes through, at 2,300 and 4,600 rows", async () => {
  const cost = async (rows) => {
    const s = setup(syntheticBoard(rows));
    await s.send("/board.json");
    const out = {};
    for (const what of ["miss", "hit"]) {
      const before = { calls: s.room.calls.length, rpcBytes: s.room.work.rpcBytes };
      const { parse } = JSON;
      let parses = 0;
      JSON.parse = (...a) => (parses++, parse(...a));
      try {
        const response = await s.send("/board.json?view=slim");
        assert.equal(response.status, 200);
        await response.arrayBuffer();
      } finally {
        JSON.parse = parse;
      }
      assert.deepEqual(s.room.calls.slice(before.calls), [], "no RPC");
      assert.equal(s.room.work.rpcBytes, before.rpcBytes);
      out[what] = parses;
    }
    return out;
  };
  const [today, doubled] = [await cost(2300), await cost(4600)];
  // A hit parses only the room's state; a miss adds one meta parse per page.
  assert.equal(doubled.hit, today.hit);
  assert.ok(doubled.hit <= 3, JSON.stringify(doubled));
  assert.ok(doubled.miss <= 60, JSON.stringify(doubled));
});

// The live board keeps its unfinished rows and its most recently finished ones in its last few
// pages, so a slim miss takes the bytes of those pages, not every page's.
test("a slim miss takes the bytes of only the pages holding a row it keeps", async () => {
  const board = syntheticBoard(2300);
  for (const [n, j] of board.jobs.entries())
    if (n >= 2240) {
      j.stage = "build";
      delete j.done;
    } else {
      if (j.stage !== "closed") j.stage = "merged";
      j.done = new Date(Date.UTC(2026, 0, 1) + n * 3600000).toISOString();
    }
  const s = setup(board);
  const full = await (await s.send("/board.json")).text();
  const store = s.room.object.board();
  const exec = store.sql.exec;
  let taken = 0;
  store.sql.exec = (...args) => {
    const out = exec(...args);
    for (const row of out.toArray()) taken += row.json?.length ?? 0;
    return out;
  };
  assert.equal(await (await s.send("/board.json?view=slim")).text(), await workerSlim(full));
  assert.ok(taken > 0 && taken < full.length / 4, `${taken} of ${full.length} bytes`);
});

test("a pages table from before meta, or with meta after json, is rebuilt meta first", async () => {
  for (const columns of ["page, json", "page, json, meta"]) {
    const s = setup();
    await s.send("/board.json");
    const sql = (query) => s.room.object.board().sql.exec(query).toArray();
    const types = columns.replace("page", "page INTEGER PRIMARY KEY").replace("json", "json TEXT");
    sql(`CREATE TABLE old_pages (${types.replace("meta", "meta TEXT")})`);
    sql(`INSERT INTO old_pages SELECT ${columns} FROM board_pages`);
    sql("DROP TABLE board_pages");
    sql("ALTER TABLE old_pages RENAME TO board_pages");
    s.room.restart();
    const full = await (await s.send("/board.json")).text();
    assert.equal(await (await s.send("/board.json?view=slim")).text(), await workerSlim(full));
    const order = () => sql("PRAGMA table_info(board_pages)").map((c) => c.name);
    assert.deepEqual(order(), ["page", "meta", "json"], columns);
    // The next room over the same table finds it in order and leaves it be.
    s.room.restart();
    assert.equal((await s.send("/board.json?view=slim")).status, 200);
    assert.deepEqual(order(), ["page", "meta", "json"]);
  }
});

// Rows a PATCH grows in place grow their page. Past MAX_PAGE (1.5 MiB) that page alone is packed
// anew into pages, and the pages after it move up, so none nears SQLite's 2 MB row limit, which
// setup's room enforces as workerd does.
test("a PATCH that grows a page past its limit splits it, and the board reads back the same", async () => {
  const jobs = Array.from({ length: 6 }, (_, n) => ({
    id: `${3000 + n}-grows`,
    project: "sample",
    stage: "build",
  }));
  // Rows of 20 KB after them fill two more pages, which the split moves.
  for (let n = 0; n < 30; n++)
    jobs.push({
      id: `${3100 + n}-after`,
      project: "sample",
      stage: "merged",
      done: "2026-11-01T00:00:00Z",
      idea: "x".repeat(20_000),
    });
  const s = setup({ ...BOARD, jobs });
  await s.send("/board.json");
  const store = () => s.room.object.board();
  const sql = (query) => store().sql.exec(query).toArray();
  const sizes = () =>
    sql("SELECT length(CAST(json AS BLOB)) AS n FROM board_pages ORDER BY page").map((p) => p.n);
  // Pages are numbered from 0 with no gap, each index row names the page its row is on, and the
  // state's last page is the highest.
  const indexed = () => {
    const pages = sql("SELECT page, json FROM board_pages ORDER BY page");
    assert.deepEqual(
      pages.map((p) => p.page),
      pages.map((_, i) => i),
    );
    for (const p of pages)
      assert.deepEqual(
        sql(`SELECT id FROM board_index WHERE page = ${p.page} ORDER BY id`).map((r) => r.id),
        JSON.parse(p.json)
          .map((j) => j.id)
          .sort(),
        `page ${p.page}`,
      );
    assert.equal(store().state().lastPage, pages.length - 1);
  };
  const check = async () => {
    const full = await (await s.send("/board.json")).text();
    assert.deepEqual(JSON.parse(full).jobs, jobs);
    assert.equal(await (await s.send("/board.json?view=slim")).text(), await workerSlim(full));
    assert.ok(Math.max(...sizes()) <= 3 << 19, JSON.stringify(sizes()));
    indexed();
  };
  assert.equal(sizes().length, 3);
  const pageOf = (id) => sql(`SELECT page FROM board_index WHERE id = '${id}'`)[0].page;
  const last = jobs.at(-1).id;
  assert.equal(pageOf(last), 2);
  for (const [n, row] of jobs.slice(0, 6).entries()) {
    // 450,000 bytes in UTF-8, so four such rows pass MAX_PAGE and five pass 2 MB.
    jobs[n] = { ...row, idea: "é".repeat(225_000) };
    const ok = await s.patch({ rows: [{ id: row.id, expect: row, row: jobs[n] }] });
    assert.equal(ok.status, 200, await ok.text());
    await check();
  }
  assert.ok(sizes().length > 3 && pageOf(last) > 2, JSON.stringify(sizes()));
  // A row on a moved page, and a new row, write through the index and the last page.
  const moved = jobs.at(-1);
  jobs[jobs.length - 1] = { ...moved, title: "Moved" };
  const added = { id: "3200-added", project: "sample", stage: "idea" };
  jobs.push(added);
  const ok = await s.patch({
    rows: [
      { id: moved.id, expect: moved, row: jobs.at(-2) },
      { id: added.id, expect: null, row: added },
    ],
  });
  assert.equal(ok.status, 200, await ok.text());
  await check();
});

// GET /api/board/archive as it answered before the room kept the board's ids: the whole board
// and the whole archive parsed on every request.
function archiveBefore(boardText, archiveText, query) {
  const parse = (text) => {
    try {
      return JSON.parse(text);
    } catch {
      return null;
    }
  };
  const jobsOf = (doc) => (Array.isArray(doc?.jobs) ? doc.jobs : null);
  const boardIds = new Set((jobsOf(parse(boardText)) ?? []).map((j) => j?.id));
  const jobs = (jobsOf(parse(archiveText)) ?? []).filter((j) => !boardIds.has(j?.id));
  const params = new URLSearchParams(query);
  if (params.has("id")) {
    const row = jobs.find((j) => j?.id === params.get("id"));
    return row ? [200, JSON.stringify({ jobs: [row] })] : [404, "not found"];
  }
  const q = params.get("q");
  if (q !== null) {
    const fields = ["id", "title", "project", "jira", "pr", "idea", "goal"];
    const pr = /^(?:pr\s*)?#(\d+)$/i.exec(q)?.[1];
    const matches = jobs.filter(
      (j) =>
        fields.some(
          (k) => typeof j?.[k] === "string" && j[k].toLowerCase().includes(q.toLowerCase()),
        ) ||
        (pr !== undefined && typeof j?.pr === "string" && j.pr.endsWith(`/pull/${pr}`)),
    );
    return [200, JSON.stringify({ jobs: matches.slice(0, 50), total: matches.length })];
  }
  const n = Number(params.get("page") ?? "1");
  return [
    200,
    JSON.stringify({
      jobs: jobs.slice((n - 1) * 50, n * 50),
      page: n,
      pages: Math.ceil(jobs.length / 50),
      total: jobs.length,
    }),
  ];
}

// Made-up boards and archives that share ids, with duplicate, missing, null, number and object
// ids and rows that are not objects. The room's id list and the isolate's parsed archive must
// answer every query the dashboard and the CLI send byte for byte as before, through room
// commits and archive rewrites, and so must a Worker with R2 alone.
test("the archive read answers byte for byte as before, across random boards and archives", async () => {
  let seed = 2323;
  const rand = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
  const pick = (list) => list[Math.floor(rand() * list.length)];
  const shared = Array.from({ length: 40 }, (_, i) => `${700 + i}-shared`);
  const row = () => {
    if (rand() < 0.03) return pick([null, "a string row", 7]);
    const out = {
      id: rand() < 0.08 ? pick([null, 12, { nested: 1 }, undefined]) : pick(shared),
      stage: pick(["merged", "closed", "build"]),
      project: pick(["sample", "demo", "Émoji 🎉"]),
      title: pick(["Fix login", "ADD export", "é 中文", 'quote " and \\']),
      done: "2026-10-01T12:00:00Z",
    };
    if (rand() < 0.5) out.pr = `https://github.example.test/sample/pull/${Math.floor(rand() * 30)}`;
    return out;
  };
  const queries = (archive) => [
    ...archive.jobs
      .map((j) => j?.id)
      .filter((id) => typeof id === "string")
      .map((id) => `id=${encodeURIComponent(id)}`),
    "id=999-none",
    ...["1", "2", "3"].map((p) => `page=${p}`),
    ...["sample", "SHARED", "#12", "PR #7", "é", "🎉", '"', "x"].map(
      (q) => `q=${encodeURIComponent(q)}`,
    ),
  ];
  for (let b = 0; b < 25; b++) {
    const board = {
      updated: "2026-11-24T00:00:00Z",
      jobs: Array.from({ length: Math.floor(rand() * 30) }, row),
    };
    const archive = { jobs: Array.from({ length: Math.floor(rand() * 120) }, row) };
    const s = setup(board);
    s.bucket.objects.set("board-archive.json", {
      key: "board-archive.json",
      bytes: Buffer.from(JSON.stringify(archive)),
      httpEtag: `"archive-${b}"`,
    });
    const check = async (what) => {
      const boardText = await (await s.send("/board.json")).text();
      const archiveText = s.bucket.objects.get("board-archive.json").bytes.toString();
      for (const query of queries(JSON.parse(archiveText))) {
        const want = archiveBefore(boardText, archiveText, query);
        const request = () => new Request("http://local/api/board/archive?" + query);
        for (const env of [s.env, { BUCKET: s.bucket }]) {
          const got = await serveFactory(request(), env, person);
          assert.deepEqual([got.status, await got.text()], want, `${what} ${query}`);
        }
      }
    };
    await check(`board ${b}`);
    const store = s.room.object.board();
    const ids = board.jobs.map((j) => j?.id).filter((id) => typeof id === "string");
    const changes = [{ id: pick(shared), row: null, add: false }];
    if (ids.length) changes.push({ id: pick(ids), row: null, add: false });
    const added = row();
    changes.push({ id: added?.id ?? "1-x", row: added, add: true });
    const out = await store.commit({
      base: store.state().etag,
      changes,
      updated: "2026-11-24T01:00:00Z",
    });
    assert.ok(out.etag, JSON.stringify(out));
    await check(`board ${b} after a commit`);
    archive.jobs.push(row());
    await s.bucket.put("board-archive.json", JSON.stringify(archive));
    await check(`board ${b} after an archive write`);
  }
});

// The isolate parses each archive version once, whichever BUCKET object a request brings, and
// holds no archive past 8 MiB, which it parses on every read instead.
test("the archive read parses each archive version once, and one past 8 MiB on every read", async () => {
  const s = setup(BOARD);
  const archive = (jobs, httpEtag) =>
    s.bucket.objects.set("board-archive.json", {
      key: "board-archive.json",
      bytes: Buffer.from(JSON.stringify({ jobs })),
      httpEtag,
    });
  const gets = () =>
    s.bucket.calls.filter(([op, key]) => op === "get" && key === "board-archive.json").length;
  // Each request's BUCKET is a new object over the same store.
  const read = async () => {
    const bucket = { head: (k) => s.bucket.head(k), get: (k, o) => s.bucket.get(k, o) };
    const response = await serveFactory(
      new Request("http://local/api/board/archive?page=1"),
      { BUCKET: bucket },
      person,
    );
    return response.text();
  };
  const small = { ...IDEA, id: "2002-archived-small", stage: "merged" };
  archive([small], '"archive-small"');
  const first = gets();
  assert.equal(await read(), JSON.stringify({ jobs: [small], page: 1, pages: 1, total: 1 }));
  assert.equal(await read(), JSON.stringify({ jobs: [small], page: 1, pages: 1, total: 1 }));
  assert.equal(gets() - first, 1);
  const big = { ...small, id: "2003-archived-big", idea: "x".repeat(9 << 20) };
  archive([big], '"archive-big"');
  const before = gets();
  const want = JSON.stringify({ jobs: [big], page: 1, pages: 1, total: 1 });
  assert.equal(await read(), want);
  assert.equal(await read(), want);
  assert.equal(gets() - before, 2);
  // The big archive lands between a small one's HEAD and its GET: the HEAD's size is not the
  // rows', so the big rows are not held, and the next read of them parses again.
  archive([small], '"archive-small-2"');
  const head = s.bucket.head;
  s.bucket.head = async (key) => {
    const out = await head(key);
    archive([big], '"archive-big"');
    s.bucket.head = head;
    return out;
  };
  const raced = gets();
  assert.equal(await read(), want);
  assert.equal(await read(), want);
  assert.equal(gets() - raced, 2);
});
