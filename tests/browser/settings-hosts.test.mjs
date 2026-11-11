// Settings Hosts section and the pickup host checkboxes: add, pair code, save, conflict,
// retire and new code against one owned local Worker, then the fixture registry for cards,
// presence, Confirm, the checkbox controls, the save-time check, keyboard and mobile width.
import assert from "node:assert/strict";
import { mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  serveHostList,
} from "../support/browser-server.mjs";
import { waitForBoard } from "../support/board-ready.cjs";

const HOSTS = JSON.parse(
  await readFile(new URL("../fixtures/hosts-list.json", import.meta.url), "utf8"),
);
const CODE = /^[A-HJ-NP-Z2-9]{5}-[A-HJ-NP-Z2-9]{5}$/;
const CONFLICT = "This host changed elsewhere. Your edits are kept. Load latest, then try again.";
const wait = { polling: 50 };
const captureDir = process.env.FACTORY_CAPTURE_DIR;

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
const setValue = (page, selector, value) =>
  page.$eval(
    selector,
    (el, v) => {
      el.value = v;
      el.dispatchEvent(new Event("input", { bubbles: true }));
    },
    value,
  );
// Host-card buttons are rebuilt by every renderHosts(), and one load renders twice.
// Click in-page so the node cannot detach between find and click.
const clickNow = (page, selector) => page.$eval(selector, (el) => el.click());
const cardMessage = (page, id) =>
  page.$eval(`.host-card[data-id="${id}"] .host-msg`, (el) => el.textContent);
const waitCardMessage = (page, id, text) =>
  page.waitForFunction(
    (i, t) => document.querySelector(`.host-card[data-id="${i}"] .host-msg`)?.textContent === t,
    wait,
    id,
    text,
  );
const badge = (page, id) =>
  page.$eval(`.host-card[data-id="${id}"] .host-badge`, (el) => el.textContent);
const rows = (page, group) =>
  page.$$eval(`#${group} label`, (labels) =>
    labels.map((label) => [
      label.querySelector("input").value,
      label.querySelector("input").checked,
      label.querySelector(".host-badge")?.textContent ?? "",
    ]),
  );
const pipelineStatus = (page) => page.$eval("#pipeline-status", (el) => el.textContent);
const waitScopeOption = (page) =>
  page.waitForSelector('#pipeline-scope option[value="projects/sample"]', wait);

