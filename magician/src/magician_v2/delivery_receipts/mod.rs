//! Bounces do not only arrive by webhook. They come back **as mail**.
//!
//! # The gap this closes
//!
//! `delivery::intake::ReceiptIntake::admit` is the one normalised door a
//! receipt comes through, and `delivery::DeliveryLedger` is the contract behind
//! it. Both are complete. What neither has is a **producer**: no provider wired
//! into this deployment emits a delivery event stream, and no skill in
//! `skillshub` exposes a bounce, complaint, webhook or delivery-status surface
//! — `agentmail-send`, `agentmail-read` and `agentmail-processing` were checked
//! and carry none. So the door exists and, for email, nothing walks through it
//! except a person typing what they read.
//!
//! There is one channel that does report, has always reported, and needs no
//! provider cooperation at all: when a message cannot be delivered, the
//! receiving MTA sends a **Delivery Status Notification** back to the sending
//! mailbox. It lands in the same inbox `agentmail-read::messages_get` already
//! reads. This module turns that mail into receipts.
//!
//! # What this module is, exactly
//!
//! A reader and a correlator. It hands back **intent** — `(act_ref,
//! DeliveryReceipt)` pairs in precisely the shape
//! [`ReceiptIntake::admit`](crate::magician_v2::delivery::intake::ReceiptIntake::admit)
//! and [`DeliveryLedger::reconcile`](crate::magician_v2::delivery::DeliveryLedger::reconcile)
//! take — and writes nothing to the ledger itself, the same split
//! `delivery_hygiene::signals_since` makes for the suppression register. The
//! order, the terminal states, the one-message-one-identity rule and the
//! act-must-have-left check all stay where they already are, decided once.
//!
//! # Provider-agnostic, because a DSN is not an AgentMail artefact
//!
//! Nothing here calls AgentMail, or any provider. [`dsn::read_dsn`] takes raw
//! RFC 5322 bytes. Those bytes can come from an inbox reader, an SMTP adapter's
//! bounce mailbox, a Kapso rail that one day emits RFC 3464, or a human
//! forwarding a bounce out of their own client — the parse is identical, and a
//! forwarded bounce is walked into because for most of this system's life it is
//! the only bounce anybody will hand it. The provider name is a **parameter**.
//!
//! # Correlation, and why it is the dangerous part
//!
//! A bounce is worthless unless it can be tied to the act that caused it, and
//! tying it to the *wrong* act is worse than tying it to none: a hard bounce
//! becomes `SuppressionReason::HardBounce` in the register, which only an
//! explicit owner act carrying evidence can ever lift. A misattribution is a
//! person we can never write to again.
//!
//! So there are exactly two admissible correlations, and both rest on an
//! identifier **we minted**:
//!
//! 1. [`Correlation::EnvelopeId`] — RFC 3461 `Original-Envelope-Id`, echoed back
//!    from the envelope id the sender set at submission. Strongest: it names one
//!    submission and cannot be reused.
//! 2. [`Correlation::ReturnedMessageId`] — the original `Message-ID` read out of
//!    the headers the reporting MTA returned inside the DSN. Strong: it is an id
//!    we generated, and the DSN quoted it back.
//!
//! **Recipient-address matching alone is deliberately not implemented.** One
//! address receives many acts; a bounce matched on the address alone would be
//! attributed to whichever act a heuristic liked, and a bounce for last month's
//! newsletter would land on this morning's contract disclosure. When neither
//! identifier is present, or the identifier names no send this scope recorded,
//! **nothing is recorded** and the case comes back as a
//! [`CorrelationRefusal`] for a human to look at. A gap in coverage is
//! recoverable; a wrong suppression is not.
//!
//! # Whose address the receipt is about
//!
//! The identity recorded is the address **we** wrote to, never a downstream one.
//! When a forwarding chain relays our message onward and the far end dies, the
//! DSN's `Final-Recipient` is somebody we have never written to and its
//! `Original-Recipient` is our addressee. So membership of the send's recorded
//! audience decides: `Original-Recipient` if it is in the audience, else
//! `Final-Recipient` if it is, else refuse. A receipt filed against an address
//! the act never targeted is the misattribution above wearing a different hat.
//!
//! # Fail closed
//!
//! Every one of these produces **no receipt** and a named refusal:
//!
//! - the mail is not structurally a DSN (see [`dsn`] — a subject line is never
//!   evidence);
//! - it announces itself as a delivery report and cannot be parsed;
//! - `Status:` is absent or not an RFC 3463 class of 2, 4 or 5;
//! - `Action:` and `Status:` contradict each other;
//! - the report carries no identifier we minted;
//! - the identifier names a send this scope has no record of;
//! - one identifier maps to more than one act;
//! - the reported recipient is not in the send's audience;
//! - no clock in the report can be parsed.
//!
//! A `4.x.x` status never suppresses anybody: it maps to
//! `DeliveryState::Bounced { hard: false }`, which the delivery module gives no
//! suppression cause, by its own design.
//!
//! # Counts, never rates
//!
//! [`DsnHarvest`] carries `recipients_reported` beside the intents and the
//! refusals. There is no `correlation_rate`: "80% correlated" over five bounces
//! and over five thousand are different facts, and the first is what a broken
//! send-side index looks like in its first hour.
//!
//! # What this can and cannot see
//!
//! It sees hard bounces and transient failures that a conforming MTA reports,
//! plus delay notices and — where a sender requested them — success
//! notifications. It is **blind** to silent drops, to spam foldering, and to
//! complaints, because a complaint arrives through a feedback loop this
//! deployment has not subscribed to and never reaches the mailbox at all. The
//! honest reading is in the module's own docs and in the notes handed back with
//! this work; it is not a substitute for a provider event stream, it is the
//! half of one that needs nobody's cooperation.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::magician_v2::delivery::{normalise_identity, DeliveryReceipt, DeliveryState};

