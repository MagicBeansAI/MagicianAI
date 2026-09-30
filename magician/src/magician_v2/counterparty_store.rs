//! The register: one append-only log per scope, folded on read.
//!
//! # Why one log, and not one per counterparty
//!
//! The question this module answers most often is *"this address just arrived —
//! whose is it?"*, and that question is scope-wide by nature. A per-counterparty
//! layout would make [`CounterpartyStore::resolve`] a scan across every
//! organisation on file, and a scan is where a resolver starts guessing to stay
//! fast. One log makes the exact answer the cheap one.
//!
//! # Index before row
//!
//! Inside the one log the ordering is still **index before row**: a
//! counterparty's own record is appended before any identity that names it, and
//! [`CounterpartyStore::add_identity`] refuses a counterparty the register does
//! not already hold.
//!
//! A crash between the two appends therefore leaves a counterparty with no
//! addresses — visible, reviewable, and harmless — rather than an address whose
//! organisation no read can name. The second is the dangerous direction: an
//! identity pointing at a row that does not exist is an address that resolves to
//! a phantom, and a phantom is something a caller would have to invent a policy
//! for.
//!
//! # What the fold is allowed to do
//!
//! Only what the log says. Derived states are computed from the clock or from
//! the merge edges at read time and never stored, so nothing depends on a sweep
//! having run. Terminal states — a merged record, a completed promotion — are
//! first-write-wins in the fold as well as at the write, because a duplicate
//! line from an older binary must not move the one decision an owner can audit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::audience::{Audience, AudienceKind, AudienceRef};

use crate::magician_v2::counterparty_types::{
    email_domain_of, normalise_identity, AddIdentity, Counterparty, CounterpartyRef,
    CounterpartySummary, CreateCounterparty, Identity, IdentityKind, MergeDecision, Minting,
    Promotion, Stage, Verification, FIELD_SEP,
};

/// How far a merge chain may be walked before the walk is declared malformed.
///
/// The visited set alone already guarantees termination; this is the second
/// stop, so a log that somehow produced a very long chain answers with an error
/// in bounded time rather than making a caller wait to find out.
const MERGE_CHAIN_LIMIT: usize = 64;

static GLOBAL_COUNTERPARTIES: OnceLock<Arc<CounterpartyStore>> = OnceLock::new();

/// Install the process-wide register. First call wins so a later caller cannot
/// redirect live reads to another workspace root.
pub fn install_global_counterparty_store(store: Arc<CounterpartyStore>) -> bool {
    GLOBAL_COUNTERPARTIES.set(store).is_ok()
}

/// Return the installed register. Absence means unreadable/unconfigured, never
/// an empty counterparty book.
pub fn global_counterparty_store() -> Option<Arc<CounterpartyStore>> {
    GLOBAL_COUNTERPARTIES.get().cloned()
}

/// Scope for a store call.
///
/// `principal` and `workspace` feed every derived id, so both are checked for
/// the separator before anything is derived from them: a principal carrying
/// U+001F could fuse one owner's book into another's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterpartyScope {
    pub principal: String,
    pub workspace: String,
}

impl CounterpartyScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }

    fn checked(&self) -> Result<()> {
        guard_component("a scope principal", &self.principal)?;
        guard_component("a scope workspace", &self.workspace)?;
        Ok(())
    }
}

/// One line in a scope's register.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum CounterpartyRecord {
    CounterpartyRecorded(Counterparty),
    StageSet {
        counterparty_id: String,
        stage: Stage,
        decided_by: String,
        at: DateTime<Utc>,
    },
    DomainSet {
        counterparty_id: String,
        domain: String,
        decided_by: String,
        at: DateTime<Utc>,
    },
    IdentityRecorded(Identity),
    IdentityObserved {
        identity_id: String,
        at: DateTime<Utc>,
    },
    IdentityPromoted {
        identity_id: String,
        channel: String,
        evidence_ref: String,
        decided_by: String,
        at: DateTime<Utc>,
    },
    Merged {
        from_counterparty_id: String,
        into_counterparty_id: String,
        decided_by: String,
        evidence_ref: String,
        at: DateTime<Utc>,
    },
}

/// The folded register — every organisation in the scope and every address
/// filed under one.
#[derive(Debug, Default)]
struct Register {
    counterparties: Vec<Counterparty>,
    identities: Vec<Identity>,
}

impl Register {
    fn counterparty(&self, counterparty_id: &str) -> Option<&Counterparty> {
        self.counterparties
            .iter()
            .find(|held| held.counterparty_id == counterparty_id)
    }

    fn identity(&self, identity_id: &str) -> Option<&Identity> {
        self.identities
            .iter()
            .find(|held| held.identity_id == identity_id)
    }

    /// Every id on the merge chain starting at `counterparty_id`, the head last.
    ///
    /// **This is the function that must not loop.** Merge edges are written by
    /// callers and folded from a log that a second process may be appending to,
    /// so a cycle is a state the reader has to survive even though
    /// [`CounterpartyStore::merge`] refuses to create one. A revisited id or an
    /// over-long walk is an **error**, never a best guess at the head: an
    /// organisation whose identity cannot be established is not an organisation
    /// a caller may route to.
    fn chain_from(&self, counterparty_id: &str) -> Result<Vec<String>> {
        let mut seen = BTreeSet::new();
        let mut walked: Vec<String> = Vec::new();
        let mut cursor = counterparty_id.to_string();
        loop {
            if !seen.insert(cursor.clone()) {
                anyhow::bail!(
                    "the merge chain from `{counterparty_id}` revisits `{cursor}`: the log holds \
                     a cycle, and a cycle has no head. Refusing rather than picking one, because \
                     picking one would route this organisation's mail to whichever record the \
                     walk happened to stop on"
                );
            }
            if walked.len() >= MERGE_CHAIN_LIMIT {
                anyhow::bail!(
                    "the merge chain from `{counterparty_id}` is longer than {MERGE_CHAIN_LIMIT} \
                     hops, which no sequence of owner decisions produces; the log is malformed"
                );
            }
            let Some(held) = self.counterparty(&cursor) else {
                anyhow::bail!(
                    "counterparty `{cursor}` is named by the register but has no record of its \
                     own. Index-before-row means this cannot happen through the store, so the \
                     log is damaged; an address whose organisation cannot be named must not \
                     resolve to anything"
                );
            };
            walked.push(cursor.clone());
            match &held.merged_into {
                None => return Ok(walked),
                Some(next) => cursor = next.clone(),
            }
        }
    }

