// Full hosted browser behavior run: the CJS suites in a bounded pool, each against its own
// fresh owned ephemeral local service, beside the node --test files, failing if
// a mandatory case marker is missing. Capture mode is private builder evidence only:
// `--capture <outdir> --reference-html <absolute path>` records matched before/after
// views against the read-only reference page and is never part of CI or assertions.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, stat } from "node:fs/promises";
import { prepareAssets } from "../../tools/assets.mjs";
import { startLocal } from "../../tools/local.mjs";
import { browserExecutable } from "../support/browser-server.mjs";
import owned from "../support/owned-origin.cjs";
import { clickNow } from "../support/board-ready.cjs";
import { selectShard } from "./shard.mjs";

const root = new URL("../", import.meta.url);
const CASES = selectShard(
  JSON.parse(await readFile(new URL("../fixtures/browser-cases.json", import.meta.url), "utf8")),
  process.env.FACTORY_BROWSER_SHARD,
);

function run(command, args, env = {}) {
  return new Promise((resolve) => {
    const child = spawn(command, args, { env: { ...process.env, ...env } });
    let output = "";
    child.stdout.on("data", (chunk) => {
      output += chunk;
    });
    child.stderr.on("data", (chunk) => {
      output += chunk;
    });
    child.once("exit", (code) => resolve({ code, output }));
  });
}

const BROWSER_CONCURRENCY = 2;

async function pool(items, limit, worker) {
  const results = [];
  let next = 0;
  async function lane() {
    while (next < items.length) {
      const index = next++;
      results[index] = await worker(items[index]);
    }
  }
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, lane));
  return results;
}

async function runSuite(suite, dir) {
  const started = Date.now();
  const service = await startLocal({ persistDir: `${dir}${suite.file}-r2` });
  try {
    const result = await run("node", [`tests/browser/${suite.file}`, "--browser", service.origin], {
      [owned.OWNED_ORIGIN_ENV]: service.origin,
    });
    return { ...result, ms: Date.now() - started };
  } finally {
    await service.close();
  }
}

async function behavior() {
  console.log(
    `browser shard ${process.env.FACTORY_BROWSER_SHARD || "all"}: ${CASES.suites.length} suites, ${CASES.nodeTests.length} node files`,
  );
  await prepareAssets();
  const dir = new URL(`../../.context/browser-run-${Date.now()}-${process.pid}/`, import.meta.url)
    .pathname;
  await mkdir(dir, { recursive: true });
  const failures = [];
  const [results, tests] = await Promise.all([
    pool(CASES.suites, BROWSER_CONCURRENCY, (suite) => runSuite(suite, dir)),
    run("node", [
      "--test",
      "--test-reporter=tap",
      `--test-concurrency=${BROWSER_CONCURRENCY}`,
      ...CASES.nodeTests,
    ]),
  ]);
  CASES.suites.forEach((suite, index) => {
    const result = results[index];
    console.log(result.output.trim());
    let suiteFailed = false;
    if (result.code !== 0) {
      suiteFailed = true;
      failures.push(`${suite.file}: exit ${result.code}`);
    }
    for (const marker of suite.cases) {
      if (!result.output.includes(marker)) {
        suiteFailed = true;
        failures.push(`${suite.file}: missing case ${JSON.stringify(marker)}`);
      }
    }
    console.log(
      `${suite.file}: ${suiteFailed ? "see failures" : "all cases present"} (${(result.ms / 1000).toFixed(1)}s)`,
    );
  });
  console.log(tests.output.trim());
  if (tests.code !== 0) failures.push(`node --test browser files: exit ${tests.code}`);
  for (const marker of ["# fail 0", "# skipped 0"]) {
    if (!tests.output.includes(marker))
      failures.push(`node --test browser files: missing ${JSON.stringify(marker)}`);
  }
  if (failures.length) {
    console.error("mandatory browser cases failed:\n" + failures.join("\n"));
    process.exit(1);
  }
  console.log("PASS full hosted browser behavior");
}

