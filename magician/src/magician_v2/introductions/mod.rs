//! **Who introduced us to whom, and what we owe them for it.**
//!
//! Doc: `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §3.4.
//! A warm introduction is the channel that actually works, and the introduction
//! *is* evidence — often stronger than a domain match, because a person we trust
//! vouched. `Identity.introduced_by` has recorded that provenance since the
//! register was built. Nothing read it.
//!
//! # Which way the dependency runs
//!
//! This is a **coordinator**, in the shape `delivery_hygiene` already uses, and
//! for the same reason. [`crate::magician_v2::counterparties`] knows who
//! vouched for whom; [`crate::magician_v2::obligations`] knows what is owed and
//! by when. Neither may import the other — a register of addresses that knew
//! about deadlines would stop being usable for anything that has no deadlines,
//! and an obligation register that knew what an introduction was would stop
//! being usable for a supplier's delivery date.
//!
//! So the join lives here, and both primitives stay ignorant of it.
//!
//! # It PROPOSES; it does not record
//!
//! [`introducer_debts`] returns what is owed and writes nothing. The write is
//! the caller's, through the obligation register's own door, because *"we owe
//! Sarah an update on the intro she made"* is a judgement about a relationship
//! and the moment to make it is not "whenever a sweep ran".
//!
//! That also keeps the derivation testable without a store to write into, and
//! it is why this module holds no handle on anything.
//!
//! # Counts, not rates
//!
//! [`ReferralGraph::len`] is the number of introductions, not the number of
//! introducers. *"Sarah introduced four"* and *"four people introduced us"* are
//! different facts and a single number conflates them, so both are reported.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::magician_v2::audience::{AudienceKind, AudienceRef};
use crate::magician_v2::counterparties::{CounterpartyScope, CounterpartyStore};
use crate::magician_v2::obligations::{ObligationDirection, RecordObligation};

/// Case- and whitespace-insensitive, so two spellings of one introducer are one
/// introducer.
///
/// `add_identity` trims `introduced_by` and does **not** lowercase it, so the
/// same person recorded as `Sarah@Example.test` on one introduction and
/// `sarah@example.test` on another arrives here as two. Grouping on the raw
/// string would then owe them two separate updates for work they did once,
/// which is the behaviour this module exists to prevent rather than to produce.
///
/// The same rule the register itself applies to organisation names: *"case and
/// spacing differences resume rather than error: they are two spellings of one
/// name."*
fn introducer_key(introduced_by: &str) -> String {
    introduced_by.trim().to_ascii_lowercase()
}

/// One introduction, as the register recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Introduction {
    /// Who vouched, in the spelling the FIRST introduction used.
    ///
    /// Reported as written so an owner recognises it; grouped on
    /// [`introducer_key`] so two spellings do not become two debts. Carried
    /// verbatim rather than resolved, because resolving it is a second question
    /// with its own failure mode and folding the two would make an
    /// unresolvable introducer look like no introduction at all.
    pub introducer: String,
    /// The organisation we were introduced TO.
    pub counterparty_id: String,
    /// Their display name, for a line an owner reads.
    pub display_name: String,
    /// The address the introduction arrived as.
    pub identity: String,
    /// When we first heard from them.
    pub introduced_at: DateTime<Utc>,
    /// When we last heard from them — the clock the debt is measured against.
    pub last_seen: DateTime<Utc>,
}

/// Who introduced us to whom.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferralGraph {
    /// Introductions, grouped by [`introducer_key`], each group ordered by when
    /// the introduction landed. The key is the comparison form; each
    /// [`Introduction::introducer`] carries the spelling to show.
    ///
    /// There is no `unresolved` list beside this, and its absence is deliberate.
    /// The first cut had one, for organisations whose row could not be read
    /// while walking the graph — and once the walk became a single grouped read
    /// there was no per-organisation failure left to report. A read that fails
    /// fails the whole graph, which is correct: answering *"nobody introduced us
    /// to anybody"* from a register we could not open is the reassuring wrong
    /// answer this module exists to avoid. A field that can never be non-empty
    /// is worse than no field, because a reader trusts it.
    pub by_introducer: BTreeMap<String, Vec<Introduction>>,
}

