import { dashboardSrc, EMBED_HASH, JOB_ID } from "./embed.ts";
import type { SeenStorage } from "./first-login.ts";

export const LOCATION_KEY = "factory-plugin:location:";

// The dashboard page last reported for this origin, or "" when none is kept.
export function readLocation(storage: (() => SeenStorage) | null, origin: string): string {
  if (!storage) return "";
  try {
    const hash = storage().getItem(LOCATION_KEY + origin);
    return hash !== null && EMBED_HASH.test(hash) ? hash : "";
  } catch {
    return "";
  }
}

export function writeLocation(
  storage: (() => SeenStorage) | null,
  origin: string,
  hash: string,
): void {
  if (!storage || !EMBED_HASH.test(hash)) return;
  try {
    storage().setItem(LOCATION_KEY + origin, hash);
  } catch {}
}

export function frameUrl(origin: string, hash: string): string {
  return origin + "/" + (EMBED_HASH.test(hash) ? hash : "");
}

// A job from a notification wins, then a page from a reply's button, then the saved page.
export function startUrl(
  origin: string,
  job: string | undefined,
  saved: string,
  hash?: string,
): string {
  return jumpUrl(origin, job, hash) ?? frameUrl(origin, saved);
}

export function jumpUrl(
  origin: string,
  job: string | undefined,
  hash: string | undefined,
): string | null {
  if (job && JOB_ID.test(job)) return dashboardSrc(origin, job);
  return hash !== undefined && EMBED_HASH.test(hash) ? origin + "/" + hash : null;
}
