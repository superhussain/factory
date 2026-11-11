// The drawer's job actions sit behind one ellipsis toggle that opens an accessible menu.
// The menu works with keyboard, mouse and touch. A closed job still offers Copy link and Add follow-up idea.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const IDEA = {
  id: "100-sample-idea",
  project: "sample",
  title: "Synthetic sample idea",
  stage: "idea",
  idea: "Synthetic sample idea",
  updated: "2026-01-01T00:00:00Z",
  priority: { at: "2026-01-01T00:00:00Z" },
};
const CLOSED = {
  id: "101-sample-closed",
  project: "sample",
  title: "Synthetic closed job",
  stage: "closed",
  updated: "2026-01-01T00:00:00Z",
};
const ITEMS = ["d-edit-idea", "d-discard", "d-follow-tracer", "d-pause", "d-copy-link"];
const VIEWPORTS = [
  { width: 390, height: 844, isMobile: true, hasTouch: true },
  { width: 1280, height: 900 },
];

async function openDrawer(browser, origin, viewport, id) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  await page.setViewport(viewport);
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.goto(origin + "/#" + id, { waitUntil: "domcontentloaded" });
  await page.waitForFunction(
    (id) => document.getElementById("d-meta").textContent.includes(id),
    {},
    id,
  );
  return { context, page, errors };
}

const state = (page) =>
  page.evaluate(
    (items) => ({
      expanded: document.getElementById("d-actions").getAttribute("aria-expanded"),
      menu: !document.getElementById("d-actions-menu").hidden,
      shown: items.filter((id) => document.getElementById(id).getClientRects().length > 0),
      focus: document.activeElement.id,
      drawer: !document.getElementById("detail").hidden,
    }),
    ITEMS,
  );
const closedAt = (focus) => ({ expanded: "false", menu: false, shown: [], focus, drawer: true });
const openAt = (focus) => ({ expanded: "true", menu: true, shown: ITEMS, focus, drawer: true });

test("The job actions menu works by keyboard, pointer and touch", { timeout: 120000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [IDEA, CLOSED]);
    for (const viewport of VIEWPORTS) {
      const label = `${viewport.width}px`;
      const { context, page, errors } = await openDrawer(browser, origin, viewport, IDEA.id);
      try {
        const press = async (key, focus, open) => {
          await page.keyboard.press(key);
          assert.deepEqual(
            await state(page),
            open ? openAt(focus) : closedAt(focus),
            `${label} ${key}`,
          );
        };
        const activate = (selector) =>
          viewport.hasTouch ? page.tap(selector) : page.click(selector);

        assert.equal(
          await page.$eval("#d-actions", (el) => el.getClientRects().length > 0),
          true,
          label,
        );
        assert.deepEqual({ ...(await state(page)), focus: "" }, closedAt(""), label);

        await page.focus("#d-actions");
        await press("Enter", "d-edit-idea", true);
        await press("Escape", "d-actions", false);
        await press("Space", "d-edit-idea", true);
        await press("Escape", "d-actions", false);
        await press("ArrowUp", "d-copy-link", true);
        await press("ArrowDown", "d-edit-idea", true);
        await press("ArrowUp", "d-copy-link", true);
        await press("ArrowUp", "d-pause", true);
        await press("Home", "d-edit-idea", true);
        await press("End", "d-copy-link", true);
        await press("Escape", "d-actions", false);
        await press("ArrowDown", "d-edit-idea", true);
        await press("Tab", "d-close", false);
        await page.focus("#d-actions");
        await press("ArrowDown", "d-edit-idea", true);
        await page.keyboard.down("Shift");
        await page.keyboard.press("Tab");
        await page.keyboard.up("Shift");
        const back = await state(page);
        assert.deepEqual({ ...back, focus: "" }, closedAt(""), `${label} Shift+Tab`);
        assert.notEqual(back.focus, "d-actions", `${label} Shift+Tab leaves the toggle`);
        await press("Tab", "d-actions", false);

        await activate("#d-actions");
        assert.deepEqual(await state(page), openAt("d-edit-idea"), `${label} open by pointer`);
        const rect = (sel) => page.$eval(sel, (el) => el.getBoundingClientRect().toJSON());
        const [menu, head, button] = [
          await rect("#d-actions-menu"),
          await rect(".detail-head"),
          await rect("#d-actions"),
        ];
        if (viewport.width <= 760) assert.ok(menu.top >= head.bottom, `${label} menu below header`);
        else
          assert.equal(Math.round(menu.top - button.bottom), 4, `${label} menu under its button`);
        await activate("#d-actions");
        assert.deepEqual(await state(page), closedAt("d-actions"), `${label} close by pointer`);

        // Focus sits outside the menu, so hiding the menu cannot drop it.
        await activate("#d-actions");
        await page.focus("#d-close");
        await page.$eval("#d-title", (el) =>
          el.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true })),
        );
        assert.deepEqual(await state(page), closedAt("d-close"), `${label} pointer outside`);
        await activate("#d-actions");
        await activate("#d-title");
        const outside = await state(page);
        assert.equal(outside.menu, false, `${label} real pointer outside`);
        assert.notEqual(outside.focus, "d-actions", `${label} real pointer outside`);

        await activate("#d-actions");
        await activate("#d-edit-idea");
        await page.waitForFunction(() => document.getElementById("edit-idea-dlg").open);
        assert.equal((await state(page)).menu, false, `${label} item closes the menu`);
        await page.keyboard.press("Escape");
        await page.waitForFunction(() => !document.getElementById("edit-idea-dlg").open);
        assert.deepEqual(await state(page), closedAt("d-actions"), `${label} dialog returns focus`);
        assert.deepEqual(errors, [], label);
      } finally {
        await context.close();
      }

      const closed = await openDrawer(browser, origin, viewport, CLOSED.id);
      try {
        assert.deepEqual(
          await closed.page.$eval("#d-actions-wrap", (el) => ({
            hidden: el.hidden,
            rendered: el.getClientRects().length > 0,
          })),
          { hidden: false, rendered: true },
          `${label} closed job`,
        );
        await closed.page.click("#d-actions");
        await closed.page.waitForSelector("#d-actions-menu:not([hidden])");
        assert.deepEqual(
          await closed.page.$$eval("#d-actions-menu [role=menuitem]", (els) =>
            els.filter((el) => el.getClientRects().length > 0).map((el) => el.id),
          ),
          ["d-copy-link", "d-follow-up"],
          `${label} closed job menu`,
        );
        assert.deepEqual(closed.errors, [], label);
      } finally {
        await closed.context.close();
      }
    }
  });
});
