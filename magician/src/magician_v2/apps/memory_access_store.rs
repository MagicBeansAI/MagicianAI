//! Persistence for owner-edited app memory grants (`app_memory_read_v1`).
//!
//! The reviewed baseline lives on the installation's `AppGrantRevision`
//! (`requested_memory_read` / `granted_memory_read`). Owner edits after
//! install live in `app_memory_read_grant_heads`, one row per installation,
//! compare-and-swapped on `revision`. An edit applies only while it pins the
//! installation's current `grant_revision`: an update or reinstall creates a
//! new grant revision and the reviewed baseline takes over again. Editing does
//! not bump the grant revision, so it never fences in-flight runs — a narrowed
//! or revoked grant takes effect on the app's next memory read.
//!
//! Everything here fails closed: a disabled, revoked or grant-less
//! installation, a head row that no longer matches its request, or an
//! undecodable record all mean "no memory".

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use super::authority::AuthenticatedAppScope;
use super::lifecycle::AppInstallationStatus;
use super::memory_access::{
    owner_memory_read_grant, validate_memory_read_grant, AppMemoryReadGrant, AppMemoryReadRequest,
    AppMemoryReadSelection,
};
use super::models::{decode_app_contract, AppContractLimits, AppInstallationId, AppReference};
use super::records::{AppGrantRevision, AppInstallation, AppScope};
use super::registry::{
    open_existing_scoped_registry_read_only, AppRegistryError, AppRegistryService,
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// What the owner and the review screen see for one installation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppMemoryReadAccess {
    pub installation_id: AppInstallationId,
    /// `None` when the app requested no memory.
    pub request: Option<AppMemoryReadRequest>,
    /// The grant made at install review.
    pub reviewed: Option<AppMemoryReadGrant>,
    /// What the app can read right now (reviewed grant or the owner's later
    /// edit). `None` means nothing.
    pub effective: Option<AppMemoryReadGrant>,
    /// Compare-and-swap token for the next edit; 0 before any edit.
    pub edit_revision: u64,
    pub installation_enabled: bool,
}

fn decode<T>(bytes: &[u8]) -> Result<T, AppRegistryError>
where
    T: serde::de::DeserializeOwned + super::models::ValidateAppContract,
{
    decode_app_contract(bytes, &AppContractLimits::default()).map_err(AppRegistryError::from)
}

struct HeadRow {
    grant_revision: u64,
    revision: u64,
    grant: Option<AppMemoryReadGrant>,
}

fn load_head(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<Option<HeadRow>, AppRegistryError> {
    let row: Option<(i64, i64, Vec<u8>)> = connection
        .query_row(
            "SELECT grant_revision, revision, grant_json FROM app_memory_read_grant_heads
             WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(grant_revision, revision, bytes)| HeadRow {
        grant_revision: u64::try_from(grant_revision).unwrap_or(0),
        revision: u64::try_from(revision).unwrap_or(0),
        // An undecodable head row is treated as no edit rather than trusted.
        grant: serde_json::from_slice(&bytes).ok(),
    }))
}

