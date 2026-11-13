import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const baseline = process.env.FACTORY_CAPTURE_BASELINE === "1";
const captureDir = process.env.FACTORY_CAPTURE_DIR;
const documents = {
  "042-pacing-check": {
    "spec.md": "# Pacing spec\n\nThe synthetic pacing specification.\n",
    "scratchpad.md": "# Pacing log\n\n## 2026-11-12 limit check\n\n- Synthetic pacing note.\n",
  },
  "043-no-notes": { "spec.md": "# No notes spec\n\nThe synthetic job without a scratchpad.\n" },
};

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(origin, [
    {
      id: "042-pacing-check",
      project: "sample",
      title: "Synthetic pacing check",
      stage: "review",
      updated: "2026-01-01T00:00:00Z",
    },
    {
      id: "043-no-notes",
      project: "sample",
      title: "Synthetic job without notes",
      stage: "review",
      updated: "2026-01-01T00:00:00Z",
    },
  ]);
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

test("scratchpad.md shows as its own tab and deep link", { timeout: 120000 }, async () => {
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
        await page.goto(origin + "/#042-pacing-check/scratchpad", {
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
              tab?.dataset.name === "scratchpad.md" &&
              document.getElementById("d-doc").textContent.includes("Pacing log")
            );
          });
          assert.equal(
            await page.$eval("#d-tabs .tab.active", (tab) => tab.textContent.trim()),
            "scratchpad",
          );
          assert.equal(new URL(page.url()).hash, "#042-pacing-check/scratchpad");
        }
        if (captureDir) {
          await page.evaluate(() => document.fonts.ready);
          await page.screenshot({ path: join(captureDir, `scratchpad-tab-${view}.png`) });
        }
        if (!baseline) {
          await page.goto(origin + "/#043-no-notes", { waitUntil: "domcontentloaded" });
          await page.waitForFunction(
            () =>
              document.querySelector("#d-tabs .tab.active")?.dataset.name === "spec.md" &&
              document.getElementById("d-doc").textContent.includes("No notes spec"),
          );
          assert.equal(await page.$('#d-tabs .tab[data-name="scratchpad.md"]'), null);
        }
        assert.deepEqual(external, [], "no external requests");
        assert.deepEqual(errors, [], "no page errors");
      } finally {
        await context.close();
      }
    }
  });
});
