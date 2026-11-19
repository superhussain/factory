// One child process for the browser run, with progress lines and a kill timeout, and the bounded
// pool the run spreads its children over.
import { spawn } from "node:child_process";

export function runChild(command, args, { env = {}, timeoutMs = 0, label = "" } = {}) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, args, { env: { ...process.env, ...env } });
    if (label) console.log(`start ${label}`);
    let output = "";
    let timedOut = false;
    child.stdout.on("data", (chunk) => {
      output += chunk;
    });
    child.stderr.on("data", (chunk) => {
      output += chunk;
    });
    const timer =
      timeoutMs &&
      setTimeout(() => {
        timedOut = true;
        child.kill("SIGKILL");
        if (label) console.log(`timeout ${label} after ${timeoutMs / 1000}s`);
      }, timeoutMs);
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
