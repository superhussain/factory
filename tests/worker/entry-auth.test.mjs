import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { FLOORS, GATES } from "../../worker/admission.js";
import { loadProduction, memoryBucket, serverEnv } from "../support/worker.mjs";
import { generateKey, jwksFixture, sign } from "../support/jwks.mjs";

const { default: worker } = await loadProduction();
const key = await generateKey("current");
const routes = [
  ["GET", "/"],
  ["GET", "/dashboard.html"],
  ["GET", "/board.json"],
  ["GET", "/projects.json"],
  ["GET", "/api/board"],
  ["PUT", "/api/board"],
  ["GET", "/api/backlog"],
  ["PUT", "/api/backlog"],
  ["GET", "/api/projects/sample.md"],
  ["PUT", "/api/projects/sample.md"],
  ["GET", "/api/project-documents/sample"],
  ["GET", "/api/project-documents/sample/notes.md"],
  ["PUT", "/api/project-documents/sample/notes.md"],
  ["GET", "/api/jobs/sample/001/"],
  ["GET", "/api/jobs/sample/001/build.md"],
  ["PUT", "/api/jobs/sample/001/build.md"],
  ["GET", "/jobs/sample/001/build.md"],
  ["GET", "/jobs/sample/001/round-1/shot.png"],
  ["GET", `/api/idea-images/${"a".repeat(64)}.png`],
  ["PUT", `/api/idea-images/${"a".repeat(64)}.png`],
  ["GET", "/assets/mermaid.min.js"],
  ["GET", "/assets/third-party-notices.txt"],
  ["POST", "/assets/mermaid.min.js"],
  ["GET", "/sw.js"],
  ["GET", "/assets/manifest.webmanifest"],
  ["GET", "/assets/icon-192.png"],
  ["GET", "/assets/icon-512.png"],
  ["GET", "/assets/apple-touch-icon.png"],
  ["POST", "/sw.js"],
  ["OPTIONS", "/api/board"],
  ["HEAD", "/dashboard.html"],
  ["DELETE", "/unknown"],
  ["GET", "/unknown"],
  ["GET", "/api/jobs/sample/..bad/build.md"],
];

function request(path, method = "GET", headers = {}, origin = serverEnv.FACTORY_ORIGIN) {
  return new Request(origin + path, { method, headers });
}

async function denied(response, status = 403) {
  assert.equal(response.status, status);
  assert.equal(await response.text(), status === 503 ? "factory unavailable" : "forbidden");
  assert.equal(response.headers.get("Content-Type"), "text/plain; charset=utf-8");
  assert.equal(response.headers.get("Cache-Control"), "no-store");
  assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
  assert.equal(response.headers.get("Location"), null);
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), null);
}

test("every route and method denies before content I/O", async (t) => {
  const f = jwksFixture(t, [key]);
  const env = { ...serverEnv, ACCESS_ISSUER: f.config.issuer, BUCKET: memoryBucket() };
  const invalid = await sign(await generateKey("current"), f.claims());
  for (const headers of [
    {},
    { Cookie: "CF_Authorization=not-an-assertion" },
    { "CF-Access-Client-Id": "device", "CF-Access-Client-Secret": "opaque" },
    { "Cf-Access-Jwt-Assertion": "garbage" },
    { "Cf-Access-Jwt-Assertion": invalid },
  ]) {
    for (const [method, path] of routes)
      await denied(await worker.fetch(request(path, method, headers), env));
  }
  assert.deepEqual(env.BUCKET.calls, []);
});

test("valid no-email service assertion gates page, assets, redirect and protocol", async (t) => {
  const f = jwksFixture(t, [key]);
  const bucket = memoryBucket();
  const env = { ...serverEnv, ACCESS_ISSUER: f.config.issuer, BUCKET: bucket };
  const headers = {
    "Cf-Access-Jwt-Assertion": await sign(key, f.claims({ type: "app", common_name: "device" })),
  };
  for (const [path, status, body] of [
    ["/dashboard.html", 302, ""],
    ["/unknown", 404, "not found"],
    ["/api/board", 404, "not found"],
    ["/projects.json", 200, "[]"],
  ]) {
    const response = await worker.fetch(request(path, "GET", headers), env);
    assert.equal(response.status, status);
    assert.equal(await response.text(), body);
  }
  for (const [path, file, type] of [
    ["/assets/mermaid.min.js", ".build/mermaid.min.txt", "text/javascript"],
    ["/assets/third-party-notices.txt", "THIRD_PARTY_NOTICES.md", "text/plain"],
  ]) {
    const response = await worker.fetch(request(path, "GET", headers), env);
    assert.equal(response.status, 200);
    assert.deepEqual(Buffer.from(await response.arrayBuffer()), await readFile(file));
    assert.equal(response.headers.get("Content-Type"), type + "; charset=utf-8");
    assert.equal(response.headers.get("Cache-Control"), "no-store");
    assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
  }
  const page = await worker.fetch(request("/", "GET", headers), env);
  assert.equal(page.status, 200);
  assert.match(
    await page.text(),
    /id="factory-config">\{"image_origins":\[\],"agent_links":\{\}\}/,
  );
  assert.equal((await worker.fetch(request("/assets/other.js", "GET", headers), env)).status, 404);
  assert.equal(
    (await worker.fetch(request("/assets/mermaid.min.js", "POST", headers), env)).status,
    404,
  );
  assert.equal((await worker.fetch(request("/sw.js", "POST", headers), env)).status, 404);
});

