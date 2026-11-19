// Dashboard links in agent replies, the shared screen openers, and the opt-in switch.
import assert from "node:assert/strict";
import test from "node:test";
import {
  createLinkSwitch,
  findDashboardLinks,
  MAX_DASHBOARD_LINKS,
  OPENERS_KEY,
  screenOpeners,
  setScreenOpener,
} from "../../paseo-plugin/shared/deep-link.ts";

const A = "https://factory.example.test";
const B = "https://other.example.test";
const BUILD = {
  url: A + "/#042-sample-links/build",
  origin: A,
  hash: "#042-sample-links/build",
  label: "042-sample-links/build",
};

test("markdown and bare links", () => {
  assert.deepEqual(findDashboardLinks(`Spec ready: [042 build](${A}/#042-sample-links/build)`, A), [
    BUILD,
  ]);
  assert.deepEqual(findDashboardLinks(`See ${A}/#042-sample-links/build.`, A), [BUILD]);
  assert.deepEqual(findDashboardLinks(`(${A}/#042-sample-links/build)`, A), [BUILD]);
  assert.deepEqual(findDashboardLinks(`${A}/#042-sample-links/build`, A), [BUILD]);
});

test("board root and host case", () => {
  const root = { url: A + "/", origin: A, hash: "", label: "the board" };
  assert.deepEqual(findDashboardLinks(`Open ${A}/ now`, A), [root]);
  assert.deepEqual(findDashboardLinks(`Open ${A}`, A), [root]);
  assert.deepEqual(findDashboardLinks("https://FACTORY.example.test/#042-sample-links/build", A), [
    BUILD,
  ]);
});

test("hash with balanced parentheses", () => {
  assert.deepEqual(findDashboardLinks(`[x](${A}/#a(b))`, A), [
    { url: A + "/#a(b)", origin: A, hash: "#a(b)", label: "a(b)" },
  ]);
});

test("rejects other links", () => {
  for (const text of [
    `${B}/#042-sample-links/build`,
    "https://factory.example.test.evil.test/#042-sample-links/build",
    "https://factory.example.test:8443/#042-sample-links/build",
    "https://factory.example.test/jobs#042-sample-links/build",
    "https://factory.example.test/?x=1#042-sample-links/build",
    "https://factory.example.test?x=1",
    "http://factory.example.test/#042-sample-links/build",
    "https://user@factory.example.test/#042",
    "https://factory.example.test/#a$b",
    "https://factory.example.test/#" + "a".repeat(300),
    "",
  ])
    assert.deepEqual(findDashboardLinks(text, A), [], text);
});

test("dedupes and caps", () => {
  assert.equal(MAX_DASHBOARD_LINKS, 5);
  assert.deepEqual(
    findDashboardLinks(`${A}/#042-sample-links/build ${BUILD.url} ${BUILD.url}`, A),
    [BUILD],
  );
  const many = Array.from({ length: 8 }, (_, i) => `${A}/#job-${i}`).join(" ");
  const links = findDashboardLinks(many, A);
  assert.equal(links.length, 5);
  assert.equal(links[4].hash, "#job-4");
});

test("openers are shared and removed only by their owner", () => {
  const scope = {};
  assert.equal(screenOpeners(scope), screenOpeners(scope));
  assert.equal(scope[Symbol.for(OPENERS_KEY)], screenOpeners(scope));
  const first = () => {};
  const second = () => {};
  const removeFirst = setScreenOpener(scope, A, first);
  const removeSecond = setScreenOpener(scope, A, second);
  removeFirst();
  assert.equal(screenOpeners(scope).get(A), second);
  removeSecond();
  assert.equal(screenOpeners(scope).has(A), false);
});

function recorder() {
  const log = [];
  const add = (origin) => {
    log.push("+" + origin);
    return () => log.push("-" + origin);
  };
  return { log, add };
}

test("switch off never registers", () => {
  const { log, add } = recorder();
  const links = createLinkSwitch(add);
  links.update(A, false);
  links.update(A, false);
  links.update(null, true);
  links.stop();
  assert.deepEqual(log, []);
});

test("switch on registers once, off cleans up", () => {
  const { log, add } = recorder();
  const links = createLinkSwitch(add);
  links.update(A, true);
  links.update(A, true);
  assert.deepEqual(log, ["+" + A]);
  links.update(A, false);
  assert.deepEqual(log, ["+" + A, "-" + A]);
});

test("origin change swaps the registration", () => {
  const { log, add } = recorder();
  const links = createLinkSwitch(add);
  links.update(A, true);
  links.update(B, true);
  links.update(null, true);
  assert.deepEqual(log, ["+" + A, "-" + A, "+" + B, "-" + B]);
  links.update(A, true);
  links.stop();
  links.stop();
  assert.deepEqual(log.slice(4), ["+" + A, "-" + A]);
});