pub mod dsn;
mod guard;
/// The one file here that records anything, and it records it through the
/// intake door rather than into the ledger.
pub mod ingest;
/// The bridge's first production mailbox: a directory of `.eml` files. Chosen
/// because every way a bounce reaches a host ends in something that can write a
/// file, so it commits to no provider — which is the reason `pull` gave for
/// leaving the port open, honoured without leaving it EMPTY.
pub mod maildir;
pub mod migration;
/// The standing bridge that reads a bounce mailbox on the delivery-hygiene
/// tick and hands what it finds to the intake door. It implements the
/// rail-agnostic `delivery_hygiene::receipts::ReceiptPuller` port — specific
/// imports generic, so the sweep never learns what RFC 3464 is.
pub mod pull;
pub mod sent_index;
mod sqlite;
pub mod store;

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod storage_packet;
#[cfg(test)]
mod tests;

pub use dsn::{read_dsn, DsnRecognition, DsnReport, ReadingFault, RecipientReport};
pub use ingest::{admit_mail, read_mail, MailIngest, MailReading};
pub use maildir::MaildirBounceMailbox;
pub use migration::SentIndexMigrationHandler;
pub use sent_index::{SendBinding, SentIndexRecord, SentMessage, SentMessageIndex};
pub use store::{open_local_sent_index, open_sqlite_sent_index, SentMessageStore};

/// Field separator for derived ids across this codebase. A caller string that
/// feeds one is refused if it holds this character, here as everywhere else.
const FIELD_SEP: char = '\u{1f}';

/// The longest anchor accepted from a hostile `Message-ID`.
///
/// The anchor becomes a provider message id, which becomes part of a derived
/// receipt id. A mail header is attacker-controlled and unbounded; this is not.
const MAX_ANCHOR: usize = 200;

