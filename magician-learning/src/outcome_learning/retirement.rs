//! Stale and contradicted claims — plan phase 5.
//!
//! Plan: `docs/plans/2026-08-07-opc-outcome-learning.md`.
//!
//! §4's last candidate kind: *"retirement — this answer has not been used in
//! six months, or contradicts a newer one."* Phase 5 pins where the evidence
//! comes from: *"Stale and conflicting claims surfaced against the reverse
//! index in outward assertions — what was actually said, rather than a
//! separately-maintained record of what we meant to say."*
//!
//! # Pure over supplied rows
//!
//! The caller reads the reverse index of
//! [`magician::magician_v2::evidence::outward_assertions`] and hands the rows in
//! as [`ClaimUse`]s; this module never opens a store, a calendar or an inbox.
//! The decoupling is deliberate: any subsystem that can say *"this claim was
//! asserted, in this act, to this audience, at this time"* can ask these two
//! questions, and none of them has to link against the one that answers.
//!
//! The `audience` field is carried verbatim as the reverse index's recipient
//! key and only ever compared for distinctness. Where that key was built from
//! an `AudienceRef` it already carries the kind, so two relationship kinds
//! that share an id stay distinct here too.
//!
//! # Descriptions, not actions
//!
//! §8's first control: *"It never applies anything; every change is an owner
//! editorial decision."* Both outputs are things to look at — there is no
//! function that retires a claim, and no rate, score or ranking anywhere,
//! only counts sitting next to the totals they came from. §8's sample-size
//! control is structural here as it is in phase 3: *"Every proposal states
//! its N… a proposal that cannot is not made"* — a [`RetirementCandidate`]
//! carries every `use_ref` it was counted from, so one that could not cite
//! its uses cannot be built at all.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// One row of the reverse index: one claim, asserted once, to one audience.
///
/// Supplied by the caller. Phase 5's whole instruction is to read *"what was
/// actually said, rather than a separately-maintained record of what we meant
/// to say"*, and the caller is the one holding that record of what was said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimUse {
    /// The claim that was asserted.
    pub claim_ref: String,
    /// The assertion-use row this came from — the receipt a finding cites.
    pub use_ref: String,
    /// The recipient key, verbatim. Compared only for distinctness, never
    /// interpreted.
    pub audience: String,
    /// When the assertion was made.
    pub used_at: DateTime<Utc>,
    /// Claims this use declared it replaces.
    pub supersedes: Vec<String>,
}

/// How long a claim may go unused before it is worth the owner's look.
///
/// §4 names six months. The number is a judgement about the domain, so it is
/// configuration rather than a constant — but it cannot be configured into
/// nonsense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetirementPolicy {
    idle_after: Duration,
}

impl RetirementPolicy {
    /// Refuses a non-positive window.
    ///
    /// Zero would nominate a claim for retirement the instant it was used,
    /// and every claim in the index has been used at least once — so the
    /// candidate list would be the whole index, and a report that says
    /// everything says nothing. The refusal keeps the list meaning what it
    /// claims to mean.
    pub fn new(idle_after: Duration) -> Result<Self> {
        if idle_after <= Duration::zero() {
            anyhow::bail!(
                "a retirement idle window must be positive: zero would nominate every claim \
                 the moment it is used, burying the genuinely stale ones the list exists to \
                 surface"
            );
        }
        Ok(Self { idle_after })
    }

    pub fn idle_after(&self) -> Duration {
        self.idle_after
    }
}

/// A claim that has gone quiet — §4: *"this answer has not been used in six
/// months"*.
///
/// A description for the owner, not an action: nothing in this module retires
/// anything. Not serialised, either — `idle_for` is derived from the clock
/// the caller asked at, so this is a reading, not a record, and persisting
/// one would freeze a "how stale" that stops being true a moment later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetirementCandidate {
    pub claim_ref: String,
    /// The most recent time the claim was asserted.
    pub last_used_at: DateTime<Utc>,
    /// The N: how many distinct uses the claim ever had, so the reader can
    /// tell "abandoned" from "was barely used to begin with".
    pub total_uses: usize,
    /// How long since the last use, at the `now` the caller asked about.
    /// Derived at read time, never stored.
    pub idle_for: Duration,
    /// Every `use_ref` the claim was counted from, oldest first — the N and
    /// the receipts. §8: a candidate that cannot cite its uses is not made.
    pub evidence_refs: Vec<String>,
}

