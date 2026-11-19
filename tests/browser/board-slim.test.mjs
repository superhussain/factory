// The dashboard reads GET /board.json?view=slim and reads /api/board only once a view needs
// older finished rows. Rows are made up: 2 active and 30 merged, 500 the oldest.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const wait = { polling: 50 };
const at = (n) => new Date(Date.UTC(2026, 0, 1) + n * 3600000).toISOString();
const MERGED = Array.from({ length: 30 }, (_, n) => ({
  id: `${500 + n}-synthetic-merged`,
  project: "sample",
  title: `Synthetic merged ${500 + n}`,
  stage: "merged",
  updated: at(n),
  done: at(n),
}));
const ACTIVE = [
  {
    id: "600-synthetic-build",
    project: "sample",
    title: "Synthetic build",
    stage: "build",
    updated: at(40),
    refs: [{ kind: "follow-up-of", id: "500-synthetic-merged" }],
  },
  {
    id: "601-synthetic-spec",
    project: "sample",
    title: "Synthetic spec",
    stage: "spec",
    updated: at(41),
  },
];

async function openPage(browser, origin, hash = "") {
  const page = await browser.newPage();
  await page.setViewport({ width: 1440, height: 900 });
  const reads = [];
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (url.pathname === "/board.json" || url.pathname === "/api/board")
      reads.push(request.method() + " " + url.pathname + url.search);
  });
  await controlBoardPolling(page);
  await page.goto(origin + "/" + hash, { waitUntil: "domcontentloaded" });
  await waitForBoard(page);
  return { page, reads, errors };
}

const fullReads = (reads) => reads.filter((r) => r === "GET /api/board").length;
const mergedCards = (page) => page.$$eval(".col.merged .card", (cards) => cards.length);
const text = (page, selector) => page.$eval(selector, (el) => el.textContent);

