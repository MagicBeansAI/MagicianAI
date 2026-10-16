//! USD cost computation for LLM usage, keyed by `(provider, model)`.
//!
//! All prices are per-million tokens. Where a model/provider doesn't publish a
//! bucket, its per-token rate is `None` and that usage falls back to the normal
//! input rate. Newer GPT-5.6/GPT-6 and Claude Opus 5.5 rows publish an explicit
//! cache-write bucket.
//!
//! Rows are effective-dated: each carries an `effective_from_ms` epoch-millis
//! stamp, and resolution for `(provider, model, at_ms)` only considers rows
//! already effective at `at_ms` (longest prefix wins, then latest effective
//! date). Per-model rates live in the pricing file ([`crate::pricing_file`]):
//! the repo seed is compiled in over the code defaults
//! ([`PricingTable::defaults`]: per-provider catch-alls, free local engines,
//! web-search fees), and a deployment's `llm_pricing.json` layers on top via
//! [`install_pricing_file`] at startup. Launch/base rows use
//! `effective_from_ms = 0` (`1970-01-01` in the file); provider revisions keep
//! their real boundary.

use std::borrow::Cow;
use std::sync::OnceLock;

use tracing::warn;

use crate::capability::LLMProviderKind;
use crate::pricing_file::{parse_pricing_file, PricingFileRows, SEED_PRICING_JSON};
use crate::types::{RealtimeUsage, TokenUsage};

/// 2026-07-30T00:00:00Z — OpenAI's effective boundary for the revised GPT-5.6
/// standard rates.
#[cfg(test)]
const GPT_5_6_JULY_30_2026_MS: i64 = 1_785_369_600_000;

/// 2026-09-22T00:00:00Z — launch boundary for Claude Opus 5.5 and GPT-6
/// Sol/Luna.
#[cfg(test)]
const SEPTEMBER_22_2026_MS: i64 = 1_790_035_200_000;
/// 2026-09-28T00:00:00Z — launch boundary for Claude Sonnet 5.5.
#[cfg(test)]
const SEPTEMBER_28_2026_MS: i64 = 1_790_553_600_000;
#[cfg(test)]
const OCTOBER_13_2026_MS: i64 = 1_791_849_600_000;
#[cfg(test)]
const DECEMBER_1_2026_MS: i64 = 1_796_083_200_000;

fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Per-million-token USD rates for one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderPricing {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_read_per_m: Option<f64>,
    pub cache_write_per_m: Option<f64>,
}

impl ProviderPricing {
    /// Builds an Anthropic-style pricing row where cache_read = 10% of input
    /// and cache_write = 125% of input (the standard Anthropic multipliers).
    pub const fn anthropic(input: f64, output: f64) -> Self {
        Self {
            input_per_m: input,
            output_per_m: output,
            cache_read_per_m: Some(input * 0.1),
            cache_write_per_m: Some(input * 1.25),
        }
    }

    /// Builds an OpenAI-style pricing row where cache_read = 50% of input
    /// and there is no cache-write bucket.
    pub const fn openai(input: f64, output: f64) -> Self {
        Self {
            input_per_m: input,
            output_per_m: output,
            cache_read_per_m: Some(input * 0.5),
            cache_write_per_m: None,
        }
    }

    /// Builds an OpenAI-style pricing row with an explicit cached-input rate.
    /// Newer GPT-5-family models use a 90% cached-input discount, while older
    /// OpenAI-compatible models may retain the historical 50% rate.
    pub const fn openai_with_cache(input: f64, cached_input: f64, output: f64) -> Self {
        Self {
            input_per_m: input,
            output_per_m: output,
            cache_read_per_m: Some(cached_input),
            cache_write_per_m: None,
        }
    }

    /// Free-tier row (Ollama, Yutori) — always returns 0.0.
    pub const fn free() -> Self {
        Self {
            input_per_m: 0.0,
            output_per_m: 0.0,
            cache_read_per_m: Some(0.0),
            cache_write_per_m: Some(0.0),
        }
    }
}

/// Optional token-count threshold that changes the effective per-token rates
/// for a complete request. OpenAI GPT-5.4/5.5 apply this to prompts above
/// 272K tokens: input (including cached input) is 2x and output is 1.5x.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongContextPricing {
    pub threshold_tokens: u32,
    pub input_multiplier: f64,
    pub output_multiplier: f64,
}

impl LongContextPricing {
    pub const fn new(threshold_tokens: u32, input_multiplier: f64, output_multiplier: f64) -> Self {
        Self {
            threshold_tokens,
            input_multiplier,
            output_multiplier,
        }
    }
}

/// Per-million-token USD rates for a Realtime (voice) model. Realtime billing
/// splits into TEXT and AUDIO token streams, each with its own input / cached-
/// input / output rate (audio dominates cost). This is a second rate *shape*
/// that lives in the same [`PricingTable`] as [`ProviderPricing`] (which has a
/// single input/output pair) — realtime models are first-class members of the
/// pricing table, resolved via [`realtime_pricing`] and applied via
/// [`compute_realtime_cost`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RealtimePricing {
    pub text_input_per_m: f64,
    pub text_cached_input_per_m: f64,
    pub text_output_per_m: f64,
    pub audio_input_per_m: f64,
    pub audio_cached_input_per_m: f64,
    pub audio_output_per_m: f64,
    /// USD per second of voice session, for a model billed by the clock.
    /// Zero for the token-priced models, which report no seconds.
    pub per_second: f64,
}

/// One effective-dated realtime pricing row — the realtime counterpart of
/// [`PricingRow`], keyed by model prefix (provider-agnostic; the realtime model
/// name is unambiguous). Longest prefix wins, then latest effective date.
#[derive(Debug, Clone)]
pub struct RealtimePricingRow {
    pub model_prefix: Cow<'static, str>,
    pub effective_from_ms: i64,
    pub pricing: RealtimePricing,
}

impl RealtimePricingRow {
    pub fn new(
        model_prefix: impl Into<Cow<'static, str>>,
        effective_from_ms: i64,
        pricing: RealtimePricing,
    ) -> Self {
        Self {
            model_prefix: model_prefix.into(),
            effective_from_ms,
            pricing,
        }
    }
}

/// Resolve realtime pricing for `model` from the active pricing table at the
/// current wall time, else `None`. Realtime models live in the same table as
/// text models — see [`PricingTable::realtime_pricing_at`].
pub fn realtime_pricing(model: &str) -> Option<RealtimePricing> {
    active_table().realtime_pricing_at(model, now_unix_ms())
}

/// Whether a realtime model is billed by the clock rather than by tokens. Its
/// token buckets carry no price at all, so a cost recomputed from them alone
/// would be an authoritative zero for a session that genuinely cost money.
pub fn realtime_is_duration_billed_at(model: &str, at_ms: i64) -> bool {
    active_table()
        .realtime_pricing_at(model, at_ms)
        .is_some_and(|pricing| pricing.per_second > 0.0)
}

/// USD cost for one realtime response/session's [`RealtimeUsage`] at the current
/// wall time. Audio and text buckets bill independently; cached input is charged
/// at its own discounted rate. Returns 0.0 for an unknown model.
pub fn compute_realtime_cost(model: &str, usage: &RealtimeUsage) -> f64 {
    compute_realtime_cost_at(model, usage, now_unix_ms())
}

/// USD cost for realtime usage as of `at_ms` (epoch millis) against the active
/// table — the realtime counterpart of [`compute_cost_at`], used for repricing
/// historical rows at the original call timestamp.
pub fn compute_realtime_cost_at(model: &str, usage: &RealtimeUsage, at_ms: i64) -> f64 {
    active_table().compute_realtime_cost_at(model, usage, at_ms)
}

/// One effective-dated pricing table row.
///
/// `model_prefix` is a `Cow` so the built-in table stays allocation-free
/// (`&'static str`) while runtime-loaded rows carry owned `String`s.
#[derive(Debug, Clone)]
pub struct PricingRow {
    pub provider: LLMProviderKind,
    pub model_prefix: Cow<'static, str>,
    /// Epoch millis from which this row applies (UTC). Built-in launch/base
    /// rows use 0; built-in provider revisions retain their real effective
    /// date so builtin-only deployments still price historical calls safely.
    pub effective_from_ms: i64,
    pub pricing: ProviderPricing,
    pub long_context: Option<LongContextPricing>,
}

impl PricingRow {
    pub fn new(
        provider: LLMProviderKind,
        model_prefix: impl Into<Cow<'static, str>>,
        effective_from_ms: i64,
        pricing: ProviderPricing,
    ) -> Self {
        Self {
            provider,
            model_prefix: model_prefix.into(),
            effective_from_ms,
            pricing,
            long_context: None,
        }
    }

    pub const fn with_long_context(mut self, long_context: LongContextPricing) -> Self {
        self.long_context = Some(long_context);
        self
    }
}

/// The pricing table. Model lookups use longest-prefix match so that
/// `claude-sonnet-4-6-20260101` resolves to the `claude-sonnet-4-6` row,
/// then latest `effective_from_ms` among equal-length prefixes.
pub struct PricingTable {
    rows: Vec<PricingRow>,
    /// Realtime (voice) rows — a second rate shape in the same table. Resolved
    /// by [`PricingTable::realtime_pricing_at`] with the same longest-prefix +
    /// latest-effective-date rule as the text rows.
    realtime_rows: Vec<RealtimePricingRow>,
    /// Provider-executed web search, USD per call, effective-dated per
    /// provider (latest effective row wins).
    web_search_rows: Vec<WebSearchPricingRow>,
    /// Text-to-speech and speech-to-text rows, from the pricing file only.
    media_rows: Vec<MediaPricingRow>,
}

/// One effective-dated web-search rate: USD per provider-executed search call.
#[derive(Debug, Clone, PartialEq)]
pub struct WebSearchPricingRow {
    pub provider: LLMProviderKind,
    pub effective_from_ms: i64,
    pub usd_per_call: f64,
}

/// What a media (voice) row prices: text-to-speech, a speech-to-text upload,
/// or streaming speech-to-text (some vendors bill streaming at its own rate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Tts,
    Stt,
    SttStreaming,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tts => "tts",
            Self::Stt => "stt",
            Self::SttStreaming => "stt_streaming",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tts" => Some(Self::Tts),
            "stt" => Some(Self::Stt),
            "stt_streaming" => Some(Self::SttStreaming),
            _ => None,
        }
    }
}

/// USD rates for one media model. A vendor bills one or more of these units;
/// a zero rate is a unit it does not bill.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MediaPricing {
    pub usd_per_million_chars: f64,
    pub usd_per_audio_minute: f64,
    pub usd_per_request: f64,
}

/// One effective-dated media row: provider, model prefix (longest wins), kind.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaPricingRow {
    pub provider: LLMProviderKind,
    pub model_prefix: Cow<'static, str>,
    pub kind: MediaKind,
    pub effective_from_ms: i64,
    pub pricing: MediaPricing,
}

/// What one media call used. A unit the call could not measure is `None`;
/// a row that bills that unit then has no price for the call.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MediaUsage {
    pub input_chars: Option<u64>,
    pub audio_seconds: Option<f64>,
    pub requests: u64,
}

/// Exact effective pricing row reduced to the conservative rates needed to
/// reserve one physical provider attempt. The identity is content-free and
/// binds the reservation to the same immutable row used for settlement.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmPhysicalPricingQuote {
    pricing_version: String,
    input_per_m_upper: f64,
    output_per_m_upper: f64,
}

impl LlmPhysicalPricingQuote {
    pub fn pricing_version(&self) -> &str {
        &self.pricing_version
    }

    pub fn cost_upper_microusd(
        &self,
        input_token_upper: u64,
        output_token_upper: u64,
    ) -> Option<u64> {
        let usd = (input_token_upper as f64 * self.input_per_m_upper
            + output_token_upper as f64 * self.output_per_m_upper)
            / 1_000_000.0;
        usd_to_microusd_ceil(usd)
    }
}

