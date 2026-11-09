import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  withBrowserFixture,
  seedBoard,
  seedDocument,
  seedProject,
} from "../support/browser-server.mjs";

const html = await readFile(new URL("../../web/dashboard.html", import.meta.url), "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const settings = script.slice(
  script.indexOf("const projectDocuments = new Map();"),
  script.indexOf("function renderBoard()"),
);
const slug = "documents-sample",
  name = "notes.md";
const storageKey = "factory-project-document-draft:" + slug + ":" + name;
const base = {
  baseText: "Original notes\n",
  baseEtag: '"original"',
  draftText: "Intended notes\n",
  mode: "edit",
  blocked: null,
  phase: "ready",
  message: "Unsaved changes",
};
const json = (x) => JSON.parse(JSON.stringify(x));
const response = (status, text = "", tag = null) =>
  new Response(text, {
    status,
    headers: { "Content-Type": "text/markdown; charset=utf-8", ...(tag && { ETag: tag }) },
  });

function setup(fetch, records = [], sessionRecords = []) {
  const storage = new Map(records),
    session = new Map(sessionRecords),
    writes = [],
    calls = [],
    elements = new Map(),
    windowEvents = new Map();
  const document = { activeElement: null };
  const $ = (id) => {
    if (!elements.has(id))
      elements.set(id, {
        id,
        value: "",
        hidden: false,
        textContent: "",
        open: false,
        listeners: new Map(),
        options: [],
        get disabled() {
          return this._disabled ?? false;
        },
        set disabled(value) {
          this._disabled = Boolean(value);
        },
        addEventListener(type, fn) {
          this.listeners.set(type, fn);
        },
        get parentElement() {
          return $(this.id + "/..");
        },
        setAttribute(name, value) {
          this[name] = value;
        },
        getAttribute(name) {
          return this[name] ?? null;
        },
        focus() {
          document.activeElement = this;
        },
        select() {
          this.selectionStart = 0;
          this.selectionEnd = this.value.length;
        },
        replaceChildren(...options) {
          this.options = options;
        },
        add(option) {
          this.options.push(option);
        },
        querySelector(selector) {
          return $(this.id + "/" + selector);
        },
        showModal() {
          this.open = true;
        },
        close() {
          this.open = false;
          this.listeners.get("close")?.();
        },
      });
    return elements.get(id);
  };
  document.getElementById = $;
  $("settings-project").value = slug;
  $("settings-tab-notes").setAttribute("aria-selected", "true");
  const context = vm.createContext({
    $,
    document,
    Headers,
    TextDecoder,
    Response,
    projects: new Map([
      [slug, { title: "Synthetic" }],
      ["documents-second", { title: "Second" }],
    ]),
    filter: "",
    Option: function (text, value) {
      this.textContent = text;
      this.value = value;
    },
    localStorage: {
      getItem: (key) => storage.get(key) ?? null,
      setItem: (key, value) => {
        storage.set(key, value);
        writes.push({ key, value });
      },
      removeItem: (key) => storage.delete(key),
    },
    sessionStorage: {
      getItem: (key) => session.get(key) ?? null,
      removeItem: (key) => session.delete(key),
    },
    window: { addEventListener: (type, fn) => windowEvents.set(type, fn) },
    navigator: {
      clipboard: {
        writeText: async (text) => {
          if (context.denyCopy) throw new Error("synthetic clipboard denial");
          context.copied.push(text);
        },
      },
    },
    copied: [],
    denyCopy: false,
    confirm: () => true,
    fetch: async (url, options = {}) => {
      calls.push({ url, options });
      return fetch(url, options);
    },
    loadProjects: async () => true,
    renderBoard: () => {},
    refreshPicker: () => {},
  });
  vm.runInContext(
    settings +
      `
    this.api={settingsResource,loadProjectDocument,saveProjectDocument,selectSettingsDocument,renderProjectSettings,
      state:(slug,name)=>projectDocuments.get(settingsResource(slug,name).id),
      seed:(slug,name,fields)=>projectDocuments.set(settingsResource(slug,name).id,{...fields,resource:settingsResource(slug,name)}),
    };`,
    context,
  );
  const resource = context.api.settingsResource(slug, name);
  const fire = (id, type, event = {}) => $(id).listeners.get(type)(event);
  const input = (text) => {
    $("settings-document").value = text;
    return fire("settings-document", "input");
  };
  return {
    context,
    api: context.api,
    $,
    calls,
    storage,
    session,
    writes,
    windowEvents,
    resource,
    fire,
    input,
    state: () => json(context.api.state(slug, name)),
    seed: (fields = base) => {
      context.api.seed(slug, name, fields);
      context.api.renderProjectSettings();
    },
  };
}

