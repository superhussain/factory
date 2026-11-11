// Contract guard for action pinning: every action folder has an action file,
// every remote `uses:` names a full commit SHA plus its release tag, and
// Dependabot keeps those pins fresh.
import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { parse } from "yaml";

const github = new URL("../../.github/", import.meta.url);
const actionDirs = readdirSync(new URL("actions/", github)).filter((name) =>
  statSync(new URL(`actions/${name}`, github)).isDirectory(),
);
const actionFiles = (dir) =>
  ["action.yml", "action.yaml"]
    .map((file) => new URL(`actions/${dir}/${file}`, github))
    .filter((url) => existsSync(url));
const files = [
  ...readdirSync(new URL("workflows/", github))
    .filter((name) => /\.ya?ml$/.test(name))
    .map((name) => new URL(`workflows/${name}`, github)),
  ...actionDirs.flatMap(actionFiles),
];
const usesValue = (line) => line.match(/^\s*(?:-\s+)?uses:\s*["']?([^\s"'#]+)/)?.[1];
const pinnedLine = /@[0-9a-f]{40}["']? # v\d+\.\d+\.\d+$/;
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
  const lines = texts
    .flatMap((text) => text.split("\n"))
    .filter((line) => {
      const value = usesValue(line);
      return value !== undefined && !value.startsWith("./");
    });
  assert.ok(lines.length > 0);
  for (const line of lines) {
    assert.match(line, pinnedLine);
  }
});

test("every actions folder has action.yml or action.yaml", () => {
  assert.ok(actionDirs.length > 0, ".github/actions has no action folders");
  for (const dir of actionDirs) {
    assert.ok(
      actionFiles(dir).length > 0,
      `.github/actions/${dir} has neither action.yml nor action.yaml`,
    );
  }
});

test("uses lines parse quoted values", () => {
  const sha = "a".repeat(40);
  const cases = [
    ['- uses: "./.github/actions/ci-setup"', "./.github/actions/ci-setup"],
    ["- uses: ./.github/actions/ci-setup", "./.github/actions/ci-setup"],
    [`uses: 'actions/checkout@${sha}' # v7.0.1`, `actions/checkout@${sha}`],
  ];
  for (const [line, value] of cases) {
    assert.equal(usesValue(line), value);
  }
  assert.ok(pinnedLine.test(cases[2][0]));
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
