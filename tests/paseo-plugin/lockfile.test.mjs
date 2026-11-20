// Distribution guard for the Paseo plugin's own lockfile: every package keeps a
// public registry tarball URL and an integrity digest, as the root lockfile does.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { parse } from "yaml";

const REGISTRY = "https://registry.npmjs.org/";
const read = (name) => readFileSync(new URL(`../../paseo-plugin/${name}`, import.meta.url), "utf8");

test("the plugin workspace keeps tarball URLs in its lockfile", () => {
  assert.deepEqual(parse(read("pnpm-workspace.yaml")), { lockfileIncludeTarballUrl: true });
});

test("the plugin workspace pins the public registry", () => {
  assert.equal(read(".npmrc"), `registry=${REGISTRY}\n`);
});

test("every plugin tarball resolves through the public registry", () => {
  const entries = Object.entries(parse(read("pnpm-lock.yaml")).packages ?? {});
  assert.ok(entries.length > 100, "lockfile inventories the plugin dependency graph");
  for (const [path, entry] of entries) {
    const resolution = entry.resolution ?? {};
    assert.ok(
      resolution.tarball,
      `${path} has no tarball URL. Run pnpm --dir paseo-plugin install to record it, then commit paseo-plugin/pnpm-lock.yaml.`,
    );
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
