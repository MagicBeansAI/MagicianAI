//! Reading a bounce, as behaviour.
//!
//! Every test names the exact failure it pins. Two of them pin the failure that
//! motivated the whole module — a transient bounce read as permanent, and a
//! bounce attributed to the wrong send — because both end in an address nobody
//! can ever write to again, and neither is visible from the outside once it has
//! happened.

use chrono::{DateTime, TimeZone, Utc};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::intake::{IntakeAttribution, ReceiptIntake, ReceiptSource};
use crate::magician_v2::delivery::{
    DeliveryKnowledge, DeliveryLedger, DeliveryScope, DeliveryState, DispatchedAct,
    SuppressionCause,
};

use super::dsn::{classify, read_dsn, DsnRecognition, ReadingFault};
use super::*;

const SOFT: DeliveryState = DeliveryState::Bounced { hard: false };
const HARD: DeliveryState = DeliveryState::Bounced { hard: true };

fn t(hour: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, hour, 0, 0)
        .single()
        .expect("a real instant")
}

fn scope() -> DeliveryScope {
    DeliveryScope::new("alpha", "prod")
}

// ── Fixtures ────────────────────────────────────────────────────────────────

/// A conforming RFC 3464 bounce, with the pieces every assertion below varies.
fn bounce(status: &str, action: &str, recipient: &str, original_message_id: &str) -> String {
    format!(
        "From: MAILER-DAEMON@mx.example.test\n\
         To: agent@example.com\n\
         Subject: Delivery Status Notification (Failure)\n\
         Date: Fri, 21 Aug 2026 11:00:00 +0000\n\
         Message-ID: <dsn-9001@mx.example.test>\n\
         Content-Type: multipart/report; report-type=delivery-status; boundary=\"B1\"\n\
         \n\
         --B1\n\
         Content-Type: text/plain; charset=utf-8\n\
         \n\
         Your message could not be delivered.\n\
         \n\
         --B1\n\
         Content-Type: message/delivery-status\n\
         \n\
         Reporting-MTA: dns; mx.example.test\n\
         Arrival-Date: Fri, 21 Aug 2026 10:30:00 +0000\n\
         \n\
         Final-Recipient: rfc822; {recipient}\n\
         Action: {action}\n\
         Status: {status}\n\
         Remote-MTA: dns; mx.recipient.test\n\
         Diagnostic-Code: smtp; 550 5.1.1 <{recipient}>: Recipient address rejected\n\
         Last-Attempt-Date: Fri, 21 Aug 2026 10:45:00 +0000\n\
         \n\
         --B1\n\
         Content-Type: text/rfc822-headers\n\
         \n\
         Message-ID: <{original_message_id}>\n\
         From: agent@example.com\n\
         To: {recipient}\n\
         Subject: The original message\n\
         \n\
         --B1--\n"
    )
}

fn recognised(raw: &str) -> DsnReport {
    match read_dsn(raw) {
        DsnRecognition::Recognised(report) => report,
        other => panic!("expected a recognised DSN, got {other:?}"),
    }
}

fn sent(act_ref: &str, audience: &[&str]) -> SentMessage {
    SentMessage {
        act_ref: act_ref.to_string(),
        audience: audience.iter().map(|value| value.to_string()).collect(),
        sent_at: t(9),
    }
}

fn bound(
    identifier: SendIdentifier,
    send: SentMessage,
) -> std::collections::BTreeMap<SendIdentifier, SendBinding> {
    let mut map = std::collections::BTreeMap::new();
    map.insert(identifier, SendBinding::Bound(send));
    map
}

// ── The status class, which is the whole ball game ──────────────────────────