impl PricingTable {
    /// What code alone prices, under the pricing file: a catch-all per
    /// provider, the free local engines, and the provider web-search fees.
    /// Every per-model rate lives in the pricing file
    /// ([`crate::pricing_file`]); these rows only keep an unknown model of a
    /// known provider from billing as free when no file row covers it. The
    /// seed file repeats them, so a change goes there as a dated row.
    pub fn defaults() -> Self {
        use LLMProviderKind::*;
        let catch_all = |provider, prefix: &'static str, pricing| PricingRow::new(provider, prefix, 0, pricing);
        let mut rows = vec![
            catch_all(Anthropic, "claude-", ProviderPricing::anthropic(3.0, 15.0)),
            catch_all(Minimax, "", ProviderPricing::openai(1.0, 5.0)),
            catch_all(
                DeepSeek,
                "deepseek-",
                ProviderPricing {
                    input_per_m: 0.30,
                    output_per_m: 1.20,
                    cache_read_per_m: Some(0.006),
                    cache_write_per_m: None,
                },
            ),
            // Unknown `grok-` ids take the flagship rate: over-stating a
            // spend gate stops a run, under-stating it lets one overrun.
            catch_all(Xai, "grok-", ProviderPricing::openai_with_cache(2.00, 0.50, 6.00))
                .with_long_context(LongContextPricing::new(199_999, 2.0, 2.0)),
            catch_all(Sarvam, "", ProviderPricing::openai_with_cache(0.333, 0.125, 0.832)),
            catch_all(Gemini, "gemini-", ProviderPricing::openai_with_cache(0.30, 0.03, 2.5)),
            catch_all(Ollama, "", ProviderPricing::free()),
            catch_all(Yutori, "", ProviderPricing::free()),
        ];
        // Local decision models run on the owner's machine.
        for adapter in ["laya-mlx", "laya-onnx", "kev-mlx", "kev-onnx"] {
            rows.push(PricingRow::new(
                LLMProviderKind::Custom(format!("decision:{adapter}")),
                "",
                0,
                ProviderPricing::free(),
            ));
        }
        let web_search_rows = [(OpenAI, 0.01), (Anthropic, 0.01), (Gemini, 0.035), (OpenRouter, 0.007)]
            .into_iter()
            .map(|(provider, usd_per_call)| WebSearchPricingRow {
                provider,
                effective_from_ms: 0,
                usd_per_call,
            })
            .collect();
        // On-machine voice engines (Fluid, macOS speech) cost nothing; their
        // rows record volume and latency at an exact $0.
        let media_rows = [MediaKind::Tts, MediaKind::Stt, MediaKind::SttStreaming]
            .into_iter()
            .map(|kind| MediaPricingRow {
                provider: LLMProviderKind::Custom("local".into()),
                model_prefix: Cow::Borrowed(""),
                kind,
                effective_from_ms: 0,
                pricing: MediaPricing::default(),
            })
            .collect();
        Self {
            rows,
            realtime_rows: Vec::new(),
            web_search_rows,
            media_rows,
        }
    }

    /// The code defaults with the seed pricing file on top
    /// ([`crate::pricing_file::SEED_PRICING_JSON`]): the table used when no
    /// deployment file is installed.
    pub fn builtin() -> Self {
        let mut table = Self::defaults();
        table.layer(seed_rows().clone());
        table
    }

    /// The built-in table with a deployment pricing file on top. On equal
    /// prefix and date the file row wins, as it comes later.
    pub fn builtin_with_file(file: PricingFileRows) -> Self {
        let mut table = Self::builtin();
        table.layer(file);
        table
    }

    fn layer(&mut self, file: PricingFileRows) {
        self.rows.extend(file.text);
        self.realtime_rows.extend(file.realtime);
        self.web_search_rows.extend(file.web_search);
        self.media_rows.extend(file.media);
    }

    /// The built-in table with `extra_rows` layered on top — the table shape
    /// that [`install_pricing_table`] installs globally. On equal prefix
    /// length, a later `effective_from_ms` wins, so dated extras override the
    /// builtin base once effective.
    pub fn builtin_with(extra_rows: Vec<PricingRow>) -> Self {
        let mut table = Self::builtin();
        table.rows.extend(extra_rows);
        table
    }

    /// Builds a table from undated rows (all effective from 0). Kept for
    /// existing call sites; prefer `with_pricing_rows` for dated rows.
    pub fn with_rows(rows: Vec<(LLMProviderKind, &'static str, ProviderPricing)>) -> Self {
        Self {
            rows: rows
                .into_iter()
                .map(|(provider, prefix, pricing)| PricingRow::new(provider, prefix, 0, pricing))
                .collect(),
            realtime_rows: Vec::new(),
            web_search_rows: Vec::new(),
            media_rows: Vec::new(),
        }
    }

    /// Builds a table from effective-dated rows. Realtime and web-search rows
    /// come from the built-in table so they still resolve in explicit-table
    /// tests.
    pub fn with_pricing_rows(rows: Vec<PricingRow>) -> Self {
        let builtin = Self::builtin();
        Self {
            rows,
            realtime_rows: builtin.realtime_rows,
            web_search_rows: builtin.web_search_rows,
            media_rows: builtin.media_rows,
        }
    }

    /// Resolves pricing for `(provider, model)` at the current wall time.
    /// Prefer [`PricingTable::lookup_at`] with the call timestamp.
    pub fn lookup(&self, provider: &LLMProviderKind, model: &str) -> Option<ProviderPricing> {
        self.lookup_at(provider, model, now_unix_ms())
    }

    /// Resolves pricing for `(provider, model)` as of `at_ms`: among rows with
    /// matching provider whose prefix matches the model AND that are already
    /// effective (`effective_from_ms <= at_ms` — the date filter applies
    /// before prefix selection, so a future-dated longer prefix cannot win
    /// today), picks the longest prefix first, then the latest
    /// `effective_from_ms` among equal-length prefixes. Returns `None` when
    /// no row covers the request.
    pub fn lookup_at(
        &self,
        provider: &LLMProviderKind,
        model: &str,
        at_ms: i64,
    ) -> Option<ProviderPricing> {
        self.lookup_row_at(provider, model, at_ms)
            .map(|row| row.pricing)
    }

    /// Stable, content-free identity of the exact effective text-pricing row.
    ///
    /// Analytics must not label two different rate tables with the same
    /// `pricing_version`: doing so makes historical costs impossible to audit
    /// after a configuration change. The fingerprint includes the resolved
    /// provider/prefix/effective date, every rate, and the optional
    /// long-context rule without exposing custom provider or model names.
    pub fn pricing_version_at(
        &self,
        provider: &LLMProviderKind,
        model: &str,
        at_ms: i64,
    ) -> Option<String> {
        self.lookup_row_at(provider, model, at_ms)
            .map(text_pricing_row_version)
    }

    fn lookup_row_at(
        &self,
        provider: &LLMProviderKind,
        model: &str,
        at_ms: i64,
    ) -> Option<&PricingRow> {
        self.rows
            .iter()
            .filter(|row| &row.provider == provider)
            .filter(|row| row.effective_from_ms <= at_ms)
            .filter(|row| model.starts_with(&*row.model_prefix))
            .max_by_key(|row| (row.model_prefix.len(), row.effective_from_ms))
    }

    /// Resolve the exact effective row and conservative per-token rate ceiling
    /// for a request whose whole context/output limits are already frozen.
    /// Unknown or malformed pricing is unavailable rather than silently free.
    pub fn physical_attempt_quote_at(
        &self,
        provider: &LLMProviderKind,
        model: &str,
        context_window_tokens: u32,
        at_ms: i64,
    ) -> Option<LlmPhysicalPricingQuote> {
        let row = self.lookup_row_at(provider, model, at_ms)?;
        let pricing = row.pricing;
        let input_multiplier = row
            .long_context
            .filter(|rule| context_window_tokens > rule.threshold_tokens)
            .map_or(1.0, |rule| rule.input_multiplier);
        let output_multiplier = row
            .long_context
            .filter(|rule| context_window_tokens > rule.threshold_tokens)
            .map_or(1.0, |rule| rule.output_multiplier);
        let input_per_m_upper = [
            pricing.input_per_m,
            pricing.cache_read_per_m.unwrap_or(pricing.input_per_m),
            pricing.cache_write_per_m.unwrap_or(pricing.input_per_m),
        ]
        .into_iter()
        .fold(0.0_f64, f64::max)
            * input_multiplier;
        let output_per_m_upper = pricing.output_per_m * output_multiplier;
        if !input_per_m_upper.is_finite()
            || !output_per_m_upper.is_finite()
            || input_per_m_upper < 0.0
            || output_per_m_upper < 0.0
        {
            return None;
        }
        Some(LlmPhysicalPricingQuote {
            pricing_version: text_pricing_row_version(row),
            input_per_m_upper,
            output_per_m_upper,
        })
    }

    /// Resolves realtime pricing for `model` as of `at_ms` — the realtime
    /// counterpart of [`lookup_at`](Self::lookup_at): among realtime rows whose
    /// prefix matches and that are already effective, picks the longest prefix,
    /// then the latest effective date. `None` when no realtime row covers it.
    pub fn realtime_pricing_at(&self, model: &str, at_ms: i64) -> Option<RealtimePricing> {
        self.realtime_pricing_row_at(model, at_ms)
            .map(|row| row.pricing)
    }

    /// Stable, content-free identity of the exact effective realtime-pricing
    /// row used for a call.
    pub fn realtime_pricing_version_at(&self, model: &str, at_ms: i64) -> Option<String> {
        self.realtime_pricing_row_at(model, at_ms)
            .map(realtime_pricing_row_version)
    }

    /// USD per provider-executed web search call for `provider` as of
    /// `at_ms`: the latest effective row, else 0.0.
    pub fn web_search_cost_per_call_at(&self, provider: &LLMProviderKind, at_ms: i64) -> f64 {
        self.web_search_rows
            .iter()
            .filter(|row| &row.provider == provider && row.effective_from_ms <= at_ms)
            .max_by_key(|row| row.effective_from_ms)
            .map_or(0.0, |row| row.usd_per_call)
    }

    fn media_pricing_row_at(
        &self,
        provider: &LLMProviderKind,
        model: &str,
        kind: MediaKind,
        at_ms: i64,
    ) -> Option<&MediaPricingRow> {
        self.media_rows
            .iter()
            .filter(|row| &row.provider == provider && row.kind == kind)
            .filter(|row| row.effective_from_ms <= at_ms)
            .filter(|row| model.starts_with(&*row.model_prefix))
            .max_by_key(|row| (row.model_prefix.len(), row.effective_from_ms))
    }

    /// Stable identity of the media row that prices `(provider, model, kind)`
    /// at `at_ms`, or `None` when no row covers it.
    pub fn media_pricing_version_at(
        &self,
        provider: &LLMProviderKind,
        model: &str,
        kind: MediaKind,
        at_ms: i64,
    ) -> Option<String> {
        self.media_pricing_row_at(provider, model, kind, at_ms)
            .map(media_pricing_row_version)
    }

    /// USD for one media call, or `None` when no row covers it or the call
    /// lacks a unit its row bills (a compressed upload's unknown duration):
    /// unknown, never a fabricated zero.
    pub fn compute_media_cost_at(
        &self,
        provider: &LLMProviderKind,
        model: &str,
        kind: MediaKind,
        usage: &MediaUsage,
        at_ms: i64,
    ) -> Option<f64> {
        let p = self.media_pricing_row_at(provider, model, kind, at_ms)?.pricing;
        let chars = if p.usd_per_million_chars > 0.0 {
            usage.input_chars? as f64 * p.usd_per_million_chars / 1_000_000.0
        } else {
            0.0
        };
        let audio = if p.usd_per_audio_minute > 0.0 {
            usage.audio_seconds.filter(|s| s.is_finite() && *s >= 0.0)? / 60.0 * p.usd_per_audio_minute
        } else {
            0.0
        };
        Some(chars + audio + usage.requests as f64 * p.usd_per_request)
    }

    fn realtime_pricing_row_at(&self, model: &str, at_ms: i64) -> Option<&RealtimePricingRow> {
        self.realtime_rows
            .iter()
            .filter(|row| row.effective_from_ms <= at_ms)
            .filter(|row| model.starts_with(&*row.model_prefix))
            .max_by_key(|row| (row.model_prefix.len(), row.effective_from_ms))
    }

    /// USD cost for realtime `usage` on `model` as of `at_ms`. Audio and text
    /// buckets bill independently at their own rates. Returns 0.0 when no
    /// realtime row matches, so the value is always safe to emit.
    pub fn compute_realtime_cost_at(&self, model: &str, usage: &RealtimeUsage, at_ms: i64) -> f64 {
        let p = match self.realtime_pricing_at(model, at_ms) {
            Some(p) => p,
            None => return 0.0,
        };
        const M: f64 = 1_000_000.0;
        usage.text_input_tokens as f64 * p.text_input_per_m / M
            + usage.text_cached_input_tokens as f64 * p.text_cached_input_per_m / M
            + usage.text_output_tokens as f64 * p.text_output_per_m / M
            + usage.audio_input_tokens as f64 * p.audio_input_per_m / M
            + usage.audio_cached_input_tokens as f64 * p.audio_cached_input_per_m / M
            + usage.audio_output_tokens as f64 * p.audio_output_per_m / M
            + usage.billed_seconds * p.per_second
    }
}

fn hash_pricing_field(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_optional_rate(hasher: &mut blake3::Hasher, value: Option<f64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_bits().to_le_bytes());
        },
        None => {
            hasher.update(&[0]);
        },
    }
}

