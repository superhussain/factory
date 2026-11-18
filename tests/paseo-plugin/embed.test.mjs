// Pure helpers the Paseo plugin uses at its boundaries: the dashboard origin setting,
// the frame URL with an optional job, and messages from the framed dashboard.
import assert from "node:assert/strict";
import test from "node:test";
import {
  dashboardSrc,
  parseDashboardOrigin,
  readEmbedMessage,
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
