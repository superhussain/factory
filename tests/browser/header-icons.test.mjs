// The six header icon buttons stay on one row and match the project filter height.
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
  {
    id: "1901-sample-a",
    project: "sample",
    title: "Synthetic sample A",
    stage: "needs-human",
  },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));

const TOUCH = { isMobile: true, hasTouch: true };
const CASES = [
  { viewport: { width: 1280, height: 900 }, size: 36 },
  { viewport: { width: 390, height: 844 }, size: 32 },
  { viewport: { width: 320, height: 844 }, size: 32 },
  { viewport: { width: 390, height: 844, ...TOUCH }, size: 44 },
  { viewport: { width: 320, height: 844, ...TOUCH }, size: 44 },
  { viewport: { width: 700, height: 1024, ...TOUCH }, size: 44 },
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
      await page.waitForSelector("#inbox-count:not([hidden])");
      await page.waitForSelector(".updated-word");
      const header = await page.evaluate(() => {
        const rect = (id) => document.getElementById(id).getBoundingClientRect();
        const icons = [
          "inbox-link",
          "notifications-button",
          "retros-link",
          "settings-link",
          "new-idea",
          "job-jump",
        ];
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
          wordShown: document.querySelector(".updated-word").getBoundingClientRect().width > 1,
          wordRead: (() => {
            const style = getComputedStyle(document.querySelector(".updated-word"));
            return style.display !== "none" && style.visibility !== "hidden";
          })(),
          text: document.getElementById("updated").textContent,
          order: getComputedStyle(document.querySelector(".header-actions")).order,
          children: [...document.querySelector(".header-actions").children].map((c) => c.id),
        };
      });
      const shots = { "320px": "320", "700px touch": "700-touch", "390px touch": "390-touch" };
      if (process.env.HEADER_SHOTS && shots[label]) {
        await (
          await page.$("header")
        ).screenshot({
          path: `${process.env.HEADER_SHOTS}/${process.env.HEADER_SHOT_STAGE || "after"}-header-${shots[label]}.png`,
        });
      }
      assert.equal(new Set(header.tops).size, 1, `icon tops at ${label}: ${header.tops}`);
      assert.deepEqual(
        header.heights,
        [size, size, size, size, size, size, size],
        `heights at ${label}`,
      );
      assert.ok(
        header.scrollWidth <= header.clientWidth,
        `header scrolls at ${label}: ${header.scrollWidth} > ${header.clientWidth}`,
      );
      assert.ok(header.rows <= 2, `third header row at ${label}: height ${header.headerHeight}`);
      assert.equal(header.wordShown, viewport.width !== 320, `updated word shown at ${label}`);
      assert.equal(header.wordRead, true, `updated word read at ${label}`);
      assert.ok(header.text.startsWith("updated "), `updated text at ${label}: ${header.text}`);
      assert.deepEqual(
        header.children,
        [
          "inbox-link",
          "notifications-wrap",
          "retros-link",
          "settings-link",
          "new-idea",
          "job-jump",
        ],
        `header order at ${label}`,
      );
      if (label === "700px touch") {
        assert.equal(header.rows, 1, `header rows at ${label}: height ${header.headerHeight}`);
        assert.equal(header.order, "0", `header-actions order at ${label}`);
      }
      if (label === "390px touch")
        assert.equal(header.order, "1", `header-actions order at ${label}`);
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
          previous: "notifications-wrap",
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

// Two runs shaped like RUNS in retros-page.test.mjs, so the Retros badge shows 2.
const RETROS = ["20261112T090000Z-a1b2c3", "20261105T090000Z-d4e5f6"].map((run) => ({
  schema: "factory.retro-run/1",
  run,
  host: "mac",
  from: "2026-10-29",
  to: "2026-11-11",
  sessions: 40,
  rejected: 0,
  findings: [],
  cards: [],
}));

test("the three header badges match", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    await page.setRequestInterception(true);
    page.on("request", (request) =>
      new URL(request.url()).pathname === "/api/retros"
        ? request.respond({
            status: 200,
            contentType: "application/json",
            body: JSON.stringify(RETROS),
          })
        : request.continue(),
    );
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    const badges = ["inbox-count", "notifications-count", "retros-count"];
    await page.waitForFunction(
      (ids) => ids.every((id) => !document.getElementById(id).hidden),
      {},
      badges,
    );
    const looks = await page.evaluate(
      (ids) =>
        ids.map((id) => {
          const el = document.getElementById(id);
          const style = getComputedStyle(el);
          const box = el.getBoundingClientRect();
          const parent = el.parentElement.getBoundingClientRect();
          return {
            text: el.textContent,
            width: style.width,
            height: style.height,
            background: style.backgroundColor,
            color: style.color,
            fontSize: style.fontSize,
            radius: style.borderRadius,
            offset: [box.top - parent.top, parent.right - box.right],
          };
        }),
      badges,
    );
    assert.deepEqual(
      looks.map((l) => l.text),
      ["1", "1", "2"],
    );
    for (const look of looks.slice(1))
      assert.deepEqual({ ...look, text: "" }, { ...looks[0], text: "" });
    await context.close();
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
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
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
      await context.close();
    }
  });
});

// WCAG AA asks 4.5:1 for the badge's small count text, in both color schemes.
test("the inbox badge keeps 4.5:1 contrast in both themes", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    for (const theme of ["light", "dark"]) {
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport({ width: 390, height: 844 });
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector("#inbox-count:not([hidden])");
      const { color, background } = await page.$eval("#inbox-count", (el) => ({
        color: getComputedStyle(el).color,
        background: getComputedStyle(el).backgroundColor,
      }));
      const luminance = (css) => {
        const [r, g, b] = css
          .match(/[\d.]+/g)
          .slice(0, 3)
          .map((n) => {
            const c = Number(n) / 255;
            return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
          });
        return 0.2126 * r + 0.7152 * g + 0.0722 * b;
      };
      const [light, dark] = [luminance(color), luminance(background)].sort((a, b) => b - a);
      const ratio = (light + 0.05) / (dark + 0.05);
      assert.ok(ratio >= 4.5, `${theme} badge ${color} on ${background}: ${ratio.toFixed(2)}:1`);
      await context.close();
    }
  });
});
