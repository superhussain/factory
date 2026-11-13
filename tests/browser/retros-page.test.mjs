// The retros page against one owned local Worker. Retro runs and the backlog text are synthetic
// and served by request interception, so no run is stored. RETROS_SHOTS names a folder for captures.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const FP = (n) => String(n).repeat(16);
const job = (id, stage, fingerprint, extra = {}) => ({
  id,
  project: "sample-tools",
  title: `Synthetic ${id}`,
  stage,
  updated: "2026-01-01T00:00:00Z",
  ...(fingerprint ? { origin: { fingerprint } } : {}),
  ...extra,
});
const JOBS = [
  job("1490-guard-the-ls-alias", "merged", "0123456789abcdef"),
  job("1491-open-work", "build", FP(1)),
  job("1492-denied-work", "closed", FP(2), {
    backlog: { line: "- [ ] x: y", decision: "denied" },
  }),
  job("1493-closed-work", "closed", FP(3)),
  job("1494-checked-line", "merged"),
];
const mark = (n) =>
  `  Retro: run 20261112T090000Z-a1b2c3, fingerprint ${FP(n)}, created 2026-11-12T09:00:00Z`;
const BACKLOG = [
  "# Backlog",
  `- [x] sample-tools: Guard the ls alias (1490-guard-the-ls-alias)`,
  mark(0).replace(FP(0), "0123456789abcdef"),
  `- [x] sample-tools: Checked line card (1494-checked-line)`,
  mark(4),
  `- [x] sample-tools: Gone card (1400-gone)`,
  mark(7),
  `- [ ] sample-tools: Waiting card`,
  mark(5),
  "",
].join("\n");
const card = (title, fingerprint, result = "admitted") => ({
  title,
  project: "sample-tools",
  fingerprint,
  result,
});
const finding = (rank, title, cards, extra = {}) => ({
  rank,
  title,
  category: "tool-economy",
  count: 253,
  cost: 250,
  project: "sample-tools",
  root_cause: "The colorls alias needs a terminal size.",
  fix: "Alias ls only in interactive shells.",
  cards,
  ...extra,
});
const RUNS = [
  {
    schema: "factory.retro-run/1",
    run: "20261112T090000Z-a1b2c3",
    host: "mac",
    from: "2026-11-05",
    to: "2026-11-11",
    sessions: 40,
    rejected: 0,
    findings: [
      finding(1, "ls alias crashes in non-interactive shells", [
        card("Guard the ls alias", "0123456789abcdef"),
      ]),
    ],
    cards: [],
  },
  {
    schema: "factory.retro-run/1",
    run: "20261105T090000Z-d4e5f6",
    host: "mac",
    from: "2026-10-29",
    to: "2026-11-04",
    sessions: 31,
    rejected: 1,
    findings: [
      finding(
        1,
        "Reviewers re-read the same files",
        [
          card("Open work card", FP(1)),
          card("Denied work card", FP(2)),
          card("Closed work card", FP(3)),
          card("Waiting card", FP(5)),
        ],
        { count: 1000, cost: 1000 },
      ),
      finding(2, "Checked and waiting cards", [
        card("Checked line card", FP(4)),
        card("Waiting card", FP(5)),
        card("Duplicate card", FP(6), "duplicate 1490-guard-the-ls-alias"),
      ]),
      finding(3, "Gone and unlogged cards", [
        card("Gone card", FP(7)),
        card("Capped card", FP(8), "daily cap"),
      ]),
      finding(4, "A finding with no card", [], { count: 1, cost: 100 }),
    ],
    cards: [card("Loose card", FP(9), "duplicate 1490-guard-the-ls-alias")],
  },
];
const wait = { polling: 50 };

