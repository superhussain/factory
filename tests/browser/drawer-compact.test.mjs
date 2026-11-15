// On a phone the job details under the title slide up with the page, one pixel per pixel,
// leaving the title and tabs, against one owned local Worker.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const paras = (n) =>
  Array.from({ length: n }, (_, i) => `Synthetic paragraph ${i + 1}.`).join("\n\n");
const FILES = {
  "spec.md": `# Spec\n\n${paras(60)}\n`,
  "build.md": `# Build\n\n${paras(60)}\n`,
  "decision.md": "# Decision\n",
};
const MOBILE = { width: 390, height: 844, isMobile: true, hasTouch: true };
const DESKTOP = { width: 1280, height: 900 };
const wait = { polling: 50 };

test("drawer head scrolls with the page", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(origin, "sample", SEED);
    await seedBoard(origin, [
      {
        id: "001-compact",
        project: "sample",
        title: "Synthetic 001-compact",
        stage: "review",
        branch: "factory/001-compact",
        pr: "https://github.com/example/sample/pull/1",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    for (const [name, body] of Object.entries(FILES)) {
      const r = await fetch(`${origin}/api/jobs/sample/001-compact/${name}`, {
        method: "PUT",
        headers: { "If-None-Match": "*" },
        body,
      });
      assert.equal(r.status, 200, name);
    }
    const page = await (await browser.createBrowserContext()).newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const open = async (tab, viewport = MOBILE) => {
      await page.setViewport(viewport);
      await page.goto("about:blank");
      await page.goto(`${origin}/#001-compact/${tab}`, { waitUntil: "load" });
      await page.waitForFunction(
        (h) => document.querySelector("#d-doc h1")?.textContent === h,
        wait,
        tab[0].toUpperCase() + tab.slice(1),
      );
    };
    const scrollTo = async (top) => {
      await page.$eval("#d-doc", (el, y) => (el.scrollTop = y), top);
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
    };
    const MOVERS =
      "#d-meta, #d-pipeline, #d-chat, #d-held, #d-refs, #d-tracer-tasks, #d-agents, .detail-head .links, .detail > .err, .detail > .copy-status, #d-assignment, #d-tabs";
    const state = () =>
      page.evaluate(() => {
        const rect = (s) => document.querySelector(s).getBoundingClientRect();
        return {
          collapse: getComputedStyle(document.getElementById("detail")).getPropertyValue(
            "--head-collapse",
          ),
          meta: document.getElementById("d-meta").offsetHeight,
          metaInert: document.getElementById("d-meta").inert,
          tabsInert: document.getElementById("d-tabs").inert,
          scrollTop: document.getElementById("d-doc").scrollTop,
          titleTop: rect("#d-title").top,
          titleBottom: rect("#d-title").bottom,
          tabsTop: rect("#d-tabs").top,
          tabsBottom: rect("#d-tabs").bottom,
          metaTop: rect("#d-meta").top,
          h1Top: rect("#d-doc h1").top,
        };
      });
    const near = (actual, expected, label) =>
      assert.ok(Math.abs(actual - expected) <= 1, `${label}: ${actual}, expected ${expected}`);
    let restTabsTop;

    await t.test("D1", async () => {
      await open("spec");
      const s = await state();
      assert.ok(s.meta > 0);
      assert.ok(parseFloat(s.collapse) > 40, `--head-collapse ${s.collapse}`);
      assert.ok(s.h1Top >= s.tabsBottom, `h1 ${s.h1Top}, tabs bottom ${s.tabsBottom}`);
      restTabsTop = s.tabsTop;
    });

    await t.test("D2", async () => {
      await open("spec");
      const rest = await state();
      for (const top of [10, 20, 40]) {
        await scrollTo(top);
        const s = await state();
        assert.equal(s.scrollTop, top);
        near(s.tabsTop, rest.tabsTop - top, `tabs at ${top}`);
        near(s.metaTop, rest.metaTop - top, `meta at ${top}`);
        near(s.h1Top, rest.h1Top - top, `h1 at ${top}`);
        assert.equal(s.titleTop, rest.titleTop);
      }
    });

    await t.test("D3", async () => {
      await open("spec");
      await scrollTo(300);
      const s = await state();
      near(s.tabsTop, s.titleBottom, "tabs under title");
      assert.equal(s.metaInert, true);
      assert.equal(s.tabsInert, false);
    });

    await t.test("D4", async () => {
      await open("spec");
      await scrollTo(300);
      await scrollTo(0);
      const movers = await page.$$eval(MOVERS, (els) =>
        els.map((el) => [el.style.transform, el.inert]),
      );
      assert.ok(movers.length > 0);
      for (const m of movers) assert.deepEqual(m, ["", false]);
    });

    await t.test("D5", async () => {
      await open("spec", DESKTOP);
      const rest = await state();
      await scrollTo(300);
      const s = await state();
      assert.equal(s.tabsTop, rest.tabsTop);
      assert.equal(s.collapse, "0px");
    });

    await t.test("D6", async () => {
      await open("spec");
      await scrollTo(300);
      await page.click('.tab[data-name="build.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Build",
        wait,
      );
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      const s = await state();
      assert.equal(s.scrollTop, 0);
      near(s.tabsTop, restTabsTop, "tabs back at rest");
    });

    await t.test("D7", async () => {
      await open("decision");
      const rest = await state();
      await scrollTo(300);
      assert.equal((await state()).tabsTop, rest.tabsTop);
    });

    await t.test("D8", async () => {
      await open("spec");
      await scrollTo(300);
      await page.click("#d-actions");
      await page.waitForFunction(() => !document.getElementById("d-actions-menu").hidden, wait);
      const gap = await page.evaluate(
        () =>
          document.getElementById("d-actions-menu").getBoundingClientRect().top -
          document.getElementById("d-actions").getBoundingClientRect().bottom,
      );
      near(gap, 4, "menu gap");
    });

    await t.test("D9", async () => {
      await open("spec");
      await scrollTo(300);
      await page.evaluate(() => {
        const err = document.getElementById("d-rerun-err");
        err.textContent = "Synthetic rerun failure.";
        err.hidden = false;
      });
      await scrollTo(300);
      const s = await page.evaluate(() => {
        const err = document.getElementById("d-rerun-err");
        const r = err.getBoundingClientRect();
        return {
          errTop: r.top,
          errBottom: r.bottom,
          hit: err.contains(document.elementFromPoint(r.left + 4, r.top + r.height / 2)),
          inert: err.inert,
          metaInert: document.getElementById("d-meta").inert,
          titleBottom: document.getElementById("d-title").getBoundingClientRect().bottom,
          tabsTop: document.getElementById("d-tabs").getBoundingClientRect().top,
        };
      });
      near(s.errTop, s.titleBottom, "notice under title");
      assert.ok(s.tabsTop >= s.errBottom, `tabs ${s.tabsTop}, notice bottom ${s.errBottom}`);
      assert.equal(s.hit, true);
      assert.equal(s.inert, false);
      assert.equal(s.metaInert, true);
    });

    await t.test("D10", async () => {
      await open("spec");
      await page.evaluate(() => {
        const pr = document.getElementById("d-pr");
        pr.href = "https://github.com/example/sample/pull/1";
        pr.textContent = "PR #1";
        pr.hidden = false;
      });
      await scrollTo(20);
      await page.focus("#d-actions");
      await page.keyboard.down("Shift");
      await page.keyboard.press("Tab");
      await page.keyboard.up("Shift");
      assert.equal(await page.evaluate(() => document.activeElement.id), "d-pr");
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      assert.equal((await state()).scrollTop, 0);
      assert.equal(await page.$eval("#d-tabs", (el) => el.style.transform), "");
    });

    assert.deepEqual(errors, []);
  });
});
