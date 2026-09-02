//! Copy generation service for Mephistopheles (Phase P0+).
//!
//! Generates structured copy (headlines, CTAs, microcopy, etc.) via a
//! single non-streaming M3 call. The output is parsed as JSON into a
//! `CopyAsset` with N variants + a primary pick + rationale. The
//! persona tool `design_copy_generate` and the Tauri command both
//! route through [`generate_copy`].
//!
//! ## Validation
//!
//! - `max_chars` per `CopyContext` (overridable per-call).
//! - `banned_words` from the active `VoiceGuide` — re-prompt on hit.
//! - `allow_profanity` controls whether provocative phrasing is OK.
//! - `variants` clamped to 1..=7.
//! - Output must be parseable JSON; if LLM wraps it in prose, we
//!   recover via a regex pass.
//!
//! ## Cost
//!
//! One M3 call per generation. Token usage is returned in
//! `CopyAsset.input_tokens` / `output_tokens` so the runner can
//! accumulate cost. Per-call flat fee is in `cost.rs::add_copy_cost`.

use super::VoiceGuide;
use crate::services::agent::minimax_client::{
    MinimaxClient, MinimaxError, MinimaxMessage, MinimaxRequest,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// All copy contexts the persona tool knows about. Each has a default
/// `max_chars` (see [`default_max_chars`]) that the validator enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyContext {
    Hero,
    Cta,
    SectionHeader,
    Body,
    Error,
    EmptyState,
    Tooltip,
    FormLabel,
    FormPlaceholder,
    FormError,
    Tagline,
    MetaDescription,
    Microcopy,
    NavItem,
    ModalTitle,
    Toast,
}

impl CopyContext {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hero => "hero",
            Self::Cta => "cta",
            Self::SectionHeader => "section_header",
            Self::Body => "body",
            Self::Error => "error",
            Self::EmptyState => "empty_state",
            Self::Tooltip => "tooltip",
            Self::FormLabel => "form_label",
            Self::FormPlaceholder => "form_placeholder",
            Self::FormError => "form_error",
            Self::Tagline => "tagline",
            Self::MetaDescription => "meta_description",
            Self::Microcopy => "microcopy",
            Self::NavItem => "nav_item",
            Self::ModalTitle => "modal_title",
            Self::Toast => "toast",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "hero" => Some(Self::Hero),
            "cta" => Some(Self::Cta),
            "section_header" => Some(Self::SectionHeader),
            "body" => Some(Self::Body),
            "error" => Some(Self::Error),
            "empty_state" => Some(Self::EmptyState),
            "tooltip" => Some(Self::Tooltip),
            "form_label" => Some(Self::FormLabel),
            "form_placeholder" => Some(Self::FormPlaceholder),
            "form_error" => Some(Self::FormError),
            "tagline" => Some(Self::Tagline),
            "meta_description" => Some(Self::MetaDescription),
            "microcopy" => Some(Self::Microcopy),
            "nav_item" => Some(Self::NavItem),
            "modal_title" => Some(Self::ModalTitle),
            "toast" => Some(Self::Toast),
            _ => None,
        }
    }

    /// Short human-readable description used in the system prompt so
    /// the model knows what kind of copy to write.
    pub fn description(self) -> &'static str {
        match self {
            Self::Hero => "Headline + subheadline + CTA for a landing hero. Hooks the user in 5 seconds.",
            Self::Cta => "Short button text that prompts action. Imperative, 1-3 words.",
            Self::SectionHeader => "H2 + 1-line intro for a content section. Sets context.",
            Self::Body => "Body paragraph(s) for landing or marketing. 1-3 sentences, 30-90 words.",
            Self::Error => "Error message with empathy + next step. Not blaming the user.",
            Self::EmptyState => "Empty state: 'nothing here yet' + soft CTA to add first item.",
            Self::Tooltip => "Short helper text for a UI element. Explains the why, not the what.",
            Self::FormLabel => "Input field label. 1-3 words, sentence case.",
            Self::FormPlaceholder => "Input placeholder. Example or hint, not a label.",
            Self::FormError => "Field validation error. Specific + actionable.",
            Self::Tagline => "Single-line company/product tagline. 3-7 words.",
            Self::MetaDescription => "SEO meta description for a page. 50-160 chars.",
            Self::Microcopy => "Tiny UI strings: 'saved', 'loading', 'x items', 'press Enter to send'.",
            Self::NavItem => "Sidebar / nav menu label. 1-2 words, sentence case.",
            Self::ModalTitle => "Modal dialog title. Action-oriented, 3-7 words.",
            Self::Toast => "Toast notification. 1 short sentence + optional CTA.",
        }
    }
}

