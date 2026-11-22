// Add steering note in the job actions menu: which jobs offer it, the board write after a trimmed
// note, an empty note, and the notes list with each delivery status line, against one owned local
// Worker at mobile and desktop width. Rows are made up: 114-steer-export runs at build, round 1
// asked for changes, and a person added notes at 14:00 and 14:30.
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
const NOW = "2026-01-02T15:00:00Z";
const AT = { at: "2026-01-01T00:00:00Z" };
const FIRST = {
  note: "Keep the CSV header order the same as the old report.",
  at: "2026-01-02T14:00:00Z",
};
const SECOND = { note: "Name the file export.csv.", at: "2026-01-02T14:30:00Z" };
const NOTE = "Keep the footer row too.";
const build = (at) => ({ role: "build", at, harness: "claude", via: "native" });
const job = (id, stage, extra = {}) => ({
  id,
  project: "sample",
  title: "Synthetic " + id,
  stage,
  updated: "2026-01-02T12:00:00Z",
  ...extra,
});
const JOBS = [
  job("114-steer-export", "build", {
    round: 2,
    steer: [FIRST, SECOND],
    agents: [build("2026-01-02T13:00:00Z"), build("2026-01-02T14:20:00Z")],
  }),
  job("113-steer-lead", "spec"),
  job("115-steer-rider", "spec", { refs: [{ id: "113-steer-lead", kind: "batched-into" }] }),
  job("116-steer-paused", "review", { paused: AT }),
  job("117-steer-human", "needs-human", { note: "builder failed twice" }),
  job("118-steer-full", "build", {
    steer: Array.from({ length: 10 }, (_, i) => ({ note: `Note ${i}.`, at: FIRST.at })),
  }),
  job("119-steer-approve", "approve", { steer: [FIRST], agents: [build("2026-01-02T13:00:00Z")] }),
];
const STEERABLE = ["114-steer-export", "113-steer-lead"];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const HELP =
  "114-steer-export keeps running. The builder reads your note when its next round starts. A note guides work inside the spec and cannot add scope.";

// The Worker takes one appended note per row and write, so the seed adds the notes a round at a
// time after a write that drops them, with 119 at build until the last write moves it to approve.
async function seedSteered(origin) {
  const rounds = Math.max(...JOBS.map((j) => j.steer?.length ?? 0));
  for (let n = 0; n <= rounds; n++)
    await seedBoard(
      origin,
      JOBS.map(({ steer, ...j }) => ({
        ...j,
        ...(j.stage === "approve" && { stage: "build" }),
        ...(steer && n && { steer: steer.slice(0, n) }),
      })),
    );
  await seedBoard(origin, JOBS);
}

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
const notesList = (page) =>
  page.$eval("#d-steer-notes", (el) => ({
    hidden: el.hidden,
    summary: el.querySelector("summary").textContent,
    items: [...el.querySelectorAll("li")].map((li) => li.textContent),
  }));

test("add steering note", { timeout: 240000 }, async (t) => {
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
      await seedSteered(origin);
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport(viewport);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      page.on("pageerror", (error) => errors.push(String(error)));
      await freezeClock(page);
      await controlBoardPolling(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector('.card[data-id="114-steer-export"]');
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);
      const openSteer = async () => {
        await openJobActions(page);
        await activate("#d-steer");
        await page.waitForSelector("#steer-dlg[open]", wait);
      };

      await t.test(`the menu item shows only on eligible jobs ${width}`, async () => {
        const got = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          got[id] = await page.$eval("#d-steer", (el) => !el.hidden);
        }
        assert.deepEqual(
          got,
          Object.fromEntries(JOBS.map((j) => [j.id, STEERABLE.includes(j.id)])),
        );
        assert.deepEqual(await page.$eval("#d-steer", (el) => [el.textContent, el.title]), [
          "Add steering note…",
          "Give the builder a note for its next round",
        ]);
      });

      await t.test(`the notes list shows each status line ${width}`, async () => {
        await openDrawer(page, "114-steer-export");
        assert.deepEqual(await notesList(page), {
          hidden: false,
          summary: "Steering notes: 2",
          items: [
            "2026-01-02 14:00 UTC: Keep the CSV header order the same as the old report. Sent to the builder 2026-01-02 14:20 UTC.",
            "2026-01-02 14:30 UTC: Name the file export.csv. Waiting for the builder's next round.",
          ],
        });
        assert.deepEqual(
          await page.$$eval("#d-steer-notes li .muted", (els) =>
            els.map((el) => [el.textContent, getComputedStyle(el).display]),
          ),
          [
            ["Sent to the builder 2026-01-02 14:20 UTC.", "block"],
            ["Waiting for the builder's next round.", "block"],
          ],
        );
        await openDrawer(page, "119-steer-approve");
        assert.deepEqual((await notesList(page)).items, [
          "2026-01-02 14:00 UTC: Keep the CSV header order the same as the old report. Not sent: no builder round ran after it.",
        ]);
        await openDrawer(page, "113-steer-lead");
        assert.equal((await notesList(page)).hidden, true);
      });

      await t.test(`Cancel and an empty note write nothing ${width}`, async () => {
        const before = await boardText(origin);
        await openDrawer(page, "114-steer-export");
        await openSteer();
        assert.equal(await page.$eval("#steer-help", (el) => el.textContent), HELP);
        assert.deepEqual(await page.$eval("#steer-note", (el) => [el.maxLength, el.placeholder]), [
          2000,
          "What the builder should keep in mind.",
        ]);
        await activate("#steer-cancel");
        await page.waitForSelector("#steer-dlg:not([open])", wait);
        await openSteer();
        await page.type("#steer-note", "   ");
        await activate("#steer-save");
        await page.waitForFunction(() => !document.getElementById("steer-err").hidden, wait);
        assert.equal(
          await page.$eval("#steer-err", (el) => el.textContent),
          "Write what the builder should keep in mind.",
        );
        await page.keyboard.press("Escape");
        await page.waitForSelector("#steer-dlg:not([open])", wait);
        await page.waitForFunction(() => document.activeElement.id === "d-actions", wait);
        assert.equal(await boardText(origin), before);
      });

      await t.test(`Add note appends the trimmed note ${width}`, async () => {
        await openSteer();
        await page.type("#steer-note", "  " + NOTE + "  ");
        await activate("#steer-save");
        await page.waitForSelector("#steer-dlg:not([open])", wait);
        const j = await boardJob(origin, "114-steer-export");
        assert.deepEqual(
          [j.stage, j.steer, j.updated],
          ["build", [FIRST, SECOND, { note: NOTE, at: NOW }], NOW],
        );
        await page.waitForFunction(
          () =>
            document.querySelector("#d-steer-notes summary").textContent === "Steering notes: 3",
          wait,
        );
        assert.equal(
          (await notesList(page)).items[2],
          "2026-01-02 15:00 UTC: Keep the footer row too. Waiting for the builder's next round.",
        );
        assert.deepEqual(errors, []);
      });
    }
  });
});
