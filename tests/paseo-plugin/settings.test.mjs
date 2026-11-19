// The host-scoped settings document: the dashboard URL and the opt-in reply buttons.
import assert from "node:assert/strict";
import test from "node:test";
import { factorySettings } from "../../paseo-plugin/shared/settings.ts";

test("a stored v1 document without the switch parses as off", () => {
  assert.equal(factorySettings.version, 1);
  assert.deepEqual(factorySettings.schema.parse({ origin: "https://factory.example.test" }), {
    origin: "https://factory.example.test",
    factoryLinkButtons: false,
  });
  assert.deepEqual(factorySettings.schema.parse({}), { origin: "", factoryLinkButtons: false });
});

test("the switch value round-trips", () => {
  const values = { origin: "https://factory.example.test", factoryLinkButtons: true };
  assert.deepEqual(factorySettings.schema.parse(values), values);
  assert.equal(factorySettings.schema.safeParse({ factoryLinkButtons: "yes" }).success, false);
});
