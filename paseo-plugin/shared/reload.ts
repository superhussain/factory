// Lets the Factory row ask the open screen to reload its frame.
export function createReload() {
  const listeners = new Set<() => void>();
  return {
    fire() {
      for (const fn of listeners) fn();
    },
    listen(fn: () => void) {
      listeners.add(fn);
      return () => void listeners.delete(fn);
    },
  };
}

export type Reload = ReturnType<typeof createReload>;