/// One assertion of a stale claim made after its correction existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OffendingUse {
    pub use_ref: String,
    /// Who was told. This is the field the finding exists for: it names where
    /// a correction now has to go.
    pub audience: String,
    pub used_at: DateTime<Utc>,
}

/// §4: a claim that *"contradicts a newer one"* — and was asserted anyway.
///
/// The finding is not that a claim was superseded; supersession is the system
/// working. The finding is that **we asserted the old figure after stating
/// the new one** — and the offending uses name exactly who correction
/// propagation must reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contradiction {
    pub stale_claim: String,
    pub superseding_claim: String,
    /// The earliest use that declared the supersession — the moment the newer
    /// answer existed.
    pub superseded_at: DateTime<Utc>,
    /// Every use of the stale claim strictly after `superseded_at`, oldest
    /// first.
    pub offending_uses: Vec<OffendingUse>,
    /// Distinct audiences among the offending uses — how many relationships
    /// heard the old figure after the new one existed. A count sitting next
    /// to its total (`offending_uses`), never a rate.
    pub audiences_told_stale: usize,
}

/// The rows knowable at `now`: dated at or before it, one per `use_ref`.
///
/// A future-dated row is excluded rather than trusted: trusted, it would
/// reset a claim's idleness and suppress a staleness finding that is true
/// today, and a declaration dated in the future has not yet made anything
/// contradictory. And because the caller is reading an index, replays happen:
/// the same `use_ref` seen twice is one use, first appearance kept, so a
/// replay cannot double a claim's N.
fn knowable(uses: &[ClaimUse], now: DateTime<Utc>) -> Vec<&ClaimUse> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::new();
    for row in uses {
        if row.used_at > now {
            continue;
        }
        if seen.insert(row.use_ref.as_str()) {
            out.push(row);
        }
    }
    out
}

/// Claims whose last use is at least `idle_after` ago, most idle first.
///
/// The boundary is **inclusive**, like every deadline in this codebase: a
/// six-month window means a claim last used exactly six months ago has
/// completed it.
///
/// A claim that was **superseded** by any use is excluded, however idle: it
/// is not stale, it is replaced — a different fact belonging to a different
/// list, surfaced by [`contradictions`] when it goes wrong and by nothing
/// when it goes right. A use declaring it supersedes its own claim is a data
/// error, not a replacement, and shields nothing.
///
/// `now` is the caller's clock, so asking about a past moment yields what was
/// knowable then. Output order is deterministic: oldest last use first, then
/// claim ref.
pub fn retirement_candidates(
    uses: &[ClaimUse],
    policy: &RetirementPolicy,
    now: DateTime<Utc>,
) -> Vec<RetirementCandidate> {
    let rows = knowable(uses, now);

    // Claims some use declared it replaces. Replaced is not stale.
    let mut superseded: BTreeSet<&str> = BTreeSet::new();
    for row in &rows {
        for replaced in &row.supersedes {
            // Self-reference is a data error, not a replacement: nothing
            // newer exists, so it must not lift the claim out of staleness.
            if replaced != &row.claim_ref {
                superseded.insert(replaced.as_str());
            }
        }
    }

    let mut by_claim: BTreeMap<&str, Vec<&ClaimUse>> = BTreeMap::new();
    for row in &rows {
        by_claim
            .entry(row.claim_ref.as_str())
            .or_default()
            .push(row);
    }

    let mut out = Vec::new();
    for (claim_ref, mut claim_uses) in by_claim {
        if superseded.contains(claim_ref) {
            continue;
        }
        claim_uses.sort_by(|left, right| {
            left.used_at
                .cmp(&right.used_at)
                .then_with(|| left.use_ref.cmp(&right.use_ref))
        });
        // §8: a candidate that cannot cite its uses is not made. A group is
        // only ever built from uses, so an empty one is unreachable today —
        // but the guard is the contract, and it holds if grouping changes.
        let Some(last) = claim_uses.last() else {
            continue;
        };
        let idle_for = now - last.used_at;
        if idle_for < policy.idle_after() {
            continue;
        }
        out.push(RetirementCandidate {
            claim_ref: claim_ref.to_string(),
            last_used_at: last.used_at,
            total_uses: claim_uses.len(),
            idle_for,
            evidence_refs: claim_uses
                .iter()
                .map(|claim_use| claim_use.use_ref.clone())
                .collect(),
        });
    }

    out.sort_by(|left, right| {
        left.last_used_at
            .cmp(&right.last_used_at)
            .then_with(|| left.claim_ref.cmp(&right.claim_ref))
    });
    out
}

