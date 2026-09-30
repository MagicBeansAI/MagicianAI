//! **Using** the register: who just wrote to us, who we are about to write to,
//! and the seam a flow adopts a real identity through.
//!
//! Doc: `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §3.1–3.2.
//!
//! [`super::store`] answers three questions — *whose address is this*, *how do
//! we reach them*, *did anybody prove it* — and takes no view on what a caller
//! is allowed to do with the answer. This file is where traffic meets those
//! answers, in both directions, and it exists because the two directions have
//! opposite failure modes:
//!
//! - **Inbound.** A message arrives claiming an address. Resolving it is cheap
//!   and useful; treating the resolution as *authority* is the escalation the
//!   whole engagements plan is arranged to prevent, because SMTP does not
//!   authenticate `From:` and a lookalike sender resolving into a real
//!   organisation's context is a wrong answer shaped exactly like a right one.
//! - **Outbound.** We are about to write somewhere. Recording where is
//!   necessary — otherwise the reply comes back a stranger — but writing to an
//!   address proves nothing about it, so the record it produces must be
//!   unverified no matter how confident the sender was.
//!
//! # The one thing this module refuses to do
//!
//! **It never lets "we have this address on file" become "this message is from
//! them".** [`resolve_inbound`] returns [`InboundIdentification`], whose
//! authority-bearing answer is a *different variant* from its context-only one.
//! There is no `verified: bool` field beside a [`CounterpartyRef`] for a caller
//! to forget to read, and no accessor that yields the reference without the
//! caller having said which of the two it wanted:
//! [`InboundIdentification::authority`] answers `Some` for exactly one variant.
//! [`InboundIdentification::Unrecognised`] is the third answer and is not a
//! weaker form of either — it is *nobody*, and nobody is never permission.
//!
//! # Trust has a root the caller does not supply
//!
//! Whether a channel proved its sender is decided by
//! [`crate::magician_v2::chat::envoy::channel_is_verified`] and nowhere else,
//! exactly as [`super::types::TrustedSignal`] does it. A caller that passes
//! `caller_claim: Some(true)` on an email channel still gets a context-only
//! answer, because a claim the subject's own adapter supplies about the subject
//! is not a verification signal, and an unclassified channel fails closed.
//!
//! # Generic first
//!
//! Nothing here names a programme. "An inbound message on a channel", "the
//! first write to an address" and "a flow that carries an owner-typed
//! organisation label" exist wherever a system talks to somebody outside it.
//! Engagements ([`crate::magician_v2::engagements`]) are one consumer of the
//! third; scheduling, envelopes and obligations are consumers of the first two.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::magician_v2::audience::{Audience, AudienceKind, AudienceRef};
use crate::magician_v2::chat::envoy::channel_is_verified;

use crate::magician_v2::counterparty_store::{
    counterparty_id_for, identity_id_for, CounterpartyScope, CounterpartyStore,
};
use crate::magician_v2::counterparty_types::{
    normalise_identity, AddIdentity, CounterpartyRef, CreateCounterparty, Identity, IdentityKind,
    MintSource, FIELD_SEP,
};

// ── The address a channel carries ───────────────────────────────────────────

/// An address as it arrived on a channel, or as we are about to write to it.
///
/// The [`IdentityKind`] travels with the value rather than being inferred from
/// the channel name, because inferring it would be a per-channel table — and a
/// table that says "sms means phone" is one entry away from saying "this
/// platform's `@acme` is that platform's `@acme`", which is the authority
/// hand-off [`super::types::normalise_identity`] refuses by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAddress {
    pub kind: IdentityKind,
    pub value: String,
}

impl ChannelAddress {
    pub fn new(kind: IdentityKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

// ── What the boundary proved about an inbound message ───────────────────────

/// The two inputs that decide whether a channel established who sent a message.
///
/// Deliberately **not** a `verified: bool` the caller computes: a bool arrives
/// already-decided, and the one decision worth centralising is whether an
/// adapter may call its own traffic verified. Both fields are fed straight to
/// [`channel_is_verified`], which honours a de-escalating `caller_claim` and
/// ignores an escalating one.
///
/// This is not [`super::types::TrustedSignal`], and the difference is not an
/// oversight: `TrustedSignal` is the shape a **promotion** takes and carries the
/// evidence an owner will read before proving an address forever. Identifying an
/// inbound message promotes nothing, so demanding a promotion's evidence ref
/// here would make callers fabricate one or hold routing until they had one.
/// Both types delegate to the same function, so the two cannot drift.
///
/// [`Default`] is "nothing was proved", which is the fail-closed reading of an
/// adapter that has not been taught to fill this in yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InboundVerification {
    /// Whether **our** outer boundary proved the request. Not the sender's
    /// claim about itself.
    pub request_authenticated: bool,
    /// An adapter's own reading. `Some(false)` de-escalates and is honoured;
    /// `Some(true)` never raises.
    pub caller_claim: Option<bool>,
}

impl InboundVerification {
    /// Nothing was proved. The state every unclassified path should be in.
    pub fn unproven() -> Self {
        Self::default()
    }

    /// What the outer boundary said about this request.
    pub fn from_boundary(request_authenticated: bool) -> Self {
        Self {
            request_authenticated,
            caller_claim: None,
        }
    }

    pub fn with_caller_claim(mut self, caller_claim: Option<bool>) -> Self {
        self.caller_claim = caller_claim;
        self
    }
}

/// Why an inbound resolution is context-only rather than authority-bearing.
///
/// Two independent legs have to hold, and naming which one failed is what makes
/// the answer actionable: a missing channel proof is a fact about the transport
/// that no owner can fix, while an unproved address is one owner decision away
/// from being fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotAuthoritative {
    /// The channel's transport does not establish who sent this message —
    /// SMTP's `From:` is the canonical case. The address may well be theirs;
    /// this message is not evidence that it was them.
    ChannelDidNotProveSender,
    /// The address is on file but nobody has proved it reaches this
    /// organisation. An owner promotion (with a server-trusted signal) is the
    /// missing step.
    AddressNotVerified,
    /// Neither leg holds.
    NeitherProved,
}

impl NotAuthoritative {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChannelDidNotProveSender => "channel_did_not_prove_sender",
            Self::AddressNotVerified => "address_not_verified",
            Self::NeitherProved => "neither_proved",
        }
    }
}

/// Who an inbound message is from, and what that answer may be used for.
///
/// **The variant is the permission.** A caller holding one of these cannot
/// reach a [`CounterpartyRef`] without having chosen between
/// [`Self::authority`] (one variant) and [`Self::for_context`] (two), so
/// "unverified sender resolved into a real organisation's authority" is not a
/// mistake this type can be used to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundIdentification {
    /// The channel proved the sender **and** the address is one an owner has
    /// verified. The only answer anything that grants may act on.
    Authoritative {
        counterparty: CounterpartyRef,
        identity_id: String,
        /// The channel as the caller named it, for the audit line.
        channel: String,
        /// The comparison form the register matched on.
        normalised: String,
    },
    /// The address is on file, but at least one leg of the proof is missing.
    ///
    /// Legitimate to use as **context** — which thread this belongs on, what an
    /// owner should be shown, whether to raise a promotion — and never as
    /// authority. Reaching an engagement's private material on the strength of
    /// this is the escalation §3.2 forbids: research and affiliation *propose*,
    /// a deterministic fact or the owner *grants*.
    ContextOnly {
        counterparty: CounterpartyRef,
        identity_id: String,
        channel: String,
        normalised: String,
        why: NotAuthoritative,
    },
    /// The register does not hold this address. A stranger.
    ///
    /// Not a weaker [`Self::ContextOnly`]: there is no counterparty here at all,
    /// so there is nothing to carry forward and nothing to widen later. Callers
    /// route this as a guest and may fire research asynchronously; research
    /// never opens the door by itself.
    Unrecognised { channel: String, normalised: String },
}

impl InboundIdentification {
    /// The counterparty this message **is authoritatively from**.
    ///
    /// `Some` for exactly one variant. `None` is never permission — it is
    /// "nobody proved this", which covers both a stranger and a known address
    /// on an unproved channel.
    pub fn authority(&self) -> Option<&CounterpartyRef> {
        match self {
            Self::Authoritative { counterparty, .. } => Some(counterparty),
            Self::ContextOnly { .. } | Self::Unrecognised { .. } => None,
        }
    }

    /// The counterparty this message **appears** to be from.
    ///
    /// `Some` for both carrying variants, because an authoritative answer is
    /// also a context answer. Use for display, threading and for assembling an
    /// owner's approval — never for reaching anything the counterparty owns.
    pub fn for_context(&self) -> Option<&CounterpartyRef> {
        match self {
            Self::Authoritative { counterparty, .. } | Self::ContextOnly { counterparty, .. } => {
                Some(counterparty)
            },
            Self::Unrecognised { .. } => None,
        }
    }

