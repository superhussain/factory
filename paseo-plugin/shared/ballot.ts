// Picks one installation per dashboard origin to own the Factory screen and row.
// All installations in one Paseo window share the ballot through globalThis.

export const BALLOT_KEY = "factory-plugin:ballot:v1";

export type Seat = { origin: string | null; online: boolean; known: boolean };
export type Ballot = { seats: Seat[]; members: Set<() => void> };
export type Member = {
  online(origin: string | null): void;
  offline(): void;
  elected(): boolean;
  leave(): void;
};

export function sharedBallot(scope: object): Ballot {
  const store = scope as Record<symbol, Ballot | undefined>;
  return (store[Symbol.for(BALLOT_KEY)] ??= { seats: [], members: new Set() });
}

export function isElected(seats: readonly Seat[], seat: Seat): boolean {
  if (!seat.known) return false;
  const group = seats.filter((s) => s.known && s.origin === seat.origin);
  return (group.find((s) => s.online) ?? group[0]) === seat;
}

export function joinBallot(
  ballot: Ballot,
  register: (origin: string | null) => () => void,
): Member {
  const seat: Seat = { origin: null, online: false, known: false };
  let unregister: (() => void) | null = null;
  let registered: string | null = null;
  let left = false;
  const sync = () => {
    const win = isElected(ballot.seats, seat);
    if (unregister && (!win || registered !== seat.origin)) {
      unregister();
      unregister = null;
    }
    if (win && !unregister) {
      registered = seat.origin;
      unregister = register(seat.origin);
    }
  };
  const notify = () => ballot.members.forEach((fn) => fn());
  ballot.seats.push(seat);
  ballot.members.add(sync);
  return {
    online(origin) {
      if (left) return;
      seat.origin = origin;
      seat.online = true;
      seat.known = true;
      notify();
    },
    offline() {
      if (left || !seat.known) return;
      seat.online = false;
      notify();
    },
    elected: () => !left && isElected(ballot.seats, seat),
    leave() {
      if (left) return;
      left = true;
      ballot.seats.splice(ballot.seats.indexOf(seat), 1);
      ballot.members.delete(sync);
      unregister?.();
      unregister = null;
      notify();
    },
  };
}
