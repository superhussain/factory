// The global factory dashboard and device API, served from one R2 bucket.
// Cloudflare Access guards every path. Devices authenticate with a service token.
import { authenticate } from "./access.ts";
import {
  checkOperatorText,
  effectivePipeline,
  RETRO_STAGES,
  hasReservedTag,
  HOST_NAME,
  validatePipeline,
} from "./admission.js";
import {
  ARCHIVE_KEY,
  archiveGet,
  needsTriageKey,
  runArchive,
  slimBoard,
  terminal,
} from "./archive.js";
import {
  DASHBOARD_VERSION,
  escapeHtml,
  readServerConfig,
  renderDashboard,
  serveAsset,
} from "./assets.js";
import {
  cleanReview,
  ESCALATION_MODEL,
  ESCALATION_REASON_BUILDER,
  ESCALATION_REASON_PERSON,
  ESCALATION_TEXT_LIMIT,
  ESCALATION_THRESHOLD,
  escalationConfidence,
  escalationInput,
  REVIEWERS,
} from "./escalation.js";
import { announce, connectEvents, EVENTS_PATH } from "./events.js";
import {
  boardPushes,
  pushKey,
  sendPushes,
  stalePushes,
  stuckPushes,
  validSubscription,
} from "./push.js";
import {
  newPairingCode,
  OFFLINE_MINUTES,
  validateHostCreate,
  validateHostPatch,
  validateHostStatus,
} from "./hosts.js";
import {
  TRIAGE_MODEL,
  TRIAGE_REASON,
  TRIAGE_REASON_NO_RULES,
  backlogQuestions,
  duplicateCandidates,
  duplicateInput,
  duplicatePick,
  needsInput,
  needsQuestions,
  triageBatchConfidence,
  triageBatchInput,
  triageChoices,
  triageConfidence,
  triageInput,
} from "./triage.js";
import { spendGet, spendNudgePass } from "./spend.js";

export { BoardEvents } from "./events.js";

// A job id starts with a word character, so "." and ".." are never ids.
const JOB_FILE =
  /^\/jobs\/[\w-]+\/\w[\w.-]*\/((round-\d+\/)?[\w.-]+\.(md|png|jpe?g|gif|webp)|(round-\d+\/)?[\w-]+\.walkthrough\.html|walkthrough\.html)$/;
const JOB_DIR = /^\/api\/jobs\/([\w-]+)\/(\w[\w.-]*)\/(.*)$/;
const JOB_REL =
  /^((round-\d+\/)?[\w.-]+\.(md|png|jpe?g|gif|webp)|(round-\d+\/)?[\w-]+\.walkthrough\.html|walkthrough\.html)$/;
const NEEDS_KEY = /^jobs\/([\w-]+)\/(\w[\w.-]*)\/needs\.md$/;
const PROJECT = /^\/api\/projects\/([\w-]+)\.md$/;
const PROJECT_DOCUMENT_NAMES = ["notes.md", "decisions.md", "runbook.md"];
const PROJECT_DOCUMENTS = /^\/api\/project-documents\/([A-Za-z0-9_-]+)$/;
const PROJECT_DOCUMENT = /^\/api\/project-documents\/([A-Za-z0-9_-]+)\/([^/]+)$/;
// History of a project file (project.md) or document: the list, or one version's bytes.
const PROJECT_HISTORY = /^\/api\/project-history\/([\w-]+)\/([\w.-]+)(?:\/([1-9]\d*))?$/;
// Retro admit's find-and-replace write to a project file or document.
const PROJECT_RETRO = /^\/api\/project-retro\/([\w-]+)\/([\w.-]+)$/;
const PROJECT_FILE_NAMES = ["project.md", ...PROJECT_DOCUMENT_NAMES];
// The one route that clears a project's stage-note trial pause.
const TRIALS_RESUME = /^\/api\/trials\/resume\/([\w-]+)$/;
const REPO = /^[\w.-]+\/[\w.-]+$/;
const BASE = /^[A-Za-z0-9][A-Za-z0-9._/-]{0,199}$/;
// Pipeline scopes are "global" or "projects/<slug>"; history keeps every saved version.
const PIPELINE = /^\/api\/pipeline\/(global|projects\/([\w-]+))$/;
const PIPELINE_HISTORY = /^\/api\/pipeline\/history\/(global|projects\/([\w-]+))(?:\/([1-9]\d*))?$/;
const PIPELINE_EFFECTIVE = /^\/api\/pipeline\/effective\/([\w-]+)$/;
const MAX_PIPELINE = 16 << 10;
// 2000 + 6000 characters at up to 4 UTF-8 bytes each, plus JSON keys and escapes.
const MAX_TRIAGE = 64 << 10;
// Eight single triage bodies.
const MAX_TRIAGE_BATCH = 512 << 10;
// 50 items of 2000 characters at up to 4 UTF-8 bytes each, plus JSON quotes and escapes.
const MAX_TRIAGE_READ = 512 << 10;
// Four reviews of 8000 characters at up to 4 UTF-8 bytes each, plus JSON keys and escapes.
const MAX_ESCALATION = 160 << 10;
// /api/hosts, /api/hosts/<id> and /api/hosts/<id>/<action>.
const HOSTS = /^\/api\/hosts(?:\/([^/]+)(?:\/(code|pair|status|history))?)?$/;
const MAX_BODY = 1 << 20;
// A day's list of hosts with a retro digest, and one host's digest for that day.
const RETRO_DAY = /^\/api\/retro\/(\d{4}-\d{2}-\d{2})$/;
const RETRO_FILE = /^\/api\/retro\/([^/]+)\/(\d{4}-\d{2}-\d{2})\.jsonl$/;
// A job's audit rows from every host, and one host's rows for that job.
const AUDIT_JOB = /^\/api\/audit\/([^/]+)$/;
const AUDIT_FILE = /^\/api\/audit\/([^/]+)\/([^/]+)\.jsonl$/;
const AUDIT_JOB_ID = /^\d+-[a-z0-9]+(?:-[a-z0-9]+)*$/;
// The red-flag index of flagged calls across jobs.
const AUDIT_FLAGS = /^\/api\/audit-flags$/;
const AUDIT_FLAGS_ACK = /^\/api\/audit-flags\/ack$/;
const AUDIT_FLAGS_BACKLOG_PATH = /^\/api\/audit-flags\/backlog$/;
// Retro runs: one factory.retro-run/1 record per admitted run, and the newest 30 listed.
const RETROS = /^\/api\/retros(?:\/([A-Za-z0-9_-]{1,64}))?$/;
// board.json holds every job, merged ones included, so it outgrows the per-file cap.
const MAX_BOARD = 16 << 20;
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
// A browser checks frame-ancestors against every ancestor, so a walkthrough inside a dashboard
// that Paseo frames lists the same embed origins the dashboard does.
// no-transform stops Cloudflare's edge rewriting the HTML, which would drop the ETag a device pull needs.
const htmlHeaders = (embedOrigins) => ({
  "Content-Security-Policy": HTML_CSP + embedOrigins.map((origin) => " " + origin).join(""),
  "Referrer-Policy": "no-referrer",
  "Cache-Control": "no-store, no-transform",
});
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

// Attached idea files: content-addressed, and the extension alone picks the type.
const IDEA_FILE = /^[0-9a-f]{64}\.(pdf|csv|txt|mp4|webm)$/;
const IDEA_FILE_ROUTE = /^\/api\/idea-files\/([^/]+)$/;
const IDEA_FILE_TYPES = {
  pdf: { type: "application/pdf", max: 10 << 20, label: "PDF" },
  csv: { type: "text/csv", max: 1 << 20, label: "CSV" },
  txt: { type: "text/plain", max: 1 << 20, label: "text" },
  mp4: { type: "video/mp4", max: 25 << 20, label: "MP4" },
  webm: { type: "video/webm", max: 25 << 20, label: "WebM" },
};
const MAX_IDEA_FILES = 4;
// PDF, CSV and text download. The sandbox CSP and nosniff stop one running as a page.
const FILE_HEADERS = (name) => ({
  "Content-Disposition": `attachment; filename="${name}"`,
  "Content-Security-Policy": "default-src 'none'; sandbox",
});

