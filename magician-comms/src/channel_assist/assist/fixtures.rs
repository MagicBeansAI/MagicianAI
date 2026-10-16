//! Fixture export for hand-labeling (design §4 — Phase-8 pull-forward,
//! first slice). Shapes `mail_threads` rows into labeled-ready JSONL rows
//! for the `magician channel-assist export-fixtures` CLI.
//!
//! Privacy contract of the exported row:
//! - thread id is HASHED (sha256, truncated) — never the raw provider id;
//! - subject is REAL (needed for labeling — the file is LOCAL-only and
//!   carries the same sensitivity as the mailbox metadata itself);
//! - sender is name + DOMAIN only — never the full address;
//! - NO recipient lists or recipient domains of any kind.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::types::{MailThreadRecord, MAIL_ASSIST_SCHEMA_VERSION};

/// Truncated-hex length of the hashed thread reference. 12 nibbles of
/// sha256 is plenty for collision-free joins across a few thousand rows
/// while staying visibly "not a Gmail id".
pub const FIXTURE_HASH_LEN: usize = 12;

const MILLIS_PER_DAY: i64 = 86_400_000;

/// One labeled-ready fixture row. Field selection is the privacy contract
/// — adding a field here means re-reviewing the module-header rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailFixtureRow {
    pub schema_version: u32,
    /// sha256(thread_id) truncated to [`FIXTURE_HASH_LEN`] hex chars.
    pub thread_ref: String,
    pub account: String,
    /// Real subject (or the redaction placeholder for suppressed rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    /// Domain of the latest sender — never the full address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_domain: Option<String>,
    #[serde(default)]
    pub label_ids: Vec<String>,
    pub message_count: i64,
    /// Whole days since the latest message (None if the thread has no
    /// message timestamp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_age_days: Option<i64>,
    /// Whole days since the sync first observed the thread.
    pub first_observed_age_days: i64,
    pub sensitive_suppressed: bool,
    /// Hand-label slot — always empty on export; the human labeler fills
    /// it and the labeled file becomes the Phase-2 classifier gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Truncated sha256 hex of an id — deterministic, so re-exports of the
/// same mailbox produce joinable references.
pub fn short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    digest[..FIXTURE_HASH_LEN].to_string()
}

/// Domain of an email address (lowercased); None when the input has no
/// `@` or an empty domain part.
pub fn sender_domain(address: Option<&str>) -> Option<String> {
    let address = address?.trim().trim_end_matches('>');
    let (_, domain) = address.rsplit_once('@')?;
    let domain = domain.trim().to_ascii_lowercase();
    (!domain.is_empty()).then_some(domain)
}

fn age_days(now_ms: i64, then_ms: i64) -> i64 {
    (now_ms - then_ms).max(0) / MILLIS_PER_DAY
}

/// Shape one thread row into its fixture form (the ONLY place the field
/// selection happens — keep it total so nothing else leaks in).
pub fn fixture_row(record: &MailThreadRecord, now_ms: i64) -> MailFixtureRow {
    MailFixtureRow {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        thread_ref: short_hash(&record.thread_id),
        account: record.account_alias.clone(),
        subject: record.subject.clone(),
        sender_name: record.latest_from_name.clone(),
        sender_domain: sender_domain(record.latest_from_address.as_deref()),
        label_ids: record.label_ids.clone(),
        message_count: record.message_count,
        last_message_age_days: record.last_message_at.map(|ts| age_days(now_ms, ts)),
        first_observed_age_days: age_days(now_ms, record.first_observed_at),
        sensitive_suppressed: record.sensitive_suppressed,
        label: None,
    }
}

