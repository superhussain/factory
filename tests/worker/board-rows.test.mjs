// A committed board PATCH or If-Match PUT sends open tabs a rows frame with the rows it
// changed, and the room sends it only to sockets whose sign-in has not expired. Rows are
// the made-up running example: 2001-add-csv-export lands on a board at "board.json-seed".
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory, BoardEvents } = await loadProduction();
globalThis.WebSocketRequestResponsePair = class {};
const person = { verified: true, kind: "user", email: "person@example.test", exp: 4102444800 };
const ORIGIN = "https://factory.example.test";
const SEED = '"board.json-seed"';
const UPDATED = "2026-11-19T11:00:00Z";

const BUILD = { id: "1999-synthetic-build", project: "sample", stage: "build" };
const SPEC = { id: "2000-synthetic-spec", project: "sample", stage: "spec" };
const IDEA = {
  id: "2001-add-csv-export",
  project: "sample",
  title: "Add CSV export",
  idea: "Add CSV export",
  stage: "idea",
  updated: "2026-11-19T12:00:00Z",
};

// An env whose board starts as `board`, with every room notify body recorded.
function setup(board = { updated: UPDATED, jobs: [BUILD, SPEC] }) {
  const bucket = memoryBucket();
  if (board)
    bucket.objects.set("board.json", {
      key: "board.json",
      bytes: Buffer.from(JSON.stringify(board)),
      httpEtag: SEED,
    });
  const bodies = [];
  const BoardEvents = {
    idFromName: (name) => ({ name }),
    get: () => ({
      fetch: async (input, init) => {
        const request = new Request(input, init);
        bodies.push([request.url, await request.text()]);
        return new Response(null, { status: 204 });
      },
    }),
  };
  const env = { BUCKET: bucket, BoardEvents };
  const send = (method, body, headers = {}) => {
    const init = {
      method,
      headers: { "Content-Type": "application/json", ...headers },
      body: JSON.stringify(body),
    };
    return serveFactory(new Request(ORIGIN + "/api/board", init), env, person, {
      waitUntil() {},
    });
  };
  return {
    bodies,
    patch: (body) => send("PATCH", body),
    put: (body, headers = { "If-Match": SEED }) => send("PUT", body, headers),
  };
}

const NOTIFY = "https://board-events.invalid/notify?key=board.json";

test("patch frame", async () => {
  const s = setup();
  const response = await s.patch({
    updated: "2026-11-19T12:00:00Z",
    rows: [{ id: IDEA.id, expect: null, row: IDEA }],
  });
  assert.equal(response.status, 200);
  assert.equal(s.bodies.length, 1);
  assert.equal(s.bodies[0][0], NOTIFY);
  assert.deepEqual(JSON.parse(s.bodies[0][1]), {
    type: "rows",
    etag: response.headers.get("ETag"),
    prev: SEED,
    updated: "2026-11-19T12:00:00Z",
    rows: [IDEA],
    removed: [],
  });
});

test("put frame", async () => {
  const s = setup();
  const opened = { ...BUILD, stage: "pr-open" };
  const moved = await s.put({ updated: UPDATED, jobs: [opened, SPEC] });
  assert.equal(moved.status, 200);
  assert.deepEqual(JSON.parse(s.bodies[0][1]), {
    type: "rows",
    etag: moved.headers.get("ETag"),
    prev: SEED,
    updated: UPDATED,
    rows: [opened],
    removed: [],
  });
  const dropped = await s.put(
    { updated: UPDATED, jobs: [opened] },
    { "If-Match": moved.headers.get("ETag") },
  );
  assert.equal(dropped.status, 200);
  assert.deepEqual(JSON.parse(s.bodies[1][1]), {
    type: "rows",
    etag: dropped.headers.get("ETag"),
    prev: moved.headers.get("ETag"),
    updated: UPDATED,
    rows: [],
    removed: [SPEC.id],
  });
});

test("a row with its keys reordered, nested or not, is a changed row", async () => {
  const nested = { ...BUILD, refs: [{ kind: "followup", id: SPEC.id }] };
  const s = setup({ updated: UPDATED, jobs: [nested, SPEC] });
  const reordered = { stage: SPEC.stage, project: SPEC.project, id: SPEC.id };
  const inner = { ...BUILD, refs: [{ id: SPEC.id, kind: "followup" }] };
  const response = await s.put({ updated: UPDATED, jobs: [inner, reordered] });
  assert.equal(response.status, 200);
  assert.deepEqual(JSON.parse(s.bodies[0][1]).rows, [inner, reordered]);
});

// The CPU budget at twice today's board, pinned as call counts so it never flakes: a write
// parses the board once per copy it reads and serializes no unchanged row to diff it.
test("a PUT and a PATCH on a 4,600-row board serialize only the changed row", async () => {
  const rows = Array.from({ length: 4600 }, (_, i) => ({
    id: `${1000 + i}-synthetic-row`,
    project: "sample",
    stage: i < 4500 ? "merged" : "build",
    done: UPDATED,
    refs: [{ kind: "followup", id: `${1001 + i}-synthetic-row` }],
  }));
  const s = setup({ updated: UPDATED, jobs: rows });
  const { parse, stringify } = JSON;
  const calls = { parse: 0, stringify: 0 };
  JSON.parse = (...a) => (calls.parse++, parse(...a));
  JSON.stringify = (...a) => (calls.stringify++, stringify(...a));
  let put, patch, putCalls;
  try {
    const opened = { ...rows[4599], stage: "pr-open" };
    put = await s.put({ updated: UPDATED, jobs: [...rows.slice(0, 4599), opened] });
    putCalls = { ...calls };
    patch = await s.patch({
      rows: [{ id: opened.id, expect: opened, row: { ...opened, stage: "review" } }],
    });
  } finally {
    JSON.parse = parse;
    JSON.stringify = stringify;
  }
  assert.equal(put.status, 200);
  assert.equal(patch.status, 200);
  // The request body and the stored board; the rest of the count is small fixed fields.
  assert.equal(putCalls.parse, 2);
  assert.ok(putCalls.stringify < 50, `PUT stringify calls ${putCalls.stringify}`);
  assert.ok(calls.stringify - putCalls.stringify < 50, `PATCH stringify calls ${calls.stringify}`);
  assert.equal(JSON.parse(s.bodies[0][1]).rows.length, 1);
  assert.equal(JSON.parse(s.bodies[1][1]).rows.length, 1);
});

