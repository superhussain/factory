// Live triage cost run: one question item, 8 plain items one call each, and the same 8 in one
// batch call, through wrangler dev with the real Workers AI binding and fresh local R2. Never part
// of CI. Usage: node tests/live/triage-cost.mjs <out.json> with CLOUDFLARE_API_TOKEN and
// CLOUDFLARE_ACCOUNT_ID in the environment.
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { prepareAssets } from "../../tools/assets.mjs";
import { startWorkerd } from "../../tools/local.mjs";

const out = process.argv[2];
if (!out || !process.env.CLOUDFLARE_API_TOKEN || !process.env.CLOUDFLARE_ACCOUNT_ID) {
  console.error(
    "usage: node tests/live/triage-cost.mjs <out.json> with CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID set",
  );
  process.exit(2);
}

const RUNS = 3;
const USD_PER_INPUT_TOKEN = 0.038 / 1e6;
const usdToNeurons = (usd) => (usd / 0.011) * 1000;
const { rules, cases } = JSON.parse(
  await readFile(new URL("triage-cases.json", import.meta.url), "utf8"),
);
const question = JSON.parse(
  await readFile(new URL("triage-question.json", import.meta.url), "utf8"),
);
const plain = cases.filter((c) => !c.context).slice(0, 8);
const median = (values) => [...values].sort((a, b) => a - b)[(values.length - 1) >> 1];

const persistDir = new URL(`../../.context/triage-cost-${Date.now()}/`, import.meta.url).pathname;
await mkdir(persistDir, { recursive: true });
await prepareAssets();
const service = await startWorkerd({
  persistDir: persistDir + "r2",
  config: new URL("wrangler.json", import.meta.url).pathname,
});

// Posts one body and returns its reply with the summed tokens of every model call it made.
const post = async (route, body) => {
  const response = await fetch(service.origin + route, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  if (response.status !== 200) throw new Error(`${route}: ${response.status} ${text}`);
  const header = response.headers.get("x-triage-calls");
  if (!header) throw new Error(`${route}: no x-triage-calls header`);
  const calls = JSON.parse(header);
  for (const call of calls)
    if (typeof call.input_tokens !== "number")
      throw new Error(`${route}: input_tokens is not a number: ${JSON.stringify(call)}`);
  const sum = (key) => calls.reduce((total, call) => total + call[key], 0);
  return {
    reply: JSON.parse(text),
    input_tokens: sum("input_tokens"),
    output_tokens: sum("output_tokens"),
  };
};
const repeat = async (route, body, check = () => {}) => {
  const runs = [];
  for (let i = 0; i < RUNS; i++) {
    const run = await post(route, body);
    check(run.reply);
    runs.push(run);
  }
  return {
    input_tokens: median(runs.map((r) => r.input_tokens)),
    output_tokens: median(runs.map((r) => r.output_tokens)),
    runs,
  };
};
const cost = ({ input_tokens, output_tokens }) => {
  const usd = input_tokens * USD_PER_INPUT_TOKEN;
  return { input_tokens, output_tokens, usd, neurons: usdToNeurons(usd) };
};

let result;
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
    JSON.stringify({ schema: 1, triage: { enabled: true, threshold: 80, rules } }),
    "application/json",
  );
  const q = await repeat("/api/backlog/triage", question, (reply) => {
    if (!reply.choices) throw new Error(`question reply has no choices: ${JSON.stringify(reply)}`);
  });
  const singles = [];
  for (const c of plain) singles.push(await repeat("/api/backlog/triage", { item: c.item }));
  const batch = await repeat("/api/backlog/triage/batch", {
    items: plain.map((c) => ({ item: c.item })),
  });
  const singlesSum = singles.reduce((total, s) => total + s.input_tokens, 0);
  result = {
    question: cost(q),
    plain: cost({
      input_tokens: median(singles.map((s) => s.input_tokens)),
      output_tokens: median(singles.map((s) => s.output_tokens)),
    }),
    batch: cost(batch),
    singles: singlesSum,
    saving_pct: Math.round((1 - batch.input_tokens / singlesSum) * 100),
    runs: {
      question: q.runs,
      plain: plain.map((c, i) => ({ id: c.id, runs: singles[i].runs })),
      batch: batch.runs,
    },
  };
} catch (error) {
  console.error(String(error?.message ?? error));
  process.exitCode = 1;
} finally {
  await service.close();
}
if (!result) process.exit(1);
await writeFile(out, JSON.stringify(result, null, 2) + "\n");

console.log("| case | input tokens | output tokens | cost USD | neurons |");
console.log("| --- | --- | --- | --- | --- |");
for (const name of ["question", "plain", "batch"]) {
  const r = result[name];
  console.log(
    `| ${name} | ${r.input_tokens} | ${r.output_tokens} | ${r.usd.toExponential(2)} | ${r.neurons.toFixed(3)} |`,
  );
}
console.log(
  `\n8 single calls: ${result.singles} input tokens; batch saving: ${result.saving_pct}%`,
);
