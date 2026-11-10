// Column collapse: stage defaults, empty/terminal collapse, show-all cap, mouse and
// keyboard toggles, accessibility tree, focus/poll/filter persistence, reload reset,
// mobile rows. Browser matrix requires --browser <owned-loopback-origin>.
const assert = require("node:assert/strict");
const { showOnly } = require("../support/project-filter.cjs");

const stages = [
  "backlog",
  "idea",
  "spec",
  "build",
  "review",
  "approve",
  "needs-you",
  "pr-open",
  "merged",
  "closed",
];
// The backlog column lists backlog.md lines, not jobs, and stays empty here.
const fixtures = stages
  .slice(1)
  .flatMap((stage, i) =>
    Array.from({ length: stage === "merged" ? 12 : stage === "closed" ? 2 : 1 }, (_, n) => ({
      id: `${stage}-${n + 1}`,
      project: "sample",
      stage: stage === "needs-you" ? "build" : stage,
      title: `Synthetic ${stage} work ${n + 1}`,
      branch: stage === "idea" ? undefined : `factory/${stage}-${n + 1}`,
      idea: stage === "idea" ? "Synthetic idea\nA small example for the board." : undefined,
      needs: stage === "needs-you" ? "Choose the synthetic option." : undefined,
      round: 1,
      updated: "2026-01-01T12:00:00Z",
      done: `2026-01-${String(n + 1).padStart(2, "0")}T12:00:00Z`,
      pr: ["merged", "closed", "pr-open"].includes(stage)
        ? `https://github.com/example/sample/pull/${i * 20 + n + 1}`
        : undefined,
    })),
  )
  .concat([
    {
      id: "second-idea",
      project: "second",
      stage: "idea",
      title: "Synthetic second project",
      idea: "Synthetic second project",
    },
    {
      id: "unassigned-idea",
      stage: "idea",
      title: "Synthetic unassigned idea",
      idea: "Synthetic unassigned idea",
    },
  ]);
const PROJECTS = [
  [
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n\n# Synthetic sample\n",
  ],
  ["second", "---\nrepo: example/second\nbase: main\ntitle: Synthetic second\n---\n"],
];
const violations = [],
  errors = [];
const toggle = (stage) => `.column-toggle[data-stage="${stage}"]`;

async function makePage(browser, origin, support, width, theme) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await support.controlBoardPolling(page);
  await page.setViewport({ width, height: width === 1280 ? 900 : 844, deviceScaleFactor: 1 });
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setRequestInterception(true);
  page.on("request", async (request) => {
    const target = new URL(request.url());
    if (request.method() !== "GET" || target.origin !== origin) {
      violations.push(request.method() + " " + request.url());
      return request.abort();
    }
    return request.continue();
  });
  await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
  await page.waitForSelector(".card", { polling: 50 });
  await page.waitForFunction(() => !document.getElementById("filter").hidden, { polling: 50 });
  return { page, context };
}

async function state(page, stage) {
  return page.$eval(toggle(stage), (el) => el.getAttribute("aria-expanded"));
}

async function click(page, stage) {
  await page.click(toggle(stage));
}

async function checkAX(page, session, expanded) {
  const name = `${expanded ? "Collapse" : "Expand"} merged column`;
  const { nodes } = await session.send("Accessibility.getFullAXTree");
  const node = nodes.find(
    (n) => !n.ignored && n.role?.value === "button" && n.name?.value === name,
  );
  assert.ok(node, name + " exposed as a button");
  const property = node.properties.find((p) => p.name === "expanded");
  assert.equal(typeof property.value.value, "boolean", "expanded AX value is a boolean");
  assert.equal(property.value.value, expanded);
  console.log(
    JSON.stringify({
      browserAX: {
        role: node.role.value,
        name: node.name.value,
        expanded: property.value.value,
        observedType: property.value.type,
      },
    }),
  );
}

