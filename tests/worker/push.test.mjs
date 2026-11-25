// Web Push: the RFC 8291 encryption vector, the board diff and payloads, subscription
// routes, and one encrypted, VAPID-signed POST per subscription after a committed board write.
import assert from "node:assert/strict";
import test from "node:test";
import { importJWK, jwtVerify } from "jose";
import { loadProduction, memoryBucket, serverEnv } from "../support/worker.mjs";
import {
  boardPushes,
  encryptWith,
  pushKey,
  pushMessage,
  stalePushes,
  stuckPushes,
  validSubscription,
} from "../../worker/push.js";

const b64 = (text) => Buffer.from(text.replace(/\s/g, ""), "base64url");

// RFC 8291 Section 5 and Appendix A, copied verbatim with whitespace kept.
const RFC = {
  plaintext: "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24",
  as_public: `BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIg
      Dll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8`,
  as_private: "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw",
  ua_public: `BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-
      JvLexhqUzORcx aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4`,
  salt: "DGv6ra1nlYgDCS1FRnbzlw",
  auth_secret: "BTBZMqHH6r4Tts7J_aSIgg", // gitleaks:allow (public RFC 8291 test vector)
  header: `DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z 9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
   mlMoZIIgDll6e3vCYLocInmYWAmS6Tlz AC8wEqKK6PBru3jl7A8`,
  ciphertext: `8pfeW0KbunFT06SuDKoJH9Ql87S1QUrd irN6GcG7sFz1y1sqLgVi1VhjVkHsUoEs
   bI_0LpXMuGvnzQ`,
  body: `DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml
   mlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPT
   pK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN`,
};

test("encryptWith matches the RFC 8291 Appendix A vector", async () => {
  const pub = b64(RFC.as_public);
  const ecdh = { name: "ECDH", namedCurve: "P-256" };
  const jwk = {
    kty: "EC",
    crv: "P-256",
    x: pub.subarray(1, 33).toString("base64url"),
    y: pub.subarray(33).toString("base64url"),
    d: RFC.as_private,
  };
  const serverKeys = {
    privateKey: await crypto.subtle.importKey("jwk", jwk, ecdh, false, ["deriveBits"]),
    publicKey: await crypto.subtle.importKey("raw", pub, ecdh, true, []),
  };
  const out = Buffer.from(
    await encryptWith(
      b64(RFC.ua_public),
      b64(RFC.auth_secret),
      b64(RFC.plaintext),
      serverKeys,
      b64(RFC.salt),
    ),
  );
  assert.equal(out.toString("base64url"), b64(RFC.body).toString("base64url"));
  assert.deepEqual(out.subarray(0, 86), b64(RFC.header));
  assert.deepEqual(out.subarray(86), b64(RFC.ciphertext));
});

const row = (id, stage, extra = {}) => ({ id, title: "Synthetic " + id, stage, ...extra });

test("boardPushes finds new waits and merges only", () => {
  const before = [
    row("042-sample-retry", "build"),
    row("043-sample-pr", "pr-open"),
    row("044-sample-wait", "needs-human"),
    row("045-sample-done", "merged"),
  ];
  const after = [
    row("042-sample-retry", "needs-human", { needs: "Approve the spec" }),
    row("043-sample-pr", "merged"),
    row("044-sample-wait", "needs-human"),
    row("045-sample-done", "merged"),
    row("046-sample-new", "spec", { needs: "Pick one" }),
    row("047-sample-fresh", "merged"),
    row("048-sample-closed", "closed", { needs: "Old ask" }),
    null,
    { stage: "needs-human" },
    { id: 7, stage: "needs-human" },
  ];
  assert.deepEqual(boardPushes(before, after), [
    { kind: "needs", id: "042-sample-retry", title: "Synthetic 042-sample-retry" },
    { kind: "merged", id: "043-sample-pr", title: "Synthetic 043-sample-pr" },
    { kind: "needs", id: "046-sample-new", title: "Synthetic 046-sample-new" },
  ]);
  assert.deepEqual(
    boardPushes(
      [row("049-sample-esc", "pr-open")],
      [row("049-sample-esc", "pr-open", { note: "escalated: auth change" })],
    ),
    [{ kind: "needs", id: "049-sample-esc", title: "Synthetic 049-sample-esc" }],
  );
});

const SEEN = "2026-11-17T05:40:00Z";
const at = (hhmm) => new Date(`2026-11-17T${hhmm}:00Z`);

