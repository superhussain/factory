// Settings, Display, Install app against one owned local Worker. A stub event stands in for Chrome's beforeinstallprompt.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";

const JOBS = [
  {
    id: "001-sample",
    project: "sample",
    title: "Synthetic sample job",
    stage: "spec",
    updated: "2026-01-01T00:00:00Z",
  },
];
const HINT = {
  installed: "This dashboard is open as an installed app.",
  ready: "Install this dashboard as an app on this device.",
  ios: "Tap Share, then Add to Home Screen.",
  none: "This browser has not offered an install yet. In Chrome or Edge, look for Install in the address bar or the browser menu.",
};
const IPHONE =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
const wait = { polling: 50 };

test("install app", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const errors = [];
    const open = async (setup) => {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 900 });
      page.on("pageerror", (error) => errors.push(String(error)));
      await blockLiveEvents(page);
      if (setup) await setup(page);
      await page.goto(origin + "/#/settings/display", { waitUntil: "domcontentloaded" });
      await page.waitForSelector("#settings-display:not([hidden])", wait);
      return page;
    };
    const dispatch = (page, outcome) =>
      page.evaluate((outcome) => {
        const e = new Event("beforeinstallprompt", { cancelable: true });
        window.prompts = 0;
        e.prompt = async () => {
          window.prompts += 1;
        };
        e.userChoice =
          outcome === "reject" ? Promise.reject(new Error("no")) : Promise.resolve({ outcome });
        window.lastInstall = e;
        dispatchEvent(e);
      }, outcome);
    const text = (page, selector) => page.$eval(selector, (el) => el.textContent);
    const hidden = (page) => page.$eval("#display-install", (el) => el.hidden);
    const status = (page, line) =>
      page.waitForFunction(
        (line) => document.getElementById("display-install-status").textContent === line,
        wait,
        line,
      );

    await t.test("install I1 none", async () => {
      const page = await open();
      assert.equal(await text(page, "#display-install-hint"), HINT.none);
      assert.equal(await hidden(page), true);
    });

    await t.test("install I2 ready and accepted", async () => {
      const page = await open();
      await dispatch(page, "accepted");
      assert.equal(await hidden(page), false);
      assert.equal(await text(page, "#display-install"), "Install app");
      assert.equal(await text(page, "#display-install-hint"), HINT.ready);
      await page.click("#display-install");
      await status(page, "Installing. Open it from your home screen or app list.");
      assert.equal(await page.evaluate(() => window.prompts), 1);
      assert.equal(await hidden(page), true);
      assert.equal(await page.evaluate(() => document.activeElement.id), "display-install-heading");
      assert.equal(await page.evaluate(() => window.lastInstall.defaultPrevented), false);
    });

    await t.test("install I3 dismissed", async () => {
      const page = await open();
      await dispatch(page, "dismissed");
      await page.click("#display-install");
      await status(page, "Install cancelled.");
      assert.equal(await text(page, "#display-install-hint"), HINT.none);
    });

    await t.test("install I4 ios", async () => {
      const page = await open((page) => page.setUserAgent(IPHONE));
      assert.equal(await text(page, "#display-install-hint"), HINT.ios);
      assert.equal(await hidden(page), true);
    });

    await t.test("install I5 installed", async () => {
      const page = await open((page) =>
        page.evaluateOnNewDocument(() =>
          Object.defineProperty(navigator, "standalone", { value: true }),
        ),
      );
      assert.equal(await text(page, "#display-install-hint"), HINT.installed);
      assert.equal(await hidden(page), true);
      await dispatch(page, "accepted");
      assert.equal(await text(page, "#display-install-hint"), HINT.installed);
      assert.equal(await hidden(page), true);
    });

    assert.deepEqual(errors, []);
  });
});
