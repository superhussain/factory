// How the plugin runs this host's installed factory skill: its Rust binary.

// What runtimeCommands reads from the skill folder; the server passes node:fs, tests pass a map.
export interface SkillFiles {
  exists(path: string): boolean;
  read(path: string): string | null;
}

// The bundle.json entry, when it names a program inside the skill: no absolute path and no `..`.
function bundleEntry(text: string | null): string[] | null {
  let entry: unknown;
  try {
    entry = text === null ? null : (JSON.parse(text) as { entry?: unknown })?.entry;
  } catch {
    return null;
  }
  if (
    !Array.isArray(entry) ||
    entry.length === 0 ||
    !entry.every((part) => typeof part === "string")
  ) {
    return null;
  }
  const first = entry[0] as string;
  if (first === "" || /^[\\/]|^[A-Za-z]:/.test(first) || first.split(/[\\/]/).includes(".."))
    return null;
  return entry as string[];
}

// The command that runs the binary, then the factory command's own arguments: bundle.json's entry, else
// runtime/factory(.exe). Throws an error naming /factory catch-up when the install has no binary.
export function runtimeCommand(skill: string, platform: string, files: SkillFiles): string[] {
  const sep = platform === "win32" ? "\\" : "/";
  const at = (rel: string) => [skill, ...rel.split("/")].join(sep);
  const entry = bundleEntry(files.read(at("bundle.json")));
  const binary = platform === "win32" ? "runtime/factory.exe" : "runtime/factory";
  for (const [program, ...args] of [entry ?? [], [binary]]) {
    if (program && files.exists(at(program))) return [at(program), ...args];
  }
  throw new Error(`factory: no binary in ${skill}; run /factory catch-up to install it`);
}
