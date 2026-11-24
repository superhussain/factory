#!/usr/bin/env node
// Private deployment authority: one JSON profile projects the fixed Wrangler config.
//
// Commands: check | render | dry-run | deploy, each with --profile <file>.
// Only ignored deploy/profile.json is deployment input. Exit 2 for invalid
// profile/options, 1 for failed build/Wrangler checks, 0 on success.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { getSystemErrorMessage } from "node:util";

const COMMANDS = ["check", "render", "dry-run", "deploy"];
const COMPATIBILITY_DATE = "2026-06-01";
const OBSERVABILITY = { enabled: true, head_sampling_rate: 1, redact_query_string: true };
const PROFILE_KEYS = [
  "version",
  "account_id",
  "worker_name",
  "bucket_name",
  "origin",
  "title",
  "issuer",
  "audience",
  "image_origins",
  "agent_links",
];
const OPTIONAL_PROFILE_KEYS = ["jira_origin", "embed_origins", "tracer_receipt_key"];
const DESKTOP_EMBED_ORIGINS = ["paseo://app"];

const DNS = /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]*$/;
// oxlint-disable-next-line no-control-regex
const CONTROLS = /[\x00-\x1f\x7f-\x9f]/;
export const ACCOUNT_ID = /^[0-9a-f]{32}$/;
const WORKER_NAME = /^[a-z0-9-]{1,63}$/;
const BUCKET_NAME = /^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/;
const IPV4_SHAPED = /^\d+\.\d+\.\d+\.\d+$/;

export const ROOT = fileURLToPath(new URL("..", import.meta.url));

export class ProfileError extends Error {}

function fail(message) {
  throw new ProfileError(message);
}

const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

function text(value, name, maximum) {
  if (typeof value !== "string") fail(name + " must be a string");
  if (!value.trim()) fail(name + " must be nonempty");
  if (CONTROLS.test(value)) fail(name + " must not contain control characters");
  if (value.includes("REPLACE_ME") || value.includes("${") || /<[a-z][a-z0-9-]*>/.test(value))
    fail(name + " contains an unresolved placeholder");
  if (maximum !== undefined && [...value].length > maximum)
    fail(name + " is over " + maximum + " code points");
  return value;
}

