// Settings page shell against one owned local Worker: route normalization, back navigation
// (arrow and header title),
// drawer hiding, inert board keys, draft survival, mobile overflow, the project list and
// per-project pickup.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  serveHostList,
} from "../support/browser-server.mjs";

const HOSTS = JSON.parse(
  await readFile(new URL("../fixtures/hosts-list.json", import.meta.url), "utf8"),
);

const JOB = {
  id: "001-sample",
  project: "sample",
  title: "Synthetic sample job",
  stage: "spec",
  updated: "2026-01-01T00:00:00Z",
};
const ORPHAN = {
  id: "002-orphan",
  project: "orphan",
  title: "Synthetic orphan job",
  stage: "spec",
  updated: "2026-01-01T00:00:00Z",
};
const DRAFT = "Synthetic draft that must survive";
const wait = { polling: 50 };

const setNote = async (page, stage, text) => {
  await page.click(`#pipeline-note-tab-${stage}`);
  await page.$eval(
    `#pipeline-note-${stage}`,
    (el, value) => {
      el.value = value;
      el.dispatchEvent(new Event("input", { bubbles: true }));
    },
    text,
  );
};

test("settings page", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB, ORPHAN]);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    page.on("dialog", (dialog) => dialog.accept());
    await page.goto(origin + "/#/settings/pipeline", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#settings-page:not([hidden])", wait);

    await t.test("settings S1 route shows the page", async () => {
      assert.equal(await page.$eval("#board", (el) => getComputedStyle(el).display), "none");
      assert.equal(await page.$eval("#detail", (el) => getComputedStyle(el).display), "none");
      assert.ok((await page.title()).startsWith("Settings · "));
      assert.equal(await page.evaluate(() => document.activeElement?.id), "pipeline-scope");
      assert.equal(await page.$eval("#pipeline-scope", (el) => el.value), "global");
    });

    await t.test("settings S2 unknown sections normalize", async () => {
      await page.evaluate(() => {
        location.hash = "#/settings";
      });
      await page.waitForFunction(() => location.hash === "#/settings/projects", wait);
      await page.evaluate(() => {
        location.hash = "#/settings/nope";
      });
      await page.waitForFunction(() => location.hash === "#/settings/projects", wait);
    });

    await t.test("settings S3 back link and browser back", async () => {
      await page.click("#settings-back");
      await page.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      assert.equal(await page.evaluate(() => location.hash), "");
      assert.equal(await page.evaluate(() => document.activeElement?.id), "settings-link");
      await page.click("#settings-link");
      await page.waitForSelector("#settings-page:not([hidden])", wait);
      await page.click("#settings-back");
      await page.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      await page.click("#settings-link");
      await page.waitForSelector("#settings-page:not([hidden])", wait);
      await page.goBack();
      await page.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      assert.equal(await page.evaluate(() => location.hash), "");
      assert.equal(await page.evaluate(() => document.activeElement?.id), "settings-link");
    });

    await t.test("settings S3b back arrow and header title return to board", async () => {
      await page.click("#settings-link");
      await page.waitForSelector("#settings-page:not([hidden])", wait);
      const back = await page.evaluate(() => {
        const el = document.getElementById("settings-back");
        return {
          text: el.textContent.trim(),
          svg: !!el.querySelector("svg"),
          label: el.getAttribute("aria-label"),
        };
      });
      assert.deepEqual(back, { text: "", svg: true, label: "Back to board" });
      await page.click("#board-link");
      await page.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      assert.equal(await page.evaluate(() => location.hash), "");
      assert.equal(await page.evaluate(() => document.activeElement?.id), "board-link");
      await page.waitForSelector(".card", wait);
      await page.evaluate(() => document.querySelector(".card").click());
      await page.waitForSelector("#detail:not([hidden])", wait);
      await page.evaluate(() => document.getElementById("board-link").click());
      await page.waitForFunction(() => document.getElementById("detail").hidden, wait);
      assert.equal(await page.evaluate(() => location.hash), "");
    });

    await t.test("settings S4 open job drawer hides", async () => {
      await page.waitForSelector(".card", wait);
      // Click in-page: the poll re-renders the board every 3s, so a coordinate
      // click can land on a detached node and silently open nothing.
      await page.evaluate(() => document.querySelector(".card").click());
      await page.waitForSelector("#detail:not([hidden])", wait);
      // The fixed-position drawer covers the header link's click point, so
      // dispatch on the element itself, like a keyboard user would.
      await page.evaluate(() => document.getElementById("settings-link").click());
      await page.waitForFunction(() => document.body.dataset.view === "settings", wait);
      assert.equal(await page.$eval("#detail", (el) => getComputedStyle(el).display), "none");
      assert.equal(await page.evaluate(() => location.hash), "#/settings/projects");
    });

    await t.test("settings S5 board keys are inert", async () => {
      await page.evaluate(() => document.activeElement?.blur());
      for (const key of ["ArrowRight", "j", "/", "n", "p", "s"]) {
        await page.keyboard.press(key);
        assert.equal(await page.evaluate(() => location.hash), "#/settings/projects");
        assert.equal(await page.evaluate(() => document.querySelector("dialog[open]")), null);
      }
    });

    await t.test("settings S6 dirty draft survives", async () => {
      await page.click("#settings-nav-pipeline");
      await page.waitForSelector("#settings-pipeline:not([hidden])", wait);
      await setNote(page, "build", DRAFT);
      await page.click("#settings-back");
      await page.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      await page.click("#settings-link");
      await page.waitForSelector("#settings-page:not([hidden])", wait);
      await page.click("#settings-nav-pipeline");
      await page.waitForSelector("#settings-pipeline:not([hidden])", wait);
      assert.equal(await page.$eval("#pipeline-note-build", (el) => el.value), DRAFT);
    });

    await t.test("settings S7 mobile layout", async () => {
      await page.setViewport({ width: 390, height: 844 });
      await page.waitForFunction(() => document.documentElement.scrollWidth <= 390, wait);
      const heights = await page.evaluate(() => ({
        settings: document.getElementById("settings-link").getBoundingClientRect().height,
        jump: document.getElementById("job-jump").getBoundingClientRect().height,
      }));
      assert.ok(Math.abs(heights.settings - heights.jump) <= 1, JSON.stringify(heights));
      await page.setViewport({ width: 1280, height: 900 });
    });

    await t.test("settings S8 projects list", async () => {
      await page.click("#settings-nav-projects");
      await page.waitForFunction(
        () => document.querySelectorAll("#project-list-body tr").length === 2,
        wait,
      );
      const rows = await page.$$eval("#project-list-body tr", (trs) =>
        trs.map((tr) => [...tr.cells].map((cell) => cell.textContent)),
      );
      assert.deepEqual(rows, [
        ["sampleSynthetic sample", "example/sample (main)"],
        ["orphan", "Not registered. Run /factory init in a checkout."],
      ]);
      assert.equal(await page.$eval("#project-list", (el) => el.hidden), false);
    });

    await t.test("settings S11 sections put their asides in a right-hand column", async () => {
      const boxes = (section, inside) =>
        page.$eval(
          section,
          (el, inside) => {
            const box = (node) => node.getBoundingClientRect().toJSON();
            const aside = el.querySelector(".settings-aside");
            return {
              main: box(el.querySelector(".settings-main")),
              aside: box(aside),
              inside: aside.contains(document.querySelector(inside)),
              pickup: box(document.getElementById("project-pickup")),
              documents: box(document.getElementById("settings-resource")),
            };
          },
          inside,
        );
      let b = await boxes("#settings-projects", "#project-list");
      assert.ok(b.inside);
      assert.ok(b.aside.left >= b.main.right, JSON.stringify(b));
      assert.ok(b.pickup.bottom <= b.documents.top, JSON.stringify(b));
      await page.setViewport({ width: 390, height: 844 });
      await page.waitForFunction(() => document.documentElement.scrollWidth <= 390, wait);
      b = await boxes("#settings-projects", "#project-list");
      assert.ok(b.aside.top >= b.main.bottom, JSON.stringify(b));
      await page.setViewport({ width: 1280, height: 900 });
      await page.click("#settings-nav-pipeline");
      await page.waitForSelector("#settings-pipeline:not([hidden])", wait);
      b = await boxes("#settings-pipeline", "#pipeline-effective");
      assert.ok(b.inside);
      assert.ok(b.aside.left >= b.main.right, JSON.stringify(b));
      await page.click("#settings-nav-projects");
      await page.waitForSelector("#settings-projects:not([hidden])", wait);
    });

    await t.test("settings S9 s key and header link", async () => {
      await page.click("#settings-back");
      await page.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      await page.evaluate(() => document.activeElement?.blur());
      assert.equal(await page.evaluate(() => document.activeElement === document.body), true);
      await page.keyboard.press("s");
      await page.waitForFunction(() => location.hash === "#/settings/projects", wait);
      await page.waitForFunction(() => document.activeElement?.id === "settings-project", wait);
      assert.deepEqual(
        await page.evaluate(() =>
          ["new-idea", "job-jump", "settings-link"].map((id) => {
            const el = document.getElementById(id);
            return {
              label: el.getAttribute("aria-label"),
              title: el.title,
              keys: el.getAttribute("aria-keyshortcuts"),
              text: el.textContent.trim(),
              svgHidden: el.querySelector("svg")?.getAttribute("aria-hidden"),
            };
          }),
        ),
        [
          {
            label: "New idea",
            title: "New idea (N)",
            keys: "n",
            text: "",
            svgHidden: "true",
          },
          {
            label: "Jump to job",
            title: "Jump to job (J, /, ⌘K or Ctrl+K)",
            keys: "j / Meta+K Control+K",
            text: "",
            svgHidden: "true",
          },
          {
            label: "Settings",
            title: "Settings (S)",
            keys: "s",
            text: "",
            svgHidden: "true",
          },
        ],
      );
      assert.equal(
        await page.$eval("#settings-link", (el) => el.getAttribute("href")),
        "#/settings",
      );
    });

    await t.test("settings S10 project pickup saves its pipeline scope", async () => {
      assert.equal(await page.$eval("#settings-project", (el) => el.value), "sample");
      await serveHostList(page, HOSTS);
      await page.evaluate(() => loadHosts());
      await page.waitForFunction(
        () => !document.getElementById("project-pickup-mode").disabled,
        wait,
      );
      await page.select("#project-pickup-mode", "on");
      await page.waitForFunction(
        () => !document.querySelector('#project-pickup-hosts input[value="studio-mac"]')?.disabled,
        wait,
      );
      await page.click('#project-pickup-hosts input[value="studio-mac"]');
      await page.click('#project-pickup-hosts input[value="lab-linux"]');
      await page.click("#project-pickup-save");
      await page.waitForFunction(
        () => document.getElementById("project-pickup-status").textContent === "Saved as version 1",
        wait,
      );
      const stored = await page.evaluate(async () =>
        (await fetch("./api/pipeline/projects/sample", { cache: "no-store" })).json(),
      );
      assert.deepEqual(stored.pickup, { enabled: true, hosts: ["studio-mac", "lab-linux"] });
      await page.click("#settings-nav-pipeline");
      await page.waitForSelector("#settings-pipeline:not([hidden])", wait);
      await page.select("#pipeline-scope", "projects/sample");
      assert.equal(await page.$eval("#pipeline-pickup-mode", (el) => el.value), "on");
      assert.deepEqual(
        await page.$$eval("#pipeline-pickup-hosts input:checked", (boxes) =>
          boxes.map((box) => box.value),
        ),
        ["lab-linux", "studio-mac"],
      );
    });

    await t.test("settings S12 Save pickup leaves the pipeline summary alone", async () => {
      await page.select("#pipeline-scope", "global");
      await page.type("#pipeline-summary", "Typed on the pipeline page");
      await page.click("#settings-nav-projects");
      await page.waitForSelector("#settings-projects:not([hidden])", wait);
      await page.select("#project-pickup-mode", "off");
      await page.click("#project-pickup-save");
      await page.waitForFunction(
        () => document.getElementById("project-pickup-status").textContent === "Saved as version 2",
        wait,
      );
      const latest = await page.evaluate(
        async () =>
          (
            await (
              await fetch("./api/pipeline/history/projects/sample", { cache: "no-store" })
            ).json()
          )[0],
      );
      assert.equal(latest.version, 2);
      assert.equal(latest.summary, "");
      assert.equal(
        await page.$eval("#pipeline-summary", (el) => el.value),
        "Typed on the pipeline page",
      );
    });

    assert.deepEqual(errors, []);
  });
});
