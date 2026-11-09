// The factory walkthrough template, filled with the queue-batching fixture, runs in the
// build tab's sandboxed frame: header, Today and Proposed map, node details, stage walk,
// step links, each visual kind, the two-pane layout and dark theme, with no network.
// node --test.
import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const MAP_HTML = await readFile(
  new URL("../fixtures/walkthrough-map.html", import.meta.url),
  "utf8",
);

const captureDir = process.env.FACTORY_CAPTURE_DIR;
if (captureDir) await mkdir(captureDir, { recursive: true });

// Chrome can swap the frame right after it attaches, so poll for a live one that rendered.
async function mapFrame(page) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    for (const f of page.frames()) {
      if (f.detached || !f.url().split("#")[0].endsWith("/build.walkthrough.html")) continue;
      const title = await f.$eval("#doc-title", (t) => t.textContent).catch(() => "");
      if (title === "Batch ticket writes") return f;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("no live walkthrough frame rendered within 30s");
}

const setTheme = (page, value) =>
  page.evaluate((v) => {
    const select = document.getElementById("display-theme");
    select.value = v;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  }, value);

async function waitBg(page, want) {
  const deadline = Date.now() + 10000;
  let got;
  while (Date.now() < deadline) {
    const frame = await mapFrame(page);
    got = await frame
      .evaluate(() => getComputedStyle(document.body).backgroundColor)
      .catch(() => "");
    if (got === want) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`frame body stayed ${got}, wanted ${want}`);
}
const frameUrl = async (page) => (await mapFrame(page)).url();
const historyLength = (page) => page.evaluate(() => history.length);

const visibleNodes = () =>
  [...document.querySelectorAll("g.node")]
    .filter((g) => getComputedStyle(g).display !== "none")
    .map((g) => g.dataset.id);
const highlighted = () => [...document.querySelectorAll("g.node.hl")].map((g) => g.dataset.id);
// Rendered label size, box height and scroll state of the map, and whether the page fits.
const mapSizes = () => {
  const map = document.getElementById("map");
  const scroller = map.parentElement;
  const scale = map.getBoundingClientRect().width / map.viewBox.baseVal.width;
  const node = document.querySelector('g.node[data-id="kiosk"]');
  return {
    label: parseFloat(getComputedStyle(node.querySelector("text")).fontSize) * scale,
    box: node.querySelector("rect").getBoundingClientRect().height,
    fits: scroller.scrollWidth <= scroller.clientWidth,
    pageFits: document.documentElement.scrollWidth <= window.innerWidth,
  };
};
// Rendered label sizes of the first svg.v, whether its parent scrolls, the pair's column count.
const vSizes = () => {
  const v = document.querySelector("#vis svg.v");
  const scale = v.getBoundingClientRect().width / v.viewBox.baseVal.width;
  const size = (q) => parseFloat(getComputedStyle(v.querySelector(q)).fontSize) * scale;
  return {
    label: size("text"),
    sub: size("text.sub"),
    scrolls: v.parentElement.scrollWidth > v.parentElement.clientWidth,
    pairCols: getComputedStyle(document.querySelector("#vis .pair")).gridTemplateColumns.split(" ")
      .length,
    pageFits: document.documentElement.scrollWidth <= window.innerWidth,
  };
};
const columns = (q) =>
  getComputedStyle(document.querySelector(q)).gridTemplateColumns.split(" ").length;

