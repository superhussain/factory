// H goes back to the board from an open job, an open backlog item, Settings and Retros, against one
// owned local Worker, with the focus each view sets on open. It does nothing in a text field or with a
// dialog open. Settings Reference lists it.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const JOBS = [
  {
    id: "001-sample",
    project: "sample",
    title: "Synthetic sample job",
    stage: "spec",
    updated: "2026-01-01T00:00:00Z",
  },
];
const wait = { polling: 50 };

test("h key goes home", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const backlog = await fetch(origin + "/api/backlog", {
      method: "PUT",
      headers: { "If-None-Match": "*", "Content-Type": "text/markdown" },
      body: "# Backlog\n\n- [ ] sample: Add a status page\n",
    });
    assert.equal(backlog.status, 200);
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/#001-sample", { waitUntil: "domcontentloaded" });
    const detailOpen = () => page.$eval("#detail", (el) => !el.hidden);

    await page.waitForSelector("#detail:not([hidden])", wait);
    await page.focus("#d-close");
    await page.keyboard.press("h");
    await page.waitForSelector("#detail[hidden]", wait);
    assert.equal(await page.evaluate(() => location.hash), "");

    await page.waitForSelector(".backlog-card", wait);
    await page.keyboard.press("b");
    await page.waitForSelector("#backlog-form", wait);
    assert.equal(await detailOpen(), true);
    // The backlog drawer focuses its first decision radio, and H still closes it.
    assert.equal(await page.evaluate(() => document.activeElement.type), "radio");
    await page.keyboard.press("h");
    await page.waitForSelector("#detail[hidden]", wait);

    await page.evaluate(() => (location.hash = "#001-sample"));
    await page.waitForSelector("#detail:not([hidden])", wait);
    await page.evaluate(() => {
      const field = document.createElement("input");
      field.id = "home-key-field";
      document.getElementById("detail").append(field);
    });
    await page.focus("#home-key-field");
    await page.keyboard.press("h");
    assert.equal(await detailOpen(), true);
    await page.$eval("#home-key-field", (el) => el.remove());
    // showModal focuses the dialog's project select; the open dialog blocks H.
    await page.$eval("#idea-dlg", (el) => el.showModal());
    await page.keyboard.press("h");
    assert.equal(await detailOpen(), true);
    await page.$eval("#idea-dlg", (el) => el.close());
    await page.keyboard.press("h");
    await page.waitForSelector("#detail[hidden]", wait);

    // Projects focuses its project select. An option starting with h proves type-ahead does not take the key.
    await page.evaluate(() => (location.hash = "#/settings/projects"));
    await page.waitForSelector("#settings-projects:not([hidden])", wait);
    await page.waitForFunction(() => document.activeElement.id === "settings-project", wait);
    const before = await page.$eval("#settings-project", (el) => {
      el.append(new Option("home-key-trap", "home-key-trap"));
      return el.value;
    });
    await page.keyboard.press("h");
    await page.waitForSelector("#settings-page[hidden]", wait);
    assert.equal(await page.evaluate(() => location.hash), "");
    assert.equal(await page.$eval("#settings-project", (el) => el.value), before);

    await page.evaluate(() => (location.hash = "#/settings/pipeline"));
    await page.waitForSelector("#settings-pipeline:not([hidden])", wait);
    await page.waitForFunction(() => document.activeElement.id === "pipeline-scope", wait);
    await page.keyboard.press("h");
    await page.waitForSelector("#settings-page[hidden]", wait);
    assert.equal(await page.evaluate(() => location.hash), "");

    await page.evaluate(() => (location.hash = "#/retros"));
    await page.waitForSelector("#retros-page:not([hidden])", wait);
    await page.keyboard.press("h");
    await page.waitForSelector("#retros-page[hidden]", wait);
    assert.equal(await page.evaluate(() => location.hash), "");

    assert.deepEqual(
      await page.$eval("#settings-back", (el) => [el.title, el.getAttribute("aria-keyshortcuts")]),
      ["Back to board (H)", "h"],
    );
    assert.deepEqual(
      await page.$eval("#retros-back", (el) => [el.title, el.getAttribute("aria-keyshortcuts")]),
      ["Back to board (H)", "h"],
    );
    assert.deepEqual(
      await page.$eval("#d-close", (el) => [el.title, el.getAttribute("aria-keyshortcuts")]),
      ["Close (Esc or H)", "Escape h"],
    );
    await page.evaluate(() => (location.hash = "#/settings/reference"));
    await page.waitForSelector("#settings-reference:not([hidden])", wait);
    const rows = await page.$eval("#reference-keys", (table) =>
      [...table.tBodies[0].rows].map((tr) => [...tr.cells].map((td) => td.textContent)),
    );
    const s = rows.findIndex(([key]) => key === "S");
    assert.deepEqual(rows[s + 1], [
      "H",
      "Back to the board from a job, a backlog item, Settings or Retros",
    ]);
    assert.deepEqual(errors, []);
  });
});
