// The board room: one Durable Object on the WebSocket Hibernation API that sends board events
// to open sockets, as events.js describes, and holds the board's rows in its SQLite through
// BoardRows in board-store.js. It extends DurableObject so the Worker reaches the store over RPC.
// Only workerd has cloudflare:workers, so modules Node tests import directly stay out of here.
import { DurableObject } from "cloudflare:workers";
import { BoardRows } from "./board-store.js";
import { BOUND_TTL_MS, KEYS, MAX_SOCKETS, UNBOUND } from "./events.js";

export class BoardEvents extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
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
    // The whole board as a body, so the Worker passes its bytes through without decoding them.
    if (request.method === "GET" && url.pathname === "/board") {
      const board = await this.board().body();
      return board
        ? new Response(board.body, { headers: { ETag: board.etag } })
        : new Response(null, { status: 404 });
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

  // The board store's RPC methods, which boardStore(env) calls.
  board() {
    return (this.store ??= new BoardRows(this.ctx.storage, this.env, (promise) =>
      this.ctx.waitUntil(promise),
    ));
  }

  boardHead() {
    return this.board().head();
  }

  boardRead() {
    return this.board().read();
  }

  boardRows(q, at) {
    return this.board().rows(q, at);
  }

  boardCommit(c) {
    return this.board().commit(c);
  }

  boardReplace(text, cond) {
    return this.board().replace(text, cond);
  }

  // A row commit's copy to R2, FLUSH_MS after it.
  alarm() {
    return this.board().flush();
  }

  webSocketMessage() {}

  webSocketClose(socket) {
    try {
      socket.close(1000);
    } catch {}
  }
}
