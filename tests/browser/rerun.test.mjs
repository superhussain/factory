// The drawer's Rerun stage item: which jobs show it, the pickup-host check, the confirm, the
// self-move board write and the host line, against one owned local Worker, at mobile and
// desktop width. Also captures the review images.
import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { openJobActions } from "../support/board-ready.cjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  serveHostList,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const HOSTS = JSON.parse(
  await readFile(new URL("../fixtures/hosts-list.json", import.meta.url), "utf8"),
);
const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const wait = { polling: 50 };
const NOW = "2026-01-01T12:00:00Z";
const claimed = { device: "lab-linux", session: "a1b2c3", since: "2026-01-01T10:00:00Z" };
const JOBS = [
  {
    id: "042-sample-export",
    project: "sample",
    title: "Export the sample report",
    stage: "build",
    branch: "factory/042-sample-export",
    round: 1,
    updated: "2026-01-01T00:00:00Z",
    claim: claimed,
    paseo: { server: "srv-lab", agent: "synthetic-agent" },
  },
  { id: "043-unclaimed", project: "sample", title: "No host", stage: "spec", updated: NOW },
  {
    id: "044-open-pr",
    project: "sample",
    title: "Open PR",
    stage: "pr-open",
    updated: NOW,
    claim: claimed,
  },
  { id: "045-no-project", title: "No project", stage: "build", updated: NOW, claim: claimed },
  {
    id: "046-paused",
    project: "sample",
    title: "Paused",
    stage: "build",
    updated: NOW,
    claim: claimed,
    paused: { at: NOW },
  },
  {
    id: "047-needs",
    project: "sample",
    title: "Needs you",
    stage: "build",
    updated: NOW,
    claim: claimed,
    needs: "answer the questions",
  },
];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true, theme: "dark" },
  { width: 1280, height: 900, theme: "light" },
];
const CARD = '.card[data-id="042-sample-export"]';
const RERUNNING = "rerunning on Lab Linux · stop queued on Lab Linux";

const freezeClock = (page) =>
  page.evaluateOnNewDocument(() => {
    const RealDate = Date;
    window.Date = class extends RealDate {
      constructor(...args) {
        super(...(args.length ? args : ["2026-01-01T12:00:00Z"]));
      }
      static now() {
        return new RealDate("2026-01-01T12:00:00Z").getTime();
      }
    };
  });
const openDrawer = async (page, id) => {
  await page.evaluate((i) => (location.hash = "#" + i), id);
  await page.waitForFunction(
    (i) => document.querySelector("#d-meta")?.textContent.includes(i),
    wait,
    id,
  );
};
const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);
// The drawer head plus one element below it, such as the open menu.
const headAnd = (page, extra) =>
  page.evaluate((extra) => {
    const boxes = [".detail-head", extra]
      .map((s) => document.querySelector(s)?.getBoundingClientRect())
      .filter((b) => b && b.height);
    const x = Math.min(...boxes.map((b) => b.left));
    const y = Math.min(...boxes.map((b) => b.top));
    return {
      x,
      y,
      width: Math.max(...boxes.map((b) => b.right)) - x,
      height: Math.max(...boxes.map((b) => b.bottom)) - y + 4,
    };
  }, extra);

