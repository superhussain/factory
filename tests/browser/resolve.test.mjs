// Rework and Approve as is in the drawer bar, the job actions menu, the card menu and the inbox:
// which jobs show them, the board write after the note or the confirm, an empty note, Cancel, a
// dismissed confirm and a row that changed meanwhile, against one owned local Worker at mobile and
// desktop width. Rows are made up: 064-sample-human waits at needs-human after a review round cap.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { openJobActions } from "../support/board-ready.cjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";

const wait = { polling: 50 };
const NOW = "2026-01-02T12:00:00Z";
const AT = { at: "2026-01-01T00:00:00Z" };
const NOTE = "Keep the CSV header order.";
const job = (id, stage, extra = {}) => ({
  id,
  project: "sample",
  title: "Synthetic " + id,
  stage,
  updated: "2026-01-01T00:00:00Z",
  ...extra,
});
const JOBS = [
  job("042-sample-export", "needs-human", {
    note: "gate: spec-approval",
    needs: "approve spec.md, then run /factory approve-spec 042-sample-export",
  }),
  job("060-sample-lead", "spec"),
  job("064-sample-human", "needs-human", {
    title: "Export the sample report",
    note: "review panel: round 2 findings need a person",
    needs: "decide on the round-2 findings",
  }),
  job("066-sample-failed", "needs-human", { note: "builder failed twice" }),
  job("067-sample-resolving", "needs-human", {
    note: "approval requested on the dashboard",
    resolve: { action: "approve", at: "2026-01-01T00:00:00Z" },
  }),
  job("068-sample-rider", "needs-human", {
    note: "builder failed twice",
    refs: [{ id: "060-sample-lead", kind: "batched-into" }],
  }),
  job("069-sample-paused", "needs-human", { note: "builder failed twice", paused: AT }),
];
const RESOLVABLE = ["064-sample-human", "066-sample-failed"];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const CONFIRM =
  "Approve 066-sample-failed as is? Pickup ships and merges its PR without another review round.";
const HELP =
  "064-sample-human is waiting on you. Pickup starts /factory rework with your note, and the builder runs a new round on it.";

const freezeClock = (page) =>
  page.evaluateOnNewDocument((now) => {
    const RealDate = Date;
    window.Date = class extends RealDate {
      constructor(...args) {
        super(...(args.length ? args : [now]));
      }
      static now() {
        return new RealDate(now).getTime();
      }
    };
  }, NOW);
const openDrawer = async (page, id) => {
  await page.evaluate((i) => (location.hash = "#" + i), id);
  await page.waitForFunction(
    (i) => document.querySelector("#d-meta")?.textContent.includes(i),
    wait,
    id,
  );
};
const boardText = async (origin) => (await fetch(origin + "/api/board")).text();
const boardJob = async (origin, id) =>
  JSON.parse(await boardText(origin)).jobs.find((j) => j.id === id);
const shown = (page, sel) => page.$eval(sel, (el) => !el.hidden);

