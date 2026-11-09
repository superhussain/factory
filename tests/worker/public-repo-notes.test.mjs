import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { validatePipeline } from "../../worker/admission.js";

const doc = readFileSync(new URL("../../docs/repo-security.md", import.meta.url), "utf8");

const note = (heading) => {
  const match = doc.match(
    new RegExp(`^### ${heading}\\n\\n\`\`\`text\\n([\\s\\S]*?)\\n\`\`\`$`, "m"),
  );
  assert.ok(match, `docs/repo-security.md has a text block under ### ${heading}`);
  return match[1];
};

test("the public-repo stage notes in docs/repo-security.md pass pipeline admission", () => {
  const stage_notes = { review: note("Review stage note"), build: note("Build stage note") };
  assert.equal(validatePipeline({ schema: 1, stage_notes }), null);
});
