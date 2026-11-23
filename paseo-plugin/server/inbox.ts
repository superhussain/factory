/// <reference types="node" />
import { execCommand } from "@getpaseo/plugin/server";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { factoryInbox, type InboxEvent } from "../shared/inbox";
import { isStartFailure, runtimeCommands, type SkillFiles } from "../shared/runtime";

const SKILL_PATHS = [".claude/skills/factory", ".agents/skills/factory"];

const files: SkillFiles = {
  exists: existsSync,
  read: (path) => {
    try {
      return readFileSync(path, "utf8");
    } catch {
      return null;
    }
  },
};

export function findSkill(home = homedir()): string | null {
  for (const path of SKILL_PATHS) {
    const full = join(home, path);
    if (existsSync(join(full, "runtime"))) return full;
  }
  return null;
}

export async function readInbox(skill: string): Promise<InboxEvent | null> {
  for (const [command, ...args] of runtimeCommands(skill, process.platform, files)) {
    let stdout: string;
    try {
      ({ stdout } = await execCommand(command, [...args, "inbox"], {
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