/// Load an installation's memory access in one read snapshot.
pub(crate) fn load_memory_read_access(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<Option<AppMemoryReadAccess>, AppRegistryError> {
    let installation: Option<Vec<u8>> = connection
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(installation) = installation else {
        return Ok(None);
    };
    let installation: AppInstallation = decode(&installation)?;
    let installation_enabled = installation.lifecycle.status == AppInstallationStatus::Enabled;
    let head = load_head(connection, installation_id)?;
    let edit_revision = head.as_ref().map_or(0, |head| head.revision);
    let Some(grant_revision) = installation.grant_revision else {
        return Ok(Some(AppMemoryReadAccess {
            installation_id: installation_id.clone(),
            request: None,
            reviewed: None,
            effective: None,
            edit_revision,
            installation_enabled,
        }));
    };
    let grant: Option<Vec<u8>> = connection
        .query_row(
            "SELECT record_json FROM app_grant_revisions
             WHERE installation_id = ?1 AND revision = ?2",
            params![installation_id.as_str(), grant_revision.get() as i64],
            |row| row.get(0),
        )
        .optional()?;
    let grant: Option<AppGrantRevision> = grant.map(|bytes| decode(&bytes)).transpose()?;
    let revoked = grant
        .as_ref()
        .is_none_or(|grant| grant.revoked_at.is_some());
    let request = grant
        .as_ref()
        .and_then(|grant| grant.requested_memory_read.clone());
    let reviewed = grant
        .as_ref()
        .and_then(|grant| grant.granted_memory_read.clone());
    let effective = match (&request, installation_enabled && !revoked) {
        (Some(request), true) => {
            let edited = head
                .as_ref()
                .filter(|head| head.grant_revision == grant_revision.get())
                .and_then(|head| head.grant.clone());
            edited
                .or_else(|| reviewed.clone())
                .filter(|grant| validate_memory_read_grant(grant, request).is_ok())
        },
        _ => None,
    };
    Ok(Some(AppMemoryReadAccess {
        installation_id: installation_id.clone(),
        request,
        reviewed,
        effective,
        edit_revision,
        installation_enabled,
    }))
}

/// The grant a running app may use, read without authentication from the
/// scope's existing registry (the executor has already proven the app
/// identity it stamps). Absent registry, installation or grant mean `None`.
pub fn effective_memory_read_grant_blocking(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    installation_id: &str,
) -> Result<Option<(AppMemoryReadRequest, AppMemoryReadGrant)>, AppRegistryError> {
    let scope = AppScope {
        principal: AppReference::parse(principal)?,
        workspace: AppReference::parse(workspace_name)?,
    };
    let installation_id = AppInstallationId::parse(installation_id)?;
    let Some(connection) = open_existing_scoped_registry_read_only(workspace, &scope)? else {
        return Ok(None);
    };
    let access = load_memory_read_access(&connection, &installation_id)?;
    Ok(
        access.and_then(|access| match (access.request, access.effective) {
            (Some(request), Some(grant)) => Some((request, grant)),
            _ => None,
        }),
    )
}

impl AppRegistryService {
    /// The owner-facing view of one installation's memory access.
    pub async fn memory_read_access(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<AppMemoryReadAccess>, AppRegistryError> {
        let installation_id = installation_id.clone();
        Ok(self
            .execute_scoped_read(authenticated, &now, move |connection, _| {
                load_memory_read_access(connection, &installation_id)
            })
            .await?
            .flatten())
    }

    /// Replace an installation's memory grant with an owner-chosen one,
    /// compare-and-swapped on `expected_edit_revision`. The selection must be
    /// within the app's request; it applies on the app's next memory read.
    pub async fn update_memory_read_grant(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        expected_edit_revision: u64,
        interactive: AppMemoryReadSelection,
        background: AppMemoryReadSelection,
        updated_by: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppMemoryReadAccess, AppRegistryError> {
        let installation_id = installation_id.clone();
        self.execute_scoped_write(authenticated, &now, move |connection, _| {
            let transaction = connection.transaction()?;
            let access =
                load_memory_read_access(&transaction, &installation_id)?.ok_or_else(|| {
                    AppRegistryError::StateConflict("installation not found".to_owned())
                })?;
            if access.edit_revision != expected_edit_revision {
                return Err(AppRegistryError::StateConflict(
                    "memory access changed since it was loaded; reload and try again".to_owned(),
                ));
            }
            let request = access.request.clone().ok_or_else(|| {
                AppRegistryError::StateConflict("this app requested no memory access".to_owned())
            })?;
            let installation: AppInstallation = decode(&transaction.query_row(
                "SELECT record_json FROM app_installations WHERE installation_id = ?1",
                params![installation_id.as_str()],
                |row| row.get::<_, Vec<u8>>(0),
            )?)?;
            let grant_revision = installation.grant_revision.ok_or_else(|| {
                AppRegistryError::StateConflict("installation has no approved grant".to_owned())
            })?;
            let reviewed_digest = access
                .reviewed
                .as_ref()
                .map(|grant| grant.request_digest.clone())
                .or_else(|| super::memory_access::memory_read_request_digest(&request))
                .ok_or_else(|| {
                    AppRegistryError::StateConflict(
                        "memory request could not be digested".to_owned(),
                    )
                })?;
            let grant =
                owner_memory_read_grant(&request, &reviewed_digest, interactive, background)
                    .map_err(AppRegistryError::StateConflict)?;
            let grant_json = serde_json::to_vec(&grant)
                .map_err(|error| AppRegistryError::StateConflict(error.to_string()))?;
            let next_revision = expected_edit_revision
                .checked_add(1)
                .ok_or_else(|| AppRegistryError::StateConflict("revision overflow".to_owned()))?;
            transaction.execute(
                "INSERT INTO app_memory_read_grant_heads
                     (installation_id, grant_revision, revision, grant_json, updated_by, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(installation_id) DO UPDATE SET
                     grant_revision = excluded.grant_revision,
                     revision = excluded.revision,
                     grant_json = excluded.grant_json,
                     updated_by = excluded.updated_by,
                     updated_at = excluded.updated_at",
                params![
                    installation_id.as_str(),
                    grant_revision.get() as i64,
                    next_revision as i64,
                    grant_json,
                    updated_by.as_str(),
                    now.to_rfc3339(),
                ],
            )?;
            let updated =
                load_memory_read_access(&transaction, &installation_id)?.ok_or_else(|| {
                    AppRegistryError::StateConflict("installation vanished".to_owned())
                })?;
            transaction.commit()?;
            Ok(updated)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::magician_v2::apps::lifecycle::AppInstallationLifecycle;
    use crate::magician_v2::apps::memory_access::{
        default_memory_read_grant, owner_memory_read_grant, AppMemoryReadRequest,
    };
    use crate::magician_v2::apps::models::{AppDataClassification, AppModelProcessing};
    use crate::magician_v2::apps::models::{AppDigest, AppRevision};
    use crate::magician_v2::apps::records::{
        AppBackgroundExecution, AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion,
        AppNetworkPolicy, AppPersonalAgentAccess, AppResourceCeiling,
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 25, 0, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn id() -> AppInstallationId {
        AppInstallationId::parse("install_1").unwrap()
    }

    fn request() -> AppMemoryReadRequest {
        AppMemoryReadRequest {
            user_tiers: vec!["preferences".into(), "identity".into()],
            agents: vec!["scribe".into()],
            purpose: "Personalise".into(),
        }
    }

    fn policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Ordinary,
            model_processing: AppModelProcessing::None,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn resources() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 10,
            max_output_tokens: 10,
            max_cost_microusd: 10,
            max_paid_tool_invocations: 10,
            max_active_seconds: 10,
            max_lifetime_seconds: 10,
            max_browser_network_actions: 10,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 1,
            max_records: 10,
            max_payload_bytes: 10,
            max_attachment_bytes: 10,
            max_monthly_tokens: 20,
            max_monthly_cost_microusd: 10,
        }
    }

    fn grant_revision(revision: u64, revoked: bool) -> AppGrantRevision {
        AppGrantRevision {
            installation_id: id(),
            revision: AppRevision::new(revision).unwrap(),
            package_revision_ref: reference("package:1"),
            requested_tools: Vec::new(),
            granted_tools: Vec::new(),
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
            requested_interactive_capabilities: Vec::new(),
            granted_interactive_capabilities: Vec::new(),
            granted_custom_surface_entry_points: Vec::new(),
            requested_behavior_grants: Vec::new(),
            granted_behavior_grants: Vec::new(),
            requested_event_behavior_grants: Vec::new(),
            granted_event_behavior_grants: Vec::new(),
            requested_notification_grants: Vec::new(),
            granted_notification_grants: Vec::new(),
            requested_memory_read: Some(request()),
            granted_memory_read: default_memory_read_grant(&request()),
            requested_secret_uses: None,
            granted_secret_uses: None,
            granted_any_public_host: false,
            requested_context_reads: Vec::new(),
            granted_context_reads: Vec::new(),
            requested_personal_agent_data_access: Vec::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: policy(),
            granted_data_handling_policy: policy(),
            granted_data_handling_policy_digest: AppDigest::blake3(b"policy"),
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: resources(),
            granted_resource_ceiling: resources(),
            approved_by: reference("actor:owner"),
            approved_at: time(1),
            authority_digest: AppDigest::blake3(b"grant"),
            revoked_at: revoked.then(|| time(2)),
        }
    }

    fn installation(status: AppInstallationStatus, grant_revision: u64) -> AppInstallation {
        AppInstallation {
            scope: AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            installation_id: id(),
            package_revision_ref: reference("package:1"),
            lifecycle: AppInstallationLifecycle {
                status,
                generation: 1,
                update_return_status: None,
            },
            grant_revision: Some(AppRevision::new(grant_revision).unwrap()),
            active_schema_revision: Some(AppRevision::new(3).unwrap()),
            active_surface_revision: Some(AppRevision::new(5).unwrap()),
            created_at: time(0),
            updated_at: time(1),
            disabled_at: (status == AppInstallationStatus::Disabled).then(|| time(1)),
            quarantined_at: None,
            uninstalled_at: None,
            purged_at: None,
        }
    }

    fn database(
        installation: &AppInstallation,
        grants: &[AppGrantRevision],
        head: Option<(u64, u64, &AppMemoryReadGrant)>,
    ) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_installations (installation_id TEXT PRIMARY KEY, record_json BLOB);
                 CREATE TABLE app_grant_revisions (installation_id TEXT, revision INTEGER, record_json BLOB);
                 CREATE TABLE app_memory_read_grant_heads (installation_id TEXT PRIMARY KEY,
                     grant_revision INTEGER, revision INTEGER, grant_json BLOB,
                     updated_by TEXT, updated_at TEXT);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO app_installations VALUES (?1, ?2)",
                params![id().as_str(), serde_json::to_vec(installation).unwrap()],
            )
            .unwrap();
        for grant in grants {
            connection
                .execute(
                    "INSERT INTO app_grant_revisions VALUES (?1, ?2, ?3)",
                    params![
                        id().as_str(),
                        grant.revision.get() as i64,
                        serde_json::to_vec(grant).unwrap()
                    ],
                )
                .unwrap();
        }
        if let Some((grant_revision, revision, grant)) = head {
            connection
                .execute(
                    "INSERT INTO app_memory_read_grant_heads VALUES (?1, ?2, ?3, ?4, 'actor:owner', 'now')",
                    params![
                        id().as_str(),
                        grant_revision as i64,
                        revision as i64,
                        serde_json::to_vec(grant).unwrap()
                    ],
                )
                .unwrap();
        }
        connection
    }

    fn edited(interactive: &[&str], background: &[&str]) -> AppMemoryReadGrant {
        let request = request();
        let digest = super::super::memory_access::memory_read_request_digest(&request).unwrap();
        let selection = |tiers: &[&str]| AppMemoryReadSelection {
            user_tiers: tiers.iter().map(|tier| (*tier).to_owned()).collect(),
            agents: Vec::new(),
        };
        owner_memory_read_grant(
            &request,
            &digest,
            selection(interactive),
            selection(background),
        )
        .unwrap()
    }

    #[test]
    fn the_reviewed_grant_applies_until_the_owner_edits_it() {
        let connection = database(
            &installation(AppInstallationStatus::Enabled, 2),
            &[grant_revision(2, false)],
            None,
        );
        let access = load_memory_read_access(&connection, &id())
            .unwrap()
            .unwrap();
        let effective = access.effective.unwrap();
        assert_eq!(
            effective.interactive.user_tiers,
            ["preferences"],
            "sensitive tier not by default"
        );
        assert!(effective.background.is_empty());
        assert_eq!(access.edit_revision, 0);
    }

    #[test]
    fn an_owner_edit_wins_only_while_it_pins_the_current_grant_revision() {
        let edit = edited(&["identity"], &["preferences"]);
        let current = database(
            &installation(AppInstallationStatus::Enabled, 2),
            &[grant_revision(2, false)],
            Some((2, 1, &edit)),
        );
        let access = load_memory_read_access(&current, &id()).unwrap().unwrap();
        assert_eq!(
            access.effective.as_ref().unwrap().background.user_tiers,
            ["preferences"]
        );
        assert_eq!(access.edit_revision, 1);

        // An update re-reviewed the app (grant revision 3): the old edit no
        // longer applies and the new reviewed baseline does.
        let updated = database(
            &installation(AppInstallationStatus::Enabled, 3),
            &[grant_revision(2, false), grant_revision(3, false)],
            Some((2, 1, &edit)),
        );
        let access = load_memory_read_access(&updated, &id()).unwrap().unwrap();
        assert!(access.effective.unwrap().background.is_empty());
    }

    #[test]
    fn disabled_revoked_or_widened_grants_read_nothing() {
        let disabled = database(
            &installation(AppInstallationStatus::Disabled, 2),
            &[grant_revision(2, false)],
            None,
        );
        assert!(load_memory_read_access(&disabled, &id())
            .unwrap()
            .unwrap()
            .effective
            .is_none());

        let revoked = database(
            &installation(AppInstallationStatus::Enabled, 2),
            &[grant_revision(2, true)],
            None,
        );
        assert!(load_memory_read_access(&revoked, &id())
            .unwrap()
            .unwrap()
            .effective
            .is_none());

        // A tampered head row that grants a tier the app never requested is
        // ignored rather than trusted.
        let mut widened = edited(&["preferences"], &[]);
        widened.interactive.user_tiers.push("contacts".into());
        let tampered = database(
            &installation(AppInstallationStatus::Enabled, 2),
            &[grant_revision(2, false)],
            Some((2, 1, &widened)),
        );
        assert!(load_memory_read_access(&tampered, &id())
            .unwrap()
            .unwrap()
            .effective
            .is_none());
    }
}
