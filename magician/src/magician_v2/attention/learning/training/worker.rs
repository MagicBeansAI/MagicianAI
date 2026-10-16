//! In-process Magician runner for actionability, routing, and bandit training.
//!
//! Magician fits and installs snapshots in-process. It does not spawn a child
//! binary: the serving process already has the store and the feature contract
//! in memory.

use std::{path::PathBuf, sync::Arc, time::Duration};

/// Delay before the first scheduled training pass after boot. Shorter
/// configured intervals still win, so a test or an operator who asks for a
/// tight cadence gets one.
const TRAINING_BOOT_QUIET_PERIOD: Duration = Duration::from_secs(10 * 60);

use anyhow::Result;
use tokio::sync::Mutex;

use crate::config::AttentionActionabilityTrainingConfig;

use super::{
    train_actionability, train_bandit, ActionabilityTrainingConfig, BanditTrainingConfig,
    TrainingOutcome, MIN_CANARY_POSTERIOR_UPDATES,
};
use crate::config::{AttentionActionabilityMode, AttentionBanditMode};
use crate::magician_v2::attention::learning::{AttentionLearningService, AttentionSurface};

#[derive(Clone)]
pub struct AttentionActionabilityTrainingWorker {
    learning: AttentionLearningService,
    config: AttentionActionabilityTrainingConfig,
    artifact_root: PathBuf,
    /// `<runtime root>/scopes`. Discovery reads scopes out of the learning
    /// database; this is how it tells one that still exists from one whose
    /// rows merely outlived it.
    scopes_root: PathBuf,
    train_lock: Arc<Mutex<()>>,
}

