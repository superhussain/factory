// Parity runner: run each parity case on a factory binary and compare it with the case's golden files.
//
// node tests/parity/run.mjs --bin PATH [--ids FILE|DIR] [--case ID]... [--jobs N] [--shard K/N] [--record]
//
// Each case runs in its own temp root with its own home, stub folder and HTTP port, so cases run in a pool of
// worker threads. The slow cases start first, and results print in case order. --shard K/N keeps every Nth case
// starting at the Kth, for a CI matrix. --record writes each selected case's normalized output to tests/parity/golden/<case-id>/ instead of
// comparing it.
//
// Byte data stays in latin1 strings, one character per byte, so every comparison and replacement is byte-exact.
import { fork, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  accessSync,
  constants as fsConstants,
  copyFileSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { createServer } from "node:http";
import { availableParallelism, hostname, constants as osConstants, tmpdir } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { gunzipSync, gzipSync } from "node:zlib";
import { launcher } from "./stub.mjs";

// A string's UTF-8 bytes as a latin1 string, and back.
const bytes = (s) => Buffer.from(s, "utf8").toString("latin1");
const text = (b) => Buffer.from(b, "latin1").toString("utf8");

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, "..", "..");
// claude is stubbed so a host's own claude on PATH never changes a case: bench checks for it.
const TOOLS = ["gh", "paseo", "git", "crontab", "claude"];
const CASES = join(HERE, "cases");
const GOLDEN = join(HERE, "golden");
const REQUEST_HEADERS = [
  "content-type",
  "if-match",
  "if-none-match",
  "cf-access-client-id",
  "cf-access-client-secret",
  "user-agent",
  "authorization",
];
const FIELDS = [
  "exit code",
  "stdout",
  "stderr",
  "home tree",
  ...TOOLS.map((tool) => `${tool} calls`),
  "requests",
];
// A golden's dates relative to the run's UTC date. --record writes these placeholders, and a compare expands them.
const DAYS = { "{today}": 0, "{today-6}": 6 };
// Every case runs at this instant: the harness's dates use it, and the binary reads it from FACTORY_PARITY_NOW, so no
// golden depends on the day it runs.
const NOW = Date.parse("2026-11-24T12:00:00Z");
// The host's OS label (host_os in the binary) in host-status.json and its PUT body, raw or JSON-escaped.
const HOST_OS = /(\\?"os\\?": \\?")(?:Linux \(WSL\)|Linux|macOS|Windows|Other)(?=\\?")/g;
// The binary's PICKUP_SCHEDULER on this host, and its value in a "scheduler" field, raw or JSON-escaped.
const PICKUP_SCHEDULER = process.platform === "darwin" ? "launchd" : "cron";
const SCHEDULER = new RegExp(`(\\\\?"scheduler\\\\?": \\\\?")${PICKUP_SCHEDULER}(?=\\\\?")`, "g");
// This host's ECONNREFUSED message, which reads as Linux's 111 because the goldens came from Linux.
const REFUSED = new RegExp(`\\[Errno ${osConstants.errno.ECONNREFUSED}\\] Connection refused`, "g");
// This host's name, where a device name falls back to it, as a whole word so a short name never matches inside a path.
const HOST = hostname()
  ? new RegExp(
      `(?<![\\w.-])${bytes(hostname()).replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")}(?![\\w.-])`,
      "g",
    )
  : null;
const ELAPSED = /"elapsed": \d+/g;
const STAMP = /\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ/g;
const DROPPED = new Set([
  "FACTORY_HOME",
  "FACTORY_SESSION",
  "PASEO_HOME",
  "PASEO_AGENT_ID",
  "CLAUDE_CONFIG_DIR",
  // CLAUDECODE, AI_AGENT and OPENCODE_TERMINAL mark an agent's shell, where autonomy commands refuse.
  "CLAUDECODE",
  "AI_AGENT",
  "OPENCODE_TERMINAL",
  "GIT_DIR",
  "GIT_WORK_TREE",
  "GIT_COMMON_DIR",
  "GIT_INDEX_FILE",
]);

// A number the case files spell as a float, or an integer past 2^53, keeps Python's spelling.
class PyNumber {
  constructor(source) {
    this.source = source;
  }
}

