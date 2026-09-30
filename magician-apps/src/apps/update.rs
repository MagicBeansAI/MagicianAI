//! Phase 6 update switch, permission diff and rollback decisions.
//!
//! Updates stay side-by-side until one short pointer CAS. Authority
//! expansion always re-enters review. Code-only rollback is immediate
//! when schemas stay compatible; data rewind is explicit because it
//! may discard writes made after the update.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    authority::{AppAuthorityError, AuthenticatedAppScope},
    entity_mutation::{
        advance_dataset_generation, next_revision, persist_storage_usage, project_storage_usage,
        reserve_change_sequences, write_record_revision, AppEntityMutationError, WorkingRecord,
    },
    entity_portability::{AppDataPortabilityService, AppEntityPortabilityError},
    entity_store::{resolve_active_schema, ActiveAppEntitySchema, AppEntityStoreError},
    lifecycle::AppInstallationStatus,
    migration::{
        apply_migration_to_handling_policy, apply_migration_to_payload, compile_inferred_migration,
        compile_migration_plan, AppCompiledMigrationPlan, AppMigrationError, AppMigrationField,
        AppMigrationOperation, APP_MIGRATION_TAIL_RECORD_CEILING,
    },
    models::{
        decode_app_contract, decode_bounded_json_value, AppContractError, AppContractLimits,
        AppDigest, AppFieldPath, AppInstallationId, AppModelProcessing, AppName, AppRecordId,
        AppReference, AppRevision, ValidateAppContract,
    },
    package_staging::{AppPackageStager, AppPackageStagingError},
    portability::{
        authorize_archive_write, AppArchiveProtectionRequest, AppDataArchiveManifest,
        AppDataImportPreview, AppDataImportPreviewStatus, AppDataImportReceipt,
        AppDataImportRecordDecision, AppExportSourceState, AppLogicalArchive, AppPortabilityError,
    },
    portable_archive_transfer::{
        decode_app_portable_archive, encode_app_portable_archive, AppArchivePassphrase,
        AppPortableArchiveTransferError,
    },
    query_semantics::AppQueryScalarKind,
    records::{
        validate_policy, AppBackgroundExecution, AppBehaviorGrant, AppBehaviorResourceCeiling,
        AppDataHandlingPolicy, AppEventBehaviorGrant, AppGrantRevision, AppInstallation,
        AppLifecycleAttempt, AppLifecycleAttemptKind, AppNetworkPolicy, AppNotificationGrant,
        AppRecordActorKind, AppRecordProvenance, AppResourceCeiling, AppSchemaCompatibility,
        AppSchemaRevision, AppScope,
    },
    registry::{
        encode_bounded_json, format_timestamp, AppRegistryError, AppRegistryService,
        AppReviewableRevisionSourceFence,
    },
    registry_lifecycle::{
        insert_outbox_event, lifecycle_event, update_installation_cas, AppLifecycleEventKind,
    },
    schema_compiler::{
        compile_app_schema, runtime_contracts_from_revision, AppSchemaCompilerError,
    },
};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppPermissionChangeKind {
    #[default]
    Unchanged,
    Narrowed,
    Expanded,
}

