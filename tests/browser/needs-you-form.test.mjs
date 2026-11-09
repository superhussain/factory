// The needs tab renders needs.md as a question form, keeps a draft in localStorage, and
// submits answers.md through the job-file PUT, all against one owned local Worker.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const NEEDS = `# Pick a release path for the synthetic sample

The sample job stopped because two choices need a human.

## Steps
1. Run \`echo one\`.

## Question: Release path
Which path should the sample take?
Pick: one
- Fast (recommended): ship now, with less soak time.
- Careful: wait a day and watch the metrics.

## Question: Extras
Which extras should ship with it?
Pick: many
- Docs: write the docs.
- Tests: add more tests.
- Metrics

## When you are done
Run \`factory resume 001-needs\`.
`;
const SHA = createHash("sha256").update(NEEDS).digest("hex").slice(0, 12);
const MALFORMED = "# Broken ask\n\n## Question: Only one option\nWhich?\nPick: one\n- Alone\n";
const STEPS =
  "# Do this by hand\n\n## Steps\n1. Run `echo hi`.\n\n## When you are done\nRun `echo done`.\n";
const job = (id, stage, extra = {}) => ({
  id,
  project: "sample",
  title: "Synthetic " + id,
  stage,
  updated: "2026-01-01T00:00:00Z",
  ...extra,
});
const JOBS = [
  job("001-needs", "needs-human", { needs: "answer the questions" }),
  job("002-bad", "needs-human", { needs: "answer" }),
  job("003-steps", "needs-human", { needs: "do the steps" }),
  job("004-done", "spec"),
  job("005-conflict", "needs-human", { needs: "answer the questions" }),
  job("006-build", "build", { needs: "answer the questions at #006-build/needs" }),
  job("007-other", "build", { needs: "approve spec.md" }),
  job("008-fail", "build", { needs: "answer the questions at #008-fail/needs" }),
];
const wait = { polling: 50 };

async function putFile(origin, id, name, body) {
  const path = `/api/jobs/sample/${id}/${name}`;
  const current = await fetch(origin + path);
  const condition = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  const r = await fetch(origin + path, { method: "PUT", headers: condition, body });
  assert.equal(r.status, 200, path);
}

const boardJob = async (origin, id) =>
  (await (await fetch(origin + "/api/board")).json()).jobs.find((j) => j.id === id);

const answersText = async (origin, id) =>
  (await fetch(`${origin}/jobs/sample/${id}/answers.md`, { cache: "no-store" })).text();

const expectedAnswers = (now, picks, other) =>
  `# Answers

For needs.md sha256 ${SHA}, submitted ${now} from the dashboard.

## Question 1: Release path

Which path should the sample take?

Pick: one

- [${picks[0]}] Fast (recommended): ship now, with less soak time.
- [${picks[1]}] Careful: wait a day and watch the metrics.

Other: none

## Question 2: Extras

Which extras should ship with it?

Pick: many

- [${picks[2]}] Docs: write the docs.
- [${picks[3]}] Tests: add more tests.
- [${picks[4]}] Metrics

${other}
`;

