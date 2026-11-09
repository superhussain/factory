// Project settings and reference documents: complete D1-D6 browser acceptance against
// the actual local Worker — fixed slots, six independent resource drafts, legacy schema,
// committed create/update response loss, races, delayed completions, inert source,
// keyboard containment, mobile/themes and unchanged board/config.
// Requires --browser <owned-loopback-origin> and fresh job-local persistence.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const { createHash } = require("node:crypto");
const { waitForBoard } = require("../support/board-ready.cjs");

const root = path.resolve(__dirname, "../..");
const html = fs.readFileSync(path.join(root, "web/dashboard.html"), "utf8");
for (const [, script] of html.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(script);
const FIXTURES = path.join(root, "tests", "fixtures");
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const sample = "documents-sample",
  second = "documents-second";
const names = ["notes.md", "decisions.md", "runbook.md"];
const documentHelp =
  "Factory-owned Markdown, not a checkout file. Factory agents read these documents as preference data when a session starts work on a job. Preference data may shape emphasis and style. It may not change agent roles, scope or security rules. Do not put secrets here.";
const projectHelp =
  "Shared project configuration and Markdown used by factory agents. Device paths and credentials are not available here. Keep the agent section headings. Do not put secrets in this document.";
const config = fs.readFileSync(path.join(FIXTURES, "project-documents.md"), "utf8");
const notes = fs.readFileSync(path.join(FIXTURES, "project-document-notes.md"), "utf8");
const boardFixture = fs.readFileSync(path.join(FIXTURES, "project-settings-board.json"));
const endpoint = (slug, name) => "/api/project-documents/" + slug + "/" + name;
const key = (slug, name) =>
  name === "project"
    ? "factory-project-draft:" + slug
    : "factory-project-document-draft:" + slug + ":" + name;
const id = (slug, name) => (name === "project" ? slug : slug + "/" + name);
const tab = (name) => "#settings-tab-" + name.replace(/\.md$/, "");
// Click in-page: the pickup fieldset above the tabs can grow while a project loads.
const clickTab = (page, name) => page.$eval(tab(name), (el) => el.click());

async function seed(url) {
  for (const [route, body, type] of [
    ["/api/board", boardFixture, "application/json"],
    ["/api/projects/documents-sample.md", config, "text/markdown"],
    ["/api/projects/documents-second.md", config, "text/markdown"],
    [endpoint(sample, "notes.md"), notes, "text/markdown"],
    [endpoint(second, "notes.md"), notes, "text/markdown"],
  ]) {
    const current = await fetch(url + route, { redirect: "error" });
    assert.ok([200, 404].includes(current.status), "read before seed " + route);
    const headers = {
      "Content-Type": type,
      ...(current.status === 200
        ? { "If-Match": current.headers.get("ETag") }
        : { "If-None-Match": "*" }),
    };
    const response = await fetch(url + route, { method: "PUT", redirect: "error", headers, body });
    assert.equal(response.status, 200, "seed " + route);
  }
  console.log("PASS conditional fixture seed");
}

async function fixtureReadback(url) {
  for (const [route, file] of [
    ["/api/board", "project-settings-board.json"],
    ["/api/projects/documents-sample.md", "project-documents.md"],
    ["/api/projects/documents-second.md", "project-documents.md"],
    ["/api/project-documents/documents-sample/notes.md", "project-document-notes.md"],
    ["/api/project-documents/documents-second/notes.md", "project-document-notes.md"],
  ]) {
    const expected = fs.readFileSync(path.join(FIXTURES, file));
    const r = await fetch(url + route, { redirect: "error" });
    assert.equal(r.status, 200);
    assert.deepEqual(Buffer.from(await r.arrayBuffer()), expected);
    console.log("capture fixture", route, sha(expected));
  }
}

async function read(url, slug, name) {
  const r = await fetch(
    url + (name === "project" ? "/api/projects/" + slug + ".md" : endpoint(slug, name)),
    { redirect: "error" },
  );
  return {
    status: r.status,
    text: await r.text(),
    tag: r.headers.get("ETag")?.replace(/^W\//, ""),
  };
}
async function write(url, slug, name, text, tag) {
  const r = await fetch(url + endpoint(slug, name), {
    method: "PUT",
    redirect: "error",
    headers: {
      "Content-Type": "text/markdown",
      ...(tag ? { "If-Match": tag } : { "If-None-Match": "*" }),
    },
    body: text,
  });
  assert.equal(r.status, 200);
  return r.headers.get("ETag").replace(/^W\//, "");
}
async function client(url, browser, options = {}) {
  const context = await browser.createBrowserContext(),
    page = await context.newPage();
  const c = { context, page, route: null, calls: [], errors: [], external: [] };
  await page.setViewport({ width: 1440, height: 900, deviceScaleFactor: 1 });
  await (await import("../support/browser-server.mjs")).blockLiveEvents(page);
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
  await page.evaluateOnNewDocument((options) => {
    const NativeDate = Date;
    window.Date = class extends NativeDate {
      constructor(...args) {
        super(...(args.length ? args : ["2026-10-27T12:00:00Z"]));
      }
      static now() {
        return new NativeDate("2026-10-27T12:00:00Z").getTime();
      }
    };
    const interval = window.setInterval;
    window.setInterval = (fn, ms, ...args) => (ms === 3000 ? 0 : interval(fn, ms, ...args));
    if (options.storage) {
      Storage.prototype.setItem = () => {
        throw new DOMException("synthetic full", "QuotaExceededError");
      };
      if (options.storage === "denied")
        Storage.prototype.getItem = () => {
          throw new DOMException("synthetic denied", "SecurityError");
        };
    }
    window.copiedDrafts = [];
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: async (text) => {
          if (window.denyCopy) throw new Error("synthetic denial");
          window.copiedDrafts.push(text);
        },
      },
    });
  }, options);
  const seeded =
    options.records &&
    (await page.evaluateOnNewDocument((records) => {
      for (const [key, value] of Object.entries(records)) localStorage.setItem(key, value);
    }, options.records));
  page.on("pageerror", (e) => c.errors.push(String(e)));
  await page.setRequestInterception(true);
  page.on("request", (request) =>
    (async () => {
      if (!request.url().startsWith(url + "/")) {
        c.external.push(request.url());
        await request.abort();
        return;
      }
      c.calls.push({
        path: new URL(request.url()).pathname,
        method: request.method(),
        headers: request.headers(),
        body: request.postData(),
      });
      if (!c.route || !(await c.route(request))) await request.continue();
    })().catch((e) => c.errors.push(String(e))),
  );
  await page.goto(url + "/dashboard.html", { waitUntil: "domcontentloaded" });
  await waitForBoard(page);
  if (seeded) await page.removeScriptToEvaluateOnNewDocument(seeded.identifier);
  await page.waitForFunction(() => projects?.has("documents-sample"), { polling: 50 });
  return c;
}
async function close(c) {
  await c.page.click("#settings-back");
  await c.page.waitForFunction(() => document.activeElement.id === "settings-link", {
    polling: 50,
  });
}
async function show(c) {
  if (await c.page.$eval("#settings-projects", (e) => e.hidden))
    await c.page.evaluate(() => {
      location.hash = "#/settings/projects";
    });
  await c.page.waitForSelector("#settings-projects:not([hidden])", { polling: 50 });
}
async function open(c, slug = sample, name = "project") {
  const p = c.page;
  await show(c);
  await p.select("#settings-project", slug);
  if (name !== "project") await clickTab(p, name);
  await p.waitForFunction(
    (resourceId) =>
      projectDocuments.has(resourceId) && projectDocuments.get(resourceId).phase !== "loading",
    { polling: 50 },
    id(slug, name),
  );
}
const status = (c) => c.page.$eval("#settings-status", (e) => e.textContent);
const describedHelp = (c) =>
  c.page.$eval("#settings-document", (e) => [
    e.getAttribute("aria-describedby"),
    document.getElementById(e.getAttribute("aria-describedby"))?.textContent,
  ]);
