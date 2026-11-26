// Shared parts of the local Worker HTTP suites, which `node tools/local.mjs --check` runs against
// owned loopback services: the target guard, a request that never follows redirects, the strong
// ETag reader, synthetic device homes, and the factory binary run in one of those homes.
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";

export const OWNED = ["http://127.0.0.1:8220", "http://127.0.0.1:8221"];

// The local origin under test, or undefined to skip; anything but an owned origin throws before any request.
export function testUrl() {
  const url = process.env.FACTORY_TEST_URL;
  if (url !== undefined && !OWNED.includes(url))
    throw new Error("FACTORY_TEST_URL must be an owned loopback origin: " + OWNED.join(" or "));
  return url;
}

export async function request(origin, path, { method = "GET", body, headers = {} } = {}) {
  const r = await fetch(origin + path, {
    method,
    ...(body === undefined ? {} : { body }),
    headers,
    redirect: "manual",
    signal: AbortSignal.timeout(30_000),
  });
  return { status: r.status, headers: r.headers, body: Buffer.from(await r.arrayBuffer()) };
}

export function strong(tag) {
  // oxlint-disable-next-line no-control-regex
  if (typeof tag !== "string" || !/^(W\/)?"[\x21\x23-\x7e\x80-\xff]+"$/.test(tag))
    throw new Error("missing or malformed ETag");
  return tag.startsWith("W/") ? tag.slice(2) : tag;
}

// A device home pointed at origin, with an empty .env and the given paths.md entries.
export function makeHome(dir, name, origin, paths) {
  const home = join(dir, name);
  mkdirSync(home);
  writeFileSync(join(home, "config.md"), `---\napi_url: ${origin}\n---\n`);
  writeFileSync(join(home, ".env"), "");
  if (paths)
    writeFileSync(
      join(home, "paths.md"),
      `---\n${Object.entries(paths)
        .map(([k, v]) => `${k}: ${v}`)
        .join("\n")}\n---\n`,
    );
  return home;
}

// A loopback proxy to origin that holds the first `count` requests matching `method path` until all
// of them have arrived: two devices' binaries meet at it as the Python suites' in-process barrier made
// them meet. By default it forwards the held requests together; with `after` it forwards each one
// first and holds its reply, so every device gets its answer before any of them can write. `calls`
// lists every `method path` it saw, and `replies` each `method path status`, in order. A held request
// whose partners don't arrive within `timeout` ms gets a 504 naming how many did, as Python's
// gate.wait(timeout=10) failed, so one device failing early can't hang the test.
export async function barrierProxy(
  origin,
  hold,
  { count = 2, after = false, timeout = 10_000 } = {},
) {
  const calls = [];
  const replies = [];
  const waiting = [];
  let release;
  const met = new Promise((resolve) => (release = resolve));
  const meet = async (call) => {
    if (call !== hold || waiting.length >= count) return;
    waiting.push(call);
    if (waiting.length === count) release();
    let timer;
    const late = new Promise((_, reject) => {
      timer = setTimeout(
        () => reject(new Error(`barrier timed out with ${waiting.length} of ${count} arrivals`)),
        timeout,
      );
    });
    try {
      await Promise.race([met, late]);
    } finally {
      clearTimeout(timer);
    }
  };
  const server = createServer(async (req, res) => {
    try {
      await forward(req, res);
    } catch (error) {
      res.writeHead(504).end(error.message);
    }
  });
  async function forward(req, res) {
    const call = `${req.method} ${req.url}`;
    calls.push(call);
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    if (!after) await meet(call);
    const headers = Object.fromEntries(
      Object.entries(req.headers).filter(
        ([k]) => !["host", "connection", "content-length"].includes(k),
      ),
    );
    const r = await fetch(origin + req.url, {
      method: req.method,
      headers,
      body: chunks.length ? Buffer.concat(chunks) : undefined,
      redirect: "manual",
    });
    const body = Buffer.from(await r.arrayBuffer());
    const out = Object.fromEntries(
      [...r.headers].filter(
        ([k]) => !["content-encoding", "content-length", "transfer-encoding"].includes(k),
      ),
    );
    replies.push(`${call} ${r.status}`);
    if (after) await meet(call);
    res.writeHead(r.status, out).end(body);
  }
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return {
    origin: `http://127.0.0.1:${server.address().port}`,
    calls,
    replies,
    close: () =>
      new Promise((resolve) => {
        server.close(resolve);
        server.closeAllConnections();
      }),
  };
}

const BIN =
  process.env.FACTORY_BIN ?? new URL("../../rust/target/host/factory", import.meta.url).pathname;
// The host's agent, session and Git markers would change what the binary does, so the run drops them.
const DROPPED = /^(FACTORY_|PASEO_|CLAUDE|AI_AGENT$|OPENCODE_|GIT_)/;

// Run the factory binary as one device: `factory --home <home> ...args`. Resolves its exit code and output.
export function factory(home, ...args) {
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !DROPPED.test(k)));
  return new Promise((resolve, reject) => {
    const child = spawn(BIN, ["--home", home, ...args], { env, cwd: home });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.once("error", reject);
    child.once("close", (code) => resolve({ code, stdout, stderr }));
  });
}