test("stuckPushes pushes a stale claim once, in its window", () => {
  const claim = { device: "device-a", seen: SEEN };
  const jobs = [
    row("042-sample-retry", "build", { claim }),
    row("060-sample-unclaimed", "review", { updated: SEEN }),
    row("061-sample-no-seen", "spec", { claim: { device: "device-a" }, updated: SEEN }),
    row("062-sample-needs", "build", { claim, needs: "Pick one" }),
    row("063-sample-pr", "pr-open", { claim }),
    row("064-sample-idea", "idea", { claim }),
    row("065-sample-merged", "merged", { claim }),
    row("066-sample-paused", "build", { claim, paused: { at: SEEN } }),
    row("067-sample-closing", "approve", { claim, close: { at: SEEN } }),
    row("068-sample-batched", "build", {
      claim,
      refs: [{ kind: "batched-into", id: "042-sample-retry" }],
    }),
    row("069-sample-move", "build", { claim, move: { to: "device-b" } }),
    row("070-sample-blank-needs", "build", { claim, needs: "  " }),
    row("071-sample-closed-close", "build", { claim, close: { at: SEEN, closed: SEEN } }),
    row("072-sample-moved", "build", { claim, move: { to: "device-b", claimed: SEEN } }),
  ];
  const stuck = (id) => ({ kind: "stuck", id, title: "Synthetic " + id });
  assert.deepEqual(stuckPushes(jobs, at("09:17")), []);
  assert.deepEqual(stuckPushes(jobs, at("10:17")), [
    stuck("042-sample-retry"),
    stuck("060-sample-unclaimed"),
    stuck("070-sample-blank-needs"),
    stuck("071-sample-closed-close"),
    stuck("072-sample-moved"),
  ]);
  assert.deepEqual(stuckPushes(jobs, at("11:17")), []);
  // The window is after its start and at or before its end.
  assert.equal(stuckPushes(jobs, at("09:40")).length, 5);
  assert.equal(stuckPushes(jobs, at("10:40")).length, 0);
});

const DAY_BEFORE = "2026-11-16T05:40:00Z";

test("stalePushes pushes a 24-hour wait once, in its window", () => {
  const updated = DAY_BEFORE;
  const jobs = [
    row("042-sample-retry", "needs-human", { needs: "Approve the spec", updated }),
    row("043-sample-needs", "build", { needs: "Pick one", updated }),
    row("044-sample-esc", "pr-open", { escalated: true, updated }),
    row("045-sample-paused", "needs-human", { paused: { at: SEEN }, updated }),
    row("046-sample-merged", "merged", { needs: "Old ask", updated }),
    row("047-sample-build", "build", { updated }),
  ];
  const stale = (id) => ({ kind: "stale", id, title: "Synthetic " + id });
  assert.deepEqual(stalePushes(jobs, at("05:17")), []);
  assert.deepEqual(stalePushes(jobs, at("06:17")), [
    stale("042-sample-retry"),
    stale("043-sample-needs"),
    stale("044-sample-esc"),
  ]);
  assert.deepEqual(stalePushes(jobs, at("07:17")), []);
  // The window is after its start and at or before its end.
  assert.equal(stalePushes(jobs, at("05:40")).length, 3);
  assert.equal(stalePushes(jobs, at("06:40")).length, 0);
});

test("pushMessage builds the needs, merged and summary payloads", () => {
  assert.deepEqual(
    pushMessage([{ kind: "needs", id: "042-sample-retry", title: "Synthetic retry job" }]),
    {
      title: "Needs you: 042-sample-retry",
      body: "Synthetic retry job",
      tag: "factory-needs-042-sample-retry",
      url: "/#042-sample-retry",
    },
  );
  assert.deepEqual(
    pushMessage([{ kind: "merged", id: "043-sample-pr", title: "Synthetic PR job" }]),
    {
      title: "Merged: 043-sample-pr",
      body: "Synthetic PR job",
      tag: "factory-merged-043-sample-pr",
      url: "/#043-sample-pr",
    },
  );
  assert.deepEqual(
    pushMessage([{ kind: "stuck", id: "042-sample-retry", title: "Synthetic retry job" }]),
    {
      title: "Stuck: 042-sample-retry",
      body: "Synthetic retry job",
      tag: "factory-stuck-042-sample-retry",
      url: "/#042-sample-retry",
    },
  );
  assert.deepEqual(
    pushMessage([{ kind: "stale", id: "042-sample-retry", title: "Synthetic retry job" }]),
    {
      title: "Still needs you: 042-sample-retry",
      body: "Synthetic retry job",
      tag: "factory-stale-042-sample-retry",
      url: "/#042-sample-retry",
    },
  );
  assert.deepEqual(
    pushMessage([
      { kind: "stuck", id: "042-sample-retry", title: "T" },
      { kind: "stuck", id: "060-sample-unclaimed", title: "T" },
    ]),
    {
      title: "2 factory updates",
      body: "Stuck: 042-sample-retry\nStuck: 060-sample-unclaimed",
      tag: "factory-updates",
      url: "/",
    },
  );
  const five = ["a", "b", "c", "d", "e"].map((x, i) => ({
    kind: i === 1 ? "merged" : "needs",
    id: "05" + i + "-" + x,
    title: "T",
  }));
  assert.deepEqual(pushMessage(five), {
    title: "5 factory updates",
    body: "Needs you: 050-a\nMerged: 051-b\nNeeds you: 052-c\nand 2 more",
    tag: "factory-updates",
    url: "/",
  });
  const long = pushMessage([{ kind: "needs", id: "x".repeat(90) + "#", title: "t".repeat(130) }]);
  assert.equal(long.title, "Needs you: " + "x".repeat(80));
  assert.equal(long.body, "t".repeat(120));
  assert.equal(long.url, "/#" + "x".repeat(90) + "%23");
});

