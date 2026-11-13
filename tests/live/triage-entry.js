// Test-only entry for the live triage harness: the local test entry with env.AI wrapped, so
// the last model call's latency, input tokens and yes probability come back in the
// x-triage-live response header, and every call's latency and tokens in x-triage-calls. No deployment profile can select this file.
import local from "../support/local-entry.js";

export { BoardEvents } from "../../worker/events.js";

export default {
  async fetch(request, env) {
    let live;
    const calls = [];
    const AI = {
      async run(model, input) {
        const start = Date.now();
        try {
          const result = await env.AI.run(model, input);
          const ms = Date.now() - start;
          calls.push({
            ms,
            input_tokens: result?.usage?.input_tokens,
            output_tokens: result?.usage?.output_tokens,
          });
          live = {
            ms,
            input_tokens: result?.usage?.input_tokens,
            noul: result?.answers?.build?.noul,
          };
          return result;
        } catch (error) {
          live = { ms: Date.now() - start, error: String(error?.message ?? error) };
          calls.push(live);
          throw error;
        }
      },
    };
    const response = await local.fetch(request, { ...env, AI });
    if (!live) return response;
    const out = new Response(response.body, response);
    out.headers.set("x-triage-live", JSON.stringify(live));
    if (calls.length) out.headers.set("x-triage-calls", JSON.stringify(calls));
    return out;
  },
};
