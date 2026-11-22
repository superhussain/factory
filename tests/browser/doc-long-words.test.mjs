import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const captureDir = process.env.FACTORY_CAPTURE_DIR;
const HASH = "a1b2c3d4".repeat(8);
const spec = [
  "# Long words spec",
  "",
  `Head: ${HASH}`,
  "",
  `- Checks: \`origin/main:.factory/checks.json\` at ${HASH}`,
  `- https://example.com/${"segment-".repeat(12)}`,
  "",
  "| Check | Hash |",
  "| --- | --- |",
  `| lint | ${HASH} |`,
  "",
  "~~~text",
  "worker/push.js:12:7: " + "x".repeat(180),
  "~~~",
  "",
].join("\n");

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(origin, [
    {
      id: "045-long-words",
      project: "sample",
      title: "Synthetic job with long words",
      stage: "review",
      updated: "2026-01-01T00:00:00Z",
    },
  ]);
  const response = await fetch(origin + "/api/jobs/sample/045-long-words/spec.md", {
    method: "PUT",
    headers: { "If-None-Match": "*", "Content-Type": "text/markdown" },
    body: spec,
  });
  assert.equal(response.status, 200);
}

test(
  "long words and code lines stay inside the document pane on mobile",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    if (captureDir) await mkdir(captureDir, { recursive: true });
    await withBrowserFixture(async ({ browser, origin }) => {
      await seed(origin);
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport({ width: 390, height: 844, deviceScaleFactor: 1 });
        await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
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
        await page.goto(origin + "/#045-long-words", { waitUntil: "domcontentloaded" });
        await page.waitForFunction(
          () =>
            !document.getElementById("detail").hidden &&
            document.querySelector("#d-doc pre") &&
            document.getElementById("d-doc").textContent.includes("Long words spec"),
        );
        const overflowing = await page.$eval("#d-doc", (doc) => {
          const edge =
            doc.getBoundingClientRect().right - parseFloat(getComputedStyle(doc).paddingRight) + 1;
          return [...doc.querySelectorAll("p, li, h1, h2, a, code, .table-wrap, pre")]
            .filter((el) => !el.parentElement.closest(".table-wrap, pre"))
            .filter((el) => el.getBoundingClientRect().right > edge)
            .map((el) => el.tagName.toLowerCase() + (el.className ? "." + el.className : ""));
        });
        assert.deepEqual(overflowing, [], "elements past the content edge");
        assert.ok(
          await page.$eval("#d-doc", (doc) => doc.scrollWidth <= doc.clientWidth),
          "#d-doc has no sideways scroll",
        );
        assert.ok(
          await page.$eval("#d-doc pre", (pre) => pre.scrollWidth > pre.clientWidth),
          "the code block scrolls inside itself",
        );
        if (captureDir) {
          await page.evaluate(() => document.fonts.ready);
          await page.screenshot({ path: join(captureDir, "doc-long-words-mobile.png") });
        }
        assert.deepEqual(external, [], "no external requests");
        assert.deepEqual(errors, [], "no page errors");
      } finally {
        await context.close();
      }
    });
  },
);
