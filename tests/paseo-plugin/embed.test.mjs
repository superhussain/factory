// Pure helpers the Paseo plugin uses at its boundaries: the dashboard origin setting,
// the frame URL with an optional job, and messages from the framed dashboard.
import assert from "node:assert/strict";
import test from "node:test";
import {
  dashboardHost,
  dashboardSrc,
  frameLoaded,
  parseDashboardOrigin,
  readEmbedMessage,
  readSignedInMessage,
} from "../../paseo-plugin/shared/embed.ts";

const ORIGIN = "https://factory.example.test";

test("origin", () => {
  assert.equal(parseDashboardOrigin(" https://Factory.Example.test/ "), ORIGIN);
  for (const value of [
    "https://factory.example.test/board",
    "http://factory.example.test",
    "https://factory.example.test:8443",
    "https://user:secret@factory.example.test",
    "https://localhost",
    "",
  ])
    assert.equal(parseDashboardOrigin(value), null, value);
});

test("host", () => {
  assert.equal(
    dashboardHost(parseDashboardOrigin("https://factory.example.test/")),
    "factory.example.test",
  );
  assert.equal(
    dashboardHost(parseDashboardOrigin("https://Deets.Example.test")),
    "deets.example.test",
  );
  assert.equal(dashboardHost("https://localhost:8220"), "localhost:8220");
});

test("src", () => {
  assert.equal(dashboardSrc(ORIGIN), ORIGIN + "/");
  assert.ok(dashboardSrc(ORIGIN, "042-sample-links").endsWith("/#042-sample-links"));
  assert.equal(dashboardSrc(ORIGIN, "042/../x"), ORIGIN + "/");
});

test("message", () => {
  assert.deepEqual(readEmbedMessage({ type: "factory-embed-ready", v: 1 }), {
    type: "factory-embed-ready",
  });
  assert.deepEqual(
    readEmbedMessage({
      type: "factory-open-agent",
      v: 1,
      server: "synthetic-server",
      agent: "synthetic-agent",
    }),
    { type: "factory-open-agent", server: "synthetic-server", agent: "synthetic-agent" },
  );
  for (const data of [
    { type: "factory-open-agent", v: 1, server: "synthetic server", agent: "synthetic-agent" },
    { type: "factory-open-agent", v: 1, server: "synthetic-server" },
    { type: "factory-embed-ready", v: 2 },
    { type: "factory-embed-hello", v: 1 },
    "factory-embed-ready",
    null,
  ])
    assert.equal(readEmbedMessage(data), null, JSON.stringify(data));
});

test("inbox count", () => {
  const base = { type: "factory-inbox-count", v: 1 };
  assert.deepEqual(readEmbedMessage({ ...base, count: 1 }), {
    type: "factory-inbox-count",
    count: 1,
  });
  assert.deepEqual(readEmbedMessage({ ...base, count: 0 }), {
    type: "factory-inbox-count",
    count: 0,
  });
  for (const data of [
    { ...base, count: -1 },
    { ...base, count: 1.5 },
    { ...base, count: "1" },
    base,
    { ...base, count: 2 ** 53 },
    { ...base, v: 2, count: 1 },
  ])
    assert.equal(readEmbedMessage(data), null, JSON.stringify(data));
});

test("signed in", () => {
  const signedIn = { type: "factory-embed-signed-in", v: 1 };
  assert.equal(readSignedInMessage(signedIn), true);
  for (const data of [
    { type: "factory-embed-signed-in", v: 2 },
    { type: "factory-embed-ready", v: 1 },
    "factory-embed-signed-in",
    null,
  ])
    assert.equal(readSignedInMessage(data), false, JSON.stringify(data));
  assert.equal(readEmbedMessage(signedIn), null);
});

test("location", () => {
  const base = { type: "factory-embed-location", v: 1 };
  for (const hash of ["", "#/settings/display"])
    assert.deepEqual(readEmbedMessage({ ...base, hash }), {
      type: "factory-embed-location",
      hash,
    });
  for (const data of [{ ...base, hash: "#a b" }, base, { ...base, hash: "#/x", v: 2 }])
    assert.equal(readEmbedMessage(data), null, JSON.stringify(data));
});

test("a frame load clears the count, then says hello", () => {
  const calls = [];
  const target = { postMessage: (message, origin) => calls.push(["hello", message, origin]) };
  const clear = (count) => calls.push(["clear", count]);
  frameLoaded(target, ORIGIN, clear);
  assert.deepEqual(calls, [
    ["clear", null],
    ["hello", { type: "factory-embed-hello", v: 1 }, ORIGIN],
  ]);
  calls.length = 0;
  frameLoaded(null, ORIGIN, clear);
  assert.deepEqual(calls, [["clear", null]]);
});
