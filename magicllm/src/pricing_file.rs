//! The LLM pricing file (`llm_pricing.json`): its format, parser and the seed
//! copy compiled into the binary.
//!
//! Per-model prices live in this file, not in code. The repo seed
//! (`magician_data_v3/llm_pricing.template.json`) is compiled in as
//! [`SEED_PRICING_JSON`] and sits under any deployment file, so a missing or
//! broken runtime file still prices every model the seed knows. Code keeps only
//! the per-provider catch-alls and free local engines
//! ([`PricingTable::defaults`](crate::pricing::PricingTable::defaults)).
//!
//! Three row kinds, each effective-dated (`YYYY-MM-DD`, UTC midnight):
//! - `rates`: text models, USD per million tokens;
//! - `realtime_rates`: realtime voice models, per-million text and audio
//!   token rates plus `per_second` for clock-billed models;
//! - `web_search_rates`: provider-executed web search, USD per call;
//! - `media_rates`: text-to-speech and speech-to-text, by provider, model
//!   prefix and kind, per million characters, per audio minute and/or per
//!   request.

use std::borrow::Cow;

use serde::Deserialize;

use crate::capability::LLMProviderKind;
use crate::pricing::{
    LongContextPricing, MediaKind, MediaPricing, MediaPricingRow, PricingRow, ProviderPricing,
    RealtimePricing, RealtimePricingRow, WebSearchPricingRow,
};

/// The repo seed, compiled in: the base layer of every pricing table.
pub const SEED_PRICING_JSON: &str =
    include_str!("../../magician_data_v3/llm_pricing.template.json");

/// Schema 1 has `rates` only; schema 2 adds `realtime_rates` and
/// `web_search_rates`.
const SUPPORTED_SCHEMA_VERSIONS: [u32; 2] = [1, 2];

/// Rows parsed from one pricing file.
#[derive(Debug, Clone, Default)]
pub struct PricingFileRows {
    pub text: Vec<PricingRow>,
    pub realtime: Vec<RealtimePricingRow>,
    pub web_search: Vec<WebSearchPricingRow>,
    pub media: Vec<MediaPricingRow>,
}

