// PATCH /api/board applies the dashboard's changed rows with row-level CAS. Rows are the
// made-up running example: the idea 2001-add-csv-export lands on a board whose pickup host
// writes 1999-synthetic-build at the same moment.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };

const BUILD = { id: "1999-synthetic-build", project: "sample", stage: "build" };
const SPEC = { id: "2000-synthetic-spec", project: "sample", stage: "spec" };
const IDEA = {
  id: "2001-add-csv-export",
  project: "sample",
  title: "Add CSV export",
  idea: "Add CSV export",
  stage: "idea",
  updated: "2026-11-19T12:00:00Z",
  capture_id: "00000000-0000-4000-8000-000000000001",
};
const add = (row = IDEA) => ({ id: row.id, expect: null, row });

function setup(board = { updated: "2026-11-19T11:00:00Z", jobs: [BUILD, SPEC] }) {
  const bucket = memoryBucket();
  bucket.objects.set("board.json", {
    key: "board.json",
    bytes: Buffer.from(JSON.stringify(board)),
    httpEtag: '"board.json-seed"',
  });
  const notified = [];
  const BoardEvents = {
    idFromName: (name) => ({ name }),
    get: () => ({
      fetch: async (url) => (notified.push(url), new Response(null, { status: 204 })),
    }),
  };
  const waited = [];
  const ctx = { waitUntil: (p) => waited.push(p) };
  const env = { BUCKET: bucket, BoardEvents };
  const patch = (body, raw = false) =>
    serveFactory(
      new Request("http://local/api/board", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: raw ? body : JSON.stringify(body),
      }),
      env,
      person,
      ctx,
    );
  const etag = () => bucket.objects.get("board.json").httpEtag;
  const stored = () => JSON.parse(bucket.objects.get("board.json").bytes.toString());
  return { bucket, notified, waited, patch, etag, stored };
}

test("commits a new row", async () => {
  const s = setup();
  const response = await s.patch({ updated: "2026-11-19T12:00:00Z", rows: [add()] });
  assert.equal(response.status, 200);
  assert.equal(await response.text(), "ok");
  assert.equal(response.headers.get("ETag"), s.etag());
  assert.deepEqual(s.stored(), { updated: "2026-11-19T12:00:00Z", jobs: [BUILD, SPEC, IDEA] });
  assert.deepEqual(s.notified, ["https://board-events.invalid/notify?key=board.json"]);
  assert.equal(s.waited.length, 0);

  const asked = { ...BUILD, needs: "Pick a CSV delimiter." };
  const push = await s.patch({
    updated: "2026-11-19T11:30:00Z",
    rows: [{ id: BUILD.id, expect: BUILD, row: asked }],
  });
  assert.equal(push.status, 200);
  assert.deepEqual(s.stored(), { updated: "2026-11-19T12:00:00Z", jobs: [asked, SPEC, IDEA] });
  assert.equal(s.waited.length, 1, "a row that starts waiting on you sends a push");

  const gone = await s.patch({
    rows: [{ id: SPEC.id, expect: { stage: "spec", ...SPEC }, row: null }],
  });
  assert.equal(gone.status, 200, "expect matches by canonical JSON, whatever the key order");
  assert.deepEqual(s.stored().jobs, [asked, IDEA]);
});

test("an unchanged read row is a no-op that still conflicts when stale", async () => {
  const s = setup();
  const read = { id: SPEC.id, expect: SPEC, row: SPEC };
  const done = { ...BUILD, stage: "review" };
  const ok = await s.patch({ rows: [{ id: BUILD.id, expect: BUILD, row: done }, read] });
  assert.equal(ok.status, 200);
  assert.deepEqual(s.stored().jobs, [done, SPEC]);

  const moved = { ...SPEC, stage: "build" };
  await s.patch({ rows: [{ id: SPEC.id, expect: SPEC, row: moved }] });
  const before = s.etag();
  const stale = await s.patch({ rows: [{ id: BUILD.id, expect: done, row: BUILD }, read] });
  assert.equal(stale.status, 409);
  assert.deepEqual(await stale.json(), {
    error: "conflict",
    rows: { [SPEC.id]: moved },
    max_id: 2000,
  });
  assert.equal(s.etag(), before);
});

test("conflicts", async () => {
  const conflict = async (s, rows, body) => {
    const before = s.etag();
    const response = await s.patch({ updated: "2026-11-19T12:00:00Z", rows });
    assert.equal(response.status, 409);
    assert.match(response.headers.get("Content-Type"), /^application\/json/);
    assert.deepEqual(await response.json(), { error: "conflict", ...body });
    assert.equal(s.etag(), before);
  };
  const s = setup();
  await conflict(
    s,
    [{ id: BUILD.id, expect: { ...BUILD, stage: "spec" }, row: { ...BUILD, stage: "review" } }],
    {
      rows: { [BUILD.id]: BUILD },
      max_id: 2000,
    },
  );
  await conflict(s, [add({ ...IDEA, id: "2000-add-csv-export" })], {
    rows: { "2000-add-csv-export": null },
    max_id: 2000,
  });
  const archived = setup({ updated: "", archived: { max_id: 2003 }, jobs: [BUILD] });
  await conflict(archived, [add()], { rows: { [IDEA.id]: null }, max_id: 2003 });
  const saved = { ...IDEA, id: "2002-add-csv-export" };
  const retried = setup({ updated: "", jobs: [BUILD, saved] });
  await conflict(retried, [add({ ...IDEA, id: "2003-add-csv-export" })], {
    rows: { [saved.id]: saved },
    max_id: 2002,
  });
  assert.equal(s.notified.length, 0);
});

