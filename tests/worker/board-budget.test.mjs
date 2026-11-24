// The room's SQLite budget under workerd: the rows each board operation reads and writes, from
// workerd's own cursor counters, at today's board (2,300 rows) and twice it. The Workers Free
// plan caps a day at 5M rows read and 100k rows written, and an operation past a cap fails until
// 00:00 UTC, so the daily projection at twice today's traffic must stay under half of each.
import assert from "node:assert/strict";
import test from "node:test";
import { bundle, roomWork } from "../../tools/board-miniflare.mjs";

const script = await bundle(new URL("../../worker/", import.meta.url).pathname);
const [today, doubled] = [await roomWork(2300, script), await roomWork(4600, script)];
const op = (work, prefix) => Object.entries(work.rows).find(([name]) => name.startsWith(prefix))[1];

// Requests a day at 2026-11-23's live rate (job 2233's tail): 5,900 PATCHes and 8,500 full
// board GETs, every GET taken as a new version. A claim with reads is one PATCH in ten, the
// alarm copies at most once per FLUSH_MS (30 s), and the archive pass runs hourly.
const DAY = {
  patch: 5900,
  claim: 590,
  get: 8500,
  copy: 2880,
  archive: 24,
};

function projection(work, rate) {
  const n = (name) => DAY[name] * rate;
  const parts = [
    [n("patch"), op(work, "PATCH 1 row")],
    [n("claim"), op(work, "PATCH claim")],
    [n("get"), op(work, "GET /board.json, new version")],
    // The bench's copy ran on text a GET had built; a copy that builds it adds a GET's reads.
    [
      Math.min(DAY.copy, n("patch")),
      {
        read: op(work, "R2 copy").read + op(work, "GET /board.json, new version").read,
        written: op(work, "R2 copy").written,
      },
    ],
    // Each copy's setAlarm writes the room's alarm, which no cursor counts: one row written.
    [Math.min(DAY.copy, n("patch")), { read: 0, written: 1 }],
    [n("archive"), op(work, "full PUT, archive pass")],
    // One seed a day, as on a deploy day.
    [1, op(work, "seed")],
  ];
  return {
    read: parts.reduce((sum, [count, o]) => sum + count * o.read, 0),
    written: parts.reduce((sum, [count, o]) => sum + count * o.written, 0),
  };
}

test("a row PATCH reads and writes the same SQLite rows as the board doubles", () => {
  for (const name of ["PATCH 1 row", "PATCH 5 rows", "PATCH adding", "PATCH a row carrying"]) {
    assert.equal(op(doubled, name).read, op(today, name).read, name);
    assert.equal(op(doubled, name).written, op(today, name).written, name);
  }
  assert.ok(op(doubled, "PATCH 1 row").read <= 20, JSON.stringify(op(doubled, "PATCH 1 row")));
  // The row vouches for its own upload names, so the 60 other rows that carry one go unread.
  assert.equal(op(doubled, "PATCH a row carrying").read, op(doubled, "PATCH 1 row").read);
  assert.ok(op(doubled, "PATCH 1 row").written <= 3);
  assert.ok(op(doubled, "PATCH 5 rows").written <= 4);
  // A claim's stage reads cost one SQL row together; its digest reads cost their pages.
  assert.ok(op(doubled, "PATCH claim").read <= 600, JSON.stringify(op(doubled, "PATCH claim")));
  assert.ok(op(doubled, "PATCH claim").written <= 3);
});

test("a whole-board read costs a row per page, and a same-version GET almost none", () => {
  for (const work of [today, doubled]) {
    assert.ok(op(work, "GET /board.json, new version").read <= 45);
    assert.ok(op(work, "GET /board.json, same version").read <= 3);
    assert.ok(op(work, "R2 copy").written <= 1);
    assert.ok(op(work, "full PUT, one row").written <= 3);
    assert.ok(op(work, "full PUT, archive pass").written <= 8);
  }
});

// Production evicts the room between GETs a minute apart, so no memo carries a read.
test("a GET after a PATCH in an evicted room reads its pages once; after the copy, none", () => {
  for (const work of [today, doubled]) {
    const built = op(work, "GET /board.json after a PATCH, room evicted");
    assert.ok(built.read <= 45 && built.written === 0, JSON.stringify(built));
    // R2's copy is the board again, so its bytes stream through and no page is read.
    const copied = op(work, "GET /board.json after the copy, room evicted");
    assert.ok(copied.read <= 3 && copied.written === 0, JSON.stringify(copied));
  }
});

test("the daily projection at twice today's board and traffic stays under half of each cap", () => {
  const p = projection(doubled, 2);
  assert.ok(p.read < 2_500_000, `rows read a day ${p.read}`);
  assert.ok(p.written < 50_000, `rows written a day ${p.written}`);
});
