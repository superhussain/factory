// A reassigned job keeps the PR and build runs its previous project's repo hosted, so
// prLink/buildRuns must match against every registered project's repo, own project
// first — while a URL under an unregistered repo still renders no link.
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedProject,
  seedBoard,
  blockLiveEvents,
} from "../support/browser-server.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const REASSIGNED = {
  id: "030-reassigned",
  project: "new",
  title: "Synthetic reassigned job",
  stage: "closed",
  updated: "2026-01-01T00:00:00Z",
  pr: "https://github.com/example/old/pull/7",
  build: { state: "done", runs: ["https://github.com/example/old/actions/runs/1234"] },
};
const FOREIGN = {
  id: "031-foreign",
  project: "new",
  title: "Synthetic unregistered-repo job",
  stage: "closed",
  updated: "2026-01-01T00:00:00Z",
  pr: "https://github.com/example/other/pull/9",
};
const ARCHIVED = {
  id: "032-archived",
  project: "new",
  title: "Synthetic archived job",
  stage: "merged",
  updated: "2026-01-01T00:00:00Z",
  pr: "https://github.com/example/new-private/pull/5",
};
const CURRENT = {
  id: "033-current",
  project: "new",
  title: "Synthetic current job",
  stage: "merged",
  updated: "2026-01-01T00:00:00Z",
  pr: "https://github.com/example/new/pull/5",
};
const DOC = "# Synthetic job\n\nMerged as #5.\n";
const captureDir = process.env.CAPTURE_DIR;

async function seed(origin) {
  await seedProject(
    origin,
    "new",
    "---\nrepo: example/new\nbase: main\ntitle: Synthetic new\n---\n",
  );
  await seedProject(
    origin,
    "old",
    "---\nrepo: example/old\nbase: main\ntitle: Synthetic old\n---\n",
  );
  await seedBoard(origin, [REASSIGNED, FOREIGN]);
}

test(
  "reassigned job keeps PR and build-run links across projects",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seed(origin);
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport({ width: 1440, height: 900 });
        const errors = [];
        page.on("pageerror", (error) => errors.push(String(error)));
        await blockLiveEvents(page);
        await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
        await waitForBoard(page);
        // Terminal columns start collapsed; expand the closed column like a user would.
        await page.click('.column-toggle[data-stage="closed"]');
        await page.waitForSelector('.card[data-id="030-reassigned"]', { visible: true });

        // The card renders the PR link to the old repo.
        const cardLink = await page.evaluate(() => {
          const a = document.querySelector(
            '.card-wrap:has(.card[data-id="030-reassigned"]) .pr-link',
          );
          return a ? { href: a.href, text: a.textContent } : null;
        });
        assert.deepEqual(cardLink, { href: REASSIGNED.pr, text: "PR #7" });

        // The foreign-repo PR stays a plain link-less card.
        const foreignLink = await page.evaluate(() =>
          document.querySelector('.card-wrap:has(.card[data-id="031-foreign"]) .pr-link'),
        );
        assert.equal(foreignLink, null);

        // The drawer renders the PR link and the build run under the old repo.
        await clickNow(page, '.card[data-id="030-reassigned"]');
        await page.waitForFunction(
          () =>
            !document.getElementById("detail").hidden &&
            document.getElementById("d-pr").textContent === "PR #7",
        );
        const drawer = await page.evaluate(() => ({
          pr: {
            hidden: document.getElementById("d-pr").hidden,
            href: document.getElementById("d-pr").href,
          },
          run: (() => {
            const a = document.querySelector(
              '#detail a[href="https://github.com/example/old/actions/runs/1234"]',
            );
            return a ? { text: a.textContent } : null;
          })(),
        }));
        assert.equal(drawer.pr.hidden, false);
        assert.equal(drawer.pr.href, REASSIGNED.pr);
        assert.deepEqual(drawer.run, { text: "run 1234" });
        assert.deepEqual(errors, [], "no page errors");
      } finally {
        await context.close();
      }
    });
  },
);

test(
  "archived PR and document links follow the project's archive",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    if (captureDir) await mkdir(captureDir, { recursive: true });
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(
        origin,
        "new",
        "---\nrepo: example/new\nbase: main\ntitle: Synthetic new\n---\n",
      );
      await seedBoard(origin, [ARCHIVED, CURRENT]);
      for (const id of [ARCHIVED.id, CURRENT.id]) {
        const response = await fetch(origin + "/api/jobs/new/" + id + "/spec.md", {
          method: "PUT",
          headers: { "If-None-Match": "*", "Content-Type": "text/markdown" },
          body: DOC,
        });
        assert.equal(response.status, 200);
      }
      const context = await browser.createBrowserContext();
      try {
        const page = await context.newPage();
        await page.setViewport({ width: 1440, height: 900 });
        const errors = [];
        page.on("pageerror", (error) => errors.push(String(error)));
        await blockLiveEvents(page);
        const wrap = '.card-wrap:has(.card[data-id="032-archived"])';
        // Only a linked card gets a wrap, so the unlinked card is cropped on its own box.
        const shown = wrap + ', .cards > .card[data-id="032-archived"]';
        const open = async (frontmatter, shot) => {
          await seedProject(origin, "new", "---\n" + frontmatter + "title: Synthetic new\n---\n");
          await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
          await waitForBoard(page);
          await page.click('.column-toggle[data-stage="merged"]');
          await page.waitForSelector(shown, { visible: true });
          if (captureDir) await (await page.$(shown)).screenshot({ path: join(captureDir, shot) });
          return page.evaluate((sel) => {
            const a = document.querySelector(sel + " .pr-link");
            return a ? { href: a.href, text: a.textContent } : null;
          }, wrap);
        };

        const docHref = async (id, shot) => {
          await clickNow(page, '.card[data-id="' + id + '"]');
          await page.waitForFunction(() =>
            document.getElementById("d-doc").textContent.includes("Merged as"),
          );
          if (captureDir && shot) {
            await (await page.$("#d-doc")).screenshot({ path: join(captureDir, shot) });
          }
          return page.evaluate(
            () => document.querySelector('#d-doc a[href^="https://github.com/"]')?.href,
          );
        };

        assert.equal(
          await open("repo: example/new\nbase: main\n", "before-card-archive.png"),
          null,
        );
        assert.equal(
          await docHref("032-archived", "before-doc-archive-link.png"),
          "https://github.com/example/new/pull/5",
        );
        assert.deepEqual(
          await open(
            "repo: example/new\nbase: main\narchive: example/new-private\n",
            "after-card-archive.png",
          ),
          { href: ARCHIVED.pr, text: "PR #5" },
        );
        assert.equal(
          await docHref("032-archived", "after-doc-archive-link.png"),
          "https://github.com/example/new-private/pull/5",
        );
        await page.waitForFunction(() => document.getElementById("d-pr").textContent === "PR #5");
        assert.equal(await page.evaluate(() => document.getElementById("d-pr").href), ARCHIVED.pr);

        await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
        await waitForBoard(page);
        await page.click('.column-toggle[data-stage="merged"]');
        await page.waitForSelector('.card[data-id="033-current"]', { visible: true });
        assert.equal(await docHref("033-current", null), "https://github.com/example/new/pull/5");
        assert.deepEqual(errors, [], "no page errors");
      } finally {
        await context.close();
      }
    });
  },
);
