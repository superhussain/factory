// The dashboard Inbox badge against the shared fixture that tests/cli/test_inbox.py also reads,
// so the runtime count and the dashboard count cannot drift apart.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const FIXTURE = JSON.parse(
  await readFile(new URL("../fixtures/inbox-count.json", import.meta.url), "utf8"),
);

async function seedBacklog(origin, text) {
  const current = await fetch(origin + "/api/backlog");
  const condition = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  await current.text();
  const response = await fetch(origin + "/api/backlog", {
    method: "PUT",
    headers: { "Content-Type": "text/markdown", ...condition },
    body: text,
  });
  assert.equal(response.status, 200);
}

test("inbox count matches the runtime fixture", { timeout: 180000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, FIXTURE.jobs);
    await seedBacklog(origin, FIXTURE.backlog);
    const page = await (await browser.createBrowserContext()).newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.setViewport({ width: 1280, height: 900 });
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForFunction(
      (n) => {
        const el = document.querySelector("#inbox-count");
        return el && !el.hidden && el.textContent === n;
      },
      { polling: 50 },
      String(FIXTURE.count),
    );
    assert.equal(
      await page.$eval("#inbox-link", (el) => el.getAttribute("aria-label")),
      `Inbox, ${FIXTURE.count} waiting`,
    );
    assert.deepEqual(errors, []);
  });
});
