// startLocal serves the test entry, which skips auth, on one IP address and never on all of
// them, whatever the spelling. Checked before any Wrangler process starts.
import assert from "node:assert/strict";
import test from "node:test";
import { startLocal } from "../../tools/local.mjs";

const REFUSED = "serve on one IP address, never all of them";

test("startLocal refuses the unspecified address in every spelling, and non-IPs", async () => {
  for (const ip of [
    "0.0.0.0",
    "0",
    "::",
    "::0",
    "0:0:0:0:0:0:0:0",
    "0000::0000",
    "::ffff:0.0.0.0",
    "::ffff:0:0",
    "::0.0.0.0",
    "localhost",
    "",
  ])
    await assert.rejects(startLocal({ ip }), { message: REFUSED }, ip);
});

test("startLocal accepts one specific address", async () => {
  // No persistDir, so an accepted address stops at the next check, before any process starts.
  for (const ip of ["127.0.0.1", "192.0.2.10", "::1", "2001:db8::10"])
    await assert.rejects(
      startLocal({ ip }),
      { message: "startLocal requires an explicit fresh persistDir" },
      ip,
    );
});
