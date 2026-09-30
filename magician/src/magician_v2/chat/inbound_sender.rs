//! **Who just wrote to us?** — the chat ingress's seam into the counterparty
//! register.
//!
//! Doc: `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §3.1.
//!
//! [`crate::magician_v2::counterparties::resolve_inbound`] answers the question
//! and is careful about what the answer may be used for. This file is what
//! stands between it and an HTTP ingress that holds two strings — a channel name
//! and an address — and it exists because the gap between those two strings and
//! a [`ChannelAddress`] is exactly where a wrong guess turns a stranger into a
//! counterparty.
//!
//! # This is the only automatic path that writes to the register
//!
//! Identifying an inbound message advances `last_seen` through
//! [`CounterpartyStore::observe`] — one line, and the only write in the register
//! that cannot touch verification, provenance or `first_seen`. Hearing from an
//! address a hundred times is a hundred repetitions of the same unauthenticated
//! claim, and a path that let volume ripen into trust would verify whoever was
//! noisiest.
//!
//! # An unverified inbound never confers authority
//!
//! [`InboundSender::authority`] answers `Some` for exactly one state, and every
//! other state — a stranger, an unproved channel, an unproved address, an
//! address kind nobody named, a register that is not installed — answers `None`.
//! `None` is never permission. That is the whole reason this returns an enum
//! rather than an `Option<CounterpartyRef>` beside a `verified: bool` the caller
//! could forget to read.
//!
//! # Where the address kind comes from, and what is refused
//!
//! An [`IdentityKind`] is part of every derived id, so getting it wrong looks
//! up a different row. There are exactly two sources, in this order:
//!
//! 1. **The caller names it.** Always wins, always available, and the only one
//!    that scales to a channel nobody here has heard of.
//! 2. **The channel's transport fixes it** — see [`kind_fixed_by_channel`].
//!
//! and when neither answers, nothing is looked up and nothing is written.
//!
//! The register's own note warns against inferring the kind from the channel
//! name, because a per-channel table is "one entry away from saying this
//! platform's `@acme` is that platform's `@acme`". [`kind_fixed_by_channel`] is
//! built so that entry cannot be written: it **never** answers
//! [`IdentityKind::Handle`] and never answers [`IdentityKind::Domain`], it
//! answers only for channels whose transport *is* the address format (an
//! address on a phone network is a phone number), and it answers `None` for
//! every channel that carries more than one shape and for every channel nobody
//! has classified. It also lives here, at the adapter boundary, rather than in
//! the register — the register still takes a kind and takes no view on where a
//! caller got it.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::magician_v2::counterparties::{
    global_counterparty_store, resolve_inbound, ChannelAddress, CounterpartyRef, CounterpartyScope,
    CounterpartyStore, IdentityKind, InboundIdentification, InboundVerification,
};

/// The kind of address a channel's **transport** fixes, when it fixes one.
///
/// Not a convenience table and not a default: every arm below is a channel
/// whose addresses are one shape because the transport says so, and everything
/// else answers `None` so the caller has to name the kind or get no
/// identification at all.
///
/// # What this deliberately does not answer
///
/// - **Never [`IdentityKind::Handle`].** A handle is the one kind whose
///   normalisation depends on a platform qualifier, and a table that mapped a
///   channel to a bare handle is precisely the entry that would make one
///   platform's `@acme` resolve to another's. Handle-shaped channels answer
///   `None`; a caller that knows the platform passes
///   `platform:handle` with the kind named.
/// - **Never [`IdentityKind::Domain`].** A domain is an affiliation hint, not
///   somebody who sends messages.
/// - **Nothing for a channel carrying more than one shape.** `imessage`
///   addresses are phone numbers *or* email addresses and `telegram` addresses
///   are numeric ids, phone numbers *or* `@names`; picking one would look up a
///   row belonging to whoever holds that string under the other kind.
/// - **Nothing for a channel nobody has classified**, for the same reason
///   [`crate::magician_v2::chat::envoy::channel_is_verified`] fails closed on
///   one.
pub fn kind_fixed_by_channel(channel: &str) -> Option<IdentityKind> {
    match channel.trim().to_ascii_lowercase().as_str() {
        // The transport is SMTP; the address is a mailbox. (Which proves
        // nothing about the sender — that is the other leg, and it is why an
        // email can be identified but can never be authoritative.)
        "email" | "mail" | "agentmail" => Some(IdentityKind::Email),
        // A phone network. The address is a number, and `normalise_identity`
        // never infers a country code, so a national number and its E.164 form
        // stay two different identities.
        "whatsapp" | "kapso" | "sms" => Some(IdentityKind::Phone),
        _ => None,
    }
}

