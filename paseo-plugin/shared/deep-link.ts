// Dashboard links found in agent replies, and the elected Factory screen each origin opens them on.
// No URL class: this also runs under React Native.
import { EMBED_HASH } from "./embed.ts";

export type DashboardLink = { url: string; origin: string; hash: string; label: string };

export const MAX_DASHBOARD_LINKS = 5;
const TOKEN = /https:\/\/[^\s<>"`\[\]{}|\\^]+/gi;
const LINK = /^https:\/\/([^\/?#@]+)\/?(#.*)?$/;
const TRAILING = ".,;:!?*'";

function count(text: string, char: string): number {
  return text.split(char).length - 1;
}

function trim(token: string): string {
  let end = token.length;
  while (end > 0) {
    const last = token[end - 1];
    const head = token.slice(0, end);
    if (TRAILING.includes(last) || (last === ")" && count(head, ")") > count(head, "("))) end--;
    else break;
  }
  return token.slice(0, end);
}

export function findDashboardLinks(text: string, origin: string): DashboardLink[] {
  const host = origin.slice("https://".length);
  const links: DashboardLink[] = [];
  for (const [token] of text.matchAll(TOKEN)) {
    const match = LINK.exec(trim(token));
    if (!match || match[1].toLowerCase() !== host) continue;
    const hash = match[2] ?? "";
    if (!EMBED_HASH.test(hash)) continue;
    const url = origin + "/" + hash;
    if (links.some((link) => link.url === url)) continue;
    links.push({ url, origin, hash, label: hash ? hash.slice(1) : "the board" });
    if (links.length === MAX_DASHBOARD_LINKS) break;
  }
  return links;
}

export const OPENERS_KEY = "factory-plugin:openers:v1";

export type ScreenOpener = (hash: string) => void;

export function screenOpeners(scope: object): Map<string, ScreenOpener> {
  const store = scope as Record<symbol, Map<string, ScreenOpener> | undefined>;
  return (store[Symbol.for(OPENERS_KEY)] ??= new Map());
}

export function setScreenOpener(scope: object, origin: string, open: ScreenOpener): () => void {
  const openers = screenOpeners(scope);
  openers.set(origin, open);
  return () => {
    if (openers.get(origin) === open) openers.delete(origin);
  };
}

// Registers for the origin only while the switch is on, and swaps on an origin change.
export function createLinkSwitch(add: (origin: string) => () => void) {
  let registered: string | null = null;
  let remove: (() => void) | null = null;
  const update = (origin: string | null, enabled: boolean) => {
    const want = enabled ? origin : null;
    if (want === registered) return;
    remove?.();
    remove = null;
    registered = want;
    if (want) remove = add(want);
  };
  return { update, stop: () => update(null, false) };
}
