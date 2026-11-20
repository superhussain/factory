// Dashboard framed by a Paseo web origin against one owned local Worker. A listed parent
// gets a ready reply to its hello, and a Session app click posts the ids instead of leaving
// the frame. An unlisted parent is refused by frame-ancestors. Every https://*.example.test
// request is fulfilled here; the dashboard origin proxies to the owned local origin.
// A sign-in popup opened by a listed parent posts factory-embed-signed-in to it; an unlisted parent gets nothing.
// A listed parent gets the frame's hash on ready and on every change.
// A listed parent also gets the Inbox count, which follows the frame's project filter.
// The config also lists paseo://app, the desktop origin; Chrome here cannot load that scheme, so this proves the extra source leaves https framing intact.
// A walkthrough renders under a listed parent, because its frame-ancestors lists the same embed origins.
// The plugin's warm frame keeper parks a frame with moveBefore and brings it back with the same document.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import { join } from "node:path";
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
const WALK_JOB = {
  id: "045-sample-walkthrough",
  project: "sample",
  title: "Synthetic walkthrough job",
  stage: "build",
  updated: "2026-01-01T00:00:00Z",
};
const PARENT = `<!doctype html><title>Synthetic Paseo</title>
<iframe src="${DASHBOARD}/#042-sample-links" style="border:0;width:100%;height:96vh"></iframe>
<script>
window.received = [];
const frame = document.querySelector("iframe");
const hello = { type: "factory-embed-hello", v: 1 };
if (location.search === "?links=external") hello.links = "external";
frame.addEventListener("load", () => frame.contentWindow.postMessage(hello, "${DASHBOARD}"));
addEventListener("message", (e) => window.received.push(e.data));
</script>`;
const ACCESS_LOGIN = "https://example-team.cloudflareaccess.com/cdn-cgi/access/login";
let accessExpired = false;

