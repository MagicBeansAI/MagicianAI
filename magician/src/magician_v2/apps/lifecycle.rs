//! Pure lifecycle reducers for app installations and immutable attempts.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppInstallationStatus {
    ReadyForReview,
    Enabled,
    Disabled,
    UpdatePending,
    Quarantined,
    UninstalledRetained,
    Purged,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppStableOperationalStatus {
    Enabled,
    Disabled,
}

impl From<AppStableOperationalStatus> for AppInstallationStatus {
    fn from(status: AppStableOperationalStatus) -> Self {
        match status {
            AppStableOperationalStatus::Enabled => Self::Enabled,
            AppStableOperationalStatus::Disabled => Self::Disabled,
        }
    }
}

/// Reducer state persisted as part of the installation record. The update
/// return state is explicit so a failed immutable update attempt cannot strand
/// or rewrite the previously healthy installation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInstallationLifecycle {
    pub status: AppInstallationStatus,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_return_status: Option<AppStableOperationalStatus>,
}

impl AppInstallationLifecycle {
    pub fn ready_for_review() -> Self {
        Self {
            status: AppInstallationStatus::ReadyForReview,
            generation: 1,
            update_return_status: None,
        }
    }

    pub fn apply(&self, command: AppInstallationCommand) -> Result<Self, AppLifecycleError> {
        self.validate()?;
        use AppInstallationCommand as Command;
        use AppInstallationStatus as Status;

        let (status, update_return_status) = match (self.status, command) {
            (Status::ReadyForReview, Command::EnableReviewed)
            | (Status::UninstalledRetained, Command::CommitReviewedReinstall) => {
                (Status::Enabled, None)
            },
            (Status::Enabled, Command::Disable) => (Status::Disabled, None),
            (Status::Disabled, Command::ReenableReviewed) => (Status::Enabled, None),
            (Status::Enabled, Command::BeginUpdate) => (
                Status::UpdatePending,
                Some(AppStableOperationalStatus::Enabled),
            ),
            (Status::Disabled, Command::BeginUpdate) => (
                Status::UpdatePending,
                Some(AppStableOperationalStatus::Disabled),
            ),
            (Status::UpdatePending, Command::CommitUpdate)
            | (Status::UpdatePending, Command::FailUpdate) => {
                let prior =
                    self.update_return_status
                        .ok_or(AppLifecycleError::CorruptUpdateState {
                            generation: self.generation,
                        })?;
                (prior.into(), None)
            },
            (Status::ReadyForReview, Command::Quarantine)
            | (Status::Enabled, Command::Quarantine)
            | (Status::Disabled, Command::Quarantine)
            | (Status::UpdatePending, Command::Quarantine) => (Status::Quarantined, None),
            (Status::Enabled, Command::UninstallRetain)
            | (Status::Disabled, Command::UninstallRetain)
            | (Status::Quarantined, Command::UninstallRetain) => {
                (Status::UninstalledRetained, None)
            },
            (Status::UninstalledRetained, Command::Purge) => {
                return Err(AppLifecycleError::PurgeEvidenceRequired)
            },
            (from, command) => {
                return Err(AppLifecycleError::InvalidInstallationTransition { from, command })
            },
        };
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(AppLifecycleError::GenerationExhausted)?;
        Ok(Self {
            status,
            generation,
            update_return_status,
        })
    }

