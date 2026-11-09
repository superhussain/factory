// On a phone the job details above the tabs scroll away with the page, leaving the title
// and tabs, against one owned local Worker.
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

test("drawer compact header", { timeout: 180000 }, async (t) => {
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
    const state = () =>
      page.evaluate(() => {
        const h = (s) => document.querySelector(s).offsetHeight;
        return {
          compact: document.getElementById("detail").classList.contains("compact"),
          meta: h("#d-meta"),
          refs: h("#d-refs"),
          links: h(".detail-head .links"),
          title: h("#d-title"),
          scrollTop: document.getElementById("d-doc").scrollTop,
          tabsTop: document.getElementById("d-tabs").getBoundingClientRect().top,
          headBottom: document.querySelector(".detail-head").getBoundingClientRect().bottom,
        };
      });

    await t.test("C1", async () => {
      await open("spec");
      const rest = await state();
      assert.equal(rest.compact, false);
      assert.ok(rest.meta > 0);
      await scrollTo(300);
      const s = await state();
      assert.equal(s.compact, true);
      assert.deepEqual([s.meta, s.refs, s.links], [0, 0, 0]);
      assert.ok(s.title > 0);
      assert.equal(s.tabsTop, s.headBottom);
      assert.ok(s.tabsTop < rest.tabsTop, `tabs ${s.tabsTop}, rest ${rest.tabsTop}`);
    });

    await t.test("C2", async () => {
      await open("spec");
      await scrollTo(300);
      assert.equal((await state()).compact, true);
      await scrollTo(0);
      const s = await state();
      assert.equal(s.compact, false);
      assert.ok(s.meta > 0);
    });

    await t.test("C3", async () => {
      await open("spec", DESKTOP);
      await scrollTo(300);
      const s = await state();
      assert.equal(s.compact, false);
      assert.ok(s.meta > 0);
    });

    await t.test("C4", async () => {
      await open("spec");
      await scrollTo(300);
      assert.equal((await state()).compact, true);
      await page.click('.tab[data-name="build.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Build",
        wait,
      );
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      const s = await state();
      assert.equal(s.compact, false);
      assert.equal(s.scrollTop, 0);
    });

    await t.test("C5", async () => {
      await open("decision");
      await scrollTo(300);
      assert.equal((await state()).compact, false);
    });

    assert.deepEqual(errors, []);
  });
});
