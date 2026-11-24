// The board rows the room keeps in SQLite: the first read seeds from R2's board.json and keeps
// its ETag, a PATCH writes only its rows, the alarm copies the board to R2, a full write copies
// it at once, and a board.json written to R2 from outside is adopted, with unflushed rows
// merged over it. Rows are the made-up running example around 2001-add-csv-export.
import assert from "node:assert/strict";
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
  assert.equal(await (await s.send("/board.json")).text(), JSON.stringify(expected));
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
  assert.equal((await s.send("/board.json")).headers.get("ETag"), etag);
  assert.equal(s.room.object.board().hasDirty(), false);
});

test("after a copy lands, a read in a room evicted since streams R2's bytes", async (t) => {
  const s = setup();
  s.env.BOARD_DEBUG = "1";
  const log = t.mock.method(console, "log", () => {});
  const read = async () => {
    s.room.restart();
    const response = await s.send("/board.json");
    return { etag: response.headers.get("ETag"), text: await response.text() };
  };
  const ok = await s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] });
  const etag = ok.headers.get("ETag");
  const built = await read();
  assert.equal(built.etag, etag);
  await s.room.runAlarm();
  const copied = await read();
  assert.deepEqual(copied, built);
  assert.equal(s.room.object.board().state().verbatim, true);
  assert.deepEqual(
    log.mock.calls.map((c) => c.arguments[0]),
    [`board store: read the pages for ${etag}`, `board store: read R2's bytes for ${etag}`],
  );
  // A commit after the copy moves the board off R2's bytes until the next copy.
  await s.patch({ rows: [{ id: IDEA.id, expect: IDEA, row: { ...IDEA, title: "CSV 2" } }] });
  assert.equal(s.room.object.board().state().verbatim, false);
  assert.equal(JSON.parse((await read()).text).jobs[2].title, "CSV 2");
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
    const s = setup(board);
    const active = board.jobs.find((j) => j.stage === "idea");
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
  assert.equal(s.room.object.board().hasDirty(), true);
  await s.room.runAlarm();
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
