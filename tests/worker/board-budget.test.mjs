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
// board GETs. One PATCH in ten is a claim with reads, counted at its own cost within the 5,900.
// The alarm is set at most once per FLUSH_MS (30 s), and the archive pass runs hourly. Each
// version is built and copied to R2 at most once, by its first read or else by the alarm, so a
// day holds at most one copy per PATCH; every other GET streams R2's copy. The dashboard's slim
// reads were 9 in 12.6 minutes of job 2280's tail, about 1,030 a day; the projection counts
// 8,500, each as a miss in an evicted room, which joins the slim body from every page.
const DAY = {
  patch: 5900,
  claim: 590,
  get: 8500,
  slim: 8500,
  copy: 2880,
  archive: 24,
};

const costlier = (a, b) => ({
  read: Math.max(a.read, b.read),
  written: Math.max(a.written, b.written),
});

function projection(work, rate) {
  const n = (name) => DAY[name] * rate;
  const parts = [
    [n("patch") - n("claim"), op(work, "PATCH 1 row")],
    [n("claim"), op(work, "PATCH claim")],
    [n("get"), op(work, "GET /board.json after the copy, room evicted")],
    [n("slim"), costlier(op(work, "GET slim, room evicted"), op(work, "GET slim, new version"))],
    // A copy costs the costlier of a first read's build and copy, and the alarm's.
    [n("patch"), costlier(op(work, "GET /board.json after a PATCH"), op(work, "R2 copy"))],
    // Each setAlarm writes the room's alarm, which no cursor counts: one row written.
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
    // The first GET of a version reads its pages, then copies them to R2.
    assert.ok(op(work, "GET /board.json, new version").read <= 80);
    assert.ok(op(work, "GET /board.json, new version").written <= 1);
    assert.ok(op(work, "GET /board.json, same version").read <= 3);
    assert.ok(op(work, "R2 copy").written <= 1);
    assert.ok(op(work, "full PUT, one row").written <= 3);
    assert.ok(op(work, "full PUT, archive pass").written <= 8);
  }
});

// Production evicts the room between GETs a minute apart, so no memo carries a read.
test("a GET after a PATCH in an evicted room reads its pages once; after the copy, none", () => {
  for (const work of [today, doubled]) {
    // Its build, then its copy's dirty pages and one state row written.
    const built = op(work, "GET /board.json after a PATCH, room evicted");
    assert.ok(built.read <= 80 && built.written <= 1, JSON.stringify(built));
    // R2's copy is the board again, so its bytes stream through and no page is read.
    const copied = op(work, "GET /board.json after the copy, room evicted");
    assert.ok(copied.read <= 3 && copied.written === 0, JSON.stringify(copied));
  }
});

// The slim body is joined from the pages' bytes by their meta, so a miss reads a row per page
// and writes nothing, and a hit reads only the room's state.
test("a slim read costs a row per page on a miss, almost none on a hit, and writes nothing", () => {
  for (const work of [today, doubled]) {
    for (const name of ["GET slim, new version", "GET slim, room evicted"]) {
      const o = op(work, name);
      assert.ok(o.read <= 80 && o.written === 0, `${name} ${JSON.stringify(o)}`);
    }
    assert.ok(op(work, "GET slim, same version").read <= 3);
  }
});

test("the daily projection at twice today's board and traffic stays under half of each cap", () => {
  const p = projection(doubled, 2);
  assert.ok(p.read < 2_500_000, `rows read a day ${p.read}`);
  assert.ok(p.written < 50_000, `rows written a day ${p.written}`);
});
