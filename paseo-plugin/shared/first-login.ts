export const SEEN_KEY = "factory-plugin:loaded:";

export type SeenStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

export type WebGlobals = { storage(): SeenStorage; origin(): string };

// True once the board has loaded on this device for this origin, so the screen can skip onboarding.
export function hasLoadedBefore(web: WebGlobals | null, origin: string): boolean {
  if (!web) return false;
  try {
    return web.storage().getItem(SEEN_KEY + origin) === "1";
  } catch {
    return false;
  }
}

export function markLoaded(web: WebGlobals | null, origin: string): void {
  if (!web) return;
  try {
    web.storage().setItem(SEEN_KEY + origin, "1");
  } catch {}
}

export function paseoOrigin(web: WebGlobals | null): string | null {
  return web ? web.origin() : null;
}
