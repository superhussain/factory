// The three header icon buttons stay on one row and match the project filter height.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const JOBS = [
  { id: "001-sample", project: "sample", title: "Synthetic sample job", stage: "spec" },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));

const TOUCH = { isMobile: true, hasTouch: true };
const CASES = [
  { viewport: { width: 1280, height: 900 }, size: 36 },
  { viewport: { width: 390, height: 844 }, size: 32 },
  { viewport: { width: 320, height: 844 }, size: 32 },
  { viewport: { width: 390, height: 844, ...TOUCH }, size: 44 },
  { viewport: { width: 320, height: 844, ...TOUCH }, size: 44 },
];

test("header icons share one row and match the filter height", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    for (const { viewport, size } of CASES) {
      const label = `${viewport.width}px${viewport.hasTouch ? " touch" : ""}`;
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport(viewport);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector("#new-idea:not([hidden])");
      const header = await page.evaluate(() => {
        const rect = (id) => document.getElementById(id).getBoundingClientRect();
        const icons = ["new-idea", "job-jump", "settings-link"];
        const el = document.querySelector("header");
        return {
          tops: icons.map((id) => Math.round(rect(id).top)),
          heights: [...icons, "filter"].map((id) => rect(id).height),
          scrollWidth: el.scrollWidth,
          clientWidth: el.clientWidth,
        };
      });
      assert.equal(new Set(header.tops).size, 1, `icon tops at ${label}: ${header.tops}`);
      assert.deepEqual(header.heights, [size, size, size, size], `heights at ${label}`);
      assert.ok(
        header.scrollWidth <= header.clientWidth,
        `header scrolls at ${label}: ${header.scrollWidth} > ${header.clientWidth}`,
      );
      if (viewport.hasTouch) {
        await page.evaluate(() => (location.hash = "#/settings"));
        await page.waitForFunction(
          () => document.getElementById("settings-back").getClientRects().length > 0,
        );
        const back = await page.evaluate(
          () => document.getElementById("settings-back").getBoundingClientRect().height,
        );
        assert.equal(back, 44, `settings-back height at ${label}`);
      }
    }
  });
});