/// The action/class table, cell by cell.
///
/// Pins: **a `4.x.x` never becomes a hard bounce and a `5.x.x` never becomes a
/// soft one.** Getting that pair backwards suppresses somebody whose mailbox
/// was briefly full, permanently — `SuppressionReason::HardBounce` is what the
/// hygiene sweep records and only an explicit owner act with evidence lifts it.
/// It also pins that `relayed` and `expanded` are `Accepted` and never
/// `Delivered`: neither confirms anybody saw anything, and reading them as
/// arrival would let the disclosure register believe a person was reached.
#[test]
fn the_action_and_status_class_decide_the_state_and_nothing_else_does() {
    let table: [(&str, u8, Result<DeliveryState, &str>); 15] = [
        ("failed", 5, Ok(HARD)),
        ("failed", 4, Ok(SOFT)),
        ("failed", 2, Err("contradictory_report")),
        ("delayed", 4, Ok(SOFT)),
        ("delayed", 5, Err("contradictory_report")),
        ("delayed", 2, Err("contradictory_report")),
        ("delivered", 2, Ok(DeliveryState::Delivered)),
        ("delivered", 4, Err("contradictory_report")),
        ("delivered", 5, Err("contradictory_report")),
        ("relayed", 2, Ok(DeliveryState::Accepted)),
        ("relayed", 4, Err("contradictory_report")),
        ("relayed", 5, Err("contradictory_report")),
        ("expanded", 2, Ok(DeliveryState::Accepted)),
        ("expanded", 4, Err("contradictory_report")),
        ("expanded", 5, Err("contradictory_report")),
    ];

    for (action, class, expected) in table {
        let status = format!("{class}.1.1");
        let got = classify(Some(action), Some(&status));
        match (got, expected) {
            (Ok(state), Ok(wanted)) => assert_eq!(
                state,
                wanted,
                "`Action: {action}` with `Status: {status}` must be `{}`",
                wanted.as_str()
            ),
            (Err(fault), Err(wanted)) => assert_eq!(
                fault.as_str(),
                wanted,
                "`Action: {action}` with `Status: {status}`"
            ),
            (got, expected) => {
                panic!(
                    "`Action: {action}` with `Status: {status}`: got {got:?}, wanted {expected:?}"
                )
            },
        }
    }
}

/// The consequence of the row above, stated in the currency that matters.
///
/// Pins: a transient bounce carries **no** suppression cause and a permanent
/// one carries `hard_bounce`. The table test proves the mapping; this proves
/// the mapping is the thing that decides whether somebody is deleted from the
/// sendable pool, so a future edit cannot "simplify" soft and hard into one
/// state without this failing.
#[test]
fn a_transient_bounce_suppresses_nobody_and_a_permanent_one_suppresses_exactly_one_cause() {
    let transient = classify(Some("failed"), Some("4.2.2")).expect("a soft bounce");
    assert_eq!(transient, SOFT);
    assert_eq!(transient.suppression_cause(), None);

    let permanent = classify(Some("failed"), Some("5.1.1")).expect("a hard bounce");
    assert_eq!(permanent, HARD);
    assert_eq!(
        permanent.suppression_cause(),
        Some(SuppressionCause::HardBounce)
    );
}

/// Pins: an unreadable status yields **no receipt**, and `Diagnostic-Code:` is
/// never mined for a substitute.
///
/// The fixture's diagnostic code literally contains `550 5.1.1`. A reader that
/// fell back to it would call this a hard bounce and suppress the address on
/// the strength of a remote server's free-text prose.
#[test]
fn a_missing_status_is_never_recovered_from_the_diagnostic_code() {
    let fault = classify(Some("failed"), None).expect_err("no status is no receipt");
    assert_eq!(fault.as_str(), "unreadable_status");
    assert_eq!(fault, ReadingFault::UnreadableStatus { raw: None });

    // And the same through the whole reader, where the diagnostic code is
    // present and carries a plausible-looking permanent code.
    let raw = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    )
    .replace("Status: 5.1.1\n", "");
    let report = recognised(&raw);
    let recipient = &report.recipients[0];
    assert_eq!(recipient.status, None);
    assert!(recipient
        .diagnostic_code
        .as_deref()
        .expect("the fixture carries one")
        .contains("550 5.1.1"));

    let harvest = correlate(
        "agentmail",
        "payload://bounce-1",
        &report,
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");
    assert_eq!(harvest.intents.len(), 0);
    assert_eq!(harvest.refused.len(), 1);
    assert_eq!(harvest.refused[0].reason, RefusalReason::UnreadableStatus);
}

/// Pins: a status class RFC 3463 does not define is refused, not rounded.
#[test]
fn a_status_class_outside_two_four_and_five_yields_no_receipt() {
    for status in ["3.1.1", "9.9.9", "0.0.0", "5.1", "5.1.1.1", "five.one.one"] {
        let fault =
            classify(Some("failed"), Some(status)).expect_err("only classes 2, 4 and 5 exist");
        assert_eq!(
            fault.as_str(),
            "unreadable_status",
            "`Status: {status}` must be unreadable rather than rounded to a neighbour"
        );
    }
}

