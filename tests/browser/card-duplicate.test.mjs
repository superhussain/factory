// An idea card that triage flagged shows "possible duplicate of <id>", with a stand-in link when
// the id is on the board. A spec card never does. Desktop light, mobile dark.
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
const DUP = "012-sample-report-export";
const triage = (duplicate) => ({
  line: "- [ ] sample: Add CSV export to reports",
  decision: "approved",
  at: NOW,
  by: "triage",
  triage: {
    model: "@cf/cloudflare/clef-flash",
    confidence: 86,
    threshold: 80,
    reason: "Rated by clef-flash against the project rules.",
    ...(duplicate && { duplicate }),
  },
});
const JOBS = [
  {
    id: DUP,
    project: "sample",
    title: "Export reports as CSV",
    stage: "build",
    round: 1,
    updated: NOW,
  },
  {
    id: "015-add-csv-export",
    project: "sample",
    title: "Add CSV export to reports",
    stage: "idea",
    idea: "Add CSV export to reports",
    backlog: triage(DUP),
    updated: NOW,
  },
  {
    id: "016-add-pdf-export",
    project: "sample",
    title: "Add PDF export to reports",
    stage: "idea",
    idea: "Add PDF export to reports",
    backlog: triage(null),
    updated: NOW,
  },
  {
    id: "017-rename-filters",
    project: "sample",
    title: "Rename report filters",
    stage: "spec",
    round: 0,
    backlog: triage(DUP),
    updated: NOW,
  },
];
const card = (id) => `.card[data-id="${id}"]`;
const WRAP = `.card-wrap:has(${card("015-add-csv-export")})`;

const settle = (page) =>
  page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));

test("card duplicate", { timeout: 240000 }, async (t) => {
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
      await page.waitForSelector(card("015-add-csv-export"));
      await settle(page);

      await t.test(`capture ${scheme}`, async () => {
        const clip = await page.evaluate(() => {
          const el = document.querySelector("section.col.idea");
          el.scrollIntoView({ block: "start", inline: "center" });
          const { x, y, width, height } = el.getBoundingClientRect();
          return { x: x + scrollX, y: y + scrollY, width, height };
        });
        await page.screenshot({ path: `${SHOTS}card-duplicate${suffix}.png`, clip });
      });

      await t.test(`note ${scheme}`, async () => {
        const rows = (id) =>
          page.$$eval(`${card(id)} .duplicate`, (els) =>
            els.map((el) => [el.className, el.textContent]),
          );
        assert.deepEqual(await rows("015-add-csv-export"), [
          ["meta refs duplicate line", `possible duplicate of ${DUP}`],
        ]);
        assert.deepEqual(await rows("016-add-pdf-export"), []);
        assert.deepEqual(await rows("017-rename-filters"), []);
        assert.equal(
          await page
            .$$eval(`${card("015-add-csv-export")} .duplicate .ref-link`, (els) =>
              els.map((el) => el.dataset.ref),
            )
            .then((refs) => refs.join()),
          DUP,
        );
        assert.deepEqual(
          await page.$$eval(`${WRAP} a.ref-focus`, (els) => els.map((a) => a.textContent)),
          [`015-add-csv-export: possible duplicate of ${DUP}`],
        );
      });

      await t.test(`open ${scheme}`, async () => {
        await page.focus(`${WRAP} a.ref-focus`);
        await page.keyboard.press("Enter");
        await page.waitForFunction(
          () => document.querySelector("#d-title")?.textContent === "Export reports as CSV",
          wait,
        );
        assert.equal(await page.evaluate(() => location.hash), `#${DUP}`);
        await page.click("#d-close");
        await page.waitForSelector("#detail", { hidden: true });
      });

      await page.close();
    }
    assert.deepEqual(errors, []);
  });
});
