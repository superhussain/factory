// The hourly archive pass and its read API. The pass moves old finished jobs from
// board.json into board-archive.json, writing the archive first so a lost board race
// leaves copies, never a loss. Reads hide any archive row whose id is still on the board.
import { validatePipeline } from "./admission.js";
import { announce } from "./events.js";

export const ARCHIVE_KEY = "board-archive.json";
export const ARCHIVE_PAGE = 50;
export const ARCHIVE_CRON = "17 * * * *";

const DAY = 86400000;
const ID_PREFIX = /^(\d{3,})-/;
const terminal = (job) => job?.stage === "merged" || job?.stage === "closed";
const doneAt = (job) => Date.parse(job?.done);
const newestFirst = (a, b) =>
  doneAt(b) - doneAt(a) || (String(a.id) < String(b.id) ? 1 : String(a.id) > String(b.id) ? -1 : 0);

/** Whether a row may leave the board: finished, and nothing on the device still needs it. */
export function archivable(job, tracerOff = false) {
  if (!terminal(job) || !Number.isFinite(doneAt(job))) return false;
  if (job.claim || job.needs || job.automerge) return false;
  if (job.escalated === true || /^escalated:/i.test(String(job.note ?? ""))) return false;
  if (job.build?.state === "queued" || job.build?.state === "running") return false;
  const backlog = job.backlog;
  if (
    job.stage === "closed" &&
    backlog?.decision === "denied" &&
    typeof backlog.notes === "string" &&
    backlog.notes.trim() &&
    !("notes_read" in backlog)
  )
    return false;
  return tracerOff || !(job.tracer?.follow === true && !job.tracer.url);
}

/** The Set of ids to move for a validated `archive` setting at Date `now`. */
export function planArchive(jobs, setting, now, tracerOff = false) {
  if (setting.mode === "age") {
    const cutoff = now.getTime() - setting.n * DAY;
    return new Set(
      jobs.filter((j) => archivable(j, tracerOff) && doneAt(j) < cutoff).map((j) => j.id),
    );
  }
  const finished = jobs.filter((j) => terminal(j) && Number.isFinite(doneAt(j))).sort(newestFirst);
  return new Set(
    finished
      .slice(setting.n)
      .filter((j) => archivable(j, tracerOff))
      .map((j) => j.id),
  );
}

/** Archive rows not back on the board plus the moved rows, newest finished first. */
export function mergeArchive(archiveJobs, moved, boardIds) {
  return [...archiveJobs.filter((j) => !boardIds.has(j?.id)), ...moved].sort(newestFirst);
}

/** The board's `archived` field once `moved` rows have left it. */
export function nextArchived(prev, moved) {
  let maxId = Number.isInteger(prev?.max_id) ? prev.max_id : 0;
  const fingerprints =
    typeof prev?.fingerprints === "object" && prev.fingerprints !== null
      ? { ...prev.fingerprints }
      : {};
  for (const job of moved) {
    const m = ID_PREFIX.exec(job.id);
    if (m) maxId = Math.max(maxId, Number(m[1]));
    if (typeof job.origin?.fingerprint === "string") fingerprints[job.origin.fingerprint] = job.id;
  }
  return { max_id: maxId, fingerprints };
}

// The parsed object and its etag, or null when the key is missing or not JSON.
async function readJson(env, key) {
  const object = await env.BUCKET.get(key);
  if (!object) return null;
  try {
    return { data: JSON.parse(await object.text()), etag: object.httpEtag };
  } catch {
    return null;
  }
}

const jobsOf = (doc) => (Array.isArray(doc?.data?.jobs) ? doc.data.jobs : null);

// Log why a write was refused so wrangler tail shows it. Key names only.
const conflict = (reason) => {
  console.warn(`archive pass conflict: ${reason}`);
  return { status: "conflict", moved: [] };
};

