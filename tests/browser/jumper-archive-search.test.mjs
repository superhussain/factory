// The jumper searches archived jobs only when a person picks "Search archived jobs":
// one GET /api/board/archive?q= per pick (answered here by request interception), the
// matches listed in an "Archived jobs" section, each opening read-only.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject } from "../support/browser-server.mjs";

const EXPORT = {
  id: "415-add-export",
  project: "sample",
  title: "Add export",
  stage: "merged",
  updated: "2026-11-05T12:00:00Z",
  done: "2026-11-05T12:00:00Z",
};
const ARCHIVED = {
  id: "412-fix-login",
  project: "sample",
  title: "Fix login",
  stage: "merged",
  round: 1,
  branch: "factory/412-fix-login",
  updated: "2026-09-01T12:00:00Z",
  done: "2026-09-01T12:00:00Z",
};
const wait = { polling: 50 };

test("the jumper searches archived jobs on a pick", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const page = await (await browser.createBrowserContext()).newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const asked = [];
    let archivedField = false;
    await page.setRequestInterception(true);
    page.on("request", (request) => {
      const url = new URL(request.url());
      if (url.pathname === "/board.json") {
        const board = { jobs: [EXPORT], ...(archivedField ? { archived: { max_id: 416 } } : {}) };
        return request.respond({
          status: 200,
          contentType: "application/json",
          headers: { ETag: '"t1"' },
          body: JSON.stringify(board),
        });
      }
      if (url.pathname !== "/api/board/archive" || !url.searchParams.has("q"))
        return request.continue();
      const q = url.searchParams.get("q");
      asked.push(q);
      if (q === "boom") return request.respond({ status: 500, body: "boom" });
      const jobs = [ARCHIVED].filter((j) =>
        [j.id, j.title, j.project].some((f) => f.toLowerCase().includes(q.toLowerCase())),
      );
      return request.respond({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ jobs, total: jobs.length }),
      });
    });
    const options = () =>
      page.$$eval('#job-results [role="option"]', (rows) =>
        rows.map((o) => ({
          title: o.querySelector(".title").textContent,
          meta: o.querySelector(".meta").textContent,
          selected: o.getAttribute("aria-selected"),
        })),
      );
    const archiveOption = () =>
      page.$$eval('#job-results [role="option"]', (rows) =>
        rows
          .map((o) => ({
            title: o.querySelector(".title").textContent,
            meta: o.querySelector(".meta").textContent,
            selected: o.getAttribute("aria-selected"),
          }))
          .find((o) => o.title === "Search archived jobs"),
      );
    const openJumper = async () => {
      await page.evaluate(() => document.getElementById("job-jump").click());
      await page.waitForSelector("#job-picker[open]", wait);
    };
    const count = () => page.$eval("#job-count", (el) => el.textContent);
    const waitMeta = (meta) =>
      page.waitForFunction(
        (m) =>
          [...document.querySelectorAll("#job-results .meta")].some((el) => el.textContent === m),
        wait,
        meta,
      );

    await t.test("a board without an archive shows no row", async () => {
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="415-add-export"]', wait);
      await openJumper();
      await page.type("#job-search", "login");
      assert.equal(await archiveOption(), undefined);
      assert.equal(await page.$eval("#job-empty", (el) => el.hidden), false);
      assert.equal(await page.$eval("#job-empty", (el) => el.textContent), "No jobs found");
      assert.deepEqual(asked, []);
    });

    await t.test("typing shows the row and sends nothing", async () => {
      archivedField = true;
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="415-add-export"]', wait);
      await openJumper();
      await page.type("#job-search", "login");
      assert.deepEqual(await options(), [
        {
          title: "Search archived jobs",
          meta: 'Command · Search the archive for "login"',
          selected: "true",
        },
      ]);
      assert.equal(
        await page.$eval("#job-section-archived", (el) => el.textContent),
        "Archived jobs",
      );
      assert.deepEqual(asked, []);
    });

    await t.test("one pick sends one request and lists the match", async () => {
      await page.keyboard.press("Enter");
      await page.waitForFunction(
        () =>
          [...document.querySelectorAll("#job-results .title")].some(
            (el) => el.textContent === "Fix login",
          ),
        wait,
      );
      assert.deepEqual(asked, ["login"]);
      assert.deepEqual(await options(), [
        {
          title: "Fix login",
          meta: "412-fix-login · Synthetic sample · merged · archived",
          selected: "true",
        },
      ]);
      assert.equal(await page.$eval("#job-count", (el) => el.textContent), "0 jobs · 1 archived");
      await new Promise((resolve) => setTimeout(resolve, 500));
      assert.equal(asked.length, 1);
    });

    await t.test("a changed query brings the row back", async () => {
      await page.type("#job-search", "s");
      await waitMeta('Command · Search the archive for "logins"');
      assert.equal(asked.length, 1);
      await page.keyboard.press("Backspace");
      await waitMeta("412-fix-login · Synthetic sample · merged · archived");
      assert.equal(asked.length, 1);
    });

    await t.test("Enter opens the match read-only", async () => {
      await page.keyboard.press("Enter");
      await page.waitForFunction(() => !document.getElementById("job-picker").open, wait);
      await page.waitForFunction(
        () => document.getElementById("d-meta").textContent.includes("412-fix-login"),
        wait,
      );
      assert.ok(
        (await page.$eval("#d-meta", (el) => el.textContent)).endsWith(" · archived, read-only"),
      );
      assert.deepEqual(
        await page.$$eval("#d-actions-menu button", (buttons) =>
          buttons.filter((b) => !b.hidden).map((b) => b.id),
        ),
        ["d-copy-link"],
      );
    });

    await t.test("zero matches say so", async () => {
      await openJumper();
      await page.type("#job-search", "zzz");
      const base = await count();
      assert.ok(!base.includes("archived"));
      await page.keyboard.press("Enter");
      await waitMeta('No archived jobs match "zzz". Pick to search again.');
      assert.equal(await count(), base + " · no archived matches");
      assert.equal(asked.at(-1), "zzz");
    });

    await t.test("an error says so", async () => {
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => !document.getElementById("job-picker").open, wait);
      await openJumper();
      await page.type("#job-search", "boom");
      const base = await count();
      assert.ok(!base.includes("archived"));
      await page.keyboard.press("Enter");
      await waitMeta("Could not search archived jobs. Pick to try again.");
      assert.equal(await count(), base + " · archive search failed");
      assert.equal(asked.at(-1), "boom");
    });

    assert.deepEqual(errors, []);
  });
});
