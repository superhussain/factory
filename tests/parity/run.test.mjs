// Self-tests for the parity runner and its tool stub.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { expand, httpFixture, loadCases, readIds, requestLines, runCase, runSide } from "./run.mjs";
import { launcher } from "./stub.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const RUN = join(HERE, "run.mjs");
const BIN = process.env.FACTORY_BIN || resolve(HERE, "../../rust/target/host/factory");
const noBin = !existsSync(BIN) && `no factory binary at ${BIN}`;
const posix = process.platform === "win32" && "stub launchers and the runner are POSIX only";

function temp(t) {
  const dir = mkdtempSync(join(tmpdir(), "parity-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function script(path, body) {
  writeFileSync(path, body);
  chmodSync(path, 0o755);
  return path;
}

// run.mjs's main in its own process: [exit code, stdout, stderr].
function run(args, env = {}) {
  const base = { ...process.env };
  delete base.FACTORY_BIN;
  const r = spawnSync(process.execPath, [RUN, ...args], {
    env: { ...base, ...env },
    encoding: "utf8",
    timeout: 120_000,
  });
  return [r.status, r.stdout, r.stderr];
}

const caseById = (id) => loadCases().find((c) => c.id === id);

test("the stub replays, logs and rejects unrecorded calls", { skip: posix }, (t) => {
  const dir = temp(t);
  const [bin, stub] = [join(dir, "bin"), join(dir, "stub")];
  mkdirSync(bin);
  mkdirSync(stub);
  launcher(bin, "gh");
  writeFileSync(
    join(stub, "gh.json"),
    JSON.stringify([{ argv: ["pr", "view"], stdout: "x\n", stderr: "", code: 0 }]),
  );
  const env = { ...process.env, PATH: bin + delimiter + process.env.PATH, FACTORY_STUB_DIR: stub };
  const gh = (...args) => {
    const r = spawnSync("gh", args, { env, encoding: "utf8", timeout: 60_000 });
    return [r.status, r.stdout, r.stderr];
  };
  assert.deepEqual(gh("pr", "view"), [0, "x\n", ""]);
  assert.deepEqual(gh("pr", "view"), [0, "x\n", ""]);
  assert.deepEqual(gh("api", "y"), [127, "", "stub: no recorded response for gh api y\n"]);
  const calls = readFileSync(join(stub, "gh.calls.jsonl"), "utf8").trim().split("\n");
  assert.deepEqual(calls.map(JSON.parse), [
    { argv: ["pr", "view"] },
    { argv: ["pr", "view"] },
    { argv: ["api", "y"] },
  ]);
});

test("the launcher quotes a tool name with shell characters", { skip: posix }, (t) => {
  const dir = temp(t);
  launcher(dir, "it's $x");
  const r = spawnSync(join(dir, "it's $x"), ["y"], {
    env: { ...process.env, FACTORY_STUB_DIR: dir },
    encoding: "utf8",
    timeout: 60_000,
  });
  assert.deepEqual([r.status, r.stderr], [127, "stub: no recorded response for it's $x y\n"]);
});

test("a wrong binary fails", { skip: posix }, (t) => {
  const wrong = script(join(temp(t), "wrong"), "#!/bin/sh\necho wrong\n");
  const [code, out, err] = run(["--bin", wrong, "--case", "main/no-args"]);
  assert.deepEqual([code, err], [1, ""]);
  const lines = out.trimEnd().split("\n");
  assert.equal(lines[0], "FAIL main/no-args: exit code differs");
  assert.deepEqual(lines.slice(1, 3), ["--- golden", "+++ bin"]);
  assert.equal(lines.at(-1), "parity: 0 passed, 1 failed");
});

test("an ids file or --case that matches no case exits 2", { skip: posix }, (t) => {
  const ids = join(temp(t), "ids.txt");
  writeFileSync(ids, "# comment\nmain/no-args\nnope/*\n");
  assert.deepEqual(run(["--ids", ids]), [2, "", "parity: no case matches nope/*\n"]);
  assert.deepEqual(run(["--case", "nope"]), [2, "", "parity: no case matches nope\n"]);
});

test("a missing ids file or binary exits 2", { skip: posix }, (t) => {
  const dir = temp(t);
  const missing = join(dir, "missing");
  assert.deepEqual(run(["--ids", missing]), [2, "", `parity: no such file or folder ${missing}\n`]);
  assert.deepEqual(run(["--bin", missing]), [2, "", `parity: not executable ${missing}\n`]);
  assert.deepEqual(run(["--bin", dir]), [2, "", `parity: not executable ${dir}\n`]);
  assert.deepEqual(run([], { FACTORY_BIN: missing }), [
    2,
    "",
    `parity: not executable ${missing}\n`,
  ]);
});

test("the cases cover CLI v1 with unique ids", () => {
  const ids = loadCases().map((c) => c.id);
  assert.equal(new Set(ids).size, ids.length);
  assert.ok(ids.includes("arity/claim/2"));
  assert.ok(ids.includes("arity/claim/4"));
  assert.deepEqual(
    ids.filter((id) => id.startsWith("arity/ideas/")),
    [],
  );
});

test("an HTTP case records its requests and home", { skip: posix || noBin }, async () => {
  const fields = Object.fromEntries(await runSide(caseById("pipeline/view"), [BIN]));
  assert.equal(fields["exit code"], "0");
  assert.deepEqual(fields.requests.split("\n").map(JSON.parse), [
    {
      method: "GET",
      path: "/api/pipeline/effective/sample",
      body: "",
      headers: {
        "cf-access-client-id": "parity-client-id",
        "cf-access-client-secret": "parity-client-secret",
        "user-agent": "factory",
      },
    },
  ]);
  assert.ok(fields["home tree"].includes("b'---\\napi_url: {api}\\ndevice: parity\\n---\\n'"));
});

test("the host's access token never reaches a side", { skip: posix }, async (t) => {
  const show = join(temp(t), "show.mjs");
  writeFileSync(
    show,
    "const env = process.env;\n" +
      'console.log(env.FACTORY_ACCESS_CLIENT_ID ?? "None", env.FACTORY_ACCESS_CLIENT_SECRET ?? "None");\n',
  );
  const saved = [process.env.FACTORY_ACCESS_CLIENT_ID, process.env.FACTORY_ACCESS_CLIENT_SECRET];
  process.env.FACTORY_ACCESS_CLIENT_ID = "host-id";
  process.env.FACTORY_ACCESS_CLIENT_SECRET = "host-secret";
  t.after(() => {
    for (const [key, value] of [
      ["FACTORY_ACCESS_CLIENT_ID", saved[0]],
      ["FACTORY_ACCESS_CLIENT_SECRET", saved[1]],
    ])
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
  });
  const fields = Object.fromEntries(
    await runSide(caseById("main/no-args"), [process.execPath, show]),
  );
  assert.equal(fields.stdout, "None None\n");
});

test("a request the golden lacks fails the case", { skip: posix || noBin }, (t) => {
  const drift = script(
    join(temp(t), "drift.mjs"),
    `#!${process.execPath}\n` +
      'import { spawnSync } from "node:child_process";\n' +
      'import { readFileSync } from "node:fs";\n' +
      "const args = process.argv.slice(2);\n" +
      'const home = args[args.indexOf("--home") + 1];\n' +
      'const origin = /api_url: (\\S+)/.exec(readFileSync(home + "/config.md", "utf8"))[1];\n' +
      'await fetch(origin + "/extra");\n' +
      `process.exit(spawnSync(${JSON.stringify(BIN)}, args, { stdio: "inherit" }).status);\n`,
  );
  const [code, out, err] = run(["--bin", drift, "--case", "pipeline/view"]);
  assert.deepEqual([code, err], [1, ""]);
  const lines = out.split("\n");
  assert.equal(lines[0], "FAIL pipeline/view: requests differs");
  assert.ok(
    lines.some((line) => line.startsWith("+{") && line.includes('"path": "/extra"')),
    out,
  );
});

test("--record refuses a run whose exit the case does not expect", { skip: posix }, async (t) => {
  const ok = join(temp(t), "ok.mjs");
  writeFileSync(ok, "");
  const c = { ...caseById("pipeline/view"), exit: 3 };
  assert.deepEqual(await runCase(c, [process.execPath, ok], true), [
    "FAIL pipeline/view: exit 0, case expects 3",
  ]);
});

test("a case lives in its subsystem file and its id is unique", (t) => {
  const dir = temp(t);
  const row = { id: "pipeline/x", argv: [], home: null, stubs: {} };
  writeFileSync(join(dir, "board.json"), JSON.stringify({ version: 1, cases: [row] }));
  assert.throws(() => loadCases(dir), {
    message: "parity: board.json: case pipeline/x belongs in pipeline.json",
  });
  rmSync(join(dir, "board.json"));
  writeFileSync(join(dir, "pipeline.json"), JSON.stringify({ version: 1, cases: [row, row] }));
  assert.throws(() => loadCases(dir), { message: "parity: duplicate case id pipeline/x" });
});

test("an ids folder reads every file, each under its own subsystem", { skip: posix }, (t) => {
  const dir = temp(t);
  writeFileSync(join(dir, "main.txt"), "# comment\nmain/no-args\n");
  writeFileSync(join(dir, "doctor.txt"), "doctor/extra\n");
  assert.deepEqual(readIds(dir), ["doctor/extra", "main/no-args"]);
  writeFileSync(join(dir, "board.txt"), "main/no-args\n");
  assert.deepEqual(run(["--ids", dir]), [
    2,
    "",
    "parity: board.txt: pattern main/no-args must start with board/\n",
  ]);
});

test("an ids folder runs every case it names", { skip: posix || noBin }, (t) => {
  const dir = temp(t);
  writeFileSync(join(dir, "main.txt"), "# comment\nmain/no-args\n");
  writeFileSync(join(dir, "doctor.txt"), "doctor/extra\n");
  const [code, out, err] = run(["--ids", dir, "--bin", BIN]);
  assert.deepEqual(
    [code, out.trimEnd().split("\n").at(-1), err],
    [0, "parity: 2 passed, 0 failed", ""],
  );
});

test("expand replaces known names only", () => {
  const values = { home: "/h", tmp: "/t", api: "http://a", today: "2026-01-02" };
  assert.equal(
    expand("{home} {tmp} {api} {today} {other}", values),
    "/h /t http://a 2026-01-02 {other}",
  );
});

test("stub values are JSON-escaped", () => {
  const values = { home: 'a"b\\c' };
  const jsonValues = Object.fromEntries(
    Object.entries(values).map(([k, v]) => [k, JSON.stringify(v).slice(1, -1)]),
  );
  assert.deepEqual(JSON.parse(expand(JSON.stringify(["{home}/x"]), jsonValues)), ['a"b\\c/x']);
});

test("home files, stubs and routes are expanded", { skip: posix }, async (t) => {
  const dir = temp(t);
  mkdirSync(join(dir, "homes/placeholders"), { recursive: true });
  writeFileSync(join(dir, "homes/placeholders/f.txt"), "{home}|{api}|{today}");
  const side = join(dir, "side.mjs");
  writeFileSync(
    side,
    'import { readFileSync } from "node:fs";\n' +
      'import { dirname } from "node:path";\n' +
      "const home = process.argv[2];\n" +
      'const [h, api, day] = readFileSync(home + "/f.txt", "utf8").split("|");\n' +
      'const stub = JSON.parse(readFileSync(process.env.FACTORY_STUB_DIR + "/gh.json", "utf8"));\n' +
      'const body = await (await fetch(api + "/day/" + day)).text();\n' +
      "console.log(h === home, api.startsWith('http://127.0.0.1:'), /^\\d{4}-\\d\\d-\\d\\d$/.test(day),\n" +
      "  JSON.stringify(stub) === JSON.stringify([dirname(home)]), body === dirname(home));\n",
  );
  const c = {
    id: "main/placeholders",
    argv: ["{home}"],
    home: "placeholders",
    stubs: { gh: ["{tmp}"] },
    routes: { "GET /day/{today}": [200, "{tmp}"] },
  };
  const fields = Object.fromEntries(await runSide(c, [process.execPath, side], join(dir, "homes")));
  assert.deepEqual(
    [fields["exit code"], fields.stdout],
    ["0", "true true true true true\n"],
    fields.stderr,
  );
});

test("a PATCH is served and its authorization recorded", async () => {
  const server = await httpFixture({ "PATCH /x": [200, "ok"] });
  const reply = await fetch(server.origin + "/x", {
    method: "PATCH",
    body: "{}",
    headers: { Authorization: "Bearer fake" },
  });
  assert.equal(await reply.text(), "ok");
  await server.close();
  const line = JSON.parse(requestLines(server, (data) => data)[0]);
  assert.deepEqual([line.method, line.headers.authorization], ["PATCH", "Bearer fake"]);
});

test("skip_stderr leaves stderr out of a case", { skip: posix }, async (t) => {
  const noisy = join(temp(t), "noisy.mjs");
  writeFileSync(noisy, 'process.stderr.write("noise\\n");\n');
  const c = { id: "main/noisy", argv: [], home: null, stubs: {} };
  const stderr = async (extra) =>
    Object.fromEntries(await runSide({ ...c, ...extra }, [process.execPath, noisy])).stderr;
  assert.equal(await stderr({}), "noise\n");
  assert.equal(await stderr({ skip_stderr: true }), "");
});
