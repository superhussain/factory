// The "Show archived jobs" button under the merged and closed columns pages
// GET /api/board/archive?page=N (answered here by request interception) into one shared list.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
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
};
const RETRY = {
  id: "414-retry-sync",
  project: "sample",
  title: "Retry sync",
  stage: "closed",
  updated: "2026-11-04T12:00:00Z",
  done: "2026-11-04T12:00:00Z",
};
const row = (id, title, stage, done, extra = {}) => ({
  id,
  project: "sample",
  title,
  stage,
  ...extra,
  updated: done,
  done,
});
const ROWS = [
  row("416-rate-limit", "Rate limit", "merged", "2026-09-10T12:00:00Z"),
  row("413-old-suggestion", "Old suggestion", "closed", "2026-09-05T12:00:00Z"),
  row("412-fix-login", "Fix login", "merged", "2026-09-01T12:00:00Z", {
    round: 1,
    branch: "factory/412-fix-login",
    pr: "https://github.com/example/sample/pull/12",
  }),
  ...Array.from({ length: 49 }, (_, k) =>
    row(
      String(399 - k) + "-filler-" + k,
      "Filler " + k,
      k % 2 ? "closed" : "merged",
      new Date(Date.UTC(2026, 7, 30) - k * 86400000).toISOString(),
    ),
  ),
];
const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const wait = { polling: 50 };

