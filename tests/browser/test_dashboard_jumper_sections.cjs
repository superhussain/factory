// Jumper sections: exact groups/state order/needs-you first/stable ties, search and
// count and empty cases, ARIA structure, page/new-idea activation, polling and
// selection stability. Requires --browser <owned-loopback-origin>.
const assert = require("node:assert/strict");
const { waitForBoard, clickNow } = require("../support/board-ready.cjs");

const command = "command:new-idea";
const stages = ["idea", "spec", "build", "review", "approve", "pr-open", "merged", "closed"];
const ids = [
  "080-idea",
  "081-spec",
  "082-build",
  "083-review",
  "084-approve",
  "085-pr",
  "086-merged",
  "087-closed",
  "088-ask",
  "089-human",
  "090-escalated",
  "091-legacy-escalated",
  "092-context",
  "093-match",
  "094-other",
  "095-blank",
  "fixture-build",
  "082-tie",
  "082-again",
];
const fixtureJobs = ids.map((id, i) => ({
  id,
  title: "Synthetic job " + id,
  project: "sample",
  updated: "",
  ...(i < 8
    ? { stage: stages[i] }
    : i === 15
      ? {}
      : {
          stage:
            {
              "089-human": "needs-human",
              "090-escalated": "pr-open",
              "091-legacy-escalated": "pr-open",
              "093-match": "idea",
              "094-other": "future-stage",
            }[id] || "build",
        }),
  ...(["086-merged", "087-closed"].includes(id) ? { needs: "Stale terminal ask" } : {}),
  ...(id === "088-ask" ? { needs: "Synthetic question" } : {}),
  ...(id === "090-escalated" ? { escalated: true } : {}),
  ...(id === "091-legacy-escalated" ? { note: "escalated: Synthetic approval" } : {}),
  ...(id === "092-context"
    ? {
        title: "New idea matching job 092",
        pr: "https://github.com/example/sample/pull/42",
        paseo: { server: "synthetic-server", agent: "synthetic-agent" },
      }
    : {}),
  ...(id === "093-match" ? { title: "New idea matching job 093" } : {}),
}));
const keys = (ids) => ids.map((id) => "job:" + id);
const expectedJobs = {
  "Needs you": keys(["091-legacy-escalated", "090-escalated", "089-human", "088-ask"]),
  Idea: keys(["093-match", "080-idea"]),
  Spec: keys(["081-spec"]),
  Build: keys(["092-context", "082-build", "082-tie", "082-again", "fixture-build"]),
  Review: keys(["083-review"]),
  Approve: keys(["084-approve"]),
  "PR open": keys(["085-pr"]),
  Merged: keys(["086-merged"]),
  Closed: keys(["087-closed"]),
  "Other jobs": keys(["095-blank", "094-other"]),
};
const writes = [],
  external = [],
  successes = [],
  errors = [];

async function conditionalPut(origin, route, body, type) {
  const old = await fetch(origin + route, { redirect: "error" });
  assert.ok([200, 404].includes(old.status), "GET before PUT " + route);
  const headers = { "Content-Type": type };
  if (old.status === 404) headers["If-None-Match"] = "*";
  else {
    headers["If-Match"] = old.headers.get("etag");
    assert.ok(headers["If-Match"], "observed ETag");
  }
  const response = await fetch(origin + route, { method: "PUT", body, headers, redirect: "error" });
  assert.equal(response.status, 200, "conditional PUT " + route);
}
const putBoard = (origin, jobs) =>
  conditionalPut(origin, "/api/board", JSON.stringify({ updated: "", jobs }), "application/json");

async function seed(origin) {
  await putBoard(origin, fixtureJobs);
  await conditionalPut(
    origin,
    "/api/projects/sample.md",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    "text/markdown; charset=utf-8",
  );
  for (const name of ["idea.md", "spec.md", "build.md", "rework-1.md"])
    await conditionalPut(
      origin,
      "/api/jobs/sample/092-context/" + name,
      "# Synthetic " + name + "\n",
      "text/markdown",
    );
  console.log("PASS conditional synthetic seed");
}