impl PricingFileRows {
    pub fn len(&self) -> usize {
        self.text.len() + self.realtime.len() + self.web_search.len() + self.media.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `{ schema_version, rates, realtime_rates?, web_search_rates? }`. Unknown
/// fields (the `_readme` header; JSON has no comments) are ignored.
#[derive(Debug, Deserialize)]
struct PricingFile {
    schema_version: u32,
    rates: Vec<FileRate>,
    #[serde(default)]
    realtime_rates: Vec<FileRealtimeRate>,
    #[serde(default)]
    web_search_rates: Vec<FileWebSearchRate>,
    #[serde(default)]
    media_rates: Vec<FileMediaRate>,
}

/// One text-model row. All rates are USD per million tokens.
#[derive(Debug, Deserialize)]
struct FileRate {
    provider: String,
    /// Longest-prefix model match; empty = per-provider catch-all.
    #[serde(default)]
    model_prefix: String,
    effective_from: String,
    input_per_m: f64,
    output_per_m: f64,
    /// Absent = no cache-read bucket (those tokens bill at `input_per_m`).
    #[serde(default)]
    cache_read_per_m: Option<f64>,
    /// Absent = no cache-write bucket (those tokens bill at `input_per_m`).
    #[serde(default)]
    cache_write_per_m: Option<f64>,
    /// Request-wide multipliers when the prompt is strictly longer than
    /// `threshold_tokens`.
    #[serde(default)]
    long_context: Option<FileLongContext>,
}

#[derive(Debug, Deserialize)]
struct FileLongContext {
    threshold_tokens: u32,
    input_multiplier: f64,
    output_multiplier: f64,
}

/// One realtime-model row, keyed by model prefix alone (realtime model names
/// are unambiguous across providers).
#[derive(Debug, Deserialize)]
struct FileRealtimeRate {
    model_prefix: String,
    effective_from: String,
    text_input_per_m: f64,
    text_cached_input_per_m: f64,
    text_output_per_m: f64,
    audio_input_per_m: f64,
    audio_cached_input_per_m: f64,
    audio_output_per_m: f64,
    /// USD per second, for a model billed by the clock. Zero otherwise.
    #[serde(default)]
    per_second: f64,
}

/// Provider-executed web search, USD per call.
#[derive(Debug, Deserialize)]
struct FileWebSearchRate {
    provider: String,
    effective_from: String,
    usd_per_call: f64,
}

/// One text-to-speech or speech-to-text row. Absent rates are units the
/// vendor does not bill.
#[derive(Debug, Deserialize)]
struct FileMediaRate {
    provider: String,
    #[serde(default)]
    model_prefix: String,
    /// `tts`, `stt` or `stt_streaming`.
    kind: String,
    effective_from: String,
    #[serde(default)]
    usd_per_million_chars: f64,
    #[serde(default)]
    usd_per_audio_minute: f64,
    #[serde(default)]
    usd_per_request: f64,
}

/// Parse and validate a pricing file. Any invalid row rejects the whole file,
/// so a typo never installs half a table.
pub fn parse_pricing_file(raw: &str) -> Result<PricingFileRows, String> {
    let file: PricingFile =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON: {error}"))?;
    if !SUPPORTED_SCHEMA_VERSIONS.contains(&file.schema_version) {
        return Err(format!(
            "unsupported schema_version {} (expected one of {:?})",
            file.schema_version, SUPPORTED_SCHEMA_VERSIONS
        ));
    }
    if file.schema_version < 2
        && (!file.realtime_rates.is_empty()
            || !file.web_search_rates.is_empty()
            || !file.media_rates.is_empty())
    {
        return Err("realtime_rates, web_search_rates and media_rates need schema_version 2".to_string());
    }
    let mut rows = PricingFileRows::default();
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
                check_rate(&context, field, value)?;
            }
        }
        let effective_from_ms = effective_from(&context, &rate.effective_from)?;
        let long_context = match rate.long_context {
            Some(rule) => {
                if rule.threshold_tokens == 0 {
                    return Err(format!(
                        "{context}: long_context.threshold_tokens must be greater than zero"
                    ));
                }
                for (field, value) in [
                    ("input_multiplier", rule.input_multiplier),
                    ("output_multiplier", rule.output_multiplier),
                ] {
                    if !value.is_finite() || value <= 0.0 {
                        return Err(format!(
                            "{context}: long_context.{field} must be a positive finite multiplier, got {value}"
                        ));
                    }
                }
                Some(LongContextPricing::new(
                    rule.threshold_tokens,
                    rule.input_multiplier,
                    rule.output_multiplier,
                ))
            },
            None => None,
        };
        let row = PricingRow::new(
            LLMProviderKind::from_str(&rate.provider),
            rate.model_prefix,
            effective_from_ms,
            ProviderPricing {
                input_per_m: rate.input_per_m,
                output_per_m: rate.output_per_m,
                cache_read_per_m: rate.cache_read_per_m,
                cache_write_per_m: rate.cache_write_per_m,
            },
        );
        rows.text.push(match long_context {
            Some(rule) => row.with_long_context(rule),
            None => row,
        });
    }
    for (index, rate) in file.realtime_rates.into_iter().enumerate() {
        let context = format!("realtime_rates[{index}] (model_prefix `{}`)", rate.model_prefix);
        if rate.model_prefix.trim().is_empty() {
            return Err(format!("{context}: model_prefix must not be empty"));
        }
        for (field, value) in [
            ("text_input_per_m", rate.text_input_per_m),
            ("text_cached_input_per_m", rate.text_cached_input_per_m),
            ("text_output_per_m", rate.text_output_per_m),
            ("audio_input_per_m", rate.audio_input_per_m),
            ("audio_cached_input_per_m", rate.audio_cached_input_per_m),
            ("audio_output_per_m", rate.audio_output_per_m),
            ("per_second", rate.per_second),
        ] {
            check_rate(&context, field, value)?;
        }
        let effective_from_ms = effective_from(&context, &rate.effective_from)?;
        rows.realtime.push(RealtimePricingRow::new(
            Cow::Owned(rate.model_prefix),
            effective_from_ms,
            RealtimePricing {
                text_input_per_m: rate.text_input_per_m,
                text_cached_input_per_m: rate.text_cached_input_per_m,
                text_output_per_m: rate.text_output_per_m,
                audio_input_per_m: rate.audio_input_per_m,
                audio_cached_input_per_m: rate.audio_cached_input_per_m,
                audio_output_per_m: rate.audio_output_per_m,
                per_second: rate.per_second,
            },
        ));
    }
    for (index, rate) in file.web_search_rates.into_iter().enumerate() {
        let context = format!("web_search_rates[{index}] (provider `{}`)", rate.provider);
        check_rate(&context, "usd_per_call", rate.usd_per_call)?;
        let effective_from_ms = effective_from(&context, &rate.effective_from)?;
        rows.web_search.push(WebSearchPricingRow {
            provider: LLMProviderKind::from_str(&rate.provider),
            effective_from_ms,
            usd_per_call: rate.usd_per_call,
        });
    }
    for (index, rate) in file.media_rates.into_iter().enumerate() {
        let context = format!(
            "media_rates[{index}] (provider `{}`, model_prefix `{}`, kind `{}`)",
            rate.provider, rate.model_prefix, rate.kind
        );
        let kind = MediaKind::parse(&rate.kind)
            .ok_or_else(|| format!("{context}: kind must be tts, stt or stt_streaming"))?;
        for (field, value) in [
            ("usd_per_million_chars", rate.usd_per_million_chars),
            ("usd_per_audio_minute", rate.usd_per_audio_minute),
            ("usd_per_request", rate.usd_per_request),
        ] {
            check_rate(&context, field, value)?;
        }
        if rate.usd_per_million_chars == 0.0 && rate.usd_per_audio_minute == 0.0 && rate.usd_per_request == 0.0 {
            return Err(format!("{context}: needs at least one non-zero rate (a free engine needs no row)"));
        }
        let effective_from_ms = effective_from(&context, &rate.effective_from)?;
        rows.media.push(MediaPricingRow {
            provider: LLMProviderKind::from_str(&rate.provider),
            model_prefix: Cow::Owned(rate.model_prefix),
            kind,
            effective_from_ms,
            pricing: MediaPricing {
                usd_per_million_chars: rate.usd_per_million_chars,
                usd_per_audio_minute: rate.usd_per_audio_minute,
                usd_per_request: rate.usd_per_request,
            },
        });
    }
    Ok(rows)
}

fn check_rate(context: &str, field: &str, value: f64) -> Result<(), String> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(format!("{context}: {field} must be a non-negative finite USD rate, got {value}"))
    }
}

fn effective_from(context: &str, date: &str) -> Result<i64, String> {
    parse_effective_from_ms(date)
        .ok_or_else(|| format!("{context}: effective_from `{date}` is not a YYYY-MM-DD date"))
}

/// `YYYY-MM-DD` as UTC midnight, in epoch millis.
pub fn parse_effective_from_ms(date: &str) -> Option<i64> {
    let date = chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}