/** One archive pass. Off unless a valid global pipeline document sets `archive`. */
export async function runArchive(env, now) {
  const pipeline = await readJson(env, "pipeline/global.json");
  if (!pipeline || validatePipeline(pipeline.data) || !pipeline.data.archive)
    return { status: "off", moved: [] };
  const board = await readJson(env, "board.json");
  const jobs = jobsOf(board);
  if (!jobs) return { status: "none", moved: [] };
  const archive = await readJson(env, ARCHIVE_KEY);
  const archiveJobs = jobsOf(archive) ?? [];
  const ids = planArchive(jobs, pipeline.data.archive, now, pipeline.data.tracer?.enabled !== true);
  const boardIds = new Set(jobs.map((j) => j?.id));
  const stale = archiveJobs.some((j) => boardIds.has(j?.id));
  if (!ids.size && !stale) return { status: "none", moved: [] };
  const moved = jobs.filter((j) => ids.has(j.id));
  const saved = await env.BUCKET.put(
    ARCHIVE_KEY,
    JSON.stringify({ jobs: mergeArchive(archiveJobs, moved, boardIds) }),
    {
      onlyIf: new Headers(archive ? { "If-Match": archive.etag } : { "If-None-Match": "*" }),
      httpMetadata: { contentType: "application/json" },
    },
  );
  if (!saved) {
    return conflict(
      archive
        ? "board-archive.json changed during the pass; nothing moved"
        : "board-archive.json is not valid JSON or appeared during the pass; nothing moved",
    );
  }
  if (!ids.size) return { status: "none", moved: [] };
  const next = {
    ...board.data,
    jobs: jobs.filter((j) => !ids.has(j.id)),
    archived: nextArchived(board.data.archived, moved),
  };
  const written = await env.BUCKET.put("board.json", JSON.stringify(next, null, 2) + "\n", {
    onlyIf: new Headers({ "If-Match": board.etag }),
    httpMetadata: { contentType: "application/json" },
  });
  if (!written) {
    return conflict(
      "board.json changed during the pass; rows stay on the board until the next pass",
    );
  }
  await announce(env, "board.json");
  return { status: "archived", moved: [...ids] };
}

const reply = (body, status = 200, type = "text/plain") =>
  new Response(typeof body === "string" ? body : JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": `${type}; charset=utf-8`,
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
    },
  });

/** GET /api/board/archive with exactly one of ?id=, ?page= or ?q=. */
export async function archiveGet(request, env) {
  const params = new URL(request.url).searchParams;
  const given = ["id", "page", "q"].filter((name) => params.has(name));
  if (given.length > 1) return reply("use one of id, page or q", 400);
  const page = params.get("page") ?? "1";
  if (!/^[1-9]\d*$/.test(page)) return reply("page: must be a whole number from 1", 400);
  const q = params.get("q");
  if (q !== null && ([...q].length < 1 || [...q].length > 200))
    return reply("q: must be 1 to 200 characters", 400);
  const boardIds = new Set((jobsOf(await readJson(env, "board.json")) ?? []).map((j) => j?.id));
  const jobs = (jobsOf(await readJson(env, ARCHIVE_KEY)) ?? []).filter((j) => !boardIds.has(j?.id));
  const json = (body) => reply(body, 200, "application/json");
  if (params.has("id")) {
    const row = jobs.find((j) => j?.id === params.get("id"));
    return row ? json({ jobs: [row] }) : reply("not found", 404);
  }
  if (q !== null) {
    const needle = q.toLowerCase();
    const matches = jobs.filter((j) =>
      [j?.id, j?.title, j?.project].some(
        (v) => typeof v === "string" && v.toLowerCase().includes(needle),
      ),
    );
    return json({ jobs: matches.slice(0, ARCHIVE_PAGE), total: matches.length });
  }
  const n = Number(page);
  return json({
    jobs: jobs.slice((n - 1) * ARCHIVE_PAGE, n * ARCHIVE_PAGE),
    page: n,
    pages: Math.ceil(jobs.length / ARCHIVE_PAGE),
    total: jobs.length,
  });
}
