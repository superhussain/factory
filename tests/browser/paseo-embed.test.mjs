// Dashboard framed by a Paseo web origin against one owned local Worker. A listed parent
// gets a ready reply to its hello, and a Session app click posts the ids instead of leaving
// the frame. An unlisted parent is refused by frame-ancestors. Every https://*.example.test
// request is fulfilled here; the dashboard origin proxies to the owned local origin.
// The config also lists paseo://app, the desktop origin; Chrome here cannot load that scheme, so this proves the extra source leaves https framing intact.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const DASHBOARD = "https://factory.example.test";
const LISTED = "https://paseo.example.test";
const UNLISTED = "https://other.example.test";
const JOB = {
  id: "042-sample-links",
  project: "sample",
  title: "Synthetic linked job",
  stage: "pr-open",
  paseo: { server: "synthetic-server", agent: "synthetic-agent" },
  updated: "2026-01-01T00:00:00Z",
};
const PARENT = `<!doctype html><title>Synthetic Paseo</title>
<iframe src="${DASHBOARD}/#042-sample-links" style="border:0;width:100%;height:96vh"></iframe>
<script>
window.received = [];
const frame = document.querySelector("iframe");
frame.addEventListener("load", () =>
  frame.contentWindow.postMessage({ type: "factory-embed-hello", v: 1 }, "${DASHBOARD}"));
addEventListener("message", (e) => window.received.push(e.data));
</script>`;

async function framedPage(browser, origin, parent) {
  const page = await (await browser.createBrowserContext()).newPage();
  await page.setViewport({ width: 1280, height: 900 });
  await page.setBypassServiceWorker(true);
  await page.evaluateOnNewDocument(() => {
    window.WebSocket = class extends EventTarget {
      close() {}
    };
  });
  await page.setRequestInterception(true);
  page.on("request", async (request) => {
    const url = new URL(request.url());
    if (url.origin === parent && url.pathname === "/")
      return request.respond({ status: 200, contentType: "text/html", body: PARENT });
    if (url.origin !== DASHBOARD) return request.abort();
    const { host: _host, ...headers } = request.headers();
    const response = await fetch(origin + url.pathname + url.search, {
      method: request.method(),
      headers,
      body: request.postData(),
      redirect: "manual",
    });
    const forwarded = Object.fromEntries(
      [...response.headers].filter(
        ([name]) => !["content-encoding", "content-length"].includes(name),
      ),
    );
    return request.respond({
      status: response.status,
      headers: forwarded,
      body: Buffer.from(await response.arrayBuffer()),
    });
  });
  await page.goto(parent + "/", { waitUntil: "domcontentloaded" });
  return page;
}

const childFrame = (page) => page.mainFrame().childFrames()[0];

test("paseo embed", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB]);

    const page = await framedPage(browser, origin, LISTED);
    await t.test("listed origin ready", async () => {
      await page.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-embed-ready" && m.v === 1),
        { timeout: 5000, polling: 50 },
      );
    });

    await t.test("session app posts ids", async () => {
      const frame = childFrame(page);
      await frame.waitForSelector("#d-paseo:not([hidden])");
      const before = frame.url();
      await frame.evaluate(() => document.querySelector("#d-paseo").click());
      await page.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-open-agent"),
        { timeout: 5000, polling: 50 },
      );
      const opened = await page.evaluate(() =>
        window.received.filter((m) => m?.type === "factory-open-agent"),
      );
      assert.deepEqual(opened, [
        { type: "factory-open-agent", v: 1, server: "synthetic-server", agent: "synthetic-agent" },
      ]);
      assert.equal(childFrame(page).url(), before);
      assert.equal(before, DASHBOARD + "/#042-sample-links");
    });

    await t.test("unlisted origin", async () => {
      const other = await framedPage(browser, origin, UNLISTED);
      await new Promise((resolve) => setTimeout(resolve, 3000));
      const board = await childFrame(other)
        .evaluate(() => document.querySelector("#board") !== null)
        .catch(() => false);
      assert.equal(board, false);
      assert.deepEqual(await other.evaluate(() => window.received), []);
    });
  });
});