/// An identifier **we** minted that a report can quote back at us.
///
/// Two namespaces, one store. A value is never compared across namespaces: an
/// envelope id that happens to read like a message id is a different fact, and
/// a lookup that crossed them could attribute a bounce to a send that merely
/// shared a string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum SendIdentifier {
    /// The RFC 5322 `Message-ID` of the message we sent, without its angle
    /// brackets.
    MessageId(String),
    /// The RFC 3461 envelope id set at submission.
    EnvelopeId(String),
}

impl SendIdentifier {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MessageId(_) => "message_id",
            Self::EnvelopeId(_) => "envelope_id",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Self::MessageId(value) | Self::EnvelopeId(value) => value.as_str(),
        }
    }
}

/// How a bounce was tied back to the act that caused it.
///
/// Both arms rest on an identifier this system generated. There is no arm for
/// "matched on the recipient address", and its absence is the point — see the
/// module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Correlation {
    /// The report echoed the RFC 3461 envelope id set at submission.
    EnvelopeId,
    /// The report returned our original headers and the `Message-ID` in them is
    /// bound to exactly one act.
    ReturnedMessageId,
}

impl Correlation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnvelopeId => "envelope_id",
            Self::ReturnedMessageId => "returned_message_id",
        }
    }

    /// How much this correlation is worth, said out loud.
    ///
    /// Both are strong, and they are not equally strong. An envelope id names
    /// one submission; a `Message-ID` names one composed message, which a
    /// resend could in principle reuse. The distinction is carried so a review
    /// of a disputed suppression can see which one was leaned on.
    pub fn confidence(self) -> &'static str {
        match self {
            Self::EnvelopeId => {
                "strong: an envelope id names exactly one submission and is set \
                                 by the sender at submission time"
            },
            Self::ReturnedMessageId => {
                "strong: the reporting MTA quoted back the Message-ID we \
                                        generated for the original message"
            },
        }
    }
}

/// A receipt, and the act it belongs to. **Intent, not a record.**
///
/// Exactly the two arguments
/// [`ReceiptIntake::admit`](crate::magician_v2::delivery::intake::ReceiptIntake::admit)
/// wants, so the caller passes them straight through and nothing here decides
/// what the ledger will do with them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptIntent {
    pub act_ref: String,
    pub receipt: DeliveryReceipt,
    pub correlation: Correlation,
    /// The reporting MTA's own words, for the audit trail. Never parsed — see
    /// [`dsn::classify`] for why a status class is read from `Status:` alone.
    pub diagnostic_code: Option<String>,
}

/// Why one reported recipient produced no receipt.
///
/// Returned, never dropped and never softened into a receipt. Each of these is
/// a case where recording something plausible would be recording something that
/// suppresses the wrong person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// The report quoted no identifier we minted — no envelope id, and no
    /// returned `Message-ID`.
    NoReturnedIdentifier,
    /// The identifier is real and this scope has no record of sending it.
    UnknownSend,
    /// The identifier maps to more than one act. The index is inconsistent and
    /// picking one would be picking whose address to suppress.
    AmbiguousSend,
    /// The reported address is not in the send's recorded audience.
    RecipientNotInSend,
    /// The address could not be normalised into an identity at all.
    UnusableIdentity,
    /// The report's `Action:` token is not one this build knows.
    UnknownAction,
    /// `Status:` absent, or not an RFC 3463 class of 2, 4 or 5.
    UnreadableStatus,
    /// `Action:` and `Status:` disagree.
    ContradictoryReport,
    /// The block named no recipient at all.
    NoRecipient,
    /// No `Last-Attempt-Date`, `Arrival-Date` or `Date` could be parsed, so
    /// there is no provider clock to record. Substituting our own would break
    /// the ledger's replay contract and turn a re-post of the same bounce into
    /// an error.
    NoObservedAt,
}

