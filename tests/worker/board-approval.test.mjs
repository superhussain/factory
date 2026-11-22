// Only a signed-in person sets the spec-approval note "spec approved on the dashboard", and
// no write sets "spec approved in Tracer" unless the row's tracer.scope is approved. A device
// also needs a Tracer-signed scope receipt for the row's job id and ticket. Rows are
// the made-up running example: 042-sample-export waits at needs-human on the spec-approval gate.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const device = { verified: true, kind: "service" };
const person = { verified: true, kind: "user", email: "person@example.test", exp: 4102444800 };

const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
const publicKey = Buffer.from(await crypto.subtle.exportKey("raw", keys.publicKey)).toString(
  "base64",
);

// A receipt as Tracer signs it, for 042-sample-export's factory-42 approved at 09:30 unless
// fields say otherwise.
async function signed(fields = {}, prefix = "tracer-scope-approval/v2") {
  const r = {
    job: "042-sample-export",
    feature: "factory-42",
    ticket: "FACTORY-42",
    approved_at: "2026-11-20T09:30:00Z",
  };
  Object.assign(r, fields);
  const head = prefix.endsWith("/v1") ? [prefix] : [prefix, r.job];
  const message = [...head, r.feature, r.ticket, r.approved_at].join("\n");
  const signature = await crypto.subtle.sign(
    { name: "Ed25519" },
    keys.privateKey,
    new TextEncoder().encode(message),
  );
  return { ...r, signature: Buffer.from(signature).toString("base64") };
}

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
const receiptRefusal =
  'a board write from a device cannot set the note "spec approved in Tracer" on "042-sample-export" without a Tracer-signed scope receipt for FACTORY-42. Nothing was written.';
const withReceipt = (receipt) => ({
  ...approved(TRACER),
  tracer: { scope: "approved", ...(receipt && { receipt }) },
});

const keptRefusal = (note) =>
  `a board write from a device cannot move "042-sample-export" into build while it keeps the note "${note}": only a person starts an approved spec again. Nothing was written.`;

