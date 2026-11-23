// GET /board.json?view=slim: every unfinished row, the newest finished rows and
// per-project finished counts. Rows are made up.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const HOUR = 3600000;
const at = (n) => new Date(Date.UTC(2026, 0, 1) + n * HOUR).toISOString();

// The Worker keeps the last slim body by board ETag, so each stored board gets its own ETag.
function setup(stored) {
  const bucket = memoryBucket();
  const etag = '"' + createHash("sha256").update(String(stored)).digest("hex") + '"';
  if (stored !== undefined)
    bucket.objects.set("board.json", {
      key: "board.json",
      bytes: Buffer.from(stored),
      httpEtag: etag,
    });
  const get = (path) =>
    serveFactory(new Request("http://local" + path), { BUCKET: bucket }, person);
  return { bucket, etag, get };
}

// 6 active rows and `finished` finished rows; finished row n was updated n hours in.
function board(finished, idea = "") {
  const jobs = [];
  for (let n = 0; n < finished; n++)
    jobs.push({
      id: `${String(1000 + n)}-synthetic`,
      project: n % 3 === 0 ? "other" : "sample",
      stage: n % 2 ? "closed" : "merged",
      updated: at(n),
      done: at(n),
      idea,
    });
  for (let n = 0; n < 6; n++)
    jobs.splice(n * 3, 0, { id: `90${n}-active`, project: "sample", stage: "build", idea });
  return { updated: at(0), jobs };
}

test("slim keeps active rows and the 10 newest rows of each finished stage, with counts, ETag and version", async () => {
  const data = board(30);
  data.jobs.push({ id: "950-no-project", stage: "closed", updated: at(-5) });
  const stored = JSON.stringify(data, null, 2) + "\n";
  const s = setup(stored);
  const slim = await s.get("/board.json?view=slim");
  assert.equal(slim.status, 200);
  assert.equal(slim.headers.get("ETag"), s.etag);
  const plain = await s.get("/board.json");
  assert.ok(slim.headers.get("X-Factory-Version"));
  assert.equal(slim.headers.get("X-Factory-Version"), plain.headers.get("X-Factory-Version"));
  const body = await slim.json();
  const finished = (j) => j.stage === "merged" || j.stage === "closed";
  const expected = data.jobs.filter(
    (j) => !finished(j) || Date.parse(j.updated) >= Date.parse(at(10)),
  );
  assert.deepEqual(
    body.jobs.map((j) => j.id),
    expected.map((j) => j.id),
  );
  assert.equal(body.jobs.filter((j) => j.stage === "merged").length, 10);
  assert.equal(body.jobs.filter((j) => j.stage === "closed").length, 10);
  assert.equal(body.updated, data.updated);
  assert.deepEqual(body.slim, {
    recent: 10,
    counts: {
      other: { merged: 5, closed: 5 },
      sample: { merged: 10, closed: 10 },
      "": { merged: 0, closed: 1 },
    },
  });
  assert.equal(await plain.text(), stored);
});

test("slim ranks finished rows by done, else updated", async () => {
  const jobs = [];
  for (let n = 1; n <= 11; n++)
    jobs.push({
      id: `${1000 + n}-synthetic`,
      project: "sample",
      stage: "merged",
      done: at(n),
      updated: at(n),
    });
  jobs.push({
    id: "980-touched",
    project: "sample",
    stage: "merged",
    done: at(0),
    updated: at(100),
  });
  jobs.push({ id: "981-no-done", project: "sample", stage: "merged", updated: at(50) });
  const body = await (await setup(JSON.stringify({ jobs })).get("/board.json?view=slim")).json();
  const merged = body.jobs.filter((j) => j.stage === "merged").map((j) => j.id);
  const expected = [];
  for (let n = 3; n <= 11; n++) expected.push(`${1000 + n}-synthetic`);
  expected.push("981-no-done");
  assert.deepEqual(merged.toSorted(), expected.toSorted());
  assert.ok(!merged.includes("980-touched"));
});

test("a missing board is a 404 and an unparseable one goes out as stored", async () => {
  assert.equal((await setup().get("/board.json?view=slim")).status, 404);
  for (const stored of ["{not json", '{"jobs":"none"}']) {
    const s = setup(stored);
    const response = await s.get("/board.json?view=slim");
    assert.equal(response.status, 200);
    assert.equal(await response.text(), stored);
    assert.equal(response.headers.get("ETag"), s.etag);
  }
});

test("slim body of 6 active and 1,929 finished rows is under 1/20 of the stored bytes", async () => {
  const stored = JSON.stringify(board(1929, "x".repeat(1500)), null, 2) + "\n";
  const text = await (await setup(stored).get("/board.json?view=slim")).text();
  assert.ok(text.length * 20 < stored.length, `${text.length} of ${stored.length}`);
});

