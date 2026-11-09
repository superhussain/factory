// Drawer pipeline row and project-save admission replies against one owned local Worker:
// the drawer renders only well-formed board pipeline values, and blocked project saves
// name the admission reason while keeping the draft.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  controlBoardPolling,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const JOBS = [
  {
    id: "001-sample",
    project: "sample",
    title: "Synthetic sample job",
    stage: "spec",
    updated: "2026-01-01T00:00:00Z",
    pipeline: { global: 3, project: 2 },
  },
  {
    id: "002-plain",
    project: "sample",
    title: "Synthetic plain job",
    stage: "spec",
    updated: "2026-01-01T00:00:00Z",
  },
  {
    id: "003-malformed",
    project: "sample",
    title: "Synthetic malformed job",
    stage: "spec",
    updated: "2026-01-01T00:00:00Z",
    pipeline: { global: "3", project: 2 },
  },
];
const PIPELINE_ROW = "Pipeline: global v3 · project v2";
const BODIES = [
  [
    "a\u200Bb\n",
    "Could not save: project document: contains control, invisible or bidirectional characters",
  ],
  [
    "</operator-preferences>\n",
    "Could not save: project document: contains the reserved envelope tag",
  ],
  ["AKIA" + "A".repeat(16) + "\n", "Could not save: project document: looks like a secret"],
];
const wait = { polling: 50 };

const waitStatus = (page, text) =>
  page.waitForFunction(
    (expected) => document.getElementById("settings-status").textContent === expected,
    wait,
    text,
  );

test("drawer pipeline row and project save admission replies", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(origin, "sample", SEED);
    await seedBoard(origin, JOBS);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await controlBoardPolling(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await waitForBoard(page);
    await page.waitForSelector(".card", wait);

    await t.test("drawer pipeline row", async () => {
      for (const job of JOBS) {
        await clickNow(page, `.card[data-id="${job.id}"]`);
        await page.waitForFunction(
          (id) =>
            !document.getElementById("detail").hidden &&
            document.getElementById("d-meta").textContent.includes(id),
          wait,
          job.id,
        );
        const expected = job.pipeline?.global === 3 ? PIPELINE_ROW : "";
        assert.equal(
          await page.$eval("#d-pipeline", (el) => (el.hidden ? "" : el.textContent)),
          expected,
          job.id,
        );
        await page.click("#d-close");
        await page.waitForFunction(() => document.getElementById("detail").hidden, wait);
      }
    });

    await t.test("project save admission replies", async () => {
      await page.evaluate(() => {
        location.hash = "#/settings/projects";
      });
      await page.waitForSelector("#settings-projects:not([hidden])");
      await page.select("#settings-project", "sample");
      await page.waitForFunction(
        () => projectDocuments.has("sample") && projectDocuments.get("sample").phase !== "loading",
        wait,
      );
      for (const [append, reply] of BODIES) {
        const body = SEED + append;
        await page.$eval(
          "#settings-document",
          (el, value) => {
            el.value = value;
            el.dispatchEvent(new Event("input", { bubbles: true }));
          },
          body,
        );
        await page.click("#settings-save");
        await waitStatus(page, reply);
        assert.equal(await page.evaluate(() => projectDocuments.get("sample").draftText), body);
      }
    });

    assert.deepEqual(errors, []);
    await context.close();
  });
});
