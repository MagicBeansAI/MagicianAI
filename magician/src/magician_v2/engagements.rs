//! Engagement authority — capability as a property of (agent, context).
//!
//! OPC Workstream B, Phase 0: the authority carrier
//! (`docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §4.2c).
//! Capability today is a static property of an agent; an engagement makes it
//! a bounded, revocable, auditable property of the *(agent, context)* pair —
//! granted when outward work begins, expiring by default, revoked in one act.
//!
//! This module owns the authority record and its store. It deliberately does
//! NOT own routing, identities, or the outward actor (§5A) — those are later
//! phases. What ships here is exactly what the §4.2c adversarial proof matrix
//! needs to exist before any engagement-scoped live action is possible:
//!
//! - an engagement with a **tool ceiling** and a **delegation team**;
//! - an **authority revision** bumped on every mutation, so a snapshot taken
//!   at revision N is detectably stale after a narrow or revoke;
//! - **mandatory expiry** — an engagement is never a permanent privilege;
//! - a **fail-closed** live check: a store that cannot answer must read as
//!   "denied", never as "unrestricted" (§4.2c row 8).
//!
//! The security property the carrier must uphold is binary: until engagement
//! authority survives every delegation, resume, restart and nested child, no
//! engagement-scoped live action is enabled. The proof is the adversarial
//! matrix in `engagement_authority_carrier` tests, written before the carrier
//! and failing until it exists.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::io::write_bytes_durably;
// Generic in, specific out: the work-context type is dependency-free and this
// module consumes it. The reverse — `work_context` reaching for engagements —
// is the coupling that would make a generic primitive an OPC special case.
use crate::magician_v2::work_context::{WorkAuthorityRef, WorkContext, WorkContextKind};

/// Why an engagement does not authorize an action right now.
///
/// Deliberately more specific than a bool: the dispatch-side denial message
/// and the audit record both want the reason, and "expired" versus "revoked"
/// versus "store unavailable" are operationally different answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityDenial {
    /// No engagement with this id exists in this scope.
    UnknownEngagement,
    /// The engagement was revoked. Revocation is immediate: nothing was
    /// copied anywhere, so there is nothing to clean up (§7 of the plan).
    Revoked,
    /// `expires_at_ms` has passed. Expiry degrades to no authority, exactly
    /// like revocation, without an owner act.
    Expired,
    /// The tool is outside the engagement's ceiling.
    ToolOutsideCeiling { tool: String },
    /// The delegation target is outside `team[]`.
    TargetOutsideTeam { agent_id: String },
    /// The authority store could not answer. **Fails closed** (§4.2c row 8):
    /// an unavailable store must never read as unrestricted.
    StoreUnavailable { detail: String },
}

/// The carrier: what an engagement-scoped execution actually carries.
///
/// Two fields, by design. The id names the authority; the revision pins the
/// state of it that the execution's policy snapshot was resolved against.
/// A child execution inherits this pair verbatim and can never supply its
/// own (§4.2c row 5) — a child that could name its own engagement could
/// grant itself one. The dispatch boundary compares the carried revision
/// with the store's current one and re-resolves on mismatch (row 9).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EngagementAuthorityRef {
    pub engagement_id: String,
    pub authority_revision: u64,
}

/// Specific → generic: what the durable execution record actually stores.
///
/// The direction matters. This module may name `work_context`; `work_context`
/// may never name this module, or the generic carrier would be an OPC type
/// wearing a generic name and a second flow could not use it.
impl From<&EngagementAuthorityRef> for WorkAuthorityRef {
    fn from(carried: &EngagementAuthorityRef) -> Self {
        Self {
            work: WorkContextKind::Engagement(carried.engagement_id.clone()),
            authority_revision: carried.authority_revision,
        }
    }
}

/// Generic → specific, and **only** for the arm this module owns.
///
/// The engagement-scoped dispatch checks take an [`EngagementAuthorityRef`],
/// so a durable carrier has to be narrowed back to one before it can be
/// enforced. Every other arm is an error rather than a `None`: a caller that
/// received "no engagement" for a program-scoped run would proceed as an
/// unbound run, which is the fail-open shape this whole carrier exists to
/// prevent. The refusal names the missing piece so the reader knows the run
/// was stopped, not silently widened.
impl TryFrom<&WorkAuthorityRef> for EngagementAuthorityRef {
    type Error = String;

    fn try_from(carried: &WorkAuthorityRef) -> Result<Self, Self::Error> {
        match &carried.work {
            WorkContextKind::Engagement(engagement_id) => Ok(Self {
                engagement_id: engagement_id.clone(),
                authority_revision: carried.authority_revision,
            }),
            WorkContextKind::Program(program_id) => Err(format!(
                "`program:{program_id}` cannot be enforced at the engagement dispatch boundary: \
                 that boundary reads a ceiling from the engagement roster and no roster owns \
                 programs yet. Refused rather than read as no authority, because an unenforced \
                 carrier and an absent one are indistinguishable downstream"
            )),
        }
    }
}

/// The durable engagement record.
///
/// One per (program, counterparty) by convention (§7 "grant sprawl"), scoped
/// to a principal/workspace like every other authority in this system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngagementAuthority {
    pub engagement_id: String,
    pub principal: String,
    pub workspace: String,
    /// The program this engagement serves (e.g. a fundraising program id).
    pub program_id: String,
    /// Owner-readable counterparty label. Identity machinery is a later
    /// phase; the carrier needs only something reviewable.
    pub counterparty: String,
    /// The agent that fronts this engagement — who an inbound message from a
    /// **proved** identity of this counterparty is forwarded to (§4.2 branch 2).
    ///
    /// `None` until an owner names one, and `None` means **no inbound lane**:
    /// an engagement can hold a ceiling, a team and an expiry and still not be
    /// a place traffic may land. That is why it is not an argument to
    /// [`EngagementStore::create`] with a fallback — a default here would be an
    /// automatic promotion, and §3.2 is explicit that research and affiliation
    /// propose while a deterministic fact or the owner grants.
    ///
    /// Set through [`EngagementStore::set_owner_agent`], which is set-once and
    /// bumps the revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent_id: Option<String>,
    /// The capability ceiling: tools an engagement-scoped execution may use.
    /// Effective capability is always an intersection — this ceiling never
    /// grants a tool the agent's own definition lacks.
    pub tool_ceiling: BTreeSet<String>,
    /// Delegation boundary: agent ids a scoped execution may delegate to.
    /// Nested delegation may narrow this set further, never widen it.
    pub team: BTreeSet<String>,
    /// Bumped on **every** mutation. A policy snapshot records the revision
    /// it was resolved against; a mismatch at dispatch means the snapshot is
    /// stale and must be re-resolved (§4.2c row 9).
    pub authority_revision: u64,
    pub created_at_ms: i64,
    /// Mandatory. An engagement is a grant for a piece of work, not a
    /// standing privilege; expiry degrades to Guest with no cleanup.
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<i64>,
}

impl EngagementAuthority {
    fn live_at(&self, now_ms: i64) -> Result<(), AuthorityDenial> {
        if self.revoked_at_ms.is_some() {
            return Err(AuthorityDenial::Revoked);
        }
        if now_ms >= self.expires_at_ms {
            return Err(AuthorityDenial::Expired);
        }
        Ok(())
    }
}

/// What a live authorization check answers with.
///
/// Carries the revision so the caller can compare against the revision its
/// snapshot was resolved under — a stale snapshot is re-resolved rather than
/// trusted (§4.2c row 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAuthority {
    pub engagement_id: String,
    pub authority_revision: u64,
    pub tool_ceiling: BTreeSet<String>,
    pub team: BTreeSet<String>,
}

/// The separator every derived id in this system is joined with.
///
/// An engagement id is a ULID and derives from nothing, but the two caller
/// strings recorded on an engagement both feed derivations elsewhere: the
/// counterparty label is hashed into a counterparty id by
/// `counterparties::counterparty_id_for`, and the program id is joined into work
/// and envelope scope keys. A component carrying this character can move the
/// boundary between two components, so two different records derive one id.
const FIELD_SEP: char = '\u{1f}';

/// A caller string this store is about to record.
///
/// Refused rather than sanitised. Trimming a separator out would record a value
/// the caller did not write, under an id derived from a third value, and nothing
/// downstream could tell which of the three was meant.
fn guard_recorded_component<'a>(label: &str, value: &'a str) -> std::io::Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} must not be blank; a blank component derives an id nobody meant"),
        ));
    }
    if trimmed.contains(FIELD_SEP) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{label} must not contain U+001F: it is the separator that keeps a derived id's \
                 components from bleeding into each other, and a value carrying it could fuse two \
                 records into one id"
            ),
        ));
    }
    Ok(trimmed)
}

