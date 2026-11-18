// The notifications menu and page against one owned local Worker, with a synthetic board for
// project sample.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const wait = { polling: 50 };
const job = (id, stage, extra = {}) => ({
  id,
  project: "sample",
  title: "Synthetic " + id,
  stage,
  updated: "2026-01-01T00:00:00Z",
  ...extra,
});
const FILTER = job("043-sample-filter", "build");
const JOBS = [
  job("060-sample-merged", "merged", {
    pr: "https://github.com/example/sample/pull/60",
    done: "2026-01-04T00:00:00Z",
  }),
  job("051-sample-retry", "build", {
    needs: "Pick a retry limit",
    updated: "2026-01-03T00:00:00Z",
  }),
  job("042-sample-export", "pr-open", {
    pr: "https://github.com/example/sample/pull/44",
    escalated: true,
    updated: "2026-01-02T00:00:00Z",
  }),
  job("061-sample-closed", "closed", { done: "2026-01-05T00:00:00Z" }),
  FILTER,
];
const EXPECTED = [
  ["Merged", "Synthetic 060-sample-merged"],
  ["Needs you", "Synthetic 051-sample-retry"],
  ["Needs you", "Synthetic 042-sample-export"],
];

test("notifications", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const openPage = async (hash = "") => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 1280, height: 900 });
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.goto(origin + "/" + hash, { waitUntil: "domcontentloaded" });
      return { page, errors };
    };
    const openMenu = async (page) => {
      await page.click("#notifications-button");
      await page.waitForSelector("#notifications-menu-list .notification-link", wait);
    };
    const pageRows = (page, count) =>
      page.waitForFunction(
        (n) => document.querySelectorAll("#notifications-list .inbox-item").length === n,
        wait,
        count,
      );

    let first;
    await t.test("menu lists recent notifications", async () => {
      first = await openPage();
      const { page, errors } = first;
      await page.waitForSelector("#notifications-dot:not([hidden])", wait);
      await openMenu(page);
      assert.equal(
        await page.$eval("#notifications-button", (el) => el.getAttribute("aria-expanded")),
        "true",
      );
      assert.deepEqual(
        await page.$$eval("#notifications-menu-list .notification-link", (links) =>
          links.map((a) => [
            a.querySelector(".inbox-kind").textContent,
            a.querySelector(".inbox-title").textContent,
          ]),
        ),
        EXPECTED,
      );
      assert.deepEqual(
        await page.$eval("#notifications-more", (a) => [a.textContent, a.getAttribute("href")]),
        ["See more", "#/notifications"],
      );
      assert.equal(await page.$eval("#notifications-dot", (el) => el.hidden), true);
      assert.deepEqual(errors, []);
    });

    await t.test("seen survives a reload", async () => {
      const { page, errors } = first;
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForSelector("#notifications-button[aria-label]", wait);
      await page.waitForFunction(() => document.querySelector(".card"), wait);
      assert.equal(await page.$eval("#notifications-dot", (el) => el.hidden), true);
      assert.equal(
        await page.evaluate(() => localStorage.getItem("factory-notifications-seen")),
        "2026-01-04T00:00:00Z",
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("menu closes", async () => {
      const { page, errors } = await openPage();
      await openMenu(page);
      await page.keyboard.press("Escape");
      assert.deepEqual(
        await page.evaluate(() => [
          document.getElementById("notifications-menu").hidden,
          document.activeElement.id,
        ]),
        [true, "notifications-button"],
      );
      await openMenu(page);
      await page.$eval("#board", (el) =>
        el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true })),
      );
      assert.equal(await page.$eval("#notifications-menu", (el) => el.hidden), true);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("menu caps at eight", async () => {
      await seedBoard(
        origin,
        Array.from({ length: 10 }, (_, i) => job(`07${i}-sample-m${i}`, "merged")),
      );
      const { page, errors } = await openPage();
      await openMenu(page);
      assert.equal(
        await page.$$eval("#notifications-menu-list .notification-link", (l) => l.length),
        8,
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
      await seedBoard(origin, JOBS);
    });

    let list;
    await t.test("see more opens the page", async () => {
      list = await openPage();
      const { page, errors } = list;
      await openMenu(page);
      await page.click("#notifications-more");
      await pageRows(page, 3);
      assert.deepEqual(
        await page.evaluate(() => [document.body.dataset.view, document.activeElement.id]),
        ["notifications", "notifications-page-heading"],
      );
      assert.match(await page.title(), /^Notifications · /);
      assert.deepEqual(
        await page.$$eval("#notifications-list .inbox-item", (items) =>
          items.map((item) =>
            [".inbox-rank", ".inbox-kind", ".inbox-title"].map(
              (sel) => item.querySelector(sel).textContent,
            ),
          ),
        ),
        EXPECTED.map((row, i) => [String(i + 1), ...row]),
      );
      assert.deepEqual(
        await page.$eval("#notifications-list .inbox-item .inbox-actions", (el) =>
          [...el.children].map((a) => a.textContent),
        ),
        ["Open PR #60", "Open job"],
      );
      assert.deepEqual(errors, []);
    });

    await t.test("open job returns to the page", async () => {
      const { page, errors } = list;
      await page.$$eval("#notifications-list .inbox-item", (items) =>
        items[1].querySelector('a[href="#051-sample-retry"]').click(),
      );
      await page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("051-sample-retry"),
        wait,
      );
      await page.click("#d-close");
      await page.waitForFunction(() => location.hash === "#/notifications", wait);
      assert.equal(await page.$eval("#notifications-page", (el) => el.checkVisibility()), true);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("h leaves the page", async () => {
      const { page, errors } = await openPage("#/notifications");
      await pageRows(page, 3);
      await page.keyboard.press("h");
      await page.waitForFunction(() => location.hash === "", wait);
      assert.equal(await page.evaluate(() => document.body.dataset.view), "board");
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("empty board", async () => {
      await seedBoard(origin, [FILTER]);
      const { page, errors } = await openPage("#/notifications");
      await page.waitForFunction(
        () => document.getElementById("notifications-status").textContent.startsWith("No "),
        wait,
      );
      assert.equal(
        await page.$eval("#notifications-status", (el) => el.textContent),
        "No notifications yet. Merged jobs and jobs waiting on you show up here.",
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });
  });
});
