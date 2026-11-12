// Chip cards with a PR link at 390 dark and 1280 light: every chip whole in its own row, clear of the PR link, the project on the id row, and captured.
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
const TIGHT = {
  id: "047-sample-tight",
  project: "sample",
  title: "Tight board sample",
  stage: "pr-open",
  round: 1,
  updated: AT,
  priority: { at: AT },
  babysit: { at: AT },
  pr: "https://github.com/example/sample/pull/47",
};
const wrapOf = (id) => `.card-wrap:has(.card[data-id="${id}"])`;

async function settle(page, id) {
  await page.waitForSelector(`.card[data-id="${id}"]`);
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  });
}

function measure(page, id) {
  return page.evaluate((sel) => {
    const wrap = document.querySelector(sel);
    const tags = wrap.querySelector(".card .tags");
    const idRow = wrap.querySelector(".card .id");
    const proj = idRow.querySelector(".proj");
    const rect = (el) => {
      const { left, right, top, bottom } = el.getBoundingClientRect();
      return { left, right, top, bottom };
    };
    const box = (el) => {
      const style = getComputedStyle(el);
      return {
        ...rect(el),
        paddingLeft: parseFloat(style.paddingLeft),
        paddingRight: parseFloat(style.paddingRight),
        title: el.title,
      };
    };
    return {
      chips: [...(tags?.querySelectorAll(".chip") ?? [])].map((c) => ({
        text: c.textContent,
        ...rect(c),
        clientWidth: c.clientWidth,
        scrollWidth: c.scrollWidth,
      })),
      row: tags ? box(tags) : null,
      idRow: box(idRow),
      idText: { width: idRow.querySelector(".id-text").getBoundingClientRect().width },
      proj: proj ? { text: proj.textContent, ...rect(proj) } : null,
      pr: rect(wrap.querySelector(".pr-link")),
      doc: {
        scrollWidth: document.documentElement.scrollWidth,
        clientWidth: document.documentElement.clientWidth,
      },
    };
  }, wrapOf(id));
}

async function checks(t, m, id, order) {
  await t.test(`${id} chip order`, () => {
    assert.deepEqual(
      m.chips.map((c) => c.text),
      order,
    );
  });
  await t.test(`${id} chips whole`, () => {
    const left = m.row.left + m.row.paddingLeft - 1;
    const right = m.row.right - m.row.paddingRight + 1;
    m.chips.forEach((c, i) => {
      assert.ok(c.left >= left && c.right <= right, `${c.text} inside the row`);
      assert.ok(c.scrollWidth <= c.clientWidth, `${c.text} not shrunk`);
      const next = m.chips[i + 1];
      if (next && next.top < c.bottom)
        assert.ok(c.right <= next.left, `${c.text} clear of ${next.text}`);
    });
  });
  await t.test(`${id} clear of PR link`, () => {
    for (const c of m.chips) {
      const apart =
        c.right <= m.pr.left ||
        c.left >= m.pr.right ||
        c.bottom <= m.pr.top ||
        c.top >= m.pr.bottom;
      assert.ok(apart, `${c.text} clear of the PR link`);
    }
  });
  await t.test(`${id} project on id row`, () => {
    assert.equal(m.proj?.text, "Synthetic sample");
    assert.ok(
      m.proj.right <= m.pr.left,
      `project ends at ${m.proj.right}, PR link at ${m.pr.left}`,
    );
  });
  await t.test(`${id} id kept`, () => {
    assert.equal(m.idRow.title, id);
    assert.ok(m.idText.width >= 20, `id text ${m.idText.width}px wide`);
  });
  await t.test(`${id} no overflow`, () => {
    assert.ok(m.doc.scrollWidth <= m.doc.clientWidth + 1);
  });
}

test("card chips", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB, TIGHT]);
    const errors = [];
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 390, height: 844, isMobile: true, hasTouch: true });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await settle(page, ID);
    await (await page.$(wrapOf(ID))).screenshot({ path: `${SHOTS}chips-card-390.png` });
    const narrow = await measure(page, ID);
    await context.close();

    const wide = await browser.createBrowserContext();
    const desk = await wide.newPage();
    await desk.setViewport({ width: 1280, height: 800, isMobile: false });
    await desk.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    desk.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(desk);
    await desk.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await settle(desk, TIGHT.id);
    await (await desk.$(wrapOf(TIGHT.id))).screenshot({ path: `${SHOTS}chips-tight-1280.png` });
    const tight = await measure(desk, TIGHT.id);
    await wide.close();

    await checks(t, narrow, ID, ["pr-open", "Paused", "Prioritized"]);
    await checks(t, tight, TIGHT.id, ["Prioritized", "Babysitting"]);
    await t.test("no page errors", () => {
      assert.deepEqual(errors, []);
    });
  });
});
