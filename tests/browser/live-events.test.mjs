// Live board events against one owned local Worker: a board write reaches the open
// dashboard over its socket, the poll timer runs only while the socket is closed, and a
// hidden tab keeps its socket only while notifications are on.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const job = (stage) => ({
  id: "001-sample",
  project: "sample",
  title: "Synthetic sample job",
  stage,
  updated: "2026-01-01T00:00:00Z",
});
const CHANGED = '{"type":"changed","key":"board.json"}';
const wait = { polling: 50, timeout: 10000 };

test("live board events", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [job("build")]);
    const spec = await fetch(origin + "/api/jobs/sample/001-sample/spec.md", {
      method: "PUT",
      headers: { "If-None-Match": "*" },
      body: "# Spec\n\nSynthetic spec.\n",
    });
    assert.equal(spec.status, 200);

    async function open({ notify = false } = {}) {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 900 });
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      const boardReads = { count: 0 };
      page.on("request", (request) => {
        if (new URL(request.url()).pathname === "/board.json") boardReads.count++;
      });
      await page.evaluateOnNewDocument((notifyOn) => {
        const interval = window.setInterval;
        const clear = window.clearInterval;
        window.pollTimers = new Map();
        window.labelTicks = [];
        window.setInterval = (fn, ms, ...args) => {
          const id = interval(fn, ms, ...args);
          if (fn.name === "pollTick") window.pollTimers.set(id, ms);
          else if (ms === 30000) window.labelTicks.push(fn);
          return id;
        };
        document.hasFocus = () => window.fixtureFocus !== false;
        // The test moves this page's clock forward without waiting.
        const now = Date.now;
        window.clockOffset = 0;
        Date.now = () => now.call(Date) + window.clockOffset;
        window.clearInterval = (id) => {
          window.pollTimers.delete(id);
          return clear(id);
        };
        if (notifyOn) {
          localStorage.setItem("factory-notify", "on");
          window.Notification = class {
            static permission = "granted";
            close() {}
          };
        }
      }, notify);
      const frames = [];
      const cdp = await page.createCDPSession();
      await cdp.send("Network.enable");
      cdp.on("Network.webSocketFrameReceived", ({ response }) =>
        frames.push({ at: Date.now(), data: response.payloadData }),
      );
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(".card", wait);
      await page.waitForFunction("liveOpen && window.pollTimers.size === 0", wait);
      return { context, page, frames, errors, boardReads };
    }
    const timers = (page) => page.evaluate(() => [...window.pollTimers.values()]);
    const setHidden = (page, hidden) =>
      page.evaluate((value) => {
        Object.defineProperty(document, "hidden", { configurable: true, get: () => value });
        document.dispatchEvent(new Event("visibilitychange"));
      }, hidden);
    const stageOf = (page) =>
      page.$eval('.card[data-id="001-sample"]', (el) => el.closest("section.col").classList[1]);

    await t.test("live L1 a board write reaches the open dashboard", async () => {
      const { context, page, frames, errors, boardReads } = await open();
      const etag = (await fetch(origin + "/board.json")).headers.get("ETag");
      // The clock covers only the write and the frame it causes, not page or seed reads.
      const started = Date.now();
      const write = await fetch(origin + "/api/board", {
        method: "PUT",
        headers: { "If-Match": etag },
        body: JSON.stringify({ updated: new Date().toISOString(), jobs: [job("pr-open")] }),
      });
      assert.equal(write.status, 200);
      while (!frames.length && Date.now() - started < 2000)
        await new Promise((resolve) => setTimeout(resolve, 20));
      assert.deepEqual(
        frames.map((frame) => frame.data),
        [CHANGED],
      );
      const elapsed = frames[0].at - started;
      assert.ok(elapsed < 2000, `board change reached the dashboard after ${elapsed} ms`);
      await page.waitForFunction(
        () =>
          document
            .querySelector('.card[data-id="001-sample"]')
            ?.closest("section.col")
            .classList.contains("pr-open"),
        wait,
      );
      assert.deepEqual(await timers(page), []);
      assert.equal(await stageOf(page), "pr-open");

      // With no further event, the freshness label still ages, and redrawing it reads nothing.
      await page.waitForFunction(
        () => /^updated \d+s ago$/.test(document.getElementById("updated").textContent),
        wait,
      );
      const reads = boardReads.count;
      await page.evaluate(() => {
        window.clockOffset = 90000;
        for (const tick of window.labelTicks) tick();
      });
      assert.equal(await page.$eval("#updated", (el) => el.textContent), "updated 2m ago");
      assert.equal(boardReads.count, reads);
      assert.equal(frames.length, 1);
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test(
      "live L2 a closed socket falls back to the timer until it reconnects",
      async () => {
        const { context, page, errors } = await open();
        await page.evaluate("liveSocket.close(1000)");
        await page.waitForFunction("!liveOpen && window.pollTimers.size === 1", wait);
        assert.deepEqual(await timers(page), [30000]);
        const dot = () =>
          page.$eval("#dot", (el) => [el.className, el.title, el.getAttribute("aria-label")]);
        const polling = "Polling every 30 seconds until live updates reconnect";
        assert.deepEqual(await dot(), ["dot ok polling", polling, polling]);
        await page.waitForFunction("liveOpen && window.pollTimers.size === 0", wait);
        assert.deepEqual(await timers(page), []);
        const live = "Live: updates arrive as they happen";
        assert.deepEqual(await dot(), ["dot ok", live, live]);
        assert.deepEqual(errors, []);
        await context.close();
      },
    );

    await t.test(
      "live L5 a socket open with an unchanged board keeps the drawn cards",
      async () => {
        const { context, page, errors } = await open();
        await page.evaluate(() => {
          window.heldCard = document.querySelector('.card[data-id="001-sample"]');
          const original = window.poll;
          window.pollRuns = [];
          window.poll = function poll(options) {
            const done = original(options);
            window.pollRuns.push({ ifChanged: Boolean(options?.ifChanged), done });
            return done;
          };
        });
        await page.evaluate("liveSocket.close(1000)");
        await page.waitForFunction("liveOpen && window.pollTimers.size === 0", wait);
        // The reopen poll's own promise, so the assert runs after any redraw.
        await page.waitForFunction("window.pollRuns.some((run) => run.ifChanged)", wait);
        assert.equal(
          await page.evaluate(() => window.pollRuns.find((run) => run.ifChanged).done),
          true,
        );
        assert.equal(await page.evaluate("heldCard.isConnected"), true);
        assert.deepEqual(errors, []);
        await context.close();
      },
    );

    await t.test(
      "live L6 a timer tick with an unchanged board keeps the focused card",
      async () => {
        const { context, page, errors } = await open();
        await page.focus('.card[data-id="001-sample"]');
        await page.evaluate(() => {
          window.heldCard = document.activeElement;
        });
        assert.equal(await page.evaluate(() => pollTick()), true);
        assert.equal(
          await page.evaluate(() => heldCard.isConnected && document.activeElement === heldCard),
          true,
        );
        // A changed ETag still redraws, and focus moves to the new card for the same job.
        assert.equal(
          await page.evaluate(() => {
            boardEtag = '"stale"';
            return pollTick();
          }),
          true,
        );
        assert.equal(await page.evaluate("heldCard.isConnected"), false);
        assert.equal(
          await page.evaluate(() => document.activeElement.matches('.card[data-id="001-sample"]')),
          true,
        );
        assert.deepEqual(errors, []);
        await context.close();
      },
    );

    await t.test("live L3 hidden tabs keep the socket only with notifications on", async () => {
      const quiet = await open();
      await setHidden(quiet.page, true);
      assert.equal(await quiet.page.evaluate("liveSocket === null && !liveOpen"), true);
      assert.deepEqual(await timers(quiet.page), []);
      assert.deepEqual(quiet.errors, []);
      await quiet.context.close();

      const notified = await open({ notify: true });
      await setHidden(notified.page, true);
      assert.equal(
        await notified.page.evaluate("liveOpen && liveSocket.readyState === WebSocket.OPEN"),
        true,
      );
      assert.deepEqual(await timers(notified.page), []);
      assert.deepEqual(notified.errors, []);
      await notified.context.close();
    });

    await t.test("live L6 a changed frame reads only its key", async () => {
      const { context, page, errors } = await open();
      const paths = [];
      page.on("request", (request) => paths.push(new URL(request.url()).pathname));
      const send = async (data) => {
        paths.length = 0;
        await page.evaluate(
          (d) => liveSocket.dispatchEvent(new MessageEvent("message", { data: d })),
          data,
        );
        await new Promise((resolve) => setTimeout(resolve, 500));
        return paths.filter((p) => p === "/board.json" || p === "/api/backlog").sort();
      };
      assert.deepEqual(await send(JSON.stringify({ type: "changed", key: "board.json" })), [
        "/board.json",
      ]);
      assert.deepEqual(await send(JSON.stringify({ type: "changed", key: "backlog.md" })), [
        "/api/backlog",
      ]);
      assert.deepEqual(await send("not json"), ["/api/backlog", "/board.json"]);
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test("live L7 a fallback tick reads the backlog every 2 minutes", async () => {
      const { context, page, errors } = await open();
      const paths = [];
      page.on("request", (request) => paths.push(new URL(request.url()).pathname));
      const tick = async () => {
        paths.length = 0;
        await page.evaluate(() => pollTick());
        await new Promise((resolve) => setTimeout(resolve, 300));
        return paths.filter((p) => p === "/board.json" || p === "/api/backlog").sort();
      };
      // The socket open already read both, so the backlog is fresh.
      assert.deepEqual(await tick(), ["/board.json"]);
      await page.evaluate(() => {
        window.clockOffset += 120000;
      });
      assert.deepEqual(await tick(), ["/api/backlog", "/board.json"]);
      assert.deepEqual(await tick(), ["/board.json"]);
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test("live L8 the socket keeps retrying at the cap", async () => {
      const { context, page, errors } = await open();
      await page.evaluate(() => {
        window.retryDelays = [];
        const timeout = window.setTimeout;
        window.setTimeout = (fn, ms, ...args) => {
          if (fn.name === "openLive") {
            window.retryDelays.push(ms);
            return 0;
          }
          return timeout(fn, ms, ...args);
        };
        liveRetries = 20;
        liveSocket.close(1000);
      });
      await page.waitForFunction("window.retryDelays.length === 1", wait);
      assert.deepEqual(await page.evaluate("window.retryDelays"), [30000]);
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test(
      "live L4 an event during a drawer load does not load the drawer again",
      async () => {
        const { context, page, frames, errors } = await open();
        const SPEC = "/jobs/sample/001-sample/spec.md";
        let specReads = 0;
        let holding = true;
        const held = [];
        const boardDone = { count: 0 };
        await page.setRequestInterception(true);
        page.on("request", (request) => {
          const path = new URL(request.url()).pathname;
          if (path === SPEC) specReads++;
          if (holding && path.startsWith("/jobs/")) return held.push(request);
          return request.continue();
        });
        page.on("requestfinished", (request) => {
          if (new URL(request.url()).pathname === "/board.json") boardDone.count++;
        });
        // A deep link names a tab, so the drawer waits for that file while it loads.
        await page.evaluate(() => {
          location.hash = "001-sample/spec";
        });
        while (!held.length) await new Promise((resolve) => setTimeout(resolve, 20));
        // Another job's write: the open job's entry stays byte-identical.
        const current = await fetch(origin + "/board.json");
        const etag = current.headers.get("ETag");
        const board = await current.json();
        board.jobs.push({ ...job("build"), id: "002-sample", title: "Synthetic other job" });
        const before = boardDone.count;
        const write = await fetch(origin + "/api/board", {
          method: "PUT",
          headers: { "If-Match": etag },
          body: JSON.stringify(board),
        });
        assert.equal(write.status, 200);
        while (frames.length < 1 || boardDone.count === before)
          await new Promise((resolve) => setTimeout(resolve, 20));
        await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 100)));
        holding = false;
        for (const request of held) request.continue();
        await page.waitForSelector('#d-tabs .tab.active[data-name="spec.md"]', wait);
        await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 300)));
        assert.equal(specReads, 1);
        assert.deepEqual(errors, []);
        await context.close();
      },
    );
  });
});
