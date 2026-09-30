//! **The seam a flow records an outbound write through.**
//!
//! Doc: `docs/plans/2026-08-07-opc-engagements-contextual-authority.md` §3.1.
//!
//! [`mint_from_outbound`] takes an [`OutboundEvidence`] that already names the
//! organisation, because the store's rule is index before row: an address is
//! filed under an organisation that is already recorded and still live. Real
//! send paths do not hold that id. They hold one of three things, and this file
//! is the one place the three are turned into it:
//!
//! - the organisation itself, when the flow already resolved it;
//! - an **owner-typed organisation label** — an engagement's counterparty, a
//!   support case's account, a vendor file, a candidate's employer. Every flow
//!   that talks to somebody outside carries one under a different word, and
//!   [`resolve_label`] is the generic form that reads them all;
//! - **nothing**, because the flow genuinely does not know who this address
//!   belongs to. The register's own answer for the address is then the only
//!   honest source, and when it has none this call files nothing.
//!
//! # The two things this file refuses to do
//!
//! **It never invents an organisation.** A label the register has never heard
//! of comes back as a [`CounterpartyLead`] and *nothing is written*. Growing the
//! register from whatever string a send path happened to carry would make a typo
//! a permanent organisation whose provenance is "it appeared in a form", and a
//! second typo a second one for an owner to merge later — the reason
//! [`CounterpartyLead::into_create`] is a convenience and not a write.
//!
//! **It never verifies anybody.** Writing to an address is not proof of who
//! holds it: confidence at send time is not proof of receipt, and proof of
//! receipt is not proof of ownership. Everything filed here is
//! [`crate::magician_v2::counterparties::Verification::Unverified`], and
//! [`mint_from_outbound`] carries a tripwire that refuses outright if a future
//! change ever lets provenance imply proof.
//!
//! # Generic first
//!
//! Nothing here names a programme. "We are about to write to an address as part
//! of some piece of work with some organisation" is the shape, and the work is a
//! supplier order, an application, a case, a hiring loop or a deal.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::magician_v2::counterparty_consumers::{
    mint_from_outbound, resolve_label, ChannelAddress, CounterpartyLead, LabelStanding,
    MintOutcome, Minted, OutboundEvidence,
};
use crate::magician_v2::counterparty_store::{CounterpartyScope, CounterpartyStore};
use crate::magician_v2::counterparty_types::{normalise_identity, CounterpartyRef};

/// **Who the flow says this address belongs to.**
///
/// Three arms rather than an `Option<String>`, because the three mean different
/// things and the difference decides whether anything gets written. An
/// `Option<String>` that was `None` would read as "no organisation" and get
/// handled with an `unwrap_or_default()`, which is a blank counterparty id — the
/// one thing the store guards hardest against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundOrganisation<'a> {
    /// The flow already resolved the organisation and holds its reference.
    Recorded(&'a CounterpartyRef),
    /// The flow carries an owner-typed organisation label. Matched through
    /// [`resolve_label`], which is exact on the register's own name fold and
    /// never guesses at a near miss.
    Label(&'a str),
    /// The flow names nobody. The register's answer **for this address** is
    /// then the only honest source; an address it does not hold files nothing,
    /// because there is no organisation to file it under and inventing one is
    /// this module's whole objection.
    FromRegister,
}

/// Why an outbound write was not filed.
///
/// **Never an error and never permission.** Both arms are ordinary states of a
/// register that has not been told something yet, and both leave the send path
/// free to send — this module records who we talk to, it does not decide
/// whether we may.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotFiled {
    /// The flow named a label nobody has recorded an organisation for. The lead
    /// carries the id [`CounterpartyStore::record_counterparty`] would file it
    /// under, so an owner surface can offer *"record this organisation"* and
    /// land on the row this read was already looking for.
    LabelUnregistered(CounterpartyLead),
    /// The flow named nobody and the register does not hold this address
    /// either, so there is no organisation to file it under. The normalised
    /// form is carried so an owner surface can show the address exactly as the
    /// register would compare it.
    NoOrganisationOnFile { normalised: String },
}

impl NotFiled {
    /// A stable label for logs and owner surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LabelUnregistered(_) => "label_unregistered",
            Self::NoOrganisationOnFile { .. } => "no_organisation_on_file",
        }
    }

    /// The lead, when a label named nobody.
    pub fn lead(&self) -> Option<&CounterpartyLead> {
        match self {
            Self::LabelUnregistered(lead) => Some(lead),
            Self::NoOrganisationOnFile { .. } => None,
        }
    }
}