impl RefusalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoReturnedIdentifier => "no_returned_identifier",
            Self::UnknownSend => "unknown_send",
            Self::AmbiguousSend => "ambiguous_send",
            Self::RecipientNotInSend => "recipient_not_in_send",
            Self::UnusableIdentity => "unusable_identity",
            Self::UnknownAction => "unknown_action",
            Self::UnreadableStatus => "unreadable_status",
            Self::ContradictoryReport => "contradictory_report",
            Self::NoRecipient => "no_recipient",
            Self::NoObservedAt => "no_observed_at",
        }
    }
}

/// One recipient the reader saw and refused to turn into a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrelationRefusal {
    pub reason: RefusalReason,
    /// The sentence a human reads. Says what was looked for, not just what was
    /// missing, because the person reading it is deciding whether to record the
    /// bounce by hand.
    pub detail: String,
    /// Everything the report said about who it concerns, carried verbatim so an
    /// operator can act on a refusal without going back to the raw mail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_recipient: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_recipient: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// What one delivery status notification yielded.
///
/// Counts, never rates. `recipients_reported` is carried beside the two lists
/// so `intents: 0` over a report about nobody and `intents: 0` over a report
/// about nine people cannot render alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnHarvest {
    /// The notification's own `Message-ID`, when it had one.
    pub report_message_id: Option<String>,
    /// The identifier the correlation was made on, when one was.
    pub correlated_on: Option<SendIdentifier>,
    /// How many per-recipient blocks the report contained.
    pub recipients_reported: usize,
    /// Receipts ready to go through the intake door. Never written here.
    pub intents: Vec<ReceiptIntent>,
    /// Everything that produced no receipt, and why.
    pub refused: Vec<CorrelationRefusal>,
}

impl DsnHarvest {
    /// Whether this report produced nothing at all.
    ///
    /// Not "whether it was fine". A report that produced nothing because every
    /// recipient was refused is the opposite of fine, which is why the refusals
    /// are a list rather than a flag.
    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }
}

/// Every identifier this report quotes back at us, for a lookup.
///
/// Envelope id first: it is the stronger of the two, so a caller that resolves
/// them in order resolves the strongest available correlation first.
pub fn identifiers_in(report: &DsnReport) -> Vec<SendIdentifier> {
    let mut out = Vec::new();
    if let Some(envelope) = report
        .original_envelope_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        out.push(SendIdentifier::EnvelopeId(envelope.to_string()));
    }
    if let Some(message) = report
        .original_message_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        out.push(SendIdentifier::MessageId(message.to_string()));
    }
    out
}

