import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const UNVERIFIED = [
  "review-code.md worker/push.js:412 bad-line",
  "review-code.md web/dashboard.html:9 off-diff",
];
const FULL = Array.from(
  { length: 20 },
  (_, i) => `review-code.md worker/push.js:${i + 1} bad-line`,
);
const PASSED = { security: "PASS", ux: "PASS", ui: "PASS" };
const documents = {
  "130-ground-export": {
    "spec.md": "# Ground export spec\n\nThe synthetic export specification.\n",
    "round-1/review-code.md": "CHANGES\n\nThe synthetic code review.\n",
    "round-1/grounding.md": [
      "GROUNDING: UNVERIFIED 2",
      "",
      "Head: 2222222222222222222222222222222222222222",
      "Diff: origin/main",
      "Checked: 5 citations in 1 review files",
      "",
      "| Review | Citation | Status | Reason |",
      "| --- | --- | --- | --- |",
      "| review-code.md | `worker/push.js:412` | bad-line | worker/push.js has 300 lines |",
      "| review-code.md | `web/dashboard.html:9` | off-diff | more than 10 lines from every change to web/dashboard.html |",
      "",
      "Unverified means the cited path or line did not check out. The finding may still be right. No verdict changed.",
      "",
    ].join("\n"),
  },
  "133-ground-clean": {
    "spec.md": "# Ground clean spec\n\nThe synthetic clean job.\n",
    "round-1/review-code.md": "PASS\n\nThe synthetic code review.\n",
  },
  "134-ground-full": { "spec.md": "# Ground full spec\n\nThe synthetic full job.\n" },
};
const TITLE = [
  "Round 1 grounding could not check 2 citations. The grounding tab has the reasons.",
  ...UNVERIFIED,
].join("\n");

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(
    origin,
    [
      ["130-ground-export", { ...PASSED, code: "CHANGES", unverified: UNVERIFIED }],
      ["133-ground-clean", { ...PASSED, code: "PASS" }],
      ["134-ground-full", { ...PASSED, code: "CHANGES", unverified: FULL }],
    ].map(([id, round]) => ({
      id,
      project: "sample",
      title: id,
      stage: "review",
      rounds: [round],
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
  "round-N/grounding.md shows in the grounding tab and the card counts unverified citations",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seed(origin);
      for (const [width, height, colorScheme] of [
        [390, 844, "dark"],
        [1440, 900, "light"],
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
          await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
          await page.waitForSelector('.card[data-id="134-ground-full"]');
          const chip = (id) =>
            page.evaluate((id) => {
              const el = document.querySelector(`.card[data-id="${id}"] .chip.unverified`);
              return el && { text: el.textContent, title: el.title };
            }, id);
          assert.deepEqual(await chip("130-ground-export"), { text: "2 unverified", title: TITLE });
          assert.equal(await chip("133-ground-clean"), null);
          assert.equal((await chip("134-ground-full")).text, "20+ unverified");

          await page.goto(origin + "/#130-ground-export/round-1/grounding", {
            waitUntil: "domcontentloaded",
          });
          await page.waitForFunction(
            () =>
              document.querySelector("#d-tabs .tab.active")?.dataset.name === "grounding.md" &&
              document.getElementById("d-doc").textContent.includes("GROUNDING: UNVERIFIED 2"),
          );
          assert.equal(
            await page.$eval("#d-tabs .tab.active", (tab) => tab.textContent.trim()),
            "grounding UNVERIFIED",
          );
          assert.deepEqual(
            await page.$$eval("#d-tabs .tab", (tabs) => tabs.map((t) => t.dataset.name)),
            ["spec.md", "review-code.md", "grounding.md"],
          );

          await page.goto(origin + "/#133-ground-clean", { waitUntil: "domcontentloaded" });
          await page.waitForFunction(
            () =>
              document.querySelector("#d-tabs .tab.active")?.dataset.name === "spec.md" &&
              document.getElementById("d-doc").textContent.includes("Ground clean spec"),
          );
          assert.deepEqual(
            await page.$$eval("#d-tabs .tab", (tabs) => tabs.map((t) => t.dataset.name)),
            ["spec.md", "review-code.md"],
          );
          assert.deepEqual(external, [], "no external requests");
          assert.deepEqual(errors, [], "no page errors");
        } finally {
          await context.close();
        }
      }
    });
  },
);
