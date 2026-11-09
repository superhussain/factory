// Nx graph guard: the four concern projects stay script-backed and cacheable,
// no target command invokes Python, and nx.json carries no lint/format
// configuration (job 108 owns those targets) or credential-shaped strings.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const read = (name) => readFileSync(new URL(`../../${name}`, import.meta.url), "utf8");

const nxText = read("nx.json");
const nx = JSON.parse(nxText);
const manifest = JSON.parse(read("package.json"));

const PROJECT_FILES = [
  "worker/project.json",
  "web/project.json",
  "tools/project.json",
  "skills/project.json",
];
const PROJECT_NAMES = ["worker-hosted", "web-dashboard", "tooling", "device-bundle"];
const TARGETS = {
  auth: "pnpm run test:auth",
  protocol: "pnpm run test:protocol",
  assets: "pnpm run test:assets",
  deploy: "pnpm run test:deploy",
  local: "pnpm run test:local",
  browser: "pnpm run test:browser",
  cli: "pnpm run test:cli",
  install: "pnpm run test:install",
  verify: "pnpm run test:verify",
};
const OUTPUTS = {
  auth: [],
  protocol: [],
  assets: ["{workspaceRoot}/.build"],
  deploy: [],
  local: [],
  browser: [],
  cli: [],
  install: [],
  verify: [],
};
const INPUTS = {
  auth: [
    "default",
    "shared",
    "{workspaceRoot}/tests/worker/access.test.mjs",
    "{workspaceRoot}/tests/worker/entry-auth.test.mjs",
  ],
  protocol: [
    "default",
    "shared",
    "{workspaceRoot}/tests/worker/project-settings.test.mjs",
    "{workspaceRoot}/tests/worker/project-documents.test.mjs",
    "{workspaceRoot}/tests/worker/protocol-v1.test.mjs",
    "{workspaceRoot}/tests/worker/coverage.test.mjs",
    "{workspaceRoot}/tests/worker/lockfile.test.mjs",
    "{workspaceRoot}/nx.json",
    "{workspaceRoot}/tests/worker/nx-config.test.mjs",
    "{workspaceRoot}/tests/worker/public-repo-notes.test.mjs",
    "{workspaceRoot}/docs/repo-security.md",
  ],
  assets: [
    "default",
    "shared",
    "{workspaceRoot}/worker/**/*",
    "{workspaceRoot}/tools/assets.mjs",
    "{workspaceRoot}/THIRD_PARTY_NOTICES.md",
  ],
  deploy: [
    "default",
    "shared",
    "{workspaceRoot}/tests/tools/**",
    "{workspaceRoot}/tests/fixtures/deployment-valid.json",
  ],
  local: [
    "default",
    "shared",
    "!{workspaceRoot}/tools/deploy.py",
    "{workspaceRoot}/tests/support/**/*",
    "{workspaceRoot}/tests/fixtures/project-documents.md",
    "{workspaceRoot}/tests/fixtures/project-document-notes.md",
    "{workspaceRoot}/tests/fixtures/project-settings.md",
    "{workspaceRoot}/worker/**/*",
    "{workspaceRoot}/web/**/*",
    "{workspaceRoot}/tests/worker/*.py",
    "{workspaceRoot}/THIRD_PARTY_NOTICES.md",
    "{workspaceRoot}/.node-version",
  ],
  browser: [
    "default",
    "shared",
    "!{workspaceRoot}/tools/deploy.py",
    "{workspaceRoot}/tests/browser/**/*",
    "{workspaceRoot}/tests/fixtures/browser-cases.json",
    "{workspaceRoot}/tests/fixtures/project-documents.md",
    "{workspaceRoot}/tests/fixtures/project-document-notes.md",
    "{workspaceRoot}/tests/fixtures/project-settings-board.json",
    "{workspaceRoot}/tests/support/**/*",
    "{workspaceRoot}/worker/**/*",
    "{workspaceRoot}/web/**/*",
    "{workspaceRoot}/THIRD_PARTY_NOTICES.md",
    "{workspaceRoot}/.node-version",
    { env: "FACTORY_BROWSER_SHARD" },
  ],
  cli: [
    "default",
    "shared",
    "{workspaceRoot}/tests/cli/**/*",
    "{workspaceRoot}/tools/install.py",
    "{workspaceRoot}/tests/tools/test_install.py",
    "{workspaceRoot}/tests/fixtures/cli-v1.json",
    "{workspaceRoot}/docs/device.md",
    "{workspaceRoot}/docs/integrations.md",
    "{workspaceRoot}/worker/admission.js",
    "{workspaceRoot}/.github/workflows/ci.yml",
    "{workspaceRoot}/.github/actions/ci-setup/action.yml",
    { env: "FACTORY_FORBID_LITERALS_JSON" },
    { runtime: "python3 --version" },
  ],
  install: [
    "default",
    "shared",
    "{workspaceRoot}/tools/install.py",
    "{workspaceRoot}/tests/tools/test_install.py",
    "{workspaceRoot}/tests/cli/support.py",
    "{workspaceRoot}/skills/**/*",
    { runtime: "python3 --version" },
  ],
  verify: [
    "default",
    "shared",
    "{workspaceRoot}/tests/tools/test_verify.py",
    "{workspaceRoot}/tests/tools/test_pr_screenshots.py",
    "{workspaceRoot}/tests/tools/test_copy_pr_screenshots.py",
    "{workspaceRoot}/tests/tools/test_public_cutover.py",
    "{workspaceRoot}/tests/tools/test_rewrite_pr_links.py",
    "{workspaceRoot}/skills/factory/runtime/pr_screenshots.py",
    { runtime: "python3 --version" },
  ],
};