/// Pins: an `Action:` token this build does not know produces a named fault,
/// never an inherited default.
#[test]
fn an_unknown_action_token_yields_no_receipt() {
    let fault = classify(Some("quarantined"), Some("5.1.1")).expect_err("not an RFC 3464 action");
    assert_eq!(
        fault,
        ReadingFault::UnknownAction {
            token: "quarantined".to_string()
        }
    );
}

// ── Recognition, which must never be lexical ────────────────────────────────

/// Pins: **a subject line is never evidence of a bounce.**
///
/// This is a real human writing about a delivery that failed. Every word a
/// naive matcher looks for is present. Recognising it would file a hard bounce
/// against a correspondent on the strength of their own prose, and a hard
/// bounce is not operationally liftable.
#[test]
fn a_human_message_about_a_failed_delivery_is_not_a_bounce() {
    let raw = "From: dana@customer.test\n\
               To: agent@example.com\n\
               Subject: Undeliverable: your invoice was returned to sender\n\
               Date: Fri, 21 Aug 2026 11:00:00 +0000\n\
               Content-Type: text/plain\n\
               \n\
               Hi — mail delivery failed for the invoice you sent, Action: failed,\n\
               status 5.1.1 apparently. Can you resend it to my new address?\n";
    match read_dsn(raw) {
        DsnRecognition::NotADsn { because } => {
            assert!(
                because.contains("multipart/report"),
                "the refusal must name the structural test that was applied, got: {because}"
            );
        },
        other => panic!("a human message must never be recognised as a bounce: {other:?}"),
    }
}

/// Pins: a read receipt is a report and is not a bounce.
///
/// `multipart/report; report-type=disposition-notification` shares the
/// container with a DSN. A recogniser that stopped at `multipart/report` would
/// read an MDN's fields as delivery status.
#[test]
fn a_read_receipt_is_not_a_bounce() {
    let raw = "From: dana@customer.test\n\
               To: agent@example.com\n\
               Subject: Read: your message\n\
               Content-Type: multipart/report; report-type=disposition-notification; boundary=\"M\"\n\
               \n\
               --M\n\
               Content-Type: message/disposition-notification\n\
               \n\
               Final-Recipient: rfc822; dana@customer.test\n\
               Disposition: automatic-action/MDN-sent-automatically; displayed\n\
               \n\
               --M--\n";
    assert!(
        matches!(read_dsn(raw), DsnRecognition::NotADsn { .. }),
        "an MDN must not be read as a delivery status notification"
    );
}

/// Pins: the structural read gets every field right, and the returned original
/// `Message-ID` is unwrapped to the same form the send index stores.
///
/// A reader that kept the angle brackets on one side and not the other would
/// correlate nothing, and every bounce would come back `unknown_send` — which
/// looks exactly like a rail that never bounces.
#[test]
fn a_conforming_bounce_is_read_field_by_field() {
    let report = recognised(&bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    ));

    assert_eq!(
        report.report_message_id.as_deref(),
        Some("dsn-9001@mx.example.test")
    );
    assert_eq!(
        report.original_message_id.as_deref(),
        Some("orig-1@agentmail.to")
    );
    assert_eq!(
        report.reporting_mta.as_deref(),
        Some("dns; mx.example.test")
    );
    assert_eq!(
        report.arrival_date,
        Utc.with_ymd_and_hms(2026, 8, 21, 10, 30, 0).single()
    );
    assert_eq!(report.recipients.len(), 1);

    let recipient = &report.recipients[0];
    assert_eq!(
        recipient.final_recipient.as_deref(),
        Some("gone@recipient.test")
    );
    assert_eq!(recipient.action.as_deref(), Some("failed"));
    assert_eq!(recipient.status.as_deref(), Some("5.1.1"));
    assert_eq!(
        recipient.last_attempt,
        Utc.with_ymd_and_hms(2026, 8, 21, 10, 45, 0).single()
    );
    assert_eq!(
        report.recipients[0].remote_mta.as_deref(),
        Some("dns; mx.recipient.test")
    );
}

