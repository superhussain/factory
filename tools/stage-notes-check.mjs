#!/usr/bin/env node
// Compare the live review and build notes with docs/repo-security.md.
//
// Run from the repository root: factory pipeline factory | node tools/stage-notes-check.mjs
// Prints one line per stage. Exit 0 if both match, 1 if not, 2 if stdin is not a pipeline view.
import { readFileSync } from "node:fs";

const DOC = new URL("../docs/repo-security.md", import.meta.url);
const NOTES = { review: "Review stage note", build: "Build stage note" };

function docNote(text, heading) {
  return new RegExp(`^### ${heading}\\n\\n\`\`\`text\\n(.*?)\\n\`\`\`$`, "ms").exec(text)?.[1];
}

function liveNote(prompts, stage) {
  const pattern = new RegExp(
    `<operator-preferences source="pipeline:factory@\\d+" stage="${stage}" trust="data">\\n(.*?)\\n</operator-preferences>`,
    "s",
  );
  return pattern.exec(prompts[stage] ?? "")?.[1];
}

function main() {
  let view;
  try {
    view = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(readFileSync(0)));
  } catch {
    view = null;
  }
  const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  const prompts = isObject(view) ? ("prompts" in view ? view.prompts : {}) : null;
  if (
    !isObject(prompts) ||
    !Object.keys(NOTES).every((s) => typeof (s in prompts ? prompts[s] : "") === "string")
  ) {
    console.error("stage-notes-check: stdin is not a factory pipeline view");
    return 2;
  }
  const doc = readFileSync(DOC, "utf8");
  let status = 0;
  for (const [stage, heading] of Object.entries(NOTES)) {
    const committed = docNote(doc, heading);
    const live = liveNote(prompts, stage);
    let line;
    if (committed === undefined) line = `${stage}: no note in docs/repo-security.md`;
    else if (live === undefined) line = `${stage}: missing from the live pipeline`;
    else if (live !== committed) line = `${stage}: differs from docs/repo-security.md`;
    else line = `${stage}: matches`;
    if (!line.endsWith("matches")) status = 1;
    console.log(line);
  }
  return status;
}

process.exitCode = main();
