// The a and . keys on a hovered or focused board card, or with a job drawer open: the jumper
// opens in actions mode with the card menu's items, filters and runs one, and Settings >
// Reference lists the keys. Also captures the Reference section and the actions jumper for review.
// Cmd+K or Ctrl+K in actions mode switches to the normal jumper.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const AT = "2026-01-02T12:00:00Z";
const ID = "042-sample-export";
const IDEA = "045-sample-idea";
const JOBS = [
  {
    id: ID,
    project: "sample",
    title: "Export the sample report as CSV",
    stage: "pr-open",
    round: 1,
    updated: AT,
    pr: "https://github.com/example/sample/pull/46",
  },
  {
    id: IDEA,
    project: "sample",
    title: "Sample idea",
    stage: "idea",
    idea: "Sample idea",
    updated: AT,
  },
];
const CARD = (id) => `.card[data-id="${id}"]`;
const MENU = "#card-actions-menu";
const KEYS = [
  ["J or /", "Open the jumper"],
  ["⌘K or Ctrl+K", "Open the jumper"],
  ["N", "New idea"],
  ["P", "Select project"],
  ["S", "Open Settings"],
  ["A or .", "Open the jumper with actions for the hovered or focused job card, or the open job"],
  ["B", "Open the first backlog item"],
  ["A or D", "Approve or deny the open backlog item"],
  ["Right or Left arrow", "Move between cards and job pages"],
];

async function openPage(browser, origin, hash) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await page.setViewport({ width: 1440, height: 900 });
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await controlBoardPolling(page);
  await page.goto(origin + "/" + hash, { waitUntil: "domcontentloaded" });
  return { context, page, errors };
}

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(origin, JOBS);
}

const picker = (page) =>
  page.evaluate(() => ({
    open: document.getElementById("job-picker").open,
    heading: document.getElementById("job-picker-heading").textContent,
    label: document.getElementById("job-search-label").textContent,
    count: document.getElementById("job-count").textContent,
    options: [...document.querySelectorAll('#job-results [role="option"] .title')].map(
      (el) => el.textContent,
    ),
  }));

const focused = (page, id) =>
  page.evaluate((sel) => document.activeElement === document.querySelector(sel), CARD(id));

const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);

async function openActions(page, id) {
  await page.focus(CARD(id));
  await page.keyboard.press("a");
  await page.waitForFunction(() => document.getElementById("job-picker").open);
}

