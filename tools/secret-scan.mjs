#!/usr/bin/env node
// Scan the tracked tree for secrets with gitleaks, or for private identifiers.
//
// Usage: secret-scan.mjs [--private-patterns FILE] [--history OUT]
// Default: copies `git ls-files` to a temp dir and runs `gitleaks dir` on it with
// <repo>/.gitleaks.toml. Exit 0 clean, 1 findings, 2 gitleaks missing.
// --private-patterns FILE: one JavaScript regex per line (the u flag is on); prints
// `<file>:<line>: private pattern <n>` per hit, never the match. A leading (?i) makes a
// line case-insensitive. Python-only syntax such as (?P<name>...) or \Z, or a line that is
// not a valid regex, exits 2 naming the line. Keep FILE outside the tree.
// --history OUT: runs `gitleaks git` over all history into the JSON report OUT
// and prints `history: <n> findings`.
import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";

const LINES = /\r\n|[\n\r\v\f\x1c-\x1e\x85\u2028\u2029]/;

function lines(text) {
  const parts = text.split(LINES);
  if (parts.at(-1) === "") parts.pop();
  return parts;
}

function tracked(repo) {
  const out = spawnSync("git", ["ls-files", "-z"], { cwd: repo, encoding: "utf8" });
  if (out.status !== 0) throw new Error("git ls-files failed");
  return out.stdout.split("\0").filter((name) => {
    try {
      return name && statSync(join(repo, name)).isFile();
    } catch {
      return false;
    }
  });
}

// Python re syntax that JavaScript reads differently or not at all: named groups and
// backreferences, comments, \A and \Z, and inline flags other than a leading (?i).
const PYTHON_ONLY = /\(\?P[<=>]|\(\?#|\\[AZ]|\(\?[aiLmsux]+[):]/;

function privateScan(repo, patternsFile) {
  const patterns = [];
  let bad = null;
  lines(readFileSync(patternsFile, "utf8")).forEach((line, i) => {
    if (!line.trim() || bad) return;
    const insensitive = line.startsWith("(?i)");
    const source = insensitive ? line.slice(4) : line;
    // The message names the line, never the pattern, which may itself be private.
    if (PYTHON_ONLY.test(source)) {
      bad = `private pattern ${i + 1} uses Python-only regex syntax; write it as a JavaScript regex`;
      return;
    }
    try {
      patterns.push([i + 1, new RegExp(source, insensitive ? "iu" : "u")]);
    } catch {
      bad = `private pattern ${i + 1} is not a valid JavaScript regex`;
    }
  });
  if (bad) {
    console.error(bad);
    return 2;
  }
  let hits = 0;
  for (const name of tracked(repo)) {
    lines(readFileSync(join(repo, name)).toString("utf8")).forEach((line, i) => {
      for (const [n, pattern] of patterns) {
        if (pattern.test(line)) {
          console.log(`${name}:${i + 1}: private pattern ${n}`);
          hits += 1;
        }
      }
    });
  }
  return hits ? 1 : 0;
}

function hasGitleaks() {
  return !spawnSync("gitleaks", ["version"], { stdio: "ignore" }).error;
}

function main(argv) {
  const { values } = parseArgs({
    args: argv,
    options: { "private-patterns": { type: "string" }, history: { type: "string" } },
  });
  const top = spawnSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" });
  if (top.status !== 0) throw new Error("not in a git checkout");
  const repo = top.stdout.trim();
  if (values["private-patterns"]) return privateScan(repo, values["private-patterns"]);
  if (!hasGitleaks()) {
    console.error("gitleaks not found; run 'mise install'");
    return 2;
  }
  if (values.history) {
    const run = spawnSync(
      "gitleaks",
      [
        "git",
        "--redact",
        "--no-banner",
        "--report-format",
        "json",
        "--report-path",
        values.history,
        "--exit-code",
        "0",
      ],
      { cwd: repo, stdio: "inherit" },
    );
    if (run.status !== 0) throw new Error(`gitleaks git exited ${run.status}`);
    console.log(`history: ${JSON.parse(readFileSync(values.history, "utf8")).length} findings`);
    return 0;
  }
  const tmp = mkdtempSync(join(tmpdir(), "secret-scan-"));
  try {
    for (const name of tracked(repo))
      cpSync(join(repo, name), join(tmp, name), {
        verbatimSymlinks: true,
        preserveTimestamps: true,
      });
    const run = spawnSync(
      "gitleaks",
      [
        "dir",
        ".",
        "--config",
        join(repo, ".gitleaks.toml"),
        "--redact",
        "--no-banner",
        "--exit-code",
        "1",
      ],
      { cwd: tmp, stdio: "inherit" },
    );
    return run.status ?? 1;
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

process.exitCode = main(process.argv.slice(2));
