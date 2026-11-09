// Jumper order: exact rows/order/counts, page/link ownership, new-idea activation,
// hidden/disabled/missing triggers, Meta/Control/arrows/focus/close/poll and
// zero-project capture. Requires --browser <owned-loopback-origin>.
const assert = require("node:assert/strict");
const { waitForBoard, clickNow } = require("../support/board-ready.cjs");

const fixtureJobs = Array.from({ length: 12 }, (_, i) => {
  const n = 80 + i;
  return {
    id: n === 91 ? "091-new-idea" : String(n).padStart(3, "0") + "-fixture",
    title: n >= 90 ? "New idea matching job " + String(n).padStart(3, "0") : "Synthetic job " + n,
    project: "sample",
    stage: n === 90 ? "merged" : "build",
    updated: "",
    ...(n === 91
      ? {
          pr: "https://github.com/example/sample/pull/91",
          paseo: { server: "synthetic-server", agent: "synthetic-agent" },
        }
      : {}),
  };
});
const commandKey = "command:new-idea";
const writes = [],
  external = [],
  externalSuccess = [],
  errors = [];

// The explicit current row set: drawer pages/links first, matching commands, then jobs
// grouped newest-first by stage. Derived from the fixture, not a historical snapshot.
function expectedRows(text, withDrawer) {
  const tokens = text.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const actions = !withDrawer
    ? []
    : [
        {
          key: "page:091-new-idea:spec.md",
          title: "spec",
          meta: "Page of 091-new-idea · spec.md",
          kind: "page",
          words: "spec spec md page",
        },
        {
          key: "link:091-new-idea:paseo",
          title: "Open in Session app",
          meta: "Link of 091-new-idea",
          kind: "link",
          words: "open in session app link session app",
        },
        {
          key: "link:091-new-idea:paseo-web",
          title: "Open in Session",
          meta: "Link of 091-new-idea · opens in a new tab",
          kind: "link",
          words: "open in session link session",
        },
      ]
        .filter((r) => tokens.every((t) => r.words.includes(t)))
        .map(({ words, ...r }) => r);
  const commands = [
    {
      key: commandKey,
      title: "New idea",
      meta: "Command · Capture an idea",
      kind: "command",
      words: "new idea add command",
    },
    {
      key: "command:select-project",
      title: "Select project",
      meta: "Command · Change the board project filter",
      kind: "command",
      words: "select project command",
    },
  ]
    .filter((r) => tokens.every((t) => r.words.includes(t)))
    .map(({ words, ...r }) => r);
  const q = text.trim().toLowerCase();
  const matched = fixtureJobs
    .filter((j) =>
      [j.id, j.title, j.project, "Synthetic sample"].some((f) =>
        String(f ?? "")
          .toLowerCase()
          .includes(q),
      ),
    )
    .sort((a, b) => (/^(\d+)/.exec(b.id)?.[1] ?? 0) - (/^(\d+)/.exec(a.id)?.[1] ?? 0));
  const jobs = ["build", "merged"].flatMap((stage) =>
    matched
      .filter((j) => j.stage === stage)
      .map((j) => ({
        key: "job:" + j.id,
        title: j.title,
        meta: j.id + " · Synthetic sample · " + j.stage,
        kind: "job",
      })),
  );
  return [...actions, ...commands, ...jobs];
}

async function conditionalPut(origin, route, body, type) {
  const get = await fetch(origin + route, { redirect: "error" });
  assert.ok([200, 404].includes(get.status), "read " + route + " before conditional seed");
  const headers = {};
  if (get.status === 404) headers["If-None-Match"] = "*";
  else {
    headers["If-Match"] = get.headers.get("etag");
    assert.ok(headers["If-Match"], route + " ETag required");
  }
  if (type) headers["Content-Type"] = type;
  const response = await fetch(origin + route, { method: "PUT", body, headers, redirect: "error" });
  assert.equal(response.status, 200, route + " seed response");
}

