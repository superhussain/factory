// Job panel header against one owned local Worker: every header control shares one vertical
// center, the close button gives the same hover and focus feedback as the actions button,
// a tap on touch leaves no hover style, and Display settings hide header links per browser
// in localStorage, including on reset.
import assert from "node:assert/strict";
import test from "node:test";
import { mkdir } from "node:fs/promises";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  seedPipeline,
} from "../support/browser-server.mjs";

const JOB = {
  id: "042-sample-links",
  project: "sample",
  title: "Synthetic linked job",
  stage: "pr-open",
  branch: "factory/042-sample-links",
  pr: "https://github.com/example/sample/pull/42",
  paseo: { server: "synthetic-server", agent: "synthetic-agent" },
  tracer: { url: "https://tracer.example.invalid/#/f/factory-42" },
  updated: "2026-01-01T00:00:00Z",
};
const BATCH = [
  {
    id: "090-sample-export-csv",
    project: "sample",
    title: "Synthetic lead",
    stage: "pr-open",
    pr: "https://github.com/example/sample/pull/90",
    paseo: { server: "synthetic-server", agent: "synthetic-agent" },
    updated: "2026-01-01T00:00:00Z",
  },
  ...[
    ["091-sample-typo", "090-sample-export-csv", null],
    ["093-sample-own", "090-sample-export-csv", { server: "synthetic-server", agent: "own-agent" }],
    ["095-sample-orphan", "094-sample-no-session", null],
  ].map(([id, lead, paseo]) => ({
    id,
    project: "sample",
    title: "Synthetic rider",
    stage: "pr-open",
    pr: "https://github.com/example/sample/pull/90",
    refs: [{ kind: "batched-into", id: lead }],
    ...(paseo && { paseo }),
    updated: "2026-01-01T00:00:00Z",
  })),
  {
    id: "094-sample-no-session",
    project: "sample",
    title: "Synthetic lead without a session",
    stage: "pr-open",
    pr: "https://github.com/example/sample/pull/94",
    updated: "2026-01-01T00:00:00Z",
  },
];
const KEY = "factory-hidden-drawer-links";
const HEAD = ["#d-pr", "#d-paseo", "#d-paseo-web", "#d-tracer", "#d-actions", "#d-close"];
const DESKTOP = { width: 1280, height: 900 };
const MOBILE = { width: 390, height: 844, isMobile: true, hasTouch: true };
const wait = { polling: 50 };

