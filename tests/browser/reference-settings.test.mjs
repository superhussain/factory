// Reference settings section against one owned local Worker. The section is read-only.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { FLOORS, GATES } from "../../worker/admission.js";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const JOBS = [
  {
    id: "001-sample",
    project: "sample",
    title: "Synthetic sample job",
    stage: "spec",
    updated: "2026-01-01T00:00:00Z",
  },
];
const wait = { polling: 50 };

async function seedPipeline(origin, path, body) {
  const response = await fetch(origin + path, {
    method: "PUT",
    headers: { "If-None-Match": "*", "Content-Type": "application/json" },
    body,
  });
  assert.equal(response.status, 200);
}

test("reference settings", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    for (const slug of ["sample", "other"])
      await seedProject(
        origin,
        slug,
        `---\nrepo: example/${slug}\nbase: main\ntitle: Synthetic ${slug}\n---\n`,
      );
    await seedBoard(origin, JOBS);
    await seedPipeline(
      origin,
      "/api/pipeline/global",
      '{"schema":1,"review_rounds":3,"gates":["spec-approval"]}',
    );
    await seedPipeline(
      origin,
      "/api/pipeline/projects/other",
      '{"schema":1,"merge":"human","review_rounds":1,"pickup":{"enabled":true,"hosts":["host-a"]},"stage_notes":{"build":"Keep changes small."}}',
    );
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const rows = (id) =>
      page.$eval(`#${id}`, (table) =>
        [...table.tBodies[0].rows].map((tr) => [...tr.cells].map((td) => td.textContent)),
      );
    const loaded = () =>
      page.waitForFunction(() => {
        const first = document.querySelector("#reference-effective tbody td");
        return first && first.textContent !== "Loading projects…";
      }, wait);

    await page.goto(origin + "/#/settings/reference", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#settings-reference:not([hidden])", wait);

    await t.test("reference R1 route", async () => {
      assert.equal(
        await page.$eval("#settings-nav-reference", (el) => el.getAttribute("aria-current")),
        "page",
      );
      await page.waitForFunction(
        () => document.activeElement?.id === "reference-settings-heading",
        wait,
      );
      assert.ok((await page.title()).startsWith("Settings · "));
    });

    await t.test("reference R2 floors and gates", async () => {
      assert.deepEqual(
        await rows("reference-floors"),
        FLOORS.map((f) => [f.id, f.rule, f.config]),
      );
      assert.deepEqual(
        await rows("reference-gates"),
        GATES.map((g) => [g.id, g.label, g.rule]),
      );
    });

    await t.test("reference R3 effective", async () => {
      await loaded();
      assert.deepEqual(await rows("reference-effective"), [
        ["other", "Held for a person", "1", "Spec approval", "On: host-a", "Build"],
        ["sample", "After green checks", "3", "Spec approval", "Off", "None"],
      ]);
    });

    await t.test("reference R4 failures", async () => {
      await page.setRequestInterception(true);
      const onRequest = (request) => {
        const path = new URL(request.url()).pathname;
        if (path === "/api/pipeline/effective/other") return request.respond({ status: 409 });
        if (path === "/api/pipeline/effective/sample") return request.abort();
        return request.continue();
      };
      page.on("request", onRequest);
      await page.evaluate(() => {
        location.hash = "#/settings/display";
      });
      await page.waitForSelector("#settings-reference", { hidden: true, ...wait });
      await page.evaluate(() => {
        location.hash = "#/settings/reference";
      });
      await loaded();
      assert.deepEqual(await rows("reference-effective"), [
        ["other", "Stored settings no longer validate. Save them again in Settings, Pipeline."],
        ["sample", "Could not load this project's pipeline."],
      ]);
      assert.equal(
        await page.$eval("#reference-effective tbody td:last-child", (td) => td.colSpan),
        5,
      );
      page.off("request", onRequest);
      await page.setRequestInterception(false);
    });

    await t.test("reference R5 instance", async () => {
      assert.equal(
        await page.$eval("#reference-title", (el) => el.textContent),
        "Instance title: Factory",
      );
      assert.deepEqual(await rows("reference-links"), [
        ["app", "Session app"],
        ["web", "Session"],
      ]);
      const text = await page.$eval("#settings-reference", (el) => el.textContent);
      for (const leak of ["sessions.example.test", "session://", "{server}"])
        assert.ok(!text.includes(leak), leak);
    });

    await t.test("reference R6 read-only", async () => {
      assert.equal(
        await page.$eval(
          "#settings-reference",
          (el) => el.querySelectorAll("input, select, textarea, button").length,
        ),
        0,
      );
      const methods = new Set();
      page.on("request", (request) => methods.add(request.method()));
      await page.evaluate(() => {
        location.hash = "#/settings/display";
      });
      await page.waitForSelector("#settings-reference", { hidden: true, ...wait });
      await page.evaluate(() => {
        location.hash = "#/settings/reference";
      });
      await loaded();
      assert.deepEqual([...methods], ["GET"]);
    });

    await t.test("reference R7 mobile", async () => {
      await page.setViewport({ width: 390, height: 844 });
      assert.equal(
        await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
        true,
      );
    });

    assert.deepEqual(errors, []);
  });
});
