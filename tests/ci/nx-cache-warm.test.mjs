// Contract guard for the nightly workflow that saves .nx under the main key.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { parse } from "yaml";

const read = (name) =>
  readFileSync(new URL(`../../.github/workflows/${name}`, import.meta.url), "utf8");
const raw = read("nx-cache-warm.yml");
const warm = parse(raw);
const ci = parse(read("ci.yml"));
const steps = warm.jobs.python.steps;
const action = (step) => step.uses?.split("@")[0];

test("the warm workflow runs nightly and by hand only", () => {
  assert.deepEqual(warm.on, { schedule: [{ cron: "17 6 * * *" }], workflow_dispatch: null });
});

test("the warm workflow reads the repository and holds no secret", () => {
  assert.deepEqual(warm.permissions, { contents: "read" });
  assert.doesNotMatch(raw, /secrets\.|id-token/);
});

test("one python job on main runs the pull request suites", () => {
  assert.deepEqual(Object.keys(warm.jobs), ["python"]);
  assert.equal(warm.jobs.python.if, "github.ref == 'refs/heads/main'");
  assert.deepEqual(warm.env, ci.env);
  const checkout = steps.find((step) => action(step) === "actions/checkout");
  assert.equal(checkout.with["persist-credentials"], false);
  assert.ok(steps.some((step) => step.uses === "./.github/actions/ci-setup"));
  const suites = (list) => list.find((step) => step.name === "Python suites").run;
  assert.equal(suites(steps), suites(ci.jobs.python.steps));
});

test("the warm workflow saves .nx under the main key the pull request job restores", () => {
  const save = steps.find((step) => action(step) === "actions/cache/save");
  const key =
    "nx-${{ runner.os }}-main-${{ hashFiles('pnpm-lock.yaml') }}-${{ github.run_id }}-${{ github.run_attempt }}";
  assert.equal(save.with.path, ".nx");
  assert.equal(save.with.key, key);
  assert.ok(steps.indexOf(save) > steps.findIndex((step) => step.name === "Python suites"));
  assert.ok(
    !steps.some((step) => ["actions/cache", "actions/cache/restore"].includes(action(step))),
  );
  const prefix = `${key.slice(0, -"-${{ github.run_id }}-${{ github.run_attempt }}".length)}-`;
  const ciCache = ci.jobs.python.steps.find((step) => action(step) === "actions/cache");
  assert.ok(ciCache.with["restore-keys"].trim().split("\n").includes(prefix));
});

test("the warm job and the pull request python job use the same runner image", () => {
  assert.ok(warm.jobs.python["runs-on"], "nx-cache-warm.yml jobs.python is missing runs-on");
  assert.equal(warm.jobs.python["runs-on"], ci.jobs.python["runs-on"]);
});
