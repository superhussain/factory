// Shared browser fixture: one owned local service, the explicitly provisioned pinned
// browser, and fixture seeding, for exactly one callback's lifetime.
import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { Browser, computeExecutablePath, detectBrowserPlatform } from "@puppeteer/browsers";
import { PUPPETEER_REVISIONS } from "puppeteer-core/internal/revisions.js";
import puppeteer from "puppeteer-core";
import { startLocal } from "../../tools/local.mjs";
import owned from "./owned-origin.cjs";

export const browserCache = new URL("../../.context/browser/", import.meta.url);

// The common target guard: owned loopback origins only, before any network or browser work.
export const assertBrowserTarget = owned.assertOwnedBrowserOrigin;

export function browserExecutable() {
  const executablePath = computeExecutablePath({
    browser: Browser.CHROME,
    platform: detectBrowserPlatform(),
    buildId: PUPPETEER_REVISIONS.chrome,
    cacheDir: browserCache.pathname,
  });
  if (!existsSync(executablePath)) throw new Error("Run node tests/browser/provision.mjs first.");
  return executablePath;
}

// Live board events never connect, so a suite that drives the poll timer keeps it.
export async function blockLiveEvents(page) {
  await page.evaluateOnNewDocument(() => {
    window.WebSocket = class extends EventTarget {
      close() {}
    };
    document.hasFocus = () => window.fixtureFocus !== false;
  });
}

export async function controlBoardPolling(page) {
  await blockLiveEvents(page);
  await page.evaluateOnNewDocument(() => {
    const interval = window.setInterval;
    window.setInterval = (fn, ms, ...args) => {
      if (fn.name === "pollTick" && ms === 30000) {
        window.fixtureBoardPoll = () => fn(...args);
        return 0;
      }
      return interval(fn, ms, ...args);
    };
  });
}

// Conditional write helper: creates when absent, replaces the observed ETag otherwise.
async function put(origin, path, body, headers = {}) {
  const current = await fetch(origin + path, { redirect: "manual" });
  if (!current.ok && current.status !== 404)
    throw new Error(`seed read ${path}: ${current.status}`);
  const condition = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  const response = await fetch(origin + path, {
    method: "PUT",
    headers: { ...condition, ...headers },
    body,
  });
  if (response.status !== 200)
    throw new Error(`seed write ${path}: ${response.status} ${await response.text()}`);
  return response.headers.get("ETag");
}

export async function seedProject(origin, slug, document) {
  return put(origin, `/api/projects/${slug}.md`, document, { "Content-Type": "text/markdown" });
}

export async function seedBoard(origin, jobs, updated = "2026-01-01T00:00:00Z") {
  // The Worker refuses one write that drops an id and adds a row, so drop first.
  const current = await fetch(origin + "/api/board", { redirect: "manual" });
  if (current.ok) {
    const ids = new Set(jobs.map((job) => job.id));
    const kept = (await current.json()).jobs.filter((job) => ids.has(job.id));
    await put(origin, "/api/board", JSON.stringify({ updated, jobs: kept }));
  }
  return put(origin, "/api/board", JSON.stringify({ updated, jobs }));
}

export async function seedPipeline(origin, doc) {
  return put(origin, "/api/pipeline/global", JSON.stringify(doc), {
    "Content-Type": "application/json",
  });
}

export async function seedDocument(origin, slug, name, body) {
  return put(origin, `/api/project-documents/${slug}/${name}`, body, {
    "Content-Type": "text/markdown",
  });
}

// Fulfills exactly GET /api/hosts with the list, or a 500 for null, and continues every other
// request. Cooperative priority 0, so a test's own handler at a higher priority wins.
export async function serveHostList(page, list) {
  await page.setRequestInterception(true);
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (request.method() === "GET" && url.pathname === "/api/hosts" && !url.search)
      return list === null
        ? request.respond({ status: 500, body: "fixture failure" }, 0)
        : request.respond(
            { status: 200, contentType: "application/json", body: JSON.stringify(list) },
            0,
          );
    return request.continue(request.continueRequestOverrides(), 0);
  });
}

// Starts a fresh local service on a documented owned ephemeral port (parallel-safe),
// launches the pinned browser, runs callback({ browser, origin, runDir, env }), and always
// closes both. Operator-facing fixed services use 8220/8221 via tools/local.mjs.
export async function withBrowserFixture(callback) {
  const executablePath = browserExecutable();
  const runDir = await mkdtemp(
    new URL(`../../.context/browser-${process.pid}-`, import.meta.url).pathname,
  );
  const service = await startLocal({ persistDir: runDir + "/r2" });
  const env = { [owned.OWNED_ORIGIN_ENV]: service.origin };
  assertBrowserTarget(service.origin, env);
  let browser;
  try {
    // Headless Chrome has no hover pointer. 2 is Blink HoverType hover; touch emulation still sets none.
    browser = await puppeteer.launch({
      executablePath,
      headless: true,
      args: ["--blink-settings=primaryHoverType=2,availableHoverTypes=2"],
    });
    return await callback({ browser, origin: service.origin, runDir, env });
  } finally {
    await browser?.close();
    await service.close();
    await rm(runDir, { recursive: true, force: true });
  }
}

// Fail-fast key-event precondition: a hidden or unfocusable target throws instead of misrouting keys.
export async function focusVisible(page, selector) {
  const problem = await page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (!el) return "missing";
    if (!el.checkVisibility({ checkVisibilityCSS: true })) return "not visible";
    el.focus();
    if (document.activeElement !== el) {
      const active = document.activeElement;
      return `did not take focus (active: ${active?.id || active?.tagName})`;
    }
    return null;
  }, selector);
  if (problem) throw new Error(`focus precondition failed for ${selector}: ${problem}`);
}

// Captures one element after two frames, clipped to its box.
export async function shootCard(page, selector, path) {
  await page.waitForSelector(selector);
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
  const clip = await page.evaluate((s) => {
    const { x, y, width, height } = document.querySelector(s).getBoundingClientRect();
    return { x: x + scrollX, y: y + scrollY, width, height };
  }, selector);
  await page.screenshot({ path, clip });
}
