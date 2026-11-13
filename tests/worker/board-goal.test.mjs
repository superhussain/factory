// Board writes keep a row's goal to a string of at most 300 code points. The row is the
// made-up running example 001-test.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const device = { verified: true, kind: "service" };

const ROW = { id: "001-test", project: "sample", stage: "build" };
const refusal = (label) =>
  `a board write cannot set goal on ${label}: it must be a string of at most 300 characters. Nothing was written.`;

function setup() {
  const bucket = memoryBucket();
  bucket.objects.set("board.json", {
    key: "board.json",
    bytes: Buffer.from(JSON.stringify({ updated: "2026-11-10T08:00:00Z", jobs: [ROW] })),
    httpEtag: '"board.json-seed"',
  });
  const etag = () => bucket.objects.get("board.json").httpEtag;
  const write = (rows) =>
    serveFactory(
      new Request("http://local/api/board", {
        method: "PUT",
        body: JSON.stringify({ updated: "2026-11-10T09:00:00Z", jobs: rows }),
        headers: { "If-Match": etag() },
        duplex: "half",
      }),
      { BUCKET: bucket },
      device,
    );
  return { etag, write };
}

async function refused(row, label) {
  const s = setup();
  const before = s.etag();
  const response = await s.write([row]);
  assert.equal(response.status, 400);
  assert.equal(await response.text(), refusal(label));
  assert.equal(s.etag(), before);
}

test("refuses a goal that is not a string", async () => {
  await refused({ ...ROW, goal: 7 }, '"001-test"');
  await refused({ ...ROW, goal: null }, '"001-test"');
});

test("refuses a goal of 301 code points", async () => {
  await refused({ ...ROW, goal: "😀".repeat(301) }, '"001-test"');
});

test("labels a row without a string id", async () => {
  const s = setup();
  const response = await s.write([ROW, { project: "sample", stage: "idea", goal: 7 }]);
  assert.equal(response.status, 400);
  assert.equal(await response.text(), refusal("a row without a string id"));
});

test("accepts a goal of 300 code points and a row without one", async () => {
  const s = setup();
  assert.equal((await s.write([{ ...ROW, goal: "😀".repeat(300) }])).status, 200);
  assert.equal((await s.write([ROW])).status, 200);
});