test("authenticated root HTML and query-preserving legacy redirects never access storage", async (t) => {
  const f = jwksFixture(t, [key]);
  const env = { ...serverEnv, ACCESS_ISSUER: f.config.issuer, BUCKET: memoryBucket() };
  const headers = { "Cf-Access-Jwt-Assertion": await sign(key, f.claims()) };
  const page = await worker.fetch(request("/", "GET", headers), env);
  assert.equal(page.status, 200);
  assert.equal(page.headers.get("Content-Type"), "text/html; charset=utf-8");
  assert.equal(page.headers.get("Cache-Control"), "no-store");
  assert.equal(page.headers.get("X-Content-Type-Options"), "nosniff");
  assert.equal(page.headers.get("Location"), null);
  assert.equal(
    page.headers.get("Content-Security-Policy"),
    "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'none'",
  );
  const html = await page.text();
  assert.match(html, /<!doctype html>/i);
  assert.match(html, /id="factory-config"/);
  assert.deepEqual(JSON.parse(html.match(/id="factory-reference">([^<]*)<\/script>/)[1]), {
    floors: FLOORS,
    gates: GATES,
  });
  const meta = html
    .match(/http-equiv="Content-Security-Policy" content="([^"]*)"/)[1]
    .replaceAll("&#x27;", "'");
  assert.equal(meta, page.headers.get("Content-Security-Policy"));
  for (const search of ["", "?view=a%2Fb&space=a+b&escape=%2f&view=second"]) {
    const response = await worker.fetch(request("/dashboard.html" + search, "GET", headers), env);
    assert.equal(response.status, 302);
    assert.equal(response.headers.get("Location"), "/" + search);
    assert.equal(await response.text(), "");
    assert.equal(response.headers.get("Cache-Control"), "no-store");
    assert.equal(response.headers.get("X-Content-Type-Options"), "nosniff");
  }
  for (const path of ["/", "/dashboard.html"]) {
    for (const method of ["HEAD", "POST", "PUT", "DELETE", "OPTIONS"]) {
      const response = await worker.fetch(request(path, method, headers), env);
      assert.equal(response.status, 404);
      assert.equal(await response.text(), "not found");
      assert.equal(response.headers.get("Location"), null);
    }
  }
  assert.deepEqual(env.BUCKET.calls, []);
});

test("configured origin mismatch denies even a valid signature without content or JWKS I/O", async (t) => {
  const f = jwksFixture(t, [key]);
  const env = { ...serverEnv, ACCESS_ISSUER: f.config.issuer, BUCKET: memoryBucket() };
  const headers = { "Cf-Access-Jwt-Assertion": await sign(key, f.claims()) };
  for (const origin of [
    "http://factory.example.test",
    "https://other.example.test",
    "https://factory.example.test:8443",
  ]) {
    for (const [method, path] of routes)
      await denied(await worker.fetch(request(path, method, headers, origin), env));
  }
  assert.deepEqual(env.BUCKET.calls, []);
  assert.deepEqual(f.state.calls, []);
});

test("missing or malformed server values fail closed before auth or dispatch", async (t) => {
  const f = jwksFixture(t, [key]);
  const base = { ...serverEnv, ACCESS_ISSUER: f.config.issuer };
  const invalid = [];
  // Required fields only: omitting optional FACTORY_IMAGE_ORIGINS/FACTORY_AGENT_LINKS is
  // valid (defaults [] and {}), so this matrix never classifies an omission as malformed.
  for (const name of ["FACTORY_ORIGIN", "FACTORY_TITLE", "ACCESS_ISSUER", "ACCESS_AUDIENCE"])
    for (const value of [undefined, "", null, false, 1, "a\nvalue"])
      invalid.push({ [name]: value });
  for (const origin of [
    "http://factory.example.test",
    "https://factory.example.test/",
    "https://user@factory.example.test",
    "https://127.0.0.1",
    "https://app.workers.dev",
    "https://factory.example.test:8443",
    "https://factory.example.test?x=1",
    "https://factory.example.test#x",
  ])
    invalid.push({ FACTORY_ORIGIN: origin });
  for (const issuer of [
    "https://wrong.example.test",
    "https://a.b.cloudflareaccess.com",
    "https://example-team.cloudflareaccess.com/",
    "http://example-team.cloudflareaccess.com",
  ])
    invalid.push({ ACCESS_ISSUER: issuer });
  invalid.push({ FACTORY_TITLE: "x".repeat(161) });
  for (const value of [
    "",
    "null",
    "{}",
    "false",
    '["http://images.example.test"]',
    '["https://images.example.test/"]',
    '["https://images.example.test:8443"]',
    '["https://u@images.example.test"]',
    '["https://images.example.test?q=1"]',
    '["https://images.example.test#x"]',
    '["https://images.example.test","https://images.example.test"]',
    '["https://x.example.test;script-src"]',
    [],
  ])
    invalid.push({ FACTORY_IMAGE_ORIGINS: value });
  for (const value of ["", "null", "[]", "false", '{"other":{}}', '{"app":null}', "{}x", {}])
    invalid.push({ FACTORY_AGENT_LINKS: value });
  for (const kind of ["app", "web"]) {
    for (const template of [
      "//example.test/{server}/{agent}",
      "javascript:{server}/{agent}",
      "data:{server}/{agent}",
      "file:///{server}/{agent}",
      "blob:{server}/{agent}",
      "http://example.test/{server}/{agent}",
      "https://u:p@example.test/{server}/{agent}",
      "https://example.test/{server}/{unknown}",
      "https://example.test/{server}",
      "https://example.test/{server}/{agent}\n",
    ]) {
      invalid.push({
        FACTORY_AGENT_LINKS: JSON.stringify({ [kind]: { label: "Session", template } }),
      });
    }
    for (const label of ["", "x".repeat(161), "bad\nlabel", null])
      invalid.push({
        FACTORY_AGENT_LINKS: JSON.stringify({
          [kind]: {
            label,
            template: (kind === "app" ? "session:" : "https:") + "//example.test/{server}/{agent}",
          },
        }),
      });
  }
  for (const values of invalid) {
    const env = { ...base, ...values, BUCKET: memoryBucket() };
    await denied(await worker.fetch(request("/assets/mermaid.min.js"), env), 503);
    assert.deepEqual(env.BUCKET.calls, []);
  }
  assert.deepEqual(f.state.calls, []);
});

