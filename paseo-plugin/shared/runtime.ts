// How the plugin runs this host's installed factory skill: its Rust binary, or the Python runtime when an
// older install has no binary.

// What runtimeCommands reads from the skill folder; the server passes node:fs, tests pass a map.
export interface SkillFiles {
  exists(path: string): boolean;
  read(path: string): string | null;
}

// On Windows `python3` is often the Microsoft Store stub, so the `py` launcher is the fallback.
// `.exe` names make execCommand spawn directly instead of through cmd.exe, which reports a
// missing command as exit 1.
export function pythonCommands(platform: string): string[][] {
  return platform === "win32" ? [["python3.exe"], ["py.exe", "-3"]] : [["python3"]];
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

// The commands to try in order, each then given the factory command's own arguments: the binary from
// bundle.json's entry or runtime/factory(.exe) when it exists, else Python on runtime/factory.py.
export function runtimeCommands(skill: string, platform: string, files: SkillFiles): string[][] {
  const sep = platform === "win32" ? "\\" : "/";
  const at = (rel: string) => [skill, ...rel.split("/")].join(sep);
  const entry = bundleEntry(files.read(at("bundle.json")));
  const binary = platform === "win32" ? "runtime/factory.exe" : "runtime/factory";
  for (const [program, ...args] of [entry ?? [], [binary]]) {
    if (program && files.exists(at(program))) return [[at(program), ...args]];
  }
  return pythonCommands(platform).map((command) => [...command, at("runtime/factory.py")]);
}

// Measured on Windows: a missing `.exe` throws code "ENOENT"; the Store stub exits 9009.
// Any other failure is a real run and is not retried.
export function isStartFailure(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const code = (error as { code?: unknown }).code;
  return code === "ENOENT" || code === 9009;
}
