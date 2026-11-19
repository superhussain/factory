import assert from "node:assert/strict";
import test from "node:test";
import { authenticate } from "../../worker/access.ts";
import { generateKey, jwksFixture, sign } from "../support/jwks.mjs";

const [oldKey, currentKey, forgedKey] = await Promise.all([
  generateKey("previous"),
  generateKey("current"),
  generateKey("current"),
]);

test("service claims without email, string/array audience and retained rotation keys", async (t) => {
  const f = jwksFixture(t, [oldKey, currentKey]);
  for (const key of [oldKey, currentKey]) {
    for (const aud of [f.config.audience, ["other", f.config.audience]]) {
      const token = await sign(key, f.claims({ aud, type: "app", common_name: "device" }));
      assert.deepEqual(await authenticate(f.request(token), f.config), {
        verified: true,
        kind: "service",
        client_id: "device",
      });
    }
  }
  assert.equal(f.state.calls.length, 1);
});

test("person claims carry kind user and email; mixed or malformed claims stay service", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  const check = async (claims, expected) =>
    assert.deepEqual(
      await authenticate(f.request(await sign(currentKey, f.claims(claims))), f.config),
      expected,
      JSON.stringify(claims),
    );
  await check(
    { type: "app", email: "person@example.test" },
    { verified: true, kind: "user", email: "person@example.test" },
  );
  await check(
    { email: "person@example.test", common_name: "device" },
    { verified: true, kind: "service", client_id: "device" },
  );
  await check(
    { email: "person@example.test", common_name: "" },
    { verified: true, kind: "service", client_id: null },
  );
  for (const email of [
    "",
    "person",
    "a b@example.test",
    "<x>@example.test",
    "a@b@example.test",
    "x".repeat(65) + "@example.test",
    1,
    null,
    ["person@example.test"],
  ]) {
    await check({ email }, { verified: true, kind: "service", client_id: null });
  }
});

test("service client_id is the common_name when well formed, otherwise null", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  const check = async (common_name, client_id) =>
    assert.deepEqual(
      await authenticate(f.request(await sign(currentKey, f.claims({ common_name }))), f.config),
      { verified: true, kind: "service", client_id },
      JSON.stringify(common_name),
    );
  await check("abc123.access", "abc123.access");
  await check("A_b-c.9", "A_b-c.9");
  await check("x".repeat(128), "x".repeat(128));
  for (const bad of ["", "x".repeat(129), "a b", "a/b", "a\nb", "dévice", 1, null, ["device"], {}])
    await check(bad, null);
  await check(undefined, null);
});

test("signature, issuer, audience, required claims and numeric types deny", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  const invalid = [
    { iss: undefined },
    { iss: "https://wrong.cloudflareaccess.com" },
    { iss: [f.config.issuer] },
    { aud: undefined },
    { aud: "wrong" },
    { aud: ["wrong"] },
    { aud: 1 },
    { exp: undefined },
    { exp: "1800003600" },
    { exp: null },
    { exp: true },
    { nbf: "0" },
    { nbf: null },
    { iat: "0" },
    { iat: null },
  ];
  for (const overrides of invalid) {
    const token = await sign(currentKey, f.claims(overrides));
    await assert.rejects(authenticate(f.request(token), f.config), JSON.stringify(overrides));
  }
  const token = await sign(forgedKey, f.claims());
  await assert.rejects(authenticate(f.request(token), f.config));
  const good = await sign(currentKey, f.claims());
  const [header, payload, signature] = good.split(".");
  const changed = Buffer.from(signature, "base64url");
  changed[0] ^= 1;
  await assert.rejects(
    authenticate(f.request(`${header}.${payload}.${changed.toString("base64url")}`), f.config),
  );
  for (const claim of ["exp", "nbf", "iat"]) {
    const raw = JSON.stringify(f.claims()).replace(/}$/, `,"${claim}":1e999}`);
    await assert.rejects(authenticate(f.request(await sign(currentKey, {}, {}, raw)), f.config));
  }
});

test("time tolerance boundaries are exactly thirty seconds", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  const now = Date.now() / 1000;
  for (const [overrides, valid] of [
    [{ exp: now - 29 }, true],
    [{ exp: now - 30 }, false],
    [{ exp: now - 31 }, false],
    [{ nbf: now + 30 }, true],
    [{ nbf: now + 31 }, false],
    [{ iat: now + 30 }, true],
    [{ iat: now + 31 }, false],
  ]) {
    const promise = authenticate(f.request(await sign(currentKey, f.claims(overrides))), f.config);
    if (valid)
      assert.deepEqual(await promise, { verified: true, kind: "service", client_id: null });
    else await assert.rejects(promise);
  }
});

