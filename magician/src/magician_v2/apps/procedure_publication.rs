//! Reviewed standalone procedure-skill publication for external authors.
//!
//! The transport supplies only bounded `SKILL.md` bytes. Magician parses the
//! identity, allocates the scoped revision and stores the exact document in the
//! immutable app registry. Publication creates dependency evidence only: it
//! never installs the procedure globally, approves an app or grants execution.

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    models::{AppDigest, AppReference, AppRevision},
    registry::{
        AppRegistryError, AppRegistryPublicationOutcome, AppRegistryService,
        AppSkillRevisionPublicationReceipt,
    },
    skill_dependencies::{AppSkillDependencyError, AppStandaloneProcedureCandidate},
};

pub const APP_PROCEDURE_SKILL_MEDIA_TYPE: &str =
    "application/vnd.magician.procedure-skill+markdown";
pub const APP_PROCEDURE_SKILL_MAX_BYTES: usize =
    tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppProcedurePublicationOutcome {
    Created,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppProcedurePublicationReceipt {
    pub state: &'static str,
    pub dependency_ref: AppReference,
    pub semantic_version: String,
    pub immutable_revision_ref: AppReference,
    pub revision: AppRevision,
    pub content_digest: AppDigest,
    pub publication_outcome: AppProcedurePublicationOutcome,
    pub global_skill_catalog_published: bool,
    pub activation_authority_granted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppStandaloneProcedureInspection {
    pub state: &'static str,
    pub dependency_ref: AppReference,
    pub semantic_version: String,
    pub content_digest: AppDigest,
    pub publication_required: bool,
    pub activation_authority_granted: bool,
}

#[derive(Debug, Error)]
pub enum AppProcedurePublicationError {
    #[error(transparent)]
    Procedure(#[from] AppSkillDependencyError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
}

#[derive(Clone)]
pub struct AppProcedurePublicationService {
    registry: AppRegistryService,
}

impl AppProcedurePublicationService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    pub async fn publish(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        skill_document_bytes: &[u8],
        now: DateTime<Utc>,
    ) -> Result<AppProcedurePublicationReceipt, AppProcedurePublicationError> {
        let candidate = AppStandaloneProcedureCandidate::admit_untrusted(skill_document_bytes)?;
        let receipt = self
            .registry
            .publish_standalone_procedure_candidate(authenticated_scope, &candidate, now)
            .await?;
        Ok(publication_receipt(receipt))
    }
}

pub fn inspect_standalone_procedure(
    skill_document_bytes: &[u8],
) -> Result<AppStandaloneProcedureInspection, AppSkillDependencyError> {
    let candidate = AppStandaloneProcedureCandidate::admit_untrusted(skill_document_bytes)?;
    Ok(AppStandaloneProcedureInspection {
        state: "valid_standalone_procedure",
        dependency_ref: candidate.dependency_ref().clone(),
        semantic_version: candidate.semantic_version().to_owned(),
        content_digest: candidate.content_digest().clone(),
        publication_required: true,
        activation_authority_granted: false,
    })
}

fn publication_receipt(
    receipt: AppSkillRevisionPublicationReceipt,
) -> AppProcedurePublicationReceipt {
    AppProcedurePublicationReceipt {
        state: "immutable_revision_published",
        dependency_ref: receipt.dependency_ref,
        semantic_version: receipt.semantic_version,
        immutable_revision_ref: receipt.immutable_revision_ref,
        revision: receipt.revision,
        content_digest: receipt.content_digest,
        publication_outcome: match receipt.outcome {
            AppRegistryPublicationOutcome::Created => AppProcedurePublicationOutcome::Created,
            AppRegistryPublicationOutcome::AlreadyPresent => {
                AppProcedurePublicationOutcome::AlreadyPresent
            },
        },
        global_skill_catalog_published: false,
        activation_authority_granted: false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        apps::registry::tests::{authenticated_scope, canonical_tempdir, time},
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    fn procedure(version: &str, instructions: &str) -> Vec<u8> {
        format!(
            "---\nname: external-summary\nversion: {version}\ndescription: Summarize reviewed \
             input.\nallowed-tools: capability:content_search\nmetadata:\n  magician:\n    \
             skill_type: procedure\n---\n{instructions}\n"
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn external_publication_allocates_revision_and_exact_replay_is_inert() {
        let temporary = canonical_tempdir();
        let service = AppProcedurePublicationService::new(AppRegistryService::new(
            ArtifactV2Workspace::new(temporary.path()),
        ));
        let authenticated = authenticated_scope("anonymous", "default");
        let bytes = procedure("1.0.0", "Produce one concise summary.");

        let created = service
            .publish(&authenticated, &bytes, time(1))
            .await
            .expect("valid procedure publishes");
        assert_eq!(
            created.publication_outcome,
            AppProcedurePublicationOutcome::Created
        );
        assert_eq!(created.dependency_ref.as_str(), "skill:external-summary");
        assert_eq!(created.revision.get(), 1);
        assert!(!created.global_skill_catalog_published);
        assert!(!created.activation_authority_granted);

        let replay = service
            .publish(&authenticated, &bytes, time(2))
            .await
            .expect("exact publication replays");
        assert_eq!(
            replay.publication_outcome,
            AppProcedurePublicationOutcome::AlreadyPresent
        );
        assert_eq!(
            replay.immutable_revision_ref,
            created.immutable_revision_ref
        );
        assert_eq!(replay.revision, created.revision);
    }

    #[tokio::test]
    async fn semantic_versions_receive_monotonic_scoped_revisions() {
        let temporary = canonical_tempdir();
        let service = AppProcedurePublicationService::new(AppRegistryService::new(
            ArtifactV2Workspace::new(temporary.path()),
        ));
        let authenticated = authenticated_scope("anonymous", "default");

        let first = service
            .publish(
                &authenticated,
                &procedure("1.0.0", "Produce one concise summary."),
                time(1),
            )
            .await
            .unwrap();
        let second = service
            .publish(
                &authenticated,
                &procedure("1.1.0", "Produce one concise reviewed summary."),
                time(2),
            )
            .await
            .unwrap();
        assert_eq!(first.revision.get(), 1);
        assert_eq!(second.revision.get(), 2);
        assert_ne!(first.immutable_revision_ref, second.immutable_revision_ref);
    }

    #[tokio::test]
    async fn same_version_rewrite_and_non_procedure_kind_fail_closed() {
        let temporary = canonical_tempdir();
        let service = AppProcedurePublicationService::new(AppRegistryService::new(
            ArtifactV2Workspace::new(temporary.path()),
        ));
        let authenticated = authenticated_scope("anonymous", "default");
        service
            .publish(
                &authenticated,
                &procedure("1.0.0", "Produce one concise summary."),
                time(1),
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .publish(
                    &authenticated,
                    &procedure("1.0.0", "Silently replace the published instructions."),
                    time(2),
                )
                .await,
            Err(AppProcedurePublicationError::Registry(
                AppRegistryError::IdentityConflict { .. }
            ))
        ));

        let wrong_kind = b"---\nname: external-summary\nversion: 1.0.0\ndescription: Not a procedure.\nmetadata:\n  magician:\n    skill_type: tool\n---\nRun something.\n";
        assert!(matches!(
            inspect_standalone_procedure(wrong_kind),
            Err(AppSkillDependencyError::InvalidProcedureDocument { .. })
        ));
    }
}
