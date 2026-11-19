// One Factory screen per dashboard origin across installations.
import assert from "node:assert/strict";
import test from "node:test";
import { BALLOT_KEY, joinBallot, sharedBallot } from "../../paseo-plugin/shared/ballot.ts";

const A = "https://factory.example.test";
const B = "https://other.example.test";

function member(ballot, log, name) {
  return joinBallot(ballot, (origin) => {
    log.push("+" + name + (origin === B ? " B" : ""));
    return () => log.push("-" + name);
  });
}

function three() {
  const ballot = sharedBallot({});
  const log = [];
  const mac = member(ballot, log, "mac");
  const windows = member(ballot, log, "windows");
  const wsl = member(ballot, log, "wsl");
  mac.online(A);
  windows.online(A);
  wsl.online(A);
  return { ballot, log, mac, windows, wsl };
}

test("one seat per origin", () => {
  const { log, mac, windows, wsl } = three();
  assert.deepEqual(log, ["+mac"]);
  assert.deepEqual([mac.elected(), windows.elected(), wsl.elected()], [true, false, false]);
});

test("offline seat hands over", () => {
  const { log, mac, windows } = three();
  mac.offline();
  assert.deepEqual(log.slice(-2).sort(), ["+windows", "-mac"]);
  assert.equal(windows.elected(), true);
});

test("back online takes it back", () => {
  const { log, mac, windows } = three();
  mac.offline();
  mac.online(A);
  assert.equal(mac.elected(), true);
  assert.equal(windows.elected(), false);
  assert.deepEqual(log.slice(-2).sort(), ["+mac", "-windows"]);
});

test("origin change re-registers", () => {
  const log = [];
  const mac = member(sharedBallot({}), log, "mac");
  mac.online(A);
  mac.online(A);
  mac.online(B);
  assert.deepEqual(log, ["+mac", "-mac", "+mac B"]);
});

test("all offline keeps the first", () => {
  const ballot = sharedBallot({});
  const mac = member(ballot, [], "mac");
  const windows = member(ballot, [], "windows");
  mac.online(A);
  windows.online(A);
  mac.offline();
  windows.offline();
  assert.equal(mac.elected(), true);
  assert.equal(windows.elected(), false);
});

test("origins split", () => {
  const ballot = sharedBallot({});
  const mac = member(ballot, [], "mac");
  const wsl = member(ballot, [], "wsl");
  mac.online(A);
  wsl.online(B);
  assert.equal(mac.elected(), true);
  assert.equal(wsl.elected(), true);
});

test("unset origins group together", () => {
  const ballot = sharedBallot({});
  const mac = member(ballot, [], "mac");
  const wsl = member(ballot, [], "wsl");
  mac.online(null);
  wsl.online(null);
  assert.equal(mac.elected(), true);
  assert.equal(wsl.elected(), false);
});

test("unknown seat waits", () => {
  const log = [];
  const mac = member(sharedBallot({}), log, "mac");
  mac.offline();
  assert.equal(mac.elected(), false);
  assert.deepEqual(log, []);
});

test("leave promotes the next", () => {
  const ballot = sharedBallot({});
  const log = [];
  const mac = member(ballot, log, "mac");
  const windows = member(ballot, log, "windows");
  mac.online(A);
  windows.online(A);
  mac.leave();
  assert.deepEqual(log.slice(-2), ["-mac", "+windows"]);
  assert.equal(ballot.seats.length, 1);
});

test("calls after leave are ignored", () => {
  const log = [];
  const mac = member(sharedBallot({}), log, "mac");
  mac.leave();
  mac.online(A);
  assert.equal(mac.elected(), false);
  assert.deepEqual(log, []);
});

test("shared per scope", () => {
  const scope = {};
  const other = {};
  const first = sharedBallot(scope);
  assert.equal(sharedBallot(scope), first);
  assert.notEqual(sharedBallot(other), first);
  assert.equal(scope[Symbol.for(BALLOT_KEY)], first);
});
