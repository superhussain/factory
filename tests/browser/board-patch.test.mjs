// Dashboard board writes send PATCH /api/board row changes and never read or write the whole
// board. Rows are the made-up running example: the idea "Add CSV export" lands beside
// 1999-synthetic-build, and a dropped save confirms on retry without a duplicate.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { openJobActions, waitForBoard } from "../support/board-ready.cjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";

const wait = { polling: 50 };
const JOBS = [
  {
    id: "1999-synthetic-build",
    project: "sample",
    title: "Synthetic build",
    stage: "build",
    updated: "2026-11-19T11:00:00Z",
  },
];
const BACKLOG = "# Backlog\n- [ ] sample: Synthetic backlog item\n";

async function putBacklog(origin, body) {
  const current = await fetch(origin + "/api/backlog");
  const saved = await fetch(origin + "/api/backlog", {
    method: "PUT",
    headers: {
      "Content-Type": "text/markdown; charset=utf-8",
      ...(current.ok ? { "If-Match": current.headers.get("ETag") } : { "If-None-Match": "*" }),
    },
    body,
  });
  assert.equal(saved.status, 200);
}

const board = async (origin) => (await (await fetch(origin + "/api/board")).json()).jobs;

test("dashboard board writes are row patches", { timeout: 180000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    await putBacklog(origin, BACKLOG);
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    page.on("dialog", (dialog) => dialog.accept());
    await controlBoardPolling(page);
    await page.setBypassServiceWorker(true);
    await page.setRequestInterception(true);
    const writes = [];
    let intercept = null;
    page.on("request", async (request) => {
      const { pathname } = new URL(request.url());
      if (pathname === "/api/board") writes.push(request.method());
      if (intercept && (await intercept(request))) return;
      return request.continue();
    });
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await waitForBoard(page);

    const addIdea = async (text, project) => {
      await page.click("#new-idea");
      await page.waitForFunction(() => document.getElementById("idea-dlg").open, wait);
      await page.select("#idea-project", project);
      await page.type("#idea-text", text);
      await page.click("#idea-add");
    };
    const openDrawer = async (id) => {
      await page.evaluate((i) => (location.hash = "#" + i), id);
      await page.waitForFunction(
        (i) => document.querySelector("#d-meta")?.textContent.includes(i),
        wait,
        id,
      );
    };

    // New idea, assign, discard and backlog accept each send one PATCH and nothing else.
    {
      await addIdea("Add CSV export", "");
      await page.waitForFunction(() => !document.getElementById("idea-dlg").open, wait);
      const idea = (await board(origin)).find((j) => j.title === "Add CSV export");
      assert.equal(idea.id, "2000-add-csv-export");
      assert.equal(idea.project, undefined);

      await openDrawer(idea.id);
      await page.select("#d-project", "sample");
      await page.click("#d-assign");
      await page.waitForFunction(
        (id) => jobs.find((j) => j.id === id)?.project === "sample",
        wait,
        idea.id,
      );

      await openJobActions(page);
      await page.click("#d-discard");
      await page.waitForFunction(
        (id) => jobs.find((j) => j.id === id)?.stage === "closed",
        wait,
        idea.id,
      );
      const closed = (await board(origin)).find((j) => j.id === idea.id);
      assert.deepEqual([closed.project, closed.stage], ["sample", "closed"]);

      await page.evaluate(() => closeDetail());
      await page.waitForSelector("#column-cards-backlog .backlog-card");
      await page.click("#column-cards-backlog .backlog-card");
      await page.waitForSelector("#backlog-form");
      await page.click("input[value=approved]");
      await page.click("#backlog-submit");
      await page.waitForFunction(
        () => !document.querySelector("#column-cards-backlog .backlog-card"),
        wait,
      );
      assert.equal(
        await (await fetch(origin + "/api/backlog")).text(),
        "# Backlog\n- [x] sample: Synthetic backlog item (2001-synthetic-backlog-item)\n",
      );
      assert.deepEqual(writes, ["PATCH", "PATCH", "PATCH", "PATCH"]);
    }

    // A dropped PATCH that landed: the page says "Not confirmed", and the retry adds no duplicate.
    // The idea dialog reconciles an uncertain save with one GET /api/board, answered here with
    // the board from before the commit, as a read that lags the write would.
    const stale = await (await fetch(origin + "/api/board")).text();
    let landed = null;
    intercept = async (request) => {
      if (new URL(request.url()).pathname !== "/api/board") return false;
      if (request.method() === "GET") {
        intercept = null;
        await request.respond({ status: 200, contentType: "application/json", body: stale });
        return true;
      }
      landed = await fetch(request.url(), {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: request.postData(),
      });
      await request.abort("connectionfailed");
      return true;
    };
    await addIdea("Add CSV export twice", "sample");
    await page.waitForFunction(
      () => document.getElementById("idea-err").textContent.startsWith("Not confirmed:"),
      wait,
    );
    assert.equal(landed.status, 200);
    const captureId = await page.evaluate(() => snap.capture_id);
    await page.click("#idea-add");
    await page.waitForFunction(() => !document.getElementById("idea-dlg").open, wait);
    assert.equal((await board(origin)).filter((j) => j.capture_id === captureId).length, 1);
    assert.deepEqual(writes.slice(4), ["PATCH", "GET", "PATCH"]);
    assert.deepEqual(errors, []);
  });
});