test("the walkthrough template draws the map, walks stages and renders each kind", async () => {
  assert.ok(Buffer.byteLength(MAP_HTML) < 1048576);
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      {
        id: "001-walkthrough",
        project: "sample",
        stage: "build",
        title: "Synthetic map",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    for (const [path, body, type] of [
      ["001-walkthrough/build.md", "# Build\n\nSynthetic map.\n", "text/markdown"],
      ["001-walkthrough/build.walkthrough.html", MAP_HTML, "text/html"],
    ]) {
      const put = await fetch(origin + "/api/jobs/sample/" + path, {
        method: "PUT",
        headers: { "Content-Type": type },
        body,
      });
      assert.equal(put.status, 200, path);
    }

    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
    const errors = [],
      external = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.setRequestInterception(true);
    page.on("request", (request) => {
      if (new URL(request.url()).origin !== origin) {
        external.push(request.url());
        return request.abort();
      }
      return request.continue();
    });
    try {
      await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
      await page.waitForSelector('.card[data-id="001-walkthrough"]');
      await clickNow(page, '.card[data-id="001-walkthrough"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Build",
      );
      await page.click("#d-doc .html-stepper-bar button:last-child");
      const frame = await mapFrame(page);

      // 1. Header and tabs.
      assert.deepEqual(
        await frame.evaluate(() => ({
          title: document.getElementById("doc-title").textContent,
          meta: document.querySelectorAll("#doc-meta dt").length,
          tabs: document.getElementById("tabs").checkVisibility(),
          tabCount: document.querySelectorAll('#tabs [role="tab"]').length,
        })),
        { title: "Batch ticket writes", meta: 2, tabs: true, tabCount: 2 },
      );

      // 2. Proposed by default.
      assert.deepEqual(await frame.evaluate(visibleNodes), [
        "kiosk",
        "api",
        "batcher",
        "store",
        "counter",
        "display",
      ]);
      assert.equal(
        await frame.$eval('[data-variant="proposed"]', (b) => b.getAttribute("aria-pressed")),
        "true",
      );

      // 3. Today swaps the batcher for the row writer.
      await frame.click('[data-variant="today"]');
      assert.deepEqual(await frame.evaluate(visibleNodes), [
        "kiosk",
        "api",
        "writer",
        "store",
        "counter",
        "display",
      ]);
      assert.equal(
        await frame.$eval("#map", (m) => m.getAttribute("aria-label")),
        "System map, Today",
      );

      // 4. Tab from the toolbar reaches the nodes in map order with a focus ring, and Enter
      // opens a node's details.
      await frame.click("#stage-reset");
      await page.keyboard.press("Tab");
      assert.deepEqual(
        await frame.evaluate(() => ({
          id: document.activeElement.dataset.id,
          stroke: getComputedStyle(document.activeElement.querySelector("rect")).strokeWidth,
        })),
        { id: "kiosk", stroke: "3px" },
      );
      await page.keyboard.press("Enter");
      assert.equal(await frame.$eval("#detail-title", (t) => t.textContent), "Kiosk K2");
      const order = [await frame.evaluate(() => document.activeElement.dataset.id)];
      for (let k = 0; k < 5; k++) {
        await page.keyboard.press("Tab");
        order.push(await frame.evaluate(() => document.activeElement.dataset.id));
      }
      assert.deepEqual(order, await frame.evaluate(visibleNodes));

      // 5. The stage walk wraps after the last stage, and Reset clears it.
      for (let k = 0; k < 3; k++) await frame.click("#stage-next");
      assert.equal(
        await frame.$eval("#stage-status", (s) => s.textContent),
        "Stage 3 of 3: Re-count and show",
      );
      assert.deepEqual(await frame.evaluate(highlighted), ["counter", "display"]);
      await frame.click("#stage-next");
      assert.equal(
        await frame.$eval("#stage-status", (s) => s.textContent),
        "Stage 1 of 3: Take the ticket",
      );
      await frame.click("#stage-reset");
      assert.deepEqual(await frame.evaluate(highlighted), []);
      assert.equal(await frame.$eval("#stage-status", (s) => s.textContent), "");
      // A variant switch also resets the walk, so the next stage starts at 1.
      for (let k = 0; k < 2; k++) await frame.click("#stage-next");
      await frame.click('[data-variant="today"]');
      assert.equal(await frame.$eval("#stage-status", (s) => s.textContent), "");
      await frame.click("#stage-next");
      assert.equal(
        await frame.$eval("#stage-status", (s) => s.textContent),
        "Stage 1 of 3: Take the ticket",
      );
      await frame.click("#stage-reset");

      // 6. A step's Show in diagram switches variant, highlights and focuses its node.
      assert.equal(await frame.$eval("#show-map", (b) => b.checkVisibility()), true);
      await frame.click("#show-map");
      assert.deepEqual(
        await frame.evaluate(() => ({
          today: document.querySelector('[data-variant="today"]').getAttribute("aria-pressed"),
          hl: [...document.querySelectorAll("g.node.hl")].map((g) => g.dataset.id),
          active: document.activeElement.dataset.id,
        })),
        { today: "true", hl: ["writer"], active: "writer" },
      );

      // 7. Each visual kind.
      assert.equal(await frame.$eval("#vis", (v) => !!v.querySelector("svg")), true);
      await frame.click("#next");
      assert.deepEqual(
        await frame.evaluate(() => ({
          bad: document.querySelector("#vis table td.t-bad")?.textContent,
          ok: document.querySelector("#vis table td.t-ok")?.textContent,
        })),
        { bad: "mismatch", ok: "match" },
      );
      await frame.click('#tabs [role="tab"]:nth-child(2)');
      assert.deepEqual(
        await frame.evaluate(() => ({
          cells: document.querySelectorAll("#vis .heat > div").length,
          changed: document.querySelectorAll("#vis .heat > .t-ex").length,
          kicker: document.getElementById("kicker").textContent,
        })),
        { cells: 24, changed: 3, kicker: "Step 1 of 3" },
      );
      await frame.click("#next");
      assert.deepEqual(
        await frame.evaluate(() => ({
          cards: document.querySelectorAll("#vis .card").length,
          measured: document.getElementById("measured").checkVisibility(),
        })),
        { cards: 3, measured: true },
      );
      await frame.click("#next");
      assert.equal(await frame.$eval("#vis", (v) => !!v.querySelector("svg.v")), true);

      // 8. The running example is marked as made up.
      assert.ok(
        (await frame.$eval("#example", (e) => e.textContent)).startsWith("Made-up example"),
      );

      // 9. The whole map shows without sideways scroll in the 1440 frame, collapsed and
      // expanded. The step has two panes when wide (expanded at 1440), one column at 390.
      await frame.click('#tabs [role="tab"]:nth-child(1)');
      const collapsed = await frame.evaluate(mapSizes);
      assert.ok(collapsed.fits && collapsed.label >= 10, JSON.stringify(collapsed));
      await page.click("#d-doc .html-stepper-bar button:first-of-type");
      await frame.waitForFunction(() => window.innerWidth > 720);
      assert.equal(await frame.evaluate(columns, ".step"), 2);
      const expanded = await frame.evaluate(mapSizes);
      assert.ok(expanded.fits && expanded.label >= 10, JSON.stringify(expanded));
      // Wide, the diagram does not scroll and the pair keeps two columns.
      await frame.click('#tabs [role="tab"]:nth-child(2)');
      await frame.click("#next");
      await frame.click("#next");
      const wide = await frame.evaluate(vSizes);
      assert.deepEqual([wide.scrolls, wide.pairCols], [false, 2], JSON.stringify(wide));
      await frame.click('#tabs [role="tab"]:nth-child(1)');
      await page.click("#d-doc .html-stepper-bar button:first-of-type");
      await page.setViewport({ width: 390, height: 844, deviceScaleFactor: 1 });
      await frame.waitForFunction(() => window.innerWidth <= 720);
      assert.deepEqual(
        [await frame.evaluate(columns, ".step"), await frame.evaluate(columns, ".map-wrap")],
        [1, 1],
      );
      // At 390 the map scrolls sideways instead of shrinking, so labels stay readable.
      const sizes = await frame.evaluate(mapSizes);
      assert.equal(sizes.fits, false, "the map scrolls inside its own box");
      assert.ok(sizes.label >= 10, JSON.stringify(sizes));
      assert.ok(sizes.box >= 32, JSON.stringify(sizes));
      assert.ok(sizes.pageFits, "only the map scrolls sideways, not the page");
      if (captureDir) {
        await page.evaluate(() => document.querySelector("#d-doc .html-stepper").scrollIntoView());
        await page.screenshot({ path: join(captureDir, "map-mobile.png") });
      }

      // At 375 the diagram keeps its size and scrolls, and the pair stacks.
      await page.setViewport({ width: 375, height: 812, deviceScaleFactor: 1 });
      await frame.click('#tabs [role="tab"]:nth-child(2)');
      await frame.click("#next");
      await frame.click("#next");
      await frame.waitForFunction(() => window.innerWidth <= 720);
      const narrow = await frame.evaluate(vSizes);
      assert.ok(narrow.label >= 12 && narrow.sub >= 10, JSON.stringify(narrow));
      assert.ok(narrow.scrolls, "the diagram scrolls inside its parent");
      assert.equal(narrow.pairCols, 1);
      assert.ok(narrow.pageFits, "only the diagram scrolls sideways, not the page");
      if (captureDir) await page.screenshot({ path: join(captureDir, "vis-mobile.png") });

      // 10. The frame follows a dark color scheme.
      await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
      const frameBg = await frame.evaluate(() => getComputedStyle(document.body).backgroundColor);
      assert.equal(frameBg, "rgb(31, 41, 55)");
      assert.equal(
        frameBg,
        await page.$eval("#detail", (d) => getComputedStyle(d).backgroundColor),
        "the dark frame matches the drawer",
      );
      if (captureDir) await page.screenshot({ path: join(captureDir, "map-dark.png") });

      // 11. A forced Display theme reaches the frame.
      assert.ok((await frameUrl(page)).endsWith("#theme=system"));
      const entries = await historyLength(page);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      await setTheme(page, "dark");
      await waitBg(page, "rgb(31, 41, 55)");
      assert.ok((await frameUrl(page)).endsWith("#theme=dark"));
      if (captureDir) await page.screenshot({ path: join(captureDir, "map-forced-dark.png") });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
      await setTheme(page, "light");
      await waitBg(page, "rgb(255, 255, 255)");
      assert.ok((await frameUrl(page)).endsWith("#theme=light"));
      await setTheme(page, "system");
      await waitBg(page, "rgb(31, 41, 55)");
      assert.ok((await frameUrl(page)).endsWith("#theme=system"));
      assert.equal(await historyLength(page), entries, "theme changes add no history entry");

      // 12. Error free and offline.
      assert.deepEqual(errors, []);
      assert.deepEqual(external, [], "no external request was attempted");
    } finally {
      await context.close();
    }
  });
});
