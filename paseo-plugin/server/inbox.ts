/// <reference types="node" />
import { execCommand } from "@getpaseo/plugin/server";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { factoryInbox, type InboxEvent } from "../shared/inbox";

const RUNTIME_PATHS = [
  ".claude/skills/factory/runtime/factory.py",
  ".agents/skills/factory/runtime/factory.py",
];

export function findRuntime(home = homedir()): string | null {
  for (const path of RUNTIME_PATHS) {
    const full = join(home, path);
    if (existsSync(full)) return full;
  }
  return null;
}

// On Windows `python3` is often the Microsoft Store stub. Fall back to the `py` launcher only when
// `python3` fails to start; a run that exits non-zero or prints bad output is not retried.
const PYTHONS = process.platform === "win32" ? [["python3"], ["py", "-3"]] : [["python3"]];

// A string code (ENOENT) is a spawn error. 9009 is cmd.exe's "command not found", which the Store stub also returns.
export function isStartFailure(error: unknown): boolean {
  const code = (error as { code?: unknown }).code;
  return typeof code === "string" || code === 9009;
}

export async function readInbox(runtime: string): Promise<InboxEvent | null> {
  for (const [command, ...args] of PYTHONS) {
    let stdout: string;
    try {
      ({ stdout } = await execCommand(command, [...args, runtime, "inbox"], {
        timeout: 30_000,
        maxBuffer: 1_048_576,
      }));
    } catch (error) {
      if (isStartFailure(error)) continue;
      return null;
    }
    try {
      const parsed = factoryInbox.event.safeParse(JSON.parse(stdout));
      return parsed.success ? parsed.data : null;
    } catch {
      return null;
    }
  }
  return null;
}
