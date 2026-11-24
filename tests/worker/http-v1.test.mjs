// Actual HTTP v1 drills against the local Worker, with device flows through the factory binary:
// concurrent claims, conditional registration, job write grammar, weak ETags, idea-image
// idempotence and two-device job/project exit-6 reconciliation. Synthetic Git checkouts and
// temporary homes only. Two devices' claims meet at a barrier proxy, so both read the same board.
//
// The cancelled slim read with a waiter runs against direct Miniflare on 8222, because wrangler
// dev never passes a client reset on to the worker. Its reader asks for a delayed R2 get so the
// cancel has a timer to kill, and a second reader running beside it still answers.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { crc32, deflateSync } from "node:zlib";
import { barrierProxy, factory, makeHome, request, strong, testUrl } from "./local-http.mjs";

// A real 1x1 RGBA PNG.
function pngBytes() {
  const chunk = (kind, data) => {
    const head = Buffer.alloc(4);
    head.writeUInt32BE(data.length);
    const tail = Buffer.alloc(4);
    tail.writeUInt32BE(crc32(Buffer.concat([Buffer.from(kind), data])));
    return Buffer.concat([head, Buffer.from(kind), data, tail]);
  };
  const header = Buffer.from([0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
  return Buffer.concat([
    Buffer.from("\x89PNG\r\n\x1a\n", "latin1"),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.from([0, 0, 0, 0, 0xff]))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const PNG = pngBytes();
const PNG_NAME = createHash("sha256").update(PNG).digest("hex") + ".png";
const JSON_TYPE = { "Content-Type": "application/json" };
const MD_TYPE = { "Content-Type": "text/markdown" };

test("the target guard refuses every other origin before any request", (t) => {
  const saved = process.env.FACTORY_TEST_URL;
  t.after(() => {
    if (saved === undefined) delete process.env.FACTORY_TEST_URL;
    else process.env.FACTORY_TEST_URL = saved;
  });
  t.mock.method(globalThis, "fetch");
  for (const url of [
    "https://factory.example.test",
    "http://localhost:8220",
    "http://127.0.0.1:8220/",
    "http://127.0.0.1:8210",
    "http://user@127.0.0.1:8220",
  ]) {
    process.env.FACTORY_TEST_URL = url;
    assert.throws(testUrl, /owned loopback origin/, url);
  }
  assert.equal(globalThis.fetch.mock.callCount(), 0);
});

const url = testUrl();

describe("HTTP v1", () => {
  // Each test skips on its own, so a run without the URL counts them as skipped.
  const skip = url ? false : "set FACTORY_TEST_URL";
  const req = (path, options) => request(url, path, options);
  let dir;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), "http-v1-"));
  });

  afterEach(() => rmSync(dir, { recursive: true, force: true }));

  function gitRepo(name) {
    const repo = join(dir, name);
    mkdirSync(repo);
    const env = {
      ...process.env,
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_AUTHOR_NAME: "Test",
      GIT_AUTHOR_EMAIL: "test@example.invalid",
      GIT_COMMITTER_NAME: "Test",
      GIT_COMMITTER_EMAIL: "test@example.invalid",
    };
    const git = (...args) => execFileSync("git", args, { cwd: repo, env, stdio: "pipe" });
    git("init");
    git("symbolic-ref", "HEAD", "refs/heads/main");
    writeFileSync(join(repo, "README.md"), "# synthetic checkout\n");
    git("add", "README.md");
    git("commit", "-m", "init");
    return repo;
  }

  const gitState = (repo) =>
    ["status --porcelain", "rev-parse HEAD"].map((args) =>
      execFileSync("git", args.split(" "), { cwd: repo, encoding: "utf8" }),
    );

  test("concurrent board claims", { skip }, async () => {
    const proxy = await barrierProxy(url, "GET /api/board");
    let ids;
    try {
      const homes = ["claim-a", "claim-b"].map((name) => makeHome(dir, name, proxy.origin));
      const runs = await Promise.all(
        homes.map((h) => factory(h, "claim", h, "synthetic-claim", "sample")),
      );
      for (const run of runs) assert.equal(run.code, 0, run.stderr);
      ids = runs.map((run) => run.stdout.trim());
      assert.equal(new Set(ids).size, 2, "concurrent claims get distinct ids");
      for (const [i, h] of homes.entries())
        assert.ok(statSync(join(h, "jobs", "sample", ids[i])).isDirectory());
    } finally {
      await proxy.close();
    }
    const board = await req("/api/board");
    assert.equal(board.status, 200);
    const claimed = JSON.parse(board.body).jobs.filter((j) => ids.includes(j.id));
    assert.equal(claimed.length, 2);
    for (const job of claimed) {
      assert.equal(job.project, "sample");
      assert.ok(!("stage" in job), "a claim stub carries no stage");
      assert.ok(job.claim?.device);
    }
  });

  test("concurrent slim reads all answer", { skip }, async () => {
    const board = await req("/api/board");
    let status;
    if (board.status === 404) {
      ({ status } = await req("/api/board", {
        method: "PUT",
        body: '{"jobs": []}',
        headers: { ...JSON_TYPE, "If-None-Match": "*" },
      }));
    } else {
      ({ status } = await req("/api/board", {
        method: "PUT",
        body: JSON.stringify(JSON.parse(board.body), null, 1) + "\n",
        headers: { ...JSON_TYPE, "If-Match": board.headers.get("ETag") },
      }));
    }
    assert.ok([200, 201].includes(status));
    const replies = await Promise.all(
      Array.from({ length: 8 }, () => req("/board.json?view=slim")),
    );
    assert.deepEqual(
      replies.map((r) => r.status),
      Array(8).fill(200),
    );
    assert.equal(new Set(replies.map((r) => r.body.toString("latin1"))).size, 1);
  });

  test("idea image idempotence", { skip }, async () => {
    const path = "/api/idea-images/" + PNG_NAME;
    const png = { method: "PUT", body: PNG, headers: { "Content-Type": "image/png" } };
    assert.equal((await req(path, png)).status, 200, "first PUT creates the object");
    const before = await req(path);
    assert.equal(
      (await req(path, png)).status,
      200,
      "second PUT is a create-only no-op on success",
    );
    const after = await req(path);
    assert.deepEqual(after.body, before.body, "stored bytes unchanged across no-op PUT");
    assert.deepEqual(after.body, PNG, "stored bytes are the bytes we PUT");
    assert.equal(
      after.headers.get("ETag"),
      before.headers.get("ETag"),
      "stored ETag is stable across no-op PUT (read from GET, not PUT)",
    );
    assert.equal(after.headers.get("Content-Security-Policy"), "default-src 'none'; sandbox");
    const mismatched = await req("/api/idea-images/" + "b".repeat(64) + ".png", png);
    assert.equal(mismatched.status, 400, "digest must match the file name");
  });

  test("job optional and conditional writes and weak ETags", { skip }, async () => {
    const path = "/api/jobs/sample/001-http/build.md";
    const put = (body, condition) =>
      req(path, { method: "PUT", body, headers: { ...MD_TYPE, ...condition } });
    let r = await put("blind v1\n");
    assert.equal(r.status, 200, "legacy blind job write remains allowed");
    const tag = strong(r.headers.get("ETag"));
    r = await req(path);
    assert.equal(r.status, 200);
    assert.equal(r.body.toString(), "blind v1\n");
    r = await put("weak v2\n", { "If-Match": "W/" + tag });
    assert.equal(r.status, 200, "edge-weakened W/ ETag is accepted");
    assert.notEqual(strong(r.headers.get("ETag")), tag);
    assert.equal(
      (await put("stale\n", { "If-Match": tag })).status,
      412,
      "stale condition is refused",
    );
    assert.equal((await req(path)).body.toString(), "weak v2\n");
    assert.equal(
      (await put("bad\n", { "If-Match": "garbage" })).status,
      428,
      "malformed present condition keeps the 428 grammar",
    );
    assert.equal(
      (await put("create-only\n", { "If-None-Match": "*" })).status,
      412,
      "create-only on an existing job file is refused",
    );
    assert.equal((await req(path)).body.toString(), "weak v2\n");
  });

  test("project conditional registration race", { skip }, async () => {
    const path = "/api/projects/http-sample.md";
    assert.equal(
      (await req(path)).status,
      404,
      "complete runs require fresh job-local persistence",
    );
    const doc = "---\nrepo: example/http-sample\nbase: main\ntitle: HTTP sample\n---\n";
    const markers = ["A\n", "B\n"];
    const results = await Promise.all(
      markers.map((marker) =>
        req(path, {
          method: "PUT",
          body: doc + marker,
          headers: { ...MD_TYPE, "If-None-Match": "*" },
        }),
      ),
    );
    assert.deepEqual(results.map((r) => r.status).sort(), [200, 412]);
    const winner = results.findIndex((r) => r.status === 200);
    const got = await req(path);
    assert.equal(got.status, 200);
    assert.equal(got.body.toString(), doc + markers[winner]);
    assert.equal(strong(got.headers.get("ETag")), strong(results[winner].headers.get("ETag")));
  });

  test("two-device job exit 6 and reconciliation", { skip }, async () => {
    const repos = [gitRepo("repo-a"), gitRepo("repo-b")];
    const before = repos.map(gitState);
    const homes = ["job-a", "job-b"].map((name, i) =>
      makeHome(dir, name, url, { sample: repos[i] }),
    );
    const target = "sample/002-http";
    const remotePath = "/api/jobs/sample/002-http/build.md";
    const ok = async (h, ...args) => {
      const run = await factory(h, ...args);
      assert.equal(run.code, 0, run.stderr);
    };
    for (const h of homes) await ok(h, "pull", target); // no files yet on either device
    const [folderA, folderB] = homes.map((h) => join(h, "jobs", "sample", "002-http"));
    mkdirSync(folderA, { recursive: true });
    writeFileSync(join(folderA, "build.md"), "device A build\n");
    await ok(homes[0], "push", target);
    let r = await req(remotePath);
    assert.equal(r.status, 200);
    assert.equal(r.body.toString(), "device A build\n");
    const winnerTag = strong((await req(remotePath)).headers.get("ETag"));
    await ok(homes[1], "pull", target);
    assert.equal(readFileSync(join(folderB, "build.md"), "utf8"), "device A build\n");
    writeFileSync(join(folderB, "build.md"), "device B build\n");
    writeFileSync(join(folderA, "build.md"), "device A build v2\n");
    await ok(homes[0], "push", target);
    assert.equal((await req(remotePath)).body.toString(), "device A build v2\n");
    assert.equal((await factory(homes[1], "push", target)).code, 6);
    assert.equal(
      readFileSync(join(folderB, "build.md"), "utf8"),
      "device B build\n",
      "local edit kept",
    );
    const remoteCopy = join(folderB, "build.md.remote");
    assert.equal(
      readFileSync(remoteCopy, "utf8"),
      "device A build v2\n",
      "R2 winner saved beside the file",
    );
    assert.equal((await req(remotePath + ".remote")).status, 404, ".remote is never uploaded");
    assert.equal(
      (await req("/api/jobs/sample/002-http/.sync.json")).status,
      404,
      "the sync manifest is never uploaded",
    );
    // Explicit reconciliation: adopt the local merge, drop the marker, push exactly that.
    writeFileSync(join(folderB, "build.md"), "device B merged\n");
    unlinkSync(remoteCopy);
    await ok(homes[1], "pull", target);
    await ok(homes[1], "push", target);
    r = await req(remotePath);
    assert.equal(r.body.toString(), "device B merged\n");
    assert.notEqual(strong(r.headers.get("ETag")), winnerTag);
    for (const [i, repo] of repos.entries())
      assert.deepEqual(gitState(repo), before[i], "claims/sync never touch Git checkouts");
  });

  test("two-device project exit 6 and reconciliation", { skip }, async () => {
    const path = "/api/projects/http-devices.md";
    const doc = "---\nrepo: example/http-devices\nbase: main\ntitle: HTTP devices\n---\n";
    const created = await req(path, {
      method: "PUT",
      body: doc,
      headers: { ...MD_TYPE, "If-None-Match": "*" },
    });
    assert.equal(created.status, 200);
    const homes = ["proj-a", "proj-b"].map((name) =>
      makeHome(dir, name, url, { "http-devices": join(dir, name) }),
    );
    for (const h of homes) assert.equal((await factory(h, "pull", "http-devices")).code, 0);
    const docs = homes.map((h) => join(h, "projects", "http-devices.md"));
    writeFileSync(docs[0], doc + "A\n");
    writeFileSync(docs[1], doc + "B\n");
    assert.equal((await factory(homes[0], "push", "http-devices")).code, 0);
    assert.equal((await factory(homes[1], "push", "http-devices")).code, 6);
    assert.equal(readFileSync(docs[1], "utf8"), doc + "B\n", "loser keeps its local edit");
    assert.equal((await factory(homes[1], "pull", "http-devices")).code, 6);
    const remote = join(homes[1], "projects", "http-devices.md.remote");
    assert.equal(readFileSync(remote, "utf8"), doc + "A\n", "winner bytes saved as .remote");
    assert.equal((await req(path)).body.toString(), doc + "A\n");
    assert.equal((await req(path + ".remote")).status, 404, ".remote is never uploaded");
    writeFileSync(docs[1], doc + "merged\n");
    unlinkSync(remote);
    assert.equal((await factory(homes[1], "push", "http-devices")).code, 0);
    const final = await req(path);
    assert.equal(final.body.toString(), doc + "merged\n");
    assert.ok(strong(final.headers.get("ETag")));
  });
});

