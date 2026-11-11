// Header project filter: a checkbox menu over a saved set of hidden project keys, its
// label and "all projects" toggle, closing, reload, the legacy single-filter migration, the
// mirrored Display fieldset and the menu layout with a long title on a narrow screen, where
// the open menu sits 4px under the button even when the header wraps, and wraps it on touch.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";
import support from "../support/project-filter.cjs";

const { filterState, serveProjects } = support;
const JOBS = [
  { id: "090-alpha", project: "alpha", title: "Synthetic Alpha job", stage: "build" },
  { id: "091-beta", project: "beta", title: "Synthetic Beta job", stage: "spec" },
  { id: "092-loose", title: "Synthetic unassigned job", stage: "idea", idea: "Synthetic idea" },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));
const LONG =
  "Synthetic Gamma project with a very long title that runs well past the edge of a narrow phone screen";
const PROJECTS = [
  { slug: "alpha", title: "Synthetic Alpha", repo: "example/sample", base: "main" },
  { slug: "beta", title: "Synthetic Beta", repo: "example/sample", base: "main" },
];
const project = (title) => `---\nrepo: example/sample\nbase: main\ntitle: ${title}\n---\n`;
const row = (key, title, checked, mixed = false) => ({ key, title, checked, mixed });
const wait = { polling: 50 };