#[cfg(test)]
mod duration_billed_realtime_tests {
    use super::*;

    /// GPT-Live is billed by the clock, not by tokens: "Voice sessions cost
    /// $0.05 per minute, billed per second"
    /// (https://developers.openai.com/api/docs/models/gpt-live-1). Its backend
    /// model's tokens are billed separately — in Magician that is the chat
    /// turn a delegation runs, which is metered on its own row — so the token
    /// rates here are genuinely zero rather than missing.
    #[test]
    fn a_live_session_bills_its_seconds_and_not_its_tokens() {
        let minute = RealtimeUsage {
            billed_seconds: 60.0,
            ..RealtimeUsage::default()
        };
        assert!((compute_realtime_cost_at("gpt-live-1", &minute, 0) - 0.05).abs() < 1e-9);
        let half = RealtimeUsage {
            billed_seconds: 30.0,
            ..RealtimeUsage::default()
        };
        assert!((compute_realtime_cost_at("gpt-live-1", &half, 0) - 0.025).abs() < 1e-9);
        // Tokens that leak in from the delegated turn are not billed twice.
        let with_tokens = RealtimeUsage {
            billed_seconds: 60.0,
            audio_input_tokens: 10_000,
            text_output_tokens: 5_000,
            ..RealtimeUsage::default()
        };
        assert!((compute_realtime_cost_at("gpt-live-1", &with_tokens, 0) - 0.05).abs() < 1e-9);
    }

    /// A token-priced realtime model is unchanged by the new dimension: no
    /// seconds, no seconds charge.
    #[test]
    fn token_priced_realtime_models_are_unchanged() {
        let usage = RealtimeUsage {
            audio_input_tokens: 1_000_000,
            ..RealtimeUsage::default()
        };
        assert!((compute_realtime_cost_at("gpt-realtime-2.1", &usage, 0) - 32.0).abs() < 1e-9);
        let seconds_too = RealtimeUsage {
            billed_seconds: 600.0,
            ..usage
        };
        assert!(
            (compute_realtime_cost_at("gpt-realtime-2.1", &seconds_too, 0) - 32.0).abs() < 1e-9,
            "a token-priced model has no per-second rate to charge"
        );
    }
}

fn text_pricing_row_version(row: &PricingRow) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_pricing_field(&mut hasher, b"text-pricing-row-v1");
    hash_pricing_field(&mut hasher, row.provider.as_str().as_bytes());
    hash_pricing_field(&mut hasher, row.model_prefix.as_bytes());
    hasher.update(&row.effective_from_ms.to_le_bytes());
    hasher.update(&row.pricing.input_per_m.to_bits().to_le_bytes());
    hasher.update(&row.pricing.output_per_m.to_bits().to_le_bytes());
    hash_optional_rate(&mut hasher, row.pricing.cache_read_per_m);
    hash_optional_rate(&mut hasher, row.pricing.cache_write_per_m);
    match row.long_context {
        Some(rule) => {
            hasher.update(&[1]);
            hasher.update(&rule.threshold_tokens.to_le_bytes());
            hasher.update(&rule.input_multiplier.to_bits().to_le_bytes());
            hasher.update(&rule.output_multiplier.to_bits().to_le_bytes());
        },
        None => {
            hasher.update(&[0]);
        },
    }
    format!("pricing-row-v1:{}", hasher.finalize().to_hex())
}

fn realtime_pricing_row_version(row: &RealtimePricingRow) -> String {
    let mut hasher = blake3::Hasher::new();
    // v2 added the per-second rate. Two rows that differ only in what a
    // second costs must not fingerprint alike, and a v1 fingerprint must not
    // collide with a v2 one over the same token rates.
    hash_pricing_field(&mut hasher, b"realtime-pricing-row-v2");
    hash_pricing_field(&mut hasher, row.model_prefix.as_bytes());
    hasher.update(&row.effective_from_ms.to_le_bytes());
    for rate in [
        row.pricing.text_input_per_m,
        row.pricing.text_cached_input_per_m,
        row.pricing.text_output_per_m,
        row.pricing.audio_input_per_m,
        row.pricing.audio_cached_input_per_m,
        row.pricing.audio_output_per_m,
        row.pricing.per_second,
    ] {
        hasher.update(&rate.to_bits().to_le_bytes());
    }
    format!("pricing-row-v1:{}", hasher.finalize().to_hex())
}

fn media_pricing_row_version(row: &MediaPricingRow) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_pricing_field(&mut hasher, b"media-pricing-row-v1");
    hash_pricing_field(&mut hasher, row.provider.as_str().as_bytes());
    hash_pricing_field(&mut hasher, row.model_prefix.as_bytes());
    hash_pricing_field(&mut hasher, row.kind.as_str().as_bytes());
    hasher.update(&row.effective_from_ms.to_le_bytes());
    for rate in [
        row.pricing.usd_per_million_chars,
        row.pricing.usd_per_audio_minute,
        row.pricing.usd_per_request,
    ] {
        hasher.update(&rate.to_bits().to_le_bytes());
    }
    format!("pricing-row-v1:{}", hasher.finalize().to_hex())
}

/// USD for one text-to-speech or speech-to-text call against the active
/// table at `at_ms`; see [`PricingTable::compute_media_cost_at`].
pub fn compute_media_cost_at(
    provider: &LLMProviderKind,
    model: &str,
    kind: MediaKind,
    usage: &MediaUsage,
    at_ms: i64,
) -> Option<f64> {
    active_table().compute_media_cost_at(provider, model, kind, usage, at_ms)
}

/// Fingerprint of the media row pricing `(provider, model, kind)` at `at_ms`.
pub fn media_pricing_version_at(
    provider: &LLMProviderKind,
    model: &str,
    kind: MediaKind,
    at_ms: i64,
) -> Option<String> {
    active_table().media_pricing_version_at(provider, model, kind, at_ms)
}

static ACTIVE: OnceLock<PricingTable> = OnceLock::new();

/// The process-wide pricing table: the installed table when
/// [`install_pricing_table`] ran first, otherwise the builtin base. The first
/// call wins the `OnceLock`, so cost computation before installation locks in
/// the builtin table for the process lifetime.
pub fn active_table() -> &'static PricingTable {
    ACTIVE.get_or_init(PricingTable::builtin)
}

/// The seed pricing file compiled into the binary, parsed once. A seed that
/// fails to parse is a build defect the seed tests catch; at runtime it is
/// logged and the code defaults stand alone.
fn seed_rows() -> &'static PricingFileRows {
    static SEED: OnceLock<PricingFileRows> = OnceLock::new();
    SEED.get_or_init(|| {
        parse_pricing_file(SEED_PRICING_JSON).unwrap_or_else(|error| {
            tracing::error!(%error, "seed pricing file does not parse; only the code defaults price calls");
            PricingFileRows::default()
        })
    })
}

/// Installs the built-in table with a deployment pricing file on top as the
/// process-wide table. Call once at startup, before any cost computation; a
/// second call (or one after [`active_table`] captured the built-in table)
/// logs a warning and is a no-op.
pub fn install_pricing_file(file: PricingFileRows) {
    let row_count = file.len();
    if ACTIVE.set(PricingTable::builtin_with_file(file)).is_err() {
        warn!(
            row_count,
            "pricing table already active; install_pricing_file is a no-op \
             (install once at startup, before any cost computation)"
        );
    }
}

/// Installs builtin + `extra_rows` as the process-wide pricing table read by
/// [`compute_cost`]/[`compute_cost_at`]. Call once at startup, before any
/// cost computation: if the table is already set (installed twice, or
/// [`active_table`] already captured the builtin), this logs a warning and
/// is a no-op.
pub fn install_pricing_table(extra_rows: Vec<PricingRow>) {
    let extra_row_count = extra_rows.len();
    if ACTIVE.set(PricingTable::builtin_with(extra_rows)).is_err() {
        warn!(
            extra_row_count,
            "pricing table already active; install_pricing_table is a no-op \
             (install once at startup, before any cost computation)"
        );
    }
}

/// Compute USD cost using the active pricing table at the current wall time.
///
/// Convenience wrapper over [`compute_cost_at`] — call sites that know when
/// the call happened should prefer `compute_cost_at` with that timestamp so
/// effective-dated rate changes resolve against the call time, not the
/// compute time.
///
/// Returns 0.0 when no pricing row matches the `(provider, model)` pair, so
/// this always produces a value safe to emit.
pub fn compute_cost(provider: &LLMProviderKind, model: &str, usage: &TokenUsage) -> f64 {
    compute_cost_at(provider, model, usage, now_unix_ms())
}

/// Compute USD cost using the active pricing table as of `at_ms` (epoch
/// millis — typically the LLM call's start timestamp).
///
/// Returns 0.0 when no pricing row matches the `(provider, model)` pair, so
/// this always produces a value safe to emit.
pub fn compute_cost_at(
    provider: &LLMProviderKind,
    model: &str,
    usage: &TokenUsage,
    at_ms: i64,
) -> f64 {
    compute_cost_with_at(active_table(), provider, model, usage, at_ms)
}

/// Convert a trusted, non-negative USD cost to integer micro-USD by rounding
/// upward. Resource settlement must never round a positive provider charge
/// down to zero or accept non-finite/overflowing arithmetic.
pub fn usd_to_microusd_ceil(usd: f64) -> Option<u64> {
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    let microusd = (usd * 1_000_000.0).ceil();
    if !microusd.is_finite() || microusd > u64::MAX as f64 {
        return None;
    }
    Some(microusd as u64)
}

/// Compute USD cost against an explicit pricing table at the current wall
/// time. Prefer [`compute_cost_with_at`] with the call timestamp.
pub fn compute_cost_with(
    table: &PricingTable,
    provider: &LLMProviderKind,
    model: &str,
    usage: &TokenUsage,
) -> f64 {
    compute_cost_with_at(table, provider, model, usage, now_unix_ms())
}

