//! Token cost estimation (Phase M0+).
//!
//! Pricing per million tokens (USD). **Update these when the provider
//! changes rates.** The actual provider dashboard is the source of
//! truth; these are best-effort defaults used only for the UI's
//! "estimated cost" display.
//!
//! ## Sources
//! - MiniMax: https://www.minimaxi.com/pricing (2026-09)
//! - Anthropic: https://www.anthropic.com/pricing (2026-09)
//! - OpenAI: https://openai.com/api/pricing (2026-09)

/// Per-million-token USD price.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: f64,
}

/// Lookup pricing for a model id. Falls back to M3 if unknown.
pub fn pricing_for(model: &str) -> ModelPricing {
    match model {
        // M2.7-highspeed — used for sub-agents. Roughly 5–10x cheaper
        // than M3 (per MiniMax's published rates as of 2026-09).
        m if m.contains("M2.7") || m.contains("M2.7-highspeed") => ModelPricing {
            input_per_million: 0.10,
            output_per_million: 0.30,
            cache_read_per_million: 0.05,
        },
        // M3 — primary supervisor model.
        m if m.contains("M3") => ModelPricing {
            input_per_million: 0.80,
            output_per_million: 2.40,
            cache_read_per_million: 0.20,
        },
        // MiniMax-Text-01 (legacy).
        m if m.contains("abab") || m.contains("Text-01") => ModelPricing {
            input_per_million: 0.50,
            output_per_million: 1.50,
            cache_read_per_million: 0.10,
        },
        // ---- Anthropic Claude models (ai_chat_stream, extraction) ----
        // claude-3-5-sonnet (2025-06): $3/input, $15/output.
        m if m.contains("claude-3-5-sonnet") => ModelPricing {
            input_per_million: 3.00,
            output_per_million: 15.00,
            cache_read_per_million: 0.30,
        },
        // claude-3-opus: $15/input, $75/output.
        m if m.contains("claude-3-opus") || m.contains("claude-opus") => ModelPricing {
            input_per_million: 15.00,
            output_per_million: 75.00,
            cache_read_per_million: 1.50,
        },
        // claude-3-haiku: $0.80/input, $4/output (used for extraction).
        m if m.contains("claude-3-haiku") || m.contains("claude-haiku") => ModelPricing {
            input_per_million: 0.80,
            output_per_million: 4.00,
            cache_read_per_million: 0.08,
        },
        // ---- OpenAI models (future-proofing) ----
        m if m.contains("gpt-4o") => ModelPricing {
            input_per_million: 2.50,
            output_per_million: 10.00,
            cache_read_per_million: 1.25,
        },
        m if m.contains("gpt-4o-mini") => ModelPricing {
            input_per_million: 0.15,
            output_per_million: 0.60,
            cache_read_per_million: 0.075,
        },
        m if m.contains("gpt-4-turbo") || m.contains("gpt-4-32k") => ModelPricing {
            input_per_million: 10.00,
            output_per_million: 30.00,
            cache_read_per_million: 0.00,
        },
        // Unknown — fall back to M3 (the current default model).
        _ => ModelPricing {
            input_per_million: 0.80,
            output_per_million: 2.40,
            cache_read_per_million: 0.20,
        },
    }
}

/// Compute the cost of a single MiniMax response in USD.
///
/// `model` is the model id, `input_tokens` and `output_tokens` come from
/// the response (or from headers if the API returns them).
pub fn estimate_response_usd(model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
    let p = pricing_for(model);
    let input_cost = (input_tokens as f64) / 1_000_000.0 * p.input_per_million;
    let output_cost = (output_tokens as f64) / 1_000_000.0 * p.output_per_million;
    input_cost + output_cost
}

/// Apply the per-response cost to a `TaskCost` and update the running
/// USD estimate.
pub fn add_response_cost(
    cost: &mut super::task::TaskCost,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
) {
    cost.add_response(input_tokens, output_tokens);
    cost.estimated_usd += estimate_response_usd(model, input_tokens, output_tokens);
}

/// Apply a sub-agent response cost.
pub fn add_subagent_cost(
    cost: &mut super::task::TaskCost,
    sub_model: &str,
    input_tokens: u64,
    output_tokens: u64,
) {
    cost.add_subagent_response(input_tokens, output_tokens);
    cost.estimated_usd += estimate_response_usd(sub_model, input_tokens, output_tokens);
}

