// scripts/test-markdown.mjs
// Tiny dependency-free smoke test for src/lib/markdown.ts.
// We bundle the TS module with esbuild (already installed via Vite) and run a
// battery of inline/block cases, printing PASS/FAIL counts. Exit code is 0
// on success, 1 on any failure.

import { build } from 'esbuild';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { writeFile, unlink, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = resolve(__dirname, '..');
const srcFile = resolve(root, 'src/lib/markdown.ts');
const tmpFile = resolve(tmpdir(), `markdown-test-${Date.now()}.mjs`);

const bundleResult = await build({
  entryPoints: [srcFile],
  bundle: true,
  format: 'esm',
  platform: 'node',
  target: 'es2022',
  outfile: tmpFile,
  logLevel: 'silent',
});
if (bundleResult.errors.length) {
  console.error('esbuild errors:', bundleResult.errors);
  process.exit(1);
}

const mod = await import(pathToFileURL(tmpFile).href);
await unlink(tmpFile).catch(() => {});

const { renderMarkdown, renderInline, escHtml } = mod;

let pass = 0;
let fail = 0;
const failures = [];

function check(name, actual, expected) {
  const a = String(actual);
  const e = String(expected);
  if (a === e) {
    pass++;
  } else {
    fail++;
    failures.push({ name, actual: a, expected: e });
  }
}

// ---------- escHtml ----------
check('esc-html <', escHtml('<script>'), '&lt;script&gt;');
check('esc-html &', escHtml('a & b'), 'a &amp; b');
check('esc-html quote', escHtml('"x"'), '&quot;x&quot;');
check('esc-html apostrophe', escHtml("it's"), 'it&#39;s');

// ---------- renderInline ----------
check('inline bold', renderInline('**x**'), '<strong>x</strong>');
check('inline em', renderInline('*x*'), '<em>x</em>');
check('inline strike', renderInline('~~x~~'), '<s>x</s>');
check('inline mark', renderInline('==x=='), '<mark>x</mark>');
check('inline code', renderInline('`x`'), '<code>x</code>');
check('inline code protects url', renderInline('see `https://x.y` here'),
  'see <code>https://x.y</code> here');
check('inline code protects em', renderInline('a `*not em*` b'),
  'a <code>*not em*</code> b');
check('inline bold-italic', renderInline('***x***'), '<strong><em>x</em></strong>');
check('inline em-around-bold', renderInline('*a **b** c*'),
  '<em>a <strong>b</strong> c</em>');
check('inline snake-case safe', renderInline('use __init__ in python'),
  'use __init__ in python');
check('inline link', renderInline('[t](https://x.y)'),
  '<a href="https://x.y" data-url="https://x.y" rel="noopener">t</a>');
check('inline autolink', renderInline('<https://x.y>'),
  '<a href="https://x.y" data-url="https://x.y" rel="noopener">https://x.y</a>');
check('inline bare url', renderInline('visit https://x.y today'),
  'visit <a href="https://x.y" data-url="https://x.y" rel="noopener">https://x.y</a> today');
check('inline javascript: blocked', renderInline('[t](javascript:alert(1))'), 't');
check('inline data: blocked', renderInline('[t](data:text/html,bad)'), 't');
check('inline xss escaped', renderInline('<script>alert(1)</script>'),
  '&lt;script&gt;alert(1)&lt;/script&gt;');
check('inline hardbreak', renderInline('a  \nb'), 'a<br>b');

// ---------- renderMarkdown: paragraphs ----------
check('md empty', renderMarkdown(''), '');
check('md single line', renderMarkdown('hello'), '<p>hello</p>');
check('md soft break -> space', renderMarkdown('a\nb'), '<p>a b</p>');
check('md paragraph split', renderMarkdown('a\n\nb'), '<p>a</p><p>b</p>');

// ---------- renderMarkdown: headings ----------
check('md h1', renderMarkdown('# H'), '<h1>H</h1>');
check('md h3', renderMarkdown('### H3'), '<h3>H3</h3>');
check('md h6', renderMarkdown('###### H6'), '<h6>H6</h6>');

// ---------- renderMarkdown: hr ----------
check('md hr dashes', renderMarkdown('---'), '<hr>');
check('md hr stars', renderMarkdown('***'), '<hr>');
check('md hr underscore', renderMarkdown('___'), '<hr>');

// ---------- renderMarkdown: lists ----------
check('md ul', renderMarkdown('- a\n- b'), '<ul><li>a</li><li>b</li></ul>');
check('md ol', renderMarkdown('1. a\n2. b'), '<ol><li>a</li><li>b</li></ol>');
check('md task list', renderMarkdown('- [ ] t\n- [x] d'),
  '<ul><li class="task"><input type="checkbox" disabled class="task-box" /> t</li><li class="task"><input type="checkbox" disabled checked class="task-box" /> d</li></ul>');

// ---------- renderMarkdown: blockquote ----------
check('md blockquote', renderMarkdown('> q\n> r'),
  '<blockquote><p>q<br>r</p></blockquote>');

// ---------- renderMarkdown: fenced code ----------
check('md fenced no lang', renderMarkdown('```\nfoo\nbar\n```'),
  '<div class="codeblock"><div class="codeblock-head"><span class="codeblock-lang"></span><button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button></div><pre class="codeblock-pre"><code class="codeblock-code">foo\nbar</code></pre></div>');
check('md fenced with lang', renderMarkdown('```ts\nlet x = 1;\n```'),
  '<div class="codeblock" data-lang="ts"><div class="codeblock-head"><span class="codeblock-lang">ts</span><button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button></div><pre class="codeblock-pre"><code class="codeblock-code language-ts"><span class="tok-keyword">let</span> x = <span class="tok-number">1</span>;</code></pre></div>');
check('md fenced protects backticks', renderMarkdown('```\nlet t = `x`;\n```'),
  '<div class="codeblock"><div class="codeblock-head"><span class="codeblock-lang"></span><button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button></div><pre class="codeblock-pre"><code class="codeblock-code">let t = `x`;</code></pre></div>');
check('md fenced followed by text', renderMarkdown('```ts\nx\n```\nhello'),
  '<div class="codeblock" data-lang="ts"><div class="codeblock-head"><span class="codeblock-lang">ts</span><button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button></div><pre class="codeblock-pre"><code class="codeblock-code language-ts">x</code></pre></div><p>hello</p>');

// ---------- renderMarkdown: tables ----------
check('md table basic', renderMarkdown('| a | b |\n|---|---|\n| 1 | 2 |'),
  '<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table>');

// ---------- renderMarkdown: combined ----------
check('md combo', renderMarkdown('## Title\n\n- a\n- b\n\n```ts\nx\n```'),
  '<h2>Title</h2><ul><li>a</li><li>b</li></ul><div class="codeblock" data-lang="ts"><div class="codeblock-head"><span class="codeblock-lang">ts</span><button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button></div><pre class="codeblock-pre"><code class="codeblock-code language-ts">x</code></pre></div>');

// ---------- extra: table + task + highlight + safety ----------
check('md table with inline bold', renderMarkdown('| a | b |\n|---|---|\n| **x** | `y` |'),
  '<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody><tr><td><strong>x</strong></td><td><code>y</code></td></tr></tbody></table>');
check('md fenced with python string', renderMarkdown('```python\nx = "hi"\n```'),
  // Strings become tok-string, "x" and "=" stay as plain text
  '<div class="codeblock" data-lang="python"><div class="codeblock-head"><span class="codeblock-lang">python</span><button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button></div><pre class="codeblock-pre"><code class="codeblock-code language-python">x = &quot;hi&quot;</code></pre></div>');
check('md fenced with ts keyword', renderMarkdown('```ts\nconst x = 1;\n```'),
  '<div class="codeblock" data-lang="ts"><div class="codeblock-head"><span class="codeblock-lang">ts</span><button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button></div><pre class="codeblock-pre"><code class="codeblock-code language-ts"><span class="tok-keyword">const</span> x = <span class="tok-number">1</span>;</code></pre></div>');
check('md nested ul', renderMarkdown('- a\n  - b\n- c'),
  '<ul><li>a<ul><li>b</li></ul></li><li>c</li></ul>');
check('md nested link with parens', renderMarkdown('[wp](https://en.wikipedia.org/wiki/Foo_(bar))'),
  '<p><a href="https://en.wikipedia.org/wiki/Foo_(bar)" data-url="https://en.wikipedia.org/wiki/Foo_(bar)" rel="noopener">wp</a></p>');
check('md xss in heading', renderMarkdown('# <script>'),
  '<h1>&lt;script&gt;</h1>');

// ---------- output ----------
console.log(`\nPASS: ${pass}`);
console.log(`FAIL: ${fail}`);
if (fail) {
  console.log('\nFailures:');
  for (const f of failures) {
    console.log(`  - ${f.name}`);
    console.log(`    expected: ${f.expected}`);
    console.log(`    actual:   ${f.actual}`);
  }
  process.exit(1);
}