async function defaults(page, width) {
  assert.equal(await state(page, "merged"), "false", "merged starts collapsed");
  assert.equal(await state(page, "closed"), "false", "closed starts collapsed");
  assert.equal(await state(page, "backlog"), "false", "empty backlog starts collapsed");
  for (const stage of stages.slice(1, 8))
    assert.equal(await state(page, stage), "true", stage + " populated default");
  const layout = await page.evaluate(() =>
    [...document.querySelectorAll(".col")].map((col) => {
      const button = col.querySelector(".column-toggle"),
        content = col.querySelector(".cards");
      const r = button.getBoundingClientRect();
      return {
        stage: button.dataset.stage,
        collapsed: col.classList.contains("collapsed-col"),
        hidden: content.hidden,
        display: getComputedStyle(content).display,
        mode: getComputedStyle(button).writingMode,
        width: r.width,
        height: r.height,
        controls: button.getAttribute("aria-controls") === content.id,
        count: button.children[1].textContent,
        action: button.children[2].textContent,
        label: button.getAttribute("aria-label"),
        type: button.type,
      };
    }),
  );
  for (const col of layout) {
    const collapsed = ["backlog", "merged", "closed"].includes(col.stage);
    assert.equal(col.collapsed, collapsed);
    assert.equal(col.hidden, collapsed);
    if (collapsed) assert.equal(col.display, "none");
    assert.equal(col.controls, true);
    assert.equal(col.type, "button");
    assert.equal(col.mode, width > 760 && collapsed ? "vertical-rl" : "horizontal-tb");
    assert.ok(col.width >= 36 && col.height >= 44, "minimum target size");
    assert.equal(col.action, collapsed ? "+" : "−");
    assert.equal(
      col.label,
      `${collapsed ? "Expand" : "Collapse"} ${col.stage === "needs-you" ? "needs you" : col.stage} column`,
    );
  }
  assert.equal(layout.find((c) => c.stage === "merged").count, "12");
  assert.equal(layout.find((c) => c.stage === "closed").count, "2");
  assert.equal(await page.$eval(".col.needs-you .card", (el) => el.dataset.id), "needs-you-1");
  assert.equal(await page.title(), "(1) Factory");
  if (width > 760) {
    const tracks = await page.$eval("#board", (el) =>
      getComputedStyle(el).gridTemplateColumns.split(" ").map(parseFloat),
    );
    assert.deepEqual([tracks[0], ...tracks.slice(-2)], [36, 36, 36]);
    assert.ok(tracks.slice(1, 8).every((n) => n >= 160));
  } else
    assert.ok(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
      "no mobile document overflow",
    );
  assert.equal(await page.$(".col.merged .more"), null);
}

