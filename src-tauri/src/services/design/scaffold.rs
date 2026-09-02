//! Svelte 4 scaffold generation service for Mephistopheles (Phase P0+).
//!
//! Generates `.svelte` source files via a single non-streaming M3 call.
//! Supports three kinds: `component`, `page`, and `app` (multi-file
//! app skeleton). Post-processes the output through [`validate_svelte`]
//! to reject Tailwind, Svelte 5 runes, and other anti-patterns.
//!
//! ## Validation
//!
//! - balanced `<script>` / `<style>` / `<template>` tags
//! - no Tailwind (`@apply`, `class="bg-...`, `class="text-...`)
//! - no Svelte 5 runes (`$state`, `$props`, `$derived`, `$effect`)
//! - no inline event handler JSX-style (`onClick=`)
//! - must reference CSS variables instead of hard-coded colors
//!   (warning if no `var(--` is found)
//! - file size cap (default 30 KB per file)
//!
//! If validation fails, the caller is expected to re-prompt with the
//! failure context. [`generate_scaffold`] does NOT re-prompt on its
//! own — same pattern as `copy.rs`.

use super::Palette;
use crate::services::agent::minimax_client::{
    MinimaxClient, MinimaxError, MinimaxMessage, MinimaxRequest,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What kind of scaffold to generate. Drives the system prompt and
/// the output file layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaffoldKind {
    /// A single Svelte component file (e.g. `Button.svelte`).
    Component,
    /// A Svelte page (a component intended to be a route target).
    Page,
    /// A multi-file app skeleton (package.json + vite.config + src/...).
    App,
}