    /// The address's id, when the register holds it.
    pub fn identity_id(&self) -> Option<&str> {
        match self {
            Self::Authoritative { identity_id, .. } | Self::ContextOnly { identity_id, .. } => {
                Some(identity_id)
            },
            Self::Unrecognised { .. } => None,
        }
    }

    /// Which leg of the proof is missing, or `None` when none is.
    pub fn why_not_authoritative(&self) -> Option<NotAuthoritative> {
        match self {
            Self::Authoritative { .. } | Self::Unrecognised { .. } => None,
            Self::ContextOnly { why, .. } => Some(*why),
        }
    }

    /// The channel as the caller named it.
    pub fn channel(&self) -> &str {
        match self {
            Self::Authoritative { channel, .. }
            | Self::ContextOnly { channel, .. }
            | Self::Unrecognised { channel, .. } => channel,
        }
    }
}

/// **Which counterparty is this inbound message from?**
///
/// Exact match on the normalised address via [`CounterpartyStore::resolve`],
/// then two independent proof legs decide which variant comes back:
///
/// | channel proved sender | address verified | answer |
/// |---|---|---|
/// | yes | yes | [`InboundIdentification::Authoritative`] |
/// | yes | no | `ContextOnly` — [`NotAuthoritative::AddressNotVerified`] |
/// | no | yes | `ContextOnly` — [`NotAuthoritative::ChannelDidNotProveSender`] |
/// | no | no | `ContextOnly` — [`NotAuthoritative::NeitherProved`] |
///
/// and an address the register does not hold is `Unrecognised`, whatever the
/// channel proved. **Both** legs are required for authority because the store
/// says so: `resolve` means "we have this address on file", and the store's own
/// guidance is that anything which grants reads
/// [`CounterpartyStore::resolve_verified`]. Requiring the channel too closes
/// the other half — a verified address does not make a spoofed message from it
/// authentic.
///
/// # This call writes. Exactly one line, and only ever `observe`
///
/// When the address is on file, `last_seen` is advanced through
/// [`CounterpartyStore::observe`] — the one write in the register that cannot
/// touch verification, provenance or `first_seen`. That is deliberate: hearing
/// from an address a hundred times is a hundred repetitions of the same
/// unauthenticated claim, and a path that let volume ripen into trust would
/// verify whoever was noisiest.
///
/// The observation happens whether or not the channel proved the sender,
/// because an unproved message did genuinely arrive claiming that address and a
/// silence sweep that could not see it would report a conversation as dead while
/// it was being spoofed at. The corollary belongs to the reader: anything that
/// must not be quieted by an unauthenticated claim reads the identity's
/// [`super::types::Verification`] alongside its `last_seen`, because `last_seen`
/// alone cannot tell the two apart.
///
/// A replayed inbound at the same `now` appends nothing (`observe` only moves
/// `last_seen` forward), so retrying is free.
///
/// An unrecognised address mints **nothing**. Minting on an observation would
/// produce an identity whose provenance is "something mentioned it".
///
/// # Failures are failures, never absences
///
/// An unreadable register propagates as an error rather than folding to
/// `Unrecognised`. "The log could not be read" and "this is a stranger" are
/// different answers, and the second is one a caller acts on.
pub fn resolve_inbound(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    channel: &str,
    address: &ChannelAddress,
    verified: InboundVerification,
    now: DateTime<Utc>,
) -> Result<InboundIdentification> {
    guard_channel(channel)?;
    // Both of these validate the scope and refuse a separator-carrying or
    // control-carrying address before anything is looked up.
    let identity_id = identity_id_for(scope, address.kind, &address.value)?;
    let normalised = normalise_identity(address.kind, &address.value)?;
    let channel = channel.trim().to_string();

    let channel_proved_sender = channel_is_verified(
        &channel,
        verified.request_authenticated,
        verified.caller_claim,
    );

    if store.load_identity(scope, &identity_id)?.is_none() {
        return Ok(InboundIdentification::Unrecognised {
            channel,
            normalised,
        });
    }

    // The only write on this path. Returns the row with `last_seen` advanced.
    let identity = store.observe(scope, &identity_id, now)?;

    let Some(counterparty) = store.resolve(scope, address.kind, &address.value)? else {
        anyhow::bail!(
            "identity `{identity_id}` is in the register but `resolve` does not answer for \
             `{normalised}`. The two read the same rows, so a disagreement means the log changed \
             underneath this call or is damaged; refusing rather than answering with whichever \
             read happened to win"
        );
    };

    let why = match (channel_proved_sender, identity.is_verified()) {
        (true, true) => None,
        (true, false) => Some(NotAuthoritative::AddressNotVerified),
        (false, true) => Some(NotAuthoritative::ChannelDidNotProveSender),
        (false, false) => Some(NotAuthoritative::NeitherProved),
    };

    Ok(match why {
        None => InboundIdentification::Authoritative {
            counterparty,
            identity_id,
            channel,
            normalised,
        },
        Some(why) => InboundIdentification::ContextOnly {
            counterparty,
            identity_id,
            channel,
            normalised,
            why,
        },
    })
}

// ── Outbound ────────────────────────────────────────────────────────────────

/// What a caller knows about an address it is about to write to.
///
/// The provenance is **derived, not chosen**: naming an introducer makes this
/// [`MintSource::Introduced`], omitting one makes it
/// [`MintSource::OwnerStated`]. Those are the only two an outbound send can
/// honestly produce — [`MintSource::ObservedOnInbound`] belongs to the other
/// direction, and [`MintSource::ResearchInferred`] is a **guess**, which the
/// send path must not launder into an owner statement (see
/// [`mint_from_outbound`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundEvidence {
    /// The organisation this address belongs to. Must already be recorded and
    /// still live — index before row.
    pub counterparty_id: String,
    /// What to read to check the claim: the draft, the approval, the thread.
    pub evidence_ref: String,
    /// Who or what is recording it.
    pub recorded_by: String,
    /// The identity that vouched, when somebody did. An introduction with no
    /// introducer is not an introduction, so a blank string is refused rather
    /// than downgraded to "owner stated".
    pub introduced_by: Option<String>,
}

/// Whether a mint wrote a new row or found one already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintOutcome {
    /// This address was not on file and now is.
    Recorded,
    /// The address was already on file. Nothing was written, and nothing about
    /// the existing row was changed.
    AlreadyOnFile,
}

/// The address as it stands on file after an outbound mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    pub identity: Identity,
    /// The channel this write is going out on, as the caller named it.
    pub channel: String,
    pub outcome: MintOutcome,
}

impl Minted {
    /// Whether this call is the one that put the address on file.
    pub fn was_recorded(&self) -> bool {
        self.outcome == MintOutcome::Recorded
    }
}

