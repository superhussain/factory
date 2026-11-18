/// <reference types="node" />
import { execCommand } from "@getpaseo/plugin/server";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { factoryInbox, type InboxEvent } from "../shared/inbox";
import { isStartFailure, pythonCommands } from "../shared/python";

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

export async function readInbox(runtime: string): Promise<InboxEvent | null> {
  for (const [command, ...args] of pythonCommands(process.platform)) {
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
