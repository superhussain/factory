// README screenshot capture: the three synthetic dashboard views embedded in the
// README, against one owned local service. Mirrors the proven pattern in
// tests/browser/run.mjs (same fixtures style, frozen clock, request guard) with an
// owned ephemeral loopback port, light theme, and no reference page. Never part of
// CI: run manually with `node tools/capture-screenshots.mjs` to rewrite
// docs/screenshots/{board,idea-entry,project-settings}.png.
import { mkdir, rm, stat } from "node:fs/promises";
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
];

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

    async function view(name, prepare) {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 800, deviceScaleFactor: 2 });
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
        if (target.origin !== origin) {
          external.push(request.url());
          return request.abort();
        }
        return request.continue();
      });
      await page.goto(origin + "/", { waitUntil: "networkidle0" });
      await page.waitForSelector(".card");
      await prepare(page);
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
    if (external.length)
      throw new Error("external requests during capture: " + external.join(", "));
  } finally {
    await browser.close();
    await service.close();
    await rm(persistDir, { recursive: true, force: true });
  }
}

await capture();