/// **Record where we are about to write.**
///
/// The first time anything writes to an address, that address becomes a
/// counterparty identity — otherwise the reply comes back a stranger and the
/// loop breaks exactly where it matters (§3.1: you apply through a web form and
/// they answer from something you have never seen).
///
/// # Writing to someone does not verify them
///
/// The recorded identity is [`super::types::Verification::Unverified`], always.
/// Confidence at send time is not proof of receipt, and proof of receipt is not
/// proof of ownership. Verification stays where the store put it: one owner
/// decision, rooted in a signal the caller does not control.
///
/// # Idempotent, and it never relabels
///
/// A second send to the same address returns [`MintOutcome::AlreadyOnFile`]
/// with the row that was already there, untouched. That matters beyond
/// tidiness: the address may already be on file as
/// [`MintSource::ResearchInferred`], and re-minting it as owner-stated would
/// erase the fact that it was a guess — along with the two guards the store
/// puts on promoting a guess (a research note may not be its own proof, and the
/// researcher may not approve its own output). So a researched address is filed
/// through [`CounterpartyStore::add_identity`] with its real provenance
/// *before* it is written to, and this call then resumes on it rather than
/// contradicting it.
///
/// # What it refuses
///
/// - An address already filed under a **different** organisation. One address
///   belongs to one organisation; two rows would make resolution ambiguous, and
///   an ambiguous resolution hands one counterparty's authority to another. A
///   caller naming an id that has since been merged into the organisation the
///   address is under resumes normally — that is the same organisation, spelled
///   with an older name.
/// - An unknown or blank counterparty, an unnamed recorder, a blank evidence
///   ref, a blank introducer, and a channel nobody named.
/// - A [`IdentityKind::Domain`] address. A domain is an affiliation hint, not
///   somewhere a message goes; recording one *as an outbound write* would claim
///   a send that never happened. Domains are filed with
///   [`CounterpartyStore::add_identity`] directly.
pub fn mint_from_outbound(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    channel: &str,
    address: &ChannelAddress,
    evidence: &OutboundEvidence,
    now: DateTime<Utc>,
) -> Result<Minted> {
    guard_channel(channel)?;
    if address.kind == IdentityKind::Domain {
        anyhow::bail!(
            "a domain is not somewhere a message goes, so it cannot be minted from an outbound \
             write; recording one here would assert a send that never happened. File it with \
             `add_identity` and the provenance it actually has"
        );
    }
    let channel = channel.trim().to_string();
    let identity_id = identity_id_for(scope, address.kind, &address.value)?;

    if let Some(existing) = store.load_identity(scope, &identity_id)? {
        // `summary` refuses an unknown id and follows merge edges, so both
        // sides are compared as the organisation that survived.
        let existing_head = store
            .summary(scope, &existing.counterparty_id)?
            .counterparty_id;
        let requested_head = store
            .summary(scope, &evidence.counterparty_id)?
            .counterparty_id;
        if existing_head != requested_head {
            anyhow::bail!(
                "`{}` is already filed under counterparty `{existing_head}`, so a write to it \
                 cannot be recorded against `{requested_head}`. One address belongs to one \
                 organisation: a second row would make resolution ambiguous, and an ambiguous \
                 resolution hands one counterparty's authority to another. If these are one \
                 organisation, `merge` them",
                existing.normalised
            );
        }
        return Ok(Minted {
            identity: existing,
            channel,
            outcome: MintOutcome::AlreadyOnFile,
        });
    }

    let (source, introduced_by) = match evidence.introduced_by.as_deref() {
        Some(introducer) if introducer.trim().is_empty() => anyhow::bail!(
            "an introduction whose introducer is blank is not an introduction; omit \
             `introduced_by` if nobody vouched, rather than recording a vouch with nothing \
             behind it"
        ),
        Some(introducer) => (MintSource::Introduced, Some(introducer.trim().to_string())),
        None => (MintSource::OwnerStated, None),
    };

    let identity = store.add_identity(
        scope,
        &AddIdentity {
            counterparty_id: evidence.counterparty_id.clone(),
            kind: address.kind,
            value: address.value.clone(),
            source,
            evidence_ref: evidence.evidence_ref.clone(),
            recorded_by: evidence.recorded_by.clone(),
            introduced_by,
        },
        now,
    )?;

    // A tripwire, not a formality: if a future change to `add_identity` ever
    // let provenance imply proof, this path — the one where the only evidence
    // is that we chose to send something — is the first place it would land.
    if identity.is_verified() {
        anyhow::bail!(
            "identity `{}` came back verified from an outbound mint. Writing to an address is \
             not proof of who holds it, and no automatic path may produce a verified identity",
            identity.identity_id
        );
    }

    Ok(Minted {
        identity,
        channel,
        outcome: MintOutcome::Recorded,
    })
}

// ── The engagement seam ─────────────────────────────────────────────────────

/// **The counterparty an engagement's label names, if the register holds one.**
///
/// # What this seam is for
///
/// [`crate::magician_v2::engagements::EngagementAuthority::counterparty`] is a
/// plain owner-readable `String` — "identity machinery is a later phase; the
/// carrier needs only something reviewable". These two functions are that later
/// phase's landing site, built so it needs no migration: an engagement keeps
/// its label, and a caller that wants the real organisation asks for it here.
/// Nothing in `engagements.rs` changes, and nothing in `engagements.rs` has to
/// change for this to start being useful.
///
/// # Today the label is authoritative and the reference is advisory
///
/// Stated plainly because the reverse is the tempting mistake. The engagement
/// record's own string is what its authority is scoped by and what an owner
/// reviews; a `CounterpartyRef` from here is a **lookup result**, and a lookup
/// result that came back `None` means the register has never heard the name — it
/// does not mean the engagement is unscoped, invalid, or open. Nothing may widen
/// an engagement's ceiling, team or expiry on the strength of this reference,
/// and nothing may narrow it either: those are `engagements.rs`'s to decide.
///
/// When engagements do adopt identities, the change is that the *reference*
/// becomes authoritative and the label becomes the display of it. Until then a
/// disagreement between the two is a fact for a human, not a conflict for a
/// resolver.
///
/// # It never guesses
///
/// The label is matched through [`counterparty_id_for`], which is the same
/// derivation [`CounterpartyStore::record_counterparty`] uses — so the store's
/// own fold (whitespace collapsed, case lowered) applies and nothing else does.
/// A label that is close to a recorded name, shares its domain, or differs by a
/// suffix resolves to `None`. There is no [`CounterpartyStore::candidates_by_domain`]
/// fallback here on purpose: candidates are a list to show a human, and a
/// function that returned one of them would turn owner review into a coin flip.
///
/// Merge edges are followed, so an engagement labelled with a name that has
/// since been folded into another organisation points at the record that
/// survived. A blank or separator-carrying label is refused rather than matched.
///
/// # A projection, kept for its callers
///
/// A projection of [`resolve_label`], which is the generic form and the one new
/// code should call — it answers with a [`CounterpartyLead`] where this answers
/// `None`, and a lead is a thing an owner can act on. Both read the same
/// derivation, so the two cannot disagree about what a label names.
pub fn counterparty_for_engagement(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    engagement_counterparty_label: &str,
) -> Result<Option<CounterpartyRef>> {
    Ok(resolve_label(store, scope, engagement_counterparty_label)?
        .registered()
        .cloned())
}

/// **The label an engagement would carry for this counterparty.**
///
/// The other direction of the same seam, and the reason it exists is round
/// tripping: a flow holding a [`CounterpartyRef`] can produce the string an
/// engagement record wants without inventing a formatting rule of its own, and
/// feeding that string back to [`counterparty_for_engagement`] returns the same
/// reference.
///
/// Merge edges are followed, so the label is the **surviving** organisation's
/// display name. A reference to a record that has been folded away therefore
/// answers with the name an owner would recognise today, and that label derives
/// back to the surviving record rather than to the dead one.
///
/// `None` means the register does not hold that id — a stale reference, or one
/// from another scope. It is not a name to fall back on and not a blank to fill
/// in.
pub fn engagement_label_for(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    counterparty: &CounterpartyRef,
) -> Result<Option<String>> {
    if store.load(scope, counterparty.as_str())?.is_none() {
        return Ok(None);
    }
    Ok(Some(
        store.summary(scope, counterparty.as_str())?.display_name,
    ))
}

// ── A work label, and the addresses it may cover ────────────────────────────

/// **A label nobody has recorded an organisation for.**
///
/// Not an error and not a refusal. A flow that carries an owner-typed
/// organisation label — an engagement, a support case, a vendor file, a
/// candidate's employer — is allowed to name somebody the register has never
/// heard of, because the label is how the owner describes the work and the
/// register is how we reach people, and those two are populated by different
/// acts at different times.
///
/// # What this is *for*
///
/// The follow-through, made exact. `would_be_counterparty_id` is what
/// [`counterparty_id_for`] derives for this label in this scope, which is the
/// id [`CounterpartyStore::record_counterparty`] will file it under — so an
/// owner-facing surface can offer *"record this organisation"* and land on the
/// row the read was already looking for, rather than on a second row spelled
/// slightly differently.
///
/// # What it is not
///
/// It is **not** a counterparty, and nothing may treat it as one. It carries no
/// identities, it resolves no address, and
/// [`verified_identities_for_label`] answers with an empty set while it is in
/// this state. See that function for why the empty set is the safe answer and
/// what a caller must not do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterpartyLead {
    /// The label as the flow carries it, trimmed. Kept verbatim so an owner
    /// reviewing the lead reads what they typed.
    pub label: String,
    /// The id the register would file this label under, derived exactly as
    /// [`CounterpartyStore::record_counterparty`] derives it.
    pub would_be_counterparty_id: String,
}

impl CounterpartyLead {
    /// The create request that turns this lead into a recorded organisation.
    ///
    /// **A convenience, not a write.** Nothing here appends anything: recording
    /// is a decision with a named decider, and `created_by` is the argument
    /// that makes somebody own it. That is why the register is never grown from
    /// a label automatically — a typo would become a permanent organisation
    /// whose provenance is *"it appeared in a form"*, and a second typo would
    /// become a second one that an owner is later asked to merge.
    ///
    /// The domain and stage are deliberately left unset: a label is a name, and
    /// inferring a domain from a name is the guess
    /// [`super::types::normalise_identity`] and
    /// [`CounterpartyStore::resolve`] both refuse.
    pub fn into_create(self, created_by: impl Into<String>) -> CreateCounterparty {
        CreateCounterparty {
            display_name: self.label,
            domain: None,
            stage: None,
            created_by: created_by.into(),
        }
    }
}

