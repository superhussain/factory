// Installable offline dashboard: the service worker caches only the shell and board reads,
// serves the saved board under the offline banner, refreshes on reconnect, clears itself on
// an Access redirect and never replays a write. A loopback proxy plays the network. node --test.
import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import owned from "../support/owned-origin.cjs";
import {
  assertBrowserTarget,
  controlBoardPolling,
  seedBoard,
  seedProject,
  withBrowserFixture,
} from "../support/browser-server.mjs";
import { waitForBoard } from "../support/board-ready.cjs";

const OFFLINE =
  "You're offline. Showing the board saved on this device. Edits won't save until you reconnect.";
const job = (id, title) => ({
  id,
  project: "sample",
  stage: "build",
  title,
  updated: "2026-01-01T00:00:00Z",
});
const JOBS = [job("001-alpha", "Synthetic alpha"), job("002-beta", "Synthetic beta")];

// up forwards to the local service, down drops the connection, access answers the Access login redirect.
async function startProxy(target) {
  const proxy = { mode: "up" };
  const upstream = new URL(target);
  const server = http.createServer((req, res) => {
    if (proxy.mode === "down") return req.socket.destroy();
    if (proxy.mode === "access") {
      res.writeHead(302, {
        Location: "https://example-team.cloudflareaccess.com/cdn-cgi/access/login",
      });
      return res.end();
    }
    const forward = http.request(
      {
        host: upstream.hostname,
        port: upstream.port,
        path: req.url,
        method: req.method,
        headers: req.headers,
      },
      (reply) => {
        res.writeHead(reply.statusCode, reply.headers);
        reply.pipe(res);
      },
    );
    forward.on("error", () => req.socket.destroy());
    req.pipe(forward);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  proxy.origin = `http://127.0.0.1:${server.address().port}`;
  proxy.close = () => {
    server.closeAllConnections();
    return new Promise((resolve) => server.close(resolve));
  };
  return proxy;
}

const boardETag = async (origin) => (await fetch(origin + "/api/board")).headers.get("ETag");
const banner = (page) =>
  page.evaluate(() => ({
    hidden: document.getElementById("conn-banner").hidden,
    text: document.getElementById("conn-text").textContent,
    signin: !document.getElementById("conn-signin").hidden,
  }));
const cacheState = (page) =>
  page.evaluate(async () => ({
    names: await caches.keys(),
    paths: (await caches.has("factory-v2"))
      ? (await (await caches.open("factory-v2")).keys()).map((r) => new URL(r.url).pathname)
      : [],
  }));

test("PWA offline board, reconnect refresh, Access sign-out and no write replay", async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const proxy = await startProxy(origin);
    assertBrowserTarget(proxy.origin, { [owned.OWNED_ORIGIN_ENV]: proxy.origin });
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await controlBoardPolling(page);
    try {
      // Online: the page registers /sw.js, and once it controls the page the board read is cached.
      await page.goto(proxy.origin + "/", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      const script = await page.evaluate(
        async () => (await navigator.serviceWorker.ready).active.scriptURL,
      );
      assert.equal(script, proxy.origin + "/sw.js");
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.waitForSelector('.card[data-id="002-beta"]');
      const online = await cacheState(page);
      assert.deepEqual(online.names, ["factory-v2"]);
      for (const path of ["/", "/board.json", "/projects.json", "/assets/mermaid.min.js"])
        assert.ok(online.paths.includes(path), `${path} is cached`);
      for (const path of online.paths) {
        assert.ok(
          ["/", "/board.json", "/projects.json"].includes(path) ||
            path.startsWith("/assets/") ||
            path.startsWith("/jobs/"),
          `${path} is an allowed cache key`,
        );
        assert.ok(!path.startsWith("/api/"), `${path} is not an API path`);
      }
      assert.equal((await banner(page)).hidden, true);

      // Server unreachable: a reload shows the saved board under the offline banner.
      proxy.mode = "down";
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForSelector("#conn-banner:not([hidden])");
      await page.waitForSelector('.card[data-id="002-beta"]');
      assert.deepEqual(await banner(page), { hidden: false, text: OFFLINE, signin: false });
      assert.equal(await page.evaluate(() => document.getElementById("dot").className), "dot err");

      // The board changes while this device is offline; an offline write fails outright.
      await seedBoard(origin, [...JOBS, job("003-gamma", "Synthetic gamma")]);
      const etag = await boardETag(origin);
      const write = await page.evaluate(() =>
        fetch("./api/board", { method: "PUT", body: "{}" }).then(
          () => "resolved",
          () => "rejected",
        ),
      );
      assert.equal(write, "rejected");

      // Reconnect: the online event polls at once, the banner hides and the change appears.
      proxy.mode = "up";
      await page.evaluate(() => window.dispatchEvent(new Event("online")));
      await page.waitForFunction(() => document.getElementById("conn-banner").hidden, {
        timeout: 2000,
      });
      await page.waitForSelector('.card[data-id="003-gamma"]', { timeout: 2000 });
      await page.evaluate(() => window.fixtureBoardPoll());
      assert.equal(await boardETag(origin), etag, "nothing replayed the offline write");

      // Access redirect: the sign-in banner shows and the cache is gone.
      proxy.mode = "access";
      await page.evaluate(() => window.fixtureBoardPoll());
      await page.waitForSelector("#conn-signin:not([hidden])");
      assert.deepEqual(await banner(page), {
        hidden: false,
        text: "Your sign-in has expired.",
        signin: true,
      });
      assert.deepEqual(await cacheState(page), { names: [], paths: [] });
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
      await proxy.close();
    }
  });
});
