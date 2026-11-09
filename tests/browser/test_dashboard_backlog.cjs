// Backlog cards: offline helper contracts (parse, load, job shapes, check-off, undo, CAS,
// capture_id retry) plus the browser Approve/Deny next-item, gone-line, Rework, a/d/b shortcut
// and keyboard flows against the local Worker. Browser part requires --browser <owned-loopback-origin>.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { waitForBoard } = require("../support/board-ready.cjs");

const root = path.resolve(__dirname, "../..");
const html = fs.readFileSync(path.join(root, "web/dashboard.html"), "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const clone = (x) => JSON.parse(JSON.stringify(x));
const now = "2026-10-27T12:00:00Z";
const CONTEXT = [
  "Recommend: approve",
  "",
  "The operator exports the board by hand today. Job 204 left CSV export out of scope.",
  "",
  "## Question: Export scope",
  "Which jobs go in the file?",
  "Pick: one",
  "- Visible jobs (recommended): Only the cards the current filter shows.",
  "- All jobs: Every job on the board.",
].join("\n");
const GONE = "This backlog item changed elsewhere, so nothing was changed.";
const BACKLOG = [
  "# Backlog",
  "",
  "- [ ] sample: Approve this synthetic item",
  "- [ ] sample: Deny this synthetic item (follow-up-of 001-original-idea)",
  "- [x] sample: Already done item (000-done)",
  "- [ ] sample: Third synthetic item",
  "",
].join("\n");

function helpers(fetch, extra = {}) {
  const context = vm.createContext({
    fetch,
    setTimeout: (f) => f(),
    crypto: { randomUUID: () => "uuid-" + ++context.uuids },
    uuids: 0,
    isoNow: () => now,
    renderBoard: () => context.renders++,
    poll: async () => context.polls++,
    renders: 0,
    polls: 0,
    ...extra,
  });
  vm.runInContext(
    script.slice(
      script.indexOf("function parseNeeds("),
      script.indexOf("function answersMarkdown("),
    ) +
      script.slice(script.indexOf("function ideaSlug("), script.indexOf("let ideaEditor =")) +
      "\nthis.api = { backlogItems, parseBacklogContext, backlogAnswersText, loadBacklog, addBacklogJob, backlogReworkHistory, markBacklogLine, removeBacklogJob," +
      " casBacklog, decideBacklog, backlogBusy, backlogPending, BOARD_UNCHANGED, get backlog() { return backlog; } };",
    context,
  );
  return { api: context.api, context };
}

const item = (raw, project, feature, text, refs = []) => ({ raw, project, feature, text, refs });

function response(body, status = 200, etag = 'W/"1"') {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: (k) => (k === "ETag" ? etag : null) },
    json: async () => clone(body),
    text: async () => (typeof body === "string" ? body : JSON.stringify(body)),
  };
}

// In-memory board.json and backlog.md behind the two API routes, with optional hooks.
function store(board, backlog, hooks = {}) {
  const state = { board, backlog, puts: [] };
  state.fetch = async (url, opts = {}) => {
    const key = url === "./api/board" ? "board" : url === "./api/backlog" ? "backlog" : null;
    assert.ok(key, "unexpected url " + url);
    if (opts.method !== "PUT") {
      assert.equal(opts.cache, "no-store");
      if (hooks.get) await hooks.get(key, state);
      return state[key] === null ? response("", 404) : response(clone(state[key]));
    }
    state.puts.push({ key, headers: opts.headers });
    if (hooks.put) {
      const r = await hooks.put(key, state, opts);
      if (r) return r;
    }
    state[key] = key === "board" ? JSON.parse(opts.body) : opts.body;
    return response("ok");
  };
  return state;
}

