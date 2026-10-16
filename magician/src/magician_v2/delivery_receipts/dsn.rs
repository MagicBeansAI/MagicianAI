//! Reading a bounce that came back **as mail**.
//!
//! # Why this file exists
//!
//! No AgentMail skill exposes a delivery-event stream — `agentmail-send`,
//! `agentmail-read` and `agentmail-processing` carry no bounce, complaint,
//! webhook or delivery-status surface — so there is no provider event feed to
//! subscribe to. There is, however, a second channel that predates webhooks by
//! thirty years and that every MTA on earth still speaks: when a message cannot
//! be delivered, the receiving system sends a **Delivery Status Notification**
//! back to the envelope sender. It arrives in the same inbox
//! `agentmail-read::messages_get` already reads.
//!
//! A DSN is a structured document, not prose: `multipart/report;
//! report-type=delivery-status` (RFC 3462) wrapping a `message/delivery-status`
//! part (RFC 3464) whose per-recipient blocks carry `Action:`, an RFC 3463
//! `Status:` code, and `Final-Recipient:`. That structure is the whole reason
//! this is readable at all.
//!
//! # Recognition is structural, never lexical
//!
//! This file will not recognise a bounce from a subject line. "Undeliverable",
//! "Mail delivery failed", "Returned to sender" and every localisation of them
//! are also the subjects of perfectly legitimate human mail *about* a failed
//! delivery — a colleague forwarding a complaint, a customer asking why their
//! invoice never arrived. Matching on that text is how a real correspondent
//! gets permanently suppressed by a message they wrote by hand, and a
//! suppression raised from a hard bounce cannot be lifted operationally.
//!
//! So the only admissible evidence is the machine-readable half:
//!
//! - the report container's `Content-Type` (`multipart/report`, with
//!   `report-type` either absent or exactly `delivery-status` — an MDN read
//!   receipt announces `report-type=disposition-notification` and is refused);
//! - a `message/delivery-status` part inside it, which is definitive on its own;
//! - per-recipient `Action:` and `Status:` fields inside that part.
//!
//! Subject, sender (`MAILER-DAEMON` or otherwise) and body prose are read by
//! nothing here.
//!
//! # Structure and meaning are separated on purpose
//!
//! [`read_dsn`] extracts fields and interprets none of them: it hands back the
//! raw `Action:` token and the raw `Status:` string exactly as the reporting
//! MTA wrote them. [`classify`] is the one place a token becomes a
//! [`DeliveryState`], and it is a total function over a table this file's tests
//! walk cell by cell. Keeping them apart means a malformed report degrades into
//! a *named* fault the caller can report, instead of an unwrap or a default.
//!
//! # Fail closed, three ways
//!
//! - **`5.x.x` is permanent, `4.x.x` is transient.** Getting that backwards
//!   suppresses somebody whose mailbox was briefly full — permanently, because
//!   `SuppressionReason::HardBounce` is what the hygiene sweep records. So the
//!   class is read from `Status:` and from nothing else. `Diagnostic-Code:` is
//!   carried for the audit trail and is deliberately **never** mined for a code
//!   to fall back on: a `550` inside a free-text SMTP reply is the remote
//!   server's prose, and inferring permanence from prose is the same guess this
//!   module refuses at the subject line.
//! - **An absent or unreadable `Status:` yields no receipt at all.** RFC 3464
//!   requires the field; a report without one cannot say whether an address is
//!   dead or merely busy, and "probably permanent" is not a thing this codebase
//!   is allowed to conclude.
//! - **A contradiction is refused, not resolved.** `Action: delivered` beside a
//!   `5.x.x` status, or `Action: failed` beside `2.x.x`, means the report is
//!   malformed or was rewritten in transit. Picking whichever half looks more
//!   plausible is picking whether to suppress a real person.
//!
//! # What this file never does
//!
//! It writes nothing, reads no store, and takes no clock. Every value it
//! returns came out of the bytes it was handed. The correlation step —
//! *which send is this bounce about* — lives in [`super`], because it needs a
//! record of what was sent and this file must stay usable against a fixture.

use chrono::{DateTime, Utc};

use crate::magician_v2::delivery::DeliveryState;

