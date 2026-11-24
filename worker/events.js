// Live board events: one Durable Object room on the WebSocket Hibernation API, the class
// BoardEvents in room.js. This module holds what the Worker uses to reach it. A board write's
// rows frame goes to bound host sockets and to sockets whose Access sign-in has not expired;
// every other socket, and every backlog write, gets "changed". A host seat names its service
// token. Retiring or re-pairing a host closes its seats at once, and the room re-checks the
// bound tokens, at most BOUND_TTL_MS old, before it sends a host rows, closing any seat whose
// token is no longer bound.
import { ROOM } from "./board-store.js";

export const EVENTS_PATH = "/events/board";
export const HOST_EVENTS_PATH = "/events/host";
export const KEYS = ["board.json", "backlog.md"];
export const MAX_SOCKETS = 100;
export const BOUND_TTL_MS = 60_000;
// The close code a host seat gets once its token is no longer bound.
export const UNBOUND = 4003;

const room = (env) => env.BoardEvents.get(env.BoardEvents.idFromName(ROOM));

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