/// Render rows as JSONL (one compact JSON object per line, trailing
/// newline included when non-empty).
pub fn render_jsonl(rows: &[MailFixtureRow]) -> Result<String> {
    let mut out = String::new();
    for row in rows {
        out.push_str(&serde_json::to_string(row).context("serializing mail fixture row")?);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::channel_assist::types::{ChannelLane, MailRecordOrigin};

    fn sample_record() -> MailThreadRecord {
        MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            account_email: Some("owner@example.com".to_string()),
            thread_id: "thread-abc-123".to_string(),
            lane: ChannelLane::UserAssist,
            subject: Some("Quarterly sync notes".to_string()),
            latest_summary: None,
            latest_from_name: Some("Sender One".to_string()),
            latest_from_address: Some("sender-one@Partner.Example".to_string()),
            recipient_domains: vec!["secret-recipient.example".to_string()],
            label_ids: vec!["INBOX".to_string(), "IMPORTANT".to_string()],
            message_count: 4,
            last_message_at: Some(1_000 * MILLIS_PER_DAY),
            provider_cursor: Some("h1".to_string()),
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
            first_observed_at: 998 * MILLIS_PER_DAY,
            last_observed_at: 1_002 * MILLIS_PER_DAY,
        }
    }

    #[test]
    fn short_hash_is_deterministic_truncated_and_id_hiding() {
        let a = short_hash("thread-abc-123");
        assert_eq!(a.len(), FIXTURE_HASH_LEN);
        assert_eq!(a, short_hash("thread-abc-123"));
        assert_ne!(a, short_hash("thread-abc-124"));
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!a.contains("thread"));
    }

    #[test]
    fn sender_domain_extracts_lowercased_domain_only() {
        assert_eq!(
            sender_domain(Some("sender-one@Partner.Example")),
            Some("partner.example".to_string())
        );
        // Defensive: angle-bracket remnants from display-form addresses.
        assert_eq!(
            sender_domain(Some("sender-one@partner.example>")),
            Some("partner.example".to_string())
        );
        assert_eq!(sender_domain(Some("no-at-sign")), None);
        assert_eq!(sender_domain(Some("dangling@")), None);
        assert_eq!(sender_domain(None), None);
    }

    #[test]
    fn fixture_row_hashes_id_keeps_subject_and_drops_recipients_and_address() {
        let record = sample_record();
        let row = fixture_row(&record, 1_005 * MILLIS_PER_DAY);

        assert_eq!(row.thread_ref, short_hash("thread-abc-123"));
        assert_eq!(row.subject.as_deref(), Some("Quarterly sync notes"));
        assert_eq!(row.sender_name.as_deref(), Some("Sender One"));
        assert_eq!(row.sender_domain.as_deref(), Some("partner.example"));
        assert_eq!(row.label_ids, vec!["INBOX", "IMPORTANT"]);
        assert_eq!(row.message_count, 4);
        assert_eq!(row.last_message_age_days, Some(5));
        assert_eq!(row.first_observed_age_days, 7);
        assert_eq!(row.label, None);

        // The serialized row must never carry the raw thread id, the full
        // sender address, or ANY recipient data — the privacy contract.
        let json = serde_json::to_string(&row).unwrap();
        assert!(!json.contains("thread-abc-123"));
        assert!(!json.contains("sender-one@"));
        assert!(!json.contains("recipient"));
        assert!(!json.contains("secret-recipient.example"));
        assert!(!json.contains("owner@example.com"));
    }

    #[test]
    fn ages_clamp_to_zero_and_missing_last_message_stays_none() {
        let mut record = sample_record();
        record.last_message_at = None;
        // A first_observed_at in the "future" (clock skew) clamps to 0.
        record.first_observed_at = 2_000 * MILLIS_PER_DAY;
        let row = fixture_row(&record, 1_005 * MILLIS_PER_DAY);
        assert_eq!(row.last_message_age_days, None);
        assert_eq!(row.first_observed_age_days, 0);
    }

    #[test]
    fn render_jsonl_emits_one_compact_line_per_row() {
        let record = sample_record();
        let rows = vec![
            fixture_row(&record, 1_005 * MILLIS_PER_DAY),
            fixture_row(&record, 1_006 * MILLIS_PER_DAY),
        ];
        let jsonl = render_jsonl(&rows).unwrap();
        let lines: Vec<&str> = jsonl.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let parsed: MailFixtureRow = serde_json::from_str(line).unwrap();
            assert_eq!(parsed.thread_ref, short_hash("thread-abc-123"));
        }
        assert!(jsonl.ends_with('\n'));
        assert!(render_jsonl(&[]).unwrap().is_empty());
    }
}
