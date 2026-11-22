// Web Push to installed dashboards: subscription checks, the board diff that picks what to
// send, RFC 8291 aes128gcm encryption and RFC 8292 VAPID signing.
import { importJWK, SignJWT } from "jose";

const TERMINAL = ["merged", "closed"];
const IN_FLIGHT = ["spec", "build", "review", "approve"];
export const STUCK_MINUTES = 240;
export const STALE_MINUTES = 1440;
const PUSH_HOSTS = [
  "fcm.googleapis.com",
  "updates.push.services.mozilla.com",
  "web.push.apple.com",
];
const PUSH_HOST_SUFFIXES = [".push.apple.com", ".notify.windows.com"];
const MAX_ENDPOINT = 1024;
const RECORD_SIZE = 4096;
const enc = new TextEncoder();

const fromBase64url = (text) =>
  Uint8Array.from(atob(text.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
const toBase64url = (bytes) =>
  btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
// Decoded bytes, or null for anything that is not base64url.
const decode = (text) => {
  if (typeof text !== "string" || !/^[\w-]*$/.test(text)) return null;
  try {
    return fromBase64url(text);
  } catch {
    return null;
  }
};
const concat = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
};

function vapidJwk(env) {
  try {
    const jwk = JSON.parse(env.FACTORY_VAPID_KEY);
    const ok =
      jwk?.kty === "EC" &&
      jwk.crv === "P-256" &&
      decode(jwk.x)?.length === 32 &&
      decode(jwk.y)?.length === 32 &&
      decode(jwk.d)?.length === 32;
    return ok ? jwk : null;
  } catch {
    return null;
  }
}

/** The VAPID public key as base64url of 0x04 || x || y, or null when push is not set up. */
export function pushKey(env) {
  const jwk = vapidJwk(env);
  return jwk && toBase64url(concat([4], decode(jwk.x), decode(jwk.y)));
}

/** The stored record for a PushSubscription.toJSON() body, or an error string. */
export function validSubscription(body) {
  const { endpoint, keys } = body ?? {};
  const p256dh = decode(keys?.p256dh);
  const auth = decode(keys?.auth);
  if (
    typeof endpoint !== "string" ||
    p256dh?.length !== 65 ||
    p256dh[0] !== 4 ||
    auth?.length !== 16
  )
    return "body needs a push subscription";
  let url;
  try {
    url = new URL(endpoint);
  } catch {
    return "body needs a push subscription";
  }
  const host = url.hostname;
  if (
    endpoint.length > MAX_ENDPOINT ||
    url.protocol !== "https:" ||
    !(PUSH_HOSTS.includes(host) || PUSH_HOST_SUFFIXES.some((s) => host.endsWith(s)))
  )
    return "push endpoint host is not allowed";
  return {
    endpoint,
    keys: { p256dh: keys.p256dh, auth: keys.auth },
    created: new Date().toISOString(),
  };
}

const asks = (s) => typeof s === "string" && s.trim() !== "";

// Ports the dashboard's waitsOnYou. Change both together.
export function waitsOnYou(j) {
  if (TERMINAL.includes(j.stage)) return false;
  return (
    j.stage === "needs-human" ||
    asks(j.needs) ||
    (j.stage === "pr-open" && (j.escalated === true || /^escalated:/i.test(String(j.note ?? ""))))
  );
}

const hasId = (j) => typeof j?.id === "string";

/** The needs and merged changes between two board job lists, in new board order. Rows without
 * a string id are skipped, as the board checks skip them. */
export function boardPushes(before, after) {
  const old = new Map(before.filter(hasId).map((j) => [j.id, j]));
  const events = [];
  for (const j of after.filter(hasId)) {
    const prev = old.get(j.id);
    if (waitsOnYou(j) && !(prev && waitsOnYou(prev)))
      events.push({ kind: "needs", id: j.id, title: j.title });
    else if (j.stage === "merged" && prev && prev.stage !== "merged")
      events.push({ kind: "merged", id: j.id, title: j.title });
  }
  return events;
}

// Paused, closing, batched into a lead, or a move its target host has not claimed.
const parked = (j) =>
  typeof j.paused?.at === "string" ||
  (typeof j.close?.at === "string" && !("closed" in j.close)) ||
  (Array.isArray(j.refs) && j.refs.some((r) => r?.kind === "batched-into")) ||
  (j.move !== null &&
    typeof j.move === "object" &&
    !Array.isArray(j.move) &&
    !("claimed" in j.move));

// Ports is_stuck in factory.py stuck. Change both together.
/** The stuck events for in-flight jobs whose claim (or, with no claim, update) went stale in
 * the hour ending STUCK_MINUTES before at, so each stale claim is pushed by one hourly run. */
export function stuckPushes(jobs, at) {
  const end = at.getTime() - STUCK_MINUTES * 60_000;
  const events = [];
  for (const j of jobs.filter(hasId)) {
    if (!IN_FLIGHT.includes(j.stage) || parked(j) || asks(j.needs)) continue;
    const seen = Date.parse(j.claim ? j.claim.seen : j.updated);
    if (seen > end - 3_600_000 && seen <= end)
      events.push({ kind: "stuck", id: j.id, title: j.title });
  }
  return events;
}

/** The stale events for jobs that wait on you and whose update falls in the hour ending
 * STALE_MINUTES before at, so each wait is pushed by one hourly run. */
export function stalePushes(jobs, at) {
  const end = at.getTime() - STALE_MINUTES * 60_000;
  const events = [];
  for (const j of jobs.filter(hasId)) {
    if (!waitsOnYou(j) || parked(j)) continue;
    const updated = Date.parse(j.updated);
    if (updated > end - 3_600_000 && updated <= end)
      events.push({ kind: "stale", id: j.id, title: j.title });
  }
  return events;
}

const LINE = {
  needs: "Needs you: ",
  merged: "Merged: ",
  stuck: "Stuck: ",
  stale: "Still needs you: ",
};
const line = (e) =>
  (e.kind === "spend" ? `Spend ${e.threshold}%: ` : LINE[e.kind]) + String(e.id).slice(0, 80);

/** The notification payload for one write's events. */
export function pushMessage(events) {
  if (events.length === 1) {
    const [e] = events;
    return {
      title: line(e),
      body: String(e.title || e.id).slice(0, 120),
      tag: `factory-${e.kind}-${e.id}`,
      url: e.kind === "spend" ? "/#/retros" : "/#" + encodeURIComponent(e.id),
    };
  }
  const lines = events.slice(0, 3).map(line);
  if (events.length > 3) lines.push(`and ${events.length - 3} more`);
  return {
    title: `${events.length} factory updates`,
    body: lines.join("\n"),
    tag: "factory-updates",
    url: "/",
  };
}

async function hkdf(salt, ikm, info, bytes) {
  const key = await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt, info },
    key,
    bytes * 8,
  );
  return new Uint8Array(bits);
}