export function httpsOrigin(value, name) {
  text(value, name);
  const match = /^https:\/\/([^/?#]+)$/.exec(value);
  if (!match) fail(name + " must be a canonical https origin without path, query or fragment");
  const host = match[1];
  if (host.includes("@")) fail(name + " must not contain credentials");
  if (host.includes(":")) fail(name + " must not contain an explicit port");
  if (host.startsWith("xn--") || host.includes("*"))
    fail(name + " must not be a wildcard or punycode-ata origin");
  if (!DNS.test(host)) fail(name + " needs a valid DNS hostname");
  return host;
}

function originList(value, name) {
  if (!Array.isArray(value) || value.some((v) => typeof v !== "string"))
    fail(name + " must be a list of origin strings");
  if (new Set(value).size !== value.length) fail(name + " must not contain duplicates");
}

function imageOrigins(value) {
  originList(value, "image_origins");
  for (const origin of value) httpsOrigin(origin, "image_origins entry");
  return [...value];
}

export function embedOrigins(value) {
  originList(value, "embed_origins");
  for (const origin of value) {
    if (DESKTOP_EMBED_ORIGINS.includes(origin)) continue;
    if (!origin.startsWith("https://"))
      fail("embed_origins entry must be a canonical https origin or paseo://app");
    httpsOrigin(origin, "embed_origins entry");
  }
  return [...value];
}

function receiptKey(value) {
  if (
    typeof value === "string" &&
    value.length % 4 === 0 &&
    /^[A-Za-z0-9+/]*={0,2}$/.test(value) &&
    Buffer.from(value, "base64").length === 32
  )
    return value;
  fail("tracer_receipt_key must be the base64 of a 32-byte Ed25519 public key");
}

function linkUrl(template, kind) {
  const substituted = template
    .replaceAll("{server}", "server-id")
    .replaceAll("{agent}", "agent-id");
  if (substituted.startsWith("//")) fail(kind + " link template must not be protocol-relative");
  const match = /^([a-z][a-z0-9+.-]*):(.*)$/s.exec(substituted);
  if (!match) fail(kind + " link template must be an absolute URL");
  const scheme = match[1];
  if (["javascript", "data", "file", "blob", "http"].includes(scheme))
    fail(kind + " link template scheme " + scheme + " is not allowed");
  if (kind === "web" && scheme !== "https") fail("web link template must be https");
  if (kind === "app" && (scheme === "http" || scheme === "https"))
    fail("app link template needs a non-http custom scheme");
  if (/\/\/[^/]*@/.test(substituted)) fail(kind + " link template must not contain credentials");
  if (CONTROLS.test(substituted)) fail(kind + " link template must not contain control characters");
}

function agentLinks(value) {
  if (!isObject(value)) fail("agent_links must be an object");
  for (const [kind, link] of Object.entries(value)) {
    if (kind !== "app" && kind !== "web") fail("agent_links allows only app and web keys");
    if (!isObject(link) || Object.keys(link).sort().join() !== "label,template")
      fail(kind + " link needs exactly label and template");
    text(link.label, kind + " link label", 160);
    const template = text(link.template, kind + " link template");
    const placeholders = template.match(/\{[^{}]*\}/g) ?? [];
    if (
      placeholders.length !== 2 ||
      !placeholders.includes("{server}") ||
      !placeholders.includes("{agent}")
    )
      fail(kind + " link template needs exactly the {server} and {agent} placeholders");
    linkUrl(template, kind);
  }
  return { ...value };
}

// The first key that repeats inside one object of already-valid JSON text, or undefined.
function duplicateKey(raw) {
  const stack = [];
  const token = /"(?:[^"\\]|\\.)*"|[{}[\]:]/gs;
  let previous;
  for (const [t] of raw.matchAll(token)) {
    if (t === "{") stack.push(new Set());
    else if (t === "[") stack.push(null);
    else if (t === "}" || t === "]") stack.pop();
    else if (t === ":") {
      const keys = stack.at(-1);
      const key = JSON.parse(previous);
      if (keys.has(key)) return key;
      keys.add(key);
    }
    previous = t;
  }
  return undefined;
}

function loadObject(path) {
  let raw;
  try {
    raw = readFileSync(path, "utf8");
  } catch (error) {
    const message = error.errno ? getSystemErrorMessage(error.errno) : error.message;
    fail("cannot read profile: " + message[0].toUpperCase() + message.slice(1));
  }
  let data;
  try {
    data = JSON.parse(raw);
  } catch (error) {
    // JSON.parse's message can quote the input, and deploy-main prints this into a public log,
    // so only a position goes out.
    const at = /at position (\d+)/.exec(error.message);
    if (!at) fail("profile is not valid JSON");
    const before = raw.slice(0, Number(at[1])).split("\n");
    fail(
      `profile is not valid JSON: line ${before.length} column ${before.at(-1).length + 1} (char ${at[1]})`,
    );
  }
  const duplicate = duplicateKey(raw);
  if (duplicate !== undefined) fail("duplicate key " + JSON.stringify(duplicate));
  if (!isObject(data)) fail("profile must be a JSON object");
  return data;
}