// Matched private captures: the same synthetic fixtures, frozen clock, viewports and
// themes against the read-only reference page and the replacement page.
const CAPTURE_JOBS = [
  {
    id: "001-capture",
    project: "sample",
    title: "Synthetic captured job",
    stage: "pr-open",
    branch: "factory/001-capture",
    round: 1,
    updated: "2026-01-01T12:00:00Z",
    pr: "https://github.com/example/sample/pull/42",
    paseo: { server: "synthetic-server", agent: "synthetic-agent" },
    note: "Synthetic capture note\nwith a second line",
  },
  {
    id: "002-needs",
    project: "sample",
    title: "Synthetic blocked job",
    stage: "build",
    updated: "2026-01-01T11:00:00Z",
    needs: "Choose the synthetic option.",
  },
  {
    id: "003-idea",
    title: "Synthetic captured idea",
    stage: "idea",
    updated: "2026-01-01T10:00:00Z",
    idea: "Synthetic captured idea\nA small example for the board.",
  },
  {
    id: "004-rework-capture",
    project: "sample",
    title: "Synthetic export idea",
    idea: "Synthetic export idea",
    stage: "closed",
    updated: "2026-01-01T09:00:00Z",
    done: "2026-01-01T09:00:00Z",
    note: "reworked into the backlog",
    backlog: {
      line: "- [ ] sample: Synthetic export idea",
      decision: "rework",
      at: "2026-01-01T09:00:00Z",
      notes: "Limit it to the weekly report and name the columns.",
    },
  },
];
const CAPTURE_BACKLOG_CONTEXT = [
  "Recommend: approve",
  "",
  "The operator exports the board by hand today. Job 204 left CSV export out of scope.",
  "",
  "## Question: Export scope",
  "Which jobs go in the file?",
  "Pick: one",
  "- Visible jobs (recommended): Only the cards the current filter shows.",
  "- All jobs: Every job on the board.",
  "",
  "## Question: Columns",
  "Which columns does the file hold?",
  "Pick: many",
  "- Title: The card title.",
  "- Stage: The stage name.",
]
  .map((line) => (line ? "  " + line : line))
  .join("\n");
const CAPTURE_BACKLOG =
  "# Backlog\n\n- [ ] sample: Synthetic backlog item with a long description that runs well past three lines on a narrow card, so the board cuts it off and the drawer shows every word of it, including this closing clause\n" +
  CAPTURE_BACKLOG_CONTEXT +
  "\n- [ ] sample: Synthetic follow-on slice (sub-job-of 001-capture)   \n" +
  "- [ ] sample: Synthetic weekly report export with date, campaign and spend columns (rework-of 004-rework-capture)\n";
const CAPTURE_LONG_TITLE =
  "Synthetic project with a very long title that runs well past the edge of a narrow phone screen";
const CAPTURE_TRIAGE_ITEM = "sample: Synthetic follow-on slice (sub-job-of 001-capture)";
const CAPTURE_TRIAGE = {
  item: CAPTURE_TRIAGE_ITEM,
  decision: "hold",
  confidence: 62,
  threshold: 80,
  reason: "Rated by clef-flash against the project rules.",
  model: "@cf/cloudflare/clef-flash",
  at: "2026-10-27T10:00:00Z",
};
const CAPTURE_SPEC =
  "# Synthetic spec\n\nCapture fixture content, not real job data.\n\n## Tasks\n\n1. **Parse the fixture**\n   - Files: `sample.md`\n   - Check: synthetic\n2. **Render the list**\n   - Files: `sample.html`\n3. **Ship it**\n   - Check: synthetic\n\n```mermaid\ngraph LR; Alpha-->Beta\n```\n";
const CAPTURE_BUILD =
  "# Synthetic build\n\n" +
  Array.from({ length: 40 }, (_, i) => `Synthetic build paragraph ${i + 1}.`).join("\n\n") +
  "\n";
const CAPTURE_ANSWERS =
  "# Answers\n\n## Question 1: Browser evidence\n\nPick: one\n\n- [ ] Ship on unit tests (recommended): open the PR now.\n- [x] Free a slot first: re-run the builder for screenshots.\n\nOther: none\n";
// The archived running example: answered by interception, never on the board.
const CAPTURE_ARCHIVED = {
  id: "412-fix-login",
  project: "sample",
  title: "Synthetic login fix",
  stage: "merged",
  round: 1,
  branch: "factory/412-fix-login",
  updated: "2026-09-01T12:00:00Z",
  done: "2026-09-01T12:00:00Z",
};
const CAPTURE_ARCHIVED_CLOSED = {
  id: "413-old-suggestion",
  project: "sample",
  title: "Synthetic old suggestion",
  stage: "closed",
  updated: "2026-08-20T12:00:00Z",
  done: "2026-08-20T12:00:00Z",
};
const CAPTURE_RECOVERY = {
  version: 1,
  slug: "sample",
  name: "notes.md",
  mode: "edit",
  baseText: "Original notes\n",
  baseEtag: '"original"',
  draftText: "Retained synthetic draft\n",
  blocked: "conflict",
};