/// Turn one parsed report into receipts, against a **supplied** view of what
/// this scope sent.
///
/// `sends` is supplied, never discovered — the same rule
/// [`DispatchedAct`](crate::magician_v2::delivery::DispatchedAct) states and for
/// the same reason. [`sent_index::sends_for`] is *a* source of that map and not
/// *the* source: a rail whose sends are recorded elsewhere supplies its own and
/// this file is not touched.
///
/// # What comes back as `Err`, and what does not
///
/// `Err` is reserved for the **caller** getting it wrong: a blank provider name,
/// a payload ref carrying the id separator. Nothing in the *mail* can produce an
/// `Err`, because the mail arrives from outside and a hostile bounce must not be
/// able to turn a door into a 500. Every content fault is a
/// [`CorrelationRefusal`] instead — visible, named, and carrying no receipt.
pub fn correlate(
    provider: &str,
    payload_ref: &str,
    report: &DsnReport,
    sends: &BTreeMap<SendIdentifier, SendBinding>,
) -> Result<DsnHarvest> {
    let provider = caller_field(provider, "a provider name")?;
    let payload_ref = caller_field(payload_ref, "a payload ref")?;

    let mut harvest = DsnHarvest {
        report_message_id: report.report_message_id.clone(),
        correlated_on: None,
        recipients_reported: report.recipients.len(),
        intents: Vec::new(),
        refused: Vec::new(),
    };

    // The identifier is a property of the message, not of a recipient, so it is
    // resolved once. Envelope id wins where both are present: it names one
    // submission and a Message-ID names one composed message.
    let identifiers = identifiers_in(report);
    let mut resolved: Option<(SendIdentifier, &SentMessage)> = None;
    let mut ambiguous: Option<(SendIdentifier, Vec<String>)> = None;
    for identifier in &identifiers {
        match sends.get(identifier) {
            Some(SendBinding::Bound(send)) => {
                resolved = Some((identifier.clone(), send));
                break;
            },
            Some(SendBinding::Ambiguous { act_refs }) => {
                ambiguous = Some((identifier.clone(), act_refs.clone()));
            },
            Some(SendBinding::Absent) | None => {},
        }
    }

    let Some((identifier, send)) = resolved else {
        let refusal = if let Some((identifier, act_refs)) = ambiguous {
            (
                RefusalReason::AmbiguousSend,
                format!(
                    "the report quotes {} `{}`, and the send index holds more than one account \
                     of it (acts named: {}). Two accounts of one identifier mean somebody \
                     upstream reused it or two writers raced, and choosing between them would \
                     choose whose address a hard bounce suppresses — so nothing is recorded and \
                     this needs a human",
                    identifier.kind(),
                    identifier.value(),
                    act_refs.join(", ")
                ),
            )
        } else if identifiers.is_empty() {
            (
                RefusalReason::NoReturnedIdentifier,
                "the report quotes no identifier this system minted: it carries neither an RFC \
                 3461 `Original-Envelope-Id` nor a returned original `Message-ID`. Matching it \
                 to a send on the recipient address alone is deliberately not implemented — one \
                 address receives many acts, and a bounce attributed to the wrong one \
                 permanently suppresses somebody on evidence about a different message. Record \
                 it by hand through the receipt intake if you can establish which send it was"
                    .to_string(),
            )
        } else {
            (
                RefusalReason::UnknownSend,
                format!(
                    "the report quotes {}, and this scope has no record of sending any of them. \
                     Either the send was never registered against its identifier, or this bounce \
                     belongs to another scope or another system. Nothing is recorded: a receipt \
                     needs an act, and inventing one would suppress an address on the strength \
                     of a send that does not exist here",
                    identifiers
                        .iter()
                        .map(|id| format!("{} `{}`", id.kind(), id.value()))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ),
            )
        };
        // One refusal per reported recipient, so a report about four people does
        // not collapse into a single line an operator reads as one problem.
        for recipient in &report.recipients {
            harvest
                .refused
                .push(refuse(recipient, refusal.0, refusal.1.clone()));
        }
        if report.recipients.is_empty() {
            harvest.refused.push(CorrelationRefusal {
                reason: refusal.0,
                detail: refusal.1,
                final_recipient: None,
                original_recipient: None,
                action: None,
                status: None,
            });
        }
        return Ok(harvest);
    };

    let correlation = match identifier {
        SendIdentifier::EnvelopeId(_) => Correlation::EnvelopeId,
        SendIdentifier::MessageId(_) => Correlation::ReturnedMessageId,
    };
    harvest.correlated_on = Some(identifier);

    // The anchor for idempotency: the notification's own Message-ID when it has
    // a usable one, else a fingerprint of the mail. Either way the same bounce
    // posted twice derives the same provider message id and the ledger resumes
    // its record instead of writing a second one.
    let anchor = report
        .report_message_id
        .as_deref()
        .map(str::trim)
        .filter(|value| is_safe_anchor(value))
        .unwrap_or(report.fingerprint.as_str())
        .to_string();

    let audience: BTreeSet<String> = send.audience.iter().cloned().collect();

    for recipient in &report.recipients {
        let state = match dsn::classify(recipient.action.as_deref(), recipient.status.as_deref()) {
            Ok(state) => state,
            Err(fault) => {
                harvest
                    .refused
                    .push(refuse(recipient, reason_for(&fault), fault.explain()));
                continue;
            },
        };

        let identity = match resolve_identity(recipient, &audience) {
            Ok(identity) => identity,
            Err((reason, detail)) => {
                harvest.refused.push(refuse(recipient, reason, detail));
                continue;
            },
        };

        // The provider's clock, in the order the report offers it. Never our
        // own: `observed_at` is part of what the ledger compares on a replay, so
        // substituting `Utc::now()` would make the same bounce posted twice a
        // "changed payload" error instead of a resumption.
        let Some(observed_at) = recipient
            .last_attempt
            .or(report.arrival_date)
            .or(report.report_date)
        else {
            harvest.refused.push(refuse(
                recipient,
                RefusalReason::NoObservedAt,
                "the report carries no parseable `Last-Attempt-Date`, `Arrival-Date` or `Date`, \
                 so there is no provider clock to record against this receipt. Our own clock is \
                 deliberately not substituted: `observed_at` is part of what the ledger compares \
                 when the same receipt arrives twice, so an invented one would turn a harmless \
                 re-post of this bounce into a hard error"
                    .to_string(),
            ));
            continue;
        };

        harvest.intents.push(ReceiptIntent {
            act_ref: send.act_ref.clone(),
            receipt: DeliveryReceipt {
                provider: provider.clone(),
                // Anchor and identity together: one DSN reporting two
                // recipients is two provider messages, because the ledger holds
                // one provider message to one identity's story and would refuse
                // the second under a shared id.
                provider_message_id: format!("dsn:{anchor}:{identity}"),
                identity,
                state,
                observed_at,
                payload_ref: payload_ref.clone(),
            },
            correlation,
            diagnostic_code: recipient.diagnostic_code.clone(),
        });
    }

    Ok(harvest)
}