impl AttentionActionabilityTrainingWorker {
    pub fn new(
        learning: AttentionLearningService,
        config: AttentionActionabilityTrainingConfig,
        artifact_root: PathBuf,
        scopes_root: PathBuf,
    ) -> Self {
        Self {
            learning,
            config,
            artifact_root,
            scopes_root,
            train_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Whether a scope still exists, by the same authority every pass that
    /// scans `scopes/` uses: its directory.
    fn scope_exists(&self, principal: &str, workspace: &str) -> bool {
        self.scopes_root.join(principal).join(workspace).is_dir()
    }

    pub fn config(&self) -> &AttentionActionabilityTrainingConfig {
        &self.config
    }

    pub async fn run_scope(&self, principal: &str, workspace: &str) -> Result<TrainingOutcome> {
        self.run_scope_with_install(principal, workspace, self.config.auto_install)
            .await
    }

    pub async fn run_scope_with_install(
        &self,
        principal: &str,
        workspace: &str,
        install: bool,
    ) -> Result<TrainingOutcome> {
        let _guard = self.train_lock.lock().await;
        let out_path = self
            .artifact_root
            .join(format!("actionability-{principal}-{workspace}.json"));
        let outcome = train_actionability(
            &self.learning.store(),
            principal,
            workspace,
            &ActionabilityTrainingConfig::default(),
            &out_path,
        )
        .await?;
        if let TrainingOutcome::Written { snapshot_id, .. } = &outcome {
            if install {
                let snapshot = serde_json::from_str(&std::fs::read_to_string(&out_path)?)?;
                let mode = self
                    .learning
                    .store()
                    .install_actionability_snapshot_for_scope(
                        principal,
                        workspace,
                        &snapshot,
                        AttentionActionabilityMode::Enforced,
                    )
                    .await?;
                tracing::info!(
                    principal,
                    workspace,
                    snapshot_id,
                    effective_mode = mode.as_str(),
                    "installed trained actionability snapshot"
                );
            }
        }
        Ok(outcome)
    }

    pub async fn run_routing_scope(
        &self,
        principal: &str,
        workspace: &str,
        install: bool,
    ) -> Result<TrainingOutcome> {
        let _guard = self.train_lock.lock().await;
        let out_path = self
            .artifact_root
            .join(format!("routing-{principal}-{workspace}.json"));
        let outcome = super::train_routing(
            &self.learning.store(),
            principal,
            workspace,
            &super::RoutingTrainingConfig::default(),
            &out_path,
        )
        .await?;
        if let TrainingOutcome::Written { snapshot_id, .. } = &outcome {
            if install {
                let snapshot = serde_json::from_str(&std::fs::read_to_string(&out_path)?)?;
                let mode = self
                    .learning
                    .store()
                    .install_routing_policy_snapshot_for_scope(
                        principal,
                        workspace,
                        &snapshot,
                        crate::config::AttentionRoutingMode::Shadow,
                    )
                    .await?;
                tracing::info!(
                    principal,
                    workspace,
                    snapshot_id,
                    effective_mode = mode.as_str(),
                    "installed trained routing snapshot in shadow"
                );
            }
        }
        Ok(outcome)
    }

    pub async fn run_bandit_scope(
        &self,
        principal: &str,
        workspace: &str,
        install: bool,
    ) -> Result<TrainingOutcome> {
        let _guard = self.train_lock.lock().await;
        let out_path = self
            .artifact_root
            .join(format!("bandit-{principal}-{workspace}.json"));
        let outcome = train_bandit(
            &self.learning.store(),
            principal,
            workspace,
            &BanditTrainingConfig::default(),
            &out_path,
        )
        .await?;
        if let TrainingOutcome::Written {
            snapshot_id, path, ..
        } = &outcome
        {
            if install {
                let snapshot = if path.exists() {
                    serde_json::from_str(&std::fs::read_to_string(path)?)?
                } else {
                    self.learning
                        .store()
                        .get_bandit_policy_snapshot(snapshot_id)
                        .await?
                        .ok_or_else(|| {
                            anyhow::anyhow!("trained bandit snapshot {snapshot_id} is missing")
                        })?
                };
                let follow_up = self
                    .learning
                    .store()
                    .get_bandit_posterior(
                        principal,
                        workspace,
                        AttentionSurface::FollowUp,
                        &snapshot,
                    )
                    .await?;
                let worth = self
                    .learning
                    .store()
                    .get_bandit_posterior(
                        principal,
                        workspace,
                        AttentionSurface::WorthALook,
                        &snapshot,
                    )
                    .await?;
                let updates = follow_up.update_count.saturating_add(worth.update_count);
                let current = self
                    .learning
                    .store()
                    .bandit_scope_install(principal, workspace)
                    .await?;
                let requested = if current
                    .as_ref()
                    .is_some_and(|install| install.effective_mode == AttentionBanditMode::Canary)
                    || updates >= MIN_CANARY_POSTERIOR_UPDATES
                {
                    AttentionBanditMode::Canary
                } else {
                    AttentionBanditMode::Shadow
                };
                let mode = self
                    .learning
                    .store()
                    .install_bandit_policy_snapshot_for_scope(
                        principal, workspace, &snapshot, requested,
                    )
                    .await?;
                tracing::info!(
                    principal,
                    workspace,
                    snapshot_id,
                    updates,
                    requested = requested.as_str(),
                    effective_mode = mode.as_str(),
                    "installed trained bandit snapshot"
                );
            }
        }
        Ok(outcome)
    }

    pub async fn run_discovered_scopes(&self) -> Result<Vec<(String, String, TrainingOutcome)>> {
        let mut scopes = self.learning.store().list_scopes().await?;
        // Actionability training ranks what a person should see. A reserved
        // sink has no person, and its rows exist only because earlier passes
        // wrote them.
        scopes.retain(|(principal, workspace)| {
            crate::magician_v2::artifact_v2::workspace::scope_hosts_user_subsystems(
                principal, workspace,
            )
        });
        // The list comes from the database, where a row records what a scope
        // once did rather than proving it still exists. Training a scope whose
        // directory is gone writes its state back and materializes the
        // directory again, so a deleted workspace returns — on this worker's
        // periodic tick rather than at boot, which is what made it look fixed
        // when only the boot-time discovery had been filtered. Retiring a
        // scope for real means clearing its rows and its wake-queue entries
        // too; see docs/runbooks/2026-09-15-retiring-a-scope.md.
        scopes.retain(|(principal, workspace)| self.scope_exists(principal, workspace));
        if !scopes
            .iter()
            .any(|(principal, workspace)| principal == "anonymous" && workspace == "default")
        {
            scopes.insert(0, ("anonymous".to_string(), "default".to_string()));
        }
        let mut results = Vec::new();
        for (principal, workspace) in scopes {
            if self.config.enabled {
                match self.run_scope(&principal, &workspace).await {
                    Ok(outcome) => {
                        match &outcome {
                            TrainingOutcome::Written {
                                snapshot_id,
                                metrics,
                                ..
                            } => {
                                tracing::info!(
                                    principal,
                                    workspace,
                                    snapshot_id,
                                    labels = metrics.label_count,
                                    auc = metrics.auc,
                                    ece = metrics.ece,
                                    "actionability training wrote a snapshot"
                                );
                            },
                            TrainingOutcome::Refused {
                                reason,
                                metrics,
                                counts,
                            } => {
                                tracing::info!(
                                    principal,
                                    workspace,
                                    reason,
                                    usable = counts.usable,
                                    unlinked = counts.unlinked,
                                    labels = metrics.label_count,
                                    "actionability training refused; watching the count"
                                );
                            },
                        }
                        results.push((principal.clone(), workspace.clone(), outcome));
                    },
                    Err(error) => {
                        tracing::warn!(
                            principal,
                            workspace,
                            error = %error,
                            "actionability training scope pass failed; retrying next tick"
                        );
                    },
                }
            }
            if self.learning.routing_training_enabled() {
                match self
                    .run_routing_scope(
                        &principal,
                        &workspace,
                        self.learning.routing_training_auto_install(),
                    )
                    .await
                {
                    Ok(outcome) => match &outcome {
                        TrainingOutcome::Written {
                            snapshot_id,
                            metrics,
                            ..
                        } => {
                            tracing::info!(
                                principal,
                                workspace,
                                snapshot_id,
                                labels = metrics.label_count,
                                "routing training wrote a snapshot"
                            );
                        },
                        TrainingOutcome::Refused {
                            reason, metrics, ..
                        } => {
                            tracing::info!(
                                principal,
                                workspace,
                                reason,
                                labels = metrics.label_count,
                                "routing training refused; watching lane corrections"
                            );
                        },
                    },
                    Err(error) => {
                        tracing::warn!(
                            principal,
                            workspace,
                            error = %error,
                            "routing training scope pass failed; retrying next tick"
                        );
                    },
                }
            }
            if self.learning.bandit_training_enabled() {
                match self
                    .run_bandit_scope(
                        &principal,
                        &workspace,
                        self.learning.bandit_training_auto_install(),
                    )
                    .await
                {
                    Ok(outcome) => match &outcome {
                        TrainingOutcome::Written { snapshot_id, .. } => {
                            tracing::info!(
                                principal,
                                workspace,
                                snapshot_id,
                                "bandit training wrote a snapshot"
                            );
                        },
                        TrainingOutcome::Refused { reason, .. } => {
                            tracing::info!(
                                principal,
                                workspace,
                                reason,
                                "bandit training refused; waiting for upstream snapshots"
                            );
                        },
                    },
                    Err(error) => {
                        tracing::warn!(
                            principal,
                            workspace,
                            error = %error,
                            "bandit training scope pass failed; retrying next tick"
                        );
                    },
                }
            }
        }
        Ok(results)
    }

    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if !crate::magician_v2::runtime::startup::wait_for_http().await {
                return;
            }
            let interval = Duration::from_secs(self.config.interval_secs.max(60));
            // Wait out boot before the first pass. Running immediately put the
            // full-scope training scan inside the startup I/O storm (thinking
            // map sweep, synthesis reconcile, per-scope attention bootstrap,
            // memory-index embedding), where it held one of the store's few
            // read connections for 49 s on a cold 40 GB database to learn that
            // the label count was below the minimum. The same scan takes well
            // under a second once boot is quiet.
            tokio::time::sleep(interval.min(TRAINING_BOOT_QUIET_PERIOD)).await;
            loop {
                if let Err(error) = self.run_discovered_scopes().await {
                    tracing::warn!(
                        error = %error,
                        "actionability training pass failed; retrying"
                    );
                }
                tokio::time::sleep(interval).await;
            }
        })
    }
}
