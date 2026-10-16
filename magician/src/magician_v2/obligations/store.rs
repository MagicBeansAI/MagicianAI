//! The obligation register — Module D.
//!
//! Append-only per audience: the register for a relationship is one log, so
//! *"what do we owe these people"* is one read rather than a scan.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::AudienceRef;

use super::types::{Obligation, ObligationDirection, RecordObligation, Settlement};

const FIELD_SEP: char = '\u{1f}';

/// Scope for a store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObligationScope {
    pub principal: String,
    pub workspace: String,
}

impl ObligationScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// One line in an engagement's register.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum ObligationRecord {
    Created(Obligation),
    Settled {
        obligation_id: String,
        at: DateTime<Utc>,
        settlement: Settlement,
    },
}

/// What is owed, per engagement.
#[derive(Debug, Clone)]
pub struct ObligationStore {
    workspace_layout: ArtifactV2Workspace,
}

impl ObligationStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &ObligationScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("obligations")
    }

    fn register_path(&self, scope: &ObligationScope, audience: &AudienceRef) -> PathBuf {
        self.root(scope)
            .join(format!("{}.jsonl", stable_id(&audience.as_key())))
    }

    /// Record a promise.
    ///
    /// Idempotent on `(engagement, what, due_at, direction)`. The same promise
    /// noticed twice — a transcript reprocessed, a poller re-reading a thread —
    /// is one obligation. Without that the register fills with duplicates of the
    /// same commitment and stops being readable, which is the failure mode that
    /// makes people ignore a to-do list.
    pub fn record(
        &self,
        scope: &ObligationScope,
        request: &RecordObligation,
        now: DateTime<Utc>,
    ) -> Result<Obligation> {
        if !request.audience.is_named() {
            anyhow::bail!("an obligation must name the relationship it belongs to");
        }
        if request.what.trim().is_empty() {
            anyhow::bail!(
                "an obligation with no description cannot be acted on; `what` is what the owner \
                 reads"
            );
        }

        let obligation_id = derive_obligation_id(scope, request);
        if let Some(existing) = self.load(scope, &request.audience, &obligation_id)? {
            return Ok(existing);
        }

        let obligation = Obligation {
            obligation_id,
            audience: request.audience.clone(),
            program_id: request.program_id.clone(),
            what: request.what.clone(),
            due_at: request.due_at,
            direction: request.direction,
            created_at: now,
            created_by: request.created_by.clone(),
            source_act_ref: request.source_act_ref.clone(),
            settled_at: None,
            settlement: None,
        };
        self.append(
            &self.register_path(scope, &request.audience),
            &ObligationRecord::Created(obligation.clone()),
        )?;
        Ok(obligation)
    }

    /// Settle an obligation — done, or no longer applicable.
    ///
    /// Idempotent: the first settlement is the settlement. A later one is
    /// ignored rather than overwriting, because *"we did it on Tuesday"* is a
    /// fact, and letting a second call move the date would make the register
    /// unusable as evidence of what happened when.
    pub fn settle(
        &self,
        scope: &ObligationScope,
        audience: &AudienceRef,
        obligation_id: &str,
        settlement: Settlement,
        now: DateTime<Utc>,
    ) -> Result<Obligation> {
        let Some(obligation) = self.load(scope, audience, obligation_id)? else {
            anyhow::bail!("no obligation `{obligation_id}` on `{}`", audience.as_key());
        };
        if obligation.settlement.is_some() {
            return Ok(obligation);
        }
        self.append(
            &self.register_path(scope, audience),
            &ObligationRecord::Settled {
                obligation_id: obligation_id.to_string(),
                at: now,
                settlement,
            },
        )?;
        self.load(scope, audience, obligation_id)?
            .context("obligation vanished immediately after being settled")
    }

    /// Everything on one audience's register, oldest first.
    ///
    /// Settled obligations are **included**. What we promised and delivered is
    /// the record a counterparty relationship is built on, and hiding it would
    /// leave the register showing only failures.
    pub fn for_audience(
        &self,
        scope: &ObligationScope,
        audience: &AudienceRef,
    ) -> Result<Vec<Obligation>> {
        self.fold_register(&self.register_path(scope, audience))
    }

    /// The whole register: every obligation in the scope, across every
    /// relationship, soonest deadline first.
    ///
    /// [`Self::lapsed_across`] takes the audiences as a parameter and says why
    /// — rosters belong to whoever owns each relationship. That is right for a
    /// cycle reading *an agent's own book*, and wrong for the owner asking
    /// *"what is owed"*: a promise filed against a relationship missing from
    /// the supplied list is invisible, and the promise nobody remembered is
    /// the one this register exists to catch. Registers are named for a hash
    /// of the audience key, so only folding the directory can answer without a
    /// roster — every `Created` record carries its [`AudienceRef`] in full.
    ///
    /// Settled rows are **included**, exactly as [`Self::for_audience`]
    /// includes them: what was promised and delivered is the record a
    /// relationship is read from, and a register showing only failures is one
    /// people stop opening. Callers wanting only live rows filter on
    /// [`Obligation::is_outstanding`].
    ///
    /// # Absent is empty; unreadable is not
    ///
    /// A scope that has never recorded a promise has no directory, and that is
    /// the only condition that reads as *"nothing is owed"*. Every other
    /// listing failure propagates, and so does a register that will not fold —
    /// answering *"nothing is owed"* out of a disk fault is the single most
    /// reassuring wrong answer this store could give.
    ///
    /// A register that vanished between the listing and the read contributes
    /// nothing rather than failing the listing.
    pub fn all_obligations(&self, scope: &ObligationScope) -> Result<Vec<Obligation>> {
        let root = self.root(scope);
        let mut out = Vec::new();
        for path in crate::magician_v2::jsonl::list_log_paths(&self.workspace_layout, &root)? {
            out.extend(self.fold_register(&path)?);
        }
        out.sort_by(|left, right| {
            left.due_at
                .cmp(&right.due_at)
                .then_with(|| left.obligation_id.cmp(&right.obligation_id))
        });
        Ok(out)
    }

    /// The fold of one relationship's register, addressed by path.
    ///
    /// Shared by [`Self::for_audience`] and [`Self::all_obligations`] rather
    /// than copied, so the first-created-wins and first-settlement-wins rules
    /// have exactly one implementation: a second copy would drift, and the two
    /// reads would then disagree about the date something was done.
    fn fold_register(&self, path: &PathBuf) -> Result<Vec<Obligation>> {
        let Some(raw) = self.read_if_present(path)? else {
            return Ok(Vec::new());
        };

        let mut seen = BTreeSet::new();
        let mut out: Vec<Obligation> = Vec::new();
        // Tolerant of a torn tail only — see `magician_v2::jsonl`.
        for record in crate::magician_v2::jsonl::parse_log_lines::<ObligationRecord>(&raw, path)? {
            match record {
                ObligationRecord::Created(obligation) => {
                    if seen.insert(obligation.obligation_id.clone()) {
                        out.push(obligation);
                    }
                },
                ObligationRecord::Settled {
                    obligation_id,
                    at,
                    settlement,
                } => {
                    if let Some(held) = out
                        .iter_mut()
                        .find(|held| held.obligation_id == obligation_id)
                    {
                        // First settlement wins, defensively as well as at the
                        // write: a duplicate line from an older binary must not
                        // move the date something was done.
                        if held.settlement.is_none() {
                            held.settled_at = Some(at);
                            held.settlement = Some(settlement);
                        }
                    }
                },
            }
        }
        Ok(out)
    }

    /// One obligation.
    pub fn load(
        &self,
        scope: &ObligationScope,
        audience: &AudienceRef,
        obligation_id: &str,
    ) -> Result<Option<Obligation>> {
        Ok(self
            .for_audience(scope, audience)?
            .into_iter()
            .find(|held| held.obligation_id == obligation_id))
    }

    /// What is still owed on one audience, soonest deadline first.
    ///
    /// Ordered by `due_at` rather than by creation, because the question an
    /// agent's cycle asks is *"what is closest to being late"* — and lapsed
    /// items sort first for free, since their deadline is already behind.
    pub fn outstanding(
        &self,
        scope: &ObligationScope,
        audience: &AudienceRef,
        now: DateTime<Utc>,
    ) -> Result<Vec<Obligation>> {
        let mut out: Vec<Obligation> = self
            .for_audience(scope, audience)?
            .into_iter()
            .filter(|obligation| obligation.is_outstanding(now))
            .collect();
        out.sort_by(|left, right| {
            left.due_at
                .cmp(&right.due_at)
                .then_with(|| left.obligation_id.cmp(&right.obligation_id))
        });
        Ok(out)
    }

    /// Everything past its deadline and unsettled, soonest-due first.
    ///
    /// This is what an agent's cycle surfaces and what escalates. Split by
    /// direction at the call site: a promise **we** broke and a reply **they**
    /// owe need different words and different urgency, and one list that reads
    /// alike for both would train the owner to skim it.
    pub fn lapsed(
        &self,
        scope: &ObligationScope,
        audience: &AudienceRef,
        direction: Option<ObligationDirection>,
        now: DateTime<Utc>,
    ) -> Result<Vec<Obligation>> {
        Ok(self
            .outstanding(scope, audience, now)?
            .into_iter()
            .filter(|obligation| obligation.overdue_by(now).is_some())
            .filter(|obligation| direction.is_none_or(|want| obligation.direction == want))
            .collect())
    }

    /// Everything lapsed across several engagements, soonest-due first.
    ///
    /// §6: an obligation *"surfaces on the owning agent's cycle"*, and a cycle
    /// asks about the agent's whole book rather than one counterparty. A
    /// per-engagement read alone would leave the caller looping and re-sorting,
    /// which is where an inconsistent order comes from.
    ///
    /// The audience list is **supplied**, not discovered: rosters belong to
    /// whoever owns each relationship, and taking a dependency on one for a read
    /// this simple would couple this module to a subsystem it does not need. The
    /// same decoupling the envelope gate uses.
    ///
    /// Unknown audiences contribute nothing rather than failing — a cycle handed
    /// a stale list should still surface what it can. Kinds may be mixed freely:
    /// a book is what a client owes, what a cohort is owed, and what a panel is
    /// waiting on.
    ///
    /// # No production caller, stated rather than implied
    ///
    /// The route an agent's cycle actually reads the register by is
    /// `GET /api/magician/v2/work/obligations`, and it goes through
    /// [`Self::all_obligations`] or [`Self::for_audience`] — precisely because
    /// it has no roster to supply. This is the roster-bearing form, kept for a
    /// caller that holds one; it is reached today only from tests, and saying
    /// so here is cheaper than a reader inferring a live path from a doc
    /// comment.
    pub fn lapsed_across(
        &self,
        scope: &ObligationScope,
        audiences: &[AudienceRef],
        direction: Option<ObligationDirection>,
        now: DateTime<Utc>,
    ) -> Result<Vec<Obligation>> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for audience in audiences {
            for obligation in self.lapsed(scope, audience, direction, now)? {
                // A duplicated audience in the caller's list must not produce a
                // doubled to-do item.
                if seen.insert(obligation.obligation_id.clone()) {
                    out.push(obligation);
                }
            }
        }
        out.sort_by(|left, right| {
            left.due_at
                .cmp(&right.due_at)
                .then_with(|| left.obligation_id.cmp(&right.obligation_id))
        });
        Ok(out)
    }

    fn append(&self, path: &PathBuf, record: &ObligationRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn read_if_present(&self, path: &PathBuf) -> Result<Option<String>> {
        // NotFound is the only error that reads as an empty store. Everything
        // else propagates: an unreadable log folded to "empty" fails open —
        // guards pass vacuously and removals report success while removing
        // nothing. Shared semantics live in `magician_v2::jsonl`.
        crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, path)
    }
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The register's id for a request — the settlement handle for emitters.
///
/// Ids are derived, never assigned, so `record` needs no handle back — but a
/// consumer that emitted a derived obligation (a sweep phrasing attention
/// signals as follow-ups) holds nothing to settle it with later. When its
/// *basis* changes — the counterparty replied, a fresh visit superseded the
/// old silence — the emitter rebuilds the OLD request tuple, obtains here the
/// id it recorded under, and settles that row. Guaranteed equal to the id
/// `record` derives for the same scope and request: both delegate to the same
/// private derivation, so they cannot drift.
pub fn obligation_id_for(scope: &ObligationScope, request: &RecordObligation) -> String {
    derive_obligation_id(scope, request)
}

/// The id for one promise.
///
/// Derived from `(audience, what, due_at, direction)` — the plan's own contract
/// tuple — so noticing the same promise twice yields one obligation.
/// `what` is normalised for whitespace and case, because the same commitment
/// re-extracted from a transcript rarely comes back character-identical.
fn derive_obligation_id(scope: &ObligationScope, request: &RecordObligation) -> String {
    let what = request
        .what
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "obl-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal,
            scope.workspace,
            request.audience.as_key(),
            what.to_ascii_lowercase(),
            format!(
                "{}{FIELD_SEP}{}",
                request.due_at.to_rfc3339(),
                request.direction.as_str()
            ),
        ))
    )
}
