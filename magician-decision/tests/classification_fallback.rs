use async_trait::async_trait;
use magician_decision::runtime::BoundModel;
use magician_decision::*;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

struct Model {
    name: &'static str,
    delay: u64,
    values: [f64; 2],
}
#[async_trait]
impl StructuredDecisionModel for Model {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("fixture", self.name)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn evaluate(&self, r: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        tokio::time::sleep(Duration::from_millis(self.delay)).await;
        Ok(DecisionResponse {
            model: self.identity(),
            pack_id: r.pack_id,
            pack_version: r.pack_version,
            answers: ["a", "b"]
                .into_iter()
                .zip(self.values)
                .map(|(q, noul)| (QuestionId::new(q), Answer::Noul { noul }))
                .collect(),
            usage: Usage {
                input_tokens: 4,
                output_tokens: 0,
            },
        })
    }
}
fn runtime(
    first: [f64; 2],
    second: [f64; 2],
    delay: u64,
    fallback_thresholds: bool,
) -> DecisionRuntime {
    let pack = Pack {
        id: "test".into(),
        version: "1".into(),
        description: None,
        questions: ["a", "b"]
            .into_iter()
            .map(|id| {
                Question::Noul(NoulQuestion {
                    id: QuestionId::new(id),
                    instructions: Instruction::Text("applies".into()),
                    criteria: None,
                })
            })
            .collect(),
    };
    let thresholds = BTreeMap::from([("a".into(), 0.9), ("b".into(), 0.9)]);
    DecisionRuntimeBuilder::new()
        .bind_route(
            "test",
            pack,
            vec![
                BoundModel {
                    name: "primary".into(),
                    model: Arc::new(Model {
                        name: "primary",
                        delay,
                        values: first,
                    }),
                    admission: Default::default(),
                    thresholds: Some(thresholds.clone()),
                },
                BoundModel {
                    name: "local".into(),
                    model: Arc::new(Model {
                        name: "local",
                        delay: 1,
                        values: second,
                    }),
                    admission: Default::default(),
                    thresholds: fallback_thresholds.then_some(thresholds),
                },
            ],
        )
        .build()
}
async fn run(
    r: &DecisionRuntime,
) -> (
    DecisionResponse,
    Vec<decision_engine_contract::telemetry::DecisionModelCall>,
) {
    let request = r
        .build_request("test", DecisionState::from_text("evidence"))
        .unwrap();
    let (result, calls) = magician_decision::telemetry::capture(r.evaluate_request_before(
        request,
        Some(tokio::time::Instant::now() + Duration::from_millis(160)),
    ))
    .await;
    (result.unwrap(), calls)
}

#[tokio::test]
async fn expired_budget_with_fitting_route_is_timeout() {
    let r = runtime([0.99, 0.99], [0.99, 0.99], 1, true);
    let request = r
        .build_request("test", DecisionState::from_text("evidence"))
        .unwrap();
    let result = r
        .evaluate_request_before(request, Some(tokio::time::Instant::now()))
        .await;
    assert!(matches!(result, Err(DecisionError::Timeout)), "{result:?}");
}
#[tokio::test]
async fn uncertain_primary_uses_confident_local_with_separate_receipts() {
    let (reply, calls) = run(&runtime([0.6, 0.6], [0.99, 0.99], 1, true)).await;
    assert_eq!(reply.model.model, "local");
    assert_eq!(calls.len(), 2);
}

#[tokio::test]
async fn unbatched_decisions_also_try_backup_when_primary_is_uncertain() {
    let r = runtime([0.6, 0.6], [0.99, 0.99], 1, true);
    let (result, calls) = magician_decision::telemetry::capture(
        r.evaluate("test", DecisionState::from_text("evidence")),
    )
    .await;
    assert_eq!(result.unwrap().model.model, "local");
    assert_eq!(calls.len(), 2);
}

