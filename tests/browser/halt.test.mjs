import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(origin, [
    {
      id: "042-pacing-check",
      project: "sample",
      title: "Synthetic pacing check",
      stage: "review",
      updated: "2026-01-01T00:00:00Z",
    },
  ]);
  const r = await fetch(origin + "/api/halt", {
    method: "PUT",
    body: '{"halted":true,"reason":"Runaway launches on wsl"}',
    headers: { "Content-Type": "application/json" },
  });
  assert.equal(r.status, 200);
}

test("the stop banner shows, Resume clears it and Stop sets it", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const context = await browser.createBrowserContext();
    try {
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
      const external = [],
        errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        if (new URL(request.url()).origin !== origin) {
          external.push(request.url());
          return request.abort();
        }
        return request.continue();
      });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="042-pacing-check"]');

      await page.waitForSelector("#halt-banner:not([hidden])");
      assert.match(
        await page.$eval("#halt-text", (el) => el.textContent),
        /Runaway launches on wsl/,
      );

      await page.click("#halt-resume");
      await page.waitForSelector("#halt-banner", { hidden: true });
      const stored = await (await fetch(origin + "/api/halt")).json();
      assert.equal(stored.halted, false);
      assert.equal(await page.evaluate(() => document.activeElement.id), "board-status");

      await page.evaluate(() => {
        location.hash = "#/settings/hosts";
      });
      await page.waitForSelector("#halt-form:not([hidden])");
      await page.type("#halt-reason", "   ");
      await page.click("#halt-stop");
      assert.equal(await page.$eval("#halt-status", (el) => el.textContent), "Enter a reason.");
      assert.equal((await (await fetch(origin + "/api/halt")).json()).halted, false);
      await page.$eval("#halt-reason", (el) => {
        el.value = "";
      });
      await page.type("#halt-reason", "Synthetic stop");
      await page.click("#halt-stop");
      await page.waitForSelector("#halt-banner:not([hidden])");
      assert.match(await page.$eval("#halt-text", (el) => el.textContent), /Synthetic stop/);
      assert.equal(await page.evaluate(() => document.activeElement.id), "halt-resume");

      await page.click("#halt-resume");
      await page.waitForSelector("#halt-form:not([hidden])");
      assert.equal(await page.evaluate(() => document.activeElement.id), "halt-reason");

      assert.deepEqual(external, [], "no external requests");
      assert.deepEqual(errors, [], "no page errors");
    } finally {
      await context.close();
    }
  });
});
