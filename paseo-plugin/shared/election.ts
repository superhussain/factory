// The election check loop: read the dashboard origin, retry once on a miss, poll and recheck on focus.
import type { Member } from "./ballot.ts";

export const ELECTION_POLL_MS = 30_000;
export const ELECTION_TIMEOUT_MS = 5_000;

export type ElectionSettings = { origin: string | null; factoryLinkButtons: boolean };

export function runElection(
  member: Member,
  readOrigin: () => Promise<ElectionSettings | null>,
  watchFocus: (listener: () => void) => () => void,
  onSettings: (origin: string | null, factoryLinkButtons: boolean) => void,
) {
  let started = 0;
  let settled = 0;
  const check = async () => {
    const seq = ++started;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let report: () => void;
    try {
      const settings = await Promise.race([
        readOrigin(),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("timeout")), ELECTION_TIMEOUT_MS);
        }),
      ]);
      const origin = settings ? settings.origin : null;
      report = () => {
        member.online(origin);
        onSettings(origin, settings ? settings.factoryLinkButtons : false);
      };
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