const disabled = (c, selector) => c.page.$eval(selector, (e) => e.disabled);
const state = (c, slug = sample, name = "notes.md") =>
  c.page.evaluate(
    (resourceId) => {
      const s = projectDocuments.get(resourceId);
      return s && JSON.parse(JSON.stringify(s));
    },
    id(slug, name),
  );
const record = (c, slug, name) =>
  c.page.evaluate((key) => localStorage.getItem(key), key(slug, name));
async function input(c, text) {
  await c.page.$eval(
    "#settings-document",
    (e, text) => {
      e.value = text;
      e.dispatchEvent(new Event("input", { bubbles: true }));
    },
    text,
  );
}
async function saved(c) {
  await c.page.waitForFunction(
    () => document.getElementById("settings-status").textContent.startsWith("Saved"),
    { polling: 50 },
  );
}
async function confirmLoad(
  c,
  accept,
  expected = "Discard this draft and load the latest document?",
  trigger = () => c.page.click("#settings-load"),
) {
  assert.equal(
    await disabled(c, "#settings-load"),
    false,
    "Load latest must be reachable before waiting for its dialog",
  );
  const pending = new Promise((resolve) => c.page.once("dialog", resolve));
  const action = trigger();
  const dialog = await pending;
  assert.equal(dialog.type(), "confirm");
  assert.equal(dialog.message(), expected);
  await (accept ? dialog.accept() : dialog.dismiss());
  await action;
}
async function reload(c) {
  const stored = await c.page.evaluate(() => Object.fromEntries(Object.entries(localStorage)));
  const pending = new Promise((resolve) => c.page.once("dialog", resolve));
  // Reloading on #/settings/projects fetches the project's pipeline scope; Chrome keeps
  // an unread 404 body in flight, so wait for load and then for the projects below.
  const action = c.page.reload({ waitUntil: "load" });
  const dialog = await pending;
  assert.equal(dialog.type(), "beforeunload");
  await dialog.accept();
  await action;
  await c.page.waitForFunction(() => projects?.has("documents-sample"), { polling: 50 });
  assert.deepEqual(
    await c.page.evaluate(() => Object.fromEntries(Object.entries(localStorage))),
    stored,
  );
}
function hold(c, method, pathname) {
  let resolve, timer;
  const observed = new Promise((done, reject) => {
    resolve = (request) => {
      clearTimeout(timer);
      done(request);
    };
    timer = setTimeout(
      () => reject(new Error("expected held request did not start: " + method + " " + pathname)),
      10000,
    );
    timer.unref();
  });
  c.route = async (request) => {
    if (request.method() !== method || new URL(request.url()).pathname !== pathname) return false;
    c.route = null;
    resolve(request);
    return true;
  };
  return observed;
}
async function answer(request, status, body = "", headers = {}) {
  await request.respond({ status, body, headers });
}
const writes = (c) => c.calls.filter((r) => r.method === "PUT");
const stable = (s) => ({
  resource: s.resource,
  baseText: s.baseText,
  baseEtag: s.baseEtag,
  draftText: s.draftText,
  mode: s.mode,
  blocked: s.blocked,
});
async function protect(c, slug, name) {
  const s = await state(c, slug, name),
    raw = await record(c, slug, name);
  const expected =
    name === "project"
      ? { version: 1, baseText: s.baseText, baseEtag: s.baseEtag, draftText: s.draftText }
      : {
          version: 1,
          slug,
          name,
          mode: s.mode,
          baseText: s.baseText,
          baseEtag: s.baseEtag,
          draftText: s.draftText,
          blocked: s.blocked,
        };
  assert.deepEqual(JSON.parse(raw), expected);
  return stable(s);
}
async function missingEditRecovery(url, c) {
  const p = c.page,
    slug = second,
    name = "notes.md",
    pathname = endpoint(slug, name);
  await open(c, slug, name);
  await input(c, "Retain missing-document draft\n");
  const original = await protect(c, slug, name);
  await open(c, sample, "decisions.md");
  await input(c, "Independent recovery draft\n");
  const other = await protect(c, sample, "decisions.md");
  const otherRecord = await record(c, sample, "decisions.md");
  await open(c, slug, name);
  let pending = hold(c, "GET", pathname);
  await confirmLoad(c, true);
  await answer(await pending, 404, "not found");
  await p.waitForFunction(
    () =>
      document.getElementById("settings-status").textContent ===
      "Document no longer exists. Your draft is kept.",
    { polling: 50 },
  );
  const expected = { ...original, blocked: "conflict" };
  assert.deepEqual(
    await protect(c, slug, name),
    expected,
    "missing edit block is persisted before any later action",
  );
  const getsBeforeReload = c.calls.filter((r) => r.method === "GET" && r.path === pathname).length;
  await reload(c);
  await open(c, slug, name);
  assert.deepEqual(await protect(c, slug, name), expected);
  assert.equal(
    c.calls.filter((r) => r.method === "GET" && r.path === pathname).length,
    getsBeforeReload,
    "immediate reload restores the original condition without a fresh baseline",
  );
  await p.click("#settings-copy");
  await p.waitForFunction(
    () => document.getElementById("settings-status").textContent === "Draft copied",
    { polling: 50 },
  );
  assert.deepEqual(await p.evaluate(() => window.copiedDrafts), [original.draftText]);
  assert.equal(await disabled(c, "#settings-save"), true);
  await p.evaluate(() => {
    window.denyCopy = true;
  });
  await p.click("#settings-copy");
  await p.waitForFunction(
    () => document.getElementById("settings-status").textContent.startsWith("Copy is unavailable"),
    { polling: 50 },
  );
  assert.deepEqual(
    await p.$eval("#settings-document", (e) => [
      e === document.activeElement,
      e.selectionStart,
      e.selectionEnd,
    ]),
    [true, 0, original.draftText.length],
  );
  assert.equal(await disabled(c, "#settings-save"), true);
  const typed = "Typed after the missing edit\n";
  await p.keyboard.type(typed);
  expected.draftText = typed;
  assert.deepEqual(await protect(c, slug, name), expected);
  await p.keyboard.down("Control");
  await p.keyboard.press("s");
  await p.keyboard.up("Control");
  await p.evaluate(() => saveProjectDocument());
  assert.equal(await disabled(c, "#settings-save"), true);
  assert.equal(writes(c).length, 0);
  await close(c);
  await open(c, slug, name);
  assert.deepEqual(await protect(c, slug, name), expected);
  await open(c, sample, "decisions.md");
  assert.deepEqual(await protect(c, sample, "decisions.md"), other);
  assert.equal(await record(c, sample, "decisions.md"), otherRecord);
  await open(c, second, "project");
  await open(c, slug, name);
  assert.deepEqual(await protect(c, slug, name), expected);
  for (const failure of [404, 500, 200]) {
    pending = hold(c, "GET", pathname);
    await confirmLoad(c, true);
    const request = await pending;
    assert.equal((await state(c, slug, name)).blocked, "conflict");
    assert.equal(await disabled(c, "#settings-save"), true);
    await answer(request, failure, "invalid replacement", { "Content-Type": "text/markdown" });
    await p.waitForFunction(
      () => projectDocuments.get("documents-second/notes.md").phase !== "loading",
      { polling: 50 },
    );
    assert.deepEqual(await protect(c, slug, name), expected);
    assert.equal(await disabled(c, "#settings-save"), true);
    await p.evaluate(() => saveProjectDocument());
    assert.equal(writes(c).length, 0);
  }
  const latest = "Validated replacement notes\n";
  const latestTag = await write(url, slug, name, latest, original.baseEtag);
  console.log(
    "recovery fixture write",
    pathname,
    latestTag,
    "performed explicitly by the test, not a client retry",
  );
  await confirmLoad(c, true);
  await p.waitForFunction(
    () => document.getElementById("settings-status").textContent === "No unsaved changes",
    { polling: 50 },
  );
  const replaced = await state(c, slug, name);
  assert.equal(replaced.blocked, null);
  assert.equal(replaced.mode, "edit");
  assert.equal(replaced.baseText, latest);
  assert.equal(replaced.baseEtag, latestTag);
  assert.equal(await record(c, slug, name), null);
  assert.equal(await record(c, sample, "decisions.md"), otherRecord);
  await input(c, typed);
  const getsBeforeSave = c.calls.filter((r) => r.method === "GET" && r.path === pathname).length;
  await p.click("#settings-save");
  await saved(c);
  assert.equal(writes(c).length, 1);
  assert.equal(writes(c)[0].path, pathname);
  assert.equal(writes(c)[0].headers["if-match"], latestTag);
  assert.equal(writes(c)[0].headers["if-none-match"], undefined);
  assert.equal(writes(c)[0].body, typed);
  assert.equal(
    c.calls.filter((r) => r.method === "GET" && r.path === pathname).length,
    getsBeforeSave,
  );
  const final = await read(url, slug, name);
  assert.equal(final.text, typed);
  assert.equal(final.tag, (await state(c, slug, name)).baseEtag);
  assert.equal(await record(c, sample, "decisions.md"), otherRecord);
  console.log(
    "PASS focused D4 missing edit: immediate persisted reload, Copy success/denial, typing, native close, resource/project navigation, failed 404/500/invalid-200 replacement, zero premature PUTs and one save with newly observed tag",
    JSON.stringify({
      originalTag: original.baseEtag,
      replacementTag: latestTag,
      savedTag: final.tag,
      explicitDocumentGets: getsBeforeSave,
      puts: writes(c).length,
    }),
  );
}