/// **Who an inbound message is from, as far as this ingress can tell.**
///
/// Four states, and only one of them can carry authority. The three that cannot
/// are kept apart rather than folded into one "no" because they are different
/// operational facts: a stranger is a fact about the sender, an unnamed address
/// kind is a fact about the adapter, and an absent register is a broken process
/// reporting a healthy one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundSender {
    /// The register was asked and answered. The answer's own variant decides
    /// what it may be used for — see [`InboundIdentification`].
    Identified(InboundIdentification),
    /// This channel carries no address to identify. The owner's own UI is the
    /// case: the authenticated request *is* the sender, so there is nothing to
    /// look up in a register of people we are not.
    NoAddress,
    /// Nothing named the address kind and the channel's transport does not fix
    /// one, so **nothing was looked up and nothing was written**. Not a
    /// stranger: this is a fact about the adapter, and an adapter that names the
    /// kind turns the same message into an identification.
    AddressKindNotNamed { channel: String },
    /// The register could not be asked — nothing is installed in this process,
    /// or an HTTP caller's read of it failed. *"Unreadable"*, never *"nobody"*:
    /// the two produce the same absence of authority and mean opposite things,
    /// and only the second is a fact about a sender. Which of the two it was is
    /// in the caller's log line; neither is ever reported as `Unrecognised`.
    RegisterUnavailable,
}

impl InboundSender {
    /// The counterparty this message is **authoritatively from**.
    ///
    /// `Some` for exactly one state, and only when both proof legs held: the
    /// channel established who sent it *and* an owner has proved the address
    /// reaches that organisation. Everything else — including a known address on
    /// an unproved channel, which is the dangerous-looking one — answers `None`,
    /// and `None` is never permission.
    pub fn authority(&self) -> Option<&CounterpartyRef> {
        match self {
            Self::Identified(identification) => identification.authority(),
            Self::NoAddress | Self::AddressKindNotNamed { .. } | Self::RegisterUnavailable => None,
        }
    }

    /// The counterparty this message **appears** to be from.
    ///
    /// For display, threading and assembling an owner's approval. Never for
    /// reaching anything the counterparty owns.
    pub fn for_context(&self) -> Option<&CounterpartyRef> {
        match self {
            Self::Identified(identification) => identification.for_context(),
            Self::NoAddress | Self::AddressKindNotNamed { .. } | Self::RegisterUnavailable => None,
        }
    }

    /// The register's answer, when it was asked.
    pub fn identification(&self) -> Option<&InboundIdentification> {
        match self {
            Self::Identified(identification) => Some(identification),
            Self::NoAddress | Self::AddressKindNotNamed { .. } | Self::RegisterUnavailable => None,
        }
    }

    /// A stable label for the audit line.
    ///
    /// The identified states report *why* they are not authoritative, because
    /// "this address is not verified" is one owner decision away from being
    /// fixed while "this channel does not prove its sender" is a fact about the
    /// transport nobody can fix.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Identified(identification) => match identification {
                InboundIdentification::Authoritative { .. } => "authoritative",
                InboundIdentification::ContextOnly { why, .. } => why.as_str(),
                InboundIdentification::Unrecognised { .. } => "unrecognised",
            },
            Self::NoAddress => "no_address",
            Self::AddressKindNotNamed { .. } => "address_kind_not_named",
            Self::RegisterUnavailable => "register_unavailable",
        }
    }
}

