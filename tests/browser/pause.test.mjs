// The drawer's Pause pickup and Resume pickup items: which jobs show them, the board writes, the
// card chip, and a job that left the board, against one owned local Worker, at mobile and
// desktop width. Also captures the review images.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { openJobActions } from "../support/board-ready.cjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const wait = { polling: 50 };
const NOW = "2026-01-02T12:00:00Z";
const JOBS = [
  {
    id: "100-sample-idea",
    project: "sample",
    title: "Sample idea",
    stage: "idea",
    idea: "Sample idea",
    updated: "2026-01-01T00:00:00Z",
  },
  {
    id: "042-sample-export",
    project: "sample",
    title: "Export the sample report",
    stage: "build",
    branch: "factory/042-sample-export",
    round: 1,
    updated: "2026-01-01T00:00:00Z",
    claim: { device: "host-a", session: "a1b2c3", since: "2026-01-01T10:00:00Z" },
  },
  { id: "044-closed", project: "sample", title: "Closed job", stage: "closed", updated: NOW },
  { id: "045-no-project", title: "No project", stage: "build", updated: NOW },
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const CARD = '.card[data-id="042-sample-export"]';

const freezeClock = (page) =>
  page.evaluateOnNewDocument(() => {
    const RealDate = Date;
    window.Date = class extends RealDate {
      constructor(...args) {
        super(...(args.length ? args : ["2026-01-02T12:00:00Z"]));
      }
      static now() {
        return new RealDate("2026-01-02T12:00:00Z").getTime();
      }
    };
  });
const openDrawer = async (page, id) => {
  await page.evaluate((i) => (location.hash = "#" + i), id);
  await page.waitForFunction(
    (i) => document.querySelector("#d-meta")?.textContent.includes(i),
    wait,
    id,
  );
};
const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);
const shown = (page, selector) => page.$eval(selector, (el) => !el.hidden);
const chip = (page) => page.$eval(CARD, (el) => el.querySelector(".tags .chip")?.textContent);
// The drawer head plus the open menu, which hangs below it.
const headAndMenu = (page) =>
  page.evaluate(() => {
    const boxes = [".detail-head", "#d-actions-menu:not([hidden])"]
      .map((s) => document.querySelector(s)?.getBoundingClientRect())
      .filter(Boolean);
    const x = Math.min(...boxes.map((b) => b.left));
    const y = Math.min(...boxes.map((b) => b.top));
    return {
      x,
      y,
      width: Math.max(...boxes.map((b) => b.right)) - x,
      height: Math.max(...boxes.map((b) => b.bottom)) - y + 4,
    };
  });

test("pause", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const errors = [];
    for (const { theme, ...viewport } of VIEWPORTS) {
      const { width } = viewport;
      await seedBoard(origin, JOBS);
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport(viewport);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      page.on("pageerror", (error) => errors.push(String(error)));
      await freezeClock(page);
      await controlBoardPolling(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(CARD);
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);

      await t.test(`Pause pickup only for pausable jobs ${width}`, async () => {
        const items = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          items[id] = [await shown(page, "#d-pause"), await shown(page, "#d-resume")];
        }
        assert.deepEqual(items, {
          "100-sample-idea": [true, false],
          "042-sample-export": [true, false],
          "044-closed": [false, false],
          "045-no-project": [false, false],
        });
      });

      await t.test(`Pause pickup writes paused ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await openJobActions(page);
        if (width === 390)
          await page.screenshot({
            path: `${SHOTS}after-menu-390.png`,
            clip: await headAndMenu(page),
          });
        await activate("#d-pause");
        await page.waitForFunction(
          () => document.getElementById("d-meta").textContent.endsWith(" · paused"),
          wait,
        );
        assert.deepEqual((await boardJob(origin, "042-sample-export")).paused, { at: NOW });
        assert.equal(await chip(page), "Paused");
        assert.equal(await shown(page, "#d-pause"), false);
        assert.equal(await shown(page, "#d-resume"), true);
        assert.equal(await shown(page, "#d-pause-err"), false);
        if (width === 1280) {
          await page.click("#d-close");
          await page.waitForSelector("#detail", { hidden: true });
          await (await page.$(CARD)).screenshot({ path: `${SHOTS}after-card-1280.png` });
        }
      });

      await t.test(`Resume pickup deletes paused ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await openJobActions(page);
        await activate("#d-resume");
        await page.waitForFunction(
          () => !document.getElementById("d-meta").textContent.endsWith(" · paused"),
          wait,
        );
        assert.equal("paused" in (await boardJob(origin, "042-sample-export")), false);
        assert.equal(await chip(page), undefined);
        assert.equal(await shown(page, "#d-pause"), true);
        assert.equal(await shown(page, "#d-resume"), false);
      });

      await t.test(`a job that left the board writes nothing ${width}`, async () => {
        await openDrawer(page, "100-sample-idea");
        await seedBoard(
          origin,
          JOBS.filter((j) => j.id !== "100-sample-idea"),
        );
        const before = await (await fetch(origin + "/api/board")).text();
        await openJobActions(page);
        await activate("#d-pause");
        await page.waitForFunction(() => !document.getElementById("d-pause-err").hidden, wait);
        assert.equal(
          await page.$eval("#d-pause-err", (el) => el.textContent),
          "This job is no longer on the board.",
        );
        assert.equal(await (await fetch(origin + "/api/board")).text(), before);
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