const CANCEL_OWNED = "http://127.0.0.1:8222";
const cancelUrl = process.env.FACTORY_CANCEL_TEST_URL;
if (cancelUrl !== undefined && cancelUrl !== CANCEL_OWNED)
  throw new Error("FACTORY_CANCEL_TEST_URL must be the owned loopback origin " + CANCEL_OWNED);

describe("cancel drill", () => {
  const skip = cancelUrl ? false : "set FACTORY_CANCEL_TEST_URL for the Miniflare cancel drill";
  const req = (path, options) => request(cancelUrl, path, options);

  // A slim read that asks for a delayed R2 get, then resets its connection after `delay` ms.
  function abortSlim(delay) {
    const { hostname, port, host } = new URL(cancelUrl);
    return new Promise((resolve, reject) => {
      const socket = connect(Number(port), hostname, () => {
        socket.write(
          `GET /board.json?view=slim HTTP/1.1\r\nHost: ${host}\r\nX-Factory-Test-Read-Delay: 300\r\n\r\n`,
        );
        setTimeout(() => {
          socket.resetAndDestroy();
          resolve();
        }, delay);
      });
      socket.once("error", reject);
    });
  }

  test("a cancelled slim read leaves a second reader answered", { skip }, async (t) => {
    let { status, headers, body: original } = await req("/api/board");
    if (status === 404) {
      original = Buffer.from('{"jobs": []}');
      ({ status, headers } = await req("/api/board", {
        method: "PUT",
        body: original,
        headers: { ...JSON_TYPE, "If-None-Match": "*" },
      }));
    }
    assert.ok([200, 201].includes(status));
    const data = JSON.parse(original);
    let etag = headers.get("ETag");
    t.after(async () => {
      const restored = await req("/api/board", {
        method: "PUT",
        body: original,
        headers: { ...JSON_TYPE, "If-Match": etag },
      });
      assert.equal(restored.status, 200);
    });
    for (let n = 0; n < 3; n++) {
      // An 8 MiB board, so the slim read has real R2 bytes to stream.
      const padded = { ...data, drillPad: Array(8192).fill("x".repeat(1024)), drillRound: n };
      const written = await req("/api/board", {
        method: "PUT",
        body: JSON.stringify(padded),
        headers: { ...JSON_TYPE, "If-Match": etag },
      });
      assert.equal(written.status, 200);
      etag = written.headers.get("ETag");
      const aborter = abortSlim(100);
      await sleep(50);
      let slim;
      try {
        slim = await req("/board.json?view=slim", { timeout: 10_000 });
      } catch {
        assert.fail(`round ${n}: second reader got no answer in 10 s`);
      }
      await aborter;
      assert.equal(slim.status, 200, `round ${n}`);
      assert.equal(strong(slim.headers.get("ETag")), strong(etag));
      const parsed = JSON.parse(slim.body);
      assert.equal(parsed.drillPad.length, 8192);
      assert.ok("slim" in parsed);
      assert.deepEqual((await req("/board.json?view=slim")).body, slim.body);
    }
  });
});
