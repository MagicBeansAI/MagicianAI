use chrono::{Duration, Utc};
use magician::magician_v2::{
    confidence::{ConfidenceConfig, ConfidenceService},
    slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType},
};

fn build_slot(
    id: &str,
    slot_type: SlotType,
    confidence: f64,
    provenance: Vec<ProvenanceRecord>,
    value: serde_json::Value,
) -> SlotRecord {
    let now = Utc::now();
    SlotRecord {
        id: id.to_string(),
        slot_type,
        value,
        confidence,
        provenance,
        evidence_links: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn combines_slot_and_provenance_scores() {
    let service = ConfidenceService::new(ConfidenceConfig::default());

    let slot = build_slot(
        "entity::1",
        SlotType::Entity,
        0.5,
        vec![
            ProvenanceRecord {
                source: ProvenanceSource::UserReply,
                timestamp: Utc::now(),
            },
            ProvenanceRecord {
                source: ProvenanceSource::DeterministicCheck,
                timestamp: Utc::now(),
            },
        ],
        serde_json::Value::String("Acme Corp".to_string()),
    );

    let score = service.calculate_slot_confidence(&slot);
    // Provenance average = (1.0 + 0.9) / 2 = 0.95
    // Combined with base 0.5 => (0.5 + 0.95)/2 = 0.725
    assert!((score - 0.725).abs() < 1e-3);
}

#[test]
fn overall_confidence_weights_critical_slots() {
    let mut config = ConfidenceConfig::default();
    config.critical_slots = vec![SlotType::Entity];
    let service = ConfidenceService::new(config);

    let slots = vec![
        build_slot(
            "entity::1",
            SlotType::Entity,
            0.8,
            Vec::new(),
            serde_json::Value::String("Acme Corp".to_string()),
        ),
        build_slot(
            "status::1",
            SlotType::Status,
            0.4,
            Vec::new(),
            serde_json::Value::String("pending".to_string()),
        ),
    ];

    let overall = service.calculate_overall_confidence(&slots);
    // Weighted average: (0.8*2 + 0.4*1) / 3 = 0.667
    let expected = (0.8 * 2.0 + 0.4) / 3.0;
    assert!((overall - expected).abs() < 1e-6);
}

#[test]
fn calculates_confidence_slope_with_linear_regression() {
    let service = ConfidenceService::new(ConfidenceConfig::default());
    let now = Utc::now();

    let history = vec![
        (now, 0.8),
        (now + Duration::seconds(10), 0.7),
        (now + Duration::seconds(20), 0.6),
        (now + Duration::seconds(30), 0.5),
    ];

    let slope = service.calculate_confidence_slope(&history).unwrap();
    // Approximately -0.01 per second.
    assert!(slope < 0.0);
    assert!((slope + 0.01).abs() < 5e-3);
}

#[test]
fn identifies_satisfied_critical_slots() {
    let mut config = ConfidenceConfig::default();
    config.critical_slots = vec![SlotType::Entity, SlotType::Temporal];
    let service = ConfidenceService::new(config);

    let slots = vec![
        build_slot(
            "entity::1",
            SlotType::Entity,
            0.9,
            Vec::new(),
            serde_json::Value::String("Acme Corp".to_string()),
        ),
        build_slot(
            "time::1",
            SlotType::Temporal,
            0.65,
            Vec::new(),
            serde_json::Value::String("Tomorrow".to_string()),
        ),
    ];

    assert!(service.is_critical_slot_satisfied(&slots, 0.6));
    assert!(!service.is_critical_slot_satisfied(&slots, 0.7));
}

#[test]
fn summarises_confidence_and_unresolved_slots() {
    let mut config = ConfidenceConfig::default();
    config.critical_slots = vec![SlotType::Entity];
    config.unresolved_slot_threshold = 0.7;
    let service = ConfidenceService::new(config);

    let slots = vec![
        build_slot(
            "entity::1",
            SlotType::Entity,
            0.9,
            Vec::new(),
            serde_json::Value::String("Acme Corp".to_string()),
        ),
        build_slot(
            "status::1",
            SlotType::Status,
            0.4,
            Vec::new(),
            serde_json::Value::String("".to_string()),
        ),
    ];

    let summary = service.summarize_confidence(&slots);
    let expected_overall = (0.9 * 2.0 + 0.4) / 3.0;
    assert!((summary.overall - expected_overall).abs() < 1e-6);
    assert_eq!(summary.min_critical_slot, 0.9);
    assert_eq!(summary.unresolved_slots, vec!["status::1".to_string()]);
}

#[test]
fn handles_large_slot_graphs_without_degradation() {
    let service = ConfidenceService::new(ConfidenceConfig::default());

    let slots: Vec<SlotRecord> = (0..150)
        .map(|idx| {
            let slot_type = if idx % 2 == 0 {
                SlotType::Entity
            } else {
                SlotType::Status
            };
            build_slot(
                &format!("slot::{}", idx),
                slot_type,
                0.5 + (idx as f64 / 300.0),
                Vec::new(),
                serde_json::Value::String(format!("value-{}", idx)),
            )
        })
        .collect();

    let overall = service.calculate_overall_confidence(&slots);
    let expected: f64 = slots.iter().map(|slot| slot.confidence).sum::<f64>() / slots.len() as f64;
    assert!((overall - expected).abs() < 1e-6);
}
