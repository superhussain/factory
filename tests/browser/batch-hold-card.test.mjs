// A follow-up or triage-queued idea shows "Held for batching". One a person added, prioritized
// or a session claimed does not.
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
];

test("held for batching chip", { timeout: 120000 }, async () => {
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
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector('.card[data-id="106-rushed-item"]');
    const chips = await page.$$eval(".card", (cards) =>
      Object.fromEntries(
        cards.map((c) => [
          c.dataset.id,
          [...c.querySelectorAll(".chip")].map((chip) => chip.textContent),
        ]),
      ),
    );
    const held = Object.keys(chips).filter((id) => chips[id].includes("Held for batching"));
    assert.deepEqual(held.sort(), ["102-move-comment", "104-triage-item"]);
    assert.match(
      await page.$eval('.card[data-id="102-move-comment"] .chip', (el) => el.title),
      /Settings, Pipeline, Batch follow-ups sets the numbers\.$/,
    );
    assert.deepEqual(errors, []);
  });
});
