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

// Paseo web serves from http(s); the desktop app uses its own scheme.
export function isBrowserOrigin(origin: string | null): boolean {
  return origin !== null && /^https?:\/\//.test(origin);
}

const SIGN_IN_STEPS = [
  "Enter your email, then the code Cloudflare sends you.",
  "That window closes once your board shows, and this page reloads. If it stays open, close it.",
];

export function firstLoginSteps(browser: boolean): string[] {
  return browser
    ? [
        "Using Safari, or another browser that blocks third-party cookies? Select Open in a browser tab and sign in there. You can skip the rest.",
        "Otherwise select Sign in. A small Paseo window opens on the Cloudflare Access page.",
        ...SIGN_IN_STEPS,
      ]
    : [
        "Select Sign in. A small Paseo window opens on the Cloudflare Access page.",
        ...SIGN_IN_STEPS,
      ];
}

export function openLabel(browser: boolean): string {
  return browser ? "Open in a browser tab" : "Open in your browser";
}
