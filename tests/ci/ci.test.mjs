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
const action = (step) => step.uses?.split("@")[0];
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
  assert.deepEqual(workflow.jobs.ci.needs, [
    "plan",
    "targets",
    "browser",
    "paseo-plugin",
    "rust-build",
    "rust-test",
    "rust-check",
    "parity",
  ]);
  assert.equal(workflow.jobs.ci.if, "${{ !cancelled() }}");
});

test("the aggregate names each job in RESULTS", () => {
  const step = workflow.jobs.ci.steps.find((entry) => entry.name === "All CI jobs passed");
  assert.equal(
    step.env.RESULTS,
    "plan=${{ needs.plan.result }} targets=${{ needs.targets.result }} browser=${{ needs.browser.result }} paseo-plugin=${{ needs.paseo-plugin.result }} rust-build=${{ needs.rust-build.result }} rust-test=${{ needs.rust-test.result }} rust-check=${{ needs.rust-check.result }} parity=${{ needs.parity.result }}",
  );
});

const runAggregate = (job, name, env) => {
  const step = workflow.jobs[job].steps.find((entry) => entry.name === name);
  try {
    const stdout = execFileSync("sh", ["-c", step.run], {
      env: { ...process.env, ALL_HIT: "", RUST_HIT: "", ...env },
      stdio: "pipe",
    });
    return { status: 0, stdout: String(stdout), stderr: "" };
  } catch (error) {
    return { status: error.status, stdout: String(error.stdout), stderr: String(error.stderr) };
  }
};

test("the aggregate step fails on any non-success result", () => {
  const run = (RESULTS, env = {}) => runAggregate("ci", "All CI jobs passed", { RESULTS, ...env });
  assert.equal(run("plan=success targets=success browser=success").status, 0);
  for (const result of ["failure", "cancelled", "skipped"]) {
    const outcome = run(`targets=success browser=${result}`);
    assert.equal(outcome.status, 1, result);
    assert.ok(outcome.stdout.includes(`browser: ${result}`), result);
    assert.ok(outcome.stderr.includes(`CI job browser finished as ${result}`), result);
  }
});

test("the aggregate accepts a skip only where the plan found a passing run", () => {
  const run = (RESULTS, env) => runAggregate("ci", "All CI jobs passed", { RESULTS, ...env });
  const rust = "rust-build=skipped rust-test=skipped rust-check=skipped parity=skipped";
  assert.equal(run(`plan=success targets=skipped ${rust}`, { ALL_HIT: "true" }).status, 0);
  assert.equal(run(`plan=success targets=success ${rust}`, { RUST_HIT: "true" }).status, 0);
  assert.equal(run(`plan=success targets=skipped ${rust}`, { RUST_HIT: "true" }).status, 1);
  assert.equal(run("plan=skipped targets=success", { ALL_HIT: "true" }).status, 1);
  assert.equal(run("plan=success rust-test=failure", { RUST_HIT: "true" }).status, 1);
});

test("the plan keys the whole merge tree and the rust inputs, and lookups never download", () => {
  const { plan, ci } = workflow.jobs;
  const keys = plan.steps.find((step) => step.name === "Input keys").run;
  assert.match(keys, /git rev-parse 'HEAD\^\{tree\}'/);
  assert.match(keys, /git merge-base "origin\/\$GITHUB_BASE_REF" HEAD/);
  assert.equal(plan.steps[0].with["fetch-depth"], 0);
  const sameRepo = "github.event.pull_request.head.repo.full_name == github.repository";
  for (const lookup of plan.steps.filter((step) => action(step) === "actions/cache/restore")) {
    assert.ok(lookup.if.includes(sameRepo), lookup.name);
  }
  for (const save of ci.steps.filter((step) => action(step) === "actions/cache/save")) {
    assert.ok(save.if.includes(sameRepo), save.name);
  }
  for (const path of [
    "rust",
    "tests/parity",
    "skills",
    "tools",
    "worker/admission.js",
    "tests/fixtures",
    ".github/workflows/ci.yml",
  ]) {
    assert.ok(keys.includes(`HEAD:${path} `) || keys.includes(`HEAD:${path} |`), path);
  }
  const lookups = plan.steps.filter((step) => action(step) === "actions/cache/restore");
  assert.equal(lookups.length, 2);
  for (const lookup of lookups) assert.equal(lookup.with["lookup-only"], true);
  const saves = ci.steps.filter((step) => action(step) === "actions/cache/save");
  assert.deepEqual(
    saves.map((step) => step.with.key),
    ["${{ needs.plan.outputs.rust-key }}", "${{ needs.plan.outputs.all-key }}"],
  );
  for (const job of ["rust-build", "rust-test", "rust-check", "parity"]) {
    assert.ok(saves[0].if.includes(`needs.${job}.result == 'success'`), job);
  }
  for (const job of ["targets", "browser", "paseo-plugin"]) {
    assert.equal(workflow.jobs[job].if, "needs.plan.outputs.all != 'true'", job);
  }
});

