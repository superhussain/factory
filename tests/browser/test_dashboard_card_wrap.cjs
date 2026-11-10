// Card text: one-row lines (a needs-you ask wraps to two) with a full-text tooltip, ordinary and hostile unbroken text,
// injection safety, card geometry, PR target separation, drawer interactions, across themes
// and viewports.
// Runs the offline source checks always; the browser matrix requires --browser <origin>.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { Script } = require("node:vm");
const { waitForBoard } = require("../support/board-ready.cjs");

const root = path.resolve(__dirname, "../..");
const candidate = fs.readFileSync(path.join(root, "web/dashboard.html"), "utf8");
const token = "UnbrokenFactoryText".repeat(24);
const payload =
  '<img src=x onerror="window.__cardWrapXss=1"><script>window.__cardWrapXss=1</script>&"\'';
const writes = [],
  external = [],
  errors = [];

function makeFixtures(stress) {
  const text = stress ? token + payload : "Ordinary spaced text";
  const jobs = [
    {
      id: "100-note",
      stage: "build",
      title: "Build note",
      note: stress ? text : "Ordinary spaced note\nwith a newline",
    },
    {
      id: "101-metadata",
      stage: "spec",
      title: text,
      branch: text,
      refs: [{ kind: "follow-up-of", id: text }],
    },
    { id: "102-needs", stage: "build", needs: text, note: "Hidden fallback note" },
    { id: "103-fallback", stage: "needs-human", note: text },
    {
      id: stress ? "104-" + token : "104-pr",
      stage: "pr-open",
      note: text,
      pr: "https://github.com/example/wrap/pull/42",
    },
    { id: "105-idea", stage: "idea", title: text, idea: text + "\n" + text },
    {
      id: "106-claim",
      stage: "build",
      claim: {
        device: text,
        session: text,
        since: "2026-01-01T00:00:00Z",
        seen: "2026-01-01T00:00:00Z",
      },
    },
  ].map((j) => ({
    title: "Synthetic card",
    branch: "",
    round: 0,
    project: "wrap",
    updated: "2026-01-01T00:00:00Z",
    ...j,
  }));
  return {
    stress,
    jobs,
    projects: [{ slug: "wrap", title: "Wrap fixtures", repo: "example/wrap" }],
  };
}

const PROJECT_DOC = "---\nrepo: example/wrap\nbase: main\ntitle: Wrap fixtures\n---\n";

async function settle(page) {
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  });
}

async function makePage(browser, origin, width, theme) {
  const context = await browser.createBrowserContext();
  try {
    const page = await context.newPage();
    await page.setViewport({ width, height: 844, deviceScaleFactor: 1 });
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
    await (await import("../support/browser-server.mjs")).blockLiveEvents(page);
    await page.evaluateOnNewDocument(() => {
      const interval = window.setInterval;
      window.setInterval = (fn, ms, ...args) => (ms === 3000 ? 0 : interval(fn, ms, ...args));
    });
    page.on("pageerror", (error) => errors.push(error.message));
    await page.setRequestInterception(true);
    page.on("request", async (request) => {
      const target = new URL(request.url());
      if (request.method() !== "GET") {
        writes.push(request.method() + " " + request.url());
        return request.abort();
      }
      if (target.origin !== origin) {
        external.push(request.url());
        return request.abort();
      }
      return request.continue();
    });
    await page.goto(origin + "/dashboard.html", { waitUntil: "domcontentloaded" });
    await waitForBoard(page);
    await page.waitForSelector(".card", { polling: 50 });
    await settle(page);
    return { page, context };
  } catch (error) {
    await context.close();
    throw error;
  }
}

async function measureCards(page) {
  return page.evaluate(() => {
    const rect = (el) => el.getBoundingClientRect().toJSON();
    const box = (el) => ({ ...rect(el), clientWidth: el.clientWidth, scrollWidth: el.scrollWidth });
    const board = document.getElementById("board");
    return {
      document: box(document.documentElement),
      board: box(board),
      xss: window.__cardWrapXss,
      fixture: JSON.parse(JSON.stringify(jobs)),
      cards: Object.fromEntries(
        [...document.querySelectorAll(".card")].map((card) => {
          const css = getComputedStyle(card);
          const bounds = rect(card);
          const content = {
            left: bounds.left + parseFloat(css.borderLeftWidth) + parseFloat(css.paddingLeft),
            right: bounds.right - parseFloat(css.borderRightWidth) - parseFloat(css.paddingRight),
          };
          return [
            card.dataset.id,
            {
              bounds: box(card),
              content,
              column: box(card.closest(".col")),
              wrapper: card.parentElement.classList.contains("card-wrap")
                ? box(card.parentElement)
                : null,
              text: card.textContent,
              injected: card.querySelectorAll("img, script").length,
              fields: [
                ...card.querySelectorAll(".id, .proj, .title, .meta, .note, .idea-text"),
              ].map((el) => {
                const range = document.createRange();
                range.selectNodeContents(el);
                const style = getComputedStyle(el);
                return {
                  selector: el.className,
                  text: el.textContent,
                  title: el.title,
                  bounds: box(el),
                  lines: [...range.getClientRects()].map((r) => r.toJSON()),
                  overflow: style.overflowX,
                  textOverflow: style.textOverflow,
                  whiteSpace: style.whiteSpace,
                  lineHeight: parseFloat(style.lineHeight),
                  clamp: style.webkitLineClamp,
                  display: style.display,
                };
              }),
            },
          ];
        }),
      ),
    };
  });
}

