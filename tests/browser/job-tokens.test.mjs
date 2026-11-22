// The job panel lists each host's token counts by model in a collapsible table, hides the table
// when a job has no tokens, and skips malformed entries.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const counts = (input, output, cache_write, cache_read) => ({
  input,
  output,
  cache_write,
  cache_read,
});
const JOB = {
  id: "1017-add-x",
  project: "sample",
  title: "Add x",
  stage: "review",
  round: 1,
  updated: "2026-01-02T13:10:00Z",
  tokens: {
    "host-b": {
      at: "2026-01-02T13:00:00Z",
      transcripts: 1,
      models: { "claude-opus-5-5": counts(300, 9100, 52000, 610000), broken: "x" },
    },
    "host-a": {
      at: "2026-01-02T13:10:00Z",
      transcripts: 3,
      models: { "claude-opus-5-5": counts(1200, 48000, 210000, 3400000) },
    },
    "host-c": "not an object",
    "host-d": { models: [1, 2] },
  },
};
const ODD = {
  ...JOB,
  id: "1018-odd-counts",
  title: "Odd counts",
  tokens: { "host-a": { models: { "model-a": counts(-1, 1.5, "7", 2 ** 60), "model-b": null } } },
};
const PLAIN = { ...JOB, id: "1019-no-tokens", title: "No tokens", tokens: undefined };
const BAD = { ...JOB, id: "1020-bad-tokens", title: "Bad tokens", tokens: ["host-a"] };

test("job tokens table", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB, ODD, PLAIN, BAD]);
    const errors = [];
    const page = await browser.newPage();
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.setViewport({ width: 1280, height: 900 });
    await page.goto(`${origin}/#${JOB.id}`, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => !document.getElementById("d-tokens").hidden);
    assert.equal(
      await page.$eval("#d-tokens summary", (el) => el.textContent),
      "Tokens: 4,330,600 on 2 hosts",
    );
    assert.deepEqual(
      await page.$$eval("#d-tokens thead th", (ths) => ths.map((th) => th.textContent)),
      ["Host", "Model", "Input", "Output", "Cache write", "Cache read"],
    );
    const rows = () =>
      page.$$eval("#d-tokens tbody tr", (trs) =>
        trs.map((tr) => [...tr.cells].map((td) => td.textContent)),
      );
    assert.deepEqual(await rows(), [
      ["host-a", "claude-opus-5-5", "1,200", "48,000", "210,000", "3,400,000"],
      ["host-b", "claude-opus-5-5", "300", "9,100", "52,000", "610,000"],
    ]);

    const open = async (job) => {
      await page.evaluate((id) => (location.hash = id), job.id);
      await page.waitForFunction(
        (title) => document.getElementById("d-title").textContent === title,
        {},
        job.title,
      );
    };
    await open(ODD);
    assert.equal(
      await page.$eval("#d-tokens summary", (el) => el.textContent),
      "Tokens: 0 on 1 host",
    );
    assert.deepEqual(await rows(), [["host-a", "model-a", "0", "0", "0", "0"]]);
    for (const job of [PLAIN, BAD]) {
      await open(job);
      assert.equal(await page.$eval("#d-tokens", (el) => el.hidden), true);
    }
    assert.deepEqual(errors, []);
  });
});
