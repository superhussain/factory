// A job a person decided in chat shows how, with the quote in the chip's title. Others show none.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";

const AT = "2026-01-02T12:00:00Z";
const backlog = (decision) => ({
  line: "- [ ] sample: Synthetic item",
  decision,
  at: AT,
  via: "chat",
  quote: "Approve   the\nexport idea.",
  by: "host-a",
});
const job = (id, extra) => ({
  id,
  project: "sample",
  title: id,
  stage: "idea",
  idea: id,
  updated: AT,
  ...extra,
});
const JOBS = [
  job("301-chat-approved", { backlog: backlog("approved") }),
  job("302-chat-rework", { backlog: backlog("rework") }),
  job("303-chat-denied", { stage: "closed", backlog: backlog("denied") }),
  job("304-gate-approved", {
    stage: "spec",
    origin: {
      gate: "human",
      approved: { option: "a", via: "chat", quote: "Go with a.", device: "host-b", at: AT },
    },
  }),
  job("305-dashboard", { backlog: { ...backlog("approved"), via: undefined } }),
];

test("chat decision chip", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, JOBS);
    const errors = [];
    const page = await browser.newPage();
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector('.card[data-id="305-dashboard"]');
    const chips = await page.$$eval(".card", (cards) =>
      Object.fromEntries(
        cards.map((c) => {
          const chip = c.querySelector(".chip.chat");
          return [c.dataset.id, chip && [chip.textContent, chip.title]];
        }),
      ),
    );
    const said = 'by host-a at 2026-01-02T12:00:00Z: "Approve the export idea."';
    assert.deepEqual(chips["301-chat-approved"], ["Approved in chat", "Approved in chat " + said]);
    assert.deepEqual(chips["302-chat-rework"], ["Rework in chat", "Rework in chat " + said]);
    assert.deepEqual(chips["304-gate-approved"], [
      "Approved in chat",
      'Approved in chat by host-b at 2026-01-02T12:00:00Z: "Go with a."',
    ]);
    assert.equal(chips["305-dashboard"], null);
    assert.deepEqual(chips["303-chat-denied"], ["Denied in chat", "Denied in chat " + said]);
    await page.evaluate(() => {
      location.hash = "#301-chat-approved";
    });
    await page.waitForSelector("#d-chat:not([hidden])");
    assert.equal(await page.$eval("#d-chat", (el) => el.textContent), "Approved in chat " + said);
    await page.evaluate(() => {
      location.hash = "#305-dashboard";
    });
    await page.waitForFunction(() =>
      document.getElementById("d-meta").textContent.includes("305-dashboard"),
    );
    assert.equal(await page.$eval("#d-chat", (el) => el.hidden), true);
    assert.deepEqual(errors, []);
  });
});
