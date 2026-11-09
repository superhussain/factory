// Display settings section against one owned local Worker: theme override, density,
// hidden columns, poll cadence and hidden-tab pause, the mirrored project filter, the
// panel width line, reset and mobile overflow. Every value lives in localStorage.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";

const JOBS = [
  { id: "001-sample", project: "sample", title: "Synthetic sample job", stage: "spec" },
  { id: "002-sample-build", project: "sample", title: "Synthetic build job", stage: "build" },
  { id: "003-sample-merged", project: "sample", title: "Synthetic merged job", stage: "merged" },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));
const KEYS = [
  "factory-theme",
  "factory-density",
  "factory-hidden-columns",
  "factory-poll",
  "factory-project",
  "factory-drawer-width",
];
const LIGHT = "rgb(249, 250, 251)";
const DARK = "rgb(17, 24, 39)";
const wait = { polling: 50 };

test("display settings", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    let boardRequests = 0;
    page.on("request", (request) => {
      if (new URL(request.url()).pathname.endsWith("/board.json")) boardRequests++;
    });
    await page.evaluateOnNewDocument(() => {
      const interval = window.setInterval;
      const clear = window.clearInterval;
      window.pollTimers = new Map();
      window.setInterval = (fn, ms, ...args) => {
        const id = interval(fn, ms, ...args);
        if (fn.name === "poll") window.pollTimers.set(id, ms);
        return id;
      };
      window.clearInterval = (id) => {
        window.pollTimers.delete(id);
        return clear(id);
      };
    });

    const bodyBackground = () =>
      page.evaluate(() => getComputedStyle(document.body).backgroundColor);
    const themeColors = () =>
      page.$$eval('meta[name="theme-color"]', (els) => els.map((el) => [el.id, el.content]));
    const SYSTEM_BARS = [
      ["theme-color-light", "#f9fafb"],
      ["theme-color-dark", "#111827"],
    ];
    const LIGHT_BARS = [
      ["theme-color-light", "#f9fafb"],
      ["theme-color-dark", "#f9fafb"],
    ];
    const DARK_BARS = [
      ["theme-color-light", "#111827"],
      ["theme-color-dark", "#111827"],
    ];
    const liveTimers = () => page.evaluate(() => [...window.pollTimers.values()]);
    const setStorage = (values) =>
      page.evaluate((entries) => {
        for (const [key, value] of Object.entries(entries)) localStorage.setItem(key, value);
      }, values);
    const openDisplay = async () => {
      await page.waitForSelector("#settings-display:not([hidden])", wait);
      await page.waitForFunction(() => window.pollTimers.size > 0, wait);
    };
    const reload = async () => {
      await page.reload({ waitUntil: "domcontentloaded" });
      await openDisplay();
    };
    const toBoard = async () => {
      await page.click("#settings-back");
      await page.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      await page.waitForSelector(".card", wait);
    };
    const toDisplay = async () => {
      await page.evaluate(() => {
        location.hash = "#/settings/display";
      });
      await page.waitForSelector("#settings-display:not([hidden])", wait);
    };

    await page.goto(origin + "/#/settings/display", { waitUntil: "domcontentloaded" });
    await openDisplay();

    await t.test("display V1 route, nav and focus", async () => {
      assert.equal(await page.$eval("#settings-pipeline", (el) => el.hidden), true);
      assert.equal(
        await page.$eval("#settings-nav-display", (el) => el.getAttribute("aria-current")),
        "page",
      );
      assert.equal(
        await page.$eval("#settings-nav-pipeline", (el) => el.getAttribute("aria-current")),
        null,
      );
      await page.waitForFunction(() => document.activeElement?.id === "display-theme", wait);
    });

    await t.test("display V2b theme survives a new window and never enters the URL", async () => {
      await page.select("#display-theme", "dark");
      const second = await context.newPage();
      await second.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      await second.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await second.waitForSelector(".card", wait);
      assert.equal(await second.evaluate(() => document.documentElement.dataset.theme), "dark");
      assert.equal(
        await second.evaluate(() => getComputedStyle(document.body).backgroundColor),
        DARK,
      );
      assert.equal(await second.evaluate(() => localStorage.getItem("factory-theme")), "dark");
      assert.ok(!/theme/i.test(second.url()), second.url());
      assert.ok(!/theme/i.test(page.url()), page.url());
      await second.close();
      await page.select("#display-theme", "system");
    });

    await t.test("display V2 theme follows the system and overrides it", async () => {
      assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), undefined);
      assert.equal(await bodyBackground(), LIGHT);
      assert.deepEqual(await themeColors(), SYSTEM_BARS);
      await page.select("#display-theme", "dark");
      assert.equal(await bodyBackground(), DARK);
      assert.deepEqual(await themeColors(), DARK_BARS);
      await reload();
      assert.equal(await page.$eval("#display-theme", (el) => el.value), "dark");
      assert.equal(await bodyBackground(), DARK);
      assert.deepEqual(await themeColors(), DARK_BARS);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
      await page.select("#display-theme", "light");
      assert.equal(await bodyBackground(), LIGHT);
      assert.deepEqual(await themeColors(), LIGHT_BARS);
      await page.select("#display-theme", "system");
      assert.deepEqual(await themeColors(), SYSTEM_BARS);
      assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), undefined);
      assert.equal(await bodyBackground(), DARK);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      assert.equal(await bodyBackground(), LIGHT);
      await setStorage({ "factory-theme": "neon" });
      await reload();
      assert.equal(await page.$eval("#display-theme", (el) => el.value), "system");
      assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), undefined);
      assert.deepEqual(await themeColors(), SYSTEM_BARS);
      assert.equal(await page.$eval("#display-status", (el) => el.textContent), "");
    });

    await t.test("display V3 compact density tightens cards", async () => {
      await page.select("#display-density", "compact");
      assert.equal(await page.evaluate(() => document.body.classList.contains("compact")), true);
      assert.equal(
        await page.$eval("#display-status", (el) => el.textContent),
        "Saved in this browser.",
      );
      await toBoard();
      assert.equal(await page.$eval(".card", (el) => getComputedStyle(el).padding), "6px 8px");
      await toDisplay();
    });

    await t.test("display V4 hidden columns", async () => {
      await page.click("#display-column-merged");
      assert.equal(
        await page.evaluate(() => localStorage.getItem("factory-hidden-columns")),
        "merged",
      );
      await toBoard();
      assert.equal(await page.$(".col.merged"), null);
      const cols = await page.$eval("#board", (el) => el.style.getPropertyValue("--cols"));
      assert.equal(cols.match(/36px|minmax\(/g).length, 9);
      assert.equal(await page.$$eval(".col", (els) => els.length), 9);
      await toDisplay();
      assert.equal(await page.$eval("#display-column-needs-you", (el) => el.disabled), true);
      await setStorage({ "factory-hidden-columns": "needs-you,bogus,closed" });
      await page.evaluate(() => {
        location.hash = "";
      });
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForSelector(".card", wait);
      assert.deepEqual(await page.$$eval(".col", (els) => els.map((el) => el.classList[1])), [
        "backlog",
        "idea",
        "spec",
        "build",
        "review",
        "approve",
        "needs-you",
        "pr-open",
        "merged",
      ]);
      await toDisplay();
      assert.deepEqual(
        await page.$$eval("#display-columns input", (els) =>
          els.filter((el) => !el.checked).map((el) => el.dataset.stage),
        ),
        ["closed"],
      );
    });

    await t.test("display V5 poll cadence and hidden tab", async () => {
      assert.deepEqual(await liveTimers(), [3000]);
      await page.select("#display-poll", "10");
      assert.deepEqual(await liveTimers(), [10000]);
      const setHidden = (hidden) =>
        page.evaluate((value) => {
          Object.defineProperty(document, "hidden", { configurable: true, get: () => value });
          document.dispatchEvent(new Event("visibilitychange"));
        }, hidden);
      await setHidden(true);
      assert.deepEqual(await liveTimers(), []);
      const before = boardRequests;
      await setHidden(false);
      await page.waitForFunction(() => window.pollTimers.size === 1, wait);
      await new Promise((resolve) => setTimeout(resolve, 500));
      assert.equal(boardRequests - before, 1);
      assert.deepEqual(await liveTimers(), [10000]);
      await setStorage({ "factory-poll": "5" });
      await reload();
      assert.deepEqual(await liveTimers(), [3000]);
      assert.equal(await page.$eval("#display-poll", (el) => el.value), "3");
    });

    await t.test("display V6 project filter mirrors the header", async () => {
      assert.deepEqual(
        await page.$$eval("#display-filter option", (els) => els.map((el) => [el.value, el.text])),
        await page.$$eval("#filter option", (els) => els.map((el) => [el.value, el.text])),
      );
      await page.select("#display-filter", "sample");
      assert.equal(await page.$eval("#filter", (el) => el.value), "sample");
      assert.equal(await page.evaluate(() => localStorage.getItem("factory-project")), "sample");
    });

    await t.test("display V7 panel width line", async () => {
      assert.equal(
        await page.$eval("#display-drawer", (el) => el.textContent),
        "Job panel width: default. Drag the panel edge to change it.",
      );
      await setStorage({ "factory-drawer-width": "500" });
      await reload();
      assert.equal(
        await page.$eval("#display-drawer", (el) => el.textContent),
        "Job panel width: 500 px. Drag the panel edge to change it.",
      );
    });

    await t.test("display V8 reset restores every default", async () => {
      await page.select("#display-theme", "dark");
      await page.select("#display-poll", "30");
      await page.select("#display-density", "compact");
      assert.equal(await page.evaluate(() => localStorage.getItem("factory-poll")), "30");
      await page.click("#display-reset");
      assert.deepEqual(
        await page.evaluate((keys) => keys.map((key) => localStorage.getItem(key)), KEYS),
        KEYS.map(() => null),
      );
      assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), undefined);
      assert.deepEqual(await themeColors(), SYSTEM_BARS);
      assert.equal(await page.evaluate(() => document.body.classList.contains("compact")), false);
      assert.deepEqual(await liveTimers(), [3000]);
      assert.equal(await page.$eval("#filter", (el) => el.value), "");
      assert.deepEqual(
        await page.$$eval("#display-columns input", (els) => els.every((el) => el.checked)),
        true,
      );
      assert.deepEqual(
        await page.$$eval(
          "#display-theme, #display-density, #display-poll, #display-filter",
          (els) => els.map((el) => el.value),
        ),
        ["system", "comfortable", "3", ""],
      );
      assert.equal(
        await page.$eval("#detail", (el) => el.style.getPropertyValue("--drawer-w")),
        "",
      );
      assert.equal(
        await page.$eval("#display-status", (el) => el.textContent),
        "Display settings reset to defaults.",
      );
    });

    await t.test("display V9 mobile layout", async () => {
      await page.setViewport({ width: 390, height: 844 });
      await page.waitForFunction(
        () => document.documentElement.scrollWidth <= window.innerWidth,
        wait,
      );
      await page.setViewport({ width: 1280, height: 900 });
    });

    assert.deepEqual(errors, []);
  });
});