/// Pins: a bounce a human forwarded out of their own client is still read.
///
/// For as long as no provider emits delivery events, a forwarded bounce is the
/// only bounce anybody will hand this system. A reader that only looked at the
/// top-level `Content-Type` would answer `NotADsn` for every one of them.
#[test]
fn a_bounce_forwarded_as_an_attachment_is_still_read() {
    let inner = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    );
    let forwarded = format!(
        "From: owner@company.test\n\
         To: agent@example.com\n\
         Subject: Fwd: this came back\n\
         Date: Fri, 21 Aug 2026 12:00:00 +0000\n\
         Content-Type: multipart/mixed; boundary=\"OUTER\"\n\
         \n\
         --OUTER\n\
         Content-Type: text/plain\n\
         \n\
         Forwarding this, looks like a bounce.\n\
         \n\
         --OUTER\n\
         Content-Type: message/rfc822\n\
         \n\
         {inner}\n\
         --OUTER--\n"
    );

    let report = recognised(&forwarded);
    assert_eq!(
        report.original_message_id.as_deref(),
        Some("orig-1@agentmail.to")
    );
    assert_eq!(report.recipients.len(), 1);
    assert_eq!(report.recipients[0].status.as_deref(), Some("5.1.1"));
}

/// Pins: a report that announces itself and cannot be read is `Unreadable`, not
/// `NotADsn`.
///
/// Folding the two together would let unreadable bounces accumulate as
/// "ordinary mail", which is the silent blindness this whole tier exists to
/// remove: an unread bounce and a clean send look identical from the outside.
#[test]
fn a_delivery_report_with_no_recipient_block_is_unreadable_not_ordinary_mail() {
    let raw = "From: MAILER-DAEMON@mx.example.test\n\
               To: agent@example.com\n\
               Content-Type: multipart/report; report-type=delivery-status; boundary=\"B\"\n\
               \n\
               --B\n\
               Content-Type: message/delivery-status\n\
               \n\
               Reporting-MTA: dns; mx.example.test\n\
               \n\
               --B--\n";
    match read_dsn(raw) {
        DsnRecognition::Unreadable { because } => {
            assert!(because.contains("per-recipient"), "got: {because}");
        },
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

// ── Correlation, which is where a wrong answer is unrecoverable ─────────────

/// Pins: a bounce quoting a `Message-ID` this scope never sent records
/// **nothing**.
///
/// The alternative — matching on the recipient address — would attribute this
/// to whichever act last wrote to that address, and permanently suppress
/// somebody on evidence about a different message.
#[test]
fn a_bounce_for_a_send_we_have_no_record_of_records_nothing() {
    let report = recognised(&bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "somebody-elses@mail.test",
    ));
    let harvest = correlate(
        "agentmail",
        "payload://bounce-1",
        &report,
        // The index holds a real send — for a DIFFERENT message. A test against
        // an empty map would pass for the wrong reason.
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");

    assert_eq!(harvest.intents.len(), 0);
    assert_eq!(harvest.correlated_on, None);
    assert_eq!(harvest.refused.len(), 1);
    assert_eq!(harvest.refused[0].reason, RefusalReason::UnknownSend);
    assert_eq!(
        harvest.refused[0].final_recipient.as_deref(),
        Some("gone@recipient.test")
    );
}

/// Pins: a bounce carrying no identifier we minted records **nothing**, even
/// when the address it names is one we really did write to.
///
/// This is the tempting case. The address matches a send exactly, and matching
/// on it would be right most of the time — and would, the rest of the time,
/// suppress a working address because of a bounce from an unrelated act.
#[test]
fn a_bounce_with_no_returned_identifier_records_nothing_even_when_the_address_matches() {
    let raw = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    );
    // Strip the returned-headers part entirely: many MTAs return only the body.
    let stripped = raw
        .split("--B1\nContent-Type: text/rfc822-headers")
        .next()
        .expect("the fixture splits")
        .to_string()
        + "--B1--\n";

    let report = recognised(&stripped);
    assert_eq!(report.original_message_id, None);
    assert_eq!(report.original_envelope_id, None);

    let harvest = correlate(
        "agentmail",
        "payload://bounce-1",
        &report,
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");

    assert_eq!(harvest.intents.len(), 0);
    assert_eq!(
        harvest.refused[0].reason,
        RefusalReason::NoReturnedIdentifier
    );
    assert!(
        harvest.refused[0]
            .detail
            .contains("recipient address alone"),
        "the refusal must say why address matching is not used: {}",
        harvest.refused[0].detail
    );
}

/// Pins: a report about an address the correlated act never wrote to records
/// **nothing**.
///
/// A hostile or broken MTA can quote a real `Message-ID` beside any
/// `Final-Recipient` it likes. Without this check that is a remote-controlled
/// permanent suppression of any address.
#[test]
fn a_bounce_about_somebody_outside_the_send_records_nothing() {
    let report = recognised(&bounce(
        "5.1.1",
        "failed",
        "victim@elsewhere.test",
        "orig-1@agentmail.to",
    ));
    let harvest = correlate(
        "agentmail",
        "payload://bounce-1",
        &report,
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");

    assert_eq!(harvest.intents.len(), 0);
    assert_eq!(harvest.refused[0].reason, RefusalReason::RecipientNotInSend);
}

/// Pins: when a forwarding chain dies downstream, the receipt is filed against
/// the address **we** wrote to, not the stranger at the end of the chain.
///
/// Suppressing the downstream address would be useless — we never write there —
/// and would leave the real, now-dead, address in the sendable pool.
#[test]
fn a_forwarding_chain_bounce_is_filed_against_the_address_we_addressed() {
    let raw = bounce(
        "5.1.1",
        "failed",
        "downstream@forwarder.test",
        "orig-1@agentmail.to",
    )
    .replace(
        "Final-Recipient: rfc822; downstream@forwarder.test\n",
        "Original-Recipient: rfc822; Gone@Recipient.Test\n\
         Final-Recipient: rfc822; downstream@forwarder.test\n",
    );

    let report = recognised(&raw);
    let harvest = correlate(
        "agentmail",
        "payload://bounce-1",
        &report,
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");

    assert_eq!(harvest.intents.len(), 1);
    // Normalised, and the address from the audience — not the forwarder's.
    assert_eq!(harvest.intents[0].receipt.identity, "gone@recipient.test");
    assert_eq!(harvest.intents[0].receipt.state, HARD);
}

/// Pins: an identifier bound to two acts records **nothing**.
///
/// Choosing one would choose whose address a hard bounce suppresses, on a
/// coin-flip.
#[test]
fn an_identifier_bound_to_two_acts_records_nothing() {
    let report = recognised(&bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    ));
    let mut sends = std::collections::BTreeMap::new();
    sends.insert(
        SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
        SendBinding::Ambiguous {
            act_refs: vec!["act-1".to_string(), "act-2".to_string()],
        },
    );

    let harvest = correlate("agentmail", "payload://bounce-1", &report, &sends)
        .expect("the caller's fields are fine");
    assert_eq!(harvest.intents.len(), 0);
    assert_eq!(harvest.refused[0].reason, RefusalReason::AmbiguousSend);
    assert!(harvest.refused[0].detail.contains("act-1"));
    assert!(harvest.refused[0].detail.contains("act-2"));
}

/// Pins: an envelope id outranks a returned `Message-ID` when both are present,
/// and the correlation says which one it leaned on.
///
/// A disputed suppression is reviewed by reading this field, so it must be the
/// truth rather than whichever branch happened to run first.
#[test]
fn an_envelope_id_outranks_a_returned_message_id() {
    let raw = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    )
    .replace(
        "Reporting-MTA: dns; mx.example.test\n",
        "Reporting-MTA: dns; mx.example.test\nOriginal-Envelope-Id: env-77\n",
    );
    let report = recognised(&raw);
    assert_eq!(report.original_envelope_id.as_deref(), Some("env-77"));

    let mut sends = bound(
        SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
        sent("act-by-message-id", &["gone@recipient.test"]),
    );
    sends.insert(
        SendIdentifier::EnvelopeId("env-77".to_string()),
        SendBinding::Bound(sent("act-by-envelope-id", &["gone@recipient.test"])),
    );

    let harvest = correlate("agentmail", "payload://bounce-1", &report, &sends)
        .expect("the caller's fields are fine");
    assert_eq!(harvest.intents.len(), 1);
    assert_eq!(harvest.intents[0].act_ref, "act-by-envelope-id");
    assert_eq!(harvest.intents[0].correlation, Correlation::EnvelopeId);
    assert_eq!(
        harvest.correlated_on,
        Some(SendIdentifier::EnvelopeId("env-77".to_string()))
    );
}

/// Pins: one report about two recipients yields two receipts under **distinct**
/// provider message ids.
///
/// The ledger holds one provider message to one identity's story and refuses a
/// second identity under one id. Sharing the DSN's own `Message-ID` across both
/// recipients would make the second recipient's bounce a hard error, and the
/// dead address would stay in the pool.
#[test]
fn one_report_about_two_recipients_yields_two_distinct_provider_messages() {
    let raw = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    )
    .replace(
        "--B1\nContent-Type: text/rfc822-headers\n",
        "Final-Recipient: rfc822; full@recipient.test\n\
             Action: failed\n\
             Status: 4.2.2\n\
             \n\
             --B1\n\
             Content-Type: text/rfc822-headers\n",
    );

    let report = recognised(&raw);
    assert_eq!(report.recipients.len(), 2);

    let harvest = correlate(
        "agentmail",
        "payload://bounce-1",
        &report,
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test", "full@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");

    assert_eq!(harvest.intents.len(), 2);
    assert_eq!(harvest.recipients_reported, 2);
    let ids: Vec<&str> = harvest
        .intents
        .iter()
        .map(|intent| intent.receipt.provider_message_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "dsn:dsn-9001@mx.example.test:gone@recipient.test",
            "dsn:dsn-9001@mx.example.test:full@recipient.test",
        ]
    );
    // And the two states are the two different facts the report carried.
    assert_eq!(harvest.intents[0].receipt.state, HARD);
    assert_eq!(harvest.intents[1].receipt.state, SOFT);
}

/// Pins: the same bounce read twice derives the same provider message id and
/// the same provider clock.
///
/// Both feed the ledger's replay contract. A clock taken from `Utc::now()`
/// instead of the report would turn a harmless re-post into "a changed payload
/// under one id", which is an error the ledger raises rather than swallows.
#[test]
fn the_same_bounce_read_twice_derives_the_same_receipt() {
    let raw = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    );
    let sends = bound(
        SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
        sent("act-1", &["gone@recipient.test"]),
    );

    let first = correlate("agentmail", "payload://b", &recognised(&raw), &sends).expect("fine");
    let second = correlate("agentmail", "payload://b", &recognised(&raw), &sends).expect("fine");
    assert_eq!(first.intents, second.intents);
    assert_eq!(
        first.intents[0].receipt.observed_at,
        Utc.with_ymd_and_hms(2026, 8, 21, 10, 45, 0)
            .single()
            .unwrap(),
        "the provider's Last-Attempt-Date, not our clock"
    );
}

