// One child process for the browser run, with progress lines and a kill timeout, and the bounded
// pool the run spreads its children over.
import { spawn } from "node:child_process";

// With `ready`, the timeout starts once the output matches it instead of at spawn.
export function runChild(command, args, { env = {}, timeoutMs = 0, label = "", ready } = {}) {
  return new Promise((resolve) => {
    const started = Date.now();
    // Its own process group, so a timeout also kills the test subprocesses and Chrome it started.
    const child = spawn(command, args, { env: { ...process.env, ...env }, detached: true });
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
