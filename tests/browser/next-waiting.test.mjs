// W opens the needs you jobs in column order, oldest wait first, and wraps. W does nothing in a
// text field, with a dialog open or on Settings, or when no shown job waits, including when the
// filter hides the waiting project.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const IDLE = {
  id: "001-sample",
  project: "sample",
  title: "Synthetic sample job",
  stage: "spec",
  updated: "2026-01-01T08:00:00Z",
};
const JOBS = [
  IDLE,
  {
    id: "1903-sample-c",
    project: "sample",
    title: "Synthetic sample C",
    stage: "spec",
    needs: "Synthetic question",
    updated: "2026-01-01T11:00:00Z",
  },
  {
    id: "1901-sample-a",
    project: "sample",
    title: "Synthetic sample A",
    stage: "needs-human",
    updated: "2026-01-01T09:00:00Z",
  },
  {
    id: "1902-sample-b",
    project: "sample",
    title: "Synthetic sample B",
    stage: "pr-open",
    escalated: true,
    updated: "2026-01-01T10:00:00Z",
  },
];
const wait = { polling: 50 };

test("w key opens the next waiting job", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#board .card.needs-you", wait);
    const hash = () => page.evaluate(() => location.hash);
    const pressW = async (expected) => {
      await page.keyboard.press("w");
      await page.waitForFunction((h) => location.hash === h, wait, expected);
    };

    await pressW("#1901-sample-a");
    await pressW("#1902-sample-b");
    await pressW("#1903-sample-c");
    await pressW("#1901-sample-a");

    // Two presses in one task land before hashchange runs, so the second reads the hash, not the drawer.
    await page.evaluate(() => (location.hash = ""));
    await page.waitForSelector("#detail[hidden]", wait);
    const quick = await page.evaluate(() => {
      for (let i = 0; i < 2; i++) {
        document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "w", bubbles: true }));
      }
      return location.hash;
    });
    assert.equal(quick, "#1902-sample-b");

    // From a job that does not wait, W opens the first waiting job, then the next.
    await page.evaluate(() => (location.hash = "#001-sample"));
    await page.waitForSelector("#detail:not([hidden])", wait);
    await pressW("#1901-sample-a");
    await pressW("#1902-sample-b");

    await page.evaluate(() => {
      const field = document.createElement("input");
      field.id = "next-waiting-field";
      document.getElementById("detail").append(field);
    });
    await page.focus("#next-waiting-field");
    await page.keyboard.press("w");
    assert.equal(await hash(), "#1902-sample-b");
    await page.$eval("#next-waiting-field", (el) => el.remove());

    await page.$eval("#idea-dlg", (el) => el.showModal());
    await page.keyboard.press("w");
    assert.equal(await hash(), "#1902-sample-b");
    await page.$eval("#idea-dlg", (el) => el.close());

    await page.evaluate(() => (location.hash = "#/settings/projects"));
    await page.waitForSelector("#settings-projects:not([hidden])", wait);
    await page.keyboard.press("w");
    assert.equal(await hash(), "#/settings/projects");

    await page.evaluate(() => (location.hash = ""));
    await page.waitForSelector("#settings-page[hidden]", wait);
    await page.click("#filter");
    await page.click('#filter-menu [data-project="sample"]');
    await page.waitForSelector("#board .card.needs-you", { hidden: true, ...wait });
    await page.keyboard.press("w");
    assert.equal(await hash(), "");
    await page.click('#filter-menu [data-project=""]');
    await page.waitForSelector("#board .card.needs-you", wait);

    await seedBoard(origin, [IDLE]);
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForSelector('#board .card[data-id="001-sample"]', wait);
    await page.keyboard.press("w");
    assert.equal(await hash(), "");
    assert.deepEqual(errors, []);
  });
});
