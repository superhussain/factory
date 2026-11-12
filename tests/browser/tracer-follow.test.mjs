// The drawer's Follow in Tracer button sets tracer.follow on the board, the drawer shows a
// waiting line until a device saves the Tracer link, and only an http(s) link renders.
// At idea the button also asks Tracer to gather, and the drawer shows the scope line.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  seedPipeline,
} from "../support/browser-server.mjs";
import { waitForBoard, clickNow, openJobActions } from "../support/board-ready.cjs";

const JOB = {
  id: "042-sample-export",
  project: "sample",
  title: "Synthetic sample export",
  stage: "build",
  updated: "2026-01-01T00:00:00Z",
};
const WAITING = "Tracer: waiting for a device with tracer.json";
const LINK = "https://tracer.example.invalid/#/f/factory-42";

async function openDrawer(browser, origin) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await page.setViewport({ width: 1440, height: 900 });
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
  await waitForBoard(page);
  await clickNow(page, '.card[data-id="042-sample-export"]');
  await page.waitForFunction(() => !document.getElementById("detail").hidden);
  const drawer = () =>
    page.evaluate(() => ({
      follow: document.getElementById("d-follow-tracer").hidden,
      link: document.getElementById("d-tracer").hidden,
      href: document.getElementById("d-tracer").getAttribute("href"),
      label: document.getElementById("d-tracer").getAttribute("aria-label"),
      refs: document.getElementById("d-refs").textContent,
    }));
  return { context, page, errors, drawer };
}

test(
  "Follow in Tracer marks the job and the drawer shows the link",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(
        origin,
        "sample",
        "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      );
      await seedBoard(origin, [JOB]);
      await seedPipeline(origin, { schema: 1, tracer: { enabled: true } });
      let { context, page, errors, drawer } = await openDrawer(browser, origin);
      try {
        await page.waitForFunction(() => tracerOff === false);
        assert.deepEqual(await drawer(), {
          follow: false,
          link: true,
          href: null,
          label: null,
          refs: "",
        });
        await openJobActions(page);
        await page.click("#d-follow-tracer");
        await page.waitForFunction(
          (text) => document.getElementById("d-refs").textContent === text,
          {},
          WAITING,
        );
        const board = await (await fetch(origin + "/api/board")).json();
        assert.equal(board.jobs[0].tracer.follow, true);
        assert.deepEqual(await drawer(), {
          follow: true,
          link: true,
          href: null,
          label: null,
          refs: WAITING,
        });
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }

      await seedBoard(origin, [{ ...JOB, tracer: { feature: "factory-42", url: LINK } }]);
      ({ context, page, errors, drawer } = await openDrawer(browser, origin));
      try {
        await page.waitForFunction(() => tracerOff === false);
        assert.deepEqual(await drawer(), {
          follow: true,
          link: false,
          href: LINK,
          label: "Open this job's feature in Tracer",
          refs: "",
        });
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }

      await seedBoard(origin, [
        { ...JOB, tracer: { follow: true, feature: "factory-42", url: "javascript:alert(1)" } },
      ]);
      ({ context, page, errors, drawer } = await openDrawer(browser, origin));
      try {
        await page.waitForFunction(() => tracerOff === false);
        assert.deepEqual(await drawer(), {
          follow: true,
          link: true,
          href: null,
          label: null,
          refs: WAITING,
        });
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }
    });
  },
);

test(
  "A card shows Building in Tracer only while the board marks it",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(
        origin,
        "sample",
        "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      );
      await seedBoard(origin, [
        {
          id: "042-sample-export",
          project: "sample",
          stage: "idea",
          title: "Synthetic sample export",
          tracer: { building: true },
        },
        { id: "043-sample-plain", project: "sample", stage: "idea", title: "Plain" },
      ]);
      await seedPipeline(origin, { schema: 1, tracer: { enabled: true } });
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport({ width: 1440, height: 900 });
        await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
        await waitForBoard(page);
        await page.waitForFunction(() => tracerOff === false);
        const label = (id) =>
          page.evaluate(
            (id) =>
              document.querySelector(`.card[data-id="${id}"] .tracer-building`)?.textContent ??
              null,
            id,
          );
        assert.equal(await label("042-sample-export"), "Building in Tracer");
        assert.equal(await label("043-sample-plain"), null);
      } finally {
        await context.close();
      }
    });
  },
);