const P256DH = Buffer.concat([Buffer.from([4]), Buffer.alloc(64, 1)]).toString("base64url");
const AUTH = Buffer.alloc(16, 2).toString("base64url");
const sub = (endpoint, keys = { p256dh: P256DH, auth: AUTH }) => ({ endpoint, keys });

test("validSubscription keeps allowed push hosts and well-formed keys only", () => {
  for (const endpoint of [
    "https://fcm.googleapis.com/fcm/send/abc",
    "https://updates.push.services.mozilla.com/wpush/v2/abc",
    "https://web.push.apple.com/abc",
    "https://api.push.apple.com/abc",
    "https://wns2-par02p.notify.windows.com/w/?token=abc",
  ]) {
    const record = validSubscription(sub(endpoint));
    assert.equal(record.endpoint, endpoint);
    assert.deepEqual(record.keys, { p256dh: P256DH, auth: AUTH });
    assert.ok(!Number.isNaN(Date.parse(record.created)));
    assert.deepEqual(Object.keys(record), ["endpoint", "keys", "created"]);
  }
  for (const endpoint of [
    "http://fcm.googleapis.com/fcm/send/abc",
    "https://evil.example.test/push",
    "https://push.apple.com.example.test/abc",
    "https://fcm.googleapis.com/" + "a".repeat(1024),
  ])
    assert.equal(validSubscription(sub(endpoint)), "push endpoint host is not allowed");
  const ok = "https://fcm.googleapis.com/fcm/send/abc";
  for (const body of [
    null,
    { endpoint: ok },
    sub(ok, { p256dh: AUTH, auth: AUTH }),
    sub(ok, { p256dh: P256DH, auth: P256DH }),
    sub(ok, { p256dh: "not base64!", auth: AUTH }),
    sub("not a url"),
  ])
    assert.equal(validSubscription(body), "body needs a push subscription");
});

test("pushKey reads a P-256 private JWK or returns null", async () => {
  const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, [
    "sign",
  ]);
  const jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
  const raw = Buffer.from(await crypto.subtle.exportKey("raw", pair.publicKey));
  assert.equal(pushKey({ FACTORY_VAPID_KEY: JSON.stringify(jwk) }), raw.toString("base64url"));
  for (const value of [undefined, "", "not json", JSON.stringify({ ...jwk, crv: "P-384" })])
    assert.equal(pushKey({ FACTORY_VAPID_KEY: value }), null);
});

