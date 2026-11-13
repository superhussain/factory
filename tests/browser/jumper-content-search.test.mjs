// The jumper matches a job's Jira key, PR link and idea text as well as its id, title and project.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const AT = "2026-01-02T12:00:00Z";
const SEARCH = {
  id: "071-sample-search",
  project: "sample",
  title: "Sample search job",
  stage: "pr-open",
  round: 1,
  updated: AT,
  jira: "PLAN-9005",
  pr: "https://github.com/example/sample/pull/71",
  idea: "Sample search job\nKeep overlapping columns apart.",
};
const OTHER = {
  id: "072-sample-other",
  project: "sample",
  title: "Sample other job",
  stage: "build",
  round: 1,
  updated: AT,
  idea: "Unrelated text.",
};
const wait = { polling: 50 };

test("the jumper matches Jira key, PR link and idea", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [SEARCH, OTHER]);
    const page = await (await browser.createBrowserContext()).newPage();
    await page.setViewport({ width: 1280, height: 900, isMobile: false });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(`.card[data-id="${SEARCH.id}"]`, wait);
    await page.evaluate(() => document.getElementById("job-jump").click());
    await page.waitForSelector("#job-picker[open]", wait);

    const setQuery = (value) =>
      page.$eval(
        "#job-search",
        (el, v) => {
          el.value = v;
          el.dispatchEvent(new Event("input", { bubbles: true }));
        },
        value,
      );
    const shown = () =>
      page.$$eval('#job-results [role="option"] .meta', (metas) =>
        metas.map((m) => m.textContent.split(" · ")[1]),
      );
    const count = () => page.$eval("#job-count", (el) => el.textContent);

    await page.type("#job-search", "PLAN-9005");
    await page.evaluate(() => document.fonts.ready);
    await (await page.$("#job-picker")).screenshot({ path: `${SHOTS}jumper-plan-1280.png` });

    await t.test("a Jira key finds its job", async () => {
      assert.deepEqual(await shown(), [SEARCH.id]);
      assert.equal(await count(), "1 job");
    });

    await t.test("a pasted PR link finds its job", async () => {
      await setQuery(SEARCH.pr);
      assert.deepEqual(await shown(), [SEARCH.id]);
      assert.equal(await count(), "1 job");
    });

    await t.test("a PR number finds its job", async () => {
      for (const query of ["#71", "PR #71", "pr#71"]) {
        await setQuery(query);
        assert.deepEqual(await shown(), [SEARCH.id], query);
      }
      await setQuery("#7");
      assert.deepEqual(await shown(), []);
    });

    await t.test("idea words find their job", async () => {
      await setQuery("overlapping columns");
      assert.deepEqual(await shown(), [SEARCH.id]);
      await setQuery("unrelated");
      assert.deepEqual(await shown(), [OTHER.id]);
    });

    await t.test("no match says so", async () => {
      await setQuery("zz-no-match");
      assert.deepEqual(await shown(), []);
      assert.equal(await page.$eval("#job-empty", (el) => el.hidden), false);
      assert.equal(await page.$eval("#job-empty", (el) => el.textContent), "No jobs found");
    });

    await t.test("no page errors", () => {
      assert.deepEqual(errors, []);
    });
  });
});
