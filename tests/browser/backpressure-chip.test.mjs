// While the jobs waiting on you, in every project, reach the idea's project limit, a plain idea
// shows pickup's backpressure pause. A prioritized or claimed idea shows no chip, and the paused
// chip replaces a held idea's batch chip.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  seedPipeline,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const AT = "2026-01-02T12:00:00Z";
const job = (id, extra) => ({
  id,
  project: "sample",
  title: id,
  stage: "idea",
  idea: id,
  updated: AT,
  ...extra,
});
const JOBS = [
  ...[1, 2, 3, 4, 5].map((n) =>
    job(`20${n}-waiting`, { project: "other", stage: "needs-human", idea: undefined }),
  ),
  job("120-add-csv-export", {}),
  job("121-rushed", { priority: { at: AT } }),
  job("122-follow-up", { refs: [{ id: "120-add-csv-export", kind: "follow-up-of" }] }),
  job("123-claimed", { claim: { device: "host-a", session: "s1", since: AT } }),
];
const project = (title) => `---\nrepo: example/sample\nbase: main\ntitle: ${title}\n---\n`;
const TITLE =
  "Pickup starts no new idea while 5 jobs wait on you. It pauses at 5. Approved specs, moved jobs and prioritized ideas still start. Settings, Pipeline, Review backpressure sets the number.";

async function openBoard(browser, origin, pipeline) {
  await seedProject(origin, "sample", project("Synthetic sample"));
  await seedProject(origin, "other", project("Synthetic other"));
  await seedPipeline(origin, pipeline);
  await seedBoard(origin, JOBS);
  const errors = [];
  const page = await browser.newPage();
  page.on("pageerror", (error) => errors.push(String(error)));
  await blockLiveEvents(page);
  await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
  return { page, errors };
}

const pausedChips = (page) =>
  page.$$eval(".card", (cards) =>
    Object.fromEntries(
      cards
        .filter((c) => c.querySelector(".chip.backpressure"))
        .map((c) => [c.dataset.id, c.querySelector(".chip.backpressure").textContent]),
    ),
  );

test("backpressure chip shows", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    const { page, errors } = await openBoard(browser, origin, { schema: 1 });
    await page.waitForSelector('.card[data-id="120-add-csv-export"] .chip.backpressure');
    assert.deepEqual(await pausedChips(page), {
      "120-add-csv-export": "Paused: 5 waiting on you",
      "122-follow-up": "Paused: 5 waiting on you",
    });
    assert.equal(await page.$('.card[data-id="122-follow-up"] .chip.held'), null);
    assert.equal(
      await page.$eval('.card[data-id="120-add-csv-export"] .chip.backpressure', (el) => el.title),
      TITLE,
    );
    await page.evaluate(() => {
      location.hash = "#120-add-csv-export";
    });
    await page.waitForSelector("#d-backpressure:not([hidden])");
    assert.equal(await page.$eval("#d-backpressure", (el) => el.textContent), TITLE);
    assert.equal(await page.$eval("#d-held", (el) => el.hidden), true);
    assert.deepEqual(errors, []);
  });
});

test("backpressure chip respects the limit", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    const { page, errors } = await openBoard(browser, origin, {
      schema: 1,
      backpressure: { needs_you: 6 },
    });
    await page.waitForFunction(
      () =>
        document.querySelector('.card[data-id="120-add-csv-export"]') &&
        !document.querySelector(".chip.backpressure"),
    );
    assert.deepEqual(await pausedChips(page), {});
    assert.ok(await page.$('.card[data-id="122-follow-up"] .chip.held'));
    assert.deepEqual(errors, []);
  });
});