async function offline() {
  const { api } = helpers(() => {
    throw new Error("unexpected fetch");
  });
  const parsed = api.backlogItems(
    "# Backlog\r\n- [ ] sample: First item  \r\n- [x] sample: Done (001-x)\n" +
      "- [ ] bad slug: nope\n-  [ ] sample: spaced\n- [ ] sample:\n" +
      "- [ ] other-proj: Second (sub-job-of 041-recipe.import)\n- [ ] sample: Third (blocked-by 002-x)\n",
  );
  assert.deepEqual(clone(parsed), [
    item("- [ ] sample: First item  ", "sample", "First item", "First item"),
    item(
      "- [ ] other-proj: Second (sub-job-of 041-recipe.import)",
      "other-proj",
      "Second (sub-job-of 041-recipe.import)",
      "Second",
      [{ id: "041-recipe.import", kind: "sub-job-of" }],
    ),
    item(
      "- [ ] sample: Third (blocked-by 002-x)",
      "sample",
      "Third (blocked-by 002-x)",
      "Third (blocked-by 002-x)",
    ),
  ]);
  assert.deepEqual(clone(api.backlogItems("")), []);

  // loadBacklog: 200 replaces, 404 empties, anything else keeps the last good list.
  let next;
  const loader = helpers(async (url, opts) => {
    assert.equal(url, "./api/backlog");
    assert.deepEqual(clone(opts), { cache: "no-store" });
    if (next instanceof Error) throw next;
    return next;
  }).api;
  next = response("- [ ] sample: Loaded\n");
  await loader.loadBacklog();
  assert.equal(loader.backlog.length, 1);
  for (const failure of [response("x", 500), response("x", 401), new TypeError("offline")]) {
    next = failure;
    assert.equal(await loader.loadBacklog(), undefined);
    assert.equal(loader.backlog[0].text, "Loaded");
  }
  next = response("not found", 404);
  let bodyRead = false;
  next.text = async () => ((bodyRead = true), "not found");
  await loader.loadBacklog();
  assert.deepEqual(clone(loader.backlog), []);
  assert.ok(bodyRead, "a 404 body is read, so the request finishes for networkidle waits");

  // addBacklogJob: exact shapes, refs only when present, notes trimmed or omitted, dedupe.
  const [first, , third] = parsed;
  const board = { updated: "old", jobs: [{ id: "007-x", stage: "build" }] };
  assert.equal(api.addBacklogJob(board, first, "approved", "", "", "c1", now), "008-first-item");
  assert.deepEqual(clone(board.jobs[1]), {
    id: "008-first-item",
    project: "sample",
    title: "First item",
    idea: "First item",
    stage: "idea",
    updated: now,
    capture_id: "c1",
    backlog: { line: first.raw, decision: "approved", at: now },
  });
  assert.equal(board.updated, now);
  const same = clone(board);
  assert.equal(
    api.addBacklogJob(board, first, "approved", "", "", "c1", "later"),
    "008-first-item",
  );
  assert.deepEqual(clone(board), same, "a retry with the same capture_id adds nothing");
  assert.throws(() => api.addBacklogJob(board, third, "approved", "", "", "c1", now), {
    message:
      "this decision was already saved as 008-first-item for another item, so nothing was changed.",
  });
  assert.equal(
    api.addBacklogJob(board, parsed[1], "denied", "  Not now.\n ", "", "c2", now),
    "009-second",
  );
  assert.deepEqual(clone(board.jobs[2]), {
    id: "009-second",
    project: "other-proj",
    title: "Second",
    idea: "Second",
    stage: "closed",
    updated: now,
    done: now,
    note: "denied from the backlog",
    capture_id: "c2",
    backlog: { line: parsed[1].raw, decision: "denied", at: now, notes: "Not now." },
    refs: [{ id: "041-recipe.import", kind: "sub-job-of" }],
  });
  assert.equal(
    api.addBacklogJob(board, parsed[0], "approved", "  Use the drawer.\n", "", "c4", now),
    "010-first-item",
  );
  assert.deepEqual(clone(board.jobs.at(-1)), {
    id: "010-first-item",
    project: "sample",
    title: "First item",
    idea: "First item\n\nNotes from the backlog decision:\nUse the drawer.",
    stage: "idea",
    updated: now,
    capture_id: "c4",
    backlog: { line: first.raw, decision: "approved", at: now, notes: "Use the drawer." },
  });
  board.jobs.pop();
  api.addBacklogJob(board, third, "denied", " \n ", "", "c3", now);
  assert.deepEqual(clone(board.jobs[3].backlog), {
    line: third.raw,
    decision: "denied",
    at: now,
  });

  // markBacklogLine: marked (CRLF line), already, gone, null.
  const text = "# Backlog\r\n- [ ] sample: First item  \r\n- [ ] sample: First item  \n";
  assert.deepEqual(clone(api.markBacklogLine(text, first, "008-first-item")), {
    text: "# Backlog\r\n- [x] sample: First item (008-first-item)\n- [ ] sample: First item  \n",
    state: "marked",
  });
  const marked = "- [x] sample: First item (008-first-item)\r\n";
  assert.deepEqual(clone(api.markBacklogLine(marked, first, "008-first-item")), {
    text: marked,
    state: "already",
  });
  assert.equal(api.markBacklogLine(marked, first, "009-other").state, "gone");
  assert.equal(api.markBacklogLine("", first, "008-first-item").state, "gone");
  assert.deepEqual(clone(api.markBacklogLine(null, first, "008-first-item")), {
    text: null,
    state: "gone",
  });

  // removeBacklogJob: only an untouched job this decision created.
  const undo = (job) => {
    const b = { updated: "old", jobs: [{ id: "001-a", stage: "build" }, job] };
    const result = api.removeBacklogJob(b, "002-b", "c");
    return [result, b];
  };
  for (const job of [
    { id: "002-b", stage: "idea", capture_id: "c" },
    { id: "002-b", stage: "idea", capture_id: "c", claim: null },
    { id: "002-b", stage: "closed", capture_id: "c", backlog: { decision: "denied" } },
  ]) {
    const [result, b] = undo(job);
    assert.equal(result, "removed");
    assert.deepEqual(clone(b), { updated: now, jobs: [{ id: "001-a", stage: "build" }] });
  }
  for (const job of [
    { id: "002-b", stage: "idea", capture_id: "c", claim: { device: "d" } },
    { id: "002-b", stage: "spec", capture_id: "c" },
    { id: "002-b", stage: "idea", capture_id: "other" },
    { id: "002-b", stage: "closed", capture_id: "c", backlog: { decision: "approved" } },
    { id: "003-c", stage: "idea", capture_id: "c" },
  ]) {
    const [result, b] = undo(clone(job));
    assert.equal(result, "kept");
    assert.deepEqual(clone(b), { updated: "old", jobs: [{ id: "001-a", stage: "build" }, job] });
  }

  // casBacklog: conditional Markdown PUT only on marked, 412 retry, uncertain classification.
  const mark = (t) => api.markBacklogLine(t, first, "008-first-item");
  let s = store(null, "- [ ] sample: First item  \n");
  const h = helpers(s.fetch).api;
  assert.equal(await h.casBacklog(mark), "marked");
  assert.deepEqual(clone(s.puts), [
    {
      key: "backlog",
      headers: { "Content-Type": "text/markdown; charset=utf-8", "If-Match": '"1"' },
    },
  ]);
  assert.equal(s.backlog, "- [x] sample: First item (008-first-item)\n");
  assert.equal(await h.casBacklog(mark), "already");
  assert.equal(s.puts.length, 1, "already writes nothing");
  s = store(null, null);
  assert.equal(await helpers(s.fetch).api.casBacklog(mark), "gone");
  assert.equal(s.puts.length, 0);
  let conflicts = 2;
  s = store(null, "- [ ] sample: First item  \n", {
    put: () => (conflicts-- > 0 ? response("", 412) : null),
  });
  assert.equal(await helpers(s.fetch).api.casBacklog(mark), "marked");
  assert.equal(s.puts.length, 3);
  for (const [failure, uncertain] of [
    [new TypeError("network down"), true],
    [response("x", 503), true],
    [response("x", 400), false],
  ]) {
    s = store(null, "- [ ] sample: First item  \n", {
      put: () => {
        if (failure instanceof Error) throw failure;
        return failure;
      },
    });
    await assert.rejects(helpers(s.fetch).api.casBacklog(mark), (x) => {
      assert.equal(!!x.uncertain, uncertain);
      return true;
    });
  }
  await assert.rejects(helpers(async () => response("", 500)).api.casBacklog(mark), {
    message: "reading the backlog failed: 500",
  });

  // decideBacklog: board first, then the line; an uncertain failure keeps the capture_id,
  // so the retry reuses the same job; a vanished line removes the job it created.
  const backlogText = "- [ ] sample: First item  \n- [ ] sample: Third (blocked-by 002-x)\n";
  let lose = 1;
  s = store({ updated: "old", jobs: [] }, backlogText, {
    put: (key) => {
      if (key === "backlog" && lose-- > 0) {
        throw new TypeError("connection reset");
      }
      return null;
    },
  });
  const ctx = helpers(s.fetch).api;
  await assert.rejects(ctx.decideBacklog(first, "approved", "", ""), {
    message: "Not confirmed: connection reset. Try again. It will not add a duplicate.",
  });
  assert.equal(ctx.backlogPending.get(first.raw), "uuid-1");
  assert.equal(ctx.backlogBusy.size, 0);
  await ctx.decideBacklog(first, "approved", "", "");
  assert.equal(s.board.jobs.length, 1, "retry after an uncertain write adds no duplicate");
  assert.equal(s.board.jobs[0].capture_id, "uuid-1");
  assert.equal(ctx.backlogPending.size, 0);
  assert.equal(
    s.backlog,
    "- [x] sample: First item (001-first-item)\n- [ ] sample: Third (blocked-by 002-x)\n",
  );
  const busy = [];
  s = store({ updated: "old", jobs: [] }, backlogText, {
    get: (key, st) => {
      busy.push(ctx2.backlogBusy.has(third.raw));
      if (key === "backlog") st.backlog = "- [ ] sample: First item  \n";
    },
  });
  const ctx2 = helpers(s.fetch).api;
  await assert.rejects(ctx2.decideBacklog(third, "denied", "notes", ""), { message: GONE });
  assert.deepEqual(s.board.jobs, [], "the job it created is removed");
  assert.ok(busy.every(Boolean), "buttons stay busy until the decision settles");
  assert.equal(ctx2.backlogBusy.size, 0);
  assert.equal(ctx2.backlogPending.size, 0);
  s = store({ updated: "old", jobs: [] }, backlogText, {
    get: (key, st) => {
      if (key !== "backlog") return;
      st.backlog = "";
      if (st.board.jobs[0]) st.board.jobs[0].claim = { device: "d" };
    },
  });
  await assert.rejects(helpers(s.fetch).api.decideBacklog(first, "approved", "", ""), {
    message: "001-first-item was saved, but its backlog line changed elsewhere. Check backlog.md.",
  });
  assert.equal(s.board.jobs.length, 1, "a claimed job is kept");

  // Rework: an idea job with trimmed, required notes; history follows rework-of links.
  const csv = item(
    "- [ ] sample: Add CSV export to reports",
    "sample",
    "Add CSV export to reports",
    "Add CSV export to reports",
  );
  const reworkBoard = { updated: "old", jobs: [{ id: "001-x", stage: "build" }] };
  assert.equal(
    api.addBacklogJob(
      reworkBoard,
      csv,
      "rework",
      "  Limit it to the weekly report and say which columns.  ",
      "",
      "c5",
      now,
    ),
    "002-add-csv-export",
  );
  const rework = {
    id: "002-add-csv-export",
    project: "sample",
    title: "Add CSV export to reports",
    idea: "Add CSV export to reports",
    stage: "idea",
    updated: now,
    note: "rework requested from the backlog",
    capture_id: "c5",
    backlog: {
      line: csv.raw,
      decision: "rework",
      at: now,
      notes: "Limit it to the weekly report and say which columns.",
    },
  };
  assert.deepEqual(clone(reworkBoard.jobs[1]), rework);
  const later = {
    ...rework,
    id: "003-add-csv-export",
    idea: "Add weekly CSV export",
    backlog: { ...rework.backlog, at: "later", notes: "Name the columns." },
    refs: [{ id: "002-add-csv-export", kind: "rework-of" }],
  };
  const reworked = (id) => ({ refs: [{ id, kind: "rework-of" }] });
  assert.deepEqual(
    clone(api.backlogReworkHistory(reworked("003-add-csv-export"), [rework, later])),
    [
      {
        id: "003-add-csv-export",
        at: "later",
        text: "Add weekly CSV export",
        notes: "Name the columns.",
      },
      {
        id: "002-add-csv-export",
        at: now,
        text: "Add CSV export to reports",
        notes: "Limit it to the weekly report and say which columns.",
      },
    ],
  );
  const loop = { ...rework, refs: [{ id: "002-add-csv-export", kind: "rework-of" }] };
  assert.equal(api.backlogReworkHistory(reworked("002-add-csv-export"), [loop]).length, 1);
  const approved = { ...rework, backlog: { ...rework.backlog, decision: "approved" } };
  assert.deepEqual(clone(api.backlogReworkHistory(reworked("002-add-csv-export"), [approved])), []);
  console.log(
    "PASS backlog offline helpers: parse/load, exact job shapes, marked/already/gone, conditional undo, CAS retry and capture_id dedupe",
  );
}