/// **Identify an inbound sender against a register the caller holds.**
///
/// `named_kind` is the adapter's word about what kind of address this is and
/// always wins; [`kind_fixed_by_channel`] fills in only where the transport
/// settles it. Neither answering is [`InboundSender::AddressKindNotNamed`], not
/// a stranger.
///
/// # Failures are failures, never absences
///
/// An unreadable register propagates as an error rather than folding to
/// "nobody". A caller that logged "stranger" while the log was unreadable would
/// be reporting a healthy system.
pub fn identify_inbound_sender(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    channel: &str,
    address: &str,
    named_kind: Option<IdentityKind>,
    verified: InboundVerification,
    now: DateTime<Utc>,
) -> Result<InboundSender> {
    if address.trim().is_empty() {
        return Ok(InboundSender::NoAddress);
    }
    let Some(kind) = named_kind.or_else(|| kind_fixed_by_channel(channel)) else {
        return Ok(InboundSender::AddressKindNotNamed {
            channel: channel.trim().to_string(),
        });
    };
    let identification = resolve_inbound(
        store,
        scope,
        channel,
        &ChannelAddress::new(kind, address),
        verified,
        now,
    )?;
    Ok(InboundSender::Identified(identification))
}

/// [`identify_inbound_sender`], against the register this process installed.
///
/// The form an HTTP handler uses, because a compiled handler cannot be threaded
/// a constructor argument. A process with no register installed answers
/// [`InboundSender::RegisterUnavailable`] — which carries no authority and is
/// never reported as a stranger.
pub fn identify_inbound_sender_in_process(
    principal: &str,
    workspace: &str,
    channel: &str,
    address: &str,
    named_kind: Option<IdentityKind>,
    verified: InboundVerification,
    now: DateTime<Utc>,
) -> Result<InboundSender> {
    if address.trim().is_empty() {
        return Ok(InboundSender::NoAddress);
    }
    let Some(store) = global_counterparty_store() else {
        return Ok(InboundSender::RegisterUnavailable);
    };
    identify_inbound_sender(
        store.as_ref(),
        &CounterpartyScope::new(principal, workspace),
        channel,
        address,
        named_kind,
        verified,
        now,
    )
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::counterparties::{
        AddIdentity, CreateCounterparty, MintSource, NotAuthoritative, Promotion, TrustedSignal,
    };

    use super::*;

    const PRINCIPAL: &str = "alpha";
    const WORKSPACE: &str = "prod";

    fn filed_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap()
    }

    fn heard_at() -> DateTime<Utc> {
        filed_at() + Duration::hours(3)
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: CounterpartyStore,
        scope: CounterpartyScope,
        acme_id: String,
        phone_identity_id: String,
    }

    impl Fixture {
        /// One organisation: a **proved** phone and an unproved email.
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let store = CounterpartyStore::new(ArtifactV2Workspace::new(dir.path()));
            let scope = CounterpartyScope::new(PRINCIPAL, WORKSPACE);
            let acme = store
                .record_counterparty(
                    &scope,
                    &CreateCounterparty {
                        display_name: "Acme Ltd".to_string(),
                        domain: None,
                        stage: None,
                        created_by: "owner".to_string(),
                    },
                    filed_at(),
                )
                .expect("record counterparty");
            let phone = store
                .add_identity(
                    &scope,
                    &AddIdentity {
                        counterparty_id: acme.counterparty_id.clone(),
                        kind: IdentityKind::Phone,
                        value: "+15550001111".to_string(),
                        source: MintSource::OwnerStated,
                        evidence_ref: "intro-1".to_string(),
                        recorded_by: "owner".to_string(),
                        introduced_by: None,
                    },
                    filed_at(),
                )
                .expect("add phone");
            store
                .promote_identity(
                    &scope,
                    &phone.identity_id,
                    &Promotion {
                        decided_by: "owner".to_string(),
                        evidence_ref: "receipt-1".to_string(),
                        signal: TrustedSignal::new("web", "receipt-1").authenticated(true),
                    },
                    filed_at(),
                )
                .expect("promote phone");
            store
                .add_identity(
                    &scope,
                    &AddIdentity {
                        counterparty_id: acme.counterparty_id.clone(),
                        kind: IdentityKind::Email,
                        value: "ops@acme.com".to_string(),
                        source: MintSource::OwnerStated,
                        evidence_ref: "intro-1".to_string(),
                        recorded_by: "owner".to_string(),
                        introduced_by: None,
                    },
                    filed_at(),
                )
                .expect("add email");
            Self {
                _dir: dir,
                store,
                scope,
                acme_id: acme.counterparty_id,
                phone_identity_id: phone.identity_id,
            }
        }

        fn identify(
            &self,
            channel: &str,
            address: &str,
            named_kind: Option<IdentityKind>,
            verified: InboundVerification,
        ) -> InboundSender {
            identify_inbound_sender(
                &self.store,
                &self.scope,
                channel,
                address,
                named_kind,
                verified,
                heard_at(),
            )
            .expect("identify")
        }
    }

    /// The point of the wiring: a proved sender on a transport that names them
    /// is authoritatively that organisation.
    ///
    /// Pins the opposite failure to every other test here — a seam so cautious
    /// it never identifies anybody, which is indistinguishable from not being
    /// wired at all.
    #[test]
    fn a_proved_address_on_a_proving_channel_is_authoritative() {
        let fixture = Fixture::new();
        let sender = fixture.identify(
            "whatsapp",
            "+1 (555) 000-1111",
            None,
            InboundVerification::from_boundary(true),
        );
        assert_eq!(sender.reason(), "authoritative");
        assert_eq!(
            sender.authority().map(CounterpartyRef::as_str),
            Some(fixture.acme_id.as_str())
        );
        assert_eq!(
            sender
                .identification()
                .and_then(InboundIdentification::identity_id),
            Some(fixture.phone_identity_id.as_str())
        );
    }

    /// **An unverified inbound must never confer authority.**
    ///
    /// Pins the escalation this whole seam exists to prevent: a lookalike sender
    /// writes from an address the register holds, and a caller that read
    /// `for_context()` — or an implementation that returned one reference with a
    /// flag beside it — would hand that message an organisation's authority. The
    /// wrong answer is shaped exactly like the right one, so nothing downstream
    /// could catch it.
    ///
    /// Every case uses the SAME register as the authoritative test above, so the
    /// only thing that differs is the proof.
    #[test]
    fn an_unproved_leg_is_context_only_and_never_authority() {
        let fixture = Fixture::new();
        let cases: Vec<(
            &str,
            &str,
            Option<IdentityKind>,
            InboundVerification,
            NotAuthoritative,
        )> = vec![
            // SMTP does not authenticate `From:`, however proved our own
            // request was. The promoted address does not rescue it.
            (
                "email",
                "ops@acme.com",
                None,
                InboundVerification::from_boundary(true),
                NotAuthoritative::NeitherProved,
            ),
            // A proving transport, but our own boundary proved nothing.
            (
                "whatsapp",
                "+15550001111",
                None,
                InboundVerification::from_boundary(false),
                NotAuthoritative::ChannelDidNotProveSender,
            ),
            // An adapter claiming its own traffic is verified may not raise.
            (
                "email",
                "ops@acme.com",
                None,
                InboundVerification::from_boundary(true).with_caller_claim(Some(true)),
                NotAuthoritative::NeitherProved,
            ),
            // A caller de-escalating is honoured.
            (
                "whatsapp",
                "+15550001111",
                None,
                InboundVerification::from_boundary(true).with_caller_claim(Some(false)),
                NotAuthoritative::ChannelDidNotProveSender,
            ),
            // Proving channel, proved request, address nobody promoted: the
            // other missing leg.
            (
                "whatsapp",
                "ops@acme.com",
                Some(IdentityKind::Email),
                InboundVerification::from_boundary(true),
                NotAuthoritative::AddressNotVerified,
            ),
        ];

        for (channel, address, named_kind, verified, expected) in cases {
            let sender = fixture.identify(channel, address, named_kind, verified);
            assert_eq!(
                sender.for_context().map(CounterpartyRef::as_str),
                Some(fixture.acme_id.as_str()),
                "{channel}/{address}: the fixture must be the dangerous case — resolvable, unproved"
            );
            assert_eq!(
                sender.authority(),
                None,
                "{channel}/{address} conferred authority without proof"
            );
            assert_eq!(sender.reason(), expected.as_str(), "{channel}/{address}");
        }
    }

    /// **Identifying an inbound message moves `last_seen` and nothing else.**
    ///
    /// Pins two failures at once. First, a seam that wrote nothing: the register
    /// would stay empty of any evidence we are still in contact, and the silence
    /// sweep that reads `last_seen` would report a live conversation as dead.
    /// Second — the dangerous one — a seam that let an inbound message ripen an
    /// address into a verified one, or overwrite its provenance: hearing from an
    /// address is a repetition of the same unauthenticated claim, and a register
    /// that trusted repetition would verify whoever was noisiest.
    #[test]
    fn identifying_a_known_address_advances_last_seen_and_touches_nothing_else() {
        let fixture = Fixture::new();
        let before = fixture
            .store
            .load_identity(&fixture.scope, &fixture.phone_identity_id)
            .expect("load")
            .expect("on file");
        assert_eq!(before.last_seen, filed_at());

        // Deliberately the UNPROVED case: an observation is recorded whether or
        // not the channel proved the sender, because the message did arrive.
        let sender = fixture.identify(
            "whatsapp",
            "+15550001111",
            None,
            InboundVerification::unproven(),
        );
        assert_eq!(sender.reason(), "channel_did_not_prove_sender");

        let after = fixture
            .store
            .load_identity(&fixture.scope, &fixture.phone_identity_id)
            .expect("load")
            .expect("on file");
        assert_eq!(after.last_seen, heard_at(), "`last_seen` did not advance");
        assert_eq!(after.first_seen, filed_at(), "`first_seen` moved");
        assert_eq!(after.verification, before.verification);
        assert_eq!(after.minted_by, before.minted_by);
    }

    /// An address the register has never heard of is a stranger, and stays one.
    ///
    /// Pins a seam that minted on an observation: the register would fill with
    /// identities whose provenance is "something mentioned it", and an owner
    /// reviewing the book could not tell those from addresses somebody actually
    /// stated.
    #[test]
    fn a_stranger_is_unrecognised_and_grows_the_register_by_nothing() {
        let fixture = Fixture::new();
        let sender = fixture.identify(
            "whatsapp",
            "+15559999999",
            None,
            InboundVerification::from_boundary(true),
        );
        assert_eq!(sender.reason(), "unrecognised");
        assert_eq!(sender.authority(), None);
        assert_eq!(sender.for_context(), None);
        assert_eq!(
            fixture
                .store
                .identities_for(&fixture.scope, &fixture.acme_id)
                .expect("identities")
                .len(),
            2,
            "an unrecognised inbound minted an identity"
        );
        assert!(fixture
            .store
            .resolve(&fixture.scope, IdentityKind::Phone, "+15559999999")
            .expect("resolve")
            .is_none());
    }

    /// **A channel whose address shape nobody fixed looks nothing up.**
    ///
    /// Pins the per-channel guess the register refuses by construction: this
    /// telegram id happens to be identical to a phone number on file, so a seam
    /// that defaulted the kind would resolve a telegram account to a phone
    /// number's organisation — and, because telegram's transport does prove its
    /// sender, would hand it that organisation's authority outright.
    #[test]
    fn an_unclassified_channel_with_no_named_kind_identifies_nobody() {
        let fixture = Fixture::new();
        let sender = fixture.identify(
            "telegram",
            "+15550001111",
            None,
            InboundVerification::from_boundary(true),
        );
        assert_eq!(
            sender,
            InboundSender::AddressKindNotNamed {
                channel: "telegram".to_string()
            }
        );
        assert_eq!(sender.authority(), None);
        assert_eq!(sender.for_context(), None);
        assert_eq!(
            fixture
                .store
                .load_identity(&fixture.scope, &fixture.phone_identity_id)
                .expect("load")
                .expect("on file")
                .last_seen,
            filed_at(),
            "a channel nobody classified still wrote an observation"
        );
    }

    /// The adapter's own word about the address kind wins over the channel's.
    ///
    /// Pins a classifier that overrode its caller: an adapter that knows this
    /// WhatsApp Business thread is keyed by an email address would otherwise
    /// have its answer silently replaced by "phone", and the lookup would land
    /// on a different row or on none.
    #[test]
    fn a_named_kind_wins_over_the_channels_fixed_shape() {
        let fixture = Fixture::new();
        assert_eq!(kind_fixed_by_channel("whatsapp"), Some(IdentityKind::Phone));

        let sender = fixture.identify(
            "whatsapp",
            "ops@acme.com",
            Some(IdentityKind::Email),
            InboundVerification::from_boundary(true),
        );
        assert_eq!(sender.reason(), "address_not_verified");
        assert_eq!(
            sender.for_context().map(CounterpartyRef::as_str),
            Some(fixture.acme_id.as_str())
        );
    }

    /// The owner's own UI has no address to identify, and asks nothing.
    ///
    /// Pins a seam that treated the owner's every poll of the chat endpoint as
    /// an inbound message from a stranger — noise in the audit line, and a
    /// register read on every request that has nothing to look up.
    #[test]
    fn a_channel_with_no_address_identifies_nobody() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.identify("web", "", None, InboundVerification::from_boundary(true)),
            InboundSender::NoAddress
        );
        assert_eq!(
            fixture.identify("web", "   ", None, InboundVerification::from_boundary(true)),
            InboundSender::NoAddress
        );
    }

    /// The channel table answers only where the transport settles the shape.
    ///
    /// Pins the entry that must never be written: a channel mapped to
    /// [`IdentityKind::Handle`] is one step from resolving one platform's
    /// `@acme` to another's, which is the authority hand-off the register
    /// refuses by construction. Ambiguous and unknown channels stay `None`.
    #[test]
    fn the_channel_table_never_answers_handle_or_domain() {
        for channel in ["email", "mail", "agentmail", "whatsapp", "kapso", "sms"] {
            let kind = kind_fixed_by_channel(channel).expect("a fixed shape");
            assert!(
                matches!(kind, IdentityKind::Email | IdentityKind::Phone),
                "{channel} mapped to {}",
                kind.as_str()
            );
        }
        for channel in ["telegram", "imessage", "web", "discord", "", "  "] {
            assert_eq!(
                kind_fixed_by_channel(channel),
                None,
                "{channel} was given an address shape its transport does not fix"
            );
        }
        assert_eq!(
            kind_fixed_by_channel(" WhatsApp "),
            Some(IdentityKind::Phone)
        );
    }

    /// A register nobody installed is *unreadable*, never *nobody*.
    ///
    /// Pins the state that reads as a healthy system: folding "no register in
    /// this process" into `Unrecognised` would fill an audit log with strangers
    /// while the real fact was that the binary never installed a register, and
    /// the two look identical from every surface downstream.
    ///
    /// Nothing installs the process-wide register in this crate's test binary,
    /// so this asserts the uninstalled state exactly. A future test that does
    /// install one will fail here loudly rather than quietly weaken the
    /// assertion.
    #[test]
    fn an_uninstalled_register_is_named_rather_than_reported_as_a_stranger() {
        assert!(
            global_counterparty_store().is_none(),
            "this test asserts the uninstalled state; something installed a register"
        );
        let sender = identify_inbound_sender_in_process(
            PRINCIPAL,
            WORKSPACE,
            "whatsapp",
            "+15550001111",
            None,
            InboundVerification::from_boundary(true),
            heard_at(),
        )
        .expect("identify");
        assert_eq!(sender, InboundSender::RegisterUnavailable);
        assert_eq!(sender.reason(), "register_unavailable");
        assert_eq!(sender.authority(), None);

        // And it short-circuits an addressless channel before it ever asks,
        // so the owner's own UI is not reported as a broken process.
        assert_eq!(
            identify_inbound_sender_in_process(
                PRINCIPAL,
                WORKSPACE,
                "web",
                "",
                None,
                InboundVerification::from_boundary(true),
                heard_at(),
            )
            .expect("identify"),
            InboundSender::NoAddress
        );
    }
}
