//! What we sent, under the identifier a bounce will quote back.
//!
//! # Why this store has to exist
//!
//! A delivery status notification returns an identifier — an RFC 3461
//! `Original-Envelope-Id`, or the original `Message-ID` inside the headers the
//! reporting MTA quoted back. That identifier is only useful if something on
//! this side remembers which act it belonged to. **Nothing did.** The outward
//! disclosure records the audience, the channel and the payload artifact; the
//! dispatch log records the act ref and an instant; neither records the id the
//! provider minted or we minted for the message itself. So every bounce that
//! ever arrived would have been uncorrelatable, and an uncorrelatable bounce is
//! one this system must refuse to act on.
//!
//! This is the missing half-line: `identifier → (act, audience)`.
//!
//! # Provider-agnostic, and a door rather than an integration
//!
//! A row is a provider name, an identifier, an act ref and the audience the act
//! went to. Nothing here knows what AgentMail is. An SMTP adapter that sets an
//! envelope id, a Kapso rail with its own message ids, and a person recording
//! what a console showed them all write the same row. The reader
//! ([`super::correlate`]) never learns which of them wrote it, which is what
//! keeps the correlation rule one rule instead of one per rail.
//!
//! # The audience is stored, and that is a safety property
//!
//! A bounce names an address. Without the audience there would be nothing to
//! check it against, and a report about `anyone@anywhere` quoting a real
//! `Message-ID` would file a hard bounce against an address the act never wrote
//! to — permanently, because `SuppressionReason::HardBounce` is not
//! operationally liftable. With the audience recorded, that report is refused.
//!
//! # Replay resumes; a changed payload is an error
//!
//! The identical row again is one row. The **same identifier** naming a
//! different act, a different audience or a different instant is refused rather
//! than kept or overwritten: two accounts of one message mean somebody upstream
//! is reusing ids, and quietly keeping the first would hide it until a bounce
//! arrived and was attributed to whichever account won.
//!
//! An identifier that has genuinely gone ambiguous on disk — two acts under one
//! id, written by two processes that raced — is reported as
//! [`SendBinding::Ambiguous`] rather than resolved. The reader turns that into
//! a refusal and records nothing, because choosing between two acts is choosing
//! whose address a hard bounce suppresses.
//!
//! # Fail closed
//!
//! Reads go through [`crate::magician_v2::jsonl`]: only a genuinely absent log
//! reads as "we have no record of this send", and every other fault propagates.
//! An unreadable index answering "unknown send" would look exactly like a
//! bounce for somebody else's mail, and the bounce would be silently dropped.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{caller_field, SendIdentifier, FIELD_SEP};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::{normalise_identity, DeliveryScope};

/// One message this scope sent, under one identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentMessage {
    /// The outward act the message carried. The ref the ledger reconciles
    /// against — opaque here, exactly as it is to the ledger.
    pub act_ref: String,
    /// Every address the act was addressed to, normalised. A report about
    /// somebody outside this set is refused.
    pub audience: Vec<String>,
    /// When the message left, as the recorder knows it.
    pub sent_at: DateTime<Utc>,
}

/// What the index says about one identifier.
///
/// Three arms rather than an `Option`, because "we have two answers" is a state
/// the caller must handle rather than pattern-match away with a `_ =>`. See the
/// module note on why it is never resolved here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendBinding {
    /// Nothing recorded. Not an error, and not permission either — the reader
    /// turns it into a refusal.
    Absent,
    Bound(SentMessage),
    /// One identifier, more than one act. Reported, never chosen between.
    Ambiguous {
        act_refs: Vec<String>,
    },
}

/// One line of the index. The export/import schema for this owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentIndexRecord {
    pub act_ref: String,
    pub provider: String,
    pub id_kind: String,
    pub id_value: String,
    pub audience: Vec<String>,
    pub sent_at: DateTime<Utc>,
    /// When **we** wrote the row. Kept beside `sent_at` because a caller can
    /// register a backdated send, and an audit filtered on the caller's clock
    /// would miss it.
    pub recorded_at: DateTime<Utc>,
}

type SentRow = SentIndexRecord;

/// The append-only index of what was sent, keyed by what a bounce will quote.
#[derive(Debug, Clone)]
pub struct SentMessageIndex {
    workspace_layout: ArtifactV2Workspace,
}

