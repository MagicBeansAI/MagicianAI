//! Candidates the owner decides on — plan phase 3.
//!
//! §4: *"Each is a proposal with its sample size and its confounders visible.
//! **A proposal that cannot state its N does not get made.**"*
//!
//! # What this is not
//!
//! It does not apply anything. §8's first control is that *"it never applies
//! anything; every change is an owner editorial decision"*, so the output here
//! is a [`Candidate`] — a thing to look at — and there is no function that acts
//! on one. That absence is the control.
//!
//! It also does not score, rank or recommend. It states what the cohorts
//! contain and what differs, and refuses to state anything it cannot support.
//! A loop that ranked its own candidates would be optimising a metric nobody
//! chose, which §5 forbids.
//!
//! # Generic
//!
//! A cohort comparison is *"did the population that got version B behave
//! differently from the population that got version A"*. Nothing about that is
//! fundraising, email, or OPC. The inputs are observations; the output is a
//! description.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{OutcomeLabel, OutcomeObservation};

/// The smallest sample that may carry a proposal at all.
///
/// Not a statistical threshold — it is deliberately not dressed as one, because
/// at these sizes no threshold is honest. It is a floor beneath which a
/// difference is obviously noise, and its job is to stop the loop speaking
/// before it has anything to say.
pub const MINIMUM_COHORT: usize = 5;

/// The smallest number of distinct counterparties.
///
/// §5: *"Not learn from a single counterparty. One rejection is a fact about
/// that counterparty, not about the pitch."* Two is the least that can possibly
/// be a pattern rather than a person.
pub const MINIMUM_COUNTERPARTIES: usize = 2;

/// What one cohort actually contains.
///
/// Every field is a count of something observed. Deliberately no rate, no score
/// and no confidence interval: a single number invites comparison without
/// looking at what produced it, which is precisely how an introducer's effect
/// gets attributed to a subject line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSummary {
    pub variant_ref: String,
    pub variant_version: String,
    /// Observations that may be used as evidence — delivered, and matured where
    /// silence is involved.
    pub usable: usize,
    /// Observations recorded but not usable. Reported so "we sent thirty" and
    /// "thirty are comparable" stay visibly different numbers.
    pub unusable: usize,
    pub engaged: usize,
    pub rejected: usize,
    pub silent: usize,
    /// Distinct counterparties, by engagement. An observation with no engagement
    /// cannot be attributed to anyone, so it does not count toward diversity.
    pub counterparties: usize,
    /// Every confounder value present, and how many usable observations carried
    /// it. This is what makes the difference between "the copy worked" and "we
    /// happened to have warm intros that round" visible.
    pub confounders: BTreeMap<String, BTreeMap<String, usize>>,
}

impl CohortSummary {
    /// Whether this cohort is large and diverse enough to say anything.
    pub fn can_support_a_proposal(&self) -> bool {
        self.usable >= MINIMUM_COHORT && self.counterparties >= MINIMUM_COUNTERPARTIES
    }
}

/// Summarise one cohort's observations.
///
/// `now` decides maturity, so a caller asking about a past moment gets what was
/// knowable then rather than what is knowable now.
pub fn summarise_cohort(
    variant_ref: &str,
    variant_version: &str,
    observations: &[OutcomeObservation],
    now: DateTime<Utc>,
) -> CohortSummary {
    let mut summary = CohortSummary {
        variant_ref: variant_ref.to_string(),
        variant_version: variant_version.to_string(),
        usable: 0,
        unusable: 0,
        engaged: 0,
        rejected: 0,
        silent: 0,
        counterparties: 0,
        confounders: BTreeMap::new(),
    };

    let mut engagements: BTreeSet<&str> = BTreeSet::new();
    for observation in observations {
        if !observation.is_usable_evidence(now) {
            summary.unusable += 1;
            continue;
        }
        summary.usable += 1;

        match observation.label {
            OutcomeLabel::Rejected => summary.rejected += 1,
            OutcomeLabel::Silent => summary.silent += 1,
            label if label.is_engagement() => summary.engaged += 1,
            _ => {},
        }

        if let Some(engagement) = observation.engagement_id.as_deref() {
            engagements.insert(engagement);
        }

        for confounder in &observation.confounders {
            *summary
                .confounders
                .entry(confounder.kind.clone())
                .or_default()
                .entry(confounder.value.clone())
                .or_insert(0) += 1;
        }
    }

    summary.counterparties = engagements.len();
    summary
}

