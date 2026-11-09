// Contract guard for action pinning: every remote `uses:` names a full commit
// SHA plus its release tag, and Dependabot keeps those pins fresh.
import assert from "node:assert/strict";
import test from "node:test";
import { readdirSync, readFileSync } from "node:fs";
import { parse } from "yaml";

const github = new URL("../../.github/", import.meta.url);
const files = [
  ...readdirSync(new URL("workflows/", github))
    .filter((name) => name.endsWith(".yml"))
    .map((name) => new URL(`workflows/${name}`, github)),
  ...readdirSync(new URL("actions/", github)).map(
    (name) => new URL(`actions/${name}/action.yml`, github),
  ),
];
const texts = files.map((file) => readFileSync(file, "utf8"));
const uses = texts.flatMap((text) => {
  const doc = parse(text);
  const jobs = Object.values(doc.jobs ?? {});
  return [
    ...jobs.flatMap((job) => (job.steps ?? []).map((step) => step.uses)),
    ...jobs.map((job) => job.uses),
    ...(doc.runs?.steps ?? []).map((step) => step.uses),
  ].filter(Boolean);
});

test("every remote uses is pinned to a full commit SHA", () => {
  assert.ok(uses.length > 0);
  for (const value of uses) {
    assert.match(value, /^\.\/|^[\w.-]+\/[\w./-]+@[0-9a-f]{40}$/);
  }
});

test("every pinned uses line names its release", () => {
  const lines = texts.flatMap((text) => text.split("\n")).filter((line) => /uses: [^.]/.test(line));
  assert.ok(lines.length > 0);
  for (const line of lines) {
    assert.match(line, /@[0-9a-f]{40} # v\d+\.\d+\.\d+$/);
  }
});

test("dependabot updates github-actions weekly for workflows and the composite action", () => {
  const config = parse(readFileSync(new URL("dependabot.yml", github), "utf8"));
  assert.equal(config.version, 2);
  assert.deepEqual(config.updates, [
    {
      "package-ecosystem": "github-actions",
      directories: ["/", "/.github/actions/ci-setup"],
      schedule: { interval: "weekly" },
    },
  ]);
});
