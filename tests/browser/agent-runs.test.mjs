// The job panel lists each agents row in a collapsible table, in board order, and hides the
// table when a job has none.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const run = (at, stage, role, via, harness, effort, extra = {}) => ({
  at,
  stage,
  role,
  harness,
  via,
  model: "model-a",
  effort,
  ...extra,
});
const JOB = {
  id: "101-wrap-doc-line",
  project: "sample",
  title: "Wrap the long doc line",
  stage: "review",
  round: 1,
  updated: "2026-01-02T13:10:00Z",
  agents: [
    run("2026-01-02T12:00:00Z", "idea", "orchestrator", "launcher", "provider-a", "high"),
    run("2026-01-02T12:05:00Z", "spec", "spec", "native", "claude", "medium"),
    run("2026-01-02T13:10:00Z", "review", "review-ui", "native", "claude", "medium", { round: 1 }),
  ],
};
const PLAIN = {
  id: "102-move-comment",
  project: "sample",
  title: "Move the comment",
  stage: "build",
  round: 0,
  updated: "2026-01-02T12:00:00Z",
};

test("agent runs table", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB, PLAIN]);
    const errors = [];
    const page = await browser.newPage();
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(`${origin}/#${JOB.id}`, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => !document.getElementById("d-agents").hidden);
    assert.equal(await page.$eval("#d-agents summary", (el) => el.textContent), "Agents: 3 runs");
    const rows = await page.$$eval("#d-agents tbody tr", (trs) =>
      trs.map((tr) => [...tr.cells].map((td) => td.textContent)),
    );
    assert.equal(rows.length, 3);
    assert.deepEqual(rows[2], [
      "2026-01-02 13:10 UTC",
      "review",
      "1",
      "review-ui",
      "claude",
      "native",
      "model-a",
      "medium",
    ]);
    assert.equal(rows[0][2], "");
    await page.evaluate((id) => (location.hash = id), PLAIN.id);
    await page.waitForFunction(
      (id) => document.getElementById("d-title").textContent === id,
      {},
      PLAIN.title,
    );
    assert.equal(await page.$eval("#d-agents", (el) => el.hidden), true);
    assert.deepEqual(errors, []);
  });
});
