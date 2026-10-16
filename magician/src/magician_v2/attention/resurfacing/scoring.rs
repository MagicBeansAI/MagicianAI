//! Pure, category-free salience scoring for the Proactive Resurfacing Engine.
//!
//! [`score_item`] blends six generic signals into a single salience score plus
//! the auditable [`SalienceSignals`] bundle. It is deliberately **content- and
//! category-agnostic**: there are no hardcoded topics or event types
//! (no "birthday"/"anniversary"/"invoice" rules). Every signal is derived from
//! generic structure — timestamps, access counts, embedding-neighborhood
//! density, and any date-shaped token found in the text.
//!
//! External signals (embedding density + access stats) arrive through the small
//! [`ScoreCtx`] trait so the real scorer (Task 6) can back it with the vector
//! index / access ledger while unit tests mock it. A vector-index hiccup on the
//! centrality lookup degrades gracefully to a `0.0` contribution rather than
//! aborting the whole score (see [`score_item`]).
//!
//! Weights, half-lives, and feedback factors live as documented `const`s at the
//! top of the file that seed [`ResurfacingScoringConfig::default`]. The runtime
//! sources them from a [`ResurfacingScoringConfig`] (env-overridable via
//! [`ResurfacingScoringConfig::from_env`]) threaded into [`score_item`], so they
//! can be retuned without a rebuild. With the default config every signal and the
//! final blend are byte-identical to the compile-time constants.

use std::sync::LazyLock;

use regex::Regex;

use super::{
    centrality::{cosine, normalize},
    types::{CorpusItem, SalienceSignals},
};

// --- Tunable constants (config-overridable later) ---------------------------

/// Half-life (days) of the **recency** signal: an item `RECENCY_HALFLIFE_DAYS`
/// old scores `0.5`. ~2 weeks keeps the last fortnight strongly weighted while
/// letting older items fall off smoothly.
const RECENCY_HALFLIFE_DAYS: f32 = 14.0;

/// Half-life (days) of the **temporal-anchor** proximity: a date this many days
/// away from `now` (in either direction) scores `0.5`. ~10 days makes a date in
/// the coming/just-passed week or two salient and distant dates near-zero.
const TEMPORAL_HALFLIFE_DAYS: f32 = 10.0;

/// Half-life (days) for the per-pass **time-decay** sweep the scorer applies to
/// stored salience between passes (threaded into the store's decay). Mirrors the
/// prior worker default; the config is now the single source of truth for it.
const DECAY_HALFLIFE_DAYS: f64 = 14.0;

/// Sane floor for any env-overridden half-life (days) so the decay/recency/
/// temporal exponentials can never divide by zero.
const MIN_HALFLIFE_DAYS_F32: f32 = 1.0;
const MIN_HALFLIFE_DAYS_F64: f64 = 1.0;

/// Blend weights for the six signals. They need not sum to 1.0, but here they
/// do (making the final score fall in `[0,1]`). Each is documented with the
/// intent it encodes; tune independently.
///
/// Recency dominates — fresh things are the strongest resurfacing driver.
const W_RECENCY: f32 = 0.30;
/// Repeatedly-touched things matter, but less than freshness.
const W_FREQUENCY: f32 = 0.15;
/// Embedding centrality: items sitting in a dense cluster of other salient
/// context are more worth resurfacing.
const W_CENTRALITY: f32 = 0.20;
/// Co-salience proxy (frequency x centrality): rewards items that are both
/// clustered and repeatedly referenced.
const W_COOCCURRENCE: f32 = 0.10;
/// A date-shaped token near `now` is a strong, fully generic salience cue.
const W_TEMPORAL: f32 = 0.15;
/// Dormancy: things that were salient and have gone quiet deserve a nudge.
const W_DORMANCY: f32 = 0.10;

// --- Tunable scoring config -------------------------------------------------

/// Runtime-tunable scoring parameters for the resurfacing engine.
///
/// Every field defaults (via [`Default`]) to the compile-time `const` above, so
/// `ResurfacingScoringConfig::default()` reproduces today's behavior byte-for-
/// byte. [`from_env`](Self::from_env) overlays any `RESURFACING_*` env var that is
/// set + parseable (clamped to a sane range), letting the weights / half-lives /
/// feedback factors be retuned without a rebuild. The struct is threaded through
/// [`score_item`] and the scorer pass; it changes only WHERE the numbers come
/// from, never the signal MATH.
#[derive(Debug, Clone, PartialEq)]
pub struct ResurfacingScoringConfig {
    pub w_recency: f32,
    pub w_frequency: f32,
    pub w_centrality: f32,
    pub w_cooccurrence: f32,
    pub w_temporal: f32,
    pub w_dormancy: f32,
    pub recency_halflife_days: f32,
    pub temporal_halflife_days: f32,
    pub decay_halflife_days: f64,
    pub dismiss_penalty_threshold: f32,
    pub dismiss_penalty_factor: f32,
    pub affinity_threshold: f32,
    pub affinity_boost_factor: f32,
    pub utility_min_multiplier: f32,
    pub utility_max_multiplier: f32,
    pub utility_smoothing: f32,
}

impl Default for ResurfacingScoringConfig {
    /// Exactly the compile-time constants — the single source of default truth,
    /// so `Default` and the pre-config behavior can never drift apart.
    fn default() -> Self {
        Self {
            w_recency: W_RECENCY,
            w_frequency: W_FREQUENCY,
            w_centrality: W_CENTRALITY,
            w_cooccurrence: W_COOCCURRENCE,
            w_temporal: W_TEMPORAL,
            w_dormancy: W_DORMANCY,
            recency_halflife_days: RECENCY_HALFLIFE_DAYS,
            temporal_halflife_days: TEMPORAL_HALFLIFE_DAYS,
            decay_halflife_days: DECAY_HALFLIFE_DAYS,
            dismiss_penalty_threshold: DISMISS_PENALTY_THRESHOLD,
            dismiss_penalty_factor: DISMISS_PENALTY_FACTOR,
            affinity_threshold: AFFINITY_THRESHOLD,
            affinity_boost_factor: AFFINITY_BOOST_FACTOR,
            utility_min_multiplier: UTILITY_MIN_MULTIPLIER,
            utility_max_multiplier: UTILITY_MAX_MULTIPLIER,
            utility_smoothing: UTILITY_SMOOTHING,
        }
    }
}

