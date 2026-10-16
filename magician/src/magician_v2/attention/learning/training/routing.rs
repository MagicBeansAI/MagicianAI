//! Offline routing trainer: two per-lane utility heads plus a hide threshold.
//!
//! Lane labels come from `not_actionable` (demote to Worth a look) and
//! `action_completed` (confirm or promote to For you). Useful never chooses a
//! lane. The live product applies Slice-1 kNN without waiting for this fitted
//! snapshot; this trainer remains the optional later head.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use anyhow::{Context, Result};

use super::{
    calibration::{expected_calibration_error, fit_platt, platt, roc_auc},
    gates::{evaluate_gates, GateConfig, GateVerdict, TrainingMetrics},
    logistic::{fit_logistic, linear_score},
    TrainingOutcome,
};
use crate::magician_v2::attention::learning::{
    routing::{
        routing_feature_vector, AttentionRoute, AttentionRoutingPolicySnapshot,
        AttentionRoutingTrainingManifest, AttentionUtilityHead, ATTENTION_ROUTING_FEATURE_CONTRACT,
    },
    store::{AttentionLearningStore, AttentionTrainingRunRecord, RoutingTrainingRow},
    ACTIONABILITY_FEATURE_CONTRACT, ATTENTION_PAIR_FEATURE_CONTRACT,
};

const MODEL_VERSION: &str = "routing_dual_head_platt_v1";
const SPLIT_STRATEGY: &str = "temporal";
const TRAINING_SLICE: &str = "routing";

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingTrainingConfig {
    pub cutoff_at: i64,
    pub holdout_fraction: f64,
    pub l2_lambda: f64,
    pub fit_steps: usize,
    pub gates: GateConfig,
}