/// Compute USD cost against an explicit pricing table as of `at_ms` (useful
/// for tests, runtime-loaded configuration, or repricing historical rows).
pub fn compute_cost_with_at(
    table: &PricingTable,
    provider: &LLMProviderKind,
    model: &str,
    usage: &TokenUsage,
    at_ms: i64,
) -> f64 {
    let row = match table.lookup_row_at(provider, model, at_ms) {
        Some(row) => row,
        None => return 0.0,
    };
    let pricing = row.pricing;

    let input = usage.prompt_tokens.unwrap_or(0) as f64;
    let output = usage.completion_tokens.unwrap_or(0) as f64;
    let cache_read = usage.cached_tokens.unwrap_or(0) as f64;
    let cache_write = usage.cache_creation_tokens.unwrap_or(0) as f64;

    // `prompt_tokens` already includes cache_read and cache_write on
    // providers that report cache detail, so subtract them to avoid
    // double-charging at the base rate.
    let uncached_input = (input - cache_read - cache_write).max(0.0);
    let (input_multiplier, output_multiplier) = row
        .long_context
        .filter(|rule| input > f64::from(rule.threshold_tokens))
        .map(|rule| (rule.input_multiplier, rule.output_multiplier))
        .unwrap_or((1.0, 1.0));

    let mut cost = 0.0;
    cost += uncached_input * pricing.input_per_m * input_multiplier / 1_000_000.0;
    cost += output * pricing.output_per_m * output_multiplier / 1_000_000.0;
    if let Some(rate) = pricing.cache_read_per_m {
        cost += cache_read * rate * input_multiplier / 1_000_000.0;
    } else {
        cost += cache_read * pricing.input_per_m * input_multiplier / 1_000_000.0;
    }
    if let Some(rate) = pricing.cache_write_per_m {
        cost += cache_write * rate * input_multiplier / 1_000_000.0;
    } else {
        cost += cache_write * pricing.input_per_m * input_multiplier / 1_000_000.0;
    }
    cost
}

/// Per-call USD rate for provider-executed server-side web search, as of
/// `at_ms`. Token usage cannot see these charges — the provider bills each
/// executed search separately — so settlement must add them explicitly via
/// [`compute_cost_with_server_web_search_at`].
///
/// The rates are the pricing file's `web_search_rates` (effective-dated per
/// provider) over the code defaults in [`PricingTable::defaults`]. Unsupported
/// transports return 0.0 — they fail the flag closed at the provider layer,
/// so a nonzero count there is impossible by construction.
pub fn server_web_search_cost_per_call_at(provider: &LLMProviderKind, at_ms: i64) -> f64 {
    active_table().web_search_cost_per_call_at(provider, at_ms)
}

