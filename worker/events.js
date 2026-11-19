// Live board events: one Durable Object room on the WebSocket Hibernation API.
// It stores nothing. A board write's rows frame goes only to sockets whose Access
// sign-in has not expired; every other socket, and every backlog write, gets "changed".
export const EVENTS_PATH = "/events/board";
const KEYS = ["board.json", "backlog.md"];
const MAX_SOCKETS = 100;

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
      server.serializeAttachment({ exp: Number.isFinite(exp) ? exp : 0 });
      return new Response(null, { status: 101, webSocket: client });
    }
    const url = new URL(request.url);
    const key = url.searchParams.get("key");
    if (request.method !== "POST" || url.pathname !== "/notify" || !KEYS.includes(key))
      return new Response("not found", { status: 404 });
    const frame = JSON.stringify({ type: "changed", key });
    const rows = key === "board.json" ? await request.text() : "";
    for (const socket of this.ctx.getWebSockets()) {
      try {
        socket.send(rows && socket.deserializeAttachment()?.exp > Date.now() / 1000 ? rows : frame);
      } catch {}
    }
    return new Response(null, { status: 204 });
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
// A non-empty frame is the rows frame, sent to sockets whose sign-in has not expired.
export async function announce(env, key, frame = "") {
  if (!env.BoardEvents) return;
  try {
    await room(env).fetch("https://board-events.invalid/notify?key=" + encodeURIComponent(key), {
      method: "POST",
      ...(frame && { body: frame }),
    });
  } catch {}
}

// The room sees only the upgrade itself, never the browser's query or other headers,
// plus the sign-in expiry the Worker verified.
export function connectEvents(request, env, exp) {
  const headers = new Headers();
  for (const [name, value] of request.headers)
    if (name === "upgrade" || name.startsWith("sec-websocket-")) headers.set(name, value);
  if (Number.isFinite(exp)) headers.set("X-Factory-Exp", String(exp));
  return room(env).fetch(new URL(EVENTS_PATH, request.url), { headers });
}
