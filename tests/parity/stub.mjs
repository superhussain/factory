// Replay stub for gh, paseo, git, crontab and claude in parity runs.
//
// bin/<tool> runs `node stub.mjs <tool> ARGS...`. It appends {"argv": ARGS} to
// $FACTORY_STUB_DIR/<tool>.calls.jsonl, then replays the first entry in
// $FACTORY_STUB_DIR/<tool>.json whose argv equals ARGS, or exits 127.
import { appendFileSync, chmodSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

// Python's json.dumps: ", " and ": " separators, and every character outside space to ~ escaped.
export function pyJsonString(s) {
  return JSON.stringify(s).replace(
    /[\u007f-\uffff]/g,
    (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"),
  );
}

const quote = (s) => "'" + s.replaceAll("'", "'\\''") + "'";

export function launcher(binDir, tool) {
  const path = join(binDir, tool);
  const command = [process.execPath, fileURLToPath(import.meta.url), tool].map(quote).join(" ");
  writeFileSync(path, `#!/bin/sh\nexec ${command} "$@"\n`);
  chmodSync(path, 0o755);
}

function main([tool, ...args]) {
  const stub = process.env.FACTORY_STUB_DIR;
  appendFileSync(
    join(stub, `${tool}.calls.jsonl`),
    `{"argv": [${args.map(pyJsonString).join(", ")}]}\n`,
  );
  const recorded = join(stub, `${tool}.json`);
  const entries = existsSync(recorded) ? JSON.parse(readFileSync(recorded, "utf8")) : [];
  for (const entry of entries) {
    if (entry.argv.length === args.length && entry.argv.every((a, i) => a === args[i])) {
      process.stdout.write(entry.stdout);
      process.stderr.write(entry.stderr);
      return entry.code;
    }
  }
  process.stderr.write(`stub: no recorded response for ${[tool, ...args].join(" ")}\n`);
  return 127;
}

if (process.argv[1] === fileURLToPath(import.meta.url))
  process.exitCode = main(process.argv.slice(2));