/// Token cost plus provider-executed web search charges for one response.
/// `search_calls` is the response's `web_search_call_count`.
pub fn compute_cost_with_server_web_search_at(
    provider: &LLMProviderKind,
    model: &str,
    usage: &TokenUsage,
    search_calls: usize,
    at_ms: i64,
) -> f64 {
    compute_cost_at(provider, model, usage, at_ms)
        + server_web_search_cost_per_call_at(provider, at_ms) * search_calls as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::LLMProviderKind;

    #[test]
    fn server_web_search_rates_cover_supported_transports_only() {
        let at = 0i64;
        assert_eq!(
            server_web_search_cost_per_call_at(&LLMProviderKind::OpenAI, at),
            0.01
        );
        assert_eq!(
            server_web_search_cost_per_call_at(&LLMProviderKind::Anthropic, at),
            0.01
        );
        assert_eq!(
            server_web_search_cost_per_call_at(&LLMProviderKind::Gemini, at),
            0.035
        );
        assert_eq!(
            server_web_search_cost_per_call_at(&LLMProviderKind::OpenRouter, at),
            0.007
        );
        for unsupported in [
            LLMProviderKind::DeepSeek,
            LLMProviderKind::Minimax,
            LLMProviderKind::Ollama,
            LLMProviderKind::Yutori,
        ] {
            assert_eq!(
                server_web_search_cost_per_call_at(&unsupported, at),
                0.0,
                "{unsupported} must not accrue search charges"
            );
        }
    }

    #[test]
    fn compute_cost_with_server_web_search_adds_per_call_charges() {
        // No pricing row matches this model, so the token cost is zero and
        // the search charge must be the entire reported cost.
        let usage = usage(1_000, 100, 0, 0);
        let at = 0i64;
        let provider = LLMProviderKind::OpenAI;
        let model = "no-such-model";
        assert_eq!(compute_cost_at(&provider, model, &usage, at), 0.0);
        let total = compute_cost_with_server_web_search_at(&provider, model, &usage, 3, at);
        assert!((total - 0.03).abs() < 1e-9);
        assert_eq!(
            compute_cost_with_server_web_search_at(&provider, model, &usage, 0, at),
            0.0
        );
    }

    fn usage(input: u32, output: u32, cache_read: u32, cache_write: u32) -> TokenUsage {
        TokenUsage {
            prompt_tokens: Some(input),
            completion_tokens: Some(output),
            total_tokens: Some(input + output),
            reasoning_tokens: None,
            cached_tokens: Some(cache_read),
            cache_creation_tokens: Some(cache_write),
        }
    }

    fn anthropic_row(
        prefix: impl Into<std::borrow::Cow<'static, str>>,
        effective_from_ms: i64,
        input: f64,
        output: f64,
    ) -> PricingRow {
        PricingRow::new(
            LLMProviderKind::Anthropic,
            prefix,
            effective_from_ms,
            ProviderPricing::anthropic(input, output),
        )
    }

    #[test]
    fn anthropic_sonnet_cost_matches_published_rate() {
        // 1M input + 1M output at $3 + $15 = $18.
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::Anthropic, "claude-sonnet-4-6", &u);
        assert!((c - 18.0).abs() < 1e-6, "expected $18, got ${}", c);
    }

    #[test]
    fn anthropic_opus_4_7_cost_matches_published_rate() {
        // 1M input + 1M output at $5 + $25 = $30.
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::Anthropic, "claude-opus-4-7", &u);
        assert!((c - 30.0).abs() < 1e-6, "expected $30, got ${}", c);
    }

    #[test]
    fn anthropic_opus_5_matches_opus_4_7_headline_rates() {
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::Anthropic, "claude-opus-5", &u);
        assert!((c - 30.0).abs() < 1e-6, "expected $30, got ${c}");
    }

    #[test]
    fn anthropic_fable_5_1_uses_quarter_cache_read() {
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::Anthropic, "claude-fable-5-1", &u);
        assert!((c - 60.0).abs() < 1e-6, "expected $60, got ${c}");
        let cached = usage(1_000_000, 0, 1_000_000, 0);
        let fable_5_1 = compute_cost(&LLMProviderKind::Anthropic, "claude-fable-5-1", &cached);
        let fable_5 = compute_cost(&LLMProviderKind::Anthropic, "claude-fable-5", &cached);
        assert!(
            (fable_5_1 - 0.25).abs() < 1e-6,
            "expected $0.25 Fable 5.1 cache read, got ${fable_5_1}"
        );
        assert!(
            (fable_5 - 1.0).abs() < 1e-6,
            "expected $1.00 Fable 5 cache read, got ${fable_5}"
        );
    }

    #[test]
    fn anthropic_cache_read_charged_at_ten_percent() {
        // 1M of the input is cache-read; should cost 10% of the base input rate.
        // 1M cache_read × $0.30 + 0 uncached × $3 + 0 output = $0.30
        let u = usage(1_000_000, 0, 1_000_000, 0);
        let c = compute_cost(&LLMProviderKind::Anthropic, "claude-sonnet-4-6", &u);
        assert!((c - 0.30).abs() < 1e-6, "expected $0.30, got ${}", c);
    }

    #[test]
    fn anthropic_cache_write_charged_at_premium() {
        // 1M cache_write × $3.75 = $3.75
        let u = usage(1_000_000, 0, 0, 1_000_000);
        let c = compute_cost(&LLMProviderKind::Anthropic, "claude-sonnet-4-6", &u);
        assert!((c - 3.75).abs() < 1e-6, "expected $3.75, got ${}", c);
    }

    #[test]
    fn anthropic_mixed_buckets_sum_correctly() {
        // 100k uncached input + 200k cache_read + 300k cache_write + 500k output.
        // = 100k*$3 + 200k*$0.30 + 300k*$3.75 + 500k*$15, all /1M
        // = 0.30 + 0.06 + 1.125 + 7.50 = $8.985
        let u = usage(600_000, 500_000, 200_000, 300_000);
        let c = compute_cost(&LLMProviderKind::Anthropic, "claude-sonnet-4-6", &u);
        assert!((c - 8.985).abs() < 1e-6, "expected $8.985, got ${}", c);
    }

    #[test]
    fn minimax_pricing_row_is_non_zero() {
        // Minimax (OpenAI-compatible chat API); pricing row exists and is non-zero.
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::Minimax, "MiniMax-M2", &u);
        assert!(c > 0.0, "minimax cost should be non-zero, got {}", c);
    }

    #[test]
    fn deepseek_v4_pricing_uses_provider_specific_cache_read_rate() {
        // 1M cached input + 1M output: no uncached input is billed, so this is
        // V4.1 Flash peak output ($1.20) + peak cache hits ($0.006).
        let u = usage(1_000_000, 1_000_000, 1_000_000, 0);
        let c = compute_cost(&LLMProviderKind::DeepSeek, "deepseek-flash", &u);
        assert!((c - 1.206).abs() < 1e-6, "expected $1.206, got ${}", c);
    }

    /// xAI's own `cost_in_usd_ticks` (1e-10 USD) for a live grok-4.7 call on
    /// 2026-09-23 — 1,249 input of which 1,152 cached, 87 output — was
    /// 12,920,000 ticks = $0.001292; the table must reproduce it. A prompt
    /// that reaches 200k tokens bills every token at twice the rate.
    #[test]
    fn grok_4_7_matches_the_providers_own_billed_cost() {
        let live = usage(1_249, 87, 1_152, 0);
        let cost = compute_cost(&LLMProviderKind::Xai, "grok-4.7", &live);
        assert!(
            (cost - 0.001292).abs() < 1e-9,
            "expected $0.001292, got ${cost}"
        );

        let long = usage(200_000, 1_000_000, 0, 0);
        let cost = compute_cost(&LLMProviderKind::Xai, "grok-4.7", &long);
        assert!(
            (cost - (0.2 * 4.0 + 12.0)).abs() < 1e-9,
            "200k prompt → $4 in / $12 out per 1M, got ${cost}"
        );
        let cached_4_5 = usage(1_000_000, 0, 1_000_000, 0);
        let cost = compute_cost(&LLMProviderKind::Xai, "grok-4.5", &cached_4_5);
        assert!(
            (cost - 0.60).abs() < 1e-9,
            "grok-4.5 cache read ×2 above 200k, got ${cost}"
        );
        assert!(
            compute_cost(
                &LLMProviderKind::Xai,
                "grok-build-0.1",
                &usage(1_000_000, 0, 0, 0)
            ) > 0.0
        );
    }

    /// V4.1 Flash peak list (2026-09-10): miss $0.30 / hit $0.006 / out $1.20.
    #[test]
    fn sarvam_105b_bills_rupee_list_at_the_stated_rate_with_cached_discount() {
        let usage = TokenUsage {
            prompt_tokens: Some(1_000_000),
            completion_tokens: Some(1_000_000),
            cached_tokens: Some(400_000),
            ..Default::default()
        };
        // ₹29.28 / ₹10.98 / ₹73.2 at launch; ₹15 / ₹5 / ₹60 from 2026-10-15
        // (checked that day), at ₹88/USD.
        let table = PricingTable::builtin();
        let day = |date| crate::pricing_file::parse_effective_from_ms(date).unwrap();
        for model in ["sarvam-105b", "sarvam-105b-conversations", "sarvam-next"] {
            for (at_ms, expected) in [
                (day("2026-10-14"), 0.6 * 0.333 + 0.4 * 0.125 + 0.832),
                (day("2026-10-15"), 0.6 * 0.170455 + 0.4 * 0.056818 + 0.681818),
            ] {
                let cost = compute_cost_with_at(&table, &LLMProviderKind::Sarvam, model, &usage, at_ms);
                assert!((cost - expected).abs() < 1e-9, "{model} at {at_ms}: {cost}");
            }
        }
    }

    /// V4 Pro keeps its last distinct dashboard rate until 2026-09-14.
    #[test]
    fn deepseek_v41_flash_matches_published_peak_list() {
        let u = usage(1_000_000, 1_000_000, 0, 0);
        for model in [
            "deepseek-flash",
            "DeepSeek-V4-Flash-0731",
            "deepseek-v4-flash",
            "deepseek-v4-flash-vision-exp",
        ] {
            let flash = compute_cost(&LLMProviderKind::DeepSeek, model, &u);
            assert!(
                (flash - (0.30 + 1.20)).abs() < 1e-9,
                "{model} should be 1.50, got {flash}"
            );
        }
        // V4 Pro: its launch base, then the 2026-04-01 list, then from
        // 2026-09-14 DeepSeek routes it to Flash at Flash rates.
        let table = PricingTable::builtin();
        let day = |date| crate::pricing_file::parse_effective_from_ms(date).unwrap();
        for (at_ms, expected) in [
            (day("2026-03-31"), 0.435 + 0.87),
            (day("2026-09-13"), 1.74 + 3.48),
            (day("2026-09-14"), 0.30 + 1.20),
        ] {
            let pro = compute_cost_with_at(&table, &LLMProviderKind::DeepSeek, "deepseek-v4-pro", &u, at_ms);
            assert!((pro - expected).abs() < 1e-9, "pro at {at_ms}: expected {expected}, got {pro}");
        }
    }

    #[test]
    fn gemini_current_model_pricing_matches_published_standard_rates() {
        // Keep the prompt at or below 200K so the Pro row remains in its
        // standard price band rather than its published long-context band.
        let usage = usage(100_000, 100_000, 0, 0);
        for (model, expected) in [
            ("gemini-3.6-flash", 0.90),
            ("gemini-3.5-flash", 1.05),
            ("gemini-3.5-flash-lite", 0.28),
            ("gemini-3.1-flash-lite", 0.175),
            ("gemini-3.1-pro-preview", 1.40),
        ] {
            let cost = compute_cost(&LLMProviderKind::Gemini, model, &usage);
            assert!(
                (cost - expected).abs() < 1e-6,
                "expected ${expected} for {model}, got ${cost}"
            );
        }
    }

    #[test]
    fn gemini4_argon_pricing_changes_at_end_of_introductory_period() {
        let table = PricingTable::builtin();
        for (at_ms, input, cached, output) in [
            (OCTOBER_13_2026_MS, 2.0, 0.1, 10.0),
            (DECEMBER_1_2026_MS - 1, 2.0, 0.1, 10.0),
            (DECEMBER_1_2026_MS, 4.0, 0.2, 20.0),
        ] {
            let rate = table
                .lookup_at(&LLMProviderKind::Gemini, "gemini-4-argon", at_ms)
                .expect("Argon has its own pricing row");
            assert_eq!(rate.input_per_m, input);
            assert_eq!(rate.cache_read_per_m, Some(cached));
            assert_eq!(rate.output_per_m, output);
            assert_eq!(rate.cache_write_per_m, None);
            let cost = compute_cost_with_at(
                &table,
                &LLMProviderKind::Gemini,
                "gemini-4-argon",
                &usage(1_000_000, 1_000_000, 500_000, 0),
                at_ms,
            );
            assert!((cost - (input * 0.5 + cached * 0.5 + output)).abs() < 1e-9);
        }
    }

    #[test]
    fn gemini35_flash_lite_cache_read_uses_published_rate() {
        let usage = usage(1_000_000, 0, 1_000_000, 0);
        let cost = compute_cost(&LLMProviderKind::Gemini, "gemini-3.5-flash-lite", &usage);
        assert!((cost - 0.03).abs() < 1e-6, "expected $0.03, got ${cost}");
    }

    #[test]
    fn openai_has_no_cache_write_bucket() {
        // cache_write is ignored (charged at base input rate) for OpenAI.
        // gpt-5 = $1.25 input, $10 output
        // 1M input (all cache_write) + 0 output = $1.25 at base rate
        let u = usage(1_000_000, 0, 0, 1_000_000);
        let c = compute_cost(&LLMProviderKind::OpenAI, "gpt-5", &u);
        assert!((c - 1.25).abs() < 1e-6, "expected $1.25, got ${}", c);
    }

    #[test]
    fn gpt_5_cached_input_uses_published_ten_percent_rate() {
        // 1M cached input at $0.125/M; prompt_tokens includes cached_tokens.
        let u = usage(1_000_000, 0, 1_000_000, 0);
        let c = compute_cost(&LLMProviderKind::OpenAI, "gpt-5", &u);
        assert!((c - 0.125).abs() < 1e-6, "expected $0.125, got ${c}");
    }

    #[test]
    fn gpt_5_4_mixed_cached_and_uncached_input_is_not_double_billed() {
        // 160K uncached × $2.50 + 40K cached × $0.25 + 100K output × $15.
        // Keep this below the separate long-context threshold.
        let u = usage(200_000, 100_000, 40_000, 0);
        let c = compute_cost(&LLMProviderKind::OpenAI, "gpt-5.4", &u);
        assert!((c - 1.91).abs() < 1e-6, "expected $1.91, got ${c}");
    }

    #[test]
    fn gpt_5_4_long_context_multiplier_includes_cached_input() {
        // Prompt exceeds 272K, so all input rates are 2x and output is 1.5x:
        // 500K uncached × $2.50 × 2 + 500K cached × $0.25 × 2
        // + 100K output × $15 × 1.5 = $5.00.
        let u = usage(1_000_000, 100_000, 500_000, 0);
        let c = compute_cost(&LLMProviderKind::OpenAI, "gpt-5.4", &u);
        assert!((c - 5.0).abs() < 1e-6, "expected $5.00, got ${c}");
    }

    #[test]
    fn gpt_5_4_long_context_threshold_is_strictly_greater_than_272k() {
        let at_threshold = usage(272_000, 0, 0, 0);
        let over_threshold = usage(272_001, 0, 0, 0);
        let base = compute_cost(&LLMProviderKind::OpenAI, "gpt-5.4", &at_threshold);
        let surcharged = compute_cost(&LLMProviderKind::OpenAI, "gpt-5.4", &over_threshold);
        assert!((base - 0.68).abs() < 1e-6, "expected $0.68, got ${base}");
        assert!(
            (surcharged - 1.360005).abs() < 1e-6,
            "expected $1.360005, got ${surcharged}"
        );
    }

    #[test]
    fn gpt_5_4_mini_does_not_apply_long_context_multiplier() {
        let u = usage(1_000_000, 100_000, 500_000, 0);
        let c = compute_cost(&LLMProviderKind::OpenAI, "gpt-5.4-mini", &u);
        assert!((c - 0.8625).abs() < 1e-6, "expected $0.8625, got ${c}");
    }

    #[test]
    fn gpt_6_astra_short_context_matches_published_rates() {
        // 100K input + 100K output stays below the 272K long-context threshold:
        // $10 + $50 per million = $6.00.
        let u = usage(100_000, 100_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::OpenAI, "gpt-6-astra", &u);
        assert!((c - 6.0).abs() < 1e-6, "expected $6.00, got ${c}");
        let openrouter = compute_cost(&LLMProviderKind::OpenRouter, "openai/gpt-6-astra", &u);
        assert!(
            (openrouter - 6.0).abs() < 1e-6,
            "expected OpenRouter $6.00, got ${openrouter}"
        );
    }

    #[test]
    fn gpt_6_astra_cache_and_long_context_match_published_rates() {
        let cached = usage(100_000, 0, 100_000, 0);
        let cached_cost = compute_cost(&LLMProviderKind::OpenAI, "gpt-6-astra", &cached);
        assert!(
            (cached_cost - 0.10).abs() < 1e-6,
            "expected $0.10 cached input, got ${cached_cost}"
        );
        let writes = usage(100_000, 0, 0, 100_000);
        let write_cost = compute_cost(&LLMProviderKind::OpenAI, "gpt-6-astra", &writes);
        assert!(
            (write_cost - 1.25).abs() < 1e-6,
            "expected $1.25 cache write, got ${write_cost}"
        );
        let long = usage(300_000, 0, 0, 0);
        let long_cost = compute_cost(&LLMProviderKind::OpenAI, "gpt-6-astra", &long);
        assert!(
            (long_cost - 6.0).abs() < 1e-6,
            "expected $6.00 long-context input, got ${long_cost}"
        );
    }

    #[test]
    fn september_22_models_match_published_standard_rates() {
        let u = usage(100_000, 100_000, 0, 0);
        for (provider, model, expected) in [
            (LLMProviderKind::Anthropic, "claude-opus-5-5", 2.4),
            (LLMProviderKind::OpenAI, "gpt-6-sol", 1.2),
            (LLMProviderKind::OpenAI, "gpt-6-luna", 0.06),
            (LLMProviderKind::OpenRouter, "openai/gpt-6-sol", 1.2),
        ] {
            let c = compute_cost(&provider, model, &u);
            assert!(
                (c - expected).abs() < 1e-6,
                "{model}: expected ${expected}, got ${c}"
            );
        }

        let table = PricingTable::builtin();
        let before = compute_cost_with_at(
            &table,
            &LLMProviderKind::OpenAI,
            "gpt-5.6-sol",
            &u,
            SEPTEMBER_22_2026_MS - 1,
        );
        let after = compute_cost_with_at(
            &table,
            &LLMProviderKind::OpenAI,
            "gpt-5.6-sol",
            &u,
            SEPTEMBER_22_2026_MS,
        );
        assert!((before - 3.5).abs() < 1e-6);
        assert!((after - 2.4).abs() < 1e-6);
    }

    #[test]
    fn sonnet_5_5_prices_all_buckets_from_its_launch() {
        for (provider, model) in [
            (LLMProviderKind::Anthropic, "claude-sonnet-5-5"),
            (LLMProviderKind::OpenRouter, "anthropic/claude-sonnet-5-5"),
        ] {
            for (u, expected) in [
                (usage(100_000, 100_000, 0, 0), 1.2),
                // Cache reads are $0.10 (0.05x input), as published.
                (usage(100_000, 0, 100_000, 0), 0.01),
                (usage(100_000, 0, 0, 100_000), 0.25),
            ] {
                let cost = compute_cost(&provider, model, &u);
                assert!(
                    (cost - expected).abs() < 1e-6,
                    "{model}: ${cost}, expected ${expected}"
                );
            }
        }
        // Before launch the id resolved to the `claude-` catch-all.
        let before = compute_cost_with_at(
            &PricingTable::builtin(),
            &LLMProviderKind::Anthropic,
            "claude-sonnet-5-5",
            &usage(100_000, 100_000, 0, 0),
            SEPTEMBER_28_2026_MS - 1,
        );
        assert!((before - 1.8).abs() < 1e-6, "pre-launch: ${before}");
    }

    #[test]
    fn gpt_6_1_sol_prices_all_buckets_and_preserves_previous_sol_rates() {
        for (provider, model) in [
            (LLMProviderKind::OpenAI, "gpt-6.1-sol"),
            (LLMProviderKind::OpenAI, "gpt-6.1-sol-2026-09-29"),
            (LLMProviderKind::OpenRouter, "openai/gpt-6.1-sol"),
        ] {
            for (u, expected) in [
                (usage(100_000, 100_000, 0, 0), 1.2),
                (usage(100_000, 0, 100_000, 0), 0.01),
                (usage(100_000, 0, 0, 100_000), 0.25),
                (usage(272_000, 100_000, 0, 0), 1.544),
                (usage(300_000, 100_000, 100_000, 100_000), 2.42),
            ] {
                let cost = compute_cost(&provider, model, &u);
                assert!(
                    (cost - expected).abs() < 1e-6,
                    "{model}: ${cost}, expected ${expected}"
                );
            }
        }
        let old = compute_cost(
            &LLMProviderKind::OpenAI,
            "gpt-6-sol",
            &usage(100_000, 0, 100_000, 0),
        );
        assert!((old - 0.02).abs() < 1e-6);
    }

    #[test]
    fn september_22_models_price_cache_and_long_context_buckets() {
        let opus_read = compute_cost(
            &LLMProviderKind::Anthropic,
            "claude-opus-5-5",
            &usage(100_000, 0, 100_000, 0),
        );
        assert!((opus_read - 0.02).abs() < 1e-6);

        let sol_write = compute_cost(
            &LLMProviderKind::OpenAI,
            "gpt-6-sol",
            &usage(100_000, 0, 0, 100_000),
        );
        assert!((sol_write - 0.25).abs() < 1e-6);

        let luna_long = compute_cost(
            &LLMProviderKind::OpenAI,
            "gpt-6-luna",
            &usage(300_000, 0, 0, 0),
        );
        assert!((luna_long - 0.06).abs() < 1e-6);
    }

    #[test]
    fn gpt_5_6_family_costs_match_current_short_context_rates() {
        // 100K input + 100K output stays below the long-context threshold.
        let u = usage(100_000, 100_000, 0, 0);
        for (model, expected) in [
            ("gpt-5.6-sol", 2.4),
            ("gpt-5.6-terra", 1.4),
            ("gpt-5.6-luna", 0.14),
        ] {
            let c = compute_cost(&LLMProviderKind::OpenAI, model, &u);
            assert!(
                (c - expected).abs() < 1e-6,
                "{model}: expected ${expected}, got ${c}"
            );
        }
    }

    #[test]
    fn gpt_5_6_cached_input_uses_ten_percent_rate() {
        // 100K cached input on Luna at $0.02/M; prompt includes cached input.
        let u = usage(100_000, 0, 100_000, 0);
        let c = compute_cost(&LLMProviderKind::OpenAI, "gpt-5.6-luna", &u);
        assert!((c - 0.002).abs() < 1e-6, "expected $0.002, got ${c}");
    }

    #[test]
    fn gpt_5_6_cache_writes_use_published_premium_rate() {
        // 100K cache-write tokens: Terra $2.50/M, Luna $0.25/M.
        let u = usage(100_000, 0, 0, 100_000);
        for (model, expected) in [("gpt-5.6-terra", 0.25), ("gpt-5.6-luna", 0.025)] {
            let c = compute_cost(&LLMProviderKind::OpenAI, model, &u);
            assert!(
                (c - expected).abs() < 1e-6,
                "{model}: expected ${expected}, got ${c}"
            );
        }
    }

    #[test]
    fn gpt_5_6_july_30_boundary_preserves_launch_prices_before_revision() {
        let table = PricingTable::builtin();
        let u = usage(100_000, 100_000, 0, 0);
        for (model, before_expected, after_expected) in
            [("gpt-5.6-terra", 1.75, 1.4), ("gpt-5.6-luna", 0.7, 0.14)]
        {
            let before = compute_cost_with_at(
                &table,
                &LLMProviderKind::OpenAI,
                model,
                &u,
                GPT_5_6_JULY_30_2026_MS - 1,
            );
            let after = compute_cost_with_at(
                &table,
                &LLMProviderKind::OpenAI,
                model,
                &u,
                GPT_5_6_JULY_30_2026_MS,
            );
            assert!(
                (before - before_expected).abs() < 1e-6,
                "{model}: expected pre-revision ${before_expected}, got ${before}"
            );
            assert!(
                (after - after_expected).abs() < 1e-6,
                "{model}: expected post-revision ${after_expected}, got ${after}"
            );
        }
    }

    #[test]
    fn gpt_5_6_current_long_context_rates_apply_above_272k() {
        // 300K input is billed at 2x across the current GPT-5.6 family.
        let u = usage(300_000, 0, 0, 0);
        for (model, expected) in [
            ("gpt-5.6-sol", 2.4),
            ("gpt-5.6-terra", 1.2),
            ("gpt-5.6-luna", 0.12),
        ] {
            let c = compute_cost(&LLMProviderKind::OpenAI, model, &u);
            assert!(
                (c - expected).abs() < 1e-6,
                "{model}: expected ${expected}, got ${c}"
            );
        }
    }

    // ── Realtime (voice) pricing ────────────────────────────────────────────
    use crate::types::RealtimeUsage;

    fn rt(
        audio_in: u64,
        audio_cached: u64,
        audio_out: u64,
        text_in: u64,
        text_out: u64,
    ) -> RealtimeUsage {
        RealtimeUsage {
            text_input_tokens: text_in,
            text_cached_input_tokens: 0,
            text_output_tokens: text_out,
            audio_input_tokens: audio_in,
            audio_cached_input_tokens: audio_cached,
            audio_output_tokens: audio_out,
            billed_seconds: 0.0,
        }
    }

    #[test]
    fn realtime_mini_resolves_over_realtime_2_prefix() {
        // Longest-prefix: gpt-realtime-2.1-mini must NOT collapse to gpt-realtime-2.
        let p = realtime_pricing("gpt-realtime-2.1-mini").unwrap();
        assert_eq!(p.audio_input_per_m, 10.0);
        assert_eq!(p.audio_output_per_m, 20.0);
        let p21 = realtime_pricing("gpt-realtime-2.1").unwrap();
        assert_eq!(p21.audio_input_per_m, 32.0);
    }

    #[test]
    fn realtime_flagship_cost_matches_published_rates() {
        // 1M in every bucket on gpt-realtime-2.1: audio 32+0.40+64 + text 4+0+24
        // = 96.40 + 28.00 = $124.40.
        let u = RealtimeUsage {
            text_input_tokens: 1_000_000,
            text_cached_input_tokens: 0,
            text_output_tokens: 1_000_000,
            audio_input_tokens: 1_000_000,
            audio_cached_input_tokens: 1_000_000,
            audio_output_tokens: 1_000_000,
            billed_seconds: 0.0,
        };
        let c = compute_realtime_cost("gpt-realtime-2.1", &u);
        assert!((c - 124.40).abs() < 1e-6, "expected $124.40, got ${c}");
    }

    #[test]
    fn realtime_mini_audio_is_three_ish_x_cheaper() {
        // 1M audio in + 1M audio out: mini $10+$20=$30, flagship $32+$64=$96.
        let u = rt(1_000_000, 0, 1_000_000, 0, 0);
        assert!((compute_realtime_cost("gpt-realtime-2.1-mini", &u) - 30.0).abs() < 1e-6);
        assert!((compute_realtime_cost("gpt-realtime-2.1", &u) - 96.0).abs() < 1e-6);
    }

    #[test]
    fn realtime_cached_audio_billed_at_discount() {
        // 1M cached audio input on mini at $0.30/M = $0.30 (not the $10 uncached rate).
        let u = rt(0, 1_000_000, 0, 0, 0);
        assert!((compute_realtime_cost("gpt-realtime-2.1-mini", &u) - 0.30).abs() < 1e-6);
    }

    #[test]
    fn realtime_2_1_priced_identically_to_realtime_2() {
        let u = rt(1_000_000, 0, 1_000_000, 500_000, 500_000);
        assert_eq!(
            compute_realtime_cost("gpt-realtime-2.1", &u),
            compute_realtime_cost("gpt-realtime-2", &u)
        );
    }

    #[test]
    fn realtime_unknown_model_is_zero() {
        let u = rt(1_000_000, 0, 1_000_000, 0, 0);
        assert_eq!(compute_realtime_cost("some-unknown-voice-model", &u), 0.0);
    }

    /// Text models reachable from config must resolve to their OWN row, not to
    /// a broad prefix or catch-all.
    ///
    /// Regression guard, and the text counterpart of
    /// `every_configured_realtime_model_is_priced`. Unlike the realtime table,
    /// a missing text row is not $0 — it silently inherits a neighbour, which
    /// is worse in one way: the number looks plausible. Four models were
    /// mispriced this way until 2026-07-27: `MiniMax-M2.7` and `MiniMax-M3`
    /// billed at M2's $1.00/$5.00 instead of $0.30/$1.20, and `gpt-4o-mini`
    /// billed at `gpt-4o`'s $2.50/$10.00, ~16x over.
    ///
    #[test]
    fn configured_text_models_resolve_to_their_own_row() {
        let table = active_table();
        let at = now_unix_ms();
        for (provider, model) in [
            (LLMProviderKind::Anthropic, "claude-sonnet-4-6"),
            (LLMProviderKind::Anthropic, "claude-opus-4-7"),
            (LLMProviderKind::Anthropic, "claude-opus-5"),
            (LLMProviderKind::Anthropic, "claude-opus-5-5"),
            (LLMProviderKind::Anthropic, "claude-sonnet-5-5"),
            (LLMProviderKind::Anthropic, "claude-fable-5-1"),
            (LLMProviderKind::Anthropic, "claude-haiku-4-5"),
            (LLMProviderKind::DeepSeek, "deepseek-flash"),
            (LLMProviderKind::DeepSeek, "DeepSeek-V4-Flash-0731"),
            (LLMProviderKind::DeepSeek, "deepseek-v4-flash-vision-exp"),
            (LLMProviderKind::DeepSeek, "deepseek-v4-pro"),
            (LLMProviderKind::Gemini, "gemini-3.1-pro-preview"),
            (LLMProviderKind::Gemini, "gemini-3.5-flash-lite"),
            (LLMProviderKind::Minimax, "MiniMax-M2.7"),
            (LLMProviderKind::Minimax, "MiniMax-M3"),
            // Retained row: no longer configured (the vision profile moved to
            // gpt-5.4-nano) but kept so historical rows reprice correctly.
            (LLMProviderKind::OpenAI, "gpt-4o-mini"),
            (LLMProviderKind::OpenAI, "gpt-5.4-nano"),
            (LLMProviderKind::OpenAI, "gpt-5.4-mini"),
            (LLMProviderKind::OpenAI, "gpt-5.4-nano"),
            (LLMProviderKind::OpenAI, "gpt-5.6-luna"),
            (LLMProviderKind::OpenAI, "gpt-5.6-sol"),
            (LLMProviderKind::OpenAI, "gpt-5.6-terra"),
            (LLMProviderKind::OpenAI, "gpt-6-astra"),
            (LLMProviderKind::OpenAI, "gpt-6-sol"),
            (LLMProviderKind::OpenAI, "gpt-6-luna"),
        ] {
            let row = table
                .lookup_row_at(&provider, model, at)
                .unwrap_or_else(|| panic!("{model} has no pricing row at all"));
            assert_eq!(
                &*row.model_prefix, model,
                "{model} resolves to the broader `{}` row — it inherits another \
                 model's rate, which reads as a plausible number rather than an error",
                row.model_prefix
            );
        }
    }

    /// Published MiniMax rates: M2.7 and M3 are both $0.30 in / $1.20 out.
    ///
    /// Sized UNDER M3's 512K long-context threshold on purpose — a 1M-token
    /// probe here silently doubles and looks like a mispriced row.
    #[test]
    fn minimax_m2_7_and_m3_are_cheaper_than_m2() {
        let u = TokenUsage {
            prompt_tokens: Some(100_000),
            completion_tokens: Some(100_000),
            ..Default::default()
        };
        let m2 = compute_cost(&LLMProviderKind::Minimax, "MiniMax-M2", &u);
        for model in ["MiniMax-M2.7", "MiniMax-M3"] {
            let cost = compute_cost(&LLMProviderKind::Minimax, model, &u);
            assert!(
                (cost - 0.15).abs() < 1e-9,
                "{model} should be (0.30 + 1.20) / 10 = 0.15, got {cost}"
            );
            assert!(cost < m2, "{model} must not inherit M2's dearer rate");
        }
    }

    /// M3 doubles above a 512K-token input.
    #[test]
    fn minimax_m3_doubles_past_the_long_context_threshold() {
        let over = TokenUsage {
            prompt_tokens: Some(600_000),
            completion_tokens: Some(1_000),
            ..Default::default()
        };
        let under = TokenUsage {
            prompt_tokens: Some(500_000),
            completion_tokens: Some(1_000),
            ..Default::default()
        };
        let over_cost = compute_cost(&LLMProviderKind::Minimax, "MiniMax-M3", &over);
        let under_cost = compute_cost(&LLMProviderKind::Minimax, "MiniMax-M3", &under);
        // 600k input at 2x is 0.36; 500k at 1x is 0.15 — the step is the point.
        assert!(
            (over_cost - (0.600_000 * 0.30 * 2.0 + 0.001 * 1.20 * 2.0)).abs() < 1e-9,
            "600k input should bill at the doubled rate, got {over_cost}"
        );
        assert!(
            over_cost > under_cost * 2.0,
            "crossing 512K must step the rate, not scale smoothly"
        );
    }

    /// Every realtime model this workspace can actually select must be priced.
    ///
    /// Regression guard. `gemini-3.1-flash-live-preview` shipped unpriced, and
    /// because an unknown realtime model resolves to `0.0` rather than an
    /// error, every Gemini voice turn was silently recorded as free — no log,
    /// no warning, just a $0 row. A new voice model added to config without a
    /// pricing row reproduces exactly that, so assert cost is non-zero for a
    /// turn that definitely consumed tokens.
    #[test]
    fn every_configured_realtime_model_is_priced() {
        let one_minute_ish = rt(1_500, 0, 1_500, 200, 200);
        for model in [
            "gpt-realtime-2.1",
            "gpt-realtime-2.1-mini",
            "gemini-3.1-flash-live-preview",
            "gemini-3.8-live",
            "gemini-3.8-live-extended-thinking",
            "gemini-3.5-live-translate-preview",
        ] {
            let cost = compute_realtime_cost(model, &one_minute_ish);
            assert!(
                cost > 0.0,
                "{model} has no realtime pricing row — voice turns would be recorded as free"
            );
        }
    }

    /// Published Gemini Live rates, per 1M tokens
    /// (<https://ai.google.dev/gemini-api/docs/pricing>):
    /// text in 0.75, text out 4.50, audio in 3.00, audio out 12.00. The 3.8
    /// Live pair is listed at the same rates as 3.1 Flash Live; adding them
    /// must not touch the 3.1 row.
    #[test]
    fn gemini_flash_live_matches_published_rates() {
        let one_m_each = rt(1_000_000, 0, 1_000_000, 1_000_000, 1_000_000);
        for model in [
            "gemini-3.1-flash-live-preview",
            "gemini-3.8-live",
            "gemini-3.8-live-extended-thinking",
        ] {
            let cost = compute_realtime_cost(model, &one_m_each);
            assert!(
                (cost - (0.75 + 4.50 + 3.00 + 12.00)).abs() < 1e-9,
                "{model}: expected 20.25, got {cost}"
            );
        }
    }

    /// The 3.8 rows resolve by their own prefix, so a later rate change for
    /// one family cannot silently reprice the other.
    #[test]
    fn gemini_live_families_resolve_to_distinct_rows() {
        let table = active_table();
        let now = now_unix_ms();
        let flash = table
            .realtime_pricing_version_at("gemini-3.1-flash-live-preview", now)
            .expect("3.1 row");
        let live = table
            .realtime_pricing_version_at("gemini-3.8-live", now)
            .expect("3.8 row");
        let thinking = table
            .realtime_pricing_version_at("gemini-3.8-live-extended-thinking", now)
            .expect("3.8 extended-thinking row");
        assert_ne!(flash, live, "3.8 Live must not borrow the 3.1 row");
        assert_eq!(live, thinking, "both 3.8 ids share the gemini-3.8-live row");
    }

    #[test]
    fn realtime_is_a_member_of_the_pricing_table() {
        // Unification: an explicit table built from a text-only row still resolves
        // realtime pricing (the builtin realtime rows travel with every table), so
        // realtime lives IN the PricingTable rather than a standalone static.
        let table = PricingTable::with_pricing_rows(vec![PricingRow::new(
            LLMProviderKind::OpenAI,
            "gpt-4o",
            0,
            ProviderPricing::openai(2.5, 10.0),
        )]);
        // Resolves off the same table, at the effective-from-0 boundary (inclusive).
        let p = table
            .realtime_pricing_at("gpt-realtime-2.1-mini", 0)
            .expect("realtime pricing should resolve from the unified table");
        assert_eq!(p.audio_input_per_m, 10.0);
        // Table method computes the audio math: 1M audio in + 1M audio out = $30 on mini.
        let u = rt(1_000_000, 0, 1_000_000, 0, 0);
        assert!(
            (table.compute_realtime_cost_at("gpt-realtime-2.1-mini", &u, 0) - 30.0).abs() < 1e-6
        );
    }

    #[test]
    fn compute_realtime_cost_at_matches_wall_clock_helper() {
        let u = rt(1_000_000, 0, 1_000_000, 500_000, 500_000);
        assert_eq!(
            compute_realtime_cost("gpt-realtime-2.1", &u),
            compute_realtime_cost_at("gpt-realtime-2.1", &u, now_unix_ms()),
        );
    }

    #[test]
    fn unknown_model_yields_zero_cost() {
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::Anthropic, "some-future-unknown-model", &u);
        assert_eq!(c, 0.0);
    }

    #[test]
    fn ollama_is_free() {
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost(&LLMProviderKind::Ollama, "llama3", &u);
        assert_eq!(c, 0.0);
    }

    #[test]
    fn longest_prefix_wins_over_generic_fallback() {
        // Both "claude-sonnet-4-6" and "claude-" match; the longer prefix should win.
        // If the fallback won, a full dated variant would still hit sonnet_4_6 pricing.
        let u = usage(1_000_000, 0, 0, 0);
        let c = compute_cost(
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6-20260101",
            &u,
        );
        assert!(
            (c - 3.0).abs() < 1e-6,
            "expected $3.0 (sonnet-4-6), got ${}",
            c
        );
    }

    #[test]
    fn compute_cost_at_zero_includes_builtin_base_rows() {
        // Builtin rows are effective from 0, so at_ms = 0 is the earliest
        // instant they apply (boundary is inclusive).
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost_at(&LLMProviderKind::Anthropic, "claude-sonnet-4-6", &u, 0);
        assert!((c - 18.0).abs() < 1e-6, "expected $18, got ${}", c);
    }

    // The effective-date tests below exercise explicit tables via
    // `compute_cost_with_at` rather than the global `install_pricing_table`
    // seam: the process-wide OnceLock races with parallel tests that read
    // through `active_table()`, so global installation stays untested here
    // and the merge shape is covered via `PricingTable::builtin_with`.

    #[test]
    fn longest_prefix_beats_newer_shorter_prefix() {
        // A newer, shorter-prefix row must not shadow an older, longer-prefix
        // row: the date filter selects candidates, then prefix length wins.
        let table = PricingTable::with_pricing_rows(vec![
            anthropic_row("claude-sonnet-4-6", 0, 3.0, 15.0),
            anthropic_row("claude-", 1_000, 99.0, 99.0),
        ]);
        let u = usage(1_000_000, 0, 0, 0);
        let c = compute_cost_with_at(
            &table,
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6-20260101",
            &u,
            2_000,
        );
        assert!(
            (c - 3.0).abs() < 1e-6,
            "expected $3.0 (longer prefix), got ${}",
            c
        );
    }

    #[test]
    fn equal_prefix_latest_effective_date_wins() {
        let table = PricingTable::with_pricing_rows(vec![
            anthropic_row("claude-sonnet-4-6", 0, 3.0, 15.0),
            anthropic_row("claude-sonnet-4-6", 1_000, 6.0, 30.0),
        ]);
        let u = usage(1_000_000, 0, 0, 0);
        let c = compute_cost_with_at(
            &table,
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &u,
            2_000,
        );
        assert!(
            (c - 6.0).abs() < 1e-6,
            "expected $6.0 (newer row), got ${}",
            c
        );
    }

    #[test]
    fn boundary_at_exact_effective_from_is_included() {
        // at_ms == effective_from_ms → the row applies (inclusive boundary).
        let table = PricingTable::with_pricing_rows(vec![
            anthropic_row("claude-sonnet-4-6", 0, 3.0, 15.0),
            anthropic_row("claude-sonnet-4-6", 1_000, 6.0, 30.0),
        ]);
        let u = usage(1_000_000, 0, 0, 0);
        let c = compute_cost_with_at(
            &table,
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &u,
            1_000,
        );
        assert!(
            (c - 6.0).abs() < 1e-6,
            "expected $6.0 at boundary, got ${}",
            c
        );
    }

    #[test]
    fn future_dated_row_is_invisible() {
        let table = PricingTable::with_pricing_rows(vec![
            anthropic_row("claude-sonnet-4-6", 0, 3.0, 15.0),
            anthropic_row("claude-sonnet-4-6", 1_000, 6.0, 30.0),
        ]);
        let u = usage(1_000_000, 0, 0, 0);
        let c = compute_cost_with_at(
            &table,
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &u,
            999,
        );
        assert!(
            (c - 3.0).abs() < 1e-6,
            "expected $3.0 (future row invisible), got ${}",
            c
        );
    }

    #[test]
    fn all_future_extra_rows_fall_back_to_builtin_base() {
        // Merged table where every extra row is future-dated: resolution at
        // an earlier at_ms must still hit the builtin base layer.
        let table = PricingTable::builtin_with(vec![
            anthropic_row("claude-sonnet-4-6", i64::MAX, 99.0, 99.0),
            anthropic_row("claude-", i64::MAX, 99.0, 99.0),
        ]);
        let u = usage(1_000_000, 1_000_000, 0, 0);
        let c = compute_cost_with_at(
            &table,
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &u,
            1,
        );
        assert!((c - 18.0).abs() < 1e-6, "expected builtin $18, got ${}", c);
    }

    #[test]
    fn install_merge_resolution_layers_extra_rows_over_builtin() {
        // Same table shape install_pricing_table sets: builtin base plus a
        // dated, owned-String override. Equal prefix length → the later
        // effective date wins once reached; before it, builtin still applies.
        let table = PricingTable::builtin_with(vec![anthropic_row(
            String::from("claude-sonnet-4-6"),
            1_000,
            6.0,
            30.0,
        )]);
        let u = usage(1_000_000, 0, 0, 0);
        let after = compute_cost_with_at(
            &table,
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &u,
            2_000,
        );
        assert!(
            (after - 6.0).abs() < 1e-6,
            "expected $6.0 (installed row), got ${}",
            after
        );
        let before = compute_cost_with_at(
            &table,
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &u,
            999,
        );
        assert!(
            (before - 3.0).abs() < 1e-6,
            "expected $3.0 (builtin base), got ${}",
            before
        );
    }

    #[test]
    fn pricing_version_identifies_the_exact_effective_rate_row() {
        let table = PricingTable::with_pricing_rows(vec![
            anthropic_row("claude-sonnet", 0, 3.0, 15.0),
            anthropic_row("claude-sonnet", 1_000, 6.0, 30.0),
        ]);
        let earlier = table
            .pricing_version_at(&LLMProviderKind::Anthropic, "claude-sonnet", 999)
            .expect("earlier pricing row");
        let later = table
            .pricing_version_at(&LLMProviderKind::Anthropic, "claude-sonnet", 1_000)
            .expect("later pricing row");
        assert!(earlier.starts_with("pricing-row-v1:"));
        assert_eq!(earlier.len(), "pricing-row-v1:".len() + 64);
        assert_ne!(earlier, later, "a rate change must change provenance");
        assert_eq!(
            later,
            table
                .pricing_version_at(&LLMProviderKind::Anthropic, "claude-sonnet-v2", 2_000)
                .expect("same resolved pricing row"),
            "model variants resolving to one row must share its identity"
        );
        assert_eq!(
            table.pricing_version_at(&LLMProviderKind::OpenAI, "claude-sonnet", 2_000),
            None
        );
    }

    #[test]
    fn realtime_pricing_version_changes_with_effective_rates() {
        let first = RealtimePricing {
            text_input_per_m: 1.0,
            text_cached_input_per_m: 0.1,
            text_output_per_m: 2.0,
            audio_input_per_m: 3.0,
            audio_cached_input_per_m: 0.2,
            audio_output_per_m: 4.0,
            per_second: 0.0,
        };
        let second = RealtimePricing {
            audio_output_per_m: 5.0,
            ..first
        };
        let table = PricingTable {
            rows: Vec::new(),
            realtime_rows: vec![
                RealtimePricingRow::new("voice", 0, first),
                RealtimePricingRow::new("voice", 1_000, second),
            ],
            web_search_rows: Vec::new(),
            media_rows: Vec::new(),
        };
        let earlier = table
            .realtime_pricing_version_at("voice-v1", 999)
            .expect("earlier realtime row");
        let later = table
            .realtime_pricing_version_at("voice-v1", 1_000)
            .expect("later realtime row");
        assert!(earlier.starts_with("pricing-row-v1:"));
        assert_ne!(earlier, later);
    }
    #[test]
    fn decision_model_pricing_is_effective_dated_provider_scoped_and_overridable() {
        let provider = LLMProviderKind::Custom("decision:typesafe".into());
        let launch = 1_789_430_400_000;
        let usage = TokenUsage {
            prompt_tokens: Some(1_000_000),
            completion_tokens: Some(2500),
            ..Default::default()
        };
        let table = PricingTable::builtin_with(vec![PricingRow::new(
            provider.clone(),
            "jev-1.13",
            launch + 86_400_000,
            ProviderPricing {
                input_per_m: 0.02,
                output_per_m: 0.0,
                cache_read_per_m: None,
                cache_write_per_m: None,
            },
        )]);
        assert!(table
            .lookup_at(&provider, "jev-1.13.0", launch - 1)
            .is_none());
        assert_eq!(
            compute_cost_with_at(&table, &provider, "jev-1.13.0", &usage, launch),
            0.042
        );
        assert_eq!(
            compute_cost_with_at(&table, &provider, "jev-1.13.0", &usage, launch + 86_400_000),
            0.02
        );
        assert_ne!(
            table.pricing_version_at(&provider, "jev-1.13.0", launch),
            table.pricing_version_at(&provider, "jev-1.13.0", launch + 86_400_000)
        );
        assert!(table
            .lookup_at(
                &LLMProviderKind::Custom("decision:systemone:typesafe".into()),
                "jev-1.13.0",
                launch
            )
            .is_none());
        assert!(table.lookup_at(&provider, "jev-2.0", launch).is_none());
        for adapter in ["laya-mlx", "laya-onnx", "kev-mlx", "kev-onnx"] {
            let local = LLMProviderKind::Custom(format!("decision:{adapter}"));
            assert_eq!(
                table.lookup_at(&local, "local", launch),
                Some(ProviderPricing::free())
            );
        }
    }
}


