import { useSyncExternalStore } from "react";

// The Inbox count the framed dashboard last posted, or null while no board is showing.
let frameCount: number | null = null;
const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function setFrameInboxCount(count: number | null): void {
  if (count === frameCount) return;
  frameCount = count;
  for (const listener of listeners) listener();
}

export function useFrameInboxCount(): number | null {
  return useSyncExternalStore(
    subscribe,
    () => frameCount,
    () => null,
  );
}
