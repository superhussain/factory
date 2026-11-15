// The job panel lists each agents row in a collapsible table, in board order, and hides the
// table when a job has none.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const run = (at, stage, role, via, harness, effort, extra = {}) => ({
  at,
  stage,
  role,
  harness,
  via,
  model: "model-a",
  effort,
  ...extra,
});
const JOB = {
  id: "101-wrap-doc-line",
  project: "sample",
  title: "Wrap the long doc line",
  stage: "review",
  round: 1,
  updated: "2026-01-02T13:10:00Z",
  agents: [
    run("2026-01-02T12:00:00Z", "idea", "orchestrator", "launcher", "provider-a", "high", {
      paseo: { server: "srv-a", agent: "agent-orch" },
    }),
    run("2026-01-02T12:05:00Z", "spec", "spec", "native", "claude", "medium", {
      paseo: { server: "srv-a", agent: "bad id!" },
    }),
    run("2026-01-02T13:10:00Z", "review", "review-ui", "native", "claude", "medium", { round: 1 }),
  ],
};
const PLAIN = {
  id: "102-move-comment",
  project: "sample",
  title: "Move the comment",
  stage: "build",
  round: 0,
  updated: "2026-01-02T12:00:00Z",
};
// A lead with PR and session links in the drawer header, which narrow the title column.
const LEAD = {
  ...JOB,
  id: "090-sample-export-csv",
  title: "Synthetic lead",
  stage: "pr-open",
  pr: "https://github.com/example/sample/pull/90",
  paseo: { server: "srv-a", agent: "agent-lead" },
};

test("agent runs table", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB, PLAIN, LEAD]);
    const errors = [];
    const page = await browser.newPage();
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.setViewport({ width: 1280, height: 900 });
    await page.goto(`${origin}/#${JOB.id}`, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => !document.getElementById("d-agents").hidden);
    assert.equal(await page.$eval("#d-agents summary", (el) => el.textContent), "Agents: 3 runs");
    const rows = await page.$$eval("#d-agents tbody tr", (trs) =>
      trs.map((tr) => [...tr.cells].map((td) => td.textContent)),
    );
    assert.equal(rows.length, 3);
    assert.deepEqual(rows[2], [
      "2026-01-02 13:10 UTC",
      "review-ui",
      "",
      "review",
      "1",
      "model-a",
      "medium",
      "claude",
      "native",
    ]);
    assert.equal(rows[0][4], "");
    assert.deepEqual(
      await page.$$eval("#d-agents thead th", (ths) => ths.map((th) => th.textContent)),
      ["When", "Role", "Session", "Stage", "Round", "Model", "Effort", "Harness", "Via"],
    );
    await page.click("#d-agents summary");
    const edges = await page.$eval("#d-agents .table-wrap", (wrap) => {
      wrap.scrollLeft = 0;
      return [
        wrap.querySelectorAll("th")[2].getBoundingClientRect().right,
        wrap.getBoundingClientRect().right,
      ];
    });
    assert.ok(edges[0] > 0 && edges[0] <= edges[1], JSON.stringify(edges));
    const links = await page.$$eval("#d-agents tbody tr", (trs) =>
      trs.map((tr) =>
        [...tr.cells[2].querySelectorAll("a")].map((a) => [
          a.getAttribute("href"),
          a.textContent,
          a.getAttribute("aria-label"),
          a.getAttribute("target"),
          a.getAttribute("rel"),
        ]),
      ),
    );
    assert.deepEqual(links, [
      [
        [
          "session://h/srv-a/agent/agent-orch",
          "Session app",
          "Open the orchestrator run in Session app",
          null,
          null,
        ],
        [
          "https://sessions.example.test/srv-a/agent-orch",
          "Session",
          "Open the orchestrator run in Session",
          "_blank",
          "noopener noreferrer",
        ],
      ],
      [],
      [],
    ]);
    await page.evaluate((id) => (location.hash = id), PLAIN.id);
    await page.waitForFunction(
      (id) => document.getElementById("d-title").textContent === id,
      {},
      PLAIN.title,
    );
    assert.equal(await page.$eval("#d-agents", (el) => el.hidden), true);

    const openLead = async () => {
      await page.evaluate((id) => (location.hash = id), LEAD.id);
      await page.waitForFunction(
        (title) => document.getElementById("d-title").textContent === title,
        {},
        LEAD.title,
      );
      await page.waitForSelector("#d-pr:not([hidden])", { visible: true });
      await page.waitForSelector("#d-paseo:not([hidden])", { visible: true });
      await page.waitForSelector("#d-agents:not([hidden])");
      await page.evaluate(() => (document.getElementById("d-agents").open = true));
      return page.evaluate(() => {
        const head = document.querySelector(".detail-head");
        const wrap = document.querySelector("#d-agents .table-wrap");
        wrap.scrollLeft = 0;
        const style = getComputedStyle(head);
        return {
          session: wrap.querySelectorAll("th")[2].getBoundingClientRect().right,
          wrap: wrap.getBoundingClientRect().right,
          head: head.getBoundingClientRect().right - parseFloat(style.paddingRight),
          agentsTop: document.getElementById("d-agents").getBoundingClientRect().top,
          linksBottom: head.querySelector(".links").getBoundingClientRect().bottom,
        };
      });
    };
    const desktop = await openLead();
    assert.ok(desktop.session <= desktop.wrap, JSON.stringify(desktop));
    assert.ok(Math.abs(desktop.wrap - desktop.head) <= 1, JSON.stringify(desktop));
    assert.ok(desktop.agentsTop >= desktop.linksBottom, JSON.stringify(desktop));
    await page.evaluate((id) => (location.hash = id), PLAIN.id);
    await page.setViewport({ width: 390, height: 844 });
    const mobile = await openLead();
    assert.ok(mobile.agentsTop >= mobile.linksBottom, JSON.stringify(mobile));
    assert.deepEqual(errors, []);
  });
});