#[cfg(test)]
mod pricing_file_layering_tests {
    use super::*;

    #[test]
    fn the_seed_file_parses_and_carries_every_row_kind() {
        let seed = parse_pricing_file(SEED_PRICING_JSON).expect("seed pricing file must parse");
        assert!(!seed.text.is_empty());
        assert!(!seed.realtime.is_empty());
        assert!(!seed.web_search.is_empty());
    }

    /// Per-model prices belong in the pricing file. Code keeps one catch-all
    /// per provider and the free local engines; a model-specific row here is a
    /// price someone has to rebuild to change.
    #[test]
    fn code_defaults_hold_only_catch_alls_and_free_engines() {
        let defaults = PricingTable::defaults();
        for row in &defaults.rows {
            let free = row.pricing == ProviderPricing::free();
            let catch_all = matches!(&*row.model_prefix, "" | "claude-" | "deepseek-" | "grok-" | "gemini-");
            assert!(
                free || catch_all,
                "model-specific price in code: {} `{}` (move it to llm_pricing.template.json)",
                row.provider.as_str(),
                row.model_prefix
            );
        }
        assert!(defaults.realtime_rows.is_empty(), "realtime prices belong in the pricing file");
    }

    /// Every code default is repeated in the seed, so the file shows every
    /// price in force.
    #[test]
    fn the_seed_repeats_every_code_default() {
        let seed = parse_pricing_file(SEED_PRICING_JSON).unwrap();
        for row in &PricingTable::defaults().rows {
            assert!(
                seed.text.iter().any(|seed_row| text_pricing_row_version(seed_row) == text_pricing_row_version(row)),
                "code default missing from the seed: {} `{}`",
                row.provider.as_str(),
                row.model_prefix
            );
        }
        for row in &PricingTable::defaults().web_search_rows {
            assert!(seed.web_search.contains(row), "web-search default missing from the seed: {:?}", row.provider);
        }
    }