/// The program id an engagement records for a piece of work.
///
/// The seam that lets an authority be minted from the **generic** carrier —
/// [`WorkContextKind`], which any flow already holds — rather than from an
/// engagement-shaped argument list only this module's vocabulary can fill in.
///
/// # Why this matches on the arms when `WorkContextKind::id()` exists
///
/// That accessor exists so a *guard* need not match, and a guard that matched
/// would silently stop covering the next variant somebody adds. This is not a
/// guard, it is a **mapping**, and the two want opposite shapes: an exhaustive
/// match here stops the build when a third kind of work appears, so somebody
/// decides what that kind means before its id is written into a field the owner
/// surface filters and displays as a program. That is the same call
/// `EngagementStanding::from_denial` makes for a new denial.
///
/// # Why the engagement arm is refused rather than mapped
///
/// The record has exactly one work field. An engagement id written into
/// `program_id` would list that engagement as a program on the owner surface,
/// and would merge with a program that happened to share the id — two different
/// authorities answering under one heading. A grant nested inside another
/// engagement is a real thing to want; it needs a parent field this record does
/// not have, and inventing one out of `program_id` would make the nesting
/// invisible exactly where an owner goes to look for it.
pub fn program_id_for_work(kind: &WorkContextKind) -> std::io::Result<&str> {
    match kind {
        WorkContextKind::Program(id) => guard_recorded_component("a program id", id),
        WorkContextKind::Engagement(id) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "work context `engagement:{id}` cannot be the work a new engagement serves: this \
                 record's only work field is `program_id`, so the id would be listed and filtered \
                 as a program and would merge with any program sharing it. A grant nested under \
                 another engagement needs a parent field this record does not have"
            ),
        )),
    }
}

/// The roster of engagements for one data root.
///
/// Same storage discipline as the device stores this session hardened:
/// whole-file durable publish (unique temp, fsync, rename, parent sync) under
/// a write lock, tolerant absence, and every read of authority computed
/// against the clock at read time — liveness is never cached.
#[derive(Debug)]
pub struct EngagementStore {
    path: PathBuf,
    engagements: tokio::sync::RwLock<Vec<EngagementAuthority>>,
    write_lock: tokio::sync::Mutex<()>,
}

