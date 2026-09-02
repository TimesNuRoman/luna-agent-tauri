<script lang="ts">
  /**
   * DesignStudio — Mephistopheles side panel.
   *
   * Shows the current design system (manifest / brief / palette / voice),
   * the most recent images, copy blocks, and scaffolds. Edits go through
   * the persona tools (which call the Rust side). Read-only on disk
   * files (scaffolds) — the user applies them to src/ via the persona.
   */
  import { onMount, createEventDispatcher } from 'svelte';
  import {
    mephistoGetState,
    type DesignState,
    type ImageRecord,
    type CopyAsset,
    type CopyContext,
    type Palette,
    type DesignBrief,
    type VoiceGuide,
  } from './lib/designClient';
  import { onTaskFinished, onTaskProgress } from './lib/taskClient';

  const dispatch = createEventDispatcher<{ switch: { mode: 'design' | 'tasks' | 'plans' } }>();

  // ---- State ----
  let loading = false;
  let error: string | null = null;
  let state: DesignState | null = null;
  let pollInterval: ReturnType<typeof setInterval> | null = null;
  let activeSection: 'palette' | 'voice' | 'brief' | 'images' | 'copy' | 'scaffolds' = 'palette';

  // Edit buffers (for in-place edits to brief / palette / voice).
  let briefEdit: string = '';
  let paletteEdit: string = '';
  let voiceEdit: string = '';
  let editMode: Record<string, boolean> = { brief: false, palette: false, voice: false };
  let editError: string | null = null;

  // ---- Helpers ----
  async function refresh() {
    loading = true;
    error = null;
    try {
      state = await mephistoGetState();
      if (state) {
        if (!editMode.brief) briefEdit = JSON.stringify(state.brief, null, 2);
        if (!editMode.palette) paletteEdit = JSON.stringify(state.palette, null, 2);
        if (!editMode.voice) voiceEdit = JSON.stringify(state.voice, null, 2);
      }
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function switchToTasks() {
    dispatch('switch', { mode: 'tasks' });
  }

  function copyToClipboard(text: string) {
    if (navigator.clipboard) {
      navigator.clipboard.writeText(text).catch(() => {});
    }
  }

  function startEdit(key: 'brief' | 'palette' | 'voice') {
    editMode = { ...editMode, [key]: true };
    editError = null;
  }
  function cancelEdit(key: 'brief' | 'palette' | 'voice') {
    editMode = { ...editMode, [key]: false };
    editError = null;
  }

  // ---- Mount ----
  onMount(() => {
    refresh();
    // Refresh every 3s while the panel is open. Cheap because the
    // payload is small (manifest + brief + recent 24 items).
    pollInterval = setInterval(refresh, 3000);

    // Auto-refresh when a Mephistopheles task finishes — the new
    // design artifacts are now on disk.
    const unsubFinished = onTaskFinished(() => refresh());
    // Also refresh on persona_payload events (live streaming).
    const unsubProgress = onTaskProgress(() => {
      // Debounce: refresh at most once per second.
      if (!(window as any).__ds_pending) {
        (window as any).__ds_pending = true;
        setTimeout(() => {
          (window as any).__ds_pending = false;
          refresh();
        }, 1000);
      }
    });

    return () => {
      if (pollInterval) clearInterval(pollInterval);
      unsubFinished();
      unsubProgress();
    };
  });

  // ---- Render helpers ----
  function copyContextLabel(c: CopyContext): string {
    return ({
      hero: 'Hero',
      cta: 'CTA',
      section_header: 'Section Header',
      body: 'Body',
      error: 'Error',
      empty_state: 'Empty State',
      tooltip: 'Tooltip',
      form_label: 'Form Label',
      form_placeholder: 'Form Placeholder',
      form_error: 'Form Error',
      tagline: 'Tagline',
      meta_description: 'Meta Description',
      microcopy: 'Microcopy',
      nav_item: 'Nav Item',
      modal_title: 'Modal Title',
      toast: 'Toast',
    } as Record<CopyContext, string>)[c];
  }

  function imageSrc(rec: ImageRecord): string {
    // Tauri 2 asset protocol: convert absolute path to asset URL.
    // For dev we use the file:// scheme.
    if (typeof window !== 'undefined' && (window as any).__TAURI__) {
      const tauri = (window as any).__TAURI__;
      if (tauri.core?.convertFileSrc) {
        return tauri.core.convertFileSrc(rec.file);
      }
    }
    return `file:///${rec.file.replace(/\\/g, '/')}`;
  }

  function timeAgo(iso: string | undefined): string {
    if (!iso) return '—';
    try {
      const d = new Date(iso);
      const sec = Math.floor((Date.now() - d.getTime()) / 1000);
      if (sec < 60) return `${sec}s ago`;
      if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
      if (sec < 86400) return `${Math.floor(sec / 3600)}h ago`;
      return `${Math.floor(sec / 86400)}d ago`;
    } catch {
      return iso;
    }
  }

  // Reactive: swatch list (key + label + value). Recomputed when
  // state.palette changes.
  $: swatches = state
    ? [
        { key: 'primary', label: 'Primary', value: state.palette.primary },
        { key: 'secondary', label: 'Secondary', value: state.palette.secondary },
        { key: 'accent', label: 'Accent', value: state.palette.accent },
        { key: 'neutral_bg', label: 'BG', value: state.palette.neutral_bg },
        { key: 'neutral_fg', label: 'FG', value: state.palette.neutral_fg },
        { key: 'semantic_ok', label: 'OK', value: state.palette.semantic_ok },
        { key: 'semantic_warn', label: 'Warn', value: state.palette.semantic_warn },
        { key: 'semantic_err', label: 'Error', value: state.palette.semantic_err },
      ]
    : [];
</script>

<aside class="ds-root" data-testid="design-studio">
  <header class="ds-header">
    <div>
      <h3>🎭 Design Studio</h3>
      {#if state}
        <span class="ds-badge">{state.manifest.name} · v{state.manifest.version}</span>
      {/if}
    </div>
    <div class="ds-header-actions">
      <button class="ds-switch" type="button" on:click={switchToTasks} title="Tasks sidebar">📋</button>
      <button class="ds-refresh" on:click={refresh} disabled={loading} title="Refresh">{loading ? '…' : '↻'}</button>
    </div>
  </header>

  {#if error}
    <div class="ds-error" role="alert">
      <strong>Error:</strong> {error}
      <p class="muted">Is the workspace open? Open a folder first.</p>
    </div>
  {:else if !state}
    <p class="ds-empty">Loading design state…</p>
  {:else}
    <!-- Section nav -->
    <nav class="ds-nav">
      {#each ['palette', 'voice', 'brief', 'images', 'copy', 'scaffolds'] as sec}
        <button
          class="ds-nav-btn"
          class:on={activeSection === sec}
          type="button"
          on:click={() => (activeSection = sec)}
        >
          {sec}
          {#if sec === 'images' && state.images.length}
            <span class="ds-nav-count">{state.images.length}</span>
          {/if}
          {#if sec === 'copy' && state.copy.length}
            <span class="ds-nav-count">{state.copy.length}</span>
          {/if}
        </button>
      {/each}
    </nav>

    <div class="ds-body">
      <!-- ============ PALETTE ============ -->
      {#if activeSection === 'palette'}
        <section class="ds-section">
          <h4>Palette <span class="muted">v{state.palette.version}</span></h4>
          <div class="ds-swatches">
            {#each swatches as sw}
              <div class="ds-swatch" title={sw.value}>
                <div class="ds-swatch-color" style="background: {sw.value};"></div>
                <div class="ds-swatch-label">
                  <strong>{sw.label}</strong>
                  <code>{sw.value}</code>
                </div>
              </div>
            {/each}
          </div>
          <p class="ds-footnote muted">
            CSS variables auto-generated in <code>tokens.css</code> at <code>{state.workspace_root}</code>
          </p>
        </section>

      <!-- ============ VOICE ============ -->
      {:else if activeSection === 'voice'}
        <section class="ds-section">
          <h4>Voice <span class="muted">{state.voice.name} · v{state.voice.version}</span></h4>
          <p class="ds-voice-desc">{state.voice.description}</p>

          <div class="ds-voice-row">
            <strong>Tone:</strong>
            <div class="ds-chips">
              {#each state.voice.tone_keywords as kw}
                <span class="ds-chip">{kw}</span>
              {/each}
            </div>
          </div>

          <div class="ds-voice-row">
            <strong>Examples:</strong>
            <ul class="ds-list">
              {#each state.voice.example_phrases as ex}
                <li><em>"{ex}"</em></li>
              {/each}
            </ul>
          </div>

          <div class="ds-voice-row">
            <strong>Banned:</strong>
            <div class="ds-chips">
              {#each state.voice.banned_words as bw}
                <span class="ds-chip ds-chip-bad">{bw}</span>
              {/each}
            </div>
          </div>

          <div class="ds-voice-row">
            <strong>Formality:</strong>
            <code>{state.voice.formality}/10</code>
            <strong>Profanity:</strong>
            <code>{state.voice.allow_profanity ? 'allowed (Manson context)' : 'strict'}</code>
          </div>
        </section>

      <!-- ============ BRIEF ============ -->
      {:else if activeSection === 'brief'}
        <section class="ds-section">
          <h4>Brief</h4>
          {#if !editMode.brief}
            <div class="ds-brief-block">
              <div class="ds-brief-field">
                <strong>Style prefix:</strong>
                <p class="ds-brief-text">{state.brief.style_prefix}</p>
              </div>
              <div class="ds-brief-field">
                <strong>Mood:</strong>
                <p>{state.brief.mood}</p>
              </div>
              <div class="ds-brief-field">
                <strong>Anti-patterns:</strong>
                <div class="ds-chips">
                  {#each state.brief.anti_patterns as ap}
                    <span class="ds-chip ds-chip-bad">{ap}</span>
                  {/each}
                </div>
              </div>
              <button class="ds-btn" type="button" on:click={() => startEdit('brief')}>Edit</button>
            </div>
          {:else}
            <textarea class="ds-textarea" rows="14" bind:value={briefEdit}></textarea>
            {#if editError}
              <p class="ds-error-mini">{editError}</p>
            {/if}
            <div class="ds-row">
              <button class="ds-btn ds-btn-primary" type="button" disabled>Save (in Mephisto chat)</button>
              <button class="ds-btn" type="button" on:click={() => cancelEdit('brief')}>Cancel</button>
            </div>
            <p class="ds-footnote muted">
              Note: edit through <code>/design brief ...</code> in chat, or via persona tool <code>design_brief_set</code>.
            </p>
          {/if}
        </section>

      <!-- ============ IMAGES ============ -->
      {:else if activeSection === 'images'}
        <section class="ds-section">
          <h4>Images <span class="muted">last {state.images.length}</span></h4>
          {#if state.images.length === 0}
            <p class="ds-empty">No images yet. Use <code>/design image "dark throne room"</code> in chat.</p>
          {:else}
            <div class="ds-grid">
              {#each state.images as img}
                <figure class="ds-image">
                  <img src={imageSrc(img)} alt={img.prompt.slice(0, 60)} loading="lazy" />
                  <figcaption>
                    <code>{img.aspect}</code>
                    <span class="muted">{timeAgo(img.created_at)}</span>
                  </figcaption>
                </figure>
              {/each}
            </div>
          {/if}
        </section>

      <!-- ============ COPY ============ -->
      {:else if activeSection === 'copy'}
        <section class="ds-section">
          <h4>Copy <span class="muted">last {state.copy.length}</span></h4>
          {#if state.copy.length === 0}
            <p class="ds-empty">No copy yet. Use <code>/design copy hero "main landing"</code> in chat.</p>
          {:else}
            {#each state.copy as c}
              <div class="ds-copy-card">
                <div class="ds-copy-head">
                  <strong>{copyContextLabel(c.context)}</strong>
                  <code class="muted">{c.language} · {c.variants.length} variants</code>
                </div>
                <div class="ds-copy-primary">
                  <p>{c.variants[c.primary_idx]?.text ?? c.variants[0].text}</p>
                  <button class="ds-btn-mini" type="button" on:click={() => copyToClipboard(c.variants[c.primary_idx]?.text ?? c.variants[0].text)} title="Copy primary">📋</button>
                </div>
                {#if c.variants.length > 1}
                  <details class="ds-copy-variants">
                    <summary>other variants ({c.variants.length - 1})</summary>
                    <ul>
                      {#each c.variants as v, i}
                        {#if i !== c.primary_idx}
                          <li>
                            <span>{v.text}</span>
                            <button class="ds-btn-mini" type="button" on:click={() => copyToClipboard(v.text)} title="Copy">📋</button>
                          </li>
                        {/if}
                      {/each}
                    </ul>
                  </details>
                {/if}
                {#if c.rationale}
                  <p class="ds-copy-rationale muted">💡 {c.rationale}</p>
                {/if}
              </div>
            {/each}
          {/if}
        </section>

      <!-- ============ SCAFFOLDS ============ -->
      {:else if activeSection === 'scaffolds'}
        <section class="ds-section">
          <h4>Scaffolds</h4>
          <p class="ds-empty">
            Scaffolds live in <code>{state.workspace_root}/scaffolds/</code>. Use <code>/design component Button "primary brass"</code> in chat to generate, then <code>design_apply</code> persona tool to copy into <code>src/</code>.
          </p>
        </section>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .ds-root {
    width: 280px;
    flex-shrink: 0;
    border-right: 1px solid var(--border, #e3e3e6);
    background: var(--bg-elevated, #fafafa);
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }
  .ds-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ds-header h3 { margin: 0; font-size: 13px; font-weight: 600; }
  .ds-badge {
    display: inline-block;
    background: var(--accent, #c9a45c);
    color: #fff;
    padding: 1px 7px;
    border-radius: 8px;
    font-size: 10px;
    font-weight: 600;
  }
  .ds-header-actions { display: flex; gap: 2px; }
  .ds-switch, .ds-refresh {
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 4px 8px;
    border-radius: 4px;
  }
  .ds-switch:hover, .ds-refresh:hover:not(:disabled) { background: rgba(0,0,0,0.05); }
  .ds-refresh:disabled { opacity: 0.5; }

  .ds-nav {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ds-nav-btn {
    flex: 1 1 30%;
    border: none;
    background: transparent;
    color: var(--text-muted, #6b6b70);
    padding: 4px 6px;
    font-size: 11px;
    border-radius: 4px;
    cursor: pointer;
    text-transform: lowercase;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
  }
  .ds-nav-btn.on { background: var(--bg, #fff); color: var(--text, #1c1c1e); font-weight: 500; }
  .ds-nav-count {
    font-size: 9px;
    background: var(--accent, #c9a45c);
    color: #fff;
    padding: 0 4px;
    border-radius: 6px;
  }

  .ds-body { flex: 1; overflow-y: auto; padding: 12px; }
  .ds-section h4 {
    margin: 0 0 10px 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--text, #1c1c1e);
  }
  .ds-section p { font-size: 12px; line-height: 1.5; }
  .ds-section code {
    font-family: ui-monospace, monospace;
    font-size: 11px;
    background: rgba(0,0,0,0.04);
    padding: 1px 4px;
    border-radius: 3px;
  }

  .ds-swatches {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }
  .ds-swatch {
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 4px;
    overflow: hidden;
    background: var(--bg, #fff);
  }
  .ds-swatch-color {
    height: 32px;
    border-bottom: 1px solid var(--border, #e3e3e6);
  }
  .ds-swatch-label {
    padding: 4px 6px;
    font-size: 10px;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .ds-swatch-label code { font-size: 9px; }

  .ds-voice-row {
    margin-bottom: 10px;
    font-size: 12px;
  }
  .ds-voice-row > strong { display: block; margin-bottom: 4px; }
  .ds-voice-desc { font-style: italic; color: var(--text-muted, #6b6b70); margin: 0 0 10px 0; }
  .ds-chips { display: flex; flex-wrap: wrap; gap: 3px; }
  .ds-chip {
    display: inline-block;
    background: var(--bg, #fff);
    border: 1px solid var(--border, #d0d0d4);
    padding: 2px 7px;
    border-radius: 8px;
    font-size: 10px;
  }
  .ds-chip-bad {
    background: rgba(201, 80, 74, 0.1);
    border-color: rgba(201, 80, 74, 0.4);
    color: #6b1a1a;
  }
  .ds-list { padding-left: 18px; margin: 4px 0; font-size: 12px; }

  .ds-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }
  .ds-image {
    margin: 0;
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 4px;
    overflow: hidden;
    background: var(--bg, #fff);
  }
  .ds-image img {
    width: 100%;
    aspect-ratio: 1 / 1;
    object-fit: cover;
    display: block;
  }
  .ds-image figcaption {
    padding: 3px 6px;
    font-size: 10px;
    display: flex;
    justify-content: space-between;
  }
  .ds-image figcaption code { font-size: 9px; }

  .ds-copy-card {
    border: 1px solid var(--border, #e3e3e6);
    border-radius: 6px;
    padding: 8px 10px;
    margin-bottom: 8px;
    background: var(--bg, #fff);
  }
  .ds-copy-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 6px;
    font-size: 11px;
  }
  .ds-copy-primary {
    display: flex;
    align-items: flex-start;
    gap: 6px;
  }
  .ds-copy-primary p {
    flex: 1;
    margin: 0;
    font-size: 12px;
    line-height: 1.4;
  }
  .ds-copy-variants summary {
    cursor: pointer;
    font-size: 10px;
    color: var(--text-muted, #6b6b70);
    margin-top: 6px;
  }
  .ds-copy-variants ul {
    list-style: none;
    padding: 0;
    margin: 4px 0 0 0;
  }
  .ds-copy-variants li {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 6px;
    padding: 3px 0;
    font-size: 11px;
    border-top: 1px solid var(--border, #e3e3e6);
  }
  .ds-copy-rationale {
    font-size: 10px;
    margin: 6px 0 0 0;
    padding-top: 6px;
    border-top: 1px dashed var(--border, #e3e3e6);
  }
  .ds-btn-mini {
    border: none;
    background: transparent;
    cursor: pointer;
    font-size: 12px;
    padding: 1px 4px;
    border-radius: 3px;
  }
  .ds-btn-mini:hover { background: rgba(0,0,0,0.06); }

  .ds-footnote { font-size: 10px; margin-top: 8px; }
  .ds-footnote code { font-size: 9px; }

  .ds-empty {
    color: var(--text-muted, #6b6b70);
    font-size: 12px;
    text-align: center;
    padding: 18px 12px;
  }
  .ds-error {
    margin: 8px 12px;
    padding: 6px 8px;
    background: rgba(176, 48, 48, 0.08);
    border: 1px solid #b03030;
    border-radius: 4px;
    font-size: 12px;
    color: #6b1a1a;
  }
  .ds-error-mini {
    color: #6b1a1a;
    font-size: 11px;
    margin: 4px 0;
  }
  .ds-brief-field { margin-bottom: 8px; }
  .ds-brief-text { font-style: italic; font-size: 11px; line-height: 1.5; }
  .ds-textarea {
    width: 100%;
    box-sizing: border-box;
    font-family: ui-monospace, monospace;
    font-size: 11px;
    padding: 6px;
    border: 1px solid var(--border, #d0d0d4);
    border-radius: 4px;
    background: var(--bg, #fff);
    color: var(--text, #1c1c1e);
  }
  .ds-row { display: flex; gap: 6px; margin-top: 6px; }
  .ds-btn {
    border: 1px solid var(--border, #d0d0d4);
    background: var(--bg, #fff);
    color: var(--text, #1c1c1e);
    padding: 4px 10px;
    border-radius: 4px;
    font-size: 11px;
    cursor: pointer;
  }
  .ds-btn:hover:not(:disabled) { background: rgba(0,0,0,0.04); }
  .ds-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .ds-btn-primary { background: var(--accent, #c9a45c); color: #fff; border-color: var(--accent, #c9a45c); }
  .muted { color: var(--text-muted, #6b6b70); }
</style>