/// The largest message this will parse.
///
/// A bounce carrying the original message can be large, and a hostile one can
/// be arbitrarily large. Bounded because everything below walks the whole
/// buffer and a door that accepts unbounded input from a mail stream is a door
/// that can be made to eat the process.
pub const MAX_MAIL_BYTES: usize = 1 << 20;

/// How far to descend looking for the report container.
///
/// A bounce forwarded by a human arrives as `multipart/mixed` wrapping a
/// `message/rfc822` wrapping the real `multipart/report`, which is depth three.
/// Bounded so a crafted nest of ten thousand parts cannot be walked.
const MAX_DEPTH: usize = 5;

/// The largest number of MIME parts examined at one level.
const MAX_PARTS: usize = 64;

/// What one message turned out to be.
///
/// Three arms rather than two, and the third is the important one: a message
/// that *is* a bounce and could not be read is a completely different fact from
/// a message that is not a bounce. The first must be reported loudly so a human
/// can look at it; the second is ordinary mail and is nobody's problem. Folding
/// them together would let unreadable bounces accumulate silently, which is the
/// exact shape of the blindness this tier exists to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DsnRecognition {
    /// Structurally not a delivery status notification. Ordinary mail.
    NotADsn {
        /// Which structural test it failed. Named so an operator debugging a
        /// bounce that "should have worked" is told what was looked for.
        because: String,
    },
    /// It announces itself as a delivery report and cannot be read. Never
    /// silently discarded and never treated as "not a bounce".
    Unreadable {
        because: String,
    },
    Recognised(DsnReport),
}

/// A delivery status notification, as fields. Nothing here is interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsnReport {
    /// The notification's **own** `Message-ID`, unwrapped. The idempotency
    /// anchor: posting the same bounce twice must reconcile to one record.
    pub report_message_id: Option<String>,
    /// A content fingerprint of the normalised mail, used as the idempotency
    /// anchor when the notification carries no `Message-ID` of its own. Two
    /// byte-identical posts fingerprint alike; anything else does not.
    pub fingerprint: String,
    /// `Date:` of the notification container.
    pub report_date: Option<DateTime<Utc>>,
    /// `Reporting-MTA:` — who is speaking. Audit only.
    pub reporting_mta: Option<String>,
    /// RFC 3461 `Original-Envelope-Id:` — an id the *sender* minted at
    /// submission and the report echoes back. The strongest correlation there
    /// is, when a sender sets one.
    pub original_envelope_id: Option<String>,
    /// `Message-ID:` of the original message, taken from the returned headers
    /// part. Strong correlation: it is an id we minted.
    pub original_message_id: Option<String>,
    /// `Arrival-Date:` from the per-message block.
    pub arrival_date: Option<DateTime<Utc>>,
    /// One entry per per-recipient block, in report order. Never deduplicated
    /// here — a report naming one address twice is a fact the caller should see.
    pub recipients: Vec<RecipientReport>,
}

/// One per-recipient block of a `message/delivery-status` part.
///
/// `action` and `status` are the **raw strings the MTA wrote**. They are not
/// parsed here so that an unrecognised token survives as itself into the
/// caller's refusal, instead of collapsing into a default that would then be
/// acted on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecipientReport {
    /// The address the report is finally about, with its `addr-type;` prefix
    /// stripped. `None` when the block named none or named a non-`rfc822` type.
    pub final_recipient: Option<String>,
    /// The address as **we** addressed it, when the report says. Present when a
    /// forwarding chain relayed our message onward: the final recipient is then
    /// somebody downstream we never wrote to, and this is the address that was
    /// actually ours.
    pub original_recipient: Option<String>,
    /// The raw `Action:` token, trimmed and lowercased.
    pub action: Option<String>,
    /// The raw `Status:` value, trimmed.
    pub status: Option<String>,
    /// `Diagnostic-Code:` — the remote server's own words. Carried for audit
    /// and **never** parsed for a status class.
    pub diagnostic_code: Option<String>,
    /// `Last-Attempt-Date:` — when this recipient's delivery last failed.
    pub last_attempt: Option<DateTime<Utc>>,
    /// `Remote-MTA:` — audit only.
    pub remote_mta: Option<String>,
}

