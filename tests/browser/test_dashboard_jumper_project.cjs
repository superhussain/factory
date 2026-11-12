// Jumper project mode: command visibility, header-filter parity and persisted choices,
// project/slug search, all/no-project rows, Back/Escape query restore, modal guard,
// refreshed labels/stale options, history/page/link ownership.
// Requires --browser <owned-loopback-origin>.
const assert = require("node:assert/strict");
const { waitForBoard, clickNow } = require("../support/board-ready.cjs");
const { filterState, showOnly } = require("../support/project-filter.cjs");

const command = "command:select-project";
const fixtureJobs = [
  { id: "090-alpha", project: "alpha", title: "Synthetic Alpha job", stage: "build", updated: "" },
  { id: "091-beta", project: "beta", title: "Synthetic Beta job", stage: "merged", updated: "" },
  {
    id: "092-loose",
    title: "Synthetic unassigned job",
    stage: "idea",
    idea: "Synthetic unassigned job",
    updated: "",
  },
  {
    id: "093-context",
    project: "alpha",
    title: "Synthetic context job",
    stage: "pr-open",
    updated: "",
    pr: "https://github.com/example/sample/pull/42",
    paseo: { server: "synthetic-server", agent: "synthetic-agent" },
  },
];
const projectText = (title) =>
  `---\nrepo: example/sample\nbase: main\ntitle: ${title}\n---\n# Synthetic project\n`;
const writes = [],
  external = [],
  successes = [],
  errors = [];

// Explicit current row sets per query, with and without the 093-context drawer open.
function actionRowsFor(text) {
  const tokens = text.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return [
    {
      key: "page:093-context:spec.md",
      title: "spec",
      meta: "Page of 093-context · spec.md",
      kind: "page",
      words: "spec spec md page",
    },
    {
      key: "page:093-context:build.md",
      title: "build",
      meta: "Page of 093-context · build.md",
      kind: "page",
      words: "build build md page",
    },
    {
      key: "link:093-context:pr",
      title: "Open PR #42",
      meta: "Link of 093-context · opens in a new tab",
      kind: "link",
      words: "open pr #42 link pr github",
    },
    {
      key: "link:093-context:paseo",
      title: "Open in Session app",
      meta: "Link of 093-context",
      kind: "link",
      words: "open in session app link session app",
    },
    {
      key: "link:093-context:paseo-web",
      title: "Open in Session",
      meta: "Link of 093-context · opens in a new tab",
      kind: "link",
      words: "open in session link session",
    },
  ]
    .filter((r) => tokens.every((t) => r.words.includes(t)))
    .map(({ words, ...r }) => r);
}
function commandRowsFor(text) {
  const tokens = text.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return [
    {
      key: "command:new-idea",
      title: "New idea",
      meta: "Command · Capture an idea",
      kind: "command",
      words: "new idea add command",
    },
    {
      key: command,
      title: "Select project",
      meta: "Command · Change the board project filter",
      kind: "command",
      words: "select project command",
    },
  ]
    .filter((r) => tokens.every((t) => r.words.includes(t)))
    .map(({ words, ...r }) => r);
}
const PROJECT_NAMES = { alpha: "Synthetic Alpha", beta: "Synthetic Beta" };
function jobRowsFor(text) {
  const q = text.trim().toLowerCase();
  const matched = fixtureJobs
    .filter((j) =>
      [
        j.id,
        j.title,
        j.project,
        PROJECT_NAMES[j.project] ?? (j.project || "No project yet"),
        j.jira,
        j.pr,
        j.idea,
      ].some((f) =>
        String(f ?? "")
          .toLowerCase()
          .includes(q),
      ),
    )
    .sort((a, b) => (/^(\d+)/.exec(b.id)?.[1] ?? 0) - (/^(\d+)/.exec(a.id)?.[1] ?? 0));
  const section = (j) => (j.stage === "idea" ? "idea" : j.stage);
  return ["idea", "spec", "build", "review", "approve", "pr-open", "merged", "closed"].flatMap(
    (stage) =>
      matched
        .filter((j) => section(j) === stage)
        .map((j) => ({
          key: "job:" + j.id,
          title: j.title,
          kind: "job",
          meta:
            (PROJECT_NAMES[j.project] ?? (j.project || "No project yet")) +
            " · " +
            j.id +
            " · " +
            j.stage,
        })),
  );
}
function expectedRows(text, withDrawer) {
  return [...(withDrawer ? actionRowsFor(text) : []), ...commandRowsFor(text), ...jobRowsFor(text)];
}

