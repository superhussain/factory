// The command the Paseo plugin runs for the installed skill.
import assert from "node:assert/strict";
import test from "node:test";
import { runtimeCommand } from "../../paseo-plugin/shared/runtime.ts";

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
  assert.deepEqual(runtimeCommand("/s", "linux", files), ["/s/runtime/factory"]);
  assert.deepEqual(runtimeCommand("/s", "darwin", files), ["/s/runtime/factory"]);
});

test("the bundle entry and its arguments come first; without bundle.json runtime/factory still runs", () => {
  const entry = JSON.stringify({ entry: ["bin/factory", "-q"] });
  const files = skill({ "/s/bundle.json": entry, "/s/bin/factory": "", "/s/runtime/factory": "" });
  assert.deepEqual(runtimeCommand("/s", "linux", files), ["/s/bin/factory", "-q"]);
  assert.deepEqual(runtimeCommand("/s", "linux", skill({ "/s/runtime/factory": "" })), [
    "/s/runtime/factory",
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
      runtimeCommand("/s", "linux", files),
      ["/s/runtime/factory"],
      JSON.stringify(entry),
    );
  }
  const broken = skill({ "/s/bundle.json": "{", "/s/runtime/factory": "" });
  assert.deepEqual(runtimeCommand("/s", "linux", broken), ["/s/runtime/factory"]);
});

test("without a binary the error names /factory catch-up", () => {
  const files = skill({ "/s/bundle.json": BUNDLE, "/s/runtime/factory.py": "" });
  assert.throws(() => runtimeCommand("/s", "linux", files), /\/factory catch-up/);
  assert.throws(
    () => runtimeCommand("C:\\s", "win32", skill({ "C:\\s\\runtime\\factory": "" })),
    /\/factory catch-up/,
  );
});

test("Windows runs factory.exe", () => {
  const exe = JSON.stringify({ entry: ["runtime/factory.exe"] });
  const files = skill({ "C:\\s\\bundle.json": exe, "C:\\s\\runtime\\factory.exe": "" });
  assert.deepEqual(runtimeCommand("C:\\s", "win32", files), ["C:\\s\\runtime\\factory.exe"]);
  assert.deepEqual(runtimeCommand("C:\\s", "win32", skill({ "C:\\s\\runtime\\factory.exe": "" })), [
    "C:\\s\\runtime\\factory.exe",
  ]);
});