function checkCards(actual, expected, width) {
  assert.equal(actual.xss, undefined);
  assert.deepEqual(actual.fixture, expected.jobs, "render/poll leaves fixture unchanged");
  assert.equal(Object.keys(actual.cards).length, expected.jobs.length);
  if (width < 760)
    for (const field of overflow(actual))
      assert.ok(
        field.viewportOverflow <= 1,
        field.id + " " + field.selector + " outside mobile viewport by " + field.viewportOverflow,
      );
  if (width < 760)
    for (const key of ["document", "board"]) {
      assert.ok(actual[key].scrollWidth <= actual[key].clientWidth + 1, key + " mobile overflow");
    }
  for (const job of expected.jobs) {
    const card = actual.cards[job.id];
    assert.ok(card, job.id);
    assert.equal(card.injected, 0);
    const expectedFields = {
      "id line": job.id + (job.needs || job.stage === "needs-human" ? job.stage : ""),
      "proj line": "Wrap fixtures",
      title: job.title,
      ...(job.stage === "idea"
        ? { "idea-text": job.idea.slice(job.title.length).trim() }
        : {
            ...(job.branch ? { "meta branch line": job.branch } : {}),
            "meta round line": "round 0",
          }),
      ...(job.refs ? { "meta refs line": "follow-up of " + job.refs[0].id } : {}),
      ...(job.claim ? { "meta host line": "on " + job.claim.device } : {}),
      ...(job.needs || job.note ? { "note line": job.needs || job.note } : {}),
    };
    assert.deepEqual(
      Object.fromEntries(card.fields.map((f) => [f.selector, f.text])),
      expectedFields,
      job.id + " exact fields",
    );
    if (job.claim)
      assert.equal(card.text.split(job.claim.device).length, 2, job.id + " host shown once");
    for (const bounds of [card.bounds, card.wrapper].filter(Boolean)) {
      assert.ok(
        bounds.left >= card.column.left - 1 && bounds.right <= card.column.right + 1,
        job.id + " column bounds",
      );
      if (width < 760)
        assert.ok(bounds.left >= -1 && bounds.right <= width + 1, job.id + " viewport bounds");
    }
    for (const field of card.fields) {
      if (field.selector.endsWith(" line")) {
        if ((job.needs || job.stage === "needs-human") && field.selector === "note line") {
          const where = job.id + " " + field.selector;
          assert.equal(field.whiteSpace, "normal", where);
          assert.equal(field.clamp, "2", where);
          assert.equal(field.overflow, "hidden", where);
          assert.equal(field.title, field.text, where + " title");
          assert.ok(field.bounds.right <= card.content.right + 1, where);
          assert.ok(field.bounds.scrollWidth <= field.bounds.clientWidth + 1, where);
          assert.ok(field.bounds.height <= 2 * field.lineHeight + 1, where);
          if (expected.stress)
            assert.equal(Math.round(field.bounds.height / field.lineHeight), 2, where + " rows");
          continue;
        }
        assert.equal(field.whiteSpace, "nowrap", job.id + " " + field.selector);
        assert.equal(field.textOverflow, "ellipsis", job.id + " " + field.selector);
        assert.equal(field.overflow, "hidden", job.id + " " + field.selector);
        assert.equal(
          field.title,
          field.selector === "id line" ? job.id : field.text,
          job.id + " " + field.selector + " title",
        );
        assert.ok(field.bounds.right <= card.content.right + 1, job.id + " " + field.selector);
        continue;
      }
      assert.ok(
        field.bounds.scrollWidth <= field.bounds.clientWidth + 1,
        job.id + " " + field.selector + " scroll overflow",
      );
      if (field.selector === "title") {
        assert.equal(field.clamp, "2");
        assert.equal(field.overflow, "hidden");
      } else if (field.selector === "idea-text") {
        assert.equal(field.clamp, "3");
        assert.equal(field.overflow, "hidden");
        assert.equal(field.whiteSpace, "pre-line");
        assert.equal(field.display, "flow-root");
        continue;
      }
      for (const line of field.lines)
        assert.ok(
          line.left >= Math.max(field.bounds.left, card.content.left) - 1 &&
            line.right <= Math.min(field.bounds.right, card.content.right) + 1,
          job.id + " " + field.selector + " text fragment overflow " + JSON.stringify(line),
        );
    }
  }
}

