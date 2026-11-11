// Idea create/edit: offline helper and UI contracts plus the full browser integration —
// metadata-preserving edits, pickup/conflict/stale reads, write locking/CAS/retry/
// response loss, paste PNG/gallery/image-bearing pickup, Cmd+K/Meta+Enter/IME,
// unassigned/project/discard flows. Browser part requires --browser <owned-loopback-origin>.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const root = path.resolve(__dirname, "../..");
const html = fs.readFileSync(path.join(root, "web/dashboard.html"), "utf8");
const captureDir = process.env.FACTORY_CAPTURE_DIR;
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
new vm.Script(script, { filename: "dashboard.html" });
const clone = (x) => JSON.parse(JSON.stringify(x));
const before = "Original idea\nKeep the context.";
const draft = "\n  Revised idea 😀  \nKeep every detail.\n";
const now = "2026-10-27T12:00:00Z";
const missing = "This idea is no longer on the board. Your draft is still here.";
const picked = "This idea was picked up or is no longer editable. Your draft is still here.";
const conflict =
  "This idea changed elsewhere. Your draft is still here. Cancel and reopen to review the latest text.";

const { isOwnedBrowserOrigin } = require("../support/owned-origin.cjs");
const { waitForBoard, openJobActions } = require("../support/board-ready.cjs");
const OWNED_ORIGINS = ["http://127.0.0.1:8220", "http://127.0.0.1:8221"];

function assertIdeaBrowserTarget(url) {
  assert.ok(isOwnedBrowserOrigin(url), "browser tests only use owned loopback origins");
}

