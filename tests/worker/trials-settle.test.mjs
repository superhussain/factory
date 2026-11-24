// Open stage-note trials: a board write may change one only the way trial settle does.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test, { mock } from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

mock.timers.enable({ apis: ["Date"], now: Date.parse("2026-11-22T09:00:00Z") });

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const hostA = { verified: true, kind: "service", client_id: "client-host-a" };
const json = { "Content-Type": "application/json" };
const NOW = "2026-11-22T09:00:00Z";

// The running example: factory-spec-v10 opened 21 days ago and is open.
const V10 = {
  id: "factory-spec-v10",
  project: "factory",
  stage: "spec",
  version: 10,
  champion_version: 9,
  opened: "2026-11-01T09:00:00Z",
  challenger: "Spec note v10",
  status: "open",
};

// trial_arm: SHA-256 of <trial id>/<job id>, first 4 bytes big-endian, mod 100, below 50 is b.
const arm = (jobId) =>
  createHash("sha256").update(`${V10.id}/${jobId}`).digest().readUInt32BE(0) % 100 < 50 ? "b" : "a";
const ids = { a: [], b: [] };
for (let i = 1; ids.a.length < 5 || ids.b.length < 5; i++)
  ids[arm(`factory-${i}-job`)].push(`factory-${i}-job`);
const finished = (id) => ({
  id,
  project: "factory",
  title: id,
  stage: "merged",
  started: "2026-11-05T09:00:00Z",
});
const FIVE_AND_FIVE = [...ids.a.slice(0, 5), ...ids.b.slice(0, 5)].map(finished);
const FIVE_AND_FOUR = [
  ...ids.a.slice(0, 5).map(finished),
  ...ids.b.slice(0, 4).map(finished),
  { ...finished(ids.b[4]), stage: "build" },
];

async function setup({ trials = [V10], jobs = [] } = {}) {
  const bucket = memoryBucket();
  const send = async (method, path, { body, principal = person, headers = {} } = {}) => {
    const init = { method, headers };
    if (body !== undefined) init.body = body;
    const response = await serveFactory(
      new Request("http://local" + path, init),
      { BUCKET: bucket },
      principal,
    );
    return { status: response.status, text: await response.text() };
  };
  bucket.objects.set("projects/factory.md", {
    key: "projects/factory.md",
    bytes: Buffer.from("---\nrepo: example/factory\nbase: main\ntitle: Factory\n---\n"),
    httpEtag: '"p"',
  });
  const created = await send("POST", "/api/hosts", {
    body: JSON.stringify({ id: "host-a", label: "host-a" }),
    headers: json,
  });
  const { code } = JSON.parse(created.text);
  const paired = await send("POST", "/api/hosts/host-a/pair", {
    body: JSON.stringify({ code }),
    headers: json,
    principal: hostA,
  });
  assert.equal(paired.status, 200);
  await bucket.put("board.json", JSON.stringify({ updated: "2026-11-22T08:00:00Z", jobs, trials }));
  const board = async () => {
    const obj = await bucket.get("board.json");
    return { etag: obj.httpEtag, doc: JSON.parse(await obj.text()) };
  };
  const put = async (change, principal = hostA) => {
    const { etag, doc } = await board();
    return send("PUT", "/api/board", {
      body: JSON.stringify({ ...doc, trials: change(doc.trials) }),
      headers: { ...json, "If-Match": etag },
      principal,
    });
  };
  const settle = (fields) =>
    put((ts) => ts.map((t) => (t.id === V10.id ? { ...t, ...fields } : t)));
  const note = (spec) =>
    bucket.put(
      "pipeline/projects/factory.json",
      JSON.stringify({ version: 10, stage_notes: { spec } }),
    );
  return { bucket, board, put, settle, note };
}

const LABEL = '"factory-spec-v10"';
const SETTLE = (status, why) =>
  `a board write cannot settle the open trial ${LABEL} as ${status}: ${why}. Nothing was written.`;
const SETTLED = { settled: NOW };
const ARMS = { a: [], b: [] };
const READS = { rounds: "no difference", tokens: "no difference" };