async function conditionalPut(origin, route, body, type) {
  const old = await fetch(origin + route, { redirect: "error" });
  assert.ok([200, 404].includes(old.status), "GET before PUT " + route);
  const headers = { "Content-Type": type };
  if (old.status === 404) headers["If-None-Match"] = "*";
  else {
    headers["If-Match"] = old.headers.get("etag");
    assert.ok(headers["If-Match"], "observed original ETag");
  }
  const response = await fetch(origin + route, { method: "PUT", body, headers, redirect: "error" });
  assert.equal(response.status, 200, "conditional PUT " + route);
}
async function seed(origin) {
  await conditionalPut(
    origin,
    "/api/board",
    JSON.stringify({ updated: "", jobs: fixtureJobs }),
    "application/json",
  );
  for (const slug of ["alpha", "beta", "empty"])
    await conditionalPut(
      origin,
      "/api/projects/" + slug + ".md",
      projectText("Synthetic " + slug[0].toUpperCase() + slug.slice(1)),
      "text/markdown; charset=utf-8",
    );
  for (const name of ["spec.md", "build.md"])
    await conditionalPut(
      origin,
      "/api/jobs/alpha/093-context/" + name,
      "# Synthetic " + name + "\n",
      "text/markdown",
    );
  console.log("PASS conditional synthetic seed");
}
async function shortcut(page, modifier = "Meta") {
  await page.bringToFront();
  await page.keyboard.down(modifier);
  await page.keyboard.press("k");
  await page.keyboard.up(modifier);
}
async function openPicker(page, modifier = "Meta") {
  await shortcut(page, modifier);
  await page.waitForSelector("#job-picker[open]", { polling: 50 });
  assert.equal(await page.$eval("#job-search", (el) => el.value), "");
}
async function setQuery(page, text) {
  await page.bringToFront();
  await page.click("#job-search");
  const length = await page.$eval("#job-search", (el) => el.value.length);
  for (let i = 0; i <= length; i++) await page.keyboard.press("ArrowRight");
  for (let i = 0; i < length; i++) await page.keyboard.press("Backspace");
  assert.equal(await page.$eval("#job-search", (el) => el.value), "", "query exactly cleared");
  if (text) await page.keyboard.type(text);
  assert.equal(
    await page.$eval("#job-search", (el) => el.value),
    text,
    "exact query before assertions",
  );
}
async function state(page) {
  return page.evaluate(() => ({
    rows: jobResults.map((r, i) => ({
      key: r.key,
      kind: r.kind,
      title: r.title,
      meta: r.meta,
      id: document.querySelectorAll('#job-results [role="option"]')[i].id,
      selected: document
        .querySelectorAll('#job-results [role="option"]')
        [i].getAttribute("aria-selected"),
    })),
    groups: [...document.querySelectorAll('#job-results [role="group"]')].map(
      (g) => g.firstElementChild.textContent,
    ),
    count: document.getElementById("job-count").textContent,
    empty: !document.getElementById("job-empty").hidden,
    emptyText: document.getElementById("job-empty").textContent,
    query: document.getElementById("job-search").value,
    focus: document.activeElement.id,
    descendant: document.getElementById("job-search").getAttribute("aria-activedescendant"),
    heading: document.getElementById("job-picker-heading").textContent,
    label: document.querySelector('label[for="job-search"]').textContent,
    listLabel: document.getElementById("job-results").getAttribute("aria-label"),
    back: !document.getElementById("job-picker-back")?.hidden,
  }));
}
function selected(s) {
  const rows = s.rows.filter((r) => r.selected === "true");
  assert.equal(rows.length, s.rows.length ? 1 : 0);
  assert.equal(s.descendant, rows[0]?.id ?? null);
  return rows[0];
}
async function choose(page, key) {
  const s = await state(page),
    from = s.rows.findIndex((r) => r.selected === "true"),
    to = s.rows.findIndex((r) => r.key === key);
  assert.ok(to >= 0, key);
  for (let i = 0; i < Math.abs(to - from); i++)
    await page.keyboard.press(to > from ? "ArrowDown" : "ArrowUp");
  assert.equal(selected(await state(page)).key, key);
}
async function enter(page) {
  const row = selected(await state(page));
  console.log(JSON.stringify({ beforeEnter: row ?? null }));
  await page.keyboard.press("Enter");
  return row;
}
async function clickKey(page, key) {
  const s = await state(page),
    row = s.rows.find((r) => r.key === key);
  assert.ok(row, key);
  await page.click("#" + row.id);
}
async function options(page) {
  return (await filterState(page)).rows.map((r) => ({ value: r.key, title: r.title }));
}
// The only shown key, '' when every key shows, else null.
async function current(page) {
  const rows = (await filterState(page)).rows.slice(1),
    shown = rows.filter((r) => r.checked);
  return shown.length === rows.length ? "" : shown.length === 1 ? shown[0].key : null;
}
async function headerFilter(page, value) {
  await showOnly(page, value);
  assert.equal(await current(page), value);
}
async function snapshot(page) {
  return page.evaluate(() => ({
    label: document.getElementById("filter-label").textContent,
    ids: [...document.querySelectorAll("#board .card")]
      .filter((c) => c.getClientRects().length)
      .map((c) => c.dataset.id),
    storage: localStorage.getItem("factory-projects-hidden"),
    hash: location.hash,
    drawer: document.getElementById("detail").hidden,
    owner: openId,
    tab: activeFile,
    jobs: jobs.map((j) => j.id),
  }));
}
async function drawer(page) {
  await page.bringToFront();
  await clickNow(page, '.card[data-id="093-context"]');
  await page.waitForSelector('#d-tabs .tab[data-name="spec.md"]', { polling: 50 });
  await page.waitForFunction(
    () => document.getElementById("d-doc").textContent.includes("Synthetic"),
    { polling: 50 },
  );
}
async function projectMode(page, click = false) {
  await choose(page, command);
  if (click) await clickKey(page, command);
  else await enter(page);
  await page.waitForFunction(
    () => document.getElementById("job-picker-heading").textContent === "Select project",
    { polling: 50 },
  );
}

