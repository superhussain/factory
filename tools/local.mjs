// Owned local hosted verification: fixed loopback Wrangler services with fresh
// job-local R2 persistence. `--port 8220|8221` serves the fixed test entry;
// `--check` runs the complete local integration and tears everything down. The cancel
// drill runs against direct Miniflare on 8222, because wrangler dev never passes a
// client reset on to the worker.
import { spawn } from "node:child_process";
import { mkdir, rm } from "node:fs/promises";
import { isIP } from "node:net";
import { build } from "esbuild";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { prepareAssets } from "./assets.mjs";

const wrangler = new URL("../node_modules/.bin/wrangler", import.meta.url).pathname;
const LOCAL_CONFIG = new URL("../tests/support/wrangler.json", import.meta.url).pathname;
const LOCAL_ENTRY = new URL("../tests/support/local-entry.js", import.meta.url).pathname;
const AUTH_CONFIG = new URL("../tests/support/auth-wrangler.json", import.meta.url).pathname;
const PORTS = [8220, 8221];
const OWNED_PORT_ENV = "FACTORY_OWNED_LOCAL_PORT";
const READY_TIMEOUT_MS = 60_000;

function runId(label) {
  return `${label}-${new Date().toISOString().replaceAll(":", "-").replaceAll(".", "-")}-${process.pid}`;
}

export async function startWorkerd({ port, persistDir, config, ip = "127.0.0.1" }) {
  if (!persistDir) throw new Error("startLocal requires an explicit fresh persistDir");
  const child = spawn(
    wrangler,
    [
      "dev",
      "--config",
      config,
      "--ip",
      ip,
      "--port",
      String(port ?? 0),
      "--inspector-ip",
      "127.0.0.1",
      "--inspector-port",
      "0",
      "--persist-to",
      persistDir,
    ],
    { stdio: ["ignore", "pipe", "pipe", "ipc"] },
  );
  let output = "";
  child.stdout.on("data", (chunk) => {
    output += chunk;
  });
  child.stderr.on("data", (chunk) => {
    output += chunk;
  });
  let origin, readyError;
  child.on("message", (raw) => {
    const message = JSON.parse(raw);
    if (message.event !== "DEV_SERVER_READY") return;
    if (
      message.ip !== ip ||
      !Number.isInteger(message.port) ||
      message.port < 1 ||
      message.port > 65535 ||
      (port !== undefined && message.port !== port)
    ) {
      readyError = new Error("local worker reported an unexpected listening address");
      return;
    }
    origin = `http://${ip}:${message.port}`;
  });
  const exited = new Promise((resolve) => child.once("exit", resolve));
  const deadline = Date.now() + READY_TIMEOUT_MS;
  for (;;) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(
        `local worker on port ${port} exited during startup (code ${child.exitCode}); another listener may own the port\n${output.slice(-2000)}`,
      );
    }
    if (readyError) {
      child.kill("SIGTERM");
      await exited;
      throw readyError;
    }
    try {
      if (origin) {
        // redirect:'manual': the auth wrapper's default path 302s to itself, and
        // redirect-following readiness probes would loop on it.
        await fetch(origin + "/projects.json", {
          redirect: "manual",
          signal: AbortSignal.timeout(1000),
        });
        if (child.exitCode === null && child.signalCode === null) break;
      }
    } catch {}
    if (Date.now() > deadline) {
      child.kill("SIGKILL");
      await exited;
      throw new Error(
        `local worker on port ${port} did not become ready within ${READY_TIMEOUT_MS}ms\n${output.slice(-2000)}`,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  let closed = false;
  async function close() {
    if (closed) return;
    closed = true;
    if (child.exitCode === null) {
      child.kill("SIGTERM");
      let timer;
      const grace = await Promise.race([
        exited.then(() => true),
        new Promise((resolve) => {
          timer = setTimeout(() => resolve(false), 10_000);
        }),
      ]);
      clearTimeout(timer);
      if (!grace) {
        child.kill("SIGKILL");
        await exited;
      }
    }
    let listening = true;
    const teardownDeadline = Date.now() + 10_000;
    while (listening && Date.now() < teardownDeadline) {
      try {
        await fetch(origin + "/projects.json", {
          redirect: "manual",
          signal: AbortSignal.timeout(1000),
        });
      } catch {
        listening = false;
      }
    }
    if (listening) throw new Error(`local worker on port ${port} is still listening after close`);
  }
  return { origin, close };
}

function declaredPort() {
  const p = Number(process.env[OWNED_PORT_ENV]);
  return Number.isInteger(p) && p >= 1024 && p <= 65535 ? p : undefined;
}

// Owned fixed ports are 8220/8221; omitting port assigns a documented owned ephemeral
// loopback port for parallel fixtures, or the caller's own held port declared in
// FACTORY_OWNED_LOCAL_PORT. ip serves the test entry, which skips auth, on one chosen
// address instead of loopback, and never on every address.
export async function startLocal({ port, persistDir, ip = "127.0.0.1" } = {}) {
  if (port !== undefined && !PORTS.includes(port) && port !== declaredPort()) {
    throw new Error(`owned loopback ports are ${PORTS.join(" and ")} only`);
  }
  // The URL parser normalizes every spelling of the unspecified address, such as "::0" or
  // "::ffff:0.0.0.0", to one of these hostnames.
  const host = isIP(ip) && new URL(`http://${isIP(ip) === 6 ? `[${ip}]` : ip}`).hostname;
  if (!host || ["0.0.0.0", "[::]", "[::ffff:0:0]"].includes(host))
    throw new Error("serve on one IP address, never all of them");
  return startWorkerd({ port, persistDir, config: LOCAL_CONFIG, ip });
}

export async function startCancelWorker({ persistDir }) {
  const { outputFiles } = await build({
    entryPoints: [LOCAL_ENTRY],
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
      modules: [{ type: "ESModule", path: "local-entry.js", contents: outputFiles[0].text }],
      compatibilityDate: "2026-06-01",
      r2Buckets: ["BUCKET"],
      durableObjects: { BoardEvents: { className: "BoardEvents", useSQLite: true } },
      defaultPersistRoot: persistDir,
      host: "127.0.0.1",
      port: 8222,
    }),
  );
  await mf.ready;
  return { origin: "http://127.0.0.1:8222", close: () => mf.dispose() };
}