fn permission_change_is_unchanged(value: &AppPermissionChangeKind) -> bool {
    *value == AppPermissionChangeKind::Unchanged
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPermissionDiff {
    pub tools: AppPermissionChangeKind,
    #[serde(default)]
    pub agents: AppPermissionChangeKind,
    #[serde(default)]
    pub personalities: AppPermissionChangeKind,
    pub personal_agent_data: AppPermissionChangeKind,
    pub data_handling: AppPermissionChangeKind,
    pub resources: AppPermissionChangeKind,
    /// Which host an app may reach is an authority axis in its own right:
    /// `ApprovedDestinations{[a.com]}` → `{[a.com, evil.com]}` is a genuine
    /// expansion that `data_handling` does not see, because `compare_policy`
    /// only compares the policy's mode.
    #[serde(default)]
    pub network_policy: AppPermissionChangeKind,
    /// Running unattended, more often, or more concurrently is an expansion.
    #[serde(default)]
    pub background_execution: AppPermissionChangeKind,
    #[serde(default)]
    pub context_reads: AppPermissionChangeKind,
    #[serde(default)]
    pub interactive_capabilities: AppPermissionChangeKind,
    /// Which scripted custom-surface entry points the owner granted is an
    /// authority axis in its own right (plan 1.6): a newly granted route, or
    /// the same route re-bound to a swapped entry-document digest, is a
    /// genuine expansion that must re-enter review. Entries compare by
    /// `(route, document, digest)`, so ANY change to the granted set shows
    /// here — the conservative direction. `#[serde(default)]` keeps
    /// persisted pre-1.6 diffs decoding.
    #[serde(default)]
    pub custom_surface_entry_points: AppPermissionChangeKind,
    /// Scheduled behavior grants are an independent unattended-authority
    /// axis. A new behavior, changed reviewed binding, faster cadence, or
    /// larger behavior ceiling is an expansion.
    #[serde(default)]
    pub background_behaviors: AppPermissionChangeKind,
    /// Event subscriptions and their exact host projection/resource bindings.
    #[serde(default, skip_serializing_if = "permission_change_is_unchanged")]
    pub event_behaviors: AppPermissionChangeKind,
    /// Workflow-local owner-notification ports and volume/severity ceilings.
    #[serde(default, skip_serializing_if = "permission_change_is_unchanged")]
    pub owner_notifications: AppPermissionChangeKind,
    /// Owner memory the app may read (`app_memory_read_v1`), per run mode.
    #[serde(default, skip_serializing_if = "permission_change_is_unchanged")]
    pub memory_read: AppPermissionChangeKind,
    /// Secrets the app's tools may use (`app_secret_use_v1`).
    #[serde(default, skip_serializing_if = "permission_change_is_unchanged")]
    pub secret_uses: AppPermissionChangeKind,
    /// The explicit "any public host" network grant
    /// (`app_in_place_skill_v1`). Gaining it is an expansion.
    #[serde(default, skip_serializing_if = "permission_change_is_unchanged")]
    pub any_public_host: AppPermissionChangeKind,
    pub requires_review: bool,
    pub diff_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppUpdateSwitchState {
    Staged,
    DryRunPassed,
    Quiesced,
    Switched,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppUpdateSwitchPlan {
    pub installation_id: AppInstallationId,
    pub source_package_revision_ref: AppReference,
    pub destination_package_revision_ref: AppReference,
    pub source_generation: u64,
    pub destination_generation: u64,
    pub source_tail_change_seq: u64,
    pub destination_schema_revision: AppRevision,
    pub destination_grant_revision: AppRevision,
    pub migration_plan_digest: Option<AppDigest>,
    pub permission_diff_digest: AppDigest,
    pub backup_export_digest: Option<AppDigest>,
    pub state: AppUpdateSwitchState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppRollbackKind {
    CodeOnly,
    DataRewind,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRollbackDecision {
    pub kind: AppRollbackKind,
    pub discards_post_update_writes: bool,
    pub requires_explicit_confirmation: bool,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppUpdateError {
    #[error("app update switch source generation or tail changed")]
    SourceGenerationMoved,
    #[error("app update switch is not quiesced")]
    NotQuiesced,
    #[error("app update requires review for authority expansion")]
    ReviewRequired,
    #[error("destructive migration requires a backup/export receipt")]
    BackupReceiptRequired,
    #[error("data rewind rollback requires explicit confirmation")]
    ExplicitRewindRequired,
    #[error("code-only rollback is unavailable after a data-changing migration")]
    CodeOnlyUnavailable,
}

pub fn compute_permission_diff(
    current: &AppGrantRevision,
    proposed: &AppGrantRevision,
) -> AppPermissionDiff {
    // Destructured with NO `..` rest pattern, on purpose. This function decides
    // whether an app update may skip owner review, and it silently missed three
    // whole authority axes — network destinations, background execution and
    // context reads — because the axis list was restated here by hand while the
    // real list lives on `AppGrantRevision`. Binding every field means adding an
    // axis to the grant FAILS TO COMPILE here until someone decides whether
    // expanding it needs re-review.
    //
    // `requested_*` and the identity/bookkeeping fields are bound to `_` because
    // a diff compares what was GRANTED; what the app asked for is not authority.
    let AppGrantRevision {
        installation_id: _,
        revision: _,
        package_revision_ref: _,
        requested_tools: _,
        granted_tools: current_tools,
        requested_agents: _,
        granted_agents: current_agents,
        requested_personalities: _,
        granted_personalities: current_personalities,
        requested_context_reads: _,
        granted_context_reads: current_context_reads,
        requested_interactive_capabilities: _,
        granted_interactive_capabilities: current_interactive_capabilities,
        granted_custom_surface_entry_points: current_custom_surfaces,
        requested_behavior_grants: _,
        granted_behavior_grants: current_behaviors,
        requested_event_behavior_grants: _,
        granted_event_behavior_grants: current_event_behaviors,
        requested_notification_grants: _,
        granted_notification_grants: current_notifications,
        requested_memory_read: _,
        granted_memory_read: current_memory_read,
        requested_secret_uses: _,
        granted_secret_uses: current_secret_uses,
        granted_any_public_host: current_any_public_host,
        requested_personal_agent_data_access: _,
        granted_personal_agent_data_access: _,
        requested_data_handling_policy: _,
        granted_data_handling_policy: current_data_handling,
        granted_data_handling_policy_digest: _,
        requested_background_execution: _,
        granted_background_execution: current_background,
        requested_network_policy: _,
        granted_network_policy: current_network,
        requested_resource_ceiling: _,
        granted_resource_ceiling: current_resources,
        approved_by: _,
        approved_at: _,
        authority_digest: _,
        revoked_at: _,
    } = current;
    let AppGrantRevision {
        installation_id: _,
        revision: _,
        package_revision_ref: _,
        requested_tools: _,
        granted_tools: proposed_tools,
        requested_agents: _,
        granted_agents: proposed_agents,
        requested_personalities: _,
        granted_personalities: proposed_personalities,
        requested_context_reads: _,
        granted_context_reads: proposed_context_reads,
        requested_interactive_capabilities: _,
        granted_interactive_capabilities: proposed_interactive_capabilities,
        granted_custom_surface_entry_points: proposed_custom_surfaces,
        requested_behavior_grants: _,
        granted_behavior_grants: proposed_behaviors,
        requested_event_behavior_grants: _,
        granted_event_behavior_grants: proposed_event_behaviors,
        requested_notification_grants: _,
        granted_notification_grants: proposed_notifications,
        requested_memory_read: _,
        granted_memory_read: proposed_memory_read,
        requested_secret_uses: _,
        granted_secret_uses: proposed_secret_uses,
        granted_any_public_host: proposed_any_public_host,
        requested_personal_agent_data_access: _,
        granted_personal_agent_data_access: _,
        requested_data_handling_policy: _,
        granted_data_handling_policy: proposed_data_handling,
        granted_data_handling_policy_digest: _,
        requested_background_execution: _,
        granted_background_execution: proposed_background,
        requested_network_policy: _,
        granted_network_policy: proposed_network,
        requested_resource_ceiling: _,
        granted_resource_ceiling: proposed_resources,
        approved_by: _,
        approved_at: _,
        authority_digest: _,
        revoked_at: _,
    } = proposed;

    let tools = compare_sets(current_tools, proposed_tools);
    let agents = compare_sets(current_agents, proposed_agents);
    let personalities = compare_sets(current_personalities, proposed_personalities);
    let context_reads = compare_sets(current_context_reads, proposed_context_reads);
    let interactive_capabilities = compare_sets(
        current_interactive_capabilities,
        proposed_interactive_capabilities,
    );
    // Full-entry equality (route, document, digest): a swapped entry
    // document is a removal plus an addition, which `compare_sets` reports
    // as an expansion — exactly the conservative posture a hosted-code
    // axis needs.
    let custom_surface_entry_points =
        compare_sets(current_custom_surfaces, proposed_custom_surfaces);
    let background_behaviors = compare_behavior_grants(current_behaviors, proposed_behaviors);
    let event_behaviors =
        compare_event_behavior_grants(current_event_behaviors, proposed_event_behaviors);
    let owner_notifications =
        compare_notification_grants(current_notifications, proposed_notifications);
    let memory_read =
        compare_memory_read_grants(current_memory_read.as_ref(), proposed_memory_read.as_ref());
    let secret_uses =
        compare_secret_use_grants(current_secret_uses.as_deref(), proposed_secret_uses.as_deref());
    let any_public_host = match (current_any_public_host, proposed_any_public_host) {
        (false, true) => AppPermissionChangeKind::Expanded,
        (true, false) => AppPermissionChangeKind::Narrowed,
        _ => AppPermissionChangeKind::Unchanged,
    };
    let personal_agent_data = compare_personal_agent(current, proposed);
    let data_handling = compare_policy(current_data_handling, proposed_data_handling);
    let resources = compare_resources(current_resources, proposed_resources);
    let network_policy = compare_network_policy(current_network, proposed_network);
    let background_execution =
        compare_background_execution(current_background, proposed_background);

    let requires_review = matches!(tools, AppPermissionChangeKind::Expanded)
        || matches!(agents, AppPermissionChangeKind::Expanded)
        || matches!(personalities, AppPermissionChangeKind::Expanded)
        || matches!(context_reads, AppPermissionChangeKind::Expanded)
        || matches!(interactive_capabilities, AppPermissionChangeKind::Expanded)
        || matches!(
            custom_surface_entry_points,
            AppPermissionChangeKind::Expanded
        )
        || matches!(personal_agent_data, AppPermissionChangeKind::Expanded)
        || matches!(data_handling, AppPermissionChangeKind::Expanded)
        || matches!(resources, AppPermissionChangeKind::Expanded)
        || matches!(network_policy, AppPermissionChangeKind::Expanded)
        || matches!(background_execution, AppPermissionChangeKind::Expanded);
    let requires_review = requires_review
        || matches!(background_behaviors, AppPermissionChangeKind::Expanded)
        || matches!(event_behaviors, AppPermissionChangeKind::Expanded)
        || matches!(owner_notifications, AppPermissionChangeKind::Expanded)
        || matches!(memory_read, AppPermissionChangeKind::Expanded)
        || matches!(secret_uses, AppPermissionChangeKind::Expanded)
        || matches!(any_public_host, AppPermissionChangeKind::Expanded);
    let legacy_diff_material = format!(
        "{tools:?}:{agents:?}:{personalities:?}:{context_reads:?}:{interactive_capabilities:?}:{custom_surface_entry_points:?}:{personal_agent_data:?}:\
         {data_handling:?}:{resources:?}:{network_policy:?}:{background_execution:?}:{background_behaviors:?}:\
         {requires_review}"
    );
    let diff_digest = if event_behaviors == AppPermissionChangeKind::Unchanged
        && owner_notifications == AppPermissionChangeKind::Unchanged
    {
        AppDigest::blake3(legacy_diff_material.as_bytes())
    } else {
        AppDigest::blake3(
            format!("{legacy_diff_material}:{event_behaviors:?}:{owner_notifications:?}")
                .as_bytes(),
        )
    };
    // Memory access joins the digest only when it changed, so every diff
    // that predates `app_memory_read_v1` keeps its digest.
    let diff_digest = if memory_read == AppPermissionChangeKind::Unchanged {
        diff_digest
    } else {
        AppDigest::blake3(
            format!("{}:memory_read:{memory_read:?}", diff_digest.as_str()).as_bytes(),
        )
    };
    // Secret use joins the same way, only when it changed.
    let diff_digest = if secret_uses == AppPermissionChangeKind::Unchanged {
        diff_digest
    } else {
        AppDigest::blake3(
            format!("{}:secret_uses:{secret_uses:?}", diff_digest.as_str()).as_bytes(),
        )
    };
    // And the "any public host" grant, only when it changed.
    let diff_digest = if any_public_host == AppPermissionChangeKind::Unchanged {
        diff_digest
    } else {
        AppDigest::blake3(
            format!("{}:any_public_host:{any_public_host:?}", diff_digest.as_str()).as_bytes(),
        )
    };
    AppPermissionDiff {
        tools,
        agents,
        personalities,
        personal_agent_data,
        data_handling,
        resources,
        network_policy,
        background_execution,
        context_reads,
        interactive_capabilities,
        custom_surface_entry_points,
        background_behaviors,
        event_behaviors,
        owner_notifications,
        memory_read,
        secret_uses,
        any_public_host,
        requires_review,
        diff_digest,
    }
}

/// Every granted (tool, secret) pair and where that key may go. A new pair
/// (including a first secret grant) or a wider scope for a kept pair (a
/// picked host added, or "any site" ticked) is an expansion that needs
/// review. A scope-less grant (a tool that declares its hosts) compares as
/// the bare pair, so such grants diff exactly as before scopes existed.
fn compare_secret_use_grants(
    current: Option<&[super::secret_access::AppSecretUseGrant]>,
    proposed: Option<&[super::secret_access::AppSecretUseGrant]>,
) -> AppPermissionChangeKind {
    type Reach<'a> = BTreeMap<(String, &'a str), (bool, BTreeSet<&'a str>)>;
    fn reach(grants: Option<&[super::secret_access::AppSecretUseGrant]>) -> Reach<'_> {
        let mut reach = Reach::new();
        for grant in grants.unwrap_or_default() {
            let entry = reach
                .entry((grant.tool.to_string(), grant.secret_ref.as_str()))
                .or_default();
            entry.0 |= grant.any_site;
            entry.1.extend(grant.hosts.iter().map(String::as_str));
        }
        reach
    }
    let current = reach(current);
    let proposed = reach(proposed);
    let mut narrowed = current.keys().any(|key| !proposed.contains_key(key));
    for (key, (any_site, hosts)) in &proposed {
        let Some((current_any_site, current_hosts)) = current.get(key) else {
            return AppPermissionChangeKind::Expanded;
        };
        // "Any site" covers every host; otherwise the picked set must not grow.
        if !current_any_site && (*any_site || !hosts.is_subset(current_hosts)) {
            return AppPermissionChangeKind::Expanded;
        }
        narrowed |= !any_site && (*current_any_site || !current_hosts.is_subset(hosts));
    }
    if narrowed {
        AppPermissionChangeKind::Narrowed
    } else {
        AppPermissionChangeKind::Unchanged
    }
}

/// Every granted memory item, per run mode. Anything new in either mode
/// (including a first memory grant) is an expansion that needs review.
fn compare_memory_read_grants(
    current: Option<&super::memory_access::AppMemoryReadGrant>,
    proposed: Option<&super::memory_access::AppMemoryReadGrant>,
) -> AppPermissionChangeKind {
    fn items(grant: Option<&super::memory_access::AppMemoryReadGrant>) -> Vec<String> {
        let Some(grant) = grant else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for (mode, selection) in [
            ("interactive", &grant.interactive),
            ("background", &grant.background),
        ] {
            items.extend(
                selection
                    .user_tiers
                    .iter()
                    .map(|tier| format!("{mode}:tier:{tier}")),
            );
            items.extend(
                selection
                    .agents
                    .iter()
                    .map(|agent| format!("{mode}:agent:{agent}")),
            );
        }
        items
    }
    compare_sets(&items(current), &items(proposed))
}

fn compare_behavior_grants(
    current: &[AppBehaviorGrant],
    proposed: &[AppBehaviorGrant],
) -> AppPermissionChangeKind {
    let current = current
        .iter()
        .map(|grant| (&grant.behavior_id, grant))
        .collect::<BTreeMap<_, _>>();
    let proposed = proposed
        .iter()
        .map(|grant| (&grant.behavior_id, grant))
        .collect::<BTreeMap<_, _>>();
    let mut narrowed = false;
    for (id, grant) in &proposed {
        let Some(previous) = current.get(id) else {
            return AppPermissionChangeKind::Expanded;
        };
        if *grant == *previous {
            continue;
        }
        if grant.action != previous.action
            || grant.input_selector_digest != previous.input_selector_digest
            || grant.operations != previous.operations
            || grant.output_schema_digest != previous.output_schema_digest
            || grant.reviewed_request_digest != previous.reviewed_request_digest
            || grant.min_interval_seconds < previous.min_interval_seconds
            || !behavior_resources_narrow(&grant.resources, &previous.resources)
        {
            return AppPermissionChangeKind::Expanded;
        }
        narrowed = true;
    }
    if current.keys().any(|id| !proposed.contains_key(id)) {
        narrowed = true;
    }
    if narrowed {
        AppPermissionChangeKind::Narrowed
    } else {
        AppPermissionChangeKind::Unchanged
    }
}

fn compare_event_behavior_grants(
    current: &[AppEventBehaviorGrant],
    proposed: &[AppEventBehaviorGrant],
) -> AppPermissionChangeKind {
    let current = current
        .iter()
        .map(|grant| (&grant.event_behavior_id, grant))
        .collect::<BTreeMap<_, _>>();
    let proposed = proposed
        .iter()
        .map(|grant| (&grant.event_behavior_id, grant))
        .collect::<BTreeMap<_, _>>();
    let mut narrowed = false;
    for (id, grant) in &proposed {
        let Some(previous) = current.get(id) else {
            return AppPermissionChangeKind::Expanded;
        };
        if *grant == *previous {
            continue;
        }
        if grant.purpose != previous.purpose
            || grant.action != previous.action
            || grant.subscription != previous.subscription
            || grant.subscription_digest != previous.subscription_digest
            || grant.projection_schema_digest != previous.projection_schema_digest
            || grant.operations != previous.operations
            || grant.output_schema_digest != previous.output_schema_digest
            || grant.reviewed_request_digest != previous.reviewed_request_digest
            || grant.min_interval_seconds < previous.min_interval_seconds
            || !behavior_resources_narrow(&grant.resources, &previous.resources)
        {
            return AppPermissionChangeKind::Expanded;
        }
        narrowed = true;
    }
    if current.keys().any(|id| !proposed.contains_key(id)) {
        narrowed = true;
    }
    if narrowed {
        AppPermissionChangeKind::Narrowed
    } else {
        AppPermissionChangeKind::Unchanged
    }
}

fn compare_notification_grants(
    current: &[AppNotificationGrant],
    proposed: &[AppNotificationGrant],
) -> AppPermissionChangeKind {
    let current = current
        .iter()
        .map(|grant| ((grant.workflow_id.clone(), grant.port_id.clone()), grant))
        .collect::<BTreeMap<_, _>>();
    let proposed = proposed
        .iter()
        .map(|grant| ((grant.workflow_id.clone(), grant.port_id.clone()), grant))
        .collect::<BTreeMap<_, _>>();
    let mut narrowed = false;
    for (key, grant) in &proposed {
        let Some(previous) = current.get(key) else {
            return AppPermissionChangeKind::Expanded;
        };
        if *grant == *previous {
            continue;
        }
        if grant.purpose != previous.purpose
            || grant.kind != previous.kind
            || grant.period_seconds != previous.period_seconds
            || grant.reviewed_request_digest != previous.reviewed_request_digest
            || grant.severity_ceiling > previous.severity_ceiling
            || grant.max_notifications_per_period > previous.max_notifications_per_period
            || grant.max_pending > previous.max_pending
            || grant.ttl_seconds > previous.ttl_seconds
        {
            return AppPermissionChangeKind::Expanded;
        }
        narrowed = true;
    }
    if current.keys().any(|key| !proposed.contains_key(key)) {
        narrowed = true;
    }
    if narrowed {
        AppPermissionChangeKind::Narrowed
    } else {
        AppPermissionChangeKind::Unchanged
    }
}

fn behavior_resources_narrow(
    grant: &AppBehaviorResourceCeiling,
    previous: &AppBehaviorResourceCeiling,
) -> bool {
    grant.period_seconds == previous.period_seconds
        && grant.max_tokens_per_run <= previous.max_tokens_per_run
        && grant.max_cost_microusd_per_run <= previous.max_cost_microusd_per_run
        && grant.max_active_seconds_per_run <= previous.max_active_seconds_per_run
        && grant.max_tokens_per_month <= previous.max_tokens_per_month
        && grant.max_cost_microusd_per_month <= previous.max_cost_microusd_per_month
        && grant.max_starts_per_period <= previous.max_starts_per_period
        && grant.max_causation_depth <= previous.max_causation_depth
        && grant.max_spend_depth <= previous.max_spend_depth
        && grant.max_contribution_proposals_per_run <= previous.max_contribution_proposals_per_run
}

/// Reaching a host the owner did not approve is an expansion, and so is moving
/// from `Denied` to any destination at all.
fn compare_network_policy(
    current: &AppNetworkPolicy,
    proposed: &AppNetworkPolicy,
) -> AppPermissionChangeKind {
    match (current, proposed) {
        (AppNetworkPolicy::Denied, AppNetworkPolicy::Denied) => AppPermissionChangeKind::Unchanged,
        (AppNetworkPolicy::Denied, AppNetworkPolicy::ApprovedDestinations { destinations }) => {
            if destinations.is_empty() {
                AppPermissionChangeKind::Unchanged
            } else {
                AppPermissionChangeKind::Expanded
            }
        },
        (AppNetworkPolicy::ApprovedDestinations { destinations }, AppNetworkPolicy::Denied) => {
            if destinations.is_empty() {
                AppPermissionChangeKind::Unchanged
            } else {
                AppPermissionChangeKind::Narrowed
            }
        },
        (
            AppNetworkPolicy::ApprovedDestinations {
                destinations: current,
            },
            AppNetworkPolicy::ApprovedDestinations {
                destinations: proposed,
            },
        ) => compare_sets(current, proposed),
    }
}

/// Running unattended when it could not before, more often, or more
/// concurrently, are all expansions.
fn compare_background_execution(
    current: &AppBackgroundExecution,
    proposed: &AppBackgroundExecution,
) -> AppPermissionChangeKind {
    match (current, proposed) {
        (AppBackgroundExecution::Denied, AppBackgroundExecution::Denied) => {
            AppPermissionChangeKind::Unchanged
        },
        (AppBackgroundExecution::Denied, AppBackgroundExecution::Granted { .. }) => {
            AppPermissionChangeKind::Expanded
        },
        (AppBackgroundExecution::Granted { .. }, AppBackgroundExecution::Denied) => {
            AppPermissionChangeKind::Narrowed
        },
        (
            AppBackgroundExecution::Granted {
                min_interval_seconds: current_interval,
                max_concurrent_runs: current_concurrency,
            },
            AppBackgroundExecution::Granted {
                min_interval_seconds: proposed_interval,
                max_concurrent_runs: proposed_concurrency,
            },
        ) => {
            // A SMALLER minimum interval means it may run more often.
            let expanded =
                proposed_interval < current_interval || proposed_concurrency > current_concurrency;
            let narrowed =
                proposed_interval > current_interval || proposed_concurrency < current_concurrency;
            match (expanded, narrowed) {
                (true, _) => AppPermissionChangeKind::Expanded,
                (false, true) => AppPermissionChangeKind::Narrowed,
                (false, false) => AppPermissionChangeKind::Unchanged,
            }
        },
    }
}

pub fn authorize_update_switch(
    plan: &AppUpdateSwitchPlan,
    permission_diff: &AppPermissionDiff,
    migration: Option<&AppCompiledMigrationPlan>,
    reviewed: bool,
) -> Result<(), AppUpdateError> {
    if permission_diff.requires_review && !reviewed {
        return Err(AppUpdateError::ReviewRequired);
    }
    if plan.state != AppUpdateSwitchState::Quiesced {
        return Err(AppUpdateError::NotQuiesced);
    }
    if migration
        .is_some_and(|plan| plan.compatibility() == AppSchemaCompatibility::MigrationRequired)
        && plan.backup_export_digest.is_none()
    {
        return Err(AppUpdateError::BackupReceiptRequired);
    }
    Ok(())
}

pub fn switch_active_generation(
    plan: &AppUpdateSwitchPlan,
    live_source_generation: u64,
    live_source_tail: u64,
) -> Result<AppUpdateSwitchPlan, AppUpdateError> {
    if plan.source_generation != live_source_generation
        || plan.source_tail_change_seq != live_source_tail
    {
        return Err(AppUpdateError::SourceGenerationMoved);
    }
    if plan.state != AppUpdateSwitchState::Quiesced {
        return Err(AppUpdateError::NotQuiesced);
    }
    let mut next = plan.clone();
    next.state = AppUpdateSwitchState::Switched;
    Ok(next)
}

pub fn rollback_decision(
    compatibility: AppSchemaCompatibility,
    kind: AppRollbackKind,
    confirmed: bool,
) -> Result<AppRollbackDecision, AppUpdateError> {
    match (kind, compatibility) {
        (AppRollbackKind::CodeOnly, AppSchemaCompatibility::Compatible) => {
            Ok(AppRollbackDecision {
                kind,
                discards_post_update_writes: false,
                requires_explicit_confirmation: false,
            })
        },
        (AppRollbackKind::CodeOnly, _) => Err(AppUpdateError::CodeOnlyUnavailable),
        (AppRollbackKind::DataRewind, _) if !confirmed => {
            Err(AppUpdateError::ExplicitRewindRequired)
        },
        (AppRollbackKind::DataRewind, _) => Ok(AppRollbackDecision {
            kind,
            discards_post_update_writes: true,
            requires_explicit_confirmation: true,
        }),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRollbackUx {
    pub kind: AppRollbackKind,
    pub confirmation_required: bool,
    pub discards_post_update_writes: bool,
    pub summary: String,
}

/// Operator-facing rollback copy. It cannot authorize a rewind.
pub fn rollback_ux(decision: &AppRollbackDecision) -> AppRollbackUx {
    let summary = match decision.kind {
        AppRollbackKind::CodeOnly => "Restore the previous package generation. Records written \
                                      after the switch stay."
            .to_owned(),
        AppRollbackKind::DataRewind => "Rewind records to the backup receipt. Writes made after \
                                        the update are discarded."
            .to_owned(),
    };
    AppRollbackUx {
        kind: decision.kind,
        confirmation_required: decision.requires_explicit_confirmation,
        discards_post_update_writes: decision.discards_post_update_writes,
        summary,
    }
}

fn compare_sets<T: PartialEq>(current: &[T], proposed: &[T]) -> AppPermissionChangeKind {
    let added = proposed.iter().any(|item| !current.contains(item));
    let removed = current.iter().any(|item| !proposed.contains(item));
    match (added, removed) {
        (false, false) => AppPermissionChangeKind::Unchanged,
        (true, _) => AppPermissionChangeKind::Expanded,
        (false, true) => AppPermissionChangeKind::Narrowed,
    }
}

fn compare_personal_agent(
    current: &AppGrantRevision,
    proposed: &AppGrantRevision,
) -> AppPermissionChangeKind {
    if proposed.granted_personal_agent_data_access.len()
        > current.granted_personal_agent_data_access.len()
        || proposed.granted_data_handling_policy.personal_agent_access
            > current.granted_data_handling_policy.personal_agent_access
    {
        AppPermissionChangeKind::Expanded
    } else if proposed.granted_personal_agent_data_access.len()
        < current.granted_personal_agent_data_access.len()
        || proposed.granted_data_handling_policy.personal_agent_access
            < current.granted_data_handling_policy.personal_agent_access
    {
        AppPermissionChangeKind::Narrowed
    } else {
        AppPermissionChangeKind::Unchanged
    }
}

fn compare_policy(
    current: &AppDataHandlingPolicy,
    proposed: &AppDataHandlingPolicy,
) -> AppPermissionChangeKind {
    let expanded = proposed.classification_floor < current.classification_floor
        || proposed.model_processing > current.model_processing
        || proposed.external_egress > current.external_egress
        || proposed.memory_promotion > current.memory_promotion;
    let narrowed = proposed.classification_floor > current.classification_floor
        || proposed.model_processing < current.model_processing
        || proposed.external_egress < current.external_egress
        || proposed.memory_promotion < current.memory_promotion;
    match (expanded, narrowed) {
        (true, _) => AppPermissionChangeKind::Expanded,
        (false, true) => AppPermissionChangeKind::Narrowed,
        (false, false) => AppPermissionChangeKind::Unchanged,
    }
}

fn compare_resources(
    current: &AppResourceCeiling,
    proposed: &AppResourceCeiling,
) -> AppPermissionChangeKind {
    let expanded = proposed.max_records > current.max_records
        || proposed.max_payload_bytes > current.max_payload_bytes
        || proposed.max_input_tokens > current.max_input_tokens
        || proposed.max_cost_microusd > current.max_cost_microusd;
    let narrowed = proposed.max_records < current.max_records
        || proposed.max_payload_bytes < current.max_payload_bytes
        || proposed.max_input_tokens < current.max_input_tokens
        || proposed.max_cost_microusd < current.max_cost_microusd;
    match (expanded, narrowed) {
        (true, _) => AppPermissionChangeKind::Expanded,
        (false, true) => AppPermissionChangeKind::Narrowed,
        (false, false) => AppPermissionChangeKind::Unchanged,
    }
}

pub const APP_UPDATE_PLAN_RECORD_CEILING: u64 = APP_MIGRATION_TAIL_RECORD_CEILING;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppUpdatePlanRequest {
    pub attempt_id: AppReference,
    pub expected_parked_generation: u64,
    /// Empty means infer the unique safe V1 plan. Rename and enum remapping
    /// require exact reviewed operation bodies here; publisher SQL is never
    /// accepted.
    #[serde(default)]
    pub migration_operations: Vec<AppMigrationOperation>,
}

impl ValidateAppContract for AppUpdatePlanRequest {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.expected_parked_generation == 0 {
            return Err(AppContractError::invalid(
                "expected_parked_generation",
                "must be greater than zero",
            ));
        }
        if self.migration_operations.len() > limits.max_collection_items() {
            return Err(AppContractError::invalid(
                "migration_operations",
                "exceeds the fixed operation ceiling",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppUpdateCoordinatorState {
    DryRunPassed,
    BackupRecorded,
    ReadyToSwitch,
    Switched,
    RewindReviewPending,
    Aborted,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppDurableUpdateRun {
    pub migration_run_id: AppReference,
    pub installation_id: AppInstallationId,
    pub attempt_id: AppReference,
    pub attempt_kind: AppLifecycleAttemptKind,
    pub source_fence: AppReviewableRevisionSourceFence,
    pub destination_package_revision_ref: AppReference,
    pub destination_schema_revision: AppRevision,
    pub destination_schema_preview: AppSchemaRevision,
    pub destination_dataset_generation: u64,
    pub permission_diff: AppPermissionDiff,
    pub schema_diff_digest: AppDigest,
    pub surface_diff_digest: AppDigest,
    pub data_diff_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_plan: Option<AppCompiledMigrationPlan>,
    pub update_plan_digest: AppDigest,
    pub destructive: bool,
    pub dry_run_examined: u64,
    pub dry_run_representable: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_receipt: Option<AppUpdateBackupReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_receipt: Option<AppUpdateRollbackReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewind_review: Option<AppDataRewindReviewFence>,
    pub state: AppUpdateCoordinatorState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppUpdateBackupReceipt {
    pub envelope_version: u8,
    pub logical_payload_digest: AppDigest,
    pub envelope_header_digest: AppDigest,
    pub ciphertext_digest: AppDigest,
    pub byte_count: u64,
    pub encrypted: bool,
    pub source_fence_digest: AppDigest,
    pub update_plan_digest: AppDigest,
    pub backup_ref: AppReference,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCodeOnlyRollbackRequest {
    pub migration_run_id: AppReference,
    pub expected_installation_generation: u64,
    pub request_id: AppReference,
}

impl ValidateAppContract for AppCodeOnlyRollbackRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.expected_installation_generation == 0 {
            return Err(AppContractError::invalid(
                "expected_installation_generation",
                "must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppUpdateRollbackReceipt {
    pub rollback_ref: AppReference,
    pub request_id: AppReference,
    pub kind: AppRollbackKind,
    pub migration_run_id: AppReference,
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_revision_ref: AppReference,
    pub grant_revision: AppRevision,
    pub schema_revision: AppRevision,
    pub surface_revision: AppRevision,
    pub post_update_writes_retained: bool,
    pub grants_restored: bool,
    pub rolled_back_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDataRewindReviewFence {
    pub installation_generation: u64,
    pub source_change_seq_high_water: u64,
    pub staged_head_count: u64,
    pub transformed_source_digest: AppDigest,
    pub preview_digest: AppDigest,
    pub record_decisions: Vec<AppDataImportRecordDecision>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDataRewindPreviewReceipt {
    pub migration_run_id: AppReference,
    pub installation_id: AppInstallationId,
    pub backup_ref: AppReference,
    pub update_plan_digest: AppDigest,
    pub preview: AppDataImportPreview,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDataRewindCommitRequest {
    pub migration_run_id: AppReference,
    pub preview_digest: AppDigest,
    pub request_id: AppReference,
    pub explicit_rewind_confirmed: bool,
}

impl ValidateAppContract for AppDataRewindCommitRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if !self.explicit_rewind_confirmed {
            return Err(AppContractError::invalid(
                "explicit_rewind_confirmed",
                "must be true for data rewind",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDataRewindCommitReceipt {
    pub rollback: AppUpdateRollbackReceipt,
    pub import_receipt: AppDataImportReceipt,
    pub record_decisions: Vec<AppDataImportRecordDecision>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppUpdatePlanReceipt {
    pub migration_run_id: AppReference,
    pub installation_id: AppInstallationId,
    pub attempt_id: AppReference,
    pub attempt_kind: AppLifecycleAttemptKind,
    pub source_fence_digest: AppDigest,
    pub destination_package_revision_ref: AppReference,
    pub destination_schema_revision: AppRevision,
    pub destination_dataset_generation: u64,
    pub permission_diff: AppPermissionDiff,
    pub schema_diff_digest: AppDigest,
    pub surface_diff_digest: AppDigest,
    pub data_diff_digest: AppDigest,
    pub migration_operations: Vec<AppMigrationOperation>,
    pub migration_plan_digest: Option<AppDigest>,
    pub update_plan_digest: AppDigest,
    pub destructive: bool,
    pub backup_required: bool,
    pub dry_run_examined: u64,
    pub dry_run_representable: u64,
    pub state: AppUpdateCoordinatorState,
}

impl From<&AppDurableUpdateRun> for AppUpdatePlanReceipt {
    fn from(run: &AppDurableUpdateRun) -> Self {
        Self {
            migration_run_id: run.migration_run_id.clone(),
            installation_id: run.installation_id.clone(),
            attempt_id: run.attempt_id.clone(),
            attempt_kind: run.attempt_kind,
            source_fence_digest: run.source_fence.fence_digest.clone(),
            destination_package_revision_ref: run.destination_package_revision_ref.clone(),
            destination_schema_revision: run.destination_schema_revision,
            destination_dataset_generation: run.destination_dataset_generation,
            permission_diff: run.permission_diff.clone(),
            schema_diff_digest: run.schema_diff_digest.clone(),
            surface_diff_digest: run.surface_diff_digest.clone(),
            data_diff_digest: run.data_diff_digest.clone(),
            migration_operations: run
                .migration_plan
                .as_ref()
                .map(|plan| plan.operations().to_vec())
                .unwrap_or_default(),
            migration_plan_digest: run
                .migration_plan
                .as_ref()
                .map(|plan| plan.plan_digest().clone()),
            update_plan_digest: run.update_plan_digest.clone(),
            destructive: run.destructive,
            backup_required: run.backup_required(),
            dry_run_examined: run.dry_run_examined,
            dry_run_representable: run.dry_run_representable,
            state: run.state,
        }
    }
}

impl AppDurableUpdateRun {
    pub fn backup_required(&self) -> bool {
        self.destructive
            || self
                .migration_plan
                .as_ref()
                .is_some_and(|plan| plan.record_processing_backup_required(self.dry_run_examined))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AppStagedMigrationRecord {
    entity: AppName,
    record_id: AppRecordId,
    source_record_revision: AppRevision,
    source_change_seq: u64,
    payload: Value,
    handling_policy: AppDataHandlingPolicy,
    created_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Error)]
pub enum AppUpdateCoordinatorError {
    #[error(transparent)]
    Authentication(#[from] AppAuthorityError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Store(#[from] AppEntityStoreError),
    #[error(transparent)]
    Mutation(#[from] AppEntityMutationError),
    #[error(transparent)]
    Staging(#[from] AppPackageStagingError),
    #[error(transparent)]
    Schema(#[from] AppSchemaCompilerError),
    #[error(transparent)]
    Portability(#[from] AppEntityPortabilityError),
    #[error(transparent)]
    ArchivePolicy(#[from] AppPortabilityError),
    #[error(transparent)]
    Archive(#[from] AppPortableArchiveTransferError),
    #[error(transparent)]
    Migration(#[from] AppMigrationError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("app update encrypted backup I/O failed: {0}")]
    BackupIo(String),
    #[error("app update candidate is unavailable or stale: {0}")]
    StaleCandidate(String),
    #[error("app update migration plan does not produce the destination schema: {0}")]
    DestinationMismatch(String),
    #[error("app update migration dry-run is not total: {0}")]
    DryRunFailed(String),
    #[error("app update coordinator record is corrupt: {0}")]
    Corrupt(String),
    #[error("app update operation requires a reviewed encrypted backup")]
    BackupRequired,
    #[error("app update destructive migration requires explicit confirmation")]
    DestructiveConfirmationRequired,
}

#[derive(Clone)]
pub struct AppUpdateCoordinatorService {
    registry: AppRegistryService,
    stager: AppPackageStager,
}

impl AppUpdateCoordinatorService {
    pub fn from_parts(registry: AppRegistryService, stager: AppPackageStager) -> Self {
        Self { registry, stager }
    }

    /// Compile, dry-run and durably stage one exact update/reinstall plan.
    /// Ordinary reads still point at the current record heads; staged rows are
    /// immutable and have no execution authority.
    pub async fn prepare_plan(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        request: AppUpdatePlanRequest,
        permission_diff: AppPermissionDiff,
        destination_data_handling_policy: AppDataHandlingPolicy,
        now: DateTime<Utc>,
    ) -> Result<AppUpdatePlanReceipt, AppUpdateCoordinatorError> {
        authenticated.ensure_live_at(&now)?;
        let installation = self
            .registry
            .installation(authenticated, installation_id, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("installation missing".into())
            })?;
        if installation.lifecycle.generation != request.expected_parked_generation {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "parked installation generation changed".into(),
            ));
        }
        let attempt = self
            .registry
            .lifecycle_attempt(authenticated, &request.attempt_id, now)
            .await?
            .ok_or_else(|| AppUpdateCoordinatorError::StaleCandidate("attempt missing".into()))?;
        if attempt.installation_id.as_ref() != Some(installation_id)
            || !matches!(
                attempt.kind,
                AppLifecycleAttemptKind::Update | AppLifecycleAttemptKind::Reinstall
            )
            || attempt.state != super::lifecycle::AppLifecycleAttemptState::ReadyForReview
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "attempt is not the current reviewable update/reinstall".into(),
            ));
        }
        let source_fence = self
            .registry
            .reviewable_revision_source_fence(authenticated, installation_id, attempt.kind, now)
            .await?;
        if attempt.source_installation_generation
            != Some(source_fence.source_installation_generation)
            || attempt.permission_migration_diff_ref.as_ref()
                != Some(&source_fence.permission_migration_diff_ref()?)
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "attempt source fence changed".into(),
            ));
        }
        let candidate_package = self
            .registry
            .package_revision(authenticated, &attempt.candidate_package_revision_ref, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("candidate package missing".into())
            })?;
        let source_package = self
            .registry
            .package_revision(
                authenticated,
                &source_fence.source_package_revision_ref,
                now,
            )
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("source package missing".into())
            })?;
        let active = super::entity_store::AppEntityStoreService::new(self.registry.clone())
            .data_owner_schema(authenticated, installation_id, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("source schema missing".into())
            })?;
        if active.schema_revision() != source_fence.source_schema_revision
            || active.grant_revision() != source_fence.source_grant_revision
            || active.active_surface_revision() != Some(source_fence.source_surface_revision)
            || active.package_revision_ref() != &source_fence.source_package_revision_ref
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "source grant/schema/surface binding changed".into(),
            ));
        }
        let staged = self
            .stager
            .load_staged_package(authenticated, candidate_package.content_digest.clone(), now)
            .await?;
        let destination_schema_revision = AppRevision::new(
            source_fence
                .source_schema_revision
                .get()
                .checked_add(1)
                .ok_or_else(|| {
                    AppUpdateCoordinatorError::Corrupt("schema revision exhausted".into())
                })?,
        )?;
        let target_schema = compile_app_schema(
            staged.candidate().manifest().manifest(),
            installation_id.clone(),
            attempt.candidate_package_revision_ref.clone(),
            destination_schema_revision,
            // Preview the policy the server's review will grant at activation.
            // The current grant governs the source, not the destination.
            destination_data_handling_policy.clone(),
            AppSchemaCompatibility::Compatible,
            None,
            &candidate_package.entity_schema_digest,
            now,
        )?
        .into_revision();
        let source_fields = migration_fields_from_schema(active.schema())?;
        let destination_fields = migration_fields_from_schema(&target_schema)?;
        let policy_only = !request.migration_operations.is_empty()
            && request
                .migration_operations
                .iter()
                .all(AppMigrationOperation::enables_remote_processing);
        for operation in &request.migration_operations {
            if let AppMigrationOperation::EnableRemoteProcessingForExistingRecords { entity } =
                operation
            {
                if destination_data_handling_policy.model_processing
                    != AppModelProcessing::RemoteAllowed
                    || !source_fields.contains_key(entity)
                    || !destination_fields.contains_key(entity)
                {
                    return Err(AppUpdateCoordinatorError::DestinationMismatch(
                        "record processing transition requires an existing entity and an explicitly reviewed remote_allowed destination".into(),
                    ));
                }
            }
        }
        let migration_plan = if source_package.entity_schema_digest
            == candidate_package.entity_schema_digest
            && !policy_only
            && active.schema().compiled_index_plan == target_schema.compiled_index_plan
        {
            if !request.migration_operations.is_empty() {
                return Err(AppUpdateCoordinatorError::DestinationMismatch(
                    "code-only update supplied data operations".into(),
                ));
            }
            None
        } else {
            let plan_ref = update_plan_reference(
                "migration-plan",
                &request.attempt_id,
                &source_package.entity_schema_digest,
                &candidate_package.entity_schema_digest,
                &request.migration_operations,
            )?;
            let plan = if request.migration_operations.is_empty() {
                compile_inferred_migration(
                    plan_ref,
                    source_package.entity_schema_digest.clone(),
                    candidate_package.entity_schema_digest.clone(),
                    &source_fields,
                    &destination_fields,
                )?
            } else {
                compile_migration_plan(
                    plan_ref,
                    source_package.entity_schema_digest.clone(),
                    candidate_package.entity_schema_digest.clone(),
                    request.migration_operations,
                )?
            };
            Some(plan)
        };
        let schema_diff_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "source": source_package.entity_schema_digest,
            "destination": candidate_package.entity_schema_digest,
            "source_revision": source_fence.source_schema_revision,
            "destination_revision": destination_schema_revision,
            "destination_compiled_schema": AppDigest::blake3_canonical_json(
                &target_schema.canonical_entity_schema,
            )?,
            "destination_index_plan": AppDigest::blake3_canonical_json(
                &target_schema.compiled_index_plan,
            )?,
        }))?;
        let surface_diff_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "source": source_package.view_schema_digest,
            "destination": candidate_package.view_schema_digest,
            "source_revision": source_fence.source_surface_revision,
        }))?;
        let data_diff_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(
            migration_plan
                .as_ref()
                .map(AppCompiledMigrationPlan::operations)
                .unwrap_or(&[]),
        )?)?;
        let destructive = migration_plan.as_ref().is_some_and(|plan| {
            plan.operations().iter().any(|operation| {
                matches!(
                    operation,
                    AppMigrationOperation::RenameField { .. }
                        | AppMigrationOperation::MapEnum { .. }
                        | AppMigrationOperation::RetireField { .. }
                )
            })
        });
        let update_plan_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "protocol": "magician.app-update-plan.v1",
            "attempt_id": request.attempt_id,
            "source_fence_digest": source_fence.fence_digest,
            "destination_package_revision_ref": attempt.candidate_package_revision_ref,
            "destination_schema_revision": destination_schema_revision,
            "permission_diff_digest": permission_diff.diff_digest,
            "schema_diff_digest": schema_diff_digest,
            "surface_diff_digest": surface_diff_digest,
            "data_diff_digest": data_diff_digest,
            "migration_plan_digest": migration_plan.as_ref().map(AppCompiledMigrationPlan::plan_digest),
            "destructive": destructive,
        }))?;
        let migration_run_id = AppReference::parse(format!(
            "migration:app-update:{}",
            update_plan_digest.as_str().trim_start_matches("blake3:")
        ))?;
        let installation_id = installation_id.clone();
        let attempt_id = request.attempt_id;
        let attempt_kind = attempt.kind;
        let destination_package_revision_ref = attempt.candidate_package_revision_ref;
        let prepared = self
            .registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                stage_update_plan_blocking(
                    connection,
                    scope,
                    migration_run_id,
                    installation_id,
                    attempt_id,
                    attempt_kind,
                    source_fence,
                    destination_package_revision_ref,
                    destination_schema_revision,
                    permission_diff,
                    schema_diff_digest,
                    surface_diff_digest,
                    data_diff_digest,
                    migration_plan,
                    update_plan_digest,
                    destructive,
                    target_schema,
                    now,
                )
            })
            .await?;
        Ok(AppUpdatePlanReceipt::from(&prepared))
    }

    pub async fn plan(
        &self,
        authenticated: &AuthenticatedAppScope,
        migration_run_id: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppUpdatePlanReceipt>, AppUpdateCoordinatorError> {
        let migration_run_id = migration_run_id.clone();
        let loaded = self
            .registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, _| {
                load_update_run(connection, &migration_run_id)
            })
            .await?;
        Ok(loaded.flatten().as_ref().map(AppUpdatePlanReceipt::from))
    }

    /// Revalidate the exact durable coordinator evidence immediately before
    /// the reviewed lifecycle commit is constructed. The registry transaction
    /// repeats these bindings before it switches active generations.
    #[allow(clippy::too_many_arguments)]
    pub async fn authorize_reviewed_switch(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        attempt_id: &AppReference,
        migration_run_id: &AppReference,
        expected_update_plan_digest: &AppDigest,
        expected_permission_diff: &AppPermissionDiff,
        destructive_confirmed: bool,
        now: DateTime<Utc>,
    ) -> Result<AppDurableUpdateRun, AppUpdateCoordinatorError> {
        authenticated.ensure_live_at(&now)?;
        let run = self
            .load_run(authenticated, migration_run_id, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("migration run missing".into())
            })?;
        if run.state != AppUpdateCoordinatorState::ReadyToSwitch
            || &run.installation_id != installation_id
            || &run.attempt_id != attempt_id
            || &run.update_plan_digest != expected_update_plan_digest
            || &run.permission_diff != expected_permission_diff
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "reviewed update evidence is stale or mismatched".into(),
            ));
        }
        if run.destructive && !destructive_confirmed {
            return Err(AppUpdateCoordinatorError::DestructiveConfirmationRequired);
        }
        if run.backup_required()
            && run.backup_receipt.as_ref().is_none_or(|receipt| {
                !receipt.encrypted
                    || receipt.source_fence_digest != run.source_fence.fence_digest
                    || receipt.update_plan_digest != run.update_plan_digest
            })
        {
            return Err(AppUpdateCoordinatorError::BackupRequired);
        }
        let current_fence = self
            .registry
            .reviewable_revision_source_fence(authenticated, installation_id, run.attempt_kind, now)
            .await?;
        if current_fence != run.source_fence {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "source fence changed after migration review".into(),
            ));
        }
        Ok(run)
    }

    /// Materialize the exact pre-update data snapshot through the portable
    /// archive owner. A destructive or record-processing transition plan needs
    /// this durable encrypted receipt before switch authority. Dry-run output is
    /// copied into the hidden immutable generation.
    pub async fn record_encrypted_backup(
        &self,
        authenticated: &AuthenticatedAppScope,
        migration_run_id: &AppReference,
        passphrase: AppArchivePassphrase,
        now: DateTime<Utc>,
    ) -> Result<AppUpdatePlanReceipt, AppUpdateCoordinatorError> {
        authenticated.ensure_live_at(&now)?;
        let run = self
            .load_run(authenticated, migration_run_id, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("migration run missing".into())
            })?;
        if !run.backup_required() {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "migration plan does not require a pre-update backup".into(),
            ));
        }
        if !matches!(
            run.state,
            AppUpdateCoordinatorState::DryRunPassed
                | AppUpdateCoordinatorState::BackupRecorded
                | AppUpdateCoordinatorState::ReadyToSwitch
        ) {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "migration run no longer accepts a backup".into(),
            ));
        }

        let data = AppDataPortabilityService::new(self.registry.clone())
            .export_data(authenticated, &run.installation_id, now)
            .await?;
        let logical = AppLogicalArchive::Data { data };
        let plan = authorize_archive_write(
            &logical,
            AppArchiveProtectionRequest::Encrypted,
            None,
            authenticated,
            now,
            &AppContractLimits::default(),
        )?;
        let backup_path = update_backup_path(&self.registry, authenticated, &run.migration_run_id)?;
        let physical = read_or_create_encrypted_backup(
            &backup_path,
            &logical,
            &plan,
            &passphrase,
            authenticated,
            now,
        )?;
        if !physical.receipt.encrypted || physical.logical != logical {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "backup archive does not match the exact source snapshot".into(),
            ));
        }
        let backup_ref = update_backup_reference(
            &run.migration_run_id,
            &run.source_fence.fence_digest,
            &run.update_plan_digest,
            &physical.receipt.ciphertext_digest,
        )?;
        let receipt = AppUpdateBackupReceipt {
            envelope_version: physical.receipt.envelope_version,
            logical_payload_digest: physical.receipt.logical_payload_digest,
            envelope_header_digest: physical.receipt.envelope_header_digest,
            ciphertext_digest: physical.receipt.ciphertext_digest,
            byte_count: physical.receipt.byte_count,
            encrypted: physical.receipt.encrypted,
            source_fence_digest: run.source_fence.fence_digest.clone(),
            update_plan_digest: run.update_plan_digest.clone(),
            backup_ref,
        };

        let migration_run_id_for_record = migration_run_id.clone();
        let migration_run_id_for_stage = migration_run_id.clone();
        let receipt_for_record = receipt.clone();
        self.registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                record_update_backup_blocking(
                    connection,
                    scope,
                    &migration_run_id_for_record,
                    receipt_for_record,
                    now,
                )
            })
            .await?;
        let ready = self
            .registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                stage_backed_update_blocking(connection, scope, &migration_run_id_for_stage, now)
            })
            .await?;
        Ok(AppUpdatePlanReceipt::from(&ready))
    }

    /// Abort only an unswitched coordinator run. Once `Switched` is durable,
    /// callers must use an explicit rollback operation instead of disguising
    /// a rewind as update cancellation.
    pub async fn abort_before_switch(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<AppUpdatePlanReceipt>, AppUpdateCoordinatorError> {
        authenticated.ensure_live_at(&now)?;
        let installation_id = installation_id.clone();
        let aborted = self
            .registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                abort_update_before_switch_blocking(connection, scope, &installation_id, now)
            })
            .await?;
        Ok(aborted.as_ref().map(AppUpdatePlanReceipt::from))
    }

    /// Restore the previous code/package surface for a code-only update while
    /// retaining the authority axes currently in force and every post-switch
    /// record write. Data-changing runs must use the separately reviewed
    /// rewind path.
    pub async fn rollback_code_only(
        &self,
        authenticated: &AuthenticatedAppScope,
        request: AppCodeOnlyRollbackRequest,
        now: DateTime<Utc>,
    ) -> Result<AppUpdateRollbackReceipt, AppUpdateCoordinatorError> {
        authenticated.ensure_live_at(&now)?;
        request.validate_app_contract(&AppContractLimits::default())?;
        let authenticated_for_write = authenticated.clone();
        let receipt = self
            .registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                rollback_code_only_blocking(
                    connection,
                    scope,
                    &authenticated_for_write,
                    request,
                    now,
                )
            })
            .await?;
        self.registry.hide_computed_capability_scope(authenticated);
        Ok(receipt)
    }

    /// Produce the exact D2 conflict/new-local-ID review for restoring the
    /// encrypted pre-update snapshot into the current schema. Current heads
    /// are fenced and staged for tombstoning only after this reviewed import
    /// commits successfully.
    pub async fn preview_data_rewind(
        &self,
        authenticated: &AuthenticatedAppScope,
        migration_run_id: &AppReference,
        passphrase: AppArchivePassphrase,
        now: DateTime<Utc>,
    ) -> Result<AppDataRewindPreviewReceipt, AppUpdateCoordinatorError> {
        authenticated.ensure_live_at(&now)?;
        let run = self
            .load_run(authenticated, migration_run_id, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("migration run missing".into())
            })?;
        if !matches!(
            run.state,
            AppUpdateCoordinatorState::Switched | AppUpdateCoordinatorState::RewindReviewPending
        ) || run.migration_plan.is_none()
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "data rewind requires a switched data-changing update".into(),
            ));
        }
        let (source, backup_ref) = self
            .transformed_rewind_source(authenticated, &run, &passphrase, now)
            .await?;
        let portability = AppDataPortabilityService::new(self.registry.clone());
        let preview = portability
            .preview_import(authenticated, &run.installation_id, &source, now)
            .await?;
        if preview.status == AppDataImportPreviewStatus::Blocked {
            return Err(AppUpdateCoordinatorError::DestinationMismatch(
                "the exact backup cannot be represented in the current reviewed schema".into(),
            ));
        }
        let migration_run_id = migration_run_id.clone();
        let preview_for_stage = preview.clone();
        let transformed_source_digest = source.logical_payload_digest.clone();
        self.registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                stage_data_rewind_review_blocking(
                    connection,
                    scope,
                    &migration_run_id,
                    &preview_for_stage,
                    transformed_source_digest,
                    now,
                )
            })
            .await?;
        Ok(AppDataRewindPreviewReceipt {
            migration_run_id: run.migration_run_id,
            installation_id: run.installation_id,
            backup_ref,
            update_plan_digest: run.update_plan_digest,
            preview,
        })
    }

    /// Consume one exact reviewed D2 preview, then tombstone the fenced
    /// post-update heads. Imported backup records use D2 destination-local IDs;
    /// conflicts remain skipped and are exposed in the returned decisions.
    pub async fn commit_data_rewind(
        &self,
        authenticated: &AuthenticatedAppScope,
        request: AppDataRewindCommitRequest,
        passphrase: AppArchivePassphrase,
        now: DateTime<Utc>,
    ) -> Result<AppDataRewindCommitReceipt, AppUpdateCoordinatorError> {
        authenticated.ensure_live_at(&now)?;
        request.validate_app_contract(&AppContractLimits::default())?;
        let run = self
            .load_run(authenticated, &request.migration_run_id, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("migration run missing".into())
            })?;
        if run.state == AppUpdateCoordinatorState::RolledBack {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "data rewind already committed; use the durable import receipt lookup".into(),
            ));
        }
        let review = run.rewind_review.clone().ok_or_else(|| {
            AppUpdateCoordinatorError::StaleCandidate("data rewind was not previewed".into())
        })?;
        if run.state != AppUpdateCoordinatorState::RewindReviewPending
            || review.preview_digest != request.preview_digest
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "reviewed data-rewind preview changed".into(),
            ));
        }
        let portability = AppDataPortabilityService::new(self.registry.clone());
        let import_receipt = if let Some(receipt) = portability
            .committed_import_receipt(
                authenticated,
                &run.installation_id,
                &review.preview_digest,
                now,
            )
            .await?
        {
            receipt
        } else {
            let (source, _) = self
                .transformed_rewind_source(authenticated, &run, &passphrase, now)
                .await?;
            if source.logical_payload_digest != review.transformed_source_digest {
                return Err(AppUpdateCoordinatorError::StaleCandidate(
                    "rewind source changed after review".into(),
                ));
            }
            let preview = portability
                .preview_import(authenticated, &run.installation_id, &source, now)
                .await?;
            if preview.preview_digest != review.preview_digest
                || preview.record_decisions != review.record_decisions
            {
                return Err(AppUpdateCoordinatorError::StaleCandidate(
                    "data-rewind conflicts or local IDs changed".into(),
                ));
            }
            let approval_ref = update_rollback_reference(
                &run.migration_run_id,
                &request.request_id,
                AppRollbackKind::DataRewind,
                review.installation_generation,
            )?;
            let maximum_expiry = now + Duration::minutes(5);
            let expires_at = if maximum_expiry < *authenticated.expires_at() {
                maximum_expiry
            } else {
                *authenticated.expires_at()
            };
            let approval = portability.approve_import(
                authenticated,
                approval_ref,
                &preview,
                now,
                expires_at,
            )?;
            portability
                .commit_import(authenticated, source, preview, approval, now)
                .await?
        };
        let authenticated_for_write = authenticated.clone();
        let request_for_commit = request.clone();
        let decisions = review.record_decisions.clone();
        let receipt = self
            .registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                finalize_data_rewind_blocking(
                    connection,
                    scope,
                    &authenticated_for_write,
                    request_for_commit,
                    import_receipt,
                    decisions,
                    now,
                )
            })
            .await?;
        self.registry.hide_computed_capability_scope(authenticated);
        self.registry
            .tombstone_app_memory_index_projection_for_scope(
                authenticated,
                &format!(
                    "app-data-rewind:{}:{}",
                    receipt.rollback.migration_run_id, receipt.rollback.rollback_ref
                ),
            )
            .await
            .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
        Ok(receipt)
    }

    async fn transformed_rewind_source(
        &self,
        authenticated: &AuthenticatedAppScope,
        run: &AppDurableUpdateRun,
        passphrase: &AppArchivePassphrase,
        now: DateTime<Utc>,
    ) -> Result<(AppDataArchiveManifest, AppReference), AppUpdateCoordinatorError> {
        let stored_receipt = run
            .backup_receipt
            .as_ref()
            .ok_or(AppUpdateCoordinatorError::BackupRequired)?;
        let path = update_backup_path(&self.registry, authenticated, &run.migration_run_id)?;
        let bytes = fs::read(&path).map_err(|error| {
            AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
        })?;
        let decoded = decode_app_portable_archive(&bytes, Some(passphrase))?;
        if !decoded.receipt.encrypted
            || decoded.receipt.logical_payload_digest != stored_receipt.logical_payload_digest
            || decoded.receipt.envelope_header_digest != stored_receipt.envelope_header_digest
            || decoded.receipt.ciphertext_digest != stored_receipt.ciphertext_digest
            || decoded.receipt.byte_count != stored_receipt.byte_count
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "encrypted backup receipt changed".into(),
            ));
        }
        let AppLogicalArchive::Data { data } = decoded.logical else {
            return Err(AppUpdateCoordinatorError::Corrupt(
                "update backup is not a data archive".into(),
            ));
        };
        let installation = self
            .registry
            .installation(authenticated, &run.installation_id, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("installation missing".into())
            })?;
        if installation.package_revision_ref != run.destination_package_revision_ref
            || installation.lifecycle.status != AppInstallationStatus::Enabled
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "current rollback destination changed".into(),
            ));
        }
        let package = self
            .registry
            .package_revision(authenticated, &installation.package_revision_ref, now)
            .await?
            .ok_or_else(|| {
                AppUpdateCoordinatorError::StaleCandidate("current package missing".into())
            })?;
        let migration_plan = run.migration_plan.as_ref().ok_or_else(|| {
            AppUpdateCoordinatorError::StaleCandidate("data migration plan missing".into())
        })?;
        let schema_revision = installation.active_schema_revision.ok_or_else(|| {
            AppUpdateCoordinatorError::StaleCandidate("current schema missing".into())
        })?;
        let mut records = data.records;
        for record in &mut records {
            record.payload =
                apply_migration_to_payload(migration_plan, &record.entity_name, &record.payload)?;
            record.payload_digest = AppDigest::blake3_canonical_json(&record.payload)?;
            record.schema_revision = schema_revision;
        }
        let transformed = AppDataArchiveManifest::from_trusted_export_projection(
            package.package_id,
            package.content_digest,
            package.entity_schema_digest,
            AppExportSourceState::Enabled,
            records,
            data.attachments,
            &AppContractLimits::default(),
        )?;
        Ok((transformed, stored_receipt.backup_ref.clone()))
    }

    async fn load_run(
        &self,
        authenticated: &AuthenticatedAppScope,
        migration_run_id: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppDurableUpdateRun>, AppUpdateCoordinatorError> {
        let migration_run_id = migration_run_id.clone();
        Ok(self
            .registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, _| {
                load_update_run(connection, &migration_run_id)
            })
            .await?
            .flatten())
    }
}

