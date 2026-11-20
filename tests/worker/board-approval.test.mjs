// Only a signed-in person sets the spec-approval note "spec approved on the dashboard", and
// no write sets "spec approved in Tracer" unless the row's tracer.scope is approved. Rows are
// the made-up running example: 042-sample-export waits at needs-human on the spec-approval gate.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const device = { verified: true, kind: "service" };
const person = { verified: true, kind: "user", email: "person@example.test", exp: 4102444800 };

const DASHBOARD = "spec approved on the dashboard";
const TRACER = "spec approved in Tracer";
const ROW = {
  id: "042-sample-export",
  project: "sample",
  stage: "needs-human",
  note: "gate: spec-approval",
  tracer: {},
};
const approved = (note) => ({ ...ROW, stage: "build", note });
const dashboardRefusal =
  'a board write from a device cannot set the note "spec approved on the dashboard" on "042-sample-export": only a signed-in person approves a spec on the dashboard. Nothing was written.';
const tracerRefusal =
  'a board write cannot set the note "spec approved in Tracer" on "042-sample-export" unless its tracer.scope is "approved". Nothing was written.';

const keptRefusal = (note) =>
  `a board write from a device cannot move "042-sample-export" into build while it keeps the note "${note}": only a person starts an approved spec again. Nothing was written.`;

function setup(row = ROW, { seed = true } = {}) {
  const bucket = memoryBucket();
  if (seed)
    bucket.objects.set("board.json", {
      key: "board.json",
      bytes: Buffer.from(JSON.stringify({ updated: "2026-11-20T08:00:00Z", jobs: [row] })),
      httpEtag: '"board.json-seed"',
    });
  const BoardEvents = {
    idFromName: (name) => ({ name }),
    get: () => ({ fetch: async () => new Response(null, { status: 204 }) }),
  };
  const env = { BUCKET: bucket, BoardEvents };
  const ctx = { waitUntil: () => {} };
  const etag = () => bucket.objects.get("board.json").httpEtag;
  const put = (next) =>
    serveFactory(
      new Request("http://local/api/board", {
        method: "PUT",
        body: JSON.stringify({ updated: "2026-11-20T09:00:00Z", jobs: [next] }),
        headers: { "If-Match": etag() },
        duplex: "half",
      }),
      env,
      device,
      ctx,
    );
  const create = (next) =>
    serveFactory(
      new Request("http://local/api/board", {
        method: "PUT",
        body: JSON.stringify({ updated: "2026-11-20T09:00:00Z", jobs: [next] }),
        headers: { "If-None-Match": "*" },
        duplex: "half",
      }),
      env,
      device,
      ctx,
    );
  const patch = (next, principal) =>
    serveFactory(
      new Request("http://local/api/board", {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ rows: [{ id: row.id, expect: row, row: next }] }),
      }),
      env,
      principal,
      ctx,
    );
  return { bucket, etag, put, create, patch };
}

async function refused(s, write, copy) {
  const before = s.etag();
  const response = await write();
  assert.equal(response.status, 400);
  assert.equal(await response.text(), copy);
  assert.equal(s.etag(), before);
}

test("a device PUT cannot set the dashboard note", async () => {
  const s = setup();
  await refused(s, () => s.put(approved(DASHBOARD)), dashboardRefusal);
});

test("a device PATCH cannot set the dashboard note", async () => {
  const s = setup();
  await refused(s, () => s.patch(approved(DASHBOARD), device), dashboardRefusal);
});

test("a device PUT cannot set the Tracer note without an approved scope", async () => {
  const s = setup();
  await refused(s, () => s.put(approved(TRACER)), tracerRefusal);
});

test("a device PUT sets the Tracer note with an approved scope", async () => {
  const response = await setup().put({ ...approved(TRACER), tracer: { scope: "approved" } });
  assert.equal(response.status, 200);
});

test("a person's PATCH sets the dashboard note", async () => {
  const response = await setup().patch(approved(DASHBOARD), person);
  assert.equal(response.status, 200);
});

test("a device PUT keeps a stored dashboard note", async () => {
  const s = setup(approved(DASHBOARD));
  const claim = { device: "host-a" };
  const response = await s.put({ ...approved(DASHBOARD), claim });
  assert.equal(response.status, 200);
});

