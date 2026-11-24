// Full hosted browser behavior run: the CJS suites in a bounded pool, each against its own
// fresh owned ephemeral local service, beside the node --test files, one process each in a second
// pool. Each suite and file logs start and done lines and is killed after 10 minutes, failing if
// a mandatory case marker is missing. Capture mode is private builder evidence only:
// `--capture <outdir> --reference-html <absolute path>` records matched before/after
// views against the read-only reference page and is never part of CI or assertions.
// Each capture side gets up to three attempts. Each retry, and every 50th side, starts a fresh browser process.
// A side that fails all attempts is logged and the run continues; it ends with a FAIL summary and a non-zero exit.
// Compare two captures of one side with `node tests/browser/capture-compare.mjs`; it passes when no channel differs by more than 16.
// Capture pages bypass the dashboard service worker so request interception answers /board.json.
// Viewports and views: docs/development.md, Visual captures.
import { createHash } from "node:crypto";
import { mkdir, readFile, stat } from "node:fs/promises";
import { loadavg } from "node:os";
import { prepareAssets } from "../../tools/assets.mjs";
import { startLocal } from "../../tools/local.mjs";
import { browserExecutable } from "../support/browser-server.mjs";
import { recordClip } from "../support/webm-clip.mjs";
import { png } from "../support/png.mjs";
import owned from "../support/owned-origin.cjs";
import { clickNow } from "../support/board-ready.cjs";
import { retryCapture } from "./capture-retry.mjs";
import { pool, runChild } from "./run-child.mjs";
import { selectShard } from "./shard.mjs";

const CASES = selectShard(
  JSON.parse(await readFile(new URL("../fixtures/browser-cases.json", import.meta.url), "utf8")),
  process.env.FACTORY_BROWSER_SHARD,
);

const BROWSER_CONCURRENCY = 2;
// A suite or node file still running after this is killed and recorded as a failure.
const SUITE_TIMEOUT_MS = 600000;

async function runSuite(suite, dir) {
  const started = Date.now();
  const service = await startLocal({ persistDir: `${dir}${suite.file}-r2` });
  try {
    const result = await runChild(
      "node",
      [`tests/browser/${suite.file}`, "--browser", service.origin],
      {
        env: { [owned.OWNED_ORIGIN_ENV]: service.origin },
        timeoutMs: SUITE_TIMEOUT_MS,
        label: suite.file,
      },
    );
    return { ...result, ms: Date.now() - started };
  } finally {
    await service.close();
  }
}

async function behavior() {
  console.log(
    `browser shard ${process.env.FACTORY_BROWSER_SHARD || "all"}: ${CASES.suites.length} suites, ${CASES.nodeTests.length} node files`,
  );
  await prepareAssets();
  const dir = new URL(`../../.context/browser-run-${Date.now()}-${process.pid}/`, import.meta.url)
    .pathname;
  await mkdir(dir, { recursive: true });
  const failures = [];
  const [results, tests] = await Promise.all([
    pool(CASES.suites, BROWSER_CONCURRENCY, (suite) => runSuite(suite, dir)),
    pool(CASES.nodeTests, BROWSER_CONCURRENCY, (file) =>
      runChild("node", ["--test", "--test-reporter=tap", file], {
        timeoutMs: SUITE_TIMEOUT_MS,
        label: file,
      }),
    ),
  ]);
  CASES.suites.forEach((suite, index) => {
    const result = results[index];
    console.log(result.output.trim());
    let suiteFailed = false;
    if (result.timedOut) {
      suiteFailed = true;
      failures.push(`${suite.file}: timed out after ${SUITE_TIMEOUT_MS / 1000}s`);
    } else if (result.code !== 0) {
      suiteFailed = true;
      failures.push(`${suite.file}: exit ${result.code}`);
    }
    for (const marker of suite.cases) {
      if (!result.output.includes(marker)) {
        suiteFailed = true;
        failures.push(`${suite.file}: missing case ${JSON.stringify(marker)}`);
      }
    }
    console.log(
      `${suite.file}: ${suiteFailed ? "see failures" : "all cases present"} (${(result.ms / 1000).toFixed(1)}s)`,
    );
  });
  CASES.nodeTests.forEach((file, index) => {
    const result = tests[index];
    console.log(result.output.trim());
    if (result.timedOut) failures.push(`${file}: timed out after ${SUITE_TIMEOUT_MS / 1000}s`);
    else if (result.code !== 0) failures.push(`${file}: exit ${result.code}`);
    for (const marker of ["# fail 0", "# skipped 0"]) {
      if (!result.output.includes(marker))
        failures.push(`${file}: missing ${JSON.stringify(marker)}`);
    }
  });
  if (failures.length) {
    console.error("mandatory browser cases failed:\n" + failures.join("\n"));
    process.exit(1);
  }
  console.log("PASS full hosted browser behavior");
}

// Matched private captures: the same synthetic fixtures, frozen clock, viewports and
// themes against the read-only reference page and the replacement page.
const CAPTURE_JOBS = [
  {
    id: "001-capture",
    project: "sample",
    title: "Synthetic captured job",
    stage: "pr-open",
    branch: "factory/001-capture",
    round: 1,
    updated: "2026-01-01T12:00:00Z",
    pr: "https://github.com/example/sample/pull/42",
    paseo: { server: "synthetic-server", agent: "synthetic-agent" },
    note: "Synthetic capture note\nwith a second line",
  },
  {
    id: "002-needs",
    project: "sample",
    title: "Synthetic blocked job",
    stage: "build",
    updated: "2026-01-01T11:00:00Z",
    needs: "Choose the synthetic option.",
  },
  {
    id: "003-idea",
    title: "Synthetic captured idea",
    stage: "idea",
    updated: "2026-01-01T10:00:00Z",
    idea: "Synthetic captured idea\nA small example for the board.",
  },
  {
    id: "004-rework-capture",
    project: "sample",
    title: "Synthetic export idea",
    idea: "Synthetic export idea",
    stage: "closed",
    updated: "2026-01-01T09:00:00Z",
    done: "2026-01-01T09:00:00Z",
    note: "reworked into the backlog",
    backlog: {
      line: "- [ ] sample: Synthetic export idea",
      decision: "rework",
      at: "2026-01-01T09:00:00Z",
      notes: "Limit it to the weekly report and name the columns.",
    },
  },
  {
    id: "101-wrap-doc-line",
    project: "sample",
    title: "Synthetic wrap doc line",
    stage: "idea",
    idea: "Synthetic wrap doc line",
    updated: "2026-01-02T13:10:00Z",
    refs: [{ id: "090-sample-old", kind: "follow-up-of" }],
    claim: { device: "host-a", session: "s1", since: "2026-01-02T13:10:00Z" },
    agents: [
      {
        at: "2026-01-02T12:00:00Z",
        stage: "idea",
        role: "orchestrator",
        harness: "provider-a",
        via: "launcher",
        model: "model-a",
        effort: "high",
        paseo: { server: "srv-a", agent: "agent-orch" },
      },
      {
        at: "2026-01-02T12:05:00Z",
        stage: "spec",
        role: "spec",
        harness: "claude",
        via: "native",
        model: "model-a",
        effort: "medium",
        paseo: { server: "srv-a", agent: "agent-orch" },
      },
      {
        at: "2026-01-02T13:10:00Z",
        stage: "review",
        round: 1,
        role: "review-ui",
        harness: "claude",
        via: "native",
        model: "model-a",
        effort: "medium",
      },
    ],
    tokens: {
      "host-a": {
        at: "2026-01-02T13:10:00Z",
        transcripts: 3,
        models: {
          "model-a": { input: 1200, output: 48000, cache_write: 210000, cache_read: 3400000 },
        },
      },
      "host-b": {
        at: "2026-01-02T13:00:00Z",
        transcripts: 1,
        models: { "model-a": { input: 300, output: 9100, cache_write: 52000, cache_read: 610000 } },
      },
    },
  },
  {
    id: "102-move-comment",
    project: "sample",
    title: "Synthetic move comment",
    stage: "idea",
    idea: "Synthetic move comment",
    updated: "2026-01-02T12:00:00Z",
    refs: [{ id: "090-sample-old", kind: "follow-up-of" }],
  },
  {
    id: "107-held-rider",
    project: "sample",
    title: "Synthetic held rider",
    stage: "idea",
    idea: "Synthetic held rider",
    updated: "2026-10-27T09:00:00Z",
    refs: [{ id: "090-sample-old", kind: "follow-up-of" }],
  },
  {
    id: "106-chat-approved",
    project: "sample",
    title: "Synthetic chat approved",
    stage: "idea",
    idea: "Synthetic chat approved",
    updated: "2026-01-02T12:00:00Z",
    backlog: {
      line: "- [ ] sample: Synthetic chat approved",
      decision: "approved",
      at: "2026-01-02T12:00:00Z",
      via: "chat",
      quote: "Approve the export idea.",
      by: "host-a",
    },
  },
  { id: "108-batch-lead", project: "sample", stage: "spec", updated: "2026-01-01T12:00:00Z" },
  {
    id: "109-batch-rider",
    project: "sample",
    stage: "spec",
    refs: [{ id: "108-batch-lead", kind: "batched-into" }],
    note: "batched into 108-batch-lead",
    updated: "2026-01-01T12:00:00Z",
  },
  {
    id: "110-spec-gate",
    project: "sample",
    title: "Synthetic spec gate",
    stage: "needs-human",
    updated: "2026-01-01T12:00:00Z",
    note: "gate: spec-approval",
    needs: "approve spec.md, then run /factory approve-spec 110-spec-gate to start the build",
  },
  {
    id: "111-tracer-gate",
    project: "sample",
    title: "Synthetic Tracer scope hold",
    stage: "needs-human",
    updated: "2026-01-01T12:00:00Z",
    note: "gate: tracer-scope",
    tracer: { url: "https://tracer.example.test/scope/111" },
  },
  {
    id: "112-held-pr",
    project: "sample",
    title: "Synthetic held PR",
    stage: "pr-open",
    updated: "2026-01-01T12:00:00Z",
    branch: "factory/112-held-pr",
    round: 1,
    pr: "https://github.com/example/sample/pull/112",
    needs: "review and merge PR #112: this project's pipeline holds every PR for a person",
  },
  {
    id: "113-round-cap",
    project: "sample",
    title: "Synthetic round cap",
    stage: "needs-human",
    updated: "2026-01-01T12:00:00Z",
    note: "review panel: round 2 findings need a person",
    needs: "decide on the round-2 findings",
  },
  {
    id: "114-steer-export",
    project: "sample",
    title: "Synthetic steered export",
    stage: "build",
    updated: "2026-01-02T14:30:00Z",
    round: 2,
    steer: [
      { note: "Keep the CSV header order the same as the old report.", at: "2026-01-02T14:00:00Z" },
      { note: "Name the file export.csv.", at: "2026-01-02T14:30:00Z" },
    ],
    agents: [
      {
        at: "2026-01-02T13:00:00Z",
        stage: "build",
        role: "build",
        round: 1,
        harness: "provider-a",
        via: "native",
      },
      {
        at: "2026-01-02T14:20:00Z",
        stage: "build",
        role: "build",
        round: 2,
        harness: "provider-a",
        via: "native",
      },
    ],
  },
  {
    id: "130-ground-export",
    project: "sample",
    title: "Synthetic grounded export",
    stage: "review",
    updated: "2026-01-02T15:00:00Z",
    round: 1,
    rounds: [
      {
        security: "PASS",
        ux: "PASS",
        ui: "PASS",
        code: "CHANGES",
        unverified: [
          "review-code.md worker/push.js:412 bad-line",
          "review-code.md web/dashboard.html:9 off-diff",
        ],
      },
      { security: "PASS", ux: "SKIPPED", ui: "SKIPPED", code: "PASS" },
    ],
  },
  {
    id: "131-launch-retry",
    project: "sample",
    title: "Synthetic launch retry",
    stage: "idea",
    idea: "Synthetic launch retry",
    updated: "2026-01-02T15:00:00Z",
    not_before: "2026-10-27T15:00:00Z",
    launch_failure: { count: 2, reason: "launcher exited 1: no free agent slot" },
  },
  {
    id: "132-steer-human",
    project: "sample",
    title: "Synthetic steered needs-human",
    stage: "needs-human",
    updated: "2026-01-02T14:00:00Z",
    note: "builder failed twice",
    steer: [
      { note: "Keep the CSV header order the same as the old report.", at: "2026-01-02T14:00:00Z" },
    ],
  },
];
// More merged rows than the slim board read carries, so the merged column loads the rest on demand.
const CAPTURE_MERGED = Array.from({ length: 30 }, (_, n) => {
  const at = new Date(Date.UTC(2026, 0, 1) + n * 3600000).toISOString();
  return {
    id: `${500 + n}-synthetic-merged`,
    project: "sample",
    title: `Synthetic merged ${500 + n}`,
    stage: "merged",
    updated: at,
    done: at,
  };
});
const CAPTURE_BOARD = [...CAPTURE_JOBS, ...CAPTURE_MERGED];
const CAPTURE_GROUNDING_FILES = {
  "spec.md": "# Synthetic grounded export\n\nCapture fixture content, not real job data.\n",
  "round-1/review-code.md":
    "CHANGES\n\nThe synthetic code review.\n\n## Findings\n\nSynthetic finding.\n",
  "round-1/grounding.md": [
    "GROUNDING: UNVERIFIED 2",
    "",
    "Head: 2222222222222222222222222222222222222222",
    "Diff: origin/main",
    "Checked: 5 citations in 1 review files",
    "",
    "| Review | Citation | Status | Reason |",
    "| --- | --- | --- | --- |",
    "| review-code.md | `worker/push.js:412` | bad-line | worker/push.js has 300 lines |",
    "| review-code.md | `web/dashboard.html:9` | off-diff | more than 10 lines from every change to web/dashboard.html |",
    "",
    "Unverified means the cited path or line did not check out. The finding may still be right. No verdict changed.",
    "",
  ].join("\n"),
  "round-2/review-code.md":
    "VERDICT: PASS\n\nThe synthetic round 2 code review.\n\n## Findings\n\nSynthetic finding.\n",
  "pre-review.md": "PRE-REVIEW: FAIL\n\nThe synthetic pre-review.\n",
  "decision.md": "APPROVE\n\nThe synthetic decision.\n",
};
const CAPTURE_BACKLOG_CONTEXT = [
  "Recommend: approve",
  "",
  "The operator exports the board by hand today. Job 204 left CSV export out of scope.",
  "",
  "## Question: Export scope",
  "Which jobs go in the file?",
  "Pick: one",
  "- Visible jobs (recommended): Only the cards the current filter shows.",
  "- All jobs: Every job on the board.",
  "",
  "## Question: Columns",
  "Which columns does the file hold?",
  "Pick: many",
  "- Title: The card title.",
  "- Stage: The stage name.",
]
  .map((line) => (line ? "  " + line : line))
  .join("\n");
