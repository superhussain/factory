// runChild logs start and done lines, and kills a child that outlives its timeout.
import assert from "node:assert/strict";
import test from "node:test";
import { runChild } from "./run-child.mjs";

const logged = async (run) => {
  const lines = [];
  const log = console.log;
  console.log = (line) => lines.push(line);
  try {
    return { result: await run(), lines };
  } finally {
    console.log = log;
  }
};

test("runChild kills a stalled child at the timeout", async () => {
  const { result, lines } = await logged(() =>
    runChild("node", ["-e", "setInterval(() => {}, 1000)"], { timeoutMs: 300, label: "stall" }),
  );
  assert.equal(result.timedOut, true);
  assert.equal(result.code, null);
  assert.ok(result.ms < 5000, `took ${result.ms}ms`);
  assert.deepEqual(lines.slice(0, 2), ["start stall", "timeout stall after 0.3s"]);
  assert.match(lines[2], /^done stall: exit null \(\d+\.\ds\)$/);
});

test("runChild returns a finished child's exit code and output", async () => {
  const { result, lines } = await logged(() =>
    runChild("node", ["-e", "console.log('hi'); process.exit(3)"], {
      timeoutMs: 60000,
      label: "quick",
    }),
  );
  assert.deepEqual([result.code, result.output, result.timedOut], [3, "hi\n", false]);
  assert.equal(lines[0], "start quick");
  assert.match(lines[1], /^done quick: exit 3 \(\d+\.\ds\)$/);
});