// Only a signed-in person sets resolve, the dashboard's Rework or Approve as is on a held job.
// A device may delete it, which only cancels the request.
const HELD = {
  id: "064-sample-human",
  project: "sample",
  stage: "needs-human",
  note: "review panel: round 2 findings need a person",
  needs: "decide on the round-2 findings",
};
const REWORK = { action: "rework", note: "Keep the CSV header order.", at: "2026-11-20T09:00:00Z" };
const resolved = (resolve) => {
  const { needs: _needs, ...row } = HELD;
  return { ...row, note: "rework requested on the dashboard", resolve };
};
const deviceResolveRefusal =
  'a board write from a device cannot set resolve on "064-sample-human": only a signed-in person asks for rework or approval on the dashboard. Nothing was written.';
const shapeRefusal =
  'resolve on "064-sample-human" must be {action: "rework", note, at} or {action: "approve", at}. Nothing was written.';

test("a device PUT cannot set resolve", async () => {
  const s = setup(HELD);
  await refused(s, () => s.put(resolved(REWORK)), deviceResolveRefusal);
});

test("a device PATCH cannot set resolve", async () => {
  const s = setup(HELD);
  await refused(s, () => s.patch(resolved(REWORK), device), deviceResolveRefusal);
});

test("a person's PATCH sets rework and approve", async () => {
  assert.equal((await setup(HELD).patch(resolved(REWORK), person)).status, 200);
  const approve = { action: "approve", at: "2026-11-20T09:00:00Z" };
  assert.equal((await setup(HELD).patch(resolved(approve), person)).status, 200);
});

test("a person's PATCH cannot set a malformed resolve", async () => {
  for (const bad of [
    { action: "rework", note: "   ", at: "2026-11-20T09:00:00Z" },
    { action: "rework", note: "x".repeat(2001), at: "2026-11-20T09:00:00Z" },
    { action: "rework", at: "2026-11-20T09:00:00Z" },
    { action: "approve", note: "extra", at: "2026-11-20T09:00:00Z" },
    { action: "approve" },
    { action: "ship", at: "2026-11-20T09:00:00Z" },
    "rework",
    null,
  ]) {
    const s = setup(HELD);
    await refused(s, () => s.patch(resolved(bad), person), shapeRefusal);
  }
});

test("a person and a device delete resolve", async () => {
  const stored = resolved(REWORK);
  const { resolve: _resolve, ...cleared } = stored;
  assert.equal((await setup(stored).patch(cleared, person)).status, 200);
  assert.equal((await setup(stored).patch(cleared, device)).status, 200);
  assert.equal((await setup(stored).put(cleared)).status, 200);
});

test("a device PUT keeps a stored resolve", async () => {
  const stored = resolved(REWORK);
  const response = await setup(stored).put({ ...stored, claim: { device: "host-a" } });
  assert.equal(response.status, 200);
});

test("a device create on an absent board runs the approval check", async () => {
  const s = setup(ROW, { seed: false });
  const response = await s.create(approved(DASHBOARD));
  assert.equal(response.status, 400);
  assert.equal(await response.text(), dashboardRefusal);
  assert.equal(s.bucket.objects.has("board.json"), false);
});

test("a device create on an existing board gets 412", async () => {
  const s = setup();
  const before = s.etag();
  const response = await s.create(approved(DASHBOARD));
  assert.equal(response.status, 412);
  assert.equal(s.etag(), before);
});

test("a device PUT cannot move a kept dashboard note into build", async () => {
  const s = setup({ ...ROW, note: DASHBOARD });
  await refused(s, () => s.put(approved(DASHBOARD)), keptRefusal(DASHBOARD));
});

test("a device PATCH cannot move a kept dashboard note into build", async () => {
  const s = setup({ ...ROW, note: DASHBOARD });
  await refused(s, () => s.patch(approved(DASHBOARD), device), keptRefusal(DASHBOARD));
});

test("a device PUT cannot move a kept Tracer note into build", async () => {
  const s = setup({ ...ROW, note: TRACER });
  await refused(s, () => s.put(approved(TRACER)), keptRefusal(TRACER));
});

test("a person's PATCH moves a kept dashboard note into build", async () => {
  const response = await setup({ ...ROW, note: DASHBOARD }).patch(approved(DASHBOARD), person);
  assert.equal(response.status, 200);
});

test("a device PUT moves a kept dashboard note out of build", async () => {
  const s = setup(approved(DASHBOARD));
  const response = await s.put({ ...ROW, note: DASHBOARD });
  assert.equal(response.status, 200);
});
