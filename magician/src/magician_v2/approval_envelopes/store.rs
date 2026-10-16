//! Durable envelopes and their consumption ledger — plan §4.
//!
//! An envelope's file is its log: the first line is the grant, each later line a
//! consumption or a revocation, and the current state is the fold of them.
//! Nothing is edited in place, so revocation is a successor rather than a
//! rewrite and §7's audit property survives — *"every outward act names the
//! envelope that authorised it"* is only true if the envelope's history cannot
//! be edited afterwards to say something else.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::ConsequenceClass;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::types::{
    ApprovalEnvelope, ConsumptionEntry, EnvelopeKind, EnvelopeLimits, EnvelopeScope, EnvelopeState,
};

/// Scope for a store call — the principal and workspace the envelopes live under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeStoreScope {
    pub principal: String,
    pub workspace: String,
}

impl EnvelopeStoreScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// What a caller supplies to grant. Deliberately not the envelope itself:
/// `envelope_id` and `revoked_at` are the store's to decide, so a caller cannot
/// mint an id that collides with another grant or hand in a pre-revoked row.
#[derive(Debug, Clone, Serialize)]
pub struct GrantEnvelope {
    pub scope: EnvelopeScope,
    pub outcome: String,
    pub kind: EnvelopeKind,
    pub covers: Vec<ConsequenceClass>,
    pub limits: EnvelopeLimits,
    pub boundary: Vec<super::types::BoundaryPredicate>,
    pub granted_by: String,
}

/// One line in an envelope's log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum EnvelopeRecord {
    Granted(ApprovalEnvelope),
    Consumed(ConsumptionEntry),
    Revoked { at: DateTime<Utc>, by: String },
}

/// Durable store for approval envelopes.
#[derive(Debug, Clone)]
pub struct ApprovalEnvelopeStore {
    workspace_layout: ArtifactV2Workspace,
}

