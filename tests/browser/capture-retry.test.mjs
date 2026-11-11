import assert from "node:assert/strict";
import test from "node:test";
import { retryCapture } from "./capture-retry.mjs";

const label = "before-idea-desktop";

test("returns after a later success and logs each failed attempt", async () => {
  const lines = [];
  let calls = 0;
  const result = await retryCapture(
    label,
    async () => {
      if (++calls < 3) throw new Error("Runtime.callFunctionOn timed out");
      return "ok";
    },
    (line) => lines.push(line),
  );
  assert.equal(result, "ok");
  assert.deepEqual(lines, [
    "capture retry before-idea-desktop: attempt 1 of 3 failed: Runtime.callFunctionOn timed out",
    "capture retry before-idea-desktop: attempt 2 of 3 failed: Runtime.callFunctionOn timed out",
  ]);
});

test("rethrows the third error after two log lines", async () => {
  const lines = [];
  await assert.rejects(
    retryCapture(
      label,
      async (attempt) => {
        throw new Error(`failure ${attempt}`);
      },
      (line) => lines.push(line),
    ),
    { message: "failure 3" },
  );
  assert.equal(lines.length, 2);
});

test("logs nothing when the first attempt succeeds", async () => {
  const lines = [];
  assert.equal(
    await retryCapture(
      label,
      async () => "ok",
      (line) => lines.push(line),
    ),
    "ok",
  );
  assert.equal(lines.length, 0);
});