    /// Reject an impossible persisted state before any transition is applied.
    /// Deserialization alone is intentionally not treated as lifecycle proof.
    pub fn validate(&self) -> Result<(), AppLifecycleError> {
        if self.generation == 0 {
            return Err(AppLifecycleError::InvalidPersistedState {
                reason: "generation must be greater than zero",
            });
        }
        match (self.status, self.update_return_status) {
            (AppInstallationStatus::UpdatePending, Some(_)) => Ok(()),
            (AppInstallationStatus::UpdatePending, None) => {
                Err(AppLifecycleError::CorruptUpdateState {
                    generation: self.generation,
                })
            },
            (_, None) => Ok(()),
            (_, Some(_)) => Err(AppLifecycleError::InvalidPersistedState {
                reason: "only update_pending may carry an update return state",
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppInstallationCommand {
    EnableReviewed,
    /// Legacy pure-reducer spelling retained for persisted/test compatibility.
    /// Production registry entry points reject it; re-enable authority must use
    /// [`ReenableReviewed`] with the dedicated current-review proof.
    Enable,
    ReenableReviewed,
    Disable,
    BeginUpdate,
    CommitUpdate,
    FailUpdate,
    Quarantine,
    UninstallRetain,
    CommitReviewedReinstall,
    Purge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLifecycleAttemptState {
    Staged,
    Conforming,
    ReadyForReview,
    Failed,
    Committed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLifecycleAttemptCommand {
    BeginConformance,
    MarkReadyForReview,
    Fail,
    Commit,
}

pub fn reduce_attempt(
    state: AppLifecycleAttemptState,
    command: AppLifecycleAttemptCommand,
) -> Result<AppLifecycleAttemptState, AppLifecycleError> {
    use AppLifecycleAttemptCommand as Command;
    use AppLifecycleAttemptState as State;

    match (state, command) {
        (State::Staged, Command::BeginConformance) => Ok(State::Conforming),
        (State::Conforming, Command::MarkReadyForReview) => Ok(State::ReadyForReview),
        (State::Staged | State::Conforming | State::ReadyForReview, Command::Fail) => {
            Ok(State::Failed)
        },
        (State::ReadyForReview, Command::Commit) => Ok(State::Committed),
        (from, command) => Err(AppLifecycleError::InvalidAttemptTransition { from, command }),
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppLifecycleError {
    #[error("invalid app installation transition from {from:?} using {command:?}")]
    InvalidInstallationTransition {
        from: AppInstallationStatus,
        command: AppInstallationCommand,
    },
    #[error("terminal purge transition requires a verified current purge receipt")]
    PurgeEvidenceRequired,
    #[error("invalid app lifecycle-attempt transition from {from:?} using {command:?}")]
    InvalidAttemptTransition {
        from: AppLifecycleAttemptState,
        command: AppLifecycleAttemptCommand,
    },
    #[error("update-pending installation at generation {generation} has no return state")]
    CorruptUpdateState { generation: u64 },
    #[error("app installation generation is exhausted")]
    GenerationExhausted,
    #[error("invalid persisted app lifecycle state: {reason}")]
    InvalidPersistedState { reason: &'static str },
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn failed_update_returns_to_the_exact_prior_operational_state() {
        for prior in [
            AppStableOperationalStatus::Enabled,
            AppStableOperationalStatus::Disabled,
        ] {
            let initial = AppInstallationLifecycle {
                status: prior.into(),
                generation: 7,
                update_return_status: None,
            };
            let pending = initial.apply(AppInstallationCommand::BeginUpdate).unwrap();
            assert_eq!(pending.status, AppInstallationStatus::UpdatePending);
            assert_eq!(pending.update_return_status, Some(prior));

            let restored = pending.apply(AppInstallationCommand::FailUpdate).unwrap();
            assert_eq!(restored.status, prior.into());
            assert_eq!(restored.update_return_status, None);
            assert_eq!(restored.generation, 9);
        }
    }

    #[test]
    fn purge_requires_retention_evidence_even_from_retained_uninstall() {
        let enabled = AppInstallationLifecycle::ready_for_review()
            .apply(AppInstallationCommand::EnableReviewed)
            .unwrap();
        assert!(matches!(
            enabled.apply(AppInstallationCommand::Purge),
            Err(AppLifecycleError::InvalidInstallationTransition { .. })
        ));
        let retained = enabled
            .apply(AppInstallationCommand::UninstallRetain)
            .unwrap();
        assert!(matches!(
            retained.apply(AppInstallationCommand::Purge),
            Err(AppLifecycleError::PurgeEvidenceRequired)
        ));
    }

    #[test]
    fn disabled_reenable_accepts_only_the_reviewed_command() {
        let disabled = AppInstallationLifecycle {
            status: AppInstallationStatus::Disabled,
            generation: 9,
            update_return_status: None,
        };
        assert!(matches!(
            disabled.apply(AppInstallationCommand::Enable),
            Err(AppLifecycleError::InvalidInstallationTransition { .. })
        ));
        let enabled = disabled
            .apply(AppInstallationCommand::ReenableReviewed)
            .unwrap();
        assert_eq!(enabled.status, AppInstallationStatus::Enabled);
        assert_eq!(enabled.generation, 10);
    }

    #[test]
    fn attempt_failure_never_mutates_an_installation_lifecycle() {
        let installation = AppInstallationLifecycle {
            status: AppInstallationStatus::Enabled,
            generation: 42,
            update_return_status: None,
        };
        let attempt = reduce_attempt(
            AppLifecycleAttemptState::Conforming,
            AppLifecycleAttemptCommand::Fail,
        )
        .unwrap();
        assert_eq!(attempt, AppLifecycleAttemptState::Failed);
        assert_eq!(installation.status, AppInstallationStatus::Enabled);
        assert_eq!(installation.generation, 42);
    }

    #[test]
    fn generation_overflow_fails_closed() {
        let lifecycle = AppInstallationLifecycle {
            status: AppInstallationStatus::Enabled,
            generation: u64::MAX,
            update_return_status: None,
        };
        assert_eq!(
            lifecycle.apply(AppInstallationCommand::Disable),
            Err(AppLifecycleError::GenerationExhausted)
        );
    }

    #[test]
    fn malformed_persisted_update_state_fails_before_transition() {
        let missing_return = AppInstallationLifecycle {
            status: AppInstallationStatus::UpdatePending,
            generation: 4,
            update_return_status: None,
        };
        assert!(matches!(
            missing_return.apply(AppInstallationCommand::CommitUpdate),
            Err(AppLifecycleError::CorruptUpdateState { .. })
        ));

        let leaked_return = AppInstallationLifecycle {
            status: AppInstallationStatus::Enabled,
            generation: 4,
            update_return_status: Some(AppStableOperationalStatus::Disabled),
        };
        assert!(matches!(
            leaked_return.apply(AppInstallationCommand::Disable),
            Err(AppLifecycleError::InvalidPersistedState { .. })
        ));
    }

    #[test]
    fn attempt_transition_graph_rejects_skips_and_replays() {
        assert!(reduce_attempt(
            AppLifecycleAttemptState::Staged,
            AppLifecycleAttemptCommand::Commit
        )
        .is_err());
        assert!(reduce_attempt(
            AppLifecycleAttemptState::Committed,
            AppLifecycleAttemptCommand::Commit
        )
        .is_err());
        assert!(reduce_attempt(
            AppLifecycleAttemptState::Failed,
            AppLifecycleAttemptCommand::BeginConformance
        )
        .is_err());
    }
}