/// Pins: a report with no parseable clock anywhere records **nothing**.
#[test]
fn a_report_with_no_readable_clock_records_nothing() {
    let raw = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    )
    .replace("Date: Fri, 21 Aug 2026 11:00:00 +0000\n", "")
    .replace("Arrival-Date: Fri, 21 Aug 2026 10:30:00 +0000\n", "")
    .replace("Last-Attempt-Date: Fri, 21 Aug 2026 10:45:00 +0000\n", "");

    let harvest = correlate(
        "agentmail",
        "payload://b",
        &recognised(&raw),
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");
    assert_eq!(harvest.intents.len(), 0);
    assert_eq!(harvest.refused[0].reason, RefusalReason::NoObservedAt);
}

/// Pins: a caller string carrying the id separator is refused at this
/// boundary, by name, rather than surfacing as a ledger error three layers down.
#[test]
fn a_caller_field_carrying_the_unit_separator_is_refused() {
    let report = recognised(&bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    ));
    let sends = bound(
        SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
        sent("act-1", &["gone@recipient.test"]),
    );

    let error = correlate("agent\u{1f}mail", "payload://b", &report, &sends)
        .expect_err("a provider name carrying U+001F is refused");
    assert!(format!("{error:#}").contains("U+001F"));

    let error = correlate("agentmail", "payload\u{1f}ref", &report, &sends)
        .expect_err("a payload ref carrying U+001F is refused");
    assert!(format!("{error:#}").contains("U+001F"));
}

