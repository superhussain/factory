// Offline bundled diagrams: the pinned Mermaid asset is served authenticated from the
// same origin, byte-identical to the inventoried build artifact, and renders nonempty
// flowchart and sequence SVGs with zero external request attempts. node --test.
import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const FLOW = "graph LR; Alpha-->Beta";
const SEQ = "sequenceDiagram; participant Auth; participant Board; Auth->>Board: signed hello";
const DOC = "# Diagrams\n\n```mermaid\n" + FLOW + "\n```\n\n```mermaid\n" + SEQ + "\n```\n";

test("same-origin Mermaid bytes render flowchart and sequence SVGs offline", async () => {
  await prepareAssets();
  const inventoried = await readFile(new URL("../../.build/mermaid.min.txt", import.meta.url));
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      {
        id: "060-diagrams",
        project: "sample",
        stage: "build",
        title: "Synthetic diagrams",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    const put = await fetch(origin + "/api/jobs/sample/060-diagrams/build.md", {
      method: "PUT",
      headers: { "Content-Type": "text/markdown" },
      body: DOC,
    });
    assert.equal(put.status, 200);

    // The served asset is byte-identical to the inventoried build output.
    const served = await fetch(origin + "/assets/mermaid.min.js");
    assert.equal(served.status, 200);
    assert.equal(served.headers.get("Content-Type"), "text/javascript; charset=utf-8");
    assert.equal(served.headers.get("Cache-Control"), "no-store");
    assert.equal(served.headers.get("X-Content-Type-Options"), "nosniff");
    assert.deepEqual(Buffer.from(await served.arrayBuffer()), inventoried);

    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
    const external = [],
      errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.setRequestInterception(true);
    page.on("request", (request) => {
      if (new URL(request.url()).origin !== origin) {
        external.push(request.url());
        return request.abort();
      }
      return request.continue();
    });
    // A failed bundle load leaves the blocks as code for good, so the wait below stops at once.
    await page.evaluateOnNewDocument(() => {
      window.addEventListener(
        "error",
        (event) => {
          if (event.target instanceof HTMLScriptElement)
            window.fixtureScriptError = event.target.src;
        },
        true,
      );
    });
    try {
      await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.waitForSelector('.card[data-id="060-diagrams"]');
      await clickNow(page, '.card[data-id="060-diagrams"]');
      // Three waits, so a timeout names the slow part. Fetching and compiling the 5.5 MB bundle
      // is most of the time, and it grows with CPU contention on a shared CI runner.
      await page.waitForFunction(
        () => document.querySelectorAll("#d-doc pre.mermaid-src, #d-doc .mermaid svg").length === 2,
        { timeout: 30000 },
      );
      await page.waitForFunction(
        () => typeof window.mermaid?.render === "function" || window.fixtureScriptError,
        { timeout: 90000 },
      );
      assert.equal(
        await page.evaluate(() => window.fixtureScriptError ?? null),
        null,
        "the Mermaid bundle loaded",
      );
      await page.waitForFunction(
        () => document.querySelectorAll("#d-doc .mermaid svg").length === 2,
        { timeout: 30000 },
      );
      const diagrams = await page.evaluate(() =>
        [...document.querySelectorAll("#d-doc .mermaid svg")].map((svg) => ({
          nodes: svg.querySelectorAll("*").length,
          text: svg.textContent,
          box: (() => {
            const r = svg.getBoundingClientRect();
            return { width: r.width, height: r.height };
          })(),
        })),
      );
      const [flow, seq] = diagrams;
      assert.ok(flow.nodes > 10, "flowchart SVG is nonempty");
      assert.ok(flow.box.width > 0 && flow.box.height > 0, "flowchart has geometry");
      assert.ok(
        flow.text.includes("Alpha") && flow.text.includes("Beta"),
        "flowchart keeps its node labels",
      );
      assert.ok(seq.nodes > 10, "sequence SVG is nonempty");
      assert.ok(seq.box.width > 0 && seq.box.height > 0, "sequence diagram has geometry");
      assert.ok(
        seq.text.includes("Auth") && seq.text.includes("Board"),
        "sequence keeps participants",
      );
      assert.ok(seq.text.includes("signed hello"), "sequence keeps the message label");
      assert.deepEqual(external, [], "no external request was attempted");
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  });
});