// Text types get a charset. Image, video and PDF types are sent exactly as given.
function reply(body, status = 200, type = "text/plain", headers = {}) {
  return new Response(body, {
    status,
    headers: {
      "Content-Type":
        /^(image|video)\//.test(type) || type === "application/pdf"
          ? type
          : `${type}; charset=utf-8`,
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

const WEEKLY_TOKENS = /^[1-9][0-9]{0,11}$/;

function validateProjectDocument(text) {
  // oxlint-disable-next-line no-control-regex -- this pattern exists to reject control characters.
  if (/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/u.test(text))
    return "project document contains control characters";
  const lines = text.split(/\r?\n/);
  const end = lines.findIndex((line, i) => i > 0 && line === "---");
  if (lines[0] !== "---" || end < 0) return "project document needs closed --- frontmatter";
  const meta = new Map();
  const fieldsError =
    "project frontmatter allows one repo, base and title, plus an optional archive, buildkite_org, auto_babysit and weekly_tokens";
  for (const line of lines.slice(1, end)) {
    if (!line.trim() || line.trim().startsWith("#")) continue;
    const match =
      /^(repo|base|title|archive|buildkite_org|auto_babysit|weekly_tokens):\s*(.*)$/.exec(line);
    if (!match || meta.has(match[1])) return fieldsError;
    let value = match[2].trim();
    if (/^["']|["']$/.test(value)) {
      if (value.length < 2 || value[0] !== value.at(-1)) return fieldsError;
      value = value.slice(1, -1);
    } else if (/^[[{!&*|>]/.test(value)) return fieldsError;
    meta.set(match[1], value);
  }
  if (
    meta.size !==
    3 +
      (meta.has("archive") ? 1 : 0) +
      (meta.has("buildkite_org") ? 1 : 0) +
      (meta.has("auto_babysit") ? 1 : 0) +
      (meta.has("weekly_tokens") ? 1 : 0)
  )
    return fieldsError;
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
  if (meta.has("buildkite_org") && !/^[A-Za-z0-9-]{1,100}$/.test(meta.get("buildkite_org")))
    return "invalid project buildkite_org";
  if (meta.has("auto_babysit") && !/^[1-9]$/.test(meta.get("auto_babysit")))
    return "invalid project auto_babysit";
  if (meta.has("weekly_tokens") && !WEEKLY_TOKENS.test(meta.get("weekly_tokens")))
    return "invalid project weekly_tokens";
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

async function listProjects(env) {
  const out = [];
  const { objects } = await env.BUCKET.list({ prefix: "projects/" });
  for (const { key } of objects) {
    const slug = key.slice("projects/".length, -".md".length);
    if (!key.endsWith(".md") || !/^[\w-]+$/.test(slug)) continue;
    const obj = await env.BUCKET.get(key);
    if (!obj) continue; // deleted between list and get
    out.push({ slug, meta: frontmatter(await obj.text()) });
  }
  return out;
}

async function projects(env) {
  const out = (await listProjects(env)).map(({ slug, meta }) => {
    const item = { slug, title: meta.title || slug };
    if (REPO.test(meta.repo || "")) item.repo = meta.repo;
    if (BASE.test(meta.base || "")) item.base = meta.base;
    if (REPO.test(meta.archive || "")) item.archive = meta.archive;
    if (/^[1-9]$/.test(meta.auto_babysit || "")) item.auto_babysit = Number(meta.auto_babysit);
    return item;
  });
  return reply(JSON.stringify(out), 200, "application/json");
}

async function weeklyBudgets(env) {
  const out = new Map();
  for (const { slug, meta } of await listProjects(env)) {
    if (WEEKLY_TOKENS.test(meta.weekly_tokens || "")) out.set(slug, Number(meta.weekly_tokens));
  }
  return out;
}

async function getFile(env, key, type, headers = {}) {
  const obj = await env.BUCKET.get(key);
  return obj ? reply(obj.body, 200, type, { ETag: obj.httpEtag, ...headers }) : notFound();
}

// The last slim body and the board ETag it came from, so this isolate slices each board version once.
let slimMemo = { etag: "", body: "" };
// The read in flight for one board ETag, so concurrent misses in this isolate parse the board once.
let slimPending = { etag: "", read: null };

// Resolves to plain strings only, so no R2 object crosses requests.
async function readSlim(env) {
  const obj = await env.BUCKET.get("board.json");
  if (!obj) return null;
  const text = await obj.text();
  let data = null;
  try {
    data = JSON.parse(text);
  } catch {}
  if (!Array.isArray(data?.jobs)) return { etag: obj.httpEtag, body: text };
  slimMemo = { etag: obj.httpEtag, body: JSON.stringify(slimBoard(data)) };
  return slimMemo;
}

// The dashboard's first read. A board that is not JSON with a jobs array goes out as stored.
async function slimBoardGet(env, ctx) {
  const head = await env.BUCKET.head("board.json");
  if (!head) return notFound();
  const send = (body, etag) =>
    reply(body, 200, "application/json", { ETag: etag, "X-Factory-Version": DASHBOARD_VERSION });
  if (slimMemo.etag === head.httpEtag) return send(slimMemo.body, head.httpEtag);
  if (slimPending.etag !== head.httpEtag) {
    const read = readSlim(env);
    const clear = () => {
      if (slimPending.read === read) slimPending = { etag: "", read: null };
    };
    // Keeps the read alive for the waiters if the request that started it is cancelled.
    const settled = read.then(clear, clear);
    if (ctx) ctx.waitUntil(settled);
    slimPending = { etag: head.httpEtag, read };
  }
  const out = await slimPending.read;
  return out ? send(out.body, out.etag) : notFound();
}

// MP4 and WebM play inline. Their names are content addresses, so a range never goes stale.
const VIDEO_HEADERS = (name) => ({
  "Content-Disposition": `inline; filename="${name}"`,
  "Content-Security-Policy": "default-src 'none'; sandbox",
  "Accept-Ranges": "bytes",
});

// One byte range: null sends the whole body, false is a 416, else { offset, length }.
function byteRange(header, size) {
  const m = /^bytes=(\d*)-(\d*)$/.exec(header || "");
  if (!m || (m[1] === "" && m[2] === "")) return null;
  if (m[1] === "") {
    const suffix = Number(m[2]);
    if (suffix === 0) return false;
    const offset = Math.max(0, size - suffix);
    return { offset, length: size - offset };
  }
  const offset = Number(m[1]);
  if (offset >= size) return false;
  const end = m[2] === "" ? size - 1 : Math.min(Number(m[2]), size - 1);
  return end < offset ? null : { offset, length: end - offset + 1 };
}

async function ideaVideoGet(request, env, name) {
  const key = `idea-files/${name}`;
  const type = IDEA_FILE_TYPES[name.slice(65)].type;
  const head = await env.BUCKET.head(key);
  if (!head) return notFound();
  const range = byteRange(request.headers.get("Range"), head.size);
  if (range === false)
    return reply("range not satisfiable", 416, "text/plain", {
      "Content-Range": `bytes */${head.size}`,
      "Content-Security-Policy": "default-src 'none'; sandbox",
    });
  if (!range) return getFile(env, key, type, VIDEO_HEADERS(name));
  const obj = await env.BUCKET.get(key, { range });
  if (!obj) return notFound();
  return reply(obj.body, 206, type, {
    ETag: obj.httpEtag,
    ...VIDEO_HEADERS(name),
    "Content-Range": `bytes ${range.offset}-${range.offset + range.length - 1}/${head.size}`,
  });
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
      ? getFile(env, key, "text/html", htmlHeaders(env.config.embed_origins))
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

async function readBody(request, max = MAX_BODY) {
  const body = await readCapped(request, max);
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

// The first reason a board write of data over stored is refused, or null. The row checks
// compare against the stored rows, so they run only when data was built on that version,
// or creates the board when none is stored.
async function boardRefusal(env, stored, data, sameBase, principal) {
  return (
    checkGoals(data.jobs) ??
    (await checkIdeaImages(env, data.jobs, stored.jobs)) ??
    (await checkIdeaFiles(env, data.jobs, stored.jobs)) ??
    (sameBase
      ? (checkRowIds(stored.jobs, data.jobs) ??
        checkArchived(stored, data) ??
        checkNewIds(stored.jobs, data.jobs) ??
        checkRiderControls(stored.jobs, data.jobs) ??
        checkBatchRefs(stored.jobs, data.jobs) ??
        checkNewStarters(stored.jobs, data.jobs) ??
        (await checkApprovalNotes(env, stored.jobs, data.jobs, principal)) ??
        checkResolve(stored.jobs, data.jobs, principal) ??
        checkSteer(stored.jobs, data.jobs, principal) ??
        checkTrials(stored.doc, data))
      : null)
  );
}

// Sends the pushes a committed board write from storedJobs to jobs earns.
async function boardCommitted(env, storedJobs, jobs, ctx) {
  const events = boardPushes(storedJobs, jobs);
  if (!events.length) return;
  const send = sendPushes(env, events).catch(() => {});
  if (ctx) ctx.waitUntil(send);
  else await send;
}

const MAX_FRAME_ROWS = 100;
const MAX_FRAME_BYTES = 262144;

// The rows frame for a committed board write from stored to data at etag, or "" when
// open tabs must read instead: no prior version, a top-level change other than jobs and
// updated, ids that cannot be diffed, a finished row, or a frame too large.
function rowsFrame(stored, data, etag) {
  if (!stored.etag || !stored.doc) return "";
  for (const k of new Set([...Object.keys(stored.doc), ...Object.keys(data)]))
    if (
      k !== "jobs" &&
      k !== "updated" &&
      JSON.stringify(stored.doc[k]) !== JSON.stringify(data[k])
    )
      return "";
  const ids = (jobs) => {
    const byId = new Map();
    for (const j of jobs) {
      if (typeof j?.id !== "string" || byId.has(j.id)) return null;
      byId.set(j.id, j);
    }
    return byId;
  };
  const before = ids(stored.jobs),
    after = ids(data.jobs);
  if (!before || !after) return "";
  const rows = [],
    removed = [];
  for (const [id, row] of after) {
    const old = before.get(id);
    if (old === row || (old && JSON.stringify(old) === JSON.stringify(row))) continue;
    if (terminal(old) || terminal(row)) return "";
    rows.push(row);
  }
  for (const [id, old] of before) {
    if (after.has(id)) continue;
    if (terminal(old)) return "";
    removed.push(id);
  }
  if (rows.length + removed.length > MAX_FRAME_ROWS) return "";
  const frame = JSON.stringify({
    type: "rows",
    etag,
    prev: stored.etag,
    updated: data.updated,
    rows,
    removed,
  });
  return new TextEncoder().encode(frame).length > MAX_FRAME_BYTES ? "" : frame;
}

const needsCondition = () => reply('PUT needs If-Match: "<etag>" or If-None-Match: *', 428);

// Board and backlog writes must name the exact version they replace, or create the
// file only if it's absent, so no device clobbers another.
async function casPut(request, env, key, type, principal, ctx) {
  const cond = condition(request);
  if (!cond) return needsCondition();
  const body = await readBody(request, MAX_BOARD);
  if (body === null) return reply("body over 16 MiB", 413);
  let data, stored;
  if (key === "board.json") {
    try {
      data = JSON.parse(body);
    } catch {
      return reply("body is not JSON", 400);
    }
    if (!Array.isArray(data?.jobs)) return reply("body needs a jobs array", 400);
    stored = await storedBoard(env);
    // A create on an absent board is built on the empty board, so the row checks run.
    const builtOn = cond.has("If-Match")
      ? stored.etag === cond.get("If-Match")
      : stored.etag === null;
    const bad = await boardRefusal(env, stored, data, builtOn, principal);
    if (bad) return reply(bad, 400);
  }
  const obj = await env.BUCKET.put(key, body, {
    onlyIf: cond,
    httpMetadata: { contentType: type },
  });
  // The stored board is the true prior version only when the write named its ETag.
  const sameBase = obj && stored && cond.has("If-Match") && stored.etag === cond.get("If-Match");
  if (obj) await announce(env, key, sameBase ? rowsFrame(stored, data, obj.httpEtag) : "");
  if (sameBase) await boardCommitted(env, stored.jobs, data.jobs, ctx);
  return obj
    ? reply("ok", 200, "text/plain", { ETag: obj.httpEtag })
    : reply("precondition failed", 412);
}

// JSON.stringify with object keys sorted at every depth, so equal rows compare equal.
function canonicalJson(value) {
  return JSON.stringify(value, (_, v) =>
    v && typeof v === "object" && !Array.isArray(v)
      ? Object.fromEntries(
          Object.keys(v)
            .sort()
            .map((k) => [k, v[k]]),
        )
      : v,
  );
}

// Lowercase hex SHA-256 of the row's canonical JSON, the digest a patch's reads carry.
async function rowSha256(row) {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(canonicalJson(row)),
  );
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

const BOARD_PATCH_SHAPE =
  "a board patch needs rows: [{id, expect, row}], one entry per id. Nothing was written.";
const READS_SHAPE =
  "a board patch's reads need [{id, sha256}], one per id and none in rows. Nothing was written.";

// Applies the dashboard's changed rows, each with the row it expects to replace, to the
// stored board. A row that changed since the client read it comes back as a 409 conflict;
// an R2 race with another writer is retried here.
async function boardPatch(request, env, principal, ctx) {
  const body = await readBody(request);
  if (body === null) return reply("body over 1 MiB", 413);
  let patch;
  try {
    patch = JSON.parse(body);
  } catch {
    return reply("body is not JSON", 400);
  }
  const rows = patch?.rows;
  const rowOf = (v, id) =>
    v === null || (typeof v === "object" && !Array.isArray(v) && v.id === id);
  if (
    !Array.isArray(rows) ||
    rows.length < 1 ||
    rows.length > 100 ||
    !rows.every(
      (e) =>
        typeof e?.id === "string" &&
        rowOf(e.expect, e.id) &&
        rowOf(e.row, e.id) &&
        (e.expect !== null || e.row !== null),
    ) ||
    new Set(rows.map((e) => e.id)).size !== rows.length
  )
    return reply(BOARD_PATCH_SHAPE, 400);
  const entries = new Map(rows.map((e) => [e.id, e]));
  const reads = patch.reads ?? [];
  if (
    !Array.isArray(reads) ||
    !reads.every(
      (r) =>
        typeof r?.id === "string" &&
        typeof r.sha256 === "string" &&
        /^[0-9a-f]{64}$/.test(r.sha256) &&
        !entries.has(r.id),
    ) ||
    new Set(reads.map((r) => r.id)).size !== reads.length
  )
    return reply(READS_SHAPE, 400);
  for (let attempt = 0; attempt < 10; attempt++) {
    const stored = await storedBoard(env);
    if (stored.etag && !stored.doc)
      return reply("the stored board is not readable JSON. Nothing was written.", 400);
    const doc = stored.doc ?? { updated: "", jobs: [] };
    const byId = new Map(),
      captured = new Map();
    let maxId = Number.isInteger(stored.archived?.max_id) ? stored.archived.max_id : 0;
    for (const j of stored.jobs) {
      if (!byId.has(j?.id)) byId.set(j?.id, j);
      if (typeof j?.capture_id === "string" && !captured.has(j.capture_id))
        captured.set(j.capture_id, j.id);
      const m = typeof j?.id === "string" && /^(\d{3,})-/.exec(j.id);
      if (m) maxId = Math.max(maxId, Number(m[1]));
    }
    const conflicts = new Set();
    for (const e of rows) {
      if (canonicalJson(byId.get(e.id) ?? null) !== canonicalJson(e.expect)) conflicts.add(e.id);
      if (e.expect !== null) continue;
      const m = /^(\d{3,})-/.exec(e.id);
      if (m && Number(m[1]) <= maxId) conflicts.add(e.id);
      if (typeof e.row.capture_id === "string" && captured.has(e.row.capture_id))
        conflicts.add(captured.get(e.row.capture_id));
    }
    for (const r of reads) {
      const row = byId.get(r.id);
      if (!row || (await rowSha256(row)) !== r.sha256) conflicts.add(r.id);
    }
    if (conflicts.size)
      return reply(
        JSON.stringify({
          error: "conflict",
          rows: Object.fromEntries([...conflicts].map((id) => [id, byId.get(id) ?? null])),
          max_id: maxId,
        }),
        409,
        "application/json",
      );
    const jobs = [];
    for (const j of stored.jobs) {
      const e = entries.get(j?.id);
      if (!e) jobs.push(j);
      else if (e.row) jobs.push(e.row);
    }
    for (const e of rows) if (e.expect === null) jobs.push(e.row);
    const updated =
      typeof patch.updated === "string" && patch.updated > (doc.updated ?? "")
        ? patch.updated
        : doc.updated;
    const data = { ...doc, jobs, updated };
    const bad = await boardRefusal(env, stored, data, true, principal);
    if (bad) return reply(bad, 400);
    const text = JSON.stringify(data);
    if (new TextEncoder().encode(text).length > MAX_BOARD)
      return reply("board over 16 MiB. Nothing was written.", 413);
    const obj = await env.BUCKET.put("board.json", text, {
      onlyIf: new Headers(stored.etag ? { "If-Match": stored.etag } : { "If-None-Match": "*" }),
      httpMetadata: { contentType: "application/json" },
    });
    if (obj) {
      await announce(env, "board.json", rowsFrame(stored, data, obj.httpEtag));
      await boardCommitted(env, stored.jobs, data.jobs, ctx);
      return reply("ok", 200, "text/plain", { ETag: obj.httpEtag });
    }
    await new Promise((resolve) => setTimeout(resolve, 20 + Math.random() * 80));
  }
  return reply(JSON.stringify({ error: "busy" }), 409, "application/json");
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

const projectFileKey = (slug, name) =>
  name === "project.md" ? `projects/${slug}.md` : `project-documents/${slug}/${name}`;
const projectHistoryKey = (slug, name, n) =>
  `project-history/${slug}/${name}/${String(n).padStart(6, "0")}.md`;

// Saves a project file or document as its next version against the exact version the writer
// read, and keeps a create-only copy under project-history/.
async function projectStore(request, env, principal, slug, name, bytes, cond) {
  const summary = readSummary(request, "X-Project-Summary");
  if (summary instanceof Response) return summary;
  const key = projectFileKey(slug, name);
  const current = await env.BUCKET.head(key);
  if (cond.has("If-Match") ? current?.httpEtag !== cond.get("If-Match") : current)
    return reply("precondition failed", 412);
  const saved = await storeVersion(
    env,
    key,
    (n) => projectHistoryKey(slug, name, n),
    bytes,
    current,
    cond,
    "text/markdown; charset=utf-8",
    { saved_by: principal.kind === "user" ? principal.email : "device", summary },
    "project",
  );
  if (!saved) return reply("precondition failed", 412);
  return reply("ok", 200, "text/plain", {
    ETag: saved.obj.httpEtag,
    "X-Project-Version": String(saved.version),
    ...(saved.missing && { "X-Project-History": "missing" }),
  });
}

async function projectHistoryVersion(env, slug, name, version) {
  let obj = await env.BUCKET.get(projectHistoryKey(slug, name, version));
  if (!obj) {
    const current = await env.BUCKET.get(projectFileKey(slug, name));
    if (current && storedVersion(current) === version) obj = current;
  }
  return obj
    ? reply(obj.body, 200, "text/markdown", {
        "X-Project-Version": String(version),
      })
    : notFound();
}

async function projectPut(request, env, principal, slug) {
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
  return projectStore(request, env, principal, slug, "project.md", bytes, cond);
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

async function projectDocumentPut(request, env, principal, slug, name) {
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
  return projectStore(request, env, principal, slug, name, bytes, cond);
}

const pipelineKey = (scope) => `pipeline/${scope}.json`;
const pipelineHistoryKey = (scope, version) =>
  `pipeline-history/${scope}/${String(version).padStart(6, "0")}.json`;
const SUMMARY_MAX = 120;
const pipelineVersion = (obj) => Number.parseInt(obj?.customMetadata?.version, 10) || 0;
const sha256 = async (bytes) =>
  Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (x) =>
    x.toString(16).padStart(2, "0"),
  ).join("");

// A history summary: one line of 1 to SUMMARY_MAX characters that passes the note checks.
function checkSummary(text) {
  const length = typeof text === "string" ? [...text].length : 0;
  if (!length || length > SUMMARY_MAX || text.includes("\n") || text.includes("\r"))
    return `summary: must be one line of 1 to ${SUMMARY_MAX} characters`;
  return checkOperatorText(text, "summary");
}

// The summary header, percent-decoded and checked, or the 400 reply. Absent means "".
function readSummary(request, header) {
  let summary = request.headers.get(header) || "";
  try {
    summary = decodeURIComponent(summary);
  } catch {
    return reply("summary: must be percent-encoded UTF-8", 400);
  }
  const error = summary && checkSummary(summary);
  return error ? reply(error, 400) : summary;
}

// An object written before versioning has no version metadata and counts as version 1.
const storedVersion = (obj) => (obj ? pipelineVersion(obj) || 1 : 0);
const definedEntries = (meta) =>
  Object.fromEntries(Object.entries(meta).filter(([, value]) => value));

// Writes bytes at key as the next version, conditional on cond, and copies it create-only to
// historyKey(version). meta's empty values are left out. Returns {obj, version, missing}, or
// null when cond fails.
async function storeVersion(env, key, historyKey, bytes, current, cond, contentType, meta, label) {
  const cur = storedVersion(current);
  // Heal a history copy an earlier save failed to write, before this save replaces it.
  if (cur > 0 && !(await env.BUCKET.head(historyKey(cur)))) {
    const old = await env.BUCKET.get(key);
    if (old) {
      const data = await old.arrayBuffer();
      await env.BUCKET.put(historyKey(storedVersion(old)), data, {
        onlyIf: new Headers({ "If-None-Match": "*" }),
        httpMetadata: { contentType },
        customMetadata: old.customMetadata?.version
          ? old.customMetadata
          : definedEntries({
              version: "1",
              saved_at: old.uploaded?.toISOString(),
              sha256: await sha256(data),
            }),
      });
    }
  }
  const version = cur + 1;
  const options = {
    httpMetadata: { contentType },
    customMetadata: {
      version: String(version),
      saved_at: new Date().toISOString(),
      sha256: await sha256(bytes),
      ...definedEntries(meta),
    },
  };
  const obj = await env.BUCKET.put(key, bytes, { ...options, onlyIf: cond });
  if (!obj) return null;
  let missing = false;
  try {
    await env.BUCKET.put(historyKey(version), bytes, {
      ...options,
      onlyIf: new Headers({ "If-None-Match": "*" }),
    });
  } catch {
    // The next save heals the missing copy. Log only the key, never the body.
    console.warn(`${label} history copy failed: ${historyKey(version)}`);
    missing = true;
  }
  return { obj, version, missing };
}

// Every version under prefix, newest first: the history copies plus the current object at key,
// whose own history copy may still be missing.
async function listVersions(env, prefix, key) {
  const versions = new Map();
  const add = (obj, copy) => {
    const {
      version = "1",
      saved_by,
      saved_at = obj.uploaded?.toISOString(),
      sha256,
      summary = "",
      run,
      host,
    } = obj.customMetadata ?? {};
    versions.set(Number(version), {
      version: Number(version),
      saved_by,
      saved_at,
      sha256,
      summary,
      ...(run && { run }),
      ...(host && { host }),
      history_copy: copy,
    });
  };
  let cursor;
  do {
    const page = await env.BUCKET.list({ prefix, include: ["customMetadata"], cursor });
    for (const obj of page.objects) add(obj, true);
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  const current = await env.BUCKET.head(key);
  if (current && !versions.has(storedVersion(current))) add(current, false);
  return [...versions.values()].sort((a, b) => b.version - a.version);
}

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
  for (const key of ["archive", "tracer"])
    if (scope !== "global" && key in doc)
      return reply(`${key}: set only in the all-projects scope`, 400);
  const summary = readSummary(request, "X-Pipeline-Summary");
  if (summary instanceof Response) return summary;
  const current = await env.BUCKET.head(pipelineKey(scope));
  if (cond.has("If-Match") ? current?.httpEtag !== cond.get("If-Match") : current)
    return reply("precondition failed", 412);
  return pipelineStore(env, scope, bytes, current, cond, { saved_by: principal.email, summary });
}

// Writes bytes as the next version of scope, conditional on cond, and copies it create-only
// under pipeline-history/. meta is {saved_by, summary, run?, host?}; empty values are left out.
async function pipelineStore(env, scope, bytes, current, cond, meta) {
  const saved = await storeVersion(
    env,
    pipelineKey(scope),
    (n) => pipelineHistoryKey(scope, n),
    bytes,
    // A pipeline object without a version saves as version 1 with no heal copy, as before.
    pipelineVersion(current) ? current : null,
    cond,
    "application/json",
    meta,
    "pipeline",
  );
  if (!saved) return reply("precondition failed", 412);
  return reply("ok", 200, "text/plain", {
    ETag: saved.obj.httpEtag,
    "X-Pipeline-Version": String(saved.version),
    ...(saved.missing && { "X-Pipeline-History": "missing" }),
  });
}

async function pipelineGet(env, scope) {
  const obj = await env.BUCKET.get(pipelineKey(scope));
  return obj ? pipelineReply(obj, pipelineVersion(obj)) : notFound();
}

async function historyReply(env, principal, prefix, key) {
  const versions = await listVersions(env, prefix, key);
  if (principal.kind !== "user") for (const entry of versions) delete entry.saved_by;
  return reply(JSON.stringify(versions), 200, "application/json");
}

// The code points changed between a and b once their shared prefix and suffix are trimmed:
// the removed ones plus the added ones.
function changedChars(a, b) {
  const x = [...a],
    y = [...b];
  let start = 0;
  while (start < x.length && start < y.length && x[start] === y[start]) start++;
  let end = 0;
  while (
    end < x.length - start &&
    end < y.length - start &&
    x[x.length - 1 - end] === y[y.length - 1 - end]
  )
    end++;
  return x.length - start - end + (y.length - start - end);
}

// factory.py's MAX_EXPLORER_PROJECTS: a longer allowlist makes the autonomy record malformed.
const MAX_EXPLORER_PROJECTS = 1;
const RETRO_EDIT_CHANGE = 600;
const RETRO_EDITS_PER_DAY = 2;
const RETRO_EDIT_KEYS = ["schema", "run", "base_version", "stage", "note", "summary"];
const RETRO_EDIT_HOST = "retro edits come only from a bound host";
const RETRO_EDIT_SCOPE = "retro edits only allowlisted project scopes; global is person-only";
const RUN_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
const RETRO_DOC_EDIT_SCOPE = "retro edits only allowlisted projects";

// The board's autonomy record as { running, projects }. running needs a well-formed record
// with paused set to false; a missing or malformed record counts as paused, as in factory.py.
// projects is the allowlist when it is a well-formed list, else empty.
async function boardAutonomy(env) {
  const object = await env.BUCKET.get("board.json");
  let autonomy;
  try {
    autonomy = object && JSON.parse(await object.text()).autonomy;
  } catch {
    autonomy = undefined;
  }
  if (typeof autonomy !== "object" || autonomy === null || Array.isArray(autonomy))
    return { running: false, projects: [] };
  const names = (list) =>
    list === undefined || (Array.isArray(list) && list.every((v) => typeof v === "string"));
  const listed =
    names(autonomy.projects) && (autonomy.projects?.length ?? 0) <= MAX_EXPLORER_PROJECTS;
  return {
    running: autonomy.paused === false && listed && names(autonomy.self),
    projects: listed ? (autonomy.projects ?? []) : [],
  };
}

// The run error in a retro body, or null.
function retroRunError(run) {
  if (
    typeof run !== "string" ||
    run.length < 1 ||
    run.length > 64 ||
    [...run].some((c) => !RUN_CHARS.includes(c))
  )
    return "run: must be 1 to 64 of A-Z a-z 0-9 _ -";
  return null;
}

// The shape error in a factory.retro-edit/1 body, or null. Text rules run later, after the caps.
function retroEditShape(body) {
  if (
    typeof body !== "object" ||
    body === null ||
    Array.isArray(body) ||
    Object.keys(body).length !== RETRO_EDIT_KEYS.length ||
    !RETRO_EDIT_KEYS.every((key) => key in body)
  )
    return `body: needs keys ${RETRO_EDIT_KEYS.join(", ")}`;
  if (body.schema !== "factory.retro-edit/1") return "schema: must be factory.retro-edit/1";
  const run = retroRunError(body.run);
  if (run) return run;
  if (!Number.isInteger(body.base_version) || body.base_version < 0)
    return "base_version: must be a whole number";
  if (!RETRO_STAGES.includes(body.stage)) return `stage: must be one of ${RETRO_STAGES.join(", ")}`;
  if (typeof body.note !== "string") return "note: must be text";
  if (typeof body.summary !== "string") return "summary: must be text";
  return null;
}

// The shape error in a factory.retro-doc-edit/1 body, or null. Text rules run later, after the caps.
function retroDocEditShape(body) {
  const keys = ["schema", "run", "base_version", "find", "replace", "summary"];
  if (
    typeof body !== "object" ||
    body === null ||
    Array.isArray(body) ||
    Object.keys(body).length !== keys.length ||
    !keys.every((key) => key in body)
  )
    return `body: needs keys ${keys.join(", ")}`;
  if (body.schema !== "factory.retro-doc-edit/1") return "schema: must be factory.retro-doc-edit/1";
  const run = retroRunError(body.run);
  if (run) return run;
  if (!Number.isInteger(body.base_version) || body.base_version < 0)
    return "base_version: must be a whole number";
  if (typeof body.find !== "string" || [...body.find].length < 1 || [...body.find].length > 600)
    return "find: must be 1 to 600 characters";
  if (typeof body.replace !== "string" || [...body.replace].length > 600)
    return "replace: must be at most 600 characters";
  if (typeof body.summary !== "string") return "summary: must be text";
  return null;
}

// retro admit's one write: a bound host replaces one stage note, against the version it read,
// while autonomy runs, within the size and daily caps. Saved as a "retro" version a person
// can restore from history.
async function pipelineRetroPut(request, env, principal, scope) {
  const host = await boundHost(env, principal);
  if (!host) return reply(RETRO_EDIT_HOST, 403);
  const autonomy = await boardAutonomy(env);
  if (scope === "global" || !autonomy.projects.includes(scope.slice("projects/".length)))
    return reply(RETRO_EDIT_SCOPE, 403);
  if (!autonomy.running) return reply("autonomy is paused; no retro edit", 409);
  const body = await readJson(request);
  if (body instanceof Response) return body;
  const shape = retroEditShape(body);
  if (shape) return reply(shape, 400);
  const current = await env.BUCKET.get(pipelineKey(scope));
  if (body.base_version !== pipelineVersion(current)) return reply("precondition failed", 412);
  let doc = { schema: 1 };
  if (current) {
    try {
      doc = JSON.parse(await current.text());
    } catch {
      doc = undefined;
    }
    if (validatePipeline(doc)) return reply(PIPELINE_INVALID, 409);
  }
  if (changedChars(doc.stage_notes?.[body.stage] ?? "", body.note) > RETRO_EDIT_CHANGE)
    return reply(`retro edit changes more than ${RETRO_EDIT_CHANGE} characters`, 413);
  const since = Date.now() - DAY_MS;
  const recent = (await listVersions(env, `pipeline-history/${scope}/`, pipelineKey(scope))).filter(
    (v) => v.saved_by === "retro" && Date.parse(v.saved_at) > since,
  );
  if (recent.length >= RETRO_EDITS_PER_DAY)
    return reply(`retro edit cap: ${RETRO_EDITS_PER_DAY} per scope per day`, 429);
  const error = checkOperatorText(body.note, "note") ?? checkSummary(body.summary);
  if (error) return reply(error, 400);
  const next = { ...doc, stage_notes: { ...doc.stage_notes, [body.stage]: body.note } };
  const invalid = validatePipeline(next);
  if (invalid) return reply(invalid, 400);
  const cond = new Headers(current ? { "If-Match": current.httpEtag } : { "If-None-Match": "*" });
  return pipelineStore(env, scope, new TextEncoder().encode(JSON.stringify(next)), current, cond, {
    saved_by: "retro",
    summary: body.summary,
    run: body.run,
    host: host.id,
  });
}

async function pipelineHistoryVersion(env, scope, version) {
  const obj = await env.BUCKET.get(pipelineHistoryKey(scope, version));
  if (obj) return pipelineReply(obj, version);
  const current = await env.BUCKET.get(pipelineKey(scope));
  return current && pipelineVersion(current) === version
    ? pipelineReply(current, version)
    : notFound();
}

// retro admit's file write: a bound host splices one find-and-replace into a project file or
// document, against the version it read, while autonomy runs, within the size and daily caps.
// Saved as a "retro" version a person can restore from history. Retro never creates a missing
// file: a find that cannot match the stored text is refused.
async function projectRetroPut(request, env, principal, slug, name) {
  const host = await boundHost(env, principal);
  if (!host) return reply(RETRO_EDIT_HOST, 403);
  const autonomy = await boardAutonomy(env);
  if (!autonomy.projects.includes(slug)) return reply(RETRO_DOC_EDIT_SCOPE, 403);
  if (!autonomy.running) return reply("autonomy is paused; no retro edit", 409);
  const body = await readJson(request);
  if (body instanceof Response) return body;
  const shape = retroDocEditShape(body);
  if (shape) return reply(shape, 400);
  const key = projectFileKey(slug, name);
  const current = await env.BUCKET.get(key);
  if (body.base_version !== storedVersion(current)) return reply("precondition failed", 412);
  const text = current ? await current.text() : "";
  const start = text.indexOf(body.find);
  if (start < 0 || text.indexOf(body.find, start + 1) >= 0)
    return reply("find: must match the stored text exactly once", 409);
  // A plain splice at the one match, never String.replace, so $& in replace stays literal.
  const next = text.slice(0, start) + body.replace + text.slice(start + body.find.length);
  if (changedChars(text, next) > RETRO_EDIT_CHANGE)
    return reply(`retro edit changes more than ${RETRO_EDIT_CHANGE} characters`, 413);
  if (
    name === "project.md" &&
    JSON.stringify(frontmatter(text)) !== JSON.stringify(frontmatter(next))
  )
    return reply("retro edits never change project frontmatter", 403);
  const since = Date.now() - DAY_MS;
  const recent = (await listVersions(env, `project-history/${slug}/${name}/`, key)).filter(
    (v) => v.saved_by === "retro" && Date.parse(v.saved_at) > since,
  );
  if (recent.length >= RETRO_EDITS_PER_DAY)
    return reply(`retro edit cap: ${RETRO_EDITS_PER_DAY} per file per day`, 429);
  const error =
    (name === "project.md" ? validateProjectDocument(next) : validateProjectMarkdown(next)) ??
    checkOperatorText(next, "project document", "document") ??
    (name === "notes.md" ? checkOperatorText(body.replace, "replace", "note") : null) ??
    checkSummary(body.summary);
  if (error) return reply(error, 400);
  const cond = new Headers(current ? { "If-Match": current.httpEtag } : { "If-None-Match": "*" });
  const saved = await storeVersion(
    env,
    key,
    (n) => projectHistoryKey(slug, name, n),
    new TextEncoder().encode(next),
    current,
    cond,
    "text/markdown; charset=utf-8",
    { saved_by: "retro", summary: body.summary, run: body.run, host: host.id },
    "project",
  );
  if (!saved) return reply("precondition failed", 412);
  return reply("ok", 200, "text/plain", {
    ETag: saved.obj.httpEtag,
    "X-Project-Version": String(saved.version),
    ...(saved.missing && { "X-Project-History": "missing" }),
  });
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

const LESSON_LINE = /^- \[ \] ([\w-]+): Lesson: (\S.{0,199})$/u;
const LESSON_MARK = /^Lesson: from (\w[\w.-]{0,80}), queued (\d{4}-\d\d-\d\d)$/;

// A person's approve of a lesson card: append the card's bullet, built from the stored backlog.md,
// to <slug>/notes.md as the next version. A bullet already there adds nothing, so a retry is safe.
async function backlogLesson(request, env, principal) {
  if (principal.kind !== "user")
    return reply("lesson cards are approved only by a signed-in person", 403);
  const body = await readJson(request);
  if (body instanceof Response) return body;
  if (
    typeof body !== "object" ||
    body === null ||
    Array.isArray(body) ||
    Object.keys(body).join() !== "line" ||
    typeof body.line !== "string"
  )
    return reply('body must be {"line": "- [ ] <slug>: Lesson: <fact>"}', 400);
  const backlog = await env.BUCKET.get("backlog.md");
  const lines = (backlog ? await backlog.text() : "").split("\n").map((l) => l.replace(/\r$/, ""));
  const at = lines.indexOf(body.line);
  const line = at < 0 ? null : LESSON_LINE.exec(body.line);
  const held = [];
  for (let i = at + 1; at >= 0 && (lines[i] === "" || lines[i]?.startsWith("  ")); i++)
    held.push(lines[i].slice(2));
  const marks = held.map((l) => LESSON_MARK.exec(l)).filter(Boolean);
  if (!line || marks.length !== 1)
    return reply("that line is not an open lesson card in the backlog", 409);
  const [, slug, fact] = line;
  const unsafe = checkOperatorText(fact, "lesson", "note");
  if (unsafe) return reply(unsafe, 400);
  const [, job, day] = marks[0];
  if (!(await env.BUCKET.head(`projects/${slug}.md`))) return notFound();
  const key = projectFileKey(slug, "notes.md");
  const current = await env.BUCKET.get(key);
  const text = current ? await current.text() : "";
  const bullet = `- ${fact} (${job}, ${day})`;
  const answer = (version, added) =>
    reply(JSON.stringify({ file: `${slug}/notes.md`, version, added }), 200, "application/json");
  if (text.split("\n").some((l) => l.replace(/\r$/, "") === bullet))
    return answer(storedVersion(current), false);
  const next = text + (text && !text.endsWith("\n") ? "\n" : "") + bullet + "\n";
  const bytes = new TextEncoder().encode(next);
  if (bytes.length > MAX_BODY) return reply("notes.md over 1 MiB; nothing was saved", 413);
  const error =
    validateProjectMarkdown(next) ?? checkOperatorText(next, "project document", "document");
  if (error) return reply(error, 400);
  const cond = new Headers(current ? { "If-Match": current.httpEtag } : { "If-None-Match": "*" });
  const saved = await storeVersion(
    env,
    key,
    (n) => projectHistoryKey(slug, "notes.md", n),
    bytes,
    current,
    cond,
    "text/markdown; charset=utf-8",
    { saved_by: principal.email, summary: `Notes: lesson from ${job}` },
    "project",
  );
  if (!saved) return reply("precondition failed", 412);
  return answer(saved.version, true);
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

// The stored hold record, or null, for each of up to 50 items, in input order. A string that is not a backlog line gets null. Read only.
async function backlogTriageVerdicts(request, env) {
  const body = await readJson(request, MAX_TRIAGE_READ);
  if (body instanceof Response) return body;
  if (typeof body !== "object" || body === null || Array.isArray(body))
    return reply("body: must be a JSON object", 400);
  for (const key of Object.keys(body))
    if (key !== "items") return reply(`${key}: unknown field`, 400);
  const { items } = body;
  if (!Array.isArray(items) || items.length < 1 || items.length > 50)
    return reply("items: must be a list of 1 to 50 backlog items", 400);
  for (const [i, item] of items.entries())
    if (typeof item !== "string")
      return reply(`items[${i}]: ${BAD_BACKLOG_ITEM.slice("item: ".length)}`, 400);
  const verdicts = await Promise.all(
    items.map(async (item) => {
      if (!backlogItemMatch(item)) return null;
      const obj = await env.BUCKET.get(await triageKey(item));
      if (!obj) return null;
      try {
        const v = JSON.parse(await obj.text());
        return v && typeof v === "object" && !Array.isArray(v) ? v : null;
      } catch {
        return null;
      }
    }),
  );
  return reply(JSON.stringify({ verdicts }), 200, "application/json");
}

// The 400 text for a backlog item and its optional context, or null when both are fine.
function triageItemError(item, context) {
  if (!backlogItemMatch(item)) return BAD_BACKLOG_ITEM;
  if (context !== undefined && typeof context !== "string") return "context: must be text";
  if (context !== undefined && [...context].length > 6000) return "context: over 6000 characters";
  for (const [field, text] of [
    ["item", item],
    ["context", context],
  ]) {
    if (text === undefined) continue;
    const error = checkOperatorText(text, field, "document");
    if (error) return error;
    if (hasReservedTag(text, "backlog-item"))
      return `${field}: contains the reserved backlog-item tag`;
  }
  return null;
}

// Keeps a hold so other hosts can read it, and drops any older hold for a queued item.
async function storeTriageVerdict(env, item, verdict) {
  const key = await triageKey(item);
  if (verdict.decision === "hold")
    await env.BUCKET.put(
      key,
      JSON.stringify({ item, ...verdict, at: new Date().toISOString() }),
      JSON_TYPE,
    );
  else await env.BUCKET.delete(key);
}

// Marks each queued verdict whose line may repeat an open job of the project. The note is
// advisory, so any failure leaves the verdicts as they are.
async function flagDuplicates(env, slug, entries, verdicts) {
  if (!entries.length) return;
  try {
    const candidates = duplicateCandidates((await storedBoard(env)).jobs, slug);
    if (!candidates.length) return;
    const result = await env.AI.run(TRIAGE_MODEL, duplicateInput(slug, candidates, entries));
    for (const { n } of entries) {
      const id = duplicatePick(result, n, candidates);
      if (id) verdicts[n - 1].duplicate = id;
    }
  } catch {}
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
  const error = triageItemError(item, context);
  if (error) return reply(error, 400);
  const m = backlogItemMatch(item);
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
    await storeTriageVerdict(env, item, verdict);
    return json(verdict);
  };
  const lines = (context ?? "").split(/\r\n|\r|\n/);
  const held = (reason) => ({
    decision: "hold",
    confidence: null,
    threshold,
    reason,
    model: null,
    decider: "rule",
  });
  if (lines[0] === "Recommend: deny") return finish(held("its author recommends deny"));
  const source = project?.triage
    ? `pipeline:${slug}@${projectVersion}`
    : `pipeline:global@${globalVersion}`;
  // A question item always holds. The model adds its pick and a reason per option when it can.
  if (lines.some((line) => line.startsWith("## Question:"))) {
    const reason = "it asks questions a person must answer";
    const questions = backlogQuestions(context);
    if (!questions || !env.AI) return finish(held(reason));
    let result;
    try {
      result = await env.AI.run(
        TRIAGE_MODEL,
        triageInput(slug, source, triage.rules, item, context, questions),
      );
    } catch {
      return finish(held(reason));
    }
    const confidence = triageConfidence(result);
    const choices = triageChoices(result, questions, triage.rules);
    if (confidence === null || !choices) return finish(held(reason));
    return finish({
      decision: "hold",
      confidence,
      threshold,
      reason,
      model: TRIAGE_MODEL,
      choices,
      decider: "rule",
    });
  }
  if (!env.AI)
    return reply("backlog triage needs the Workers AI binding; deploy the current Worker", 503);
  let result;
  try {
    result = await env.AI.run(TRIAGE_MODEL, triageInput(slug, source, triage.rules, item, context));
  } catch {
    return reply("the triage model failed", 502);
  }
  const confidence = triageConfidence(result);
  if (confidence === null) return reply("the triage model gave an unusable reply", 502);
  const verdict = {
    decision: confidence >= threshold ? "queue" : "hold",
    confidence,
    threshold,
    reason: triage.rules ? TRIAGE_REASON : TRIAGE_REASON_NO_RULES,
    model: TRIAGE_MODEL,
    decider: "model",
  };
  if (verdict.decision === "queue")
    await flagDuplicates(env, slug, [{ n: 1, item, context }], [verdict]);
  return finish(verdict);
}

// Rates up to 8 plain backlog lines of one project in one model call. Each entry gets its own
// verdict, or its own error, so one bad item or answer holds only that line.
async function backlogTriageBatch(request, env) {
  const body = await readJson(request, MAX_TRIAGE_BATCH);
  if (body instanceof Response) return body;
  if (typeof body !== "object" || body === null || Array.isArray(body))
    return reply("body: must be a JSON object", 400);
  for (const key of Object.keys(body))
    if (key !== "items") return reply(`${key}: unknown field`, 400);
  const { items } = body;
  if (!Array.isArray(items) || items.length < 1 || items.length > 8)
    return reply("items: must be a list of 1 to 8 entries", 400);
  for (const [i, entry] of items.entries()) {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry))
      return reply(`items[${i}]: must be a JSON object`, 400);
    for (const key of Object.keys(entry))
      if (key !== "item" && key !== "context")
        return reply(`items[${i}].${key}: unknown field`, 400);
  }
  const slugs = new Set(items.map(({ item }) => backlogItemMatch(item)?.[1]).filter(Boolean));
  if (slugs.size > 1) return reply("items: every item must name the same project", 400);
  const errors = items.map(({ item, context }) => triageItemError(item, context));
  const json = (obj) => reply(JSON.stringify(obj), 200, "application/json");
  if (!slugs.size) return json({ verdicts: errors.map((error) => ({ error, status: 400 })) });
  const [slug] = slugs;
  if (!(await env.BUCKET.head(`projects/${slug}.md`))) return notFound();
  const docs = await pipelineDocs(env, slug);
  if (!docs) return reply(PIPELINE_INVALID, 409);
  const [, globalVersion, project, projectVersion] = docs;
  const { triage } = effectivePipeline(slug, ...docs);
  if (!triage.enabled) return json({ decision: "off" });
  const { threshold } = triage;
  const verdicts = items.map(({ context }, i) => {
    if (errors[i]) return { error: errors[i], status: 400 };
    const lines = (context ?? "").split(/\r\n|\r|\n/);
    if (lines[0] === "Recommend: deny")
      return {
        decision: "hold",
        confidence: null,
        threshold,
        reason: "its author recommends deny",
        model: null,
        decider: "rule",
      };
    if (lines.some((line) => line.startsWith("## Question:")))
      return {
        error: "context: an item that asks questions goes to POST /api/backlog/triage",
        status: 400,
      };
    return null;
  });
  const entries = items.flatMap(({ item, context }, i) =>
    verdicts[i] ? [] : [{ n: i + 1, item, context }],
  );
  if (entries.length) {
    if (!env.AI)
      return reply("backlog triage needs the Workers AI binding; deploy the current Worker", 503);
    const source = project?.triage
      ? `pipeline:${slug}@${projectVersion}`
      : `pipeline:global@${globalVersion}`;
    let result;
    try {
      result = await env.AI.run(
        TRIAGE_MODEL,
        triageBatchInput(slug, source, triage.rules, entries),
      );
    } catch {
      return reply("the triage model failed", 502);
    }
    for (const { n } of entries) {
      const confidence = triageBatchConfidence(result, n);
      verdicts[n - 1] =
        confidence === null
          ? { error: "the triage model gave an unusable reply", status: 502 }
          : {
              decision: confidence >= threshold ? "queue" : "hold",
              confidence,
              threshold,
              reason: triage.rules ? TRIAGE_REASON : TRIAGE_REASON_NO_RULES,
              model: TRIAGE_MODEL,
              decider: "model",
            };
    }
    const queued = entries.filter(({ n }) => verdicts[n - 1].decision === "queue");
    await flagDuplicates(env, slug, queued, verdicts);
  }
  for (const [i, verdict] of verdicts.entries())
    if (verdict.decision) await storeTriageVerdict(env, items[i].item, verdict);
  return json({ verdicts });
}

// Rates up to 8 bare backlog lines of one project under candidate rules in one model call.
// It stores nothing and ignores the saved triage settings, so replay can try rules safely.
async function backlogTriageRate(request, env) {
  const body = await readJson(request, MAX_TRIAGE_BATCH);
  if (body instanceof Response) return body;
  if (typeof body !== "object" || body === null || Array.isArray(body))
    return reply("body: must be a JSON object", 400);
  for (const key of Object.keys(body))
    if (key !== "rules" && key !== "items") return reply(`${key}: unknown field`, 400);
  const { rules, items } = body;
  if (typeof rules !== "string") return reply("rules: must be text", 400);
  const rulesError = rules && checkOperatorText(rules, "rules");
  if (rulesError) return reply(rulesError, 400);
  if (
    !Array.isArray(items) ||
    items.length < 1 ||
    items.length > 8 ||
    items.some((item) => typeof item !== "string")
  )
    return reply("items: must be a list of 1 to 8 backlog items", 400);
  for (const [i, item] of items.entries()) {
    const error = triageItemError(item);
    if (error) return reply(`items[${i}]: ${error.slice("item: ".length)}`, 400);
  }
  const slugs = new Set(items.map((item) => backlogItemMatch(item)[1]));
  if (slugs.size > 1) return reply("items: every item must name the same project", 400);
  const [slug] = slugs;
  if (!(await env.BUCKET.head(`projects/${slug}.md`))) return notFound();
  if (!env.AI)
    return reply("backlog triage needs the Workers AI binding; deploy the current Worker", 503);
  const entries = items.map((item, i) => ({ n: i + 1, item }));
  let result;
  try {
    result = await env.AI.run(
      TRIAGE_MODEL,
      triageBatchInput(slug, `rules-file:${slug}`, rules, entries),
    );
  } catch {
    return reply("the triage model failed", 502);
  }
  const confidences = entries.map(({ n }) => triageBatchConfidence(result, n));
  return reply(JSON.stringify({ model: TRIAGE_MODEL, confidences }), 200, "application/json");
}

// Rates one review round's CHANGES findings. The Worker compares the score with the fixed
// threshold and writes the reason, so review text can move the score but write no words.
async function reviewEscalation(request, env) {
  const body = await readJson(request, MAX_ESCALATION);
  if (body instanceof Response) return body;
  if (typeof body !== "object" || body === null || Array.isArray(body))
    return reply("body: must be a JSON object", 400);
  for (const key of Object.keys(body))
    if (key !== "reviews") return reply(`${key}: unknown field`, 400);
  const { reviews } = body;
  if (typeof reviews !== "object" || reviews === null || Array.isArray(reviews))
    return reply("reviews: must be an object", 400);
  if (!Object.keys(reviews).length) return reply("reviews: must name at least one reviewer", 400);
  const cleaned = {};
  for (const [name, text] of Object.entries(reviews)) {
    const field = `reviews.${name}`;
    if (!REVIEWERS.includes(name)) return reply(`${field}: unknown reviewer`, 400);
    if (typeof text !== "string") return reply(`${field}: must be text`, 400);
    if ([...text].length > ESCALATION_TEXT_LIMIT)
      return reply(`${field}: over ${ESCALATION_TEXT_LIMIT} characters`, 400);
    const clean = cleanReview(text);
    const error = checkOperatorText(clean, field, "document");
    if (error) return reply(error, 400);
    if (hasReservedTag(clean, "review-findings"))
      return reply(`${field}: contains the reserved review-findings tag`, 400);
    cleaned[name] = clean;
  }
  if (!env.AI)
    return reply("review escalation needs the Workers AI binding; deploy the current Worker", 503);
  let result;
  try {
    result = await env.AI.run(ESCALATION_MODEL, escalationInput(cleaned));
  } catch {
    return reply("the escalation model failed", 502);
  }
  const confidence = escalationConfidence(result);
  if (confidence === null) return reply("the escalation model gave an unusable reply", 502);
  const escalate = confidence >= ESCALATION_THRESHOLD;
  return reply(
    JSON.stringify({
      decision: escalate ? "escalate" : "continue",
      confidence,
      threshold: ESCALATION_THRESHOLD,
      reason: escalate ? ESCALATION_REASON_PERSON : ESCALATION_REASON_BUILDER,
      model: ESCALATION_MODEL,
      decider: "model",
    }),
    200,
    "application/json",
  );
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
const RETRO_WRITE_ONLY = "retro digests are written only by the host's bound service token";
const RETRO_READ_ONLY = "retro digests are read only by a device service token";
const RETRO_DATE_WINDOW = "retro digests are written only for yesterday, today or tomorrow in UTC";
const DAY_MS = 86400000;
const RETRO_KEEP_DAYS = 35;
const retroKey = (id, date) => `retro/${id}/${date}.jsonl`;
const isCalendarDate = (date) => {
  const time = Date.parse(`${date}T00:00:00Z`);
  return !Number.isNaN(time) && new Date(time).toISOString().slice(0, 10) === date;
};
const utcDay = (time) => Math.floor(time / DAY_MS) * DAY_MS;
const retroDateNear = (date, now) =>
  Math.abs(Date.parse(date + "T00:00:00Z") - utcDay(now)) <= DAY_MS;

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

// The paired or active host record bound to a service principal's token, or null.
async function boundHost(env, principal) {
  if (principal.kind !== "service" || !principal.client_id) return null;
  const records = await hostRecords(env);
  return (
    records
      .map((r) => r.host)
      .find(
        (h) => h.token === principal.client_id && (h.state === "paired" || h.state === "active"),
      ) ?? null
  );
}

const HALT_KEY = "halt.json";
const HALT_SHAPE = "a halt needs {halted: true, reason: 1-200 characters} or {halted: false}";
const HALT_PERSON_ONLY = "only a signed-in person can resume the factory";
const HALT_UNREADABLE =
  "the stored halt record is unreadable; resume from the dashboard to rewrite it";
const UNREADABLE_HALT = Object.freeze({ halted: true, reason: HALT_UNREADABLE });

// The stored global stop record. No key is not halted; a record that does not parse is halted.
async function readHalt(env) {
  const obj = await env.BUCKET.get(HALT_KEY);
  if (!obj) return { halted: false };
  let record = null;
  try {
    record = JSON.parse(await obj.text());
  } catch {}
  if (
    typeof record !== "object" ||
    record === null ||
    Array.isArray(record) ||
    typeof record.halted !== "boolean"
  )
    return UNREADABLE_HALT;
  return record;
}

// A service never sees the email of the person who set the halt.
function haltView(record, principal) {
  const view = { ...record };
  if (principal.kind !== "user" && view.by_kind === "person") delete view.by;
  return view;
}

async function haltGet(env, principal) {
  return reply(JSON.stringify(haltView(await readHalt(env), principal)), 200, "application/json");
}

async function haltPut(request, env, principal) {
  const text = await readBody(request, 16384);
  if (text === null) return reply("body over 16 KiB", 413);
  let body = null;
  try {
    body = JSON.parse(text);
  } catch {}
  const keys =
    typeof body === "object" && body !== null && !Array.isArray(body)
      ? Object.keys(body).sort().join(",")
      : "";
  const reason = typeof body?.reason === "string" ? body.reason.trim() : "";
  const valid =
    (keys === "halted,reason" &&
      body.halted === true &&
      reason.length >= 1 &&
      [...reason].length <= 200 &&
      // oxlint-disable-next-line no-control-regex -- this pattern exists to reject control characters.
      !/[\x00-\x1f\x7f-\x9f]/u.test(body.reason)) ||
    (keys === "halted" && body.halted === false);
  if (!valid) return reply(HALT_SHAPE, 400);
  if (!body.halted && principal.kind !== "user") return reply(HALT_PERSON_ONLY, 403);
  const current = await readHalt(env);
  if (current.halted === body.halted && current !== UNREADABLE_HALT)
    return reply(JSON.stringify(haltView(current, principal)), 200, "application/json");
  const record =
    principal.kind === "user"
      ? { halted: body.halted, by_kind: "person", by: principal.email }
      : {
          halted: body.halted,
          by_kind: "host",
          by: (await boundHost(env, principal))?.id ?? "device",
        };
  record.at = new Date().toISOString().slice(0, 19) + "Z";
  if (body.halted) record.reason = reason;
  await env.BUCKET.put(HALT_KEY, JSON.stringify(record), JSON_TYPE);
  return reply(JSON.stringify(haltView(record, principal)), 200, "application/json");
}

const TRIAL_RESUME_SHAPE =
  "a trial resume needs {instruction: 1-500 characters}; a signed-in person may send {}";
const TRIAL_RESUME_ONLY =
  "only a signed-in person, or a paired host passing a person's instruction, resumes stage-note trials";

// Marks slug's unresumed reverted trials resumed, recording who resumed them. An R2 race with
// another board writer is retried here.
async function trialsResume(request, env, principal, slug) {
  const text = await readBody(request, 16384);
  if (text === null) return reply("body over 16 KiB", 413);
  let body = null;
  try {
    body = JSON.parse(text);
  } catch {}
  const keys =
    typeof body === "object" && body !== null && !Array.isArray(body)
      ? Object.keys(body).join(",")
      : null;
  const person = principal.kind === "user";
  if (!(keys === "instruction" || (person && keys === ""))) return reply(TRIAL_RESUME_SHAPE, 400);
  const instruction = typeof body.instruction === "string" ? body.instruction.trim() : "";
  if (keys === "instruction") {
    if (instruction.length < 1 || [...instruction].length > 500)
      return reply(TRIAL_RESUME_SHAPE, 400);
    const error = checkOperatorText(instruction, "instruction");
    if (error) return reply(error, 400);
  }
  const host = person ? null : await boundHost(env, principal);
  if (!person && !host) return reply(TRIAL_RESUME_ONLY, 403);
  const resumedBy = person ? { kind: "person" } : { kind: "host", by: host.id, instruction };
  for (let attempt = 0; attempt < 10; attempt++) {
    const stored = await storedBoard(env);
    if (stored.etag && !stored.doc)
      return reply("the stored board is not readable JSON. Nothing was written.", 400);
    const resumed = new Date().toISOString().slice(0, 19) + "Z";
    let n = 0;
    const trials = (Array.isArray(stored.doc?.trials) ? stored.doc.trials : []).map((t) => {
      if (
        typeof t !== "object" ||
        t === null ||
        t.project !== slug ||
        t.status !== "reverted" ||
        t.resumed
      )
        return t;
      n++;
      return { ...t, resumed, resumed_by: resumedBy };
    });
    if (!n) return reply(JSON.stringify({ resumed: 0 }), 200, "application/json");
    const data = { ...stored.doc, trials };
    const obj = await env.BUCKET.put("board.json", JSON.stringify(data), {
      onlyIf: new Headers({ "If-Match": stored.etag }),
      httpMetadata: { contentType: "application/json" },
    });
    if (obj) {
      await announce(env, "board.json", rowsFrame(stored, data, obj.httpEtag));
      return reply(JSON.stringify({ resumed: n }), 200, "application/json");
    }
    await new Promise((resolve) => setTimeout(resolve, 20 + Math.random() * 80));
  }
  return reply(JSON.stringify({ error: "busy" }), 409, "application/json");
}

// A re-push replaces the host's file for that day. Every line is a JSON object naming the host.
async function retroPut(request, env, principal, id, date) {
  if ((await boundHost(env, principal))?.id !== id) return reply(RETRO_WRITE_ONLY, 403);
  if (!retroDateNear(date, Date.now())) return reply(RETRO_DATE_WINDOW, 400);
  const bytes = await readCapped(request, MAX_BODY);
  if (bytes === null) return reply("body over 1 MiB", 413);
  let text;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return reply("retro digest must be valid UTF-8", 400);
  }
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  for (const [i, line] of lines.entries()) {
    let row = null;
    try {
      row = JSON.parse(line);
    } catch {}
    if (typeof row !== "object" || row === null || Array.isArray(row) || row.host !== id)
      return reply(`line ${i + 1} is not a JSON object with host ${id}`, 400);
  }
  await env.BUCKET.put(retroKey(id, date), bytes, {
    httpMetadata: { contentType: "application/x-ndjson" },
  });
  return reply("ok");
}

const AUDIT_WRITE_ONLY = "audit rows are written only by the host's bound service token";
const auditKey = (job, id) => `audit/${job}/${id}.jsonl`;

// A re-push replaces the host's rows for the job. Every line is a JSON object naming the host and job.
async function auditPut(request, env, principal, job, id) {
  if ((await boundHost(env, principal))?.id !== id) return reply(AUDIT_WRITE_ONLY, 403);
  const bytes = await readCapped(request, MAX_BODY);
  if (bytes === null) return reply("body over 1 MiB", 413);
  let text;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return reply("audit rows must be valid UTF-8", 400);
  }
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  const rows = [];
  for (const [i, line] of lines.entries()) {
    let row = null;
    try {
      row = JSON.parse(line);
    } catch {}
    if (
      typeof row !== "object" ||
      row === null ||
      Array.isArray(row) ||
      row.host !== id ||
      row.job !== job
    )
      return reply(`line ${i + 1} is not a JSON object with host ${id} and job ${job}`, 400);
    rows.push(row);
  }
  await env.BUCKET.put(auditKey(job, id), bytes, {
    httpMetadata: { contentType: "application/x-ndjson" },
  });
  if (!(await auditFlagsUpdate(env, job, id, rows))) return reply(AUDIT_FLAGS_BUSY, 503);
  return reply("ok");
}

const AUDIT_FLAGS_KEY = "audit-flags.json";
const AUDIT_FLAGS_MAX = 500;
const AUDIT_FLAGS_HOST_MAX = 100;
const AUDIT_FLAGS_BUSY = "audit rows were saved, but the red-flag index kept changing; push again";
const AUDIT_FLAGS_SCHEMA = "factory.audit-flags/1";
const AUDIT_FLAGS_TIME = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/;
const AUDIT_FLAGS_AHEAD_MS = 24 * 60 * 60 * 1000;
const AUDIT_FLAGS_TEXT_MAX = 120;
const AUDIT_FLAGS_FLAGS_MAX = 16;
const AUDIT_FLAGS_FLAG_MAX = 32;

function auditFlagTime(time, now) {
  if (typeof time !== "string" || !AUDIT_FLAGS_TIME.test(time)) return false;
  const iso = time.replace(" ", "T");
  const ms = Date.parse(iso + "Z");
  return (
    !Number.isNaN(ms) &&
    new Date(ms).toISOString().slice(0, 19) === iso &&
    ms <= now + AUDIT_FLAGS_AHEAD_MS
  );
}

// A row's Paseo link when both ids are well formed, else null.
function auditFlagPaseo(p) {
  const id = /^[A-Za-z0-9_-]{1,64}$/;
  return p &&
    typeof p.server === "string" &&
    typeof p.agent === "string" &&
    id.test(p.server) &&
    id.test(p.agent)
    ? { server: p.server, agent: p.agent }
    : null;
}

// The flagged calls of one job and host with a host-shaped UTC time, each field capped.
function auditFlagRows(rows, job, host) {
  const now = Date.now();
  const cap = (value, max) =>
    typeof value === "string" ? Array.from(value).slice(0, max).join("") : "";
  return rows
    .filter(
      (row) =>
        auditFlagTime(row.time, now) &&
        Array.isArray(row.flags) &&
        row.flags.length > 0 &&
        row.flags.every((flag) => typeof flag === "string"),
    )
    .map((row) => {
      const paseo = auditFlagPaseo(row.paseo);
      return {
        time: row.time,
        job,
        host,
        agent: cap(row.agent, AUDIT_FLAGS_TEXT_MAX),
        tool: cap(row.tool, AUDIT_FLAGS_TEXT_MAX),
        flags: row.flags
          .slice(0, AUDIT_FLAGS_FLAGS_MAX)
          .map((flag) => cap(flag, AUDIT_FLAGS_FLAG_MAX)),
        target: cap(row.target, AUDIT_FLAGS_TEXT_MAX),
        ...(paseo && { paseo }),
      };
    });
}

// Newest first, then job, host and agent.
function auditFlagOrder(a, b) {
  if (a.time !== b.time) return a.time < b.time ? 1 : -1;
  for (const field of ["job", "host", "agent"])
    if (a[field] !== b[field]) return a[field] < b[field] ? -1 : 1;
  return 0;
}

// Newest first, at most 100 rows per host and 500 in all.
function auditFlagsKeep(rows) {
  const counts = new Map();
  return rows
    .sort(auditFlagOrder)
    .filter((row) => {
      counts.set(row.host, (counts.get(row.host) ?? 0) + 1);
      return counts.get(row.host) <= AUDIT_FLAGS_HOST_MAX;
    })
    .slice(0, AUDIT_FLAGS_MAX);
}

const auditFlagKey = (row) =>
  JSON.stringify([row.time, row.job, row.host, row.agent, row.tool, row.target]);

// Replaces the job and host's rows in the index and keeps their ack and backlog. False after ten lost conditional puts.
async function auditFlagsUpdate(env, job, host, rows) {
  const fresh = auditFlagRows(rows, job, host);
  for (let attempt = 0; attempt < 10; attempt++) {
    const prev = await env.BUCKET.get(AUDIT_FLAGS_KEY);
    const old = prev ? JSON.parse(await prev.text()).rows : [];
    const kept = old.filter((row) => row.job !== job || row.host !== host);
    const prior = new Map(
      old
        .filter((row) => (row.ack || row.backlog) && row.job === job && row.host === host)
        .map((row) => [auditFlagKey(row), row]),
    );
    for (const row of fresh) {
      const was = prior.get(auditFlagKey(row));
      if (was?.ack) row.ack = was.ack;
      else delete row.ack;
      if (was?.backlog) row.backlog = was.backlog;
      else delete row.backlog;
    }
    const next = auditFlagsKeep([...kept, ...fresh]);
    const saved = await env.BUCKET.put(
      AUDIT_FLAGS_KEY,
      JSON.stringify({ schema: AUDIT_FLAGS_SCHEMA, rows: next }),
      {
        ...JSON_TYPE,
        onlyIf: new Headers(prev ? { "If-Match": prev.httpEtag } : { "If-None-Match": "*" }),
      },
    );
    if (saved) return true;
  }
  return false;
}

async function auditFlagsGet(env) {
  const text = await (await env.BUCKET.get(AUDIT_FLAGS_KEY))?.text();
  return reply(
    text ?? JSON.stringify({ schema: AUDIT_FLAGS_SCHEMA, rows: [] }),
    200,
    "application/json",
  );
}

const AUDIT_FLAGS_ACK_BODY =
  'body must be {"row": {time, job, host, agent, tool, target}, "ack": true or false}';

// Sets or clears one row's ack time in the index. Only a signed-in person may.
async function auditFlagsAck(request, env, principal) {
  if (principal.kind !== "user")
    return reply("red flags are acknowledged only by a signed-in person", 403);
  const body = await readJson(request);
  if (body instanceof Response) return body;
  const row = body?.row;
  const fields = ["time", "job", "host", "agent", "tool", "target"];
  if (
    typeof body?.ack !== "boolean" ||
    !row ||
    typeof row !== "object" ||
    !fields.every((field) => typeof row[field] === "string")
  )
    return reply(AUDIT_FLAGS_ACK_BODY, 400);
  const key = auditFlagKey(row);
  for (let attempt = 0; attempt < 10; attempt++) {
    const prev = await env.BUCKET.get(AUDIT_FLAGS_KEY);
    const index = prev ? JSON.parse(await prev.text()) : { schema: AUDIT_FLAGS_SCHEMA, rows: [] };
    const target = index.rows.find((r) => auditFlagKey(r) === key);
    if (!target) return reply("that red flag is no longer in the index; reload", 409);
    if (body.ack) target.ack = new Date().toISOString().slice(0, 19) + "Z";
    else delete target.ack;
    const saved = await env.BUCKET.put(AUDIT_FLAGS_KEY, JSON.stringify(index), {
      ...JSON_TYPE,
      onlyIf: new Headers({ "If-Match": prev.httpEtag }),
    });
    if (saved) return reply(JSON.stringify(index), 200, "application/json");
  }
  return reply("the red-flag index kept changing; try again", 503);
}

const AUDIT_FLAGS_BACKLOG = /^- \[ \] [\w-]+: \S[^\p{Cc}\u2028\u2029]*$/u;
const AUDIT_FLAGS_BACKLOG_BODY =
  'body must be {"backlog": "- [ ] <project>: <title>", "rows": [{time, job, host, agent, tool, target}, ...]} with 1 to 500 rows';

// Stores a retro's backlog line on each listed row. Only a bound host may.
async function auditFlagsBacklog(request, env, principal) {
  if (!(await boundHost(env, principal)))
    return reply("red-flag backlog links are written only by a bound host's service token", 403);
  const body = await readJson(request, 256 << 10);
  if (body instanceof Response) return body;
  const fields = ["time", "job", "host", "agent", "tool", "target"];
  const line = body?.backlog;
  const rows = body?.rows;
  if (
    typeof line !== "string" ||
    line.length > 240 ||
    !AUDIT_FLAGS_BACKLOG.test(line) ||
    !Array.isArray(rows) ||
    rows.length < 1 ||
    rows.length > AUDIT_FLAGS_MAX ||
    !rows.every(
      (row) => row && typeof row === "object" && fields.every((f) => typeof row[f] === "string"),
    )
  )
    return reply(AUDIT_FLAGS_BACKLOG_BODY, 400);
  const keys = new Set(rows.map(auditFlagKey));
  for (let attempt = 0; attempt < 10; attempt++) {
    const prev = await env.BUCKET.get(AUDIT_FLAGS_KEY);
    if (!prev) return reply(JSON.stringify({ linked: 0 }), 200, "application/json");
    const index = JSON.parse(await prev.text());
    const hits = index.rows.filter((r) => keys.has(auditFlagKey(r)));
    if (!hits.length) return reply(JSON.stringify({ linked: 0 }), 200, "application/json");
    for (const r of hits) r.backlog = line;
    const saved = await env.BUCKET.put(AUDIT_FLAGS_KEY, JSON.stringify(index), {
      ...JSON_TYPE,
      onlyIf: new Headers({ "If-Match": prev.httpEtag }),
    });
    if (saved) return reply(JSON.stringify({ linked: hits.length }), 200, "application/json");
  }
  return reply("the red-flag index kept changing; try again", 503);
}

// Every host's rows for the job, in host record order, each file ending in a newline.
async function auditGet(env, job) {
  let out = "";
  for (const { host } of await hostRecords(env)) {
    const text = await (await env.BUCKET.get(auditKey(job, host.id)))?.text();
    if (text) out += text.endsWith("\n") ? text : text + "\n";
  }
  return reply(out, 200, "application/x-ndjson");
}

const RETRO_RUN_WRITE_ONLY = "retro runs are written only by a bound host's service token";
const RETRO_RUN_TAKEN = "retro run belongs to another host";
const RETRO_RUN_CHANGED = "retro run changed, retry";
const RETRO_RUN_BYTES = 64 << 10;
const RETRO_RUNS_LISTED = 30;
const RETRO_RUN_KEEP_DAYS = 90;
const RETRO_CATEGORIES = [
  "navigation",
  "automated-checks",
  "reviewer-rules",
  "steering",
  "tool-economy",
  "information-access",
  "permissions",
];
const RETRO_RUN_KEYS = "schema run host from to sessions rejected findings cards".split(" ");
const RETRO_RUN_DATES = "from to".split(" ");
const retroRunKey = (run) => `retro-runs/${run}.json`;

// The shape error in a factory.retro-run/1 body, or null. Every string is one line.
function retroRunShape(body, run, hostId) {
  const isObject = (v) => typeof v === "object" && v !== null && !Array.isArray(v);
  const exact = (v, keys) =>
    isObject(v) && Object.keys(v).length === keys.length && keys.every((k) => k in v);
  const text = (v, lo, hi) =>
    typeof v === "string" && v.length >= lo && v.length <= hi && !/[\p{Cc}\u2028\u2029]/u.test(v);
  const whole = (v, lo, hi) => Number.isInteger(v) && v >= lo && v <= hi;
  const slug = (v) => typeof v === "string" && /^[\p{L}\p{N}_-]+$/u.test(v);
  const card = (c) =>
    exact(c, ["title", "project", "fingerprint", "result"]) &&
    text(c.title, 1, 80) &&
    slug(c.project) &&
    /^[0-9a-f]{16}$/.test(c.fingerprint) &&
    text(c.result, 1, 120);
  const cards = (list, max) => Array.isArray(list) && list.length <= max && list.every(card);
  const finding = (f) =>
    exact(f, [
      "rank",
      "title",
      "category",
      "count",
      "cost",
      "project",
      "root_cause",
      "fix",
      "cards",
    ]) &&
    whole(f.rank, 1, 12) &&
    text(f.title, 1, 80) &&
    RETRO_CATEGORIES.includes(f.category) &&
    whole(f.count, 1, 10_000_000) &&
    whole(f.cost, 1, 1_000_000_000) &&
    slug(f.project) &&
    text(f.root_cause, 1, 300) &&
    text(f.fix, 1, 300) &&
    cards(f.cards, 5);
  if (!exact(body, RETRO_RUN_KEYS))
    return "body: needs keys schema, run, host, from, to, sessions, rejected, findings, cards";
  if (body.schema !== "factory.retro-run/1") return "schema: must be factory.retro-run/1";
  if (body.run !== run) return `run: must be ${run}`;
  if (body.host !== hostId) return `host: must be ${hostId}`;
  for (const key of RETRO_RUN_DATES)
    if (typeof body[key] !== "string" || !isCalendarDate(body[key]))
      return `${key}: must be a calendar date`;
  for (const key of ["sessions", "rejected"])
    if (!whole(body[key], 0, 10_000_000))
      return `${key}: must be a whole number from 0 to 10000000`;
  if (!Array.isArray(body.findings) || body.findings.length < 1 || body.findings.length > 12)
    return "findings: must be a list of 1-12 items";
  if (!body.findings.every(finding)) return "findings: an item is malformed";
  if (!cards(body.cards, 5)) return "cards: must be a list of 0-5 well-formed cards";
  return null;
}

// The host a stored retro run names, or undefined when the record does not parse.
async function retroRunOwner(object) {
  try {
    return JSON.parse(await object.text()).host;
  } catch {}
}

// A bound host stores its run's record. Only the host that stored a run replaces it.
async function retrosPut(request, env, principal, run) {
  const host = await boundHost(env, principal);
  if (!host) return reply(RETRO_RUN_WRITE_ONLY, 403);
  const body = await readJson(request, RETRO_RUN_BYTES);
  if (body instanceof Response) return body;
  const error = retroRunShape(body, run, host.id);
  if (error) return reply(error, 400);
  const key = retroRunKey(run);
  const prev = await env.BUCKET.get(key);
  if (prev && (await retroRunOwner(prev)) !== host.id) return reply(RETRO_RUN_TAKEN, 409);
  const stored = await env.BUCKET.put(key, JSON.stringify(body), {
    ...JSON_TYPE,
    onlyIf: new Headers(prev ? { "If-Match": prev.httpEtag } : { "If-None-Match": "*" }),
  });
  if (!stored) {
    const now = await env.BUCKET.get(key);
    if (now && (await retroRunOwner(now)) !== host.id) return reply(RETRO_RUN_TAKEN, 409);
    return reply(RETRO_RUN_CHANGED, 409);
  }
  return reply("ok");
}

// The newest retro runs, newest first. Run ids start with a UTC timestamp, so name order is date order.
async function retrosList(env) {
  const keys = [];
  let cursor;
  do {
    const page = await env.BUCKET.list({ prefix: "retro-runs/", cursor });
    keys.push(...page.objects.map((o) => o.key));
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  keys.sort().reverse();
  const records = await Promise.all(
    keys.slice(0, RETRO_RUNS_LISTED).map(async (key) => (await env.BUCKET.get(key))?.text()),
  );
  return reply(
    `[${records.filter((text) => text !== undefined).join(",")}]`,
    200,
    "application/json",
  );
}

// The hosts with a retro digest for the day, in host record order.
async function retroDay(env, date) {
  const out = [];
  for (const { host } of await hostRecords(env))
    if (await env.BUCKET.head(retroKey(host.id, date))) out.push(host.id);
  return reply(JSON.stringify(out), 200, "application/json");
}

const RETRO_KEY = /^retro\/[^/]+\/(\d{4}-\d{2}-\d{2})\.jsonl$/;

// Deletes retro digests dated more than RETRO_KEEP_DAYS before now's UTC date.
async function pruneRetro(env, now) {
  const cutoff = new Date(utcDay(now.getTime()) - RETRO_KEEP_DAYS * DAY_MS)
    .toISOString()
    .slice(0, 10);
  const deleted = [];
  let cursor;
  do {
    const page = await env.BUCKET.list({ prefix: "retro/", cursor });
    for (const { key } of page.objects) {
      const date = key.match(RETRO_KEY)?.[1];
      if (date && date < cutoff) {
        await env.BUCKET.delete(key);
        deleted.push(key);
      }
    }
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  return { status: "pruned", deleted };
}

// Deletes retro run records uploaded more than RETRO_RUN_KEEP_DAYS before now's UTC date.
async function pruneRetroRuns(env, now) {
  const cutoff = utcDay(now.getTime()) - RETRO_RUN_KEEP_DAYS * DAY_MS;
  const deleted = [];
  let cursor;
  do {
    const page = await env.BUCKET.list({ prefix: "retro-runs/", cursor });
    for (const { key, uploaded } of page.objects) {
      if (uploaded.getTime() < cutoff) {
        await env.BUCKET.delete(key);
        deleted.push(key);
      }
    }
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  return { status: "pruned", deleted };
}

const IDEA_ORPHAN_GRACE_MS = 2 * DAY_MS;
// A board write may add a reference only to an upload this fresh, half the sweep's grace, so no write can name an object a running sweep deletes.
const IDEA_REF_FRESH_MS = DAY_MS;

// Deletes idea uploads older than IDEA_ORPHAN_GRACE_MS that no board or archive row names.
// The board is read before the archive, since runArchive writes the archive first.
async function pruneIdeaUploads(env, now) {
  const named = new Set();
  for (const key of ["board.json", ARCHIVE_KEY]) {
    const object = await env.BUCKET.get(key);
    if (!object && key === ARCHIVE_KEY) continue;
    let jobs;
    try {
      jobs = JSON.parse(await object.text()).jobs;
    } catch {}
    if (!Array.isArray(jobs)) {
      console.warn(`idea upload sweep skipped: ${key} is unreadable`);
      return { status: "skipped", deleted: [] };
    }
    for (const j of jobs) {
      for (const n of j?.idea_images ?? [])
        if (typeof n === "string") named.add(`idea-images/${n}`);
      for (const n of j?.idea_files ?? []) if (typeof n === "string") named.add(`idea-files/${n}`);
    }
  }
  const cutoff = now.getTime() - IDEA_ORPHAN_GRACE_MS;
  const deleted = [];
  for (const prefix of ["idea-images/", "idea-files/"]) {
    let cursor;
    do {
      const page = await env.BUCKET.list({ prefix, cursor });
      for (const { key, uploaded } of page.objects) {
        if (named.has(key) || uploaded.getTime() >= cutoff) continue;
        const fresh = await env.BUCKET.head(key);
        if (fresh && fresh.uploaded.getTime() < cutoff) {
          await env.BUCKET.delete(key);
          deleted.push(key);
        }
      }
      cursor = page.truncated ? page.cursor : undefined;
    } while (cursor);
  }
  return { status: "pruned", deleted };
}

// Raw image bytes, never decoded as text, or null past the cap.
const readImage = (request) => readCapped(request, MAX_IMAGE);

// Stores clef-flash's pick and fixed reasons for a pushed needs.md. Advisory: every early
// return or failure leaves the form as it was, and the push reply's status never changes.
// The record's ms is the model call alone.
async function rateNeeds(env, slug, id, text) {
  if (!env.AI) return;
  const questions = needsQuestions(text);
  if (
    !questions ||
    checkOperatorText(text, "needs.md", "document") ||
    hasReservedTag(text, "backlog-item")
  )
    return;
  if (!(await env.BUCKET.head(`projects/${slug}.md`))) return;
  const docs = await pipelineDocs(env, slug);
  if (!docs) return;
  const [, globalVersion, project, projectVersion] = docs;
  const { triage } = effectivePipeline(slug, ...docs);
  if (!triage.enabled) return;
  const source = project?.triage
    ? `pipeline:${slug}@${projectVersion}`
    : `pipeline:global@${globalVersion}`;
  const started = Date.now();
  const result = await env.AI.run(TRIAGE_MODEL, needsInput(source, triage.rules, text, questions));
  const ms = Date.now() - started;
  const choices = triageChoices(result, questions, triage.rules);
  if (!choices) return;
  await env.BUCKET.put(
    needsTriageKey(slug, id),
    JSON.stringify({
      sha256: await sha256(new TextEncoder().encode(text)),
      model: TRIAGE_MODEL,
      ms,
      at: new Date().toISOString(),
      choices,
      decider: "model",
    }),
    JSON_TYPE,
  );
}

// Job files take an optional condition, so a device never overwrites a version it
// hasn't seen. Devices that send none still write blind. Images keep the type their
// extension names.
async function jobPut(request, env, key, ctx) {
  const cond = condition(request);
  if (cond === null) return needsCondition();
  const type = imageType(key);
  const body = type ? await readImage(request) : await readBody(request);
  if (body === null) return reply(type ? "image over 5 MiB" : "body over 1 MiB", 413);
  const obj = await env.BUCKET.put(key, body, {
    ...(cond && { onlyIf: cond }),
    ...(type && { httpMetadata: { contentType: type } }),
  });
  // A stored needs.md is rated after the reply; Server-Timing says how long the push waited on it.
  const needs = obj && NEEDS_KEY.exec(key);
  let timing;
  if (needs) {
    const started = Date.now();
    ctx.waitUntil(rateNeeds(env, needs[1], needs[2], body).catch(() => {}));
    timing = `needs-rating;dur=${Date.now() - started}`;
  }
  return obj
    ? reply("ok", 200, "text/plain", {
        ETag: obj.httpEtag,
        ...(timing && { "Server-Timing": timing }),
      })
    : reply("precondition failed", 412);
}

// The stored board's version, rows, archived field and parsed document. An unreadable board
// has no rows and a null doc.
async function storedBoard(env) {
  const object = await env.BUCKET.get("board.json");
  if (!object) return { etag: null, jobs: [], doc: null };
  try {
    const doc = JSON.parse(await object.text());
    const { jobs, archived } = doc;
    return {
      etag: object.httpEtag,
      jobs: Array.isArray(jobs) ? jobs : [],
      archived,
      doc: typeof doc === "object" && !Array.isArray(doc) ? doc : null,
    };
  } catch {
    return { etag: object.httpEtag, jobs: [], doc: null };
  }
}

// Spend nudges for the hour; a failure is logged so the stuck and stale pushes still go out.
async function spendEvents(env, at) {
  if (!pushKey(env)) return [];
  try {
    return await spendNudgePass(env, await weeklyBudgets(env), at);
  } catch (error) {
    if (error instanceof Error) console.warn(`spend nudge pass failed: ${error.message}`);
    else console.warn("spend nudge pass failed:", error);
    return [];
  }
}

// The hourly stuck, stale and spend push. A scheduled env has no config, so it is read here for VAPID.
async function pushHourly(env, at) {
  const { jobs } = await storedBoard(env);
  return sendPushes({ ...env, config: readServerConfig(env) }, [
    ...stuckPushes(jobs, at),
    ...stalePushes(jobs, at),
    ...(await spendEvents(env, at)),
  ]);
}

// JSON-quoted id with every char outside printable ASCII as \uXXXX, safe to echo to a terminal.
const quoteId = (id) =>
  JSON.stringify(id).replace(
    /[^\x20-\x7e]/g,
    (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"),
  );

// The dashboard keeps 200 characters of a refusal; two ids at this cap fit beside the rider text.
const QUOTED_ID_MAX = 36;

// quoteId, cut on the raw id so the result is at most max characters, ending in ...
function quoteIdCut(id, max) {
  const full = quoteId(id);
  if (full.length <= max) return full;
  let n = Math.min(id.length, max - 5);
  while (quoteId(id.slice(0, n)).length + 3 > max) n--;
  return quoteId(id.slice(0, n)).slice(0, -1) + '..."';
}

// An error message, or null. A row's goal is the spec Goal paragraph, which factory.py push
// cuts to 300 code points, so anything else is a client bug.
function checkGoals(jobs) {
  for (const row of jobs) {
    if (!Object.hasOwn(row ?? {}, "goal")) continue;
    const { goal } = row;
    if (typeof goal === "string" && [...goal].length <= 300) continue;
    const label =
      typeof row.id === "string" ? quoteIdCut(row.id, QUOTED_ID_MAX) : "a row without a string id";
    return `a board write cannot set goal on ${label}: it must be a string of at most 300 characters. Nothing was written.`;
  }
  return null;
}

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

// An error message, or null. A new row's id must be one job folder name, as JOB_DIR reads it, and
// not a bare number. Otherwise a client that patches "<project>/<id>", "592" or a stray "--help"
// as a job id adds a projectless duplicate whose links go nowhere. Stored rows keep their ids.
function checkNewIds(storedJobs, jobs) {
  const ids = new Set(storedJobs.map((j) => j?.id));
  for (const j of jobs) {
    const id = j?.id;
    if (ids.has(id) || (typeof id === "string" && /^\w[\w.-]*$/.test(id) && !/^\d+$/.test(id)))
      continue;
    return `a board write cannot add ${quoteIdCut(String(id), QUOTED_ID_MAX)}: a job id has no project prefix and is never a bare number, like 003-rate-limiting. Nothing was written. Upgrade the factory skill, or claim new ids with factory.py claim.`;
  }
  return null;
}

// An error message, or null. Run batch now marks a held idea already on the board as the
// starter, and no read can cover a row that arrives new with that mark.
function checkNewStarters(storedJobs, jobs) {
  const ids = new Set(storedJobs.map((j) => j?.id));
  const row = jobs.find(
    (j) => !ids.has(j?.id) && j?.stage === "idea" && j.priority?.batch === true,
  );
  return row
    ? `a board write cannot add ${quoteIdCut(row.id, QUOTED_ID_MAX)} as a batch starter: start a batch from a held idea already on the board. Nothing was written.`
    : null;
}

// An error message, or null. Only the archive pass writes `archived`, and a client that
// ignores its id floor must not hand out an archived job's id again.
function checkArchived(stored, data) {
  if (JSON.stringify(data.archived ?? null) !== JSON.stringify(stored.archived ?? null))
    return "a board write cannot change the archived field. Nothing was written. Upgrade the factory skill.";
  const max = stored.archived?.max_id;
  if (!Number.isInteger(max)) return null;
  const ids = new Set(stored.jobs.map((j) => j?.id));
  for (const j of data.jobs) {
    const m = typeof j?.id === "string" && !ids.has(j.id) && /^(\d{3,})-/.exec(j.id);
    if (m && Number(m[1]) <= max)
      return `a board write cannot add ${quoteId(j.id)}: ids up to ${max} belong to archived jobs. Nothing was written. Upgrade the factory skill.`;
  }
  return null;
}

const RIDER_CONTROLS = ["move", "priority", "paused", "close", "babysit", "resolve", "steer"];
const DASHBOARD_APPROVED_NOTE = "spec approved on the dashboard";
const TRACER_APPROVED_NOTE = "spec approved in Tracer";
const RECEIPT_PREFIX = "tracer-scope-approval/v2";
let receiptKeyMemo = { text: "", key: null };
const leadOfRow = (j) =>
  Array.isArray(j?.refs)
    ? (j.refs.find((r) => r?.kind === "batched-into" && typeof r.id === "string")?.id ?? null)
    : null;
const batchLeads = (j) =>
  Array.isArray(j?.refs)
    ? j.refs.filter((r) => r?.kind === "batched-into" && typeof r.id === "string").map((r) => r.id)
    : [];

// A rider follows its lead, so a write cannot add these dashboard controls to it.
// An error message, or null.
function checkRiderControls(storedJobs, jobs) {
  const stored = new Map();
  for (const j of storedJobs)
    if (typeof j?.id === "string" && !stored.has(j.id)) stored.set(j.id, j);
  for (const row of jobs) {
    if (typeof row !== "object" || row === null || typeof row.id !== "string") continue;
    const storedRow = stored.get(row.id);
    const lead = leadOfRow(row) ?? leadOfRow(storedRow);
    if (lead === null) continue;
    const added = RIDER_CONTROLS.find((f) => f in row && !(storedRow && f in storedRow));
    if (added)
      return `a board write cannot add ${added} to ${quoteIdCut(row.id, QUOTED_ID_MAX)}: it is batched into ${quoteIdCut(lead, QUOTED_ID_MAX)} and follows its lead. Act on the lead instead. Nothing was written.`;
  }
  return null;
}

// [ticket, feature] Tracer knows a row by, as factory.py tracer_ids: the Jira key, else
// FACTORY-<n> from the id's leading digits, else the id.
function tracerIds(row) {
  if (typeof row.jira === "string" && /^[A-Za-z][A-Za-z0-9]*-[1-9][0-9]*$/.test(row.jira))
    return [row.jira.toUpperCase(), row.jira.toLowerCase()];
  const m = /^(\d{1,7})-/.exec(row.id);
  if (m) return [`FACTORY-${Number(m[1])}`, `factory-${Number(m[1])}`];
  return [row.id.toUpperCase(), row.id];
}

// The Ed25519 public key Tracer signs scope approvals with, or null when none is configured.
async function receiptKey(env) {
  const text = env.FACTORY_TRACER_RECEIPT_KEY;
  if (typeof text !== "string") return null;
  if (receiptKeyMemo.text === text) return receiptKeyMemo.key;
  let key = null;
  try {
    const bytes = Uint8Array.from(atob(text), (c) => c.charCodeAt(0));
    if (bytes.length === 32)
      key = await crypto.subtle.importKey("raw", bytes, { name: "Ed25519" }, false, ["verify"]);
  } catch {
    key = null;
  }
  receiptKeyMemo = { text, key };
  return key;
}

// Whether the row carries a Tracer-signed scope receipt for its own job and ticket.
async function validReceipt(env, row) {
  const r = row.tracer?.receipt;
  if (typeof r !== "object" || r === null) return false;
  const line = (v) => typeof v === "string" && v.length >= 1 && v.length <= 64 && !v.includes("\n");
  if (![r.job, r.feature, r.ticket, r.approved_at].every(line)) return false;
  if (r.job !== row.id) return false;
  if (typeof r.signature !== "string" || !/^[A-Za-z0-9+/]{86}==$/.test(r.signature)) return false;
  const [ticket, feature] = tracerIds(row);
  if (r.feature !== feature || r.ticket !== ticket) return false;
  const key = await receiptKey(env);
  if (key === null) return false;
  const signature = Uint8Array.from(atob(r.signature), (c) => c.charCodeAt(0));
  const message = [RECEIPT_PREFIX, r.job, r.feature, r.ticket, r.approved_at].join("\n");
  return crypto.subtle.verify(
    { name: "Ed25519" },
    key,
    signature,
    new TextEncoder().encode(message),
  );
}

// Only a signed-in person passes the spec-approval gate on the dashboard, and the Tracer
// note needs the row's Tracer scope approved, plus a Tracer-signed receipt from a device.
// A row whose note is unchanged passes. A device cannot move a row into build while it
// keeps an approval note. An error message, or null.
async function checkApprovalNotes(env, storedJobs, jobs, principal) {
  const stored = new Map();
  for (const j of storedJobs)
    if (typeof j?.id === "string" && !stored.has(j.id)) stored.set(j.id, j);
  for (const row of jobs) {
    if (typeof row !== "object" || row === null || typeof row.id !== "string") continue;
    const storedRow = stored.get(row.id);
    if (row.note === storedRow?.note) {
      if (
        (row.note === DASHBOARD_APPROVED_NOTE || row.note === TRACER_APPROVED_NOTE) &&
        principal?.kind !== "user" &&
        row.stage === "build" &&
        storedRow.stage !== "build"
      )
        return `a board write from a device cannot move ${quoteIdCut(row.id, QUOTED_ID_MAX)} into build while it keeps the note "${row.note}": only a person starts an approved spec again. Nothing was written.`;
      continue;
    }
    if (row.note === DASHBOARD_APPROVED_NOTE && principal?.kind !== "user")
      return `a board write from a device cannot set the note "${DASHBOARD_APPROVED_NOTE}" on ${quoteIdCut(row.id, QUOTED_ID_MAX)}: only a signed-in person approves a spec on the dashboard. Nothing was written.`;
    if (row.note === TRACER_APPROVED_NOTE && row.tracer?.scope !== "approved")
      return `a board write cannot set the note "${TRACER_APPROVED_NOTE}" on ${quoteIdCut(row.id, QUOTED_ID_MAX)} unless its tracer.scope is "approved". Nothing was written.`;
    if (
      row.note === TRACER_APPROVED_NOTE &&
      principal?.kind !== "user" &&
      !(await validReceipt(env, row))
    )
      return `a board write from a device cannot set the note "${TRACER_APPROVED_NOTE}" on ${quoteIdCut(row.id, QUOTED_ID_MAX)} without a Tracer-signed scope receipt for ${tracerIds(row)[0]}. Nothing was written.`;
  }
  return null;
}

// Only a signed-in person sets resolve, the dashboard's Rework or Approve as is. Any writer
// may delete it. A row whose resolve is unchanged passes. An error message, or null.
function checkResolve(storedJobs, jobs, principal) {
  const stored = new Map();
  for (const j of storedJobs)
    if (typeof j?.id === "string" && !stored.has(j.id)) stored.set(j.id, j);
  for (const row of jobs) {
    if (typeof row !== "object" || row === null || typeof row.id !== "string") continue;
    const r = row.resolve;
    if (r === undefined || JSON.stringify(r) === JSON.stringify(stored.get(row.id)?.resolve))
      continue;
    const id = quoteIdCut(row.id, QUOTED_ID_MAX);
    if (principal?.kind !== "user")
      return `a board write from a device cannot set resolve on ${id}: only a signed-in person asks for rework or approval on the dashboard. Nothing was written.`;
    const keys =
      r && typeof r === "object" && !Array.isArray(r) ? Object.keys(r).sort().join() : "";
    const note = typeof r?.note === "string" ? r.note.trim().length : 0;
    const wellFormed =
      typeof r?.at === "string" &&
      ((r.action === "rework" && keys === "action,at,note" && note >= 1 && note <= 2000) ||
        (r.action === "approve" && keys === "action,at"));
    if (!wellFormed)
      return `resolve on ${id} must be {action: "rework", note, at} or {action: "approve", at}. Nothing was written.`;
  }
  return null;
}

const STEER_STAGES = ["spec", "build", "review"];
const STEER_MAX = 10;
const STEER_AT = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/;

// Only a signed-in person appends a steering note, one {note, at} per write, while the stored
// row is at spec, build or review. A row whose steer is unchanged passes. A row that omits steer
// passes only when no note is stored. An error message, or null.
function checkSteer(storedJobs, jobs, principal) {
  const stored = new Map();
  for (const j of storedJobs)
    if (typeof j?.id === "string" && !stored.has(j.id)) stored.set(j.id, j);
  for (const row of jobs) {
    if (typeof row !== "object" || row === null || typeof row.id !== "string") continue;
    const s = row.steer;
    const storedRow = stored.get(row.id);
    if (s === undefined) {
      if (Array.isArray(storedRow?.steer) && storedRow.steer.length > 0)
        return `a board write cannot drop the steering notes stored on ${quoteIdCut(row.id, QUOTED_ID_MAX)}: send the row with its steer as stored. Nothing was written.`;
      continue;
    }
    if (JSON.stringify(s) === JSON.stringify(storedRow?.steer)) continue;
    const id = quoteIdCut(row.id, QUOTED_ID_MAX);
    if (principal?.kind !== "user")
      return `a board write from a device cannot change steer on ${id}: only a signed-in person adds a steering note on the dashboard. Nothing was written.`;
    const kept = Array.isArray(storedRow?.steer) ? storedRow.steer : [];
    const added = Array.isArray(s) ? s[s.length - 1] : undefined;
    const wellFormed =
      Array.isArray(s) &&
      STEER_STAGES.includes(storedRow?.stage) &&
      s.length === kept.length + 1 &&
      s.length <= STEER_MAX &&
      JSON.stringify(s.slice(0, -1)) === JSON.stringify(kept) &&
      added !== null &&
      typeof added === "object" &&
      Object.keys(added).sort().join() === "at,note" &&
      typeof added.at === "string" &&
      STEER_AT.test(added.at) &&
      typeof added.note === "string" &&
      added.note.trim().length >= 1;
    if (!wellFormed)
      return `steer on ${id} must keep its stored notes and add one {note, at} while the job is at spec, build or review, at most 10 notes. Nothing was written.`;
    const text = checkOperatorText(added.note, "note");
    if (text) return `steer on ${id} ${text}. Nothing was written.`;
  }
  return null;
}

// Only POST /api/trials/resume clears a stage-note trial pause. Every stored unresumed revert
// must be written unchanged, and no written record may carry resumed or resumed_by unless it
// equals a stored record. An error message, or null.
function checkTrials(storedDoc, data) {
  const trials = (doc) => (Array.isArray(doc?.trials) ? doc.trials : []);
  const label = (t, i) =>
    typeof t?.id === "string" ? quoteIdCut(t.id, QUOTED_ID_MAX) : `at trials[${i}]`;
  const stored = trials(storedDoc).map(canonicalJson),
    written = trials(data);
  const writtenJson = new Set(written.map(canonicalJson));
  for (const [i, t] of trials(storedDoc).entries())
    if (
      typeof t === "object" &&
      t !== null &&
      t.status === "reverted" &&
      !t.resumed &&
      !writtenJson.has(stored[i])
    )
      return `a board write cannot change or drop the unresumed reverted trial ${label(t, i)}: only POST /api/trials/resume clears a stage-note trial pause. Nothing was written.`;
  for (const [i, t] of written.entries())
    if (
      typeof t === "object" &&
      t !== null &&
      ("resumed" in t || "resumed_by" in t) &&
      !stored.includes(canonicalJson(t))
    )
      return `a board write cannot set resumed or resumed_by on trial ${label(t, i)}: only POST /api/trials/resume clears a stage-note trial pause. Nothing was written.`;
  return null;
}

// The dashboard's Remove from batch: a rider that is not stacked, stored or written, goes from spec to idea and
// drops its one ref, while its lead is at spec both stored and written.
const unbatchedAtSpec = (storedRow, row, storedLead, lead) =>
  batchLeads(storedRow).length === 1 &&
  batchLeads(row).length === 0 &&
  storedRow.stage === "spec" &&
  row.stage === "idea" &&
  storedRow.stacked !== true &&
  row.stacked !== true &&
  storedLead?.stage === "spec" &&
  lead?.stage === "spec";

// Only factory.py batch adds a batched-into ref, into a lead stored at spec or new in this write.
// Only the lead's close or merge drops one or deletes its rider row, besides the dashboard's
// Remove from batch while the lead is at spec. An error message, or null.
function checkBatchRefs(storedJobs, jobs) {
  const firstById = (rows) => {
    const byId = new Map();
    for (const j of rows) if (typeof j?.id === "string" && !byId.has(j.id)) byId.set(j.id, j);
    return byId;
  };
  const stored = firstById(storedJobs);
  const written = firstById(jobs);
  for (const row of jobs) {
    if (typeof row?.id !== "string") continue;
    const storedRow = stored.get(row.id);
    const before = batchLeads(storedRow);
    const after = batchLeads(row);
    const dropped = before.find((id) => !after.includes(id));
    if (dropped !== undefined) {
      const lead = written.get(dropped);
      if (
        lead &&
        lead.stage !== "closed" &&
        lead.stage !== "merged" &&
        !unbatchedAtSpec(storedRow, row, stored.get(dropped), lead)
      )
        return `a board write cannot unbatch ${quoteIdCut(row.id, QUOTED_ID_MAX)}: its lead ${quoteIdCut(dropped, QUOTED_ID_MAX)} is still open. Only the lead's close or merge drops the ref. Nothing was written.`;
    }
    const added = after.find((id) => !before.includes(id));
    if (
      added !== undefined &&
      !(
        before.length === 0 &&
        after.length === 1 &&
        row.stage === "spec" &&
        (!storedRow || storedRow.stage === "idea") &&
        written.get(added)?.stage === "spec" &&
        (!stored.get(added) || stored.get(added).stage === "spec")
      )
    )
      return `a board write cannot batch ${quoteIdCut(row.id, QUOTED_ID_MAX)} into ${quoteIdCut(added, QUOTED_ID_MAX)}: factory.py batch does that, while the lead is at spec. Nothing was written.`;
  }
  for (const [id, storedRow] of stored) {
    if (written.has(id)) continue;
    for (const leadId of batchLeads(storedRow)) {
      const lead = written.get(leadId);
      if (lead && lead.stage !== "closed" && lead.stage !== "merged")
        return `a board write cannot delete ${quoteIdCut(id, QUOTED_ID_MAX)}: its lead ${quoteIdCut(leadId, QUOTED_ID_MAX)} is still open. Close or merge the lead first. Nothing was written.`;
    }
  }
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
    const head = await env.BUCKET.head(`idea-images/${name}`);
    if (head?.customMetadata?.sha256 !== name.slice(0, 64))
      return `idea image ${name} is not uploaded`;
    if (head.uploaded.getTime() <= Date.now() - IDEA_REF_FRESH_MS)
      return `idea image ${name} is not uploaded: the upload is over a day old, so upload it again`;
  }
  return null;
}

// An error message, or null, for the idea_files field of a board write, checked like
// idea_images: only names the stored board doesn't already carry are looked up.
async function checkIdeaFiles(env, jobs, storedJobs) {
  const wanted = new Set();
  for (const j of jobs) {
    if (typeof j !== "object" || j === null || !("idea_files" in j)) continue;
    const names = j.idea_files;
    if (
      !Array.isArray(names) ||
      names.length < 1 ||
      names.length > MAX_IDEA_FILES ||
      new Set(names).size !== names.length ||
      !names.every((n) => IDEA_FILE.test(n))
    ) {
      return "idea_files needs 1-4 distinct <sha256>.<pdf|csv|txt|mp4|webm> names";
    }
    names.forEach((n) => wanted.add(n));
  }
  if (!wanted.size) return null;
  try {
    for (const j of storedJobs) for (const n of j.idea_files ?? []) wanted.delete(n);
  } catch {} // an unreadable stored row vouches for nothing
  if (wanted.size > MAX_NEW_REFS) return "too many new idea files in one board write";
  for (const name of wanted) {
    const head = await env.BUCKET.head(`idea-files/${name}`);
    if (head?.customMetadata?.sha256 !== name.slice(0, 64))
      return `idea file ${name} is not uploaded`;
    if (head.uploaded.getTime() <= Date.now() - IDEA_REF_FRESH_MS)
      return `idea file ${name} is not uploaded: the upload is over a day old, so upload it again`;
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

// Create-only first. A repeat upload rewrites the same bytes to reset the idea upload sweep clock.
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
  const options = {
    httpMetadata: { contentType: "image/png" },
    customMetadata: {
      sha256: hex,
      width: String(w),
      height: String(h),
      length: String(body.length),
    },
  };
  const key = `idea-images/${name}`;
  const onlyIf = new Headers({ "If-None-Match": "*" });
  if (!(await env.BUCKET.put(key, body, { ...options, onlyIf })))
    await env.BUCKET.put(key, body, options);
  return reply("ok");
}

const ascii = (b, at, text) => [...text].every((c, i) => b[at + i] === c.charCodeAt(0));

// Whether the body starts like its type. CSV and text must be UTF-8 with no NUL byte.
function ideaFileFormatOk(ext, b) {
  if (ext === "pdf") return ascii(b, 0, "%PDF-");
  if (ext === "mp4") return b.length >= 12 && ascii(b, 4, "ftyp");
  if (ext === "webm")
    return (
      [0x1a, 0x45, 0xdf, 0xa3].every((v, i) => b[i] === v) &&
      Array.from({ length: 61 }, (_, i) => ascii(b, i, "webm")).some(Boolean)
    );
  if (b.includes(0)) return false;
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(b);
    return true;
  } catch {
    return false;
  }
}

// Create-only first, like ideaImagePut. A repeat upload rewrites the same bytes to reset the
// idea upload sweep clock.
async function ideaFilePut(request, env, name) {
  const ext = name.slice(65);
  const { type, max, label } = IDEA_FILE_TYPES[ext];
  if (request.headers.get("Content-Type") !== type)
    return reply(`Content-Type must be ${type}`, 415);
  const body = await readCapped(request, max);
  if (body === null) return reply(`${label} over ${max >> 20} MiB`, 413);
  if (!body.length) return reply("empty file", 400);
  if (!ideaFileFormatOk(ext, body))
    return reply(`the body does not match the ${label} format`, 415);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", body));
  const hex = Array.from(digest, (x) => x.toString(16).padStart(2, "0")).join("");
  if (`${hex}.${ext}` !== name) return reply("SHA-256 does not match the file name", 400);
  const options = {
    httpMetadata: { contentType: type },
    customMetadata: { sha256: hex, length: String(body.length) },
  };
  const key = `idea-files/${name}`;
  const onlyIf = new Headers({ "If-None-Match": "*" });
  if (!(await env.BUCKET.put(key, body, { ...options, onlyIf })))
    await env.BUCKET.put(key, body, options);
  return reply("ok");
}

const MAX_PUSH_BODY = 4 << 10;
const MAX_PUSH_DEVICES = 20;
const pushRecordKey = async (endpoint) =>
  `push/${await sha256(new TextEncoder().encode(endpoint))}.json`;

function pushKeyGet(env) {
  const key = pushKey(env);
  return key ? reply(JSON.stringify({ key }), 200, "application/json") : notFound();
}

// One record per browser, keyed by its endpoint's hash, so a repeat subscribe replaces it.
async function pushSubscribe(request, env) {
  const body = await readJson(request, MAX_PUSH_BODY);
  if (body instanceof Response) return body;
  const record = validSubscription(body);
  if (typeof record === "string") return reply(record, 400);
  const key = await pushRecordKey(record.endpoint);
  const { objects } = await env.BUCKET.list({ prefix: "push/" });
  if (objects.length >= MAX_PUSH_DEVICES && !objects.some((o) => o.key === key))
    return reply(`push device limit reached (${MAX_PUSH_DEVICES})`, 409);
  await env.BUCKET.put(key, JSON.stringify(record), JSON_TYPE);
  return reply(null, 204);
}

async function pushUnsubscribe(request, env) {
  const body = await readJson(request, MAX_PUSH_BODY);
  if (body instanceof Response) return body;
  if (typeof body?.endpoint !== "string") return reply("body needs an endpoint", 400);
  await env.BUCKET.delete(await pushRecordKey(body.endpoint));
  return reply(null, 204);
}

async function api(request, env, path, principal, ctx) {
  const method = request.method;
  if (path.startsWith("/api/pipeline/retro/")) {
    const p = ("/api/pipeline/" + path.slice("/api/pipeline/retro/".length)).match(PIPELINE);
    if (!p || method !== "PUT") return notFound();
    if (p[2] !== undefined && !(await env.BUCKET.head(`projects/${p[2]}.md`))) return notFound();
    return pipelineRetroPut(request, env, principal, p[1]);
  }
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
        ? historyReply(env, principal, `pipeline-history/${p[1]}/`, pipelineKey(p[1]))
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
  m = path.match(RETRO_DAY);
  if (m) {
    if (method !== "GET" || !isCalendarDate(m[1])) return notFound();
    if (principal.kind !== "service") return reply(RETRO_READ_ONLY, 403);
    return retroDay(env, m[1]);
  }
  m = path.match(RETRO_FILE);
  if (m) {
    const [, id, date] = m;
    if (!HOST_NAME.test(id) || !isCalendarDate(date)) return notFound();
    if (method === "PUT") return retroPut(request, env, principal, id, date);
    if (method !== "GET") return notFound();
    if (principal.kind !== "service") return reply(RETRO_READ_ONLY, 403);
    return getFile(env, retroKey(id, date), "application/x-ndjson");
  }
  if (AUDIT_FLAGS.test(path)) return method === "GET" ? auditFlagsGet(env) : notFound();
  if (AUDIT_FLAGS_ACK.test(path))
    return method === "POST" ? auditFlagsAck(request, env, principal) : notFound();
  if (AUDIT_FLAGS_BACKLOG_PATH.test(path))
    return method === "POST" ? auditFlagsBacklog(request, env, principal) : notFound();
  m = path.match(AUDIT_JOB);
  if (m) {
    if (method !== "GET" || !AUDIT_JOB_ID.test(m[1])) return notFound();
    return auditGet(env, m[1]);
  }
  m = path.match(AUDIT_FILE);
  if (m) {
    const [, job, id] = m;
    if (method !== "PUT" || !AUDIT_JOB_ID.test(job) || !HOST_NAME.test(id)) return notFound();
    return auditPut(request, env, principal, job, id);
  }
  m = path.match(RETROS);
  if (m) {
    if (m[1] === undefined) return method === "GET" ? retrosList(env) : notFound();
    return method === "PUT" ? retrosPut(request, env, principal, m[1]) : notFound();
  }
  m = path.match(IDEA_IMAGE_ROUTE);
  if (m) {
    if (!IDEA_IMAGE.test(m[1])) return notFound();
    if (method === "GET") return getFile(env, `idea-images/${m[1]}`, "image/png", IMAGE_HEADERS);
    if (method === "PUT") return ideaImagePut(request, env, m[1]);
    return notFound();
  }
  m = path.match(IDEA_FILE_ROUTE);
  if (m) {
    if (!IDEA_FILE.test(m[1])) return notFound();
    const name = m[1];
    if (method === "GET" && /\.(mp4|webm)$/.test(name)) return ideaVideoGet(request, env, name);
    if (method === "GET")
      return getFile(
        env,
        `idea-files/${name}`,
        IDEA_FILE_TYPES[name.slice(65)].type,
        FILE_HEADERS(name),
      );
    if (method === "PUT") return ideaFilePut(request, env, name);
    return notFound();
  }
  if (path === "/api/backlog/lesson" && method === "POST")
    return backlogLesson(request, env, principal);
  if (path === "/api/backlog/triage/rate" && method === "POST")
    return backlogTriageRate(request, env);
  if (path === "/api/backlog/triage/batch" && method === "POST")
    return backlogTriageBatch(request, env);
  if (path === "/api/backlog/triage/verdicts" && method === "POST")
    return backlogTriageVerdicts(request, env);
  if (path === "/api/backlog/triage" && method === "POST") return backlogTriage(request, env);
  if (path === "/api/backlog/triage" && method === "GET") return backlogTriageGet(request, env);
  if (path === "/api/review/escalation" && method === "POST") return reviewEscalation(request, env);
  if (path === "/api/board/archive")
    return method === "GET" ? archiveGet(request, env) : notFound();
  if (path === "/api/spend")
    return method === "GET" ? spendGet(env, await weeklyBudgets(env)) : notFound();
  if (path === "/api/push/key" || path === "/api/push/subscriptions") {
    if (principal.kind !== "user") return notFound();
    if (path === "/api/push/key") return method === "GET" ? pushKeyGet(env) : notFound();
    if (method === "PUT") return pushSubscribe(request, env);
    if (method === "DELETE") return pushUnsubscribe(request, env);
    return notFound();
  }
  if (path === "/api/halt") {
    if (method === "GET") return haltGet(env, principal);
    if (method === "PUT") return haltPut(request, env, principal);
    return notFound();
  }
  m = path.match(TRIALS_RESUME);
  if (m) {
    if (method !== "POST") return notFound();
    if (!(await env.BUCKET.head(`projects/${m[1]}.md`))) return notFound();
    return trialsResume(request, env, principal, m[1]);
  }
  if (path === "/api/board" || path === "/api/backlog") {
    const [key, type] =
      path === "/api/board" ? ["board.json", "application/json"] : ["backlog.md", "text/markdown"];
    if (method === "GET") return getFile(env, key, type);
    if (method === "PUT") return casPut(request, env, key, type, principal, ctx);
    if (method === "PATCH" && key === "board.json") return boardPatch(request, env, principal, ctx);
    return notFound();
  }
  m = path.match(PROJECT_RETRO);
  if (m) {
    const [, slug, name] = m;
    if (method !== "PUT" || !PROJECT_FILE_NAMES.includes(name)) return notFound();
    if (!(await env.BUCKET.head(`projects/${slug}.md`))) return notFound();
    return projectRetroPut(request, env, principal, slug, name);
  }
  m = path.match(PROJECT_HISTORY);
  if (m) {
    const [, slug, name, n] = m;
    if (method !== "GET" || !PROJECT_FILE_NAMES.includes(name)) return notFound();
    if (!(await env.BUCKET.head(`projects/${slug}.md`))) return notFound();
    return n === undefined
      ? historyReply(env, principal, `project-history/${slug}/${name}/`, projectFileKey(slug, name))
      : projectHistoryVersion(env, slug, name, Number(n));
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
    return method === "GET"
      ? getFile(env, projectFileKey(m[1], m[2]), "text/markdown")
      : projectDocumentPut(request, env, principal, m[1], m[2]);
  }
  m = path.match(PROJECT);
  if (m) {
    if (method === "GET") return getFile(env, projectFileKey(m[1], "project.md"), "text/markdown");
    if (method === "PUT") return projectPut(request, env, principal, m[1]);
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
    if (m[3] === "needs-triage.json")
      return method === "GET"
        ? getFile(env, needsTriageKey(m[1], m[2]), "application/json")
        : notFound();
    if (!JOB_REL.test(m[3])) return notFound();
    if (method === "GET") return getJobFile(request, env, principal, prefix + m[3]);
    if (method === "PUT") return jobPut(request, env, prefix + m[3], ctx);
  }
  return notFound();
}

export default {
  async fetch(request, env, ctx) {
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
    return serveFactory(request, { ...env, config }, principal, ctx);
  },
  scheduled(controller, env, ctx) {
    ctx.waitUntil(runArchive(env, new Date(controller.scheduledTime)));
    ctx.waitUntil(pruneRetro(env, new Date(controller.scheduledTime)));
    ctx.waitUntil(pruneRetroRuns(env, new Date(controller.scheduledTime)));
    ctx.waitUntil(pruneIdeaUploads(env, new Date(controller.scheduledTime)));
    ctx.waitUntil(pushHourly(env, new Date(controller.scheduledTime)).catch(() => {}));
  },
};

/** @param {Request} request
 * @param {object} env
 * @param {import('./access.ts').VerifiedPrincipal} principal
 * @param {ExecutionContext} ctx
 */
export async function serveFactory(request, env, principal, ctx) {
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
    return connectEvents(request, env, principal.exp);
  }
  if (path.startsWith("/api/")) return api(request, env, path, principal, ctx);
  if (request.method !== "GET") return notFound();
  if (path === "/") return renderDashboard(env.config);
  if (path === "/dashboard.html")
    return reply(null, 302, "text/plain", { Location: "/" + new URL(request.url).search });
  if (path.startsWith("/assets/") || path === "/sw.js") return serveAsset(request, env.config);
  if (path === "/board.json")
    return new URL(request.url).searchParams.get("view") === "slim"
      ? slimBoardGet(env, ctx)
      : getFile(env, "board.json", "application/json", {
          "X-Factory-Version": DASHBOARD_VERSION,
        });
  if (path === "/projects.json") return projects(env);
  if (JOB_FILE.test(path)) return getJobFile(request, env, principal, path.slice(1));
  return notFound();
}
