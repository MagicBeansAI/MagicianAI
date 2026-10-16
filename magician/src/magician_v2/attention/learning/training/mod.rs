//! Offline actionability trainer.
//!
//! Serving already scores L2 logistic + Platt snapshots. This module is the
//! missing producer: it joins explicit outcomes to the feature vector captured
//! at serve time, fits the same model family the Python eval script uses, and
//! refuses to emit a weak artifact.

pub mod bandit;
pub mod calibration;
pub mod features;
pub mod gates;
pub mod labels;
pub mod logistic;
pub mod routing;
pub mod worker;

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use super::{
    actionability::{
        ActionabilityModelSnapshot, ActionabilityTrainingManifest, ACTIONABILITY_FEATURE_CONTRACT,
        ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT, ATTENTION_SEMANTIC_SCHEMA_VERSION,
    },
    store::{ActionabilityTrainingRow, AttentionLearningStore, AttentionTrainingRunRecord},
};

use calibration::{expected_calibration_error, fit_platt, platt, roc_auc};
use gates::{evaluate_gates, GateConfig};
use labels::{
    classify_label, excluded_outcomes, negative_outcomes, positive_outcomes, LabelClass,
    LabelExample,
};
use logistic::{fit_logistic, linear_score};

pub use bandit::{train_bandit, BanditTrainingConfig, MIN_CANARY_POSTERIOR_UPDATES};
pub use gates::{GateVerdict, TrainingMetrics};
pub use labels::LabelSet;
pub use routing::{train_routing, RoutingTrainingConfig};
pub use worker::AttentionActionabilityTrainingWorker;

const MODEL_VERSION: &str = "l2_logistic_platt_v1";
const SPLIT_STRATEGY: &str = "temporal";
const TRAINING_SLICE: &str = "actionability";

#[derive(Debug, Clone, PartialEq)]
pub struct ActionabilityTrainingConfig {
    pub cutoff_at: i64,
    pub holdout_fraction: f64,
    pub l2_lambda: f64,
    pub fit_steps: usize,
    pub gates: GateConfig,
}