async function seed(origin) {
  await conditionalPut(origin, "/api/board", JSON.stringify({ updated: "", jobs: fixtureJobs }));
  await conditionalPut(
    origin,
    "/api/projects/sample.md",
    "---\ntitle: Synthetic sample\nrepo: example/sample\nbase: main\n---\n",
    "text/markdown",
  );
  const spec = await fetch(origin + "/api/jobs/sample/091-new-idea/spec.md", {
    method: "PUT",
    body: "# Synthetic spec\n",
    redirect: "error",
  });
  assert.equal(spec.status, 200, "job spec seed response");
  console.log("PASS synthetic local R2 seed");
}

async function shortcut(page, modifier = "Meta") {
  await page.keyboard.down(modifier);
  await page.keyboard.press("k");
  await page.keyboard.up(modifier);
}

async function openPicker(page, modifier = "Meta") {
  await page.bringToFront();
  await shortcut(page, modifier);
  await page.waitForSelector("#job-picker[open]");
  assert.equal(await page.$eval("#job-search", (el) => el.value), "", "fresh picker clears query");
}

async function query(page, text) {
  await page.bringToFront();
  await page.click("#job-search");
  assert.equal(
    await page.evaluate(() => document.activeElement.id),
    "job-search",
    "query input focused",
  );
  const stale = await page.$eval("#job-search", (el) => el.value);
  await page.keyboard.press("End");
  for (let i = 0; i < stale.length; i++) await page.keyboard.press("Backspace");
  assert.equal(
    await page.$eval("#job-search", (el) => el.value),
    "",
    "existing query removed before typing",
  );
  if (text) await page.keyboard.type(text);
  assert.equal(
    await page.$eval("#job-search", (el) => el.value),
    text,
    "input exactly equals intended query",
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
    descendant: document.getElementById("job-search").getAttribute("aria-activedescendant"),
    count: document.getElementById("job-count").textContent,
    empty: !document.getElementById("job-empty").hidden,
    emptyText: document.getElementById("job-empty").textContent,
  }));
}

function selected(s) {
  const rows = s.rows.filter((r) => r.selected === "true");
  assert.equal(rows.length, s.rows.length ? 1 : 0, "exactly one selected option when nonempty");
  assert.equal(s.descendant, rows[0]?.id ?? null, "active descendant identifies selected option");
  return rows[0];
}

// Interval polling (50ms) because rAF-driven waits stall when a backgrounded headless
// CI page throttles its rendering loop. Staged so a timeout names which half raced.
async function waitIdeaClosed(page, focusId) {
  try {
    await page.waitForFunction(() => !document.getElementById("idea-dlg").open, { polling: 50 });
    // Blink queues the close event, so without a focus target wait for its listener to run.
    if (focusId === undefined) {
      await page.waitForFunction(() => !ideaFromPicker, { polling: 50 });
    } else {
      await page.waitForFunction(
        (id) => document.activeElement.id === id,
        { polling: 50 },
        focusId,
      );
    }
  } catch (error) {
    const observed = await page
      .evaluate(() => ({
        ideaOpen: document.getElementById("idea-dlg").open,
        pickerOpen: document.getElementById("job-picker").open,
        active: document.activeElement.id,
        detailHidden: document.getElementById("detail").hidden,
        ideaFromPicker,
        pickerFocus: pickerFocus?.id ?? null,
      }))
      .catch(() => null);
    console.error(JSON.stringify({ waitIdeaClosedTimeout: { focusId, observed } }));
    throw error;
  }
}

async function enter(page) {
  console.log(JSON.stringify({ beforeEnter: selected(await state(page)) ?? null }));
  await page.keyboard.press("Enter");
}

async function drawer(page) {
  await page.bringToFront();
  await clickNow(page, '.card[data-id="091-new-idea"]');
  await page.waitForSelector('#d-tabs .tab[data-name="spec.md"]');
  await page.waitForFunction(
    () => document.getElementById("d-doc").textContent.includes("Synthetic spec"),
    { polling: 50 },
  );
}

