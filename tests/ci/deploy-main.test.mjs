// Contract guard for the main-push deploy workflow: least permissions,
// head reconciliation (every run deploys origin/main's current head, so a
// displaced or stale run cannot lose it), cached install with an uncached
// deploy, and fail-fast diagnostics for unprovisioned inputs
// (docs/cloudflare.md 8).
import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { parse } from "yaml";

const workflowPath = new URL("../../.github/workflows/deploy-main.yml", import.meta.url);
const workflow = parse(readFileSync(workflowPath, "utf8"));
const workflowText = readFileSync(workflowPath, "utf8");
const job = workflow.jobs.deploy;
const step = (name) => job.steps.find((entry) => entry.name === name);

const reconcile = step("Reconcile with origin/main");
const reconcileIndex = job.steps.indexOf(reconcile);
const checkoutIndex = job.steps.findIndex((entry) => entry.uses === "actions/checkout@v4");

// Runs the workflow's reconcile script body against a stub `git` whose
// ls-remote prints `fakeHead`, returning the exit status and the outputs the
// step wrote to its GITHUB_OUTPUT file.
function runReconcile({ runSha, fakeHead }) {
  const dir = mkdtempSync(join(tmpdir(), "reconcile-"));
  writeFileSync(
    join(dir, "git"),
    '#!/bin/sh\nif [ "$1" = "ls-remote" ]; then\n  if [ -n "$FAKE_HEAD" ]; then printf "%s\\trefs/heads/main\\n" "$FAKE_HEAD"; fi\n  exit 0\nfi\necho "unexpected git call: $*" >&2\nexit 1\n',
    { mode: 0o755 },
  );
  const outputFile = join(dir, "github-output");
  writeFileSync(outputFile, ""); // the runner provides the GITHUB_OUTPUT file
  try {
    execFileSync("sh", ["-c", reconcile.run], {
      env: {
        PATH: `${dir}:${process.env.PATH}`,
        GH_TOKEN: "test-token",
        GITHUB_SERVER_URL: "https://github.example",
        GITHUB_REPOSITORY: "owner/repo",
        GITHUB_SHA: runSha,
        GITHUB_OUTPUT: outputFile,
        FAKE_HEAD: fakeHead,
      },
      encoding: "utf8",
      stdio: "pipe",
    });
    return { status: 0, outputs: readFileSync(outputFile, "utf8") };
  } catch (error) {
    return { status: error.status, outputs: readFileSync(outputFile, "utf8") };
  }
}

test("main push is the only trigger", () => {
  assert.deepEqual(workflow.on, { push: { branches: ["main"] } });
});

test("checkout pins the reconciled head without persisting credentials", () => {
  const checkout = job.steps[checkoutIndex];
  assert.equal(checkout.with.ref, "${{ steps.reconcile.outputs.sha }}");
  assert.equal(checkout.with["persist-credentials"], false);
});

test("permissions are the read-only ceiling with no id-token", () => {
  assert.deepEqual(workflow.permissions, { contents: "read" });
  assert.deepEqual(job.permissions, undefined);
  assert.doesNotMatch(workflowText, /id-token/);
});

test("concurrency coalesces main pushes without cancelling a deploy", () => {
  assert.equal(workflow.concurrency.group, "deploy-main");
  assert.equal(workflow.concurrency["cancel-in-progress"], false);
});

test("install is cached, the deploy step is uncached and exact", () => {
  const setupNode = job.steps.find((entry) => entry.uses === "actions/setup-node@v4");
  assert.equal(setupNode.with.cache, "pnpm");
  const deploy = step("Deploy");
  assert.equal(deploy.run, "python3 tools/deploy.py deploy --profile deploy/profile.json");
  assert.equal(deploy.env.CLOUDFLARE_API_TOKEN, "${{ secrets.CLOUDFLARE_API_TOKEN }}");
  assert.doesNotMatch(workflowText, /actions\/cache|actions\/upload-artifact/);
});

test("the profile is materialized at runtime from the repository variable", () => {
  assert.match(workflowText, /vars\.FACTORY_DEPLOY_PROFILE/);
  const materialize = step("Materialize profile");
  assert.equal(materialize.env.PROFILE_JSON, "${{ vars.FACTORY_DEPLOY_PROFILE }}");
  assert.match(materialize.run, /printf '%s\\n' "\$PROFILE_JSON" > deploy\/profile\.json/);
  assert.match(materialize.run, /python3 tools\/deploy\.py check --profile deploy\/profile\.json/);
});

test("the reconcile step runs first and fails closed on a failed head lookup", () => {
  assert.equal(reconcileIndex, 0, "reconcile runs before checkout so it can pick the ref");
  assert.ok(checkoutIndex !== -1);
  assert.match(reconcile.run, /git ls-remote/);
  assert.match(reconcile.run, /refs\/heads\/main/);
  assert.match(reconcile.run, /\$GITHUB_SHA/);
  assert.match(reconcile.run, /exit 1/);
  assert.match(reconcile.run, /echo "sha=\$head" >> "\$GITHUB_OUTPUT"/);
  assert.equal(
    step("Deploy").if,
    undefined,
    "deploy is not gated: reconciliation already picked the head",
  );
});

test("a run at the current head deploys its own SHA", () => {
  const result = runReconcile({ runSha: "aaa", fakeHead: "aaa" });
  assert.equal(result.status, 0);
  assert.match(result.outputs, /^sha=aaa$/m);
});

test("a reordered or displaced stale run still deploys the latest head", () => {
  // An older run B displaced the current head C's pending run: when B finally
  // executes it must deploy C, not its own stale SHA and not skip.
  const result = runReconcile({ runSha: "bbb", fakeHead: "ccc" });
  assert.equal(result.status, 0);
  assert.match(result.outputs, /^sha=ccc$/m);
});

test("an old rerun reconciles to the current head instead of replaying its SHA", () => {
  const result = runReconcile({ runSha: "old", fakeHead: "ccc" });
  assert.equal(result.status, 0);
  assert.match(result.outputs, /^sha=ccc$/m);
});

test("a failed head lookup fails closed with no deployable SHA", () => {
  const result = runReconcile({ runSha: "aaa", fakeHead: "" });
  assert.equal(result.status, 1);
  assert.doesNotMatch(result.outputs, /^sha=/m);
});

test("missing inputs fail fast naming both inputs and the docs", () => {
  let output = "";
  try {
    output = execFileSync("sh", ["-c", step("Check deployment inputs").run], {
      env: {},
      encoding: "utf8",
    });
    assert.fail("input check passed with an empty environment");
  } catch (error) {
    assert.equal(error.status, 1);
    output = String(error.stdout);
  }
  assert.match(output, /FACTORY_DEPLOY_PROFILE/);
  assert.match(output, /CLOUDFLARE_API_TOKEN/);
  assert.match(output, /docs\/cloudflare\.md/);
});