/// An RFC 3463 status code, split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusCode {
    /// 2 = success, 4 = persistent transient failure, 5 = permanent failure.
    /// No other class exists, and one is refused rather than assumed.
    pub class: u8,
    pub subject: u16,
    pub detail: u16,
}

/// Why a per-recipient block yields no receipt.
///
/// Every arm is a case where recording *something* would be worse than
/// recording nothing, so each one is returned to the caller to report rather
/// than resolved here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingFault {
    /// An `Action:` token this build does not know. A future RFC arm must not
    /// inherit whichever behaviour a catch-all happened to have.
    UnknownAction { token: String },
    /// `Status:` absent, or not `class.subject.detail` with a class of 2, 4 or
    /// 5. Without it, permanent and transient are indistinguishable.
    UnreadableStatus { raw: Option<String> },
    /// The action and the status class disagree. Refused, never reconciled by
    /// preferring one half.
    ContradictoryReport { action: String, status: String },
    /// The block named no recipient at all.
    NoRecipient,
}

impl ReadingFault {
    /// A stable token for logs and API bodies.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UnknownAction { .. } => "unknown_action",
            Self::UnreadableStatus { .. } => "unreadable_status",
            Self::ContradictoryReport { .. } => "contradictory_report",
            Self::NoRecipient => "no_recipient",
        }
    }

    /// The sentence an operator reads.
    pub fn explain(&self) -> String {
        match self {
            Self::UnknownAction { token } => format!(
                "the report's `Action: {token}` is not one this build knows (failed, delayed, \
                 delivered, relayed, expanded), and guessing which of them it resembles would \
                 guess whether an address is dead"
            ),
            Self::UnreadableStatus { raw: Some(raw) } => format!(
                "the report's `Status: {raw}` is not an RFC 3463 code of class 2, 4 or 5, so it \
                 cannot say whether this failure is permanent or transient; a permanent reading \
                 suppresses the address forever and a transient one hides a dead one"
            ),
            Self::UnreadableStatus { raw: None } => "the report carries no `Status:` field for \
                 this recipient, so permanent and transient are indistinguishable; RFC 3464 \
                 requires the field and the `Diagnostic-Code:` prose is deliberately not mined \
                 for a substitute"
                .to_string(),
            Self::ContradictoryReport { action, status } => format!(
                "the report says `Action: {action}` beside `Status: {status}`, which contradict: \
                 the report is malformed or was rewritten in transit, and choosing whichever \
                 half looks more plausible would choose whether to suppress a real person"
            ),
            Self::NoRecipient => "the per-recipient block names no `Final-Recipient:` and no \
                 `Original-Recipient:`, so there is nobody the receipt could be about"
                .to_string(),
        }
    }
}

