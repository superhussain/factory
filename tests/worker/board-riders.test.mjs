// Board writes cannot add a dashboard job control to a batch rider, nor add or drop a
// batched-into ref outside factory.py batch and the lead's close or merge. Rows are the made-up
// running example: lead 1201 owns PR #88, rider 1203 is batched into it, 1204 is an idea.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
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
const IDEA = { id: "1204-etag-board", project: "sample", stage: "idea" };
const CONTROLS = {
  move: { from: "build", to: "spec", at: "2026-11-10T09:00:00Z" },
  priority: { at: "2026-11-10T09:00:00Z" },
  paused: { at: "2026-11-10T09:00:00Z" },
  close: { reason: "not needed", at: "2026-11-10T09:00:00Z" },
  babysit: { at: "2026-11-10T09:00:00Z" },
};
const RIDER_CONTROLS = JSON.parse(
  /const RIDER_CONTROLS = (\[[^\]]*\]);/.exec(
    readFileSync(new URL("../../worker/index.js", import.meta.url), "utf8"),
  )[1],
);
const refusal = (field) =>
  `a board write cannot add ${field} to "1203-cache-headers": it is batched into "1201-sync-retry" and follows its lead. Act on the lead instead. Nothing was written.`;

const batchRefusal = (row, lead) =>
  `a board write cannot batch "${row}" into "${lead}": factory.py batch does that, while the lead is at spec. Nothing was written.`;
const unbatchRefusal = (row, lead) =>
  `a board write cannot unbatch "${row}": its lead "${lead}" is still open. Only the lead's close or merge drops the ref. Nothing was written.`;
const { refs: _riderRefs, ...UNBATCHED } = RIDER;

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

test("the refusal fits 200 characters for every rider control", async () => {
  assert.deepEqual(RIDER_CONTROLS, Object.keys(CONTROLS));
  const lead = { ...LEAD, id: "1201-" + "a".repeat(60) };
  const rider = {
    ...RIDER,
    id: "1203-" + "b".repeat(60),
    refs: [{ id: lead.id, kind: "batched-into" }],
  };
  for (const field of RIDER_CONTROLS) {
    const response = await setup([lead, rider]).write([
      lead,
      { ...rider, [field]: CONTROLS[field] },
    ]);
    const body = await response.text();
    assert.equal(response.status, 400, field);
    assert.ok(body.startsWith("a board write cannot add " + field + " to "), body);
    assert.ok(body.length <= 200, field + ": " + body.length);
  }
});

async function refusedWith(s, rows, message) {
  const before = s.etag();
  const response = await s.write(rows);
  assert.equal(response.status, 400);
  assert.equal(await response.text(), message);
  assert.equal(s.etag(), before);
}

test("refuses adding a batched-into ref outside factory.py batch", async () => {
  const ref = { refs: [{ id: LEAD.id, kind: "batched-into" }] };
  const message = batchRefusal(IDEA.id, LEAD.id);
  await refusedWith(setup([LEAD, IDEA]), [LEAD, { ...IDEA, ...ref, stage: "spec" }], message);
  const atSpec = { ...LEAD, stage: "spec" };
  const s = setup([atSpec, IDEA]);
  await refusedWith(s, [atSpec, { ...IDEA, ...ref }], message);
  await refusedWith(s, [atSpec, { ...IDEA, ...ref, stage: "build" }], message);
  const built = { ...IDEA, stage: "build" };
  await refusedWith(setup([atSpec, built]), [atSpec, { ...built, ...ref, stage: "spec" }], message);
  const other = { ...LEAD, id: "1202-other-lead", stage: "spec" };
  await refusedWith(
    setup([atSpec, other, { ...RIDER, stage: "spec" }]),
    [
      atSpec,
      other,
      { ...RIDER, stage: "spec", refs: [...RIDER.refs, { id: other.id, kind: "batched-into" }] },
    ],
    batchRefusal(RIDER.id, other.id),
  );
});

test("allows the factory.py batch shape", async () => {
  const atSpec = { ...LEAD, stage: "spec" };
  const ref = { stage: "spec", refs: [{ id: LEAD.id, kind: "batched-into" }] };
  const s = setup([atSpec, IDEA]);
  assert.equal((await s.write([atSpec, { ...IDEA, ...ref }])).status, 200);
  const fresh = { id: "1205-backlog-line", project: "sample", ...ref };
  assert.equal((await setup([atSpec]).write([atSpec, fresh])).status, 200);
});

test("refuses dropping a rider's ref while its lead is open", async () => {
  const s = setup();
  await refusedWith(s, [LEAD, UNBATCHED], unbatchRefusal(RIDER.id, LEAD.id));
  await refused(s, [LEAD, { ...UNBATCHED, close: CONTROLS.close }], "close");
});

test("drops the ref once the lead closes, merges, or is gone", async () => {
  const idle = { ...UNBATCHED, stage: "idea" };
  for (const stage of ["closed", "merged"])
    assert.equal((await setup().write([{ ...LEAD, stage }, idle])).status, 200, stage);
  assert.equal((await setup().write([idle])).status, 200);
});

test("batch refusals fit 200 characters", async () => {
  const lead = { ...LEAD, id: "1201-" + "a".repeat(60) };
  const rider = {
    ...RIDER,
    id: "1203-" + "b".repeat(60),
    refs: [{ id: lead.id, kind: "batched-into" }],
  };
  const { refs: _, ...bare } = rider;
  const idea = { ...IDEA, id: "1204-" + "c".repeat(60) };
  for (const [stored, rows, start] of [
    [[lead, rider], [lead, bare], "a board write cannot unbatch "],
    [
      [lead, idea],
      [lead, { ...idea, stage: "spec", refs: rider.refs }],
      "a board write cannot batch ",
    ],
  ]) {
    const response = await setup(stored).write(rows);
    const body = await response.text();
    assert.equal(response.status, 400);
    assert.ok(body.startsWith(start), body);
    assert.ok(body.length <= 200, body.length + ": " + body);
  }
});