/// Why no candidate could be made.
///
/// Returned rather than logged, because *"we cannot say anything yet, and here
/// is what is missing"* is the useful answer to an owner asking why nothing has
/// been proposed. Silence from a learning loop is indistinguishable from a
/// broken one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NotProposable {
    /// One or both cohorts is too small.
    SampleTooSmall {
        baseline: usize,
        candidate: usize,
        needed: usize,
    },
    /// One or both cohorts comes from too few counterparties. §5: one rejection
    /// is a fact about that counterparty, not about the pitch.
    TooFewCounterparties {
        baseline: usize,
        candidate: usize,
        needed: usize,
    },
    /// The cohorts differ so much in a confounder that any difference in
    /// outcome is at least as easily explained by it.
    ConfoundedBy {
        kind: String,
        baseline: String,
        candidate: String,
    },
    /// Nothing distinguishes the cohorts.
    NoDifference,
    /// No evidence was cited.
    ///
    /// §9: *"Every proposal carries sample size, confounders and evidence
    /// refs."* A candidate whose claims cannot be checked is worse than none —
    /// it looks like a finding and cannot be audited into one.
    NoEvidenceCited,
}

/// A change the owner might make, with everything needed to disagree with it.
///
/// There is deliberately no `apply` and no score. The owner decides; this
/// describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub variant_ref: String,
    pub baseline: CohortSummary,
    pub candidate: CohortSummary,
    /// Observation ids behind both cohorts, so a reader can go and look.
    pub evidence_refs: Vec<String>,
    /// Confounders that differ between the cohorts without being severe enough
    /// to block. Present so an owner can weigh them; **not** filtered away.
    pub caveats: Vec<String>,
    pub observed_at: DateTime<Utc>,
}

impl Candidate {
    /// Engagement counts, never a rate.
    ///
    /// A rate on a sample of six reads as precision that is not there, and the
    /// owner comparing `0.33` with `0.50` is comparing two and three replies.
    pub fn engagement_counts(&self) -> ((usize, usize), (usize, usize)) {
        (
            (self.baseline.engaged, self.baseline.usable),
            (self.candidate.engaged, self.candidate.usable),
        )
    }
}

