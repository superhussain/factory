// Times a board PATCH inside workerd, under Miniflare, on job 2230's synthetic board, so the
// costs Node cannot see show up: R2 get and put of the whole board, text decode and encode,
// and the Durable Object hop. Prints one markdown table.
//   - "in Worker" rows time one step at a time inside the Worker with performance.now(),
//     as a mean over --runs. They are wall times, so the R2 steps include Miniflare's simulator.
//   - "workerd CPU" rows are the workerd process's CPU time per request, read with ps around
//     --runs requests sent over HTTP. That counts the Worker, the Durable Object and Miniflare's
//     R2 simulator together. Production bills the Worker and each Durable Object invocation
//     separately, and charges neither for R2's side, so each row is an upper bound on the CPU
//     of any one invocation. The R2 copy runs later in its own alarm invocation and has its
//     own row.
// Usage: node tools/board-miniflare.mjs [--rows 2300,4600] [--runs 30] [--worker DIR]
import { build } from "esbuild";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
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
// The room with three bench calls: its SQLite row counters, an R2 copy now with the put's wall
// time apart from the room's own, and the board text built anew. performance.now() in workerd
// moves in whole milliseconds, so the text build runs 20 times.
export class BenchRoom extends BenchBase {
  boardWork() {
    return { ...this.board().work };
  }
  async boardFlushTimed() {
    const board = this.board();
    const bucket = board.env.BUCKET;
    let putMs = 0;
    board.env = {
      ...board.env,
      BUCKET: {
        head: (...a) => bucket.head(...a),
        get: (...a) => bucket.get(...a),
        put: async (...a) => {
          const t = performance.now();
          const out = await bucket.put(...a);
          putMs += performance.now() - t;
          return out;
        },
      },
    };
    const t = performance.now();
    await board.flush();
    const total = performance.now() - t;
    board.env = { ...board.env, BUCKET: bucket };
    return { room: total - putMs, put: putMs };
  }
  boardTextTimed() {
    const board = this.board();
    const state = board.state();
    let text = "";
    const t = performance.now();
    for (let i = 0; i < 20; i++) {
      board.memo = { etag: null, text: "" };
      text = board.text(state);
    }
    const build = (performance.now() - t) / 20;
    const e = performance.now();
    for (let i = 0; i < 20; i++) new TextEncoder().encode(text);
    return { build, encode: (performance.now() - e) / 20 };
  }
}
const room = (env) => env.BoardEvents.get(env.BoardEvents.idFromName("main"));
export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (url.pathname === "/bench/steps")
      return Response.json(await benchSteps(env, Number(url.searchParams.get("runs"))));
    if (url.pathname === "/bench/work") return Response.json(await room(env).boardWork());
    if (url.pathname === "/bench/flush") return Response.json(await room(env).boardFlushTimed());
    if (url.pathname === "/bench/text") return Response.json(await room(env).boardTextTimed());
    return serveFactory(request, { ...env, config }, principal, ctx);
  },
};
`;

export async function bundle(dir) {
  // index.js already exports BoardEvents; its own default export gives way to the bench's.
  const source = (await readFile(dir + "index.js", "utf8")).replace(
    "export default {",
    "const production = {",
  );
  const result = await build({
    stdin: {
      // The room class lives in room.js since job 2264, in events.js before.
      contents: `${source}\nimport { BoardEvents as BenchBase } from "./${existsSync(dir + "room.js") ? "room" : "events"}.js";\n${ENTRY}`,
      resolveDir: dir,
      loader: "js",
    },
    bundle: true,
    write: false,
    format: "esm",
    platform: "neutral",
    conditions: ["workerd", "worker", "browser"],
    mainFields: ["module", "main"],
    external: ["cloudflare:workers"],
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

const ORIGIN = "https://factory.example.test";

// The digest a read carries: SHA-256 over the row's JSON with keys sorted at every depth.
const sha256 = (row) =>
  createHash("sha256")
    .update(
      JSON.stringify(row, (_, v) =>
        v && typeof v === "object" && !Array.isArray(v)
          ? Object.fromEntries(
              Object.keys(v)
                .sort()
                .map((k) => [k, v[k]]),
            )
          : v,
      ),
    )
    .digest("hex");

// A Miniflare running the bench Worker over a synthetic board of rows rows in R2.
export async function benchWorker(rows, script) {
  const mf = new Miniflare(
    convertV4MiniflareOptions({
      modules: [{ type: "ESModule", path: "bench-entry.js", contents: script }],
      compatibilityDate: "2026-06-01",
      r2Buckets: ["BUCKET"],
      durableObjects: { BoardEvents: { className: "BenchRoom", useSQLite: true } },
    }),
  );
  await mf.ready;
  const bucket = await mf.getR2Bucket("BUCKET");
  await bucket.put(
    "hosts/mac-a.json",
    JSON.stringify({ id: "mac-a", token: "client-aaaa", state: "active" }),
  );
  const board = syntheticBoard(rows);
  await bucket.put("board.json", JSON.stringify(board));
  const json = async (path, init) => {
    const res = await mf.dispatchFetch(ORIGIN + path, init);
    if (res.status !== 200) throw new Error(`${path} ${res.status}: ${await res.text()}`);
    return path.startsWith("/bench/") ? res.json() : res.text();
  };
  return { mf, bucket, board, json };
}

// SQLite rows read and written by each board operation in the room, on a board of rows rows,
// with the room's own milliseconds for an R2 copy and for building the board text.
export async function roomWork(rows, script) {
  const { mf, bucket, board, json } = await benchWorker(rows, script);
  try {
    const out = {};
    const measure = async (name, fn) => {
      const before = await json("/bench/work");
      await fn();
      const after = await json("/bench/work");
      out[name] = {
        read: after.rowsRead - before.rowsRead,
        written: after.rowsWritten - before.rowsWritten,
      };
    };
    const get = () => json("/board.json");
    const patch = (body) => json("/api/board", { method: "PATCH", body: JSON.stringify(body) });
    const active = board.jobs.filter((j) => j.stage !== "merged" && j.stage !== "closed");
    let tick = 0;
    const touch = (n) =>
      patch({
        rows: active.slice(0, n).map((row) => {
          const expect = { ...row };
          row.updated = new Date(Date.UTC(2026, 10, 23, 12, 0, ++tick))
            .toISOString()
            .replace(/\.\d+Z$/, "Z");
          return { id: row.id, expect, row };
        }),
      });
    // 60 rows spread over the board carry an idea image, as about 56 of today's 2,317 do.
    const merged = board.jobs.filter((j) => j.stage === "merged");
    const uploaded = merged.filter((_, i) => i % Math.floor(merged.length / 60) === 0).slice(0, 60);
    uploaded.forEach((j, i) => (j.idea_images = [`${i.toString(16).padStart(64, "0")}.png`]));
    await bucket.put("board.json", JSON.stringify(board));
    await measure("seed (first read after deploy)", get);
    await touch(1);
    await measure("PATCH 1 row", () => touch(1));
    await measure("PATCH 5 rows", () => touch(5));
    await measure("PATCH a row carrying an idea image", () => {
      const row = uploaded[0];
      const expect = { ...row };
      row.updated = new Date(Date.UTC(2026, 10, 23, 12, 0, ++tick))
        .toISOString()
        .replace(/\.\d+Z$/, "Z");
      return patch({ rows: [{ id: row.id, expect, row }] });
    });
    const added = {
      id: `${rows + 1}-synthetic-job-${rows + 1}`,
      project: "sample",
      stage: "idea",
      title: "New idea",
      capture_id: "00000000-0000-4000-8000-000000000001",
    };
    await measure("PATCH adding a captured idea", () =>
      patch({ rows: [{ id: added.id, expect: null, row: added }] }),
    );
    // A pickup claim under an in-flight cap: one row claimed, every open row read by digest and
    // every other row of the project by stage.
    const claimed = active[1];
    const open = active.filter((j) => j !== claimed);
    const project = board.jobs.filter((j) => j.project === claimed.project && !active.includes(j));
    await measure("PATCH claim, open rows by digest and the project by stage", () =>
      patch({
        rows: [
          {
            id: claimed.id,
            expect: { ...claimed },
            row: { ...claimed, claim: { device: "mac-a" } },
          },
        ],
        reads: [
          ...open.map((j) => ({ id: j.id, sha256: sha256(j) })),
          ...project.map((j) => ({ id: j.id, stage: j.stage })),
        ],
      }),
    );
    claimed.claim = { device: "mac-a" };
    await measure("GET /board.json, new version", get);
    await measure("GET /board.json, same version", get);
    let copy;
    await measure("R2 copy (alarm)", async () => (copy = await json("/bench/flush")));
    const text = await json("/bench/text");
    const etag = async () => (await mf.dispatchFetch(ORIGIN + "/board.json")).headers.get("ETag");
    const doc = JSON.parse(await get());
    await measure("full PUT, one row changed", async () =>
      json("/api/board", {
        method: "PUT",
        headers: { "If-Match": await etag() },
        body: JSON.stringify({
          ...doc,
          jobs: doc.jobs.map((j) => (j.id === added.id ? { ...j, title: "Renamed" } : j)),
        }),
      }),
    );
    const now = JSON.parse(await get());
    const gone = new Set(
      now.jobs
        .filter((j) => j.stage === "merged")
        .slice(0, 3)
        .map((j) => j.id),
    );
    await measure("full PUT, archive pass removing 3 rows", async () =>
      json("/api/board", {
        method: "PUT",
        headers: { "If-Match": await etag() },
        body: JSON.stringify({ ...now, jobs: now.jobs.filter((j) => !gone.has(j.id)) }),
      }),
    );
    return {
      rows: out,
      ms: {
        "room: R2 copy without the put": copy.room,
        "room: R2 copy, the put's wall time": copy.put,
        "room: build board text (GET of a new version)": text.build,
        "room: encode board text (GET body or R2 put)": text.encode,
      },
    };
  } finally {
    await mf.dispose();
  }
}

export async function benchMiniflare(rows, runs, script) {
  const { mf, board } = await benchWorker(rows, script);
  try {
    const origin = ORIGIN;
    const out = {};
    const steps = await (await mf.dispatchFetch(`${origin}/bench/steps?runs=${runs}`)).json();
    for (const [name, ms] of Object.entries(steps)) out[`in Worker: ${name}`] = ms;

    // PATCHes of n active rows, each run moving their updated stamp on, with the rows it expects.
    const active = board.jobs.filter((j) => j.stage !== "merged" && j.stage !== "closed");
    let tick = 0;
    const patchRows = (n) => async () => {
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
    };
    for (const n of [1, 5]) {
      const r = await perRequest(runs, patchRows(n));
      out[`workerd CPU: PATCH ${n} row${n > 1 ? "s" : ""}`] = r.cpu;
      out[`wall: PATCH ${n} row${n > 1 ? "s" : ""}`] = r.wall;
    }
    // The room copies the board to R2 from an alarm FLUSH_MS after a row commit, in its own
    // invocation; /bench/flush runs that copy now. Main's room has no copy, so it counts 0.
    const idle = workerdCpu();
    const copied = await mf.dispatchFetch(`${origin}/bench/flush`);
    await copied.arrayBuffer();
    out["workerd CPU: one R2 copy (alarm)"] = copied.ok ? (workerdCpu() - idle) * 1000 : 0;
    const get = async () => {
      const res = await mf.dispatchFetch(`${origin}/board.json`);
      if (res.status !== 200) throw new Error(`GET ${res.status}`);
      await res.arrayBuffer();
    };
    const cached = await perRequest(runs, get);
    out["workerd CPU: GET /board.json, same version"] = cached.cpu;
    // A GET after each PATCH reads a new version, which the room builds once.
    const both = await perRequest(runs, async () => {
      await patchRows(1)();
      await get();
    });
    out["workerd CPU: GET /board.json after a PATCH"] = both.cpu - out["workerd CPU: PATCH 1 row"];
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
  // SQLite rows per operation and the room's own milliseconds, for a room that stores rows.
  const work = [];
  for (const rows of sizes) work.push(await roomWork(rows, script).catch(() => null));
  if (work.every(Boolean)) {
    console.log(`\n| operation | ${sizes.map((r) => `read ${r} | written ${r}`).join(" | ")} |`);
    console.log(`| --- |${sizes.map(() => " --- | --- |").join("")}`);
    for (const name of Object.keys(work[0].rows))
      console.log(
        `| ${name} | ${work.map((w) => `${w.rows[name].read} | ${w.rows[name].written}`).join(" | ")} |`,
      );
    console.log(`\n| room (ms) | ${sizes.map((r) => `${r} rows`).join(" | ")} |`);
    console.log(`| --- |${sizes.map(() => " --- |").join("")}`);
    for (const name of Object.keys(work[0].ms))
      console.log(`| ${name} | ${work.map((w) => w.ms[name].toFixed(1)).join(" | ")} |`);
  }
}
