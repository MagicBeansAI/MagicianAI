//! The one lasting write alignment makes (plan §10.2, P7.1): a change the
//! owner confirmed, word for word, on a private surface.
//!
//! - **Remember** marks an inferred native record as the owner's confirmed
//!   preference. The wording is unchanged; the record now carries
//!   confirmation force (`owner_confirmed` with a stamp).
//! - **Replace** supersedes a native record with the owner's new wording. The
//!   new record carries the owner's reply as its evidence, so it reads as a
//!   direct owner statement; the old one is kept, retired, with a pointer to
//!   its successor.
//!
//! Both are bound to the source revision the wording was read back at: under
//! the knowledge file's exclusive lock the record must still hash to that
//! revision, or nothing is written and the owner is asked again. Both are
//! idempotent on the confirmation token, so a retried answer writes once.
//! Profile and app sources are never written here (plan §5.3).

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::ask::{LastingWrite, LastingWriteKind};
use super::sources::native::intention_id;
use crate::magician_v2::agents::memory_lifecycle::{digest, record_id, retired};
use crate::magician_v2::agents::{AgentMemoryError, AgentMemoryResolver};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    Applied,
    /// This exact confirmation was already written.
    AlreadyApplied,
    /// The record changed since the wording was read back.
    Stale,
    /// The record is gone or retired.
    NotFound,
}

/// Apply a confirmed lasting write for the owner of `principal`/`workspace`.
pub async fn apply_lasting_write(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    write: &LastingWrite,
    ask_id: &str,
    now: DateTime<Utc>,
) -> Result<WriteOutcome, AgentMemoryError> {
    let memory = resolver.resolve_for_scope(principal, workspace)?;
    let mut outcome = WriteOutcome::NotFound;
    memory
        .update_user_knowledge_as("alignment", |knowledge| {
            outcome = apply_to_document(knowledge, write, ask_id, now);
            Ok(outcome == WriteOutcome::Applied)
        })
        .await?;
    Ok(outcome)
}

fn confirmation(write: &LastingWrite, ask_id: &str, now: DateTime<Utc>) -> Value {
    json!({
        "ask_id": ask_id,
        "token": write.token,
        "kind": write.kind,
        "at": now.to_rfc3339(),
    })
}

fn carries_token(item: &Value, token: &str) -> bool {
    item.pointer("/alignment_confirmation/token")
        .and_then(Value::as_str)
        == Some(token)
}

/// The pure document change, under the caller's lock.
pub fn apply_to_document(
    knowledge: &mut Value,
    write: &LastingWrite,
    ask_id: &str,
    now: DateTime<Utc>,
) -> WriteOutcome {
    let Some(items) = knowledge
        .get_mut(&write.tier)
        .and_then(Value::as_array_mut)
    else {
        return WriteOutcome::NotFound;
    };
    if items.iter().any(|item| carries_token(item, &write.token)) {
        return WriteOutcome::AlreadyApplied;
    }
    let Some(index) = items.iter().position(|item| {
        item.is_object() && intention_id(&write.tier, item) == write.intention_id
    }) else {
        return WriteOutcome::NotFound;
    };
    if retired(&items[index]) {
        return WriteOutcome::NotFound;
    }
    if digest(&items[index]) != write.expected_revision {
        return WriteOutcome::Stale;
    }
    let stamp = now.to_rfc3339();
    match write.kind {
        LastingWriteKind::Remember => {
            let item = &mut items[index];
            confirm_in_place(item, &stamp, "preference_alignment");
            item["alignment_confirmation"] = confirmation(write, ask_id, now);
        },
        LastingWriteKind::Replace => {
            let evidence_id = format!("alignment-ask:{ask_id}");
            supersede_with_owner_wording(
                items,
                index,
                &write.tier,
                &write.wording,
                &OwnerWording {
                    evidence_id: &evidence_id,
                    rationale: "Changed by the owner while reviewing a held action.",
                    supersession_source: "preference_alignment",
                    stamp: &stamp,
                },
            );
            if let Some(successor) = items.last_mut() {
                successor["alignment_confirmation"] = confirmation(write, ask_id, now);
            }
        },
    }
    WriteOutcome::Applied
}