function pyFloat(n) {
  if (!Number.isFinite(n)) return n > 0 ? "Infinity" : n < 0 ? "-Infinity" : "NaN";
  const [mantissa, exp] = n.toExponential().split("e");
  const e = Number(exp);
  const digits = mantissa.replace("-", "").replace(".", "");
  const sign = n < 0 || Object.is(n, -0) ? "-" : "";
  if (e < -4 || e >= 16) {
    const body = digits.length > 1 ? digits[0] + "." + digits.slice(1) : digits;
    return `${sign}${body}e${e < 0 ? "-" : "+"}${String(Math.abs(e)).padStart(2, "0")}`;
  }
  if (e < 0) return `${sign}0.${"0".repeat(-e - 1)}${digits}`;
  const whole = digits.slice(0, e + 1).padEnd(e + 1, "0");
  return `${sign}${whole}.${digits.slice(e + 1) || "0"}`;
}

function parseCaseJson(source) {
  return JSON.parse(source, (key, value, context) => {
    if (typeof value !== "number") return value;
    if (/[.eE]/.test(context.source)) return new PyNumber(pyFloat(value));
    if (!Number.isSafeInteger(value)) return new PyNumber(BigInt(context.source).toString());
    return value;
  });
}

const isObject = (v) =>
  v !== null && typeof v === "object" && !Array.isArray(v) && !(v instanceof PyNumber);

// Python's json.dumps(v, sort_keys=sorted): ", " and ": " separators and ASCII escapes.
function pyDumps(v, sorted = false) {
  if (v instanceof PyNumber) return v.source;
  if (Array.isArray(v)) return "[" + v.map((x) => pyDumps(x, sorted)).join(", ") + "]";
  if (isObject(v)) {
    const keys = Object.keys(v);
    if (sorted) keys.sort();
    return "{" + keys.map((k) => pyString(k) + ": " + pyDumps(v[k], sorted)).join(", ") + "}";
  }
  if (typeof v === "string") return pyString(v);
  if (v === null) return "null";
  return String(v);
}

function pyString(s) {
  return JSON.stringify(s).replace(
    /[^ -~]/g,
    (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"),
  );
}

// Python's repr() of a bytes value, from a latin1 string.
function bytesRepr(b) {
  const q = b.includes("'") && !b.includes('"') ? '"' : "'";
  let out = "b" + q;
  for (const c of b) {
    const n = c.charCodeAt(0);
    if (c === q || c === "\\") out += "\\" + c;
    else if (c === "\t") out += "\\t";
    else if (c === "\n") out += "\\n";
    else if (c === "\r") out += "\\r";
    else if (n < 0x20 || n >= 0x7f) out += "\\x" + n.toString(16).padStart(2, "0");
    else out += c;
  }
  return out + q;
}

// Python's bytes.decode("utf-8", "surrogateescape") from a latin1 string: each undecodable byte becomes U+DC80-U+DCFF.
function surrogateEscape(b) {
  const buffer = Buffer.from(b, "latin1");
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(buffer);
  } catch {
    let out = "";
    let i = 0;
    while (i < buffer.length) {
      const n = buffer[i];
      const size =
        n < 0x80
          ? 1
          : n >= 0xc2 && n < 0xe0
            ? 2
            : n >= 0xe0 && n < 0xf0
              ? 3
              : n >= 0xf0 && n < 0xf5
                ? 4
                : 0;
      if (size === 1) {
        out += String.fromCharCode(n);
        i += 1;
        continue;
      }
      if (size) {
        try {
          out += new TextDecoder("utf-8", { fatal: true }).decode(buffer.subarray(i, i + size));
          i += size;
          continue;
        } catch {
          // Falls through to escape this one byte.
        }
      }
      out += String.fromCharCode(0xdc00 + n);
      i += 1;
    }
    return out;
  }
}

function isolatedEnv(home) {
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (DROPPED.has(key) || key.startsWith("GIT_CONFIG_") || key.toLowerCase().endsWith("_proxy"))
      continue;
    env[key] = value;
  }
  return Object.assign(env, {
    HOME: home,
    USERPROFILE: home,
    XDG_CONFIG_HOME: join(home, "config"),
    NO_PROXY: "localhost,127.0.0.1",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: join(home, "gitconfig"),
    GIT_AUTHOR_NAME: "Test",
    GIT_AUTHOR_EMAIL: "test@example.invalid",
    GIT_COMMITTER_NAME: "Test",
    GIT_COMMITTER_EMAIL: "test@example.invalid",
  });
}

