import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const mac = { verified: true, kind: "service", client_id: "client-mac" };
const pc = { verified: true, kind: "service", client_id: "client-pc" };
const unbound = { verified: true, kind: "service", client_id: "client-other" };
const anonymousService = { verified: true, kind: "service", client_id: null };
const DATE = "2026-11-11";
const row = (host, extra = {}) => JSON.stringify({ host, session: "s1", job: "1017", ...extra });

async function setup() {
  const bucket = memoryBucket();
  const send = async (method, path, { body, principal = person, type } = {}) => {
    const init = { method, headers: type ? { "Content-Type": type } : {} };
    if (body !== undefined) init.body = body;
    const response = await serveFactory(
      new Request("http://local" + path, init),
      { BUCKET: bucket },
      principal,
    );
    return { status: response.status, text: await response.text(), headers: response.headers };
  };
  for (const [id, principal] of [
    ["mac", mac],
    ["pc", pc],
    ["idle", { ...pc, client_id: "client-idle" }],
  ]) {
    const created = await send("POST", "/api/hosts", {
      body: JSON.stringify({ id, label: id }),
      type: "application/json",
    });
    const { code } = JSON.parse(created.text);
    const paired = await send("POST", `/api/hosts/${id}/pair`, {
      body: JSON.stringify({ code }),
      type: "application/json",
      principal,
    });
    assert.equal(paired.status, 200);
  }
  const put = (host, body, principal = mac, date = DATE) =>
    send("PUT", `/api/retro/${host}/${date}.jsonl`, {
      body,
      principal,
      type: "application/x-ndjson",
    });
  const retroKeys = () => [...bucket.objects.keys()].filter((k) => k.startsWith("retro/"));
  return { bucket, send, put, retroKeys };
}

test("a bound host stores its digest and any device reads it back", async () => {
  const s = await setup();
  const macBody = `${row("mac")}\n${row("mac", { session: "s2" })}\n`;
  let r = await s.put("mac", macBody);
  assert.deepEqual([r.status, r.text], [200, "ok"]);
  assert.equal((await s.put("pc", `${row("pc")}\n`, pc)).status, 200);
  assert.equal((await s.put("mac", "", mac, "2026-11-10")).status, 200);
  const stored = s.bucket.objects.get(`retro/mac/${DATE}.jsonl`);
  assert.equal(stored.bytes.toString(), macBody);
  assert.equal(stored.httpMetadata.contentType, "application/x-ndjson");
  for (const principal of [mac, pc, unbound, anonymousService]) {
    r = await s.send("GET", `/api/retro/mac/${DATE}.jsonl`, { principal });
    assert.deepEqual([r.status, r.text], [200, macBody]);
    r = await s.send("GET", `/api/retro/${DATE}`, { principal });
    assert.deepEqual([r.status, JSON.parse(r.text)], [200, ["mac", "pc"]]);
  }
  r = await s.send("GET", "/api/retro/2026-11-10", { principal: pc });
  assert.deepEqual(JSON.parse(r.text), ["mac"]);
  r = await s.send("GET", "/api/retro/2026-11-09", { principal: pc });
  assert.deepEqual(JSON.parse(r.text), []);
  assert.equal(
    (await s.send("GET", `/api/retro/idle/${DATE}.jsonl`, { principal: pc })).status,
    404,
  );
  const again = `${row("mac", { session: "s3" })}\n`;
  assert.equal((await s.put("mac", again)).status, 200);
  assert.equal(s.bucket.objects.get(`retro/mac/${DATE}.jsonl`).bytes.toString(), again);
});

test("refused writes and reads store nothing", async () => {
  const s = await setup();
  const body = `${row("mac")}\n`;
  for (const principal of [person, unbound, anonymousService, pc]) {
    const r = await s.put("mac", body, principal);
    assert.deepEqual(
      [r.status, r.text],
      [403, "retro digests are written only by the host's bound service token"],
    );
  }
  assert.equal((await s.put("ghost", `${row("ghost")}\n`)).status, 403);
  for (const path of [`/api/retro/mac/${DATE}.jsonl`, `/api/retro/${DATE}`]) {
    const r = await s.send("GET", path, { principal: person });
    assert.deepEqual(
      [r.status, r.text],
      [403, "retro digests are read only by a device service token"],
    );
  }
  assert.equal((await s.put("mac", `${row("mac", { pad: "x".repeat(1 << 20) })}\n`)).status, 413);
  for (const bad of [
    `${row("pc")}\n`,
    `${row("mac")}\nnot json\n`,
    `${row("mac")}\n\n`,
    "[1]\n",
    "null\n",
    `${row("mac")}\n${JSON.stringify({ session: "s1" })}\n`,
    Buffer.from([0xff, 0x0a]),
  ])
    assert.equal((await s.put("mac", bad)).status, 400, String(bad));
  for (const path of [
    "/api/retro/mac/2026-02-30.jsonl",
    "/api/retro/mac/2026-13-01.jsonl",
    "/api/retro/mac/26-11-11.jsonl",
    "/api/retro/-mac/2026-11-11.jsonl",
    `/api/retro/${"m".repeat(65)}/2026-11-11.jsonl`,
    "/api/retro/2026-02-30",
  ]) {
    for (const method of ["GET", "PUT"]) {
      const r = await s.send(method, path, {
        principal: mac,
        body: method === "PUT" ? body : undefined,
      });
      assert.equal(r.status, 404, `${method} ${path}`);
    }
  }
  assert.equal(
    (await s.send("DELETE", `/api/retro/mac/${DATE}.jsonl`, { principal: mac })).status,
    404,
  );
  assert.deepEqual(s.retroKeys(), []);
});
