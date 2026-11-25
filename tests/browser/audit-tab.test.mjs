import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const call = (time, agent, tool, flags, target) => ({
  host: "example-host",
  job: "1017-add-x",
  session: "3f2a9c1d",
  agent,
  time: `2026-11-10 10:00:0${time}`,
  tool,
  flags,
  target,
});
const ROWS = [
  call(2, "main", "Bash", ["force-push"], "git push --force origin factory/1017-add-x"),
  call(1, "main", "Bash", [], "git status"),
  call(3, "main", "Read", ["secret-file"], "~/app/.env"),
  call(
    4,
    "main",
    "Bash",
    ["pipe-to-shell"],
    "curl -H 'Authorization: [redacted]' https://get.example.test/i.sh | sh",
  ),
  call(5, "factory-builder", "Bash", ["recursive-delete", "denied"], "rm -rf build"),
  call(6, "factory-builder", "Edit", [], "<img src=x>"),
];

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(
    origin,
    ["1017-add-x", "1018-no-audit"].map((id) => ({
      id,
      project: "sample",
      title: "Synthetic " + id,
      stage: "review",
      updated: "2026-01-01T00:00:00Z",
    })),
  );
  for (const id of ["1017-add-x", "1018-no-audit"]) {
    const response = await fetch(origin + `/api/jobs/sample/${id}/spec.md`, {
      method: "PUT",
      headers: { "If-None-Match": "*", "Content-Type": "text/markdown" },
      body: `# Spec ${id}\n`,
    });
    assert.equal(response.status, 200);
  }
}

async function openAudit(page, origin, width, height, rows = ROWS) {
  await page.setViewport({ width, height, deviceScaleFactor: 1 });
  await page.setRequestInterception(true);
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (path === "/api/audit/1017-add-x")
      return request.respond({
        status: 200,
        contentType: "application/x-ndjson",
        body: rows.map((r) => JSON.stringify(r) + "\n").join("") + "not json\n",
      });
    if (path === "/api/audit/1018-no-audit")
      return request.respond({ status: 200, contentType: "application/x-ndjson", body: "" });
    return request.continue();
  });
  await page.goto(origin + "/#1017-add-x/audit", { waitUntil: "domcontentloaded" });
  await page.waitForSelector("#d-doc table.audit");
}

test("the audit tab lists every pushed call as text", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const context = await browser.createBrowserContext();
    try {
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await openAudit(page, origin, 1440, 900);
      assert.equal(
        await page.$eval("#d-tabs .tab.active", (tab) => tab.textContent.trim()),
        "audit",
      );
      assert.equal(new URL(page.url()).hash, "#1017-add-x/audit");
      const table = await page.$eval("#d-doc", (doc) => ({
        summary: doc.querySelector("p").textContent,
        headers: [...doc.querySelectorAll("table.audit th[scope=col]")].map((th) => th.textContent),
        rows: [...doc.querySelectorAll("table.audit tbody tr")].map((tr) =>
          [...tr.cells].map((td) => td.textContent),
        ),
        strong: [...doc.querySelectorAll("table.audit strong")].map((s) => s.textContent),
        images: doc.querySelectorAll("img").length,
        display: getComputedStyle(doc.querySelector("table.audit")).display,
        role: doc.querySelector("table.audit").getAttribute("role"),
        labels: [...doc.querySelectorAll("table.audit tbody tr")].map((tr) =>
          [...tr.cells].map((td) => td.dataset.label),
        ),
      }));
      assert.equal(table.display, "table");
      assert.equal(table.role, "table");
      for (const labels of table.labels) assert.deepEqual(labels, table.headers);
      assert.equal(
        table.summary,
        "6 calls from 1 host, 4 flagged. Each host redacted its targets before upload.",
      );
      assert.deepEqual(table.headers, [
        "Time (UTC)",
        "Host",
        "Session",
        "Agent",
        "Tool",
        "Flags",
        "Target",
      ]);
      assert.equal(table.rows.length, 6);
      assert.deepEqual(table.rows[0], [
        "2026-11-10 10:00:01",
        "example-host",
        "3f2a9c1d",
        "main",
        "Bash",
        "-",
        "git status",
      ]);
      assert.deepEqual(table.rows[4].slice(5), ["recursive-delete, denied", "rm -rf build"]);
      assert.equal(table.rows[5][6], "<img src=x>");
      assert.deepEqual(table.strong, [
        "force-push",
        "secret-file",
        "pipe-to-shell",
        "recursive-delete, denied",
      ]);
      assert.equal(table.images, 0);
      await page.goto(origin + "/#1018-no-audit", { waitUntil: "domcontentloaded" });
      await page.waitForFunction(() =>
        document.getElementById("d-doc").textContent.includes("Spec 1018-no-audit"),
      );
      assert.equal(await page.$('#d-tabs .tab[data-name="audit.md"]'), null);
      assert.deepEqual(errors, [], "no page errors");
    } finally {
      await context.close();
    }
  });
});

test("the audit tab stacks each call as a card on a phone", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const context = await browser.createBrowserContext();
    try {
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await openAudit(page, origin, 390, 844);
      const layout = await page.$eval("#d-doc", (doc) => {
        const wrap = doc.querySelector(".table-wrap");
        const row = doc.querySelectorAll("table.audit tbody tr")[4];
        const cell = (label) => {
          const td = row.querySelector(`td[data-label="${label}"]`);
          td.scrollIntoView({ block: "center" });
          const box = td.getBoundingClientRect();
          return {
            text: td.textContent,
            inside:
              box.left >= 0 && box.right <= innerWidth && box.top >= 0 && box.bottom <= innerHeight,
          };
        };
        return {
          display: getComputedStyle(doc.querySelector("table.audit")).display,
          scrollWidth: wrap.scrollWidth,
          clientWidth: wrap.clientWidth,
          flags: cell("Flags"),
          target: cell("Target"),
        };
      });
      assert.equal(layout.display, "block");
      assert.ok(
        layout.scrollWidth <= layout.clientWidth,
        `no sideways scroll: ${layout.scrollWidth} > ${layout.clientWidth}`,
      );
      assert.deepEqual(layout.flags, { text: "recursive-delete, denied", inside: true });
      assert.deepEqual(layout.target, { text: "rm -rf build", inside: true });
      assert.deepEqual(errors, [], "no page errors");
    } finally {
      await context.close();
    }
  });
});

test("the audit tab counts a skip marker row as no call", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const context = await browser.createBrowserContext();
    try {
      const page = await context.newPage();
      const marker = {
        ...call(7, "-", "-", [], "audit push skipped: a flag pattern hit its backtrack limit"),
        session: "-",
        skipped: true,
      };
      // A flagged row that also carries skipped: true still counts as a flagged call.
      const flaggedSkip = {
        ...call(8, "main", "Bash", ["recursive-delete"], "rm -rf build"),
        skipped: true,
      };
      await openAudit(page, origin, 1440, 900, [ROWS[0], ROWS[1], marker, flaggedSkip]);
      const doc = await page.$eval("#d-doc", (d) => ({
        summary: d.querySelector("p").textContent,
        rows: d.querySelectorAll("table.audit tbody tr").length,
      }));
      assert.deepEqual(doc, {
        summary: "3 calls from 1 host, 2 flagged. Each host redacted its targets before upload.",
        rows: 4,
      });
    } finally {
      await context.close();
    }
  });
});
