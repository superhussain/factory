// The host registry: one person-written record and one device-written status per host.
// Pairing codes are one-time and short-lived; only their hash is ever stored.
import { checkOperatorText, HOST_NAME } from "./admission.js";

export const OFFLINE_MINUTES = 30;
const CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const MOVES = { paired: ["active", "retired"], pending: ["retired"], active: ["retired"] };
const SCHEDULERS = ["launchd", "cron", "schtasks", "none"];
const STATUS_KEYS = ["os", "scheduler", "last_pickup", "bundle", "doctor", "routes", "checkouts"];
const ROUTE_KEYS = ["name", "provider", "model", "effort", "limited_until"];
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z$/;
const SHA = /^[0-9a-f]{7,64}$/;
const MODEL = /^[A-Za-z0-9._:@/[\]-]{1,128}$/;

const isObject = (value) => typeof value === "object" && value !== null && !Array.isArray(value);
const unknownKey = (body, keys) => Object.keys(body).find((key) => !keys.includes(key));
const isIso = (value) => typeof value === "string" && ISO.test(value) && !isNaN(Date.parse(value));

function labelError(label) {
  if (typeof label !== "string" || label.length < 1 || [...label].length > 80)
    return "label: must be 1 to 80 characters";
  return checkOperatorText(label, "label");
}

const maxInflightError = (value) =>
  Number.isInteger(value) && value >= 0 && value <= 5
    ? null
    : "max_inflight: must be a whole number from 0 to 5";

/** The first error in a POST /api/hosts body, or null. */
export function validateHostCreate(body) {
  if (!isObject(body)) return "body: must be a JSON object";
  const extra = unknownKey(body, ["id", "label", "max_inflight"]);
  if (extra !== undefined) return `${extra}: unknown field`;
  if (typeof body.id !== "string" || !HOST_NAME.test(body.id))
    return "id: must be a host name of letters, digits, dot, dash and underscore";
  return (
    labelError(body.label) ?? ("max_inflight" in body ? maxInflightError(body.max_inflight) : null)
  );
}

/** The first error in a PATCH body against the stored record, or null. */
export function validateHostPatch(record, patch) {
  if (!isObject(patch)) return "body: must be a JSON object";
  const extra = unknownKey(patch, ["label", "max_inflight", "state"]);
  if (extra !== undefined) return `${extra}: unknown field`;
  if ("label" in patch) {
    const error = labelError(patch.label);
    if (error) return error;
  }
  if ("max_inflight" in patch) {
    const error = maxInflightError(patch.max_inflight);
    if (error) return error;
  }
  if ("state" in patch && !MOVES[record.state]?.includes(patch.state))
    return `state: cannot move from ${record.state} to ${patch.state}`;
  return null;
}

function routeError(route, i) {
  const at = `routes[${i}]`;
  if (!isObject(route)) return `${at}: must be an object`;
  const extra = unknownKey(route, ROUTE_KEYS);
  if (extra !== undefined) return `${at}.${extra}: unknown field`;
  for (const key of ["name", "provider"])
    if (typeof route[key] !== "string" || !HOST_NAME.test(route[key]))
      return `${at}.${key}: must be a name of letters, digits, dot, dash and underscore`;
  if (typeof route.model !== "string" || !MODEL.test(route.model))
    return `${at}.model: must be a model name`;
  if (route.effort != null && (typeof route.effort !== "string" || !HOST_NAME.test(route.effort)))
    return `${at}.effort: must be a name or null`;
  if (route.limited_until != null && !isIso(route.limited_until))
    return `${at}.limited_until: must be an ISO 8601 UTC time or null`;
  return null;
}

/** The first error in a PUT /api/hosts/<id>/status body, or null. */
export function validateHostStatus(body) {
  if (!isObject(body)) return "body: must be a JSON object";
  if ("seen" in body) return "seen: set by the server";
  const extra = unknownKey(body, STATUS_KEYS);
  if (extra !== undefined) return `${extra}: unknown field`;
  const missing = STATUS_KEYS.find((key) => !(key in body));
  if (missing !== undefined) return `${missing}: required`;
  if (
    typeof body.os !== "string" ||
    body.os.length < 1 ||
    body.os.length > 40 ||
    !/^[A-Za-z0-9 ._()-]+$/.test(body.os)
  )
    return "os: must be 1 to 40 letters, digits, spaces and ._()-";
  if (!SCHEDULERS.includes(body.scheduler))
    return `scheduler: must be one of ${SCHEDULERS.join(", ")}`;
  if (body.last_pickup !== null && !isIso(body.last_pickup))
    return "last_pickup: must be an ISO 8601 UTC time or null";
  if (body.bundle !== null) {
    const b = body.bundle;
    if (
      !isObject(b) ||
      unknownKey(b, ["commit", "tree"]) !== undefined ||
      typeof b.commit !== "string" ||
      !SHA.test(b.commit) ||
      typeof b.tree !== "string" ||
      !SHA.test(b.tree)
    )
      return "bundle: must be {commit, tree} hashes or null";
  }
  if (body.doctor !== "ok") {
    if (
      !Array.isArray(body.doctor) ||
      body.doctor.length > 32 ||
      !body.doctor.every((name) => typeof name === "string" && /^[a-z0-9-]{1,64}$/.test(name))
    )
      return 'doctor: must be "ok" or a list of at most 32 check names';
  }
  if (!Array.isArray(body.routes) || body.routes.length > 16)
    return "routes: must be a list of at most 16 routes";
  for (const [i, route] of body.routes.entries()) {
    const error = routeError(route, i);
    if (error) return error;
  }
  if (
    !Array.isArray(body.checkouts) ||
    body.checkouts.length > 64 ||
    !body.checkouts.every((slug) => typeof slug === "string" && /^[\w-]+$/.test(slug))
  )
    return "checkouts: must be a list of at most 64 project slugs";
  return null;
}

/** A fresh pairing code, shown as XXXXX-XXXXX. */
export function newPairingCode() {
  // 248 is the largest multiple of the 31-letter alphabet under 256, so no letter is favoured.
  const chars = [];
  while (chars.length < 10) {
    for (const byte of crypto.getRandomValues(new Uint8Array(16))) {
      if (byte < 248 && chars.length < 10) chars.push(CODE_ALPHABET[byte % 31]);
    }
  }
  return `${chars.slice(0, 5).join("")}-${chars.slice(5).join("")}`;
}