const CAPTURE_BACKLOG =
  "# Backlog\n\n- [ ] sample: Synthetic backlog item with a long description that runs well past three lines on a narrow card, so the board cuts it off and the drawer shows every word of it, including this closing clause\n" +
  CAPTURE_BACKLOG_CONTEXT +
  "\n- [ ] sample: Synthetic follow-on slice (sub-job-of 001-capture)   \n" +
  "- [ ] sample: Synthetic weekly report export with date, campaign and spend columns (rework-of 004-rework-capture)\n" +
  "- [ ] sample: Lesson: Worker route changes run pnpm test:protocol; pnpm test skips them.\n" +
  "  Recommend: approve\n\n" +
  "  Approve adds this bullet to sample/notes.md. No job starts.\n" +
  "  Why it waits: The close reason names the command but not which routes need it.\n\n" +
  "  Lesson: from 001-capture, queued 2026-10-27\n";
const CAPTURE_LONG_TITLE =
  "Synthetic project with a very long title that runs well past the edge of a narrow phone screen";
const CAPTURE_TRIAGE_ITEM = "sample: Synthetic follow-on slice (sub-job-of 001-capture)";
const CAPTURE_TRIAGE = {
  item: CAPTURE_TRIAGE_ITEM,
  decision: "hold",
  confidence: 62,
  threshold: 80,
  reason: "Rated by clef-flash against the project rules.",
  model: "@cf/cloudflare/clef-flash",
  at: "2026-10-27T10:00:00Z",
};
const CAPTURE_SPEC =
  "# Synthetic spec\n\nCapture fixture content, not real job data.\n\n## Tasks\n\n1. **Parse the fixture**\n   - Files: `sample.md`\n   - Check: synthetic\n2. **Render the list**\n   - Files: `sample.html`\n3. **Ship it**\n   - Check: synthetic\n\n```mermaid\ngraph LR; Alpha-->Beta\n```\n";
const CAPTURE_BUILD =
  "# Synthetic build\n\n" +
  Array.from({ length: 40 }, (_, i) => `Synthetic build paragraph ${i + 1}.`).join("\n\n") +
  "\n";
const CAPTURE_ANSWERS =
  "# Answers\n\n## Question 1: Browser evidence\n\nPick: one\n\n- [ ] Ship on unit tests (recommended): open the PR now.\n- [x] Free a slot first: re-run the builder for screenshots.\n\nOther: none\n";
const auditCall = (second, agent, tool, flags, target) => ({
  host: "example-host",
  job: "001-capture",
  session: "3f2a9c1d",
  agent,
  time: `2026-11-10 10:00:0${second}`,
  tool,
  flags,
  target,
});
const CAPTURE_AUDIT = [
  auditCall(1, "main", "Bash", [], "git status"),
  auditCall(2, "main", "Bash", ["force-push"], "git push --force origin factory/001-capture"),
  auditCall(3, "main", "Read", ["secret-file"], "~/app/.env"),
  auditCall(
    4,
    "main",
    "Bash",
    ["pipe-to-shell"],
    "curl -H 'Authorization: [redacted]' https://get.example.test/i.sh | sh",
  ),
  auditCall(5, "factory-builder", "Bash", ["recursive-delete", "denied"], "rm -rf build"),
  auditCall(6, "factory-builder", "Edit", [], "~/app/src/main.py"),
];
// The Red flags index of the running example, newest first.
const CAPTURE_AUDIT_FLAGS = [
  ...CAPTURE_AUDIT.filter((row) => row.flags.length).reverse(),
  ...Array.from({ length: 8 }, (_, i) =>
    auditCall(0, "factory-builder", "Bash", ["recursive-delete"], `rm -rf build-${i + 1}`),
  ),
].map(({ time, job, host, agent, tool, flags, target }, i) => ({
  time: i < 4 ? time : `2026-11-10 09:${String(59 - i).padStart(2, "0")}:00`,
  job,
  host,
  agent,
  tool,
  flags,
  target,
  paseo: { server: "srv-a", agent: "agent-build" },
  ...(flags[0] === "force-push" && { ack: "2026-11-11T09:00:00Z" }),
  ...(flags[0] === "recursive-delete" && {
    backlog: "- [ ] sample: Red flag: recursive-delete via Bash",
  }),
}));
// Settings > Hosts for the retros-flags views: the fixture list plus the capture host.
const CAPTURE_HOSTS = JSON.parse(
  await readFile(new URL("../fixtures/hosts-list.json", import.meta.url), "utf8"),
);
CAPTURE_HOSTS.push({ ...CAPTURE_HOSTS[0], id: "example-host", label: "Example Mac" });
// Two retro runs this browser never opened, for the header-badges view's Retros count.
const CAPTURE_RETROS = ["20261020T090000Z-a1b2c3", "20261013T090000Z-d4e5f6"].map((run) => ({
  schema: "factory.retro-run/1",
  run,
  host: "mac",
  from: "2026-10-06",
  to: "2026-10-19",
  sessions: 40,
  rejected: 0,
  findings: [],
  cards: [],
}));
// Stage-note trials for the Settings, Pipeline views: one open, two reverted, so sample is paused.
const CAPTURE_TRIALS = [
  {
    id: "sample-spec-v8",
    project: "sample",
    stage: "spec",
    status: "open",
    opened: "2026-11-12T09:00:00Z",
    arms: {
      a: ["101-sample-a", "102-sample-a", "103-sample-a"],
      b: ["104-sample-b", "105-sample-b"],
    },
    reads: { rounds: "inconclusive", tokens: "inconclusive" },
  },
  {
    id: "sample-build-v5",
    project: "sample",
    stage: "build",
    status: "reverted",
    opened: "2026-11-05T09:00:00Z",
    arms: {
      a: [
        "111-sample-a",
        "112-sample-a",
        "113-sample-a",
        "114-sample-a",
        "115-sample-a",
        "116-sample-a",
      ],
      b: ["117-sample-b", "118-sample-b", "119-sample-b", "120-sample-b", "121-sample-b"],
    },
    reads: { rounds: "b higher", tokens: "no difference" },
  },
  {
    id: "sample-review-v3",
    project: "sample",
    stage: "review",
    status: "reverted",
    opened: "2026-11-01T09:00:00Z",
    arms: {
      a: ["131-sample-a", "132-sample-a", "133-sample-a", "134-sample-a", "135-sample-a"],
      b: [
        "136-sample-b",
        "137-sample-b",
        "138-sample-b",
        "139-sample-b",
        "140-sample-b",
        "141-sample-b",
      ],
    },
    reads: { rounds: "no difference", tokens: "b higher" },
  },
];
// The archived running example: answered by interception, never on the board.
const CAPTURE_ARCHIVED = {
  id: "412-fix-login",
  project: "sample",
  title: "Synthetic login fix",
  stage: "merged",
  round: 1,
  branch: "factory/412-fix-login",
  updated: "2026-09-01T12:00:00Z",
  done: "2026-09-01T12:00:00Z",
};
const CAPTURE_ARCHIVED_CLOSED = {
  id: "413-old-suggestion",
  project: "sample",
  title: "Synthetic old suggestion",
  stage: "closed",
  updated: "2026-08-20T12:00:00Z",
  done: "2026-08-20T12:00:00Z",
};
const CAPTURE_RECOVERY = {
  version: 1,
  slug: "sample",
  name: "notes.md",
  mode: "edit",
  baseText: "Original notes\n",
  baseEtag: '"original"',
  draftText: "Retained synthetic draft\n",
  blocked: "conflict",
};

async function put(origin, route, body, type) {
  const current = await fetch(origin + route, { redirect: "manual" });
  if (!current.ok && current.status !== 404)
    throw new Error(`capture seed read ${route}: ${current.status}`);
  const headers = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  if (type) headers["Content-Type"] = type;
  const response = await fetch(origin + route, { method: "PUT", headers, body });
  if (response.status !== 200) throw new Error(`capture seed ${route}: ${response.status}`);
}

// A browser that served many sides can stop answering, so capture mode starts a fresh one this often.
const CAPTURE_SIDES_PER_BROWSER = 50;

