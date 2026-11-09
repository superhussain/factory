// Owned browser origin check shared by CJS suites and ESM fixtures: the fixed
// operator ports, or exactly the fixture's own declared ephemeral loopback origin.
const FIXED_OWNED_ORIGINS = ["http://127.0.0.1:8220", "http://127.0.0.1:8221"];
const OWNED_ORIGIN_ENV = "FACTORY_OWNED_BROWSER_ORIGIN";

function isOwnedBrowserOrigin(origin, env = process.env) {
  if (FIXED_OWNED_ORIGINS.includes(origin)) return true;
  if (typeof origin !== "string" || origin !== env[OWNED_ORIGIN_ENV]) return false;
  const match = /^http:\/\/127\.0\.0\.1:([1-9]\d{3,4})$/.exec(origin);
  if (!match) return false;
  const port = Number(match[1]);
  return port >= 1024 && port <= 65535;
}

function assertOwnedBrowserOrigin(origin, env = process.env) {
  if (!isOwnedBrowserOrigin(origin, env)) {
    throw new Error(
      "browser tests only use owned loopback origins: http://127.0.0.1:8220 or http://127.0.0.1:8221, or this fixture's own origin in FACTORY_OWNED_BROWSER_ORIGIN",
    );
  }
}

module.exports = {
  FIXED_OWNED_ORIGINS,
  OWNED_ORIGIN_ENV,
  isOwnedBrowserOrigin,
  assertOwnedBrowserOrigin,
};
