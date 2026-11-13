// Run batch now and Start alone on a held idea's card and drawer menus, against one owned local
// Worker. Each writes priority with its mode, and the other held idea's chip shows what pickup
// will do next. Synthetic running example: other holds 201-wait-a and 202-wait-b.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { openJobActions } from "../support/board-ready.cjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  seedPipeline,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const RECENT = new Date(Date.now() - 3.5 * 3600000).toISOString().replace(/\.\d{3}Z$/, "Z");
const FOLLOW = [{ id: "090-sample-old", kind: "follow-up-of" }];
const idea = (id) => ({
  id,
  project: "other",
  title: id,
  stage: "idea",
  idea: id,
  updated: RECENT,
  refs: FOLLOW,
});
const JOBS = [idea("201-wait-a"), idea("202-wait-b")];
const MENU = "#card-actions-menu";
const WAITS = "Waits for a batch (2 of 3, 3h of 24h)";

const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);

test("held batch actions", { timeout: 180000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "other",
      "---\nrepo: example/other\nbase: main\ntitle: Synthetic other\n---\n",
    );
    await seedPipeline(origin, { schema: 1, hold: { min_ideas: 3, max_wait_hours: 24 } });
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    const chip = (id) =>
      page.$eval(
        `.card[data-id="${id}"]`,
        (c) => c.querySelector(".chip.held")?.textContent ?? null,
      );
    const fresh = async () => {
      await seedBoard(origin, JOBS);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForFunction(() =>
        document
          .querySelector('.card[data-id="201-wait-a"] .chip.held')
          ?.textContent.includes("of 3,"),
      );
    };

    await fresh();
    assert.equal(await chip("201-wait-a"), WAITS);
    await page.click('.card-actions[data-actions="202-wait-b"]');
    await page.waitForSelector(`${MENU}:not([hidden])`);
    const labels = await page.$$eval(`${MENU} [data-action]`, (items) =>
      items.filter((b) => !b.hidden).map((b) => b.textContent),
    );
    assert.deepEqual(labels, [
      "Edit idea",
      "Discard idea",
      "Run batch now",
      "Start alone",
      "Pause pickup",
      "Copy link",
    ]);
    await page.keyboard.press("Escape");
    await page.evaluate(() => (location.hash = "#202-wait-b"));
    await page.waitForFunction(() =>
      document.getElementById("d-meta").textContent.includes("202-wait-b"),
    );
    await openJobActions(page);
    assert.deepEqual(
      await page.$$eval("#d-actions-menu button", (items) =>
        items.filter((b) => !b.hidden).map((b) => b.textContent),
      ),
      labels,
    );
    assert.deepEqual(
      await page.$eval("#d-run-batch", (el) => el.title),
      "Pickup starts this idea next and batches the project's other held ideas behind it",
    );
    assert.deepEqual(
      await page.$eval("#d-start-alone", (el) => el.title),
      "Pickup starts this idea next by itself, with no riders",
    );
    await page.click("#d-run-batch");
    await page.waitForFunction(() =>
      document
        .querySelector('.card[data-id="201-wait-a"] .chip.held')
        ?.textContent.startsWith("Batches"),
    );
    const batch = (await boardJob(origin, "202-wait-b")).priority;
    assert.deepEqual(Object.keys(batch), ["at", "batch"]);
    assert.equal(batch.batch, true);
    assert.equal(await chip("201-wait-a"), "Batches behind 202-wait-b");
    assert.equal(await chip("202-wait-b"), null);
    // A project with a starter offers no second Run batch now.
    await page.evaluate(() => (location.hash = ""));
    await page.waitForFunction(() => document.getElementById("detail").hidden);
    await page.click('.card-actions[data-actions="201-wait-a"]');
    await page.waitForSelector(`${MENU}:not([hidden])`);
    assert.deepEqual(
      await page.$$eval(`${MENU} [data-action]`, (items) =>
        items.filter((b) => !b.hidden).map((b) => b.dataset.action),
      ),
      ["d-edit-idea", "d-discard", "d-start-alone", "d-pause", "d-copy-link"],
    );
    await page.keyboard.press("Escape");

    await fresh();
    await page.click('.card-actions[data-actions="202-wait-b"]');
    await page.waitForSelector(`${MENU}:not([hidden])`);
    await page.click(`${MENU} [data-action="d-start-alone"]`);
    await page.waitForFunction(
      () => !document.querySelector('.card[data-id="202-wait-b"] .chip.held'),
    );
    const alone = (await boardJob(origin, "202-wait-b")).priority;
    assert.deepEqual(Object.keys(alone), ["at", "alone"]);
    assert.equal(alone.alone, true);
    assert.equal(await chip("201-wait-a"), "Waits for a batch (1 of 3, 3h of 24h)");
    assert.deepEqual(errors, []);
  });
});