/// Pins: a hostile `Message-ID` never reaches a derived id.
///
/// The report's own `Message-ID` is attacker-controlled. One carrying the unit
/// separator could shear a receipt id's components apart and resume another
/// act's record, so the anchor falls back to a fingerprint this side computes.
#[test]
fn a_report_message_id_carrying_the_separator_falls_back_to_a_fingerprint() {
    let raw = bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    )
    .replace(
        "Message-ID: <dsn-9001@mx.example.test>",
        "Message-ID: <dsn\u{1f}9001@mx.example.test>",
    );
    let report = recognised(&raw);
    let harvest = correlate(
        "agentmail",
        "payload://b",
        &report,
        &bound(
            SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            sent("act-1", &["gone@recipient.test"]),
        ),
    )
    .expect("the caller's fields are fine");

    let derived = &harvest.intents[0].receipt.provider_message_id;
    assert!(
        !derived.contains('\u{1f}'),
        "the derived id must not carry the separator: {derived}"
    );
    assert_eq!(
        *derived,
        format!("dsn:{}:gone@recipient.test", report.fingerprint)
    );
}

// ── The send index ──────────────────────────────────────────────────────────

fn index() -> (tempfile::TempDir, SentMessageIndex) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let index = SentMessageIndex::new(ArtifactV2Workspace::new(tmp.path()));
    (tmp, index)
}

