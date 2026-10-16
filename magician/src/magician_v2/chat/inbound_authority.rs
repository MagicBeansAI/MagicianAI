//! **Does this inbound message reach an engagement's lane?**
//!
//! Doc: `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §4.2
//! branch 2. This is the seam between two modules that were built not to know
//! about each other: [`crate::magician_v2::counterparties`] answers *whose
//! address is this, and did anybody prove it*, and
//! [`crate::magician_v2::engagements`] answers *what may be done, by whom,
//! until when*. Neither grants anything on its own. This file joins them, and
//! joining them is the only place the escalation can happen, so it is the only
//! place worth reading closely.
//!
//! # One line does the work
//!
//! ```text
//! let Some(counterparty) = identification.authority() else { return NotReached };
//! ```
//!
//! [`InboundIdentification::authority`] answers `Some` for exactly one variant.
//! `ContextOnly` — the address is on file, but either the channel could not
//! establish who sent this message or nobody has proved the address reaches
//! that organisation — answers `None`, and so does `Unrecognised`. **`None` is
//! not a weaker yes.** A `ContextOnly` sender is one forgeable `From:` header
//! away from being anybody, and an engagement lane is where a counterparty's
//! private material and an agent holding its grant live.
//!
//! Nothing here promotes. A sender that resolves as context can become
//! authoritative only through an owner-made promotion carrying a server-trusted
//! signal ([`crate::magician_v2::counterparty_store::TrustedSignal`]), and that
//! decision is recorded against the identity, not inferred here from a domain,
//! a display name, or how often we have heard from them.
//!
//! # Three further fail-closed steps
//!
//! Authority over an *organisation* is still not a lane. After the proof:
//!
//! - the engagement must be **live** at this instant, read from the clock at
//!   read time — a revoked or expired engagement degrades to no lane, which is
//!   the guest lane, which is safe by default;
//! - exactly **one** live engagement may name the counterparty. Two is not a
//!   tie to break: the plan's model is one engagement per (program,
//!   counterparty), so a counterparty in two programs is legitimate and
//!   choosing between them would be a guess with a real-world effect;
//! - an owner must have **named the agent that fronts it**
//!   ([`crate::magician_v2::engagements::EngagementStore::set_owner_agent`]).
//!   Nothing derives that agent from `team[]` or from whoever last sent
//!   outbound — a derived front is an automatic promotion wearing a heuristic.
//!
//! # Generic first
//!
//! Nothing here names a programme. "A message arrived from an address we have
//! proved belongs to an organisation we have open work with" is the shape, and
//! the work is a supplier order, an application, a case, or a deal.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

use crate::magician_v2::counterparty_consumers::InboundIdentification;
use crate::magician_v2::counterparty_store::{CounterpartyScope, CounterpartyStore};
use crate::magician_v2::engagements::{EngagementAuthority, EngagementStore};

use super::envoy::{engagement_thread_id, EngagementLane};

/// Whether an inbound message reaches an engagement, and when it does not, why.
///
/// The reason is not decoration: "this sender is not authoritative" and "this
/// counterparty has two live engagements" are the same routing outcome and
/// completely different operational facts, and the second is the one an owner
/// can fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundEngagementOutcome {
    /// A proved identity of a counterparty with exactly one live, fronted
    /// engagement. The lane still routes nowhere new until
    /// [`crate::config::EnvoyConfig::engagement_forwarding_enabled`] is on.
    Reached(EngagementLane),
    /// No lane. The message stays in the guest lane it was already in.
    NotReached(NoEngagementLane),
}

impl InboundEngagementOutcome {
    /// The lane, for feeding to
    /// [`crate::magician_v2::chat::envoy::resolve_inbound_lane`].
    pub fn into_lane(self) -> Option<EngagementLane> {
        match self {
            Self::Reached(lane) => Some(lane),
            Self::NotReached(_) => None,
        }
    }

    /// A stable label for logs and review.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Reached(_) => "reached",
            Self::NotReached(why) => why.as_str(),
        }
    }
}

