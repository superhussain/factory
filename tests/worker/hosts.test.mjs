import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test, { mock } from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const deviceA = { verified: true, kind: "service", client_id: "client-aaaa" };
const deviceB = { verified: true, kind: "service", client_id: "client-bbbb" };
const anonymousService = { verified: true, kind: "service", client_id: null };
const status = (extra = {}) => ({
  os: "macOS 15.1",
  scheduler: "launchd",
  last_pickup: null,
  bundle: { commit: "abcdef1", tree: "1234567" },
  doctor: "ok",
  routes: [
    { name: "builder", provider: "claude", model: "claude-opus-5-5", effort: "medium" },
    { name: "review", provider: "codex", model: "gpt-x", limited_until: null },
  ],
  checkouts: ["factory", "sample_2"],
  ...extra,
});

function setup() {
  const bucket = memoryBucket();
  bucket.objects.set("projects/sample.md", {
    key: "projects/sample.md",
    bytes: Buffer.from("---\n---\n"),
    httpEtag: '"p"',
  });
  const replies = [];
  const send = async (method, path, { body, headers = {}, principal = person } = {}) => {
    const init = { method, headers: { ...headers } };
    if (body !== undefined) {
      init.headers["Content-Type"] ??= "application/json";
      init.body = typeof body === "string" ? body : JSON.stringify(body);
    }
    const response = await serveFactory(
      new Request("http://local" + path, init),
      { BUCKET: bucket },
      principal,
    );
    const text = await response.text();
    replies.push({ method, path, status: response.status, text });
    return { status: response.status, text, headers: response.headers };
  };
  const json = (r) => JSON.parse(r.text);
  const create = async (id, extra = {}) =>
    json(await send("POST", "/api/hosts", { body: { id, label: `Host ${id}`, ...extra } }));
  const record = (id) => bucket.objects.get(`hosts/${id}.json`);
  const host = (id) => JSON.parse(record(id).bytes.toString());
  const writes = () => bucket.calls.filter(([op]) => op === "put" || op === "delete").length;
  const pair = (id, code, principal = deviceA) =>
    send("POST", `/api/hosts/${id}/pair`, { body: { code }, principal });
  const patch = (id, body, etag = record(id).httpEtag) =>
    send("PATCH", `/api/hosts/${id}`, { body, headers: { "If-Match": etag } });
  const putStatus = (id, body = status(), principal = deviceA) =>
    send("PUT", `/api/hosts/${id}/status`, { body, principal });
  const code = (id, principal = person) =>
    send("POST", `/api/hosts/${id}/code`, { body: {}, principal });
  // The next conditional write to `key` finds the record changed, as if another request won.
  const loseRaceOn = (key) => {
    const put = bucket.put;
    bucket.put = async (k, value, options) => {
      if (k === key && options?.onlyIf?.has("If-Match")) {
        bucket.put = put;
        bucket.objects.get(key).httpEtag = `"lost-${bucket.calls.length}"`;
      }
      return put(k, value, options);
    };
  };
  return {
    bucket,
    replies,
    send,
    json,
    create,
    record,
    host,
    writes,
    pair,
    patch,
    putStatus,
    code,
    loseRaceOn,
  };
}

test("only a person creates, changes or issues a code for a host", async () => {
  const s = setup();
  const { code } = await s.create("work-mac");
  await s.pair("work-mac", code);
  const before = s.writes();
  const bytes = s.record("work-mac").bytes;
  for (const principal of [deviceA, deviceB, anonymousService, { verified: true }]) {
    const attempts = [
      s.send("POST", "/api/hosts", { body: { id: "new-host", label: "New" }, principal }),
      s.send("PATCH", "/api/hosts/work-mac", {
        body: { state: "active" },
        headers: { "If-Match": s.record("work-mac").httpEtag },
        principal,
      }),
      s.code("work-mac", principal),
    ];
    for (const r of await Promise.all(attempts)) {
      assert.equal(r.status, 403);
      assert.equal(r.text, "hosts are managed only by a signed-in person");
    }
  }
  assert.equal(s.writes(), before);
  assert.equal(s.bucket.objects.has("hosts/new-host.json"), false);
  assert.deepEqual(s.record("work-mac").bytes, bytes);
  assert.equal(s.host("work-mac").state, "paired");
});

