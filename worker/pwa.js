// Installable dashboard: the web manifest and the service worker served at /sw.js.

/** @param {string} title */
export function manifest(title) {
  const icon = (size, purpose) => ({
    src: `/assets/icon-${size}.png`,
    sizes: `${size}x${size}`,
    type: "image/png",
    purpose,
  });
  return JSON.stringify({
    id: "/",
    name: title,
    short_name: title,
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#f9fafb",
    theme_color: "#f9fafb",
    icons: [icon(192, "any"), icon(512, "any"), icon(512, "maskable")],
  });
}

// Serialized into /sw.js, so it references only service worker globals. Helpers are object
// methods because Wrangler's keep_names wraps named functions in a module-level __name call.
export function serviceWorker() {
  const CACHE = "factory-v2";
  const sw = {
    load(url) {
      return fetch(url, { credentials: "same-origin", redirect: "manual", cache: "no-store" });
    },

    // Fetches from the network and keeps the cache in step with what the server said.
    // An Access redirect, 401 or 403 clears the cache so a signed-out device keeps no board.
    async update(url, key, navigate) {
      const response = await sw.load(url);
      if (
        response.type === "opaqueredirect" ||
        response.status === 401 ||
        response.status === 403
      ) {
        await caches.delete(CACHE);
        if (navigate) return response;
        return new Response("sign in required", {
          status: 401,
          headers: { "X-Factory-Signin": "required", "Cache-Control": "no-store" },
        });
      }
      const cache = await caches.open(CACHE);
      if (response.status === 200 && response.type === "basic")
        await cache.put(key, response.clone());
      else if (response.status === 404) await cache.delete(key);
      return response;
    },

    async offline(key) {
      const hit = await (await caches.open(CACHE)).match(key);
      if (!hit) return Response.error();
      const headers = new Headers(hit.headers);
      headers.set("X-Factory-Offline", "1");
      return new Response(hit.body, { status: hit.status, statusText: hit.statusText, headers });
    },

    async staleWhileRevalidate(event, key) {
      const hit = await (await caches.open(CACHE)).match(key);
      const fresh = sw.update(event.request.url, key, event.request.mode === "navigate");
      if (!hit) return fresh.catch(() => Response.error());
      event.waitUntil(fresh.catch(() => {}));
      return hit;
    },
  };

  self.addEventListener("install", (event) => {
    self.skipWaiting();
    event.waitUntil(
      Promise.all(
        ["/", "/assets/mermaid.min.js"].map((path) => sw.update(path, path, false).catch(() => {})),
      ),
    );
  });

  self.addEventListener("activate", (event) => {
    event.waitUntil(
      caches
        .keys()
        .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
        .then(() => self.clients.claim()),
    );
  });

  self.addEventListener("fetch", (event) => {
    const request = event.request;
    const url = new URL(request.url);
    const path = url.pathname;
    if (
      request.method !== "GET" ||
      url.origin !== location.origin ||
      path.startsWith("/api/") ||
      path === "/sw.js" ||
      // A job's walkthroughs go straight to the network, so the Worker sees the frame's own
      // Sec-Fetch-Dest. A service worker re-fetch would send "empty" and be refused.
      path.endsWith("walkthrough.html")
    )
      return;
    const navigate = request.mode === "navigate";
    if (navigate && path.startsWith("/cdn-cgi/access/logout"))
      event.waitUntil(caches.delete(CACHE));
    else if ((navigate && path === "/") || path.startsWith("/assets/"))
      event.respondWith(sw.staleWhileRevalidate(event, path));
    else if (path === "/board.json" || path === "/projects.json" || path.startsWith("/jobs/"))
      event.respondWith(sw.update(request.url, path, navigate).catch(() => sw.offline(path)));
  });
}
