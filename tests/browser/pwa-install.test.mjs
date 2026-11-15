// Installable dashboard: Chrome's own manifest parse and installability report against one owned
// local Worker. Synthetic running example: the local Worker titled Factory.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  controlBoardPolling,
  seedBoard,
  seedProject,
  withBrowserFixture,
} from "../support/browser-server.mjs";
import { waitForBoard } from "../support/board-ready.cjs";

test("Chrome finds the dashboard installable", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, []);
    // The default context: an incognito one makes Chrome report in-incognito.
    const page = await browser.defaultBrowserContext().newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    try {
      await controlBoardPolling(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.evaluate(async () => {
        await navigator.serviceWorker.ready;
      });
      const cdp = await page.createCDPSession();

      const manifest = await cdp.send("Page.getAppManifest");
      assert.deepEqual(manifest.errors, []);
      assert.ok(manifest.url.endsWith("/assets/manifest.webmanifest"), manifest.url);
      const data = JSON.parse(manifest.data);
      assert.equal(data.name, "Factory");
      assert.equal(data.short_name, "Factory");
      assert.equal(data.display, "standalone");
      assert.equal(data.start_url, "/");
      assert.equal(data.icons.length, 3);

      const { installabilityErrors } = await cdp.send("Page.getInstallabilityErrors");
      assert.deepEqual(installabilityErrors, []);
      assert.deepEqual(errors, []);
    } finally {
      await page.close();
    }
  });
});