test("The drawer lists Tracer task statuses", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const tasks = [
      { id: "t1", title: "Add export button", status: "done" },
      { id: "t2", title: "Write CSV encoder", status: "in_progress" },
      { id: "t3", title: "Document export", status: "not_started" },
    ];
    await seedBoard(origin, [{ ...JOB, stage: "idea", tracer: { building: true, tasks } }]);
    await seedPipeline(origin, { schema: 1, tracer: { enabled: true } });
    const { context, page, errors } = await openDrawer(browser, origin);
    try {
      await page.waitForFunction(() => tracerOff === false);
      const list = () =>
        page.evaluate(() => {
          const box = document.getElementById("d-tracer-tasks");
          return {
            hidden: box.hidden,
            open: box.open,
            summary: box.querySelector("summary").textContent,
            rows: [...box.querySelectorAll("li")].map((li) => li.textContent),
            listStyle: getComputedStyle(box.querySelector("ol")).listStyleType,
            role: box.querySelector("ol").getAttribute("role"),
            indent: getComputedStyle(box.querySelector("li")).textIndent,
            pad: getComputedStyle(box.querySelector("li")).paddingLeft,
            summaryIndent: getComputedStyle(box.querySelector("summary")).textIndent,
            summaryPad: getComputedStyle(box.querySelector("summary")).paddingLeft,
          };
        });
      assert.deepEqual(await list(), {
        hidden: false,
        open: false,
        summary: "Tracer tasks: 1 of 3 done",
        rows: [
          "t1 Add export button: done",
          "t2 Write CSV encoder: in progress",
          "t3 Document export: not started",
        ],
        listStyle: "none",
        role: "list",
        indent: "-10px",
        pad: "10px",
        summaryIndent: "-14px",
        summaryPad: "14px",
      });
      await page.click("#d-tracer-tasks summary");
      await seedBoard(origin, [
        {
          ...JOB,
          stage: "idea",
          tracer: { building: true, tasks: [tasks[0], { ...tasks[1], status: "done" }, tasks[2]] },
        },
      ]);
      await page.waitForFunction(
        () =>
          document.querySelector("#d-tracer-tasks summary").textContent ===
          "Tracer tasks: 2 of 3 done",
        { timeout: 15000 },
      );
      assert.equal((await list()).open, true);
      await seedBoard(origin, [{ ...JOB, stage: "idea", tracer: { building: true } }]);
      await page.waitForFunction(() => document.getElementById("d-tracer-tasks").hidden, {
        timeout: 15000,
      });
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  });
});

test("The Tracer tasks summary gets a 44px tap target on touch", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const tasks = [
      { id: "t1", title: "Add export button", status: "done" },
      { id: "t2", title: "Write CSV encoder", status: "in_progress" },
      { id: "t3", title: "Document export", status: "not_started" },
    ];
    await seedBoard(origin, [{ ...JOB, stage: "idea", tracer: { building: true, tasks } }]);
    await seedPipeline(origin, { schema: 1, tracer: { enabled: true } });
    const measure = async (viewport) => {
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport(viewport);
        await page.goto(origin + "/#042-sample-export", { waitUntil: "domcontentloaded" });
        await page.waitForFunction(() => tracerOff === false);
        await page.waitForSelector("#d-tracer-tasks:not([hidden])");
        return await page.evaluate(() => ({
          coarse: matchMedia("(pointer: coarse)").matches,
          height: document.querySelector("#d-tracer-tasks summary").getBoundingClientRect().height,
        }));
      } finally {
        await context.close();
      }
    };
    const touch = await measure({ width: 390, height: 844, isMobile: true, hasTouch: true });
    assert.equal(touch.coarse, true);
    assert.ok(touch.height >= 44, `Summary height ${touch.height}`);
    const mouse = await measure({ width: 1440, height: 900 });
    assert.equal(mouse.coarse, false);
    assert.ok(mouse.height < 44, `Summary height ${mouse.height}`);
  });
});

test(
  "Follow in Tracer at idea asks Tracer to gather and the drawer shows the scope",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(
        origin,
        "sample",
        "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      );
      const idea = {
        ...JOB,
        title: "Export samples as CSV",
        stage: "idea",
        idea: "Export samples as CSV",
      };
      await seedBoard(origin, [idea]);
      await seedPipeline(origin, { schema: 1, tracer: { enabled: true } });
      let { context, page, errors, drawer } = await openDrawer(browser, origin);
      try {
        await page.waitForFunction(() => tracerOff === false);
        assert.equal((await drawer()).follow, false);
        await openJobActions(page);
        await page.click("#d-follow-tracer");
        await page.waitForFunction(() => document.getElementById("d-follow-tracer").hidden);
        const board = await (await fetch(origin + "/api/board")).json();
        assert.deepEqual(board.jobs[0].tracer, { follow: true, gather: true });
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }

      await seedBoard(origin, [
        {
          ...idea,
          tracer: {
            follow: true,
            gather: true,
            feature: "factory-42",
            url: LINK,
            scope: "gathering",
          },
        },
      ]);
      ({ context, page, errors, drawer } = await openDrawer(browser, origin));
      try {
        await page.waitForFunction(() => tracerOff === false);
        const shown = await drawer();
        assert.equal(shown.refs, "Tracer: gathering scope");
        assert.equal(shown.href, LINK);
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }
    });
  },
);

