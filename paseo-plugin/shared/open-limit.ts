// Caps how often the framed dashboard can open links in the system browser.
export const OPEN_LIMIT = 3;
export const OPEN_WINDOW_MS = 1000;

export function createOpenLimiter(now: () => number = Date.now): () => boolean {
  let times: number[] = [];
  return () => {
    const t = now();
    times = times.filter((time) => time > t - OPEN_WINDOW_MS);
    if (times.length >= OPEN_LIMIT) return false;
    times.push(t);
    return true;
  };
}
