// A review card whose round 1 skipped ux and ui: both chips carry the SKIPPED class, title and dashed outline, captured at 390 dark and 1280 light.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const AT = "2026-01-02T12:00:00Z";
const ID = "047-sample-skipped";
const WRAP = `.card-wrap:has(.card[data-id="${ID}"])`;
const JOB = {
  id: ID,
  project: "sample",
  title: "Docs-only sample",
  stage: "review",
  round: 1,
  rounds: [{ security: "PASS", ux: "SKIPPED", ui: "SKIPPED", code: "PASS" }],
  updated: AT,
};
const VIEWS = [
  {
    name: "390-dark",
    scheme: "dark",
    viewport: { width: 390, height: 844, isMobile: true, hasTouch: true },
  },
  { name: "1280-light", scheme: "light", viewport: { width: 1280, height: 800 } },
];

test("skipped chips", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB]);
    for (const view of VIEWS) {
      const errors = [];
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport(view.viewport);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: view.scheme }]);
      page.on("pageerror", (error) => errors.push(String(error)));
      await blockLiveEvents(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(`.card[data-id="${ID}"]`);
      await page.evaluate(async () => {
        await document.fonts.ready;
        await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
      });
      await (await page.$(WRAP)).screenshot({ path: `${SHOTS}skipped-chips-${view.name}.png` });
      const chips = await page.evaluate(
        (sel) =>
          [...document.querySelectorAll(`${sel} .round .chip`)].map((c) => ({
            text: c.textContent,
            cls: c.className,
            title: c.title,
            outline: getComputedStyle(c).outlineStyle,
          })),
        WRAP,
      );
      await context.close();

      await t.test(`${view.name} skipped chips`, () => {
        for (const name of ["ux", "ui"]) {
          const chip = chips.find((c) => c.text === name);
          assert.equal(chip.cls, "chip SKIPPED");
          assert.equal(chip.title, `${name}: SKIPPED`);
          assert.equal(chip.outline, "dashed");
        }
      });
      await t.test(`${view.name} no page errors`, () => {
        assert.deepEqual(errors, []);
      });
    }
  });
});
