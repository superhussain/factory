// Text buttons reach a 44px tap target on coarse pointers and keep their size with a mouse.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { openJobActions } from "../support/board-ready.cjs";

const JOBS = [
  { id: "001-sample", project: "sample", title: "Synthetic sample job", stage: "spec" },
  {
    id: "002-idea",
    title: "Synthetic idea",
    stage: "idea",
    idea: "Synthetic idea\nTap target fixture.",
  },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));

test("text buttons get 44px tap targets on coarse pointers", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const measure = async (viewport) => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport(viewport);
      await page.goto(origin + "/#002-idea", { waitUntil: "domcontentloaded" });
      await page.waitForSelector("#d-discard:not([hidden])");
      await openJobActions(page);
      return page.evaluate(() => ({
        coarse: matchMedia("(pointer: coarse)").matches,
        actions: document.getElementById("d-actions").getBoundingClientRect().height,
        discard: document.getElementById("d-discard").getBoundingClientRect().height,
        newIdea: document.getElementById("new-idea").getBoundingClientRect().height,
      }));
    };
    const touch = await measure({ width: 390, height: 844, isMobile: true, hasTouch: true });
    assert.equal(touch.coarse, true);
    assert.ok(touch.actions >= 44, `Actions height ${touch.actions}`);
    assert.ok(touch.discard >= 44, `Discard height ${touch.discard}`);
    assert.equal(touch.newIdea, 44);
    const mouse = await measure({ width: 1280, height: 900 });
    assert.equal(mouse.coarse, false);
    assert.ok(mouse.discard < 44, `Discard height ${mouse.discard}`);
    assert.equal(mouse.newIdea, 36);
  });
});
