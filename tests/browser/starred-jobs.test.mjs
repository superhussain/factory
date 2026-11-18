// Starred jobs: the job panel star button, the card chip, the float, only and off views, and the
// Display setting with its reset, against one owned local Worker, at mobile and desktop width.
// STARRED_SHOTS names a folder for captures.
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

const wait = { polling: 50 };
const NOW = "2026-01-02T12:00:00Z";
const build = (id, title, extra = {}) => ({
  id,
  project: "sample",
  title,
  stage: "build",
  round: 1,
  updated: "2026-01-01T00:00:00Z",
  ...extra,
});
const JOBS = [
  build("041-sample-cleanup", "Clean up the sample", { priority: { at: NOW } }),
  build("043-sample-import", "Import the sample"),
  build("042-sample-export", "Export the sample report"),
  {
    id: "046-sample-review",
    project: "sample",
    title: "Review the sample",
    stage: "needs-human",
    needs: "Pick a format.",
    updated: NOW,
  },
  {
    id: "044-closed",
    project: "sample",
    title: "Closed job",
    stage: "closed",
    updated: NOW,
    done: "2026-01-01T00:00:00Z",
  },
  {
    id: "047-closed-newer",
    project: "sample",
    title: "Newer closed job",
    stage: "closed",
    updated: NOW,
    done: NOW,
  },
];
const ARCHIVED = {
  id: "040-sample-archived",
  project: "sample",
  title: "Archived sample",
  stage: "merged",
  updated: "2025-12-01T00:00:00Z",
  done: "2025-12-01T00:00:00Z",
};
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const STAR = "#d-star";
const STARRED = "factory-starred-jobs";
const VIEW = "factory-starred-view";

const openDrawer = async (page, id) => {
  await page.evaluate((i) => (location.hash = "#" + i), id);
  await page.waitForFunction(
    (i) => document.querySelector("#d-meta")?.textContent.includes(i),
    wait,
    id,
  );
};
const showBoard = async (page) => {
  await page.evaluate(() => (location.hash = ""));
  await page.waitForFunction(
    () =>
      document.getElementById("detail").hidden && document.getElementById("settings-page").hidden,
    wait,
  );
};
const openDisplay = async (page) => {
  await page.evaluate(() => (location.hash = "#/settings/display"));
  await page.waitForSelector("#settings-display:not([hidden])");
};
const stored = (page, key) => page.evaluate((k) => localStorage.getItem(k), key);
const pressed = (page) => page.$eval(STAR, (el) => el.getAttribute("aria-pressed"));
const column = (page, stage) =>
  page.$$eval(`#column-cards-${stage} .card`, (cards) => cards.map((c) => c.dataset.id));
const onBoard = (page) => page.$$eval("#board .card", (cards) => cards.map((c) => c.dataset.id));
const firstChip = (page, id) =>
  page.$eval(`.card[data-id="${id}"]`, (el) => el.querySelector(".tags .chip")?.textContent);

