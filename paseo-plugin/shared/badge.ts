export const INBOX_POLL_MS = 60_000;

export function badgeText(count: number): string {
  return count > 99 ? "99+" : String(count);
}

export function badgeLabel(count: number): string {
  return `Factory inbox, ${count} waiting`;
}

// The framed dashboard's count while the Factory screen shows it, else the daemon's.
export function shownInboxCount(daemon: number, frame: number | null, open: boolean): number {
  return open && frame !== null ? frame : daemon;
}