// The local HTTP suites, one file at a time, since they share the services' storage.
const SUITES = ["tests/worker/project-api.test.mjs", "tests/worker/project-documents-api.test.mjs"];

function nodeTest(env) {
  return new Promise((resolve) => {
    const child = spawn(
      process.execPath,
      ["--test", "--test-concurrency=1", "--test-reporter=spec", ...SUITES],
      { env: { ...process.env, ...env } },
    );
    let output = "";
    child.stdout.on("data", (chunk) => {
      output += chunk;
    });
    child.stderr.on("data", (chunk) => {
      output += chunk;
    });
    child.once("exit", (code) => resolve({ code, output }));
  });
}

function nodeTestCounts(output) {
  const count = (name) => Number(new RegExp(`^ℹ ${name} (\\d+)$`, "m").exec(output)?.[1] ?? 0);
  return {
    executed: count("tests"),
    passed: count("pass"),
    failed: count("fail") + count("cancelled"),
    errors: 0,
    skipped: count("skipped") + count("todo"),
  };
}

// The workerd production-entry crypto checks: real default fetch, real jose, locally
// signed assertions. Expected storage calls prove denials precede content I/O.
const AUTH_CASES = [
  ["valid page", { mode: "valid", path: "/" }, 200, 0],
  ["valid legacy dashboard", { mode: "valid", path: "/dashboard.html" }, 302, 0],
  ["valid board read", { mode: "valid", path: "/api/board" }, 404, 1],
  ["valid unknown path", { mode: "valid", path: "/unknown" }, 404, 0],
  [
    "valid asset wrong method",
    { mode: "valid", method: "POST", path: "/assets/mermaid.min.js" },
    404,
    0,
  ],
  ["no assertion", { mode: "none" }, 403, 0],
  ["garbage assertion", { mode: "garbage" }, 403, 0],
  ["cookie only", { mode: "cookie" }, 403, 0],
  ["forged signature", { mode: "wrong-key" }, 403, 0],
  ["wrong issuer", { mode: "wrong-issuer" }, 403, 0],
  ["wrong audience", { mode: "wrong-audience" }, 403, 0],
  ["expired", { mode: "expired" }, 403, 0],
  ["other origin with valid assertion", { mode: "valid", origin: "other" }, 403, 0],
];

