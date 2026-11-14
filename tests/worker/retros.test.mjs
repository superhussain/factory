import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const mac = { verified: true, kind: "service", client_id: "client-mac" };
const pc = { verified: true, kind: "service", client_id: "client-pc" };
const RUN = "20261112T090000Z-a1b2c3";
const JSON_TYPE = "application/json";

const card = (over = {}) => ({
  title: "Guard the ls alias",
  project: "sample-tools",
  fingerprint: "0123456789abcdef",
  result: "admitted",
  ...over,
});
const finding = (over = {}) => ({
  rank: 1,
  title: "ls alias crashes in non-interactive shells",
  category: "tool-economy",
  count: 253,
  cost: 250,
  project: "sample-tools",
  root_cause: "The colorls alias needs a terminal size.",
  fix: "Alias ls only in interactive shells.",
  cards: [card()],
  ...over,
});
const record = (over = {}) => ({
  schema: "factory.retro-run/1",
  run: RUN,
  host: "mac",
  from: "2026-11-05",
  to: "2026-11-11",
  sessions: 40,
  rejected: 0,
  findings: [finding()],
  cards: [],
  ...over,
});

async function setup() {
  const bucket = memoryBucket();
  const send = async (method, path, { body, principal = person, type } = {}) => {
    const init = { method, headers: type ? { "Content-Type": type } : {} };
    if (body !== undefined) init.body = body;
    const response = await serveFactory(
      new Request("http://local" + path, init),
      { BUCKET: bucket },
      principal,
    );
    return { status: response.status, text: await response.text() };
  };
  for (const [id, principal] of [
    ["mac", mac],
    ["pc", pc],
  ]) {
    const created = await send("POST", "/api/hosts", {
      body: JSON.stringify({ id, label: id }),
      type: JSON_TYPE,
    });
    const { code } = JSON.parse(created.text);
    const paired = await send("POST", `/api/hosts/${id}/pair`, {
      body: JSON.stringify({ code }),
      type: JSON_TYPE,
      principal,
    });
    assert.equal(paired.status, 200);
  }
  const put = (body, principal = mac, run = RUN) =>
    send("PUT", `/api/retros/${run}`, {
      body: typeof body === "string" ? body : JSON.stringify(body),
      principal,
      type: JSON_TYPE,
    });
  return { bucket, send, put };
}

test("a bound host stores its run and any verified principal lists it", async () => {
  const s = await setup();
  const r = await s.put(record());
  assert.deepEqual([r.status, r.text], [200, "ok"]);
  assert.ok(s.bucket.objects.has(`retro-runs/${RUN}.json`));
  assert.equal(
    s.bucket.objects.get(`retro-runs/${RUN}.json`).httpMetadata.contentType,
    "application/json",
  );
  for (const principal of [person, mac, pc]) {
    const listed = await s.send("GET", "/api/retros", { principal });
    assert.equal(listed.status, 200);
    assert.deepEqual(JSON.parse(listed.text), [record()]);
  }
  assert.equal((await s.put(record({ rejected: 2 }))).status, 200);
  assert.equal(JSON.parse((await s.send("GET", "/api/retros")).text)[0].rejected, 2);
});

test("the list is newest first and holds at most 30 runs", async () => {
  const s = await setup();
  for (let n = 0; n < 32; n += 1) {
    const run = `20261101T0000${String(n).padStart(2, "0")}Z-aaaaaa`;
    assert.equal((await s.put(record({ run }), mac, run)).status, 200);
  }
  const listed = JSON.parse((await s.send("GET", "/api/retros")).text);
  assert.equal(listed.length, 30);
  assert.equal(listed[0].run, "20261101T000031Z-aaaaaa");
  assert.equal(listed[29].run, "20261101T000002Z-aaaaaa");
});

test("a run deleted between list and get is skipped", async () => {
  const s = await setup();
  const older = "20261101T000000Z-aaaaaa";
  assert.equal((await s.put(record({ run: older }), mac, older)).status, 200);
  assert.equal((await s.put(record())).status, 200);
  const get = s.bucket.get.bind(s.bucket);
  s.bucket.get = async (key) => (key === `retro-runs/${older}.json` ? null : get(key));
  const listed = await s.send("GET", "/api/retros");
  assert.equal(listed.status, 200);
  assert.deepEqual(
    JSON.parse(listed.text).map((r) => r.run),
    [RUN],
  );
});

test("the scheduled pass deletes retro runs uploaded more than 90 days ago", async () => {
  const s = await setup();
  const runs = {
    "20260801T090000Z-aaaaaa": "2026-08-14T23:00:00Z",
    "20260802T090000Z-bbbbbb": "2026-08-15T00:00:00Z",
    [RUN]: "2026-11-12T09:00:00Z",
  };
  for (const [run, uploaded] of Object.entries(runs)) {
    assert.equal((await s.put(record({ run }), mac, run)).status, 200);
    s.bucket.objects.get(`retro-runs/${run}.json`).uploaded = new Date(uploaded);
  }
  await s.bucket.put("retro/mac/2026-11-12.jsonl", "{}\n");
  const pending = [];
  (await loadProduction()).default.scheduled(
    { scheduledTime: Date.parse("2026-11-13T12:00:00Z") },
    { BUCKET: s.bucket },
    { waitUntil: (p) => pending.push(p) },
  );
  assert.deepEqual(await pending[2], {
    status: "pruned",
    deleted: ["retro-runs/20260801T090000Z-aaaaaa.json"],
  });
  await Promise.all(pending);
  assert.deepEqual([...s.bucket.objects.keys()].filter((k) => k.startsWith("retro")).sort(), [
    "retro-runs/20260802T090000Z-bbbbbb.json",
    `retro-runs/${RUN}.json`,
    "retro/mac/2026-11-12.jsonl",
  ]);
});

