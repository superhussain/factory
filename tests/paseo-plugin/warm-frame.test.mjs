// The warm frame keeper: park with moveBefore, return warm, drop on origin change, expiry or no moveBefore.
import assert from "node:assert/strict";
import test from "node:test";
import {
  PRESS_MS,
  WARM_FRAME_MS,
  createFrameKeeper,
  freshPress,
  shouldFocusFrame,
} from "../../paseo-plugin/shared/warm-frame.ts";

const A = "https://factory.example.test";
const B = "https://other.example.test";

function parent(name, calls, { move = true, throws = false } = {}) {
  const node = { appendChild: (element) => calls.push([name, "appendChild", element.id]) };
  if (move)
    node.moveBefore = (element) => {
      if (throws) throw new Error("cannot move");
      calls.push([name, "moveBefore", element.id]);
    };
  return node;
}

function setup({ parkMove = true } = {}) {
  const calls = [];
  const clock = { now: 0 };
  let next = 0;
  const parkParent = parent("park", calls, { move: parkMove });
  const dom = {
    create(origin, src) {
      const id = ++next;
      calls.push(["create", origin, src]);
      return { id, src, remove: () => calls.push(["remove", id]) };
    },
    park: () => parkParent,
    dropped: (element) => calls.push(["dropped", element.id]),
  };
  const keeper = createFrameKeeper(dom, () => clock.now);
  const named = (type) => calls.filter((call) => call[0] === type);
  return { calls, clock, keeper, named };
}

test("first attach creates and appends", () => {
  const { calls, keeper, named } = setup();
  const result = keeper.attach(parent("host", calls), A, A + "/");
  assert.equal(result.warm, false);
  assert.equal(named("create").length, 1);
  assert.deepEqual(named("host"), [["host", "appendChild", 1]]);
});

test("park then attach on the same origin after two minutes is warm", () => {
  const { calls, clock, keeper, named } = setup();
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  keeper.park(frame);
  assert.deepEqual(named("park"), [["park", "moveBefore", 1]]);
  clock.now += 120_000;
  const again = keeper.attach(parent("host2", calls), A, A + "/");
  assert.equal(again.warm, true);
  assert.equal(again.frame, frame);
  assert.equal(frame.parkedAt, null);
  assert.deepEqual(named("host2"), [["host2", "moveBefore", 1]]);
  assert.equal(named("create").length, 1);
});

test("attach with another origin drops the parked frame", () => {
  const { calls, keeper, named } = setup();
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  keeper.park(frame);
  const other = keeper.attach(parent("host2", calls), B, B + "/");
  assert.equal(other.warm, false);
  assert.deepEqual(named("remove"), [["remove", 1]]);
  assert.deepEqual(named("dropped"), [["dropped", 1]]);
  assert.deepEqual(named("create")[1], ["create", B, B + "/"]);
});

test("attach after WARM_FRAME_MS parked is cold", () => {
  const { calls, clock, keeper, named } = setup();
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  keeper.park(frame);
  clock.now += WARM_FRAME_MS;
  const again = keeper.attach(parent("host2", calls), A, A + "/");
  assert.equal(again.warm, false);
  assert.deepEqual(named("dropped"), [["dropped", 1]]);
  assert.equal(named("create").length, 2);
});

test("the expiry timer drops a parked frame", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const { calls, keeper, named } = setup();
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  keeper.park(frame);
  t.mock.timers.tick(WARM_FRAME_MS - 1);
  assert.equal(keeper.current(), frame);
  t.mock.timers.tick(1);
  assert.deepEqual(named("remove"), [["remove", 1]]);
  assert.equal(keeper.current(), null);
});

test("park without moveBefore drops at once", () => {
  const { calls, keeper, named } = setup({ parkMove: false });
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  keeper.park(frame);
  assert.deepEqual(named("dropped"), [["dropped", 1]]);
  assert.deepEqual(named("park"), []);
  assert.equal(keeper.current(), null);
});

test("a host moveBefore that throws drops and creates a fresh frame", () => {
  const { calls, keeper, named } = setup();
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  keeper.park(frame);
  const again = keeper.attach(parent("host2", calls, { throws: true }), A, A + "/");
  assert.equal(again.warm, false);
  assert.deepEqual(named("dropped"), [["dropped", 1]]);
  assert.deepEqual(named("host2"), [["host2", "appendChild", 2]]);
});

test("park of a dropped frame touches no parent", () => {
  const { calls, keeper, named } = setup();
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  keeper.drop();
  const before = calls.length;
  keeper.park(frame);
  assert.equal(calls.length, before);
  assert.deepEqual(named("park"), []);
});

test("attach while the kept frame is attached replaces it", () => {
  const { calls, keeper, named } = setup();
  keeper.attach(parent("host", calls), A, A + "/");
  const again = keeper.attach(parent("host2", calls), A, A + "/");
  assert.equal(again.warm, false);
  assert.deepEqual(named("dropped"), [["dropped", 1]]);
  assert.deepEqual(named("host2"), [["host2", "appendChild", 2]]);
});

test("park clears onMessage and jumpTo moves only the kept frame", () => {
  const { calls, keeper } = setup();
  const { frame } = keeper.attach(parent("host", calls), A, A + "/");
  frame.onMessage = () => {};
  keeper.park(frame);
  assert.equal(frame.onMessage, null);
  keeper.jumpTo(frame, A + "/#043-sample-ask");
  assert.equal(frame.element.src, A + "/#043-sample-ask");
  keeper.drop();
  keeper.jumpTo(frame, A + "/#044-sample-other");
  assert.equal(frame.element.src, A + "/#043-sample-ask");
});

test("WARM_FRAME_MS is 30 minutes", () => {
  assert.equal(WARM_FRAME_MS, 1_800_000);
});

test("shouldFocusFrame focuses only a warm pointer return", () => {
  const ROW = { tagName: "DIV", isContentEditable: false };
  const cases = [
    [true, "pointer", ROW, true],
    [true, "pointer", null, true],
    [false, "pointer", ROW, false],
    [true, "keyboard", ROW, false],
    [true, null, ROW, false],
    [true, "pointer", { tagName: "TEXTAREA", isContentEditable: false }, false],
    [true, "pointer", { tagName: "INPUT", isContentEditable: false }, false],
    [true, "pointer", { tagName: "DIV", isContentEditable: true }, false],
  ];
  for (const [warm, press, active, expected] of cases)
    assert.equal(
      shouldFocusFrame(warm, press, active),
      expected,
      JSON.stringify([warm, press, active]),
    );
});

test("freshPress forgets a press after PRESS_MS", () => {
  assert.equal(PRESS_MS, 2_000);
  const at = 1_000_000;
  assert.equal(freshPress({ kind: "pointer", at }, at), "pointer");
  assert.equal(freshPress({ kind: "pointer", at }, at + 1_999), "pointer");
  assert.equal(freshPress({ kind: "pointer", at }, at + 2_000), null);
  assert.equal(freshPress({ kind: "keyboard", at }, at + 1_999), "keyboard");
  assert.equal(freshPress(null, at), null);
});
