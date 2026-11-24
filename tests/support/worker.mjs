import { build } from "esbuild";
import { createHash } from "node:crypto";

// Node has no cloudflare:workers module, so the Worker's DurableObject base comes from a stand-in.
export const CLOUDFLARE_WORKERS = new URL("./cloudflare-workers.mjs", import.meta.url).pathname;

async function bundle(entry) {
  const result = await build({
    entryPoints: [new URL(entry, import.meta.url).pathname],
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

export function loadProduction() {
  return bundle("../../worker/index.js");
}

export function loadAssets() {
  return bundle("../../worker/assets.js");
}

export function memoryBucket() {
  const objects = new Map();
  const calls = [];
  const tag = (bytes) => '"' + createHash("sha256").update(bytes).digest("hex") + '"';
  return {
    objects,
    calls,
    async head(key) {
      calls.push(["head", key]);
      const object = objects.get(key);
      return object ? { ...object, body: undefined, size: object.bytes.length } : null;
    },
    async get(key, options = {}) {
      calls.push(["get", key]);
      const object = objects.get(key);
      if (!object) return null;
      const { range } = options;
      return {
        ...object,
        size: object.bytes.length,
        body: range
          ? object.bytes.subarray(range.offset, range.offset + range.length)
          : object.bytes,
        ...(range && { range }),
        text: async () => object.bytes.toString(),
      };
    },
    async put(key, value, options = {}) {
      calls.push(["put", key, options]);
      const previous = objects.get(key);
      const condition = options.onlyIf;
      if (
        (condition?.has("If-Match") && previous?.httpEtag !== condition.get("If-Match")) ||
        (condition?.get("If-None-Match") === "*" && previous)
      )
        return null;
      const bytes = Buffer.from(value);
      const object = {
        key,
        bytes,
        httpEtag: tag(bytes),
        httpMetadata: options.httpMetadata,
        customMetadata: options.customMetadata,
        options,
        uploaded: new Date(),
      };
      objects.set(key, object);
      return object;
    },
    async delete(key) {
      calls.push(["delete", key]);
      objects.delete(key);
    },
    async list(options) {
      calls.push(["list", options]);
      return {
        objects: [...objects.values()]
          .filter((object) => object.key.startsWith(options.prefix))
          .sort((a, b) => a.key.localeCompare(b.key))
          .slice(0, 1000),
      };
    },
  };
}

export const serverEnv = {
  FACTORY_ORIGIN: "https://factory.example.test",
  FACTORY_TITLE: "Factory",
  ACCESS_ISSUER: "https://example-team.cloudflareaccess.com",
  ACCESS_AUDIENCE: "example-application-audience",
};

// The board room as a real BoardEvents object over Node's SQLite, for Worker tests in Node.
// Its stub copies every argument and result as RPC does and hands fetches, the room's notify
// calls, to fetch. Alarms wait for runAlarm(), so a test decides when R2's copy is written.
// valueLimit refuses a longer SQL value as workerd does (2 MB); Node's SQLite allows 1 GB.
export function memoryRoom(
  BoardEvents,
  env,
  fetch = async () => new Response(null, { status: 204 }),
  valueLimit = Infinity,
) {
  // Loaded here, so only the tests that use a room import node:sqlite.
  const { DatabaseSync } = process.getBuiltinModule("node:sqlite");
  const db = new DatabaseSync(":memory:");
  let alarm = null;
  // Work units a budget test pins: SQL rows the room reads, and bytes crossing the RPC stub.
  const work = { sqlRows: 0, rpcBytes: 0 };
  const storage = {
    sql: {
      exec(query, ...bindings) {
        if (bindings.some((b) => typeof b === "string" && Buffer.byteLength(b) > valueLimit))
          throw new Error("string or blob too big: SQLITE_TOOBIG");
        const rows = db.prepare(query).all(...bindings);
        work.sqlRows += rows.length;
        return { toArray: () => rows };
      },
    },
    transactionSync(fn) {
      db.exec("BEGIN");
      try {
        const out = fn();
        db.exec("COMMIT");
        return out;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
    getAlarm: async () => alarm,
    setAlarm: async (at) => {
      alarm = at;
    },
  };
  globalThis.WebSocketRequestResponsePair ??= class {};
  // Work a request hands to waitUntil, such as a read's R2 copy, which settle() awaits.
  const pending = [];
  const ctx = {
    storage,
    setWebSocketAutoResponse() {},
    getWebSockets: () => [],
    waitUntil(promise) {
      promise.catch(() => {});
      pending.push(promise);
    },
  };
  let object = new BoardEvents(ctx, env);
  const calls = [];
  const rpc =
    (name) =>
    async (...args) => {
      calls.push(name);
      const out = await object[name](...structuredClone(args));
      work.rpcBytes += JSON.stringify(args).length + (JSON.stringify(out) ?? "").length;
      return structuredClone(out);
    };
  const stub = {
    // The room serves GET /board and /board/slim itself; the rest are notify calls for the test.
    fetch: (input, init) =>
      ["/board", "/board/slim"].includes(new URL(input).pathname)
        ? object.fetch(new Request(input, init))
        : fetch(input, init),
    boardHead: rpc("boardHead"),
    boardRead: rpc("boardRead"),
    boardRows: rpc("boardRows"),
    boardCommit: rpc("boardCommit"),
    boardReplace: rpc("boardReplace"),
  };
  return {
    namespace: { idFromName: (name) => ({ name }), get: () => stub },
    get object() {
      return object;
    },
    // A new room object over the same SQLite, as after an eviction or a deploy.
    restart() {
      object = new BoardEvents(ctx, env);
    },
    calls,
    work,
    stub,
    alarm: () => alarm,
    settle: () => Promise.all(pending.splice(0)),
    async runAlarm() {
      alarm = null;
      await object.alarm();
    },
    // The board the room holds, read without a request: its version and parsed document.
    etag: () => object.board().state()?.etag ?? null,
    stored: async () => JSON.parse(await object.board().blob(object.board().state()).text()),
  };
}
