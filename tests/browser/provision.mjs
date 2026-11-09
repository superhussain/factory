// Explicit one-time browser provisioning for the pinned Chrome for Testing revision.
// Runtime and offline tests never download; they resolve this exact cache.
import { Browser, detectBrowserPlatform, install } from "@puppeteer/browsers";
import { PUPPETEER_REVISIONS } from "puppeteer-core/internal/revisions.js";
import { browserCache } from "../support/browser-server.mjs";

const platform = detectBrowserPlatform();
if (!platform) throw new Error("no Chrome for Testing build for this platform");
const buildId = PUPPETEER_REVISIONS.chrome;
console.log(`installing Chrome for Testing ${buildId} (${platform}) into ${browserCache.pathname}`);
const installed = await install({
  browser: Browser.CHROME,
  buildId,
  cacheDir: browserCache.pathname,
  platform,
  unpack: true,
});
console.log(`installed ${installed.executablePath}`);
