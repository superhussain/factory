// Stage-note trial pauses: a board write cannot resume, change or drop an unresumed revert, and
// POST /api/trials/resume/<slug> resumes for a person or a paired host passing an instruction.
import assert from "node:assert/strict";
import test, { mock } from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

mock.timers.enable({ apis: ["Date"], now: Date.parse("2026-11-22T09:00:00Z") });

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const hostA = { verified: true, kind: "service", client_id: "client-host-a" };
const unbound = { verified: true, kind: "service", client_id: "client-other" };
const json = { "Content-Type": "application/json" };
const INSTRUCTION = "Owner asked in chat to resume factory trials";

// The running example: project factory has two unresumed reverts and one open trial.
const trial = (version, fields) => ({
  id: `factory-spec-v${version}`,
  project: "factory",
  stage: "spec",
  version,
  champion_version: version - 1,
  opened: "2026-11-01T09:00:00Z",
  ...fields,
});
const V5 = trial(5, { status: "reverted", settled: "2026-11-10T09:00:00Z", revert_version: 6 });
const V8 = trial(8, { status: "reverted", settled: "2026-11-20T09:00:00Z", revert_version: 9 });
const V10 = trial(10, { status: "open" });
const SAMPLE = { ...V5, id: "sample-spec-v5", project: "sample" };

