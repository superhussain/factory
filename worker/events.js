// Live board events: one Durable Object room on the WebSocket Hibernation API.
// It stores nothing. A board write's rows frame goes to bound host sockets and to sockets
// whose Access sign-in has not expired; every other socket, and every backlog write, gets
// "changed". A host seat names its service token. Retiring or re-pairing a host closes its
// seats at once, and the room re-checks the bound tokens, at most BOUND_TTL_MS old, before it
// sends a host rows, closing any seat whose token is no longer bound.
export const EVENTS_PATH = "/events/board";
export const HOST_EVENTS_PATH = "/events/host";
const KEYS = ["board.json", "backlog.md"];
const MAX_SOCKETS = 100;
const BOUND_TTL_MS = 60_000;
// The close code a host seat gets once its token is no longer bound.
export const UNBOUND = 4003;

export class BoardEvents {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    // Keepalive pings are answered by the runtime, so the room stays hibernated.
    this.ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async fetch(request) {
    if (request.headers.get("Upgrade")?.toLowerCase() === "websocket") {
      if (this.ctx.getWebSockets().length >= MAX_SOCKETS)
        return new Response("room full", { status: 503 });
      const [client, server] = Object.values(new WebSocketPair());
      this.ctx.acceptWebSocket(server);
      const exp = Number(request.headers.get("X-Factory-Exp"));
      const token = request.headers.get("X-Factory-Host");
      server.serializeAttachment(
        token ? { host: true, token } : { exp: Number.isFinite(exp) ? exp : 0 },
      );
      return new Response(null, { status: 101, webSocket: client });
    }
    const url = new URL(request.url);
    if (request.method === "POST" && url.pathname === "/revoke") {
      this.boundTokens = null;
      this.closeSeats((token) => token === url.searchParams.get("token"));
      return new Response(null, { status: 204 });
    }
    const key = url.searchParams.get("key");
    if (request.method !== "POST" || url.pathname !== "/notify" || !KEYS.includes(key))
      return new Response("not found", { status: 404 });
    const frame = JSON.stringify({ type: "changed", key });
    const rows = key === "board.json" ? await request.text() : "";
    const bound = rows ? await this.bound() : null;
    if (bound) this.closeSeats((token) => !bound.has(token));
    for (const socket of this.ctx.getWebSockets()) {
      try {
        const seat = socket.deserializeAttachment();
        if (seat?.host && !bound?.has(seat.token)) {
          if (!rows) socket.send(frame);
          continue;
        }
        socket.send(rows && (seat?.host || seat?.exp > Date.now() / 1000) ? rows : frame);
      } catch {}
    }
    return new Response(null, { status: 204 });
  }

  // The service tokens of paired or active hosts, read from R2 at most every BOUND_TTL_MS.
  async bound() {
    if (this.boundTokens && Date.now() - this.boundTokens.at < BOUND_TTL_MS)
      return this.boundTokens.tokens;
    const tokens = new Set();
    for (const { key } of (await this.env.BUCKET.list({ prefix: "hosts/" })).objects) {
      try {
        const host = JSON.parse(await (await this.env.BUCKET.get(key)).text());
        if (host.token && (host.state === "paired" || host.state === "active"))
          tokens.add(host.token);
      } catch {}
    }
    this.boundTokens = { at: Date.now(), tokens };
    return tokens;
  }

  // Closes every host seat whose token `gone` picks.
  closeSeats(gone) {
    for (const socket of this.ctx.getWebSockets()) {
      try {
        const seat = socket.deserializeAttachment();
        if (seat?.host && gone(seat.token)) socket.close(UNBOUND, "host no longer bound");
      } catch {}
    }
  }

  webSocketMessage() {}

  webSocketClose(socket) {
    try {
      socket.close(1000);
    } catch {}
  }
}

const room = (env) => env.BoardEvents.get(env.BoardEvents.idFromName("main"));

// The write is already committed, so a failed announcement only delays the fallback poll.
// A non-empty frame is the rows frame, sent to host sockets and sockets whose sign-in has
// not expired.
export async function announce(env, key, frame = "") {
  if (!env.BoardEvents) return;
  try {
    await room(env).fetch("https://board-events.invalid/notify?key=" + encodeURIComponent(key), {
      method: "POST",
      ...(frame && { body: frame }),
    });
  } catch {}
}

// Closes the host seats of a service token that is no longer bound, such as on retire.
export async function revokeSeats(env, token) {
  if (!env.BoardEvents || !token) return;
  try {
    await room(env).fetch(
      "https://board-events.invalid/revoke?token=" + encodeURIComponent(token),
      { method: "POST" },
    );
  } catch {}
}

// The room sees only the upgrade itself, never the browser's query or other headers,
// plus the sign-in expiry the Worker verified, or the token of the bound host it admitted.
export function connectEvents(request, env, exp, hostToken = "") {
  const headers = new Headers();
  for (const [name, value] of request.headers)
    if (name === "upgrade" || name.startsWith("sec-websocket-")) headers.set(name, value);
  if (Number.isFinite(exp)) headers.set("X-Factory-Exp", String(exp));
  if (hostToken) headers.set("X-Factory-Host", hostToken);
  return room(env).fetch(new URL(EVENTS_PATH, request.url), { headers });
}