test("same refusals", async () => {
  const refused = async (s, rows, text) => {
    const before = s.etag();
    const response = await s.patch({ rows });
    assert.equal(response.status, 400);
    assert.equal(await response.text(), text);
    assert.equal(s.etag(), before);
  };
  const s = setup();
  await refused(
    s,
    [add({ ...IDEA, goal: "x".repeat(301) })],
    'a board write cannot set goal on "2001-add-csv-export": it must be a string of at most 300 characters. Nothing was written.',
  );
  const lead = { id: "2000-synthetic-spec", project: "sample", stage: "build", pr: "#88" };
  const rider = { ...BUILD, pr: "#88", refs: [{ id: lead.id, kind: "batched-into" }] };
  const batch = setup({ updated: "", jobs: [rider, lead] });
  await refused(
    batch,
    [{ id: rider.id, expect: rider, row: { ...rider, paused: { at: "2026-11-19T12:00:00Z" } } }],
    'a board write cannot add paused to "1999-synthetic-build": it is batched into "2000-synthetic-spec" and follows its lead. Act on the lead instead. Nothing was written.',
  );
  await refused(
    s,
    [{ id: SPEC.id, expect: SPEC, row: null }, add()],
    `a board write cannot change a row's id: "2000-synthetic-spec" is gone and a new row appeared. Nothing was written. Upgrade the factory skill, or drop and re-add the row in two writes.`,
  );
});

test("retries", async () => {
  const s = setup();
  const put = s.bucket.put.bind(s.bucket);
  let races = 1;
  s.bucket.put = async (key, value, options) => {
    if (races-- > 0) {
      // A pickup host writes first: the stored version moves under the patch.
      await put(
        key,
        JSON.stringify({
          updated: "2026-11-19T11:59:00Z",
          jobs: [{ ...BUILD, note: "host" }, SPEC],
        }),
      );
      return null;
    }
    return put(key, value, options);
  };
  const response = await s.patch({ updated: "2026-11-19T12:00:00Z", rows: [add()] });
  assert.equal(response.status, 200);
  assert.deepEqual(s.stored().jobs, [{ ...BUILD, note: "host" }, SPEC, IDEA]);

  const busy = setup();
  let puts = 0;
  busy.bucket.put = async () => (puts++, null);
  const tired = await busy.patch({ rows: [add()] });
  assert.equal(tired.status, 409);
  assert.deepEqual(await tired.json(), { error: "busy" });
  assert.equal(puts, 10);
  assert.equal(busy.notified.length, 0);
});

test("shape", async () => {
  const s = setup();
  const before = s.etag();
  const SHAPE =
    "a board patch needs rows: [{id, expect, row}], one entry per id. Nothing was written.";
  const bad = [
    {},
    { rows: [] },
    { rows: "x" },
    { rows: Array.from({ length: 101 }, (_, k) => add({ ...IDEA, id: `${3000 + k}-row` })) },
    { rows: [{ expect: null, row: IDEA }] },
    { rows: [{ id: IDEA.id, row: IDEA }] },
    { rows: [{ id: IDEA.id, expect: null, row: null }] },
    { rows: [{ id: IDEA.id, expect: null, row: { ...IDEA, id: "2002-other" } }] },
    { rows: [{ id: IDEA.id, expect: [], row: IDEA }] },
    { rows: [add(), add()] },
  ];
  for (const body of bad) {
    const response = await s.patch(body);
    assert.equal(response.status, 400, JSON.stringify(body).slice(0, 80));
    assert.equal(await response.text(), SHAPE);
  }
  const notJson = await s.patch("{rows", true);
  assert.equal(notJson.status, 400);
  assert.equal(await notJson.text(), "body is not JSON");
  const big = await s.patch({ rows: [add({ ...IDEA, idea: "x".repeat(1 << 20) })] });
  assert.equal(big.status, 413);
  assert.equal(await big.text(), "body over 1 MiB");
  assert.equal(s.etag(), before);

  const broken = setup();
  broken.bucket.objects.get("board.json").bytes = Buffer.from("{not json");
  const unreadable = await broken.patch({ rows: [add()] });
  assert.equal(unreadable.status, 400);
  assert.equal(
    await unreadable.text(),
    "the stored board is not readable JSON. Nothing was written.",
  );

  const empty = setup();
  empty.bucket.objects.delete("board.json");
  const first = await empty.patch({ updated: "2026-11-19T12:00:00Z", rows: [add()] });
  assert.equal(first.status, 200, "a missing board is created");
  assert.deepEqual(empty.stored(), { updated: "2026-11-19T12:00:00Z", jobs: [IDEA] });
  assert.equal(
    (
      await serveFactory(
        new Request("http://local/api/backlog", { method: "PATCH" }),
        { BUCKET: s.bucket },
        person,
      )
    ).status,
    404,
  );
});
