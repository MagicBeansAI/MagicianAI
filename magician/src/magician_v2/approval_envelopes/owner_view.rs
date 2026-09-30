//! What the owner can see and do about their envelopes — plan phase 4.
//!
//! *"An envelope you cannot see the state of is worse than a prompt, because you
//! no longer know what was done under it."*
//!
//! # A projection, not a screen
//!
//! This is the read model and its commands, with no HTTP, no serialisation
//! format beyond serde, and no assumption about who renders it. A CLI, an API
//! handler and a test all consume the same thing — which is what stops "what the
//! owner sees" drifting from "what the resolver decided" the moment a second
//! surface appears.
//!
//! It deliberately answers **four questions and no others**, because those are
//! the ones §4 and §7 say an owner must be able to answer:
//!
//! 1. what have I authorised?
//! 2. how much of it is left?
//! 3. what was actually done under it?
//! 4. how do I stop it?

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::store::{ApprovalEnvelopeStore, EnvelopeStoreScope};
use super::types::{
    ApprovalEnvelope, ConsumptionEntry, EnvelopeKind, EnvelopeScope, EnvelopeState,
};

/// Why an envelope is not currently usable, or that it is.
///
/// Ordered by how the owner should read it: `Active` is the only state that
/// authorises anything, and every other value means "this asks now".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeStanding {
    Active,
    Revoked,
    Expired,
    /// Every cap is spent. Distinct from `Expired` because the remedy differs:
    /// a spent envelope was used as intended, an expired one ran out of time.
    Exhausted,
}

impl EnvelopeStanding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Exhausted => "exhausted",
        }
    }

    pub fn authorises_anything(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// How much of a cap is left.
///
/// `remaining` is `None` when the cap is unset — *"no limit"*, which must not be
/// rendered as `0`. That confusion is the reason this is a type rather than a
/// bare number: an owner shown "0 remaining" for an uncapped envelope would
/// revoke something that was working correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Headroom {
    pub used: u64,
    pub limit: Option<u64>,
}

impl Headroom {
    pub fn remaining(self) -> Option<u64> {
        self.limit.map(|limit| limit.saturating_sub(self.used))
    }

    pub fn is_spent(self) -> bool {
        self.remaining().is_some_and(|left| left == 0)
    }
}

/// One envelope as the owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeSummary {
    pub envelope_id: String,
    pub scope: EnvelopeScope,
    /// The owner's own words for what they authorised. Never an input to a
    /// decision — this is the one place it is load-bearing.
    pub outcome: String,
    pub kind: String,
    pub covers: Vec<String>,
    pub standing: EnvelopeStanding,
    pub acts: Headroom,
    pub value_micros: Headroom,
    pub granted_by: String,
    pub granted_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// Distinct recipients reached under this envelope, so "who has it written
    /// to" is answerable without reading the ledger row by row.
    pub recipients_reached: Vec<String>,
    /// The boundary, rendered by name. Enough to see what the envelope is
    /// bounded BY without exposing the predicate internals to a renderer.
    pub boundary: Vec<String>,
}

/// One act performed under an envelope — §7's audit property, as a row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumptionRow {
    pub act_ref: String,
    pub at: DateTime<Utc>,
    pub recipients: Vec<String>,
    /// Which predicates matched. *"Why did it send that"* is answered here
    /// rather than reconstructed.
    pub matched_predicates: Vec<String>,
    pub value_micros: Option<u64>,
}

impl From<&ConsumptionEntry> for ConsumptionRow {
    fn from(entry: &ConsumptionEntry) -> Self {
        Self {
            act_ref: entry.act_ref.clone(),
            at: entry.at,
            recipients: entry.recipients.clone(),
            matched_predicates: entry.matched_predicates.clone(),
            value_micros: entry.value_micros,
        }
    }
}

/// An envelope and everything done under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeDetail {
    pub summary: EnvelopeSummary,
    pub consumed: Vec<ConsumptionRow>,
}

/// Read and revoke envelopes on the owner's behalf.
#[derive(Debug, Clone)]
pub struct OwnerView {
    store: ApprovalEnvelopeStore,
    scope: EnvelopeStoreScope,
}

impl OwnerView {
    pub fn new(store: ApprovalEnvelopeStore, scope: EnvelopeStoreScope) -> Self {
        Self { store, scope }
    }

    /// Grant an envelope.
    ///
    /// A thin pass-through to the store, and deliberately so: every rule about
    /// what may be granted — commitment never, standing over disclosure never,
    /// a standing envelope must expire, an empty batch covers nothing — lives in
    /// one place. Re-checking here would be a second opinion that can disagree,
    /// and the one that disagreed would be whichever a caller happened to reach.
    ///
    /// Returns the summary rather than the raw envelope so a surface renders
    /// grants and listings through the same shape.
    pub fn grant(
        &self,
        request: &super::store::GrantEnvelope,
        now: DateTime<Utc>,
    ) -> Result<EnvelopeSummary> {
        let envelope = self.store.grant(&self.scope, request, now)?;
        Ok(summarise(
            &EnvelopeState {
                envelope,
                consumed: Vec::new(),
            },
            now,
        ))
    }

