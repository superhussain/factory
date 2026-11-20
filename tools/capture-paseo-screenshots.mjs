// Paseo plugin screenshot capture: the dashboard board framed by a synthetic Paseo sidebar and header,
// and the plugin's first-login card in Paseo's light palette, at 1280x800 and 2x like
// tools/capture-screenshots.mjs. One owned local service on an ephemeral loopback port, a frozen
// clock, and every off-origin request aborted and fatal. Needs `node tests/browser/provision.mjs`
// and `pnpm --dir paseo-plugin install --frozen-lockfile` first. Never part of CI: run manually
// with `node tools/capture-paseo-screenshots.mjs` to rewrite
// docs/screenshots/{paseo-framed-board,paseo-first-login}.png.
import { mkdir, rm, stat } from "node:fs/promises";
import { prepareAssets } from "./assets.mjs";
import { bundleCards, cardHtml } from "./capture-plugin-cards.mjs";
import { startLocal } from "./local.mjs";
import { browserExecutable, seedBoard, seedProject } from "../tests/support/browser-server.mjs";

const CLOCK = "2026-01-01T12:00:00Z";
const OUT = new URL("../docs/screenshots/", import.meta.url).pathname;
const DASHBOARD = "https://factory.example.test";
const LISTED = "https://paseo.example.test";

// Copied from tests/browser/paseo-embed.test.mjs.
const JOBS = [
  {
    id: "042-sample-links",
    project: "sample",
    title: "Synthetic linked job",
    stage: "pr-open",
    paseo: { server: "synthetic-server", agent: "synthetic-agent" },
    updated: "2026-01-01T00:00:00Z",
  },
  {
    id: "043-sample-ask",
    project: "sample",
    title: "Synthetic sample ask",
    stage: "needs-human",
    updated: "2026-01-01T00:00:00Z",
  },
  {
    id: "044-other-ask",
    project: "other",
    title: "Synthetic other ask",
    stage: "needs-human",
    updated: "2026-01-01T00:00:00Z",
  },
];
const PARENT = `<!doctype html><title>Synthetic Paseo</title><link rel="icon" href="data:,">
<style>
body{margin:0;display:flex;height:100vh;font:13px system-ui,-apple-system,"Segoe UI",sans-serif;color:#1a1a1e;background:#ffffff}
nav{width:220px;flex-shrink:0;box-sizing:border-box;padding:12px;background:#f4f4f5;border-right:1px solid #e4e4e7}
nav b{display:block;margin:4px 8px 16px;font-size:14px}
nav p{margin:0 0 4px;padding:6px 8px;border-radius:6px;color:#71717a}
nav p.on{background:#e4e4e7;color:#1a1a1e}
main{flex:1;display:flex;flex-direction:column;min-width:0}
header{height:40px;flex-shrink:0;display:flex;align-items:center;padding:0 16px;border-bottom:1px solid #e4e4e7;font-weight:600}
iframe{flex:1;width:100%;border:0}
</style>
<nav aria-label="Synthetic Paseo sidebar"><b>Paseo</b><p>Synthetic workspace</p><p>Synthetic agent</p><p class="on">Factory</p></nav>
<main><header>Factory</header><iframe src="${DASHBOARD}/"></iframe></main>
<script>
window.received = [];
const frame = document.querySelector("iframe");
const hello = { type: "factory-embed-hello", v: 1 };
if (location.search === "?links=external") hello.links = "external";
frame.addEventListener("load", () => frame.contentWindow.postMessage(hello, "${DASHBOARD}"));
addEventListener("message", (e) => window.received.push(e.data));
</script>`;

async function capture() {
  await prepareAssets();
  const { code } = await bundleCards();
  await mkdir(OUT, { recursive: true });
  const persistDir = new URL(
    `../.context/capture-paseo-${Date.now()}-${process.pid}/`,
    import.meta.url,
  ).pathname;
  await mkdir(persistDir, { recursive: true });
  const service = await startLocal({ persistDir: persistDir + "r2" });
  const puppeteer = (await import("puppeteer-core")).default;
  const browser = await puppeteer.launch({ executablePath: browserExecutable(), headless: true });
  const origin = service.origin;
  const external = [];

  async function proxy(request) {
    const url = new URL(request.url());
    if (url.origin === LISTED && url.pathname === "/")
      return request.respond({ status: 200, contentType: "text/html", body: PARENT });
    if (url.origin !== DASHBOARD) {
      external.push(request.url());
      return request.abort();
    }
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
  }

  async function freshPage(handler) {
    const page = await (await browser.createBrowserContext()).newPage();
    await page.setViewport({ width: 1280, height: 800, deviceScaleFactor: 2 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
    await page.setBypassServiceWorker(true);
    await page.evaluateOnNewDocument((clock) => {
      const NativeDate = Date;
      window.Date = class extends NativeDate {
        constructor(...args) {
          super(...(args.length ? args : [clock]));
        }
        static now() {
          return new NativeDate(clock).getTime();
        }
      };
      const interval = window.setInterval;
      window.setInterval = (fn, ms, ...args) => (ms === 3000 ? 0 : interval(fn, ms, ...args));
      window.WebSocket = class extends EventTarget {
        close() {}
      };
    }, CLOCK);
    await page.setRequestInterception(true);
    page.on("request", handler);
    return page;
  }

  async function shoot(page, name) {
    await page.evaluate(async () => {
      await document.fonts.ready;
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    });
    const file = `${OUT}${name}.png`;
    await page.screenshot({ path: file });
    const { size } = await stat(file);
    if (size >= 5 * 1024 * 1024) throw new Error(`${file} exceeds 5 MiB`);
    console.log(JSON.stringify({ file: `docs/screenshots/${name}.png`, bytes: size }));
    await page.browserContext().close();
  }

  try {
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
    await seedBoard(origin, JOBS);

    const board = await freshPage(proxy);
    await board.goto(LISTED + "/", { waitUntil: "domcontentloaded" });
    await board.waitForFunction(
      () => window.received.some((m) => m?.type === "factory-embed-ready"),
      { timeout: 15000, polling: 50 },
    );
    await board
      .mainFrame()
      .childFrames()[0]
      .waitForFunction(
        () =>
          [...document.querySelectorAll(".card")].some((card) =>
            card.textContent.includes("Synthetic linked job"),
          ),
        { timeout: 15000 },
      );
    await shoot(board, "paseo-framed-board");

    const card = await freshPage((request) => {
      external.push(request.url());
      request.abort();
    });
    await card.setContent(cardHtml(code, "first-login", { scheme: "light" }));
    await card.waitForSelector('[role="button"]', { timeout: 15000 });
    await shoot(card, "paseo-first-login");

    if (external.length)
      throw new Error("external requests during capture: " + external.join(", "));
  } finally {
    await browser.close();
    await service.close();
    await rm(persistDir, { recursive: true, force: true });
  }
}

await capture();
