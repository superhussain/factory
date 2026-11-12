// PUT /api/project-retro/<slug>/<name>: retro admit's find-and-replace write to a project file
// or document. Every gate refuses with its exact status and text and stores nothing; a passing
// edit splices once and saves a restorable retro version.
import assert from "node:assert/strict";
import test, { mock } from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

mock.timers.enable({ apis: ["Date"], now: Date.parse("2026-11-12T09:01:00Z") });

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const wsl = { verified: true, kind: "service", client_id: "client-wsl" };
const unbound = { verified: true, kind: "service", client_id: "client-other" };
const RUN = "20261112T090000Z-a1b2c3";
const SUMMARY = "Notes: name the Worker test command";
const NOTE_LINE = "- Run pnpm test.";
const NEXT_LINE = "- Run pnpm test:protocol for Worker changes.";
const NOTES = `# Notes\n\n${NOTE_LINE}\n`;
const RUNNING = { paused: false, projects: ["factory"], self: [] };

async function setup({ autonomy = RUNNING } = {}) {
  const bucket = memoryBucket();
  const get = bucket.get;
  // R2 objects from get() expose arrayBuffer() and uploaded; the shared memory bucket does not.
  bucket.get = async (key) => {
    const obj = await get(key);
    return obj && { ...obj, arrayBuffer: async () => obj.bytes };
  };
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
  const json = { "Content-Type": "application/json" };
  const markdown = { "Content-Type": "text/markdown; charset=utf-8" };
  for (const slug of ["factory", "sample"])
    bucket.objects.set(`projects/${slug}.md`, {
      key: `projects/${slug}.md`,
      bytes: Buffer.from(
        "---\nrepo: example/factory\nbase: main\ntitle: Factory\n---\n\n# Factory\n",
      ),
      httpEtag: '"p"',
    });
  const created = await send("POST", "/api/hosts", {
    body: JSON.stringify({ id: "wsl", label: "wsl" }),
    headers: json,
  });
  const { code } = JSON.parse(created.text);
  const paired = await send("POST", "/api/hosts/wsl/pair", {
    body: JSON.stringify({ code }),
    headers: json,
    principal: wsl,
  });
  assert.equal(paired.status, 200);
  await bucket.put(
    "board.json",
    JSON.stringify({ updated: "2026-11-12T09:00:00Z", jobs: [], ...(autonomy && { autonomy }) }),
  );
  // The running example: notes.md is at version 4 and holds the wrong test command.
  let etag;
  for (let version = 1; version <= 4; version++) {
    const saved = await send("PUT", "/api/project-documents/factory/notes.md", {
      body: version === 4 ? NOTES : `# Notes\n\nold ${version}\n`,
      headers: { ...markdown, ...(etag ? { "If-Match": etag } : { "If-None-Match": "*" }) },
    });
    assert.equal(saved.status, 200);
    etag = saved.headers.get("ETag");
  }
  const edit = (fields = {}, { principal = wsl, slug = "factory", name = "notes.md" } = {}) =>
    send("PUT", `/api/project-retro/${slug}/${name}`, {
      body: JSON.stringify({
        schema: "factory.retro-doc-edit/1",
        run: RUN,
        base_version: 4,
        find: NOTE_LINE,
        replace: NEXT_LINE,
        summary: SUMMARY,
        ...fields,
      }),
      headers: json,
      principal,
    });
  const puts = () => bucket.calls.filter(([op]) => op === "put").length;
  const stored = () => bucket.objects.get("project-documents/factory/notes.md");
  const storedText = () => Buffer.from(stored().bytes).toString();
  return { bucket, send, edit, puts, stored, storedText };
}