// =====================================================================
// Mephistopheles (P0+) — image + copy call cost
// =====================================================================
//
// Image generation (image-01) and the M3 copy-generation call are
// priced differently than token-based chat. We track them as flat
// per-call fees (in USD) on top of the token-based estimate. When
// the platform exposes per-image / per-call usage fields, these
// become fallbacks.

/// Per-image USD price for `image-01` at 1024².
pub const IMAGE_GEN_COST_PER_IMAGE_USD: f64 = 0.04;
/// Per-image USD price for `image-01` at 2K / HD.
pub const IMAGE_GEN_COST_PER_IMAGE_HD_USD: f64 = 0.08;
/// Per-call flat fee for a copy-generation request (3-7 variants on M3).
/// ~$0.015 is roughly the M3 cost of a 1-2K-token round-trip for a
/// structured JSON copy task. We round up to be safe.
pub const COPY_GEN_COST_PER_CALL_USD: f64 = 0.015;
/// Per-call flat fee for a scaffold-generation request (component / page / app).
/// Heavier than copy (4K output tokens), so ~$0.025.
pub const SCAFFOLD_GEN_COST_PER_CALL_USD: f64 = 0.025;

/// Apply image-generation cost to a `TaskCost`. The persona tool
/// tracks this separately from token usage so the UI can show
/// "X images × $0.04 = $0.16" in the cost breakdown.
pub fn add_image_gen_cost(cost: &mut super::task::TaskCost, n: u32, hd: bool) {
    let per_image = if hd {
        IMAGE_GEN_COST_PER_IMAGE_HD_USD
    } else {
        IMAGE_GEN_COST_PER_IMAGE_USD
    };
    cost.estimated_usd += per_image * n as f64;
}

/// Apply a copy-generation call cost (flat fee).
pub fn add_copy_cost(cost: &mut super::task::TaskCost) {
    cost.estimated_usd += COPY_GEN_COST_PER_CALL_USD;
}