test("projects named __proto__ and constructor keep their own counts", async () => {
  const row = (id, project, stage) => ({ id, project, stage, updated: at(1) });
  const stored = JSON.stringify({
    jobs: [
      row("960-proto", "__proto__", "merged"),
      row("961-ctor", "constructor", "closed"),
      row("962-ctor", "constructor", "merged"),
    ],
  });
  const body = await (await setup(stored).get("/board.json?view=slim")).json();
  assert.deepEqual(Object.entries(body.slim.counts), [
    ["__proto__", { merged: 1, closed: 0 }],
    ["constructor", { merged: 1, closed: 1 }],
  ]);
  assert.equal({}.merged, undefined);
});

test("two reads of one board version slice it once, and a new version slices again", async () => {
  const s = setup(JSON.stringify(board(30)));
  const reads = () =>
    s.bucket.calls.filter(([op, key]) => op === "get" && key === "board.json").length;
  const first = await s.get("/board.json?view=slim");
  const second = await s.get("/board.json?view=slim");
  assert.equal(reads(), 1);
  assert.equal(await second.text(), await first.text());
  for (const response of [first, second]) {
    assert.equal(response.headers.get("ETag"), s.etag);
    assert.ok(response.headers.get("X-Factory-Version"));
  }
  const next = JSON.stringify(board(31));
  s.bucket.objects.set("board.json", {
    key: "board.json",
    bytes: Buffer.from(next),
    httpEtag: '"next"',
  });
  const third = await s.get("/board.json?view=slim");
  assert.equal(reads(), 2);
  assert.equal(third.headers.get("ETag"), '"next"');
  assert.deepEqual((await third.json()).slim.counts.other, { merged: 6, closed: 5 });
});

test("a write between head and get answers with the get's ETag and counts, then hits the memo", async () => {
  const s = setup(JSON.stringify(board(4)));
  const head = s.bucket.head;
  let stale = true;
  s.bucket.head = async (key) => {
    const found = await head(key);
    if (!stale) return found;
    stale = false;
    return { ...found, httpEtag: '"stale"' };
  };
  const reads = () =>
    s.bucket.calls.filter(([op, key]) => op === "get" && key === "board.json").length;
  const first = await s.get("/board.json?view=slim");
  assert.equal(first.headers.get("ETag"), s.etag);
  assert.deepEqual((await first.json()).slim.counts, {
    other: { merged: 1, closed: 1 },
    sample: { merged: 1, closed: 1 },
  });
  const second = await s.get("/board.json?view=slim");
  assert.equal(second.headers.get("ETag"), s.etag);
  assert.equal(reads(), 1);
});

// Counts board reads and holds each one 20 ms, so concurrent requests overlap.
function slowGets(s, failFirst = false) {
  const get = s.bucket.get;
  const counter = { gets: 0 };
  s.bucket.get = async (key) => {
    const n = ++counter.gets;
    await new Promise((resolve) => setTimeout(resolve, 20));
    if (failFirst && n === 1) throw new Error("read failed");
    return get(key);
  };
  return counter;
}

// Each request owns its read, since a promise one request starts cannot settle in another.
test("3 concurrent misses on one ETag each make their own get and return identical bodies", async () => {
  const s = setup(JSON.stringify(board(32)));
  const counter = slowGets(s);
  const responses = await Promise.all([1, 2, 3].map(() => s.get("/board.json?view=slim")));
  assert.equal(counter.gets, 3);
  const bodies = await Promise.all(responses.map((r) => r.text()));
  for (const r of responses) assert.equal(r.status, 200);
  assert.equal(new Set(bodies).size, 1);
});

test("a failed read fails only its own request", async () => {
  const s = setup(JSON.stringify(board(33)));
  const counter = slowGets(s, true);
  const [first, second] = await Promise.allSettled(
    [1, 2].map(() => s.get("/board.json?view=slim")),
  );
  assert.equal(first.status, "rejected");
  assert.equal(second.value.status, 200);
  assert.equal(counter.gets, 2);
});

test("a slim read hands nothing to waitUntil", async () => {
  const s = setup(JSON.stringify(board(34)));
  const kept = [];
  const ctx = { waitUntil: (promise) => kept.push(promise) };
  const r = await serveFactory(
    new Request("http://local/board.json?view=slim"),
    { BUCKET: s.bucket },
    person,
    ctx,
  );
  assert.equal(r.status, 200);
  assert.equal(kept.length, 0);
});

test("a duplicate id counts once per stage", async () => {
  const row = (id, stage, n) => ({ id, project: "sample", stage, updated: at(n) });
  const stored = JSON.stringify({
    jobs: [
      row("970-twice", "merged", 1),
      row("970-twice", "merged", 2),
      row("971-once", "merged", 3),
      row("972-closed", "closed", 4),
    ],
  });
  const body = await (await setup(stored).get("/board.json?view=slim")).json();
  assert.deepEqual(body.slim.counts, { sample: { merged: 2, closed: 1 } });
});