/// Which address the receipt is about: the one **we** wrote to.
///
/// `Original-Recipient` first, because that is the address as we addressed it
/// when a forwarding chain moved the message on. Both are checked for
/// membership of the send's audience, and a report about somebody the act never
/// targeted is refused rather than filed against the nearest plausible person.
fn resolve_identity(
    recipient: &RecipientReport,
    audience: &BTreeSet<String>,
) -> Result<String, (RefusalReason, String)> {
    let original = recipient
        .original_recipient
        .as_deref()
        .map(normalise_identity)
        .transpose();
    let final_recipient = recipient
        .final_recipient
        .as_deref()
        .map(normalise_identity)
        .transpose();

    let (original, final_recipient) =
        match (original, final_recipient) {
            (Ok(original), Ok(final_recipient)) => (original, final_recipient),
            _ => return Err((
                RefusalReason::UnusableIdentity,
                "the report's recipient address could not be read as an identity — it is blank, \
                 or carries control characters, which means it was mis-parsed somewhere upstream. \
                 Nothing is recorded, because an identity nobody can compare is an identity that \
                 could match anybody"
                    .to_string(),
            )),
        };

    if original.is_none() && final_recipient.is_none() {
        return Err((
            RefusalReason::NoRecipient,
            "the per-recipient block names no `Final-Recipient:` and no `Original-Recipient:` in \
             a form this reader accepts (an `rfc822;` address), so there is nobody the receipt \
             could be about"
                .to_string(),
        ));
    }

    for candidate in [original.as_ref(), final_recipient.as_ref()]
        .into_iter()
        .flatten()
    {
        if audience.contains(candidate) {
            return Ok(candidate.clone());
        }
    }

    Err((
        RefusalReason::RecipientNotInSend,
        format!(
            "the report is about {}, and the send it quotes went to {}. The address is not one \
             this act wrote to, so filing the bounce against it would suppress somebody on \
             evidence about a message they were never sent — and a hard bounce can only be \
             lifted by an explicit owner act. If the far end is a forwarding target, the report \
             carried no `Original-Recipient:` naming our addressee and there is nothing honest \
             to attribute it to",
            describe_pair(original.as_deref(), final_recipient.as_deref()),
            if audience.is_empty() {
                "nobody this scope has recorded".to_string()
            } else {
                audience.iter().cloned().collect::<Vec<_>>().join(", ")
            }
        ),
    ))
}

