// Pure helpers behind the Paseo plugin's needs-you alerts: which jobs are new, and their copy.
import assert from "node:assert/strict";
import test from "node:test";
import {
  arrivals,
  createInbox,
  needsBody,
  needsTitle,
} from "../../paseo-plugin/shared/arrivals.ts";
import { factoryInbox } from "../../paseo-plugin/shared/inbox.ts";

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
});

test("inbox store alerts only on arrivals after the first snapshot", () => {
  const alerts = [];
  const inbox = createInbox((job) => alerts.push(job.id));
  let renders = 0;
  inbox.listen(() => renders++);
  inbox.apply({ count: 3, jobs: [OLD] });
  assert.deepEqual([inbox.get(), alerts, renders], [3, [], 1]);
  inbox.apply({ count: 4, jobs: [OLD, JOB] });
  inbox.apply({ count: 4, jobs: [OLD, JOB] });
  assert.deepEqual([inbox.get(), alerts], [4, ["042-sample-links"]]);
  const ten = Array.from({ length: 10 }, (_, i) => ({ id: `05${i}-new`, project: "", title: "" }));
  inbox.apply({ count: 14, jobs: [OLD, JOB, ...ten] });
  assert.deepEqual(alerts.slice(1), ["050-new", "051-new", "052-new"]);
});

test("inbox event defaults a missing jobs list to empty", () => {
  assert.deepEqual(factoryInbox.event.parse({ count: 2 }), { count: 2, jobs: [] });
});