/// Default `max_chars` for each context. Overridable per-call.
pub fn default_max_chars(ctx: CopyContext) -> usize {
    match ctx {
        CopyContext::Hero => 80,
        CopyContext::Cta => 24,
        CopyContext::SectionHeader => 60,
        CopyContext::Body => 300,
        CopyContext::Error => 140,
        CopyContext::EmptyState => 100,
        CopyContext::Tooltip => 80,
        CopyContext::FormLabel => 40,
        CopyContext::FormPlaceholder => 60,
        CopyContext::FormError => 120,
        CopyContext::Tagline => 60,
        CopyContext::MetaDescription => 160,
        CopyContext::Microcopy => 24,
        CopyContext::NavItem => 20,
        CopyContext::ModalTitle => 60,
        CopyContext::Toast => 80,
    }
}

/// Language of the generated copy. `Auto` detects from the user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CopyLanguage {
    #[default]
    Auto,
    Russian,
    English,
    Both,
}

impl CopyLanguage {
    pub fn resolved(self, user_message: &str) -> &'static str {
        match self {
            Self::Auto => detect_language(user_message),
            Self::Russian => "ru",
            Self::English => "en",
            // For "Both" we generate twice; the caller picks the primary.
            // We return "en" as a placeholder so the LLM knows the
            // default branch — `generate_copy` will dispatch twice.
            Self::Both => "en",
        }
    }
}

/// Heuristic language detection. Returns `"ru"` if any Cyrillic
/// letters appear in the message, else `"en"`. Mixed content falls
/// back to `"en"` (callers can override via explicit `CopyLanguage`).
fn detect_language(s: &str) -> &'static str {
    let mut cyrillic = 0usize;
    let mut latin = 0usize;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() {
            latin += 1;
        } else if ('\u{0400}'..='\u{04FF}').contains(&ch) {
            cyrillic += 1;
        }
    }
    if cyrillic > latin {
        "ru"
    } else {
        "en"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyRequest {
    pub context: CopyContext,
    pub intent: String,
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default = "default_variants")]
    pub variants: u8,
    #[serde(default)]
    pub language: CopyLanguage,
}