fn describe_pair(original: Option<&str>, final_recipient: Option<&str>) -> String {
    match (original, final_recipient) {
        (Some(original), Some(final_recipient)) if original != final_recipient => {
            format!("`{final_recipient}` (originally addressed to `{original}`)")
        },
        (Some(value), _) | (None, Some(value)) => format!("`{value}`"),
        (None, None) => "an address it did not name".to_string(),
    }
}

fn reason_for(fault: &ReadingFault) -> RefusalReason {
    // Exhaustive with no catch-all: a fault added to `dsn` later is a compile
    // error here rather than a case that silently inherits a neighbour's meaning.
    match fault {
        ReadingFault::UnknownAction { .. } => RefusalReason::UnknownAction,
        ReadingFault::UnreadableStatus { .. } => RefusalReason::UnreadableStatus,
        ReadingFault::ContradictoryReport { .. } => RefusalReason::ContradictoryReport,
        ReadingFault::NoRecipient => RefusalReason::NoRecipient,
    }
}

fn refuse(
    recipient: &RecipientReport,
    reason: RefusalReason,
    detail: String,
) -> CorrelationRefusal {
    CorrelationRefusal {
        reason,
        detail,
        final_recipient: recipient.final_recipient.clone(),
        original_recipient: recipient.original_recipient.clone(),
        action: recipient.action.clone(),
        status: recipient.status.clone(),
    }
}

/// Whether a value read out of a mail header may become part of a derived id.
///
/// A `Message-ID` is attacker-controlled: it arrives from a remote MTA and
/// nothing constrains it. It is refused if it is blank, longer than
/// [`MAX_ANCHOR`], carries the id separator, or carries any control character
/// or whitespace — which would shear a derived id, tear the append-only log
/// line in two, or shift a component boundary so a crafted bounce resumed
/// another act's record. When it is refused the caller falls back to a content
/// fingerprint, which is derived here and cannot be steered.
fn is_safe_anchor(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ANCHOR
        && !value.contains(FIELD_SEP)
        && !value.chars().any(|character| {
            character.is_control() || character.is_whitespace() || character == ':'
        })
}

/// A caller-supplied string that feeds an id derivation, checked and trimmed.
///
/// The same rule the delivery ledger applies at its own boundary, applied again
/// here so a refusal names *this* module's parameter rather than surfacing as a
/// ledger error three layers down.
fn caller_field(value: &str, what: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{what} is required: a receipt that names nothing reconciles nothing");
    }
    if trimmed.contains(FIELD_SEP) {
        anyhow::bail!(
            "{what} must not contain U+001F: it is the separator that keeps a receipt id's \
             components from bleeding into each other"
        );
    }
    if trimmed.chars().any(char::is_control) {
        anyhow::bail!(
            "{what} must not contain control characters: one arriving here means the value was \
             mis-parsed upstream, and a newline would tear the log line in two"
        );
    }
    Ok(trimmed.to_string())
}

/// The states this reader can ever produce, for a surface that wants to say so.
///
/// Deliberately short, and the omissions are the honest part: email's return
/// channel cannot report a complaint (that needs a feedback loop this
/// deployment has not subscribed to) and cannot report a silent drop or a spam
/// foldering at all, because nothing comes back.
pub const OBSERVABLE_STATES: [DeliveryState; 4] = [
    DeliveryState::Accepted,
    DeliveryState::Bounced { hard: false },
    DeliveryState::Delivered,
    DeliveryState::Bounced { hard: true },
];
