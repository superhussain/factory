import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const baseline = process.env.FACTORY_CAPTURE_BASELINE === "1";
const captureDir = process.env.FACTORY_CAPTURE_DIR;
const TEXT = "Copy is unavailable. The link is https://example.test/#042-pacing-check";
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

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
}

// Shows the toast and waits until its text is set, so the hide timer is armed.
async function show(page) {
  await page.evaluate((text) => showBoardStatus(text), TEXT);
  await page.waitForFunction(
    (text) => document.getElementById("board-status").textContent === text,
    {},
    TEXT,
  );
}

const visible = (page) =>
  page.$eval("#board-status", (el) => !el.hidden && el.getClientRects().length > 0);

async function release(page) {
  await page.evaluate(() => {
    document.getElementById("board-status").blur();
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
  });
}

test("a tapped or focused board toast stays until released", { timeout: 120000 }, async () => {
  await prepareAssets();
  if (captureDir) await mkdir(captureDir, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const context = await browser.createBrowserContext();
    try {
      const page = await context.newPage();
      await page.setViewport({
        width: 390,
        height: 844,
        deviceScaleFactor: 1,
        isMobile: true,
        hasTouch: true,
      });
      await page.evaluateOnNewDocument(() => {
        const real = window.setTimeout;
        window.setTimeout = (fn, ms, ...args) => real(fn, ms === 12000 ? 600 : ms, ...args);
      });
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

      await show(page);
      await wait(1500);
      if (!baseline) assert.equal(await visible(page), false, "untouched toast hides");

      await show(page);
      await page.tap("#board-status");
      await wait(1500);
      if (captureDir) {
        await page.evaluate(() => document.fonts.ready);
        await page.screenshot({ path: join(captureDir, "board-status-held-390.png") });
      }
      if (!baseline) {
        assert.equal(await visible(page), true, "tapped toast stays");
        await release(page);
        await wait(1500);
        assert.equal(await visible(page), false, "released toast hides");

        await show(page);
        await page.focus("#board-status");
        await wait(1500);
        assert.equal(await visible(page), true, "focused toast stays");
        await release(page);
        await wait(1500);
        assert.equal(await visible(page), false, "blurred toast hides");
      }
      assert.deepEqual(external, [], "no external requests");
      assert.deepEqual(errors, [], "no page errors");
    } finally {
      await context.close();
    }
  });
});
