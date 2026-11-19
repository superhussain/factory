// Live board events: one Durable Object room on the WebSocket Hibernation API.
// It stores nothing. It sends changed frames, and applies a person's board patch frames
// with the same core as PATCH /api/board.
import { applyBoardPatch, BOARD_PATCH_SHAPE } from "./index.js";

export const EVENTS_PATH = "/events/board";
const KEYS = ["board.json", "backlog.md"];
const MAX_SOCKETS = 100;
const MAX_FRAME = 1 << 20;

export class BoardEvents {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    // Keepalive pings are answered by the runtime, so the room stays hibernated.
    this.ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  fetch(request) {
    if (request.headers.get("Upgrade")?.toLowerCase() === "websocket") {
      if (this.ctx.getWebSockets().length >= MAX_SOCKETS)
        return new Response("room full", { status: 503 });
      const [client, server] = Object.values(new WebSocketPair());
      // Access checks only this upgrade, so the socket keeps its sign-in expiry and origin
      // through hibernation and refuses writes after that expiry.
      const exp = Number(request.headers.get("X-Factory-Access-Exp"));
      server.serializeAttachment({
        exp: Number.isFinite(exp) ? exp : 0,
        origin: new URL(request.url).origin,
      });
      this.ctx.acceptWebSocket(server);
      return new Response(null, { status: 101, webSocket: client });
    }
    const url = new URL(request.url);
    const key = url.searchParams.get("key");
    if (request.method !== "POST" || url.pathname !== "/notify" || !KEYS.includes(key))
      return new Response("not found", { status: 404 });
    this.broadcast(key);
    return new Response(null, { status: 204 });
  }

  broadcast(key) {
    const frame = JSON.stringify({ type: "changed", key });
    for (const socket of this.ctx.getWebSockets()) {
      try {
        socket.send(frame);
      } catch {}
    }
  }

  // A patch frame, {type: "patch", op, updated, rows, reads}, is the PATCH /api/board body
  // on the open socket. It gets an ack or a typed error with its op, and a commit tells
  // every socket, as /notify does.
  async webSocketMessage(socket, message) {
    const send = (frame) => {
      try {
        socket.send(JSON.stringify(frame));
      } catch {}
    };
    const refuse = (op, status, text) =>
      send({ type: "error", op, error: "refused", status, message: text });
    if (typeof message !== "string" || new TextEncoder().encode(message).length > MAX_FRAME)
      return refuse(null, 413, "body over 1 MiB");
    let frame;
    try {
      frame = JSON.parse(message);
    } catch {
      return refuse(null, 400, "body is not JSON");
    }
    const op =
      typeof frame?.op === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(frame.op) ? frame.op : null;
    if (frame?.type !== "patch" || !op) return refuse(op, 400, BOARD_PATCH_SHAPE);
    const { exp, origin } = socket.deserializeAttachment() ?? {};
    if (!(exp * 1000 > Date.now())) return send({ type: "error", op, error: "expired" });
    const result = await applyBoardPatch(
      { ...this.env, config: { origin } },
      { updated: frame.updated, rows: frame.rows, reads: frame.reads },
      this.ctx,
    );
    if (result.ok) {
      send({ type: "ack", op, etag: result.etag });
      this.broadcast("board.json");
    } else if (result.conflict) send({ type: "error", op, error: "conflict", ...result.conflict });
    else if (result.busy) send({ type: "error", op, error: "busy" });
    else refuse(op, result.status, result.text);
  }

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
    await room(env).fetch("https://board-events.invalid/notify?key=" + encodeURIComponent(key), {
      method: "POST",
    });
  } catch {}
}

// The room sees only the upgrade itself, never the browser's query or other headers, plus
// the verified sign-in's expiry in seconds, which only the Worker sets.
export function connectEvents(request, env, exp) {
  const headers = new Headers();
  for (const [name, value] of request.headers)
    if (name === "upgrade" || name.startsWith("sec-websocket-")) headers.set(name, value);
  if (Number.isFinite(exp)) headers.set("X-Factory-Access-Exp", String(exp));
  return room(env).fetch(new URL(EVENTS_PATH, request.url), { headers });
}