// Python's Path ordering: by path parts, so "a/b" sorts before "a-b".
function compareParts(a, b) {
  const x = a.split("/");
  const y = b.split("/");
  for (let i = 0; i < Math.min(x.length, y.length); i++)
    if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
  return x.length - y.length;
}

// Every path under root, as pathlib's rglob("*") finds them: symlinked folders are listed, not entered.
function walk(root, folder = root, out = []) {
  for (const name of readdirSync(folder, { encoding: "buffer" })) {
    const path = join(folder, name.toString("latin1"));
    out.push(path);
    if (lstatSync(Buffer.from(path, "latin1")).isDirectory()) walk(root, path, out);
  }
  return out;
}

// [relative path, mode, latin1 bytes or null] for root and everything under it.
function snapshot(root) {
  const rootBytes = bytes(root);
  const paths = walk(rootBytes).sort((a, b) => compareParts(a, b));
  return [rootBytes, ...paths].map((path) => {
    const raw = Buffer.from(path, "latin1");
    const info = statSync(raw);
    return [
      path === rootBytes ? "." : relative(rootBytes, path),
      info.mode & 0o7777,
      info.isFile() ? readFileSync(raw).toString("latin1") : null,
    ];
  });
}

export async function httpFixture(routes) {
  const requests = [];
  const server = createServer((req, res) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      const headers = [];
      for (let i = 0; i < req.rawHeaders.length; i += 2)
        headers.push([req.rawHeaders[i], req.rawHeaders[i + 1]]);
      requests.push([req.method, req.url, headers, Buffer.concat(chunks).toString("latin1")]);
      let route = routes[`${req.method} ${req.url}`] ?? routes[req.url] ?? [404, ""];
      // A list serves its replies in order, and the last one repeats.
      if (Array.isArray(route[0])) route = route.length > 1 ? route.shift() : route[0];
      const [status, body, extra = {}] = route;
      const data = Buffer.from(typeof body === "string" ? body : pyDumps(body), "latin1");
      const out = { ETag: '"' + createHash("sha256").update(data).digest("hex") + '"' };
      if (status === 302) out.Location = "https://example.invalid/login";
      Object.assign(out, extra);
      res.writeHead(status, out);
      res.end(data);
    });
  });
  await new Promise((done) => server.listen(0, "127.0.0.1", done));
  return {
    requests,
    origin: `http://127.0.0.1:${server.address().port}`,
    close: () =>
      new Promise((done) => {
        server.close(done);
        server.closeAllConnections();
      }),
  };
}

export function loadCases(folder = CASES) {
  const cases = [];
  for (const name of readdirSync(folder)
    .filter((n) => n.endsWith(".json"))
    .sort()) {
    for (const c of parseCaseJson(readFileSync(join(folder, name), "utf8")).cases) {
      const prefix = c.id.split("/")[0];
      if (prefix !== basename(name, ".json"))
        throw new Error(`parity: ${name}: case ${c.id} belongs in ${prefix}.json`);
      cases.push(c);
    }
  }
  const cli = JSON.parse(readFileSync(join(ROOT, "tests/fixtures/cli-v1.json"), "utf8"));
  for (const row of cli.commands) {
    if (row.name === "ideas") continue;
    for (const count of [row.min_args - 1, row.max_args + 1])
      if (count >= 0)
        cases.push({
          id: `arity/${row.name}/${count}`,
          argv: [row.name, ...Array(count).fill("operand")],
          home: null,
          stubs: {},
        });
  }
  const seen = new Set();
  for (const c of cases) {
    if (seen.has(c.id)) throw new Error(`parity: duplicate case id ${c.id}`);
    seen.add(c.id);
  }
  return cases;
}

export function readIds(path) {
  const folder = statSync(path).isDirectory();
  const files = folder
    ? readdirSync(path)
        .filter((n) => n.endsWith(".txt"))
        .sort()
        .map((n) => join(path, n))
    : [path];
  const patterns = [];
  for (const file of files) {
    const stem = basename(file, ".txt");
    for (const line of readFileSync(file, "utf8").split(/\r\n|\n|\r/)) {
      const pattern = line.split("#")[0].trim();
      if (pattern && folder && !pattern.startsWith(stem + "/"))
        throw new Error(`parity: ${basename(file)}: pattern ${pattern} must start with ${stem}/`);
      if (pattern) patterns.push(pattern);
    }
  }
  return patterns;
}

