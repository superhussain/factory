import page from "../web/dashboard.html";
import mermaid from "../.build/mermaid.min.txt";
import notices from "../.build/third-party-notices.txt";
import { FLOORS, GATES } from "./admission.js";
import { APPLE_TOUCH_ICON, ICON_192, ICON_512 } from "./icons.js";
import { manifest, serviceWorker } from "./pwa.js";

/** @typedef {{origin:string, title:string, issuer:string, audience:string,
 * image_origins:string[], agent_links:Record<string,{label:string,template:string}>,
 * jira_origin:string|null}} ServerConfig */
// oxlint-disable-next-line no-control-regex -- this pattern exists to reject control characters.
const controls = /[\x00-\x1f\x7f-\x9f]/u;
const dns = /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]*$/;
export const escapeHtml = (s) =>
  s.replace(
    /[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#x27;" })[c],
  );

function text(value, max = Infinity) {
  if (typeof value !== "string" || !value.trim() || controls.test(value) || [...value].length > max)
    throw new Error("invalid configuration");
  return value;
}

function httpsOrigin(value) {
  text(value);
  const url = new URL(value);
  if (
    url.protocol !== "https:" ||
    url.origin !== value ||
    url.username ||
    url.password ||
    url.port ||
    !(
      dns.test(url.hostname) ||
      /^[0-9.]+$/.test(url.hostname) ||
      /^\[[0-9a-f:]+\]$/.test(url.hostname)
    )
  )
    throw new Error("invalid origin");
  return url;
}

function linkURL(value, kind) {
  if (controls.test(value) || value.startsWith("//") || /[{}]/.test(value))
    throw new Error("invalid link");
  const url = new URL(value);
  if (
    url.username ||
    url.password ||
    ["javascript:", "data:", "file:", "blob:", "http:"].includes(url.protocol) ||
    (kind === "web" ? url.protocol !== "https:" : url.protocol === "https:")
  )
    throw new Error("invalid link");
  return url;
}

/** @returns {ServerConfig} */
export function readServerConfig(env) {
  const origin = text(env.FACTORY_ORIGIN);
  const host = httpsOrigin(origin).hostname;
  if (!dns.test(host) || host === "workers.dev" || host.endsWith(".workers.dev"))
    throw new Error("invalid origin");
  const issuer = text(env.ACCESS_ISSUER);
  if (
    !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.cloudflareaccess\.com$/.test(httpsOrigin(issuer).hostname)
  )
    throw new Error("invalid issuer");
  const title = text(env.FACTORY_TITLE, 160);
  const audience = text(env.ACCESS_AUDIENCE);
  for (const name of ["FACTORY_IMAGE_ORIGINS", "FACTORY_AGENT_LINKS", "FACTORY_JIRA_ORIGIN"]) {
    if (env[name] !== undefined && typeof env[name] !== "string")
      throw new Error("invalid configuration");
  }
  const image_origins =
    env.FACTORY_IMAGE_ORIGINS === undefined ? [] : JSON.parse(env.FACTORY_IMAGE_ORIGINS);
  const agent_links =
    env.FACTORY_AGENT_LINKS === undefined ? {} : JSON.parse(env.FACTORY_AGENT_LINKS);
  if (!Array.isArray(image_origins) || new Set(image_origins).size !== image_origins.length)
    throw new Error("invalid images");
  image_origins.forEach(httpsOrigin);
  if (!agent_links || typeof agent_links !== "object" || Array.isArray(agent_links))
    throw new Error("invalid links");
  for (const [kind, link] of Object.entries(agent_links)) {
    if (
      !["app", "web"].includes(kind) ||
      !link ||
      typeof link !== "object" ||
      Array.isArray(link) ||
      Object.keys(link).sort().join(",") !== "label,template"
    )
      throw new Error("invalid links");
    text(link.label, 160);
    text(link.template);
    const placeholders = link.template.match(/\{[^{}]*\}/g) ?? [];
    if (
      placeholders.length !== 2 ||
      !placeholders.includes("{server}") ||
      !placeholders.includes("{agent}")
    )
      throw new Error("invalid template");
    linkURL(
      link.template.replaceAll("{server}", "server-id").replaceAll("{agent}", "agent-id"),
      kind,
    );
  }
  const jira_origin =
    env.FACTORY_JIRA_ORIGIN === undefined
      ? null
      : (httpsOrigin(env.FACTORY_JIRA_ORIGIN), env.FACTORY_JIRA_ORIGIN);
  return { origin, title, issuer, audience, image_origins, agent_links, jira_origin };
}

// FNV-1a 32 of the dashboard source: a tab compares it with the version it loaded.
export const DASHBOARD_VERSION = (() => {
  let h = 0x811c9dc5;
  for (let i = 0; i < page.length; i++) h = Math.imul(h ^ page.charCodeAt(i), 0x01000193);
  return (h >>> 0).toString(16).padStart(8, "0");
})();

const jsonScript = (value) =>
  JSON.stringify(value).replace(
    /[<>&\u2028\u2029]/g,
    (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`,
  );
const reference = jsonScript({ floors: FLOORS, gates: GATES });

function response(body, type, headers = {}) {
  return new Response(body, {
    headers: {
      "Content-Type": `${type}; charset=utf-8`,
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
      ...headers,
    },
  });
}

/** @param {ServerConfig} env */
export function renderDashboard(env) {
  const config = jsonScript({
    image_origins: env.image_origins,
    agent_links: env.agent_links,
    ...(env.jira_origin ? { jira_origin: env.jira_origin } : {}),
  });
  const csp =
    "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:" +
    env.image_origins.map((origin) => " " + origin).join("") +
    "; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'none'";
  const values = {
    __FACTORY_TITLE__: escapeHtml(env.title),
    __FACTORY_CSP__: escapeHtml(csp),
    __FACTORY_CONFIG__: config,
    __FACTORY_REFERENCE__: reference,
    __FACTORY_VERSION__: DASHBOARD_VERSION,
  };
  const html = page.replace(
    /__FACTORY_(?:TITLE|CSP|CONFIG|REFERENCE|VERSION)__/g,
    (token) => values[token],
  );
  return response(html, "text/html", { "Content-Security-Policy": csp });
}

const icons = {
  "/assets/icon-192.png": ICON_192,
  "/assets/icon-512.png": ICON_512,
  "/assets/apple-touch-icon.png": APPLE_TOUCH_ICON,
};

export async function serveAsset(request, env) {
  const path = new URL(request.url).pathname;
  if (request.method === "GET" && path === "/sw.js")
    return response("(" + serviceWorker + ")();", "text/javascript");
  if (request.method === "GET" && path === "/assets/manifest.webmanifest")
    return response(manifest(env.title), "application/manifest+json");
  if (request.method === "GET" && Object.hasOwn(icons, path))
    return new Response(
      Uint8Array.from(atob(icons[path]), (c) => c.charCodeAt(0)),
      {
        headers: {
          "Content-Type": "image/png",
          "Cache-Control": "no-store",
          "X-Content-Type-Options": "nosniff",
        },
      },
    );
  if (request.method === "GET" && path === "/assets/mermaid.min.js")
    return response(mermaid, "text/javascript");
  if (request.method === "GET" && path === "/assets/third-party-notices.txt")
    return response(notices, "text/plain");
  return new Response("not found", {
    status: 404,
    headers: {
      "Content-Type": "text/plain; charset=utf-8",
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
    },
  });
}
