// The commands the Paseo plugin tries for the installed skill, and which errors mean "this one did not start".
import assert from "node:assert/strict";
import test from "node:test";
import {
  isStartFailure,
  pythonCommands,
  runtimeCommands,
} from "../../paseo-plugin/shared/runtime.ts";

// A skill folder of path -> text; a present file with no text reads as null, like a binary.
function skill(entries) {
  const map = new Map(Object.entries(entries));
  return { exists: (path) => map.has(path), read: (path) => map.get(path) ?? null };
}

const BUNDLE = JSON.stringify({
  commit: "a".repeat(40),
  tree: "b".repeat(64),
  entry: ["runtime/factory"],
});

test("the binary runs when the install has one", () => {
  const files = skill({
    "/s/bundle.json": BUNDLE,
    "/s/runtime/factory": "",
    "/s/runtime/factory.py": "",
  });
  assert.deepEqual(runtimeCommands("/s", "linux", files), [["/s/runtime/factory"]]);
  assert.deepEqual(runtimeCommands("/s", "darwin", files), [["/s/runtime/factory"]]);
});

test("the bundle entry and its arguments come first; without bundle.json runtime/factory still runs", () => {
  const entry = JSON.stringify({ entry: ["bin/factory", "-q"] });
  const files = skill({ "/s/bundle.json": entry, "/s/bin/factory": "", "/s/runtime/factory": "" });
  assert.deepEqual(runtimeCommands("/s", "linux", files), [["/s/bin/factory", "-q"]]);
  assert.deepEqual(runtimeCommands("/s", "linux", skill({ "/s/runtime/factory": "" })), [
    ["/s/runtime/factory"],
  ]);
});

test("an entry outside the skill or of the wrong shape is ignored", () => {
  for (const entry of [
    ["/usr/bin/factory"],
    ["../factory"],
    ["bin/../../factory"],
    ["C:/factory"],
    [],
    [""],
    [1],
    "bin/factory",
    null,
  ]) {
    const files = skill({ "/s/bundle.json": JSON.stringify({ entry }), "/s/runtime/factory": "" });
    assert.deepEqual(
      runtimeCommands("/s", "linux", files),
      [["/s/runtime/factory"]],
      JSON.stringify(entry),
    );
  }
  const broken = skill({ "/s/bundle.json": "{", "/s/runtime/factory": "" });
  assert.deepEqual(runtimeCommands("/s", "linux", broken), [["/s/runtime/factory"]]);
});

test("without a binary the plugin falls back to Python on factory.py", () => {
  const files = skill({ "/s/bundle.json": BUNDLE, "/s/runtime/factory.py": "" });
  assert.deepEqual(runtimeCommands("/s", "linux", files), [["python3", "/s/runtime/factory.py"]]);
});

test("Windows runs factory.exe, else python3.exe then the py launcher", () => {
  const exe = JSON.stringify({ entry: ["runtime/factory.exe"] });
  const files = skill({ "C:\\s\\bundle.json": exe, "C:\\s\\runtime\\factory.exe": "" });
  assert.deepEqual(runtimeCommands("C:\\s", "win32", files), [["C:\\s\\runtime\\factory.exe"]]);
  assert.deepEqual(
    runtimeCommands("C:\\s", "win32", skill({ "C:\\s\\runtime\\factory.exe": "" })),
    [["C:\\s\\runtime\\factory.exe"]],
  );
  assert.deepEqual(runtimeCommands("C:\\s", "win32", skill({ "C:\\s\\runtime\\factory": "" })), [
    ["python3.exe", "C:\\s\\runtime\\factory.py"],
    ["py.exe", "-3", "C:\\s\\runtime\\factory.py"],
  ]);
});

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