/// Apply a scaffold-generation call cost (flat fee).
pub fn add_scaffold_cost(cost: &mut super::task::TaskCost) {
    cost.estimated_usd += SCAFFOLD_GEN_COST_PER_CALL_USD;
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::agent::task::TaskCost;

    #[test]
    fn pricing_known_models() {
        let p = pricing_for("MiniMax-M3");
        assert!(p.input_per_million > 0.0);
        assert!(p.output_per_million > p.input_per_million);
    }

    #[test]
    fn pricing_unknown_falls_back_to_m3() {
        let unknown = pricing_for("Mystery-Model-99");
        let m3 = pricing_for("MiniMax-M3");
        assert_eq!(unknown.input_per_million, m3.input_per_million);
        assert_eq!(unknown.output_per_million, m3.output_per_million);
    }

    #[test]
    fn pricing_legacy_text_01_uses_its_own_rates() {
        let legacy = pricing_for("MiniMax-Text-01 (abab6.5s)");
        let m3 = pricing_for("MiniMax-M3");
        assert_ne!(legacy.input_per_million, m3.input_per_million);
    }

    #[test]
    fn pricing_m27_cheaper_than_m3() {
        let m27 = pricing_for("MiniMax-M2.7-highspeed");
        let m3 = pricing_for("MiniMax-M3");
        assert!(m27.input_per_million < m3.input_per_million);
        assert!(m27.output_per_million < m3.output_per_million);
    }

    #[test]
    fn pricing_claude_35_sonnet_known_rates() {
        let p = pricing_for("claude-3-5-sonnet-20250620");
        assert!((p.input_per_million - 3.00).abs() < 1e-9);
        assert!((p.output_per_million - 15.00).abs() < 1e-9);
        assert!(p.output_per_million > p.input_per_million);
    }

    #[test]
    fn pricing_claude_haiku_cheapest_anthropic() {
        let haiku = pricing_for("claude-3-haiku-20240307");
        let sonnet = pricing_for("claude-3-5-sonnet-latest");
        assert!(haiku.input_per_million < sonnet.input_per_million);
        assert!(haiku.output_per_million < sonnet.output_per_million);
    }

    #[test]
    fn pricing_gpt_4o_known_rates() {
        let p = pricing_for("gpt-4o-2024-08-06");
        assert!((p.input_per_million - 2.50).abs() < 1e-9);
        assert!((p.output_per_million - 10.00).abs() < 1e-9);
        // Cache read should be half of input.
        assert!(p.cache_read_per_million < p.input_per_million);
    }

    #[test]
    fn estimate_response_usd_claude_haiku_extraction() {
        // Simulate an extraction call: ~500 input, ~200 output tokens.
        let usd = estimate_response_usd("claude-3-haiku-20240307", 500, 200);
        // 500/1M * $0.80 + 200/1M * $4.00 = $0.0004 + $0.0008 = $0.0012
        assert!((usd - 0.0012).abs() < 1e-5);
    }

    #[test]
    fn estimate_response_usd_claude_sonnet_chat() {
        // Simulate a chat call: ~2000 input, ~500 output tokens.
        let usd = estimate_response_usd("claude-3-5-sonnet-latest", 2000, 500);
        // 2000/1M * $3.00 + 500/1M * $15.00 = $0.006 + $0.0075 = $0.0135
        assert!((usd - 0.0135).abs() < 1e-5);
    }

    #[test]
    fn estimate_response_zero_tokens_zero_cost() {
        let usd = estimate_response_usd("MiniMax-M3", 0, 0);
        assert_eq!(usd, 0.0);
    }

    #[test]
    fn estimate_response_one_million_tokens_input_costs_one_input_price() {
        let usd = estimate_response_usd("MiniMax-M3", 1_000_000, 0);
        let p = pricing_for("MiniMax-M3");
        assert!((usd - p.input_per_million).abs() < 1e-9);
    }

    #[test]
    fn add_response_cost_accumulates() {
        let mut cost = TaskCost::default();
        add_response_cost(&mut cost, "MiniMax-M3", 1000, 500);
        assert_eq!(cost.input_tokens, 1000);
        assert_eq!(cost.output_tokens, 500);
        assert!(cost.estimated_usd > 0.0);
        add_response_cost(&mut cost, "MiniMax-M3", 1000, 500);
        assert_eq!(cost.input_tokens, 2000);
        assert_eq!(cost.output_tokens, 1000);
    }

    #[test]
    fn add_subagent_cost_separate_buckets() {
        let mut cost = TaskCost::default();
        add_response_cost(&mut cost, "MiniMax-M3", 100, 50);
        add_subagent_cost(&mut cost, "MiniMax-M2.7-highspeed", 200, 100);
        assert_eq!(cost.input_tokens, 100);
        assert_eq!(cost.output_tokens, 50);
        assert_eq!(cost.sub_agent_input_tokens, 200);
        assert_eq!(cost.sub_agent_output_tokens, 100);
    }

    #[test]
    fn add_image_gen_cost_scales_with_n() {
        let mut cost = TaskCost::default();
        add_image_gen_cost(&mut cost, 4, false);
        let expected = IMAGE_GEN_COST_PER_IMAGE_USD * 4.0;
        assert!((cost.estimated_usd - expected).abs() < 1e-9);
    }

    #[test]
    fn add_image_gen_cost_hd_uses_hd_price() {
        let mut cost = TaskCost::default();
        add_image_gen_cost(&mut cost, 1, true);
        assert!((cost.estimated_usd - IMAGE_GEN_COST_PER_IMAGE_HD_USD).abs() < 1e-9);
    }

    #[test]
    fn add_copy_cost_is_flat() {
        let mut cost = TaskCost::default();
        add_copy_cost(&mut cost);
        assert!((cost.estimated_usd - COPY_GEN_COST_PER_CALL_USD).abs() < 1e-9);
        add_copy_cost(&mut cost);
        assert!((cost.estimated_usd - 2.0 * COPY_GEN_COST_PER_CALL_USD).abs() < 1e-9);
    }

    #[test]
    fn add_scaffold_cost_is_flat() {
        let mut cost = TaskCost::default();
        add_scaffold_cost(&mut cost);
        assert!((cost.estimated_usd - SCAFFOLD_GEN_COST_PER_CALL_USD).abs() < 1e-9);
    }
}
