// One line per fact on a job card: order, one-row truncation (a needs-you ask wraps to two rows)
// with a title tooltip, colour per kind, host label from the registry, ref links and their hover,
// at desktop light and mobile dark.
import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  serveHostList,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const FIXTURE_HOSTS = JSON.parse(
  await readFile(new URL("../fixtures/hosts-list.json", import.meta.url), "utf8"),
);
const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const wait = { polling: 50 };
const NOW = "2026-01-01T12:00:00Z";
const NOTE = "resumed by owner; recovery session continuing from spec";
const NEEDS =
  "Pick the CSV column order for the weekly export: date first, or campaign first as the old report did. Reply in needs.md with one of the two orders.";
const JOBS = [
  {
    id: "041-sample-report",
    project: "sample",
    title: "Sample report",
    stage: "merged",
    branch: "factory/041-sample-report",
    round: 1,
    updated: NOW,
  },
  {
    id: "042-sample-export",
    project: "sample",
    title: "Export the sample report as CSV for every project",
    stage: "build",
    branch: "factory/042-sample-export",
    round: 1,
    build: { state: "running" },
    refs: [{ kind: "follow-up-of", id: "041-sample-report" }],
    claim: { device: "lab-linux", session: "a1b2c3", since: NOW },
    tracer: { building: true },
    note: NOTE,
    updated: NOW,
  },
  {
    id: "043-orphan",
    project: "sample",
    title: "Orphan ref",
    stage: "spec",
    branch: "factory/043-orphan",
    round: 0,
    refs: [{ kind: "follow-up-of", id: "001-gone" }],
    updated: NOW,
  },
  {
    id: "044-ghost",
    project: "sample",
    title: "Unregistered host",
    stage: "build",
    branch: "factory/044-ghost",
    round: 0,
    claim: { device: "ghost-box", session: "d4e5f6", since: NOW },
    updated: NOW,
  },
  {
    id: "045-sample-needs",
    project: "sample",
    title: "Weekly export column order",
    stage: "build",
    round: 1,
    needs: NEEDS,
    updated: NOW,
  },
];
const LINES_042 = [
  ["id line", "042-sample-export"],
  ["proj line", "Synthetic sample"],
  ["title", "Export the sample report as CSV for every project"],
  ["meta branch line", "factory/042-sample-export"],
  ["meta round line", "round 1 · build running"],
  ["meta refs line", "follow-up of 041-sample-report"],
  ["meta host line", "on Lab Linux"],
  ["meta tracer-building line", "Building in Tracer"],
  ["note line", NOTE],
];
const TOKENS = {
  light: {
    "--info": "rgb(29, 78, 216)",
    "--host-fg": "rgb(15, 118, 110)",
    "--ref-fg": "rgb(190, 24, 93)",
  },
  dark: {
    "--info": "rgb(147, 197, 253)",
    "--host-fg": "rgb(94, 234, 212)",
    "--ref-fg": "rgb(249, 168, 212)",
  },
};
const CARD = '.card[data-id="042-sample-export"]';

// Each read queries and measures in one evaluate, so a board redraw cannot detach the nodes.
const cardLines = (page, id) =>
  page.evaluate(
    (i) =>
      [...document.querySelectorAll(`.card[data-id="${i}"] > div`)].map((el) => [
        el.className,
        el.textContent,
      ]),
    id,
  );
const tokenColor = (page, name) =>
  page.evaluate((n) => {
    const probe = document.createElement("span");
    probe.style.color = `var(${n})`;
    document.body.append(probe);
    const color = getComputedStyle(probe).color;
    probe.remove();
    return color;
  }, name);
const color = (page, selector) =>
  page.evaluate((s) => getComputedStyle(document.querySelector(s)).color, selector);
const hostLabelShown = (page) =>
  page.waitForFunction(
    (s) => document.querySelector(s + " .host")?.textContent === "on Lab Linux",
    wait,
    CARD,
  );
const settle = (page) =>
  page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));