impl ResurfacingScoringConfig {
    /// Build a config from [`Default`], overriding each field from its
    /// `RESURFACING_*` env var when set + parseable + finite. Out-of-range values
    /// are clamped (weights + feedback factors ≥ 0; half-lives > 0 with a sane
    /// floor; thresholds in `[0,1]`); unset / unparseable / non-finite values keep
    /// the default. Reading the env here makes the config the single source of
    /// truth (the worker no longer parses `RESURFACING_DECAY_HALFLIFE_DAYS`).
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            w_recency: env_weight("RESURFACING_W_RECENCY", d.w_recency),
            w_frequency: env_weight("RESURFACING_W_FREQUENCY", d.w_frequency),
            w_centrality: env_weight("RESURFACING_W_CENTRALITY", d.w_centrality),
            w_cooccurrence: env_weight("RESURFACING_W_COOCCURRENCE", d.w_cooccurrence),
            w_temporal: env_weight("RESURFACING_W_TEMPORAL", d.w_temporal),
            w_dormancy: env_weight("RESURFACING_W_DORMANCY", d.w_dormancy),
            recency_halflife_days: env_halflife_f32(
                "RESURFACING_RECENCY_HALFLIFE_DAYS",
                d.recency_halflife_days,
            ),
            temporal_halflife_days: env_halflife_f32(
                "RESURFACING_TEMPORAL_HALFLIFE_DAYS",
                d.temporal_halflife_days,
            ),
            decay_halflife_days: env_halflife_f64(
                "RESURFACING_DECAY_HALFLIFE_DAYS",
                d.decay_halflife_days,
            ),
            dismiss_penalty_threshold: env_unit(
                "RESURFACING_DISMISS_PENALTY_THRESHOLD",
                d.dismiss_penalty_threshold,
            ),
            // Direction-clamped: a dismissal PENALTY multiplier must be <= 1 (1.0 =
            // no penalty, 0 = full suppression). Parsing with `env_weight` (>= 0, no
            // ceiling) would let e.g. `=2` INVERT the penalty into a boost, so clamp
            // to `[0,1]` via `env_unit`.
            dismiss_penalty_factor: env_unit(
                "RESURFACING_DISMISS_PENALTY_FACTOR",
                d.dismiss_penalty_factor,
            ),
            affinity_threshold: env_unit("RESURFACING_AFFINITY_THRESHOLD", d.affinity_threshold),
            // Direction-clamped: an affinity BOOST multiplier must be >= 1 (1.0 = no
            // boost). Parsing with `env_weight` would let e.g. `=0.3` INVERT the boost
            // into a penalty, so clamp to `>= 1` via `env_min_one`.
            affinity_boost_factor: env_min_one(
                "RESURFACING_AFFINITY_BOOST_FACTOR",
                d.affinity_boost_factor,
            ),
            utility_min_multiplier: env_unit(
                "RESURFACING_UTILITY_MIN_MULTIPLIER",
                d.utility_min_multiplier,
            ),
            utility_max_multiplier: env_min_one(
                "RESURFACING_UTILITY_MAX_MULTIPLIER",
                d.utility_max_multiplier,
            ),
            utility_smoothing: env_positive("RESURFACING_UTILITY_SMOOTHING", d.utility_smoothing),
        }
    }
}

/// Parse a finite `f32` env var and clamp to `>= 0` (weights + feedback factors);
/// unset / unparseable / non-finite -> `default`.
fn env_weight(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|n| n.is_finite())
        .map(|n| n.max(0.0))
        .unwrap_or(default)
}

/// Parse a finite `f32` env var and clamp to `[0,1]` (thresholds); unset /
/// unparseable / non-finite -> `default`.
fn env_unit(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|n| n.is_finite())
        .map(|n| n.clamp(0.0, 1.0))
        .unwrap_or(default)
}

/// Parse a finite `f32` env var and clamp to `>= 1.0`; unset / unparseable /
/// non-finite -> `default`. Used for the utility MAX multiplier, which is an
/// up-weight and must never drop below the neutral `1.0` (a lane you engage with
/// can only be lifted, never sunk, by the max bound).
fn env_min_one(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|n| n.is_finite())
        .map(|n| n.max(1.0))
        .unwrap_or(default)
}

/// Parse a strictly-positive finite `f32` env var; unset / unparseable /
/// non-finite / non-positive -> `default`. Used for the Laplace smoothing alpha,
/// which must be `> 0` so the smoothed rate stays defined.
fn env_positive(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(default)
}

/// Parse a strictly-positive finite `f32` half-life env var, clamped up to the
/// sane floor; unset / unparseable / non-finite / non-positive -> `default`.
fn env_halflife_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n.max(MIN_HALFLIFE_DAYS_F32))
        .unwrap_or(default)
}

/// Parse a strictly-positive finite `f64` half-life env var, clamped up to the
/// sane floor; unset / unparseable / non-finite / non-positive -> `default`.
fn env_halflife_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n.max(MIN_HALFLIFE_DAYS_F64))
        .unwrap_or(default)
}

// --- External signal source -------------------------------------------------

/// External signal source for scoring. The real impl (Task 6) queries the
/// embedding/vector index + access stats; tests mock it.
///
/// `Send + Sync` so a `&dyn ScoreCtx` stays `Send` across the `.await` points in
/// `run_scorer_pass` — the scorer runs inside a `tokio::spawn`ed worker loop.
pub trait ScoreCtx: Send + Sync {
    /// Current wall-clock time, unix seconds.
    fn now(&self) -> i64;
    /// Embedding-neighborhood density in `[0,1]` ("how much other salient
    /// context clusters around this"). Fallible — a vector-index hiccup must
    /// NOT abort scoring; [`score_item`] maps `Err` to a `0.0` centrality
    /// contribution rather than propagating.
    fn neighbor_density(&self, embedding_text: &str) -> anyhow::Result<f32>;
    /// How often this `source_ref` has been touched/referenced.
    fn access_count(&self, source_ref: &str) -> u32;
}

// --- Public entry point -----------------------------------------------------

/// Score one corpus item into `(salience_score, signals)`. Every signal field
/// of the returned [`SalienceSignals`] is populated (each normalized to
/// `[0,1]`) so the score is auditable after the fact, and the returned score is
/// always finite.
pub fn score_item(
    item: &CorpusItem,
    ctx: &dyn ScoreCtx,
    cfg: &ResurfacingScoringConfig,
) -> (f32, SalienceSignals) {
    let (score, signals, _) = score_item_with_temporal_anchor(item, ctx, cfg);
    (score, signals)
}

