// Notification toasts for new board and backlog events, against one owned local Worker.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";

const job = (id, title, stage, extra = {}) => ({
  id,
  project: "sample",
  title,
  stage,
  updated: "2026-01-01T00:00:00Z",
  ...extra,
});
const RETRY = job("073-sample-retry", "Synthetic retry limit", "spec");
const RETRY_WAITS = { ...RETRY, stage: "needs-human", note: "gate: spec-approval" };
const BURST = ["074-sample-a", "075-sample-b", "076-sample-c", "077-sample-d", "078-sample-e"].map(
  (id) => job(id, "Synthetic burst " + id, "build"),
);
const UNSAFE = job("079-sample-unsafe", '<img src=x onerror="window.injected=1">', "build");
const EXISTING = "- [ ] sample: Synthetic existing idea\n";
const IDEA = "- [ ] sample: Synthetic backlog idea";
const wait = { polling: 50 };

async function seedBacklog(origin, text) {
  const current = await fetch(origin + "/api/backlog");
  const condition = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  await current.text();
  const response = await fetch(origin + "/api/backlog", {
    method: "PUT",
    headers: { "Content-Type": "text/markdown", ...condition },
    body: text,
  });
  assert.equal(response.status, 200);
}

test("notification toasts", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [RETRY, ...BURST, UNSAFE]);
    await seedBacklog(origin, EXISTING);
    const page = await (await browser.createBrowserContext()).newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);

    const toasts = () =>
      page.$$eval("#notify-toasts .notify-toast", (els) =>
        els.map((li) => ({
          chip: li.querySelector(".inbox-kind").textContent,
          title: li.querySelector(".inbox-title").textContent,
          meta: li.querySelector(".inbox-meta").textContent,
          href: li.querySelector("a.notification-link").getAttribute("href"),
        })),
      );
    const clear = () =>
      page.evaluate(() => document.getElementById("notify-toasts").replaceChildren());
    const focused = () =>
      page.evaluate(() => {
        const el = document.activeElement;
        const meta = el.closest(".notify-toast")?.querySelector(".inbox-meta").textContent;
        return meta ? "close " + meta : el.id;
      });
    const storage = () => page.evaluate(() => localStorage.getItem("factory-alerts-off"));
    const toSettings = async () => {
      await page.evaluate(() => {
        location.hash = "#/settings/notifications";
      });
      await page.waitForSelector("#settings-notifications:not([hidden])", wait);
    };

    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => toastSeen !== null && backlogSeen !== null, wait);

    await t.test("toast T1 spec ready names the job and links to it", async () => {
      assert.deepEqual(await toasts(), []);
      await seedBoard(origin, [RETRY_WAITS, ...BURST, UNSAFE]);
      await page.evaluate(() => poll({ withBacklog: false }));
      assert.deepEqual(await toasts(), [
        {
          chip: "Spec ready",
          title: "Synthetic retry limit",
          meta: "073-sample-retry",
          href: "#073-sample-retry",
        },
      ]);
      assert.deepEqual(
        await page.$eval(".notify-toast-close", (el) => [
          el.getAttribute("aria-label"),
          el.querySelector("svg path").getAttribute("d"),
        ]),
        ["Dismiss notification", "M4 4l8 8M12 4l-8 8"],
      );
      await page.focus("#notify-toasts a.notification-link");
      await page.keyboard.press("Enter");
      await page.waitForFunction(() => location.hash === "#073-sample-retry", wait);
      await page.waitForFunction(() => !document.querySelector(".notify-toast"), wait);
      assert.equal(await focused(), "d-close");
      await page.evaluate(() => {
        location.hash = "";
      });
      await page.waitForFunction(() => document.getElementById("detail").hidden, wait);
    });

    await t.test("toast T2 a new backlog line toasts; the first load did not", async () => {
      assert.deepEqual(await toasts(), []);
      await seedBacklog(origin, EXISTING + IDEA + "\n");
      await page.evaluate(() => refreshBacklog());
      const key = await page.evaluate((raw) => backlog.find((b) => b.raw === raw).key, IDEA);
      assert.deepEqual(await toasts(), [
        {
          chip: "Backlog",
          title: "Synthetic backlog idea",
          meta: "Synthetic sample",
          href: "#/backlog/" + key,
        },
      ]);
      await page.evaluate(() => refreshBacklog());
      assert.equal((await toasts()).length, 1);
      await clear();
    });

    await t.test("toast T3 five events keep three; close and timeout remove", async () => {
      await seedBoard(origin, [
        RETRY_WAITS,
        ...BURST.map((j) => ({ ...j, stage: "needs-human" })),
        UNSAFE,
      ]);
      await page.evaluate(() => poll({ withBacklog: false }));
      assert.deepEqual(
        (await toasts()).map((x) => [x.chip, x.meta]),
        [
          ["Needs you", "076-sample-c"],
          ["Needs you", "077-sample-d"],
          ["Needs you", "078-sample-e"],
        ],
      );
      await page.focus(".notify-toast:nth-child(2) .notify-toast-close");
      await page.keyboard.press("Enter");
      await page.waitForFunction(
        () => document.querySelectorAll(".notify-toast").length === 2,
        wait,
      );
      assert.equal(await focused(), "close 078-sample-e");
      await page.mouse.move(0, 0);
      await page.evaluate(() => document.activeElement.blur());
      const started = Date.now();
      await page.waitForFunction(() => !document.querySelector(".notify-toast"), {
        ...wait,
        timeout: 20000,
      });
      assert.ok(Date.now() - started >= 9000 - 2000);
      assert.equal(await page.evaluate(() => TOAST_MS), 10000);
    });

    await t.test("toast T4 per-event settings pick the toast kind", async () => {
      await seedBoard(origin, [RETRY, ...BURST, UNSAFE]);
      await page.evaluate(() => poll({ withBacklog: false }));
      await clear();
      await toSettings();
      await page.click("#alert-spec");
      assert.equal(await storage(), "spec");
      await seedBoard(origin, [RETRY_WAITS, ...BURST, UNSAFE]);
      await page.evaluate(() => poll({ withBacklog: false }));
      assert.deepEqual(
        (await toasts()).map((x) => [x.chip, x.meta]),
        [["Needs you", "073-sample-retry"]],
      );
      await clear();
      await page.click("#alert-needs");
      assert.equal(await storage(), "spec,needs");
      assert.equal(
        await page.$eval("#alert-status", (el) => el.textContent),
        "Toast settings saved for this browser.",
      );
      await seedBoard(origin, [RETRY, ...BURST, UNSAFE]);
      await page.evaluate(() => poll({ withBacklog: false }));
      await seedBoard(origin, [RETRY_WAITS, ...BURST, UNSAFE]);
      await page.evaluate(() => poll({ withBacklog: false }));
      assert.deepEqual(await toasts(), []);
      await page.click("#alert-spec");
      await page.click("#alert-needs");
      assert.equal(await storage(), null);
      await page.evaluate(() => {
        location.hash = "";
      });
      await page.waitForSelector("#settings-notifications", { hidden: true });
    });

    await t.test("toast T5 board text renders as text", async () => {
      await seedBoard(origin, [RETRY_WAITS, ...BURST, { ...UNSAFE, stage: "needs-human" }]);
      await page.evaluate(() => poll({ withBacklog: false }));
      assert.deepEqual(
        (await toasts()).map((x) => x.title),
        ['<img src=x onerror="window.injected=1">'],
      );
      assert.equal(await page.$("#notify-toasts img"), null);
      assert.equal(await page.evaluate(() => window.injected), undefined);
      await page.focus(".notify-toast-close");
      await page.keyboard.press("Enter");
      assert.deepEqual(await toasts(), []);
      assert.equal(await focused(), "notifications-button");
    });

    assert.deepEqual(errors, []);
  });
});
