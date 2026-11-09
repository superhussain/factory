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
const FALLBACK = "Copy is unavailable. Copy the link from the address bar.";

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
        await copyLink(ok.page, viewport);
        // Text and visibility are read together, since the 2 s timer may clear them under load.
        await ok.page.waitForFunction(() => {
          const el = document.getElementById("d-copy-status");
          return el.textContent === "Copied" && el.getClientRects().length > 0;
        });
        assert.deepEqual(
          await ok.page.evaluate(() => window.__copied),
          [origin + "/#100-sample-idea"],
          label,
        );
        assert.deepEqual(
          await ok.page.evaluate(() => ({
            focus: document.activeElement.id,
            menu: !document.getElementById("d-actions-menu").hidden,
          })),
          { focus: "d-actions", menu: false },
          `${label} focus`,
        );
        await ok.page.waitForFunction(
          () => document.getElementById("d-copy-status").textContent === "",
          { timeout: 2500 },
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
          FALLBACK,
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
