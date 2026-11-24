import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const baseline = process.env.FACTORY_CAPTURE_BASELINE === "1";
const captureDir = process.env.FACTORY_CAPTURE_DIR;
const documents = {
  "042-sample-export": {
    "spec.md": "# Sample export spec\n\nThe synthetic export specification.\n",
    "build.md": "# Build\n\nThe synthetic build log.\n",
    "pre-review.md": [
      "PRE-REVIEW: FAIL",
      "",
      "Head: 2222222222222222222222222222222222222222",
      "Checks: origin/main:.factory/checks.json at 3333333333333333333333333333333333333333",
      "Logs: /tmp/factory-sample-042-sample-export/pre-review",
      "",
      "| Check | Exit | Seconds |",
      "| --- | --- | --- |",
      "| lint | 1 | 4 |",
      "| format | 0 | 2 |",
      "| cli | 0 | 31 |",
      "",
      "## lint",
      "",
      "~~~text",
      "worker/push.js:12:7: 'unused' is assigned a value but never used",
      "~~~",
      "",
    ].join("\n"),
  },
  "043-no-checks": {
    "spec.md": "# No checks spec\n\nThe synthetic job without checks.\n",
    "pre-review.md":
      "PRE-REVIEW: NONE\n\nHead: 1111111111111111111111111111111111111111\nReason: origin/main has no .factory/checks.json\n",
  },
  "044-no-gate": { "spec.md": "# No gate spec\n\nThe synthetic job without a gate.\n" },
};

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(
    origin,
    [
      ["042-sample-export", "Synthetic sample export"],
      ["043-no-checks", "Synthetic job without checks"],
      ["044-no-gate", "Synthetic job without a gate"],
    ].map(([id, title]) => ({
      id,
      project: "sample",
      title,
      stage: "review",
      updated: "2026-01-01T00:00:00Z",
    })),
  );
  for (const [id, files] of Object.entries(documents)) {
    for (const [name, body] of Object.entries(files)) {
      const response = await fetch(origin + `/api/jobs/sample/${id}/${name}`, {
        method: "PUT",
        headers: { "If-None-Match": "*", "Content-Type": "text/markdown" },
        body,
      });
      assert.equal(response.status, 200);
    }
  }
}

test(
  "pre-review.md shows as a pre-review tab with its status chip",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    if (captureDir) await mkdir(captureDir, { recursive: true });
    await withBrowserFixture(async ({ browser, origin }) => {
      await seed(origin);
      for (const [view, width, height, colorScheme] of [
        ["desktop", 1440, 900, "light"],
        ["mobile", 390, 844, "dark"],
      ]) {
        const context = await browser.createBrowserContext();
        try {
          const page = await context.newPage();
          await page.setViewport({ width, height, deviceScaleFactor: 1 });
          await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: colorScheme }]);
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
          await page.goto(origin + "/#042-sample-export/pre-review", {
            waitUntil: "domcontentloaded",
          });
          await page.waitForFunction(
            () =>
              !document.getElementById("detail").hidden &&
              document.querySelector("#d-tabs .tab.active") &&
              document.getElementById("d-doc").textContent.trim(),
          );
          if (!baseline) {
            await page.waitForFunction(() => {
              const tab = document.querySelector("#d-tabs .tab.active");
              return (
                tab?.dataset.name === "pre-review.md" &&
                document.getElementById("d-doc").textContent.includes("PRE-REVIEW: FAIL")
              );
            });
            assert.equal(
              await page.$eval("#d-tabs .tab.active", (tab) => tab.textContent.trim()),
              "pre-review FAIL",
            );
            assert.ok(
              await page.$eval("#d-tabs .tab.active .chip", (chip) =>
                chip.classList.contains("CHANGES"),
              ),
            );
            assert.deepEqual(
              await page.$$eval("#d-tabs .tab", (tabs) => tabs.map((t) => t.dataset.name)),
              ["spec.md", "build.md", "pre-review.md"],
            );
            assert.equal(new URL(page.url()).hash, "#042-sample-export/pre-review");
          }
          if (captureDir) {
            await page.evaluate(() => document.fonts.ready);
            await page.screenshot({ path: join(captureDir, `pre-review-tab-${view}.png`) });
          }
          if (!baseline) {
            await page.goto(origin + "/#043-no-checks/pre-review", {
              waitUntil: "domcontentloaded",
            });
            await page.waitForFunction(
              () =>
                document.querySelector("#d-tabs .tab.active")?.dataset.name === "pre-review.md" &&
                document.getElementById("d-doc").textContent.includes("PRE-REVIEW: NONE"),
            );
            assert.equal(
              await page.$eval("#d-tabs .tab.active", (tab) => tab.textContent.trim()),
              "pre-review NONE",
            );
            assert.ok(
              await page.$eval("#d-tabs .tab.active .chip", (chip) =>
                chip.classList.contains("SKIPPED"),
              ),
            );
            await page.goto(origin + "/#044-no-gate", { waitUntil: "domcontentloaded" });
            await page.waitForFunction(
              () =>
                document.querySelector("#d-tabs .tab.active")?.dataset.name === "spec.md" &&
                document.getElementById("d-doc").textContent.includes("No gate spec"),
            );
            assert.equal(await page.$('#d-tabs .tab[data-name="pre-review.md"]'), null);
          }
          assert.deepEqual(external, [], "no external requests");
          assert.deepEqual(errors, [], "no page errors");
        } finally {
          await context.close();
        }
      }
    });
  },
);