test("a bound host's doc edit splices once and saves the next version as a restorable retro version", async () => {
  const s = await setup();
  const r = await s.edit();
  assert.deepEqual([r.status, r.text], [200, "ok"]);
  assert.equal(r.headers.get("X-Project-Version"), "5");
  assert.equal(s.storedText(), `# Notes\n\n${NEXT_LINE}\n`);
  const meta = s.stored().customMetadata;
  assert.deepEqual(
    { saved_by: meta.saved_by, summary: meta.summary, run: meta.run, host: meta.host },
    { saved_by: "retro", summary: SUMMARY, run: RUN, host: "wsl" },
  );
  assert.equal(meta.version, "5");
  const copy = s.bucket.objects.get("project-history/factory/notes.md/000005.md");
  assert.equal(Buffer.from(copy.bytes).toString(), `# Notes\n\n${NEXT_LINE}\n`);
  const list = JSON.parse((await s.send("GET", "/api/project-history/factory/notes.md")).text);
  const [latest] = list;
  assert.deepEqual(
    [latest.version, latest.saved_by, latest.summary, latest.run, latest.host],
    [5, "retro", SUMMARY, RUN, "wsl"],
  );
  const oldest = list.at(-1);
  assert.deepEqual(
    [oldest.version, oldest.summary, "run" in oldest, "host" in oldest],
    [1, "", false, false],
  );
  // A person restores version 4 with a summary, which saves version 6.
  const restore = await s.send("PUT", "/api/project-documents/factory/notes.md", {
    body: NOTES,
    headers: {
      "Content-Type": "text/markdown; charset=utf-8",
      "If-Match": r.headers.get("ETag"),
      "X-Project-Summary": encodeURIComponent("Undo retro run a1b2c3"),
    },
  });
  assert.equal(restore.headers.get("X-Project-Version"), "6");
  const [undo] = JSON.parse((await s.send("GET", "/api/project-history/factory/notes.md")).text);
  assert.deepEqual([undo.saved_by, undo.summary], ["person@example.test", "Undo retro run a1b2c3"]);
});

test("$& in replace stays literal", async () => {
  const s = await setup();
  const r = await s.edit({ replace: `${NOTE_LINE} $&` });
  assert.equal(r.status, 200);
  assert.equal(s.storedText(), `# Notes\n\n${NOTE_LINE} $&\n`);
});

test("a doc edit to project.md keeps the frontmatter and saves a version", async () => {
  const s = await setup();
  const r = await s.edit(
    { base_version: 1, find: "# Factory\n", replace: "# Factory\n\n## Why\n" },
    { name: "project.md" },
  );
  assert.equal(r.status, 200);
  assert.equal(r.headers.get("X-Project-Version"), "2");
  assert.equal(
    Buffer.from(s.bucket.objects.get("projects/factory.md").bytes).toString(),
    "---\nrepo: example/factory\nbase: main\ntitle: Factory\n---\n\n# Factory\n\n## Why\n",
  );
});

test("each gate refuses with its exact status and text and stores nothing", async () => {
  const cases = [
    [
      "unbound service token",
      {},
      { principal: unbound },
      403,
      "retro edits come only from a bound host",
    ],
    ["a person", {}, { principal: person }, 403, "retro edits come only from a bound host"],
    ["off the allowlist", {}, { slug: "sample" }, 403, "retro edits only allowlisted projects"],
    [
      "empty allowlist",
      {},
      { autonomy: { ...RUNNING, projects: [] } },
      403,
      "retro edits only allowlisted projects",
    ],
    ["no autonomy record", {}, { autonomy: null }, 403, "retro edits only allowlisted projects"],
    [
      "paused",
      {},
      { autonomy: { ...RUNNING, paused: true } },
      409,
      "autonomy is paused; no retro edit",
    ],
    ["stale base", { base_version: 3 }, {}, 412, "precondition failed"],
    ["future base", { base_version: 5 }, {}, 412, "precondition failed"],
    [
      "unknown find",
      { find: "- Run pnpm lint." },
      {},
      409,
      "find: must match the stored text exactly once",
    ],
    ["ambiguous find", { find: "s" }, {}, 409, "find: must match the stored text exactly once"],
    ["empty find", { find: "" }, {}, 400, "find: must be 1 to 600 characters"],
    ["long find", { find: "x".repeat(601) }, {}, 400, "find: must be 1 to 600 characters"],
    [
      "long replace",
      { replace: "x".repeat(601) },
      {},
      400,
      "replace: must be at most 600 characters",
    ],
    [
      "too large",
      { replace: "y".repeat(585) },
      {},
      413,
      "retro edit changes more than 600 characters",
    ],
    [
      "frontmatter edit",
      { base_version: 1, find: "title: Factory", replace: "title: Renamed" },
      { name: "project.md" },
      403,
      "retro edits never change project frontmatter",
    ],
    [
      "wrong schema",
      { schema: "factory.retro-doc-edit/2" },
      {},
      400,
      "schema: must be factory.retro-doc-edit/1",
    ],
    [
      "extra key",
      { stage: "spec" },
      {},
      400,
      "body: needs keys schema, run, base_version, find, replace, summary",
    ],
    ["bad run", { run: "a/b" }, {}, 400, "run: must be 1 to 64 of A-Z a-z 0-9 _ -"],
    ["bad base_version", { base_version: "4" }, {}, 400, "base_version: must be a whole number"],
    ["empty summary", { summary: "" }, {}, 400, "summary: must be one line of 1 to 120 characters"],
    [
      "secret replace",
      { replace: "token ghp_" + "a".repeat(30) },
      {},
      400,
      "project document: looks like a secret",
    ],
    [
      "control character in replace",
      { replace: "one\u000btwo" },
      {},
      400,
      "document contains control characters",
    ],
  ];
  for (const [name, fields, { principal, autonomy, slug, name: file }, status, text] of cases) {
    const s = await setup(autonomy === undefined ? {} : { autonomy });
    const before = s.puts();
    const r = await s.edit(fields, { principal, slug, name: file });
    assert.deepEqual([r.status, r.text], [status, text], name);
    assert.equal(s.puts(), before, name);
    assert.equal(s.storedText(), NOTES, name);
    assert.equal(
      [...s.bucket.objects.keys()].some((k) => k.includes("sample/")),
      false,
      name,
    );
  }
});