/// Turn one recipient block's `Action:` and `Status:` into a delivery state.
///
/// The whole mapping, in one total function. The table, with the class read
/// from `Status:`:
///
/// | `Action:`   | class 2       | class 4        | class 5        |
/// |-------------|---------------|----------------|----------------|
/// | `failed`    | contradiction | **soft** bounce| **hard** bounce|
/// | `delayed`   | contradiction | soft bounce    | contradiction  |
/// | `delivered` | delivered     | contradiction  | contradiction  |
/// | `relayed`   | accepted      | contradiction  | contradiction  |
/// | `expanded`  | accepted      | contradiction  | contradiction  |
///
/// Three cells are worth defending:
///
/// - **`failed` + `4.x.x` is a soft bounce.** An MTA that gives up after days of
///   retrying still reported a *transient* class, and a transient class must
///   never suppress: `DeliveryState::Bounced { hard: false }` carries no
///   suppression cause, by the delivery module's own design. A mailbox that was
///   full is not somebody we may never contact again.
/// - **`relayed` and `expanded` are `Accepted`, not `Delivered`.** RFC 3464 is
///   explicit that `relayed` means the message was passed to a system that
///   *cannot* report delivery, and `expanded` means an alias was expanded — in
///   neither case did anybody confirm arrival. Reading them as delivery would
///   let the disclosure register believe a person saw something nobody
///   confirmed they saw. `Accepted` is the honest floor, and it still counts as
///   an acknowledgement, so the act leaves the silence watch — which is true:
///   something *did* come back.
/// - **`delivered` + `2.x.x` is a real positive receipt.** It arrives only when
///   the sender asked for success notification, but when it arrives it is the
///   only proof of arrival email can produce, and discarding it would leave a
///   confirmed send parked at `dispatch_unknown`.
pub fn classify(action: Option<&str>, status: Option<&str>) -> Result<DeliveryState, ReadingFault> {
    let Some(action) = action.map(str::trim).filter(|token| !token.is_empty()) else {
        return Err(ReadingFault::UnknownAction {
            token: String::new(),
        });
    };
    let action = action.to_ascii_lowercase();

    let raw_status = status.map(str::trim).filter(|value| !value.is_empty());
    let Some(code) = raw_status.and_then(parse_status) else {
        return Err(ReadingFault::UnreadableStatus {
            raw: raw_status.map(str::to_string),
        });
    };

    // Exhaustive over the five RFC 3464 actions, with no catch-all: a token
    // this build does not know is a named fault, not an inherited default.
    match (action.as_str(), code.class) {
        ("failed", 5) => Ok(DeliveryState::Bounced { hard: true }),
        ("failed", 4) => Ok(DeliveryState::Bounced { hard: false }),
        ("delayed", 4) => Ok(DeliveryState::Bounced { hard: false }),
        ("delivered", 2) => Ok(DeliveryState::Delivered),
        ("relayed", 2) | ("expanded", 2) => Ok(DeliveryState::Accepted),
        ("failed" | "delayed" | "delivered" | "relayed" | "expanded", _) => {
            Err(ReadingFault::ContradictoryReport {
                action: action.clone(),
                status: raw_status.unwrap_or_default().to_string(),
            })
        },
        _ => Err(ReadingFault::UnknownAction { token: action }),
    }
}

/// Parse an RFC 3463 `Status:` value.
///
/// `None` for anything that is not `class.subject.detail` with a class of 2, 4
/// or 5 — the only three RFC 3463 defines. A class of 3 or 9 is a report this
/// build cannot read, and mapping it onto the nearest neighbour would map it
/// onto a suppression decision.
fn parse_status(raw: &str) -> Option<StatusCode> {
    let token = raw.split_whitespace().next()?;
    let mut fields = token.split('.');
    let class: u8 = fields.next()?.parse().ok()?;
    let subject: u16 = fields.next()?.parse().ok()?;
    let detail: u16 = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    if !matches!(class, 2 | 4 | 5) {
        return None;
    }
    Some(StatusCode {
        class,
        subject,
        detail,
    })
}

/// Read one raw RFC 5322 message and say what it is.
///
/// Pure: no clock, no store, no network. Hand it the bytes an inbox reader
/// fetched, a human forwarded, or a fixture holds.
pub fn read_dsn(raw_mail: &str) -> DsnRecognition {
    if raw_mail.len() > MAX_MAIL_BYTES {
        return DsnRecognition::Unreadable {
            because: format!(
                "the message is {} bytes and this reader accepts at most {MAX_MAIL_BYTES}; an \
                 unbounded parse of mail-stream input is a door that can be made to eat the \
                 process",
                raw_mail.len()
            ),
        };
    }
    if raw_mail.trim().is_empty() {
        return DsnRecognition::NotADsn {
            because: "the message is empty".to_string(),
        };
    }

    let normalised = normalise(raw_mail);
    let fingerprint = blake3::hash(normalised.as_bytes()).to_hex()[..32].to_string();
    let envelope = parse_message(&normalised);

    let Some(container) = locate_report(&envelope, 0) else {
        return DsnRecognition::NotADsn {
            because: "no `multipart/report` container and no `message/delivery-status` part was \
                      found, at any nesting depth this reader walks. Recognition is structural: \
                      a subject line is never evidence of a bounce, because legitimate human \
                      mail about a failed delivery carries the same words"
                .to_string(),
        };
    };

    let ReportContainer {
        status_part,
        returned_headers,
        report_date,
    } = container;

    let per_message = status_part.per_message;
    let recipients = status_part.recipients;
    if recipients.is_empty() {
        return DsnRecognition::Unreadable {
            because: "the `message/delivery-status` part carries no per-recipient block, so the \
                      report names nothing it is about. It announced itself as a delivery report \
                      and could not be read, which is not the same as not being one"
                .to_string(),
        };
    }

    DsnRecognition::Recognised(DsnReport {
        report_message_id: header_of(&envelope.headers, "message-id").map(unwrap_message_id),
        fingerprint,
        report_date,
        reporting_mta: field_of(&per_message, "reporting-mta").map(str::to_string),
        original_envelope_id: field_of(&per_message, "original-envelope-id")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        original_message_id: returned_headers
            .as_ref()
            .and_then(|headers| header_of(headers, "message-id"))
            .map(unwrap_message_id),
        arrival_date: field_of(&per_message, "arrival-date").and_then(parse_date),
        recipients,
    })
}