impl ApprovalEnvelopeStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn lock(
        &self,
        scope: &EnvelopeStoreScope,
    ) -> Result<crate::magician_v2::agents::storage::FileLockGuard> {
        ensure!(
            magicllm::LlmScope::new(&scope.principal, &scope.workspace).is_valid(),
            "invalid envelope store scope"
        );
        Ok(
            crate::magician_v2::agents::AgentStorage::acquire_file_lock_exclusive_sync(
                &self.root(scope).join("authority.json"),
            )?,
        )
    }

    /// Resolve and reserve from the same locked authority snapshot. Grant,
    /// revocation and every debit use this lock, so parallel callers cannot
    /// spend the same remaining slot or reserve after a committed revocation.
    pub fn resolve_and_reserve(
        &self,
        scope: &EnvelopeStoreScope,
        envelope_scope: &EnvelopeScope,
        facts: &super::types::ActFacts<'_>,
    ) -> Result<super::types::EnvelopeDecision> {
        let _lock = self.lock(scope)?;
        let states = self.load_for_scope(scope, envelope_scope)?;
        let decision = super::resolver::resolve_any(&states, facts);
        if let super::types::EnvelopeDecision::Covered {
            envelope_id,
            matched_predicates,
            ..
        } = &decision
        {
            self.record_consumption_unlocked(
                scope,
                envelope_id,
                &ConsumptionEntry {
                    act_ref: facts.act_ref.to_owned(),
                    content_ref: Some(facts.effective.content_ref.clone()),
                    at: facts.now,
                    recipients: facts.effective.recipients.clone(),
                    matched_predicates: matched_predicates.clone(),
                    value_micros: facts.value_micros,
                },
            )?;
        }
        Ok(decision)
    }

    fn root(&self, scope: &EnvelopeStoreScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("approval_envelopes")
    }

    fn envelope_path(&self, scope: &EnvelopeStoreScope, envelope_id: &str) -> PathBuf {
        self.root(scope)
            .join("envelopes")
            .join(format!("{envelope_id}.jsonl"))
    }

    /// Which envelopes exist for one scope key. Written when the envelope is,
    /// not derived by scanning: resolution happens on every outward dispatch, so
    /// a directory walk would be on the hot path.
    fn scope_index_path(&self, scope: &EnvelopeStoreScope, scope_key: &str) -> PathBuf {
        self.root(scope)
            .join("by_scope")
            .join(format!("{}.jsonl", stable_id(scope_key)))
    }

    /// Grant an envelope.
    ///
    /// Refuses at the door anything the taxonomy says can never be covered, so
    /// an impossible envelope cannot sit in the store looking authoritative. The
    /// resolver checks the same rule again; that duplication is deliberate,
    /// because a store written by an older binary must not become enforceable by
    /// a newer one.
    pub fn grant(
        &self,
        scope: &EnvelopeStoreScope,
        request: &GrantEnvelope,
        now: DateTime<Utc>,
    ) -> Result<ApprovalEnvelope> {
        let _lock = self.lock(scope)?;
        if request.covers.is_empty() {
            anyhow::bail!("an envelope that covers nothing is not a grant, it is a decoration");
        }
        for class in &request.covers {
            if !class.requires_gate() {
                anyhow::bail!(
                    "`{}` needs no gate, so an envelope for it would authorise nothing",
                    class.as_str()
                );
            }
            if !request.kind.may_cover(*class) {
                anyhow::bail!(
                    "a {} envelope may never cover `{}`",
                    request.kind.label(),
                    class.as_str()
                );
            }
        }
        // §9: *"expiry degrades to asking"* and envelopes must be *"scoped to one
        // goal/program/engagement, expiring"*. A STANDING envelope is consent to
        // acts the owner has not seen, so one that never expires is precisely the
        // blanket yes the risk table exists to prevent. A reviewed batch is
        // self-limiting — exhausted by its own list — so an expiry is optional
        // there.
        if matches!(request.kind, EnvelopeKind::Standing) && request.limits.expires_at.is_none() {
            anyhow::bail!(
                "a standing envelope must expire: it is consent to acts the owner has not seen, \
                 and one that never lapses is a blanket yes rather than a bounded grant"
            );
        }
        if let EnvelopeKind::ReviewedBatch { instances } = &request.kind {
            if instances.is_empty() {
                anyhow::bail!(
                    "a reviewed batch is exhausted by its own list, so an empty list covers \
                     nothing — and an empty list that read as `covers everything` would be the \
                     expensive way to be wrong"
                );
            }
            ensure!(
                instances.iter().all(|instance| instance
                    .content_ref
                    .as_deref()
                    .is_some_and(valid_content_ref)),
                "every reviewed batch instance needs its exact runtime-computed content ref"
            );
        }
        for predicate in &request.boundary {
            if let super::types::BoundaryPredicate::ExactContent { content_refs } = predicate {
                ensure!(
                    !content_refs.is_empty()
                        && content_refs
                            .iter()
                            .all(|content| valid_content_ref(content)),
                    "exact content boundary requires valid nonempty content refs"
                );
            }
        }

        let envelope_id = derive_envelope_id(scope, request, now);
        if let Some(existing) = self.load(scope, &envelope_id)? {
            self.append_scope_index(scope, &request.scope.as_key(), &envelope_id)?;
            return Ok(existing.envelope);
        }

        let envelope = ApprovalEnvelope {
            envelope_id: envelope_id.clone(),
            scope: request.scope.clone(),
            outcome: request.outcome.clone(),
            kind: request.kind.clone(),
            covers: request.covers.clone(),
            limits: request.limits.clone(),
            boundary: request.boundary.clone(),
            granted_by: request.granted_by.clone(),
            granted_at: now,
            revoked_at: None,
        };

        let path = self.envelope_path(scope, &envelope_id);
        self.append(&path, &EnvelopeRecord::Granted(envelope.clone()))?;
        self.append_scope_index(scope, &request.scope.as_key(), &envelope_id)?;
        Ok(envelope)
    }

    /// Debit one act against an envelope.
    ///
    /// Idempotent on `act_ref`: a retry of the same act returns without writing
    /// a second entry, so a resend cannot consume an envelope twice. That is the
    /// same property the outward assertions store gets from its path; here it
    /// cannot be a path, because the ledger is a log, so it is a fold check.
    pub fn record_consumption(
        &self,
        scope: &EnvelopeStoreScope,
        envelope_id: &str,
        entry: &ConsumptionEntry,
    ) -> Result<EnvelopeState> {
        let _lock = self.lock(scope)?;
        self.record_consumption_unlocked(scope, envelope_id, entry)
    }

    fn record_consumption_unlocked(
        &self,
        scope: &EnvelopeStoreScope,
        envelope_id: &str,
        entry: &ConsumptionEntry,
    ) -> Result<EnvelopeState> {
        let Some(state) = self.load(scope, envelope_id)? else {
            anyhow::bail!("no envelope `{envelope_id}` to debit");
        };
        ensure!(
            !state.envelope.is_revoked() && !state.envelope.is_expired(entry.at),
            "envelope revoked or expired before reservation"
        );
        ensure!(
            !entry.act_ref.trim().is_empty(),
            "envelope debit needs an exact act identity"
        );
        ensure!(
            entry.content_ref.as_deref().is_some_and(valid_content_ref),
            "envelope debit needs its exact dispatch content ref"
        );
        if let Some(existing) = state
            .consumed
            .iter()
            .find(|existing| existing.act_ref == entry.act_ref)
        {
            ensure!(
                existing.recipients == entry.recipients
                    && existing.content_ref == entry.content_ref
                    && existing.value_micros == entry.value_micros
                    && existing.matched_predicates == entry.matched_predicates,
                "act identity reused for a different envelope debit"
            );
            return Ok(state);
        }

        // Re-check the act cap against state read HERE, not against what the
        // resolver saw.
        //
        // The shared authority lock covers the read, checks and durable append.
        if let Some(max_acts) = state.envelope.limits.max_acts {
            if state.acts_used() >= max_acts {
                anyhow::bail!(
                    "envelope `{envelope_id}` is spent ({}/{max_acts} acts): it was covered on an \
                     earlier read and consumed in between",
                    state.acts_used()
                );
            }
        }
        // The same locked snapshot supplies recipient and value headroom.
        if let Some(cap) = state.envelope.limits.per_recipient_cap {
            for recipient in &entry.recipients {
                if state.acts_for_recipient(recipient) >= cap {
                    anyhow::bail!(
                        "envelope `{envelope_id}` is spent for recipient `{recipient}` \
                         ({}/{cap} acts): it was covered on an earlier read and consumed in \
                         between",
                        state.acts_for_recipient(recipient)
                    );
                }
            }
        }
        if let Some(ceiling) = state.envelope.limits.max_value_micros {
            let prospective = state
                .value_used_micros()
                .saturating_add(entry.value_micros.unwrap_or(0));
            if prospective > ceiling {
                anyhow::bail!(
                    "envelope `{envelope_id}` would exceed its value ceiling \
                     ({prospective}/{ceiling} micros): it was covered on an earlier read and \
                     consumed in between"
                );
            }
        }
        let path = self.envelope_path(scope, envelope_id);
        self.append(&path, &EnvelopeRecord::Consumed(entry.clone()))?;
        self.load(scope, envelope_id)?
            .context("envelope vanished immediately after being debited")
    }

    /// Revoke an envelope.
    ///
    /// §7: takes effect on the next act, with nothing to clean up, because
    /// nothing was ever copied out of the envelope. Prior consumption stays in
    /// the log — what was done under it is exactly what an owner needs to see
    /// after revoking.
    pub fn revoke(
        &self,
        scope: &EnvelopeStoreScope,
        envelope_id: &str,
        by: &str,
        now: DateTime<Utc>,
    ) -> Result<EnvelopeState> {
        let _lock = self.lock(scope)?;
        let Some(state) = self.load(scope, envelope_id)? else {
            anyhow::bail!("no envelope `{envelope_id}` to revoke");
        };
        if state.envelope.is_revoked() {
            return Ok(state);
        }
        let path = self.envelope_path(scope, envelope_id);
        self.append(
            &path,
            &EnvelopeRecord::Revoked {
                at: now,
                by: by.to_string(),
            },
        )?;
        self.load(scope, envelope_id)?
            .context("envelope vanished immediately after being revoked")
    }

    /// The folded state of one envelope.
    pub fn load(
        &self,
        scope: &EnvelopeStoreScope,
        envelope_id: &str,
    ) -> Result<Option<EnvelopeState>> {
        ensure!(
            magicllm::LlmScope::new(&scope.principal, &scope.workspace).is_valid(),
            "invalid envelope store scope"
        );
        ensure!(
            !envelope_id.trim().is_empty()
                && envelope_id != "."
                && envelope_id != ".."
                && envelope_id.len() <= 255
                && !envelope_id
                    .chars()
                    .any(|c| c.is_control() || c == '/' || c == '\\'),
            "invalid envelope identity"
        );
        let path = self.envelope_path(scope, envelope_id);
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(None);
        };

        let mut envelope: Option<ApprovalEnvelope> = None;
        let mut consumed: Vec<ConsumptionEntry> = Vec::new();
        // A set rather than a linear scan per line: an envelope with no
        // `max_acts` has an unbounded ledger, and this fold runs on every
        // outward dispatch. Quadratic here would degrade silently with use,
        // which is the worst shape for a performance bug.
        let mut debited: HashMap<String, usize> = HashMap::new();

        // Tolerant of a torn tail only — see `magician_v2::jsonl`.
        for record in crate::magician_v2::jsonl::parse_log_lines::<EnvelopeRecord>(&raw, &path)? {
            match record {
                EnvelopeRecord::Granted(granted) => envelope = Some(granted),
                EnvelopeRecord::Consumed(entry) => {
                    // Defensive: a duplicate line cannot double-count even if one
                    // were written by an older binary that did not check.
                    if let Some(index) = debited.get(&entry.act_ref) {
                        let existing = &consumed[*index];
                        ensure!(
                            existing.recipients == entry.recipients
                                && existing.content_ref == entry.content_ref
                                && existing.value_micros == entry.value_micros
                                && existing.matched_predicates == entry.matched_predicates,
                            "conflicting duplicate envelope act binding"
                        );
                    } else {
                        debited.insert(entry.act_ref.clone(), consumed.len());
                        consumed.push(entry);
                    }
                },
                EnvelopeRecord::Revoked { at, .. } => {
                    if let Some(current) = envelope.as_mut() {
                        current.revoked_at = Some(at);
                    }
                },
            }
        }

        Ok(envelope.map(|envelope| EnvelopeState { envelope, consumed }))
    }

    /// Every envelope granted against one scope, folded.
    ///
    /// Revoked and expired envelopes are returned rather than filtered: the
    /// resolver reports *why* an act is not covered, and an owner told "no
    /// envelope" when theirs is merely expired goes looking in the wrong place.
    pub fn load_for_scope(
        &self,
        scope: &EnvelopeStoreScope,
        envelope_scope: &EnvelopeScope,
    ) -> Result<Vec<EnvelopeState>> {
        let path = self.scope_index_path(scope, &envelope_scope.as_key());
        let Some(raw) = self.read_if_present(&path)? else {
            return Ok(Vec::new());
        };

        let mut seen = BTreeSet::new();
        let mut states = Vec::new();
        for envelope_id in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if !seen.insert(envelope_id.to_string()) {
                continue;
            }
            if let Some(state) = self.load(scope, envelope_id)? {
                states.push(state);
            }
        }
        Ok(states)
    }

    fn append(&self, path: &PathBuf, record: &EnvelopeRecord) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }

    fn append_scope_index(
        &self,
        scope: &EnvelopeStoreScope,
        scope_key: &str,
        envelope_id: &str,
    ) -> Result<()> {
        let path = self.scope_index_path(scope, scope_key);
        let mut line = envelope_id.as_bytes().to_vec();
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending scope index {}", path.display()))?;
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

fn valid_content_ref(value: &str) -> bool {
    value
        .strip_prefix("parameters://blake3:")
        .is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
}

/// The id for one grant.
///
/// Folds in the grant instant and every authority field, so re-granting
/// the same outcome after an envelope expired produces a NEW envelope rather
/// than resuming the spent one — an expiring authority that could be revived by
/// re-issuing the same words would not be an expiry.
fn derive_envelope_id(
    scope: &EnvelopeStoreScope,
    request: &GrantEnvelope,
    now: DateTime<Utc>,
) -> String {
    format!(
        "env-{}",
        stable_id(
            &serde_json::to_string(&(&scope.principal, &scope.workspace, request, now))
                .expect("typed envelope grant serializes")
        )
    )
}