impl ReferralGraph {
    /// How many introductions this graph holds, across every introducer.
    pub fn len(&self) -> usize {
        self.by_introducer.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_introducer.is_empty()
    }
}

/// Build the referral graph for one scope.
///
/// Walks every live organisation's identities and groups the ones carrying an
/// `introduced_by`. Merged organisations are excluded by
/// [`CounterpartyStore::list`], which is correct: a name that was folded into
/// another is not a second introduction, and counting it would double every
/// introducer whose contact later turned out to be the same company.
pub fn referral_graph(
    counterparties: &CounterpartyStore,
    scope: &CounterpartyScope,
) -> Result<ReferralGraph> {
    let mut graph = ReferralGraph::default();
    // TWO reads of the register, not N+1. `identities_for` re-reads and re-folds
    // the whole file on every call, so asking it once per organisation parses
    // the register once per organisation — invisible at ten and quadratic at a
    // thousand, on the read that decides whether anybody is owed anything.
    //
    // Both failures propagate: an unreadable register is not an empty one, and
    // answering "nobody introduced us to anybody" from a file we could not open
    // is the reassuring wrong answer this whole module exists to avoid.
    let by_counterparty = counterparties.identities_by_counterparty(scope)?;
    for counterparty in counterparties.list(scope)? {
        // Absent means this organisation has no addresses on file, which is
        // normal and not a failure — the whole read has already succeeded by
        // the time we get here.
        let Some(identities) = by_counterparty.get(&counterparty.counterparty_id) else {
            continue;
        };
        for identity in identities {
            let Some(introducer) = identity.introduced_by.as_deref() else {
                continue;
            };
            let key = introducer_key(introducer);
            if key.is_empty() {
                continue;
            }
            // The group's own spelling is whichever introduction was filed
            // first, which the `entry` below fixes: later spellings of the same
            // introducer join that group rather than starting a new one.
            let shown = graph
                .by_introducer
                .get(&key)
                .and_then(|held| held.first())
                .map(|held| held.introducer.clone())
                .unwrap_or_else(|| introducer.trim().to_string());
            graph
                .by_introducer
                .entry(key)
                .or_default()
                .push(Introduction {
                    introducer: shown,
                    counterparty_id: counterparty.counterparty_id.clone(),
                    display_name: counterparty.display_name.clone(),
                    identity: identity.normalised.clone(),
                    introduced_at: identity.first_seen,
                    last_seen: identity.last_seen,
                });
        }
    }
    for introductions in graph.by_introducer.values_mut() {
        introductions.sort_by(|left, right| {
            left.introduced_at
                .cmp(&right.introduced_at)
                .then_with(|| left.counterparty_id.cmp(&right.counterparty_id))
        });
    }
    Ok(graph)
}

/// How long an introducer may go un-updated before we owe them one.
///
/// A parameter, because the right interval is a fact about how you work rather
/// than about the code, and because "we owe Sarah an update" fourteen days after
/// an intro and ninety days after one are both defensible positions that
/// somebody should have to choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntroducerPolicy {
    update_after: Duration,
}

impl IntroducerPolicy {
    /// Refused at or below zero rather than substituted.
    ///
    /// A zero window owes the introducer an update the instant they make one,
    /// which is a debt nobody could ever have discharged and a register that
    /// fills with rows on its first sweep.
    pub fn new(update_after: Duration) -> Result<Self> {
        if update_after <= Duration::zero() {
            anyhow::bail!(
                "an introducer update window must be positive; `{}` seconds owes an update the \
                 instant the introduction lands, which is a debt that was never dischargeable",
                update_after.num_seconds()
            );
        }
        Ok(Self { update_after })
    }

