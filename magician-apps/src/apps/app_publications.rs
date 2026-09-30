//! App-owned projection into the shared published-surface index.
//!
//! Active compiler bindings are the sole source. Reconciliation writes the
//! existing central publication record shape with `task_id = None`; it never
//! fabricates a task, execution or output owner. Only records explicitly owned
//! by the target installation are updated, so agent/task publication goldens
//! and their index entries are left untouched.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{Arc, Mutex as StdMutex, Weak},
};

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    lifecycle::AppInstallationStatus,
    models::{decode_app_contract, AppContractLimits, AppInstallationId, AppReference},
    records::{AppInstallation, AppSurfaceBinding, AppSurfaceStatus},
    registry_lifecycle::AppLifecycleOutboxEvent,
    surface_hydration::{AppSurfaceHydrationError, AppSurfaceHydrationService},
};
use magician::magician_v2::{
    artifact_v2::{
        models::{PublishedSurfacePlacement, PublishedSurfaceRecord},
        publications::{
            published_surface_changed_payload, FilesystemPublishedSurfaceStore,
            PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
        },
        service::ArtifactV2Error,
        workspace::ArtifactV2Workspace,
    },
    realtime_events::{AgentEventEnvelope, RuntimeTransportBroadcaster},
};

const APP_SURFACE_KIND: &str = "app";
const APP_SURFACE_PLACEMENT_KIND: &str = "apps";
pub const MAX_APP_PUBLICATION_REPAIR_RECORDS: usize = 4_096;
pub const MAX_APP_PUBLICATION_REPAIR_INSTALLATIONS: usize = 1_024;
const MAX_APP_PUBLICATION_RECONCILIATION_ATTEMPTS: usize = 3;

type ReconciliationLock = tokio::sync::Mutex<()>;
type ReconciliationLockRegistry =
    Arc<StdMutex<HashMap<(String, String, String), Weak<ReconciliationLock>>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPublicationReconciliationReceipt {
    pub installation_id: AppInstallationId,
    pub surface_revision: Option<super::models::AppRevision>,
    pub projected: usize,
    pub superseded: usize,
    pub unchanged: usize,
}

struct PlannedAppPublicationReconciliation {
    receipt: AppPublicationReconciliationReceipt,
    writes: Vec<PublishedSurfaceRecord>,
}

#[derive(Debug, Clone)]
pub struct AppPublicationReconciler {
    registry: super::registry::AppRegistryService,
    hydration: AppSurfaceHydrationService,
    store: FilesystemPublishedSurfaceStore,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    reconciliation_locks: ReconciliationLockRegistry,
}

