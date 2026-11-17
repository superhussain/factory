// The inbox page against one owned local Worker, with a synthetic board and backlog for project
// sample. INBOX_SHOTS names a folder for captures. INBOX_SHOT_STAGE=before takes only the board
// capture and skips every other subtest, so the file runs against a checkout without the page.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const wait = { polling: 50 };
const SHOTS = process.env.INBOX_SHOTS;
const BEFORE = process.env.INBOX_SHOT_STAGE === "before";
const job = (id, stage, extra = {}) => ({
  id,
  project: "sample",
  title: "Synthetic " + id,
  stage,
  updated: "2026-01-01T00:00:00Z",
  ...extra,
});
const JOBS = [
  job("042-sample-export", "pr-open", {
    title: "Export the sample report",
    branch: "factory/042-sample-export",
    round: 1,
    pr: "https://github.com/example/sample/pull/44",
    needs: "review and merge PR #44",
    escalated: true,
    updated: "2026-01-02T00:00:00Z",
  }),
  job("043-sample-filter", "build"),
  job("046-sample-human", "needs-human", { title: "Recover the sample host" }),
  job("051-sample-retry", "build", {
    title: "Retry the sample upload",
    needs: "Pick a retry limit in #051-sample-retry/needs",
    updated: "2026-01-03T00:00:00Z",
  }),
];
const indent = (lines) => lines.map((line) => (line ? "  " + line : line)).join("\n");
const BACKLOG = [
  "# Backlog",
  "",
  "- [ ] sample: Cache the sample report",
  indent(["Recommend: approve", "", "The sample report reloads on every visit."]),
  "- [ ] sample: Trim the export columns",
  indent([
    "Recommend: deny",
    "",
    "Fewer columns are simpler to read.",
    "",
    "## Question: Columns",
    "Which columns stay?",
    "Pick: one",
    "- Title: The card title.",
    "- Stage: The stage name.",
  ]),
  "",
].join("\n");

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

