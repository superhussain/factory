// Board writes on the live socket: the room applies a patch frame with the same core as
// PATCH /api/board, acks it and tells every socket, or answers a typed error and writes
// nothing. Rows are the made-up running example: the idea 2001-add-csv-export.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { BoardEvents } = await loadProduction();
globalThis.WebSocketRequestResponsePair = class {
  constructor(request, response) {
    Object.assign(this, { request, response });
  }
};
const ORIGIN = "https://factory.example.test";

// A socket that records every frame it is sent and keeps its attachment.
function recordingSocket() {
  let attachment = null;
  const sent = [];
  return {
    sent,
    frames: () => sent.map((f) => JSON.parse(f)),
    send: (frame) => sent.push(frame),
    serializeAttachment: (value) => (attachment = structuredClone(value)),
    deserializeAttachment: () => attachment,
  };
}
const pairs = [];
globalThis.WebSocketPair = class {
  constructor() {
    const pair = [recordingSocket(), recordingSocket()];
    pairs.push(pair);
    Object.assign(this, pair);
  }
};
// Node's Response refuses the 101 an accepted upgrade answers with.
const NodeResponse = Response;
globalThis.Response = class extends NodeResponse {
  constructor(body, init) {
    super(body, init?.status === 101 ? { ...init, status: 200 } : init);
  }
};

const BUILD = { id: "1999-synthetic-build", project: "sample", stage: "build" };
const IDEA = {
  id: "2001-add-csv-export",
  project: "sample",
  title: "Add CSV export",
  idea: "Add CSV export",
  stage: "idea",
  updated: "2026-11-19T12:00:00Z",
};
const add = (row = IDEA) => ({ id: row.id, expect: null, row });

// One room with a seeded board, one connected writer and one other tab.
async function setup({ exp = Math.floor(Date.now() / 1000) + 3600 } = {}) {
  const bucket = memoryBucket();
  bucket.objects.set("board.json", {
    key: "board.json",
    bytes: Buffer.from(JSON.stringify({ updated: "2026-11-19T11:00:00Z", jobs: [BUILD] })),
    httpEtag: '"board.json-seed"',
  });
  const sockets = [];
  const ctx = {
    getWebSockets: () => sockets,
    acceptWebSocket: (socket) => sockets.push(socket),
    setWebSocketAutoResponse() {},
    waitUntil() {},
  };
  const room = new BoardEvents(ctx, { BUCKET: bucket });
  const headers = { Upgrade: "websocket" };
  if (exp !== null) headers["X-Factory-Access-Exp"] = String(exp);
  await room.fetch(new Request(ORIGIN + "/events/board", { headers }));
  const writer = pairs.at(-1)[1];
  const other = recordingSocket();
  sockets.push(other);
  const send = (frame) =>
    room.webSocketMessage(
      writer,
      typeof frame === "string" || frame instanceof ArrayBuffer ? frame : JSON.stringify(frame),
    );
  const etag = () => bucket.objects.get("board.json").httpEtag;
  const stored = () => JSON.parse(bucket.objects.get("board.json").bytes.toString());
  return { bucket, writer, other, send, etag, stored };
}

// Sends one frame and checks it wrote nothing and told no other tab.
async function refused(s, frame, reply) {
  const before = s.etag();
  await s.send(frame);
  assert.deepEqual(s.writer.frames().at(-1), reply);
  assert.equal(s.etag(), before);
  assert.deepEqual(s.other.sent, []);
}

test("acks and broadcasts", async () => {
  const s = await setup();
  assert.deepEqual(s.writer.deserializeAttachment().origin, ORIGIN);
  await s.send({ type: "patch", op: "w1", updated: "2026-11-19T12:00:00Z", rows: [add()] });
  assert.deepEqual(s.stored(), { updated: "2026-11-19T12:00:00Z", jobs: [BUILD, IDEA] });
  const changed = { type: "changed", key: "board.json" };
  assert.deepEqual(s.writer.frames(), [{ type: "ack", op: "w1", etag: s.etag() }, changed]);
  assert.deepEqual(s.other.frames(), [changed]);
});

test("conflict", async () => {
  const s = await setup();
  await refused(
    s,
    {
      type: "patch",
      op: "w1",
      rows: [{ id: BUILD.id, expect: { ...BUILD, stage: "spec" }, row: null }],
    },
    { type: "error", op: "w1", error: "conflict", rows: { [BUILD.id]: BUILD }, max_id: 1999 },
  );
});

test("busy", async () => {
  const s = await setup();
  s.bucket.put = async () => null;
  await refused(
    s,
    { type: "patch", op: "w1", rows: [add()] },
    { type: "error", op: "w1", error: "busy" },
  );
});

test("refused", async () => {
  const s = await setup();
  await refused(
    s,
    { type: "patch", op: "w1", rows: [add({ ...IDEA, goal: "x".repeat(301) })] },
    {
      type: "error",
      op: "w1",
      error: "refused",
      status: 400,
      message:
        'a board write cannot set goal on "2001-add-csv-export": it must be a string of at most 300 characters. Nothing was written.',
    },
  );
});

test("shape", async () => {
  const s = await setup();
  const SHAPE =
    "a board patch needs rows: [{id, expect, row}], one entry per id. Nothing was written.";
  const error = (op, status, message) => ({ type: "error", op, error: "refused", status, message });
  await refused(s, "{rows", error(null, 400, "body is not JSON"));
  await refused(s, "x".repeat((1 << 20) + 1), error(null, 413, "body over 1 MiB"));
  await refused(s, { type: "changed", op: "w1", rows: [add()] }, error("w1", 400, SHAPE));
  await refused(s, { type: "patch", op: "w 1", rows: [add()] }, error(null, 400, SHAPE));
  await refused(s, { type: "patch", rows: [add()] }, error(null, 400, SHAPE));
  await refused(s, { type: "patch", op: "w2", rows: [] }, error("w2", 400, SHAPE));
  await refused(s, new ArrayBuffer(4), error(null, 413, "body over 1 MiB"));
});

test("expired", async () => {
  const s = await setup({ exp: Math.floor(Date.now() / 1000) - 1 });
  await refused(
    s,
    { type: "patch", op: "w1", rows: [add()] },
    { type: "error", op: "w1", error: "expired" },
  );
});

test("missing exp", async () => {
  const s = await setup({ exp: null });
  assert.equal(s.writer.deserializeAttachment().exp, 0);
  await refused(
    s,
    { type: "patch", op: "w1", rows: [add()] },
    { type: "error", op: "w1", error: "expired" },
  );
});