async function registerIdeaFixture(url, request = fetch) {
  assertIdeaBrowserTarget(url);
  const target = url + "/api/projects/sample.md";
  const current = await request(target, { method: "GET", redirect: "error" });
  assert.equal(current.redirected, false, "fixture GET must not redirect");
  let condition;
  if (current.status === 200) {
    const tag = current.headers.get("ETag");
    assert.ok(
      /^(W\/)?"[\x21\x23-\x7e\x80-\xff]+"$/.test(tag || ""),
      "fixture GET needs an observed ETag",
    );
    condition = { "If-Match": tag.replace(/^W\//, "") };
  } else {
    assert.equal(current.status, 404, "only a genuine missing fixture permits creation");
    condition = { "If-None-Match": "*" };
  }
  return request(target, {
    method: "PUT",
    redirect: "error",
    headers: { "Content-Type": "text/markdown; charset=utf-8", ...condition },
    body: "---\nrepo: example/sample\nbase: main\ntitle: Sample project\n---\n",
  });
}

async function offlineRegistration() {
  const allowed = OWNED_ORIGINS;
  const rejected = [
    undefined,
    "http://localhost:8220",
    "https://127.0.0.1:8220",
    "http://127.0.0.1:8201",
    "http://127.0.0.1:8210",
    "http://127.0.0.1:8220/",
    "http://127.0.0.1:8221/",
    "http://user@127.0.0.1:8220",
    "http://127.0.0.1:8220@other",
    "https://factory.example.test",
    "http://127.0.0.1:8220?x",
    "http://127.0.0.1:8220#x",
  ];
  for (const url of rejected) {
    assert.throws(() => assertIdeaBrowserTarget(url));
    let called = false;
    await assert.rejects(
      registerIdeaFixture(url, async () => {
        called = true;
      }),
    );
    assert.equal(called, false, "reject unsafe target before network or browser work");
  }
  for (const url of allowed) {
    assert.doesNotThrow(() => assertIdeaBrowserTarget(url));
    for (const [status, etag, condition] of [
      [200, '"original"', { "If-Match": '"original"' }],
      [200, 'W/"original"', { "If-Match": '"original"' }],
      [404, null, { "If-None-Match": "*" }],
    ]) {
      const calls = [];
      const putResponse = { status: 200 };
      const result = await registerIdeaFixture(url, async (target, options) => {
        calls.push({ target, options });
        return options.method === "GET"
          ? { status, redirected: false, headers: new Headers(etag ? { ETag: etag } : {}) }
          : putResponse;
      });
      assert.equal(result, putResponse);
      assert.deepEqual(calls, [
        { target: url + "/api/projects/sample.md", options: { method: "GET", redirect: "error" } },
        {
          target: url + "/api/projects/sample.md",
          options: {
            method: "PUT",
            redirect: "error",
            headers: { "Content-Type": "text/markdown; charset=utf-8", ...condition },
            body: "---\nrepo: example/sample\nbase: main\ntitle: Sample project\n---\n",
          },
        },
      ]);
    }
  }
  const target = allowed[1] + "/api/projects/sample.md";
  const getFailures = [
    ...[null, "", "unquoted", '""', '"a", "b"', "*", 'w/"a"'].map((tag) => ({
      status: 200,
      redirected: false,
      headers: new Headers(tag === null ? {} : { ETag: tag }),
    })),
    ...[201, 204, 302, 400, 401, 403, 412, 500].map((status) => ({
      status,
      redirected: false,
      headers: new Headers(),
    })),
    ...[200, 404].map((status) => ({
      status,
      redirected: true,
      headers: new Headers({ ETag: '"a"' }),
    })),
    new TypeError("synthetic GET network error"),
    new TypeError("synthetic GET redirect error"),
  ];
  for (const failure of getFailures) {
    const calls = [];
    await assert.rejects(
      registerIdeaFixture(allowed[1], async (url, options) => {
        calls.push({ url, options });
        if (failure instanceof Error) throw failure;
        return failure;
      }),
    );
    assert.deepEqual(calls, [{ url: target, options: { method: "GET", redirect: "error" } }]);
  }
  for (const failure of [
    { status: 412 },
    { status: 500 },
    new TypeError("synthetic PUT network error"),
    new TypeError("synthetic PUT redirect error"),
  ]) {
    const calls = [];
    const pending = registerIdeaFixture(allowed[1], async (url, options) => {
      calls.push({ url, options });
      if (options.method === "GET")
        return { status: 200, redirected: false, headers: new Headers({ ETag: 'W/"observed"' }) };
      if (failure instanceof Error) throw failure;
      return failure;
    });
    if (failure instanceof Error) await assert.rejects(pending, { message: failure.message });
    else assert.equal(await pending, failure);
    assert.deepEqual(
      calls.map((c) => c.options.method),
      ["GET", "PUT"],
    );
    assert.ok(calls.every((c) => c.url === target && c.options.redirect === "error"));
    assert.deepEqual(calls[1].options.headers, {
      "Content-Type": "text/markdown; charset=utf-8",
      "If-Match": '"observed"',
    });
    assert.equal(
      calls[1].options.body,
      "---\nrepo: example/sample\nbase: main\ntitle: Sample project\n---\n",
    );
  }
  console.log(
    "PASS offline setup: exact owned targets, one observed/absent conditional Markdown registration, strict ETags, redirects/errors and no retry",
  );
}

function fixture() {
  return {
    updated: "2026-10-26T12:00:00Z",
    opaque: { revision: [1, 2] },
    jobs: [
      {
        id: "001-original-idea",
        project: "sample",
        title: "Original idea",
        idea: before,
        stage: "idea",
        updated: "2026-10-26T12:00:00Z",
        branch: "opaque-branch",
        refs: [{ kind: "follow-up-of", id: "000-context" }],
        capture_id: "opaque-capture",
        idea_images: [{ name: "first.png", extra: { keep: true } }, { name: "second.webp" }],
        unknown: { nested: ["keep", { even: null }] },
      },
      { id: "002-other", title: "Other job", idea: "Other job", stage: "build", updated: "old" },
    ],
  };
}

function helpers(
  fetch = () => {
    throw new Error("unexpected fetch");
  },
) {
  const context = vm.createContext({ fetch, setTimeout: (f) => f() });
  vm.runInContext(
    script.slice(script.indexOf("function ideaSlug("), script.indexOf("let ideaEditor =")) +
      "\nthis.api = { canEditIdea, ideaTitle, editIdea, addIdea, casBoard, BOARD_UNCHANGED };",
    context,
  );
  return context.api;
}

function response(board, status = 200, etag = 'W/"1"') {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: { get: () => etag },
    json: async () => clone(board),
    text: async () => "synthetic error <img src=x>",
  };
}

async function offline() {
  const api = helpers();
  for (const claim of [undefined, null]) {
    assert.equal(api.canEditIdea({ stage: "idea", idea: before, claim }), true);
  }
  for (const claim of [{}, false, "", 0, { device: "this-device", seen: "old" }]) {
    assert.equal(api.canEditIdea({ stage: "idea", idea: before, claim }), false);
  }
  for (const stage of ["spec", "build", "closed", undefined])
    assert.equal(api.canEditIdea({ stage, idea: before }), false);
  for (const job of [null, {}, { stage: "idea", idea: null }])
    assert.equal(api.canEditIdea(job), false);
  assert.equal(api.ideaTitle("\n \n  First 😀  \nSecond"), "First 😀");
  assert.equal(api.ideaTitle("😀".repeat(81)), "😀".repeat(80));
  assert.equal(api.ideaTitle("a".repeat(81)), "a".repeat(80));
  assert.equal(api.ideaTitle("a".repeat(79) + "😀x"), "a".repeat(79) + "😀");
  const board = fixture();
  const expected = clone(board);
  Object.assign(expected.jobs[0], { idea: draft, title: "Revised idea 😀", updated: now });
  expected.updated = now;
  const identity = board.jobs[0];
  assert.equal(api.editIdea(board, identity.id, before, draft, now), "updated");
  assert.equal(board.jobs[0], identity);
  assert.deepEqual(board, expected, "only idea/title/job.updated/board.updated change");
  const unchanged = clone(board);
  assert.equal(api.editIdea(board, identity.id, before, draft, "later"), "unchanged");
  assert.deepEqual(board, unchanged);
  for (const [text, message] of [
    [" \n\t", "Write the idea first."],
    ["a".repeat(4001), "Keep the idea to 4000 characters or fewer."],
    ["😀".repeat(2001), "Keep the idea to 4000 characters or fewer."],
  ]) {
    const b = fixture();
    assert.throws(() => api.editIdea(b, b.jobs[0].id, before, text, now), { message });
    assert.deepEqual(b, fixture());
  }
  for (const text of ["a".repeat(4000), "😀".repeat(2000)]) {
    const b = fixture();
    assert.equal(api.editIdea(b, b.jobs[0].id, before, text, now), "updated");
    assert.equal(b.jobs[0].idea, text);
  }
  const create = { jobs: [] };
  api.addIdea(
    create,
    { project: "", text: draft, images: [], capture_id: "07500000-0000-4000-8000-000000000001" },
    now,
  );
  assert.equal(create.jobs[0].title, api.ideaTitle(draft));
  assert.equal(create.jobs[0].idea, draft);
  assert.equal("refs" in create.jobs[0], false);
  const followUp = {
    project: "sample",
    text: "Export the sample report as CSV",
    images: [],
    capture_id: "07500000-0000-4000-8000-000000000002",
    refs: [{ id: "042-sample-export", kind: "follow-up-of" }],
  };
  const followUpId = api.addIdea(create, followUp, now);
  assert.deepEqual(create.jobs.at(-1).refs, [{ id: "042-sample-export", kind: "follow-up-of" }]);
  assert.equal(api.addIdea(create, followUp, now), followUpId);
  assert.equal(create.jobs.length, 2);
  for (const mutate of [
    (b) => {
      b.jobs = [];
    },
    (b) => {
      b.jobs[0].claim = {};
    },
    (b) => {
      b.jobs[0].idea = "Winner";
    },
  ]) {
    const b = fixture();
    mutate(b);
    const winner = clone(b);
    assert.throws(() => api.editIdea(b, "001-original-idea", before, draft, now));
    assert.deepEqual(b, winner);
  }
  const titleOnly = fixture();
  titleOnly.jobs[0].title = "Another derived title";
  assert.equal(api.editIdea(titleOnly, titleOnly.jobs[0].id, before, draft, now), "updated");
  const claimedSame = clone(expected);
  claimedSame.jobs[0].claim = false;
  assert.throws(() => api.editIdea(claimedSame, identity.id, before, draft, now), {
    message: picked,
  });

  async function race(mutate, message) {
    let latest = fixture(),
      puts = 0,
      gets = 0;
    const h = helpers(async (url, opts) => {
      assert.equal(url, "./api/board");
      if (opts.method !== "PUT") {
        gets++;
        assert.equal(opts.cache, "no-store");
        return response(latest);
      }
      puts++;
      assert.equal(opts.headers["If-Match"], '"1"');
      if (puts === 1) {
        mutate(latest);
        return response(null, 412);
      }
      latest = JSON.parse(opts.body);
      return response(null);
    });
    const save = () =>
      h.casBoard((b) =>
        h.editIdea(b, identity.id, before, draft, now) === "unchanged"
          ? h.BOARD_UNCHANGED
          : undefined,
      );
    if (message) {
      const winner = fixture();
      mutate(winner);
      await assert.rejects(save(), { message });
      assert.equal(puts, 1);
      assert.equal(gets, 2);
      assert.deepEqual(latest, winner);
    } else {
      await save();
      assert.equal(puts, 2);
      assert.equal(gets, 2);
      const winner = fixture();
      mutate(winner);
      Object.assign(winner.jobs[0], { idea: draft, title: api.ideaTitle(draft), updated: now });
      winner.updated = now;
      assert.deepEqual(latest, winner);
    }
  }
  await race((b) => {
    b.jobs[0].claim = { device: "agent", seen: "old" };
  }, picked);
  await race((b) => {
    b.jobs[0].idea = "Concurrent requirements";
  }, conflict);
  await race((b) => {
    b.jobs.shift();
  }, missing);
  await race((b) => {
    b.jobs[0].project = "assigned-concurrently";
    b.jobs[1].note = "unrelated";
    b.opaque.revision.push(3);
  });

  for (const kind of ["missing-etag", "ten-412", "put-500", "get-500"]) {
    let puts = 0,
      gets = 0;
    const h = helpers(async (url, opts) => {
      if (opts.method !== "PUT") {
        gets++;
        return response(
          fixture(),
          kind === "get-500" ? 500 : 200,
          kind === "missing-etag" ? null : '"1"',
        );
      }
      puts++;
      return response(null, kind === "ten-412" ? 412 : 500);
    });
    await assert.rejects(
      h.casBoard((b) => h.editIdea(b, identity.id, before, draft, now)),
      new RegExp(
        {
          "missing-etag": "without an ETag",
          "ten-412": "after 10 tries",
          "put-500": "saving the board failed: 500",
          "get-500": "reading the board failed: 500",
        }[kind],
      ),
    );
    assert.equal(puts, { "missing-etag": 0, "ten-412": 10, "put-500": 1, "get-500": 0 }[kind]);
    assert.equal(gets, kind === "ten-412" ? 10 : 1);
  }
  let latest = fixture(),
    puts = 0;
  const h = helpers(async (url, opts) => {
    if (opts.method !== "PUT") return response(latest);
    puts++;
    latest = JSON.parse(opts.body);
    throw new Error("response lost");
  });
  const save = () =>
    h.casBoard((b) =>
      h.editIdea(b, identity.id, before, draft, now) === "unchanged"
        ? h.BOARD_UNCHANGED
        : undefined,
    );
  await assert.rejects(save(), /response lost/);
  const committed = clone(latest);
  await save();
  assert.equal(puts, 1);
  assert.deepEqual(latest, committed);
  latest.jobs[0].claim = {};
  await assert.rejects(save(), { message: picked });
  assert.equal(puts, 1);
  const brokenBody = helpers(async (url, opts) =>
    opts.method !== "PUT"
      ? response(fixture())
      : {
          ok: false,
          status: 503,
          text: async () => {
            throw new Error("body interrupted");
          },
        },
  );
  await assert.rejects(
    brokenBody.casBoard((b) => b.jobs.push({ id: "new" })),
    (error) => error.uncertain === true && error.message === "saving the board failed: 503",
  );
  console.log(
    "PASS offline: actual helpers, validation, metadata, retries, pickup/conflict refusal and response-loss no-op",
  );
}

function deferred() {
  let resolve;
  const promise = new Promise((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

function fakeUI(fetch) {
  const elements = new Map();
  const document = {
    title: "factory",
    activeElement: null,
    getElementById(id) {
      if (!elements.has(id))
        elements.set(id, {
          id,
          value: "",
          hidden: false,
          disabled: false,
          open: false,
          textContent: id === "factory-config" ? '{"image_origins":[],"agent_links":{}}' : "",
          isConnected: true,
          listeners: {},
          style: { removeProperty() {}, setProperty() {} },
          addEventListener(type, fn) {
            this.listeners[type] = fn;
          },
          setAttribute() {},
          getClientRects() {
            return this.hidden ? [] : [{}];
          },
          focus() {
            document.activeElement = this;
          },
          showModal() {
            this.open = true;
          },
          close() {
            this.open = false;
          },
        });
      return elements.get(id);
    },
    addEventListener() {},
    querySelector() {
      return null;
    },
  };
  const context = vm.createContext({
    fetch,
    document,
    location: { hash: "", pathname: "/", search: "" },
    history: { replaceState() {} },
    window: { addEventListener() {} },
    matchMedia: () => ({ matches: false, addEventListener() {} }),
    ResizeObserver: class {
      observe() {}
    },
    localStorage: {
      getItem() {
        return null;
      },
    },
    setTimeout: (f) => f(),
  });
  vm.runInContext(
    script.slice(0, script.indexOf("loadProjects().then(")) +
      `
    renderBoard = () => {}; syncDetail = () => {}; renderDetail = () => {}; writeHash = () => {};
    this.ui = { openIdeaEditor: () => openIdeaEditor(drawerAction('d-edit-idea')), closeIdeaEditor, syncIdeaEditor, poll,
      setJobs(value) { jobs = value; }, target(id) { openId = id; drawerVersion++; },
      session() { return ideaEditor; }, jobs() { return jobs; } };
  `,
    context,
  );
  context.ui.setJobs(fixture().jobs);
  context.ui.target("001-original-idea");
  return { ...context.ui, $: (id) => document.getElementById(id), document };
}

async function offlineUI() {
  const stale = deferred();
  const u = fakeUI(() => stale.promise);
  const opening = u.openIdeaEditor();
  assert.equal(u.$("d-edit-idea").disabled, true);
  u.target("002-other");
  stale.resolve(response(fixture()));
  await opening;
  assert.equal(u.$("edit-idea-dlg").open, false);
  assert.equal(u.$("d-edit-idea").hidden, true);
  for (const [mutate, message] of [
    [
      (b) => {
        b.jobs = [];
      },
      missing,
    ],
    [
      (b) => {
        b.jobs[0].claim = {};
      },
      picked,
    ],
  ]) {
    const board = fixture();
    mutate(board);
    const v = fakeUI(async () => response(board));
    await v.openIdeaEditor();
    assert.equal(v.$("edit-idea-dlg").open, false);
    assert.equal(v.$("d-edit-idea-err").textContent, message);
  }
  const failed = fakeUI(async () => {
    throw new Error("network");
  });
  await failed.openIdeaEditor();
  assert.equal(failed.$("d-edit-idea-err").textContent, "Could not load the idea. Try again.");
  let board = fixture();
  const v = fakeUI(async () => response(board));
  await v.openIdeaEditor();
  assert.equal(v.$("edit-idea-dlg").open, true);
  assert.equal(v.document.activeElement.id, "edit-idea-text");
  v.$("edit-idea-text").value = draft;
  const session = v.session();
  board.jobs[0].project = "new-project";
  await v.poll();
  assert.equal(v.$("edit-idea-save").disabled, false);
  board.jobs[0].claim = false;
  await v.poll();
  assert.equal(v.$("d-edit-idea").hidden, true);
  assert.equal(v.$("edit-idea-save").disabled, true);
  assert.equal(v.$("edit-idea-text").disabled, false);
  assert.equal(v.$("edit-idea-cancel").disabled, false);
  assert.equal(v.$("edit-idea-err").textContent, picked);
  delete board.jobs[0].claim;
  board.jobs[0].idea = "Winner";
  await v.poll();
  assert.equal(v.$("edit-idea-err").textContent, conflict);
  assert.equal(v.$("edit-idea-text").value, draft);
  assert.equal(v.session(), session);
  assert.equal(session.originalText, before);
  board.jobs[0].idea = draft;
  board.jobs[0].title = helpers().ideaTitle(draft);
  await v.poll();
  assert.equal(v.$("edit-idea-save").disabled, false, "same-draft retry is eligible");
  v.$("edit-idea-text").value = "Different";
  v.$("edit-idea-text").listeners.input();
  assert.equal(v.$("edit-idea-save").disabled, true, "input rechecks baseline, never rebases");
  v.closeIdeaEditor();
  assert.equal(v.document.activeElement.id, "d-actions");

  let count = 0;
  const oldPoll = deferred();
  const w = fakeUI(async () =>
    ++count === 1 ? oldPoll.promise : response({ jobs: [{ id: "new" }] }),
  );
  const p = w.poll();
  await w.poll();
  oldPoll.resolve(response(fixture()));
  await p;
  assert.equal(w.jobs()[0].id, "new", "older poll cannot restore stale records");

  const heldPut = deferred();
  let puts = 0;
  const x = fakeUI(async (url, opts = {}) => {
    if (opts.method === "PUT") {
      puts++;
      return heldPut.promise;
    }
    if (url === "./board.json") return response(null, 500);
    return response(fixture());
  });
  await x.openIdeaEditor();
  x.$("edit-idea-text").value = draft;
  x.syncIdeaEditor();
  const submit = () => x.$("edit-idea-form").listeners.submit({ preventDefault() {} });
  const writing = submit();
  await Promise.resolve();
  await Promise.resolve();
  await submit();
  assert.equal(x.$("edit-idea-text").disabled, true);
  assert.equal(x.$("edit-idea-cancel").disabled, true);
  assert.equal(x.$("edit-idea-save").textContent, "Saving…");
  x.closeIdeaEditor();
  assert.equal(x.$("edit-idea-dlg").open, true);
  heldPut.resolve(response(null));
  await writing;
  assert.equal(puts, 1);
  assert.equal(x.$("edit-idea-dlg").open, false);
  assert.equal(
    x.$("d-edit-idea-err").textContent,
    "Changes saved. Refresh to see the latest board.",
  );
  console.log(
    "PASS offline UI: actual activation/poll/submit, stale reads, immutable drafts, input conflicts and single write",
  );
}

async function browserTest(url) {
  assertIdeaBrowserTarget(url);
  const support = await import("../support/browser-server.mjs");
  const puppeteer = require("puppeteer-core");
  function httpFixture(images = []) {
    const board = fixture();
    board.jobs[0].capture_id = "07500000-0000-4000-8000-000000000001";
    if (images.length) board.jobs[0].idea_images = images;
    else delete board.jobs[0].idea_images;
    return board;
  }
  async function seed(board = httpFixture()) {
    const current = await fetch(url + "/api/board");
    assert.ok(current.ok || current.status === 404);
    const saved = await fetch(url + "/api/board", {
      method: "PUT",
      headers: current.ok ? { "If-Match": current.headers.get("ETag") } : { "If-None-Match": "*" },
      body: JSON.stringify(board),
    });
    assert.equal(saved.status, 200);
  }
  const registered = await registerIdeaFixture(url);
  assert.equal(registered.status, 200);
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  try {
    const page = await browser.newPage();
    await support.blockLiveEvents(page);
    await page.evaluateOnNewDocument(() => {
      localStorage.clear();
      const interval = window.setInterval;
      window.setInterval = (fn, ms, ...args) => (ms === 3000 ? 0 : interval(fn, ms, ...args));
    });
    let route = null,
      puts = 0;
    const external = [],
      requestErrors = [],
      pageErrors = [];
    page.on("pageerror", (error) => pageErrors.push(String(error)));
    // Stubbed reads must reach interception rather than the dashboard service worker.
    await page.setBypassServiceWorker(true);
    await page.setRequestInterception(true);
    page.on("request", (request) =>
      (async () => {
        if (!request.url().startsWith(url + "/")) {
          external.push(request.url());
          await request.abort();
          return;
        }
        if (request.url() === url + "/api/board" && request.method() === "PUT") puts++;
        if (route && (await route(request))) return;
        await request.continue();
      })().catch((error) => requestErrors.push(String(error))),
    );
    const read = async () => {
      const r = await fetch(url + "/api/board");
      assert.equal(r.status, 200);
      return r.json();
    };
    const poll = () => page.evaluate(() => poll());
    const value = (selector) => page.$eval(selector, (e) => e.value);
    const error = () => page.$eval("#edit-idea-err", (e) => (e.hidden ? "" : e.textContent));
    const disabled = (selector) => page.$eval(selector, (e) => e.disabled);
    const editorOpen = () => page.$eval("#edit-idea-dlg", (e) => e.open);
    const setDraft = (text) =>
      page.$eval(
        "#edit-idea-text",
        (e, text) => {
          e.value = text;
          e.dispatchEvent(new Event("input", { bubbles: true }));
        },
        text,
      );
    const open = async () => {
      await openJobActions(page);
      await page.click("#d-edit-idea");
      await page.waitForFunction(() => document.getElementById("edit-idea-dlg").open, {
        polling: 50,
      });
    };
    const reset = async (board = httpFixture()) => {
      route = null;
      await seed(board);
      await page.goto(url + "/dashboard.html#001-original-idea/idea", {
        waitUntil: "domcontentloaded",
      });
      await waitForBoard(page);
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      try {
        await page.waitForSelector("#detail:not([hidden]) .tab.active", { polling: 50 });
      } catch (cause) {
        const state = await page
          .evaluate(() => ({
            hash: location.hash,
            openId,
            activeFile,
            loaded,
            jobIds: jobs.map((j) => j.id),
            detailHidden: document.getElementById("detail").hidden,
            tabs: [...document.querySelectorAll("#d-tabs .tab")].map((t) => ({
              name: t.dataset.name,
              active: t.classList.contains("active"),
            })),
            connection: document.getElementById("dot").title,
          }))
          .catch((e) => ({ diagnosticError: String(e) }));
        console.error("reset state: " + JSON.stringify({ state, requestErrors, pageErrors }));
        throw cause;
      }
    };
    const hold = (method, pathname) => {
      const held = deferred();
      route = async (request) => {
        if (request.method() !== method || new URL(request.url()).pathname !== pathname)
          return false;
        route = null;
        held.resolve(request);
        return true;
      };
      return held.promise;
    };
    const waitClosed = () =>
      page.waitForFunction(() => !document.getElementById("edit-idea-dlg").open, { polling: 50 });
    const waitError = (text) =>
      page.waitForFunction(
        (text) => {
          const e = document.getElementById("edit-idea-err");
          return !e.hidden && e.textContent === text;
        },
        { polling: 50 },
        text,
      );
    const assertEdited = (actual, original, text) => {
      const expected = clone(original);
      expected.jobs[0].idea = text;
      expected.jobs[0].title = helpers().ideaTitle(text);
      expected.jobs[0].updated = expected.updated = actual.updated;
      assert.match(actual.updated, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
      assert.deepEqual(
        actual,
        expected,
        "complete board preserves opaque fields and all other jobs",
      );
    };
    for (const [name, width, height, theme] of [
      ["desktop", 1280, 900, "light"],
      ["mobile", 390, 844, "dark"],
    ]) {
      await page.setViewport({ width, height, deviceScaleFactor: 1 });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      await reset();
      await open();
      await setDraft(draft);
      assert.equal(await value("#edit-idea-text"), draft);
      const winner = httpFixture();
      winner.jobs[0].idea = "Concurrent requirements";
      await seed(winner);
      await poll();
      await waitError(conflict);
      assert.equal(await disabled("#edit-idea-save"), true);
      assert.equal(await disabled("#edit-idea-text"), false);
      assert.equal(await disabled("#edit-idea-cancel"), false);
      for (const selector of [
        "#edit-idea-text",
        "#edit-idea-err",
        "#edit-idea-cancel",
        "#edit-idea-save",
      ]) {
        const rect = await page.$eval(selector, (e) => {
          e.scrollIntoView({ block: "nearest" });
          const r = e.getBoundingClientRect();
          return { left: r.left, right: r.right, top: r.top, bottom: r.bottom };
        });
        assert.ok(
          rect.left >= 0 && rect.right <= width && rect.top >= 0 && rect.bottom <= height,
          selector + " reachable",
        );
      }
      assert.equal(await value("#edit-idea-text"), draft);
      await page.click("#edit-idea-cancel");
      await waitClosed();
    }
    console.log("PASS browser editor open/conflict/reachable at desktop and mobile");
    await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    await reset();
    for (const change of [
      (b) => {
        b.jobs[0].claim = null;
      },
      (b) => {
        b.jobs[0].claim = {};
      },
      (b) => {
        b.jobs[0].claim = false;
      },
      (b) => {
        b.jobs[0].claim = { device: "old-agent", seen: "2000-01-01" };
      },
      (b) => {
        b.jobs[0].stage = "spec";
      },
      (b) => {
        b.jobs.shift();
      },
    ]) {
      const b = httpFixture();
      change(b);
      await seed(b);
      await poll();
      assert.equal(
        await page.$eval("#d-edit-idea", (e) => !e.hidden),
        helpers().canEditIdea(b.jobs.find((j) => j.id === "001-original-idea")),
      );
    }
    await reset();
    let held = hold("GET", "/api/board");
    let request;
    await openJobActions(page);
    await page.click("#d-edit-idea");
    request = await held;
    assert.equal(await disabled("#d-edit-idea"), true);
    await page.evaluate(() => {
      closeDetail();
      openJob("001-original-idea");
    });
    await request.respond({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(httpFixture()),
    });
    await page.waitForFunction(() => !document.getElementById("d-edit-idea").disabled, {
      polling: 50,
    });
    assert.equal(await editorOpen(), false, "delayed activation cannot open after close/reopen");
    held = hold("GET", "/api/board");
    await openJobActions(page);
    await page.click("#d-edit-idea");
    request = await held;
    const claimed = httpFixture();
    claimed.jobs[0].claim = {};
    await request.respond({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(claimed),
    });
    await page.waitForFunction(
      (text) => document.getElementById("d-edit-idea-err").textContent === text,
      { polling: 50 },
      picked,
    );
    assert.equal(await editorOpen(), false);
    held = hold("GET", "/api/board");
    await openJobActions(page);
    await page.click("#d-edit-idea");
    request = await held;
    await request.abort();
    await page.waitForFunction(
      () =>
        document.getElementById("d-edit-idea-err").textContent ===
        "Could not load the idea. Try again.",
      { polling: 50 },
    );
    assert.equal(await editorOpen(), false);

    await reset();
    await open();
    assert.equal(await page.$eval("#edit-idea-text", (e) => e === document.activeElement), true);
    const writesBeforeInvalid = puts;
    for (const [text, message] of [
      ["", "Write the idea first."],
      [" \n ", "Write the idea first."],
      ["a".repeat(4001), "Keep the idea to 4000 characters or fewer."],
    ]) {
      await setDraft(text);
      assert.equal(await disabled("#edit-idea-save"), true);
      assert.equal(await error(), message);
      await page.$eval("#edit-idea-form", (e) => e.requestSubmit());
    }
    assert.equal(puts, writesBeforeInvalid);
    assert.equal(
      await page.$eval("#edit-idea-text", (e) => e.required && e.maxLength === 4000),
      true,
    );
    await setDraft("Line");
    await support.focusVisible(page, "#edit-idea-text");
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    assert.equal(await value("#edit-idea-text"), "Line\n");
    await page.evaluate(() => {
      window.backgroundFocus = [];
      document.addEventListener("focusin", (e) => {
        const dlg = document.getElementById("edit-idea-dlg");
        if (dlg.open && e.target !== document.body && !dlg.contains(e.target))
          window.backgroundFocus.push(e.target.id);
      });
      document.getElementById("job-jump").focus();
    });
    const tabStates = [];
    for (const shift of [false, true]) {
      if (shift) await page.keyboard.down("Shift");
      for (let i = 0; i < 6; i++) {
        await page.keyboard.down("Tab");
        tabStates.push(
          await page.evaluate(() => ({
            id: document.activeElement.id,
            focused: document.hasFocus(),
          })),
        );
        await page.keyboard.up("Tab");
      }
      if (shift) await page.keyboard.up("Shift");
    }
    assert.ok(tabStates.some((s) => s.id === "edit-idea-cancel"));
    assert.ok(
      tabStates.every((s) =>
        ["", "edit-idea-text", "edit-idea-save", "edit-idea-cancel"].includes(s.id),
      ),
      "native Tab may visit browser chrome, never background controls",
    );
    assert.deepEqual(await page.evaluate(() => window.backgroundFocus), []);
    for (const modifier of ["Meta", "Control"]) {
      await page.click("#edit-idea-text");
      await page.keyboard.down(modifier);
      await page.keyboard.press("k");
      await page.keyboard.up(modifier);
      assert.equal(await page.$eval("#job-picker", (e) => e.open), false);
      assert.equal(await editorOpen(), true);
    }
    await page.click("#edit-idea-text");
    await page.keyboard.press("Escape");
    await waitClosed();
    assert.equal(await page.evaluate(() => document.activeElement.id), "d-actions");
    assert.equal(await page.$eval("#detail", (e) => e.hidden), false);
    await open();
    await setDraft(draft);
    await page.$eval("#edit-idea-text", (e) => {
      e.setSelectionRange(2, 10);
    });
    const selected = await page.$eval("#edit-idea-text", (e) => [e.selectionStart, e.selectionEnd]);
    const unrelated = httpFixture();
    unrelated.jobs[1].note = "Unrelated poll";
    await seed(unrelated);
    await poll();
    assert.deepEqual(
      await page.$eval("#edit-idea-text", (e) => [e.selectionStart, e.selectionEnd]),
      selected,
    );
    assert.equal(await value("#edit-idea-text"), draft);
    assert.equal(await page.evaluate(() => document.activeElement.id), "edit-idea-text");
    held = hold("GET", "/board.json");
    const failedPoll = poll();
    request = await held;
    await request.respond({ status: 500, body: "offline" });
    assert.equal(await failedPoll, false);
    assert.equal(await value("#edit-idea-text"), draft);
    assert.deepEqual(
      await page.$eval("#edit-idea-text", (e) => [e.selectionStart, e.selectionEnd]),
      selected,
    );
    assert.equal(await disabled("#edit-idea-save"), false);

    held = hold("PUT", "/api/board");
    const startWrites = puts;
    await page.click("#edit-idea-save");
    request = await held;
    await page.$eval("#edit-idea-form", (e) => {
      e.dispatchEvent(new Event("submit", { cancelable: true }));
      e.requestSubmit();
    });
    assert.equal(await disabled("#edit-idea-text"), true);
    assert.equal(await disabled("#edit-idea-cancel"), true);
    assert.equal(await page.$eval("#edit-idea-save", (e) => e.textContent), "Saving…");
    await page.keyboard.press("Escape");
    assert.equal(await editorOpen(), true);
    await page.click("#edit-idea-cancel");
    assert.equal(await editorOpen(), true);
    await request.continue();
    await waitClosed();
    assert.equal(puts - startWrites, 1);
    assertEdited(await read(), unrelated, draft);

    for (const [mutate, message] of [
      [
        (b) => {
          b.jobs[0].claim = {};
        },
        picked,
      ],
      [
        (b) => {
          b.jobs[0].idea = "Winner requirements";
        },
        conflict,
      ],
      [
        (b) => {
          b.jobs.shift();
        },
        missing,
      ],
    ]) {
      await reset();
      await open();
      await setDraft(draft);
      const start = puts;
      held = hold("PUT", "/api/board");
      await page.click("#edit-idea-save");
      request = await held;
      const winner = httpFixture();
      mutate(winner);
      await seed(winner);
      await request.continue();
      await waitError(message);
      await poll();
      assert.equal(puts - start, 1, "412 refusal never PUTs again");
      assert.deepEqual(await read(), winner);
      assert.equal(await value("#edit-idea-text"), draft);
      assert.equal(await disabled("#edit-idea-text"), false);
      assert.equal(await disabled("#edit-idea-save"), true);
      await page.click("#edit-idea-cancel");
      await waitClosed();
      assert.equal(
        await page.evaluate(() => document.activeElement.id),
        message === missing ? "d-close" : "d-actions",
      );
    }
    await reset();
    await open();
    await setDraft(draft);
    held = hold("PUT", "/api/board");
    const assignmentStart = puts;
    await page.click("#edit-idea-save");
    request = await held;
    const assigned = httpFixture();
    assigned.jobs[0].project = "concurrent-project";
    assigned.jobs[0].unknown.extra = ["untouched"];
    assigned.jobs[1].note = "Keep me";
    await seed(assigned);
    await request.continue();
    await waitClosed();
    assert.equal(puts - assignmentStart, 2);
    assertEdited(await read(), assigned, draft);

    await reset();
    await open();
    await setDraft(draft);
    held = hold("PUT", "/api/board");
    await page.click("#edit-idea-save");
    request = await held;
    const accepted = await fetch(url + "/api/board", {
      method: "PUT",
      headers: { "If-Match": request.headers()["if-match"] },
      body: request.postData(),
    });
    assert.equal(accepted.status, 200);
    await request.abort();
    await page.waitForFunction(() => !document.getElementById("edit-idea-err").hidden, {
      polling: 50,
    });
    const committed = await read();
    const retryWrites = puts;
    assert.equal(await value("#edit-idea-text"), draft);
    await poll();
    assert.equal(await disabled("#edit-idea-save"), false);
    await page.click("#edit-idea-save");
    await waitClosed();
    assert.equal(puts, retryWrites);
    assert.deepEqual(
      await read(),
      committed,
      "response-loss retry does not write or change timestamps",
    );

    await reset();
    await open();
    await setDraft(draft);
    held = hold("PUT", "/api/board");
    await page.click("#edit-idea-save");
    request = await held;
    await request.respond({ status: 500, body: "<img src=x onerror=alert(1)>" });
    await waitError("saving the board failed: 500 <img src=x onerror=alert(1)>");
    assert.equal(await page.$eval("#edit-idea-err", (e) => e.children.length), 0);
    assert.equal(await value("#edit-idea-text"), draft);
    assert.equal(await disabled("#edit-idea-text"), false);
    assert.deepEqual(await read(), httpFixture());
    await page.click("#edit-idea-cancel");

    await reset();
    await open();
    await setDraft(draft);
    route = async (request) => {
      if (new URL(request.url()).pathname !== "/board.json") return false;
      await request.respond({ status: 500, body: "refresh failed" });
      return true;
    };
    const refreshWrites = puts;
    await page.click("#edit-idea-save");
    await waitClosed();
    await page.waitForFunction(() => !document.getElementById("d-edit-idea-err").hidden, {
      polling: 50,
    });
    assert.equal(
      await page.$eval("#d-edit-idea-err", (e) => e.textContent),
      "Changes saved. Refresh to see the latest board.",
    );
    assert.equal(puts - refreshWrites, 1);
    assertEdited(await read(), httpFixture(), draft);

    await reset();
    await open();
    await setDraft(draft);
    held = hold("PUT", "/api/board");
    await page.click("#edit-idea-save");
    request = await held;
    await page.evaluate(() => {
      location.hash = "#002-other/idea";
    });
    await page.waitForFunction(() => openId === "002-other", { polling: 50 });
    await request.continue();
    await waitClosed();
    assert.equal(await page.evaluate(() => openId), "002-other");
    assert.equal(await page.$eval("#detail", (e) => e.hidden), false);
    await page.evaluate(() => closeDetail());
    await page.evaluate(() => openJob("001-original-idea"));
    await poll();
    await open();
    assert.equal(await value("#edit-idea-text"), draft, "explicit reopen obtains saved baseline");
    await page.evaluate(() => closeDetail());
    await page.click("#edit-idea-cancel");
    assert.equal(await page.evaluate(() => document.activeElement.id), "job-jump");

    const projectless = httpFixture();
    delete projectless.jobs[0].project;
    await seed(projectless);
    route = async (request) => {
      if (new URL(request.url()).pathname !== "/projects.json") return false;
      await request.respond({ status: 200, contentType: "application/json", body: "[]" });
      return true;
    };
    await page.reload({ waitUntil: "domcontentloaded" });
    await waitForBoard(page);
    route = null;
    await page.evaluate(() => {
      location.hash = "#001-original-idea/idea";
    });
    await open();
    await setDraft(draft);
    await page.click("#edit-idea-save");
    await waitClosed();
    assertEdited(await read(), projectless, draft);
    await reset(projectless);
    await page.select("#d-project", "sample");
    await page.click("#d-assign");
    await page.waitForFunction(() => jobs[0].project === "sample", { polling: 50 });
    assert.equal((await read()).jobs[0].project, "sample");
    await page.evaluate(() => closeDetail());
    await page.click("#new-idea");
    await page.waitForFunction(() => document.getElementById("idea-dlg").open, { polling: 50 });
    await page.type("#idea-text", "\nCreated idea 😀\nBody");
    held = hold("GET", "/board.json");
    await page.click("#idea-add");
    request = await held;
    await page.waitForFunction(() => !document.getElementById("idea-dlg").open, { polling: 50 });
    const created = (await read()).jobs.at(-1);
    assert.equal(created.title, "Created idea 😀");
    assert.equal(created.idea, "\nCreated idea 😀\nBody");
    assert.notEqual(
      await page.evaluate(() => parseHash().id),
      created.id,
      "dialog closes before post-save navigation",
    );
    await request.continue();
    await page.waitForFunction(
      (id) => parseHash().id === id && openId === id && !document.getElementById("detail").hidden,
      { polling: 50 },
      created.id,
    );

    await reset();
    await page.evaluate(() => closeDetail());
    await support.focusVisible(page, "#job-jump");
    await page.keyboard.down("Meta");
    await page.keyboard.press("k");
    await page.keyboard.up("Meta");
    await page.waitForFunction(() => document.getElementById("job-picker").open, { polling: 50 });
    await page.type("#job-search", "new idea");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.getElementById("idea-dlg").open, { polling: 50 });
    assert.equal(await editorOpen(), false);
    await page.type("#idea-text", before);
    await page.evaluate(async () => {
      const canvas = document.createElement("canvas");
      canvas.width = 32;
      canvas.height = 32;
      canvas.getContext("2d").fillStyle = "#9b2d86";
      canvas.getContext("2d").fillRect(0, 0, 32, 32);
      const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
      const data = new DataTransfer();
      data.items.add(new File([blob], "9b2d86.png", { type: "image/png" }));
      document
        .getElementById("idea-form")
        .dispatchEvent(
          new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }),
        );
      await decoding;
    });
    await page.click("#idea-image-list li:nth-child(1) button");
    assert.deepEqual(await page.evaluate(() => [draft.images.length, document.activeElement.id]), [
      0,
      "idea-text",
    ]);
    const names = await page.evaluate(async () => {
      const data = new DataTransfer();
      for (const color of ["#2176ae", "#f0a202"]) {
        const canvas = document.createElement("canvas");
        canvas.width = 128;
        canvas.height = 60;
        canvas.getContext("2d").fillStyle = color;
        canvas.getContext("2d").fillRect(0, 0, 128, 60);
        const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
        data.items.add(new File([blob], color.slice(1) + ".png", { type: "image/png" }));
      }
      document
        .getElementById("idea-form")
        .dispatchEvent(
          new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }),
        );
      await decoding;
      return draft.images.map((image) => image.name);
    });
    assert.equal(names.length, 2);
    assert.equal(new Set(names).size, 2);
    names.forEach((name) => assert.match(name, /^[0-9a-f]{64}\.png$/));
    const chosen = await page.evaluate(async () => {
      const canvas = document.createElement("canvas");
      canvas.width = 64;
      canvas.height = 64;
      canvas.getContext("2d").fillStyle = "#3c9d4e";
      canvas.getContext("2d").fillRect(0, 0, 64, 64);
      const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/png"));
      const data = new DataTransfer();
      data.items.add(new File([blob], "3c9d4e.png", { type: "image/png" }));
      const input = document.getElementById("idea-file");
      input.files = data.files;
      input.dispatchEvent(new Event("change", { bubbles: true }));
      await decoding;
      return [draft.images.length, input.value];
    });
    assert.deepEqual(chosen, [3, ""]);
    assert.equal(
      await page.$eval("#idea-upload", (e) => e.getAttribute("aria-describedby")),
      "idea-images-help",
    );
    assert.equal(
      await page.$eval("#idea-images-help", (e) => e.textContent),
      "Paste or add up to 4 images (PNG, JPEG or WebP, 5 MiB each).",
    );
    assert.equal(
      await page.$eval("#idea-text", (e) => e.getAttribute("aria-describedby")),
      "idea-ref idea-key-help idea-images-help",
    );
    await page.click("#idea-image-list li:nth-child(3) button");
    assert.equal(await page.evaluate(() => draft.images.length), 2);
    assert.match(
      await page.evaluate(() => document.activeElement.getAttribute("aria-label")),
      /^Remove: Image 2: /,
    );
    held = hold("PUT", "/api/board");
    await page.keyboard.down("Meta");
    await page.keyboard.press("Enter");
    await page.keyboard.up("Meta");
    request = await held;
    const frozen = JSON.parse(request.postData()).jobs.at(-1);
    assert.equal(await page.$eval("#idea-add", (e) => e.textContent), "Saving…");
    assert.equal(await disabled("#idea-add"), true);
    // Cancel and Escape do nothing while the save runs: closing would drop the snapshot.
    assert.equal(await disabled("#idea-cancel"), true);
    await page.click("#idea-cancel");
    // A click on the disabled Cancel focuses no control, so give Escape a known target.
    await support.focusVisible(page, "#idea-text");
    await page.keyboard.press("Escape");
    assert.equal(await page.$eval("#idea-dlg", (e) => e.open), true);
    await page.setViewport({ width: 390, height: 844, deviceScaleFactor: 1 });
    const row = await page.$eval("#idea-dlg .actions", (r) => {
      const [cancel, add] = [...r.children].map((b) => b.getBoundingClientRect());
      return { sameLine: cancel.top === add.top, fits: r.scrollWidth <= r.clientWidth };
    });
    assert.deepEqual(row, { sameLine: true, fits: true });
    if (captureDir) {
      fs.mkdirSync(captureDir, { recursive: true });
      await (
        await page.$("#idea-dlg")
      ).screenshot({
        path: path.join(captureDir, "idea-saving-390.png"),
      });
    }
    await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
    assert.deepEqual(frozen.idea_images, names);
    assert.equal(frozen.idea, before);
    await request.respond({ status: 503, body: "Storage is busy." });
    await page.waitForFunction(() => !inflight && snap?.uncertain === true, { polling: 50 });
    if (captureDir) {
      await (
        await page.$("#idea-dlg")
      ).screenshot({
        path: path.join(captureDir, "idea-uncertain-1280.png"),
      });
    }
    assert.equal(
      await page.$eval("#idea-err", (e) => e.textContent),
      "Not confirmed: saving the board failed: 503 Storage is busy. The board has no record of it yet, but the save could still land late. Retry re-checks with the same content and will not add a duplicate.",
    );
    assert.equal(await page.$eval("#idea-add", (e) => e.textContent), "Retry");
    assert.equal(await disabled("#idea-add"), false);
    assert.equal(await page.$eval("#idea-text", (e) => e.readOnly), true);
    assert.equal(await disabled("#idea-upload"), true);
    assert.equal(await disabled("#idea-cancel"), true);
    assert.equal((await read()).jobs.length, 2);
    held = hold("GET", "/api/board");
    await page.click("#idea-add");
    request = await held;
    await request.respond({ status: 503, body: "synthetic retry read failure" });
    await page.waitForFunction(
      () =>
        !inflight &&
        document.getElementById("idea-err").textContent === "reading the board failed: 503",
      { polling: 50 },
    );
    assert.equal(await page.evaluate(() => snap.capture_id), frozen.capture_id);
    assert.equal(await page.evaluate(() => snap.uncertain), true);
    await page.keyboard.press("Escape");
    assert.equal(await page.$eval("#idea-dlg", (e) => e.open), true);
    const captureWrites = puts;
    await page.click("#idea-add");
    await page.waitForFunction(
      () =>
        !document.getElementById("idea-dlg").open &&
        snap === null &&
        !inflight &&
        draft.images.length === 0,
      { polling: 50 },
    );
    assert.equal(puts - captureWrites, 1);
    const captured = await read();
    assert.equal(captured.jobs.length, 3);
    assert.deepEqual(captured.jobs.at(-1), { ...frozen, updated: captured.updated });
    for (const name of names) {
      const image = await fetch(url + "/api/idea-images/" + name);
      assert.equal(image.status, 200);
      assert.equal(image.headers.get("Content-Type"), "image/png");
      assert.equal(image.headers.get("Content-Security-Policy"), "default-src 'none'; sandbox");
      assert.equal(image.headers.get("X-Content-Type-Options"), "nosniff");
      assert.equal((await fetch(url + "/idea-images/" + name)).status, 404);
    }
    const imageBoard = httpFixture(names);
    imageBoard.jobs[0].capture_id = frozen.capture_id;
    const gallery = async () => {
      await page.waitForFunction(
        () => {
          const images = [...document.querySelectorAll("#d-gallery img")];
          return (
            images.length === 2 &&
            images.every((image) => image.complete && image.naturalWidth === 128 && !image.hidden)
          );
        },
        { polling: 50 },
      );
      return page.$$eval("#d-gallery img", (images) =>
        images.map((image) => ({
          src: image.getAttribute("src"),
          alt: image.alt,
          width: image.naturalWidth,
          height: image.naturalHeight,
        })),
      );
    };
    for (const [name, width, height, theme] of [
      ["desktop", 1280, 900, "light"],
      ["mobile", 390, 844, "dark"],
    ]) {
      await page.setViewport({ width, height, deviceScaleFactor: 1 });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      await reset(imageBoard);
      const originalGallery = await gallery();
      assert.deepEqual(
        originalGallery.map((image) => image.src),
        names.map((name) => "./api/idea-images/" + name),
      );
      await open();
      assert.equal(
        await page.$eval(
          "#edit-idea-form",
          (e) => e.querySelectorAll("select,input[type=file],img").length,
        ),
        0,
      );
      assert.equal(await page.$eval("#idea-dlg", (e) => e.open), false);
      await setDraft(draft);
      await page.click("#edit-idea-save");
      await waitClosed();
      await page.waitForFunction(
        (text) =>
          jobs[0].idea === text &&
          document.getElementById("d-title").textContent === "Revised idea 😀",
        { polling: 50 },
        draft,
      );
      assertEdited(await read(), imageBoard, draft);
      assert.deepEqual(await gallery(), originalGallery);
    }
    await reset(imageBoard);
    await open();
    await setDraft(draft);
    held = hold("PUT", "/api/board");
    const imageRaceWrites = puts;
    await page.click("#edit-idea-save");
    request = await held;
    const imageWinner = clone(imageBoard);
    imageWinner.jobs[0].claim = { device: "synthetic-agent" };
    await seed(imageWinner);
    await request.continue();
    await waitError(picked);
    assert.equal(puts - imageRaceWrites, 1);
    assert.deepEqual(await read(), imageWinner);
    assert.deepEqual(
      await gallery(),
      names.map((name, i) => ({
        src: "./api/idea-images/" + name,
        alt: "Pasted image " + (i + 1),
        width: 128,
        height: 60,
      })),
    );
    assert.equal(await value("#edit-idea-text"), draft);
    await page.click("#edit-idea-cancel");
    await waitClosed();
    console.log(
      "PASS integration: Cmd+K/Meta+Enter synthetic paste, sticky uncertain capture retry, private PNG routes, ordered gallery/edit preservation at two viewports and image-bearing pickup CAS",
    );

    await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    await reset();
    await open();
    const hostile =
      '  <img src="https://invalid.example/x" onerror="window.__ideaExecuted=1">\n<script>window.__ideaExecuted=1</script>\n[bad](javascript:alert(1)) ![x](https://invalid.example/image.png)\n[web](https://invalid.example/link)  \n';
    await setDraft(hostile);
    await page.click("#edit-idea-save");
    await waitClosed();
    assertEdited(await read(), httpFixture(), hostile);
    await page.waitForFunction(
      () => document.getElementById("d-title").textContent.startsWith("<img"),
      { polling: 50 },
    );
    assert.equal(await page.$eval("#d-title", (e) => e.children.length), 0);
    assert.equal(
      await page.$eval('.card[data-id="001-original-idea"] .title', (e) => e.children.length),
      0,
    );
    assert.equal(
      await page.$eval(
        "#d-doc",
        (e) => e.querySelectorAll('img,script,[onerror],a[href^="javascript:"]').length,
      ),
      0,
    );
    assert.equal(await page.evaluate(() => window.__ideaExecuted), undefined);
    assert.deepEqual(external, []);
    assert.deepEqual(requestErrors, []);
    assert.deepEqual(pageErrors, []);
    console.log(
      "PASS browser: native keyboard/inertness, freshness, held writes, real Worker 412s, response loss, metadata, zero-project/assignment/create, safe HTML at desktop/mobile",
    );
    console.log("Native Tab states: " + JSON.stringify(tabStates));
  } finally {
    await browser.close();
  }
}

(async () => {
  await offlineRegistration();
  await offline();
  await offlineUI();
  const i = process.argv.indexOf("--browser");
  if (i >= 0) await browserTest(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
