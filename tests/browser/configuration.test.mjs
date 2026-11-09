// Optional link/image configuration: approved configured links/images work in a real
// browser, disallowed origins stay inert, empty policies yield no anchors and no
// external requests, and hostile label/title content stays escaped. Runs under
// node --test; the browser part uses the owned fixture and pinned browser.
import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";
import { mkdtemp, rm } from "node:fs/promises";
import { loadAssets } from "../support/worker.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { prepareAssets } from "../../tools/assets.mjs";
import { startLocal } from "../../tools/local.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const assets = await loadAssets();

const LINKS = {
  app: { label: "Session app", template: "session://h/{server}/agent/{agent}" },
  web: { label: "Session", template: "https://sessions.example.test/{server}/{agent}" },
};
const IMAGES = ["https://images.example.test"];

function config(overrides = {}) {
  return {
    origin: "https://factory.example.test",
    title: "Factory",
    issuer: "https://example-team.cloudflareaccess.com",
    audience: "example-application-audience",
    image_origins: [],
    agent_links: {},
    ...overrides,
  };
}

test("readServerConfig applies omitted defaults and accepts configured policies", () => {
  const bare = assets.readServerConfig({
    FACTORY_ORIGIN: "https://factory.example.test",
    FACTORY_TITLE: "Factory",
    ACCESS_ISSUER: "https://example-team.cloudflareaccess.com",
    ACCESS_AUDIENCE: "aud",
  });
  assert.deepEqual(bare.image_origins, []);
  assert.deepEqual(bare.agent_links, {});
  const full = assets.readServerConfig({
    FACTORY_ORIGIN: "https://factory.example.test",
    FACTORY_TITLE: "Factory",
    ACCESS_ISSUER: "https://example-team.cloudflareaccess.com",
    ACCESS_AUDIENCE: "aud",
    FACTORY_IMAGE_ORIGINS: JSON.stringify(IMAGES),
    FACTORY_AGENT_LINKS: JSON.stringify(LINKS),
  });
  assert.deepEqual(full.image_origins, IMAGES);
  assert.deepEqual(full.agent_links, LINKS);
});

test("renderDashboard injects the configured policy, CSP and escapes", () => {
  const response = assets.renderDashboard(
    config({
      title: "Factory </script> \"&'",
      image_origins: IMAGES,
      agent_links: { web: { label: 'Session <b> & "quoted"', template: LINKS.web.template } },
    }),
  );
  const csp = response.headers.get("Content-Security-Policy");
  assert.match(csp, /img-src 'self' data: blob: https:\/\/images\.example\.test/);
  assert.match(csp, /script-src 'self' 'unsafe-inline';/);
  assert.match(csp, /connect-src 'self';/);
  return response.text().then((html) => {
    const meta = html
      .match(/http-equiv="Content-Security-Policy" content="([^"]*)"/)[1]
      .replaceAll("&#x27;", "'");
    assert.equal(meta, csp);
    assert.ok(!html.includes("</script> \"&'"), "title markup cannot escape the page");
    assert.match(html, /Factory &lt;\/script&gt;/);
    const injected = JSON.parse(html.match(/id="factory-config">([^<]*)<\/script>/)[1]);
    assert.equal(injected.agent_links.web.label, 'Session <b> & "quoted"');
    assert.deepEqual(injected.image_origins, IMAGES);
  });
});

test("jira_origin is optional, https only, and injected only when set", async () => {
  const base = {
    FACTORY_ORIGIN: "https://factory.example.test",
    FACTORY_TITLE: "Factory",
    ACCESS_ISSUER: "https://example-team.cloudflareaccess.com",
    ACCESS_AUDIENCE: "aud",
  };
  assert.equal(assets.readServerConfig(base).jira_origin, null);
  const jira = "https://jira.example.test";
  assert.equal(assets.readServerConfig({ ...base, FACTORY_JIRA_ORIGIN: jira }).jira_origin, jira);
  assert.throws(() =>
    assets.readServerConfig({ ...base, FACTORY_JIRA_ORIGIN: "http://jira.example.test" }),
  );
  assert.throws(() => assets.readServerConfig({ ...base, FACTORY_JIRA_ORIGIN: 1 }));
  const injected = async (env) =>
    JSON.parse(
      (await assets.renderDashboard(env).text()).match(/id="factory-config">([^<]*)<\/script>/)[1],
    );
  assert.ok(!("jira_origin" in (await injected(config({ jira_origin: null })))));
  assert.equal((await injected(config({ jira_origin: jira }))).jira_origin, jira);
});

const JOB = {
  id: "050-config",
  project: "sample",
  title: "Synthetic config job",
  stage: "build",
  updated: "2026-01-01T00:00:00Z",
  paseo: { server: "synthetic-server", agent: "synthetic-agent" },
};
const DOC = [
  "# Config fixture",
  "",
  "![configured](https://images.example.test/pic.png)",
  "",
  "![denied](https://other.example.test/x.png)",
  "",
  "![local](shot.png)",
  "",
  "![traversal](../board.json)",
  "",
  "![scheme](data:image/png;base64,AAAA)",
  "",
  "[bad](javascript:alert(1)) and [web](https://other.example.test/page)",
  "",
].join("\n");