/// **What a work label resolves to in the register.**
///
/// Two arms, no `Option`, and that is the point: `None` beside a
/// [`CounterpartyRef`] reads as *"nothing here"* and gets handled with a
/// `unwrap_or_default()`, while [`Self::Unregistered`] carries a
/// [`CounterpartyLead`] that a caller has to look at to get past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelStanding {
    /// The register holds this organisation. The reference is the record that
    /// **survived every merge**, so two labels an owner has already folded into
    /// one answer identically.
    Registered(CounterpartyRef),
    /// Nothing in the register carries this label — a lead. Never permission,
    /// and never an error either.
    Unregistered(CounterpartyLead),
}

impl LabelStanding {
    /// The organisation, when there is one. `None` for a lead.
    pub fn registered(&self) -> Option<&CounterpartyRef> {
        match self {
            Self::Registered(counterparty) => Some(counterparty),
            Self::Unregistered(_) => None,
        }
    }

    /// The lead, when the label named nobody.
    pub fn lead(&self) -> Option<&CounterpartyLead> {
        match self {
            Self::Registered(_) => None,
            Self::Unregistered(lead) => Some(lead),
        }
    }

    /// A stable label for logs and owner surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Registered(_) => "registered",
            Self::Unregistered(_) => "unregistered",
        }
    }
}

/// **Which organisation does this work label name?**
///
/// The generic form of [`counterparty_for_engagement`], and the one new code
/// should call: an engagement is one flow that carries an owner-typed
/// organisation label, and support triage, vendor management and recruiting
/// carry the same thing under a different word. Nothing in this function, or in
/// the two built on it, names a programme.
///
/// # It never guesses
///
/// The label is matched through [`counterparty_id_for`] — the same derivation
/// [`CounterpartyStore::record_counterparty`] uses — so the register's own name
/// fold (whitespace collapsed, case lowered) applies and nothing else does. A
/// label that is close to a recorded name, shares its domain, or differs by a
/// legal suffix is [`LabelStanding::Unregistered`], not the nearest record.
/// There is no [`CounterpartyStore::candidates_by_domain`] fallback: candidates
/// are a list to show a human, and returning one of them would turn owner
/// review into a coin flip.
///
/// A blank, separator-carrying or control-carrying label is an **error** rather
/// than a lead: it derives an id nobody meant, so it cannot be recorded either.
pub fn resolve_label(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    label: &str,
) -> Result<LabelStanding> {
    let would_be_counterparty_id = counterparty_id_for(scope, label)?;
    if store.load(scope, &would_be_counterparty_id)?.is_none() {
        return Ok(LabelStanding::Unregistered(CounterpartyLead {
            label: label.trim().to_string(),
            would_be_counterparty_id,
        }));
    }
    // `summary` follows merge edges, so a label folded into another
    // organisation answers with the record that survived.
    Ok(LabelStanding::Registered(CounterpartyRef::new(
        store
            .summary(scope, &would_be_counterparty_id)?
            .counterparty_id,
    )))
}

/// **Which addresses may an act scoped to this label reach?**
///
/// The question an envelope, a room roster or an outbound boundary is really
/// asking when it holds a work label. The answer is the **verified** addresses
/// of the organisation that label names, in the register's own comparison form
/// (`normalised`), ordered stably by `(kind, normalised)`.
///
/// # Unverified addresses are not in it
///
/// An address on file is an address on file; nobody proved it reaches whom we
/// think. An envelope that covered an unverified address would let one
/// autonomous act reach somewhere the owner never confirmed, which is the
/// escalation the engagements plan is arranged to prevent. So the filter is
/// [`Identity::is_verified`] and the failure direction is "treated as a
/// stranger", which is recoverable.
///
/// # An empty answer is never permission
///
/// **Three different facts produce an empty vector**, and all three mean the
/// same thing to a caller: nothing here may be reached under this label.
///
/// - the label names nobody ([`LabelStanding::Unregistered`] — the fail-closed
///   reading of a lead);
/// - the organisation is recorded and has no addresses at all;
/// - the organisation is recorded and none of its addresses is proved.
///
/// A caller must therefore never write `if identities.is_empty() { allow }`, and
/// must never feed the vector to a predicate that passes vacuously over an
/// empty collection. A membership check over this list answers `false` for
/// everybody when it is empty, and that is the correct answer.
///
/// # These are comparison forms, not display values
///
/// Compare against [`super::types::normalise_identity`] of the address you
/// hold, never against a raw header. Comparing a raw value fails to match a
/// normalised one, which is again the safe direction, but it is a bug rather
/// than a policy.
pub fn verified_identities_for_label(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    label: &str,
) -> Result<Vec<String>> {
    let LabelStanding::Registered(counterparty) = resolve_label(store, scope, label)? else {
        return Ok(Vec::new());
    };
    Ok(store
        .verified_identities_for(scope, counterparty.as_str())?
        .into_iter()
        .map(|identity| identity.normalised)
        .collect())
}

/// [`verified_identities_for_label`], shaped as an [`Audience`].
///
/// The same answer for a caller that wants the carrier rather than the list, so
/// nobody rebuilds one from the other and gets the membership rule subtly
/// different. The identities are exactly what that function returns.
///
/// # The kind is the caller's, and every kind is available
///
/// `kind` is a plain [`AudienceKind`] argument, not a constant: the same
/// organisation is an [`AudienceKind::Engagement`] to a deal, an
/// [`AudienceKind::Account`] to support, an [`AudienceKind::Panel`] to a review
/// board and an [`AudienceKind::Person`] to somebody's own records.
/// [`AudienceRef::as_key`] keeps those apart, so a second flow adopts this path
/// by passing a different kind — not by editing anything here or downstream.
///
/// The kind changes the **key**, never the membership: the same label yields
/// the same identities under every kind, because who may be reached is a fact
/// about the register and not about who is asking.
///
/// # A lead yields an audience that admits nobody
///
/// For [`LabelStanding::Unregistered`] the audience is built over the id the
/// label *would* be filed under, with **no members**. [`Audience::admits`] then
/// answers `false` for everyone, which is the whole fail-closed reading: an
/// unresolvable label must produce an empty set, never a permissive one. The
/// reference still names the would-be id rather than the raw label so the key
/// does not change shape when an owner records the lead.
///
/// No expiry is set, matching [`CounterpartyStore::audience_for`]: a
/// counterparty relationship has no scheduled end of its own, and whoever owns
/// the relationship — an engagement with its mandatory expiry, for one — adds
/// theirs.
pub fn audience_for_label(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    label: &str,
    kind: AudienceKind,
) -> Result<Audience> {
    match resolve_label(store, scope, label)? {
        LabelStanding::Registered(counterparty) => {
            store.audience_for(scope, counterparty.as_str(), kind)
        },
        LabelStanding::Unregistered(lead) => Ok(Audience::new(
            AudienceRef::new(kind, lead.would_be_counterparty_id),
            Vec::new(),
        )),
    }
}

