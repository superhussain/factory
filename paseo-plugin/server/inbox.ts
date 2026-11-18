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

// On Windows `python3` is often the Microsoft Store stub, so fall back to the `py` launcher.
const PYTHONS = process.platform === "win32" ? [["python3"], ["py", "-3"]] : [["python3"]];

export async function readInbox(runtime: string): Promise<InboxEvent | null> {
  for (const [command, ...args] of PYTHONS) {
    try {
      const { stdout } = await execCommand(command, [...args, runtime, "inbox"], {
        timeout: 30_000,
        maxBuffer: 1_048_576,
      });
      const parsed = factoryInbox.event.safeParse(JSON.parse(stdout));
      return parsed.success ? parsed.data : null;
    } catch {}
  }
  return null;
}
