// While its own project's jobs waiting on you reach the project limit, a plain idea shows pickup's
// backpressure pause. Another project's waiting jobs never pause it. A prioritized or claimed idea shows no chip, and the paused
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
const waiting = (project) =>
  [1, 2, 3, 4, 5].map((n) =>
    job(`20${n}-waiting`, { project, stage: "needs-human", idea: undefined }),
  );
const IDEAS = [
  job("120-add-csv-export", {}),
  job("121-rushed", { priority: { at: AT } }),
  job("122-follow-up", { refs: [{ id: "120-add-csv-export", kind: "follow-up-of" }] }),
  job("123-claimed", { claim: { device: "host-a", session: "s1", since: AT } }),
];
const JOBS = [...waiting("sample"), ...IDEAS];
const project = (title) => `---\nrepo: example/sample\nbase: main\ntitle: ${title}\n---\n`;
const TITLE =
  "Pickup starts no new idea in this project while 5 of its jobs wait on you. It pauses at 5. Approved specs, moved jobs and prioritized ideas still start. Settings, Pipeline, Review backpressure sets the number.";

async function openBoard(browser, origin, pipeline, jobs = JOBS, onPage) {
  await seedProject(origin, "sample", project("Synthetic sample"));
  await seedProject(origin, "other", project("Synthetic other"));
  await seedPipeline(origin, pipeline);
  await seedBoard(origin, jobs);
  const errors = [];
  const page = await browser.newPage();
  page.on("pageerror", (error) => errors.push(String(error)));
  await blockLiveEvents(page);
  await page.setViewport({ width: 1280, height: 900 });
  if (onPage) await onPage(page);
  await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
  return { page, errors };
}

const clippedChips = (page) =>
  page.$$eval(".card .tags .chip", (els) =>
    els.filter((e) => e.scrollWidth > e.clientWidth).map((e) => e.textContent),
  );

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
    assert.deepEqual(await clippedChips(page), []);
    // A wrapped chip in the same row must not stretch the one-line Paused chip to its height.
    // The 40% cap keeps the wrapped chip on the Paused chip's line.
    const heights = await page.$eval('.card[data-id="120-add-csv-export"] .tags', (tags) => {
      const paused = tags.querySelector(".chip.backpressure");
      const before = paused.offsetHeight;
      const long = document.createElement("span");
      long.className = "chip";
      long.style.maxWidth = "40%";
      long.textContent = "Batches behind 999-synthetic-rider";
      tags.prepend(long);
      const after = {
        paused: paused.offsetHeight,
        long: long.offsetHeight,
        sameRow: long.offsetTop === paused.offsetTop,
      };
      long.remove();
      return { before, ...after };
    });
    assert.ok(heights.sameRow && heights.long > heights.before, JSON.stringify(heights));
    assert.equal(heights.paused, heights.before);
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
    await page.waitForFunction(
      () =>
        document.querySelector('.card[data-id="122-follow-up"] .chip.held')?.textContent ===
        "Batch ready",
    );
    assert.deepEqual(await pausedChips(page), {});
    assert.deepEqual(await clippedChips(page), []);
    assert.deepEqual(errors, []);
  });
});

test("another project's waiting jobs do not pause", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    const { page, errors } = await openBoard(browser, origin, { schema: 1 }, [
      ...waiting("other"),
      ...IDEAS,
    ]);
    await page.waitForSelector('.card[data-id="122-follow-up"] .chip.held');
    assert.deepEqual(await pausedChips(page), {});
    assert.deepEqual(errors, []);
  });
});

// Routes /api/pipeline/effective/sample to answer(request) and lets every other request through.
const routeEffective = (answer) => async (page) => {
  await page.setBypassServiceWorker(true);
  await page.setRequestInterception(true);
  page.on("request", (request) =>
    new URL(request.url()).pathname === "/api/pipeline/effective/sample"
      ? answer(request)
      : request.continue(),
  );
};

const heldText = (page, id) =>
  page.$eval(`.card[data-id="${id}"] .chip.held`, (el) => el.textContent);

test("backpressure chip waits for the effective view", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    let held;
    let arrived;
    const asked = new Promise((done) => (arrived = done));
    const { page, errors } = await openBoard(
      browser,
      origin,
      { schema: 1, backpressure: { needs_you: 6 } },
      JOBS,
      routeEffective((request) => {
        held = request;
        arrived();
      }),
    );
    await asked;
    await page.waitForSelector('.card[data-id="122-follow-up"] .chip.held');
    assert.deepEqual(await pausedChips(page), {});
    assert.equal(await heldText(page, "122-follow-up"), "Held for batching");
    await held.continue();
    await page.waitForFunction(
      () =>
        document.querySelector('.card[data-id="122-follow-up"] .chip.held')?.textContent ===
        "Batch ready",
    );
    assert.deepEqual(await pausedChips(page), {});
    assert.deepEqual(errors, []);
  });
});

test(
  "backpressure chip falls back when the effective view fails",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      const { page, errors } = await openBoard(
        browser,
        origin,
        { schema: 1 },
        JOBS,
        routeEffective((request) => request.respond({ status: 500, body: "fixture failure" })),
      );
      await page.waitForSelector('.card[data-id="120-add-csv-export"] .chip.backpressure');
      assert.equal((await pausedChips(page))["120-add-csv-export"], "Paused: 5 waiting on you");
      assert.deepEqual(errors, []);
    });
  },
);
