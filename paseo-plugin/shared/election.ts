// The election check loop: read the dashboard origin, retry once on a miss, poll and recheck on focus.
import type { Member } from "./ballot.ts";

export const ELECTION_POLL_MS = 30_000;
export const ELECTION_TIMEOUT_MS = 5_000;

export function runElection(
  member: Member,
  readOrigin: () => Promise<string | null>,
  watchFocus: (listener: () => void) => () => void,
) {
  let started = 0;
  let settled = 0;
  const check = async () => {
    const seq = ++started;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let report: () => void;
    try {
      const origin = await Promise.race([
        readOrigin(),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("timeout")), ELECTION_TIMEOUT_MS);
        }),
      ]);
      report = () => member.online(origin);
    } catch {
      report = () => {
        if (member.miss()) void check();
      };
    } finally {
      clearTimeout(timer);
    }
    // A slower, older check must not override a newer result.
    if (seq < settled) return;
    settled = seq;
    report();
  };
  void check();
  const poll = setInterval(() => void check(), ELECTION_POLL_MS);
  const stopFocus = watchFocus(() => void check());
  return {
    recheck: () => void check(),
    stop() {
      clearInterval(poll);
      stopFocus();
      member.leave();
    },
  };
}