/// Uses of a superseded claim made after its supersession was declared — one
/// row per `(stale, superseding)` pair, each with its own offence list.
///
/// A superseded claim never re-asserted is **healthy** and produces nothing:
/// the finding is the re-assertion, not the supersession. And a use stamped
/// **at exactly** the declaration instant is not offending — a batch carrying
/// one timestamp has no knowable internal order, and accusing part of it
/// would be manufacturing a fact — so only uses strictly after count.
///
/// Multiple superseders each get their own row, because which correction the
/// old figure conflicts with changes what the follow-up has to say. Rows are
/// ordered by stale claim then superseding claim, so everything wrong with
/// one claim reads together; offending uses are oldest first.
///
/// `now` bounds what is knowable, exactly as in [`retirement_candidates`].
pub fn contradictions(uses: &[ClaimUse], now: DateTime<Utc>) -> Vec<Contradiction> {
    let rows = knowable(uses, now);

    // The earliest declaration per (stale, superseding) pair: the newer
    // answer existed from the first time anything said so, and repeating the
    // declaration later must not shrink the offence window.
    let mut declared_at: BTreeMap<(&str, &str), DateTime<Utc>> = BTreeMap::new();
    for row in &rows {
        for replaced in &row.supersedes {
            // A claim superseding itself is a data error, not a contradiction:
            // there is no newer answer for its own uses to conflict with.
            if replaced == &row.claim_ref {
                continue;
            }
            let earliest = declared_at
                .entry((replaced.as_str(), row.claim_ref.as_str()))
                .or_insert(row.used_at);
            if row.used_at < *earliest {
                *earliest = row.used_at;
            }
        }
    }

    let mut uses_of: BTreeMap<&str, Vec<&ClaimUse>> = BTreeMap::new();
    for row in &rows {
        uses_of.entry(row.claim_ref.as_str()).or_default().push(row);
    }
    for claim_uses in uses_of.values_mut() {
        claim_uses.sort_by(|left, right| {
            left.used_at
                .cmp(&right.used_at)
                .then_with(|| left.use_ref.cmp(&right.use_ref))
        });
    }

    let mut out = Vec::new();
    for ((stale_claim, superseding_claim), superseded_at) in declared_at {
        let Some(stale_uses) = uses_of.get(stale_claim) else {
            continue;
        };
        let offending_uses: Vec<OffendingUse> = stale_uses
            .iter()
            // Strictly after: a use at exactly the declaration instant is a
            // simultaneous batch whose order is unknowable.
            .filter(|claim_use| claim_use.used_at > superseded_at)
            .map(|claim_use| OffendingUse {
                use_ref: claim_use.use_ref.clone(),
                audience: claim_use.audience.clone(),
                used_at: claim_use.used_at,
            })
            .collect();
        // No offending uses means the supersession simply worked. A row here
        // would punish the healthy case and train the owner to skim the list.
        if offending_uses.is_empty() {
            continue;
        }
        let audiences: BTreeSet<&str> = offending_uses
            .iter()
            .map(|offence| offence.audience.as_str())
            .collect();
        out.push(Contradiction {
            stale_claim: stale_claim.to_string(),
            superseding_claim: superseding_claim.to_string(),
            superseded_at,
            audiences_told_stale: audiences.len(),
            offending_uses,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    //! Phase 5's contract, as behaviour.

    use chrono::TimeZone;

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    /// §4's own number: *"this answer has not been used in six months."*
    fn policy() -> RetirementPolicy {
        RetirementPolicy::new(Duration::days(180)).expect("a positive window")
    }

    fn fixture(claim_ref: &str, use_ref: &str, audience: &str, used_at: DateTime<Utc>) -> ClaimUse {
        ClaimUse {
            claim_ref: claim_ref.to_string(),
            use_ref: use_ref.to_string(),
            audience: audience.to_string(),
            used_at,
            supersedes: Vec::new(),
        }
    }

    fn superseding(
        claim_ref: &str,
        use_ref: &str,
        audience: &str,
        used_at: DateTime<Utc>,
        supersedes: &[&str],
    ) -> ClaimUse {
        ClaimUse {
            supersedes: supersedes.iter().map(|claim| claim.to_string()).collect(),
            ..fixture(claim_ref, use_ref, audience, used_at)
        }
    }

    /// §4: *"this answer has not been used in six months."* The window is
    /// inclusive, like every deadline in this codebase: exactly `idle_after`
    /// idle has completed the window; a second less has not.
    #[test]
    fn the_idle_window_is_inclusive_at_exactly_idle_after() {
        let rows = vec![
            fixture(
                "at-boundary",
                "use-1",
                "engagement:acme",
                now() - Duration::days(180),
            ),
            fixture(
                "just-inside",
                "use-2",
                "engagement:acme",
                now() - Duration::days(180) + Duration::seconds(1),
            ),
        ];

        let candidates = retirement_candidates(&rows, &policy(), now());
        assert_eq!(
            candidates,
            vec![RetirementCandidate {
                claim_ref: "at-boundary".to_string(),
                last_used_at: now() - Duration::days(180),
                total_uses: 1,
                idle_for: Duration::days(180),
                evidence_refs: vec!["use-1".to_string()],
            }]
        );
    }

    /// §4 keeps the two findings apart: *"has not been used in six months, or
    /// contradicts a newer one."* A superseded claim is not stale, it is
    /// replaced — it leaves the staleness list entirely, while its
    /// replacement earns staleness on its own record.
    #[test]
    fn a_superseded_claim_is_replaced_not_stale() {
        let rows = vec![
            fixture(
                "old-figure",
                "use-1",
                "engagement:acme",
                now() - Duration::days(400),
            ),
            superseding(
                "new-figure",
                "use-2",
                "engagement:acme",
                now() - Duration::days(300),
                &["old-figure"],
            ),
        ];

        let candidates = retirement_candidates(&rows, &policy(), now());
        assert_eq!(
            candidates,
            vec![RetirementCandidate {
                claim_ref: "new-figure".to_string(),
                last_used_at: now() - Duration::days(300),
                total_uses: 1,
                idle_for: Duration::days(300),
                evidence_refs: vec!["use-2".to_string()],
            }]
        );
    }

    /// The list answers "what has gone quietest longest", so it is sorted
    /// most idle first, with a claim-ref tie-break so equal idleness reads in
    /// one stable order on every run.
    #[test]
    fn candidates_surface_most_idle_first_in_a_stable_order() {
        let rows = vec![
            fixture(
                "recent-ish",
                "use-1",
                "engagement:acme",
                now() - Duration::days(200),
            ),
            fixture(
                "z-old",
                "use-2",
                "engagement:acme",
                now() - Duration::days(400),
            ),
            fixture(
                "a-old",
                "use-3",
                "engagement:acme",
                now() - Duration::days(400),
            ),
        ];

        let order: Vec<String> = retirement_candidates(&rows, &policy(), now())
            .into_iter()
            .map(|candidate| candidate.claim_ref)
            .collect();
        assert_eq!(order, vec!["a-old", "z-old", "recent-ish"]);
    }

    /// §8: *"Every proposal states its N… a proposal that cannot is not
    /// made."* Every use of the claim rides along as a receipt, oldest first,
    /// and the N is their count — whatever order the index handed them over
    /// in.
    #[test]
    fn a_candidate_cites_every_use_it_was_counted_from() {
        let rows = vec![
            fixture(
                "claim-a",
                "use-middle",
                "engagement:acme",
                now() - Duration::days(300),
            ),
            fixture(
                "claim-a",
                "use-last",
                "account:acme",
                now() - Duration::days(200),
            ),
            fixture(
                "claim-a",
                "use-first",
                "engagement:acme",
                now() - Duration::days(400),
            ),
        ];

        let candidates = retirement_candidates(&rows, &policy(), now());
        assert_eq!(
            candidates,
            vec![RetirementCandidate {
                claim_ref: "claim-a".to_string(),
                last_used_at: now() - Duration::days(200),
                total_uses: 3,
                idle_for: Duration::days(200),
                evidence_refs: vec![
                    "use-first".to_string(),
                    "use-middle".to_string(),
                    "use-last".to_string(),
                ],
            }]
        );
    }

    /// The caller reads a reverse index, and index reads replay. The same
    /// `use_ref` seen twice is one use — a doubled N would overstate how
    /// load-bearing a claim was, which is the §8 failure in miniature.
    #[test]
    fn a_replayed_index_row_does_not_double_the_n() {
        let row = fixture(
            "claim-a",
            "use-1",
            "engagement:acme",
            now() - Duration::days(300),
        );

        let candidates = retirement_candidates(&[row.clone(), row], &policy(), now());
        assert_eq!(
            candidates,
            vec![RetirementCandidate {
                claim_ref: "claim-a".to_string(),
                last_used_at: now() - Duration::days(300),
                total_uses: 1,
                idle_for: Duration::days(300),
                evidence_refs: vec!["use-1".to_string()],
            }]
        );
    }

    /// A row dated after `now` is not yet knowable. Trusted, it would reset
    /// the claim's idleness and hide a staleness finding that is true today —
    /// the permissive reading, refused.
    #[test]
    fn a_use_dated_after_now_is_not_yet_knowable() {
        let rows = vec![
            fixture(
                "claim-a",
                "use-1",
                "engagement:acme",
                now() - Duration::days(200),
            ),
            fixture(
                "claim-a",
                "use-2",
                "engagement:acme",
                now() + Duration::days(1),
            ),
        ];

        let candidates = retirement_candidates(&rows, &policy(), now());
        assert_eq!(
            candidates,
            vec![RetirementCandidate {
                claim_ref: "claim-a".to_string(),
                last_used_at: now() - Duration::days(200),
                total_uses: 1,
                idle_for: Duration::days(200),
                evidence_refs: vec!["use-1".to_string()],
            }]
        );
    }

    /// A zero window would nominate a claim the moment it is used, making the
    /// candidate list the whole index. Refused at construction, so the state
    /// cannot be reached by configuration.
    #[test]
    fn a_non_positive_idle_window_is_refused() {
        let refused = RetirementPolicy::new(Duration::zero()).expect_err("zero must be refused");
        assert!(
            refused.to_string().contains("must be positive"),
            "{refused}"
        );
        assert!(RetirementPolicy::new(Duration::days(-7)).is_err());
        assert_eq!(
            RetirementPolicy::new(Duration::days(180))
                .expect("a positive window")
                .idle_after(),
            Duration::days(180)
        );
    }

    /// The finding itself: we asserted the old number after stating the new
    /// one. The use before the correction is clean; the one after is the row,
    /// carrying its audience — who was told is exactly who a correction must
    /// now reach.
    #[test]
    fn asserting_the_old_figure_after_the_correction_is_the_finding() {
        let declared = now() - Duration::days(30);
        let rows = vec![
            fixture(
                "old-figure",
                "use-0",
                "engagement:acme",
                declared - Duration::days(3),
            ),
            superseding(
                "new-figure",
                "use-1",
                "engagement:acme",
                declared,
                &["old-figure"],
            ),
            fixture(
                "old-figure",
                "use-2",
                "engagement:beta",
                declared + Duration::days(3),
            ),
        ];

        let found = contradictions(&rows, now());
        assert_eq!(
            found,
            vec![Contradiction {
                stale_claim: "old-figure".to_string(),
                superseding_claim: "new-figure".to_string(),
                superseded_at: declared,
                offending_uses: vec![OffendingUse {
                    use_ref: "use-2".to_string(),
                    audience: "engagement:beta".to_string(),
                    used_at: declared + Duration::days(3),
                }],
                audiences_told_stale: 1,
            }]
        );
    }

    /// The batch case: a correction and a use of the old figure stamped with
    /// the same instant have no knowable order, and accusing the use would be
    /// manufacturing a fact. Only strictly-after counts.
    #[test]
    fn a_use_at_exactly_the_supersession_instant_is_clean() {
        let declared = now() - Duration::days(30);
        let rows = vec![
            fixture("old-figure", "use-1", "engagement:acme", declared),
            superseding(
                "new-figure",
                "use-2",
                "engagement:acme",
                declared,
                &["old-figure"],
            ),
        ];

        assert_eq!(contradictions(&rows, now()), Vec::new());
    }

    /// The healthy lifecycle: a figure was used, then corrected, and the old
    /// one never spoken again. No contradiction row — the finding is the
    /// re-assertion, not the supersession — and no staleness row either,
    /// because replaced is not stale.
    #[test]
    fn a_superseded_claim_never_reasserted_is_healthy() {
        let rows = vec![
            fixture(
                "old-figure",
                "use-1",
                "engagement:acme",
                now() - Duration::days(300),
            ),
            superseding(
                "new-figure",
                "use-2",
                "engagement:acme",
                now() - Duration::days(30),
                &["old-figure"],
            ),
        ];

        assert_eq!(contradictions(&rows, now()), Vec::new());
        assert_eq!(retirement_candidates(&rows, &policy(), now()), Vec::new());
    }

    /// Which correction the old figure conflicts with changes what the
    /// follow-up has to say, so each superseder gets its own row with its own
    /// offence list — and the rows come out in one stable order.
    #[test]
    fn each_superseding_claim_gets_its_own_row() {
        let first_correction_at = now() - Duration::days(40);
        let second_correction_at = now() - Duration::days(20);
        let rows = vec![
            superseding(
                "rev-b",
                "use-1",
                "engagement:acme",
                first_correction_at,
                &["rev-a"],
            ),
            superseding(
                "rev-c",
                "use-2",
                "engagement:acme",
                second_correction_at,
                &["rev-a"],
            ),
            fixture(
                "rev-a",
                "use-3",
                "engagement:acme",
                now() - Duration::days(30),
            ),
            fixture(
                "rev-a",
                "use-4",
                "engagement:beta",
                now() - Duration::days(10),
            ),
        ];

        let found = contradictions(&rows, now());
        assert_eq!(
            found,
            vec![
                Contradiction {
                    stale_claim: "rev-a".to_string(),
                    superseding_claim: "rev-b".to_string(),
                    superseded_at: first_correction_at,
                    offending_uses: vec![
                        OffendingUse {
                            use_ref: "use-3".to_string(),
                            audience: "engagement:acme".to_string(),
                            used_at: now() - Duration::days(30),
                        },
                        OffendingUse {
                            use_ref: "use-4".to_string(),
                            audience: "engagement:beta".to_string(),
                            used_at: now() - Duration::days(10),
                        },
                    ],
                    audiences_told_stale: 2,
                },
                Contradiction {
                    stale_claim: "rev-a".to_string(),
                    superseding_claim: "rev-c".to_string(),
                    superseded_at: second_correction_at,
                    offending_uses: vec![OffendingUse {
                        use_ref: "use-4".to_string(),
                        audience: "engagement:beta".to_string(),
                        used_at: now() - Duration::days(10),
                    }],
                    audiences_told_stale: 1,
                },
            ]
        );
    }

    /// `superseded_at` is the earliest declaration: the newer answer existed
    /// from the first time anything said so, and repeating the declaration
    /// later must not shrink the offence window.
    #[test]
    fn the_earliest_declaration_opens_the_offence_window() {
        let early = now() - Duration::days(40);
        let late = now() - Duration::days(10);
        let rows = vec![
            superseding(
                "new-figure",
                "use-1",
                "engagement:acme",
                late,
                &["old-figure"],
            ),
            superseding(
                "new-figure",
                "use-2",
                "engagement:beta",
                early,
                &["old-figure"],
            ),
            fixture(
                "old-figure",
                "use-3",
                "engagement:acme",
                now() - Duration::days(20),
            ),
        ];

        let found = contradictions(&rows, now());
        assert_eq!(
            found,
            vec![Contradiction {
                stale_claim: "old-figure".to_string(),
                superseding_claim: "new-figure".to_string(),
                superseded_at: early,
                offending_uses: vec![OffendingUse {
                    use_ref: "use-3".to_string(),
                    audience: "engagement:acme".to_string(),
                    used_at: now() - Duration::days(20),
                }],
                audiences_told_stale: 1,
            }]
        );
    }

    /// A use listing its own claim in `supersedes` is a data error, not a
    /// correction: nothing newer exists. It creates no contradiction, and it
    /// shields the claim from staleness no more than any other use of it.
    #[test]
    fn self_supersession_is_a_data_error_not_a_correction() {
        let rows = vec![
            superseding(
                "claim-a",
                "use-1",
                "engagement:acme",
                now() - Duration::days(400),
                &["claim-a"],
            ),
            fixture(
                "claim-a",
                "use-2",
                "engagement:acme",
                now() - Duration::days(200),
            ),
        ];

        assert_eq!(contradictions(&rows, now()), Vec::new());
        let candidates = retirement_candidates(&rows, &policy(), now());
        assert_eq!(
            candidates,
            vec![RetirementCandidate {
                claim_ref: "claim-a".to_string(),
                last_used_at: now() - Duration::days(200),
                total_uses: 2,
                idle_for: Duration::days(200),
                evidence_refs: vec!["use-1".to_string(), "use-2".to_string()],
            }]
        );
    }

    /// The triage number: how many distinct relationships heard the old
    /// figure after the new one existed. Distinct audiences, with the full
    /// offence list beside it, so three assertions into one room and one each
    /// into three rooms read differently.
    #[test]
    fn audiences_told_the_stale_figure_are_counted_distinctly() {
        let declared = now() - Duration::days(30);
        let rows = vec![
            superseding(
                "new-figure",
                "use-1",
                "engagement:acme",
                declared,
                &["old-figure"],
            ),
            fixture(
                "old-figure",
                "use-2",
                "engagement:acme",
                declared + Duration::days(5),
            ),
            fixture(
                "old-figure",
                "use-3",
                "engagement:acme",
                declared + Duration::days(10),
            ),
            fixture(
                "old-figure",
                "use-4",
                "engagement:beta",
                declared + Duration::days(15),
            ),
        ];

        let found = contradictions(&rows, now());
        assert_eq!(
            found,
            vec![Contradiction {
                stale_claim: "old-figure".to_string(),
                superseding_claim: "new-figure".to_string(),
                superseded_at: declared,
                offending_uses: vec![
                    OffendingUse {
                        use_ref: "use-2".to_string(),
                        audience: "engagement:acme".to_string(),
                        used_at: declared + Duration::days(5),
                    },
                    OffendingUse {
                        use_ref: "use-3".to_string(),
                        audience: "engagement:acme".to_string(),
                        used_at: declared + Duration::days(10),
                    },
                    OffendingUse {
                        use_ref: "use-4".to_string(),
                        audience: "engagement:beta".to_string(),
                        used_at: declared + Duration::days(15),
                    },
                ],
                audiences_told_stale: 2,
            }]
        );
    }

    /// The vacuous case, pinned: an empty index yields empty findings. "No
    /// data" must never read as "everything is stale", and a contradiction
    /// about nobody must not be manufactured.
    #[test]
    fn an_empty_index_yields_no_findings() {
        assert_eq!(retirement_candidates(&[], &policy(), now()), Vec::new());
        assert_eq!(contradictions(&[], now()), Vec::new());
    }
}
