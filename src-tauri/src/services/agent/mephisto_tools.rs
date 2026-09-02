//! Mephistopheles persona tools (Phase P0+ Luna Agent).
//!
//! 9 design tools that drive the `DesignService` from the Mephistopheles
//! persona's supervisor loop. All tools follow the same dispatch
//! pattern as `persona_tools::execute_persona_tool` — return
//! `ToolOutcome` with the JSON-serialized result on success or an
//! `is_error: true` payload on failure.
//!
//! The 9 tools:
//! - `design_manifest_get` / `design_manifest_set` — DesignSystem CRUD
//! - `design_brief_get` / `design_brief_set` — DesignBrief CRUD
//! - `design_palette_generate` — M3 LLM-call to generate a palette
//! - `design_image_generate` — image-01 + save to .luna/design/images/
//! - `design_scaffold_generate` — M3 Svelte generation + validation
//! - `design_copy_generate` — M3 copy generation + variants
//! - `design_copy_apply` — replace `{{copy:ctx}}` placeholders
//! - `design_component_propose` — M3 HTML+CSS preview snippet
//! - `design_apply` — export tokens / apply scaffold to user's src/

use super::persona_tools::PersonaToolContext;
use super::supervisor::ToolOutcome;
use crate::services::design::{
    copy as design_copy, image_gen as design_image_gen, scaffold as design_scaffold,
    CopyContext, CopyLanguage, CopyRequest, DesignBrief, DesignService, DesignSystem, ImageAspect,
    ImageGenRequest, Palette, ScaffoldKind, ScaffoldRequest, VoiceGuide,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Tool name constants. Also added to `PERSONA_TOOL_NAMES` in
/// `persona_tools.rs` so the supervisor routes them here.
pub const MEPHISTO_TOOL_NAMES: &[&str] = &[
    "design_manifest_get",
    "design_manifest_set",
    "design_brief_get",
    "design_brief_set",
    "design_palette_generate",
    "design_image_generate",
    "design_scaffold_generate",
    "design_copy_generate",
    "design_copy_apply",
    "design_component_propose",
    "design_apply",
];

/// True if `name` is a Mephistopheles design tool. The supervisor
/// uses this — together with `is_persona_tool` — to dispatch.
pub fn is_mephisto_tool(name: &str) -> bool {
    MEPHISTO_TOOL_NAMES.contains(&name)
}

/// Dispatch a Mephistopheles design tool. Returns the `tool` message
/// content the model will see. On error, sets `is_error = true`.
pub async fn execute_mephisto_tool(
    name: &str,
    args: &serde_json::Value,
    ctx: &PersonaToolContext,
) -> ToolOutcome {
    let Some(svc) = ctx.design.as_ref() else {
        return ToolOutcome {
            content: format!("error: design service not initialized (no workspace open?)"),
            is_error: true,
        };
    };

    match name {
        "design_manifest_get" => tool_manifest_get(svc),
        "design_manifest_set" => tool_manifest_set(args, svc),
        "design_brief_get" => tool_brief_get(svc),
        "design_brief_set" => tool_brief_set(args, svc),
        "design_palette_generate" => tool_palette_generate(args, svc).await,
        "design_image_generate" => tool_image_generate(args, svc).await,
        "design_scaffold_generate" => tool_scaffold_generate(args, svc).await,
        "design_copy_generate" => tool_copy_generate(args, svc).await,
        "design_copy_apply" => tool_copy_apply(args, svc),
        "design_component_propose" => tool_component_propose(args, svc).await,
        "design_apply" => tool_design_apply(args, svc),
        _ => ToolOutcome {
            content: format!("error: unknown mephisto tool '{name}'"),
            is_error: true,
        },
    }
}

// =====================================================================
// Tool handlers
// =====================================================================

fn tool_manifest_get(svc: &Arc<DesignService>) -> ToolOutcome {
    let m = svc.get_manifest();
    match serde_json::to_string_pretty(&m) {
        Ok(s) => ToolOutcome { content: s, is_error: false },
        Err(e) => ToolOutcome {
            content: format!("error: serialize manifest: {e}"),
            is_error: true,
        },
    }
}

fn tool_manifest_set(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let m: DesignSystem = match serde_json::from_value(args.clone()) {
        Ok(m) => m,
        Err(e) => {
            return ToolOutcome {
                content: format!("error: invalid manifest: {e}"),
                is_error: true,
            }
        }
    };
    match svc.set_manifest(m) {
        Ok(v) => ToolOutcome {
            content: format!("ok: manifest version = {v}"),
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: set manifest: {e}"),
            is_error: true,
        },
    }
}

fn tool_brief_get(svc: &Arc<DesignService>) -> ToolOutcome {
    let b = svc.get_brief();
    match serde_json::to_string_pretty(&b) {
        Ok(s) => ToolOutcome { content: s, is_error: false },
        Err(e) => ToolOutcome {
            content: format!("error: serialize brief: {e}"),
            is_error: true,
        },
    }
}

fn tool_brief_set(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let b: DesignBrief = match serde_json::from_value(args.clone()) {
        Ok(b) => b,
        Err(e) => {
            return ToolOutcome {
                content: format!("error: invalid brief: {e}"),
                is_error: true,
            }
        }
    };
    match svc.set_brief(b) {
        Ok(()) => ToolOutcome {
            content: "ok: brief saved".to_string(),
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: set brief: {e}"),
            is_error: true,
        },
    }
}

async fn tool_palette_generate(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let mood = args.get("mood").and_then(|v| v.as_str()).unwrap_or("dark");
    let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("#1a1a1a");
    let model = "MiniMax-M3"; // TODO: pull from personas::model_for

    let system = format!(
        "Ты — Mephistopheles, дизайнер палитр. Генерируешь JSON-палитру из 8 hex-цветов \
         под mood «{mood}» с базой {base}. Constraints:\n\
         - WCAG AA contrast ≥ 4.5:1 между `neutral_bg` и `neutral_fg`\n\
         - primary/secondary/accent — брендовые, оттенки друг друга\n\
         - semantic_ok/warn/err — зелёный/жёлтый/красный с читаемостью на bg\n\
         \n\
         Верни ТОЛЬКО JSON: {{\"primary\": \"#...\", \"secondary\": \"#...\", \"accent\": \"#...\", \
         \"neutral_bg\": \"#...\", \"neutral_fg\": \"#...\", \"semantic_ok\": \"#...\", \
         \"semantic_warn\": \"#...\", \"semantic_err\": \"#...\"}}"
    );
    let user = format!("mood: {mood}\nbase: {base}");

    let (raw, _in_t, _out_t) = match svc.llm_call(model, &system, &user, 512).await {
        Ok(r) => r,
        Err(e) => {
            return ToolOutcome {
                content: format!("error: LLM: {e}"),
                is_error: true,
            }
        }
    };

    // Parse JSON. The model may wrap in prose — try direct first, then
    // brace-balanced recovery.
    let parsed: serde_json::Value = match serde_json::from_str(&raw)
        .or_else(|_| extract_json(&raw))
    {
        Ok(v) => v,
        Err(e) => {
            return ToolOutcome {
                content: format!("error: parse palette JSON: {e}; body: {}", raw.chars().take(200).collect::<String>()),
                is_error: true,
            }
        }
    };

    let mut p = match serde_json::from_value::<Palette>(parsed.clone()) {
        Ok(p) => p,
        Err(_) => {
            // Try to map the JSON to our Palette shape with sensible defaults.
            Palette {
                primary: parsed.get("primary").and_then(|v| v.as_str()).unwrap_or("#c9a45c").to_string(),
                secondary: parsed.get("secondary").and_then(|v| v.as_str()).unwrap_or("#8a6f3a").to_string(),
                accent: parsed.get("accent").and_then(|v| v.as_str()).unwrap_or("#d4a04a").to_string(),
                neutral_bg: parsed.get("neutral_bg").and_then(|v| v.as_str()).unwrap_or("#0a0a0c").to_string(),
                neutral_fg: parsed.get("neutral_fg").and_then(|v| v.as_str()).unwrap_or("#e8e3d8").to_string(),
                semantic_ok: parsed.get("semantic_ok").and_then(|v| v.as_str()).unwrap_or("#4a9b5e").to_string(),
                semantic_warn: parsed.get("semantic_warn").and_then(|v| v.as_str()).unwrap_or("#d4a04a").to_string(),
                semantic_err: parsed.get("semantic_err").and_then(|v| v.as_str()).unwrap_or("#c9504a").to_string(),
                version: 1,
            }
        }
    };
    p.version = svc.get_palette().version + 1;

    match svc.set_palette(p.clone()) {
        Ok(v) => ToolOutcome {
            content: format!(
                "ok: palette generated and saved (version = {v})\n{}",
                serde_json::to_string_pretty(&p).unwrap_or_default()
            ),
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: save palette: {e}"),
            is_error: true,
        },
    }
}

async fn tool_image_generate(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let request = match args.get("request").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolOutcome {
                content: "error: 'request' is required".to_string(),
                is_error: true,
            }
        }
    };
    let aspect = args
        .get("aspect")
        .and_then(|v| v.as_str())
        .and_then(ImageAspect::from_str_opt)
        .unwrap_or(svc.get_brief().aspect_default);
    let n = args.get("n").and_then(|v| v.as_u64()).unwrap_or(1) as u8;

    // Rate-limit guard: 10 req/min. We don't track minute windows here;
    // a simpler check is to bound `n` to 4 per call. The image_gen
    // module clamps n internally.
    let n_clamped = n.clamp(1, 4);
    if n_clamped != n {
        return ToolOutcome {
            content: format!("error: n={n} out of range (1..=4)"),
            is_error: true,
        };
    }

    match svc.generate_and_save_images(request, aspect, n_clamped).await {
        Ok(records) => {
            let summary = json!({
                "count": records.len(),
                "images": records.iter().map(|r| json!({
                    "id": r.id,
                    "path": r.file.display().to_string(),
                    "aspect": r.aspect.as_str(),
                    "model": r.model,
                })).collect::<Vec<_>>(),
                "cost_usd": records.len() as f64 * 0.04,
                "request": request,
            });
            ToolOutcome {
                content: serde_json::to_string_pretty(&summary).unwrap_or_default(),
                is_error: false,
            }
        }
        Err(e) => ToolOutcome {
            content: format!("error: image-gen: {e}"),
            is_error: true,
        },
    }
}