test("project filter", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(origin, "alpha", project("Synthetic Alpha"));
    await seedProject(origin, "beta", project("Synthetic Beta"));
    await seedBoard(origin, JOBS);
    const errors = [];
    const open = async () => {
      const page = await (await browser.createBrowserContext()).newPage();
      await page.setViewport({ width: 1280, height: 900 });
      page.on("pageerror", (error) => errors.push(String(error)));
      await blockLiveEvents(page);
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await ready(page);
      return page;
    };
    const ready = async (page) => {
      await page.waitForFunction(() => loaded === true, wait);
      await page.waitForSelector("#filter:not([hidden])", wait);
    };
    // One evaluate, so a poll re-render cannot detach the cards between finding and reading them.
    const cards = (page) =>
      page.evaluate(() =>
        [...document.querySelectorAll("#board .card")]
          .filter((el) => el.getClientRects().length)
          .map((el) => el.dataset.id)
          .sort(),
      );
    const menu = (page) =>
      page.evaluate(() => ({
        open: !document.getElementById("filter-menu").hidden,
        expanded: document.getElementById("filter").getAttribute("aria-expanded"),
        focus: document.activeElement.dataset.project ?? document.activeElement.id,
      }));
    const emptyLine = (page) =>
      page.evaluate(() => {
        const el = document.getElementById("filter-empty");
        return el && el.getClientRects().length ? el.textContent : null;
      });
    const box = (key, root = "#filter-menu") => `${root} [data-project="${key}"]`;
    const page = await open();

    await t.test("F1 default", async () => {
      assert.deepEqual(await filterState(page), {
        label: "all projects",
        rows: [
          row("", "all projects", true),
          row("alpha", "Synthetic Alpha", true),
          row("beta", "Synthetic Beta", true),
          row(":unassigned", "No project yet", true),
        ],
        stored: null,
      });
      assert.equal(
        await page.$eval("#filter", (el) => el.getAttribute("aria-label")),
        "Show jobs from: all projects",
      );
      assert.deepEqual(await menu(page), { open: false, expanded: "false", focus: "" });
      assert.deepEqual(await cards(page), ["090-alpha", "091-beta", "092-loose"]);
      assert.equal(await emptyLine(page), null);
    });

    await t.test("F2 uncheck", async () => {
      await page.click("#filter");
      assert.deepEqual(await menu(page), { open: true, expanded: "true", focus: "" });
      await page.click(box("beta"));
      assert.deepEqual(await filterState(page), {
        label: "2 selected",
        rows: [
          row("", "all projects", false, true),
          row("alpha", "Synthetic Alpha", true),
          row("beta", "Synthetic Beta", false),
          row(":unassigned", "No project yet", true),
        ],
        stored: '["beta"]',
      });
      assert.deepEqual(await menu(page), { open: true, expanded: "true", focus: "beta" });
      assert.deepEqual(await cards(page), ["090-alpha", "092-loose"]);
    });

    await t.test("F3 reload", async () => {
      const before = await filterState(page);
      await page.reload({ waitUntil: "domcontentloaded" });
      await ready(page);
      assert.deepEqual(await filterState(page), before);
      assert.equal((await menu(page)).open, false);
      assert.deepEqual(await cards(page), ["090-alpha", "092-loose"]);
    });

    await t.test("F4 toggle", async () => {
      await page.click("#filter");
      await page.click(box("alpha"));
      assert.equal((await filterState(page)).label, "No project yet");
      assert.deepEqual(await cards(page), ["092-loose"]);
      await page.click(box(""));
      assert.deepEqual(await filterState(page), {
        label: "all projects",
        rows: [
          row("", "all projects", true),
          row("alpha", "Synthetic Alpha", true),
          row("beta", "Synthetic Beta", true),
          row(":unassigned", "No project yet", true),
        ],
        stored: null,
      });
      assert.deepEqual(await cards(page), ["090-alpha", "091-beta", "092-loose"]);
      await page.click(box(""));
      assert.deepEqual(await filterState(page), {
        label: "no projects",
        rows: [
          row("", "all projects", false),
          row("alpha", "Synthetic Alpha", false),
          row("beta", "Synthetic Beta", false),
          row(":unassigned", "No project yet", false),
        ],
        stored: '["alpha","beta",":unassigned"]',
      });
      assert.equal(
        await page.$eval("#filter", (el) => el.getAttribute("aria-label")),
        "Show jobs from: no projects",
      );
      assert.deepEqual(await cards(page), []);
      assert.equal(
        await emptyLine(page),
        "No projects selected. Pick projects in the project filter to show their jobs.",
      );
      await page.click(box(""));
      assert.equal((await filterState(page)).label, "all projects");
      assert.equal(await emptyLine(page), null);
      assert.equal((await menu(page)).open, true);
    });

    await t.test("F14 few columns", async () => {
      const few = await open();
      await few.evaluate(() =>
        localStorage.setItem(
          "factory-hidden-columns",
          "backlog,idea,spec,build,review,approve,pr-open,closed",
        ),
      );
      await few.reload({ waitUntil: "domcontentloaded" });
      await ready(few);
      const measure = () =>
        few.evaluate(() => ({
          cols: document.getElementById("board").style.getPropertyValue("--cols"),
          width: document.getElementById("filter-empty")?.getBoundingClientRect().width ?? 0,
        }));
      await few.click("#filter");
      await few.click(box(""));
      assert.equal(
        await emptyLine(few),
        "No projects selected. Pick projects in the project filter to show their jobs.",
      );
      const none = await measure();
      assert.equal(none.cols, "36px 36px auto");
      assert.ok(none.width >= 600, `line width ${none.width}`);
      await few.click(box(""));
      assert.equal(await emptyLine(few), null);
      assert.equal((await measure()).cols, "36px 36px");
      await few.browserContext().close();
    });

    await t.test("F5 close", async () => {
      await page.keyboard.press("Escape");
      assert.deepEqual(await menu(page), { open: false, expanded: "false", focus: "filter" });
      await page.keyboard.press("ArrowDown");
      assert.deepEqual(await menu(page), { open: true, expanded: "true", focus: "" });
      await page.click("#updated");
      assert.equal((await menu(page)).open, false);
      await page.click("#filter");
      await page.focus(box(":unassigned"));
      await page.keyboard.press("Tab");
      assert.equal((await menu(page)).open, false);
      assert.deepEqual(await cards(page), ["090-alpha", "091-beta", "092-loose"]);
    });

    await t.test("F6 legacy", async () => {
      const legacy = await open();
      await legacy.evaluate(() => {
        localStorage.clear();
        localStorage.setItem("factory-project", "alpha");
      });
      await legacy.reload({ waitUntil: "domcontentloaded" });
      await ready(legacy);
      assert.equal((await filterState(legacy)).stored, '["beta",":unassigned"]');
      assert.equal((await filterState(legacy)).label, "Synthetic Alpha");
      assert.equal(await legacy.evaluate(() => localStorage.getItem("factory-project")), null);
      assert.deepEqual(await cards(legacy), ["090-alpha"]);
      await legacy.browserContext().close();
    });

    await t.test("F7 display", async () => {
      const display = () =>
        page.$$eval("#display-filter [data-project]", (els) =>
          els.map((el) => ({
            key: el.dataset.project,
            title: el.closest("label").textContent.trim(),
            checked: el.checked,
            mixed: el.indeterminate,
          })),
        );
      await page.evaluate(() => (location.hash = "#/settings/display"));
      await page.waitForSelector("#settings-display:not([hidden])", wait);
      assert.equal(
        await page.$eval("#display-filter legend", (el) => el.textContent),
        "Show jobs from",
      );
      assert.deepEqual(await display(), (await filterState(page)).rows);
      await page.click(box("alpha", "#display-filter"));
      const state = await filterState(page);
      assert.equal(state.label, "2 selected");
      assert.equal(state.stored, '["alpha"]');
      assert.deepEqual(await display(), state.rows);
      assert.equal(
        await page.$eval("#display-status", (el) => el.textContent),
        "Saved in this browser.",
      );
      await page.click("#filter");
      await page.click(box(""));
      assert.equal((await filterState(page)).label, "all projects");
      assert.deepEqual(await display(), (await filterState(page)).rows);
    });

    await t.test("F8 projects load fails", async () => {
      const failed = await open();
      await failed.evaluate(() => {
        localStorage.clear();
        localStorage.setItem("factory-projects-hidden", "beta");
      });
      await failed.setBypassServiceWorker(true);
      await failed.setRequestInterception(true);
      failed.on("request", (request) =>
        new URL(request.url()).pathname.endsWith("/projects.json")
          ? request.abort()
          : request.continue(),
      );
      await failed.reload({ waitUntil: "domcontentloaded" });
      await ready(failed);
      assert.deepEqual(await filterState(failed), {
        label: "all projects",
        rows: [row("", "all projects", true), row(":unassigned", "No project yet", true)],
        stored: "beta",
      });
      assert.deepEqual(await cards(failed), ["090-alpha", "091-beta", "092-loose"]);
      await failed.browserContext().close();
    });

    const old = await open();

    await t.test("F10 comma list", async () => {
      await old.evaluate(() => {
        localStorage.clear();
        localStorage.setItem("factory-projects-hidden", "beta");
      });
      await old.reload({ waitUntil: "domcontentloaded" });
      await ready(old);
      const state = await filterState(old);
      assert.equal(state.stored, '["beta"]');
      assert.equal(state.label, "2 selected");
      assert.deepEqual(await cards(old), ["090-alpha", "092-loose"]);
    });

    await t.test("F11 comma in a key", async () => {
      const keys = await old.evaluate(() => {
        localStorage.setItem("factory-projects-hidden", JSON.stringify(["a,b"]));
        return [...readHiddenProjects()];
      });
      assert.deepEqual(keys, ["a,b"]);
      await old.browserContext().close();
    });

    await t.test("F13 comma key reload", async () => {
      const comma = await open();
      await serveProjects(comma, [
        ...PROJECTS,
        { slug: "a,b", title: "Synthetic Comma", repo: "example/sample", base: "main" },
      ]);
      await comma.evaluate(() => {
        localStorage.clear();
        localStorage.setItem("factory-projects-hidden", JSON.stringify(["a,b"]));
      });
      await comma.reload({ waitUntil: "domcontentloaded" });
      await ready(comma);
      assert.deepEqual(await filterState(comma), {
        label: "3 selected",
        rows: [
          row("", "all projects", false, true),
          row("alpha", "Synthetic Alpha", true),
          row("beta", "Synthetic Beta", true),
          row("a,b", "Synthetic Comma", false),
          row(":unassigned", "No project yet", true),
        ],
        stored: '["a,b"]',
      });
      assert.deepEqual(await cards(comma), ["090-alpha", "091-beta", "092-loose"]);
      await comma.browserContext().close();
    });

    await t.test("F9 narrow long title", async () => {
      const narrow = await open();
      await serveProjects(narrow, [
        ...PROJECTS,
        { slug: "gamma", title: LONG, repo: "example/sample", base: "main" },
      ]);
      await narrow.reload({ waitUntil: "domcontentloaded" });
      await ready(narrow);
      await narrow.setViewport({ width: 390, height: 844 });
      await narrow.click("#filter");
      const measure = () =>
        narrow.evaluate(() => {
          const rect = document.getElementById("filter-menu").getBoundingClientRect();
          const title = document
            .querySelector('#filter-menu [data-project="gamma"]')
            .closest("label")
            .querySelector(".filter-title");
          const accent = document.createElement("i");
          accent.style.color = "var(--accent)";
          document.body.append(accent);
          const accentColor = getComputedStyle(accent).color;
          accent.remove();
          return {
            top: rect.top,
            buttonBottom: document.getElementById("filter").getBoundingClientRect().bottom,
            statusTop: document.querySelector("header .status").getBoundingClientRect().top,
            left: rect.left,
            right: rect.right,
            width: rect.width,
            innerWidth,
            scrollWidth: title.scrollWidth,
            clientWidth: title.clientWidth,
            textOverflow: getComputedStyle(title).textOverflow,
            title: title.title,
            accents: [...document.querySelectorAll("#filter-menu input")].map(
              (el) => getComputedStyle(el).accentColor === accentColor,
            ),
          };
        });
      const title = (m) => {
        assert.ok(m.scrollWidth > m.clientWidth);
        assert.equal(m.textOverflow, "ellipsis");
        assert.equal(m.title, LONG);
      };
      const phone = await measure();
      assert.ok(phone.statusTop >= phone.buttonBottom);
      assert.ok(Math.abs(phone.top - phone.buttonBottom - 4) < 1);
      assert.equal(phone.left, 16);
      assert.equal(phone.right, 374);
      title(phone);
      assert.ok(phone.accents.length > 0 && phone.accents.every(Boolean));
      await narrow.setViewport({ width: 1280, height: 900 });
      const desktop = await measure();
      assert.ok(Math.abs(desktop.top - desktop.buttonBottom - 4) < 1);
      assert.ok(desktop.width <= 360);
      title(desktop);
      await narrow.browserContext().close();
    });

    await t.test("F14 touch wraps long title", async () => {
      const touch = await open();
      await serveProjects(touch, [
        ...PROJECTS,
        { slug: "gamma", title: LONG, repo: "example/sample", base: "main" },
      ]);
      await touch.setViewport({ width: 390, height: 844, isMobile: true, hasTouch: true });
      await touch.reload({ waitUntil: "domcontentloaded" });
      await ready(touch);
      await touch.tap("#filter");
      const m = await touch.evaluate(() => {
        const menu = document.getElementById("filter-menu");
        const rect = menu.getBoundingClientRect();
        const label = menu.querySelector('[data-project="gamma"]').closest("label");
        const title = label.querySelector(".filter-title");
        return {
          left: rect.left,
          right: rect.right,
          menuScrollWidth: menu.scrollWidth,
          menuClientWidth: menu.clientWidth,
          scrollWidth: title.scrollWidth,
          clientWidth: title.clientWidth,
          whiteSpace: getComputedStyle(title).whiteSpace,
          text: title.textContent,
          labelHeight: label.getBoundingClientRect().height,
        };
      });
      assert.ok(m.scrollWidth <= m.clientWidth);
      assert.equal(m.whiteSpace, "normal");
      assert.equal(m.text, LONG);
      assert.ok(m.labelHeight > 44);
      assert.equal(m.left, 16);
      assert.equal(m.right, 374);
      assert.ok(m.menuScrollWidth <= m.menuClientWidth);
      await touch.browserContext().close();
    });

    await t.test("F12 no jobs", async () => {
      await seedBoard(origin, []);
      const empty = await open();
      await empty.evaluate(() => {
        localStorage.clear();
        localStorage.setItem(
          "factory-projects-hidden",
          JSON.stringify(["alpha", "beta", ":unassigned"]),
        );
      });
      await empty.reload({ waitUntil: "domcontentloaded" });
      await ready(empty);
      assert.equal(await empty.$eval(".blank h2", (el) => el.textContent), "No jobs yet");
      assert.equal(await emptyLine(empty), null);
      await empty.browserContext().close();
    });

    assert.deepEqual(errors, []);
  });
});
