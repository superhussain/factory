// The Python commands the Paseo plugin tries, and which errors mean "this one did not start".
import assert from "node:assert/strict";
import test from "node:test";
import { isStartFailure, pythonCommands } from "../../paseo-plugin/shared/python.ts";

test("Windows tries python3.exe, then the py launcher", () => {
  assert.deepEqual(pythonCommands("win32"), [["python3.exe"], ["py.exe", "-3"]]);
});

test("other platforms run python3 only", () => {
  assert.deepEqual(pythonCommands("linux"), [["python3"]]);
  assert.deepEqual(pythonCommands("darwin"), [["python3"]]);
});

test("a missing command or the Store stub is a start failure", () => {
  assert.equal(isStartFailure({ code: "ENOENT" }), true);
  assert.equal(isStartFailure({ code: 9009 }), true);
});

test("other exits and non-errors are not start failures", () => {
  for (const code of [1, 2, "9009"]) assert.equal(isStartFailure({ code }), false, String(code));
  assert.equal(isStartFailure(null), false);
  assert.equal(isStartFailure(undefined), false);
  assert.equal(isStartFailure(new Error("x")), false);
});
