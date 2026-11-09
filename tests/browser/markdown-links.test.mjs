// Offline checks for the dashboard markdown renderer's reference links.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const NEW_TAB =
  '<span class="new-tab" aria-hidden="true">&#8599;</span><span class="sr-only"> (opens in a new tab)</span>';

const html = await readFile(new URL("../../web/dashboard.html", import.meta.url), "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const slice = (from, to) => script.slice(script.indexOf(from), script.indexOf(to));
const context = { URL };
vm.runInNewContext(
  [
    "let docDir = null, docBase = null, docRepo = 'example/web';",
    "let jobs = [{ id: '175-sample-dates' }, { id: '133-a' }, { id: '133-b' }];",
    "const factoryConfig = { image_origins: ['https://x.example.test'], jira_origin: 'https://jira.example.test' };",
    slice("function esc(", "function ago("),
    slice("const JOB_IMG", "// Board writes."),
    "globalThis.md = md;",
    "globalThis.set = (repo, origin) => { docRepo = repo; factoryConfig.jira_origin = origin; };",
  ].join("\n"),
  context,
);
const { md, set } = context;
const JIRA =
  '<a href="https://jira.example.test/browse/PLAN-8558" target="_blank" rel="noopener noreferrer">PLAN-8558<span class="new-tab" aria-hidden="true">&#8599;</span><span class="sr-only"> (opens in a new tab)</span></a>';
const PR =
  '<a href="https://github.com/example/web/pull/17126" target="_blank" rel="noopener noreferrer">PR #17126<span class="new-tab" aria-hidden="true">&#8599;</span><span class="sr-only"> (opens in a new tab)</span></a>';

test("job, Jira key and PR number become links", () => {
  set("example/web", "https://jira.example.test");
  assert.equal(
    md("job 175 (PLAN-8558, PR #17126)"),
    `<p><a href="#175-sample-dates">job 175</a> (${JIRA}, ${PR})</p>`,
  );
  assert.equal(
    md("See 175-sample-dates and #9."),
    '<p>See <a href="#175-sample-dates">175-sample-dates</a> and <a href="https://github.com/example/web/pull/9" target="_blank" rel="noopener noreferrer">#9<span class="new-tab" aria-hidden="true">&#8599;</span><span class="sr-only"> (opens in a new tab)</span></a>.</p>',
  );
});

test("code, fences, links and image alt text stay unlinked", () => {
  set("example/web", "https://jira.example.test");
  assert.equal(
    md("`PLAN-8558` and `job 175`"),
    "<p><code>PLAN-8558</code> and <code>job 175</code></p>",
  );
  assert.equal(md("```\nPLAN-8558 #17126\n```"), "<pre><code>PLAN-8558 #17126</code></pre>");
  assert.equal(
    md("[PLAN-1](https://x.example.test)"),
    `<p><a href="https://x.example.test" target="_blank" rel="noopener noreferrer">PLAN-1${NEW_TAB}</a></p>`,
  );
  assert.equal(
    md("![PLAN-1 shot](https://x.example.test/a.png)"),
    '<p><a class="img-link" href="https://x.example.test/a.png" target="_blank" rel="noopener noreferrer"><img src="https://x.example.test/a.png" alt="PLAN-1 shot" loading="lazy"></a></p>',
  );
  assert.equal(md("![PLAN-1 shot](https://y.example.test/a.png)"), "<p>PLAN-1 shot</p>");
  assert.equal(
    md("![see https://y.example.test](https://y.example.test/a.png)"),
    "<p>see https://y.example.test</p>",
  );
});

test("false positives stay text", () => {
  set("example/web", "https://jira.example.test");
  for (const text of ["GMT-04:00", "UTF-8", "SHA-256", "it's", "2026-11-02", "job 133"])
    assert.doesNotMatch(md(text), /<a /, text);
  assert.equal(md("it's"), "<p>it&#39;s</p>");
});

test("keys and PR numbers stay text without config", () => {
  set(null, undefined);
  assert.equal(md("PLAN-8558, PR #17126"), "<p>PLAN-8558, PR #17126</p>");
});

const url = (href) =>
  `<a href="${href}" target="_blank" rel="noopener noreferrer">${href}${NEW_TAB}</a>`;

test("bare https URLs become links", () => {
  set("example/web", "https://jira.example.test");
  assert.equal(
    md("Preview at https://preview.example.test/jobs?id=175&tab=spec."),
    `<p>Preview at ${url("https://preview.example.test/jobs?id=175&amp;tab=spec")}.</p>`,
  );
  assert.equal(
    md("(https://x.example.test/a_(b))"),
    `<p>(${url("https://x.example.test/a_(b)")})</p>`,
  );
  assert.equal(
    md('"https://q.example.test"'),
    `<p>&quot;${url("https://q.example.test")}&quot;</p>`,
  );
  assert.equal(md("https://"), "<p>https://</p>");
});

test("bare URLs skip code, fences, links and http", () => {
  set("example/web", "https://jira.example.test");
  assert.equal(md("`https://code.example.test`"), "<p><code>https://code.example.test</code></p>");
  assert.equal(
    md("```\nhttps://code.example.test\n```"),
    "<pre><code>https://code.example.test</code></pre>",
  );
  assert.equal(
    md("[docs](https://x.example.test/d)"),
    `<p><a href="https://x.example.test/d" target="_blank" rel="noopener noreferrer">docs${NEW_TAB}</a></p>`,
  );
  assert.doesNotMatch(md("![shot](https://x.example.test/a.png)"), /new-tab/);
  assert.doesNotMatch(md("job 175"), /new-tab/);
  assert.equal(
    md("![shot](https://x.example.test/a.png)"),
    '<p><a class="img-link" href="https://x.example.test/a.png" target="_blank" rel="noopener noreferrer"><img src="https://x.example.test/a.png" alt="shot" loading="lazy"></a></p>',
  );
  assert.equal(
    md("see https://a.test/x`code`"),
    `<p>see ${url("https://a.test/x")}<code>code</code></p>`,
  );
  assert.equal(
    md("https://a.test/![shot](https://x.example.test/a.png)"),
    `<p>${url("https://a.test/")}<a class="img-link" href="https://x.example.test/a.png" target="_blank" rel="noopener noreferrer"><img src="https://x.example.test/a.png" alt="shot" loading="lazy"></a></p>`,
  );
  assert.equal(md("http://plain.example.test"), "<p>http://plain.example.test</p>");
  assert.equal(
    md("https://x.example.test/?k=PLAN-2"),
    `<p>${url("https://x.example.test/?k=PLAN-2")}</p>`,
  );
});