impl EngagementStore {
    /// Open the roster, tolerating its absence — no engagements yet is the
    /// correct state for a fresh install, not an error.
    ///
    /// A corrupt roster is a hard error, deliberately the opposite call from
    /// the device policy store: a policy file's safe fallback is its default,
    /// but an engagement roster's "default" would be *no restrictions
    /// recorded anywhere while executions still carry engagement ids* — and
    /// the fail-closed dispatch check would then deny everything with
    /// `UnknownEngagement`, which is safe but undebuggable. Better to refuse
    /// boot loudly and keep the file's history intact.
    pub async fn open(base_root: &Path) -> std::io::Result<Self> {
        let path = base_root.join("system").join("engagements.json");
        let engagements = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "engagement roster at {} is unreadable: {error}",
                        path.display()
                    ),
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            engagements: tokio::sync::RwLock::new(engagements),
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Mint a new engagement. The ceiling and team are fixed at creation for
    /// Phase 0; narrowing them later bumps the revision via [`Self::narrow`],
    /// and the window moves forward through [`Self::extend_expiry`].
    ///
    /// The engagement-shaped primitive. Callers that already hold a
    /// [`WorkContext`] should mint through [`Self::grant_for_work`] instead —
    /// same door, stated in the vocabulary every flow shares rather than in this
    /// module's.
    ///
    /// Three refusals live here rather than at any one caller, because this is
    /// the only function that writes a record: a blank or separator-carrying
    /// program id, the same for the counterparty label, and a window that is
    /// already over at `now_ms`. The last is the one worth naming — expiry is
    /// inclusive, so such an engagement authorises nothing from birth while
    /// still appearing in the roster as a grant, and an owner reading it would
    /// believe they had granted something.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        principal: &str,
        workspace: &str,
        program_id: &str,
        counterparty: &str,
        tool_ceiling: BTreeSet<String>,
        team: BTreeSet<String>,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> std::io::Result<EngagementAuthority> {
        // The single write door, so nothing reaches the roster unguarded
        // whichever entry point minted it.
        let program_id = guard_recorded_component("a program id", program_id)?;
        let counterparty = guard_recorded_component("a counterparty label", counterparty)?;
        if expires_at_ms <= now_ms {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "an engagement expiring at {expires_at_ms} is already over at {now_ms}: \
                     expiry is inclusive, so it would authorise nothing from the instant it was \
                     minted while still reading as a grant on the owner surface"
                ),
            ));
        }
        let engagement = EngagementAuthority {
            engagement_id: ulid::Ulid::new().to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            program_id: program_id.to_string(),
            counterparty: counterparty.to_string(),
            // No lane until an owner names the agent that fronts this
            // engagement. Minting one never opens a door.
            owner_agent_id: None,
            tool_ceiling,
            team,
            authority_revision: 1,
            created_at_ms: now_ms,
            expires_at_ms,
            revoked_at_ms: None,
        };
        let _guard = self.write_lock.lock().await;
        {
            let mut engagements = self.engagements.write().await;
            engagements.push(engagement.clone());
        }
        self.persist().await?;
        Ok(engagement)
    }

    /// Mint an engagement from a **work context**.
    ///
    /// [`Self::create`] takes a program id and a bag of tool names — the shape
    /// an engagement happens to have. This takes [`WorkContext`], the carrier
    /// the rest of the system already uses for *"what is this execution doing,
    /// and what does that work need"*, so a support triage, a recruiting loop or
    /// a vendor review can bound its authority through the same door without
    /// learning this module's vocabulary and without this module growing an
    /// entry point per flow.
    ///
    /// # The ceiling is the work's stated needs, and copying them widens nothing
    ///
    /// A work context *identifies* capabilities and never attaches them, and
    /// neither does a ceiling: effective capability stays the intersection with
    /// what the agent already holds. `work_binding_for_dispatch` reads a live
    /// ceiling back into a [`WorkContext`] at every dispatch; this is that
    /// mapping inverted, which is what keeps one description of the work rather
    /// than two that can disagree.
    ///
    /// `needs_playbooks` are folded in beside `needs_capabilities` because
    /// [`crate::magician_v2::work_context::resolve`] already treats them as one
    /// namespace — a ceiling that dropped them would refuse a procedure skill
    /// the work is explicitly for.
    ///
    /// # Names are recorded exactly as the work context spells them
    ///
    /// [`authorize_engagement_dispatch`] tests the dispatch's policy name
    /// against this set with `contains`: exact, case-sensitive membership. A
    /// ceiling holding `WebSearch` for a tool dispatched as `websearch`
    /// authorises nothing while reading as a grant. Note that
    /// [`WorkContext::needing`] lower-cases what it is given, so a caller
    /// building the context that way must name lower-cased tools; the HTTP
    /// surface checks every name against the capability registry before calling
    /// here, so what arrives already matches a dispatchable name exactly.
    #[allow(clippy::too_many_arguments)]
    pub async fn grant_for_work(
        &self,
        principal: &str,
        workspace: &str,
        work: &WorkContext,
        counterparty: &str,
        team: BTreeSet<String>,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> std::io::Result<EngagementAuthority> {
        let program_id = program_id_for_work(&work.kind)?;
        let tool_ceiling: BTreeSet<String> = work
            .needs_capabilities
            .iter()
            .chain(work.needs_playbooks.iter())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        self.create(
            principal,
            workspace,
            program_id,
            counterparty,
            tool_ceiling,
            team,
            now_ms,
            expires_at_ms,
        )
        .await
    }

    /// Revoke. Takes effect on the **next consequential dispatch** — the live
    /// check reads this store, so nothing needs cleaning up and no snapshot
    /// needs chasing (§4.2c's revocation row is the one "most likely to be
    /// got wrong, because it is the only one that fails a snapshot that was
    /// valid when it was taken").
    pub async fn revoke(&self, engagement_id: &str, now_ms: i64) -> std::io::Result<bool> {
        let _guard = self.write_lock.lock().await;
        let mutated = {
            let mut engagements = self.engagements.write().await;
            match engagements
                .iter_mut()
                .find(|entry| entry.engagement_id == engagement_id)
            {
                Some(entry) if entry.revoked_at_ms.is_none() => {
                    entry.revoked_at_ms = Some(now_ms);
                    entry.authority_revision += 1;
                    true
                },
                _ => false,
            }
        };
        if mutated {
            self.persist().await?;
        }
        Ok(mutated)
    }

    /// Narrow the ceiling and/or team. Widening is not offered: an engagement
    /// only ever shrinks after creation; broader authority is a new
    /// engagement with its own owner act.
    pub async fn narrow(
        &self,
        engagement_id: &str,
        tool_ceiling: Option<BTreeSet<String>>,
        team: Option<BTreeSet<String>>,
    ) -> std::io::Result<bool> {
        let _guard = self.write_lock.lock().await;
        let mutated = {
            let mut engagements = self.engagements.write().await;
            match engagements
                .iter_mut()
                .find(|entry| entry.engagement_id == engagement_id)
            {
                Some(entry) => {
                    if let Some(ceiling) = tool_ceiling {
                        entry.tool_ceiling.retain(|tool| ceiling.contains(tool));
                    }
                    if let Some(team) = team {
                        entry.team.retain(|agent| team.contains(agent));
                    }
                    entry.authority_revision += 1;
                    true
                },
                None => false,
            }
        };
        if mutated {
            self.persist().await?;
        }
        Ok(mutated)
    }

    /// Renew an engagement by moving its expiry forward — **the same
    /// engagement**, keeping its id.
    ///
    /// Re-granting instead mints a new ULID, and every execution, pause blob,
    /// envelope, disclosure record and retrieval partition already bound to the
    /// old id goes on naming an authority that has lapsed. Nothing repoints
    /// them, because nothing was ever copied anywhere. Renewal is therefore the
    /// only way to keep a live piece of work alive without orphaning what it has
    /// already done.
    ///
    /// # Forward-only, and never onto a terminal state
    ///
    /// - **Revoked refuses.** Terminal states never resurrect; an owner who
    ///   withdrew an authority did not ask for it back.
    /// - **Already lapsed refuses.** `now_ms >= expires_at_ms` is expired by
    ///   exactly the rule the liveness check applies, and moving
    ///   that window would re-authorise acts the clock had already stopped —
    ///   including any that were denied while it was down. A lapsed engagement
    ///   is re-granted with a fresh owner act, not renewed.
    /// - **An earlier expiry refuses.** Shortening a window is a withdrawal, and
    ///   this store has one withdrawal act — [`Self::revoke`] — whose instant the
    ///   audit records. A second path to "ends sooner" writing a different field
    ///   would make *when did authority end* answerable two ways.
    ///
    /// Expiry is read here against the caller's `now_ms` rather than deferred to
    /// [`Self::live_authority`], because unlike [`Self::set_owner_agent`] this
    /// call *changes* the answer to the liveness question. Deciding it from the
    /// clock is the only way the refusal and the live check can agree.
    ///
    /// An identical replay resumes: `Ok(false)`, nothing written, the revision
    /// unmoved, so a retried owner action is free. Every other disagreement is an
    /// error rather than a silent no-op, for the reason spelled out on
    /// [`Self::set_owner_agent`].
    pub async fn extend_expiry(
        &self,
        engagement_id: &str,
        new_expires_at_ms: i64,
        now_ms: i64,
    ) -> std::io::Result<bool> {
        let _guard = self.write_lock.lock().await;
        let mutated = {
            let mut engagements = self.engagements.write().await;
            let Some(entry) = engagements
                .iter_mut()
                .find(|entry| entry.engagement_id == engagement_id)
            else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "no engagement `{engagement_id}` in this roster; renewing one that does \
                         not exist would answer the same as a replay"
                    ),
                ));
            };
            if entry.revoked_at_ms.is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "engagement `{engagement_id}` is revoked; renewing it would resurrect an \
                         authority the owner withdrew"
                    ),
                ));
            }
            if now_ms >= entry.expires_at_ms {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "engagement `{engagement_id}` lapsed at {} and is already over at \
                         {now_ms}; renewing it would re-authorise acts the clock had stopped. \
                         Grant a new engagement instead",
                        entry.expires_at_ms
                    ),
                ));
            }
            match new_expires_at_ms.cmp(&entry.expires_at_ms) {
                // Identical replay: resume, write nothing, move nothing.
                std::cmp::Ordering::Equal => false,
                std::cmp::Ordering::Less => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "engagement `{engagement_id}` expires at {}, and {new_expires_at_ms} \
                             is earlier: ending a window sooner is a withdrawal, and `revoke` is \
                             the one act that records when authority ended",
                            entry.expires_at_ms
                        ),
                    ));
                },
                std::cmp::Ordering::Greater => {
                    entry.expires_at_ms = new_expires_at_ms;
                    entry.authority_revision += 1;
                    true
                },
            }
        };
        if mutated {
            self.persist().await?;
        }
        Ok(mutated)
    }

    /// Name the agent that fronts this engagement — **the owner act that opens
    /// the inbound lane** (§4.2 branch 2).
    ///
    /// Until this is set, a proved identity of this counterparty still routes
    /// to the guest lane, because the engagement names nowhere for it to go.
    /// Nothing derives the agent from `team[]`, from the program, or from the
    /// last agent to send outbound: each of those would be an automatic
    /// promotion wearing a plausible heuristic, and §3.2 forbids exactly that.
    ///
    /// **Set-once, and a changed payload is an error.** An identical replay
    /// resumes — `Ok(false)`, nothing written, the revision unmoved — so a
    /// retried owner action is free. A *different* agent is refused rather than
    /// silently ignored: redirecting the front of a live engagement moves
    /// traffic an owner already approved somewhere else, and that decision
    /// deserves a new engagement with its own owner act.
    ///
    /// A revoked engagement refuses: terminal states never resurrect. Expiry is
    /// not checked here because it is derived from the clock at read time — an
    /// expired engagement's lane is already closed by
    /// [`Self::live_authority`], and re-deciding it from a second clock is how
    /// two answers to one question come to disagree.
    pub async fn set_owner_agent(
        &self,
        engagement_id: &str,
        owner_agent_id: &str,
    ) -> std::io::Result<bool> {
        let owner_agent_id = owner_agent_id.trim();
        if owner_agent_id.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "an engagement's owner agent must be named: a blank agent id would open a lane \
                 that forwards to nobody, which is worse than the guest lane rather than a \
                 weaker form of it",
            ));
        }
        let _guard = self.write_lock.lock().await;
        let mutated = {
            let mut engagements = self.engagements.write().await;
            let Some(entry) = engagements
                .iter_mut()
                .find(|entry| entry.engagement_id == engagement_id)
            else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "no engagement `{engagement_id}` in this roster; naming the front of an \
                         engagement that does not exist would answer the same as a replay"
                    ),
                ));
            };
            if entry.revoked_at_ms.is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "engagement `{engagement_id}` is revoked; naming its owner agent would \
                         resurrect a lane the owner closed"
                    ),
                ));
            }
            // Cloned so the arm that writes is not holding a borrow of the
            // field it writes.
            let recorded = entry.owner_agent_id.clone();
            match recorded.as_deref() {
                // Identical replay: resume, write nothing, move nothing.
                Some(existing) if existing == owner_agent_id => false,
                Some(existing) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "engagement `{engagement_id}` is already fronted by `{existing}`; \
                             re-pointing it at `{owner_agent_id}` would move traffic the owner \
                             approved for one agent to another. Revoke and mint instead"
                        ),
                    ));
                },
                None => {
                    entry.owner_agent_id = Some(owner_agent_id.to_string());
                    entry.authority_revision += 1;
                    true
                },
            }
        };
        if mutated {
            self.persist().await?;
        }
        Ok(mutated)
    }

    /// The live authority check — the one the dispatch boundary calls.
    ///
    /// Scope must match: an engagement minted for one principal/workspace
    /// answers no other. Liveness (revoked/expired) is computed against
    /// `now_ms` at every call, never cached, because the revocation case is
    /// exactly the one that invalidates cached state.
    pub async fn live_authority(
        &self,
        principal: &str,
        workspace: &str,
        engagement_id: &str,
        now_ms: i64,
    ) -> Result<LiveAuthority, AuthorityDenial> {
        let engagements = self.engagements.read().await;
        let Some(entry) = engagements.iter().find(|entry| {
            entry.engagement_id == engagement_id
                && entry.principal == principal
                && entry.workspace == workspace
        }) else {
            return Err(AuthorityDenial::UnknownEngagement);
        };
        entry.live_at(now_ms)?;
        Ok(LiveAuthority {
            engagement_id: entry.engagement_id.clone(),
            authority_revision: entry.authority_revision,
            tool_ceiling: entry.tool_ceiling.clone(),
            team: entry.team.clone(),
        })
    }

    pub async fn list(&self, principal: &str, workspace: &str) -> Vec<EngagementAuthority> {
        self.engagements
            .read()
            .await
            .iter()
            .filter(|entry| entry.principal == principal && entry.workspace == workspace)
            .cloned()
            .collect()
    }

    async fn persist(&self) -> std::io::Result<()> {
        // Snapshot under the read guard; the caller holds `write_lock`, so
        // two persists cannot interleave snapshot and rename (the pairing
        // roster's monotonicity discipline).
        let bytes = {
            let engagements = self.engagements.read().await;
            serde_json::to_vec_pretty(&*engagements).expect("engagement roster serializes")
        };
        write_bytes_durably(&self.path, &bytes).await
    }
}

