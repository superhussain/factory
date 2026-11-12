// Settings, Projects history against one owned local Worker: the History rows, Restore to draft,
// saving the restored text as a new version with a Change summary, and a summary refusal.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedDocument } from "../support/browser-server.mjs";

const V1 = "# Notes\nRun pnpm test.\n";
const V2 = "# Notes\nRun pnpm test.\nThen run the deploy step.\n";
const wait = { polling: 50 };
const rows = (page) =>
  page.$$eval("#settings-history li", (lis) =>
    lis.map((li) => ({
      text: li.firstChild.textContent,
      button: li.querySelector("button")?.textContent ?? null,
    })),
  );

test("project history", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Sample\n---\n",
    );
    const tag = await seedDocument(origin, "sample", "notes.md", V1);
    const seeded = await fetch(origin + "/api/project-documents/sample/notes.md", {
      method: "PUT",
      headers: {
        "If-Match": tag,
        "Content-Type": "text/markdown",
        "X-Project-Summary": encodeURIComponent("Add the deploy step"),
      },
      body: V2,
    });
    assert.equal(seeded.headers.get("X-Project-Version"), "2");
    const page = await browser.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    page.on("dialog", (dialog) => dialog.accept());
    await page.goto(origin + "/#/settings/projects", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#settings-tab-notes:not([disabled])", wait);
    await page.click("#settings-tab-notes");
    await page.waitForFunction(
      (v2) => document.getElementById("settings-document").value === v2,
      wait,
      V2,
    );

    await t.test("history H1 rows list every version, newest first", async () => {
      await page.waitForFunction(
        () => document.querySelectorAll("#settings-history li").length === 2,
        wait,
      );
      const [current, first] = await rows(page);
      assert.match(
        current.text,
        /^Version 2 \(current\) · operator@example\.test · \d{4}-\d\d-\d\dT[\d:.]+Z · Add the deploy step$/,
      );
      assert.equal(current.button, null);
      assert.match(first.text, /^Version 1 · operator@example\.test · \d{4}-\d\d-\d\dT[\d:.]+Z$/);
      assert.equal(first.button, "Restore to draft");
      assert.equal(
        await page.$eval("#settings-history button", (b) => b.getAttribute("aria-label")),
        "Restore version 1 to draft",
      );
    });

    await t.test("history H2 Restore to draft puts version 1 in the draft", async () => {
      await page.click("#settings-history button");
      await page.waitForFunction(
        (v1) => document.getElementById("settings-document").value === v1,
        wait,
        V1,
      );
      assert.equal(
        await page.$eval("#settings-status", (el) => el.textContent),
        "Version 1 is in the draft. Save to keep it as a new version.",
      );
    });

    await t.test("history H3 a bad summary is refused and the draft kept", async () => {
      await page.$eval("#settings-summary", (el) => {
        el.removeAttribute("maxlength");
        el.value = "x".repeat(121);
      });
      await page.click("#settings-save");
      await page.waitForFunction(
        () => document.getElementById("settings-status").textContent.startsWith("Could not save"),
        wait,
      );
      assert.equal(
        await page.$eval("#settings-status", (el) => el.textContent),
        "Could not save: summary: must be one line of 1 to 120 characters",
      );
      assert.equal(await page.$eval("#settings-document", (el) => el.value), V1);
    });

    await t.test("history H4 Save keeps the restored text as version 3", async () => {
      await page.$eval("#settings-summary", (el) => {
        el.value = "Undo the deploy step";
      });
      await page.click("#settings-save");
      await page.waitForFunction(
        () => document.querySelectorAll("#settings-history li").length === 3,
        wait,
      );
      const [current] = await rows(page);
      assert.match(current.text, /^Version 3 \(current\) · .* · Undo the deploy step$/);
      assert.equal(await page.$eval("#settings-summary", (el) => el.value), "");
      assert.equal(await page.$eval("#settings-status", (el) => el.textContent), "Saved");
      const stored = await page.evaluate(async () => {
        const r = await fetch("./api/project-history/sample/notes.md/3", { cache: "no-store" });
        return [r.headers.get("X-Project-Version"), await r.text()];
      });
      assert.deepEqual(stored, ["3", V1]);
    });

    assert.deepEqual(errors, []);
  });
});
