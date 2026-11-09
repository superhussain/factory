// Notifications section and the Display card details and New idea project additions,
// against one owned local Worker. A stub Notification class stands in for the browser's.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";

const NOTE = { branch: "factory/001-sample", round: 1, note: "Synthetic note" };
const JOBS = [
  { id: "001-sample", project: "sample", title: "Synthetic sample job", stage: "spec", ...NOTE },
  {
    id: "004-sample-pr",
    project: "sample",
    title: "Synthetic PR job",
    stage: "pr-open",
    pr: "https://github.com/example/sample/pull/7",
    ...NOTE,
  },
  {
    id: "005-sample-needs",
    project: "sample",
    title: "Synthetic needs job",
    stage: "build",
    needs: "Approve the spec",
  },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));
const wait = { polling: 50 };

test("notification settings", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedProject(
      origin,
      "other",
      "---\nrepo: example/other\nbase: main\ntitle: Synthetic other\n---\n",
    );
    await seedBoard(origin, JOBS);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.evaluateOnNewDocument(() => {
      const interval = window.setInterval;
      const clear = window.clearInterval;
      window.pollTimers = new Map();
      window.setInterval = (fn, ms, ...args) => {
        const id = interval(fn, ms, ...args);
        if (fn.name === "poll") window.pollTimers.set(id, ms);
        return id;
      };
      window.clearInterval = (id) => {
        window.pollTimers.delete(id);
        return clear(id);
      };
    });
    await page.evaluateOnNewDocument(() => {
      if (sessionStorage.getItem("no-notify")) {
        delete window.Notification;
        return;
      }
      window.shown = [];
      window.Notification = class {
        static permission = "default";
        static async requestPermission() {
          this.permission = window.__grant;
          return this.permission;
        }
        constructor(title, { body, tag }) {
          window.shown.push({ title, body, tag, instance: this });
        }
        close() {}
      };
    });

    const liveTimers = () => page.evaluate(() => [...window.pollTimers.values()]);
    const setHidden = (hidden) =>
      page.evaluate((value) => {
        Object.defineProperty(document, "hidden", { configurable: true, get: () => value });
        document.dispatchEvent(new Event("visibilitychange"));
      }, hidden);
    const storage = (key) => page.evaluate((k) => localStorage.getItem(k), key);
    const setStorage = (key, value) =>
      page.evaluate((k, v) => localStorage.setItem(k, v), key, value);
    const text = (selector) => page.$eval(selector, (el) => el.textContent);
    const toHash = async (hash, selector) => {
      await page.evaluate((h) => {
        location.hash = h;
      }, hash);
      await page.waitForSelector(selector, wait);
    };
    const reload = async (selector) => {
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForSelector(selector, wait);
      await page.waitForFunction(() => window.pollTimers.size > 0, wait);
    };
    const shown = () =>
      page.evaluate(() => window.shown.map(({ title, body, tag }) => ({ title, body, tag })));

    await page.goto(origin + "/#/settings/notifications", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#settings-notifications:not([hidden])", wait);
    await page.waitForFunction(() => window.pollTimers.size > 0, wait);

    await t.test("notify N1 route, nav and focus", async () => {
      assert.equal(await page.$eval("#settings-display", (el) => el.hidden), true);
      assert.equal(
        await page.$eval("#settings-nav-notifications", (el) => el.getAttribute("aria-current")),
        "page",
      );
      assert.equal(
        await page.$eval("#settings-nav-display", (el) => el.getAttribute("aria-current")),
        null,
      );
      await page.waitForFunction(() => document.activeElement?.id === "notify-needs", wait);
    });

    await t.test("notify N2 denied and granted prompts", async () => {
      await page.evaluate(() => {
        window.__grant = "denied";
      });
      await page.click("#notify-needs");
      await page.waitForFunction(
        () => document.getElementById("notify-status").textContent === "Notifications are off.",
        wait,
      );
      assert.equal(await page.$eval("#notify-needs", (el) => el.checked), false);
      assert.equal(await storage("factory-notify"), null);
      assert.equal(
        await text("#notify-permission"),
        "Notifications are blocked for this site. Allow them in your browser's site settings, then turn this on again.",
      );
      await page.evaluate(() => {
        window.Notification.permission = "default";
        window.__grant = "granted";
      });
      await page.click("#notify-needs");
      await page.waitForFunction(
        () =>
          document.getElementById("notify-status").textContent ===
          "Notifications are on in this browser.",
        wait,
      );
      assert.equal(await storage("factory-notify"), "on");
      assert.equal(await page.$eval("#notify-needs", (el) => el.checked), true);
      assert.equal(await text("#notify-permission"), "Browser permission: allowed.");
    });

    await t.test("notify N3 hidden tab polls only while notifications are on", async () => {
      await setHidden(true);
      assert.deepEqual(await liveTimers(), [30000]);
      await page.click("#notify-needs");
      await page.waitForFunction(
        () => document.getElementById("notify-status").textContent === "Notifications are off.",
        wait,
      );
      assert.deepEqual(await liveTimers(), []);
      await page.click("#notify-needs");
      await page.waitForFunction(
        () =>
          document.getElementById("notify-status").textContent ===
          "Notifications are on in this browser.",
        wait,
      );
      assert.deepEqual(await liveTimers(), [30000]);
      await setHidden(false);
      await page.waitForFunction(() => [...window.pollTimers.values()].includes(3000), wait);
    });

    await t.test("notify N4 one notification per newly waiting job", async () => {
      const pick = JOBS.map((j) => (j.id === "001-sample" ? { ...j, needs: "Pick one" } : j));
      await seedBoard(origin, pick);
      await page.evaluate(() => poll());
      assert.deepEqual(await shown(), []);
      await setHidden(true);
      await seedBoard(origin, [
        ...pick,
        {
          id: "006-sample-wait",
          project: "sample",
          title: "Synthetic waiting job",
          stage: "needs-human",
          updated: "2026-01-01T00:00:00Z",
        },
      ]);
      await page.evaluate(() => poll());
      await page.evaluate(() => poll());
      assert.deepEqual(await shown(), [
        {
          title: "Needs you: 006-sample-wait",
          body: "Synthetic waiting job",
          tag: "factory-needs-006-sample-wait",
        },
      ]);
      await page.evaluate(() => window.shown[0].instance.onclick());
      assert.equal(await page.evaluate(() => location.hash), "#006-sample-wait");
      await setHidden(false);
      await toHash("#/settings/display", "#settings-display:not([hidden])");
    });

    await t.test("notify C1 card detail toggles", async () => {
      for (const f of ["note", "meta", "pr"]) await page.click("#display-card-field-" + f);
      assert.equal(await storage("factory-hidden-card-fields"), "note,meta,pr");
      await page.click("#settings-back");
      await page.waitForSelector('.card[data-id="004-sample-pr"]', wait);
      const pr = '.card[data-id="004-sample-pr"]';
      assert.equal(await page.$(pr + " .note"), null);
      assert.equal(await page.$(pr + " .meta:not(.refs)"), null);
      assert.equal(await page.$("#board .pr-link"), null);
      assert.equal(
        await text('.card.needs-you[data-id="005-sample-needs"] .note'),
        "Approve the spec",
      );
      await setStorage("factory-hidden-card-fields", "note,bogus");
      await page.evaluate(() => {
        location.hash = "#/settings/display";
      });
      await reload("#settings-display:not([hidden])");
      assert.deepEqual(
        await page.$$eval("#display-card-fields input", (els) =>
          els.filter((el) => !el.checked).map((el) => el.dataset.field),
        ),
        ["note"],
      );
    });

    await t.test("notify C2 New idea project default", async () => {
      assert.deepEqual(
        await page.$$eval("#display-idea-project option", (els) => els.map((el) => el.value)),
        await page.$$eval("#idea-project option", (els) => els.map((el) => el.value)),
      );
      await page.select("#display-idea-project", "other");
      assert.equal(await storage("factory-idea-project"), "other");
      const ideaProject = async () => {
        await page.click("#new-idea");
        const value = await page.$eval("#idea-project", (el) => el.value);
        await page.evaluate(() => document.getElementById("idea-dlg").close());
        return value;
      };
      await page.select("#display-filter", "");
      assert.equal(await ideaProject(), "other");
      await page.select("#display-filter", "sample");
      assert.equal(await ideaProject(), "sample");
      await page.select("#display-filter", "");
      await setStorage("factory-idea-project", "ghost");
      await reload("#settings-display:not([hidden])");
      assert.equal(await page.$eval("#display-idea-project", (el) => el.value), "");
      assert.equal(await ideaProject(), "");
    });

    await t.test("notify C3 display reset keeps notifications", async () => {
      await page.select("#display-idea-project", "other");
      await page.click("#display-card-field-pr");
      await page.click("#display-reset");
      assert.equal(await storage("factory-hidden-card-fields"), null);
      assert.equal(await storage("factory-idea-project"), null);
      assert.equal(await storage("factory-notify"), "on");
      assert.equal(
        await page.$$eval("#display-card-fields input", (els) => els.every((el) => el.checked)),
        true,
      );
    });

    await t.test("notify C4 mobile layout", async () => {
      await page.setViewport({ width: 390, height: 844 });
      for (const [hash, selector] of [
        ["#/settings/display", "#settings-display:not([hidden])"],
        ["#/settings/notifications", "#settings-notifications:not([hidden])"],
      ]) {
        await toHash(hash, selector);
        await page.waitForFunction(
          () => document.documentElement.scrollWidth <= window.innerWidth,
          wait,
        );
      }
      await page.setViewport({ width: 1280, height: 900 });
    });

    await t.test("notify N5 unsupported browser", async () => {
      await page.evaluate(() => sessionStorage.setItem("no-notify", "1"));
      await reload("#settings-notifications:not([hidden])");
      assert.equal(await page.evaluate(() => "Notification" in window), false);
      assert.equal(await page.$eval("#notify-needs", (el) => el.disabled), true);
      assert.equal(await page.$eval("#notify-needs", (el) => el.checked), false);
      assert.equal(
        await text("#notify-permission"),
        "This browser does not support notifications.",
      );
    });

    assert.deepEqual(errors, []);
  });
});