test("a service token cannot pair a host that is not pending or write another host's status", async () => {
  const s = setup();
  for (const id of ["paired-host", "active-host", "retired-host"]) {
    const { code } = await s.create(id);
    assert.equal((await s.pair(id, code, { ...deviceA, client_id: `token-${id}` })).status, 200);
  }
  await s.patch("active-host", { state: "active" });
  await s.patch("retired-host", { state: "retired" });
  for (const id of ["paired-host", "active-host", "retired-host"]) {
    const bytes = s.record(id).bytes;
    const r = await s.pair(id, "ABCDE-FGHJK", deviceB);
    assert.equal(r.status, 403);
    assert.equal(r.text, "pairing code is wrong, used or expired");
    assert.deepEqual(s.record(id).bytes, bytes);
  }
  assert.equal((await s.pair("paired-host", "ABCDE-FGHJK", anonymousService)).status, 403);
  assert.equal((await s.pair("paired-host", "ABCDE-FGHJK", person)).status, 403);
  const bound = { ...deviceA, client_id: "token-paired-host" };
  for (const principal of [deviceA, deviceB, anonymousService, person]) {
    const r = await s.putStatus("paired-host", status(), principal);
    assert.equal(r.status, 403);
    assert.equal(r.text, "status is written only by the host's bound service token");
  }
  assert.equal((await s.putStatus("missing-host", status(), bound)).status, 403);
  assert.equal(s.bucket.objects.has("host-status/paired-host.json"), false);
  assert.equal((await s.putStatus("paired-host", status(), bound)).status, 200);
  assert.equal(
    (await s.putStatus("active-host", status(), { ...deviceA, client_id: "token-active-host" }))
      .status,
    200,
  );
});

