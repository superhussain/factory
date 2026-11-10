// A needs-you card with a PR link and three id-row chips at 390 dark: every chip whole, clear of the PR link, and captured.
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
const AT = "2026-01-02T12:00:00Z";
const ID = "046-sample-chips";
const JOB = {
  id: ID,
  project: "sample",
  title: "Export chips sample",
  stage: "pr-open",
  needs: "Pick the export format",
  round: 1,
  updated: AT,
  paused: { at: AT },
  priority: { at: AT },
  pr: "https://github.com/example/sample/pull/46",
};

const measure = (page, id) =>
  page.evaluate((i) => {
    const wrap = document.querySelector(`.card[data-id="${i}"]`).closest(".card-wrap");
    const row = wrap.querySelector(".card .id");
    const rect = (el) => {
      const { left, right, top, bottom } = el.getBoundingClientRect();
      return { left, right, top, bottom };
    };
    const style = getComputedStyle(row);
    return {
      chips: [...row.querySelectorAll(".chip")].map((c) => ({
        text: c.textContent,
        ...rect(c),
        clientWidth: c.clientWidth,
        scrollWidth: c.scrollWidth,
      })),
      row: {
        ...rect(row),
        paddingLeft: parseFloat(style.paddingLeft),
        paddingRight: parseFloat(style.paddingRight),
        title: row.title,
      },
      idText: { width: row.querySelector(".id-text").getBoundingClientRect().width },
      pr: rect(wrap.querySelector(".pr-link")),
      doc: {
        scrollWidth: document.documentElement.scrollWidth,
        clientWidth: document.documentElement.clientWidth,
      },
    };
  }, id);

test("card chips", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB]);
    const errors = [];
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 390, height: 844, isMobile: true, hasTouch: true });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(`.card[data-id="${ID}"]`);
    await page.evaluate(async () => {
      await document.fonts.ready;
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    const m = await measure(page, ID);

    await t.test("chip order", () => {
      assert.deepEqual(
        m.chips.map((c) => c.text),
        ["pr-open", "Paused", "Prioritized"],
      );
    });
    await t.test("chips whole", () => {
      const left = m.row.left + m.row.paddingLeft - 1;
      const right = m.row.right - m.row.paddingRight + 1;
      m.chips.forEach((c, i) => {
        assert.ok(c.left >= left && c.right <= right, `${c.text} inside the row`);
        assert.ok(c.scrollWidth <= c.clientWidth, `${c.text} not shrunk`);
        const next = m.chips[i + 1];
        if (next) assert.ok(c.right <= next.left, `${c.text} clear of ${next.text}`);
      });
    });
    await t.test("clear of PR link", () => {
      for (const c of m.chips) {
        const apart =
          c.right <= m.pr.left ||
          c.left >= m.pr.right ||
          c.bottom <= m.pr.top ||
          c.top >= m.pr.bottom;
        assert.ok(apart, `${c.text} clear of the PR link`);
      }
    });
    await t.test("id kept", () => {
      assert.equal(m.row.title, ID);
      assert.ok(m.idText.width >= 20, `id text ${m.idText.width}px wide`);
    });
    await t.test("no overflow", () => {
      assert.ok(m.doc.scrollWidth <= m.doc.clientWidth + 1);
    });

    await (
      await page
        .$(`.card[data-id="${ID}"]`)
        .then((c) => c.evaluateHandle((e) => e.closest(".card-wrap")))
    ).screenshot({ path: `${SHOTS}chips-card-390.png` });
    await context.close();
    assert.deepEqual(errors, []);
  });
});