// Python's fnmatch.fnmatchcase: *, ?, [seq] and [!seq].
function fnmatch(name, pattern) {
  let re = "";
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === "*") re += ".*";
    else if (c === "?") re += ".";
    else if (c === "[") {
      const end = pattern.indexOf("]", i + 2);
      if (end < 0) re += "\\[";
      else {
        let set = pattern.slice(i + 1, end).replaceAll("\\", "\\\\");
        if (set[0] === "!") set = "^" + set.slice(1);
        else if (set[0] === "^") set = "\\" + set;
        re += "[" + set + "]";
        i = end;
      }
    } else re += c.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
  }
  return new RegExp("^(?:" + re + ")$", "s").test(name);
}

// Text with each {name} placeholder in values replaced by its value.
export function expand(s, values) {
  for (const [name, value] of Object.entries(values)) s = s.replaceAll("{" + name + "}", value);
  return s;
}

// The case's routes for httpFixture, as fresh lists because list replies pop. A string body is sent as text.
// Keys and string bodies get their placeholders expanded. Bodies are latin1 byte strings.
function routesFor(c, values) {
  const reply = ([status, body, ...extra]) => [
    status,
    typeof body === "string" ? bytes(expand(body, values)) : body,
    ...extra,
  ];
  return Object.fromEntries(
    Object.entries(c.routes).map(([key, value]) => [
      expand(key, values),
      Array.isArray(value[0]) ? value.map(reply) : reply(value),
    ]),
  );
}

export function requestLines(server, normalize) {
  return server.requests.map(([method, path, headers, body]) => {
    const kept = {};
    for (const [name, value] of headers)
      if (REQUEST_HEADERS.includes(name.toLowerCase())) kept[name.toLowerCase()] = value;
    const line = pyDumps({ method, path, headers: kept, body: surrogateEscape(body) }, true);
    return normalize(line);
  });
}

function utcDate(daysBack) {
  return new Date(NOW - daysBack * 86400_000).toISOString().slice(0, 10);
}

function nowStamps(data, start, end) {
  return data.replace(STAMP, (match) => {
    const ms = Date.parse(match);
    if (Number.isNaN(ms) || new Date(ms).toISOString().slice(0, 19) + "Z" !== match) return match;
    const seconds = ms / 1000;
    return start <= seconds && seconds <= end ? "{now}" : match;
  });
}

// The command run from a copy of its runtime in the case's skill folder, beside the case's bundle.json.
function placeSkill(c, command, values) {
  if (!("skill" in c)) return command;
  const skill = expand(c.skill, values);
  mkdirSync(join(skill, "runtime"), { recursive: true });
  copyFileSync(command[0], join(skill, "runtime", "factory"));
  if ("bundle" in c) writeFileSync(join(skill, "bundle.json"), expand(c.bundle, values));
  return [join(skill, "runtime", "factory")];
}

function signalCode(signal) {
  return -(osConstants.signals[signal] ?? 0);
}

function execute(command, args, env, cwd, stdin) {
  return new Promise((done) => {
    const child = spawn(command[0], [...command.slice(1), ...args], {
      env,
      cwd,
      stdio: [stdin === undefined ? "ignore" : "pipe", "pipe", "pipe"],
    });
    const out = [];
    const err = [];
    child.stdout.on("data", (chunk) => out.push(chunk));
    child.stderr.on("data", (chunk) => err.push(chunk));
    const timer = setTimeout(() => child.kill("SIGKILL"), 60_000);
    child.on("close", (code, signal) => {
      clearTimeout(timer);
      done({
        code: code ?? signalCode(signal),
        stdout: Buffer.concat(out).toString("latin1"),
        stderr: Buffer.concat(err).toString("latin1"),
      });
    });
    if (stdin !== undefined) child.stdin.end(Buffer.from(stdin, "utf8"));
  });
}

