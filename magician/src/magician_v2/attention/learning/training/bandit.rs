//! In-process Slice-5 trainer.
//!
//! This does not shuffle lists on its own. Magician writes an immutable prior
//! snapshot as soon as actionability and routing snapshots exist, installs it
//! in shadow (baseline order, posterior may learn), and later promotes the
//! same snapshot to canary after enough attributed posterior updates.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};

use super::{gates::TrainingMetrics, labels::LabelSet, TrainingOutcome};
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;
use crate::magician_v2::attention::learning::{
    bandit::{
        AttentionBanditPolicySnapshot, AttentionBanditRewardSpec, AttentionBanditTrainingManifest,
        ATTENTION_BANDIT_FEATURE_CONTRACT, ATTENTION_BANDIT_SEED_CONTRACT,
    },
    store::AttentionLearningStore,
    AttentionOutcomeKind, ACTIONABILITY_FEATURE_CONTRACT, ATTENTION_PAIR_FEATURE_CONTRACT,
    ATTENTION_ROUTING_FEATURE_CONTRACT,
};

const MODEL_VERSION: &str = "bayesian-linear-v1";
const SPLIT_STRATEGY: &str = "online_posterior";
const TRAINING_SLICE: &str = "bandit";
const SEED_IDENTITY: &str = "attention-bandit-auto-v1";

pub const MIN_CANARY_POSTERIOR_UPDATES: u64 = 40;

const FEATURE_NAMES: &[&str] = &[
    "routing.owner_action_required_probability",
    "routing.information_value_probability",
    "routing.follow_up_utility",
    "routing.worth_a_look_utility",
    "routing.uncertainty",
    "group.duplicate_exposure_cost_log1p",
    "context.queue_size_log1p",
    "context.recent_impression_count_log1p",
    "context.recent_action_count_log1p",
];

#[derive(Debug, Clone, PartialEq)]
pub struct BanditTrainingConfig {
    pub cutoff_at: i64,
    pub first_page_size: usize,
    pub slate_size: usize,
}

impl Default for BanditTrainingConfig {
    fn default() -> Self {
        Self {
            cutoff_at: i64::MAX,
            first_page_size: 50,
            slate_size: 8,
        }
    }
}

