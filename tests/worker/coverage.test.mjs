// Hosted coverage map integrity: unique groups, existing replacement files and case
// identifiers, valid statuses, and complete original-file coverage.
import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, readFileSync } from "node:fs";

const map = JSON.parse(
  readFileSync(new URL("../fixtures/hosted-coverage.json", import.meta.url), "utf8"),
);

const ORIGINAL_FILES = [
  "project-settings.test.mjs",
  "project-documents.test.mjs",
  "project-document-recovery.test.mjs",
  "test_dashboard_card_wrap.cjs",
  "test_dashboard_column_collapse.cjs",
  "test_dashboard_idea_edit.cjs",
  "test_dashboard_jumper_order.cjs",
  "test_dashboard_jumper_project.cjs",
  "test_dashboard_jumper_sections.cjs",
  "test_dashboard_mobile_header.cjs",
  "test_dashboard_project_documents.cjs",
  "test_project_api.py",
  "test_project_documents_api.py",
];

test("hosted coverage map is complete and resolvable", () => {
  assert.equal(map.version, 1);
  assert.ok(Array.isArray(map.groups) && map.groups.length > 0);
  const ids = new Set();
  for (const group of map.groups) {
    assert.ok(/^[a-z0-9-]+$/.test(group.id), `neutral id ${group.id}`);
    assert.ok(!ids.has(group.id), `duplicate group ${group.id}`);
    ids.add(group.id);
    assert.ok(ORIGINAL_FILES.includes(group.source_file), `known source file for ${group.id}`);
    assert.ok(group.source_group, `source group for ${group.id}`);
    assert.ok(
      ["ported", "removed-historical-mechanism"].includes(group.status),
      `status for ${group.id}`,
    );
    if (group.status === "removed-historical-mechanism") {
      assert.ok(group.reason, `removed group ${group.id} needs a reason`);
      assert.ok(
        group.replacements.length > 0,
        `removed group ${group.id} names its current-behavior replacement`,
      );
    }
    assert.ok(group.replacements.length > 0, `group ${group.id} has replacements`);
    for (const replacement of group.replacements) {
      const path = new URL("../../" + replacement.file, import.meta.url);
      assert.ok(existsSync(path), `${replacement.file} exists for ${group.id}`);
      const content = readFileSync(path, "utf8");
      assert.ok(
        replacement.case && content.includes(replacement.case),
        `case ${JSON.stringify(replacement.case)} found in ${replacement.file} for ${group.id}`,
      );
    }
  }
  for (const file of ORIGINAL_FILES) {
    assert.ok(
      map.groups.some((g) => g.source_file === file),
      `no missing group for ${file}`,
    );
  }
});
