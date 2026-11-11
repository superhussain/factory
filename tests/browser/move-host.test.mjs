// Host line on cards and the drawer, the move button and dialog, and the board write, against
// one owned local Worker, at desktop and mobile width. Also captures the review images.
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

const FIXTURE_HOSTS = JSON.parse(
  await readFile(new URL("../fixtures/hosts-list.json", import.meta.url), "utf8"),
);
const studio = FIXTURE_HOSTS.find((h) => h.id === "studio-mac");
const HOSTS = [...FIXTURE_HOSTS, { ...studio, id: "cloud-box", label: "Cloud box" }];
const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const wait = { polling: 50 };
const NOW = "2026-01-01T12:00:00Z";
const JOBS = [
  {
    id: "042-sample-export",
    project: "sample",
    title: "Export the sample report",
    stage: "build",
    branch: "factory/042-sample-export",
    round: 1,
    updated: "2026-01-01T00:00:00Z",
    claim: { device: "lab-linux", session: "a1b2c3", since: "2026-01-01T10:00:00Z" },
    paseo: { server: "srv-lab", agent: "4cec0a47-1d21-4115-945b-9e7dfb668ef4" },
  },
  { id: "043-unclaimed", project: "sample", title: "No host", stage: "spec", updated: NOW },
  { id: "044-open-pr", project: "sample", title: "Open PR", stage: "pr-open", updated: NOW },
  { id: "045-no-project", title: "No project", stage: "build", updated: NOW },
  {
    id: "046-queued",
    project: "sample",
    title: "Queued stop",
    stage: "build",
    updated: NOW,
    claim: { device: "studio-mac", session: "d4e5f6", since: NOW },
    move: {
      from: "lab-linux",
      session: "a1b2c3",
      agent: "4cec0a47-1d21-4115-945b-9e7dfb668ef4",
      server: "srv-lab",
      to: "studio-mac",
      at: NOW,
      claimed: NOW,
    },
  },
  {
    id: "047-done",
    project: "sample",
    title: "Both done",
    stage: "build",
    updated: NOW,
    claim: { device: "studio-mac", session: "d4e5f6", since: NOW },
    move: {
      from: "lab-linux",
      session: "a1b2c3",
      to: "studio-mac",
      at: NOW,
      claimed: NOW,
      stopped: NOW,
    },
  },
];
const MOVE_HINT =
  " The move may have saved. Press Move job again with the same host to check. It will not queue a second move.";
const PICKUP_HOSTS = ["lab-linux", "studio-mac", "new-box", "spare-pc", "old-laptop"];

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
const hostLine = (page, id) =>
  page.$eval(`.card[data-id="${id}"] .host`, (el) => el.textContent).catch(() => null);
const options = (page) =>
  page.$$eval("#move-host-select option", (os) => os.map((o) => o.textContent));

