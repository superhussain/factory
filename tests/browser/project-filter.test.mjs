// Header project filter: a checkbox menu over a saved set of hidden project keys, its
// label and "all projects" toggle, closing, reload, the legacy single-filter migration and
// the mirrored Display fieldset.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";
import support from "../support/project-filter.cjs";

const { filterState } = support;
const JOBS = [
  { id: "090-alpha", project: "alpha", title: "Synthetic Alpha job", stage: "build" },
  { id: "091-beta", project: "beta", title: "Synthetic Beta job", stage: "spec" },
  { id: "092-loose", title: "Synthetic unassigned job", stage: "idea", idea: "Synthetic idea" },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));
const project = (title) => `---\nrepo: example/sample\nbase: main\ntitle: ${title}\n---\n`;
const row = (key, title, checked, mixed = false) => ({ key, title, checked, mixed });
const wait = { polling: 50 };

test("project filter", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(origin, "alpha", project("Synthetic Alpha"));
    await seedProject(origin, "beta", project("Synthetic Beta"));
    await seedBoard(origin, JOBS);
    const errors = [];
    const open = async () => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 1280, height: 900 });
      page.on("pageerror", (error) => errors.push(String(error)));
      await blockLiveEvents(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await ready(page);
      return page;
    };
    const ready = async (page) => {
      await page.waitForFunction(() => loaded === true, wait);
      await page.waitForSelector("#filter:not([hidden])", wait);
    };
    // One evaluate, so a poll re-render cannot detach the cards between finding and reading them.
    const cards = (page) =>
      page.evaluate(() =>
        [...document.querySelectorAll("#board .card")]
          .filter((el) => el.getClientRects().length)
          .map((el) => el.dataset.id)
          .sort(),
      );
    const menu = (page) =>
      page.evaluate(() => ({
        open: !document.getElementById("filter-menu").hidden,
        expanded: document.getElementById("filter").getAttribute("aria-expanded"),
        focus: document.activeElement.dataset.project ?? document.activeElement.id,
      }));
    const box = (key, root = "#filter-menu") => `${root} [data-project="${key}"]`;
    const page = await open();

    await t.test("F1 default", async () => {
      assert.deepEqual(await filterState(page), {
        label: "all projects",
        rows: [
          row("", "all projects", true),
          row("alpha", "Synthetic Alpha", true),
          row("beta", "Synthetic Beta", true),
          row(":unassigned", "No project yet", true),
        ],
        stored: null,
      });
      assert.equal(
        await page.$eval("#filter", (el) => el.getAttribute("aria-label")),
        "Show jobs from: all projects",
      );
      assert.deepEqual(await menu(page), { open: false, expanded: "false", focus: "" });
      assert.deepEqual(await cards(page), ["090-alpha", "091-beta", "092-loose"]);
    });

    await t.test("F2 uncheck", async () => {
      await page.click("#filter");
      assert.deepEqual(await menu(page), { open: true, expanded: "true", focus: "" });
      await page.click(box("beta"));
      assert.deepEqual(await filterState(page), {
        label: "2 selected",
        rows: [
          row("", "all projects", false, true),
          row("alpha", "Synthetic Alpha", true),
          row("beta", "Synthetic Beta", false),
          row(":unassigned", "No project yet", true),
        ],
        stored: '["beta"]',
      });
      assert.deepEqual(await menu(page), { open: true, expanded: "true", focus: "beta" });
      assert.deepEqual(await cards(page), ["090-alpha", "092-loose"]);
    });

    await t.test("F3 reload", async () => {
      const before = await filterState(page);
      await page.reload({ waitUntil: "domcontentloaded" });
      await ready(page);
      assert.deepEqual(await filterState(page), before);
      assert.equal((await menu(page)).open, false);
      assert.deepEqual(await cards(page), ["090-alpha", "092-loose"]);
    });

    await t.test("F4 toggle", async () => {
      await page.click("#filter");
      await page.click(box("alpha"));
      assert.equal((await filterState(page)).label, "No project yet");
      assert.deepEqual(await cards(page), ["092-loose"]);
      await page.click(box(""));
      assert.deepEqual(await filterState(page), {
        label: "all projects",
        rows: [
          row("", "all projects", true),
          row("alpha", "Synthetic Alpha", true),
          row("beta", "Synthetic Beta", true),
          row(":unassigned", "No project yet", true),
        ],
        stored: null,
      });
      assert.deepEqual(await cards(page), ["090-alpha", "091-beta", "092-loose"]);
      await page.click(box(""));
      assert.deepEqual(await filterState(page), {
        label: "no projects",
        rows: [
          row("", "all projects", false),
          row("alpha", "Synthetic Alpha", false),
          row("beta", "Synthetic Beta", false),
          row(":unassigned", "No project yet", false),
        ],
        stored: '["alpha","beta",":unassigned"]',
      });
      assert.equal(
        await page.$eval("#filter", (el) => el.getAttribute("aria-label")),
        "Show jobs from: no projects",
      );
      assert.deepEqual(await cards(page), []);
      await page.click(box(""));
      assert.equal((await filterState(page)).label, "all projects");
      assert.equal((await menu(page)).open, true);
    });

    await t.test("F5 close", async () => {
      await page.keyboard.press("Escape");
      assert.deepEqual(await menu(page), { open: false, expanded: "false", focus: "filter" });
      await page.keyboard.press("ArrowDown");
      assert.deepEqual(await menu(page), { open: true, expanded: "true", focus: "" });
      await page.click("#updated");
      assert.equal((await menu(page)).open, false);
      await page.click("#filter");
      await page.focus(box(":unassigned"));
      await page.keyboard.press("Tab");
      assert.equal((await menu(page)).open, false);
      assert.deepEqual(await cards(page), ["090-alpha", "091-beta", "092-loose"]);
    });

    await t.test("F6 legacy", async () => {
      const legacy = await open();
      await legacy.evaluate(() => {
        localStorage.clear();
        localStorage.setItem("factory-project", "alpha");
      });
      await legacy.reload({ waitUntil: "domcontentloaded" });
      await ready(legacy);
      assert.equal((await filterState(legacy)).stored, '["beta",":unassigned"]');
      assert.equal((await filterState(legacy)).label, "Synthetic Alpha");
      assert.equal(await legacy.evaluate(() => localStorage.getItem("factory-project")), null);
      assert.deepEqual(await cards(legacy), ["090-alpha"]);
      await legacy.browserContext().close();
    });

    await t.test("F7 display", async () => {
      const display = () =>
        page.$$eval("#display-filter [data-project]", (els) =>
          els.map((el) => ({
            key: el.dataset.project,
            title: el.closest("label").textContent.trim(),
            checked: el.checked,
            mixed: el.indeterminate,
          })),
        );
      await page.evaluate(() => (location.hash = "#/settings/display"));
      await page.waitForSelector("#settings-display:not([hidden])", wait);
      assert.equal(
        await page.$eval("#display-filter legend", (el) => el.textContent),
        "Show jobs from",
      );
      assert.deepEqual(await display(), (await filterState(page)).rows);
      await page.click(box("alpha", "#display-filter"));
      const state = await filterState(page);
      assert.equal(state.label, "2 selected");
      assert.equal(state.stored, '["alpha"]');
      assert.deepEqual(await display(), state.rows);
      assert.equal(
        await page.$eval("#display-status", (el) => el.textContent),
        "Saved in this browser.",
      );
      await page.click("#filter");
      await page.click(box(""));
      assert.equal((await filterState(page)).label, "all projects");
      assert.deepEqual(await display(), (await filterState(page)).rows);
    });

    await t.test("F8 projects load fails", async () => {
      const failed = await open();
      await failed.evaluate(() => {
        localStorage.clear();
        localStorage.setItem("factory-projects-hidden", "beta");
      });
      await failed.setBypassServiceWorker(true);
      await failed.setRequestInterception(true);
      failed.on("request", (request) =>
        new URL(request.url()).pathname.endsWith("/projects.json")
          ? request.abort()
          : request.continue(),
      );
      await failed.reload({ waitUntil: "domcontentloaded" });
      await ready(failed);
      assert.deepEqual(await filterState(failed), {
        label: "all projects",
        rows: [row("", "all projects", true), row(":unassigned", "No project yet", true)],
        stored: "beta",
      });
      assert.deepEqual(await cards(failed), ["090-alpha", "091-beta", "092-loose"]);
      await failed.browserContext().close();
    });

    const old = await open();

    await t.test("F9 comma list", async () => {
      await old.evaluate(() => {
        localStorage.clear();
        localStorage.setItem("factory-projects-hidden", "beta");
      });
      await old.reload({ waitUntil: "domcontentloaded" });
      await ready(old);
      const state = await filterState(old);
      assert.equal(state.stored, '["beta"]');
      assert.equal(state.label, "2 selected");
      assert.deepEqual(await cards(old), ["090-alpha", "092-loose"]);
    });

    await t.test("F10 comma in a key", async () => {
      const keys = await old.evaluate(() => {
        localStorage.setItem("factory-projects-hidden", JSON.stringify(["a,b"]));
        return [...readHiddenProjects()];
      });
      assert.deepEqual(keys, ["a,b"]);
      await old.browserContext().close();
    });

    assert.deepEqual(errors, []);
  });
});