/// What the register did with an outbound write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundRecord {
    /// The address is on file against an organisation — either because this
    /// call put it there, or because it was already there and was left exactly
    /// as it was.
    Filed(Minted),
    /// Nothing was written, and the reason is a fact an owner can act on.
    NotFiled(NotFiled),
}

impl OutboundRecord {
    /// The row on file, when there is one.
    pub fn filed(&self) -> Option<&Minted> {
        match self {
            Self::Filed(minted) => Some(minted),
            Self::NotFiled(_) => None,
        }
    }

    /// Whether **this call** is the one that put the address on file.
    ///
    /// `false` for an address that was already there. That distinction is the
    /// one worth logging: a register that never answers `true` is a register
    /// nothing is feeding, which is exactly how a store stays empty while every
    /// call site reports success.
    pub fn was_recorded(&self) -> bool {
        matches!(
            self,
            Self::Filed(Minted {
                outcome: MintOutcome::Recorded,
                ..
            })
        )
    }

    /// A stable label for logs and owner surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Filed(minted) => match minted.outcome {
                MintOutcome::Recorded => "recorded",
                MintOutcome::AlreadyOnFile => "already_on_file",
            },
            Self::NotFiled(why) => why.as_str(),
        }
    }
}

/// What a caller knows about the write itself, independent of who it is going to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundWrite<'a> {
    /// What to read to check the claim: the draft, the approval, the thread.
    pub evidence_ref: &'a str,
    /// Who or what is recording it. **The actor, not the owner** on an
    /// automatic path: the store compares this against a promotion's
    /// `decided_by` to refuse a guesser approving its own output, so naming the
    /// owner here from a path the owner did not personally drive would erase
    /// that check.
    pub recorded_by: &'a str,
    /// The identity that vouched, when somebody did. Naming one makes the
    /// provenance [`super::types::MintSource::Introduced`]; omitting one makes
    /// it [`super::types::MintSource::OwnerStated`]. A blank string is refused
    /// rather than downgraded.
    pub introduced_by: Option<&'a str>,
}

