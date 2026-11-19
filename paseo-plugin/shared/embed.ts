// Boundary checks for the framed factory dashboard. Wire format version 1.

export const EMBED_ID = /^[A-Za-z0-9_-]{1,64}$/;
const ORIGIN =
  /^https:\/\/([a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+)\/?$/i;
export const JOB_ID = /^[A-Za-z0-9-]{1,128}$/;
export const OPEN_URL = /^https?:\/\/[^\/?#@\s\x00-\x1f\x7f]+(?:[\/?#][^\s\x00-\x1f\x7f]*)?$/i;
export const OPEN_URL_MAX = 2048;
export const EMBED_HASH = /^(?:#[A-Za-z0-9\-_.!~*'()%/]{1,256})?$/;

export type EmbedMessage =
  | { type: "factory-embed-ready" }
  | { type: "factory-open-agent"; server: string; agent: string }
  | { type: "factory-open-url"; url: string }
  | { type: "factory-inbox-count"; count: number }
  | { type: "factory-embed-location"; hash: string };

export function parseDashboardOrigin(value: string): string | null {
  const match = ORIGIN.exec(value.trim());
  return match ? "https://" + match[1].toLowerCase() : null;
}

export function dashboardSrc(origin: string, job?: string): string {
  return origin + "/" + (job && JOB_ID.test(job) ? "#" + job : "");
}

export function readSignedInMessage(data: unknown): boolean {
  if (!data || typeof data !== "object") return false;
  const message = data as { type?: unknown; v?: unknown };
  return message.type === "factory-embed-signed-in" && message.v === 1;
}

export function readEmbedMessage(data: unknown): EmbedMessage | null {
  if (!data || typeof data !== "object") return null;
  const message = data as {
    type?: unknown;
    v?: unknown;
    server?: unknown;
    agent?: unknown;
    count?: unknown;
    hash?: unknown;
    url?: unknown;
  };
  if (message.v !== 1) return null;
  if (message.type === "factory-embed-ready") return { type: "factory-embed-ready" };
  if (
    message.type === "factory-open-agent" &&
    typeof message.server === "string" &&
    typeof message.agent === "string" &&
    EMBED_ID.test(message.server) &&
    EMBED_ID.test(message.agent)
  )
    return { type: "factory-open-agent", server: message.server, agent: message.agent };
  if (
    message.type === "factory-open-url" &&
    typeof message.url === "string" &&
    message.url.length <= OPEN_URL_MAX &&
    OPEN_URL.test(message.url)
  )
    return { type: "factory-open-url", url: message.url };
  if (
    message.type === "factory-inbox-count" &&
    typeof message.count === "number" &&
    Number.isSafeInteger(message.count) &&
    message.count >= 0
  )
    return { type: "factory-inbox-count", count: message.count };
  if (
    message.type === "factory-embed-location" &&
    typeof message.hash === "string" &&
    EMBED_HASH.test(message.hash)
  )
    return { type: "factory-embed-location", hash: message.hash };
  return null;
}

type HelloTarget = { postMessage(message: unknown, targetOrigin: string): void };

// Every frame load, first or self-navigated: drop the count the old page posted, then greet the new page.
export function frameLoaded(
  target: HelloTarget | null | undefined,
  origin: string,
  clearCount: (count: null) => void,
  externalLinks: boolean,
): void {
  clearCount(null);
  target?.postMessage(
    externalLinks
      ? { type: "factory-embed-hello", v: 1, links: "external" }
      : { type: "factory-embed-hello", v: 1 },
    origin,
  );
}