/// Strip one surrounding pair of angle brackets from a `Message-ID`.
///
/// `<a@b>` and `a@b` are the same id written two ways, and an index that stored
/// one form and looked up the other would find nothing and report every bounce
/// as uncorrelatable. The sent-side index applies the identical rule.
pub fn unwrap_message_id(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

// ── MIME, as much of it as a DSN needs ──────────────────────────────────────

/// One parsed MIME entity: unfolded headers and the body after the blank line.
#[derive(Debug, Clone)]
struct Entity {
    /// `(lowercased name, unfolded value)`, in file order.
    headers: Vec<(String, String)>,
    body: String,
}

/// The parts of a located report that anything downstream cares about.
struct ReportContainer {
    status_part: StatusPart,
    /// Headers of the returned original message, from the `message/rfc822` or
    /// `text/rfc822-headers` part. Absent when the reporting MTA returned none.
    returned_headers: Option<Vec<(String, String)>>,
    /// `Date:` of the message the report was found in.
    report_date: Option<DateTime<Utc>>,
}

/// A parsed `message/delivery-status` body.
struct StatusPart {
    per_message: Vec<(String, String)>,
    recipients: Vec<RecipientReport>,
}

/// CRLF to LF, and a leading BOM removed.
///
/// Every offset below is computed on this form, so boundary matching cannot
/// depend on which line ending the transport used.
fn normalise(raw: &str) -> String {
    raw.trim_start_matches('\u{feff}').replace("\r\n", "\n")
}

/// Split an entity into unfolded headers and a body.
///
/// A continuation line — one starting with a space or a tab — belongs to the
/// header above it and is joined with a single space. Multi-line
/// `Diagnostic-Code:` values arrive folded and are unreadable without this.
fn parse_message(raw: &str) -> Entity {
    let (head, body) = match raw.find("\n\n") {
        Some(at) => (&raw[..at], &raw[at + 2..]),
        None => (raw, ""),
    };
    Entity {
        headers: parse_fields(head),
        body: body.to_string(),
    }
}

/// Parse a block of `Name: value` fields, unfolding continuations.
fn parse_fields(block: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in block.split('\n') {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
            // A continuation with nothing above it is a malformed header block;
            // it is dropped rather than promoted to a field of its own, because
            // inventing a field name is inventing a fact.
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        out.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    out
}

fn header_of<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn field_of<'f>(fields: &'f [(String, String)], name: &str) -> Option<&'f str> {
    header_of(fields, name)
}

/// A parsed `Content-Type`.
struct ContentType {
    /// `type/subtype`, lowercased.
    mime: String,
    params: Vec<(String, String)>,
}

impl ContentType {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn content_type(headers: &[(String, String)]) -> ContentType {
    // RFC 2045: an entity with no Content-Type is text/plain. Said explicitly
    // rather than left as an empty string, so the multipart tests below read as
    // the questions they are.
    let raw = header_of(headers, "content-type").unwrap_or("text/plain");
    let mut fields = split_unquoted(raw, ';').into_iter();
    let mime = fields
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    // Parameter NAMES are case-insensitive and parameter VALUES are not: a
    // MIME boundary is compared byte for byte, and lowercasing it here would
    // make `--B1` unmatchable, every part of the report invisible, and a real
    // bounce read as ordinary mail. Values that ARE case-insensitive — only
    // `report-type` below — are folded at the comparison instead.
    let params = fields
        .filter_map(|field| {
            let (name, value) = field.split_once('=')?;
            Some((name.trim().to_ascii_lowercase(), unquote(value.trim())))
        })
        .collect();
    ContentType { mime, params }
}

/// Split on a separator that is not inside a quoted string.
///
/// A `boundary="a;b"` is legal and splitting it naively would truncate the
/// boundary, after which every part of the report becomes invisible and a real
/// bounce reads as ordinary mail.
fn split_unquoted(raw: &str, separator: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in raw.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                current.push(character);
            },
            other if other == separator && !quoted => {
                out.push(std::mem::take(&mut current));
            },
            other => current.push(other),
        }
    }
    out.push(current);
    out
}

fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    match trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => trimmed.to_string(),
    }
}

/// Whether a part's transfer encoding is one this reader can read as-is.
///
/// RFC 3462 requires the `message/delivery-status` part to be 7bit, and in
/// practice the returned-headers part is too. A base64 part is refused rather
/// than decoded-by-guess: a wrong decode of a bounce is a wrong suppression.
fn is_plain_encoding(headers: &[(String, String)]) -> bool {
    match header_of(headers, "content-transfer-encoding") {
        None => true,
        Some(encoding) => matches!(
            encoding.trim().to_ascii_lowercase().as_str(),
            "7bit" | "8bit" | "binary" | ""
        ),
    }
}

/// Split a multipart body on its boundary.
fn split_parts(body: &str, boundary: &str) -> Vec<String> {
    let opener = format!("--{boundary}");
    let closer = format!("--{boundary}--");
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in body.split('\n') {
        let trimmed = line.trim_end();
        if trimmed == closer {
            if let Some(lines) = current.take() {
                parts.push(lines.join("\n"));
            }
            break;
        }
        if trimmed == opener {
            if let Some(lines) = current.take() {
                parts.push(lines.join("\n"));
            }
            if parts.len() >= MAX_PARTS {
                break;
            }
            current = Some(Vec::new());
            continue;
        }
        if let Some(lines) = current.as_mut() {
            lines.push(line);
        }
    }
    if let Some(lines) = current.take() {
        parts.push(lines.join("\n"));
    }
    parts
}

/// Find the delivery report, descending through wrappers.
///
/// Two containers count, and both tests are structural:
///
/// - `multipart/report` whose `report-type` is absent or exactly
///   `delivery-status`, holding a `message/delivery-status` part. An MDN read
///   receipt (`report-type=disposition-notification`) is refused here — it is a
///   report, and it is not a bounce.
/// - a bare `message/delivery-status` entity, which some MTAs send unwrapped.
///
/// The descent through `multipart/*` and `message/rfc822` is what makes a
/// human-forwarded bounce readable, which matters because for most of this
/// system's life a forwarded bounce is the only bounce anybody will hand it.
fn locate_report(entity: &Entity, depth: usize) -> Option<ReportContainer> {
    if depth > MAX_DEPTH {
        return None;
    }
    let content = content_type(&entity.headers);
    let report_date = header_of(&entity.headers, "date").and_then(parse_date);

    if content.mime == "message/delivery-status" && is_plain_encoding(&entity.headers) {
        let status_part = parse_status_part(&entity.body);
        return Some(ReportContainer {
            status_part,
            returned_headers: None,
            report_date,
        });
    }

    if content.mime == "multipart/report" {
        let report_type = content.param("report-type");
        if report_type.is_none_or(|value| value.eq_ignore_ascii_case("delivery-status")) {
            if let Some(boundary) = content.param("boundary") {
                let parts: Vec<Entity> = split_parts(&entity.body, boundary)
                    .iter()
                    .map(|part| parse_message(part))
                    .collect();
                let status_part = parts.iter().find(|part| {
                    content_type(&part.headers).mime == "message/delivery-status"
                        && is_plain_encoding(&part.headers)
                });
                if let Some(status) = status_part {
                    let returned = parts
                        .iter()
                        .find(|part| {
                            matches!(
                                content_type(&part.headers).mime.as_str(),
                                "message/rfc822" | "text/rfc822-headers"
                            ) && is_plain_encoding(&part.headers)
                        })
                        .map(|part| parse_message(&part.body).headers);
                    return Some(ReportContainer {
                        status_part: parse_status_part(&status.body),
                        returned_headers: returned,
                        report_date,
                    });
                }
            }
        }
    }

    if content.mime.starts_with("multipart/") {
        if let Some(boundary) = content.param("boundary") {
            for part in split_parts(&entity.body, boundary) {
                let child = parse_message(&part);
                if let Some(found) = locate_report(&child, depth + 1) {
                    return Some(ReportContainer {
                        report_date: found.report_date.or(report_date),
                        ..found
                    });
                }
            }
        }
    }

    if content.mime == "message/rfc822" && is_plain_encoding(&entity.headers) {
        let inner = parse_message(&entity.body);
        if let Some(found) = locate_report(&inner, depth + 1) {
            return Some(ReportContainer {
                report_date: found.report_date.or(report_date),
                ..found
            });
        }
    }

    None
}