async function recoveryControls(url, c, legacy) {
  const p = c.page;
  await open(c, second, "project");
  assert.equal(await record(c, second, "project"), JSON.stringify(legacy));
  const original = await protect(c, second, "project");
  assert.equal(original.baseEtag, legacy.baseEtag);
  await p.click("#settings-save");
  await saved(c);
  assert.equal(writes(c)[0].headers["if-match"], legacy.baseEtag);
  assert.equal(await record(c, second, "project"), null);
  await open(c, sample, "decisions.md");
  assert.equal((await state(c, sample, "decisions.md")).mode, "create");
  assert.equal(await disabled(c, "#settings-save"), false);
  await p.focus("#settings-save");
  await p.keyboard.press("Enter");
  await saved(c);
  assert.equal(writes(c)[1].headers["if-none-match"], "*");
  assert.equal(writes(c)[1].body || "", "");
  assert.equal((await read(url, sample, "decisions.md")).text, "");
  assert.equal((await state(c, sample, "decisions.md")).mode, "edit");
  await open(c, sample, "runbook.md");
  const held = hold(c, "PUT", endpoint(sample, "runbook.md"));
  await p.click("#settings-save");
  const request = await held;
  const empty = await protect(c, sample, "runbook.md");
  assert.equal(empty.mode, "create");
  assert.equal(empty.blocked, "uncertain");
  assert.equal(empty.baseEtag, null);
  assert.equal(empty.baseText, "");
  assert.equal(empty.draftText, "");
  assert.equal(request.headers()["if-none-match"], "*");
  await answer(request, 500, "synthetic uncertain response");
  await p.waitForFunction(
    () => projectDocuments.get("documents-sample/runbook.md").phase === "uncertain",
    { polling: 50 },
  );
  await reload(c);
  await open(c, sample, "runbook.md");
  assert.deepEqual(await protect(c, sample, "runbook.md"), empty);
  await p.click("#settings-copy");
  await p.waitForFunction(
    () => document.getElementById("settings-status").textContent === "Draft copied",
    { polling: 50 },
  );
  assert.deepEqual(await p.evaluate(() => window.copiedDrafts), [""]);
  assert.equal(await disabled(c, "#settings-save"), true);
  await confirmLoad(c, true);
  await p.waitForFunction(
    () => projectDocuments.get("documents-sample/runbook.md").phase !== "loading",
    { polling: 50 },
  );
  assert.deepEqual(await protect(c, sample, "runbook.md"), empty);
  assert.equal(await disabled(c, "#settings-save"), true);
  assert.equal(writes(c).length, 3);
  assert.equal((await read(url, sample, "runbook.md")).status, 404);
  console.log(
    "PASS focused positive controls: exact legacy schema/original If-Match, successful empty expected-absent creation, pending/failed empty uncertain create survives immediate reload/Copy/404 without retry",
  );
}

