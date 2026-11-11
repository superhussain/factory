// The drawer's Prioritize and Remove priority items: which jobs show each, both board writes, and
// a job that left the board, against one owned local Worker, at mobile and desktop width. The board
// card's Prioritized badge shows only after Prioritize and goes after Remove priority. Opening a
// backlog item hides a Prioritize error left from a job. Also captures the review images.
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
  {
    id: "044-open-pr",
    project: "sample",
    title: "Open PR",
    stage: "pr-open",
    updated: NOW,
    priority: { at: "2026-01-01T00:00:00Z" },
  },
  { id: "045-no-project", title: "No project", stage: "build", updated: NOW },
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];

const freezeClock = (page) =>
  page.evaluateOnNewDocument((now) => {
    const RealDate = Date;
    window.now = now;
    window.Date = class extends RealDate {
      constructor(...args) {
        super(...(args.length ? args : [window.now]));
      }
      static now() {
        return new RealDate(window.now).getTime();
      }
    };
  }, NOW);
const openDrawer = async (page, id) => {
  await page.evaluate((i) => (location.hash = "#" + i), id);
  await page.waitForFunction(
    (i) => document.querySelector("#d-meta")?.textContent.includes(i),
    wait,
    id,
  );
};
const badges = (page, id) =>
  page.$$eval(`.card[data-id="${id}"] .id .chip`, (els) => els.map((e) => e.textContent));
const ideaOrder = (page) =>
  page.$$eval("#column-cards-idea .card", (els) => els.map((e) => e.dataset.id));
