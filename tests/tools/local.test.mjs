// startLocal serves the test entry, which skips auth, on one IP address and never on all of
// them, whatever the spelling. Checked before any Wrangler process starts.
import assert from "node:assert/strict";
import { mkdtemp, rm, utimes } from "node:fs/promises";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
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

// Parallel browser files each call prepareAssets. Wrangler dev reloads the worker when a
// .build file it bundles changes, which drops every live socket, so an unchanged file stays as is.
test("prepareAssets with unchanged assets keeps a running service's live socket open", async () => {
  await prepareAssets();
  // Wrangler compares only the contents of a file written moments before its build, so age them.
  const past = new Date(Date.now() - 60000);
  for (const name of ["mermaid.min.txt", "third-party-notices.txt"])
    await utimes(new URL(`../../.build/${name}`, import.meta.url), past, past);
  const dir = await mkdtemp(
    new URL(`../../.context/local-test-${process.pid}-`, import.meta.url).pathname,
  );
  const service = await startLocal({ persistDir: dir + "/r2" });
  try {
    const ws = new WebSocket(service.origin.replace("http", "ws") + "/events/board", {
      headers: { Origin: service.origin },
    });
    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = reject;
    });
    await prepareAssets();
    await new Promise((resolve) => setTimeout(resolve, 2000));
    assert.equal(ws.readyState, WebSocket.OPEN);
    ws.close(1000);
  } finally {
    await service.close();
    await rm(dir, { recursive: true, force: true });
  }
});
