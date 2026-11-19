// Test-only entry for the local protocol/HTTP suites: serveFactory with a synthetic
// principal and generic config. Production auth lives only in worker/index.js, which
// never imports this file, and no deployment profile can select it.
// The links are reserved synthetic templates exercising the configured-link paths;
// the workerd auth wrapper (auth-wrangler.json) covers the omitted-policy path.
// The synthetic principal is a person, so pipeline saves work locally.
// A drill request may send X-Factory-Test-Read-Delay to put a cancellable timer before its R2 get.
import { serveFactory } from "../../worker/index.js";

export { BoardEvents } from "../../worker/events.js";

const READ_DELAY = "X-Factory-Test-Read-Delay";

const config = {
  origin: "https://factory.example.test",
  title: "Factory",
  issuer: "https://example-team.cloudflareaccess.com",
  audience: "example-application-audience",
  image_origins: [],
  embed_origins: ["https://paseo.example.test", "paseo://app"],
  jira_origin: "https://jira.example.test",
  agent_links: {
    app: { label: "Session app", template: "session://h/{server}/agent/{agent}" },
    web: { label: "Session", template: "https://sessions.example.test/{server}/{agent}" },
  },
};

function delayedBucket(bucket, ms) {
  return new Proxy(bucket, {
    get(target, prop) {
      if (prop === "get")
        return async (...args) => {
          await new Promise((r) => setTimeout(r, ms));
          return target.get(...args);
        };
      const value = Reflect.get(target, prop);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}

function testEnv(request, env) {
  const ms = Number(request.headers.get(READ_DELAY));
  return Number.isInteger(ms) && ms >= 1 && ms <= 1000
    ? { ...env, BUCKET: delayedBucket(env.BUCKET, ms) }
    : env;
}

export default {
  fetch: (request, env, ctx) =>
    serveFactory(
      request,
      { ...testEnv(request, env), config },
      { verified: true, kind: "user", email: "operator@example.test", exp: 4102444800 },
      ctx,
    ),
};