function overflow(actual) {
  return Object.entries(actual.cards).flatMap(([id, card]) =>
    card.fields
      .filter((f) => !f.selector.endsWith(" line"))
      .map((f) => ({
        id,
        selector: f.selector,
        scrollOverflow: f.bounds.scrollWidth - f.bounds.clientWidth,
        fragmentOverflow: Math.max(
          0,
          ...f.lines.map((r) => r.right - Math.min(f.bounds.right, card.content.right)),
        ),
        viewportOverflow: Math.max(0, ...f.lines.map((r) => r.right - actual.document.clientWidth)),
      })),
  );
}

async function interactions(page, fixture, support) {
  await page.evaluate(() => document.querySelector('[data-id="100-note"]').scrollIntoView());
  await support.focusVisible(page, '[data-id="100-note"]');
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => !document.getElementById("detail").hidden, { polling: 50 });
  await page.click("#d-close");
  await page.waitForFunction(() => document.getElementById("detail").hidden, { polling: 50 });
  const pr = fixture.jobs.find((j) => j.pr);
  const link = await page.evaluate((id) => {
    const card = [...document.querySelectorAll(".card")].find((el) => el.dataset.id === id);
    const anchor = card.parentElement.querySelector(".pr-link");
    anchor.scrollIntoView({ block: "center", inline: "center" });
    const r = anchor.getBoundingClientRect();
    const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
    return {
      href: anchor.getAttribute("href"),
      target: anchor.target,
      rel: anchor.rel,
      sibling: anchor.parentElement === card.parentElement && !card.contains(anchor),
      hit: hit === anchor || anchor.contains(hit),
    };
  }, pr.id);
  assert.deepEqual(link, {
    href: pr.pr,
    target: "_blank",
    rel: "noopener noreferrer",
    sibling: true,
    hit: true,
  });
  await page.evaluate(() => window.scrollTo(0, 0));
  await settle(page);
}

async function browserRun(origin) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(origin);
  const puppeteer = require("puppeteer-core");
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  const contexts = new Set();
  try {
    for (const stress of [true, false]) {
      const fixture = makeFixtures(stress);
      const snapshot = structuredClone(fixture);
      await support.seedProject(origin, "wrap", PROJECT_DOC);
      await support.seedBoard(origin, fixture.jobs);
      for (const width of [320, 390, 1440])
        for (const theme of ["light", "dark"]) {
          const current = await makePage(browser, origin, width, theme);
          contexts.add(current.context);
          const actual = await measureCards(current.page);
          checkCards(actual, fixture, width);
          await interactions(current.page, fixture, support);
          if (stress) {
            await current.page.evaluate(() => poll());
            await settle(current.page);
            checkCards(await measureCards(current.page), fixture, width);
          }
          assert.deepEqual(fixture, snapshot);
          console.log(
            JSON.stringify({
              width,
              theme,
              fixture: stress ? "stress" : "ordinary",
              maximumOverflow: Math.max(
                0,
                ...overflow(actual).flatMap((f) => [f.scrollOverflow, f.fragmentOverflow]),
              ),
              result: "PASS",
            }),
          );
          await current.context.close();
          contexts.delete(current.context);
        }
    }
    assert.deepEqual(writes, []);
    assert.deepEqual(external, []);
    assert.deepEqual(errors, []);
    console.log(
      JSON.stringify({
        result: "PASS",
        writes: writes.length,
        external: external.length,
        browserErrors: errors.length,
      }),
    );
  } finally {
    for (const context of contexts) await context.close();
    await browser.close();
  }
}

function offline() {
  const script = (html) => html.match(/<script>([\s\S]*?)<\/script>/)[1];
  new Script(script(candidate));
  const cardRule = candidate.match(/  \.card \{([\s\S]*?)\n  \}/)[1];
  assert.match(cardRule, /overflow-wrap: anywhere;/, "cards wrap unbroken text anywhere");
  console.log("card wrap offline checks PASS");
}

(async () => {
  offline();
  const i = process.argv.indexOf("--browser");
  if (i >= 0) await browserRun(process.argv[i + 1]);
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
