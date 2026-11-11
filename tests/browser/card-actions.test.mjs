// The job actions button on every board card: its markup, the same items as the drawer menu,
// running an action with the drawer closed, dialogs, copy, keyboard and the PR link beside it.
// Hover styles apply only to hover-capable pointers, so a touch tap leaves none behind.
// Also captures the open card menu for review.
// Also covers the double-run and hidden-item guards and menu focus after a poll.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  controlBoardPolling,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const AT = "2026-01-02T12:00:00Z";
const ID = "042-sample-export";
const JOBS = [
  {
    id: ID,
    project: "sample",
    title: "Export the sample report as CSV",
    stage: "pr-open",
    round: 1,
    updated: AT,
    pr: "https://github.com/example/sample/pull/46",
  },
  {
    id: "045-sample-idea",
    project: "sample",
    title: "Sample idea",
    stage: "idea",
    idea: "Sample idea",
    updated: AT,
  },
  {
    id: "047-sample-build",
    project: "sample",
    title: "Build the sample",
    stage: "build",
    round: 1,
    updated: AT,
    claim: { device: "lab-linux", session: "s1", since: AT },
  },
  {
    id: "041-sample-report",
    project: "sample",
    title: "Sample report",
    stage: "merged",
    updated: AT,
  },
];
const TRIGGER = `.card-actions[data-actions="${ID}"]`;
const PR_LINK = `.card[data-id="${ID}"] ~ .pr-link`;
const MENU = "#card-actions-menu";

async function openBoard(browser, origin, viewport) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await page.setViewport(viewport);
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await controlBoardPolling(page);
  await page.evaluateOnNewDocument(() => {
    window.__copied = [];
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: async (text) => {
          if (window.__denyCopy) throw new Error("synthetic denial");
          window.__copied.push(text);
        },
      },
    });
  });
  await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
  await page.waitForSelector(TRIGGER);
  return { context, page, errors };
}

const cardLabels = (page) =>
  page.$$eval(`${MENU} [data-action]`, (items) =>
    items.filter((b) => !b.hidden).map((b) => b.textContent),
  );

const menuState = (page) =>
  page.evaluate(
    (sel) => ({
      menu: !document.querySelector("#card-actions-menu").hidden,
      expanded: document.querySelector(sel).getAttribute("aria-expanded"),
      focus: document.activeElement.dataset.action ?? document.activeElement.className,
    }),
    TRIGGER,
  );

async function setPaused(page, origin, paused) {
  await seedBoard(
    origin,
    JOBS.map((j) => (j.id === ID && paused ? { ...j, paused: { at: AT } } : j)),
    new Date().toISOString(),
  );
  await page.evaluate(() => window.fixtureBoardPoll());
  await page.waitForFunction(
    (id, paused) =>
      [...document.querySelectorAll(`.card[data-id="${id}"] .chip`)].some(
        (c) => c.textContent === "Paused",
      ) === paused,
    {},
    ID,
    paused,
  );
}

const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);

async function clickItem(page, action) {
  await page.click(TRIGGER);
  await page.waitForSelector(`${MENU}:not([hidden])`);
  await page.click(`${MENU} [data-action="${action}"]`);
}

async function menuClip(page) {
  return page.evaluate((sel) => {
    const a = document.querySelector(sel).closest(".card-wrap").getBoundingClientRect();
    const b = document.querySelector("#card-actions-menu").getBoundingClientRect();
    const x = Math.min(a.left, b.left) - 8;
    const y = Math.min(a.top, b.top) - 8;
    // getBoundingClientRect is viewport-relative; Puppeteer's screenshot clip is document-relative.
    return {
      x: x + window.scrollX,
      y: y + window.scrollY,
      width: Math.max(a.right, b.right) + 8 - x,
      height: Math.max(a.bottom, b.bottom) + 8 - y,
    };
  }, TRIGGER);
}

