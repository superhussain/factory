// Test-only entry for the host events check: serveFactory with a synthetic device
// principal. Production auth lives only in worker/index.js, which never imports this
// file, and no deployment profile can select it.
import { serveFactory } from "../../worker/index.js";

export { BoardEvents } from "../../worker/events.js";

const config = {
  origin: "https://factory.example.test",
  title: "Factory",
  issuer: "https://example-team.cloudflareaccess.com",
  audience: "example-application-audience",
  image_origins: [],
  embed_origins: [],
};

export default {
  fetch: (request, env, ctx) =>
    serveFactory(
      request,
      { ...env, config },
      { verified: true, kind: "service", client_id: "client-aaaa" },
      ctx,
    ),
};