    /// The record an id ultimately points at.
    fn live_head(&self, counterparty_id: &str) -> Result<String> {
        let walked = self.chain_from(counterparty_id)?;
        walked
            .last()
            .cloned()
            .context("a merge chain walk produced no ids, which is impossible")
    }
}

/// Who we are talking to, and the addresses that are them.
#[derive(Debug, Clone)]
pub struct CounterpartyStore {
    workspace_layout: ArtifactV2Workspace,
}

impl CounterpartyStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    fn root(&self, scope: &CounterpartyScope) -> PathBuf {
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("counterparties")
    }

    /// The one log. Exposed to this module's own tests so a damaged log can be
    /// constructed on purpose; it is not part of the public surface.
    pub(super) fn register_path(&self, scope: &CounterpartyScope) -> PathBuf {
        self.root(scope).join("register.jsonl")
    }

    // ── Organisations ───────────────────────────────────────────────────────

    /// Record an organisation, or return the one already on file.
    ///
    /// Idempotent on the **derived id**, which comes from the display name's
    /// comparison key — so the same organisation recorded twice by two callers,
    /// or twice by one caller that never saw our first response, is one row.
    /// Case and spacing differences resume rather than error: they are two
    /// spellings of one name, and the first spelling recorded is the one shown.
    ///
    /// A replay that carries a **different** `domain` or `stage` is an error
    /// naming [`Self::set_domain`] or [`Self::set_stage`], never a silent
    /// no-op — the caller would otherwise be left believing the register holds
    /// what it just sent while the register holds something else. A replay that
    /// omits either field carries no opinion about it and resumes.
    ///
    /// `created_by` is **not** compared. Who first wrote an organisation down is
    /// a fact about that moment; a second actor recording the same organisation
    /// is not a contradiction of it.
    ///
    /// Recording a name that has since been **merged away** is refused. Merged
    /// is terminal, and handing back the dead row would invite the caller to
    /// file addresses under it.
    pub fn record_counterparty(
        &self,
        scope: &CounterpartyScope,
        request: &CreateCounterparty,
        now: DateTime<Utc>,
    ) -> Result<Counterparty> {
        scope.checked()?;
        guard_component("a counterparty display name", &request.display_name)?;
        guard_present("`created_by`", &request.created_by)?;
        let domain = match &request.domain {
            Some(domain) => Some(normalise_identity(IdentityKind::Domain, domain)?),
            None => None,
        };

        let counterparty_id = derive_counterparty_id(scope, &request.display_name);
        let register = self.register(scope)?;
        if let Some(existing) = register.counterparty(&counterparty_id) {
            if let Some(head) = &existing.merged_into {
                anyhow::bail!(
                    "counterparty `{counterparty_id}` was merged into `{head}` and a merged \
                     record never resurrects; record against `{head}` instead"
                );
            }
            if let Some(domain) = &domain {
                if existing.domain.as_deref() != Some(domain.as_str()) {
                    anyhow::bail!(
                        "counterparty `{counterparty_id}` is already recorded with domain {:?}, \
                         not `{domain}`. An identical replay resumes; changing the domain is \
                         `set_domain`, so that the change is a line in the log rather than a \
                         silently discarded write",
                        existing.domain
                    );
                }
            }
            if let Some(stage) = &request.stage {
                if existing.stage.as_ref().map(Stage::key) != Some(stage.key()) {
                    anyhow::bail!(
                        "counterparty `{counterparty_id}` is already at stage {:?}, not `{}`. \
                         The stage on a create request is the INITIAL stage; moving it is \
                         `set_stage`",
                        existing.stage.as_ref().map(Stage::as_str),
                        stage.as_str()
                    );
                }
            }
            return Ok(existing.clone());
        }

        let counterparty = Counterparty {
            counterparty_id,
            display_name: request.display_name.trim().to_string(),
            domain,
            stage: request.stage.clone(),
            created_at: now,
            created_by: request.created_by.trim().to_string(),
            merged_into: None,
        };
        self.append(
            scope,
            &CounterpartyRecord::CounterpartyRecorded(counterparty.clone()),
        )?;
        Ok(counterparty)
    }

    /// Move an organisation to a stage the owner names.
    ///
    /// A stage is a label, not a state machine: this module records the one the
    /// owner last set and takes no view on which order they come in. Setting the
    /// stage it is already at appends nothing.
    pub fn set_stage(
        &self,
        scope: &CounterpartyScope,
        counterparty_id: &str,
        stage: &Stage,
        decided_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Counterparty> {
        scope.checked()?;
        guard_present("`decided_by`", decided_by)?;
        let register = self.register(scope)?;
        let existing = self.require_live(&register, counterparty_id)?;
        if existing.stage.as_ref().map(Stage::key) == Some(stage.key()) {
            return Ok(existing);
        }
        self.append(
            scope,
            &CounterpartyRecord::StageSet {
                counterparty_id: existing.counterparty_id.clone(),
                stage: stage.clone(),
                decided_by: decided_by.trim().to_string(),
                at: now,
            },
        )?;
        self.load(scope, &existing.counterparty_id)?
            .context("counterparty vanished immediately after its stage was set")
    }

    /// Correct the domain hint.
    ///
    /// The domain is a hint for candidate review and never a resolution key, so
    /// changing it cannot move anyone's authority — but it still goes in the log
    /// rather than being edited in place, because an append-only store with no
    /// correction path forces an owner to create a second organisation instead.
    pub fn set_domain(
        &self,
        scope: &CounterpartyScope,
        counterparty_id: &str,
        domain: &str,
        decided_by: &str,
        now: DateTime<Utc>,
    ) -> Result<Counterparty> {
        scope.checked()?;
        guard_present("`decided_by`", decided_by)?;
        let domain = normalise_identity(IdentityKind::Domain, domain)?;
        let register = self.register(scope)?;
        let existing = self.require_live(&register, counterparty_id)?;
        if existing.domain.as_deref() == Some(domain.as_str()) {
            return Ok(existing);
        }
        self.append(
            scope,
            &CounterpartyRecord::DomainSet {
                counterparty_id: existing.counterparty_id.clone(),
                domain,
                decided_by: decided_by.trim().to_string(),
                at: now,
            },
        )?;
        self.load(scope, &existing.counterparty_id)?
            .context("counterparty vanished immediately after its domain was set")
    }

    // ── Addresses ───────────────────────────────────────────────────────────

    /// File an address under an organisation.
    ///
    /// The derived id covers `(scope, kind, normalised value)` and deliberately
    /// **not** the counterparty: an address belongs to at most one organisation,
    /// so a second call binding the same address to a different one derives the
    /// same id and is refused. That refusal is the whole point of the module —
    /// two rows for one address would make [`Self::resolve`] ambiguous, and an
    /// ambiguous resolve is one counterparty's authority landing on another.
    ///
    /// An identical replay resumes and returns the row already written. A replay
    /// that changes the provenance, the evidence, the recorder or the introducer
    /// is an error: those four are what an owner reads when deciding whether to
    /// trust the address, and quietly keeping the first would leave the caller
    /// believing it changed them.
    ///
    /// The counterparty must already be recorded and still live — see the
    /// module's index-before-row note.
    ///
    /// Nothing here verifies anything. `first_seen` and `last_seen` both start
    /// at `now`, and [`Verification::Unverified`] is the only state this call
    /// can produce, whatever the provenance says.
    pub fn add_identity(
        &self,
        scope: &CounterpartyScope,
        request: &AddIdentity,
        now: DateTime<Utc>,
    ) -> Result<Identity> {
        scope.checked()?;
        guard_present("an identity's `evidence_ref`", &request.evidence_ref)?;
        guard_present("an identity's `recorded_by`", &request.recorded_by)?;
        let normalised = normalise_identity(request.kind, &request.value)?;

        let introduced_by = match (&request.introduced_by, request.source.requires_introducer()) {
            (Some(introducer), true) => {
                guard_present("an introduction's `introduced_by`", introducer)?;
                Some(introducer.trim().to_string())
            },
            (None, true) => anyhow::bail!(
                "an identity minted as `introduced` must name who introduced it; an introduction \
                 with no introducer is a provenance label with nothing behind it"
            ),
            (Some(_), false) => anyhow::bail!(
                "`introduced_by` is only meaningful when the source is `introduced`; naming an \
                 introducer on a `{}` identity would record a vouch nobody made",
                request.source.as_str()
            ),
            (None, false) => None,
        };

        let register = self.register(scope)?;
        let counterparty = self.require_live(&register, &request.counterparty_id)?;
        let identity_id = derive_identity_id(scope, request.kind, &normalised);

        if let Some(existing) = register.identity(&identity_id) {
            if existing.counterparty_id != counterparty.counterparty_id {
                anyhow::bail!(
                    "`{normalised}` is already filed under counterparty `{}`, so it cannot also \
                     be filed under `{}`. One address belongs to one organisation: two rows would \
                     make resolution ambiguous, and an ambiguous resolution hands one \
                     counterparty's authority to another. If these are one organisation, `merge` \
                     them",
                    existing.counterparty_id,
                    counterparty.counterparty_id
                );
            }
            if existing.minted_by.source != request.source
                || existing.minted_by.evidence_ref != request.evidence_ref.trim()
                || existing.minted_by.recorded_by != request.recorded_by.trim()
                || existing.introduced_by != introduced_by
            {
                anyhow::bail!(
                    "identity `{identity_id}` is already recorded with different provenance \
                     (source `{}`, evidence `{}`, recorded by `{}`). An identical replay resumes; \
                     a changed payload is an error, because provenance is what an owner reads \
                     before trusting an address and silently keeping the first would hide the \
                     disagreement",
                    existing.minted_by.source.as_str(),
                    existing.minted_by.evidence_ref,
                    existing.minted_by.recorded_by
                );
            }
            return Ok(existing.clone());
        }

        let identity = Identity {
            identity_id,
            counterparty_id: counterparty.counterparty_id.clone(),
            kind: request.kind,
            value: request.value.trim().to_string(),
            normalised,
            minted_by: Minting {
                source: request.source,
                evidence_ref: request.evidence_ref.trim().to_string(),
                recorded_by: request.recorded_by.trim().to_string(),
                at: now,
            },
            introduced_by,
            verification: Verification::Unverified,
            first_seen: now,
            last_seen: now,
        };
        self.append(
            scope,
            &CounterpartyRecord::IdentityRecorded(identity.clone()),
        )?;
        Ok(identity)
    }

    /// Note that we heard from an address again.
    ///
    /// **An observation is not a promotion.** It moves `last_seen` and nothing
    /// else: not the verification, not the provenance, not `first_seen`. Seeing
    /// an address a hundred times is a hundred repetitions of the same
    /// unauthenticated claim — SMTP does not authenticate `From:` — and a store
    /// that let volume ripen into trust would verify whoever was noisiest.
    ///
    /// `last_seen` only ever moves **forward**. An observation from a backfilled
    /// thread arriving out of order is recorded as having happened, but it does
    /// not rewrite when we last heard from an organisation, because "last heard
    /// from" is what a silence sweep reads.
    ///
    /// `first_seen` is set once, when the address is filed, and never moves.
    ///
    /// An address the register does not hold is an **error**, not a quiet
    /// create: minting on an observation would produce an identity whose
    /// provenance is "something mentioned it", which is exactly the rumour
    /// [`Minting`] exists to prevent.
    pub fn observe(
        &self,
        scope: &CounterpartyScope,
        identity_id: &str,
        at: DateTime<Utc>,
    ) -> Result<Identity> {
        scope.checked()?;
        let register = self.register(scope)?;
        let Some(existing) = register.identity(identity_id) else {
            anyhow::bail!(
                "no identity `{identity_id}` in this register. An observation does not mint an \
                 address: an address with no provenance is one nobody can decide about"
            );
        };
        if at <= existing.last_seen {
            return Ok(existing.clone());
        }
        self.append(
            scope,
            &CounterpartyRecord::IdentityObserved {
                identity_id: existing.identity_id.clone(),
                at,
            },
        )?;
        self.load_identity(scope, identity_id)?
            .context("identity vanished immediately after being observed")
    }

    /// The owner decision that turns an address into a verified one.
    ///
    /// Four things must hold, and each closes a way an automatic path could
    /// grant itself the only control this module has:
    ///
    /// 1. **A named decider and evidence.** An unnamed promotion is unauditable,
    ///    and one with nothing to point at cannot be checked.
    /// 2. **A server-trusted signal.** [`crate::magician_v2::chat::envoy::channel_is_verified`]
    ///    decides, not this module and not the caller: a caller's own claim may
    ///    de-escalate and may never raise, and a channel nobody has classified
    ///    fails closed.
    /// 3. **A guess may not be promoted on the strength of the guess.** For a
    ///    [`MintSource::ResearchInferred`](super::types::MintSource::ResearchInferred)
    ///    identity the promotion evidence must differ from the evidence that
    ///    minted it — a research note cannot be its own proof.
    /// 4. **And the guesser may not approve itself.** For that same inferred
    ///    identity, `decided_by` must differ from the `recorded_by` that filed
    ///    it. Inferred-and-unverified is the default state of everything
    ///    research produces; letting the researcher promote its own output is
    ///    how one counterparty's authority reaches another without anybody
    ///    choosing it.
    ///
    /// The first promotion is the promotion. An identical replay resumes; a
    /// promotion with different evidence, channel or decider is an error rather
    /// than an overwrite, because the recorded decision is the audit trail.
    pub fn promote_identity(
        &self,
        scope: &CounterpartyScope,
        identity_id: &str,
        promotion: &Promotion,
        now: DateTime<Utc>,
    ) -> Result<Identity> {
        scope.checked()?;
        guard_present("a promotion's `decided_by`", &promotion.decided_by)?;
        guard_present("a promotion's `evidence_ref`", &promotion.evidence_ref)?;

        let register = self.register(scope)?;
        let Some(existing) = register.identity(identity_id) else {
            anyhow::bail!("no identity `{identity_id}` in this register");
        };

        if !promotion.signal.is_trusted() {
            anyhow::bail!(
                "the signal offered for `{identity_id}` is not one a server stands behind: \
                 channel `{}` with request_authenticated={} and caller_claim={:?} does not \
                 establish who sent it. An unclassified channel fails closed, and a claim the \
                 subject supplies about itself is not a verification signal",
                promotion.signal.channel,
                promotion.signal.request_authenticated,
                promotion.signal.caller_claim
            );
        }

        if existing.is_inferred() {
            if promotion.evidence_ref.trim() == existing.minted_by.evidence_ref {
                anyhow::bail!(
                    "`{identity_id}` was inferred by research, and the evidence offered to \
                     promote it is the same research note that produced it. A guess cannot be \
                     its own proof; promotion needs something the inference did not author"
                );
            }
            if promotion
                .decided_by
                .trim()
                .eq_ignore_ascii_case(existing.minted_by.recorded_by.trim())
            {
                anyhow::bail!(
                    "`{identity_id}` was inferred by `{}`, which may not also be the one to \
                     approve it. Inferred-and-unverified is the default state of everything \
                     research produces, and an automatic path promoting its own output is how \
                     one counterparty's authority reaches another with nobody deciding",
                    existing.minted_by.recorded_by
                );
            }
        }

        if let Verification::Verified {
            channel,
            evidence_ref,
            decided_by,
            at,
        } = &existing.verification
        {
            if channel == &promotion.signal.channel
                && evidence_ref == promotion.evidence_ref.trim()
                && decided_by == promotion.decided_by.trim()
            {
                return Ok(existing.clone());
            }
            anyhow::bail!(
                "`{identity_id}` was already verified on `{channel}` by `{decided_by}` at {at} \
                 against `{evidence_ref}`. An identical replay resumes; a different promotion is \
                 an error, because overwriting it would erase the one decision an owner can audit"
            );
        }

        self.append(
            scope,
            &CounterpartyRecord::IdentityPromoted {
                identity_id: existing.identity_id.clone(),
                channel: promotion.signal.channel.clone(),
                evidence_ref: promotion.evidence_ref.trim().to_string(),
                decided_by: promotion.decided_by.trim().to_string(),
                at: now,
            },
        )?;
        self.load_identity(scope, identity_id)?
            .context("identity vanished immediately after being promoted")
    }

    // ── Resolution ──────────────────────────────────────────────────────────

    /// Whose address is this? **Exact match on the normalised value only.**
    ///
    /// This is the lookup scheduling, envelopes and inbound routing all need,
    /// and it is deliberately the dullest function in the module. There is no
    /// fuzzy match, no edit distance, no "same domain so probably the same
    /// organisation", and no public-suffix logic. Every one of those is a guess,
    /// and a wrong guess here is one counterparty's authority landing on
    /// another. The only fold performed is the one written down in
    /// [`normalise_identity`]; a near miss resolves to `None`.
    ///
    /// Merge edges are followed, so an address filed under a name that has since
    /// been folded into another resolves to the organisation that survived.
    ///
    /// # Resolving is not authorising
    ///
    /// A match here says *"we have this address on file"*. It does not say the
    /// address was ever proved to reach whom we think — an address minted from
    /// research is a guess that resolves. Anything that **grants** should use
    /// [`Self::resolve_verified`] or read the identity's own
    /// [`Verification`].
    ///
    /// `None` means "not on file", which is never permission for anything.
    pub fn resolve(
        &self,
        scope: &CounterpartyScope,
        kind: IdentityKind,
        value: &str,
    ) -> Result<Option<CounterpartyRef>> {
        self.resolve_matching(scope, kind, value, |_| true)
    }

    /// [`Self::resolve`], restricted to addresses a server-trusted signal has
    /// proved.
    ///
    /// The one to use where a match confers something — routing an inbound
    /// message into an existing context, or admitting a reply to a bounded set.
    /// An address that is merely on file resolves to `None` here, so the failure
    /// mode is "treated as a stranger", which is the recoverable direction.
    pub fn resolve_verified(
        &self,
        scope: &CounterpartyScope,
        kind: IdentityKind,
        value: &str,
    ) -> Result<Option<CounterpartyRef>> {
        self.resolve_matching(scope, kind, value, Identity::is_verified)
    }

    fn resolve_matching(
        &self,
        scope: &CounterpartyScope,
        kind: IdentityKind,
        value: &str,
        admit: impl Fn(&Identity) -> bool,
    ) -> Result<Option<CounterpartyRef>> {
        scope.checked()?;
        let normalised = normalise_identity(kind, value)?;
        let register = self.register(scope)?;
        let Some(identity) = register
            .identities
            .iter()
            .find(|held| held.kind == kind && held.normalised == normalised)
        else {
            return Ok(None);
        };
        if !admit(identity) {
            return Ok(None);
        }
        Ok(Some(CounterpartyRef::new(
            register.live_head(&identity.counterparty_id)?,
        )))
    }

    /// Organisations that *might* be behind a domain — **for owner review only.**
    ///
    /// This is not a resolution and must never be used as one. A domain match is
    /// affiliation, not authorisation: holding a mailbox at a large company
    /// proves somebody works there, and nothing more. Its job is to make an
    /// owner's decision cheap — the evidence is already assembled — not to make
    /// the decision.
    ///
    /// Callers must therefore treat the result as a list to show a human. In
    /// particular a **single** candidate is still a candidate, and an **empty**
    /// list means "nothing to review", never "cleared" and never "resolved".
    ///
    /// A counterparty is a candidate if its domain hint matches, if it holds a
    /// domain identity that matches, or if it holds an email identity in that
    /// domain. Results are the live records, deduplicated across merges, ordered
    /// by display name so the list reads the same on every call. Each is
    /// returned as a [`CounterpartySummary`] — counts, so the reviewer can see
    /// how much of a record is inference before deciding.
    pub fn candidates_by_domain(
        &self,
        scope: &CounterpartyScope,
        domain: &str,
    ) -> Result<Vec<CounterpartySummary>> {
        scope.checked()?;
        let domain = normalise_identity(IdentityKind::Domain, domain)?;
        let register = self.register(scope)?;

        let mut heads: Vec<String> = Vec::new();
        let mut seen = BTreeSet::new();
        for counterparty in &register.counterparties {
            if counterparty.domain.as_deref() == Some(domain.as_str()) {
                note_head(
                    register.live_head(&counterparty.counterparty_id)?,
                    &mut heads,
                    &mut seen,
                );
            }
        }
        for identity in &register.identities {
            let hit = match identity.kind {
                IdentityKind::Domain => identity.normalised == domain,
                IdentityKind::Email => {
                    email_domain_of(&identity.normalised) == Some(domain.as_str())
                },
                IdentityKind::Phone | IdentityKind::Handle => false,
            };
            if hit {
                note_head(
                    register.live_head(&identity.counterparty_id)?,
                    &mut heads,
                    &mut seen,
                );
            }
        }

        let mut out = Vec::with_capacity(heads.len());
        for head in heads {
            out.push(summarise(&register, &head)?);
        }
        out.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.counterparty_id.cmp(&right.counterparty_id))
        });
        Ok(out)
    }

    // ── Reading an organisation ─────────────────────────────────────────────

    /// Every address on file for an organisation, merges followed.
    ///
    /// Ordered by `(kind, normalised)` so the list reads the same on every call.
    ///
    /// An organisation the register does not hold is an **error**, not an empty
    /// list. "We have never heard of them" and "we know them and have no way to
    /// reach them" are different answers, and collapsing the first into the
    /// second is how an unknown organisation comes to look like a known one with
    /// nothing owed to it.
    pub fn identities_for(
        &self,
        scope: &CounterpartyScope,
        counterparty_id: &str,
    ) -> Result<Vec<Identity>> {
        scope.checked()?;
        let register = self.register(scope)?;
        let head = self.require_head(&register, counterparty_id)?;
        gather_identities(&register, &head)
    }

    /// Resolve many work labels from **one** read of the register.
    ///
    /// The batch form of `consumers::resolve_label`, and the same answer for
    /// each: `None` when the register holds no organisation under the label's
    /// derived id, and otherwise the **surviving** organisation — merge edges
    /// followed, exactly as `summary` follows them.
    ///
    /// # Why this exists beside the single form
    ///
    /// `resolve_label` costs TWO register reads per label (`load` then
    /// `summary`), and its natural caller is a loop: the inbound engagement
    /// lane resolves the label of every live engagement to find which one a
    /// proved sender belongs to. That is `2N` full reads and parses of the
    /// register on a chat request — for a question that is one read of the same
    /// bytes.
    ///
    /// The comparison key is derived, not stored, so the labels do not need to
    /// be spelled the way the organisation was recorded: case and spacing
    /// differences resolve to the same id here exactly as they do at the write.
    pub fn resolve_labels(
        &self,
        scope: &CounterpartyScope,
        labels: &[String],
    ) -> Result<BTreeMap<String, Option<CounterpartyRef>>> {
        scope.checked()?;
        let register = self.register(scope)?;
        let mut out: BTreeMap<String, Option<CounterpartyRef>> = BTreeMap::new();
        for label in labels {
            if out.contains_key(label) {
                continue;
            }
            // A label the guards refuse is `None`, not an error: it is an owner
            // typing a work label, and refusing the whole batch because one
            // engagement carries a blank one would take the inbound lane down
            // for every other engagement.
            let Ok(would_be) = counterparty_id_for(scope, label) else {
                out.insert(label.clone(), None);
                continue;
            };
            let resolved = match register.counterparty(&would_be) {
                // Present — including a row that has since been merged away,
                // which is why the head is walked rather than the id returned.
                Some(_) => Some(CounterpartyRef::new(register.live_head(&would_be)?)),
                None => None,
            };
            out.insert(label.clone(), resolved);
        }
        Ok(out)
    }

    /// Every organisation's addresses, from **one** read of the register.
    ///
    /// Keyed by the LIVE head, so a merged-away organisation's addresses appear
    /// under the record that survived — the same answer
    /// [`Self::identities_for`] gives, for every organisation at once.
    ///
    /// # Why this exists beside `identities_for`
    ///
    /// Because asking that question in a loop is quadratic and the shape is
    /// easy to miss: each call re-reads and re-folds the WHOLE register, so
    /// walking N organisations parses the file N times. It is invisible at ten
    /// organisations and it is the read path at a thousand. Anything that wants
    /// the register as a whole — a referral graph, an export, an audit — should
    /// take it in one read.
    pub fn identities_by_counterparty(
        &self,
        scope: &CounterpartyScope,
    ) -> Result<BTreeMap<String, Vec<Identity>>> {
        scope.checked()?;
        let register = self.register(scope)?;
        let mut out: BTreeMap<String, Vec<Identity>> = BTreeMap::new();
        for identity in &register.identities {
            // A merge chain that cannot be walked is an error here exactly as
            // it is in `identities_for`: an address filed under an id whose
            // head is unreachable belongs to nobody we can name, and filing it
            // under the id it happens to carry would attribute it to a record
            // that was merged away.
            let head = register.live_head(&identity.counterparty_id)?;
            out.entry(head).or_default().push(identity.clone());
        }
        Ok(out)
    }

    /// The addresses a server-trusted signal has proved, merges followed.
    ///
    /// The subset anything that grants should be reading. It can legitimately be
    /// **empty** — an organisation whose addresses are all unverified is normal,
    /// and it means nothing here may be treated as reaching them.
    pub fn verified_identities_for(
        &self,
        scope: &CounterpartyScope,
        counterparty_id: &str,
    ) -> Result<Vec<Identity>> {
        Ok(self
            .identities_for(scope, counterparty_id)?
            .into_iter()
            .filter(Identity::is_verified)
            .collect())
    }

    /// The organisation as an [`Audience`] — **verified addresses only.**
    ///
    /// The bridge to [`crate::magician_v2::audience`], and the one place an
    /// organisation turns into a set of people something may be shared with. Only
    /// verified identities cross it: an audience is what admits somebody, and
    /// admitting an address nobody proved is the authority hand-off the whole
    /// module exists to prevent.
    ///
    /// # An empty audience admits nobody, and that is the point
    ///
    /// A counterparty with no verified address yields an audience with no
    /// members. [`Audience::admits`] then answers `false` for everyone,
    /// including for addresses this register holds as unverified. A caller must
    /// never read "the audience is empty" as "no restriction applies" — a
    /// predicate over an empty set that passes a membership check is the bug
    /// this shape is built to make impossible.
    ///
    /// `kind` is the caller's, because the same organisation is an engagement in
    /// one flow and an account in another, and
    /// [`AudienceRef::as_key`] keeps those apart. No expiry is set: a
    /// counterparty relationship has no scheduled end of its own, and whoever
    /// governs the relationship can add one.
    pub fn audience_for(
        &self,
        scope: &CounterpartyScope,
        counterparty_id: &str,
        kind: AudienceKind,
    ) -> Result<Audience> {
        scope.checked()?;
        let register = self.register(scope)?;
        let head = self.require_head(&register, counterparty_id)?;
        let identities = gather_identities(&register, &head)?
            .into_iter()
            .filter(Identity::is_verified)
            .map(|identity| identity.normalised)
            .collect();
        Ok(Audience::new(AudienceRef::new(kind, head), identities))
    }

    /// Counts for one organisation. Counts, never rates.
    pub fn summary(
        &self,
        scope: &CounterpartyScope,
        counterparty_id: &str,
    ) -> Result<CounterpartySummary> {
        scope.checked()?;
        let register = self.register(scope)?;
        let head = self.require_head(&register, counterparty_id)?;
        summarise(&register, &head)
    }

    /// One organisation, exactly as recorded — merged records included, so a
    /// caller can see that a name was folded away and where it went.
    pub fn load(
        &self,
        scope: &CounterpartyScope,
        counterparty_id: &str,
    ) -> Result<Option<Counterparty>> {
        scope.checked()?;
        Ok(self.register(scope)?.counterparty(counterparty_id).cloned())
    }

    /// One address.
    pub fn load_identity(
        &self,
        scope: &CounterpartyScope,
        identity_id: &str,
    ) -> Result<Option<Identity>> {
        scope.checked()?;
        Ok(self.register(scope)?.identity(identity_id).cloned())
    }

    /// Every organisation still on its own row, oldest first.
    ///
    /// Merged records are omitted: they are names, not organisations, and a list
    /// that showed both would double-count the book.
    pub fn list(&self, scope: &CounterpartyScope) -> Result<Vec<Counterparty>> {
        scope.checked()?;
        let mut out: Vec<Counterparty> = self
            .register(scope)?
            .counterparties
            .into_iter()
            .filter(Counterparty::is_live)
            .collect();
        out.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.counterparty_id.cmp(&right.counterparty_id))
        });
        Ok(out)
    }

    // ── Merging ─────────────────────────────────────────────────────────────

    /// Fold one organisation into another: they were always one company.
    ///
    /// Append-only, so nothing is rewritten. A `Merged` edge is recorded on
    /// `from` and every read follows it — addresses filed under the old name
    /// keep saying they were filed under the old name, which is a fact worth
    /// keeping, while [`Self::resolve`] and [`Self::identities_for`] answer with
    /// the record that survived.
    ///
    /// Refused when:
    ///
    /// - the two are the same record — nothing to fold, and the edge would be a
    ///   one-hop cycle;
    /// - `from` has already been merged — merged is **terminal**, and a second
    ///   edge would silently move an organisation's whole address book;
    /// - `into` has itself been merged away — the caller is pointing at a name
    ///   rather than an organisation, and the error names the live head so the
    ///   real decision can be recorded instead;
    /// - the edge would close a **cycle**. A cycle has no head, and a fold that
    ///   walked one would either loop forever or stop on whichever record it
    ///   happened to reach.
    ///
    /// Replaying the identical merge resumes.
    pub fn merge(
        &self,
        scope: &CounterpartyScope,
        from_counterparty_id: &str,
        into_counterparty_id: &str,
        decision: &MergeDecision,
        now: DateTime<Utc>,
    ) -> Result<Counterparty> {
        scope.checked()?;
        guard_present("a merge's `decided_by`", &decision.decided_by)?;
        guard_present("a merge's `evidence_ref`", &decision.evidence_ref)?;
        if from_counterparty_id == into_counterparty_id {
            anyhow::bail!(
                "`{from_counterparty_id}` cannot be merged into itself; the edge would be a cycle \
                 one hop long and the record would have no head"
            );
        }

        let register = self.register(scope)?;
        let Some(from) = register.counterparty(from_counterparty_id) else {
            anyhow::bail!("no counterparty `{from_counterparty_id}` in this register");
        };
        let Some(into) = register.counterparty(into_counterparty_id) else {
            anyhow::bail!("no counterparty `{into_counterparty_id}` in this register");
        };

        if let Some(already) = &from.merged_into {
            if already == into_counterparty_id {
                return Ok(from.clone());
            }
            anyhow::bail!(
                "`{from_counterparty_id}` was already merged into `{already}`. A merged record \
                 never merges again: a second edge would move an organisation's entire address \
                 book without anyone deciding it should move"
            );
        }
        // The cycle check comes FIRST, before the "into is a name" check. The
        // commonest way to ask for a cycle is to merge B into A immediately
        // after merging A into B — a plausible owner slip — and the refusal
        // should say *cycle* rather than blame the target for having been
        // merged. It also generalises: any length of chain that already
        // resolves back through `from` is caught here.
        if register
            .chain_from(into_counterparty_id)?
            .iter()
            .any(|walked| walked == from_counterparty_id)
        {
            anyhow::bail!(
                "merging `{from_counterparty_id}` into `{into_counterparty_id}` would close a \
                 cycle: `{into_counterparty_id}` already resolves through \
                 `{from_counterparty_id}`. A cycle has no head, and a fold that walked one would \
                 either never terminate or stop on whichever record it happened to reach"
            );
        }
        if let Some(head) = &into.merged_into {
            anyhow::bail!(
                "`{into_counterparty_id}` has itself been merged into `{head}`, so it is a name \
                 rather than an organisation; merge into `{head}` if that is the decision"
            );
        }

        self.append(
            scope,
            &CounterpartyRecord::Merged {
                from_counterparty_id: from_counterparty_id.to_string(),
                into_counterparty_id: into_counterparty_id.to_string(),
                decided_by: decision.decided_by.trim().to_string(),
                evidence_ref: decision.evidence_ref.trim().to_string(),
                at: now,
            },
        )?;
        self.load(scope, from_counterparty_id)?
            .context("counterparty vanished immediately after being merged")
    }

    // ── Internals ───────────────────────────────────────────────────────────

    /// Fold the log.
    ///
    /// First write wins for every row and for every terminal transition,
    /// defensively as well as at the write: a duplicated line from an older
    /// binary or a second process must not turn one address into two, nor move
    /// the moment an owner verified something.
    fn register(&self, scope: &CounterpartyScope) -> Result<Register> {
        let path = self.register_path(scope);
        // NotFound is the only error that reads as an empty register. Everything
        // else propagates: an unreadable log folded to "empty" would answer
        // "we have never heard of them" to every question, which is a stranger
        // becoming a stranger's authority the moment somebody records them
        // again. Shared semantics live in `magician_v2::jsonl`.
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(Register::default());
        };

        let mut register = Register::default();
        let mut counterparties_seen = BTreeSet::new();
        let mut identities_seen = BTreeSet::new();
        // Tolerant of a torn tail only — see `magician_v2::jsonl`.
        for record in crate::magician_v2::jsonl::parse_log_lines::<CounterpartyRecord>(&raw, &path)?
        {
            match record {
                CounterpartyRecord::CounterpartyRecorded(counterparty) => {
                    if counterparties_seen.insert(counterparty.counterparty_id.clone()) {
                        register.counterparties.push(counterparty);
                    }
                },
                CounterpartyRecord::StageSet {
                    counterparty_id,
                    stage,
                    ..
                } => {
                    if let Some(held) = register
                        .counterparties
                        .iter_mut()
                        .find(|held| held.counterparty_id == counterparty_id)
                    {
                        // Last write wins: a stage is the label the owner last
                        // set, not a state machine.
                        held.stage = Some(stage);
                    }
                },
                CounterpartyRecord::DomainSet {
                    counterparty_id,
                    domain,
                    ..
                } => {
                    if let Some(held) = register
                        .counterparties
                        .iter_mut()
                        .find(|held| held.counterparty_id == counterparty_id)
                    {
                        held.domain = Some(domain);
                    }
                },
                CounterpartyRecord::IdentityRecorded(identity) => {
                    if identities_seen.insert(identity.identity_id.clone()) {
                        register.identities.push(identity);
                    }
                },
                CounterpartyRecord::IdentityObserved { identity_id, at } => {
                    if let Some(held) = register
                        .identities
                        .iter_mut()
                        .find(|held| held.identity_id == identity_id)
                    {
                        // Forward only, and provenance and verification are not
                        // touched: an observation is not a promotion.
                        if at > held.last_seen {
                            held.last_seen = at;
                        }
                    }
                },
                CounterpartyRecord::IdentityPromoted {
                    identity_id,
                    channel,
                    evidence_ref,
                    decided_by,
                    at,
                } => {
                    if let Some(held) = register
                        .identities
                        .iter_mut()
                        .find(|held| held.identity_id == identity_id)
                    {
                        // The first promotion is the promotion.
                        if !held.verification.is_verified() {
                            held.verification = Verification::Verified {
                                channel,
                                evidence_ref,
                                decided_by,
                                at,
                            };
                        }
                    }
                },
                CounterpartyRecord::Merged {
                    from_counterparty_id,
                    into_counterparty_id,
                    ..
                } => {
                    if let Some(held) = register
                        .counterparties
                        .iter_mut()
                        .find(|held| held.counterparty_id == from_counterparty_id)
                    {
                        // Terminal: a merged record never resurrects and never
                        // merges again.
                        if held.merged_into.is_none() {
                            held.merged_into = Some(into_counterparty_id);
                        }
                    }
                },
            }
        }
        Ok(register)
    }

    /// The record an id names, refusing an unknown one and a merged one alike.
    fn require_live(&self, register: &Register, counterparty_id: &str) -> Result<Counterparty> {
        let Some(held) = register.counterparty(counterparty_id) else {
            anyhow::bail!(
                "no counterparty `{counterparty_id}` in this register. An organisation is \
                 recorded before anything is filed under it, so an unknown id is a caller error \
                 rather than a row to create on the way past"
            );
        };
        if let Some(head) = &held.merged_into {
            anyhow::bail!(
                "counterparty `{counterparty_id}` was merged into `{head}`; act on `{head}`, \
                 because a merged record never resurrects"
            );
        }
        Ok(held.clone())
    }

    /// The head an id resolves to, refusing an unknown id.
    fn require_head(&self, register: &Register, counterparty_id: &str) -> Result<String> {
        if register.counterparty(counterparty_id).is_none() {
            anyhow::bail!(
                "no counterparty `{counterparty_id}` in this register; an unknown organisation \
                 is not an organisation with nothing on file"
            );
        }
        register.live_head(counterparty_id)
    }

    fn append(&self, scope: &CounterpartyScope, record: &CounterpartyRecord) -> Result<()> {
        let path = self.register_path(scope);
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending {}", path.display()))?;
        Ok(())
    }
}