test("card actions shortcut", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const { context, page, errors } = await openPage(browser, origin, "");
    await page.waitForSelector(CARD(ID));

    await t.test("hover", async () => {
      await page.click(`.card-actions[data-actions="${ID}"]`);
      await page.waitForSelector(`${MENU}:not([hidden])`);
      const labels = await page.$$eval(`${MENU} [data-action]`, (items) =>
        items.filter((b) => !b.hidden).map((b) => b.textContent),
      );
      await page.keyboard.press("Escape");
      await page.evaluate(() => document.activeElement.blur());
      await page.hover(CARD(ID));
      await page.keyboard.press("a");
      await page.waitForFunction(() => document.getElementById("job-picker").open);
      const state = await picker(page);
      assert.equal(state.heading, `Actions for ${ID}`);
      assert.equal(state.label, "Search actions");
      assert.deepEqual(state.options, labels);
      assert.ok(labels.length > 0);
      assert.equal(await page.$eval(MENU, (el) => el.hidden), true);
      const shot = await page.$("#job-picker");
      await shot.screenshot({ path: `${SHOTS}jumper-actions-1440.png` });
    });

    await t.test("escape", async () => {
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => !document.getElementById("job-picker").open);
      assert.equal(await focused(page, ID), true);
    });

    await t.test("period", async () => {
      await page.evaluate(() => document.activeElement.blur());
      await page.hover(CARD(ID));
      await page.keyboard.press(".");
      await page.waitForFunction(() => document.getElementById("job-picker").open);
      assert.equal((await picker(page)).heading, `Actions for ${ID}`);
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => !document.getElementById("job-picker").open);
      assert.equal(await focused(page, ID), true);
    });

    await t.test("focus", async () => {
      await page.hover(CARD(ID));
      await page.focus(CARD(IDEA));
      await page.keyboard.press("a");
      await page.waitForFunction(() => document.getElementById("job-picker").open);
      assert.equal((await picker(page)).heading, `Actions for ${IDEA}`);
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => !document.getElementById("job-picker").open);
      assert.equal(await focused(page, IDEA), true);
    });

    await t.test("switch to jumper", async () => {
      await openActions(page, ID);
      await page.keyboard.type("pau");
      await page.keyboard.down("Control");
      await page.keyboard.press("k");
      await page.keyboard.up("Control");
      const shot = await page.$("#job-picker");
      await shot.screenshot({ path: `${SHOTS}jumper-cmdk-1440.png` });
      const state = await picker(page);
      assert.equal(state.open, true);
      assert.equal(state.heading, "Jump to job");
      assert.equal(state.label, "Search jobs");
      assert.equal(await page.$eval("#job-search", (el) => el.value), "");
      assert.equal(await page.evaluate(() => document.activeElement.id), "job-search");
      assert.ok(state.options.includes("Export the sample report as CSV"));
      assert.ok(state.options.includes("Sample idea"));
      await page.keyboard.down("Meta");
      await page.keyboard.press("k");
      await page.keyboard.up("Meta");
      assert.equal((await picker(page)).heading, "Jump to job");
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => !document.getElementById("job-picker").open);
      assert.equal(await focused(page, ID), true);
    });

    await t.test("filter and run", async () => {
      await openActions(page, ID);
      await page.keyboard.type("pau");
      const state = await picker(page);
      assert.deepEqual(state.options, ["Pause pickup"]);
      assert.equal(state.count, "1 action");
      await page.keyboard.press("Enter");
      await page.waitForFunction(
        (sel) =>
          [...document.querySelectorAll(`${sel} .chip`)].some((c) => c.textContent === "Paused"),
        {},
        CARD(ID),
      );
      assert.equal(typeof (await boardJob(origin, ID)).paused?.at, "string");
      assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
      assert.equal(await page.$eval("#detail", (el) => el.hidden), true);
      assert.equal(await page.evaluate(() => location.hash), "");
    });

    await t.test("dialog", async () => {
      await openActions(page, ID);
      await page.keyboard.type("close job");
      assert.deepEqual((await picker(page)).options, ["Close job"]);
      await page.keyboard.press("Enter");
      await page.waitForFunction(() => document.getElementById("close-job-dlg").open);
      assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
      await page.click("#close-job-cancel");
      await page.waitForFunction(() => !document.getElementById("close-job-dlg").open);
    });

    await t.test("no target", async () => {
      await page.evaluate(() => document.activeElement.blur());
      const point = await page.$eval(".col.spec", (el) => {
        const r = el.getBoundingClientRect();
        return { x: r.left + r.width / 2, y: r.bottom - 8 };
      });
      await page.mouse.move(point.x, point.y);
      assert.equal(
        await page.evaluate(
          ({ x, y }) => !!document.elementFromPoint(x, y).closest(".card-wrap"),
          point,
        ),
        false,
      );
      for (const key of ["a", "."]) {
        await page.keyboard.press(key);
        assert.equal(await page.$eval("#job-picker", (el) => el.open), false);
        assert.equal(await page.evaluate(() => !!document.querySelector("dialog[open]")), false);
      }
    });

    await t.test("drawer", async () => {
      await page.evaluate((id) => (location.hash = "#" + id), ID);
      await page.waitForSelector("#detail:not([hidden])");
      await page.evaluate(() => document.activeElement.blur());
      const point = await page.$eval("#detail", (el) => {
        const r = el.getBoundingClientRect();
        return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
      });
      await page.mouse.move(point.x, point.y);
      for (const key of ["a", "."]) {
        await page.keyboard.press(key);
        await page.waitForFunction(() => document.getElementById("job-picker").open);
        assert.equal((await picker(page)).heading, `Actions for ${ID}`);
        if (key === ".") await page.screenshot({ path: `${SHOTS}jumper-actions-drawer-1440.png` });
        await page.keyboard.press("Escape");
        await page.waitForFunction(() => !document.getElementById("job-picker").open);
        assert.equal(await page.evaluate(() => document.activeElement.id), "d-actions");
        assert.equal(await page.$eval("#detail", (el) => el.hidden), false);
      }
      assert.deepEqual(
        await page.$eval("#d-actions", (el) => [el.title, el.getAttribute("aria-keyshortcuts")]),
        ["Job actions (A or .)", "a ."],
      );
    });

    await t.test("drawer switch to jumper", async () => {
      await page.keyboard.press(".");
      await page.waitForFunction(() => document.getElementById("job-picker").open);
      assert.equal((await picker(page)).heading, `Actions for ${ID}`);
      await page.keyboard.down("Meta");
      await page.keyboard.press("k");
      await page.keyboard.up("Meta");
      assert.equal((await picker(page)).heading, "Jump to job");
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => !document.getElementById("job-picker").open);
      assert.equal(await page.evaluate(() => document.activeElement.id), "d-actions");
      assert.equal(await page.$eval("#detail", (el) => el.hidden), false);
    });

    await t.test("drawer run", async () => {
      assert.equal(typeof (await boardJob(origin, ID)).paused?.at, "string");
      await page.keyboard.press(".");
      await page.waitForFunction(() => document.getElementById("job-picker").open);
      await page.keyboard.type("resume");
      assert.deepEqual((await picker(page)).options, ["Resume pickup"]);
      await page.keyboard.press("Enter");
      await page.waitForFunction(
        (sel) =>
          ![...document.querySelectorAll(`${sel} .chip`)].some((c) => c.textContent === "Paused"),
        {},
        CARD(ID),
      );
      assert.equal((await boardJob(origin, ID)).paused, undefined);
      assert.equal(await page.$eval("#detail", (el) => el.hidden), false);
    });

    await t.test("title", async () => {
      const cards = await page.$$eval("#board .card", (els) =>
        els.map((c) => [c.title, c.getAttribute("aria-keyshortcuts")]),
      );
      assert.equal(cards.length, JOBS.length);
      for (const c of cards) assert.deepEqual(c, ["Press A or . for job actions", "a ."]);
    });

    await t.test("reference", async () => {
      await page.evaluate(() => (location.hash = "#/settings/reference"));
      await page.waitForSelector("#settings-reference:not([hidden])");
      const rows = await page.$eval("#reference-keys", (table) =>
        [...table.tBodies[0].rows].map((tr) => [...tr.cells].map((td) => td.textContent)),
      );
      assert.deepEqual(rows, KEYS);
      assert.equal(
        await page.$eval("#reference-keys", (el) => el.getAttribute("aria-labelledby")),
        "reference-keys-heading",
      );
    });

    await context.close();
    assert.deepEqual(errors, []);
  });
});

test("reference capture", { timeout: 180000 }, async () => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const { context, page, errors } = await openPage(browser, origin, "#/settings/reference");
    await page.waitForSelector("#settings-reference:not([hidden])");
    await page.waitForFunction(() => {
      const first = document.querySelector("#reference-effective tbody td");
      return first && first.textContent !== "Loading projects…";
    });
    const section = await page.$("#settings-reference");
    await section.screenshot({ path: `${SHOTS}reference-keys-1440.png` });
    await context.close();
    assert.deepEqual(errors, []);
  });
});
