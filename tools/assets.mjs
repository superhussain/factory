import { readFile, writeFile, rename, mkdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import { createHash } from "node:crypto";
import { gzipSync, constants } from "node:zlib";

const root = new URL("../", import.meta.url);
export const MERMAID_SRI =
  "sha384-8xUhuLNMjW/cI2Dacg3DR4WliYznvIg0LVG1stctThKD/YVrJFI9TNCFxft5NYgp";
const CEILING = 3_000_000;
// Source inputs whose change invalidates a dry-run report.
const SOURCES = [
  "worker/index.js",
  "worker/events.js",
  "worker/access.ts",
  "worker/assets.js",
  "worker/pwa.js",
  "worker/icons.js",
  "web/dashboard.html",
  ".build/mermaid.min.txt",
  ".build/third-party-notices.txt",
];
// Test-only material that must never appear in the production upload.
const FORBIDDEN = [
  "tests/support",
  "local-entry",
  "auth-entry",
  "auth-wrangler",
  "X-Test-Storage-Calls",
  "workerd-current",
  "synthetic",
];

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const gzip9 = (bytes) => gzipSync(bytes, { level: 9 }).length;
// Wrangler sizes uploads with zlib.gzipSync at the default level over the concatenation
// of module contents; recomputing the same algorithm reconciles its reported size.
const gzipDefault = (bytes) => gzipSync(bytes, { level: constants.Z_DEFAULT_COMPRESSION }).length;

export async function prepareAssets() {
  const bytes = await readFile(new URL("node_modules/mermaid/dist/mermaid.min.js", root));
  const sri = "sha384-" + createHash("sha384").update(bytes).digest("base64");
  const gzip = gzip9(bytes);
  if (sri !== MERMAID_SRI || bytes.length !== 5494801) {
    throw new Error("Pinned Mermaid artifact does not match reviewed bytes");
  }
  if (
    /\bimport\s*\(/.test(bytes.toString()) ||
    /(?:from\s*|import\s*)["']\.\.?\//.test(bytes.toString())
  ) {
    throw new Error("Mermaid artifact has external chunks");
  }
  const notices = await readFile(new URL("THIRD_PARTY_NOTICES.md", root));
  await mkdir(new URL(".build/", root), { recursive: true });
  for (const [name, body] of [
    ["mermaid.min.txt", bytes],
    ["third-party-notices.txt", notices],
  ]) {
    const target = new URL(`.build/${name}`, root);
    const temporary = new URL(`.build/${name}.${process.pid}.tmp`, root);
    await writeFile(temporary, body);
    await rename(temporary, target);
  }
  return {
    bytes: bytes.length,
    gzip,
    sri,
    compression: {
      algorithm: "gzip",
      level: 9,
      node: process.version,
      zlib: process.versions.zlib,
    },
  };
}

function parseForm(bytes) {
  const boundary = bytes.subarray(0, bytes.indexOf("\r\n"));
  const parts = [];
  let at = 0;
  for (;;) {
    const start = bytes.indexOf(boundary, at);
    if (start < 0) break;
    const headEnd = bytes.indexOf("\r\n\r\n", start);
    if (headEnd < 0) break;
    const head = bytes.subarray(start + boundary.length, headEnd).toString();
    const end = bytes.indexOf(Buffer.concat([Buffer.from("\r\n"), boundary]), headEnd);
    if (end < 0) break;
    const name = /name="([^"]+)"/.exec(head)?.[1];
    const type = /Content-Type: ([^\r]+)/.exec(head)?.[1] ?? null;
    parts.push({ name, type, body: bytes.subarray(headEnd + 4, end) });
    at = end + 2;
  }
  return parts;
}

// Wrangler colors its deploy summary when FORCE_COLOR is set, and Nx forces
// FORCE_COLOR=true for every task it forks. The escape codes land inside the
// reported line, so strip them before matching: same regex, same strictness,
// and byte-identical behavior for uncolored output.
// oxlint-disable-next-line no-control-regex -- ANSI escapes start with the ESC control character.
const ANSI = /\u001b\[[0-9;]*[A-Za-z]/g;

function parseReported(output) {
  const match = /Total Upload:\s*([0-9.]+) KiB \/ gzip: ([0-9.]+) KiB/.exec(
    output.replace(ANSI, ""),
  );
  if (!match) throw new Error("Wrangler did not report a Total Upload line");
  return {
    line: match[0],
    raw: Number(match[1]),
    gzip: Number(match[2]),
    unit: "KiB",
    precision: 2,
  };
}

export async function inspectBundle(outDir, reported = "") {
  const form = await readFile(`${outDir}/upload.form`);
  const parts = parseForm(form);
  const metadataPart = parts.find((part) => part.name === "metadata");
  if (!metadataPart) throw new Error("dry-run upload lacks the metadata part");
  const metadata = JSON.parse(metadataPart.body.toString());
  const maps = parts.filter((part) => part.type === "application/source-map");
  const modules = parts.filter(
    (part) => part !== metadataPart && part.type !== "application/source-map",
  );
  if (!modules.length) throw new Error("dry-run upload contains no modules");
  const entry = modules.find((part) => part.name === metadata.main_module);
  if (!entry) throw new Error("main module missing from upload");

  // Every static import in the entry resolves to an inventoried uploaded module.
  const names = new Set(modules.map((part) => part.name.replace(/^\.\//, "")));
  const specifiers = [...entry.body.toString().matchAll(/(?:import|from)\s*["']([^"']+)["']/g)].map(
    (m) => m[1],
  );
  for (const specifier of specifiers) {
    if (!names.has(specifier.replace(/^\.\//, ""))) {
      throw new Error(`entry import ${specifier} does not resolve to an uploaded module`);
    }
  }

  // No test entrypoint, synthetic principal/key or auth override string ships.
  for (const part of parts) {
    for (const mark of FORBIDDEN) {
      if (part.body.includes(mark))
        throw new Error(`forbidden test material in upload part ${part.name}: ${mark}`);
    }
  }

  // Uploaded text modules are byte-identical to their repository/build sources.
  const pairs = [
    ["dashboard.html", "web/dashboard.html"],
    ["mermaid.min.txt", ".build/mermaid.min.txt"],
    ["third-party-notices.txt", ".build/third-party-notices.txt"],
  ];
  for (const [suffix, source] of pairs) {
    const part = modules.find((m) => m.name.endsWith("-" + suffix));
    if (!part) throw new Error(`upload lacks the ${suffix} module`);
    const sourceBytes = await readFile(new URL(source, root));
    if (!part.body.equals(sourceBytes))
      throw new Error(`${suffix} module bytes differ from ${source}`);
  }

  const raw = modules.reduce((sum, part) => sum + part.body.length, 0);
  const gzipSum = modules.reduce((sum, part) => sum + gzip9(part.body), 0);
  // Wrangler's own gzip measurement concatenates the non-entry modules in bundle order
  // with the entry last; recompute that exact figure for reconciliation.
  const ordered = [...modules.filter((part) => part !== entry), entry];
  const concatDefault = gzipDefault(Buffer.concat(ordered.map((part) => part.body)));

  const report = parseReported(reported);
  // One least-significant printed unit (0.01 KiB) covers the printed rounding.
  const allowance = Math.pow(10, -report.precision) * 1024;
  const bound = Math.ceil(report.gzip * 1024 + allowance);
  const reportedRawBytes = report.raw * 1024;
  if (Math.abs(raw - reportedRawBytes) > allowance) {
    throw new Error(`raw module bytes ${raw} diverge from Wrangler's reported ${report.raw} KiB`);
  }
  if ((concatDefault / 1024).toFixed(report.precision) !== report.gzip.toFixed(report.precision)) {
    throw new Error(
      `recomputed concatenated gzip ${concatDefault} does not match Wrangler's reported ${report.gzip} KiB`,
    );
  }
  const limitBytes = Math.max(gzipSum, bound);
  const document = {
    version: 1,
    delivery: "bundled",
    assets: [],
    modules: modules.map((part) => ({
      path: part.name,
      bytes: part.body.length,
      sha256: sha256(part.body),
    })),
    totals: { raw, gzip_level9_sum: gzipSum },
    compression: {
      algorithm: "gzip",
      tool: "node:zlib gzipSync",
      level: 9,
      reconciliation_level: "default",
      node: process.version,
      zlib: process.versions.zlib,
    },
    wrangler: {
      reported_line: report.line,
      unit: report.unit,
      precision: report.precision,
      reported_gzip_kib: report.gzip,
      reported_raw_kib: report.raw,
      rounding_allowance_bytes: allowance,
      rounded_upper_bound_bytes: bound,
      recomputed_concat_gzip_bytes: concatDefault,
      recomputed_matches_reported: true,
      packaging:
        "Wrangler sizes the concatenation of non-entry modules in bundle order with the entry last, gzipped at its default level; the binding sum uses per-module level 9. The greater figure is the limit result.",
    },
    exclusions: [
      "metadata form part (upload manifest JSON, not a module)",
      ...maps.map((m) => `${m.name} (source map; excluded from Wrangler's own size)`),
    ],
    ceiling: CEILING,
    limit: { result_bytes: limitBytes, pass: limitBytes <= CEILING },
    offline: null,
  };
  for (const source of SOURCES) {
    const bytes = await readFile(new URL(source, root));
    document.assets.push({ path: source, bytes: bytes.length, sha256: sha256(bytes) });
  }
  if (!document.limit.pass)
    throw new Error(`complete bundle ${limitBytes} exceeds the ${CEILING} byte ceiling`);
  const target = new URL(".build/assets.json", root);
  const temporary = new URL(`.build/assets.json.${process.pid}.tmp`, root);
  await writeFile(temporary, JSON.stringify(document, null, 2) + "\n");
  await rename(temporary, target);
  return {
    modules: document.modules.length,
    raw,
    gzip: gzipSum,
    bound,
    limit: limitBytes,
    ceiling: CEILING,
  };
}

export async function verifyAssets() {
  const path = new URL(".build/assets.json", root);
  if (!existsSync(path)) throw new Error("Run python3 tools/deploy.py dry-run first.");
  const document = JSON.parse(await readFile(path, "utf8"));
  for (const asset of document.assets) {
    const bytes = await readFile(new URL(asset.path, root));
    if (asset.bytes !== bytes.length || asset.sha256 !== sha256(bytes)) {
      throw new Error(`stale dry-run report: ${asset.path} changed; re-run the dry-run`);
    }
  }
  if (!document.limit?.pass) throw new Error("recorded limit result does not pass");
  const { spawn } = await import("node:child_process");
  const run = (args) =>
    new Promise((resolve) => {
      const child = spawn(process.execPath, args, { stdio: "inherit" });
      child.once("exit", (code) => resolve(code));
    });
  // The owned offline fixture proves the browser received the inventoried bytes and
  // rendered both diagram kinds with no external attempt; the auth suites re-prove
  // the gate the dry-run inventory ships.
  // One node --test run overlaps the browser fixture with the auth suites' timeout waits.
  const suites = [
    "tests/browser/offline-mermaid.test.mjs",
    "tests/worker/access.test.mjs",
    "tests/worker/entry-auth.test.mjs",
  ];
  if ((await run(["--test", `--test-concurrency=${suites.length}`, ...suites])) !== 0)
    throw new Error(`${suites.join(", ")}: node --test failed`);
  document.offline = { result: "pass", node: process.version, at: new Date().toISOString() };
  const temporary = new URL(`.build/assets.json.${process.pid}.tmp`, root);
  await writeFile(temporary, JSON.stringify(document, null, 2) + "\n");
  await rename(temporary, path);
  return { verified: true, limit: document.limit };
}

const isMain = process.argv[1] && new URL(import.meta.url).pathname === process.argv[1];
if (isMain) {
  const args = process.argv.slice(2);
  if (args.length === 1 && args[0] === "--offline") {
    // prepareAssets, then the owned offline fixture (real browser, same-origin bytes).
    await prepareAssets();
    const { spawn } = await import("node:child_process");
    const child = spawn(process.execPath, ["--test", "tests/browser/offline-mermaid.test.mjs"], {
      stdio: "inherit",
    });
    child.once("exit", (code) => process.exit(code ?? 1));
  } else if (args.length === 1 && args[0] === "--verify") {
    await verifyAssets();
  } else {
    console.error("usage: node tools/assets.mjs --offline | --verify");
    process.exit(2);
  }
}
