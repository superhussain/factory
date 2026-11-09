// Test-only workerd wrapper around the production default entry: real fetch and real
// jose verification in the Workers runtime. It builds the synthetic configured HTTPS
// request, signs assertions with a locally generated RSA key, intercepts only the
// configured synthetic JWKS fetch, and records storage calls so the driver can prove
// denials happen before content I/O. Production never imports this file and no
// deployment profile can select it.
import worker from "../../worker/index.js";

const encoder = new TextEncoder();
const encode = (value) =>
  btoa(String.fromCharCode(...encoder.encode(value)))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
// Signatures are raw ArrayBuffer bytes: base64url-encode them directly, never via
// the string encoder, which would TextEncoder-coerce "[object ArrayBuffer]".
const encodeBytes = (buffer) =>
  btoa(String.fromCharCode(...new Uint8Array(buffer)))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");

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
const jwk = {
  ...(await crypto.subtle.exportKey("jwk", pair.publicKey)),
  kid: "workerd-current",
  alg: "RS256",
  use: "sig",
};
const other = await crypto.subtle.generateKey(
  {
    name: "RSASSA-PKCS1-v1_5",
    modulusLength: 2048,
    publicExponent: new Uint8Array([1, 0, 1]),
    hash: "SHA-256",
  },
  true,
  ["sign"],
);

async function sign(privateKey, kid, claims) {
  const input = `${encode(JSON.stringify({ alg: "RS256", kid }))}.${encode(JSON.stringify(claims))}`;
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    privateKey,
    encoder.encode(input),
  );
  return `${input}.${encodeBytes(signature)}`;
}

function recording(bucket, calls) {
  return {
    head: async (key) => {
      calls.push(["head", key]);
      return bucket.head(key);
    },
    get: async (key, options) => {
      calls.push(["get", key]);
      return bucket.get(key, options);
    },
    put: async (key, value, options) => {
      calls.push(["put", key]);
      return bucket.put(key, value, options);
    },
    list: async (options) => {
      calls.push(["list", options?.prefix ?? null]);
      return bucket.list(options);
    },
  };
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const mode = url.searchParams.get("mode") ?? "valid";
    const path = url.searchParams.get("path") ?? "/dashboard.html";
    const method = url.searchParams.get("method") ?? "GET";
    const claims = {
      iss: env.ACCESS_ISSUER,
      aud: env.ACCESS_AUDIENCE,
      exp: Math.floor(Date.now() / 1000) + 3600,
      type: "app",
      common_name: "device",
    };
    const headers = {};
    if (mode === "valid")
      headers["Cf-Access-Jwt-Assertion"] = await sign(pair.privateKey, jwk.kid, claims);
    else if (mode === "wrong-key")
      headers["Cf-Access-Jwt-Assertion"] = await sign(other.privateKey, jwk.kid, claims);
    else if (mode === "wrong-issuer")
      headers["Cf-Access-Jwt-Assertion"] = await sign(pair.privateKey, jwk.kid, {
        ...claims,
        iss: "https://other.cloudflareaccess.com",
      });
    else if (mode === "wrong-audience")
      headers["Cf-Access-Jwt-Assertion"] = await sign(pair.privateKey, jwk.kid, {
        ...claims,
        aud: "other-audience",
      });
    else if (mode === "expired")
      headers["Cf-Access-Jwt-Assertion"] = await sign(pair.privateKey, jwk.kid, {
        ...claims,
        exp: Math.floor(Date.now() / 1000) - 3600,
      });
    else if (mode === "garbage") headers["Cf-Access-Jwt-Assertion"] = "garbage";
    else if (mode === "cookie") headers["Cookie"] = "CF_Authorization=not-an-assertion";
    else if (mode !== "none") return new Response("unknown mode", { status: 400 });

    const realFetch = globalThis.fetch;
    globalThis.fetch = (input) => {
      const target = String(input);
      if (target === `${env.ACCESS_ISSUER}/cdn-cgi/access/certs`)
        return Promise.resolve(Response.json({ keys: [jwk] }));
      return Promise.reject(new Error("unexpected external fetch"));
    };
    const calls = [];
    try {
      const origin =
        url.searchParams.get("origin") === "other"
          ? "https://other.example.test"
          : env.FACTORY_ORIGIN;
      const target = new Request(origin + path, { method, headers });
      const response = await worker.fetch(target, { ...env, BUCKET: recording(env.BUCKET, calls) });
      const copy = new Response(response.body, response);
      copy.headers.set("X-Test-Storage-Calls", JSON.stringify(calls));
      return copy;
    } finally {
      globalThis.fetch = realFetch;
    }
  },
};