/// Mark a record as confirmed by the owner, in place. It now reads as
/// `confirmed`; the wording is unchanged.
pub(crate) fn confirm_in_place(item: &mut Value, stamp: &str, confirmed_from: &str) {
    item["source_type"] = json!("owner_confirmed");
    item["owner_confirmed_at"] = json!(stamp);
    item["confirmed_from"] = json!(confirmed_from);
    item["memory_lifecycle"] = json!("active");
    item["memory_review_needed"] = json!(false);
    item["updated_at"] = json!(stamp);
}

/// Where the owner's new wording came from.
pub(crate) struct OwnerWording<'a> {
    /// The owner-turn evidence id: an ask, a surface request.
    pub evidence_id: &'a str,
    pub rationale: &'a str,
    /// Who superseded the old record (`preference_alignment`, `owner_surface`).
    pub supersession_source: &'a str,
    pub stamp: &'a str,
}

/// Supersede `items[index]` with a successor carrying the owner's wording as
/// their own statement, keeping what the old record applies to. Returns the
/// successor's record id; the successor is pushed last.
pub(crate) fn supersede_with_owner_wording(
    items: &mut Vec<Value>,
    index: usize,
    tier: &str,
    wording: &str,
    origin: &OwnerWording<'_>,
) -> String {
    let old = items[index].clone();
    let old_id = intention_id(tier, &old);
    let stamp = origin.stamp;
    let mut successor = json!({
        "key": old.get("key"),
        "value": wording,
        "confidence": 1.0,
        "source_type": "explicit_user_statement",
        "rationale": origin.rationale,
        "source_event_id": origin.evidence_id,
        "memory_evidence": [{
            "id": origin.evidence_id,
            "source_role": "user",
            "source_id": origin.evidence_id,
            "source_type": "explicit_user_statement",
            "quote": wording,
            "at": stamp,
        }],
        "memory_lifecycle": "active",
        "memory_review_needed": false,
        "memory_saved_at": stamp,
        "updated_at": stamp,
        "supersedes": old_id,
    });
    // Keep what the record applies to; the owner changed the words, not
    // where they apply.
    for field in [
        "memory_kind",
        "memory_context",
        "project_id",
        "engagement_scope",
        "scope",
        "subject",
        "valid_from",
        "valid_until",
    ] {
        if let Some(value) = old.get(field) {
            successor[field] = value.clone();
        }
    }
    let successor_id = digest(&json!([tier, record_id(&successor)]));
    successor["memory_record_id"] = json!(successor_id);
    let item = &mut items[index];
    item["memory_lifecycle"] = json!("superseded");
    item["superseded_id"] = json!(old_id);
    item["superseded_by"] = json!(successor_id);
    item["superseded_at"] = json!(stamp);
    item["supersession_reason"] = json!("owner_changed_preference");
    item["supersession_source"] = json!(origin.supersession_source);
    item["memory_review_needed"] = json!(false);
    items.push(successor);
    successor_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::intention::AssertionStatus;
    use crate::magician_v2::preference_alignment::sources::native::{
        assertion_status, project_native_intentions,
    };

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-09T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn knowledge() -> Value {
        json!({
            "preferences": [
                {"memory_record_id": "inf-1", "key": "spice", "value": "mild",
                 "source_type": "inferred", "memory_lifecycle": "pending_review",
                 "memory_context": "weeknight dinners"},
                {"memory_record_id": "pref-1", "key": "diet", "value": "vegetarian dinners",
                 "source_type": "explicit_user_statement",
                 "memory_evidence": [{"source_role": "user", "source_id": "chat-turn:t1",
                                      "quote": "keep dinners vegetarian"}]}
            ]
        })
    }

    fn write(kind: LastingWriteKind, id: &str, doc: &Value, wording: &str) -> LastingWrite {
        let item = doc["preferences"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["memory_record_id"] == id)
            .unwrap();
        LastingWrite {
            kind,
            intention_id: id.to_string(),
            tier: "preferences".to_string(),
            expected_revision: digest(item),
            wording: wording.to_string(),
            superseded: None,
            token: format!("tok-{id}-{wording}"),
        }
    }

    #[test]
    fn remembering_confirms_the_record_without_changing_its_words() {
        let mut doc = knowledge();
        let w = write(LastingWriteKind::Remember, "inf-1", &doc, "spice: mild");
        assert_eq!(apply_to_document(&mut doc, &w, "ask-1", now()), WriteOutcome::Applied);
        let item = &doc["preferences"][0];
        assert_eq!(item["value"], json!("mild"));
        assert_eq!(assertion_status(item), AssertionStatus::Confirmed);
        // Retried: written once.
        assert_eq!(
            apply_to_document(&mut doc, &w, "ask-1", now()),
            WriteOutcome::AlreadyApplied
        );
        assert_eq!(doc["preferences"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn replacing_supersedes_with_the_owners_words_as_evidence() {
        let mut doc = knowledge();
        let w = write(
            LastingWriteKind::Replace,
            "pref-1",
            &doc,
            "vegetarian on weekdays only",
        );
        assert_eq!(apply_to_document(&mut doc, &w, "ask-2", now()), WriteOutcome::Applied);
        let items = doc["preferences"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[1]["memory_lifecycle"], json!("superseded"));
        let successor = &items[2];
        assert_eq!(successor["value"], json!("vegetarian on weekdays only"));
        assert_eq!(successor["key"], json!("diet"));
        assert_eq!(items[1]["superseded_by"], successor["memory_record_id"]);
        assert_eq!(assertion_status(successor), AssertionStatus::DirectOwnerStatement);
        // The view now holds the new statement only.
        let projected = project_native_intentions(&doc, now());
        let statements: Vec<&str> = projected
            .intentions
            .iter()
            .map(|intention| intention.statement.as_str())
            .collect();
        assert!(statements.contains(&"diet: vegetarian on weekdays only"));
        assert!(!statements.contains(&"diet: vegetarian dinners"));
        assert_eq!(
            apply_to_document(&mut doc, &w, "ask-2", now()),
            WriteOutcome::AlreadyApplied
        );
    }

    #[test]
    fn a_record_that_changed_after_the_readback_is_not_written() {
        let mut doc = knowledge();
        let w = write(LastingWriteKind::Remember, "inf-1", &doc, "spice: mild");
        doc["preferences"][0]["value"] = json!("medium");
        let before = doc.clone();
        assert_eq!(apply_to_document(&mut doc, &w, "ask-1", now()), WriteOutcome::Stale);
        assert_eq!(doc, before);

        let mut gone = knowledge();
        gone["preferences"][0]["memory_lifecycle"] = json!("retracted");
        let w = write(LastingWriteKind::Remember, "inf-1", &knowledge(), "spice: mild");
        assert_eq!(apply_to_document(&mut gone, &w, "ask-1", now()), WriteOutcome::NotFound);
    }

    #[tokio::test]
    async fn the_write_runs_under_the_owners_knowledge_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let memory = resolver.resolve_for_scope("owner", "ws").unwrap();
        memory.save_user_knowledge(&knowledge()).await.unwrap();
        let w = write(LastingWriteKind::Remember, "inf-1", &knowledge(), "spice: mild");
        assert_eq!(
            apply_lasting_write(&resolver, "owner", "ws", &w, "ask-1", now())
                .await
                .unwrap(),
            WriteOutcome::Applied
        );
        let saved = memory.load_user_knowledge().await.unwrap();
        assert_eq!(assertion_status(&saved["preferences"][0]), AssertionStatus::Confirmed);
    }
}
