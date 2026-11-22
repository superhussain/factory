// GET /api/spend and its rollup: tokens per merged PR and reverts by project and UTC week.
// Rows are the made-up running example.
import assert from "node:assert/strict";
import test from "node:test";
import { jobTokens, spendExpected, spendNudges, spendRows, weekStart } from "../../worker/spend.js";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const NOW = new Date("2026-11-22T12:00:00Z");
const PR = "https://github.com/example/sample/pull/";
const tokens = (counts) => ({ mac: { models: { "model-a": counts } } });

const JOBS = [
  {
    id: "1017-add-x",
    project: "sample",
    stage: "merged",
    done: "2026-11-18T10:00:00Z",
    pr: PR + "17",
    tokens: tokens({ input: 1200, output: 48000, cache_write: 210000, cache_read: 3400000 }),
    outcome: { result: "held" },
  },
  {
    id: "1018-add-y",
    project: "sample",
    stage: "merged",
    done: "2026-11-19T10:00:00Z",
    pr: PR + "18",
    tokens: tokens({ input: 100000, output: 300000 }),
    outcome: { result: "reverted" },
  },
  {
    id: "1019-add-z",
    project: "sample",
    stage: "closed",
    done: "2026-11-20T10:00:00Z",
    tokens: {
      mac: { models: { "model-a": { output: 40800 } } },
      pc: { models: { "model-b": { input: 100000, output: -5, cache_write: "9" } } },
    },
  },
  {
    id: "1020-rider-x",
    project: "sample",
    stage: "merged",
    done: "2026-11-18T10:05:00Z",
    pr: PR + "17",
  },
  {
    id: "1012-fix-a",
    project: "sample",
    stage: "merged",
    done: "2026-11-10T09:00:00Z",
    pr: PR + "12",
    tokens: tokens({ output: 300000 }),
  },
  {
    id: "1000-old",
    project: "sample",
    stage: "merged",
    done: "2026-09-27T23:59:00Z",
    pr: PR + "1",
    tokens: tokens({ output: 5 }),
  },
  {
    id: "1021-no-project",
    stage: "merged",
    done: "2026-11-18T10:00:00Z",
    tokens: tokens({ output: 7 }),
  },
  { id: "1022-running", project: "sample", stage: "build", tokens: tokens({ output: 9 }) },
];

const WEEK_16 = {
  project: "sample",
  week: "2026-11-16",
  merged_prs: 2,
  tokens: 800000,
  tokens_per_pr: 400000,
  judged: 2,
  reverted: 1,
  reworked: 0,
};
const WEEK_09 = {
  project: "sample",
  week: "2026-11-09",
  merged_prs: 1,
  tokens: 300000,
  tokens_per_pr: 300000,
  judged: 0,
  reverted: 0,
  reworked: 0,
};

test("jobTokens counts input, output and cache write and reads malformed counts as 0", () => {
  assert.equal(jobTokens(JOBS[0]), 259200);
  assert.equal(jobTokens(JOBS[2]), 140800);
  assert.equal(jobTokens({ tokens: "lots" }), 0);
  assert.equal(jobTokens({}), 0);
});

test("weekStart returns the UTC Monday on or before a time", () => {
  assert.equal(weekStart(NOW.getTime()), Date.parse("2026-11-16T00:00:00Z"));
  assert.equal(weekStart(Date.parse("2026-11-16T00:00:00Z")), Date.parse("2026-11-16T00:00:00Z"));
  assert.equal(weekStart(Date.parse("2026-11-15T23:59:59Z")), Date.parse("2026-11-09T00:00:00Z"));
});

test("spendRows rolls the example into the 2026-11-16 and 2026-11-09 rows", () => {
  assert.deepEqual(spendRows(JOBS, NOW), [WEEK_16, WEEK_09]);
});

test("spendRows keeps a job done on the first Monday of the window", () => {
  const edge = { ...JOBS[5], id: "1001-edge", done: "2026-09-28T00:00:00Z" };
  assert.deepEqual(spendRows([edge], NOW), [
    {
      project: "sample",
      week: "2026-09-28",
      merged_prs: 1,
      tokens: 5,
      tokens_per_pr: 5,
      judged: 0,
      reverted: 0,
      reworked: 0,
    },
  ]);
});

test("spendRows sorts by project, then newest week, and a closed-only week has no PR", () => {
  const rows = spendRows(
    [{ ...JOBS[2], project: "zeta" }, JOBS[4], { ...JOBS[3], project: "alpha", pr: undefined }],
    NOW,
  );
  assert.deepEqual(
    rows.map((r) => [r.project, r.week, r.merged_prs, r.tokens_per_pr]),
    [
      ["alpha", "2026-11-16", 1, 0],
      ["sample", "2026-11-09", 1, 300000],
      ["zeta", "2026-11-16", 0, null],
    ],
  );
});

