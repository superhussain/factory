// tools/verify.mjs: selection, arguments, propagation and JSON. Every command goes to a fake
// runner, so no heavy command runs inside a test.
import assert from "node:assert/strict";
import test from "node:test";
import { HOSTED_SCRIPTS, ROOT, main } from "../../tools/verify.mjs";

const CARGO = ["cargo", "test", "--locked", "--manifest-path", "rust/Cargo.toml"];

function capture(fn) {
  const out = [];
  const err = [];
  const log = console.log;
  const error = console.error;
  console.log = (line) => out.push(line);
  console.error = (line) => err.push(line);
  try {
    return { code: fn(), out, err };
  } finally {
    console.log = log;
    console.error = error;
  }
}

function runMain(argv, { codes = [], hasPnpm = () => true } = {}) {
  const calls = [];
  const run = (command, options) => {
    calls.push({ command, options });
    return codes[calls.length - 1] ?? 0;
  };
  return { ...capture(() => main(argv, { run, hasPnpm })), calls };
}

const ids = (calls) =>
  calls.map(({ command }) => (command[1] === "run" ? command[2] : "rust-test"));

test("device group runs cargo test", () => {
  const { code, calls } = runMain(["device"]);
  assert.equal(code, 0);
  assert.deepEqual(
    calls.map((c) => c.command),
    [CARGO],
  );
  assert.equal(calls[0].options.cwd, ROOT);
  assert.equal(calls[0].options.env.WRANGLER_SEND_METRICS, "false");
});

test("hosted group runs the pnpm scripts in order", () => {
  const { code, calls } = runMain(["hosted"]);
  assert.equal(code, 0);
  assert.deepEqual(
    calls.map((c) => c.command),
    HOSTED_SCRIPTS.map((s) => ["pnpm", "run", s]),
  );
});

test("all group runs each leaf once in order", () => {
  const { code, calls } = runMain(["all"]);
  assert.equal(code, 0);
  assert.deepEqual(ids(calls), ["rust-test", ...HOSTED_SCRIPTS]);
});

test("missing pnpm fails closed for hosted and all", () => {
  for (const group of ["hosted", "all"]) {
    const { code, calls, err } = runMain([group], { hasPnpm: () => false });
    assert.equal(code, 2);
    assert.equal(calls.length, 0);
    assert.deepEqual(err, ["pnpm is required for the hosted checks but was not found on PATH"]);
  }
});

test("device group does not look for pnpm", () => {
  const { code, calls } = runMain(["device"], {
    hasPnpm: () => {
      throw new Error("pnpm resolved");
    },
  });
  assert.equal(code, 0);
  assert.equal(calls.length, 1);
});

test("invalid arguments exit 2 without running", () => {
  for (const argv of [
    [],
    ["nosuch"],
    ["device", "--wat"],
    ["device", "hosted"],
    ["--json"],
    ["device", "--json", "extra"],
  ]) {
    const { code, calls } = runMain(argv);
    assert.equal(code, 2, argv.join(" "));
    assert.equal(calls.length, 0);
  }
});

test("the first failure exits 1 and stops", () => {
  let result = runMain(["device"], { codes: [3] });
  assert.equal(result.code, 1);
  assert.equal(result.calls.length, 1);
  result = runMain(["all"], { codes: [0, 0, 0, 0, 3] });
  assert.equal(result.code, 1);
  assert.equal(result.calls.length, 5);
  assert.deepEqual(result.err, ["test:local failed with exit code 3"]);
});

test("the JSON summary keeps its shape and separators", () => {
  let result = runMain(["device", "--json"]);
  assert.equal(result.code, 0);
  assert.equal(
    result.out.at(-1),
    '{"version": 1, "group": "device", "status": "pass", "results": [{"id": "rust-test", "exit_code": 0}]}',
  );
  result = runMain(["device", "--json"], { codes: [1] });
  assert.equal(result.code, 1);
  assert.deepEqual(JSON.parse(result.out.at(-1)), {
    version: 1,
    group: "device",
    status: "fail",
    results: [{ id: "rust-test", exit_code: 1 }],
  });
});
