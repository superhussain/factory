// An idea with a depends-on ref shows "depends on <id>" on its card and in the drawer, linked.
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
const AT = "2026-01-02T12:00:00Z";
const job = (id, stage, extra) => ({
  id,
  project: "sample",
  title: id,
  stage,
  idea: id,
  updated: AT,
  ...extra,
});
const JOBS = [
  job("201-sample-csv-api", "build", {}),
  job("202-sample-csv-button", "idea", {
    refs: [{ id: "201-sample-csv-api", kind: "depends-on" }],
  }),
];
const CARD = '.card[data-id="202-sample-csv-button"]';

test("depends on ref", { timeout: 120000 }, async () => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const errors = [];
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 800 });
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(CARD);
    const wrap = await page.$(`.card-wrap:has(${CARD})`);
    await wrap.screenshot({ path: SHOTS + "depends-on-card-1280.png" });
    assert.equal(
      await page.$eval(`${CARD} .meta.refs`, (el) => el.textContent),
      "depends on 201-sample-csv-api",
    );
    await page.evaluate(() => (location.hash = "#202-sample-csv-button"));
    await page.waitForSelector('#d-refs a[href="#201-sample-csv-api"]');
    assert.deepEqual(errors, []);
  });
});