const { serveFactory } = await loadProduction();
const ORIGIN = "https://factory.example.test";
const person = { verified: true, kind: "user", email: "person@example.test" };
const device = { verified: true, kind: "service", client_id: "client-aaaa" };
const ecdh = { name: "ECDH", namedCurve: "P-256" };
const VAPID = await crypto.subtle.exportKey(
  "jwk",
  (await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign"]))
    .privateKey,
);
const RETRY = row("042-sample-retry", "build", { title: "Synthetic retry job" });
const RETRY_WAITS = { ...RETRY, stage: "needs-human", needs: "Approve the spec" };

// A browser's subscription: its ECDH key pair, auth secret and the toJSON() body.
async function browser(endpoint) {
  const pair = await crypto.subtle.generateKey(ecdh, true, ["deriveBits"]);
  const auth = crypto.getRandomValues(new Uint8Array(16));
  const p256dh = Buffer.from(await crypto.subtle.exportKey("raw", pair.publicKey));
  return {
    pair,
    auth,
    p256dh,
    body: {
      endpoint,
      keys: { p256dh: p256dh.toString("base64url"), auth: Buffer.from(auth).toString("base64url") },
    },
  };
}

// The user agent side of RFC 8291: one record, padding delimiter 0x02.
async function decrypt(ua, bytes) {
  const salt = bytes.subarray(0, 16);
  assert.equal(bytes.readUInt32BE(16), 4096);
  const asPublic = bytes.subarray(21, 21 + bytes[20]);
  const as = await crypto.subtle.importKey("raw", asPublic, ecdh, false, []);
  const secret = new Uint8Array(
    await crypto.subtle.deriveBits({ name: "ECDH", public: as }, ua.pair.privateKey, 256),
  );
  const hkdf = async (s, ikm, info, n) =>
    new Uint8Array(
      await crypto.subtle.deriveBits(
        { name: "HKDF", hash: "SHA-256", salt: s, info: Buffer.from(info) },
        await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]),
        n * 8,
      ),
    );
  const ikm = await hkdf(
    ua.auth,
    secret,
    Buffer.concat([Buffer.from("WebPush: info\0"), ua.p256dh, asPublic]),
    32,
  );
  const cek = await hkdf(salt, ikm, "Content-Encoding: aes128gcm\0", 16);
  const nonce = await hkdf(salt, ikm, "Content-Encoding: nonce\0", 12);
  const key = await crypto.subtle.importKey("raw", cek, "AES-GCM", false, ["decrypt"]);
  const plain = Buffer.from(
    await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: nonce },
      key,
      bytes.subarray(21 + bytes[20]),
    ),
  );
  assert.equal(plain.at(-1), 2);
  return JSON.parse(plain.subarray(0, -1).toString());
}

// Records every outbound fetch and answers each with status(url).
function stubFetch(t, status = () => 201) {
  const saved = globalThis.fetch;
  t.after(() => (globalThis.fetch = saved));
  const sent = [];
  globalThis.fetch = async (url, init) => {
    sent.push({ url, ...init, body: Buffer.from(init.body) });
    return new Response(null, { status: status(url) });
  };
  return sent;
}

function setup({ key = true } = {}) {
  const env = {
    BUCKET: memoryBucket(),
    config: { origin: ORIGIN },
    ...(key && { FACTORY_VAPID_KEY: JSON.stringify(VAPID) }),
  };
  const call = (path, { method = "GET", body, principal = person, headers = {}, ctx } = {}) =>
    serveFactory(
      new Request(
        ORIGIN + path,
        body === undefined
          ? { method, headers }
          : {
              method,
              headers: { "Content-Type": "application/json", ...headers },
              body: JSON.stringify(body),
            },
      ),
      env,
      principal,
      ctx,
    );
  const writeBoard = async (jobs, etag, ctx) =>
    call("/api/board", {
      method: "PUT",
      body: { jobs },
      headers: etag ? { "If-Match": etag } : { "If-None-Match": "*" },
      principal: device,
      ctx,
    });
  return { env, call, writeBoard };
}

test("the key route serves the public key to a person, else 404", async () => {
  const { call } = setup();
  const response = await call("/api/push/key");
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), {
    key: pushKey({ FACTORY_VAPID_KEY: JSON.stringify(VAPID) }),
  });
  assert.equal((await setup({ key: false }).call("/api/push/key")).status, 404);
  assert.equal((await call("/api/push/key", { method: "POST" })).status, 404);
});

