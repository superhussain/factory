// A run of "#### 1. Title", "#### 2. Title" headings renders as a stepper inside #d-doc:
// pills, Back/Next with wrap, hidden-step mermaid, arrows scoped to the stepper, step kept
// across re-render, Find in page, and non-runs left plain. node --test.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { waitForBoard, clickNow } from "../support/board-ready.cjs";

const STEP_DOC = `# Spec: slash opens the jumper

Synthetic stepper fixture.

## Design

Running example: the \`/\` key on the board.

#### 1. Today

\`\`\`mermaid
flowchart LR
  K[keydown] --> H[handleBoardKeys]
  H -->|key j| O[openJobPicker]
\`\`\`

- \`handleBoardKeys\` handles single-key openers.

#### 2. The gap

| Key | Today |
| --- | --- |
| \`j\` | Jumper opens |
| \`/\` | Nothing |

- \`/\` falls through every branch.

#### 3. The change: \`/\` joins \`j\`

\`\`\`mermaid
flowchart LR
  H[handleBoardKeys] -->|key j or slash| O[openJobPicker]
  classDef added fill:#d4f7d4,stroke:#2a7a2a
  class O added
\`\`\`

- Change the condition to \`e.key === 'j' || e.key === '/'\`.

#### 4. Consequences

- \`/\` typed in an input still types a slash.
- \`/\` while a dialog is open is ignored.

#### 5. Before and after

| | Before | After |
| --- | --- | --- |
| \`/\` on board | Nothing | Jumper opens |

## Acceptance criteria

1. \`/\` opens the jumper.
`;

const NOT_STEPS = `# Build

#### 2. Starts at two

- No step one, so no stepper.

#### 1. Lone step

- One step only, so no stepper.
`;

const state = () =>
  document.querySelector("#d-doc .stepper") &&
  (() => {
    const box = document.querySelector("#d-doc .stepper");
    const pills = [...box.querySelectorAll(".step-pill")];
    const panels = [...box.querySelectorAll(".step-panel")];
    return {
      current: pills.findIndex((b) => b.getAttribute("aria-current") === "step") + 1,
      kicker: panels.find((p) => !p.hidden)?.querySelector(".step-kicker").textContent,
      count: box.querySelector(".step-count").textContent,
      tab: document.querySelector(".tab.active")?.dataset.name,
    };
  })();