const closeDrawer = async (page) => {
  await page.click("#d-close");
  await page.waitForSelector("#detail", { hidden: true });
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
    await fetch(origin + "/api/backlog", {
      method: "PUT",
      headers: { "If-None-Match": "*", "Content-Type": "text/markdown" },
      body: "# Backlog\n\n- [ ] sample: Synthetic backlog item\n",
    });
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
      await page.waitForFunction(() => document.querySelector(".backlog-card"), wait);
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);

      await t.test(`item only for prioritizable jobs ${width}`, async () => {
        const shown = {};
        const removable = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          shown[id] = await page.$eval("#d-prioritize", (el) => !el.hidden);
          removable[id] = await page.$eval("#d-remove-priority", (el) => !el.hidden);
        }
        assert.deepEqual(shown, {
          "100-sample-idea": true,
          "101-sample-urgent": true,
          "042-sample-export": true,
          "044-open-pr": false,
          "045-no-project": false,
        });
        assert.deepEqual(removable, {
          "100-sample-idea": false,
          "101-sample-urgent": false,
          "042-sample-export": false,
          "044-open-pr": true,
          "045-no-project": false,
        });
      });

      await t.test(`no badge before Prioritize ${width}`, async () => {
        await closeDrawer(page);
        assert.deepEqual(await badges(page, "042-sample-export"), []);
        if (width === 1280)
          await (
            await page.$('.card[data-id="042-sample-export"]')
          ).screenshot({ path: `${SHOTS}before-card-1280.png` });
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
        await closeDrawer(page);
        await page.waitForFunction(
          () => document.querySelector('.card[data-id="042-sample-export"] .id .chip'),
          wait,
        );
        assert.deepEqual(await badges(page, "042-sample-export"), ["Prioritized"]);
        assert.deepEqual(await badges(page, "100-sample-idea"), []);
        await (
          await page.$('.card[data-id="042-sample-export"]')
        ).screenshot({ path: `${SHOTS}after-card-${width}.png` });
        await openDrawer(page, "042-sample-export");
        assert.equal(await page.$eval("#d-remove-priority", (el) => el.hidden), false);
      });

      await t.test(`Remove priority deletes priority ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await openJobActions(page);
        if (width === 390)
          await page.screenshot({
            path: `${SHOTS}menu-prioritized-390.png`,
            clip: await headAndMenu(page),
          });
        // The drawer redraws when the job's updated stamp changes, so the clock moves on.
        await page.evaluate(() => (window.now = "2026-01-02T12:00:01Z"));
        await activate("#d-remove-priority");
        await page.waitForFunction(
          () => !document.getElementById("d-meta").textContent.endsWith(" · prioritized"),
          wait,
        );
        assert.equal("priority" in (await boardJob(origin, "042-sample-export")), false);
        assert.equal(await page.$eval("#d-prioritize", (el) => el.hidden), false);
        assert.equal(await page.$eval("#d-remove-priority", (el) => el.hidden), true);
        assert.equal(await page.$eval("#d-prioritize-err", (el) => el.hidden), true);
        if (width === 1280)
          await page.screenshot({
            path: `${SHOTS}after-removed-1280.png`,
            clip: await headAndMenu(page),
          });
        await closeDrawer(page);
        await page.waitForFunction(
          () => !document.querySelector('.card[data-id="042-sample-export"] .id .chip'),
          wait,
        );
        assert.deepEqual(await badges(page, "042-sample-export"), []);
      });

      await t.test(`prioritized card leads its column ${width}`, async () => {
        const boardOrder = ["100-sample-idea", "101-sample-urgent"];
        const sameOrder = (order) =>
          page.waitForFunction(
            (o) =>
              [...document.querySelectorAll("#column-cards-idea .card")]
                .map((e) => e.dataset.id)
                .join() === o.join(),
            wait,
            order,
          );
        assert.deepEqual(await ideaOrder(page), boardOrder);
        await page.evaluate(() => (window.now = "2026-01-02T12:00:01Z"));
        await openDrawer(page, "101-sample-urgent");
        await openJobActions(page);
        await activate("#d-prioritize");
        await page.waitForFunction(
          () => document.getElementById("d-meta").textContent.endsWith(" · prioritized"),
          wait,
        );
        await closeDrawer(page);
        await page.waitForFunction(
          () => document.querySelector('.card[data-id="101-sample-urgent"] .id .chip'),
          wait,
        );
        if (width === 1280)
          await (
            await page.$("section.col.idea")
          ).screenshot({ path: `${SHOTS}after-idea-column-1280.png` });
        await sameOrder(["101-sample-urgent", "100-sample-idea"]);
        await page.evaluate(() => (window.now = "2026-01-02T12:00:02Z"));
        await openDrawer(page, "101-sample-urgent");
        await openJobActions(page);
        await activate("#d-remove-priority");
        await page.waitForFunction(
          () => !document.getElementById("d-meta").textContent.endsWith(" · prioritized"),
          wait,
        );
        await closeDrawer(page);
        await sameOrder(boardOrder);
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

      await t.test(`a backlog item hides the Prioritize error ${width}`, async () => {
        assert.equal(await page.$eval("#d-prioritize-err", (el) => el.hidden), false);
        await page.$eval(".backlog-card", (el) => el.click());
        await page.waitForFunction(
          () => document.getElementById("d-title").textContent === "Backlog item",
          wait,
        );
        assert.equal(await page.$eval("#d-prioritize-err", (el) => el.hidden), true);
      });

      await t.test(
        `Remove priority on a job that left the board writes nothing ${width}`,
        async () => {
          await openDrawer(page, "044-open-pr");
          const left = JOBS.filter((j) => j.id !== "101-sample-urgent" && j.id !== "044-open-pr");
          await seedBoard(origin, left);
          const before = await (await fetch(origin + "/api/board")).text();
          await openJobActions(page);
          await activate("#d-remove-priority");
          await page.waitForFunction(
            () => !document.getElementById("d-prioritize-err").hidden,
            wait,
          );
          assert.equal(
            await page.$eval("#d-prioritize-err", (el) => el.textContent),
            "This job is no longer on the board.",
          );
          assert.equal(await (await fetch(origin + "/api/board")).text(), before);
        },
      );
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
