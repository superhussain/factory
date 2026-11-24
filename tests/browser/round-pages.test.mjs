import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

// Round 1 is long enough that an old round-2 link has to scroll to its heading.
const FILLER = Array.from({ length: 80 }, (_, i) => `Synthetic finding line ${i + 1}.`).join(
  "\n\n",
);
const documents = {
  "130-ground-export": {
    "spec.md": "# Ground export spec\n\nThe synthetic export specification.\n",
    "pre-review.md": "PRE-REVIEW: FAIL\n\nHead: 2222222222222222222222222222222222222222\n",
    "round-1/review-code.md": `VERDICT: CHANGES\n\nThe synthetic round 1 code review.\n\n${FILLER}\n`,
    "round-1/grounding.md": "GROUNDING: UNVERIFIED 2\n\nThe synthetic grounding report.\n",
    "round-2/review-code.md":
      "VERDICT: PASS\n\nThe synthetic round 2 code review.\n\n![shot](shot.png)\n\n## Findings\n\nSynthetic finding.\n",
    "decision.md": "APPROVE\n\nThe synthetic decision.\n",
  },
  "141-round-escalate": {
    "spec.md": "# Escalate spec\n\nThe synthetic escalated job.\n",
    "decision.md": "ESCALATE\n\nThe synthetic escalation.\n",
  },
  "142-round-closed": {
    "spec.md": "# Closed spec\n\nThe synthetic closed job.\n",
    "decision.md": "CLOSED: synthetic reason\n",
  },
};

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  const round = { security: "PASS", ux: "SKIPPED", ui: "SKIPPED", code: "PASS" };
  await seedBoard(
    origin,
    [
      ["130-ground-export", [{ ...round, code: "CHANGES" }, round]],
      ["141-round-escalate", []],
      ["142-round-closed", []],
    ].map(([id, rounds]) => ({
      id,
      project: "sample",
      title: id,
      stage: "review",
      rounds,
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

const tabs = (page) =>
  page.$$eval("#d-tabs .tab", (list) =>
    list.map((t) => [
      t.dataset.name,
      t.textContent.trim(),
      t.querySelector(".chip")?.className ?? null,
    ]),
  );

test(
  "round files show as one page per reviewer and grounding, with a verdict chip on every verdict tab",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seed(origin);
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 1 });
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

        await page.goto(origin + "/#130-ground-export/round-2/review-code", {
          waitUntil: "domcontentloaded",
        });
        await page.waitForFunction(
          () =>
            document.querySelector("#d-tabs .tab.active")?.dataset.name === "review-code.md" &&
            document
              .getElementById("d-doc")
              .textContent.includes("The synthetic round 2 code review."),
        );
        assert.deepEqual(await tabs(page), [
          ["spec.md", "spec", null],
          ["pre-review.md", "pre-review FAIL", "chip CHANGES"],
          ["review-code.md", "code PASS", "chip PASS"],
          ["grounding.md", "grounding UNVERIFIED", "chip unverified"],
          ["decision.md", "decision APPROVE", "chip PASS"],
        ]);
        assert.deepEqual(
          await page.$$eval("#d-doc .round-head", (heads) =>
            heads.map((h) => [h.tagName, h.dataset.round, h.textContent]),
          ),
          [
            ["H2", "1", "Round 1"],
            ["H2", "2", "Round 2"],
          ],
        );
        assert.deepEqual(
          await page.$$eval("#d-doc .round-head", (heads) =>
            heads.map((h) => {
              const style = getComputedStyle(h);
              return [style.fontSize, style.borderTopWidth, style.borderTopStyle];
            }),
          ),
          [
            ["20px", "0px", "none"],
            ["20px", "2px", "solid"],
          ],
        );
        assert.deepEqual(
          await page.$$eval("#d-doc h2:not(.round-head)", (heads) =>
            heads.map((h) => getComputedStyle(h).fontSize),
          ),
          ["17px"],
        );
        const text = await page.$eval("#d-doc", (doc) => doc.textContent);
        assert.ok(
          text.indexOf("The synthetic round 1 code review.") <
            text.indexOf("The synthetic round 2 code review."),
          "rounds render oldest first",
        );
        assert.ok(await page.$eval("#d-doc", (doc) => doc.scrollTop > 0), "scrolled to Round 2");
        assert.equal(new URL(page.url()).hash, "#130-ground-export/review-code");
        assert.equal(
          await page.$eval('#d-doc img[alt="shot"]', (img) => img.getAttribute("src")),
          "/jobs/sample/130-ground-export/round-2/shot.png",
        );

        await page.click('#d-tabs .tab[data-name="grounding.md"]');
        await page.waitForFunction(() =>
          document.getElementById("d-doc").textContent.includes("The synthetic grounding report."),
        );
        assert.deepEqual(
          await page.$$eval("#d-doc .round-head", (heads) => heads.map((h) => h.textContent)),
          ["Round 1"],
        );
        assert.equal(new URL(page.url()).hash, "#130-ground-export/grounding");

        // An old round link followed while the job is already open still scrolls to its round.
        await page.evaluate(() => {
          location.hash = "#130-ground-export/round-2/review-code";
        });
        await page.waitForFunction(
          () =>
            document.querySelector("#d-tabs .tab.active")?.dataset.name === "review-code.md" &&
            location.hash === "#130-ground-export/review-code",
        );
        assert.ok(
          await page.$eval("#d-doc", (doc) => doc.scrollTop > 0),
          "scrolled to Round 2 in an open job",
        );

        for (const [id, chip] of [
          ["141-round-escalate", ["decision.md", "decision ESCALATE", "chip CHANGES"]],
          ["142-round-closed", ["decision.md", "decision CLOSED", "chip SKIPPED"]],
        ]) {
          await page.goto(origin + `/#${id}/decision`, { waitUntil: "domcontentloaded" });
          await page.waitForFunction(
            (id) =>
              document.getElementById("d-meta").textContent.includes(id) &&
              document.querySelector("#d-tabs .tab.active")?.dataset.name === "decision.md",
            {},
            id,
          );
          assert.deepEqual(await tabs(page), [["spec.md", "spec", null], chip]);
        }
        assert.deepEqual(external, [], "no external requests");
        assert.deepEqual(errors, [], "no page errors");
      } finally {
        await context.close();
      }
    });
  },
);