test("retros page", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample-tools",
      "---\nrepo: example/sample-tools\nbase: main\ntitle: Synthetic tools\n---\n",
    );
    await seedBoard(origin, JOBS);
    let retros = { status: 200, body: JSON.stringify(RUNS) };
    const openPage = async (hash, viewport = { width: 1280, height: 900 }, scheme = "light") => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport(viewport);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      await page.setRequestInterception(true);
      page.on("request", (request) => {
        const { pathname } = new URL(request.url());
        if (pathname === "/api/retros")
          return request.respond({
            ...retros,
            contentType: "application/json",
          });
        if (pathname === "/api/backlog")
          return request.respond({
            status: 200,
            contentType: "text/plain",
            body: BACKLOG,
          });
        return request.continue();
      });
      await page.goto(origin + "/" + hash, { waitUntil: "domcontentloaded" });
      return { page, errors };
    };
    const rows = (page) =>
      page.$$eval("#retros-list .retro-finding", (items) =>
        items.map((item) =>
          [".retro-rank", ".retro-title", ".retro-share-value"]
            .map((selector) => item.querySelector(selector).textContent)
            .concat(
              [...item.querySelectorAll(".retro-cards > *")]
                .map((c) => c.lastChild.textContent)
                .join(""),
            ),
        ),
      );

    await t.test("each status resolves with its link", async () => {
      const { page, errors } = await openPage("#/retros");
      await page.waitForSelector("#retros-list .retro-finding", wait);
      await page.waitForFunction(
        () => document.querySelectorAll("#retros-list .retro-summary").length === 2,
        wait,
      );
      assert.match(await page.title(), /^Retros · /);
      assert.equal(await page.$eval("#retros-page-heading", (el) => el.textContent), "Retros");
      assert.deepEqual(
        await page.$$eval("#retros-list .retro-summary", (els) =>
          els.map((el) => [...el.querySelectorAll(".chip")].map((chip) => chip.textContent)),
        ),
        [
          ["1 merged"],
          ["3 merged", "1 open", "1 waiting", "1 denied", "1 closed", "1 archived", "1 not logged"],
        ],
      );
      const table = await rows(page);
      assert.deepEqual(
        table[0].slice(0, 2).map((c) => c.slice(0, 20)),
        ["1", "ls alias crashes in "],
      );
      assert.equal(table[0][2], "100%");
      assert.equal(table[0][3], "Merged");
      assert.deepEqual(
        table.slice(1).map((r) => r[2]),
        ["89%", "6%", "6%", "<1%"],
      );
      assert.deepEqual(
        table.slice(1).map((r) => r[3]),
        [
          "Open · buildDeniedClosedWaiting",
          "MergedWaitingMerged",
          "ArchivedNot logged: daily cap",
          "No card",
        ],
      );
      assert.equal(
        await page.$$eval("#retros-list .retro-share", (els) =>
          els.some((el) => el.textContent.includes(",")),
        ),
        false,
      );
      assert.equal(
        await page.$eval("#retro-20261105T090000Z-d4e5f6-4 .retro-cards .chip", (el) =>
          el.classList.contains("is-none"),
        ),
        true,
      );
      const links = await page.$$eval("#retros-list .retro-cards a", (as) =>
        as.map((a) => [a.lastChild.textContent, a.getAttribute("href")]),
      );
      assert.deepEqual(links[0], ["Merged", "#1490-guard-the-ls-alias"]);
      const waiting = links.find(([text]) => text === "Waiting");
      assert.match(waiting[1], /^#\/backlog\/[0-9a-f]{12}$/);
      assert.equal(
        links.some(([text]) => text === "Archived"),
        false,
      );
      assert.equal(
        await page.$eval("#retro-h-20261112T090000Z-a1b2c3", (el) => el.textContent),
        "Nov 12, 2026, 09:00 UTC",
      );
      assert.equal(
        await page.$eval(
          "#retro-run-20261112T090000Z-a1b2c3 .retro-run-meta",
          (el) => el.textContent,
        ),
        "mac · Nov 5 to Nov 11 · 40 sessions · 20261112T090000Z-a1b2c3",
      );
      assert.match(
        await page.$eval(".retro-findings-head", (el) => el.textContent),
        /Share of estimated cost/,
      );
      const unlinked = await page.$eval("#retros-list .retro-unlinked", (el) => el.textContent);
      assert.match(unlinked, /^Not linked to a finding/);
      assert.match(unlinked, /Loose card\s*Merged/);
      assert.equal(await page.$("#retros-list h4"), null);
      assert.equal(
        await page.$eval(
          "#retro-20261105T090000Z-d4e5f6-3 .retro-cards > :first-child",
          (el) => el.textContent,
        ),
        "Gone card: Archived",
      );
      assert.equal(await page.$("#retros-list .retro-unlinked .sr-only"), null);
      const summary = await page.$("#retros-list .retro-why summary");
      assert.equal(await summary.evaluate((el) => el.textContent), "Root cause and fix");
      assert.ok(await summary.$("svg"));
      assert.equal(await summary.evaluate((el) => getComputedStyle(el).listStyleType), "none");
      await summary.click();
      assert.equal(await summary.evaluate((el) => el.parentElement.open), true);
      assert.deepEqual(
        await summary.evaluate((el) =>
          [...el.parentElement.querySelectorAll(".retro-why-label")].map((l) => l.textContent),
        ),
        ["Root cause", "Fix"],
      );
      assert.deepEqual(errors, []);
      await page.browserContext().close();
    });

    await t.test("a run and rank route opens that finding", async () => {
      const { page } = await openPage("#/retros/20261105T090000Z-d4e5f6/2");
      await page.waitForSelector("#retro-20261105T090000Z-d4e5f6-2[aria-current]", wait);
      assert.equal(
        await page.$eval("#retro-20261105T090000Z-d4e5f6-2 details", (el) => el.open),
        true,
      );
      await page.browserContext().close();
    });

    await t.test("a phone width has no sideways scroll", async () => {
      const { page } = await openPage("#/retros", { width: 390, height: 844 });
      await page.waitForSelector("#retros-list .retro-finding", wait);
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
        ),
        true,
      );
      assert.equal(
        await page.$eval("#retro-run-20261105T090000Z-d4e5f6 .retro-share", (el) => el.textContent),
        "89% of estimated cost",
      );
      await page.browserContext().close();
    });

    await t.test("r on the board and the Retros jumper command open the page", async () => {
      const { page } = await openPage("");
      await page.waitForSelector(".card", wait);
      await page.keyboard.press("r");
      await page.waitForSelector("#retros-page:not([hidden])", wait);
      assert.equal(await page.evaluate(() => location.hash), "#/retros");
      assert.equal(await page.evaluate(() => document.body.dataset.view), "retros");
      await page.keyboard.press("r");
      assert.equal(await page.evaluate(() => location.hash), "#/retros");
      await page.click("#retros-back");
      await page.waitForSelector("#retros-page", { hidden: true, ...wait });
      assert.equal(await page.evaluate(() => document.body.dataset.view), "board");
      await page.keyboard.press("j");
      await page.waitForSelector("#job-picker[open]", wait);
      await page.type("#job-search", "retros");
      const option = await page.$eval(".job-option", (el) => el.textContent);
      assert.equal(option, "RetrosCommand · Open retro runs and findings (R)");
      await page.keyboard.press("Enter");
      await page.waitForSelector("#retros-page:not([hidden])", wait);
      assert.equal(await page.evaluate(() => location.hash), "#/retros");
      await page.browserContext().close();
    });

    await t.test("the top bar Retros link opens the page and shows it as current", async () => {
      const { page } = await openPage("");
      await page.waitForSelector(".card", wait);
      await page.click("#retros-link");
      await page.waitForSelector("#retros-page:not([hidden])", wait);
      assert.equal(await page.evaluate(() => location.hash), "#/retros");
      assert.equal(
        await page.$eval("#retros-link", (el) => el.getAttribute("aria-current")),
        "page",
      );
      await page.click("#retros-back");
      await page.waitForSelector("#retros-page", { hidden: true, ...wait });
      assert.equal(await page.$eval("#retros-link", (el) => el.getAttribute("aria-current")), null);
      assert.equal(await page.evaluate(() => document.activeElement.id), "retros-link");
      await page.browserContext().close();
    });

    await t.test("going from retros to settings does not focus the Retros link", async () => {
      const { page } = await openPage("#/retros");
      await page.waitForSelector("#retros-page:not([hidden])", wait);
      await page.click("#settings-link");
      await page.waitForSelector("#settings-page:not([hidden])", wait);
      assert.notEqual(await page.evaluate(() => document.activeElement.id), "retros-link");
      await page.browserContext().close();
    });

    await t.test("empty and failed loads say so", async () => {
      retros = { status: 200, body: "[]" };
      let { page } = await openPage("#/retros");
      await page.waitForFunction(
        () =>
          document.querySelector("#retros-status").textContent ===
          "No retro runs yet. Each retro admit uploads its findings here.",
        wait,
      );
      await page.browserContext().close();
      retros = { status: 500, body: "boom" };
      ({ page } = await openPage("#/retros"));
      await page.waitForFunction(
        () =>
          document.querySelector("#retros-status").textContent ===
          "Could not load retros. Reload to try again.",
        wait,
      );
      await page.browserContext().close();
      retros = { status: 200, body: JSON.stringify(RUNS) };
    });

    await t.test("captures", async () => {
      if (!process.env.RETROS_SHOTS) return;
      await mkdir(process.env.RETROS_SHOTS, { recursive: true });
      for (const [name, viewport, scheme] of [
        ["desktop", { width: 1280, height: 900 }, "light"],
        ["mobile", { width: 390, height: 844 }, "light"],
        ["dark", { width: 1280, height: 900 }, "dark"],
      ]) {
        const { page } = await openPage("#/retros", viewport, scheme);
        await page.waitForSelector("#retros-list .retro-finding", wait);
        await page.screenshot({
          path: `${process.env.RETROS_SHOTS}/after-retros-${name}.png`,
          fullPage: true,
        });
        await page.browserContext().close();
      }
      const open = await openPage("#/retros/20261105T090000Z-d4e5f6/2");
      await open.page.waitForSelector("#retro-20261105T090000Z-d4e5f6-2[aria-current]", wait);
      await (
        await open.page.$("#retro-run-20261105T090000Z-d4e5f6")
      ).screenshot({ path: `${process.env.RETROS_SHOTS}/after-retros-open.png` });
      await open.page.browserContext().close();
      const { page } = await openPage("#/settings/reference");
      await page.waitForSelector("#settings-reference:not([hidden])", wait);
      await (
        await page.$("#reference-keys")
      ).screenshot({
        path: `${process.env.RETROS_SHOTS}/after-reference-keys.png`,
      });
      await page.browserContext().close();
    });
  });
});
