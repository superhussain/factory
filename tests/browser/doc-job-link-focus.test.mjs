import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const LINE =
  "Follows job 175 (DEMO-8558, PR #17126). ![DEMO-9 chart](https://other.example.test/c.png)\n";

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(origin, [
    {
      id: "001-capture",
      project: "sample",
      title: "Synthetic captured job",
      stage: "build",
      updated: "2026-01-01T00:00:00Z",
    },
    {
      id: "175-sample-dates",
      project: "sample",
      title: "Synthetic sample dates",
      stage: "build",
      updated: "2026-01-01T00:00:00Z",
    },
  ]);
  const response = await fetch(origin + "/api/jobs/sample/001-capture/spec.md", {
    method: "PUT",
    headers: { "If-None-Match": "*", "Content-Type": "text/markdown" },
    body: LINE,
  });
  assert.equal(response.status, 200);
}

test("Enter on a job link in a document moves focus to the drawer close button", async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seed(origin);
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.goto(origin + "/#001-capture/spec", { waitUntil: "domcontentloaded" });
    const link = '#d-doc a[href="#175-sample-dates"]';
    await page.waitForSelector(link);
    await page.focus(link);
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => location.hash === "#175-sample-dates");
    await page.waitForFunction(() => document.activeElement?.id === "d-close");
    assert.equal(await page.evaluate(() => location.hash), "#175-sample-dates");
    assert.equal(await page.evaluate(() => document.activeElement.id), "d-close");
    assert.deepEqual(errors, []);
  });
});