async fn tool_scaffold_generate(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let kind = match args.get("kind").and_then(|v| v.as_str()).and_then(ScaffoldKind::from_str_opt) {
        Some(k) => k,
        None => {
            return ToolOutcome {
                content: "error: 'kind' must be 'component' | 'page' | 'app'".to_string(),
                is_error: true,
            }
        }
    };
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolOutcome {
                content: "error: 'name' is required".to_string(),
                is_error: true,
            }
        }
    };
    let intent = args.get("intent").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let refs: Vec<String> = args
        .get("refs")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let model = "MiniMax-M3";

    let req = ScaffoldRequest {
        kind,
        name: name.to_string(),
        intent,
        refs,
    };

    match svc.generate_and_save_scaffold(model, req).await {
        Ok(rec) => {
            let summary = json!({
                "id": rec.id,
                "kind": rec.kind.as_str(),
                "name": rec.name,
                "summary": rec.summary,
                "files": rec.files.iter().map(|f| json!({
                    "path": f.path,
                    "size": f.content.len(),
                })).collect::<Vec<_>>(),
                "tokens": { "in": rec.input_tokens, "out": rec.output_tokens },
            });
            ToolOutcome {
                content: serde_json::to_string_pretty(&summary).unwrap_or_default(),
                is_error: false,
            }
        }
        Err(e) => ToolOutcome {
            content: format!("error: scaffold-gen: {e}"),
            is_error: true,
        },
    }
}

