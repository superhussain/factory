// A tab whose page version differs from the Worker's reloads once nothing is unsaved,
// after dropping the service worker's cached "/". An unchanged version never reloads.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const wait = { polling: 50, timeout: 10000 };

test("deploy reload", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      {
        id: "001-sample",
        project: "sample",
        title: "Synthetic sample job",
        stage: "build",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);

    // While state.version is set, board.json answers with that version header.
    async function open() {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 900 });
      await page.setBypassServiceWorker(true);
      const state = { version: null };
      await page.setRequestInterception(true);
      page.on("request", async (request) => {
        if (new URL(request.url()).pathname !== "/board.json" || !state.version)
          return request.continue();
        const board = await fetch(origin + "/board.json");
        await request.respond({
          status: 200,
          contentType: "application/json",
          headers: { "X-Factory-Version": state.version },
          body: await board.text(),
        });
      });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(".card", wait);
      await page.evaluate(async () => {
        window.reloadMarker = 1;
        const cache = await caches.open("factory-v2");
        await cache.put("/", new Response("sentinel"));
      });
      return { context, page, state };
    }
    const sentinel = (page) =>
      page.evaluate(async () => !!(await (await caches.open("factory-v2")).match("/")));
    const reloaded = (page, ms = 10000) =>
      page.waitForFunction("window.reloadMarker !== 1", { polling: 50, timeout: ms });

    await t.test("idle tab reloads once", async () => {
      const { context, page, state } = await open();
      state.version = "0000beef";
      await page.evaluate("poll()");
      await reloaded(page);
      await page.waitForSelector(".card", wait);
      assert.equal(await sentinel(page), false);
      assert.equal(
        await page.evaluate("sessionStorage.getItem('factory-reloaded-for')"),
        "0000beef",
      );
      await page.evaluate("window.reloadMarker = 1");
      await page.evaluate("poll()");
      await new Promise((resolve) => setTimeout(resolve, 1500));
      assert.equal(await page.evaluate("window.reloadMarker"), 1);
      await context.close();
    });

    await t.test("draft waits", async () => {
      const { context, page, state } = await open();
      await page.evaluate("openIdeaDialog()");
      await page.type("#idea-text", "an unsent idea");
      state.version = "0000beef";
      await page.evaluate("poll()");
      await new Promise((resolve) => setTimeout(resolve, 6500));
      assert.equal(await page.evaluate("window.reloadMarker"), 1);
      await page.click("#idea-cancel");
      await reloaded(page);
      await context.close();
    });

    await t.test("backlog draft waits", async () => {
      const { context, page, state } = await open();
      await page.evaluate(
        "backlogBusy.add('item'); backlogDrafts.set('item', { pick: '', notes: 'needs a smaller scope', answers: [] })",
      );
      state.version = "0000beef";
      await page.evaluate("poll()");
      await new Promise((resolve) => setTimeout(resolve, 6500));
      assert.equal(await page.evaluate("window.reloadMarker"), 1);
      await page.evaluate("backlogDrafts.clear()");
      await reloaded(page);
      await context.close();
    });

    await t.test("writes and memory-only answers hold", async () => {
      const { context, page } = await open();
      assert.equal(await page.evaluate("unsavedWork()"), false);
      assert.equal(await page.evaluate("needsSaving = true; unsavedWork()"), true);
      await page.evaluate("needsSaving = false");
      assert.deepEqual(
        await page.evaluate(
          "casBoard(() => { window.during = unsavedWork(); return BOARD_UNCHANGED; }).then(() => [window.during, unsavedWork()])",
        ),
        [true, false],
      );
      await page.evaluate(
        "needsStorageFailed = true; needsMemory.set('k', [{ picked: [], other: 'kept answer' }])",
      );
      assert.equal(await page.evaluate("unsavedWork()"), true);
      await page.evaluate("needsMemory.set('k', [{ picked: [], other: ' ' }])");
      assert.equal(await page.evaluate("unsavedWork()"), false);
      await context.close();
    });

    await t.test("orphaned backlog draft is pruned", async () => {
      const { context, page } = await open();
      await page.evaluate(
        "backlogDrafts.set('- [ ] sample: gone item', { pick: '', notes: 'needs a smaller scope', answers: [] })",
      );
      await page.evaluate("refreshBacklog()");
      await page.waitForFunction("backlogDrafts.size === 0", wait);
      await context.close();
    });

    await t.test("offline tab waits for the connection", async () => {
      const { context, page, state } = await open();
      await page.evaluate("backlogSaving = true");
      state.version = "0000beef";
      await page.evaluate("poll()");
      await page.evaluate("backlogSaving = false; setConnection('offline')");
      await new Promise((resolve) => setTimeout(resolve, 6500));
      assert.equal(await page.evaluate("window.reloadMarker"), 1);
      assert.equal(await sentinel(page), true);
      await page.evaluate("setConnection('online')");
      await reloaded(page);
      await context.close();
    });

    await t.test("same version stays", async () => {
      const { context, page, state } = await open();
      state.version = await page.$eval('meta[name="factory-version"]', (el) => el.content);
      await page.evaluate("poll()");
      await new Promise((resolve) => setTimeout(resolve, 6500));
      assert.equal(await page.evaluate("window.reloadMarker"), 1);
      assert.equal(await page.evaluate("sessionStorage.getItem('factory-reloaded-for')"), null);
      await context.close();
    });
  });
});