test("nx.json parses into a local cache over the shared inputs", () => {
  assert.equal(nx.cacheDirectory, ".nx/cache");
  assert.equal(nx.defaultBase, "main");
  assert.deepEqual(nx.namedInputs, {
    default: ["{projectRoot}/**/*"],
    shared: [
      "{workspaceRoot}/package.json",
      "{workspaceRoot}/pnpm-lock.yaml",
      "{workspaceRoot}/.node-version",
    ],
  });
});

test("every affected target is cacheable with its designed inputs and outputs", () => {
  assert.deepEqual(Object.keys(nx.targetDefaults), Object.keys(TARGETS));
  for (const [target, inputs] of Object.entries(INPUTS)) {
    const definition = nx.targetDefaults[target];
    assert.equal(definition.cache, true, `${target} is cacheable`);
    assert.deepEqual(definition.outputs, OUTPUTS[target], `${target} caches its outputs`);
    assert.deepEqual(definition.inputs, inputs, `${target} inputs match the design`);
  }
});

test("browser runs beside local on its own ports", () => {
  const tooling = JSON.parse(read("tools/project.json"));
  assert.equal(tooling.targets.browser.dependsOn, undefined);
  assert.equal(tooling.targets.local.dependsOn, undefined);
  assert.doesNotMatch(read("tests/browser/run.mjs"), /port: 82(20|21)/);
});

test("every declared target runs an existing package.json script", () => {
  const declared = new Set();
  const names = [];
  for (const file of PROJECT_FILES) {
    const project = JSON.parse(read(file));
    names.push(project.name);
    for (const [target, definition] of Object.entries(project.targets ?? {})) {
      declared.add(target);
      assert.equal(
        definition.command,
        TARGETS[target],
        `${project.name}:${target} runs its own script`,
      );
      assert.ok(
        manifest.scripts[definition.command.replace("pnpm run ", "")],
        `${project.name}:${target} names a real package.json script`,
      );
    }
  }
  assert.deepEqual(names, PROJECT_NAMES);
  assert.deepEqual([...declared].sort(), Object.keys(TARGETS).sort());
});

test("no target command invokes Python and nx.json declares no lint or format configuration", () => {
  assert.doesNotMatch(nxText, /lint|format/);
  for (const file of PROJECT_FILES) {
    for (const [target, definition] of Object.entries(JSON.parse(read(file)).targets ?? {})) {
      assert.doesNotMatch(
        definition.command,
        /python|\.py\b/,
        `${file}:${target} stays Python-free`,
      );
    }
  }
});

test("nx.json carries no credential-shaped strings", () => {
  assert.doesNotMatch(nxText, /token|secret|password|api[_-]?key|credential|bearer/i);
  assert.doesNotMatch(nxText, /\b[0-9a-f]{32,}\b/i);
  assert.doesNotMatch(nxText, /[A-Za-z0-9+/]{40,}={0,2}/);
});

test("nx.json connects to no remote cache", () => {
  for (const key of ["nxCloudId", "nxCloudAccessToken", "nxCloudUrl", "tasksRunnerOptions"]) {
    assert.equal(nx[key], undefined, key);
  }
  assert.doesNotMatch(nxText, /cloud/i);
});
