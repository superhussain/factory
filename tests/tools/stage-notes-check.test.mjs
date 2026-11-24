// tools/stage-notes-check.mjs with synthetic pipeline views built from the committed notes.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const ROOT = new URL("../../", import.meta.url).pathname;
const TOOL = join(ROOT, "tools", "stage-notes-check.mjs");
const DOC = readFileSync(join(ROOT, "docs", "repo-security.md"), "utf8");
const note = (heading) =>
  new RegExp(`^### ${heading}\\n\\n\`\`\`text\\n(.*?)\\n\`\`\`$`, "ms").exec(DOC)[1];
const REVIEW = note("Review stage note");
const BUILD = note("Build stage note");
const NOT_A_VIEW = "stdin is not a factory pipeline view";

const envelope = (source, stage, text) =>
  `<operator-preferences source="${source}" stage="${stage}" trust="data">\n${text}\n</operator-preferences>`;

function view(review, build) {
  const prompts = { review: envelope("pipeline:global@3", "review", "Global review text") };
  prompts.review += "\n\n" + envelope("pipeline:factory@3", "review", review);
  if (build !== null) prompts.build = envelope("pipeline:factory@3", "build", build);
  return JSON.stringify({ prompts });
}

const run = (input, tool = TOOL) =>
  spawnSync(process.execPath, [tool], { input, encoding: "utf8" });

test("both match with a global envelope first", () => {
  const result = run(view(REVIEW, BUILD));
  assert.equal(result.stdout, "review: matches\nbuild: matches\n");
  assert.equal(result.status, 0);
});

test("edited build text differs", () => {
  const result = run(view(REVIEW, BUILD.replace("synthetic", "fake")));
  assert.equal(result.stdout, "review: matches\nbuild: differs from docs/repo-security.md\n");
  assert.equal(result.status, 1);
});

test("missing build prompt", () => {
  const result = run(view(REVIEW, null));
  assert.equal(result.stdout, "review: matches\nbuild: missing from the live pipeline\n");
  assert.equal(result.status, 1);
});

test("input that is not a pipeline view exits 2", () => {
  for (const input of [
    "not json",
    "[]",
    JSON.stringify({ prompts: [] }),
    JSON.stringify({ prompts: { review: 1 } }),
    Buffer.from([0xff]),
  ]) {
    const result = run(input);
    assert.equal(result.status, 2, String(input));
    assert.match(result.stderr, new RegExp(NOT_A_VIEW));
  }
});

test("a checkout without the build note block", (t) => {
  const root = mkdtempSync(join(tmpdir(), "stage-notes-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "tools"));
  mkdirSync(join(root, "docs"));
  const tool = join(root, "tools", "stage-notes-check.mjs");
  copyFileSync(TOOL, tool);
  writeFileSync(
    join(root, "docs", "repo-security.md"),
    `### Review stage note\n\n\`\`\`text\n${REVIEW}\n\`\`\`\n`,
  );
  const result = run(view(REVIEW, BUILD), tool);
  assert.equal(result.stdout, "review: matches\nbuild: no note in docs/repo-security.md\n");
  assert.equal(result.status, 1);
});
