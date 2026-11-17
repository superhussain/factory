// An idea whose not_before is still ahead shows a "Not before" chip and drawer line in local time.
// A past stamp shows none, and the 30-second tick drops the chip once the stamp passes.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const AT = "2026-01-02T12:00:00Z";
const idea = (id, extra) => ({
  id,
  project: "sample",
  title: id,
  stage: "idea",
  idea: id,
  updated: AT,
  ...extra,
});
const JOBS = [
  idea("401-sample-hold", { not_before: "2099-01-01T14:00:00Z" }),
  idea("402-sample-past", { not_before: "2020-01-01T00:00:00Z" }),
  idea("403-sample-none", {}),
];
const TITLE = "Pickup leaves this job alone until Jan 1, 9:00 AM (2099-01-01T14:00:00Z).";

test("not before chip", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const errors = [];
    const page = await browser.newPage();
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.emulateTimezone("America/Toronto");
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector('.card[data-id="403-sample-none"]');
    const chips = () =>
      page.$$eval(".card", (cards) =>
        Object.fromEntries(
          cards
            .filter((c) => c.querySelector(".chip.not-before"))
            .map((c) => [c.dataset.id, c.querySelector(".chip.not-before").textContent]),
        ),
      );
    assert.deepEqual(await chips(), { "401-sample-hold": "Not before Jan 1, 9:00 AM" });
    assert.equal(
      await page.$eval('.card[data-id="401-sample-hold"] .chip.not-before', (el) => el.title),
      TITLE,
    );
    await page.evaluate(() => (location.hash = "#401-sample-hold"));
    await page.waitForSelector("#d-not-before:not([hidden])");
    assert.equal(await page.$eval("#d-not-before", (el) => el.textContent), TITLE);
    await page.evaluate(() => (location.hash = "#402-sample-past"));
    await page.waitForFunction(
      () => document.getElementById("d-title").textContent === "402-sample-past",
    );
    assert.equal(await page.$eval("#d-not-before", (el) => el.hidden), true);
    // Once 401's stamp passes, the 30-second tick drops its chip and its open drawer line.
    await page.evaluate(() => (location.hash = "#401-sample-hold"));
    await page.waitForSelector("#d-not-before:not([hidden])");
    await page.evaluate(() => {
      Date.now = () => Date.parse("2099-01-01T14:00:01Z");
      document.activeElement?.blur();
      clockTick();
    });
    assert.deepEqual(await chips(), {});
    assert.equal(await page.$eval("#d-not-before", (el) => el.hidden), true);
    assert.deepEqual(errors, []);
  });
});
