//! Deployment-level LLM pricing file (`llm_pricing.json`) — load + install.
//!
//! Resolution follows the standard runtime-config idiom
//! ([`runtime_config_path`](crate::magician_v2::artifact_v2::workspace::runtime_config_path)):
//! prefer `<MAGICIAN_ROOT_DIR>/llm_pricing.json` when it exists, otherwise
//! fall back to the git-tracked seed template
//! `magician_data_v3/llm_pricing.template.json`. The template is an
//! effective-dated deployment overlay on the built-in launch/base rows and is
//! the default for dev checkouts (same shape as `magician_config_path`'s
//! seed-template fallback).
//!
//! Parsed rows are layered over the built-in base table via
//! [`magicllm::install_pricing_table`]. This MUST run once at startup before
//! anything can price a call: `magicllm::active_table()` locks in the
//! builtin-only table on first use, silently, for the process lifetime.
//! Any failure (missing file, bad JSON, bad row) is fail-open — one loud
//! warning naming the path + error, and the built-in table stays active.

use serde::Deserialize;
use tracing::{info, warn};

use magicllm::{LLMProviderKind, LongContextPricing, PricingRow, ProviderPricing};

/// File name resolved under the runtime root (`MAGICIAN_ROOT_DIR`).
pub const PRICING_FILE_NAME: &str = "llm_pricing.json";
/// Git-tracked seed template; also the dev-checkout fallback content.
pub const PRICING_SEED_TEMPLATE: &str = "magician_data_v3/llm_pricing.template.json";

const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// `{ schema_version, rates: [...] }`. Unknown fields (the `_readme` header
/// the template carries for humans — JSON has no comments) are ignored.
#[derive(Debug, Deserialize)]
struct PricingFile {
    schema_version: u32,
    rates: Vec<PricingFileRate>,
}

/// One effective-dated rate row. All rates are USD per million tokens.
#[derive(Debug, Deserialize)]
struct PricingFileRate {
    provider: String,
    /// Longest-prefix model match; empty = per-provider catch-all.
    #[serde(default)]
    model_prefix: String,
    /// `YYYY-MM-DD`, interpreted as UTC midnight.
    effective_from: String,
    input_per_m: f64,
    output_per_m: f64,
    /// Absent = provider has no cache-read bucket (those tokens bill at
    /// `input_per_m`), matching `ProviderPricing`'s `Option` semantics.
    #[serde(default)]
    cache_read_per_m: Option<f64>,
    /// Absent = provider has no cache-write bucket (those tokens bill at
    /// `input_per_m`).
    #[serde(default)]
    cache_write_per_m: Option<f64>,
    /// Optional request-wide rate multipliers when the complete prompt token
    /// count is strictly greater than `threshold_tokens`.
    #[serde(default)]
    long_context: Option<PricingFileLongContext>,
}

#[derive(Debug, Deserialize)]
struct PricingFileLongContext {
    threshold_tokens: u32,
    input_multiplier: f64,
    output_multiplier: f64,
}

/// Resolve, parse, validate, and install `llm_pricing.json` over the built-in
/// pricing table. Call once at startup, immediately after main config load —
/// before any service construction or CLI command can compute a cost.
///
/// Fail-open: on any failure the built-in table stays active and a warning
/// names the resolved path and the error.
pub fn load_and_install_llm_pricing() {
    let path = crate::magician_v2::artifact_v2::workspace::runtime_config_path(
        PRICING_FILE_NAME,
        PRICING_SEED_TEMPLATE,
    );
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => {
            warn!(
                path = %path.display(),
                %error,
                "llm_pricing: pricing file unreadable; keeping built-in pricing table"
            );
            return;
        },
    };
    match parse_pricing_rows(&raw) {
        Ok(rows) => {
            let rate_count = rows.len();
            magicllm::install_pricing_table(rows);
            info!(
                rate_count,
                path = %path.display(),
                "llm_pricing: installed effective-dated pricing table over built-in base"
            );
        },
        Err(error) => {
            warn!(
                path = %path.display(),
                error = %error,
                "llm_pricing: invalid pricing file; keeping built-in pricing table"
            );
        },
    }
}

