// Approve spec in the drawer bar, the job actions menu and the inbox: which jobs show it, the board
// write after the confirm, a dismissed confirm and a row that changed meanwhile, against one owned
// local Worker at mobile and desktop width. The inbox also offers Open in Tracer for a Tracer hold
// and Babysit PR and Open PR for a PR held for a person.
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
const GATE = "gate: spec-approval";
const GATE_NEEDS = "approve spec.md, then run /factory approve-spec 042-sample-export";
const TRACER = "https://tracer.example.test/scope/061";
const PR65 = "https://github.com/example/sample/pull/65";
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
    title: "Export the sample report",
    note: GATE,
    needs: GATE_NEEDS,
  }),
  job("060-sample-lead", "spec"),
  job("061-sample-tracer", "needs-human", {
    note: "gate: tracer-scope",
    tracer: { url: TRACER },
  }),
  job("062-sample-rider", "needs-human", {
    note: GATE,
    refs: [{ id: "060-sample-lead", kind: "batched-into" }],
  }),
  job("063-sample-paused", "needs-human", { note: GATE, paused: AT }),
  job("064-sample-human", "needs-human", { note: "review round cap reached" }),
  job("065-sample-held", "pr-open", {
    branch: "factory/065-sample-held",
    round: 1,
    pr: PR65,
    needs: "review and merge PR #65: this project's pipeline holds every PR for a person",
  }),
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const CONFIRM = "Approve the spec of 042-sample-export? Pickup starts the build on its next run.";
const OFFLINE = "Could not reach the factory. Check your connection, then try again.";

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
const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);
const shown = (page, sel) => page.$eval(sel, (el) => !el.hidden);
const inboxRows = (page) =>
  page.$$eval("#inbox-list .inbox-item", (items) =>
    items.map((item) => [
      item.querySelector(".inbox-kind").textContent,
      item.querySelector(".inbox-meta").textContent.split(" · ")[1],
      [...item.querySelectorAll(".inbox-actions > *")].map((el) =>
        [el.tagName, el.textContent, el.getAttribute("href")].filter((v) => v !== null),
      ),
    ]),
  );

