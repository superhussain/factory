// A job's walkthrough.html shows as a bar above the spec stepper, and build.walkthrough.html on
// the build tab. Each loads, only on Show, in a sandboxed frame with an opaque origin and no
// network. "### Step N. Title" runs also become steppers, and an arrow from a link in a step
// hands focus to the new pill. node --test.
import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const SPEC_DOC = `# Spec: push keeps the walkthrough

Synthetic walkthrough fixture.

## Design

#### 1. Today

| File | Pushed |
| --- | --- |
| \`spec.md\` | yes |
| \`walkthrough.html\` | no |

- See [the shell](https://example.com/shell).

#### 2. The gap

- \`JOB_REL\` drops \`walkthrough.html\`.

#### 3. After

- \`walkthrough.html\` syncs and shows sandboxed.
`;

const STEPTHRU_DOC = `# Walkthrough

### Step 1. The request

- One.

### Step 2. The fix

- Two.

### Step 3. Before and after

- Three.
`;

const MIXED_DOC = `# Build

#### 1. Four

- A.

### Step 2. Three

- B.
`;

const WALK_HTML = await readFile(
  new URL("../fixtures/walkthrough-rich.html", import.meta.url),
  "utf8",
);

const MAP_HTML = await readFile(
  new URL("../fixtures/walkthrough-map.html", import.meta.url),
  "utf8",
);

const CSP =
  "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; " +
  "style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; " +
  "frame-ancestors 'self'";

const PROBE = "origin:null cookie:blocked parent:blocked storage:blocked fetch:blocked";

const FRAME_ONLY = "Open this walkthrough from its job in the dashboard.";

const captureDir = process.env.FACTORY_CAPTURE_DIR;
if (captureDir) await mkdir(captureDir, { recursive: true });

const toggleState = () => {
  const b = document.querySelector("#d-doc .html-stepper-bar button:last-child");
  return {
    text: b.textContent,
    label: b.getAttribute("aria-label"),
    expanded: b.getAttribute("aria-expanded"),
  };
};

// Chrome can swap the walkthrough frame right after it attaches, so a handle from
// waitForFrame may go stale. Poll for a live frame whose probe has finished instead.
async function probeFrame(page) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    for (const f of page.frames()) {
      if (f.detached || !f.url().split("#")[0].endsWith("/walkthrough.html")) continue;
      const text = await f.$eval("#probe", (p) => p.textContent).catch(() => "");
      if (text.includes("fetch:")) return f;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("no live walkthrough frame showed the probe within 30s");
}