fn update_backup_path(
    registry: &AppRegistryService,
    authenticated: &AuthenticatedAppScope,
    migration_run_id: &AppReference,
) -> Result<PathBuf, AppUpdateCoordinatorError> {
    let scope = authenticated.scope();
    let apps_root = registry
        .workspace_layout()
        .apps_root(scope.principal.as_str(), scope.workspace.as_str());
    ensure_backup_directory(&apps_root)?;
    let backups = apps_root.join("update-backups");
    ensure_backup_directory(&backups)?;
    let digest = AppDigest::blake3(migration_run_id.as_str().as_bytes());
    Ok(backups.join(format!(
        "{}.appdata",
        digest.as_str().trim_start_matches("blake3:")
    )))
}

fn ensure_backup_directory(path: &Path) -> Result<(), AppUpdateCoordinatorError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(AppUpdateCoordinatorError::BackupIo(format!(
                "unsafe backup directory {}",
                path.display()
            )));
        },
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
                    AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
                })?;
            }
        },
        Err(error) => {
            return Err(AppUpdateCoordinatorError::BackupIo(format!(
                "{}: {error}",
                path.display()
            )));
        },
    }
    Ok(())
}

fn read_or_create_encrypted_backup(
    path: &Path,
    logical: &AppLogicalArchive,
    plan: &super::portability::AppArchiveWritePlan,
    passphrase: &AppArchivePassphrase,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<super::portable_archive_transfer::DecodedAppPortableArchive, AppUpdateCoordinatorError>
{
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AppUpdateCoordinatorError::BackupIo(format!(
                "unsafe backup file {}",
                path.display()
            )));
        }
        let bytes = fs::read(path).map_err(|error| {
            AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
        })?;
        return Ok(decode_app_portable_archive(&bytes, Some(passphrase))?);
    }

    let encoded = encode_app_portable_archive(
        logical.clone(),
        None,
        plan,
        Some(passphrase),
        authenticated,
        now,
    )?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(&encoded.bytes).map_err(|error| {
                AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
            })?;
            file.sync_all().map_err(|error| {
                AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
            })?;
            drop(file);
            let bytes = fs::read(path).map_err(|error| {
                AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
            })?;
            Ok(decode_app_portable_archive(&bytes, Some(passphrase))?)
        },
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let bytes = fs::read(path).map_err(|error| {
                AppUpdateCoordinatorError::BackupIo(format!("{}: {error}", path.display()))
            })?;
            Ok(decode_app_portable_archive(&bytes, Some(passphrase))?)
        },
        Err(error) => Err(AppUpdateCoordinatorError::BackupIo(format!(
            "{}: {error}",
            path.display()
        ))),
    }
}