test("inbox page", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const reset = async () => {
      await seedBoard(origin, JOBS);
      await seedBacklog(origin, BACKLOG);
    };
    await reset();
    const board = async () => (await (await fetch(origin + "/api/board")).json()).jobs;
    const backlogText = async () => (await fetch(origin + "/api/backlog")).text();
    const openPage = async (hash, viewport = { width: 1280, height: 900 }, hidden = null) => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport(viewport);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      if (hidden)
        await page.evaluateOnNewDocument(
          (h) => localStorage.setItem("factory-projects-hidden", h),
          hidden,
        );
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.goto(origin + "/" + hash, { waitUntil: "domcontentloaded" });
      return { page, errors };
    };
    const rows = (page) =>
      page.$$eval("#inbox-list .inbox-item", (items) =>
        items.map((item) =>
          [".inbox-rank", ".inbox-kind", ".inbox-title"].map(
            (sel) => item.querySelector(sel).textContent,
          ),
        ),
      );
    const rowOf = (page, title) =>
      page.waitForFunction(
        (t) =>
          [...document.querySelectorAll("#inbox-list .inbox-item")].find(
            (el) => el.querySelector(".inbox-title").textContent === t,
          ),
        wait,
        title,
      );
    const gone = (page, title) =>
      page.waitForFunction(
        (t) =>
          document.querySelector("#inbox-list .inbox-item") &&
          ![...document.querySelectorAll("#inbox-list .inbox-title")].some(
            (el) => el.textContent === t,
          ),
        wait,
        title,
      );
    const controls = (row) =>
      row.$$eval(".inbox-actions > *", (els) =>
        els.map((el) => [el.tagName, el.textContent, el.getAttribute("href")]),
      );

    if (BEFORE && SHOTS)
      await t.test("before capture", async () => {
        await mkdir(SHOTS, { recursive: true });
        const { page } = await openPage("");
        await page.waitForSelector(".card", wait);
        await page.waitForSelector(".backlog-card, [data-backlog-open]", wait);
        await page.screenshot({ path: `${SHOTS}/before-board-needs.png` });
        await page.browserContext().close();
      });

    const it = (name, fn) => t.test(name, { skip: BEFORE }, fn);

    await it("ranks every waiting item", async () => {
      const { page, errors } = await openPage("#/inbox");
      await page.waitForFunction(
        () => document.querySelectorAll("#inbox-list .inbox-item").length === 5,
        wait,
      );
      assert.match(await page.title(), /^Inbox · /);
      assert.equal(await page.$eval("#inbox-page-heading", (el) => el.textContent), "Inbox");
      assert.equal(await page.evaluate(() => document.body.dataset.view), "inbox");
      assert.deepEqual(await rows(page), [
        ["1", "Question", "Retry the sample upload"],
        ["2", "Escalated PR", "Export the sample report"],
        ["3", "Needs recovery", "Recover the sample host"],
        ["4", "Backlog decision", "Cache the sample report"],
        ["5", "Backlog decision", "Trim the export columns"],
      ]);
      const metas = await page.$$eval("#inbox-list .inbox-meta", (els) =>
        els.map((el) => el.textContent),
      );
      assert.match(metas[0], /051-sample-retry/);
      assert.match(metas[0], /Pick a retry limit/);
      assert.match(metas[3], /Agent recommends approve/);
      assert.match(metas[4], /Agent recommends deny/);
      assert.equal(await page.$eval("#inbox-status", (el) => el.textContent), "");
      assert.deepEqual(errors, []);
      if (SHOTS) {
        await mkdir(SHOTS, { recursive: true });
        await page.screenshot({ path: `${SHOTS}/after-inbox-desktop.png` });
        await page.browserContext().close();
        const phone = await openPage("#/inbox", { width: 390, height: 844 });
        await phone.page.waitForFunction(
          () => document.querySelectorAll("#inbox-list .inbox-item").length === 5,
          wait,
        );
        assert.equal(
          await phone.page.evaluate(
            () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
          ),
          true,
        );
        await phone.page.screenshot({ path: `${SHOTS}/after-inbox-mobile.png` });
        await phone.page.browserContext().close();
        return;
      }
      await page.browserContext().close();
    });

    await it("questions link to the drawer", async () => {
      const { page } = await openPage("#/inbox");
      const row = await rowOf(page, "Trim the export columns");
      const [[tag, text, href], ...rest] = await controls(row);
      assert.deepEqual([tag, text, rest], ["A", "Answer questions", []]);
      assert.match(href, /^#\/backlog\/[0-9a-f]{12}$/);
      const cache = await controls(await rowOf(page, "Cache the sample report"));
      assert.deepEqual(
        cache.map(([tag, text]) => [tag, text]),
        [
          ["BUTTON", "Approve"],
          ["BUTTON", "Deny"],
          ["A", "Open"],
        ],
      );
      assert.match(cache[2][2], /^#\/backlog\/[0-9a-f]{12}$/);
      await page.browserContext().close();
    });

    await it("links open the drawer", async () => {
      const { page } = await openPage("#/inbox");
      assert.deepEqual(await controls(await rowOf(page, "Retry the sample upload")), [
        ["A", "Answer", "#051-sample-retry/needs"],
      ]);
      assert.deepEqual(await controls(await rowOf(page, "Recover the sample host")), [
        ["A", "Open job", "#046-sample-human"],
      ]);
      assert.deepEqual(await controls(await rowOf(page, "Export the sample report")), [
        ["BUTTON", "Babysit PR #44", null],
        ["A", "Open PR #44", "https://github.com/example/sample/pull/44"],
        ["A", "Open job", "#042-sample-export"],
      ]);
      await page.click('#inbox-list a[href="#046-sample-human"]');
      await page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("046-sample-human"),
        wait,
      );
      await page.browserContext().close();
    });

    for (const [name, pick, decision] of [
      ["approve in place", "approved", "approved"],
      ["deny in place", "denied", "denied"],
    ])
      await it(name, async () => {
        await reset();
        const { page, errors } = await openPage("#/inbox");
        const row = await rowOf(page, "Cache the sample report");
        await (await row.$(`[data-inbox-decide="${pick}"]`)).click();
        await gone(page, "Cache the sample report");
        const added = (await board()).find(
          (j) => j.backlog?.line === "- [ ] sample: Cache the sample report",
        );
        assert.equal(added?.backlog.decision, decision);
        assert.match(await backlogText(), /^- \[x\] sample: Cache the sample report/m);
        assert.equal(await page.$eval("#inbox-status", (el) => el.textContent), "");
        assert.deepEqual(errors, []);
        await page.browserContext().close();
      });

    await it("babysit in place", async () => {
      await reset();
      const { page } = await openPage("#/inbox");
      const row = await rowOf(page, "Export the sample report");
      let accept = false;
      page.on("dialog", (dialog) => (accept ? dialog.accept() : dialog.dismiss()));
      await (await row.$("[data-inbox-babysit]")).click();
      const after = (await board()).find((j) => j.id === "042-sample-export");
      assert.equal(after.babysit, undefined);
      assert.equal(after.escalated, true);
      accept = true;
      await (
        await rowOf(page, "Export the sample report")
      ).$eval("[data-inbox-babysit]", (b) => b.click());
      await gone(page, "Export the sample report");
      assert.ok((await board()).find((j) => j.id === "042-sample-export").babysit);
      await page.browserContext().close();
    });

    await it("focus follows the action", async () => {
      await reset();
      const { page } = await openPage("#/inbox");
      const focused = () =>
        page.evaluate(() => document.activeElement.textContent || document.activeElement.id);
      await (
        await rowOf(page, "Cache the sample report")
      ).$eval('[data-inbox-decide="approved"]', (b) => b.focus());
      await page.keyboard.press("Enter");
      await gone(page, "Cache the sample report");
      await page.waitForFunction(() => document.activeElement !== document.body, wait);
      assert.equal(await focused(), "Answer questions");
      page.on("dialog", (dialog) => dialog.accept());
      await (
        await rowOf(page, "Export the sample report")
      ).$eval("[data-inbox-babysit]", (b) => b.focus());
      await page.keyboard.press("Enter");
      await gone(page, "Export the sample report");
      await page.waitForFunction(() => document.activeElement !== document.body, wait);
      assert.equal(await focused(), "Open job");
      await page.browserContext().close();
      await seedBoard(origin, [JOBS[1]]);
      await seedBacklog(origin, BACKLOG.split("- [ ] sample: Trim")[0]);
      const last = await openPage("#/inbox");
      await (
        await rowOf(last.page, "Cache the sample report")
      ).$eval('[data-inbox-decide="denied"]', (b) => b.focus());
      await last.page.keyboard.press("Enter");
      await last.page.waitForFunction(
        () => document.querySelector("#inbox-status").textContent === "Nothing is waiting on you.",
        wait,
      );
      assert.equal(await last.page.evaluate(() => document.activeElement.id), "inbox-page-heading");
      await last.page.browserContext().close();
    });

    await it("keys and jumper", async () => {
      await reset();
      const { page } = await openPage("");
      await page.waitForSelector(".card", wait);
      await page.keyboard.press("i");
      await page.waitForSelector("#inbox-page:not([hidden])", wait);
      assert.equal(await page.evaluate(() => location.hash), "#/inbox");
      assert.equal(await page.evaluate(() => document.activeElement.id), "inbox-page-heading");
      await rowOf(page, "Cache the sample report");
      const before = JSON.stringify(await board());
      await page.keyboard.press("a");
      await page.keyboard.press("b");
      assert.equal(await page.$("#job-picker[open]"), null);
      assert.equal(JSON.stringify(await board()), before);
      assert.equal(await page.evaluate(() => location.hash), "#/inbox");
      await page.keyboard.press("h");
      await page.waitForSelector("#inbox-page", { hidden: true, ...wait });
      assert.equal(await page.evaluate(() => document.body.dataset.view), "board");
      await page.keyboard.press("j");
      await page.waitForSelector("#job-picker[open]", wait);
      await page.type("#job-search", "inbox");
      assert.equal(
        await page.$eval(".job-option", (el) => el.textContent),
        "InboxCommand · Everything waiting on you (I)",
      );
      await page.keyboard.press("Enter");
      await page.waitForSelector("#inbox-page:not([hidden])", wait);
      await page.click("#inbox-back");
      await page.waitForSelector("#inbox-page", { hidden: true, ...wait });
      assert.equal(await page.evaluate(() => location.hash), "");
      await page.browserContext().close();
    });

    await it("empty and filtered", async () => {
      await reset();
      const filtered = await openPage("#/inbox", undefined, "sample");
      await filtered.page.waitForFunction(
        () => document.querySelector("#inbox-status").textContent === "Nothing is waiting on you.",
        wait,
      );
      assert.equal(await filtered.page.$("#inbox-list .inbox-item"), null);
      await filtered.page.browserContext().close();
      await seedBoard(origin, [JOBS[1]]);
      await seedBacklog(origin, "# Backlog\n");
      const { page } = await openPage("#/inbox");
      await page.waitForFunction(
        () => document.querySelector("#inbox-status").textContent === "Nothing is waiting on you.",
        wait,
      );
      assert.equal(await page.$("#inbox-list .inbox-item"), null);
      await page.browserContext().close();
    });
  });
});