async fn tool_copy_generate(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let context = match args.get("context").and_then(|v| v.as_str()).and_then(CopyContext::from_str_opt) {
        Some(c) => c,
        None => {
            return ToolOutcome {
                content: "error: 'context' must be one of: hero | cta | section_header | body | error | empty_state | tooltip | form_label | form_placeholder | form_error | tagline | meta_description | microcopy | nav_item | modal_title | toast".to_string(),
                is_error: true,
            }
        }
    };
    let intent = match args.get("intent").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return ToolOutcome {
                content: "error: 'intent' is required".to_string(),
                is_error: true,
            }
        }
    };
    let max_chars = args.get("max_chars").and_then(|v| v.as_u64()).map(|n| n as usize);
    let variants = args.get("variants").and_then(|v| v.as_u64()).unwrap_or(3) as u8;
    let language = match args.get("language").and_then(|v| v.as_str()) {
        Some("russian") | Some("ru") => CopyLanguage::Russian,
        Some("english") | Some("en") => CopyLanguage::English,
        Some("both") => CopyLanguage::Both,
        _ => CopyLanguage::Auto,
    };
    let model = "MiniMax-M3";

    let req = CopyRequest {
        context,
        intent,
        max_chars,
        variants,
        language,
    };

    match svc.generate_and_save_copy(model, req).await {
        Ok(asset) => ToolOutcome {
            content: serde_json::to_string_pretty(&asset).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: copy-gen: {e}"),
            is_error: true,
        },
    }
}