// ---------------------------------------------------------------------------
// The dispatch-boundary check
// ---------------------------------------------------------------------------

/// The live authorization check for one consequential dispatch.
///
/// This is the function the execution boundary calls — after trust policy,
/// before any side effect — whenever the execution context carries an
/// [`EngagementAuthorityRef`]. Its shape encodes three §4.2c rules:
///
/// - **Unconditional when a ref is present.** The pre-existing snapshot gate
///   (`validate_action_against_policy_snapshot`) skips its ceiling check when
///   its guard slot is `None`; this check must never inherit that fail-open
///   shape. A context that names an engagement is checked, every time.
/// - **Fails closed on store absence** (row 8). No installed store means no
///   answer, and no answer means denied — an unavailable authority must not
///   read as unrestricted.
/// - **Live, not snapshot** (rows 7 and 9). Revocation, expiry and narrowing
///   are read from the store at dispatch time; the revision carried by the
///   execution is how staleness is *observed* (`revision_changed`), but the
///   ceiling actually enforced is always the store's current one.
pub async fn authorize_engagement_dispatch(
    carried: &EngagementAuthorityRef,
    principal: &str,
    workspace: &str,
    policy_tool_name: &str,
    now_ms: i64,
) -> Result<EngagementDispatchGrant, AuthorityDenial> {
    let Some(store) = global_engagement_store() else {
        return Err(AuthorityDenial::StoreUnavailable {
            detail: "no engagement store installed in this process".to_string(),
        });
    };
    authorize_engagement_dispatch_with_store(
        &store,
        carried,
        principal,
        workspace,
        policy_tool_name,
        now_ms,
    )
    .await
}

/// The store-taking core. The global wrapper above is the one the dispatch
/// boundary calls; this is the same logic against an explicit store, so the
/// property tests exercise the check without racing the process-wide
/// `OnceLock`.
pub async fn authorize_engagement_dispatch_with_store(
    store: &EngagementStore,
    carried: &EngagementAuthorityRef,
    principal: &str,
    workspace: &str,
    policy_tool_name: &str,
    now_ms: i64,
) -> Result<EngagementDispatchGrant, AuthorityDenial> {
    let live = store
        .live_authority(principal, workspace, &carried.engagement_id, now_ms)
        .await?;
    if !live.tool_ceiling.contains(policy_tool_name) {
        return Err(AuthorityDenial::ToolOutsideCeiling {
            tool: policy_tool_name.to_string(),
        });
    }
    Ok(EngagementDispatchGrant {
        revision_changed: live.authority_revision != carried.authority_revision,
        live,
    })
}

/// The same live check, for a delegation or handover target rather than a
/// tool: the target must be inside the engagement's `team[]` (§4.2c row 4).
pub async fn authorize_engagement_delegation(
    carried: &EngagementAuthorityRef,
    principal: &str,
    workspace: &str,
    target_agent_id: &str,
    now_ms: i64,
) -> Result<EngagementDispatchGrant, AuthorityDenial> {
    let Some(store) = global_engagement_store() else {
        return Err(AuthorityDenial::StoreUnavailable {
            detail: "no engagement store installed in this process".to_string(),
        });
    };
    authorize_engagement_delegation_with_store(
        &store,
        carried,
        principal,
        workspace,
        target_agent_id,
        now_ms,
    )
    .await
}

/// Store-taking core; see [`authorize_engagement_dispatch_with_store`].
pub async fn authorize_engagement_delegation_with_store(
    store: &EngagementStore,
    carried: &EngagementAuthorityRef,
    principal: &str,
    workspace: &str,
    target_agent_id: &str,
    now_ms: i64,
) -> Result<EngagementDispatchGrant, AuthorityDenial> {
    let live = store
        .live_authority(principal, workspace, &carried.engagement_id, now_ms)
        .await?;
    if !live.team.contains(target_agent_id) {
        return Err(AuthorityDenial::TargetOutsideTeam {
            agent_id: target_agent_id.to_string(),
        });
    }
    Ok(EngagementDispatchGrant {
        revision_changed: live.authority_revision != carried.authority_revision,
        live,
    })
}

/// **The durable authority a ROOT execution carries for the work that asked for
/// it** — §4.2c, from the entry point rather than from a parent.
///
/// # Why a root needs its own function at all
///
/// A child inherits its carrier verbatim from the parent's `ExecutionRun`.
/// A root has no parent, so the only place its authority can come from is the
/// request that started the run — and a request naming a work context must not
/// be the same thing as being granted it. This is the validation that keeps
/// those two apart: the caller names the work, and the **store** supplies the
/// revision that gets persisted. A caller cannot pin a revision, cannot pin a
/// ceiling, and cannot start a run under an engagement that is not live right
/// now.
///
/// # Generic in, generic out
///
/// Takes a [`WorkContextKind`] and answers with a
/// [`WorkAuthorityRef`], because the same entry point serves support triage,
/// recruiting and vendor management as well as the flow this was built for.
/// The durable record now holds every arm, so the arm that is still refused is
/// refused for the one honest reason left — nothing can *validate* it — and
/// that refusal is by name, never a silent coercion.
///
/// # Every arm fails closed
///
/// - a blank id, or one carrying U+001F, is refused before any read: it
///   is a caller string feeding an id derivation, and one that could carry the
///   separator could address a record it did not name;
/// - an unknown, revoked or expired engagement is refused — liveness is read
///   from the clock at read time, never cached;
/// - **no roster installed in this process** is refused, not treated as "no
///   restriction": an unavailable authority has not answered yes;
/// - [`WorkContextKind::Program`] is refused, because **no roster owns
///   programs**: there is nothing to read a liveness answer or an authority
///   revision from, and a revision this function invented rather than read
///   would be exactly the caller-supplied authority row 5 forbids. The durable
///   carrier holds the arm; the validation does not exist yet. Returning
///   `Ok(None)` here would start the run unbound while the caller believed it
///   was confined, and nothing downstream could tell the difference.
pub async fn root_authority_for_work(
    principal: &str,
    workspace: &str,
    work: &WorkContextKind,
    now_ms: i64,
) -> Result<WorkAuthorityRef, String> {
    guard_work_id(work)?;
    let Some(store) = global_engagement_store() else {
        return Err(format!(
            "`{}` cannot be verified: no engagement roster is installed in this process, so the \
             run would start unconfined while the request asked for it to be confined",
            work.as_key()
        ));
    };
    root_authority_for_work_with_store(&store, principal, workspace, work, now_ms).await
}

/// Store-taking core; see [`root_authority_for_work`]. Separate so tests
/// exercise the validation against a real roster without racing the
/// process-wide `OnceLock`.
///
/// # The program refusal below is now the ONLY thing missing
///
/// It used to be one of several. The in-memory execution context carries the
/// generic [`WorkAuthorityRef`], the dispatch boundary
/// (`execution::agentic::executor::work_binding_for_dispatch`) builds a bound
/// `Program` work context from it, and the outward act it gates is filed under
/// `program_id` and scoped to `EnvelopeScope::Program`. So the path from
/// carrier to enforcement is plumbed end to end; what is still absent is a
/// roster that owns programs and can answer with a revision.
///
/// Which is why this refusal must stay until that roster exists. Minting a
/// revision here — zero, or the current clock — would be exactly the
/// caller-supplied authority §4.2c row 5 forbids, dressed as a store answer,
/// and every downstream staleness check (row 9) would compare against a number
/// nothing can ever bump.
pub async fn root_authority_for_work_with_store(
    store: &EngagementStore,
    principal: &str,
    workspace: &str,
    work: &WorkContextKind,
    now_ms: i64,
) -> Result<WorkAuthorityRef, String> {
    guard_work_id(work)?;
    let engagement_id = match work {
        WorkContextKind::Engagement(id) => id,
        WorkContextKind::Program(id) => {
            return Err(format!(
                "a root execution cannot yet carry program `{id}`: the durable carrier holds \
                 every kind of work, but no roster owns programs, so there is no live answer and \
                 no authority revision to read. Refused rather than dropped, because dropping it \
                 would start the run unconfined, and refused rather than invented, because a \
                 revision this function made up is a caller-supplied authority in disguise"
            ));
        },
    };
    let live = store
        .live_authority(principal, workspace, engagement_id, now_ms)
        .await
        .map_err(|denial| {
            format!(
                "`{}` does not authorise a run right now: {denial:?}",
                work.as_key()
            )
        })?;
    // Both halves from the store. The caller named the work; it did not get to
    // say which revision of it, and it did not get to say that the id it named
    // is the id that was found — the carrier is rebuilt from `live`, not from
    // the argument.
    WorkAuthorityRef::new(
        WorkContextKind::Engagement(live.engagement_id),
        live.authority_revision,
    )
}