impl AppPublicationReconciler {
    pub fn new(
        registry: super::registry::AppRegistryService,
        workspace: ArtifactV2Workspace,
    ) -> Self {
        Self {
            registry: registry.clone(),
            hydration: AppSurfaceHydrationService::new(registry),
            store: FilesystemPublishedSurfaceStore::new(workspace),
            event_broadcaster: None,
            reconciliation_locks: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    /// Reconcile the lifecycle-visible publication state. Enabled apps publish
    /// their exact active binding set; every other durable lifecycle state
    /// withdraws only records owned by this installation.
    pub async fn reconcile_installation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppPublicationReconciliationReceipt, AppPublicationReconciliationError> {
        let lock = self.reconciliation_lock(authenticated_scope, installation_id);
        let _guard = lock.lock().await;
        self.reconcile_current_with_retry(authenticated_scope, installation_id, now)
            .await
    }

    pub fn with_event_broadcaster(
        mut self,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        self.event_broadcaster = Some(event_broadcaster);
        self
    }

    pub async fn reconcile_active_installation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppPublicationReconciliationReceipt, AppPublicationReconciliationError> {
        let receipt = self
            .reconcile_installation(authenticated_scope, installation_id, now)
            .await?;
        if receipt.surface_revision.is_none() {
            return Err(AppPublicationReconciliationError::InstallationNotActive);
        }
        Ok(receipt)
    }

    /// Reconcile a durable lifecycle delivery against the current registry
    /// head. A later generation supersedes an older queued event; an event
    /// ahead of the registry head is corruption and is retried without
    /// touching files.
    pub async fn reconcile_lifecycle_event(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        event: &AppLifecycleOutboxEvent,
        now: DateTime<Utc>,
    ) -> Result<AppPublicationReconciliationReceipt, AppPublicationReconciliationError> {
        let installation_id = &event.installation_id;
        let lock = self.reconciliation_lock(authenticated_scope, installation_id);
        let _guard = lock.lock().await;
        let installation = self
            .registry
            .installation(authenticated_scope, installation_id, now)
            .await?;
        if let Some(current) = installation.as_ref() {
            ensure_installation_scope(current, authenticated_scope, installation_id)?;
            if current.lifecycle.generation < event.installation_generation {
                return Err(AppPublicationReconciliationError::LifecycleEventAhead);
            }
            if current.lifecycle.generation == event.installation_generation
                && (current.lifecycle.status != event.lifecycle_status
                    || current.package_revision_ref != event.package_revision_ref
                    || current.active_surface_revision != event.surface_revision)
            {
                return Err(AppPublicationReconciliationError::LifecycleEventMismatch);
            }
        }
        self.reconcile_current_with_retry(authenticated_scope, installation_id, now)
            .await
    }

    /// Bounded boot repair. Authoritative record files repair both directions
    /// of index drift, then exact registry installations and app-owned orphan
    /// records are reconciled. Shared task/agent records are only copied into
    /// the rebuilt central index; they are never mutated here.
    pub async fn repair_scope(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<usize, AppPublicationReconciliationError> {
        for _attempt in 0..MAX_APP_PUBLICATION_RECONCILIATION_ATTEMPTS {
            let principal = authenticated_scope.scope().principal.as_str();
            let workspace = authenticated_scope.scope().workspace.as_str();
            let loaded = self
                .store
                .reconcile_index_from_record_files_bounded_validated(
                    principal,
                    workspace,
                    MAX_APP_PUBLICATION_REPAIR_RECORDS,
                    |records| validate_inventory(records, authenticated_scope),
                )
                .await?;
            if loaded.unreadable_records != 0 {
                return Err(AppPublicationReconciliationError::UnreadableAuthoritativeRecords);
            }
            let registry_snapshots =
                load_installation_snapshots_bounded(&self.registry, authenticated_scope, now)
                    .await?;
            let mut installation_ids = registry_snapshots.keys().cloned().collect::<BTreeSet<_>>();
            for record in &loaded.records {
                if record_claims_app_installation(record) {
                    let raw = record
                        .placement
                        .placement_id
                        .as_deref()
                        .ok_or(AppPublicationReconciliationError::InvalidOwnedSurfaceRecord)?;
                    let installation_id = AppInstallationId::parse(raw.to_owned())?;
                    installation_ids.insert(installation_id);
                }
            }
            if installation_ids.len() > MAX_APP_PUBLICATION_REPAIR_INSTALLATIONS {
                return Err(AppPublicationReconciliationError::RepairInstallationLimitExceeded);
            }

            // A single authoritative inventory feeds every plan. Holding the
            // sorted installation locks prevents another in-process repair
            // from rewriting an installation between this batch's plans.
            let mut guards = Vec::with_capacity(installation_ids.len());
            for installation_id in &installation_ids {
                guards.push(
                    self.reconciliation_lock(authenticated_scope, installation_id)
                        .lock_owned()
                        .await,
                );
            }
            let mut working = loaded.records;
            let mut positions = working
                .iter()
                .enumerate()
                .map(|(index, record)| (record.surface_id.clone(), index))
                .collect::<HashMap<_, _>>();
            let mut snapshots = Vec::with_capacity(installation_ids.len());
            let mut plans = Vec::with_capacity(installation_ids.len());
            let mut retry = false;
            for installation_id in &installation_ids {
                let installation = registry_snapshots.get(installation_id).cloned();
                if let Some(installation) = installation.as_ref() {
                    ensure_installation_scope(installation, authenticated_scope, installation_id)?;
                }
                let plan = match self
                    .plan_snapshot(
                        authenticated_scope,
                        installation_id,
                        installation.as_ref(),
                        &working,
                        &now,
                    )
                    .await
                {
                    Ok(plan) => plan,
                    Err(AppPublicationReconciliationError::InstallationChanged) => {
                        retry = true;
                        break;
                    },
                    Err(error) => return Err(error),
                };
                for write in &plan.writes {
                    if let Some(index) = positions.get(&write.surface_id).copied() {
                        working[index] = write.clone();
                    } else {
                        if working.len() >= MAX_APP_PUBLICATION_REPAIR_RECORDS {
                            return Err(
                                AppPublicationReconciliationError::RepairRecordLimitExceeded,
                            );
                        }
                        let index = working.len();
                        positions.insert(write.surface_id.clone(), index);
                        working.push(write.clone());
                    }
                }
                snapshots.push((installation_id.clone(), installation));
                plans.push(plan);
                if plans.len() % 16 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            if retry {
                continue;
            }
            if !self
                .all_snapshots_current_batched(authenticated_scope, &snapshots, now)
                .await?
            {
                continue;
            }
            let writes = plans
                .iter()
                .flat_map(|plan| plan.writes.iter().cloned())
                .collect::<Vec<_>>();
            if !writes.is_empty() {
                self.store.upsert_surfaces(&writes).await?;
            }
            if !self
                .all_snapshots_current_batched(authenticated_scope, &snapshots, now)
                .await?
            {
                // Compensate immediately from a new single inventory pass;
                // after the bounded retry limit, the durable lifecycle outbox
                // remains pending and will repair the final state.
                continue;
            }
            for plan in &plans {
                if !plan.writes.is_empty() {
                    self.emit_publication_events(
                        &plan.receipt.installation_id,
                        principal,
                        workspace,
                        &plan.writes,
                    );
                }
            }
            drop(guards);
            return Ok(plans.len());
        }
        Err(AppPublicationReconciliationError::InstallationChanged)
    }

    async fn reconcile_current_with_retry(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppPublicationReconciliationReceipt, AppPublicationReconciliationError> {
        for _attempt in 0..MAX_APP_PUBLICATION_RECONCILIATION_ATTEMPTS {
            let installation = self
                .registry
                .installation(authenticated_scope, installation_id, now)
                .await?;
            if let Some(installation) = installation.as_ref() {
                ensure_installation_scope(installation, authenticated_scope, installation_id)?;
            }
            let existing = self.load_valid_inventory(authenticated_scope).await?;
            let plan = self
                .plan_snapshot(
                    authenticated_scope,
                    installation_id,
                    installation.as_ref(),
                    &existing,
                    &now,
                )
                .await?;
            ensure_reconciliation_record_capacity(&existing, &plan.writes)?;
            match self
                .ensure_snapshot_current(
                    authenticated_scope,
                    installation_id,
                    installation.as_ref(),
                    now,
                )
                .await
            {
                Ok(()) => {},
                Err(AppPublicationReconciliationError::InstallationChanged) => continue,
                Err(error) => return Err(error),
            }
            if !plan.writes.is_empty() {
                self.store.upsert_surfaces(&plan.writes).await?;
            }
            match self
                .ensure_snapshot_current(
                    authenticated_scope,
                    installation_id,
                    installation.as_ref(),
                    now,
                )
                .await
            {
                Ok(()) => {
                    if !plan.writes.is_empty() {
                        self.emit_publication_events(
                            installation_id,
                            authenticated_scope.scope().principal.as_str(),
                            authenticated_scope.scope().workspace.as_str(),
                            &plan.writes,
                        );
                    }
                    return Ok(plan.receipt);
                },
                Err(AppPublicationReconciliationError::InstallationChanged) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(AppPublicationReconciliationError::InstallationChanged)
    }

    async fn load_valid_inventory(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
    ) -> Result<Vec<PublishedSurfaceRecord>, AppPublicationReconciliationError> {
        let loaded = self
            .store
            .reconcile_index_from_record_files_bounded_validated(
                authenticated_scope.scope().principal.as_str(),
                authenticated_scope.scope().workspace.as_str(),
                MAX_APP_PUBLICATION_REPAIR_RECORDS,
                |records| validate_inventory(records, authenticated_scope),
            )
            .await?;
        if loaded.unreadable_records != 0 {
            return Err(AppPublicationReconciliationError::UnreadableAuthoritativeRecords);
        }
        Ok(loaded.records)
    }

    async fn plan_snapshot(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        installation: Option<&AppInstallation>,
        existing: &[PublishedSurfaceRecord],
        now: &DateTime<Utc>,
    ) -> Result<PlannedAppPublicationReconciliation, AppPublicationReconciliationError> {
        if let Some(installation) = installation {
            if installation.lifecycle.status == AppInstallationStatus::Enabled {
                return self
                    .plan_active_snapshot(
                        authenticated_scope,
                        installation_id,
                        installation,
                        existing,
                        now,
                    )
                    .await;
            }
        }
        self.plan_inactive_snapshot(installation_id, installation, existing, now)
    }

    async fn plan_active_snapshot(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        installation: &AppInstallation,
        existing: &[PublishedSurfaceRecord],
        now: &DateTime<Utc>,
    ) -> Result<PlannedAppPublicationReconciliation, AppPublicationReconciliationError> {
        let active = self
            .hydration
            .active_surface_bindings(authenticated_scope, installation_id, now.to_owned())
            .await?;
        if installation.active_surface_revision != Some(active.surface_revision) {
            return Err(AppPublicationReconciliationError::InstallationChanged);
        }
        let existing_by_id = existing
            .iter()
            .map(|record| (record.surface_id.as_str(), record))
            .collect::<HashMap<_, _>>();

        let mut active_ids = HashSet::with_capacity(active.bindings.len());
        let mut writes = Vec::new();
        let mut projected = 0usize;
        let mut unchanged = 0usize;
        for binding in &active.bindings {
            ensure_binding_identity(binding, installation_id, active.surface_revision)?;
            let surface_ref = binding
                .published_surface_ref
                .as_ref()
                .ok_or(AppPublicationReconciliationError::MissingPublishedSurfaceReference)?;
            if !active_ids.insert(surface_ref.as_str()) {
                return Err(AppPublicationReconciliationError::DuplicatePublishedSurfaceReference);
            }
            if existing_by_id
                .get(surface_ref.as_str())
                .is_some_and(|record| !app_record_belongs_to_installation(record, installation_id))
            {
                return Err(AppPublicationReconciliationError::ForeignSurfaceIdentityCollision);
            }
            let candidate = projected_record(
                authenticated_scope,
                binding,
                surface_ref,
                existing_by_id.get(surface_ref.as_str()).copied(),
                now,
            );
            if existing_by_id
                .get(surface_ref.as_str())
                .is_some_and(|record| *record == &candidate)
            {
                unchanged = unchanged.saturating_add(1);
            } else {
                projected = projected.saturating_add(1);
                writes.push(candidate);
            }
        }

        let mut superseded = 0usize;
        for record in existing.iter().filter(|record| {
            app_record_belongs_to_installation(record, installation_id)
                && !active_ids.contains(record.surface_id.as_str())
        }) {
            if record.status == "superseded" {
                unchanged = unchanged.saturating_add(1);
                continue;
            }
            let timestamp = now.to_rfc3339();
            let mut stale = record.clone();
            stale.status = "superseded".to_owned();
            stale.unpublished_at = Some(timestamp.clone());
            stale.updated_at = timestamp;
            writes.push(stale);
            superseded = superseded.saturating_add(1);
        }

        Ok(PlannedAppPublicationReconciliation {
            receipt: AppPublicationReconciliationReceipt {
                installation_id: installation_id.clone(),
                surface_revision: Some(active.surface_revision),
                projected,
                superseded,
                unchanged,
            },
            writes,
        })
    }

    fn plan_inactive_snapshot(
        &self,
        installation_id: &AppInstallationId,
        _installation: Option<&AppInstallation>,
        existing: &[PublishedSurfaceRecord],
        now: &DateTime<Utc>,
    ) -> Result<PlannedAppPublicationReconciliation, AppPublicationReconciliationError> {
        let mut writes = Vec::new();
        let mut unchanged = 0usize;
        for record in existing
            .iter()
            .filter(|record| app_record_belongs_to_installation(record, installation_id))
        {
            if record.status == "unpublished" {
                unchanged = unchanged.saturating_add(1);
                continue;
            }
            let timestamp = now.to_rfc3339();
            let mut withdrawn = record.clone();
            withdrawn.status = "unpublished".to_owned();
            withdrawn.unpublished_at = Some(timestamp.clone());
            withdrawn.updated_at = timestamp;
            writes.push(withdrawn);
        }
        let superseded = writes.len();
        Ok(PlannedAppPublicationReconciliation {
            receipt: AppPublicationReconciliationReceipt {
                installation_id: installation_id.clone(),
                surface_revision: None,
                projected: 0,
                superseded,
                unchanged,
            },
            writes,
        })
    }

    async fn ensure_snapshot_current(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        expected: Option<&AppInstallation>,
        now: DateTime<Utc>,
    ) -> Result<(), AppPublicationReconciliationError> {
        let current = self
            .registry
            .installation(authenticated_scope, installation_id, now)
            .await?;
        let matches = match (expected, current.as_ref()) {
            (None, None) => true,
            (Some(expected), Some(current)) => expected == current,
            _ => false,
        };
        if matches {
            Ok(())
        } else {
            Err(AppPublicationReconciliationError::InstallationChanged)
        }
    }

    async fn all_snapshots_current_batched(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        snapshots: &[(AppInstallationId, Option<AppInstallation>)],
        now: DateTime<Utc>,
    ) -> Result<bool, AppPublicationReconciliationError> {
        let current =
            load_installation_snapshots_bounded(&self.registry, authenticated_scope, now).await?;
        for (installation_id, expected) in snapshots {
            if expected.as_ref() != current.get(installation_id) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn reconciliation_lock(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
    ) -> Arc<ReconciliationLock> {
        let key = (
            authenticated_scope.scope().principal.to_string(),
            authenticated_scope.scope().workspace.to_string(),
            installation_id.to_string(),
        );
        let mut registry = self
            .reconciliation_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if registry.len() > MAX_APP_PUBLICATION_REPAIR_INSTALLATIONS.saturating_mul(2) {
            registry.retain(|_, lock| lock.strong_count() != 0);
        }
        if let Some(lock) = registry.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(ReconciliationLock::new(()));
        registry.insert(key, Arc::downgrade(&lock));
        lock
    }

    fn emit_publication_events(
        &self,
        installation_id: &AppInstallationId,
        principal: &str,
        workspace: &str,
        records: &[PublishedSurfaceRecord],
    ) {
        let Some(broadcaster) = &self.event_broadcaster else {
            return;
        };
        let agent_id = format!("app:{}", installation_id.as_str());
        for record in records {
            broadcaster.emit_agent_transport_event(AgentEventEnvelope::new_scoped(
                PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
                &agent_id,
                principal,
                workspace,
                published_surface_changed_payload(record),
            ));
        }
    }
}

fn projected_record(
    authenticated_scope: &AuthenticatedAppScope,
    binding: &AppSurfaceBinding,
    surface_ref: &AppReference,
    existing: Option<&PublishedSurfaceRecord>,
    now: &DateTime<Utc>,
) -> PublishedSurfaceRecord {
    let timestamp = now.to_rfc3339();
    let mut candidate = PublishedSurfaceRecord {
        surface_id: surface_ref.to_string(),
        principal: authenticated_scope.scope().principal.to_string(),
        workspace: authenticated_scope.scope().workspace.to_string(),
        surface_kind: APP_SURFACE_KIND.to_owned(),
        status: "active".to_owned(),
        logical_surface_id: Some(surface_ref.to_string()),
        route: binding.canonical_host_route.clone(),
        document_key: format!(
            "app:{}:{}:{}",
            binding.installation_id.as_str(),
            binding.view_id.as_str(),
            binding.compiled_view_digest.as_str()
        ),
        task_id: None,
        ui_thread_id: None,
        source_output_id: None,
        source_execution_id: None,
        media_type: Some("application/vnd.magician.app-surface+json".to_owned()),
        materialized_render_kind: None,
        materialized_document_key: None,
        materialized_at: None,
        title: display_label(binding.view_id.as_str()),
        summary: Some("Installed app view".to_owned()),
        placement: PublishedSurfacePlacement {
            placement_kind: APP_SURFACE_PLACEMENT_KIND.to_owned(),
            placement_id: Some(binding.installation_id.to_string()),
            pinned: existing.is_some_and(|record| record.placement.pinned),
        },
        manifest_artifact_uid: None,
        manifest_name: None,
        input_artifact_ids: Vec::new(),
        published_at: existing
            .map(|record| record.published_at.clone())
            .unwrap_or_else(|| timestamp.clone()),
        unpublished_at: None,
        updated_at: timestamp,
    };
    if let Some(existing) = existing {
        let mut comparable = candidate.clone();
        comparable.updated_at = existing.updated_at.clone();
        if &comparable == existing {
            candidate.updated_at = existing.updated_at.clone();
        }
    }
    candidate
}

fn app_record_belongs_to_installation(
    record: &PublishedSurfaceRecord,
    installation_id: &AppInstallationId,
) -> bool {
    let route_prefix = format!("/apps/{}", installation_id.as_str());
    let document_prefix = format!("app:{}:", installation_id.as_str());
    let Some(document_identity) = record.document_key.strip_prefix(&document_prefix) else {
        return false;
    };
    let Some((view_id, compiled_digest)) = document_identity.split_once(':') else {
        return false;
    };
    let Ok(view_id) = super::models::AppName::parse(view_id.to_owned()) else {
        return false;
    };
    let Ok(_compiled_digest) = super::models::AppDigest::parse(compiled_digest.to_owned()) else {
        return false;
    };
    let Ok(expected_surface_ref) =
        super::surface_compiler::app_published_surface_ref(installation_id, &view_id)
    else {
        return false;
    };
    record.surface_kind == APP_SURFACE_KIND
        && record.placement.placement_kind == APP_SURFACE_PLACEMENT_KIND
        && record.placement.placement_id.as_deref() == Some(installation_id.as_str())
        && record.surface_id == expected_surface_ref.as_str()
        && record.logical_surface_id.as_deref() == Some(record.surface_id.as_str())
        && (record.route == route_prefix
            || record
                .route
                .strip_prefix(&route_prefix)
                .is_some_and(|suffix| suffix.starts_with('/')))
        && record.document_key.starts_with(&document_prefix)
        && record.task_id.is_none()
        && record.ui_thread_id.is_none()
        && record.source_output_id.is_none()
        && record.source_execution_id.is_none()
        && record.media_type.as_deref() == Some("application/vnd.magician.app-surface+json")
        && record.materialized_render_kind.is_none()
        && record.materialized_document_key.is_none()
        && record.materialized_at.is_none()
        && record.manifest_artifact_uid.is_none()
        && record.manifest_name.is_none()
        && record.input_artifact_ids.is_empty()
        && matches!(
            record.status.as_str(),
            "active" | "superseded" | "unpublished"
        )
}

fn record_claims_app_installation(record: &PublishedSurfaceRecord) -> bool {
    record.surface_kind == APP_SURFACE_KIND
        || record.placement.placement_kind == APP_SURFACE_PLACEMENT_KIND
}

fn ensure_reconciliation_record_capacity(
    existing: &[PublishedSurfaceRecord],
    writes: &[PublishedSurfaceRecord],
) -> Result<(), AppPublicationReconciliationError> {
    let mut identities = existing
        .iter()
        .map(|record| record.surface_id.as_str())
        .collect::<HashSet<_>>();
    for write in writes {
        identities.insert(write.surface_id.as_str());
        if identities.len() > MAX_APP_PUBLICATION_REPAIR_RECORDS {
            return Err(AppPublicationReconciliationError::RepairRecordLimitExceeded);
        }
    }
    Ok(())
}

fn ensure_record_scope(
    record: &PublishedSurfaceRecord,
    authenticated_scope: &AuthenticatedAppScope,
) -> Result<(), AppPublicationReconciliationError> {
    if record.principal == authenticated_scope.scope().principal.as_str()
        && record.workspace == authenticated_scope.scope().workspace.as_str()
    {
        Ok(())
    } else {
        Err(AppPublicationReconciliationError::PublicationScopeMismatch)
    }
}

fn validate_inventory(
    records: &[PublishedSurfaceRecord],
    authenticated_scope: &AuthenticatedAppScope,
) -> Result<(), AppPublicationReconciliationError> {
    for record in records {
        ensure_record_scope(record, authenticated_scope)?;
        if record_claims_app_installation(record) {
            let raw = record
                .placement
                .placement_id
                .as_deref()
                .ok_or(AppPublicationReconciliationError::InvalidOwnedSurfaceRecord)?;
            let installation_id = AppInstallationId::parse(raw.to_owned())?;
            if !app_record_belongs_to_installation(record, &installation_id) {
                return Err(AppPublicationReconciliationError::InvalidOwnedSurfaceRecord);
            }
        }
    }
    Ok(())
}

fn ensure_installation_scope(
    installation: &AppInstallation,
    authenticated_scope: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
) -> Result<(), AppPublicationReconciliationError> {
    if installation.scope == *authenticated_scope.scope()
        && installation.installation_id == *installation_id
    {
        Ok(())
    } else {
        Err(AppPublicationReconciliationError::InstallationScopeMismatch)
    }
}

fn ensure_binding_identity(
    binding: &AppSurfaceBinding,
    installation_id: &AppInstallationId,
    surface_revision: super::models::AppRevision,
) -> Result<(), AppPublicationReconciliationError> {
    let route_prefix = format!("/apps/{}", installation_id.as_str());
    let expected_surface_ref =
        super::surface_compiler::app_published_surface_ref(installation_id, &binding.view_id)?;
    if binding.installation_id != *installation_id
        || binding.surface_revision != surface_revision
        || binding.status != AppSurfaceStatus::Active
        || !(binding.canonical_host_route == route_prefix
            || binding
                .canonical_host_route
                .strip_prefix(&route_prefix)
                .is_some_and(|suffix| suffix.starts_with('/')))
        || binding.published_surface_ref.as_ref() != Some(&expected_surface_ref)
    {
        return Err(AppPublicationReconciliationError::InvalidActiveBindingIdentity);
    }
    Ok(())
}

async fn load_installation_snapshots_bounded(
    registry: &super::registry::AppRegistryService,
    authenticated_scope: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<BTreeMap<AppInstallationId, AppInstallation>, AppPublicationReconciliationError> {
    let limit = MAX_APP_PUBLICATION_REPAIR_INSTALLATIONS
        .checked_add(1)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(AppPublicationReconciliationError::RepairInstallationLimitExceeded)?;
    let max_document_bytes = i64::try_from(AppContractLimits::default().max_document_bytes())
        .map_err(|_| AppPublicationReconciliationError::RepairInstallationLimitExceeded)?;
    let snapshots = registry
        .execute_scoped_typed_read(authenticated_scope, &now, move |connection, scope| {
            let invalid_rows: i64 = connection.query_row(
                "SELECT COUNT(*) FROM app_installations
                  WHERE length(installation_id) NOT BETWEEN 1 AND 128
                     OR length(record_json) NOT BETWEEN 1 AND ?1",
                rusqlite::params![max_document_bytes],
                |row| row.get(0),
            )?;
            if invalid_rows != 0 {
                return Err(AppPublicationReconciliationError::CorruptRegistrySnapshot);
            }
            let mut statement = connection.prepare(
                "SELECT installation_id, record_json
                   FROM app_installations
                  WHERE length(installation_id) BETWEEN 1 AND 128
                    AND length(record_json) BETWEEN 1 AND ?2
                  ORDER BY installation_id
                  LIMIT ?1",
            )?;
            let mut rows = statement.query(rusqlite::params![limit, max_document_bytes])?;
            let mut snapshots = BTreeMap::new();
            while let Some(row) = rows.next()? {
                let installation_id = AppInstallationId::parse(row.get::<_, String>(0)?)?;
                let installation: AppInstallation =
                    decode_app_contract(&row.get::<_, Vec<u8>>(1)?, &AppContractLimits::default())?;
                if installation.installation_id != installation_id || installation.scope != *scope {
                    return Err(AppPublicationReconciliationError::InstallationScopeMismatch);
                }
                snapshots.insert(installation_id, installation);
            }
            Ok(snapshots)
        })
        .await?
        .unwrap_or_default();
    if snapshots.len() > MAX_APP_PUBLICATION_REPAIR_INSTALLATIONS {
        return Err(AppPublicationReconciliationError::RepairInstallationLimitExceeded);
    }
    Ok(snapshots)
}

fn display_label(value: &str) -> String {
    value
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + characters.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Error)]
pub enum AppPublicationReconciliationError {
    #[error("app installation does not exist in this authenticated scope")]
    InstallationNotFound,
    #[error("app installation is not active")]
    InstallationNotActive,
    #[error("app installation changed before publication writes were fenced")]
    InstallationChanged,
    #[error("durable lifecycle event is ahead of its installation head")]
    LifecycleEventAhead,
    #[error("durable lifecycle event differs from its exact installation generation")]
    LifecycleEventMismatch,
    #[error("app installation does not exactly belong to the authenticated scope")]
    InstallationScopeMismatch,
    #[error("published-surface record does not exactly belong to the authenticated scope")]
    PublicationScopeMismatch,
    #[error("app-owned published-surface record has incomplete or conflicting ownership markers")]
    InvalidOwnedSurfaceRecord,
    #[error("active app binding has inconsistent installation, generation or route identity")]
    InvalidActiveBindingIdentity,
    #[error("authoritative published-surface files include unreadable records")]
    UnreadableAuthoritativeRecords,
    #[error("bounded app publication repair installation limit was exceeded")]
    RepairInstallationLimitExceeded,
    #[error("bounded app publication repair record limit was exceeded")]
    RepairRecordLimitExceeded,
    #[error("app registry contains an unbounded or corrupt installation snapshot")]
    CorruptRegistrySnapshot,
    #[error("active app surface is missing its deterministic published-surface reference")]
    MissingPublishedSurfaceReference,
    #[error("active app surfaces contain a duplicate published-surface reference")]
    DuplicatePublishedSurfaceReference,
    #[error("an app published-surface identity collides with a foreign publication")]
    ForeignSurfaceIdentityCollision,
    #[error(transparent)]
    Contract(#[from] super::models::AppContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Hydration(#[from] AppSurfaceHydrationError),
    #[error(transparent)]
    Registry(#[from] super::registry::AppRegistryError),
    #[error(transparent)]
    Publication(#[from] ArtifactV2Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::{
        apps::{
            models::{AppDigest, AppName, AppRevision},
            records::AppSurfaceStatus,
        },
        artifact_v2::models::PublishedSurfaceIndexRecord,
    };

    #[tokio::test]
    async fn active_binding_generation_reconciles_to_taskless_shared_records() {
        let temporary = magician::magician_v2::apps::registry::tests::canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let registry = super::super::registry::AppRegistryService::new(workspace.clone());
        let (package_schema_digest, schema) =
            magician::magician_v2::apps::entity_store::tests::compiled_schema();
        magician::magician_v2::apps::entity_store::tests::seed_enabled_installation(
            &registry,
            package_schema_digest,
            schema,
            super::super::lifecycle::AppInstallationStatus::Enabled,
        )
        .await;
        let authenticated = magician::magician_v2::apps::registry::tests::authenticated_scope(
            "anonymous",
            "default",
        );
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let receipt = AppPublicationReconciler::new(registry, workspace.clone())
            .reconcile_active_installation(
                &authenticated,
                &installation_id,
                magician::magician_v2::apps::registry::tests::time(4),
            )
            .await
            .unwrap();
        assert!(receipt.projected > 0);
        let records = FilesystemPublishedSurfaceStore::new(workspace)
            .list_surfaces("anonymous", "default")
            .await
            .unwrap();
        assert_eq!(records.len(), receipt.projected);
        assert!(records.iter().all(|record| {
            record.surface_kind == "app"
                && record.task_id.is_none()
                && record.source_execution_id.is_none()
        }));
    }

    #[tokio::test]
    async fn disabled_installation_withdraws_only_its_owned_publications() {
        let temporary = magician::magician_v2::apps::registry::tests::canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let registry = super::super::registry::AppRegistryService::new(workspace.clone());
        let (package_schema_digest, schema) =
            magician::magician_v2::apps::entity_store::tests::compiled_schema();
        magician::magician_v2::apps::entity_store::tests::seed_enabled_installation(
            &registry,
            package_schema_digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        let authenticated = magician::magician_v2::apps::registry::tests::authenticated_scope(
            "anonymous",
            "default",
        );
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let reconciler = AppPublicationReconciler::new(registry.clone(), workspace.clone());
        reconciler
            .reconcile_installation(
                &authenticated,
                &installation_id,
                magician::magician_v2::apps::registry::tests::time(4),
            )
            .await
            .unwrap();

        let mut installation = registry
            .installation(
                &authenticated,
                &installation_id,
                magician::magician_v2::apps::registry::tests::time(5),
            )
            .await
            .unwrap()
            .unwrap();
        let enabled_snapshot = installation.clone();
        installation.lifecycle.status = AppInstallationStatus::Disabled;
        installation.disabled_at = Some(magician::magician_v2::apps::registry::tests::time(5));
        installation.updated_at = magician::magician_v2::apps::registry::tests::time(5);
        let record_json = serde_json::to_vec(&installation).unwrap();
        let installation_for_write = installation_id.clone();
        registry
            .execute_scoped_write(
                &authenticated,
                &magician::magician_v2::apps::registry::tests::time(5),
                move |connection, _| {
                    connection.execute(
                        "UPDATE app_installations
                            SET lifecycle_status = 'disabled', record_json = ?2
                          WHERE installation_id = ?1",
                        rusqlite::params![installation_for_write.as_str(), record_json],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert!(matches!(
            reconciler
                .ensure_snapshot_current(
                    &authenticated,
                    &installation_id,
                    Some(&enabled_snapshot),
                    magician::magician_v2::apps::registry::tests::time(6),
                )
                .await,
            Err(AppPublicationReconciliationError::InstallationChanged)
        ));

        let receipt = reconciler
            .reconcile_installation(
                &authenticated,
                &installation_id,
                magician::magician_v2::apps::registry::tests::time(6),
            )
            .await
            .unwrap();
        assert!(receipt.superseded > 0);
        assert!(receipt.surface_revision.is_none());
        assert!(matches!(
            reconciler
                .reconcile_active_installation(
                    &authenticated,
                    &installation_id,
                    magician::magician_v2::apps::registry::tests::time(6),
                )
                .await,
            Err(AppPublicationReconciliationError::InstallationNotActive)
        ));
        let records = FilesystemPublishedSurfaceStore::new(workspace)
            .list_surfaces("anonymous", "default")
            .await
            .unwrap();
        assert!(records.iter().all(|record| record.status == "unpublished"));
    }

    #[tokio::test]
    async fn boot_repair_restores_authoritative_records_and_removes_orphan_index_entries() {
        let temporary = magician::magician_v2::apps::registry::tests::canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let registry = super::super::registry::AppRegistryService::new(workspace.clone());
        let (package_schema_digest, schema) =
            magician::magician_v2::apps::entity_store::tests::compiled_schema();
        magician::magician_v2::apps::entity_store::tests::seed_enabled_installation(
            &registry,
            package_schema_digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        let authenticated = magician::magician_v2::apps::registry::tests::authenticated_scope(
            "anonymous",
            "default",
        );
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let reconciler = AppPublicationReconciler::new(registry, workspace.clone());
        reconciler
            .reconcile_installation(
                &authenticated,
                &installation_id,
                magician::magician_v2::apps::registry::tests::time(4),
            )
            .await
            .unwrap();

        let store = FilesystemPublishedSurfaceStore::new(workspace.clone());
        let mut shared_task_record = store
            .list_surfaces("anonymous", "default")
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        shared_task_record.surface_id = "shared-task-surface".to_owned();
        shared_task_record.surface_kind = "task".to_owned();
        shared_task_record.logical_surface_id = Some("shared-task-surface".to_owned());
        shared_task_record.route = "/today".to_owned();
        shared_task_record.document_key = "task:shared".to_owned();
        shared_task_record.task_id = Some("task-shared".to_owned());
        shared_task_record.media_type = Some("application/json".to_owned());
        shared_task_record.placement = PublishedSurfacePlacement {
            placement_kind: "briefing".to_owned(),
            placement_id: None,
            pinned: true,
        };
        store.upsert_surface(&shared_task_record).await.unwrap();

        let index_path = workspace.published_surfaces_index_path("anonymous", "default");
        let mut index: PublishedSurfaceIndexRecord =
            workspace.read_json_path(&index_path).await.unwrap();
        let mut phantom = index.surfaces[0].clone();
        phantom.surface_id = "orphan-index-entry".to_owned();
        index.surfaces = vec![phantom];
        workspace
            .write_json_atomic_path(&index_path, &index)
            .await
            .unwrap();

        reconciler
            .repair_scope(
                &authenticated,
                magician::magician_v2::apps::registry::tests::time(5),
            )
            .await
            .unwrap();
        let repaired: PublishedSurfaceIndexRecord =
            workspace.read_json_path(&index_path).await.unwrap();
        assert!(repaired
            .surfaces
            .iter()
            .any(|entry| entry.surface_kind == "app"
                && entry.placement.placement_kind == "apps"
                && entry.placement.placement_id.as_deref() == Some("install_1")
                && (entry.route == "/apps/install_1"
                    || entry.route.starts_with("/apps/install_1/"))));
        assert!(repaired
            .surfaces
            .iter()
            .all(|entry| entry.surface_id != "orphan-index-entry"));
        assert!(repaired
            .surfaces
            .iter()
            .any(|entry| entry.surface_id == "shared-task-surface"
                && entry.surface_kind == "task"
                && entry.task_id.as_deref() == Some("task-shared")));
    }

    #[tokio::test]
    async fn lifecycle_event_ahead_of_registry_head_writes_no_publication() {
        let temporary = magician::magician_v2::apps::registry::tests::canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let registry = super::super::registry::AppRegistryService::new(workspace.clone());
        let (package_schema_digest, schema) =
            magician::magician_v2::apps::entity_store::tests::compiled_schema();
        magician::magician_v2::apps::entity_store::tests::seed_enabled_installation(
            &registry,
            package_schema_digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        let authenticated = magician::magician_v2::apps::registry::tests::authenticated_scope(
            "anonymous",
            "default",
        );
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let installation = registry
            .installation(
                &authenticated,
                &installation_id,
                magician::magician_v2::apps::registry::tests::time(4),
            )
            .await
            .unwrap()
            .unwrap();
        let event = AppLifecycleOutboxEvent {
            event_id: AppReference::parse("event:ahead").unwrap(),
            installation_id: installation_id.clone(),
            package_revision_ref: installation.package_revision_ref.clone(),
            installation_generation: installation.lifecycle.generation + 1,
            event_kind:
                super::super::registry_lifecycle::AppLifecycleEventKind::InstallationUpdated,
            lifecycle_status: AppInstallationStatus::Enabled,
            grant_revision: installation.grant_revision,
            schema_revision: installation.active_schema_revision,
            surface_revision: installation.active_surface_revision,
            reenable_review_identity: None,
            occurred_at: magician::magician_v2::apps::registry::tests::time(4),
        };
        let error = AppPublicationReconciler::new(registry, workspace.clone())
            .reconcile_lifecycle_event(
                &authenticated,
                &event,
                magician::magician_v2::apps::registry::tests::time(5),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppPublicationReconciliationError::LifecycleEventAhead
        ));
        assert!(FilesystemPublishedSurfaceStore::new(workspace)
            .list_surfaces("anonymous", "default")
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn app_projection_has_no_fabricated_task_or_execution_owner() {
        let authenticated = magician::magician_v2::apps::registry::tests::authenticated_scope(
            "anonymous",
            "default",
        );
        let installation_id = AppInstallationId::parse("install_projection").unwrap();
        let binding = AppSurfaceBinding {
            installation_id: installation_id.clone(),
            surface_revision: AppRevision::new(2).unwrap(),
            package_revision_ref: AppReference::parse("package:test").unwrap(),
            app_local_route: "/items".to_owned(),
            canonical_host_route: "/apps/install_projection/items".to_owned(),
            view_id: AppName::parse("my_items").unwrap(),
            compiled_view_digest: AppDigest::blake3(b"view"),
            published_surface_ref: Some(
                super::super::surface_compiler::app_published_surface_ref(
                    &installation_id,
                    &AppName::parse("my_items").unwrap(),
                )
                .unwrap(),
            ),
            status: AppSurfaceStatus::Active,
        };
        let record = projected_record(
            &authenticated,
            &binding,
            binding.published_surface_ref.as_ref().unwrap(),
            None,
            &Utc::now(),
        );
        assert_eq!(record.surface_kind, "app");
        assert_eq!(record.placement.placement_kind, "apps");
        assert!(record.route.starts_with("/apps/"));
        assert_ne!(record.route, "/briefing");
        assert_eq!(record.title, "My Items");
        assert!(record.task_id.is_none());
        assert!(record.ui_thread_id.is_none());
        assert!(record.source_output_id.is_none());
        assert!(record.source_execution_id.is_none());
    }

    #[test]
    fn reconciliation_ownership_requires_all_app_projection_markers() {
        let installation_id = AppInstallationId::parse("install_projection").unwrap();
        let authenticated = magician::magician_v2::apps::registry::tests::authenticated_scope(
            "anonymous",
            "default",
        );
        let binding = AppSurfaceBinding {
            installation_id: installation_id.clone(),
            surface_revision: super::super::models::AppRevision::new(2).unwrap(),
            package_revision_ref: AppReference::parse("package:test").unwrap(),
            app_local_route: "/".to_owned(),
            canonical_host_route: "/apps/install_projection".to_owned(),
            view_id: super::super::models::AppName::parse("home").unwrap(),
            compiled_view_digest: super::super::models::AppDigest::blake3(b"home"),
            published_surface_ref: Some(
                super::super::surface_compiler::app_published_surface_ref(
                    &installation_id,
                    &AppName::parse("home").unwrap(),
                )
                .unwrap(),
            ),
            status: super::super::records::AppSurfaceStatus::Active,
        };
        let mut record = projected_record(
            &authenticated,
            &binding,
            binding.published_surface_ref.as_ref().unwrap(),
            None,
            &Utc::now(),
        );
        assert!(app_record_belongs_to_installation(
            &record,
            &installation_id
        ));
        record.task_id = Some("task-real".to_owned());
        assert!(!app_record_belongs_to_installation(
            &record,
            &installation_id
        ));
    }

    #[test]
    fn reconciliation_refuses_to_grow_the_authoritative_inventory_past_its_cap() {
        let installation_id = AppInstallationId::parse("install_projection").unwrap();
        let authenticated = magician::magician_v2::apps::registry::tests::authenticated_scope(
            "anonymous",
            "default",
        );
        let binding = AppSurfaceBinding {
            installation_id: installation_id.clone(),
            surface_revision: AppRevision::new(2).unwrap(),
            package_revision_ref: AppReference::parse("package:test").unwrap(),
            app_local_route: "/".to_owned(),
            canonical_host_route: "/apps/install_projection".to_owned(),
            view_id: AppName::parse("home").unwrap(),
            compiled_view_digest: AppDigest::blake3(b"home"),
            published_surface_ref: Some(
                super::super::surface_compiler::app_published_surface_ref(
                    &installation_id,
                    &AppName::parse("home").unwrap(),
                )
                .unwrap(),
            ),
            status: AppSurfaceStatus::Active,
        };
        let template = projected_record(
            &authenticated,
            &binding,
            binding.published_surface_ref.as_ref().unwrap(),
            None,
            &Utc::now(),
        );
        let existing = (0..MAX_APP_PUBLICATION_REPAIR_RECORDS)
            .map(|index| {
                let mut record = template.clone();
                record.surface_id = format!("existing-{index}");
                record
            })
            .collect::<Vec<_>>();
        let mut addition = template;
        addition.surface_id = "one-more".to_owned();
        assert!(matches!(
            ensure_reconciliation_record_capacity(&existing, &[addition]),
            Err(AppPublicationReconciliationError::RepairRecordLimitExceeded)
        ));
        assert!(ensure_reconciliation_record_capacity(&existing, &[existing[0].clone()]).is_ok());
    }
}