fn tool_copy_apply(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let scaffold_id = match args.get("scaffold_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolOutcome {
                content: "error: 'scaffold_id' is required".to_string(),
                is_error: true,
            }
        }
    };
    let replacements = match args.get("replacements").and_then(|v| v.as_object()) {
        Some(o) => o
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect::<std::collections::HashMap<_, _>>(),
        None => {
            return ToolOutcome {
                content: "error: 'replacements' must be an object {placeholder: variant_text}".to_string(),
                is_error: true,
            }
        }
    };

    // Find the scaffold directory by ID.
    let mut scaffold_dir: Option<PathBuf> = None;
    for kind_dir in ["components", "pages", "apps"] {
        let dir = svc.workspace_root().join("scaffolds").join(kind_dir);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if entry.file_name().to_string_lossy().contains(scaffold_id) {
                    if entry.path().is_dir() {
                        scaffold_dir = Some(entry.path());
                        break;
                    }
                }
            }
        }
        if scaffold_dir.is_some() {
            break;
        }
    }
    let dir = match scaffold_dir {
        Some(d) => d,
        None => {
            return ToolOutcome {
                content: format!("error: scaffold '{scaffold_id}' not found under .luna/design/scaffolds/"),
                is_error: true,
            }
        }
    };

    let mut changed = Vec::new();
    let walk = walkdir::WalkDir::new(&dir);
    for entry in walk.into_iter().filter_map(|e| e.ok()) {
        if !entry.path().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|s| s.to_str()) != Some("svelte") {
            continue;
        }
        let raw = match std::fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut new = raw.clone();
        for (placeholder, variant) in &replacements {
            let token = format!("{{{{copy:{placeholder}}}}}");
            new = new.replace(&token, variant);
        }
        if new != raw {
            if std::fs::write(entry.path(), &new).is_ok() {
                changed.push(entry.path().display().to_string());
            }
        }
    }
    ToolOutcome {
        content: format!(
            "ok: applied {} replacement(s) across {} file(s):\n{}",
            replacements.len(),
            changed.len(),
            changed.join("\n")
        ),
        is_error: false,
    }
}