test("settings hosts", { timeout: 240000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      {
        id: "001-sample",
        project: "sample",
        title: "Synthetic sample job",
        stage: "spec",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    const errors = [];
    const context = await browser.createBrowserContext();
    const open = async (hash, list) => {
      const page = await context.newPage();
      await page.setViewport({ width: 1280, height: 900 });
      page.on("pageerror", (error) => errors.push(String(error)));
      page.on("dialog", (dialog) => dialog.accept());
      if (list !== undefined) {
        await freezeClock(page);
        await serveHostList(page, list);
      }
      await page.goto(origin + "/" + hash, { waitUntil: "domcontentloaded" });
      await page.waitForSelector("#settings-page:not([hidden])", wait);
      return page;
    };

    const live = await open("#/settings/hosts");

    await t.test("hosts H1 route and add host with a one-time code", async () => {
      await live.waitForSelector("#settings-hosts:not([hidden])", wait);
      assert.equal(
        await live.$eval("#settings-nav-hosts", (el) => el.getAttribute("aria-current")),
        "page",
      );
      assert.equal(await live.evaluate(() => document.activeElement?.id), "hosts-settings-heading");
      await live.waitForFunction(
        () =>
          document.getElementById("hosts-status").textContent === "No hosts yet. Add one below.",
        wait,
      );
      await live.click("#settings-back");
      await live.waitForFunction(() => document.getElementById("settings-page").hidden, wait);
      await live.goBack();
      await live.waitForSelector("#settings-hosts:not([hidden])", wait);

      await live.type("#host-add-id", "test-host");
      await live.type("#host-add-label", "Test host");
      await live.click("#host-add-save");
      await live.waitForSelector("#host-code:not([hidden])", wait);
      const command = await live.$eval("#host-code-command", (el) => el.textContent);
      const code = command.split(" ").at(-1);
      assert.match(code, CODE);
      assert.equal(command, "factory.py host pair test-host " + code);
      assert.ok(
        (await live.$eval("#host-code-text", (el) => el.textContent)).startsWith(
          "Pairing code for test-host: " + code + ". It works once and expires at ",
        ),
      );
      await live.waitForSelector('.host-card[data-id="test-host"]', wait);
      assert.equal(await badge(live, "test-host"), "Pending pairing");
      const leaks = await live.evaluate(
        (c) =>
          [
            JSON.stringify({ ...sessionStorage }),
            JSON.stringify({ ...localStorage }),
            location.href,
          ].filter((text) => text.includes(c) || text.includes(c.replace("-", ""))),
        code,
      );
      assert.deepEqual(leaks, []);
      await live.click("#host-code-done");
      assert.equal(await live.$eval("#host-code", (el) => el.hidden), true);
      assert.equal(await live.$eval("#host-code-command", (el) => el.textContent), "");
    });

    await t.test("hosts H2 save and conflict keep typed values", async () => {
      await setValue(live, "#host-test-host-label", "Renamed");
      await setValue(live, "#host-test-host-max", "3");
      await clickNow(live, '[aria-label="Save Test host"]');
      await waitCardMessage(live, "test-host", "Saved.");
      await live.reload({ waitUntil: "domcontentloaded" });
      await waitForBoard(live);
      await live.waitForSelector('.host-card[data-id="test-host"]', wait);
      assert.equal(await live.$eval("#host-test-host-label", (el) => el.value), "Renamed");
      assert.equal(await live.$eval("#host-test-host-max", (el) => el.value), "3");
      await setValue(live, "#host-test-host-max", "0");
      await clickNow(live, '[aria-label="Save Renamed"]');
      await waitCardMessage(live, "test-host", "Saved.");
      await live.reload({ waitUntil: "domcontentloaded" });
      await waitForBoard(live);
      await live.waitForSelector('.host-card[data-id="test-host"]', wait);
      assert.equal(await live.$eval("#host-test-host-max", (el) => el.value), "0");
      assert.ok(
        (await live.$eval('.host-card[data-id="test-host"]', (li) => li.textContent)).includes(
          "Picks up: nothing while paused. Max in flight is 0.",
        ),
      );
      await setValue(live, "#host-test-host-max", "");
      await clickNow(live, '[aria-label="Save Renamed"]');
      await waitCardMessage(
        live,
        "test-host",
        "Could not save: max_inflight: must be a whole number from 0 to 5",
      );
      assert.equal((await (await fetch(origin + "/api/hosts")).json())[0].max_inflight, 0);
      await setValue(live, "#host-test-host-max", "0");

      const [current] = await (await fetch(origin + "/api/hosts")).json();
      const patch = await fetch(origin + "/api/hosts/test-host", {
        method: "PATCH",
        headers: { "If-Match": current.etag, "Content-Type": "application/json" },
        body: JSON.stringify({ label: "Elsewhere" }),
      });
      assert.equal(patch.status, 200);
      await setValue(live, "#host-test-host-label", "Mine");
      await clickNow(live, '[aria-label="Save Renamed"]');
      await waitCardMessage(live, "test-host", CONFLICT);
      assert.equal(
        await live.$eval('.host-card[data-id="test-host"] .host-msg', (el) =>
          el.getAttribute("role"),
        ),
        "alert",
      );
      assert.equal(await live.$eval("#host-test-host-label", (el) => el.value), "Mine");
    });

    await t.test("hosts H3 retire then new code", async () => {
      await live.click("#hosts-load");
      await live.waitForSelector('[aria-label="Retire Elsewhere"]', wait);
      assert.equal(await live.$eval("#host-test-host-label", (el) => el.value), "Mine");
      await clickNow(live, '[aria-label="Retire Elsewhere"]');
      await waitCardMessage(live, "test-host", "Retired.");
      await live.waitForFunction(
        () =>
          document.querySelector('.host-card[data-id="test-host"] .host-badge')?.textContent ===
          "Retired",
        wait,
      );
      await clickNow(live, '[aria-label="New code Elsewhere"]');
      await live.waitForSelector("#host-code:not([hidden])", wait);
      assert.match(
        (await live.$eval("#host-code-command", (el) => el.textContent)).split(" ").at(-1),
        CODE,
      );
      await live.waitForFunction(
        () =>
          document.querySelector('.host-card[data-id="test-host"] .host-badge')?.textContent ===
          "Pending pairing",
        wait,
      );
      await live.click("#settings-nav-pipeline");
      await live.waitForSelector("#settings-pipeline:not([hidden])", wait);
      assert.equal(await live.$eval("#host-code", (el) => el.hidden), true);
      await live.close();
    });

    await t.test("hosts H4 fixture cards, presence and Confirm", async (st) => {
      const page = await open("#/settings/hosts", HOSTS);
      let confirmed = null;
      page.on("request", (request) => {
        const path = new URL(request.url()).pathname;
        if (request.method() === "PATCH" && path === "/api/hosts/new-box") {
          confirmed = { body: request.postData(), ifMatch: request.headers()["if-match"] };
          return request.respond(
            { status: 200, contentType: "application/json", body: JSON.stringify(HOSTS[1]) },
            1,
          );
        }
        if (request.method() === "GET" && path === "/api/pipeline/effective/sample")
          return request.respond(
            {
              status: 200,
              contentType: "application/json",
              body: JSON.stringify({ pickup: { enabled: true, hosts: ["studio-mac"] } }),
            },
            1,
          );
      });
      await page.click("#hosts-load");
      await page.waitForFunction(
        () =>
          document.querySelectorAll(".host-card").length === 5 &&
          document.querySelector('.host-card[data-id="studio-mac"]').textContent.includes("sample"),
        wait,
      );
      const cards = await page.$$eval(".host-card", (lis) =>
        lis.map((li) => ({
          id: li.dataset.id,
          heading: li.querySelector("h4").textContent,
          seen: li.querySelector(".host-seen")?.textContent ?? null,
          actions: [...li.querySelectorAll(".actions button")].map((b) => b.textContent),
          routes: li.querySelector(".host-routes")?.querySelectorAll("tbody tr").length ?? null,
          routeInputs: li.querySelectorAll(".host-routes input").length,
          picks: [...li.querySelectorAll("p")].find((p) => p.textContent.startsWith("Picks up"))
            ?.textContent,
        })),
      );
      assert.deepEqual(cards, [
        {
          id: "lab-linux",
          heading: "Lab Linux",
          seen: "Offline, last seen 13 h ago",
          actions: ["Save", "Retire"],
          routes: 0,
          routeInputs: 0,
          picks: "Picks up: no projects",
        },
        {
          id: "new-box",
          heading: "New box",
          seen: "No heartbeat yet",
          actions: ["Save", "Confirm", "Retire"],
          routes: null,
          routeInputs: 0,
          picks: "Picks up: no projects",
        },
        {
          id: "old-laptop",
          heading: "Old laptop",
          seen: null,
          actions: ["Save", "New code"],
          routes: null,
          routeInputs: 0,
          picks: "Picks up: no projects",
        },
        {
          id: "spare-pc",
          heading: "Spare PC",
          seen: "No heartbeat yet",
          actions: ["Save", "Retire", "New code"],
          routes: null,
          routeInputs: 0,
          picks: "Picks up: no projects",
        },
        {
          id: "studio-mac",
          heading: "Studio Mac",
          seen: "Seen 3 min ago",
          actions: ["Save", "Retire"],
          routes: 2,
          routeInputs: 0,
          picks: "Picks up: sample",
        },
      ]);
      assert.deepEqual(
        await page.evaluate(() => [
          document.querySelector("#host-studio-mac-max").getAttribute("aria-describedby"),
          document.querySelector("#host-studio-mac-max-help").textContent,
        ]),
        [
          "host-studio-mac-max-help",
          "Set 0 to pause pickup on this host. Jobs waiting on a human never count toward this limit.",
        ],
      );
      const studio = await page.$eval('.host-card[data-id="studio-mac"]', (li) => li.textContent);
      assert.ok(
        studio.includes(
          "macOS, launchd, last pickup 2026-01-01 11:00 UTC. Bundle 1a2b3c4. Doctor ok.",
        ),
        studio,
      );
      assert.ok(studio.includes("fallbackopencodeexample-model-bnone2026-01-01 13:00 UTC"), studio);
      assert.ok(
        (await page.$eval('.host-card[data-id="lab-linux"]', (li) => li.textContent)).includes(
          "Doctor failed: schedule-entry.",
        ),
      );
      await page.click('[aria-label="Confirm New box"]');
      await waitCardMessage(page, "new-box", "Confirmed. It picks up work now.");
      assert.deepEqual(confirmed, { body: '{"state":"active"}', ifMatch: '"etag-new-box"' });
      assert.equal(await cardMessage(page, "new-box"), "Confirmed. It picks up work now.");

      await st.test("hosts H6 keyboard and mobile width on hosts", async () => {
        const wanted = await page.$$eval("#hosts-list button, #hosts-list input", (els) =>
          els.map((el) => el.id || el.getAttribute("aria-label")),
        );
        const reached = new Set();
        await page.focus("#hosts-settings-heading");
        for (let i = 0; i < wanted.length + 10; i++) {
          await page.keyboard.press("Tab");
          reached.add(
            await page.evaluate(
              () => document.activeElement.id || document.activeElement.getAttribute("aria-label"),
            ),
          );
        }
        assert.deepEqual(
          wanted.filter((name) => !reached.has(name)),
          [],
        );
        await page.setViewport({ width: 390, height: 844 });
        await page.waitForFunction(() => document.documentElement.scrollWidth <= 390, wait);
      });
      await page.close();
    });

    await t.test("hosts H7 paused pill and Confirm copy", async () => {
      const paused = HOSTS.map((h) =>
        h.id === "new-box" || h.id === "old-laptop" ? { ...h, max_inflight: 0 } : h,
      );
      const page = await open("#/settings/hosts", paused);
      page.on("request", (request) => {
        const path = new URL(request.url()).pathname;
        if (request.method() === "PATCH" && path === "/api/hosts/new-box")
          return request.respond(
            {
              status: 200,
              contentType: "application/json",
              body: JSON.stringify({ ...paused.find((h) => h.id === "new-box"), state: "active" }),
            },
            1,
          );
      });
      await page.click("#hosts-load");
      await page.waitForFunction(() => document.querySelectorAll(".host-card").length === 5, wait);
      const pills = (id) =>
        page.$$eval(`.host-card[data-id="${id}"] .host-badge`, (els) =>
          els.map((el) => el.textContent),
        );
      assert.deepEqual(await pills("new-box"), ["Paired, not confirmed", "Paused"]);
      assert.deepEqual(await pills("old-laptop"), ["Retired"]);
      assert.deepEqual(await pills("studio-mac"), ["Active"]);
      await page.click('[aria-label="Confirm New box"]');
      await waitCardMessage(
        page,
        "new-box",
        "Confirmed. It stays paused while Max in flight is 0.",
      );
      await page.close();
    });

    await t.test("hosts H8 triage and doctor failures show in the alert color", async () => {
      const failing = HOSTS.map((h) =>
        h.id === "studio-mac"
          ? { ...h, status: { ...h.status, triage_failed_since: "2026-01-01T09:00:00Z" } }
          : h,
      );
      const page = await open("#/settings/hosts", failing);
      await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      await page.click("#hosts-load");
      await page.waitForFunction(() => document.querySelectorAll(".host-card").length === 5, wait);
      const card = '.host-card[data-id="studio-mac"]';
      if (captureDir) {
        await mkdir(captureDir, { recursive: true });
        await (await page.$(card)).screenshot({ path: join(captureDir, "host-card-1280.png") });
        await (
          await page.$('.host-card[data-id="lab-linux"]')
        ).screenshot({
          path: join(captureDir, "host-card-doctor-1280.png"),
        });
        await page.setViewport({ width: 390, height: 844 });
        await (await page.$(card)).screenshot({ path: join(captureDir, "host-card-390.png") });
      }
      assert.equal(
        await page.$eval(`${card} .host-triage`, (el) => el.textContent),
        "Backlog triage failing since 2026-01-01 09:00 UTC. Pickup still launches ideas. " +
          "See the host's pickup log for the error.",
      );
      assert.equal(await page.$('.host-card[data-id="lab-linux"] .host-triage'), null);
      // A late renderHosts() can detach a node between $eval's find and read, so read in one call.
      const color = (sel) =>
        page.evaluate((s) => getComputedStyle(document.querySelector(s)).color, sel);
      const alert = "rgb(209, 52, 47)";
      assert.equal(await color(`${card} .host-triage`), alert);
      const doctor = '.host-card[data-id="lab-linux"] .host-doctor';
      assert.equal(
        await page.$eval(doctor, (el) => el.textContent),
        "Doctor failed: schedule-entry.",
      );
      assert.equal(await color(doctor), alert);
      assert.equal(await page.$(`${card} .host-doctor`), null);
      assert.equal(
        await page.evaluate((c) => {
          const ps = [...document.querySelectorAll(`${c} p`)];
          const el = ps.find((p) => p.textContent.startsWith("macOS, launchd"));
          return getComputedStyle(el).color;
        }, card),
        "rgb(107, 114, 128)",
      );
      await page.close();
    });

    await t.test("hosts H9 the routes table fits the card at phone width", async () => {
      const page = await open("#/settings/hosts", HOSTS);
      await page.click("#hosts-load");
      await page.waitForFunction(() => document.querySelectorAll(".host-card").length === 5, wait);
      const table = '.host-card[data-id="studio-mac"] .host-routes';
      const fallback = `${table} tbody tr:last-child`;
      const display = () =>
        page.$eval(`${fallback} td:last-child`, (el) => getComputedStyle(el).display);
      await page.setViewport({ width: 390, height: 844 });
      assert.equal(await page.$eval(table, (el) => el.scrollWidth <= el.clientWidth), true);
      assert.equal(await page.$eval(`${fallback} td`, (el) => el.textContent), "fallback");
      assert.deepEqual(
        await page.$$eval(`${fallback} td`, (cells) => cells.map((el) => el.dataset.label)),
        ["Route", "Provider", "Model", "Effort", "Limited until"],
      );
      assert.equal(await display(), "block");
      await page.setViewport({ width: 1280, height: 900 });
      assert.equal(await display(), "table-cell");
      await page.close();
    });

    await t.test("hosts H5 pickup checkboxes and the save-time check", async (st) => {
      const current = await fetch(origin + "/api/pipeline/projects/sample");
      const put = await fetch(origin + "/api/pipeline/projects/sample", {
        method: "PUT",
        headers: {
          ...(current.ok ? { "If-Match": current.headers.get("ETag") } : { "If-None-Match": "*" }),
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          schema: 1,
          pickup: { enabled: true, hosts: ["studio-mac", "old-laptop", "ghost-host"] },
        }),
      });
      assert.equal(put.status, 200);
      const page = await open("#/settings/pipeline", HOSTS);
      await waitScopeOption(page);
      await page.select("#pipeline-scope", "projects/sample");
      await page.waitForFunction(
        () => document.querySelectorAll("#pipeline-pickup-hosts input").length === 4,
        wait,
      );
      assert.deepEqual(await rows(page, "pipeline-pickup-hosts"), [
        ["lab-linux", false, ""],
        ["studio-mac", true, ""],
        ["old-laptop", true, "Retired"],
        ["ghost-host", true, "Not registered"],
      ]);
      await page.click('#pipeline-pickup-hosts input[value="lab-linux"]');
      assert.equal(
        await pipelineStatus(page),
        "Fix before saving: pickup.hosts: not registered: ghost-host",
      );
      assert.equal(await page.$eval("#pipeline-save", (el) => el.disabled), true);
      assert.equal(
        await page.$eval("#pipeline-pickup-hosts", (el) => el.getAttribute("aria-invalid")),
        "true",
      );
      await page.click('#pipeline-pickup-hosts input[value="ghost-host"]');
      assert.equal(await pipelineStatus(page), "Unsaved changes");
      assert.equal(await page.$$eval("#pipeline-pickup-hosts input", (els) => els.length), 3);
      assert.equal(await page.evaluate(() => document.activeElement.value), "lab-linux");
      await page.click("#pipeline-save");
      await page.waitForFunction(
        () => document.getElementById("pipeline-status").textContent.startsWith("Saved as version"),
        wait,
      );
      const stored = await (await fetch(origin + "/api/pipeline/projects/sample")).json();
      assert.deepEqual(stored.pickup.hosts, ["studio-mac", "old-laptop", "lab-linux"]);

      await page.click("#settings-nav-projects");
      await page.waitForFunction(
        () => document.querySelectorAll("#project-pickup-hosts input").length === 3,
        wait,
      );
      assert.deepEqual(await rows(page, "project-pickup-hosts"), [
        ["lab-linux", true, ""],
        ["studio-mac", true, ""],
        ["old-laptop", true, "Retired"],
      ]);

      await st.test("hosts H6 keyboard and mobile width on pickup", async () => {
        await page.click("#settings-nav-pipeline");
        await page.waitForSelector("#settings-pipeline:not([hidden])", wait);
        await page.select("#pipeline-scope", "projects/sample");
        await page.waitForFunction(
          () => !document.querySelector("#pipeline-pickup-hosts input").disabled,
          wait,
        );
        await page.focus("#pipeline-pickup-mode");
        const reached = [];
        for (let i = 0; i < 3; i++) {
          await page.keyboard.press("Tab");
          reached.push(await page.evaluate(() => document.activeElement.value));
        }
        assert.deepEqual(reached, ["lab-linux", "studio-mac", "old-laptop"]);
        await page.keyboard.press("Space");
        assert.equal(await pipelineStatus(page), "Unsaved changes");
        await page.setViewport({ width: 390, height: 844 });
        await page.waitForFunction(() => document.documentElement.scrollWidth <= 390, wait);
      });
      await page.close();

      const failed = await open("#/settings/pipeline", null);
      await waitScopeOption(failed);
      await failed.select("#pipeline-scope", "projects/sample");
      await failed.waitForFunction(
        () =>
          document
            .getElementById("pipeline-pickup-hosts")
            .textContent.includes("Could not load registered hosts. Stored hosts show by id.") &&
          document.querySelectorAll("#pipeline-pickup-hosts input").length === 3,
        wait,
      );
      assert.deepEqual(await rows(failed, "pipeline-pickup-hosts"), [
        ["studio-mac", true, ""],
        ["old-laptop", true, ""],
        ["lab-linux", true, ""],
      ]);
      await failed.click('#pipeline-pickup-hosts input[value="old-laptop"]');
      assert.equal(await pipelineStatus(failed), "Unsaved changes");
      assert.equal(await failed.$eval("#pipeline-save", (el) => el.disabled), false);
      await failed.close();
    });

    assert.deepEqual(errors, []);
    await context.close();
  });
});
