// Held-by-triage section in the backlog drawer against one owned local Worker.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject } from "../support/browser-server.mjs";

const wait = { polling: 50 };
const MODEL_ITEM = "sample: Add billing totals to the CSV export";
const FIXED_ITEM = "sample: Rename the export button";
const TRAILING_ITEM = "sample: Add a sidebar";
const MISSING_ITEM = "sample: Add a footer";
const MALFORMED_ITEM = "sample: Add a header";
const LATE_ITEM = "sample: Add a changelog";
const MODEL_VERDICT = {
  item: MODEL_ITEM,
  decision: "hold",
  confidence: 62,
  threshold: 80,
  reason: "Rated by clef-flash against the project rules.",
  model: "@cf/cloudflare/clef-flash",
  at: new Date(Date.now() - 2 * 3600 * 1000).toISOString(),
};
const LATE_VERDICT = { ...MODEL_VERDICT, item: LATE_ITEM, confidence: 40 };

test("backlog triage section", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    const send = (method, path, body, headers = {}) =>
      fetch(origin + path, {
        method,
        headers: { "Content-Type": "application/json", ...headers },
        body,
      });
    assert.equal(
      (
        await send(
          "PUT",
          "/api/pipeline/projects/sample",
          '{"schema":1,"triage":{"enabled":true,"threshold":80,"rules":""}}',
          { "If-None-Match": "*" },
        )
      ).status,
      200,
    );
    const posted = await send(
      "POST",
      "/api/backlog/triage",
      JSON.stringify({ item: FIXED_ITEM, context: "Recommend: deny\nSynthetic reason." }),
    );
    assert.equal(posted.status, 200);
    assert.equal((await posted.json()).decision, "hold");
    const trailing = await send(
      "POST",
      "/api/backlog/triage",
      JSON.stringify({ item: TRAILING_ITEM, context: "Recommend: deny\nSynthetic reason." }),
    );
    assert.equal(trailing.status, 200);
    const lines = [MODEL_ITEM, FIXED_ITEM, TRAILING_ITEM + "   ", MISSING_ITEM, MALFORMED_ITEM];
    assert.equal(
      (
        await send(
          "PUT",
          "/api/backlog",
          "# Backlog\n\n" + lines.map((l) => `- [ ] ${l}\n`).join(""),
          {
            "If-None-Match": "*",
            "Content-Type": "text/markdown",
          },
        )
      ).status,
      200,
    );

    const page = await (await browser.createBrowserContext()).newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    let releaseLate;
    const late = new Promise((resolve) => {
      releaseLate = resolve;
    });
    const verdictReads = [];
    let itemReads = 0;
    await page.setRequestInterception(true);
    page.on("request", async (request) => {
      const url = new URL(request.url());
      if (url.pathname === "/api/backlog/triage") itemReads++;
      if (url.pathname !== "/api/backlog/triage/verdicts") return request.continue();
      const body = request.postData();
      const { items } = JSON.parse(body);
      verdictReads.push(items);
      const real = await fetch(origin + url.pathname, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
      });
      const { verdicts } = await real.json();
      items.forEach((item, i) => {
        if (item === MODEL_ITEM) verdicts[i] = MODEL_VERDICT;
        if (item === MALFORMED_ITEM) verdicts[i] = { bad: 1 };
        if (item === LATE_ITEM) verdicts[i] = LATE_VERDICT;
      });
      if (items.includes(LATE_ITEM)) await late;
      return request.respond({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ verdicts }),
      });
    });
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await page.waitForSelector(".backlog-card", wait);

    const open = async (line) => {
      await page.evaluate((text) => {
        [...document.querySelectorAll(".backlog-card")]
          .find((card) => card.textContent.includes(text))
          .click();
      }, line.slice("sample: ".length));
      await page.waitForSelector("#backlog-form", wait);
    };
    const section = () =>
      page.$eval("#backlog-triage", (el) => ({
        hidden: el.hidden,
        heading: el.querySelector("h3")?.textContent ?? null,
        labelledBy:
          document.getElementById(el.getAttribute("aria-labelledby"))?.textContent ?? null,
        paragraphs: [...el.querySelectorAll("p")].map((p) => p.textContent),
        before: !!(el.compareDocumentPosition(document.querySelector("#backlog-form")) & 4),
      }));

    await t.test("the form does not move when the verdict is cached", async () => {
      await page.waitForFunction(() => backlogTriageSeen.size === 5, wait);
      assert.deepEqual(verdictReads, [
        [MODEL_ITEM, FIXED_ITEM, TRAILING_ITEM, MISSING_ITEM, MALFORMED_ITEM],
      ]);
      await page.evaluate(() => renderBoard());
      assert.equal(verdictReads.length, 1);
      const opened = await page.evaluate((text) => {
        [...document.querySelectorAll(".backlog-card")]
          .find((card) => card.textContent.includes(text))
          .click();
        return {
          hidden: document.getElementById("backlog-triage").hidden,
          top: document.getElementById("backlog-form").getBoundingClientRect().top,
        };
      }, MODEL_ITEM.slice("sample: ".length));
      assert.equal(opened.hidden, false);
      await new Promise((resolve) => setTimeout(resolve, 300));
      assert.equal(
        await page.$eval("#backlog-form", (el) => el.getBoundingClientRect().top),
        opened.top,
      );
    });

    await t.test("a model hold shows the score, the threshold and the reason", async () => {
      await open(MODEL_ITEM);
      await page.waitForSelector("#backlog-triage:not([hidden])", wait);
      assert.deepEqual(await section(), {
        hidden: false,
        heading: "Held by triage",
        labelledBy: "Held by triage",
        paragraphs: [
          "Confidence 62% is below the 80% threshold. Rated by clef-flash against the project rules.",
          "Checked 2h ago",
        ],
        before: true,
      });
    });

    await t.test("a fixed hold posted to the Worker shows end to end", async () => {
      await open(FIXED_ITEM);
      await page.waitForSelector("#backlog-triage:not([hidden])", wait);
      assert.deepEqual(
        (await section()).paragraphs[0],
        "Held without a model score because its author recommends deny.",
      );
    });

    await t.test(
      "a line with trailing spaces shows the hold saved under the stripped item",
      async () => {
        await open(TRAILING_ITEM);
        await page.waitForSelector("#backlog-triage:not([hidden])", wait);
        assert.deepEqual(
          (await section()).paragraphs[0],
          "Held without a model score because its author recommends deny.",
        );
      },
    );

    await t.test("a missing verdict or a malformed record leaves the section hidden", async () => {
      assert.equal(verdictReads.length, 1);
      for (const line of [MISSING_ITEM, MALFORMED_ITEM]) {
        await open(line);
        await new Promise((resolve) => setTimeout(resolve, 300));
        assert.deepEqual(await section(), {
          hidden: true,
          heading: null,
          labelledBy: null,
          paragraphs: [],
          before: true,
        });
      }
      assert.equal(verdictReads.length, 2);
      assert.deepEqual(verdictReads[1], [MISSING_ITEM]);
    });

    await t.test("a reply that lands after the open fills the section", async () => {
      const etag = (await send("GET", "/api/backlog")).headers.get("etag");
      const text = "# Backlog\n\n" + [...lines, LATE_ITEM].map((l) => `- [ ] ${l}\n`).join("");
      assert.equal(
        (
          await send("PUT", "/api/backlog", text, {
            "If-Match": etag,
            "Content-Type": "text/markdown",
          })
        ).status,
        200,
      );
      await page.evaluate(async () => {
        await loadBacklog();
        renderBoard();
      });
      await page.waitForFunction(
        () => document.querySelectorAll(".backlog-card").length === 6,
        wait,
      );
      assert.equal(verdictReads.length, 3);
      assert.deepEqual(verdictReads[2], [MISSING_ITEM, LATE_ITEM]);
      await open(LATE_ITEM);
      assert.equal((await section()).hidden, true);
      releaseLate();
      await page.waitForSelector("#backlog-triage:not([hidden])", wait);
      assert.equal(
        (await section()).paragraphs[0],
        "Confidence 40% is below the 80% threshold. Rated by clef-flash against the project rules.",
      );
      assert.equal(verdictReads.length, 3);
    });

    assert.equal(itemReads, 0);
    assert.deepEqual(errors, []);
  });
});
