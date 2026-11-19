// The Red flags section of the retros page. GET /api/audit-flags is answered by request interception.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedBoard, seedProject } from "../support/browser-server.mjs";
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
          // Labels fill in when /api/hosts lands and loadHosts redraws the card.
          await page.waitForSelector("#new-idea:not([hidden])");
          await page.waitForFunction(() => {
            const host = document.querySelector("#audit-flags-rows td:nth-child(3)");
            return !host || host.textContent !== "example-host";
          });
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
            codes: section.querySelectorAll("#audit-flags-rows td:nth-child(8) > code").length,
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
          "Actions",
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
          "AcknowledgeMake an idea",
        ]);
        const layout = await page.$$eval("#audit-flags-rows tr", (trs) =>
          trs.map((tr) => {
            const [a, b] = tr.querySelectorAll(".audit-flag-actions button");
            return {
              side: a.getBoundingClientRect().top === b.getBoundingClientRect().top,
              height: Math.round(tr.getBoundingClientRect().height),
            };
          }),
        );
        assert.ok(
          layout.every((row) => row.side),
          JSON.stringify(layout),
        );
        assert.ok(layout[2].height <= 48, JSON.stringify(layout));
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

const R = STEP_4[1];
const IDEA_TEXT = `Look into the pipe-to-shell red flag in 1017-add-x.

Flags: pipe-to-shell
Target: curl -H 'Authorization: [redacted]' https://get.example.test/i.sh | sh
Job: 1017-add-x
Host: Example Mac (example-host)
Agent: main, tool Bash, at 2026-11-10 10:00:04 UTC`;

