// src/lib/markdown.ts
// Tiny, dependency-free markdown renderer used by the chat bubble.
//
// Goals (in priority order):
//   1. Safe — never inject raw user HTML. `escHtml` is the first step on every
//      input. We then apply a hand-curated set of inline transforms that emit
//      only a known whitelist of tags.
//   2. Streaming-friendly — pure function of the input string, no globals, no
//      hidden state. Safe to call on every rAF tick during a stream.
//   3. Visually correct — emits proper <p> paragraphs (instead of <br>-spammed
//      lines), GFM tables, task-list checkboxes, fenced code with a language
//      label and a copy button, and a small set of inline rules that don't
//      step on each other (`**` vs `*`, `_` in snake_case, URLs in `code`).
//   4. Cheap — no AST, no recursion, just a single linear pass per block.
//
// Non-goals: CommonMark full compliance, raw HTML pass-through, LaTeX, Mermaid.
// When the model emits something the parser can't represent, we fall back to
// escaping it verbatim inside a <p> — the user never loses their text.

export function escHtml(s: string): string {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c] as string,
  );
}

// ---------------------------------------------------------------------------
// Inline parser
// ---------------------------------------------------------------------------
//
// We pre-protect code spans (`` `…` ``, ` ``…`` `, leading-space code-fence)
// with sentinel placeholders, run the bold/italic/etc. rules on the protected
// string, then restore the code untouched. The "links last" pass converts
// bare URLs, [text](url) and <url> into <a> tags, but only OUTSIDE the
// restored code spans — that's the trick that fixes the "URL inside backticks
// becomes clickable" bug from the old renderer.

const PLACEHOLDER = '\u0000';

interface InlineState {
  codes: string[];
}

