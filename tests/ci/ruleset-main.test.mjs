// Contract guard for the `main` ruleset: it protects the default branch with
// no bypass, and its one required check is the `ci` aggregate job in ci.yml.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { parse } from "yaml";

const ruleset = JSON.parse(
  readFileSync(new URL("../../.github/rulesets/main.json", import.meta.url), "utf8"),
);
const workflow = parse(
  readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8"),
);
const rule = (type) => ruleset.rules.find((r) => r.type === type);

test("ruleset targets the default branch, active, with no bypass", () => {
  assert.equal(ruleset.target, "branch");
  assert.equal(ruleset.enforcement, "active");
  assert.deepEqual(ruleset.conditions.ref_name.include, ["~DEFAULT_BRANCH"]);
  assert.deepEqual(ruleset.bypass_actors, []);
});

test("ruleset has exactly the four rules", () => {
  assert.deepEqual(ruleset.rules.map((r) => r.type).sort(), [
    "deletion",
    "non_fast_forward",
    "pull_request",
    "required_status_checks",
  ]);
});

test("pull_request rule needs zero approvals", () => {
  assert.equal(rule("pull_request").parameters.required_approving_review_count, 0);
});

test("required check is the ci aggregate", () => {
  const { parameters } = rule("required_status_checks");
  assert.deepEqual(parameters.required_status_checks, [{ context: "ci", integration_id: 15368 }]);
  assert.equal(parameters.strict_required_status_checks_policy, false);
  const { context } = parameters.required_status_checks[0];
  const job = workflow.jobs[context];
  assert.ok(job);
  assert.equal("name" in job, false);
  const others = Object.keys(workflow.jobs).filter((key) => key !== context);
  assert.deepEqual([...job.needs].sort(), others.sort());
});