#[test]
fn ambiguous_model_identity_cannot_borrow_another_profiles_thresholds() {
    let original = runtime([0.6, 0.6], [0.99, 0.99], 1, true);
    let op = original.bound_operation("test").unwrap();
    let mut route = op.route.clone();
    route[1].model = route[0].model.clone();
    route[1].thresholds = Some(BTreeMap::from([("a".into(), 0.75), ("b".into(), 0.75)]));
    let identity = route[0].model.identity();
    let r = DecisionRuntimeBuilder::new()
        .bind_route("test", op.pack.clone(), route)
        .build();
    assert!(r.thresholds_for("test", &identity).is_none());
}
#[tokio::test]
async fn confident_primary_avoids_local_work() {
    let (reply, calls) = run(&runtime([0.99, 0.99], [0.01, 0.01], 1, true)).await;
    assert_eq!(reply.model.model, "primary");
    assert_eq!(calls.len(), 1);
}
#[tokio::test]
async fn local_cannot_overwrite_confident_primary_head_or_borrow_thresholds() {
    for (values, thresholds) in [([0.01, 0.99], true), ([0.99, 0.99], false)] {
        let (reply, calls) = run(&runtime([0.99, 0.6], values, 1, thresholds)).await;
        assert_eq!(reply.model.model, "primary");
        assert_eq!(calls.len(), 2);
    }
}
#[tokio::test]
async fn slow_primary_leaves_time_for_local_without_false_provider_outage() {
    let r = runtime([0.99, 0.99], [0.99, 0.99], 500, true);
    let (reply, calls) = run(&r).await;
    assert_eq!(reply.model.model, "local");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].error_class.as_deref(),
        Some("structured_provider_timeout")
    );
    assert!(!r
        .health_for("test")
        .iter()
        .any(|h| h.model == "primary" && h.issue.is_some()));
}
#[tokio::test]
async fn uncertain_local_retains_primary_partial_answer() {
    let r = runtime([0.99, 0.6], [0.6, 0.6], 1, true);
    let (reply, _) = run(&r).await;
    assert_eq!(reply.model.model, "primary");
}
#[test]
fn fallback_routes_are_explicit_deduplicated_and_allow_remote_in_cloud() {
    use magician_decision::{
        config::DecisionConfig,
        engine::{build_runtime_with, route_names},
    };
    let config:DecisionConfig=serde_yaml::from_str("enabled: true\nmodels:\n  primary: {adapter: systemone, model: primary, endpoint: 'http://localhost:9999/v1/systemone'}\n  remote: {adapter: systemone, model: remote, endpoint: 'https://example.org/v1/systemone'}\noperations:\n  test:\n    model: primary\n    fallback_models: [remote, primary]\n    pack: memory_applicability\n").unwrap();
    assert!(config.validate().is_ok());
    assert_eq!(
        route_names(&config, &config.operations["test"]),
        vec!["primary", "remote"]
    );
    let r = build_runtime_with(&config, false, &PackStore::from_roots(vec![]), &|_| None).unwrap();
    assert_eq!(r.bound_operation("test").unwrap().route.len(), 2);
}

#[test]
fn shipped_memory_fallbacks_own_explicit_complete_thresholds() {
    let seed: magician_decision::config::DecisionConfig =
        serde_yaml::from_str(include_str!("../../decision-engine.yaml")).unwrap();
    seed.validate().unwrap();
    for (name, op) in &seed.operations {
        for fallback in &op.fallback_models {
            assert!(
                op.thresholds_by_model.contains_key(fallback),
                "{name}/{fallback} has no thresholds"
            );
        }
        // Candidate maps must be complete even while automatic fallback is off.
        for (fallback, thresholds) in &op.thresholds_by_model {
            assert!(
                op.thresholds
                    .keys()
                    .all(|question| thresholds.contains_key(question)),
                "{name}/{fallback} missing a head"
            );
        }
    }
}

#[tokio::test]
async fn background_wait_does_not_spend_its_inference_allowance() {
    use magician_decision::runtime::{with_background_budget, BackgroundBudget};
    let r = runtime([0.99, 0.99], [0.99, 0.99], 80, true);
    let admission = r.bound_operation("test").unwrap().route[0]
        .admission
        .clone();
    admission.set_limit(1);
    let held = admission.enter().await.unwrap();
    let request = r
        .build_request("test", DecisionState::from_text("evidence"))
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let release = async {
        tokio::time::sleep(Duration::from_millis(160)).await;
        drop(held);
    };
    // Primary gets 300ms queue time and 120ms inference time. Spending
    // 160ms waiting must not cancel its 80ms inference or invoke the fallback.
    let evaluation = magician_decision::telemetry::capture(with_background_budget(
        Some(BackgroundBudget {
            queue: Duration::from_millis(600),
            inference: Duration::from_millis(240),
            deadline,
        }),
        r.evaluate_request_before(request, Some(deadline)),
    ));
    let ((result, calls), ()) = tokio::join!(evaluation, release);
    assert_eq!(result.unwrap().model.model, "primary");
    assert_eq!(calls.len(), 1);
    assert!(calls[0].queue_wait_ms >= 150);
    assert!(calls[0].latency_ms >= 75);
}

#[tokio::test]
async fn short_background_owner_deadline_still_reserves_a_local_fallback_turn() {
    use magician_decision::runtime::{with_background_budget, BackgroundBudget};
    let r = runtime([0.99, 0.99], [0.99, 0.99], 500, true);
    let request = r
        .build_request("test", DecisionState::from_text("evidence"))
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(160);
    let (result, calls) = magician_decision::telemetry::capture(with_background_budget(
        Some(BackgroundBudget {
            queue: Duration::from_secs(30),
            inference: Duration::from_secs(30),
            deadline,
        }),
        r.evaluate_request_before(request, Some(deadline)),
    ))
    .await;
    assert_eq!(result.unwrap().model.model, "local");
    assert_eq!(calls.len(), 2);
    assert!(r.health_for("test").iter().all(|h| h.issue.is_none()));
}

