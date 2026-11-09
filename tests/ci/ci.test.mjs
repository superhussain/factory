// Contract guard for the pull-request workflow: parallel jobs, four browser
// shards, and one `ci` aggregate that fails unless every job succeeded.
import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parse } from "yaml";
import { selectShard } from "../browser/shard.mjs";

const workflow = parse(
  readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8"),
);
const fixture = JSON.parse(
  readFileSync(new URL("../fixtures/browser-cases.json", import.meta.url), "utf8"),
);

test("the four shards are disjoint, non-empty and cover every case", () => {
  const shards = [1, 2, 3, 4].map((n) => selectShard(fixture, `${n}/4`));
  for (const shard of shards) {
    assert.ok(shard.suites.length >= 1);
    assert.ok(shard.nodeTests.length >= 1);
  }
  const suites = shards.flatMap((shard) => shard.suites.map((suite) => suite.file));
  const nodeTests = shards.flatMap((shard) => shard.nodeTests);
  assert.deepEqual(suites.sort(), fixture.suites.map((suite) => suite.file).sort());
  assert.deepEqual(nodeTests.sort(), [...fixture.nodeTests].sort());
});

test("an unset or empty shard runs everything", () => {
  assert.equal(selectShard(fixture, undefined), fixture);
  assert.equal(selectShard(fixture, ""), fixture);
});

test("a bad shard value throws naming the variable", () => {
  for (const value of ["0/4", "5/4", "1", "a/b", "1/0"]) {
    assert.throws(() => selectShard(fixture, value), /FACTORY_BROWSER_SHARD/, value);
  }
});

test("the browser job is a four-shard matrix that keeps running on failure", () => {
  const { browser } = workflow.jobs;
  assert.deepEqual(browser.strategy.matrix.shard, [1, 2, 3, 4]);
  assert.equal(browser.strategy["fail-fast"], false);
  assert.equal(browser.env.FACTORY_BROWSER_SHARD, "${{ matrix.shard }}/4");
});

test("the targets job provisions the browser before running Nx", () => {
  const runs = workflow.jobs.targets.steps.map((step) => step.run ?? "");
  const provision = runs.indexOf("node tests/browser/provision.mjs");
  const nx = runs.findIndex((run) => run.includes("nx affected"));
  assert.ok(provision >= 0 && provision < nx);
});

test("the targets job installs a checksum-verified gitleaks and scans for secrets after setup", () => {
  const steps = workflow.jobs.targets.steps;
  const setup = steps.findIndex((step) => step.uses === "./.github/actions/ci-setup");
  const install = steps.findIndex((step) => step.name === "Install gitleaks");
  const scan = steps.findIndex((step) => step.name === "Secret scan");
  assert.ok(setup >= 0 && install === setup + 1 && scan === install + 1);
  assert.match(steps[install].run, /v8\.30\.1\/gitleaks_8\.30\.1_linux_x64\.tar\.gz/);
  assert.match(steps[install].run, /sha256sum -c/);
  assert.equal(steps[scan].run, "python3 tools/secret-scan.py");
});

test("ci aggregates every job and skips only cancelled runs", () => {
  assert.deepEqual(workflow.jobs.ci.needs, ["targets", "python", "browser"]);
  assert.equal(workflow.jobs.ci.if, "${{ !cancelled() }}");
});

test("the aggregate names each job in RESULTS", () => {
  const step = workflow.jobs.ci.steps.find((entry) => entry.name === "All CI jobs passed");
  assert.equal(
    step.env.RESULTS,
    "targets=${{ needs.targets.result }} python=${{ needs.python.result }} browser=${{ needs.browser.result }}",
  );
});

test("the aggregate step fails on any non-success result", () => {
  const step = workflow.jobs.ci.steps.find((entry) => entry.name === "All CI jobs passed");
  const run = (RESULTS) => {
    try {
      const stdout = execFileSync("sh", ["-c", step.run], {
        env: { ...process.env, RESULTS },
        stdio: "pipe",
      });
      return { status: 0, stdout: String(stdout), stderr: "" };
    } catch (error) {
      return { status: error.status, stdout: String(error.stdout), stderr: String(error.stderr) };
    }
  };
  assert.equal(run("targets=success python=success browser=success").status, 0);
  for (const result of ["failure", "cancelled", "skipped"]) {
    const outcome = run(`targets=success python=success browser=${result}`);
    assert.equal(outcome.status, 1, result);
    assert.ok(outcome.stdout.includes(`browser: ${result}`), result);
    assert.ok(outcome.stderr.includes(`CI job browser finished as ${result}`), result);
  }
});

test("targets restores the browser cache read-only", () => {
  const uses = (job) => workflow.jobs[job].steps.map((step) => step.uses);
  assert.ok(uses("targets").includes("actions/cache/restore@v4"));
  assert.ok(uses("browser").includes("actions/cache@v4"));
});

test("the python job keeps .nx per pull request and falls back to main", () => {
  const steps = workflow.jobs.python.steps;
  const cache = steps.find((step) => step.uses === "actions/cache@v4");
  const prefix = "nx-${{ runner.os }}-pr${{ github.event.pull_request.number }}-";
  const suffix = "-${{ github.run_id }}-${{ github.run_attempt }}";
  assert.equal(cache.with.path, ".nx");
  assert.ok(cache.with.key.startsWith(prefix));
  assert.ok(cache.with.key.endsWith(suffix));
  assert.deepEqual(cache.with["restore-keys"].trim().split("\n"), [
    `${cache.with.key.slice(0, -suffix.length)}-`,
    "nx-${{ runner.os }}-main-${{ hashFiles('pnpm-lock.yaml') }}-",
  ]);
  const index = (name) => steps.findIndex((step) => step.name === name);
  assert.ok(steps.indexOf(cache) < index("Adopt the restored Nx database"));
  assert.ok(index("Adopt the restored Nx database") < index("Python suites"));
  const raw = readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");
  assert.doesNotMatch(raw, /secrets\./);
});