async function offlineContext() {
  const { api } = helpers(() => {
    throw new Error("unexpected fetch");
  });
  const indented = CONTEXT.split("\n").map((l) => (l ? "  " + l : l));
  const items = api.backlogItems(
    [
      "# Backlog",
      "",
      "- [ ] sample: With context (sub-job-of 001-x)",
      "",
      ...indented,
      "",
      "",
      "- [ ] sample: No context",
      "",
      "- [x] sample: Done (002-y)",
      "  Recommend: deny",
      "  Reasons.",
      "- [ ] sample: Spaces only",
      "  ",
      "- [ ] sample: Last",
    ].join("\r\n"),
  );
  assert.deepEqual(clone(items.map((i) => i.feature.split(" (")[0])), [
    "With context",
    "No context",
    "Spaces only",
    "Last",
  ]);
  const [one, two, three] = items;
  assert.equal(one.context, CONTEXT);
  assert.equal(one.text, "With context");
  assert.equal("context" in two, false, "the context stops at the next item line");
  assert.equal("context" in three, false, "blank-only indented lines are no context");

  const parsed = clone(api.parseBacklogContext(CONTEXT));
  assert.equal(parsed.recommend, "approved");
  assert.equal(
    parsed.reasons,
    "The operator exports the board by hand today. Job 204 left CSV export out of scope.",
  );
  assert.equal(parsed.questions.length, 1);
  assert.deepEqual(
    parsed.questions[0].options.map((o) => [o.label, o.recommended]),
    [
      ["Visible jobs", true],
      ["All jobs", false],
    ],
  );
  assert.equal(
    api.parseBacklogContext("Recommend: deny\r\nNo.\r\n## Other\r\nmore\r\n").recommend,
    "denied",
  );
  assert.deepEqual(clone(api.parseBacklogContext("\n\nRecommend: deny\nNo.\n\n")), {
    recommend: "denied",
    reasons: "No.",
    questions: [],
  });
  const q = (n) => "## Question: Q" + n + "\nPrompt?\nPick: one\n- A: a\n- B: b";
  for (const bad of [
    "",
    "Recommend: maybe\nreasons",
    "Recommend: approve\n",
    "Recommend: approve\n" + q(1),
    "Recommend: approve\nreasons\n" + [1, 2, 3, 4].map(q).join("\n"),
    "Recommend: approve\nreasons\n" + q(1) + "\n## Notes\nlater",
    "Recommend: approve\nreasons\n" + q(1).replace("Pick: one", "Pick: some"),
    "Recommend: approve\n" + "x".repeat(6000),
  ])
    assert.equal(api.parseBacklogContext(bad), null, JSON.stringify(bad.slice(0, 60)));
  assert.equal(
    api.parseBacklogContext("Recommend: approve\nreasons\n" + [1, 2, 3].map(q).join("\n")).questions
      .length,
    3,
  );

  const [scope] = parsed.questions;
  const text = (picked, other) => api.backlogAnswersText([scope], [{ picked, other }]);
  assert.equal(
    text([0], " Skip   closed\njobs. "),
    "- Export scope: Visible jobs. Other: Skip closed jobs.",
  );
  assert.equal(text([0], ""), "- Export scope: Visible jobs");
  assert.equal(text([], "Skip closed jobs."), "- Export scope: Other: Skip closed jobs.");
  assert.equal(text([], "  "), "");
  assert.equal(text([0, 1], ""), "- Export scope: Visible jobs, All jobs");
  assert.equal(api.backlogAnswersText([], []), "");
  assert.equal(
    api.backlogAnswersText(
      [scope, { ...scope, header: "Second" }],
      [
        { picked: [], other: "" },
        { picked: [1], other: "" },
      ],
    ),
    "- Export scope: no answer\n- Second: All jobs",
  );

  const answers = text([0], "Skip closed jobs.");
  const board = { updated: "old", jobs: [] };
  api.addBacklogJob(board, one, "approved", "Use the drawer.", "  " + answers + "\n", "c1", now);
  assert.deepEqual(clone(board.jobs[0]), {
    id: "001-with-context",
    project: "sample",
    title: "With context",
    idea:
      "With context\n\nNotes from the backlog decision:\nUse the drawer." +
      "\n\nAnswers from the backlog decision:\n" +
      answers,
    stage: "idea",
    updated: now,
    capture_id: "c1",
    backlog: { line: one.raw, decision: "approved", at: now, notes: "Use the drawer.", answers },
    refs: [{ id: "001-x", kind: "sub-job-of" }],
  });
  api.addBacklogJob(board, one, "denied", "", answers, "c2", now);
  assert.deepEqual(clone(board.jobs[1]), {
    id: "002-with-context",
    project: "sample",
    title: "With context",
    idea: "With context",
    stage: "closed",
    updated: now,
    done: now,
    note: "denied from the backlog",
    capture_id: "c2",
    backlog: { line: one.raw, decision: "denied", at: now, answers },
    refs: [{ id: "001-x", kind: "sub-job-of" }],
  });
  api.addBacklogJob(board, one, "approved", "", answers, "c3", now);
  assert.equal(
    board.jobs[2].idea,
    "With context\n\nAnswers from the backlog decision:\n" + answers,
  );
  api.addBacklogJob(board, one, "rework", "Narrow it.", answers, "c4", now);
  assert.deepEqual(clone(board.jobs[3].backlog), {
    line: one.raw,
    decision: "rework",
    at: now,
    notes: "Narrow it.",
    answers,
  });
  assert.equal(board.jobs[3].idea, "With context");
  console.log("PASS backlog offline context");
}

