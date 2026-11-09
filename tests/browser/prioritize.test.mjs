// The drawer's Prioritize item: which jobs show it, the board write, and a job that left the
// board, against one owned local Worker, at mobile and desktop width. Also captures the review images.
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
    id: "101-sample-urgent",
    project: "sample",
    title: "Sample urgent idea",
    stage: "idea",
    idea: "Sample urgent idea",
    updated: "2026-01-02T00:00:00Z",
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
    paseo: { agent: "synthetic-agent", server: "synthetic-server" },
  },
  { id: "044-open-pr", project: "sample", title: "Open PR", stage: "pr-open", updated: NOW },
  { id: "045-no-project", title: "No project", stage: "build", updated: NOW },
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];

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

test("prioritize", { timeout: 240000 }, async (t) => {
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
      await page.waitForSelector('.card[data-id="042-sample-export"]');
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);

      await t.test(`item only for prioritizable jobs ${width}`, async () => {
        const shown = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          shown[id] = await page.$eval("#d-prioritize", (el) => !el.hidden);
        }
        assert.deepEqual(shown, {
          "100-sample-idea": true,
          "101-sample-urgent": true,
          "042-sample-export": true,
          "044-open-pr": false,
          "045-no-project": false,
        });
      });

      await t.test(`Prioritize writes priority ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await openJobActions(page);
        if (width === 390)
          await page.screenshot({
            path: `${SHOTS}after-menu-390.png`,
            clip: await headAndMenu(page),
          });
        await activate("#d-prioritize");
        await page.waitForFunction(
          () => document.getElementById("d-meta").textContent.endsWith(" · prioritized"),
          wait,
        );
        assert.deepEqual((await boardJob(origin, "042-sample-export")).priority, { at: NOW });
        assert.equal(await page.$eval("#d-prioritize", (el) => el.hidden), true);
        assert.equal(await page.$eval("#d-prioritize-err", (el) => el.hidden), true);
        if (width === 1280)
          await page.screenshot({
            path: `${SHOTS}after-prioritized-1280.png`,
            clip: await headAndMenu(page),
          });
      });

      await t.test(`a job that left the board writes nothing ${width}`, async () => {
        await openDrawer(page, "101-sample-urgent");
        const left = JOBS.filter((j) => j.id !== "101-sample-urgent");
        await seedBoard(origin, left);
        const before = await (await fetch(origin + "/api/board")).text();
        await openJobActions(page);
        await activate("#d-prioritize");
        await page.waitForFunction(() => !document.getElementById("d-prioritize-err").hidden, wait);
        assert.equal(
          await page.$eval("#d-prioritize-err", (el) => el.textContent),
          "This job is no longer on the board.",
        );
        assert.equal(await (await fetch(origin + "/api/board")).text(), before);
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
