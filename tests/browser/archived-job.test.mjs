// Jobs the Worker archived off the board: a link opens one read-only from
// GET /api/board/archive (answered here by request interception), rework history walks
// archived rounds, and new ids start above the board's archived.max_id.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const EXPORT = {
  id: "415-add-export",
  project: "sample",
  title: "Add export",
  stage: "merged",
  updated: "2026-11-05T12:00:00Z",
  done: "2026-11-05T12:00:00Z",
  refs: [{ id: "412-fix-login", kind: "follow-up-of" }],
};
const ARCHIVE = [
  {
    id: "412-fix-login",
    project: "sample",
    title: "Fix login",
    stage: "merged",
    round: 1,
    branch: "factory/412-fix-login",
    updated: "2026-09-01T12:00:00Z",
    done: "2026-09-01T12:00:00Z",
  },
  {
    id: "410-retry-sync",
    project: "sample",
    title: "Retry sync",
    stage: "closed",
    idea: "Retry the sync twice",
    updated: "2026-08-20T12:00:00Z",
    done: "2026-08-20T12:00:00Z",
    backlog: { decision: "rework", at: "2026-08-20T12:00:00Z", notes: "Use a queue." },
    refs: [{ id: "409-retry-sync", kind: "rework-of" }],
  },
  {
    id: "409-retry-sync",
    project: "sample",
    title: "Retry sync",
    stage: "closed",
    idea: "Retry the sync",
    updated: "2026-08-10T12:00:00Z",
    done: "2026-08-10T12:00:00Z",
    backlog: { decision: "rework", at: "2026-08-10T12:00:00Z", notes: "Say how often." },
  },
  {
    id: "411-sync-retry",
    project: "sample",
    title: "Sync retry",
    stage: "merged",
    round: 1,
    branch: "factory/411-sync-retry",
    updated: "2026-08-25T12:00:00Z",
    done: "2026-08-25T12:00:00Z",
  },
];
const wait = { polling: 50 };
const FAILED = "Could not load archived job 411-sync-retry. Retrying while the link stays open.";