export async function runSide(c, command, homes = join(HERE, "homes")) {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "parity-")));
  let server;
  try {
    const home = join(root, "home");
    const binDir = join(root, "bin");
    const stubDir = join(root, "stub");
    const routes = {};
    if ("routes" in c) server = await httpFixture(routes);
    const values = { home, tmp: root, today: utcDate(0), root: ROOT };
    if (server) {
      values.api = server.origin;
      Object.assign(routes, routesFor(c, values));
    }
    if (c.home) {
      cpSync(join(homes, c.home), home, {
        recursive: true,
        dereference: true,
        preserveTimestamps: true,
      });
      const byteValues = Object.fromEntries(Object.entries(values).map(([k, v]) => [k, bytes(v)]));
      for (const path of walk(bytes(home))) {
        const raw = Buffer.from(path, "latin1");
        if (lstatSync(raw).isFile())
          writeFileSync(
            raw,
            Buffer.from(expand(readFileSync(raw).toString("latin1"), byteValues), "latin1"),
          );
      }
    } else mkdirSync(home);
    mkdirSync(binDir);
    mkdirSync(stubDir);
    for (const tool of TOOLS) launcher(binDir, tool);
    // Stub files are JSON text, so each value goes in JSON-escaped.
    const jsonValues = Object.fromEntries(
      Object.entries(values).map(([k, v]) => [k, JSON.stringify(v).slice(1, -1)]),
    );
    for (const [tool, entries] of Object.entries(c.stubs))
      writeFileSync(join(stubDir, `${tool}.json`), expand(JSON.stringify(entries), jsonValues));
    command = placeSkill(c, command, values);
    const env = isolatedEnv(home);
    // A side gets only the fake token from home/.env, so a real one never reaches the fixture or a diff.
    delete env.FACTORY_ACCESS_CLIENT_ID;
    delete env.FACTORY_ACCESS_CLIENT_SECRET;
    // The binary's local dates are UTC dates, so a golden holds the same dates in every zone.
    env.TZ = "UTC";
    env.FACTORY_PARITY_NOW = String(NOW / 1000);
    Object.assign(env, c.env ?? {}, {
      PATH: binDir + ":" + (process.env.PATH ?? ""),
      FACTORY_STUB_DIR: stubDir,
    });
    const argv = c.argv.map((arg) => arg.replaceAll("{home}", home));
    if (server) {
      writeFileSync(
        join(home, "config.md"),
        `---\napi_url: ${server.origin}\ndevice: parity\n---\n`,
      );
      writeFileSync(
        join(home, ".env"),
        "FACTORY_ACCESS_CLIENT_ID=parity-client-id\nFACTORY_ACCESS_CLIENT_SECRET=parity-client-secret\n",
      );
    }
    const start = NOW / 1000;
    const result = await execute(command, argv, env, root, c.stdin);
    const end = NOW / 1000;
    if (server) await server.close();

    const runtime = realpathSync(command.at(-1));
    const swaps = [];
    if (server) swaps.push([server.origin, "{api}"]);
    // The goldens came from Python, which printed factory.py's path where the binary prints its own;
    // both read {runtime}. The skill root is two folders up.
    swaps.push([join(dirname(runtime), "factory.py"), "{runtime}"]);
    // In a skill folder the binary sits in runtime/ and reads {bin}.
    if ("skill" in c) swaps.push([join(dirname(runtime), "factory"), "{bin}"]);
    swaps.push(
      [runtime, "{runtime}"],
      [resolve(command.at(-1)), "{runtime}"],
      [dirname(dirname(runtime)), "{skill}"],
      [home, "{home}"],
      [root, "{tmp}"],
      [ROOT, "{root}"],
    );
    const normalize = (data) => {
      for (const [from, to] of swaps) data = data.replaceAll(bytes(from), to);
      data = nowStamps(data, start, end)
        .replace(ELAPSED, '"elapsed": {n}')
        .replace(HOST_OS, "$1{os}")
        .replace(REFUSED, "[Errno 111] Connection refused");
      if (c.host_scheduler) data = data.replace(SCHEDULER, "$1{scheduler}");
      return HOST ? data.replace(HOST, "{host}") : data;
    };

    const tree = snapshot(home).map(([path, mode, data]) =>
      data === null
        ? `${path} ${mode.toString(8)}`
        : `${path} ${mode.toString(8)} ${bytesRepr(normalize(data))}`,
    );
    const fields = [
      ["exit code", String(result.code)],
      ["stdout", normalize(result.stdout)],
      ["stderr", c.skip_stderr ? "" : normalize(result.stderr)],
      ["home tree", tree.join("\n")],
    ];
    for (const tool of TOOLS) {
      const log = join(stubDir, `${tool}.calls.jsonl`);
      fields.push([`${tool} calls`, existsSync(log) ? normalize(readFileSync(log, "latin1")) : ""]);
    }
    fields.push(["requests", server ? requestLines(server, normalize).join("\n") : ""]);
    return fields;
  } finally {
    if (server) await server.close();
    rmSync(root, { recursive: true, force: true });
  }
}