async function putText(url, route, body, type) {
  const current = await fetch(url + route);
  assert.ok(current.ok || current.status === 404);
  const saved = await fetch(url + route, {
    method: "PUT",
    headers: {
      "Content-Type": type,
      ...(current.ok ? { "If-Match": current.headers.get("ETag") } : { "If-None-Match": "*" }),
    },
    body,
  });
  assert.equal(saved.status, 200);
}

async function browserTest(url) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(url);
  const puppeteer = require("puppeteer-core");
  await support.seedProject(
    url,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Sample project\n---\n",
  );
  const JOBS = [
    {
      id: "001-original-idea",
      project: "sample",
      title: "Original idea",
      idea: "Original idea",
      stage: "idea",
      updated: "2026-10-26T12:00:00Z",
    },
  ];
  const seed = async (backlog) => {
    await support.seedBoard(url, clone(JOBS));
    await putText(url, "/api/backlog", backlog, "text/markdown; charset=utf-8");
  };
  const read = async (route) => {
    const r = await fetch(url + route);
    assert.equal(r.status, 200);
    return route === "/api/board" ? r.json() : r.text();
  };
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  try {
    const page = await browser.newPage();
    await page.evaluateOnNewDocument(() => {
      localStorage.clear();
      const interval = window.setInterval;
      window.setInterval = (fn, ms, ...args) => (ms === 3000 ? 0 : interval(fn, ms, ...args));
    });
    const pageErrors = [],
      external = [],
      dialogs = [];
    let route = null;
    page.on("pageerror", (error) => pageErrors.push(String(error)));
    page.on("dialog", async (dialog) => {
      dialogs.push([dialog.type(), dialog.message()]);
      await dialog.accept();
    });
    await page.setBypassServiceWorker(true);
    await page.setRequestInterception(true);
    page.on("request", async (request) => {
      if (!request.url().startsWith(url + "/")) {
        external.push(request.url());
        return request.abort();
      }
      if (route && (await route(request))) return;
      return request.continue();
    });
    const load = async (count) => {
      await page.goto(url + "/dashboard.html", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.waitForFunction(
        (n) => document.querySelectorAll("#column-cards-backlog .backlog-card").length === n,
        { polling: 50 },
        count,
      );
    };
    const cards = () =>
      page.$$eval("#column-cards-backlog .backlog-card", (els) =>
        els.map((el) => ({
          proj: el.querySelector(".proj").textContent,
          title: el.querySelector(".title").textContent,
          refs: el.querySelector(".refs")?.textContent ?? null,
          label: el.getAttribute("aria-label"),
          tag: el.tagName,
        })),
      );
    const settle = (count) =>
      page.waitForFunction(
        (n) =>
          document.querySelectorAll("#column-cards-backlog .backlog-card").length === n &&
          !document.querySelector("#column-cards-backlog button:disabled"),
        { polling: 50 },
        count,
      );
    const open = async (index) => {
      await page.click(`#column-cards-backlog .backlog-card:nth-child(${index})`);
      await page.waitForSelector("#backlog-form");
    };
    const decide = async (value, notes) => {
      await page.click(`input[value=${value}]`);
      if (notes) await page.type("#backlog-notes", notes);
      await page.click("#backlog-submit");
    };
    const next = (text) =>
      page.waitForFunction(
        (t) => document.getElementById("d-doc").textContent.includes(t),
        {
          polling: 50,
        },
        text,
      );
    const shown = async (title) => {
      assert.equal(await page.evaluate(() => location.hash), "");
      assert.equal(
        await page.$eval(
          "#column-cards-backlog .backlog-card.selected .title",
          (e) => e.textContent,
        ),
        title,
      );
    };

    await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
    await seed(BACKLOG);
    await load(3);
    assert.deepEqual(
      (await page.$$eval(".col", (els) => els.map((el) => el.classList[1]))).slice(0, 2),
      ["backlog", "idea"],
    );
    assert.equal(
      await page.$eval(
        '.column-toggle[data-stage="backlog"] span:nth-child(2)',
        (e) => e.textContent,
      ),
      "3",
    );
    assert.deepEqual(await cards(), [
      {
        proj: "Sample project",
        title: "Approve this synthetic item",
        refs: null,
        label: "Open backlog item: Approve this synthetic item",
        tag: "BUTTON",
      },
      {
        proj: "Sample project",
        title: "Deny this synthetic item",
        refs: "follow-up of 001-original-idea",
        label: "Open backlog item: Deny this synthetic item",
        tag: "BUTTON",
      },
      {
        proj: "Sample project",
        title: "Third synthetic item",
        refs: null,
        label: "Open backlog item: Third synthetic item",
        tag: "BUTTON",
      },
    ]);
    await open(1);
    assert.deepEqual(
      await page.$eval("#d-tabs", (e) => [e.hidden, e.offsetHeight]),
      [true, 0],
      "a backlog item has no pages bar",
    );
    assert.equal(await page.$eval("#d-title", (e) => e.textContent), "Backlog item");
    assert.equal(await page.$eval("#d-meta", (e) => e.textContent), "Sample project · backlog");
    assert.ok(
      (await page.$eval("#d-doc", (e) => e.textContent)).includes("Approve this synthetic item"),
    );
    assert.equal(await page.$eval("legend", (e) => e.textContent), "Decision");
    assert.equal(await page.$eval("#backlog-submit", (e) => e.disabled), true);
    assert.equal(
      await page.$eval("#column-cards-backlog .backlog-card", (e) =>
        e.classList.contains("selected"),
      ),
      true,
    );
    await decide("approved", "  Use the drawer.  ");
    await next("Deny this synthetic item");
    await settle(2);
    await shown("Deny this synthetic item");
    assert.deepEqual(dialogs, [], "no confirm or alert fires");
    const approved = (await read("/api/board")).jobs.find((j) => j.id !== "001-original-idea");
    assert.match(approved.capture_id, /^[0-9a-f-]{36}$/);
    assert.match(approved.updated, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
    assert.deepEqual(approved, {
      id: "002-approve-this-synthetic",
      project: "sample",
      title: "Approve this synthetic item",
      idea: "Approve this synthetic item\n\nNotes from the backlog decision:\nUse the drawer.",
      stage: "idea",
      updated: approved.updated,
      capture_id: approved.capture_id,
      backlog: {
        line: "- [ ] sample: Approve this synthetic item",
        decision: "approved",
        at: approved.updated,
        notes: "Use the drawer.",
      },
    });
    assert.ok(
      (await read("/api/backlog")).includes(
        "\n- [x] sample: Approve this synthetic item (002-approve-this-synthetic)\n",
      ),
    );
    assert.equal(await page.$eval("#d-title", (e) => e.textContent), "Backlog item");
    assert.equal(
      await page.$eval(
        '#column-cards-idea .card[data-id="002-approve-this-synthetic"] .title',
        (e) => e.textContent,
      ),
      "Approve this synthetic item",
    );
    assert.equal(
      await page.$eval(
        '.column-toggle[data-stage="backlog"] span:nth-child(2)',
        (e) => e.textContent,
      ),
      "2",
    );
    console.log(
      "PASS backlog browser approve: first column, cards, drawer decision page, no confirm, exact idea job with notes and check-off, next item opens",
    );

    assert.deepEqual(
      await page.$$eval("#d-refs a", (els) =>
        els.map((e) => [e.textContent, e.getAttribute("href")]),
      ),
      [["001-original-idea", "#001-original-idea"]],
    );
    await decide("denied", "  Not now, revisit after launch.  ");
    await next("Third synthetic item");
    await settle(1);
    await shown("Third synthetic item");
    assert.equal(await page.$eval("#backlog-notes", (e) => e.value), "");
    await decide("denied");
    await page.waitForFunction(() => document.getElementById("detail").hidden, { polling: 50 });
    await settle(0);
    assert.equal(await page.evaluate(() => location.hash), "");
    assert.ok(
      await page.evaluate(() =>
        document.activeElement.matches('.column-toggle[data-stage="backlog"]'),
      ),
      "focus returns to the Backlog toggle",
    );
    const jobs = (await read("/api/board")).jobs;
    const denied = jobs.find((j) => j.id === "003-deny-this-synthetic");
    assert.deepEqual(denied, {
      id: "003-deny-this-synthetic",
      project: "sample",
      title: "Deny this synthetic item",
      idea: "Deny this synthetic item",
      stage: "closed",
      updated: denied.updated,
      done: denied.updated,
      note: "denied from the backlog",
      capture_id: denied.capture_id,
      backlog: {
        line: "- [ ] sample: Deny this synthetic item (follow-up-of 001-original-idea)",
        decision: "denied",
        at: denied.updated,
        notes: "Not now, revisit after launch.",
      },
      refs: [{ id: "001-original-idea", kind: "follow-up-of" }],
    });
    const plain = jobs.find((j) => j.id === "004-third-synthetic-item");
    assert.equal(plain.stage, "closed");
    assert.equal("notes" in plain.backlog, false);
    assert.equal(
      await read("/api/backlog"),
      BACKLOG.replace(
        "- [ ] sample: Approve this synthetic item",
        "- [x] sample: Approve this synthetic item (002-approve-this-synthetic)",
      )
        .replace(
          "- [ ] sample: Deny this synthetic item (follow-up-of 001-original-idea)",
          "- [x] sample: Deny this synthetic item (follow-up-of 001-original-idea) (003-deny-this-synthetic)",
        )
        .replace(
          "- [ ] sample: Third synthetic item",
          "- [x] sample: Third synthetic item (004-third-synthetic-item)",
        ),
    );
    assert.equal(await page.$eval("#column-cards-backlog .empty", (e) => e.textContent), "none");
    assert.deepEqual(dialogs, []);
    console.log(
      "PASS backlog browser deny: closed jobs with trimmed notes and without notes, lines checked off, last deny closes the drawer",
    );

    // The line is removed while its item is open.
    await seed("- [ ] sample: Gone synthetic item\n");
    await load(1);
    await open(1);
    await putText(url, "/api/backlog", "# Backlog\n", "text/markdown; charset=utf-8");
    await page.click("input[value=approved]");
    await page.evaluate(() => poll());
    await page.waitForFunction(
      () =>
        document.getElementById("backlog-status").textContent.startsWith("This item is no longer"),
      { polling: 50, timeout: 10000 },
    );
    assert.equal(
      await page.$eval("#backlog-status", (e) => e.textContent),
      "This item is no longer in the backlog. It was decided or edited elsewhere.",
    );
    assert.equal(await page.$eval("#backlog-submit", (e) => e.disabled), true);
    assert.deepEqual((await read("/api/board")).jobs, JOBS, "the board gained no job");
    assert.equal(await read("/api/backlog"), "# Backlog\n");
    console.log(
      "PASS backlog browser gone line: changed-elsewhere copy shown, Submit disabled, no job added",
    );

    await seed("- [ ] sample: Add CSV export to reports\n");
    await load(1);
    await open(1);
    await page.click("input[value=rework]");
    assert.equal(await page.$eval("#backlog-submit", (e) => e.disabled), true);
    assert.equal(
      await page.$eval("#backlog-status", (e) => e.textContent),
      "Add notes that say what to change.",
    );
    assert.equal(
      await page.$eval("#backlog-notes-label", (e) => e.textContent),
      "Notes (required for Rework)",
    );
    await page.type("#backlog-notes", "  Limit it to the weekly report and say which columns.  ");
    assert.deepEqual(await page.$eval("#backlog-submit", (e) => [e.disabled, e.textContent]), [
      false,
      "Rework item",
    ]);
    await page.click("#backlog-submit");
    await page.waitForFunction(() => location.hash.startsWith("#002-add-csv-export"), {
      polling: 50,
    });
    const rework = (await read("/api/board")).jobs.find((j) => j.id === "002-add-csv-export");
    assert.match(rework.capture_id, /^[0-9a-f-]{36}$/);
    assert.deepEqual(rework, {
      id: "002-add-csv-export",
      project: "sample",
      title: "Add CSV export to reports",
      idea: "Add CSV export to reports",
      stage: "idea",
      updated: rework.updated,
      note: "rework requested from the backlog",
      capture_id: rework.capture_id,
      backlog: {
        line: "- [ ] sample: Add CSV export to reports",
        decision: "rework",
        at: rework.updated,
        notes: "Limit it to the weekly report and say which columns.",
      },
    });
    assert.equal(
      await read("/api/backlog"),
      "- [x] sample: Add CSV export to reports (002-add-csv-export)\n",
    );
    await support.seedBoard(url, [...clone(JOBS), { ...rework, stage: "closed" }]);
    await putText(
      url,
      "/api/backlog",
      "- [ ] sample: Add CSV export to the weekly report (rework-of 002-add-csv-export)\n",
      "text/markdown; charset=utf-8",
    );
    await load(1);
    await open(1);
    assert.deepEqual(
      await page.$$eval(".backlog-history li", (els) =>
        els.map((e) => [...e.querySelectorAll("p")].map((p) => p.textContent)),
      ),
      [
        [
          "Earlier text: Add CSV export to reports",
          "Notes: Limit it to the weekly report and say which columns.",
        ],
      ],
    );
    const meta = await page.$eval(".backlog-history .meta", (e) => {
      const style = getComputedStyle(e);
      return [style.fontSize, style.fontFamily, style.color];
    });
    assert.equal(meta[0], "12px");
    assert.match(meta[1], /monospace/);
    assert.equal(
      meta[2],
      await page.$eval("#backlog-form .muted", (e) => getComputedStyle(e).color),
    );
    assert.deepEqual(
      await page.$eval("#backlog-submit", (e) => {
        const style = getComputedStyle(e);
        return [style.whiteSpace, style.flexShrink];
      }),
      ["nowrap", "0"],
    );
    assert.deepEqual(dialogs, []);
    assert.deepEqual(external, []);
    assert.deepEqual(pageErrors, []);
    console.log(
      "PASS backlog browser rework: required notes, idea job with notes, check-off, history from rework-of",
    );

    // b with an empty backlog, and a or d with no item open, change nothing.
    await seed("# Backlog\n");
    await load(0);
    for (const key of ["b", "a", "d"]) await page.keyboard.press(key);
    assert.equal(await page.$eval("#detail", (e) => e.hidden), true);
    await seed(BACKLOG);
    await load(3);
    for (const key of ["a", "d"]) await page.keyboard.press(key);
    assert.equal(await page.$eval("#detail", (e) => e.hidden), true);
    assert.equal(await page.evaluate(() => location.hash), "");
    assert.deepEqual((await read("/api/board")).jobs, JOBS, "no item open, nothing saved");
    assert.equal(await read("/api/backlog"), BACKLOG);
    assert.deepEqual(
      await page.$$eval("#column-cards-backlog [aria-keyshortcuts]", (els) =>
        els.map((e) => [e.getAttribute("aria-label"), e.getAttribute("aria-keyshortcuts")]),
      ),
      [["Open backlog item: Approve this synthetic item", "b"]],
    );
    await page.keyboard.press("b");
    await page.waitForSelector("#backlog-form");
    await shown("Approve this synthetic item");
    assert.equal(
      await page.$eval("#backlog-form p.muted", (e) => e.textContent),
      "Choose one. Press A to approve or D to deny, then the next item opens.",
    );
    assert.deepEqual(
      await page.$$eval("input[name=backlog-pick]", (els) =>
        els.map((e) => [e.value, e.getAttribute("aria-keyshortcuts")]),
      ),
      [
        ["approved", "a"],
        ["denied", "d"],
        ["rework", null],
      ],
    );
    await page.type("#backlog-notes", "abd typed");
    assert.equal(await page.$eval("#backlog-notes", (e) => e.value), "abd typed");
    assert.equal(await page.$("input[name=backlog-pick]:checked"), null, "typing picks nothing");
    await shown("Approve this synthetic item");
    assert.equal(await read("/api/backlog"), BACKLOG, "typing in notes saves nothing");
    await page.focus("input[value=approved]");
    await page.keyboard.press("a");
    await next("Deny this synthetic item");
    await settle(2);
    await shown("Deny this synthetic item");
    assert.ok(
      await page.evaluate(() => document.activeElement.matches("input[value=approved]")),
      "focus lands on the next item's Approve",
    );
    await page.keyboard.press("d");
    await next("Third synthetic item");
    await settle(1);
    await shown("Third synthetic item");
    const keyed = (await read("/api/board")).jobs;
    assert.deepEqual(
      keyed.map((j) => [j.id, j.stage, j.backlog?.decision ?? null, j.backlog?.notes ?? null]),
      [
        ["001-original-idea", "idea", null, null],
        ["002-approve-this-synthetic", "idea", "approved", "abd typed"],
        ["003-deny-this-synthetic", "closed", "denied", null],
      ],
    );
    assert.deepEqual(dialogs, []);
    assert.deepEqual(external, []);
    assert.deepEqual(pageErrors, []);
    console.log(
      "PASS backlog browser shortcuts: b opens item 1, a approves and d denies with the next item opening, typed notes saved, no-op cases, hint and aria-keyshortcuts",
    );

    await page.setViewport({ width: 390, height: 844, deviceScaleFactor: 1 });
    await seed(BACKLOG);
    await load(3);
    for (
      let i = 0;
      i < 40 &&
      !(await page.evaluate(() => document.activeElement.hasAttribute("data-backlog-open")));
      i++
    )
      await page.keyboard.press("Tab");
    assert.equal(
      await page.evaluate(() => document.activeElement.getAttribute("aria-label")),
      "Open backlog item: Approve this synthetic item",
      "the first card is reachable by Tab",
    );
    // A poll rebuilds the cards, and focus must stay on the same card.
    await page.evaluate(async () => {
      window.oldButton = document.activeElement;
      await poll();
    });
    assert.ok(
      await page.evaluate(() => !window.oldButton.isConnected),
      "the poll rebuilt the cards",
    );
    assert.equal(
      await page.evaluate(() => document.activeElement.getAttribute("aria-label")),
      "Open backlog item: Approve this synthetic item",
      "focus survives a poll",
    );
    await page.keyboard.press("Enter");
    await page.waitForSelector("#backlog-form");
    assert.equal(
      await page.evaluate(() => document.activeElement.matches("input[value=approved]")),
      true,
      "focus lands on Approve",
    );
    await page.keyboard.press("ArrowDown");
    assert.equal(await page.$eval("input[value=denied]", (e) => e.checked), true);
    await page.keyboard.press("Tab");
    assert.equal(await page.evaluate(() => document.activeElement.id), "backlog-notes");
    await page.keyboard.press("Tab");
    assert.equal(await page.evaluate(() => document.activeElement.id), "backlog-submit");
    await page.keyboard.press("Escape");
    assert.equal(await page.$eval("#detail", (e) => e.hidden), true);
    assert.equal(
      await page.evaluate(() => document.activeElement.getAttribute("aria-label")),
      "Open backlog item: Approve this synthetic item",
      "focus returns to the card",
    );
    assert.equal(await read("/api/backlog"), BACKLOG, "Escape writes nothing");
    assert.ok(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
      "no mobile overflow",
    );
    assert.deepEqual(dialogs, []);
    assert.deepEqual(external, []);
    assert.deepEqual(pageErrors, []);
    console.log(
      "PASS backlog browser keyboard/mobile: Tab to a card at 390x844, Enter opens, arrows, Tab order and Escape",
    );

    // Context: reasons above Decision, the badge on Approve only, two extra questions.
    const second = [
      "## Question: Columns",
      "Which columns are exported?",
      "Pick: many",
      "- Title: The card title.",
      "- Stage: The stage name.",
    ].join("\n");
    const indent = (t) =>
      t
        .split("\n")
        .map((l) => (l ? "  " + l : l))
        .join("\n");
    await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
    const contextBacklog =
      "# Backlog\n\n- [ ] sample: Export the board\n" +
      indent(CONTEXT + "\n\n" + second) +
      "\n\n- [ ] sample: Broken context\n  Recommend: maybe\n  Not the format.\n";
    await seed(contextBacklog);
    await load(2);
    await open(1);
    const order = await page.$eval("#d-doc", (el) => [
      el.querySelector(".backlog-context").compareDocumentPosition(el.querySelector("legend")) &
        Node.DOCUMENT_POSITION_FOLLOWING,
      el.querySelector(".backlog-context h3").textContent,
      el.querySelector(".backlog-context").textContent,
      [...el.querySelectorAll(".ask-rec")].map((b) => b.closest("label").textContent.slice(0, 8)),
      [...el.querySelectorAll("fieldset.backlog-q legend")].map((l) => l.textContent),
      el.querySelector("#backlog-q-note").textContent,
    ]);
    assert.ok(order[0], "the reasons come before Decision");
    assert.equal(order[1], "Why this item");
    assert.ok(order[2].includes("The operator exports the board by hand today."));
    assert.deepEqual(
      order[3],
      ["Approve ", "Visible "],
      "Recommended on Approve and on the agent's option only",
    );
    assert.deepEqual(order[4], ["Export scope", "Columns"]);
    assert.equal(
      order[5],
      "Questions from the agent. Answer each one before you approve or deny. Your answers are copied to the job this decision creates.",
    );
    assert.equal(await page.$$eval("input:checked", (e) => e.length), 0, "nothing is preselected");
    await page.click("input[value=denied]");
    assert.equal(
      await page.$$eval(
        "input[value=approved]",
        (e) => e[0].closest("label").querySelectorAll(".ask-rec").length,
      ),
      1,
    );
    // The sticky submit bar covers the drawer's bottom band, and a click there would submit.
    // Scroll each option clear of it first, as a person would.
    const tap = async (selector) => {
      await page.$eval(selector, (e) => e.scrollIntoView({ block: "center" }));
      await page.click(selector);
    };
    await tap("input[name=bq0][value='0']");
    await page.type("fieldset.backlog-q textarea", " Skip  closed jobs. ");
    await tap("input[name=bq1][value='0']");
    await tap("input[name=bq1][value='1']");
    await tap("input[value=approved]");
    await page.click("#backlog-submit");
    await next("Not the format.");
    await settle(1);
    const answers =
      "- Export scope: Visible jobs. Other: Skip closed jobs.\n- Columns: Title, Stage";
    const exported = (await read("/api/board")).jobs.find((j) => j.title === "Export the board");
    assert.equal(
      exported.idea,
      "Export the board\n\nAnswers from the backlog decision:\n" + answers,
    );
    assert.equal(exported.backlog.answers, answers);
    assert.ok(
      (await read("/api/backlog")).includes(
        "- [x] sample: Export the board (" + exported.id + ")\n  Recommend: approve\n",
      ),
      "the context stays under the checked line",
    );

    await open(1);
    assert.equal(await page.$(".backlog-context"), null);
    assert.equal(await page.$(".ask-rec"), null, "no badge");
    assert.equal(await page.$("fieldset.backlog-q"), null, "no extra questions");
    const text = await page.$eval("#d-doc", (e) => e.textContent);
    assert.ok(
      text.includes(
        "This item's context is not in the backlog context format, so it shows as text.",
      ),
    );
    assert.ok(text.includes("Not the format."));
    assert.deepEqual(pageErrors, []);
    console.log("PASS backlog browser context");

    // Questions gate Approve and Deny, and a and d walk them.
    await page.setViewport({ width: 390, height: 844, deviceScaleFactor: 1 });
    await seed(contextBacklog);
    await load(2);
    await open(1);
    assert.equal(
      await page.$eval("#backlog-form p.muted", (e) => e.textContent),
      "Choose one. Answer the questions below first, then press A to approve or D to deny, and the next item opens.",
    );
    const submit = () => page.$eval("#backlog-submit", (e) => [e.textContent, e.disabled]);
    const status = () => page.$eval("#backlog-status", (e) => e.textContent);
    const focused = () =>
      page.evaluate(() => {
        const a = document.activeElement;
        return a.matches("input") ? a.name + ":" + a.value : a.id;
      });
    const clear = async () => {
      const [bar, el] = await page.evaluate(() => [
        document.querySelector(".ask-bar").getBoundingClientRect().top,
        document.activeElement.getBoundingClientRect().bottom,
      ]);
      assert.ok(el <= bar, "focus sits above the submit bar");
    };
    const stillBacklog = contextBacklog;
    const gate = "Answer every question to approve or deny. ";
    await tap("input[value=rework]");
    await page.type("#backlog-notes", "Keep");
    assert.deepEqual(await submit(), ["Rework item", false], "Rework stays open with notes");
    await page.$eval("#backlog-notes", (e) => {
      e.value = "";
      e.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await tap("input[value=approved]");
    assert.deepEqual(await submit(), ["Approve item", true]);
    assert.equal(await status(), gate + "0 of 2 answered.");
    await tap("input[value=denied]");
    assert.deepEqual(await submit(), ["Deny item", true]);
    await tap("input[name=bq1][value='0']");
    await page.focus("input[name=bq1][value='1']");
    await page.keyboard.press("d");
    assert.equal(
      await page.$eval("input[name=bq1][value='1']", (e) => e.checked),
      false,
      "Stage stays unticked",
    );
    assert.equal(await focused(), "bq0:0");
    assert.equal(await status(), gate + "1 of 2 answered.");
    await tap("input[name=bq1][value='0']");
    assert.equal(await status(), gate + "0 of 2 answered.");
    await page.focus("input[value=approved]");
    await page.keyboard.press("a");
    assert.equal(await focused(), "bq0:0");
    await clear();
    assert.deepEqual(await submit(), ["Approve item", true]);
    await page.keyboard.press("a");
    assert.equal(await page.$eval("input[name=bq0][value='0']", (e) => e.checked), true);
    assert.equal(await focused(), "bq1:0");
    assert.equal(await status(), gate + "1 of 2 answered.");
    await clear();
    await page.focus("input[name=bq0][value='1']");
    await page.keyboard.press("a");
    assert.deepEqual(await page.$$eval("input[name=bq0]:checked", (l) => l.map((e) => e.value)), [
      "0",
    ]);
    assert.equal(await focused(), "bq1:0");
    await page.keyboard.press("Space");
    assert.deepEqual(await submit(), ["Approve item", false]);
    assert.equal(await status(), "");
    assert.equal(await read("/api/backlog"), stillBacklog, "walking the questions writes nothing");
    console.log("PASS backlog browser shortcut gate");
    console.log("PASS backlog browser shortcut answered question");

    // A failed save keeps the draft.
    route = async (request) => {
      if (request.method() !== "PUT" || !request.url().endsWith("/api/board")) return false;
      await request.respond({ status: 500, body: "no" });
      return true;
    };
    await page.keyboard.press("d");
    await page.waitForFunction(
      () => document.getElementById("backlog-status").textContent.includes("failed"),
      { polling: 50 },
    );
    assert.ok((await status()).includes("saving the board failed: 500"));
    await page.keyboard.press("Escape");
    await open(1);
    assert.equal(await page.$$eval("fieldset.backlog-q", (e) => e.length), 2);
    assert.deepEqual(
      await page.$$eval("input:checked", (e) => e.map((i) => i.name + ":" + i.value)),
      ["backlog-pick:denied", "bq0:0", "bq1:0"],
    );
    console.log("PASS backlog browser shortcut failed save");

    // With every question answered, a approves and copies the answers.
    route = null;
    await page.focus("input[value=approved]");
    await page.keyboard.press("a");
    await next("Not the format.");
    await settle(1);
    const keyedJob = (await read("/api/board")).jobs.find((j) => j.title === "Export the board");
    assert.equal(keyedJob.backlog.answers, "- Export scope: Visible jobs\n- Columns: Title");
    assert.deepEqual(pageErrors, []);
    console.log("PASS backlog browser shortcut answers");

    // On a touch phone, the tap that completes Approve or Deny submits.
    await page.setViewport({
      width: 390,
      height: 844,
      deviceScaleFactor: 1,
      hasTouch: true,
      isMobile: true,
    });
    const touch = async (selector) => {
      await page.$eval(selector, (e) => e.scrollIntoView({ block: "center" }));
      await page.tap(selector);
    };
    const hint = () =>
      page.$eval(".backlog-tap-hint", (e) => [getComputedStyle(e).display, e.textContent]);
    const HINT =
      "On a touch screen, the decision submits by itself once you tap Approve or Deny and every question has an answer. Write any notes first.";
    const keyHint = () => page.$eval(".backlog-key-hint", (e) => getComputedStyle(e).display);
    await seed(BACKLOG);
    await load(3);
    await open(1);
    assert.deepEqual(await hint(), ["block", HINT]);
    assert.equal(await keyHint(), "none");
    await touch("input[value=approved]");
    await next("Deny this synthetic item");
    await settle(2);
    await shown("Deny this synthetic item");
    const tapped = (await read("/api/board")).jobs.find((j) => j.id !== "001-original-idea");
    assert.equal(tapped.id, "002-approve-this-synthetic");
    assert.equal(tapped.stage, "idea");
    assert.equal(tapped.backlog.decision, "approved");
    assert.ok(
      (await read("/api/backlog")).includes(
        "\n- [x] sample: Approve this synthetic item (002-approve-this-synthetic)\n",
      ),
    );
    console.log("PASS backlog browser tap submit");

    const tapBacklog =
      "# Backlog\n\n- [ ] sample: Export the board\n" +
      indent(CONTEXT) +
      "\n\n- [ ] sample: Export the columns\n" +
      indent(CONTEXT + "\n\n" + second) +
      "\n\n- [ ] sample: Broken context\n  Recommend: maybe\n  Not the format.\n";
    await seed(tapBacklog);
    await load(3);
    await open(1);
    await touch("input[value=approved]");
    assert.deepEqual(await submit(), ["Approve item", true]);
    assert.equal(await status(), gate + "0 of 1 answered.");
    assert.equal(await read("/api/backlog"), tapBacklog);
    await touch("input[name=bq0][value='0']");
    await next("Export the columns");
    await settle(2);
    const board = (await read("/api/board")).jobs.find((j) => j.title === "Export the board");
    assert.equal(board.backlog.answers, "- Export scope: Visible jobs");
    console.log("PASS backlog browser tap gate");

    const afterBoard = await read("/api/backlog");
    await touch("input[name=bq0][value='0']");
    await touch("input[value=approved]");
    assert.equal(await status(), gate + "1 of 2 answered.");
    await touch("input[name=bq1][value='0']");
    assert.deepEqual(await submit(), ["Approve item", false], "a checkbox tap does not submit");
    assert.equal(await status(), "");
    assert.equal(await read("/api/backlog"), afterBoard);
    await touch("#backlog-submit");
    await next("Not the format.");
    await settle(1);
    const columns = (await read("/api/board")).jobs.find((j) => j.title === "Export the columns");
    assert.equal(columns.backlog.answers, "- Export scope: Visible jobs\n- Columns: Title");
    const afterColumns = await read("/api/backlog");
    await touch("input[value=rework]");
    assert.deepEqual(await submit(), ["Rework item", true], "Rework does not submit");
    assert.equal(await status(), "Add notes that say what to change.");
    assert.equal(await read("/api/backlog"), afterColumns);
    console.log("PASS backlog browser tap no submit");

    await page.setViewport({ width: 390, height: 844, deviceScaleFactor: 1 });
    await load(1);
    await open(1);
    assert.deepEqual(await hint(), ["none", HINT]);
    assert.equal(await keyHint(), "block");
    await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
    await load(1);
    await open(1);
    assert.deepEqual(await hint(), ["none", HINT]);
    assert.equal(await keyHint(), "block");
    assert.deepEqual(pageErrors, []);
    console.log("PASS backlog browser tap hint");
  } finally {
    await browser.close();
  }
}

(async () => {
  await offline();
  await offlineContext();
  const i = process.argv.indexOf("--browser");
  if (i >= 0) await browserTest(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
