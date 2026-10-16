//! The named entry point: one piece of mail in, receipts through the door.
//!
//! # Why this is a separate file
//!
//! [`super::dsn`] and [`super::correlate`] write nothing and touch no store, and
//! that is a property worth being able to prove by looking at the file list.
//! This is the one place in the module that records anything, and it records it
//! by handing intent to
//! [`ReceiptIntake::admit`](crate::magician_v2::delivery::intake::ReceiptIntake::admit)
//! — the door that already exists — rather than by reaching into the ledger. The
//! same split `delivery_hygiene` makes between `signals_since` and
//! `sweep_delivery_into_suppression`, for the same reason: the reading must be
//! testable against a fixture with no store at all.
//!
//! # Every rule stays where it was decided
//!
//! The order, the terminal states, the one-provider-message-one-identity rule
//! and the act-must-have-left check are all the intake's and the ledger's. This
//! file adds none of them and softens none of them. What it adds is the two
//! steps that had no home: *is this mail a bounce*, and *which send is it about*.
//!
//! # Fail closed
//!
//! - Mail that is not a delivery report is [`MailReading::NotADeliveryReport`],
//!   which is a **statement**, not a receipt and not a silent discard.
//! - Mail that announces itself as a delivery report and cannot be read is
//!   [`MailReading::Unreadable`], reported separately so unreadable bounces
//!   cannot pile up looking like ordinary mail.
//! - An intake failure **propagates**. Receipts admitted before it are durable
//!   and idempotent, so re-presenting the same mail resumes them and re-attempts
//!   the one that failed; a partial success reported as a success would leave a
//!   bounce unrecorded with nothing saying so.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use super::sent_index::sends_for;
use super::store::SentMessageStore;
use super::{correlate, identifiers_in, CorrelationRefusal, DsnHarvest, SendIdentifier};
use crate::magician_v2::delivery::intake::{AdmittedReceipt, IntakeAttribution, ReceiptIntake};
use crate::magician_v2::delivery::{DeliveryScope, DispatchedAct};
use crate::magician_v2::delivery_receipts::dsn::{read_dsn, DsnRecognition};

/// What one piece of mail turned out to be.
///
/// Three arms, and the middle one is load-bearing: an unreadable bounce and a
/// message that is not a bounce are opposite findings, and rendering the first
/// as the second is how a rail goes quiet without anybody noticing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailReading<T> {
    /// Structurally not a delivery status notification. Ordinary mail — the
    /// expected answer for almost every message in an inbox.
    NotADeliveryReport {
        because: String,
    },
    /// It is a delivery report and could not be read. Never silently dropped.
    Unreadable {
        because: String,
    },
    Read(T),
}

/// What one delivery report did, once it went through the door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailIngest {
    pub report_message_id: Option<String>,
    pub correlated_on: Option<SendIdentifier>,
    /// How many per-recipient blocks the report carried. Carried beside the two
    /// lists so "nothing was admitted" over a report about nobody and over a
    /// report about nine people cannot render alike.
    pub recipients_reported: usize,
    /// What the ledger decided, receipt by receipt, untouched.
    pub admitted: Vec<AdmittedReceipt>,
    /// Everything that produced no receipt, and why.
    pub refused: Vec<CorrelationRefusal>,
}

/// Read one piece of mail and say what receipts it *would* produce.
///
/// Records nothing. This is the form an owner runs before letting a bounce
/// suppress anybody, and the form a poller runs over an inbox to find which
/// messages are worth presenting at all.
pub fn read_mail(
    index: &dyn SentMessageStore,
    scope: &DeliveryScope,
    provider: &str,
    payload_ref: &str,
    raw_mail: &str,
) -> Result<MailReading<DsnHarvest>> {
    let report = match read_dsn(raw_mail) {
        DsnRecognition::NotADsn { because } => {
            return Ok(MailReading::NotADeliveryReport { because })
        },
        DsnRecognition::Unreadable { because } => return Ok(MailReading::Unreadable { because }),
        DsnRecognition::Recognised(report) => report,
    };

    // The map is resolved here and passed in, so `correlate` stays pure and a
    // rail whose sends live somewhere else can build its own and call it
    // directly. A store fault propagates: an index that could not be read
    // answering "unknown send" would look exactly like somebody else's bounce.
    let sends = sends_for(index, scope, provider, &identifiers_in(&report))
        .context("resolving the identifiers a delivery report quoted")?;

    Ok(MailReading::Read(correlate(
        provider,
        payload_ref,
        &report,
        &sends,
    )?))
}

/// Read one piece of mail and put whatever it correlated through the intake
/// door.
///
/// **The named entry point of this module.** `dispatched` is supplied by
/// whoever dispatched — the intake refuses a receipt for an act this scope
/// never recorded as having left, which is the check that stops a crafted
/// bounce suppressing an arbitrary address.
///
/// # Ordering, and what a mid-way failure leaves behind
///
/// Receipts are admitted one at a time, in report order, and a failure stops
/// the run and propagates. Everything admitted before it is already durable —
/// and idempotent, because the provider message id is derived from the report
/// rather than from the clock — so presenting the same mail again resumes those
/// records and re-attempts the one that failed. Swallowing the failure to
/// report a tidy success would leave a bounce unrecorded with nothing saying so.
pub fn admit_mail(
    intake: &ReceiptIntake,
    index: &dyn SentMessageStore,
    scope: &DeliveryScope,
    dispatched: &[DispatchedAct],
    provider: &str,
    payload_ref: &str,
    raw_mail: &str,
    attribution: &IntakeAttribution,
    now: DateTime<Utc>,
) -> Result<MailReading<MailIngest>> {
    let harvest = match read_mail(index, scope, provider, payload_ref, raw_mail)? {
        MailReading::NotADeliveryReport { because } => {
            return Ok(MailReading::NotADeliveryReport { because })
        },
        MailReading::Unreadable { because } => return Ok(MailReading::Unreadable { because }),
        MailReading::Read(harvest) => harvest,
    };

    let mut admitted = Vec::with_capacity(harvest.intents.len());
    for intent in &harvest.intents {
        let outcome = intake
            .admit(
                scope,
                dispatched,
                &intent.act_ref,
                &intent.receipt,
                attribution,
                now,
            )
            .with_context(|| {
                format!(
                    "admitting the `{}` receipt for `{}` on act `{}` ({} of this report's \
                     receipts were already recorded and are idempotent on a retry)",
                    intent.receipt.state.as_str(),
                    intent.receipt.identity,
                    intent.act_ref,
                    admitted.len()
                )
            })?;
        admitted.push(outcome);
    }

    Ok(MailReading::Read(MailIngest {
        report_message_id: harvest.report_message_id,
        correlated_on: harvest.correlated_on,
        recipients_reported: harvest.recipients_reported,
        admitted,
        refused: harvest.refused,
    }))
}