test("rerun stage", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const errors = [];
    for (const { theme, ...viewport } of VIEWPORTS) {
      const { width } = viewport;
      await seedBoard(origin, JOBS);
      let pickupHosts = ["lab-linux", "studio-mac"];
      let effectiveFails = false;
      let holdEffective = null;
      let effectiveSeen = null;
      const context = await browser.createBrowserContext();
      const page = await context.newPage();
      await page.setViewport(viewport);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      page.on("pageerror", (error) => errors.push(String(error)));
      await freezeClock(page);
      await blockLiveEvents(page);
      await serveHostList(page, HOSTS);
      // Puppeteer already runs each request handler as an intercept action, so awaiting the
      // hold here keeps the reply pending until the test releases it.
      page.on("request", async (request) => {
        if (new URL(request.url()).pathname !== "/api/pipeline/effective/sample") return;
        effectiveSeen?.();
        if (holdEffective) await holdEffective;
        return effectiveFails
          ? request.respond({ status: 500, body: "fixture failure" }, 1)
          : request.respond(
              {
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({ pickup: { enabled: true, hosts: pickupHosts } }),
              },
              1,
            );
      });
      await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
      await page.waitForSelector(CARD);
      const activate = (selector) =>
        viewport.hasTouch ? page.tap(selector) : page.click(selector);
      // Opens the menu, picks Rerun stage, and waits for the click to finish.
      const rerun = async () => {
        await openJobActions(page);
        await activate("#d-rerun");
        await page.waitForFunction(() => !document.getElementById("d-rerun").disabled, wait);
      };
      const rerunError = async () => {
        await page.waitForSelector("#d-rerun-err:not([hidden])", wait);
        return page.$eval("#d-rerun-err", (el) => el.textContent);
      };

      await t.test(`Rerun stage only for claimed, running jobs ${width}`, async () => {
        const shown = {};
        for (const { id } of JOBS) {
          await openDrawer(page, id);
          shown[id] = await page.$eval("#d-rerun", (el) => !el.hidden);
        }
        assert.deepEqual(shown, {
          "042-sample-export": true,
          "043-unclaimed": false,
          "044-open-pr": false,
          "045-no-project": false,
          "046-paused": false,
          "047-needs": false,
        });
      });

      await t.test(`a host without pickup or a failed check writes nothing ${width}`, async () => {
        await openDrawer(page, "042-sample-export");
        await openJobActions(page);
        assert.equal(await page.$eval("#d-rerun", (el) => el.textContent), "Rerun stage");
        if (width === 390)
          await page.screenshot({
            path: `${SHOTS}after-menu-390.png`,
            clip: await headAnd(page, "#d-actions-menu:not([hidden])"),
          });
        pickupHosts = ["studio-mac"];
        await rerun();
        assert.equal(
          await rerunError(),
          "lab-linux does not run pickup for sample, so nothing would restart the job. Use Move to another host instead.",
        );
        effectiveFails = true;
        await rerun();
        assert.equal(await rerunError(), "Could not check lab-linux. Try again.");
        assert.equal(await page.$eval("#d-rerun-status", (el) => el.textContent), "");
        effectiveFails = false;
        pickupHosts = ["lab-linux", "studio-mac"];
        const job = await boardJob(origin, "042-sample-export");
        assert.deepEqual([job.claim, job.move], [claimed, undefined]);
      });

      await t.test(
        `the check shows progress, then clears before the confirm ${width}`,
        async () => {
          await openDrawer(page, "042-sample-export");
          let release;
          holdEffective = new Promise((resolve) => (release = resolve));
          const seen = new Promise((resolve) => (effectiveSeen = resolve));
          try {
            await page.evaluate(() => {
              window.realConfirm = window.confirm;
              window.confirm = () => {
                window.statusAtConfirm = document.getElementById("d-rerun-status").textContent;
                return false;
              };
            });
            await openJobActions(page);
            await activate("#d-rerun");
            await seen;
            if (width === 390)
              await page.screenshot({
                path: `${SHOTS}after-checking-390.png`,
                clip: await headAnd(page, "#d-rerun-status"),
              });
            assert.equal(
              await page.$eval("#d-rerun-status", (el) => el.textContent),
              "Checking lab-linux…",
            );
            assert.equal(await page.$eval("#d-rerun", (el) => el.disabled), true);
            release();
            await page.waitForFunction(() => !document.getElementById("d-rerun").disabled, wait);
            assert.equal(await page.evaluate(() => window.statusAtConfirm), "");
            assert.equal(await page.$eval("#d-rerun-status", (el) => el.textContent), "");
            const job = await boardJob(origin, "042-sample-export");
            assert.deepEqual([job.claim, job.move], [claimed, undefined]);
          } finally {
            release();
            holdEffective = null;
            effectiveSeen = null;
            await page.waitForFunction(() => !document.getElementById("d-rerun").disabled, wait);
            await page.evaluate(() => {
              if (window.realConfirm) window.confirm = window.realConfirm;
            });
          }
        },
      );

      await t.test(`dismissing the confirm writes nothing ${width}`, async () => {
        let message = "";
        page.once("dialog", (d) => {
          message = d.message();
          return d.dismiss();
        });
        await rerun();
        assert.equal(
          message,
          "Rerun the build stage of 042-sample-export on lab-linux? This stops its current session. The next pickup run on lab-linux starts the stage again in a fresh session.",
        );
        const job = await boardJob(origin, "042-sample-export");
        assert.deepEqual([job.claim, job.move], [claimed, undefined]);
      });

      await t.test(`accepting writes a self-move and drops the claim ${width}`, async () => {
        page.once("dialog", (d) => d.accept());
        await rerun();
        const job = await boardJob(origin, "042-sample-export");
        assert.deepEqual(job.move, {
          from: "lab-linux",
          session: "a1b2c3",
          agent: "synthetic-agent",
          server: "srv-lab",
          to: "lab-linux",
          at: NOW,
        });
        assert.equal("claim" in job, false);
        assert.equal("paseo" in job, false);
        await page.waitForFunction(
          (card, line) => document.querySelector(card + " .host")?.textContent === line,
          wait,
          CARD,
          RERUNNING,
        );
        assert.ok((await page.$eval("#d-meta", (el) => el.textContent)).endsWith("· " + RERUNNING));
        assert.equal(await page.$eval("#d-rerun", (el) => el.hidden), true);
        await activate("#d-close");
        await page.waitForSelector("#detail", { hidden: true });
        if (width === 1280)
          await (await page.$(CARD)).screenshot({ path: `${SHOTS}after-rerunning-1280.png` });
      });
      await context.close();
    }
    assert.deepEqual(errors, []);
  });
});
