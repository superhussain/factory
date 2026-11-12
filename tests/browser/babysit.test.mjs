// The drawer's Babysit PR button and menu item: which jobs show them, the board write after the
// confirm, a dismissed confirm, and a job that left the board, against one owned local Worker at
// mobile and desktop width. The written card leaves needs you for pr-open with a Babysitting chip.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { openJobActions } from "../support/board-ready.cjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";

const wait = { polling: 50 };
const NOW = "2026-01-02T12:00:00Z";
const AT = { at: "2026-01-01T00:00:00Z" };
const PR = (n) => `https://github.com/example/sample/pull/${n}`;
const NEEDS = "review and merge PR #44: this project's pipeline holds every PR for a person";
const prOpen = (id, n, extra = {}) => ({
  id,
  project: "sample",
  title: "Synthetic " + id,
  stage: "pr-open",
  branch: "factory/" + id,
  round: 1,
  pr: PR(n),
  updated: "2026-01-01T00:00:00Z",
  ...extra,
});
const JOBS = [
  prOpen("042-sample-export", 44, {
    title: "Export the sample report",
    needs: NEEDS,
    escalated: true,
  }),
  prOpen("043-sample-filter", 43, { priority: AT }),
  prOpen("046-sample-human", 46, { stage: "needs-human" }),
  prOpen("047-sample-no-pr", 47, { pr: undefined }),
  prOpen("048-sample-paused", 48, { paused: AT }),
  prOpen("049-sample-babysat", 49, { babysit: AT }),
  prOpen("050-sample-rider", 43, { refs: [{ id: "043-sample-filter", kind: "batched-into" }] }),
  prOpen("051-sample-closing", 51, { close: { reason: "not needed", at: AT.at } }),
  prOpen("052-sample-other-repo", 52, { pr: "https://github.com/example/other/pull/52" }),
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const CONFIRM =
  "Babysit PR #44 of 042-sample-export? An agent fixes review comments, CI failures and conflicts, then merges the PR once GitHub shows an approval.";

const freezeClock = (page) =>
  page.evaluateOnNewDocument((now) => {
    const RealDate = Date;
    window.Date = class extends RealDate {
      constructor(...args) {
        super(...(args.length ? args : [now]));
      }
      static now() {
        return new RealDate(now).getTime();
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
const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);
const shown = (page, sel) => page.$eval(sel, (el) => !el.hidden);
const columnOf = (page, id) =>
  page.$$eval(`.card[data-id="${id}"]`, (els) =>
    els.map((e) => e.closest("section.col").className.split(" ")[1]),
  );
const chipsOf = (page, id) =>
  page.$$eval(`.card[data-id="${id}"] .id .chip`, (els) => els.map((e) => e.textContent));

test("babysit", { timeout: 240000 }, async (t) => {
  await prepareAssets();
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
      const dialogs = [];
      let answer = true;
      page.on("dialog", async (d) => {
        dialogs.push(d.message());
        await (answer ? d.accept() : d.dismiss());
      });
      await freezeClock(page);
      await controlBoardPolling(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="042-sample-export"]');
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);

      await t.test(`button and item only when canBabysit ${width}`, async () => {
        const bar = {};
        const item = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          bar[id] = await shown(page, "#d-babysit-bar");
          item[id] = await shown(page, "#d-babysit");
        }
        const expected = Object.fromEntries(
          JOBS.map((j) => [j.id, ["042-sample-export", "043-sample-filter"].includes(j.id)]),
        );
        assert.deepEqual(bar, expected);
        assert.deepEqual(item, expected);
        assert.equal(
          await page.$eval("#d-babysit-bar", (el) => el.title),
          "Fix review comments and CI, then merge this PR",
        );
        assert.deepEqual(await chipsOf(page, "049-sample-babysat"), ["Babysitting"]);
      });

      await t.test(`a dismissed confirm writes nothing ${width}`, async () => {
        const before = await (await fetch(origin + "/api/board")).text();
        await openDrawer(page, "042-sample-export");
        answer = false;
        await activate("#d-babysit-bar");
        answer = true;
        assert.deepEqual(dialogs, [CONFIRM]);
        assert.equal(await (await fetch(origin + "/api/board")).text(), before);
      });

      await t.test(`Babysit PR writes the board ${width}`, async () => {
        assert.deepEqual(await columnOf(page, "042-sample-export"), ["needs-you"]);
        await activate("#d-babysit-bar");
        await page.waitForFunction(() => document.getElementById("d-babysit-bar").hidden, wait);
        const j = await boardJob(origin, "042-sample-export");
        assert.deepEqual(
          [j.babysit, j.priority, j.note, "needs" in j, "escalated" in j],
          [{ at: NOW }, { at: NOW }, "babysitting PR #44", false, false],
        );
        assert.equal(await page.$eval("#d-babysit-err", (el) => el.hidden), true);
        await page.waitForFunction(
          () => !document.querySelector('section.col.needs-you .card[data-id="042-sample-export"]'),
          wait,
        );
        assert.deepEqual(await columnOf(page, "042-sample-export"), ["pr-open"]);
        assert.deepEqual(await chipsOf(page, "042-sample-export"), ["Prioritized", "Babysitting"]);
      });

      await t.test(`the menu item keeps an earlier priority ${width}`, async () => {
        await openDrawer(page, "043-sample-filter");
        await openJobActions(page);
        await activate("#d-babysit");
        await page.waitForFunction(() => document.getElementById("d-babysit").hidden, wait);
        const j = await boardJob(origin, "043-sample-filter");
        assert.deepEqual([j.babysit, j.priority, j.note], [{ at: NOW }, AT, "babysitting PR #43"]);
      });

      await t.test(`a job that left the board writes nothing ${width}`, async () => {
        await seedBoard(origin, JOBS);
        await page.evaluate(() => window.fixtureBoardPoll());
        await openDrawer(page, "042-sample-export");
        await page.waitForFunction(() => !document.getElementById("d-babysit-bar").hidden, wait);
        await seedBoard(
          origin,
          JOBS.filter((j) => j.id !== "042-sample-export"),
        );
        const before = await (await fetch(origin + "/api/board")).text();
        await activate("#d-babysit-bar");
        await page.waitForFunction(() => !document.getElementById("d-babysit-err").hidden, wait);
        assert.equal(
          await page.$eval("#d-babysit-err", (el) => el.textContent),
          "This job is no longer on the board.",
        );
        assert.equal(await (await fetch(origin + "/api/board")).text(), before);
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
