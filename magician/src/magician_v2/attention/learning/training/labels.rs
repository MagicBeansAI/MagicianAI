//! Actionability labels from explicit owner acts only.
//!
//! The vocabulary is the same one serving and the frozen eval fixture already
//! use: `action_completed` is positive; dismiss / not-actionable / not-owner
//! are negative; usefulness, acknowledge, duplicates, timing, and "I already
//! handled this" never become a class. Historical outcomes without a served
//! decision are counted, but they are not usable for propensity-weighted fit.

use super::super::AttentionOutcomeKind;

const DONE_REASONS: &[&str] = &["already_handled", "done", "completed", "handled"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelClass {
    Positive,
    Negative,
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelExample<'a> {
    pub outcome: AttentionOutcomeKind,
    pub reason: Option<&'a str>,
    pub decision_id: Option<&'a str>,
    /// True when the trainer joined a serve-time feature vector. Usable means
    /// joinable, not merely "a decision_id string is present".
    pub has_features: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelSet {
    pub positive: usize,
    pub negative: usize,
    pub excluded: usize,
    /// Explicit acts joined to a serve-time feature vector.
    pub usable: usize,
    /// Explicit acts with no joinable feature vector.
    pub unlinked: usize,
}

impl LabelSet {
    pub fn from_outcomes(rows: &[LabelExample<'_>]) -> Self {
        let mut set = Self::default();
        for row in rows {
            match classify_label(row.outcome, row.reason) {
                LabelClass::Positive => {
                    set.positive += 1;
                    count_link(&mut set, row.has_features);
                },
                LabelClass::Negative => {
                    set.negative += 1;
                    count_link(&mut set, row.has_features);
                },
                LabelClass::Excluded => set.excluded += 1,
            }
        }
        set
    }

    pub fn labelled(&self) -> usize {
        self.positive.saturating_add(self.negative)
    }
}

pub fn classify_label(outcome: AttentionOutcomeKind, reason: Option<&str>) -> LabelClass {
    if is_done_dismissal(outcome, reason) {
        return LabelClass::Excluded;
    }
    match outcome.actionability_target() {
        Some(true) => LabelClass::Positive,
        Some(false) => LabelClass::Negative,
        None => LabelClass::Excluded,
    }
}

pub fn is_done_dismissal(outcome: AttentionOutcomeKind, reason: Option<&str>) -> bool {
    if outcome == AttentionOutcomeKind::Obsolete {
        return true;
    }
    if matches!(
        outcome,
        AttentionOutcomeKind::ActionCompleted | AttentionOutcomeKind::Useful
    ) {
        return false;
    }
    reason
        .map(str::trim)
        .is_some_and(|value| DONE_REASONS.contains(&value))
}

pub const fn positive_outcomes() -> &'static [&'static str] {
    &["action_completed"]
}

pub const fn negative_outcomes() -> &'static [&'static str] {
    &["irrelevant", "not_actionable", "not_owner"]
}

pub const fn excluded_outcomes() -> &'static [&'static str] {
    &[
        "useful",
        "obsolete",
        "duplicate_of",
        "neutral_seen",
        "timing_negative",
        "no_interaction",
    ]
}

fn count_link(set: &mut LabelSet, has_features: bool) {
    if has_features {
        set.usable += 1;
    } else {
        set.unlinked += 1;
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn outcome(kind: AttentionOutcomeKind, reason: Option<&str>) -> LabelExample<'_> {
        LabelExample {
            outcome: kind,
            reason,
            decision_id: None,
            has_features: false,
        }
    }

    fn outcome_with_decision<'a>(
        kind: AttentionOutcomeKind,
        decision_id: Option<&'a str>,
    ) -> LabelExample<'a> {
        LabelExample {
            outcome: kind,
            reason: None,
            decision_id,
            has_features: decision_id.is_some_and(|value| !value.trim().is_empty()),
        }
    }

    #[test]
    fn only_explicit_acts_become_labels() {
        let rows = vec![
            // Actionability positives are completed acts, not "this was useful".
            outcome(AttentionOutcomeKind::ActionCompleted, None),
            outcome(AttentionOutcomeKind::Irrelevant, None),
            // Acknowledge is "seen, no action needed" and deliberately logs no
            // learning signal; counting it as either class contradicts the surface
            // it came from.
            outcome(AttentionOutcomeKind::NeutralSeen, None),
            // Usefulness is a different task. Folding it in would train the
            // actionability head on "I liked knowing this".
            outcome(AttentionOutcomeKind::Useful, None),
        ];
        let set = LabelSet::from_outcomes(&rows);
        assert_eq!(set.positive, 1);
        assert_eq!(set.negative, 1);
        assert_eq!(set.excluded, 2);
    }

    #[test]
    fn a_done_dismissal_is_excluded_not_negative() {
        // "I handled this" is not "show me fewer like this".
        let set = LabelSet::from_outcomes(&[outcome(
            AttentionOutcomeKind::Irrelevant,
            Some("already_handled"),
        )]);
        assert_eq!(set.negative, 0);
        assert_eq!(set.excluded, 1);

        let obsolete = LabelSet::from_outcomes(&[outcome(AttentionOutcomeKind::Obsolete, None)]);
        assert_eq!(obsolete.negative, 0);
        assert_eq!(obsolete.excluded, 1);
    }

    #[test]
    fn labels_without_joinable_features_are_counted_separately() {
        let set = LabelSet::from_outcomes(&[
            outcome_with_decision(AttentionOutcomeKind::ActionCompleted, Some("decision-1")),
            outcome_with_decision(AttentionOutcomeKind::ActionCompleted, None),
        ]);
        assert_eq!(set.usable, 1);
        assert_eq!(set.unlinked, 1);
    }

    #[test]
    fn a_completed_act_is_not_excluded_by_a_done_reason() {
        let set = LabelSet::from_outcomes(&[outcome(
            AttentionOutcomeKind::ActionCompleted,
            Some("completed"),
        )]);
        assert_eq!(set.positive, 1);
        assert_eq!(set.excluded, 0);
    }
}