function proxy(origin, parent) {
  return async (request) => {
    const url = new URL(request.url());
    if (url.origin === parent && url.pathname === "/")
      return request.respond({ status: 200, contentType: "text/html", body: PARENT });
    if (url.origin !== DASHBOARD) return request.abort();
    if (accessExpired && url.pathname === "/board.json")
      return request.respond({ status: 302, headers: { Location: ACCESS_LOGIN }, body: "" });
    const { host: _host, ...headers } = request.headers();
    // Puppeteer's intercepted headers omit Sec-Fetch-Dest, and the Worker serves walkthroughs only to frame loads.
    if (request.isNavigationRequest() && request.frame()?.parentFrame())
      headers["sec-fetch-dest"] = "iframe";
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

async function framedPage(browser, origin, parent, path = "/") {
  const page = await (await browser.createBrowserContext()).newPage();
  await stubbed(page, { width: 1280, height: 900 }, proxy(origin, parent));
  await page.goto(parent + path, { waitUntil: "domcontentloaded" });
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

const DRAFT = "Synthetic draft: keep me warm";
const CAPTURE_DIR = process.env.FACTORY_PASEO_CAPTURE_DIR;

// The plugin's DOM adapter, without moveBefore on the park when withMove is false.
function installKeeper(withMove) {
  const SANDBOX =
    "allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-downloads allow-modals";
  const adapter = {
    create(origin, src) {
      const element = document.createElement("iframe");
      element.title = "Factory dashboard";
      element.setAttribute("sandbox", SANDBOX);
      element.style.cssText = "border:0;width:100%;height:100%;display:block";
      element.src = src;
      element.addEventListener("load", () => {
        element.contentWindow.postMessage({ type: "factory-embed-hello", v: 1 }, origin);
      });
      return element;
    },
    park(element) {
      if (!withMove) return { appendChild: (node) => document.body.appendChild(node) };
      let park = document.getElementById("factory-plugin-warm-park");
      if (!park) {
        park = document.createElement("div");
        park.id = "factory-plugin-warm-park";
        park.setAttribute("inert", "");
        park.setAttribute("aria-hidden", "true");
        park.style.cssText =
          "position:fixed;left:-100000px;top:0;visibility:hidden;pointer-events:none";
        document.body.appendChild(park);
      }
      park.style.width = element.clientWidth + "px";
      park.style.height = element.clientHeight + "px";
      return park;
    },
    dropped() {},
  };
  window.keeper = window.createFrameKeeper(adapter, () => window.fakeNow);
}

async function warmFrame(page) {
  const handle = await page.waitForSelector("#warm-host iframe");
  const frame = await handle.contentFrame();
  await frame.waitForSelector("#board");
  await frame.waitForFunction(() => document.readyState === "complete");
  return frame;
}

async function capture(page, name) {
  if (!CAPTURE_DIR) return;
  const host = await page.$("#warm-host");
  await host.screenshot({ path: join(CAPTURE_DIR, name) });
}

async function typeDraft(frame) {
  await frame.evaluate("openIdeaDialog()");
  await frame.type("#idea-text", DRAFT);
  await frame.evaluate(() => {
    window.warmMarker = 1;
  });
}

const attach = (page) =>
  page.evaluate((src) => {
    window.attached = window.keeper.attach(
      document.getElementById("warm-host"),
      "https://factory.example.test",
      src,
    );
    return window.attached.warm;
  }, DASHBOARD + "/#042-sample-links");
const park = (page) => page.evaluate(() => window.keeper.park(window.attached.frame));
const draftState = (frame) =>
  frame.evaluate(() => ({
    marker: window.warmMarker ?? null,
    text: document.querySelector("#idea-text").value,
    open: document.querySelector("#idea-dlg").open,
  }));

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
    await seedBoard(origin, [JOB, ASK_SAMPLE, ASK_OTHER, WALK_JOB]);
    for (const [path, body, type] of [
      ["045-sample-walkthrough/spec.md", "# Spec\n\nSynthetic spec.\n", "text/markdown"],
      [
        "045-sample-walkthrough/walkthrough.html",
        '<!doctype html><title>Synthetic walkthrough</title><p id="synthetic-walk">Synthetic walkthrough renders</p>',
        "text/html",
      ],
    ]) {
      const put = await fetch(origin + "/api/jobs/sample/" + path, {
        method: "PUT",
        headers: { "Content-Type": type },
        body,
      });
      assert.equal(put.status, 200, path);
    }

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

    await t.test("external link posts url", async () => {
      const ext = await framedPage(browser, origin, LISTED, "/?links=external");
      await ext.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-embed-ready"),
        { timeout: 5000, polling: 50 },
      );
      const frame = childFrame(ext);
      await frame.waitForSelector("#d-paseo:not([hidden])");
      const before = frame.url();
      await frame.evaluate(() => {
        document.body.insertAdjacentHTML(
          "beforeend",
          '<a id="synthetic-out" href="https://docs.example.test/page" target="_blank">out</a>' +
            '<a id="synthetic-hash" href="#/inbox">hash</a>',
        );
      });
      const target = ext
        .browserContext()
        .waitForTarget((x) => x.url().includes("docs.example.test"), { timeout: 2000 })
        .catch(() => null);
      await frame.evaluate(() => document.querySelector("#synthetic-out").click());
      await ext.waitForFunction(() => window.received.some((m) => m?.type === "factory-open-url"), {
        timeout: 5000,
        polling: 50,
      });
      assert.equal(await target, null);
      assert.deepEqual(
        await ext.evaluate(() => window.received.filter((m) => m?.type === "factory-open-url")),
        [{ type: "factory-open-url", v: 1, url: "https://docs.example.test/page" }],
      );
      assert.equal(childFrame(ext).url(), before);
      await ext.evaluate(() => {
        window.received = [];
      });
      await frame.evaluate(() => document.querySelector("#synthetic-hash").click());
      await frame.evaluate(() => document.querySelector("#d-paseo").click());
      await ext.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-open-agent"),
        { timeout: 5000, polling: 50 },
      );
      assert.equal(
        await ext.evaluate(() => window.received.some((m) => m?.type === "factory-open-url")),
        false,
      );
    });

    await t.test("links stay default without opt-in", async () => {
      const frame = childFrame(page);
      await frame.evaluate(() => {
        document.body.insertAdjacentHTML(
          "beforeend",
          '<a id="synthetic-out" href="https://docs.example.test/page" target="_blank">out</a>',
        );
      });
      await page.evaluate(() => {
        window.received = [];
      });
      const target = page
        .browserContext()
        .waitForTarget((x) => x.url().includes("docs.example.test"), { timeout: 5000 });
      await frame.evaluate(() => document.querySelector("#synthetic-out").click());
      assert.ok(await target);
      assert.equal(
        await page.evaluate(() => window.received.some((m) => m?.type === "factory-open-url")),
        false,
      );
    });

    await t.test("walkthrough renders under a listed parent", async () => {
      const walk = await framedPage(browser, origin, LISTED);
      try {
        await walk.waitForFunction(
          () => window.received.some((m) => m?.type === "factory-embed-ready"),
          { timeout: 5000, polling: 50 },
        );
        await childFrame(walk).evaluate(() => {
          location.hash = "#045-sample-walkthrough/spec";
        });
        const show = await childFrame(walk).waitForSelector(
          '#d-doc .html-stepper-bar button[aria-label="Show walkthrough"]',
        );
        await show.click();
        let text = null;
        const deadline = Date.now() + 10000;
        while (text === null && Date.now() < deadline) {
          for (const f of walk.frames()) {
            if (f.detached || !f.url().split("#")[0].endsWith("/walkthrough.html")) continue;
            text = await f.$eval("#synthetic-walk", (p) => p.textContent).catch(() => null);
            if (text !== null) break;
          }
          if (text === null) await new Promise((resolve) => setTimeout(resolve, 100));
        }
        if (CAPTURE_DIR)
          await walk.screenshot({ path: join(CAPTURE_DIR, "paseo-walkthrough-1280.png") });
        assert.equal(text, "Synthetic walkthrough renders");
      } finally {
        await walk.browserContext().close();
      }
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

    await t.test("expired sign-in posts signin", async () => {
      const expired = await framedPage(browser, origin, LISTED);
      const embedTypes = () =>
        expired.evaluate(() =>
          window.received
            .filter((m) => m?.type === "factory-embed-ready" || m?.type === "factory-embed-signin")
            .map((m) => m.type),
        );
      try {
        await expired.waitForFunction(
          () => window.received.some((m) => m?.type === "factory-embed-ready"),
          { timeout: 5000, polling: 50 },
        );
        await childFrame(expired).waitForFunction(
          () => document.body.innerText.includes("Synthetic linked job"),
          { timeout: 8000 },
        );
        accessExpired = true;
        await childFrame(expired).evaluate(() => poll());
        if (CAPTURE_DIR)
          await expired.screenshot({ path: join(CAPTURE_DIR, "frame-sign-in-expired.png") });
        assert.deepEqual(
          await expired.evaluate(() =>
            window.received.filter((m) => m?.type === "factory-embed-signin"),
          ),
          [{ type: "factory-embed-signin", v: 1 }],
        );
        const banner = await childFrame(expired).evaluate(() => ({
          text: document.querySelector("#conn-text").textContent,
          button: document.querySelector("#conn-signin").checkVisibility(),
        }));
        assert.deepEqual(banner, { text: "Your sign-in has expired.", button: true });
        await expired.evaluate((dashboard) => {
          document
            .querySelector("iframe")
            .contentWindow.postMessage({ type: "factory-embed-hello", v: 1 }, dashboard);
        }, DASHBOARD);
        await expired.waitForFunction(
          () => window.received.filter((m) => m?.type === "factory-embed-signin").length === 2,
          { timeout: 5000, polling: 50 },
        );
        assert.deepEqual(await embedTypes(), [
          "factory-embed-ready",
          "factory-embed-signin",
          "factory-embed-ready",
          "factory-embed-signin",
        ]);
      } finally {
        accessExpired = false;
        await expired.browserContext().close();
      }
    });

    await t.test("leaving signin posts ready", async () => {
      const recovered = await framedPage(browser, origin, LISTED);
      try {
        await recovered.waitForFunction(
          () => window.received.some((m) => m?.type === "factory-embed-ready"),
          { timeout: 5000, polling: 50 },
        );
        await childFrame(recovered).waitForFunction(
          () => document.body.innerText.includes("Synthetic linked job"),
          { timeout: 8000 },
        );
        accessExpired = true;
        await childFrame(recovered).evaluate(() => poll());
        await recovered.waitForFunction(
          () => window.received.filter((m) => m?.type === "factory-embed-signin").length === 1,
          { timeout: 5000, polling: 50 },
        );
        accessExpired = false;
        await childFrame(recovered).evaluate(() => poll());
        assert.equal(
          await childFrame(recovered).evaluate(() => document.querySelector("#conn-banner").hidden),
          true,
        );
        if (CAPTURE_DIR)
          await recovered.screenshot({
            path: join(CAPTURE_DIR, "frame-sign-in-recovered.png"),
            fullPage: true,
          });
        await recovered.waitForFunction(
          () => window.received.filter((m) => m?.type === "factory-embed-ready").length === 2,
          { timeout: 5000, polling: 50 },
        );
        assert.deepEqual(
          await recovered.evaluate(() =>
            window.received
              .filter(
                (m) => m?.type === "factory-embed-ready" || m?.type === "factory-embed-signin",
              )
              .map((m) => m.type),
          ),
          ["factory-embed-ready", "factory-embed-signin", "factory-embed-ready"],
        );
      } finally {
        accessExpired = false;
        await recovered.browserContext().close();
      }
    });

    await t.test("warm frame keeps the draft", async () => {
      const source = await readFile(
        new URL("../../paseo-plugin/shared/warm-frame.ts", import.meta.url),
        "utf8",
      );
      const content =
        stripTypeScriptTypes(source) +
        "\nwindow.createFrameKeeper = createFrameKeeper; window.WARM_FRAME_MS = WARM_FRAME_MS;\n";
      const warm = await framedPage(browser, origin, LISTED);
      await warm.evaluate(() => {
        document.querySelector("iframe").remove();
        const host = document.createElement("div");
        host.id = "warm-host";
        host.style.cssText = "width:1280px;height:720px";
        document.body.prepend(host);
        window.fakeNow = 0;
      });
      // Each new dashboard document in the warm host is one navigation request. A fragment
      // jump makes none, though Chrome still fires a late load event on the iframe for it.
      const documents = [];
      warm.on("request", (request) => {
        if (request.isNavigationRequest() && request.url().startsWith(DASHBOARD + "/"))
          documents.push(request.url());
      });
      await warm.addScriptTag({ type: "module", content });
      await warm.waitForFunction(() => typeof window.createFrameKeeper === "function");
      await warm.evaluate(installKeeper, true);

      assert.equal(await attach(warm), false);
      await typeDraft(await warmFrame(warm));

      await park(warm);
      await new Promise((resolve) => setTimeout(resolve, 500));
      await warm.evaluate(() => {
        window.fakeNow += 120_000;
      });
      assert.equal(await attach(warm), true);
      let frame = await warmFrame(warm);
      assert.equal(documents.length, 1);
      assert.deepEqual(await draftState(frame), { marker: 1, text: DRAFT, open: true });
      await capture(warm, "after-return.png");

      await warm.evaluate(() => {
        const side = document.createElement("button");
        side.id = "side";
        side.textContent = "Synthetic session row";
        document.body.append(side);
      });
      await warm.click("#side");
      await park(warm);
      assert.equal(await attach(warm), true);
      await warm.evaluate(() => window.attached.frame.element.focus());
      frame = await warmFrame(warm);
      assert.equal(await frame.evaluate(() => document.activeElement?.id), "idea-text");
      await warm.keyboard.type(" more");
      assert.equal(
        await frame.evaluate(() => document.querySelector("#idea-text").value),
        DRAFT + " more",
      );
      await warm.click("#side");
      // A real click on plain dialog text focuses the modal dialog, so model a press that leaves focus on body.
      await frame.evaluate(() => {
        document.querySelector("#idea-key-help").dispatchEvent(new PointerEvent("pointerdown"));
        document.activeElement.blur();
      });
      await warm.evaluate(() => window.attached.frame.element.focus());
      await capture(warm, "empty-click.png");
      assert.notEqual(await frame.evaluate(() => document.activeElement?.id), "idea-text");

      await frame.evaluate(() => document.querySelector("#idea-dlg").close());
      await warm.evaluate(
        (src) => window.keeper.jumpTo(window.attached.frame, src),
        DASHBOARD + "/#043-sample-ask",
      );
      await frame.waitForFunction(() => location.hash === "#043-sample-ask", { timeout: 5000 });
      assert.equal(documents.length, 1);
      assert.equal(await frame.evaluate(() => window.warmMarker), 1);

      await park(warm);
      await warm.evaluate(() => {
        window.fakeNow += window.WARM_FRAME_MS;
      });
      assert.equal(await attach(warm), false);
      frame = await warmFrame(warm);
      assert.equal(documents.length, 2);
      assert.equal(await frame.evaluate(() => window.warmMarker), undefined);

      await warm.evaluate(() => window.keeper.drop());
      await warm.evaluate(installKeeper, false);
      assert.equal(await attach(warm), false);
      await typeDraft(await warmFrame(warm));
      await park(warm);
      assert.equal(await attach(warm), false);
      frame = await warmFrame(warm);
      assert.equal(documents.length, 4);
      assert.deepEqual(await draftState(frame), { marker: null, text: "", open: false });
      await capture(warm, "before-return.png");
    });
  });
});

