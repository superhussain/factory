// Paseo plugin card capture: bundles the real paseo-plugin/client/dashboard-screen.tsx
// with the owned stubs in tools/paseo-capture/ and screenshots its three cards (first
// login, stall, sign-in expired) at two widths in headless Chrome, with synthetic
// settings and every network request aborted. Never part of CI: run by hand with
// `node tools/capture-plugin-cards.mjs --capture <outdir> [--plugin <paseo-plugin dir>]`.
import { existsSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import * as esbuild from "esbuild";

const REPO = fileURLToPath(new URL("..", import.meta.url));
const DEFAULT_PLUGIN = REPO + "paseo-plugin";
const CARDS = ["first-login", "stall", "expired"];
const VIEWPORTS = [
  [1280, 800],
  [390, 844],
];

export async function bundleCards(pluginDir = DEFAULT_PLUGIN) {
  const screen = resolve(pluginDir, "client/dashboard-screen.tsx");
  if (!existsSync(screen)) throw new Error(`no client/dashboard-screen.tsx under ${pluginDir}`);
  if (!existsSync(REPO + "paseo-plugin/node_modules/react-dom"))
    throw new Error("react-dom missing: run pnpm --dir paseo-plugin install --frozen-lockfile");
  const stub = (name) => ({ path: REPO + `tools/paseo-capture/${name}` });
  const stubs = {
    name: "paseo-capture-stubs",
    setup(build) {
      build.onResolve({ filter: /^factory-capture:screen$/ }, () => ({ path: screen }));
      build.onResolve({ filter: /^react-native$/ }, () => stub("react-native.tsx"));
      build.onResolve({ filter: /^@getpaseo\/plugin(\/client(\/ui)?)?$/ }, () =>
        stub("paseo-plugin.tsx"),
      );
      build.onResolve({ filter: /^\.\/web$/ }, (args) =>
        args.importer.endsWith("dashboard-screen.tsx") ? stub("web.tsx") : undefined,
      );
    },
  };
  const { outputFiles, metafile } = await esbuild.build({
    entryPoints: [REPO + "tools/paseo-capture/entry.tsx"],
    absWorkingDir: REPO,
    bundle: true,
    write: false,
    metafile: true,
    format: "iife",
    platform: "browser",
    jsx: "automatic",
    define: { "process.env.NODE_ENV": '"production"' },
    nodePaths: [REPO + "paseo-plugin/node_modules"],
    plugins: [stubs],
  });
  return { code: outputFiles[0].text, inputs: Object.keys(metafile.inputs) };
}

async function capture(outdir, pluginDir) {
  const { code } = await bundleCards(pluginDir);
  const puppeteer = (await import("puppeteer-core")).default;
  const { browserExecutable } = await import("../tests/support/browser-server.mjs");
  const only = process.env.FACTORY_CAPTURE_VIEWS?.split(",");
  const probe = process.env.FACTORY_CAPTURE_PROBE_URL;
  const requests = [];
  await mkdir(outdir, { recursive: true });
  const browser = await puppeteer.launch({ executablePath: browserExecutable(), headless: true });
  try {
    for (const card of CARDS) {
      for (const [width, height] of VIEWPORTS) {
        const name = `plugin-card-${card}-${width}`;
        if (only && !only.includes(name)) continue;
        const context = await browser.createBrowserContext();
        const page = await context.newPage();
        await page.setViewport({ width, height, deviceScaleFactor: 1 });
        await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
        await page.setRequestInterception(true);
        page.on("request", (request) => {
          requests.push(request.url());
          request.abort();
        });
        await page.setContent(
          '<!doctype html><meta charset="utf-8"><style>html,body,#root{margin:0;height:100%}#root{display:flex;flex-direction:column}</style><div id="root"></div>' +
            (probe ? `<img alt="" src="${encodeURI(probe)}">` : "") +
            `<script>globalThis.FACTORY_CAPTURE_CARD=${JSON.stringify(card)}</script>` +
            `<script>${code.replaceAll("</script", "<\\/script")}</script>`,
        );
        await page.waitForSelector('[role="button"]', { timeout: 15000 });
        await page.evaluate(async () => {
          await document.fonts.ready;
          await new Promise((resolve) =>
            requestAnimationFrame(() => requestAnimationFrame(resolve)),
          );
        });
        const png = await page.screenshot();
        if (png.length >= 5 * 1024 * 1024) throw new Error(`${name}.png is 5 MiB or more`);
        if (requests.length)
          throw new Error(`requests during plugin card capture: ${requests.join(", ")}`);
        await writeFile(`${outdir}/${name}.png`, png);
        console.log(JSON.stringify({ file: `${name}.png`, bytes: png.length }));
        await context.close();
      }
    }
  } finally {
    await browser.close();
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const { values } = parseArgs({
    options: { capture: { type: "string" }, plugin: { type: "string" } },
  });
  if (!values.capture) {
    console.log(
      "usage: node tools/capture-plugin-cards.mjs --capture <outdir> [--plugin <paseo-plugin dir>]",
    );
    process.exit(2);
  }
  await capture(values.capture, values.plugin);
}