/// Compare two cohorts of the same variant and produce a candidate, or say why
/// not.
///
/// # The refusals are the feature
///
/// Three of them, in order of how badly they would mislead:
///
/// 1. **too small** — §8: *"a proposal that cannot state its N is not made"*;
/// 2. **too few counterparties** — one counterparty is a fact about them;
/// 3. **confounded** — the cohorts differ in something other than the thing
///    being tested, so attributing the difference to the change would be
///    exactly the introducer-versus-subject-line error §3 warns about. This
///    includes a confounder dominant on one side and **unrecorded** on the
///    other, which is no information rather than no difference;
/// 4. **no evidence cited** — §9 requires evidence refs, and a claim nobody can
///    check is worse than no claim.
///
/// A confounder that differs but not dominantly becomes a **caveat** rather than
/// a block, and is carried on the candidate. Filtering it away would hand the
/// owner a cleaner story than the data supports.
pub fn propose(
    variant_ref: &str,
    baseline: &CohortSummary,
    candidate: &CohortSummary,
    evidence_refs: Vec<String>,
    now: DateTime<Utc>,
) -> Result<Candidate, NotProposable> {
    if !meets(baseline.usable, candidate.usable, MINIMUM_COHORT) {
        return Err(NotProposable::SampleTooSmall {
            baseline: baseline.usable,
            candidate: candidate.usable,
            needed: MINIMUM_COHORT,
        });
    }
    if !meets(
        baseline.counterparties,
        candidate.counterparties,
        MINIMUM_COUNTERPARTIES,
    ) {
        return Err(NotProposable::TooFewCounterparties {
            baseline: baseline.counterparties,
            candidate: candidate.counterparties,
            needed: MINIMUM_COUNTERPARTIES,
        });
    }

    if evidence_refs.is_empty() {
        return Err(NotProposable::NoEvidenceCited);
    }

    // The UNION of confounder kinds, not just the baseline's.
    //
    // Iterating only one side misses the worse case: a kind dominant in one
    // cohort and entirely **unrecorded** in the other. That is not "no
    // difference" — it is no information, and it cannot be ruled out as the
    // explanation. Treating an absent confounder as absent-in-fact is how a
    // cohort that simply was not measured passes as comparable.
    const UNRECORDED: &str = "<unrecorded>";
    let mut kinds: BTreeSet<&String> = baseline.confounders.keys().collect();
    kinds.extend(candidate.confounders.keys());

    let mut caveats = Vec::new();
    for kind in kinds {
        let empty = BTreeMap::new();
        let baseline_values = baseline.confounders.get(kind).unwrap_or(&empty);
        let candidate_values = candidate.confounders.get(kind).unwrap_or(&empty);

        let left = dominant(baseline_values, baseline.usable);
        let right = dominant(candidate_values, candidate.usable);

        match (left, right) {
            // Dominated by different values — whatever else changed, this
            // changed too, and it explains the outcome at least as well.
            (Some(left), Some(right)) if left != right => {
                return Err(NotProposable::ConfoundedBy {
                    kind: kind.clone(),
                    baseline: left.to_string(),
                    candidate: right.to_string(),
                });
            },
            // Dominant on one side, unrecorded on the other. Cannot be ruled
            // out, so it is not compared away.
            (Some(left), None) if candidate_values.is_empty() => {
                return Err(NotProposable::ConfoundedBy {
                    kind: kind.clone(),
                    baseline: left.to_string(),
                    candidate: UNRECORDED.to_string(),
                });
            },
            (None, Some(right)) if baseline_values.is_empty() => {
                return Err(NotProposable::ConfoundedBy {
                    kind: kind.clone(),
                    baseline: UNRECORDED.to_string(),
                    candidate: right.to_string(),
                });
            },
            _ => {
                if baseline_values != candidate_values {
                    caveats.push(format!(
                        "{kind} differs between cohorts: {baseline_values:?} vs {candidate_values:?}"
                    ));
                }
            },
        }
    }

    if baseline.engaged == candidate.engaged
        && baseline.rejected == candidate.rejected
        && baseline.silent == candidate.silent
    {
        return Err(NotProposable::NoDifference);
    }

    Ok(Candidate {
        variant_ref: variant_ref.to_string(),
        baseline: baseline.clone(),
        candidate: candidate.clone(),
        evidence_refs,
        caveats,
        observed_at: now,
    })
}

fn meets(left: usize, right: usize, needed: usize) -> bool {
    left >= needed && right >= needed
}

/// The value carrying more than half a cohort, if any.
///
/// A simple majority rather than a plurality: with three values at 40/30/30 no
/// single one explains the cohort, and calling the 40 "dominant" would block
/// proposals on a split that is really just variety.
fn dominant<'a>(values: &'a BTreeMap<String, usize>, total: usize) -> Option<&'a str> {
    if total == 0 {
        return None;
    }
    values
        .iter()
        .find(|(_, count)| **count * 2 > total)
        .map(|(value, _)| value.as_str())
}
