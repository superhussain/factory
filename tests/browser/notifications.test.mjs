// The notifications menu and page against one owned local Worker, with a synthetic board for
// project sample.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";
import { waitForBoard } from "../support/board-ready.cjs";

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
    paseo: { server: "srv-a", agent: "agent-60" },
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
// 22 older merged jobs, so JOBS plus OLD gives 25 items: 23 merged, 2 needs.
const OLD = Array.from({ length: 22 }, (_, i) => {
  const nn = String(i + 1).padStart(2, "0");
  return job(`1${nn}-sample-old`, "merged", { done: `2025-12-${nn}T00:00:00Z` });
});
// A pr-open job that does not wait on you, a job with a logged spec ready event and one
// logged backlog line, each newer than JOBS.
const KINDS = [
  job("080-sample-pr", "pr-open", { updated: "2026-01-06T00:00:00Z" }),
  job("073-sample-retry", "build"),
];
const BACKLOG_HASH = "0123456789ab";
const seedLog = (page) =>
  page.evaluateOnNewDocument((key) => {
    const ago = (min) => new Date(Date.now() - min * 60000).toISOString();
    try {
      localStorage.setItem(
        "factory-notifications-log",
        JSON.stringify([
          { kind: "spec", id: "073-sample-retry", at: ago(1) },
          {
            kind: "backlog",
            key,
            title: "Synthetic backlog idea",
            project: "sample",
            at: ago(2),
          },
        ]),
      );
    } catch {}
  }, BACKLOG_HASH);
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
    const openPage = async (hash = "", setup = null) => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 1280, height: 900 });
      if (setup) await setup(page);
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.goto(origin + "/" + hash, { waitUntil: "domcontentloaded" });
      return { page, errors };
    };
    const openMenu = async (page) => {
      await waitForBoard(page);
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
      await page.waitForFunction(
        () => document.querySelector("#notifications-count:not([hidden])")?.textContent === "3",
        wait,
      );
      assert.equal(
        await page.$eval("#notifications-button", (el) => el.getAttribute("aria-label")),
        "Notifications, 3 new",
      );
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
      const menu = await page.$eval("#notifications-menu", (el) => {
        const box = el.getBoundingClientRect();
        return { left: box.left, right: box.right, innerWidth };
      });
      assert.ok(
        menu.left >= 0 && menu.right <= menu.innerWidth,
        `menu off screen: ${JSON.stringify(menu)}`,
      );
      assert.equal(await page.$eval("#notifications-count", (el) => el.hidden), true);
      assert.deepEqual(errors, []);
    });

    await t.test("seen survives a reload", async () => {
      const { page, errors } = first;
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForSelector("#notifications-button[aria-label]", wait);
      await page.waitForFunction(() => document.querySelector(".card"), wait);
      assert.equal(await page.$eval("#notifications-count", (el) => el.hidden), true);
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

    await t.test("menu row keeps focus across a re-render", async () => {
      const { page, errors } = await openPage();
      await openMenu(page);
      await page.focus('#notifications-menu-list a[href="#060-sample-merged"]');
      await page.evaluate(() => renderBoard());
      assert.equal(
        await page.evaluate(() => document.activeElement.getAttribute("href")),
        "#060-sample-merged",
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("menu row keeps focus when its time label changes", async () => {
      const { page, errors } = await openPage();
      await openMenu(page);
      const row = '#notifications-menu-list a[href="#060-sample-merged"]';
      const meta = () => page.$eval(row + " .inbox-meta", (el) => el.textContent);
      const before = await meta();
      await page.focus(row);
      await page.evaluate(() => {
        jobs.find((j) => j.id === "060-sample-merged").done = new Date().toISOString();
        renderBoard();
      });
      assert.notEqual(await meta(), before);
      assert.deepEqual(
        await page.evaluate(() => [
          document.activeElement.getAttribute("href"),
          document.getElementById("notifications-menu").hidden,
        ]),
        ["#060-sample-merged", false],
      );
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

    await t.test("menu lists every kind", async () => {
      await seedBoard(origin, [...JOBS, ...KINDS]);
      const { page, errors } = await openPage("", seedLog);
      await openMenu(page);
      const rows = await page.$$eval("#notifications-menu-list .notification-link", (links) =>
        links.map((a) => [
          a.querySelector(".inbox-kind").textContent,
          a.querySelector(".inbox-title").textContent,
          a.getAttribute("href"),
          a.querySelector(".inbox-meta").textContent,
        ]),
      );
      assert.deepEqual(
        rows.slice(0, 3).map((row) => row.slice(0, 3)),
        [
          ["Spec ready", "Synthetic 073-sample-retry", "#073-sample-retry"],
          ["Backlog", "Synthetic backlog idea", "#/backlog/" + BACKLOG_HASH],
          ["PR open", "Synthetic 080-sample-pr", "#080-sample-pr"],
        ],
      );
      assert.match(rows[0][3], /^073-sample-retry · /);
      assert.doesNotMatch(rows[1][3], /·/);
      await page.click("#notifications-more");
      await pageRows(page, 6);
      assert.deepEqual(
        await page.$$eval("#notifications-list .inbox-item", (items) =>
          [...items[1].querySelectorAll(".inbox-actions a")].map((a) => [
            a.textContent,
            a.getAttribute("href"),
          ]),
        ),
        [["Open item", "#/backlog/" + BACKLOG_HASH]],
      );
      assert.equal(
        await page.$eval(
          "#notifications-list .inbox-item:nth-child(2) .inbox-meta",
          (el) => el.textContent.split(" · ")[0],
        ),
        "Synthetic sample",
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
        ["Open job", "Open PR #60", "Open in Session app", "Open in Session"],
      );
      assert.deepEqual(errors, []);
    });

    await t.test("row links", async () => {
      const { page, errors } = await openPage("#/notifications");
      await pageRows(page, 3);
      const actions = (id) =>
        page.$$eval(
          "#notifications-list .inbox-item",
          (items, id) =>
            [
              ...items
                .find((item) => item.querySelector(`a[href="#${id}"]`))
                .querySelectorAll(".inbox-actions a"),
            ].map((a) => [a.textContent, a.getAttribute("href"), a.getAttribute("target")]),
          id,
        );
      assert.deepEqual(await actions("060-sample-merged"), [
        ["Open job", "#060-sample-merged", null],
        ["Open PR #60", "https://github.com/example/sample/pull/60", "_blank"],
        ["Open in Session app", "session://h/srv-a/agent/agent-60", null],
        ["Open in Session", "https://sessions.example.test/srv-a/agent-60", "_blank"],
      ]);
      assert.deepEqual(
        (await actions("042-sample-export")).map((a) => a[0]),
        ["Open job", "Open PR #44"],
      );
      assert.deepEqual(
        (await actions("051-sample-retry")).map((a) => a[0]),
        ["Open job"],
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("row links honor hidden panel links", async () => {
      const { page, errors } = await openPage("#/notifications", (p) =>
        p.evaluateOnNewDocument(() =>
          localStorage.setItem("factory-hidden-drawer-links", "pr,web"),
        ),
      );
      await pageRows(page, 3);
      assert.deepEqual(
        await page.$$eval(
          '#notifications-list .inbox-item:has(a[href="#060-sample-merged"]) .inbox-actions a',
          (links) => links.map((a) => a.textContent),
        ),
        ["Open job", "Open in Session app"],
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    const ranks = (page) =>
      page.$$eval("#notifications-list .inbox-rank", (els) => els.map((el) => el.textContent));

    await t.test("page paginates at twenty", async () => {
      await seedBoard(origin, [...JOBS, ...OLD]);
      const { page, errors } = await openPage("#/notifications");
      await pageRows(page, 20);
      assert.deepEqual(
        await page.evaluate(() => [
          document.getElementById("notifications-pager").hidden,
          document.getElementById("notifications-page-label").textContent,
          document.getElementById("notifications-prev").disabled,
        ]),
        [false, "Page 1 of 2", true],
      );
      await page.click("#notifications-next");
      await pageRows(page, 5);
      assert.deepEqual(await ranks(page), ["21", "22", "23", "24", "25"]);
      assert.deepEqual(
        await page.evaluate(() => [
          document.getElementById("notifications-page-label").textContent,
          document.getElementById("notifications-next").disabled,
          document.activeElement.id,
        ]),
        ["Page 2 of 2", true, "notifications-prev"],
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
      await seedBoard(origin, JOBS);
    });

    await t.test("filters", async () => {
      await seedBoard(origin, [...JOBS, ...OLD]);
      const { page, errors } = await openPage("#/notifications");
      await pageRows(page, 20);
      const pressed = () =>
        page.$$eval("[data-notifications-filter]", (bs) =>
          bs.map((b) => [b.textContent, b.getAttribute("aria-pressed")]),
        );
      const kinds = () =>
        page.$$eval("#notifications-list .inbox-kind", (els) => [
          ...new Set(els.map((el) => el.textContent)),
        ]);
      await page.click("#notifications-next");
      await pageRows(page, 5);
      await page.click('[data-notifications-filter="merged"]');
      await page.waitForFunction(
        () => document.querySelector("#notifications-list .inbox-rank").textContent === "1",
        wait,
      );
      await pageRows(page, 20);
      assert.deepEqual(await kinds(), ["Merged"]);
      assert.deepEqual(await pressed(), [
        ["All", "false"],
        ["Backlog", "false"],
        ["Needs you", "false"],
        ["Spec ready", "false"],
        ["PR open", "false"],
        ["Merged", "true"],
      ]);
      assert.equal(
        await page.$eval("#notifications-page-label", (el) => el.textContent),
        "Page 1 of 2",
      );
      await page.click("#notifications-next");
      await pageRows(page, 3);
      await page.click('[data-notifications-filter="needs"]');
      await pageRows(page, 2);
      assert.deepEqual(await kinds(), ["Needs you"]);
      assert.equal(await page.$eval("#notifications-pager", (el) => el.hidden), true);
      await page.browserContext().close();
      await seedBoard(origin, [JOBS[0]]);
      const only = await openPage("#/notifications");
      await pageRows(only.page, 1);
      await only.page.click('[data-notifications-filter="needs"]');
      await pageRows(only.page, 0);
      assert.equal(
        await only.page.$eval("#notifications-status", (el) => el.textContent),
        "Nothing needs you right now.",
      );
      for (const [filter, copy] of [
        [
          "backlog",
          "No new backlog items yet. This list keeps the ones a tab in this browser saw.",
        ],
        ["spec", "No specs ready yet. This list keeps the ones a tab in this browser saw."],
        ["pr", "No open pull requests."],
      ]) {
        await only.page.click(`[data-notifications-filter="${filter}"]`);
        await only.page.waitForFunction(
          (c) => document.getElementById("notifications-status").textContent === c,
          wait,
          copy,
        );
      }
      assert.deepEqual(errors, []);
      assert.deepEqual(only.errors, []);
      await only.page.browserContext().close();
      await seedBoard(origin, [...JOBS, ...KINDS]);
      const every = await openPage("#/notifications", seedLog);
      await pageRows(every.page, 6);
      for (const [filter, chip] of [
        ["backlog", "Backlog"],
        ["spec", "Spec ready"],
        ["pr", "PR open"],
      ]) {
        await every.page.click(`[data-notifications-filter="${filter}"]`);
        await pageRows(every.page, 1);
        assert.deepEqual(
          await every.page.$$eval("#notifications-list .inbox-kind", (els) =>
            els.map((el) => el.textContent),
          ),
          [chip],
        );
      }
      assert.deepEqual(every.errors, []);
      await every.page.browserContext().close();
      await seedBoard(origin, JOBS);
    });

    await t.test("board load failure", async () => {
      let fail = true;
      let hold = null;
      const failBoard = async (p) => {
        await blockLiveEvents(p);
        await p.setBypassServiceWorker(true);
        await p.setRequestInterception(true);
        p.on("request", async (request) => {
          if (
            !fail ||
            request.method() !== "GET" ||
            new URL(request.url()).pathname !== "/board.json"
          )
            return request.continue();
          await hold;
          return request.respond({ status: 503, body: "" });
        });
      };
      const { page, errors } = await openPage("#/notifications", failBoard);
      const copy =
        "Couldn't load notifications. Can't reach the factory. Check your connection, then retry.";
      const status = (p, id, text) =>
        p.waitForFunction((i, c) => document.getElementById(i).textContent === c, wait, id, text);
      await status(page, "notifications-status", copy);
      assert.equal(await page.$eval("#notifications-retry", (el) => el.hidden), false);
      await page.click("#notifications-button");
      assert.deepEqual(
        await page.evaluate(() => [
          document.getElementById("notifications-menu-status").textContent,
          document.getElementById("notifications-menu-retry").hidden,
        ]),
        [copy, false],
      );
      await page.keyboard.press("Escape");
      let release;
      hold = new Promise((resolve) => (release = resolve));
      await page.click("#notifications-retry");
      await status(page, "notifications-status", "Retrying\u2026");
      assert.equal(await page.$eval("#notifications-retry", (el) => el.disabled), true);
      release();
      hold = null;
      await status(page, "notifications-status", copy);
      await page.waitForFunction(() => document.activeElement.id === "notifications-retry", wait);
      assert.equal(await page.$eval("#notifications-retry", (el) => el.disabled), false);
      fail = false;
      await page.click("#notifications-retry");
      await pageRows(page, 3);
      await page.waitForFunction(
        () => document.activeElement.id === "notifications-page-heading",
        wait,
      );
      assert.equal(await page.$eval("#notifications-retry", (el) => el.hidden), true);
      assert.deepEqual(errors, []);
      await page.browserContext().close();

      fail = true;
      const menu = await openPage("", failBoard);
      await menu.page.click("#notifications-button");
      await status(menu.page, "notifications-menu-status", copy);
      fail = false;
      await menu.page.click("#notifications-menu-retry");
      await menu.page.waitForFunction(
        () => document.activeElement.matches("#notifications-menu-list a"),
        wait,
      );
      assert.deepEqual(
        await menu.page.evaluate(() => [
          document.getElementById("notifications-menu").hidden,
          document.getElementById("notifications-menu-retry").hidden,
        ]),
        [false, true],
      );
      assert.deepEqual(menu.errors, []);
      await menu.page.browserContext().close();
    });

    await t.test("open job returns to the page", async () => {
      const { page, errors } = list;
      await pageRows(page, 3);
      await page.$$eval("#notifications-list .inbox-item", (items) =>
        items[1].querySelector('a[href="#051-sample-retry"]').click(),
      );
      await page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("051-sample-retry"),
        wait,
      );
      const split = await page.evaluate(() => {
        const list = document.getElementById("notifications-page");
        const detail = document.getElementById("detail");
        return {
          view: document.body.dataset.view,
          split: document.body.hasAttribute("data-notifications-split"),
          visible: [list.checkVisibility(), detail.checkVisibility()],
          beside: list.getBoundingClientRect().right <= detail.getBoundingClientRect().left + 1,
          current: [...document.querySelectorAll("#notifications-list .inbox-item")].map((li) =>
            li.getAttribute("aria-current"),
          ),
        };
      });
      assert.deepEqual(split, {
        view: "notifications",
        split: true,
        visible: [true, true],
        beside: true,
        current: [null, "true", null],
      });
      await page.click("#d-close");
      await page.waitForFunction(() => location.hash === "#/notifications", wait);
      assert.equal(await page.$eval("#notifications-page", (el) => el.checkVisibility()), true);
      assert.deepEqual(
        await page.evaluate(() => [
          document.body.hasAttribute("data-notifications-split"),
          document.activeElement.matches('#notifications-list a[href="#051-sample-retry"]'),
        ]),
        [false, true],
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("h leaves the page", async () => {
      const { page, errors } = await openPage("#/notifications");
      await pageRows(page, 3);
      await page.keyboard.press("h");
      await page.waitForFunction(() => location.hash === "", wait);
      await page.waitForFunction(() => document.body.dataset.view === "board", wait);
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
        "No notifications yet. New backlog items, ready specs, open pull requests, merged jobs and jobs waiting on you show up here.",
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });
  });
});
