//! The owner's own edits to their preferences, from a private surface (web,
//! iOS): add, edit wording, confirm, retire, forget.
//!
//! Each edit is a pure change to `knowledge.json`, applied under the
//! caller's flock (`update_user_knowledge_as`). Every edit but `Add` names the
//! revision it was made against and is refused when the record changed since
//! (`Stale`). An edit never rewrites the old wording in place: editing
//! supersedes the record with a successor, so the old wording survives in the
//! record chain and the preference history. Only `Forget` removes a record,
//! and the history then erases its wording too.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::intention::IntentionKind;
use super::lasting_write::{confirm_in_place, supersede_with_owner_wording, OwnerWording};
use super::sources::native::{bears_intention, intention_id};
use crate::magician_v2::agents::memory_lifecycle::{digest, retired};

/// The tier an owner-added preference is written to. Its kind is recorded
/// explicitly (`memory_kind`), so the tier's default kind does not decide it.
pub const OWNER_ADD_TIER: &str = "preferences";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OwnerEdit {
    Add {
        kind: IntentionKind,
        #[serde(default)]
        label: Option<String>,
        wording: String,
        #[serde(default)]
        conditions: Option<String>,
    },
    Edit {
        id: String,
        expected_revision: String,
        wording: String,
    },
    Confirm {
        id: String,
        expected_revision: String,
    },
    Retire {
        id: String,
        expected_revision: String,
    },
    Forget {
        id: String,
        expected_revision: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EditOutcome {
    /// Applied; `record_id` is the record that now carries the preference
    /// (the successor for an edit, the forgotten id for a forget).
    Applied { record_id: String },
    /// The record changed since the owner read it.
    Stale { current_revision: String },
    NotFound,
    /// Empty wording, or an edit that would change nothing.
    Invalid { reason: String },
}

/// Whether going from `before` to `after` removes an instruction the owner
/// gave (one that may hold the agent's acts: a direct or confirmed
/// statement). Such a removal needs the owner on their own app, the REST
/// counterpart of the chat guard `owner_turn::owner_turn_may_retire`.
pub fn removes_an_owner_instruction(before: &Value, after: &Value) -> bool {
    use super::history::{diff_intentions, HistoryOp};
    use super::sources::native::assertion_status;
    diff_intentions(before, after)
        .iter()
        .filter(|change| change.op == HistoryOp::Forgotten)
        .any(|change| {
            locate(before, &change.record_id).is_some_and(|(tier, index)| {
                assertion_status(&before[&tier][index]).may_hold()
            })
        })
}

fn locate(knowledge: &Value, id: &str) -> Option<(String, usize)> {
    knowledge.as_object()?.iter().find_map(|(tier, items)| {
        items.as_array()?.iter().position(|item| {
            item.is_object() && bears_intention(tier, item) && intention_id(tier, item) == id
        })
        .map(|index| (tier.clone(), index))
    })
}

fn kind_label(kind: IntentionKind) -> &'static str {
    match kind {
        IntentionKind::Taste => "preference",
        IntentionKind::Goal => "goal",
        IntentionKind::StandingInstruction => "instruction",
    }
}

/// Apply one owner edit to the document. `surface` is the private surface it
/// came from (`web`, `ios`); `request_id` names the request as the owner's
/// evidence.
pub fn apply_owner_edit(
    knowledge: &mut Value,
    edit: &OwnerEdit,
    surface: &str,
    request_id: &str,
    now: DateTime<Utc>,
) -> EditOutcome {
    let stamp = now.to_rfc3339();
    let evidence_id = format!("owner_surface:{surface}:{request_id}");
    if let OwnerEdit::Add {
        kind,
        label,
        wording,
        conditions,
    } = edit
    {
        let wording = wording.trim();
        if wording.is_empty() {
            return EditOutcome::Invalid {
                reason: "wording is empty".to_string(),
            };
        }
        if !knowledge.is_object() {
            *knowledge = json!({});
        }
        let record_id = format!("owner-{}", uuid::Uuid::new_v4());
        let mut record = json!({
            "memory_record_id": record_id,
            "key": label.as_deref().map(str::trim).filter(|l| !l.is_empty()).unwrap_or("preference"),
            "value": wording,
            "memory_kind": kind_label(*kind),
            "confidence": 1.0,
            "source_type": "explicit_user_statement",
            "rationale": format!("Added by the owner on {surface}."),
            "source_event_id": evidence_id,
            "memory_evidence": [{
                "id": evidence_id,
                "source_role": "user",
                "source_id": evidence_id,
                "source_type": "explicit_user_statement",
                "quote": wording,
                "at": stamp,
            }],
            "memory_lifecycle": "active",
            "memory_review_needed": false,
            "memory_saved_at": stamp,
            "updated_at": stamp,
        });
        if let Some(conditions) = conditions.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            record["memory_context"] = json!(conditions);
        }
        let tier = knowledge
            .as_object_mut()
            .expect("an object")
            .entry(OWNER_ADD_TIER)
            .or_insert_with(|| json!([]));
        if !tier.is_array() {
            return EditOutcome::Invalid {
                reason: format!("the {OWNER_ADD_TIER} tier is not a list"),
            };
        }
        tier.as_array_mut().expect("an array").push(record);
        return EditOutcome::Applied { record_id };
    }

    let (id, expected_revision) = match edit {
        OwnerEdit::Edit {
            id,
            expected_revision,
            ..
        }
        | OwnerEdit::Confirm {
            id,
            expected_revision,
        }
        | OwnerEdit::Retire {
            id,
            expected_revision,
        }
        | OwnerEdit::Forget {
            id,
            expected_revision,
        } => (id, expected_revision),
        OwnerEdit::Add { .. } => unreachable!("handled above"),
    };
    let Some((tier, index)) = locate(knowledge, id) else {
        return EditOutcome::NotFound;
    };
    let items = knowledge[&tier].as_array_mut().expect("located in an array");
    let current_revision = digest(&items[index]);
    if &current_revision != expected_revision {
        return EditOutcome::Stale { current_revision };
    }
    // A retired record can only be forgotten.
    if retired(&items[index]) && !matches!(edit, OwnerEdit::Forget { .. }) {
        return EditOutcome::NotFound;
    }
    match edit {
        OwnerEdit::Edit { wording, .. } => {
            let wording = wording.trim();
            if wording.is_empty() {
                return EditOutcome::Invalid {
                    reason: "wording is empty".to_string(),
                };
            }
            let record_id = supersede_with_owner_wording(
                items,
                index,
                &tier,
                wording,
                &OwnerWording {
                    evidence_id: &evidence_id,
                    rationale: &format!("Changed by the owner on {surface}."),
                    supersession_source: "owner_surface",
                    stamp: &stamp,
                },
            );
            EditOutcome::Applied { record_id }
        },
        OwnerEdit::Confirm { id, .. } => {
            confirm_in_place(&mut items[index], &stamp, "owner_surface");
            EditOutcome::Applied {
                record_id: id.clone(),
            }
        },
        OwnerEdit::Retire { id, .. } => {
            let item = &mut items[index];
            item["memory_lifecycle"] = json!("retracted");
            item["retracted_at"] = json!(stamp);
            item["retraction_source"] = json!("owner_surface");
            item["memory_review_needed"] = json!(false);
            EditOutcome::Applied {
                record_id: id.clone(),
            }
        },
        OwnerEdit::Forget { id, .. } => {
            items.remove(index);
            EditOutcome::Applied {
                record_id: id.clone(),
            }
        },
        OwnerEdit::Add { .. } => unreachable!("handled above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::preference_alignment::intention::AssertionStatus;
    use crate::magician_v2::preference_alignment::sources::native::{
        assertion_status, project_native_intentions,
    };

    fn record(id: &str, key: &str, value: &str) -> Value {
        json!({"memory_record_id": id, "key": key, "value": value})
    }

    fn now() -> DateTime<Utc> {
        "2026-10-10T12:00:00Z".parse().unwrap()
    }

    fn apply(doc: &mut Value, edit: OwnerEdit) -> EditOutcome {
        apply_owner_edit(doc, &edit, "web", "req-1", now())
    }

    #[test]
    fn an_added_preference_reads_as_the_owners_direct_statement() {
        let mut doc = json!({});
        let EditOutcome::Applied { record_id } = apply(
            &mut doc,
            OwnerEdit::Add {
                kind: IntentionKind::StandingInstruction,
                label: Some("door codes".to_string()),
                wording: "Ask me before giving a door code to anyone outside the household".to_string(),
                conditions: None,
            },
        ) else {
            panic!("not applied");
        };
        let added = &doc[OWNER_ADD_TIER][0];
        assert_eq!(added["memory_record_id"], json!(record_id));
        assert_eq!(assertion_status(added), AssertionStatus::DirectOwnerStatement);
        let projected = project_native_intentions(&doc, now());
        assert_eq!(projected.intentions[0].kind, IntentionKind::StandingInstruction);
        assert_eq!(projected.intentions[0].id, record_id);
    }

    #[test]
    fn editing_supersedes_and_keeps_the_old_record() {
        let mut doc = json!({"preferences": [record("a", "dinner", "No meat")]});
        let revision = digest(&doc["preferences"][0]);
        let EditOutcome::Applied { record_id } = apply(
            &mut doc,
            OwnerEdit::Edit {
                id: "a".to_string(),
                expected_revision: revision,
                wording: "No red meat".to_string(),
            },
        ) else {
            panic!("not applied");
        };
        assert_eq!(doc["preferences"][0]["memory_lifecycle"], "superseded");
        assert_eq!(doc["preferences"][0]["superseded_by"], json!(record_id));
        assert_eq!(doc["preferences"][0]["supersession_source"], "owner_surface");
        assert_eq!(doc["preferences"][1]["value"], "No red meat");
        assert_eq!(
            assertion_status(&doc["preferences"][1]),
            AssertionStatus::DirectOwnerStatement
        );
    }

    #[test]
    fn a_stale_revision_is_refused_and_names_the_current_one() {
        let mut doc = json!({"preferences": [record("a", "dinner", "No meat")]});
        let current = digest(&doc["preferences"][0]);
        let before = doc.clone();
        assert_eq!(
            apply(
                &mut doc,
                OwnerEdit::Retire {
                    id: "a".to_string(),
                    expected_revision: "old".to_string(),
                },
            ),
            EditOutcome::Stale {
                current_revision: current
            }
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn confirm_retire_and_forget() {
        let mut doc = json!({"preferences": [record("a", "dinner", "No meat"), record("b", "coffee", "decaf")]});
        let rev = |doc: &Value, i: usize| digest(&doc["preferences"][i]);
        let r = rev(&doc, 0);
        apply(&mut doc, OwnerEdit::Confirm { id: "a".to_string(), expected_revision: r });
        assert_eq!(assertion_status(&doc["preferences"][0]), AssertionStatus::Confirmed);
        let r = rev(&doc, 1);
        apply(&mut doc, OwnerEdit::Retire { id: "b".to_string(), expected_revision: r });
        assert_eq!(doc["preferences"][1]["memory_lifecycle"], "retracted");
        assert!(project_native_intentions(&doc, now())
            .intentions
            .iter()
            .all(|intention| intention.id != "b"));
        // A retired record can be forgotten, not edited.
        let r = rev(&doc, 1);
        assert_eq!(
            apply(&mut doc, OwnerEdit::Confirm { id: "b".to_string(), expected_revision: r.clone() }),
            EditOutcome::NotFound
        );
        apply(&mut doc, OwnerEdit::Forget { id: "b".to_string(), expected_revision: r });
        assert_eq!(doc["preferences"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn empty_wording_and_unknown_records_change_nothing() {
        let mut doc = json!({"preferences": [record("a", "dinner", "No meat")]});
        let before = doc.clone();
        let r = digest(&doc["preferences"][0]);
        assert!(matches!(
            apply(&mut doc, OwnerEdit::Edit { id: "a".to_string(), expected_revision: r, wording: "  ".to_string() }),
            EditOutcome::Invalid { .. }
        ));
        assert_eq!(
            apply(&mut doc, OwnerEdit::Forget { id: "zz".to_string(), expected_revision: "x".to_string() }),
            EditOutcome::NotFound
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn removing_an_owner_instruction_is_recognised_whole_tier_or_single() {
        let owner = json!({"memory_record_id": "a", "key": "codes", "value": "Ask me before giving out door codes",
            "source_type": "explicit_user_statement",
            "memory_evidence": [{"source_role": "user", "source_id": "turn-1", "quote": "Ask me before giving out door codes"}]});
        let inferred = record("b", "coffee", "decaf");
        let before = json!({"preferences": [owner, inferred.clone()]});
        assert!(removes_an_owner_instruction(&before, &json!({"preferences": [inferred]})));
        assert!(removes_an_owner_instruction(&before, &json!({})));
        let mut only_inferred_gone = before.clone();
        only_inferred_gone["preferences"].as_array_mut().unwrap().remove(1);
        assert!(!removes_an_owner_instruction(&before, &only_inferred_gone));
    }
}