const many = Array.from({ length: 101 }, (_, i) => ({
  id: `${3000 + i}-synthetic-row`,
  project: "sample",
  stage: "build",
}));
const CLOSED = { ...SPEC, stage: "closed" };

for (const [name, board, write] of [
  [
    "a PUT with If-None-Match: *",
    null,
    (s) => s.put({ updated: UPDATED, jobs: [IDEA] }, { "If-None-Match": "*" }),
  ],
  ["a first PATCH", null, (s) => s.patch({ rows: [{ id: IDEA.id, expect: null, row: IDEA }] })],
  [
    "a row moved to merged",
    undefined,
    (s) => s.put({ updated: UPDATED, jobs: [{ ...BUILD, stage: "merged" }, SPEC] }),
  ],
  [
    "a closed row removed",
    { updated: UPDATED, jobs: [BUILD, CLOSED] },
    (s) => s.put({ updated: UPDATED, jobs: [BUILD] }),
  ],
  [
    "a closed row changed by PATCH",
    { updated: UPDATED, jobs: [BUILD, CLOSED] },
    (s) => s.patch({ rows: [{ id: SPEC.id, expect: CLOSED, row: { ...CLOSED, title: "x" } }] }),
  ],
  [
    "another top-level field changed",
    undefined,
    (s) => s.put({ updated: UPDATED, jobs: [BUILD, SPEC], schema: 2 }),
  ],
  [
    "duplicate ids",
    { updated: UPDATED, jobs: [BUILD, BUILD, SPEC] },
    (s) => s.put({ updated: UPDATED, jobs: [BUILD, BUILD, { ...SPEC, stage: "build" }] }),
  ],
  [
    "a non-string id",
    { updated: UPDATED, jobs: [BUILD, { id: 7, stage: "build" }] },
    (s) =>
      s.put({
        updated: UPDATED,
        jobs: [
          { ...BUILD, stage: "pr-open" },
          { id: 7, stage: "build" },
        ],
      }),
  ],
  [
    "over 100 changed rows",
    { updated: UPDATED, jobs: many },
    (s) => s.put({ updated: UPDATED, jobs: many.map((j) => ({ ...j, stage: "pr-open" })) }),
  ],
  [
    "a frame over 262,144 bytes",
    undefined,
    (s) => s.put({ updated: UPDATED, jobs: [{ ...BUILD, summary: "x".repeat(262144) }, SPEC] }),
  ],
]) {
  test(`keeps the ping: ${name}`, async () => {
    const s = setup(board === undefined ? { updated: UPDATED, jobs: [BUILD, SPEC] } : board);
    const response = await write(s);
    assert.equal(response.status, 200, await response.clone().text());
    assert.deepEqual(s.bodies, [[NOTIFY, ""]]);
  });
}

// A socket that records every frame it is sent, opened with attachment `attached`.
const socket = (attached) => {
  const sent = [];
  return { sent, send: (frame) => sent.push(frame), deserializeAttachment: () => attached };
};

test("room", async () => {
  const now = Math.floor(Date.now() / 1000);
  const fresh = socket({ exp: now + 3600 });
  const expired = socket({ exp: now - 1 });
  const unattached = socket(null);
  const room = new BoardEvents(
    { getWebSockets: () => [fresh, expired, unattached], setWebSocketAutoResponse() {} },
    {},
  );
  const frame = JSON.stringify({ type: "rows", etag: '"e2"', prev: '"e1"', rows: [IDEA] });
  const notify = (key) =>
    room.fetch(
      new Request("https://board-events.invalid/notify?key=" + key, {
        method: "POST",
        body: frame,
      }),
    );
  assert.equal((await notify("board.json")).status, 204);
  assert.equal((await notify("backlog.md")).status, 204);
  const changed = (key) => JSON.stringify({ type: "changed", key });
  assert.deepEqual(fresh.sent, [frame, changed("backlog.md")]);
  assert.deepEqual(expired.sent, [changed("board.json"), changed("backlog.md")]);
  assert.deepEqual(unattached.sent, [changed("board.json"), changed("backlog.md")]);
});

test("exp header", async () => {
  const forwarded = [];
  const BoardEvents = {
    idFromName: (name) => ({ name }),
    get: () => ({
      fetch: async (input, init) => (
        forwarded.push(new Request(input, init)),
        new Response(null, { status: 204 })
      ),
    }),
  };
  const headers = { Upgrade: "websocket", Origin: ORIGIN, "X-Factory-Exp": "9999999999" };
  for (const principal of [person, { ...person, exp: undefined }])
    await serveFactory(
      new Request(ORIGIN + "/events/board", { headers }),
      { BUCKET: memoryBucket(), BoardEvents },
      principal,
    );
  assert.deepEqual(
    forwarded.map((r) => r.headers.get("X-Factory-Exp")),
    ["4102444800", null],
  );
});
