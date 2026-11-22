// README screenshot capture: synthetic dashboard views for the README and docs/screenshots,
// against one owned local service. Mirrors the proven pattern in tests/browser/run.mjs (same
// fixtures style, frozen clock, request guard) with an owned ephemeral loopback port, light
// theme, and no reference page. Never part of CI: run manually with
// `node tools/capture-screenshots.mjs` to rewrite docs/screenshots/{board,idea-entry,
// project-settings,inbox,notifications,retros-red-flags,retros-spend,retros-spend-phone,
// settings-notifications,settings-display,settings-hosts,settings-pipeline,board-phone,
// idea-attachment}.png.
import { readFileSync } from "node:fs";
import { mkdir, rm, stat, writeFile } from "node:fs/promises";
import { prepareAssets } from "./assets.mjs";
import { startLocal } from "./local.mjs";
import { browserExecutable } from "../tests/support/browser-server.mjs";

const CLOCK = "2026-10-27T12:00:00Z";
const OUT = new URL("../docs/screenshots/", import.meta.url).pathname;

// Synthetic board only: placeholder example URLs, no real projects, tokens or user data.
const JOBS = [
  {
    id: "001-capture",
    project: "sample",
    title: "Synthetic captured job",
    stage: "pr-open",
    branch: "factory/001-capture",
    round: 1,
    updated: "2026-10-27T12:00:00Z",
    pr: "https://github.com/example/sample/pull/42",
    paseo: { server: "synthetic-server", agent: "synthetic-agent" },
    note: "Synthetic capture note\nwith a second line",
  },
  {
    id: "002-needs",
    project: "sample",
    title: "Synthetic blocked job",
    stage: "build",
    updated: "2026-10-27T11:00:00Z",
    needs: "Choose the synthetic option.",
  },
  {
    id: "003-idea",
    title: "Synthetic captured idea",
    stage: "idea",
    updated: "2026-10-27T10:00:00Z",
    idea: "Synthetic captured idea\nA small example for the board.",
  },
  {
    id: "004-plain",
    project: "beta",
    title: "Synthetic plain job",
    stage: "build",
    branch: "factory/004-plain",
    round: 1,
    updated: "2026-10-27T09:00:00Z",
  },
  {
    id: "042-sample-export",
    project: "sample",
    title: "Export the sample report",
    stage: "pr-open",
    branch: "factory/042-sample-export",
    round: 1,
    pr: "https://github.com/example/sample/pull/44",
    needs: "review and merge PR #44",
    escalated: true,
    updated: "2026-10-27T08:00:00Z",
  },
  {
    id: "046-sample-human",
    project: "sample",
    title: "Recover the sample host",
    stage: "needs-human",
    updated: "2026-10-27T07:00:00Z",
  },
  {
    id: "051-sample-retry",
    project: "sample",
    title: "Retry the sample upload",
    stage: "build",
    needs: "Pick a retry limit in #051-sample-retry/needs",
    updated: "2026-10-27T06:00:00Z",
  },
  {
    id: "060-sample-merged",
    project: "sample",
    title: "Synthetic merged job",
    stage: "merged",
    pr: "https://github.com/example/sample/pull/60",
    updated: "2026-10-26T12:00:00Z",
    done: "2026-10-26T12:00:00Z",
  },
];

// Copied from tests/browser/inbox-page.test.mjs.
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

