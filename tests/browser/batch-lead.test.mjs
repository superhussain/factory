// A batch lead's card and drawer name its riders, and a rider offers no Move, Prioritize, Pause,
// Rerun or Close. Captures the lead card at desktop light and mobile dark and a rider's open menu.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const SHOTS = new URL("../../.context/shots/", import.meta.url).pathname;
const NOW = "2026-01-01T12:00:00Z";
const LEAD = "090-sample-export-csv";
const TYPO = "091-sample-typo";
const NAME = "092-sample-csv-name";
const rider = (id, title) => ({
  id,
  project: "sample",
  title,
  stage: "build",
  round: 0,
  refs: [{ id: LEAD, kind: "batched-into" }],
  updated: NOW,
});
const JOBS = [
  {
    id: LEAD,
    project: "sample",
    title: "Add CSV export to the weekly report",
    stage: "build",
    branch: `factory/${LEAD}`,
    round: 0,
    claim: { device: "lab-linux", session: "s1", since: NOW },
    updated: NOW,
  },
  rider(TYPO, "Fix the Weekley typo in the weekly report header"),
  rider(NAME, "Rename the CSV file after the report"),
  {
    id: "093-sample-login",
    project: "sample",
    title: "Show the login error",
    stage: "idea",
    idea: "Show the login error",
    updated: NOW,
  },
];
const RIDERS_LINE = `batches ${TYPO} · ${NAME}`;
const FIVE = ["d-move-host", "d-rerun", "d-prioritize", "d-pause", "d-close-job"];
const card = (id) => `.card[data-id="${id}"]`;

async function openBoard(browser, origin, width, height, scheme) {
  const page = await browser.newPage();
  await page.setViewport({ width, height });
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
  await blockLiveEvents(page);
  await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
  await page.waitForSelector(card(LEAD));
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
  return page;
}

async function shoot(page, selector, name) {
  const clip = await page.evaluate((s) => {
    const { x, y, width, height } = document.querySelector(s).getBoundingClientRect();
    return { x: x + scrollX, y: y + scrollY, width, height };
  }, selector);
  await page.screenshot({ path: `${SHOTS}${name}.png`, clip });
}

const cardMenu = (page) =>
  page.$$eval("#card-actions-menu [data-action]", (b) =>
    b.filter((x) => !x.hidden).map((x) => x.dataset.action),
  );
const drawerMenu = (page) =>
  page.$$eval("#d-actions-menu button", (b) => b.filter((x) => !x.hidden).map((x) => x.id));

const drawerOpen = (page, title) =>
  page.waitForFunction((t) => document.querySelector("#d-title").textContent === t, {}, title);

async function openCardMenu(page, id) {
  await page.click(`.card-actions[data-actions="${id}"]`);
  await page.waitForSelector("#card-actions-menu:not([hidden])");
}

test("batch lead riders and rider actions", { timeout: 120000 }, async () => {
  await prepareAssets();
  await mkdir(SHOTS, { recursive: true });
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);

    const shots = [
      [1440, 900, "light", "desktop"],
      [390, 844, "dark", "mobile"],
    ];
    for (const [width, height, scheme, suffix] of shots) {
      const page = await openBoard(browser, origin, width, height, scheme);
      await shoot(page, card(LEAD), `batch-lead-${suffix}`);
      await page.close();
    }
    const page = await openBoard(browser, origin, 1440, 900, "light");
    await openCardMenu(page, TYPO);
    await shoot(page, "#card-actions-menu", "rider-menu-desktop");
    const riderMenu = await cardMenu(page);
    await page.keyboard.press("Escape");

    // 1. The lead card names its riders and each id opens its job.
    assert.equal(await page.$eval(`${card(LEAD)} .riders`, (el) => el.textContent), RIDERS_LINE);
    await page.click(`${card(LEAD)} .riders .ref-link[data-ref="${TYPO}"]`);
    await drawerOpen(page, JOBS[1].title);

    // 3. The rider's card and drawer menus omit the five actions and keep Copy link.
    assert.deepEqual(
      riderMenu.filter((a) => FIVE.includes(a)),
      [],
    );
    assert.ok(riderMenu.includes("d-copy-link"));
    const riderDrawer = await drawerMenu(page);
    assert.deepEqual(
      riderDrawer.filter((a) => FIVE.includes(a)),
      [],
    );
    assert.ok(riderDrawer.includes("d-copy-link"));

    // 2. The lead drawer carries the same line with a link per rider; its menu keeps all five.
    await page.evaluate((id) => (location.hash = id), LEAD);
    await drawerOpen(page, JOBS[0].title);
    assert.ok((await page.$eval("#d-refs", (el) => el.textContent)).includes(RIDERS_LINE));
    assert.deepEqual(await page.$$eval("#d-refs a", (a) => a.map((x) => x.getAttribute("href"))), [
      `#${TYPO}`,
      `#${NAME}`,
    ]);
    assert.deepEqual(
      (await drawerMenu(page)).filter((a) => FIVE.includes(a)).sort(),
      [...FIVE].sort(),
    );
    await openCardMenu(page, LEAD);
    assert.deepEqual(
      (await cardMenu(page)).filter((a) => FIVE.includes(a)).sort(),
      [...FIVE].sort(),
    );

    // 4. The writers refuse a claimed rider and leave it unchanged.
    const results = await page.evaluate(
      (id, lead) => {
        const claimed = { id, project: "sample", stage: "build", claim: { device: "lab-linux" } };
        const rows = {};
        const run = (name, call, withRef) => {
          const board = structuredClone({
            jobs: [{ ...claimed, ...(withRef && { refs: [{ id: lead, kind: "batched-into" }] }) }],
          });
          const before = JSON.stringify(board);
          rows[name] = [call(board), JSON.stringify(board) === before];
        };
        for (const withRef of [true, false]) {
          const k = withRef ? "rider " : "plain ";
          run(k + "move", (b) => moveJob(b, id, "lab-mac", "t"), withRef);
          run(k + "rerun", (b) => rerunJob(b, id, "lab-linux", "t"), withRef);
          run(k + "prioritize", (b) => prioritizeJob(b, id, "t"), withRef);
          run(k + "pause", (b) => pauseJob(b, id, "t"), withRef);
          run(k + "close", (b) => closeJob(b, id, "done", "t"), withRef);
        }
        return rows;
      },
      TYPO,
      LEAD,
    );
    assert.deepEqual(results["rider move"], ["This job can no longer be moved.", true]);
    assert.deepEqual(results["rider rerun"], ["This job can no longer be rerun.", true]);
    assert.deepEqual(results["rider prioritize"], ["This job can no longer be prioritized.", true]);
    assert.deepEqual(results["rider pause"], ["This job can no longer be paused.", true]);
    assert.deepEqual(results["rider close"], ["This job can no longer be closed.", true]);
    assert.equal(results["plain move"][0], "");
    await page.close();
  });
});
