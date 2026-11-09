// Live board events: one Durable Object room on the WebSocket Hibernation API.
// It stores nothing and never carries board content.
export const EVENTS_PATH = "/events/board";
const KEYS = ["board.json", "backlog.md"];

export class BoardEvents {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
  }

  fetch(request) {
    if (request.headers.get("Upgrade")?.toLowerCase() === "websocket") {
      const [client, server] = Object.values(new WebSocketPair());
      this.ctx.acceptWebSocket(server);
      return new Response(null, { status: 101, webSocket: client });
    }
    const url = new URL(request.url);
    const key = url.searchParams.get("key");
    if (request.method !== "POST" || url.pathname !== "/notify" || !KEYS.includes(key))
      return new Response("not found", { status: 404 });
    const frame = JSON.stringify({ type: "changed", key });
    for (const socket of this.ctx.getWebSockets()) {
      try {
        socket.send(frame);
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
export async function announce(env, key) {
  if (!env.BoardEvents) return;
  try {
    await room(env).fetch("https://board-events.invalid/notify?key=" + key, { method: "POST" });
  } catch {}
}

// The room sees only the upgrade itself, never the browser's query or other headers.
export function connectEvents(request, env) {
  const headers = new Headers();
  for (const [name, value] of request.headers)
    if (name === "upgrade" || name.startsWith("sec-websocket-")) headers.set(name, value);
  return room(env).fetch(new URL(EVENTS_PATH, request.url), { headers });
}
