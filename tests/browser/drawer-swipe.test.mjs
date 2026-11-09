// A sideways swipe on an open job's page moves between file tabs on a phone, with the
// neighbour page and the tab pill following the finger, against one owned local Worker.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const paras = (n) =>
  Array.from({ length: n }, (_, i) => `Synthetic paragraph ${i + 1}.`).join("\n\n");
const cols = Array.from({ length: 8 }, (_, i) => `Column ${i + 1}`);
const TABLE = [
  `| ${cols.join(" | ")} |`,
  `| ${cols.map(() => "---").join(" | ")} |`,
  `| ${cols.map((c) => c + " value").join(" | ")} |`,
].join("\n");
const FILES = {
  "spec.md": `# Spec\n\n${TABLE}\n\n${paras(60)}\n`,
  "build.md": `# Build\n\n${paras(60)}\n`,
  "decision.md": "# Decision\n",
};
const MOBILE = { width: 390, height: 844, isMobile: true, hasTouch: true };
const wait = { polling: 50 };
// Touches carry this clock as their timestamp, so flick speed follows the script's own
// pauses rather than how late a loaded machine delivers each event.
let clock = 0;
const sleep = (ms) => {
  clock += ms;
  return new Promise((resolve) => setTimeout(resolve, ms));
};
const line = (x0, x1, y = 500, step = 10) => {
  const points = [];
  const dir = Math.sign(x1 - x0);
  for (let x = x0 + dir * step; dir * (x1 - x) >= 0; x += dir * step) points.push([x, y]);
  return [[x0, y], ...points];
};