impl SentMessageIndex {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    /// One log per identifier, named by the **hash** of it.
    ///
    /// The identifier is never a path component: a `Message-ID` arrives from
    /// outside — a caller registers it, and a remote MTA quotes it back — and a
    /// caller-supplied string used raw as a filename is a traversal waiting for
    /// its first hostile payload. The kind is inside the hashed tuple so the
    /// two namespaces cannot collide: an envelope id that happens to read like
    /// a message id is a different fact.
    ///
    /// It sits under the same `delivery/` root as the ledger and the dispatch
    /// log, because a send index kept somewhere else is a send index that
    /// drifts from the receipts it is compared against.
    fn path(&self, scope: &DeliveryScope, provider: &str, identifier: &SendIdentifier) -> PathBuf {
        let identifier = canonical(identifier);
        self.workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("delivery")
            .join("index")
            .join("sent")
            .join(format!(
                "{}.jsonl",
                stable_id(&format!(
                    "{provider}{FIELD_SEP}{}{FIELD_SEP}{}",
                    identifier.kind(),
                    identifier.value()
                ))
            ))
    }

    /// Record that an act left under an identifier.
    ///
    /// Written **at send time**, by whoever sent. Called after the fact it is
    /// still useful — a bounce can arrive days later — but an identifier never
    /// recorded is a bounce that can never be attributed, so the honest place
    /// for this call is beside the dispatch record.
    ///
    /// # An empty audience is refused
    ///
    /// The audience is what a reported address is checked against. Recorded
    /// empty, every check would fail closed and every bounce for this send would
    /// be refused — which is safe, and is also a silent hole that would look
    /// exactly like a provider that never bounces. It is an error instead.
    pub fn record(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
        sent: &SentMessage,
        now: DateTime<Utc>,
    ) -> Result<()> {
        validate_scope(scope)?;
        let provider = caller_field(provider, "a provider name")?;
        let act_ref = caller_field(&sent.act_ref, "an act ref")?;
        // Canonicalised BEFORE validation, so the string that is checked is the
        // string that is stored and hashed into the path.
        let identifier = canonical(identifier);
        let id_value = caller_field(identifier.value(), "a send identifier")?;
        if sent.audience.is_empty() {
            anyhow::bail!(
                "a send registered with no audience can never have a bounce attributed to it: \
                 every reported address is checked against this list, so an empty one refuses \
                 every receipt for act `{act_ref}` while looking exactly like a rail that never \
                 bounces"
            );
        }
        let mut audience: Vec<String> = sent
            .audience
            .iter()
            .map(|address| normalise_identity(address))
            .collect::<Result<Vec<_>>>()
            .context("normalising the audience of a sent message")?;
        audience.sort();
        audience.dedup();

        let row = SentRow {
            act_ref: act_ref.clone(),
            provider: provider.clone(),
            id_kind: identifier.kind().to_string(),
            id_value: id_value.clone(),
            audience: audience.clone(),
            sent_at: sent.sent_at,
            recorded_at: now,
        };

        for held in self.rows(scope, &provider, &identifier)? {
            if held.act_ref == row.act_ref
                && held.audience == row.audience
                && held.sent_at == row.sent_at
            {
                // The identical registration again. One record, not two.
                return Ok(());
            }
            anyhow::bail!(
                "{} `{id_value}` is already recorded for act `{}` to {} sent at {}, and this \
                 call says act `{}` to {} sent at {}: an identical replay resumes, but two \
                 accounts of one message mean somebody upstream is reusing identifiers, and \
                 keeping the first would decide which act a later bounce is attributed to",
                identifier.kind(),
                held.act_ref,
                held.audience.join(", "),
                held.sent_at.to_rfc3339(),
                row.act_ref,
                row.audience.join(", "),
                row.sent_at.to_rfc3339(),
            );
        }

        let path = self.path(scope, &provider, &identifier);
        let line = serde_json::to_vec(&row).context("serialising a sent-message record")?;
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &line)
            .with_context(|| format!("appending {}", path.display()))
    }

    /// What this scope recorded under one identifier.
    ///
    /// An absent log is [`SendBinding::Absent`] — the one case that legitimately
    /// reads as "no record". Every other fault propagates, because an index that
    /// answered "unknown send" because it could not be read would make a real
    /// bounce indistinguishable from somebody else's mail.
    pub fn lookup(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
    ) -> Result<SendBinding> {
        validate_scope(scope)?;
        let provider = caller_field(provider, "a provider name")?;
        let rows = self.rows(scope, &provider, identifier)?;
        let mut acts: Vec<String> = rows.iter().map(|row| row.act_ref.clone()).collect();
        acts.sort();
        acts.dedup();
        // The arbitration is on the number of DISTINCT ROWS, not on the number
        // of distinct acts. Two rows naming one act with two different
        // audiences are still two accounts of one message, and picking the
        // first would pick which addresses a later bounce may be filed against
        // — the same coin-flip as picking between two acts, one layer down.
        match rows.len() {
            0 => Ok(SendBinding::Absent),
            1 => {
                let row = rows.into_iter().next().expect("exactly one row");
                Ok(SendBinding::Bound(SentMessage {
                    act_ref: row.act_ref,
                    audience: row.audience,
                    sent_at: row.sent_at,
                }))
            },
            // Two writers raced past each other's read. The record is honestly
            // ambiguous and is reported as such; see the module note.
            _ => Ok(SendBinding::Ambiguous { act_refs: acts }),
        }
    }

    fn rows(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
    ) -> Result<Vec<SentRow>> {
        let path = self.path(scope, provider, identifier);
        let Some(raw) =
            crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
        else {
            return Ok(Vec::new());
        };
        let rows = crate::magician_v2::jsonl::parse_log_lines::<SentRow>(&raw, &path)?;
        // Two concurrent registrations of one identifier can both miss each
        // other's read and both append. Identical rows fold to one; genuinely
        // different ones survive so `lookup` can report the ambiguity rather
        // than silently picking the first.
        let mut seen = std::collections::BTreeSet::new();
        Ok(rows
            .into_iter()
            .filter(|row| seen.insert((row.act_ref.clone(), row.audience.clone(), row.sent_at)))
            .collect())
    }

    pub fn list_scope(&self, scope: &DeliveryScope) -> Result<Vec<SentIndexRecord>> {
        validate_scope(scope)?;
        let dir = self
            .workspace_layout
            .scope_root(&scope.principal, &scope.workspace)
            .join("delivery")
            .join("index")
            .join("sent");
        let mut records = Vec::new();
        for path in crate::magician_v2::jsonl::list_log_paths(&self.workspace_layout, &dir)? {
            let Some(raw) =
                crate::magician_v2::jsonl::read_log_if_present(&self.workspace_layout, &path)?
            else {
                continue;
            };
            records.extend(crate::magician_v2::jsonl::parse_log_lines::<SentRow>(
                &raw, &path,
            )?);
        }
        Ok(fold_scope_records(records))
    }

    pub fn export_scope(&self, scope: &DeliveryScope) -> Result<Vec<u8>> {
        serde_json::to_vec(&self.list_scope(scope)?).context("serialising sent-index export")
    }

    pub fn import_scope(&self, scope: &DeliveryScope, bytes: &[u8]) -> Result<()> {
        let records: Vec<SentIndexRecord> =
            serde_json::from_slice(bytes).context("parsing sent-index export")?;
        let mut groups = std::collections::BTreeMap::<String, Vec<SentIndexRecord>>::new();
        for record in fold_scope_records(records) {
            let key = format!(
                "{}\u{1f}{}\u{1f}{}",
                record.provider, record.id_kind, record.id_value
            );
            groups.entry(key).or_default().push(record);
        }
        for records in groups.into_values() {
            self.import_identifier_rows(scope, records)?;
        }
        Ok(())
    }

    fn import_identifier_rows(
        &self,
        scope: &DeliveryScope,
        records: Vec<SentIndexRecord>,
    ) -> Result<()> {
        validate_scope(scope)?;
        let Some(first) = records.first() else {
            return Ok(());
        };
        let provider = caller_field(&first.provider, "a provider name")?;
        let identifier = identifier_from_parts(&first.id_kind, &first.id_value)?;
        let identifier = canonical(&identifier);
        let held = self.rows(scope, &provider, &identifier)?;
        let mut body = Vec::new();
        for record in records {
            if held.iter().any(|row| {
                row.act_ref == record.act_ref
                    && row.audience == record.audience
                    && row.sent_at == record.sent_at
            }) {
                continue;
            }
            body.extend(serde_json::to_vec(&record).context("serialising a sent-message record")?);
            body.push(b'\n');
        }
        if body.is_empty() {
            return Ok(());
        }
        let path = self.path(scope, &provider, &identifier);
        crate::magician_v2::jsonl::append_log_line(&self.workspace_layout, &path, &body)
            .with_context(|| format!("appending {}", path.display()))
    }
}

