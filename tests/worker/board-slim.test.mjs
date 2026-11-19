// GET /board.json?view=slim: every unfinished row, the newest finished rows and
// per-project finished counts. Rows are made up.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const HOUR = 3600000;
const at = (n) => new Date(Date.UTC(2026, 0, 1) + n * HOUR).toISOString();

function setup(stored) {
  const bucket = memoryBucket();
  if (stored !== undefined)
    bucket.objects.set("board.json", {
      key: "board.json",
      bytes: Buffer.from(stored),
      httpEtag: '"board-seed"',
    });
  const get = (path) =>
    serveFactory(new Request("http://local" + path), { BUCKET: bucket }, person);
  return { get };
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
  assert.equal(slim.headers.get("ETag"), '"board-seed"');
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

test("a missing board is a 404 and an unparseable one goes out as stored", async () => {
  assert.equal((await setup().get("/board.json?view=slim")).status, 404);
  for (const stored of ["{not json", '{"jobs":"none"}']) {
    const response = await setup(stored).get("/board.json?view=slim");
    assert.equal(response.status, 200);
    assert.equal(await response.text(), stored);
    assert.equal(response.headers.get("ETag"), '"board-seed"');
  }
});

test("slim body of 6 active and 1,929 finished rows is under 1/20 of the stored bytes", async () => {
  const stored = JSON.stringify(board(1929, "x".repeat(1500)), null, 2) + "\n";
  const text = await (await setup(stored).get("/board.json?view=slim")).text();
  assert.ok(text.length * 20 < stored.length, `${text.length} of ${stored.length}`);
});