test("a board write that makes 042-sample-retry wait sends one signed push per device", async (t) => {
  const sent = stubFetch(t);
  const { env, call, writeBoard } = setup();
  const phones = [
    await browser("https://fcm.googleapis.com/fcm/send/sample-one"),
    await browser("https://web.push.apple.com/sample-two"),
  ];
  for (const phone of phones)
    assert.equal(
      (await call("/api/push/subscriptions", { method: "PUT", body: phone.body })).status,
      204,
    );
  const stored = [...env.BUCKET.objects.keys()].filter((k) => k.startsWith("push/"));
  assert.equal(stored.length, 2);
  for (const k of stored) {
    assert.match(k, /^push\/[0-9a-f]{64}\.json$/);
    assert.deepEqual(Object.keys(JSON.parse(env.BUCKET.objects.get(k).bytes)), [
      "endpoint",
      "keys",
      "created",
    ]);
  }
  const etag = (await writeBoard([RETRY])).headers.get("ETag");
  assert.equal(sent.length, 0);
  const waits = [];
  const response = await writeBoard([RETRY_WAITS], etag, { waitUntil: (p) => waits.push(p) });
  assert.equal(response.status, 200);
  assert.equal(waits.length, 1);
  await Promise.all(waits);
  assert.deepEqual(sent.map((s) => s.url).sort(), phones.map((p) => p.body.endpoint).sort());
  const publicKey = await importJWK({ kty: "EC", crv: "P-256", x: VAPID.x, y: VAPID.y }, "ES256");
  for (const request of sent) {
    const phone = phones.find((p) => p.body.endpoint === request.url);
    const { Authorization, ...rest } = request.headers;
    assert.equal(request.method, "POST");
    assert.deepEqual(rest, {
      TTL: "86400",
      Urgency: "high",
      "Content-Encoding": "aes128gcm",
      "Content-Type": "application/octet-stream",
    });
    const [, jwt, k] = Authorization.match(/^vapid t=([^,]+), k=(.+)$/);
    assert.equal(k, pushKey(env));
    const { payload, protectedHeader } = await jwtVerify(jwt, publicKey, {
      audience: new URL(request.url).origin,
    });
    assert.equal(protectedHeader.alg, "ES256");
    assert.equal(payload.sub, ORIGIN);
    assert.ok(Math.abs(payload.exp - (Date.now() / 1000 + 3600)) < 60);
    assert.deepEqual(await decrypt(phone, request.body), {
      title: "Needs you: 042-sample-retry",
      body: "Synthetic retry job",
      tag: "factory-needs-042-sample-retry",
      url: "/#042-sample-retry",
    });
  }
});

test("a merge and a five-event write push their payloads", async (t) => {
  const sent = stubFetch(t);
  const { call, writeBoard } = setup();
  const phone = await browser("https://updates.push.services.mozilla.com/wpush/v2/sample");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  const pr = row("043-sample-pr", "pr-open", { title: "Synthetic PR job" });
  let etag = (await writeBoard([pr])).headers.get("ETag");
  etag = (await writeBoard([{ ...pr, stage: "merged" }], etag)).headers.get("ETag");
  assert.deepEqual(await decrypt(phone, sent[0].body), {
    title: "Merged: 043-sample-pr",
    body: "Synthetic PR job",
    tag: "factory-merged-043-sample-pr",
    url: "/#043-sample-pr",
  });
  const ids = ["050-a", "051-b", "052-c", "053-d", "054-e"];
  await writeBoard([{ ...pr, stage: "merged" }, ...ids.map((id) => row(id, "needs-human"))], etag);
  assert.equal(sent.length, 2);
  assert.deepEqual(await decrypt(phone, sent[1].body), {
    title: "5 factory updates",
    body: "Needs you: 050-a\nNeeds you: 051-b\nNeeds you: 052-c\nand 2 more",
    tag: "factory-updates",
    url: "/",
  });
});

test("no key, a 412 write or no subscriptions sends nothing and the reply is unchanged", async (t) => {
  const sent = stubFetch(t);
  const replies = [];
  for (const [key, subscribe] of [
    [true, true],
    [false, true],
    [true, false],
  ]) {
    const { call, writeBoard } = setup({ key });
    if (subscribe)
      await call("/api/push/subscriptions", {
        method: "PUT",
        body: (await browser("https://fcm.googleapis.com/fcm/send/sample")).body,
      });
    const etag = (await writeBoard([RETRY])).headers.get("ETag");
    const stale = await writeBoard([RETRY_WAITS], '"stale"');
    assert.equal(stale.status, 412);
    assert.equal(await stale.text(), "precondition failed");
    const response = await writeBoard([RETRY_WAITS], etag);
    replies.push([response.status, await response.text(), response.headers.get("ETag")]);
  }
  assert.equal(sent.length, 1);
  assert.deepEqual(replies[1], replies[0]);
  assert.deepEqual(replies[2], replies[0]);
  assert.equal(replies[0][0], 200);
  assert.equal(replies[0][1], "ok");
});

