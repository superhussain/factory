// Cache-first boot: the dashboard paints the board the service worker saved while the
// network reads are still out, then the network copy replaces it. One page load reads
// each boot resource once. A loopback proxy plays the network. node --test.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import http from "node:http";
import { join } from "node:path";
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

const captureDir = process.env.FACTORY_CAPTURE_DIR;
const job = (id, title, stage = "build") => ({
  id,
  project: "sample",
  stage,
  title,
  updated: "2026-01-01T00:00:00Z",
});
const JOBS = [job("001-alpha", "Synthetic alpha"), job("002-beta", "Synthetic beta")];
const HELD = new Set(["/board.json", "/projects.json", "/api/backlog"]);

// up forwards to the local service; hold parks the board, projects and backlog reads
// until proxy.release() forwards them.
async function startProxy(target) {
  const proxy = { mode: "up", held: [] };
  const upstream = new URL(target);
  const server = http.createServer((req, res) => {
    const forward = () => {
      const upstreamRequest = http.request(
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
      upstreamRequest.on("error", () => req.socket.destroy());
      req.pipe(upstreamRequest);
    };
    if (proxy.mode === "hold" && HELD.has(new URL(req.url, target).pathname))
      return proxy.held.push(forward);
    forward();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  proxy.origin = `http://127.0.0.1:${server.address().port}`;
  proxy.release = () => {
    proxy.mode = "up";
    for (const go of proxy.held.splice(0)) go();
  };
  proxy.close = () => {
    server.closeAllConnections();
    return new Promise((resolve) => server.close(resolve));
  };
  return proxy;
}

const cardTitle = (page, id) =>
  page.evaluate(
    (id) => document.querySelector(`.card[data-id="${id}"] .title`)?.textContent ?? null,
    id,
  );

test("cache-first board boot", async (t) => {
  await prepareAssets();
  if (captureDir) await mkdir(captureDir, { recursive: true });
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
      body: "# Backlog\n\n- [ ] sample: Synthetic backlog idea\n",
    });
    assert.equal(backlog.status, 200);

    await t.test("paints the saved board while reads are held", async () => {
      const proxy = await startProxy(origin);
      assertBrowserTarget(proxy.origin, { [owned.OWNED_ORIGIN_ENV]: proxy.origin });
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      await controlBoardPolling(page);
      try {
        // The first visit installs /sw.js. Once it controls the page, a reload saves the copies.
        await page.goto(proxy.origin + "/", { waitUntil: "domcontentloaded" });
        await waitForBoard(page);
        await page.evaluate(() => navigator.serviceWorker.ready);
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForFunction(() => backlogLoaded && navigator.serviceWorker.controller);
        await page.waitForFunction(async () => {
          const cache = await caches.open("factory-v2");
          const paths = (await cache.keys()).map((r) => new URL(r.url).pathname);
          return ["/board.json", "/projects.json"].every((p) => paths.includes(p));
        });

        await seedBoard(origin, [job("001-alpha", "Synthetic alpha renamed", "review"), JOBS[1]]);
        proxy.mode = "hold";
        await page.reload({ waitUntil: "domcontentloaded" });
        await new Promise((resolve) => setTimeout(resolve, 1500));
        if (captureDir) await page.screenshot({ path: join(captureDir, "board-held-1440.png") });
        assert.ok(proxy.held.length > 0, "the network reads are held");
        assert.equal(await cardTitle(page, "001-alpha"), "Synthetic alpha");
        assert.equal(await cardTitle(page, "002-beta"), "Synthetic beta");

        proxy.release();
        await page.waitForFunction(
          () =>
            document.querySelector('.card[data-id="001-alpha"] .title')?.textContent ===
            "Synthetic alpha renamed",
        );
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
        await proxy.close();
      }
    });

    await t.test("reads each boot resource once", async () => {
      await seedBoard(origin, JOBS);
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.evaluateOnNewDocument(() => {
        const real = window.fetch;
        window.bootReads = {};
        window.fetch = (input, init) => {
          const path = new URL(String(input instanceof Request ? input.url : input), location.href)
            .pathname;
          window.bootReads[path] = (window.bootReads[path] || 0) + 1;
          return real(input, init);
        };
      });
      try {
        await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
        await page.waitForFunction(() => liveOpen && loaded && backlogLoaded);
        await new Promise((resolve) => setTimeout(resolve, 3000));
        const reads = await page.evaluate(() => window.bootReads);
        for (const path of [
          "/board.json",
          "/api/backlog",
          "/api/pipeline/global",
          "/projects.json",
        ])
          assert.equal(reads[path], 1, `${path} read once, got ${JSON.stringify(reads)}`);
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }
    });
  });
});
