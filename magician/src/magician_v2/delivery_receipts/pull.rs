//! Reading a bounce mailbox on a cadence, and handing what is in it to the door.
//!
//! # Which way the dependency runs
//!
//! [`super::read_dsn`] parses, [`super::correlate`] ties a report to an act, and
//! `delivery::intake::ReceiptIntake::admit` records it. All three are built.
//! What connected them was a person: a human had to notice a bounce and POST
//! it. This file is the standing bridge that does it without one.
//!
//! It implements a port owned by
//! [`delivery_hygiene::receipts::ReceiptPuller`](crate::magician_v2::delivery_hygiene::receipts::ReceiptPuller),
//! and the direction is the whole point. The hygiene worker is rail-agnostic
//! and must stay so — a second rail is a second file like this one, not an edit
//! to the sweep. **Specific imports generic, never the other way round**, so
//! nothing in `delivery_hygiene` knows what RFC 3464 is and nothing here knows
//! what a suppression register is.
//!
//! # The one thing still unbuilt, said plainly
//!
//! [`BounceMailbox`] has **no production implementor in this build**. Nothing
//! in-process reads a mailbox: `agentmail-read::messages_get` is a skill, run
//! by an agent, and there is no in-runtime inbox reader to call. So this bridge
//! is complete and inert until something implements three methods over whatever
//! mailbox the deployment actually has — an AgentMail poller, an IMAP client, a
//! maildir a postmaster drops files into.
//!
//! That is deliberately a port rather than a guess. Writing an AgentMail-shaped
//! poller here would tie the bounce path to one provider's API for a channel
//! that predates it by thirty years, and it would have to be written again for
//! the second mailbox.
//!
//! # Fail closed
//!
//! - **Ordinary mail is settled, never recorded.** A message that is
//!   structurally not a delivery report produced no receipt and never will, so
//!   the bridge is done with it. Recognition is structural — see [`super::dsn`]
//!   — so a human's reply *about* a bounce is ordinary mail here, which is the
//!   whole reason it is safe to settle it.
//! - **A report that announces itself and cannot be read is HELD.** It is
//!   counted as `unreadable` and left in the mailbox for a person to look at.
//!   Settling it would discard the one message that says the reader is wrong.
//! - **An uncorrelated bounce is HELD.** [`super::correlate`] refuses a report
//!   it cannot tie to a send this scope recorded, and that refusal is carried
//!   up as a count rather than dropped: a rising `uncorrelated` is a broken
//!   send-side index, and it must never render as a quiet mailbox.
//! - **A mailbox that cannot be read propagates.** An empty answer from a
//!   broken mailbox would look exactly like a week with no bounces.
//!
//! # Counts, never rates
//!
//! `examined` is carried beside everything else, so "placed none of them" over
//! two messages and over two hundred cannot render alike.

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::{DeliveryScope, DeliveryState};
use crate::magician_v2::delivery_hygiene::receipts::{PulledBatch, PulledReceipt, ReceiptPuller};

use super::ingest::{read_mail, MailReading};
use super::store::{open_local_sent_index, SentMessageStore};

const LOG_TARGET: &str = "delivery_receipts::pull";

/// One message a mailbox is holding, as raw RFC 5322 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundMail {
    /// The mailbox's own handle for it — a message id, a uid, a filename.
    /// Opaque here; handed back to [`BounceMailbox::settle`].
    pub handle: String,
    /// The message exactly as it arrived. Parsed, never trusted: recognition is
    /// structural and a hostile body can only ever produce a refusal.
    pub raw: String,
}

/// A mailbox that delivery status notifications come back to.
///
/// Deliberately not an AgentMail client, an IMAP session or a file watcher.
/// Three methods over "what is unread, and let me mark it read" — which every
/// one of those can satisfy, and none of which this file has to learn.
pub trait BounceMailbox: Send + Sync {
    /// A short stable name for the health line. Reported, never parsed.
    fn name(&self) -> &str;

    /// Messages this mailbox holds that this bridge has not settled.
    ///
    /// An empty list means *"nothing new"* and is not an error. A mailbox that
    /// cannot be read is an error and must say so: an empty answer from a
    /// broken mailbox is indistinguishable from a week with no bounces, and the
    /// second is the reading that lets a dead address stay sendable.
    fn unread(&self, scope: &DeliveryScope, now: DateTime<Utc>) -> Result<Vec<InboundMail>>;

    /// Stop offering one message.
    ///
    /// Never a delete. The report has to stay readable for whoever later doubts
    /// the suppression it produced — a hard bounce is not operationally
    /// liftable, so the evidence outlives the decision.
    fn settle(&self, scope: &DeliveryScope, handle: &str, now: DateTime<Utc>) -> Result<()>;
}