function original(s, draft = base.draftText) {
  assert.equal(s.mode, "edit");
  assert.equal(s.baseText, base.baseText);
  assert.equal(s.baseEtag, base.baseEtag);
  assert.equal(s.draftText, draft);
}

test("Copy cannot unblock a missing observed edit, including denial and subsequent input", async () => {
  const s = setup(async () => response(404));
  s.seed();
  await s.api.loadProjectDocument(s.resource, true);
  assert.equal(s.$("settings-save").disabled, true);
  await s.fire("settings-copy", "click");
  assert.equal(s.$("settings-status").textContent, "Draft copied");
  assert.equal(
    s.$("settings-save").disabled,
    true,
    "Copy must not enable Save after a missing edit",
  );
  assert.deepEqual(json(s.context.copied), [base.draftText]);
  s.context.denyCopy = true;
  await s.fire("settings-copy", "click");
  assert.equal(s.$("settings-save").disabled, true);
  assert.equal(s.context.document.activeElement.id, "settings-document");
  assert.equal(s.$("settings-document").selectionEnd, base.draftText.length);
  s.input("Typed after missing\n");
  assert.equal(s.$("settings-save").disabled, true);
  original(s.state(), "Typed after missing\n");
  await s.fire("settings-save", "click");
  assert.equal(s.calls.filter((c) => c.options.method === "PUT").length, 0);
});

test("missing observed edits persist the block immediately, including clean drafts and immediate reload", async () => {
  const other = [
    "factory-project-draft:documents-second",
    JSON.stringify({
      version: 1,
      baseText: "Other",
      baseEtag: '"other"',
      draftText: "Other draft",
    }),
  ];
  for (const draftText of [base.draftText, base.baseText]) {
    const s = setup(async () => response(404), [other]);
    s.seed({ ...base, draftText });
    await s.api.loadProjectDocument(s.resource, true);
    const record = JSON.parse(s.storage.get(storageKey));
    assert.deepEqual(record, {
      version: 1,
      slug,
      name,
      mode: "edit",
      baseText: base.baseText,
      baseEtag: base.baseEtag,
      draftText,
      blocked: "conflict",
    });
    assert.equal(s.writes.at(-1).key, storageKey);
    let prevented = false;
    s.windowEvents.get("beforeunload")({
      preventDefault() {
        prevented = true;
      },
    });
    assert.equal(prevented, true, "even a clean missing edit protects tab departure");
    const restored = setup(async () => {
      throw new Error("restoration must not request a new base");
    }, [...s.storage]);
    restored.api.selectSettingsDocument();
    original(restored.state(), draftText);
    assert.equal(restored.state().blocked, "conflict");
    assert.equal(restored.$("settings-save").disabled, true);
    assert.equal(restored.storage.get(other[0]), other[1]);
    assert.equal(restored.calls.length, 0);
  }
});

test("failed replacements preserve prior uncertainty or conflict and the original edit condition", async () => {
  for (const blocked of ["uncertain", "conflict"]) {
    const outcomes = [404, 500, 200];
    const s = setup(async () => response(outcomes.shift(), "replacement without an ETag"));
    s.seed({ ...base, blocked, phase: blocked });
    for (let i = 0; i < 3; i++) {
      const pending = s.api.loadProjectDocument(s.resource, true);
      assert.equal(
        s.state().blocked,
        blocked,
        "replacement loading keeps the stronger prior block",
      );
      await pending;
      original(s.state());
      assert.equal(s.state().blocked, blocked);
      assert.equal(s.$("settings-save").disabled, true);
      await s.api.saveProjectDocument();
    }
    assert.equal(s.calls.filter((c) => c.options.method === "PUT").length, 0);
  }
});

test("only validated explicit replacement clears the block and the next save uses that new observed tag", async () => {
  const other = [
    "factory-project-document-draft:documents-second:decisions.md",
    "keep-other-record",
  ];
  const queue = [
    response(404),
    response(500),
    response(200, "Latest notes\n", 'W/"latest"'),
    response(200, "ok", '"saved"'),
  ];
  const s = setup(async () => queue.shift(), [other]);
  s.seed();
  await s.api.loadProjectDocument(s.resource, true);
  await s.api.loadProjectDocument(s.resource, true);
  assert.equal(s.$("settings-save").disabled, true);
  original(s.state());
  await s.api.loadProjectDocument(s.resource, true);
  assert.equal(s.state().blocked, null);
  assert.equal(s.state().baseText, "Latest notes\n");
  assert.equal(s.state().baseEtag, '"latest"');
  assert.equal(s.storage.has(storageKey), false);
  s.input("Explicitly reapplied\n");
  await s.api.saveProjectDocument();
  const puts = s.calls.filter((c) => c.options.method === "PUT");
  assert.equal(puts.length, 1);
  assert.deepEqual(json(puts[0].options.headers), {
    "If-Match": '"latest"',
    "Content-Type": "text/markdown; charset=utf-8",
  });
  assert.equal(puts[0].options.body, "Explicitly reapplied\n");
  assert.equal(s.state().baseEtag, '"saved"');
  assert.equal(s.state().baseText, "Explicitly reapplied\n");
  assert.equal(s.storage.get(other[0]), other[1]);
  assert.equal(queue.length, 0);
});

