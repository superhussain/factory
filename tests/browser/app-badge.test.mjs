// The needs-you count as the installed app's icon badge, against one owned local Worker.
// Stub setAppBadge and clearAppBadge methods record each call in window.badgeCalls.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";

const JOBS = [
  { id: "001-sample", project: "sample", title: "Synthetic sample job", stage: "spec" },
  {
    id: "005-sample-needs",
    project: "sample",
    title: "Synthetic needs job",
    stage: "build",
    needs: "Approve the spec",
  },
  { id: "007-sample-human", project: "sample", title: "Synthetic human job", stage: "needs-human" },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));
const wait = { polling: 50, timeout: 10000 };

test("app badge", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);

    async function open(badging = true) {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 900 });
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await blockLiveEvents(page);
      await page.evaluateOnNewDocument((stub) => {
        if (!stub) {
          delete Navigator.prototype.setAppBadge;
          delete Navigator.prototype.clearAppBadge;
          return;
        }
        window.badgeCalls = [];
        Navigator.prototype.setAppBadge = async (n) => {
          window.badgeCalls.push(["set", n]);
        };
        Navigator.prototype.clearAppBadge = async () => {
          window.badgeCalls.push(["clear"]);
        };
      }, badging);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="005-sample-needs"]', wait);
      return { context, page, errors };
    }
    const calls = (page) => page.evaluate(() => window.badgeCalls);

    const { context, page, errors } = await open();

    await t.test("badge B1 the needs-you count reaches setAppBadge", async () => {
      assert.deepEqual(await calls(page), [["set", 2]]);
      assert.ok((await page.title()).startsWith("(2) "));
    });

    await t.test("badge B2 a repeat render sends nothing", async () => {
      assert.equal(await page.evaluate(() => poll()), true);
      assert.deepEqual(await calls(page), [["set", 2]]);
    });

    await t.test("badge B3 no waiting jobs clears the badge", async () => {
      await seedBoard(origin, JOBS.slice(0, 1));
      assert.equal(await page.evaluate(() => poll()), true);
      assert.deepEqual(await calls(page), [["set", 2], ["clear"]]);
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test("badge B4 an expired sign-in clears the badge", async () => {
      await seedBoard(origin, JOBS);
      const { context, page, errors } = await open();
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        if (new URL(request.url()).pathname === "/board.json")
          return request.respond({ status: 401, body: "" });
        return request.continue();
      });
      assert.equal(await page.evaluate(() => poll()), false);
      assert.deepEqual(await calls(page), [["set", 2], ["clear"]]);
      assert.deepEqual(
        errors.filter((e) => !e.includes("401")),
        [],
      );
      await context.close();
    });

    await t.test("badge B5 a browser without the Badging API renders the board", async () => {
      const { context, page, errors } = await open(false);
      assert.equal(await page.evaluate(() => "setAppBadge" in navigator), false);
      assert.ok((await page.title()).startsWith("(2) "));
      assert.deepEqual(errors, []);
      await context.close();
    });
  });
});