test("signed requests with omitted optional settings get content with empty policies", async (t) => {
  const f = jwksFixture(t, [key]);
  const configured = {
    image: '["https://images.example.test"]',
    links: JSON.stringify({
      web: { label: "Session", template: "https://sessions.example.test/{server}/{agent}" },
    }),
  };
  const cases = [
    ["both omitted", {}, { image_origins: [], agent_links: {} }],
    [
      "image origins omitted",
      { FACTORY_AGENT_LINKS: configured.links },
      {
        image_origins: [],
        agent_links: {
          web: { label: "Session", template: "https://sessions.example.test/{server}/{agent}" },
        },
      },
    ],
    [
      "agent links omitted",
      { FACTORY_IMAGE_ORIGINS: configured.image },
      { image_origins: ["https://images.example.test"], agent_links: {} },
    ],
  ];
  for (const [name, optionals, expected] of cases) {
    await t.test(name, async () => {
      const env = {
        ...serverEnv,
        ACCESS_ISSUER: f.config.issuer,
        ...optionals,
        BUCKET: memoryBucket(),
      };
      const headers = {
        "Cf-Access-Jwt-Assertion": await sign(
          key,
          f.claims({ type: "app", common_name: "device" }),
        ),
      };
      const page = await worker.fetch(request("/", "GET", headers), env);
      assert.equal(page.status, 200);
      const config = JSON.parse(
        (await page.text()).match(/id="factory-config">([^<]*)<\/script>/)[1],
      );
      assert.deepEqual(config, expected);
      const asset = await worker.fetch(request("/assets/mermaid.min.js", "GET", headers), env);
      assert.equal(asset.status, 200);
    });
  }
});

test("hostile titles and JSON are escaped once and CSP matches rendered meta", async (t) => {
  const f = jwksFixture(t, [key]);
  const label = "</script><script>globalThis.injected=true</script>&\u2028\u2029__FACTORY_TITLE__";
  const title = 'Title </title>"&__FACTORY_CONFIG____FACTORY_CSP__';
  const env = {
    ...serverEnv,
    ACCESS_ISSUER: f.config.issuer,
    FACTORY_TITLE: title,
    FACTORY_IMAGE_ORIGINS: '["https://images.example.test"]',
    FACTORY_AGENT_LINKS: JSON.stringify({
      web: { label, template: "https://sessions.example.test/{server}/{agent}" },
      app: { label: "Session app", template: "session://h/{server}/agent/{agent}" },
    }),
    BUCKET: memoryBucket(),
  };
  const response = await worker.fetch(
    request("/", "GET", { "Cf-Access-Jwt-Assertion": await sign(key, f.claims()) }),
    env,
  );
  assert.equal(response.status, 200);
  const html = await response.text();
  assert.ok(!html.includes(label));
  assert.match(html, /Title &lt;\/title&gt;&quot;&amp;__FACTORY_CONFIG____FACTORY_CSP__/);
  const config = JSON.parse(html.match(/id="factory-config">([^<]*)<\/script>/)[1]);
  assert.equal(config.agent_links.web.label, label);
  assert.deepEqual(config.image_origins, ["https://images.example.test"]);
  assert.ok(
    !response.headers
      .get("Content-Security-Policy")
      .includes("script-src 'self' 'unsafe-inline' https:"),
  );
  const meta = html
    .match(/http-equiv="Content-Security-Policy" content="([^"]*)"/)[1]
    .replaceAll("&#x27;", "'");
  assert.equal(meta, response.headers.get("Content-Security-Policy"));
});
