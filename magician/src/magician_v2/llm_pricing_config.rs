//! Deployment-level LLM pricing file (`llm_pricing.json`): resolve + install.
//!
//! Per-model prices live in the pricing file, not in code. The repo seed
//! (`magician_data_v3/llm_pricing.template.json`) is compiled into `magicllm`
//! as the base table over its code defaults (per-provider catch-alls, free
//! local engines, web-search fees). At startup this module layers the
//! deployment's `<MAGICIAN_ROOT_DIR>/llm_pricing.json` on top, falling back to
//! the seed on disk in a dev checkout (the standard
//! [`runtime_config_path`](crate::magician_v2::artifact_v2::workspace::runtime_config_path)
//! idiom). The format and parser are `magicllm::pricing_file`.
//!
//! This MUST run once at startup before anything can price a call:
//! `magicllm::active_table()` locks in the built-in table on first use,
//! silently, for the process lifetime. Any failure (missing file, bad JSON,
//! bad row) is fail-open: one loud warning naming the path and error, and the
//! built-in table (code defaults + compiled-in seed) stays active.

use tracing::{info, warn};

use magicllm::{LLMProviderKind, PricingFileRows};

/// File name resolved under the runtime root (`MAGICIAN_ROOT_DIR`).
pub const PRICING_FILE_NAME: &str = "llm_pricing.json";
/// Git-tracked seed template; also the dev-checkout fallback content.
pub const PRICING_SEED_TEMPLATE: &str = "magician_data_v3/llm_pricing.template.json";

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
            let (text, realtime, web_search) = (rows.text.len(), rows.realtime.len(), rows.web_search.len());
            magicllm::install_pricing_file(rows);
            info!(
                text,
                realtime,
                web_search,
                path = %path.display(),
                "llm_pricing: installed pricing file over built-in table"
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

/// Parse + validate a pricing file body (`magicllm::parse_pricing_file`).
/// Unknown provider strings are kept as [`LLMProviderKind::Custom`] with a
/// warning — they only match calls attributed to that exact provider string.
fn parse_pricing_rows(raw: &str) -> Result<PricingFileRows, String> {
    let rows = magicllm::parse_pricing_file(raw)?;
    for row in rows.text.iter().filter(|row| {
        matches!(&row.provider, LLMProviderKind::Custom(name) if !name.starts_with("decision:"))
    }) {
        warn!(
            provider = %row.provider.as_str(),
            model_prefix = %row.model_prefix,
            "llm_pricing: unknown provider kind kept as custom — this row only matches calls attributed to that exact provider string"
        );
    }
    Ok(rows)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magicllm::pricing_file::parse_effective_from_ms;
    use magicllm::LongContextPricing;

    /// The git-tracked seed template — the dev-checkout fallback content, so
    /// it must always parse with this loader.
    const SEED_TEMPLATE: &str = include_str!("../../../magician_data_v3/llm_pricing.template.json");

    fn one_rate_file(rate_body: &str) -> String {
        format!(r#"{{"schema_version": 1, "rates": [{rate_body}]}}"#)
    }

    #[test]
    fn seed_template_parses_with_effective_dated_gpt_5_6_revision() {
        let file = parse_pricing_rows(SEED_TEMPLATE).expect("seed template must parse");
        // Every per-model price lives in the file: the 1970-01-01 launch/base
        // rows plus the dated revisions, the realtime voice models and the
        // web-search fees.
        assert_eq!(file.text.len(), 148);
        assert_eq!(file.realtime.len(), 9);
        assert_eq!(file.web_search.len(), 5);
        assert_eq!(file.media.len(), 21);
        let rows = file.text;
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
        // Haiku 4.5 is $1 / $5; the seed once carried Haiku 3.5's $0.80 / $4,
        // and a file row outranks the corrected built-in base row.
        for (provider, model) in [
            (LLMProviderKind::Anthropic, "claude-haiku-4-5"),
            (LLMProviderKind::OpenRouter, "anthropic/claude-haiku-4-5"),
        ] {
            let row = rows
                .iter()
                .find(|row| row.provider == provider && row.model_prefix == model)
                .expect("Haiku 4.5 row");
            assert_eq!(row.pricing.input_per_m, 1.0);
            assert_eq!(row.pricing.output_per_m, 5.0);
            assert_eq!(row.pricing.cache_read_per_m, Some(0.1));
            assert_eq!(row.pricing.cache_write_per_m, Some(1.25));
        }
        let september_28_ms = parse_effective_from_ms("2026-09-28").expect("valid date");
        for (provider, model) in [
            (LLMProviderKind::Anthropic, "claude-sonnet-5-5"),
            (LLMProviderKind::OpenRouter, "anthropic/claude-sonnet-5-5"),
        ] {
            let row = rows
                .iter()
                .find(|row| {
                    row.effective_from_ms == september_28_ms
                        && row.provider == provider
                        && row.model_prefix == model
                })
                .expect("Sonnet 5.5 row");
            assert_eq!(row.pricing.input_per_m, 2.0);
            assert_eq!(row.pricing.output_per_m, 10.0);
            // 0.05x input, as published (the row once said 0.1x).
            assert_eq!(row.pricing.cache_read_per_m, Some(0.1));
            assert_eq!(row.pricing.cache_write_per_m, Some(2.5));
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
        let error = parse_pricing_rows(r#"{"schema_version": 3, "rates": []}"#)
            .expect_err("schema_version 3 must fail");
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
        let rows = parse_pricing_rows(&raw).expect("unknown provider is not an error").text;
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
        let rows = parse_pricing_rows(&raw).expect("parses").text;
        assert_eq!(rows[0].pricing.cache_read_per_m, None);
        assert_eq!(rows[0].pricing.cache_write_per_m, None);
    }

    #[test]
    fn long_context_rule_is_parsed() {
        let raw = one_rate_file(
            r#"{"provider": "openai", "model_prefix": "gpt-5.4", "effective_from": "2026-04-01", "input_per_m": 2.5, "output_per_m": 15.0, "cache_read_per_m": 0.25, "long_context": {"threshold_tokens": 272000, "input_multiplier": 2.0, "output_multiplier": 1.5}}"#,
        );
        let rows = parse_pricing_rows(&raw).expect("long-context rule parses").text;
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