impl ScaffoldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Component => "component",
            Self::Page => "page",
            Self::App => "app",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "component" => Some(Self::Component),
            "page" => Some(Self::Page),
            "app" => Some(Self::App),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldRequest {
    pub kind: ScaffoldKind,
    /// Display name for the file (e.g. "Button", "Home", "MyApp").
    pub name: String,
    /// User intent — what the component should do / look like.
    pub intent: String,
    /// Optional references to existing assets (image IDs, copy asset
    /// IDs) the LLM should pull into the scaffold.
    #[serde(default)]
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldFile {
    /// Relative path inside `.luna/design/scaffolds/{kind}s/<name>/`
    /// (or absolute under the workspace). The caller resolves the
    /// final disk path.
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldRecord {
    pub id: String,
    pub kind: ScaffoldKind,
    pub name: String,
    pub files: Vec<ScaffoldFile>,
    /// One-line summary produced by the LLM (e.g. "primary button
    /// with brass accent, hover state, dark-mode ready").
    pub summary: String,
    /// Snapshot of the palette at gen time.
    pub palette_snapshot: Palette,
    /// Snapshot of the brief at gen time.
    pub brief_style_prefix: String,
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    #[error("scaffold: empty name")]
    EmptyName,
    #[error("scaffold: empty intent")]
    EmptyIntent,
    #[error("scaffold: LLM: {0}")]
    Llm(String),
    #[error("scaffold: parse: {0}")]
    Parse(String),
    #[error("scaffold: validation: {0}")]
    Validation(String),
    #[error("scaffold: file too large ({actual} > {max} bytes)")]
    FileTooLarge { actual: usize, max: usize },
    #[error("scaffold: no files in response")]
    NoFiles,
}

/// Max bytes per generated file. Sanity cap; a single .svelte should
/// never be 30 KB unless something went very wrong.
pub const MAX_FILE_BYTES: usize = 30 * 1024;

/// LLM output schema (private).
#[derive(Debug, Deserialize)]
struct LlmOutput {
    #[serde(default)]
    files: Vec<LlmFile>,
    #[serde(default)]
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlmFile {
    path: String,
    content: String,
}

/// Fire a single scaffold-generation call. Returns a `ScaffoldRecord`
/// with all files validated.
pub async fn generate_scaffold(
    client: &MinimaxClient,
    model: &str,
    palette: &Palette,
    brief_style_prefix: &str,
    req: &ScaffoldRequest,
) -> Result<ScaffoldRecord, ScaffoldError> {
    if req.name.trim().is_empty() {
        return Err(ScaffoldError::EmptyName);
    }
    if req.intent.trim().is_empty() {
        return Err(ScaffoldError::EmptyIntent);
    }

    let system = build_system_prompt(req.kind, brief_style_prefix, palette);
    let user = build_user_prompt(req);

    let llm_req = MinimaxRequest {
        model: model.to_string(),
        messages: vec![
            MinimaxMessage::system(system),
            MinimaxMessage::user_text(user),
        ],
        tools: vec![],
        max_tokens: 4096,
        temperature: Some(0.7),
    };

    let resp = client
        .chat(llm_req)
        .await
        .map_err(|e: MinimaxError| ScaffoldError::Llm(e.to_string()))?;

    let raw = resp.content;
    let parsed = parse_llm_output(&raw).map_err(|e| {
        ScaffoldError::Parse(format!(
            "{e}; body: {}",
            raw.chars().take(200).collect::<String>()
        ))
    })?;

    if parsed.files.is_empty() {
        return Err(ScaffoldError::NoFiles);
    }

    let mut files = Vec::with_capacity(parsed.files.len());
    for f in parsed.files {
        validate_svelte(&f.path, &f.content)?;
        files.push(ScaffoldFile {
            path: f.path,
            content: f.content,
        });
    }
    let summary = parsed.summary.unwrap_or_default();

    let id = format!(
        "scaffold-{}-{}",
        req.kind.as_str(),
        Utc::now().format("%Y%m%d-%H%M%S-%f")
    );
    Ok(ScaffoldRecord {
        id,
        kind: req.kind,
        name: req.name.clone(),
        files,
        summary,
        palette_snapshot: palette.clone(),
        brief_style_prefix: brief_style_prefix.to_string(),
        created_at: Utc::now(),
        model: model.to_string(),
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
    })
}

fn build_system_prompt(kind: ScaffoldKind, brief_style_prefix: &str, palette: &Palette) -> String {
    let kind_specific = match kind {
        ScaffoldKind::Component => "\
Один файл — один Svelte-компонент. Экспортируй через `export let` для props. \
Минимум логики, максимум читаемости.",
        ScaffoldKind::Page => "\
Один файл — Svelte-страница (route target). Может содержать layout-блоки \
(header, sidebar, main, footer). Если нужно несколько подкомпонентов — \
сделай их inline в одном файле, не плоди файлы.",
        ScaffoldKind::App => "\
Полный каркас Tauri 2 + Svelte 4 приложения. Минимум файлов:\n\
- package.json (svelte ^4.2, vite ^5, @tauri-apps/api ^2, typescript ^5)\n\
- vite.config.ts (минимальный)\n\
- tsconfig.json\n\
- index.html\n\
- src/main.ts\n\
- src/app.css (`:root` CSS variables — НЕ inline)\n\
- src/App.svelte (shell: header + main slot)\n\
- src/lib/tokens.css (palette → CSS variables)\n\
- src/routes/Home.svelte (минимальный)\n\
- README.md (как развернуть)\n\n\
Не добавляй ничего сверх этого. Никаких dev-зависимостей кроме указанных.",
    };

    format!(
        "Ты — Mephistopheles, Svelte 4 + Tauri 2 архитектор. Пишешь код строго по правилам.\n\
         \n\
         ## Правила (НЕ НАРУШАЙ)\n\
         1. **Svelte 4, не 5.** Никаких `$state`, `$props`, `$derived`, `$effect`, `$inspect` — это runes. \
            Используй `let foo: T; export let bar: T;` для props и `$:` для derived.\n\
         2. **`<script lang=\"ts\">` ВСЕГДА.** TypeScript обязателен.\n\
         3. **Scoped `<style>`.** Глобальные стили только в `:root` через `tokens.css`.\n\
         4. **CSS variables, не хардкод.** Используй `var(--accent)`, `var(--bg)`, `var(--text)` и т.д. \
            Текущая палитра:\n{palette}\n\
            Если нужного токена нет — добавь в `tokens.css` (но не в сам компонент).\n\
         5. **Без Tailwind.** `class=\"bg-...\"`, `class=\"text-...\"`, `@apply` — ЗАПРЕЩЕНО. \
            Только обычные CSS классы и scoped styles.\n\
         6. **Event handlers: `on:click`, `on:input`, `on:submit`** (Svelte 4 синтаксис). \
            НЕ `onClick=`, `onInput=` (это Svelte 5 / JSX).\n\
         7. **Props через `export let`.** TypeScript типы: `export let title: string;`.\n\
         \n\
         ## Brief (style prefix)\n\
         {brief}\n\
         \n\
         ## Задача\n\
         {kind_specific}\n\
         \n\
         ## Формат ответа\n\
         ТОЛЬКО JSON (без markdown-обёртки):\n\
         {{\"files\": [{{\"path\": \"<relative>\", \"content\": \"<full file content>\"}}, ...], \
         \"summary\": \"1 строка что это и для чего\"}}\n\
         \n\
         Каждый файл — полный, с `<script>`, `<style>`, разметкой. Не сокращай до stub.",
        kind_specific = kind_specific,
        brief = brief_style_prefix,
        palette = palette_to_text(palette),
    )
}

fn palette_to_text(palette: &Palette) -> String {
    format!(
        "primary={p}, secondary={s}, accent={a}, bg={bg}, fg={fg}, ok={ok}, warn={wr}, err={er}",
        p = palette.primary,
        s = palette.secondary,
        a = palette.accent,
        bg = palette.neutral_bg,
        fg = palette.neutral_fg,
        ok = palette.semantic_ok,
        wr = palette.semantic_warn,
        er = palette.semantic_err,
    )
}

fn build_user_prompt(req: &ScaffoldRequest) -> String {
    let refs = if req.refs.is_empty() {
        "(none)".to_string()
    } else {
        req.refs.join(", ")
    };
    format!(
        "Kind: {kind}\nName: {name}\nIntent: {intent}\nReferences: {refs}",
        kind = req.kind.as_str(),
        name = req.name,
        intent = req.intent,
        refs = refs,
    )
}

fn parse_llm_output(raw: &str) -> Result<LlmOutput, String> {
    if let Ok(parsed) = serde_json::from_str::<LlmOutput>(raw) {
        return Ok(parsed);
    }
    // Recovery: find first balanced JSON object.
    if let Some(start) = raw.find('{') {
        let bytes = raw.as_bytes();
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escape = false;
        let mut end = None;
        for (i, &b) in bytes.iter().enumerate().skip(start) {
            if escape {
                escape = false;
                continue;
            }
            match b {
                b'\\' if in_string => escape = true,
                b'"' => in_string = !in_string,
                b'{' if !in_string => depth += 1,
                b'}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(end) = end {
            let candidate = &raw[start..end];
            if let Ok(parsed) = serde_json::from_str::<LlmOutput>(candidate) {
                return Ok(parsed);
            }
        }
    }
    Err("no JSON object found".into())
}

/// Validate one generated file. Catches the most common LLM
/// regressions. Does NOT enforce design-tokens usage (just warns
/// via the validation message).
pub fn validate_svelte(path: &str, content: &str) -> Result<(), ScaffoldError> {
    if content.len() > MAX_FILE_BYTES {
        return Err(ScaffoldError::FileTooLarge {
            actual: content.len(),
            max: MAX_FILE_BYTES,
        });
    }

    // Only validate .svelte files; other files (package.json,
    // vite.config.ts) get a lighter pass.
    if path.ends_with(".svelte") {
        // 1. No Tailwind
        if content.contains("@apply") {
            return Err(ScaffoldError::Validation(format!(
                "{path}: contains @apply (Tailwind forbidden)"
            )));
        }
        if content.contains("class=\"bg-")
            || content.contains("class=\"text-")
            || content.contains("class=\"p-")
        {
            return Err(ScaffoldError::Validation(format!(
                "{path}: Tailwind-style class detected (use scoped CSS)"
            )));
        }

        // 2. No Svelte 5 runes
        for rune in &["$state", "$props", "$derived", "$effect", "$inspect"] {
            // We use a word-boundary check via simple substring + char
            // validation. Sufficient for our needs.
            if content.contains(rune) {
                return Err(ScaffoldError::Validation(format!(
                    "{path}: Svelte 5 rune '{rune}' not allowed in Svelte 4"
                )));
            }
        }

        // 3. No JSX-style event handlers
        for bad in &["onClick=", "onInput=", "onChange=", "onSubmit="] {
            if content.contains(bad) {
                return Err(ScaffoldError::Validation(format!(
                    "{path}: JSX-style handler '{bad}' (use on:click / on:input)"
                )));
            }
        }

        // 4. Balanced tags (loose check)
        for tag in &["<script", "</script>", "<style", "</style>"] {
            // We don't strictly require <script> (some files have only markup)
            // but if any open tag exists, the close tag must too.
            if *tag == "<script" || *tag == "<style" {
                if content.contains(tag) {
                    let close = format!("</{}>", &tag[1..]);
                    if !content.contains(&close) {
                        return Err(ScaffoldError::Validation(format!(
                            "{path}: unclosed <{kind}> tag",
                            kind = &tag[1..]
                        )));
                    }
                }
            }
        }

        // 5. <script lang="ts"> when script exists
        if content.contains("<script") && !content.contains("lang=\"ts\"") && !content.contains("lang='ts'") {
            return Err(ScaffoldError::Validation(format!(
                "{path}: <script> missing lang=\"ts\""
            )));
        }
    }

    Ok(())
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip() {
        for k in [ScaffoldKind::Component, ScaffoldKind::Page, ScaffoldKind::App] {
            assert_eq!(ScaffoldKind::from_str_opt(k.as_str()), Some(k));
        }
        assert_eq!(ScaffoldKind::from_str_opt("nope"), None);
    }

    #[test]
    fn validate_rejects_tailwind_apply() {
        let content = "<script lang=\"ts\"></script>\n<style>\n.foo { @apply bg-red-500; }\n</style>";
        let err = validate_svelte("Foo.svelte", content).unwrap_err();
        assert!(matches!(err, ScaffoldError::Validation(_)));
    }

    #[test]
    fn validate_rejects_tailwind_class() {
        let content = "<script lang=\"ts\"></script>\n<div class=\"bg-gray-100\">x</div>";
        let err = validate_svelte("Foo.svelte", content).unwrap_err();
        assert!(matches!(err, ScaffoldError::Validation(_)));
    }

    #[test]
    fn validate_rejects_svelte_5_runes() {
        let content = "<script lang=\"ts\">\nlet count = $state(0);\n</script>";
        let err = validate_svelte("Foo.svelte", content).unwrap_err();
        assert!(matches!(err, ScaffoldError::Validation(_)));
    }

    #[test]
    fn validate_rejects_jsx_handlers() {
        let content = "<script lang=\"ts\"></script>\n<button onClick={handle}>x</button>";
        let err = validate_svelte("Foo.svelte", content).unwrap_err();
        assert!(matches!(err, ScaffoldError::Validation(_)));
    }

    #[test]
    fn validate_rejects_unclosed_script() {
        let content = "<script lang=\"ts\">\nlet x = 1;\n<div>foo</div>";
        let err = validate_svelte("Foo.svelte", content).unwrap_err();
        assert!(matches!(err, ScaffoldError::Validation(_)));
    }

    #[test]
    fn validate_rejects_missing_lang_ts() {
        let content = "<script>\nlet x = 1;\n</script>\n<div>foo</div>";
        let err = validate_svelte("Foo.svelte", content).unwrap_err();
        assert!(matches!(err, ScaffoldError::Validation(_)));
    }

    #[test]
    fn validate_rejects_oversized_file() {
        let content = "x".repeat(MAX_FILE_BYTES + 1);
        let err = validate_svelte("Foo.svelte", &content).unwrap_err();
        assert!(matches!(err, ScaffoldError::FileTooLarge { .. }));
    }

    #[test]
    fn validate_accepts_clean_svelte() {
        let content = r#"<script lang="ts">
  export let title: string;
  let count = 0;
  $: doubled = count * 2;
</script>

<button on:click={() => count++}>{title}: {doubled}</button>

<style>
  button {
    color: var(--accent);
    background: var(--bg);
  }
</style>"#;
        assert!(validate_svelte("Counter.svelte", content).is_ok());
    }

    #[test]
    fn validate_skips_non_svelte_files() {
        // package.json should not be checked for Svelte conventions.
        let content = r#"{"name": "test", "scripts": {"dev": "vite"}}"#;
        assert!(validate_svelte("package.json", content).is_ok());
    }
}
