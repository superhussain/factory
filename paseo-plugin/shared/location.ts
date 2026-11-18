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

// A job from a notification wins over the saved page.
export function startUrl(origin: string, job: string | undefined, saved: string): string {
  return job && JOB_ID.test(job) ? dashboardSrc(origin, job) : frameUrl(origin, saved);
}