pub async fn train_bandit(
    store: &AttentionLearningStore,
    principal: &str,
    workspace: &str,
    config: &BanditTrainingConfig,
    out_path: &Path,
) -> Result<TrainingOutcome> {
    let Some(actionability) = store
        .actionability_scope_install(principal, workspace)
        .await?
    else {
        return refuse(
            store,
            principal,
            workspace,
            empty_metrics(),
            "actionability snapshot is not installed; bandit cannot train",
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
            empty_metrics(),
            "installed actionability snapshot is missing from the store",
        )
        .await;
    };
    let Some((routing_snapshot_id, routing_mode, _)) =
        store.routing_scope_install(principal, workspace).await?
    else {
        return refuse(
            store,
            principal,
            workspace,
            empty_metrics(),
            "routing snapshot is not installed; bandit cannot train",
        )
        .await;
    };
    if routing_mode == crate::config::AttentionRoutingMode::Baseline {
        return refuse(
            store,
            principal,
            workspace,
            empty_metrics(),
            "routing install is baseline; waiting for a scoring snapshot",
        )
        .await;
    }
    let Some(routing_snapshot) = store
        .get_routing_policy_snapshot(&routing_snapshot_id)
        .await?
    else {
        return refuse(
            store,
            principal,
            workspace,
            empty_metrics(),
            "installed routing snapshot is missing from the store",
        )
        .await;
    };

    let feature_names = FEATURE_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    let snapshot_id = snapshot_id_for(
        &routing_snapshot.snapshot_id,
        &actionability_snapshot.snapshot_id,
        routing_snapshot.grouping_snapshot_id.as_deref(),
        &feature_names,
    );
    if store
        .get_bandit_policy_snapshot(&snapshot_id)
        .await?
        .is_some()
    {
        let metrics = TrainingMetrics {
            label_count: 0,
            positive: 0,
            negative: 0,
            usable: 0,
            unlinked: 0,
            auc: 0.5,
            ece: 0.0,
        };
        persist_run(
            store,
            principal,
            workspace,
            "written",
            None,
            &metrics,
            Some(&snapshot_id),
        )
        .await?;
        return Ok(TrainingOutcome::Written {
            snapshot_id,
            path: out_path.to_path_buf(),
            metrics,
            counts: LabelSet {
                positive: 0,
                negative: 0,
                excluded: 0,
                usable: 0,
                unlinked: 0,
            },
        });
    }

    let dimension = feature_names.len();
    let prior_mean = vec![0.0; dimension];
    let prior_precision = (0..dimension)
        .map(|row| {
            (0..dimension)
                .map(|column| if row == column { 1.0 } else { 0.0 })
                .collect()
        })
        .collect();
    let trained_at = chrono::Utc::now().timestamp_millis();
    let mut manifest_metrics = BTreeMap::new();
    manifest_metrics.insert("prior_only".to_string(), 1.0);
    let snapshot = AttentionBanditPolicySnapshot {
        snapshot_id: snapshot_id.clone(),
        model_version: MODEL_VERSION.to_string(),
        feature_contract: ATTENTION_BANDIT_FEATURE_CONTRACT.to_string(),
        routing_feature_contract: ATTENTION_ROUTING_FEATURE_CONTRACT.to_string(),
        routing_snapshot_id: routing_snapshot.snapshot_id.clone(),
        routing_model_version: routing_snapshot.model_version.clone(),
        actionability_feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
        actionability_snapshot_id: actionability_snapshot.snapshot_id.clone(),
        actionability_model_version: actionability_snapshot.model_version.clone(),
        grouping_feature_contract: ATTENTION_PAIR_FEATURE_CONTRACT.to_string(),
        grouping_snapshot_id: routing_snapshot.grouping_snapshot_id.clone(),
        grouping_model_version: routing_snapshot.grouping_model_version.clone(),
        semantic_schema_version: actionability_snapshot.semantic_schema_version,
        semantic_extractor_contract: actionability_snapshot.semantic_extractor_contract.clone(),
        semantic_prompt_version: actionability_snapshot.semantic_prompt_version.clone(),
        semantic_model: actionability_snapshot.semantic_model.clone(),
        semantic_profile: actionability_snapshot.semantic_profile.clone(),
        feature_names,
        prior_mean,
        prior_precision,
        observation_noise_variance: 1.0,
        posterior_draw_count: 64,
        exploration_floor: 0.05,
        first_page_size: config.first_page_size.max(1).min(200),
        slate_size: config
            .slate_size
            .max(1)
            .min(config.first_page_size.max(1).min(200)),
        seed_contract: ATTENTION_BANDIT_SEED_CONTRACT.to_string(),
        seed_identity: SEED_IDENTITY.to_string(),
        reward_mapping: default_reward_mapping(),
        attribution_window_ms: 86_400_000,
        require_verified_impression: false,
        trained_at,
        training_manifest: AttentionBanditTrainingManifest {
            dataset_digest: snapshot_id.clone(),
            data_cutoff_at: config.cutoff_at.min(trained_at),
            split_strategy: SPLIT_STRATEGY.to_string(),
            group_keys: vec![
                "principal".to_string(),
                "workspace".to_string(),
                "surface".to_string(),
            ],
            metrics: manifest_metrics,
        },
    };
    snapshot
        .validate()
        .context("trained bandit snapshot failed serving validation")?;
    write_snapshot(out_path, &snapshot)?;
    let metrics = TrainingMetrics {
        label_count: 0,
        positive: 0,
        negative: 0,
        usable: 0,
        unlinked: 0,
        auc: 0.5,
        ece: 0.0,
    };
    persist_run(
        store,
        principal,
        workspace,
        "written",
        Some("prior_only_shadow_ready"),
        &metrics,
        Some(&snapshot_id),
    )
    .await?;
    Ok(TrainingOutcome::Written {
        snapshot_id,
        path: out_path.to_path_buf(),
        metrics,
        counts: LabelSet {
            positive: 0,
            negative: 0,
            excluded: 0,
            usable: 0,
            unlinked: 0,
        },
    })
}

