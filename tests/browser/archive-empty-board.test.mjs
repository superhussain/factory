// The "Show archived jobs" button on the blank board, shown once board.json carries `archived`.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject } from "../support/browser-server.mjs";

const ROWS = [
  {
    id: "416-rate-limit",
    project: "sample",
    title: "Rate limit",
    stage: "merged",
    updated: "2026-09-10T12:00:00Z",
    done: "2026-09-10T12:00:00Z",
  },
  {
    id: "413-old-suggestion",
    project: "sample",
    title: "Old suggestion",
    stage: "closed",
    updated: "2026-09-05T12:00:00Z",
    done: "2026-09-05T12:00:00Z",
  },
];
const wait = { polling: 50 };

test("archived jobs from the blank board", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const errors = [];
    const asked = [];
    let withArchive = false;
    let failNext = false;
    const openPage = async () => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 1280, height: 900 });
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        const url = new URL(request.url());
        if (url.pathname === "/board.json" && withArchive)
          return request.respond({
            status: 200,
            contentType: "application/json",
            headers: { ETag: '"empty-archived"' },
            body: JSON.stringify({
              updated: "2026-11-05T12:00:00Z",
              jobs: [],
              archived: { max_id: 416 },
            }),
          });
        if (url.pathname !== "/api/board/archive") return request.continue();
        asked.push(url.search);
        if (failNext) {
          failNext = false;
          return request.respond({ status: 500, contentType: "text/plain", body: "boom" });
        }
        return request.respond({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ jobs: ROWS, page: 1, pages: 1, total: 2 }),
        });
      });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(".blank", wait);
      return page;
    };

    await t.test("no archived field means no button", async () => {
      const page = await openPage();
      assert.equal(await page.$eval(".blank h2", (el) => el.textContent), "No jobs yet");
      assert.equal(await page.$(".archive-more"), null);
    });

    withArchive = true;

    await t.test("archived.max_id adds the button to the blank board", async () => {
      const page = await openPage();
      await page.waitForSelector(".blank .archive-more", wait);
      assert.deepEqual(
        await page.$eval(".blank .archive-more", (el) => [el.textContent, el.dataset.stage]),
        ["Show archived jobs", "closed"],
      );
    });

    await t.test("the click loads page 1 and draws the columns", async () => {
      const page = await openPage();
      await page.waitForSelector(".blank .archive-more", wait);
      await page.click(".blank .archive-more");
      await page.waitForSelector(".col.closed .archive-list .card", wait);
      assert.equal(await page.$(".blank"), null);
      assert.ok(asked.includes("?page=1"));
      assert.deepEqual(
        await page.$$eval(
          ".col.merged .archive-list .card, .col.closed .archive-list .card",
          (els) => els.map((c) => c.dataset.id),
        ),
        ["416-rate-limit", "413-old-suggestion"],
      );
      for (const stage of ["merged", "closed"])
        assert.equal(await page.$eval(`#column-cards-${stage}`, (el) => el.hidden), false);
      assert.equal(
        await page.$eval(".archive-count", (el) => el.textContent),
        "2 of 2 archived jobs loaded",
      );
      assert.deepEqual(
        await page.evaluate(() => {
          const el = document.activeElement;
          return [el.classList.contains("column-toggle"), el.dataset.stage];
        }),
        [true, "closed"],
      );
    });

    await t.test("a failed load stays on the blank board and can be retried", async () => {
      const page = await openPage();
      await page.waitForSelector(".blank .archive-more", wait);
      failNext = true;
      await page.click(".blank .archive-more");
      await page.waitForSelector(".blank .archive-error", wait);
      assert.equal(
        await page.$eval(".archive-error", (el) => el.textContent),
        "Could not load archived jobs. Try again.",
      );
      await page.waitForFunction(
        () =>
          document.getElementById("board-status").textContent ===
          "Could not load archived jobs. Try again.",
        wait,
      );
      assert.equal(await page.$(".blank [role=alert]"), null);
      assert.equal(
        await page.$eval(".blank .archive-more", (el) => el.textContent),
        "Show archived jobs",
      );
      await page.click(".blank .archive-more");
      await page.waitForSelector(".col.closed .archive-list .card", wait);
      assert.equal(await page.$(".blank"), null);
    });

    assert.deepEqual(errors, []);
  });
});