async function browserRun(origin) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(origin);
  const puppeteer = require("puppeteer-core");
  await seed(origin);
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  async function makePage({ mobile = false, stored, variant } = {}) {
    const context = await browser.createBrowserContext(),
      page = await context.newPage();
    await support.controlBoardPolling(page);
    await page.setViewport({
      width: mobile ? 390 : 1280,
      height: mobile ? 844 : 800,
      deviceScaleFactor: 1,
    });
    await page.emulateMediaFeatures([
      { name: "prefers-color-scheme", value: mobile ? "dark" : "light" },
    ]);
    // The legacy single filter, seeded once per tab: it migrates on load and is then removed.
    if (stored !== undefined)
      await page.evaluateOnNewDocument((value) => {
        if (sessionStorage.getItem("seeded")) return;
        sessionStorage.setItem("seeded", "1");
        localStorage.setItem("factory-project", value);
      }, stored);
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("response", (r) => {
      if (new URL(r.url()).origin !== origin && r.ok()) successes.push(r.url());
    });
    // Stubbed reads must reach interception rather than the dashboard service worker.
    await page.setBypassServiceWorker(true);
    await page.setRequestInterception(true);
    page.on("request", (r) => {
      const url = new URL(r.url());
      if (!["GET", "HEAD"].includes(r.method())) {
        writes.push(r.method() + " " + r.url());
        return r.abort();
      }
      if (url.origin !== origin) {
        external.push(r.url());
        return r.abort();
      }
      if (variant?.response && url.pathname === "/projects.json")
        return r.respond(variant.response);
      return r.continue();
    });
    await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
    await waitForBoard(page);
    await page.waitForSelector(".card", { polling: 50 });
    await page.waitForSelector("#filter:not([hidden])", { polling: 50 });
    return page;
  }
  async function commands() {
    const page = await makePage();
    for (const context of [false, true]) {
      if (context) await drawer(page);
      await openPicker(page);
      for (const text of [
        "",
        "project",
        "SELECT PROJECT",
        "select proj",
        "new id",
        "command",
        "zz-no-match",
      ]) {
        await setQuery(page, text);
        const s = await state(page);
        const commandKeys =
          text === "" || text === "command"
            ? ["command:new-idea", command]
            : text === "new id"
              ? ["command:new-idea"]
              : text === "zz-no-match"
                ? []
                : [command];
        assert.deepEqual(
          s.rows.filter((r) => r.kind === "command").map((r) => r.key),
          commandKeys,
        );
        const jobKeys =
          text === ""
            ? ["job:092-loose", "job:090-alpha", "job:093-context", "job:091-beta"]
            : text === "project"
              ? ["job:092-loose"]
              : [];
        assert.deepEqual(
          s.rows.filter((r) => r.kind === "job").map((r) => r.key),
          jobKeys,
        );
        assert.deepEqual(
          s.rows.map(({ key, title, meta, kind }) => ({ key, title, meta, kind })),
          expectedRows(text, context),
          "rows " + JSON.stringify(text),
        );
        const actions = s.rows.filter((r) => ["page", "link"].includes(r.kind));
        const groups = [
          ...(actions.some((r) => r.kind === "page") ? ["Pages"] : []),
          ...(actions.some((r) => r.kind === "link") ? ["Links"] : []),
          ...commandKeys.map((k) => (k === command ? "Select project" : "New idea")),
          ...(text === ""
            ? ["Idea", "Build", "PR open", "Merged"]
            : text === "project"
              ? ["Idea"]
              : []),
        ];
        assert.deepEqual(s.groups, groups);
        assert.equal(
          s.count,
          `${jobKeys.length} ${jobKeys.length === 1 ? "job" : "jobs"}${actions.length ? " · " + actions.length + " for 093-context" : ""}${commandKeys.length ? " · " + commandKeys.length + (commandKeys.length === 1 ? " command" : " commands") : ""}`,
        );
        assert.equal(s.empty, !s.rows.length);
        if (text === "")
          assert.equal(selected(s).key, context ? actions[0].key : "command:new-idea");
        if (!s.rows.length) {
          await enter(page);
          assert.equal(await page.$eval("#job-picker", (el) => el.open), true);
        }
      }
      await page.keyboard.press("Escape");
    }
    for (const availability of ["hidden", "disabled"])
      for (const phase of ["render", "activation"]) {
        await openPicker(page);
        await setQuery(page, "project");
        await page.$eval(
          "#filter",
          (el, property) => {
            el[property] = true;
          },
          availability,
        );
        if (phase === "render") await setQuery(page, "project");
        else await enter(page);
        assert.deepEqual(
          (await state(page)).rows.map((r) => r.key),
          ["job:092-loose"],
        );
        assert.equal((await state(page)).heading, "Jump to job");
        assert.equal(await page.$eval("#job-picker", (el) => el.open), true);
        await page.$eval(
          "#filter",
          (el, property) => {
            el[property] = false;
          },
          availability,
        );
        await page.keyboard.press("Escape");
      }
    await page.$eval("#new-idea", (el) => {
      el.hidden = true;
    });
    await openPicker(page);
    await setQuery(page, "project");
    assert.equal(selected(await state(page)).key, command);
    await page.$eval("#new-idea", (el) => {
      el.hidden = false;
    });
    await page.keyboard.press("Escape");
    await page.browserContext().close();
    console.log("PASS command");
  }
  async function projectsCase() {
    const page = await makePage();
    await headerFilter(page, "alpha");
    await drawer(page);
    const os = await options(page),
      response = await (await fetch(origin + "/projects.json")).json();
    assert.deepEqual(os, [
      { value: "", title: "all projects" },
      ...response.map((p) => ({ value: p.slug, title: p.title || p.slug })),
      { value: ":unassigned", title: "No project yet" },
    ]);
    await openPicker(page);
    await setQuery(page, "SELECT PROJECT");
    const nodes = await page.evaluateHandle(() =>
      ["job-picker", "job-search", "job-results"].map((id) => document.getElementById(id)),
    );
    const old = await snapshot(page);
    await projectMode(page);
    assert.equal(
      await page.evaluate((ns) => ns.every((n) => n === document.getElementById(n.id)), nodes),
      true,
    );
    assert.deepEqual(await page.$$eval("dialog[open]", (ds) => ds.map((d) => d.id)), [
      "job-picker",
    ]);
    let s = await state(page);
    assert.deepEqual(
      [s.heading, s.label, s.listLabel, s.back],
      ["Select project", "Search projects", "Projects", true],
    );
    assert.equal(s.count, os.length + " projects");
    assert.equal(selected(s).key, "project:alpha");
    assert.deepEqual(
      s.rows.map(({ key, title, meta }) => ({ key, title, meta })),
      os.map((o) => ({
        key: "project:" + o.value,
        title: o.title,
        meta: o.value === "alpha" ? "Current project" : "",
      })),
    );
    for (const [q, values] of [
      ["  bEtA  ", ["beta"]],
      ["Synthetic Empty", ["empty"]],
      ["alpha", ["alpha"]],
      ["all projects", [""]],
      ["No project yet", [":unassigned"]],
      ["zz-no-match", []],
    ]) {
      await setQuery(page, q);
      s = await state(page);
      assert.deepEqual(
        s.rows.map((r) => r.key),
        values.map((v) => "project:" + v),
      );
      assert.equal(s.count, values.length + (values.length === 1 ? " project" : " projects"));
      selected(s);
      if (!values.length) {
        assert.equal(s.emptyText, "No projects found");
        await enter(page);
        assert.deepEqual(await snapshot(page), old);
      }
    }
    await page.click("#job-picker-close");
    await page.browserContext().close();
    for (const option of os) {
      const stored = option.value === "alpha" ? "" : "alpha";
      const native = await makePage({ stored }),
        picker = await makePage({ stored });
      await drawer(native);
      await drawer(picker);
      const initial = await snapshot(picker);
      await headerFilter(native, option.value);
      await openPicker(picker);
      await projectMode(picker, true);
      await clickKey(picker, "project:" + option.value);
      const actual = await snapshot(picker);
      assert.deepEqual(actual, await snapshot(native), "native parity " + option.value);
      assert.equal(actual.hash, initial.hash);
      assert.equal(actual.drawer, false);
      assert.equal(actual.tab, initial.tab);
      await picker.bringToFront();
      await picker.reload({ waitUntil: "domcontentloaded" });
      await native.bringToFront();
      await native.reload({ waitUntil: "domcontentloaded" });
      await picker.waitForSelector("#filter:not([hidden])", { polling: 50 });
      await native.waitForSelector("#filter:not([hidden])", { polling: 50 });
      await picker.waitForSelector("#d-tabs .tab.active", { polling: 50 });
      await native.waitForSelector("#d-tabs .tab.active", { polling: 50 });
      assert.deepEqual(
        await snapshot(picker),
        await snapshot(native),
        "persisted parity " + option.value,
      );
      assert.equal(await current(picker), option.value);
      await native.browserContext().close();
      await picker.browserContext().close();
    }
    for (const stored of ["gone-slug", "beta"]) {
      const p = await makePage({ stored });
      assert.equal(await current(p), stored === "beta" ? "beta" : "");
      await openPicker(p);
      await projectMode(p);
      assert.equal(selected(await state(p)).key, "project:" + (stored === "beta" ? "beta" : ""));
      await p.browserContext().close();
    }
    for (const value of ["alpha", "", ":unassigned"]) {
      const p = await makePage();
      await openPicker(p);
      await projectMode(p);
      await clickKey(p, "project:" + value);
      await openPicker(p);
      await setQuery(p, "new id");
      await enter(p);
      await p.waitForSelector("#idea-dlg[open]", { polling: 50 });
      assert.equal(
        await p.$eval("#idea-project", (el) => el.value),
        value === "alpha" ? "alpha" : "",
      );
      await p.keyboard.press("Escape");
      await p.browserContext().close();
    }
    console.log("PASS projects");
  }
  async function keyboard() {
    const p = await makePage();
    await drawer(p);
    await support.focusVisible(p, "#d-close");
    for (const modifier of ["Meta", "Control"]) {
      await openPicker(p, modifier);
      await setQuery(p, "  SeLeCt Proj  ");
      const old = await snapshot(p);
      await projectMode(p);
      const s = await state(p);
      await shortcut(p, modifier);
      assert.deepEqual(await state(p), s, "shortcut refocus preserves mode");
      for (let i = 0; i < s.rows.length + 2; i++) await p.keyboard.press("ArrowUp");
      assert.equal(selected(await state(p)).key, s.rows[0].key);
      for (let i = 0; i < s.rows.length + 2; i++) await p.keyboard.press("ArrowDown");
      assert.equal(selected(await state(p)).key, s.rows.at(-1).key);
      assert.equal((await state(p)).focus, "job-search");
      await p.keyboard.press("Tab");
      assert.equal(await p.evaluate(() => document.activeElement.id), "job-picker-back");
      await p.keyboard.press("Tab");
      assert.equal(await p.evaluate(() => document.activeElement.id), "job-picker-close");
      await p.click("#job-picker-back");
      assert.equal((await state(p)).query, "  SeLeCt Proj  ");
      assert.equal(selected(await state(p)).key, command);
      assert.deepEqual(await snapshot(p), old);
      await projectMode(p);
      await p.keyboard.press("Escape");
      assert.equal((await state(p)).heading, "Jump to job");
      assert.equal((await state(p)).query, "  SeLeCt Proj  ");
      assert.equal(selected(await state(p)).key, command);
      assert.deepEqual(await snapshot(p), old);
      await p.keyboard.press("Escape");
      assert.equal(await p.$eval("#job-picker", (el) => el.open), false);
      assert.equal(await p.evaluate(() => document.activeElement.id), "d-close");
      assert.equal(await p.$eval("#detail", (el) => el.hidden), false);
      await openPicker(p, modifier);
      await projectMode(p);
      await p.click("#job-picker-close");
      assert.equal(await p.$eval("#job-picker", (el) => el.open), false);
      await openPicker(p, modifier);
      assert.equal((await state(p)).heading, "Jump to job");
      assert.equal((await state(p)).query, "");
      await projectMode(p);
      await choose(p, "project:beta");
      assert.equal((await enter(p)).key, "project:beta");
      assert.equal(await current(p), "beta");
      await support.focusVisible(p, "#d-close");
    }
    await openPicker(p);
    await setQuery(p, "new id");
    await enter(p);
    await p.waitForSelector("#idea-dlg[open]", { polling: 50 });
    for (const m of ["Meta", "Control"]) {
      await shortcut(p, m);
      assert.equal(await p.$eval("#job-picker", (el) => el.open), false);
    }
    await p.keyboard.press("Escape");
    await p.browserContext().close();
    console.log("PASS keyboard");
  }
  async function refresh() {
    const p = await makePage();
    await openPicker(p);
    await projectMode(p);
    await setQuery(p, "synthetic");
    await choose(p, "project:beta");
    const before = await state(p);
    assert.equal(await p.evaluate(() => poll()), true);
    assert.deepEqual(await state(p), before, "no-change poll identity");
    try {
      await conditionalPut(
        origin,
        "/api/projects/beta.md",
        projectText("Synthetic Renamed"),
        "text/markdown; charset=utf-8",
      );
      await p.evaluate(() => loadProjects());
      let s = await state(p);
      assert.equal(s.rows.find((r) => r.key === "project:beta").title, "Synthetic Renamed");
      assert.equal(s.query, before.query);
      assert.equal(selected(s).id, selected(before).id);
      assert.equal(s.heading, "Select project");
      await setQuery(p, "beta");
      assert.deepEqual(
        (await state(p)).rows.map((r) => r.key),
        ["project:beta"],
        "slug search independent of title",
      );
      await setQuery(p, "");
      await choose(p, "project:beta");
      const old = await snapshot(p);
      await p.evaluate(() => projects.delete("beta"));
      await enter(p);
      assert.deepEqual(await snapshot(p), old);
      assert.ok(!(await state(p)).rows.some((r) => r.key === "project:beta"));
      assert.equal(await p.$eval("#job-picker", (el) => el.open), true);
      await p.evaluate(() => loadProjects());
      await choose(p, "project:beta");
      await p.$eval("#filter", (el) => {
        el.disabled = true;
      });
      await enter(p);
      assert.deepEqual(await snapshot(p), old);
      await p.$eval("#filter", (el) => {
        el.disabled = false;
      });
    } finally {
      await conditionalPut(
        origin,
        "/api/projects/beta.md",
        projectText("Synthetic Beta"),
        "text/markdown; charset=utf-8",
      );
    }
    await p.browserContext().close();
    const variant = {},
      v = await makePage({ variant });
    await openPicker(v);
    await projectMode(v);
    await choose(v, "project:beta");
    const list = await (await fetch(origin + "/projects.json")).json();
    variant.response = {
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(list.filter((r) => r.slug !== "beta")),
    };
    await v.evaluate(() => loadProjects());
    assert.ok(!(await state(v)).rows.some((r) => r.key === "project:beta"));
    assert.equal(selected(await state(v)).key, "project:");
    await setQuery(v, "beta");
    assert.equal((await state(v)).descendant, null);
    variant.response.body = "[]";
    await v.evaluate(() => loadProjects());
    await setQuery(v, "");
    assert.deepEqual(
      (await state(v)).rows.map((r) => r.key),
      (await options(v)).map((o) => "project:" + o.value),
    );
    assert.equal((await state(v)).rows.length, 2);
    await v.browserContext().close();
    const failed = await makePage({
      variant: { response: { status: 503, contentType: "application/json", body: "[]" } },
    });
    await openPicker(failed);
    await projectMode(failed);
    assert.deepEqual(
      (await state(failed)).rows.map((r) => r.key),
      (await options(failed)).map((o) => "project:" + o.value),
    );
    assert.equal((await state(failed)).rows.length, 2);
    await failed.browserContext().close();
    console.log("PASS refresh");
  }
  async function regression() {
    for (const context of [false, true]) {
      const p = await makePage();
      if (context) await drawer(p);
      await openPicker(p);
      for (const q of ["", "synthetic", "spec", "session", "new id", "beta", "zz-no-match"]) {
        await setQuery(p, q);
        const rows = (s) =>
          s.rows
            .filter((r) => r.key !== command)
            .map(({ key, title, meta, kind }) => ({ key, title, meta, kind }));
        const expected = expectedRows(q, context).filter((r) => r.key !== command);
        assert.deepEqual(
          rows(await state(p)),
          expected,
          "explicit rows " + JSON.stringify(q) + " context " + context,
        );
      }
      await p.browserContext().close();
    }
    const p = await makePage();
    await headerFilter(p, "alpha");
    await drawer(p);
    await openPicker(p);
    let s = await state(p);
    assert.deepEqual(
      s.rows.filter((r) => r.kind === "link").map((r) => r.key),
      ["pr", "paseo", "paseo-web"].map((k) => "link:093-context:" + k),
    );
    assert.deepEqual(
      await p.$$eval("#d-pr, #d-paseo, #d-paseo-web", (as) =>
        as.map((a) => ({ href: a.getAttribute("href"), target: a.target, rel: a.rel })),
      ),
      [
        {
          href: "https://github.com/example/sample/pull/42",
          target: "_blank",
          rel: "noopener noreferrer",
        },
        { href: "session://h/synthetic-server/agent/synthetic-agent", target: "", rel: "" },
        {
          href: "https://sessions.example.test/synthetic-server/synthetic-agent",
          target: "_blank",
          rel: "noopener noreferrer",
        },
      ],
    );
    await setQuery(p, "spec");
    await clickKey(p, "page:093-context:spec.md");
    assert.equal(await p.$eval("#d-tabs .tab.active", (el) => el.dataset.name), "spec.md");
    await support.focusVisible(p, "#d-close");
    await openPicker(p);
    await setQuery(p, "new id");
    await enter(p);
    assert.equal(await p.$eval("#job-picker", (el) => el.open), false);
    assert.equal(await p.evaluate(() => document.activeElement.id), "idea-text");
    await p.keyboard.press("Escape");
    await p.waitForFunction(() => document.activeElement.id === "d-close", { polling: 50 });
    await openPicker(p);
    await setQuery(p, "beta");
    assert.equal((await enter(p)).key, "job:091-beta");
    await p.waitForFunction(() => openId === "091-beta", { polling: 50 });
    assert.equal(await current(p), "alpha");
    await openPicker(p);
    await projectMode(p);
    await p.goBack();
    await p.waitForFunction(() => openId === "093-context", { polling: 50 });
    await p.waitForSelector('#d-tabs .tab[data-name="spec.md"]', { polling: 50 });
    await p.click("#job-picker-back");
    s = await state(p);
    assert.ok(s.rows.some((r) => r.key === "page:093-context:spec.md"));
    assert.ok(!s.rows.some((r) => r.key.startsWith("page:091-beta:")));
    await projectMode(p);
    await p.goForward();
    await p.waitForFunction(() => openId === "091-beta", { polling: 50 });
    await p.click("#job-picker-back");
    assert.ok(!(await state(p)).rows.some((r) => /^(page|link):093-context:/.test(r.key)));
    await p.browserContext().close();
    console.log("PASS regression");
  }
  try {
    console.log(JSON.stringify({ browser: await browser.version() }));
    for (const run of [commands, projectsCase, keyboard, refresh, regression]) await run();
  } finally {
    console.log(
      JSON.stringify({
        attemptedWrites: writes,
        blockedExternal: external,
        successfulExternal: successes,
        javascriptErrors: errors,
      }),
    );
    await browser.close();
    assert.deepEqual(writes, []);
    assert.deepEqual(external, []);
    assert.deepEqual(successes, []);
    assert.deepEqual(errors, []);
  }
}

(async () => {
  const i = process.argv.indexOf("--browser");
  assert.ok(
    i >= 0 && process.argv.length === i + 2,
    "jumper project requires --browser <owned-loopback-origin>",
  );
  await browserRun(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