test("targets restores the browser cache read-only", () => {
  const uses = (job) => workflow.jobs[job].steps.map(action);
  assert.ok(uses("targets").includes("actions/cache/restore"));
  assert.ok(uses("browser").includes("actions/cache"));
});

test("no job runs the Python CLI suites and the workflow reads no secret", () => {
  assert.equal(workflow.jobs.python, undefined);
  const raw = readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");
  assert.doesNotMatch(raw, /secrets\./);
});

test("the paseo-plugin job installs, typechecks and tests the plugin in order", () => {
  const steps = workflow.jobs["paseo-plugin"].steps.map((step) => step.run ?? action(step));
  assert.deepEqual(steps, [
    "actions/checkout",
    "./.github/actions/ci-setup",
    "pnpm --dir paseo-plugin install --frozen-lockfile",
    "pnpm --dir paseo-plugin run typecheck",
    "pnpm run test:paseo-plugin",
  ]);
});

test("the rust jobs build, test and check in parallel and shard parity over the built binary", () => {
  const { "rust-build": build, "rust-test": unit, "rust-check": check, parity } = workflow.jobs;
  const runs = (job) => job.steps.filter((step) => step.run).map((step) => step.run.trim());
  for (const job of [build, unit, check, parity]) {
    assert.equal(job["runs-on"], "ubuntu-22.04");
    assert.ok(job["timeout-minutes"] <= 10);
  }
  for (const job of [build, unit, check]) {
    assert.deepEqual(job.needs, "plan");
    assert.equal(job.defaults.run["working-directory"], "rust");
    assert.equal(action(job.steps[0]), "actions/checkout");
    assert.equal(job.steps[0].with["persist-credentials"], false);
    assert.ok(job.steps.some((step) => action(step) === "Swatinem/rust-cache"));
  }
  assert.deepEqual(runs(build), [
    "rustup toolchain install",
    "cargo build --profile host --locked",
    [
      'cargo_home="${CARGO_HOME:-$HOME/.cargo}"',
      'rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"',
      'root="$(mktemp -d)"',
      'HOME="$root/home" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" sh ../tools/install.sh "$root/skills"',
      'test -x "$root/skills/factory/runtime/factory"',
      'grep -q \'"entry": \\["runtime/factory"\\]\' "$root/skills/factory/bundle.json"',
    ].join("\n"),
  ]);
  assert.deepEqual(runs(unit), ["rustup toolchain install", "cargo test --locked"]);
  assert.deepEqual(runs(check), [
    "rustup toolchain install\nrustup target add aarch64-apple-darwin x86_64-pc-windows-gnu",
    "cargo fmt --check",
    "cargo check --locked --target aarch64-apple-darwin",
    "cargo check --locked --target x86_64-pc-windows-gnu",
  ]);
  assert.equal(parity.needs, "rust-build");
  assert.deepEqual(parity.strategy.matrix.shard, [1, 2, 3, 4, 5, 6]);
  assert.equal(parity.strategy["fail-fast"], false);
  assert.match(
    runs(parity)[0],
    /python3 tests\/parity\/run\.py --bin rust\/target\/host\/factory --ids rust\/parity --shard \$\{\{ matrix\.shard \}\}\/6$/,
  );
});