/// **Record where we are about to write.**
///
/// Resolves the organisation the caller named, then hands the whole thing to
/// [`mint_from_outbound`], which owns every refusal that matters: a domain is
/// not somewhere a message goes, an address already filed under a *different*
/// organisation is not re-filed under this one, and an identity that comes back
/// verified from a send path is a bug rather than a result.
///
/// # Idempotent, and it never relabels
///
/// A second write to the same address returns the row that was already there,
/// untouched. That matters beyond tidiness: the address may already be on file
/// as [`super::types::MintSource::ResearchInferred`], and re-minting it as
/// owner-stated would erase the fact that it was a guess — along with the two
/// guards the store puts on promoting a guess.
///
/// # Failures are failures, never absences
///
/// An unreadable register propagates as an error. It is not folded into
/// [`NotFiled`], because "the log could not be read" and "nobody has recorded
/// this organisation" are different answers and only the second is one an owner
/// acts on.
pub fn record_outbound_write(
    store: &CounterpartyStore,
    scope: &CounterpartyScope,
    channel: &str,
    address: &ChannelAddress,
    organisation: OutboundOrganisation<'_>,
    write: &OutboundWrite<'_>,
    now: DateTime<Utc>,
) -> Result<OutboundRecord> {
    let counterparty_id = match organisation {
        OutboundOrganisation::Recorded(reference) => reference.as_str().to_string(),
        OutboundOrganisation::Label(label) => match resolve_label(store, scope, label)? {
            LabelStanding::Registered(reference) => reference.as_str().to_string(),
            LabelStanding::Unregistered(lead) => {
                return Ok(OutboundRecord::NotFiled(NotFiled::LabelUnregistered(lead)));
            },
        },
        OutboundOrganisation::FromRegister => {
            match store.resolve(scope, address.kind, &address.value)? {
                Some(reference) => reference.as_str().to_string(),
                None => {
                    return Ok(OutboundRecord::NotFiled(NotFiled::NoOrganisationOnFile {
                        normalised: normalise_identity(address.kind, &address.value)?,
                    }));
                },
            }
        },
    };

    let minted = mint_from_outbound(
        store,
        scope,
        channel,
        address,
        &OutboundEvidence {
            counterparty_id,
            evidence_ref: write.evidence_ref.to_string(),
            recorded_by: write.recorded_by.to_string(),
            introduced_by: write.introduced_by.map(ToString::to_string),
        },
        now,
    )?;
    Ok(OutboundRecord::Filed(minted))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    use super::*;
    use crate::magician_v2::counterparty_store::counterparty_id_for;
    use crate::magician_v2::counterparty_types::{
        AddIdentity, CreateCounterparty, IdentityKind, MintSource, Promotion, TrustedSignal,
    };

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 21, 9, 0, 0).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, CounterpartyStore, CounterpartyScope) {
        let tmp = tempfile::tempdir().expect("temp dir");
        let store = CounterpartyStore::new(ArtifactV2Workspace::new(tmp.path()));
        (tmp, store, CounterpartyScope::new("alpha", "prod"))
    }

    fn write() -> OutboundWrite<'static> {
        OutboundWrite {
            evidence_ref: "outbox-1",
            recorded_by: "company-assistant",
            introduced_by: None,
        }
    }

    /// One organisation with one address a server-trusted signal has proved.
    fn acme(store: &CounterpartyStore, scope: &CounterpartyScope) -> String {
        let recorded = store
            .record_counterparty(
                scope,
                &CreateCounterparty {
                    display_name: "Acme Ltd".to_string(),
                    domain: None,
                    stage: None,
                    created_by: "owner".to_string(),
                },
                now(),
            )
            .expect("record counterparty");
        let identity = store
            .add_identity(
                scope,
                &AddIdentity {
                    counterparty_id: recorded.counterparty_id.clone(),
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
        store
            .promote_identity(
                scope,
                &identity.identity_id,
                &Promotion {
                    decided_by: "owner".to_string(),
                    evidence_ref: "receipt-1".to_string(),
                    signal: TrustedSignal::new("web", "receipt-1").authenticated(true),
                },
                now(),
            )
            .expect("promote phone");
        recorded.counterparty_id
    }

    /// **Writing to somebody never verifies them.**
    ///
    /// Pins the escalation a send path would otherwise perform on itself: the
    /// organisation already has a *proved* address, so an implementation that
    /// inherited verification from the counterparty — or that read "we chose to
    /// send this" as proof of who holds the address — would hand this new,
    /// never-confirmed address the authority an owner granted a different one.
    /// The failure is invisible downstream, because
    /// `verified_identities_for_label` would then hand it to every envelope and
    /// room roster as a proved address.
    #[test]
    fn a_first_outbound_write_files_the_address_unverified() {
        let (_tmp, store, scope) = fixture();
        let acme_id = acme(&store, &scope);
        assert_eq!(
            store
                .verified_identities_for(&scope, &acme_id)
                .expect("verified")
                .len(),
            1,
            "the fixture must already hold a proved address, or this proves nothing"
        );

        let record = record_outbound_write(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "Ops@Acme.com"),
            OutboundOrganisation::Label("Acme Ltd"),
            &write(),
            now(),
        )
        .expect("record the write");

        assert!(
            record.was_recorded(),
            "the first write must file the address"
        );
        assert_eq!(record.as_str(), "recorded");
        let minted = record.filed().expect("filed");
        assert_eq!(minted.identity.normalised, "ops@acme.com");
        assert_eq!(minted.identity.counterparty_id, acme_id);
        assert_eq!(minted.identity.minted_by.source, MintSource::OwnerStated);
        assert_eq!(minted.identity.minted_by.recorded_by, "company-assistant");
        assert!(
            !minted.identity.is_verified(),
            "an outbound write must never produce a verified identity"
        );
        assert_eq!(
            store
                .verified_identities_for(&scope, &acme_id)
                .expect("verified")
                .len(),
            1,
            "the proved set must be exactly what the owner proved, and no larger"
        );
    }

    /// **A second write never relabels a guess as an owner statement.**
    ///
    /// Pins the laundering path: research files an address as a guess, the send
    /// path writes to it, and an implementation that re-minted it would replace
    /// `research_inferred` with `owner_stated` — erasing both guards the store
    /// puts on promoting a guess (a research note may not be its own proof, and
    /// the researcher may not approve its own output). Nothing downstream could
    /// tell, because the row would read exactly like one an owner had stated.
    #[test]
    fn a_repeat_write_leaves_a_researched_address_exactly_as_it_was() {
        let (_tmp, store, scope) = fixture();
        let acme_id = acme(&store, &scope);
        let guessed = store
            .add_identity(
                &scope,
                &AddIdentity {
                    counterparty_id: acme_id.clone(),
                    kind: IdentityKind::Email,
                    value: "ops@acme.com".to_string(),
                    source: MintSource::ResearchInferred,
                    evidence_ref: "research-note-7".to_string(),
                    recorded_by: "research-agent".to_string(),
                    introduced_by: None,
                },
                now(),
            )
            .expect("file the guess");
        assert_eq!(guessed.minted_by.source, MintSource::ResearchInferred);

        let record = record_outbound_write(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            OutboundOrganisation::Label("Acme Ltd"),
            &write(),
            now(),
        )
        .expect("record the write");

        assert!(!record.was_recorded());
        assert_eq!(record.as_str(), "already_on_file");
        let minted = record.filed().expect("filed");
        assert_eq!(
            minted.identity.minted_by.source,
            MintSource::ResearchInferred,
            "the send path relabelled a guess as an owner statement"
        );
        assert_eq!(minted.identity.minted_by.recorded_by, "research-agent");
        assert_eq!(minted.identity.minted_by.evidence_ref, "research-note-7");
    }

    /// **A label nobody recorded writes nothing, and says so as a lead.**
    ///
    /// Pins the register growing itself: a send path that created an
    /// organisation from whatever string it carried would make one typo a
    /// permanent counterparty and a second typo a second one, and an owner would
    /// be asked to merge names they never typed. The lead's id must be the one
    /// `record_counterparty` will use, or the owner's follow-through lands on a
    /// different row than the read was looking for.
    #[test]
    fn an_unregistered_label_files_nothing_and_names_the_row_it_would_file() {
        let (_tmp, store, scope) = fixture();
        acme(&store, &scope);

        let record = record_outbound_write(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme-holdings.com"),
            OutboundOrganisation::Label("Acme Holdings"),
            &write(),
            now(),
        )
        .expect("record the write");

        assert_eq!(record.as_str(), "label_unregistered");
        assert!(!record.was_recorded());
        let lead = match &record {
            OutboundRecord::NotFiled(why) => why.lead().expect("a lead"),
            OutboundRecord::Filed(_) => panic!("an unregistered label must file nothing"),
        };
        assert_eq!(lead.label, "Acme Holdings");
        assert_eq!(
            lead.would_be_counterparty_id,
            counterparty_id_for(&scope, "Acme Holdings").expect("derive")
        );
        assert_eq!(
            store.list(&scope).expect("list").len(),
            1,
            "the register grew an organisation from a label"
        );
        assert!(store
            .resolve(&scope, IdentityKind::Email, "ops@acme-holdings.com")
            .expect("resolve")
            .is_none());
    }

    /// **Naming nobody, for an address the register does not hold, files
    /// nothing.**
    ///
    /// Pins the tempting shortcut for a flow that has only an address: creating
    /// an organisation named after the address. That row would then be the
    /// organisation, and the day an owner records the real company the register
    /// would refuse to file the address under it — leaving a merge nobody asked
    /// for as the only way out.
    #[test]
    fn naming_nobody_for_an_unknown_address_files_nothing() {
        let (_tmp, store, scope) = fixture();
        acme(&store, &scope);

        let record = record_outbound_write(
            &store,
            &scope,
            "whatsapp",
            &ChannelAddress::new(IdentityKind::Phone, "+15559999999"),
            OutboundOrganisation::FromRegister,
            &write(),
            now(),
        )
        .expect("record the write");

        assert_eq!(record.as_str(), "no_organisation_on_file");
        assert_eq!(
            record,
            OutboundRecord::NotFiled(NotFiled::NoOrganisationOnFile {
                normalised: "+15559999999".to_string()
            })
        );
        assert_eq!(store.list(&scope).expect("list").len(), 1);
    }

    /// **Naming nobody, for an address the register does hold, resumes on the
    /// organisation that holds it.**
    ///
    /// Pins the opposite failure to the test above — a `FromRegister` arm so
    /// cautious it never answers. The address IS on file here, so the call must
    /// come back `already_on_file` naming the real organisation rather than
    /// reporting it as unknown.
    #[test]
    fn naming_nobody_for_a_known_address_resumes_on_its_organisation() {
        let (_tmp, store, scope) = fixture();
        let acme_id = acme(&store, &scope);

        let record = record_outbound_write(
            &store,
            &scope,
            "whatsapp",
            &ChannelAddress::new(IdentityKind::Phone, "+1 (555) 000-1111"),
            OutboundOrganisation::FromRegister,
            &write(),
            now(),
        )
        .expect("record the write");

        assert_eq!(record.as_str(), "already_on_file");
        let minted = record.filed().expect("filed");
        assert_eq!(minted.identity.counterparty_id, acme_id);
        assert_eq!(minted.identity.normalised, "+15550001111");
        assert_eq!(minted.channel, "whatsapp");
    }

    /// **One address belongs to one organisation.**
    ///
    /// Pins the ambiguous register: a second row for one address makes
    /// `resolve` answer with whichever read wins, and an ambiguous resolve is
    /// one counterparty's authority landing on another. The refusal must be an
    /// error the caller sees, not a quiet second row.
    #[test]
    fn writing_to_another_organisations_address_is_refused() {
        let (_tmp, store, scope) = fixture();
        acme(&store, &scope);
        let rival = store
            .record_counterparty(
                &scope,
                &CreateCounterparty {
                    display_name: "Rival GmbH".to_string(),
                    domain: None,
                    stage: None,
                    created_by: "owner".to_string(),
                },
                now(),
            )
            .expect("record rival");

        let error = record_outbound_write(
            &store,
            &scope,
            "whatsapp",
            &ChannelAddress::new(IdentityKind::Phone, "+15550001111"),
            OutboundOrganisation::Recorded(&CounterpartyRef::new(rival.counterparty_id.clone())),
            &write(),
            now(),
        )
        .expect_err("filing one address under two organisations must be refused");
        assert!(
            error
                .to_string()
                .contains("already filed under counterparty"),
            "unexpected refusal: {error}"
        );

        let resolved = store
            .resolve(&scope, IdentityKind::Phone, "+15550001111")
            .expect("resolve")
            .expect("still on file");
        assert_ne!(
            resolved.as_str(),
            rival.counterparty_id,
            "the refused write moved an address to another organisation"
        );
    }

    /// **A channel carrying the id separator is refused before anything is
    /// looked up.**
    ///
    /// Pins the fusing input: U+001F is what keeps a derived id's components
    /// from bleeding into each other, and a channel carrying it in an audit line
    /// makes two different acts read as one.
    #[test]
    fn a_channel_carrying_the_field_separator_is_refused() {
        let (_tmp, store, scope) = fixture();
        acme(&store, &scope);

        let error = record_outbound_write(
            &store,
            &scope,
            "what\u{1f}sapp",
            &ChannelAddress::new(IdentityKind::Email, "ops@acme.com"),
            OutboundOrganisation::Label("Acme Ltd"),
            &write(),
            now(),
        )
        .expect_err("a separator-carrying channel must be refused");
        assert!(
            error.to_string().contains("U+001F"),
            "unexpected refusal: {error}"
        );
        assert!(
            store
                .resolve(&scope, IdentityKind::Email, "ops@acme.com")
                .expect("resolve")
                .is_none(),
            "a refused write still filed the address"
        );
    }

    /// **A domain is not somewhere a message goes.**
    ///
    /// Pins a send path recording an affiliation hint as an outbound write,
    /// which asserts a send that never happened — and puts a row in the register
    /// that reads, to every later owner review, exactly like an address we have
    /// actually corresponded with.
    #[test]
    fn a_domain_cannot_be_minted_from_an_outbound_write() {
        let (_tmp, store, scope) = fixture();
        acme(&store, &scope);

        let error = record_outbound_write(
            &store,
            &scope,
            "email",
            &ChannelAddress::new(IdentityKind::Domain, "acme.com"),
            OutboundOrganisation::Label("Acme Ltd"),
            &write(),
            now(),
        )
        .expect_err("a domain must not be minted from an outbound write");
        assert!(
            error.to_string().contains("a domain is not somewhere"),
            "unexpected refusal: {error}"
        );
    }
}