/// The bridge: read the mailbox, parse, correlate, hand over.
///
/// Records nothing itself. The ledger write happens at the door, on the other
/// side of the port, so the act-must-have-left check, the attempt log and the
/// severity order all apply exactly as they do to a receipt somebody posts by
/// hand.
pub struct DsnPuller {
    /// The provider name recorded on every receipt this bridge produces, and
    /// the namespace its send-index lookups are keyed under.
    ///
    /// A parameter, because a DSN is not an artefact of any one provider: the
    /// same parse serves an AgentMail mailbox, an SMTP adapter's bounce box and
    /// a postmaster's forward. What it must **not** be is invented per message
    /// — the send index is keyed on it, and a lookup under the wrong provider
    /// finds nothing and refuses a bounce that was correlatable.
    provider: String,
    mailbox: Arc<dyn BounceMailbox>,
    index: Arc<dyn SentMessageStore>,
}

impl DsnPuller {
    pub fn new(
        provider: impl Into<String>,
        mailbox: Arc<dyn BounceMailbox>,
        workspace_layout: ArtifactV2Workspace,
    ) -> Self {
        Self {
            provider: provider.into(),
            mailbox,
            index: open_local_sent_index(workspace_layout),
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }
}

impl ReceiptPuller for DsnPuller {
    fn name(&self) -> &str {
        "dsn"
    }

    fn pull(&self, scope: &DeliveryScope, now: DateTime<Utc>) -> Result<PulledBatch> {
        let mail = self
            .mailbox
            .unread(scope, now)
            .with_context(|| format!("reading the `{}` bounce mailbox", self.mailbox.name()))?;

        let mut batch = PulledBatch {
            examined: mail.len(),
            ..PulledBatch::default()
        };

        for message in mail {
            // The payload ref points at the message this receipt was read out
            // of, so an auditor doubting a suppression can go back to the bytes.
            let payload_ref = format!("mailbox:{}:{}", self.mailbox.name(), message.handle);

            // `read_mail` and not `admit_mail`: the reading is reused whole —
            // recognition, correlation, every refusal — and the recording is
            // left to the pass on the other side of the port, which is where
            // the disclosure advance and the health counts live. Two callers
            // that both admitted would be two sets of counts of one fact.
            let reading = read_mail(
                &self.index,
                scope,
                &self.provider,
                &payload_ref,
                &message.raw,
            )
            .context("reading a message from the bounce mailbox")?;

            let harvest = match reading {
                // Structurally not a delivery report. It produced no receipt and
                // never will, so this bridge is done with it. Safe precisely
                // because recognition never reads a subject line: a human's mail
                // *about* a bounce lands here, and settling it means "read, and
                // not a bounce" rather than "deleted".
                MailReading::NotADeliveryReport { .. } => {
                    if let Err(error) = self.mailbox.settle(scope, &message.handle, now) {
                        tracing::warn!(
                            target: LOG_TARGET,
                            handle = %message.handle,
                            %error,
                            "could not settle a message that is not a delivery report"
                        );
                    }
                    continue;
                },
                // It says it is a delivery report and cannot be read. Held for a
                // person: settling it would discard the one message that says
                // this reader is wrong about something.
                MailReading::Unreadable { because } => {
                    batch.unreadable += 1;
                    tracing::warn!(
                        target: LOG_TARGET,
                        handle = %message.handle,
                        because = %because,
                        "a delivery report could not be read and was left in the mailbox"
                    );
                    continue;
                },
                MailReading::Read(harvest) => harvest,
            };

            batch.uncorrelated += harvest.refused.len();
            for refusal in &harvest.refused {
                tracing::warn!(
                    target: LOG_TARGET,
                    handle = %message.handle,
                    reason = refusal.reason.as_str(),
                    detail = %refusal.detail,
                    "a reported recipient produced no receipt"
                );
            }

            for intent in harvest.intents {
                batch.receipts.push(PulledReceipt {
                    handle: message.handle.clone(),
                    act_ref: intent.act_ref,
                    note: Some(format!(
                        "correlated on {}; {}",
                        intent.correlation.as_str(),
                        intent.correlation.confidence()
                    )),
                    receipt: intent.receipt,
                });
            }
        }

        Ok(batch)
    }

    fn settle(&self, scope: &DeliveryScope, handle: &str, now: DateTime<Utc>) -> Result<()> {
        self.mailbox.settle(scope, handle, now)
    }
}

/// Whether a state this bridge can produce will suppress somebody.
///
/// Exported for a health line rather than for a decision: the mapping from a
/// cause to a suppression reason lives in `delivery_hygiene`, and this is only
/// *"is this the kind of bounce that ends an address"*. A `4.x.x` transient
/// answers `false`, which is the distinction the whole reader is built around.
pub fn is_suppressing(state: DeliveryState) -> bool {
    state.suppression_cause().is_some()
}