function setup(row = ROW, { seed = true, key = publicKey } = {}) {
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
  const env = { BUCKET: bucket, BoardEvents, FACTORY_TRACER_RECEIPT_KEY: key };
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

test("a device PUT cannot set the Tracer note with an approved scope and no receipt", async () => {
  const s = setup();
  await refused(s, () => s.put(withReceipt(null)), receiptRefusal);
});

test("a device PUT sets the Tracer note with a signed receipt", async () => {
  const response = await setup().put(withReceipt(await signed()));
  assert.equal(response.status, 200);
});

test("a device PUT cannot reuse another job's receipt", async () => {
  const s = setup();
  const receipt = await signed({ feature: "factory-7", ticket: "FACTORY-7" });
  await refused(s, () => s.put(withReceipt(receipt)), receiptRefusal);
});

test("refuses a receipt replayed from another job", async () => {
  const row = { ...ROW, jira: "PLAN-7" };
  const s = setup(row);
  const receipt = await signed({ job: "007-sample-other", feature: "plan-7", ticket: "PLAN-7" });
  await refused(
    s,
    () => s.put({ ...withReceipt(receipt), jira: "PLAN-7" }),
    receiptRefusal.replace("FACTORY-42", "PLAN-7"),
  );
});

test("refuses a v1 receipt", async () => {
  const s = setup();
  const receipt = await signed({}, "tracer-scope-approval/v1");
  await refused(s, () => s.put(withReceipt(receipt)), receiptRefusal);
});

test("refuses a receipt without a job", async () => {
  const s = setup();
  const { job: _job, ...receipt } = await signed();
  await refused(s, () => s.put(withReceipt(receipt)), receiptRefusal);
});

test("a device PUT cannot edit a receipt after signing", async () => {
  const s = setup();
  const receipt = { ...(await signed()), approved_at: "2026-11-20T09:31:00Z" };
  await refused(s, () => s.put(withReceipt(receipt)), receiptRefusal);
});

test("a device PUT cannot set the Tracer note when no receipt key is configured", async () => {
  const s = setup(ROW, { key: null });
  await refused(s, async () => s.put(withReceipt(await signed())), receiptRefusal);
});

test("a person's PATCH sets the Tracer note with an approved scope and no receipt", async () => {
  const response = await setup().patch(withReceipt(null), person);
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

// Only a signed-in person appends a steering note while the job runs. Rows are the made-up
// running example: 114-steer-export sits at review, and a person adds one note at 14:00.
const STEER_ROW = { id: "114-steer-export", project: "sample", stage: "review" };
const NOTE = {
  note: "Keep the CSV header order the same as the old report.",
  at: "2026-01-02T14:00:00Z",
};
const steered = (steer, row = STEER_ROW) => ({ ...row, steer });
const deviceSteerRefusal =
  'a board write from a device cannot change steer on "114-steer-export": only a signed-in person adds a steering note on the dashboard. Nothing was written.';
const steerShapeRefusal =
  'steer on "114-steer-export" must keep its stored notes and add one {note, at} while the job is at spec, build or review, at most 10 notes. Nothing was written.';

test("a person's PATCH appends a steering note", async () => {
  assert.equal((await setup(STEER_ROW).patch(steered([NOTE]), person)).status, 200);
  const later = { note: "Name the file export.csv.", at: "2026-01-02T14:30:00Z" };
  const response = await setup(steered([NOTE])).patch(steered([NOTE, later]), person);
  assert.equal(response.status, 200);
});

test("a device PUT cannot change steer", async () => {
  const s = setup(STEER_ROW);
  await refused(s, () => s.put(steered([NOTE])), deviceSteerRefusal);
});

test("a device PATCH cannot change steer", async () => {
  const s = setup(STEER_ROW);
  await refused(s, () => s.patch(steered([NOTE]), device), deviceSteerRefusal);
});

test("a device PUT keeps a stored steer", async () => {
  const stored = steered([NOTE]);
  const response = await setup(stored).put({ ...stored, claim: { device: "host-a" } });
  assert.equal(response.status, 200);
});

test("a person's PATCH cannot add an 11th note", async () => {
  const ten = Array.from({ length: 10 }, (_, i) => ({ ...NOTE, note: `Note ${i}.` }));
  const s = setup(steered(ten));
  await refused(s, () => s.patch(steered([...ten, NOTE]), person), steerShapeRefusal);
});

test("a person's PATCH cannot edit a stored note", async () => {
  const s = setup(steered([NOTE]));
  const edited = { ...NOTE, note: "Drop the header row." };
  const later = { ...NOTE, at: "2026-01-02T14:30:00Z" };
  await refused(s, () => s.patch(steered([edited, later]), person), steerShapeRefusal);
});

test("a person's PATCH cannot add a note at merged", async () => {
  const merged = { ...STEER_ROW, stage: "merged" };
  const s = setup(merged);
  await refused(s, () => s.patch(steered([NOTE], merged), person), steerShapeRefusal);
});

test("a person's PATCH cannot add a malformed note", async () => {
  for (const bad of [
    { note: "   ", at: NOTE.at },
    { note: NOTE.note, at: "2026-01-02 14:00" },
    { note: NOTE.note },
    { ...NOTE, extra: true },
    null,
  ]) {
    const s = setup(STEER_ROW);
    await refused(s, () => s.patch(steered([bad]), person), steerShapeRefusal);
  }
  const s = setup(STEER_ROW);
  await refused(s, () => s.patch(steered(null), person), steerShapeRefusal);
});

test("a person's PATCH cannot add a note with a link", async () => {
  const s = setup(STEER_ROW);
  const linked = { ...NOTE, note: "Copy https://example.test/report." };
  await refused(
    s,
    () => s.patch(steered([linked]), person),
    'steer on "114-steer-export" note: links are not allowed. Nothing was written.',
  );
});

test("a person's PATCH cannot add steer to a rider", async () => {
  const rider = { ...STEER_ROW, refs: [{ id: "113-export-core", kind: "batched-into" }] };
  const s = setup(rider);
  await refused(
    s,
    () => s.patch(steered([NOTE], rider), person),
    'a board write cannot add steer to "114-steer-export": it is batched into "113-export-core" and follows its lead. Act on the lead instead. Nothing was written.',
  );
});
