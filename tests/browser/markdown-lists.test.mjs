// Offline checks for the dashboard markdown renderer's list handling.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const html = await readFile(new URL("../../web/dashboard.html", import.meta.url), "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const slice = (from, to) => script.slice(script.indexOf(from), script.indexOf(to));
const context = { URL };
vm.runInNewContext(
  [
    "let docDir = null, docBase = null, docRepo = null; let jobs = []; const factoryConfig = { image_origins: [] };",
    slice("function esc(", "function ago("),
    slice("const JOB_IMG", "// Board writes."),
    "globalThis.md = md;",
  ].join("\n"),
  context,
);
const md = context.md;

test("ordered items keep their bullets nested", () => {
  assert.equal(
    md("1. **A**\n   - x\n   - y\n2. **B**\n   - z\n3. **C**"),
    "<ol><li><strong>A</strong><ul><li>x</li><li>y</li></ul></li><li><strong>B</strong><ul><li>z</li></ul></li><li><strong>C</strong></li></ol>",
  );
});

test("blank lines keep a list open and start is emitted", () => {
  assert.equal(
    md("1. one\n\n2. two\n\n3. three"),
    "<ol><li>one</li><li>two</li><li>three</li></ol>",
  );
  assert.equal(md("3. a\n4. b"), '<ol start="3"><li>a</li><li>b</li></ol>');
});

test("existing shapes are unchanged", () => {
  assert.equal(md("- a\n\nPara"), "<ul><li>a</li></ul>\n<p>Para</p>");
  assert.equal(md("- a\n1. b"), "<ul><li>a</li></ul>\n<ol><li>b</li></ol>");
  assert.equal(md("- a\n- b"), "<ul><li>a</li><li>b</li></ul>");
});

test("task items render as disabled checkboxes", () => {
  assert.equal(
    md("- [ ] a\n- [x] b\n- [X] c"),
    '<ul><li class="task"><label><input type="checkbox" disabled> a</label></li>' +
      '<li class="task"><label><input type="checkbox" disabled checked> b</label></li>' +
      '<li class="task"><label><input type="checkbox" disabled checked> c</label></li></ul>',
  );
  assert.equal(
    md("- [x] A\n  - [ ] b"),
    '<ul><li class="task"><label><input type="checkbox" disabled checked> A</label><ul><li class="task"><label><input type="checkbox" disabled> b</label></li></ul></li></ul>',
  );
  assert.equal(
    md("1. [x] done"),
    '<ol><li class="task"><label><input type="checkbox" disabled checked> done</label></li></ol>',
  );
});

test("non-task brackets stay text", () => {
  assert.equal(md("- [ ]"), "<ul><li>[ ]</li></ul>");
  assert.equal(md("- [ ] "), "<ul><li>[ ] </li></ul>");
  assert.equal(md("- [x] "), "<ul><li>[x] </li></ul>");
  assert.equal(md("[ ] para"), "<p>[ ] para</p>");
  assert.ok(!md("- [link](./x)").includes('class="task"'));
});
