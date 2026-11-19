// The slim CPU summarizer on made-up tail events.
import assert from "node:assert/strict";
import test from "node:test";
import { splitRecords, summarize } from "../live/slim-cpu.mjs";

const read = (at, cpuTime, outcome = "ok") => ({
  outcome,
  eventTimestamp: at,
  cpuTime,
  event: { request: { url: "https://factory.example.test/board.json?view=slim" } },
});

// Six misses, the last two 300 ms apart, the rest a minute apart, plus 1 ms hits.
function events(missMs) {
  const out = [];
  for (let n = 0; n < 5; n++) out.push(read(n * 60000, missMs), read(n * 60000 + 10000, 1));
  out.push(read(4 * 60000 + 300, missMs));
  out.push({
    ...read(5, 40),
    event: { request: { url: "https://factory.example.test/api/board" } },
  });
  return out;
}

test("12 ms misses with one close pair share", () => {
  const s = summarize(events(12));
  assert.equal(s.reads, 11);
  assert.equal(s.misses, 6);
  assert.equal(s.medianMissMs, 12);
  assert.equal(s.maxMissMs, 12);
  assert.equal(s.maxHitMs, 1);
  assert.equal(s.pairs, 1);
  assert.equal(s.exceeded, 0);
  assert.equal(s.share, true);
});

test("6 ms misses do not share", () => {
  const s = summarize(events(6));
  assert.equal(s.pairs, 1);
  assert.equal(s.share, false);
});

test("one exceededCpu outcome shares", () => {
  const s = summarize([...events(6), read(9 * 60000, 3, "exceededCpu")]);
  assert.equal(s.exceeded, 1);
  assert.equal(s.share, true);
});

test("pretty-printed events split into records, a brace inside a string included", () => {
  const first = { ...read(1, 2), logs: ['a } and \\" { b'] };
  const text = JSON.stringify(first, null, 2) + "\n" + JSON.stringify(read(2, 7), null, 2) + "\n";
  assert.deepEqual(splitRecords(text), [first, read(2, 7)]);
});
