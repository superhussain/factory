// Stage-note trials on Settings, Pipeline against one owned local Worker: rows from the board's
// trials list, inert agent-written text, scope filtering and the pause banner.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  seedTrials,
  resumeTrials,
} from "../support/browser-server.mjs";
import { waitForBoard } from "../support/board-ready.cjs";

const JOB = {
  id: "001-sample",
  project: "sample",
  title: "Synthetic sample job",
  stage: "spec",
  updated: "2026-01-01T00:00:00Z",
};
const XSS = '<img src=x onerror="window.pwned=1">';
const arm = (n, side) => Array.from({ length: n }, (_, i) => `${101 + i}-sample-${side}`);
const TRIALS = [
  {
    id: "sample-spec-v8",
    project: "sample",
    stage: "spec",
    status: "open",
    opened: "2026-11-12T09:00:00Z",
    arms: { a: arm(3, "a"), b: arm(2, "b") },
    reads: { rounds: "inconclusive", tokens: "inconclusive" },
  },
  {
    id: "sample-build-v5",
    project: "sample",
    stage: "build",
    status: "reverted",
    opened: "2026-11-05T09:00:00Z",
    arms: { a: arm(6, "a"), b: arm(5, "b") },
    reads: { rounds: "b higher", tokens: "no difference" },
  },
  {
    id: "sample-review-v3",
    project: "sample",
    stage: "review",
    status: "reverted",
    opened: "2026-11-01T09:00:00Z",
    arms: { a: arm(5, "a"), b: arm(6, "b") },
    reads: { rounds: "no difference", tokens: "b higher" },
  },
  { id: XSS, project: "other", stage: "spec", status: "open", opened: "2026-11-13T09:00:00Z" },
];
const BANNER =
  'Stage-note trials for sample are paused after 2 reverts. Retro edits no sample stage note until a person runs factory.py trial resume sample "<reason>" or asks an agent to run it.';
const wait = { polling: 50 };

const rows = (page) =>
  page.$$eval("#pipeline-trials-body tr", (trs) =>
    trs.map((tr) => [...tr.cells].map((c) => c.textContent)),
  );
const shown = (page, id) => page.$eval(`#${id}`, (el) => !el.hidden);
const rowCount = (page, n) =>
  page.waitForFunction(
    (count) => document.querySelectorAll("#pipeline-trials-body tr").length === count,
    wait,
    n,
  );

test("pipeline stage-note trials", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB]);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await waitForBoard(page);
    await page.evaluate(() => {
      location.hash = "#/settings/pipeline";
    });
    await page.waitForSelector("#settings-pipeline:not([hidden])", wait);
    await page.select("#pipeline-scope", "global");

    await t.test("trials T0", async () => {
      assert.equal(await shown(page, "pipeline-trials-empty"), true);
      assert.equal(
        await page.$eval("#pipeline-trials-empty", (el) => el.textContent),
        "No stage-note trials yet.",
      );
      assert.equal(await shown(page, "pipeline-trials-wrap"), false);
      assert.deepEqual(
        await page.$eval("#pipeline-trials-wrap", (el) => [
          el.getAttribute("tabindex"),
          el.getAttribute("role"),
          el.getAttribute("aria-label"),
          el.getAttribute("aria-labelledby"),
        ]),
        ["0", "region", "Stage-note trials table", null],
      );
      assert.equal(await shown(page, "pipeline-trials-paused"), false);
    });

    await t.test("trials T1", async () => {
      await seedTrials(origin, TRIALS);
      await page.evaluate(() => poll());
      await rowCount(page, 4);
      const cells = await rows(page);
      assert.deepEqual(
        cells.map((r) => r[0]),
        [XSS, "sample-spec-v8", "sample-build-v5", "sample-review-v3"],
      );
      assert.deepEqual(cells[1], [
        "sample-spec-v8",
        "spec",
        "open",
        "3 / 2",
        "inconclusive",
        "inconclusive",
      ]);
      assert.deepEqual(cells[0].slice(3), [
        "not settled yet",
        "not settled yet",
        "not settled yet",
      ]);
      assert.equal(await page.$("#pipeline-trials-body img"), null);
      assert.equal(await page.evaluate(() => window.pwned), undefined);
      assert.equal(await shown(page, "pipeline-trials-wrap"), true);
      assert.equal(await shown(page, "pipeline-trials-empty"), false);
    });

    await t.test("trials T2", async () => {
      assert.equal(await shown(page, "pipeline-trials-paused"), true);
      assert.deepEqual(
        await page.$$eval("#pipeline-trials-paused p", (ps) => ps.map((p) => p.textContent)),
        [BANNER],
      );
      assert.equal(
        await page.$eval("#pipeline-trials-paused p code", (el) => el.textContent),
        'factory.py trial resume sample "<reason>"',
      );
      // The quoted reason never wraps apart, so a lone quote never ends a line.
      assert.deepEqual(
        await page.$eval("#pipeline-trials-paused p code span", (el) => [
          el.textContent,
          getComputedStyle(el).whiteSpace,
        ]),
        ['"<reason>"', "nowrap"],
      );
      assert.equal(
        await page.$eval("#pipeline-trials-paused", (el) => el.getAttribute("role")),
        "status",
      );
      // An unchanged paused set keeps the same banner node, so the status region stays quiet.
      assert.equal(
        await page.evaluate(() => {
          const before = document.querySelector("#pipeline-trials-paused p");
          renderPipelineTrials();
          return document.querySelector("#pipeline-trials-paused p") === before;
        }),
        true,
      );
    });

    await t.test("trials T3", async () => {
      await page.select("#pipeline-scope", "projects/sample");
      await rowCount(page, 3);
      assert.ok((await rows(page)).every((r) => r[0] !== XSS));
      assert.equal(await shown(page, "pipeline-trials-paused"), true);
    });

    await t.test("trials T4", async () => {
      await page.select("#pipeline-scope", "global");
      await rowCount(page, 4);
      await resumeTrials(origin, "sample");
      await page.evaluate(() => poll());
      await page.waitForFunction(
        () => document.getElementById("pipeline-trials-paused").hidden,
        wait,
      );
      const build = (await rows(page)).find((r) => r[0] === "sample-build-v5");
      assert.equal(build[2], "reverted, resumed");
    });

    await t.test("trials T5", async () => {
      // Like factory.py trials_paused, reverts count even when the record cannot render as a row.
      const bare = { project: "other", status: "reverted" };
      await seedTrials(origin, [...TRIALS, bare, { ...bare }]);
      await page.evaluate(() => poll());
      await page.waitForFunction(
        () => !document.getElementById("pipeline-trials-paused").hidden,
        wait,
      );
      assert.deepEqual(
        await page.$$eval("#pipeline-trials-paused code", (els) => els.map((el) => el.textContent)),
        ['factory.py trial resume other "<reason>"', 'factory.py trial resume sample "<reason>"'],
      );
      assert.equal((await rows(page)).length, 4);
    });

    assert.deepEqual(errors, []);
    await context.close();
  });
});