impl Default for RoutingTrainingConfig {
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

pub async fn train_routing(
    store: &AttentionLearningStore,
    principal: &str,
    workspace: &str,
    config: &RoutingTrainingConfig,
    out_path: &Path,
) -> Result<TrainingOutcome> {
    let actionability = store
        .actionability_scope_install(principal, workspace)
        .await?;
    let Some(actionability) = actionability else {
        return refuse(
            store,
            principal,
            workspace,
            TrainingMetrics {
                label_count: 0,
                positive: 0,
                negative: 0,
                usable: 0,
                unlinked: 0,
                auc: 0.5,
                ece: 1.0,
            },
            "actionability snapshot is not installed; routing cannot train",
        )
        .await;
    };
    let Some(actionability_snapshot) = store
        .get_actionability_snapshot(&actionability.snapshot_id)
        .await?
    else {
        return refuse(
            store,
            principal,
            workspace,
            TrainingMetrics {
                label_count: 0,
                positive: 0,
                negative: 0,
                usable: 0,
                unlinked: 0,
                auc: 0.5,
                ece: 1.0,
            },
            "installed actionability snapshot is missing from the store",
        )
        .await;
    };

    let rows = store
        .list_routing_training_rows(principal, workspace, config.cutoff_at)
        .await?;
    let mut unlinked = 0_usize;
    let mut excluded = 0_usize;
    let mut prepared = Vec::new();
    for row in &rows {
        if row.decision_id.as_deref().is_none_or(str::is_empty) {
            if row.outcome.routing_target().is_some() {
                unlinked += 1;
            } else {
                excluded += 1;
            }
            continue;
        }
        let Some(target) = row.outcome.routing_target() else {
            excluded += 1;
            continue;
        };
        let Some(features) = reconstruct_routing_features(row) else {
            excluded += 1;
            continue;
        };
        prepared.push(PreparedRoutingExample {
            occurred_at: row.occurred_at,
            target,
            features,
        });
    }
    prepared.sort_by_key(|row| (row.occurred_at, route_sort_key(row.target)));

    let follow_positives = prepared
        .iter()
        .filter(|row| row.target == AttentionRoute::FollowUp)
        .count();
    let worth_positives = prepared
        .iter()
        .filter(|row| row.target == AttentionRoute::WorthALook)
        .count();
    let hide_labels = prepared
        .iter()
        .filter(|row| row.target == AttentionRoute::NonSurfaced)
        .count();
    let counts_metrics = TrainingMetrics {
        label_count: prepared.len(),
        positive: follow_positives.saturating_add(worth_positives),
        negative: hide_labels,
        usable: prepared.len(),
        unlinked,
        auc: 0.5,
        ece: 1.0,
    };
    if prepared.len() < 2 {
        let reason = evaluate_gates(&counts_metrics, &config.gates).explain();
        return refuse(store, principal, workspace, counts_metrics, reason).await;
    }

    let holdout_start = temporal_holdout_start(prepared.len(), config.holdout_fraction);
    let (train, holdout) = prepared.split_at(holdout_start);
    if train.is_empty() || holdout.is_empty() {
        return refuse(
            store,
            principal,
            workspace,
            counts_metrics,
            format!(
                "temporal split needs both train and holdout; usable labels {}",
                prepared.len()
            ),
        )
        .await;
    }

    let feature_names = routing_feature_vector(0.0, 0.0, 0.0, 0.0, 0.0, 0).names;
    let train_x = vectorize(train, &feature_names);
    let holdout_x = vectorize(holdout, &feature_names);
    let follow_train_y = head_labels(train, AttentionRoute::FollowUp);
    let worth_train_y = head_labels(train, AttentionRoute::WorthALook);
    let follow_fit = fit_logistic(
        &train_x,
        &follow_train_y,
        config.l2_lambda,
        config.fit_steps,
    );
    let worth_fit = fit_logistic(&train_x, &worth_train_y, config.l2_lambda, config.fit_steps);

    let follow_scores = holdout_x
        .iter()
        .map(|row| linear_score(row, &follow_fit.coefficients, follow_fit.intercept))
        .collect::<Vec<_>>();
    let worth_scores = holdout_x
        .iter()
        .map(|row| linear_score(row, &worth_fit.coefficients, worth_fit.intercept))
        .collect::<Vec<_>>();
    let follow_holdout_y = head_labels(holdout, AttentionRoute::FollowUp);
    let worth_holdout_y = head_labels(holdout, AttentionRoute::WorthALook);
    let (follow_a, follow_b) = fit_platt(&follow_scores, &follow_holdout_y);
    let (worth_a, worth_b) = fit_platt(&worth_scores, &worth_holdout_y);
    let follow_probs = follow_scores
        .iter()
        .map(|score| platt(*score, follow_a, follow_b))
        .collect::<Vec<_>>();
    let worth_probs = worth_scores
        .iter()
        .map(|score| platt(*score, worth_a, worth_b))
        .collect::<Vec<_>>();
    let follow_auc = roc_auc(&follow_probs, &follow_holdout_y);
    let worth_auc = roc_auc(&worth_probs, &worth_holdout_y);
    let follow_ece = expected_calibration_error(&follow_probs, &follow_holdout_y, 10);
    let worth_ece = expected_calibration_error(&worth_probs, &worth_holdout_y, 10);
    let auc = follow_auc.min(worth_auc);
    let ece = follow_ece.max(worth_ece);
    let metrics = TrainingMetrics {
        label_count: prepared.len(),
        positive: follow_positives.saturating_add(worth_positives),
        negative: hide_labels,
        usable: prepared.len(),
        unlinked,
        auc,
        ece,
    };
    let mut extra_reasons = Vec::new();
    if follow_positives == 0 {
        extra_reasons.push("follow_up labels are empty".to_string());
    }
    if worth_positives == 0 {
        extra_reasons.push("worth_a_look labels are empty".to_string());
    }
    match evaluate_gates(&metrics, &config.gates) {
        GateVerdict::Refused { mut reasons } => {
            reasons.extend(extra_reasons);
            return refuse(store, principal, workspace, metrics, reasons.join("; ")).await;
        },
        GateVerdict::Passed if !extra_reasons.is_empty() => {
            return refuse(
                store,
                principal,
                workspace,
                metrics,
                extra_reasons.join("; "),
            )
            .await;
        },
        GateVerdict::Passed => {},
    }

    let min_surface = minimum_surface_utility(&follow_probs, &worth_probs, holdout);
    let margins: Vec<f64> = follow_probs
        .iter()
        .zip(&worth_probs)
        .map(|(follow, worth)| (follow - worth).abs())
        .collect();
    let correct: Vec<f64> = holdout
        .iter()
        .zip(follow_probs.iter().zip(&worth_probs))
        .map(|(example, (follow, worth))| {
            let predicted = if follow.max(*worth) < min_surface {
                AttentionRoute::NonSurfaced
            } else if follow >= worth {
                AttentionRoute::FollowUp
            } else {
                AttentionRoute::WorthALook
            };
            if predicted == example.target {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    let (confidence_a, confidence_b) = fit_platt(&margins, &correct);

    let dataset_digest = dataset_digest(&prepared, &feature_names);
    let snapshot_id = format!(
        "routing-{}",
        &blake3::hash(
            serde_json::to_vec(&serde_json::json!({
                "dataset": dataset_digest,
                "actionability": actionability_snapshot.snapshot_id,
                "l2": config.l2_lambda,
            }))
            .unwrap_or_default()
            .as_slice()
        )
        .to_hex()
        .to_string()[..16]
    );
    let cutoff_at = prepared
        .iter()
        .map(|row| row.occurred_at)
        .max()
        .unwrap_or(config.cutoff_at);
    let mut manifest_metrics = BTreeMap::new();
    manifest_metrics.insert("auc".to_string(), auc);
    manifest_metrics.insert("ece".to_string(), ece);
    manifest_metrics.insert("follow_up_auc".to_string(), follow_auc);
    manifest_metrics.insert("worth_a_look_auc".to_string(), worth_auc);
    manifest_metrics.insert("follow_up_positives".to_string(), follow_positives as f64);
    manifest_metrics.insert("worth_a_look_positives".to_string(), worth_positives as f64);
    manifest_metrics.insert("non_surfaced".to_string(), hide_labels as f64);
    manifest_metrics.insert("unlinked".to_string(), unlinked as f64);
    manifest_metrics.insert("excluded".to_string(), excluded as f64);
    let snapshot = AttentionRoutingPolicySnapshot {
        snapshot_id: snapshot_id.clone(),
        model_version: MODEL_VERSION.to_string(),
        feature_contract: ATTENTION_ROUTING_FEATURE_CONTRACT.to_string(),
        actionability_feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
        actionability_snapshot_id: actionability_snapshot.snapshot_id.clone(),
        actionability_model_version: actionability_snapshot.model_version.clone(),
        grouping_feature_contract: ATTENTION_PAIR_FEATURE_CONTRACT.to_string(),
        grouping_snapshot_id: None,
        grouping_model_version: None,
        semantic_schema_version: actionability_snapshot.semantic_schema_version,
        semantic_extractor_contract: actionability_snapshot.semantic_extractor_contract.clone(),
        semantic_prompt_version: actionability_snapshot.semantic_prompt_version.clone(),
        semantic_model: actionability_snapshot.semantic_model.clone(),
        semantic_profile: actionability_snapshot.semantic_profile.clone(),
        feature_names,
        follow_up: AttentionUtilityHead {
            coefficients: follow_fit.coefficients,
            intercept: follow_fit.intercept,
            platt_a: follow_a,
            platt_b: follow_b,
        },
        worth_a_look: AttentionUtilityHead {
            coefficients: worth_fit.coefficients,
            intercept: worth_fit.intercept,
            platt_a: worth_a,
            platt_b: worth_b,
        },
        minimum_surface_utility: min_surface,
        confidence_platt_a: confidence_a,
        confidence_platt_b: confidence_b,
        minimum_route_confidence: 0.55,
        minimum_utility_margin: 0.02,
        trained_at: chrono::Utc::now().timestamp_millis(),
        training_manifest: AttentionRoutingTrainingManifest {
            dataset_digest,
            data_cutoff_at: cutoff_at,
            split_strategy: SPLIT_STRATEGY.to_string(),
            group_keys: vec!["occurred_at".to_string()],
            metrics: manifest_metrics,
        },
    };
    snapshot
        .validate()
        .context("fitted routing snapshot failed the serving contract")?;
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = super::snapshot_tmp_path(out_path);
    std::fs::write(&tmp, serde_json::to_string_pretty(&snapshot)?)?;
    std::fs::rename(&tmp, out_path)?;
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
        counts: super::labels::LabelSet {
            positive: follow_positives.saturating_add(worth_positives),
            negative: hide_labels,
            excluded,
            usable: prepared.len(),
            unlinked,
        },
    })
}

struct PreparedRoutingExample {
    occurred_at: i64,
    target: AttentionRoute,
    features: HashMap<String, f64>,
}

fn reconstruct_routing_features(row: &RoutingTrainingRow) -> Option<HashMap<String, f64>> {
    let actionability = row.owner_action_required_probability?;
    let features = row.features.as_ref()?;
    let extracted = routing_feature_vector(
        actionability,
        row.information_value_probability.unwrap_or_else(|| {
            features
                .get("semantic.information_value_probability")
                .copied()
                .unwrap_or(0.0)
        }),
        features
            .get("semantic.direct_request_probability")
            .copied()
            .unwrap_or(0.0),
        features
            .get("semantic.broadcast_probability")
            .copied()
            .unwrap_or(0.0),
        features
            .get("semantic.personal_obligation_probability")
            .copied()
            .unwrap_or(0.0),
        row.cluster_size.saturating_sub(1),
    );
    Some(extracted.names.into_iter().zip(extracted.values).collect())
}

fn head_labels(rows: &[PreparedRoutingExample], route: AttentionRoute) -> Vec<f64> {
    rows.iter()
        .map(|row| if row.target == route { 1.0 } else { 0.0 })
        .collect()
}

fn vectorize(rows: &[PreparedRoutingExample], names: &[String]) -> Vec<Vec<f64>> {
    rows.iter()
        .map(|row| {
            names
                .iter()
                .map(|name| row.features.get(name).copied().unwrap_or(0.0))
                .collect()
        })
        .collect()
}

fn temporal_holdout_start(len: usize, fraction: f64) -> usize {
    let fraction = fraction.clamp(0.05, 0.5);
    let holdout = ((len as f64) * fraction).ceil() as usize;
    len.saturating_sub(holdout.max(1))
        .max(1)
        .min(len.saturating_sub(1))
}

fn route_sort_key(route: AttentionRoute) -> u8 {
    match route {
        AttentionRoute::FollowUp => 2,
        AttentionRoute::WorthALook => 1,
        AttentionRoute::NonSurfaced => 0,
    }
}

fn minimum_surface_utility(
    follow: &[f64],
    worth: &[f64],
    holdout: &[PreparedRoutingExample],
) -> f64 {
    let mut positives = follow
        .iter()
        .zip(worth)
        .zip(holdout)
        .filter(|(_, example)| example.target != AttentionRoute::NonSurfaced)
        .map(|((follow, worth), _)| follow.max(*worth))
        .collect::<Vec<_>>();
    if positives.is_empty() {
        return 0.25;
    }
    positives.sort_by(|left, right| left.total_cmp(right));
    positives[positives.len() / 10].clamp(0.05, 0.45)
}

fn dataset_digest(rows: &[PreparedRoutingExample], names: &[String]) -> String {
    let payload = serde_json::json!({
        "names": names,
        "rows": rows.iter().map(|row| serde_json::json!({
            "occurred_at": row.occurred_at,
            "target": row.target.as_str(),
            "features": names.iter().map(|name| row.features.get(name).copied().unwrap_or(0.0)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    blake3::hash(&serde_json::to_vec(&payload).unwrap_or_default())
        .to_hex()
        .to_string()
}

async fn refuse(
    store: &AttentionLearningStore,
    principal: &str,
    workspace: &str,
    metrics: TrainingMetrics,
    reason: impl Into<String>,
) -> Result<TrainingOutcome> {
    let reason = reason.into();
    let counts = super::labels::LabelSet {
        positive: metrics.positive,
        negative: metrics.negative,
        excluded: 0,
        usable: metrics.usable,
        unlinked: metrics.unlinked,
    };
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
    Ok(TrainingOutcome::Refused {
        reason,
        metrics,
        counts,
    })
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
    store
        .record_training_run(&AttentionTrainingRunRecord {
            run_id: uuid::Uuid::new_v4().to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            slice: TRAINING_SLICE.to_string(),
            status: status.to_string(),
            reason: reason.map(str::to_string),
            metrics_json: serde_json::to_string(metrics).ok(),
            snapshot_id: snapshot_id.map(str::to_string),
            created_at: chrono::Utc::now().timestamp_millis(),
        })
        .await
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::attention::learning::AttentionOutcomeKind;

    #[test]
    fn owner_lane_corrections_set_routing_targets() {
        use crate::magician_v2::attention::learning::AttentionRoute;
        assert_eq!(
            AttentionOutcomeKind::NotActionable.routing_target(),
            Some(AttentionRoute::WorthALook)
        );
        assert_eq!(
            AttentionOutcomeKind::ActionCompleted.routing_target(),
            Some(AttentionRoute::FollowUp)
        );
        assert_eq!(AttentionOutcomeKind::Useful.routing_target(), None);
        assert_eq!(AttentionOutcomeKind::Irrelevant.routing_target(), None);
        assert_eq!(AttentionOutcomeKind::NeutralSeen.routing_target(), None);
    }
}