async function put(origin, route, body, type) {
  const current = await fetch(origin + route, { redirect: "manual" });
  if (!current.ok && current.status !== 404)
    throw new Error(`capture seed read ${route}: ${current.status}`);
  const headers = {
    ...(current.ok ? { "If-Match": current.headers.get("ETag") } : { "If-None-Match": "*" }),
  };
  if (type) headers["Content-Type"] = type;
  const response = await fetch(origin + route, { method: "PUT", headers, body });
  if (response.status !== 200) throw new Error(`capture seed ${route}: ${response.status}`);
}

async function capture(outdir, referencePath) {
  if (
    !outdir ||
    !referencePath ||
    !referencePath.startsWith("/") ||
    !referencePath.endsWith(".html")
  ) {
    throw new Error("capture mode needs --capture <outdir> --reference-html <absolute .html path>");
  }
  const reference = await readFile(referencePath, "utf8");
  const candidate = await readFile(new URL("../../web/dashboard.html", import.meta.url), "utf8");
  const sha = (s) => createHash("sha256").update(s).digest("hex");
  await prepareAssets();
  const dir = new URL(`../../.context/capture-${Date.now()}-${process.pid}/`, import.meta.url)
    .pathname;
  await mkdir(dir, { recursive: true });
  const service = await startLocal({ persistDir: dir + "r2" });
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
      "/api/board",
      JSON.stringify({ updated: "2026-01-01T12:00:00Z", jobs: CAPTURE_JOBS }),
      "application/json",
    );
    await put(origin, "/api/backlog", CAPTURE_BACKLOG, "text/markdown");
    await put(origin, "/api/jobs/sample/001-capture/spec.md", CAPTURE_SPEC, "text/markdown");
    await put(origin, "/api/jobs/sample/001-capture/build.md", CAPTURE_BUILD, "text/markdown");
    await put(origin, "/api/jobs/sample/001-capture/answers.md", CAPTURE_ANSWERS, "text/markdown");
    await put(
      origin,
      "/api/jobs/sample/412-fix-login/spec.md",
      "# Synthetic login fix\n\nCapture fixture content, not real job data.\n",
      "text/markdown",
    );
    await put(
      origin,
      "/api/pipeline/global",
      JSON.stringify({ schema: 1, archive: { mode: "age", n: 30 } }),
      "application/json",
    );
    await put(
      origin,
      "/api/project-documents/sample/notes.md",
      "Original notes\n",
      "text/markdown",
    );

    const served = await fetch(origin + "/");
    const servedCsp = served.headers.get("Content-Security-Policy");
    const servedHtml = await served.text();
    const inner = (pattern) => servedHtml.match(pattern)[1];
    const servedTitle = inner(/<title>([^<]*)<\/title>/);
    const servedConfig = inner(/<script[^>]*id="factory-config"[^>]*>([^<]*)<\/script>/);
    const servedReference = inner(/<script[^>]*id="factory-reference"[^>]*>([^<]*)<\/script>/);
    const servedMetaCsp = inner(/http-equiv="Content-Security-Policy" content="([^"]*)"/);

    let archivedBoard = false;
    let longProject = false;
    async function view(name, width, height, theme, prepare, touch = false) {
      for (const side of ["before", "after"]) {
        const context = await browser.createBrowserContext();
        const page = await context.newPage();
        await page.setViewport({
          width,
          height,
          deviceScaleFactor: 1,
          hasTouch: touch,
          isMobile: touch,
        });
        await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
        await page.evaluateOnNewDocument((recovery) => {
          const NativeDate = Date;
          window.Date = class extends NativeDate {
            constructor(...args) {
              super(...(args.length ? args : ["2026-10-27T12:00:00Z"]));
            }
            static now() {
              return new NativeDate("2026-10-27T12:00:00Z").getTime();
            }
          };
          const interval = window.setInterval;
          window.setInterval = (fn, ms, ...args) => (ms === 3000 ? 0 : interval(fn, ms, ...args));
          localStorage.setItem("factory-project-document-draft:sample:notes.md", recovery);
        }, JSON.stringify(CAPTURE_RECOVERY));
        await page.setRequestInterception(true);
        page.on("request", async (request) => {
          const target = new URL(request.url());
          if (target.origin !== origin) {
            // The reference page requests its documented CDN; the replacement must not
            // attempt any external request. Both are blocked either way.
            if (side === "after") external.push(request.url());
            return request.abort();
          }
          if (target.pathname === "/api/backlog/triage") {
            const held = target.searchParams.get("item") === CAPTURE_TRIAGE_ITEM;
            return request.respond({
              status: held ? 200 : 404,
              contentType: "application/json",
              body: held ? JSON.stringify(CAPTURE_TRIAGE) : "",
            });
          }
          if (longProject && target.pathname === "/projects.json") {
            const projects = await fetch(target.href);
            return request.respond({
              status: projects.status,
              contentType: "application/json",
              body: JSON.stringify([
                ...(await projects.json()),
                { slug: "long", title: CAPTURE_LONG_TITLE, repo: "example/long", base: "main" },
              ]),
            });
          }
          if (archivedBoard && target.pathname === "/board.json") {
            const board = await fetch(target.href);
            return request.respond({
              status: board.status,
              contentType: "application/json",
              headers: { ETag: board.headers.get("ETag") ?? "" },
              body: JSON.stringify({ ...(await board.json()), archived: { max_id: 416 } }),
            });
          }
          if (target.pathname === "/api/board/archive") {
            if (target.searchParams.get("page") === "1")
              return request.respond({
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({
                  jobs: [CAPTURE_ARCHIVED, CAPTURE_ARCHIVED_CLOSED],
                  page: 1,
                  pages: 2,
                  total: 52,
                }),
              });
            const q = target.searchParams.get("q");
            if (q !== null) {
              const jobs = [CAPTURE_ARCHIVED].filter((j) =>
                [j.id, j.title].some((f) => f.toLowerCase().includes(q.toLowerCase())),
              );
              return request.respond({
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({ jobs, total: jobs.length }),
              });
            }
            if (target.searchParams.get("id") === "411-sync-retry")
              return request.respond({
                status: 503,
                contentType: "text/plain",
                body: "unavailable",
              });
            const found = target.searchParams.get("id") === CAPTURE_ARCHIVED.id;
            return request.respond({
              status: found ? 200 : 404,
              contentType: found ? "application/json" : "text/plain",
              body: found ? JSON.stringify({ jobs: [CAPTURE_ARCHIVED] }) : "not found",
            });
          }
          if (side === "before" && target.pathname === "/dashboard.html") {
            const body = reference
              .replaceAll("__FACTORY_TITLE__", () => servedTitle)
              .replaceAll("__FACTORY_CONFIG__", () => servedConfig)
              .replaceAll("__FACTORY_REFERENCE__", () => servedReference)
              .replaceAll("__FACTORY_CSP__", () => servedMetaCsp)
              .replaceAll("__FACTORY_IMG_HOST__", "images.example.test");
            return request.respond({
              status: 200,
              contentType: "text/html",
              headers: { "Content-Security-Policy": servedCsp },
              body,
            });
          }
          return request.continue();
        });
        await page.goto(origin + "/dashboard.html", { waitUntil: "networkidle0" });
        await page.waitForSelector(".card");
        await prepare(page, side);
        await page.evaluate(async () => {
          await document.fonts.ready;
          await new Promise((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(resolve)),
          );
        });
        const file = `${outdir.replace(/\/$/, "")}/${side}-${name}.png`;
        await page.screenshot({ path: file });
        const { size } = await stat(file);
        if (size >= 5 * 1024 * 1024) throw new Error(`${file} exceeds 5 MiB`);
        console.log(JSON.stringify({ file, side, view: name, width, height, theme, bytes: size }));
        await context.close();
      }
    }

    const openDrawer = async (page, side) => {
      await clickNow(page, '.card[data-id="001-capture"]');
      await page.waitForSelector('#d-tabs .tab[data-name="spec.md"]');
      // The replacement renders the bundled diagram; the blocked-CDN reference keeps the code block.
      await page.waitForFunction(
        (after) =>
          after
            ? document.querySelector("#d-doc .mermaid svg")
            : document.querySelector("#d-doc .mermaid svg") ||
              document.querySelector("#d-doc pre.mermaid-src"),
        { timeout: 30000 },
        side === "after",
      );
    };
    const openScrolled = async (page, side) => {
      await openDrawer(page, side);
      await page.click('.tab[data-name="build.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Synthetic build",
      );
      await page.evaluate(() => {
        document.getElementById("d-doc").scrollTop = 400;
      });
    };
    const openAnswers = async (page, side) => {
      await openDrawer(page, side);
      await page.click('.tab[data-name="answers.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Answers",
      );
      await page.waitForFunction(() => {
        const pill = document.getElementById("d-tab-pill");
        const tab = document.querySelector("#d-tabs .tab.active");
        if (!pill || !tab || pill.hidden) return true;
        const style = getComputedStyle(pill);
        return (
          parseFloat(style.left) === tab.offsetLeft && parseFloat(style.width) === tab.offsetWidth
        );
      });
      if (side === "after") {
        const weight = await page.evaluate(() => {
          const label = document.querySelector("#d-doc li.task input:checked")?.closest("label");
          return label ? getComputedStyle(label).fontWeight : null;
        });
        if (weight === null) throw new Error("no ticked task label rendered");
        if (weight !== "600") throw new Error("ticked task label is not bold");
      }
    };
    const openSettings = async (page) => {
      if (await page.$("#settings-link")) {
        await page.evaluate(() => {
          location.hash = "#/settings/projects";
        });
        await page.waitForSelector("#settings-projects:not([hidden])");
        await page.click("#settings-tab-notes");
      } else {
        await page.click("#project-settings");
        await page.select("#settings-resource", "notes.md");
      }
      await page.waitForFunction(() => !document.getElementById("settings-status").hidden);
    };
    const openSettingsField = async (page) => {
      await openSettings(page);
      await page.evaluate(() =>
        document
          .getElementById("settings-copy")
          .closest(".actions")
          .scrollIntoView({ block: "end" }),
      );
      const cropped = await page.evaluate(() =>
        [
          "settings-resource-help",
          "settings-document",
          "settings-status",
          "settings-copy",
          "settings-load",
        ].filter((id) => {
          const r = document.getElementById(id).getBoundingClientRect();
          return r.top < 0 || r.bottom > window.innerHeight;
        }),
      );
      if (cropped.length) throw new Error("settings capture crops " + cropped.join(", "));
    };
    const openJumper = async (page) => {
      await page.evaluate(() => {
        document.getElementById("job-jump").click();
      });
      await page.waitForSelector("#job-picker[open]");
    };
    const openIdea = async (page) => {
      await page.waitForSelector("#new-idea:not([hidden])");
      await page.click("#new-idea");
      await page.waitForSelector("#idea-dlg[open]");
    };

    await view("board-desktop", 1440, 900, "light", async () => {});
    await view("board-desktop-dark", 1440, 900, "dark", async () => {});
    await view("board-mobile", 390, 844, "dark", async () => {});
    await view("drawer-desktop", 1440, 900, "light", openDrawer);
    await view("drawer-mobile", 390, 844, "dark", openDrawer);
    await view("drawer-mobile-scrolled", 375, 667, "dark", openScrolled);
    await view("drawer-desktop-scrolled", 1440, 900, "light", openScrolled);
    await view("answers-desktop", 1440, 900, "light", openAnswers);
    await view("answers-mobile", 390, 844, "dark", openAnswers);
    await view("settings-recovery-desktop", 1440, 900, "light", openSettings);
    await view("settings-recovery-mobile", 390, 844, "dark", openSettingsField);
    await view("jumper-desktop", 1440, 900, "light", openJumper);
    await view("jumper-mobile", 390, 844, "dark", openJumper);
    await view("idea-desktop", 1440, 900, "light", openIdea);
    const openFilter = async (page) => {
      await page.click("#filter");
    };
    await view("filter-open-desktop", 1440, 900, "light", openFilter);
    await view("filter-open-mobile", 390, 844, "dark", openFilter);
    longProject = true;
    try {
      await view("filter-long-desktop", 1440, 900, "light", openFilter);
      await view("filter-long-mobile", 390, 844, "dark", openFilter);
    } finally {
      longProject = false;
    }
    const hideAllProjects = async (page) => {
      await page.click("#filter");
      await page.click('#filter-menu [data-project=""]');
      await page.keyboard.press("Escape");
    };
    await view("filter-none-desktop", 1440, 900, "light", hideAllProjects);
    await view("filter-none-mobile", 390, 844, "dark", hideAllProjects);
    // Both pages have the decision page, so both sides open it.
    const openDecision = async (page) => {
      await page.click(".backlog-card");
      await page.waitForSelector("#backlog-form");
    };
    const openQuestions = async (page, side) => {
      await openDecision(page);
      await page.evaluate(
        (selector) => {
          document.querySelector(selector).scrollIntoView();
        },
        side === "after" ? ".backlog-q" : "#backlog-submit",
      );
    };
    const openGate = async (page, side) => {
      await openDecision(page);
      await page.focus("input[value=approved]");
      if (side === "after") await page.keyboard.press("a");
    };
    const openAnswered = async (page) => {
      await openDecision(page);
      const center = (selector) =>
        page.evaluate(
          (s) => document.querySelector(s).scrollIntoView({ block: "center" }),
          selector,
        );
      await center("input[name=bq1][value='0']");
      await page.click("input[name=bq1][value='0']");
      await page.focus("input[name=bq1][value='1']");
      await page.keyboard.press("d");
      await page.evaluate(() =>
        document.querySelectorAll("fieldset.backlog-q")[1].scrollIntoView({ block: "center" }),
      );
    };
    const openRework = async (page, side) => {
      await page.click('.backlog-card[data-index="2"]');
      await page.waitForSelector("#backlog-form");
      if (side !== "after") return;
      await page.click("input[value=rework]");
      await page.type("#backlog-notes", "Split the export into its own settings page.");
    };
    const openTriage = async (page) => {
      await page.click('.backlog-card[data-index="1"]');
      await page.waitForSelector("#backlog-form");
      await page.waitForSelector("#backlog-triage:not([hidden])");
    };
    const openTriageFirstPaint = async (page, side) => {
      if (side === "after") await page.waitForFunction(() => backlogTriageSeen.size === 3);
      await page.click('.backlog-card[data-index="1"]');
      await page.waitForSelector("#backlog-form");
    };
    // The reference has no archive fieldset or archive lookup, so its side shows triage and the board.
    const openArchiveSettings = async (page, side) => {
      await page.evaluate(() => {
        location.hash = "#/settings/pipeline";
      });
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.waitForFunction(() => !document.getElementById("pipeline-triage-mode").disabled);
      await page.evaluate(
        (selector) => document.querySelector(selector).scrollIntoView({ block: "center" }),
        side === "after" ? "#pipeline-archive" : "#pipeline-triage",
      );
    };
    const openArchivedDrawer = async (page, side) => {
      await page.evaluate(() => {
        location.hash = "#412-fix-login";
      });
      if (side === "after")
        await page.waitForFunction(
          () => document.querySelector("#d-doc h1")?.textContent === "Synthetic login fix",
        );
      else await new Promise((resolve) => setTimeout(resolve, 1000));
    };
    // The reference has no archive list, so its side shows the closed column.
    // Terminal columns start collapsed, so both sides expand closed first.
    const openArchiveList = async (page, side) => {
      await page.click('.column-toggle[data-stage="closed"]');
      await page.waitForSelector("#column-cards-closed:not([hidden])");
      if (side === "after") {
        await page.click('.archive-more[data-stage="closed"]');
        await page.waitForSelector('.col.closed .archive-list .card[data-id="413-old-suggestion"]');
      }
      await page.evaluate(() =>
        document.querySelector(".col.closed").scrollIntoView({ block: "start", inline: "start" }),
      );
    };
    await view("archive-settings", 1280, 900, "light", openArchiveSettings);
    const openArchivePrefill = async (page) => {
      await page.evaluate(() => {
        location.hash = "#/settings/pipeline";
      });
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.select("#pipeline-scope", "global");
      await page.waitForFunction(
        () =>
          !document.getElementById("pipeline-archive").hidden &&
          !document.getElementById("pipeline-archive-mode").disabled,
      );
      await page.select("#pipeline-archive-mode", "");
      await page.select("#pipeline-archive-mode", "count");
      await page.evaluate(() =>
        document.getElementById("pipeline-archive").scrollIntoView({ block: "center" }),
      );
    };
    await view("archive-prefill", 1280, 900, "light", openArchivePrefill);
    await view("archived-drawer", 1280, 900, "light", openArchivedDrawer);
    const openArchivedLoadFailed = async (page, side) => {
      await page.evaluate(() => {
        location.hash = "#411-sync-retry";
      });
      if (side === "after")
        await page.waitForFunction(() =>
          document
            .getElementById("board-status")
            .textContent.startsWith("Could not load archived job"),
        );
      else await new Promise((resolve) => setTimeout(resolve, 1000));
    };
    await view("archived-load-failed", 1280, 900, "light", openArchivedLoadFailed);
    await view("archive-list", 1280, 900, "light", openArchiveList);
    await view("archive-list-mobile", 390, 844, "dark", openArchiveList);
    const openArchiveJumper = async (page, side) => {
      archivedBoard = true;
      try {
        await page.evaluate(() => poll());
        await openJumper(page);
        await page.type("#job-search", "login");
        if (side !== "after") return;
        await page.evaluate(() =>
          [...document.querySelectorAll('#job-results [role="option"]')]
            .find((o) => o.querySelector(".title").textContent === "Search archived jobs")
            .click(),
        );
        await page.waitForFunction(() =>
          [...document.querySelectorAll("#job-results .title")].some(
            (el) => el.textContent === "Synthetic login fix",
          ),
        );
      } finally {
        archivedBoard = false;
      }
    };
    await view("jumper-archive-search", 1280, 900, "light", openArchiveJumper);
    const openArchiveEmpty = async (page) => {
      archivedBoard = true;
      try {
        await page.evaluate(() => poll());
        await openJumper(page);
        await page.type("#job-search", "zzz");
        await page.evaluate(() =>
          [...document.querySelectorAll('#job-results [role="option"]')]
            .find((o) => o.querySelector(".title").textContent === "Search archived jobs")
            .click(),
        );
        await page.waitForFunction(() =>
          [...document.querySelectorAll("#job-results .meta")].some(
            (el) => el.textContent === 'No archived jobs match "zzz". Pick to search again.',
          ),
        );
      } finally {
        archivedBoard = false;
      }
    };
    await view("jumper-archive-empty", 1280, 900, "light", openArchiveEmpty);
    await view("backlog-triage-desktop", 1440, 900, "light", openTriage);
    await view("backlog-triage-mobile", 390, 844, "dark", openTriage);
    await view("backlog-triage-open-desktop", 1440, 900, "light", openTriageFirstPaint);
    await view("backlog-triage-open-mobile", 390, 844, "dark", openTriageFirstPaint);
    await view("backlog-decide-desktop", 1440, 900, "light", openDecision);
    await view("backlog-decide-mobile", 390, 844, "dark", openDecision);
    await view("backlog-decide-touch", 390, 844, "dark", openDecision, true);
    await view("backlog-questions-mobile", 390, 844, "dark", openQuestions);
    await view("backlog-gate-mobile", 390, 844, "dark", openGate);
    await view("backlog-answered-mobile", 390, 844, "dark", openAnswered);
    await view("backlog-rework-desktop", 1440, 900, "light", openRework);
    await view("backlog-rework-mobile", 390, 844, "dark", openRework);
    if (external.length)
      throw new Error("external requests during capture: " + external.join(", "));
    console.log(
      JSON.stringify({
        result: "PASS",
        provenance: {
          reference: { path: referencePath, sha256: sha(reference) },
          candidate: { path: "web/dashboard.html", sha256: sha(candidate) },
          board: { jobs: CAPTURE_JOBS.length, sha256: sha(JSON.stringify(CAPTURE_JOBS)) },
          origin,
          viewport: ["1440x900", "1280x900", "390x844", "375x667"],
          themes: ["light", "dark"],
        },
      }),
    );
  } finally {
    await browser.close();
    await service.close();
  }
}

const args = process.argv.slice(2);
if (args.length === 0) {
  await behavior();
} else if (args.length === 4 && args[0] === "--capture" && args[2] === "--reference-html") {
  await capture(args[1], args[3]);
} else {
  console.error(
    "usage: node tests/browser/run.mjs [--capture <outdir> --reference-html <absolute-path>]",
  );
  process.exit(2);
}