test("only the bound host writes, and only its own record", async () => {
  const s = await setup();
  const stranger = { ...pc, client_id: "client-other" };
  for (const principal of [person, stranger, { ...mac, client_id: null }]) {
    const r = await s.put(record(), principal);
    assert.deepEqual(
      [r.status, r.text],
      [403, "retro runs are written only by a bound host's service token"],
    );
  }
  assert.equal((await s.put(record({ host: "pc" }))).status, 400);
  assert.equal((await s.put(record({ host: "pc" }), pc)).status, 200);
  assert.equal((await s.send("DELETE", `/api/retros/${RUN}`, { principal: mac })).status, 404);
  assert.equal((await s.send("PUT", "/api/retros", { principal: mac })).status, 404);
  assert.equal((await s.send("GET", `/api/retros/${RUN}`)).status, 404);
});

test("another host cannot replace a stored run", async () => {
  const s = await setup();
  assert.equal((await s.put(record())).status, 200);
  const r = await s.put(record({ host: "pc" }), pc);
  assert.deepEqual([r.status, r.text], [409, "retro run belongs to another host"]);
  assert.equal(JSON.parse(s.bucket.objects.get(`retro-runs/${RUN}.json`).bytes).host, "mac");
  assert.equal((await s.put(record({ rejected: 2 }))).status, 200);
});

test("a write that loses the race to another host answers 409", async () => {
  const s = await setup();
  assert.equal((await s.put(record({ host: "pc" }), pc)).status, 200);
  const get = s.bucket.get.bind(s.bucket);
  s.bucket.get = async (key) => (key === `retro-runs/${RUN}.json` ? null : get(key));
  const r = await s.put(record());
  assert.deepEqual([r.status, r.text], [409, "retro run belongs to another host"]);
});

test("project slugs follow the runtime's Unicode rule", async () => {
  const s = await setup();
  for (const project of ["café-工具", "x".repeat(80)]) {
    const r = await s.put(record({ findings: [finding({ project, cards: [card({ project })] })] }));
    assert.equal(r.status, 200, project);
  }
  assert.equal(
    (await s.put(record({ findings: [finding({ project: "no spaces" })] }))).status,
    400,
  );
});

test("a malformed record is refused with 400", async () => {
  const s = await setup();
  const long = "x".repeat(81);
  const bad = [
    { ...record(), extra: 1 },
    record({ schema: "factory.retro-run/2" }),
    record({ run: "other" }),
    record({ from: "2026-13-01" }),
    record({ sessions: -1 }),
    record({ rejected: 1.5 }),
    record({ findings: [] }),
    record({ findings: Array.from({ length: 13 }, (_, n) => finding({ rank: n + 1 })) }),
    record({ findings: [finding({ rank: 13 })] }),
    record({ findings: [finding({ title: long })] }),
    record({ findings: [finding({ title: "two\nlines" })] }),
    record({ findings: [finding({ category: "speed" })] }),
    record({ findings: [finding({ count: 0 })] }),
    record({ findings: [finding({ cost: 1_000_000_001 })] }),
    record({ findings: [finding({ project: "no spaces" })] }),
    record({ findings: [finding({ fix: "" })] }),
    record({ findings: [finding({ extra: 1 })] }),
    record({ findings: [finding({ cards: Array.from({ length: 6 }, () => card()) })] }),
    record({ findings: [finding({ cards: [card({ fingerprint: "XYZ" })] })] }),
    record({ findings: [finding({ cards: [card({ result: "y".repeat(121) })] })] }),
    record({ findings: [finding({ cards: [{ ...card(), extra: 1 }] })] }),
    record({ cards: [card({ title: "" })] }),
    record({ cards: Array.from({ length: 6 }, () => card()) }),
    [],
    "not json",
  ];
  for (const body of bad) {
    const r = await s.put(body);
    assert.equal(r.status, 400, JSON.stringify(body).slice(0, 80));
  }
  assert.equal([...s.bucket.objects.keys()].filter((k) => k.startsWith("retro-runs/")).length, 0);
  const wrongType = await s.send("PUT", `/api/retros/${RUN}`, {
    body: JSON.stringify(record()),
    principal: mac,
    type: "text/plain",
  });
  assert.equal(wrongType.status, 415);
});

test("a body over 64 KiB is refused with 413", async () => {
  const s = await setup();
  const r = await s.put(JSON.stringify(record({ pad: "x".repeat(65536) })));
  assert.equal(r.status, 413);
});
