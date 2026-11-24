#!/usr/bin/env node
// Compose the repository's verification commands into device, hosted and all groups.
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const ROOT = fileURLToPath(new URL("..", import.meta.url));
const GROUPS = ["device", "hosted", "all"];
export const HOSTED_SCRIPTS = [
  "test:assets",
  "test:auth",
  "test:protocol",
  "test:local",
  "test:browser",
  "test:deploy",
];

function pnpmOnPath() {
  return !spawnSync("pnpm", ["--version"], { stdio: "ignore" }).error;
}

// [[id, argv]] for a group, or null when pnpm is required but missing.
function plan(group, hasPnpm) {
  const device = [
    ["rust-test", ["cargo", "test", "--locked", "--manifest-path", "rust/Cargo.toml"]],
  ];
  if (group === "device") return device;
  if (!hasPnpm()) return null;
  const hosted = HOSTED_SCRIPTS.map((script) => [script, ["pnpm", "run", script]]);
  return group === "hosted" ? hosted : [...device, ...hosted];
}

function parse(argv) {
  let group = null;
  let json = false;
  for (const arg of argv) {
    if (arg === "--json") json = true;
    else if (GROUPS.includes(arg) && group === null) group = arg;
    else return null;
  }
  return group === null ? null : { group, json };
}

// JSON with Python json.dumps separators, as the summary line has always printed.
function pyJson(v) {
  if (Array.isArray(v)) return "[" + v.map(pyJson).join(", ") + "]";
  if (v && typeof v === "object")
    return (
      "{" +
      Object.entries(v)
        .map(([k, x]) => JSON.stringify(k) + ": " + pyJson(x))
        .join(", ") +
      "}"
    );
  return JSON.stringify(v);
}

function spawn(argv, options) {
  return spawnSync(argv[0], argv.slice(1), { ...options, stdio: "inherit" }).status ?? 1;
}

export function main(argv, { run = spawn, hasPnpm = pnpmOnPath } = {}) {
  const parsed = parse(argv);
  if (parsed === null) {
    console.error("usage: verify.mjs [--json] {device|hosted|all}");
    return 2;
  }
  const commands = plan(parsed.group, hasPnpm);
  if (commands === null) {
    console.error("pnpm is required for the hosted checks but was not found on PATH");
    return 2;
  }
  const env = { ...process.env, WRANGLER_SEND_METRICS: "false" };
  const results = [];
  let status = "pass";
  for (const [id, command] of commands) {
    const code = run(command, { cwd: ROOT, env });
    results.push({ id, exit_code: code });
    if (code !== 0) {
      console.error(`${id} failed with exit code ${code}`);
      status = "fail";
      break;
    }
  }
  if (parsed.json) console.log(pyJson({ version: 1, group: parsed.group, status, results }));
  return status === "pass" ? 0 : 1;
}

if (process.argv[1] === fileURLToPath(import.meta.url))
  process.exitCode = main(process.argv.slice(2));
