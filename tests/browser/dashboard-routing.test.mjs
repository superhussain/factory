import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { clickNow } from "../support/board-ready.cjs";

const baseline = process.env.FACTORY_CAPTURE_BASELINE === "1";
const captureDir = process.env.FACTORY_CAPTURE_DIR;
const documents = {
  "decision.md": "# Routing decision\n\nThe synthetic dashboard decision is ready for review.\n",
  "spec.md":
    "# Routing specification\n\nThe synthetic dashboard specification is distinct from the decision.\n",
};

async function seed(origin) {
  await seedProject(
    origin,
    "sample",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
  );
  await seedBoard(origin, [
    {
      id: "020-example",
      project: "sample",
      title: "Synthetic routing example",
      stage: "review",
      updated: "2026-01-01T00:00:00Z",
    },
  ]);
  for (const [name, body] of Object.entries(documents)) {
    const path = origin + "/api/jobs/sample/020-example/" + name;
    const current = await fetch(path);
    assert.ok(current.ok || current.status === 404);
    const condition = current.ok
      ? { "If-Match": current.headers.get("ETag") }
      : { "If-None-Match": "*" };
    const response = await fetch(path, {
      method: "PUT",
      headers: { ...condition, "Content-Type": "text/markdown" },
      body,
    });
    assert.equal(response.status, 200);
  }
}

async function selected(page, name) {
  await page.waitForFunction(
    (file) => {
      const tab = document.querySelector("#d-tabs .tab.active");
      return (
        !document.getElementById("detail").hidden &&
        tab?.dataset.name === file &&
        document
          .getElementById("d-doc")
          .textContent.includes(
            file === "decision.md" ? "Routing decision" : "Routing specification",
          )
      );
    },
    {},
    name,
  );
  assert.equal(
    decodeURIComponent(new URL(page.url()).hash),
    "#020-example/" + name.replace(/\.md$/, ""),
  );
}

function canonical(page, origin, search) {
  const url = new URL(page.url());
  assert.equal(url.origin, origin);
  assert.equal(url.pathname, "/");
  assert.equal(url.search, search);
}

test(
  "root and historical dashboard links preserve document selection and history",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    if (captureDir) await mkdir(captureDir, { recursive: true });
    await withBrowserFixture(async ({ browser, origin }) => {
      await seed(origin);
      for (const [view, width, height, colorScheme] of [
        ["desktop", 1440, 900, "light"],
        ["mobile", 390, 844, "dark"],
      ]) {
        const context = await browser.createBrowserContext();
        try {
          const page = await context.newPage();
          await page.setViewport({ width, height, deviceScaleFactor: 1 });
          await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: colorScheme }]);
          await page.evaluateOnNewDocument(() => {
            const RealDate = Date;
            window.Date = class extends RealDate {
              constructor(...args) {
                super(...(args.length ? args : ["2026-01-01T12:00:00Z"]));
              }
              static now() {
                return new RealDate("2026-01-01T12:00:00Z").getTime();
              }
            };
          });
          const external = [],
            errors = [];
          page.on("pageerror", (error) => errors.push(String(error)));
          await page.setRequestInterception(true);
          page.on("request", (request) => {
            if (new URL(request.url()).origin !== origin) {
              external.push(request.url());
              return request.abort();
            }
            return request.continue();
          });
          const links = baseline
            ? ["/dashboard.html?view=a%2Fb#020-example/decision"]
            : [
                "/?view=a%2Fb#020-example/decision",
                "/dashboard.html?view=a%2Fb#020-example/decision",
                "/#%30%32%30-example/decision",
                "/dashboard.html#%30%32%30-example/decision",
              ];
          for (const [index, link] of links.entries()) {
            const search = new URL(origin + link).search;
            await page.goto(origin + link, { waitUntil: "domcontentloaded" });
            await selected(page, "decision.md");
            if (!baseline) canonical(page, origin, search);
            await page.reload({ waitUntil: "domcontentloaded" });
            await selected(page, "decision.md");
            if (!baseline) canonical(page, origin, search);
            if (captureDir && index === 0) {
              await page.evaluate(() => document.fonts.ready);
              await page.screenshot({ path: join(captureDir, `decision-${view}.png`) });
            }
          }
          if (!baseline) {
            await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
            await page.waitForSelector('.card[data-id="020-example"]');
            await clickNow(page, '.card[data-id="020-example"]');
            await page.waitForSelector('#d-tabs .tab[data-name="spec.md"]');
            await page.click('#d-tabs .tab[data-name="spec.md"]');
            await selected(page, "spec.md");
            canonical(page, origin, "");
            await page.goBack();
            await page.waitForFunction(() => document.getElementById("detail").hidden);
            canonical(page, origin, "");
            assert.equal(new URL(page.url()).hash, "");
            await page.goForward();
            await selected(page, "spec.md");
            canonical(page, origin, "");
          }
          assert.deepEqual(external, [], "no external requests");
          assert.deepEqual(errors, [], "no page errors");
        } finally {
          await context.close();
        }
      }
    });
  },
);
