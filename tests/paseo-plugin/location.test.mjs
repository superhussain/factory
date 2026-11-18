// The per-origin saved dashboard page, the frame URL it builds, and the reload signal.
import assert from "node:assert/strict";
import test from "node:test";
import {
  frameUrl,
  readLocation,
  startUrl,
  writeLocation,
} from "../../paseo-plugin/shared/location.ts";
import { createReload } from "../../paseo-plugin/shared/reload.ts";

const A = "https://factory.example.test";
const B = "https://other.example.test";

function memory() {
  const map = new Map();
  return () => ({ getItem: (k) => map.get(k) ?? null, setItem: (k, v) => map.set(k, v) });
}

test("saved per origin", () => {
  const storage = memory();
  writeLocation(storage, A, "#042-sample-links/build");
  assert.equal(readLocation(storage, A), "#042-sample-links/build");
  assert.equal(readLocation(storage, B), "");
  assert.equal(readLocation(null, A), "");
});

test("storage errors", () => {
  const storage = () => {
    throw new Error("blocked");
  };
  assert.equal(readLocation(storage, A), "");
  writeLocation(storage, A, "#/inbox");
});

test("bad hashes", () => {
  for (const hash of ['#x"y', "#" + "a".repeat(300)]) {
    const storage = memory();
    writeLocation(storage, A, hash);
    assert.equal(readLocation(storage, A), "", hash);
    assert.equal(frameUrl(A, hash), A + "/");
  }
  const forged = () => ({ getItem: () => '#x"y', setItem() {} });
  assert.equal(readLocation(forged, A), "");
});

test("urls", () => {
  assert.equal(frameUrl(A, "#/inbox"), A + "/#/inbox");
  assert.equal(startUrl(A, "042-sample-links", "#/inbox"), A + "/#042-sample-links");
  assert.equal(startUrl(A, "042/../x", "#/inbox"), A + "/#/inbox");
  assert.equal(startUrl(A, undefined, ""), A + "/");
});

test("reload", () => {
  const reload = createReload();
  const calls = [];
  const off = reload.listen(() => calls.push("a"));
  reload.listen(() => calls.push("b"));
  reload.fire();
  off();
  reload.fire();
  assert.deepEqual(calls, ["a", "b", "b"]);
});
