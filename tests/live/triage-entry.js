// Test-only entry for the live triage harness: the local test entry with env.AI wrapped, so
// each model call's latency, input tokens and yes probability come back in the
// x-triage-live response header. No deployment profile can select this file.
import local from "../support/local-entry.js";

export { BoardEvents } from "../../worker/events.js";

export default {
  async fetch(request, env) {
    let live;
    const AI = {
      async run(model, input) {
        const start = Date.now();
        try {
          const result = await env.AI.run(model, input);
          live = {
            ms: Date.now() - start,
            input_tokens: result?.usage?.input_tokens,
            noul: result?.answers?.build?.noul,
          };
          return result;
        } catch (error) {
          live = { ms: Date.now() - start, error: String(error?.message ?? error) };
          throw error;
        }
      },
    };
    const response = await local.fetch(request, { ...env, AI });
    if (!live) return response;
    const out = new Response(response.body, response);
    out.headers.set("x-triage-live", JSON.stringify(live));
    return out;
  },
};