async function matrix(browser, origin, support, width, theme) {
  // Each matrix starts from the complete board; earlier runs leave it mutated.
  await support.seedBoard(origin, structuredClone(fixtures));
  const { page, context } = await makePage(browser, origin, support, width, theme);
  const update = async (jobs) => {
    await support.seedBoard(origin, structuredClone(jobs));
    assert.equal(await page.evaluate(() => poll()), true, "changed fixture poll succeeds");
  };
  try {
    await defaults(page, width);
    const original = await page.evaluate(() => JSON.stringify(jobs));
    const session = await page.createCDPSession();
    await checkAX(page, session, false);
    await click(page, "merged");
    await checkAX(page, session, true);
    const ids = () => page.$$eval(".col.merged .card", (els) => els.map((el) => el.dataset.id));
    assert.deepEqual(
      await ids(),
      Array.from({ length: 10 }, (_, i) => "merged-" + (12 - i)),
      "newest ten in timestamp order",
    );
    assert.equal(await page.$eval(".col.merged .more", (el) => el.textContent), "show all 12");
    await page.click(".col.merged .more");
    assert.equal((await ids()).length, 12);
    assert.equal(await page.evaluate(() => document.activeElement.className), "more");
    await page.evaluate(() => poll());
    assert.equal(
      await page.evaluate(() => document.activeElement.className),
      "more",
      "poll restores more focus",
    );
    await click(page, "merged");
    await click(page, "merged");
    assert.equal((await ids()).length, 12, "pagination retained across collapse");
    await page.click(".col.merged .more");
    assert.equal((await ids()).length, 10, "show 10 cap");
    await update(fixtures.filter((j) => j.id !== "merged-1" && j.id !== "merged-2"));
    assert.equal(
      await page.evaluate(() =>
        document.activeElement.matches('.column-toggle[data-stage="merged"]'),
      ),
      true,
      "missing more falls back to its toggle",
    );
    await update(fixtures);
    await click(page, "closed");
    assert.deepEqual(
      await page.$$eval(".col.closed .card", (els) => els.map((el) => el.dataset.id)),
      ["closed-2", "closed-1"],
    );
    await click(page, "closed");
    await support.focusVisible(page, toggle("closed"));
    await page.keyboard.press("Enter");
    assert.equal(await state(page, "closed"), "true", "Enter opens");
    await page.keyboard.press("Space");
    assert.equal(await state(page, "closed"), "false", "Space closes and keeps focus");
    assert.equal(
      await page.evaluate(() =>
        document.activeElement.matches('.column-toggle[data-stage="closed"]'),
      ),
      true,
    );
    const ring = await page.$eval(toggle("closed"), (el) => {
      const s = getComputedStyle(el);
      return [s.outlineWidth, s.outlineStyle, s.outlineOffset, s.borderRadius];
    });
    assert.deepEqual(ring, ["2px", "solid", "-2px", "4px"], "visible keyboard focus ring");
    await support.focusVisible(page, toggle("merged"));
    await page.keyboard.press("Space");
    await page.keyboard.press("Tab");
    assert.equal(
      await page.evaluate(() =>
        document.activeElement.matches('.column-toggle[data-stage="closed"]'),
      ),
      true,
      "Tab skips hidden merged cards and PR links",
    );
    await page.evaluate(() => poll());
    assert.equal(
      await page.evaluate(() =>
        document.activeElement.matches('.column-toggle[data-stage="closed"]'),
      ),
      true,
      "poll retains toggle focus",
    );
    const hiddenAX = await session.send("Accessibility.getFullAXTree");
    assert.ok(
      !hiddenAX.nodes.some((n) => !n.ignored && n.name?.value.includes("Synthetic merged work")),
      "collapsed cards absent from AX",
    );
    for (const stage of stages) if ((await state(page, stage)) === "true") await click(page, stage);
    assert.equal(
      await page.$eval(
        "#board",
        (el) => el.querySelectorAll('.column-toggle[aria-expanded="false"]').length,
      ),
      10,
    );
    assert.equal(await page.$(".blank"), null, "all collapsed is not empty board");
    if (width > 760)
      assert.deepEqual(
        await page.$eval("#board", (el) =>
          getComputedStyle(el).gridTemplateColumns.split(" ").map(parseFloat),
        ),
        Array(10).fill(36),
      );
    for (const stage of stages) {
      await click(page, stage);
      assert.equal(
        await page.$$eval('.column-toggle[aria-expanded="true"]', (els) => els.length),
        1,
        "independent reopen",
      );
      await click(page, stage);
    }
    assert.equal(
      await page.evaluate(() => JSON.stringify(jobs)),
      original,
      "UI does not mutate jobs",
    );
    await support.focusVisible(page, "#filter");
    for (const value of ["second", ":unassigned", "sample", ""]) {
      await showOnly(page, value);
      assert.equal(
        await page.evaluate(() => document.activeElement.id),
        "filter",
        "filter focus not stolen",
      );
      assert.equal(
        await page.$$eval('.column-toggle[aria-expanded="false"]', (els) => els.length),
        10,
        "choices global across filters",
      );
    }
    await page.evaluate(() => {
      location.hash = "merged-12";
    });
    await page.waitForFunction(
      () =>
        !document.getElementById("detail").hidden &&
        document.getElementById("d-doc").textContent.includes("Local regression fixture."),
      { polling: 50 },
    );
    assert.equal(await state(page, "merged"), "false", "hash access does not expand");
    await support.focusVisible(page, "#d-close");
    await page.evaluate(() => poll());
    assert.equal(
      await page.evaluate(() => document.activeElement.id),
      "d-close",
      "drawer focus not stolen",
    );
    assert.equal(
      await page.evaluate(() => location.hash),
      "#merged-12/spec",
      "poll keeps the canonical drawer hash",
    );
    assert.equal(await page.evaluate(() => JSON.stringify(jobs)), original);
    await page.click("#d-close");
    await page.waitForFunction(() => document.getElementById("detail").hidden, { polling: 50 });
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector(toggle("merged"), { polling: 50 });
    await defaults(page, width);
    const without = fixtures.filter((j) => !["review", "approve", "spec"].includes(j.stage));
    await click(page, "approve");
    await click(page, "approve");
    await click(page, "spec");
    await update(without);
    assert.equal(await state(page, "approve"), "true", "explicit expanded empty stays open");
    assert.equal(await page.$eval(".col.approve .empty", (el) => el.textContent), "none");
    assert.equal(await state(page, "review"), "false", "untouched empty collapses");
    assert.equal(await state(page, "spec"), "false", "explicit collapsed empty");
    await update(
      fixtures.concat({
        id: "merged-13",
        project: "sample",
        stage: "merged",
        title: "Synthetic newest",
        done: "2026-01-13T12:00:00Z",
      }),
    );
    assert.equal(await state(page, "review"), "true", "untouched empty expands on changed poll");
    assert.equal(await state(page, "spec"), "false", "explicit collapse survives new jobs");
    assert.equal(await state(page, "merged"), "false");
    assert.equal(
      await page.$eval(toggle("merged") + " span:nth-child(2)", (el) => el.textContent),
      "13",
      "collapsed count updates",
    );
    await update([]);
    assert.equal(await page.$eval(".blank h2", (el) => el.textContent), "No jobs yet");
    assert.equal(await page.title(), "Factory");
    await update(fixtures);
    assert.equal(await state(page, "spec"), "false", "empty board does not clear choices");
    assert.equal(await state(page, "approve"), "true");
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector(toggle("review"), { polling: 50 });
    await update(without);
    for (const stage of ["review", "approve", "spec"]) {
      assert.equal(await state(page, stage), "false", "fresh empty default");
      await click(page, stage);
      assert.equal(await state(page, stage), "true", "empty can reopen");
      await click(page, stage);
    }
    if (width <= 760) {
      const rows = await page.$$eval(".collapsed-col .column-toggle", (els) =>
        els.map((el) => ({
          mode: getComputedStyle(el).writingMode,
          width: el.getBoundingClientRect().width,
          height: el.getBoundingClientRect().height,
        })),
      );
      assert.ok(
        rows.every((r) => r.mode === "horizontal-tb" && r.width >= 280 && r.height >= 44),
        "mobile header-only rows",
      );
      assert.ok(
        await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
        "no mobile overflow with empty rows",
      );
    }
    console.log(
      JSON.stringify({
        width,
        theme,
        result: "PASS",
        cases:
          "defaults, AX, sorting, pagination, keyboard, focus, all-collapsed, filters, drawer, reload, empty transitions",
      }),
    );
  } finally {
    await context.close();
  }
}