/// Parse + validate the pricing file body into installable rows.
///
/// Rejects (whole file — install nothing): unsupported `schema_version`,
/// negative or non-finite rates, undated/unparseable `effective_from`.
/// Unknown provider strings are kept as [`LLMProviderKind::Custom`] with a
/// warning — they only match calls attributed to that exact provider string.
fn parse_pricing_rows(raw: &str) -> Result<Vec<PricingRow>, String> {
    let file: PricingFile =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON: {error}"))?;
    if file.schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(format!(
            "unsupported schema_version {} (expected {})",
            file.schema_version, SUPPORTED_SCHEMA_VERSION
        ));
    }
    let mut rows = Vec::with_capacity(file.rates.len());
    for (index, rate) in file.rates.into_iter().enumerate() {
        let context = format!(
            "rates[{index}] (provider `{}`, model_prefix `{}`)",
            rate.provider, rate.model_prefix
        );
        for (field, value) in [
            ("input_per_m", Some(rate.input_per_m)),
            ("output_per_m", Some(rate.output_per_m)),
            ("cache_read_per_m", rate.cache_read_per_m),
            ("cache_write_per_m", rate.cache_write_per_m),
        ] {
            if let Some(value) = value {
                if !value.is_finite() || value < 0.0 {
                    return Err(format!(
                        "{context}: {field} must be a non-negative finite USD-per-million rate, got {value}"
                    ));
                }
            }
        }
        let effective_from_ms = parse_effective_from_ms(&rate.effective_from).ok_or_else(|| {
            format!(
                "{context}: effective_from `{}` is not a YYYY-MM-DD date",
                rate.effective_from
            )
        })?;
        if let Some(long_context) = &rate.long_context {
            if long_context.threshold_tokens == 0 {
                return Err(format!(
                    "{context}: long_context.threshold_tokens must be greater than zero"
                ));
            }
            for (field, value) in [
                ("input_multiplier", long_context.input_multiplier),
                ("output_multiplier", long_context.output_multiplier),
            ] {
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!(
                        "{context}: long_context.{field} must be a positive finite multiplier, got {value}"
                    ));
                }
            }
        }
        let provider = LLMProviderKind::from_str(&rate.provider);
        if matches!(provider, LLMProviderKind::Custom(_)) && !rate.provider.starts_with("decision:")
        {
            warn!(
                provider = %rate.provider,
                model_prefix = %rate.model_prefix,
                "llm_pricing: unknown provider kind kept as custom — this row only matches calls attributed to that exact provider string"
            );
        }
        let row = PricingRow::new(
            provider,
            rate.model_prefix,
            effective_from_ms,
            ProviderPricing {
                input_per_m: rate.input_per_m,
                output_per_m: rate.output_per_m,
                cache_read_per_m: rate.cache_read_per_m,
                cache_write_per_m: rate.cache_write_per_m,
            },
        );
        rows.push(match rate.long_context {
            Some(rule) => row.with_long_context(LongContextPricing::new(
                rule.threshold_tokens,
                rule.input_multiplier,
                rule.output_multiplier,
            )),
            None => row,
        });
    }
    Ok(rows)
}

