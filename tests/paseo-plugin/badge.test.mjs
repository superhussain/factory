// The sidebar badge's text and screen reader label.
import assert from "node:assert/strict";
import test from "node:test";
import { badgeLabel, badgeText } from "../../paseo-plugin/shared/badge.ts";

test("badge text caps at 99+", () => {
  assert.equal(badgeText(1), "1");
  assert.equal(badgeText(99), "99");
  assert.equal(badgeText(100), "99+");
});

test("badge label names the inbox", () => {
  assert.equal(badgeLabel(6), "Factory inbox, 6 waiting");
});