fn update_backup_reference(
    migration_run_id: &AppReference,
    source_fence_digest: &AppDigest,
    update_plan_digest: &AppDigest,
    ciphertext_digest: &AppDigest,
) -> Result<AppReference, AppUpdateCoordinatorError> {
    let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-update-backup.v1",
        "migration_run_id": migration_run_id,
        "source_fence_digest": source_fence_digest,
        "update_plan_digest": update_plan_digest,
        "ciphertext_digest": ciphertext_digest,
    }))?;
    Ok(AppReference::parse(format!(
        "backup:app-update:{}",
        digest.as_str().trim_start_matches("blake3:")
    ))?)
}

fn record_update_backup_blocking(
    connection: &mut rusqlite::Connection,
    scope: &super::records::AppScope,
    migration_run_id: &AppReference,
    receipt: AppUpdateBackupReceipt,
    now: DateTime<Utc>,
) -> Result<AppDurableUpdateRun, AppUpdateCoordinatorError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut run = load_update_run(&transaction, migration_run_id)?
        .ok_or_else(|| AppUpdateCoordinatorError::StaleCandidate("migration run missing".into()))?;
    if receipt.source_fence_digest != run.source_fence.fence_digest
        || receipt.update_plan_digest != run.update_plan_digest
        || !receipt.encrypted
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "backup receipt is not bound to the reviewed source and plan".into(),
        ));
    }
    if let Some(existing) = &run.backup_receipt {
        if existing != &receipt {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "backup receipt replay changed".into(),
            ));
        }
        return Ok(run);
    }
    if run.state != AppUpdateCoordinatorState::DryRunPassed || !run.backup_required() {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "migration run is not waiting for its backup".into(),
        ));
    }
    let current_fence = super::registry::reviewable_revision_source_fence_blocking(
        &transaction,
        scope,
        &run.installation_id,
        run.attempt_kind,
    )?;
    if current_fence != run.source_fence {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "source moved while the backup was written".into(),
        ));
    }
    run.backup_receipt = Some(receipt);
    run.state = AppUpdateCoordinatorState::BackupRecorded;
    run.updated_at = now;
    replace_update_run(&transaction, &run, AppUpdateCoordinatorState::DryRunPassed)?;
    transaction.commit()?;
    Ok(run)
}