/// Pins: replay resumes, and a changed account of one message is an error.
///
/// Two accounts of one identifier decide which act a later bounce lands on.
/// Quietly keeping the first would hide the reuse until a hard bounce was
/// attributed to the wrong disclosure.
#[test]
fn registering_the_same_send_twice_resumes_and_a_changed_one_is_refused() {
    let (_tmp, index) = index();
    let identifier = SendIdentifier::MessageId("orig-1@agentmail.to".to_string());
    let send = sent("act-1", &["Gone@Recipient.Test"]);

    index
        .record(&scope(), "agentmail", &identifier, &send, t(10))
        .expect("the first registration records");
    index
        .record(&scope(), "agentmail", &identifier, &send, t(11))
        .expect("the identical registration resumes");

    match index
        .lookup(&scope(), "agentmail", &identifier)
        .expect("the index reads")
    {
        SendBinding::Bound(found) => {
            assert_eq!(found.act_ref, "act-1");
            // Normalised on the way in, so a bounce reporting `GONE@…` matches.
            assert_eq!(found.audience, vec!["gone@recipient.test".to_string()]);
        },
        other => panic!("expected one binding, got {other:?}"),
    }

    let error = index
        .record(
            &scope(),
            "agentmail",
            &identifier,
            &sent("act-2", &["Gone@Recipient.Test"]),
            t(12),
        )
        .expect_err("a second act under one identifier is refused");
    assert!(format!("{error:#}").contains("act-1"));
    assert!(format!("{error:#}").contains("act-2"));
}

/// Pins: an identifier nobody registered reads as `Absent`, which is not
/// permission and not an error.
#[test]
fn an_unregistered_identifier_reads_as_absent_beside_a_real_registration() {
    let (_tmp, index) = index();
    // A real row, so the assertion below cannot pass against an empty store.
    index
        .record(
            &scope(),
            "agentmail",
            &SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            &sent("act-1", &["gone@recipient.test"]),
            t(10),
        )
        .expect("the seed records");

    assert_eq!(
        index
            .lookup(
                &scope(),
                "agentmail",
                &SendIdentifier::MessageId("never-sent@agentmail.to".to_string())
            )
            .expect("the index reads"),
        SendBinding::Absent
    );
    // And the two namespaces do not bleed: the same string as an envelope id is
    // a different fact.
    assert_eq!(
        index
            .lookup(
                &scope(),
                "agentmail",
                &SendIdentifier::EnvelopeId("orig-1@agentmail.to".to_string())
            )
            .expect("the index reads"),
        SendBinding::Absent
    );
}

/// Pins: a send registered with no audience is refused.
///
/// Recorded empty, every reported address would fail the membership check and
/// every bounce for this send would be silently refused — which is safe and
/// looks exactly like a rail that never bounces.
#[test]
fn a_send_registered_with_no_audience_is_refused() {
    let (_tmp, index) = index();
    let error = index
        .record(
            &scope(),
            "agentmail",
            &SendIdentifier::MessageId("orig-1@agentmail.to".to_string()),
            &SentMessage {
                act_ref: "act-1".to_string(),
                audience: Vec::new(),
                sent_at: t(9),
            },
            t(10),
        )
        .expect_err("an empty audience is refused");
    assert!(format!("{error:#}").contains("never bounces"));
}

// ── The whole path, through the door that already exists ────────────────────