async function geometry(page) {
  const g = await page.evaluate(() => {
    const list = document.getElementById("job-results");
    const index = jobResults.findIndex((r) => r.kind === "command");
    const row = list.querySelectorAll('[role="option"]')[index];
    const r = row.getBoundingClientRect(),
      l = list.getBoundingClientRect();
    return {
      top: r.top,
      bottom: r.bottom,
      listTop: l.top,
      listBottom: l.bottom,
      scroll: list.scrollTop,
      width: document.documentElement.scrollWidth,
      viewport: document.documentElement.clientWidth,
      listWidth: list.scrollWidth,
      listClient: list.clientWidth,
    };
  });
  assert.equal(g.scroll, 0, "command visible at initial scroll position");
  assert.ok(
    g.top >= g.listTop && g.bottom <= g.listBottom + 1,
    "command visible without scrolling",
  );
  assert.ok(g.width <= g.viewport && g.listWidth <= g.listClient, "no horizontal overflow");
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
  let holdBoard;
  async function makePage(width = 1280, dark = false, zeroProjects = false) {
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await support.controlBoardPolling(page);
    await page.setViewport({ width, height: width === 1280 ? 800 : 844, deviceScaleFactor: 1 });
    await page.emulateMediaFeatures([
      { name: "prefers-color-scheme", value: dark ? "dark" : "light" },
    ]);
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("response", (response) => {
      if (new URL(response.url()).origin !== origin && response.ok())
        externalSuccess.push(response.url());
    });
    // Stubbed reads must reach interception rather than the dashboard service worker.
    await page.setBypassServiceWorker(true);
    await page.setRequestInterception(true);
    page.on("request", (request) => {
      const target = new URL(request.url());
      if (!["GET", "HEAD"].includes(request.method())) {
        writes.push(request.method() + " " + request.url());
        return request.abort();
      }
      if (target.origin !== origin) {
        external.push(request.url());
        return request.abort();
      }
      if (holdBoard && target.pathname === "/board.json") {
        const resolve = holdBoard;
        holdBoard = null;
        resolve(request);
        return;
      }
      if (zeroProjects && target.pathname === "/projects.json") {
        return request.respond({ status: 200, contentType: "application/json", body: "[]" });
      }
      return request.continue();
    });
    await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
    await waitForBoard(page);
    await page.waitForSelector(".card");
    await page.waitForFunction(() => projects !== null, { polling: 50 });
    return page;
  }
  try {
    console.log(JSON.stringify({ browser: await browser.version() }));

    const page = await makePage();
    const held = new Promise((resolve) => {
      holdBoard = resolve;
    });
    await page.evaluate(() => {
      window.fixturePendingPoll = poll();
    });
    const request = await held;
    assert.equal(await page.evaluate(() => typeof window.fixtureBoardPoll), "function");
    assert.equal(
      await page.evaluate(() => window.fixtureBoardPoll()),
      true,
      "scheduled poll applies",
    );
    const response = page.waitForResponse((r) => r.request() === request);
    await request.continue();
    assert.equal((await response).status(), 200, "superseded read returned successfully");
    assert.equal(
      await page.evaluate(() => window.fixturePendingPoll),
      false,
      "newer poll supersedes the held read",
    );
    const failed = new Promise((resolve) => {
      holdBoard = resolve;
    });
    await page.evaluate(() => {
      window.fixturePendingPoll = poll();
    });
    await (await failed).respond({ status: 503, body: "offline" });
    assert.equal(
      await page.evaluate(() => window.fixturePendingPoll),
      false,
      "network failure remains a failed poll",
    );
    assert.equal(await page.$eval("#dot", (el) => el.title), "board.json unreachable");
    assert.equal(await page.evaluate(() => poll()), true, "controlled explicit poll applies");
    console.log("PASS controlled polling supersession");
    for (const withDrawer of [false, true]) {
      if (withDrawer) await drawer(page);
      await openPicker(page);
      for (const text of ["", "new id", "NEW IDEA", "idea", "add idea", "spec", "zz-no-match"]) {
        await query(page, text);
        const actual = await state(page);
        const rows = (s) =>
          s.rows.map(({ key, title, meta, kind }) => ({ key, title, meta, kind }));
        assert.deepEqual(
          rows(actual),
          expectedRows(text, withDrawer),
          "grouped order for " + JSON.stringify(text),
        );
        const jobs = expectedRows(text, withDrawer).filter((r) => r.kind === "job").length;
        const actions = expectedRows(text, withDrawer).filter((r) =>
          ["page", "link"].includes(r.kind),
        ).length;
        const commands = expectedRows(text, withDrawer).filter((r) => r.kind === "command").length;
        assert.equal(
          actual.count,
          `${jobs} ${jobs === 1 ? "job" : "jobs"}${actions ? " · " + actions + " for 091-new-idea" : ""}` +
            (commands ? " · " + commands + (commands === 1 ? " command" : " commands") : ""),
          "exact count " + JSON.stringify(text),
        );
        assert.equal(actual.empty, actual.rows.length === 0);
        const active = selected(actual);
        const matching = ["", "new id", "NEW IDEA", "idea", "add idea"].includes(text);
        if (matching)
          assert.equal(
            active.key,
            withDrawer && text === "" ? "page:091-new-idea:spec.md" : commandKey,
          );
        if (text === "spec" && withDrawer) assert.equal(active.key, "page:091-new-idea:spec.md");
        if (!actual.rows.length) {
          assert.equal(
            actual.emptyText,
            withDrawer ? "No matching jobs or actions" : "No jobs found",
          );
          const hash = await page.evaluate(() => location.hash);
          await enter(page);
          assert.equal(await page.evaluate(() => location.hash), hash);
          assert.equal(await page.$eval("#job-picker", (el) => el.open), true, "empty Enter inert");
          assert.equal(await page.$eval("#idea-dlg", (el) => el.open), false);
        }
      }
      await page.keyboard.press("Escape");
    }
    console.log("PASS board/drawer queries, counts, empty Enter, exact fixture arrays");

    for (const withDrawer of [false, true])
      for (const modifier of ["Meta", "Control"]) {
        if (withDrawer) await support.focusVisible(page, "#d-close");
        else {
          if (!(await page.$eval("#detail", (el) => el.hidden))) await page.click("#d-close");
          await support.focusVisible(page, "#job-jump");
        }
        const focus = await page.evaluate(() => document.activeElement.id);
        await openPicker(page, modifier);
        assert.equal(
          selected(await state(page)).key,
          withDrawer ? "page:091-new-idea:spec.md" : commandKey,
          "fresh selection",
        );
        if (withDrawer) await query(page, "add idea");
        assert.equal(selected(await state(page)).key, commandKey, "command Enter destination");
        await enter(page);
        await page.waitForSelector("#idea-dlg[open]");
        assert.deepEqual(
          await page.evaluate(() =>
            [...document.querySelectorAll("dialog[open]")].map((el) => el.id),
          ),
          ["idea-dlg"],
        );
        assert.equal(await page.evaluate(() => document.activeElement.id), "idea-text");
        await shortcut(page, "Meta");
        await shortcut(page, "Control");
        assert.equal(
          await page.$eval("#job-picker", (el) => el.open),
          false,
          "shortcuts suppressed in idea modal",
        );
        await support.focusVisible(page, "#idea-text");
        await page.keyboard.press("Escape");
        await waitIdeaClosed(page, focus);
        assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
        await openPicker(page, modifier);
        await query(page, "new id");
        assert.equal(selected(await state(page)).key, commandKey, "mixed Enter destination");
        await enter(page);
        await page.waitForSelector("#idea-dlg[open]");
        assert.equal(await page.evaluate(() => document.activeElement.id), "idea-text");
        await page.keyboard.press("Escape");
        await waitIdeaClosed(page);
        await openPicker(page, modifier);
        await query(page, "new id");
        await page.keyboard.press("ArrowDown");
        const job = selected(await state(page));
        assert.equal(job.key, "job:091-new-idea");
        assert.equal(await page.evaluate(() => poll()), true);
        assert.deepEqual(selected(await state(page)), job, "poll keeps job key, ID and selection");
        await enter(page);
        await page.waitForFunction(
          () =>
            location.hash.startsWith("#091-new-idea") && !document.getElementById("detail").hidden,
          { polling: 50 },
        );
        await page.waitForSelector('#d-tabs .tab[data-name="spec.md"]');
        assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
        await openPicker(page, modifier);
        const tab = selected(await state(page));
        assert.equal(tab.key, "page:091-new-idea:spec.md");
        assert.equal(await page.evaluate(() => poll()), true);
        assert.deepEqual(selected(await state(page)), tab, "poll keeps page key, ID and selection");
        await enter(page);
        assert.equal(await page.$eval("#d-tabs .tab.active", (el) => el.dataset.name), "spec.md");
        assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
        await openPicker(page, modifier);
        await query(page, "new id");
        await page.keyboard.press("ArrowDown");
        await page.keyboard.press("ArrowUp");
        assert.equal(selected(await state(page)).key, commandKey, "ArrowUp returns to command");
        await page.click('#job-results [role="option"]');
        await page.waitForSelector("#idea-dlg[open]");
        assert.equal(
          await page.evaluate(() => document.activeElement.id),
          "idea-text",
          "command click focuses textarea",
        );
        await page.keyboard.press("Escape");
        await waitIdeaClosed(page);
      }
    console.log(
      "PASS Meta/Control activation, arrows, poll selection, focus, close/reopen and click",
    );

    for (const availability of ["hidden", "disabled", "absent"]) {
      for (const phase of ["render", "activation"]) {
        await openPicker(page);
        await query(page, "add idea");
        assert.equal(selected(await state(page)).key, commandKey);
        await page.evaluate((availability) => {
          window.savedIdeaTrigger = document.getElementById("new-idea");
          if (availability === "absent") window.savedIdeaTrigger.remove();
          else window.savedIdeaTrigger[availability] = true;
        }, availability);
        if (phase === "render") assert.equal(await page.evaluate(() => poll()), true);
        else await enter(page);
        const s = await state(page);
        assert.ok(
          s.rows.every((r) => r.kind !== "command"),
          availability + "/" + phase + " omits command",
        );
        assert.equal(
          await page.$eval("#idea-dlg", (el) => el.open),
          false,
          "unavailable activation opens nothing",
        );
        assert.equal(await page.$eval("#job-picker", (el) => el.open), true);
        selected(s);
        await page.evaluate(() => {
          const trigger = window.savedIdeaTrigger;
          trigger.hidden = false;
          trigger.disabled = false;
          if (!trigger.isConnected) document.getElementById("job-jump").before(trigger);
          delete window.savedIdeaTrigger;
          renderJobResults();
        });
        await page.keyboard.press("Escape");
      }
    }
    console.log("PASS hidden, disabled, absent trigger checks at render and activation");
    await geometry(page);
    console.log("PASS command row geometry");
    await page.browserContext().close();
    const zero = await makePage(1280, false, true);
    await support.focusVisible(zero, "#job-jump");
    await openPicker(zero, "Control");
    assert.equal(selected(await state(zero)).key, commandKey);
    await enter(zero);
    await zero.waitForSelector("#idea-dlg[open]");
    assert.equal(await zero.$eval("#idea-project option", (el) => el.value), "");
    assert.equal(await zero.evaluate(() => document.activeElement.id), "idea-text");
    assert.match(
      await zero.$eval("#idea-project-help", (el) => el.textContent),
      /No registered projects/,
    );
    await zero.browserContext().close();
    console.log("PASS zero-project capture available");
  } finally {
    console.log(
      JSON.stringify({
        attemptedWrites: writes,
        blockedExternal: external,
        successfulExternal: externalSuccess,
        javascriptErrors: errors,
      }),
    );
    await browser.close();
    assert.deepEqual(writes, [], "no attempted browser writes");
    assert.deepEqual(externalSuccess, [], "no successful external requests");
    assert.deepEqual(errors, [], "no browser JavaScript errors");
  }
  console.log("PASS jumper order regression");
}

(async () => {
  const i = process.argv.indexOf("--browser");
  assert.ok(
    i >= 0 && process.argv.length === i + 2,
    "jumper order requires --browser <owned-loopback-origin>",
  );
  await browserRun(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