impl Default for ActionabilityTrainingConfig {
    fn default() -> Self {
        Self {
            cutoff_at: i64::MAX,
            holdout_fraction: 0.2,
            l2_lambda: 0.1,
            fit_steps: 1500,
            gates: GateConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrainingOutcome {
    Written {
        snapshot_id: String,
        path: PathBuf,
        metrics: TrainingMetrics,
        counts: LabelSet,
    },
    Refused {
        reason: String,
        metrics: TrainingMetrics,
        counts: LabelSet,
    },
}

pub async fn train_actionability(
    store: &AttentionLearningStore,
    principal: &str,
    workspace: &str,
    config: &ActionabilityTrainingConfig,
    out_path: &Path,
) -> Result<TrainingOutcome> {
    let rows = store
        .list_actionability_training_rows(principal, workspace, config.cutoff_at)
        .await?;
    let examples = rows
        .iter()
        .map(|row| LabelExample {
            outcome: row.outcome,
            reason: row.reason.as_deref(),
            decision_id: row.decision_id.as_deref(),
            has_features: row.features.is_some(),
        })
        .collect::<Vec<_>>();
    let counts = LabelSet::from_outcomes(&examples);
    let prepared = prepare_examples(&rows);
    let metrics_for = |auc: f64, ece: f64, label_count: usize| TrainingMetrics {
        label_count,
        positive: counts.positive,
        negative: counts.negative,
        usable: counts.usable,
        unlinked: counts.unlinked,
        auc,
        ece,
    };

    if prepared.len() < config.gates.min_labels.max(3) {
        let metrics = metrics_for(0.5, 1.0, prepared.len());
        let reason = format!(
            "labels {} below minimum {}",
            prepared.len(),
            config.gates.min_labels
        );
        persist_run(
            store,
            principal,
            workspace,
            "refused",
            Some(&reason),
            &metrics,
            None,
        )
        .await?;
        return Ok(TrainingOutcome::Refused {
            reason,
            metrics,
            counts,
        });
    }

    let Some((train_end, cal_end)) = temporal_three_way(prepared.len(), config.holdout_fraction)
    else {
        let metrics = metrics_for(0.5, 1.0, prepared.len());
        let reason = format!(
            "temporal split needs train, calibration, and test; usable labels {}",
            prepared.len()
        );
        persist_run(
            store,
            principal,
            workspace,
            "refused",
            Some(&reason),
            &metrics,
            None,
        )
        .await?;
        return Ok(TrainingOutcome::Refused {
            reason,
            metrics,
            counts,
        });
    };

    let feature_names = collect_feature_names(&prepared);
    let dataset_digest = dataset_digest(&prepared, &feature_names);
    if let Some(existing) = reuse_matching_snapshot(
        store,
        principal,
        workspace,
        &dataset_digest,
        &feature_names,
        config.l2_lambda,
    )
    .await?
    {
        write_snapshot_atomically(out_path, &existing)?;
        return Ok(TrainingOutcome::Written {
            snapshot_id: existing.snapshot_id,
            path: out_path.to_path_buf(),
            metrics: metrics_for(
                existing
                    .training_manifest
                    .metrics
                    .get("auc")
                    .copied()
                    .unwrap_or(0.5),
                existing
                    .training_manifest
                    .metrics
                    .get("ece")
                    .copied()
                    .unwrap_or(1.0),
                prepared.len(),
            ),
            counts,
        });
    }

    let train = &prepared[..train_end];
    let calibrate = &prepared[train_end..cal_end];
    let test = &prepared[cal_end..];
    let train_x = vectorize(train, &feature_names);
    let train_y = labels_of(train);
    let cal_x = vectorize(calibrate, &feature_names);
    let cal_y = labels_of(calibrate);
    let test_x = vectorize(test, &feature_names);
    let test_y = labels_of(test);
    let l2 = config.l2_lambda;
    let steps = config.fit_steps;
    let fit = tokio::task::spawn_blocking(move || {
        let fit = fit_logistic(&train_x, &train_y, l2, steps);
        let cal_scores = cal_x
            .iter()
            .map(|row| linear_score(row, &fit.coefficients, fit.intercept))
            .collect::<Vec<_>>();
        let (platt_a, platt_b) = fit_platt(&cal_scores, &cal_y);
        let test_scores = test_x
            .iter()
            .map(|row| linear_score(row, &fit.coefficients, fit.intercept))
            .collect::<Vec<_>>();
        let test_probs = test_scores
            .iter()
            .map(|score| platt(*score, platt_a, platt_b))
            .collect::<Vec<_>>();
        let auc = roc_auc(&test_probs, &test_y);
        let ece = expected_calibration_error(&test_probs, &test_y, 10);
        (fit, platt_a, platt_b, auc, ece)
    })
    .await
    .context("actionability fit task panicked")?;
    let (fit, platt_a, platt_b, auc, ece) = fit;
    let metrics = metrics_for(auc, ece, prepared.len());
    let verdict = evaluate_gates(&metrics, &config.gates);
    if let GateVerdict::Refused { .. } = &verdict {
        let reason = verdict.explain();
        persist_run(
            store,
            principal,
            workspace,
            "refused",
            Some(&reason),
            &metrics,
            None,
        )
        .await?;
        return Ok(TrainingOutcome::Refused {
            reason,
            metrics,
            counts,
        });
    }

    let identity = majority_identity(&prepared);
    let cutoff_at = prepared
        .iter()
        .map(|row| row.occurred_at)
        .max()
        .unwrap_or(config.cutoff_at);
    let snapshot_id = snapshot_id_for(&dataset_digest, &feature_names, config.l2_lambda);
    let mut manifest_metrics = BTreeMap::new();
    manifest_metrics.insert("auc".to_string(), auc);
    manifest_metrics.insert("ece".to_string(), ece);
    manifest_metrics.insert("label_count".to_string(), prepared.len() as f64);
    manifest_metrics.insert("positive".to_string(), counts.positive as f64);
    manifest_metrics.insert("negative".to_string(), counts.negative as f64);
    manifest_metrics.insert("unlinked".to_string(), counts.unlinked as f64);
    let snapshot = ActionabilityModelSnapshot {
        snapshot_id: snapshot_id.clone(),
        model_version: MODEL_VERSION.to_string(),
        feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
        semantic_schema_version: identity
            .schema_version
            .unwrap_or(ATTENTION_SEMANTIC_SCHEMA_VERSION),
        semantic_extractor_contract: identity
            .extractor_contract
            .unwrap_or_else(|| ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string()),
        semantic_prompt_version: identity
            .prompt_version
            .unwrap_or_else(|| "unspecified".to_string()),
        semantic_model: identity.model,
        semantic_profile: identity.profile,
        feature_names,
        coefficients: fit.coefficients,
        intercept: fit.intercept,
        l2_lambda: config.l2_lambda,
        platt_a,
        platt_b,
        trained_at: chrono::Utc::now().timestamp_millis(),
        training_manifest: ActionabilityTrainingManifest {
            dataset_digest,
            data_cutoff_at: cutoff_at,
            split_strategy: SPLIT_STRATEGY.to_string(),
            group_keys: vec!["occurred_at".to_string()],
            positive_outcomes: positive_outcomes()
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            negative_outcomes: negative_outcomes()
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            excluded_outcomes: excluded_outcomes()
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            metrics: manifest_metrics,
        },
    };
    snapshot
        .validate()
        .context("fitted actionability snapshot failed the serving contract")?;
    write_snapshot_atomically(out_path, &snapshot)?;
    persist_run(
        store,
        principal,
        workspace,
        "written",
        None,
        &metrics,
        Some(&snapshot.snapshot_id),
    )
    .await?;
    Ok(TrainingOutcome::Written {
        snapshot_id: snapshot.snapshot_id,
        path: out_path.to_path_buf(),
        metrics,
        counts,
    })
}

struct PreparedExample {
    occurred_at: i64,
    label: f64,
    features: BTreeMap<String, f64>,
    extractor_contract: Option<String>,
    prompt_version: Option<String>,
    schema_version: Option<u32>,
    model: Option<String>,
    profile: Option<String>,
}

#[derive(Default)]
struct ProducerIdentity {
    extractor_contract: Option<String>,
    prompt_version: Option<String>,
    schema_version: Option<u32>,
    model: Option<String>,
    profile: Option<String>,
}

fn prepare_examples(rows: &[ActionabilityTrainingRow]) -> Vec<PreparedExample> {
    let mut prepared = Vec::new();
    for row in rows {
        if classify_label(row.outcome, row.reason.as_deref()) == LabelClass::Excluded {
            continue;
        }
        let Some(features) = row.features.clone() else {
            continue;
        };
        if row.feature_contract.as_deref() != Some(ACTIONABILITY_FEATURE_CONTRACT) {
            continue;
        }
        if row.semantic_extractor_contract.as_deref() != Some(ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT)
        {
            continue;
        }
        let label = match classify_label(row.outcome, row.reason.as_deref()) {
            LabelClass::Positive => 1.0,
            LabelClass::Negative => 0.0,
            LabelClass::Excluded => continue,
        };
        prepared.push(PreparedExample {
            occurred_at: row.occurred_at,
            label,
            features,
            extractor_contract: row.semantic_extractor_contract.clone(),
            prompt_version: row.semantic_prompt_version.clone(),
            schema_version: row.semantic_schema_version,
            model: row.semantic_model.clone(),
            profile: row.semantic_profile.clone(),
        });
    }
    prepared.sort_by_key(|row| (row.occurred_at, ordered_label_key(row.label)));
    filter_to_majority_identity(prepared)
}

fn ordered_label_key(label: f64) -> u8 {
    if label >= 0.5 {
        1
    } else {
        0
    }
}

fn filter_to_majority_identity(rows: Vec<PreparedExample>) -> Vec<PreparedExample> {
    if rows.is_empty() {
        return rows;
    }
    let mut counts = HashMap::<(Option<String>, Option<String>, Option<String>), usize>::new();
    for row in &rows {
        *counts
            .entry((
                row.prompt_version.clone(),
                row.model.clone(),
                row.profile.clone(),
            ))
            .or_default() += 1;
    }
    let majority = counts
        .into_iter()
        .max_by(|(left_key, left), (right_key, right)| {
            left.cmp(right).then_with(|| left_key.cmp(right_key))
        })
        .map(|(key, _)| key);
    let Some((prompt, model, profile)) = majority else {
        return rows;
    };
    rows.into_iter()
        .filter(|row| row.prompt_version == prompt && row.model == model && row.profile == profile)
        .collect()
}

fn majority_identity(rows: &[PreparedExample]) -> ProducerIdentity {
    rows.first()
        .map(|row| ProducerIdentity {
            extractor_contract: row.extractor_contract.clone(),
            prompt_version: row.prompt_version.clone(),
            schema_version: row.schema_version,
            model: row.model.clone(),
            profile: row.profile.clone(),
        })
        .unwrap_or_default()
}

fn collect_feature_names(rows: &[PreparedExample]) -> Vec<String> {
    let mut names = BTreeMap::new();
    for row in rows {
        for name in row.features.keys() {
            if super::actionability::supported_training_feature(name) {
                names.insert(name.clone(), ());
            }
        }
    }
    names.into_keys().collect()
}

fn vectorize(rows: &[PreparedExample], names: &[String]) -> Vec<Vec<f64>> {
    rows.iter()
        .map(|row| {
            names
                .iter()
                .map(|name| row.features.get(name).copied().unwrap_or(0.0))
                .collect()
        })
        .collect()
}

fn labels_of(rows: &[PreparedExample]) -> Vec<f64> {
    rows.iter().map(|row| row.label).collect()
}

/// Train / calibrate / test cut points. Platt is fit on calibrate and scored
/// on test so the ECE gate is not in-sample.
fn temporal_three_way(len: usize, fraction: f64) -> Option<(usize, usize)> {
    if len < 3 {
        return None;
    }
    let fraction = fraction.clamp(0.05, 0.4);
    let mut test = ((len as f64) * fraction).ceil() as usize;
    let mut cal = ((len as f64) * fraction).ceil() as usize;
    test = test.max(1);
    cal = cal.max(1);
    if test + cal >= len {
        let holdout = len - 1;
        test = (holdout / 2).max(1);
        cal = holdout.saturating_sub(test);
        if cal == 0 {
            return None;
        }
    }
    let train_end = len - test - cal;
    if train_end == 0 {
        return None;
    }
    Some((train_end, train_end + cal))
}

async fn reuse_matching_snapshot(
    store: &AttentionLearningStore,
    principal: &str,
    workspace: &str,
    dataset_digest: &str,
    feature_names: &[String],
    l2: f64,
) -> Result<Option<ActionabilityModelSnapshot>> {
    let Some(run) = store
        .latest_training_run(principal, workspace, TRAINING_SLICE)
        .await?
    else {
        return Ok(None);
    };
    if run.status != "written" {
        return Ok(None);
    }
    let Some(snapshot_id) = run.snapshot_id.as_deref() else {
        return Ok(None);
    };
    let Some(existing) = store.get_actionability_snapshot(snapshot_id).await? else {
        return Ok(None);
    };
    if existing.training_manifest.dataset_digest == dataset_digest
        && existing.feature_names == feature_names
        && (existing.l2_lambda - l2).abs() <= f64::EPSILON
    {
        return Ok(Some(existing));
    }
    Ok(None)
}

fn dataset_digest(rows: &[PreparedExample], names: &[String]) -> String {
    let payload = serde_json::json!({
        "names": names,
        "rows": rows.iter().map(|row| {
            serde_json::json!({
                "occurred_at": row.occurred_at,
                "label": row.label,
                "features": names.iter().map(|name| row.features.get(name).copied().unwrap_or(0.0)).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_vec(&payload).unwrap_or_default();
    blake3::hash(&encoded).to_hex().to_string()
}

fn snapshot_id_for(dataset_digest: &str, names: &[String], l2: f64) -> String {
    let seed = serde_json::json!({
        "dataset": dataset_digest,
        "features": names,
        "l2": l2,
    });
    let encoded = serde_json::to_vec(&seed).unwrap_or_default();
    format!(
        "actionability-{}",
        &blake3::hash(&encoded).to_hex().to_string()[..16]
    )
}

fn write_snapshot_atomically(path: &Path, snapshot: &ActionabilityModelSnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating snapshot directory {}", parent.display()))?;
        }
    }
    let encoded = serde_json::to_string_pretty(snapshot)?;
    let tmp = snapshot_tmp_path(path);
    std::fs::write(&tmp, encoded.as_bytes())
        .with_context(|| format!("writing snapshot temp {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("publishing snapshot {}", path.display()))?;
    Ok(())
}

pub(super) fn snapshot_tmp_path(path: &Path) -> PathBuf {
    let name = format!(
        "{}.{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("snapshot.json"),
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

async fn persist_run(
    store: &AttentionLearningStore,
    principal: &str,
    workspace: &str,
    status: &str,
    reason: Option<&str>,
    metrics: &TrainingMetrics,
    snapshot_id: Option<&str>,
) -> Result<()> {
    let metrics_json = serde_json::to_string(metrics).ok();
    store
        .record_training_run(&AttentionTrainingRunRecord {
            run_id: uuid::Uuid::new_v4().to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            slice: TRAINING_SLICE.to_string(),
            status: status.to_string(),
            reason: reason.map(str::to_string),
            metrics_json,
            snapshot_id: snapshot_id.map(str::to_string),
            created_at: chrono::Utc::now().timestamp_millis(),
        })
        .await
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::config::AttentionRoutingMode;
    use crate::magician_v2::attention::learning::{
        actionability::{ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT, ATTENTION_SEMANTIC_SCHEMA_VERSION},
        routing::{
            AttentionDecisionContext, AttentionDecisionFeatureContracts, AttentionDecisionItem,
            AttentionRoute, AttentionRoutingEvaluation,
        },
        AttentionLabelQuality, AttentionOutcomeAttribution, AttentionOutcomeKind, AttentionSurface,
        RecordAttentionOutcome, SemanticAttentionCandidate, ATTENTION_ROUTING_FEATURE_CONTRACT,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static OUT_SEQ: AtomicU64 = AtomicU64::new(0);

    fn out_path() -> PathBuf {
        let n = OUT_SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "attention-actionability-train-{}-{n}.json",
            std::process::id()
        ))
    }

    fn config() -> ActionabilityTrainingConfig {
        ActionabilityTrainingConfig {
            gates: GateConfig {
                min_labels: 20,
                max_ece: 0.10,
            },
            ..ActionabilityTrainingConfig::default()
        }
    }

    fn store() -> AttentionLearningStore {
        let directory = tempfile::TempDir::new().expect("creating attention learning temp dir");
        AttentionLearningStore::open(&directory.keep()).expect("opening attention learning store")
    }

    async fn seeded_store(labels: usize) -> AttentionLearningStore {
        let store = store();
        let mut items = Vec::with_capacity(labels);
        for index in 0..labels {
            let positive = index % 2 == 0;
            // Complementary serve-time features. A single probability with
            // the default decaying-rate fit stays mid-calibrated and the
            // ECE gate refuses; direct-request vs broadcast is the actual
            // serving contrast and clears 0.10.
            let feature_values = if positive {
                BTreeMap::from([
                    ("semantic.direct_request_probability".to_string(), 1.0),
                    ("semantic.broadcast_probability".to_string(), 0.0),
                ])
            } else {
                BTreeMap::from([
                    ("semantic.direct_request_probability".to_string(), 0.0),
                    ("semantic.broadcast_probability".to_string(), 1.0),
                ])
            };
            items.push(AttentionDecisionItem {
                decision_id: "decision-train".to_string(),
                candidate_id: format!("follow_up:candidate-{index}"),
                source_revision: Some("distill:7".to_string()),
                feature_values: Some(feature_values),
                source_family: "comms_ingest".to_string(),
                hard_eligible: true,
                ineligibility_reason: None,
                baseline_route: AttentionRoute::FollowUp,
                learned_route: AttentionRoute::FollowUp,
                served_route: AttentionRoute::FollowUp,
                routing_mode: AttentionRoutingMode::Baseline,
                routing_snapshot_id: None,
                routing_model_version: None,
                learned_route_confidence: None,
                utility_margin: None,
                route_reason: "baseline_mode".to_string(),
                route_applied: false,
                canary_assigned: false,
                owner_action_required_probability: None,
                information_value_probability: None,
                follow_up_utility: None,
                worth_a_look_utility: None,
                uncertainty: None,
                cluster_id: format!("cluster-{index}"),
                cluster_size: 1,
                representative: true,
                baseline_rank: index + 1,
                learned_rank: index + 1,
                served_rank: index + 1,
                selected: true,
                selection_probability: 1.0,
                exploration: false,
                feature_snapshot_digest: None,
                bandit_decision: None,
                extraction_status:
                    crate::magician_v2::attention::learning::SemanticExtractionStatus::Succeeded,
                feature_contracts: AttentionDecisionFeatureContracts {
                    routing_feature_contract: ATTENTION_ROUTING_FEATURE_CONTRACT.to_string(),
                    routing_snapshot_id: None,
                    actionability_snapshot_id: None,
                    actionability_model_version: None,
                    grouping_snapshot_id: None,
                    grouping_model_version: None,
                    semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
                    semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
                    semantic_prompt_version: Some("1.1.0".to_string()),
                    semantic_model: None,
                    semantic_profile: None,
                },
            });
        }
        let evaluation = AttentionRoutingEvaluation {
            decision_id: "decision-train".to_string(),
            decided_at: 10,
            surface: AttentionSurface::FollowUp,
            mode: AttentionRoutingMode::Baseline,
            snapshot_id: None,
            model_version: None,
            policy_seed_identity: "slice4".to_string(),
            canary_assigned: false,
            complete_cross_lane_universe: false,
            degradation_reason: None,
            bandit_health: None,
            items,
        };
        store
            .record_decision(
                "p",
                "w",
                "candidate-set",
                AttentionDecisionContext {
                    queue_size: labels,
                    ..Default::default()
                },
                4,
                labels,
                &evaluation,
            )
            .await
            .expect("recording training decision");
        for index in 0..labels {
            let positive = index % 2 == 0;
            let outcome = if positive {
                AttentionOutcomeKind::ActionCompleted
            } else {
                AttentionOutcomeKind::Irrelevant
            };
            let request = RecordAttentionOutcome {
                event_id: format!("event-{index}"),
                candidate: SemanticAttentionCandidate {
                    candidate_id: format!("follow_up:candidate-{index}"),
                    source_revision: Some("distill:7".to_string()),
                    semantic_text: "safe summary".to_string(),
                    existing_embedding: None,
                    actionability_features: None,
                    grouping_features: None,
                },
                outcome,
                reason: None,
                label_quality: AttentionLabelQuality::Strong,
                occurred_at: 1_000 + index as i64,
                attribution: Some(AttentionOutcomeAttribution {
                    decision_id: "decision-train".to_string(),
                    candidate_id: format!("follow_up:candidate-{index}"),
                    source_revision: Some("distill:7".to_string()),
                    impression_id: Some(format!("impression-{index}")),
                    delivery_id: None,
                }),
            };
            store
                .record_outcome("p", "w", AttentionSurface::FollowUp, &request, None)
                .await
                .expect("recording training outcome");
        }
        store
    }

    #[test]
    fn temporal_three_way_keeps_train_calibrate_and_test() {
        let (train_end, cal_end) = temporal_three_way(10, 0.2).expect("split");
        assert!(train_end >= 1);
        assert!(cal_end > train_end);
        assert!(cal_end < 10);
        assert!(temporal_three_way(2, 0.2).is_none());
    }

    #[tokio::test]
    async fn training_refuses_and_writes_nothing_when_gates_fail() {
        let store = seeded_store(8).await;
        let path = out_path();
        let _ = std::fs::remove_file(&path);
        let outcome = train_actionability(&store, "p", "w", &config(), &path)
            .await
            .unwrap();
        assert!(matches!(outcome, TrainingOutcome::Refused { .. }));
        assert!(!path.exists(), "a refused run must not leave an artifact");
    }

    #[tokio::test]
    async fn a_passing_run_writes_a_snapshot_that_validates() {
        let store = seeded_store(80).await;
        let path = out_path();
        let _ = std::fs::remove_file(&path);
        let outcome = train_actionability(&store, "p", "w", &config(), &path)
            .await
            .unwrap();
        match &outcome {
            TrainingOutcome::Written { .. } => {},
            TrainingOutcome::Refused {
                reason,
                metrics,
                counts,
            } => panic!(
                "expected Written, refused: {reason}; metrics={metrics:?}; counts={counts:?}"
            ),
        }

        let snapshot: ActionabilityModelSnapshot =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        snapshot
            .validate()
            .expect("emitted snapshot must satisfy the serving contract");
        assert!(!snapshot.training_manifest.positive_outcomes.is_empty());
        assert!(snapshot
            .training_manifest
            .excluded_outcomes
            .contains(&"neutral_seen".to_string()));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_raw_follow_up_outcome_joins_the_canonical_decision_item() {
        let store = seeded_store(2).await;
        let request = RecordAttentionOutcome {
            event_id: "raw-join".to_string(),
            candidate: SemanticAttentionCandidate {
                candidate_id: "candidate-0".to_string(),
                source_revision: Some("distill:7".to_string()),
                semantic_text: "safe summary".to_string(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: None,
            },
            outcome: AttentionOutcomeKind::ActionCompleted,
            reason: None,
            label_quality: AttentionLabelQuality::Strong,
            occurred_at: 2_000,
            attribution: Some(AttentionOutcomeAttribution {
                decision_id: "decision-train".to_string(),
                candidate_id: "follow_up:candidate-0".to_string(),
                source_revision: Some("distill:7".to_string()),
                impression_id: Some("impression-raw".to_string()),
                delivery_id: None,
            }),
        };
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &request, None)
            .await
            .expect("recording raw-id outcome");
        let rows = store
            .list_actionability_training_rows("p", "w", i64::MAX)
            .await
            .expect("listing training rows");
        let joined = rows
            .iter()
            .find(|row| row.outcome_id.contains("raw") || row.candidate_id == "candidate-0")
            .expect("raw outcome persisted");
        assert!(
            joined.features.is_some(),
            "canonical decision features must join a raw follow-up outcome"
        );
    }

    #[tokio::test]
    async fn last_served_features_train_an_unattributed_completion() {
        let store = seeded_store(2).await;
        let request = RecordAttentionOutcome {
            event_id: "reconcile-complete".to_string(),
            candidate: SemanticAttentionCandidate {
                candidate_id: "candidate-1".to_string(),
                source_revision: Some("distill:7".to_string()),
                semantic_text: "safe summary".to_string(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: None,
            },
            outcome: AttentionOutcomeKind::ActionCompleted,
            reason: Some("reconciled_owner_sent".to_string()),
            label_quality: AttentionLabelQuality::Strong,
            occurred_at: 2_001,
            attribution: None,
        };
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &request, None)
            .await
            .expect("recording unattributed completion");
        let rows = store
            .list_actionability_training_rows("p", "w", i64::MAX)
            .await
            .expect("listing training rows");
        let joined = rows
            .iter()
            .find(|row| row.candidate_id == "candidate-1" && row.decision_id.is_none())
            .expect("unattributed outcome persisted");
        assert!(
            joined.features.is_some(),
            "last-served binding must supply features without inventing a decision_id"
        );
    }
}
