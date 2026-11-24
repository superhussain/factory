// Times a board PATCH inside workerd, under Miniflare, on job 2230's synthetic board, so the
// costs Node cannot see show up: R2 get and put of the whole board, text decode and encode,
// and the Durable Object hop. Prints one markdown table.
//   - "in Worker" rows time one step at a time inside the Worker with performance.now(),
//     as a mean over --runs. They are wall times, so the R2 steps include Miniflare's simulator.
//   - "workerd CPU" rows are the workerd process's CPU time per request, read with ps around
//     --runs requests sent over HTTP. That counts the Worker, the Durable Object and Miniflare's
//     R2 simulator, which production does not charge to the Worker, so it is an upper bound.
// Usage: node tools/board-miniflare.mjs [--rows 2300,4600] [--runs 30] [--worker DIR]
import { build } from "esbuild";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { parseArgs } from "node:util";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { syntheticBoard } from "./board-cpu.mjs";

// The bench Worker: serveFactory as a signed-in host, plus /bench/steps, which times the
// steps a PATCH took before job 2264 on the board stored in R2.
const ENTRY = `
const principal = { verified: true, kind: "service", client_id: "client-aaaa" };
const config = {
  origin: "https://factory.example.test",
  title: "Factory",
  issuer: "https://example-team.cloudflareaccess.com",
  audience: "example-application-audience",
  image_origins: [],
  embed_origins: [],
};
async function benchSteps(env, runs) {
  const out = {};
  const time = async (name, fn) => {
    let total = 0, value;
    for (let i = 0; i < runs; i++) {
      const t = performance.now();
      value = await fn();
      total += performance.now() - t;
    }
    out[name] = total / runs;
    return value;
  };
  const text = await time("R2 get + text", async () => (await env.BUCKET.get("board.json")).text());
  const doc = await time("parse board", () => JSON.parse(text));
  const next = structuredClone(doc);
  const row = next.jobs.find((j) => j.stage === "idea");
  row.stage = "spec";
  const stored = { etag: '"v1"', jobs: doc.jobs, doc, archived: doc.archived };
  await time("boardRefusal", () => boardRefusal(env, stored, next, true, principal));
  await time("rowsFrame", () => rowsFrame(stored, next, '"v2"'));
  const json = await time("stringify board", () => JSON.stringify(next));
  const bytes = await time("encode board", () => new TextEncoder().encode(json));
  await time("R2 put", () => env.BUCKET.put("bench-scratch.json", bytes));
  return out;
}
export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/bench/steps")
      return Response.json(await benchSteps(env, Number(url.searchParams.get("runs"))));
    return serveFactory(request, { ...env, config }, principal, ctx);
  },
};
`;

async function bundle(dir) {
  // index.js already exports BoardEvents; its own default export gives way to the bench's.
  const source = (await readFile(dir + "index.js", "utf8")).replace(
    "export default {",
    "const production = {",
  );
  const result = await build({
    stdin: { contents: source + "\n" + ENTRY, resolveDir: dir, loader: "js" },
    bundle: true,
    write: false,
    format: "esm",
    platform: "neutral",
    conditions: ["workerd", "worker", "browser"],
    mainFields: ["module", "main"],
    loader: { ".html": "text", ".txt": "text", ".md": "text" },
  });
  return result.outputFiles[0].text;
}

// CPU seconds the workerd processes this Node process started have used so far.
function workerdCpu() {
  const lines = execFileSync("ps", ["-A", "-o", "ppid=,time=,comm="], { encoding: "utf8" });
  let total = 0;
  for (const line of lines.split("\n")) {
    const [ppid, time, comm] = line.trim().split(/\s+/);
    if (Number(ppid) !== process.pid || !comm?.endsWith("workerd")) continue;
    const parts = time.split(":").map(Number);
    total += parts.reduce((acc, n) => acc * 60 + n, 0);
  }
  return total;
}

// Mean milliseconds of workerd CPU and of wall time per call of fn over runs calls.
async function perRequest(runs, fn) {
  await fn(-1);
  const cpu = workerdCpu();
  const t = performance.now();
  for (let i = 0; i < runs; i++) await fn(i);
  return { cpu: ((workerdCpu() - cpu) * 1000) / runs, wall: (performance.now() - t) / runs };
}

export async function benchMiniflare(rows, runs, script) {
  const mf = new Miniflare(
    convertV4MiniflareOptions({
      modules: [{ type: "ESModule", path: "bench-entry.js", contents: script }],
      compatibilityDate: "2026-06-01",
      r2Buckets: ["BUCKET"],
      durableObjects: { BoardEvents: { className: "BoardEvents", useSQLite: true } },
    }),
  );
  try {
    await mf.ready;
    const bucket = await mf.getR2Bucket("BUCKET");
    await bucket.put(
      "hosts/mac-a.json",
      JSON.stringify({ id: "mac-a", token: "client-aaaa", state: "active" }),
    );
    const board = syntheticBoard(rows);
    await bucket.put("board.json", JSON.stringify(board));
    const origin = "https://factory.example.test";
    const out = {};
    const steps = await (await mf.dispatchFetch(`${origin}/bench/steps?runs=${runs}`)).json();
    for (const [name, ms] of Object.entries(steps)) out[`in Worker: ${name}`] = ms;

    // PATCHes of n active rows, each run moving their updated stamp on, with the rows it expects.
    const active = board.jobs.filter((j) => j.stage !== "merged" && j.stage !== "closed");
    let tick = 0;
    for (const n of [1, 5]) {
      const r = await perRequest(runs, async () => {
        const updated = new Date(Date.UTC(2026, 10, 23, 12, 0, ++tick)).toISOString();
        const body = JSON.stringify({
          rows: active.slice(0, n).map((row) => {
            const expect = { ...row };
            row.updated = updated.replace(/\.\d+Z$/, "Z");
            return { id: row.id, expect, row };
          }),
        });
        const res = await mf.dispatchFetch(`${origin}/api/board`, { method: "PATCH", body });
        if (res.status !== 200) throw new Error(`PATCH ${res.status}: ${await res.text()}`);
        await res.arrayBuffer();
      });
      out[`workerd CPU: PATCH ${n} row${n > 1 ? "s" : ""}`] = r.cpu;
      out[`wall: PATCH ${n} row${n > 1 ? "s" : ""}`] = r.wall;
    }
    const get = await perRequest(runs, async () => {
      const res = await mf.dispatchFetch(`${origin}/board.json`);
      if (res.status !== 200) throw new Error(`GET ${res.status}`);
      await res.arrayBuffer();
    });
    out["workerd CPU: GET /board.json"] = get.cpu;
    out["wall: GET /board.json"] = get.wall;
    return out;
  } finally {
    await mf.dispose();
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const { values } = parseArgs({
    options: {
      rows: { type: "string", default: "2300,4600" },
      runs: { type: "string", default: "30" },
      worker: { type: "string", default: new URL("../worker/", import.meta.url).pathname },
    },
  });
  const script = await bundle(values.worker.replace(/\/?$/, "/"));
  const sizes = values.rows.split(",").map(Number);
  const table = {};
  for (const rows of sizes)
    for (const [name, ms] of Object.entries(
      await benchMiniflare(rows, Number(values.runs), script),
    ))
      (table[name] ??= []).push(ms.toFixed(1));
  console.log(`| step (mean ms) | ${sizes.map((r) => `${r} rows`).join(" | ")} |`);
  console.log(`| --- |${sizes.map(() => " --- |").join("")}`);
  for (const [name, cells] of Object.entries(table))
    console.log(`| ${name} | ${cells.join(" | ")} |`);
}
