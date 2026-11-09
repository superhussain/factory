// Owned browser fixtures: the shared origin check admits only fixed ports or the
// fixture's own declared loopback origin, parallel fixtures stay isolated, and
// hidden or missing key targets fail setup at once. Synthetic data, ephemeral ports.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
  assertBrowserTarget,
  focusVisible,
  seedBoard,
  seedProject,
  withBrowserFixture,
} from "../support/browser-server.mjs";
import owned from "../support/owned-origin.cjs";
import { prepareAssets } from "../../tools/assets.mjs";

const { FIXED_OWNED_ORIGINS, isOwnedBrowserOrigin } = owned;
const ownedOriginPath = fileURLToPath(new URL("../support/owned-origin.cjs", import.meta.url));
const run = promisify(execFile);

test(
  "owned origin check accepts fixed ports and only the declared loopback fixture origin",
  { timeout: 180000 },
  () => {
    const D = "http://127.0.0.1:52000";
    assert.equal(isOwnedBrowserOrigin("http://127.0.0.1:8220", {}), true);
    assert.equal(isOwnedBrowserOrigin("http://127.0.0.1:8221", {}), true);
    assert.equal(isOwnedBrowserOrigin(D, { FACTORY_OWNED_BROWSER_ORIGIN: D }), true);
    assert.equal(isOwnedBrowserOrigin(D, {}), false);
    assert.equal(isOwnedBrowserOrigin(undefined, {}), false);
    for (const origin of [
      "http://localhost:52000",
      "https://127.0.0.1:52000",
      D + "/",
      "http://user@127.0.0.1:52000",
      "http://127.0.0.1:80",
      "http://127.0.0.1:70000",
      "http://10.0.0.1:52000",
      "https://factory.example.test",
    ]) {
      assert.equal(
        isOwnedBrowserOrigin(origin, { FACTORY_OWNED_BROWSER_ORIGIN: origin }),
        false,
        origin,
      );
    }
    assert.equal(
      isOwnedBrowserOrigin("http://127.0.0.1:52001", { FACTORY_OWNED_BROWSER_ORIGIN: D }),
      false,
    );
    assert.throws(() => assertBrowserTarget(D, {}), /owned loopback origins/);
  },
);

test("parallel owned fixtures do not collide", { timeout: 180000 }, async () => {
  await prepareAssets();
  const origins = [];
  let arrive;
  const arrived = new Promise((resolve) => (arrive = resolve));
  let waiting = 2;
  async function barrier() {
    if (--waiting === 0) arrive();
    await arrived;
  }
  const runs = await Promise.all(
    [1, 2].map((i) =>
      withBrowserFixture(async ({ origin, runDir, env }) => {
        origins[i - 1] = origin;
        const id = `80${i}-owned`;
        await seedBoard(origin, [
          { id, title: `Synthetic owned ${i}`, stage: "spec", updated: "2026-01-01T00:00:00Z" },
        ]);
        await barrier();
        const board = await (await fetch(origin + "/api/board")).json();
        assert.deepEqual(
          board.jobs.map((job) => job.id),
          [id],
        );
        const check = (target) =>
          run(
            process.execPath,
            [
              "-e",
              "require(process.argv[1]).assertOwnedBrowserOrigin(process.argv[2])",
              ownedOriginPath,
              target,
            ],
            { env: { ...process.env, ...env } },
          );
        await check(origin);
        const sibling = origins[2 - i];
        await assert.rejects(check(sibling), /owned loopback origins/);
        return { origin, runDir };
      }),
    ),
  );
  assert.notEqual(runs[0].origin, runs[1].origin);
  assert.notEqual(runs[0].runDir, runs[1].runDir);
  for (const { origin } of runs) assert.ok(!FIXED_OWNED_ORIGINS.includes(origin), origin);
});

test("hidden or missing key targets fail setup immediately", { timeout: 180000 }, async () => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedBoard(origin, [
      {
        id: "801-owned",
        project: "sample",
        title: "Synthetic owned card",
        stage: "spec",
        updated: "2026-01-01T00:00:00Z",
      },
    ]);
    const page = await browser.newPage();
    await page.goto(origin + "/dashboard.html");
    await page.waitForSelector(".card");
    const started = Date.now();
    await assert.rejects(focusVisible(page, "#d-resize"), /#d-resize: not visible/);
    assert.ok(Date.now() - started < 1000, "hidden target fails in under 1000 ms");
    await assert.rejects(focusVisible(page, "#no-such-control"), /missing/);
    await focusVisible(page, "#job-jump");
    assert.equal(await page.evaluate(() => document.activeElement.id), "job-jump");
  });
});

test("no browser or support file binds a fixed owned port", () => {
  const FIXED_PORT = /\bport:\s*(82(?:20|21))\b|listen\(\s*(82(?:20|21))\b/;
  for (const dir of ["browser", "support"]) {
    const root = new URL(`../${dir}/`, import.meta.url);
    for (const file of readdirSync(root, { recursive: true })) {
      if (!/\.[mc]js$/.test(file)) continue;
      const bound = readFileSync(new URL(file, root), "utf8").match(FIXED_PORT);
      const port = bound?.[1] ?? bound?.[2];
      assert.equal(bound, null, `tests/${dir}/${file} binds ${port}; use port 0`);
    }
  }
});