/// Pins: the intent this module hands back is **exactly** what the intake door
/// takes, and a hard bounce read out of mail ends up as a suppression signal.
///
/// This is the tier's own claim, end to end: a bounce arrives as mail, is
/// recognised structurally, is correlated on an identifier we minted, goes
/// through `ReceiptIntake::admit` unchanged, and the act stops being
/// `dispatch_unknown`. Nothing in it is stubbed and the store is seeded, so a
/// shape mismatch anywhere fails here rather than in production.
#[test]
fn a_bounce_read_from_mail_reconciles_through_the_intake_door() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let layout = ArtifactV2Workspace::new(tmp.path());
    let index = SentMessageIndex::new(layout.clone());
    let intake = ReceiptIntake::new(layout.clone());
    let ledger = DeliveryLedger::new(layout);
    let scope = scope();

    // What was sent, recorded at send time.
    let identifier = SendIdentifier::MessageId("orig-1@agentmail.to".to_string());
    index
        .record(
            &scope,
            "agentmail",
            &identifier,
            &sent("act-1", &["gone@recipient.test"]),
            t(9),
        )
        .expect("the send registers");

    // The bounce, as mail.
    let report = recognised(&bounce(
        "5.1.1",
        "failed",
        "gone@recipient.test",
        "orig-1@agentmail.to",
    ));
    let sends = sent_index::sends_for(&index, &scope, "agentmail", &identifiers_in(&report))
        .expect("the index reads");
    let harvest = correlate("agentmail", "payload://bounce-1", &report, &sends)
        .expect("the caller's fields are fine");
    assert_eq!(harvest.intents.len(), 1);
    assert_eq!(
        harvest.intents[0].correlation,
        Correlation::ReturnedMessageId
    );

    // Before: the act is dispatch_unknown, which is the state this whole tier
    // exists to make resolvable.
    assert_eq!(
        ledger.state_of(&scope, "act-1").expect("the ledger reads"),
        DeliveryKnowledge::DispatchUnknown
    );

    let dispatched = vec![DispatchedAct::new("act-1", t(9))];
    let admitted = intake
        .admit(
            &scope,
            &dispatched,
            &harvest.intents[0].act_ref,
            &harvest.intents[0].receipt,
            &IntakeAttribution {
                source: ReceiptSource::Provider,
                actor: "dsn-reader".to_string(),
                authentication: "test".to_string(),
            },
            t(12),
        )
        .expect("the intake admits a correlated bounce");

    assert_eq!(admitted.outcome.identity_state, HARD);
    assert_eq!(
        ledger.state_of(&scope, "act-1").expect("the ledger reads"),
        DeliveryKnowledge::Observed(HARD)
    );
    assert_eq!(
        ledger
            .suppression_signals(&scope, t(0))
            .expect("the signals read"),
        vec![(
            "gone@recipient.test".to_string(),
            SuppressionCause::HardBounce
        )]
    );
}

/// Pins: a **transient** bounce read from mail leaves the address contactable.
///
/// The mirror of the test above, and the one that matters more: a full mailbox
/// on Tuesday must not delete somebody from the sendable pool. The act stops
/// being `dispatch_unknown` — something did come back — and the suppression
/// register is offered nothing.
#[test]
fn a_transient_bounce_read_from_mail_suppresses_nobody() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let layout = ArtifactV2Workspace::new(tmp.path());
    let index = SentMessageIndex::new(layout.clone());
    let intake = ReceiptIntake::new(layout.clone());
    let ledger = DeliveryLedger::new(layout);
    let scope = scope();

    let identifier = SendIdentifier::MessageId("orig-2@agentmail.to".to_string());
    index
        .record(
            &scope,
            "agentmail",
            &identifier,
            &sent("act-2", &["busy@recipient.test"]),
            t(9),
        )
        .expect("the send registers");

    let report = recognised(&bounce(
        "4.2.2",
        "failed",
        "busy@recipient.test",
        "orig-2@agentmail.to",
    ));
    let sends = sent_index::sends_for(&index, &scope, "agentmail", &identifiers_in(&report))
        .expect("the index reads");
    let harvest = correlate("agentmail", "payload://bounce-2", &report, &sends)
        .expect("the caller's fields are fine");
    assert_eq!(harvest.intents[0].receipt.state, SOFT);

    intake
        .admit(
            &scope,
            &[DispatchedAct::new("act-2", t(9))],
            &harvest.intents[0].act_ref,
            &harvest.intents[0].receipt,
            &IntakeAttribution {
                source: ReceiptSource::Provider,
                actor: "dsn-reader".to_string(),
                authentication: "test".to_string(),
            },
            t(12),
        )
        .expect("the intake admits it");

    assert_eq!(
        ledger.state_of(&scope, "act-2").expect("the ledger reads"),
        DeliveryKnowledge::Observed(SOFT)
    );
    assert_eq!(
        ledger
            .suppression_signals(&scope, t(0))
            .expect("the signals read"),
        Vec::<(String, SuppressionCause)>::new(),
        "a 4.x.x status must never offer the suppression register anything"
    );
}