// Read and fully validate the private profile into a frozen boundary value.
export function loadProfile(path) {
  const data = loadObject(path);
  const unknown = Object.keys(data)
    .filter((key) => !PROFILE_KEYS.includes(key) && !OPTIONAL_PROFILE_KEYS.includes(key))
    .sort();
  if (unknown.length) fail("unknown profile keys: " + unknown.join(", "));
  const missing = PROFILE_KEYS.filter((key) => !(key in data));
  if (missing.length) fail("missing profile keys: " + missing.join(", "));
  if (data.version !== 1) fail("version must be the number 1");
  const accountId = text(data.account_id, "account_id");
  if (!ACCOUNT_ID.test(accountId)) fail("account_id must be 32 hexadecimal characters");
  const workerName = text(data.worker_name, "worker_name");
  if (!WORKER_NAME.test(workerName))
    fail("worker_name must be 1-63 lowercase letters, digits or hyphens");
  const bucketName = text(data.bucket_name, "bucket_name");
  if (
    !BUCKET_NAME.test(bucketName) ||
    IPV4_SHAPED.test(bucketName) ||
    ["xn--", "sthree-", "sthree-configurator"].some((p) => bucketName.startsWith(p)) ||
    ["-s3alias", "--ol-s3"].some((s) => bucketName.endsWith(s))
  )
    fail("bucket_name is not an allowed bucket name");
  const originHost = httpsOrigin(data.origin, "origin");
  if (originHost === "workers.dev" || originHost.endsWith(".workers.dev"))
    fail("origin must be a custom domain, not workers.dev");
  const issuerHost = httpsOrigin(data.issuer, "issuer");
  if (!/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.cloudflareaccess\.com$/.test(issuerHost))
    fail("issuer must be a canonical https <team>.cloudflareaccess.com origin");
  const audience = text(data.audience, "audience");
  const title = text(data.title, "title", 160);
  const profile = {
    version: 1,
    account_id: accountId,
    worker_name: workerName,
    bucket_name: bucketName,
    origin: data.origin,
    title,
    issuer: data.issuer,
    audience,
    image_origins: imageOrigins(data.image_origins),
    agent_links: agentLinks(data.agent_links),
  };
  if ("jira_origin" in data) {
    httpsOrigin(data.jira_origin, "jira_origin");
    profile.jira_origin = data.jira_origin;
  }
  if ("embed_origins" in data) profile.embed_origins = embedOrigins(data.embed_origins);
  if ("tracer_receipt_key" in data)
    profile.tracer_receipt_key = receiptKey(data.tracer_receipt_key);
  return profile;
}

