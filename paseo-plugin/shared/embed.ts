// Boundary checks for the framed factory dashboard. Wire format version 1.

export const EMBED_ID = /^[A-Za-z0-9_-]{1,64}$/;
const ORIGIN =
  /^https:\/\/([a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+)\/?$/i;
export const JOB_ID = /^[A-Za-z0-9-]{1,128}$/;

export type EmbedMessage =
  | { type: "factory-embed-ready" }
  | { type: "factory-open-agent"; server: string; agent: string };

export function parseDashboardOrigin(value: string): string | null {
  const match = ORIGIN.exec(value.trim());
  return match ? "https://" + match[1].toLowerCase() : null;
}

export function dashboardSrc(origin: string, job?: string): string {
  return origin + "/" + (job && JOB_ID.test(job) ? "#" + job : "");
}

export function readEmbedMessage(data: unknown): EmbedMessage | null {
  if (!data || typeof data !== "object") return null;
  const message = data as { type?: unknown; v?: unknown; server?: unknown; agent?: unknown };
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
  return null;
}
