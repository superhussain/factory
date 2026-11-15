// The four header icon buttons stay on one row and match the project filter height.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const JOBS = [
  {
    id: "001-sample",
    project: "sample",
    title: "Synthetic sample job",
    stage: "spec",
  },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));

const TOUCH = { isMobile: true, hasTouch: true };
const CASES = [
  { viewport: { width: 1280, height: 900 }, size: 36 },
  { viewport: { width: 390, height: 844 }, size: 32 },
  { viewport: { width: 320, height: 844 }, size: 32 },
  { viewport: { width: 390, height: 844, ...TOUCH }, size: 44 },
  { viewport: { width: 320, height: 844, ...TOUCH }, size: 44 },
];

test("header icons share one row and match the filter height", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    for (const { viewport, size } of CASES) {
      const label = `${viewport.width}px${viewport.hasTouch ? " touch" : ""}`;
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport(viewport);
      // A date west of UTC is the widest status text.
      await page.emulateTimezone("America/New_York");
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector("#new-idea:not([hidden])");
      await page.waitForSelector(".updated-word");
      const header = await page.evaluate(() => {
        const rect = (id) => document.getElementById(id).getBoundingClientRect();
        const icons = ["new-idea", "job-jump", "retros-link", "settings-link"];
        const el = document.querySelector("header");
        const boxes = [...el.children]
          .map((child) => child.getBoundingClientRect())
          .filter((box) => box.height > 0)
          .sort((a, b) => a.top - b.top);
        let rows = 0;
        let bottom = -Infinity;
        for (const box of boxes) {
          if (box.top >= bottom) rows++;
          bottom = Math.max(bottom, box.bottom);
        }
        return {
          tops: icons.map((id) => Math.round(rect(id).top)),
          heights: [...icons, "filter"].map((id) => rect(id).height),
          scrollWidth: el.scrollWidth,
          clientWidth: el.clientWidth,
          rows,
          headerHeight: el.getBoundingClientRect().height,
          wordShown: document.querySelector(".updated-word").getClientRects().length > 0,
          text: document.getElementById("updated").textContent,
        };
      });
      if (process.env.HEADER_SHOTS && label === "320px") {
        await (
          await page.$("header")
        ).screenshot({
          path: `${process.env.HEADER_SHOTS}/${process.env.HEADER_SHOT_STAGE || "after"}-header-320.png`,
        });
      }
      assert.equal(new Set(header.tops).size, 1, `icon tops at ${label}: ${header.tops}`);
      assert.deepEqual(header.heights, [size, size, size, size, size], `heights at ${label}`);
      assert.ok(
        header.scrollWidth <= header.clientWidth,
        `header scrolls at ${label}: ${header.scrollWidth} > ${header.clientWidth}`,
      );
      assert.ok(header.rows <= 2, `third header row at ${label}: height ${header.headerHeight}`);
      assert.equal(header.wordShown, viewport.width !== 320, `updated word shown at ${label}`);
      assert.ok(header.text.startsWith("updated "), `updated text at ${label}: ${header.text}`);
      if (viewport.width === 1280) {
        const link = await page.$eval("#retros-link", (el) => ({
          next: el.nextElementSibling.id,
          previous: el.previousElementSibling.id,
          href: el.getAttribute("href"),
          label: el.getAttribute("aria-label"),
          title: el.title,
          keys: el.getAttribute("aria-keyshortcuts"),
          svg: [
            el.querySelector("svg").getBoundingClientRect().width,
            el.querySelector("svg").getBoundingClientRect().height,
          ],
        }));
        assert.deepEqual(link, {
          next: "settings-link",
          previous: "job-jump",
          href: "#/retros",
          label: "Retros",
          title: "Retros (R)",
          keys: "r",
          svg: [16, 16],
        });
      }
      if (viewport.hasTouch) {
        await page.evaluate(() => (location.hash = "#/settings"));
        await page.waitForFunction(
          () => document.getElementById("settings-back").getClientRects().length > 0,
        );
        const back = await page.evaluate(
          () => document.getElementById("settings-back").getBoundingClientRect().height,
        );
        assert.equal(back, 44, `settings-back height at ${label}`);
      }
    }
  });
});

// A first board read that fails with a 5xx or a body that is not JSON says why in full.
test("a failed first load shows the unreachable banner", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const failures = [
      { status: 503, contentType: "text/plain", body: "unavailable" },
      { status: 200, contentType: "application/json", body: "not json" },
    ];
    for (const failure of failures) {
      const label = `${failure.status} ${failure.body}`;
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 320, height: 844, ...TOUCH });
      await blockLiveEvents(page);
      await page.setBypassServiceWorker(true);
      await page.setRequestInterception(true);
      let failing = true;
      page.on("request", (request) =>
        failing && new URL(request.url()).pathname === "/board.json"
          ? request.respond(failure)
          : request.continue(),
      );
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector("#conn-banner:not([hidden])");
      const banner = await page.evaluate(() => ({
        text: document.getElementById("conn-text").textContent,
        signin: document.getElementById("conn-signin").hidden,
        scrollWidth: document.documentElement.scrollWidth,
        clientWidth: document.documentElement.clientWidth,
      }));
      assert.equal(
        banner.text,
        "Can't reach the factory. The board loads when it answers.",
        `banner text after ${label}`,
      );
      assert.equal(banner.signin, true, `sign-in button after ${label}`);
      assert.ok(
        banner.scrollWidth <= banner.clientWidth,
        `page scrolls after ${label}: ${banner.scrollWidth} > ${banner.clientWidth}`,
      );
      failing = false;
      assert.equal(await page.evaluate(() => poll()), true, `poll after ${label}`);
      assert.equal(
        await page.$eval("#conn-banner", (el) => el.hidden),
        true,
        `banner after a good read following ${label}`,
      );
    }
  });
});