async function openPicker(page, modifier = "Meta") {
  await page.bringToFront();
  await page.keyboard.down(modifier);
  await page.keyboard.press("k");
  await page.keyboard.up(modifier);
  await page.waitForSelector("#job-picker[open]", { polling: 50 });
  assert.equal(await page.$eval("#job-search", (el) => el.value), "");
}
async function setQuery(page, text) {
  await page.bringToFront();
  await page.click("#job-search");
  const length = await page.$eval("#job-search", (el) => el.value.length);
  for (let i = 0; i <= length; i++) await page.keyboard.press("ArrowRight");
  for (let i = 0; i < length; i++) await page.keyboard.press("Backspace");
  assert.equal(await page.$eval("#job-search", (el) => el.value), "", "query cleared");
  if (text) await page.keyboard.type(text);
  assert.equal(await page.$eval("#job-search", (el) => el.value), text, "exact query");
}
async function state(page) {
  return page.evaluate(() => {
    const options = [...document.querySelectorAll('#job-results [role="option"]')];
    return {
      rows: jobResults.map((r, i) => ({
        key: r.key,
        kind: r.kind,
        meta: r.meta,
        id: options[i].id,
        index: options[i].dataset.index,
        selected: options[i].getAttribute("aria-selected"),
      })),
      groups: [...document.querySelectorAll('#job-results [role="group"]')].map((g) => ({
        title: document.getElementById(g.getAttribute("aria-labelledby"))?.textContent,
        label: g.getAttribute("aria-labelledby"),
        first: g.firstElementChild.id,
        keys: [...g.querySelectorAll('[role="option"]')].map(
          (o) => jobResults[Number(o.dataset.index)].key,
        ),
      })),
      count: document.getElementById("job-count").textContent,
      empty: !document.getElementById("job-empty").hidden,
      emptyText: document.getElementById("job-empty").textContent,
      descendant: document.getElementById("job-search").getAttribute("aria-activedescendant"),
      focus: document.activeElement.id,
    };
  });
}
function selected(s) {
  const active = s.rows.filter((r) => r.selected === "true");
  assert.equal(active.length, s.rows.length ? 1 : 0);
  assert.equal(s.descendant, active[0]?.id ?? null);
  return active[0];
}
async function enter(page) {
  const row = selected(await state(page));
  console.log(JSON.stringify({ beforeEnter: row ?? null }));
  await page.keyboard.press("Enter");
  return row;
}
async function drawer(page) {
  await page.bringToFront();
  await clickNow(page, '.card[data-id="092-context"]');
  await page.waitForSelector('#d-tabs .tab[data-name="spec.md"]', { polling: 50 });
  await page.waitForFunction(
    () => document.getElementById("d-doc").textContent.includes("Synthetic"),
    { polling: 50 },
  );
  return page.$$eval("#d-tabs .tab", (tabs) =>
    tabs.map((tab) => "page:092-context:" + tab.dataset.name),
  );
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

async function browserRun(origin) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(origin);
  const puppeteer = require("puppeteer-core");
  await seed(origin);
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  async function makePage(mobile = false) {
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
    page.on("pageerror", (e) => errors.push(e.message));
    page.on("response", (r) => {
      if (new URL(r.url()).origin !== origin && r.ok()) successes.push(r.url());
    });
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
      return r.continue();
    });
    await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
    await waitForBoard(page);
    await page.waitForSelector(".card", { polling: 50 });
    await page.waitForFunction(() => projects !== null, { polling: 50 });
    return page;
  }
  async function sections() {
    const page = await makePage();
    for (const withDrawer of [false, true]) {
      const pages = withDrawer ? await drawer(page) : [];
      await openPicker(page);
      const s = await state(page),
        expected = {
          ...(withDrawer
            ? { Pages: pages, Links: ["paseo", "paseo-web"].map((k) => "link:092-context:" + k) }
            : {}),
          "New idea": [command],
          "Select project": ["command:select-project"],
          ...expectedJobs,
        };
      assert.deepEqual(
        s.groups.map((g) => [g.title, g.keys]),
        Object.entries(expected),
        "exact groups and stable state order",
      );
      assert.ok(
        !s.rows.some((r) => r.key === "link:092-context:pr"),
        "build-stage PR action remains absent",
      );
      assert.equal(new Set(s.rows.map((r) => r.key)).size, s.rows.length);
      assert.equal(s.rows.filter((r) => r.kind === "job").length, 19);
      assert.deepEqual(
        s.rows.map((r) => Number(r.index)),
        s.rows.map((_, i) => i),
      );
      assert.equal(new Set(s.groups.map((g) => g.label)).size, s.groups.length);
      assert.ok(s.groups.every((g) => g.label && g.label === g.first));
      assert.equal(
        await page.$$eval(".job-section-heading", (els) =>
          els.every(
            (el) =>
              el.getAttribute("role") === "presentation" &&
              !["tabindex", "data-index", "aria-selected"].some((n) => el.hasAttribute(n)),
          ),
        ),
        true,
      );
      selected(s);
      await page.keyboard.press("Escape");
    }
    await page.browserContext().close();
  }
  async function search() {
    const page = await makePage();
    for (const withDrawer of [false, true]) {
      const pages = withDrawer ? await drawer(page) : [];
      await openPicker(page);
      for (const text of [
        "",
        "new id",
        "NEW IDEA",
        "add idea",
        "spec",
        "session",
        "needs you",
        "zz-no-match",
      ]) {
        await setQuery(page, text);
        const s = await state(page);
        const matchingCommand = ["", "new id", "NEW IDEA", "add idea"].includes(text);
        const contextual = !withDrawer
          ? {}
          : !text
            ? { Pages: pages, Links: ["paseo", "paseo-web"].map((k) => "link:092-context:" + k) }
            : text === "spec"
              ? { Pages: ["page:092-context:spec.md"] }
              : text === "session"
                ? { Links: ["link:092-context:paseo", "link:092-context:paseo-web"] }
                : {};
        const matchingJobs = !text
          ? expectedJobs
          : ["new id", "NEW IDEA"].includes(text)
            ? { Idea: ["job:093-match"], Build: ["job:092-context"] }
            : text === "spec"
              ? { Spec: ["job:081-spec"] }
              : {};
        const expected = {
          ...contextual,
          ...(matchingCommand ? { "New idea": [command] } : {}),
          ...(text === "" ? { "Select project": ["command:select-project"] } : {}),
          ...matchingJobs,
        };
        assert.deepEqual(
          s.groups.map((g) => [g.title, g.keys]),
          Object.entries(expected),
          text + " groups/keys",
        );
        assert.deepEqual(
          s.rows.map((r) => r.key),
          Object.values(expected).flat(),
        );
        const jobs = Object.values(matchingJobs).flat().length,
          actions = Object.values(contextual).flat().length;
        assert.equal(
          s.count,
          `${jobs} ${jobs === 1 ? "job" : "jobs"}${actions ? " · " + actions + " for 092-context" : ""}${text === "" ? " · 2 commands" : matchingCommand ? " · 1 command" : ""}`,
        );
        assert.equal(s.empty, !s.rows.length);
        assert.equal(s.emptyText, withDrawer ? "No matching jobs or actions" : "No jobs found");
        selected(s);
      }
      for (const kind of ["hidden", "disabled", "absent"]) {
        await setQuery(page, "add idea");
        await page.evaluate((kind) => {
          window.savedTrigger = document.getElementById("new-idea");
          if (kind === "absent") savedTrigger.remove();
          else savedTrigger[kind] = true;
          renderJobResults();
        }, kind);
        const s = await state(page);
        assert.deepEqual(s.groups, []);
        assert.deepEqual(s.rows, []);
        assert.equal(s.count, "0 jobs");
        assert.equal(s.empty, true);
        selected(s);
        await page.evaluate(() => {
          savedTrigger.hidden = false;
          savedTrigger.disabled = false;
          if (!savedTrigger.isConnected) document.getElementById("job-jump").before(savedTrigger);
          delete window.savedTrigger;
          renderJobResults();
        });
        assert.equal(selected(await state(page)).key, command);
      }
      await page.keyboard.press("Escape");
    }
    await page.browserContext().close();
  }
  async function keyboard() {
    const page = await makePage(),
      pages = await drawer(page);
    for (const modifier of ["Meta", "Control"]) {
      await support.focusVisible(page, "#d-close");
      await openPicker(page, modifier);
      const s = await state(page);
      for (let i = 0; i <= s.rows.length; i++) {
        const now = await state(page);
        assert.equal(selected(now).key, s.rows[Math.min(i, s.rows.length - 1)].key);
        assert.equal(now.focus, "job-search");
        await page.keyboard.press("ArrowDown");
      }
      for (let i = s.rows.length - 1; i >= -1; i--) {
        const now = await state(page);
        assert.equal(selected(now).key, s.rows[Math.max(i, 0)].key);
        assert.equal(now.focus, "job-search");
        await page.keyboard.press("ArrowUp");
      }
      const old = selected(await state(page));
      await page.click(".job-section-heading");
      assert.deepEqual(selected(await state(page)), old);
      assert.equal(await page.$eval("#job-picker", (el) => el.open), true);
      await page.click("#job-search");
      const activePage = await enter(page);
      assert.equal(activePage.key, pages[0]);
      await page.waitForFunction(
        (n) => document.querySelector("#d-tabs .tab.active")?.dataset.name === n,
        { polling: 50 },
        activePage.key.split(":")[2],
      );
      assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
      await openPicker(page, modifier);
      await choose(page, "job:081-spec");
      assert.equal((await enter(page)).key, "job:081-spec");
      await page.waitForFunction(
        () => location.hash.startsWith("#081-spec") && !document.getElementById("detail").hidden,
        { polling: 50 },
      );
      await page.click("#d-close");
      await drawer(page);
      await support.focusVisible(page, "#d-close");
      await openPicker(page, modifier);
      await setQuery(page, "add idea");
      assert.equal((await enter(page)).key, command);
      await page.waitForSelector("#idea-dlg[open]", { polling: 50 });
      assert.equal(await page.evaluate(() => document.activeElement.id), "idea-text");
      assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
      await page.keyboard.press("Escape");
      await page.waitForFunction(
        () => !document.getElementById("idea-dlg").open && document.activeElement.id === "d-close",
        { polling: 50 },
      );
      assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
      await openPicker(page, modifier);
      await setQuery(page, "zz-no-match");
      assert.equal(await enter(page), undefined);
      assert.equal(await page.$eval("#job-picker", (el) => el.open), true);
      assert.equal(await page.$eval("#idea-dlg", (el) => el.open), false);
      assert.ok((await page.evaluate(() => location.hash)).startsWith("#092-context"));
      await page.keyboard.press("Escape");
    }
    await openPicker(page);
    await page.click('#job-results [role="option"][data-index="1"]');
    assert.equal(
      await page.$eval("#d-tabs .tab.active", (el) => el.dataset.name),
      pages[1].split(":")[2],
    );
    assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
    await openPicker(page);
    await setQuery(page, "081-spec");
    await page.click('#job-results [role="option"]');
    await page.waitForFunction(() => location.hash.startsWith("#081-spec"), { polling: 50 });
    assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
    await page.browserContext().close();
  }
  async function pollCase() {
    const page = await makePage();
    await openPicker(page);
    await setQuery(page, "new id");
    await choose(page, "job:092-context");
    const old = selected(await state(page));
    try {
      await putBoard(
        origin,
        fixtureJobs.map((j) =>
          j.id === "092-context" ? { ...j, needs: "Synthetic poll ask" } : j,
        ),
      );
      assert.equal(await page.evaluate(() => poll()), true);
      const s = await state(page);
      assert.deepEqual(selected(s), { ...old, index: "1" });
      assert.equal(
        s.groups.find((g) => g.title === "Needs you").keys.filter((k) => k === old.key).length,
        1,
      );
      assert.equal(s.rows.filter((r) => r.key === old.key).length, 1);
      assert.equal(s.focus, "job-search");
      assert.equal(await page.$eval("#job-search", (el) => el.value), "new id");
    } finally {
      await putBoard(origin, fixtureJobs);
    }
    assert.equal(await page.evaluate(() => poll()), true);
    assert.equal(selected(await state(page)).id, old.id);
    try {
      await putBoard(
        origin,
        fixtureJobs.filter((j) => j.id !== "092-context"),
      );
      assert.equal(await page.evaluate(() => poll()), true);
      assert.equal(selected(await state(page)).key, command);
      assert.equal((await state(page)).focus, "job-search");
    } finally {
      await putBoard(origin, fixtureJobs);
    }
    assert.equal(await page.evaluate(() => poll()), true);
    await page.keyboard.press("Escape");
    const pages = await drawer(page);
    await openPicker(page);
    await choose(page, pages[1]);
    const oldPage = selected(await state(page));
    assert.equal(await page.evaluate(() => poll()), true);
    assert.deepEqual(selected(await state(page)), oldPage);
    assert.equal((await state(page)).focus, "job-search");
    await page.browserContext().close();
  }
  try {
    console.log(JSON.stringify({ browser: await browser.version() }));
    for (const [name, run] of Object.entries({ sections, search, keyboard, poll: pollCase })) {
      await run();
      console.log("PASS " + name);
    }
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
    "jumper sections requires --browser <owned-loopback-origin>",
  );
  await browserRun(process.argv[i + 1]);
})().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