/** RFC 8291 aes128gcm body: one record of size 4096 keyed by the server ECDH key pair. */
export async function encryptWith(uaPublic, authSecret, plaintext, serverKeys, salt) {
  const asPublic = new Uint8Array(await crypto.subtle.exportKey("raw", serverKeys.publicKey));
  const ua = await crypto.subtle.importKey(
    "raw",
    uaPublic,
    { name: "ECDH", namedCurve: "P-256" },
    false,
    [],
  );
  const ecdh = new Uint8Array(
    await crypto.subtle.deriveBits({ name: "ECDH", public: ua }, serverKeys.privateKey, 256),
  );
  const keyInfo = concat(enc.encode("WebPush: info\0"), uaPublic, asPublic);
  const ikm = await hkdf(authSecret, ecdh, keyInfo, 32);
  const cek = await hkdf(salt, ikm, enc.encode("Content-Encoding: aes128gcm\0"), 16);
  const nonce = await hkdf(salt, ikm, enc.encode("Content-Encoding: nonce\0"), 12);
  const key = await crypto.subtle.importKey("raw", cek, "AES-GCM", false, ["encrypt"]);
  const sealed = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, key, concat(plaintext, [2])),
  );
  const header = new Uint8Array(21);
  header.set(salt);
  new DataView(header.buffer).setUint32(16, RECORD_SIZE);
  header[20] = asPublic.length;
  return concat(header, asPublic, sealed);
}

/** Encrypts plaintext bytes to one stored subscription with a fresh key pair and salt. */
export async function encryptPayload(record, plaintext) {
  const serverKeys = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, [
    "deriveBits",
  ]);
  return encryptWith(
    fromBase64url(record.keys.p256dh),
    fromBase64url(record.keys.auth),
    plaintext,
    serverKeys,
    crypto.getRandomValues(new Uint8Array(16)),
  );
}

/** The RFC 8292 Authorization header value for one push endpoint. */
export async function vapidAuthorization(env, endpoint) {
  const key = await importJWK(vapidJwk(env), "ES256");
  const jwt = await new SignJWT({ sub: env.config.origin })
    .setProtectedHeader({ typ: "JWT", alg: "ES256" })
    .setAudience(new URL(endpoint).origin)
    .setExpirationTime(Math.floor(Date.now() / 1000) + 3600)
    .sign(key);
  return `vapid t=${jwt}, k=${pushKey(env)}`;
}

// One push per stored subscription. A push service that says the subscription is gone (404 or
// 410) drops its record. Every other failure is ignored, and nothing about a device is logged.
async function sendOne(env, key, body) {
  const obj = await env.BUCKET.get(key);
  if (!obj) return;
  const record = JSON.parse(await obj.text());
  const response = await fetch(record.endpoint, {
    method: "POST",
    headers: {
      TTL: "86400",
      Urgency: "high",
      "Content-Encoding": "aes128gcm",
      "Content-Type": "application/octet-stream",
      Authorization: await vapidAuthorization(env, record.endpoint),
    },
    body: await encryptPayload(record, body),
  });
  if (response.status === 404 || response.status === 410) await env.BUCKET.delete(key);
}

/** Sends the payload for events to every subscription under push/, in parallel. */
export async function sendPushes(env, events) {
  if (!events.length || !pushKey(env)) return;
  const body = enc.encode(JSON.stringify(pushMessage(events)));
  const { objects } = await env.BUCKET.list({ prefix: "push/" });
  await Promise.allSettled(objects.map((o) => sendOne(env, o.key, body)));
}
