//! "Autofix sweep" helpers for the CTO detect-and-fix loop.
//!
//! The sweep reads the anomaly queue and, for the top open + uncooled anomaly
//! per scope, dispatches a coding task that drafts a MINIMAL fix as a review
//! proposal (never auto-applied). These are the *pure* helpers — the periodic
//! loop + task dispatch live in `api/web_api.rs`.
//!
//! **Default ON (opt-out)**: the sweep runs unless disabled via the typed
//! config field `harness.autofix_enabled: false`. The env var
//! `MAGICIAN_HARNESS_AUTOFIX` is an OVERRIDE — `0/false/no/off` disables and
//! `1/true/yes/on` enables, regardless of config; anything else defers to the
//! config value. This module exposes only the env override; the config default
//! is applied by the caller in `api/web_api.rs`.

use chrono::{DateTime, Duration, Utc};

use super::anomaly::{AnomalyStatus, HarnessAnomaly};

/// Cadence of the autofix sweep loop (15 minutes).
pub const AUTOFIX_TICK_INTERVAL_SECS: u64 = 15 * 60;

/// Rate limit: at most one fix dispatched per scope per sweep.
pub const AUTOFIX_MAX_PER_SWEEP: usize = 1;

/// Per-anomaly cooldown after a fix is dispatched, so a still-open anomaly is
/// not re-dispatched every sweep while its fix proposal is being reviewed.
pub fn autofix_cooldown() -> Duration {
    Duration::hours(6)
}

/// Env override for the autofix sweep: `Some(true)` when `MAGICIAN_HARNESS_AUTOFIX`
/// is `1`/`true`/`yes`/`on`, `Some(false)` when `0`/`false`/`no`/`off`
/// (case-insensitive), and `None` when unset or unrecognized. The caller applies
/// the typed config default (`harness.autofix_enabled`, default true) when this
/// returns `None`.
pub fn autofix_env_override() -> Option<bool> {
    let v = std::env::var("MAGICIAN_HARNESS_AUTOFIX").ok()?;
    let v = v.trim();
    if v == "1"
        || v.eq_ignore_ascii_case("true")
        || v.eq_ignore_ascii_case("yes")
        || v.eq_ignore_ascii_case("on")
    {
        Some(true)
    } else if v == "0"
        || v.eq_ignore_ascii_case("false")
        || v.eq_ignore_ascii_case("no")
        || v.eq_ignore_ascii_case("off")
    {
        Some(false)
    } else {
        None
    }
}

/// Select the anomalies eligible for an autofix dispatch this sweep, capped at
/// `cap`. An anomaly is eligible when it is `Open` and either has never had a
/// fix dispatched or its last dispatch is older than `cooldown`. Preserves the
/// input order (callers pass a queue already sorted open-first, newest-first).
pub fn select_autofix_targets<'a>(
    anoms: &'a [HarnessAnomaly],
    now: DateTime<Utc>,
    cooldown: Duration,
    cap: usize,
) -> Vec<&'a HarnessAnomaly> {
    anoms
        .iter()
        .filter(|a| a.status == AnomalyStatus::Open)
        .filter(|a| a.last_fix_dispatch_at.map_or(true, |t| now - t >= cooldown))
        .take(cap)
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn autofix_env_override_parses_or_none() {
        std::env::remove_var("MAGICIAN_HARNESS_AUTOFIX");
        assert_eq!(autofix_env_override(), None);
        std::env::set_var("MAGICIAN_HARNESS_AUTOFIX", "1");
        assert_eq!(autofix_env_override(), Some(true));
        std::env::set_var("MAGICIAN_HARNESS_AUTOFIX", "off");
        assert_eq!(autofix_env_override(), Some(false));
        std::env::set_var("MAGICIAN_HARNESS_AUTOFIX", "banana");
        assert_eq!(autofix_env_override(), None);
        std::env::remove_var("MAGICIAN_HARNESS_AUTOFIX");
    }

    #[test]
    fn select_targets_open_uncooled_capped() {
        let now = Utc::now();
        let mk = |sig: &str, status: AnomalyStatus, last: Option<DateTime<Utc>>| {
            let mut a = HarnessAnomaly::new(
                "p",
                "w",
                "cto",
                "g",
                super::super::anomaly::AnomalyKind::CycleFailed,
                "s",
                "d",
            );
            a.signature = sig.into();
            a.status = status;
            a.last_fix_dispatch_at = last;
            a
        };
        let anoms = vec![
            mk("open_fresh", AnomalyStatus::Open, None),
            mk(
                "open_cooled",
                AnomalyStatus::Open,
                Some(now - Duration::hours(7)),
            ), // > cooldown → eligible
            mk(
                "open_recent",
                AnomalyStatus::Open,
                Some(now - Duration::minutes(30)),
            ), // < cooldown → skip
            mk("resolved", AnomalyStatus::Resolved, None),
            mk("dispatched", AnomalyStatus::FixDispatched, None),
        ];
        let picked = select_autofix_targets(&anoms, now, Duration::hours(6), 10);
        let sigs: Vec<&str> = picked.iter().map(|a| a.signature.as_str()).collect();
        assert!(sigs.contains(&"open_fresh") && sigs.contains(&"open_cooled"));
        assert!(
            !sigs.contains(&"open_recent")
                && !sigs.contains(&"resolved")
                && !sigs.contains(&"dispatched")
        );
        assert_eq!(
            select_autofix_targets(&anoms, now, Duration::hours(6), 1).len(),
            1
        );
    }
}