// A 1x1 PNG.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
  "base64",
);

async function seedAll(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(origin, [JOB]);
  const put = await fetch(origin + "/api/jobs/sample/050-config/build.md", {
    method: "PUT",
    headers: { "Content-Type": "text/markdown" },
    body: DOC,
  });
  assert.equal(put.status, 200);
  const image = await fetch(origin + "/api/jobs/sample/050-config/shot.png", {
    method: "PUT",
    headers: { "Content-Type": "image/png" },
    body: PNG,
  });
  assert.equal(image.status, 200);
}

async function pageWithConfig(origin, browser, serverConfig) {
  const rendered = assets.renderDashboard(serverConfig);
  const html = await rendered.text();
  const csp = rendered.headers.get("Content-Security-Policy");
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
  const attempts = [],
    errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  // Stubbed reads must reach interception rather than the dashboard service worker.
  await page.setBypassServiceWorker(true);
  await page.setRequestInterception(true);
  page.on("request", async (request) => {
    const target = new URL(request.url());
    if (target.origin !== origin) {
      attempts.push(request.url());
      return request.abort();
    }
    if (target.pathname === "/") {
      return request.respond({
        status: 200,
        contentType: "text/html",
        headers: { "Content-Security-Policy": csp },
        body: html,
      });
    }
    return request.continue();
  });
  await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
  await waitForBoard(page);
  await page.waitForSelector(".card");
  return { context, page, attempts, errors };
}

test("configured links and images work; everything else stays inert", async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedAll(origin);
    const { context, page, attempts, errors } = await pageWithConfig(
      origin,
      browser,
      config({ image_origins: IMAGES, agent_links: LINKS }),
    );
    try {
      await clickNow(page, '.card[data-id="050-config"]');
      await page.waitForFunction(() =>
        document.getElementById("d-doc").textContent.includes("Config fixture"),
      );
      // Images load lazily: scroll the document so the configured-origin attempt fires.
      await page.waitForFunction(() => document.querySelectorAll("#d-doc img").length === 2);
      await page.evaluate(() => {
        const d = document.getElementById("d-doc");
        d.scrollTop = d.scrollHeight;
      });
      await page.waitForFunction(() =>
        [...document.querySelectorAll("#d-doc img")].some(
          (img) =>
            img.getAttribute("src") === "/jobs/sample/050-config/shot.png" &&
            img.complete &&
            img.naturalWidth > 0,
        ),
      );
      await new Promise((resolve) => setTimeout(resolve, 300));
      const state = await page.evaluate(() => {
        const imgs = [...document.querySelectorAll("#d-doc img")].map((img) => ({
          src: img.getAttribute("src"),
          alt: img.alt,
          loaded: img.complete && img.naturalWidth > 0,
        }));
        const paragraphs = [...document.querySelectorAll("#d-doc p")].map((p) => p.textContent);
        const anchors = [...document.querySelectorAll("#d-doc a")].map((a) => ({
          href: a.getAttribute("href"),
          text: a.textContent,
        }));
        const links = ["d-pr", "d-paseo", "d-paseo-web"].map((id) => {
          const a = document.getElementById(id);
          return {
            id,
            hidden: a.hidden,
            href: a.getAttribute("href"),
            label: a.textContent,
            aria: a.getAttribute("aria-label"),
            target: a.target,
            rel: a.rel,
          };
        });
        return { imgs, paragraphs, anchors, links };
      });
      // The configured origin renders an <img>; CSP permits the attempt and the harness
      // blocks the actual fetch. The local image loads same-origin. Everything else is alt text.
      assert.deepEqual(
        state.imgs.map((i) => [i.src, i.alt, i.loaded]),
        [
          ["https://images.example.test/pic.png", "configured", false],
          ["/jobs/sample/050-config/shot.png", "local", true],
        ],
      );
      assert.ok(
        state.paragraphs.some((p) => p.includes("denied") && !p.includes("<")),
        "denied origin renders as alt text",
      );
      assert.ok(
        state.paragraphs.some((p) => p.includes("traversal")),
        "traversal renders as alt text",
      );
      assert.ok(
        state.paragraphs.some((p) => p.includes("scheme")),
        "data: image renders as alt text",
      );
      assert.deepEqual(
        state.anchors.filter((a) => a.text === "bad"),
        [],
        "javascript: link renders as text",
      );
      assert.ok(
        state.paragraphs.some((p) => p.includes("bad)")),
        "javascript: source stays visible text",
      );
      assert.ok(
        state.anchors.every((a) => !String(a.href).startsWith("javascript:")),
        "no javascript: anchor",
      );
      assert.deepEqual(state.links, [
        {
          id: "d-pr",
          hidden: true,
          href: null,
          label: "",
          aria: null,
          target: "_blank",
          rel: "noopener noreferrer",
        },
        {
          id: "d-paseo",
          hidden: false,
          href: "session://h/synthetic-server/agent/synthetic-agent",
          label: "Session app",
          aria: "Open this job's session in Session app",
          target: "",
          rel: "",
        },
        {
          id: "d-paseo-web",
          hidden: false,
          href: "https://sessions.example.test/synthetic-server/synthetic-agent",
          label: "Session",
          aria: "Open this job's session in Session",
          target: "_blank",
          rel: "noopener noreferrer",
        },
      ]);
      // Click interception: links record the click instead of navigating away.
      const clicked = await page.evaluate(() => {
        const seen = [];
        for (const id of ["d-paseo", "d-paseo-web"]) {
          const a = document.getElementById(id);
          a.addEventListener("click", (e) => {
            e.preventDefault();
            seen.push(id);
          });
        }
        window.__seen = seen;
        return true;
      });
      assert.equal(clicked, true);
      let popup = false;
      page.once("popup", () => {
        popup = true;
      });
      await page.click("#d-paseo");
      await page.click("#d-paseo-web");
      await new Promise((resolve) => setTimeout(resolve, 200));
      assert.deepEqual(await page.evaluate(() => window.__seen), ["d-paseo", "d-paseo-web"]);
      assert.equal(popup, false, "no external navigation or popup");
      assert.equal(new URL(page.url()).origin, origin);
      assert.deepEqual(
        attempts,
        ["https://images.example.test/pic.png"],
        "only the configured image origin is attempted",
      );
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  });
});