async fn tool_component_propose(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let kind = match args.get("kind").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolOutcome {
                content: "error: 'kind' is required (e.g. 'button' | 'card' | 'input')".to_string(),
                is_error: true,
            }
        }
    };
    let style_ref = args.get("style_ref").and_then(|v| v.as_str()).unwrap_or("primary");
    let model = "MiniMax-M3";

    let palette = svc.get_palette();
    let brief = svc.get_brief();

    let system = format!(
        "Ты — Mephistopheles. Генерируешь HTML+CSS сниппет компонента «{kind}» (стиль: {style_ref}).\n\
         \n\
         Constraints:\n\
         - Inline HTML+CSS, без JS\n\
         - Используй CSS variables (текущая палитра): {p}\n\
         - Без Tailwind, без внешних шрифтов\n\
         - Dark-first (background: var(--bg))\n\
         \n\
         Верни ТОЛЬКО JSON: {{\"html\": \"...\", \"css\": \"...\"}}",
        p = serde_json::to_string(&palette).unwrap_or_default(),
        style_ref = style_ref,
    );
    let user = format!("Kind: {kind}\nStyle: {style_ref}\nBrief mood: {}", brief.mood);

    let (raw, _in_t, _out_t) = match svc.llm_call(model, &system, &user, 2048).await {
        Ok(r) => r,
        Err(e) => {
            return ToolOutcome {
                content: format!("error: LLM: {e}"),
                is_error: true,
            }
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&raw).or_else(|_| extract_json(&raw)) {
        Ok(v) => v,
        Err(e) => {
            return ToolOutcome {
                content: format!("error: parse component JSON: {e}"),
                is_error: true,
            }
        }
    };
    ToolOutcome {
        content: serde_json::to_string_pretty(&parsed).unwrap_or_default(),
        is_error: false,
    }
}

fn tool_design_apply(args: &serde_json::Value, svc: &Arc<DesignService>) -> ToolOutcome {
    let target = match args.get("target_path").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolOutcome {
                content: "error: 'target_path' is required (e.g. 'src/styles/tokens.css')".to_string(),
                is_error: true,
            }
        }
    };
    let format = args.get("format").and_then(|v| v.as_str()).unwrap_or("css");

    // Allow-list on target_path. We restrict to safe prefixes.
    let allowed_prefixes = [
        "src/styles/",
        "src/lib/styles/",
        "src/tokens.",
    ];
    if !allowed_prefixes.iter().any(|p| target.starts_with(p)) {
        return ToolOutcome {
            content: format!(
                "error: target_path '{target}' not in allow-list (allowed: {})",
                allowed_prefixes.join(", ")
            ),
            is_error: true,
        };
    }

    let content = match format {
        "css" => build_tokens_css_for_svc(svc),
        _ => {
            return ToolOutcome {
                content: format!("error: format '{format}' not yet supported (css only for now)"),
                is_error: true,
            }
        }
    };

    // Resolve target relative to the workspace root (parent of .luna/).
    let workspace_root = svc.workspace_root().parent().and_then(|p| p.parent()).map(|p| p.to_path_buf());
    let Some(ws) = workspace_root else {
        return ToolOutcome {
            content: "error: cannot resolve workspace root from design service path".to_string(),
            is_error: true,
        };
    };
    let out_path = ws.join(target);
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&out_path, content.as_bytes()) {
        Ok(()) => ToolOutcome {
            content: format!("ok: tokens exported to {}", out_path.display()),
            is_error: false,
        },
        Err(e) => ToolOutcome {
            content: format!("error: write file: {e}"),
            is_error: true,
        },
    }
}

// =====================================================================
// Helpers
// =====================================================================

/// Try to extract the first balanced JSON object from a string. Used
/// as a fallback when the model wraps JSON in prose.
fn extract_json(s: &str) -> Result<serde_json::Value, String> {
    if let Some(start) = s.find('{') {
        let bytes = s.as_bytes();
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
            let candidate = &s[start..end];
            return serde_json::from_str(candidate).map_err(|e| e.to_string());
        }
    }
    Err("no JSON object found".into())
}

/// Re-export build_tokens_css at the design module level so the
/// persona tool can call it without importing `export` directly.
fn build_tokens_css_for_svc(svc: &DesignService) -> String {
    let p = svc.get_palette();
    crate::services::design::export::build_tokens_css(&p)
}