/// Refuse a work id that cannot safely name a record.
///
/// One guard, owned by the generic type: [`WorkContextKind::guard_id`]. The
/// storage boundary rebuilds a carrier from a wire token and has to apply the
/// same rule, and two copies of a U+001F check are two chances for one of them
/// to be forgotten.
fn guard_work_id(work: &WorkContextKind) -> Result<(), String> {
    work.guard_id()
}

/// A granted dispatch, with what the boundary needs to know afterwards.
#[derive(Debug, Clone)]
pub struct EngagementDispatchGrant {
    /// The authority as it stands now — the ceiling that was actually
    /// enforced, not the one the snapshot was resolved under.
    pub live: LiveAuthority,
    /// True when the store's revision differs from the one the execution
    /// carried: the policy snapshot is stale and should be re-resolved
    /// before the next provider decision (§4.2c row 9). The dispatch itself
    /// was still judged against live authority, so this is a freshness
    /// signal, not a denial.
    pub revision_changed: bool,
}

// ---------------------------------------------------------------------------
// Process-wide handle, the device-governance pattern: compiled handlers and
// the dispatch boundary cannot thread constructor arguments, so the binary
// installs the store at boot.
// ---------------------------------------------------------------------------

static GLOBAL_ENGAGEMENTS: OnceLock<Arc<EngagementStore>> = OnceLock::new();

pub fn install_global_engagement_store(store: Arc<EngagementStore>) {
    let _ = GLOBAL_ENGAGEMENTS.set(store);
}