async function browserRun(origin) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(origin);
  const puppeteer = require("puppeteer-core");
  for (const [slug, doc] of PROJECTS) await support.seedProject(origin, slug, doc);
  await support.seedBoard(origin, fixtures);
  const spec = await fetch(origin + "/api/jobs/sample/merged-12/spec.md", {
    method: "PUT",
    headers: { "Content-Type": "text/markdown" },
    body: "# Synthetic work\n\nLocal regression fixture.",
  });
  assert.equal(spec.status, 200, "job spec seed response");
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  try {
    console.log(JSON.stringify({ browser: await browser.version() }));
    for (const width of [1280, 760, 390, 320])
      for (const theme of ["light", "dark"]) await matrix(browser, origin, support, width, theme);
    assert.deepEqual(violations, [], "no attempted non-GET or nonlocal requests");
    assert.deepEqual(errors, [], "no browser JavaScript errors");
    console.log(
      JSON.stringify({
        result: "PASS",
        violations: violations.length,
        browserErrors: errors.length,
      }),
    );
  } finally {
    await browser.close();
  }
}

(async () => {
  const i = process.argv.indexOf("--browser");
  assert.ok(
    i >= 0 && process.argv.length === i + 2,
    "column collapse requires --browser <owned-loopback-origin>",
  );
  await browserRun(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
