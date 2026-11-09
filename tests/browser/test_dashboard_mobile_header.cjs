// Mobile drawer header: all seven fixture variants, long title/metadata/ref wrapping,
// visible buttons/links/close hit area, action group geometry, Tab/Escape/discard
// cancel, across light/dark and 320/360/390/430/1280 widths.
// Requires --browser <owned-loopback-origin>.
const assert = require("node:assert/strict");
const { once } = require("node:events");

const title =
  "Make the drawer title easier to read while keeping idea actions and session links available on a small screen";
const fixtures = [
  { name: "unassigned", stage: "idea", idea: title + "\nSynthetic layout fixture." },
  {
    name: "assigned",
    stage: "idea",
    project: "layout",
    claim: null,
    idea: title + "\nSynthetic layout fixture.",
  },
  { name: "claimed", stage: "idea", project: "layout", claim: "synthetic-agent", idea: title },
  { name: "missing-text", stage: "idea", project: "layout" },
  { name: "number-text", stage: "idea", project: "layout", idea: 42 },
  { name: "non-idea", stage: "spec", project: "layout", idea: title },
  {
    name: "linked-job",
    stage: "pr-open",
    project: "layout",
    idea: title,
    branch: "factory/synthetic-long-branch-name",
    pr: "https://github.com/example/layout/pull/42",
    paseo: { server: "synthetic-server", agent: "synthetic-agent" },
    tracer: { follow: true },
  },
].map((j) => ({
  ...j,
  id: "100-" + j.name + "-" + "x".repeat(48),
  title,
  updated: "",
  refs: [{ kind: "follow-up-of", id: "source-" + "r".repeat(48) }],
}));
const expectedButtons = {
  unassigned: ["d-edit-idea", "d-discard"],
  assigned: ["d-edit-idea", "d-discard", "d-follow-tracer"],
  claimed: ["d-discard", "d-follow-tracer"],
  "missing-text": ["d-discard", "d-follow-tracer"],
  "number-text": ["d-discard", "d-follow-tracer"],
  "non-idea": ["d-follow-tracer"],
  "linked-job": [],
};
const PROJECT_DOC = "---\nrepo: example/layout\nbase: main\ntitle: Synthetic layout project\n---\n";
const SPEC = "# Synthetic layout fixture\n\nNo real job data or remote content.";
const writes = [],
  external = [],
  errors = [];
const selectors = [
  ".detail-head",
  ".detail-head .titles",
  "#d-title",
  "#d-meta",
  "#d-refs",
  ".detail-head .links",
  "#d-pr",
  "#d-paseo",
  "#d-paseo-web",
  "#d-edit-idea",
  "#d-discard",
  "#d-follow-tracer",
  "#d-close",
];

async function makePage(browser, origin, width, theme, support) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await support.controlBoardPolling(page);
  await page.setViewport({ width, height: 844, deviceScaleFactor: 1 });
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setRequestInterception(true);
  page.on("request", async (request) => {
    const target = new URL(request.url());
    if (request.method() !== "GET") {
      writes.push(request.method() + " " + request.url());
      return request.abort();
    }
    if (target.origin !== origin) {
      external.push(request.url());
      return request.abort();
    }
    return request.continue();
  });
  await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
  await page.waitForSelector(".card", { polling: 50 });
  assert.equal(
    await page.evaluate(() => typeof window.fixtureBoardPoll),
    "function",
    "3 s board poll captured",
  );
  return { page, context };
}

async function open(page, fixture) {
  await page.evaluate((id) => {
    location.hash = encodeURIComponent(id);
  }, fixture.id);
  await page.waitForFunction(
    (id) =>
      !document.getElementById("detail").hidden &&
      document.getElementById("d-meta").textContent.includes(id) &&
      !document.getElementById("d-doc").textContent.includes("loading files"),
    { polling: 50 },
    fixture.id,
  );
}

