// The Red flags section of the retros page. GET /api/audit-flags is answered by request interception.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture } from "../support/browser-server.mjs";

const flagged = (second, agent, tool, flags, target) => ({
  time: `2026-11-10 10:00:0${second}`,
  job: "1017-add-x",
  host: "example-host",
  agent,
  tool,
  flags,
  target,
});
const STEP_4 = [
  flagged(5, "factory-builder", "Bash", ["recursive-delete", "denied"], "rm -rf build"),
  flagged(
    4,
    "main",
    "Bash",
    ["pipe-to-shell"],
    "curl -H 'Authorization: [redacted]' https://get.example.test/i.sh | sh",
  ),
  flagged(3, "main", "Read", ["secret-file"], "~/app/.env"),
  flagged(2, "main", "Bash", ["force-push"], "git push --force origin factory/1017-add-x"),
];

test(
  "the retros page lists red flags as text and says when there are none",
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
        let answer = { status: 200, rows: STEP_4 };
        await page.setRequestInterception(true);
        page.on("request", (request) => {
          if (new URL(request.url()).pathname !== "/api/audit-flags") return request.continue();
          return request.respond({
            status: answer.status,
            contentType: "application/json",
            body: JSON.stringify({ schema: "factory.audit-flags/1", rows: answer.rows }),
          });
        });
        const open = async (wait) => {
          if (page.url().endsWith("#/retros")) await page.reload({ waitUntil: "domcontentloaded" });
          else await page.goto(origin + "/#/retros", { waitUntil: "domcontentloaded" });
          await page.waitForFunction(
            (text) => document.getElementById("audit-flags-status").textContent.startsWith(text),
            {},
            wait,
          );
          return page.$eval("#audit-flags", (section) => ({
            next: section.nextElementSibling.id,
            afterNext: section.nextElementSibling.nextElementSibling.id,
            previous: section.previousElementSibling.querySelector("h2")?.id,
            heading: section.querySelector("h3").textContent,
            status: section.querySelector("#audit-flags-status").textContent,
            hidden: section.querySelector("#audit-flags-wrap").hidden,
            headers: [...section.querySelectorAll("th[scope=col]")].map((th) => th.textContent),
            rows: [...section.querySelectorAll("#audit-flags-rows tr")].map((tr) =>
              [...tr.cells].map((td) => td.textContent),
            ),
            links: [...section.querySelectorAll("#audit-flags-rows a")].map((a) =>
              a.getAttribute("href"),
            ),
            codes: section.querySelectorAll("#audit-flags-rows td:last-child > code").length,
            images: section.querySelectorAll("img").length,
          }));
        };

        let shown = await open("4 flagged");
        assert.equal(shown.previous, "retros-page-heading");
        assert.equal(shown.next, "retros-runs-heading");
        assert.equal(shown.afterNext, "retros-status");
        assert.equal(shown.heading, "Red flags");
        assert.equal(
          shown.status,
          "4 flagged calls across 1 job, newest first. Each host redacted its targets before upload.",
        );
        assert.equal(shown.hidden, false);
        assert.deepEqual(shown.headers, [
          "Time (UTC)",
          "Job",
          "Host",
          "Agent",
          "Tool",
          "Flags",
          "Target",
        ]);
        assert.deepEqual(shown.rows[0], [
          "2026-11-10 10:00:05",
          "1017-add-x",
          "example-host",
          "factory-builder",
          "Bash",
          "recursive-delete, denied",
          "rm -rf build",
        ]);
        assert.deepEqual(
          shown.rows.map((row) => row[6]),
          STEP_4.map((row) => row.target),
        );
        assert.deepEqual(shown.links, Array(4).fill("#1017-add-x"));
        assert.equal(shown.codes, 4);

        answer = { status: 200, rows: [{ ...STEP_4[0], target: "<img src=x>" }] };
        shown = await open("1 flagged");
        assert.equal(
          shown.status,
          "1 flagged call across 1 job, newest first. Each host redacted its targets before upload.",
        );
        assert.equal(shown.rows[0][6], "<img src=x>");
        assert.equal(shown.images, 0);

        const none =
          "No flagged tool calls yet. Each audit push adds its job's flagged calls here.";
        for (const status of [200, 404]) {
          answer = { status, rows: [] };
          shown = await open(none);
          assert.deepEqual(
            [shown.status, shown.hidden, shown.rows],
            [none, true, []],
            String(status),
          );
        }
        answer = { status: 500, rows: [] };
        shown = await open("Could not");
        assert.deepEqual(
          [shown.status, shown.hidden],
          ["Could not load red flags. Reload to try again.", true],
        );
        assert.deepEqual(errors, [], "no page errors");
      } finally {
        await context.close();
      }
    });
  },
);
