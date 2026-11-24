// A device socket on /events/host under Miniflare gets rows frames for board writes that
// have one, "changed" for the rest and for backlog writes, and the room's ping auto-response.
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { build } from "esbuild";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";

const ENTRY = new URL("../support/host-entry.js", import.meta.url).pathname;

async function startHostWorker(persistDir) {
  const { outputFiles } = await build({
    entryPoints: [ENTRY],
    bundle: true,
    format: "esm",
    write: false,
    platform: "neutral",
    conditions: ["workerd", "worker", "browser"],
    mainFields: ["module", "main"],
    external: ["cloudflare:workers"],
    loader: { ".html": "text", ".txt": "text" },
  });
  const mf = new Miniflare(
    convertV4MiniflareOptions({
      modules: [{ type: "ESModule", path: "host-entry.js", contents: outputFiles[0].text }],
      compatibilityDate: "2026-06-01",
      r2Buckets: ["BUCKET"],
      durableObjects: { BoardEvents: { className: "BoardEvents", useSQLite: true } },
      defaultPersistRoot: persistDir,
      host: "127.0.0.1",
      port: 0,
    }),
  );
  const url = await mf.ready;
  // host-entry.js signs in as client-aaaa, so mac-a is the bound host.
  const bucket = await mf.getR2Bucket("BUCKET");
  await bucket.put(
    "hosts/mac-a.json",
    JSON.stringify({ id: "mac-a", token: "client-aaaa", state: "active" }),
  );
  return { origin: url.origin, close: () => mf.dispose() };
}

// Collects every frame and resolves waiters in arrival order.
function frames(socket) {
  const queue = [];
  const waiters = [];
  socket.addEventListener("message", (event) => {
    const waiter = waiters.shift();
    if (waiter) waiter(String(event.data));
    else queue.push(String(event.data));
  });
  return () =>
    queue.length ? Promise.resolve(queue.shift()) : new Promise((resolve) => waiters.push(resolve));
}

test("a host socket gets rows frames, changed frames, and pong for ping", async () => {
  const dir = await mkdtemp(join(tmpdir(), "factory-host-events-"));
  const worker = await startHostWorker(dir);
  try {
    const socket = new WebSocket(worker.origin.replace("http", "ws") + "/events/host");
    await new Promise((resolve, reject) => {
      socket.addEventListener("open", resolve, { once: true });
      socket.addEventListener("error", reject, { once: true });
    });
    const next = frames(socket);
    const board = JSON.stringify({ type: "changed", key: "board.json" });
    const put = (path, body, type, headers) =>
      fetch(worker.origin + path, {
        method: "PUT",
        headers: { "Content-Type": type, ...headers },
        body,
      });

    const created = await put("/api/board", JSON.stringify({ jobs: [] }), "application/json", {
      "If-None-Match": "*",
    });
    assert.equal(created.status, 200);
    assert.equal(await next(), board);

    const row = { id: "001-a", stage: "idea" };
    const updated = await put(
      "/api/board",
      JSON.stringify({ updated: "2026-11-23T00:00:00Z", jobs: [row] }),
      "application/json",
      { "If-Match": created.headers.get("ETag") },
    );
    assert.equal(updated.status, 200);
    assert.deepEqual(JSON.parse(await next()), {
      type: "rows",
      etag: updated.headers.get("ETag"),
      prev: created.headers.get("ETag"),
      updated: "2026-11-23T00:00:00Z",
      rows: [row],
      removed: [],
    });

    const backlog = await put("/api/backlog", "# Backlog\n", "text/markdown", {
      "If-None-Match": "*",
    });
    assert.equal(backlog.status, 200);
    assert.equal(await next(), JSON.stringify({ type: "changed", key: "backlog.md" }));

    socket.send("ping");
    assert.equal(await next(), "pong");
    socket.close();
  } finally {
    await worker.close();
    await rm(dir, { recursive: true, force: true });
  }
});
