// N, J, /, P, S, R, I and W work on Settings, Retros, the Inbox and Notifications, against one owned
// local Worker. S, R and I do nothing on their own page, the board-only keys do nothing off the board,
// and a focused text field blocks the openers.
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
    needs: "Synthetic question",
    updated: "2026-01-01T00:00:00Z",
  },
];
const PAGES = ["#/settings/projects", "#/retros", "#/inbox", "#/notifications"];
const wait = { polling: 50 };

test("single keys work on every page", { timeout: 180000 }, async () => {
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
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#board .card.needs-you", wait);
    await page.waitForSelector(".backlog-card", wait);
    const hash = () => page.evaluate(() => location.hash);
    const goTo = async (h) => {
      await page.evaluate((target) => (location.hash = target), h);
      await page.waitForFunction((target) => location.hash === target, wait, h);
      // Let the route render and set its focus before the key press.
      await page.evaluate(
        () => new Promise((r) => requestAnimationFrame(() => setTimeout(r, 100))),
      );
    };
    const dialogOpens = async (h, key, id) => {
      await goTo(h);
      await page.keyboard.press(key);
      await page.waitForSelector(`#${id}[open]`, wait);
      assert.equal(await hash(), h, `${key} on ${h} stays on the page`);
      await page.$eval(`#${id}`, (el) => el.close());
    };
    const hashBecomes = async (h, key, expected) => {
      await goTo(h);
      await page.keyboard.press(key);
      await page.waitForFunction((target) => location.hash === target, wait, expected);
    };
    const nothing = async (h, key) => {
      await goTo(h);
      await page.keyboard.press(key);
      await page.evaluate(() => new Promise((r) => setTimeout(r, 200)));
      assert.equal(await hash(), h, `${key} on ${h} keeps the hash`);
      assert.equal(await page.$("dialog[open]"), null, `${key} on ${h} opens no dialog`);
      assert.equal(
        await page.$eval("#detail", (el) => el.hidden),
        true,
        `${key} on ${h} opens no drawer`,
      );
    };

    for (const h of PAGES) {
      await dialogOpens(h, "n", "idea-dlg");
      await dialogOpens(h, "j", "job-picker");
      await dialogOpens(h, "/", "job-picker");
      await dialogOpens(h, "p", "job-picker");
      if (h !== "#/settings/projects") await hashBecomes(h, "s", "#/settings/projects");
      if (h !== "#/retros") await hashBecomes(h, "r", "#/retros");
      if (h !== "#/inbox") await hashBecomes(h, "i", "#/inbox");
      await hashBecomes(h, "w", "#001-sample");
      // The Inbox moves between its own rows with the arrow keys.
      if (h !== "#/inbox") await nothing(h, "ArrowRight");
      await nothing(h, "b");
    }

    await nothing("#/settings/pipeline", "s");
    await nothing("#/retros/run1", "r");
    await nothing("#/inbox", "i");

    await goTo("#/retros");
    await page.evaluate(() => {
      const field = document.createElement("input");
      field.id = "global-keys-field";
      document.getElementById("retros-page").append(field);
    });
    await page.focus("#global-keys-field");
    for (const key of ["n", "j", "s", "i", "w"]) {
      await page.keyboard.press(key);
      assert.equal(await hash(), "#/retros", `${key} in a text field keeps the hash`);
      assert.equal(await page.$("dialog[open]"), null, `${key} in a text field opens no dialog`);
    }
    assert.equal(await page.$eval("#global-keys-field", (el) => el.value), "njsiw");
    assert.deepEqual(errors, []);
  });
});
