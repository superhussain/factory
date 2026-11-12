// A batched rider's card names the lead it ships inside once, at desktop light and mobile dark,
// and a batched rider waiting on you shows the lead's needs ask and one whose lead set no ask names the lead.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const NOW = "2026-01-01T12:00:00Z";
const JOBS = [
  {
    id: "090-sample-export-csv",
    project: "sample",
    title: "Add CSV export to the weekly report",
    stage: "spec",
    branch: "factory/090-sample-export-csv",
    round: 0,
    updated: NOW,
  },
  {
    id: "091-sample-typo",
    project: "sample",
    title: "Fix the Weekley typo in the weekly report header",
    stage: "spec",
    round: 0,
    refs: [{ id: "090-sample-export-csv", kind: "batched-into" }],
    note: "batched into 090-sample-export-csv",
    updated: NOW,
  },
];
const CARD = '.card[data-id="091-sample-typo"]';

test("batched ref label", { timeout: 120000 }, async () => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    for (const [width, height, scheme, suffix] of [
      [1440, 900, "light", "desktop"],
      [390, 844, "dark", "mobile"],
    ]) {
      const page = await browser.newPage();
      await page.setViewport({ width, height });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
      await blockLiveEvents(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(CARD);
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      const clip = await page.evaluate((s) => {
        const { x, y, width, height } = document.querySelector(s).getBoundingClientRect();
        return { x: x + scrollX, y: y + scrollY, width, height };
      }, CARD);
      await page.screenshot({ path: `${SHOTS}batch-ref-${suffix}.png`, clip });
      assert.equal(
        await page.$eval(`${CARD} .refs`, (el) => el.textContent),
        "batched into 090-sample-export-csv",
      );
      assert.equal(await page.$(`${CARD} .note`), null);
      await page.close();
    }
  });
});

const ASK = "Pick the CSV date format in needs.md";
const NEEDS_CARD = '.card.needs-you[data-id="092-sample-csv-name"]';

test("batched rider needs ask", { timeout: 120000 }, async () => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      { ...JOBS[0], stage: "needs-human", needs: ASK },
      {
        id: "092-sample-csv-name",
        project: "sample",
        title: "Name the weekly CSV weekly-<date>.csv",
        stage: "needs-human",
        round: 0,
        refs: [{ id: "090-sample-export-csv", kind: "batched-into" }],
        note: "batched into 090-sample-export-csv",
        needs: ASK,
        updated: NOW,
      },
    ]);
    const page = await browser.newPage();
    await page.setViewport({ width: 1440, height: 900 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(NEEDS_CARD);
    await page.evaluate(
      () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
    );
    const clip = await page.evaluate((s) => {
      const { x, y, width, height } = document.querySelector(s).getBoundingClientRect();
      return { x: x + scrollX, y: y + scrollY, width, height };
    }, NEEDS_CARD);
    await page.screenshot({ path: `${SHOTS}batch-ref-needs-desktop.png`, clip });
    assert.equal(await page.$eval(`${NEEDS_CARD} .note`, (el) => el.textContent), ASK);
    await page.close();
  });
});

const WAIT_CARD = '.card.needs-you[data-id="093-sample-csv-header"]';

test("batched rider waits on lead", { timeout: 120000 }, async () => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      { ...JOBS[0], stage: "needs-human" },
      {
        id: "093-sample-csv-header",
        project: "sample",
        title: "Add a header row to the weekly CSV",
        stage: "needs-human",
        round: 0,
        refs: [{ id: "090-sample-export-csv", kind: "batched-into" }],
        note: "batched into 090-sample-export-csv",
        updated: NOW,
      },
    ]);
    const page = await browser.newPage();
    await page.setViewport({ width: 1440, height: 900 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(WAIT_CARD);
    await page.evaluate(
      () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
    );
    const clip = await page.evaluate((s) => {
      const { x, y, width, height } = document.querySelector(s).getBoundingClientRect();
      return { x: x + scrollX, y: y + scrollY, width, height };
    }, WAIT_CARD);
    await page.screenshot({ path: `${SHOTS}batch-ref-waits-on-lead-desktop.png`, clip });
    assert.equal(
      await page.$eval(`${WAIT_CARD} .note`, (el) => el.textContent),
      "Waiting on its lead",
    );
    const text = await page.$eval(WAIT_CARD, (el) => el.textContent);
    assert.equal(text.split("090-sample-export-csv").length - 1, 1);
    await page.close();
  });
});