test("card actions", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const { context, page, errors } = await openBoard(browser, origin, {
      width: 1440,
      height: 900,
    });

    await t.test("trigger", async () => {
      const triggers = await page.$$eval(".card-wrap", (wraps) =>
        wraps.map((w) => {
          const b = w.querySelector(":scope > .card-actions");
          return {
            card: w.querySelector(".card").dataset.id,
            label: b?.getAttribute("aria-label"),
            title: b?.title,
            popup: b?.getAttribute("aria-haspopup"),
          };
        }),
      );
      assert.equal(triggers.length, JOBS.length);
      for (const x of triggers)
        assert.deepEqual(x, {
          card: x.card,
          label: `Job actions for ${x.card}`,
          title: "Job actions",
          popup: "menu",
        });
    });

    await t.test("capture", async () => {
      await page.click(TRIGGER);
      await page.waitForSelector(`${MENU}:not([hidden])`);
      // A full-page capture resizes the viewport, which closes the menu.
      await page.screenshot({
        path: `${SHOTS}card-actions-menu-1440.png`,
        clip: await menuClip(page),
        captureBeyondViewport: false,
      });
      assert.equal(await page.$eval(MENU, (el) => el.hidden), false);
      await page.keyboard.press("Escape");
    });

    await t.test("capture 390", async () => {
      const other = await openBoard(browser, origin, {
        width: 390,
        height: 844,
        isMobile: true,
        hasTouch: true,
      });
      try {
        const p = other.page;
        await p.$eval(TRIGGER, (el) => el.scrollIntoView({ block: "center", inline: "center" }));
        await p.click(TRIGGER);
        await p.waitForSelector(`${MENU}:not([hidden])`);
        const rect = await p.evaluate((sel) => {
          const m = document.querySelector("#card-actions-menu").getBoundingClientRect();
          const t = document.querySelector(sel).getBoundingClientRect();
          return {
            left: m.left,
            right: m.right,
            top: m.top,
            bottom: m.bottom,
            tTop: t.top,
            tBottom: t.bottom,
            scrollY,
          };
        }, TRIGGER);
        assert.ok(rect.scrollY > 0, JSON.stringify(rect));
        assert.ok(rect.left >= 8 && rect.right <= 382, JSON.stringify(rect));
        // The menu opens 4px below or above its trigger on a scrolled board.
        assert.ok(
          Math.abs(rect.top - (rect.tBottom + 4)) <= 1 ||
            Math.abs(rect.bottom - (rect.tTop - 4)) <= 1,
          JSON.stringify(rect),
        );
        await p.screenshot({
          path: `${SHOTS}card-actions-menu-390.png`,
          clip: await menuClip(p),
          captureBeyondViewport: false,
        });
        await p.keyboard.press("Escape");
        await p.evaluate(() => (window.__denyCopy = true));
        await clickItem(p, "d-copy-link");
        await p.waitForFunction(() =>
          document.getElementById("board-status").textContent.startsWith("Copy is unavailable."),
        );
        await (
          await p.$("#board-status")
        ).screenshot({
          path: `${SHOTS}board-status-copy-fail-390.png`,
        });
        assert.deepEqual(other.errors, []);
      } finally {
        await other.context.close();
      }
    });

    await t.test("parity", async () => {
      // Terminal columns start collapsed, so the merged card needs its column open.
      await page.click('.column-toggle[data-stage="merged"]');
      for (const job of JOBS) {
        const sel = `.card-actions[data-actions="${job.id}"]`;
        await page.click(sel);
        await page.waitForSelector(`${MENU}:not([hidden])`);
        const card = await cardLabels(page);
        await page.keyboard.press("Escape");
        await page.evaluate((id) => (location.hash = id), job.id);
        await page.waitForFunction(
          (id) => document.getElementById("d-meta").textContent.includes(id),
          {},
          job.id,
        );
        const drawer = await page.$$eval("#d-actions-menu button", (items) =>
          items.filter((b) => !b.hidden).map((b) => b.textContent),
        );
        assert.deepEqual(card, drawer, job.id);
        assert.ok(card.length > 0, job.id);
        await page.evaluate(() => (location.hash = ""));
        await page.waitForFunction(() => document.getElementById("detail").hidden);
      }
    });

    await t.test("runs without drawer", async () => {
      await clickItem(page, "d-pause");
      await page.waitForFunction(
        (id) =>
          [...document.querySelectorAll(`.card[data-id="${id}"] .chip`)].some(
            (c) => c.textContent === "Paused",
          ),
        {},
        ID,
      );
      assert.equal(
        await page.evaluate(
          (sel) => document.activeElement === document.querySelector(sel),
          TRIGGER,
        ),
        true,
      );
      assert.equal(typeof (await boardJob(origin, ID)).paused?.at, "string");
      assert.equal(await page.$eval("#detail", (el) => el.hidden), true);
      assert.equal(await page.evaluate(() => location.hash), "");
    });

    await t.test("dialog", async () => {
      await clickItem(page, "d-close-job");
      await page.waitForFunction(() => document.getElementById("close-job-dlg").open);
      await page.click("#close-job-cancel");
      await page.waitForFunction(() => !document.getElementById("close-job-dlg").open);
      assert.equal(
        await page.evaluate(
          (sel) => document.activeElement === document.querySelector(sel),
          TRIGGER,
        ),
        true,
      );
      assert.equal(await page.$eval("#detail", (el) => el.hidden), true);
    });

    await t.test("copy", async () => {
      const FALLBACK = `Copy is unavailable. The link is ${origin}/#${ID}`;
      const shows = (text) =>
        page.waitForFunction(
          (text) => {
            const el = document.getElementById("board-status");
            return el.textContent === text && el.getClientRects().length > 0;
          },
          {},
          text,
        );
      await clickItem(page, "d-copy-link");
      await shows("Copied");
      await page.waitForFunction(() => document.getElementById("board-status").hidden, {
        timeout: 5000,
      });
      assert.deepEqual(await page.evaluate(() => window.__copied), [`${origin}/#${ID}`]);
      await page.evaluate(() => (window.__denyCopy = true));
      await clickItem(page, "d-copy-link");
      await shows(FALLBACK);
      await new Promise((resolve) => setTimeout(resolve, 7000));
      assert.deepEqual(await page.$eval("#board-status", (el) => [el.textContent, el.hidden]), [
        FALLBACK,
        false,
      ]);
      assert.deepEqual(
        await page.evaluate(() => [BOARD_STATUS_MS, COPIED_STATUS_MS]),
        [12000, 2000],
      );
      assert.equal(await page.$eval("#board-status", (el) => el.getAttribute("role")), "status");
    });

    await t.test("keyboard", async () => {
      await setPaused(page, origin, true);
      await page.focus(TRIGGER);
      const press = async (key, expected) => {
        await page.keyboard.press(key);
        assert.deepEqual(await menuState(page), expected, key);
      };
      const open = (focus) => ({ menu: true, expanded: "true", focus });
      await press("ArrowDown", open("d-follow-tracer"));
      await press("ArrowDown", open("d-resume"));
      await press("ArrowUp", open("d-follow-tracer"));
      await press("End", open("d-close-job"));
      await press("Home", open("d-follow-tracer"));
      await press("Escape", { menu: false, expanded: "false", focus: "card-actions" });
      await press("ArrowUp", open("d-close-job"));
      await press("Escape", { menu: false, expanded: "false", focus: "card-actions" });
    });

    await t.test("hidden item", async () => {
      await setPaused(page, origin, true);
      await page.click(TRIGGER);
      await page.waitForSelector(`${MENU}:not([hidden])`);
      await page.evaluate(() => {
        window.__runs = 0;
        window.__real = JOB_ACTION_RUN["d-pause"];
        JOB_ACTION_RUN["d-pause"] = () => window.__runs++;
      });
      try {
        await page.$eval(`${MENU} [data-action="d-pause"]`, (el) => el.click());
        assert.equal(await page.evaluate(() => window.__runs), 0);
        assert.equal((await menuState(page)).menu, true);
        assert.equal((await menuState(page)).expanded, "true");
      } finally {
        await page.evaluate(() => (JOB_ACTION_RUN["d-pause"] = window.__real));
        await page.keyboard.press("Escape");
      }
    });

    await t.test("double run", async () => {
      await setPaused(page, origin, true);
      await page.evaluate(() => {
        window.__runs = 0;
        window.__gate = new Promise((r) => (window.__release = r));
        window.__real = JOB_ACTION_RUN["d-resume"];
        JOB_ACTION_RUN["d-resume"] = (ctx) => {
          window.__runs++;
          return window.__gate.then(() => window.__real(ctx));
        };
      });
      try {
        await clickItem(page, "d-resume");
        await clickItem(page, "d-resume");
        assert.equal(await page.evaluate(() => window.__runs), 1);
      } finally {
        await page.evaluate(() => {
          window.__release();
          JOB_ACTION_RUN["d-resume"] = window.__real;
        });
      }
      await page.waitForFunction(
        (id) =>
          ![...document.querySelectorAll(`.card[data-id="${id}"] .chip`)].some(
            (c) => c.textContent === "Paused",
          ),
        {},
        ID,
      );
      await page.waitForFunction(() => cardRuns.size === 0);
    });

    await t.test("poll keeps menu focus", async () => {
      await setPaused(page, origin, false);
      await page.focus(TRIGGER);
      await page.keyboard.press("ArrowDown");
      await page.keyboard.press("ArrowDown");
      assert.equal((await menuState(page)).focus, "d-pause");
      await setPaused(page, origin, true);
      const clip = await page.evaluate((sel) => {
        const a = document.querySelector(sel).closest(".card-wrap").getBoundingClientRect();
        const b = document.querySelector("#card-actions-menu").getBoundingClientRect();
        const x = Math.min(a.left, b.left) - 8;
        const y = Math.min(a.top, b.top) - 8;
        return {
          x,
          y,
          width: Math.max(a.right, b.right) + 8 - x,
          height: Math.max(a.bottom, b.bottom) + 8 - y,
        };
      }, TRIGGER);
      await page.mouse.move(0, 0);
      await page.screenshot({
        path: `${SHOTS}card-menu-poll-focus-1440.png`,
        clip,
        captureBeyondViewport: false,
      });
      assert.deepEqual(await menuState(page), {
        menu: true,
        expanded: "true",
        focus: "d-follow-tracer",
      });
      assert.ok((await page.$eval(MENU, (el) => el.getBoundingClientRect().width)) < 400);
      await page.keyboard.press("Escape");
    });

    await t.test("PR link keeps focus across a redraw", async () => {
      await page.focus(TRIGGER);
      await page.keyboard.down("Shift");
      await page.keyboard.press("Tab");
      await page.keyboard.up("Shift");
      await page.mouse.move(5, 5);
      await page.evaluate(() => {
        window.__oldPr = document.activeElement;
        renderBoard();
      });
      await (await page.$(".col.pr-open")).screenshot({ path: `${SHOTS}pr-link-focus-1440.png` });
      assert.deepEqual(
        await page.evaluate(
          (sel) => [
            document.activeElement === document.querySelector(sel),
            document.activeElement !== window.__oldPr,
          ],
          PR_LINK,
        ),
        [true, true],
      );
      await page.evaluate(() => {
        hiddenCardFields.add("pr");
        renderBoard();
      });
      assert.equal(
        await page.evaluate(
          (id) => document.activeElement === document.querySelector(`.card[data-id="${id}"]`),
          ID,
        ),
        true,
      );
      await page.evaluate(() => {
        hiddenCardFields.delete("pr");
        renderBoard();
      });
    });

    await t.test("no overlap", async () => {
      for (const viewport of [
        { width: 390, height: 844, isMobile: true, hasTouch: true },
        { width: 1440, height: 900 },
      ]) {
        const other = await openBoard(browser, origin, viewport);
        try {
          const m = await other.page.evaluate((sel) => {
            const trigger = document.querySelector(sel);
            const link = trigger.parentElement.querySelector(".pr-link");
            trigger.scrollIntoView({ block: "center", inline: "center" });
            const hits = (el) => {
              const r = el.getBoundingClientRect();
              const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
              return hit === el || el.contains(hit);
            };
            const a = link.getBoundingClientRect();
            const b = trigger.getBoundingClientRect();
            return {
              apart:
                a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top,
              link: hits(link),
              trigger: hits(trigger),
            };
          }, TRIGGER);
          assert.deepEqual(m, { apart: true, link: true, trigger: true }, `${viewport.width}px`);
          assert.deepEqual(other.errors, []);
        } finally {
          await other.context.close();
        }
      }
    });

    const CARD = `.card[data-id="${ID}"]`;
    const ACCENT = "rgb(124, 58, 237)";
    const border = (p) => p.$eval(CARD, (el) => getComputedStyle(el).borderTopColor);

    await t.test("hover only on hover pointers", async () => {
      const other = await openBoard(browser, origin, { width: 1440, height: 900 });
      try {
        const loose = await other.page.evaluate(() => {
          const out = [];
          const walk = (rules, gated) => {
            for (const r of rules) {
              if (r instanceof CSSMediaRule)
                walk(r.cssRules, gated || r.conditionText === "(hover: hover)");
              else if (r instanceof CSSStyleRule && r.selectorText.includes(":hover") && !gated)
                out.push(r.selectorText);
            }
          };
          for (const sheet of document.styleSheets) walk(sheet.cssRules, false);
          return out;
        });
        assert.deepEqual(loose, []);
        await other.page.addStyleTag({ content: "*{transition:none!important}" });
        assert.equal(await other.page.evaluate(() => matchMedia("(hover: hover)").matches), true);
        await other.page.hover(CARD);
        assert.equal(await border(other.page), ACCENT);
        assert.deepEqual(other.errors, []);
      } finally {
        await other.context.close();
      }
    });

    await t.test("touch tap and keyboard", async () => {
      const other = await openBoard(browser, origin, {
        width: 390,
        height: 844,
        isMobile: true,
        hasTouch: true,
      });
      try {
        const p = other.page;
        await p.addStyleTag({ content: "*{transition:none!important}" });
        assert.equal(await p.evaluate(() => matchMedia("(hover: hover)").matches), false);
        const background = () => p.$eval(TRIGGER, (el) => getComputedStyle(el).backgroundColor);
        const rest = await background();
        await p.focus(CARD);
        await p.keyboard.press("Tab");
        await p.keyboard.down("Shift");
        await p.keyboard.press("Tab");
        await p.keyboard.up("Shift");
        const focused = (el) => el === document.activeElement && el.matches(":focus-visible");
        assert.equal(await p.$eval(CARD, focused), true);
        assert.equal(await border(p), ACCENT);
        await p.tap(TRIGGER);
        assert.equal(await background(), rest);
        assert.deepEqual(other.errors, []);
      } finally {
        await other.context.close();
      }
    });

    await context.close();
    assert.deepEqual(errors, []);
  });
});