async function capture(outdir, referencePath) {
  if (
    !outdir ||
    !referencePath ||
    !referencePath.startsWith("/") ||
    !referencePath.endsWith(".html")
  ) {
    throw new Error("capture mode needs --capture <outdir> --reference-html <absolute .html path>");
  }
  await mkdir(outdir, { recursive: true });
  const reference = await readFile(referencePath, "utf8");
  const candidate = await readFile(new URL("../../web/dashboard.html", import.meta.url), "utf8");
  const sha = (s) => createHash("sha256").update(s).digest("hex");
  await prepareAssets();
  const dir = new URL(`../../.context/capture-${Date.now()}-${process.pid}/`, import.meta.url)
    .pathname;
  await mkdir(dir, { recursive: true });
  const service = await startLocal({ persistDir: dir + "r2" });
  const puppeteer = (await import("puppeteer-core")).default;
  const launchBrowser = () =>
    puppeteer.launch({
      executablePath: browserExecutable(),
      headless: true,
      protocolTimeout: 60_000,
    });
  let browser = await launchBrowser();
  let sidesOnBrowser = 0;
  const origin = service.origin;
  const external = [];
  try {
    await put(
      origin,
      "/api/projects/sample.md",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      "text/markdown",
    );
    // The Worker takes one appended steering note per row and write, so notes land a round at a time.
    // 132 waits at build until the board lands once more with its needs-human stage.
    const steerRounds = Math.max(...CAPTURE_BOARD.map((j) => j.steer?.length ?? 0));
    for (let n = 0; n <= steerRounds; n++)
      await put(
        origin,
        "/api/board",
        JSON.stringify({
          updated: "2026-01-01T12:00:00Z",
          jobs: CAPTURE_BOARD.map(({ steer, ...j }) => ({
            ...j,
            ...(j.id === "132-steer-human" && { stage: "build" }),
            ...(steer && n && { steer: steer.slice(0, n) }),
          })),
        }),
        "application/json",
      );
    await put(
      origin,
      "/api/board",
      JSON.stringify({ updated: "2026-01-01T12:00:00Z", jobs: CAPTURE_BOARD }),
      "application/json",
    );
    await put(origin, "/api/backlog", CAPTURE_BACKLOG, "text/markdown");
    for (const [name, body] of Object.entries(CAPTURE_GROUNDING_FILES))
      await put(origin, "/api/jobs/sample/130-ground-export/" + name, body, "text/markdown");
    await put(origin, "/api/jobs/sample/001-capture/spec.md", CAPTURE_SPEC, "text/markdown");
    await put(origin, "/api/jobs/sample/001-capture/build.md", CAPTURE_BUILD, "text/markdown");
    await put(origin, "/api/jobs/sample/001-capture/answers.md", CAPTURE_ANSWERS, "text/markdown");
    await put(
      origin,
      "/api/jobs/sample/412-fix-login/spec.md",
      "# Synthetic login fix\n\nCapture fixture content, not real job data.\n",
      "text/markdown",
    );
    await put(
      origin,
      "/api/pipeline/global",
      JSON.stringify({ schema: 1, archive: { mode: "age", n: 30 } }),
      "application/json",
    );
    await put(
      origin,
      "/api/project-documents/sample/notes.md",
      "Original notes\n",
      "text/markdown",
    );
    const clip = await recordClip(browser, origin);
    const clipName = sha(clip) + ".webm";
    await put(origin, "/api/idea-files/" + clipName, clip, "video/webm");
    const ideaImage = png(320, 200, [33, 118, 174]);
    const ideaImageName = sha(ideaImage) + ".png";
    await put(origin, "/api/idea-images/" + ideaImageName, ideaImage, "image/png");

    const served = await fetch(origin + "/");
    const servedCsp = served.headers.get("Content-Security-Policy");
    const servedHtml = await served.text();
    const inner = (pattern) => servedHtml.match(pattern)[1];
    const servedTitle = inner(/<title>([^<]*)<\/title>/);
    const servedConfig = inner(/<script[^>]*id="factory-config"[^>]*>([^<]*)<\/script>/);
    const servedReference = inner(/<script[^>]*id="factory-reference"[^>]*>([^<]*)<\/script>/);
    const servedMetaCsp = inner(/http-equiv="Content-Security-Policy" content="([^"]*)"/);

    let archivedBoard = false;
    let trialsBoard = false;
    let archivePageFails = false;
    let archiveSearchFails = false;
    let longProject = false;
    let refBoard = false;
    let notifyBoard = false;
    let auditBoard = false;
    let flagsHosts = false;
    let retrosBadge = false;
    let flagsAckAbort = false;
    let refusalBacklog = false;
    let leadAgentsBoard = false;
    let writerBoard = false;
    let tallAgentsBoard = false;
    let ideaFilesBoard = false;
    let videoBoard = false;
    const failed = [];
    const onlyViews = process.env.FACTORY_CAPTURE_VIEWS?.split(",");
    const waitLiveDot = (page) =>
      page.waitForFunction(() => document.getElementById("dot").className === "dot ok", {
        timeout: 30000,
      });
    // A hash route reads the full board and redraws once it lands.
    const waitFullBoard = (page) =>
      page.waitForFunction(() => typeof boardComplete === "undefined" || boardComplete === true, {
        timeout: 30000,
      });
    // keepViewport captures without resizing the viewport, which would close an open card menu.
    async function view(name, width, height, theme, prepare, touch = false, keepViewport = false) {
      if (onlyViews && !onlyViews.includes(name)) return;
      for (const side of ["before", "after"]) {
        const started = Date.now();
        try {
          await retryCapture(`${side}-${name}`, async (attempt) => {
            // A timed-out page can wedge the shared browser, so each retry starts a fresh one.
            if (attempt > 1 || sidesOnBrowser >= CAPTURE_SIDES_PER_BROWSER) {
              if (attempt > 1)
                console.error(`capture relaunch ${side}-${name}: attempt ${attempt}`);
              else
                console.error(
                  `capture recycle ${side}-${name}: ${sidesOnBrowser} sides on this browser`,
                );
              await browser.close().catch(() => {});
              browser = await launchBrowser();
              sidesOnBrowser = 0;
            }
            sidesOnBrowser++;
            const context = await browser.createBrowserContext();
            try {
              await context.setPermission(origin, {
                permission: { name: "loopback-network" },
                state: "granted",
              });
              const page = await context.newPage();
              await page.setViewport({
                width,
                height,
                deviceScaleFactor: 1,
                hasTouch: touch,
                isMobile: touch,
              });
              await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
              await page.evaluateOnNewDocument((recovery) => {
                const NativeDate = Date;
                window.Date = class extends NativeDate {
                  constructor(...args) {
                    super(...(args.length ? args : ["2026-10-27T12:00:00Z"]));
                  }
                  static now() {
                    return new NativeDate("2026-10-27T12:00:00Z").getTime();
                  }
                };
                const interval = window.setInterval;
                window.setInterval = (fn, ms, ...args) =>
                  fn.name === "poll" || fn.name === "pollTick" ? 0 : interval(fn, ms, ...args);
                localStorage.setItem("factory-project-document-draft:sample:notes.md", recovery);
              }, JSON.stringify(CAPTURE_RECOVERY));
              // The dashboard's service worker would answer /board.json past this interception.
              await page.setBypassServiceWorker(true);
              await page.setRequestInterception(true);
              page.on("request", async (request) => {
                const target = new URL(request.url());
                // Chrome's own video controls draw their icons from data: URLs.
                if (target.protocol === "data:") return request.continue();
                if (target.origin !== origin) {
                  // The reference page requests its documented CDN; the replacement must not
                  // attempt any external request. Both are blocked either way.
                  if (side === "after") external.push(request.url());
                  return request.abort();
                }
                if (
                  target.pathname === "/api/backlog/triage/verdicts" &&
                  request.method() === "POST"
                ) {
                  const { items } = JSON.parse(request.postData() ?? "{}");
                  return request.respond({
                    status: 200,
                    contentType: "application/json",
                    body: JSON.stringify({
                      verdicts: (Array.isArray(items) ? items : []).map((item) =>
                        item === CAPTURE_TRIAGE_ITEM ? CAPTURE_TRIAGE : null,
                      ),
                    }),
                  });
                }
                // Reference pages older than the batched read ask one line at a time.
                if (target.pathname === "/api/backlog/triage") {
                  const held = target.searchParams.get("item") === CAPTURE_TRIAGE_ITEM;
                  return request.respond({
                    status: held ? 200 : 404,
                    contentType: "application/json",
                    body: held ? JSON.stringify(CAPTURE_TRIAGE) : "",
                  });
                }
                if (longProject && target.pathname === "/projects.json") {
                  const projects = await fetch(target.href);
                  return request.respond({
                    status: projects.status,
                    contentType: "application/json",
                    body: JSON.stringify([
                      ...(await projects.json()),
                      {
                        slug: "long",
                        title: CAPTURE_LONG_TITLE,
                        repo: "example/long",
                        base: "main",
                      },
                    ]),
                  });
                }
                if (auditBoard && target.pathname === "/api/audit/001-capture")
                  return request.respond({
                    status: 200,
                    contentType: "application/x-ndjson",
                    body: CAPTURE_AUDIT.map((row) => JSON.stringify(row) + "\n").join(""),
                  });
                // Served, so the backlog reread after a refused approve keeps the Depends on: line.
                if (
                  refusalBacklog &&
                  target.pathname === "/api/backlog" &&
                  request.method() === "GET"
                )
                  return request.respond({
                    status: 200,
                    contentType: "text/markdown",
                    body: CAPTURE_BACKLOG.replace(
                      "(rework-of 004-rework-capture)\n",
                      "(rework-of 004-rework-capture)\n" +
                        "  Depends on: sample: Synthetic follow-on slice (sub-job-of 001-capture)\n",
                    ),
                  });
                if (retrosBadge && target.pathname === "/api/retros")
                  return request.respond({
                    status: 200,
                    contentType: "application/json",
                    body: JSON.stringify(CAPTURE_RETROS),
                  });
                if (flagsHosts && target.pathname === "/api/hosts")
                  return request.respond({
                    status: 200,
                    contentType: "application/json",
                    body: JSON.stringify(CAPTURE_HOSTS),
                  });
                if (flagsAckAbort && target.pathname === "/api/audit-flags/ack")
                  return request.abort("internetdisconnected");
                if (target.pathname === "/api/audit-flags")
                  return request.respond({
                    status: 200,
                    contentType: "application/json",
                    body: JSON.stringify({
                      schema: "factory.audit-flags/1",
                      rows: CAPTURE_AUDIT_FLAGS,
                    }),
                  });
                if (refBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  const body = await board.json();
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({
                      ...body,
                      jobs: body.jobs.map((j) =>
                        j.id === "002-needs"
                          ? {
                              ...j,
                              refs: [
                                { kind: "follow-up-of", id: "004-rework-capture" },
                                { kind: "follow-up-of", id: "001-capture" },
                              ],
                            }
                          : j,
                      ),
                    }),
                  });
                }
                if (notifyBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  const body = await board.json();
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({
                      ...body,
                      ...(body.slim && {
                        slim: {
                          ...body.slim,
                          counts: {
                            ...body.slim.counts,
                            sample: {
                              ...body.slim.counts.sample,
                              merged: (body.slim.counts.sample?.merged ?? 0) + 1,
                            },
                          },
                        },
                      }),
                      jobs: [
                        ...body.jobs,
                        {
                          id: "120-sample-merged",
                          project: "sample",
                          title: "Synthetic merged export",
                          stage: "merged",
                          updated: "2026-10-27T10:00:00Z",
                          done: "2026-10-27T10:00:00Z",
                          pr: "https://github.com/example/sample/pull/120",
                        },
                      ],
                    }),
                  });
                }
                if (leadAgentsBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  const body = await board.json();
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({
                      ...body,
                      jobs: body.jobs.map((j) =>
                        j.id === "101-wrap-doc-line"
                          ? {
                              ...j,
                              stage: "pr-open",
                              pr: "https://github.com/example/sample/pull/101",
                              paseo: { server: "srv-a", agent: "agent-lead" },
                            }
                          : j,
                      ),
                    }),
                  });
                }
                if (writerBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  const body = await board.json();
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({
                      ...body,
                      jobs: body.jobs.map((j) =>
                        j.id === "002-needs"
                          ? {
                              ...j,
                              paseo_writer: {
                                server: "synthetic-server",
                                agent: "synthetic-writer",
                              },
                            }
                          : j,
                      ),
                    }),
                  });
                }
                if (tallAgentsBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  const body = await board.json();
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({
                      ...body,
                      jobs: body.jobs.map((j) =>
                        j.id === "101-wrap-doc-line"
                          ? {
                              ...j,
                              agents: Array.from(
                                { length: 30 },
                                (_, i) => j.agents[i % j.agents.length],
                              ),
                            }
                          : j,
                      ),
                    }),
                  });
                }
                if (ideaFilesBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  const body = await board.json();
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({
                      ...body,
                      jobs: body.jobs.map((j) =>
                        j.id === "101-wrap-doc-line"
                          ? {
                              ...j,
                              idea:
                                "# Synthetic idea\n\n" +
                                Array.from(
                                  { length: 40 },
                                  (_, i) => `Synthetic idea paragraph ${i + 1}.`,
                                ).join("\n\n"),
                              idea_images: [ideaImageName],
                              idea_files: ["a".repeat(64) + ".pdf", "b".repeat(64) + ".csv"],
                            }
                          : j,
                      ),
                    }),
                  });
                }
                if (videoBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  const body = await board.json();
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({
                      ...body,
                      jobs: body.jobs.map((j) =>
                        j.id === "003-idea" ? { ...j, idea_files: [clipName] } : j,
                      ),
                    }),
                  });
                }
                if (trialsBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({ ...(await board.json()), trials: CAPTURE_TRIALS }),
                  });
                }
                if (archivedBoard && target.pathname === "/board.json") {
                  const board = await fetch(target.href);
                  return request.respond({
                    status: board.status,
                    contentType: "application/json",
                    headers: { ETag: board.headers.get("ETag") ?? "" },
                    body: JSON.stringify({ ...(await board.json()), archived: { max_id: 416 } }),
                  });
                }
                if (target.pathname === "/api/board/archive") {
                  if (archivePageFails && target.searchParams.has("page"))
                    return request.respond({
                      status: 503,
                      contentType: "text/plain",
                      body: "unavailable",
                    });
                  if (target.searchParams.get("page") === "1")
                    return request.respond({
                      status: 200,
                      contentType: "application/json",
                      body: JSON.stringify({
                        jobs: [CAPTURE_ARCHIVED, CAPTURE_ARCHIVED_CLOSED],
                        page: 1,
                        pages: 2,
                        total: 52,
                      }),
                    });
                  if (archiveSearchFails && target.searchParams.has("q"))
                    return request.respond({
                      status: 500,
                      contentType: "text/plain",
                      body: "unavailable",
                    });
                  const q = target.searchParams.get("q");
                  if (q !== null) {
                    const jobs = [CAPTURE_ARCHIVED].filter((j) =>
                      [j.id, j.title, j.project].some((f) =>
                        f.toLowerCase().includes(q.toLowerCase()),
                      ),
                    );
                    return request.respond({
                      status: 200,
                      contentType: "application/json",
                      body: JSON.stringify({ jobs, total: jobs.length }),
                    });
                  }
                  if (target.searchParams.get("id") === "413-expired-link")
                    return request.respond({
                      status: 401,
                      contentType: "text/plain",
                      body: "unauthorized",
                    });
                  if (target.searchParams.get("id") === "411-sync-retry")
                    return request.respond({
                      status: 503,
                      contentType: "text/plain",
                      body: "unavailable",
                    });
                  const found = target.searchParams.get("id") === CAPTURE_ARCHIVED.id;
                  return request.respond({
                    status: found ? 200 : 404,
                    contentType: found ? "application/json" : "text/plain",
                    body: found ? JSON.stringify({ jobs: [CAPTURE_ARCHIVED] }) : "not found",
                  });
                }
                if (side === "before" && target.pathname === "/dashboard.html") {
                  const body = reference
                    .replaceAll("__FACTORY_TITLE__", () => servedTitle)
                    .replaceAll("__FACTORY_CONFIG__", () => servedConfig)
                    .replaceAll("__FACTORY_REFERENCE__", () => servedReference)
                    .replaceAll("__FACTORY_CSP__", () => servedMetaCsp)
                    .replaceAll("__FACTORY_IMG_HOST__", "images.example.test");
                  return request.respond({
                    status: 200,
                    contentType: "text/html",
                    headers: { "Content-Security-Policy": servedCsp },
                    body,
                  });
                }
                return request.continue();
              });
              await named(
                "goto",
                page.goto(origin + "/dashboard.html", { waitUntil: "networkidle0" }),
              );
              await named("board cards", page.waitForSelector(".card"));
              await named("live dot", waitLiveDot(page));
              const clip = await named("prepare", prepare(page, side));
              await named(
                "frame settle",
                page.evaluate(async () => {
                  await document.fonts.ready;
                  await new Promise((resolve) =>
                    requestAnimationFrame(() => requestAnimationFrame(resolve)),
                  );
                }),
              );
              const file = `${outdir.replace(/\/$/, "")}/${side}-${name}.png`;
              await named(
                "screenshot",
                page.screenshot(
                  clip
                    ? { path: file, clip, ...(keepViewport && { captureBeyondViewport: false }) }
                    : { path: file },
                ),
              );
              const { size } = await stat(file);
              if (size >= 5 * 1024 * 1024) throw new Error(`${file} exceeds 5 MiB`);
              console.log(
                JSON.stringify({ file, side, view: name, width, height, theme, bytes: size }),
              );
            } catch (error) {
              console.error(
                `capture fault ${side}-${name}: attempt ${attempt}, side ${sidesOnBrowser} on this browser, ${browser.browserContexts().length} contexts open, load ${loadavg()[0].toFixed(2)}: ${error?.message ?? error}`,
              );
              throw error;
            } finally {
              await context.close().catch(() => {});
            }
          });
        } catch (caught) {
          const error = String(caught?.message ?? caught);
          failed.push({ view: `${side}-${name}`, error });
          console.log(
            JSON.stringify({ side, view: name, result: "FAIL", ms: Date.now() - started, error }),
          );
        }
      }
    }

    // Prefixes a wait's error with the step it guarded, so a capture retry names the step.
    const named = (step, wait) =>
      wait.catch((error) => {
        throw new Error(`${step}: ${error?.message ?? error}`);
      });
    const openDrawer = async (page, side) => {
      await clickNow(page, '.card[data-id="001-capture"]');
      await page.waitForSelector('#d-tabs .tab[data-name="spec.md"]');
      // The replacement renders the bundled diagram; the blocked-CDN reference keeps the code block.
      await page.waitForFunction(
        (after) =>
          after
            ? document.querySelector("#d-doc .mermaid svg")
            : document.querySelector("#d-doc .mermaid svg") ||
              document.querySelector("#d-doc pre.mermaid-src"),
        { timeout: 30000 },
        side === "after",
      );
    };
    const openScrolled = async (page, side) => {
      await openDrawer(page, side);
      await page.click('.tab[data-name="build.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Synthetic build",
      );
      await page.evaluate(() => {
        document.getElementById("d-doc").scrollTop = 400;
      });
    };
    const openMidScrolled = async (page, side) => {
      await openScrolled(page, side);
      await page.evaluate(() => {
        document.getElementById("d-doc").scrollTop = 40;
      });
    };
    const openNotice = async (page, side) => {
      await openScrolled(page, side);
      await page.evaluate(() => {
        const err = document.getElementById("d-rerun-err");
        err.textContent = "Synthetic rerun failure.";
        err.hidden = false;
        document.getElementById("d-doc").scrollTop = 300;
      });
    };
    // A phone drawer on its build tab. It skips openDrawer's wait for the spec tab's diagram, which the build tab never shows and which a loaded host can take past 30 s to draw.
    const openBuildTab = async (page) => {
      await clickNow(page, '.card[data-id="001-capture"]');
      await page.waitForSelector('#d-tabs .tab[data-name="build.md"]');
      await page.click('.tab[data-name="build.md"]');
      await named(
        "build tab",
        page.waitForFunction(
          () => document.querySelector("#d-doc h1")?.textContent === "Synthetic build",
          { polling: 50, timeout: 30000 },
        ),
      );
    };
    // A phone drawer after a 150 px touch drag that starts on the job details.
    // The build tab is long enough for the page to scroll.
    const openMetaDragged = async (page, side) => {
      await openBuildTab(page);
      const m = await page.$eval("#d-meta", (el) => {
        const r = el.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
      });
      await page.touchscreen.touchStart(m.x, m.y);
      for (let k = 1; k <= 3; k++) await page.touchscreen.touchMove(m.x, m.y - 50 * k);
      // A pause before lifting the finger keeps the flick out of the capture.
      await new Promise((r) => setTimeout(r, 150));
      await page.touchscreen.touchEnd();
      if (side === "after") {
        const top = await page.$eval("#d-doc", (el) => el.scrollTop);
        if (Math.abs(top - 150) > 1) throw new Error(`meta drag scrolled to ${top}, expected 150`);
      }
    };
    // A phone drawer after a flick on the job details: three 50 px moves 40 ms apart, then a lift.
    // Before, the page stops at 150. After, it glides on and settles further down.
    const openMetaFlicked = async (page, side) => {
      await openBuildTab(page);
      const m = await page.$eval("#d-meta", (el) => {
        const r = el.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
      });
      const cdp = await page.createCDPSession();
      const t0 = Date.now();
      const touch = (type, y, ms) =>
        cdp.send("Input.dispatchTouchEvent", {
          type,
          touchPoints: type === "touchEnd" ? [] : [{ x: m.x, y }],
          timestamp: (t0 + ms) / 1000,
        });
      await touch("touchStart", m.y, 0);
      for (let k = 1; k <= 3; k++) await touch("touchMove", m.y - 50 * k, 40 * k);
      await touch("touchEnd", 0, 120);
      await cdp.detach();
      await named(
        "glide end",
        page.waitForFunction(() => typeof headGlide === "undefined" || headGlide === null, {
          polling: 50,
          timeout: 30000,
        }),
      );
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      const top = await page.$eval("#d-doc", (el) => el.scrollTop);
      if (side === "after" && top < 250)
        throw new Error(`meta flick scrolled to ${top}, expected 250 or more`);
    };
    const openAnswers = async (page, side) => {
      await openDrawer(page, side);
      await page.click('.tab[data-name="answers.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Answers",
      );
      await page.waitForFunction(() => {
        const pill = document.getElementById("d-tab-pill");
        const tab = document.querySelector("#d-tabs .tab.active");
        if (!pill || !tab || pill.hidden) return true;
        const style = getComputedStyle(pill);
        return (
          parseFloat(style.left) === tab.offsetLeft && parseFloat(style.width) === tab.offsetWidth
        );
      });
      if (side === "after") {
        const weight = await page.evaluate(() => {
          const label = document.querySelector("#d-doc li.task input:checked")?.closest("label");
          return label ? getComputedStyle(label).fontWeight : null;
        });
        if (weight === null) throw new Error("no ticked task label rendered");
        if (weight !== "600") throw new Error("ticked task label is not bold");
      }
    };
    // A prepare step that returns this page rectangle crops its view to it.
    const clipOf = (page, first, last = first) =>
      page.evaluate(
        (a, b) => {
          const top = document.querySelector(a).getBoundingClientRect();
          const bottom = document.querySelector(b).getBoundingClientRect();
          const left = Math.floor(Math.min(top.left, bottom.left));
          const y = Math.floor(top.top);
          return {
            x: left + scrollX,
            y: y + scrollY,
            width: Math.ceil(Math.max(top.right, bottom.right)) - left,
            height: Math.ceil(bottom.bottom) - y,
          };
        },
        first,
        last,
      );
    const openSettings = async (page) => {
      if (await page.$("#settings-link")) {
        await page.evaluate(() => {
          location.hash = "#/settings/projects";
        });
        await named(
          "settings projects",
          page.waitForSelector("#settings-projects:not([hidden])", { timeout: 30000 }),
        );
        // The pickup status above the tabs can grow a line late, so wait for settled text. Pages without the project pickup pass at once.
        await named(
          "settings pickup",
          page.waitForFunction(
            () => {
              const status = document.getElementById("project-pickup-status");
              if (!status || typeof renderProjectPickup !== "function") return true;
              const text = status.textContent;
              return text !== "" && !text.startsWith("Loading");
            },
            { polling: 50, timeout: 30000 },
          ),
        );
        await named("settings notes tab", clickNow(page, "#settings-tab-notes"));
      } else {
        await page.click("#project-settings");
        await page.select("#settings-resource", "notes.md");
      }
      // The recovered draft's conflict message, not the project tab's status, marks the view ready.
      await named(
        "settings draft",
        page.waitForFunction(
          () =>
            document.getElementById("settings-status").textContent.includes("Your draft is kept"),
          { polling: 50, timeout: 30000 },
        ),
      );
    };
    // History and Change summary push the field past one 390x844 screen, so the view clips from the document tabs to Save.
    const openSettingsField = async (page) => {
      await openSettings(page);
      return clipOf(page, "#settings-resource", "#settings-save");
    };
    const openJumper = async (page) => {
      await page.evaluate(() => {
        document.getElementById("job-jump").click();
      });
      await page.waitForSelector("#job-picker[open]");
    };
    const clickArchiveSearch = (page) =>
      page.evaluate(() =>
        [...document.querySelectorAll('#job-results [role="option"]')]
          .find((o) => o.querySelector(".title").textContent === "Search archived jobs")
          .click(),
      );
    const openIdea = async (page) => {
      await page.waitForSelector("#new-idea:not([hidden])");
      await page.click("#new-idea");
      await page.waitForSelector("#idea-dlg[open]");
    };

    await view("board-desktop", 1440, 900, "light", async () => {});
    await view("board-desktop-dark", 1440, 900, "dark", async () => {});
    await view("board-mobile", 390, 844, "dark", async () => {});
    const putHalt = (page, body) =>
      page.evaluate(
        (text) =>
          fetch("./api/halt", {
            method: "PUT",
            body: text,
            headers: { "Content-Type": "application/json" },
          }),
        body,
      );
    // The after page reads the halt it was just given; the reference page has no banner.
    const haltBanner = async (page, side) => {
      await putHalt(page, '{"halted":true,"reason":"Runaway launches on wsl"}');
      if (side === "after") {
        await page.evaluate(() => loadHalt());
        await page.waitForSelector("#halt-banner:not([hidden])");
      }
      await putHalt(page, '{"halted":false}');
    };
    await view("halt-banner-1440", 1440, 900, "light", haltBanner);
    await view("halt-banner-390", 390, 844, "dark", haltBanner);
    await view("settings-halt-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/hosts";
      });
      await page.waitForSelector("#settings-hosts:not([hidden])");
      await putHalt(page, '{"halted":false}');
    });
    await view("settings-halt-blank-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/hosts";
      });
      await page.waitForSelector("#settings-hosts:not([hidden])");
      await putHalt(page, '{"halted":false}');
      await page.type("#halt-reason", "   ");
      await page.click("#halt-stop");
      return clipOf(page, "#halt-form");
    });
    const expandMerged = (page) =>
      page.evaluate(() => document.querySelector('.column-toggle[data-stage="merged"]').click());
    const mergedHeader = async (page) => {
      await expandMerged(page);
      return clipOf(page, ".col.merged h2");
    };
    // At 1440 the clip takes in the pr-open column too, so card tops can be compared.
    const mergedBeside = async (page) => {
      await expandMerged(page);
      return clipOf(page, ".col.pr-open h2", ".col.merged .card");
    };
    await view("board-merged-header-1440", 1440, 900, "light", mergedBeside);
    await view("board-merged-header-390", 390, 844, "dark", mergedHeader);
    // The full read never answers, so the button stays in its loading state.
    const holdFullRead = (page) =>
      page.evaluate(() => {
        const fetchNow = window.fetch;
        window.fetch = (url, options) =>
          String(url).endsWith("api/board") && !options?.method
            ? new Promise(() => {})
            : fetchNow(url, options);
      });
    const clickMore = (page) =>
      page.evaluate(() => document.querySelector('.more[data-stage="merged"]').click());
    await view("board-merged-loading-1440", 1440, 900, "light", async (page) => {
      await holdFullRead(page);
      await expandMerged(page);
      await clickMore(page);
      return clipOf(page, ".col.pr-open h2", ".col.merged .card");
    });
    // The saved slim copy is on screen, so a click on show all turns busy until the network read.
    await view("board-merged-saved-click-1440", 1440, 900, "light", async (page) => {
      await holdFullRead(page);
      await expandMerged(page);
      await page.evaluate(() => {
        if (typeof boardFromCache !== "undefined") boardFromCache = true;
      });
      await clickMore(page);
      return clipOf(page, ".col.pr-open h2", ".col.merged .card");
    });
    await view("board-banner-saved-1440", 1440, 900, "light", async (page) => {
      await page.evaluate(() => {
        boardFromCache = true;
        setConnection("unreachable");
      });
      return clipOf(page, "#conn-banner");
    });
    await view("board-offline-deeplink-1440", 1440, 900, "light", async (page) => {
      await page.evaluate(() => {
        const fetchNow = window.fetch;
        window.fetch = (url, options) =>
          /api\/board(\/archive|$)/.test(new URL(url, location.href).pathname)
            ? Promise.reject(new TypeError("offline"))
            : fetchNow(url, options);
        boardFromCache = true;
        setConnection("offline");
        location.hash = "#500-synthetic-merged";
      });
      await new Promise((resolve) => setTimeout(resolve, 1500));
    });
    await view("board-saved-deeplink-1440", 1440, 900, "light", async (page) => {
      await page.evaluate(() => {
        const fetchNow = window.fetch;
        window.fetch = (url, options) =>
          /api\/board(\/archive|$)/.test(new URL(url, location.href).pathname)
            ? new Promise(() => {})
            : fetchNow(url, options);
        boardFromCache = true;
        location.hash = "#500-synthetic-merged";
      });
      await new Promise((resolve) => setTimeout(resolve, 1500));
    });
    await view("board-saved-deeplink-back-1440", 1440, 900, "light", async (page) => {
      await page.evaluate(() => {
        const fetchNow = window.fetch;
        window.fetch = (url, options) =>
          /api\/board(\/archive|$)/.test(new URL(url, location.href).pathname)
            ? new Promise(() => {})
            : fetchNow(url, options);
        boardFromCache = true;
        location.hash = "#500-synthetic-merged";
      });
      await new Promise((resolve) => setTimeout(resolve, 1500));
      await page.evaluate(() => history.back());
      await new Promise((resolve) => setTimeout(resolve, 500));
    });
    // The merged column at 160px with a production-sized total. The reference page has no
    // slim counts, so its before shot keeps the fixture's own total.
    const narrowThousands = async (page) => {
      await holdFullRead(page);
      await expandMerged(page);
      await page.evaluate(() => {
        const style = document.createElement("style");
        style.textContent = ".col.merged { width: 160px; max-width: 160px; }";
        document.head.append(style);
        if (typeof slimCounts !== "undefined") {
          slimCounts.sample.merged = 1763;
          renderBoard();
        }
      });
    };
    await view("board-merged-160-1763", 1440, 900, "light", async (page) => {
      await narrowThousands(page);
      return clipOf(page, ".col.pr-open h2", ".col.merged .card");
    });
    await view("board-merged-160-1763-loading", 1440, 900, "light", async (page) => {
      await narrowThousands(page);
      await clickMore(page);
      return clipOf(page, ".col.pr-open h2", ".col.merged .card");
    });
    await view("board-merged-all-1440", 1440, 900, "light", async (page) => {
      await expandMerged(page);
      await page.evaluate(() => document.querySelector('.more[data-stage="merged"]').click());
      await page.waitForFunction(
        (n) => document.querySelectorAll(".col.merged .card").length === n,
        {},
        CAPTURE_BOARD.filter((j) => j.stage === "merged").length,
      );
    });
    await view("header-inbox-dark-390", 390, 844, "dark", (page) => clipOf(page, "header"));
    await view("header-inbox-dark-1440", 1440, 900, "dark", (page) => clipOf(page, "header"));
    retrosBadge = true;
    try {
      await view("header-badges-1440", 1440, 900, "light", (page) => clipOf(page, "header"));
    } finally {
      retrosBadge = false;
    }
    await view("drawer-desktop", 1440, 900, "light", openDrawer);
    await view("drawer-mobile", 390, 844, "dark", openDrawer);
    await view("drawer-mobile-scrolled", 375, 667, "dark", openScrolled);
    await view("drawer-mobile-midscroll", 390, 844, "dark", openMidScrolled);
    await view("drawer-mobile-notice", 390, 844, "dark", openNotice);
    await view("drawer-mobile-meta-drag", 390, 844, "dark", openMetaDragged, true);
    await view("drawer-mobile-meta-flick", 390, 844, "dark", openMetaFlicked, true);
    await view("drawer-desktop-scrolled", 1440, 900, "light", openScrolled);
    await view("answers-desktop", 1440, 900, "light", openAnswers);
    await view("answers-mobile", 390, 844, "dark", openAnswers);
    await view("settings-recovery-desktop", 1440, 900, "light", openSettings);
    await view("settings-recovery-mobile", 390, 844, "dark", openSettingsField);
    await view("jumper-desktop", 1440, 900, "light", openJumper);
    await view("jumper-mobile", 390, 844, "dark", openJumper);
    await view("idea-desktop", 1440, 900, "light", openIdea);
    await view("idea-mobile", 390, 844, "light", openIdea);
    const openFilter = async (page) => {
      await page.click("#filter");
    };
    await view("filter-open-desktop", 1440, 900, "light", openFilter);
    await view("filter-open-mobile", 390, 844, "dark", openFilter);
    longProject = true;
    try {
      await view("filter-long-desktop", 1440, 900, "light", openFilter);
      await view("filter-long-mobile", 390, 844, "dark", openFilter);
      await view("filter-long-touch", 390, 844, "dark", (page) => page.tap("#filter"), true);
    } finally {
      longProject = false;
    }
    const hideAllProjects = async (page) => {
      await page.click("#filter");
      await page.click('#filter-menu [data-project=""]');
      await page.keyboard.press("Escape");
    };
    await view("filter-none-desktop", 1440, 900, "light", hideAllProjects);
    await view("filter-none-mobile", 390, 844, "dark", hideAllProjects);
    const hideColumnsAndProjects = async (page) => {
      await page.evaluate(() =>
        localStorage.setItem(
          "factory-hidden-columns",
          "backlog,idea,spec,build,review,approve,pr-open,closed",
        ),
      );
      await page.reload({ waitUntil: "networkidle0" });
      await page.waitForSelector("#filter:not([hidden])");
      await waitLiveDot(page);
      await hideAllProjects(page);
    };
    await view("filter-none-few-columns-desktop", 1440, 900, "light", hideColumnsAndProjects);
    const focusSecondRef = async (page) => {
      await page.focus('.card-actions[data-actions="002-needs"]');
      await page.keyboard.press("Tab");
      await page.keyboard.press("Tab");
      const scrolled = await page.evaluate(() => {
        const a = document.activeElement;
        a.closest(".card-wrap").scrollIntoView({ block: "center" });
        const row = a.closest(".card-wrap").querySelector(".refs.line");
        return a.matches('a.ref-focus[data-ref="001-capture"]') && row.scrollLeft > 0;
      });
      if (!scrolled)
        throw new Error("card ref capture: 001-capture ref not focused in a scrolled row");
    };
    refBoard = true;
    try {
      await view("card-ref-focus-mobile", 390, 844, "dark", focusSecondRef);
    } finally {
      refBoard = false;
    }
    const openNotificationsMenu = (press) => async (page) => {
      await named("notifications button", page[press]("#notifications-button"));
      await named(
        "notifications list",
        page.waitForSelector("#notifications-menu-list .notification-link", { timeout: 30000 }),
      );
    };
    const openNotificationsPage = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/notifications";
      });
      await waitFullBoard(page);
      await page.waitForSelector("#notifications-list .inbox-item");
    };
    const openInboxSplit = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/inbox";
      });
      await page.waitForSelector('#inbox-list a[href="#002-needs"]');
      await page.click('#inbox-list a[href="#002-needs"]');
      await page.waitForFunction(
        () =>
          document.querySelector("#d-meta")?.textContent.includes("002-needs") &&
          document.querySelector("#d-doc").textContent !== "loading files",
        { timeout: 30000 },
      );
    };
    const openNotificationsSplit = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/notifications";
      });
      await waitFullBoard(page);
      await page.waitForSelector('#notifications-list a[href="#001-capture"]');
      await page.click('#notifications-list a[href="#001-capture"]');
      await page.waitForFunction(
        () =>
          document.querySelector("#d-meta")?.textContent.includes("001-capture") &&
          document.querySelector("#d-doc").textContent !== "loading files",
        { timeout: 30000 },
      );
    };
    const openNotificationsHiddenLinks = async (page) => {
      await page.evaluate(() => localStorage.setItem("factory-hidden-drawer-links", "web"));
      await page.reload({ waitUntil: "networkidle0" });
      await page.waitForSelector(".card");
      await openNotificationsPage(page);
    };
    // The reference page has no toast stack, so both sides crop the same bottom right corner.
    const openToasts = async (page, side) => {
      if (side === "after")
        await page.evaluate(() => {
          showNotifyToast("spec", "#073-sample-retry", "Synthetic retry limit", "073-sample-retry");
          showNotifyToast("backlog", "#/backlog/0123456789ab", "Synthetic backlog idea", "sample");
        });
      return page.evaluate(() => {
        const width = Math.min(innerWidth, 380);
        return { x: innerWidth - width, y: innerHeight - 220, width, height: 220 };
      });
    };
    // Both sides show the toasts when the page has a toast stack.
    const showCaptureToasts = (page) =>
      page.evaluate(() => {
        if (typeof showNotifyToast !== "function") return;
        showNotifyToast("spec", "#073-sample-retry", "Synthetic retry limit", "073-sample-retry");
        showNotifyToast("backlog", "#/backlog/0123456789ab", "Synthetic backlog idea", "sample");
      });
    notifyBoard = true;
    try {
      await view("notifications-menu-1280", 1280, 900, "light", openNotificationsMenu("click"));
      await view("notifications-menu-390", 390, 844, "dark", openNotificationsMenu("tap"), true);
      await view("notifications-page-1280", 1280, 900, "light", openNotificationsPage);
      await view("notifications-page-390", 390, 844, "dark", openNotificationsPage);
      await view("notifications-split-1280", 1280, 900, "light", openNotificationsSplit);
      await view(
        "notifications-page-hidden-links-1280",
        1280,
        900,
        "light",
        openNotificationsHiddenLinks,
      );
      await view(
        "header-notifications-320",
        320,
        844,
        "dark",
        (page) => clipOf(page, "header"),
        true,
      );
      await view("inbox-split-900", 900, 900, "light", openInboxSplit);
      await view("inbox-split-1280", 1280, 900, "light", openInboxSplit);
      await view("notify-toast-1280", 1280, 900, "light", openToasts);
      await view("notify-toast-390", 390, 844, "dark", openToasts);
      // The reference page has no position select, so its side centers the poll select above.
      await view("display-toast-position-1280", 1280, 900, "light", async (page, side) => {
        await page.evaluate(() => {
          location.hash = "#/settings/display";
        });
        await page.waitForSelector("#settings-display:not([hidden])");
        await page.evaluate(
          (s) => document.querySelector(s).scrollIntoView({ block: "center" }),
          side === "after" ? "#display-toast-position" : "#display-poll",
        );
      });
      await view("notify-toast-top-left-1280", 1280, 900, "light", async (page) => {
        await page.evaluate(() => localStorage.setItem("factory-toast-position", "top-left"));
        await page.reload({ waitUntil: "networkidle0" });
        await page.waitForSelector(".card");
        await showCaptureToasts(page);
      });
      await view("notify-toast-bottom-center-status-1280", 1280, 900, "light", async (page) => {
        await page.evaluate(() => localStorage.setItem("factory-toast-position", "bottom-center"));
        await page.reload({ waitUntil: "networkidle0" });
        await page.waitForSelector(".card");
        await showCaptureToasts(page);
        await page.evaluate(() => showBoardStatus("Could not resume the factory. Try again."));
        await page.waitForFunction(() => document.getElementById("board-status").textContent);
        return page.evaluate(() => ({
          x: 0,
          y: innerHeight - 220,
          width: innerWidth,
          height: 220,
        }));
      });
      await view("notify-toast-corner-status-1280", 1280, 900, "light", async (page) => {
        await page.evaluate(() => localStorage.removeItem("factory-toast-position"));
        await page.reload({ waitUntil: "networkidle0" });
        await page.waitForSelector(".card");
        await showCaptureToasts(page);
        await page.evaluate(() =>
          showBoardStatus(
            "Could not save the board. Another device changed 073-sample-retry and 074-sample-queue while you edited. Reload the board to see their changes, then try again.",
          ),
        );
        await page.waitForFunction(() => document.getElementById("board-status").textContent);
        return page.evaluate(() => ({
          x: 0,
          y: innerHeight - 300,
          width: innerWidth,
          height: 300,
        }));
      });
      await view("notify-toast-bottom-status-390", 390, 844, "dark", async (page) => {
        await page.evaluate(() => localStorage.removeItem("factory-toast-position"));
        await page.reload({ waitUntil: "networkidle0" });
        await page.waitForSelector(".card");
        await showCaptureToasts(page);
        await page.evaluate(() => showBoardStatus("Could not resume the factory. Try again."));
        await page.waitForFunction(() => document.getElementById("board-status").textContent);
        return page.evaluate(() => ({
          x: 0,
          y: innerHeight - 220,
          width: innerWidth,
          height: 220,
        }));
      });
      await view(
        "notify-toast-swipe-390",
        390,
        844,
        "dark",
        async (page) => {
          await showCaptureToasts(page);
          await page.evaluate(() => {
            const first = document.querySelector(".notify-toast");
            if (first) first.style.transform = "translateX(120px)";
          });
          return page.evaluate(() => ({
            x: 0,
            y: innerHeight - 220,
            width: innerWidth,
            height: 220,
          }));
        },
        true,
      );
      await view("notifications-menu-kinds-1280", 1280, 900, "light", async (page) => {
        await page.evaluate(() => {
          const at = new Date(Date.now() - 60000).toISOString();
          const backlogHash = "0123456789ab";
          localStorage.setItem(
            "factory-notifications-log",
            JSON.stringify([
              { kind: "spec", id: "120-sample-merged", at },
              {
                kind: "backlog",
                key: backlogHash,
                title: "Synthetic backlog idea",
                project: "sample",
                at,
              },
            ]),
          );
        });
        await page.click("#notifications-button");
        await page.waitForSelector("#notifications-menu-list .notification-link");
      });
    } finally {
      notifyBoard = false;
    }
    const focusSecondMenuItem = async (page) => {
      await page.focus('.card-actions[data-actions="001-capture"]');
      await page.keyboard.press("ArrowDown");
      await page.keyboard.press("ArrowDown");
    };
    await view("card-menu-focus-desktop", 1440, 900, "light", focusSecondMenuItem);
    // Both pages have the decision page, so both sides open it.
    const openDecision = async (page) => {
      await page.click(".backlog-card");
      await page.waitForSelector("#backlog-form");
    };
    const openQuestions = async (page, side) => {
      await openDecision(page);
      await page.evaluate(
        (selector) => {
          document.querySelector(selector).scrollIntoView();
        },
        side === "after" ? ".backlog-q" : "#backlog-submit",
      );
    };
    const openGate = async (page, side) => {
      await openDecision(page);
      await page.focus("input[value=approved]");
      if (side === "after") await page.keyboard.press("a");
    };
    const openAnswered = async (page) => {
      await openDecision(page);
      const center = (selector) =>
        page.evaluate(
          (s) => document.querySelector(s).scrollIntoView({ block: "center" }),
          selector,
        );
      await center("input[name=bq1][value='0']");
      await page.click("input[name=bq1][value='0']");
      await page.focus("input[name=bq1][value='1']");
      await page.keyboard.press("d");
      await page.evaluate(() =>
        document.querySelectorAll("fieldset.backlog-q")[1].scrollIntoView({ block: "center" }),
      );
    };
    const openRework = async (page, side) => {
      await page.click('.backlog-card[data-index="2"]');
      await page.waitForSelector("#backlog-form");
      if (side !== "after") return;
      await page.click("input[value=rework]");
      await page.type("#backlog-notes", "Split the export into its own settings page.");
    };
    const openTriage = async (page) => {
      await page.click('.backlog-card[data-index="1"]');
      await page.waitForSelector("#backlog-form");
      await page.waitForSelector("#backlog-triage:not([hidden])");
    };
    // The after side waits until the board has prefetched all four triage verdicts.
    const openTriagePrefetched = async (page, side) => {
      if (side === "after") await page.waitForFunction(() => backlogTriageSeen.size === 4);
      await page.click('.backlog-card[data-index="1"]');
      await page.waitForSelector("#backlog-form");
    };
    // The reference has no archive fieldset or archive lookup, so its side shows triage and the board.
    const openArchiveSettings = async (page, side) => {
      await page.evaluate(() => {
        location.hash = "#/settings/pipeline";
      });
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.waitForFunction(() => !document.getElementById("pipeline-triage-mode").disabled);
      await page.evaluate(
        (selector) => document.querySelector(selector).scrollIntoView({ block: "center" }),
        side === "after" ? "#pipeline-archive" : "#pipeline-triage",
      );
    };
    const openArchivedDrawer = async (page) => {
      // Both sides wait for the archive answer, so the before shot is not a timed guess.
      const answered = page.waitForResponse((response) => {
        const url = new URL(response.url());
        return (
          url.pathname === "/api/board/archive" && url.searchParams.get("id") === "412-fix-login"
        );
      });
      await page.evaluate(() => {
        location.hash = "#412-fix-login";
      });
      await answered;
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Synthetic login fix",
      );
    };
    // The reference has no archive list, so its side shows the closed column.
    // Terminal columns start collapsed, so both sides expand closed first.
    const openArchiveList = async (page, side) => {
      await page.click('.column-toggle[data-stage="closed"]');
      await page.waitForSelector("#column-cards-closed:not([hidden])");
      if (side === "after") {
        await page.click('.archive-more[data-stage="closed"]');
        await page.waitForSelector('.col.closed .archive-list .card[data-id="413-old-suggestion"]');
      }
      await page.evaluate(() =>
        document.querySelector(".col.closed").scrollIntoView({ block: "start", inline: "start" }),
      );
    };
    await view("archive-settings", 1280, 900, "light", openArchiveSettings);
    const openPollSettings = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/display";
      });
      await page.waitForSelector("#settings-display:not([hidden])");
      await page.evaluate(() =>
        document.querySelector("#display-poll").scrollIntoView({ block: "center" }),
      );
      // The gear's aria-current border fades in over 0.15 s. A side whose dot is already live
      // would otherwise catch it mid-transition.
      await page.evaluate(() =>
        Promise.all(
          document
            .getElementById("settings-link")
            .getAnimations()
            .map((animation) => animation.finished),
        ),
      );
    };
    await view("display-poll-settings", 1280, 900, "light", openPollSettings);
    const openInstallSettings = async (page, ios) => {
      await page.evaluate((ios) => {
        if (ios)
          Object.defineProperty(navigator, "userAgent", {
            get: () =>
              "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
          });
        else {
          const e = new Event("beforeinstallprompt");
          e.prompt = async () => {};
          e.userChoice = Promise.resolve({ outcome: "dismissed" });
          dispatchEvent(e);
        }
        location.hash = "#/settings/display";
      }, ios);
      await page.waitForSelector("#settings-display:not([hidden])");
      await page.evaluate(() =>
        document.querySelector("#display-reset").scrollIntoView({ block: "start" }),
      );
    };
    await view("display-install-desktop", 1280, 900, "light", (page) =>
      openInstallSettings(page, false),
    );
    await view(
      "display-install-ios",
      390,
      844,
      "light",
      (page) => openInstallSettings(page, true),
      true,
    );
    const starCapture = async (page) => {
      await page.evaluate(() =>
        localStorage.setItem("factory-starred-jobs", JSON.stringify(["001-capture"])),
      );
      await page.reload({ waitUntil: "networkidle0" });
      await page.waitForSelector('.card[data-id="001-capture"]');
      await waitLiveDot(page);
    };
    await view("starred-board-1280", 1280, 900, "light", starCapture);
    await view("starred-drawer-390", 390, 844, "dark", async (page, side) => {
      await starCapture(page);
      await openDrawer(page, side);
      return clipOf(page, ".detail-head");
    });
    await view("starred-display-1280", 1280, 900, "light", async (page, side) => {
      await starCapture(page);
      await page.evaluate(() => {
        location.hash = "#/settings/display";
      });
      await page.waitForSelector("#settings-display:not([hidden])");
      await page.evaluate(() =>
        document.querySelector("#display-filter").scrollIntoView({ block: "start" }),
      );
      await page.evaluate(() =>
        Promise.all(
          document
            .getElementById("settings-link")
            .getAnimations()
            .map((animation) => animation.finished),
        ),
      );
      // The reference page has no starred setting, so its crop ends at the project filter.
      return clipOf(
        page,
        "#display-filter",
        side === "after" ? "#display-starred-help" : "#display-filter",
      );
    });
    await view("starred-only-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => localStorage.setItem("factory-starred-view", "only"));
      await starCapture(page);
    });
    await view("starred-filter-hidden-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        localStorage.setItem("factory-starred-jobs", JSON.stringify(["001-capture"]));
        localStorage.setItem("factory-starred-view", "only");
        localStorage.setItem("factory-projects-hidden", JSON.stringify(["sample"]));
      });
      await page.reload({ waitUntil: "networkidle0" });
      await page.waitForSelector("#starred-only");
      await waitLiveDot(page);
    });
    const openSchedulerSettings = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/scheduler";
      });
      await page.waitForSelector("#settings-scheduler:not([hidden])");
      await page.evaluate(() =>
        document.querySelector("#scheduler-settings-heading").scrollIntoView({ block: "start" }),
      );
      await page.evaluate(() =>
        Promise.all(
          document
            .getElementById("settings-link")
            .getAnimations()
            .map((animation) => animation.finished),
        ),
      );
    };
    await view("scheduler-settings", 1280, 900, "light", openSchedulerSettings);
    await view("scheduler-settings-mobile", 390, 844, "light", openSchedulerSettings);
    await view("scheduler-settings-human", 1280, 900, "light", async (page) => {
      await openSchedulerSettings(page);
      await page.evaluate(() => document.getElementById("scheduler-mode-tab-human")?.click());
    });
    const openNotificationSettings = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/notifications";
      });
      await page.waitForSelector("#settings-notifications:not([hidden])");
      await page.evaluate(() =>
        document.querySelector("#notify-settings-heading").scrollIntoView({ block: "start" }),
      );
      await page.evaluate(() =>
        Promise.all(
          document
            .getElementById("settings-link")
            .getAnimations()
            .map((animation) => animation.finished),
        ),
      );
    };
    await view("notify-settings-desktop", 1280, 900, "light", openNotificationSettings);
    await view("notify-settings-mobile", 390, 844, "light", openNotificationSettings, true);
    await view("notify-alerts-390", 390, 844, "light", async (page) => {
      await openNotificationSettings(page);
      await page.evaluate(() =>
        document.getElementById("alert-heading").scrollIntoView({ block: "start" }),
      );
    });
    await view("notify-sound-wait-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => localStorage.setItem("factory-sound", "on"));
      await page.reload({ waitUntil: "networkidle0" });
      await page.waitForSelector(".card");
      await openNotificationSettings(page);
      await page.evaluate(() =>
        document.getElementById("sound-heading").scrollIntoView({ block: "start" }),
      );
    });
    const settleArchiveSettings = async (page, name) => {
      try {
        await page.waitForFunction(() => {
          const t = document.getElementById("pipeline-status").textContent;
          return t.startsWith("Fix before saving") || t === "Unsaved changes";
        });
      } catch (error) {
        const seen = await page
          .$eval("#pipeline-status", (el) => el.textContent)
          .catch(() => {
            throw error;
          });
        throw new Error(
          name + ' capture status wait timed out; #pipeline-status read "' + seen + '"',
          {
            cause: error,
          },
        );
      }
      await page.evaluate(() =>
        document.getElementById("pipeline-archive").scrollIntoView({ block: "start" }),
      );
      // Tracer and the stage notes push Status past one 1280x900 screen, so the view clips from Archiving to Status.
      return clipOf(page, "#pipeline-archive", "#pipeline-status");
    };
    const openArchivePrefill = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/pipeline";
      });
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.select("#pipeline-scope", "global");
      await page.waitForFunction(
        () =>
          !document.getElementById("pipeline-archive").hidden &&
          !document.getElementById("pipeline-archive-mode").disabled,
      );
      await page.select("#pipeline-archive-mode", "");
      await page.select("#pipeline-archive-mode", "count");
      return settleArchiveSettings(page, "archive-prefill");
    };
    await view("archive-prefill", 1280, 900, "light", openArchivePrefill);
    const openArchiveSwap = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/pipeline";
      });
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.select("#pipeline-scope", "global");
      await page.waitForFunction(
        () =>
          !document.getElementById("pipeline-archive").hidden &&
          !document.getElementById("pipeline-archive-mode").disabled,
      );
      await page.select("#pipeline-archive-mode", "");
      await page.select("#pipeline-archive-mode", "age");
      await page.select("#pipeline-archive-mode", "count");
      return settleArchiveSettings(page, "archive-swap");
    };
    await view("archive-swap", 1280, 900, "light", openArchiveSwap);
    await view("archived-drawer", 1280, 900, "light", openArchivedDrawer);
    const openArchivedLoadFailed = async (page) => {
      // Both sides wait for the archive answer and then the toast text, so neither shot is taken mid-request.
      const answered = page.waitForResponse((response) => {
        const url = new URL(response.url());
        return (
          url.pathname === "/api/board/archive" && url.searchParams.get("id") === "411-sync-retry"
        );
      });
      await page.evaluate(() => {
        location.hash = "#411-sync-retry";
      });
      await answered;
      await page.waitForFunction(() =>
        document
          .getElementById("board-status")
          .textContent.startsWith("Could not load archived job"),
      );
    };
    await view("archived-load-failed", 1280, 900, "light", openArchivedLoadFailed);
    const openArchivedLoadSignin = async (page) => {
      await page.evaluate(() => {
        location.hash = "#413-expired-link";
      });
      await page.waitForFunction(
        () =>
          !document.getElementById("conn-banner").hidden &&
          document.getElementById("conn-text").textContent === "Your sign-in has expired.",
      );
    };
    await view("archived-load-signin", 1280, 900, "light", openArchivedLoadSignin);
    await view("archived-load-signin-mobile", 390, 844, "dark", openArchivedLoadSignin);
    // The reference has no hold fieldset, so its side shows triage straight above archive.
    const openPipelineHold = async (page, side) => {
      await page.evaluate(() => {
        location.hash = "#/settings/pipeline";
      });
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.select("#pipeline-scope", "global");
      await page.waitForFunction(() => !document.getElementById("pipeline-triage-mode").disabled);
      if (side === "after") {
        await page.select("#pipeline-hold-mode", "on");
        for (const [id, value] of [
          ["#pipeline-hold-min", "3"],
          ["#pipeline-hold-hours", "6"],
        ])
          await page.$eval(
            id,
            (el, v) => {
              el.value = v;
              el.dispatchEvent(new Event("input", { bubbles: true }));
            },
            value,
          );
        await page.waitForFunction(
          () => document.getElementById("pipeline-status").textContent === "Unsaved changes",
        );
      }
      await page.evaluate(() =>
        document.getElementById("pipeline-triage").scrollIntoView({ block: "start" }),
      );
      return clipOf(page, "#pipeline-triage", "#pipeline-archive");
    };
    await view("pipeline-hold-1280", 1280, 900, "light", openPipelineHold);
    // The reference has no trials section, so its side shows the stage notes straight above History.
    const openPipelineTrials = async (page) => {
      trialsBoard = true;
      try {
        await page.evaluate(() => poll());
        await page.evaluate(() => {
          location.hash = "#/settings/pipeline";
        });
        await page.waitForSelector("#settings-pipeline:not([hidden])");
        await page.select("#pipeline-scope", "global");
        await page.waitForFunction(() => !document.getElementById("pipeline-triage-mode").disabled);
        await page.evaluate(() =>
          document.getElementById("pipeline-note-tabs").scrollIntoView({ block: "start" }),
        );
        return clipOf(page, "#pipeline-note-tabs", "#pipeline-history-heading");
      } finally {
        trialsBoard = false;
      }
    };
    await view("pipeline-trials-1280", 1280, 900, "light", openPipelineTrials);
    await view("pipeline-trials-390", 390, 844, "dark", openPipelineTrials);
    // The reference has no backpressure fieldset, so its side shows hold straight above archive.
    const openPipelineBackpressure = async (page, side) => {
      await page.evaluate(() => {
        location.hash = "#/settings/pipeline";
      });
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.select("#pipeline-scope", "global");
      await page.waitForFunction(() => !document.getElementById("pipeline-hold-mode").disabled);
      if (side === "after") {
        await page.select("#pipeline-backpressure-mode", "on");
        await page.$eval("#pipeline-backpressure-needs", (el) => {
          el.value = "3";
          el.dispatchEvent(new Event("input", { bubbles: true }));
        });
        await page.waitForFunction(
          () => document.getElementById("pipeline-status").textContent === "Unsaved changes",
        );
      }
      await page.evaluate(() =>
        document.getElementById("pipeline-hold").scrollIntoView({ block: "start" }),
      );
      return clipOf(page, "#pipeline-hold", "#pipeline-archive");
    };
    await view("pipeline-backpressure-1280", 1280, 900, "light", openPipelineBackpressure);
    const openHeldCard = async (page) => {
      const wrap = '.card-wrap:has(.card[data-id="102-move-comment"])';
      await page.evaluate(
        (s) => document.querySelector(s).scrollIntoView({ block: "center", inline: "center" }),
        wrap,
      );
      return clipOf(page, wrap);
    };
    await view("held-card-390", 390, 844, "dark", openHeldCard, true);
    const openJobTab = (hash, text) => async (page) => {
      await page.evaluate((h) => {
        location.hash = h;
      }, hash);
      await page.waitForFunction(
        (t) =>
          document.getElementById("d-meta").textContent.includes("130-ground-export") &&
          document.getElementById("d-doc").textContent.includes(t),
        { timeout: 30000 },
        text,
      );
    };
    const openGroundingTab = openJobTab(
      "#130-ground-export/round-1/grounding",
      "GROUNDING: UNVERIFIED 2",
    );
    await view("grounding-tab-1280", 1280, 900, "light", openGroundingTab);
    await view("grounding-tab-390", 390, 844, "dark", openGroundingTab, true);
    const openRoundTabs = openJobTab(
      "#130-ground-export/round-2/review-code",
      "The synthetic round 2 code review.",
    );
    await view("round-tabs-1280", 1280, 900, "light", openRoundTabs);
    await view("round-tabs-390", 390, 844, "dark", openRoundTabs);
    // keepViewport, since a full-page capture would lay the board out again and shift the clip.
    await view(
      "grounding-chips-1280",
      1280,
      900,
      "light",
      async (page) => {
        const wrap = '.card-wrap:has(.card[data-id="130-ground-export"])';
        await page.evaluate(
          (s) => document.querySelector(s).scrollIntoView({ block: "center", inline: "center" }),
          wrap,
        );
        return clipOf(page, wrap);
      },
      false,
      true,
    );
    await view("not-before-launch-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        location.hash = "#131-launch-retry";
      });
      await page.waitForFunction(() =>
        document.getElementById("d-meta").textContent.includes("131-launch-retry"),
      );
      return clipOf(page, ".detail-head");
    });
    const openIdeaChips = async (page) => {
      const wrap = (id) => `.card-wrap:has(.card[data-id="${id}"])`;
      await page.evaluate(
        (s) => document.querySelector(s).scrollIntoView({ block: "center", inline: "center" }),
        wrap("107-held-rider"),
      );
      return clipOf(page, wrap("102-move-comment"), wrap("106-chat-approved"));
    };
    await view("idea-chips-390", 390, 844, "dark", openIdeaChips, true);
    await view("idea-chips-1280", 1280, 900, "light", openIdeaChips);
    const openIdeaChipsLoading = async (page) => {
      await page.evaluate(() => {
        clearHoldConfigs();
        renderBoard();
      });
      return openIdeaChips(page);
    };
    await view("idea-chips-loading-1280", 1280, 900, "light", openIdeaChipsLoading);
    const openAgentRuns = async (page) => {
      await page.evaluate(() => {
        location.hash = "#101-wrap-doc-line";
      });
      await page.waitForFunction(
        () => document.getElementById("d-title").textContent === "Synthetic wrap doc line",
      );
      await page.waitForSelector("#d-agents:not([hidden])");
      // A 1200px drawer fits the whole table at 1280, since the screenshot resets any
      // scroll there. At 390 the table scrolls to its right end so the last columns show.
      await page.evaluate(() => {
        document.getElementById("detail").style.setProperty("--drawer-w", "1200px");
        document.getElementById("d-agents").open = true;
      });
      await page.evaluate(() => {
        const wrap = document.querySelector("#d-agents .table-wrap");
        wrap.scrollLeft = wrap.scrollWidth;
      });
      return clipOf(page, ".detail-head");
    };
    await view("agent-runs-390", 390, 844, "dark", openAgentRuns, true);
    await view("agent-runs-1280", 1280, 900, "light", openAgentRuns);
    // The reference page has no Tokens table, so its side shows the drawer head with Agents only.
    const openJobTokens = async (page, side) => {
      await page.evaluate(() => {
        location.hash = "#101-wrap-doc-line";
      });
      await page.waitForFunction(
        () => document.getElementById("d-title").textContent === "Synthetic wrap doc line",
      );
      await page.waitForSelector(
        side === "after" ? "#d-tokens:not([hidden])" : "#d-agents:not([hidden])",
      );
      await page.evaluate(() => {
        document.getElementById("detail").style.setProperty("--drawer-w", "1200px");
        const box = document.getElementById("d-tokens");
        if (!box) return;
        box.open = true;
        const wrap = box.querySelector(".table-wrap");
        wrap.scrollLeft = wrap.scrollWidth;
      });
      return clipOf(page, ".detail-head");
    };
    await view("job-tokens-390", 390, 844, "dark", openJobTokens, true);
    await view("job-tokens-1280", 1280, 900, "light", openJobTokens);
    const openLeadAgents = async (page) => {
      await page.evaluate(() => {
        location.hash = "#101-wrap-doc-line";
      });
      await page.waitForFunction(
        () => document.getElementById("d-title").textContent === "Synthetic wrap doc line",
      );
      await page.waitForSelector("#d-agents:not([hidden])");
      await page.evaluate(() => {
        document.getElementById("d-agents").open = true;
      });
      return clipOf(page, ".detail-head");
    };
    const openLeadAgents360 = async (page) => {
      await openLeadAgents(page);
      await page.evaluate(() => {
        document.getElementById("detail").style.setProperty("--drawer-w", "360px");
      });
      return clipOf(page, ".detail-head");
    };
    leadAgentsBoard = true;
    try {
      await view("agent-runs-lead-390", 390, 844, "dark", openLeadAgents, true);
      await view("agent-runs-lead-1280", 1280, 900, "light", openLeadAgents);
      await view("agent-runs-lead-360", 1280, 900, "light", openLeadAgents360);
    } finally {
      leadAgentsBoard = false;
    }
    writerBoard = true;
    try {
      await view("writer-session-drawer-1280", 1280, 900, "light", async (page) => {
        await page.evaluate(() => {
          location.hash = "#002-needs";
        });
        await page.waitForFunction(
          () => document.getElementById("d-title").textContent === "Synthetic blocked job",
        );
        return clipOf(page, ".detail-head");
      });
    } finally {
      writerBoard = false;
    }
    // A phone drawer whose open agents table is taller than the screen, scrolled to its last row.
    const openTallAgents = async (page) => {
      await openLeadAgents(page);
      await page.evaluate(() => {
        const rows = document.querySelectorAll("#d-agents tbody tr");
        const last = rows[rows.length - 1].getBoundingClientRect().bottom;
        document.getElementById("d-doc").scrollTop = last - innerHeight + 8;
      });
    };
    // A desktop drawer whose open agents table is scrolled to its last row.
    const openTallAgentsDesktop = async (page) => {
      await openLeadAgents(page);
      await page.evaluate(() => {
        const wrap = document.querySelector("#d-agents .table-wrap");
        wrap.scrollTop = wrap.scrollHeight;
      });
    };
    tallAgentsBoard = true;
    try {
      await view("drawer-mobile-agents", 390, 844, "dark", openTallAgents, true);
      await view("drawer-desktop-agents", 1280, 900, "light", openTallAgentsDesktop);
    } finally {
      tallAgentsBoard = false;
    }
    const openAudit = async (page, side) => {
      await openDrawer(page, side);
      await page.click('.tab[data-name="audit.md"]');
      await page.waitForSelector("#d-doc table.audit");
      await page.waitForFunction(() => {
        const pill = document.getElementById("d-tab-pill");
        const tab = document.querySelector("#d-tabs .tab.active");
        const style = getComputedStyle(pill);
        return (
          parseFloat(style.left) === tab.offsetLeft && parseFloat(style.width) === tab.offsetWidth
        );
      });
    };
    auditBoard = true;
    try {
      await view("audit-desktop", 1440, 900, "light", openAudit);
      await view("audit-mobile", 390, 844, "dark", openAudit);
    } finally {
      auditBoard = false;
    }
    // Both sides scroll to the Red flags card when the page has one, so each pair frames the same region.
    const openRetrosFlags = async (page, _side) => {
      await page.evaluate(() => (location.hash = "#/retros"));
      await page.waitForFunction(() => {
        const status = document.getElementById("retros-status").textContent;
        return status && !status.startsWith("Loading");
      });
      if (await page.$("#audit-flags-heading")) {
        await page.waitForSelector("#audit-flags-rows tr");
        await page.evaluate(() => {
          if (innerWidth > 600)
            return document
              .getElementById("audit-flags-heading")
              .scrollIntoView({ block: "start" });
          document.getElementById("audit-flags-pager").scrollIntoView({ block: "end" });
          const wrap = document.getElementById("audit-flags-wrap");
          wrap.scrollLeft = wrap.scrollWidth;
        });
      }
    };
    flagsHosts = true;
    try {
      await view("retros-flags-desktop", 1440, 900, "light", openRetrosFlags);
      await view("retros-flags-mobile", 390, 844, "dark", openRetrosFlags);
      await view("retros-flags-web-hidden-desktop", 1440, 900, "light", async (page, side) => {
        await page.evaluate(() => localStorage.setItem("factory-hidden-drawer-links", "web"));
        await page.reload({ waitUntil: "networkidle0" });
        await openRetrosFlags(page, side);
      });
      await view("retros-flags-idea-desktop", 1440, 900, "light", async (page, side) => {
        await openRetrosFlags(page, side);
        if (side !== "after") return;
        await page.click("#audit-flags-rows .btn + .btn");
        await page.waitForSelector("#idea-dlg[open]");
      });
      flagsAckAbort = true;
      await view("retros-flags-offline-desktop", 1440, 900, "light", async (page, side) => {
        await openRetrosFlags(page, side);
        await page.waitForSelector("#audit-flags-rows tr .btn");
        await page.click("#audit-flags-rows tr .btn");
        await page.waitForSelector("#audit-flags-err:not([hidden])");
        await page.evaluate(() =>
          document.getElementById("audit-flags-err").scrollIntoView({ block: "center" }),
        );
      });
    } finally {
      flagsHosts = false;
      flagsAckAbort = false;
    }
    // A drawer opened from Retros or Settings keeps that page beside it.
    const openDrawerFrom = (hash, open) => async (page) => {
      await page.evaluate((h) => {
        location.hash = h;
      }, hash);
      await open(page);
      await page.waitForFunction(
        () =>
          document.querySelector("#d-meta")?.textContent.includes("001-capture") &&
          document.querySelector("#d-doc").textContent !== "loading files",
        { timeout: 30000 },
      );
    };
    await view(
      "retros-split-1280",
      1280,
      900,
      "light",
      openDrawerFrom("#/retros", async (page) => {
        await page.waitForSelector('#audit-flags-rows a[href="#001-capture"]');
        await page.click('#audit-flags-rows a[href="#001-capture"]');
      }),
    );
    await view(
      "settings-split-1280",
      1280,
      900,
      "light",
      openDrawerFrom("#/settings/display", async (page) => {
        await page.waitForSelector("#settings-display:not([hidden])");
        await page.evaluate(() => {
          location.hash = "#001-capture";
        });
      }),
    );
    // A phone drawer on an idea with two attached files, at rest and scrolled 40 px.
    const openIdeaFiles = async (page) => {
      await page.evaluate(() => {
        location.hash = "#101-wrap-doc-line/idea";
      });
      await page.waitForFunction(() => document.querySelectorAll("#d-files a").length === 2);
      await page.waitForFunction(() => {
        const img = document.querySelector("#d-gallery img");
        return img?.complete && img.naturalWidth > 0;
      });
    };
    const openIdeaFilesMid = async (page) => {
      await openIdeaFiles(page);
      await page.evaluate(() => {
        document.getElementById("d-doc").scrollTop = 40;
      });
    };
    ideaFilesBoard = true;
    try {
      await view("drawer-mobile-idea-files", 390, 844, "dark", openIdeaFiles, true);
      await view("drawer-mobile-idea-files-midscroll", 390, 844, "dark", openIdeaFilesMid, true);
    } finally {
      ideaFilesBoard = false;
    }
    // A drawer on an idea with one attached WebM clip; the before side lists only its link.
    const openIdeaVideo = async (page) => {
      await page.evaluate(() => {
        location.hash = "#003-idea/idea";
      });
      await page.waitForSelector("#d-files a");
      await page.waitForFunction(() =>
        [...document.querySelectorAll("#d-files video")].every((v) => v.readyState >= 1),
      );
    };
    videoBoard = true;
    try {
      await view("idea-video-mobile", 390, 844, "light", openIdeaVideo);
      await view("idea-video-desktop", 1280, 900, "light", openIdeaVideo);
    } finally {
      videoBoard = false;
    }
    const openChatDrawer = async (page) => {
      await page.evaluate(() => {
        location.hash = "#106-chat-approved";
      });
      await page.waitForFunction(() =>
        document.getElementById("d-meta").textContent.includes("106-chat-approved"),
      );
      return clipOf(page, ".detail-head");
    };
    await view("chat-drawer-390", 390, 844, "dark", openChatDrawer, true);
    await view("chat-drawer-1280", 1280, 900, "light", openChatDrawer);
    const openHeldDrawer = async (page) => {
      await page.evaluate(() => {
        location.hash = "#107-held-rider";
      });
      await page.waitForFunction(() =>
        document.getElementById("d-meta").textContent.includes("107-held-rider"),
      );
      return clipOf(page, ".detail-head");
    };
    await view("held-drawer-390", 390, 844, "dark", openHeldDrawer, true);
    const openSpecGate = async (page) => {
      await page.evaluate(() => {
        location.hash = "#110-spec-gate";
      });
      await page.waitForFunction(
        () =>
          document.getElementById("d-meta").textContent.includes("110-spec-gate") &&
          document.querySelector("#d-doc").textContent !== "loading files",
        { timeout: 30000 },
      );
    };
    await view("approve-spec-drawer-1280", 1280, 900, "light", async (page) => {
      await openSpecGate(page);
      return clipOf(page, ".detail-head");
    });
    await view("approve-spec-drawer-390", 390, 844, "dark", openSpecGate);
    await view("babysit-drawer-390", 390, 844, "dark", async (page) => {
      await page.evaluate(() => {
        location.hash = "#112-held-pr";
      });
      await page.waitForFunction(
        () =>
          document.getElementById("d-meta").textContent.includes("112-held-pr") &&
          document.getElementById("d-babysit-bar").checkVisibility(),
        { timeout: 30000 },
      );
      return clipOf(page, ".detail-head");
    });
    await view("approve-spec-failed-1280", 1280, 900, "light", async (page) => {
      await openSpecGate(page);
      page.once("dialog", (dialog) => dialog.accept());
      // The board write fails as a dropped connection would.
      await page.evaluate(() => {
        const fetchNow = window.fetch;
        window.fetch = (url, options) =>
          String(url).endsWith("api/board") && options?.method === "PATCH"
            ? Promise.reject(new TypeError("Failed to fetch"))
            : fetchNow(url, options);
      });
      await page.focus("#d-approve-spec-bar");
      await page.keyboard.press("Enter");
      await page.waitForFunction(() => !document.getElementById("d-approve-spec-err").hidden, {
        timeout: 30000,
      });
      return clipOf(page, ".detail-head", "#d-approve-spec-err");
    });
    await view("inbox-spec-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        location.hash = "#/inbox";
      });
      await page.waitForFunction(() =>
        [...document.querySelectorAll("#inbox-list .inbox-meta")].some((el) =>
          el.textContent.includes("110-spec-gate"),
        ),
      );
    });
    await view("inbox-unblock-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        location.hash = "#/inbox";
      });
      await page.waitForFunction(() => {
        const meta = [...document.querySelectorAll("#inbox-list .inbox-meta")];
        return ["111-tracer-gate", "112-held-pr"].every((id) =>
          meta.some((el) => el.textContent.includes(id)),
        );
      });
    });
    const openRoundCap = async (page) => {
      await page.evaluate(() => {
        location.hash = "#113-round-cap";
      });
      await page.waitForFunction(
        () =>
          document.getElementById("d-meta").textContent.includes("113-round-cap") &&
          document.querySelector("#d-doc").textContent !== "loading files",
        { timeout: 30000 },
      );
    };
    await view("resolve-drawer-1280", 1280, 900, "light", async (page) => {
      await openRoundCap(page);
      return clipOf(page, ".detail-head");
    });
    // The reference page has no Rework, so its side shows the drawer alone.
    await view(
      "rework-dialog-390",
      390,
      844,
      "dark",
      async (page) => {
        await openRoundCap(page);
        await page.evaluate(() => document.getElementById("d-rework-bar")?.click());
        await page.waitForFunction(
          () =>
            !document.getElementById("d-rework-bar") || document.getElementById("rework-dlg").open,
          { timeout: 30000 },
        );
        await page.evaluate(() => {
          const note = document.getElementById("rework-note");
          if (note) note.value = "Keep the CSV header order.";
        });
      },
      true,
    );
    await view(
      "rework-dialog-saving-390",
      390,
      844,
      "dark",
      async (page) => {
        await openRoundCap(page);
        await page.evaluate(() => document.getElementById("d-rework-bar")?.click());
        await page.waitForFunction(() => document.getElementById("rework-dlg").open, {
          timeout: 30000,
        });
        await page.evaluate(() => {
          document.getElementById("rework-note").value = "Keep the CSV header order.";
          const real = window.fetch;
          window.fetch = (url, init) =>
            init?.method === "PATCH" ? new Promise(() => {}) : real(url, init);
          document.getElementById("rework-save").click();
        });
        await page.waitForFunction(() => document.getElementById("rework-save").disabled);
      },
      true,
    );
    const openSteered = async (page) => {
      await page.evaluate(() => {
        location.hash = "#114-steer-export";
      });
      await page.waitForFunction(
        () =>
          document.getElementById("d-meta").textContent.includes("114-steer-export") &&
          document.querySelector("#d-doc").textContent !== "loading files",
        { timeout: 30000 },
      );
    };
    // The reference page has no steering notes, so its side shows the drawer head alone.
    await view("steer-drawer-1280", 1280, 900, "light", async (page) => {
      await openSteered(page);
      await page.evaluate(() => {
        const list = document.getElementById("d-steer-notes");
        if (list) list.open = true;
      });
      return clipOf(page, ".detail-head");
    });
    await view("steer-human-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        location.hash = "#132-steer-human";
      });
      await page.waitForFunction(
        () => document.getElementById("d-meta").textContent.includes("132-steer-human"),
        { timeout: 30000 },
      );
      await page.evaluate(() => {
        const list = document.getElementById("d-steer-notes");
        if (list) list.open = true;
      });
      return clipOf(page, ".detail-head");
    });
    // The reference page has no Add steering note, so its side shows the drawer alone.
    await view(
      "steer-dialog-390",
      390,
      844,
      "dark",
      async (page) => {
        await openSteered(page);
        await page.evaluate(() => document.getElementById("d-steer")?.click());
        await page.waitForFunction(
          () => !document.getElementById("d-steer") || document.getElementById("steer-dlg").open,
          { timeout: 30000 },
        );
        await page.evaluate(() => {
          const note = document.getElementById("steer-note");
          if (note) note.value = "Keep the CSV header order the same as the old report.";
        });
      },
      true,
    );
    await view("inbox-resolve-1280", 1280, 900, "light", async (page) => {
      await page.evaluate(() => {
        location.hash = "#/inbox";
      });
      await page.waitForFunction(() =>
        [...document.querySelectorAll("#inbox-list .inbox-meta")].some((el) =>
          el.textContent.includes("113-round-cap"),
        ),
      );
    });
    const openCardActions = (id) => async (page) => {
      const wrap = `.card-wrap:has(.card[data-id="${id}"])`;
      await page.evaluate(
        (s) => document.querySelector(s).scrollIntoView({ block: "center", inline: "center" }),
        wrap,
      );
      await clickNow(page, `.card-actions[data-actions="${id}"]`);
      await page.waitForSelector("#card-actions-menu:not([hidden])");
      return page.evaluate((s) => {
        const a = document.querySelector(s).getBoundingClientRect();
        const b = document.getElementById("card-actions-menu").getBoundingClientRect();
        const x = Math.floor(Math.min(a.left, b.left));
        const y = Math.floor(Math.min(a.top, b.top));
        return {
          x: x + scrollX,
          y: y + scrollY,
          width: Math.ceil(Math.max(a.right, b.right)) - x,
          height: Math.ceil(Math.max(a.bottom, b.bottom)) - y,
        };
      }, wrap);
    };
    await view("held-actions-390", 390, 844, "dark", openCardActions("107-held-rider"), true, true);
    await view(
      "held-actions-1280",
      1280,
      900,
      "light",
      openCardActions("107-held-rider"),
      false,
      true,
    );
    await view(
      "remove-batch-390",
      390,
      844,
      "dark",
      openCardActions("109-batch-rider"),
      true,
      true,
    );
    await view(
      "remove-batch-1280",
      1280,
      900,
      "light",
      openCardActions("109-batch-rider"),
      false,
      true,
    );
    const openReferenceKeys = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/reference";
      });
      await page.waitForSelector("#settings-reference:not([hidden])");
      await page.evaluate(() =>
        document.getElementById("reference-keys-heading").scrollIntoView({ block: "start" }),
      );
    };
    await view("reference-keys-1280", 1280, 900, "light", openReferenceKeys);
    // The reference keeps 001-capture open behind the sign-in banner; the candidate closes it.
    const openArchiveSignin = async (page) => {
      await page.evaluate(() => {
        location.hash = "#001-capture";
      });
      await page.waitForFunction(() =>
        document.getElementById("d-meta").textContent.includes("001-capture"),
      );
      await openArchivedLoadSignin(page);
      await new Promise((resolve) => setTimeout(resolve, 300));
    };
    await view("archive-signin-1280", 1280, 900, "light", openArchiveSignin);
    await view("archive-list", 1280, 900, "light", openArchiveList);
    await view("archive-list-mobile", 390, 844, "dark", openArchiveList);
    const openArchiveListError = async (page, side) => {
      archivePageFails = true;
      try {
        for (const stage of ["merged", "closed"]) {
          const toggle = `.column-toggle[data-stage="${stage}"]`;
          if ((await page.$eval(toggle, (el) => el.getAttribute("aria-expanded"))) === "false")
            await page.click(toggle);
        }
        await page.click('.archive-more[data-stage="closed"]');
        await page.waitForSelector(".archive-error");
        if (side === "after")
          await page.waitForFunction(
            () =>
              document.getElementById("board-status").textContent ===
              "Could not load archived jobs. Try again.",
          );
      } finally {
        archivePageFails = false;
      }
    };
    await view("archive-list-error", 1280, 900, "light", openArchiveListError);
    // The reference has no archive button, so its side shows the plain blank board.
    const openEmptyArchive = async (page) => {
      await page.evaluate(() => {
        jobs = [];
        backlog = [];
        boardArchived = true;
        renderBoard();
      });
    };
    const openEmptyArchiveLoaded = async (page, side) => {
      await openEmptyArchive(page);
      if (side === "after") {
        await page.click(".blank .archive-more");
        await page.waitForSelector('.col.closed .archive-list .card[data-id="413-old-suggestion"]');
        await page.evaluate(() =>
          document.querySelector(".col.closed").scrollIntoView({ block: "start", inline: "start" }),
        );
      }
    };
    await view("empty-archive", 1280, 900, "light", openEmptyArchive);
    await view("empty-archive-loaded-mobile", 390, 844, "dark", openEmptyArchiveLoaded);
    const openArchiveJumper = async (page) => {
      archivedBoard = true;
      try {
        await page.evaluate(() => poll());
        await openJumper(page);
        await page.type("#job-search", "login");
        await clickArchiveSearch(page);
        await page.waitForFunction(() =>
          [...document.querySelectorAll("#job-results .title")].some(
            (el) => el.textContent === "Synthetic login fix",
          ),
        );
      } finally {
        archivedBoard = false;
      }
    };
    await view("jumper-archive-search", 1280, 900, "light", openArchiveJumper);
    const openArchiveEmpty = async (page) => {
      archivedBoard = true;
      try {
        await page.evaluate(() => poll());
        await openJumper(page);
        await page.type("#job-search", "zzz");
        await clickArchiveSearch(page);
        await page.waitForFunction(() =>
          [...document.querySelectorAll("#job-results .meta")].some(
            (el) => el.textContent === 'No archived jobs match "zzz". Pick to search again.',
          ),
        );
      } finally {
        archivedBoard = false;
      }
    };
    await view("jumper-archive-empty", 1280, 900, "light", openArchiveEmpty);
    const openArchiveFailed = async (page) => {
      archivedBoard = true;
      archiveSearchFails = true;
      try {
        await page.evaluate(() => poll());
        await openJumper(page);
        await page.type("#job-search", "zzz");
        await clickArchiveSearch(page);
        await page.waitForFunction(() =>
          [...document.querySelectorAll("#job-results .meta")].some(
            (el) => el.textContent === "Could not search archived jobs. Pick to try again.",
          ),
        );
      } finally {
        archivedBoard = false;
        archiveSearchFails = false;
      }
    };
    await view("jumper-archive-failed", 1280, 900, "light", openArchiveFailed);
    await view("backlog-triage-desktop", 1440, 900, "light", openTriage);
    await view("backlog-triage-mobile", 390, 844, "dark", openTriage);
    await view("backlog-triage-open-desktop", 1440, 900, "light", openTriagePrefetched);
    await view("backlog-triage-open-mobile", 390, 844, "dark", openTriagePrefetched);
    await view("backlog-decide-desktop", 1440, 900, "light", openDecision);
    await view("backlog-decide-mobile", 390, 844, "dark", openDecision);
    await view("backlog-decide-touch", 390, 844, "dark", openDecision, true);
    await view("backlog-questions-mobile", 390, 844, "dark", openQuestions);
    await view("backlog-gate-mobile", 390, 844, "dark", openGate);
    await view("backlog-answered-mobile", 390, 844, "dark", openAnswered);
    await view("backlog-rework-desktop", 1440, 900, "light", openRework);
    await view("backlog-rework-mobile", 390, 844, "dark", openRework);
    const openLesson = async (page) => {
      await page.evaluate(() =>
        [...document.querySelectorAll(".backlog-card")]
          .find((card) => card.textContent.includes("Lesson: Worker route"))
          .click(),
      );
      await page.waitForSelector("#backlog-form");
    };
    await view("backlog-lesson-desktop", 1440, 900, "light", openLesson);
    await view("backlog-lesson-mobile", 390, 844, "dark", openLesson);
    const openInboxLesson = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/inbox";
      });
      await page.waitForFunction(() =>
        [...document.querySelectorAll("#inbox-list .inbox-title")].some((t) =>
          t.textContent.includes("Lesson: Worker route"),
        ),
      );
      await page.evaluate(() =>
        [...document.querySelectorAll("#inbox-list .inbox-title")]
          .find((t) => t.textContent.includes("Lesson: Worker route"))
          .closest("li")
          .scrollIntoView({ block: "center" }),
      );
    };
    await view("inbox-lesson-1280", 1280, 900, "light", openInboxLesson);
    await view("inbox-lesson-390", 390, 844, "dark", openInboxLesson);
    // Both sides refuse the approve. Only the after side shows the link.
    const openRefusal = async (page) => {
      await page.click('.backlog-card[data-index="2"]');
      await page.waitForSelector("#backlog-form");
      await page.evaluate(() => {
        backlog.find((b) => b.raw.includes("weekly report")).context =
          "Depends on: sample: Synthetic follow-on slice (sub-job-of 001-capture)";
      });
      await page.click("input[value=approved]");
      await page.click("#backlog-submit");
      await page.waitForFunction(() =>
        document.getElementById("backlog-status").textContent.includes("first."),
      );
    };
    refusalBacklog = true;
    try {
      await view("backlog-refusal-desktop", 1440, 900, "light", openRefusal);
      await view("backlog-refusal-mobile", 390, 844, "dark", openRefusal);
    } finally {
      refusalBacklog = false;
    }
    // The weekly report waits on the unchecked follow-on slice.
    const openWaits = async (page) => {
      await page.evaluate(() => {
        backlog.find((b) => b.raw.includes("weekly report")).context = [
          "Recommend: approve",
          "",
          "Depends on: sample: Synthetic follow-on slice (sub-job-of 001-capture)",
          "The weekly report needs the follow-on slice first.",
        ].join("\n");
      });
      await page.click('.backlog-card[data-index="2"]');
      await page.waitForSelector("#backlog-form");
    };
    await view("backlog-waits-desktop", 1440, 900, "light", openWaits);
    await view("backlog-waits-mobile", 390, 844, "dark", openWaits);
    // A backlog poll after the refusal. The reference clears the status line.
    const openRefusalPoll = async (page) => {
      await openRefusal(page);
      await page.evaluate(() => syncBacklogDetail());
    };
    refusalBacklog = true;
    try {
      await view("backlog-refusal-poll-mobile", 390, 844, "dark", openRefusalPoll);
    } finally {
      refusalBacklog = false;
    }
    if (failed.length) {
      console.log(JSON.stringify({ result: "FAIL", failed, external }));
      throw new Error(
        `${failed.length} capture views failed: ${failed.map((f) => f.view).join(", ")}` +
          (external.length ? "; external requests during capture: " + external.join(", ") : ""),
      );
    }
    if (external.length)
      throw new Error("external requests during capture: " + external.join(", "));
    console.log(
      JSON.stringify({
        result: "PASS",
        provenance: {
          reference: { path: referencePath, sha256: sha(reference) },
          candidate: { path: "web/dashboard.html", sha256: sha(candidate) },
          board: { jobs: CAPTURE_BOARD.length, sha256: sha(JSON.stringify(CAPTURE_BOARD)) },
          origin,
          viewport: ["1440x900", "1280x900", "390x844", "375x667"],
          themes: ["light", "dark"],
        },
      }),
    );
  } finally {
    await browser.close();
    await service.close();
  }
}

const args = process.argv.slice(2);
if (args.length === 0) {
  await behavior();
} else if (args.length === 4 && args[0] === "--capture" && args[2] === "--reference-html") {
  await capture(args[1], args[3]);
} else {
  console.error(
    "usage: node tests/browser/run.mjs [--capture <outdir> --reference-html <absolute-path>]",
  );
  process.exit(2);
}
