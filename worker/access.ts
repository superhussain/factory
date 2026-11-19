import { createRemoteJWKSet, decodeProtectedHeader, jwtVerify } from "jose";

export type VerifiedPrincipal =
  | { verified: true; kind: "user"; email: string }
  | { verified: true; kind: "service"; client_id: string | null };
export type AuthConfig = { issuer: string; audience: string };

const resolvers = new Map<string, ReturnType<typeof createRemoteJWKSet>>();

export async function authenticate(
  request: Request,
  config: AuthConfig,
): Promise<VerifiedPrincipal> {
  const issuer = new URL(config.issuer);
  if (
    issuer.origin !== config.issuer ||
    issuer.protocol !== "https:" ||
    !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.cloudflareaccess\.com$/.test(issuer.hostname) ||
    issuer.username ||
    issuer.password ||
    !config.audience ||
    typeof config.audience !== "string"
  ) {
    throw new Error("invalid configuration");
  }
  const token = request.headers.get("Cf-Access-Jwt-Assertion");
  if (typeof token !== "string" || !token) throw new Error("invalid assertion");
  const header = decodeProtectedHeader(token);
  if (header.alg !== "RS256" || typeof header.kid !== "string" || !header.kid.trim()) {
    throw new Error("invalid assertion");
  }
  let resolver = resolvers.get(config.issuer);
  if (!resolver) {
    resolver = createRemoteJWKSet(new URL("/cdn-cgi/access/certs", issuer), {
      timeoutDuration: 5000,
      cacheMaxAge: 600000,
      cooldownDuration: 5000,
    });
    resolvers.set(config.issuer, resolver);
  }
  const { payload } = await jwtVerify(token, resolver, {
    algorithms: ["RS256"],
    issuer: config.issuer,
    audience: config.audience,
    requiredClaims: ["exp"],
    clockTolerance: 30,
  });
  for (const name of ["exp", "nbf", "iat"]) {
    if (
      (name === "exp" || name in payload) &&
      (typeof payload[name] !== "number" || !Number.isFinite(payload[name]))
    ) {
      throw new Error("invalid assertion");
    }
  }
  if ((payload.iat as number) > Date.now() / 1000 + 30) throw new Error("invalid assertion");
  // A person's assertion carries an email; a service token's carries a common_name.
  if (
    typeof payload.email === "string" &&
    /^[^\s@<>"]{1,64}@[^\s@<>"]{1,255}$/.test(payload.email) &&
    !("common_name" in payload)
  ) {
    return { verified: true, kind: "user", email: payload.email };
  }
  const client_id =
    typeof payload.common_name === "string" && /^[A-Za-z0-9._-]{1,128}$/.test(payload.common_name)
      ? payload.common_name
      : null;
  return { verified: true, kind: "service", client_id };
}