/// Score an item and materialize its closest source-supported temporal anchor.
/// The anchor uses epoch milliseconds; date-only values are UTC date markers,
/// not inferred reminder times.
pub fn score_item_with_temporal_anchor(
    item: &CorpusItem,
    ctx: &dyn ScoreCtx,
    cfg: &ResurfacingScoringConfig,
) -> (f32, SalienceSignals, Option<i64>) {
    let now = ctx.now();

    let recency = recency_signal(now, item.occurred_at, cfg.recency_halflife_days);
    let frequency = frequency_signal(ctx.access_count(&item.source_ref));

    // Graceful degradation: a vector-index error contributes 0.0 centrality
    // rather than aborting the whole score (never panic / propagate).
    let centrality = match ctx.neighbor_density(&item.embedding_text) {
        Ok(density) => clamp01(density),
        Err(_) => 0.0,
    };

    // Generic co-salience proxy: an item is "co-salient" when it is BOTH
    // embedding-central (clustered with other context) AND frequently
    // referenced. Product of the two normalized signals — no category logic.
    let cooccurrence = clamp01(centrality * frequency);

    let (temporal_anchor, temporal_anchor_at) =
        temporal_anchor_for_item(now, item, cfg.temporal_halflife_days);

    // "Was salient, has gone quiet": high when historically touched (frequency)
    // but not touched recently (low recency).
    let dormancy = clamp01(frequency * (1.0 - recency));

    let signals = SalienceSignals {
        recency,
        frequency,
        centrality,
        cooccurrence,
        temporal_anchor,
        dormancy,
        source_affinity: 0.0,
    };

    let score = cfg.w_recency * recency
        + cfg.w_frequency * frequency
        + cfg.w_centrality * centrality
        + cfg.w_cooccurrence * cooccurrence
        + cfg.w_temporal * temporal_anchor
        + cfg.w_dormancy * dormancy;

    // Weights and signals are all finite by construction, but guard anyway so a
    // downstream sort/store never sees a NaN.
    let score = if score.is_finite() { score } else { 0.0 };

    (score, signals, temporal_anchor_at)
}

// --- Individual signal helpers ----------------------------------------------

/// Exponential recency decay: `0.5^(age_days / halflife_days)`. A future
/// timestamp (age < 0) clamps to age = 0 -> `1.0`. `halflife_days` is sourced
/// from [`ResurfacingScoringConfig`] (default [`RECENCY_HALFLIFE_DAYS`]).
fn recency_signal(now: i64, occurred_at: i64, halflife_days: f32) -> f32 {
    let age_secs = (now - occurred_at).max(0) as f32;
    let age_days = age_secs / 86_400.0;
    clamp01(0.5_f32.powf(age_days / halflife_days))
}

/// Saturating frequency from an access count: `1 - 1/(1 + count)`. Monotonic in
/// `count`, `0` accesses -> `0`, asymptotes to `1`.
fn frequency_signal(access_count: u32) -> f32 {
    clamp01(1.0 - 1.0 / (1.0 + access_count as f32))
}

static TEMPORAL_ISO_DATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b").expect("valid ISO date pattern")
});
static TEMPORAL_SLASH_DATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d{1,2})/(\d{1,2})/(\d{4})\b").expect("valid slash date pattern")
});
static TEMPORAL_MONTH_FIRST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s+(\d{1,2})(?:st|nd|rd|th)?,?\s+(\d{4})\b",
    )
    .expect("valid month-first date pattern")
});
static TEMPORAL_DAY_FIRST_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(\d{1,2})(?:st|nd|rd|th)?\s+(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?,?\s+(\d{4})\b",
    )
    .expect("valid day-first date pattern")
});

/// Parse only source-supported absolute dates. Date-only values are encoded as
/// UTC-midnight markers so they can be sorted/scored, not as inferred reminder
/// times. Callers that create a reminder must still ask for an exact time.
///
/// Moved lib-side with the scoring engine (plan workstream 3.0) so the
/// temporal-anchor signal has no comms dependency; `magician-comms`
/// re-exports it from its `channel_assist::types` shim for existing callers.
pub fn supported_temporal_markers_ms(text: &str) -> Vec<i64> {
    let mut markers = Vec::new();

    for token in text.split_whitespace() {
        let token = token.trim_matches(|ch: char| matches!(ch, ',' | ';' | '(' | ')'));
        if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(token) {
            push_temporal_marker(&mut markers, timestamp.timestamp_millis());
        }
    }

    for captures in TEMPORAL_ISO_DATE_PATTERN.captures_iter(text) {
        push_date_capture(
            &mut markers,
            captures[1].parse().ok(),
            captures[2].parse().ok(),
            captures[3].parse().ok(),
        );
    }
    for captures in TEMPORAL_SLASH_DATE_PATTERN.captures_iter(text) {
        let first = captures[1].parse::<u32>().ok();
        let second = captures[2].parse::<u32>().ok();
        let year = captures[3].parse::<i32>().ok();
        if let (Some(first), Some(second), Some(year)) = (first, second, year) {
            let date = chrono::NaiveDate::from_ymd_opt(year, second, first)
                .or_else(|| chrono::NaiveDate::from_ymd_opt(year, first, second));
            if let Some(date) = date {
                push_date_marker(&mut markers, date);
            }
        }
    }
    for captures in TEMPORAL_MONTH_FIRST_PATTERN.captures_iter(text) {
        push_date_capture(
            &mut markers,
            captures[3].parse().ok(),
            temporal_month_number(&captures[1]),
            captures[2].parse().ok(),
        );
    }
    for captures in TEMPORAL_DAY_FIRST_PATTERN.captures_iter(text) {
        push_date_capture(
            &mut markers,
            captures[3].parse().ok(),
            temporal_month_number(&captures[2]),
            captures[1].parse().ok(),
        );
    }
    markers
}

fn push_date_capture(
    markers: &mut Vec<i64>,
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
) {
    if let (Some(year), Some(month), Some(day)) = (year, month, day) {
        if let Some(date) = chrono::NaiveDate::from_ymd_opt(year, month, day) {
            push_date_marker(markers, date);
        }
    }
}

fn push_date_marker(markers: &mut Vec<i64>, date: chrono::NaiveDate) {
    if let Some(midnight) = date.and_hms_opt(0, 0, 0) {
        push_temporal_marker(markers, midnight.and_utc().timestamp_millis());
    }
}

fn push_temporal_marker(markers: &mut Vec<i64>, marker: i64) {
    if !markers.contains(&marker) {
        markers.push(marker);
    }
}