// A field's golden file. The home tree repeats each home's fixtures, so it is gzipped.
function goldenFile(folder, field) {
  const name = field.replaceAll(" ", "-");
  return join(folder, field === "home tree" ? name + ".gz" : name);
}

// The case's golden fields as latin1 strings, or null when it has no golden folder. A missing file is empty.
function readGolden(c) {
  const folder = join(GOLDEN, c.id);
  if (!existsSync(folder)) return null;
  return FIELDS.map((field) => {
    const path = goldenFile(folder, field);
    let data = existsSync(path) ? readFileSync(path) : Buffer.alloc(0);
    if (data.length && path.endsWith(".gz")) data = gunzipSync(data);
    return [field, data.toString("latin1")];
  });
}

// A golden whose content did not change keeps its gzip bytes, since another zlib compresses differently.
function writeGolden(c, fields) {
  const folder = join(GOLDEN, c.id);
  const old = Object.fromEntries(readGolden(c) ?? []);
  const oldTree = goldenFile(folder, "home tree");
  const keptTree = existsSync(oldTree) ? readFileSync(oldTree) : null;
  rmSync(folder, { recursive: true, force: true });
  mkdirSync(folder, { recursive: true });
  for (const [field, value] of fields) {
    if (!value && field !== "exit code") continue;
    const path = goldenFile(folder, field);
    const data = Buffer.from(value, "latin1");
    if (!path.endsWith(".gz")) writeFileSync(path, data);
    else if (keptTree && old[field] === value) writeFileSync(path, keptTree);
    else writeFileSync(path, gzipSync(data, { level: 9 }));
  }
}

// A short unified-style diff: the changed middle of want and got, after their common first and last lines.
function diff(want, got) {
  const a = want.split("\n");
  const b = got.split("\n");
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  )
    tail++;
  return [
    "--- golden",
    "+++ bin",
    `@@ line ${head + 1} @@`,
    ...a.slice(head, a.length - tail).map((line) => "-" + text(line)),
    ...b.slice(head, b.length - tail).map((line) => "+" + text(line)),
  ];
}

export async function runCase(c, command, record) {
  let fields = await runSide(c, command);
  if (c.host_scheduler && PICKUP_SCHEDULER !== "cron") {
    const i = FIELDS.indexOf("crontab calls");
    fields[i][1] = readGolden(c)?.[i][1] ?? "";
  }
  const dates = Object.fromEntries(
    Object.entries(DAYS).map(([name, back]) => [name, utcDate(back)]),
  );
  if (record) {
    // A date the case itself spells out is fixture data, not today's date, so it stays literal.
    const written = JSON.stringify(c, (k, v) => (v instanceof PyNumber ? v.source : v));
    for (const [name, date] of Object.entries(dates))
      if (!written.includes(date))
        fields = fields.map(([field, value]) => [field, value.replaceAll(date, name)]);
    if (("routes" in c || "exit" in c) && fields[0][1] !== String(c.exit ?? "None"))
      return [`FAIL ${c.id}: exit ${fields[0][1]}, case expects ${c.exit ?? "None"}`];
    writeGolden(c, fields);
    return [];
  }
  const golden = readGolden(c);
  if (golden === null)
    return [`FAIL ${c.id}: no golden folder ${join(GOLDEN, c.id)}; run with --record`];
  const days = Object.fromEntries(
    Object.entries(dates).map(([name, date]) => [name.slice(1, -1), date]),
  );
  for (let i = 0; i < golden.length; i++) {
    const [field, stored] = golden[i];
    const want = expand(stored, days);
    const got = fields[i][1];
    if (want !== got) return [`FAIL ${c.id}: ${field} differs`, ...diff(want, got).slice(0, 40)];
  }
  return [];
}

// The cases that take seconds on a CI runner: each retries a conflicting write 20 times with 50-500 ms of
// jitter. Fed last in case order, the slowest started about 11 s into its shard and ended it at about 20 s.
export const SLOW = [
  "pipeline/batch-failed-rollback-names-riders-left",
  "pipeline/batch-backlog-412-rolls-back-riders-and-exits-5",
];

