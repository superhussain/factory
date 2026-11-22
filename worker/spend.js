// GET /api/spend: finished jobs rolled up by project and UTC week into tokens per merged
// PR and a revert count. Read only, from board.json plus archive rows not on the board.
import { ARCHIVE_KEY, jobsOf, readJson, reply, terminal } from "./archive.js";

export const SPEND_WEEKS = 8;
export const SPEND_SCHEMA = "factory.spend/1";

const DAY = 86400000;
const WEEK = 7 * DAY;
const COUNTED = ["input", "output", "cache_write"];
const RESULTS = new Set(["held", "reworked", "reverted"]);
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/** Input, output and cache write over every host and model; a malformed count reads 0. */
export function jobTokens(job) {
  let total = 0;
  if (!isObject(job?.tokens)) return 0;
  for (const host of Object.values(job.tokens)) {
    if (!isObject(host?.models)) continue;
    for (const counts of Object.values(host.models)) {
      if (!isObject(counts)) continue;
      for (const kind of COUNTED) {
        const n = counts[kind];
        if (Number.isSafeInteger(n) && n >= 0) total += n;
      }
    }
  }
  return total;
}

/** The ms of the Monday 00:00 UTC on or before `ms`. */
export function weekStart(ms) {
  const day = Math.floor(ms / DAY) * DAY;
  return day - ((new Date(day).getUTCDay() + 6) % 7) * DAY;
}

/** Spend rows for the last SPEND_WEEKS weeks at Date `now`, by project then newest week. */
export function spendRows(jobs, now) {
  const cutoff = weekStart(now.getTime()) - (SPEND_WEEKS - 1) * WEEK;
  const groups = new Map();
  for (const job of jobs) {
    const done = Date.parse(job?.done);
    if (!terminal(job) || typeof job.project !== "string" || !job.project) continue;
    if (!(done >= cutoff)) continue;
    const week = new Date(weekStart(done)).toISOString().slice(0, 10);
    const key = JSON.stringify([job.project, week]);
    let group = groups.get(key);
    if (!group)
      groups.set(key, (group = { project: job.project, week, tokens: 0, prs: new Map() }));
    group.tokens += jobTokens(job);
    if (job.stage !== "merged") continue;
    const pr = typeof job.pr === "string" ? job.pr : job.id;
    const result = RESULTS.has(job.outcome?.result) ? job.outcome.result : null;
    if (!group.prs.has(pr) || group.prs.get(pr) === null) group.prs.set(pr, result);
  }
  return [...groups.values()]
    .sort((a, b) =>
      a.project < b.project ? -1 : a.project > b.project ? 1 : a.week < b.week ? 1 : -1,
    )
    .map(({ project, week, tokens, prs }) => {
      const results = [...prs.values()];
      const count = (r) => results.filter((x) => x === r).length;
      return {
        project,
        week,
        merged_prs: prs.size,
        tokens,
        tokens_per_pr: prs.size ? Math.round(tokens / prs.size) : null,
        judged: results.filter((r) => r !== null).length,
        reverted: count("reverted"),
        reworked: count("reworked"),
      };
    });
}

/** GET /api/spend. */
export async function spendGet(env, now = new Date()) {
  const board = jobsOf(await readJson(env, "board.json")) ?? [];
  const boardIds = new Set(board.map((j) => j?.id));
  const archive = (jobsOf(await readJson(env, ARCHIVE_KEY)) ?? []).filter(
    (j) => !boardIds.has(j?.id),
  );
  return reply(
    { schema: SPEND_SCHEMA, weeks: SPEND_WEEKS, rows: spendRows([...board, ...archive], now) },
    200,
    "application/json",
  );
}
