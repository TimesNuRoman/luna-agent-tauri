<script lang="ts">
  // Memory tab. Phase M0 + M1 — Recent Activity list with kind-filter
  // and a small "Archive now" button. M2 adds Search + Graph panels.
  // See docs/adr/0009-memory-layers-l0-l3.md for the design.

  import { onMount, onDestroy } from 'svelte';
  import {
    memoryStats,
    memoryListRecent,
    memoryConsolidateNow,
    memoryForget,
    memoryAddFact,
    memorySearch,
    memoryListGraphEntities,
    type MemoryEvent,
    type MemoryEventKind,
    type MemoryStats,
    type ConsolidationReport,
    type RecallHit,
    type Entity,
  } from './lib/tauri';

  // Tab state.
  type SubTab = 'activity' | 'search' | 'graph';
  let active: SubTab = 'activity';

  // Dashboard data.
  let stats: MemoryStats | null = null;
  let events: MemoryEvent[] = [];
  let kindFilter: '' | MemoryEventKind = '';
  let loading = false;
  let lastError = '';
  let busy = false;
  let lastConsolidation: ConsolidationReport | null = null;

  // Search state (M2).
  let searchQuery = '';
  let searchHits: RecallHit[] = [];
  let searching = false;

  // Graph state (M2 — list only, viz in M3).
  let graphEntities: Entity[] = [];
  let newFactText = '';
  let newFactImportance = 0.6;

  // Auto-refresh every 5s so new chat/edit events show up.
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  function fmtTime(ts: number): string {
    try {
      return new Date(ts).toLocaleString();
    } catch (e) {
      return String(ts);
    }
  }

  function kindBadgeClass(k: MemoryEventKind): string {
    // Color-codes event kinds. Inline-styled to avoid coupling to
    // the global theme tokens.
    switch (k) {
      case 'chat_turn':       return 'badge badge-blue';
      case 'file_edit':       return 'badge badge-amber';
      case 'interest_update': return 'badge badge-violet';
      case 'vision_trigger':  return 'badge badge-rose';
      case 'user_fact':       return 'badge badge-green';
      case 'tool_call':       return 'badge badge-gray';
      default:                return 'badge badge-gray';
    }
  }

  function recallLayerClass(l: string): string {
    switch (l) {
      case 'l1': return 'badge badge-blue';
      case 'l2': return 'badge badge-green';
      case 'l3': return 'badge badge-gray';
      default:   return 'badge badge-gray';
    }
  }

  async function refresh() {
    if (busy) return;
    busy = true;
    try {
      const [s, ev, ents] = await Promise.all([
        memoryStats(),
        memoryListRecent(200, kindFilter || null),
        memoryListGraphEntities().catch(() => [] as Entity[]),
      ]);
      stats = s;
      events = ev;
      graphEntities = ents;
      lastError = '';
    } catch (e) {
      lastError = String(e);
    } finally {
      busy = false;
    }
  }

  async function archiveNow() {
    busy = true;
    try {
      lastConsolidation = await memoryConsolidateNow(90);
      await refresh();
    } catch (e) {
      lastError = String(e);
    } finally {
      busy = false;
    }
  }

  async function forgetOne(id: string) {
    if (!confirm('Forget this memory entry?')) return;
    busy = true;
    try {
      await memoryForget(id);
      await refresh();
    } catch (e) {
      lastError = String(e);
    } finally {
      busy = false;
    }
  }

  async function doSearch() {
    if (!searchQuery.trim()) return;
    searching = true;
    try {
      searchHits = await memorySearch(searchQuery, 15);
    } catch (e) {
      lastError = String(e);
      searchHits = [];
    } finally {
      searching = false;
    }
  }

  async function rememberFact() {
    const t = newFactText.trim();
    if (!t) return;
    busy = true;
    try {
      await memoryAddFact(t, newFactImportance, []);
      newFactText = '';
      await refresh();
    } catch (e) {
      lastError = String(e);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    refresh();
    pollTimer = setInterval(refresh, 5000);
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
    return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  }
</script>

<div class="memory-root">
  <header class="hdr">
    <h2>🧠 Memory</h2>
    <p class="sub">
      Local-only event log + knowledge graph. See
      <code>docs/adr/0009-memory-layers-l0-l3.md</code> for the design.
    </p>
  </header>

  {#if lastError}
    <div class="banner banner-err">⚠ {lastError}</div>
  {/if}

  {#if stats && !stats.layers.l1 && !stats.layers.l3}
    <div class="banner banner-warn">
      Memory layer unavailable. Check the log for init errors
      (the service falls back to a no-op if the data directory isn't writable).
    </div>
  {/if}

  <!-- Dashboard cards -->
  <section class="cards">
    <div class="card">
      <div class="card-label">L0 Working</div>
      <div class="card-value">
        {stats?.layers.l0 ? '✓' : '—'}
      </div>
      <div class="card-sub">in-RAM ring buffer</div>
    </div>
    <div class="card">
      <div class="card-label">L1 Episodic</div>
      <div class="card-value">{stats?.l1_events ?? 0}</div>
      <div class="card-sub">
        {stats?.layers.l1 ? 'events.jsonl + SQLite' : 'disabled'}
      </div>
    </div>
    <div class="card">
      <div class="card-label">L2 Semantic</div>
      <div class="card-value">{stats?.l2_facts ?? 0}</div>
      <div class="card-sub">
        {stats?.layers.l2 ? 'facts (LanceDB)' : 'coming in M2'}
      </div>
    </div>
    <div class="card">
      <div class="card-label">L3 Archive</div>
      <div class="card-value">{stats?.l3_events ?? 0}</div>
      <div class="card-sub">gzipped monthly chunks</div>
    </div>
    <div class="card">
      <div class="card-label">Graph</div>
      <div class="card-value">
        {(stats?.l2_entities ?? 0)} / {(stats?.l2_edges ?? 0)}
      </div>
      <div class="card-sub">nodes / edges (M3)</div>
    </div>
    <div class="card">
      <div class="card-label">Disk</div>
      <div class="card-value">
        {stats ? fmtBytes(stats.disk_bytes) : '—'}
      </div>
      <div class="card-sub">schema v{stats?.schema_version ?? '?'}</div>
    </div>
  </section>

  <!-- Sub-tabs -->
  <nav class="subtabs" aria-label="Memory sections">
    <button class:on={active === 'activity'} on:click={() => (active = 'activity')}>
      Activity
    </button>
    <button class:on={active === 'search'} on:click={() => (active = 'search')}>
      Search
      <span class="hint">(M2)</span>
    </button>
    <button class:on={active === 'graph'} on:click={() => (active = 'graph')}>
      Graph
      <span class="hint">(M2)</span>
    </button>
  </nav>

  {#if active === 'activity'}
    <section class="activity">
      <div class="toolbar">
        <label>
          Filter by kind:
          <select bind:value={kindFilter} on:change={refresh}>
            <option value="">all</option>
            <option value="chat_turn">chat_turn</option>
            <option value="file_edit">file_edit</option>
            <option value="interest_update">interest_update</option>
            <option value="vision_trigger">vision_trigger</option>
            <option value="user_fact">user_fact</option>
            <option value="tool_call">tool_call</option>
          </select>
        </label>
        <button on:click={refresh} disabled={busy}>↻ Refresh</button>
        <button on:click={archiveNow} disabled={busy || !stats?.layers.l1}>
          Archive now (&gt; 90 days)
        </button>
        <span class="grow"></span>
        <span class="muted">{events.length} shown</span>
      </div>

      {#if lastConsolidation}
        <div class="banner banner-info">
          Archived {lastConsolidation.archived} event(s) in
          {lastConsolidation.elapsed_ms} ms. Files:
          {lastConsolidation.archive_files.join(', ') || '(none)'}
        </div>
      {/if}

      {#if events.length === 0}
        <div class="empty">
          No events yet. Chat with the assistant, edit a file, or
          update your interests — they'll show up here.
        </div>
      {:else}
        <ul class="evlist">
          {#each events as e (e.id)}
            <li>
              <div class="evrow">
                <span class={kindBadgeClass(e.kind)}>{e.kind}</span>
                <span class="ev-time">{fmtTime(e.ts)}</span>
                <span class="ev-src">{e.source}</span>
                {#if e.secret}
                  <span class="badge badge-red" title="Likely secret — filtered from auto-recall">secret</span>
                {/if}
                <span class="grow"></span>
                <button
                  class="link"
                  on:click={() => forgetOne(e.id)}
                  title="Forget this entry">✕</button>
              </div>
              <div class="ev-text">{e.content}</div>
              {#if e.tags.length > 0}
                <div class="ev-tags">
                  {#each e.tags as t}
                    <span class="tag">#{t}</span>
                  {/each}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {:else if active === 'search'}
    <section class="activity">
      <div class="toolbar">
        <input
          class="search-input"
          type="text"
          placeholder="Search facts and recent events…"
          bind:value={searchQuery}
          on:keydown={(e) => { if (e.key === 'Enter') doSearch(); }}
        />
        <button on:click={doSearch} disabled={searching || !searchQuery.trim()}>
          {searching ? 'Searching…' : 'Search'}
        </button>
        <span class="grow"></span>
        <span class="muted">
          L1 keyword + L2 cosine (RRF fusion)
        </span>
      </div>

      <div class="remember-form">
        <div class="remember-title">Remember a fact</div>
        <textarea
          class="remember-input"
          placeholder="User prefers Tauri over Electron for desktop apps…"
          bind:value={newFactText}
          rows="2"></textarea>
        <div class="remember-row">
          <label>
            Importance:
            <input
              type="range"
              min="0"
              max="1"
              step="0.05"
              bind:value={newFactImportance}
            />
            <span class="muted">{newFactImportance.toFixed(2)}</span>
          </label>
          <button on:click={rememberFact} disabled={busy || !newFactText.trim()}>
            Remember
          </button>
        </div>
      </div>

      {#if searchHits.length > 0}
        <ul class="evlist">
          {#each searchHits as h (h.id)}
            <li>
              <div class="evrow">
                <span class={recallLayerClass(h.layer)}>{h.layer}</span>
                <span class="ev-time">{fmtTime(h.ts)}</span>
                <span class="muted">score {(h.score * 100).toFixed(0)}%</span>
                {#if h.source}
                  <span class="ev-src">{h.source}</span>
                {/if}
              </div>
              <div class="ev-text">{h.text}</div>
            </li>
          {/each}
        </ul>
      {:else if !searching && searchQuery}
        <div class="empty">No hits for "{searchQuery}".</div>
      {:else if !searching}
        <div class="empty">
          Search across L1 events (keyword) and L2 facts (cosine).
          Results are merged with reciprocal rank fusion.
        </div>
      {/if}
    </section>
  {:else if active === 'graph'}
    <section class="activity">
      <div class="toolbar">
        <span class="muted">
          {graphEntities.length} entity / entities in the knowledge graph.
        </span>
        <span class="grow"></span>
        <span class="muted">
          Visualization with cytoscape.js lands in M3 (see ADR-0010).
        </span>
      </div>
      {#if graphEntities.length === 0}
        <div class="empty">
          No entities yet. They appear automatically as you chat —
          the agent's fact extractor adds them with each turn.
        </div>
      {:else}
        <table class="entity-table">
          <thead>
            <tr><th>name</th><th>kind</th><th>importance</th><th>ts</th></tr>
          </thead>
          <tbody>
            {#each graphEntities as e (e.id)}
              <tr>
                <td>{e.name}</td>
                <td><span class="badge badge-violet">{e.kind}</span></td>
                <td>{(e.importance * 100).toFixed(0)}%</td>
                <td class="muted">{fmtTime(e.ts)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}
</div>

<style>
  .memory-root {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 20px;
    height: 100%;
    overflow: auto;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    color: var(--text);
    background: var(--bg);
  }
  .hdr h2 { margin: 0 0 4px 0; font-size: 18px; font-weight: 600; }
  .hdr .sub { margin: 0; font-size: 12px; color: var(--text-muted); }
  .hdr code { background: var(--bg-elevated); padding: 1px 4px; border-radius: 3px; }

  .banner {
    padding: 8px 12px;
    border-radius: 6px;
    font-size: 12px;
    line-height: 1.4;
  }
  .banner-err  { background: var(--danger-soft,  #fee);  color: var(--danger,  #c33); border: 1px solid var(--danger); }
  .banner-warn { background: var(--warn-soft,   #fff8e1); color: var(--warn,   #b80); border: 1px solid var(--warn); }
  .banner-info { background: var(--accent-soft, #eef);  color: var(--accent,  #36c); border: 1px solid var(--accent); }

  .cards {
    display: grid;
    grid-template-columns: repeat(6, minmax(0, 1fr));
    gap: 8px;
  }
  .card {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .card-label { font-size: 10px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.5px; }
  .card-value { font-size: 18px; font-weight: 600; }
  .card-sub   { font-size: 10px; color: var(--text-muted); }

  .subtabs {
    display: flex;
    gap: 4px;
    border-bottom: 1px solid var(--border);
    padding-bottom: 0;
  }
  .subtabs button {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    padding: 6px 14px;
    cursor: pointer;
    font-family: inherit;
    font-size: 13px;
    border-bottom: 2px solid transparent;
  }
  .subtabs button:hover:not(:disabled) { color: var(--text); }
  .subtabs button.on { color: var(--text); border-bottom-color: var(--accent); }
  .subtabs button:disabled { opacity: 0.45; cursor: not-allowed; }
  .hint { font-size: 10px; color: var(--text-muted); margin-left: 4px; }

  .activity { display: flex; flex-direction: column; gap: 10px; }
  .toolbar {
    display: flex;
    gap: 8px;
    align-items: center;
    font-size: 12px;
  }
  .toolbar label { display: flex; align-items: center; gap: 6px; color: var(--text-muted); }
  .toolbar select, .toolbar button {
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 4px 10px;
    font-family: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .toolbar button:disabled { opacity: 0.5; cursor: not-allowed; }
  .toolbar .grow { flex: 1; }
  .toolbar .muted { color: var(--text-muted); }

  .empty {
    padding: 24px;
    text-align: center;
    color: var(--text-muted);
    font-size: 13px;
    background: var(--bg-elevated);
    border: 1px dashed var(--border);
    border-radius: 6px;
  }

  .evlist { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 6px; }
  .evlist li {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 12px;
  }
  .evrow { display: flex; gap: 8px; align-items: center; font-size: 11px; color: var(--text-muted); }
  .evrow .grow { flex: 1; }
  .ev-time { font-family: monospace; }
  .ev-src  { color: var(--text-muted); }
  .ev-text { font-size: 13px; color: var(--text); margin-top: 4px; word-break: break-word; }
  .ev-tags { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 4px; }
  .tag {
    font-size: 10px;
    color: var(--text-muted);
    background: var(--bg);
    padding: 1px 6px;
    border-radius: 3px;
  }
  .link {
    background: transparent;
    border: 0;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0 4px;
  }
  .link:hover { color: var(--danger); }

  .badge {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 3px;
    font-weight: 600;
    text-transform: lowercase;
    letter-spacing: 0.2px;
  }
  .badge-blue   { background: #dbeafe; color: #1e40af; }
  .badge-amber  { background: #fef3c7; color: #92400e; }
  .badge-violet { background: #ede9fe; color: #5b21b6; }
  .badge-rose   { background: #ffe4e6; color: #be123c; }
  .badge-green  { background: #d1fae5; color: #065f46; }
  .badge-gray   { background: #f3f4f6; color: #374151; }
  .badge-red    { background: #fee2e2; color: #991b1b; }

  .search-input {
    flex: 1;
    min-width: 200px;
    background: var(--bg-elevated);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 4px 10px;
    font-family: inherit;
    font-size: 12px;
  }
  .remember-form {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .remember-title { font-size: 12px; font-weight: 600; color: var(--text-muted); }
  .remember-input {
    background: var(--bg);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 6px 10px;
    font-family: inherit;
    font-size: 12px;
    resize: vertical;
  }
  .remember-row { display: flex; align-items: center; gap: 12px; }
  .remember-row input[type="range"] { vertical-align: middle; }
  .entity-table {
    width: 100%;
    border-collapse: collapse;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
    font-size: 12px;
  }
  .entity-table th, .entity-table td {
    text-align: left;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border);
  }
  .entity-table th {
    background: var(--bg);
    color: var(--text-muted);
    font-weight: 600;
    text-transform: uppercase;
    font-size: 10px;
    letter-spacing: 0.4px;
  }
  .entity-table tr:last-child td { border-bottom: 0; }
</style>