test("closed algorithms, kid and malformed compact assertions", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  for (const header of [
    { alg: "none" },
    { alg: "HS256" },
    { kid: undefined },
    { kid: "" },
    { kid: " " },
    { kid: 1 },
    { kid: "unknown" },
  ]) {
    await assert.rejects(
      authenticate(f.request(await sign(currentKey, f.claims(), header)), f.config),
    );
  }
  for (const token of ["garbage", "a.b.c", "..", "e30.e30.", "null"]) {
    await assert.rejects(authenticate(f.request(token), f.config));
  }
  await assert.rejects(
    authenticate(
      new Request("https://factory.example.test", {
        headers: {
          Cookie: "CF_Authorization=not-identity",
          "CF-Access-Client-Id": "device",
          "CF-Access-Client-Secret": "not-identity",
        },
      }),
      f.config,
    ),
  );
});

test("token URL headers cannot choose trust", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  const token = await sign(currentKey, f.claims(), {
    jku: "https://untrusted.example.test/keys",
    x5u: "https://untrusted.example.test/cert",
  });
  assert.deepEqual(await authenticate(f.request(token), f.config), {
    verified: true,
    kind: "service",
    client_id: null,
  });
  assert.deepEqual(
    f.state.calls.map((call) => call.url),
    [`${f.config.issuer}/cdn-cgi/access/certs`],
  );
});

test("unknown kid refresh cooldown, rotation and previous-key continuity", async (t) => {
  const f = jwksFixture(t, [oldKey]);
  await authenticate(f.request(await sign(oldKey, f.claims())), f.config);
  f.state.keys = [oldKey.jwk, currentKey.jwk];
  const token = await sign(currentKey, f.claims());
  await assert.rejects(authenticate(f.request(token), f.config));
  f.advance(4999);
  await assert.rejects(authenticate(f.request(token), f.config));
  assert.equal(f.state.calls.length, 1);
  f.advance(1);
  await authenticate(f.request(token), f.config);
  await authenticate(f.request(await sign(oldKey, f.claims())), f.config);
  assert.equal(f.state.calls.length, 2);
});

test("fresh cached keys survive outage but stale and missing keys never bypass", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  const token = await sign(currentKey, f.claims());
  await authenticate(f.request(token), f.config);
  f.state.mode = "outage";
  f.advance(599999);
  await authenticate(f.request(token), f.config);
  assert.equal(f.state.calls.length, 1);
  f.advance(1);
  await assert.rejects(authenticate(f.request(token), f.config));
  assert.equal(f.state.calls.length, 2);
  f.state.mode = "ok";
  await authenticate(f.request(token), f.config);
  assert.equal(f.state.calls.length, 3);
});

for (const mode of ["outage", "status", "invalid-json", "timeout"]) {
  test(`unusable key service denies: ${mode}`, async (t) => {
    const f = jwksFixture(t, [currentKey]);
    f.state.mode = mode;
    const token = await sign(currentKey, f.claims());
    const keepAlive = setTimeout(() => {}, 6000);
    try {
      await assert.rejects(authenticate(f.request(token), f.config));
    } finally {
      clearTimeout(keepAlive);
    }
    assert.equal(f.state.calls.length, 1);
  });
}

for (const body of [
  {},
  { keys: "bad" },
  { keys: [] },
  { keys: [currentKey.jwk, currentKey.jwk] },
  { keys: [{ ...currentKey.jwk, n: "invalid" }] },
]) {
  test(`malformed or ambiguous JWKS denies: ${JSON.stringify(body).slice(0, 50)}`, async (t) => {
    const f = jwksFixture(t, [currentKey]);
    f.state.body = body;
    await assert.rejects(authenticate(f.request(await sign(currentKey, f.claims())), f.config));
  });
}

test("issuer resolver caches cannot cross issuer boundaries", async (t) => {
  const f = jwksFixture(t, [currentKey]);
  const token = await sign(currentKey, f.claims());
  await authenticate(f.request(token), f.config);
  const other = { ...f.config, issuer: "https://isolated.cloudflareaccess.com" };
  t.mock.method(globalThis, "fetch", async (url) => {
    assert.equal(String(url), `${other.issuer}/cdn-cgi/access/certs`);
    return Response.json({ keys: [oldKey.jwk] });
  });
  await assert.rejects(
    authenticate(f.request(await sign(currentKey, { ...f.claims(), iss: other.issuer })), other),
  );
  await authenticate(f.request(token), f.config);
});
