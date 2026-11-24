// tools/secret-scan.mjs against a temp git repo with a runtime-generated token.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { randomInt } from "node:crypto";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const ROOT = new URL("../../", import.meta.url).pathname;
const TOOL = join(ROOT, "tools", "secret-scan.mjs");
// Node warns on stderr when FORCE_COLOR and NO_COLOR are both set, as CI sets FORCE_COLOR, so
// children run without FORCE_COLOR; NO_COLOR stays for gitleaks.
const { FORCE_COLOR: _forceColor, ...CHILD_ENV } = process.env;
const GITLEAKS = !spawnSync("gitleaks", ["version"], { stdio: "ignore" }).error;
const which = (name) =>
  spawnSync("sh", ["-c", `command -v ${name}`], { encoding: "utf8" }).stdout.trim();

function setup(t) {
  const tmp = mkdtempSync(join(tmpdir(), "secret-scan-test-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  const repo = join(tmp, "repo");
  mkdirSync(repo);
  copyFileSync(join(ROOT, ".gitleaks.toml"), join(repo, ".gitleaks.toml"));
  writeFileSync(join(repo, "clean.txt"), "nothing here\n");
  const patterns = join(tmp, "patterns.txt");
  writeFileSync(patterns, "synthetic-owner\n(?i)acme-internal\n");
  spawnSync("git", ["init", "-q"], { cwd: repo });
  const commit = () => {
    spawnSync("git", ["add", "-A"], { cwd: repo });
    spawnSync(
      "git",
      ["-c", "user.name=t", "-c", "user.email=t@example.test", "commit", "-qm", "x", "--no-verify"],
      {
        cwd: repo,
      },
    );
  };
  const scan = (args = [], env = CHILD_ENV) =>
    spawnSync(process.execPath, [TOOL, ...args], {
      cwd: repo,
      encoding: "utf8",
      env: { ...env, NO_COLOR: "1" },
    });
  return { tmp, repo, patterns, commit, scan };
}

test("private patterns report the location, not the match", (t) => {
  const { repo, patterns, commit, scan } = setup(t);
  writeFileSync(
    join(repo, "notes.md"),
    "line one\nowned by synthetic-owner\nsee ACME-Internal docs\n",
  );
  writeFileSync(join(repo, "untracked.md"), "synthetic-owner\n");
  commit();
  unlinkSync(join(repo, "untracked.md"));
  writeFileSync(join(repo, "later.md"), "synthetic-owner\n");
  const result = scan(["--private-patterns", patterns]);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, "notes.md:2: private pattern 1\nnotes.md:3: private pattern 2\n");
  assert.ok(!(result.stdout + result.stderr).includes("synthetic-owner"));
  assert.ok(!(result.stdout + result.stderr).includes("ACME"));
});

test("private patterns on a clean tree", (t) => {
  const { patterns, commit, scan } = setup(t);
  commit();
  assert.equal(scan(["--private-patterns", patterns]).status, 0);
});

test("Python-only or invalid patterns exit 2 naming the line, not the pattern", (t) => {
  const { patterns, commit, scan } = setup(t);
  commit();
  for (const [line, message] of [
    [
      "(?P<owner>synthetic-owner)",
      "private pattern 2 uses Python-only regex syntax; write it as a JavaScript regex",
    ],
    [
      "synthetic-owner\\Z",
      "private pattern 2 uses Python-only regex syntax; write it as a JavaScript regex",
    ],
    [
      "synthetic(?i)-owner",
      "private pattern 2 uses Python-only regex syntax; write it as a JavaScript regex",
    ],
    ["synthetic-owner(", "private pattern 2 is not a valid JavaScript regex"],
  ]) {
    writeFileSync(patterns, `acme-internal\n${line}\n`);
    const result = scan(["--private-patterns", patterns]);
    assert.equal(result.status, 2, line);
    assert.equal(result.stderr, `${message}\n`);
    assert.ok(!(result.stdout + result.stderr).includes("synthetic"));
  }
  writeFileSync(patterns, "(?<owner>synthetic-owner)\n(?:acme)\n(?i)ACME\n");
  assert.equal(scan(["--private-patterns", patterns]).status, 0);
});

test(
  "gitleaks finds a planted token and redacts it",
  { skip: !GITLEAKS && "gitleaks not installed" },
  (t) => {
    const { tmp, repo, commit, scan } = setup(t);
    const chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    const token =
      "ghp_" + Array.from({ length: 36 }, () => chars[randomInt(chars.length)]).join("");
    mkdirSync(join(repo, "nested"));
    writeFileSync(join(repo, "nested", "config.txt"), `token = ${token}\n`);
    symlinkSync("nested/config.txt", join(repo, "link.txt"));
    commit();
    let result = scan();
    assert.equal(result.status, 1);
    assert.ok(!(result.stdout + result.stderr).includes(token));
    const history = join(tmp, "history.json");
    result = scan(["--history", history]);
    assert.equal(result.status, 0);
    assert.equal(result.stdout, "history: 1 findings\n");
    assert.ok(!readFileSync(history, "utf8").includes(token));
  },
);

test("gitleaks on a clean tree", { skip: !GITLEAKS && "gitleaks not installed" }, (t) => {
  const { commit, scan } = setup(t);
  commit();
  assert.equal(scan().status, 0);
});

test("gitleaks missing exits 2", (t) => {
  const { tmp, commit, scan } = setup(t);
  commit();
  const bin = join(tmp, "bin");
  mkdirSync(bin);
  symlinkSync(which("git"), join(bin, "git"));
  const result = scan([], { ...CHILD_ENV, PATH: bin });
  assert.equal(result.status, 2);
  assert.match(result.stderr, /gitleaks not found/);
});
