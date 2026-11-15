// Remove from batch on a rider's card and drawer menus while its lead is at spec, against one owned
// local Worker. A dismissed confirm writes nothing; an accepted one sends the rider back to idea
// with no ref. A stacked rider and a rider whose lead is at build show no item. When the lead
// moves to build after the drawer menu opens, the accepted confirm shows the refusal and writes
// nothing. Synthetic running example: lead 108-batch-lead and rider 109-batch-rider in project sample.
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

const NOW = "2026-01-01T12:00:00Z";
const LEAD = {
  id: "108-batch-lead",
  project: "sample",
  title: "108-batch-lead",
  stage: "spec",
  updated: NOW,
};
const RIDER = {
  id: "109-batch-rider",
  project: "sample",
  title: "109-batch-rider",
  stage: "spec",
  refs: [{ id: "108-batch-lead", kind: "batched-into" }],
  note: "batched into 108-batch-lead",
  updated: NOW,
};
const MENU = "#card-actions-menu";
const ITEM = `${MENU} [data-action="d-unbatch"]`;
const CONFIRM =
  "Remove 109-batch-rider from 108-batch-lead's batch? It goes back to idea, and pickup runs it on its own.";
const TITLE =
  "Take this rider out of its lead's batch. It goes back to idea, and pickup runs it on its own.";
const REFUSAL = "This rider can no longer be removed from its batch.";

const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);

test("remove from batch", { timeout: 180000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const dialogs = [];
    let answer = false;
    page.on("dialog", async (d) => {
      dialogs.push(d.message());
      await (answer ? d.accept() : d.dismiss());
    });
    await controlBoardPolling(page);
    const fresh = async (jobs) => {
      await seedBoard(origin, jobs);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="109-batch-rider"]');
    };
    const cardItem = async () => {
      await page.click('.card-actions[data-actions="109-batch-rider"]');
      await page.waitForSelector(`${MENU}:not([hidden])`);
      const shown = await page.$eval(ITEM, (b) => !b.hidden);
      await page.keyboard.press("Escape");
      return shown;
    };

    await fresh([LEAD, RIDER]);
    assert.equal(await cardItem(), true);
    await page.evaluate(() => (location.hash = "#109-batch-rider"));
    await page.waitForFunction(() =>
      document.getElementById("d-meta").textContent.includes("109-batch-rider"),
    );
    await openJobActions(page);
    assert.equal(await page.$eval("#d-unbatch", (b) => !b.hidden), true);
    assert.equal(await page.$eval("#d-unbatch", (b) => b.textContent), "Remove from batch");
    assert.equal(await page.$eval("#d-unbatch", (b) => b.title), TITLE);

    const before = await (await fetch(origin + "/api/board")).text();
    await page.click("#d-unbatch");
    assert.deepEqual(dialogs, [CONFIRM]);
    assert.equal(await (await fetch(origin + "/api/board")).text(), before);

    await page.evaluate(() => (location.hash = ""));
    await page.waitForFunction(() => document.getElementById("detail").hidden);
    answer = true;
    await page.click('.card-actions[data-actions="109-batch-rider"]');
    await page.waitForSelector(`${MENU}:not([hidden])`);
    await page.click(ITEM);
    await page.waitForFunction(() =>
      document.querySelector('section.col.idea .card[data-id="109-batch-rider"]'),
    );
    assert.deepEqual(dialogs, [CONFIRM, CONFIRM]);
    const j = await boardJob(origin, "109-batch-rider");
    assert.deepEqual(
      [j.stage, "refs" in j, j.note],
      ["idea", false, "unbatched: removed from 108-batch-lead on the dashboard"],
    );
    assert.equal((await boardJob(origin, "108-batch-lead")).stage, "spec");

    await fresh([LEAD, { ...RIDER, stacked: true }]);
    assert.equal(await cardItem(), false);
    await fresh([{ ...LEAD, stage: "build" }, RIDER]);
    assert.equal(await cardItem(), false);

    answer = true;
    await fresh([LEAD, RIDER]);
    await page.evaluate(() => (location.hash = "#109-batch-rider"));
    await page.waitForFunction(() =>
      document.getElementById("d-meta").textContent.includes("109-batch-rider"),
    );
    await openJobActions(page);
    assert.equal(await page.$eval("#d-unbatch", (b) => !b.hidden), true);
    await seedBoard(origin, [{ ...LEAD, stage: "build" }, RIDER]);
    const moved = await (await fetch(origin + "/api/board")).text();
    await page.click("#d-unbatch");
    await page.waitForSelector("#d-prioritize-err:not([hidden])");
    assert.equal(await page.$eval("#d-prioritize-err", (p) => p.textContent), REFUSAL);
    assert.equal(dialogs.length, 3);
    assert.equal(await (await fetch(origin + "/api/board")).text(), moved);
    assert.equal((await boardJob(origin, "109-batch-rider")).stage, "spec");
    assert.deepEqual(errors, []);
  });
});
