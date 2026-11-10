// Keyboard arrow navigation between cards/pages and single-key openers: offline
// source contracts plus browser integration on synthetic data. Browser part
// requires --browser <owned-loopback-origin> (fixed 8220/8221, or the fixture's own FACTORY_OWNED_BROWSER_ORIGIN).
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { waitForBoard } = require("../support/board-ready.cjs");

const root = path.resolve(__dirname, "../..");
const html = fs.readFileSync(path.join(root, "web/dashboard.html"), "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
new vm.Script(script, { filename: "dashboard.html" });

// The pre-existing handlers this feature must not touch, byte-for-byte.
const LEGACY_HANDLER = `document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && $('job-picker').open) {
    e.preventDefault();
    cancelJobPicker();
    return;
  }
  if (e.key.toLowerCase() === 'k' && (e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && !e.repeat && !e.isComposing) {
    if (document.querySelector('dialog[open]:not(#job-picker)')) return;
    e.preventDefault();
    openJobPicker();
  }
  if (e.key === 'Escape' && openId && !document.querySelector('dialog[open]')) closeDetail();
});`;
const RESIZE_HANDLER = `handle.addEventListener('keydown', (e) => {
  const w = $('detail').getBoundingClientRect().width;
  if (e.key === 'ArrowLeft') setDrawerWidth(w + 40, true);
  else if (e.key === 'ArrowRight') setDrawerWidth(w - 40, true);
  else if (e.key === 'Home') resetDrawerWidth();
  else return;
  e.preventDefault();
});`;
const JUMPER_HANDLER = `$('job-search').addEventListener('keydown', (e) => {
  if (e.isComposing) return;
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const index = jobResults.findIndex((r) => r.key === selectedKey);
    selectJobResult(Math.max(0, Math.min(jobResults.length - 1, index + (e.key === 'ArrowDown' ? 1 : -1))));
  } else if (e.key === 'Enter') {
    e.preventDefault();
    activateJobResult();
  }
});`;

function offlineSourceChecks() {
  assert.ok(script.includes(LEGACY_HANDLER), "Esc/Cmd+K keydown handler is byte-identical");
  assert.ok(
    script.includes(RESIZE_HANDLER),
    "resize-handle arrow keydown handler is byte-identical",
  );
  assert.ok(
    script.includes(JUMPER_HANDLER),
    "jumper ArrowUp/Down keydown handler is byte-identical",
  );
  assert.ok(script.includes("function visibleCards()"), "visibleCards helper exists");
  assert.ok(
    script.includes("querySelectorAll('#board .cards:not([hidden]) .card')"),
    "visibleCards reads rendered cards only",
  );
  assert.ok(script.includes("function openCard(id)"), "openCard helper exists");
  assert.ok(
    script.includes("location.hash = encodeURIComponent(id)"),
    "openCard mirrors a card click",
  );
  const nav = script.indexOf("function handleBoardKeys(e)");
  const legacy = script.indexOf("document.addEventListener('keydown', (e) => {");
  assert.ok(nav > legacy, "handleBoardKeys is defined after the legacy handler");
  assert.equal(
    script.split("document.addEventListener('keydown', handleBoardKeys);").length - 1,
    1,
    "handleBoardKeys registered once",
  );
  const body = script.slice(
    nav,
    script.indexOf("document.addEventListener('keydown', handleBoardKeys);"),
  );
  for (const guard of [
    "e.metaKey || e.ctrlKey || e.altKey",
    "e.isComposing",
    "e.target.closest('input, textarea, select, [contenteditable], #d-resize')",
    "document.querySelector('dialog[open]')",
    "e.repeat",
    "$('new-idea').hidden",
  ]) {
    assert.ok(body.includes(guard), "handler guard: " + guard);
  }
  assert.ok(script.includes("e.key === 'j' || e.key === '/'"), "/ shares the j opener");
  for (const opener of [
    "openIdeaDialog()",
    "openJobPicker()",
    "openProjectPicker()",
    "location.hash = '#/settings/projects';",
  ]) {
    assert.ok(body.includes(opener), "handler opener: " + opener);
  }
  console.log(
    "PASS offline source checks: helpers, guards, openers and byte-identical legacy handlers",
  );
}

// Synthetic board: three visible cards over three columns (spec/build/review), two
// jobs with project files, no real data anywhere.
function fixtureJobs() {
  return [
    {
      id: "901-arrow-first",
      project: "sample",
      title: "Synthetic first card",
      stage: "spec",
      updated: "2026-10-27T12:00:00Z",
    },
    {
      id: "902-arrow-second",
      project: "sample",
      title: "Synthetic second card",
      stage: "build",
      updated: "2026-10-27T11:00:00Z",
    },
    {
      id: "903-arrow-third",
      project: "sample",
      title: "Synthetic third card",
      stage: "review",
      updated: "2026-10-27T10:00:00Z",
    },
  ];
}

async function browserTest(url) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(url);
  const puppeteer = require("puppeteer-core");

  async function put(route, body, type) {
    const current = await fetch(url + route, { redirect: "error" });
    const headers = current.ok
      ? { "If-Match": current.headers.get("ETag") }
      : { "If-None-Match": "*" };
    const saved = await fetch(url + route, {
      method: "PUT",
      headers: { ...headers, ...(type ? { "Content-Type": type } : {}) },
      body,
    });
    assert.equal(saved.status, 200, "seed " + route);
  }
  await put(
    "/api/projects/sample.md",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    "text/markdown",
  );
  await put(
    "/api/jobs/sample/901-arrow-first/spec.md",
    "# Synthetic spec\n\nCapture fixture, not real data.\n",
    "text/markdown",
  );
  await put(
    "/api/jobs/sample/901-arrow-first/build.md",
    "# Synthetic build\n\nCapture fixture, not real data.\n",
    "text/markdown",
  );
  await put(
    "/api/jobs/sample/902-arrow-second/build.md",
    "# Synthetic build two\n\nCapture fixture, not real data.\n",
    "text/markdown",
  );

  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  try {
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    const pageErrors = [];
    page.on("pageerror", (error) => pageErrors.push(String(error)));
    await page.setRequestInterception(true);
    const external = [];
    page.on("request", (request) =>
      (async () => {
        if (!request.url().startsWith(url + "/")) {
          external.push(request.url());
          await request.abort();
          return;
        }
        await request.continue();
      })().catch(() => {}),
    );

    const seed = async (jobs = fixtureJobs()) => {
      const current = await fetch(url + "/api/board");
      const headers = current.ok
        ? { "If-Match": current.headers.get("ETag") }
        : { "If-None-Match": "*" };
      const saved = await fetch(url + "/api/board", {
        method: "PUT",
        headers,
        body: JSON.stringify({ updated: "2026-10-27T12:00:00Z", jobs }),
      });
      assert.equal(saved.status, 200);
    };
    const load = async (jobs = fixtureJobs()) => {
      await seed(jobs);
      await page.goto(url + "/dashboard.html", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.waitForSelector(".card", { polling: 50 });
      await page.evaluate(() => {
        document.activeElement?.blur?.();
        document.body.focus();
      });
    };
    const hash = () => page.evaluate(() => decodeURIComponent(location.hash));
    const detailHidden = () => page.$eval("#detail", (e) => e.hidden);
    const activeTab = () => page.$eval("#d-tabs .tab.active", (t) => t.dataset.name);
    const dialogOpen = (id) => page.$eval(id, (e) => e.open);
    const press = (key, opts = {}) =>
      page.evaluate(
        (key, opts) => {
          document.activeElement?.blur?.();
          document.body.focus();
          document.body.dispatchEvent(
            new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...opts }),
          );
        },
        key,
        opts,
      );
    // A closed dialog restores focus to its opener; body is not focusable, so focus
    // stays on a control inside the just-closed dialog. Reset to body before each
    // bare keypress that assumes a neutral target.
    const resetFocus = () =>
      page.evaluate(() => {
        document.activeElement?.blur?.();
        document.body.focus();
      });
    const cardIds = () =>
      page.evaluate(() =>
        [...document.querySelectorAll("#board .cards:not([hidden]) .card")].map(
          (c) => c.dataset.id,
        ),
      );

    await load();
    assert.deepEqual(
      await cardIds(),
      ["901-arrow-first", "902-arrow-second", "903-arrow-third"],
      "rendered reading order",
    );
    // Criterion 1: board ArrowRight opens the first visible card.
    await page.keyboard.press("ArrowRight");
    await page.waitForFunction(
      () => openId === "901-arrow-first" && !document.getElementById("detail").hidden,
      { polling: 50 },
    );
    assert.equal(await page.evaluate(() => parseHash().id), "901-arrow-first");
    // Criterion 6 regression part: ArrowRight from the empty board is a quiet no-op.
    await page.evaluate(() => closeDetail());
    await seed([]);
    await page.evaluate(() => poll());
    await page.waitForFunction(() => !document.querySelector("#board .card"), { polling: 50 });
    await page.keyboard.press("ArrowRight");
    assert.equal(await hash(), "");
    assert.equal(await detailHidden(), true);
    await page.keyboard.press("ArrowLeft");
    assert.equal(await detailHidden(), true);
    assert.deepEqual(pageErrors, [], "no page errors on the empty board");
    console.log("PASS board arrows: first visible card opens, empty board is a quiet no-op");

    // Criterion 2: arrows walk the open job's file pages in files order.
    await load();
    await page.keyboard.press("ArrowRight");
    await page.waitForFunction(
      () => openId === "901-arrow-first" && document.querySelector("#d-tabs .tab"),
      { polling: 50 },
    );
    await page.waitForFunction(() => activeFile === "spec.md", { polling: 50 });
    await page.keyboard.press("ArrowRight");
    await page.waitForFunction(() => activeFile === "build.md", { polling: 50 });
    assert.equal(await activeTab(), "build.md");
    assert.equal(await hash(), "#901-arrow-first/build");
    await page.keyboard.press("ArrowLeft");
    await page.waitForFunction(() => activeFile === "spec.md", { polling: 50 });
    assert.equal(await activeTab(), "spec.md");
    console.log("PASS page arrows: next/previous file page with the active tab following");

    // Criterion 3+4: card-to-card movement and the edges.
    await load();
    await page.keyboard.press("ArrowRight"); // open 901
    await page.waitForFunction(
      () => openId === "901-arrow-first" && document.querySelector("#d-tabs .tab"),
      { polling: 50 },
    );
    await page.keyboard.press("ArrowRight"); // 901 spec -> build
    await page.waitForFunction(() => activeFile === "build.md", { polling: 50 });
    await page.keyboard.press("ArrowRight"); // last page -> next card 902
    await page.waitForFunction(() => openId === "902-arrow-second", { polling: 50 });
    await page.waitForFunction(() => document.querySelector("#d-tabs .tab"), { polling: 50 });
    await page.keyboard.press("ArrowLeft"); // 902 first page -> previous card 901
    await page.waitForFunction(() => openId === "901-arrow-first" && activeFile === "spec.md", {
      polling: 50,
    });
    await page.keyboard.press("ArrowLeft"); // first card, first page -> close like Esc
    await page.waitForFunction(() => document.getElementById("detail").hidden, { polling: 50 });
    assert.equal(await hash(), "");
    // Last visible card: ArrowRight is a no-op.
    await page.click('.card[data-id="903-arrow-third"]');
    await page.waitForFunction(
      () => openId === "903-arrow-third" && document.querySelector("#d-tabs .tab") === null,
      { polling: 50 },
    );
    await page.waitForFunction(() => activeFile === null, { polling: 50 });
    const lastHash = await hash();
    await page.keyboard.press("ArrowRight");
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(await hash(), lastHash, "ArrowRight on the last card is a no-op");
    assert.equal(await page.evaluate(() => openId), "903-arrow-third");
    // A card filtered out of the board counts as last for Right (a no-op even
    // while other cards stay visible) and first for Left.
    await page.evaluate(() => {
      jobs.find((j) => j.id === "903-arrow-third").project = "other";
      hiddenProjects = new Set(["other"]);
      renderBoard();
      window.__cardOpens = [];
      const oc = openCard;
      openCard = (id) => {
        window.__cardOpens.push(id);
        return oc(id);
      };
    });
    assert.deepEqual(
      await cardIds(),
      ["901-arrow-first", "902-arrow-second"],
      "open card filtered out while others stay visible",
    );
    assert.equal(
      await page.evaluate(() => openId),
      "903-arrow-third",
      "excluded card is the open card",
    );
    assert.deepEqual(
      await page.evaluate(() => visibleCards()),
      ["901-arrow-first", "902-arrow-second"],
      "another card stays visible to the handler",
    );
    await page.keyboard.press("ArrowRight");
    assert.deepEqual(
      await page.evaluate(() => window.__cardOpens),
      [],
      "unrendered card: Right opens no other card",
    );
    assert.equal(
      await page.evaluate(() => openId),
      "903-arrow-third",
      "unrendered card: Right no-ops",
    );
    await page.keyboard.press("ArrowLeft");
    await page.waitForFunction(() => document.getElementById("detail").hidden, {
      timeout: 3000,
      polling: 50,
    });
    await page.evaluate(() => {
      jobs.find((j) => j.id === "903-arrow-third").project = "sample";
      hiddenProjects = new Set();
      renderBoard();
    });
    console.log(
      "PASS card arrows: next/previous card, close on first card like Esc, last-card no-op",
    );

    // Criterion 5: single keys and their guards.
    await load();
    for (const [key, dialog] of [
      ["n", "#idea-dlg"],
      ["j", "#job-picker"],
      ["/", "#job-picker"],
    ]) {
      await resetFocus();
      await page.keyboard.press(key);
      await page.waitForFunction((id) => document.querySelector(id).open, { polling: 50 }, dialog);
      await page.keyboard.press("Escape");
      await page.waitForFunction((id) => !document.querySelector(id).open, { polling: 50 }, dialog);
    }
    await resetFocus();
    await page.keyboard.press("s");
    await page.waitForFunction(() => location.hash === "#/settings/projects", { polling: 50 });
    await page.click("#settings-back");
    await page.waitForFunction(() => document.getElementById("settings-page").hidden, {
      polling: 50,
    });
    await resetFocus();
    await page.keyboard.press("p");
    await page.waitForFunction(
      () => document.getElementById("job-picker").open && pickerMode === "project",
      { polling: 50 },
    );
    // Existing picker UX (kept): Escape in project mode returns to job mode first.
    await page.keyboard.press("Escape");
    await page.waitForFunction(
      () => document.getElementById("job-picker").open && pickerMode === "job",
      { polling: 50 },
    );
    await page.keyboard.press("Escape");
    await page.waitForFunction(() => !document.getElementById("job-picker").open, { polling: 50 });
    // Guards: modifiers, IME composition, dialogs, editable targets, repeat, hidden New idea.
    for (const opts of [
      { metaKey: true },
      { ctrlKey: true },
      { altKey: true },
      { isComposing: true },
      { repeat: true },
    ]) {
      await press("n", opts);
      await press("j", opts);
      await press("/", opts);
      assert.equal(await dialogOpen("#idea-dlg"), false);
      assert.equal(await dialogOpen("#job-picker"), false);
    }
    await resetFocus();
    await page.keyboard.press("j");
    await page.waitForFunction(() => document.getElementById("job-picker").open, { polling: 50 });
    await press("n", {});
    await press("s", {});
    await press("/", {});
    assert.equal(await dialogOpen("#idea-dlg"), false, "no single keys while a dialog is open");
    assert.equal(await page.evaluate(() => location.hash), "");
    await page.keyboard.press("Escape");
    await page.waitForFunction(() => !document.getElementById("job-picker").open, { polling: 50 });
    await page.evaluate(() => {
      const field = document.createElement("input");
      field.id = "kbnav-synthetic-input";
      document.body.append(field);
      field.focus();
    });
    await page.keyboard.press("n");
    await page.keyboard.press("j");
    await page.keyboard.press("p");
    await page.keyboard.press("s");
    await page.keyboard.press("/");
    assert.equal(await dialogOpen("#idea-dlg"), false, "no keys while typing in an input");
    assert.equal(await dialogOpen("#job-picker"), false);
    assert.equal(await page.evaluate(() => location.hash), "");
    await page.evaluate(() => document.getElementById("kbnav-synthetic-input").remove());
    await page.evaluate(() => {
      document.getElementById("new-idea").hidden = true;
    });
    await page.keyboard.press("n");
    assert.equal(await dialogOpen("#idea-dlg"), false, "n is ignored while New idea is hidden");
    await page.evaluate(() => {
      document.getElementById("new-idea").hidden = false;
    });
    await page.keyboard.press("n");
    await page.waitForFunction(() => document.getElementById("idea-dlg").open, { polling: 50 });
    await page.keyboard.press("Escape");
    console.log(
      "PASS single keys: n/j/p/s openers with modifier, IME, dialog, editable, repeat and hidden-button guards",
    );

    // Criterion 6: the pre-existing keyboard behavior is untouched.
    await load();
    await page.evaluate(() => openJob("901-arrow-first"));
    await page.waitForFunction(
      () => openId === "901-arrow-first" && document.querySelector("#d-tabs .tab"),
      { polling: 50 },
    );
    await page.keyboard.press("Escape");
    await page.waitForFunction(() => document.getElementById("detail").hidden, { polling: 50 });
    await page.keyboard.down("Meta");
    await page.keyboard.press("k");
    await page.keyboard.up("Meta");
    await page.waitForFunction(() => document.getElementById("job-picker").open, { polling: 50 });
    const beforeDown = await page.evaluate(() =>
      [...document.querySelectorAll('#job-results [role="option"]')].findIndex(
        (r) => r.getAttribute("aria-selected") === "true",
      ),
    );
    await page.keyboard.press("ArrowDown");
    const afterDown = await page.evaluate(() =>
      [...document.querySelectorAll('#job-results [role="option"]')].findIndex(
        (r) => r.getAttribute("aria-selected") === "true",
      ),
    );
    assert.equal(afterDown, beforeDown + 1, "jumper ArrowDown still moves the selection");
    await page.keyboard.press("Escape");
    await page.waitForFunction(() => !document.getElementById("job-picker").open, { polling: 50 });
    // The drawer is still hidden from the Esc close above, so the resize handle
    // is not focusable yet; reopen the fixture job first.
    await page.evaluate(() => openJob("901-arrow-first"));
    await page.waitForFunction(
      () => openId === "901-arrow-first" && !document.getElementById("detail").hidden,
      { polling: 50 },
    );
    const widthBefore = await page.$eval("#d-resize", (e) =>
      Number(e.getAttribute("aria-valuenow")),
    );
    await support.focusVisible(page, "#d-resize");
    assert.equal(
      await page.evaluate(() => document.activeElement?.id),
      "d-resize",
      "resize handle is focused",
    );
    await page.keyboard.press("ArrowLeft");
    const widthAfter = await page.$eval("#d-resize", (e) =>
      Number(e.getAttribute("aria-valuenow")),
    );
    assert.equal(widthAfter, widthBefore + 40, "resize handle arrows still resize");
    await page.keyboard.press("ArrowRight");
    assert.equal(
      await page.$eval("#d-resize", (e) => Number(e.getAttribute("aria-valuenow"))),
      widthBefore,
    );
    assert.deepEqual(external, [], "no external requests");
    assert.deepEqual(pageErrors, [], "no page errors");
    console.log(
      "PASS regressions: Esc close, Cmd+K jumper, jumper arrows and resize-handle arrows all intact",
    );
    await context.close();
  } finally {
    await browser.close();
  }
}

(async () => {
  await offlineSourceChecks();
  const i = process.argv.indexOf("--browser");
  if (i >= 0) await browserTest(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
