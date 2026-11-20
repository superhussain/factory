// Only a signed-in person sets the spec-approval note "spec approved on the dashboard", and
// no write sets "spec approved in Tracer" unless the row's tracer.scope is approved. A device
// also needs a Tracer-signed scope receipt for the row's ticket. Rows are
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

// A receipt as Tracer signs it, for factory-42 approved at 09:30 unless fields say otherwise.
async function signed(fields = {}) {
  const r = { feature: "factory-42", ticket: "FACTORY-42", approved_at: "2026-11-20T09:30:00Z" };
  Object.assign(r, fields);
  const message = ["tracer-scope-approval/v1", r.feature, r.ticket, r.approved_at].join("\n");
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

function setup(row = ROW, key = publicKey) {
  const bucket = memoryBucket();
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
  return { etag, put, patch };
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

test("a device PUT cannot edit a receipt after signing", async () => {
  const s = setup();
  const receipt = { ...(await signed()), approved_at: "2026-11-20T09:31:00Z" };
  await refused(s, () => s.put(withReceipt(receipt)), receiptRefusal);
});

test("a device PUT cannot set the Tracer note when no receipt key is configured", async () => {
  const s = setup(ROW, null);
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