test("scrollbar press keeps the draft field", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(
    async ({ browser, origin }) => {
      await seedProject(
        origin,
        "sample",
        "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      );
      await seedBoard(origin, [JOB]);
      const page = await framedPage(browser, origin, LISTED);
      await page.waitForFunction(
        () => window.received.some((m) => m?.type === "factory-embed-ready" && m.v === 1),
        { timeout: 5000, polling: 50 },
      );
      const frame = childFrame(page);
      await frame.waitForSelector("#board");
      await typeDraft(frame);
      const dlg = await frame.evaluate(() => {
        const d = document.querySelector("#idea-dlg");
        d.style.maxHeight = "200px";
        d.style.overflowY = "scroll";
        const r = d.getBoundingClientRect();
        return {
          bar: d.offsetWidth - d.clientWidth - 2 * d.clientLeft,
          x: r.left + d.clientLeft + d.clientWidth,
          y: r.top + r.height / 2,
        };
      });
      assert.ok(dlg.bar > 0, "scrollbars hidden");
      assert.equal(
        await frame.evaluate(() => {
          const s = document.querySelector("#idea-key-help");
          const r = s.getBoundingClientRect();
          return onScrollbar({ target: s, clientX: r.left + 1, clientY: r.top + 1 });
        }),
        false,
        "an inline span press is not a scrollbar press",
      );
      const box = await (await page.$("iframe")).boundingBox();
      await page.mouse.click(box.x + dlg.x + dlg.bar / 2, box.y + dlg.y);
      assert.equal(await frame.evaluate(() => document.activeElement?.id), "idea-dlg");

      await page.evaluate(() => {
        const side = document.createElement("button");
        side.id = "side";
        side.textContent = "Synthetic session row";
        document.body.append(side);
      });
      await page.click("#side");
      await page.evaluate(() => document.querySelector("iframe").focus());
      if (CAPTURE_DIR)
        await (
          await page.$("iframe")
        ).screenshot({
          path: join(CAPTURE_DIR, "scrollbar-return.png"),
        });
      assert.equal(await frame.evaluate(() => document.activeElement?.id), "idea-text");
      assert.equal(await frame.evaluate(() => document.querySelector("#idea-text").value), DRAFT);
    },
    { scrollbars: true },
  );
});