test("needs you form", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(origin, "sample", SEED);
    await seedBoard(origin, JOBS);
    for (const j of JOBS) await putFile(origin, j.id, "spec.md", "# Spec\n\nSynthetic spec.\n");
    await putFile(origin, "001-needs", "needs.md", NEEDS);
    await putFile(origin, "002-bad", "needs.md", MALFORMED);
    await putFile(origin, "003-steps", "needs.md", STEPS);
    await putFile(origin, "004-done", "needs.md", NEEDS);
    await putFile(origin, "005-conflict", "needs.md", NEEDS);
    for (const id of ["006-build", "007-other", "008-fail"])
      await putFile(origin, id, "needs.md", NEEDS);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    const open = async (hash) => {
      await page.goto(origin + "/", { waitUntil: "load" });
      await page.evaluate((h) => {
        location.hash = h;
      }, hash);
      await page.waitForFunction(() => document.querySelector("#d-doc h1, #d-doc form"), wait);
    };
    const tab = () => page.$eval(".tab.active", (el) => el.dataset.name);
    const status = () => page.$eval("#ask-status", (el) => el.textContent);
    const fillAndSubmit = async (hash) => {
      await open(hash);
      await page.click("fieldset.ask:nth-of-type(1) input");
      await page.type("fieldset.ask:nth-of-type(2) textarea", "None");
      await page.click("#ask-submit");
    };
    const readOnly = /This job is no longer waiting on you\. The form is read-only\./;

    await t.test("form renders", async () => {
      await open("001-needs");
      assert.equal(await tab(), "needs.md");
      assert.equal(await page.$$eval("fieldset.ask", (e) => e.length), 2);
      assert.equal(
        await page.$$eval("fieldset.ask:nth-of-type(1) input[type=radio]", (e) => e.length),
        2,
      );
      assert.equal(
        await page.$$eval("fieldset.ask:nth-of-type(2) input[type=checkbox]", (e) => e.length),
        3,
      );
      assert.deepEqual(await page.$$eval(".ask-rec", (e) => e.map((x) => x.textContent)), [
        "Recommended",
      ]);
      assert.equal(await page.$$eval("textarea.ask-other", (e) => e.length), 2);
    });

    await t.test("completeness", async () => {
      assert.equal(await page.$eval("#ask-submit", (el) => el.disabled), true);
      assert.equal(await status(), "Answer every question to submit. 0 of 2 answered.");
      await page.click("fieldset.ask:nth-of-type(1) input");
      assert.equal(await status(), "Answer every question to submit. 1 of 2 answered.");
      await page.type("fieldset.ask:nth-of-type(2) textarea", "  ");
      assert.equal(await page.$eval("#ask-submit", (el) => el.disabled), true);
      await page.type("fieldset.ask:nth-of-type(2) textarea", "None");
      assert.equal(await page.$eval("#ask-submit", (el) => el.disabled), false);
      assert.equal(await status(), "");
    });

    await t.test("draft persists", async () => {
      await page.click("fieldset.ask:nth-of-type(2) input[value='1']");
      const check = async () => {
        assert.deepEqual(
          await page.$$eval("input:checked", (e) => e.map((x) => x.name + ":" + x.value)),
          ["q0:0", "q1:1"],
        );
        assert.equal(
          await page.$eval("fieldset.ask:nth-of-type(2) textarea", (el) => el.value),
          "  None",
        );
      };
      await page.click('.tab[data-name="spec.md"]');
      await page.click('.tab[data-name="needs.md"]');
      await check();
      await page.reload({ waitUntil: "load" });
      await page.waitForSelector("#needs-form", wait);
      await check();
    });

    await t.test("submit writes answers", async () => {
      await page.click("#ask-submit");
      await page.waitForFunction(
        () => /Saved to answers\.md at /.test(document.getElementById("d-doc").textContent),
        wait,
      );
      const first = await answersText(origin, "001-needs");
      const now = first.match(/submitted (\S+) from/)[1];
      assert.match(now, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
      assert.equal(first, expectedAnswers(now, ["x", " ", " ", "x", " "], "Other:\n> None"));
      await page.click("fieldset.ask:nth-of-type(1) input[value='1']");
      await page.click("#ask-submit");
      await page.waitForFunction(
        () =>
          document.querySelector("#needs-form input[value='1']:checked") &&
          !document.getElementById("ask-submit").disabled,
        wait,
      );
      const second = await answersText(origin, "001-needs");
      const again = second.match(/submitted (\S+) from/)[1];
      assert.equal(second, expectedAnswers(again, [" ", "x", " ", "x", " "], "Other:\n> None"));
      assert.equal((await boardJob(origin, "001-needs")).needs, "answer the questions");
    });

    await t.test("saved confirmation and focus", async () => {
      assert.match(await status(), /^Saved to answers\.md at /);
      assert.equal(await page.evaluate(() => document.activeElement.id), "ask-submit");
    });

    await t.test("needs-human keeps needs", async () => {
      const j = await boardJob(origin, "001-needs");
      assert.equal(j.stage, "needs-human");
      assert.equal(j.needs, "answer the questions");
      assert.equal(j.updated, "2026-01-01T00:00:00Z");
    });

    await t.test("submit clears needs", async () => {
      await fillAndSubmit("006-build/needs");
      await page.waitForFunction(
        (re) => new RegExp(re).test(document.getElementById("d-doc").textContent),
        wait,
        readOnly.source,
      );
      assert.match(await answersText(origin, "006-build"), /^# Answers\n\nFor needs\.md sha256 /);
      const j = await boardJob(origin, "006-build");
      assert.equal("needs" in j, false);
      assert.match(j.updated, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
      assert.notEqual(j.updated, "2026-01-01T00:00:00Z");
      assert.equal(await page.$eval("#ask-submit", (el) => el.disabled), true);
      await page.waitForFunction(
        () =>
          document.getElementById("ask-status").textContent ===
          "Saved to answers.md. This job is no longer waiting on you.",
        wait,
      );
      assert.equal(
        await page.evaluate(() =>
          document.activeElement.matches('.tab.active[data-name="needs.md"]'),
        ),
        true,
      );
    });

    await t.test("other ask kept", async () => {
      await fillAndSubmit("007-other/needs");
      await page.waitForFunction(
        () =>
          document.getElementById("ask-status").textContent.startsWith("Saved to answers.md at "),
        wait,
      );
      assert.match(await answersText(origin, "007-other"), /^# Answers\n\nFor needs\.md sha256 /);
      const j = await boardJob(origin, "007-other");
      assert.equal(j.needs, "approve spec.md");
      assert.equal(j.updated, "2026-01-01T00:00:00Z");
    });

    await t.test("board write fails", async () => {
      await page.setRequestInterception(true);
      const fail = (request) =>
        request.method() === "PUT" && new URL(request.url()).pathname === "/api/board"
          ? request.respond({ status: 500, body: "fixture failure" })
          : request.continue();
      page.on("request", fail);
      try {
        await fillAndSubmit("008-fail/needs");
        await page.waitForFunction(
          () => /board still shows/.test(document.getElementById("ask-status").textContent),
          wait,
        );
      } finally {
        page.off("request", fail);
        await page.setRequestInterception(false);
      }
      assert.equal(
        await status(),
        "Saved to answers.md, but the board still shows this job waiting on you (saving the board failed: 500 fixture failure). Submit again to retry.",
      );
      assert.match(await answersText(origin, "008-fail"), /^# Answers\n\nFor needs\.md sha256 /);
      assert.equal(
        (await boardJob(origin, "008-fail")).needs,
        "answer the questions at #008-fail/needs",
      );
      assert.equal(await page.$eval("#ask-submit", (el) => el.disabled), false);
    });

    await t.test("conflict", async () => {
      await open("005-conflict/needs");
      await page.click("fieldset.ask:nth-of-type(1) input");
      await page.type("fieldset.ask:nth-of-type(2) textarea", "None");
      await putFile(origin, "005-conflict", "answers.md", "# Answers\n\nSomeone else.\n");
      await page.click("#ask-submit");
      await page.waitForFunction(
        () =>
          /changed since this page loaded/.test(document.getElementById("ask-status").textContent),
        wait,
      );
      assert.equal(await page.$eval("#ask-submit", (el) => el.disabled), false);
    });

    await t.test("malformed", async () => {
      await open("002-bad/needs");
      assert.equal(await page.$("#needs-form"), null);
      assert.match(
        await page.$eval("#d-doc", (el) => el.textContent),
        /needs\.md is not in the question format/,
      );
      assert.match(await page.$eval("#d-doc", (el) => el.textContent), /Only one option/);
    });

    await t.test("steps only", async () => {
      await open("003-steps/needs");
      assert.equal(await page.$("#needs-form"), null);
      assert.equal(await page.$$eval("#d-doc ol li", (e) => e.length), 1);
    });

    await t.test("not waiting", async () => {
      await open("004-done/needs");
      assert.equal(
        await page.$$eval(
          "#needs-form input:not(:disabled), #needs-form textarea:not(:disabled)",
          (e) => e.length,
        ),
        0,
      );
      assert.equal(await page.$eval("#ask-submit", (el) => el.disabled), true);
      assert.match(
        await page.$eval("#d-doc", (el) => el.textContent),
        /This job is no longer waiting on you\. The form is read-only\./,
      );
    });

    assert.deepEqual(errors, []);
    await context.close();
  });
});
