// Live board events: the socket route guards, and one announcement per committed
// board or backlog write, with write replies unchanged.
import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const device = { verified: true, kind: "service", client_id: "client-aaaa" };
const ORIGIN = "https://factory.example.test";
const EVENTS = ORIGIN + "/events/board";
const upgrade = { Upgrade: "websocket", Origin: ORIGIN };
// The RFC 6455 sample nonce.
const NONCE = "dGhlIHNhbXBsZSBub25jZQ==";

// A BoardEvents namespace whose one stub records every call and answers with `answer`.
function fakeEvents(answer = () => new Response(null, { status: 204 })) {
  const calls = [];
  const requests = [];
  const stub = {
    setName: async (name) => calls.push(["setName", name]),
    fetch: async (input, init) => {
      const request = new Request(input, init);
      calls.push(["fetch", request.method, request.url]);
      requests.push(request);
      return answer(request);
    },
  };
  return {
    calls,
    requests,
    fetches: () => calls.filter(([kind]) => kind === "fetch"),
    idFromName: (name) => ({ name }),
    get: () => stub,
  };
}

const connect = (env, { method = "GET", headers = upgrade, principal = person } = {}) =>
  serveFactory(new Request(EVENTS, { method, headers }), env, principal);

test("no verified principal is refused before routing", async () => {
  const events = fakeEvents();
  const response = await serveFactory(
    new Request(EVENTS, { headers: upgrade }),
    { BUCKET: memoryBucket(), BoardEvents: events },
    { verified: false },
  );
  assert.equal(response.status, 403);
  assert.deepEqual(events.calls, []);
});

for (const [name, options] of [
  ["GET without Upgrade", { headers: { Origin: ORIGIN } }],
  ["POST", { method: "POST" }],
  ["a service token principal", { principal: device }],
]) {
  test(`${name} gets 404 and never reaches the room`, async () => {
    const events = fakeEvents();
    const response = await connect({ BUCKET: memoryBucket(), BoardEvents: events }, options);
    assert.equal(response.status, 404);
    assert.deepEqual(events.calls, []);
  });
}

for (const [name, origin] of [
  ["another Origin", "https://evil.example.test"],
  ["no Origin", null],
]) {
  test(`${name} gets 403`, async () => {
    const events = fakeEvents();
    const headers = { Upgrade: "websocket", ...(origin && { Origin: origin }) };
    const response = await connect({ BUCKET: memoryBucket(), BoardEvents: events }, { headers });
    assert.equal(response.status, 403);
    assert.deepEqual(events.calls, []);
  });
}

test("a person's same-origin upgrade is forwarded to the main room", async () => {
  const room = new Response("room reply", { status: 200, headers: { "X-Room": "main" } });
  const events = fakeEvents(() => room);
  const response = await connect({ BUCKET: memoryBucket(), BoardEvents: events });
  assert.equal(response, room);
  assert.deepEqual(events.calls, [
    ["setName", "main"],
    ["fetch", "GET", EVENTS],
  ]);
});

test("the room gets only the upgrade headers, never partyserver controls", async () => {
  const events = fakeEvents();
  await serveFactory(
    new Request(EVENTS + "?_pk=chosen-id", {
      headers: {
        ...upgrade,
        "Sec-WebSocket-Key": NONCE,
        "Sec-WebSocket-Version": "13",
        "x-partykit-props": "not-json",
        "x-partykit-room": "other",
        Cookie: "session=synthetic",
      },
    }),
    { BUCKET: memoryBucket(), BoardEvents: events },
    person,
  );
  const [forwarded] = events.requests;
  assert.equal(forwarded.url, EVENTS);
  assert.deepEqual([...forwarded.headers].sort(), [
    ["sec-websocket-key", NONCE],
    ["sec-websocket-version", "13"],
    ["upgrade", "websocket"],
  ]);
});

test("no BoardEvents binding gets 404", async () => {
  const response = await connect({ BUCKET: memoryBucket() });
  assert.equal(response.status, 404);
});

const writes = [
  ["/api/board", "board.json", JSON.stringify({ jobs: [] }), "application/json"],
  ["/api/backlog", "backlog.md", "# Backlog\n", "text/markdown"],
];

async function write(env, path, body, type, headers = { "If-None-Match": "*" }) {
  const response = await serveFactory(
    new Request(ORIGIN + path, {
      method: "PUT",
      headers: { "Content-Type": type, ...headers },
      body,
    }),
    env,
    device,
  );
  return {
    status: response.status,
    headers: [...response.headers],
    body: await response.text(),
  };
}

for (const [path, key, body, type] of writes) {
  test(`PUT ${path} announces ${key} once and replies as before`, async () => {
    const events = fakeEvents();
    const live = await write({ BUCKET: memoryBucket(), BoardEvents: events }, path, body, type);
    const plain = await write({ BUCKET: memoryBucket() }, path, body, type);
    assert.deepEqual(live, plain);
    assert.equal(live.status, 200);
    assert.deepEqual(events.fetches(), [
      ["fetch", "POST", "https://board-events.invalid/notify?key=" + key],
    ]);
  });

  test(`a 412 on ${path} announces nothing`, async () => {
    const events = fakeEvents();
    const env = { BUCKET: memoryBucket(), BoardEvents: events };
    await write(env, path, body, type);
    events.calls.length = 0;
    const conflict = await write(env, path, body, type);
    assert.equal(conflict.status, 412);
    assert.deepEqual(events.calls, []);
  });

  test(`a failed announcement leaves the ${path} reply unchanged`, async () => {
    const events = fakeEvents(() => {
      throw new Error("room unavailable");
    });
    const live = await write({ BUCKET: memoryBucket(), BoardEvents: events }, path, body, type);
    const plain = await write({ BUCKET: memoryBucket() }, path, body, type);
    assert.deepEqual(live, plain);
    assert.equal(events.fetches().length, 1);
  });
}