test("drawer header links", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB, ...BATCH]);
    await seedPipeline(origin, { schema: 1, tracer: { enabled: true } });
    const page = await (await browser.createBrowserContext()).newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const openJob = async (viewport = DESKTOP) => {
      await page.setViewport(viewport);
      await page.goto("about:blank");
      await page.goto(`${origin}/#042-sample-links`, { waitUntil: "load" });
      await page.waitForFunction(() => tracerOff === false);
      await page.waitForFunction(() => !document.getElementById("d-pr").hidden, wait);
    };
    const openDisplay = async () => {
      await page.setViewport(DESKTOP);
      await page.goto("about:blank");
      await page.goto(`${origin}/#/settings/display`, { waitUntil: "load" });
      await page.waitForSelector("#display-drawer-links", { visible: true });
    };
    const boxes = (selectors) =>
      page.evaluate(
        (list) =>
          list
            .map((s) => document.querySelector(s))
            .filter((el) => el.checkVisibility())
            .map((el) => {
              const r = el.getBoundingClientRect();
              return { id: el.id, center: r.top + r.height / 2, width: r.width, height: r.height };
            }),
        selectors,
      );
    const spread = (list) =>
      Math.max(...list.map((b) => b.center)) - Math.min(...list.map((b) => b.center));
    const visible = (id) => page.$eval(id, (el) => el.checkVisibility());

    await t.test("desktop header centers", async () => {
      await openJob();
      const list = await boxes(HEAD);
      assert.equal(list.length, HEAD.length, JSON.stringify(list));
      assert.ok(spread(list) <= 1, JSON.stringify(list));
    });

    await t.test("mobile header centers", async () => {
      await openJob(MOBILE);
      const list = await boxes(["#d-actions", "#d-close"]);
      assert.equal(list.length, 2);
      assert.ok(spread(list) <= 1, JSON.stringify(list));
      for (const b of list) assert.deepEqual([b.width, b.height], [44, 44], b.id);
    });

    await t.test("settings rows", async () => {
      await openDisplay();
      assert.equal(
        await page.$eval("#display-drawer-links legend", (el) => el.textContent),
        "Job panel links",
      );
      const rows = await page.$$eval("#display-drawer-links input", (els) =>
        els.map((el) => [el.id, el.labels[0].textContent.trim(), el.checked]),
      );
      assert.deepEqual(rows, [
        ["display-drawer-link-pr", "PR", true],
        ["display-drawer-link-app", "Session app", true],
        ["display-drawer-link-web", "Session", true],
        ["display-drawer-link-tracer", "Tracer", true],
      ]);
    });

    await t.test("hide web", async () => {
      await openJob();
      const href = await page.$eval("#d-paseo", (el) => el.getAttribute("href"));
      await page.evaluate(() => (location.hash = "#/settings/display"));
      await page.waitForSelector("#display-drawer-link-web", { visible: true });
      await page.click("#display-drawer-link-web");
      assert.equal(await page.evaluate((k) => localStorage.getItem(k), KEY), "web");
      for (const reload of [false, true]) {
        if (reload) await openJob();
        else {
          await page.evaluate(() => (location.hash = "#042-sample-links"));
          await page.waitForFunction(() => !document.getElementById("detail").hidden, wait);
        }
        assert.equal(await visible("#d-paseo-web"), false, `reload ${reload}`);
        assert.equal(await visible("#d-paseo"), true, `reload ${reload}`);
        assert.equal(await page.$eval("#d-paseo", (el) => el.getAttribute("href")), href);
      }
      await openDisplay();
      assert.equal(await page.$eval("#display-drawer-link-web", (el) => el.checked), false);
    });

    await t.test("bad value and reset", async () => {
      await page.evaluate((k) => localStorage.setItem(k, "bogus,web"), KEY);
      await openJob();
      for (const id of ["#d-pr", "#d-paseo", "#d-tracer"])
        assert.equal(await visible(id), true, id);
      assert.equal(await visible("#d-paseo-web"), false);
      await openDisplay();
      await page.click("#display-reset");
      assert.equal(await page.evaluate((k) => localStorage.getItem(k), KEY), null);
      assert.equal(await page.$eval("#display-drawer-link-web", (el) => el.checked), true);
      await openJob();
      assert.equal(await visible("#d-paseo-web"), true);
    });

    await t.test("close matches actions on hover and focus", async () => {
      await openJob();
      await page.addStyleTag({ content: "*{transition:none!important}" });
      const hovered = async (id) => {
        await page.hover(id);
        return page.$eval(id, (el) => {
          const s = getComputedStyle(el);
          return { borderTopColor: s.borderTopColor, color: s.color };
        });
      };
      assert.deepEqual(await hovered("#d-close"), await hovered("#d-actions"));
      const focusStyle = (id) =>
        page.$eval(id, (el) => {
          const s = getComputedStyle(el);
          return {
            outlineStyle: s.outlineStyle,
            outlineWidth: s.outlineWidth,
            outlineColor: s.outlineColor,
            outlineOffset: s.outlineOffset,
            borderTopColor: s.borderTopColor,
          };
        });
      await page.mouse.move(0, 0);
      await page.focus("#d-actions");
      await page.keyboard.press("Tab");
      assert.equal(await page.evaluate(() => document.activeElement.id), "d-close");
      assert.equal(await page.$eval("#d-close", (el) => el.matches(":focus-visible")), true);
      const close = await focusStyle("#d-close");
      await page.keyboard.down("Shift");
      await page.keyboard.press("Tab");
      await page.keyboard.up("Shift");
      assert.equal(await page.evaluate(() => document.activeElement.id), "d-actions");
      assert.deepEqual(close, await focusStyle("#d-actions"));
    });

    await t.test("tap leaves no hover on touch", async () => {
      await openJob(MOBILE);
      await page.addStyleTag({ content: "*{transition:none!important}" });
      assert.equal(await page.evaluate(() => matchMedia("(hover: hover)").matches), false);
      const border = () => page.$eval("#d-actions", (el) => getComputedStyle(el).borderTopColor);
      const rest = await border();
      await page.tap("#d-actions");
      assert.equal(await border(), rest);
    });

    await t.test("reset re-renders open links", async () => {
      await page.evaluate((k) => localStorage.setItem(k, "web"), KEY);
      await openJob();
      assert.equal(await visible("#d-paseo-web"), false);
      await page.evaluate(() => document.getElementById("display-reset").click());
      assert.equal(await visible("#d-paseo-web"), true);
      assert.equal(await visible("#detail"), true);
      assert.equal(await page.evaluate((k) => localStorage.getItem(k), KEY), null);
    });

    await t.test("rider uses the lead's session", async () => {
      await page.setViewport(DESKTOP);
      const links = async (id) => {
        await page.goto("about:blank");
        await page.goto(`${origin}/#${id}`, { waitUntil: "load" });
        await page.waitForFunction(() => !document.getElementById("d-pr").hidden, wait);
        return page.evaluate(() =>
          ["d-paseo", "d-paseo-web"].map((i) => {
            const el = document.getElementById(i);
            return [el.hidden, el.getAttribute("href"), el.getAttribute("aria-label")];
          }),
        );
      };
      const [leadApp, leadWeb] = await links("090-sample-export-csv");
      assert.equal(leadApp[2], "Open this job's session in Session app");
      assert.deepEqual(await links("091-sample-typo"), [
        [false, leadApp[1], "Open the session of 090-sample-export-csv in Session app"],
        [false, leadWeb[1], "Open the session of 090-sample-export-csv in Session"],
      ]);
      const own = await links("093-sample-own");
      assert.match(own[0][1], /own-agent/);
      assert.match(own[1][1], /own-agent/);
      assert.deepEqual(
        own.map((l) => [l[0], l[2]]),
        [
          [false, "Open this job's session in Session app"],
          [false, "Open this job's session in Session"],
        ],
      );
      assert.deepEqual(await links("095-sample-orphan"), [
        [true, null, null],
        [true, null, null],
      ]);
      const jumperLinks = async (id, shot) => {
        await links(id);
        await page.keyboard.down("Control");
        await page.keyboard.press("k");
        await page.keyboard.up("Control");
        await page.waitForSelector("#job-picker[open]");
        if (shot) {
          await mkdir(".context/shots", { recursive: true });
          await (await page.$("#job-picker")).screenshot({ path: shot });
        }
        const titles = await page.$$eval('#job-results [role="option"] .title', (els) =>
          els
            .map((el) => el.textContent)
            .filter((t) => t.startsWith("Open in") || t.startsWith("Open the session")),
        );
        await page.keyboard.press("Escape");
        return titles;
      };
      const riderTitles = await jumperLinks(
        "091-sample-typo",
        ".context/shots/jumper-rider-links-1280.png",
      );
      assert.deepEqual(riderTitles, [
        "Open the session of 090-sample-export-csv in Session app",
        "Open the session of 090-sample-export-csv in Session",
      ]);
      for (const id of ["090-sample-export-csv", "093-sample-own"])
        assert.deepEqual(await jumperLinks(id), ["Open in Session app", "Open in Session"], id);
    });

    assert.deepEqual(errors, []);
  });
});