async function geometry(page) {
  return page.evaluate((selectors) => {
    const rect = (el) => el.getBoundingClientRect().toJSON();
    const bounds = Object.fromEntries(selectors.map((s) => [s, rect(document.querySelector(s))]));
    const buttons = ["d-edit-idea", "d-discard", "d-follow-tracer"].filter(
      (id) => document.getElementById(id).getClientRects().length,
    );
    const group = document.querySelector(".idea-actions");
    const close = document.getElementById("d-close");
    const r = rect(close);
    const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    const text = ["d-title", "d-meta", "d-refs"].map((id) => {
      const el = document.getElementById(id);
      const range = document.createRange();
      range.selectNodeContents(el);
      return {
        id,
        bounds: rect(el),
        lines: [...range.getClientRects()].map((r) => r.toJSON()),
        scrollWidth: el.scrollWidth,
        clientWidth: el.clientWidth,
        wrap: getComputedStyle(el).overflowWrap,
      };
    });
    return {
      bounds,
      buttons,
      text,
      closeHit: close === hit || close.contains(hit),
      closeLabel: close.getAttribute("aria-label"),
      viewport: document.documentElement.clientWidth,
      pageWidth: document.documentElement.scrollWidth,
      headWidth: document.querySelector(".detail-head").scrollWidth,
      headClientWidth: document.querySelector(".detail-head").clientWidth,
      titleFlex: (() => {
        const s = getComputedStyle(document.querySelector(".detail-head .titles"));
        return { flexGrow: s.flexGrow, minWidth: s.minWidth };
      })(),
      group: group
        ? {
            display: getComputedStyle(group).display,
            boxes: group.getClientRects().length,
            bounds: rect(group),
          }
        : null,
      links: ["d-pr", "d-paseo", "d-paseo-web"]
        .filter((id) => !document.getElementById(id).hidden)
        .map((id) => ({ id, href: document.getElementById(id).href })),
    };
  }, selectors);
}