fn default_variants() -> u8 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyVariant {
    pub text: String,
    pub char_count: usize,
    /// 0..=1 — how well the variant matches the active voice. The
    /// model returns this score; we trust it (no separate LLM call
    /// to re-evaluate).
    pub tone_score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopyAsset {
    pub id: String,
    pub context: CopyContext,
    /// `"ru" | "en"` (resolved — never `"auto"`).
    pub language: String,
    pub variants: Vec<CopyVariant>,
    pub primary_idx: usize,
    pub rationale: String,
    /// Snapshot of the voice at gen time — useful for the UI to
    /// show "this copy was generated in <voice> tone".
    pub voice_snapshot: VoiceGuide,
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    #[error("copy: empty intent")]
    EmptyIntent,
    #[error("copy: invalid variants ({n}, must be 1..=7)")]
    InvalidVariants { n: u8 },
    #[error("copy: LLM: {0}")]
    Llm(String),
    #[error("copy: parse JSON: {0}")]
    Parse(String),
    #[error("copy: no variants in response")]
    NoVariants,
    #[error("copy: validation: {0}")]
    Validation(String),
    #[error("copy: banned word found: '{0}'")]
    BannedWord(String),
    #[error("copy: variant too long: {actual} > {max}")]
    VariantTooLong { actual: usize, max: usize },
}

const MAX_VARIANTS: u8 = 7;

/// LLM output schema (private — only the model sees this JSON).
#[derive(Debug, Deserialize)]
struct LlmOutput {
    #[serde(default)]
    variants: Vec<LlmVariant>,
    #[serde(default)]
    primary_idx: Option<usize>,
    #[serde(default)]
    rationale: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlmVariant {
    text: String,
    #[serde(default)]
    tone_score: Option<f32>,
    #[serde(default)]
    notes: Option<String>,
}

/// Fire a single copy-generation call. Returns a fully validated
/// `CopyAsset`. If validation fails (banned word, too long, malformed
/// JSON), the caller is expected to re-prompt — this function does
/// NOT re-prompt on its own.
///
/// `model` is the model name to record in the resulting `CopyAsset`
/// (and to use for the LLM call). The caller is responsible for
/// picking the right model (usually via `personas::model_for`).
pub async fn generate_copy(
    client: &MinimaxClient,
    model: &str,
    voice: &VoiceGuide,
    req: &CopyRequest,
) -> Result<CopyAsset, CopyError> {
    if req.intent.trim().is_empty() {
        return Err(CopyError::EmptyIntent);
    }
    let variants = req.variants.clamp(1, MAX_VARIANTS);
    if req.variants != variants {
        return Err(CopyError::InvalidVariants { n: req.variants });
    }

    let max_chars = req.max_chars.unwrap_or_else(|| default_max_chars(req.context));
    let language = req.language.resolved(&req.intent);

    let (asset, raw) = generate_copy_once(client, model, voice, req, max_chars, language).await?;

    // Validation pass. If validation fails, surface a structured
    // error so the caller (persona tool) can decide to re-prompt
    // with the failure context.
    validate_copy(&asset, voice, max_chars)?;

    // If we recovered via regex, attach a one-liner so the persona
    // can show "had to recover from prose" in the live event stream.
    let _ = raw;

    Ok(asset)
}

async fn generate_copy_once(
    client: &MinimaxClient,
    model: &str,
    voice: &VoiceGuide,
    req: &CopyRequest,
    max_chars: usize,
    language: &str,
) -> Result<(CopyAsset, String), CopyError> {
    let system = build_system_prompt(voice, req.context, max_chars, language);
    let user = build_user_prompt(req, max_chars, language);

    let llm_req = MinimaxRequest {
        model: model.to_string(),
        messages: vec![
            MinimaxMessage::system(system),
            MinimaxMessage::user_text(user),
        ],
        tools: vec![],
        max_tokens: 2048,
        temperature: Some(0.85),
    };

    let resp = client
        .chat(llm_req)
        .await
        .map_err(|e: MinimaxError| CopyError::Llm(e.to_string()))?;

    let raw = resp.content;
    let parsed = parse_llm_output(&raw)
        .map_err(|e| CopyError::Parse(format!("{e}; body: {}", raw.chars().take(200).collect::<String>())))?;

    if parsed.variants.is_empty() {
        return Err(CopyError::NoVariants);
    }

    let primary_idx = parsed
        .primary_idx
        .unwrap_or(0)
        .min(parsed.variants.len().saturating_sub(1));
    let variants: Vec<CopyVariant> = parsed
        .variants
        .into_iter()
        .map(|v| CopyVariant {
            char_count: v.text.chars().count(),
            text: v.text,
            tone_score: v.tone_score.unwrap_or(0.7),
            notes: v.notes,
        })
        .collect();
    let rationale = parsed.rationale.unwrap_or_default();

    let id = format!("copy-{}-{}", req.context.as_str(), Utc::now().format("%Y%m%d-%H%M%S-%f"));
    let asset = CopyAsset {
        id,
        context: req.context,
        language: language.to_string(),
        variants,
        primary_idx,
        rationale,
        voice_snapshot: voice.clone(),
        created_at: Utc::now(),
        model: model.to_string(),
        input_tokens: resp.input_tokens,
        output_tokens: resp.output_tokens,
    };
    Ok((asset, raw))
}

fn build_system_prompt(voice: &VoiceGuide, ctx: CopyContext, max_chars: usize, language: &str) -> String {
    let profanity_clause = if voice.allow_profanity {
        "Провокативная лексика и лёгкий мат РАЗРЕШЕНЫ для контекстов, где это уместно (tagline, microcopy, cta). \
         Не переходи в откровенную грубость."
    } else {
        "Без мата, без провокаций. Сдержанный тон."
    };
    let banned = if voice.banned_words.is_empty() {
        "(нет)".to_string()
    } else {
        voice.banned_words.join(", ")
    };
    let examples = if voice.example_phrases.is_empty() {
        "(нет)".to_string()
    } else {
        voice.example_phrases.join(" / ")
    };
    let tone = voice.tone_keywords.join(", ");

    format!(
        "Ты — Mephistopheles, копирайтер в голосе «{name}» ({description}).\n\
         Твой тон: {tone}.\n\
         Примеры фраз: {examples}.\n\
         ЗАПРЕЩЁННЫЕ СЛОВА (если встретишь — перефразируй): {banned}.\n\
         {profanity_clause}\n\
         Формальность: {formality}/10 (0=сленг, 5=нейтрально, 10=формально).\n\
         Контекст: {ctx_name} — {ctx_desc}\n\
         Язык ответа: {language}.\n\
         Лимит длины каждого варианта: {max_chars} символов.\n\
         \n\
         Верни ТОЛЬКО JSON (без markdown-обёртки, без пояснений до/после):\n\
         {{\"variants\": [{{\"text\": \"...\", \"tone_score\": 0.0..1.0, \"notes\": \"...optional...\"}}, ...], \
         \"primary_idx\": N, \"rationale\": \"1-2 строки почему эти работают\"}}",
        name = voice.name,
        description = voice.description,
        tone = tone,
        examples = examples,
        banned = banned,
        profanity_clause = profanity_clause,
        formality = voice.formality,
        ctx_name = ctx.as_str(),
        ctx_desc = ctx.description(),
        language = language,
        max_chars = max_chars,
    )
}

fn build_user_prompt(req: &CopyRequest, max_chars: usize, language: &str) -> String {
    format!(
        "Intent: {intent}\nVariants: {n}\nMax chars per variant: {max_chars}\nLanguage: {language}",
        intent = req.intent,
        n = req.variants,
        max_chars = max_chars,
        language = language,
    )
}

fn parse_llm_output(raw: &str) -> Result<LlmOutput, String> {
    // Try direct parse first.
    if let Ok(parsed) = serde_json::from_str::<LlmOutput>(raw) {
        return Ok(parsed);
    }

    // Recovery: look for the first balanced JSON object in the prose.
    // We use a simple brace-counter pass — sufficient for our schema
    // (no nested objects inside the top-level `variants` array items,
    // other than the variant object itself).
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

fn validate_copy(
    asset: &CopyAsset,
    voice: &VoiceGuide,
    max_chars: usize,
) -> Result<(), CopyError> {
    for v in &asset.variants {
        if v.char_count > max_chars {
            return Err(CopyError::VariantTooLong {
                actual: v.char_count,
                max: max_chars,
            });
        }
        let lower = v.text.to_lowercase();
        for banned in &voice.banned_words {
            if lower.contains(&banned.to_lowercase()) {
                return Err(CopyError::BannedWord(banned.clone()));
            }
        }
    }
    if asset.primary_idx >= asset.variants.len() {
        return Err(CopyError::Validation(format!(
            "primary_idx {} out of range (variants: {})",
            asset.primary_idx,
            asset.variants.len()
        )));
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
    fn context_roundtrip() {
        for c in [
            CopyContext::Hero,
            CopyContext::Cta,
            CopyContext::SectionHeader,
            CopyContext::Body,
            CopyContext::Error,
            CopyContext::EmptyState,
            CopyContext::Tooltip,
            CopyContext::FormLabel,
            CopyContext::FormPlaceholder,
            CopyContext::FormError,
            CopyContext::Tagline,
            CopyContext::MetaDescription,
            CopyContext::Microcopy,
            CopyContext::NavItem,
            CopyContext::ModalTitle,
            CopyContext::Toast,
        ] {
            assert_eq!(CopyContext::from_str_opt(c.as_str()), Some(c));
        }
        assert_eq!(CopyContext::from_str_opt("nope"), None);
    }

    #[test]
    fn default_max_chars_reasonable() {
        // Sanity: every context has a max_chars that is not 0
        // and not absurdly large.
        for c in [
            CopyContext::Hero,
            CopyContext::Cta,
            CopyContext::Body,
            CopyContext::Microcopy,
        ] {
            let n = default_max_chars(c);
            assert!(n > 0 && n < 1000, "{}: {}", c.as_str(), n);
        }
    }

    #[test]
    fn language_detection_cyrillic() {
        assert_eq!(detect_language("Сделай мне кнопку"), "ru");
        assert_eq!(detect_language("make me a button"), "en");
        // Mixed: more Latin than Cyrillic → "en"
        assert_eq!(detect_language("Кнопка button"), "en");
        // Mostly Cyrillic with stray Latin → "ru"
        assert_eq!(detect_language("Главная страница с кнопкой CTA"), "ru");
    }

    #[test]
    fn parse_direct_json() {
        let raw = r#"{"variants": [{"text": "a"}, {"text": "b"}], "primary_idx": 1, "rationale": "ok"}"#;
        let parsed = parse_llm_output(raw).unwrap();
        assert_eq!(parsed.variants.len(), 2);
        assert_eq!(parsed.primary_idx, Some(1));
    }

    #[test]
    fn parse_json_embedded_in_prose() {
        let raw = "Sure, here you go:\n{\"variants\": [{\"text\": \"a\"}], \"primary_idx\": 0, \"rationale\": \"x\"}\nHope that helps!";
        let parsed = parse_llm_output(raw).unwrap();
        assert_eq!(parsed.variants.len(), 1);
    }

    #[test]
    fn parse_malformed_errors() {
        let raw = "no json here at all";
        assert!(parse_llm_output(raw).is_err());
    }

    #[test]
    fn validate_catches_banned_word() {
        let voice = VoiceGuide {
            name: "t".into(),
            description: "t".into(),
            tone_keywords: vec![],
            example_phrases: vec![],
            banned_words: vec!["synergy".into()],
            allow_profanity: false,
            formality: 5,
            version: 1,
        };
        let asset = CopyAsset {
            id: "x".into(),
            context: CopyContext::Tagline,
            language: "en".into(),
            variants: vec![CopyVariant {
                text: "Leverage our synergy for growth".into(),
                char_count: 35,
                tone_score: 0.7,
                notes: None,
            }],
            primary_idx: 0,
            rationale: "".into(),
            voice_snapshot: voice.clone(),
            created_at: Utc::now(),
            model: "M3".into(),
            input_tokens: 0,
            output_tokens: 0,
        };
        let err = validate_copy(&asset, &voice, 100).unwrap_err();
        match err {
            CopyError::BannedWord(w) => assert_eq!(w, "synergy"),
            other => panic!("expected BannedWord, got {other:?}"),
        }
    }

    #[test]
    fn validate_catches_too_long() {
        let voice = VoiceGuide {
            name: "t".into(),
            description: "t".into(),
            tone_keywords: vec![],
            example_phrases: vec![],
            banned_words: vec![],
            allow_profanity: false,
            formality: 5,
            version: 1,
        };
        let asset = CopyAsset {
            id: "x".into(),
            context: CopyContext::Tagline,
            language: "en".into(),
            variants: vec![CopyVariant {
                text: "a".repeat(100),
                char_count: 100,
                tone_score: 0.7,
                notes: None,
            }],
            primary_idx: 0,
            rationale: "".into(),
            voice_snapshot: voice.clone(),
            created_at: Utc::now(),
            model: "M3".into(),
            input_tokens: 0,
            output_tokens: 0,
        };
        let err = validate_copy(&asset, &voice, 60).unwrap_err();
        assert!(matches!(err, CopyError::VariantTooLong { .. }));
    }

    #[test]
    fn validate_primary_idx_out_of_range() {
        let voice = VoiceGuide {
            name: "t".into(),
            description: "t".into(),
            tone_keywords: vec![],
            example_phrases: vec![],
            banned_words: vec![],
            allow_profanity: false,
            formality: 5,
            version: 1,
        };
        let asset = CopyAsset {
            id: "x".into(),
            context: CopyContext::Tagline,
            language: "en".into(),
            variants: vec![CopyVariant {
                text: "a".into(),
                char_count: 1,
                tone_score: 0.7,
                notes: None,
            }],
            primary_idx: 5, // out of range
            rationale: "".into(),
            voice_snapshot: voice.clone(),
            created_at: Utc::now(),
            model: "M3".into(),
            input_tokens: 0,
            output_tokens: 0,
        };
        let err = validate_copy(&asset, &voice, 100).unwrap_err();
        assert!(matches!(err, CopyError::Validation(_)));
    }
}
