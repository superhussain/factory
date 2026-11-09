// Picks one CI shard of the browser cases. FACTORY_BROWSER_SHARD="<n>/<total>" keeps
// every total-th CJS suite and node file, starting at position n - 1. Unset runs all.
export function selectShard(cases, value) {
  if (value === undefined || value === "") return cases;
  const match = /^([1-9][0-9]*)\/([1-9][0-9]*)$/.exec(value);
  const index = match ? Number(match[1]) : 0;
  const total = match ? Number(match[2]) : 0;
  if (!match || index > total) {
    throw new Error(
      `FACTORY_BROWSER_SHARD must be <n>/<total> with 1 <= n <= total, got ${JSON.stringify(value)}`,
    );
  }
  const keep = (_, position) => position % total === index - 1;
  return { ...cases, suites: cases.suites.filter(keep), nodeTests: cases.nodeTests.filter(keep) };
}