function checkGeometry(g, fixture, width, label) {
  assert.deepEqual(g.buttons, expectedButtons[fixture.name], label + " visible buttons");
  assert.equal(g.closeLabel, "Close", label + " close label");
  assert.ok(g.closeHit, label + " close hit target");
  assert.ok(g.pageWidth <= g.viewport, label + " document horizontal overflow");
  assert.ok(g.headWidth <= g.headClientWidth, label + " header horizontal overflow");
  const boxes = [".detail-head .titles", ...g.buttons.map((id) => "#" + id), "#d-close"];
  if (g.links.length) boxes.push(".detail-head .links");
  for (const key of boxes) {
    const r = g.bounds[key];
    assert.ok(
      r.width > 0 &&
        r.height > 0 &&
        r.x >= 0 &&
        r.right <= g.viewport + 1 &&
        r.y >= 0 &&
        r.bottom <= 844,
      `${label} ${key} outside viewport ${JSON.stringify(r)}`,
    );
  }
  for (let i = 0; i < boxes.length; i++) {
    for (let j = i + 1; j < boxes.length; j++) {
      const a = g.bounds[boxes[i]],
        b = g.bounds[boxes[j]];
      assert.ok(
        Math.min(a.right, b.right) - Math.max(a.left, b.left) <= 1 ||
          Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) <= 1,
        `${label} overlapping ${boxes[i]} and ${boxes[j]}`,
      );
    }
  }
  for (const t of g.text) {
    assert.equal(t.wrap, "anywhere", label + " " + t.id + " wrapping");
    assert.ok(t.scrollWidth <= t.clientWidth + 1, label + " " + t.id + " text overflow");
    for (const r of t.lines)
      assert.ok(
        r.left >= t.bounds.left - 1 && r.right <= t.bounds.right + 1,
        `${label} ${t.id} line outside bounds ${JSON.stringify(r)}`,
      );
    if (width < 760) assert.ok(t.lines.length > 1, label + " " + t.id + " long text wraps");
  }
  // The title takes the full remaining header width. On mobile the wrapped title row is
  // titles, the stacked action group (when any button shows), then close, with 12px gaps.
  // On desktop the empty links box also occupies a gap slot, so assert the flex contract
  // instead of gap arithmetic: titles grows and can shrink for wrapping.
  const titles = g.bounds[".detail-head .titles"];
  if (width < 760) {
    const neighbour = g.buttons.length ? g.bounds["#" + g.buttons[0]] : g.bounds["#d-close"];
    assert.ok(
      Math.abs(titles.right + 12 - neighbour.left) <= 1,
      label + " title takes the full remaining header width",
    );
  } else {
    assert.equal(g.titleFlex.flexGrow, "1", label + " desktop title flexes");
    assert.equal(g.titleFlex.minWidth, "0px", label + " desktop title can shrink");
  }
  if (width < 760) {
    for (let i = 1; i < g.buttons.length; i++) {
      const above = g.bounds["#" + g.buttons[i - 1]],
        below = g.bounds["#" + g.buttons[i]];
      assert.equal(above.x, below.x, label + " action left bounds");
      assert.equal(above.width, below.width, label + " action width");
      assert.equal(below.top - above.bottom, 8, label + " stacked action gap");
    }
    assert.equal(g.group.display, g.buttons.length ? "flex" : "none", label + " group display");
    if (!g.buttons.length) assert.equal(g.group.boxes, 0, label + " hidden group box");
    if (g.links.length)
      assert.ok(
        g.bounds[".detail-head .links"].top >=
          Math.max(g.bounds[".detail-head .titles"].bottom, g.bounds["#d-close"].bottom),
        label + " links below title",
      );
  } else {
    assert.equal(g.group.display, "contents", label + " desktop group display");
  }
  assert.deepEqual(
    g.links,
    fixture.name === "linked-job"
      ? [
          { id: "d-pr", href: fixture.pr },
          { id: "d-paseo", href: "session://h/synthetic-server/agent/synthetic-agent" },
          {
            id: "d-paseo-web",
            href: "https://sessions.example.test/synthetic-server/synthetic-agent",
          },
        ]
      : [],
    label + " configured links preserved",
  );
}

async function keyboard(page, fixture, label, support) {
  const buttons = expectedButtons[fixture.name];
  await support.focusVisible(page, "#d-refs a");
  const sequence = [
    ...(fixture.name === "linked-job" ? ["d-pr", "d-paseo", "d-paseo-web"] : []),
    ...buttons,
    "d-close",
  ];
  for (const id of sequence) {
    await page.keyboard.press("Tab");
    assert.equal(
      await page.evaluate(() => document.activeElement.id),
      id,
      label + " Tab reaches " + id,
    );
    if (id === "d-edit-idea") {
      await page.keyboard.press("Enter");
      await page.waitForFunction(
        () =>
          document.getElementById("edit-idea-dlg").open ||
          !document.getElementById("d-edit-idea-err").hidden,
        { polling: 50 },
      );
      assert.equal(
        await page.$eval("#d-edit-idea-err", (el) => (el.hidden ? "" : el.textContent)),
        "",
        label + " editor loads without error",
      );
      assert.equal(
        await page.evaluate(() => document.activeElement.id),
        "edit-idea-text",
        label + " editor focus",
      );
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => !document.getElementById("edit-idea-dlg").open, {
        polling: 50,
      });
      assert.equal(
        await page.evaluate(() => document.activeElement.id),
        id,
        label + " Escape restores focus",
      );
      assert.equal(
        await page.$eval("#detail", (el) => el.hidden),
        false,
        label + " Escape keeps drawer open",
      );
    }
    if (id === "d-discard") {
      const pending = once(page, "dialog", { signal: AbortSignal.timeout(5000) });
      const clicked = page.click("#d-discard");
      const [dialog] = await pending;
      const type = dialog.type(),
        message = dialog.message();
      await dialog.dismiss();
      await clicked;
      assert.equal(type, "confirm", label + " discard confirmation type");
      assert.equal(
        message,
        "Discard idea " + fixture.id + "? It moves to closed.",
        label + " discard confirmation text",
      );
      assert.equal(
        await page.$eval("#detail", (el) => el.hidden),
        false,
        label + " discard cancel keeps drawer",
      );
    }
  }
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.getElementById("detail").hidden, { polling: 50 });
}