/// Every address whose organisation resolves to `head`, ordered stably.
fn gather_identities(register: &Register, head: &str) -> Result<Vec<Identity>> {
    let mut out = Vec::new();
    for identity in &register.identities {
        if register.live_head(&identity.counterparty_id)? == head {
            out.push(identity.clone());
        }
    }
    out.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.normalised.cmp(&right.normalised))
            .then_with(|| left.identity_id.cmp(&right.identity_id))
    });
    Ok(out)
}

/// Counts for one head record.
///
/// `merged_in_count` is the number of *other* records that resolve to this one —
/// how many names an owner has already decided were the same company.
fn summarise(register: &Register, head: &str) -> Result<CounterpartySummary> {
    let Some(record) = register.counterparty(head) else {
        anyhow::bail!("no counterparty `{head}` to summarise");
    };
    let identities = gather_identities(register, head)?;
    let mut merged_in_count = 0usize;
    for counterparty in &register.counterparties {
        if counterparty.counterparty_id != head
            && register.live_head(&counterparty.counterparty_id)? == head
        {
            merged_in_count += 1;
        }
    }
    Ok(CounterpartySummary {
        counterparty_id: record.counterparty_id.clone(),
        display_name: record.display_name.clone(),
        stage: record.stage.clone(),
        identity_count: identities.len(),
        verified_identity_count: identities.iter().filter(|held| held.is_verified()).count(),
        inferred_identity_count: identities.iter().filter(|held| held.is_inferred()).count(),
        merged_in_count,
    })
}

