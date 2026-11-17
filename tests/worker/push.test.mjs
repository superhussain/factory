// Web Push: the RFC 8291 encryption vector, the board diff and payloads, subscription
// routes, and one encrypted, VAPID-signed POST per subscription after a committed board write.
import assert from "node:assert/strict";
import test from "node:test";
import {
  boardPushes,
  encryptWith,
  pushKey,
  pushMessage,
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