const ascii = (s) =>
  s.replace(/[\u007f-\uffff]/g, (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"));

function sortKeys(v) {
  if (Array.isArray(v)) return v.map(sortKeys);
  if (isObject(v))
    return Object.fromEntries(
      Object.keys(v)
        .sort()
        .map((k) => [k, sortKeys(v[k])]),
    );
  return v;
}

// Python's json.dumps(v, sort_keys=sorted): ", " and ": " separators and ASCII escapes.
export function pyDumps(v, sorted = false) {
  if (sorted) v = sortKeys(v);
  if (Array.isArray(v)) return "[" + v.map((x) => pyDumps(x)).join(", ") + "]";
  if (isObject(v))
    return (
      "{" +
      Object.entries(v)
        .map(([k, x]) => ascii(JSON.stringify(k)) + ": " + pyDumps(x))
        .join(", ") +
      "}"
    );
  return ascii(JSON.stringify(v));
}

// Write document as json.dumps(indent=2, sort_keys=True) through a temp file and a rename.
export function writeAtomic(path, document) {
  const temporary = `${path}.${process.pid}.tmp`;
  writeFileSync(temporary, ascii(JSON.stringify(sortKeys(document), null, 2)) + "\n");
  renameSync(temporary, path);
}

// Atomically render the fixed production Wrangler config and connection descriptor.
export function projectWrangler(profile, sourceRoot, output) {
  const main = join(resolve(sourceRoot), "worker", "index.js");
  if (!existsSync(main) || !statSync(main).isFile())
    fail("worker/index.js not found under " + resolve(sourceRoot));
  const vars = {
    FACTORY_ORIGIN: profile.origin,
    FACTORY_TITLE: profile.title,
    ACCESS_ISSUER: profile.issuer,
    ACCESS_AUDIENCE: profile.audience,
    FACTORY_IMAGE_ORIGINS: pyDumps(profile.image_origins),
    FACTORY_AGENT_LINKS: pyDumps(profile.agent_links, true),
  };
  if ("jira_origin" in profile) vars.FACTORY_JIRA_ORIGIN = profile.jira_origin;
  if ("embed_origins" in profile) vars.FACTORY_EMBED_ORIGINS = pyDumps(profile.embed_origins);
  if ("tracer_receipt_key" in profile) vars.FACTORY_TRACER_RECEIPT_KEY = profile.tracer_receipt_key;
  const config = {
    name: profile.worker_name,
    account_id: profile.account_id,
    main,
    compatibility_date: COMPATIBILITY_DATE,
    workers_dev: false,
    preview_urls: false,
    routes: [{ pattern: profile.origin.split("://")[1], custom_domain: true }],
    r2_buckets: [{ binding: "BUCKET", bucket_name: profile.bucket_name }],
    ai: { binding: "AI" },
    durable_objects: { bindings: [{ name: "BoardEvents", class_name: "BoardEvents" }] },
    migrations: [{ tag: "v1", new_sqlite_classes: ["BoardEvents"] }],
    triggers: { crons: ["17 * * * *"] },
    observability: { ...OBSERVABILITY },
    vars,
  };
  mkdirSync(output, { recursive: true });
  const configPath = join(output, "wrangler.production.json");
  writeAtomic(configPath, config);
  writeAtomic(join(output, "connection.json"), { version: 1, api_url: profile.origin });
  return configPath;
}

export function nodeToolingProblem(sourceRoot, version = process.version) {
  if (!existsSync(join(sourceRoot, "node_modules", ".bin", "wrangler")))
    return "locked Wrangler is not installed; run mise exec -- pnpm install --frozen-lockfile first";
  const engines = JSON.parse(readFileSync(join(sourceRoot, "package.json"), "utf8")).engines.node;
  const minimum = engines.replace(/^>=/, "").split(".").map(Number);
  const match = /^v?(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?$/.exec(version);
  if (match === null)
    return `could not read node --version output "${version}"; run this command through mise exec --`;
  const found = match.slice(1).map(Number);
  const older = found.findIndex((part, i) => part !== minimum[i]);
  if (older >= 0 && found[older] < minimum[older])
    return `node ${version} is older than package.json engines ${engines}; run this command through mise exec --`;
  return null;
}

function wrangler(sourceRoot, args) {
  return spawnSync(join(sourceRoot, "node_modules", ".bin", "wrangler"), args, {
    encoding: "utf8",
  });
}

async function assets() {
  return import("./assets.mjs");
}

// deps lets tests stand in for Wrangler, the asset steps and the Node version, and write to their
// own build folder.
export async function main(argv, deps = {}) {
  const {
    run = wrangler,
    tools = assets,
    version = process.version,
    build = join(ROOT, ".build"),
  } = deps;
  if (argv.length !== 3 || !COMMANDS.includes(argv[0]) || argv[1] !== "--profile") {
    console.error("usage: node tools/deploy.mjs <check|render|dry-run|deploy> --profile <file>");
    return 2;
  }
  const [command, , profilePath] = argv;
  try {
    const profile = loadProfile(profilePath);
    if (command === "check") {
      console.log("profile ok: " + profile.worker_name);
      return 0;
    }
    const configPath = projectWrangler(profile, ROOT, build);
    if (command === "render") {
      console.log(configPath);
      console.log(join(build, "connection.json"));
      return 0;
    }
    const problem = nodeToolingProblem(ROOT, version);
    if (problem) {
      process.stderr.write("preflight: " + problem + "\n");
      return 1;
    }
    const { prepareAssets, inspectBundle } = await tools();
    try {
      await prepareAssets();
    } catch (error) {
      process.stderr.write(String(error?.stack ?? error) + "\n");
      return 1;
    }
    const outdir = join(build, "worker");
    const dryRun = run(ROOT, [
      "deploy",
      "--dry-run",
      "--outdir",
      outdir,
      "--outfile",
      join(outdir, "upload.form"),
      "--config",
      configPath,
    ]);
    if (dryRun.status !== 0) {
      process.stderr.write(dryRun.stderr ?? "");
      return 1;
    }
    let report;
    try {
      report = await inspectBundle(outdir, (dryRun.stdout ?? "") + (dryRun.stderr ?? ""));
    } catch (error) {
      process.stderr.write(String(error?.stack ?? error) + "\n");
      return 1;
    }
    console.log(JSON.stringify(report));
    if (command === "dry-run") return 0;
    const deploy = run(ROOT, ["deploy", "--config", configPath]);
    process.stdout.write(deploy.stdout ?? "");
    if (deploy.status !== 0) {
      process.stderr.write(deploy.stderr ?? "");
      return 1;
    }
    return 0;
  } catch (error) {
    if (!(error instanceof ProfileError)) throw error;
    console.error("invalid profile: " + error.message);
    return 2;
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url))
  process.exitCode = await main(process.argv.slice(2));
