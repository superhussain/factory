// GET /api/spend: finished jobs rolled up by project and UTC week into tokens per merged
// PR and a revert count. Read only, from board.json plus archive rows not on the board.
import { jobsOf, readHourly, readJson, reply, terminal } from "./archive.js";

export const SPEND_WEEKS = 8;
export const SPEND_SCHEMA = "factory.spend/1";
export const SPEND_THRESHOLDS = [50, 80, 100];
export const NUDGE_KEY = "spend-nudges.json";

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

const weekOf = (now) => new Date(weekStart(now.getTime())).toISOString().slice(0, 10);

/** Spend rows from board.json plus archive rows not on the board, at Date `now`, from
 * readHourly's read, or a read of its own. */
export async function readSpendRows(env, now, hourly = readHourly(env)) {
  const read = await hourly;
  const board = jobsOf(read.board) ?? [];
  const boardIds = new Set(board.map((j) => j?.id));
  const archive = (jobsOf(read.archive) ?? []).filter((j) => !boardIds.has(j?.id));
  return spendRows([...board, ...archive], now);
}

/** This week's tokens against each project's weekly_tokens in `budgets`, sorted by project. */
export function spendExpected(rows, budgets, now) {
  const week = weekOf(now);
  return [...budgets]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([project, weekly_tokens]) => {
      const tokens = rows.find((r) => r.project === project && r.week === week)?.tokens ?? 0;
      return {
        project,
        weekly_tokens,
        tokens,
        percent: Math.floor((tokens * 100) / weekly_tokens),
      };
    });
}

/** One event per project for the highest newly crossed threshold, and the next stored state.
 * A state from another week starts empty. */
export function spendNudges(expected, week, state) {
  const sent = state?.week === week && isObject(state.sent) ? state.sent : {};
  // A null prototype keeps a project named __proto__ an ordinary key.
  const next = {
    schema: "factory.spend-nudges/1",
    week,
    sent: Object.assign(Object.create(null), sent),
  };
  const n = (v) => v.toLocaleString("en-US");
  const events = [];
  for (const { project, weekly_tokens, tokens, percent } of expected) {
    const done = Object.hasOwn(sent, project) && Array.isArray(sent[project]) ? sent[project] : [];
    const crossed = SPEND_THRESHOLDS.filter((t) => t <= percent && !done.includes(t));
    if (!crossed.length) continue;
    next.sent[project] = [...done, ...crossed];
    events.push({
      kind: "spend",
      id: project,
      threshold: crossed.at(-1),
      title: `${n(tokens)} of ${n(weekly_tokens)} expected weekly tokens (${percent}%), week of ${week}`,
    });
  }
  return { events, state: next };
}

/** The spend events at Date `at` and the state to record once they are delivered. */
export async function spendNudgePass(env, budgets, at, hourly) {
  const expected = spendExpected(await readSpendRows(env, at, hourly), budgets, at);
  return spendNudges(expected, weekOf(at), (await readJson(env, NUDGE_KEY))?.data);
}

/** Stores the spend nudge state after a delivered push. */
export async function recordSpendNudges(env, state) {
  await env.BUCKET.put(NUDGE_KEY, JSON.stringify(state));
}

/** GET /api/spend. */
export async function spendGet(env, budgets = new Map(), now = new Date()) {
  const rows = await readSpendRows(env, now);
  return reply(
    {
      schema: SPEND_SCHEMA,
      weeks: SPEND_WEEKS,
      rows,
      week: weekOf(now),
      expected: spendExpected(rows, budgets, now),
    },
    200,
    "application/json",
  );
}