/// Why an inbound message reached no engagement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoEngagementLane {
    /// The message is not authoritatively from anybody — a stranger, or a
    /// known address on a message nothing proved. **The load-bearing case.**
    SenderNotAuthoritative,
    /// The sender is proved, and no live engagement names their organisation.
    /// Covers "none was ever opened", "it expired" and "it was revoked"
    /// identically on purpose: all three mean there is no open work, and the
    /// difference belongs to the engagement surface, not to routing.
    NoLiveEngagement,
    /// One live engagement matched and no owner has named the agent that
    /// fronts it, so the engagement names nowhere for the message to go.
    NoOwnerNamedAgent { engagement_id: String },
    /// More than one live engagement names this counterparty. Fail closed:
    /// picking one would be a guess that hands a message to an agent holding a
    /// grant the owner scoped to different work.
    Ambiguous { count: usize },
    /// The register or the roster **could not be asked** — nothing installed in
    /// this process, or a read that failed. Reported as itself and never folded
    /// into [`Self::NoLiveEngagement`]: the two route identically and mean
    /// opposite things, and only the second is a fact about a counterparty. A
    /// broken process must not appear in the log as a healthy one with no open
    /// work.
    AuthorityUnreadable,
}

impl NoEngagementLane {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SenderNotAuthoritative => "sender_not_authoritative",
            Self::NoLiveEngagement => "no_live_engagement",
            Self::NoOwnerNamedAgent { .. } => "no_owner_named_agent",
            Self::Ambiguous { .. } => "ambiguous_engagement",
            Self::AuthorityUnreadable => "authority_unreadable",
        }
    }
}

/// **Which engagement, if any, does this inbound message reach?**
///
/// The scope is derived once from `principal`/`workspace` and used for both
/// stores, so the register and the roster cannot be read at different scopes by
/// a caller that assembled two of them.
///
/// # Failures are failures, never absences
///
/// An unreadable register propagates as an error rather than folding to "no
/// lane". "The log could not be read" and "this sender reaches nothing" are
/// different answers, and only the second is one a caller may act on — though
/// both happen to be safe here, a caller that logged the second while the first
/// was true would be reporting a healthy system.
pub async fn engagement_lane_for_inbound(
    counterparties: &CounterpartyStore,
    engagements: &EngagementStore,
    principal: &str,
    workspace: &str,
    identification: &InboundIdentification,
    now: DateTime<Utc>,
) -> Result<InboundEngagementOutcome> {
    // THE LINE THIS MODULE EXISTS FOR. `authority()` answers `Some` for exactly
    // one variant; `ContextOnly` and `Unrecognised` both answer `None`, and
    // `None` is never permission. An unverified resolution must not confer
    // authority, which is the escalation the whole engagements plan is arranged
    // to prevent.
    let Some(counterparty) = identification.authority() else {
        return Ok(InboundEngagementOutcome::NotReached(
            NoEngagementLane::SenderNotAuthoritative,
        ));
    };

    let scope = CounterpartyScope::new(principal, workspace);
    let now_ms = now.timestamp_millis();

    // Liveness first, and from the clock at read time — never cached, because
    // revocation and expiry are exactly the facts a cached answer would be
    // wrong about. This is an in-memory roster read; the expensive question
    // comes after, and is asked only about what survives here.
    let mut live: Vec<EngagementAuthority> = Vec::new();
    for record in engagements.list(principal, workspace).await {
        if engagements
            .live_authority(principal, workspace, &record.engagement_id, now_ms)
            .await
            .is_ok()
        {
            live.push(record);
        }
    }

    // ONE register read for every label, not two per engagement.
    //
    // `counterparty_for_engagement` costs a `load` and a `summary` — two full
    // reads and parses of the register — and calling it in this loop put `2N`
    // of them on a chat request the moment a proved sender arrived. The batch
    // form answers identically: merge edges followed, a label the register does
    // not hold resolving to nothing rather than to a near miss.
    let labels: Vec<String> = live
        .iter()
        .map(|record| record.counterparty.clone())
        .collect();
    let resolved = counterparties.resolve_labels(&scope, &labels)?;

    let live_matches: Vec<EngagementAuthority> = live
        .into_iter()
        .filter(|record| {
            resolved
                .get(&record.counterparty)
                .and_then(Option::as_ref)
                .is_some_and(|engaged| engaged.as_str() == counterparty.as_str())
        })
        .collect();

    if live_matches.is_empty() {
        return Ok(InboundEngagementOutcome::NotReached(
            NoEngagementLane::NoLiveEngagement,
        ));
    }
    if live_matches.len() > 1 {
        return Ok(InboundEngagementOutcome::NotReached(
            NoEngagementLane::Ambiguous {
                count: live_matches.len(),
            },
        ));
    }

    let matched = &live_matches[0];
    let owner_agent_id = matched
        .owner_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|agent| !agent.is_empty());
    let Some(owner_agent_id) = owner_agent_id else {
        return Ok(InboundEngagementOutcome::NotReached(
            NoEngagementLane::NoOwnerNamedAgent {
                engagement_id: matched.engagement_id.clone(),
            },
        ));
    };

    // The second gate, deliberately not the same one. `for_authoritative_sender`
    // re-reads the identification rather than trusting that this function
    // checked it, so a future edit that reorders the checks above cannot mint a
    // lane for a context-only sender.
    let lane = EngagementLane::for_authoritative_sender(
        identification,
        &matched.engagement_id,
        owner_agent_id,
        &engagement_thread_id(&matched.engagement_id),
    )
    .ok_or_else(|| {
        anyhow!(
            "engagement `{}` matched an authoritative sender and still could not mint a lane; \
             the two checks disagree, which means one of them changed",
            matched.engagement_id
        )
    })?;
    Ok(InboundEngagementOutcome::Reached(lane))
}