test("a 410 drops the record, a bad host gets 400, a 21st device 409, a service 404", async (t) => {
  stubFetch(t, (url) => (url.endsWith("/gone") ? 410 : 201));
  const { env, call, writeBoard } = setup();
  const records = () => [...env.BUCKET.objects.keys()].filter((k) => k.startsWith("push/"));
  const put = (body, principal) =>
    call("/api/push/subscriptions", { method: "PUT", body, principal });
  const gone = await browser("https://fcm.googleapis.com/fcm/send/gone");
  const live = await browser("https://fcm.googleapis.com/fcm/send/live");
  await put(gone.body);
  await put(live.body);
  const etag = (await writeBoard([RETRY])).headers.get("ETag");
  await writeBoard([RETRY_WAITS], etag);
  assert.equal(records().length, 1);
  assert.equal(JSON.parse(env.BUCKET.objects.get(records()[0]).bytes).endpoint, live.body.endpoint);

  const bad = await put((await browser("https://evil.example.test/push")).body);
  assert.equal(bad.status, 400);
  assert.equal(await bad.text(), "push endpoint host is not allowed");

  for (let i = 0; records().length < 20; i++)
    assert.equal(
      (await put((await browser(`https://fcm.googleapis.com/fcm/send/d${i}`)).body)).status,
      204,
    );
  const full = await put((await browser("https://fcm.googleapis.com/fcm/send/extra")).body);
  assert.equal(full.status, 409);
  assert.equal(await full.text(), "push device limit reached (20)");
  assert.equal((await put(live.body)).status, 204);
  assert.equal(
    (
      await call("/api/push/subscriptions", {
        method: "DELETE",
        body: { endpoint: live.body.endpoint },
      })
    ).status,
    204,
  );
  assert.equal(records().length, 19);

  const before = records().length;
  for (const [path, method, body] of [
    ["/api/push/key", "GET"],
    ["/api/push/subscriptions", "PUT", live.body],
    ["/api/push/subscriptions", "DELETE", { endpoint: gone.body.endpoint }],
  ])
    assert.equal((await call(path, { method, body, principal: device })).status, 404);
  assert.equal(records().length, before);
});

test("the hourly cron pushes 042-sample-retry once when its claim goes stale", async (t) => {
  const sent = stubFetch(t);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  const claim = { device: "device-a", seen: SEEN };
  await env.BUCKET.put("board.json", JSON.stringify({ jobs: [{ ...RETRY, claim }] }));
  const { scheduled } = (await loadProduction()).default;
  const cronEnv = { ...serverEnv, BUCKET: env.BUCKET, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const run = async (hhmm) => {
    const pending = [];
    scheduled({ scheduledTime: at(hhmm).getTime() }, cronEnv, {
      waitUntil: (p) => pending.push(p),
    });
    await Promise.allSettled(pending);
  };
  await run("10:17");
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, phone.body.endpoint);
  assert.deepEqual(await decrypt(phone, sent[0].body), {
    title: "Stuck: 042-sample-retry",
    body: "Synthetic retry job",
    tag: "factory-stuck-042-sample-retry",
    url: "/#042-sample-retry",
  });
  await run("11:17");
  assert.equal(sent.length, 1);
});

