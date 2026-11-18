// The sidebar badge's text and screen reader label.
import assert from "node:assert/strict";
import test from "node:test";
import { badgeLabel, badgeText, shownInboxCount } from "../../paseo-plugin/shared/badge.ts";

test("badge text caps at 99+", () => {
  assert.equal(badgeText(1), "1");
  assert.equal(badgeText(99), "99");
  assert.equal(badgeText(100), "99+");
});

test("badge label names the inbox", () => {
  assert.equal(badgeLabel(6), "Factory inbox, 6 waiting");
});

test("frame count wins only while the board is open", () => {
  assert.equal(shownInboxCount(2, 1, true), 1);
  assert.equal(shownInboxCount(2, 0, true), 0);
  assert.equal(shownInboxCount(2, null, true), 2);
  assert.equal(shownInboxCount(2, 1, false), 2);
});