test("slim board read", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [...ACTIVE, ...MERGED]);

    await t.test("a plain load reads the slim board only", async () => {
      const { page, reads, errors } = await openPage(browser, origin);
      assert.ok(reads.includes("GET /board.json?view=slim"), reads.join(", "));
      assert.equal(fullReads(reads), 0);
      assert.equal(await mergedCards(page), 10);
      assert.equal(await text(page, ".col.merged .count-wide"), "showing 10 of 30");
      assert.equal(await text(page, ".col.merged .count-narrow"), "10 / 30");
      await clickNow(page, '.column-toggle[data-stage="merged"]');
      assert.equal(await text(page, '.more[data-stage="merged"]'), "show all");
      // A ref to a finished row outside the slim window stays a link.
      assert.ok(
        await page.$(
          '.card[data-id="600-synthetic-build"] .ref-link[data-ref="500-synthetic-merged"]',
        ),
      );

      // A failed full read hands the button back and says so.
      let failNext = true;
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        if (request.isInterceptResolutionHandled()) return;
        const url = new URL(request.url());
        if (failNext && request.method() === "GET" && url.pathname === "/api/board") {
          failNext = false;
          return request.respond({ status: 500, contentType: "text/plain", body: "boom" });
        }
        return request.continue();
      });
      await clickNow(page, '.more[data-stage="merged"]');
      await page.waitForFunction(
        () =>
          document.getElementById("board-status").textContent ===
          "Could not load merged and closed jobs. Try again.",
        wait,
      );
      assert.deepEqual(
        await page.$eval('.more[data-stage="merged"]', (el) => [
          el.textContent,
          el.getAttribute("aria-disabled"),
        ]),
        ["show all", null],
      );
      assert.equal(await mergedCards(page), 10);
      assert.equal(fullReads(reads), 1);

      const loading = await page.evaluate(() => {
        document.querySelector('.more[data-stage="merged"]').click();
        const more = document.querySelector('.more[data-stage="merged"]');
        return [
          more.textContent,
          more.getAttribute("aria-disabled"),
          more.getAttribute("aria-busy"),
          more.getAttribute("aria-label"),
        ];
      });
      assert.deepEqual(loading, ["show all", "true", "true", "show all 30 merged"]);
      await page.waitForFunction(
        () => document.querySelectorAll(".col.merged .card").length === 30,
        wait,
      );
      assert.equal(fullReads(reads), 2);
      assert.equal(await page.$(".col.merged .count-wide"), null);
      assert.equal(
        await page.$eval(
          '.column-toggle[data-stage="merged"] > span:nth-child(2)',
          (el) => el.textContent,
        ),
        "30",
      );
      assert.equal(await text(page, '.more[data-stage="merged"]'), "show 10");

      const before = reads.length;
      await seedBoard(origin, [
        { ...ACTIVE[0], title: "Synthetic build, renamed" },
        ACTIVE[1],
        ...MERGED,
      ]);
      await page.evaluate(() => poll({ ifChanged: true }));
      await page.waitForFunction(
        () =>
          document.querySelector('.card[data-id="600-synthetic-build"] .title')?.textContent ===
          "Synthetic build, renamed",
        wait,
      );
      assert.deepEqual(reads.slice(before), ["GET /board.json?view=slim"]);
      assert.equal(await mergedCards(page), 30);
      assert.deepEqual(errors, []);
      await page.close();
    });

    await t.test("a deep link opens the oldest merged row from the board", async () => {
      const { page, errors } = await openPage(browser, origin, "#500-synthetic-merged");
      await page.waitForFunction(
        () => document.getElementById("d-title").textContent === "Synthetic merged 500",
        wait,
      );
      assert.ok(!(await text(page, "#d-meta")).includes("archived, read-only"));
      assert.deepEqual(errors, []);
      await page.close();
    });

    await t.test("the jumper finds the oldest merged row by title", async () => {
      const { page, reads, errors } = await openPage(browser, origin);
      await page.evaluate(() => document.getElementById("job-jump").click());
      await page.waitForSelector("#job-picker[open]", wait);
      await page.type("#job-search", "Synthetic merged 500");
      await page.waitForFunction(
        () =>
          [...document.querySelectorAll("#job-results .title")].some(
            (el) => el.textContent === "Synthetic merged 500",
          ),
        wait,
      );
      assert.equal(fullReads(reads), 1);
      assert.deepEqual(errors, []);
      await page.close();
    });

    await t.test("a 160px column with a 4-digit total keeps the stage name", async () => {
      const { page, errors } = await openPage(browser, origin);
      await clickNow(page, '.column-toggle[data-stage="merged"]');
      // Probe values: the merged column at 160px, as many merged rows as production has.
      const name = () =>
        page.evaluate(() => {
          const span = document.querySelector('.column-toggle[data-stage="merged"] > span');
          const more = document.querySelector('.more[data-stage="merged"]');
          return {
            shown: span.clientWidth,
            full: span.scrollWidth,
            more: more.textContent,
            busy: more.getAttribute("aria-busy"),
            title: span.parentElement.title,
          };
        });
      await page.evaluate(() => {
        const style = document.createElement("style");
        style.textContent = ".col.merged { width: 160px; max-width: 160px; }";
        document.head.append(style);
        slimCounts.sample.merged = 1763;
        renderBoard();
        window.fetch = new Proxy(window.fetch, {
          apply: (fetchNow, self, [url, options]) =>
            String(url).endsWith("api/board") && !options?.method
              ? new Promise(() => {})
              : fetchNow.call(self, url, options),
        });
      });
      assert.equal(await text(page, ".col.merged .count-narrow"), "10 / 1763");
      const idle = await name();
      await clickNow(page, '.more[data-stage="merged"]');
      const loading = await name();
      for (const [state, n] of [
        ["idle", idle],
        ["loading", loading],
      ]) {
        assert.ok(n.shown * 2 >= n.full, `${state}: name ${n.shown} of ${n.full}px`);
        assert.equal(n.title, "merged");
        assert.equal(n.more, "show all");
      }
      assert.equal(idle.busy, null);
      assert.equal(loading.busy, "true");
      assert.deepEqual(errors, []);
      await page.close();
    });
  });
});
