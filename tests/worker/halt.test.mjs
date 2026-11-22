import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const deviceA = { verified: true, kind: "service", client_id: "client-aaaa" };
const anonymousService = { verified: true, kind: "service", client_id: null };
const HALT_SHAPE = "a halt needs {halted: true, reason: 1-200 characters} or {halted: false}";
const HALT_PERSON_ONLY = "only a signed-in person can resume the factory";
const HALT_UNREADABLE =
  "the stored halt record is unreadable; resume from the dashboard to rewrite it";
const STOP = { halted: true, reason: "Runaway launches on wsl" };

function setup() {
  const bucket = memoryBucket();
  const send = async (method, path, { body, principal = person } = {}) => {
    const init = { method, headers: {} };
    if (body !== undefined) {
      init.headers["Content-Type"] = "application/json";
      init.body = typeof body === "string" ? body : JSON.stringify(body);
    }
    const response = await serveFactory(
      new Request("http://local" + path, init),
      { BUCKET: bucket },
      principal,
    );
    return { status: response.status, text: await response.text() };
  };
  const halt = (body, principal) => send("PUT", "/api/halt", { body, principal });
  const stored = () => bucket.objects.get("halt.json")?.bytes.toString();
  const puts = () => bucket.calls.filter(([op, key]) => op === "put" && key === "halt.json").length;
  return { bucket, send, halt, stored, puts };
}

test("GET with no stored record is not halted", async () => {
  const s = setup();
  const r = await s.send("GET", "/api/halt");
  assert.equal(r.status, 200);
  assert.deepEqual(JSON.parse(r.text), { halted: false });
});

test("a person sets and clears the halt", async () => {
  const s = setup();
  const set = await s.halt(STOP);
  assert.equal(set.status, 200);
  const record = JSON.parse(s.stored());
  assert.deepEqual(JSON.parse(set.text), record);
  assert.equal(record.halted, true);
  assert.equal(record.reason, "Runaway launches on wsl");
  assert.equal(record.by_kind, "person");
  assert.equal(record.by, "person@example.test");
  assert.match(record.at, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
  const clear = await s.halt({ halted: false });
  assert.equal(clear.status, 200);
  const cleared = JSON.parse(s.stored());
  assert.equal(cleared.halted, false);
  assert.equal("reason" in cleared, false);
  assert.equal(cleared.by_kind, "person");
});

test("a service set names its bound host, else device, and a repeat set writes nothing", async () => {
  const s = setup();
  const created = await s.send("POST", "/api/hosts", { body: { id: "wsl", label: "Host wsl" } });
  const { code } = JSON.parse(created.text);
  assert.equal(
    (await s.send("POST", "/api/hosts/wsl/pair", { body: { code }, principal: deviceA })).status,
    200,
  );
  assert.equal((await s.halt(STOP, deviceA)).status, 200);
  const record = JSON.parse(s.stored());
  assert.equal(record.by_kind, "host");
  assert.equal(record.by, "wsl");

  const bytes = s.stored();
  const puts = s.puts();
  const again = await s.halt({ halted: true, reason: "A second cause" }, deviceA);
  assert.equal(again.status, 200);
  assert.equal(JSON.parse(again.text).reason, "Runaway launches on wsl");
  assert.equal(s.stored(), bytes);
  assert.equal(s.puts(), puts);

  const clear = await s.halt({ halted: false }, deviceA);
  assert.equal(clear.status, 403);
  assert.equal(clear.text, HALT_PERSON_ONLY);
  assert.equal(s.stored(), bytes);
});

test("an unbound service token stops the factory as device", async () => {
  const s = setup();
  assert.equal((await s.halt(STOP, anonymousService)).status, 200);
  const record = JSON.parse(s.stored());
  assert.equal(record.by_kind, "host");
  assert.equal(record.by, "device");
});

test("a bad body gets 400 and writes nothing", async () => {
  const s = setup();
  for (const body of [
    {},
    { halted: true },
    { halted: true, reason: "" },
    { halted: true, reason: "x".repeat(201) },
    { halted: false, reason: "x" },
    { halted: true, reason: "line one\nline two" },
    [],
    "not json",
  ]) {
    const r = await s.halt(body);
    assert.equal(r.status, 400, JSON.stringify(body));
    assert.equal(r.text, HALT_SHAPE);
  }
  assert.equal(s.puts(), 0);
});

test("a service GET hides a person's email", async () => {
  const s = setup();
  await s.halt(STOP);
  const r = JSON.parse((await s.send("GET", "/api/halt", { principal: deviceA })).text);
  assert.equal(r.halted, true);
  assert.equal(r.by_kind, "person");
  assert.equal("by" in r, false);
  assert.equal(JSON.parse((await s.send("GET", "/api/halt")).text).by, "person@example.test");
});

test("an unreadable stored record reads as halted", async () => {
  const s = setup();
  await s.bucket.put("halt.json", "not json");
  const r = await s.send("GET", "/api/halt");
  assert.equal(r.status, 200);
  assert.deepEqual(JSON.parse(r.text), { halted: true, reason: HALT_UNREADABLE });
});

test("other methods are not found", async () => {
  const s = setup();
  assert.equal((await s.send("DELETE", "/api/halt")).status, 404);
});