/// The form an HTTP handler uses, because a compiled handler cannot be threaded
/// two constructor arguments.
///
/// Both stores come from the process-wide handles the binary installs at boot.
/// A process missing either answers
/// [`NoEngagementLane::AuthorityUnreadable`], which routes exactly like "no
/// engagement" and reads in the log as what it is.
///
/// # Why this takes an `InboundSender` and not an address
///
/// The identification is the caller's already — it did the register read that
/// advances `last_seen`, and re-deriving it here would ask the register twice
/// and could get two different answers for one message. Taking it also makes
/// the fail-closed rule structural: a caller holding an `InboundSender` that
/// carries no authority gets no lane, and there is no argument it can pass to
/// change that.
pub async fn engagement_lane_in_process(
    principal: &str,
    workspace: &str,
    sender: &crate::magician_v2::chat::inbound_sender::InboundSender,
    now: DateTime<Utc>,
) -> InboundEngagementOutcome {
    use crate::magician_v2::counterparties::global_counterparty_store;
    use crate::magician_v2::engagements::global_engagement_store;

    // Not an identification at all — no address, an unnamed address kind, or a
    // register that could not be read. None of the three is a sender we may act
    // on, and only the third is a fault; the caller has already logged which.
    let Some(identification) = sender.identification() else {
        return InboundEngagementOutcome::NotReached(match sender {
            crate::magician_v2::chat::inbound_sender::InboundSender::RegisterUnavailable => {
                NoEngagementLane::AuthorityUnreadable
            },
            _ => NoEngagementLane::SenderNotAuthoritative,
        });
    };
    let (Some(counterparties), Some(engagements)) =
        (global_counterparty_store(), global_engagement_store())
    else {
        return InboundEngagementOutcome::NotReached(NoEngagementLane::AuthorityUnreadable);
    };

    match engagement_lane_for_inbound(
        counterparties.as_ref(),
        engagements.as_ref(),
        principal,
        workspace,
        identification,
        now,
    )
    .await
    {
        Ok(outcome) => outcome,
        // An unreadable log is a failure. It degrades to the guest lane like
        // everything else here, and it says so.
        Err(error) => {
            tracing::warn!(
                "[INBOUND-AUTHORITY] the engagement lane could not be resolved: {error}"
            );
            InboundEngagementOutcome::NotReached(NoEngagementLane::AuthorityUnreadable)
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::TimeZone;

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::counterparty_consumers::{
        resolve_inbound, ChannelAddress, InboundVerification,
    };
    use crate::magician_v2::counterparty_types::{
        AddIdentity, CreateCounterparty, IdentityKind, MintSource, Promotion, TrustedSignal,
    };

    use super::*;

    const PRINCIPAL: &str = "alpha";
    const WORKSPACE: &str = "prod";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap()
    }

    fn names(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    struct Fixture {
        _register_dir: tempfile::TempDir,
        _roster_dir: tempfile::TempDir,
        counterparties: CounterpartyStore,
        engagements: EngagementStore,
        scope: CounterpartyScope,
    }

    impl Fixture {
        /// One organisation, one **proved** phone and one unproved email.
        async fn new() -> Self {
            let register_dir = tempfile::tempdir().expect("register dir");
            let roster_dir = tempfile::tempdir().expect("roster dir");
            let counterparties =
                CounterpartyStore::new(ArtifactV2Workspace::new(register_dir.path()));
            let engagements = EngagementStore::open(roster_dir.path())
                .await
                .expect("open roster");
            let scope = CounterpartyScope::new(PRINCIPAL, WORKSPACE);

            let acme = counterparties
                .record_counterparty(
                    &scope,
                    &CreateCounterparty {
                        display_name: "Acme Ltd".to_string(),
                        domain: None,
                        stage: None,
                        created_by: "owner".to_string(),
                    },
                    now(),
                )
                .expect("record counterparty");

            let phone = counterparties
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
                    now(),
                )
                .expect("add phone");
            counterparties
                .promote_identity(
                    &scope,
                    &phone.identity_id,
                    &Promotion {
                        decided_by: "owner".to_string(),
                        evidence_ref: "receipt-1".to_string(),
                        signal: TrustedSignal::new("web", "receipt-1").authenticated(true),
                    },
                    now(),
                )
                .expect("promote phone");

            // On file, never promoted — the case an owner has not decided yet.
            counterparties
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
                    now(),
                )
                .expect("add email");

            Self {
                _register_dir: register_dir,
                _roster_dir: roster_dir,
                counterparties,
                engagements,
                scope,
            }
        }

        /// A live engagement for `label`, fronted unless `front` is `None`.
        async fn engage(&self, program_id: &str, label: &str, front: Option<&str>) -> String {
            let created = self
                .engagements
                .create(
                    PRINCIPAL,
                    WORKSPACE,
                    program_id,
                    label,
                    names(&["research"]),
                    names(&["worker-agent"]),
                    now().timestamp_millis(),
                    now().timestamp_millis() + 60_000,
                )
                .await
                .expect("create engagement");
            if let Some(front) = front {
                self.engagements
                    .set_owner_agent(&created.engagement_id, front)
                    .await
                    .expect("name the front");
            }
            created.engagement_id
        }

        /// Identify an inbound message exactly as an adapter would.
        fn identify(
            &self,
            channel: &str,
            kind: IdentityKind,
            value: &str,
            verified: InboundVerification,
        ) -> InboundIdentification {
            resolve_inbound(
                &self.counterparties,
                &self.scope,
                channel,
                &ChannelAddress::new(kind, value),
                verified,
                now(),
            )
            .expect("identify")
        }

        async fn outcome(
            &self,
            identification: &InboundIdentification,
        ) -> InboundEngagementOutcome {
            engagement_lane_for_inbound(
                &self.counterparties,
                &self.engagements,
                PRINCIPAL,
                WORKSPACE,
                identification,
                now(),
            )
            .await
            .expect("resolve lane")
        }
    }

    /// The whole point of the wiring: a proved identity of an engaged
    /// counterparty names that engagement's lane.
    ///
    /// Pins the opposite failure to every other test here — a join so cautious
    /// it never joins. Without this, `resolve_inbound` and the engagement
    /// roster stay two facts nothing acts on and branch 2 can never ship.
    #[tokio::test]
    async fn a_proved_identity_of_an_engaged_counterparty_reaches_that_engagements_lane() {
        let fixture = Fixture::new().await;
        let engagement_id = fixture.engage("p-1", "Acme Ltd", Some("ambassador")).await;

        // WhatsApp names its sender, and our boundary proved the request.
        let identification = fixture.identify(
            "whatsapp",
            IdentityKind::Phone,
            "+15550001111",
            InboundVerification::from_boundary(true),
        );
        assert!(
            identification.authority().is_some(),
            "the fixture must be the authoritative case"
        );

        let lane = fixture
            .outcome(&identification)
            .await
            .into_lane()
            .expect("a proved sender with one fronted live engagement reaches it");
        assert_eq!(lane.engagement_id(), engagement_id);
        assert_eq!(lane.owner_agent_id(), "ambassador");
        assert_eq!(lane.ui_thread_id(), format!("eng:{engagement_id}"));
    }

    /// **An UNVERIFIED resolution must never confer authority.**
    ///
    /// The failure pinned is the escalation the engagements plan exists for: a
    /// lookalike sender writes from an address that is on file, the register
    /// resolves it to a real organisation, and — if this joined on
    /// `for_context` instead of `authority` — the message reaches an agent
    /// holding that organisation's grant. The wrong answer is shaped exactly
    /// like the right one, so nothing downstream could catch it.
    ///
    /// Every leg is exercised against the SAME organisation and the SAME live,
    /// fronted engagement that the test above reaches, so the only difference
    /// is the proof.
    #[tokio::test]
    async fn an_unverified_resolution_never_reaches_the_lane() {
        let fixture = Fixture::new().await;
        fixture.engage("p-1", "Acme Ltd", Some("ambassador")).await;

        let cases: Vec<(&str, IdentityKind, &str, InboundVerification)> = vec![
            // SMTP does not authenticate `From:`, however proved our own
            // request was. The promoted address does not rescue it.
            (
                "email",
                IdentityKind::Phone,
                "+15550001111",
                InboundVerification::from_boundary(true),
            ),
            // The adapter claiming its own traffic is verified may not raise.
            (
                "agentmail",
                IdentityKind::Phone,
                "+15550001111",
                InboundVerification::from_boundary(true).with_caller_claim(Some(true)),
            ),
            // A trusted transport with an unproved request is not proof.
            (
                "whatsapp",
                IdentityKind::Phone,
                "+15550001111",
                InboundVerification::from_boundary(false),
            ),
            // An adapter nobody taught to fill this in proves nothing.
            (
                "whatsapp",
                IdentityKind::Phone,
                "+15550001111",
                InboundVerification::unproven(),
            ),
            // A caller de-escalating is honoured.
            (
                "whatsapp",
                IdentityKind::Phone,
                "+15550001111",
                InboundVerification::from_boundary(true).with_caller_claim(Some(false)),
            ),
            // Proved channel, address nobody promoted: the other missing leg.
            (
                "whatsapp",
                IdentityKind::Email,
                "ops@acme.com",
                InboundVerification::from_boundary(true),
            ),
        ];

        for (channel, kind, value, verification) in cases {
            let identification = fixture.identify(channel, kind, value, verification);
            assert!(
                identification.for_context().is_some(),
                "{channel}/{value}: the fixture must be the dangerous case — resolvable, unproved"
            );
            assert_eq!(
                fixture.outcome(&identification).await,
                InboundEngagementOutcome::NotReached(NoEngagementLane::SenderNotAuthoritative),
                "{channel}/{value} reached an engagement lane without proof"
            );
        }
    }

    /// A stranger reaches nothing, and reaches it for the sender reason rather
    /// than the roster reason.
    ///
    /// Pins a join that reported "no live engagement" for an address it had
    /// never heard of — the log would then read as though an engagement had
    /// lapsed when in fact nobody knows who wrote.
    #[tokio::test]
    async fn a_stranger_reaches_no_lane() {
        let fixture = Fixture::new().await;
        fixture.engage("p-1", "Acme Ltd", Some("ambassador")).await;

        let identification = fixture.identify(
            "whatsapp",
            IdentityKind::Phone,
            "+15559999999",
            InboundVerification::from_boundary(true),
        );
        assert_eq!(
            fixture.outcome(&identification).await,
            InboundEngagementOutcome::NotReached(NoEngagementLane::SenderNotAuthoritative)
        );
    }

    fn proved(fixture: &Fixture) -> InboundIdentification {
        fixture.identify(
            "whatsapp",
            IdentityKind::Phone,
            "+15550001111",
            InboundVerification::from_boundary(true),
        )
    }

    /// **Revoking an engagement takes effect on the next inbound**, with no
    /// cleanup — an acceptance criterion of the plan.
    ///
    /// Pins a join that reads the roster once, or that treats "the record
    /// exists" as "the engagement is open": the owner's revocation would then
    /// keep delivering to the agent it was meant to cut off.
    #[tokio::test]
    async fn a_revoked_engagement_degrades_to_no_lane() {
        let fixture = Fixture::new().await;
        let engagement_id = fixture.engage("p-1", "Acme Ltd", Some("ambassador")).await;
        assert!(fixture
            .outcome(&proved(&fixture))
            .await
            .into_lane()
            .is_some());

        fixture
            .engagements
            .revoke(&engagement_id, now().timestamp_millis())
            .await
            .expect("revoke");

        assert_eq!(
            fixture.outcome(&proved(&fixture)).await,
            InboundEngagementOutcome::NotReached(NoEngagementLane::NoLiveEngagement)
        );
    }

    /// Expiry closes the lane with no owner act, and it is **inclusive**: at
    /// exactly `expires_at_ms` the engagement is already over.
    ///
    /// Pins the off-by-one that leaves an engagement authoritative for one more
    /// instant than the owner granted — and, because routing reads the same
    /// clock the dispatch boundary does, pins the two disagreeing.
    #[tokio::test]
    async fn expiry_closes_the_lane_and_the_boundary_instant_is_already_over() {
        let fixture = Fixture::new().await;
        let expires_at = now().timestamp_millis();
        fixture
            .engagements
            .create(
                PRINCIPAL,
                WORKSPACE,
                "p-1",
                "Acme Ltd",
                names(&["research"]),
                names(&["worker-agent"]),
                expires_at - 60_000,
                expires_at,
            )
            .await
            .expect("create");

        // `now()` IS the expiry instant.
        assert_eq!(
            fixture.outcome(&proved(&fixture)).await,
            InboundEngagementOutcome::NotReached(NoEngagementLane::NoLiveEngagement)
        );
    }

    /// **No front, no lane.** An engagement nobody staffed names nowhere for a
    /// message to go.
    ///
    /// Pins the automatic path this design refuses to build: deriving the
    /// forwarding agent from `team[]`, from the program, or from whoever last
    /// sent outbound would open a lane on every engagement in the roster
    /// without an owner having chosen any of them.
    #[tokio::test]
    async fn an_engagement_with_no_owner_named_agent_has_no_lane() {
        let fixture = Fixture::new().await;
        let engagement_id = fixture.engage("p-1", "Acme Ltd", None).await;

        assert_eq!(
            fixture.outcome(&proved(&fixture)).await,
            InboundEngagementOutcome::NotReached(NoEngagementLane::NoOwnerNamedAgent {
                engagement_id: engagement_id.clone()
            })
        );

        // And the owner act opens it, in one call, with nothing else changed.
        fixture
            .engagements
            .set_owner_agent(&engagement_id, "ambassador")
            .await
            .expect("name the front");
        assert_eq!(
            fixture
                .outcome(&proved(&fixture))
                .await
                .into_lane()
                .map(|lane| lane.owner_agent_id().to_string()),
            Some("ambassador".to_string())
        );
    }

    /// Two live engagements for one counterparty are never guessed between.
    ///
    /// Pins a `find()` that takes the first match: a counterparty in two
    /// programs is the plan's own model, so the first-match answer is whichever
    /// engagement happens to sit earlier in the roster — and it hands the
    /// message to an agent holding a grant the owner scoped to other work.
    #[tokio::test]
    async fn two_live_engagements_for_one_counterparty_are_never_guessed_between() {
        let fixture = Fixture::new().await;
        fixture.engage("p-1", "Acme Ltd", Some("ambassador")).await;
        fixture
            .engage("p-2", "Acme Ltd", Some("other-ambassador"))
            .await;

        assert_eq!(
            fixture.outcome(&proved(&fixture)).await,
            InboundEngagementOutcome::NotReached(NoEngagementLane::Ambiguous { count: 2 })
        );
    }

    /// An engagement labelled for a *different* organisation does not match,
    /// however close the label reads.
    ///
    /// Pins the near-miss: a lookalike label resolving to the real
    /// organisation's engagement is the same authority hand-off the register
    /// refuses on addresses, arriving through the label instead.
    #[tokio::test]
    async fn an_engagement_labelled_for_another_organisation_does_not_match() {
        let fixture = Fixture::new().await;
        fixture
            .engage("p-1", "Acme Holdings", Some("ambassador"))
            .await;

        assert_eq!(
            fixture.outcome(&proved(&fixture)).await,
            InboundEngagementOutcome::NotReached(NoEngagementLane::NoLiveEngagement)
        );
    }

    /// An engagement minted in another scope answers nobody here.
    ///
    /// Pins a roster read that forgot the scope — the same property every store
    /// in this system holds, arriving on the path where getting it wrong routes
    /// one principal's counterparty into another principal's agent.
    #[tokio::test]
    async fn another_scopes_engagement_is_not_reachable() {
        let fixture = Fixture::new().await;
        let created = fixture
            .engagements
            .create(
                "beta",
                WORKSPACE,
                "p-1",
                "Acme Ltd",
                names(&["research"]),
                names(&["worker-agent"]),
                now().timestamp_millis(),
                now().timestamp_millis() + 60_000,
            )
            .await
            .expect("create");
        fixture
            .engagements
            .set_owner_agent(&created.engagement_id, "ambassador")
            .await
            .expect("name the front");

        assert_eq!(
            fixture.outcome(&proved(&fixture)).await,
            InboundEngagementOutcome::NotReached(NoEngagementLane::NoLiveEngagement)
        );
    }
}