test("rework and approve as is", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const errors = [];
    for (const { theme, ...viewport } of VIEWPORTS) {
      const { width } = viewport;
      await seedBoard(origin, JOBS);
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport(viewport);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      page.on("pageerror", (error) => errors.push(String(error)));
      const dialogs = [];
      let answer = true;
      page.on("dialog", async (d) => {
        dialogs.push(d.message());
        await (answer ? d.accept() : d.dismiss());
      });
      await freezeClock(page);
      await controlBoardPolling(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="064-sample-human"]');
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);
      const reseed = async () => {
        await seedBoard(origin, JOBS);
        await page.evaluate(() => window.fixtureBoardPoll());
      };

      await t.test(`bar and menu items only when canResolve ${width}`, async () => {
        const got = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          got[id] = await Promise.all(
            ["#d-rework-bar", "#d-approve-as-is-bar", "#d-rework", "#d-approve-as-is"].map((s) =>
              shown(page, s),
            ),
          );
        }
        assert.deepEqual(
          got,
          Object.fromEntries(JOBS.map((j) => [j.id, Array(4).fill(RESOLVABLE.includes(j.id))])),
        );
        await openDrawer(page, "064-sample-human");
        assert.deepEqual(
          await page.$$eval("#d-actions-menu .btn:not([hidden])", (els) =>
            els.slice(0, 2).map((el) => el.textContent),
          ),
          ["Rework…", "Approve as is"],
        );
        await page.evaluate(() => (location.hash = ""));
      });

      await t.test(`the card menu offers them only when canResolve ${width}`, async () => {
        for (const id of ["064-sample-human", "042-sample-export"]) {
          await page.waitForSelector(`.card-actions[data-actions="${id}"]`);
          await page.$eval(`.card-actions[data-actions="${id}"]`, (b) => b.click());
          await page.waitForSelector("#card-actions-menu:not([hidden])", wait);
          const labels = await page.$$eval("#card-actions-menu [data-action]", (items) =>
            items.filter((b) => !b.hidden).map((b) => b.textContent),
          );
          const expected = id === "064-sample-human";
          assert.equal(labels.includes("Rework…"), expected, id);
          assert.equal(labels.includes("Approve as is"), expected, id);
          await page.keyboard.press("Escape");
          await page.waitForSelector("#card-actions-menu", { ...wait, hidden: true });
        }
      });

      await t.test(`the inbox row offers Rework and Approve as is ${width}`, async () => {
        await page.evaluate(() => (location.hash = "#/inbox"));
        await page.waitForSelector('[data-inbox-rework="064-sample-human"]', wait);
        const row = await page.$eval('[data-inbox-rework="064-sample-human"]', (b) =>
          [...b.closest(".inbox-actions").children].map((el) => el.textContent),
        );
        assert.deepEqual(row, ["Rework…", "Approve as is", "Open job"]);
        assert.deepEqual(
          await page.$$eval("[data-inbox-rework]", (els) =>
            els.map((el) => el.dataset.inboxRework),
          ),
          RESOLVABLE,
        );
      });

      await t.test(`Cancel and an empty note write nothing ${width}`, async () => {
        const before = await boardText(origin);
        await openDrawer(page, "064-sample-human");
        await activate("#d-rework-bar");
        await page.waitForSelector("#rework-dlg[open]", wait);
        assert.equal(await page.$eval("#rework-help", (el) => el.textContent), HELP);
        assert.equal(await page.$eval("#rework-note", (el) => el.maxLength), 2000);
        await activate("#rework-cancel");
        await page.waitForSelector("#rework-dlg:not([open])", wait);
        await activate("#d-rework-bar");
        await page.waitForSelector("#rework-dlg[open]", wait);
        await page.type("#rework-note", "   ");
        await activate("#rework-save");
        await page.waitForFunction(() => !document.getElementById("rework-err").hidden, wait);
        assert.equal(
          await page.$eval("#rework-err", (el) => el.textContent),
          "Write what the builder should change.",
        );
        assert.equal(await boardText(origin), before);
        await page.keyboard.press("Escape");
        await page.waitForSelector("#rework-dlg:not([open])", wait);
        assert.equal(await boardText(origin), before);
      });

      await t.test(`Rework with a note writes the board ${width}`, async () => {
        await activate("#d-rework-bar");
        await page.waitForSelector("#rework-dlg[open]", wait);
        await page.type("#rework-note", NOTE);
        await activate("#rework-save");
        await page.waitForSelector("#rework-dlg:not([open])", wait);
        await page.waitForFunction(() => document.getElementById("d-rework-bar").hidden, wait);
        await page.waitForFunction(() => document.activeElement.id === "d-actions", {
          ...wait,
          timeout: 5000,
        });
        const j = await boardJob(origin, "064-sample-human");
        assert.deepEqual(
          [j.stage, j.note, "needs" in j, j.resolve, j.updated],
          [
            "needs-human",
            "rework requested on the dashboard",
            false,
            { action: "rework", note: NOTE, at: NOW },
            NOW,
          ],
        );
      });

      await t.test(`a dismissed confirm writes nothing ${width}`, async () => {
        const before = await boardText(origin);
        await openDrawer(page, "066-sample-failed");
        dialogs.length = 0;
        answer = false;
        await activate("#d-approve-as-is-bar");
        answer = true;
        assert.deepEqual(dialogs, [CONFIRM]);
        assert.equal(await boardText(origin), before);
      });

      await t.test(`Approve as is writes the board ${width}`, async () => {
        await activate("#d-approve-as-is-bar");
        await page.waitForFunction(
          () => document.getElementById("d-approve-as-is-bar").hidden,
          wait,
        );
        const j = await boardJob(origin, "066-sample-failed");
        assert.deepEqual(
          [j.stage, j.note, "needs" in j, j.resolve, j.updated],
          [
            "needs-human",
            "approval requested on the dashboard",
            false,
            { action: "approve", at: NOW },
            NOW,
          ],
        );
        assert.equal(await page.$eval("#d-resolve-err", (el) => el.hidden), true);
      });

      await t.test(`the inbox buttons write the board ${width}`, async () => {
        await reseed();
        await page.evaluate(() => (location.hash = "#/inbox"));
        await page.waitForSelector('[data-inbox-approve-as-is="066-sample-failed"]', wait);
        await activate('[data-inbox-approve-as-is="066-sample-failed"]');
        await page.waitForFunction(
          () => !document.querySelector('[data-inbox-approve-as-is="066-sample-failed"]'),
          wait,
        );
        assert.deepEqual((await boardJob(origin, "066-sample-failed")).resolve, {
          action: "approve",
          at: NOW,
        });
        await activate('[data-inbox-rework="064-sample-human"]');
        await page.waitForSelector("#rework-dlg[open]", wait);
        await page.type("#rework-note", NOTE);
        await activate("#rework-save");
        await page.waitForSelector("#rework-dlg:not([open])", wait);
        assert.deepEqual((await boardJob(origin, "064-sample-human")).resolve, {
          action: "rework",
          note: NOTE,
          at: NOW,
        });
      });

      await t.test(`a row that changed meanwhile writes nothing ${width}`, async () => {
        await reseed();
        await openDrawer(page, "066-sample-failed");
        await page.waitForFunction(
          () => !document.getElementById("d-approve-as-is-bar").hidden,
          wait,
        );
        await seedBoard(
          origin,
          JOBS.map((j) => (j.id === "066-sample-failed" ? { ...j, paused: AT } : j)),
        );
        const before = await boardText(origin);
        await openJobActions(page);
        await activate("#d-approve-as-is");
        await page.waitForFunction(() => !document.getElementById("d-resolve-err").hidden, wait);
        assert.equal(
          await page.$eval("#d-resolve-err", (el) => el.textContent),
          "This job can no longer be reworked or approved here.",
        );
        assert.equal(await boardText(origin), before);
      });

      await t.test(`Escape during the save still refreshes the board ${width}`, async () => {
        await reseed();
        await openDrawer(page, "064-sample-human");
        await activate("#d-rework-bar");
        await page.waitForSelector("#rework-dlg[open]", wait);
        await page.type("#rework-note", NOTE);
        // Hold the board PATCH so Escape lands while the save is in flight.
        await page.evaluate(() => {
          const real = window.fetch;
          window.fetch = (url, init) => {
            if (init?.method !== "PATCH") return real(url, init);
            window.fetch = real;
            return new Promise((resolve) => {
              window.fixtureReleaseSave = () => resolve(real(url, init));
            });
          };
        });
        await activate("#rework-save");
        await page.waitForFunction(() => window.fixtureReleaseSave, wait);
        await page.keyboard.press("Escape");
        await page.waitForSelector("#rework-dlg:not([open])", wait);
        await page.evaluate(() => window.fixtureReleaseSave());
        await page.waitForFunction(() => document.getElementById("d-rework-bar").hidden, {
          ...wait,
          timeout: 5000,
        });
        assert.equal((await boardJob(origin, "064-sample-human")).resolve.note, NOTE);
        assert.deepEqual(errors, []);
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
