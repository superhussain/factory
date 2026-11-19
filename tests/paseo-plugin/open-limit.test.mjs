// The cap on how often the framed dashboard can open links in the system browser.
import assert from "node:assert/strict";
import test from "node:test";
import { createOpenLimiter } from "../../paseo-plugin/shared/open-limit.ts";

test("allows three opens a second", () => {
  let t = 0;
  const allowOpen = createOpenLimiter(() => t);
  assert.deepEqual([allowOpen(), allowOpen(), allowOpen(), allowOpen()], [true, true, true, false]);
  t = 1000;
  assert.equal(allowOpen(), true);
});

test("dropped opens do not extend the window", () => {
  let t = 0;
  const allowOpen = createOpenLimiter(() => t);
  allowOpen();
  allowOpen();
  allowOpen();
  t = 500;
  assert.equal(allowOpen(), false);
  t = 1000;
  assert.equal(allowOpen(), true);
});