async function browserRun(origin) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(origin);
  const puppeteer = require("puppeteer-core");
  await support.seedProject(origin, "layout", PROJECT_DOC);
  await support.seedBoard(origin, fixtures);
  for (const fixture of fixtures) {
    if (fixture.stage === "idea") continue;
    const file = await fetch(origin + `/api/jobs/layout/${fixture.id}/spec.md`, {
      method: "PUT",
      headers: { "Content-Type": "text/markdown" },
      body: SPEC,
    });
    assert.equal(file.status, 200, "spec seed " + fixture.name);
  }
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  let comparisons = 0,
    rerenders = 0,
    switchbacks = 0;
  try {
    console.log(
      JSON.stringify({ browser: await browser.version(), viewportHeight: 844, scale: 1 }),
    );
    for (const width of [320, 360, 390, 430, 1280])
      for (const theme of ["light", "dark"]) {
        const current = await makePage(browser, origin, width, theme, support);
        const rows = [];
        for (const fixture of fixtures) {
          const label = `${width}/${theme}/${fixture.name}`;
          await open(current.page, fixture);
          const g = await geometry(current.page);
          checkGeometry(g, fixture, width, label);
          if (!expectedButtons[fixture.name].length && width < 760) {
            const withoutGroup = await current.page.evaluate(() => {
              const group = document.querySelector(".idea-actions");
              group.remove();
              const width = document
                .querySelector(".detail-head .titles")
                .getBoundingClientRect().width;
              document.getElementById("d-close").before(group);
              return width;
            });
            assert.equal(
              g.bounds[".detail-head .titles"].width,
              withoutGroup,
              label + " no reserved group gap",
            );
          }
          await current.page.evaluate(() => poll());
          assert.deepEqual(
            await geometry(current.page),
            g,
            label + " rerender preserves geometry and state",
          );
          await keyboard(current.page, fixture, label, support);
          await open(current.page, fixture);
          comparisons++;
          rerenders++;
          rows.push({
            state: fixture.name,
            buttons: g.buttons,
            titleWidth: g.bounds[".detail-head .titles"].width,
            result: "PASS",
          });
        }
        await open(current.page, fixtures[0]);
        checkGeometry(
          await geometry(current.page),
          fixtures[0],
          width,
          `${width}/${theme}/switch-back`,
        );
        switchbacks++;
        console.log(JSON.stringify({ width, theme, rows }));
        await current.context.close();
      }
    // Desktop geometry is theme-independent: identical bounds for the same fixture.
    const light = await makePage(browser, origin, 1280, "light", support);
    const dark = await makePage(browser, origin, 1280, "dark", support);
    for (const fixture of fixtures) {
      await open(light.page, fixture);
      await open(dark.page, fixture);
      const a = await geometry(light.page),
        b = await geometry(dark.page);
      assert.deepEqual(a.bounds, b.bounds, fixture.name + " desktop theme-invariant geometry");
    }
    await light.context.close();
    await dark.context.close();
    assert.deepEqual(writes, [], "no attempted writes (all write methods are aborted)");
    assert.deepEqual(external, [], "no external requests (all external requests are aborted)");
    assert.deepEqual(errors, [], "no browser JavaScript errors");
    console.log(
      JSON.stringify({
        result: "PASS",
        comparisons,
        rerenders,
        switchbacks,
        keyboardCases: comparisons,
        writes: writes.length,
        external: external.length,
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
    "mobile header requires --browser <owned-loopback-origin>",
  );
  await browserRun(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
