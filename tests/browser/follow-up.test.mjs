// The drawer's Add follow-up idea item: which jobs show it, the dialog it opens, the board
// write with a follow-up-of ref, and Cancel, against one owned local Worker, at mobile and
// desktop width. Also captures the review images.
// Also covers the read-only state and an empty menu after the job leaves the board.
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
const NEW_ID = "101-export-the-sample";

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
const visibleItems = (page) =>
  page.$$eval("#d-actions-menu [role=menuitem]", (els) =>
    els.filter((el) => !el.hidden).map((el) => el.id),
  );
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
const dialog = (page) =>
  page.evaluate(() => ({
    open: document.getElementById("idea-dlg").open,
    ref: document.getElementById("idea-ref").hidden
      ? null
      : document.getElementById("idea-ref").textContent,
    title: document.getElementById("idea-h").textContent,
    project: document.getElementById("idea-project").value,
    focus: document.activeElement.id,
  }));
const dialogName = async (page) =>
  (await page.accessibility.snapshot({ root: await page.$("#idea-dlg"), interestingOnly: false }))
    ?.name ?? "";

const refA11y = async (page) => ({
  description:
    (
      await page.accessibility.snapshot({
        root: await page.$("#idea-text"),
        interestingOnly: false,
      })
    )?.description ?? "",
  ...(await page.evaluate(() => {
    const code = document.querySelector("#idea-ref code");
    return {
      kids: document.getElementById("idea-ref").childNodes.length,
      code: code ? code.textContent : null,
      font: code ? getComputedStyle(code).fontFamily : null,
    };
  })),
});

test("follow-up", { timeout: 240000 }, async (t) => {
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
      const openFollowUp = async (id) => {
        await openDrawer(page, id);
        await openJobActions(page);
        await activate("#d-follow-up");
        await page.waitForFunction(() => document.getElementById("idea-dlg").open, wait);
      };
      const cancel = async () => {
        await activate("#idea-cancel");
        await page.waitForFunction(() => !document.getElementById("idea-dlg").open, wait);
      };

      await t.test(`Add follow-up idea outside the idea stage ${width}`, async () => {
        const items = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          items[id] = await shown(page, "#d-follow-up");
        }
        assert.deepEqual(items, {
          "100-sample-idea": false,
          "042-sample-export": true,
          "044-closed": true,
          "045-no-project": true,
        });
      });

      await t.test(`Cancel returns focus and drops the ref ${width}`, async () => {
        await openFollowUp("042-sample-export");
        await cancel();
        assert.equal(await page.evaluate(() => document.activeElement.id), "d-actions");
        await activate("#d-close");
        await page.waitForSelector("#detail", { hidden: true });
        await activate("#new-idea");
        await page.waitForFunction(() => document.getElementById("idea-dlg").open, wait);
        const { ref, title } = await dialog(page);
        assert.equal(ref, null);
        assert.equal(title, "New idea");
        assert.equal(await dialogName(page), "New idea");
        const plain = await refA11y(page);
        assert.equal(plain.kids, 0);
        assert.doesNotMatch(plain.description, /Follow-up/);
        await cancel();
      });

      await t.test(`a job with no project opens with no project ${width}`, async () => {
        await openFollowUp("045-no-project");
        assert.deepEqual(await dialog(page), {
          open: true,
          ref: "Follow-up of 045-no-project",
          title: "New follow-up idea",
          project: "",
          focus: "idea-text",
        });
        assert.equal(await dialogName(page), "New follow-up idea");
        await cancel();
      });

      await t.test(`Add saves the follow-up-of ref ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await openJobActions(page);
        if (width === 390)
          await page.screenshot({
            path: `${SHOTS}after-menu-390.png`,
            clip: await headAndMenu(page),
          });
        await activate("#d-follow-up");
        await page.waitForFunction(() => document.getElementById("idea-dlg").open, wait);
        assert.deepEqual(await dialog(page), {
          open: true,
          ref: "Follow-up of 042-sample-export",
          title: "New follow-up idea",
          project: "sample",
          focus: "idea-text",
        });
        assert.equal(await dialogName(page), "New follow-up idea");
        const ref = await refA11y(page);
        assert.match(ref.description, /^Follow-up of 042-sample-export/);
        assert.equal(ref.code, "042-sample-export");
        assert.match(ref.font, /monospace/);
        await page.keyboard.type("Export the sample report as CSV");
        if (width === 390)
          await (await page.$("#idea-dlg")).screenshot({ path: `${SHOTS}after-dialog-390.png` });
        await activate("#idea-add");
        await page.waitForFunction(
          (i) => document.querySelector("#d-meta")?.textContent.includes(i),
          wait,
          NEW_ID,
        );
        const saved = await boardJob(origin, NEW_ID);
        assert.deepEqual(saved.refs, [{ id: "042-sample-export", kind: "follow-up-of" }]);
        assert.equal(saved.project, "sample");
        assert.equal(
          await page.$eval("#d-refs", (el) => el.textContent),
          "follow-up of 042-sample-export",
        );
        if (width === 1280)
          await (
            await page.$(".detail-head")
          ).screenshot({
            path: `${SHOTS}after-new-job-1280.png`,
          });
      });

      await t.test(`Add follow-up idea stays hidden when New idea is hidden ${width}`, async () => {
        await page.$eval("#new-idea", (el) => (el.hidden = true));
        await openDrawer(page, "042-sample-export");
        assert.equal(await shown(page, "#d-follow-up"), false);
        await openDrawer(page, "044-closed");
        await openJobActions(page);
        assert.deepEqual(await visibleItems(page), ["d-copy-link"]);
        await page.keyboard.press("Escape");
        await page.waitForSelector("#d-actions-menu", { hidden: true });
        await page.$eval("#new-idea", (el) => (el.hidden = false));
      });

      await t.test(`An empty job actions menu hides its toggle ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await page.focus("#d-actions");
        await page.keyboard.press("ArrowDown");
        await page.waitForSelector("#d-actions-menu:not([hidden])");
        await seedBoard(
          origin,
          JOBS.filter((j) => j.id !== "042-sample-export"),
        );
        await page.evaluate(() => window.fixtureBoardPoll());
        await page.waitForFunction(() => document.getElementById("d-actions-wrap").hidden, wait);
        if (width === 1280)
          await (
            await page.$(".detail-head")
          ).screenshot({ path: `${SHOTS}after-empty-menu-focus-1280.png` });
        assert.deepEqual(
          {
            ...(await page.evaluate(() => ({
              focus: document.activeElement.id,
              wrap: document.getElementById("d-actions-wrap").hidden,
              menu: document.getElementById("d-actions-menu").hidden,
              expanded: document.getElementById("d-actions").getAttribute("aria-expanded"),
              drawer: !document.getElementById("detail").hidden,
            }))),
            items: await visibleItems(page),
          },
          { focus: "d-close", wrap: true, menu: true, expanded: "false", items: [], drawer: true },
        );
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
