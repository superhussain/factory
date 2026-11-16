// The hourly idea upload sweep and the repeat-upload clock reset. Keys and rows are the
// made-up running example: the sweep runs at 2026-11-13T12:00:00Z.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test, { mock } from "node:test";
import { loadProduction, memoryBucket, serverEnv } from "../support/worker.mjs";

const { serveFactory, default: worker } = await loadProduction();
const NOW = new Date("2026-11-13T12:00:00Z");
const MP4 = `idea-files/${"1".repeat(64)}.mp4`;
const CSV = `idea-files/${"4".repeat(64)}.csv`;
const PNG = `idea-images/${"2".repeat(64)}.png`;
const PDF = `idea-files/${"3".repeat(64)}.pdf`;

function setup({ board = { jobs: [{ id: "101-demo", idea_images: [PNG.slice(12)] }] } } = {}) {
  const bucket = memoryBucket();
  const seed = (key, body, uploaded = NOW) =>
    bucket.objects.set(key, { key, bytes: Buffer.from(body), uploaded: new Date(uploaded) });
  if (board !== null) seed("board.json", typeof board === "string" ? board : JSON.stringify(board));
  seed(
    "board-archive.json",
    JSON.stringify({ jobs: [{ id: "090-old", idea_files: [PDF.slice(11)] }] }),
  );
  seed(MP4, "x", "2026-11-10T00:00:00Z");
  seed(CSV, "x", "2026-11-13T11:00:00Z");
  seed(PNG, "x", "2026-11-01T00:00:00Z");
  seed(PDF, "x", "2026-10-01T00:00:00Z");
  return { bucket, env: { BUCKET: bucket } };
}

const sweep = (env) => {
  const pending = [];
  worker.scheduled({ scheduledTime: NOW.getTime() }, env, { waitUntil: (p) => pending.push(p) });
  return pending[3];
};

test("deletes only old unnamed uploads", async () => {
  const s = setup();
  assert.deepEqual(await sweep(s.env), { status: "pruned", deleted: [MP4] });
  const kept = [...s.bucket.objects.keys()].filter((k) => k.startsWith("idea-")).sort();
  assert.deepEqual(kept, [PDF, CSV, PNG]);
});

test("skips when a board document is unreadable", async (t) => {
  const warn = mock.method(console, "warn", () => {});
  t.after(() => warn.mock.restore());
  for (const [board, archive, key] of [
    [null, undefined, "board.json"],
    ["{not json", undefined, "board.json"],
    [undefined, "{not json", "board-archive.json"],
  ]) {
    const s = setup(board === undefined ? {} : { board });
    if (archive !== undefined)
      s.bucket.objects.get("board-archive.json").bytes = Buffer.from(archive);
    assert.deepEqual(await sweep(s.env), { status: "skipped", deleted: [] }, key);
    assert.ok(s.bucket.objects.has(MP4), key);
    assert.equal(s.bucket.calls.filter(([op]) => op === "delete").length, 0, key);
    assert.equal(
      warn.mock.calls.at(-1).arguments[0],
      `idea upload sweep skipped: ${key} is unreadable`,
    );
  }
});

test("re-heads before each delete", async () => {
  const s = setup();
  const head = s.bucket.head;
  s.bucket.head = async (key) => ({ ...(await head(key)), uploaded: new Date(NOW) });
  assert.deepEqual(await sweep(s.env), { status: "pruned", deleted: [] });
  assert.ok(s.bucket.objects.has(MP4));
  assert.deepEqual(
    s.bucket.calls.filter(([op]) => op === "head").map(([, key]) => key),
    [MP4],
  );
});

test("a repeat upload resets the clock", async () => {
  const s = setup();
  const put = (path, type, bytes) =>
    serveFactory(
      new Request(serverEnv.FACTORY_ORIGIN + path, {
        method: "PUT",
        headers: { "Content-Type": type },
        body: bytes,
        duplex: "half",
      }),
      { ...serverEnv, BUCKET: s.bucket },
      { verified: true },
    );
  const png = Buffer.alloc(29);
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]).copy(png);
  png.writeUInt32BE(13, 8);
  png.write("IHDR", 12);
  png.writeUInt32BE(1, 16);
  png.writeUInt32BE(1, 20);
  const csv = Buffer.from("a,b\n1,2\n");
  const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
  for (const [path, type, bytes] of [
    [`/api/idea-images/${sha(png)}.png`, "image/png", png],
    [`/api/idea-files/${sha(csv)}.csv`, "text/csv", csv],
  ]) {
    assert.equal((await put(path, type, bytes)).status, 200, path);
    const key = path.slice(5);
    const old = new Date("2026-11-10T00:00:00Z");
    s.bucket.objects.get(key).uploaded = old;
    const response = await put(path, type, bytes);
    assert.equal(response.status, 200, path);
    assert.equal(await response.text(), "ok", path);
    assert.ok(s.bucket.objects.get(key).uploaded > old, path);
    assert.deepEqual(s.bucket.objects.get(key).bytes, bytes, path);
  }
});