fn stage_backed_update_blocking(
    connection: &mut rusqlite::Connection,
    scope: &super::records::AppScope,
    migration_run_id: &AppReference,
    now: DateTime<Utc>,
) -> Result<AppDurableUpdateRun, AppUpdateCoordinatorError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut run = load_update_run(&transaction, migration_run_id)?
        .ok_or_else(|| AppUpdateCoordinatorError::StaleCandidate("migration run missing".into()))?;
    if run.state == AppUpdateCoordinatorState::ReadyToSwitch {
        return Ok(run);
    }
    if run.state != AppUpdateCoordinatorState::BackupRecorded
        || run.backup_receipt.as_ref().is_none_or(|receipt| {
            !receipt.encrypted
                || receipt.source_fence_digest != run.source_fence.fence_digest
                || receipt.update_plan_digest != run.update_plan_digest
        })
    {
        return Err(AppUpdateCoordinatorError::BackupRequired);
    }
    let current_fence = super::registry::reviewable_revision_source_fence_blocking(
        &transaction,
        scope,
        &run.installation_id,
        run.attempt_kind,
    )?;
    if current_fence != run.source_fence {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "source moved before backed staging".into(),
        ));
    }
    let staged = collect_staged_records(
        &transaction,
        &run.installation_id,
        run.migration_plan.as_ref(),
        &run.destination_schema_preview,
    )?;
    if u64::try_from(staged.len()).unwrap_or(u64::MAX) != run.dry_run_examined {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "source record set changed after dry-run".into(),
        ));
    }
    insert_staged_records(&transaction, &run, &staged, now)?;
    run.state = AppUpdateCoordinatorState::ReadyToSwitch;
    run.updated_at = now;
    replace_update_run(
        &transaction,
        &run,
        AppUpdateCoordinatorState::BackupRecorded,
    )?;
    transaction.commit()?;
    Ok(run)
}

fn abort_update_before_switch_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    now: DateTime<Utc>,
) -> Result<Option<AppDurableUpdateRun>, AppUpdateCoordinatorError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut statement = transaction.prepare(
        "SELECT record_json FROM app_migration_runs
         WHERE installation_id = ?1
           AND state IN ('dry_run_passed', 'backup_recorded', 'ready_to_switch')
         ORDER BY updated_at DESC LIMIT 2",
    )?;
    let rows = statement
        .query_map(params![installation_id.as_str()], |row| {
            row.get::<_, Vec<u8>>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if rows.is_empty() {
        transaction.commit()?;
        return Ok(None);
    }
    if rows.len() != 1 {
        return Err(AppUpdateCoordinatorError::Corrupt(
            "multiple active migration runs exist for one installation".into(),
        ));
    }
    let mut run: AppDurableUpdateRun = serde_json::from_slice(&rows[0])
        .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
    let current_fence = super::registry::reviewable_revision_source_fence_blocking(
        &transaction,
        scope,
        installation_id,
        run.attempt_kind,
    )?;
    if current_fence.installation_id != run.source_fence.installation_id
        || current_fence.parked_lifecycle_generation != run.source_fence.parked_lifecycle_generation
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "update abort target moved".into(),
        ));
    }
    transaction.execute(
        "DELETE FROM app_migration_staged_records WHERE migration_run_id = ?1",
        params![run.migration_run_id.as_str()],
    )?;
    let expected = run.state;
    run.state = AppUpdateCoordinatorState::Aborted;
    run.updated_at = now;
    replace_update_run(&transaction, &run, expected)?;
    transaction.commit()?;
    Ok(Some(run))
}

fn rollback_code_only_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    authenticated: &AuthenticatedAppScope,
    request: AppCodeOnlyRollbackRequest,
    now: DateTime<Utc>,
) -> Result<AppUpdateRollbackReceipt, AppUpdateCoordinatorError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut run = load_update_run(&transaction, &request.migration_run_id)?
        .ok_or_else(|| AppUpdateCoordinatorError::StaleCandidate("migration run missing".into()))?;
    if run.state == AppUpdateCoordinatorState::RolledBack {
        let receipt = run.rollback_receipt.ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("rolled-back run has no receipt".into())
        })?;
        if receipt.request_id != request.request_id
            || receipt.installation_generation
                != request.expected_installation_generation.saturating_add(1)
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "rollback replay identity changed".into(),
            ));
        }
        return Ok(receipt);
    }
    if run.state != AppUpdateCoordinatorState::Switched || run.migration_plan.is_some() {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "code-only rollback is available only for a switched code-only update".into(),
        ));
    }
    let installation =
        super::registry_lifecycle::load_installation(&transaction, &run.installation_id)?;
    if installation.scope != *scope
        || installation.lifecycle.generation != request.expected_installation_generation
        || !matches!(
            installation.lifecycle.status,
            AppInstallationStatus::Enabled | AppInstallationStatus::Disabled
        )
        || installation.package_revision_ref != run.destination_package_revision_ref
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "active installation is not the exact switched target".into(),
        ));
    }
    let current_grant_revision = installation.grant_revision.ok_or_else(|| {
        AppUpdateCoordinatorError::Corrupt("switched installation has no active grant".into())
    })?;
    let current_schema_revision = installation.active_schema_revision.ok_or_else(|| {
        AppUpdateCoordinatorError::Corrupt("switched installation has no active schema".into())
    })?;
    let current_grant: AppGrantRevision = load_revision_contract(
        &transaction,
        "app_grant_revisions",
        &installation.installation_id,
        current_grant_revision,
    )?;
    let current_schema: AppSchemaRevision = load_revision_contract(
        &transaction,
        "app_schema_revisions",
        &installation.installation_id,
        current_schema_revision,
    )?;
    if current_grant.revoked_at.is_some()
        || current_grant.package_revision_ref != run.destination_package_revision_ref
        || current_schema.package_revision_ref != run.destination_package_revision_ref
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "current grant/schema no longer match the switched target".into(),
        ));
    }
    let source_surface_exists: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM app_surface_generations
              WHERE installation_id = ?1 AND revision = ?2
                AND package_revision_ref = ?3
             UNION ALL
             SELECT 1 FROM app_surface_bindings
              WHERE installation_id = ?1 AND revision = ?2
                AND package_revision_ref = ?3 AND status = 'active'
         )",
        params![
            installation.installation_id.as_str(),
            i64::try_from(run.source_fence.source_surface_revision.get()).unwrap_or(i64::MAX),
            run.source_fence.source_package_revision_ref.as_str(),
        ],
        |row| row.get(0),
    )?;
    if !source_surface_exists {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "previous code surface generation is unavailable".into(),
        ));
    }
    let next_grant_revision =
        AppRevision::new(current_grant_revision.get().checked_add(1).ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("grant revision exhausted".into())
        })?)?;
    let next_schema_revision = AppRevision::new(
        current_schema_revision
            .get()
            .checked_add(1)
            .ok_or_else(|| {
                AppUpdateCoordinatorError::Corrupt("schema revision exhausted".into())
            })?,
    )?;
    let mut rollback_grant = current_grant;
    rollback_grant.revision = next_grant_revision;
    rollback_grant.package_revision_ref = run.source_fence.source_package_revision_ref.clone();
    rollback_grant.approved_by = authenticated.actor_ref().clone();
    rollback_grant.approved_at = now;
    let mut rollback_schema = current_schema;
    rollback_schema.revision = next_schema_revision;
    rollback_schema.package_revision_ref = run.source_fence.source_package_revision_ref.clone();
    rollback_schema.compatibility_with_previous = AppSchemaCompatibility::Compatible;
    rollback_schema.migration_plan_ref = None;
    rollback_schema.created_at = now;
    rollback_grant.validate_app_contract(&AppContractLimits::default())?;
    rollback_schema.validate_app_contract(&AppContractLimits::default())?;

    transaction.execute(
        "INSERT INTO app_grant_revisions (
             installation_id, revision, package_revision_ref, authority_digest,
             granted_data_policy_digest, revoked_at, record_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7)",
        params![
            rollback_grant.installation_id.as_str(),
            i64::try_from(rollback_grant.revision.get()).unwrap_or(i64::MAX),
            rollback_grant.package_revision_ref.as_str(),
            rollback_grant.authority_digest.as_str(),
            rollback_grant.granted_data_handling_policy_digest.as_str(),
            encode_bounded_json(&rollback_grant, &AppContractLimits::default())?,
            format_timestamp(&now),
        ],
    )?;
    transaction.execute(
        "INSERT INTO app_schema_revisions (
             installation_id, revision, package_revision_ref, record_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            rollback_schema.installation_id.as_str(),
            i64::try_from(rollback_schema.revision.get()).unwrap_or(i64::MAX),
            rollback_schema.package_revision_ref.as_str(),
            encode_bounded_json(&rollback_schema, &AppContractLimits::default())?,
            format_timestamp(&now),
        ],
    )?;

    let next_generation = installation
        .lifecycle
        .generation
        .checked_add(1)
        .ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("installation generation exhausted".into())
        })?;
    let rollback_ref = update_rollback_reference(
        &request.migration_run_id,
        &request.request_id,
        AppRollbackKind::CodeOnly,
        next_generation,
    )?;
    let receipt = AppUpdateRollbackReceipt {
        rollback_ref: rollback_ref.clone(),
        request_id: request.request_id,
        kind: AppRollbackKind::CodeOnly,
        migration_run_id: run.migration_run_id.clone(),
        installation_id: installation.installation_id.clone(),
        installation_generation: next_generation,
        package_revision_ref: run.source_fence.source_package_revision_ref.clone(),
        grant_revision: rollback_grant.revision,
        schema_revision: rollback_schema.revision,
        surface_revision: run.source_fence.source_surface_revision,
        post_update_writes_retained: true,
        grants_restored: false,
        rolled_back_at: now,
    };
    let mut next_installation = installation.clone();
    next_installation.package_revision_ref = receipt.package_revision_ref.clone();
    next_installation.lifecycle.generation = next_generation;
    next_installation.grant_revision = Some(receipt.grant_revision);
    next_installation.active_schema_revision = Some(receipt.schema_revision);
    next_installation.active_surface_revision = Some(receipt.surface_revision);
    next_installation.updated_at = now;
    next_installation.validate_app_contract(&AppContractLimits::default())?;
    update_installation_cas(&transaction, &installation, &next_installation)?;
    let event = lifecycle_event(
        AppReference::parse(format!(
            "event:{}",
            rollback_ref.as_str().trim_start_matches("rollback:")
        ))?,
        &next_installation,
        AppLifecycleEventKind::InstallationRolledBack,
        now,
    );
    let idempotency_key = AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-code-rollback.v1",
        "scope_binding_ref": authenticated.scope_binding_ref(),
        "request_id": receipt.request_id,
        "rollback_ref": receipt.rollback_ref,
    }))?;
    insert_outbox_event(&transaction, &event, &idempotency_key, &now)?;
    run.state = AppUpdateCoordinatorState::RolledBack;
    run.rollback_receipt = Some(receipt.clone());
    run.updated_at = now;
    replace_update_run(&transaction, &run, AppUpdateCoordinatorState::Switched)?;
    transaction.commit()?;
    Ok(receipt)
}

fn stage_data_rewind_review_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    migration_run_id: &AppReference,
    preview: &AppDataImportPreview,
    transformed_source_digest: AppDigest,
    now: DateTime<Utc>,
) -> Result<AppDurableUpdateRun, AppUpdateCoordinatorError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut run = load_update_run(&transaction, migration_run_id)?
        .ok_or_else(|| AppUpdateCoordinatorError::StaleCandidate("migration run missing".into()))?;
    let installation =
        super::registry_lifecycle::load_installation(&transaction, &run.installation_id)?;
    if installation.scope != *scope
        || installation.lifecycle.status != AppInstallationStatus::Enabled
        || installation.lifecycle.generation != preview.destination_installation_generation
        || installation.package_revision_ref != run.destination_package_revision_ref
        || preview.destination_installation_id != installation.installation_id
        || preview.source_archive_digest != transformed_source_digest
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "rewind preview destination changed before staging".into(),
        ));
    }
    let high_water: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(change_seq), 0) FROM app_record_heads
         WHERE installation_id = ?1",
        params![installation.installation_id.as_str()],
        |row| row.get(0),
    )?;
    let high_water = u64::try_from(high_water)
        .map_err(|_| AppUpdateCoordinatorError::Corrupt("negative rewind high-water".into()))?;
    let schema_revision = installation.active_schema_revision.ok_or_else(|| {
        AppUpdateCoordinatorError::Corrupt("rewind destination schema missing".into())
    })?;
    let schema: AppSchemaRevision = load_revision_contract(
        &transaction,
        "app_schema_revisions",
        &installation.installation_id,
        schema_revision,
    )?;
    let staged =
        collect_staged_records(&transaction, &installation.installation_id, None, &schema)?;
    let fence = AppDataRewindReviewFence {
        installation_generation: installation.lifecycle.generation,
        source_change_seq_high_water: high_water,
        staged_head_count: u64::try_from(staged.len()).unwrap_or(u64::MAX),
        transformed_source_digest,
        preview_digest: preview.preview_digest.clone(),
        record_decisions: preview.record_decisions.clone(),
    };
    if run.state == AppUpdateCoordinatorState::RewindReviewPending {
        if run.rewind_review.as_ref() != Some(&fence) {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "rewind preview replay changed".into(),
            ));
        }
        return Ok(run);
    }
    if run.state != AppUpdateCoordinatorState::Switched || run.migration_plan.is_none() {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "migration run cannot enter data-rewind review".into(),
        ));
    }
    insert_staged_records(&transaction, &run, &staged, now)?;
    run.rewind_review = Some(fence);
    run.state = AppUpdateCoordinatorState::RewindReviewPending;
    run.updated_at = now;
    replace_update_run(&transaction, &run, AppUpdateCoordinatorState::Switched)?;
    transaction.commit()?;
    Ok(run)
}

