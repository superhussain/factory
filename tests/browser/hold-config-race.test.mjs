// A duplicate loadHoldConfigs starts no second fetch for a slug, and a fetch that started
// before a pipeline save cleared the holds cannot overwrite the newer hold.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  seedPipeline,
  controlBoardPolling,
} from "../support/browser-server.mjs";

test("hold config fetch race", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedPipeline(origin, { schema: 1, hold: { min_ideas: 3, max_wait_hours: 24 } });
    await seedBoard(origin, [
      {
        id: "104-triage-item",
        project: "sample",
        title: "104-triage-item",
        stage: "idea",
        idea: "104-triage-item",
        updated: "2026-01-02T12:00:00Z",
        backlog: { by: "triage" },
      },
    ]);
    const errors = [];
    const page = await browser.newPage();
    page.on("pageerror", (error) => errors.push(String(error)));
    await controlBoardPolling(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(".chip.held");
    const result = await page.evaluate(async () => {
      const waiting = [];
      const real = window.fetch;
      window.fetch = (url, init) =>
        String(url).includes("/api/pipeline/effective/")
          ? new Promise((done) => waiting.push(done))
          : real(url, init);
      const answer = (hold) => new Response(JSON.stringify({ hold }), { status: 200 });
      clearHoldConfigs();
      const a = loadHoldConfigs();
      const dup = loadHoldConfigs();
      const started = waiting.length;
      clearHoldConfigs();
      const b = loadHoldConfigs();
      waiting[1](answer({ min_ideas: 4, max_wait_hours: 48 }));
      await b;
      waiting[0](answer({ min_ideas: 2, max_wait_hours: 6 }));
      await Promise.all([a, dup]);
      window.fetch = real;
      return { started, total: waiting.length, hold: holdConfigs.get("sample") };
    });
    assert.deepEqual(result, { started: 1, total: 2, hold: { min_ideas: 4, max_wait_hours: 48 } });
    assert.deepEqual(errors, []);
  });
});