test("approve spec", { timeout: 240000 }, async (t) => {
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
      await page.waitForSelector('.card[data-id="042-sample-export"]');
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);

      await t.test(`button and item only when canApproveSpec ${width}`, async () => {
        const bar = {};
        const item = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          bar[id] = await shown(page, "#d-approve-spec-bar");
          item[id] = await shown(page, "#d-approve-spec");
        }
        const expected = Object.fromEntries(JOBS.map((j) => [j.id, j.id === "042-sample-export"]));
        assert.deepEqual(bar, expected);
        assert.deepEqual(item, expected);
        assert.equal(
          await page.$eval("#d-approve-spec-bar", (el) => el.title),
          "Start the build on this spec",
        );
        await openDrawer(page, "042-sample-export");
        assert.equal(
          await page.$eval("#d-actions-menu .btn:not([hidden])", (el) => el.id),
          "d-approve-spec",
        );
      });

      await t.test(`the inbox offers the unblocking action ${width}`, async () => {
        await page.evaluate(() => (location.hash = "#/inbox"));
        await page.waitForFunction(
          () => document.querySelectorAll("#inbox-list .inbox-item").length === 6,
          wait,
        );
        assert.deepEqual(await inboxRows(page), [
          [
            "Question",
            "065-sample-held",
            [
              ["BUTTON", "Babysit PR #65"],
              ["A", "Open PR #65", PR65],
              ["A", "Open job", "#065-sample-held"],
            ],
          ],
          [
            "Spec approval",
            "042-sample-export",
            [
              ["BUTTON", "Approve spec"],
              ["A", "Open spec", "#042-sample-export/spec"],
            ],
          ],
          [
            "Spec approval",
            "061-sample-tracer",
            [
              ["A", "Open in Tracer", TRACER],
              ["A", "Open spec", "#061-sample-tracer/spec"],
            ],
          ],
          ["Spec approval", "062-sample-rider", [["A", "Open spec", "#062-sample-rider/spec"]]],
          ["Spec approval", "063-sample-paused", [["A", "Open spec", "#063-sample-paused/spec"]]],
          [
            "Needs recovery",
            "064-sample-human",
            [
              ["BUTTON", "Rework…"],
              ["BUTTON", "Approve as is"],
              ["A", "Open job", "#064-sample-human"],
            ],
          ],
        ]);
        assert.ok(
          (
            await page.$eval(
              '[data-inbox-approve-spec="042-sample-export"]',
              (b) => b.closest(".inbox-item").querySelector(".inbox-meta").textContent,
            )
          ).endsWith(" · Read the spec, then press Approve spec to start the build."),
        );
      });

      await t.test(`the focused bar button shows a ring ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await page.focus("#d-approve-spec-bar");
        assert.deepEqual(
          await page.$eval("#d-approve-spec-bar", (el) => [
            el.matches(":focus-visible"),
            getComputedStyle(el).outlineStyle,
          ]),
          [true, "solid"],
        );
      });

      await t.test(`a dismissed confirm writes nothing ${width}`, async () => {
        const before = await (await fetch(origin + "/api/board")).text();
        await openDrawer(page, "042-sample-export");
        answer = false;
        await activate("#d-approve-spec-bar");
        answer = true;
        assert.deepEqual(dialogs, [CONFIRM]);
        assert.equal(await (await fetch(origin + "/api/board")).text(), before);
      });

      await t.test(`Approve spec writes the board ${width}`, async () => {
        await activate("#d-approve-spec-bar");
        await page.waitForFunction(
          () => document.getElementById("d-approve-spec-bar").hidden,
          wait,
        );
        assert.equal(await page.evaluate(() => document.activeElement.id), "d-actions");
        const j = await boardJob(origin, "042-sample-export");
        assert.deepEqual(
          [j.stage, j.note, "needs" in j, j.updated],
          ["build", "spec approved on the dashboard", false, NOW],
        );
        assert.equal(await page.$eval("#d-approve-spec-err", (el) => el.hidden), true);
      });

      await t.test(`the inbox button writes the board ${width}`, async () => {
        await seedBoard(origin, JOBS);
        await page.evaluate(() => window.fixtureBoardPoll());
        await page.evaluate(() => (location.hash = "#/inbox"));
        await page.waitForSelector("[data-inbox-approve-spec]", wait);
        await activate("[data-inbox-approve-spec]");
        await page.waitForFunction(
          () => !document.querySelector("[data-inbox-approve-spec]"),
          wait,
        );
        const j = await boardJob(origin, "042-sample-export");
        assert.deepEqual(
          [j.stage, j.note, "needs" in j],
          ["build", "spec approved on the dashboard", false],
        );
      });

      await t.test(`a row that changed meanwhile writes nothing ${width}`, async () => {
        await seedBoard(origin, JOBS);
        await page.evaluate(() => window.fixtureBoardPoll());
        await openDrawer(page, "042-sample-export");
        await page.waitForFunction(
          () => !document.getElementById("d-approve-spec-bar").hidden,
          wait,
        );
        await seedBoard(
          origin,
          JOBS.map((j) => (j.id === "042-sample-export" ? { ...j, paused: AT } : j)),
        );
        const before = await (await fetch(origin + "/api/board")).text();
        await openJobActions(page);
        await activate("#d-approve-spec");
        await page.waitForFunction(
          () => !document.getElementById("d-approve-spec-err").hidden,
          wait,
        );
        assert.equal(
          await page.$eval("#d-approve-spec-err", (el) => el.textContent),
          "This spec can no longer be approved.",
        );
        assert.equal(await (await fetch(origin + "/api/board")).text(), before);
      });

      await t.test(`a failed bar Approve spec keeps focus on the button ${width}`, async () => {
        await seedBoard(origin, JOBS);
        await page.evaluate(() => window.fixtureBoardPoll());
        await openDrawer(page, "042-sample-export");
        await page.waitForFunction(
          () => !document.getElementById("d-approve-spec-bar").hidden,
          wait,
        );
        const before = await (await fetch(origin + "/api/board")).text();
        // The board write fails as a dropped connection would.
        await page.evaluate(() => {
          const fetchNow = window.fetch;
          window.fetch = (url, options) =>
            String(url).endsWith("api/board") && options?.method === "PATCH"
              ? Promise.reject(new TypeError("Failed to fetch"))
              : fetchNow(url, options);
        });
        await activate("#d-approve-spec-bar");
        await page.waitForFunction(
          (line) => document.getElementById("d-approve-spec-err").textContent === line,
          wait,
          OFFLINE,
        );
        assert.equal(await page.evaluate(() => document.activeElement.id), "d-approve-spec-bar");
        assert.equal(await (await fetch(origin + "/api/board")).text(), before);
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