    pub fn update_after(&self) -> Duration {
        self.update_after
    }
}

/// What we owe an introducer, and why.
///
/// No `PartialEq`: [`RecordObligation`] does not derive one, deliberately — a
/// request carrying a `due_at` is compared by the store through its derived id,
/// not field by field.
#[derive(Debug, Clone)]
pub struct IntroducerDebt {
    /// The introducer in the spelling their first introduction used — what an
    /// owner reads. The grouping key is [`introducer_key`].
    pub introducer: String,
    /// Every introduction this debt covers. One obligation per introducer, not
    /// one per introduction: the thing owed is *an update*, and four of them to
    /// the same person on the same day is the behaviour this module exists to
    /// prevent rather than to automate.
    pub covers: Vec<Introduction>,
    /// The obligation to record, if the caller decides to.
    pub obligation: RecordObligation,
}

/// What each introducer is owed, as of `now`.
///
/// An introducer is owed an update when the **most recent** thing that happened
/// on any introduction they made is older than the policy window. Most recent,
/// not oldest: somebody who introduced us in March and again last week does not
/// need an update about March, and a derivation that used the oldest would tell
/// them so every time they helped again.
///
/// # The debt is owed to a PERSON, and that is not a formality
///
/// [`AudienceKind::Person`] — *"one named individual, their own records, their
/// own offer"* — keyed by [`introducer_key`].
///
/// The first cut filed it under [`AudienceKind::Engagement`] with the
/// introducer's address as the engagement id, which claims a bilateral
/// engagement nobody opened and keys the obligation log under a relationship
/// that does not exist. It was the exact error the code's own comment warned
/// about: *a debt filed against a guessed audience is a debt against somebody
/// else.*
///
/// Nothing needs the counterparty register for this. Whether we can REACH Sarah
/// is a different question from whether we owe her an update, and the obligation
/// register records what is owed rather than how to deliver it. So there is no
/// resolution to fail and nothing is ever `unaddressable`: the key is the
/// introducer, two spellings collapse to one log, and a person we have never
/// recorded is still owed what they are owed.
pub fn introducer_debts(
    graph: &ReferralGraph,
    policy: IntroducerPolicy,
    created_by: &str,
    now: DateTime<Utc>,
) -> Vec<IntroducerDebt> {
    let mut owed = Vec::new();

    for (key, introductions) in &graph.by_introducer {
        let Some(latest) = introductions.iter().map(|held| held.last_seen).max() else {
            continue;
        };
        if now - latest < policy.update_after() {
            continue;
        }
        // Keyed on the comparison form and reported in the spelling. The log is
        // addressed by the key, so two spellings share one; an owner reading the
        // debt sees the introducer the way they wrote them down.
        let shown = introductions
            .first()
            .map(|held| held.introducer.clone())
            .unwrap_or_else(|| key.clone());
        let audience = AudienceRef::new(AudienceKind::Person, key);

        let names: Vec<&str> = introductions
            .iter()
            .map(|held| held.display_name.as_str())
            .collect();
        owed.push(IntroducerDebt {
            introducer: shown.clone(),
            covers: introductions.clone(),
            obligation: RecordObligation {
                audience,
                program_id: None,
                what: format!(
                    "Update {shown} on the {} they introduced: {}",
                    if introductions.len() == 1 {
                        "introduction".to_string()
                    } else {
                        format!("{} introductions", introductions.len())
                    },
                    names.join(", ")
                ),
                // Due NOW, not now + window. The window has already elapsed —
                // that is what put this row here — so a deadline in the future
                // would say the debt has not started when it is already late.
                due_at: now,
                direction: ObligationDirection::OwedByUs,
                created_by: created_by.to_string(),
                source_act_ref: None,
            },
        });
    }
    owed
}

#[cfg(test)]
mod tests;