// Copied from tests/browser/audit-flags.test.mjs.
const HOSTS = JSON.parse(
  readFileSync(new URL("../tests/fixtures/hosts-list.json", import.meta.url), "utf8"),
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
const RED_FLAGS = [
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

// GET answers for routes a fresh local Worker has no data for.
const STUBS = {
  "/api/hosts": HOSTS,
  "/api/audit-flags": { schema: "factory.audit-flags/1", rows: RED_FLAGS },
  "/api/spend": {
    schema: "factory.spend/1",
    weeks: 8,
    rows: [
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
    ],
  },
};

async function put(origin, route, body, type) {
  const current = await fetch(origin + route, { redirect: "manual" });
  if (!current.ok && current.status !== 404)
    throw new Error(`capture seed read ${route}: ${current.status}`);
  const headers = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  if (type) headers["Content-Type"] = type;
  const response = await fetch(origin + route, { method: "PUT", headers, body });
  if (response.status !== 200) throw new Error(`capture seed ${route}: ${response.status}`);
}

async function capture() {
  await prepareAssets();
  await mkdir(OUT, { recursive: true });
  const persistDir = new URL(`../.context/capture-${Date.now()}-${process.pid}/`, import.meta.url)
    .pathname;
  await mkdir(persistDir, { recursive: true });
  const service = await startLocal({ persistDir: persistDir + "r2" });
  const puppeteer = (await import("puppeteer-core")).default;
  const browser = await puppeteer.launch({ executablePath: browserExecutable(), headless: true });
  const origin = service.origin;
  const external = [];
  try {
    await put(
      origin,
      "/api/projects/sample.md",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      "text/markdown",
    );
    await put(
      origin,
      "/api/projects/beta.md",
      "---\nrepo: example/beta\nbase: main\ntitle: Synthetic beta\n---\n",
      "text/markdown",
    );
    await put(
      origin,
      "/api/board",
      JSON.stringify({ updated: CLOCK, jobs: JOBS }),
      "application/json",
    );
    await put(
      origin,
      "/api/project-documents/sample/notes.md",
      "Synthetic project notes.\n",
      "text/markdown",
    );
    await put(
      origin,
      "/api/project-documents/beta/notes.md",
      "Synthetic project notes.\n",
      "text/markdown",
    );
    await put(origin, "/api/backlog", BACKLOG, "text/markdown");
    await put(origin, "/api/jobs/sample/051-sample-retry/needs.md", NEEDS);

    async function view(name, prepare, viewport = { width: 1280, height: 800 }) {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ ...viewport, deviceScaleFactor: 2 });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      await page.evaluateOnNewDocument((clock) => {
        const NativeDate = Date;
        window.Date = class extends NativeDate {
          constructor(...args) {
            super(...(args.length ? args : [clock]));
          }
          static now() {
            return new NativeDate(clock).getTime();
          }
        };
        const interval = window.setInterval;
        window.setInterval = (fn, ms, ...args) => (ms === 3000 ? 0 : interval(fn, ms, ...args));
      }, CLOCK);
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        const target = new URL(request.url());
        const stub = STUBS[target.pathname];
        if (target.origin === origin && request.method() === "GET" && stub)
          return request.respond({
            status: 200,
            contentType: "application/json",
            body: JSON.stringify(stub),
          });
        if (target.origin !== origin) {
          external.push(request.url());
          return request.abort();
        }
        return request.continue();
      });
      await page.goto(origin + "/", { waitUntil: "networkidle0" });
      await page.waitForSelector(".card");
      await prepare(page);
      // Route changes focus the heading; drop that ring unless a dialog keeps the typed caret.
      await page.evaluate(() => {
        if (!document.querySelector("dialog[open]")) document.activeElement?.blur();
      });
      await page.evaluate(async () => {
        await document.fonts.ready;
        await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      });
      const file = `${OUT}${name}.png`;
      await page.screenshot({ path: file });
      const { size } = await stat(file);
      if (size >= 5 * 1024 * 1024) throw new Error(`${file} exceeds 5 MiB`);
      console.log(JSON.stringify({ file: `docs/screenshots/${name}.png`, bytes: size }));
      await context.close();
    }

    await view("board", async () => {});
    await view("idea-entry", async (page) => {
      await page.waitForSelector("#new-idea:not([hidden])");
      await page.click("#new-idea");
      await page.waitForSelector("#idea-dlg[open]");
    });
    await view("project-settings", async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/projects";
      });
      await page.waitForSelector("#settings-projects:not([hidden])");
      // The document list has loaded once the missing files carry their suffix.
      await page.waitForFunction(
        () =>
          (document.getElementById("settings-tab-decisions")?.textContent ?? "").endsWith(
            "(not created)",
          ),
        { timeout: 15000 },
      );
    });
    const go = (page, hash) =>
      page.evaluate((h) => {
        location.hash = h;
      }, hash);
    await view("inbox", async (page) => {
      await go(page, "#/inbox");
      await page.waitForFunction(() =>
        document.getElementById("inbox-list").textContent.includes("Trim the export columns"),
      );
    });
    await view("notifications", async (page) => {
      await go(page, "#/notifications");
      await page.waitForFunction(() =>
        document.getElementById("notifications-list").textContent.includes("Synthetic merged job"),
      );
    });
    await view("retros-red-flags", async (page) => {
      await go(page, "#/retros");
      await page.waitForSelector("#audit-flags-wrap:not([hidden])");
      await page.waitForFunction(
        () =>
          ![...document.querySelectorAll("#audit-flags-rows td:nth-child(3)")].some(
            (cell) => cell.textContent === "example-host",
          ),
      );
    });
    for (const [name, viewport] of [
      ["retros-spend", undefined],
      ["retros-spend-phone", { width: 390, height: 844 }],
    ])
      await view(
        name,
        async (page) => {
          await go(page, "#/retros");
          await page.waitForSelector("#spend-tables table");
          await page.evaluate(() =>
            document.getElementById("spend").scrollIntoView({ block: "start" }),
          );
        },
        viewport,
      );
    for (const section of ["notifications", "display"])
      await view(`settings-${section}`, async (page) => {
        await go(page, `#/settings/${section}`);
        await page.waitForSelector(`#settings-${section}:not([hidden])`);
      });
    await view("settings-hosts", async (page) => {
      await go(page, "#/settings/hosts");
      await page.waitForSelector('.host-card[data-id="lab-linux"]');
    });
    await view("settings-pipeline", async (page) => {
      await go(page, "#/settings/pipeline");
      await page.waitForFunction(
        () =>
          document.getElementById("pipeline-status").textContent ===
          "Not saved yet. Choose values, then Save to create version 1.",
      );
      await page.waitForSelector('#pipeline-pickup-hosts input[value="lab-linux"]');
    });
    await view("board-phone", async () => {}, { width: 390, height: 844 });
    const brief = persistDir + "brief.pdf";
    await writeFile(brief, "%PDF-1.4\n%%EOF\n");
    await view("idea-attachment", async (page) => {
      await page.waitForSelector("#new-idea:not([hidden])");
      await page.click("#new-idea");
      await page.waitForSelector("#idea-dlg[open]");
      await page.type("#idea-text", "Import the attached sales sheet.");
      await (await page.$("#idea-file")).uploadFile(brief);
      await page.waitForFunction(
        () => !draft.filePending && document.querySelectorAll("#idea-image-list li").length === 1,
      );
    });
    if (external.length)
      throw new Error("external requests during capture: " + external.join(", "));
  } finally {
    await browser.close();
    await service.close();
    await rm(persistDir, { recursive: true, force: true });
  }
}

await capture();
