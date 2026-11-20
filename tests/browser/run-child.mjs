// One child process for the browser run, with progress lines and a kill timeout, and the bounded
// pool the run spreads its children over.
import { spawn } from "node:child_process";
import { constants } from "node:os";

// Live children's process groups. Each child runs detached, so Ctrl-C reaches only this process;
// it passes SIGINT or SIGTERM on to every group, then exits 130 or 143.
const groups = new Set();
let forwarding = false;
function forwardSignals() {
  if (forwarding) return;
  forwarding = true;
  for (const signal of ["SIGINT", "SIGTERM"])
    process.once(signal, () => {
      for (const pid of groups) {
        try {
          process.kill(-pid, signal);
        } catch {}
      }
      process.exit(128 + constants.signals[signal]);
    });
}

// With `ready`, the timeout starts once the output matches it instead of at spawn. A child whose output never matches `ready` gets no timeout.
export function runChild(command, args, { env = {}, timeoutMs = 0, label = "", ready } = {}) {
  return new Promise((resolve) => {
    const started = Date.now();
    // Its own process group, so a timeout also kills the test subprocesses and Chrome it started.
    const child = spawn(command, args, { env: { ...process.env, ...env }, detached: true });
    forwardSignals();
    groups.add(child.pid);
    if (label) console.log(`start ${label}`);
    let output = "";
    let timedOut = false;
    let timer;
    const arm = () => {
      timer = setTimeout(() => {
        timedOut = true;
        try {
          process.kill(-child.pid, "SIGKILL");
        } catch {
          child.kill("SIGKILL");
        }
        if (label) console.log(`timeout ${label} after ${timeoutMs / 1000}s`);
      }, timeoutMs);
    };
    const collect = (chunk) => {
      output += chunk;
      if (timeoutMs && ready && !timer && ready.test(output)) arm();
    };
    child.stdout.on("data", collect);
    child.stderr.on("data", collect);
    if (timeoutMs && !ready) arm();
    child.once("exit", (code) => {
      groups.delete(child.pid);
      clearTimeout(timer);
      const ms = Date.now() - started;
      if (label) console.log(`done ${label}: exit ${code} (${(ms / 1000).toFixed(1)}s)`);
      resolve({ code, output, timedOut, ms });
    });
  });
}

export async function pool(items, limit, worker) {
  const results = [];
  let next = 0;
  async function lane() {
    while (next < items.length) {
      const index = next++;
      results[index] = await worker(items[index]);
    }
  }
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, lane));
  return results;
}