test("drawer swipe", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(origin, "sample", SEED);
    await seedBoard(origin, [
      {
        id: "001-swipe",
        project: "sample",
        title: "Synthetic 001-swipe",
        stage: "review",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    for (const [name, body] of Object.entries(FILES)) {
      const r = await fetch(`${origin}/api/jobs/sample/001-swipe/${name}`, {
        method: "PUT",
        headers: { "If-None-Match": "*" },
        body,
      });
      assert.equal(r.status, 200, name);
    }
    const page = await (await browser.createBrowserContext()).newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const cdp = await page.createCDPSession();
    const touch = (type, [x, y] = []) => {
      if (type === "touchStart") clock = Math.max(clock, Date.now());
      return cdp.send("Input.dispatchTouchEvent", {
        type,
        touchPoints: type === "touchEnd" ? [] : [{ x, y }],
        timestamp: clock / 1000,
      });
    };
    const drag = async (points, stepMs, release = true) => {
      await touch("touchStart", points[0]);
      for (const point of points) {
        await touch("touchMove", point);
        await sleep(stepMs);
      }
      if (release) await touch("touchEnd");
    };
    const settled = () =>
      page.waitForFunction(() => !swipeSettling && !document.querySelector(".doc-peek"), wait);
    const open = async (tab, viewport = MOBILE) => {
      await page.setViewport(viewport);
      await page.goto("about:blank");
      await page.goto(`${origin}/#001-swipe/${tab}`, { waitUntil: "load" });
      await page.waitForFunction(
        (h) => document.querySelector("#d-doc h1")?.textContent === h,
        wait,
        tab[0].toUpperCase() + tab.slice(1),
      );
    };
    const state = () =>
      page.evaluate(() => ({
        tab: document.querySelector(".tab.active")?.dataset.name,
        h1: document.querySelector("#d-doc h1")?.textContent,
        hash: location.hash,
        transform: getComputedStyle(document.getElementById("d-doc")).transform,
        peeks: document.querySelectorAll(".doc-peek").length,
      }));
    const docX = () =>
      page.$eval("#d-doc", (el) => new DOMMatrix(getComputedStyle(el).transform).m41);
    const rects = () =>
      page.evaluate(() =>
        [".detail-head", "#d-tabs"].map((s) =>
          document.querySelector(s).getBoundingClientRect().toJSON(),
        ),
      );
    const unchanged = async (tab) => {
      await settled();
      const s = await state();
      assert.deepEqual([s.tab, s.transform, s.peeks], [tab, "none", 0]);
    };

    await t.test("S1", async () => {
      await open("spec");
      const before = await rects();
      const restTop = await page.$eval("#d-doc h1", (el) => el.getBoundingClientRect().top);
      await drag(line(300, 150), 16, false);
      assert.ok(Math.abs((await docX()) + 126) <= 1, `doc x ${await docX()}`);
      const peek = await page.$eval(".doc-peek", (el) => ({
        h1: el.querySelector("h1")?.textContent,
        h1Top: el.querySelector("h1").getBoundingClientRect().top,
        left: el.getBoundingClientRect().left,
        inert: el.inert,
        ids: el.id !== "" || !!el.querySelector("[id]"),
        docs: document.querySelectorAll("#d-doc").length,
      }));
      assert.equal(peek.h1, "Build");
      assert.ok(Math.abs(peek.h1Top - restTop) <= 1, `peek h1 ${peek.h1Top}, rest ${restTop}`);
      assert.ok(Math.abs(peek.left - 264) <= 1, `peek left ${peek.left}`);
      assert.equal(peek.inert, true);
      assert.equal(peek.ids, false);
      assert.equal(peek.docs, 1);
      assert.deepEqual(await rects(), before);
      await touch("touchEnd");
      await settled();
    });

    await t.test("S2", async () => {
      await open("spec");
      await drag(line(300, 150), 16, false);
      const mid = await page.evaluate(() => {
        const tabs = [...document.querySelectorAll("#d-tabs .tab")];
        return {
          left: parseFloat(document.getElementById("d-tab-pill").style.left),
          spec: tabs[0].offsetLeft,
          build: tabs[1].offsetLeft,
        };
      });
      assert.ok(mid.left > mid.spec && mid.left < mid.build, JSON.stringify(mid));
      await touch("touchEnd");
      await settled();
      const rest = await page.evaluate(() => {
        const pill = document.getElementById("d-tab-pill");
        const tab = document.querySelector("#d-tabs .tab.active");
        return {
          pill: ["left", "top", "width", "height"].map((k) => parseFloat(pill.style[k])),
          tab: [tab.offsetLeft, tab.offsetTop, tab.offsetWidth, tab.offsetHeight],
          radius: getComputedStyle(pill).borderRadius,
        };
      });
      assert.deepEqual(rest.pill, rest.tab);
      assert.equal(rest.radius, "6px");
      await page.click('#d-tabs .tab[data-name="decision.md"]');
      assert.match(await page.$eval("#d-tab-pill", (el) => el.style.transition), /220ms/);
    });

    await t.test("S3", async () => {
      await open("spec");
      await drag(line(300, 150), 16);
      await settled();
      const s = await state();
      assert.equal(s.tab, "build.md");
      assert.ok(s.hash.endsWith("/build"), s.hash);
      assert.equal(s.h1, "Build");
      assert.equal(s.transform, "none");
      assert.equal(s.peeks, 0);
    });

    await t.test("S4", async () => {
      await open("spec");
      await drag(line(300, 260), 8);
      await settled();
      assert.equal((await state()).tab, "build.md");
      await drag(line(100, 140), 8);
      await settled();
      assert.equal((await state()).tab, "spec.md");
    });

    await t.test("S5", async () => {
      await open("spec");
      await drag(line(300, 250), 40, false);
      await sleep(200);
      await touch("touchEnd");
      await settled();
      const s = await state();
      assert.equal(s.tab, "spec.md");
      assert.equal(s.transform, "none");
    });

    await t.test("S6", async () => {
      await open("decision");
      await drag(line(300, 150), 16, false);
      assert.ok(Math.abs((await docX()) + 42) <= 1, `doc x ${await docX()}`);
      assert.equal((await state()).peeks, 0);
      await touch("touchEnd");
      await settled();
      assert.equal((await state()).tab, "decision.md");
      await open("spec");
      await drag(line(100, 250), 16, false);
      assert.ok(Math.abs((await docX()) - 42) <= 1, `doc x ${await docX()}`);
      assert.equal((await state()).peeks, 0);
      await touch("touchEnd");
      await settled();
      assert.equal((await state()).tab, "spec.md");
    });

    await t.test("S7", async () => {
      await open("spec");
      await drag(
        [
          [200, 400],
          [205, 404],
          [210, 408],
          [214, 413],
          [250, 415],
          [300, 415],
        ],
        16,
      );
      await unchanged("spec.md");
    });

    await t.test("S8", async () => {
      await open("spec");
      await drag(line(10, 160), 16);
      await unchanged("spec.md");
      await drag(line(380, 230), 16);
      await unchanged("spec.md");
    });

    await t.test("S9", async () => {
      await open("spec");
      const r = await page.$eval("#d-doc .table-wrap", (el) => ({
        ...el.getBoundingClientRect().toJSON(),
        overflows: el.scrollWidth > el.clientWidth,
      }));
      assert.equal(r.overflows, true);
      const y = Math.round(r.top + r.height / 2);
      await drag(line(300, 150, y), 16);
      await unchanged("spec.md");
    });

    await t.test("S10", async () => {
      await open("spec");
      for (const points of [line(300, 150), line(300, 150), line(100, 250)]) {
        await drag(points, 16);
        await settled();
      }
      const s = await page.evaluate(() => {
        const doc = document.getElementById("d-doc");
        doc.scrollTop = 300;
        return {
          tab: document.querySelector(".tab.active").dataset.name,
          transform: getComputedStyle(doc).transform,
          hit: doc.contains(document.elementFromPoint(195, 600)),
          scrollTop: doc.scrollTop,
        };
      });
      assert.equal(s.tab, "build.md");
      assert.equal(s.transform, "none");
      assert.equal(s.hit, true);
      assert.ok(s.scrollTop > 0, String(s.scrollTop));
    });

    await t.test("S12 rotate", async () => {
      await open("build");
      await page.setViewport({ ...MOBILE, width: 844, height: 390 });
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
      const s = await page.evaluate(() => ({
        pill: parseFloat(document.getElementById("d-tab-pill").style.left),
        tab: document.querySelector("#d-tabs .tab.active").offsetLeft,
      }));
      assert.equal(s.pill, s.tab);
    });

    await t.test("S11", async () => {
      await open("spec", { width: 1280, height: 900, hasTouch: true });
      // The S1 gesture, shifted into the drawer, which starts right of the board at this width.
      await drag(line(1100, 950), 16);
      await unchanged("spec.md");
    });

    assert.deepEqual(errors, []);
  });
});
