import assert from "node:assert/strict";
import test from "node:test";
import { loadProduction, memoryBucket } from "../support/worker.mjs";

const { serveFactory } = await loadProduction();
const person = { verified: true, kind: "user", email: "person@example.test" };
const mac = { verified: true, kind: "service", client_id: "client-mac" };
const pc = { verified: true, kind: "service", client_id: "client-pc" };
const unbound = { verified: true, kind: "service", client_id: "client-other" };
const JOB = "1017-add-x";
const row = (host, extra = {}) =>
  JSON.stringify({ host, job: JOB, session: "3f2a9c1d", agent: "main", ...extra });

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
  const put = (host, body, principal = mac, job = JOB) =>
    send("PUT", `/api/audit/${job}/${host}.jsonl`, {
      body,
      principal,
      type: "application/x-ndjson",
    });
  const auditKeys = () => [...bucket.objects.keys()].filter((k) => k.startsWith("audit/"));
  return { bucket, send, put, auditKeys };
}

test("each bound host stores its rows and any principal reads them merged", async () => {
  const s = await setup();
  let r = await s.send("GET", `/api/audit/${JOB}`);
  assert.deepEqual([r.status, r.text], [200, ""]);
  const macBody = `${row("mac")}\n${row("mac", { session: "s2" })}\n`;
  r = await s.put("mac", macBody);
  assert.deepEqual([r.status, r.text], [200, "ok"]);
  assert.equal((await s.put("pc", `${row("pc")}\n`, pc)).status, 200);
  const stored = s.bucket.objects.get(`audit/${JOB}/mac.jsonl`);
  assert.equal(stored.bytes.toString(), macBody);
  assert.equal(stored.httpMetadata.contentType, "application/x-ndjson");
  for (const principal of [person, mac, unbound]) {
    r = await s.send("GET", `/api/audit/${JOB}`, { principal });
    assert.deepEqual([r.status, r.text], [200, `${macBody}${row("pc")}\n`]);
    assert.equal(r.headers.get("Content-Type"), "application/x-ndjson; charset=utf-8");
  }
  const again = `${row("mac", { session: "s3" })}\n`;
  assert.equal((await s.put("mac", again)).status, 200);
  r = await s.send("GET", `/api/audit/${JOB}`);
  assert.equal(r.text, `${again}${row("pc")}\n`);
  assert.equal((await s.send("GET", "/api/audit/1018-other")).text, "");
});

test("refused writes store nothing", async () => {
  const s = await setup();
  const body = `${row("mac")}\n`;
  for (const principal of [person, unbound, pc]) {
    const r = await s.put("mac", body, principal);
    assert.deepEqual(
      [r.status, r.text],
      [403, "audit rows are written only by the host's bound service token"],
    );
  }
  assert.equal((await s.put("mac", `${row("mac", { pad: "x".repeat(1 << 20) })}\n`)).status, 413);
  let r = await s.put("mac", Buffer.from([0xff, 0x0a]));
  assert.deepEqual([r.status, r.text], [400, "audit rows must be valid UTF-8"]);
  r = await s.put("mac", `${row("mac")}\n${row("pc")}\n`);
  assert.deepEqual(
    [r.status, r.text],
    [400, `line 2 is not a JSON object with host mac and job ${JOB}`],
  );
  for (const bad of [
    `${row("mac", { job: "1018-other" })}\n`,
    `${row("mac")}\nnot json\n`,
    `${row("mac")}\n\n`,
    "[1]\n",
    "null\n",
  ])
    assert.equal((await s.put("mac", bad)).status, 400, String(bad));
  for (const path of [
    "/api/audit/1017/mac.jsonl",
    "/api/audit/1017-Add-x/mac.jsonl",
    "/api/audit/x-1017/mac.jsonl",
    "/api/audit/1017-add-/mac.jsonl",
    `/api/audit/${JOB}/-mac.jsonl`,
    `/api/audit/${JOB}/${"m".repeat(65)}.jsonl`,
    "/api/audit/1017",
    "/api/audit/1017-add-x-",
  ]) {
    for (const method of ["GET", "PUT"]) {
      const r = await s.send(method, path, {
        principal: mac,
        body: method === "PUT" ? body : undefined,
      });
      assert.equal(r.status, 404, `${method} ${path}`);
    }
  }
  for (const [method, path] of [
    ["DELETE", `/api/audit/${JOB}/mac.jsonl`],
    ["GET", `/api/audit/${JOB}/mac.jsonl`],
    ["PUT", `/api/audit/${JOB}`],
    ["DELETE", `/api/audit/${JOB}`],
  ])
    assert.equal((await s.send(method, path, { principal: mac })).status, 404, `${method} ${path}`);
  assert.deepEqual(s.auditKeys(), []);
});
