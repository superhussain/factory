// Generates a Web Push VAPID key for the FACTORY_VAPID_KEY Worker secret. The private JWK goes
// to stdout, so it can be piped straight into `wrangler secret put` and never shown. The public
// key, which browsers subscribe with, goes to stderr.
const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, [
  "sign",
]);
const { kty, crv, x, y, d } = await crypto.subtle.exportKey("jwk", pair.privateKey);
process.stdout.write(JSON.stringify({ kty, crv, x, y, d }));
const raw = Buffer.from(await crypto.subtle.exportKey("raw", pair.publicKey));
process.stderr.write(`public key: ${raw.toString("base64url")}\n`);