    /// Every envelope granted against one scope, newest grant first.
    ///
    /// Revoked and expired envelopes are **included**. An owner asking "what did
    /// this thing do" after revoking it is the main reason to look, so filtering
    /// them out would remove the answer at the moment it is wanted.
    pub fn list(
        &self,
        envelope_scope: &EnvelopeScope,
        now: DateTime<Utc>,
    ) -> Result<Vec<EnvelopeSummary>> {
        let mut summaries: Vec<EnvelopeSummary> = self
            .store
            .load_for_scope(&self.scope, envelope_scope)?
            .iter()
            .map(|state| summarise(state, now))
            .collect();
        summaries.sort_by(|left, right| {
            right
                .granted_at
                .cmp(&left.granted_at)
                .then_with(|| left.envelope_id.cmp(&right.envelope_id))
        });
        Ok(summaries)
    }

    /// One envelope and its full consumption ledger, oldest act first.
    pub fn detail(&self, envelope_id: &str, now: DateTime<Utc>) -> Result<Option<EnvelopeDetail>> {
        let Some(state) = self.store.load(&self.scope, envelope_id)? else {
            return Ok(None);
        };
        let mut consumed: Vec<ConsumptionRow> =
            state.consumed.iter().map(ConsumptionRow::from).collect();
        consumed.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.act_ref.cmp(&right.act_ref))
        });
        Ok(Some(EnvelopeDetail {
            summary: summarise(&state, now),
            consumed,
        }))
    }

    /// Withdraw an envelope.
    ///
    /// §7: takes effect on the next act, with nothing to clean up, because
    /// nothing was ever copied out of it. Prior consumption stays — that record
    /// is exactly what an owner wants after revoking.
    ///
    /// Idempotent: revoking twice returns the state rather than failing, so a
    /// double-click cannot produce an error the owner has to interpret.
    pub fn revoke(
        &self,
        envelope_id: &str,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<EnvelopeSummary> {
        let state = self.store.revoke(&self.scope, envelope_id, by, now)?;
        Ok(summarise(&state, now))
    }
}

/// Project one folded envelope into the owner's view.
///
/// Standing is **derived** rather than stored, so it cannot disagree with what
/// the resolver would decide: both read the same limits against the same clock.
/// A stored status would be a second source of truth that drifts the moment an
/// envelope expires without anyone writing to it.
pub fn summarise(state: &EnvelopeState, now: DateTime<Utc>) -> EnvelopeSummary {
    let envelope: &ApprovalEnvelope = &state.envelope;

    let acts = Headroom {
        used: u64::from(state.acts_used()),
        limit: envelope.limits.max_acts.map(u64::from),
    };
    let value_micros = Headroom {
        used: state.value_used_micros(),
        limit: envelope.limits.max_value_micros,
    };

    // Order matters: an envelope can be several of these at once, and the owner
    // needs the reason that will not change by waiting. Revocation is a
    // decision, expiry is a fact about the clock, exhaustion is a fact about
    // use — reported in that order.
    let standing = if envelope.is_revoked() {
        EnvelopeStanding::Revoked
    } else if envelope.is_expired(now) {
        EnvelopeStanding::Expired
    } else if acts.is_spent() || value_micros.is_spent() || batch_is_exhausted(state) {
        EnvelopeStanding::Exhausted
    } else {
        EnvelopeStanding::Active
    };

    let mut recipients_reached: Vec<String> = state
        .consumed
        .iter()
        .flat_map(|entry| entry.recipients.iter().cloned())
        .collect();
    recipients_reached.sort();
    recipients_reached.dedup();

    EnvelopeSummary {
        envelope_id: envelope.envelope_id.clone(),
        scope: envelope.scope.clone(),
        outcome: envelope.outcome.clone(),
        kind: envelope.kind.label().to_string(),
        covers: envelope
            .covers
            .iter()
            .map(|class| class.as_str().to_string())
            .collect(),
        standing,
        acts,
        value_micros,
        granted_by: envelope.granted_by.clone(),
        granted_at: envelope.granted_at,
        expires_at: envelope.limits.expires_at,
        revoked_at: envelope.revoked_at,
        recipients_reached,
        boundary: envelope
            .boundary
            .iter()
            .map(|predicate| predicate.name().to_string())
            .collect(),
    }
}

/// A reviewed batch is exhausted once every instance it named has been reached.
///
/// Without this a batch would read `Active` forever, because it has no act cap —
/// and an owner would be told a batch is live when nothing remains that it could
/// authorise.
fn batch_is_exhausted(state: &EnvelopeState) -> bool {
    let EnvelopeKind::ReviewedBatch { instances } = &state.envelope.kind else {
        return false;
    };
    instances.iter().all(|instance| {
        state.consumed.iter().any(|entry| {
            entry
                .recipients
                .iter()
                .any(|had| *had == instance.recipient)
        })
    })
}