test("walkthrough.html embeds sandboxed above the stepper, and Step headings step", async () => {
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
        title: "Synthetic walkthrough",
        updated: "2026-01-01T00:00:00Z",
      },
      {
        id: "002-plain",
        project: "sample",
        stage: "build",
        title: "Synthetic plain",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    for (const [path, body, type] of [
      ["001-walkthrough/spec.md", SPEC_DOC, "text/markdown"],
      ["001-walkthrough/build.md", "# Build\n\nNo walkthrough here.\n", "text/markdown"],
      ["001-walkthrough/walkthrough.html", WALK_HTML, "text/html"],
      ["001-walkthrough/build.walkthrough.html", MAP_HTML, "text/html"],
      ["002-plain/spec.md", STEPTHRU_DOC, "text/markdown"],
      ["002-plain/build.md", MIXED_DOC, "text/markdown"],
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
    const external = [],
      errors = [],
      requests = [],
      walkResponses = [];
    // While hold is true, a walkthrough.html request waits in held, so the loading state shows.
    let hold = false,
      held = null;
    const watch = async (p) => {
      p.on("pageerror", (error) => errors.push(String(error)));
      await p.setRequestInterception(true);
      p.on("request", (request) => {
        requests.push(request.url());
        if (new URL(request.url()).origin !== origin) {
          external.push(request.url());
          return request.abort();
        }
        if (hold && request.url().split("#")[0].endsWith("/walkthrough.html")) {
          held = request;
          return;
        }
        return request.continue();
      });
    };
    await watch(page);
    page.on("response", (response) => {
      if (response.url().split("#")[0].endsWith("/walkthrough.html")) walkResponses.push(response);
    });
    try {
      await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      // 0. The frame loads under a controlling service worker.
      await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
      await page.waitForSelector('.card[data-id="001-walkthrough"]');
      await clickNow(page, '.card[data-id="001-walkthrough"]');
      await page.waitForSelector("#d-doc .stepper");
      await page.waitForSelector("#d-doc .html-stepper");

      // 1. One bar, right before the stepper, collapsed with no frame.
      const bar = await page.evaluate(() => ({
        bars: document.querySelectorAll("#d-doc .html-stepper").length,
        beforeStepper:
          document.querySelector("#d-doc .stepper").previousElementSibling ===
          document.querySelector("#d-doc .html-stepper"),
        name: document.querySelector("#d-doc .html-stepper-name").textContent,
        frames: document.querySelectorAll("#d-doc iframe").length,
      }));
      assert.deepEqual(bar, {
        bars: 1,
        beforeStepper: true,
        name: "Interactive walkthrough",
        frames: 0,
      });
      assert.deepEqual(await page.evaluate(toggleState), {
        text: "Show",
        label: "Show walkthrough",
        expanded: "false",
      });

      // 2. The bar holds the name and Show, and no link.
      assert.deepEqual(
        await page.evaluate(() =>
          [...document.querySelector("#d-doc .html-stepper-bar").children].map((c) => c.tagName),
        ),
        ["SPAN", "BUTTON"],
      );
      assert.equal(
        await page.evaluate(() => document.querySelectorAll("#d-doc .html-stepper-bar a").length),
        0,
      );

      // 3. Show says it is loading and hides the frame until it loads. Then the sandboxed
      // frame shows, and the probe finds every door shut.
      hold = true;
      await page.click("#d-doc .html-stepper-bar button:last-child");
      const until = Date.now() + 10000;
      while (!held) {
        if (Date.now() > until)
          throw new Error("the walkthrough.html request was not held within 10s");
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      assert.deepEqual(
        await page.evaluate(() => {
          const loading = document.querySelector("#d-doc .html-stepper-loading");
          return {
            text: loading.textContent,
            role: loading.getAttribute("role"),
            visibility: getComputedStyle(document.querySelector("#d-doc iframe")).visibility,
          };
        }),
        { text: "Loading walkthrough…", role: "status", visibility: "hidden" },
      );
      hold = false;
      held.continue();
      const frame = await probeFrame(page);
      assert.equal(
        await page.evaluate(() => document.querySelector("#d-doc iframe").getAttribute("sandbox")),
        "allow-scripts",
      );
      assert.deepEqual(await page.evaluate(toggleState), {
        text: "Hide",
        label: "Hide walkthrough",
        expanded: "true",
      });
      assert.deepEqual(
        await page.evaluate(() =>
          [...document.querySelector("#d-doc .html-stepper-bar").children].map((c) => c.tagName),
        ),
        ["SPAN", "BUTTON", "BUTTON"],
      );
      assert.equal(await frame.$eval("#probe", (p) => p.textContent), PROBE);
      assert.equal(await frame.$eval("#title", (t) => t.textContent), "Where the page opens");
      await page.waitForFunction(() => !document.querySelector("#d-doc .html-stepper-loading"));
      assert.equal(
        await page.evaluate(() =>
          document.querySelector("#d-doc iframe").classList.contains("loaded"),
        ),
        true,
      );
      assert.equal(
        await page.evaluate(
          () => document.querySelector("#d-doc .html-stepper-view").getBoundingClientRect().height,
        ),
        720,
      );

      // 4. The probe's fetch never left, and the page came with the sandbox CSP.
      assert.deepEqual(
        requests.filter((u) => u.includes("/api/probe-from-walkthrough")),
        [],
      );
      const served = walkResponses.find((r) => r.request().resourceType() === "document");
      assert.ok(served, "walkthrough.html document response recorded");
      assert.equal(served.headers()["content-security-policy"], CSP);
      assert.equal(served.headers()["content-type"], "text/html; charset=utf-8");

      // 5. The page's own script runs: Next moves to its step 2.
      await frame.click("#next");
      assert.equal(await frame.$eval("#title", (t) => t.textContent), "The Worker decides");

      // 5b. Every fixture step fits the 720px frame without scrolling.
      for (let k = 0; k < 4; k++) {
        await frame.click(`#pills button:nth-child(${k + 1})`);
        const height = await frame.evaluate(() => document.documentElement.scrollHeight);
        assert.ok(height <= 720, `step ${k + 1} is ${height}px tall`);
      }

      // 6. Hide drops the frame.
      await page.click("#d-doc .html-stepper-bar button:last-child");
      assert.equal(await page.evaluate(() => document.querySelectorAll("#d-doc iframe").length), 0);
      assert.equal((await page.evaluate(toggleState)).text, "Show");
      assert.equal((await page.evaluate(toggleState)).label, "Show walkthrough");

      // 7. build.md carries its own bar, and Show frames build.walkthrough.html sandboxed.
      await page.click('.tab[data-name="build.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Build",
      );
      assert.deepEqual(
        await page.evaluate(() =>
          [...document.querySelectorAll(".html-stepper-name")].map((n) => n.textContent),
        ),
        ["Interactive walkthrough"],
      );
      await page.click("#d-doc .html-stepper-bar button:last-child");
      await page.waitForSelector("#d-doc .html-stepper-frame.loaded");
      assert.deepEqual(
        await page.evaluate(() => {
          const frame = document.querySelector("#d-doc .html-stepper-frame");
          return {
            build: frame.src.split("#")[0].endsWith("/build.walkthrough.html"),
            sandbox: frame.getAttribute("sandbox"),
          };
        }),
        { build: true, sandbox: "allow-scripts" },
      );
      if (captureDir) {
        await page.evaluate(() => document.fonts.ready);
        await page.screenshot({ path: join(captureDir, "build-tab.png") });
      }

      // 7b. Expand fills the viewport less 12px a side without reloading the frame, and
      // Escape collapses it while the drawer stays open.
      await page.evaluate(() => {
        window.expandFrame = document.querySelector("#d-doc .html-stepper-frame");
      });
      await page.click("#d-doc .html-stepper-bar button:first-of-type");
      assert.deepEqual(
        await page.evaluate(() => {
          const box = document.querySelector("#d-doc .html-stepper");
          const expand = document.querySelector("#d-doc .html-stepper-bar button");
          const frame = document.querySelector("#d-doc .html-stepper-frame");
          const r = box.getBoundingClientRect();
          return {
            expanded: box.classList.contains("expanded"),
            rect: [r.x, r.y, r.width, r.height],
            text: expand.textContent,
            label: expand.getAttribute("aria-label"),
            pressed: expand.getAttribute("aria-pressed"),
            focused: document.activeElement === expand,
            sameFrame: frame === window.expandFrame,
            build: frame.src.split("#")[0].endsWith("/build.walkthrough.html"),
          };
        }),
        {
          expanded: true,
          rect: [12, 12, 1416, 876],
          text: "Collapse",
          label: "Collapse walkthrough",
          pressed: null,
          focused: true,
          sameFrame: true,
          build: true,
        },
      );
      if (captureDir) await page.screenshot({ path: join(captureDir, "expanded.png") });
      await page.keyboard.press("Escape");
      assert.deepEqual(
        await page.evaluate(() => {
          const expand = document.querySelector("#d-doc .html-stepper-bar button");
          return {
            expanded: document.querySelector("#d-doc .html-stepper").classList.contains("expanded"),
            drawer: !document.getElementById("detail").hidden,
            text: expand.textContent,
            label: expand.getAttribute("aria-label"),
            pressed: expand.getAttribute("aria-pressed"),
            focused: document.activeElement === expand,
            sameFrame: document.querySelector("#d-doc .html-stepper-frame") === window.expandFrame,
          };
        }),
        {
          expanded: false,
          drawer: true,
          text: "Expand",
          label: "Expand walkthrough",
          pressed: null,
          focused: true,
          sameFrame: true,
        },
      );

      // 8. The spec's bar keeps its own state, and ArrowRight from the link in step 1 moves
      // focus to pill 2, which shows a focus ring.
      await page.click('.tab[data-name="spec.md"]');
      await page.waitForSelector("#d-doc .stepper");
      assert.equal((await page.evaluate(toggleState)).text, "Show");
      await page.focus("#d-doc .step-panel a");
      await page.keyboard.press("ArrowRight");
      const focus = await page.evaluate(() => {
        const el = document.activeElement;
        const style = getComputedStyle(el);
        return {
          pill2: el === document.querySelectorAll("#d-doc .step-pill")[1],
          current: el.getAttribute("aria-current"),
          outlineStyle: style.outlineStyle,
          outlineWidth: style.outlineWidth,
        };
      });
      assert.deepEqual(focus, {
        pill2: true,
        current: "step",
        outlineStyle: "solid",
        outlineWidth: "2px",
      });

      // 9. A job with no walkthrough.html: no bar, and Step N. headings become a stepper.
      // Open through the hash, as the app does. A direct openJob leaves the old id in the hash,
      // so a poll landing during the load reopens the old job and this wait never ends.
      await page.evaluate(() => {
        location.hash = "002-plain";
      });
      await page.waitForFunction(
        () =>
          document.querySelector("#d-doc .step-pill")?.getAttribute("aria-label") ===
          "Step 1: The request",
      );
      const plain = await page.evaluate(() => ({
        bars: document.querySelectorAll(".html-stepper").length,
        steppers: document.querySelectorAll("#d-doc .stepper").length,
        labels: [...document.querySelectorAll("#d-doc .step-pill")].map((b) =>
          b.getAttribute("aria-label"),
        ),
        title: [...document.querySelectorAll("#d-doc .step-title")].find((h) => h.checkVisibility())
          ?.textContent,
      }));
      assert.deepEqual(plain, {
        bars: 0,
        steppers: 1,
        labels: ["Step 1: The request", "Step 2: The fix", "Step 3: Before and after"],
        title: "The request",
      });

      // 10. A run that mixes the two shapes stays plain.
      await page.click('.tab[data-name="build.md"]');
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Build",
      );
      const mixed = await page.evaluate(() => ({
        steppers: document.querySelectorAll("#d-doc .stepper").length,
        heads: [...document.querySelectorAll("#d-doc h3, #d-doc h4")].map((h) => h.tagName),
      }));
      assert.deepEqual(mixed, { steppers: 0, heads: ["H4", "H3"] });

      // 11. Opened as its own top-level page, the Worker refuses it, so its script never runs.
      const tab = await context.newPage();
      await watch(tab);
      const res = await tab.goto(origin + "/jobs/sample/001-walkthrough/walkthrough.html");
      assert.equal(res.status(), 403);
      assert.equal(await tab.$eval("p", (p) => p.textContent), FRAME_ONLY);
      assert.equal(await tab.$eval("a", (a) => a.getAttribute("href")), "/#001-walkthrough/spec");
      assert.equal(await tab.$("#probe"), null);
      assert.deepEqual(
        requests.filter((u) => u.includes("/api/probe-from-walkthrough")),
        [],
      );
      await tab.setViewport({ width: 1440, height: 900 });
      if (captureDir) await tab.screenshot({ path: join(captureDir, "403-tab.png") });
      await Promise.all([tab.waitForNavigation(), tab.click("a")]);
      await tab.waitForSelector("#d-doc .html-stepper");
      assert.equal(await tab.evaluate(() => location.hash), "#001-walkthrough/spec");
      assert.equal(
        await tab.$eval("#d-doc .html-stepper-bar button", (b) => b.textContent),
        "Show",
      );
      if (captureDir) await tab.screenshot({ path: join(captureDir, "link-opens-job.png") });

      // 12. At 390px the bar stays on one row.
      await page.setViewport({ width: 390, height: 844, deviceScaleFactor: 1 });
      await page.evaluate(() => {
        location.hash = "001-walkthrough";
      });
      await page.waitForSelector("#d-doc .html-stepper");
      const centers = await page.evaluate(() =>
        ["#d-doc .html-stepper-name", "#d-doc .html-stepper-bar button"].map((q) => {
          const r = document.querySelector(q).getBoundingClientRect();
          return r.top + r.height / 2;
        }),
      );
      assert.ok(Math.abs(centers[0] - centers[1]) <= 2, JSON.stringify(centers));

      // 13. Offline and error free, on the dashboard and the tab.
      assert.deepEqual(errors, []);
      assert.deepEqual(external, [], "no external request was attempted");
    } finally {
      await context.close();
    }
  });
});
