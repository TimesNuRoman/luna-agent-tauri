//! Token cost estimation (Phase M0+).
//!
//! Pricing per million tokens (USD). **Update these when the provider
//! changes rates.** The actual `MiniMax` dashboard is the source of
//! truth; these are best-effort defaults used only for the UI's
//! "estimated cost" display.
//!
//! ## Why not query the API?
//! Some providers return token counts only on the response payload,
//! not in headers. We prefer to keep cost estimation local + explicit
//! (and overridable via Settings in a future v1.1).

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
}
