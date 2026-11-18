// Pure helpers behind the Paseo plugin's needs-you alerts: which jobs are new, and their copy.
import assert from "node:assert/strict";
import test from "node:test";
import { arrivals, needsBody, needsCount, needsTitle } from "../../paseo-plugin/shared/arrivals.ts";

const OLD = { id: "041-old", project: "sample", title: "Old" };
const JOB = { id: "042-sample-links", project: "sample", title: "Sample links" };

test("arrivals", () => {
  assert.deepEqual(arrivals(null, [JOB]), []);
  assert.deepEqual(arrivals(new Set(["041-old"]), [OLD, JOB]), [JOB]);
});

test("copy", () => {
  assert.equal(needsTitle(JOB), "Needs you: 042-sample-links");
  assert.equal(needsBody(JOB), "Sample links");
  assert.equal(needsBody({ ...JOB, title: "" }), "042-sample-links");
  assert.equal(needsCount(1), "1 job needs you");
  assert.equal(needsCount(2), "2 jobs need you");
});