test("empty policy renders no link anchors and attempts no external requests", async () => {
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedAll(origin);
    const { context, page, attempts, errors } = await pageWithConfig(origin, browser, config());
    try {
      await clickNow(page, '.card[data-id="050-config"]');
      await page.waitForFunction(() =>
        document.getElementById("d-doc").textContent.includes("Config fixture"),
      );
      const state = await page.evaluate(() => ({
        links: ["d-paseo", "d-paseo-web"].map((id) => ({
          id,
          hidden: document.getElementById(id).hidden,
          href: document.getElementById(id).getAttribute("href"),
        })),
        imgs: [...document.querySelectorAll("#d-doc img")].map((img) => img.getAttribute("src")),
      }));
      assert.deepEqual(state.links, [
        { id: "d-paseo", hidden: true, href: null },
        { id: "d-paseo-web", hidden: true, href: null },
      ]);
      assert.deepEqual(
        state.imgs,
        ["/jobs/sample/050-config/shot.png"],
        "only same-origin job images render",
      );
      assert.deepEqual(attempts, [], "no external request attempts with empty policy");
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  });
});

test(
  "local fixtures own dynamic ports and reject an occupied fixed port",
  { timeout: 90000 },
  async () => {
    await prepareAssets();
    const dir = await mkdtemp(new URL("../../.context/startup-", import.meta.url).pathname);
    const services = [];
    const sockets = new Set();
    let server;
    try {
      const results = await Promise.allSettled(
        [0, 1].map(async (i) => {
          const service = await startLocal({ persistDir: `${dir}/r2-${i}` });
          services.push(service);
          return service;
        }),
      );
      assert.deepEqual(
        results.map((r) => r.status),
        ["fulfilled", "fulfilled"],
      );
      assert.notEqual(services[0].origin, services[1].origin);
      for (const service of services) {
        assert.match(service.origin, /^http:\/\/127\.0\.0\.1:[1-9]\d*$/);
        assert.deepEqual(await (await fetch(service.origin + "/projects.json")).json(), []);
      }
      await Promise.all(services.map((service) => service.close()));
      services.length = 0;

      let connections = 0;
      server = net.createServer((socket) => {
        connections++;
        sockets.add(socket);
        socket.on("close", () => sockets.delete(socket));
        socket.end("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\n[]");
      });
      await new Promise((resolve, reject) =>
        server.once("error", reject).listen(0, "127.0.0.1", resolve),
      );
      const held = server.address().port;
      await assert.rejects(
        startLocal({ port: held, persistDir: `${dir}/undeclared` }),
        /owned loopback ports/,
      );
      process.env.FACTORY_OWNED_LOCAL_PORT = String(held);
      await assert.rejects(
        startLocal({ port: held, persistDir: `${dir}/occupied` }),
        /exited during startup/,
      );
      assert.equal(connections, 0, "readiness must not contact the competing listener");
      assert.deepEqual(await (await fetch(`http://127.0.0.1:${held}/projects.json`)).json(), []);
      assert.equal(connections, 1, "competing listener remains available");
    } finally {
      delete process.env.FACTORY_OWNED_LOCAL_PORT;
      await Promise.all(services.map((service) => service.close()));
      for (const socket of sockets) socket.destroy();
      if (server?.listening)
        await new Promise((resolve, reject) =>
          server.close((error) => (error ? reject(error) : resolve())),
        );
      await rm(dir, { recursive: true, force: true });
    }
  },
);
