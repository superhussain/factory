// On a phone the job details under the title slide up with the page, one pixel per pixel,
// leaving the title and tabs, against one owned local Worker.
import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { crc32, deflateSync } from "node:zlib";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

function png(w, h, [r, g, b]) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([len, body, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array(w).fill([r, g, b]).flat())]);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(Buffer.concat(Array(h).fill(row)))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const paras = (n) =>
  Array.from({ length: n }, (_, i) => `Synthetic paragraph ${i + 1}.`).join("\n\n");
const FILES = {
  "spec.md": `# Spec\n\n${paras(60)}\n`,
  "build.md": `# Build\n\n${paras(60)}\n`,
  "decision.md": "# Decision\n",
};
const IMAGE = png(320, 200, [33, 118, 174]);
const PDF = "%PDF-1.4\n%%EOF\n";
const CSV = "region,total\nnorth,12\n";
const sha = (b) => createHash("sha256").update(b).digest("hex");
const MOBILE = { width: 390, height: 844, isMobile: true, hasTouch: true };
const DESKTOP = { width: 1280, height: 900 };
const wait = { polling: 50 };
const captureDir = process.env.FACTORY_CAPTURE_DIR;
if (captureDir) await mkdir(captureDir, { recursive: true });
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
    for (const [path, type, body] of [
      [`idea-images/${sha(IMAGE)}.png`, "image/png", IMAGE],
      [`idea-files/${sha(PDF)}.pdf`, "application/pdf", PDF],
      [`idea-files/${sha(CSV)}.csv`, "text/csv", CSV],
    ]) {
      const r = await fetch(`${origin}/api/${path}`, {
        method: "PUT",
        headers: { "Content-Type": type },
        body,
      });
      assert.equal(r.status, 200, path);
    }
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
      {
        id: "003-media",
        project: "sample",
        title: "Synthetic 003-media",
        stage: "review",
        branch: "factory/003-media",
        updated: "2026-01-01T00:00:00Z",
        idea: "# Idea\n\n" + paras(60) + "\n",
        idea_images: [sha(IMAGE) + ".png"],
        idea_files: [sha(PDF) + ".pdf", sha(CSV) + ".csv"],
      },
    ]);
    for (const job of ["001-compact", "002-agents", "003-media"]) {
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
      await (captureDir &&
        page.screenshot({
          path: join(captureDir, "drawer-pr-focus-390.png"),
          clip: { x: 0, y: 0, width: 390, height: 260 },
        }));
      // The page scrolls back far enough to show the link 4 px under the band.
      const shown = await place();
      near(shown.pr, shown.title + 4, "PR link top");
      near((await state()).scrollTop, rest.pr - rest.title - 4, "scrollTop");
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
    // The middle of the part of #d-meta the title band does not cover, and whether a touch there lands on it.
    const touchPoint = () =>
      page.$eval("#d-meta", (el) => {
        const r = el.getBoundingClientRect();
        const top = Math.max(
          r.top,
          document.getElementById("d-title").getBoundingClientRect().bottom,
        );
        const x = r.x + r.width / 2;
        const y = (top + r.bottom) / 2;
        return { x, y, onMeta: el.contains(document.elementFromPoint(x, y)) };
      });

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
        assert.ok(f.top >= f.titleBottom + 3, `${label} ${f.text} top ${f.top}`);
        assert.ok(f.bottom <= f.innerHeight - 3, `${label} ${f.text} bottom ${f.bottom}`);
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

    // On a desktop a tall open agents table scrolls in its own box, so the tabs and page stay on screen.
    await t.test("D16", async () => {
      await open("spec", DESKTOP, "002-agents");
      await page.waitForFunction(() => !document.getElementById("d-agents").hidden, wait);
      await page.$eval("#d-agents", (el) => (el.open = true));
      await frames();
      const before = await page.$eval("#d-agents .table-wrap", (el) => ({
        client: el.clientHeight,
        scroll: el.scrollHeight,
      }));
      assert.ok(before.client <= 361, `wrap height ${before.client}`);
      assert.ok(before.scroll > before.client + 50, `wrap scrollHeight ${before.scroll}`);
      await page.$eval("#d-agents .table-wrap", (el) => (el.scrollTop = el.scrollHeight));
      await frames();
      const s = await page.evaluate(() => {
        const rect = (el) => el.getBoundingClientRect();
        const wrap = document.querySelector("#d-agents .table-wrap");
        const trs = wrap.querySelectorAll("tr");
        return {
          wrapTop: rect(wrap).top,
          wrapBottom: rect(wrap).bottom,
          lastBottom: rect(trs[trs.length - 1]).bottom,
          thTop: rect(wrap.querySelector("thead th")).top,
          tabsBottom: rect(document.getElementById("d-tabs")).bottom,
          docHeight: rect(document.getElementById("d-doc")).height,
          innerHeight,
        };
      });
      assert.ok(s.lastBottom <= s.wrapBottom + 1, `last row ${s.lastBottom}, wrap ${s.wrapBottom}`);
      near(s.thTop, s.wrapTop, "sticky header");
      assert.ok(s.tabsBottom <= s.innerHeight, `tabs bottom ${s.tabsBottom}`);
      assert.ok(s.docHeight >= 200, `page height ${s.docHeight}`);
      // Row 10 sits at the wrap top, under the sticky header, as Shift+Tab leaves it.
      await page.$eval("#d-agents tbody tr:nth-child(10) td:nth-child(3)", (td) => {
        const wrap = td.closest(".table-wrap");
        wrap.scrollTop += td.getBoundingClientRect().top - wrap.getBoundingClientRect().top;
        const a = document.createElement("a");
        a.href = "#run-10";
        a.textContent = "Run 10";
        td.append(a);
        a.focus();
      });
      await frames();
      const f = await page.evaluate(() => ({
        linkTop: document.activeElement.getBoundingClientRect().top,
        theadBottom: document.querySelector("#d-agents thead th").getBoundingClientRect().bottom,
      }));
      assert.ok(f.linkTop >= f.theadBottom, `focused link ${f.linkTop}, header ${f.theadBottom}`);
      await openAgents("spec");
      assert.equal(
        await page.$eval("#d-agents .table-wrap", (el) => getComputedStyle(el).maxHeight),
        "none",
      );
    });

    const openMedia = async (viewport = MOBILE) => {
      await open("idea", viewport, "003-media");
      await page.waitForFunction(() => {
        const img = document.querySelector("#d-gallery img");
        return img && img.complete && img.naturalWidth > 0;
      }, wait);
      await frames();
    };
    const media = () =>
      page.evaluate(() => {
        const rect = (s) => document.querySelector(s).getBoundingClientRect().toJSON();
        const detail = document.getElementById("detail");
        return {
          on: detail.classList.contains("head-media"),
          collapse: parseFloat(getComputedStyle(detail).getPropertyValue("--head-collapse")),
          scrollTop: document.getElementById("d-doc").scrollTop,
          title: rect("#d-title"),
          tabs: rect("#d-tabs"),
          gallery: rect("#d-gallery"),
          files: rect("#d-files"),
          link: rect("#d-files a"),
          open: rect("#d-gallery .gallery-open"),
          h1: rect("#d-doc h1"),
        };
      });
    const hit = (r, selector) =>
      page.evaluate(
        (x, y, sel) => Boolean(document.elementFromPoint(x, y)?.closest(sel)),
        r.x + r.width / 2,
        r.y + r.height / 2,
        selector,
      );
    let mediaRest;

    // On a phone an idea with images and files collapses its head; the media stack under the tabs and stay tappable.
    await t.test("D17", async () => {
      await openMedia();
      const s = await media();
      assert.equal(s.on, true);
      assert.ok(s.collapse > 40, `--head-collapse ${s.collapse}`);
      assert.ok(
        s.gallery.top >= s.tabs.bottom - 1,
        `gallery ${s.gallery.top}, tabs ${s.tabs.bottom}`,
      );
      assert.ok(
        s.files.top >= s.gallery.bottom - 1,
        `files ${s.files.top}, gallery ${s.gallery.bottom}`,
      );
      assert.ok(s.h1.top >= s.files.bottom - 1, `h1 ${s.h1.top}, files ${s.files.bottom}`);
      assert.equal(await hit(s.link, "#d-files a"), true);
      assert.equal(await hit(s.open, ".gallery-open"), true);
      mediaRest = s;
    });

    // The head slides by at most C and pins; the media and page move with the whole scroll.
    await t.test("D18", async () => {
      const rest = mediaRest;
      await scrollTo(40);
      const a = await media();
      near(a.tabs.top, rest.tabs.top - 40, "tabs at 40");
      near(a.link.top, rest.link.top - 40, "link at 40");
      near(a.h1.top, rest.h1.top - 40, "h1 at 40");
      assert.equal(a.title.top, rest.title.top);
      await scrollTo(300);
      const b = await media();
      near(b.tabs.top, rest.tabs.top - rest.collapse, "tabs at 300");
      near(b.link.top, rest.link.top - 300, "link at 300");
      near(b.h1.top, rest.h1.top - 300, "h1 at 300");
    });

    // A wheel on the gallery and a drag on a file link scroll the page, and the link keeps pace.
    await t.test("D19", async () => {
      await openMedia();
      const rest = await media();
      await page.mouse.move(rest.open.x + rest.open.width / 2, rest.open.y + rest.open.height / 2);
      await page.mouse.wheel({ deltaY: 100 });
      await page.waitForFunction(() => document.getElementById("d-doc").scrollTop >= 99, wait);
      await frames();
      const w = await media();
      near(w.scrollTop, 100, "scrollTop after wheel");
      near(rest.link.top - w.link.top, 100, "link after wheel");
      await scrollTo(0);
      const back = await media();
      await drag(back.link.x + back.link.width / 2, back.link.y + back.link.height / 2, 150);
      const d = await media();
      assert.ok(d.scrollTop > 100, `scrollTop after drag ${d.scrollTop}`);
      near(back.link.top - d.link.top, d.scrollTop, "link after drag");
      near(back.h1.top - d.h1.top, d.scrollTop, "h1 after drag");
    });

    // Other tabs and the desktop drawer keep the gallery and files outside the page, as before.
    await t.test("D20", async () => {
      await openMedia();
      await page.click('.tab[data-name="spec.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Spec",
        wait,
      );
      await frames();
      const styles = () =>
        page.evaluate(() => ({
          on: document.getElementById("detail").classList.contains("head-media"),
          parents: ["d-gallery", "d-files"].map(
            (id) => document.getElementById(id).parentElement.id,
          ),
          position: getComputedStyle(document.getElementById("d-gallery")).position,
        }));
      const spec = await styles();
      assert.equal(spec.on, false);
      assert.deepEqual(spec.parents, ["d-pager", "d-pager"]);
      await openMedia(DESKTOP);
      const desk = await styles();
      assert.equal(desk.on, false);
      assert.deepEqual(desk.parents, ["d-pager", "d-pager"]);
      assert.equal(desk.position, "static");
    });

    // A touch drag that starts on the job details scrolls the page both ways; a sideways drag does not.
    await t.test("D21", async () => {
      await open("spec");
      const rest = await state();
      const collapse = parseFloat(rest.collapse);
      let m = await touchPoint();
      assert.ok(m.onMeta, `touch at rest lands on #d-meta: ${m.x}, ${m.y}`);
      await page.touchscreen.touchStart(m.x, m.y);
      for (let k = 1; k <= 3; k++) await page.touchscreen.touchMove(m.x - 40 * k, m.y);
      await page.touchscreen.touchEnd();
      await frames();
      assert.equal((await state()).scrollTop, 0);
      await drag(m.x, m.y, 150);
      const up = await state();
      near(up.scrollTop, 150, "scrollTop after drag");
      near(rest.h1Top - up.h1Top, 150, "h1 after drag");
      near(up.tabsTop, rest.tabsTop - Math.min(collapse, 150), "tabs after drag");
      await scrollTo(30);
      m = await touchPoint();
      assert.ok(m.onMeta, `touch at scrollTop 30 lands on #d-meta: ${m.x}, ${m.y}`);
      await drag(m.x, m.y, -30);
      const down = await state();
      near(down.scrollTop, 0, "scrollTop after drag down");
      near(down.h1Top, rest.h1Top, "h1 back at rest");
    });

    // A flick on the job details keeps the page moving after the finger lifts.
    // A held finger does not glide, and a new touch or the page top stops the glide.
    await t.test("D22", async () => {
      await open("spec");
      const cdp = await page.createCDPSession();
      // Touches carry this clock as their timestamp, so flick speed does not depend on machine load.
      let flickClock = Date.now();
      const touch = (type, x, y) =>
        cdp.send("Input.dispatchTouchEvent", {
          type,
          touchPoints: type === "touchEnd" ? [] : [{ x, y }],
          timestamp: flickClock / 1000,
        });
      // Three moves of dy px 40 ms apart, then a lift `hold` ms after the last move.
      const flick = async (m, dy, hold) => {
        flickClock = Math.max(flickClock, Date.now());
        await touch("touchStart", m.x, m.y);
        for (let k = 1; k <= 3; k++) {
          flickClock += 40;
          await touch("touchMove", m.x, m.y - dy * k);
        }
        flickClock += hold;
        await touch("touchEnd");
      };
      const gliding = () => page.evaluate(() => headGlide !== null);
      const rest = await state();
      const collapse = parseFloat(rest.collapse);
      const m = await touchPoint();
      assert.ok(m.onMeta, `touch at rest lands on #d-meta: ${m.x}, ${m.y}`);

      await flick(m, 30, 150);
      await frames();
      assert.equal(await gliding(), false, "held finger glides");
      near((await state()).scrollTop, 90, "scrollTop after held drag");

      await scrollTo(0);
      await flick(m, 30, 0);
      await page.waitForFunction(() => headGlide === null, wait);
      await frames();
      const s = await state();
      assert.ok(s.scrollTop > 190, `scrollTop after flick ${s.scrollTop}`);
      near(rest.h1Top - s.h1Top, s.scrollTop, "h1 after flick");
      near(s.tabsTop, rest.tabsTop - Math.min(collapse, s.scrollTop), "tabs after flick");

      await scrollTo(0);
      await flick(m, 30, 0);
      await frames();
      assert.equal(await gliding(), true, "flick glides");
      await touch("touchStart", 195, 600);
      assert.equal(await gliding(), false, "touch stops the glide");
      const caught = (await state()).scrollTop;
      await frames();
      near((await state()).scrollTop, caught, "scrollTop after the stopping touch");
      await touch("touchEnd");

      // A scroll set by code mid-glide, like a tab change, stops the glide and stays put.
      await scrollTo(0);
      await flick(m, 30, 0);
      await frames();
      assert.equal(await gliding(), true, "second flick glides");
      await page.evaluate(() => {
        document.getElementById("d-doc").scrollTop = 0;
      });
      await frames();
      await frames();
      assert.equal(await gliding(), false, "code scroll stops the glide");
      assert.equal((await state()).scrollTop, 0, "scrollTop after code scroll mid-glide");

      await scrollTo(0);
      await flick(m, -30, 0);
      await frames();
      assert.equal(await gliding(), false, "flick down at the page top glides");
      assert.equal((await state()).scrollTop, 0);
      await cdp.detach();
    });

    // Hidden gallery and file list take no space on a tab without media.
    await t.test("D23", async () => {
      await open("spec", DESKTOP);
      const boxes = await page.evaluate(() =>
        ["d-gallery", "d-files"].map((id) => {
          const el = document.getElementById(id);
          return { id, hidden: el.hidden, height: el.offsetHeight };
        }),
      );
      assert.deepEqual(boxes, [
        { id: "d-gallery", hidden: true, height: 0 },
        { id: "d-files", hidden: true, height: 0 },
      ]);
    });

    // A focused file link or gallery button keeps focus when a width change moves the media.
    await t.test("D24", async () => {
      await openMedia();
      const parent = () => page.evaluate(() => document.getElementById("d-files").parentElement.id);
      await page.focus("#d-files a");
      assert.equal(await parent(), "d-doc");
      await page.setViewport({ ...MOBILE, width: 1280, height: 900 });
      await frames();
      assert.equal(await parent(), "d-pager");
      assert.equal(
        await page.evaluate(() => document.activeElement === document.querySelector("#d-files a")),
        true,
      );
      await page.focus("#d-gallery .gallery-open");
      await page.setViewport(MOBILE);
      await frames();
      assert.equal(await parent(), "d-doc");
      assert.equal(
        await page.evaluate(
          () => document.activeElement === document.querySelector("#d-gallery .gallery-open"),
        ),
        true,
      );
    });

    assert.deepEqual(errors, []);
  });
});
