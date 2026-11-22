// The Cost per merged PR section of the retros page. GET /api/spend is answered by request
// interception. Rows are the made-up running example.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture } from "../support/browser-server.mjs";

const ROWS = [
  {
    project: "sample",
    week: "2026-11-16",
    merged_prs: 2,
    tokens: 800000,
    tokens_per_pr: 400000,
    judged: 2,
    reverted: 1,
    reworked: 0,
  },
  {
    project: "sample",
    week: "2026-11-09",
    merged_prs: 1,
    tokens: 300000,
    tokens_per_pr: 300000,
    judged: 0,
    reverted: 0,
    reworked: 0,
  },
  {
    project: "<b>zeta</b>",
    week: "2026-11-16",
    merged_prs: 0,
    tokens: 140800,
    tokens_per_pr: null,
    judged: 0,
    reverted: 0,
    reworked: 0,
  },
];
const LOADED =
  "Last 8 weeks, grouped by the UTC Monday each job finished. Tokens are input, output and cache write from every merged or closed job, divided by distinct merged PRs. Jobs still running are not counted. Reverted counts PRs that factory.py outcomes record has judged.";
const HEADERS = ["Week of (UTC)", "Merged PRs", "Tokens", "Tokens per merged PR", "Reverted"];

test(
  "the retros page shows cost per merged PR per project, and its empty and failed copy",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
        const errors = [];
        page.on("pageerror", (error) => errors.push(String(error)));
        let answer = { status: 200, rows: ROWS };
        await page.setRequestInterception(true);
        page.on("request", (request) => {
          if (new URL(request.url()).pathname !== "/api/spend") return request.continue();
          return request.respond({
            status: answer.status,
            contentType: "application/json",
            body: JSON.stringify({ schema: "factory.spend/1", weeks: 8, rows: answer.rows }),
          });
        });
        const open = async (text) => {
          if (page.url().endsWith("#/retros")) await page.reload({ waitUntil: "domcontentloaded" });
          else await page.goto(origin + "/#/retros", { waitUntil: "domcontentloaded" });
          await page.waitForFunction(
            (want) => document.getElementById("spend-status").textContent.startsWith(want),
            {},
            text,
          );
          return page.$eval("#spend", (section) => ({
            previous: section.previousElementSibling.id,
            next: section.nextElementSibling.id,
            heading: section.querySelector("h3").textContent,
            status: section.querySelector("#spend-status").textContent,
            projects: [...section.querySelectorAll("#spend-tables > h4")].map((h) => h.innerHTML),
            regions: [...section.querySelectorAll("#spend-tables > .reference-scroll")].map(
              (div) => [div.tabIndex, div.getAttribute("role"), div.getAttribute("aria-label")],
            ),
            tables: [...section.querySelectorAll("table.reference-table")].map((table) => ({
              headers: [...table.querySelectorAll("th[scope=col]")].map((th) => th.textContent),
              rows: [...table.tBodies[0].rows].map((tr) =>
                [...tr.cells].map((td) => td.textContent),
              ),
            })),
            bold: section.querySelectorAll("b").length,
          }));
        };

        const shown = await open("Last 8 weeks");
        assert.equal(shown.previous, "audit-flags");
        assert.equal(shown.next, "retros-runs-heading");
        assert.equal(shown.heading, "Cost per merged PR");
        assert.equal(shown.status, LOADED);
        assert.deepEqual(shown.projects, ["sample", "&lt;b&gt;zeta&lt;/b&gt;"]);
        assert.deepEqual(shown.regions, [
          [0, "region", "Cost per merged PR for sample"],
          [0, "region", "Cost per merged PR for <b>zeta</b>"],
        ]);
        assert.equal(shown.bold, 0);
        assert.deepEqual(shown.tables, [
          {
            headers: HEADERS,
            rows: [
              ["2026-11-16", "2", "800,000", "400,000", "1 of 2 (50%)"],
              ["2026-11-09", "1", "300,000", "300,000", "none judged"],
            ],
          },
          {
            headers: HEADERS,
            rows: [["2026-11-16", "0", "140,800", "none merged", "none judged"]],
          },
        ]);

        answer = { status: 200, rows: [] };
        const empty = await open("No merged");
        assert.equal(empty.status, "No merged or closed jobs in the last 8 weeks.");
        assert.deepEqual(empty.tables, []);

        answer = { status: 500, rows: ROWS };
        const failed = await open("Could not");
        assert.equal(failed.status, "Could not load cost per merged PR. Reload to try again.");
        assert.deepEqual(failed.tables, []);
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }
    });
  },
);
