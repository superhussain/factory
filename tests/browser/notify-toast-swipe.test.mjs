// Swipe to dismiss notification toasts with a mouse and a touchscreen, against one owned local Worker.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";

const RETRY = {
  id: "073-sample-retry",
  project: "sample",
  title: "Synthetic retry limit",
  stage: "spec",
  updated: "2026-01-01T00:00:00Z",
};
const wait = { polling: 50 };
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test("notification toast swipe", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [RETRY]);

    const open = async (viewport) => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport(viewport);
      page.errors = [];
      page.on("pageerror", (error) => page.errors.push(String(error)));
      await blockLiveEvents(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForFunction(() => toastSeen !== null, wait);
      return page;
    };
    const showToast = (page) =>
      page.evaluate(() =>
        showNotifyToast("spec", "#073-sample-retry", "Synthetic retry limit", "073-sample-retry"),
      );
    const center = (page) =>
      page.$eval("#notify-toasts .notification-link", (el) => {
        const r = el.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
      });
    const count = (page) => page.$$eval("#notify-toasts .notify-toast", (els) => els.length);
    const hash = (page) => page.evaluate(() => location.hash);

    const page = await open({ width: 1280, height: 900 });

    await t.test("swipe S1 a 120 px mouse drag removes the toast and keeps the hash", async () => {
      await showToast(page);
      const { x, y } = await center(page);
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(x + 120, y, { steps: 12 });
      await page.mouse.up();
      await page.waitForFunction(() => !document.querySelector(".notify-toast"), wait);
      await pause(100);
      assert.equal(await hash(page), "");
    });

    await t.test("swipe S2 a slow 30 px drag snaps back", async () => {
      await showToast(page);
      const { x, y } = await center(page);
      await page.mouse.move(x, y);
      await page.mouse.down();
      await page.mouse.move(x - 30, y, { steps: 6 });
      await pause(200);
      await page.mouse.up();
      await pause(await page.evaluate(() => SWIPE_MS + 100));
      assert.equal(await count(page), 1);
      assert.equal(
        await page.$eval("#notify-toasts .notify-toast", (li) => li.style.transform),
        "",
      );
      assert.equal(await hash(page), "");
    });

    await t.test("swipe S3 a tap still opens the job", async () => {
      await page.click("#notify-toasts .notification-link");
      await page.waitForFunction(() => location.hash === "#073-sample-retry", wait);
      await page.waitForFunction(() => !document.querySelector(".notify-toast"), wait);
      assert.deepEqual(page.errors, []);
    });

    await t.test("swipe S4 a 120 px touch drag left at 390x844 removes the toast", async () => {
      const phone = await open({ width: 390, height: 844, isMobile: true, hasTouch: true });
      await showToast(phone);
      const { x, y } = await center(phone);
      const touch = await phone.touchscreen.touchStart(x, y);
      for (let dx = 10; dx <= 120; dx += 10) await touch.move(x - dx, y);
      await touch.end();
      await phone.waitForFunction(() => !document.querySelector(".notify-toast"), wait);
      await pause(100);
      assert.equal(await hash(phone), "");
      assert.deepEqual(phone.errors, []);
    });

    await t.test("swipe S5 a press released outside the list still expires", async () => {
      const other = await open({ width: 1280, height: 900 });
      await showToast(other);
      const { x, y } = await other.$eval("#notify-toasts .notify-toast", (li) => {
        const r = li.getBoundingClientRect();
        return { x: r.x + 2, y: r.y + 2 };
      });
      await other.mouse.move(x, y);
      await other.mouse.down();
      await other.mouse.move(x, y - 300);
      await other.mouse.up();
      const ms = await other.evaluate(() => TOAST_MS);
      await other.waitForFunction(() => !document.querySelector(".notify-toast"), {
        ...wait,
        timeout: ms + 2000,
      });
      assert.deepEqual(other.errors, []);
    });
  });
});
