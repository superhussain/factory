// A sideways swipe that starts inside a stepper moves one step on a phone, and one inside
// the open walkthrough frame does the same. A swipe elsewhere still flips the job file.
import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const paras = (n) =>
  Array.from({ length: n }, (_, i) => `Synthetic paragraph ${i + 1}.`).join("\n\n");
const WALK_HTML = await readFile(
  new URL("../fixtures/walkthrough-map.html", import.meta.url),
  "utf8",
);
const FILES = {
  "spec.md": `# Spec\n\n## Design\n\n#### 1. Today\n\n- Swipe flips the page.\n\n#### 2. The gap\n\n- The stepper loses the swipe.\n\n\`\`\`mermaid\nflowchart LR\nA[finger] --> B[step 2]\n\`\`\`\n\n#### 3. After\n\n- The swipe moves one step.\n\n## Notes\n\n${paras(40)}\n`,
  "build.md": "# Build\n",
  "walkthrough.html": WALK_HTML,
};
const MOBILE = { width: 390, height: 844, isMobile: true, hasTouch: true };
const wait = { polling: 50 };
const captureDir = process.env.FACTORY_CAPTURE_DIR;
if (captureDir) await mkdir(captureDir, { recursive: true });
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

test("stepper swipe", { timeout: 180000 }, async (t) => {
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
    const drag = async (points, stepMs) => {
      await touch("touchStart", points[0]);
      for (const point of points) {
        await touch("touchMove", point);
        await sleep(stepMs);
      }
      await touch("touchEnd");
    };
    const press = async (points, stepMs) => {
      await touch("touchStart", points[0]);
      for (const point of points) {
        await touch("touchMove", point);
        await sleep(stepMs);
      }
    };
    const settled = () =>
      page.waitForFunction(
        () => !swipeSettling && !document.querySelector(".doc-peek, .step-peek"),
        wait,
      );
    // The panel under the finger and the neighbour beside it, mid-drag.
    const panning = () =>
      page.evaluate(() => {
        const cur = document.querySelector("#d-doc .step-panel:not([hidden]):not(.step-peek)");
        const peek = document.querySelector("#d-doc .step-panel.step-peek");
        return {
          x: new DOMMatrix(getComputedStyle(cur).transform).m41,
          peek: peek?.querySelector(".step-kicker").textContent,
          side: peek
            ? Math.sign(peek.getBoundingClientRect().left - cur.getBoundingClientRect().left)
            : 0,
        };
      });
    const clean = () =>
      page.evaluate(() => ({
        peeks: document.querySelectorAll(".step-peek").length,
        panning: document.querySelectorAll(".step-panning").length,
        styled: document.querySelectorAll(
          "#d-doc .step-panel[style], #d-doc .stepper[style], #d-doc .step-nav[style]",
        ).length,
      }));
    // The Back and Next row against the peek and the box bottom, at rest or mid-drag.
    const navFit = () =>
      page.evaluate(() => {
        const box = document.querySelector("#d-doc .stepper");
        const nav = box.querySelector(".step-nav").getBoundingClientRect();
        const peek = box.querySelector(".step-panel.step-peek");
        const cur = box.querySelector(".step-panel:not([hidden]):not(.step-peek)");
        return {
          under: Math.round(box.getBoundingClientRect().bottom - nav.bottom),
          clipped: peek ? peek.scrollHeight > peek.clientHeight + 1 : false,
          taller: peek ? peek.offsetHeight > cur.offsetHeight : false,
          below: peek ? nav.top >= peek.getBoundingClientRect().bottom - 1 : true,
        };
      });
    const open = async () => {
      await page.setViewport(MOBILE);
      await page.goto("about:blank");
      await page.goto(`${origin}/#001-swipe/spec`, { waitUntil: "load" });
      await page.waitForFunction(
        () =>
          document.querySelector("#d-doc h1")?.textContent === "Spec" &&
          document.querySelector("#d-doc .stepper"),
        wait,
      );
    };
    const state = () =>
      page.evaluate(() => ({
        tab: document.querySelector(".tab.active")?.dataset.name,
        kicker: document.querySelector("#d-doc .step-panel:not([hidden]) .step-kicker")
          ?.textContent,
        peeks: document.querySelectorAll(".doc-peek").length,
      }));
    const touches = (type, points) =>
      cdp.send("Input.dispatchTouchEvent", {
        type,
        touchPoints: points.map(([x, y]) => ({ x, y })),
        timestamp: clock / 1000,
      });
    const openFrame = async () => {
      await page.click(".html-stepper-bar .btn");
      let frame;
      const deadline = Date.now() + 30000;
      while (!frame && Date.now() < deadline) {
        for (const f of page.frames()) {
          if (f.detached || !f.url().split("#")[0].endsWith("/walkthrough.html")) continue;
          const title = await f.$eval("#doc-title", (e) => e.textContent).catch(() => "");
          if (title === "Batch ticket writes") frame = f;
        }
        if (!frame) await new Promise((r) => setTimeout(r, 100));
      }
      assert.ok(frame, "no live walkthrough frame");
      const y = await frame.evaluate(() => {
        const el = document.getElementById("title");
        el.scrollIntoView({ block: "center" });
        const r = el.getBoundingClientRect();
        return Math.round(r.top + r.height / 2);
      });
      const top = await page.$eval(".html-stepper-frame", (el) => {
        el.scrollIntoView({ block: "start" });
        return el.getBoundingClientRect().top;
      });
      return { frame, y: Math.round(top + y) };
    };
    const frameSettled = (frame) =>
      frame.waitForFunction(() => !document.querySelector("#steps .peek, #steps.panning"), wait);
    const framePan = (frame) =>
      frame.evaluate(() => {
        const pane = document.getElementById("pane");
        const peek = document.querySelector("#steps .peek");
        return {
          x: new DOMMatrix(getComputedStyle(pane).transform).m41,
          peek: peek ? peek.querySelector(".muted").textContent : null,
          side: peek
            ? Math.sign(peek.getBoundingClientRect().left - pane.getBoundingClientRect().left)
            : 0,
        };
      });
    const frameClean = (frame) =>
      frame.evaluate(() => ({
        peeks: document.querySelectorAll("#steps .peek").length,
        panning: document.querySelectorAll("#steps.panning").length,
        styled: document.querySelectorAll("#pane[style], #steps[style], #steps .controls[style]")
          .length,
      }));
    // The same fit, for the walkthrough frame's Back and Next row.
    const frameFit = (frame) =>
      frame.evaluate(() => {
        const steps = document.getElementById("steps");
        const nav = steps.querySelector(".controls").getBoundingClientRect();
        const peek = steps.querySelector(".peek");
        const cur = document.getElementById("pane");
        return {
          under: Math.round(steps.getBoundingClientRect().bottom - nav.bottom),
          clipped: peek ? peek.scrollHeight > peek.clientHeight + 1 : false,
          taller: peek ? peek.offsetHeight > cur.offsetHeight : false,
          below: peek ? nav.top >= peek.getBoundingClientRect().bottom - 1 : true,
        };
      });
    const capture = (name) => captureDir && page.screenshot({ path: join(captureDir, name) });
    const stepY = () =>
      page.$eval("#d-doc .step-panel:not([hidden]) .step-title", (el) => {
        el.scrollIntoView({ block: "center" });
        const r = el.getBoundingClientRect();
        return Math.round(r.top + r.height / 2);
      });

    await t.test("T1 left swipe on a step shows step 2", async () => {
      await open();
      await drag(line(300, 150, await stepY()), 16);
      await settled();
      await capture("stepper-swipe.png");
      assert.deepEqual(await state(), { tab: "spec.md", kicker: "Step 2 of 3", peeks: 0 });
    });

    await t.test("T2 right swipe on step 1 wraps to step 3", async () => {
      await open();
      await drag(line(100, 250, await stepY()), 16);
      await settled();
      assert.equal((await state()).kicker, "Step 3 of 3");
    });

    await t.test("T3 vertical drag keeps step 1", async () => {
      await open();
      const y = await stepY();
      await drag(
        Array.from({ length: 8 }, (_, i) => [200 + i, y + i * 10]),
        16,
      );
      await settled();
      const s = await state();
      assert.equal(s.kicker, "Step 1 of 3");
      assert.equal(s.tab, "spec.md");
    });

    await t.test("T4 left swipe on a paragraph still opens build.md", async () => {
      await open();
      const y = await page.evaluate(() => {
        const p = [...document.querySelectorAll("#d-doc p")].find((e) =>
          e.textContent.startsWith("Synthetic paragraph 5."),
        );
        p.scrollIntoView({ block: "center" });
        const r = p.getBoundingClientRect();
        return Math.round(r.top + r.height / 2);
      });
      await drag(line(300, 150, y), 16);
      await settled();
      assert.equal((await state()).tab, "build.md");
    });

    await t.test("T5 left swipe in the frame shows frame step 2", async () => {
      await open();
      const { frame, y } = await openFrame();
      const kicker = () => frame.$eval("#kicker", (e) => e.textContent);
      const before = await kicker();
      await drag(line(300, 150, y), 16);
      await settled();
      await frameSettled(frame);
      await capture("frame-swipe.png");
      assert.match(before, /Step 1 of 2/);
      assert.match(await kicker(), /Step 2 of 2/);
      assert.equal((await state()).tab, "spec.md");
    });

    await t.test("T6 a drag that becomes a pinch keeps frame step 1", async () => {
      await open();
      const { frame, y } = await openFrame();
      // The second finger lands on #example, outside #steps, so its touchstart never
      // reaches the swipe. Only the touchmove second-finger guard can drop the swipe.
      const { left, top } = await page.$eval(".html-stepper-frame", (el) => {
        const r = el.getBoundingClientRect();
        return { left: r.left, top: r.top };
      });
      const { dy, outside } = await frame.evaluate(
        ([fx, fy]) => {
          const mid = (id) => {
            const r = document.getElementById(id).getBoundingClientRect();
            return r.top + r.height / 2;
          };
          const dy = Math.round(mid("example") - mid("title"));
          const hit = document.elementFromPoint(fx, fy + dy);
          return { dy, outside: Boolean(hit) && !hit.closest("#steps") };
        },
        [200 - left, y - top],
      );
      assert.ok(outside, "the second finger must land outside #steps");
      const second = [200, y + dy];
      for (const [type, points] of [
        ["touchStart", [[300, y]]],
        ["touchMove", [[270, y]]],
        ["touchMove", [[240, y]]],
        ["touchStart", [[240, y], second]],
        ["touchMove", [[150, y], second]],
        ["touchEnd", []],
      ]) {
        await touches(type, points);
        await sleep(16);
      }
      await settled();
      await frameSettled(frame);
      assert.match(await frame.$eval("#kicker", (e) => e.textContent), /Step 1 of 2/);
    });

    await t.test("T7 touchcancel drops the frame swipe, so a lone touchend stays put", async () => {
      await open();
      const { frame, y } = await openFrame();
      const kicker = () => frame.$eval("#kicker", (e) => e.textContent);
      await touches("touchStart", [[300, y]]);
      for (const point of line(300, 150, y)) {
        await touches("touchMove", [point]);
        await sleep(16);
      }
      await touches("touchCancel", []);
      await settled();
      await frameSettled(frame);
      await frame.evaluate(() =>
        document
          .getElementById("steps")
          .dispatchEvent(new TouchEvent("touchend", { bubbles: true })),
      );
      await settled();
      await frameSettled(frame);
      assert.match(await kicker(), /Step 1 of 2/);
      await drag(line(300, 150, y), 16);
      await settled();
      await frameSettled(frame);
      assert.match(await kicker(), /Step 2 of 2/);
    });

    await t.test("T8 swipe names stay out of the frame globals", async () => {
      await open();
      const { frame } = await openFrame();
      assert.deepEqual(
        await frame.evaluate(() => [
          typeof x0,
          typeof y0,
          typeof x,
          typeof swiping,
          typeof sideways,
          typeof pan,
          typeof settling,
          typeof peekAt,
        ]),
        [
          "undefined",
          "undefined",
          "undefined",
          "undefined",
          "undefined",
          "undefined",
          "undefined",
          "undefined",
        ],
      );
    });

    await t.test("T9 left drag pans step 2 in", async () => {
      await open();
      await press(line(300, 150, await stepY()), 16);
      await capture("stepper-mid-drag.png");
      const mid = await panning();
      assert.ok(mid.x < 0, `current panel x ${mid.x}`);
      assert.equal(mid.peek, "Step 2 of 3");
      assert.equal(mid.side, 1);
      await touch("touchEnd");
      await settled();
      assert.equal((await state()).kicker, "Step 2 of 3");
      assert.deepEqual(await clean(), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T10 short drag snaps back to step 1", async () => {
      await open();
      await drag(line(300, 260, await stepY()), 120);
      await settled();
      assert.equal((await state()).kicker, "Step 1 of 3");
      assert.equal(
        await page.$$eval("#d-doc .stepper .step-panel", (ps) => ps[1].getAttribute("hidden")),
        "until-found",
      );
      assert.deepEqual(await clean(), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T11 right drag on step 1 pans step 3 in", async () => {
      await open();
      await press(line(100, 250, await stepY()), 16);
      const mid = await panning();
      assert.ok(mid.x > 0, `current panel x ${mid.x}`);
      assert.equal(mid.peek, "Step 3 of 3");
      assert.equal(mid.side, -1);
      await touch("touchEnd");
      await settled();
      assert.equal((await state()).kicker, "Step 3 of 3");
      assert.deepEqual(await clean(), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T12 touchcancel snaps back to step 1", async () => {
      await open();
      await press(line(300, 150, await stepY()), 16);
      await touches("touchCancel", []);
      await settled();
      assert.equal((await state()).kicker, "Step 1 of 3");
      assert.deepEqual(await clean(), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T13 second finger outside the stepper snaps back to step 1", async () => {
      await open();
      const y = await stepY();
      const bottom = await page.$eval("#d-doc .stepper", (el) =>
        Math.round(el.getBoundingClientRect().bottom),
      );
      const second = [200, bottom + 40];
      assert.ok(
        await page.evaluate(([x, sy]) => {
          const hit = document.elementFromPoint(x, sy);
          return Boolean(hit) && !hit.closest(".stepper");
        }, second),
        "the second finger must land outside the stepper",
      );
      for (const [type, points] of [
        ["touchStart", [[300, y]]],
        ["touchMove", [[270, y]]],
        ["touchMove", [[240, y]]],
        ["touchMove", [[210, y]]],
        ["touchStart", [[210, y], second]],
        ["touchMove", [[150, y], second]],
        ["touchEnd", []],
      ]) {
        await touches(type, points);
        await sleep(16);
      }
      await settled();
      assert.equal((await state()).kicker, "Step 1 of 3");
      assert.deepEqual(await clean(), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T14 mermaid in step 2 pans in mid-drag", async () => {
      await open();
      await page.waitForSelector("#d-doc .step-panel div.mermaid svg");
      const rest = await navFit();
      await press(line(300, 150, await stepY()), 16);
      assert.ok(
        await page.$("#d-doc .step-panel.step-peek div.mermaid svg"),
        "the peek must hold the rendered diagram",
      );
      const fit = await navFit();
      assert.deepEqual(
        { clipped: fit.clipped, taller: fit.taller, below: fit.below },
        { clipped: false, taller: true, below: true },
      );
      assert.ok(Math.abs(fit.under - rest.under) <= 1, `under ${fit.under}, at rest ${rest.under}`);
      await capture("stepper-mermaid-mid-drag.png");
      await touch("touchEnd");
      await settled();
      assert.deepEqual(await clean(), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T15 frame left drag pans step 2 in", async () => {
      await open();
      const { frame, y } = await openFrame();
      const rest = await frameFit(frame);
      await press(line(300, 150, y), 16);
      await capture("frame-mid-drag.png");
      const mid = await framePan(frame);
      assert.ok(mid.x < 0, `pane x ${mid.x}`);
      assert.equal(mid.peek, "Step 2 of 2");
      assert.equal(mid.side, 1);
      const fit = await frameFit(frame);
      assert.deepEqual(
        { clipped: fit.clipped, taller: fit.taller, below: fit.below },
        { clipped: false, taller: true, below: true },
      );
      assert.ok(Math.abs(fit.under - rest.under) <= 1, `under ${fit.under}, at rest ${rest.under}`);
      await touch("touchEnd");
      await frameSettled(frame);
      assert.match(await frame.$eval("#kicker", (e) => e.textContent), /Step 2 of 2/);
      assert.deepEqual(await frameClean(frame), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T16 frame short drag snaps back to step 1", async () => {
      await open();
      const { frame, y } = await openFrame();
      await drag(line(300, 260, y), 120);
      await frameSettled(frame);
      assert.match(await frame.$eval("#kicker", (e) => e.textContent), /Step 1 of 2/);
      assert.deepEqual(await frameClean(frame), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T17 frame right drag on step 1 pans step 2 in from the left", async () => {
      await open();
      const { frame, y } = await openFrame();
      await press(line(100, 250, y), 16);
      const mid = await framePan(frame);
      assert.ok(mid.x > 0, `pane x ${mid.x}`);
      assert.equal(mid.peek, "Step 2 of 2");
      assert.equal(mid.side, -1);
      await touch("touchEnd");
      await frameSettled(frame);
      assert.match(await frame.$eval("#kicker", (e) => e.textContent), /Step 2 of 2/);
      assert.deepEqual(await frameClean(frame), { peeks: 0, panning: 0, styled: 0 });
    });

    await t.test("T18 a pill tap during the frame settle keeps the reader's step", async () => {
      await open();
      const { frame, y } = await openFrame();
      // The three-step tab, so the tapped step differs from both the start and the pan target.
      await frame.evaluate(() => document.querySelectorAll("#tabs button")[1].click());
      assert.match(await frame.$eval("#kicker", (e) => e.textContent), /Step 1 of 3/);
      await press(line(300, 150, y), 16);
      await touch("touchEnd");
      await frame.evaluate(() => document.querySelectorAll("#pills button")[2].click());
      await frameSettled(frame);
      assert.match(await frame.$eval("#kicker", (e) => e.textContent), /Step 3 of 3/);
      assert.deepEqual(await frameClean(frame), { peeks: 0, panning: 0, styled: 0 });
    });

    assert.deepEqual(errors, []);
  });
});