test(
  "a red flag is acknowledged, hidden, shown again and turned into an idea",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(
        origin,
        "sample",
        "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      );
      await seedBoard(origin, [
        {
          id: "1017-add-x",
          project: "sample",
          title: "Add x",
          stage: "build",
          updated: "2026-01-01T00:00:00Z",
        },
      ]);
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
        const errors = [];
        page.on("pageerror", (error) => errors.push(String(error)));
        let rows = STEP_4.slice(1, 4);
        let ackAnswer = { status: 200 };
        const posts = [];
        await page.setRequestInterception(true);
        page.on("request", (request) => {
          const path = new URL(request.url()).pathname;
          if (path === "/api/hosts")
            return request.respond({
              status: 200,
              contentType: "application/json",
              body: JSON.stringify(HOSTS),
            });
          if (path === "/api/audit-flags/ack") {
            const { row, ack } = JSON.parse(request.postData());
            posts.push({ row, ack });
            if (ackAnswer.abort) return request.abort("internetdisconnected");
            if (ackAnswer.status !== 200)
              return request.respond({ status: ackAnswer.status, body: ackAnswer.body });
            rows = rows.map((r) =>
              r.target === row.target
                ? ack
                  ? { ...r, ack: "2026-11-19T10:00:00Z" }
                  : {
                      time: r.time,
                      job: r.job,
                      host: r.host,
                      agent: r.agent,
                      tool: r.tool,
                      flags: r.flags,
                      target: r.target,
                    }
                : r,
            );
            return request.respond({
              status: 200,
              contentType: "application/json",
              body: JSON.stringify({ schema: "factory.audit-flags/1", rows }),
            });
          }
          if (path !== "/api/audit-flags") return request.continue();
          return request.respond({
            status: 200,
            contentType: "application/json",
            body: JSON.stringify({ schema: "factory.audit-flags/1", rows }),
          });
        });
        await page.goto(origin + "/#/retros", { waitUntil: "domcontentloaded" });
        const status = () => page.$eval("#audit-flags-status", (p) => p.textContent);
        const waitStatus = (text) =>
          page.waitForFunction(
            (want) => document.getElementById("audit-flags-status").textContent.startsWith(want),
            {},
            text,
          );
        const trs = () =>
          page.$$eval("#audit-flags-rows tr", (list) => list.map((tr) => tr.className));
        await waitStatus("3 flagged");
        await page.waitForSelector("#audit-flags-rows .btn + .btn");
        assert.equal(
          await page.$eval("#audit-flags-show-acked", (c) => c.parentElement.parentElement.hidden),
          true,
        );

        await page.click(
          '#audit-flags-rows button[aria-label="Acknowledge the pipe-to-shell call in 1017-add-x"]',
        );
        await waitStatus("2 flagged calls");
        assert.equal(
          await status(),
          "2 flagged calls across 1 job, newest first. Each host redacted its targets before upload. 1 acknowledged call hidden.",
        );
        assert.deepEqual(posts, [{ row: { ...R }, ack: true }]);
        assert.equal(
          await page.evaluate(() => document.activeElement.id),
          "audit-flags-show-acked",
        );
        assert.equal((await trs()).length, 2);

        await page.click("#audit-flags-show-acked");
        await waitStatus("2 flagged calls");
        assert.match(await status(), /1 acknowledged call shown\.$/);
        assert.deepEqual(await trs(), ["acked", "", ""]);
        assert.equal(
          await page.$eval(
            "tr.acked .btn",
            (b) => `${b.textContent}|${b.getAttribute("aria-label")}`,
          ),
          "Unacknowledge|Unacknowledge the pipe-to-shell call in 1017-add-x",
        );
        await page.click("tr.acked .btn");
        await page.waitForFunction(() => !document.querySelector("tr.acked"));
        assert.equal(posts[1].ack, false);
        assert.equal(
          await status(),
          "3 flagged calls across 1 job, newest first. Each host redacted its targets before upload.",
        );

        assert.deepEqual(
          await page.evaluate(() => [
            document.getElementById("audit-flags-show-acked").checked,
            document.activeElement.id,
          ]),
          [false, "audit-flags-wrap"],
        );
        for (const left of [2, 1, 0]) {
          await page.click("#audit-flags-rows tr .btn");
          await page.waitForFunction(
            (n) => document.querySelectorAll("#audit-flags-rows tr").length === n,
            {},
            left,
          );
        }
        assert.equal(await status(), "No open flagged calls. 3 acknowledged calls hidden.");
        assert.equal(await page.$eval("#audit-flags-wrap", (w) => w.hidden), true);
        await page.click("#audit-flags-show-acked");
        await page.waitForFunction(() => document.querySelectorAll("tr.acked").length === 3);
        await page.click("tr.acked .btn");
        await waitStatus("1 flagged call ");
        await page.click("#audit-flags-show-acked");

        ackAnswer = { status: 409, body: "that red flag is no longer in the index; reload" };
        await page.click("#audit-flags-rows tr .btn");
        await page.waitForSelector("#audit-flags-err:not([hidden])");
        assert.equal(
          await page.$eval("#audit-flags-err", (p) => p.textContent),
          "Could not save: that red flag is no longer in the index; reload",
        );
        assert.equal((await trs()).length, 1);
        ackAnswer = { abort: true };
        await page.click("#audit-flags-rows tr .btn");
        await page.waitForFunction(
          () =>
            document.getElementById("audit-flags-err").textContent ===
            "Could not reach the factory. Check your connection, then try again.",
        );
        assert.equal(
          await page.evaluate(() => OFFLINE_MESSAGE),
          await page.$eval("#audit-flags-err", (p) => p.textContent),
        );
        assert.equal((await trs()).length, 1);

        // The idea button reads the job's project from the board, which can land after the flag rows.
        await page.waitForFunction(() => jobs.some((j) => j.id === "1017-add-x"));
        const idea = await page.$(
          '#audit-flags-rows button[aria-label="Make an idea from the pipe-to-shell call in 1017-add-x"]',
        );
        assert.ok(idea);
        await idea.click();
        await page.waitForSelector("#idea-dlg[open]");
        assert.deepEqual(
          await page.evaluate(() => [
            document.getElementById("idea-text").value,
            document.getElementById("idea-project").value,
          ]),
          [IDEA_TEXT, "sample"],
        );
        await page.evaluate(async () => {
          document.getElementById("idea-dlg").close();
          await new Promise((resolve) => setTimeout(resolve, 50));
          document.getElementById("idea-text").value = "an unsent idea";
        });
        await idea.click();
        await page.waitForSelector("#idea-dlg[open]");
        assert.equal(
          await page.$eval("#idea-text", (t) => t.value),
          "an unsent idea\n\n" + IDEA_TEXT,
        );
        assert.deepEqual(errors, [], "no page errors");
      } finally {
        await context.close();
      }
    });
  },
);

test("a red flag row links its backlog decision", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    const context = await browser.createBrowserContext();
    try {
      const page = await context.newPage();
      await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      const rows = STEP_4.slice(1, 3);
      rows[0] = { ...rows[0], backlog: "- [ ] factory: Red flag: pipe-to-shell via Bash" };
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        const path = new URL(request.url()).pathname;
        if (path !== "/api/audit-flags") return request.continue();
        return request.respond({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify({ schema: "factory.audit-flags/1", rows }),
        });
      });
      await page.goto(origin + "/#/retros", { waitUntil: "domcontentloaded" });
      await page.waitForFunction(() =>
        document.getElementById("audit-flags-status").textContent.startsWith("2 flagged"),
      );
      const links = await page.$$eval("#audit-flags-rows tr", (trs) =>
        trs.map((tr) =>
          [...tr.querySelectorAll(".audit-flag-actions a")].map((a) => [
            a.className,
            a.getAttribute("href"),
            a.textContent,
            a.title,
            a.getAttribute("aria-label"),
          ]),
        ),
      );
      assert.deepEqual(links, [
        [
          [
            "audit-flag-backlog",
            "#/backlog/7f76477c9762",
            "Backlog decision",
            "factory: Red flag: pipe-to-shell via Bash",
            "Open the backlog decision for the pipe-to-shell call in 1017-add-x",
          ],
        ],
        [],
      ]);
      assert.deepEqual(errors, [], "no page errors");
    } finally {
      await context.close();
    }
  });
});
