// The plugin card capture fails and writes no PNG when the page makes a request. Needs the
// provisioned browser and paseo-plugin dependencies, so it runs by hand, never in CI.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

test("a page request fails the capture and writes no PNG", () => {
  const dir = mkdtempSync(join(tmpdir(), "card-capture-guard-"));
  const run = spawnSync(
    process.execPath,
    [new URL("../../tools/capture-plugin-cards.mjs", import.meta.url).pathname, "--capture", dir],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        FACTORY_CAPTURE_VIEWS: "plugin-card-first-login-1280",
        FACTORY_CAPTURE_PROBE_URL: "http://127.0.0.1:9/factory-capture-probe",
      },
    },
  );
  assert.notEqual(run.status, 0);
  assert.match(run.stderr, /requests during plugin card capture/);
  assert.deepEqual(readdirSync(dir), []);
});
