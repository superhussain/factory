// Live backlog triage run: the synthetic cases through wrangler dev with the real Workers AI
// binding and fresh local R2. Never part of CI. Usage: node tests/live/triage-live.mjs <out.json>
// with CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID in the environment.
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { prepareAssets } from "../../tools/assets.mjs";
import { startWorkerd } from "../../tools/local.mjs";

const out = process.argv[2];
if (!out || !process.env.CLOUDFLARE_API_TOKEN || !process.env.CLOUDFLARE_ACCOUNT_ID) {
  console.error(
    "usage: node tests/live/triage-live.mjs <out.json> with CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID set",
  );
  process.exit(2);
}

const RUNS = 3;
const THRESHOLD = 80;
const USD_PER_INPUT_TOKEN = 0.038 / 1e6;
const usdToNeurons = (usd) => (usd / 0.011) * 1000;
const { rules, cases } = JSON.parse(
  await readFile(new URL("triage-cases.json", import.meta.url), "utf8"),
);
const median = (values) => [...values].sort((a, b) => a - b)[(values.length - 1) >> 1];
const verdictAt = (noul, threshold) => (Math.round(noul * 100) >= threshold ? "queue" : "hold");

const persistDir = new URL(`../../.context/triage-live-${Date.now()}/`, import.meta.url).pathname;
await mkdir(persistDir, { recursive: true });
await prepareAssets();
const service = await startWorkerd({
  persistDir: persistDir + "r2",
  config: new URL("wrangler.json", import.meta.url).pathname,
});
const results = [];
let failure;
try {
  const put = async (route, body, type) => {
    const response = await fetch(service.origin + route, {
      method: "PUT",
      headers: { "If-None-Match": "*", "Content-Type": type },
      body,
    });
    if (response.status !== 200) throw new Error(`seed ${route}: ${response.status}`);
  };
  await put(
    "/api/projects/sample.md",
    "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    "text/markdown",
  );
  await put(
    "/api/pipeline/projects/sample",
    JSON.stringify({ schema: 1, triage: { enabled: true, threshold: THRESHOLD, rules } }),
    "application/json",
  );
  cases: for (const c of cases) {
    const runs = [];
    for (let i = 0; i < RUNS; i++) {
      const start = performance.now();
      const response = await fetch(service.origin + "/api/backlog/triage", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ item: c.item, ...(c.context && { context: c.context }) }),
      });
      const wall_ms = Math.round(performance.now() - start);
      const text = await response.text();
      const live = JSON.parse(response.headers.get("x-triage-live") ?? "{}");
      if (response.status !== 200) {
        failure = { id: c.id, run: i + 1, status: response.status, body: text, live };
        break cases;
      }
      const verdict = JSON.parse(text);
      runs.push({
        wall_ms,
        model_ms: live.ms,
        input_tokens: live.input_tokens,
        noul: live.noul,
        ...verdict,
      });
    }
    results.push({ ...c, runs });
  }
} finally {
  await service.close();
}

const rows = results.map((c) => {
  const nouls = c.runs.map((r) => r.noul);
  const noul = median(nouls);
  const input_tokens = median(c.runs.map((r) => r.input_tokens));
  const usd = input_tokens * USD_PER_INPUT_TOKEN;
  const verdict = verdictAt(noul, THRESHOLD);
  return {
    id: c.id,
    noul,
    spread: Math.max(...nouls) - Math.min(...nouls),
    wall_ms: median(c.runs.map((r) => r.wall_ms)),
    model_ms: median(c.runs.map((r) => r.model_ms)),
    input_tokens,
    usd,
    neurons: usdToNeurons(usd),
    human: c.human,
    verdict,
    match: verdict === c.human,
  };
});
const agreement = Object.fromEntries(
  [50, 60, 70, 80, 90].map((t) => [
    t,
    {
      agree: rows.filter((r) => verdictAt(r.noul, t) === r.human).length,
      false_queues: rows.filter((r) => r.human === "hold" && verdictAt(r.noul, t) === "queue")
        .length,
    },
  ]),
);
const tokens = median(results.flatMap((c) => c.runs.map((r) => r.input_tokens)));
const perCall = { input_tokens: tokens, usd: tokens * USD_PER_INPUT_TOKEN };
perCall.neurons = usdToNeurons(perCall.usd);
perCall.free_calls_per_day = Math.floor(10000 / perCall.neurons);
await writeFile(
  out,
  JSON.stringify(
    { rules, threshold: THRESHOLD, failure, per_call: perCall, agreement, rows, results },
    null,
    2,
  ) + "\n",
);

console.log(
  "| id | median probability | spread | wall ms | model ms | input tokens | cost USD | neurons | human | verdict at 80 | match |",
);
console.log("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
for (const r of rows)
  console.log(
    `| ${r.id} | ${r.noul.toFixed(3)} | ${r.spread.toFixed(3)} | ${r.wall_ms} | ${r.model_ms} | ${r.input_tokens} | ${r.usd.toExponential(2)} | ${r.neurons.toFixed(3)} | ${r.human} | ${r.verdict} | ${r.match ? "yes" : "no"} |`,
  );
console.log("\n| threshold | agree | false queues |\n| --- | --- | --- |");
for (const [t, a] of Object.entries(agreement))
  console.log(`| ${t} | ${a.agree}/${rows.length} | ${a.false_queues} |`);
console.log(
  `\nper call: ${perCall.input_tokens} input tokens, $${perCall.usd.toExponential(2)}, ${perCall.neurons.toFixed(3)} neurons, ${perCall.free_calls_per_day} calls a day on the free tier`,
);
if (failure) {
  console.error(`stopped at ${failure.id} run ${failure.run}: ${failure.status} ${failure.body}`);
  if (failure.live.error) console.error(`model error: ${failure.live.error}`);
  process.exit(1);
}