fn default_reward_mapping() -> Vec<AttentionBanditRewardSpec> {
    [
        (AttentionOutcomeKind::Useful, Some(1.0)),
        (AttentionOutcomeKind::ActionCompleted, Some(1.0)),
        (AttentionOutcomeKind::Irrelevant, Some(-1.0)),
        (AttentionOutcomeKind::NotActionable, Some(-1.0)),
        (AttentionOutcomeKind::Obsolete, Some(-1.0)),
        (AttentionOutcomeKind::NotOwner, Some(-1.0)),
        (AttentionOutcomeKind::DuplicateOf, None),
        (AttentionOutcomeKind::NeutralSeen, None),
        (AttentionOutcomeKind::TimingNegative, None),
    ]
    .into_iter()
    .map(|(outcome, reward)| AttentionBanditRewardSpec {
        outcome,
        reward,
        strong_strength: 1.0,
        weak_strength: 0.5,
        unknown_strength: 0.0,
    })
    .collect()
}

fn snapshot_id_for(
    routing_snapshot_id: &str,
    actionability_snapshot_id: &str,
    grouping_snapshot_id: Option<&str>,
    names: &[String],
) -> String {
    let seed = serde_json::json!({
        "model": MODEL_VERSION,
        "routing": routing_snapshot_id,
        "actionability": actionability_snapshot_id,
        "grouping": grouping_snapshot_id,
        "features": names,
    });
    format!(
        "bandit-{}",
        &blake3::hash(&serde_json::to_vec(&seed).unwrap_or_default()).to_hex()[..16]
    )
}

fn empty_metrics() -> TrainingMetrics {
    TrainingMetrics {
        label_count: 0,
        positive: 0,
        negative: 0,
        usable: 0,
        unlinked: 0,
        auc: 0.5,
        ece: 1.0,
    }
}

fn write_snapshot(path: &Path, snapshot: &AttentionBanditPolicySnapshot) -> Result<()> {
    let encoded = serde_json::to_string_pretty(snapshot)?;
    write_bytes_durably_sync(path, encoded.as_bytes())
        .with_context(|| format!("publishing snapshot {}", path.display()))?;
    Ok(())
}

async fn refuse(
    store: &AttentionLearningStore,
    principal: &str,
    workspace: &str,
    metrics: TrainingMetrics,
    reason: impl Into<String>,
) -> Result<TrainingOutcome> {
    let reason = reason.into();
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
        counts: LabelSet {
            positive: 0,
            negative: 0,
            excluded: 0,
            usable: 0,
            unlinked: 0,
        },
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
        .record_training_run(
            &crate::magician_v2::attention::learning::AttentionTrainingRunRecord {
                run_id: uuid::Uuid::new_v4().to_string(),
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                slice: TRAINING_SLICE.to_string(),
                status: status.to_string(),
                reason: reason.map(str::to_string),
                metrics_json: serde_json::to_string(metrics).ok(),
                snapshot_id: snapshot_id.map(str::to_string),
                created_at: chrono::Utc::now().timestamp_millis(),
            },
        )
        .await
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bandit_training_refuses_until_upstream_snapshots_exist() {
        let directory = tempfile::TempDir::new().expect("temp dir");
        let store = AttentionLearningStore::open(directory.path()).expect("store");
        let path = directory.path().join("bandit.json");
        let outcome = train_bandit(&store, "p", "w", &BanditTrainingConfig::default(), &path)
            .await
            .expect("train");
        match outcome {
            TrainingOutcome::Refused { reason, .. } => {
                assert!(reason.contains("actionability snapshot is not installed"));
            },
            other => panic!("expected refuse, got {other:?}"),
        }
        assert!(!path.exists());
    }
}