#[allow(clippy::too_many_arguments)]
fn finalize_data_rewind_blocking(
    connection: &mut rusqlite::Connection,
    scope: &AppScope,
    authenticated: &AuthenticatedAppScope,
    request: AppDataRewindCommitRequest,
    import_receipt: AppDataImportReceipt,
    record_decisions: Vec<AppDataImportRecordDecision>,
    now: DateTime<Utc>,
) -> Result<AppDataRewindCommitReceipt, AppUpdateCoordinatorError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut run = load_update_run(&transaction, &request.migration_run_id)?
        .ok_or_else(|| AppUpdateCoordinatorError::StaleCandidate("migration run missing".into()))?;
    let review = run
        .rewind_review
        .clone()
        .ok_or_else(|| AppUpdateCoordinatorError::Corrupt("rewind review fence missing".into()))?;
    if run.state != AppUpdateCoordinatorState::RewindReviewPending
        || review.preview_digest != request.preview_digest
        || review.record_decisions != record_decisions
        || import_receipt.preview_digest != review.preview_digest
        || import_receipt.destination_installation_id != run.installation_id
        || import_receipt.destination_installation_generation != review.installation_generation
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "data-rewind import does not match the reviewed fence".into(),
        ));
    }
    let installation =
        super::registry_lifecycle::load_installation(&transaction, &run.installation_id)?;
    if installation.scope != *scope
        || installation.lifecycle.status != AppInstallationStatus::Enabled
        || installation.lifecycle.generation != review.installation_generation
        || installation.package_revision_ref != run.destination_package_revision_ref
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "data-rewind installation changed after import".into(),
        ));
    }
    let expected_high_water = review
        .source_change_seq_high_water
        .checked_add(u64::from(import_receipt.created_count))
        .ok_or_else(|| AppUpdateCoordinatorError::Corrupt("rewind high-water exhausted".into()))?;
    let current_high_water: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(change_seq), 0) FROM app_record_heads
         WHERE installation_id = ?1",
        params![installation.installation_id.as_str()],
        |row| row.get(0),
    )?;
    if u64::try_from(current_high_water).ok() != Some(expected_high_water) {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "writes raced with the reviewed data rewind".into(),
        ));
    }
    let staged = load_staged_records(&transaction, &run)?;
    if u64::try_from(staged.len()).ok() != Some(review.staged_head_count) {
        return Err(AppUpdateCoordinatorError::Corrupt(
            "rewind source-head staging is incomplete".into(),
        ));
    }
    let active = resolve_active_schema(&transaction, scope, &installation.installation_id)?
        .ok_or_else(|| {
            AppUpdateCoordinatorError::StaleCandidate("rewind destination is not active".into())
        })?;
    let mut tombstones = BTreeMap::new();
    for record in staged {
        let current: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT record_revision, change_seq FROM app_record_heads
                 WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
                params![
                    installation.installation_id.as_str(),
                    record.entity.as_str(),
                    record.record_id.as_str(),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if current
            != Some((
                i64::try_from(record.source_record_revision.get()).unwrap_or(i64::MAX),
                i64::try_from(record.source_change_seq).unwrap_or(i64::MAX),
            ))
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(format!(
                "rewind source head changed for {}/{}",
                record.entity, record.record_id
            )));
        }
        if record.deleted_at.is_none() {
            tombstones.insert(
                (record.entity.clone(), record.record_id.clone()),
                WorkingRecord {
                    entity: record.entity,
                    record_id: record.record_id,
                    prior_revision: Some(record.source_record_revision),
                    created_at: record.created_at,
                    payload: record.payload,
                    handling_policy: record.handling_policy,
                    was_deleted: false,
                    deleted: true,
                },
            );
        }
    }
    if !tombstones.is_empty() {
        let usage = project_storage_usage(&transaction, &active, &tombstones)?;
        let dataset_generation =
            advance_dataset_generation(&transaction, &installation.installation_id, now)?;
        let first_change_seq = reserve_change_sequences(
            &transaction,
            &installation.installation_id,
            tombstones.len(),
        )?;
        let provenance = AppRecordProvenance {
            actor_kind: AppRecordActorKind::Migration,
            actor_id: run.migration_run_id.clone(),
            execution_id: Some(request.request_id.clone()),
            output_revision: installation.active_schema_revision,
            mutation_receipt_id: Some(import_receipt.receipt_ref.clone()),
            source_artifact_refs: run
                .backup_receipt
                .as_ref()
                .map(|receipt| vec![receipt.backup_ref.clone()])
                .unwrap_or_default(),
            citation_refs: Vec::new(),
        };
        for (offset, record) in tombstones.values().enumerate() {
            let revision = next_revision(record.prior_revision)?;
            let change_seq = first_change_seq
                .checked_add(u64::try_from(offset).unwrap_or(u64::MAX))
                .ok_or_else(|| {
                    AppUpdateCoordinatorError::Corrupt("rewind sequence exhausted".into())
                })?;
            write_record_revision(
                &transaction,
                &active,
                record,
                revision,
                dataset_generation,
                change_seq,
                &provenance,
                now,
            )?;
        }
        persist_storage_usage(&transaction, &installation.installation_id, usage, now)?;
    }
    let next_generation = installation
        .lifecycle
        .generation
        .checked_add(1)
        .ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("installation generation exhausted".into())
        })?;
    let rollback_ref = update_rollback_reference(
        &run.migration_run_id,
        &request.request_id,
        AppRollbackKind::DataRewind,
        next_generation,
    )?;
    let rollback = AppUpdateRollbackReceipt {
        rollback_ref: rollback_ref.clone(),
        request_id: request.request_id,
        kind: AppRollbackKind::DataRewind,
        migration_run_id: run.migration_run_id.clone(),
        installation_id: installation.installation_id.clone(),
        installation_generation: next_generation,
        package_revision_ref: installation.package_revision_ref.clone(),
        grant_revision: installation.grant_revision.ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("rewind grant revision missing".into())
        })?,
        schema_revision: installation.active_schema_revision.ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("rewind schema revision missing".into())
        })?,
        surface_revision: installation.active_surface_revision.ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("rewind surface revision missing".into())
        })?,
        post_update_writes_retained: false,
        grants_restored: false,
        rolled_back_at: now,
    };
    let mut next_installation = installation.clone();
    next_installation.lifecycle.generation = next_generation;
    next_installation.updated_at = now;
    next_installation.validate_app_contract(&AppContractLimits::default())?;
    update_installation_cas(&transaction, &installation, &next_installation)?;
    let event = lifecycle_event(
        AppReference::parse(format!(
            "event:{}",
            rollback_ref.as_str().trim_start_matches("rollback:")
        ))?,
        &next_installation,
        AppLifecycleEventKind::InstallationRolledBack,
        now,
    );
    let idempotency_key = AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-data-rewind.v1",
        "scope_binding_ref": authenticated.scope_binding_ref(),
        "request_id": rollback.request_id,
        "rollback_ref": rollback.rollback_ref,
        "import_receipt_ref": import_receipt.receipt_ref,
    }))?;
    insert_outbox_event(&transaction, &event, &idempotency_key, &now)?;
    transaction.execute(
        "DELETE FROM app_migration_staged_records WHERE migration_run_id = ?1",
        params![run.migration_run_id.as_str()],
    )?;
    run.state = AppUpdateCoordinatorState::RolledBack;
    run.rollback_receipt = Some(rollback.clone());
    run.updated_at = now;
    replace_update_run(
        &transaction,
        &run,
        AppUpdateCoordinatorState::RewindReviewPending,
    )?;
    transaction.commit()?;
    Ok(AppDataRewindCommitReceipt {
        rollback,
        import_receipt,
        record_decisions,
    })
}

fn load_revision_contract<T>(
    connection: &rusqlite::Connection,
    table: &'static str,
    installation_id: &AppInstallationId,
    revision: AppRevision,
) -> Result<T, AppUpdateCoordinatorError>
where
    T: serde::de::DeserializeOwned + ValidateAppContract,
{
    let sql = match table {
        "app_grant_revisions" => {
            "SELECT record_json FROM app_grant_revisions
             WHERE installation_id = ?1 AND revision = ?2"
        },
        "app_schema_revisions" => {
            "SELECT record_json FROM app_schema_revisions
             WHERE installation_id = ?1 AND revision = ?2"
        },
        _ => {
            return Err(AppUpdateCoordinatorError::Corrupt(
                "unsupported rollback revision table".into(),
            ));
        },
    };
    let bytes: Vec<u8> = connection
        .query_row(
            sql,
            params![
                installation_id.as_str(),
                i64::try_from(revision.get()).unwrap_or(i64::MAX)
            ],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            AppUpdateCoordinatorError::StaleCandidate(format!(
                "rollback source revision missing from {table}"
            ))
        })?;
    Ok(decode_app_contract(&bytes, &AppContractLimits::default())?)
}

fn update_rollback_reference(
    migration_run_id: &AppReference,
    request_id: &AppReference,
    kind: AppRollbackKind,
    destination_generation: u64,
) -> Result<AppReference, AppUpdateCoordinatorError> {
    let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-rollback.v1",
        "migration_run_id": migration_run_id,
        "request_id": request_id,
        "kind": kind,
        "destination_generation": destination_generation,
    }))?;
    Ok(AppReference::parse(format!(
        "rollback:{}",
        digest.as_str().trim_start_matches("blake3:")
    ))?)
}

#[allow(clippy::too_many_arguments)]
fn stage_update_plan_blocking(
    connection: &mut rusqlite::Connection,
    scope: &super::records::AppScope,
    migration_run_id: AppReference,
    installation_id: AppInstallationId,
    attempt_id: AppReference,
    attempt_kind: AppLifecycleAttemptKind,
    source_fence: AppReviewableRevisionSourceFence,
    destination_package_revision_ref: AppReference,
    destination_schema_revision: AppRevision,
    permission_diff: AppPermissionDiff,
    schema_diff_digest: AppDigest,
    surface_diff_digest: AppDigest,
    data_diff_digest: AppDigest,
    migration_plan: Option<AppCompiledMigrationPlan>,
    update_plan_digest: AppDigest,
    destructive: bool,
    target_schema: AppSchemaRevision,
    now: DateTime<Utc>,
) -> Result<AppDurableUpdateRun, AppUpdateCoordinatorError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(mut existing) = load_update_run(&transaction, &migration_run_id)? {
        if existing.update_plan_digest != update_plan_digest
            || existing.source_fence != source_fence
            || existing.destination_package_revision_ref != destination_package_revision_ref
            || existing.destination_schema_preview.canonical_entity_schema
                != target_schema.canonical_entity_schema
        {
            return Err(AppUpdateCoordinatorError::Corrupt(
                "migration run identity replay changed".into(),
            ));
        }
        // An empty processing transition has no data to export. Resume an
        // exact dry-run replay left waiting by an older backup requirement,
        // after rechecking the source fence and the actual empty head set.
        if existing.state == AppUpdateCoordinatorState::DryRunPassed
            && existing.dry_run_examined == 0
            && !existing.backup_required()
        {
            let current_fence = super::registry::reviewable_revision_source_fence_blocking(
                &transaction,
                scope,
                &installation_id,
                attempt_kind,
            )?;
            let head_count: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM app_record_heads WHERE installation_id = ?1",
                params![installation_id.as_str()],
                |row| row.get(0),
            )?;
            if current_fence != source_fence || head_count != 0 {
                return Err(AppUpdateCoordinatorError::StaleCandidate(
                    "source records changed after the empty update dry-run".into(),
                ));
            }
            existing.state = AppUpdateCoordinatorState::ReadyToSwitch;
            existing.updated_at = now;
            replace_update_run(
                &transaction,
                &existing,
                AppUpdateCoordinatorState::DryRunPassed,
            )?;
            transaction.commit()?;
        }
        return Ok(existing);
    }
    let current_fence = super::registry::reviewable_revision_source_fence_blocking(
        &transaction,
        scope,
        &installation_id,
        attempt_kind,
    )?;
    if current_fence != source_fence {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "source fence moved before migration staging".into(),
        ));
    }
    let current_dataset_generation: i64 = transaction.query_row(
        "SELECT COALESCE((SELECT current_generation FROM app_dataset_generations
          WHERE installation_id = ?1), 0)",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    let current_dataset_generation = u64::try_from(current_dataset_generation)
        .map_err(|_| AppUpdateCoordinatorError::Corrupt("negative dataset generation".into()))?;
    let destination_dataset_generation = if migration_plan.is_some() {
        current_dataset_generation
            .checked_add(1)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                AppUpdateCoordinatorError::Corrupt("dataset generation exhausted".into())
            })?
    } else {
        current_dataset_generation
    };
    let staged_records = collect_staged_records(
        &transaction,
        &installation_id,
        migration_plan.as_ref(),
        &target_schema,
    )?;
    let examined = u64::try_from(staged_records.len()).unwrap_or(u64::MAX);
    let mut run = AppDurableUpdateRun {
        migration_run_id,
        installation_id,
        attempt_id,
        attempt_kind,
        source_fence,
        destination_package_revision_ref,
        destination_schema_revision,
        destination_schema_preview: target_schema.clone(),
        destination_dataset_generation,
        permission_diff,
        schema_diff_digest,
        surface_diff_digest,
        data_diff_digest,
        migration_plan,
        update_plan_digest,
        destructive,
        dry_run_examined: examined,
        dry_run_representable: examined,
        backup_receipt: None,
        rollback_receipt: None,
        rewind_review: None,
        state: AppUpdateCoordinatorState::DryRunPassed,
        created_at: now,
        updated_at: now,
    };
    persist_update_run(&transaction, &run)?;
    if run.backup_required() {
        transaction.commit()?;
        return Ok(run);
    }
    if run.migration_plan.is_some() {
        insert_staged_records(&transaction, &run, &staged_records, now)?;
    }
    run.state = AppUpdateCoordinatorState::ReadyToSwitch;
    run.updated_at = now;
    replace_update_run(&transaction, &run, AppUpdateCoordinatorState::DryRunPassed)?;
    transaction.commit()?;
    Ok(run)
}

fn collect_staged_records(
    transaction: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    migration_plan: Option<&AppCompiledMigrationPlan>,
    target_schema: &AppSchemaRevision,
) -> Result<Vec<AppStagedMigrationRecord>, AppUpdateCoordinatorError> {
    let runtime = runtime_contracts_from_revision(target_schema)?;
    let mut statement = transaction.prepare(
        "SELECT h.entity_name, h.record_id, h.record_revision, h.change_seq,
                r.payload_json, r.handling_policy_json, r.created_at, r.deleted_at
           FROM app_record_heads h
           JOIN app_record_revisions r
             ON r.installation_id = h.installation_id
            AND r.entity_name = h.entity_name
            AND r.record_id = h.record_id
            AND r.record_revision = h.record_revision
          WHERE h.installation_id = ?1
          ORDER BY h.change_seq, h.entity_name, h.record_id",
    )?;
    let rows = statement.query_map(params![installation_id.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, Vec<u8>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;
    let mut staged_records = Vec::new();
    for row in rows {
        if u64::try_from(staged_records.len()).unwrap_or(u64::MAX) >= APP_UPDATE_PLAN_RECORD_CEILING
        {
            return Err(AppMigrationError::TailCeilingExceeded.into());
        }
        let (entity, record_id, revision, change_seq, payload, policy, created_at, deleted_at) =
            row?;
        let entity = AppName::parse(entity)?;
        let record_id = AppRecordId::parse(record_id)?;
        let source_record_revision =
            AppRevision::new(u64::try_from(revision).map_err(|_| {
                AppUpdateCoordinatorError::Corrupt("negative record revision".into())
            })?)?;
        let source_change_seq = u64::try_from(change_seq)
            .map_err(|_| AppUpdateCoordinatorError::Corrupt("negative change sequence".into()))?;
        let payload = decode_bounded_json_value(&payload, &AppContractLimits::default())?;
        let payload = match migration_plan {
            Some(plan) => apply_migration_to_payload(plan, &entity, &payload)?,
            None => payload,
        };
        let entity_runtime = runtime.get(&entity).ok_or_else(|| {
            AppUpdateCoordinatorError::DestinationMismatch(format!(
                "destination removed entity `{entity}` without a total mapping"
            ))
        })?;
        entity_runtime.validate_payload(&payload).map_err(|error| {
            AppUpdateCoordinatorError::DryRunFailed(format!("{entity}/{record_id}: {error}"))
        })?;
        let policy_value = decode_bounded_json_value(&policy, &AppContractLimits::default())?;
        let handling_policy: AppDataHandlingPolicy = serde_json::from_value(policy_value)
            .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
        let handling_policy = migration_plan
            .map(|plan| apply_migration_to_handling_policy(plan, &entity, &handling_policy))
            .unwrap_or(handling_policy);
        validate_policy(&handling_policy, &AppContractLimits::default())?;
        let created_at = DateTime::parse_from_rfc3339(&created_at)
            .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?
            .with_timezone(&Utc);
        let deleted_at = deleted_at
            .map(|value| DateTime::parse_from_rfc3339(&value).map(|time| time.with_timezone(&Utc)))
            .transpose()
            .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
        staged_records.push(AppStagedMigrationRecord {
            entity,
            record_id,
            source_record_revision,
            source_change_seq,
            payload,
            handling_policy,
            created_at,
            deleted_at,
        });
    }
    drop(statement);
    Ok(staged_records)
}

fn insert_staged_records(
    transaction: &rusqlite::Transaction<'_>,
    run: &AppDurableUpdateRun,
    staged_records: &[AppStagedMigrationRecord],
    now: DateTime<Utc>,
) -> Result<(), AppUpdateCoordinatorError> {
    for record in staged_records {
        let bytes = serde_json::to_vec(record)
            .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
        let payload_digest = AppDigest::blake3_canonical_json(&record.payload)?;
        transaction.execute(
            "INSERT INTO app_migration_staged_records (
                 migration_run_id, installation_id, entity_name, record_id,
                 source_record_revision, source_change_seq, target_dataset_generation,
                 target_schema_revision, payload_digest, record_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run.migration_run_id.as_str(),
                run.installation_id.as_str(),
                record.entity.as_str(),
                record.record_id.as_str(),
                i64::try_from(record.source_record_revision.get()).unwrap_or(i64::MAX),
                i64::try_from(record.source_change_seq).unwrap_or(i64::MAX),
                i64::try_from(run.destination_dataset_generation).unwrap_or(i64::MAX),
                i64::try_from(run.destination_schema_revision.get()).unwrap_or(i64::MAX),
                payload_digest.as_str(),
                bytes,
                now.to_rfc3339(),
            ],
        )?;
    }
    Ok(())
}

fn persist_update_run(
    transaction: &rusqlite::Transaction<'_>,
    run: &AppDurableUpdateRun,
) -> Result<(), AppUpdateCoordinatorError> {
    let bytes = serde_json::to_vec(run)
        .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
    transaction.execute(
        "INSERT INTO app_migration_runs (
             migration_run_id, installation_id, source_schema_revision,
             target_schema_revision, state, next_batch, record_json,
             created_at, updated_at, plan_digest, source_generation,
             destination_generation
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            run.migration_run_id.as_str(),
            run.installation_id.as_str(),
            i64::try_from(run.source_fence.source_schema_revision.get()).unwrap_or(i64::MAX),
            i64::try_from(run.destination_schema_revision.get()).unwrap_or(i64::MAX),
            update_state_label(run.state),
            bytes,
            run.created_at.to_rfc3339(),
            run.updated_at.to_rfc3339(),
            run.update_plan_digest.as_str(),
            i64::try_from(run.source_fence.source_installation_generation).unwrap_or(i64::MAX),
            i64::try_from(run.destination_dataset_generation).unwrap_or(i64::MAX),
        ],
    )?;
    Ok(())
}

