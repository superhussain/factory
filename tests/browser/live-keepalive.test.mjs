// Live socket keepalive against one owned local Worker: the room answers ping with pong,
// a silent socket is closed, and the polling dot names how the last socket closed.
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
const wait = { polling: 50, timeout: 10000 };

test("live keepalive", { timeout: 120000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [job("build")]);

    async function open() {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 900 });
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      const boardReads = { count: 0 };
      page.on("request", (request) => {
        if (new URL(request.url()).pathname === "/board.json") boardReads.count++;
      });
      await page.evaluateOnNewDocument(() => {
        const interval = window.setInterval;
        window.pingTicks = [];
        window.setInterval = (fn, ms, ...args) => {
          if (fn.name === "livePing") window.pingTicks.push({ fn, ms });
          return interval(fn, ms, ...args);
        };
        // The test moves this page's clock forward without waiting.
        const now = Date.now;
        window.clockOffset = 0;
        Date.now = () => now.call(Date) + window.clockOffset;
      });
      const frames = [];
      const cdp = await page.createCDPSession();
      await cdp.send("Network.enable");
      cdp.on("Network.webSocketFrameReceived", ({ response }) => frames.push(response.payloadData));
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(".card", wait);
      await page.waitForFunction("liveOpen", wait);
      return { context, page, frames, errors, boardReads };
    }
    const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    const title = (page) => page.$eval("#dot", (el) => el.title);
    const polling = (page) =>
      page.evaluate(() => "Polling every " + pollSeconds + " seconds until live updates reconnect");

    await t.test("live K1 a ping gets a pong and reads nothing", async () => {
      const { context, page, frames, errors, boardReads } = await open();
      assert.deepEqual(
        (await page.evaluate("pingTicks")).map((tick) => tick.ms),
        [25000],
      );
      const reads = boardReads.count;
      await page.evaluate("liveSocket.send('ping')");
      const started = Date.now();
      while (!frames.includes("pong") && Date.now() - started < 2000) await sleep(20);
      assert.deepEqual(frames, ["pong"]);
      await sleep(300);
      assert.equal(boardReads.count, reads);
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test("live K2 a silent socket is closed and named", async () => {
      const { context, page, errors } = await open();
      const closed = page.evaluate(
        () =>
          new Promise((resolve) =>
            liveSocket.addEventListener("close", () =>
              setTimeout(() => resolve(document.getElementById("dot").title), 0),
            ),
          ),
      );
      await page.evaluate(() => {
        window.clockOffset = 61000;
        window.pingTicks[0].fn();
      });
      assert.match(
        await closed,
        /\. Last live connection stopped answering pings after 6\d seconds open$/,
      );
      await page.waitForFunction("liveOpen", wait);
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test("live K3 an abnormal close names its code", async () => {
      const { context, page, errors } = await open();
      await page.evaluate("liveSocket.dispatchEvent(new CloseEvent('close', { code: 1006 }))");
      assert.match(
        await title(page),
        /\. Last live connection closed with code 1006 after \d+ seconds open$/,
      );
      await page.waitForFunction("liveOpen", wait);
      await page.evaluate("liveSocket.close(1000)");
      await page.waitForFunction("!liveOpen", wait);
      assert.equal(await title(page), await polling(page));
      assert.deepEqual(errors, []);
      await context.close();
    });

    await t.test("live K4 a close before open names the attempt", async () => {
      const { context, page, errors } = await open();
      const note = await page.evaluate(() => {
        const bare = new EventTarget();
        keepLive(bare);
        bare.dispatchEvent(Object.assign(new Event("close"), { code: 1011, reason: "synthetic" }));
        return liveNote;
      });
      assert.equal(
        note,
        "Last live connection closed with code 1011 (synthetic) before it opened, 0 seconds after it started",
      );
      assert.deepEqual(errors, []);
      await context.close();
    });
  });
});