test("numbered step headings render as a stepper", async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      {
        id: "001-stepper",
        project: "sample",
        stage: "build",
        title: "Synthetic stepper",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    for (const [name, body] of [
      ["spec.md", STEP_DOC],
      ["build.md", NOT_STEPS],
    ]) {
      const put = await fetch(origin + "/api/jobs/sample/001-stepper/" + name, {
        method: "PUT",
        headers: { "Content-Type": "text/markdown" },
        body,
      });
      assert.equal(put.status, 200);
    }

    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
    const external = [],
      errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await page.setRequestInterception(true);
    page.on("request", (request) => {
      if (new URL(request.url()).origin !== origin) {
        external.push(request.url());
        return request.abort();
      }
      return request.continue();
    });
    try {
      await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
      await waitForBoard(page);
      await page.waitForSelector('.card[data-id="001-stepper"]');
      await clickNow(page, '.card[data-id="001-stepper"]');
      await page.waitForSelector("#d-doc .stepper");

      // 1. One stepper, five labelled pills, the first current.
      const pills = await page.evaluate(() => ({
        boxes: document.querySelectorAll("#d-doc .stepper").length,
        labels: [...document.querySelectorAll("#d-doc .step-pill")].map((b) =>
          b.getAttribute("aria-label"),
        ),
        current: [...document.querySelectorAll("#d-doc .step-pill")].map((b) =>
          b.getAttribute("aria-current"),
        ),
      }));
      assert.equal(pills.boxes, 1);
      assert.deepEqual(pills.labels, [
        "Step 1: Today",
        "Step 2: The gap",
        "Step 3: The change: / joins j",
        "Step 4: Consequences",
        "Step 5: Before and after",
      ]);
      assert.deepEqual(pills.current, ["step", null, null, null, null]);

      // 2. Kicker, title, visual then bullets, count; the stepper sits between intro and next h2.
      const layout = await page.evaluate(() => {
        const box = document.querySelector("#d-doc .stepper");
        const panel = box.querySelector(".step-panel:not([hidden])");
        return {
          kicker: panel.querySelector(".step-kicker").textContent,
          order: [...panel.children].map((el) => el.className || el.tagName),
          title: panel.querySelector(".step-title").textContent,
          count: box.querySelector(".step-count").textContent,
          before: box.previousElementSibling.textContent,
          after: box.nextElementSibling.tagName + " " + box.nextElementSibling.textContent,
        };
      });
      assert.equal(layout.kicker, "Step 1 of 5");
      assert.equal(layout.title, "Today");
      assert.deepEqual(layout.order.slice(0, 2), ["step-kicker", "step-title"]);
      assert.equal(layout.order.at(-1), "UL");
      assert.equal(layout.count, "1 / 5");
      assert.ok(layout.before.startsWith("Running example"), layout.before);
      assert.equal(layout.after, "H2 Acceptance criteria");

      // 3. Mermaid renders in every step, including the hidden step 3.
      await page.waitForFunction(
        () => document.querySelectorAll("#d-doc .mermaid svg").length === 2,
        { timeout: 30000 },
      );
      assert.equal(
        await page.evaluate(
          () =>
            document.querySelectorAll("#d-doc .step-panel")[2].querySelectorAll(".mermaid svg")
              .length,
        ),
        1,
      );

      // 4. Next twice shows only step 3, with a laid-out diagram.
      const next = () => page.click("#d-doc .step-nav .btn:last-child");
      const back = () => page.click("#d-doc .step-nav .btn:first-child");
      await next();
      await next();
      const shown = await page.evaluate(() => {
        const panels = [...document.querySelectorAll("#d-doc .step-panel")];
        const r = panels[2].querySelector(".mermaid svg").getBoundingClientRect();
        return {
          heights: panels.map((p) => p.offsetHeight > 0),
          hidden: panels.map((p) => p.getAttribute("hidden")),
          svg: r.width > 0 && r.height > 0,
        };
      });
      assert.equal((await page.evaluate(state)).current, 3);
      assert.deepEqual(shown.heights, [false, false, true, false, false]);
      assert.deepEqual(shown.hidden, [
        "until-found",
        "until-found",
        null,
        "until-found",
        "until-found",
      ]);
      assert.ok(shown.svg, "step 3 diagram has geometry");

      // 5. Back three times wraps from step 1 to step 5.
      await back();
      await back();
      await back();
      assert.deepEqual(await page.evaluate(state), {
        current: 5,
        kicker: "Step 5 of 5",
        count: "5 / 5",
        tab: "spec.md",
      });

      // 6. ArrowRight with focus on Next wraps to step 1 and leaves the tab alone.
      await page.focus("#d-doc .step-nav .btn:last-child");
      await page.keyboard.press("ArrowRight");
      assert.deepEqual(await page.evaluate(state), {
        current: 1,
        kicker: "Step 1 of 5",
        count: "1 / 5",
        tab: "spec.md",
      });

      // 7. ArrowRight on pill 2 selects step 3 and moves focus to pill 3.
      await page.click("#d-doc .step-pill:nth-child(2)");
      await page.focus("#d-doc .step-pill:nth-child(2)");
      await page.keyboard.press("ArrowRight");
      assert.equal((await page.evaluate(state)).current, 3);
      assert.equal(
        await page.evaluate(() => document.activeElement.getAttribute("aria-label")),
        "Step 3: The change: / joins j",
      );

      // 8. A re-render keeps step 3 and renders both diagrams again.
      await page.evaluate(() => renderDetail());
      await page.waitForFunction(
        () => document.querySelectorAll("#d-doc .mermaid svg").length === 2,
        { timeout: 30000 },
      );
      assert.equal((await page.evaluate(state)).current, 3);

      // 9. Find in page revealing step 4 selects it.
      await page.evaluate(() =>
        document.querySelectorAll("#d-doc .step-panel")[3].dispatchEvent(new Event("beforematch")),
      );
      assert.equal((await page.evaluate(state)).current, 4);

      // 10. Outside the stepper ArrowRight still moves to the next tab.
      await page.evaluate(() => document.activeElement.blur());
      assert.equal(await page.evaluate(() => document.activeElement === document.body), true);
      await page.keyboard.press("ArrowRight");
      await page.waitForFunction(
        () => document.querySelector(".tab.active")?.dataset.name === "build.md",
      );

      // 11. Headings that are not a run of two or more from 1 stay plain.
      await page.waitForFunction(
        () => document.querySelector("#d-doc h1")?.textContent === "Build",
      );
      const plain = await page.evaluate(() => ({
        steppers: document.querySelectorAll("#d-doc .stepper").length,
        h4: [...document.querySelectorAll("#d-doc h4")].map((h) => h.textContent),
      }));
      assert.deepEqual(plain, { steppers: 0, h4: ["2. Starts at two", "1. Lone step"] });

      // 12. Offline and error free.
      assert.deepEqual(external, [], "no external request was attempted");
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  });
});
