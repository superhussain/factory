// On Windows `python3` is often the Microsoft Store stub, so the `py` launcher is the fallback.
// `.exe` names make execCommand spawn directly instead of through cmd.exe, which reports a
// missing command as exit 1.
export function pythonCommands(platform: string): string[][] {
  return platform === "win32" ? [["python3.exe"], ["py.exe", "-3"]] : [["python3"]];
}

// Measured on Windows: a missing `.exe` throws code "ENOENT"; the Store stub exits 9009.
// Any other failure is a real run and is not retried.
export function isStartFailure(error: unknown): boolean {
  if (typeof error !== "object" || error === null) return false;
  const code = (error as { code?: unknown }).code;
  return code === "ENOENT" || code === 9009;
}
