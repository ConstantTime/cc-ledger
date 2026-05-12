//! Cost computation for Anthropic API pricing.
//!
//! Pricing data is loaded from (in priority order):
//!   1. `$CC_LEDGER_PRICING` (env path)
//!   2. `~/.cc-ledger/pricing.toml` (user override)
//!   3. The embedded fallback baked at build time (see `pricing.toml`).
//!
//! Models are matched by **longest-prefix-wins** so distinct snapshots
//! resolve correctly (e.g. `claude-opus-4-7-20260301` matches the
//! `claude-opus-4-7` row, not the older `claude-opus-4` one).
//!
//! Prices change. Bump `pricing_version` in the TOML when they do; the
//! version is stored on each `turns` row so historical numbers stay
//! interpretable.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::store::queries::TurnTokens;

const EMBEDDED_PRICING_TOML: &str = include_str!("pricing.toml");

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PricingTable {
    pub pricing_version: i64,
    pub as_of: String,
    #[serde(default)]
    pub source: Option<String>,
    pub models: Vec<ModelPricing>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelPricing {
    pub prefix: String,
    pub input_per_mtok: f64,
    pub cache_write_5m_per_mtok: f64,
    pub cache_write_1h_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl PricingTable {
    /// Resolve the active pricing table. `user_path` is typically
    /// `paths::pricing_path()`; pass `None` to skip the user-file step.
    pub fn load(user_path: Option<&Path>) -> Result<Self> {
        if let Some(p) = std::env::var_os("CC_LEDGER_PRICING") {
            if !p.is_empty() {
                return Self::load_from(Path::new(&p));
            }
        }
        if let Some(p) = user_path {
            if p.exists() {
                return Self::load_from(p);
            }
        }
        Self::embedded()
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read pricing toml at {}", path.display()))?;
        let mut t: Self = toml::from_str(&raw)
            .with_context(|| format!("parse pricing toml at {}", path.display()))?;
        t.sort_for_lookup();
        Ok(t)
    }

    pub fn embedded() -> Result<Self> {
        let mut t: Self =
            toml::from_str(EMBEDDED_PRICING_TOML).context("parse embedded pricing.toml")?;
        t.sort_for_lookup();
        Ok(t)
    }

    /// Sort by descending prefix length so `lookup` returns the most
    /// specific entry — `claude-opus-4-7` wins over `claude-opus-4`.
    fn sort_for_lookup(&mut self) {
        self.models
            .sort_by_key(|m| std::cmp::Reverse(m.prefix.len()));
    }

    pub fn lookup(&self, model: &str) -> Option<&ModelPricing> {
        self.models.iter().find(|m| model.starts_with(&m.prefix))
    }
}

/// Compute API-equivalent cost from token counts.
///
/// `service_tier="batch"` halves the result (Anthropic's batch API discount).
/// All other values pass through unchanged.
///
/// Returns `None` when the model isn't in the pricing table — caller should
/// log a warning and store NULL on the turns row; raw token counts are
/// always preserved regardless.
pub fn compute_cost_usd_api_equiv(
    model: &str,
    tokens: &TurnTokens,
    service_tier: Option<&str>,
    pricing: &PricingTable,
) -> Option<f64> {
    let p = pricing.lookup(model)?;
    let raw = tokens.input as f64 / 1e6 * p.input_per_mtok
        + tokens.output as f64 / 1e6 * p.output_per_mtok
        + tokens.cache_read as f64 / 1e6 * p.cache_read_per_mtok
        + tokens.cache_write_5m as f64 / 1e6 * p.cache_write_5m_per_mtok
        + tokens.cache_write_1h as f64 / 1e6 * p.cache_write_1h_per_mtok;
    Some(match service_tier {
        Some("batch") => raw * 0.5,
        _ => raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PricingTable {
        PricingTable::embedded().unwrap()
    }

    #[test]
    fn embedded_loads_with_models() {
        let t = p();
        assert!(!t.models.is_empty());
        assert_eq!(t.pricing_version, 1);
    }

    #[test]
    fn longest_prefix_wins_within_opus_family() {
        let t = p();
        let v = t.lookup("claude-opus-4-7-20260301").unwrap();
        assert_eq!(v.prefix, "claude-opus-4-7");
        assert_eq!(v.input_per_mtok, 5.0);

        let v = t.lookup("claude-opus-4-1-20250901").unwrap();
        assert_eq!(v.prefix, "claude-opus-4-1");
        assert_eq!(v.input_per_mtok, 15.0);

        let v = t.lookup("claude-opus-4-20240307").unwrap();
        assert_eq!(v.prefix, "claude-opus-4");
        assert_eq!(v.input_per_mtok, 15.0);
    }

    #[test]
    fn longest_prefix_wins_within_sonnet_family() {
        let t = p();
        let v = t.lookup("claude-sonnet-4-6-20260201").unwrap();
        assert_eq!(v.prefix, "claude-sonnet-4-6");
        assert_eq!(v.input_per_mtok, 3.0);
        assert_eq!(v.output_per_mtok, 15.0);
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(p().lookup("gpt-4-turbo").is_none());
        assert!(p().lookup("not-a-model").is_none());
    }

    #[test]
    fn zero_tokens_is_zero_cost() {
        let cost =
            compute_cost_usd_api_equiv("claude-opus-4-7", &TurnTokens::default(), None, &p());
        assert_eq!(cost, Some(0.0));
    }

    #[test]
    fn opus_4_7_million_in_million_out_is_30_dollars() {
        // 1M input @ $5 + 1M output @ $25 = $30
        let cost = compute_cost_usd_api_equiv(
            "claude-opus-4-7-20260301",
            &TurnTokens {
                input: 1_000_000,
                output: 1_000_000,
                ..TurnTokens::default()
            },
            None,
            &p(),
        )
        .unwrap();
        assert!((cost - 30.0).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn sonnet_4_6_with_caching_breakdown() {
        // Sonnet 4.6 rates:
        //   input          $3.00 / Mtok
        //   cache_read     $0.30 / Mtok
        //   cache_write_5m $3.75 / Mtok
        //   output         $15.00 / Mtok
        let cost = compute_cost_usd_api_equiv(
            "claude-sonnet-4-6",
            &TurnTokens {
                input: 100_000,
                output: 30_000,
                cache_read: 50_000,
                cache_write_5m: 200_000,
                cache_write_1h: 0,
            },
            None,
            &p(),
        )
        .unwrap();
        let expected = 0.1 * 3.0 + 0.05 * 0.30 + 0.2 * 3.75 + 0.03 * 15.0;
        assert!((cost - expected).abs() < 1e-9, "{cost} vs {expected}");
    }

    #[test]
    fn batch_tier_halves_cost() {
        let tokens = TurnTokens {
            input: 1_000_000,
            output: 1_000_000,
            ..TurnTokens::default()
        };
        let std_cost = compute_cost_usd_api_equiv("claude-haiku-4-5", &tokens, None, &p()).unwrap();
        let batch_cost =
            compute_cost_usd_api_equiv("claude-haiku-4-5", &tokens, Some("batch"), &p()).unwrap();
        assert!((batch_cost * 2.0 - std_cost).abs() < 1e-9);
        // Standard tier passes through unchanged
        let std_cost2 =
            compute_cost_usd_api_equiv("claude-haiku-4-5", &tokens, Some("standard"), &p())
                .unwrap();
        assert_eq!(std_cost, std_cost2);
    }

    #[test]
    fn load_from_user_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing.toml");
        std::fs::write(&path, EMBEDDED_PRICING_TOML).unwrap();
        let t = PricingTable::load_from(&path).unwrap();
        assert_eq!(t.pricing_version, 1);
        assert!(t.lookup("claude-opus-4-7").is_some());
    }

    #[test]
    fn unknown_model_in_compute_returns_none() {
        let cost = compute_cost_usd_api_equiv(
            "gpt-4",
            &TurnTokens {
                input: 1_000_000,
                ..TurnTokens::default()
            },
            None,
            &p(),
        );
        assert!(cost.is_none());
    }
}
