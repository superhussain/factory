// Contract guard for the pull-request workflow: parallel jobs, eight browser
// shards, and one `ci` aggregate that fails unless every job succeeded.
import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { parse } from "yaml";
import { selectShard } from "../browser/shard.mjs";

const workflow = parse(
  readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8"),
);
const action = (step) => step.uses?.split("@")[0];
const fixture = JSON.parse(
  readFileSync(new URL("../fixtures/browser-cases.json", import.meta.url), "utf8"),
);

test("the eight shards are disjoint, non-empty and cover every case", () => {
  const shards = [1, 2, 3, 4, 5, 6, 7, 8].map((n) => selectShard(fixture, `${n}/8`));
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

test("the browser job is an eight-shard matrix that keeps running on failure", () => {
  const { browser } = workflow.jobs;
  assert.deepEqual(browser.strategy.matrix.shard, [1, 2, 3, 4, 5, 6, 7, 8]);
  assert.equal(browser.strategy["fail-fast"], false);
  assert.equal(browser.env.FACTORY_BROWSER_SHARD, "${{ matrix.shard }}/8");
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
  assert.equal(steps[scan].run, "node tools/secret-scan.mjs");
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
      env: { ...process.env, ALL_HIT: "", RUST_HIT: "", BROWSER_RUN: "", ...env },
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

test("the aggregate accepts a skipped browser job only when no browser input changed", () => {
  const run = (RESULTS, env) => runAggregate("ci", "All CI jobs passed", { RESULTS, ...env });
  const skipped = run("plan=success targets=success browser=skipped", { BROWSER_RUN: "false" });
  assert.equal(skipped.status, 0);
  assert.ok(skipped.stdout.includes("browser: no browser input changed"));
  assert.equal(run("plan=success browser=skipped", { BROWSER_RUN: "true" }).status, 1);
  assert.equal(run("plan=success browser=skipped", { BROWSER_RUN: "" }).status, 1);
  assert.equal(run("plan=success targets=skipped", { BROWSER_RUN: "false" }).status, 1);
  assert.equal(run("plan=success browser=failure", { BROWSER_RUN: "false" }).status, 1);
});

test("the plan's browser inputs cover the Nx browser target's inputs", () => {
  const nx = JSON.parse(readFileSync(new URL("../../nx.json", import.meta.url), "utf8"));
  const tooling = JSON.parse(
    readFileSync(new URL("../../tools/project.json", import.meta.url), "utf8"),
  );
  assert.ok(tooling.targets.browser);
  const step = workflow.jobs.plan.steps.find((entry) => entry.name === "Browser inputs");
  const paths = step.run.split(" HEAD -- ")[1].split(";")[0].split(" ");
  // "default" is the tooling project's own folder; "shared" is nx.json's named input.
  const wanted = [
    "tools",
    ...nx.namedInputs.shared.map((input) => input.replace("{workspaceRoot}/", "")),
  ];
  for (const input of nx.targetDefaults.browser.inputs) {
    if (typeof input !== "string" || ["default", "shared"].includes(input)) continue;
    const path = input.replace("{workspaceRoot}/", "").replace(/\/\*\*(\/\*)?$/, "");
    wanted.push(path.startsWith("!") ? `':!${path.slice(1)}'` : path);
  }
  wanted.push("nx.json", "paseo-plugin", ".github/workflows/ci.yml", ".github/actions");
  assert.deepEqual([...new Set(paths)].sort(), [...new Set(wanted)].sort());
  assert.deepEqual(workflow.jobs.plan.outputs.browser, "${{ steps.browser.outputs.run }}");
  assert.equal(
    workflow.jobs.browser.if,
    "needs.plan.outputs.all != 'true' && needs.plan.outputs.browser == 'true'",
  );
});

test("the browser inputs step skips a Rust-only change and runs a web change", () => {
  const step = workflow.jobs.plan.steps.find((entry) => entry.name === "Browser inputs");
  const repo = mkdtempSync(join(tmpdir(), "ci-browser-inputs-"));
  const git = (...args) => execFileSync("git", ["-C", repo, ...args], { stdio: "pipe" });
  git("init", "-q");
  git(
    "-c",
    "user.email=ci@example.test",
    "-c",
    "user.name=ci",
    "commit",
    "-q",
    "--allow-empty",
    "-m",
    "base",
  );
  git("update-ref", "refs/remotes/origin/main", "HEAD");
  const touched = (path) => {
    mkdirSync(dirname(join(repo, path)), { recursive: true });
    writeFileSync(join(repo, path), `${path}\n`);
    git("add", path);
    git("-c", "user.email=ci@example.test", "-c", "user.name=ci", "commit", "-q", "-m", path);
    const output = join(repo, ".output");
    writeFileSync(output, "");
    execFileSync("sh", ["-c", step.run], {
      cwd: repo,
      env: { ...process.env, GITHUB_BASE_REF: "main", GITHUB_OUTPUT: output },
    });
    return readFileSync(output, "utf8");
  };
  try {
    assert.equal(touched("rust/factory/src/main.rs"), "run=false\n");
    assert.equal(touched("tools/deploy.mjs"), "run=false\n");
    assert.equal(touched("tests/fixtures/hosts-list.json"), "run=true\n");
    assert.equal(touched("web/app.js"), "run=true\n");
  } finally {
    rmSync(repo, { recursive: true, force: true });
  }
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
  for (const job of ["targets", "paseo-plugin"]) {
    assert.equal(workflow.jobs[job].if, "needs.plan.outputs.all != 'true'", job);
  }
});

test("targets and browser restore the browser cache read-only", () => {
  for (const job of ["targets", "browser"]) {
    const uses = workflow.jobs[job].steps.map(action);
    assert.ok(uses.includes("actions/cache/restore"), job);
    assert.ok(!uses.includes("actions/cache") && !uses.includes("actions/cache/save"), job);
  }
});

test("rust-build caches the binary under the rust key, and targets restores it or builds it on a miss", () => {
  const { targets, "rust-build": build } = workflow.jobs;
  const key = "${{ needs.plan.outputs.rust-key }}-bin";
  const binary = "rust/target/host/factory";
  const save = build.steps.find((step) => step.name === "Cache the factory binary for targets");
  assert.equal(action(save), "actions/cache/save");
  assert.deepEqual(save.with, { path: binary, key });
  const buildStep = build.steps.findIndex((step) => step.name === "Build linux-x64");
  assert.ok(build.steps.indexOf(save) > buildStep);
  // targets never waits on rust-build, so it still runs when a rust hit skips rust-build.
  assert.equal(targets.needs, "plan");
  const restore = targets.steps.findIndex((step) => step.name === "Restore the factory binary");
  assert.equal(action(targets.steps[restore]), "actions/cache/restore");
  assert.deepEqual(targets.steps[restore].with, { path: binary, key });
  const miss = "steps.factory-bin.outputs.cache-hit != 'true'";
  const onMiss = targets.steps.filter((step) => step.if === miss);
  assert.deepEqual(
    onMiss.map((step) => step.run ?? action(step)),
    [
      "rustup toolchain install",
      "Swatinem/rust-cache",
      "actions/cache/restore",
      "CARGO_INCREMENTAL=1 cargo build --profile host --locked",
    ],
  );
  // warm.yml saves the shared rust/target cache and the incremental artifacts; targets only reads them.
  assert.equal(onMiss[1].with["save-if"], false);
  const incremental = build.steps.find((step) => step.name === "Restore incremental artifacts");
  assert.deepEqual(onMiss[2].with, incremental.with);
  assert.ok(!targets.steps.some((step) => action(step) === "actions/cache/save"));
  const nx = targets.steps.findIndex((step) => step.name === "Nx affected");
  assert.ok(onMiss.every((step) => targets.steps.indexOf(step) > restore));
  assert.ok(onMiss.every((step) => targets.steps.indexOf(step) < nx));
});

test("rust-build restores main's incremental artifacts before an incremental build and never saves them", () => {
  const steps = workflow.jobs["rust-build"].steps;
  const at = (name) => steps.findIndex((step) => step.name === name);
  const cache = steps.findIndex((step) => action(step) === "Swatinem/rust-cache");
  const restore = steps[at("Restore incremental artifacts")];
  assert.equal(action(restore), "actions/cache/restore");
  // After rust-cache, whose restore would otherwise land on top.
  assert.ok(cache < at("Restore incremental artifacts"));
  assert.ok(at("Restore incremental artifacts") < at("Build linux-x64"));
  assert.equal(
    steps[at("Build linux-x64")].run,
    "CARGO_INCREMENTAL=1 cargo build --profile host --locked",
  );
  // A PR run's save would add about 50 MB per run to the repo's cache budget; the binary is the only save.
  assert.deepEqual(
    steps.filter((step) => action(step) === "actions/cache/save").map((step) => step.with.path),
    ["rust/target/host/factory"],
  );
  const path = "rust/target/host/incremental";
  const prefix = "v1-rust-incr-${{ hashFiles('rust/rust-toolchain.toml', 'rust/Cargo.lock') }}-";
  const key = `${prefix}\${{ github.run_id }}-\${{ github.run_attempt }}`;
  // restore takes the newest with the toolchain's prefix.
  assert.deepEqual(restore.with, { path, key, "restore-keys": prefix });
});

test("the nextest config neither retries nor terminates tests", () => {
  const config = readFileSync(new URL("../../rust/.config/nextest.toml", import.meta.url), "utf8");
  // A retry would turn a flaky failure green, and terminate-after would cut short the timeout tests.
  const settings = config.split("\n").filter((line) => !line.trimStart().startsWith("#"));
  for (const line of settings) {
    assert.doesNotMatch(line, /\bretries\b|terminate-after/, line);
  }
});

test("rust-test restores main's incremental artifacts before the build and never saves them", () => {
  const steps = workflow.jobs["rust-test"].steps;
  const at = (name) => steps.findIndex((step) => step.name === name);
  const cache = steps.findIndex((step) => action(step) === "Swatinem/rust-cache");
  const restore = steps[at("Restore incremental artifacts")];
  assert.equal(action(restore), "actions/cache/restore");
  assert.ok(cache < at("Restore incremental artifacts"));
  assert.ok(at("Restore incremental artifacts") < at("Build the tests"));
  assert.ok(at("Build the tests") < at("Test linux-x64"));
  assert.ok(!steps.some((step) => action(step) === "actions/cache/save"));
  const prefix =
    "v1-rust-test-incr-${{ hashFiles('rust/rust-toolchain.toml', 'rust/Cargo.lock') }}-";
  assert.deepEqual(restore.with, {
    path: "rust/target/debug/incremental",
    key: `${prefix}\${{ github.run_id }}-\${{ github.run_attempt }}`,
    "restore-keys": prefix,
  });
});

test("no job runs the Python CLI suites or sets up Python, and the workflow reads no secret", () => {
  assert.equal(workflow.jobs.python, undefined);
  const raw = readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");
  assert.doesNotMatch(raw, /secrets\.|setup-python|PYTHON/);
  const setup = readFileSync(
    new URL("../../.github/actions/ci-setup/action.yml", import.meta.url),
    "utf8",
  );
  assert.doesNotMatch(setup, /setup-python|python-version/);
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
    "CARGO_INCREMENTAL=1 cargo build --profile host --locked",
    [
      'cargo_home="${CARGO_HOME:-$HOME/.cargo}"',
      'rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"',
      'root="$(mktemp -d)"',
      'out="$(CARGO_INCREMENTAL=1 HOME="$root/home" CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" sh ../tools/install.sh "$root/skills" 2>&1)"',
      "printf '%s\\n' \"$out\"",
      'printf \'%s\\n\' "$out" | grep -qx "install: Rust binary from $PWD/target/host/factory"',
      'test -x "$root/skills/factory/runtime/factory"',
      'grep -q \'"entry": \\["runtime/factory"\\]\' "$root/skills/factory/bundle.json"',
    ].join("\n"),
  ]);
  const unitRuns = runs(unit);
  assert.equal(unitRuns[0], "rustup toolchain install");
  assert.match(unitRuns[1], /cargo-nextest-0\.9\.152-x86_64-unknown-linux-gnu\.tar\.gz/);
  assert.match(unitRuns[1], /sha256sum -c/);
  assert.equal(unitRuns[2], "CARGO_INCREMENTAL=1 cargo test --locked --no-run");
  // Every shard runs its hash partition of every test but the heavy ones, and its share of those beside it.
  assert.equal(
    unitRuns[3],
    [
      'CARGO_INCREMENTAL=1 cargo nextest run --locked --no-fail-fast -E "$HEAVY" &',
      "heavy=$!",
      "status=0",
      'CARGO_INCREMENTAL=1 cargo nextest run --locked --no-fail-fast --partition hash:${{ matrix.shard }}/2 -E "not ($ALL_HEAVY)" || status=1',
      'wait "$heavy" || status=1',
      'exit "$status"',
    ].join("\n"),
  );
  assert.equal(unitRuns.length, 4);
  assert.equal(unit.name, "rust-test (${{ matrix.shard }})");
  const shards = unit.strategy.matrix.include;
  assert.deepEqual(
    shards.map((entry) => entry.shard),
    [1, 2],
  );
  // The shards' heavy tests together are exactly ALL_HEAVY, which is exactly nextest.toml's priority-100 list.
  const tests = (expr) => expr.split("|").map((part) => part.trim());
  const step = unit.steps.find((entry) => entry.name === "Test linux-x64");
  assert.equal(step.env.HEAVY, "${{ matrix.heavy }}");
  const all = tests(step.env.ALL_HEAVY);
  assert.deepEqual(shards.flatMap((entry) => tests(entry.heavy)).sort(), [...all].sort());
  assert.ok(shards.every((entry) => tests(entry.heavy).length > 0));
  const config = readFileSync(new URL("../../rust/.config/nextest.toml", import.meta.url), "utf8");
  const first = /filter = '''([^]*?)'''\npriority = 100/.exec(config)[1];
  assert.deepEqual(tests(first).sort(), [...all].sort());
  assert.equal(unit.strategy["fail-fast"], false);
  assert.deepEqual(runs(check), [
    "rustup toolchain install\nrustup target add aarch64-apple-darwin x86_64-pc-windows-gnu",
    "cargo fmt --check",
    "cargo check --locked --target aarch64-apple-darwin",
    "cargo check --locked --tests --target x86_64-pc-windows-gnu",
  ]);
  assert.equal(parity.needs, "rust-build");
  assert.deepEqual(parity.strategy.matrix.shard, [1, 2, 3, 4, 5, 6]);
  assert.equal(parity.strategy["fail-fast"], false);
  assert.match(
    runs(parity)[0],
    /node tests\/parity\/run\.mjs --bin rust\/target\/host\/factory --ids rust\/parity --shard \$\{\{ matrix\.shard \}\}\/6$/,
  );
  // The harness self-tests run once, on shard 1, against the same binary.
  const selfTest = parity.steps.find((step) => step.name === "Harness self-tests");
  assert.equal(selfTest.if, "matrix.shard == 1");
  assert.deepEqual(selfTest.env, { FACTORY_BIN: "rust/target/host/factory" });
  assert.equal(selfTest.run, "node --test tests/parity/run.test.mjs");
  assert.deepEqual(runs(parity).slice(1), ["node --test tests/parity/run.test.mjs"]);
});