pub(crate) fn fold_scope_records(records: Vec<SentIndexRecord>) -> Vec<SentIndexRecord> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for record in records {
        let key = (
            record.provider.clone(),
            record.id_kind.clone(),
            record.id_value.clone(),
            record.act_ref.clone(),
            record.audience.clone(),
            record.sent_at,
        );
        if seen.insert(key) {
            out.push(record);
        }
    }
    out.sort_by(|a, b| {
        (
            &a.provider,
            &a.id_kind,
            &a.id_value,
            &a.act_ref,
            &a.audience,
            a.sent_at,
        )
            .cmp(&(
                &b.provider,
                &b.id_kind,
                &b.id_value,
                &b.act_ref,
                &b.audience,
                b.sent_at,
            ))
    });
    out
}

pub(crate) fn binding_from_records(records: Vec<SentIndexRecord>) -> SendBinding {
    let folded = fold_scope_records(records);
    let mut acts: Vec<String> = folded.iter().map(|row| row.act_ref.clone()).collect();
    acts.sort();
    acts.dedup();
    match folded.len() {
        0 => SendBinding::Absent,
        1 => {
            let row = folded.into_iter().next().expect("exactly one row");
            SendBinding::Bound(SentMessage {
                act_ref: row.act_ref,
                audience: row.audience,
                sent_at: row.sent_at,
            })
        },
        _ => SendBinding::Ambiguous { act_refs: acts },
    }
}

