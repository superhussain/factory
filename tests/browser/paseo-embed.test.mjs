// Dashboard framed by a Paseo web origin against one owned local Worker. A listed parent
// gets a ready reply to its hello, and a Session app click posts the ids instead of leaving
// the frame. An unlisted parent is refused by frame-ancestors. Every https://*.example.test
// request is fulfilled here; the dashboard origin proxies to the owned local origin.
// A sign-in popup opened by a listed parent posts factory-embed-signed-in to it; an unlisted parent gets nothing.
// A listed parent gets the frame's hash on ready and on every change.
// A listed parent also gets the Inbox count, which follows the frame's project filter.
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
const ASK_SAMPLE = {
  id: "043-sample-ask",
  project: "sample",
  title: "Synthetic sample ask",
  stage: "needs-human",
  updated: "2026-01-01T00:00:00Z",
};
const ASK_OTHER = {
  id: "044-other-ask",
  project: "other",
  title: "Synthetic other ask",
  stage: "needs-human",
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

function proxy(origin, parent) {
  return async (request) => {
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
  };
}

async function stubbed(page, viewport, handler) {
  await page.setViewport(viewport);
  await page.setBypassServiceWorker(true);
  await page.evaluateOnNewDocument(() => {
    window.WebSocket = class extends EventTarget {
      close() {}
    };
  });
  await page.setRequestInterception(true);
  page.on("request", handler);
}

async function framedPage(browser, origin, parent) {
  const page = await (await browser.createBrowserContext()).newPage();
  await stubbed(page, { width: 1280, height: 900 }, proxy(origin, parent));
  await page.goto(parent + "/", { waitUntil: "domcontentloaded" });
  return page;
}

async function signInPopup(page, origin, parent) {
  const opened = new Promise((resolve) => page.once("popup", resolve));
  await page.evaluate(() => {
    window.popup = window.open("about:blank", "factory-sign-in", "popup,width=480,height=720");
  });
  const popup = await opened;
  await stubbed(popup, { width: 480, height: 720 }, proxy(origin, parent));
  await page.evaluate((href) => {
    window.popup.location.href = href;
  }, DASHBOARD + "/#042-sample-links");
  return popup;
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
    await seedProject(
      origin,
      "other",
      "---\nrepo: example/other\nbase: main\ntitle: Synthetic other\n---\n",
    );
    await seedBoard(origin, [JOB, ASK_SAMPLE, ASK_OTHER]);

    const page = await framedPage(browser, origin, LISTED);
    await t.test("listed origin ready", async () => {
      await page.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-embed-ready" && m.v === 1),
        { timeout: 5000, polling: 50 },
      );
      assert.equal(
        await page.evaluate(() =>
          window.received.some((m) => m?.type === "factory-embed-signed-in"),
        ),
        false,
      );
    });

    await t.test("inbox count follows the project filter", async () => {
      const counts = () =>
        page.evaluate(() => window.received.filter((m) => m?.type === "factory-inbox-count"));
      await page.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-inbox-count"),
        { timeout: 8000, polling: 50 },
      );
      for (const m of await counts())
        assert.deepEqual(m, { type: "factory-inbox-count", v: 1, count: 2 });
      await page.evaluate(() => {
        window.received = [];
      });
      await childFrame(page).evaluate(() => {
        localStorage.setItem("factory-projects-hidden", JSON.stringify(["other"]));
        setTimeout(() => location.reload(), 0);
      });
      await page.waitForFunction(
        () => {
          const c = window.received.filter((m) => m?.type === "factory-inbox-count");
          return c.length > 0 && c[c.length - 1].count === 1;
        },
        { timeout: 8000, polling: 50 },
      );
      await childFrame(page).waitForFunction(
        () => document.querySelector("#inbox-count")?.textContent === "1",
        { timeout: 5000, polling: 50 },
      );
    });

    await t.test("sign-in popup posts to listed opener", async () => {
      const popup = await signInPopup(page, origin, LISTED);
      await popup.waitForSelector("#board");
      await popup.waitForFunction(() => document.body.innerText.includes("Synthetic linked job"), {
        timeout: 8000,
      });
      await page.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-embed-signed-in" && m.v === 1),
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

    await t.test("location posts", async () => {
      const locations = () =>
        page.evaluate(() => window.received.filter((m) => m?.type === "factory-embed-location"));
      const waitFor = (hash) =>
        page.waitForFunction(
          (h) =>
            window.received.filter((m) => m?.type === "factory-embed-location").at(-1)?.hash === h,
          { timeout: 5000, polling: 50 },
          hash,
        );
      await waitFor("#042-sample-links");
      assert.deepEqual((await locations())[0], {
        type: "factory-embed-location",
        v: 1,
        hash: "#042-sample-links",
      });
      await childFrame(page).evaluate(() => {
        location.hash = "#/inbox";
      });
      await waitFor("#/inbox");
      await childFrame(page).evaluate(() => replaceUrl("/#/settings/display"));
      await waitFor("#/settings/display");
    });

    await t.test("unlisted origin", async () => {
      const other = await framedPage(browser, origin, UNLISTED);
      const popup = await signInPopup(other, origin, UNLISTED);
      await popup.waitForSelector("#board");
      await new Promise((resolve) => setTimeout(resolve, 3000));
      const board = await childFrame(other)
        .evaluate(() => document.querySelector("#board") !== null)
        .catch(() => false);
      assert.equal(board, false);
      assert.deepEqual(await other.evaluate(() => window.received), []);
    });
  });
});