#[tokio::test]
async fn cancelled_wait_has_no_physical_receipt_or_provider_outage() {
    let r = runtime([0.99, 0.99], [0.99, 0.99], 1, true);
    let admission = r.bound_operation("test").unwrap().route[0]
        .admission
        .clone();
    admission.set_limit(1);
    let held = admission.enter().await.unwrap();
    let request = r
        .build_request("test", DecisionState::from_text("evidence"))
        .unwrap();
    let (result, calls) = magician_decision::telemetry::capture(tokio::time::timeout(
        Duration::from_millis(15),
        r.evaluate_request(request),
    ))
    .await;
    assert!(result.is_err());
    assert!(calls.is_empty());
    assert!(admission.health().is_none());
    drop(held);
    assert!(run(&r).await.0.model.model == "primary");
}

#[tokio::test]
async fn full_queue_falls_through_without_billing_or_poisoning_primary_health() {
    let r = runtime([0.99, 0.99], [0.99, 0.99], 1, true);
    let admission = r.bound_operation("test").unwrap().route[0]
        .admission
        .clone();
    admission.configure(1, 1, 1); // The primary's typed request exceeds one byte.
    let (response, calls) = run(&r).await;
    assert_eq!(response.model.model, "local");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].model, "local");
    assert!(admission.health().is_none());
}

#[test]
fn queue_configuration_is_bounded_and_reloads_reuse_admission() {
    let mut config = magician_decision::config::DecisionConfig::default();
    let mut model = magician_decision::config::DecisionModelConfig::default();
    config.models.insert("model".into(), model.clone());
    assert!(config.validate().is_ok());
    config.models.get_mut("model").unwrap().queue_capacity = 0;
    assert!(config.validate().is_err());
    let registry = magician_decision::registry::ModelRegistry::new();
    let old = registry.admission(&model);
    config.models.insert("model".into(), model.clone());
    let operation = magician_decision::config::DecisionOperationConfig::default();
    let behavior = config.behavior_fingerprint(&operation);
    model.queue_capacity = 8;
    model.queue_max_bytes = 1024;
    model.max_in_flight = 1;
    config.models.insert("model".into(), model.clone());
    assert_eq!(behavior, config.behavior_fingerprint(&operation));
    let reloaded = magician_decision::registry::ModelRegistry::reusing(&registry);
    assert!(Arc::ptr_eq(&old, &reloaded.admission(&model)));
}

#[test]
fn restricted_outputs_are_pack_validated_and_change_behavior_identity() {
    use magician_decision::config::{DecisionConfig, DecisionOperationConfig};
    let mut config = DecisionConfig::default();
    let mut operation = DecisionOperationConfig {
        pack: "memory_conflict_review".into(),
        ..Default::default()
    };
    config
        .operations
        .insert("memory_conflict_review".into(), operation.clone());
    let before = config.behavior_fingerprint(&operation);
    operation.restricted_outputs.insert(
        "resolution".into(),
        vec!["replace_existing".into(), "keep_existing".into()],
    );
    config
        .operations
        .insert("memory_conflict_review".into(), operation.clone());
    assert!(config.validate().is_ok());
    assert_ne!(before, config.behavior_fingerprint(&operation));
    operation
        .restricted_outputs
        .insert("resolution".into(), vec!["typo".into()]);
    config
        .operations
        .insert("memory_conflict_review".into(), operation.clone());
    assert!(config
        .validate()
        .unwrap_err()
        .contains("invalid restricted output"));
    operation.restricted_outputs.clear();
    operation
        .restricted_outputs
        .insert("wrong_head".into(), vec!["replace_existing".into()]);
    config
        .operations
        .insert("memory_conflict_review".into(), operation);
    assert!(config
        .validate()
        .unwrap_err()
        .contains("unknown restricted question"));
}

#[test]
fn shared_grouping_changes_behavior_but_observation_rate_does_not() {
    use magician_decision::config::{BatchStrategy, DecisionConfig, DecisionOperationConfig};
    let config = DecisionConfig::default();
    let mut operation = DecisionOperationConfig::default();
    let per_item = config.behavior_fingerprint(&operation);
    operation.observation.gate_sample_rate = 0.1;
    assert_eq!(per_item, config.behavior_fingerprint(&operation));
    operation.batch_strategy = BatchStrategy::SharedChunk;
    let shared = config.behavior_fingerprint(&operation);
    assert_ne!(per_item, shared);
    operation.classification.chunk_size += 1;
    assert_ne!(shared, config.behavior_fingerprint(&operation));
    operation.observation.gate_sample_rate = f64::NAN;
    let mut configured = config.clone();
    configured
        .operations
        .insert("memory_applicability".into(), operation);
    assert!(configured
        .validate()
        .unwrap_err()
        .contains("finite probability"));
}
