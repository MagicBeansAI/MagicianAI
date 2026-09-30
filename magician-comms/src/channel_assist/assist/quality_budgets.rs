//! Channel Assist quality budgets (plan workstream 3.1) — the product-lane
//! latency/quality budget constants, moved behind the assist seam from
//! `magician-api/src/channel_assist_api.rs` so the budgets live with the
//! product decisions they bound instead of inside one handler file.
//!
//! Values and the env override are unchanged; the API handlers now import
//! them from here (`magician_comms::channel_assist::assist::quality_budgets`).
//! These are SOFT budgets: an over-budget projection is still served, with an
//! INFO log and metric so a chronically-over-budget path is visible without
//! degrading the surface.

/// Default soft latency budget for the Today channel projection, in
/// milliseconds. Override: `CHANNEL_TODAY_PROJECTION_LATENCY_BUDGET_MS`
/// (positive values only; anything else falls back to this default).
pub const DEFAULT_TODAY_PROJECTION_LATENCY_BUDGET_MS: u64 = 250;

/// The effective Today channel projection latency budget for this process:
/// the env override when it parses to a positive `u64`, else the default.
pub fn today_projection_latency_budget_ms() -> u64 {
    std::env::var("CHANNEL_TODAY_PROJECTION_LATENCY_BUDGET_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TODAY_PROJECTION_LATENCY_BUDGET_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_is_the_phase8_value() {
        // The Phase 8 budget (mail-assist.md): 250ms soft budget for the
        // Today channel projection. Pinned here so the seam move cannot
        // silently re-budget the surface. (The env override path is
        // intentionally not exercised: env mutation is process-global and
        // racy under the threaded test harness.)
        assert_eq!(DEFAULT_TODAY_PROJECTION_LATENCY_BUDGET_MS, 250);
    }
}
