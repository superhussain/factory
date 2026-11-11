// The drawer's Copy link item: it shows last for every board job, writes the job's dashboard
// link to the clipboard, and reports the result, at mobile and desktop width.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const IDEA = {
  id: "100-sample-idea",
  project: "sample",
  title: "Synthetic sample idea",
  stage: "idea",
  idea: "Synthetic sample idea",
  updated: "2026-01-01T00:00:00Z",
  priority: { at: "2026-01-01T00:00:00Z" },
};
const CLOSED = {
  id: "101-sample-closed",
  project: "sample",
  title: "Synthetic closed job",
  stage: "closed",
  updated: "2026-01-01T00:00:00Z",
};
const ITEMS = [
  "d-edit-idea",
  "d-discard",
  "d-follow-tracer",
  "d-move-host",
  "d-prioritize",
  "d-copy-link",
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true },
  { width: 1280, height: 900 },
];
const LINK = (origin) => origin + "/#100-sample-idea";
const FALLBACK = (origin) => "Copy is unavailable. The link is " + LINK(origin);

async function openDrawer(browser, origin, viewport, id, reject) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await page.setViewport(viewport);
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.evaluateOnNewDocument((reject) => {
    window.__copied = [];
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: async (text) => {
          if (reject) throw new Error("synthetic denial");
          window.__copied.push(text);
        },
      },
    });
  }, reject);
  await page.goto(origin + "/#" + id, { waitUntil: "domcontentloaded" });
  await page.waitForFunction(
    (id) => document.getElementById("d-meta").textContent.includes(id),
    {},
    id,
  );
  return { context, page, errors };
}

const shownItems = (page) =>
  page.evaluate(
    (items) => items.filter((id) => document.getElementById(id).getClientRects().length > 0),
    ITEMS,
  );

async function copyLink(page, viewport) {
  const activate = (selector) => (viewport.hasTouch ? page.tap(selector) : page.click(selector));
  await activate("#d-actions");
  await page.waitForSelector("#d-actions-menu:not([hidden])");
  await activate("#d-copy-link");
}

// Logs every text the status line takes, so a repeat copy shows as a clear then a write.
const watchStatus = (page) =>
  page.evaluate(() => {
    window.__status = [];
    new MutationObserver(() =>
      window.__status.push(document.getElementById("d-copy-status").textContent),
    ).observe(document.getElementById("d-copy-status"), {
      childList: true,
      characterData: true,
      subtree: true,
    });
  });

const status = (page) => page.$eval("#d-copy-status", (el) => el.textContent);

test("Copy link copies the job's dashboard link", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [IDEA, CLOSED]);
    for (const viewport of VIEWPORTS) {
      const label = `${viewport.width}px`;

      const ok = await openDrawer(browser, origin, viewport, IDEA.id, false);
      try {
        await ok.page.click("#d-actions");
        assert.deepEqual(
          await shownItems(ok.page),
          ["d-edit-idea", "d-discard", "d-follow-tracer", "d-copy-link"],
          `${label} idea items`,
        );
        await ok.page.keyboard.press("Escape");
        await watchStatus(ok.page);
        await copyLink(ok.page, viewport);
        // Text and visibility are read together, since the 2 s timer may clear them under load.
        await ok.page.waitForFunction(() => {
          const el = document.getElementById("d-copy-status");
          return el.textContent === "Copied" && el.getClientRects().length > 0;
        });
        assert.deepEqual(await ok.page.evaluate(() => window.__copied), [LINK(origin)], label);
        assert.deepEqual(
          await ok.page.evaluate(() => ({
            focus: document.activeElement.id,
            menu: !document.getElementById("d-actions-menu").hidden,
          })),
          { focus: "d-actions", menu: false },
          `${label} focus`,
        );
        await copyLink(ok.page, viewport);
        await ok.page.waitForFunction(() => {
          const el = document.getElementById("d-copy-status");
          return window.__copied.length === 2 && el.textContent === "Copied";
        });
        assert.deepEqual(
          await ok.page.evaluate(() => window.__copied),
          [LINK(origin), LINK(origin)],
          `${label} repeat`,
        );
        assert.deepEqual(
          await ok.page.evaluate(() => window.__status),
          ["Copied", "", "Copied"],
          `${label} repeat log`,
        );
        await ok.page.waitForFunction(
          () => document.getElementById("d-copy-status").textContent === "",
          { timeout: 3000 },
        );
        assert.deepEqual(
          await ok.page.evaluate(() => window.__status),
          ["Copied", "", "Copied", ""],
          `${label} clear log`,
        );
        assert.equal(
          await ok.page.$eval("#d-copy-status", (el) => el.getClientRects().length),
          0,
          `${label} status hidden`,
        );
        assert.deepEqual(ok.errors, [], label);
      } finally {
        await ok.context.close();
      }

      const denied = await openDrawer(browser, origin, viewport, IDEA.id, true);
      try {
        await copyLink(denied.page, viewport);
        await denied.page.waitForFunction(
          (text) => document.getElementById("d-copy-status").textContent === text,
          {},
          FALLBACK(origin),
        );
        await denied.page.evaluate(() => {
          location.hash = "#101-sample-closed";
        });
        await denied.page.waitForFunction(() =>
          document.getElementById("d-meta").textContent.includes("101-sample-closed"),
        );
        assert.equal(await status(denied.page), "", `${label} job switch`);
        assert.equal(
          await denied.page.$eval("#d-copy-status", (el) => el.getClientRects().length),
          0,
          `${label} job switch hidden`,
        );
        assert.deepEqual(await denied.page.evaluate(() => window.__copied), [], label);
        assert.deepEqual(denied.errors, [], label);
      } finally {
        await denied.context.close();
      }

      const closed = await openDrawer(browser, origin, viewport, CLOSED.id, false);
      try {
        await closed.page.click("#d-actions");
        assert.deepEqual(await shownItems(closed.page), ["d-copy-link"], `${label} closed job`);
        assert.equal(await status(closed.page), "", label);
        assert.deepEqual(closed.errors, [], label);
      } finally {
        await closed.context.close();
      }
    }
  });
});
