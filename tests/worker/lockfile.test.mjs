// Distribution regression: every package tarball in the committed lockfile must
// resolve through the public npm registry with an integrity digest. A private
// or credentialed mirror authority would make the standalone installation
// non-portable and leak team-internal package sourcing into first-party source.
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import assert from "node:assert/strict";
import { parse } from "yaml";

const REGISTRY = "https://registry.npmjs.org/";

const DIRECT_PINS = {
  jose: "6.2.13",
  wrangler: "4.154.0",
  mermaid: "12.2.0",
  nx: "24.0.2",
  esbuild: "0.28.2",
  "puppeteer-core": "25.16.0",
  "@puppeteer/browsers": "3.2.6",
  yaml: "2.9.1",
  oxlint: "1.90.0",
  oxfmt: "0.75.0",
  typescript: "5.9.3",
  husky: "9.1.7",
};

const read = (name) => readFile(new URL(`../../${name}`, import.meta.url), "utf8");

test("every locked tarball resolves through the public registry", async () => {
  const lock = parse(await read("pnpm-lock.yaml"));
  const entries = Object.entries(lock.packages ?? {});
  assert.ok(entries.length > 200, "lockfile inventories the full dependency graph");
  for (const [path, entry] of entries) {
    const resolution = entry.resolution ?? {};
    assert.ok(
      resolution.tarball?.startsWith(REGISTRY),
      `${path} resolves through a non-public authority: ${resolution.tarball}`,
    );
    const url = new URL(resolution.tarball);
    assert.equal(url.username, "", `${path} has no registry username`);
    assert.equal(url.password, "", `${path} has no registry password`);
    assert.equal(url.search, "", `${path} has no registry query credentials`);
    assert.equal(url.hash, "", `${path} has no registry fragment`);
    assert.match(
      resolution.integrity ?? "",
      /^sha512-[A-Za-z0-9+/]{86}==$/,
      `${path} keeps an integrity digest`,
    );
  }
});

test("direct dependency pins keep their exact versions", async () => {
  const lock = parse(await read("pnpm-lock.yaml"));
  const manifest = JSON.parse(await read("package.json"));
  const importer = lock.importers["."];
  assert.deepEqual(Object.keys(lock.importers), ["."], "only the root is installed");
  assert.deepEqual({ ...manifest.dependencies, ...manifest.devDependencies }, DIRECT_PINS);
  for (const [name, version] of Object.entries(DIRECT_PINS)) {
    const locked = importer.dependencies?.[name] ?? importer.devDependencies?.[name];
    assert.equal(locked?.version, version, `${name} stays pinned at ${version}`);
    assert.equal(locked?.specifier, version, `${name} stays an exact direct pin`);
    assert.ok(lock.packages[`${name}@${version}`], `${name} has a locked resolution`);
  }
});

test("toolchain pins and installation safeguards agree", async () => {
  const manifest = JSON.parse(await read("package.json"));
  assert.equal(manifest.packageManager, "pnpm@11.9.0");
  assert.equal(manifest.engines.node, ">=22.18.0");
  assert.equal((await read(".node-version")).trim(), "24.14.0");
  assert.equal(
    await read("mise.toml"),
    '[tools]\nnode = "24.14.0"\npnpm = "11.9.0"\ngitleaks = "8.30.1"\n',
  );
  assert.equal(await read(".npmrc"), `registry=${REGISTRY}\n`);
  assert.deepEqual(parse(await read("pnpm-workspace.yaml")), {
    nodeLinker: "isolated",
    lockfileIncludeTarballUrl: true,
    strictDepBuilds: true,
    sideEffectsCache: false,
    verifyDepsBeforeRun: "error",
    allowBuilds: {
      "esbuild@0.28.2": true,
      "workerd@1.20261023.1": true,
      "nx@24.0.2": true,
      "fsevents@2.3.3": false,
    },
  });
});