fn replace_update_run(
    transaction: &rusqlite::Transaction<'_>,
    run: &AppDurableUpdateRun,
    expected: AppUpdateCoordinatorState,
) -> Result<(), AppUpdateCoordinatorError> {
    let bytes = serde_json::to_vec(run)
        .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
    let changed = transaction.execute(
        "UPDATE app_migration_runs SET state = ?1, record_json = ?2, updated_at = ?3
         WHERE migration_run_id = ?4 AND state = ?5 AND plan_digest = ?6",
        params![
            update_state_label(run.state),
            bytes,
            run.updated_at.to_rfc3339(),
            run.migration_run_id.as_str(),
            update_state_label(expected),
            run.update_plan_digest.as_str(),
        ],
    )?;
    if changed != 1 {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "update coordinator state changed".into(),
        ));
    }
    Ok(())
}

fn load_update_run(
    connection: &rusqlite::Connection,
    migration_run_id: &AppReference,
) -> Result<Option<AppDurableUpdateRun>, AppUpdateCoordinatorError> {
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT record_json FROM app_migration_runs WHERE migration_run_id = ?1",
            params![migration_run_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| {
            let run: AppDurableUpdateRun = serde_json::from_slice(&bytes)
                .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
            if &run.migration_run_id != migration_run_id {
                return Err(AppUpdateCoordinatorError::Corrupt(
                    "migration-run key and body differ".into(),
                ));
            }
            Ok(run)
        })
        .transpose()
}

fn update_state_label(state: AppUpdateCoordinatorState) -> &'static str {
    match state {
        AppUpdateCoordinatorState::DryRunPassed => "dry_run_passed",
        AppUpdateCoordinatorState::BackupRecorded => "backup_recorded",
        AppUpdateCoordinatorState::ReadyToSwitch => "ready_to_switch",
        AppUpdateCoordinatorState::Switched => "switched",
        AppUpdateCoordinatorState::RewindReviewPending => "rewind_review_pending",
        AppUpdateCoordinatorState::Aborted => "aborted",
        AppUpdateCoordinatorState::RolledBack => "rolled_back",
    }
}

fn update_plan_reference(
    prefix: &str,
    attempt_id: &AppReference,
    source_schema_digest: &AppDigest,
    destination_schema_digest: &AppDigest,
    operations: &[AppMigrationOperation],
) -> Result<AppReference, AppUpdateCoordinatorError> {
    let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-migration-plan.v1",
        "attempt_id": attempt_id,
        "source_schema_digest": source_schema_digest,
        "destination_schema_digest": destination_schema_digest,
        "operations": operations,
    }))?;
    Ok(AppReference::parse(format!(
        "{prefix}:{}",
        digest.as_str().trim_start_matches("blake3:")
    ))?)
}

fn migration_fields_from_schema(
    schema: &AppSchemaRevision,
) -> Result<BTreeMap<AppName, BTreeMap<AppFieldPath, AppMigrationField>>, AppUpdateCoordinatorError>
{
    let entities = schema
        .canonical_entity_schema
        .get("entities")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            AppUpdateCoordinatorError::Corrupt("compiled schema has no entities".into())
        })?;
    let mut result = BTreeMap::new();
    for (entity_name, entity_value) in entities {
        let entity = AppName::parse(entity_name.clone())?;
        let fields = entity_value
            .get("fields")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                AppUpdateCoordinatorError::Corrupt(format!("entity `{entity}` has no fields"))
            })?;
        let mut compiled = BTreeMap::new();
        for (field_name, field_value) in fields {
            let field = AppFieldPath::parse(field_name.clone())?;
            let kind: AppQueryScalarKind =
                serde_json::from_value(field_value.get("kind").cloned().ok_or_else(|| {
                    AppUpdateCoordinatorError::Corrupt(format!("field `{field}` has no kind"))
                })?)
                .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?;
            let required = field_value
                .get("required")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    AppUpdateCoordinatorError::Corrupt(format!(
                        "field `{field}` has no required flag"
                    ))
                })?;
            let nullable = field_value
                .get("nullable")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    AppUpdateCoordinatorError::Corrupt(format!(
                        "field `{field}` has no nullable flag"
                    ))
                })?;
            let enum_values = field_value
                .get("enum_values")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| {
                            AppUpdateCoordinatorError::Corrupt("enum value is not text".into())
                        })
                        .and_then(|value| AppName::parse(value.to_owned()).map_err(Into::into))
                })
                .collect::<Result<BTreeSet<_>, AppUpdateCoordinatorError>>()?;
            compiled.insert(
                field,
                AppMigrationField {
                    kind,
                    required,
                    nullable,
                    enum_values,
                },
            );
        }
        result.insert(entity, compiled);
    }
    Ok(result)
}

