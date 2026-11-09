// The global factory dashboard and device API, served from one R2 bucket.
// Cloudflare Access guards every path. Devices authenticate with a service token.
import { authenticate } from "./access.ts";
import {
  checkOperatorText,
  effectivePipeline,
  hasReservedTag,
  HOST_NAME,
  validatePipeline,
} from "./admission.js";
import { escapeHtml, readServerConfig, renderDashboard, serveAsset } from "./assets.js";
import { announce, connectEvents, EVENTS_PATH } from "./events.js";
import {
  newPairingCode,
  OFFLINE_MINUTES,
  validateHostCreate,
  validateHostPatch,
  validateHostStatus,
} from "./hosts.js";
import { parseTriageReply, TRIAGE_MODEL, TRIAGE_SCHEMA, triageMessages } from "./triage.js";

export { BoardEvents } from "./events.js";

// A job id starts with a word character, so "." and ".." are never ids.
const JOB_FILE =
  /^\/jobs\/[\w-]+\/\w[\w.-]*\/((round-\d+\/)?[\w.-]+\.(md|png|jpe?g|gif|webp)|(round-\d+\/)?[\w-]+\.walkthrough\.html|walkthrough\.html)$/;
const JOB_DIR = /^\/api\/jobs\/([\w-]+)\/(\w[\w.-]*)\/(.*)$/;
const JOB_REL =
  /^((round-\d+\/)?[\w.-]+\.(md|png|jpe?g|gif|webp)|(round-\d+\/)?[\w-]+\.walkthrough\.html|walkthrough\.html)$/;
const PROJECT = /^\/api\/projects\/([\w-]+)\.md$/;
const PROJECT_DOCUMENT_NAMES = ["notes.md", "decisions.md", "runbook.md"];
const PROJECT_DOCUMENTS = /^\/api\/project-documents\/([A-Za-z0-9_-]+)$/;
const PROJECT_DOCUMENT = /^\/api\/project-documents\/([A-Za-z0-9_-]+)\/([^/]+)$/;
const REPO = /^[\w.-]+\/[\w.-]+$/;
const BASE = /^[A-Za-z0-9][A-Za-z0-9._/-]{0,199}$/;
// Pipeline scopes are "global" or "projects/<slug>"; history keeps every saved version.
const PIPELINE = /^\/api\/pipeline\/(global|projects\/([\w-]+))$/;
const PIPELINE_HISTORY = /^\/api\/pipeline\/history\/(global|projects\/([\w-]+))(?:\/([1-9]\d*))?$/;
const PIPELINE_EFFECTIVE = /^\/api\/pipeline\/effective\/([\w-]+)$/;
const MAX_PIPELINE = 16 << 10;
// 2000 + 6000 characters at up to 4 UTF-8 bytes each, plus JSON keys and escapes.
const MAX_TRIAGE = 64 << 10;
// /api/hosts, /api/hosts/<id> and /api/hosts/<id>/<action>.
const HOSTS = /^\/api\/hosts(?:\/([^/]+)(?:\/(code|pair|status|history))?)?$/;
const MAX_BODY = 1 << 20;
const MAX_IMAGE = 5 << 20;
// The server picks the type from the extension and never trusts a client header.
const IMAGE_TYPES = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};
const imageType = (name) => IMAGE_TYPES[name.slice(name.lastIndexOf(".") + 1)];
// Images get a sandbox CSP, so a file posing as an image can't run as a page.
const IMAGE_HEADERS = { "Content-Security-Policy": "default-src 'none'; sandbox" };
// A job's walkthroughs are agent-written HTML and script. The sandbox CSP gives each an
// opaque origin wherever it loads, so it can't read cookies, storage or the dashboard, and
// default-src 'none' stops every fetch, image, font and frame it might request.
const HTML_CSP =
  "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; " +
  "style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; " +
  "frame-ancestors 'self'";
// no-transform stops Cloudflare's edge rewriting the HTML, which would drop the ETag a device pull needs.
const HTML_HEADERS = {
  "Content-Security-Policy": HTML_CSP,
  "Referrer-Policy": "no-referrer",
  "Cache-Control": "no-store, no-transform",
};
// A browser gets walkthrough.html only as a frame load. Opened as its own tab, a sandboxed
// page could still navigate itself off-site, and only the dashboard's frame-src stops that.
// A device pull sends no Fetch Metadata and signs in as a service.
// The refusal links to the job's tab in the dashboard, which reads #<id>/<page>.
const FRAME_ONLY_CSP =
  "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";
const frameLoad = (request, principal) => {
  const dest = request.headers.get("Sec-Fetch-Dest");
  return dest === "iframe" || (dest === null && principal.kind === "service");
};

// Pasted idea images: content-addressed PNGs the browser already decoded and re-encoded.
const IDEA_IMAGE = /^[0-9a-f]{64}\.png$/;
const IDEA_IMAGE_ROUTE = /^\/api\/idea-images\/([^/]+)$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const MAX_IDEA_IMAGES = 4;
const MAX_NEW_REFS = 8;
const MAX_SIDE = 4096;
const MAX_PIXELS = 16_000_000;
const PNG_SIGNATURE = [137, 80, 78, 71, 13, 10, 26, 10];

// Text types get a charset. Image types are sent exactly as given.
function reply(body, status = 200, type = "text/plain", headers = {}) {
  return new Response(body, {
    status,
    headers: {
      "Content-Type": type.startsWith("image/") ? type : `${type}; charset=utf-8`,
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
      ...headers,
    },
  });
}

const notFound = () => reply("not found", 404);

// Flat `key: value` frontmatter between --- lines, as factory.py reads it.
function frontmatter(text) {
  const lines = text.split(/\r?\n/);
  if (lines[0].trim() !== "---") return {};
  const meta = {};
  for (const raw of lines.slice(1)) {
    const line = raw.trim();
    if (line === "---") return meta;
    const i = line.indexOf(":");
    if (line && !line.startsWith("#") && i > 0) {
      let value = line.slice(i + 1).trim();
      if (value.length >= 2 && value[0] === value.at(-1) && `"'`.includes(value[0]))
        value = value.slice(1, -1);
      meta[line.slice(0, i).trim()] = value;
    }
  }
  return {};
}

