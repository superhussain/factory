// Fixed JSON deployment projection: validation, determinism, nonmutation and locked
// invocation, with Wrangler and asset fakes for preflight and deploy.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  loadProfile,
  main,
  nodeToolingProblem,
  projectWrangler,
  ROOT,
} from "../../tools/deploy.mjs";

const FIXTURES = join(ROOT, "tests", "fixtures");
const VALID = join(FIXTURES, "deployment-valid.json");
const INVALID = join(FIXTURES, "deployment-invalid.json");
const CONFIG = join(ROOT, ".build", "wrangler.production.json");
const CONNECTION = join(ROOT, ".build", "connection.json");
const base = () => JSON.parse(readFileSync(VALID, "utf8"));

const runCli = (args, cwd = ROOT) =>
  spawnSync(process.execPath, [join(ROOT, "tools", "deploy.mjs"), ...args], {
    cwd,
    encoding: "utf8",
  });

const outputs = () =>
  Object.fromEntries(
    [CONFIG, CONNECTION].filter((p) => existsSync(p)).map((p) => [p, readFileSync(p, "utf8")]),
  );

function tempDir(t) {
  const dir = mkdtempSync(join(tmpdir(), "deploy-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function writeProfile(t, profile, raw) {
  const path = join(tempDir(t), "profile.json");
  writeFileSync(path, raw ?? JSON.stringify(profile));
  return path;
}

function renderVars(t, extra) {
  const dir = tempDir(t);
  const path = join(dir, "profile.json");
  writeFileSync(path, JSON.stringify({ ...base(), ...extra }));
  const config = projectWrangler(loadProfile(path), ROOT, join(dir, "out"));
  return JSON.parse(readFileSync(config, "utf8")).vars;
}

test("a valid check writes nothing", () => {
  const before = outputs();
  const result = runCli(["check", "--profile", VALID]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /profile ok: factory-example/);
  assert.deepEqual(outputs(), before);
});

test("named invalid cases", (t) => {
  const document = JSON.parse(readFileSync(INVALID, "utf8"));
  assert.equal(document.version, 1);
  const names = new Set();
  for (const c of document.cases) {
    assert.ok(!names.has(c.name), "unique case names");
    names.add(c.name);
    const profile = base();
    for (const key of c.remove ?? []) delete profile[key];
    Object.assign(profile, c.set ?? {});
    const path = writeProfile(t, profile, c.raw);
    const before = outputs();
    const result = runCli(["render", "--profile", path]);
    assert.equal(result.status, 2, `${c.name}: ${result.stderr}`);
    assert.ok(result.stderr.includes(c.error), c.name);
    assert.deepEqual(outputs(), before, "validation failure changed no output");
  }
});

test("overlay and test entry keys are rejected", (t) => {
  for (const [key, value] of [
    ["main", "tests/support/local-entry.js"],
    ["vars", {}],
    ["env", {}],
    ["routes", []],
    ["secrets", {}],
    ["assets", "./web"],
    ["auth", {}],
  ]) {
    const path = writeProfile(t, { ...base(), [key]: value });
    const before = outputs();
    const result = runCli(["render", "--profile", path]);
    assert.equal(result.status, 2, key);
    assert.match(result.stderr, /unknown profile keys/);
    assert.deepEqual(outputs(), before);
  }
});

test("usage errors exit 2", () => {
  for (const args of [
    [],
    ["check"],
    ["render", "--profile"],
    ["bogus", "--profile", VALID],
    ["check", "--profile", VALID, "extra"],
  ])
    assert.equal(runCli(args).status, 2, args.join(" "));
});

test("render is deterministic and fixed", () => {
  const first = runCli(["render", "--profile", VALID]);
  assert.equal(first.status, 0, first.stderr);
  assert.deepEqual(first.stdout.trim().split("\n"), [CONFIG, CONNECTION]);
  const snapshot = outputs();
  assert.equal(runCli(["render", "--profile", VALID]).status, 0);
  assert.deepEqual(outputs(), snapshot, "render is byte deterministic");
  assert.deepEqual(JSON.parse(snapshot[CONFIG]), {
    account_id: "00000000000000000000000000000000",
    ai: { binding: "AI" },
    compatibility_date: "2026-06-01",
    durable_objects: { bindings: [{ name: "BoardEvents", class_name: "BoardEvents" }] },
    main: join(ROOT, "worker", "index.js"),
    migrations: [{ tag: "v1", new_sqlite_classes: ["BoardEvents"] }],
    name: "factory-example",
    observability: { enabled: true, head_sampling_rate: 1, redact_query_string: true },
    preview_urls: false,
    r2_buckets: [{ binding: "BUCKET", bucket_name: "factory-example" }],
    routes: [{ custom_domain: true, pattern: "factory.example.test" }],
    triggers: { crons: ["17 * * * *"] },
    vars: {
      ACCESS_AUDIENCE: "example-application-audience",
      ACCESS_ISSUER: "https://example-team.cloudflareaccess.com",
      FACTORY_AGENT_LINKS: "{}",
      FACTORY_IMAGE_ORIGINS: "[]",
      FACTORY_ORIGIN: "https://factory.example.test",
      FACTORY_TITLE: "Factory",
    },
    workers_dev: false,
  });
  assert.deepEqual(JSON.parse(snapshot[CONNECTION]), {
    api_url: "https://factory.example.test",
    version: 1,
  });
  for (const text of Object.values(snapshot)) {
    assert.ok(text.endsWith("\n"));
    assert.ok(!text.includes("tests/support"));
    assert.ok(!text.includes("local-entry"));
  }
});

test("render writes Python's sorted, indented, ASCII-escaped JSON", (t) => {
  const vars = {
    title: "F\u00e4ctory \u{1f680}",
    agent_links: { web: { template: "https://p.example.test/{server}/{agent}", label: "W" } },
  };
  const dir = tempDir(t);
  const path = join(dir, "profile.json");
  writeFileSync(path, JSON.stringify({ ...base(), ...vars }));
  const config = readFileSync(projectWrangler(loadProfile(path), ROOT, join(dir, "out")), "utf8");
  assert.ok(config.includes('"FACTORY_TITLE": "F\\u00e4ctory \\ud83d\\ude80"'));
  assert.ok(
    config.includes(
      '"FACTORY_AGENT_LINKS": "{\\"web\\": {\\"label\\": \\"W\\", \\"template\\": \\"https://p.example.test/{server}/{agent}\\"}}"',
    ),
  );
  assert.ok(config.startsWith('{\n  "account_id": '));
});

test("render from another cwd", (t) => {
  const before = outputs();
  const result = runCli(["render", "--profile", VALID], tempDir(t));
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(outputs(), before, "render from another cwd writes the same bytes");
});

test("render with image publication configured and links disabled", (t) => {
  const vars = renderVars(t, { image_origins: ["https://images.example.test"] });
  assert.equal(vars.FACTORY_IMAGE_ORIGINS, '["https://images.example.test"]');
  assert.equal(vars.FACTORY_AGENT_LINKS, "{}");
});

test("render with a jira origin", (t) => {
  assert.equal(
    renderVars(t, { jira_origin: "https://jira.example.test" }).FACTORY_JIRA_ORIGIN,
    "https://jira.example.test",
  );
});

test("render with embed origins, the desktop one included", (t) => {
  assert.equal(
    renderVars(t, { embed_origins: ["https://paseo.example.test"] }).FACTORY_EMBED_ORIGINS,
    '["https://paseo.example.test"]',
  );
  const origins = ["https://paseo.example.test", "paseo://app"];
  assert.equal(
    runCli(["check", "--profile", writeProfile(t, { ...base(), embed_origins: origins })]).status,
    0,
  );
  assert.equal(
    renderVars(t, { embed_origins: origins }).FACTORY_EMBED_ORIGINS,
    '["https://paseo.example.test", "paseo://app"]',
  );
});

test("embed origins reject bad desktop entries and http", (t) => {
  for (const entry of [
    "paseo://app/",
    "paseo://*",
    "paseo:",
    "PASEO://app",
    "paseo://app.example.test",
    "http://paseo.example.test",
  ]) {
    const result = runCli([
      "check",
      "--profile",
      writeProfile(t, { ...base(), embed_origins: [entry] }),
    ]);
    assert.notEqual(result.status, 0, entry);
    assert.match(
      result.stderr,
      /embed_origins entry must be a canonical https origin or paseo:\/\/app/,
    );
  }
});

test("render with a tracer receipt key", (t) => {
  const key = "A".repeat(43) + "=";
  assert.equal(renderVars(t, { tracer_receipt_key: key }).FACTORY_TRACER_RECEIPT_KEY, key);
});

test("tracer receipt key rejects bad keys", (t) => {
  for (const key of ["A".repeat(42) + "==", "A".repeat(43) + "!", "", 32]) {
    const result = runCli([
      "check",
      "--profile",
      writeProfile(t, { ...base(), tracer_receipt_key: key }),
    ]);
    assert.notEqual(result.status, 0, String(key));
    assert.match(
      result.stderr,
      /tracer_receipt_key must be the base64 of a 32-byte Ed25519 public key/,
    );
  }
});

function fakes(calls, { dryRun = 0, deploy = 0, inspect = true } = {}) {
  return {
    version: "v24.14.0",
    run: (root, args) => {
      calls.push(["wrangler", ...args]);
      if (args.includes("--dry-run"))
        return {
          status: dryRun,
          stdout: "Total Upload: 1000.00 KiB / gzip: 200.00 KiB\n",
          stderr: "",
        };
      return { status: deploy, stdout: "deployed\n", stderr: "" };
    },
    tools: async () => ({
      prepareAssets: async () => calls.push(["prepare"]),
      inspectBundle: async () => {
        calls.push(["inspect"]);
        if (!inspect) throw new Error("bundle over the limit");
        return { limit: 1 };
      },
    }),
  };
}

async function quiet(fn) {
  const { log, error } = console;
  const write = process.stdout.write;
  const errWrite = process.stderr.write;
  console.log = console.error = () => {};
  process.stdout.write = process.stderr.write = () => true;
  try {
    return await fn();
  } finally {
    Object.assign(console, { log, error });
    process.stdout.write = write;
    process.stderr.write = errWrite;
  }
}

test("deploy runs prepare, dry-run, inspect, then deploy", async (t) => {
  if (!existsSync(join(ROOT, "node_modules", ".bin", "wrangler"))) return t.skip("no wrangler");
  const calls = [];
  assert.equal(await quiet(() => main(["deploy", "--profile", VALID], fakes(calls))), 0);
  assert.deepEqual(
    calls.map((c) => c[0]),
    ["prepare", "wrangler", "inspect", "wrangler"],
  );
  const [, dryRun, , real] = calls;
  assert.ok(dryRun.includes("--dry-run") && dryRun.includes("--config") && dryRun.includes(CONFIG));
  assert.deepEqual(real.slice(1), ["deploy", "--config", CONFIG]);
});

test("a failed dry-run or inspection never deploys", async (t) => {
  if (!existsSync(join(ROOT, "node_modules", ".bin", "wrangler"))) return t.skip("no wrangler");
  for (const options of [{ dryRun: 1 }, { inspect: false }]) {
    const calls = [];
    assert.equal(await quiet(() => main(["deploy", "--profile", VALID], fakes(calls, options))), 1);
    assert.equal(calls.filter((c) => c[0] === "wrangler").length, 1);
  }
});

test("the dry-run command stops after inspection", async (t) => {
  if (!existsSync(join(ROOT, "node_modules", ".bin", "wrangler"))) return t.skip("no wrangler");
  const calls = [];
  assert.equal(await quiet(() => main(["dry-run", "--profile", VALID], fakes(calls))), 0);
  assert.deepEqual(
    calls.map((c) => c[0]),
    ["prepare", "wrangler", "inspect"],
  );
});

test("preflight reports missing Wrangler", (t) => {
  assert.equal(
    nodeToolingProblem(tempDir(t)),
    "locked Wrangler is not installed; run mise exec -- pnpm install --frozen-lockfile first",
  );
});

test("preflight checks the Node version against package.json engines", (t) => {
  if (!existsSync(join(ROOT, "node_modules", ".bin", "wrangler"))) return t.skip("no wrangler");
  const old = (v) =>
    `node ${v} is older than package.json engines >=22.18.0; run this command through mise exec --`;
  assert.equal(nodeToolingProblem(ROOT, "v20.16.0"), old("v20.16.0"));
  assert.equal(nodeToolingProblem(ROOT, "v20.16.0-nightly"), old("v20.16.0-nightly"));
  assert.equal(
    nodeToolingProblem(ROOT, "v22.18.0x"),
    'could not read node --version output "v22.18.0x"; run this command through mise exec --',
  );
  assert.equal(nodeToolingProblem(ROOT, "v22.18.0"), null);
  assert.equal(nodeToolingProblem(ROOT, "v24.14.0"), null);
});
