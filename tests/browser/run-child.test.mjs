// runChild logs start and done lines, and kills a child that outlives its timeout.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { runChild } from "./run-child.mjs";

const logged = async (run) => {
  const lines = [];
  const log = console.log;
  console.log = (line) => lines.push(line);
  try {
    return { result: await run(), lines };
  } finally {
    console.log = log;
  }
};

test("runChild kills a stalled child at the timeout", async () => {
  const { result, lines } = await logged(() =>
    runChild("node", ["-e", "setInterval(() => {}, 1000)"], { timeoutMs: 300, label: "stall" }),
  );
  assert.equal(result.timedOut, true);
  assert.equal(result.code, null);
  assert.ok(result.ms < 5000, `took ${result.ms}ms`);
  assert.deepEqual(lines.slice(0, 2), ["start stall", "timeout stall after 0.3s"]);
  assert.match(lines[2], /^done stall: exit null \(\d+\.\ds\)$/);
});

test("runChild returns a finished child's exit code and output", async () => {
  const { result, lines } = await logged(() =>
    runChild("node", ["-e", "console.log('hi'); process.exit(3)"], {
      timeoutMs: 60000,
      label: "quick",
    }),
  );
  assert.deepEqual([result.code, result.output, result.timedOut], [3, "hi\n", false]);
  assert.equal(lines[0], "start quick");
  assert.match(lines[1], /^done quick: exit 3 \(\d+\.\ds\)$/);
});

test("runChild kills a timed-out child's own children too", async () => {
  const script =
    "const c = require('child_process').spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });" +
    "process.stdout.write(c.pid + '\\n'); setInterval(() => {}, 1000)";
  // console.log colors a number under FORCE_COLOR, which Nx sets, so the script writes a plain line.
  // ready holds the timer until that line, so a slow runner cannot kill the child before it spawns.
  const { result } = await logged(() =>
    runChild("node", ["-e", script], { timeoutMs: 300, ready: /\n/ }),
  );
  assert.match(
    result.output,
    /^\d+\n$/,
    "the child printed its grandchild's pid before the timeout",
  );
  const pid = Number(result.output);
  // SIGKILL lands asynchronously, so poll until the grandchild is gone.
  const gone = () => {
    try {
      process.kill(pid, 0);
      return false;
    } catch (error) {
      return error.code === "ESRCH";
    }
  };
  for (let tries = 0; !gone() && tries < 100; tries++) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.ok(gone(), `grandchild ${pid} still running`);
});

test("SIGINT on the parent ends a detached child's process group", async () => {
  const dir = await mkdtemp(join(tmpdir(), "run-child-"));
  const pidFile = join(dir, "pid");
  const runChildUrl = new URL("./run-child.mjs", import.meta.url).href;
  const script =
    `import { runChild } from ${JSON.stringify(runChildUrl)};` +
    `runChild(process.execPath, ["-e", "require('fs').writeFileSync(process.env.PID_FILE, String(process.pid)); setInterval(() => {}, 1000)"]);`;
  const parent = spawn(process.execPath, ["--input-type=module", "-e", script], {
    env: { ...process.env, PID_FILE: pidFile },
    stdio: "ignore",
  });
  let pid = 0;
  try {
    for (let tries = 0; !pid && tries < 100; tries++) {
      pid = Number(await readFile(pidFile, "utf8").catch(() => "0"));
      if (!pid) await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(pid, "the child wrote its pid");
    const exited = once(parent, "exit");
    parent.kill("SIGINT");
    const [code] = await exited;
    assert.equal(code, 130);
    const gone = () => {
      try {
        process.kill(pid, 0);
        return false;
      } catch (error) {
        return error.code === "ESRCH";
      }
    };
    for (let tries = 0; !gone() && tries < 100; tries++) {
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(gone(), `child ${pid} still running`);
  } finally {
    if (pid) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {}
    }
    parent.kill("SIGKILL");
    await rm(dir, { recursive: true, force: true });
  }
});