test("starred jobs", { timeout: 240000 }, async (t) => {
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
      await controlBoardPolling(page);
      await page.setBypassServiceWorker(true);
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        const url = new URL(request.url());
        if (url.pathname !== "/api/board/archive") return request.continue();
        const found = url.searchParams.get("id") === ARCHIVED.id;
        return request.respond(
          found
            ? {
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({ jobs: [ARCHIVED] }),
              }
            : { status: 404, contentType: "text/plain", body: "not found" },
        );
      });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="042-sample-export"]');
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);

      await t.test(`the star button toggles and saves the star ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        assert.equal(await pressed(page), "false");
        await activate(STAR);
        assert.equal(await pressed(page), "true");
        assert.equal(await stored(page, STARRED), '["042-sample-export"]');
        await activate(STAR);
        assert.equal(await pressed(page), "false");
        assert.equal(await stored(page, STARRED), null);
        await activate(STAR);
        await openDrawer(page, ARCHIVED.id);
        assert.equal(await page.$eval(STAR, (el) => el.hidden), true);
        await openDrawer(page, "043-sample-import");
        assert.equal(await page.$eval(STAR, (el) => el.hidden), false);
        assert.equal(await pressed(page), "false");
        await openDrawer(page, "044-closed");
        await activate(STAR);
        assert.equal(await stored(page, STARRED), '["042-sample-export","044-closed"]');
        await showBoard(page);
      });

      await t.test(`a starred card shows the Starred chip first ${width}`, async () => {
        assert.equal(await firstChip(page, "042-sample-export"), "Starred");
        assert.equal(await firstChip(page, "041-sample-cleanup"), "Prioritized");
        assert.equal(await firstChip(page, "043-sample-import"), undefined);
      });

      await t.test(`float puts starred jobs first and off keeps board order ${width}`, async () => {
        assert.deepEqual(await column(page, "build"), [
          "042-sample-export",
          "041-sample-cleanup",
          "043-sample-import",
        ]);
        assert.deepEqual(await column(page, "closed"), ["044-closed", "047-closed-newer"]);
        await openDisplay(page);
        assert.equal(await page.$eval("#display-starred", (el) => el.value), "float");
        await page.select("#display-starred", "off");
        assert.equal(await stored(page, VIEW), "off");
        assert.equal(
          await page.$eval("#display-status", (el) => el.textContent),
          "Saved in this browser.",
        );
        await showBoard(page);
        assert.deepEqual(await column(page, "build"), [
          "041-sample-cleanup",
          "043-sample-import",
          "042-sample-export",
        ]);
        assert.deepEqual(await column(page, "closed"), ["047-closed-newer", "044-closed"]);
      });

      await t.test(`only shows starred and needs-you jobs ${width}`, async () => {
        await openDisplay(page);
        await page.select("#display-starred", "only");
        await showBoard(page);
        const ids = await onBoard(page);
        assert.ok(ids.includes("042-sample-export"));
        assert.ok(ids.includes("046-sample-review"));
        assert.ok(!ids.includes("041-sample-cleanup"));
        assert.ok(!ids.includes("043-sample-import"));
        assert.equal(
          await page.$eval("#starred-only", (el) => el.textContent),
          "Showing starred jobs only. Show all jobs",
        );
        await activate("#starred-show-all");
        await page.waitForSelector('.card[data-id="041-sample-cleanup"]');
        assert.equal(await stored(page, VIEW), null);
        assert.equal(await page.$("#starred-only"), null);
        assert.equal(
          await page.evaluate(
            () => document.activeElement === document.querySelector(".column-toggle"),
          ),
          true,
        );
      });

      await t.test(`only with no starred job says how to star one ${width}`, async () => {
        await page.evaluate(() => {
          localStorage.removeItem("factory-starred-jobs");
          localStorage.setItem("factory-starred-view", "only");
        });
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForSelector("#starred-only");
        assert.equal(
          await page.$eval("#starred-only", (el) => el.textContent),
          "No starred jobs yet. Star a job with the star button in its job panel. Show all jobs",
        );
        assert.deepEqual(await onBoard(page), ["046-sample-review"]);
      });

      await t.test(
        `only with every starred job filtered out says the filter hides them ${width}`,
        async () => {
          await page.evaluate(() => {
            localStorage.setItem("factory-starred-jobs", '["042-sample-export"]');
            localStorage.setItem("factory-starred-view", "only");
            localStorage.setItem("factory-projects-hidden", '["sample"]');
          });
          await page.reload({ waitUntil: "domcontentloaded" });
          await page.waitForSelector("#starred-only");
          assert.equal(
            await page.$eval("#starred-only", (el) => el.textContent),
            "The project filter hides every starred job. Show all projects Show all jobs",
          );
          if (process.env.STARRED_SHOTS && theme === "light") {
            await mkdir(process.env.STARRED_SHOTS, { recursive: true });
            await page.screenshot({
              path: `${process.env.STARRED_SHOTS}/after-starred-show-projects.png`,
            });
          }
          await activate("#starred-show-projects");
          await page.waitForSelector('.card[data-id="042-sample-export"]');
          assert.equal(await stored(page, "factory-projects-hidden"), null);
          assert.equal(
            await page.evaluate(
              () => document.activeElement === document.querySelector(".column-toggle"),
            ),
            true,
          );
        },
      );

      await t.test(`reset keeps stars and garbage values are ignored ${width}`, async () => {
        await page.evaluate(() =>
          localStorage.setItem("factory-starred-jobs", '["042-sample-export"]'),
        );
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForSelector("#starred-only");
        await openDisplay(page);
        assert.equal(await page.$eval("#display-starred", (el) => el.value), "only");
        await page.click("#display-reset");
        assert.equal(await page.$eval("#display-starred", (el) => el.value), "float");
        assert.equal(await stored(page, VIEW), null);
        assert.equal(await stored(page, STARRED), '["042-sample-export"]');
        await showBoard(page);
        assert.equal(await page.$("#starred-only"), null);
        assert.equal((await column(page, "build"))[0], "042-sample-export");

        await page.evaluate(() => {
          localStorage.setItem("factory-starred-jobs", "{not json");
          localStorage.setItem("factory-starred-view", "sideways");
        });
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForSelector('.card[data-id="042-sample-export"]');
        assert.deepEqual(await column(page, "build"), [
          "041-sample-cleanup",
          "043-sample-import",
          "042-sample-export",
        ]);
        assert.equal(
          await page.$$eval(
            ".tags .chip",
            (c) => c.filter((x) => x.textContent === "Starred").length,
          ),
          0,
        );
        assert.equal(await page.$("#starred-only"), null);
      });

      await t.test(`star tooltip follows the pressed state ${width}`, async () => {
        const label = () =>
          page.$eval(STAR, (el) => [el.getAttribute("title"), el.getAttribute("aria-label")]);
        await openDrawer(page, "043-sample-import");
        assert.deepEqual(await label(), ["Star job", "Star job"]);
        await activate(STAR);
        assert.deepEqual(await label(), ["Unstar job", "Star job"]);
        await activate(STAR);
        assert.deepEqual(await label(), ["Star job", "Star job"]);
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