function protectCodeSpans(s: string, st: InlineState): string {
  // ```…``` first, then `…` (single). The leading backtick must NOT be inside
  // another backtick run of the same length — i.e. ``foo`bar`` is the
  // double-backtick span containing a single backtick.
  let out = s;
  // Multi-backtick: longest run of 2+ backticks wins, then any char inside.
  out = out.replace(/(``+)([\s\S]*?)\1/g, (_m, ticks: string, body: string) => {
    const i = st.codes.length;
    st.codes.push(escHtml(body));
    return `${PLACEHOLDER}CB${i}${PLACEHOLDER}`;
  });
  // Single-backtick: `…` with no inner newline and at least one non-` ` char.
  out = out.replace(/`([^`\n]+)`/g, (_m, body: string) => {
    const i = st.codes.length;
    st.codes.push(escHtml(body));
    return `${PLACEHOLDER}CB${i}${PLACEHOLDER}`;
  });
  return out;
}

function restoreCodeSpans(s: string, st: InlineState): string {
  return s.replace(new RegExp(`${PLACEHOLDER}CB(\\d+)${PLACEHOLDER}`, 'g'), (_m, n: string) => {
    const idx = +n;
    return `<code>${st.codes[idx]}</code>`;
  });
}

// Apply emphasis / strong / strikethrough / mark on an already-escHtml'd and
// code-protected string. Single linear pass; no regex backtracking tricks.
function applyEmphasis(s: string): string {
  // `***x***` → strong+em. We have to handle this before `**` and `*` or the
  // first `**` will eat the outer markers.
  s = s.replace(/\*\*\*([^\s*][\s\S]*?[^\s*]|[^\s*])\*\*\*/g, '<strong><em>$1</em></strong>');
  s = s.replace(/\*\*\*([^\s*])\*\*\*/g, '<strong><em>$1</em></strong>');
  // `**x**` → strong. Require non-space on both edges so `** not bold **` is
  // left alone (the old renderer required only `[^*]+`, which broke on
  // bold-italic and on `**` inside words).
  s = s.replace(/\*\*([^\s*][\s\S]*?[^\s*])\*\*/g, '<strong>$1</strong>');
  s = s.replace(/\*\*([^\s*])\*\*/g, '<strong>$1</strong>');
  // `*x*` → em. The look-behind/ahead rules out `**` leftovers and `_em_`-
  // like mixes. No newlines inside (CommonMark).
  s = s.replace(/(^|[^\*\w])\*([^\s\*][^\*\n]*?)\*(?=[\s\.,;:!\?\)_\*\}]|$)/g, '$1<em>$2</em>');
  // `__x__` / `_x_` — but NOT inside snake_case / dunder identifiers like
  // `__init__` or `use_var_name`. We require the inner content to be NOT a
  // pure identifier (only word chars + underscores). This is stricter than
  // CommonMark but safer for chat where Python dunders appear frequently.
  s = s.replace(/(^|[^\w])__(\S(?:[\s\S]*?\S)?)__(?!\w)/g, (m, lead: string, body: string) => {
    if (/^\w+$/.test(body)) return m; // looks like a dunder / single identifier — leave alone
    return `${lead}<strong>${body}</strong>`;
  });
  s = s.replace(/(^|[^\w_])_(\S(?:[^_\n]*?\S)?)_(?!\w)/g, (m, lead: string, body: string) => {
    if (/^\w+$/.test(body)) return m; // snake_case identifier — leave alone
    return `${lead}<em>${body}</em>`;
  });
  // `~~x~~` → strikethrough.
  s = s.replace(/~~([^\s~][\s\S]*?[^\s~])~~/g, '<s>$1</s>');
  s = s.replace(/~~([^\s~])~~/g, '<s>$1</s>');
  // `==x==` → mark (highlight). Same rules as strikethrough.
  s = s.replace(/==([^\s=][\s\S]*?[^\s=])==/g, '<mark>$1</mark>');
  s = s.replace(/==([^\s=])==/g, '<mark>$1</mark>');
  return s;
}

function applyLinks(s: string): string {
  // `[text](url)` first. URLs may contain one level of balanced parens
  // (CommonMark), e.g. `[wp](https://en.wikipedia.org/wiki/Foo_(bar))`.
  // Block any non-http(s) scheme — we won't be a launchpad for `javascript:`
  // or `data:`.
  s = s.replace(/\[([^\]\n]+)\]\(((?:[^()\s]|\([^()]*\))*)\)/g, (_m, text: string, url: string) => {
    if (!/^https?:\/\//i.test(url)) return text; // drop the URL, keep label
    return `<a href="${url}" data-url="${url}" rel="noopener">${text}</a>`;
  });
  // `<https://…>` autolink.
  s = s.replace(/&lt;(https?:\/\/[^\s<>]+)&gt;/g, (_m, url: string) =>
    `<a href="${url}" data-url="${url}" rel="noopener">${url}</a>`,
  );
  // Bare URLs that we missed (e.g. after restore) — keep this pass LAST.
  s = s.replace(/(?<!["'>])(https?:\/\/[^\s<"]+)/g, (m) =>
    `<a href="${m}" data-url="${m}" rel="noopener">${m}</a>`,
  );
  return s;
}

export function renderInline(raw: string): string {
  if (!raw) return '';
  const st: InlineState = { codes: [] };
  // Escape FIRST. This means the rest of the pipeline sees a string that
  // never contains raw `<` or `>`. Autolinks are reconstructed by the link
  // pass via the `&lt;` form.
  let s = escHtml(raw);
  s = protectCodeSpans(s, st);
  s = applyEmphasis(s);
  // Hard line break: two spaces + newline → <br>. CommonMark-compatible.
  s = s.replace(/( {2,}|\\)\n/g, '<br>');
  s = restoreCodeSpans(s, st);
  s = applyLinks(s);
  return s;
}

// ---------------------------------------------------------------------------
// Block parser
// ---------------------------------------------------------------------------
//
// One pass over the input, line by line. State is just `inFence` + `lang` +
// a small accumulator. We never recurse into the inline parser from inside a
// fenced code body, so user content there is escaped once and never re-parsed.

interface FencedCode {
  lang: string;
  body: string;
}