fn note_head(head: String, heads: &mut Vec<String>, seen: &mut BTreeSet<String>) {
    if seen.insert(head.clone()) {
        heads.push(head);
    }
}

/// A caller string that feeds a derived id.
///
/// Refused if blank, if it carries [`FIELD_SEP`], or if it carries any other
/// control character. The separator is the one that matters: a component
/// carrying it can fuse two components into one, and here "two components" means
/// two organisations.
fn guard_component(label: &str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{label} must not be blank; a blank component derives an id nobody meant");
    }
    if trimmed.contains(FIELD_SEP) {
        anyhow::bail!(
            "{label} must not contain U+001F: it is the separator that keeps a derived id's \
             components from bleeding into each other, and a value carrying it could fuse two \
             organisations into one id"
        );
    }
    if trimmed.chars().any(char::is_control) {
        anyhow::bail!("{label} must not contain control characters");
    }
    Ok(())
}

/// A caller string that must be there but derives nothing.
fn guard_present(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!(
            "{label} must not be blank; an unattributed record is one nobody can check afterwards"
        );
    }
    Ok(())
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// The comparison form of a display name.
///
/// Whitespace-collapsed and lower-cased, because the same organisation typed on
/// two different days rarely comes back character-identical, and two rows for
/// one company is how half its addresses become invisible to the other half.
fn display_name_key(display_name: &str) -> String {
    display_name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The register's id for an organisation.
///
/// Derived, never assigned — so a caller that recorded one and lost the response
/// can compute the handle again rather than creating a second row. Guaranteed
/// equal to what [`CounterpartyStore::record_counterparty`] derives for the same
/// scope and name: both delegate to the same private derivation, so they cannot
/// drift.
pub fn counterparty_id_for(scope: &CounterpartyScope, display_name: &str) -> Result<String> {
    scope.checked()?;
    guard_component("a counterparty display name", display_name)?;
    Ok(derive_counterparty_id(scope, display_name))
}

/// The register's id for an address.
///
/// Note what is **not** in it: the counterparty. An address belongs to at most
/// one organisation, so the id is the address — which is what makes a second
/// binding of the same address a collision the store can refuse rather than a
/// second row nobody notices.
pub fn identity_id_for(
    scope: &CounterpartyScope,
    kind: IdentityKind,
    value: &str,
) -> Result<String> {
    scope.checked()?;
    let normalised = normalise_identity(kind, value)?;
    Ok(derive_identity_id(scope, kind, &normalised))
}

fn derive_counterparty_id(scope: &CounterpartyScope, display_name: &str) -> String {
    format!(
        "cp-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}",
            scope.principal.trim(),
            scope.workspace.trim(),
            display_name_key(display_name)
        ))
    )
}

fn derive_identity_id(scope: &CounterpartyScope, kind: IdentityKind, normalised: &str) -> String {
    format!(
        "idy-{}",
        stable_id(&format!(
            "{}{FIELD_SEP}{}{FIELD_SEP}{}{FIELD_SEP}{normalised}",
            scope.principal.trim(),
            scope.workspace.trim(),
            kind.as_str()
        ))
    )
}
