// Live slim board CPU run: tails the deployed Worker for n minutes and prints one aggregate row
// and a share line. Tail output stays in memory and is never printed. Never part of CI. Usage:
// node tests/live/slim-cpu.mjs --worker <name> --minutes <n> with CLOUDFLARE_API_TOKEN and
// CLOUDFLARE_ACCOUNT_ID in the environment.
import { spawn } from "node:child_process";

export const MISS_MS = 5;
export const PAIR_MS = 2000;
export const SHARE_MS = 10;
export const MIN_MISSES = 5;

// Splits concatenated, possibly pretty-printed JSON objects into parsed records.
export function splitRecords(text) {
  const out = [];
  let depth = 0;
  let start = 0;
  let inString = false;
  let escaped = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (inString) {
      if (escaped) escaped = false;
      else if (c === "\\") escaped = true;
      else if (c === '"') inString = false;
    } else if (c === '"') inString = true;
    else if (c === "{") {
      if (depth === 0) start = i;
      depth++;
    } else if (c === "}" && --depth === 0) out.push(JSON.parse(text.slice(start, i + 1)));
  }
  return out;
}

export function summarize(events) {
  const reads = events.filter((e) => new URL(e.event.request.url).pathname === "/board.json");
  const misses = reads.filter((e) => e.cpuTime >= MISS_MS);
  const missMs = misses.map((e) => e.cpuTime).sort((a, b) => a - b);
  const mid = missMs.length >> 1;
  const medianMissMs = missMs.length % 2 ? missMs[mid] : (missMs[mid - 1] + missMs[mid]) / 2 || 0;
  const times = misses.map((e) => e.eventTimestamp).sort((a, b) => a - b);
  let pairs = 0;
  for (let i = 1; i < times.length; i++) if (times[i] - times[i - 1] <= PAIR_MS) pairs++;
  const exceeded = reads.filter((e) => e.outcome === "exceededCpu").length;
  return {
    reads: reads.length,
    misses: misses.length,
    medianMissMs,
    maxMissMs: Math.max(0, ...missMs),
    maxHitMs: Math.max(
      0,
      ...reads.filter((e) => !(e.cpuTime >= MISS_MS)).map((e) => e.cpuTime ?? 0),
    ),
    pairs,
    exceeded,
    share: exceeded > 0 || (pairs >= 1 && medianMissMs >= SHARE_MS),
  };
}

const isMain = process.argv[1] && new URL(import.meta.url).pathname === process.argv[1];
if (isMain) {
  const args = process.argv.slice(2);
  const worker = args[0] === "--worker" ? args[1] : "";
  const minutes = args[2] === "--minutes" ? Number(args[3]) : 0;
  if (args.length !== 4 || !worker || !(minutes > 0)) {
    console.error("usage: node tests/live/slim-cpu.mjs --worker <name> --minutes <n>");
    process.exit(2);
  }
  const wrangler = new URL("../../node_modules/.bin/wrangler", import.meta.url).pathname;
  const tail = spawn(wrangler, ["tail", worker, "--format", "json", "--method", "GET"], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  let text = "";
  tail.stdout.setEncoding("utf8").on("data", (chunk) => (text += chunk));
  const timer = setTimeout(() => tail.kill("SIGINT"), minutes * 60000);
  await new Promise((resolve) => tail.on("close", resolve));
  clearTimeout(timer);
  const events = splitRecords(text).map((e) => ({
    eventTimestamp: e.eventTimestamp,
    event: { request: { url: e.event?.request?.url ?? "http://none/" } },
    outcome: e.outcome,
    cpuTime: e.cpuTime,
  }));
  text = "";
  if (events.length === 0) {
    console.error("tail returned no events; see wrangler output above");
    process.exit(3);
  }
  if (!events.some((e) => typeof e.cpuTime === "number")) {
    console.error("tail events carry no cpuTime");
    process.exit(3);
  }
  const s = summarize(events);
  console.log(
    "| reads | misses | median miss ms | max miss ms | max hit ms | concurrent pairs | exceededCpu |",
  );
  console.log("| --- | --- | --- | --- | --- | --- | --- |");
  console.log(
    `| ${s.reads} | ${s.misses} | ${s.medianMissMs} | ${s.maxMissMs} | ${s.maxHitMs} | ${s.pairs} | ${s.exceeded} |`,
  );
  if (s.misses < MIN_MISSES) {
    console.error(`fewer than ${MIN_MISSES} misses; run longer`);
    process.exit(4);
  }
  console.log(s.share ? "share: yes" : "share: no");
}