test("legacy configuration records, fresh empty creation and uncertain empty creates keep their contracts", async () => {
  const legacy = {
    version: 1,
    baseText: "Legacy base",
    baseEtag: '"legacy"',
    draftText: "Legacy draft",
  };
  const l = setup(
    async () => response(200, "ok", '"legacy-saved"'),
    [["factory-project-draft:" + slug, JSON.stringify(legacy)]],
  );
  l.$("settings-tab-notes").setAttribute("aria-selected", "false");
  l.api.selectSettingsDocument();
  assert.equal(l.$("settings-save").disabled, false);
  await l.api.saveProjectDocument();
  assert.equal(l.calls[0].options.headers["If-Match"], '"legacy"');
  assert.equal(l.storage.has("factory-project-draft:" + slug), false);
  let finish;
  const pending = new Promise((resolve) => (finish = resolve));
  const s = setup(async (url, options) => (options.method === "PUT" ? pending : response(404)));
  await s.api.loadProjectDocument(s.resource);
  assert.equal(s.state().mode, "create");
  assert.equal(s.$("settings-save").disabled, false);
  const saving = s.api.saveProjectDocument();
  const record = JSON.parse(s.storage.get(storageKey));
  assert.deepEqual(record, {
    version: 1,
    slug,
    name,
    mode: "create",
    baseText: "",
    baseEtag: null,
    draftText: "",
    blocked: "uncertain",
  });
  assert.equal(s.calls.at(-1).options.headers["If-None-Match"], "*");
  assert.equal(s.calls.at(-1).options.body, "");
  finish(response(500));
  await saving;
  await s.api.loadProjectDocument(s.resource, true);
  assert.equal(s.state().blocked, "uncertain");
  assert.equal(s.state().mode, "create");
  assert.equal(s.state().baseEtag, null);
  assert.equal(s.$("settings-save").disabled, true);
});

test("a draft left in sessionStorage by the previous version moves to localStorage once", () => {
  const record = JSON.stringify({
    version: 1,
    slug,
    name,
    mode: "edit",
    baseText: base.baseText,
    baseEtag: base.baseEtag,
    draftText: base.draftText,
    blocked: "uncertain",
  });
  const s = setup(async () => response(500), [], [[storageKey, record]]);
  s.api.selectSettingsDocument();
  original(s.state());
  assert.equal(s.state().blocked, "uncertain");
  assert.equal(s.calls.length, 0);
  assert.equal(s.storage.get(storageKey), record);
  assert.equal(s.session.has(storageKey), false);
  assert.equal(s.$("settings-save").disabled, true);
});

test(
  "a notes.md draft survives closing the page and keeps its base ETag",
  { timeout: 120000 },
  async () => {
    await prepareAssets();
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(
        origin,
        "sample",
        "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
      );
      await seedBoard(origin, []);
      await seedDocument(origin, "sample", "notes.md", "Original notes\n");
      const context = await browser.createBrowserContext();
      const open = async () => {
        const page = await context.newPage();
        await page.goto(origin + "/#/settings/projects", { waitUntil: "domcontentloaded" });
        await page.waitForFunction(
          () =>
            document.getElementById("settings-tab-decisions").textContent.endsWith("(not created)"),
          { polling: 50 },
        );
        await page.$eval("#settings-tab-notes", (el) => el.click());
        await page.waitForFunction(() => !document.getElementById("settings-document").disabled, {
          polling: 50,
        });
        return page;
      };
      const first = await open();
      assert.equal(await first.$eval("#settings-document", (el) => el.value), "Original notes\n");
      const etag = await first.evaluate(async () =>
        (await fetch("./api/project-documents/sample/notes.md", { cache: "no-store" })).headers
          .get("ETag")
          .replace(/^W\//, ""),
      );
      await first.type("#settings-document", "Draft line\n");
      await first.close();

      const second = await open();
      assert.equal(
        await second.$eval("#settings-document", (el) => el.value),
        "Original notes\nDraft line\n",
      );
      const record = JSON.parse(
        await second.evaluate(() =>
          localStorage.getItem("factory-project-document-draft:sample:notes.md"),
        ),
      );
      assert.equal(record.baseEtag, etag);
      assert.equal(
        await second.$eval("#settings-status", (el) => el.textContent),
        "Restored unsaved draft. Its original base version is kept.",
      );
      await context.close();
    });
  },
);