test("warm return scrolls the draft field into view", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB]);
    const page = await framedPage(browser, origin, LISTED);
    await page.waitForFunction(
      () => window.received.some((m) => m?.type === "factory-embed-ready" && m.v === 1),
      { timeout: 5000, polling: 50 },
    );
    const frame = childFrame(page);
    await frame.waitForSelector("#board");
    await typeDraft(frame);
    // Whether #idea-text lies inside the dialog's client box. scrollTop snaps to whole
    // pixels while the field sits on a half pixel, so allow 1px.
    const inView = () =>
      frame.evaluate(() => {
        const dlg = document.querySelector("#idea-dlg");
        const top = dlg.getBoundingClientRect().top + dlg.clientTop;
        const t = document.querySelector("#idea-text").getBoundingClientRect();
        return t.top >= top - 1 && t.bottom <= top + dlg.clientHeight + 1;
      });
    await frame.evaluate(() => {
      const d = document.querySelector("#idea-dlg");
      d.style.maxHeight = "240px";
      d.style.overflowY = "scroll";
      d.scrollTop = d.scrollHeight;
      // A scrollbar press leaves focus on the dialog, as in the scrollbar test.
      d.focus({ preventScroll: true });
    });
    assert.equal(await inView(), false, "draft field not clipped");

    await page.evaluate(() => {
      const side = document.createElement("button");
      side.id = "side";
      side.textContent = "Synthetic session row";
      document.body.append(side);
    });
    await page.click("#side");
    await page.evaluate(() => document.querySelector("iframe").focus());
    assert.equal(await frame.evaluate(() => document.activeElement?.id), "idea-text");
    assert.equal(await inView(), true, "draft field still clipped");
  });
});