test("card lines", { timeout: 240000 }, async (t) => {
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
      await serveHostList(page, FIXTURE_HOSTS);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await hostLabelShown(page);
      await settle(page);
      const clip = await page.evaluate((s) => {
        const { x, y, width, height } = document.querySelector(s).getBoundingClientRect();
        return { x, y, width, height };
      }, CARD);
      await page.screenshot({ path: `${SHOTS}card-lines${suffix}.png`, clip });
      await page.evaluate(() =>
        document
          .querySelector('.card[data-id="045-sample-needs"]')
          .scrollIntoView({ block: "center", inline: "center" }),
      );
      const needsClip = await page.evaluate(() => {
        const { x, y, width, height } = document
          .querySelector('.card[data-id="045-sample-needs"]')
          .getBoundingClientRect();
        return { x: x + scrollX, y: y + scrollY, width, height };
      });
      await page.screenshot({ path: `${SHOTS}card-needs${suffix}.png`, clip: needsClip });

      await t.test(`lines ${scheme}`, async () => {
        assert.deepEqual(await cardLines(page, "042-sample-export"), LINES_042);
      });

      await t.test(`truncation ${scheme}`, async () => {
        const lines = await page.evaluate(() =>
          [...document.querySelectorAll(".card .line")].map((el) => {
            const s = getComputedStyle(el);
            return {
              id: el.closest(".card").dataset.id,
              cls: el.className,
              text: el.textContent,
              title: el.title,
              whiteSpace: s.whiteSpace,
              textOverflow: s.textOverflow,
              overflow: s.overflowX,
              needsYou: el.closest(".card").classList.contains("needs-you"),
              clamp: s.webkitLineClamp,
              rows: Math.round(el.getBoundingClientRect().height / parseFloat(s.lineHeight)),
            };
          }),
        );
        assert.equal(lines.length, 26);
        for (const l of lines) {
          const where = `${l.id} ${l.cls}`;
          if (l.needsYou && l.cls === "note line") {
            assert.equal(l.whiteSpace, "normal", where);
            assert.equal(l.clamp, "2", where);
            assert.equal(l.overflow, "hidden", where);
            assert.equal(l.title, l.text, where);
            assert.equal(l.rows, 2, where);
            continue;
          }
          assert.equal(l.whiteSpace, "nowrap", where);
          assert.equal(l.textOverflow, "ellipsis", where);
          assert.equal(l.overflow, "hidden", where);
          assert.equal(l.title, l.cls === "id line" ? l.id : l.text, where);
        }
        assert.equal(
          await page.evaluate(
            (s) => getComputedStyle(document.querySelector(s)).webkitLineClamp,
            `${CARD} .title`,
          ),
          "2",
        );
      });

      await t.test(`colours ${scheme}`, async () => {
        for (const [name, value] of Object.entries(TOKENS[scheme])) {
          assert.equal(await tokenColor(page, name), value, name);
        }
        for (const theme of ["light", "dark"]) {
          await page.evaluate((v) => (document.documentElement.dataset.theme = v), theme);
          for (const [name, value] of Object.entries(TOKENS[theme])) {
            assert.equal(await tokenColor(page, name), value, `theme ${theme} ${name}`);
          }
        }
        await page.evaluate(() => delete document.documentElement.dataset.theme);
        assert.equal(await color(page, `${CARD} .branch`), await tokenColor(page, "--info"));
        assert.equal(await color(page, `${CARD} .refs`), await tokenColor(page, "--ref-fg"));
        assert.equal(await color(page, `${CARD} .host`), await tokenColor(page, "--host-fg"));
      });

      await t.test(`host label ${scheme}`, async () => {
        assert.equal(await page.$eval(`${CARD} .host`, (el) => el.textContent), "on Lab Linux");
        assert.equal(
          await page.$eval('.card[data-id="044-ghost"] .host', (el) => el.textContent),
          "on ghost-box",
        );
        await page.evaluate(() => (location.hash = "#042-sample-export"));
        await page.waitForFunction(
          () => document.getElementById("d-meta")?.textContent.includes("042-sample-export"),
          wait,
        );
        assert.ok((await page.$eval("#d-meta", (el) => el.textContent)).endsWith("· on Lab Linux"));
        await page.click("#d-close");
        await page.waitForFunction(() => document.getElementById("detail").hidden, wait);
      });

      await t.test(`ref hover ${scheme}`, async () => {
        const link = `${CARD} .ref-link[data-ref="041-sample-report"]`;
        await page.hover(link);
        await settle(page);
        await page.screenshot({ path: `${SHOTS}card-lines-ref-hover${suffix}.png`, clip });
        assert.equal(await color(page, link), await tokenColor(page, "--accent"));
        assert.equal(
          await page.$eval(link, (el) => getComputedStyle(el).textDecorationThickness),
          "2px",
        );
        await page.mouse.move(0, 0);
        assert.equal(await color(page, link), await tokenColor(page, "--ref-fg"));
      });

      await t.test(`ref link ${scheme}`, async () => {
        assert.equal(await page.$('.card[data-id="043-orphan"] .ref-link'), null);
        assert.equal(
          await page.$eval('.card[data-id="043-orphan"] .refs', (el) => el.textContent),
          "follow-up of 001-gone",
        );
        await page.locator(`${CARD} .ref-link[data-ref="041-sample-report"]`).click();
        await page.waitForFunction(
          () => document.getElementById("d-title")?.textContent === "Sample report",
          wait,
        );
        assert.equal(await page.evaluate(() => location.hash), "#041-sample-report");
        await page.click("#d-close");
        await page.waitForFunction(() => document.getElementById("detail").hidden, wait);
      });

      await t.test(`display setting ${scheme}`, async () => {
        await page.evaluate(() => localStorage.setItem("factory-hidden-card-fields", "meta"));
        try {
          await page.reload({ waitUntil: "domcontentloaded" });
          await hostLabelShown(page);
          assert.deepEqual(
            await cardLines(page, "042-sample-export"),
            LINES_042.filter(([cls]) => cls !== "meta branch line" && cls !== "meta round line"),
          );
        } finally {
          await page.evaluate(() => localStorage.removeItem("factory-hidden-card-fields"));
        }
      });
      await page.close();
    }
    assert.deepEqual(errors, []);
  });
});