fn temporal_month_number(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

fn temporal_anchor_for_item(now: i64, item: &CorpusItem, halflife_days: f32) -> (f32, Option<i64>) {
    let now_ms = now.saturating_mul(1000);
    let mut structured: Vec<i64> = Vec::new();
    if let Some(details) = item.content_details.as_ref() {
        for fact in &details.temporal_facts {
            if let Some(at_ms) = fact.at_ms {
                push_unique_anchor(&mut structured, at_ms);
            } else {
                for at_ms in supported_temporal_markers_ms(&fact.text) {
                    push_unique_anchor(&mut structured, at_ms);
                }
            }
        }
        if structured.is_empty() {
            for change in &details.changes {
                if let Some(effective_text) = change.effective_text.as_deref() {
                    for at_ms in supported_temporal_markers_ms(effective_text) {
                        push_unique_anchor(&mut structured, at_ms);
                    }
                }
            }
        }
    }

    let anchors = if structured.is_empty() {
        let mut text = String::with_capacity(item.title.len() + item.digest.len() + 1);
        text.push_str(&item.title);
        text.push(' ');
        text.push_str(&item.digest);
        supported_temporal_markers_ms(&text)
    } else {
        structured
    };

    let Some(at_ms) = anchors
        .into_iter()
        .min_by_key(|candidate: &i64| (*candidate).saturating_sub(now_ms).unsigned_abs())
    else {
        return (0.0, None);
    };
    let distance_days = at_ms.saturating_sub(now_ms).unsigned_abs() as f32 / 86_400_000.0;
    let proximity = clamp01(0.5_f32.powf(distance_days / halflife_days));
    (proximity, Some(at_ms))
}

#[cfg(any(test, feature = "test-fixtures"))]
fn parse_dates(text: &str) -> Vec<chrono::NaiveDate> {
    supported_temporal_markers_ms(text)
        .into_iter()
        .filter_map(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|timestamp| timestamp.date_naive())
        .collect()
}

fn push_unique_anchor(anchors: &mut Vec<i64>, at_ms: i64) {
    if !anchors.contains(&at_ms) {
        anchors.push(at_ms);
    }
}

/// Clamp to `[0,1]`, mapping any non-finite input to `0.0`.
fn clamp01(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

// --- P3a cross-session dismissal penalty ------------------------------------

/// Cosine similarity at/above which a NEW candidate is treated as resembling a
/// PAST dismissal and penalized. Mirrors the store's in-session
/// `SUPPRESS_THRESHOLD` (0.82) but slightly looser (0.80): the durable signal
/// must catch re-phrased recurrences of a dismissed theme across sessions, not
/// only the near-duplicates in-session suppression handles. Tunable; a later
/// config layer can override it.
pub const DISMISS_PENALTY_THRESHOLD: f32 = 0.80;

/// Multiplicative salience penalty for a candidate resembling a past dismissal
/// (`salience_score *= DISMISS_PENALTY_FACTOR`). 0.4 is a strong-but-not-
/// silencing cut: a genuinely dominant item can still surface, but "you keep
/// dismissing this kind of thing" pushes it well down the curator's top-`cap`
/// pick. Applied as a POST-score multiplier (a feedback OVERRIDE), never folded
/// into `score_item`'s intrinsic signal bundle. Tunable; config-overridable
/// later.
pub const DISMISS_PENALTY_FACTOR: f32 = 0.4;

/// Pure dismissal-penalty multiplier for a candidate's salience given the scope's
/// past-dismissed vectors.
///
/// Returns `factor` when the item resembles ANY past dismissal (max cosine
/// `>= threshold`) and `1.0` otherwise — including an empty `dismissed_norm` set
/// (nothing dismissed yet ⇒ no penalty) or a degenerate/empty `item_vec` (cosine
/// against a length-mismatched vector is `0.0`, so no penalty; graceful).
///
/// `dismissed_norm` must ALREADY be L2-normalized — the scorer normalizes the
/// scope's dismissed set ONCE per pass and reuses it across every item.
/// `item_vec` is the item's raw embedding and is normalized here.
///
/// This is deliberately a feedback OVERRIDE applied on top of the intrinsic
/// score rather than a `score_item` signal: the auditable [`SalienceSignals`]
/// stay a pure function of the item's OWN content, while cross-session dismissal
/// learning modulates only the final salience the curator ranks on.
pub fn dismissal_penalty(
    item_vec: &[f32],
    dismissed_norm: &[Vec<f32>],
    threshold: f32,
    factor: f32,
) -> f32 {
    if dismissed_norm.is_empty() {
        return 1.0;
    }
    let item = normalize(item_vec.to_vec());
    let mut best = 0.0_f32;
    for d in dismissed_norm {
        let sim = cosine(&item, d);
        if sim > best {
            best = sim;
        }
    }
    if best >= threshold {
        factor
    } else {
        1.0
    }
}

// --- P4a cross-session affinity boost ---------------------------------------

/// Cosine similarity at/above which a NEW candidate is treated as resembling a
/// PAST positive action (open/acknowledge) and BOOSTED. The positive mirror of
/// [`DISMISS_PENALTY_THRESHOLD`], set to the same 0.80: the durable affinity
/// signal must catch re-phrased recurrences of an engaged-with theme across
/// sessions, not only in-session near-duplicates. Tunable; a later config layer
/// can override it.
pub const AFFINITY_THRESHOLD: f32 = 0.80;

/// Multiplicative salience BOOST for a candidate resembling a past positive
/// action (`salience_score *= AFFINITY_BOOST_FACTOR`). 1.5 is a strong nudge:
/// "you keep engaging with this kind of thing" lifts a resembling item ~50% up
/// the curator's top-`cap` pick without dominating it outright. The positive
/// mirror of [`DISMISS_PENALTY_FACTOR`] (0.4): applied as a POST-score
/// multiplier (a feedback OVERRIDE), never folded into `score_item`'s intrinsic
/// signal bundle. Tunable; config-overridable later.
pub const AFFINITY_BOOST_FACTOR: f32 = 1.5;

/// Pure affinity-boost multiplier for a candidate's salience given the scope's
/// past positive-action vectors. The positive mirror of [`dismissal_penalty`].
///
/// Returns `factor` (> 1) when the item resembles ANY past positive action (max
/// cosine `>= threshold`) and `1.0` otherwise — including an empty
/// `affinity_norm` set (nothing engaged with yet ⇒ no boost) or a degenerate/
/// empty `item_vec` (cosine against a length-mismatched vector is `0.0`, so no
/// boost; graceful).
///
/// `affinity_norm` must ALREADY be L2-normalized — the scorer normalizes the
/// scope's affinity set ONCE per pass and reuses it across every item.
/// `item_vec` is the item's raw embedding and is normalized here.
///
/// Like the dismissal penalty this is deliberately a feedback OVERRIDE applied on
/// top of the intrinsic score rather than a `score_item` signal: the auditable
/// [`SalienceSignals`] stay a pure function of the item's OWN content, while
/// cross-session affinity learning modulates only the final salience the curator
/// ranks on.
pub fn affinity_boost(
    item_vec: &[f32],
    affinity_norm: &[Vec<f32>],
    threshold: f32,
    factor: f32,
) -> f32 {
    if affinity_norm.is_empty() {
        return 1.0;
    }
    let item = normalize(item_vec.to_vec());
    let mut best = 0.0_f32;
    for a in affinity_norm {
        let sim = cosine(&item, a);
        if sim > best {
            best = sim;
        }
    }
    if best >= threshold {
        factor
    } else {
        1.0
    }
}

// --- P4c per-lane utility signal --------------------------------------------

/// Floor of the per-lane utility multiplier: a lane whose surfaced cards the
/// owner keeps DISMISSING is down-weighted at score time, but never below `0.5`
/// (a persistently-ignored lane goes quieter, not silent — a genuinely dominant
/// item can still surface). Seeds [`ResurfacingScoringConfig::utility_min_multiplier`].
const UTILITY_MIN_MULTIPLIER: f32 = 0.5;

/// Ceiling of the per-lane utility multiplier: a lane whose surfaced cards the
/// owner keeps ENGAGING with (open/acknowledge) is up-weighted at score time, up
/// to `1.5×`. The positive mirror of [`UTILITY_MIN_MULTIPLIER`]. Seeds
/// [`ResurfacingScoringConfig::utility_max_multiplier`].
const UTILITY_MAX_MULTIPLIER: f32 = 1.5;

/// Laplace smoothing alpha for the per-lane engagement RATE. `1.0` means a
/// cold-start lane (0 positive, 0 negative) reads as a neutral `0.5` rate ⇒ a
/// `1.0×` (no-op) multiplier, so a brand-new/unengaged lane is unaffected until
/// real feedback accumulates. Seeds [`ResurfacingScoringConfig::utility_smoothing`].
const UTILITY_SMOOTHING: f32 = 1.0;

/// Laplace-smoothed per-lane engagement rate in `(0, 1)`:
/// `(positive + alpha) / (positive + negative + 2*alpha)`. A cold-start lane
/// `(0, 0)` reads as exactly `0.5` for any `alpha > 0`; an all-positive lane
/// trends toward `1`, an all-negative lane toward `0`. `alpha` is floored at the
/// smallest positive `f32` so a mis-configured `0` can't divide by zero.
pub fn utility_rate(positive: u64, negative: u64, smoothing: f32) -> f32 {
    let alpha = if smoothing.is_finite() && smoothing > 0.0 {
        smoothing
    } else {
        f32::MIN_POSITIVE
    };
    let p = positive as f32;
    let n = negative as f32;
    (p + alpha) / (p + n + 2.0 * alpha)
}

/// Pure per-lane utility multiplier (P4c): converts a lane's engagement tally
/// into a multiplicative salience re-weight applied at score time to EVERY item
/// in that lane (it is lane-based and embedding-independent, unlike the
/// dismissal/affinity signals).
///
/// The [`utility_rate`] `r ∈ (0, 1)` is mapped piecewise-linearly around the
/// neutral pivot `r = 0.5 → 1.0`:
///
/// * `r >= 0.5`: `1.0 + (max - 1.0) * (2r - 1)` — rises from `1.0` at `r = 0.5`
///   to `max` at `r = 1.0`.
/// * `r <  0.5`: `1.0 - (1.0 - min) * (1 - 2r)` — falls from `1.0` at `r = 0.5`
///   to `min` at `r = 0.0`.
///
/// The result is clamped to `[min, max]`. A cold-start lane `(0, 0)` has
/// `r = 0.5` exactly, so the multiplier is **exactly `1.0`** — a new/unengaged
/// lane is left untouched. This makes [`ResurfacingScoringConfig::default`] fully
/// neutral: with no engagement recorded anywhere, every lane multiplies by `1.0`
/// and scoring is byte-identical to the pre-P4c behavior.
pub fn utility_multiplier(positive: u64, negative: u64, cfg: &ResurfacingScoringConfig) -> f32 {
    let rate = utility_rate(positive, negative, cfg.utility_smoothing);
    let min = cfg.utility_min_multiplier;
    let max = cfg.utility_max_multiplier;
    let mult = if rate >= 0.5 {
        1.0 + (max - 1.0) * (2.0 * rate - 1.0)
    } else {
        1.0 - (1.0 - min) * (1.0 - 2.0 * rate)
    };
    // Guard against a mis-ordered (min > max) config so `clamp` can't panic; a
    // well-formed config always has min <= 1.0 <= max.
    if min <= max {
        mult.clamp(min, max)
    } else {
        mult
    }
}

/// Similarity at which a NEW candidate is treated as another instance of a
/// series the owner already dismissed, and is never created at all.
///
/// Well above [`DISMISS_PENALTY_THRESHOLD`] (0.80) on purpose. That threshold
/// governs *down-weighting* something merely similar to a past dismissal; this
/// one governs *not existing*, so it must mean "this is the same thing again",
/// not "this is related". At 0.97 a recurring generated digest — same sender,
/// same template, a new date — clears it while a genuinely different message
/// from the same sender does not.
pub const DISMISSED_SERIES_THRESHOLD: f32 = 0.97;

/// Whether a new item is another instance of an already-dismissed series.
///
/// A per-item dismissal penalty cannot stop a recurring digest: tomorrow's copy
/// is a brand-new candidate that scores on its own freshness — near-maximal
/// recency and a temporal anchor on today's date — and surfaces before any
/// penalty from yesterday's copy has been re-scored onto it. So one dismissal
/// never sticks, and the owner has to dismiss the same worthless mail every
/// single day.
///
/// Suppressing at creation time makes one dismissal end the series, and it
/// generalizes: it keys on what the owner actually rejected rather than on a
/// list of phrasings someone has to keep extending. Only dismissals that
/// penalize similar items write these signals, so a "done" dismissal — "I
/// handled this one" — never suppresses anything.
pub fn matches_dismissed_series(
    item_vec: &[f32],
    dismissed_norm: &[Vec<f32>],
    threshold: f32,
) -> bool {
    if dismissed_norm.is_empty() || item_vec.is_empty() {
        return false;
    }
    let item = normalize(item_vec.to_vec());
    dismissed_norm
        .iter()
        .any(|dismissed| cosine(&item, dismissed) >= threshold)
}

/// Per-item form of [`utility_multiplier`]: a lane's positive history only
/// boosts an item to the extent that the item is itself connected to current
/// context.
///
/// The flat lane multiplier is what let one lane dominate the surface. A lane
/// the owner engages with heavily pins at the ceiling, and that ceiling is then
/// applied to *every* item in the lane regardless of whether the particular item
/// has anything to do with what is going on now — so a stored preference can
/// outrank unread mail purely because its lane is popular. Worse, it is
/// self-reinforcing: the boost surfaces more of the lane, which accrues more
/// positives, which raises the boost.
///
/// Scaling the boost by the item's own relevance keeps the lane signal (a
/// well-liked lane still wins ties) while requiring each item to earn the
/// ranking it gets. The penalty side is deliberately NOT scaled: a lane the
/// owner keeps dismissing should go quieter whether or not a given item looks
/// topical.
pub fn utility_multiplier_for_item(
    positive: u64,
    negative: u64,
    relevance: f32,
    cfg: &ResurfacingScoringConfig,
) -> f32 {
    let lane = utility_multiplier(positive, negative, cfg);
    if lane <= 1.0 {
        return lane;
    }
    let relevance = if relevance.is_finite() {
        clamp01(relevance)
    } else {
        0.0
    };
    1.0 + (lane - 1.0) * relevance
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::attention::resurfacing::types::{
        ResurfacingContentDetails, ResurfacingDetailStatus, ResurfacingTemporalFact, SourceKind,
    };
    use std::sync::Mutex;

    /// The process environment is global; serialize the `from_env` config test so
    /// its set/remove_var calls can't interleave across the harness's test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Every `RESURFACING_*` scoring var — cleared before/after the env test so a
    /// stray value from another run (or this one) can't leak.
    const CFG_ENV_VARS: &[&str] = &[
        "RESURFACING_W_RECENCY",
        "RESURFACING_W_FREQUENCY",
        "RESURFACING_W_CENTRALITY",
        "RESURFACING_W_COOCCURRENCE",
        "RESURFACING_W_TEMPORAL",
        "RESURFACING_W_DORMANCY",
        "RESURFACING_RECENCY_HALFLIFE_DAYS",
        "RESURFACING_TEMPORAL_HALFLIFE_DAYS",
        "RESURFACING_DECAY_HALFLIFE_DAYS",
        "RESURFACING_DISMISS_PENALTY_THRESHOLD",
        "RESURFACING_DISMISS_PENALTY_FACTOR",
        "RESURFACING_AFFINITY_THRESHOLD",
        "RESURFACING_AFFINITY_BOOST_FACTOR",
        "RESURFACING_UTILITY_MIN_MULTIPLIER",
        "RESURFACING_UTILITY_MAX_MULTIPLIER",
        "RESURFACING_UTILITY_SMOOTHING",
    ];

    /// The default scoring config — every existing test scores with this so the
    /// numbers stay byte-identical to the pre-config behavior.
    fn cfg() -> ResurfacingScoringConfig {
        ResurfacingScoringConfig::default()
    }

    /// A mock [`ScoreCtx`]: fixed `now`, an optional configured density result
    /// (default `Ok(0.0)`), and a flat access count.
    struct MockCtx {
        now: i64,
        density: Option<anyhow::Result<f32>>,
        access: u32,
    }

    impl MockCtx {
        fn at(now: i64) -> Self {
            Self {
                now,
                density: None,
                access: 0,
            }
        }

        fn with_density(mut self, d: f32) -> Self {
            self.density = Some(Ok(d));
            self
        }

        fn with_density_err(mut self) -> Self {
            self.density = Some(Err(anyhow::anyhow!("mock vector index error")));
            self
        }

        fn with_access(mut self, count: u32) -> Self {
            self.access = count;
            self
        }
    }

    impl ScoreCtx for MockCtx {
        fn now(&self) -> i64 {
            self.now
        }

        fn neighbor_density(&self, _embedding_text: &str) -> anyhow::Result<f32> {
            // anyhow::Error isn't Clone, so reconstruct the configured result.
            match &self.density {
                Some(Ok(v)) => Ok(*v),
                Some(Err(e)) => Err(anyhow::anyhow!("{e}")),
                None => Ok(0.0),
            }
        }

        fn access_count(&self, _source_ref: &str) -> u32 {
            self.access
        }
    }

    /// Build a `CorpusItem` at a fixed `occurred_at`, with a title/digest that
    /// contain NO date token (so `temporal_anchor` stays 0 and other signals
    /// decide these tests).
    fn item_at(occurred_at: i64) -> CorpusItem {
        CorpusItem {
            source_kind: SourceKind::Memory,
            source_ref: "memory#note-1".to_string(),
            title: "a note about a topic".to_string(),
            digest: "some short details here".to_string(),
            content_details: None,
            content_revision: None,
            occurred_at,
            watermark_cursor: occurred_at,
            embedding_text: "topic context vector text".to_string(),
        }
    }

    /// Default item anchored at `occurred_at = 0`.
    fn item() -> CorpusItem {
        item_at(0)
    }

    #[test]
    fn recent_item_scores_higher_than_old() {
        let ctx = MockCtx::at(1_000_000);
        let recent = item_at(1_000_000 - 3600);
        let old = item_at(1_000_000 - 3600 * 24 * 90);
        assert!(score_item(&recent, &ctx, &cfg()).0 > score_item(&old, &ctx, &cfg()).0);
    }

    #[test]
    fn central_item_scores_higher() {
        // Both ctxs share now=0 and the item's occurred_at=0, so recency and
        // all other signals are equal; only centrality differs.
        let dense = MockCtx::at(0).with_density(0.9);
        let sparse = MockCtx::at(0).with_density(0.1);
        assert!(score_item(&item(), &dense, &cfg()).0 > score_item(&item(), &sparse, &cfg()).0);
    }

    #[test]
    fn failed_neighbor_query_contributes_zero_not_panic() {
        let ctx = MockCtx::at(0).with_density_err();
        let (s, sig) = score_item(&item(), &ctx, &cfg());
        assert_eq!(sig.centrality, 0.0);
        assert!(s.is_finite());
    }

    #[test]
    fn frequency_saturates_and_zero_access_is_zero() {
        assert_eq!(frequency_signal(0), 0.0);
        assert!(frequency_signal(1) > 0.0);
        assert!(frequency_signal(100) > frequency_signal(1));
        assert!(frequency_signal(u32::MAX) <= 1.0);
    }

    #[test]
    fn future_timestamp_clamps_recency_to_one() {
        // occurred_at in the future -> age clamps to 0 -> recency 1.0.
        let ctx = MockCtx::at(1_000);
        let future = item_at(2_000);
        let (_score, sig) = score_item(&future, &ctx, &cfg());
        assert_eq!(sig.recency, 1.0);
    }

    #[test]
    fn dormancy_high_when_frequently_touched_but_stale() {
        // High access count + very old occurred_at -> low recency, high
        // frequency -> dormancy should be substantial.
        let ctx = MockCtx::at(1_000_000_000).with_access(50);
        let stale = item_at(0); // ~decades old
        let (_score, sig) = score_item(&stale, &ctx, &cfg());
        assert!(
            sig.recency < 0.01,
            "recency should be near zero for a stale item"
        );
        assert!(
            sig.dormancy > 0.5,
            "dormancy = frequency*(1-recency) should be high"
        );
    }

    #[test]
    fn temporal_anchor_high_near_now_low_when_absent_or_far() {
        // now = 2021-01-01 (unix 1_609_459_200).
        let now = 1_609_459_200;

        // A date the next day should score high.
        let near = CorpusItem {
            source_kind: SourceKind::Memory,
            source_ref: "r".to_string(),
            title: "meeting on 2021-01-02".to_string(),
            digest: String::new(),
            content_details: None,
            content_revision: None,
            occurred_at: now,
            watermark_cursor: now,
            embedding_text: String::new(),
        };
        let (_s, near_sig) = score_item(&near, &MockCtx::at(now), &cfg());
        assert!(
            near_sig.temporal_anchor > 0.8,
            "date ~1 day away should be salient"
        );

        // A far-off date should score low.
        let far = CorpusItem {
            title: "deadline 2025-01-01".to_string(),
            ..near.clone()
        };
        let (_s, far_sig) = score_item(&far, &MockCtx::at(now), &cfg());
        assert!(
            far_sig.temporal_anchor < 0.05,
            "date years away should be near zero"
        );

        // No date at all -> 0.0.
        let none = CorpusItem {
            title: "just a plain note".to_string(),
            ..near.clone()
        };
        let (_s, none_sig) = score_item(&none, &MockCtx::at(now), &cfg());
        assert_eq!(none_sig.temporal_anchor, 0.0);
    }

    #[test]
    fn structured_temporal_fact_materializes_and_outranks_display_text_fallback() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 7, 10)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let mut structured = item_at(now);
        structured.title = "Legacy title mentions 2030-01-01".to_string();
        structured.content_details = Some(ResurfacingContentDetails {
            schema_version: 2,
            key_facts: Vec::new(),
            changes: Vec::new(),
            temporal_facts: vec![ResurfacingTemporalFact {
                kind: "due".to_string(),
                text: "July 12, 2026".to_string(),
                at_ms: None,
                timezone: None,
            }],
            detail_status: ResurfacingDetailStatus::Complete,
            missing_details: Vec::new(),
        });

        let (_score, signals, anchor) =
            score_item_with_temporal_anchor(&structured, &MockCtx::at(now), &cfg());
        let anchor_date = chrono::DateTime::from_timestamp_millis(anchor.unwrap())
            .unwrap()
            .date_naive();
        assert_eq!(
            anchor_date,
            chrono::NaiveDate::from_ymd_opt(2026, 7, 12).unwrap()
        );
        assert!(signals.temporal_anchor > 0.8);
    }

    #[test]
    fn parse_dates_handles_generic_formats() {
        // ISO, month-name, and day-first month-name are all recognized.
        let dates = parse_dates("iso 2021-03-15, worded March 20, 2021 and 5 Apr 2021");
        assert!(dates.contains(&chrono::NaiveDate::from_ymd_opt(2021, 3, 15).unwrap()));
        assert!(dates.contains(&chrono::NaiveDate::from_ymd_opt(2021, 3, 20).unwrap()));
        assert!(dates.contains(&chrono::NaiveDate::from_ymd_opt(2021, 4, 5).unwrap()));
    }

    #[test]
    fn all_signals_populated_and_in_unit_range() {
        let ctx = MockCtx::at(1_000_000).with_density(0.5).with_access(10);
        let (score, sig) = score_item(&item_at(999_000), &ctx, &cfg());
        for v in [
            sig.recency,
            sig.frequency,
            sig.centrality,
            sig.cooccurrence,
            sig.temporal_anchor,
            sig.dormancy,
        ] {
            assert!((0.0..=1.0).contains(&v), "signal {v} out of [0,1]");
        }
        assert!(score.is_finite());
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn dismissal_penalty_hits_near_misses_far_and_empty() {
        // `[1,0]` is already unit-length, so it doubles as its own normalized
        // form for the pre-normalized `dismissed_norm` set.
        let dismissed = vec![vec![1.0_f32, 0.0]];

        // Parallel to a past dismissal (cosine 1.0 >= 0.80) -> penalized.
        assert_eq!(dismissal_penalty(&[2.0, 0.0], &dismissed, 0.80, 0.4), 0.4);

        // Orthogonal to every dismissal (cosine 0.0 < 0.80) -> no penalty.
        assert_eq!(dismissal_penalty(&[0.0, 3.0], &dismissed, 0.80, 0.4), 1.0);

        // Empty dismissed set -> no penalty regardless of the item.
        assert_eq!(dismissal_penalty(&[1.0, 0.0], &[], 0.80, 0.4), 1.0);

        // Degenerate item (length mismatch -> cosine 0.0) -> graceful, no penalty.
        assert_eq!(dismissal_penalty(&[], &dismissed, 0.80, 0.4), 1.0);
    }

    #[test]
    fn affinity_boost_hits_near_misses_far_and_empty() {
        // `[1,0]` is already unit-length, so it doubles as its own normalized
        // form for the pre-normalized `affinity_norm` set.
        let affinity = vec![vec![1.0_f32, 0.0]];

        // Parallel to a past positive action (cosine 1.0 >= 0.80) -> boosted.
        assert_eq!(affinity_boost(&[2.0, 0.0], &affinity, 0.80, 1.5), 1.5);

        // Orthogonal to every positive action (cosine 0.0 < 0.80) -> no boost.
        assert_eq!(affinity_boost(&[0.0, 3.0], &affinity, 0.80, 1.5), 1.0);

        // Empty affinity set -> no boost regardless of the item.
        assert_eq!(affinity_boost(&[1.0, 0.0], &[], 0.80, 1.5), 1.0);

        // Degenerate item (length mismatch -> cosine 0.0) -> graceful, no boost.
        assert_eq!(affinity_boost(&[], &affinity, 0.80, 1.5), 1.0);
    }

    // --- P4b config-tunable scoring ----------------------------------------

    #[test]
    fn scoring_config_from_env_overrides_and_defaults() {
        let _g = ENV_LOCK.lock().unwrap();

        // Clean slate: with every var unset, `from_env` == `Default`.
        for name in CFG_ENV_VARS {
            std::env::remove_var(name);
        }
        assert_eq!(
            ResurfacingScoringConfig::from_env(),
            ResurfacingScoringConfig::default(),
            "unset env -> defaults",
        );

        // Valid overrides are picked up; out-of-range values clamp; unparseable
        // values fall back to the default field.
        std::env::set_var("RESURFACING_W_RECENCY", "0.9"); // picked up verbatim
        std::env::set_var("RESURFACING_DISMISS_PENALTY_FACTOR", "0.1"); // picked up
        std::env::set_var("RESURFACING_W_FREQUENCY", "-2.0"); // clamps to 0.0 (>= 0)
        std::env::set_var("RESURFACING_DISMISS_PENALTY_THRESHOLD", "1.5"); // clamps to 1.0
        std::env::set_var("RESURFACING_DECAY_HALFLIFE_DAYS", "0.5"); // clamps up to floor 1.0
        std::env::set_var("RESURFACING_AFFINITY_THRESHOLD", "notanumber"); // invalid -> default
        std::env::set_var("RESURFACING_UTILITY_MIN_MULTIPLIER", "0.2"); // picked up verbatim
        std::env::set_var("RESURFACING_UTILITY_MAX_MULTIPLIER", "0.5"); // clamps up to 1.0
        std::env::set_var("RESURFACING_UTILITY_SMOOTHING", "0"); // non-positive -> default

        let cfg = ResurfacingScoringConfig::from_env();
        let d = ResurfacingScoringConfig::default();
        assert_eq!(cfg.w_recency, 0.9, "valid weight picked up");
        assert_eq!(cfg.dismiss_penalty_factor, 0.1, "valid factor picked up");
        assert_eq!(cfg.w_frequency, 0.0, "negative weight clamps to 0");
        assert_eq!(
            cfg.dismiss_penalty_threshold, 1.0,
            "threshold clamps into [0,1]"
        );
        assert_eq!(
            cfg.utility_min_multiplier, 0.2,
            "valid utility min picked up"
        );
        assert_eq!(
            cfg.utility_max_multiplier, 1.0,
            "utility max clamps up to the neutral floor 1.0",
        );
        assert_eq!(
            cfg.utility_smoothing, d.utility_smoothing,
            "non-positive smoothing falls back to the default",
        );
        assert!(
            (cfg.decay_halflife_days - 1.0).abs() < f64::EPSILON,
            "sub-floor half-life clamps up to the floor",
        );
        assert_eq!(
            cfg.affinity_threshold, d.affinity_threshold,
            "unparseable value falls back to the default",
        );
        // Untouched fields stay at their defaults.
        assert_eq!(cfg.w_centrality, d.w_centrality);
        assert_eq!(cfg.affinity_boost_factor, d.affinity_boost_factor);

        // Direction-clamped feedback factors: the dismissal PENALTY factor must stay
        // <= 1 (a value > 1 would invert the penalty into a boost) and the affinity
        // BOOST factor must stay >= 1 (a value < 1 would invert the boost into a
        // penalty). Set out-of-direction values and re-read from the env.
        std::env::set_var("RESURFACING_DISMISS_PENALTY_FACTOR", "2"); // penalty > 1 clamps to 1.0
        std::env::set_var("RESURFACING_AFFINITY_BOOST_FACTOR", "0.3"); // boost < 1 clamps to 1.0
        let clamped = ResurfacingScoringConfig::from_env();
        assert_eq!(
            clamped.dismiss_penalty_factor, 1.0,
            "a dismissal PENALTY factor > 1 clamps down to the no-penalty ceiling 1.0",
        );
        assert_eq!(
            clamped.affinity_boost_factor, 1.0,
            "an affinity BOOST factor < 1 clamps up to the no-boost floor 1.0",
        );

        for name in CFG_ENV_VARS {
            std::env::remove_var(name);
        }
    }

    #[test]
    fn score_item_default_config_matches_prior_weights() {
        // recency 1.0 (occurred_at == now), frequency 1 - 1/(1+3) = 0.75,
        // centrality 0.5, cooccurrence 0.5*0.75 = 0.375, temporal 0 (date-free),
        // dormancy 0.75*(1-1) = 0. With the documented default weights:
        //   0.30*1 + 0.15*0.75 + 0.20*0.5 + 0.10*0.375 + 0.15*0 + 0.10*0 = 0.55
        let ctx = MockCtx::at(1_000).with_density(0.5).with_access(3);
        let (score, _sig) = score_item(&item_at(1_000), &ctx, &ResurfacingScoringConfig::default());
        assert!(
            (score - 0.55).abs() < 1e-5,
            "default-config score must match the documented weights: got {score}",
        );
    }

    #[test]
    fn score_item_respects_custom_weights() {
        // Recency-only weighting: every other weight is zeroed, so the score is
        // exactly `w_recency * recency` regardless of the (deliberately strong)
        // centrality/frequency signals — proving the config drives the blend.
        let cfg = ResurfacingScoringConfig {
            w_recency: 1.0,
            w_frequency: 0.0,
            w_centrality: 0.0,
            w_cooccurrence: 0.0,
            w_temporal: 0.0,
            w_dormancy: 0.0,
            ..Default::default()
        };
        let ctx = MockCtx::at(1_000).with_density(0.9).with_access(50);

        // A fresh item (recency 1.0) scores exactly 1.0 under recency-only weights.
        let (recent, _) = score_item(&item_at(1_000), &ctx, &cfg);
        assert!(
            (recent - 1.0).abs() < 1e-6,
            "recency-only fresh item -> 1.0: {recent}"
        );

        // A one-half-life-old item (14 days) scores exactly recency 0.5.
        let old_at = 1_000 - 86_400 * 14;
        let (old, _) = score_item(&item_at(old_at), &ctx, &cfg);
        assert!(
            (old - 0.5).abs() < 1e-3,
            "recency-only 14d-old item -> 0.5: {old}"
        );
        assert!(recent > old, "the score must track recency alone");
    }

    // --- P4c per-lane utility signal ---------------------------------------

    /// One dismissal has to end a recurring series, or the owner dismisses the
    /// same generated digest every day forever.
    #[test]
    fn dismissed_series_matches_a_repeat_but_not_a_merely_similar_item() {
        let dismissed = vec![normalize(vec![1.0, 0.0, 0.0])];

        // The same thing again (a new date on the same template).
        let repeat = normalize(vec![0.999, 0.045, 0.0]);
        assert!(matches_dismissed_series(
            &repeat,
            &dismissed,
            DISMISSED_SERIES_THRESHOLD
        ));

        // Related but not the same — this is the dismissal PENALTY's job, not
        // suppression's. Suppression must mean "identical again".
        let related = normalize(vec![0.9, 0.44, 0.0]);
        assert!(!matches_dismissed_series(
            &related,
            &dismissed,
            DISMISSED_SERIES_THRESHOLD
        ));
        // ...and that same item is still within penalty range, so it is
        // down-weighted rather than silently discarded.
        assert!(
            cosine(&related, &dismissed[0]) >= DISMISS_PENALTY_THRESHOLD,
            "the merely-similar case must still reach the penalty"
        );

        // Nothing dismissed, or no embedding: never suppress.
        assert!(!matches_dismissed_series(
            &repeat,
            &[],
            DISMISSED_SERIES_THRESHOLD
        ));
        assert!(!matches_dismissed_series(
            &[],
            &dismissed,
            DISMISSED_SERIES_THRESHOLD
        ));
    }

    #[test]
    fn utility_multiplier_neutral_at_coldstart_and_moves_with_engagement() {
        let cfg = ResurfacingScoringConfig::default();

        // Cold-start (no engagement) -> exactly neutral 1.0, so a brand-new lane
        // is left untouched (Default stays byte-identical to pre-P4c behavior).
        assert_eq!(utility_multiplier(0, 0, &cfg), 1.0);

        // Balanced engagement (equal positives/negatives) -> still ~neutral.
        let balanced = utility_multiplier(50, 50, &cfg);
        assert!(
            (balanced - 1.0).abs() < 1e-6,
            "balanced lane ~1.0: {balanced}"
        );

        // A heavily-positive lane climbs toward the max multiplier (but never
        // above it).
        let high = utility_multiplier(100, 0, &cfg);
        assert!(high > 1.0 && high <= cfg.utility_max_multiplier);
        assert!(
            (high - cfg.utility_max_multiplier).abs() < 0.05,
            "many positives approach the max: {high}",
        );

        // A heavily-negative lane sinks toward the min multiplier (but never
        // below it).
        let low = utility_multiplier(0, 100, &cfg);
        assert!(low < 1.0 && low >= cfg.utility_min_multiplier);
        assert!(
            (low - cfg.utility_min_multiplier).abs() < 0.05,
            "many negatives approach the min: {low}",
        );

        // Monotonic + strictly clamped at the extremes.
        assert!(high > low, "an engaged lane must outweigh a dismissed lane");
        assert!(utility_multiplier(1_000_000, 0, &cfg) <= cfg.utility_max_multiplier);
        assert!(utility_multiplier(0, 1_000_000, &cfg) >= cfg.utility_min_multiplier);
    }
}
