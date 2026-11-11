// The drawer's Close job item and dialog: which jobs show it, the reason check, the board write,
// the card chip, and a job that left the board, against one owned local Worker, at mobile and
// desktop width. Also captures the review images.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { openJobActions } from "../support/board-ready.cjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const wait = { polling: 50 };
const NOW = "2026-01-02T12:00:00Z";
const REASON = "Duplicate of 041-sample-report";
const JOBS = [
  {
    id: "100-sample-idea",
    project: "sample",
    title: "Sample idea",
    stage: "idea",
    idea: "Sample idea",
    updated: "2026-01-01T00:00:00Z",
  },
  {
    id: "042-sample-export",
    project: "sample",
    title: "Export the sample report",
    stage: "build",
    branch: "factory/042-sample-export",
    round: 1,
    updated: "2026-01-01T00:00:00Z",
    claim: { device: "host-a", session: "a1b2c3", since: "2026-01-01T10:00:00Z" },
  },
  { id: "044-closed", project: "sample", title: "Closed job", stage: "closed", updated: NOW },
  { id: "045-no-project", title: "No project", stage: "build", updated: NOW },
  {
    id: "046-sample-pr",
    project: "sample",
    title: "Sample pull request",
    stage: "pr-open",
    branch: "factory/046-sample-pr",
    round: 1,
    updated: NOW,
  },
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const CARD = '.card[data-id="042-sample-export"]';
const CLOSE_HINT =
  " The close may have saved. Press Close job again to check. It will not queue a second close or change a saved reason.";
// null sends board writes through, 'drop' aborts them unsent, 'land' sends then aborts the page's copy,
// '5xx' answers 503 with a body ending in a period.
let putMode = null;
let readFails = false;

const freezeClock = (page) =>
  page.evaluateOnNewDocument(() => {
    const RealDate = Date;
    window.Date = class extends RealDate {
      constructor(...args) {
        super(...(args.length ? args : ["2026-01-02T12:00:00Z"]));
      }
      static now() {
        return new RealDate("2026-01-02T12:00:00Z").getTime();
      }
    };
  });
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
const shown = (page, selector) => page.$eval(selector, (el) => !el.hidden);
const chip = (page) =>
  page.$eval(CARD + " .id", (el) => [...el.querySelectorAll(".chip")].map((c) => c.textContent));
const errText = (page) => page.$eval("#close-job-err", (el) => (el.hidden ? null : el.textContent));
// The drawer head plus the open menu, which hangs below it.
const headAndMenu = (page) =>
  page.evaluate(() => {
    const boxes = [".detail-head", "#d-actions-menu:not([hidden])"]
      .map((s) => document.querySelector(s)?.getBoundingClientRect())
      .filter(Boolean);
    const x = Math.min(...boxes.map((b) => b.left));
    const y = Math.min(...boxes.map((b) => b.top));
    return {
      x,
      y,
      width: Math.max(...boxes.map((b) => b.right)) - x,
      height: Math.max(...boxes.map((b) => b.bottom)) - y + 4,
    };
  });

test("close job", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
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
      await freezeClock(page);
      await controlBoardPolling(page);
      await page.setBypassServiceWorker(true);
      await page.setRequestInterception(true);
      page.on("request", async (request) => {
        if (
          readFails &&
          request.method() === "GET" &&
          new URL(request.url()).pathname === "/api/board"
        )
          return request.abort("failed");
        if (request.method() !== "PUT" || new URL(request.url()).pathname !== "/api/board")
          return request.continue();
        const mode = putMode;
        putMode = null;
        if (mode === "5xx")
          return request.respond({
            status: 503,
            contentType: "text/plain",
            body: "Storage is busy.",
          });
        if (mode === "land") {
          const headers = request.headers();
          await fetch(request.url(), {
            method: "PUT",
            headers: Object.fromEntries(
              ["if-match", "if-none-match"].filter((h) => headers[h]).map((h) => [h, headers[h]]),
            ),
            body: request.postData(),
          });
        }
        if (mode) return request.abort("failed");
        return request.continue();
      });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(CARD);
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);
      const openDialog = async (id) => {
        await openDrawer(page, id);
        await openJobActions(page);
        await activate("#d-close-job");
        await page.waitForSelector("#close-job-dlg[open]");
      };

      await t.test(`Close job only for closable jobs ${width}`, async () => {
        const items = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          items[id] = await shown(page, "#d-close-job");
        }
        assert.deepEqual(items, {
          "100-sample-idea": false,
          "042-sample-export": true,
          "044-closed": false,
          "045-no-project": false,
          "046-sample-pr": true,
        });
      });

      await t.test(`hidden dialog fields stay hidden ${width}`, async () => {
        const display = await page.evaluate(
          (ids) => {
            const out = {};
            for (const id of ids) {
              const el = document.getElementById(id);
              el.hidden = true;
              const hidden = getComputedStyle(el).display;
              el.hidden = false;
              out[id] = [hidden, getComputedStyle(el).display];
            }
            return out;
          },
          ["idea-text", "edit-idea-text", "close-job-reason"],
        );
        assert.deepEqual(display, {
          "idea-text": ["none", "block"],
          "edit-idea-text": ["none", "block"],
          "close-job-reason": ["none", "block"],
        });
      });

      await t.test(`an empty reason writes nothing ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await openJobActions(page);
        if (width === 390)
          await page.screenshot({
            path: `${SHOTS}after-menu-390.png`,
            clip: await headAndMenu(page),
          });
        await activate("#d-close-job");
        await page.waitForSelector("#close-job-dlg[open]");
        assert.equal(
          await page.$eval("#close-job-help", (el) => el.textContent),
          "042-sample-export stops at build. The next pickup run writes decision.md with your reason, " +
            "then moves the job to Closed. If a session is running it, the close waits for that session to stop.",
        );
        assert.equal(
          await page.$eval("#close-job-reason", (el) => el.getAttribute("aria-describedby")),
          "close-job-help",
        );
        assert.equal(await page.evaluate(() => document.activeElement.id), "close-job-reason");
        const before = await boardText(origin);
        await page.type("#close-job-reason", "   ");
        await activate("#close-job-save");
        await page.waitForFunction(() => !document.getElementById("close-job-err").hidden, wait);
        assert.equal(
          await errText(page),
          "Enter a reason of 1 to 200 characters. Line breaks and control characters are not allowed.",
        );
        assert.equal(await boardText(origin), before);
        await activate("#close-job-cancel");
        await page.waitForSelector("#close-job-dlg:not([open])");
      });

      await t.test(`a reason with a line separator writes nothing ${width}`, async () => {
        await openDialog("042-sample-export");
        const before = await boardText(origin);
        await page.$eval(
          "#close-job-reason",
          (el, v) => {
            el.value = v;
          },
          "Duplicate\u2028of 041-sample-report",
        );
        await activate("#close-job-save");
        await page.waitForFunction(() => !document.getElementById("close-job-err").hidden, wait);
        assert.equal(
          await errText(page),
          "Enter a reason of 1 to 200 characters. Line breaks and control characters are not allowed.",
        );
        assert.equal(await boardText(origin), before);
        await activate("#close-job-cancel");
        await page.waitForSelector("#close-job-dlg:not([open])");
      });

      await t.test(`a job that left the board writes nothing ${width}`, async () => {
        await openDialog("046-sample-pr");
        assert.equal(await errText(page), null);
        await seedBoard(
          origin,
          JOBS.filter((j) => j.id !== "046-sample-pr"),
        );
        const before = await boardText(origin);
        await page.type("#close-job-reason", REASON);
        await activate("#close-job-save");
        await page.waitForFunction(() => !document.getElementById("close-job-err").hidden, wait);
        assert.equal(await errText(page), "This job is no longer on the board.");
        assert.equal(await boardText(origin), before);
        await activate("#close-job-cancel");
        await page.waitForSelector("#close-job-dlg:not([open])");
        await seedBoard(origin, JOBS);
      });

      const reseed = async () => {
        await seedBoard(origin, JOBS);
        await openDrawer(page, "042-sample-export");
        await page.evaluate(() => window.fixtureBoardPoll());
        await page.waitForFunction(
          () => !document.getElementById("d-meta").textContent.endsWith(" · closing"),
          wait,
        );
      };
      const failedSave = async () => {
        await activate("#close-job-save");
        await page.waitForFunction(() => !document.getElementById("close-job-err").hidden, wait);
        return errText(page);
      };

      await t.test(`a dropped save retries cleanly ${width}`, async () => {
        await openDialog("042-sample-export");
        const before = await boardText(origin);
        putMode = "drop";
        await page.type("#close-job-reason", REASON);
        assert.match(await failedSave(), /^Not confirmed: /);
        assert.equal(await boardText(origin), before);
        await activate("#close-job-save");
        await page.waitForSelector("#close-job-dlg:not([open])");
        assert.deepEqual((await boardJob(origin, "042-sample-export")).close, {
          reason: REASON,
          at: NOW,
        });
      });

      await t.test(`a landed save confirms on retry ${width}`, async () => {
        await reseed();
        await openDialog("042-sample-export");
        putMode = "land";
        await page.type("#close-job-reason", REASON);
        const text = await failedSave();
        assert.ok(text.startsWith("Not confirmed: "), text);
        assert.ok(text.endsWith("." + CLOSE_HINT), text);
        assert.deepEqual((await boardJob(origin, "042-sample-export")).close, {
          reason: REASON,
          at: NOW,
        });
        if (width === 390)
          await (
            await page.$("#close-job-dlg")
          ).screenshot({ path: `${SHOTS}close-uncertain-390.png` });
        const after = await boardText(origin);
        await activate("#close-job-save");
        await page.waitForSelector("#close-job-dlg:not([open])");
        assert.equal(await boardText(origin), after);
      });

      await t.test(`an offline retry keeps the close hint ${width}`, async () => {
        await reseed();
        await openDialog("042-sample-export");
        putMode = "land";
        await page.type("#close-job-reason", REASON);
        const first = await failedSave();
        assert.match(first, /^Not confirmed: /);
        const clearErr = () =>
          page.$eval("#close-job-err", (el) => {
            el.hidden = true;
            el.textContent = "";
          });
        try {
          readFails = true;
          await clearErr();
          const text = await failedSave();
          if (width === 390)
            await (
              await page.$("#close-job-dlg")
            ).screenshot({ path: `${SHOTS}close-offline-retry-390.png` });
          assert.equal(text, first);
        } finally {
          readFails = false;
        }
        await page.type("#close-job-reason", " again");
        await clearErr();
        const edited = await failedSave();
        if (width === 390)
          await (
            await page.$("#close-job-dlg")
          ).screenshot({ path: `${SHOTS}close-edited-reason-390.png` });
        assert.equal(edited, "A close is already queued for this job.");
        await activate("#close-job-cancel");
        await page.waitForSelector("#close-job-dlg:not([open])");
      });

      await t.test(`a 5xx body ending in a period gets one period ${width}`, async () => {
        await reseed();
        await openDialog("042-sample-export");
        const before = await boardText(origin);
        putMode = "5xx";
        await page.type("#close-job-reason", REASON);
        const text = await failedSave();
        if (width === 390)
          await (await page.$("#close-job-dlg")).screenshot({ path: `${SHOTS}close-5xx-390.png` });
        assert.equal(
          text,
          "Not confirmed: saving the board failed: 503 Storage is busy." + CLOSE_HINT,
        );
        assert.equal(await boardText(origin), before);
        await activate("#close-job-save");
        await page.waitForSelector("#close-job-dlg:not([open])");
        assert.deepEqual((await boardJob(origin, "042-sample-export")).close, {
          reason: REASON,
          at: NOW,
        });
      });

      await reseed();
      await t.test(`a valid reason writes close ${width}`, async () => {
        await openDialog("042-sample-export");
        assert.equal(await page.$eval("#close-job-reason", (el) => el.value), "");
        assert.equal(await errText(page), null);
        // The desktop pass pads the reason to check that it is trimmed.
        await page.type("#close-job-reason", width === 390 ? REASON : "  " + REASON + " ");
        if (width === 390)
          await (
            await page.$("#close-job-dlg")
          ).screenshot({ path: `${SHOTS}after-dialog-390.png` });
        await activate("#close-job-save");
        await page.waitForFunction(
          () => document.getElementById("d-meta").textContent.endsWith(" · closing"),
          wait,
        );
        assert.deepEqual((await boardJob(origin, "042-sample-export")).close, {
          reason: REASON,
          at: NOW,
        });
        assert.equal(await page.$eval("#close-job-dlg", (el) => el.open), false);
        assert.equal(await page.evaluate(() => document.activeElement.id), "d-actions");
        assert.deepEqual(await chip(page), ["Closing"]);
        assert.equal(await shown(page, "#d-close-job"), false);
        if (width === 1280) {
          await page.click("#d-close");
          await page.waitForSelector("#detail", { hidden: true });
          await (await page.$(CARD)).screenshot({ path: `${SHOTS}after-card-1280.png` });
        }
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