test("the right code inside 15 minutes pairs; wrong, reused or expired codes leave the host as it was", async (t) => {
  const s = setup();
  const created = await s.send("POST", "/api/hosts", {
    body: { id: "work-mac", label: "Work Mac", max_inflight: 2 },
  });
  assert.equal(created.status, 201);
  const { host, code, expires_at } = s.json(created);
  assert.match(code, /^[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{5}-[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{5}$/);
  assert.equal(host.state, "pending");
  assert.equal(host.max_inflight, 2);
  assert.equal(s.host("work-mac").created_by, "person@example.test");
  const ttl = Date.parse(expires_at) - Date.now();
  assert.ok(ttl > 14 * 60_000 && ttl <= 15 * 60_000);
  assert.deepEqual(
    Object.keys(JSON.parse(s.bucket.objects.get("host-pairing/work-mac.json").bytes)),
    ["sha256", "expires_at"],
  );

  // A wrong code uses the code up and changes nothing.
  const pending = s.record("work-mac").bytes;
  const wrong = await s.pair("work-mac", "AAAAA-AAAAA");
  assert.equal(wrong.status, 403);
  assert.equal(wrong.text, "pairing code is wrong, used or expired");
  assert.deepEqual(s.record("work-mac").bytes, pending);
  assert.equal((await s.pair("work-mac", code)).status, 403);
  assert.deepEqual(s.record("work-mac").bytes, pending);

  // A fresh code pairs, typed in lower case without the dash.
  const fresh = s.json(await s.code("work-mac"));
  const ok = await s.pair("work-mac", fresh.code.replace("-", "").toLowerCase());
  assert.equal(ok.status, 200);
  assert.deepEqual(s.json(ok), { id: "work-mac", state: "paired" });
  assert.equal(s.host("work-mac").token, "client-aaaa");
  assert.equal(s.host("work-mac").state, "paired");
  assert.ok(s.host("work-mac").paired_at);
  assert.equal(s.bucket.objects.has("host-pairing/work-mac.json"), false);

  // Reusing it fails.
  const paired = s.record("work-mac").bytes;
  assert.equal((await s.pair("work-mac", fresh.code, deviceB)).status, 403);
  assert.deepEqual(s.record("work-mac").bytes, paired);

  // An expired code fails.
  const late = await s.create("late-host");
  mock.timers.enable({ apis: ["Date"], now: Date.now() + 15 * 60_000 + 1 });
  t.after(() => mock.timers.reset());
  const lateBytes = s.record("late-host").bytes;
  const expired = await s.pair("late-host", late.code, deviceB);
  assert.equal(expired.status, 403);
  assert.equal(expired.text, "pairing code is wrong, used or expired");
  assert.deepEqual(s.record("late-host").bytes, lateBytes);
  assert.equal((await s.pair("missing-host", late.code, deviceB)).status, 403);
  assert.equal((await s.pair("late-host", undefined, deviceB)).status, 403);
});

test("a paired host keeps its token and one token binds one live host", async () => {
  const s = setup();
  const one = await s.create("host-one");
  const two = await s.create("host-two");
  assert.equal((await s.pair("host-one", one.code, deviceA)).status, 200);
  const r = await s.pair("host-two", two.code, deviceA);
  assert.equal(r.status, 409);
  assert.equal(r.text, "this service token is already bound to another host");
  assert.equal(s.host("host-two").token, null);
  assert.equal(s.host("host-two").state, "pending");
  // The used code is gone; another device with a fresh code still pairs host-two.
  const again = s.json(await s.code("host-two"));
  assert.equal((await s.pair("host-one", again.code, deviceB)).status, 403);
  assert.equal(s.host("host-one").token, "client-aaaa");
  // Once host-one is retired, its token may bind another host.
  await s.patch("host-one", { state: "retired" });
  const three = s.json(await s.code("host-two"));
  assert.equal((await s.pair("host-two", three.code, deviceA)).status, 200);
  assert.equal(s.host("host-two").token, "client-aaaa");
});

test("retiring clears the token, voids the code and stops status writes", async () => {
  const s = setup();
  const { code } = await s.create("work-mac");
  await s.pair("work-mac", code);
  assert.equal((await s.patch("work-mac", { state: "active" })).status, 200);
  assert.equal((await s.putStatus("work-mac")).status, 200);
  const retired = await s.patch("work-mac", { state: "retired" });
  assert.equal(retired.status, 200);
  assert.equal(retired.headers.get("ETag"), s.record("work-mac").httpEtag);
  assert.equal(s.json(retired).token_tail, null);
  assert.equal(s.host("work-mac").token, null);
  assert.equal(s.host("work-mac").state, "retired");
  const r = await s.putStatus("work-mac");
  assert.equal(r.status, 403);
  assert.equal(r.text, "status is written only by the host's bound service token");

  // A pending host's outstanding code is voided by retiring it.
  const other = await s.create("spare");
  await s.patch("spare", { state: "retired" });
  assert.equal(s.bucket.objects.has("host-pairing/spare.json"), false);
  assert.equal((await s.pair("spare", other.code, deviceB)).status, 403);
  // A retired host returns to pending with a new code.
  const reissued = await s.code("work-mac");
  assert.equal(reissued.status, 201);
  assert.equal(s.host("work-mac").state, "pending");
  const second = await s.code("work-mac");
  assert.equal(second.status, 201);
  await s.pair("work-mac", s.json(second).code);
  const paired = await s.code("work-mac");
  assert.equal(paired.status, 409);
  assert.equal(paired.text, "host is paired");
});

test("codes never reach a log, storage or any reply but the two that issue them", async () => {
  const source = await readFile(new URL("../../worker/hosts.js", import.meta.url), "utf8");
  assert.doesNotMatch(source, /console\./);
  const s = setup();
  const logged = [];
  for (const name of ["log", "info", "warn", "error", "debug"])
    mock.method(console, name, (...args) => logged.push(args.join(" ")));
  try {
    const first = await s.create("work-mac");
    const codes = [first.code];
    await s.pair("work-mac", "WRONG-CODES");
    codes.push(s.json(await s.code("work-mac")).code);
    await s.pair("work-mac", codes[1]);
    await s.pair("work-mac", codes[1], deviceB);
    await s.patch("work-mac", { state: "active" });
    await s.putStatus("work-mac");
    await s.send("GET", "/api/hosts");
    await s.send("GET", "/api/hosts", { principal: deviceA });
    await s.send("GET", "/api/pipeline/effective/sample");
    const forms = codes.flatMap((code) => [code, code.replace("-", "")]);
    for (const reply of s.replies) {
      const issuing =
        reply.method === "POST" && (reply.path === "/api/hosts" || reply.path.endsWith("/code"));
      for (const form of forms)
        if (!issuing) assert.equal(reply.text.includes(form), false, reply.path);
    }
    for (const object of s.bucket.objects.values())
      for (const form of forms) assert.equal(object.bytes.toString().includes(form), false);
    for (const line of logged) for (const form of forms) assert.equal(line.includes(form), false);
  } finally {
    mock.restoreAll();
  }
});

test("PATCH and status reject fields their writer does not own", async () => {
  const s = setup();
  const { code } = await s.create("work-mac");
  await s.pair("work-mac", code);
  const bytes = s.record("work-mac").bytes;
  for (const [body, message] of [
    [{ id: "other" }, "id: unknown field"],
    [{ token: "client-bbbb" }, "token: unknown field"],
    [{ routes: [] }, "routes: unknown field"],
    [{ state: "paired" }, "state: cannot move from paired to paired"],
    [{ state: "pending" }, "state: cannot move from paired to pending"],
    [{ max_inflight: 6 }, "max_inflight: must be a whole number from 1 to 5"],
    [{ label: "" }, "label: must be 1 to 80 characters"],
  ]) {
    const r = await s.patch("work-mac", body);
    assert.equal(r.status, 400, JSON.stringify(body));
    assert.equal(r.text, message);
  }
  assert.deepEqual(s.record("work-mac").bytes, bytes);
  assert.equal((await s.patch("work-mac", { label: "New" }, '"stale"')).status, 412);
  assert.equal(
    (await s.send("PATCH", "/api/hosts/work-mac", { body: { label: "New" } })).status,
    428,
  );
  const ok = await s.patch("work-mac", { label: "Desk Mac", max_inflight: 3, state: "active" });
  assert.equal(ok.status, 200);
  assert.deepEqual(
    [s.host("work-mac").label, s.host("work-mac").max_inflight, s.host("work-mac").state],
    ["Desk Mac", 3, "active"],
  );

  for (const [body, message] of [
    [
      status({ checkouts: ["/Users/me/factory"] }),
      "checkouts: must be a list of at most 64 project slugs",
    ],
    [
      status({ checkouts: ["../factory"] }),
      "checkouts: must be a list of at most 64 project slugs",
    ],
    [status({ path: "/Users/me" }), "path: unknown field"],
    [status({ seen: new Date().toISOString() }), "seen: set by the server"],
    [
      status({ routes: [{ name: "builder", provider: "claude", model: "m", cwd: "/tmp" }] }),
      "routes[0].cwd: unknown field",
    ],
    [{ ...status(), os: undefined }, "os: required"],
  ]) {
    const r = await s.putStatus("work-mac", JSON.parse(JSON.stringify(body)));
    assert.equal(r.status, 400, JSON.stringify(body));
    assert.equal(r.text, message);
  }
  assert.equal(s.bucket.objects.has("host-status/work-mac.json"), false);
  const wrongType = await s.send("PUT", "/api/hosts/work-mac/status", {
    body: JSON.stringify(status()),
    headers: { "Content-Type": "text/plain" },
    principal: deviceA,
  });
  assert.equal(wrongType.status, 415);
  const large = await s.putStatus("work-mac", status({ os: "x".repeat(17 << 10) }));
  assert.equal(large.status, 413);
});

test("the effective view keeps only active hosts once any host is registered", async () => {
  const s = setup();
  const pickup = {
    enabled: true,
    hosts: ["active-host", "paired-host", "pending-host", "retired-host", "unregistered"],
  };
  const saved = await s.send("PUT", "/api/pipeline/global", {
    body: { schema: 1, pickup },
    headers: { "If-None-Match": "*" },
  });
  assert.equal(saved.status, 200);
  const effective = async () =>
    s.json(await s.send("GET", "/api/pipeline/effective/sample", { principal: deviceA })).pickup;
  assert.deepEqual(await effective(), pickup);

  await s.create("pending-host");
  assert.deepEqual(await effective(), { enabled: true, hosts: [] });
  for (const [id, token] of [
    ["paired-host", "token-p"],
    ["active-host", "token-a"],
    ["retired-host", "token-r"],
  ]) {
    const { code } = await s.create(id);
    await s.pair(id, code, { ...deviceA, client_id: token });
  }
  await s.patch("active-host", { state: "active" });
  await s.patch("retired-host", { state: "retired" });
  assert.deepEqual(await effective(), { enabled: true, hosts: ["active-host"] });
  const stored = s.json(await s.send("GET", "/api/pipeline/global"));
  assert.deepEqual(stored.pickup, pickup);
});

test("offline flips after 30 minutes without a status write", async (t) => {
  const start = Date.parse("2026-10-29T12:00:00.000Z");
  mock.timers.enable({ apis: ["Date"], now: start });
  t.after(() => mock.timers.reset());
  const s = setup();
  const { code } = await s.create("work-mac");
  await s.pair("work-mac", code);
  const list = async () => s.json(await s.send("GET", "/api/hosts", { principal: deviceB }));
  const [before] = await list();
  assert.equal(before.offline, true);
  assert.equal(before.status, null);
  assert.equal(before.token_tail, "aaaa");
  assert.equal("token" in before, false);
  assert.equal("created_by" in before, false);
  assert.equal(before.etag, s.record("work-mac").httpEtag);

  assert.equal((await s.putStatus("work-mac")).status, 200);
  const [online] = await list();
  assert.equal(online.offline, false);
  assert.deepEqual(online.status, { ...status(), seen: "2026-10-29T12:00:00.000Z" });

  // An unchanged status inside ten minutes is not rewritten.
  mock.timers.setTime(start + 9 * 60_000);
  const puts = () =>
    s.bucket.calls.filter(([op, key]) => op === "put" && key.startsWith("host-status/")).length;
  const writes = puts();
  assert.equal((await s.putStatus("work-mac")).status, 200);
  assert.equal(puts(), writes);
  mock.timers.setTime(start + 30 * 60_000);
  assert.equal((await list())[0].offline, false);
  mock.timers.setTime(start + 30 * 60_000 + 1);
  assert.equal((await list())[0].offline, true);
  assert.equal((await s.putStatus("work-mac")).status, 200);
  assert.equal(puts(), writes + 1);
  assert.equal((await list())[0].offline, false);
});

test("every other /api/hosts path and method is not found", async () => {
  const s = setup();
  await s.create("work-mac");
  for (const [method, path] of [
    ["GET", "/api/hosts/work-mac"],
    ["DELETE", "/api/hosts/work-mac"],
    ["PUT", "/api/hosts"],
    ["DELETE", "/api/hosts"],
    ["GET", "/api/hosts/work-mac/status"],
    ["POST", "/api/hosts/work-mac/routes"],
    ["PATCH", "/api/hosts/.hidden"],
    ["GET", "/api/hosts/"],
  ]) {
    assert.equal((await s.send(method, path)).status, 404, `${method} ${path}`);
  }
});

test("host replies never carry the bound token or the creator's email", async () => {
  const s = setup();
  const created = await s.create("work-mac");
  assert.equal("token" in created.host, false);
  assert.equal("created_by" in created.host, false);
  assert.equal(created.host.token_tail, null);
  await s.pair("work-mac", created.code);
  for (const body of [{ label: "Desk Mac" }, { state: "active" }, { state: "retired" }]) {
    const r = await s.patch("work-mac", body);
    assert.equal(r.status, 200);
    assert.equal(r.text.includes("client-aaaa"), false);
    assert.equal(r.text.includes("person@example.test"), false);
    const reply = s.json(r);
    assert.equal("token" in reply, false);
    assert.equal("created_by" in reply, false);
    assert.equal(reply.token_tail, body.state === "retired" ? null : "aaaa");
  }
  for (const reply of s.replies) {
    assert.equal(reply.text.includes("client-aaaa"), false, reply.path);
    assert.equal(reply.text.includes("person@example.test"), false, reply.path);
  }
});

test("two hosts paired at once by one token leave at most one bound", async () => {
  const s = setup();
  const one = await s.create("host-one");
  const two = await s.create("host-two");
  // Hold both host writes until each pair has passed its token check.
  const put = s.bucket.put;
  let waiting = [];
  s.bucket.put = async (key, value, options) => {
    if (waiting && key.startsWith("hosts/") && options?.onlyIf?.has("If-Match")) {
      await new Promise((resolve) => {
        waiting.push(resolve);
        if (waiting.length === 2) {
          waiting.forEach((release) => release());
          waiting = null;
        }
      });
    }
    return put(key, value, options);
  };
  const replies = await Promise.all([
    s.pair("host-one", one.code, deviceA),
    s.pair("host-two", two.code, deviceA),
  ]);
  assert.equal(replies.filter((r) => r.status === 200).length <= 1, true);
  for (const r of replies) if (r.status !== 200) assert.equal(r.status, 409);
  const bound = ["host-one", "host-two"].filter((id) => s.host(id).token === "client-aaaa");
  assert.equal(bound.length, replies.filter((r) => r.status === 200).length);
  for (const id of ["host-one", "host-two"])
    if (!bound.includes(id))
      assert.deepEqual([s.host(id).state, s.host(id).token], ["pending", null]);
});

test("pair and code answer a lost conditional write without changing the host", async () => {
  const s = setup();
  const { code } = await s.create("work-mac");
  s.loseRaceOn("hosts/work-mac.json");
  const lost = await s.pair("work-mac", code);
  assert.equal(lost.status, 403);
  assert.equal(lost.text, "pairing code is wrong, used or expired");
  assert.equal(s.host("work-mac").token, null);
  assert.equal(s.host("work-mac").state, "pending");

  s.loseRaceOn("hosts/work-mac.json");
  const reissue = await s.code("work-mac");
  assert.equal(reissue.status, 412);
  assert.equal(s.bucket.objects.has("host-pairing/work-mac.json"), false);
  const ok = await s.code("work-mac");
  assert.equal(ok.status, 201);
  assert.equal(ok.headers.get("ETag"), s.record("work-mac").httpEtag);
});

test("a code reissue needs a JSON body, so a cross-site form cannot void a code", async () => {
  const s = setup();
  const { code } = await s.create("work-mac");
  const form = await s.send("POST", "/api/hosts/work-mac/code", {
    body: "x=1",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
  });
  assert.equal(form.status, 415);
  assert.equal((await s.send("POST", "/api/hosts/work-mac/code")).status, 415);
  assert.equal((await s.pair("work-mac", code)).status, 200);
});

test("retiring removes the pairing record before the host write", async () => {
  const s = setup();
  await s.create("work-mac");
  s.loseRaceOn("hosts/work-mac.json");
  assert.equal((await s.patch("work-mac", { state: "retired" })).status, 412);
  assert.equal(s.bucket.objects.has("host-pairing/work-mac.json"), false);
});

test("a label edit lands in the host's history with who saved it", async () => {
  const s = setup();
  const { code } = await s.create("work-mac");
  await s.pair("work-mac", code);
  const edited = await s.patch("work-mac", { label: "Desk Mac" });
  assert.equal(edited.status, 200);
  assert.equal(s.json(edited).version, 2);
  const r = await s.send("GET", "/api/hosts/work-mac/history");
  assert.equal(r.status, 200);
  const history = s.json(r);
  assert.deepEqual(
    history.map((entry) => [entry.version, entry.host.label, entry.saved_by]),
    [
      [2, "Desk Mac", "person@example.test"],
      [1, "Host work-mac", "person@example.test"],
    ],
  );
  assert.ok(history.every((entry) => Date.parse(entry.saved_at) > 0));
  assert.equal(r.text.includes("client-aaaa"), false);
  // A history copy is create-only, and pairing is not a person write.
  const copies = [...s.bucket.objects.keys()].filter((key) => key.startsWith("host-history/"));
  assert.deepEqual(copies, [
    "host-history/work-mac/000001.json",
    "host-history/work-mac/000002.json",
  ]);
  for (const key of copies)
    assert.equal(s.bucket.objects.get(key).options.onlyIf.get("If-None-Match"), "*");
});

test("a service token cannot read a host's history", async () => {
  const s = setup();
  await s.create("work-mac");
  for (const principal of [deviceA, anonymousService, { verified: true }]) {
    const r = await s.send("GET", "/api/hosts/work-mac/history", { principal });
    assert.equal(r.status, 403);
    assert.equal(r.text.includes("person@example.test"), false);
  }
});

test("the host list and the effective view name active hosts by id and label", async () => {
  const s = setup();
  for (const [id, state] of [
    ["active-host", "active"],
    ["paired-host", "paired"],
    ["pending-host", "pending"],
    ["retired-host", "retired"],
  ]) {
    const { code } = await s.create(id);
    if (state === "pending") continue;
    await s.pair(id, code, { ...deviceA, client_id: `token-${id}` });
    if (state !== "paired") await s.patch(id, { state });
  }
  await s.patch("active-host", { label: "Work Mac" });
  const list = s.json(await s.send("GET", "/api/hosts", { principal: deviceA }));
  assert.deepEqual(
    list.map(({ id, label, state }) => [id, label, state]),
    [
      ["active-host", "Work Mac", "active"],
      ["paired-host", "Host paired-host", "paired"],
      ["pending-host", "Host pending-host", "pending"],
      ["retired-host", "Host retired-host", "retired"],
    ],
  );
  const view = s.json(
    await s.send("GET", "/api/pipeline/effective/sample", { principal: deviceA }),
  );
  assert.deepEqual(view.active_hosts, [{ id: "active-host", label: "Work Mac" }]);
});