function renderCodeBlock(code: FencedCode): string {
  const lang = (code.lang || '').trim().toLowerCase();
  const safeLang = lang.replace(/[^a-z0-9+#-]/g, '').slice(0, 24);
  const bodyHtml = highlightCode(code.body, safeLang);
  const classLang = safeLang ? ` language-${safeLang}` : '';
  const dataLang = safeLang ? ` data-lang="${safeLang}"` : '';
  const langLabel = safeLang ? `<span class="codeblock-lang">${safeLang}</span>` : '<span class="codeblock-lang"></span>';
  // The `code` element keeps the language class — we use it for CSS-token
  // syntax highlighting (see styles in Chat.svelte).
  return (
    `<div class="codeblock"${dataLang}>` +
    `<div class="codeblock-head">` +
    langLabel +
    `<button class="codeblock-copy" type="button" aria-label="Скопировать код">⧉ Копировать</button>` +
    `</div>` +
    `<pre class="codeblock-pre"><code class="codeblock-code${classLang}">${bodyHtml}</code></pre>` +
    `</div>`
  );
}

// ---------------------------------------------------------------------------
// Tiny CSS-token highlighter. No AST, no proper lexer — just enough regex to
// paint keywords / strings / numbers / comments / punctuation. For every
// language we know about, we run the same passes in the same order:
//   1. comments           (so we don't highlight inside them)
//   2. strings
//   3. numbers
//   4. keywords
//   5. leftover punctuation (very small list)
// Anything we don't recognize is left as plain text. This is intentionally
// rough — see the plan's "open questions" for the deliberate non-goal of
// replacing this with `highlight.js` / `shiki`.
// ---------------------------------------------------------------------------

const KEYWORDS: Record<string, string> = {
  ts: 'abstract any as assert async await break case catch class const continue debugger default delete do else enum export extends finally for from function get if implements import in infer instanceof interface is keyof let module namespace never new null of override private protected public readonly require return set static super switch this throw true false try type typeof undefined unique var void while with yield',
  js: 'abstract async await boolean break byte case catch char class const continue debugger default delete do double else enum export extends false final finally float for from function get goto if implements import in instanceof int interface let long new null of package private protected public return set short static super switch synchronized this throw throws transient true try typeof undefined var void volatile while with yield',
  py: 'False None True and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield match case',
  rs: 'as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while',
  sh: 'if then else elif fi case esac for in while do done function select until return break continue export local readonly declare set unset alias source',
  bash: 'if then else elif fi case esac for in while do done function select until return break continue export local readonly declare set unset alias source',
  json: 'true false null',
  html: '',
  css: '',
  sql: 'select from where insert into update delete create drop alter table index view join left right inner outer full on group by order having as and or not null is like in between exists distinct union all',
};

function highlightCode(src: string, lang: string): string {
  // We escape FIRST so the original \n and HTML-special chars are preserved
  // verbatim inside the highlighted output. Then we re-introduce `<span
  // class="tok-X">…</span>` around recognized tokens.
  const esc = escHtml(src);
  if (!lang || !KEYWORDS[lang]) return esc;

  // 1) Comments. We support line (`//`, `#`) and block (`/* … */`).
  let s = esc;
  if (lang === 'html' || lang === 'css') {
    s = s.replace(/&lt;!--[\s\S]*?--&gt;/g, (m) => `<span class="tok-comment">${m}</span>`);
    if (lang === 'css') {
      s = s.replace(/\/\*[\s\S]*?\*\//g, (m) => `<span class="tok-comment">${m}</span>`);
    }
  } else if (lang === 'py' || lang === 'sh' || lang === 'bash') {
    s = s.replace(/(^|[^\\])#.*$/gm, (m) => `<span class="tok-comment">${m}</span>`);
  } else if (lang === 'sql') {
    s = s.replace(/--.*$/gm, (m) => `<span class="tok-comment">${m}</span>`);
    s = s.replace(/\/\*[\s\S]*?\*\//g, (m) => `<span class="tok-comment">${m}</span>`);
  } else {
    // ts/js/rs/json
    s = s.replace(/\/\*[\s\S]*?\*\//g, (m) => `<span class="tok-comment">${m}</span>`);
    s = s.replace(/(^|[^:\\])\/\/[^\n]*/g, (m) => `<span class="tok-comment">${m}</span>`);
  }

  // 2) Strings. We work on already-escaped text, so `"` becomes `&quot;`
  // and `<` becomes `&lt;`. Backticks survive as-is.
  s = s.replace(/`[^`\n]*`/g, (m) => `<span class="tok-string">${m}</span>`);
  s = s.replace(/&quot;(?:\\.|(?!&quot;|\\).)*&quot;/g, (m) => `<span class="tok-string">${m}</span>`);
  s = s.replace(/&#39;(?:\\.|(?!&#39;|\\).)*&#39;/g, (m) => `<span class="tok-string">${m}</span>`);

  // 3) Numbers.
  s = s.replace(/\b\d+(?:\.\d+)?\b/g, (m) => `<span class="tok-number">${m}</span>`);

  // 4) Keywords (only outside already-wrapped spans — the simple trick is
  // to split on our span boundaries and re-join).
  const kws = KEYWORDS[lang].split(/\s+/).filter(Boolean);
  if (kws.length) {
    const parts = s.split(/(<span class="tok-[a-z]+">[\s\S]*?<\/span>)/g);
    const kwRe = new RegExp(`\\b(${kws.map((k) => k.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})\\b`, 'g');
    s = parts
      .map((p) => (p.startsWith('<span ') ? p : p.replace(kwRe, '<span class="tok-keyword">$1</span>')))
      .join('');
  }

  return s;
}

function renderTable(header: string[], rows: string[][]): string {
  const ths = header.map((c) => `<th>${renderInline(c)}</th>`).join('');
  const trs = rows
    .map(
      (r) =>
        '<tr>' +
        r.map((c) => `<td>${renderInline(c)}</td>`).join('') +
        (r.length < header.length ? header.slice(r.length).map(() => '<td></td>').join('') : '') +
        '</tr>',
    )
    .join('');
  return `<table><thead><tr>${ths}</tr></thead><tbody>${trs}</tbody></table>`;
}

function isTableSeparator(line: string): boolean {
  // `| --- | :---: | ---: |` — at least one dash row, may include colons and spaces.
  if (!line.includes('|') && !/^\s*[-:]+[-:\s|]*$/.test(line)) return false;
  const cells = splitTableRow(line);
  if (cells.length === 0) return false;
  return cells.every((c) => /^\s*:?-{1,}:?\s*$/.test(c));
}

function splitTableRow(line: string): string[] {
  // Trim leading/trailing pipes, then split on `|`. Don't split escaped pipes
  // (we don't support them — model output is rare, and we keep this simple).
  let s = line.trim();
  if (s.startsWith('|')) s = s.slice(1);
  if (s.endsWith('|')) s = s.slice(0, -1);
  return s.split('|').map((c) => c.trim());
}

function isListItem(line: string): { kind: 'ul' | 'ol'; indent: number; marker: string; text: string; checked: boolean | null } | null {
  const m = line.match(/^(\s*)([-*]|\d+\.)\s+(.*)$/);
  if (!m) return null;
  const indent = Math.floor(m[1].length / 2);
  const marker = m[2];
  const rest = m[3];
  const kind: 'ul' | 'ol' = /^\d+\./.test(marker) ? 'ol' : 'ul';
  // Task list: `- [ ]` / `- [x]` / `- [X]`.
  const task = rest.match(/^\[([ xX])\]\s+(.*)$/);
  let checked: boolean | null = null;
  let text = rest;
  if (task) {
    checked = task[1].toLowerCase() === 'x';
    text = task[2];
  }
  return { kind, indent, marker, text, checked };
}

function renderListItem(item: { checked: boolean | null; text: string }, sub: string): string {
  const checkbox =
    item.checked === null
      ? ''
      : `<input type="checkbox" disabled${item.checked ? ' checked' : ''} class="task-box" /> `;
  const cls = item.checked === null ? '' : ' class="task"';
  return `<li${cls}>${checkbox}${renderInline(item.text)}${sub}</li>`;
}

// Render a flat list of indented lines into a (possibly nested) <ul>/<ol>.
// We accept a pre-parsed array of `ListNode`s from the caller, so the block
// loop only has to detect list runs.
interface ListNode {
  kind: 'ul' | 'ol';
  indent: number;
  checked: boolean | null;
  text: string;
  // children are children-of-this-item, in body order, rendered as
  // additional inline + nested-list content inside the <li>.
  children: ListNode[];
  // After the first line of the item, any continuation lines (indented more
  // than the item marker) join as additional inline content.
  continuations: string[];
}

function buildListTree(items: Array<{ kind: 'ul' | 'ol'; indent: number; checked: boolean | null; text: string }>): ListNode[] {
  // We use a flat stack; for each new item, we attach it as a child of the
  // most recent item with a strictly smaller indent that hasn't been closed
  // yet. Indentation 0 = top level. Items with the same indent as the parent
  // are siblings.
  const roots: ListNode[] = [];
  const stack: ListNode[] = [];
  for (const it of items) {
    const node: ListNode = { ...it, children: [], continuations: [] };
    // Pop stack until we find an indent < ours, or empty.
    while (stack.length && stack[stack.length - 1].indent >= it.indent) stack.pop();
    if (stack.length === 0) {
      roots.push(node);
    } else {
      stack[stack.length - 1].children.push(node);
    }
    stack.push(node);
  }
  return roots;
}

function renderListHTML(nodes: ListNode[]): string {
  if (nodes.length === 0) return '';
  // All top-level nodes share the same kind in well-formed markdown; if the
  // model mixes, we still emit separate <ul>/<ol> for each.
  const parts: string[] = [];
  let i = 0;
  while (i < nodes.length) {
    const n = nodes[i];
    const tag = n.kind === 'ol' ? 'ol' : 'ul';
    const group: ListNode[] = [];
    while (i < nodes.length && nodes[i].kind === n.kind && nodes[i].indent === n.indent) {
      group.push(nodes[i]);
      i++;
    }
    const inner = group
      .map((g) => {
        const sub = g.children.length ? renderListHTML(g.children) : '';
        const cont = g.continuations.length
          ? g.continuations.map((c) => renderInline(c)).join('<br>')
          : '';
        const body = cont ? `${renderInline(g.text)}<br>${cont}` : renderInline(g.text);
        const checkbox =
          g.checked === null
            ? ''
            : `<input type="checkbox" disabled${g.checked ? ' checked' : ''} class="task-box" /> `;
        const cls = g.checked === null ? '' : ' class="task"';
        return `<li${cls}>${checkbox}${body}${sub}</li>`;
      })
      .join('');
    parts.push(`<${tag}>${inner}</${tag}>`);
  }
  return parts.join('');
}

export function renderMarkdown(input: string): string {
  if (input == null || input === '') return '';
  const text = String(input).replace(/\r\n?/g, '\n');
  const lines = text.split('\n');

  const out: string[] = [];
  let para: string[] = [];
  let fence: FencedCode | null = null;

  const flushPara = () => {
    if (para.length === 0) return;
    // Collapse soft line breaks: a single \n inside a paragraph becomes a
    // space; only hard breaks (2 trailing spaces) become <br>.
    const joined = para.map((l) => l.replace(/ {2,}$/, ' \u0001')).join('\n').replace(/\n/g, ' ').replace(/ \u0001/g, '<br>');
    out.push(`<p>${renderInline(joined)}</p>`);
    para = [];
  };

  // Detect a contiguous run of list items (possibly with indented continuations).
  const collectListRun = (start: number): { items: ListNode[]; next: number } => {
    const flat: Array<{ kind: 'ul' | 'ol'; indent: number; checked: boolean | null; text: string; cont: string[] }> = [];
    let i = start;
    while (i < lines.length) {
      const m = isListItem(lines[i]);
      if (m) {
        flat.push({ kind: m.kind, indent: m.indent, checked: m.checked, text: m.text, cont: [] });
        i++;
        continue;
      }
      // Continuation: line is indented more than 0 and not blank, and not a
      // new list item. We attribute it to the last item.
      if (flat.length && lines[i].trim() !== '' && /^\s{2,}\S/.test(lines[i]) && !isListItem(lines[i])) {
        flat[flat.length - 1].cont.push(lines[i].trim());
        i++;
        continue;
      }
      if (lines[i].trim() === '') {
        // Peek: if next non-blank line is a list item (same or greater depth
        // relative to current), the blank line is part of the list run.
        let j = i;
        while (j < lines.length && lines[j].trim() === '') j++;
        if (j < lines.length && isListItem(lines[j])) {
          i = j;
          continue;
        }
      }
      break;
    }
    // Build the indent tree.
    const items = flat.map((f) => ({ kind: f.kind, indent: f.indent, checked: f.checked, text: f.text }));
    const tree = buildListTree(items);
    // Re-attach continuations to the matching leaves (matching by the linear
    // order — flat[i].cont belongs to tree-leaves in DFS order).
    let k = 0;
    const walk = (nodes: ListNode[]) => {
      for (const n of nodes) {
        if (flat[k]) n.continuations = flat[k].cont;
        k++;
        if (n.children.length) walk(n.children);
      }
    };
    walk(tree);
    return { items: tree, next: i };
  };

  // Detect a contiguous run of table lines (header + separator + ≥0 body rows).
  const collectTableRun = (start: number): { header: string[]; rows: string[][]; next: number } | null => {
    if (start + 1 >= lines.length) return null;
    if (!isTableSeparator(lines[start + 1])) return null;
    const header = splitTableRow(lines[start]);
    if (header.length === 0) return null;
    let i = start + 2;
    const rows: string[][] = [];
    while (i < lines.length && lines[i].trim() !== '' && lines[i].includes('|')) {
      // Don't accept a separator-looking line as a body row.
      if (isTableSeparator(lines[i])) break;
      rows.push(splitTableRow(lines[i]));
      i++;
    }
    return { header, rows, next: i };
  };

  // Collect a blockquote run.
  const collectQuoteRun = (start: number): { body: string; next: number } => {
    const buf: string[] = [];
    let i = start;
    while (i < lines.length && /^>\s?/.test(lines[i])) {
      buf.push(lines[i].replace(/^>\s?/, ''));
      i++;
    }
    return { body: buf.join('\n'), next: i };
  };

  let i = 0;
  while (i < lines.length) {
    const line = lines[i];

    // Inside a fenced code block: keep adding until closing fence.
    if (fence) {
      if (/^```\s*$/.test(line)) {
        out.push(renderCodeBlock(fence));
        fence = null;
        i++;
        continue;
      }
      fence.body += (fence.body ? '\n' : '') + line;
      i++;
      continue;
    }

    // Fence open: ```lang
    const fenceOpen = line.match(/^```\s*([\w+-]*)\s*$/);
    if (fenceOpen) {
      flushPara();
      fence = { lang: fenceOpen[1] || '', body: '' };
      i++;
      continue;
    }

    // Blank line → paragraph break.
    if (line.trim() === '') {
      flushPara();
      i++;
      continue;
    }

    // Horizontal rule.
    if (/^---+\s*$/.test(line) || /^\*\*\*+\s*$/.test(line) || /^___+\s*$/.test(line)) {
      flushPara();
      out.push('<hr>');
      i++;
      continue;
    }

    // Heading.
    const h = line.match(/^(#{1,6})\s+(.+?)\s*#*\s*$/);
    if (h) {
      flushPara();
      const level = h[1].length;
      out.push(`<h${level}>${renderInline(h[2])}</h${level}>`);
      i++;
      continue;
    }

    // Blockquote.
    if (/^>\s?/.test(line)) {
      flushPara();
      const q = collectQuoteRun(i);
      out.push(`<blockquote><p>${renderInline(q.body).replace(/\n/g, '<br>')}</p></blockquote>`);
      i = q.next;
      continue;
    }

    // List run.
    if (isListItem(line)) {
      flushPara();
      const run = collectListRun(i);
      out.push(renderListHTML(run.items));
      i = run.next;
      continue;
    }

    // Table run.
    if (line.includes('|') && i + 1 < lines.length && isTableSeparator(lines[i + 1])) {
      flushPara();
      const t = collectTableRun(i);
      if (t) {
        out.push(renderTable(t.header, t.rows));
        i = t.next;
        continue;
      }
    }

    // Default: paragraph text.
    para.push(line);
    i++;
  }

  if (fence) {
    // Unterminated fence — emit what we have as a code block anyway so the
    // user never loses their code.
    out.push(renderCodeBlock(fence));
  } else {
    flushPara();
  }

  return out.join('');
}

// Convenience wrapper used by Chat.svelte. If the renderer throws for any
// reason (defensive — it shouldn't, but a future regex might regress), we
// surface a minimal notice plus the raw text inside a <pre> so the user
// still sees the model output.
export function safeRenderMarkdown(input: string): string {
  try {
    return renderMarkdown(input);
  } catch (e) {
    const notice = '<p>⚠ render error</p>';
    return `${notice}<pre>${escHtml(String(input))}</pre>`;
  }
}