test("Tracer off hides every Tracer control", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const tasks = [
      { id: "t1", title: "Add export button", status: "done" },
      { id: "t2", title: "Write CSV encoder", status: "in_progress" },
      { id: "t3", title: "Document export", status: "not_started" },
    ];
    await seedBoard(origin, [
      {
        ...JOB,
        tracer: { follow: true, url: LINK, building: true, scope: "approved", tasks },
      },
      { ...JOB, id: "043-sample-plain", title: "Plain" },
    ]);
    await seedPipeline(origin, { schema: 1, tracer: { enabled: false } });
    const { context, page, errors, drawer } = await openDrawer(browser, origin);
    try {
      await page.waitForFunction(() => tracerOff === true);
      assert.equal(await page.$('.card[data-id="042-sample-export"] .tracer-building'), null);
      assert.deepEqual(await drawer(), {
        follow: true,
        link: true,
        href: null,
        label: null,
        refs: "",
      });
      assert.equal(await page.$eval("#d-tracer-tasks", (el) => el.hidden), true);
      await page.evaluate(() => (location.hash = "#043-sample-plain"));
      await page.waitForFunction(() => document.getElementById("d-title").textContent === "Plain");
      await openJobActions(page);
      assert.equal(await page.$eval("#d-follow-tracer", (el) => el.hidden), true);
      assert.equal(
        (await page.$eval("#d-actions-menu", (el) => el.innerText)).includes("Follow in Tracer"),
        false,
      );
      await page.keyboard.press("Escape");
      await page.click("#settings-link");
      await page.click("#settings-nav-pipeline");
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.select("#pipeline-scope", "global");
      await page.waitForFunction(() => !document.getElementById("pipeline-tracer-mode").disabled);
      assert.deepEqual(
        await page.evaluate(() => ({
          hidden: document.getElementById("pipeline-tracer").hidden,
          value: document.getElementById("pipeline-tracer-mode").value,
          text: document.getElementById("pipeline-tracer-mode").selectedOptions[0].textContent,
        })),
        { hidden: false, value: "", text: "Off (default)" },
      );
      await page.select("#pipeline-scope", "projects/sample");
      await page.waitForFunction(() => document.getElementById("pipeline-tracer").hidden);
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  });
});

test("Tracer is off with no tracer key in the pipeline", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [JOB]);
    const { context, page, errors } = await openDrawer(browser, origin);
    try {
      await page.evaluate(() => loadTracerSetting());
      await openJobActions(page);
      assert.equal(await page.evaluate(() => tracerOff), true);
      assert.equal(await page.$eval("#d-follow-tracer", (el) => el.hidden), true);
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  });
});

test("Picking On saves tracer.enabled true", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedBoard(origin, [JOB]);
    const context = await browser.createBrowserContext();
    try {
      const page = await context.newPage();
      await page.setViewport({ width: 1440, height: 900 });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.click("#settings-link");
      await page.click("#settings-nav-pipeline");
      await page.waitForSelector("#settings-pipeline:not([hidden])");
      await page.select("#pipeline-scope", "global");
      await page.waitForFunction(() => !document.getElementById("pipeline-tracer-mode").disabled);
      assert.deepEqual(
        await page.$$eval("#pipeline-tracer-mode option", (options) =>
          options.map((o) => [o.value, o.textContent]),
        ),
        [
          ["", "Off (default)"],
          ["on", "On"],
        ],
      );
      await page.select("#pipeline-tracer-mode", "on");
      await page.waitForFunction(() => !document.getElementById("pipeline-save").disabled);
      await page.click("#pipeline-save");
      await page.waitForFunction(
        () => document.getElementById("pipeline-status").textContent === "Saved as version 1",
      );
      const saved = await (await fetch(origin + "/api/pipeline/global")).json();
      assert.deepEqual(saved.tracer, { enabled: true });
    } finally {
      await context.close();
    }
  });
});
