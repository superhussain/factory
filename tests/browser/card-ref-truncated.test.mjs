// A card ref that starts past its row's ellipsis scrolls into view while its stand-in link has
// focus. Each stand-in names its card, and rider ids get stand-ins too. Desktop light, mobile dark.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const wait = { polling: 50 };
const NOW = "2026-01-01T12:00:00Z";
const LONG = "060-sample-weekly-campaign-report-export-by-region";
const JOBS = [
  {
    id: LONG,
    project: "sample",
    title: "Weekly campaign report by region",
    stage: "merged",
    round: 1,
    updated: NOW,
  },
  {
    id: "061-sample-chart",
    project: "sample",
    title: "Chart colours",
    stage: "merged",
    round: 1,
    updated: NOW,
  },
  {
    id: "062-sample-dashboard",
    project: "sample",
    title: "Dashboard chart for the weekly campaign report",
    stage: "build",
    branch: "factory/062-sample-dashboard",
    round: 1,
    refs: [
      { kind: "follow-up-of", id: LONG },
      { kind: "follow-up-of", id: "061-sample-chart" },
    ],
    updated: NOW,
  },
  {
    id: "063-sample-typo",
    project: "sample",
    title: "Fix the chart legend typo",
    stage: "spec",
    round: 0,
    refs: [{ kind: "batched-into", id: "062-sample-dashboard" }],
    updated: NOW,
  },
];
const CARD = '.card[data-id="062-sample-dashboard"]';
const WRAP = `.card-wrap:has(${CARD})`;
const ACTIONS = '.card-actions[data-actions="062-sample-dashboard"]';
const SPAN = `${CARD} .refs:not(.riders) .ref-link[data-ref="061-sample-chart"]`;
const RIDER_SPAN = `${CARD} .riders .ref-link[data-ref="063-sample-typo"]`;
const LABELS = [
  `062-sample-dashboard: follow-up of ${LONG}`,
  "062-sample-dashboard: follow-up of 061-sample-chart",
  "062-sample-dashboard: batches 063-sample-typo",
];

const settle = (page) =>
  page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
// Span and row edges in one evaluate, so a redraw cannot detach the nodes.
const edges = (page, selector) =>
  page.evaluate((s) => {
    const span = document.querySelector(s);
    const row = span.closest(".line");
    const a = span.getBoundingClientRect();
    const r = row.getBoundingClientRect();
    return {
      spanLeft: a.left,
      rowLeft: r.left,
      rowRight: r.right,
      scrollLeft: row.scrollLeft,
      focused: span.classList.contains("ref-focused"),
    };
  }, selector);
const tab = async (page, n, shift = false) => {
  if (shift) await page.keyboard.down("Shift");
  for (let i = 0; i < n; i++) await page.keyboard.press("Tab");
  if (shift) await page.keyboard.up("Shift");
  await settle(page);
};
const activeIs = (page, selector) =>
  page.evaluate((s) => document.activeElement.matches(s), selector);

test("card ref truncated", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const errors = [];
    for (const [width, height, scheme, suffix] of [
      [1440, 900, "light", "-desktop"],
      [390, 844, "dark", "-mobile"],
    ]) {
      const page = await browser.newPage();
      await page.setViewport({ width, height });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
      page.on("pageerror", (error) => errors.push(String(error)));
      await blockLiveEvents(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(CARD);
      await settle(page);

      await t.test(`rest ${scheme}`, async () => {
        const e = await edges(page, SPAN);
        assert.ok(e.spanLeft >= e.rowRight, `span left ${e.spanLeft} row right ${e.rowRight}`);
        assert.equal(e.scrollLeft, 0);
        assert.equal(e.focused, false);
      });

      await t.test(`capture ${scheme}`, async () => {
        await page.focus(ACTIONS);
        await tab(page, 2);
        const clip = await page.evaluate((s) => {
          const el = document.querySelector(s);
          el.scrollIntoView({ block: "center", inline: "center" });
          const { x, y, width, height } = el.getBoundingClientRect();
          return { x: x + scrollX, y: y + scrollY, width, height };
        }, CARD);
        await page.screenshot({ path: `${SHOTS}card-ref-truncated${suffix}.png`, clip });
        await tab(page, 2, true);
      });

      await t.test(`focus ${scheme}`, async () => {
        await page.focus(ACTIONS);
        await tab(page, 2);
        assert.equal(
          await activeIs(page, `${WRAP} a.ref-focus[data-ref="061-sample-chart"]`),
          true,
        );
        const e = await edges(page, SPAN);
        assert.equal(e.focused, true);
        assert.ok(e.scrollLeft > 0, `scrollLeft ${e.scrollLeft}`);
        assert.ok(e.spanLeft >= e.rowLeft, `span left ${e.spanLeft} row left ${e.rowLeft}`);
        assert.ok(e.spanLeft < e.rowRight, `span left ${e.spanLeft} row right ${e.rowRight}`);
        await tab(page, 2, true);
        assert.equal(await activeIs(page, ACTIONS), true);
        assert.equal(await page.$(".ref-link.ref-focused"), null);
        assert.equal((await edges(page, SPAN)).scrollLeft, 0);
      });

      await t.test(`labels ${scheme}`, async () => {
        assert.deepEqual(
          await page.$$eval(`${WRAP} a.ref-focus`, (els) =>
            els.map((a) => [
              a.textContent,
              a.className,
              getComputedStyle(a).position,
              getComputedStyle(a).clipPath,
            ]),
          ),
          LABELS.map((t) => [t, "ref-focus sr-only", "absolute", "inset(50%)"]),
        );
      });

      await t.test(`rider ${scheme}`, async () => {
        await page.focus(ACTIONS);
        await tab(page, 3);
        assert.equal(await activeIs(page, `${WRAP} a.ref-focus[data-ref="063-sample-typo"]`), true);
        assert.equal(
          await page.$eval(RIDER_SPAN, (el) => el.classList.contains("ref-focused")),
          true,
        );
        await page.keyboard.press("Enter");
        await page.waitForFunction(
          () => document.querySelector("#d-title")?.textContent === "Fix the chart legend typo",
          wait,
        );
        assert.equal(await page.evaluate(() => location.hash), "#063-sample-typo");
        await page.click("#d-close");
        await page.waitForSelector("#detail", { hidden: true });
      });

      await page.close();
    }
    assert.deepEqual(errors, []);
  });
});
