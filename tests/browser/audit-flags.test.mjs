// The Red flags section of the retros page. GET /api/audit-flags is answered by request interception.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture } from "../support/browser-server.mjs";
import { readFileSync } from "node:fs";

const HOSTS = JSON.parse(
  readFileSync(new URL("../fixtures/hosts-list.json", import.meta.url), "utf8"),
);
HOSTS.push({ ...HOSTS[0], id: "example-host", label: "Example Mac" });

const flagged = (second, agent, tool, flags, target, extra = {}) => ({
  time: `2026-11-10 10:00:0${second}`,
  job: "1017-add-x",
  host: "example-host",
  agent,
  tool,
  flags,
  target,
  ...extra,
});
const STEP_4 = [
  flagged(5, "factory-builder", "Bash", ["recursive-delete", "denied"], "rm -rf build", {
    paseo: { server: "srv-a", agent: "agent-build" },
  }),
  flagged(
    4,
    "main",
    "Bash",
    ["pipe-to-shell"],
    "curl -H 'Authorization: [redacted]' https://get.example.test/i.sh | sh",
  ),
  flagged(3, "main", "Read", ["secret-file"], "~/app/.env"),
  flagged(2, "main", "Bash", ["force-push"], "git push --force origin factory/1017-add-x"),
  flagged(1, "main", "Bash", ["sudo"], "sudo x", { host: "other-host" }),
];
// 23 rows, one per minute, newest first.
const MANY = Array.from({ length: 23 }, (_, i) =>
  flagged(0, "main", "Bash", ["sudo"], `sudo ${i + 1}`, {
    time: `2026-11-10 10:${String(59 - i).padStart(2, "0")}:00`,
  }),
);

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
          const path = new URL(request.url()).pathname;
          if (path === "/api/hosts")
            return request.respond({
              status: 200,
              contentType: "application/json",
              body: JSON.stringify(HOSTS),
            });
          if (path !== "/api/audit-flags") return request.continue();
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
          return read();
        };
        const read = () =>
          page.$eval("#audit-flags", (section) => ({
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
            links: [...section.querySelectorAll("#audit-flags-rows td:nth-child(2) a")].map((a) =>
              a.getAttribute("href"),
            ),
            titles: [...section.querySelectorAll("#audit-flags-rows td:nth-child(3)")].map(
              (td) => td.title,
            ),
            sessions: [...section.querySelectorAll("#audit-flags-rows td:nth-child(5)")].map((td) =>
              [...td.querySelectorAll("a")].map((a) => [
                a.getAttribute("href"),
                a.getAttribute("aria-label"),
                a.target,
                a.rel,
              ]),
            ),
            pager: {
              hidden: section.querySelector("#audit-flags-pager").hidden,
              text: section.querySelector("#audit-flags-page").textContent,
              prev: section.querySelector("#audit-flags-prev").disabled,
              next: section.querySelector("#audit-flags-next").disabled,
            },
            codes: section.querySelectorAll("#audit-flags-rows td:last-child > code").length,
            images: section.querySelectorAll("img").length,
          }));

        let shown = await open("5 flagged");
        assert.equal(shown.previous, "retros-page-heading");
        assert.equal(shown.next, "retros-runs-heading");
        assert.equal(shown.afterNext, "retros-status");
        assert.equal(shown.heading, "Red flags");
        assert.equal(
          shown.status,
          "5 flagged calls across 1 job, newest first. Each host redacted its targets before upload.",
        );
        assert.equal(shown.hidden, false);
        assert.deepEqual(shown.headers, [
          "Time (UTC)",
          "Job",
          "Host",
          "Agent",
          "Session",
          "Tool",
          "Flags",
          "Target",
        ]);
        assert.deepEqual(shown.rows[0], [
          "2026-11-10 10:00:05",
          "1017-add-x",
          "Example Mac",
          "factory-builder",
          "Session appSession",
          "Bash",
          "recursive-delete, denied",
          "rm -rf build",
        ]);
        assert.deepEqual(
          shown.rows.map((row) => row[7]),
          STEP_4.map((row) => row.target),
        );
        assert.deepEqual(shown.links, Array(5).fill("#1017-add-x"));
        assert.equal(shown.codes, 5);
        assert.deepEqual(shown.titles, [
          "example-host",
          "example-host",
          "example-host",
          "example-host",
          "",
        ]);
        assert.equal(shown.rows[4][2], "other-host");
        assert.deepEqual(shown.sessions[0], [
          [
            "session://h/srv-a/agent/agent-build",
            "Open the factory-builder session in Session app",
            "",
            "",
          ],
          [
            "https://sessions.example.test/srv-a/agent-build",
            "Open the factory-builder session in Session",
            "_blank",
            "noopener noreferrer",
          ],
        ]);
        assert.deepEqual(shown.sessions.slice(1), Array(4).fill([]));
        assert.equal(shown.pager.hidden, true);

        answer = { status: 200, rows: MANY };
        shown = await open("23 flagged");
        assert.equal(shown.rows.length, 10);
        assert.deepEqual(shown.pager, {
          hidden: false,
          text: "Rows 1 to 10 of 23",
          prev: true,
          next: false,
        });
        for (const [text, count, prev, next] of [
          ["Rows 11 to 20 of 23", 10, false, false],
          ["Rows 21 to 23 of 23", 3, false, true],
        ]) {
          await page.click("#audit-flags-next");
          await page.waitForFunction(
            (want) => document.getElementById("audit-flags-page").textContent === want,
            {},
            text,
          );
          shown = await read();
          assert.deepEqual(
            [shown.rows.length, shown.pager.prev, shown.pager.next],
            [count, prev, next],
          );
        }
        assert.equal(await page.evaluate(() => document.activeElement.id), "audit-flags-prev");

        answer = { status: 200, rows: [{ ...STEP_4[0], target: "<img src=x>" }] };
        shown = await open("1 flagged");
        assert.equal(
          shown.status,
          "1 flagged call across 1 job, newest first. Each host redacted its targets before upload.",
        );
        assert.equal(shown.rows[0][7], "<img src=x>");
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
