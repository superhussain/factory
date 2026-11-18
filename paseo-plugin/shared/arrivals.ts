// Which waiting jobs are new since the last snapshot, and the copy that announces them.

export type WaitingJob = { id: string; project: string; title: string };

export const MAX_ALERTS = 3;

export function arrivals(
  seen: ReadonlySet<string> | null,
  jobs: readonly WaitingJob[],
): WaitingJob[] {
  return seen ? jobs.filter((job) => !seen.has(job.id)) : [];
}

export function needsTitle(job: WaitingJob): string {
  return "Needs you: " + job.id.slice(0, 80);
}

export function needsBody(job: WaitingJob): string {
  return (job.title || job.id).slice(0, 120);
}

// Keeps the latest inbox count outside any component, so a remounted sidebar row shows it at once.
// The first snapshot only records what is already waiting; later ones alert on new ids.
export function createInbox(onArrive: (job: WaitingJob) => void) {
  let count = 0;
  let seen: Set<string> | null = null;
  const listeners = new Set<() => void>();
  return {
    apply(event: { count: number; jobs: WaitingJob[] }) {
      const fresh = arrivals(seen, event.jobs);
      seen = new Set(event.jobs.map((job) => job.id));
      count = event.count;
      fresh.slice(0, MAX_ALERTS).forEach(onArrive);
      listeners.forEach((listener) => listener());
    },
    get: () => count,
    listen(listener: () => void) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

export type Inbox = ReturnType<typeof createInbox>;