// The archive pass, the idea upload sweep, the spend nudge and the stuck push all read the board
// and the archive; the run reads each once and shares it.
test("the hourly cron reads the board and the archive once for all its passes", async (t) => {
  const sent = stubFetch(t);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  const claim = { device: "device-a", seen: SEEN };
  await env.BUCKET.put("board.json", JSON.stringify({ jobs: [{ ...RETRY, claim }] }));
  await env.BUCKET.put("board-archive.json", JSON.stringify({ jobs: [] }));
  await env.BUCKET.put(
    "pipeline/global.json",
    JSON.stringify({ schema: 1, archive: { mode: "age", n: 30 } }),
  );
  const { scheduled } = (await loadProduction()).default;
  const cronEnv = { ...serverEnv, BUCKET: env.BUCKET, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const pending = [];
  const calls = env.BUCKET.calls.length;
  scheduled({ scheduledTime: at("10:17").getTime() }, cronEnv, {
    waitUntil: (p) => pending.push(p),
  });
  const results = await Promise.allSettled(pending);
  assert.deepEqual(
    results.map((r) => r.status),
    ["fulfilled", "fulfilled", "fulfilled", "fulfilled", "fulfilled"],
  );
  const gets = env.BUCKET.calls
    .slice(calls)
    .filter(([op, key]) => op === "get" && ["board.json", "board-archive.json"].includes(key))
    .map(([, key]) => key);
  assert.deepEqual(gets, ["board.json", "board-archive.json"]);
  assert.equal(sent.length, 1);
});

// A board read that throws fails the archive pass and the idea upload sweep, each with a
// warning, rather than leaving a rejected promise.
test("the hourly cron logs a failed board read in the archive pass and the idea sweep", async (t) => {
  const warn = t.mock.method(console, "warn", () => {});
  const bucket = memoryBucket();
  await bucket.put(
    "pipeline/global.json",
    JSON.stringify({ schema: 1, archive: { mode: "age", n: 30 } }),
  );
  const get = bucket.get;
  bucket.get = (key, options) => {
    if (key === "board.json") throw new Error("board read failed");
    return get(key, options);
  };
  const { scheduled } = (await loadProduction()).default;
  const pending = [];
  scheduled(
    { scheduledTime: at("10:17").getTime() },
    { ...serverEnv, BUCKET: bucket },
    {
      waitUntil: (p) => pending.push(p),
    },
  );
  const results = await Promise.allSettled(pending);
  assert.deepEqual(
    results.map((r) => r.status),
    ["fulfilled", "fulfilled", "fulfilled", "fulfilled", "fulfilled"],
  );
  assert.deepEqual(warn.mock.calls.map((c) => c.arguments[0]).sort(), [
    "archive pass failed: board read failed",
    "idea upload sweep failed: board read failed",
  ]);
});

test("the hourly cron still pushes 042-sample-retry when the spend pass throws", async (t) => {
  const sent = stubFetch(t);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  const claim = { device: "device-a", seen: SEEN };
  await env.BUCKET.put("board.json", JSON.stringify({ jobs: [{ ...RETRY, claim }] }));
  const bucket = {
    ...env.BUCKET,
    list: (options) =>
      options?.prefix === "projects/"
        ? Promise.reject(new Error("R2 list failed"))
        : env.BUCKET.list(options),
  };
  const warn = t.mock.method(console, "warn", () => {});
  const { scheduled } = (await loadProduction()).default;
  const cronEnv = { ...serverEnv, BUCKET: bucket, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const pending = [];
  scheduled({ scheduledTime: at("10:17").getTime() }, cronEnv, {
    waitUntil: (p) => pending.push(p),
  });
  await Promise.allSettled(pending);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, phone.body.endpoint);
  assert.deepEqual(
    warn.mock.calls.map((c) => c.arguments.join(" ")),
    ["spend nudge pass failed: R2 list failed"],
  );
});

test("the hourly cron logs a non-Error spend failure as its value", async (t) => {
  const sent = stubFetch(t);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  const claim = { device: "device-a", seen: SEEN };
  await env.BUCKET.put("board.json", JSON.stringify({ jobs: [{ ...RETRY, claim }] }));
  const bucket = {
    ...env.BUCKET,
    list: (options) =>
      options?.prefix === "projects/" ? Promise.reject("R2 list failed") : env.BUCKET.list(options),
  };
  const warn = t.mock.method(console, "warn", () => {});
  const { scheduled } = (await loadProduction()).default;
  const cronEnv = { ...serverEnv, BUCKET: bucket, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const pending = [];
  scheduled({ scheduledTime: at("10:17").getTime() }, cronEnv, {
    waitUntil: (p) => pending.push(p),
  });
  await Promise.allSettled(pending);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, phone.body.endpoint);
  assert.deepEqual(
    warn.mock.calls.map((c) => c.arguments.join(" ")),
    ["spend nudge pass failed: R2 list failed"],
  );
});

test("the hourly cron pushes 042-sample-retry once when its wait goes stale", async (t) => {
  const sent = stubFetch(t);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  const waiting = { ...RETRY_WAITS, updated: DAY_BEFORE };
  await env.BUCKET.put("board.json", JSON.stringify({ jobs: [waiting] }));
  const { scheduled } = (await loadProduction()).default;
  const cronEnv = { ...serverEnv, BUCKET: env.BUCKET, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const run = async (hhmm) => {
    const pending = [];
    scheduled({ scheduledTime: at(hhmm).getTime() }, cronEnv, {
      waitUntil: (p) => pending.push(p),
    });
    await Promise.allSettled(pending);
  };
  await run("06:17");
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, phone.body.endpoint);
  assert.deepEqual(await decrypt(phone, sent[0].body), {
    title: "Still needs you: 042-sample-retry",
    body: "Synthetic retry job",
    tag: "factory-stale-042-sample-retry",
    url: "/#042-sample-retry",
  });
  await run("07:17");
  assert.equal(sent.length, 1);

  const claim = { device: "device-a", seen: "2026-11-17T01:40:00Z" };
  const stuck = row("060-sample-unclaimed", "build", { claim });
  await env.BUCKET.put("board.json", JSON.stringify({ jobs: [stuck, waiting] }));
  await run("06:17");
  assert.equal(sent.length, 2);
  assert.deepEqual(await decrypt(phone, sent[1].body), {
    title: "2 factory updates",
    body: "Stuck: 060-sample-unclaimed\nStill needs you: 042-sample-retry",
    tag: "factory-updates",
    url: "/",
  });
});

// Board row 070-sample-spent at 80% of sample's weekly_tokens, with 50% already sent.
async function seedSpend(env) {
  const spent = row("070-sample-spent", "merged", {
    project: "sample",
    done: "2026-11-17T01:00:00Z",
    tokens: { mac: { models: { "model-a": { output: 800000 } } } },
  });
  await env.BUCKET.put("board.json", JSON.stringify({ jobs: [spent] }));
  await env.BUCKET.put(
    "projects/sample.md",
    "---\nrepo: example/sample\nbase: main\ntitle: Sample\nweekly_tokens: 1000000\n---\n",
  );
  await env.BUCKET.put(
    "spend-nudges.json",
    JSON.stringify({
      schema: "factory.spend-nudges/1",
      week: "2026-11-16",
      sent: { sample: [50] },
    }),
  );
}

test("the hourly cron pushes Spend 80%: sample once and writes nothing without a VAPID key", async (t) => {
  const sent = stubFetch(t);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  await seedSpend(env);
  const { scheduled } = (await loadProduction()).default;
  const run = async (hhmm, cronEnv) => {
    const pending = [];
    scheduled({ scheduledTime: at(hhmm).getTime() }, cronEnv, {
      waitUntil: (p) => pending.push(p),
    });
    await Promise.allSettled(pending);
  };
  const nudges = async () => JSON.parse(await (await env.BUCKET.get("spend-nudges.json")).text());

  await run("12:17", { ...serverEnv, BUCKET: env.BUCKET });
  assert.equal(sent.length, 0);
  assert.deepEqual((await nudges()).sent, { sample: [50] });

  const cronEnv = { ...serverEnv, BUCKET: env.BUCKET, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const start = performance.now();
  await run("12:17", cronEnv);
  console.log(`hourly cron with spend: ${(performance.now() - start).toFixed(1)} ms`);
  assert.equal(sent.length, 1);
  assert.deepEqual(await decrypt(phone, sent[0].body), {
    title: "Spend 80%: sample",
    body: "800,000 of 1,000,000 expected weekly tokens (80%), week of 2026-11-16",
    tag: "factory-spend-sample",
    url: "/#/retros",
  });
  assert.deepEqual(await nudges(), {
    schema: "factory.spend-nudges/1",
    week: "2026-11-16",
    sent: { sample: [50, 80] },
  });
  await run("13:17", cronEnv);
  assert.equal(sent.length, 1);
});

test("the hourly cron records Spend 80%: sample only after a push is accepted", async (t) => {
  let status = 500;
  const sent = stubFetch(t, () => status);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  await seedSpend(env);
  const { scheduled } = (await loadProduction()).default;
  const cronEnv = { ...serverEnv, BUCKET: env.BUCKET, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const run = async (hhmm) => {
    const pending = [];
    scheduled({ scheduledTime: at(hhmm).getTime() }, cronEnv, {
      waitUntil: (p) => pending.push(p),
    });
    await Promise.allSettled(pending);
  };
  const nudges = async () => JSON.parse(await (await env.BUCKET.get("spend-nudges.json")).text());

  await run("12:17");
  assert.equal(sent.length, 1);
  assert.deepEqual((await nudges()).sent, { sample: [50] });

  status = 201;
  await run("13:17");
  assert.equal(sent.length, 2);
  assert.equal((await decrypt(phone, sent[1].body)).title, "Spend 80%: sample");
  assert.deepEqual((await nudges()).sent, { sample: [50, 80] });

  await run("14:17");
  assert.equal(sent.length, 2);
});

test("the hourly cron keeps Spend 80%: sample unrecorded when the subscription list fails", async (t) => {
  const sent = stubFetch(t);
  const { env, call } = setup();
  const phone = await browser("https://fcm.googleapis.com/fcm/send/sample-cron");
  await call("/api/push/subscriptions", { method: "PUT", body: phone.body });
  await seedSpend(env);
  const bucket = {
    ...env.BUCKET,
    list: (options) =>
      options?.prefix === "push/"
        ? Promise.reject(new Error("R2 list failed"))
        : env.BUCKET.list(options),
  };
  const { scheduled } = (await loadProduction()).default;
  const cronEnv = { ...serverEnv, BUCKET: bucket, FACTORY_VAPID_KEY: env.FACTORY_VAPID_KEY };
  const pending = [];
  scheduled({ scheduledTime: at("12:17").getTime() }, cronEnv, {
    waitUntil: (p) => pending.push(p),
  });
  await Promise.allSettled(pending);
  assert.equal(sent.length, 0);
  const nudges = JSON.parse(await (await env.BUCKET.get("spend-nudges.json")).text());
  assert.deepEqual(nudges.sent, { sample: [50] });
});