test("exactly 600 changed characters passes", async () => {
  const s = await setup();
  const r = await s.edit({ replace: "y".repeat(584) });
  assert.equal(r.status, 200);
});

test("at most 2 retro versions per file in 24 hours", async () => {
  const s = await setup();
  assert.equal((await s.edit()).status, 200);
  assert.equal(
    (await s.edit({ base_version: 5, find: NEXT_LINE, replace: `${NEXT_LINE} Run the tests.` }))
      .status,
    200,
  );
  const before = s.puts();
  const third = await s.edit({
    base_version: 6,
    find: `${NEXT_LINE} Run the tests.`,
    replace: `${NEXT_LINE} Run all the tests.`,
  });
  assert.deepEqual([third.status, third.text], [429, "retro edit cap: 2 per file per day"]);
  assert.equal(s.puts(), before);
  // Another allowlisted project's document has its own cap.
  await s.send("PUT", "/api/project-documents/sample/notes.md", {
    body: NOTES,
    headers: { "Content-Type": "text/markdown; charset=utf-8", "If-None-Match": "*" },
  });
  await s.bucket.put(
    "board.json",
    JSON.stringify({
      updated: "2026-11-12T09:00:00Z",
      jobs: [],
      autonomy: { ...RUNNING, projects: ["sample"] },
    }),
  );
  const other = await s.edit({ base_version: 1 }, { slug: "sample" });
  assert.equal(other.status, 200);
  // A day later the window has moved on, and the board allows factory again.
  await s.bucket.put(
    "board.json",
    JSON.stringify({ updated: "2026-11-12T09:00:00Z", jobs: [], autonomy: RUNNING }),
  );
  mock.timers.setTime(Date.parse("2026-11-13T09:02:00Z"));
  try {
    const later = await s.edit({
      base_version: 6,
      find: `${NEXT_LINE} Run the tests.`,
      replace: `${NEXT_LINE} Run all the tests.`,
    });
    assert.equal(later.status, 200);
  } finally {
    mock.timers.setTime(Date.parse("2026-11-12T09:01:00Z"));
  }
});

test("the retro route takes only PUT on a registered project and a known file name", async () => {
  const s = await setup();
  for (const path of [
    "/api/project-retro/factory/other.md",
    "/api/project-retro/missing/notes.md",
    "/api/project-retro/factory/",
    "/api/project-retro/factory",
  ]) {
    const r = await s.send("PUT", path, { principal: wsl, body: "{}" });
    assert.equal(r.status, 404, path);
  }
  assert.equal(
    (await s.send("GET", "/api/project-retro/factory/notes.md", { principal: wsl })).status,
    404,
  );
});
