// Test-only entry for the local protocol/HTTP suites: serveFactory with a synthetic
// principal and generic config. Production auth lives only in worker/index.js, which
// never imports this file, and no deployment profile can select it.
// The links are reserved synthetic templates exercising the configured-link paths;
// the workerd auth wrapper (auth-wrangler.json) covers the omitted-policy path.
// The synthetic principal is a person, so pipeline saves work locally.
import { serveFactory } from "../../worker/index.js";

export { BoardEvents } from "../../worker/events.js";

const config = {
  origin: "https://factory.example.test",
  title: "Factory",
  issuer: "https://example-team.cloudflareaccess.com",
  audience: "example-application-audience",
  image_origins: [],
  jira_origin: "https://jira.example.test",
  agent_links: {
    app: { label: "Session app", template: "session://h/{server}/agent/{agent}" },
    web: { label: "Session", template: "https://sessions.example.test/{server}/{agent}" },
  },
};

export default {
  fetch: (request, env, ctx) =>
    serveFactory(
      request,
      { ...env, config },
      { verified: true, kind: "user", email: "operator@example.test" },
      ctx,
    ),
};
