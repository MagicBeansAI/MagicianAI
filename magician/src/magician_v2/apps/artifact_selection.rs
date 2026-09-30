//! Provider-neutral artifact-selection contract for VibeDev app creation.
//!
//! A model may classify product requirements, but it cannot choose a more
//! powerful artifact by naming one. The server deterministically derives the
//! smallest supported primary artifact and any required companion artifact.

use std::collections::BTreeSet;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::models::AppDigest;
use crate::magician_v2::json_traversal::canonical_json_bytes;

pub const APP_ARTIFACT_SELECTION_VERSION: u8 = 1;

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, ValueEnum, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppArtifactRequirement {
    /// Reusable playbook / procedure skill.
    ReusableInstructions,
    /// Typed app records and store.
    DurableTypedRecords,
    /// Personal app surface.
    InteractivePersonalSurface,
    /// Background or scheduled app work.
    BackgroundLifecycle,
    /// A genuinely new executable tool that does not already exist as a skill
    /// or compiled pack.
    NewExecutableIntegration,
    /// App-private procedure companion.
    AppPrivateProcedure,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppArtifactSelectionInput {
    pub requirements: BTreeSet<AppArtifactRequirement>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppAuthoringArtifactKind {
    ProcedureSkill,
    DeclarativeApp,
    ExecutableCapability,
    AppWithPrivateProcedures,
}

/// Server-derived selection evidence. It is serialization-only so a generated
/// project or request body cannot assert that the classification was accepted.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppArtifactSelectionDecision {
    version: u8,
    primary: AppAuthoringArtifactKind,
    companions: BTreeSet<AppAuthoringArtifactKind>,
    requirements: BTreeSet<AppArtifactRequirement>,
    requirements_digest: AppDigest,
}

impl AppArtifactSelectionDecision {
    pub fn primary(&self) -> AppAuthoringArtifactKind {
        self.primary
    }

    pub fn companions(&self) -> &BTreeSet<AppAuthoringArtifactKind> {
        &self.companions
    }

    pub fn requirements(&self) -> &BTreeSet<AppArtifactRequirement> {
        &self.requirements
    }

    pub fn requirements_digest(&self) -> &AppDigest {
        &self.requirements_digest
    }
}

pub fn select_authoring_artifact(
    input: AppArtifactSelectionInput,
) -> Result<AppArtifactSelectionDecision, AppArtifactSelectionError> {
    if input.requirements.is_empty() {
        return Err(AppArtifactSelectionError::NoProductRequirement);
    }
    let needs_app = input.requirements.iter().any(|requirement| {
        matches!(
            requirement,
            AppArtifactRequirement::DurableTypedRecords
                | AppArtifactRequirement::InteractivePersonalSurface
                | AppArtifactRequirement::BackgroundLifecycle
                | AppArtifactRequirement::AppPrivateProcedure
        )
    });
    let needs_executable = input
        .requirements
        .contains(&AppArtifactRequirement::NewExecutableIntegration);
    let needs_reusable_instructions = input
        .requirements
        .contains(&AppArtifactRequirement::ReusableInstructions);
    let needs_private_procedure = input
        .requirements
        .contains(&AppArtifactRequirement::AppPrivateProcedure);

    let primary = if needs_app && needs_private_procedure {
        AppAuthoringArtifactKind::AppWithPrivateProcedures
    } else if needs_app {
        AppAuthoringArtifactKind::DeclarativeApp
    } else if needs_executable {
        AppAuthoringArtifactKind::ExecutableCapability
    } else if needs_reusable_instructions {
        AppAuthoringArtifactKind::ProcedureSkill
    } else {
        return Err(AppArtifactSelectionError::NoSupportedArtifact);
    };

    let mut companions = BTreeSet::new();
    if needs_app && needs_executable {
        companions.insert(AppAuthoringArtifactKind::ExecutableCapability);
    }
    if !needs_app && needs_executable && needs_reusable_instructions {
        companions.insert(AppAuthoringArtifactKind::ProcedureSkill);
    }
    companions.remove(&primary);

    let requirement_bytes = canonical_json_bytes(
        &serde_json::to_value(&input.requirements)
            .map_err(|error| AppArtifactSelectionError::Encoding(error.to_string()))?,
    )
    .map_err(|error| AppArtifactSelectionError::Encoding(error.to_string()))?;
    Ok(AppArtifactSelectionDecision {
        version: APP_ARTIFACT_SELECTION_VERSION,
        primary,
        companions,
        requirements: input.requirements,
        requirements_digest: AppDigest::blake3(&requirement_bytes),
    })
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppArtifactSelectionError {
    #[error("artifact selection requires at least one classified product requirement")]
    NoProductRequirement,
    #[error("classified requirements do not map to a supported authoring artifact")]
    NoSupportedArtifact,
    #[error("failed to encode artifact-selection identity: {0}")]
    Encoding(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn select(requirements: &[AppArtifactRequirement]) -> AppArtifactSelectionDecision {
        select_authoring_artifact(AppArtifactSelectionInput {
            requirements: requirements.iter().copied().collect(),
        })
        .unwrap()
    }

    #[test]
    fn instruction_only_work_selects_a_procedure_not_an_app() {
        let decision = select(&[AppArtifactRequirement::ReusableInstructions]);
        assert_eq!(decision.primary(), AppAuthoringArtifactKind::ProcedureSkill);
        assert!(decision.companions().is_empty());
    }

    #[test]
    fn durable_product_state_selects_an_app_and_private_work_stays_private() {
        let app = select(&[AppArtifactRequirement::DurableTypedRecords]);
        assert_eq!(app.primary(), AppAuthoringArtifactKind::DeclarativeApp);
        let private = select(&[
            AppArtifactRequirement::DurableTypedRecords,
            AppArtifactRequirement::AppPrivateProcedure,
        ]);
        assert_eq!(
            private.primary(),
            AppAuthoringArtifactKind::AppWithPrivateProcedures
        );
        assert!(!private
            .companions()
            .contains(&AppAuthoringArtifactKind::ProcedureSkill));
    }

    #[test]
    fn executable_need_is_a_companion_when_the_product_is_an_app() {
        let decision = select(&[
            AppArtifactRequirement::InteractivePersonalSurface,
            AppArtifactRequirement::NewExecutableIntegration,
        ]);
        assert_eq!(decision.primary(), AppAuthoringArtifactKind::DeclarativeApp);
        assert_eq!(
            decision.companions(),
            &BTreeSet::from([AppAuthoringArtifactKind::ExecutableCapability])
        );
    }

    #[test]
    fn decision_is_order_independent_and_cannot_arrive_from_transport() {
        let left = select(&[
            AppArtifactRequirement::ReusableInstructions,
            AppArtifactRequirement::NewExecutableIntegration,
        ]);
        let right = select(&[
            AppArtifactRequirement::NewExecutableIntegration,
            AppArtifactRequirement::ReusableInstructions,
        ]);
        assert_eq!(left, right);
        assert_eq!(
            left.primary(),
            AppAuthoringArtifactKind::ExecutableCapability
        );
        static_assertions::assert_not_impl_any!(
            AppArtifactSelectionDecision: serde::de::DeserializeOwned
        );
    }
}
