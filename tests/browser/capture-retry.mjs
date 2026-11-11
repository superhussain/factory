// Retries one capture side so a hung page under host load costs one attempt, not the run.
export const CAPTURE_ATTEMPTS = 3;

export async function retryCapture(label, run, log = console.error) {
  for (let attempt = 1; ; attempt++) {
    try {
      return await run(attempt);
    } catch (error) {
      if (attempt === CAPTURE_ATTEMPTS) throw error;
      log(
        `capture retry ${label}: attempt ${attempt} of ${CAPTURE_ATTEMPTS} failed: ${String(error?.message ?? error)}`,
      );
    }
  }
}