    #[test]
    fn a_deployment_row_beats_the_seed_on_a_full_tie() {
        let model = "claude-opus-5-5";
        let seed_rate = PricingTable::builtin()
            .lookup_at(&LLMProviderKind::Anthropic, model, 1_800_000_000_000)
            .expect("seed prices Opus 5.5");
        let file = parse_pricing_file(
            r#"{"schema_version": 2, "rates": [{"provider": "anthropic", "model_prefix": "claude-opus-5-5", "effective_from": "2026-09-22", "input_per_m": 1.0, "output_per_m": 2.0}]}"#,
        )
        .unwrap();
        let table = PricingTable::builtin_with_file(file);
        let rate = table
            .lookup_at(&LLMProviderKind::Anthropic, model, 1_800_000_000_000)
            .unwrap();
        assert_ne!(rate, seed_rate);
        assert_eq!((rate.input_per_m, rate.output_per_m), (1.0, 2.0));
    }

    #[test]
    fn web_search_rates_follow_the_file_by_date() {
        let file = parse_pricing_file(
            r#"{"schema_version": 2, "rates": [], "web_search_rates": [{"provider": "gemini", "effective_from": "2027-01-01", "usd_per_call": 0.014}]}"#,
        )
        .unwrap();
        let table = PricingTable::builtin_with_file(file);
        let jan_2027 = crate::pricing_file::parse_effective_from_ms("2027-01-01").unwrap();
        let oct_15 = crate::pricing_file::parse_effective_from_ms("2026-10-15").unwrap();
        assert_eq!(table.web_search_cost_per_call_at(&LLMProviderKind::Gemini, oct_15 - 1), 0.035);
        assert_eq!(table.web_search_cost_per_call_at(&LLMProviderKind::Gemini, jan_2027 - 1), 0.014);
        assert_eq!(table.web_search_cost_per_call_at(&LLMProviderKind::Gemini, jan_2027), 0.014);
        assert_eq!(table.web_search_cost_per_call_at(&LLMProviderKind::Ollama, jan_2027), 0.0);
    }

