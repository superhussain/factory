import assert from "node:assert/strict";

const encoder = new TextEncoder();
const encode = (value) => Buffer.from(value).toString("base64url");
let nextIssuer = 0;

export async function generateKey(kid) {
  const pair = await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: new Uint8Array([1, 0, 1]),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  );
  return {
    ...pair,
    jwk: {
      ...(await crypto.subtle.exportKey("jwk", pair.publicKey)),
      kid,
      alg: "RS256",
      use: "sig",
    },
  };
}

export async function sign(key, claims, header = {}, rawPayload) {
  const protectedHeader = { alg: "RS256", kid: key.jwk.kid, ...header };
  const input = `${encode(JSON.stringify(protectedHeader))}.${encode(rawPayload ?? JSON.stringify(claims))}`;
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    key.privateKey,
    encoder.encode(input),
  );
  return `${input}.${encode(signature)}`;
}

export function jwksFixture(t, keys, now = 1800000000000) {
  const config = {
    issuer: `https://test-${++nextIssuer}.cloudflareaccess.com`,
    audience: "example-application-audience",
  };
  const state = { keys: keys.map((key) => key.jwk), calls: [], mode: "ok", body: null };
  t.mock.timers.enable({ apis: ["Date"], now });
  t.mock.method(globalThis, "fetch", async (url, options) => {
    assert.equal(String(url), `${config.issuer}/cdn-cgi/access/certs`);
    assert.equal(options.redirect, "manual");
    state.calls.push({ url: String(url), at: Date.now() });
    if (state.mode === "outage") throw new Error("synthetic outage");
    if (state.mode === "timeout") {
      return new Promise((resolve, reject) =>
        options.signal.addEventListener("abort", () => reject(options.signal.reason), {
          once: true,
        }),
      );
    }
    if (state.mode === "status") return new Response("unavailable", { status: 503 });
    if (state.mode === "invalid-json") return new Response("{");
    return Response.json(state.body ?? { keys: state.keys });
  });
  return {
    config,
    state,
    advance: (milliseconds) => t.mock.timers.setTime(Date.now() + milliseconds),
    claims: (overrides) => ({
      iss: config.issuer,
      aud: config.audience,
      exp: Date.now() / 1000 + 3600,
      ...overrides,
    }),
    request: (token) =>
      new Request("https://factory.example.test/", {
        headers: { "Cf-Access-Jwt-Assertion": token },
      }),
  };
}