pub(crate) fn identifier_from_parts(kind: &str, value: &str) -> Result<SendIdentifier> {
    match kind {
        "message_id" => Ok(SendIdentifier::MessageId(value.to_string())),
        "envelope_id" => Ok(SendIdentifier::EnvelopeId(value.to_string())),
        other => anyhow::bail!("unknown send identifier kind `{other}`"),
    }
}

/// Resolve every identifier a report quotes, in one pass.
///
/// *A* source of [`correlate`](super::correlate)'s map, and not *the* source:
/// a rail whose sends are recorded somewhere else builds its own map and this
/// function is not touched. The same "supplied, never discovered" split
/// `delivery_hygiene::silence::rails_from_disclosures` makes for rail
/// attribution, for the same reason — a second copy of "which act was this"
/// kept inside the reader would drift from the record that owns it.
pub fn sends_for(
    index: &dyn super::store::SentMessageStore,
    scope: &DeliveryScope,
    provider: &str,
    identifiers: &[SendIdentifier],
) -> Result<BTreeMap<SendIdentifier, SendBinding>> {
    let mut out = BTreeMap::new();
    for identifier in identifiers {
        let binding = index.lookup(scope, provider, identifier).with_context(|| {
            format!(
                "resolving {} `{}` against the sent-message index",
                identifier.kind(),
                identifier.value()
            )
        })?;
        out.insert(identifier.clone(), binding);
    }
    Ok(out)
}

/// The one written form of an identifier, applied on **every** path in and out.
///
/// `<a@b>` and `a@b` are the same `Message-ID` written two ways: a mail header
/// carries the angle brackets and a caller registering a send usually does not.
/// A store that hashed one form on the way in and the other on the way out
/// would find nothing, every bounce would come back `unknown_send`, and that
/// looks exactly like a rail that never bounces — which is why the folding
/// lives here, in the store both sides go through, rather than in either
/// caller.
pub(crate) fn canonical(identifier: &SendIdentifier) -> SendIdentifier {
    match identifier {
        SendIdentifier::MessageId(value) => {
            SendIdentifier::MessageId(super::dsn::unwrap_message_id(value))
        },
        SendIdentifier::EnvelopeId(value) => SendIdentifier::EnvelopeId(value.trim().to_string()),
    }
}

fn stable_id(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..32].to_string()
}

/// A scope carrying the separator could shear a path or an id apart, so it is
/// refused before any read — the same check, in the same place, the ledger and
/// the silence watch make.
pub(crate) fn validate_scope(scope: &DeliveryScope) -> Result<()> {
    if scope.principal.contains(FIELD_SEP) || scope.workspace.contains(FIELD_SEP) {
        anyhow::bail!(
            "a scope's principal and workspace must not contain U+001F: it is the separator that \
             keeps a derived id's components apart, and a crafted scope could otherwise resume \
             another owner's record"
        );
    }
    Ok(())
}