/// A channel name that will be recorded against an act.
///
/// Refused if blank, separator-carrying, or control-carrying. The blank case is
/// the one that matters: [`channel_is_verified`] answers `false` for an
/// unclassified channel, so a blank one would fail closed correctly and then sit
/// in an audit line saying nothing about what actually happened.
fn guard_channel(channel: &str) -> Result<()> {
    let trimmed = channel.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "a channel must be named: whether a transport establishes who sent a message is the \
             whole of this decision, and an unnamed channel records an act nobody can check \
             afterwards"
        );
    }
    if trimmed.contains(FIELD_SEP) {
        anyhow::bail!(
            "a channel must not contain U+001F: it is the separator that keeps a derived id's \
             components from bleeding into each other"
        );
    }
    if trimmed.chars().any(char::is_control) {
        anyhow::bail!("a channel must not contain control characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::ChannelAddress;
    use crate::magician_v2::counterparty_types::{IdentityKind, MintSource};
    use chrono::{Duration, TimeZone};

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    use crate::magician_v2::counterparty_types::*;
    use crate::magician_v2::counterparty_types::{
        CreateCounterparty, MergeDecision, Promotion, TrustedSignal, Verification,
    };

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn store() -> (tempfile::TempDir, CounterpartyStore, CounterpartyScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = CounterpartyStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store, CounterpartyScope::new("anonymous", "default"))
    }

    fn create(display_name: &str) -> CreateCounterparty {
        CreateCounterparty {
            display_name: display_name.to_string(),
            domain: None,
            stage: None,
            created_by: "owner".to_string(),
        }
    }

    fn address(counterparty_id: &str, kind: IdentityKind, value: &str) -> AddIdentity {
        AddIdentity {
            counterparty_id: counterparty_id.to_string(),
            kind,
            value: value.to_string(),
            source: MintSource::OwnerStated,
            evidence_ref: "msg-1".to_string(),
            recorded_by: "company-assistant".to_string(),
            introduced_by: None,
        }
    }

    /// A promotion a server would stand behind: an authenticated request on a
    /// channel whose transport establishes the sender.
    fn owner_promotion(evidence_ref: &str) -> Promotion {
        Promotion {
            decided_by: "owner".to_string(),
            evidence_ref: evidence_ref.to_string(),
            signal: TrustedSignal::new("web", evidence_ref).authenticated(true),
        }
    }

    fn outbound(counterparty_id: &str) -> OutboundEvidence {
        OutboundEvidence {
            counterparty_id: counterparty_id.to_string(),
            evidence_ref: "outbox-1".to_string(),
            recorded_by: "company-assistant".to_string(),
            introduced_by: None,
        }
    }

    /// A counterparty with one verified email on file.
    fn acme_with_verified_email() -> (
        tempfile::TempDir,
        CounterpartyStore,
        CounterpartyScope,
        String,
    ) {
        let (tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let identity = store
            .add_identity(
                &scope,
                &address(&acme.counterparty_id, IdentityKind::Email, "Ops@Acme.com"),
                now(),
            )
            .expect("add");
        store
            .promote_identity(
                &scope,
                &identity.identity_id,
                &owner_promotion("receipt-1"),
                now(),
            )
            .expect("promote");
        (tmp, store, scope, acme.counterparty_id)
    }

    // ── Inbound: the escalation this module exists to prevent ───────────────

    /// **An unverified inbound must never be authority-bearing.** The address is
    /// on file AND an owner has verified it, so the only thing missing is proof
    /// that *this message* came from them — and SMTP's `From:` is not that
    /// proof. If `authority()` answered here, a lookalike sender would reach a
    /// real organisation's engagement context and the caller could not tell,
    /// because a wrong answer is shaped exactly like a right one.
    #[test]
    fn an_unverified_inbound_never_yields_an_authority_bearing_resolution() {
        let (_tmp, store, scope, acme_id) = acme_with_verified_email();
        let from = ChannelAddress::new(IdentityKind::Email, "ops@acme.com");

        // Our own boundary proved the request; email still cannot prove the sender.
        let identified = resolve_inbound(
            &store,
            &scope,
            "email",
            &from,
            InboundVerification::from_boundary(true),
            now(),
        )
        .expect("identify");

        assert_eq!(identified.authority(), None);
        assert_eq!(
            identified.for_context(),
            Some(&CounterpartyRef::new(acme_id.clone()))
        );
        assert_eq!(
            identified.why_not_authoritative(),
            Some(NotAuthoritative::ChannelDidNotProveSender)
        );

        // A trusted transport with an unproved request is equally not proof.
        let unproved_request = resolve_inbound(
            &store,
            &scope,
            "whatsapp",
            &from,
            InboundVerification::from_boundary(false),
            now(),
        )
        .expect("identify");
        assert_eq!(unproved_request.authority(), None);
        assert_eq!(
            unproved_request.why_not_authoritative(),
            Some(NotAuthoritative::ChannelDidNotProveSender)
        );

        // An adapter nobody has taught to fill this in proves nothing.
        let unproven = resolve_inbound(
            &store,
            &scope,
            "whatsapp",
            &from,
            InboundVerification::unproven(),
            now(),
        )
        .expect("identify");
        assert_eq!(unproven.authority(), None);
        assert_eq!(
            unproven.why_not_authoritative(),
            Some(NotAuthoritative::ChannelDidNotProveSender)
        );

        // And an adapter claiming its own traffic is verified may not raise it.
        let self_claimed = resolve_inbound(
            &store,
            &scope,
            "email",
            &from,
            InboundVerification::from_boundary(true).with_caller_claim(Some(true)),
            now(),
        )
        .expect("identify");
        assert_eq!(self_claimed.authority(), None);
        assert_eq!(
            self_claimed.why_not_authoritative(),
            Some(NotAuthoritative::ChannelDidNotProveSender)
        );
    }

    /// **Both legs are required, and the answer says which one is missing.** A
    /// proved channel over an address nobody promoted is context, not authority:
    /// the transport proved a sender holds that address, not that the address is
    /// the organisation we think it is. Reporting the leg is what turns the
    /// answer into an owner action rather than a dead end.
    #[test]
    fn a_proved_channel_over_an_unproved_address_is_context_only() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        store
            .add_identity(
                &scope,
                &address(
                    &acme.counterparty_id,
                    IdentityKind::Phone,
                    "+1 555 123 4567",
                ),
                now(),
            )
            .expect("add");

        let identified = resolve_inbound(
            &store,
            &scope,
            "whatsapp",
            &ChannelAddress::new(IdentityKind::Phone, "+15551234567"),
            InboundVerification::from_boundary(true),
            now(),
        )
        .expect("identify");

        assert_eq!(identified.authority(), None);
        assert_eq!(
            identified.why_not_authoritative(),
            Some(NotAuthoritative::AddressNotVerified)
        );

        // Neither leg holding is reported as neither, not as one of them.
        let neither = resolve_inbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Phone, "+15551234567"),
            InboundVerification::from_boundary(true),
            now(),
        )
        .expect("identify");
        assert_eq!(
            neither.why_not_authoritative(),
            Some(NotAuthoritative::NeitherProved)
        );
    }

    /// The positive case, so the two legs are not vacuously unsatisfiable: a
    /// platform-authenticated sender over an owner-verified address is the one
    /// combination that grants, and it names the surviving organisation.
    #[test]
    fn a_proved_channel_over_a_proved_address_is_authoritative() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let identity = store
            .add_identity(
                &scope,
                &address(&acme.counterparty_id, IdentityKind::Phone, "+15551234567"),
                now(),
            )
            .expect("add");
        store
            .promote_identity(
                &scope,
                &identity.identity_id,
                &owner_promotion("callback-9"),
                now(),
            )
            .expect("promote");

        let identified = resolve_inbound(
            &store,
            &scope,
            "whatsapp",
            &ChannelAddress::new(IdentityKind::Phone, "+1 (555) 123-4567"),
            InboundVerification::from_boundary(true),
            now() + Duration::hours(1),
        )
        .expect("identify");

        assert_eq!(
            identified.authority(),
            Some(&CounterpartyRef::new(acme.counterparty_id.clone()))
        );
        assert_eq!(identified.why_not_authoritative(), None);
        assert_eq!(
            identified.identity_id(),
            Some(identity.identity_id.as_str())
        );
        assert_eq!(identified.channel(), "whatsapp");
    }

    /// **A near miss must resolve to nobody, not to the nearest match.**
    /// Resolution is exact match on the normalised value. A one-character
    /// difference, a `+tag`, a `www.` prefix or a truncated TLD are all
    /// different addresses, and answering with the organisation they resemble
    /// would hand that organisation's context to a stranger.
    #[test]
    fn a_near_miss_inbound_address_resolves_to_nobody() {
        let (_tmp, store, scope, _acme_id) = acme_with_verified_email();

        for near_miss in [
            "op@acme.com",
            "ops@acme.co",
            "ops+deals@acme.com",
            "ops@www.acme.com",
            "0ps@acme.com",
            "ops@acme.com.co",
        ] {
            let identified = resolve_inbound(
                &store,
                &scope,
                "whatsapp",
                &ChannelAddress::new(IdentityKind::Email, near_miss),
                InboundVerification::from_boundary(true),
                now(),
            )
            .expect("identify");
            assert_eq!(
                identified,
                InboundIdentification::Unrecognised {
                    channel: "whatsapp".to_string(),
                    normalised: near_miss.to_lowercase(),
                },
                "`{near_miss}` must not resolve to the organisation it resembles"
            );
            assert_eq!(identified.authority(), None);
            assert_eq!(identified.for_context(), None);
            assert_eq!(identified.identity_id(), None);
        }

        // And the folds that ARE performed still land.
        let exact = resolve_inbound(
            &store,
            &scope,
            "whatsapp",
            &ChannelAddress::new(IdentityKind::Email, "  OPS@Acme.COM  "),
            InboundVerification::from_boundary(true),
            now(),
        )
        .expect("identify");
        assert_eq!(exact.why_not_authoritative(), None);
    }

    /// **An unrecognised inbound mints nothing.** An address whose provenance is
    /// "a message mentioned it" is a rumour with a category label, and a
    /// register that grew one row per stranger would make every later resolution
    /// answer for addresses nobody chose to record.
    #[test]
    fn an_unrecognised_inbound_writes_no_identity() {
        let (_tmp, store, scope, acme_id) = acme_with_verified_email();

        resolve_inbound(
            &store,
            &scope,
            "whatsapp",
            &ChannelAddress::new(IdentityKind::Email, "stranger@example.com"),
            InboundVerification::from_boundary(true),
            now() + Duration::hours(1),
        )
        .expect("identify");

        assert_eq!(store.list(&scope).expect("list").len(), 1);
        assert_eq!(
            store
                .identities_for(&scope, &acme_id)
                .expect("identities")
                .len(),
            1
        );
    }

    /// **Observing must not verify, and must not touch provenance.** This is the
    /// path an unauthenticated sender can drive at will: if arriving repeatedly
    /// could ripen an address into a verified one, the register would verify
    /// whoever was noisiest. `last_seen` moves; the verification decision, the
    /// provenance and `first_seen` are exactly what they were.
    #[test]
    fn an_inbound_observation_moves_last_seen_and_nothing_else() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let minted = store
            .add_identity(
                &scope,
                &address(&acme.counterparty_id, IdentityKind::Email, "ops@acme.com"),
                now(),
            )
            .expect("add");

        let later = now() + Duration::hours(3);
        let identified = resolve_inbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            InboundVerification::from_boundary(true),
            later,
        )
        .expect("identify");
        assert_eq!(identified.authority(), None);

        let after = store
            .load_identity(&scope, &minted.identity_id)
            .expect("load")
            .expect("present");
        assert_eq!(after.last_seen, later);
        assert_eq!(after.first_seen, now());
        assert_eq!(after.verification, Verification::Unverified);
        assert_eq!(after.minted_by.source, MintSource::OwnerStated);
        assert_eq!(after.minted_by.evidence_ref, "msg-1");
        assert_eq!(after.minted_by.recorded_by, "company-assistant");
    }

    /// The same guarantee from the other side: an address an owner already
    /// verified is not *de*-verified by an inbound on an unproved channel, and
    /// the recorded decision — channel, evidence, decider, timestamp — comes
    /// back byte for byte. Observing rewriting a promotion would erase the one
    /// decision an owner can audit.
    #[test]
    fn an_inbound_observation_does_not_rewrite_an_existing_promotion() {
        let (_tmp, store, scope, acme_id) = acme_with_verified_email();
        let identity_id =
            identity_id_for(&scope, IdentityKind::Email, "ops@acme.com").expect("derive");

        let later = now() + Duration::hours(2);
        resolve_inbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            InboundVerification::from_boundary(true),
            later,
        )
        .expect("identify");

        let after = store
            .load_identity(&scope, &identity_id)
            .expect("load")
            .expect("present");
        assert_eq!(
            after.verification,
            Verification::Verified {
                channel: "web".to_string(),
                evidence_ref: "receipt-1".to_string(),
                decided_by: "owner".to_string(),
                at: now(),
            }
        );
        assert_eq!(after.last_seen, later);
        assert_eq!(
            store
                .verified_identities_for(&scope, &acme_id)
                .expect("verified")
                .len(),
            1
        );
    }

    /// A channel nobody named is refused rather than silently failing closed
    /// into an audit line that says nothing, and one carrying the id separator
    /// is refused for the same reason every other component is.
    #[test]
    fn an_unnamed_or_separator_carrying_channel_is_refused() {
        let (_tmp, store, scope, _acme_id) = acme_with_verified_email();
        let from = ChannelAddress::new(IdentityKind::Email, "ops@acme.com");

        let blank = resolve_inbound(
            &store,
            &scope,
            "   ",
            &from,
            InboundVerification::from_boundary(true),
            now(),
        )
        .expect_err("blank channel");
        assert!(
            blank.to_string().contains("a channel must be named"),
            "{blank}"
        );

        let fused = resolve_inbound(
            &store,
            &scope,
            "email\u{1f}whatsapp",
            &from,
            InboundVerification::from_boundary(true),
            now(),
        )
        .expect_err("separator");
        assert!(fused.to_string().contains("U+001F"), "{fused}");
    }

    // ── Outbound ────────────────────────────────────────────────────────────

    /// **Minting twice yields one identity.** A retried send, or a second
    /// message to the same person, must not produce a second row: two rows for
    /// one address make resolution ambiguous, which is the failure the whole
    /// module is arranged around. Case and spacing differences are the same
    /// address, so they resume too.
    #[test]
    fn minting_the_same_address_twice_yields_one_identity() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");

        let first = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "Ops@Acme.com"),
            &outbound(&acme.counterparty_id),
            now(),
        )
        .expect("mint");
        assert_eq!(first.outcome, MintOutcome::Recorded);
        assert!(first.was_recorded());

        let second = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "  ops@ACME.com "),
            &outbound(&acme.counterparty_id),
            now() + Duration::hours(6),
        )
        .expect("replay");
        assert_eq!(second.outcome, MintOutcome::AlreadyOnFile);
        assert!(!second.was_recorded());
        assert_eq!(second.identity.identity_id, first.identity.identity_id);
        assert_eq!(second.identity.first_seen, now());
        assert_eq!(second.identity.last_seen, now());

        assert_eq!(
            store
                .identities_for(&scope, &acme.counterparty_id)
                .expect("identities")
                .len(),
            1
        );
    }

    /// **Writing to someone does not verify them.** Confidence at send time is
    /// not proof of receipt and proof of receipt is not proof of ownership, so
    /// the row an outbound mint produces is unverified and stays outside every
    /// audience until an owner decides otherwise.
    #[test]
    fn an_outbound_mint_records_an_unverified_identity() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");

        let minted = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            &outbound(&acme.counterparty_id),
            now(),
        )
        .expect("mint");

        assert_eq!(minted.identity.verification, Verification::Unverified);
        assert_eq!(minted.identity.minted_by.source, MintSource::OwnerStated);
        assert_eq!(minted.identity.minted_by.evidence_ref, "outbox-1");
        assert_eq!(minted.identity.introduced_by, None);
        assert_eq!(minted.channel, "email");
        assert!(store
            .verified_identities_for(&scope, &acme.counterparty_id)
            .expect("verified")
            .is_empty());
    }

    /// Provenance is derived from what the caller actually knows: naming an
    /// introducer records an introduction, and a blank one is refused rather
    /// than quietly downgraded to "the owner said so" — which would credit the
    /// owner with a statement nobody made.
    #[test]
    fn an_outbound_mint_with_an_introducer_records_an_introduction() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");

        let mut evidence = outbound(&acme.counterparty_id);
        evidence.introduced_by = Some("idy-partner".to_string());
        let minted = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            &evidence,
            now(),
        )
        .expect("mint");
        assert_eq!(minted.identity.minted_by.source, MintSource::Introduced);
        assert_eq!(
            minted.identity.introduced_by.as_deref(),
            Some("idy-partner")
        );

        let mut blank = outbound(&acme.counterparty_id);
        blank.introduced_by = Some("   ".to_string());
        let error = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "sales@acme.com"),
            &blank,
            now(),
        )
        .expect_err("blank introducer");
        assert!(
            error.to_string().contains("is not an introduction"),
            "{error}"
        );
    }

    /// An address already filed under a different organisation is refused, not
    /// resumed and not duplicated. Letting an outbound send re-file somebody
    /// else's address is precisely how one counterparty's replies would start
    /// resolving into another's context.
    #[test]
    fn an_outbound_mint_may_not_move_an_address_between_organisations() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let globex = store
            .record_counterparty(&scope, &create("Globex Inc"), now())
            .expect("record");
        mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            &outbound(&acme.counterparty_id),
            now(),
        )
        .expect("mint");

        let error = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            &outbound(&globex.counterparty_id),
            now(),
        )
        .expect_err("cross-filing");
        assert!(
            error
                .to_string()
                .contains("One address belongs to one organisation"),
            "{error}"
        );
        assert_eq!(
            store
                .identities_for(&scope, &globex.counterparty_id)
                .expect("identities")
                .len(),
            0
        );
    }

    /// An unknown organisation is an error, never a row created on the way past.
    /// Index before row: an identity whose organisation no read can name is an
    /// address that resolves to a phantom.
    #[test]
    fn an_outbound_mint_against_an_unknown_organisation_is_refused() {
        let (_tmp, store, scope) = store();
        let error = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            &outbound("cp-nobody"),
            now(),
        )
        .expect_err("unknown counterparty");
        assert!(error.to_string().contains("cp-nobody"), "{error}");
    }

    /// A domain is an affiliation hint, not somewhere a message goes. Minting
    /// one *from an outbound write* would record a send that never happened and
    /// then let a domain sit in the register with send-shaped provenance.
    #[test]
    fn an_outbound_mint_refuses_a_domain() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let error = mint_from_outbound(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Domain, "acme.com"),
            &outbound(&acme.counterparty_id),
            now(),
        )
        .expect_err("domain");
        assert!(
            error.to_string().contains("not somewhere a message goes"),
            "{error}"
        );
    }

    // ── The engagement seam ─────────────────────────────────────────────────

    /// **The bridge returns None rather than guessing.** An engagement's label
    /// is owner-typed prose; a near name, a shared domain or a different legal
    /// suffix is not the same organisation, and answering with the nearest
    /// record would let an engagement's authority land on a counterparty nobody
    /// chose. `None` here means "the register has never heard this name", which
    /// is never permission for anything.
    #[test]
    fn the_engagement_bridge_returns_none_rather_than_guessing() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(
                &scope,
                &CreateCounterparty {
                    display_name: "Acme Ltd".to_string(),
                    domain: Some("acme.com".to_string()),
                    stage: None,
                    created_by: "owner".to_string(),
                },
                now(),
            )
            .expect("record");

        for label in [
            "Acme Limited",
            "Acme",
            "Acme Ltd.",
            "acme.com",
            "Globex Inc",
        ] {
            assert_eq!(
                counterparty_for_engagement(&store, &scope, label).expect("bridge"),
                None,
                "`{label}` must not be guessed onto a recorded organisation"
            );
        }

        // The store's own name fold still applies: case and spacing are two
        // spellings of one name, not two organisations.
        assert_eq!(
            counterparty_for_engagement(&store, &scope, "  acme   LTD ").expect("bridge"),
            Some(CounterpartyRef::new(acme.counterparty_id.clone()))
        );

        // A blank label matches nobody rather than whoever came first.
        assert!(counterparty_for_engagement(&store, &scope, "  ").is_err());
    }

    /// The bridge follows merges in both directions, so an engagement still
    /// labelled with a name that has since been folded away points at the record
    /// that survived — and the label that comes back is the surviving name,
    /// which derives to that same record.
    #[test]
    fn the_engagement_bridge_follows_a_merge_to_the_surviving_record() {
        let (_tmp, store, scope) = store();
        let ltd = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let gmbh = store
            .record_counterparty(&scope, &create("Acme GmbH"), now())
            .expect("record");
        store
            .merge(
                &scope,
                &gmbh.counterparty_id,
                &ltd.counterparty_id,
                &MergeDecision {
                    decided_by: "owner".to_string(),
                    evidence_ref: "companies-house-filing".to_string(),
                },
                now(),
            )
            .expect("merge");

        assert_eq!(
            counterparty_for_engagement(&store, &scope, "Acme GmbH").expect("bridge"),
            Some(CounterpartyRef::new(ltd.counterparty_id.clone()))
        );
        assert_eq!(
            engagement_label_for(&store, &scope, &CounterpartyRef::new(gmbh.counterparty_id))
                .expect("label"),
            Some("Acme Ltd".to_string())
        );
    }

    /// A reference the register does not hold answers `None`, not a fabricated
    /// or blank label. A stale or cross-scope reference is a fact for a human;
    /// filling it in would put a name on an engagement nobody recorded.
    #[test]
    fn a_label_for_an_unknown_reference_is_none() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");

        assert_eq!(
            engagement_label_for(&store, &scope, &CounterpartyRef::new("cp-nobody"))
                .expect("label"),
            None
        );
        // Round trip: label out, reference back.
        let label = engagement_label_for(
            &store,
            &scope,
            &CounterpartyRef::new(acme.counterparty_id.clone()),
        )
        .expect("label")
        .expect("present");
        assert_eq!(label, "Acme Ltd");
        assert_eq!(
            counterparty_for_engagement(&store, &scope, &label).expect("bridge"),
            Some(CounterpartyRef::new(acme.counterparty_id))
        );
    }
    // ── A work label, and the addresses it may cover ────────────────────────

    /// **Only proved addresses are covered.** The organisation has two
    /// addresses on file and one of them is verified.
    ///
    /// Pins the read that filters on nothing: an envelope built from every
    /// address on file would cover one nobody proved reaches this organisation,
    /// which is exactly the escalation the engagements plan forbids. The
    /// unverified address is asserted to EXIST first, so a filter that silently
    /// dropped both cannot pass this.
    #[test]
    fn a_labels_audience_carries_only_the_addresses_somebody_proved() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let proved = store
            .add_identity(
                &scope,
                &address(&acme.counterparty_id, IdentityKind::Email, "Ops@Acme.com"),
                now(),
            )
            .expect("add");
        store
            .add_identity(
                &scope,
                &address(
                    &acme.counterparty_id,
                    IdentityKind::Email,
                    "billing@acme.com",
                ),
                now(),
            )
            .expect("add");
        store
            .promote_identity(
                &scope,
                &proved.identity_id,
                &owner_promotion("receipt-1"),
                now(),
            )
            .expect("promote");

        // Both are on file. A test that skipped this would pass against a
        // register that had lost the second address entirely.
        let on_file: Vec<String> = store
            .identities_for(&scope, &acme.counterparty_id)
            .expect("identities")
            .into_iter()
            .map(|identity| identity.normalised)
            .collect();
        assert_eq!(
            on_file,
            vec!["billing@acme.com".to_string(), "ops@acme.com".to_string()]
        );

        assert_eq!(
            verified_identities_for_label(&store, &scope, "Acme Ltd").expect("identities"),
            vec!["ops@acme.com".to_string()]
        );

        let audience = audience_for_label(&store, &scope, "Acme Ltd", AudienceKind::Engagement)
            .expect("audience");
        assert_eq!(audience.size(), 1);
        assert!(audience.admits("ops@acme.com", now()));
        assert!(
            !audience.admits("billing@acme.com", now()),
            "an address on file that nobody proved must not be admitted"
        );
    }

    /// **A label the register has never heard yields NO addresses, not every
    /// address.** The register is seeded and answering for a real label at the
    /// same moment.
    ///
    /// Pins the fail-open shape this whole seam exists to refuse: an
    /// unresolvable counterparty read as "unscoped", producing a covering set
    /// that admits whoever happens to be on file. Both halves are asserted in
    /// one test on purpose — an empty answer from an empty store proves
    /// nothing.
    #[test]
    fn an_unresolvable_label_yields_an_empty_set_not_a_permissive_one() {
        let (_tmp, store, scope, _acme_id) = acme_with_verified_email();

        // The register holds a real, proved address right now.
        assert_eq!(
            verified_identities_for_label(&store, &scope, "Acme Ltd").expect("identities"),
            vec!["ops@acme.com".to_string()]
        );

        // And a label nobody recorded covers none of it.
        assert_eq!(
            verified_identities_for_label(&store, &scope, "Globex Inc").expect("identities"),
            Vec::<String>::new()
        );
        let audience = audience_for_label(&store, &scope, "Globex Inc", AudienceKind::Engagement)
            .expect("audience");
        assert_eq!(audience.size(), 0);
        assert!(
            !audience.admits("ops@acme.com", now()),
            "an unresolvable label must not admit another organisation's proved address"
        );
        assert!(!audience.admits("anyone@example.com", now()));
    }

    /// **An unmatched label is a lead, and the lead names the row it would
    /// become.**
    ///
    /// Pins two failures. First, a label that matched nothing reported as an
    /// error or as `None` with nothing to act on — the owner is then told the
    /// work is broken instead of being offered the one decision that fixes it.
    /// Second, a follow-through that lands on a *different* row than the read
    /// was looking for: recording the lead must make the very same label
    /// resolve, and must not move the audience key.
    #[test]
    fn an_unmatched_label_is_a_lead_that_records_onto_the_row_the_read_wanted() {
        let (_tmp, store, scope, _acme_id) = acme_with_verified_email();

        let standing = resolve_label(&store, &scope, "  Globex   Inc ").expect("resolve");
        assert_eq!(standing.as_str(), "unregistered");
        assert_eq!(standing.registered(), None);
        let lead = standing.lead().expect("a lead").clone();
        assert_eq!(lead.label, "Globex   Inc");
        assert_eq!(
            lead.would_be_counterparty_id,
            counterparty_id_for(&scope, "Globex Inc").expect("derive"),
            "the lead must name the id the register derives for this name"
        );

        let key_before = audience_for_label(&store, &scope, "Globex Inc", AudienceKind::Engagement)
            .expect("audience")
            .reference
            .as_key();

        let recorded = store
            .record_counterparty(&scope, &lead.clone().into_create("owner"), now())
            .expect("record");
        assert_eq!(recorded.counterparty_id, lead.would_be_counterparty_id);
        assert_eq!(recorded.display_name, "Globex   Inc");
        assert_eq!(recorded.created_by, "owner");

        let after = resolve_label(&store, &scope, "Globex Inc").expect("resolve");
        assert_eq!(after.as_str(), "registered");
        assert_eq!(
            after.registered(),
            Some(&CounterpartyRef::new(lead.would_be_counterparty_id.clone()))
        );

        let key_after = audience_for_label(&store, &scope, "Globex Inc", AudienceKind::Engagement)
            .expect("audience")
            .reference
            .as_key();
        assert_eq!(
            key_before, key_after,
            "recording a lead must not move the audience key it was already answering under"
        );
        // Recorded is still not reachable: an organisation with no proved
        // address admits nobody.
        assert_eq!(
            verified_identities_for_label(&store, &scope, "Globex Inc").expect("identities"),
            Vec::<String>::new()
        );
    }

    /// **Reading a lead writes nothing.**
    ///
    /// Pins the convenience that would grow the register from a form field: if
    /// any of the three reads recorded the organisation on the way past, a typo
    /// in an engagement label would become a permanent row with no decider, and
    /// two typos would become two organisations somebody is later asked to
    /// merge.
    #[test]
    fn resolving_a_lead_never_records_it() {
        let (_tmp, store, scope, acme_id) = acme_with_verified_email();

        for label in ["Globex Inc", "Acme Limited", "acme.com"] {
            resolve_label(&store, &scope, label).expect("resolve");
            verified_identities_for_label(&store, &scope, label).expect("identities");
            audience_for_label(&store, &scope, label, AudienceKind::Account).expect("audience");
        }

        let recorded: Vec<String> = store
            .list(&scope)
            .expect("list")
            .into_iter()
            .map(|counterparty| counterparty.counterparty_id)
            .collect();
        assert_eq!(recorded, vec![acme_id]);
    }

    /// **The audience kind is the caller's, so a second flow adopts this path
    /// without editing it.**
    ///
    /// Pins a hardcoded `AudienceKind::Engagement`. Support triage, vendor
    /// management and recruiting read the same register through the same
    /// function; if the kind were fixed here, every one of them would file its
    /// roster under `engagement:<id>` and two different relationships with one
    /// organisation would silently share a key.
    #[test]
    fn the_audience_kind_is_the_callers_and_never_changes_who_is_admitted() {
        let (_tmp, store, scope, acme_id) = acme_with_verified_email();

        let mut keys = Vec::new();
        for kind in AudienceKind::ALL {
            let audience = audience_for_label(&store, &scope, "Acme Ltd", kind).expect("audience");
            assert_eq!(
                audience.identities,
                vec!["ops@acme.com".to_string()],
                "the kind must not change who may be reached"
            );
            assert!(audience.admits("ops@acme.com", now()));
            keys.push(audience.reference.as_key());
        }
        assert_eq!(
            keys,
            vec![
                format!("engagement:{acme_id}"),
                format!("program:{acme_id}"),
                format!("account:{acme_id}"),
                format!("panel:{acme_id}"),
                format!("person:{acme_id}"),
            ]
        );
    }

    /// **The list and the audience answer with the same addresses.**
    ///
    /// Pins the drift a caller would inherit by rebuilding one from the other:
    /// two shapes of one answer that disagree about membership is how an
    /// envelope comes to cover an address a room does not admit, and neither
    /// side can tell which is right.
    #[test]
    fn the_list_shape_and_the_audience_shape_carry_the_same_addresses() {
        let (_tmp, store, scope) = store();
        let acme = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        for value in ["ops@acme.com", "billing@acme.com", "+441234567890"] {
            let kind = if value.starts_with('+') {
                IdentityKind::Phone
            } else {
                IdentityKind::Email
            };
            let identity = store
                .add_identity(&scope, &address(&acme.counterparty_id, kind, value), now())
                .expect("add");
            store
                .promote_identity(
                    &scope,
                    &identity.identity_id,
                    &owner_promotion(&format!("receipt-{value}")),
                    now(),
                )
                .expect("promote");
        }

        let listed = verified_identities_for_label(&store, &scope, "Acme Ltd").expect("identities");
        assert_eq!(
            listed,
            vec![
                "billing@acme.com".to_string(),
                "ops@acme.com".to_string(),
                "+441234567890".to_string(),
            ],
            "ordered by (kind, normalised) so the answer reads the same on every call"
        );
        assert_eq!(
            audience_for_label(&store, &scope, "Acme Ltd", AudienceKind::Panel)
                .expect("audience")
                .identities,
            listed
        );
    }

    /// **A recorded organisation with nothing proved admits nobody, and that is
    /// not an error.**
    ///
    /// Pins the third route to an empty answer. `identities_for` on an unknown
    /// organisation is an error by design; this one is *known* and has an
    /// address, so a caller that treated "no error and no members" as "no
    /// restriction" would admit the very address nobody has proved.
    #[test]
    fn a_recorded_organisation_with_no_proved_address_admits_nobody() {
        let (_tmp, store, scope) = store();
        let globex = store
            .record_counterparty(&scope, &create("Globex Inc"), now())
            .expect("record");
        store
            .add_identity(
                &scope,
                &address(
                    &globex.counterparty_id,
                    IdentityKind::Email,
                    "hello@globex.com",
                ),
                now(),
            )
            .expect("add");

        // The address EXISTS on file.
        assert_eq!(
            store
                .identities_for(&scope, &globex.counterparty_id)
                .expect("identities")
                .len(),
            1
        );
        assert_eq!(
            resolve_label(&store, &scope, "Globex Inc")
                .expect("resolve")
                .as_str(),
            "registered"
        );

        assert_eq!(
            verified_identities_for_label(&store, &scope, "Globex Inc").expect("identities"),
            Vec::<String>::new()
        );
        let audience = audience_for_label(&store, &scope, "Globex Inc", AudienceKind::Engagement)
            .expect("audience");
        assert_eq!(audience.size(), 0);
        assert!(!audience.admits("hello@globex.com", now()));
    }

    /// **A label folded into another organisation covers the survivor's
    /// addresses, under the survivor's key.**
    ///
    /// Pins a read that stops at the dead row: an engagement still labelled
    /// with a name an owner has already merged away would cover nothing, and
    /// the work would look unreachable while its addresses sat one edge away.
    #[test]
    fn a_merged_away_label_covers_the_surviving_organisations_addresses() {
        let (_tmp, store, scope) = store();
        let ltd = store
            .record_counterparty(&scope, &create("Acme Ltd"), now())
            .expect("record");
        let gmbh = store
            .record_counterparty(&scope, &create("Acme GmbH"), now())
            .expect("record");
        let identity = store
            .add_identity(
                &scope,
                &address(&gmbh.counterparty_id, IdentityKind::Email, "ops@acme.de"),
                now(),
            )
            .expect("add");
        store
            .promote_identity(
                &scope,
                &identity.identity_id,
                &owner_promotion("receipt-de"),
                now(),
            )
            .expect("promote");

        // Before the merge the old name answers for itself.
        assert_eq!(
            verified_identities_for_label(&store, &scope, "Acme GmbH").expect("identities"),
            vec!["ops@acme.de".to_string()]
        );
        assert_eq!(
            verified_identities_for_label(&store, &scope, "Acme Ltd").expect("identities"),
            Vec::<String>::new()
        );

        store
            .merge(
                &scope,
                &gmbh.counterparty_id,
                &ltd.counterparty_id,
                &MergeDecision {
                    decided_by: "owner".to_string(),
                    evidence_ref: "companies-house-filing".to_string(),
                },
                now(),
            )
            .expect("merge");

        // After it, both names answer with the survivor's addresses, under the
        // survivor's key.
        for label in ["Acme GmbH", "Acme Ltd"] {
            assert_eq!(
                verified_identities_for_label(&store, &scope, label).expect("identities"),
                vec!["ops@acme.de".to_string()],
                "`{label}` must follow the merge edge"
            );
            let audience =
                audience_for_label(&store, &scope, label, AudienceKind::Account).expect("audience");
            assert_eq!(
                audience.reference.as_key(),
                format!("account:{}", ltd.counterparty_id)
            );
            assert!(audience.admits("ops@acme.de", now()));
        }
    }

    /// **A label that cannot derive an id is refused, not read as a lead.**
    ///
    /// Pins the separator: a label carrying U+001F can fuse two id components
    /// into one, which here means two organisations sharing a row. A blank
    /// label derives an id nobody meant. Neither may come back as an empty
    /// audience, because an empty audience is a *fact about a real label* and a
    /// caller may legitimately record it as one.
    #[test]
    fn a_label_that_cannot_derive_an_id_is_refused_rather_than_read_as_a_lead() {
        let (_tmp, store, scope, _acme_id) = acme_with_verified_email();

        for label in ["", "   ", "Acme\u{1f}Ltd", "Acme\u{1f}", "Acme\nLtd"] {
            assert!(
                resolve_label(&store, &scope, label).is_err(),
                "`{}` must be refused rather than resolved",
                label.escape_debug()
            );
            assert!(verified_identities_for_label(&store, &scope, label).is_err());
            assert!(audience_for_label(&store, &scope, label, AudienceKind::Engagement).is_err());
        }
    }
}
