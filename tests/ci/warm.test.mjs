// Contract guard for warm.yml: each job must save the exact cache key its ci.yml job restores.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { parse } from "yaml";

const read = (name) =>
  readFileSync(new URL(`../../.github/workflows/${name}`, import.meta.url), "utf8");
const ci = parse(read("ci.yml"));
const raw = read("warm.yml");
const warm = parse(raw);
const action = (step) => step.uses?.split("@")[0];
const rustCache = (job) => job.steps.find((step) => action(step) === "Swatinem/rust-cache");
const named = (job, name) => job.steps.find((step) => step.name === name);
const JOBS = ["rust-build", "targets", "rust-test", "rust-check"];

test("warm runs on main pushes and by hand, reads only contents, and keeps the newest run", () => {
  assert.deepEqual(warm.on, { push: { branches: ["main"] }, workflow_dispatch: null });
  assert.deepEqual(warm.permissions, { contents: "read" });
  assert.equal(warm.concurrency["cancel-in-progress"], true);
  // Per ref, so a manual run elsewhere never cancels main's.
  assert.equal(warm.concurrency.group, "warm-caches-${{ github.ref }}");
  // A manual run on another branch must not seed caches there.
  for (const id of [...JOBS, "browser"]) {
    assert.equal(warm.jobs[id].if, "github.ref == 'refs/heads/main'", id);
  }
  assert.doesNotMatch(raw, /secrets\./);
  assert.deepEqual(Object.keys(warm.jobs), [...JOBS, "browser"]);
});

// rust-cache's key is its prefix-key, the job id, the runner, rustc -vV, every CARGO*, CC*, CFLAGS*, CXX*, CMAKE*
// and RUST* variable, and the manifests. The job ids match by construction; the rest must match here.
test("each warm job's rust-cache key inputs match its ci.yml job", () => {
  for (const id of JOBS) {
    const [w, c] = [warm.jobs[id], ci.jobs[id]];
    assert.equal(w["runs-on"], c["runs-on"], id);
    assert.equal(w.env, undefined, id);
    assert.equal(c.env, undefined, id);
    assert.equal(warm.env, undefined);
    const { "save-if": saveIf, ...inputs } = rustCache(c).with;
    assert.deepEqual(rustCache(w).with, inputs, id);
    assert.equal(rustCache(w).uses, rustCache(c).uses, id);
    for (const step of w.steps) assert.equal(step.env, undefined, `${id}: ${step.name}`);
  }
  // targets never saves in CI, so warm is the only writer of its key.
  assert.equal(rustCache(ci.jobs.targets).with["save-if"], false);
});

test("each warm job installs the same toolchain and compiles what its ci.yml job compiles", () => {
  const toolchain = (job) =>
    job.steps.find((step) => step.run?.startsWith("rustup toolchain install")).run.trim();
  for (const id of JOBS) {
    assert.equal(toolchain(warm.jobs[id]), toolchain(ci.jobs[id]), id);
    // Warm saves the toolchain cache under the key ci.yml's job looks up.
    const cache = named(warm.jobs[id], "Cache the pinned toolchain");
    assert.equal(action(cache), "actions/cache", id);
    assert.deepEqual(cache.with, named(ci.jobs[id], "Cache the pinned toolchain").with, id);
    assert.equal(action(warm.jobs[id].steps[0]), "actions/checkout", id);
    assert.equal(warm.jobs[id].steps[0].with["persist-credentials"], false, id);
  }
  const run = (job, name) => named(job, name).run.trim();
  assert.equal(
    run(warm.jobs["rust-build"], "Build linux-x64"),
    run(ci.jobs["rust-build"], "Build linux-x64"),
  );
  assert.equal(
    run(warm.jobs.targets, "Build the factory binary"),
    run(ci.jobs.targets, "Build the factory binary"),
  );
  assert.equal(
    run(warm.jobs["rust-test"], "Build the tests"),
    run(ci.jobs["rust-test"], "Build the tests"),
  );
  for (const name of ["Check darwin-arm64", "Check windows-x64"]) {
    assert.equal(run(warm.jobs["rust-check"], name), run(ci.jobs["rust-check"], name));
  }
});

test("warm rust-build and rust-test save the incremental artifacts under the keys ci.yml restores", () => {
  // CI's rust-build and rust-test only restore, under the keys warm saves.
  for (const job of ["rust-build", "rust-test"]) {
    const restore = named(ci.jobs[job], "Restore incremental artifacts");
    assert.equal(named(warm.jobs[job], "Restore incremental artifacts").uses, restore.uses, job);
    assert.deepEqual(
      named(warm.jobs[job], "Restore incremental artifacts").with,
      restore.with,
      job,
    );
    const save = named(warm.jobs[job], "Save incremental artifacts");
    assert.equal(action(save), "actions/cache/save", job);
    assert.deepEqual(save.with, { path: restore.with.path, key: restore.with.key }, job);
  }
});

test("warm browser saves the provision under the key ci.yml's browser and targets jobs restore", () => {
  const save = warm.jobs.browser.steps.find((step) => action(step) === "actions/cache");
  const runs = warm.jobs.browser.steps.map((step) => step.run ?? action(step));
  assert.ok(
    runs.indexOf("node tests/browser/provision.mjs") > warm.jobs.browser.steps.indexOf(save),
  );
  assert.equal(warm.jobs.browser.steps[0].with["persist-credentials"], false);
  for (const id of ["browser", "targets"]) {
    const restore = named(ci.jobs[id], "Restore browser provision");
    assert.equal(action(restore), "actions/cache/restore", id);
    assert.equal(restore.with.path, save.with.path, id);
    assert.equal(restore.with.key, save.with.key, id);
  }
});
