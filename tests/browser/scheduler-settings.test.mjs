// Scheduler settings section against one owned local Worker. A stub clipboard records copies.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const JOBS = [
  {
    id: "001-sample",
    project: "sample",
    title: "Synthetic sample job",
    stage: "spec",
    updated: "2026-01-01T00:00:00Z",
  },
];
const wait = { polling: 50 };

test("scheduler settings", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  const pickup = await readFile(
    new URL("../../skills/factory/references/pickup.md", import.meta.url),
    "utf8",
  );
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
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.evaluateOnNewDocument(() => {
      window.copied = [];
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: {
          writeText: async (text) => {
            if (window.__copyFails) throw new Error("denied");
            window.copied.push(text);
          },
        },
      });
    });
    const snippet = (name) => page.$eval(`#scheduler-snippet-${name}`, (el) => el.textContent);
    const selected = (id) => page.$eval(`#${id}`, (el) => el.getAttribute("aria-selected"));
    const visiblePanels = () =>
      page.$$eval("[id^=scheduler-panel-]", (els) =>
        els.filter((el) => !el.hidden).map((el) => el.id),
      );
    const status = () => page.$eval("#scheduler-status", (el) => el.textContent);

    await page.goto(origin + "/#/settings/scheduler", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#settings-scheduler:not([hidden])", wait);

    await t.test("scheduler K1 route, nav and focus", async () => {
      assert.equal(await page.$eval("#settings-notifications", (el) => el.hidden), true);
      assert.equal(
        await page.$eval("#settings-nav-scheduler", (el) => el.getAttribute("aria-current")),
        "page",
      );
      await page.waitForFunction(
        () => document.activeElement?.id === "scheduler-settings-heading",
        wait,
      );
      assert.equal(await status(), "");
    });

    await t.test("scheduler K2 tabs", async () => {
      assert.equal(await selected("scheduler-tab-cron"), "true");
      assert.deepEqual(await visiblePanels(), ["scheduler-panel-cron"]);
      await page.focus("#scheduler-tab-cron");
      await page.keyboard.press("ArrowRight");
      assert.equal(await selected("scheduler-tab-launchd"), "true");
      assert.deepEqual(await visiblePanels(), ["scheduler-panel-launchd"]);
      await page.keyboard.press("End");
      assert.equal(await selected("scheduler-tab-windows"), "true");
      assert.deepEqual(await visiblePanels(), ["scheduler-panel-windows"]);
      await page.keyboard.press("ArrowRight");
      assert.equal(await selected("scheduler-tab-cron"), "true");
      await page.click("#scheduler-tab-windows");
      assert.equal(await selected("scheduler-tab-windows"), "true");
      await page.click("#scheduler-tab-cron");
      assert.deepEqual(await visiblePanels(), ["scheduler-panel-cron"]);
    });

    await t.test("scheduler K3 snippets match pickup.md", async () => {
      for (const name of ["launchers", "cron", "windows-task", "windows-tzdata"]) {
        assert.ok(pickup.includes(await snippet(name)), name);
      }
      assert.equal(
        await snippet("cron"),
        "*/5 * * * * PATH=<launcher bin dir>:/usr/bin:/bin python3 <installed-skill>/runtime/factory.py pickup >> <home>/logs/pickup.log 2>&1",
      );
      const plist = await snippet("launchd-plist");
      for (const line of [
        "<string><installed-skill>/runtime/factory.py</string>",
        "<integer>300</integer>",
        "<string><home>/logs/pickup.log</string>",
      ]) {
        assert.ok(plist.includes(line), line);
      }
      const section = await page.$eval("#settings-scheduler", (el) => el.textContent);
      for (const pattern of [/paseo/i, /claude/i, /dangerously/i, /model-a/i]) {
        assert.doesNotMatch(section, pattern);
      }
    });

    await t.test("scheduler K4 copy", async () => {
      await page.click('button[data-copy="scheduler-snippet-cron"]');
      await page.waitForFunction(
        () => document.getElementById("scheduler-status").textContent === "Copied.",
        wait,
      );
      assert.equal((await page.evaluate(() => window.copied)).at(-1), await snippet("cron"));
      await page.evaluate(() => {
        window.__copyFails = true;
      });
      await page.click('button[data-copy="scheduler-snippet-dry-run"]');
      await page.waitForFunction(
        () =>
          document.getElementById("scheduler-status").textContent.startsWith("Copy is unavailable"),
        wait,
      );
      assert.equal(
        await status(),
        "Copy is unavailable. The snippet is selected; copy it with your keyboard.",
      );
      assert.equal(await page.evaluate(() => getSelection().toString()), await snippet("dry-run"));
    });

    await t.test("scheduler K6 standing instruction", async () => {
      assert.equal(await page.$eval("#scheduler-standing-instruction", (el) => el.open), false);
      await page.click("#scheduler-standing-link");
      assert.equal(await page.$eval("#scheduler-standing-instruction", (el) => el.open), true);
      assert.equal(
        await page.evaluate(
          () =>
            document.activeElement ===
            document.querySelector("#scheduler-standing-instruction summary"),
        ),
        true,
      );
      assert.equal(await page.evaluate(() => location.hash), "#/settings/scheduler");
      const text = await page.$eval("#scheduler-standing-instruction p", (el) => el.textContent);
      assert.ok(text.startsWith("A standing instruction from the user"));
      assert.ok(pickup.includes(text));
      const sentence =
        "A deny-notes launch sends /factory deny-notes <id> --session <tag> in place of the idea command.";
      const help = await page.$eval(
        "#scheduler-standing-link",
        (el) => el.parentElement.textContent,
      );
      assert.ok(help.includes(sentence));
      assert.ok(pickup.replaceAll("`", "").replace(/\s+/g, " ").includes(sentence));
    });

    await t.test("scheduler K5 mobile", async () => {
      await page.setViewport({ width: 390, height: 844 });
      await page.click("#scheduler-tab-launchd");
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        true,
      );
      assert.equal(
        await page.$eval(
          "#scheduler-snippet-launchers",
          (el) => el.parentElement.scrollWidth <= el.parentElement.clientWidth,
        ),
        true,
      );
    });

    assert.deepEqual(errors, []);
  });
});