/// Final coordinator boundary called from the registry lifecycle transaction.
/// It performs the zero-tail/final catch-up check and publishes the hidden data
/// generation before the installation pointer CAS and outbox append commit.
#[allow(clippy::too_many_arguments)]
pub(super) fn promote_staged_update_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    scope: &AppScope,
    installation: &AppInstallation,
    attempt: &AppLifecycleAttempt,
    grant: &AppGrantRevision,
    schema: &AppSchemaRevision,
    surface_revision: AppRevision,
    migration_run_id: Option<&AppReference>,
    expected_update_plan_digest: Option<&AppDigest>,
    expected_source_fence_digest: Option<&AppDigest>,
    now: DateTime<Utc>,
) -> Result<(), AppUpdateCoordinatorError> {
    if attempt.kind == AppLifecycleAttemptKind::InitialInstall {
        if migration_run_id.is_some()
            || expected_update_plan_digest.is_some()
            || expected_source_fence_digest.is_some()
        {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "initial install carried update coordinator authority".into(),
            ));
        }
        return Ok(());
    }
    let (migration_run_id, expected_update_plan_digest, expected_source_fence_digest) = match (
        migration_run_id,
        expected_update_plan_digest,
        expected_source_fence_digest,
    ) {
        (Some(run), Some(plan), Some(fence)) => (run, plan, fence),
        _ => {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "reviewed update commit has no exact coordinator authority".into(),
            ));
        },
    };
    let mut run = load_update_run(transaction, migration_run_id)?.ok_or_else(|| {
        AppUpdateCoordinatorError::StaleCandidate("migration run missing at switch".into())
    })?;
    if run.state != AppUpdateCoordinatorState::ReadyToSwitch
        || run.installation_id != installation.installation_id
        || run.attempt_id != attempt.attempt_id
        || run.attempt_kind != attempt.kind
        || &run.update_plan_digest != expected_update_plan_digest
        || &run.source_fence.fence_digest != expected_source_fence_digest
        || run.destination_package_revision_ref != grant.package_revision_ref
        || run.destination_package_revision_ref != schema.package_revision_ref
        || run.destination_schema_revision != schema.revision
    {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "migration run no longer matches the reviewed switch".into(),
        ));
    }
    if run.backup_required()
        && run.backup_receipt.as_ref().is_none_or(|receipt| {
            !receipt.encrypted
                || receipt.source_fence_digest != run.source_fence.fence_digest
                || receipt.update_plan_digest != run.update_plan_digest
        })
    {
        return Err(AppUpdateCoordinatorError::BackupRequired);
    }
    if run
        .migration_plan
        .as_ref()
        .is_some_and(AppCompiledMigrationPlan::enables_remote_processing)
        && grant.granted_data_handling_policy.model_processing != AppModelProcessing::RemoteAllowed
    {
        return Err(AppUpdateCoordinatorError::DestinationMismatch(
            "record processing transition requires the reviewed remote_allowed grant at switch"
                .into(),
        ));
    }
    let current_fence = super::registry::reviewable_revision_source_fence_blocking(
        transaction,
        scope,
        &installation.installation_id,
        attempt.kind,
    )?;
    if current_fence != run.source_fence {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "write-tail changed before final catch-up".into(),
        ));
    }
    let canonical_destination = AppDigest::blake3_canonical_json(&schema.canonical_entity_schema)?;
    let canonical_preview =
        AppDigest::blake3_canonical_json(&run.destination_schema_preview.canonical_entity_schema)?;
    if canonical_destination != canonical_preview
        || schema.compiled_index_plan != run.destination_schema_preview.compiled_index_plan
    {
        return Err(AppUpdateCoordinatorError::DestinationMismatch(
            "reviewed destination schema differs from the dry-run target".into(),
        ));
    }
    let head_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_record_heads WHERE installation_id = ?1",
        params![installation.installation_id.as_str()],
        |row| row.get(0),
    )?;
    if u64::try_from(head_count).ok() != Some(run.dry_run_examined) {
        return Err(AppUpdateCoordinatorError::StaleCandidate(
            "source record set changed before final catch-up".into(),
        ));
    }

    if run.migration_plan.is_some() {
        let staged = load_staged_records(transaction, &run)?;
        if u64::try_from(staged.len()).ok() != Some(run.dry_run_examined) {
            return Err(AppUpdateCoordinatorError::Corrupt(
                "immutable staged generation is incomplete".into(),
            ));
        }
        let mut working = BTreeMap::new();
        for record in staged {
            let current: Option<(i64, i64)> = transaction
                .query_row(
                    "SELECT record_revision, change_seq FROM app_record_heads
                     WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
                    params![
                        installation.installation_id.as_str(),
                        record.entity.as_str(),
                        record.record_id.as_str(),
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if current
                != Some((
                    i64::try_from(record.source_record_revision.get()).unwrap_or(i64::MAX),
                    i64::try_from(record.source_change_seq).unwrap_or(i64::MAX),
                ))
            {
                return Err(AppUpdateCoordinatorError::StaleCandidate(format!(
                    "write-tail changed for {}/{}",
                    record.entity, record.record_id
                )));
            }
            let deleted = record.deleted_at.is_some();
            working.insert(
                (record.entity.clone(), record.record_id.clone()),
                WorkingRecord {
                    entity: record.entity,
                    record_id: record.record_id,
                    prior_revision: Some(record.source_record_revision),
                    created_at: record.created_at,
                    payload: record.payload,
                    handling_policy: record.handling_policy,
                    was_deleted: deleted,
                    deleted,
                },
            );
        }
        let next_installation_generation = installation
            .lifecycle
            .generation
            .checked_add(1)
            .ok_or_else(|| {
                AppUpdateCoordinatorError::Corrupt("installation generation exhausted".into())
            })?;
        let active = ActiveAppEntitySchema::from_reviewed_update(
            next_installation_generation,
            surface_revision,
            grant.clone(),
            schema.clone(),
        )?;
        let usage = project_storage_usage(transaction, &active, &working)?;
        let generation =
            advance_dataset_generation(transaction, &installation.installation_id, now)?;
        if generation != run.destination_dataset_generation {
            return Err(AppUpdateCoordinatorError::StaleCandidate(
                "dataset generation changed before switch".into(),
            ));
        }
        let first_change_seq = if working.is_empty() {
            None
        } else {
            Some(reserve_change_sequences(
                transaction,
                &installation.installation_id,
                working.len(),
            )?)
        };
        let provenance = AppRecordProvenance {
            actor_kind: AppRecordActorKind::Migration,
            actor_id: run.migration_run_id.clone(),
            execution_id: Some(run.attempt_id.clone()),
            output_revision: Some(schema.revision),
            // This durable run becomes Switched in the same transaction as
            // these revisions. It is the write receipt; a backup is optional
            // recovery evidence and cannot identify every migration write.
            mutation_receipt_id: Some(run.migration_run_id.clone()),
            source_artifact_refs: run
                .migration_plan
                .as_ref()
                .map(|plan| vec![plan.plan_ref().clone()])
                .unwrap_or_default(),
            citation_refs: Vec::new(),
        };
        for (offset, record) in working.values().enumerate() {
            let revision = next_revision(record.prior_revision)?;
            let change_seq = first_change_seq
                .and_then(|first| first.checked_add(u64::try_from(offset).ok()?))
                .ok_or_else(|| {
                    AppUpdateCoordinatorError::Corrupt("change sequence exhausted".into())
                })?;
            write_record_revision(
                transaction,
                &active,
                record,
                revision,
                generation,
                change_seq,
                &provenance,
                now,
            )?;
        }
        persist_storage_usage(transaction, &installation.installation_id, usage, now)?;
    }

    transaction.execute(
        "DELETE FROM app_migration_staged_records WHERE migration_run_id = ?1",
        params![run.migration_run_id.as_str()],
    )?;
    run.state = AppUpdateCoordinatorState::Switched;
    run.updated_at = now;
    replace_update_run(transaction, &run, AppUpdateCoordinatorState::ReadyToSwitch)?;
    Ok(())
}

fn load_staged_records(
    transaction: &rusqlite::Transaction<'_>,
    run: &AppDurableUpdateRun,
) -> Result<Vec<AppStagedMigrationRecord>, AppUpdateCoordinatorError> {
    let mut statement = transaction.prepare(
        "SELECT record_json FROM app_migration_staged_records
         WHERE migration_run_id = ?1 AND installation_id = ?2
         ORDER BY source_change_seq, entity_name, record_id",
    )?;
    let rows = statement.query_map(
        params![run.migration_run_id.as_str(), run.installation_id.as_str()],
        |row| row.get::<_, Vec<u8>>(0),
    )?;
    let mut records = Vec::new();
    for row in rows {
        records.push(
            serde_json::from_slice(&row?)
                .map_err(|error| AppUpdateCoordinatorError::Corrupt(error.to_string()))?,
        );
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::apps::{
        models::{AppDataClassification, AppModelProcessing},
        records::{
            AppBackgroundExecution, AppEventSubscriptionV1, AppEventTerminalOutcomeV1,
            AppExternalEgress, AppMemoryPromotion, AppNetworkPolicy, AppNotificationKindV1,
            AppNotificationSeverityV1, AppPersonalAgentAccess,
        },
    };

    fn item4_behavior_resources() -> AppBehaviorResourceCeiling {
        AppBehaviorResourceCeiling {
            max_tokens_per_run: 100,
            max_cost_microusd_per_run: 100,
            max_active_seconds_per_run: 30,
            max_tokens_per_month: 1_000,
            max_cost_microusd_per_month: 1_000,
            max_starts_per_period: 4,
            period_seconds: 3_600,
            max_causation_depth: 2,
            max_spend_depth: 2,
            max_contribution_proposals_per_run: 1,
        }
    }

    fn item4_event_grant() -> AppEventBehaviorGrant {
        AppEventBehaviorGrant {
            event_behavior_id: AppName::parse("on_completion").unwrap(),
            purpose: "Summarize one canonical completion.".to_owned(),
            action: AppName::parse("summarize_completion").unwrap(),
            subscription: AppEventSubscriptionV1::InstallationExecutionTerminal {
                outcomes: vec![AppEventTerminalOutcomeV1::Succeeded],
            },
            subscription_digest: AppDigest::blake3(b"subscription"),
            projection_schema_digest: AppDigest::blake3(b"projection"),
            steps_digest: None,
            operations: Vec::new(),
            output_schema_digest: None,
            min_interval_seconds: 60,
            resources: item4_behavior_resources(),
            reviewed_request_digest: AppDigest::blake3(b"event-review"),
        }
    }

    fn item4_notification_grant() -> AppNotificationGrant {
        AppNotificationGrant {
            workflow_id: AppName::parse("daily_digest").unwrap(),
            port_id: AppName::parse("owner_briefing").unwrap(),
            purpose: "Keep the owner informed.".to_owned(),
            kind: AppNotificationKindV1::Briefing,
            severity_ceiling: AppNotificationSeverityV1::Warning,
            max_notifications_per_period: 4,
            period_seconds: 3_600,
            max_pending: 4,
            ttl_seconds: 7_200,
            reviewed_request_digest: AppDigest::blake3(b"notification-review"),
        }
    }

    fn policy(
        floor: AppDataClassification,
        processing: AppModelProcessing,
    ) -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: floor,
            model_processing: processing,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn ceiling(records: u64) -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 1,
            max_output_tokens: 1,
            max_cost_microusd: 1,
            max_paid_tool_invocations: 1,
            max_active_seconds: 1,
            max_lifetime_seconds: 1,
            max_browser_network_actions: 1,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 0,
            max_records: records,
            max_payload_bytes: 1_024,
            max_attachment_bytes: 1_024,
            max_monthly_tokens: 1,
            max_monthly_cost_microusd: 1,
        }
    }

    fn grant(records: u64, handling: AppDataHandlingPolicy) -> AppGrantRevision {
        AppGrantRevision {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            revision: AppRevision::new(1).unwrap(),
            package_revision_ref: AppReference::parse("package:1").unwrap(),
            requested_tools: Vec::new(),
            granted_tools: Vec::new(),
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
            requested_context_reads: Vec::new(),
            granted_context_reads: Vec::new(),
            requested_interactive_capabilities: Vec::new(),
            granted_interactive_capabilities: Vec::new(),
            granted_custom_surface_entry_points: Vec::new(),
            requested_behavior_grants: Vec::new(),
            granted_behavior_grants: Vec::new(),
            requested_event_behavior_grants: Vec::new(),
            granted_event_behavior_grants: Vec::new(),
            requested_notification_grants: Vec::new(),
            granted_notification_grants: Vec::new(),
            requested_memory_read: None,
            granted_memory_read: None,
            requested_secret_uses: None,
            granted_secret_uses: None,
            granted_any_public_host: false,
            requested_personal_agent_data_access: Vec::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: handling.clone(),
            granted_data_handling_policy: handling.clone(),
            granted_data_handling_policy_digest: AppDigest::blake3(b"grant"),
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: ceiling(records),
            granted_resource_ceiling: ceiling(records),
            approved_by: AppReference::parse("actor:owner").unwrap(),
            approved_at: chrono::Utc::now(),
            authority_digest: AppDigest::blake3(b"authority"),
            revoked_at: None,
        }
    }

    #[test]
    fn gaining_any_public_host_requires_review_and_absence_keeps_old_digests() {
        let handling = policy(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
        );
        let plain = grant(10, handling);
        let unchanged = compute_permission_diff(&plain, &plain);
        assert_eq!(unchanged.any_public_host, AppPermissionChangeKind::Unchanged);
        assert!(serde_json::to_value(&unchanged)
            .unwrap()
            .get("any_public_host")
            .is_none());
        let mut any = plain.clone();
        any.granted_any_public_host = true;
        let widened = compute_permission_diff(&plain, &any);
        assert_eq!(widened.any_public_host, AppPermissionChangeKind::Expanded);
        assert!(widened.requires_review);
        assert_ne!(widened.diff_digest, unchanged.diff_digest);
        let narrowed = compute_permission_diff(&any, &plain);
        assert_eq!(narrowed.any_public_host, AppPermissionChangeKind::Narrowed);
        assert!(!narrowed.requires_review);
    }

    #[test]
    fn widening_a_keys_scope_requires_review() {
        use crate::apps::secret_access::AppSecretUseGrant;
        let key = |hosts: &[&str], any_site: bool| AppSecretUseGrant {
            tool: AppReference::parse("capability:fetcher").unwrap(),
            secret_ref: "API_KEY".to_owned(),
            hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            any_site,
        };
        let handling = policy(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
        );
        let with = |grants: Vec<AppSecretUseGrant>| {
            let mut revision = grant(10, handling.clone());
            revision.granted_secret_uses = Some(grants);
            revision
        };
        let diff = |current: Vec<AppSecretUseGrant>, proposed: Vec<AppSecretUseGrant>| {
            compute_permission_diff(&with(current), &with(proposed))
        };
        // A scope-less (declared-host) key diffs exactly as before scopes.
        let plain = diff(vec![key(&[], false)], vec![key(&[], false)]);
        assert_eq!(plain.secret_uses, AppPermissionChangeKind::Unchanged);
        assert_eq!(
            plain.diff_digest,
            compute_permission_diff(&grant(10, handling.clone()), &grant(10, handling.clone()))
                .diff_digest
        );
        // Same scope: unchanged.
        let same = diff(
            vec![key(&["a.example.com"], false)],
            vec![key(&["a.example.com"], false)],
        );
        assert_eq!(same.secret_uses, AppPermissionChangeKind::Unchanged);
        assert!(!same.requires_review);
        // Picked host -> any site, or another picked host: expanded, reviewed.
        for wider in [
            key(&[], true),
            key(&["a.example.com", "b.example.com"], false),
            key(&["b.example.com"], false),
        ] {
            let widened = diff(vec![key(&["a.example.com"], false)], vec![wider.clone()]);
            assert_eq!(
                widened.secret_uses,
                AppPermissionChangeKind::Expanded,
                "{wider:?}"
            );
            assert!(widened.requires_review, "{wider:?}");
            assert_ne!(widened.diff_digest, same.diff_digest);
        }
        // Any site -> a picked host, or fewer picked hosts: narrowed.
        for (current, narrower) in [
            (key(&[], true), key(&["a.example.com"], false)),
            (
                key(&["a.example.com", "b.example.com"], false),
                key(&["a.example.com"], false),
            ),
        ] {
            let narrowed = diff(vec![current], vec![narrower]);
            assert_eq!(narrowed.secret_uses, AppPermissionChangeKind::Narrowed);
            assert!(!narrowed.requires_review);
        }
    }

    #[test]
    fn memory_access_widening_requires_review_and_absence_keeps_old_digests() {
        use crate::apps::memory_access::{
            default_memory_read_grant, AppMemoryReadRequest, AppMemoryReadSelection,
        };
        let handling = policy(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
        );
        let plain = grant(10, handling.clone());
        let unchanged = compute_permission_diff(&plain, &plain);
        assert_eq!(unchanged.memory_read, AppPermissionChangeKind::Unchanged);
        assert!(
            serde_json::to_value(&unchanged)
                .unwrap()
                .get("memory_read")
                .is_none(),
            "an unchanged memory axis is omitted so pre-feature diffs keep their bytes"
        );

        let request = AppMemoryReadRequest {
            user_tiers: vec!["preferences".into()],
            agents: vec!["scribe".into()],
            purpose: "Personalise".into(),
        };
        let mut with_memory = plain.clone();
        with_memory.requested_memory_read = Some(request.clone());
        with_memory.granted_memory_read = default_memory_read_grant(&request);
        let first_grant = compute_permission_diff(&plain, &with_memory);
        assert_eq!(first_grant.memory_read, AppPermissionChangeKind::Expanded);
        assert!(first_grant.requires_review);
        assert_ne!(first_grant.diff_digest, unchanged.diff_digest);

        let mut background = with_memory.clone();
        if let Some(grant) = background.granted_memory_read.as_mut() {
            grant.background = AppMemoryReadSelection {
                user_tiers: vec!["preferences".into()],
                agents: Vec::new(),
            };
        }
        let widened = compute_permission_diff(&with_memory, &background);
        assert_eq!(widened.memory_read, AppPermissionChangeKind::Expanded);
        assert!(
            widened.requires_review,
            "adding background memory access is a widening"
        );

        let narrowed = compute_permission_diff(&background, &with_memory);
        assert_eq!(narrowed.memory_read, AppPermissionChangeKind::Narrowed);
    }

    #[test]
    fn item4_update_diff_distinguishes_narrowing_from_authority_expansion() {
        let handling = policy(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
        );
        let mut current = grant(10, handling);
        current.granted_event_behavior_grants = vec![item4_event_grant()];
        current.granted_notification_grants = vec![item4_notification_grant()];

        let mut narrowed = current.clone();
        narrowed.granted_event_behavior_grants[0].min_interval_seconds = 120;
        narrowed.granted_event_behavior_grants[0]
            .resources
            .max_tokens_per_run = 50;
        narrowed.granted_notification_grants[0].severity_ceiling = AppNotificationSeverityV1::Info;
        narrowed.granted_notification_grants[0].max_pending = 2;
        narrowed.granted_notification_grants[0].ttl_seconds = 3_600;
        let narrowing = compute_permission_diff(&current, &narrowed);
        assert_eq!(narrowing.event_behaviors, AppPermissionChangeKind::Narrowed);
        assert_eq!(
            narrowing.owner_notifications,
            AppPermissionChangeKind::Narrowed
        );
        assert!(!narrowing.requires_review);

        let mut event_expansion = current.clone();
        event_expansion.granted_event_behavior_grants[0].purpose =
            "Substituted event purpose.".to_owned();
        let event_diff = compute_permission_diff(&current, &event_expansion);
        assert_eq!(
            event_diff.event_behaviors,
            AppPermissionChangeKind::Expanded
        );
        assert!(event_diff.requires_review);

        let mut notification_expansion = current.clone();
        notification_expansion.granted_notification_grants[0].ttl_seconds += 1;
        let notification_diff = compute_permission_diff(&current, &notification_expansion);
        assert_eq!(
            notification_diff.owner_notifications,
            AppPermissionChangeKind::Expanded
        );
        assert!(notification_diff.requires_review);

        let added_event = compute_permission_diff(
            &grant(
                10,
                policy(
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                ),
            ),
            &current,
        );
        assert_eq!(
            added_event.event_behaviors,
            AppPermissionChangeKind::Expanded
        );
        assert_eq!(
            added_event.owner_notifications,
            AppPermissionChangeKind::Expanded
        );
        assert!(added_event.requires_review);
    }

    /// The residual the exhaustive destructure cannot close on its own: the
    /// compiler forces a new axis to be BOUND, not to be COMPARED correctly
    /// (`_` still compiles). This walks each axis, expands only that one, and
    /// insists the diff notices — so an axis that is bound and then ignored
    /// fails here.
    ///
    /// These three were the real gap: network destinations, background
    /// execution and context reads were absent from the diff entirely, so an
    /// update that added an egress host, tightened its schedule or widened its
    /// context reads would have skipped owner review while claiming
    /// "authority expansion always re-enters review".
    #[test]
    fn every_authority_axis_expansion_requires_review() {
        let base = || {
            grant(
                10,
                policy(
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                ),
            )
        };
        let tool = |name: &str| AppReference::parse(name.to_owned()).unwrap();

        // Network: a new approved destination is a new place data can go.
        let mut proposed = base();
        proposed.granted_network_policy = AppNetworkPolicy::ApprovedDestinations {
            destinations: vec![tool("host:evil.example")],
        };
        let diff = compute_permission_diff(&base(), &proposed);
        assert_eq!(diff.network_policy, AppPermissionChangeKind::Expanded);
        assert!(
            diff.requires_review,
            "a new egress destination must re-enter review"
        );

        // Adding a host to an existing list is the subtler case the old
        // mode-only comparison could never see.
        let mut current = base();
        current.granted_network_policy = AppNetworkPolicy::ApprovedDestinations {
            destinations: vec![tool("host:api.example")],
        };
        let mut proposed = base();
        proposed.granted_network_policy = AppNetworkPolicy::ApprovedDestinations {
            destinations: vec![tool("host:api.example"), tool("host:evil.example")],
        };
        let diff = compute_permission_diff(&current, &proposed);
        assert_eq!(diff.network_policy, AppPermissionChangeKind::Expanded);
        assert!(diff.requires_review);

        // Background execution: unattended at all, then more often.
        let mut proposed = base();
        proposed.granted_background_execution = AppBackgroundExecution::Granted {
            min_interval_seconds: 3600,
            max_concurrent_runs: 1,
        };
        let diff = compute_permission_diff(&base(), &proposed);
        assert_eq!(diff.background_execution, AppPermissionChangeKind::Expanded);
        assert!(diff.requires_review);

        let mut current = base();
        current.granted_background_execution = AppBackgroundExecution::Granted {
            min_interval_seconds: 3600,
            max_concurrent_runs: 1,
        };
        let mut proposed = current.clone();
        proposed.granted_background_execution = AppBackgroundExecution::Granted {
            min_interval_seconds: 60,
            max_concurrent_runs: 1,
        };
        let diff = compute_permission_diff(&current, &proposed);
        assert_eq!(
            diff.background_execution,
            AppPermissionChangeKind::Expanded,
            "a shorter minimum interval means it may run more often"
        );

        // Context reads.
        let mut proposed = base();
        proposed.granted_context_reads = vec![tool("context:calendar")];
        let diff = compute_permission_diff(&base(), &proposed);
        assert_eq!(diff.context_reads, AppPermissionChangeKind::Expanded);
        assert!(diff.requires_review);

        // Tools, for completeness of the walk.
        let mut proposed = base();
        proposed.granted_tools = vec![tool("capability:http")];
        let diff = compute_permission_diff(&base(), &proposed);
        assert_eq!(diff.tools, AppPermissionChangeKind::Expanded);
        assert!(diff.requires_review);

        // An identical grant expands nothing.
        let diff = compute_permission_diff(&base(), &base());
        assert!(
            !diff.requires_review,
            "an unchanged grant must not force a pointless re-review"
        );
    }

    /// Narrowing is not an expansion and must not force review.
    #[test]
    fn narrowing_an_axis_does_not_require_review() {
        let base = || {
            grant(
                10,
                policy(
                    AppDataClassification::Personal,
                    AppModelProcessing::LocalOnly,
                ),
            )
        };
        let mut current = base();
        current.granted_network_policy = AppNetworkPolicy::ApprovedDestinations {
            destinations: vec![AppReference::parse("host:api.example".to_owned()).unwrap()],
        };
        current.granted_background_execution = AppBackgroundExecution::Granted {
            min_interval_seconds: 60,
            max_concurrent_runs: 4,
        };
        let proposed = base();
        let diff = compute_permission_diff(&current, &proposed);
        assert_eq!(diff.network_policy, AppPermissionChangeKind::Narrowed);
        assert_eq!(diff.background_execution, AppPermissionChangeKind::Narrowed);
        assert!(!diff.requires_review);
    }

    #[test]
    fn resource_or_policy_expansion_requires_review() {
        let current = grant(
            10,
            policy(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let proposed = grant(
            100,
            policy(
                AppDataClassification::Public,
                AppModelProcessing::RemoteAllowed,
            ),
        );
        let diff = compute_permission_diff(&current, &proposed);
        assert!(diff.requires_review);
        assert_eq!(diff.resources, AppPermissionChangeKind::Expanded);
        assert_eq!(diff.data_handling, AppPermissionChangeKind::Expanded);
    }

    #[test]
    fn agent_or_personality_expansion_requires_review() {
        let current = grant(
            10,
            policy(
                AppDataClassification::Personal,
                AppModelProcessing::LocalOnly,
            ),
        );
        let mut proposed = current.clone();
        proposed
            .requested_agents
            .push(AppReference::parse("agent:research-agent").unwrap());
        proposed
            .granted_agents
            .push(AppReference::parse("agent:research-agent").unwrap());
        let diff = compute_permission_diff(&current, &proposed);
        assert!(diff.requires_review);
        assert_eq!(diff.agents, AppPermissionChangeKind::Expanded);
        assert_eq!(diff.personalities, AppPermissionChangeKind::Unchanged);
    }

    #[test]
    fn switch_fails_closed_if_the_source_tail_moved() {
        let plan = AppUpdateSwitchPlan {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            source_package_revision_ref: AppReference::parse("package:1").unwrap(),
            destination_package_revision_ref: AppReference::parse("package:2").unwrap(),
            source_generation: 3,
            destination_generation: 4,
            source_tail_change_seq: 9,
            destination_schema_revision: AppRevision::new(2).unwrap(),
            destination_grant_revision: AppRevision::new(2).unwrap(),
            migration_plan_digest: None,
            permission_diff_digest: AppDigest::blake3(b"diff"),
            backup_export_digest: None,
            state: AppUpdateSwitchState::Quiesced,
        };
        assert!(matches!(
            switch_active_generation(&plan, 3, 10),
            Err(AppUpdateError::SourceGenerationMoved)
        ));
        assert_eq!(
            switch_active_generation(&plan, 3, 9).unwrap().state,
            AppUpdateSwitchState::Switched
        );
    }

    #[test]
    fn data_rewind_is_explicit_and_code_only_stays_compatible() {
        assert!(rollback_decision(
            AppSchemaCompatibility::MigrationRequired,
            AppRollbackKind::CodeOnly,
            false
        )
        .is_err());
        assert!(rollback_decision(
            AppSchemaCompatibility::MigrationRequired,
            AppRollbackKind::DataRewind,
            false
        )
        .is_err());
        let rewind = rollback_decision(
            AppSchemaCompatibility::MigrationRequired,
            AppRollbackKind::DataRewind,
            true,
        )
        .unwrap();
        assert!(rewind.discards_post_update_writes);
        assert!(
            !rollback_decision(
                AppSchemaCompatibility::Compatible,
                AppRollbackKind::CodeOnly,
                false
            )
            .unwrap()
            .discards_post_update_writes
        );
        let ux = rollback_ux(&rewind);
        assert!(ux.confirmation_required);
        assert!(ux.discards_post_update_writes);
        assert!(ux.summary.contains("discarded"));
    }

    #[test]
    fn migration_run_identity_is_deterministic_and_binds_exact_operation_bodies() {
        let attempt = AppReference::parse("attempt:update-1").unwrap();
        let source = AppDigest::blake3(b"schema-v1");
        let destination = AppDigest::blake3(b"schema-v2");
        let operation = AppMigrationOperation::AddField {
            entity: AppName::parse("note").unwrap(),
            field: AppFieldPath::parse("archived").unwrap(),
            scalar: AppQueryScalarKind::Boolean,
            nullable: false,
            default: Some(Value::Bool(false)),
        };
        let first = update_plan_reference(
            "migration-run",
            &attempt,
            &source,
            &destination,
            std::slice::from_ref(&operation),
        )
        .unwrap();
        let replay = update_plan_reference(
            "migration-run",
            &attempt,
            &source,
            &destination,
            std::slice::from_ref(&operation),
        )
        .unwrap();
        assert_eq!(first, replay, "exact replay must recover one run identity");

        let changed = update_plan_reference(
            "migration-run",
            &attempt,
            &source,
            &destination,
            &[AppMigrationOperation::AddField {
                entity: AppName::parse("note").unwrap(),
                field: AppFieldPath::parse("archived").unwrap(),
                scalar: AppQueryScalarKind::Boolean,
                nullable: false,
                default: Some(Value::Bool(true)),
            }],
        )
        .unwrap();
        assert_ne!(
            first, changed,
            "a changed migration body must not replay as the reviewed plan"
        );
    }
}
