// Times dashboard board writes against a local Worker on a synthetic board: the old GET plus
// PUT round trip beside one PATCH /api/board, each with the backlog half of an accept, and the
// live event and slim read a second tab waits on. Prints one markdown table.
// Usage: node tools/board-timing.mjs [--rows N] [--runs N]
import { randomBytes } from "node:crypto";
import { request as httpRequest } from "node:http";
import { rm } from "node:fs/promises";
import { parseArgs } from "node:util";
import { prepareAssets } from "./assets.mjs";
import { startLocal } from "./local.mjs";

const { values } = parseArgs({
  options: { rows: { type: "string", default: "2000" }, runs: { type: "string", default: "10" } },
});
const ROWS = Number(values.rows);
const RUNS = Number(values.runs);
const dir = new URL(`../.context/board-timing-${process.pid}/`, import.meta.url).pathname;

await prepareAssets();
const service = await startLocal({ persistDir: dir + "r2" });
const { origin } = service;

// One request: its status, body and the bytes each way.
async function call(path, init = {}) {
  const r = await fetch(origin + path, init);
  const body = Buffer.from(await r.arrayBuffer());
  if (!r.ok) throw new Error(`${init.method ?? "GET"} ${path}: ${r.status} ${body}`);
  return { r, body, sent: init.body ? Buffer.byteLength(init.body) : 0, received: body.length };
}

const at = (k) => new Date(Date.UTC(2026, 10, 19) + k * 60000).toISOString().replace(".000", "");
const seedJobs = Array.from({ length: ROWS }, (_, k) => ({
  id: `${String(k + 1).padStart(4, "0")}-synthetic-row`,
  project: "sample",
  title: `Synthetic row ${k + 1}`,
  idea: "Synthetic row. ".repeat(94).slice(0, 1400),
  stage: k >= ROWS - 10 ? "build" : "merged",
  updated: at(k),
}));
await call("/api/board", {
  method: "PUT",
  headers: { "If-None-Match": "*" },
  body: JSON.stringify({ updated: at(ROWS), jobs: seedJobs }),
});
const backlogLines = Array.from(
  { length: 20 },
  (_, k) => `- [ ] sample: Synthetic backlog item ${k + 1}`,
);
await call("/api/backlog", {
  method: "PUT",
  headers: { "Content-Type": "text/markdown; charset=utf-8", "If-None-Match": "*" },
  body: `# Backlog\n${backlogLines.join("\n")}\n`,
});

let nextId = ROWS;
const newRow = () => {
  nextId++;
  return {
    id: `${String(nextId).padStart(4, "0")}-timing-idea`,
    project: "sample",
    title: "Timing idea",
    idea: "Timing idea",
    stage: "idea",
    updated: at(nextId),
  };
};

async function restIdea() {
  const read = await call("/api/board");
  const board = JSON.parse(read.body);
  const row = newRow();
  board.jobs.push(row);
  board.updated = row.updated;
  const write = await call("/api/board", {
    method: "PUT",
    headers: { "If-Match": read.r.headers.get("ETag") },
    body: JSON.stringify(board),
  });
  return { sent: read.sent + write.sent, received: read.received + write.received, id: row.id };
}

async function patchIdea() {
  const row = newRow();
  const write = await call("/api/board", {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ updated: row.updated, rows: [{ id: row.id, expect: null, row }] }),
  });
  return { sent: write.sent, received: write.received, id: row.id };
}

// The backlog half of an accept: checks one line off, or back on once all are checked.
async function acceptLine(idea) {
  const step = await idea();
  const read = await call("/api/backlog");
  const text = read.body.toString();
  const next = text.includes("- [ ] ")
    ? text.replace(/- \[ \] (.*)/, `- [x] $1 (${step.id})`)
    : text.replace("- [x] ", "- [ ] ");
  const write = await call("/api/backlog", {
    method: "PUT",
    headers: {
      "Content-Type": "text/markdown; charset=utf-8",
      "If-Match": read.r.headers.get("ETag"),
    },
    body: next,
  });
  return {
    sent: step.sent + read.sent + write.sent,
    received: step.received + read.received + write.received,
  };
}

// A second tab's live socket: a raw WebSocket upgrade, read as frames arrive.
const socket = await new Promise((resolve, reject) => {
  const req = httpRequest(origin + "/events/board", {
    headers: {
      Connection: "Upgrade",
      Upgrade: "websocket",
      "Sec-WebSocket-Version": "13",
      "Sec-WebSocket-Key": randomBytes(16).toString("base64"),
      Origin: origin,
    },
  });
  req.on("upgrade", (_, s) => resolve(s));
  req.on("response", (res) => reject(new Error(`/events/board: ${res.statusCode}, no upgrade`)));
  req.on("error", reject);
  req.end();
});
// Resolves the wait for the next server text frame, whose opcode sits in the low bits of its
// first byte. Frames with no wait are dropped, so the socket never buffers stale ones.
let waiter = null;
socket.on("data", (chunk) => {
  if ((chunk[0] & 0x0f) !== 1 || !waiter) return;
  waiter(chunk);
  waiter = null;
});
const textFrame = () => new Promise((resolve) => (waiter = resolve));

async function frameAfterPatch() {
  const frame = textFrame();
  const write = await patchIdea();
  const chunk = await frame;
  return { sent: write.sent, received: chunk.length };
}

async function slimRead() {
  const read = await call("/board.json?view=slim");
  return { sent: 0, received: read.received };
}

const STEPS = [
  ["rest new idea", restIdea],
  ["patch new idea", patchIdea],
  ["rest backlog accept", () => acceptLine(restIdea)],
  ["patch backlog accept", () => acceptLine(patchIdea)],
  ["second tab frame", frameAfterPatch],
  ["second tab read", slimRead],
];
const results = new Map(STEPS.map(([name]) => [name, []]));
for (let run = 0; run < RUNS; run++)
  for (const [name, step] of STEPS) {
    const started = performance.now();
    const bytes = await step();
    results.get(name).push({ ms: performance.now() - started, ...bytes });
  }

socket.destroy();
await service.close();
await rm(dir, { recursive: true, force: true });

const pick = (list, q) => list.toSorted((a, b) => a - b)[Math.ceil(q * list.length) - 1];
console.log("| step | median ms | p90 ms | bytes sent | bytes received |");
console.log("| --- | --- | --- | --- | --- |");
for (const [name, list] of results) {
  const ms = list.map((r) => r.ms);
  console.log(
    `| ${name} | ${pick(ms, 0.5).toFixed(1)} | ${pick(ms, 0.9).toFixed(1)} | ${pick(
      list.map((r) => r.sent),
      0.5,
    )} | ${pick(
      list.map((r) => r.received),
      0.5,
    )} |`,
  );
}
console.log("The dashboard waits LIVE_READ_MS (2000 ms) after a frame before its read.");