function setup({ board, archive, projects = {} }) {
  const bucket = memoryBucket();
  const seed = (key, value) =>
    bucket.objects.set(key, {
      key,
      bytes: Buffer.from(JSON.stringify(value)),
      httpEtag: `"${key}-seed"`,
    });
  seed("board.json", { updated: "2026-11-22T11:00:00Z", jobs: board });
  if (archive) seed("board-archive.json", { jobs: archive });
  for (const [slug, text] of Object.entries(projects))
    bucket.objects.set(`projects/${slug}.md`, {
      key: `projects/${slug}.md`,
      bytes: Buffer.from(text),
      httpEtag: `"${slug}-seed"`,
    });
  return (method = "GET") =>
    serveFactory(new Request("http://local/api/spend", { method }), { BUCKET: bucket }, person);
}

test("GET /api/spend merges board and archive rows, the board row winning", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: NOW });
  const board = JOBS.filter((j) => j.id !== "1012-fix-a");
  const stale = { ...JOBS[1], outcome: { result: "held" }, tokens: tokens({ output: 1 }) };
  const send = setup({ board, archive: [stale, JOBS[4]] });
  const res = await send();
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("Content-Type"), "application/json; charset=utf-8");
  assert.equal(res.headers.get("Cache-Control"), "no-store");
  const body = await res.json();
  assert.equal(body.schema, "factory.spend/1");
  assert.equal(body.weeks, 8);
  assert.deepEqual(body.rows, [WEEK_16, WEEK_09]);
});

test("GET /api/spend answers empty rows with no archive and 404 for PUT", async () => {
  const send = setup({ board: [JOBS[7]] });
  assert.deepEqual((await (await send()).json()).rows, []);
  const put = await send("PUT");
  assert.equal(put.status, 404);
});

const EXPECTED = { project: "sample", weekly_tokens: 1000000, tokens: 800000, percent: 80 };
const projectFile = (extra) =>
  `---\nrepo: example/sample\nbase: main\ntitle: Sample\n${extra}---\n# Sample\n`;

test("GET /api/spend adds this week and the expected entry for a project with weekly_tokens", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now: NOW });
  const send = setup({
    board: JOBS,
    projects: {
      sample: projectFile("weekly_tokens: 1000000\n"),
      other: projectFile(""),
      bad: projectFile("weekly_tokens: 1e6\n"),
    },
  });
  const body = await (await send()).json();
  assert.equal(body.week, "2026-11-16");
  assert.deepEqual(body.expected, [EXPECTED]);
});

test("spendExpected sorts by project and reads a missing week as 0 tokens", () => {
  const budgets = new Map([
    ["sample", 1000000],
    ["alpha", 3],
  ]);
  assert.deepEqual(spendExpected(spendRows(JOBS, NOW), budgets, NOW), [
    { project: "alpha", weekly_tokens: 3, tokens: 0, percent: 0 },
    EXPECTED,
  ]);
});

test("spendNudges sends 50, then 80, then nothing on the example", () => {
  const week = "2026-11-16";
  const at = (percent) => [{ ...EXPECTED, tokens: percent * 10000, percent }];
  const first = spendNudges(at(50), week, null);
  assert.deepEqual(first.events, [
    {
      kind: "spend",
      id: "sample",
      threshold: 50,
      title: "500,000 of 1,000,000 expected weekly tokens (50%), week of 2026-11-16",
    },
  ]);
  assert.deepEqual(first.state, {
    schema: "factory.spend-nudges/1",
    week,
    sent: { sample: [50] },
  });
  const second = spendNudges(at(80), week, first.state);
  assert.deepEqual(second.events, [
    {
      kind: "spend",
      id: "sample",
      threshold: 80,
      title: "800,000 of 1,000,000 expected weekly tokens (80%), week of 2026-11-16",
    },
  ]);
  assert.deepEqual(second.state.sent, { sample: [50, 80] });
  const third = spendNudges(at(80), week, second.state);
  assert.deepEqual(third.events, []);
  assert.deepEqual(third.state, second.state);
});

test("spendNudges pushes only the highest new threshold and resets on a new week", () => {
  const over = [{ ...EXPECTED, tokens: 1200000, percent: 120 }];
  const { events, state } = spendNudges(over, "2026-11-16", null);
  assert.deepEqual(
    events.map((e) => [e.threshold, e.title]),
    [[100, "1,200,000 of 1,000,000 expected weekly tokens (120%), week of 2026-11-16"]],
  );
  assert.deepEqual(state.sent, { sample: [50, 80, 100] });
  const next = spendNudges([{ ...EXPECTED, tokens: 500000, percent: 50 }], "2026-11-23", state);
  assert.deepEqual(
    next.events.map((e) => e.threshold),
    [50],
  );
  assert.deepEqual(next.state, {
    schema: "factory.spend-nudges/1",
    week: "2026-11-23",
    sent: { sample: [50] },
  });
});

test("spendRows reads 2,000 finished rows quickly", () => {
  const rows = Array.from({ length: 2000 }, (_, i) => ({
    ...JOBS[0],
    id: `${2000 + i}-row`,
    pr: PR + i,
    done: new Date(NOW.getTime() - (i % 60) * 86400000).toISOString(),
  }));
  const text = JSON.stringify({ jobs: rows });
  const start = performance.now();
  spendRows(JSON.parse(text).jobs, NOW);
  const ms = performance.now() - start;
  console.log(`parse and roll up 2,000 rows (${text.length} bytes): ${ms.toFixed(1)} ms`);
  assert.ok(ms < 500);
});
