use super::MetricsQuery;
use serde_json::json;
#[tokio::test]
async fn decision_metrics_lazy_capture_scope_isolation_and_pressure_are_visible() {
    use crate::magician_v2::{
        artifact_v2::workspace::ArtifactV2Workspace, realtime_events::RuntimeTransportEvent,
    };
    let d = tempfile::tempdir().unwrap();
    let layout = ArtifactV2Workspace::new(d.path());
    for principal in ["owner", "other"] {
        std::fs::create_dir_all(layout.scope_root(principal, "default")).unwrap();
    }
    let service = super::DecisionMetrics::start(layout.clone());
    assert!(!layout
        .analytics_decision_metrics_path("owner", "default")
        .exists());
    let now = chrono::Utc::now().timestamp_millis();
    for i in 0..1000 {
        let mut record = json!({"comparison_id":"case","operation":"channel_classify","required_questions":[],"items":[],"offered_item_ids":[],"mode":"gate"});
        record["comparison_id"] = json!(format!("case-{i}"));
        super::capture(&RuntimeTransportEvent::DecisionShadowAgreement {
            principal: "owner".into(),
            workspace: "default".into(),
            record: Box::new(record),
            timestamp: now,
        });
    }
    service.shutdown().await;
    let owner = service
        .summary(
            magicllm::LlmScope::new("owner", "default"),
            MetricsQuery::default(),
        )
        .await
        .unwrap();
    assert!(!owner.rows.is_empty());
    assert!(
        owner.capture_dropped > 0,
        "queue saturation must be reported"
    );
    let other = service
        .summary(
            magicllm::LlmScope::new("other", "default"),
            MetricsQuery::default(),
        )
        .await
        .unwrap();
    assert!(other.rows.is_empty());
    assert!(!layout
        .analytics_decision_metrics_path("other", "default")
        .exists());
}