/// Parse a `message/delivery-status` body into per-message and per-recipient
/// blocks.
///
/// RFC 3464 separates the blocks with blank lines and puts the per-message
/// block first. Rather than trusting position, a block is a **recipient** block
/// exactly when it carries a `Final-Recipient:`, an `Original-Recipient:` or an
/// `Action:`; everything else folds into the per-message fields. Positional
/// parsing breaks on the real-world reports that emit an empty leading block,
/// and breaking there would silently drop every recipient in the report.
fn parse_status_part(body: &str) -> StatusPart {
    let mut per_message: Vec<(String, String)> = Vec::new();
    let mut recipients = Vec::new();

    for block in body.split("\n\n") {
        if block.trim().is_empty() {
            continue;
        }
        let fields = parse_fields(block);
        let is_recipient = fields.iter().any(|(name, _)| {
            matches!(
                name.as_str(),
                "final-recipient" | "original-recipient" | "action"
            )
        });
        if !is_recipient {
            per_message.extend(fields);
            continue;
        }
        if recipients.len() >= MAX_PARTS {
            break;
        }
        recipients.push(RecipientReport {
            final_recipient: field_of(&fields, "final-recipient").and_then(strip_addr_type),
            original_recipient: field_of(&fields, "original-recipient").and_then(strip_addr_type),
            action: field_of(&fields, "action")
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty()),
            status: field_of(&fields, "status")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            diagnostic_code: field_of(&fields, "diagnostic-code")
                .map(str::to_string)
                .filter(|value| !value.is_empty()),
            last_attempt: field_of(&fields, "last-attempt-date").and_then(parse_date),
            remote_mta: field_of(&fields, "remote-mta").map(str::to_string),
        });
    }

    StatusPart {
        per_message,
        recipients,
    }
}

/// `rfc822; user@example.test` becomes `user@example.test`.
///
/// A non-`rfc822` address type yields `None`. RFC 3464 allows `x-` types and
/// local mailbox names that are not email addresses at all, and treating one as
/// an email identity would file a bounce against a string that is not a
/// mailbox — which the ledger would then hand to the suppression register.
fn strip_addr_type(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.split_once(';') {
        Some((kind, address)) => {
            if !kind.trim().eq_ignore_ascii_case("rfc822") {
                return None;
            }
            let address = address.trim();
            (!address.is_empty()).then(|| address.to_string())
        },
        // No type prefix at all: real MTAs omit it. Taken as an address, which
        // the caller then normalises and validates before anything is recorded.
        None => Some(trimmed.to_string()),
    }
}

/// Parse an RFC 5322 date, then an RFC 3339 one.
///
/// `None` rather than a substituted clock. See [`super`] for why an invented
/// `observed_at` is worse than no receipt: it breaks the ledger's replay
/// contract, so the same bounce posted twice becomes an error instead of a
/// resumption.
fn parse_date(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim();
    // A trailing `(GMT)`-style comment is legal and chrono does not take it.
    let cleaned = match trimmed.split_once('(') {
        Some((before, _)) => before.trim(),
        None => trimmed,
    };
    DateTime::parse_from_rfc2822(cleaned)
        .or_else(|_| DateTime::parse_from_rfc3339(cleaned))
        .ok()
        .map(|stamped| stamped.with_timezone(&Utc))
}
