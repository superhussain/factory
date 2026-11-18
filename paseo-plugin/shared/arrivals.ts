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

export function needsCount(n: number): string {
  return n === 1 ? "1 job needs you" : `${n} jobs need you`;
}
