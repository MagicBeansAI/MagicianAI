//! Review-acceptance feedback + the utility metric (WEG eval depth — the 5th
//! plan metric).
//!
//! Utility = "how often generated reviews are accepted with light editing." It
//! needs an outside signal, so a consumer (the review UI, an agent, or a
//! one-off) records a [`ReviewFeedback`] per generated review via
//! `POST /evidence/review/feedback`; [`utility`] aggregates the ledger.
//!
//! The metric is deterministic + pure here; only the *intake* needs a caller.
//! Feedback is keyed by the review's durable-artifact name, and the latest
//! feedback per review wins (re-recording corrects, never double-counts).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// What the user did with a generated review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// Used as generated (no meaningful edits).
    Accepted,
    /// Used after editing — `edit_ratio` (when supplied) gauges how heavy.
    Edited,
    /// Thrown away / regenerated — not useful.
    Discarded,
}

/// One acceptance signal for a generated review (durable-artifact `name`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFeedback {
    /// The review's durable-artifact name (its id in the `evidence-reviews` ns).
    pub review: String,
    pub verdict: ReviewVerdict,
    /// Fraction of the review the user changed (0.0 = none … 1.0 = rewrote). The
    /// caller computes it; optional. Used to split `edited` into light vs heavy.
    #[serde(default)]
    pub edit_ratio: Option<f64>,
    /// RFC3339 stamp (server-set). Latest per review wins in [`utility`].
    #[serde(default)]
    pub recorded_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewFeedbackLedger {
    #[serde(default)]
    pub feedback: Vec<ReviewFeedback>,
}

/// Aggregate utility over a feedback ledger.
#[derive(Debug, Clone, Default, Serialize)]
pub struct UtilityReport {
    /// Distinct reviews with feedback (latest per review).
    pub total: usize,
    /// `accepted + light_edited` — the numerator of `utility_rate`.
    pub useful: usize,
    pub accepted: usize,
    pub light_edited: usize,
    pub heavy_edited: usize,
    pub discarded: usize,
    /// `useful / total` (0.0 when there's no feedback yet).
    pub utility_rate: f64,
    /// `light_edit_threshold` used to split `edited` into light vs heavy.
    pub light_edit_threshold: f64,
}

/// Utility = (accepted + lightly-edited) / distinct-reviews-with-feedback. An
/// `edited` verdict counts as light when `edit_ratio` is absent or ≤ threshold,
/// heavy otherwise. The latest feedback per review wins.
pub fn utility(feedback: &[ReviewFeedback], light_edit_threshold: f64) -> UtilityReport {
    // Latest feedback per review (by `recorded_at`; ties keep last seen).
    let mut latest: HashMap<&str, &ReviewFeedback> = HashMap::new();
    for fb in feedback {
        match latest.get(fb.review.as_str()) {
            Some(existing) if existing.recorded_at >= fb.recorded_at => {},
            _ => {
                latest.insert(fb.review.as_str(), fb);
            },
        }
    }

    let (mut accepted, mut light_edited, mut heavy_edited, mut discarded) = (0, 0, 0, 0);
    for fb in latest.values() {
        match fb.verdict {
            ReviewVerdict::Accepted => accepted += 1,
            ReviewVerdict::Edited => {
                if fb.edit_ratio.map_or(true, |r| r <= light_edit_threshold) {
                    light_edited += 1;
                } else {
                    heavy_edited += 1;
                }
            },
            ReviewVerdict::Discarded => discarded += 1,
        }
    }

    let total = latest.len();
    let useful = accepted + light_edited;
    let utility_rate = if total == 0 {
        0.0
    } else {
        useful as f64 / total as f64
    };
    UtilityReport {
        total,
        useful,
        accepted,
        light_edited,
        heavy_edited,
        discarded,
        utility_rate,
        light_edit_threshold,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn fb(
        review: &str,
        verdict: ReviewVerdict,
        edit_ratio: Option<f64>,
        at: &str,
    ) -> ReviewFeedback {
        ReviewFeedback {
            review: review.to_string(),
            verdict,
            edit_ratio,
            recorded_at: at.to_string(),
        }
    }

    #[test]
    fn accepted_and_light_edits_are_useful() {
        let ledger = [
            fb("r1", ReviewVerdict::Accepted, None, "2026-06-14T01:00:00Z"),
            fb(
                "r2",
                ReviewVerdict::Edited,
                Some(0.1),
                "2026-06-14T01:00:00Z",
            ),
            fb(
                "r3",
                ReviewVerdict::Edited,
                Some(0.8),
                "2026-06-14T01:00:00Z",
            ),
            fb("r4", ReviewVerdict::Discarded, None, "2026-06-14T01:00:00Z"),
        ];
        let u = utility(&ledger, 0.3);
        assert_eq!(u.total, 4);
        assert_eq!(u.accepted, 1);
        assert_eq!(u.light_edited, 1);
        assert_eq!(u.heavy_edited, 1);
        assert_eq!(u.discarded, 1);
        assert_eq!(u.useful, 2);
        assert!((u.utility_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn edited_without_ratio_counts_as_light() {
        let ledger = [fb(
            "r1",
            ReviewVerdict::Edited,
            None,
            "2026-06-14T01:00:00Z",
        )];
        let u = utility(&ledger, 0.3);
        assert_eq!(u.light_edited, 1);
        assert!((u.utility_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn latest_feedback_per_review_wins() {
        // r1 first discarded, then re-recorded as accepted → counts once, accepted.
        let ledger = [
            fb("r1", ReviewVerdict::Discarded, None, "2026-06-14T01:00:00Z"),
            fb("r1", ReviewVerdict::Accepted, None, "2026-06-14T02:00:00Z"),
        ];
        let u = utility(&ledger, 0.3);
        assert_eq!(u.total, 1, "deduped by review");
        assert_eq!(u.accepted, 1);
        assert_eq!(u.discarded, 0);
    }

    #[test]
    fn empty_ledger_is_zero_not_one() {
        let u = utility(&[], 0.3);
        assert_eq!(u.total, 0);
        assert_eq!(u.utility_rate, 0.0);
    }
}