test("archived jobs open read-only", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [EXPORT]);
    const put = (path, body, type) =>
      fetch(origin + path, {
        method: "PUT",
        headers: { "If-None-Match": "*", "Content-Type": type },
        body,
      });
    assert.equal(
      (
        await put(
          "/api/jobs/sample/412-fix-login/spec.md",
          "# Fix login\n\nSynthetic spec.\n",
          "text/markdown",
        )
      ).status,
      200,
    );
    assert.equal(
      (
        await put(
          "/api/backlog",
          "# Backlog\n\n- [ ] sample: Retry the sync with a queue (rework-of 410-retry-sync)\n",
          "text/markdown",
        )
      ).status,
      200,
    );
    const page = await (await browser.createBrowserContext()).newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const asked = [];
    let syncRetryUp = false;
    await page.setRequestInterception(true);
    page.on("request", (request) => {
      const url = new URL(request.url());
      if (url.pathname !== "/api/board/archive") return request.continue();
      const id = url.searchParams.get("id");
      asked.push(id);
      if (id === "411-sync-retry" && !syncRetryUp)
        return request.respond({ status: 503, contentType: "text/plain", body: "unavailable" });
      const row = ARCHIVE.find((j) => j.id === id);
      return request.respond(
        row
          ? { status: 200, contentType: "application/json", body: JSON.stringify({ jobs: [row] }) }
          : { status: 404, contentType: "text/plain", body: "not found" },
      );
    });
    const statusHidden = () => page.$eval("#board-status", (el) => el.hidden);
    const until = async (done) => {
      for (let i = 0; i < 200 && !done(); i++)
        await new Promise((resolve) => setTimeout(resolve, 50));
      assert.ok(done());
    };
    const meta = () => page.$eval("#d-meta", (el) => el.textContent);
    const shownActions = () =>
      page.$$eval("#d-actions-menu button", (buttons) =>
        buttons.filter((b) => !b.hidden).map((b) => b.id),
      );

    await t.test("a link to an archived job opens it read-only", async () => {
      await page.goto(origin + "/#412-fix-login", { waitUntil: "domcontentloaded" });
      await page.waitForFunction(
        () => document.getElementById("d-meta").textContent.includes("412-fix-login"),
        wait,
      );
      assert.equal(
        await meta(),
        "Synthetic sample · 412-fix-login · merged · round 1 · factory/412-fix-login · archived, read-only",
      );
      assert.equal(await page.$eval("#d-title", (el) => el.textContent), "Fix login");
      assert.deepEqual(await shownActions(), ["d-copy-link"]);
      assert.equal(await page.$eval("#d-actions-wrap", (el) => el.hidden), false);
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Fix login",
        wait,
      );
      assert.equal(await page.$('.card[data-id="412-fix-login"]'), null);
    });

    await t.test("a ref link on a board job opens the archived job", async () => {
      await page.evaluate(() => {
        location.hash = "#415-add-export";
      });
      await page.waitForFunction(
        () => document.getElementById("d-meta").textContent.includes("415-add-export"),
        wait,
      );
      assert.equal((await meta()).includes("archived"), false);
      assert.equal((await shownActions()).includes("d-follow-up"), true);
      await page.click('#d-refs a[href="#412-fix-login"]');
      await page.waitForFunction(
        () => document.getElementById("d-meta").textContent.includes("412-fix-login"),
        wait,
      );
      assert.ok((await meta()).endsWith(" · archived, read-only"));
      assert.deepEqual(await shownActions(), ["d-copy-link"]);
    });

    await t.test("an id in neither place is fetched once", async () => {
      await page.evaluate(() => {
        location.hash = "#999-missing";
      });
      await until(() => asked.includes("999-missing"));
      await page.evaluate(() => poll());
      await page.evaluate(() => poll());
      await new Promise((resolve) => setTimeout(resolve, 500));
      assert.equal(asked.filter((id) => id === "999-missing").length, 1);
      assert.equal((await meta()).includes("999-missing"), false);
      assert.equal(await statusHidden(), true);
    });

    await t.test("a tab change on the same failing id stays quiet", async () => {
      await page.evaluate(() => {
        location.hash = "#411-sync-retry";
      });
      await page.waitForFunction(
        (text) => document.getElementById("board-status").textContent === text,
        wait,
        FAILED,
      );
      await page.evaluate(() => showBoardStatus(""));
      const tries = asked.filter((id) => id === "411-sync-retry").length;
      await page.evaluate(() => {
        location.hash = "#411-sync-retry/spec.md";
      });
      await until(() => asked.filter((id) => id === "411-sync-retry").length > tries);
      await new Promise((resolve) => setTimeout(resolve, 300));
      assert.equal(await statusHidden(), true);
      await page.evaluate(() => {
        location.hash = "";
      });
    });

    await t.test("a failed archived load says so and retries", async () => {
      await page.evaluate(() => {
        location.hash = "#411-sync-retry";
      });
      await page.waitForFunction(
        (text) => document.getElementById("board-status").textContent === text,
        wait,
        FAILED,
      );
      assert.equal((await meta()).includes("411-sync-retry"), false);

      await page.evaluate(() => showBoardStatus(""));
      const tries = asked.filter((id) => id === "411-sync-retry").length;
      await page.evaluate(() => poll());
      await until(() => asked.filter((id) => id === "411-sync-retry").length > tries);
      await new Promise((resolve) => setTimeout(resolve, 300));
      assert.equal(await statusHidden(), true);

      await page.evaluate(() => {
        location.hash = "";
      });
      await page.evaluate(() => {
        location.hash = "#411-sync-retry";
      });
      await page.waitForFunction(
        (text) => document.getElementById("board-status").textContent === text,
        wait,
        FAILED,
      );

      syncRetryUp = true;
      await page.evaluate(() => poll());
      await page.waitForFunction(
        () => document.getElementById("d-meta").textContent.includes("411-sync-retry"),
        wait,
      );
      assert.ok((await meta()).endsWith(" · archived, read-only"));
    });

    await t.test("rework history walks archived rounds", async () => {
      await page.evaluate(() => {
        location.hash = "";
      });
      await page.waitForSelector(".backlog-card", wait);
      await page.click(".backlog-card");
      await page.waitForSelector("#backlog-form", wait);
      await page.waitForSelector("#backlog-history:not([hidden])", wait);
      assert.deepEqual(
        await page.$$eval("#backlog-history li .meta a", (links) =>
          links.map((a) => a.textContent),
        ),
        ["410-retry-sync", "409-retry-sync"],
      );
      assert.deepEqual(
        await page.$$eval("#backlog-history li p", (ps) => ps.map((p) => p.textContent)),
        [
          "Earlier text: Retry the sync twice",
          "Notes: Use a queue.",
          "Earlier text: Retry the sync",
          "Notes: Say how often.",
        ],
      );
    });

    await t.test("new ids start above archived.max_id", async () => {
      assert.equal(
        await page.evaluate(() =>
          nextJobId({ jobs: [{ id: "415-add-export" }], archived: { max_id: 416 } }, "x"),
        ),
        "417-x",
      );
      assert.equal(
        await page.evaluate(() =>
          nextJobId({ jobs: [{ id: "420-later" }], archived: { max_id: 416 } }, "x"),
        ),
        "421-x",
      );
      assert.equal(
        await page.evaluate(() => nextJobId({ jobs: [{ id: "415-add-export" }] }, "x")),
        "416-x",
      );
    });

    assert.deepEqual(errors, []);
  });
});
