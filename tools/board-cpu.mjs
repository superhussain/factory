// Times each CPU step of a host's PUT /api/board, a PATCH and a slim read in Node, on a
// synthetic board shaped like the live one: about 89% merged rows, median row 1.35 KB,
// p90 4.6 KB. Prints one markdown table of median milliseconds per step.
// Usage: node --expose-gc tools/board-cpu.mjs [--rows 2300,4600] [--runs 15] [--worker DIR]
// --worker times another copy of worker/, such as main's, with its .build beside it.
import { build } from "esbuild";
import { readFile } from "node:fs/promises";
import { parseArgs } from "node:util";
import { CLOUDFLARE_WORKERS } from "../tests/support/worker.mjs";

// The made-up board: row n has a size drawn from the live distribution's quantiles.
export function syntheticBoard(rows) {
  let seed = 7;
  const rand = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
  const sizes = [400, 600, 900, 1350, 1700, 2300, 3500, 5000];
  const jobs = [];
  for (let n = 1; n <= rows; n++) {
    const r = rand();
    const stage =
      r < 0.89 ? "merged" : r < 0.97 ? "closed" : ["idea", "spec", "build", "pr-open"][n % 4];
    const day = String(1 + (n % 28)).padStart(2, "0");
    const row = {
      id: `${n}-synthetic-job-${n}`,
      updated: `2026-10-${day}T12:00:00Z`,
      project: ["factory", "sample", "demo"][n % 3],
      stage,
      title: `Synthetic job ${n}`,
      note: "made-up note",
      pr: `https://github.example.test/sample/pull/${n}`,
      refs: [{ kind: "followup", id: `${n + 1}-synthetic-job-${n + 1}` }],
      round: 1,
      rounds: [{ n: 1, verdict: "PASS" }],
      ...(stage === "merged" || stage === "closed" ? { done: `2026-10-${day}T13:00:00Z` } : {}),
    };
    const want = sizes[Math.floor(rand() * sizes.length)];
    row.idea = "x".repeat(Math.max(0, want - JSON.stringify(row).length));
    jobs.push(row);
  }
  return { updated: "2026-11-23T12:00:00Z", jobs, autonomy: { mode: "auto" } };
}

async function load(dir) {
  const source = await readFile(dir + "index.js", "utf8");
  const result = await build({
    stdin: {
      contents:
        source +
        "\nexport { boardRefusal, rowsFrame, storedBoard, casPut, boardPatch, slimBoardGet };" +
        '\nexport { boardPushes } from "./push.js";\nexport { slimBoard } from "./archive.js";',
      resolveDir: dir,
      loader: "js",
    },
    bundle: true,
    write: false,
    format: "esm",
    platform: "neutral",
    target: "es2022",
    loader: { ".html": "text", ".txt": "text", ".md": "text" },
    alias: { "cloudflare:workers": CLOUDFLARE_WORKERS },
  });
  return import(
    "data:text/javascript;base64," + Buffer.from(result.outputFiles[0].contents).toString("base64")
  );
}

const median = (xs) => xs.sort((a, b) => a - b)[Math.floor(xs.length / 2)];
async function time(runs, fn) {
  const out = [];
  for (let i = 0; i < runs; i++) {
    globalThis.gc?.();
    const t = performance.now();
    await fn();
    out.push(performance.now() - t);
  }
  return median(out);
}

// An R2 stand-in whose get and put cost no CPU beyond handing over the string.
function bucket(text) {
  let stored = { text, httpEtag: '"v1"' };
  return {
    async get() {
      return { httpEtag: stored.httpEtag, body: stored.text, text: async () => stored.text };
    },
    async head() {
      return { httpEtag: stored.httpEtag };
    },
    async put(key, value) {
      stored = { text: value, httpEtag: '"v1"' };
      return { httpEtag: '"v2"' };
    },
  };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const { values } = parseArgs({
    options: {
      rows: { type: "string", default: "2300,4600" },
      runs: { type: "string", default: "15" },
      worker: { type: "string", default: new URL("../worker/", import.meta.url).pathname },
    },
  });
  const w = await load(values.worker.replace(/\/?$/, "/"));
  const runs = Number(values.runs);
  const service = { verified: true, kind: "service" };
  const env = { BoardEvents: null };
  const ctx = { waitUntil() {} };
  console.log(
    "| step | " +
      values.rows
        .split(",")
        .map((r) => `${r} rows`)
        .join(" | ") +
      " |",
  );
  console.log(
    "| --- |" +
      values.rows
        .split(",")
        .map(() => " --- |")
        .join(""),
  );
  const table = {};
  const put = (name, ms) => (table[name] ??= []).push(ms.toFixed(1));
  for (const rows of values.rows.split(",").map(Number)) {
    const board = syntheticBoard(rows);
    const text = JSON.stringify(board);
    // A pickup claim: one active row changes.
    const next = structuredClone(board);
    const active = next.jobs.find((j) => j.stage === "idea");
    active.stage = "spec";
    active.updated = "2026-11-23T12:01:00Z";
    next.updated = "2026-11-23T12:01:00Z";
    const body = JSON.stringify(next);
    const stored = { etag: '"v1"', jobs: board.jobs, doc: board, archived: undefined };
    put("board bytes (MB)", body.length / 1e6);
    put("parse request body", await time(runs, () => JSON.parse(body)));
    put(
      "storedBoard (get + parse)",
      await time(runs, () => w.storedBoard({ BUCKET: bucket(text) })),
    );
    put("boardRefusal", await time(runs, () => w.boardRefusal({}, stored, next, true, service)));
    put("rowsFrame", await time(runs, () => w.rowsFrame(stored, next, '"v2"')));
    put("boardPushes", await time(runs, () => w.boardPushes(board.jobs, next.jobs)));
    put("stringify board", await time(runs, () => JSON.stringify(next)));
    put(
      "PUT /api/board end to end",
      await time(runs, () =>
        w.casPut(
          new Request("http://local/api/board", {
            method: "PUT",
            headers: { "If-Match": '"v1"' },
            body,
          }),
          { ...env, BUCKET: bucket(text) },
          "board.json",
          "application/json",
          service,
          ctx,
        ),
      ),
    );
    const patch = JSON.stringify({
      rows: [{ id: active.id, expect: board.jobs.find((j) => j.id === active.id), row: active }],
    });
    put(
      "PATCH /api/board end to end",
      await time(runs, () =>
        w.boardPatch(
          new Request("http://local/api/board", { method: "PATCH", body: patch }),
          { ...env, BUCKET: bucket(text) },
          service,
          ctx,
        ),
      ),
    );
    put(
      "slimBoard (parse + slice + stringify)",
      await time(runs, () => JSON.stringify(w.slimBoard(JSON.parse(text)))),
    );
  }
  for (const [name, cells] of Object.entries(table))
    console.log(`| ${name} | ${cells.join(" | ")} |`);
}