test("archived jobs page under the terminal columns", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [EXPORT, RETRY]);
    const errors = [];
    const asked = [];
    let failNext = false;
    // Terminal columns start collapsed, so each page expands them before clicking inside.
    const openPage = async () => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 1280, height: 900 });
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        const url = new URL(request.url());
        if (url.pathname !== "/api/board/archive") return request.continue();
        asked.push(url.search);
        if (failNext) {
          failNext = false;
          return request.respond({ status: 500, contentType: "text/plain", body: "boom" });
        }
        const n = Number(url.searchParams.get("page"));
        if (n)
          return request.respond({
            status: 200,
            contentType: "application/json",
            body: JSON.stringify({
              jobs: ROWS.slice((n - 1) * 50, n * 50),
              page: n,
              pages: 2,
              total: 52,
            }),
          });
        const found = ROWS.find((j) => j.id === url.searchParams.get("id"));
        return request.respond(
          found
            ? {
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({ jobs: [found] }),
              }
            : { status: 404, contentType: "text/plain", body: "not found" },
        );
      });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="414-retry-sync"]', wait);
      for (const stage of ["merged", "closed"]) {
        await page.click(`.column-toggle[data-stage="${stage}"]`);
        await page.waitForSelector(`#column-cards-${stage}:not([hidden])`, wait);
      }
      return page;
    };
    const cards = (page, selector) => page.$$eval(selector, (els) => els.length);
    const page = await openPage();

    await t.test("the button sits under merged and closed only", async () => {
      assert.deepEqual(
        await page.$$eval(".archive-more", (els) =>
          els.map((b) => [b.dataset.stage, b.textContent]),
        ),
        [
          ["merged", "Show archived jobs"],
          ["closed", "Show archived jobs"],
        ],
      );
    });

    await t.test("the first click loads page 1", async () => {
      await page.click('.archive-more[data-stage="closed"]');
      await page.waitForFunction(
        () => document.querySelectorAll(".archive-list .card").length === 50,
        wait,
      );
      assert.equal(await cards(page, ".col.merged .archive-list .card"), 26);
      assert.equal(await cards(page, ".col.closed .archive-list .card"), 24);
      assert.deepEqual(
        await page.$$eval(".col.merged .archive-list .card", (els) =>
          els.slice(0, 2).map((c) => c.dataset.id),
        ),
        ["416-rate-limit", "412-fix-login"],
      );
      assert.equal(
        await page.$eval(".archive-count", (el) => el.textContent),
        "50 of 52 archived jobs loaded",
      );
      assert.ok(asked.includes("?page=1"));
      assert.deepEqual(
        await page.evaluate(() => {
          const el = document.activeElement;
          return [el.classList.contains("archive-more"), el.dataset.stage, el.textContent];
        }),
        [true, "closed", "Show more"],
      );
    });

    await t.test("show more loads page 2", async () => {
      await page.click('.archive-more[data-stage="closed"]');
      await page.waitForFunction(
        () => document.querySelectorAll(".archive-list .card").length === 52,
        wait,
      );
      assert.equal(await cards(page, ".col.merged .archive-list .card"), 27);
      assert.equal(await cards(page, ".col.closed .archive-list .card"), 25);
      assert.equal(await cards(page, ".archive-more"), 0);
      assert.equal(
        await page.$eval(".archive-count", (el) => el.textContent),
        "52 of 52 archived jobs loaded",
      );
    });

    await t.test("archived cards have their own style", async () => {
      const styles = await page.evaluate(() => {
        const css = (el, ...props) => props.map((p) => getComputedStyle(el)[p]);
        const card = (id) => document.querySelector(`.card[data-id="${id}"]`);
        const old = card("413-old-suggestion");
        const login = card("412-fix-login");
        const type = ["fontSize", "letterSpacing", "textTransform", "fontWeight", "color"];
        return {
          frame: css(old, "borderTopStyle", "borderTopColor", "backgroundColor", "boxShadow"),
          noPr: [
            css(old.querySelector(".id"), "paddingRight")[0],
            css(old.querySelector(".proj"), "paddingRight")[0],
          ],
          withPr: [
            css(login.querySelector(".id"), "paddingRight")[0],
            css(login.closest(".card-wrap").querySelector(".pr-link"), "right")[0],
          ],
          head: css(document.querySelector(".archive-head"), ...type),
          col: css(document.querySelector(".col.closed h2"), ...type),
        };
      });
      assert.deepEqual(styles.frame, ["dashed", "rgb(107, 114, 128)", "rgba(0, 0, 0, 0)", "none"]);
      assert.deepEqual(styles.noPr, ["0px", "0px"]);
      assert.deepEqual(styles.withPr, ["64px", "16px"]);
      assert.deepEqual(styles.head, styles.col);
    });

    await t.test("archived card states", async () => {
      const CARD = '.card[data-id="412-fix-login"]';
      const WRAP = `.card-wrap:has(> ${CARD})`;
      const shot = async (name) => {
        await page.mouse.move(5, 5);
        await (await page.$(WRAP)).screenshot({ path: `${SHOTS}${name}` });
      };
      const look = () =>
        page.$eval(CARD, (c) => [
          getComputedStyle(c).borderTopColor,
          getComputedStyle(c).boxShadow,
          getComputedStyle(c.parentElement.querySelector(".pr-link")).right,
        ]);
      const ACCENT = "rgb(124, 58, 237)";
      await shot("archived-card-1280.png");
      await page.evaluate(() => {
        openId = "412-fix-login";
        renderBoard();
      });
      await shot("archived-selected-1280.png");
      assert.deepEqual(await look(), [ACCENT, `${ACCENT} 0px 0px 0px 1px`, "16px"]);
      await page.evaluate(() => {
        openId = null;
        document.body.classList.add("compact");
        renderBoard();
      });
      await shot("archived-compact-1280.png");
      assert.deepEqual(await look(), ["rgb(107, 114, 128)", "none", "8px"]);
      await page.hover(CARD);
      // The .15s border-color transition has to finish before the colour reads as accent.
      await page.waitForFunction(
        (sel, c) => getComputedStyle(document.querySelector(sel)).borderTopColor === c,
        wait,
        CARD,
        ACCENT,
      );
      assert.equal((await look())[0], ACCENT);
      await page.mouse.move(5, 5);
      await page.evaluate(() => {
        document.body.classList.remove("compact");
        renderBoard();
      });
    });

    await t.test("a redraw keeps the list", async () => {
      const before = asked.length;
      await page.evaluate(() => renderBoard());
      assert.equal(await cards(page, ".archive-list .card"), 52);
      assert.equal(asked.length, before);
    });

    await t.test("an archived card opens read-only", async () => {
      assert.equal(
        await page.$eval('.card[data-id="412-fix-login"]', (c) =>
          c.closest(".card-wrap").querySelector(".card-actions"),
        ),
        null,
      );
      await page.click('.card[data-id="412-fix-login"]');
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

    await t.test("a failed page can be retried", async () => {
      const fresh = await openPage();
      failNext = true;
      await fresh.click('.archive-more[data-stage="closed"]');
      await fresh.waitForSelector(".archive-error", wait);
      assert.equal(
        await fresh.$eval(".archive-error", (el) => el.textContent),
        "Could not load archived jobs. Try again.",
      );
      assert.equal(
        await fresh.$eval('.archive-more[data-stage="closed"]', (el) => el.textContent),
        "Show archived jobs",
      );
      assert.equal(await cards(fresh, ".archive-error"), 1);
      assert.equal(await cards(fresh, ".col.closed .archive-error"), 1);
      assert.equal(
        await cards(
          fresh,
          ".archive-list [role=alert], .archive-list [role=status], .archive-list [aria-live]",
        ),
        0,
      );
      await fresh.waitForFunction(
        () =>
          document.getElementById("board-status").textContent ===
          "Could not load archived jobs. Try again.",
        wait,
      );
      await fresh.evaluate(() => {
        window.statusWrites = 0;
        new MutationObserver(() => window.statusWrites++).observe(
          document.getElementById("board-status"),
          { childList: true, characterData: true, subtree: true },
        );
        renderBoard();
        renderBoard();
      });
      await new Promise((resolve) => setTimeout(resolve, 300));
      assert.equal(await fresh.evaluate(() => window.statusWrites), 0);
      assert.equal(await cards(fresh, ".archive-error"), 1);
      await fresh.click('.archive-more[data-stage="closed"]');
      await fresh.waitForFunction(
        () => document.querySelectorAll(".archive-list .card").length === 50,
        wait,
      );
      assert.equal(await cards(fresh, ".archive-error"), 0);
    });

    assert.deepEqual(errors, []);
  });
});
