// A held follow-up or triage-queued idea shows pickup's batch state: the first of a ready group
// says "Batch ready", the rest batch behind it, and a group short of the hold waits. One a person
// added, prioritized or a session claimed shows no held chip.
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
const RECENT = new Date(Date.now() - 3.5 * 3600000).toISOString().replace(/\.\d{3}Z$/, "Z");
const FOLLOW = [{ id: "090-sample-old", kind: "follow-up-of" }];
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
  idea("090-sample-old", {}),
  idea("101-wrap-doc-line", {
    refs: FOLLOW,
    claim: { device: "host-a", session: "s1", since: AT },
  }),
  idea("102-move-comment", { refs: FOLLOW }),
  idea("104-triage-item", { backlog: { by: "triage" } }),
  idea("105-person-item", { refs: FOLLOW, capture_id: "00000000-0000-4000-8000-000000000105" }),
  idea("106-rushed-item", { refs: FOLLOW, priority: { at: AT } }),
  idea("201-wait-a", { project: "other", refs: FOLLOW, updated: RECENT }),
  idea("202-wait-b", { project: "other", refs: FOLLOW, updated: RECENT }),
];
const project = (title) => `---\nrepo: example/sample\nbase: main\ntitle: ${title}\n---\n`;

test("held for batching chip", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(origin, "sample", project("Synthetic sample"));
    await seedProject(origin, "other", project("Synthetic other"));
    await seedPipeline(origin, { schema: 1, hold: { min_ideas: 3, max_wait_hours: 24 } });
    await seedBoard(origin, JOBS);
    const errors = [];
    const page = await browser.newPage();
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() =>
      document
        .querySelector('.card[data-id="201-wait-a"] .chip.held')
        ?.textContent.includes("of 3,"),
    );
    const held = await page.$$eval(".card", (cards) =>
      Object.fromEntries(
        cards
          .filter((c) => c.querySelector(".chip.held"))
          .map((c) => [c.dataset.id, c.querySelector(".chip.held").textContent]),
      ),
    );
    assert.deepEqual(held, {
      "102-move-comment": "Batch ready",
      "104-triage-item": "Batches behind 102-move-comment",
      "201-wait-a": "Waits for a batch (2 of 3, 3h of 24h)",
      "202-wait-b": "Waits for a batch (2 of 3, 3h of 24h)",
    });
    assert.match(
      await page.$eval('.card[data-id="102-move-comment"] .chip.held', (el) => el.title),
      /Settings, Pipeline, Batch follow-ups sets the numbers\.$/,
    );
    const wait = await page.$eval('.card[data-id="201-wait-a"] .chip.held', (el) => el.title);
    assert.ok(
      wait.startsWith("Waits for a batch (2 of 3, 3h of 24h). Pickup holds this idea"),
      wait,
    );
    await page.evaluate(() => {
      location.hash = "#104-triage-item";
    });
    await page.waitForSelector("#d-held:not([hidden])");
    assert.equal(
      await page.$eval("#d-held", (el) => el.textContent),
      "Pickup launches 102-move-comment next and batches this idea behind it. Settings, Pipeline, Batch follow-ups sets the numbers.",
    );
    // 21 hours on, the oldest of other's held ideas has waited 24h. The 30-second tick redraws the
    // chips, but not while a card has focus.
    const chip = () => page.$eval('.card[data-id="201-wait-a"] .chip.held', (el) => el.textContent);
    await page.evaluate(() => (location.hash = ""));
    await page.waitForSelector("#detail[hidden]");
    await page.focus('.card[data-id="201-wait-a"]');
    await page.evaluate(() => {
      const now = Date.now;
      Date.now = () => now() + 21 * 3600000;
    });
    await page.evaluate(() => clockTick());
    assert.equal(await chip(), "Waits for a batch (2 of 3, 3h of 24h)");
    await page.evaluate(() => {
      document.activeElement.blur();
      clockTick();
    });
    assert.equal(await chip(), "Batch ready");
    assert.deepEqual(errors, []);
  });
});
