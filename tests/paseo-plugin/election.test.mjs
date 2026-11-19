// Election retry and recheck with a stubbed settings read.
import assert from "node:assert/strict";
import test from "node:test";
import { joinBallot, sharedBallot } from "../../paseo-plugin/shared/ballot.ts";
import { runElection } from "../../paseo-plugin/shared/election.ts";

const A = "https://factory.example.test";
const B = "https://other.example.test";

const settle = () => new Promise((r) => setImmediate(r));

function stubRead(results) {
  let reads = 0;
  return {
    read() {
      reads += 1;
      const next = results.shift();
      return next === "fail" ? Promise.reject(new Error("read failed")) : Promise.resolve(next);
    },
    count: () => reads,
  };
}

function member(ballot, log, name) {
  return joinBallot(ballot, (origin) => {
    log.push("+" + name + (origin === B ? " B" : ""));
    return () => log.push("-" + name);
  });
}

async function run(t, results) {
  const ballot = sharedBallot({});
  const log = [];
  const mac = member(ballot, log, "mac");
  const windows = member(ballot, log, "windows");
  windows.online(A);
  const { read, count } = stubRead(results);
  const election = runElection(mac, read, () => () => {});
  t.after(() => election.stop());
  await settle();
  election.recheck();
  await settle();
  return { log, mac, windows, count };
}

test("one failure retries at once", async (t) => {
  const { log, mac, count } = await run(t, [A, "fail", A]);
  assert.equal(count(), 3);
  assert.deepEqual(log, ["+windows", "+mac", "-windows"]);
  assert.equal(mac.elected(), true);
});

test("two failures hand over", async (t) => {
  const { log, windows, count } = await run(t, [A, "fail", "fail"]);
  assert.equal(count(), 3);
  assert.deepEqual(log, ["+windows", "+mac", "-windows", "-mac", "+windows"]);
  assert.equal(windows.elected(), true);
});

test("recheck reads again", async (t) => {
  const { log, mac, count } = await run(t, [A, B]);
  assert.equal(count(), 2);
  assert.deepEqual(log, ["+windows", "+mac", "-windows", "-mac", "+mac B", "+windows"]);
  assert.equal(mac.elected(), true);
});
