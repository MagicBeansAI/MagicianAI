use std::thread;
use std::time::Duration;

use chrono::Utc;
use magician::magician_v2::slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType};

fn build_slot_record(slot_type: SlotType) -> SlotRecord {
    let now = Utc::now();
    SlotRecord {
        id: "slot-1".to_string(),
        slot_type,
        value: serde_json::json!("Acme Corp"),
        confidence: 0.85,
        provenance: vec![ProvenanceRecord {
            source: ProvenanceSource::UserReply,
            timestamp: now,
        }],
        evidence_links: vec!["https://example.com/doc".to_string()],
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn slot_record_serializes_and_roundtrips() {
    let record = build_slot_record(SlotType::Entity);
    let json = serde_json::to_string(&record).expect("serialize");
    assert!(
        json.contains("\"slot_type\":\"entity\""),
        "slot type should serialize in snake_case"
    );
    let restored: SlotRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored.id, record.id);
    assert_eq!(restored.slot_type, SlotType::Entity);
    assert_eq!(restored.value, record.value);
    assert_eq!(restored.provenance.len(), 1);
}

#[test]
fn touch_updates_timestamp_and_preserves_metadata() {
    let mut record = build_slot_record(SlotType::Temporal);
    let created_at = record.created_at;
    let previous_updated_at = record.updated_at;
    thread::sleep(Duration::from_millis(5));
    record.touch();
    assert!(
        record.updated_at > previous_updated_at,
        "touch should move updated_at forward"
    );
    assert_eq!(record.created_at, created_at, "created_at unchanged");
    assert_eq!(record.provenance.len(), 1);
}