    #[test]
    fn media_calls_price_by_their_billed_units() {
        let file = parse_pricing_file(
            r#"{"schema_version": 2, "rates": [], "media_rates": [
                {"provider": "xai", "model_prefix": "grok-tts", "kind": "tts", "effective_from": "2026-01-01", "usd_per_million_chars": 15.0},
                {"provider": "xai", "model_prefix": "grok-voice-transcribe", "kind": "stt", "effective_from": "2026-01-01", "usd_per_audio_minute": 0.1},
                {"provider": "xai", "model_prefix": "grok-voice-transcribe", "kind": "stt_streaming", "effective_from": "2026-01-01", "usd_per_audio_minute": 0.2}
            ]}"#,
        )
        .unwrap();
        // Code defaults and these rows only: the seed has its own xAI rows.
        let mut table = PricingTable::defaults();
        table.layer(file);
        let at = crate::pricing_file::parse_effective_from_ms("2026-06-01").unwrap();
        let xai = LLMProviderKind::Xai;
        let tts = MediaUsage { input_chars: Some(2_000), requests: 1, ..MediaUsage::default() };
        let cost = table.compute_media_cost_at(&xai, "grok-tts", MediaKind::Tts, &tts, at).unwrap();
        assert!((cost - 0.03).abs() < 1e-12, "{cost}");
        let upload = MediaUsage { audio_seconds: Some(90.0), requests: 1, ..MediaUsage::default() };
        let stt = table
            .compute_media_cost_at(&xai, "grok-voice-transcribe-2.0", MediaKind::Stt, &upload, at)
            .unwrap();
        assert!((stt - 0.15).abs() < 1e-12, "{stt}");
        let streaming = table
            .compute_media_cost_at(&xai, "grok-voice-transcribe-2.0", MediaKind::SttStreaming, &upload, at)
            .unwrap();
        assert!((streaming - 0.30).abs() < 1e-12, "{streaming}");
        // A compressed upload with no measurable duration has no price, not $0.
        let unknown = MediaUsage { requests: 1, ..MediaUsage::default() };
        assert_eq!(table.compute_media_cost_at(&xai, "grok-voice-transcribe-2.0", MediaKind::Stt, &unknown, at), None);
        // Before the row, and for an unpriced model, there is no price either.
        assert_eq!(table.compute_media_cost_at(&xai, "grok-tts", MediaKind::Tts, &tts, 0), None);
        assert_eq!(table.media_pricing_version_at(&xai, "other-tts", MediaKind::Tts, at), None);
        assert!(table.media_pricing_version_at(&xai, "grok-tts", MediaKind::Tts, at).is_some());
    }

    /// Grok voice bills its session by the minute ($0.08), reported as the
    /// session's seconds; its token buckets price at $0.
    #[test]
    fn grok_voice_bills_session_minutes() {
        assert!(realtime_is_duration_billed_at("grok-voice-latest", 0));
        let minute = RealtimeUsage { billed_seconds: 60.0, ..RealtimeUsage::default() };
        let cost = PricingTable::builtin().compute_realtime_cost_at("grok-voice-latest", &minute, 0);
        assert!((cost - 0.08).abs() < 1e-9, "{cost}");
        let tokens_only = RealtimeUsage { audio_output_tokens: 10_000, ..RealtimeUsage::default() };
        assert_eq!(PricingTable::builtin().compute_realtime_cost_at("grok-voice-latest", &tokens_only, 0), 0.0);
    }

    #[test]
    fn local_voice_engines_price_at_an_exact_zero() {
        let table = PricingTable::builtin();
        let local = LLMProviderKind::Custom("local".into());
        let usage = MediaUsage { input_chars: Some(500), audio_seconds: Some(12.0), requests: 1 };
        for kind in [MediaKind::Tts, MediaKind::Stt, MediaKind::SttStreaming] {
            assert_eq!(table.compute_media_cost_at(&local, "FluidInference/kokoro-82m-coreml", kind, &usage, 0), Some(0.0));
            assert!(table.media_pricing_version_at(&local, "macos", kind, 0).is_some());
        }
    }

    #[test]
    fn media_rows_need_a_known_kind_and_a_rate() {
        for (body, needle) in [
            (r#"{"provider": "xai", "kind": "speech", "effective_from": "2026-01-01", "usd_per_request": 1.0}"#, "kind"),
            (r#"{"provider": "xai", "kind": "tts", "effective_from": "2026-01-01"}"#, "non-zero"),
        ] {
            let error = parse_pricing_file(&format!(r#"{{"schema_version": 2, "rates": [], "media_rates": [{body}]}}"#)).unwrap_err();
            assert!(error.contains(needle), "{error}");
        }
    }

    #[test]
    fn schema_one_files_cannot_carry_the_new_sections() {
        let error = parse_pricing_file(
            r#"{"schema_version": 1, "rates": [], "web_search_rates": [{"provider": "gemini", "effective_from": "2027-01-01", "usd_per_call": 0.014}]}"#,
        )
        .unwrap_err();
        assert!(error.contains("schema_version 2"), "{error}");
    }
}