async function setup(trials = [V5, V8, V10, SAMPLE]) {
  const bucket = memoryBucket();
  const send = async (method, path, { body, principal = person, headers = {} } = {}) => {
    const init = { method, headers };
    if (body !== undefined) init.body = body;
    const response = await serveFactory(
      new Request("http://local" + path, init),
      { BUCKET: bucket },
      principal,
    );
    return { status: response.status, text: await response.text(), headers: response.headers };
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
  await bucket.put(
    "board.json",
    JSON.stringify({ updated: "2026-11-22T08:00:00Z", jobs: [], trials }),
  );
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
  const resume = (body, { principal = hostA, slug = "factory", method = "POST" } = {}) =>
    send(method, `/api/trials/resume/${slug}`, {
      body: method === "GET" ? undefined : JSON.stringify(body),
      headers: json,
      principal,
    });
  return { bucket, send, board, put, resume };
}

const LOCKED = (id) =>
  `a board write cannot change or drop the unresumed reverted trial "${id}": only POST /api/trials/resume clears a stage-note trial pause. Nothing was written.`;
const SETS = (label) =>
  `a board write cannot set resumed or resumed_by on trial ${label}: only POST /api/trials/resume clears a stage-note trial pause. Nothing was written.`;

test("a board write cannot resume, change or drop an unresumed revert", async () => {
  const { board, put } = await setup();
  const before = await board();
  const cases = [
    [(ts) => ts.map((t) => ({ ...t, resumed: "2026-11-22T09:00:00Z" })), LOCKED("factory-spec-v5")],
    [(ts) => ts.filter((t) => t.id !== "factory-spec-v5"), LOCKED("factory-spec-v5")],
    [
      (ts) => ts.map((t) => (t.id === "factory-spec-v8" ? { ...t, revert_version: 7 } : t)),
      LOCKED("factory-spec-v8"),
    ],
    [() => [], LOCKED("factory-spec-v5")],
  ];
  for (const principal of [hostA, person])
    for (const [change, message] of cases) {
      const got = await put(change, principal);
      assert.deepEqual([got.status, got.text], [400, message]);
    }
  assert.deepEqual(await board(), before);
});

test("a board write cannot add resumed or resumed_by to any trial", async () => {
  const { put } = await setup();
  const v10 = await put((ts) =>
    ts.map((t) => (t.id === "factory-spec-v10" ? { ...t, resumed_by: { kind: "person" } } : t)),
  );
  assert.deepEqual([v10.status, v10.text], [400, SETS('"factory-spec-v10"')]);
  const bare = await put((ts) => [...ts, { resumed: "2026-11-22T09:00:00Z" }]);
  assert.deepEqual([bare.status, bare.text], [400, SETS("at trials[4]")]);
});

test("settle-style and admit-style board writes still pass", async () => {
  const { board, put, resume } = await setup();
  const settled = await put((ts) =>
    ts.map((t) =>
      t.id === "factory-spec-v10"
        ? { ...t, status: "reverted", settled: "2026-11-22T09:00:00Z" }
        : t,
    ),
  );
  assert.equal(settled.status, 200);
  const admitted = await put((ts) => [...ts, trial(11, { status: "open" })]);
  assert.equal(admitted.status, 200);
  // A resumed record passes unchanged, and settle may prune it.
  assert.equal((await resume({ instruction: INSTRUCTION })).status, 200);
  const kept = await put((ts) => ts.map((t) => ({ ...t })));
  assert.equal(kept.status, 200);
  const pruned = await put((ts) => ts.filter((t) => t.id !== "factory-spec-v5"));
  assert.equal(pruned.status, 200);
  assert.deepEqual(
    (await board()).doc.trials.map((t) => t.id),
    ["factory-spec-v8", "factory-spec-v10", "sample-spec-v5", "factory-spec-v11"],
  );
});

test("a bound host resumes with an instruction, recording who and why", async () => {
  const { board, resume } = await setup();
  const got = await resume({ instruction: `  ${INSTRUCTION}  ` });
  assert.deepEqual([got.status, JSON.parse(got.text)], [200, { resumed: 2 }]);
  const by = { kind: "host", by: "host-a", instruction: INSTRUCTION };
  const { doc } = await board();
  assert.deepEqual(doc.trials, [
    { ...V5, resumed: "2026-11-22T09:00:00Z", resumed_by: by },
    { ...V8, resumed: "2026-11-22T09:00:00Z", resumed_by: by },
    V10,
    SAMPLE,
  ]);
  const again = await resume({ instruction: INSTRUCTION });
  assert.deepEqual([again.status, JSON.parse(again.text)], [200, { resumed: 0 }]);
  assert.deepEqual((await board()).doc, doc);
});

test("a person resumes with {} and no email is stored", async () => {
  const { board, resume } = await setup();
  const got = await resume({}, { principal: person });
  assert.deepEqual([got.status, JSON.parse(got.text)], [200, { resumed: 2 }]);
  const { doc } = await board();
  assert.deepEqual(
    doc.trials.slice(0, 2).map((t) => t.resumed_by),
    [{ kind: "person" }, { kind: "person" }],
  );
  assert.ok(!JSON.stringify(doc).includes("example.test"));
});

test("the route refuses unbound services, bad bodies, unknown projects and other methods", async () => {
  const { board, resume } = await setup();
  const before = await board();
  const SHAPE =
    "a trial resume needs {instruction: 1-500 characters}; a signed-in person may send {}";
  const cases = [
    [
      resume({ instruction: INSTRUCTION }, { principal: unbound }),
      403,
      "only a signed-in person, or a paired host passing a person's instruction, resumes stage-note trials",
    ],
    [resume({}), 400, SHAPE],
    [resume({ instruction: "   " }), 400, SHAPE],
    [resume({ instruction: "x".repeat(501) }), 400, SHAPE],
    [resume({ instruction: INSTRUCTION, extra: 1 }), 400, SHAPE],
    [resume({ instruction: 5 }, { principal: person }), 400, SHAPE],
    [
      resume({ instruction: "resume‮trials" }),
      400,
      "instruction: contains control, invisible or bidirectional characters",
    ],
    [resume({ instruction: INSTRUCTION }, { slug: "sample" }), 404, "not found"],
    [resume(undefined, { method: "GET" }), 404, "not found"],
  ];
  for (const [pending, status, text] of cases) {
    const got = await pending;
    assert.deepEqual([got.status, got.text], [status, text]);
  }
  assert.deepEqual(await board(), before);
});
