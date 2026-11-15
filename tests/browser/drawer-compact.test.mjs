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
const AGENTS = Array.from({ length: 30 }, (_, i) => ({
  at: `2026-01-01T10:${String(i).padStart(2, "0")}:00Z`,
  stage: "review",
  role: `role-${i + 1}`,
  model: "model-a",
}));

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
      {
        id: "002-agents",
        project: "sample",
        title: "Synthetic 002-agents",
        stage: "review",
        branch: "factory/002-agents",
        updated: "2026-01-01T00:00:00Z",
        agents: AGENTS,
      },
    ]);
    for (const job of ["001-compact", "002-agents"]) {
      for (const [name, body] of Object.entries(FILES)) {
        const r = await fetch(`${origin}/api/jobs/sample/${job}/${name}`, {
          method: "PUT",
          headers: { "If-None-Match": "*" },
          body,
        });
        assert.equal(r.status, 200, `${job} ${name}`);
      }
    }
    const page = await (await browser.createBrowserContext()).newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const open = async (tab, viewport = MOBILE, job = "001-compact") => {
      await page.setViewport(viewport);
      await page.goto("about:blank");
      await page.goto(`${origin}/#${job}/${tab}`, { waitUntil: "load" });
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
          margin: parseFloat(getComputedStyle(err).marginTop),
          metaInert: document.getElementById("d-meta").inert,
          titleBottom: document.getElementById("d-title").getBoundingClientRect().bottom,
          tabsTop: document.getElementById("d-tabs").getBoundingClientRect().top,
        };
      });
      assert.ok(s.margin > 0);
      near(s.errTop, s.titleBottom + s.margin, "notice under title");
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
      // Scroll until the PR link sits 4 px under the title band, short of the full slide.
      const place = () =>
        page.evaluate(() => ({
          pr: document.getElementById("d-pr").getBoundingClientRect().top,
          title: document.getElementById("d-title").getBoundingClientRect().bottom,
          collapse: parseFloat(
            getComputedStyle(document.getElementById("detail")).getPropertyValue("--head-collapse"),
          ),
        }));
      const rest = await place();
      const hidden = Math.ceil(rest.pr - rest.title) + 4;
      assert.ok(hidden < rest.collapse, `shift ${hidden} under C ${rest.collapse}`);
      await scrollTo(hidden);
      await page.focus("#d-actions");
      await page.keyboard.down("Shift");
      await page.keyboard.press("Tab");
      await page.keyboard.up("Shift");
      assert.equal(await page.evaluate(() => document.activeElement.id), "d-pr");
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      // The page scrolls back just far enough to show the link under the band.
      const shown = await place();
      near(shown.pr, shown.title, "PR link top");
      near((await state()).scrollTop, rest.pr - rest.title, "scrollTop");
    });

    await t.test("D11", async () => {
      await open("spec", DESKTOP);
      await page.evaluate(() => {
        const pr = document.getElementById("d-pr");
        pr.href = "https://github.com/example/sample/pull/1";
        pr.textContent = "PR #1";
        pr.hidden = false;
      });
      await scrollTo(300);
      await page.focus("#d-actions");
      await page.keyboard.down("Shift");
      await page.keyboard.press("Tab");
      await page.keyboard.up("Shift");
      assert.equal(await page.evaluate(() => document.activeElement.id), "d-pr");
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      assert.equal((await state()).scrollTop, 300);
    });

    const frames = () =>
      page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
    const openAgents = async (tab, expand = true) => {
      await open(tab, MOBILE, "002-agents");
      await page.waitForFunction(() => !document.getElementById("d-agents").hidden, wait);
      if (expand) await page.$eval("#d-agents", (el) => (el.open = true));
      await frames();
    };
    const rows = () =>
      page.evaluate(() => {
        const rect = (el) => el.getBoundingClientRect();
        const cells = document.querySelectorAll("#d-agents tbody td");
        return {
          scrollTop: document.getElementById("d-doc").scrollTop,
          innerHeight,
          titleBottom: rect(document.getElementById("d-title")).bottom,
          summaryTop: rect(document.querySelector("#d-agents summary")).top,
          agentsBottom: rect(document.getElementById("d-agents")).bottom,
          tabsTop: rect(document.getElementById("d-tabs")).top,
          tabsBottom: rect(document.getElementById("d-tabs")).bottom,
          h1Top: rect(document.querySelector("#d-doc h1")).top,
          cell: cells.length ? rect(cells[4]).toJSON() : null,
          last: cells.length ? rect(cells[cells.length - 1].parentElement).toJSON() : null,
        };
      });
    const drag = async (x, y, dy) => {
      await page.touchscreen.touchStart(x, y);
      for (let k = 1; k <= 3; k++) await page.touchscreen.touchMove(x, y - (dy * k) / 3);
      // A pause before lifting the finger keeps the flick out of the measurement.
      await new Promise((r) => setTimeout(r, 150));
      await page.touchscreen.touchEnd();
      await frames();
    };

    // A tall open agents table scrolls to its last row, even on a short page.
    await t.test("D12", async () => {
      await page.setViewport(MOBILE);
      await openAgents("decision");
      const rest = await rows();
      assert.ok(rest.tabsBottom > rest.innerHeight, `tabs bottom ${rest.tabsBottom}`);
      const need = Math.ceil(rest.last.bottom - rest.innerHeight + 8);
      await scrollTo(need);
      const s = await rows();
      near(s.scrollTop, need, "scrollTop");
      assert.ok(s.last.top >= s.titleBottom, `last row top ${s.last.top}`);
      assert.ok(s.last.bottom <= s.innerHeight, `last row bottom ${s.last.bottom}`);
    });

    // A drag or wheel on the details moves them and the page together, at the same pace.
    await t.test("D13", async () => {
      await page.setViewport(MOBILE);
      await openAgents("spec");
      const rest = await rows();
      await drag(rest.cell.x + 4, rest.cell.y + rest.cell.height / 2, 150);
      const s = await rows();
      // The browser keeps a few pixels of touch slop, so the drag scrolls a little under 150.
      assert.ok(s.scrollTop > 100, `scrollTop after drag ${s.scrollTop}`);
      near(rest.cell.y - s.cell.y, s.scrollTop, "cell after drag");
      near(rest.h1Top - s.h1Top, s.scrollTop, "h1 after drag");
      assert.equal(
        await page.evaluate(() =>
          Boolean(document.elementFromPoint(195, 500).closest("#d-agents")),
        ),
        true,
      );
      await page.mouse.move(195, 500);
      await page.mouse.wheel({ deltaY: 100 });
      const after = s.scrollTop + 100;
      await page.waitForFunction(
        (top) => document.getElementById("d-doc").scrollTop >= top,
        wait,
        after - 1,
      );
      await frames();
      const w = await rows();
      near(w.scrollTop, after, "scrollTop after wheel");
      near(s.cell.y - w.cell.y, 100, "cell after wheel");
      near(s.h1Top - w.h1Top, 100, "h1 after wheel");
    });

    // Opening or closing agents mid-scroll re-measures with no gap, overlap or jump.
    await t.test("D14", async () => {
      await page.setViewport(MOBILE);
      await openAgents("spec", false);
      await scrollTo(20);
      const closed = await rows();
      await page.click("#d-agents summary");
      await frames();
      const opened = await rows();
      assert.equal(opened.scrollTop, 20);
      near(opened.summaryTop, closed.summaryTop, "summary after open");
      assert.ok(opened.tabsTop >= opened.agentsBottom, "tabs below the table");
      near(opened.h1Top - opened.tabsBottom, closed.h1Top - closed.tabsBottom, "gap after open");
      await page.click("#d-agents summary");
      await frames();
      const shut = await rows();
      assert.equal(shut.scrollTop, 20);
      near(shut.summaryTop, closed.summaryTop, "summary after close");
      near(shut.h1Top - shut.tabsBottom, closed.h1Top - closed.tabsBottom, "gap after close");

      // On a short page the newly opened table still scrolls to its last row. Closing it
      // leaves nothing to scroll, so the browser settles the page back at its top with the same gap.
      await openAgents("decision", false);
      const rest = await rows();
      await page.click("#d-agents summary");
      await frames();
      const tall = await rows();
      const need = Math.ceil(tall.last.bottom - tall.innerHeight + 8);
      await scrollTo(need);
      const bottom = await rows();
      near(bottom.scrollTop, need, "scrollTop");
      assert.ok(bottom.last.bottom <= bottom.innerHeight, `last row bottom ${bottom.last.bottom}`);
      await scrollTo(20);
      await page.click("#d-agents summary");
      await frames();
      const back = await rows();
      assert.equal(back.scrollTop, 0);
      near(back.summaryTop, rest.summaryTop, "summary back at rest");
      near(back.h1Top - back.tabsBottom, rest.h1Top - rest.tabsBottom, "gap back at rest");
    });

    // Tabbing through a tall agents table keeps each focused link on screen, both ways.
    await t.test("D15", async () => {
      await page.setViewport(MOBILE);
      await openAgents("spec");
      await page.evaluate(() => {
        for (const [i, td] of document
          .querySelectorAll("#d-agents tbody td:nth-child(3)")
          .entries()) {
          const a = document.createElement("a");
          a.href = `#run-${i + 1}`;
          a.textContent = `Run ${i + 1}`;
          td.append(a);
        }
      });
      await page.focus("#d-agents summary");
      const seen = () =>
        page.evaluate(() => {
          const r = document.activeElement.getBoundingClientRect();
          return {
            text: document.activeElement.textContent,
            top: r.top,
            bottom: r.bottom,
            titleBottom: document.getElementById("d-title").getBoundingClientRect().bottom,
            innerHeight,
            scrollTop: document.getElementById("d-doc").scrollTop,
          };
        });
      const check = (f, label) => {
        assert.ok(f.top >= f.titleBottom - 1, `${label} ${f.text} top ${f.top}`);
        assert.ok(f.bottom <= f.innerHeight + 1, `${label} ${f.text} bottom ${f.bottom}`);
      };
      for (let i = 1; i <= 30; i++) {
        await page.keyboard.press("Tab");
        await frames();
        const f = await seen();
        assert.equal(f.text, `Run ${i}`);
        check(f, "Tab");
      }
      assert.ok((await seen()).scrollTop > 0);
      await page.keyboard.down("Shift");
      for (let i = 29; i >= 1; i--) {
        await page.keyboard.press("Tab");
        await frames();
        const f = await seen();
        assert.equal(f.text, `Run ${i}`);
        check(f, "Shift+Tab");
      }
      await page.keyboard.up("Shift");
    });

    assert.deepEqual(errors, []);
  });
});
