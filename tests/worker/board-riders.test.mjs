// Board writes cannot add a dashboard job control to a batch rider. Rows are the made-up
// running example: lead 1201 owns PR #88 and rider 1203 is batched into it.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const device = { verified: true, kind: "service" };

const LEAD = { id: "1201-sync-retry", project: "sample", stage: "build", pr: "#88" };
const RIDER = {
  id: "1203-cache-headers",
  project: "sample",
  stage: "build",
  pr: "#88",
  refs: [{ id: "1201-sync-retry", kind: "batched-into" }],
};
const CONTROLS = {
  move: { from: "build", to: "spec", at: "2026-11-10T09:00:00Z" },
  priority: { at: "2026-11-10T09:00:00Z" },
  paused: { at: "2026-11-10T09:00:00Z" },
  close: { reason: "not needed", at: "2026-11-10T09:00:00Z" },
};
const refusal = (field) =>
  `a board write cannot add ${field} to "1203-cache-headers": it is batched into "1201-sync-retry" and follows its lead. Act on the lead instead. Nothing was written.`;

function setup(jobs = [LEAD, RIDER]) {
  const bucket = memoryBucket();
  bucket.objects.set("board.json", {
    key: "board.json",
    bytes: Buffer.from(JSON.stringify({ updated: "2026-11-10T08:00:00Z", jobs })),
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

async function refused(s, rows, field) {
  const before = s.etag();
  const response = await s.write(rows);
  assert.equal(response.status, 400, field);
  assert.equal(await response.text(), refusal(field));
  assert.equal(s.etag(), before);
}

test("refuses each control on a rider", async () => {
  const s = setup();
  for (const [field, value] of Object.entries(CONTROLS))
    await refused(s, [LEAD, { ...RIDER, [field]: value }], field);
});

test("refuses a rerun on a rider", async () => {
  const rerun = { from: "build", to: "build", at: "2026-11-10T09:00:00Z" };
  await refused(setup(), [LEAD, { ...RIDER, move: rerun }], "move");
});

test("the lead still takes controls", async () => {
  const response = await setup().write([{ ...LEAD, priority: CONTROLS.priority }, RIDER]);
  assert.equal(response.status, 200);
});

test("existing controls can change or go", async () => {
  const s = setup([LEAD, { ...RIDER, priority: CONTROLS.priority }]);
  const nudged = { ...CONTROLS.priority, nudged: "2026-11-10T10:00:00Z" };
  assert.equal((await s.write([LEAD, { ...RIDER, priority: nudged }])).status, 200);
  assert.equal((await s.write([LEAD, RIDER])).status, 200);
});

test("rider status comes from either side", async () => {
  const { refs: _, ...unbatched } = RIDER;
  await refused(setup(), [LEAD, { ...unbatched, paused: CONTROLS.paused }], "paused");
  const fresh = setup([LEAD]);
  await refused(fresh, [LEAD, { ...RIDER, paused: CONTROLS.paused }], "paused");
});

test("rows without a string id are skipped", async () => {
  const NO_ID = { project: "sample", stage: "build", refs: RIDER.refs };
  const NUM_ID = { id: 1205, project: "sample", stage: "build", refs: RIDER.refs };
  const s = setup([LEAD, RIDER, NO_ID, NUM_ID]);
  const before = s.etag();
  const response = await s.write([
    LEAD,
    RIDER,
    { ...NO_ID, priority: CONTROLS.priority },
    { ...NUM_ID, paused: CONTROLS.paused },
    null,
  ]);
  assert.equal(response.status, 200);
  assert.notEqual(s.etag(), before);
});

test("the refusal quotes long ids cut", async () => {
  const lead = { ...LEAD, id: "1201-sync-retry-with-backoff-on-every-device-host" };
  const rider = {
    ...RIDER,
    id: "1203-cache-headers-for-the-board-and-backlog-routes",
    refs: [{ id: lead.id, kind: "batched-into" }],
  };
  let s = setup([lead, rider]);
  let response = await s.write([lead, { ...rider, priority: CONTROLS.priority }]);
  let body = await response.text();
  assert.equal(response.status, 400);
  assert.equal(
    body,
    'a board write cannot add priority to "1203-cache-headers-for-the-boar...": it is batched into "1201-sync-retry-with-backoff-on..." and follows its lead. Act on the lead instead. Nothing was written.',
  );
  assert.ok(body.length <= 200);

  const accented = { ...RIDER, id: "1203-" + "é".repeat(40) };
  s = setup([LEAD, accented]);
  response = await s.write([LEAD, { ...accented, paused: CONTROLS.paused }]);
  assert.equal(response.status, 400);
  assert.equal(
    await response.text(),
    String.raw`a board write cannot add paused to "1203-\u00e9\u00e9\u00e9\u00e9...": it is batched into "1201-sync-retry" and follows its lead. Act on the lead instead. Nothing was written.`,
  );
});