// The case indices in the order the workers take them: SLOW's cases first, then the rest in case order.
export function feedOrder(cases) {
  const slow = SLOW.map((id) => cases.findIndex((c) => c.id === id)).filter((i) => i >= 0);
  return [...slow, ...Array.from(cases.keys()).filter((i) => !slow.includes(i))];
}

function usage(message) {
  console.error(message);
  return 2;
}

async function main(argv) {
  let args;
  try {
    ({ values: args } = parseArgs({
      args: argv,
      options: {
        bin: { type: "string" },
        ids: { type: "string" },
        case: { type: "string", multiple: true, default: [] },
        jobs: { type: "string", default: String(availableParallelism()) },
        shard: { type: "string", default: "1/1" },
        record: { type: "boolean", default: false },
      },
    }));
  } catch (error) {
    return usage(`run.mjs: ${error.message}`);
  }
  const jobs = Number(args.jobs);
  const shard = /^([1-9]\d*)\/([1-9]\d*)$/.exec(args.shard);
  if (!Number.isInteger(jobs) || jobs < 1 || !shard || Number(shard[1]) > Number(shard[2]))
    return usage("parity: --jobs takes a positive count and --shard takes K/N with 1 <= K <= N");
  if (process.platform === "win32") return usage("parity: POSIX hosts only in phase 0");
  if (args.ids && !existsSync(args.ids)) return usage(`parity: no such file or folder ${args.ids}`);
  const binary = args.bin || process.env.FACTORY_BIN || "";
  if (binary) {
    let ok = false;
    try {
      ok = statSync(binary).isFile();
      accessSync(binary, fsConstants.X_OK);
    } catch {
      ok = false;
    }
    if (!ok) return usage(`parity: not executable ${binary}`);
  }
  let cases;
  let patterns;
  try {
    cases = loadCases();
    patterns = [...args.case, ...(args.ids ? readIds(args.ids) : [])];
  } catch (error) {
    return usage(error.message);
  }
  for (const pattern of patterns)
    if (!cases.some((c) => fnmatch(c.id, pattern)))
      return usage(`parity: no case matches ${pattern}`);
  if (patterns.length) cases = cases.filter((c) => patterns.some((p) => fnmatch(c.id, p)));
  const [k, n] = [Number(shard[1]), Number(shard[2])];
  cases = cases.filter((_, i) => i >= k - 1 && (i - (k - 1)) % n === 0);
  if (!binary) return usage("parity: --bin or FACTORY_BIN required");
  const command = [resolve(binary)];

  // Worker processes, each running one case at a time, so the harness's own file work overlaps too.
  // Processes, not threads: a thread's half-written copy of the binary can be open in another
  // thread's forked child, and exec then fails with ETXTBSY.
  const results = Array.from({ length: cases.length });
  let printed = 0;
  let failed = 0;
  let next = 0;
  const order = feedOrder(cases);
  const flush = () => {
    while (printed < cases.length && results[printed] !== undefined) {
      const lines = results[printed];
      console.log(lines.length ? lines.join("\n") : `PASS ${cases[printed].id}`);
      if (lines.length) failed += 1;
      printed += 1;
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(jobs, cases.length) }, () => {
      const worker = fork(fileURLToPath(import.meta.url), [
        "--worker",
        JSON.stringify({ command, record: args.record }),
      ]);
      return new Promise((done, fail) => {
        const feed = () => {
          if (next >= cases.length) {
            worker.disconnect();
            return done();
          }
          const i = order[next++];
          worker.send([i, cases[i].id]);
        };
        worker.on("message", ([i, lines]) => {
          results[i] = lines;
          flush();
          feed();
        });
        worker.on("error", fail);
        worker.on("exit", (code) => {
          if (next < cases.length || results.includes(undefined))
            fail(new Error(`parity: a worker exited ${code}`));
        });
        feed();
      });
    }),
  );
  flush();
  console.log(
    `parity: ${cases.length - failed} ${args.record ? "recorded" : "passed"}, ${failed} failed`,
  );
  return failed ? 1 : 0;
}

if (process.argv[2] === "--worker") {
  // A worker loads the same case list and runs the cases the main process names.
  const { command, record } = JSON.parse(process.argv[3]);
  const cases = new Map(loadCases().map((c) => [c.id, c]));
  process.on("message", async ([i, id]) => {
    process.send([i, await runCase(cases.get(id), command, record)]);
  });
} else if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exitCode = await main(process.argv.slice(2));
}
