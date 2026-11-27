// A draft pr-open card shows one whole Draft chip; pr-open cards with draft false, null or missing, and a draft review card, show none.
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
const TITLE =
  "Draft PR, not meant to merge yet. It skips Needs you until its draft flag is cleared.";
const base = { project: "sample", round: 1, updated: AT };
const JOBS = [
  {
    ...base,
    id: "300-proto-pacing",
    title: "Synthetic prototype pacing",
    stage: "pr-open",
    draft: true,
    escalated: true,
    needs: "review the prototype PR #70",
  },
  { ...base, id: "301-sample-ready", title: "Ready sample", stage: "pr-open" },
  { ...base, id: "302-sample-false", title: "False sample", stage: "pr-open", draft: false },
  { ...base, id: "303-sample-null", title: "Null sample", stage: "pr-open", draft: null },
  { ...base, id: "304-sample-review", title: "Review sample", stage: "review", draft: true },
];

test("draft chip", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const errors = [];
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900, isMobile: false });
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector('.card[data-id="304-sample-review"]');
    await page.evaluate(async () => {
      await document.fonts.ready;
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    const chips = await page.evaluate(
      (ids) => {
        const out = {};
        for (const id of ids) {
          out[id] = [...document.querySelectorAll(`.card[data-id="${id}"] .tags .chip`)]
            .filter((c) => c.textContent === "Draft")
            .map((c) => ({
              title: c.title,
              inPrOpen: !!c.closest("section.col.pr-open"),
              clientWidth: c.clientWidth,
              scrollWidth: c.scrollWidth,
            }));
        }
        return out;
      },
      JOBS.map((j) => j.id),
    );
    await context.close();

    await t.test("draft pr-open card shows one Draft chip", () => {
      const found = chips["300-proto-pacing"];
      assert.equal(found.length, 1);
      assert.equal(found[0].title, TITLE);
      assert.ok(found[0].inPrOpen, "card sits in section.col.pr-open");
    });
    await t.test("Draft chip not clipped", () => {
      const [c] = chips["300-proto-pacing"];
      assert.ok(c.scrollWidth <= c.clientWidth, `${c.scrollWidth} > ${c.clientWidth}`);
    });
    for (const id of [
      "301-sample-ready",
      "302-sample-false",
      "303-sample-null",
      "304-sample-review",
    ]) {
      await t.test(`${id} shows no Draft chip`, () => {
        assert.deepEqual(chips[id], []);
      });
    }
    await t.test("no page errors", () => {
      assert.deepEqual(errors, []);
    });
  });
});