async function check() {
  const dir = new URL(`../.context/${runId("check")}/`, import.meta.url).pathname;
  await mkdir(dir, { recursive: true });
  await prepareAssets();
  const service = await startLocal({ port: 8220, persistDir: dir + "r2" });
  let auth, cancel;
  const onSignal = () => {
    (auth ? auth.close() : Promise.resolve())
      .finally(() => (cancel ? cancel.close() : undefined))
      .finally(() => service.close().finally(() => process.exit(1)));
  };
  process.once("SIGINT", onSignal);
  process.once("SIGTERM", onSignal);
  const totals = { executed: 0, passed: 0, failed: 0, errors: 0, skipped: 0 };
  try {
    auth = await startWorkerd({ port: 8221, persistDir: dir + "r2-auth", config: AUTH_CONFIG });
    cancel = await startCancelWorker({ persistDir: dir + "r2-cancel" });
    const suite = await nodeTest({
      FACTORY_TEST_URL: service.origin,
      FACTORY_CANCEL_TEST_URL: cancel.origin,
    });
    console.log(suite.output);
    const counts = nodeTestCounts(suite.output);
    for (const key of Object.keys(totals)) totals[key] += counts[key];
    if (suite.code !== 0) totals.failed += 1;
    let authFailed = 0;
    for (const [name, params, status, storageCalls] of AUTH_CASES) {
      const query = new URLSearchParams(
        Object.fromEntries(Object.entries(params).map(([k, v]) => [k, String(v)])),
      );
      const response = await fetch(`${auth.origin}/?${query}`, { redirect: "manual" });
      const calls = JSON.parse(response.headers.get("X-Test-Storage-Calls"));
      let ok = response.status === status && calls.length === storageCalls;
      if (ok && status === 302)
        ok = response.headers.get("Location") === "/" && response.redirected === false;
      totals.executed += 1;
      if (ok) totals.passed += 1;
      else {
        totals.failed += 1;
        authFailed += 1;
      }
      console.log(
        `${ok ? "PASS" : "FAIL"} auth-entry ${name}: status ${response.status} (want ${status}), storage calls ${calls.length} (want ${storageCalls})`,
      );
    }
    console.log(
      `executed=${totals.executed} passed=${totals.passed} failed=${totals.failed} errors=${totals.errors} skipped=${totals.skipped}`,
    );
    if (totals.failed || totals.errors || totals.skipped) {
      throw new Error(`local check failed: ${JSON.stringify(totals)}`);
    }
  } finally {
    process.removeListener("SIGINT", onSignal);
    process.removeListener("SIGTERM", onSignal);
    if (auth) await auth.close();
    if (cancel) await cancel.close();
    await service.close();
    await rm(dir + "r2", { recursive: true, force: true });
    await rm(dir + "r2-auth", { recursive: true, force: true });
    await rm(dir + "r2-cancel", { recursive: true, force: true });
  }
}

async function serve(port) {
  const dir = new URL(`../.context/${runId("local")}/`, import.meta.url).pathname;
  await mkdir(dir, { recursive: true });
  await prepareAssets();
  const service = await startLocal({ port, persistDir: dir + "r2" });
  console.log(`serving ${service.origin} (fresh local R2 at ${dir}r2); stop with Ctrl-C`);
  await new Promise((resolve) => {
    for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, resolve);
  });
  await service.close();
  await rm(dir, { recursive: true, force: true });
  console.log("stopped");
}

const isMain = process.argv[1] && new URL(import.meta.url).pathname === process.argv[1];
const args = isMain ? process.argv.slice(2) : [];
if (isMain && args.length === 2 && args[0] === "--port" && PORTS.includes(Number(args[1]))) {
  await serve(Number(args[1]));
} else if (isMain && args.length === 1 && args[0] === "--check") {
  await check();
} else if (isMain) {
  console.error("usage: node tools/local.mjs --check | --port 8220|8221");
  process.exit(2);
}