async function focusAndViews(c) {
  const p = c.page;
  await input(c, (await state(c, second, "notes.md")).draftText + "\nKeyboard reachability draft");
  const eligible = await p.$$eval(
    "#settings-projects select, #settings-projects textarea, #settings-projects button",
    (els) => els.filter((e) => !e.disabled && e.getClientRects().length).map((e) => e.id),
  );
  assert.deepEqual(eligible, [
    "settings-project",
    "project-pickup-mode",
    "project-pickup-load",
    "project-pickup-save",
    "settings-tab-project",
    "settings-tab-notes",
    "settings-tab-decisions",
    "settings-tab-runbook",
    "settings-document",
    "settings-copy",
    "settings-load",
    "settings-save",
  ]);
  const observed = await state(c, second, "notes.md");
  const recovery = {
    version: 1,
    slug: second,
    name: "notes.md",
    mode: "edit",
    baseText: observed.baseText,
    baseEtag: observed.baseEtag,
    draftText: observed.draftText,
    blocked: "uncertain",
  };
  await p.evaluate(({ storageKey, value }) => localStorage.setItem(storageKey, value), {
    storageKey: key(second, "notes.md"),
    value: JSON.stringify(recovery),
  });
  await close(c);
  await reload(c);
  await open(c, second, "notes.md");
  assert.deepEqual(JSON.parse(await record(c, second, "notes.md")), recovery);
  assert.equal(await disabled(c, "#settings-save"), true);
  assert.equal(await p.$eval("#settings-status", (e) => e.getAttribute("role")), "alert");
  for (const [name, width, height, theme] of [
    ["dark", 1440, 900, "dark"],
    ["mobile", 390, 844, "light"],
  ]) {
    await p.setViewport({ width, height, deviceScaleFactor: 1 });
    await p.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
    assert.equal(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
    const strip = await p.$eval("#settings-resource", (list) => {
      const tabs = [...list.querySelectorAll('[role="tab"]')];
      const saved = tabs.map((t) => t.textContent);
      document.getElementById("settings-tab-decisions").textContent = "decisions.md (not created)";
      document.getElementById("settings-tab-runbook").textContent = "runbook.md (not created)";
      const box = list.getBoundingClientRect();
      const clipped = tabs
        .filter((t) => {
          const r = t.getBoundingClientRect();
          return (
            r.left < box.left - 0.5 ||
            r.right > box.right + 0.5 ||
            r.top < box.top - 0.5 ||
            r.bottom > box.bottom + 0.5 ||
            t.scrollWidth > t.clientWidth
          );
        })
        .map((t) => t.id);
      const overflow = list.scrollWidth - list.clientWidth;
      tabs.forEach((t, i) => (t.textContent = saved[i]));
      return { overflow, clipped };
    });
    console.log("D6 tab strip", name, JSON.stringify(strip));
    assert.deepEqual(strip, { overflow: 0, clipped: [] }, "settings tab strip shows every label");
    for (const selector of [
      "#settings-project",
      "#settings-resource",
      "#settings-document",
      "#settings-copy",
      "#settings-load",
      "#settings-save",
      "#settings-status",
    ]) {
      const hit = await p.$eval(selector, (e) => {
        e.scrollIntoView({ block: "nearest" });
        const r = e.getBoundingClientRect();
        let left = Math.max(0, r.left),
          right = Math.min(innerWidth, r.right),
          top = Math.max(0, r.top),
          bottom = Math.min(innerHeight, r.bottom);
        for (let a = e.parentElement; a; a = a.parentElement) {
          if (!/(auto|scroll|hidden)/.test(getComputedStyle(a).overflowY)) continue;
          const clip = a.getBoundingClientRect();
          top = Math.max(top, clip.top);
          bottom = Math.min(bottom, clip.bottom);
        }
        const target = document.elementFromPoint((left + right) / 2, (top + bottom) / 2);
        return {
          left: r.left,
          right: r.right,
          top,
          bottom,
          hit: bottom > top && right > left && (e === target || e.contains(target)),
        };
      });
      console.log("D6 reachability", name, selector, JSON.stringify(hit));
      assert.ok(hit.left >= 0 && hit.right <= width && hit.hit, selector + " reachable hit target");
    }
  }
  console.log("PASS D6 remaining keyboard reachability, mobile/light/dark error and controls");
}

async function browserRun(url) {
  const support = await import("../support/browser-server.mjs");
  support.assertBrowserTarget(url);
  const puppeteer = require("puppeteer-core");
  await seed(url);
  await fixtureReadback(url);
  const browser = await puppeteer.launch({
    executablePath: support.browserExecutable(),
    headless: true,
  });
  const clients = [];
  const make = async (options) => {
    const c = await client(url, browser, options);
    clients.push(c);
    return c;
  };
  try {
    const a = await make(),
      p = a.page;
    const configOriginal = await read(url, sample, "project");
    const boardOriginal = await (await fetch(url + "/api/board")).text();

    await open(a);
    assert.equal(await p.$eval("#settings-resource-help", (e) => e.textContent), projectHelp);
    assert.deepEqual(await describedHelp(a), ["settings-resource-help", projectHelp]);
    await p.waitForFunction(
      () => document.getElementById("settings-tab-decisions").textContent.endsWith("(not created)"),
      { polling: 50 },
    );
    assert.deepEqual(
      await p.$$eval("#settings-resource [role=tab]", (els) => els.map((e) => e.dataset.name)),
      ["project", ...names],
    );
    for (const slug of [sample, second])
      for (const name of names) {
        await open(a, slug, name);
        assert.equal(await p.$eval("#settings-resource-help", (e) => e.textContent), documentHelp);
        assert.deepEqual(await describedHelp(a), ["settings-resource-help", documentHelp]);
        assert.equal(
          await p.$eval("#settings-document", (e) => e.value),
          name === "notes.md" ? notes : "",
        );
        assert.equal((await state(a, slug, name)).mode, name === "notes.md" ? "edit" : "create");
        assert.equal(writes(a).length, 0, "selection never creates");
      }
    await open(a, sample, "notes.md");
    const heldCreate = hold(a, "PUT", endpoint(sample, "decisions.md"));
    await open(a, sample, "decisions.md");
    await p.focus("#settings-save");
    await p.keyboard.press("Enter");
    const createRequest = await heldCreate;
    assert.equal(createRequest.headers()["if-none-match"], "*");
    assert.equal(createRequest.headers()["if-match"], undefined);
    assert.equal(createRequest.postData() || "", "");
    const emptyPending = await protect(a, sample, "decisions.md");
    assert.equal(emptyPending.blocked, "uncertain");
    assert.equal(emptyPending.draftText, "");
    await p.evaluate(() => {
      for (const opts of [{ repeat: true }, { isComposing: true }, {}])
        document
          .getElementById("settings-projects")
          .dispatchEvent(
            new KeyboardEvent("keydown", { key: "s", ctrlKey: true, bubbles: true, ...opts }),
          );
    });
    assert.equal(writes(a).length, 1);
    await createRequest.continue();
    await saved(a);
    assert.equal((await state(a, sample, "decisions.md")).mode, "edit");
    assert.equal((await read(url, sample, "decisions.md")).text, "");
    await p.waitForFunction(
      () => document.getElementById("settings-tab-decisions").textContent === "decisions.md",
      { polling: 50 },
    );
    await open(a, second, "decisions.md");
    await p.focus("#settings-document");
    await p.keyboard.type("Second decisions\n");
    await p.keyboard.down("Control");
    await p.keyboard.press("s");
    await p.keyboard.up("Control");
    await saved(a);
    assert.equal((await read(url, second, "decisions.md")).text, "Second decisions\n");
    for (const slug of [sample, second])
      assert.equal((await read(url, slug, "runbook.md")).status, 404);
    const heldList = hold(a, "GET", "/api/project-documents/" + sample);
    await p.select("#settings-project", sample);
    const listRequest = await heldList;
    await p.select("#settings-project", second);
    await answer(listRequest, 200, JSON.stringify(names.map((name) => ({ name, exists: false }))), {
      "Content-Type": "application/json",
    });
    await p.waitForFunction(
      () => document.getElementById("settings-tab-notes").textContent === "notes.md",
      { polling: 50 },
    );
    const failingList = hold(a, "GET", "/api/project-documents/" + sample);
    await p.select("#settings-project", sample);
    await answer(await failingList, 500, "untrusted");
    await p.waitForFunction(() => !document.getElementById("settings-list-status").hidden, {
      polling: 50,
    });
    assert.equal(
      await p.$eval("#settings-list-status", (e) => e.textContent),
      "Could not load document list. Select a document to load it directly.",
    );
    await clickTab(p, "notes.md");
    assert.equal(await p.$eval("#settings-document", (e) => e.value), notes);
    console.log(
      "PASS D1 fixed slots, source/absence, zero implicit writes, empty/nonempty keyboard creation and stale/failed listing",
    );

    await close(a);
    const legacy = {
      version: 1,
      baseText: configOriginal.text,
      baseEtag: configOriginal.tag,
      draftText: configOriginal.text + "Legacy draft\n",
    };
    const b = await make({ records: { [key(sample, "project")]: JSON.stringify(legacy) } });
    await open(b);
    assert.equal(await record(b, sample, "project"), JSON.stringify(legacy));
    assert.deepEqual(
      await protect(b, sample, "project"),
      stable(await state(b, sample, "project")),
    );
    assert.equal((await state(b, sample, "project")).draftText, legacy.draftText);
    await b.page.click("#settings-save");
    await saved(b);
    assert.equal(writes(b).at(-1).headers["if-match"], legacy.baseEtag);
    assert.equal(await record(b, sample, "project"), null);
    const snapshots = new Map();
    for (const slug of [sample, second])
      for (const name of ["project", "notes.md", "decisions.md"]) {
        await open(b, slug, name);
        const s = await state(b, slug, name);
        await input(b, s.draftText + "\nDraft " + slug + "/" + name);
        snapshots.set(id(slug, name), await protect(b, slug, name));
      }
    await close(b);
    await reload(b);
    for (const slug of [sample, second])
      for (const name of ["project", "notes.md", "decisions.md"]) {
        await open(b, slug, name);
        assert.deepEqual(await protect(b, slug, name), snapshots.get(id(slug, name)));
      }
    const otherRecords = await b.page.evaluate(() =>
      Object.fromEntries(Object.entries(localStorage)),
    );
    await open(b, second, "decisions.md");
    await b.page.click("#settings-save");
    await saved(b);
    delete otherRecords[key(second, "decisions.md")];
    assert.deepEqual(
      await b.page.evaluate(() => Object.fromEntries(Object.entries(localStorage))),
      otherRecords,
    );
    await open(b, sample, "project");
    await input(
      b,
      (await state(b, sample, "project")).draftText.replace(
        "title: Documents sample",
        "title: Changed sample",
      ),
    );
    await b.page.click("#settings-save");
    await saved(b);
    await b.page.waitForFunction(
      () => projects.get("documents-sample").title === "Changed sample",
      { polling: 50 },
    );
    await input(b, "invalid config");
    await b.page.click("#settings-save");
    await b.page.waitForFunction(
      () =>
        document
          .getElementById("settings-status")
          .textContent.startsWith("Could not save: project document needs closed"),
      { polling: 50 },
    );
    assert.equal((await state(b, sample, "project")).draftText, "invalid config");
    console.log(
      "PASS D2 exact legacy record, six resource drafts, native departure/reload, original conditions, isolated clearing and config title/schema",
    );

    const c = await make(),
      d = await make();
    await open(c, sample, "notes.md");
    await open(d, sample, "notes.md");
    const original = await state(d);
    await input(c, "Winner notes\n");
    await input(d, "Loser notes\n");
    await c.page.click("#settings-save");
    await saved(c);
    await d.page.click("#settings-save");
    await d.page.waitForFunction(
      () => projectDocuments.get("documents-sample/notes.md").phase === "conflict",
      { polling: 50 },
    );
    const conflict = await protect(d, sample, "notes.md");
    assert.equal(conflict.baseText, original.baseText);
    assert.equal(conflict.baseEtag, original.baseEtag);
    assert.equal(conflict.blocked, "conflict");
    assert.equal(await disabled(d, "#settings-save"), true);
    await close(d);
    await reload(d);
    await open(d, sample, "notes.md");
    assert.deepEqual(await protect(d, sample, "notes.md"), conflict);
    await d.page.click("#settings-copy");
    await d.page.waitForFunction(
      () => document.getElementById("settings-status").textContent === "Draft copied",
      { polling: 50 },
    );
    assert.deepEqual(await d.page.evaluate(() => window.copiedDrafts), ["Loser notes\n"]);
    await confirmLoad(d, false);
    assert.deepEqual(await protect(d, sample, "notes.md"), conflict);
    let held = hold(d, "GET", endpoint(sample, "notes.md"));
    await confirmLoad(d, true);
    await answer(await held, 500, "failure");
    await d.page.waitForFunction(
      () => projectDocuments.get("documents-sample/notes.md").phase === "conflict",
      { polling: 50 },
    );
    assert.deepEqual(await protect(d, sample, "notes.md"), conflict);
    await confirmLoad(d, true);
    await d.page.waitForFunction(
      () => document.getElementById("settings-status").textContent === "No unsaved changes",
      { polling: 50 },
    );
    assert.equal((await state(d)).baseText, "Winner notes\n");
    assert.equal(await record(d, sample, "notes.md"), null);
    await input(d, "Explicitly reapplied\n");
    await d.page.click("#settings-save");
    await saved(d);
    assert.equal(writes(d).at(-1).headers["if-match"], (await state(c)).baseEtag);
    await open(c, sample, "runbook.md");
    await open(d, sample, "runbook.md");
    await input(c, "Runbook A");
    await input(d, "Runbook B");
    await Promise.all([c.page.click("#settings-save"), d.page.click("#settings-save")]);
    await Promise.all([
      c.page.waitForFunction(
        () => projectDocuments.get("documents-sample/runbook.md").phase !== "saving",
        { polling: 50 },
      ),
      d.page.waitForFunction(
        () => projectDocuments.get("documents-sample/runbook.md").phase !== "saving",
        { polling: 50 },
      ),
    ]);
    const race = [await state(c, sample, "runbook.md"), await state(d, sample, "runbook.md")];
    assert.deepEqual(race.map((s) => s.phase).sort(), ["conflict", "ready"]);
    assert.equal(
      (await read(url, sample, "runbook.md")).text,
      race.find((s) => s.phase === "ready").draftText,
    );
    for (const [slug, name] of [
      [sample, "notes.md"],
      [second, "decisions.md"],
    ]) {
      const cl = slug === sample ? c : d;
      await open(cl, slug, name);
      const remote = await read(url, slug, name);
      await cl.page.click("#settings-load");
      await cl.page.waitForFunction(
        ({ resourceId, tag }) =>
          projectDocuments.get(resourceId).baseEtag === tag &&
          projectDocuments.get(resourceId).phase === "ready",
        { polling: 50 },
        { resourceId: id(slug, name), tag: remote.tag },
      );
      await input(cl, "Independent " + slug + "/" + name);
    }
    await Promise.all([c.page.click("#settings-save"), d.page.click("#settings-save")]);
    await Promise.all([saved(c), saved(d)]);
    assert.equal((await read(url, sample, "notes.md")).text, "Independent " + sample + "/notes.md");
    assert.equal(
      (await read(url, second, "decisions.md")).text,
      "Independent " + second + "/decisions.md",
    );
    console.log(
      "PASS D3 stale edit/conflict exact recovery, same-name create race, independent documents/projects",
      JSON.stringify(race.map(stable)),
    );

    let e = await make();
    await open(e, second, "runbook.md");
    for (const outcome of ["offline", "500", "auth", "redirect", "no-etag", "bad-etag"]) {
      const pathname = endpoint(second, "runbook.md");
      const pending = hold(e, "PUT", pathname);
      await e.page.click("#settings-save");
      const request = await pending;
      assert.equal(request.headers()["if-none-match"], "*");
      assert.equal((await protect(e, second, "runbook.md")).blocked, "uncertain");
      if (outcome === "offline") await request.abort();
      else if (outcome === "auth") await answer(request, 403, "denied");
      else if (outcome === "redirect") {
        e.route = async (req) => {
          if (new URL(req.url()).pathname !== "/synthetic-login") return false;
          e.route = null;
          await answer(req, 200, "login", { "Content-Type": "text/html" });
          return true;
        };
        await answer(request, 302, "", { Location: "/synthetic-login" });
      } else
        await answer(
          request,
          outcome === "500" ? 500 : 200,
          "untrusted",
          outcome === "bad-etag" ? { ETag: "unquoted" } : {},
        );
      await e.page.waitForFunction(
        () => projectDocuments.get("documents-second/runbook.md").phase === "uncertain",
        { polling: 50 },
      );
      assert.equal(await disabled(e, "#settings-save"), true);
      const retained = await protect(e, second, "runbook.md");
      assert.equal(retained.mode, "create");
      assert.equal(retained.baseEtag, null);
      assert.equal(retained.draftText, "");
      const count = e.calls.length;
      await close(e);
      await reload(e);
      await open(e, second, "runbook.md");
      assert.deepEqual(await protect(e, second, "runbook.md"), retained);
      assert.equal(
        e.calls.slice(count).filter((r) => r.path === pathname).length,
        0,
        "restoring uncertainty never fetches baseline",
      );
      const replacement = hold(e, "GET", pathname);
      await confirmLoad(e, true);
      const latest = await replacement;
      assert.equal(
        (await state(e, second, "runbook.md")).blocked,
        "uncertain",
        "loading retains blocked empty draft",
      );
      await answer(latest, 404, "not found");
      await e.page.waitForFunction(
        () => projectDocuments.get("documents-second/runbook.md").phase === "uncertain",
        { polling: 50 },
      );
      assert.deepEqual(await protect(e, second, "runbook.md"), retained);
      if (outcome !== "bad-etag") {
        await e.context.close();
        e = await make();
        await open(e, second, "runbook.md");
      }
    }
    await e.context.close();
    const loss = await make();
    for (const [slug, name, text] of [
      [second, "runbook.md", ""],
      [second, "notes.md", "Committed update with lost response\n"],
    ]) {
      await open(loss, slug, name);
      if (text) await input(loss, text);
      const beforeState = await state(loss, slug, name),
        start = writes(loss).length;
      const pending = hold(loss, "PUT", endpoint(slug, name));
      await loss.page.click("#settings-save");
      const request = await pending;
      const actual = await fetch(request.url(), {
        method: "PUT",
        redirect: "error",
        headers: request.headers(),
        body: request.postData() || "",
      });
      assert.equal(actual.status, 200, "actual R2 commit precedes response loss");
      const committedTag = actual.headers.get("ETag").replace(/^W\//, "");
      await request.abort();
      await loss.page.waitForFunction(
        (resourceId) => projectDocuments.get(resourceId).phase === "uncertain",
        { polling: 50 },
        id(slug, name),
      );
      const uncertain = await protect(loss, slug, name);
      assert.equal(uncertain.baseEtag, beforeState.baseEtag);
      assert.equal(uncertain.mode, beforeState.mode);
      assert.equal((await read(url, slug, name)).text, text);
      assert.equal((await read(url, slug, name)).tag, committedTag);
      await close(loss);
      await reload(loss);
      await open(loss, slug, name);
      assert.deepEqual(await protect(loss, slug, name), uncertain);
      assert.equal(writes(loss).length, start + 1);
      await confirmLoad(loss, true);
      await loss.page.waitForFunction(
        (resourceId) => projectDocuments.get(resourceId).phase === "ready",
        { polling: 50 },
        id(slug, name),
      );
      assert.equal((await state(loss, slug, name)).baseEtag, committedTag);
      assert.equal(await record(loss, slug, name), null);
    }
    for (const storage of ["denied", "full"]) {
      const s = await make({ storage });
      await open(s, second, "notes.md");
      await input(s, "Memory only " + storage);
      assert.ok((await status(s)).includes("Browser storage is unavailable"));
      assert.equal((await state(s, second, "notes.md")).draftText, "Memory only " + storage);
      await close(s);
      await reload(s);
      await s.context.close();
    }
    for (const malformed of [
      "{bad",
      JSON.stringify({
        version: 1,
        slug: second,
        name: "runbook.md",
        mode: "edit",
        baseText: "",
        baseEtag: null,
        draftText: "Keep me",
        blocked: null,
      }),
      JSON.stringify({
        version: 1,
        slug: sample,
        name: "runbook.md",
        mode: "create",
        baseText: "",
        baseEtag: null,
        draftText: "Wrong identity",
        blocked: null,
      }),
    ]) {
      const m = await make({ records: { [key(second, "runbook.md")]: malformed } });
      await open(m, second, "runbook.md");
      assert.ok((await status(m)).includes("A saved draft could not be restored"));
      assert.equal(await record(m, second, "runbook.md"), malformed);
      assert.equal(
        (await state(m, second, "runbook.md")).mode,
        "edit",
        "malformed edit cannot become create",
      );
      assert.equal(writes(m).length, 0);
      await m.context.close();
    }
    const missing = await make();
    await missingEditRecovery(url, missing);
    const faults = await make();
    await open(faults, second, "notes.md");
    await input(faults, "Retain observed edit");
    await faults.page.evaluate(() => {
      window.denyCopy = true;
    });
    await faults.page.click("#settings-copy");
    await faults.page.waitForFunction(
      () =>
        document.getElementById("settings-status").textContent.startsWith("Copy is unavailable"),
      { polling: 50 },
    );
    assert.deepEqual(
      await faults.page.$eval("#settings-document", (e) => [
        e === document.activeElement,
        e.selectionStart,
        e.selectionEnd,
      ]),
      [true, 0, "Retain observed edit".length],
    );
    const editBefore = await protect(faults, second, "notes.md");
    let retainedEdit = editBefore;
    for (const fault of ["missing", "no-etag", "bad-etag", "media", "utf8", "oversize", "auth"]) {
      const pending = hold(faults, "GET", endpoint(second, "notes.md"));
      await confirmLoad(faults, true);
      const request = await pending;
      const headers = { "Content-Type": "text/markdown", ETag: '"valid"' };
      if (fault === "no-etag") delete headers.ETag;
      if (fault === "bad-etag") headers.ETag = "invalid";
      if (fault === "media") headers["Content-Type"] = "text/html";
      await answer(
        request,
        fault === "missing" ? 404 : fault === "auth" ? 403 : 200,
        fault === "utf8"
          ? Buffer.from([255])
          : fault === "oversize"
            ? "x".repeat((1 << 20) + 1)
            : "invalid replacement",
        headers,
      );
      await faults.page.waitForFunction(
        () => projectDocuments.get("documents-second/notes.md").phase !== "loading",
        { polling: 50 },
      );
      if (fault === "missing") retainedEdit = { ...editBefore, blocked: "conflict" };
      assert.deepEqual(await protect(faults, second, "notes.md"), retainedEdit);
      assert.equal((await state(faults, second, "notes.md")).mode, "edit");
      if (fault === "missing") assert.equal(await disabled(faults, "#settings-save"), true);
    }
    await confirmLoad(faults, true);
    await faults.page.waitForFunction(
      () => document.getElementById("settings-status").textContent === "No unsaved changes",
      { polling: 50 },
    );
    await input(faults, "Retain observed edit");
    assert.deepEqual(await protect(faults, second, "notes.md"), editBefore);
    assert.equal(await disabled(faults, "#settings-save"), false);
    const rejection = hold(faults, "PUT", endpoint(second, "notes.md"));
    await faults.page.click("#settings-save");
    const rejected = await rejection;
    assert.equal(rejected.headers()["if-match"], editBefore.baseEtag);
    await answer(rejected, 400, "document contains control characters");
    await faults.page.waitForFunction(
      () =>
        document.getElementById("settings-status").textContent ===
        "Could not save: document contains control characters",
      { polling: 50 },
    );
    assert.deepEqual(await protect(faults, second, "notes.md"), editBefore);
    await input(faults, "Retain observed edit again");
    const editAgain = await protect(faults, second, "notes.md");
    assert.equal(await disabled(faults, "#settings-save"), false);
    const rejectionAgain = hold(faults, "PUT", endpoint(second, "notes.md"));
    await faults.page.click("#settings-save");
    const rejectedAgain = await rejectionAgain;
    assert.equal(rejectedAgain.headers()["if-match"], editBefore.baseEtag);
    await answer(rejectedAgain, 400, "project document: contains the reserved envelope tag");
    await faults.page.waitForFunction(
      () =>
        document.getElementById("settings-status").textContent ===
        "Could not save: project document: contains the reserved envelope tag",
      { polling: 50 },
    );
    assert.deepEqual(await protect(faults, second, "notes.md"), editAgain);
    const hint = () => faults.page.$eval("#settings-hint", (e) => e.textContent);
    const pasteHint =
      "Invisible characters usually come from pasted text. Type the pasted lines again, then save.";
    const markHint =
      "This draft starts with an invisible byte-order mark. Select the whole first line, type it again, check the line break is still there, then save.";
    const invisible = "project document: contains control, invisible or bidirectional characters";
    assert.equal(
      await hint(),
      "Remove the operator-preferences tag, including any look-alike spelling, then save. The factory keeps that tag for its agent prompts.",
    );
    await input(faults, "﻿Release steps live here.\nTag the build first.\n");
    assert.equal(await hint(), "");
    const markRejection = hold(faults, "PUT", endpoint(second, "notes.md"));
    await faults.page.click("#settings-save");
    await answer(await markRejection, 400, invisible);
    await faults.page.waitForFunction(
      (text) => document.getElementById("settings-status").textContent === text,
      { polling: 50 },
      "Could not save: " + invisible,
    );
    assert.equal(await hint(), markHint);
    await faults.page.click("#settings-document", { count: 3, offset: { x: 40, y: 14 } });
    await faults.page.keyboard.type("Release steps live here.\n");
    assert.equal(
      await faults.page.$eval("#settings-document", (e) => e.value),
      "Release steps live here.\nTag the build first.\n",
    );
    assert.equal(await hint(), "");
    for (const [message, draftText, expected] of [
      [
        "Could not save: project document: looks like a secret",
        "x",
        "Remove the key or token, then save. Say where the secret is kept instead of pasting it.",
      ],
      ["Could not save: document contains control characters", "x", pasteHint],
      ["Could not save: " + invisible, "zero​width", pasteHint],
      ["Could not save: project document needs closed --- frontmatter", "﻿---", markHint],
      ["Could not save: project document needs closed --- frontmatter", "x", ""],
      ["Could not save. Your draft is kept. Try again.", "﻿x", ""],
    ])
      assert.equal(
        await faults.page.evaluate(
          (message, draftText) => settingsRefusalHint({ message, draftText }),
          message,
          draftText,
        ),
        expected,
        message,
      );
    console.log(
      "PASS D4 network/auth/ETag/bytes/storage/clipboard failures; real committed create/update response loss; retained identity and no retries",
    );

    const delayed = await make();
    let pending = hold(delayed, "GET", endpoint(sample, "notes.md"));
    await show(delayed);
    await delayed.page.select("#settings-project", sample);
    await delayed.page.waitForFunction(
      () => document.getElementById("settings-project").value === "documents-sample",
      { polling: 50 },
    );
    await clickTab(delayed.page, "notes.md");
    const old = await pending;
    await open(delayed, sample, "runbook.md");
    await open(delayed, second, "notes.md");
    await input(delayed, "Current second notes draft");
    const current = await protect(delayed, second, "notes.md");
    await answer(old, 200, "Old delayed source", {
      "Content-Type": "text/markdown",
      ETag: '"old"',
    });
    await delayed.page.waitForFunction(
      () => projectDocuments.get("documents-sample/notes.md").phase !== "loading",
      { polling: 50 },
    );
    assert.deepEqual(await protect(delayed, second, "notes.md"), current);
    await delayed.page.evaluate(() => poll());
    assert.deepEqual(await protect(delayed, second, "notes.md"), current);
    pending = hold(delayed, "PUT", endpoint(second, "notes.md"));
    await delayed.page.click("#settings-save");
    const saving = await pending;
    await close(delayed);
    await open(delayed, sample, "decisions.md");
    await input(delayed, "Other pending draft");
    const other = await protect(delayed, sample, "decisions.md");
    await saving.continue();
    await delayed.page.waitForFunction(
      () => projectDocuments.get("documents-second/notes.md").phase === "ready",
      { polling: 50 },
    );
    assert.equal(await record(delayed, second, "notes.md"), null);
    assert.deepEqual(await protect(delayed, sample, "decisions.md"), other);
    const first = hold(delayed, "GET", endpoint(sample, "decisions.md"));
    await confirmLoad(delayed, true);
    const firstRequest = await first;
    const secondHold = hold(delayed, "GET", endpoint(sample, "decisions.md"));
    await delayed.page.evaluate(() => {
      loadProjectDocument(settingsResource("documents-sample", "decisions.md"), true);
    });
    const secondRequest = await secondHold;
    await answer(secondRequest, 200, "New replacement", {
      "Content-Type": "text/markdown",
      ETag: '"new"',
    });
    await delayed.page.waitForFunction(
      () => document.getElementById("settings-document").value === "New replacement",
      { polling: 50 },
    );
    await input(delayed, "New input after replacement");
    const latest = await protect(delayed, sample, "decisions.md");
    await answer(firstRequest, 200, "Superseded replacement", {
      "Content-Type": "text/markdown",
      ETag: '"superseded"',
    });
    await delayed.page.waitForNetworkIdle({ idleTime: 50 });
    assert.deepEqual(await protect(delayed, sample, "decisions.md"), latest);
    console.log(
      "PASS D5 held cross-resource load, closed captured save, superseded same-resource load, input and polling snapshots",
    );

    const inert =
      '</textarea><script>window.__executed=1</script>\n<img src="https://invalid.example/x" onerror="window.__executed=1">\n```mermaid\ngraph LR; A-->B\n```\n[bad](javascript:alert(1))\n';
    const keyboard = await make();
    await open(keyboard, second, "notes.md");
    await keyboard.page.focus(tab("notes.md"));
    for (const [press, expected] of [
      ["Home", "project"],
      ["ArrowRight", "notes.md"],
      ["End", "runbook.md"],
      ["ArrowRight", "project"],
      ["ArrowLeft", "runbook.md"],
      ["ArrowLeft", "decisions.md"],
      ["ArrowLeft", "notes.md"],
    ]) {
      await keyboard.page.keyboard.press(press);
      assert.deepEqual(
        await keyboard.page.evaluate(() => {
          const selected = document.querySelector('#settings-resource [aria-selected="true"]');
          return [
            document.activeElement.dataset.name,
            selected.dataset.name,
            selected.tabIndex,
            document.querySelectorAll('#settings-resource [tabindex="0"]').length,
            document.getElementById("settings-fields").getAttribute("aria-labelledby"),
          ];
        }),
        [expected, expected, 0, 1, tab(expected).slice(1)],
        press,
      );
    }
    await keyboard.page.waitForFunction(
      () => !document.getElementById("settings-document").disabled,
      { polling: 50 },
    );
    await keyboard.page.focus("#settings-document");
    await keyboard.page.keyboard.type(inert);
    const inertBase = await state(keyboard, second, "notes.md");
    const heldSave = hold(keyboard, "PUT", endpoint(second, "notes.md"));
    await keyboard.page.keyboard.down("Control");
    await keyboard.page.keyboard.press("s");
    await keyboard.page.keyboard.up("Control");
    const inertRequest = await heldSave;
    await keyboard.page.evaluate(() => {
      for (const opts of [{ repeat: true }, { isComposing: true }])
        document
          .getElementById("settings-projects")
          .dispatchEvent(
            new KeyboardEvent("keydown", { key: "s", ctrlKey: true, bubbles: true, ...opts }),
          );
    });
    await inertRequest.continue();
    await saved(keyboard);
    assert.equal(writes(keyboard).length, 1);
    assert.equal((await state(keyboard, second, "notes.md")).baseText, inertBase.draftText);
    assert.equal(await keyboard.page.evaluate(() => window.__executed), undefined);
    assert.equal(await keyboard.page.$eval("#settings-document", (e) => e.children.length), 0);
    await keyboard.page.focus("#settings-copy");
    await keyboard.page.keyboard.press("Enter");
    await keyboard.page.waitForFunction(
      () => document.getElementById("settings-status").textContent === "Draft copied",
      { polling: 50 },
    );
    assert.deepEqual(await keyboard.page.evaluate(() => window.copiedDrafts), [
      inertBase.draftText,
    ]);
    await keyboard.page.focus("#settings-document");
    await keyboard.page.keyboard.type("Temporary keyboard draft");
    await keyboard.page.focus("#settings-load");
    await confirmLoad(keyboard, true, "Discard this draft and load the latest document?", () =>
      keyboard.page.keyboard.press("Enter"),
    );
    await keyboard.page.waitForFunction(
      () => document.getElementById("settings-status").textContent === "No unsaved changes",
      { polling: 50 },
    );
    assert.equal((await state(keyboard, second, "notes.md")).draftText, inertBase.draftText);
    await focusAndViews(keyboard);
    assert.equal(
      await (await fetch(url + "/api/board")).text(),
      boardOriginal,
      "extra edits never change board",
    );
    assert.equal(
      (await read(url, second, "project")).text,
      config,
      "extra edits never change other project config",
    );
    console.log(
      "PASS D6 inert source, shortcut single writes, modal guard and remaining focus/layout checks",
    );
    for (const cl of clients) {
      assert.deepEqual(cl.errors, []);
      assert.deepEqual(cl.external, []);
      cl.route = null;
    }
    console.log("PASS D1-D6 actual local Worker browser acceptance");
  } finally {
    await browser.close();
  }
}

(async () => {
  const i = process.argv.indexOf("--browser");
  assert.ok(
    i >= 0 && process.argv.length === i + 2,
    "project documents requires --browser <owned-loopback-origin>",
  );
  await browserRun(process.argv[i + 1]);
})().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