/// `YYYY-MM-DD` → epoch millis at UTC midnight.
fn parse_effective_from_ms(date: &str) -> Option<i64> {
    let date = chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// The git-tracked seed template — the dev-checkout fallback content, so
    /// it must always parse with this loader.
    const SEED_TEMPLATE: &str = include_str!("../../../magician_data_v3/llm_pricing.template.json");

    fn one_rate_file(rate_body: &str) -> String {
        format!(r#"{{"schema_version": 1, "rates": [{rate_body}]}}"#)
    }

    #[test]
    fn seed_template_parses_with_effective_dated_gpt_5_6_revision() {
        let rows = parse_pricing_rows(SEED_TEMPLATE).expect("seed template must parse");
        // Inventory includes both effective-dated Gemini 3.8 prices, the six
        // September DeepSeek revisions and the 2026-09-22 Opus 5.5 / GPT-6
        // additions and September 29 GPT-6.1 Sol; historical rates remain present.
        assert_eq!(rows.len(), 71);
        let september_29_ms = parse_effective_from_ms("2026-09-29").expect("valid date");
        let sol = rows
            .iter()
            .filter(|row| row.effective_from_ms == september_29_ms)
            .collect::<Vec<_>>();
        assert_eq!(sol.len(), 2);
        for (provider, model) in [
            (LLMProviderKind::OpenAI, "gpt-6.1-sol"),
            (LLMProviderKind::OpenRouter, "openai/gpt-6.1-sol"),
        ] {
            let row = sol
                .iter()
                .find(|row| row.provider == provider && row.model_prefix == model)
                .expect("Sol 6.1 row");
            assert_eq!(row.pricing.input_per_m, 2.0);
            assert_eq!(row.pricing.output_per_m, 10.0);
            assert_eq!(row.pricing.cache_read_per_m, Some(0.10));
            assert_eq!(row.pricing.cache_write_per_m, Some(2.50));
            assert_eq!(
                row.long_context,
                Some(LongContextPricing::new(272_000, 2.0, 1.5))
            );
        }
        // The 2026-09-22 batch added eight rows — four direct, four through
        // OpenRouter. Counting them alone would let a row be swapped for
        // another without the total moving, so name them.
        let september_22_ms = parse_effective_from_ms("2026-09-22").expect("valid date");
        // `LLMProviderKind` is Eq + Hash but not Ord, so this is a HashSet;
        // `model_prefix` is a Cow, so both sides normalise to String.
        let added = rows
            .iter()
            .filter(|row| row.effective_from_ms == september_22_ms)
            .map(|row| (row.provider.clone(), row.model_prefix.to_string()))
            .collect::<std::collections::HashSet<_>>();
        let expected = [
            (LLMProviderKind::Anthropic, "claude-opus-5-5"),
            (LLMProviderKind::OpenAI, "gpt-5.6-sol"),
            (LLMProviderKind::OpenAI, "gpt-6-sol"),
            (LLMProviderKind::OpenAI, "gpt-6-luna"),
            (LLMProviderKind::OpenRouter, "anthropic/claude-opus-5-5"),
            (LLMProviderKind::OpenRouter, "openai/gpt-5.6-sol"),
            (LLMProviderKind::OpenRouter, "openai/gpt-6-sol"),
            (LLMProviderKind::OpenRouter, "openai/gpt-6-luna"),
        ]
        .into_iter()
        .map(|(provider, model)| (provider, model.to_string()))
        .collect::<std::collections::HashSet<_>>();
        assert_eq!(added, expected, "the 2026-09-22 model-catalog rows");
        assert!(rows
            .iter()
            .any(|row| { row.provider == LLMProviderKind::Yutori && row.model_prefix.is_empty() }));
        let july_30_ms = parse_effective_from_ms("2026-07-30").expect("valid date");
        let revised = rows
            .iter()
            .filter(|row| row.effective_from_ms == july_30_ms)
            .collect::<Vec<_>>();
        assert_eq!(revised.len(), 6, "OpenAI + OpenRouter GPT-5.6 rows");
        assert!(revised.iter().all(|row| {
            row.model_prefix.contains("gpt-5.6")
                && row.pricing.cache_write_per_m.is_some()
                && row.long_context == Some(LongContextPricing::new(272_000, 2.0, 1.5))
        }));

        let terra = revised
            .iter()
            .find(|row| {
                row.provider == LLMProviderKind::OpenAI && row.model_prefix == "gpt-5.6-terra"
            })
            .expect("direct OpenAI Terra revision");
        assert_eq!(terra.pricing.input_per_m, 2.0);
        assert_eq!(terra.pricing.cache_read_per_m, Some(0.2));
        assert_eq!(terra.pricing.cache_write_per_m, Some(2.5));
        assert_eq!(terra.pricing.output_per_m, 12.0);
    }

    #[test]
    fn effective_from_is_utc_midnight_epoch_ms() {
        // 1970-01-02 UTC midnight = exactly one day of millis.
        assert_eq!(parse_effective_from_ms("1970-01-02"), Some(86_400_000));
        assert_eq!(parse_effective_from_ms("not-a-date"), None);
        assert_eq!(parse_effective_from_ms("2026-13-01"), None);
    }

    #[test]
    fn negative_rate_is_rejected() {
        let raw = one_rate_file(
            r#"{"provider": "openai", "model_prefix": "gpt-5", "effective_from": "2026-04-01", "input_per_m": -1.0, "output_per_m": 10.0}"#,
        );
        let error = parse_pricing_rows(&raw).expect_err("negative rate must fail");
        assert!(error.contains("input_per_m"), "unexpected error: {error}");
    }

    #[test]
    fn bad_date_is_rejected() {
        let raw = one_rate_file(
            r#"{"provider": "openai", "model_prefix": "gpt-5", "effective_from": "April 2026", "input_per_m": 1.25, "output_per_m": 10.0}"#,
        );
        let error = parse_pricing_rows(&raw).expect_err("bad date must fail");
        assert!(
            error.contains("effective_from"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let error = parse_pricing_rows(r#"{"schema_version": 2, "rates": []}"#)
            .expect_err("schema_version 2 must fail");
        assert!(
            error.contains("schema_version"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_provider_is_kept_as_custom() {
        let raw = one_rate_file(
            r#"{"provider": "somefuturelab", "model_prefix": "sf-1", "effective_from": "2026-04-01", "input_per_m": 1.0, "output_per_m": 2.0}"#,
        );
        let rows = parse_pricing_rows(&raw).expect("unknown provider is not an error");
        assert_eq!(
            rows[0].provider,
            LLMProviderKind::Custom("somefuturelab".to_string())
        );
    }

    #[test]
    fn absent_cache_fields_stay_unpriced_options() {
        let raw = one_rate_file(
            r#"{"provider": "openai", "model_prefix": "gpt-5", "effective_from": "2026-04-01", "input_per_m": 1.25, "output_per_m": 10.0}"#,
        );
        let rows = parse_pricing_rows(&raw).expect("parses");
        assert_eq!(rows[0].pricing.cache_read_per_m, None);
        assert_eq!(rows[0].pricing.cache_write_per_m, None);
    }

    #[test]
    fn long_context_rule_is_parsed() {
        let raw = one_rate_file(
            r#"{"provider": "openai", "model_prefix": "gpt-5.4", "effective_from": "2026-04-01", "input_per_m": 2.5, "output_per_m": 15.0, "cache_read_per_m": 0.25, "long_context": {"threshold_tokens": 272000, "input_multiplier": 2.0, "output_multiplier": 1.5}}"#,
        );
        let rows = parse_pricing_rows(&raw).expect("long-context rule parses");
        assert_eq!(
            rows[0].long_context,
            Some(LongContextPricing::new(272_000, 2.0, 1.5))
        );
    }

    #[test]
    fn invalid_long_context_rule_is_rejected() {
        let raw = one_rate_file(
            r#"{"provider": "openai", "model_prefix": "gpt-5.4", "effective_from": "2026-04-01", "input_per_m": 2.5, "output_per_m": 15.0, "long_context": {"threshold_tokens": 0, "input_multiplier": 2.0, "output_multiplier": 1.5}}"#,
        );
        let error = parse_pricing_rows(&raw).expect_err("zero threshold must fail");
        assert!(
            error.contains("threshold_tokens"),
            "unexpected error: {error}"
        );
    }
}
