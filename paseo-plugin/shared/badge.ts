export const INBOX_POLL_MS = 60_000;

export function badgeText(count: number): string {
  return count > 99 ? "99+" : String(count);
}

export function badgeLabel(count: number): string {
  return `Factory inbox, ${count} waiting`;
}
