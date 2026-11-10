// One line per fact on a job card: order, one-row truncation with a title tooltip, colour per
// kind, host label from the registry and ref links, at desktop light and mobile dark.
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
  light: { "--info": "rgb(29, 78, 216)" },
  dark: { "--info": "rgb(147, 197, 253)" },
};
const CARD = '.card[data-id="042-sample-export"]';

const cardLines = (page, id) =>
  page.$$eval(`.card[data-id="${id}"] > div`, (els) =>
    els.map((el) => [el.className, el.textContent]),
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
const color = (page, selector) => page.$eval(selector, (el) => getComputedStyle(el).color);
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
      await page.waitForSelector(CARD);
      await page.waitForNetworkIdle({ idleTime: 300 });
      await settle(page);
      await (await page.$(CARD)).screenshot({ path: `${SHOTS}card-lines${suffix}.png` });

      await t.test(`lines ${scheme}`, async () => {
        assert.deepEqual(await cardLines(page, "042-sample-export"), LINES_042);
      });

      await t.test(`truncation ${scheme}`, async () => {
        const lines = await page.$$eval(".card .line", (els) =>
          els.map((el) => {
            const s = getComputedStyle(el);
            return {
              id: el.closest(".card").dataset.id,
              cls: el.className,
              text: el.textContent,
              title: el.title,
              whiteSpace: s.whiteSpace,
              textOverflow: s.textOverflow,
              overflow: s.overflowX,
            };
          }),
        );
        assert.equal(lines.length, 22);
        for (const l of lines) {
          const where = `${l.id} ${l.cls}`;
          assert.equal(l.whiteSpace, "nowrap", where);
          assert.equal(l.textOverflow, "ellipsis", where);
          assert.equal(l.overflow, "hidden", where);
          assert.equal(l.title, l.cls === "id line" ? l.id : l.text, where);
        }
        assert.equal(
          await page.$eval(`${CARD} .title`, (el) => getComputedStyle(el).webkitLineClamp),
          "2",
        );
      });

      await t.test(`colours ${scheme}`, async () => {
        assert.equal(await tokenColor(page, "--info"), TOKENS[scheme]["--info"]);
        assert.equal(await color(page, `${CARD} .branch`), await tokenColor(page, "--info"));
        assert.equal(await color(page, `${CARD} .refs`), await tokenColor(page, "--changes-fg"));
        assert.equal(await color(page, `${CARD} .host`), await tokenColor(page, "--pass-fg"));
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

      await t.test(`ref link ${scheme}`, async () => {
        assert.equal(await page.$('.card[data-id="043-orphan"] .ref-link'), null);
        assert.equal(
          await page.$eval('.card[data-id="043-orphan"] .refs', (el) => el.textContent),
          "follow-up of 001-gone",
        );
        await page.click(`${CARD} .ref-link[data-ref="041-sample-report"]`);
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
          await page.waitForSelector(CARD);
          await page.waitForNetworkIdle({ idleTime: 300 });
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
