//! Publish a new typed tool skill for later app (or agent) use.
//!
//! This is ordinary skill publication, not a second wrapper format. The
//! document is the same `SKILL.md` used by skillshub: `skill_type: tool`,
//! a Universal Skill Runtime contract and typed actions.
//! Magician stores the exact bytes
//! write-once and internally names the lock identity `capability:{name}`.
//! Publication is inert: it never installs a global tool, approves an app, or
//! opens a Universal Skill Runtime dispatch route. Existing reviewed catalog
//! skills do not need this path; the app engine snapshots them into the lock.

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
    skill_dependencies::{AppSkillDependencyError, AppStandaloneCapabilityCandidate},
};

pub const APP_CAPABILITY_SKILL_MEDIA_TYPE: &str =
    "application/vnd.magician.capability-skill+markdown";
pub const APP_CAPABILITY_SKILL_MAX_BYTES: usize =
    tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppCapabilityPublicationOutcome {
    Created,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppCapabilityPublicationReceipt {
    pub state: &'static str,
    pub dependency_ref: AppReference,
    pub semantic_version: String,
    pub immutable_revision_ref: AppReference,
    pub revision: AppRevision,
    pub content_digest: AppDigest,
    pub publication_outcome: AppCapabilityPublicationOutcome,
    pub global_skill_catalog_published: bool,
    pub activation_authority_granted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppStandaloneCapabilityInspection {
    pub state: &'static str,
    pub dependency_ref: AppReference,
    pub semantic_version: String,
    pub content_digest: AppDigest,
    pub publication_required: bool,
    pub activation_authority_granted: bool,
}

#[derive(Debug, Error)]
pub enum AppCapabilityPublicationError {
    #[error(transparent)]
    Capability(#[from] AppSkillDependencyError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
}

#[derive(Clone)]
pub struct AppCapabilityPublicationService {
    registry: AppRegistryService,
}

impl AppCapabilityPublicationService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    pub async fn publish(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        skill_document_bytes: &[u8],
        now: DateTime<Utc>,
    ) -> Result<AppCapabilityPublicationReceipt, AppCapabilityPublicationError> {
        let candidate = AppStandaloneCapabilityCandidate::admit_untrusted(skill_document_bytes)?;
        let receipt = self
            .registry
            .publish_standalone_capability_candidate(authenticated_scope, &candidate, now)
            .await?;
        Ok(publication_receipt(receipt))
    }

    pub async fn resolve(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        immutable_revision_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<(AppCapabilityPublicationReceipt, Vec<u8>), AppCapabilityPublicationError> {
        let (receipt, bytes) = self
            .registry
            .resolve_standalone_skill_document(authenticated_scope, immutable_revision_ref, now)
            .await?;
        let candidate = AppStandaloneCapabilityCandidate::admit_untrusted(&bytes)?;
        if candidate.dependency_ref() != &receipt.dependency_ref
            || candidate.content_digest() != &receipt.content_digest
        {
            return Err(AppSkillDependencyError::MissingCapabilityRevision(
                immutable_revision_ref.to_string(),
            )
            .into());
        }
        Ok((publication_receipt(receipt), bytes))
    }
}

pub fn inspect_standalone_capability(
    skill_document_bytes: &[u8],
) -> Result<AppStandaloneCapabilityInspection, AppSkillDependencyError> {
    let candidate = AppStandaloneCapabilityCandidate::admit_untrusted(skill_document_bytes)?;
    Ok(AppStandaloneCapabilityInspection {
        state: "valid_standalone_capability",
        dependency_ref: candidate.dependency_ref().clone(),
        semantic_version: candidate.semantic_version().to_owned(),
        content_digest: candidate.content_digest().clone(),
        publication_required: true,
        activation_authority_granted: false,
    })
}

fn publication_receipt(
    receipt: AppSkillRevisionPublicationReceipt,
) -> AppCapabilityPublicationReceipt {
    AppCapabilityPublicationReceipt {
        state: "immutable_revision_published",
        dependency_ref: receipt.dependency_ref,
        semantic_version: receipt.semantic_version,
        immutable_revision_ref: receipt.immutable_revision_ref,
        revision: receipt.revision,
        content_digest: receipt.content_digest,
        publication_outcome: match receipt.outcome {
            AppRegistryPublicationOutcome::Created => AppCapabilityPublicationOutcome::Created,
            AppRegistryPublicationOutcome::AlreadyPresent => {
                AppCapabilityPublicationOutcome::AlreadyPresent
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

    fn capability(version: &str, instructions: &str) -> Vec<u8> {
        format!(
            "---\nname: next-step\nversion: {version}\ndescription: Rank the next learning step.\nallowed-tools: capability:content_search\nmetadata:\n  magician:\n    skill_type: tool\n    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n      requires: {{bins: [next-step]}}\n      runtime:\n        protocol: cli\n        command_prefix: []\n    runtime_actions:\n      schema_version: tool-runtime.typed-action-overrides.v1\n      actions:\n        rank:\n          description: Rank the next step.\n          fixed_args: [rank]\n---\n{instructions}\n"
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn capability_publication_is_inert_and_resolved_by_immutable_ref() {
        let temporary = canonical_tempdir();
        let service = AppCapabilityPublicationService::new(AppRegistryService::new(
            ArtifactV2Workspace::new(temporary.path()),
        ));
        let authenticated = authenticated_scope("anonymous", "default");
        let bytes = capability("1.0.0", "Return one ranked next step.");

        let created = service
            .publish(&authenticated, &bytes, time(1))
            .await
            .expect("valid capability publishes");
        assert_eq!(
            created.publication_outcome,
            AppCapabilityPublicationOutcome::Created
        );
        assert_eq!(created.dependency_ref.as_str(), "capability:next-step");
        assert_eq!(created.revision.get(), 1);
        assert!(!created.global_skill_catalog_published);
        assert!(!created.activation_authority_granted);

        let replay = service
            .publish(&authenticated, &bytes, time(2))
            .await
            .expect("exact publication replays");
        assert_eq!(
            replay.publication_outcome,
            AppCapabilityPublicationOutcome::AlreadyPresent
        );
        assert_eq!(
            replay.immutable_revision_ref,
            created.immutable_revision_ref
        );

        let (resolved, resolved_bytes) = service
            .resolve(&authenticated, &created.immutable_revision_ref, time(3))
            .await
            .expect("resolver loads exact bytes");
        assert_eq!(resolved.dependency_ref, created.dependency_ref);
        assert_eq!(resolved_bytes, bytes);
    }

    #[tokio::test]
    async fn procedure_bytes_cannot_publish_as_a_capability() {
        let temporary = canonical_tempdir();
        let service = AppCapabilityPublicationService::new(AppRegistryService::new(
            ArtifactV2Workspace::new(temporary.path()),
        ));
        let authenticated = authenticated_scope("anonymous", "default");
        let bytes = b"---\nname: next-step\nversion: 1.0.0\ndescription: A playbook.\nmetadata:\n  magician:\n    skill_type: procedure\n---\nDo not execute this.\n";
        let error = service
            .publish(&authenticated, bytes, time(1))
            .await
            .expect_err("procedure documents stay off the capability writer");
        assert!(matches!(
            error,
            AppCapabilityPublicationError::Capability(
                AppSkillDependencyError::InvalidCapabilityDocument { .. }
            )
        ));
    }

    #[tokio::test]
    async fn resolver_never_falls_back_to_a_mutable_name() {
        let temporary = canonical_tempdir();
        let service = AppCapabilityPublicationService::new(AppRegistryService::new(
            ArtifactV2Workspace::new(temporary.path()),
        ));
        let authenticated = authenticated_scope("anonymous", "default");
        let error = service
            .resolve(
                &authenticated,
                &AppReference::parse("capability:next-step").unwrap(),
                time(1),
            )
            .await
            .expect_err("a mutable capability name is not a revision");
        assert!(matches!(
            error,
            AppCapabilityPublicationError::Registry(AppRegistryError::MissingRecord { .. })
        ));
    }

    #[tokio::test]
    async fn tool_documents_without_a_usr_contract_cannot_publish() {
        let temporary = canonical_tempdir();
        let service = AppCapabilityPublicationService::new(AppRegistryService::new(
            ArtifactV2Workspace::new(temporary.path()),
        ));
        let authenticated = authenticated_scope("anonymous", "default");
        let bytes = b"---\nname: next-step\nversion: 1.0.0\ndescription: Rank the next learning step.\nmetadata:\n  magician:\n    skill_type: tool\n---\nReturn one ranked next step.\n";
        let error = service
            .publish(&authenticated, bytes, time(1))
            .await
            .expect_err("non-executable tool documents stay unpublished");
        assert!(matches!(
            error,
            AppCapabilityPublicationError::Capability(
                AppSkillDependencyError::InvalidCapabilityDocument { .. }
            )
        ));
    }
}