test("move host", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const errors = [];
    for (const [width, height, suffix] of [
      [1440, 900, ""],
      [390, 844, "-mobile"],
    ]) {
      await seedBoard(origin, JOBS);
      let pickupHosts = PICKUP_HOSTS;
      let effectiveFails = false;
      const page = await browser.newPage();
      await page.setViewport({ width, height });
      page.on("pageerror", (error) => errors.push(String(error)));
      await freezeClock(page);
      await blockLiveEvents(page);
      await serveHostList(page, HOSTS);
      await page.setBypassServiceWorker(true);
      // Sends the page's board write from Node, then drops the page's copy of it.
      let landPut = false;
      page.on("request", async (request) => {
        if (
          !landPut ||
          request.method() !== "PUT" ||
          new URL(request.url()).pathname !== "/api/board"
        )
          return;
        landPut = false;
        const headers = request.headers();
        await fetch(request.url(), {
          method: "PUT",
          headers: Object.fromEntries(
            ["if-match", "if-none-match"].filter((h) => headers[h]).map((h) => [h, headers[h]]),
          ),
          body: request.postData(),
        });
        return request.abort("failed", 1);
      });
      page.on("request", (request) => {
        if (new URL(request.url()).pathname !== "/api/pipeline/effective/sample") return;
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

      await t.test(`host line on cards ${width}`, async () => {
        await page.waitForFunction(
          () =>
            document.querySelector('.card[data-id="042-sample-export"] .host')?.textContent ===
            "on Lab Linux",
          wait,
        );
        assert.equal(await hostLine(page, "043-unclaimed"), null);
        assert.equal(
          await hostLine(page, "046-queued"),
          "on Studio Mac · stop queued on Lab Linux",
        );
        assert.equal(await hostLine(page, "047-done"), "on Studio Mac");
        const card = await page.waitForSelector('.card[data-id="042-sample-export"]');
        await card.screenshot({ path: `${SHOTS}after-card-host${suffix}.png` });
      });

      await t.test(`button only for movable jobs ${width}`, async () => {
        const shown = {};
        for (const id of ["042-sample-export", "043-unclaimed", "044-open-pr", "045-no-project"]) {
          await page.evaluate((i) => (location.hash = "#" + i), id);
          await page.waitForFunction(
            (i) =>
              document.getElementById("d-title") &&
              document.querySelector("#d-meta")?.textContent.includes(i),
            wait,
            id,
          );
          shown[id] = await page.$eval("#d-move-host", (el) => !el.hidden);
        }
        assert.deepEqual(shown, {
          "042-sample-export": true,
          "043-unclaimed": true,
          "044-open-pr": false,
          "045-no-project": false,
        });
      });

      await t.test(`drawer host line ${width}`, async () => {
        await page.evaluate(() => (location.hash = "#042-sample-export"));
        await page.waitForFunction(
          () => document.getElementById("d-meta").textContent.endsWith("· on Lab Linux"),
          wait,
        );
        assert.equal(
          await page.$eval("#d-move-host", (el) => el.textContent),
          "Move to another host",
        );
      });

      await t.test(`dialog lists eligible hosts ${width}`, async () => {
        await openJobActions(page);
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        await (
          await page.$("#move-host-dlg")
        ).screenshot({ path: `${SHOTS}after-move-dialog${suffix}.png` });
        assert.equal(
          await page.$eval("#move-host-help", (el) => el.textContent),
          "042-sample-export runs on Lab Linux. Moving it stops that session and starts the job on the host you pick. If Lab Linux is offline, the stop waits until it comes back.",
        );
        assert.equal(
          await page.$eval("#move-host-select", (el) => el.getAttribute("aria-describedby")),
          "move-host-help",
        );
        assert.deepEqual(await options(page), ["Studio Mac (studio-mac)"]);
        await page.click("#move-host-cancel");
        assert.equal(
          await page.evaluate(() => document.getElementById("move-host-dlg").open),
          false,
        );
      });

      await t.test(`dialog for a job with no claim, empty and failure ${width}`, async () => {
        await page.evaluate(() => (location.hash = "#043-unclaimed"));
        await page.waitForFunction(
          () => document.getElementById("d-title").textContent === "No host",
          wait,
        );
        await openJobActions(page);
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        assert.equal(
          await page.$eval("#move-host-help", (el) => el.textContent),
          "043-unclaimed has no host right now. The host you pick starts it.",
        );
        assert.deepEqual(await options(page), [
          "Lab Linux (lab-linux) · offline",
          "Studio Mac (studio-mac)",
        ]);
        await page.click("#move-host-cancel");

        pickupHosts = ["lab-linux"];
        await page.evaluate(() => (location.hash = "#042-sample-export"));
        await page.waitForFunction(
          () => document.getElementById("d-title").textContent === "Export the sample report",
          wait,
        );
        await openJobActions(page);
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        await page.waitForFunction(
          () => document.getElementById("move-host-empty").textContent !== "",
          wait,
        );
        assert.equal(
          await page.$eval("#move-host-empty", (el) => el.textContent),
          "No other active host runs pickup for sample. Add one in Settings, Hosts.",
        );
        assert.equal(await page.$eval("#move-host-save", (el) => el.disabled), true);
        await page.click("#move-host-cancel");

        effectiveFails = true;
        await openJobActions(page);
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        assert.equal(
          await page.$eval("#move-host-err", (el) => el.textContent),
          "Could not load hosts. Try again.",
        );
        await page.click("#move-host-cancel");
        effectiveFails = false;
        pickupHosts = PICKUP_HOSTS;
      });

      await t.test(
        `dialog after a move excludes the old host and the target ${width}`,
        async () => {
          await seedBoard(origin, [
            {
              ...JOBS[0],
              claim: undefined,
              paseo: undefined,
              move: { from: "lab-linux", session: "a1b2c3", to: "studio-mac", at: NOW },
            },
          ]);
          await page.evaluate(() => window.fixtureBoardPoll?.());
          await page.reload({ waitUntil: "domcontentloaded" });
          await page.waitForFunction(
            () =>
              document.querySelector("#d-move-host") && !document.getElementById("detail").hidden,
            wait,
          );
          await openJobActions(page);
          await page.click("#d-move-host");
          await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
          await (
            await page.$("#move-host-dlg")
          ).screenshot({ path: `${SHOTS}after-move-dialog-moving${suffix}.png` });
          assert.equal(
            await page.$eval("#move-host-help", (el) => el.textContent),
            "042-sample-export is moving to Studio Mac. The host you pick starts it instead.",
          );
          assert.deepEqual(await options(page), []);
          await page.click("#move-host-cancel");
          await seedBoard(origin, JOBS);
          await page.reload({ waitUntil: "domcontentloaded" });
          await page.waitForSelector("#d-move-host:not([hidden])", wait);
        },
      );

      await t.test(`an uncertain move confirms on retry ${width}`, async () => {
        landPut = true;
        await openJobActions(page);
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        await page.click("#move-host-save");
        await page.waitForFunction(() => !document.getElementById("move-host-err").hidden, wait);
        const text = await page.$eval("#move-host-err", (el) => el.textContent);
        assert.ok(text.startsWith("Not confirmed: "), text);
        assert.ok(text.endsWith("." + MOVE_HINT), text);
        const board = await (await fetch(origin + "/api/board")).json();
        assert.equal(board.jobs.find((j) => j.id === "042-sample-export").move.to, "studio-mac");
        if (width === 1440)
          await (
            await page.$("#move-host-dlg")
          ).screenshot({ path: `${SHOTS}move-uncertain-1440.png` });
        const after = await (await fetch(origin + "/api/board")).text();
        await page.click("#move-host-save");
        await page.waitForFunction(() => !document.getElementById("move-host-dlg").open, wait);
        assert.equal(await (await fetch(origin + "/api/board")).text(), after);
        await seedBoard(origin, JOBS);
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForSelector("#d-move-host:not([hidden])", wait);
      });

      await t.test(`Move job writes move and drops the claim ${width}`, async () => {
        await openJobActions(page);
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        await page.click("#move-host-save");
        await page.waitForFunction(() => !document.getElementById("move-host-dlg").open, wait);
        const board = await (await fetch(origin + "/api/board")).json();
        const job = board.jobs.find((j) => j.id === "042-sample-export");
        assert.deepEqual(job.move, {
          from: "lab-linux",
          session: "a1b2c3",
          agent: "4cec0a47-1d21-4115-945b-9e7dfb668ef4",
          server: "srv-lab",
          to: "studio-mac",
          at: NOW,
        });
        assert.equal("claim" in job, false);
        assert.equal("paseo" in job, false);
        await page.waitForFunction(
          () =>
            document.querySelector('.card[data-id="042-sample-export"] .host')?.textContent ===
            "moving to Studio Mac · stop queued on Lab Linux",
          wait,
        );
        await page.click("#d-close");
        await page.waitForFunction(() => document.getElementById("detail").hidden, wait);
        await (
          await page.$('.card[data-id="042-sample-export"]')
        ).screenshot({ path: `${SHOTS}after-card-moving${suffix}.png` });
      });

      await t.test(`move keeps the paseo link ${width}`, async () => {
        const board = await (await fetch(origin + "/api/board")).json();
        const { move } = board.jobs.find((j) => j.id === "042-sample-export");
        assert.equal(move.agent, "4cec0a47-1d21-4115-945b-9e7dfb668ef4");
        assert.equal(move.server, "srv-lab");
      });

      await t.test(`chained move keeps the earlier stop ${width}`, async () => {
        const T1 = "2026-01-01T10:00:00Z";
        await seedBoard(origin, [
          {
            ...JOBS[0],
            claim: { device: "studio-mac", session: "d4e5f6", since: NOW },
            paseo: undefined,
            move: {
              from: "lab-linux",
              session: "a1b2c3",
              agent: "4cec0a47-1d21-4115-945b-9e7dfb668ef4",
              server: "srv-lab",
              to: "studio-mac",
              at: T1,
              claimed: NOW,
            },
          },
        ]);
        pickupHosts = [...PICKUP_HOSTS, "cloud-box"];
        await page.evaluate(() => (location.hash = "#042-sample-export"));
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForSelector("#d-move-host:not([hidden])", wait);
        await openJobActions(page);
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        await (
          await page.$("#move-host-dlg")
        ).screenshot({ path: `${SHOTS}after-move-dialog-chained${suffix}.png` });
        assert.deepEqual(await options(page), ["Cloud box (cloud-box)"]);
        await page.select("#move-host-select", "cloud-box");
        await page.click("#move-host-save");
        await page.waitForFunction(() => !document.getElementById("move-host-dlg").open, wait);
        await page.waitForFunction(
          () =>
            document.querySelector('.card[data-id="042-sample-export"] .host')?.textContent ===
            "moving to Cloud box · stop queued on Lab Linux, Studio Mac",
          wait,
        );
        await page.click("#d-close");
        await page.waitForFunction(() => document.getElementById("detail").hidden, wait);
        await (
          await page.$('.card[data-id="042-sample-export"]')
        ).screenshot({ path: `${SHOTS}after-card-chained${suffix}.png` });
        const board = await (await fetch(origin + "/api/board")).json();
        const job = board.jobs.find((j) => j.id === "042-sample-export");
        assert.deepEqual(job.move, {
          from: "studio-mac",
          session: "d4e5f6",
          to: "cloud-box",
          at: NOW,
          earlier: [
            {
              from: "lab-linux",
              session: "a1b2c3",
              agent: "4cec0a47-1d21-4115-945b-9e7dfb668ef4",
              server: "srv-lab",
              at: T1,
            },
          ],
        });
        assert.equal(
          await hostLine(page, "042-sample-export"),
          "moving to Cloud box · stop queued on Lab Linux, Studio Mac",
        );
        pickupHosts = PICKUP_HOSTS;
      });

      await t.test(`non-string stopped keeps the stop queued ${width}`, async () => {
        const q = JOBS.find((j) => j.id === "046-queued");
        const d = JOBS.find((j) => j.id === "047-done");
        await seedBoard(origin, [
          { ...q, move: { ...q.move, stopped: true } },
          { ...d, move: { ...d.move, stopped: null } },
        ]);
        await page.evaluate(() => (location.hash = ""));
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForFunction(
          () =>
            document.querySelector('.card[data-id="047-done"] .host')?.textContent ===
            "on Studio Mac · stop queued on Lab Linux",
          wait,
        );
        const queued = await page.waitForSelector('.card[data-id="046-queued"]');
        await queued.screenshot({ path: `${SHOTS}after-card-true-stopped${suffix}.png` });
        assert.equal(
          await hostLine(page, "046-queued"),
          "on Studio Mac · stop queued on Lab Linux",
        );
        assert.equal(await hostLine(page, "047-done"), "on Studio Mac · stop queued on Lab Linux");
      });

      await t.test(`dialog says which hosts have a stop queued ${width}`, async () => {
        await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
        await seedBoard(origin, [
          {
            ...JOBS[0],
            claim: { device: "cloud-box", session: "g7h8i9", since: NOW },
            paseo: undefined,
            move: {
              from: "studio-mac",
              session: "d4e5f6",
              to: "cloud-box",
              at: NOW,
              claimed: NOW,
              earlier: [{ from: "lab-linux", session: "a1b2c3", at: "2026-01-01T10:00:00Z" }],
            },
          },
        ]);
        pickupHosts = [...PICKUP_HOSTS, "cloud-box"];
        await page.evaluate(() => (location.hash = "#042-sample-export"));
        await page.reload({ waitUntil: "domcontentloaded" });
        await page.waitForSelector("#d-move-host:not([hidden])", wait);
        await openJobActions(page);
        await page.evaluate(() => {
          const d = document.getElementById("move-host-dlg");
          d.showModal = function () {
            window.emptyAtOpen = document.getElementById("move-host-empty").textContent;
            return HTMLDialogElement.prototype.showModal.call(this);
          };
        });
        await page.click("#d-move-host");
        await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
        await page.waitForFunction(
          () => document.getElementById("move-host-empty").textContent !== "",
          wait,
        );
        assert.equal(await page.evaluate(() => window.emptyAtOpen), "");
        await (
          await page.$("#move-host-dlg")
        ).screenshot({ path: `${SHOTS}after-move-dialog-stops-queued${suffix}.png` });
        assert.equal(
          await page.$eval("#move-host-empty", (el) => el.textContent),
          "Every other host that runs pickup for sample still has a stop queued: lab-linux, studio-mac. lab-linux is offline and stops the job only when it comes back online. Try again after one of them stops the job.",
        );
        assert.equal(
          await page.$eval(
            "#move-host-empty",
            (el) => el.getAttribute("role") + " " + el.getAttribute("aria-live"),
          ),
          "status polite",
        );
        for (const id of ["#move-host-label", "#move-host-select"]) {
          assert.equal(await page.$eval(id, (el) => getComputedStyle(el).display), "none");
        }
        assert.equal(await page.$eval("#move-host-save", (el) => el.disabled), true);
        await page.click("#move-host-cancel");
        pickupHosts = PICKUP_HOSTS;
      });

      await t.test(`dialog says when an offline host stops the job ${width}`, async () => {
        const lab = FIXTURE_HOSTS.find((h) => h.id === "lab-linux");
        await seedBoard(origin, [
          {
            ...JOBS[0],
            claim: { device: "cloud-box", session: "g7h8i9", since: NOW },
            paseo: undefined,
            move: {
              from: "studio-mac",
              session: "d4e5f6",
              to: "cloud-box",
              at: NOW,
              claimed: NOW,
              earlier: [
                {
                  from: "lab-linux",
                  session: "a1b2c3",
                  at: "2026-01-01T10:00:00Z",
                },
              ],
            },
          },
        ]);
        pickupHosts = [...PICKUP_HOSTS, "cloud-box"];
        try {
          const emptyText = async () => {
            await page.evaluate(() => (location.hash = "#042-sample-export"));
            await page.reload({ waitUntil: "domcontentloaded" });
            await page.waitForSelector("#d-move-host:not([hidden])", wait);
            await openJobActions(page);
            await page.click("#d-move-host");
            await page.waitForFunction(() => document.getElementById("move-host-dlg").open, wait);
            await page.waitForFunction(
              () => document.getElementById("move-host-empty").textContent !== "",
              wait,
            );
            const text = await page.$eval("#move-host-empty", (el) => el.textContent);
            await page.click("#move-host-cancel");
            return text;
          };
          studio.offline = true;
          assert.equal(
            await emptyText(),
            "Every other host that runs pickup for sample still has a stop queued: lab-linux, studio-mac. lab-linux, studio-mac are offline and stop the job only when they come back online. Try again after one of them stops the job.",
          );
          lab.offline = false;
          studio.offline = false;
          assert.equal(
            await emptyText(),
            "Every other host that runs pickup for sample still has a stop queued: lab-linux, studio-mac. Try again after one of them stops the job.",
          );
        } finally {
          lab.offline = true;
          studio.offline = false;
          pickupHosts = PICKUP_HOSTS;
        }
      });
      await page.close();
    }
    assert.deepEqual(errors, []);
  });
});