pub fn global_engagement_store() -> Option<Arc<EngagementStore>> {
    GLOBAL_ENGAGEMENTS.get().cloned()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn tools(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    async fn store_with_one(temp: &tempfile::TempDir) -> (EngagementStore, EngagementAuthority) {
        let store = EngagementStore::open(temp.path()).await.unwrap();
        let engagement = store
            .create(
                "alpha",
                "prod",
                "fundraising",
                "Acme Capital",
                tools(&["research", "restricted_email"]),
                tools(&["worker-agent"]),
                1_000,
                100_000,
            )
            .await
            .unwrap();
        (store, engagement)
    }

    #[tokio::test]
    async fn a_live_engagement_answers_with_its_ceiling_and_revision() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let live = store
            .live_authority("alpha", "prod", &engagement.engagement_id, 2_000)
            .await
            .unwrap();
        assert_eq!(live.authority_revision, 1);
        assert!(live.tool_ceiling.contains("research"));
        assert!(live.team.contains("worker-agent"));
    }

    #[tokio::test]
    async fn scope_mismatch_is_unknown_not_leaked() {
        // An engagement minted in one scope must answer no other — the same
        // property every store in this system holds.
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let denial = store
            .live_authority("beta", "prod", &engagement.engagement_id, 2_000)
            .await
            .unwrap_err();
        assert_eq!(denial, AuthorityDenial::UnknownEngagement);
    }

    #[tokio::test]
    async fn revocation_bumps_the_revision_and_denies_immediately() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        assert!(store
            .revoke(&engagement.engagement_id, 3_000)
            .await
            .unwrap());
        let denial = store
            .live_authority("alpha", "prod", &engagement.engagement_id, 3_001)
            .await
            .unwrap_err();
        assert_eq!(denial, AuthorityDenial::Revoked);
        // The stored record carries the bumped revision: a snapshot resolved
        // at revision 1 is now detectably stale.
        let listed = store.list("alpha", "prod").await;
        assert_eq!(listed[0].authority_revision, 2);
    }

    #[tokio::test]
    async fn expiry_denies_without_any_owner_act() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let denial = store
            .live_authority("alpha", "prod", &engagement.engagement_id, 100_000)
            .await
            .unwrap_err();
        assert_eq!(denial, AuthorityDenial::Expired);
    }

    #[tokio::test]
    async fn narrowing_shrinks_and_bumps_but_never_widens() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        // Attempt to "narrow" to a set containing a NEW tool: intersection
        // semantics mean the new tool must not appear.
        store
            .narrow(
                &engagement.engagement_id,
                Some(tools(&["research", "raw_email"])),
                None,
            )
            .await
            .unwrap();
        let live = store
            .live_authority("alpha", "prod", &engagement.engagement_id, 2_000)
            .await
            .unwrap();
        assert!(live.tool_ceiling.contains("research"), "kept: in both sets");
        assert!(
            !live.tool_ceiling.contains("raw_email"),
            "a narrow can never introduce a tool the ceiling lacked"
        );
        assert!(
            !live.tool_ceiling.contains("restricted_email"),
            "dropped: absent from the narrowed set"
        );
        assert_eq!(live.authority_revision, 2);
    }

    #[tokio::test]
    async fn the_roster_survives_a_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        store
            .revoke(&engagement.engagement_id, 3_000)
            .await
            .unwrap();
        drop(store);

        let reopened = EngagementStore::open(temp.path()).await.unwrap();
        let listed = reopened.list("alpha", "prod").await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].revoked_at_ms, Some(3_000));
        assert_eq!(listed[0].authority_revision, 2);
    }

    // -- §4.2c dispatch boundary (layer 2), proven at the enforcement
    //    function independently of policy projection. The scenario is the
    //    plan's central adversarial case: a worker whose static grant is
    //    strictly broader than the engagement ceiling.

    #[tokio::test]
    async fn dispatch_allows_within_ceiling_and_denies_the_broader_static_grant() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await; // ceiling: research + restricted_email
        let carried = EngagementAuthorityRef {
            engagement_id: engagement.engagement_id.clone(),
            authority_revision: engagement.authority_revision,
        };
        // In ceiling → allowed.
        let ok = authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 2_000,
        )
        .await
        .unwrap();
        assert!(!ok.revision_changed);
        // The worker's static grant also has raw_email + browser; the
        // engagement ceiling does not — dispatch denies them even though the
        // agent definition permits them.
        for raw in ["raw_email", "browser", "files"] {
            let denial = authorize_engagement_dispatch_with_store(
                &store, &carried, "alpha", "prod", raw, 2_000,
            )
            .await
            .unwrap_err();
            assert_eq!(
                denial,
                AuthorityDenial::ToolOutsideCeiling {
                    tool: raw.to_string()
                }
            );
        }
    }

    #[tokio::test]
    async fn dispatch_denies_after_mid_run_revocation_before_the_side_effect() {
        // The revocation row: the one that fails a snapshot valid when taken.
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let carried = EngagementAuthorityRef {
            engagement_id: engagement.engagement_id.clone(),
            authority_revision: engagement.authority_revision,
        };
        // Was allowed a moment ago.
        authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 2_000,
        )
        .await
        .unwrap();
        // Revoked while the execution is still alive holding revision 1.
        store
            .revoke(&engagement.engagement_id, 2_500)
            .await
            .unwrap();
        // The very next dispatch is denied — live check, not the snapshot.
        let denial = authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 3_000,
        )
        .await
        .unwrap_err();
        assert_eq!(denial, AuthorityDenial::Revoked);
    }

    #[tokio::test]
    async fn delegation_is_bounded_by_team_and_narrowing_bumps_the_staleness_signal() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await; // team: worker-agent
        let carried = EngagementAuthorityRef {
            engagement_id: engagement.engagement_id.clone(),
            authority_revision: engagement.authority_revision,
        };
        assert!(authorize_engagement_delegation_with_store(
            &store,
            &carried,
            "alpha",
            "prod",
            "worker-agent",
            2_000
        )
        .await
        .is_ok());
        let denial = authorize_engagement_delegation_with_store(
            &store,
            &carried,
            "alpha",
            "prod",
            "outsider-agent",
            2_000,
        )
        .await
        .unwrap_err();
        assert_eq!(
            denial,
            AuthorityDenial::TargetOutsideTeam {
                agent_id: "outsider-agent".to_string()
            }
        );

        // A narrow bumps the revision; the still-live tool dispatch now
        // reports the carried snapshot as stale (row 9) without denying.
        store
            .narrow(&engagement.engagement_id, Some(tools(&["research"])), None)
            .await
            .unwrap();
        let grant = authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 3_000,
        )
        .await
        .unwrap();
        assert!(
            grant.revision_changed,
            "carried revision 1 vs live revision 2"
        );
    }

    #[tokio::test]
    async fn the_global_wrapper_fails_closed_when_no_store_is_installed() {
        // Row 8: an authority store that cannot answer must read as denied,
        // never as unrestricted. In a test process no store is installed, so
        // the global wrapper takes the fail-closed path.
        let carried = EngagementAuthorityRef {
            engagement_id: "eng-x".to_string(),
            authority_revision: 1,
        };
        let denial = authorize_engagement_dispatch(&carried, "alpha", "prod", "research", 2_000)
            .await
            .unwrap_err();
        assert!(matches!(denial, AuthorityDenial::StoreUnavailable { .. }));
    }

    #[tokio::test]
    async fn a_corrupt_roster_refuses_to_open_rather_than_defaulting() {
        // The opposite call from the device policy store, deliberately: a
        // "default" engagement roster under live engagement ids would deny
        // everything undebuggably. Loud refusal preserves the evidence.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("system").join("engagements.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        let error = EngagementStore::open(temp.path()).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    // ── The owner act that opens an inbound lane ────────────────────────────

    /// **A minted engagement fronts nobody.**
    ///
    /// Pins the default that would turn creating an engagement into opening a
    /// door: if `create` filled this in from the team, the program, or anything
    /// else plausible, every engagement in the roster would become a place
    /// inbound traffic could land the moment forwarding was switched on, with
    /// no owner having chosen it.
    #[tokio::test]
    async fn a_new_engagement_names_no_owner_agent() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        assert_eq!(engagement.owner_agent_id, None);
        let listed = store.list("alpha", "prod").await;
        assert_eq!(listed[0].owner_agent_id, None);
        assert_eq!(listed[0].authority_revision, 1);
    }

    /// Naming the agent is a mutation like any other: it bumps the revision, so
    /// a snapshot resolved before the lane opened is detectably stale.
    ///
    /// Pins a write that changes what an engagement means while leaving the
    /// revision alone — every staleness check downstream would then miss it.
    #[tokio::test]
    async fn naming_the_owner_agent_records_it_and_bumps_the_revision() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        assert!(store
            .set_owner_agent(&engagement.engagement_id, "ambassador")
            .await
            .unwrap());
        let listed = store.list("alpha", "prod").await;
        assert_eq!(listed[0].owner_agent_id.as_deref(), Some("ambassador"));
        assert_eq!(listed[0].authority_revision, 2);
    }

    /// An identical replay resumes; a changed payload is an error.
    ///
    /// Pins the silent re-point: an owner action retried twice must be free,
    /// and a *second, different* agent must not quietly take over the front of
    /// a live engagement — that moves traffic the owner approved for one agent
    /// to another with nothing recorded.
    #[tokio::test]
    async fn re_pointing_a_fronted_engagement_is_an_error_and_a_replay_is_free() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        assert!(store
            .set_owner_agent(&engagement.engagement_id, "ambassador")
            .await
            .unwrap());

        // Replay, whitespace and all: resumes, writes nothing, moves nothing.
        assert!(!store
            .set_owner_agent(&engagement.engagement_id, "  ambassador  ")
            .await
            .unwrap());
        assert_eq!(store.list("alpha", "prod").await[0].authority_revision, 2);

        let error = store
            .set_owner_agent(&engagement.engagement_id, "other-agent")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            store.list("alpha", "prod").await[0]
                .owner_agent_id
                .as_deref(),
            Some("ambassador"),
            "a refused re-point must not have written"
        );
    }

    /// A revoked engagement cannot be given a front. Terminal states never
    /// resurrect.
    ///
    /// Pins the resurrection path: revoke closes the lane, and a later
    /// `set_owner_agent` that succeeded would reopen an engagement the owner
    /// deliberately ended — the live check would still deny dispatch, but the
    /// roster would read as though the engagement were staffed.
    #[tokio::test]
    async fn a_revoked_engagement_refuses_to_be_fronted() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        assert!(store
            .revoke(&engagement.engagement_id, 3_000)
            .await
            .unwrap());
        let error = store
            .set_owner_agent(&engagement.engagement_id, "ambassador")
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            store.list("alpha", "prod").await[0].owner_agent_id,
            None,
            "a refused write must not have landed"
        );
    }

    /// An unknown engagement and a blank agent are refusals, not `false`.
    ///
    /// Pins the ambiguity that `Ok(false)` would create: it already means
    /// "identical replay, nothing to do", so returning it for "no such
    /// engagement" would let a typo'd id read as a successful no-op.
    #[tokio::test]
    async fn an_unknown_engagement_or_a_blank_agent_is_refused_not_reported_as_a_replay() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;

        let missing = store
            .set_owner_agent("eng-does-not-exist", "ambassador")
            .await
            .unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

        for blank in ["", "   "] {
            let error = store
                .set_owner_agent(&engagement.engagement_id, blank)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
        assert_eq!(store.list("alpha", "prod").await[0].owner_agent_id, None);
    }

    /// The named agent survives a restart, and an older roster written before
    /// this field existed still opens.
    ///
    /// Pins two failures at once: a field that is not persisted (the lane
    /// closes on every boot), and a serde shape that makes every pre-existing
    /// `engagements.json` a hard `InvalidData` refusal at startup.
    #[tokio::test]
    async fn the_named_agent_survives_a_reopen_and_an_older_roster_still_opens() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        store
            .set_owner_agent(&engagement.engagement_id, "ambassador")
            .await
            .unwrap();
        drop(store);

        let reopened = EngagementStore::open(temp.path()).await.unwrap();
        let listed = reopened.list("alpha", "prod").await;
        assert_eq!(listed[0].owner_agent_id.as_deref(), Some("ambassador"));
        assert_eq!(listed[0].authority_revision, 2);

        // A roster written before the field existed.
        let older = tempfile::tempdir().unwrap();
        let path = older.path().join("system").join("engagements.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"[{"engagement_id":"eng-old","principal":"alpha","workspace":"prod",
                 "program_id":"p","counterparty":"Acme Capital","tool_ceiling":["research"],
                 "team":["worker-agent"],"authority_revision":1,"created_at_ms":1,
                 "expires_at_ms":100000}]"#,
        )
        .unwrap();
        let legacy = EngagementStore::open(older.path()).await.unwrap();
        let rows = legacy.list("alpha", "prod").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].owner_agent_id, None,
            "no front until an owner names one"
        );
    }

    // ── Minting from the generic work carrier ───────────────────────────────

    /// An engagement minted from a [`WorkContext`] records the program the work
    /// names and takes its ceiling from what the work needs — capabilities and
    /// playbooks alike.
    ///
    /// Pins the mapping silently dropping half of itself. `work_context::resolve`
    /// treats `needs_capabilities` and `needs_playbooks` as one namespace, so a
    /// ceiling built from only the first would refuse the procedure skill the
    /// work exists to run, and the refusal would read as "outside this work" for
    /// something the work literally names.
    #[tokio::test]
    async fn an_engagement_granted_from_a_work_context_takes_its_ceiling_from_the_work() {
        let temp = tempfile::tempdir().unwrap();
        let store = EngagementStore::open(temp.path()).await.unwrap();
        let work = WorkContext::new(WorkContextKind::Program("support-triage".to_string()))
            .needing(&["research", "restricted_email"])
            .with_playbooks(&["escalation_ladder"]);

        let engagement = store
            .grant_for_work(
                "alpha",
                "prod",
                &work,
                "Globex",
                tools(&["support-agent"]),
                1_000,
                100_000,
            )
            .await
            .unwrap();

        assert_eq!(engagement.program_id, "support-triage");
        assert_eq!(engagement.counterparty, "Globex");
        assert_eq!(engagement.expires_at_ms, 100_000);
        assert_eq!(engagement.authority_revision, 1);
        assert_eq!(
            engagement.owner_agent_id, None,
            "minting still opens no inbound lane"
        );
        assert_eq!(
            engagement.tool_ceiling,
            tools(&["escalation_ladder", "research", "restricted_email"]),
            "the playbook the work names is in the ceiling beside the capabilities"
        );

        // And it is enforceable: the ceiling is what dispatch reads.
        let carried = EngagementAuthorityRef {
            engagement_id: engagement.engagement_id.clone(),
            authority_revision: engagement.authority_revision,
        };
        assert!(authorize_engagement_dispatch_with_store(
            &store,
            &carried,
            "alpha",
            "prod",
            "escalation_ladder",
            2_000
        )
        .await
        .is_ok());
        assert_eq!(
            authorize_engagement_dispatch_with_store(
                &store,
                &carried,
                "alpha",
                "prod",
                "raw_email",
                2_000
            )
            .await
            .unwrap_err(),
            AuthorityDenial::ToolOutsideCeiling {
                tool: "raw_email".to_string()
            }
        );
    }

    /// The ceiling is recorded with the exact spelling the work context used.
    ///
    /// Pins a normalisation nobody would see until an act failed:
    /// `authorize_engagement_dispatch` tests the dispatch's policy name against
    /// the ceiling with case-sensitive `contains`, so a ceiling that lower-cased
    /// `WebSearch` on the way in authorises nothing for a tool dispatched under
    /// that name — a grant that grants nothing, which reads on the owner surface
    /// exactly like one that works.
    #[tokio::test]
    async fn the_ceiling_keeps_the_exact_spelling_the_work_context_used() {
        let temp = tempfile::tempdir().unwrap();
        let store = EngagementStore::open(temp.path()).await.unwrap();
        // Built by struct literal rather than `needing`, which lower-cases.
        let work = WorkContext {
            kind: WorkContextKind::Program("vendor-review".to_string()),
            needs_capabilities: vec!["WebSearch".to_string(), " padded_tool ".to_string()],
            needs_playbooks: Vec::new(),
        };
        let engagement = store
            .grant_for_work("alpha", "prod", &work, "Initech", BTreeSet::new(), 1, 10)
            .await
            .unwrap();
        assert_eq!(
            engagement.tool_ceiling,
            tools(&["WebSearch", "padded_tool"]),
            "case survives; surrounding whitespace does not, because a name with it \
             could never equal a dispatched one either"
        );
    }

    /// A work context that names an *engagement* is refused, not filed under
    /// `program_id`.
    ///
    /// Pins the quiet conflation: an engagement id written into the one work
    /// field this record has would be listed and filtered as a program on the
    /// owner surface, and would merge with any program sharing the id — two
    /// authorities answering under one heading, with nothing to tell them apart.
    #[tokio::test]
    async fn a_work_context_naming_an_engagement_is_refused_rather_than_filed_as_a_program() {
        let temp = tempfile::tempdir().unwrap();
        let store = EngagementStore::open(temp.path()).await.unwrap();
        let work = WorkContext::new(WorkContextKind::Engagement("eng-parent".to_string()))
            .needing(&["research"]);
        let error = store
            .grant_for_work("alpha", "prod", &work, "Acme", BTreeSet::new(), 1, 10)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            store.list("alpha", "prod").await.len(),
            0,
            "a refused mint must not have written"
        );
        // The program arm of the same generic carrier is accepted, so the
        // refusal is about the kind and not about work contexts.
        let program_work = WorkContext::new(WorkContextKind::Program("recruiting".to_string()))
            .needing(&["research"]);
        assert_eq!(
            store
                .grant_for_work(
                    "alpha",
                    "prod",
                    &program_work,
                    "Acme",
                    BTreeSet::new(),
                    1,
                    10
                )
                .await
                .unwrap()
                .program_id,
            "recruiting"
        );
    }

    /// A window that is already over at mint time is refused.
    ///
    /// Pins the engagement that is dead on arrival. Expiry is inclusive, so an
    /// engagement minted at or after its own expiry denies every dispatch while
    /// sitting in the roster looking like authority — the owner believes they
    /// granted something, and the run fails somewhere with an unrelated message.
    #[tokio::test]
    async fn a_window_that_is_already_over_is_refused_rather_than_minted() {
        let temp = tempfile::tempdir().unwrap();
        let store = EngagementStore::open(temp.path()).await.unwrap();
        for expires_at_ms in [5_000_i64, 4_999, 0, -1] {
            let error = store
                .create(
                    "alpha",
                    "prod",
                    "outreach",
                    "Acme",
                    tools(&["research"]),
                    BTreeSet::new(),
                    5_000,
                    expires_at_ms,
                )
                .await
                .unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidInput,
                "expiry {expires_at_ms} at now 5000 is not a window"
            );
        }
        assert_eq!(store.list("alpha", "prod").await.len(), 0);
        // One millisecond of window is a window.
        assert_eq!(
            store
                .create(
                    "alpha",
                    "prod",
                    "outreach",
                    "Acme",
                    tools(&["research"]),
                    BTreeSet::new(),
                    5_000,
                    5_001,
                )
                .await
                .unwrap()
                .expires_at_ms,
            5_001
        );
    }

    /// A program id or counterparty label carrying U+001F is refused.
    ///
    /// Pins the fused derivation. The counterparty label is hashed into a
    /// counterparty id and the program id is joined into scope keys, both with
    /// this character as the separator, so a value carrying it can move the
    /// boundary between two components and derive one id for two different
    /// records.
    #[tokio::test]
    async fn a_recorded_component_carrying_the_field_separator_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let store = EngagementStore::open(temp.path()).await.unwrap();
        let hostile = "Acme\u{1f}Capital";
        for (program, counterparty) in [
            ("outreach", hostile),
            (hostile, "Acme"),
            ("  ", "Acme"),
            ("outreach", "   "),
        ] {
            let error = store
                .create(
                    "alpha",
                    "prod",
                    program,
                    counterparty,
                    tools(&["research"]),
                    BTreeSet::new(),
                    1_000,
                    100_000,
                )
                .await
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
        assert_eq!(
            store.list("alpha", "prod").await.len(),
            0,
            "every refusal must have written nothing"
        );
    }

    // ── Renewal ─────────────────────────────────────────────────────────────

    /// Renewal moves the window forward, keeps the id, and bumps the revision.
    ///
    /// Pins the orphaning re-grant. Minting a replacement gives a new ULID, and
    /// every execution, pause blob and disclosure already carrying the old id
    /// goes on naming an authority that has lapsed — nothing repoints them,
    /// because nothing was ever copied anywhere.
    #[tokio::test]
    async fn renewing_moves_the_window_forward_and_keeps_the_same_engagement() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await; // expires 100_000
        let carried = EngagementAuthorityRef {
            engagement_id: engagement.engagement_id.clone(),
            authority_revision: engagement.authority_revision,
        };
        // Before renewal, the clock has already closed this instant.
        assert_eq!(
            store
                .live_authority("alpha", "prod", &engagement.engagement_id, 150_000)
                .await
                .unwrap_err(),
            AuthorityDenial::Expired
        );

        assert!(store
            .extend_expiry(&engagement.engagement_id, 200_000, 2_000)
            .await
            .unwrap());

        let listed = store.list("alpha", "prod").await;
        assert_eq!(listed.len(), 1, "renewal is not a second engagement");
        assert_eq!(listed[0].engagement_id, engagement.engagement_id);
        assert_eq!(listed[0].expires_at_ms, 200_000);
        assert_eq!(listed[0].authority_revision, 2);
        assert_eq!(listed[0].revoked_at_ms, None);
        assert_eq!(
            listed[0].tool_ceiling,
            tools(&["research", "restricted_email"]),
            "renewal moves the window and nothing else"
        );

        // The same carried ref — the one an already-running execution holds —
        // now dispatches at an instant that was outside the old window.
        let grant = authorize_engagement_dispatch_with_store(
            &store, &carried, "alpha", "prod", "research", 150_000,
        )
        .await
        .unwrap();
        assert_eq!(grant.live.authority_revision, 2);
        assert!(
            grant.revision_changed,
            "carried revision 1 vs live revision 2"
        );
    }

    /// An identical renewal is a free replay; an earlier expiry is an error.
    ///
    /// Pins two failures at once. A replay that bumped the revision would
    /// invalidate every live policy snapshot for nothing. And a silently
    /// accepted earlier expiry would end authority through a path the audit does
    /// not record, leaving *when did this end* answerable two ways.
    #[tokio::test]
    async fn an_identical_renewal_is_a_replay_and_an_earlier_one_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        assert!(store
            .extend_expiry(&engagement.engagement_id, 200_000, 2_000)
            .await
            .unwrap());

        assert!(!store
            .extend_expiry(&engagement.engagement_id, 200_000, 3_000)
            .await
            .unwrap());
        assert_eq!(store.list("alpha", "prod").await[0].authority_revision, 2);

        let error = store
            .extend_expiry(&engagement.engagement_id, 150_000, 3_000)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let listed = store.list("alpha", "prod").await;
        assert_eq!(
            listed[0].expires_at_ms, 200_000,
            "a refused renewal must not have moved the window"
        );
        assert_eq!(listed[0].authority_revision, 2);
    }

    /// A revoked engagement and a lapsed one both refuse renewal, and an unknown
    /// id is an error rather than a replay.
    ///
    /// Pins resurrection. Both states are terminal: revocation because an owner
    /// ended it, expiry because the clock did. A renewal that moved either
    /// window would re-authorise acts that were already being denied, and would
    /// do it without any record that authority had ever stopped.
    #[tokio::test]
    async fn a_revoked_or_lapsed_engagement_refuses_renewal() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        assert!(store
            .revoke(&engagement.engagement_id, 3_000)
            .await
            .unwrap());
        let revoked = store
            .extend_expiry(&engagement.engagement_id, 200_000, 4_000)
            .await
            .unwrap_err();
        assert_eq!(revoked.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(store.list("alpha", "prod").await[0].expires_at_ms, 100_000);

        let other = tempfile::tempdir().unwrap();
        let (fresh, lapsed) = store_with_one(&other).await; // expires 100_000
        for now_ms in [100_000_i64, 100_001] {
            let error = fresh
                .extend_expiry(&lapsed.engagement_id, 300_000, now_ms)
                .await
                .unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidInput,
                "expiry is inclusive, so {now_ms} is already past the window"
            );
        }
        assert_eq!(fresh.list("alpha", "prod").await[0].expires_at_ms, 100_000);
        assert_eq!(fresh.list("alpha", "prod").await[0].authority_revision, 1);

        let missing = fresh
            .extend_expiry("eng-does-not-exist", 300_000, 2_000)
            .await
            .unwrap_err();
        assert_eq!(
            missing.kind(),
            std::io::ErrorKind::NotFound,
            "`Ok(false)` already means `identical replay`, so a typo'd id must not read as one"
        );
    }

    /// The renewed window survives a restart.
    ///
    /// Pins a renewal that lived only in memory: the roster would reopen with
    /// the original expiry and the engagement would lapse on the next boot,
    /// while every surface had already reported it as renewed.
    #[tokio::test]
    async fn a_renewal_survives_a_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        store
            .extend_expiry(&engagement.engagement_id, 250_000, 2_000)
            .await
            .unwrap();
        drop(store);

        let reopened = EngagementStore::open(temp.path()).await.unwrap();
        let listed = reopened.list("alpha", "prod").await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].engagement_id, engagement.engagement_id);
        assert_eq!(listed[0].expires_at_ms, 250_000);
        assert_eq!(listed[0].authority_revision, 2);
    }

    // ── Root authority: the write path that had no caller ──────────────────

    /// Pins: a root execution's authority revision comes from the STORE, and
    /// tracks it.
    ///
    /// The failure this stops is the one §4.2c row 5 exists for, moved from the
    /// child to the root: a caller that could pin a revision could pin the one
    /// under which a ceiling was wider, and then run against a snapshot the
    /// owner has since narrowed. The caller names the work and nothing else, so
    /// the assertion is on VALUES — revision `1` before the narrowing and `2`
    /// after, from the same unchanged request.
    #[tokio::test]
    async fn a_root_takes_its_authority_revision_from_the_store_not_the_caller() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let work = WorkContextKind::Engagement(engagement.engagement_id.clone());

        let first = root_authority_for_work_with_store(&store, "alpha", "prod", &work, 2_000)
            .await
            .expect("a live engagement authorises a root");
        assert_eq!(
            first.work,
            WorkContextKind::Engagement(engagement.engagement_id.clone()),
            "the carrier must name the work that granted it, in the generic shape the durable \
             record stores"
        );
        assert_eq!(first.authority_revision, 1);

        store
            .narrow(&engagement.engagement_id, Some(tools(&["research"])), None)
            .await
            .expect("narrow the ceiling");

        let second = root_authority_for_work_with_store(&store, "alpha", "prod", &work, 2_000)
            .await
            .expect("still live after narrowing");
        assert_eq!(
            second.authority_revision, 2,
            "the persisted revision must follow the store, from an identical request"
        );
    }

    /// Pins: a revoked engagement cannot start a run.
    ///
    /// Revocation is the row most likely to be got wrong because it invalidates
    /// a snapshot that was valid when it was taken. A root created after
    /// revocation would carry a carrier the dispatch boundary then has to
    /// refuse on every act — a run that exists only to fail. The engagement is
    /// asserted to authorise a root BEFORE it is revoked, so the refusal
    /// afterwards is known to be the revocation.
    #[tokio::test]
    async fn a_revoked_engagement_cannot_start_a_root_run() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let work = WorkContextKind::Engagement(engagement.engagement_id.clone());

        assert!(
            root_authority_for_work_with_store(&store, "alpha", "prod", &work, 2_000)
                .await
                .is_ok()
        );

        store
            .revoke(&engagement.engagement_id, 3_000)
            .await
            .unwrap();

        let refusal = root_authority_for_work_with_store(&store, "alpha", "prod", &work, 4_000)
            .await
            .expect_err("a revoked engagement authorises nothing");
        assert!(
            refusal.contains("Revoked"),
            "the refusal must say which denial it was, got: {refusal}"
        );
    }

    /// Pins: expiry degrades to no root run, from the clock, with no owner act.
    ///
    /// The same engagement authorises a root one millisecond before its expiry
    /// and refuses at it — asserted in that order so the refusal cannot be an
    /// empty fixture.
    #[tokio::test]
    async fn an_expired_engagement_cannot_start_a_root_run() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let work = WorkContextKind::Engagement(engagement.engagement_id.clone());

        assert!(
            root_authority_for_work_with_store(&store, "alpha", "prod", &work, 99_999)
                .await
                .is_ok()
        );
        let refusal = root_authority_for_work_with_store(&store, "alpha", "prod", &work, 100_000)
            .await
            .expect_err("expiring at an instant means expired at that instant");
        assert!(refusal.contains("Expired"), "got: {refusal}");
    }

    /// Pins: an engagement from another scope is unknown, not borrowable.
    ///
    /// A root created under a scope that does not hold the engagement would be
    /// the cross-tenant authority hand-off every store here is arranged to
    /// prevent.
    #[tokio::test]
    async fn a_root_cannot_borrow_another_scopes_engagement() {
        let temp = tempfile::tempdir().unwrap();
        let (store, engagement) = store_with_one(&temp).await;
        let work = WorkContextKind::Engagement(engagement.engagement_id.clone());

        let refusal = root_authority_for_work_with_store(&store, "beta", "prod", &work, 2_000)
            .await
            .expect_err("another principal must not reach this engagement");
        assert!(refusal.contains("UnknownEngagement"), "got: {refusal}");
    }

    /// Pins: a work id that cannot safely name a record is refused BEFORE any
    /// read, and the refusal names U+001F.
    ///
    /// A blank id addresses every record and none of them; an id carrying the
    /// field separator can bleed into the next component of a derived key. Both
    /// are caller strings feeding an id derivation, so both are refused rather
    /// than looked up.
    #[tokio::test]
    async fn a_blank_or_separator_carrying_work_id_is_refused_before_any_read() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _engagement) = store_with_one(&temp).await;

        let blank = root_authority_for_work_with_store(
            &store,
            "alpha",
            "prod",
            &WorkContextKind::Engagement("   ".to_string()),
            2_000,
        )
        .await
        .expect_err("a blank id names nothing");
        assert!(blank.contains("must name something"), "got: {blank}");

        let separated = root_authority_for_work_with_store(
            &store,
            "alpha",
            "prod",
            &WorkContextKind::Engagement("eng\u{1f}1".to_string()),
            2_000,
        )
        .await
        .expect_err("U+001F must never reach an id derivation");
        assert!(separated.contains("U+001F"), "got: {separated}");
    }

    /// Pins: a PROGRAM work context is refused by name, never dropped.
    ///
    /// The durable carrier now holds a program arm, so the reason moved: what
    /// is missing is a **roster that owns programs** — something to read a
    /// liveness answer and an authority revision from. Inventing a revision
    /// here would be the caller-supplied authority §4.2c row 5 forbids, and
    /// returning "no authority" would start the run **unconfined** while the
    /// caller believed it was confined, which nothing downstream can tell apart
    /// from a run that never asked.
    #[tokio::test]
    async fn a_program_work_context_is_refused_rather_than_silently_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _engagement) = store_with_one(&temp).await;

        let refusal = root_authority_for_work_with_store(
            &store,
            "alpha",
            "prod",
            &WorkContextKind::Program("recruiting".to_string()),
            2_000,
        )
        .await
        .expect_err("a program root has no roster to validate it against");
        assert!(
            refusal.contains("recruiting") && refusal.contains("Refused rather than"),
            "the refusal must name the work and say it was not dropped, got: {refusal}"
        );
        assert!(
            refusal.contains("no roster owns programs"),
            "the refusal must name the piece that is missing, so nobody re-reads it as `the \
             record cannot hold it`, got: {refusal}"
        );
    }
}