test("a board write cannot drop an open trial or change its other keys", async () => {
  const { board, put, settle } = await setup();
  const before = await board();
  const dropped = await put((ts) => ts.filter((t) => t.id !== V10.id));
  assert.deepEqual(
    [dropped.status, dropped.text],
    [
      400,
      `a board write cannot drop the open trial ${LABEL}: only trial settle moves an open stage-note trial. Nothing was written.`,
    ],
  );
  const changed = await settle({ challenger: "Another note", status: "kept", ...SETTLED });
  assert.deepEqual(
    [changed.status, changed.text],
    [
      400,
      `a board write cannot change "challenger" on the open trial ${LABEL}: trial settle sets only status, settled, arms, reads and revert_version. Nothing was written.`,
    ],
  );
  const long = await settle({ ["k".repeat(60)]: 1 });
  assert.deepEqual(
    [long.status, long.text],
    [
      400,
      `a board write cannot change "${"k".repeat(31)}..." on the open trial ${LABEL}: trial settle sets only status, settled, arms, reads and revert_version. Nothing was written.`,
    ],
  );
  const unknown = await settle({ status: "done", ...SETTLED });
  assert.deepEqual(
    [unknown.status, unknown.text],
    [
      400,
      `a board write cannot set the open trial ${LABEL} to an unknown status: trial settle uses kept, reverted, superseded, inconclusive or revert-failed. Nothing was written.`,
    ],
  );
  assert.deepEqual(await board(), before);
});

test("kept needs 5 finished jobs per arm and no b higher read", async () => {
  const short = await setup({ jobs: FIVE_AND_FOUR });
  const few = await short.settle({ status: "kept", ...SETTLED, arms: ARMS, reads: READS });
  assert.deepEqual(
    [few.status, few.text],
    [
      400,
      SETTLE("kept", "each arm needs 5 finished jobs on the board, and arm a has 5, arm b has 4"),
    ],
  );
  const { board, settle } = await setup({ jobs: FIVE_AND_FIVE });
  const higher = await settle({
    status: "kept",
    ...SETTLED,
    reads: { rounds: "b higher", tokens: "no difference" },
  });
  assert.deepEqual(
    [higher.status, higher.text],
    [400, SETTLE("kept", "reads.rounds and reads.tokens must each be a read other than b higher")],
  );
  const stamp = await settle({ status: "kept", settled: "today", reads: READS });
  assert.deepEqual(
    [stamp.status, stamp.text],
    [400, SETTLE("kept", "settled must be a board timestamp")],
  );
  const kept = await settle({ status: "kept", ...SETTLED, reads: READS });
  assert.equal(kept.status, 200);
  assert.equal((await board()).doc.trials[0].status, "kept");
});

test("inconclusive and revert-failed wait 28 days from opened", async () => {
  const { settle } = await setup();
  for (const status of ["inconclusive", "revert-failed"]) {
    const early = await settle({ status, ...SETTLED });
    assert.deepEqual(
      [early.status, early.text],
      [400, SETTLE(status, "it opened less than 28 days ago")],
    );
  }
  const old = await setup({ trials: [{ ...V10, opened: "2026-10-24T09:00:00Z" }] });
  assert.equal((await old.settle({ status: "inconclusive", ...SETTLED })).status, 200);
});

test("superseded needs the stored stage note to differ from the challenger", async () => {
  const { bucket, settle, note } = await setup();
  await note("Spec note v10");
  const same = await settle({ status: "superseded", ...SETTLED });
  assert.deepEqual(
    [same.status, same.text],
    [400, SETTLE("superseded", "the stored stage note still equals the challenger")],
  );
  await bucket.put("pipeline/projects/factory.json", "{not json");
  const unreadable = await settle({ status: "superseded", ...SETTLED });
  assert.deepEqual(
    [unreadable.status, unreadable.text],
    [400, SETTLE("superseded", "the stored pipeline for its project cannot be read")],
  );
  await note("Spec note v11");
  assert.equal((await settle({ status: "superseded", ...SETTLED })).status, 200);
});

test("reverted needs a revert_version, and an open trial takes new arms and reads", async () => {
  const { board, settle } = await setup();
  const bare = await settle({ status: "reverted", ...SETTLED });
  assert.deepEqual(
    [bare.status, bare.text],
    [400, SETTLE("reverted", "revert_version must be a whole number above the trial's version")],
  );
  const open = await settle({
    arms: ARMS,
    reads: { rounds: "inconclusive", tokens: "inconclusive" },
  });
  assert.equal(open.status, 200);
  const settled = await settle({ status: "open", ...SETTLED });
  assert.deepEqual(
    [settled.status, settled.text],
    [
      400,
      `a board write cannot change "settled" on the open trial ${LABEL}: trial settle sets only status, settled, arms, reads and revert_version. Nothing was written.`,
    ],
  );
  const reverted = await settle({ status: "reverted", ...SETTLED, revert_version: 11 });
  assert.equal(reverted.status, 200);
  assert.equal((await board()).doc.trials[0].revert_version, 11);
});