function validateProjectDocument(text) {
  // oxlint-disable-next-line no-control-regex -- this pattern exists to reject control characters.
  if (/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/u.test(text))
    return "project document contains control characters";
  const lines = text.split(/\r?\n/);
  const end = lines.findIndex((line, i) => i > 0 && line === "---");
  if (lines[0] !== "---" || end < 0) return "project document needs closed --- frontmatter";
  const meta = new Map();
  const fieldsError =
    "project frontmatter allows one repo, base and title, plus an optional archive";
  for (const line of lines.slice(1, end)) {
    if (!line.trim() || line.trim().startsWith("#")) continue;
    const match = /^(repo|base|title|archive):\s*(.*)$/.exec(line);
    if (!match || meta.has(match[1])) return fieldsError;
    let value = match[2].trim();
    if (/^["']|["']$/.test(value)) {
      if (value.length < 2 || value[0] !== value.at(-1)) return fieldsError;
      value = value.slice(1, -1);
    } else if (/^[\[{!&*|>]/.test(value)) return fieldsError;
    meta.set(match[1], value);
  }
  if (meta.size !== 3 + (meta.has("archive") ? 1 : 0)) return fieldsError;
  const repo = meta.get("repo"),
    base = meta.get("base"),
    title = meta.get("title"),
    archive = meta.get("archive");
  const badRepo = (value) =>
    value &&
    (!/^[A-Za-z0-9_.-]{1,100}\/[A-Za-z0-9_.-]{1,100}$/.test(value) ||
      value.split("/").some((p) => p === "." || p === ".."));
  if (badRepo(repo)) return "invalid project repo";
  if (badRepo(archive)) return "invalid project archive";
  if (
    !/^[A-Za-z0-9][A-Za-z0-9._/-]{0,199}$/.test(base) ||
    base.includes("..") ||
    base.includes("//") ||
    base.endsWith("/") ||
    base.split("/").some((p) => p.startsWith(".") || p.endsWith(".") || p.endsWith(".lock"))
  )
    return "invalid project base";
  // oxlint-disable-next-line no-control-regex -- this pattern exists to reject control characters.
  if (!title || [...title].length > 160 || /[\x00-\x1f\x7f-\x9f]/u.test(title))
    return "invalid project title";
  return null;
}

async function projects(env) {
  const out = [];
  const { objects } = await env.BUCKET.list({ prefix: "projects/" });
  for (const { key } of objects) {
    const slug = key.slice("projects/".length, -".md".length);
    if (!key.endsWith(".md") || !/^[\w-]+$/.test(slug)) continue;
    const obj = await env.BUCKET.get(key);
    if (!obj) continue; // deleted between list and get
    const meta = frontmatter(await obj.text());
    const item = { slug, title: meta.title || slug };
    if (REPO.test(meta.repo || "")) item.repo = meta.repo;
    if (BASE.test(meta.base || "")) item.base = meta.base;
    if (REPO.test(meta.archive || "")) item.archive = meta.archive;
    out.push(item);
  }
  return reply(JSON.stringify(out), 200, "application/json");
}

async function getFile(env, key, type, headers = {}) {
  const obj = await env.BUCKET.get(key);
  return obj ? reply(obj.body, 200, type, { ETag: obj.httpEtag, ...headers }) : notFound();
}

// The 403 page for a walkthrough opened outside a frame, with one link back to its job.
function frameOnly(key) {
  const parts = key.split("/");
  const id = parts[2];
  const file = parts.slice(3).join("/");
  const page = file === "walkthrough.html" ? "spec" : file.slice(0, -".walkthrough.html".length);
  const href = "/#" + encodeURIComponent(id) + "/" + page;
  return reply(
    `<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>Open in the dashboard</title>
<p>Open this walkthrough from its job in the dashboard.</p>
<p><a href="${escapeHtml(href)}" target="_top">Open job ${escapeHtml(id)}</a></p>
`,
    403,
    "text/html",
    { "Content-Security-Policy": FRAME_ONLY_CSP },
  );
}

// Markdown, an image with its exact type and a locked-down CSP, or a job's walkthroughs
// under the sandbox CSP.
function getJobFile(request, env, principal, key) {
  if (key.endsWith("walkthrough.html"))
    return frameLoad(request, principal)
      ? getFile(env, key, "text/html", HTML_HEADERS)
      : frameOnly(key);
  const type = imageType(key);
  return type ? getFile(env, key, type, IMAGE_HEADERS) : getFile(env, key, "text/markdown");
}

// The request body, or null past max bytes. Content-Length is checked first, and the stream
// is counted as it arrives, so a chunked upload with no length is never buffered past the cap.
async function readCapped(request, max) {
  if (Number(request.headers.get("Content-Length")) > max) return null;
  if (!request.body) return new Uint8Array(0);
  const reader = request.body.getReader();
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    // Past the cap, keep reading but drop the bytes. Cancelling the stream instead breaks
    // the connection for the client's next request.
    if (size <= max) chunks.push(value);
  }
  if (size > max) return null;
  const body = new Uint8Array(size);
  let at = 0;
  for (const c of chunks) {
    body.set(c, at);
    at += c.byteLength;
  }
  return body;
}

async function readBody(request) {
  const body = await readCapped(request, MAX_BODY);
  return body === null ? null : new TextDecoder().decode(body);
}

// The R2 onlyIf for a PUT's If-Match: "<etag>" or If-None-Match: *. undefined when the
// request sends neither, null when it sends something else.
function condition(request) {
  // Cloudflare's edge may hand clients a weak W/"..." ETag. R2 compares the bare quoted one.
  const ifMatch = request.headers.get("If-Match")?.replace(/^W\//, "");
  const ifNone = request.headers.get("If-None-Match");
  if (!ifMatch && !ifNone) return undefined;
  const cond = new Headers();
  if (ifMatch && !ifNone && /^"[^"*]+"$/.test(ifMatch)) cond.set("If-Match", ifMatch);
  else if (ifNone === "*" && !ifMatch) cond.set("If-None-Match", "*");
  else return null;
  return cond;
}

const needsCondition = () => reply('PUT needs If-Match: "<etag>" or If-None-Match: *', 428);

// Board and backlog writes must name the exact version they replace, or create the
// file only if it's absent, so no device clobbers another.
async function casPut(request, env, key, type) {
  const cond = condition(request);
  if (!cond) return needsCondition();
  const body = await readBody(request);
  if (body === null) return reply("body over 1 MiB", 413);
  if (key === "board.json") {
    let data;
    try {
      data = JSON.parse(body);
    } catch {
      return reply("body is not JSON", 400);
    }
    if (!Array.isArray(data?.jobs)) return reply("body needs a jobs array", 400);
    const stored = data.jobs.length ? await storedBoard(env) : { etag: null, jobs: [] };
    const bad =
      (await checkIdeaImages(env, data.jobs, stored.jobs)) ??
      (stored.etag === cond.get("If-Match") ? checkRowIds(stored.jobs, data.jobs) : null);
    if (bad) return reply(bad, 400);
  }
  const obj = await env.BUCKET.put(key, body, {
    onlyIf: cond,
    httpMetadata: { contentType: type },
  });
  if (obj) await announce(env, key);
  return obj
    ? reply("ok", 200, "text/plain", { ETag: obj.httpEtag })
    : reply("precondition failed", 412);
}

// The R2 onlyIf for exactly one well-formed If-Match or If-None-Match: *, or the error reply.
function strictCondition(request, label) {
  const match = request.headers.get("If-Match"),
    none = request.headers.get("If-None-Match");
  if (match === null && none === null) return needsCondition();
  const cond = new Headers();
  if (none === null && /^(W\/)?"[\x21\x23-\x7e\x80-\xff]+"$/.test(match || ""))
    cond.set("If-Match", match.replace(/^W\//, ""));
  else if (match === null && none === "*") cond.set("If-None-Match", "*");
  else return reply(`invalid ${label} write condition`, 400);
  return cond;
}

async function projectPut(request, env, key) {
  const cond = strictCondition(request, "project");
  if (cond instanceof Response) return cond;
  if (
    !/^text\/markdown(?:\s*;\s*charset\s*=\s*(?:utf-8|"utf-8"))?$/i.test(
      request.headers.get("Content-Type") || "",
    )
  )
    return reply("project document needs text/markdown", 415);
  const bytes = await readCapped(request, MAX_BODY);
  if (bytes === null) return reply("body over 1 MiB", 413);
  let body;
  try {
    body = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return reply("project document must be valid UTF-8", 400);
  }
  const error =
    validateProjectDocument(body) ?? checkOperatorText(body, "project document", "document");
  if (error) return reply(error, 400);
  const obj = await env.BUCKET.put(key, bytes, {
    onlyIf: cond,
    httpMetadata: { contentType: "text/markdown; charset=utf-8" },
  });
  return obj
    ? reply("ok", 200, "text/plain", { ETag: obj.httpEtag })
    : reply("precondition failed", 412);
}

function validateProjectMarkdown(text) {
  // oxlint-disable-next-line no-control-regex -- this pattern exists to reject control characters.
  return /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/u.test(text)
    ? "document contains control characters"
    : null;
}

async function projectDocumentList(env, slug) {
  const out = await Promise.all(
    PROJECT_DOCUMENT_NAMES.map(async (name) => ({
      name,
      exists: !!(await env.BUCKET.head(`project-documents/${slug}/${name}`)),
    })),
  );
  return reply(JSON.stringify(out), 200, "application/json");
}

async function projectDocumentPut(request, env, key) {
  const cond = strictCondition(request, "document");
  if (cond instanceof Response) return cond;
  if (
    !/^text\/markdown(?:\s*;\s*charset\s*=\s*(?:utf-8|"utf-8"))?$/i.test(
      request.headers.get("Content-Type") || "",
    )
  )
    return reply("document needs text/markdown", 415);
  const bytes = await readCapped(request, MAX_BODY);
  if (bytes === null) return reply("body over 1 MiB", 413);
  let body;
  try {
    body = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return reply("document must be valid UTF-8", 400);
  }
  const error =
    validateProjectMarkdown(body) ?? checkOperatorText(body, "project document", "document");
  if (error) return reply(error, 400);
  const obj = await env.BUCKET.put(key, bytes, {
    onlyIf: cond,
    httpMetadata: { contentType: "text/markdown; charset=utf-8" },
  });
  return obj
    ? reply("ok", 200, "text/plain", { ETag: obj.httpEtag })
    : reply("precondition failed", 412);
}

const pipelineKey = (scope) => `pipeline/${scope}.json`;
const pipelineHistoryKey = (scope, version) =>
  `pipeline-history/${scope}/${String(version).padStart(6, "0")}.json`;
const pipelineVersion = (obj) => Number.parseInt(obj?.customMetadata?.version, 10) || 0;
const sha256 = async (bytes) =>
  Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (x) =>
    x.toString(16).padStart(2, "0"),
  ).join("");

function pipelineReply(obj, version) {
  return reply(obj.body, 200, "application/json", {
    ETag: obj.httpEtag,
    "X-Pipeline-Version": String(version),
  });
}

// Only a signed-in person saves, against the exact version they read. Every saved version
// is also kept create-only under pipeline-history/, so nothing is ever lost or rewritten.
async function pipelinePut(request, env, principal, scope) {
  if (principal.kind !== "user")
    return reply("pipeline configuration is saved only by a signed-in person", 403);
  const cond = strictCondition(request, "pipeline");
  if (cond instanceof Response) return cond;
  if (
    !/^application\/json(?:\s*;\s*charset\s*=\s*(?:utf-8|"utf-8"))?$/i.test(
      request.headers.get("Content-Type") || "",
    )
  )
    return reply("pipeline configuration needs application/json", 415);
  const bytes = await readCapped(request, MAX_PIPELINE);
  if (bytes === null) return reply("body over 16 KiB", 413);
  let text;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return reply("pipeline configuration must be valid UTF-8", 400);
  }
  let doc;
  try {
    doc = JSON.parse(text);
  } catch {
    return reply("body is not JSON", 400);
  }
  const error = validatePipeline(doc);
  if (error) return reply(error, 400);
  const key = pipelineKey(scope);
  const current = await env.BUCKET.head(key);
  if (cond.has("If-Match") ? current?.httpEtag !== cond.get("If-Match") : current)
    return reply("precondition failed", 412);
  const cur = pipelineVersion(current);
  // Heal a history copy an earlier save failed to write, before this save replaces it.
  if (cur > 0 && !(await env.BUCKET.head(pipelineHistoryKey(scope, cur)))) {
    const old = await env.BUCKET.get(key);
    if (old) {
      await env.BUCKET.put(
        pipelineHistoryKey(scope, pipelineVersion(old)),
        await old.arrayBuffer(),
        {
          onlyIf: new Headers({ "If-None-Match": "*" }),
          httpMetadata: old.httpMetadata,
          customMetadata: old.customMetadata,
        },
      );
    }
  }
  const options = {
    httpMetadata: { contentType: "application/json" },
    customMetadata: {
      version: String(cur + 1),
      saved_by: principal.email,
      saved_at: new Date().toISOString(),
      sha256: await sha256(bytes),
    },
  };
  const obj = await env.BUCKET.put(key, bytes, { ...options, onlyIf: cond });
  if (!obj) return reply("precondition failed", 412);
  let missing = false;
  try {
    await env.BUCKET.put(pipelineHistoryKey(scope, cur + 1), bytes, {
      ...options,
      onlyIf: new Headers({ "If-None-Match": "*" }),
    });
  } catch {
    // The next save heals the missing copy. Log only the key, never the body.
    console.warn(`pipeline history copy failed: ${pipelineHistoryKey(scope, cur + 1)}`);
    missing = true;
  }
  return reply("ok", 200, "text/plain", {
    ETag: obj.httpEtag,
    "X-Pipeline-Version": String(cur + 1),
    ...(missing && { "X-Pipeline-History": "missing" }),
  });
}

async function pipelineGet(env, scope) {
  const obj = await env.BUCKET.get(pipelineKey(scope));
  return obj ? pipelineReply(obj, pipelineVersion(obj)) : notFound();
}

// Every version, newest first: the history copies plus the current object, whose own
// history copy may still be missing.
async function pipelineHistory(env, scope, principal) {
  const versions = new Map();
  const add = (obj, copy) => {
    const { version, saved_by, saved_at, sha256 } = obj.customMetadata;
    const entry = { version: Number(version), saved_by, saved_at, sha256, history_copy: copy };
    if (principal.kind !== "user") delete entry.saved_by;
    versions.set(entry.version, entry);
  };
  let cursor;
  do {
    const page = await env.BUCKET.list({
      prefix: `pipeline-history/${scope}/`,
      include: ["customMetadata"],
      cursor,
    });
    for (const obj of page.objects) add(obj, true);
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  const current = await env.BUCKET.head(pipelineKey(scope));
  if (current && !versions.has(pipelineVersion(current))) add(current, false);
  return reply(
    JSON.stringify([...versions.values()].sort((a, b) => b.version - a.version)),
    200,
    "application/json",
  );
}

async function pipelineHistoryVersion(env, scope, version) {
  const obj = await env.BUCKET.get(pipelineHistoryKey(scope, version));
  if (obj) return pipelineReply(obj, version);
  const current = await env.BUCKET.get(pipelineKey(scope));
  return current && pipelineVersion(current) === version
    ? pipelineReply(current, version)
    : notFound();
}

const PIPELINE_INVALID =
  "stored pipeline configuration no longer validates; save it again from the dashboard";

// [global, globalVersion, project, projectVersion], or null when a stored document no longer
// validates. That is a 409, never a silent default: dropping it could loosen a "human" merge.
async function pipelineDocs(env, slug) {
  const docs = [];
  for (const scope of ["global", `projects/${slug}`]) {
    const obj = await env.BUCKET.get(pipelineKey(scope));
    let doc = null;
    if (obj) {
      try {
        doc = JSON.parse(await obj.text());
      } catch {
        doc = undefined;
      }
      if (validatePipeline(doc)) return null;
    }
    docs.push(doc, pipelineVersion(obj));
  }
  return docs;
}

async function pipelineEffective(env, slug) {
  const docs = await pipelineDocs(env, slug);
  if (!docs) return reply(PIPELINE_INVALID, 409);
  // Once any host is registered, pickup reaches only active ones.
  const records = await hostRecords(env);
  const active = records.filter((r) => r.host.state === "active").map((r) => r.host);
  const view = effectivePipeline(
    slug,
    ...docs,
    records.length ? new Set(active.map((host) => host.id)) : null,
  );
  view.active_hosts = active.map(({ id, label }) => ({ id, label }));
  return reply(JSON.stringify(view), 200, "application/json");
}

const BACKLOG_ITEM = /^([\w-]+): (\S.*)$/;
const BAD_BACKLOG_ITEM = 'item: must be one "<slug>: <feature>" line of at most 2000 characters';
const backlogItemMatch = (item) =>
  typeof item === "string" && [...item].length <= 2000 && BACKLOG_ITEM.exec(item);
const triageKey = async (item) =>
  `backlog-triage/${await sha256(new TextEncoder().encode(item))}.json`;

async function backlogTriageGet(request, env) {
  const item = new URL(request.url).searchParams.get("item");
  if (!backlogItemMatch(item)) return reply(BAD_BACKLOG_ITEM, 400);
  return getFile(env, await triageKey(item), "application/json");
}

// Rates one new backlog line against its project's triage rules. The Worker compares the
// score with the threshold, so a device cannot move the bar.
async function backlogTriage(request, env) {
  const body = await readJson(request, MAX_TRIAGE);
  if (body instanceof Response) return body;
  if (typeof body !== "object" || body === null || Array.isArray(body))
    return reply("body: must be a JSON object", 400);
  for (const key of Object.keys(body))
    if (key !== "item" && key !== "context") return reply(`${key}: unknown field`, 400);
  const { item, context } = body;
  const m = backlogItemMatch(item);
  if (!m) return reply(BAD_BACKLOG_ITEM, 400);
  if (context !== undefined && typeof context !== "string")
    return reply("context: must be text", 400);
  if (context !== undefined && [...context].length > 6000)
    return reply("context: over 6000 characters", 400);
  for (const [field, text] of [
    ["item", item],
    ["context", context],
  ]) {
    if (text === undefined) continue;
    const error = checkOperatorText(text, field, "document");
    if (error) return reply(error, 400);
    if (hasReservedTag(text, "backlog-item"))
      return reply(`${field}: contains the reserved backlog-item tag`, 400);
  }
  const slug = m[1];
  if (!(await env.BUCKET.head(`projects/${slug}.md`))) return notFound();
  const docs = await pipelineDocs(env, slug);
  if (!docs) return reply(PIPELINE_INVALID, 409);
  const [, globalVersion, project, projectVersion] = docs;
  const { triage } = effectivePipeline(slug, ...docs);
  const json = (obj) => reply(JSON.stringify(obj), 200, "application/json");
  if (!triage.enabled) return json({ decision: "off" });
  const { threshold } = triage;
  const finish = async (verdict) => {
    const key = await triageKey(item);
    if (verdict.decision === "hold")
      await env.BUCKET.put(
        key,
        JSON.stringify({ item, ...verdict, at: new Date().toISOString() }),
        JSON_TYPE,
      );
    else await env.BUCKET.delete(key);
    return json(verdict);
  };
  const lines = (context ?? "").split(/\r\n|\r|\n/);
  const fixed =
    lines[0] === "Recommend: deny"
      ? "its author recommends deny"
      : lines.some((line) => line.startsWith("## Question:"))
        ? "it asks questions a person must answer"
        : null;
  if (fixed)
    return finish({ decision: "hold", confidence: null, threshold, reason: fixed, model: null });
  if (!env.AI)
    return reply("backlog triage needs the Workers AI binding; deploy the current Worker", 503);
  const source = project?.triage
    ? `pipeline:${slug}@${projectVersion}`
    : `pipeline:global@${globalVersion}`;
  let result;
  try {
    result = await env.AI.run(TRIAGE_MODEL, {
      messages: triageMessages(slug, source, triage.rules, item, context),
      response_format: { type: "json_schema", json_schema: TRIAGE_SCHEMA },
      max_tokens: 256,
      temperature: 0,
    });
  } catch {
    return reply("the triage model failed", 502);
  }
  const verdict = parseTriageReply(result);
  if (!verdict) return reply("the triage model gave an unusable reply", 502);
  return finish({
    decision: verdict.confidence >= threshold ? "queue" : "hold",
    confidence: verdict.confidence,
    threshold,
    reason: verdict.reason,
    model: TRIAGE_MODEL,
  });
}

const hostKey = (id) => `hosts/${id}.json`;
const hostStatusKey = (id) => `host-status/${id}.json`;
const hostPairingKey = (id) => `host-pairing/${id}.json`;
const hostHistoryKey = (id, version) =>
  `host-history/${id}/${String(version).padStart(6, "0")}.json`;
const JSON_TYPE = { httpMetadata: { contentType: "application/json" } };
const PAIRING_MINUTES = 15;
const STATUS_REFRESH_MINUTES = 10;
const PERSON_ONLY = "hosts are managed only by a signed-in person";
const PAIRING_FAILED = "pairing code is wrong, used or expired";
const TOKEN_TAKEN = "this service token is already bound to another host";
const STATUS_ONLY = "status is written only by the host's bound service token";

// A JSON body under the cap, or the error reply.
async function readJson(request, max = MAX_PIPELINE) {
  if (
    !/^application\/json(?:\s*;\s*charset\s*=\s*(?:utf-8|"utf-8"))?$/i.test(
      request.headers.get("Content-Type") || "",
    )
  )
    return reply("body needs application/json", 415);
  const bytes = await readCapped(request, max);
  if (bytes === null) return reply(`body over ${max >> 10} KiB`, 413);
  try {
    return JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes));
  } catch {
    return reply("body is not JSON", 400);
  }
}

// Every host record with its R2 object, in key order.
async function hostRecords(env) {
  const out = [];
  for (const { key } of (await env.BUCKET.list({ prefix: "hosts/" })).objects) {
    const obj = await env.BUCKET.get(key);
    if (obj) out.push({ obj, host: JSON.parse(await obj.text()) });
  }
  return out;
}

const isOffline = (status) =>
  !status || Date.now() - Date.parse(status.seen) > OFFLINE_MINUTES * 60_000;

// Only the code's hash is stored. Writing over the old record voids the old code.
async function issuePairingCode(env, id) {
  const code = newPairingCode();
  const expires_at = new Date(Date.now() + PAIRING_MINUTES * 60_000).toISOString();
  const hash = await sha256(new TextEncoder().encode(code.replace("-", "")));
  await env.BUCKET.put(hostPairingKey(id), JSON.stringify({ sha256: hash, expires_at }), JSON_TYPE);
  return { code, expires_at };
}

// A person's write to a host record, plus a create-only history copy of that version with who
// saved it and when. The saved object and reply headers, or null when the conditional write lost.
async function hostSave(env, principal, host, onlyIf) {
  const bytes = JSON.stringify(host);
  const saved = await env.BUCKET.put(hostKey(host.id), bytes, { ...JSON_TYPE, onlyIf });
  if (!saved) return null;
  let missing = false;
  try {
    await env.BUCKET.put(hostHistoryKey(host.id, host.version), bytes, {
      ...JSON_TYPE,
      onlyIf: new Headers({ "If-None-Match": "*" }),
      customMetadata: {
        version: String(host.version),
        saved_by: principal.email,
        saved_at: new Date().toISOString(),
      },
    });
  } catch {
    missing = true;
  }
  return { ETag: saved.httpEtag, ...(missing && { "X-Host-History": "missing" }) };
}

// A host as replies show it: never the bound client id or the creator's email.
function hostView({ token, ...fields }) {
  delete fields.created_by;
  return { ...fields, token_tail: token ? token.slice(-4) : null };
}

async function hostsList(env) {
  const out = [];
  for (const { obj, host } of await hostRecords(env)) {
    const statusObj = await env.BUCKET.get(hostStatusKey(host.id));
    const status = statusObj ? JSON.parse(await statusObj.text()) : null;
    out.push({
      ...hostView(host),
      etag: obj.httpEtag,
      status,
      offline: isOffline(status),
    });
  }
  out.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return reply(JSON.stringify(out), 200, "application/json");
}

async function hostCreate(request, env, principal) {
  if (principal.kind !== "user") return reply(PERSON_ONLY, 403);
  const body = await readJson(request);
  if (body instanceof Response) return body;
  const error = validateHostCreate(body);
  if (error) return reply(error, 400);
  const host = {
    schema: 1,
    id: body.id,
    label: body.label,
    state: "pending",
    max_inflight: body.max_inflight ?? 1,
    token: null,
    created_at: new Date().toISOString(),
    created_by: principal.email,
    paired_at: null,
    version: 1,
  };
  const headers = await hostSave(env, principal, host, new Headers({ "If-None-Match": "*" }));
  if (!headers) return reply("host already exists", 409);
  const { code, expires_at } = await issuePairingCode(env, host.id);
  return reply(
    JSON.stringify({ host: hostView(host), code, expires_at }),
    201,
    "application/json",
    headers,
  );
}

async function hostPatch(request, env, principal, id) {
  if (principal.kind !== "user") return reply(PERSON_ONLY, 403);
  const cond = strictCondition(request, "host");
  if (cond instanceof Response) return cond;
  const patch = await readJson(request);
  if (patch instanceof Response) return patch;
  const obj = await env.BUCKET.get(hostKey(id));
  if (!obj) return notFound();
  const host = JSON.parse(await obj.text());
  const error = validateHostPatch(host, patch);
  if (error) return reply(error, 400);
  const next = { ...host, ...patch, version: host.version + 1 };
  // The code goes first, so a retire never deletes one issued after it.
  if (patch.state === "retired") {
    next.token = null;
    await env.BUCKET.delete(hostPairingKey(id));
  }
  const headers = await hostSave(env, principal, next, cond);
  if (!headers) return reply("precondition failed", 412);
  return reply(JSON.stringify(hostView(next)), 200, "application/json", headers);
}

// The JSON body is ignored. Requiring it stops a cross-site form from voiding a live code.
async function hostCode(request, env, principal, id) {
  if (principal.kind !== "user") return reply(PERSON_ONLY, 403);
  const body = await readJson(request);
  if (body instanceof Response) return body;
  const obj = await env.BUCKET.get(hostKey(id));
  if (!obj) return notFound();
  const host = JSON.parse(await obj.text());
  if (host.state !== "pending" && host.state !== "retired")
    return reply(`host is ${host.state}`, 409);
  const headers = await hostSave(
    env,
    principal,
    { ...host, state: "pending", token: null, version: host.version + 1 },
    new Headers({ "If-Match": obj.httpEtag }),
  );
  if (!headers) return reply("precondition failed", 412);
  return reply(JSON.stringify(await issuePairingCode(env, id)), 201, "application/json", headers);
}

// Every saved version of one host, newest first. Only a person reads who saved each one.
async function hostHistory(env, principal, id) {
  if (principal.kind !== "user") return reply(PERSON_ONLY, 403);
  const out = [];
  let cursor;
  do {
    const page = await env.BUCKET.list({ prefix: `host-history/${id}/`, cursor });
    for (const { key } of page.objects) {
      const obj = await env.BUCKET.get(key);
      if (!obj) continue;
      const { version, saved_by, saved_at } = obj.customMetadata;
      out.push({
        version: Number(version),
        saved_by,
        saved_at,
        host: hostView(JSON.parse(await obj.text())),
      });
    }
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  out.sort((a, b) => b.version - a.version);
  return reply(JSON.stringify(out), 200, "application/json");
}

// The pairing record is deleted before anything is checked, so every attempt uses the code up.
async function hostPair(request, env, principal, id) {
  if (principal.kind !== "service" || !principal.client_id)
    return reply("hosts are paired only by a service token", 403);
  const body = await readJson(request);
  const obj = await env.BUCKET.get(hostKey(id));
  const pairingObj = await env.BUCKET.get(hostPairingKey(id));
  await env.BUCKET.delete(hostPairingKey(id));
  const host = obj && JSON.parse(await obj.text());
  const pairing = pairingObj && JSON.parse(await pairingObj.text());
  const code =
    !(body instanceof Response) && typeof body?.code === "string"
      ? body.code.toUpperCase().replace("-", "")
      : null;
  if (
    host?.state !== "pending" ||
    !pairing ||
    !(Date.parse(pairing.expires_at) > Date.now()) ||
    code === null ||
    (await sha256(new TextEncoder().encode(code))) !== pairing.sha256
  )
    return reply(PAIRING_FAILED, 403);
  const boundElsewhere = async () =>
    (await hostRecords(env)).some(
      (r) => r.host.id !== id && r.host.state !== "retired" && r.host.token === principal.client_id,
    );
  if (await boundElsewhere()) return reply(TOKEN_TAKEN, 409);
  const saved = await env.BUCKET.put(
    hostKey(id),
    JSON.stringify({
      ...host,
      state: "paired",
      token: principal.client_id,
      paired_at: new Date().toISOString(),
    }),
    { ...JSON_TYPE, onlyIf: new Headers({ "If-Match": obj.httpEtag }) },
  );
  if (!saved) return reply(PAIRING_FAILED, 403);
  // Two pairs on different hosts can both pass the check above. Each checks again after its own
  // write and backs out if another host holds the token, so at most one keeps it.
  if (await boundElsewhere()) {
    await env.BUCKET.put(hostKey(id), JSON.stringify(host), {
      ...JSON_TYPE,
      onlyIf: new Headers({ "If-Match": saved.httpEtag }),
    });
    return reply(TOKEN_TAKEN, 409);
  }
  return reply(JSON.stringify({ id, state: "paired" }), 200, "application/json");
}

// An unchanged status is rewritten at most every ten minutes, to keep `seen` fresh.
async function hostStatusPut(request, env, principal, id) {
  if (principal.kind !== "service" || !principal.client_id) return reply(STATUS_ONLY, 403);
  const obj = await env.BUCKET.get(hostKey(id));
  const host = obj && JSON.parse(await obj.text());
  if (host?.token !== principal.client_id || (host.state !== "paired" && host.state !== "active"))
    return reply(STATUS_ONLY, 403);
  const body = await readJson(request);
  if (body instanceof Response) return body;
  const error = validateHostStatus(body);
  if (error) return reply(error, 400);
  const storedObj = await env.BUCKET.get(hostStatusKey(id));
  if (storedObj) {
    const text = await storedObj.text();
    const { seen } = JSON.parse(text);
    if (
      Date.now() - Date.parse(seen) < STATUS_REFRESH_MINUTES * 60_000 &&
      JSON.stringify({ ...body, seen }) === text
    )
      return reply("ok");
  }
  await env.BUCKET.put(
    hostStatusKey(id),
    JSON.stringify({ ...body, seen: new Date().toISOString() }),
    JSON_TYPE,
  );
  return reply("ok");
}

// Raw image bytes, never decoded as text, or null past the cap.
const readImage = (request) => readCapped(request, MAX_IMAGE);

// Job files take an optional condition, so a device never overwrites a version it
// hasn't seen. Devices that send none still write blind. Images keep the type their
// extension names.
async function jobPut(request, env, key) {
  const cond = condition(request);
  if (cond === null) return needsCondition();
  const type = imageType(key);
  const body = type ? await readImage(request) : await readBody(request);
  if (body === null) return reply(type ? "image over 5 MiB" : "body over 1 MiB", 413);
  const obj = await env.BUCKET.put(key, body, {
    ...(cond && { onlyIf: cond }),
    ...(type && { httpMetadata: { contentType: type } }),
  });
  return obj
    ? reply("ok", 200, "text/plain", { ETag: obj.httpEtag })
    : reply("precondition failed", 412);
}

// The stored board's version and rows. An unreadable board has no rows.
async function storedBoard(env) {
  const object = await env.BUCKET.get("board.json");
  if (!object) return { etag: null, jobs: [] };
  try {
    const { jobs } = JSON.parse(await object.text());
    return { etag: object.httpEtag, jobs: Array.isArray(jobs) ? jobs : [] };
  } catch {
    return { etag: object.httpEtag, jobs: [] };
  }
}

// JSON-quoted id with every char outside printable ASCII as \uXXXX, safe to echo to a terminal.
const quoteId = (id) =>
  JSON.stringify(id).replace(
    /[^\x20-\x7e]/g,
    (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"),
  );

// An error message, or null. A row has no identity but its id, so a write that drops a
// stored id and adds a row in the same write is changing that row's id.
function checkRowIds(storedJobs, jobs) {
  const left = new Map();
  for (const j of storedJobs) left.set(j?.id, (left.get(j?.id) ?? 0) + 1);
  let added = false;
  for (const j of jobs) {
    const count = left.get(j?.id);
    if (count) left.set(j?.id, count - 1);
    else added = true;
  }
  if (!added) return null;
  for (const [id, count] of left)
    if (count && typeof id === "string")
      return `a board write cannot change a row's id: ${quoteId(id)} is gone and a new row appeared. Nothing was written. Upgrade the factory skill, or drop and re-add the row in two writes.`;
  return null;
}

// An error message, or null, for the idea_images and capture_id fields of a board write.
// Only references the stored board doesn't already carry are looked up: an image is
// immutable once uploaded, so one the stored board names was checked when it was saved.
async function checkIdeaImages(env, jobs, storedJobs) {
  const wanted = new Set();
  for (const j of jobs) {
    if (typeof j !== "object" || j === null) continue;
    if ("capture_id" in j && !UUID.test(j.capture_id)) return "capture_id must be a UUID";
    if (!("idea_images" in j)) continue;
    const names = j.idea_images;
    if (
      !Array.isArray(names) ||
      names.length < 1 ||
      names.length > MAX_IDEA_IMAGES ||
      new Set(names).size !== names.length ||
      !names.every((n) => IDEA_IMAGE.test(n))
    ) {
      return "idea_images needs 1-4 distinct <sha256>.png names";
    }
    names.forEach((n) => wanted.add(n));
  }
  if (!wanted.size) return null;
  try {
    for (const j of storedJobs) for (const n of j.idea_images ?? []) wanted.delete(n);
  } catch {} // an unreadable stored row vouches for nothing
  if (wanted.size > MAX_NEW_REFS) return "too many new idea images in one board write";
  for (const name of wanted) {
    const meta = (await env.BUCKET.head(`idea-images/${name}`))?.customMetadata;
    if (meta?.sha256 !== name.slice(0, 64)) return `idea image ${name} is not uploaded`;
  }
  return null;
}

// [status, message] for a body that isn't a plausible PNG, or [0, width, height]. This reads
// only the signature and the first IHDR chunk. The browser did the decoding.
function pngEnvelope(b) {
  if (b.length < 8 || PNG_SIGNATURE.some((v, i) => b[i] !== v)) return [415, "not a PNG"];
  if (b.length < 29) return [400, "truncated IHDR"];
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  if (v.getUint32(8) !== 13 || v.getUint32(12) !== 0x49484452)
    return [400, "first chunk must be a 13-byte IHDR"];
  const [w, h] = [v.getUint32(16), v.getUint32(20)];
  if (!w || !h || w > MAX_SIDE || h > MAX_SIDE || w * h > MAX_PIXELS)
    return [400, "image dimensions out of range"];
  return [0, w, h];
}

// Create-only, so a repeat upload of the same bytes succeeds without rewriting the object.
async function ideaImagePut(request, env, name) {
  if (request.headers.get("Content-Type") !== "image/png")
    return reply("Content-Type must be image/png", 415);
  const body = await readImage(request);
  if (body === null) return reply("image over 5 MiB", 413);
  const [status, w, h] = pngEnvelope(body);
  if (status) return reply(w, status);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", body));
  const hex = Array.from(digest, (x) => x.toString(16).padStart(2, "0")).join("");
  if (`${hex}.png` !== name) return reply("SHA-256 does not match the file name", 400);
  await env.BUCKET.put(`idea-images/${name}`, body, {
    onlyIf: new Headers({ "If-None-Match": "*" }),
    httpMetadata: { contentType: "image/png" },
    customMetadata: {
      sha256: hex,
      width: String(w),
      height: String(h),
      length: String(body.length),
    },
  });
  return reply("ok");
}

async function api(request, env, path, principal) {
  const method = request.method;
  const p = path.match(PIPELINE) || path.match(PIPELINE_HISTORY) || path.match(PIPELINE_EFFECTIVE);
  if (p) {
    const history = PIPELINE_HISTORY.test(path),
      effective = PIPELINE_EFFECTIVE.test(path);
    const slug = effective ? p[1] : p[2];
    if (!(method === "GET" || (method === "PUT" && !history && !effective))) return notFound();
    if (slug !== undefined && !(await env.BUCKET.head(`projects/${slug}.md`))) return notFound();
    if (effective) return pipelineEffective(env, slug);
    if (history)
      return p[3] === undefined
        ? pipelineHistory(env, p[1], principal)
        : pipelineHistoryVersion(env, p[1], Number(p[3]));
    return method === "GET" ? pipelineGet(env, p[1]) : pipelinePut(request, env, principal, p[1]);
  }
  let m = path.match(HOSTS);
  if (m) {
    const [, id, action] = m;
    if (id === undefined) {
      if (method === "GET") return hostsList(env);
      if (method === "POST") return hostCreate(request, env, principal);
    } else if (HOST_NAME.test(id)) {
      if (action === undefined && method === "PATCH") return hostPatch(request, env, principal, id);
      if (action === "code" && method === "POST") return hostCode(request, env, principal, id);
      if (action === "pair" && method === "POST") return hostPair(request, env, principal, id);
      if (action === "history" && method === "GET") return hostHistory(env, principal, id);
      if (action === "status" && method === "PUT")
        return hostStatusPut(request, env, principal, id);
    }
    return notFound();
  }
  m = path.match(IDEA_IMAGE_ROUTE);
  if (m) {
    if (!IDEA_IMAGE.test(m[1])) return notFound();
    if (method === "GET") return getFile(env, `idea-images/${m[1]}`, "image/png", IMAGE_HEADERS);
    if (method === "PUT") return ideaImagePut(request, env, m[1]);
    return notFound();
  }
  if (path === "/api/backlog/triage" && method === "POST") return backlogTriage(request, env);
  if (path === "/api/backlog/triage" && method === "GET") return backlogTriageGet(request, env);
  if (path === "/api/board" || path === "/api/backlog") {
    const [key, type] =
      path === "/api/board" ? ["board.json", "application/json"] : ["backlog.md", "text/markdown"];
    if (method === "GET") return getFile(env, key, type);
    if (method === "PUT") return casPut(request, env, key, type);
    return notFound();
  }
  m = path.match(PROJECT_DOCUMENTS) || path.match(PROJECT_DOCUMENT);
  if (m) {
    if (
      m[2] === undefined
        ? method !== "GET"
        : !PROJECT_DOCUMENT_NAMES.includes(m[2]) || !["GET", "PUT"].includes(method)
    )
      return notFound();
    if (!(await env.BUCKET.head(`projects/${m[1]}.md`))) return notFound();
    if (m[2] === undefined) return projectDocumentList(env, m[1]);
    const key = `project-documents/${m[1]}/${m[2]}`;
    return method === "GET"
      ? getFile(env, key, "text/markdown")
      : projectDocumentPut(request, env, key);
  }
  m = path.match(PROJECT);
  if (m) {
    const key = `projects/${m[1]}.md`;
    if (method === "GET") return getFile(env, key, "text/markdown");
    if (method === "PUT") return projectPut(request, env, key);
    return notFound();
  }
  m = path.match(JOB_DIR);
  if (m) {
    const prefix = `jobs/${m[1]}/${m[2]}/`;
    if (m[3] === "" && method === "GET") {
      const { objects } = await env.BUCKET.list({ prefix });
      const name = (o) => o.key.slice(prefix.length);
      // ?etags=1 adds each file's version for factory.py's sync manifest. Older devices get names only.
      const list = new URL(request.url).searchParams.has("etags")
        ? objects.map((o) => ({ name: name(o), etag: o.httpEtag }))
        : objects.map(name);
      return reply(JSON.stringify(list), 200, "application/json");
    }
    if (!JOB_REL.test(m[3])) return notFound();
    if (method === "GET") return getJobFile(request, env, principal, prefix + m[3]);
    if (method === "PUT") return jobPut(request, env, prefix + m[3]);
  }
  return notFound();
}

export default {
  async fetch(request, env) {
    let config;
    try {
      config = readServerConfig(env);
    } catch {
      return reply("factory unavailable", 503);
    }
    if (new URL(request.url).origin !== config.origin) return reply("forbidden", 403);
    let principal;
    try {
      principal = await authenticate(request, config);
    } catch {
      return reply("forbidden", 403);
    }
    return serveFactory(request, { ...env, config }, principal);
  },
};

/** @param {Request} request
 * @param {object} env
 * @param {import('./access.ts').VerifiedPrincipal} principal
 */
export async function serveFactory(request, env, principal) {
  if (principal?.verified !== true) return reply("forbidden", 403);
  const path = new URL(request.url).pathname;
  if (path.includes("..")) return reply("bad path", 400);
  if (path === EVENTS_PATH) {
    // Only a person's browser upgrades here; announce() reaches the room directly.
    if (
      request.method !== "GET" ||
      request.headers.get("Upgrade")?.toLowerCase() !== "websocket" ||
      principal.kind !== "user" ||
      !env.BoardEvents
    )
      return notFound();
    if (request.headers.get("Origin") !== new URL(request.url).origin)
      return reply("forbidden", 403);
    return connectEvents(request, env);
  }
  if (path.startsWith("/api/")) return api(request, env, path, principal);
  if (request.method !== "GET") return notFound();
  if (path === "/") return renderDashboard(env.config);
  if (path === "/dashboard.html")
    return reply(null, 302, "text/plain", { Location: "/" + new URL(request.url).search });
  if (path.startsWith("/assets/") || path === "/sw.js") return serveAsset(request, env.config);
  if (path === "/board.json") return getFile(env, "board.json", "application/json");
  if (path === "/projects.json") return projects(env);
  if (JOB_FILE.test(path)) return getJobFile(request, env, principal, path.slice(1));
  return notFound();
}
