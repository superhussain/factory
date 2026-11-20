// The inbox page against one owned local Worker, with a synthetic board and backlog for project
// sample. INBOX_SHOTS names a folder for captures. INBOX_SHOT_STAGE=before takes only the before
// captures and skips every other subtest, so the file runs against an older checkout.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const wait = { polling: 50 };
const BOARD_COPY =
  "Couldn't load the inbox. Can't reach the factory. Check your connection, then retry.";
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

const NEEDS = [
  "## Question: Retry limit",
  "How many times should the upload retry?",
  "Pick: one",
  "- 3 (recommended): Enough for a short outage.",
  "- 5: Slower to fail.",
  "",
].join("\n");

async function putFile(origin, id, name, body) {
  const path = `/api/jobs/sample/${id}/${name}`;
  const current = await fetch(origin + path);
  const condition = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  const r = await fetch(origin + path, { method: "PUT", headers: condition, body });
  assert.equal(r.status, 200, path);
}

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
    await putFile(origin, "051-sample-retry", "needs.md", NEEDS);
    const board = async () => (await (await fetch(origin + "/api/board")).json()).jobs;
    const backlogText = async () => (await fetch(origin + "/api/backlog")).text();
    const openPage = async (
      hash,
      viewport = { width: 1280, height: 900 },
      hidden = null,
      failBacklog = false,
    ) => {
      const page = await (await browser.createBrowserContext()).newPage();
      if (failBacklog) {
        await page.setRequestInterception(true);
        page.on("request", (request) =>
          request.method() === "GET" && new URL(request.url()).pathname === "/api/backlog"
            ? request.respond({ status: 503, body: "" })
            : request.continue(),
        );
      }
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

    const fiveRows = (page) =>
      page.waitForFunction(
        () => document.querySelectorAll("#inbox-list .inbox-item").length === 5,
        wait,
      );
    const BACKLOG_ERROR =
      "Could not load the backlog, so backlog rows may be missing or out of date.";
    const answerForm = async (page) => {
      await (
        await rowOf(page, "Retry the sample upload")
      ).$eval("[data-inbox-answer]", (b) => b.click());
      await page.waitForSelector("#inbox-answer .inbox-needs fieldset.ask", wait);
    };
    const openRow3 = async (page) => {
      await (
        await rowOf(page, "Recover the sample host")
      ).$eval('a[href="#046-sample-human"]', (a) => a.click());
      await page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("046-sample-human"),
        wait,
      );
    };
    const shotKeys = async (name) => {
      const { page } = await openPage("#/settings/reference");
      await page.waitForSelector("#settings-reference:not([hidden])", wait);
      await (await page.$("#reference-keys")).screenshot({ path: `${SHOTS}/${name}` });
      await page.browserContext().close();
    };
    const openFailedBoard = async () => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 1280, height: 900 });
      await blockLiveEvents(page);
      await page.setBypassServiceWorker(true);
      await page.setRequestInterception(true);
      const state = { fail: true, hold: null };
      page.on("request", async (request) => {
        if (
          state.fail &&
          request.method() === "GET" &&
          new URL(request.url()).pathname === "/board.json"
        ) {
          await state.hold;
          request.respond({ status: 503, body: "" });
        } else request.continue();
      });
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.goto(origin + "/#/inbox", { waitUntil: "domcontentloaded" });
      await page.waitForFunction(
        (copy) => document.querySelector("#inbox-status").textContent === copy,
        wait,
        BOARD_COPY,
      );
      return { page, errors, state };
    };
    const heldRetry = async (page, state) => {
      // The log starts at the Retry click, so a redraw that lands before it never counts.
      await page.evaluate(() => {
        const status = document.querySelector("#inbox-status");
        window.inboxStatusLog = [];
        document.addEventListener(
          "click",
          () =>
            new MutationObserver(() => window.inboxStatusLog.push(status.textContent)).observe(
              status,
              { childList: true, characterData: true, subtree: true },
            ),
          { capture: true, once: true },
        );
      });
      let release;
      state.hold = new Promise((resolve) => (release = resolve));
      const backlog = page.waitForResponse(
        (response) => new URL(response.url()).pathname === "/api/backlog",
      );
      // Stands in for the first load's backlog redraw landing just before the click (CI run 44478429364).
      await page.evaluate(() => renderInboxPage());
      await page.click("#inbox-retry");
      await backlog;
      await new Promise((resolve) => setTimeout(resolve, 500));
      return release;
    };
    const statusLog = (page) =>
      page.evaluate(() => window.inboxStatusLog.filter((text, i, log) => text !== log[i - 1]));

    if (BEFORE && SHOTS)
      await t.test("before capture", async () => {
        await mkdir(SHOTS, { recursive: true });
        const { page } = await openPage("#/inbox");
        await fiveRows(page);
        await page.screenshot({ path: `${SHOTS}/before-inbox.png` });
        await page.browserContext().close();
        const failed = await openPage("#/inbox", undefined, null, true);
        await failed.page.waitForSelector(".card", wait);
        await failed.page.screenshot({ path: `${SHOTS}/before-inbox-backlog-error.png` });
        await failed.page.browserContext().close();
        await shotKeys("before-reference-keys.png");
        const opened = await openPage("#/inbox");
        await openRow3(opened.page);
        await opened.page.screenshot({ path: `${SHOTS}/before-inbox-open-job-desktop.png` });
        await opened.page.browserContext().close();
        const held = await openFailedBoard();
        const release = await heldRetry(held.page, held.state);
        await (
          await held.page.$("#inbox-page")
        ).screenshot({
          path: `${SHOTS}/before-inbox-retrying.png`,
        });
        release();
        await held.page.browserContext().close();
      });

    const it = (name, fn) => t.test(name, { skip: BEFORE }, fn);

    await it("ranks every waiting item", async () => {
      const { page, errors } = await openPage("#/inbox");
      await fiveRows(page);
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
        ["BUTTON", "Answer", null],
        ["A", "Open job", "#051-sample-retry/needs"],
      ]);
      assert.deepEqual(await controls(await rowOf(page, "Recover the sample host")), [
        ["BUTTON", "Rework…", null],
        ["BUTTON", "Approve as is", null],
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

    await it("answer in place", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      await answerForm(page);
      const button = await (await rowOf(page, "Retry the sample upload")).$("[data-inbox-answer]");
      assert.deepEqual(
        await button.evaluate((b) => [
          b.getAttribute("aria-expanded"),
          b.getAttribute("aria-controls"),
        ]),
        ["true", "inbox-answer"],
      );
      assert.equal(await page.$eval("#inbox-answer legend", (el) => el.textContent), "Retry limit");
      assert.equal(await page.$eval("#inbox-answer [type=submit]", (b) => b.disabled), true);
      assert.equal(
        await page.$eval("#inbox-answer .inbox-ask-status", (el) => el.textContent),
        "Answer every question to submit. 0 of 1 answered.",
      );
      if (SHOTS) {
        await mkdir(SHOTS, { recursive: true });
        await page.screenshot({ path: `${SHOTS}/after-inbox-answer-desktop.png` });
        const phone = await openPage("#/inbox", { width: 390, height: 844 });
        await answerForm(phone.page);
        assert.equal(
          await phone.page.evaluate(
            () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
          ),
          true,
        );
        await phone.page.screenshot({ path: `${SHOTS}/after-inbox-answer-mobile.png` });
        await phone.page.browserContext().close();
      }
      await page.click('#inbox-answer input[value="0"]');
      await page.click("#inbox-answer [type=submit]");
      await gone(page, "Retry the sample upload");
      assert.match(
        await (await fetch(`${origin}/jobs/sample/051-sample-retry/answers.md`)).text(),
        /^- \[x\] 3 \(recommended\): Enough for a short outage\.$/m,
      );
      assert.equal((await board()).find((j) => j.id === "051-sample-retry").needs, undefined);
      await page.waitForFunction(
        () =>
          document.querySelector("#inbox-status").textContent ===
          "Saved to answers.md for 051-sample-retry. The agent reads it when the job continues.",
        wait,
      );
      await page.waitForFunction(() => document.activeElement !== document.body, wait);
      assert.equal(await page.evaluate(() => document.activeElement.textContent), "Babysit PR #44");
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("answer survives a redraw", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      await answerForm(page);
      await page.focus("#inbox-answer textarea");
      await page.keyboard.type("soon");
      await seedBoard(origin, [
        ...JOBS,
        job("052-sample-bucket", "build", {
          title: "Name the sample bucket",
          needs: "Pick a bucket name in #052-sample-bucket/needs",
          updated: "2026-01-04T00:00:00Z",
        }),
      ]);
      await rowOf(page, "Name the sample bucket");
      assert.deepEqual(
        await page.evaluate(() => [
          document.activeElement.matches("#inbox-answer textarea"),
          document.activeElement.value,
          document.activeElement.selectionStart,
          document.activeElement.closest(".inbox-item").querySelector(".inbox-title").textContent,
        ]),
        [true, "soon", 4, "Retry the sample upload"],
      );
      assert.equal(await page.$eval("#inbox-answer [type=submit]", (b) => b.disabled), false);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("answer save fails", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      await page.setRequestInterception(true);
      page.on("request", (request) =>
        request.method() === "PUT" && request.url().endsWith("/answers.md")
          ? request.respond({ status: 500, body: "" })
          : request.continue(),
      );
      await answerForm(page);
      assert.equal(await page.$eval("#inbox-answer", (el) => el.className), "inbox-answer");
      await page.click('#inbox-answer input[value="0"]');
      await page.click("#inbox-answer [type=submit]");
      await page.waitForFunction(
        () =>
          document.querySelector("#inbox-answer .inbox-ask-status").textContent ===
          "Could not save answers (status 500). Your answers are kept. Use Open job to check answers.md before you submit again.",
        wait,
      );
      const onSubmit = () =>
        page.evaluate(() => document.activeElement.matches("#inbox-answer [type=submit]"));
      assert.equal(await onSubmit(), true);
      await seedBoard(origin, [
        ...JOBS,
        job("052-sample-bucket", "build", {
          title: "Name the sample bucket",
          needs: "Pick a bucket name in #052-sample-bucket/needs",
          updated: "2026-01-04T00:00:00Z",
        }),
      ]);
      await rowOf(page, "Name the sample bucket");
      assert.equal(await onSubmit(), true);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("answer shows clef-flash picks", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      const record = {
        sha256: createHash("sha256").update(NEEDS).digest("hex"),
        choices: [
          {
            header: "Retry limit",
            pick: "3",
            confidence: 64,
            options: [
              { label: "3", chance: 64, reason: "A short outage clears in three tries." },
              { label: "5", chance: 36, reason: "More tries ride out a longer outage." },
            ],
          },
        ],
      };
      await page.setRequestInterception(true);
      page.on("request", (request) =>
        request.method() === "GET" &&
        new URL(request.url()).pathname === "/api/jobs/sample/051-sample-retry/needs-triage.json"
          ? request.respond({
              status: 200,
              contentType: "application/json",
              body: JSON.stringify(record),
            })
          : request.continue(),
      );
      await answerForm(page);
      assert.equal(
        await page.$eval("#inbox-answer .ask-model", (el) => el.textContent),
        "clef-flash picks 3 at 64%.",
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("answer conflict asks once", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      await page.setRequestInterception(true);
      page.on("request", (request) =>
        request.method() === "PUT" && request.url().endsWith("/answers.md")
          ? request.respond({ status: 412, body: "" })
          : request.continue(),
      );
      await answerForm(page);
      await page.click('#inbox-answer input[value="0"]');
      await page.click("#inbox-answer [type=submit]");
      await page.waitForFunction(
        () =>
          document
            .querySelector("#inbox-answer .inbox-ask-status")
            .textContent.startsWith("answers.md"),
        wait,
      );
      assert.equal(
        await page.$eval("#inbox-answer .inbox-ask-status", (el) => el.textContent),
        "answers.md changed since this page loaded. Your answers are kept. Use Open job to check answers.md before you submit again.",
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("answer retry after a failed board clear", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      let boardPuts = 0;
      const answerPuts = [];
      await page.setRequestInterception(true);
      page.on("request", (request) =>
        request.method() === "PATCH" &&
        new URL(request.url()).pathname === "/api/board" &&
        boardPuts++ === 0
          ? request.respond({ status: 503, body: "" })
          : request.continue(),
      );
      page.on("response", (response) => {
        if (response.request().method() === "PUT" && response.url().endsWith("/answers.md"))
          answerPuts.push(response.status());
      });
      await answerForm(page);
      await page.click('#inbox-answer input[value="0"]');
      await page.click("#inbox-answer [type=submit]");
      await page.waitForFunction(
        () =>
          document
            .querySelector("#inbox-answer .inbox-ask-status")
            .textContent.startsWith("Saved to answers.md, but the board still shows"),
        wait,
      );
      await page.waitForSelector("#inbox-answer [type=submit]:not([disabled])", wait);
      await page.click("#inbox-answer [type=submit]");
      await gone(page, "Retry the sample upload");
      assert.deepEqual(answerPuts, [200, 200]);
      assert.equal((await board()).find((j) => j.id === "051-sample-retry").needs, undefined);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("answer needs the question format", async () => {
      await reset();
      await putFile(origin, "051-sample-retry", "needs.md", "# Needs\n\nPick a retry limit.\n");
      const { page } = await openPage("#/inbox");
      await (
        await rowOf(page, "Retry the sample upload")
      ).$eval("[data-inbox-answer]", (b) => b.click());
      await page.waitForFunction(
        () =>
          document.querySelector("#inbox-answer")?.textContent ===
          "needs.md is not in the question format. Open the job to read it.",
        wait,
      );
      assert.equal((await page.$('#inbox-list a[href="#051-sample-retry/needs"]')) !== null, true);
      assert.equal(await page.$eval("#inbox-answer", (el) => el.className), "inbox-answer muted");
      await page.browserContext().close();
      await putFile(origin, "051-sample-retry", "needs.md", NEEDS);
    });

    await it("backlog fails", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox", undefined, null, true);
      await page.waitForFunction(
        (text) => document.querySelector("#inbox-status").textContent === text,
        wait,
        BACKLOG_ERROR,
      );
      assert.deepEqual(await rows(page), [
        ["1", "Question", "Retry the sample upload"],
        ["2", "Escalated PR", "Export the sample report"],
        ["3", "Needs recovery", "Recover the sample host"],
      ]);
      if (SHOTS) {
        await mkdir(SHOTS, { recursive: true });
        await page.screenshot({ path: `${SHOTS}/after-inbox-backlog-error.png` });
      }
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("drawer returns to the inbox", async () => {
      await reset();
      const { page } = await openPage("#/inbox");
      for (const close of [() => page.click("#d-close"), () => page.keyboard.press("Escape")]) {
        await (
          await rowOf(page, "Recover the sample host")
        ).$eval('a[href="#046-sample-human"]', (a) => a.click());
        await page.waitForFunction(
          () => document.querySelector("#d-meta")?.textContent.includes("046-sample-human"),
          wait,
        );
        await close();
        await page.waitForSelector("#inbox-page:not([hidden])", wait);
        assert.equal(await page.evaluate(() => location.hash), "#/inbox");
      }
      await page.browserContext().close();
      const fromBoard = await openPage("");
      await fromBoard.page.waitForSelector(".card", wait);
      await fromBoard.page.evaluate(() => (location.hash = "#046-sample-human"));
      await fromBoard.page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("046-sample-human"),
        wait,
      );
      await fromBoard.page.click("#d-close");
      await fromBoard.page.waitForFunction(() => location.hash === "", wait);
      assert.equal(await fromBoard.page.evaluate(() => document.body.dataset.view), "board");
      await fromBoard.page.browserContext().close();
    });

    await it("split drawer keeps the inbox", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      await openRow3(page);
      assert.deepEqual(
        await page.evaluate(() => {
          const inbox = document.querySelector("#inbox-page");
          const detail = document.querySelector("#detail");
          return {
            view: document.body.dataset.view,
            split: document.body.hasAttribute("data-inbox-split"),
            inbox: inbox.checkVisibility(),
            detail: detail.checkVisibility(),
            current: [...document.querySelectorAll('#inbox-list [aria-current="true"]')].map(
              (el) => el.querySelector(".inbox-title").textContent,
            ),
            beside:
              inbox.getBoundingClientRect().right <= detail.getBoundingClientRect().left + 0.5,
            background: (() => {
              const probe = document.createElement("div");
              probe.style.background = "var(--accent-soft)";
              document.body.append(probe);
              const soft = getComputedStyle(probe).backgroundColor;
              probe.remove();
              const row = document.querySelector('#inbox-list [aria-current="true"]');
              return getComputedStyle(row).backgroundColor === soft;
            })(),
          };
        }),
        {
          view: "inbox",
          split: true,
          inbox: true,
          detail: true,
          current: ["Recover the sample host"],
          beside: true,
          background: true,
        },
      );
      if (SHOTS) {
        const settled = () => document.querySelector("#d-doc").textContent !== "loading files";
        await mkdir(SHOTS, { recursive: true });
        await page.waitForFunction(settled, wait);
        await page.screenshot({ path: `${SHOTS}/after-inbox-split-desktop.png` });
        const phone = await openPage("#/inbox", { width: 390, height: 844 });
        await openRow3(phone.page);
        await phone.page.waitForFunction(settled, wait);
        await phone.page.screenshot({ path: `${SHOTS}/after-inbox-split-mobile.png` });
        await phone.page.browserContext().close();
      }
      const focusesOpenRowLink = async () =>
        assert.ok(
          await page.evaluate(() => {
            const a = document.activeElement;
            return (
              a.matches("a.btn") &&
              a.getAttribute("href") === location.hash &&
              a.closest('[aria-current="true"]')?.parentElement.id === "inbox-list"
            );
          }),
        );
      await page.keyboard.press("ArrowRight");
      await page.waitForFunction(
        () =>
          /^#\/backlog\/[0-9a-f]{12}$/.test(location.hash) &&
          document.querySelector("#d-title").textContent === "Backlog item",
        wait,
      );
      await focusesOpenRowLink();
      await page.keyboard.press("ArrowLeft");
      await page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("046-sample-human"),
        wait,
      );
      await focusesOpenRowLink();
      await page.keyboard.press("ArrowLeft");
      await page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("042-sample-export"),
        wait,
      );
      await focusesOpenRowLink();
      assert.equal(await page.evaluate(() => location.hash), "#042-sample-export");
      await page.keyboard.press("Escape");
      await page.waitForFunction(() => location.hash === "#/inbox", wait);
      assert.deepEqual(
        await page.evaluate(() => [
          document.body.hasAttribute("data-inbox-split"),
          document.activeElement.textContent,
        ]),
        [false, "Babysit PR #44"],
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });
    await it("a stale arrow step does not pull focus later", async () => {
      await reset();
      const { page, errors } = await openPage("#/inbox");
      await openRow3(page);
      // A step whose row never rendered as the open one, then a render that drops it.
      await page.evaluate(() => {
        inboxStepHref = "#042-sample-export";
        renderInboxPage();
        document.querySelector("#inbox-page-heading").focus();
        location.hash = "#042-sample-export";
      });
      await page.waitForFunction(
        () => document.querySelector("#d-meta")?.textContent.includes("042-sample-export"),
        wait,
      );
      assert.equal(await page.evaluate(() => document.activeElement.id), "inbox-page-heading");
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("split inbox keeps 400px at 800 to 1100", async () => {
      await reset();
      for (const width of [800, 900, 1100, 1280]) {
        const { page, errors } = await openPage("#/inbox", { width, height: 900 });
        await openRow3(page);
        const box = await page.evaluate(() => {
          const inbox = document.querySelector("#inbox-page").getBoundingClientRect();
          const detail = document.querySelector("#detail").getBoundingClientRect();
          return {
            inbox: inbox.width,
            detail: detail.width,
            beside: inbox.right <= detail.left + 0.5,
          };
        });
        if (width === 1280) assert.equal(box.detail, 760, `drawer at ${width}`);
        else {
          assert.ok(box.inbox >= 360, `inbox at ${width}: ${box.inbox}`);
          assert.ok(box.detail >= 360, `drawer at ${width}: ${box.detail}`);
          assert.ok(box.beside, `overlap at ${width}`);
        }
        assert.deepEqual(errors, []);
        await page.browserContext().close();
      }
    });

    if (SHOTS) await it("reference capture", () => shotKeys("after-reference-keys.png"));

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
      assert.equal(await page.evaluate(() => inboxFocusRow), -1);
      await page.evaluate(() => {
        inboxFocusRow = 3;
        location.hash = "";
      });
      await page.waitForSelector("#inbox-page", { hidden: true, ...wait });
      await page.evaluate(() => (location.hash = "#/inbox"));
      await page.waitForSelector("#inbox-page:not([hidden])", wait);
      assert.equal(await page.evaluate(() => inboxFocusRow), -1);
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
      assert.equal(await focused(), "Rework…");
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

    await it("header inbox link", async () => {
      await reset();
      const phone = { width: 390, height: 844, isMobile: true, hasTouch: true };
      const { page, errors } = await openPage("", phone);
      const link = () =>
        page.$eval("#inbox-link", (el) => ({
          shown: el.getClientRects().length > 0,
          label: el.getAttribute("aria-label"),
          count: document.getElementById("inbox-count").textContent,
          countHidden: document.getElementById("inbox-count").hidden,
        }));
      await page.waitForSelector("#inbox-count:not([hidden])", wait);
      assert.deepEqual(await link(), {
        shown: true,
        label: "Inbox, 5 waiting",
        count: "5",
        countHidden: false,
      });
      await page.tap("#inbox-link");
      await page.waitForFunction(() => location.hash === "#/inbox", wait);
      await page.waitForSelector("#inbox-page:not([hidden])", wait);
      await page.waitForSelector('#inbox-link[aria-current="page"]', wait);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
      await seedBoard(origin, [JOBS[1]]);
      await seedBacklog(origin, "# Backlog\n");
      const empty = await openPage("#/inbox", phone);
      await empty.page.waitForFunction(
        () => document.querySelector("#inbox-status").textContent === "Nothing is waiting on you.",
        wait,
      );
      assert.deepEqual(
        await empty.page.$eval("#inbox-link", (el) => ({
          shown: el.getClientRects().length > 0,
          label: el.getAttribute("aria-label"),
          countHidden: document.getElementById("inbox-count").hidden,
        })),
        { shown: true, label: "Inbox", countHidden: true },
      );
      await empty.page.browserContext().close();
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

    await it("board load failure", async () => {
      await reset();
      const { page, errors, state } = await openFailedBoard();
      assert.equal(await page.$eval("#inbox-retry", (el) => el.hidden), false);
      state.fail = false;
      await page.click("#inbox-retry");
      await page.waitForSelector("#inbox-list .inbox-item", wait);
      await page.waitForFunction(() => document.activeElement.id === "inbox-page-heading", wait);
      assert.equal(await page.$eval("#inbox-retry", (el) => el.hidden), true);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await it("retrying holds until the board read settles", async () => {
      await reset();
      const { page, errors, state } = await openFailedBoard();
      const release = await heldRetry(page, state);
      assert.equal(await page.$eval("#inbox-status", (el) => el.textContent), "Retrying\u2026");
      assert.deepEqual(await statusLog(page), ["Retrying\u2026"]);
      if (SHOTS) {
        await mkdir(SHOTS, { recursive: true });
        await (
          await page.$("#inbox-page")
        ).screenshot({
          path: `${SHOTS}/after-inbox-retrying.png`,
        });
      }
      release();
      await page.waitForFunction(
        (copy) => document.querySelector("#inbox-status").textContent === copy,
        wait,
        BOARD_COPY,
      );
      await page.waitForFunction(() => document.activeElement.id === "inbox-retry", wait);
      assert.equal(await page.$eval("#inbox-retry", (el) => el.disabled), false);
      assert.deepEqual(await statusLog(page), ["Retrying\u2026", BOARD_COPY]);
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });
  });
});
