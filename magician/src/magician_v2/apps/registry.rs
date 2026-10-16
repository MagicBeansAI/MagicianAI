//! Scoped durable package and installation registry.
//!
//! The registry is lazy: constructing [`AppRegistryService`] does not touch the
//! filesystem or scan scopes. Every operation requires server-minted
//! [`AuthenticatedAppScope`] evidence, resolves exactly one per-scope database,
//! and performs synchronous SQLite work on Tokio's blocking pool. Initial
//! publication commits the immutable package revision, conformance attempt and
//! reviewable installation in one immediate transaction.

mod admission;
mod connection_pool;
mod encryption_recovery;
mod maintenance;
mod metrics;
mod recurring;

pub use connection_pool::{ConnectionPoolStats, RegistryConnectionLease};
pub(crate) use encryption_recovery::recover_legacy_fixture_encryption;
pub use maintenance::{AppStoreMaintenanceAction, AppStoreMaintenanceReport};
pub use metrics::RegistrySqlStats;

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use super::{
    approval_boundary::AppInstallationApprovalError,
    authority::{AppAuthorityError, AuthenticatedAppScope, ResolvedAppAuthority},
    capability_catalog::{
        compile_computed_capability_catalog, compile_scope_computed_capability_overlay,
        AppComputedCapabilityAdmission, AppComputedCapabilityDocument,
        AppComputedCapabilityScopeOverlay,
    },
    contribution_terminal::AppPreparedTerminalContribution,
    lifecycle::{AppInstallationStatus, AppLifecycleAttemptState, AppLifecycleError},
    manifest::{canonical_view_schema_digest, AppDependencyKind, AppPackageLimits},
    models::{
        decode_app_contract, validate_json_value, AppContractError, AppContractLimits, AppDigest,
        AppInstallationId, AppName, AppReference, AppRevision, ValidateAppContract,
    },
    package_lock::{
        decode_persisted_package_lock, AppLockedDependencySource, AppPackageLock,
        AppPackageLockError,
    },
    package_staging::{
        ensure_app_scope_binding, verify_staged_package_snapshot, AppPackageStagingError,
        StagedAppPackage,
    },
    records::{
        AppGrantRevision, AppInstallation, AppInstallationApproval, AppLifecycleAttempt,
        AppLifecycleAttemptKind, AppPackageDirectoryMetadata, AppPackageRevision,
        AppSchemaRevision, AppScope,
    },
    resource_authority::AppResourceAcceptedSettlementPermit,
    schema_compiler::canonical_entity_schema_digest,
    skill_dependencies::{AppStandaloneProcedureCandidate, AppVerifiedRegistryProcedureRevision},
};
use crate::magician_v2::{
    artifact_v2::{io::publish_staged_file_durably_sync, workspace::ArtifactV2Workspace},
    json_traversal::canonical_json_bytes,
    secrets::encryption::{
        app_data_scope_key_candidates, rotate_app_data_root_key, AppDataScopeKey,
    },
};

const APP_REGISTRY_SCHEMA_V36: &str = r#"
CREATE TABLE app_data_cleanup_jobs (
    job_ref TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    record_json BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (installation_id) REFERENCES app_installations(installation_id)
) STRICT;
CREATE INDEX app_data_cleanup_jobs_installation_idx
    ON app_data_cleanup_jobs(installation_id, updated_at DESC);
CREATE TABLE app_data_cleanup_candidates (
    job_ref TEXT NOT NULL,
    record_id TEXT NOT NULL,
    record_revision INTEGER NOT NULL,
    dataset_generation INTEGER NOT NULL,
    payload_bytes INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    PRIMARY KEY (job_ref, record_id),
    FOREIGN KEY (job_ref) REFERENCES app_data_cleanup_jobs(job_ref) ON DELETE CASCADE
) STRICT;
CREATE INDEX app_data_cleanup_candidates_pending_idx
    ON app_data_cleanup_candidates(job_ref, state, record_id);
"#;

const APP_REGISTRY_SCHEMA_V37: &str = r#"
CREATE TABLE app_recurring_occurrence_locators (
    occurrence_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    sealed_blob BLOB NOT NULL,
    UNIQUE (task_id, execution_id)
) STRICT;
CREATE TRIGGER app_recurring_occurrence_immutable BEFORE UPDATE ON app_recurring_occurrence_locators
BEGIN SELECT RAISE(ABORT, 'immutable recurring occurrence'); END;
CREATE TABLE app_recurring_task_heads (
    task_id TEXT PRIMARY KEY,
    occurrence_id TEXT NOT NULL,
    FOREIGN KEY (occurrence_id) REFERENCES app_recurring_occurrence_locators(occurrence_id)
) STRICT;
CREATE TABLE app_behavior_execution_state (
    installation_id TEXT NOT NULL,
    behavior_id TEXT NOT NULL,
    record_json BLOB NOT NULL,
    needs_observation INTEGER NOT NULL DEFAULT 0 CHECK (needs_observation IN (0, 1)),
    PRIMARY KEY (installation_id, behavior_id),
    FOREIGN KEY (installation_id) REFERENCES app_installations(installation_id)
) STRICT;
CREATE INDEX app_behavior_execution_observation_idx
    ON app_behavior_execution_state(needs_observation, installation_id, behavior_id);
"#;

/// Owner-edited app memory grants (`app_memory_read_v1`). One row per
/// installation, compare-and-swapped on `revision`; it applies only while
/// `grant_revision` matches the installation's current grant revision.
const APP_REGISTRY_SCHEMA_V38: &str = r#"
CREATE TABLE IF NOT EXISTS app_memory_read_grant_heads (
    installation_id TEXT PRIMARY KEY,
    grant_revision INTEGER NOT NULL CHECK (grant_revision > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    grant_json BLOB NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (installation_id) REFERENCES app_installations(installation_id)
) STRICT;
"#;

const APP_REGISTRY_SCHEMA_VERSION: i32 = 38;
// Development builds perform the one-time upgrade before this code is deployed.
// Keep the current schema as the only readable steady-state format; historical
// versioned fragments below are composition pieces for creating a fresh store.
const APP_REGISTRY_MINIMUM_READABLE_SCHEMA_VERSION: i32 = APP_REGISTRY_SCHEMA_VERSION;
const APP_REGISTRY_APPLICATION_ID: i32 = 0x4d41_5050; // "MAPP"
const APP_REGISTRY_CIPHER_FORMAT_VERSION: i32 = 1;
const APP_REGISTRY_CIPHER_ALGORITHM: &str = "sqlcipher-4-aes256cbc-hmac-sha512";
/// Concurrent blocking registry operations.
///
/// Sized at 4 when app background behaviors were disarmed, which meant the
/// registry served foreground reads and installs only. Arming them adds a
/// scheduler that walks *every* scope each tick, so the same permits now also
/// carry per-scope claims and installation reconciliation.
///
/// Raised to 8 while chasing the Town Square capacity warnings. Measure it
/// before crediting it: on its own the raise did **not** reduce them (8.1% of
/// log lines before, 9.7% after — noise). What removed them was batching the
/// per-package revision reads in `package_revisions` so the Town Square feed
/// takes one admission for a page instead of one per post. Raising the ceiling
/// buys headroom against a demand that batching had not yet removed; it is not
/// the fix, and a future reader should not read it as one.
///
/// These are SQLite operations on the tokio blocking pool, whose default is far
/// larger, so the constraint this expresses is database contention rather than
/// threads. WAL readers do not block each other; writers are already serialised
/// by the per-scope write guard above this.
const DEFAULT_BLOCKING_OPERATIONS: usize = 8;
// This bounds retained idle handles, not Apps or accepted work. Active handles
// remain governed by admission and are returned after synchronous database I/O.
const MAX_IDLE_REGISTRY_CONNECTIONS: usize = 32;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
/// Admission waits asynchronously under the caller's cancellation/deadline.
/// The registry adds no short timeout that turns capacity contention into an
/// App failure. Writes acquire the
/// scope guard and blocking slot as a pair, never holding either while waiting
/// for the other. A 250ms read window and fail-fast writes turned brief overlap
/// between two App runs and background scans into terminal workflow failures.
/// This changes queueing, not the eight-operation concurrency ceiling.
const MAX_SCHEMA_READY_SCOPES: usize = 1_024;
const MAX_WORKFLOW_CONTROL_BLOB_BYTES: usize = 64 * 1024 * 1024;
const WORKFLOW_PAUSE_CLAIM_LEASE_SECONDS: i64 = 10 * 60;
const WORKFLOW_PAUSE_PREPARE_LEASE_SECONDS: i64 = 10 * 60;

const APP_REGISTRY_SCHEMA: &str = r#"
CREATE TABLE app_registry_scope (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL
) STRICT;

CREATE TABLE app_package_revisions (
    package_revision_ref TEXT PRIMARY KEY,
    package_id TEXT NOT NULL,
    semantic_version TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    publisher_identity TEXT NOT NULL,
    dependency_lock_digest TEXT NOT NULL,
    record_json BLOB NOT NULL,
    dependency_lock_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (package_id, semantic_version, content_digest)
) STRICT;

CREATE TABLE app_lifecycle_attempts (
    attempt_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    installation_id TEXT,
    candidate_package_revision_ref TEXT NOT NULL,
    state TEXT NOT NULL,
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (candidate_package_revision_ref)
        REFERENCES app_package_revisions(package_revision_ref)
) STRICT;

CREATE TABLE app_installations (
    installation_id TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    package_revision_ref TEXT NOT NULL,
    lifecycle_status TEXT NOT NULL,
    lifecycle_generation INTEGER NOT NULL CHECK (lifecycle_generation > 0),
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (package_revision_ref)
        REFERENCES app_package_revisions(package_revision_ref)
) STRICT;

CREATE INDEX app_package_revisions_package_idx
    ON app_package_revisions(package_id, semantic_version, created_at);
CREATE INDEX app_lifecycle_attempts_package_idx
    ON app_lifecycle_attempts(candidate_package_revision_ref, created_at);
CREATE INDEX app_installations_package_idx
    ON app_installations(package_revision_ref, created_at);
"#;

const APP_REGISTRY_SCHEMA_V2: &str = r#"
CREATE TABLE app_grant_revisions (
    installation_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    package_revision_ref TEXT NOT NULL,
    authority_digest TEXT NOT NULL,
    granted_data_policy_digest TEXT NOT NULL,
    revoked_at TEXT,
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, revision),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id),
    FOREIGN KEY (package_revision_ref)
        REFERENCES app_package_revisions(package_revision_ref)
) STRICT;

CREATE TABLE app_schema_revisions (
    installation_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    package_revision_ref TEXT NOT NULL,
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, revision),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id),
    FOREIGN KEY (package_revision_ref)
        REFERENCES app_package_revisions(package_revision_ref)
) STRICT;

CREATE TABLE app_surface_bindings (
    installation_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    package_revision_ref TEXT NOT NULL,
    status TEXT NOT NULL,
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, revision),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id),
    FOREIGN KEY (package_revision_ref)
        REFERENCES app_package_revisions(package_revision_ref)
) STRICT;

CREATE TABLE app_installation_approvals (
    approval_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    attempt_id TEXT NOT NULL,
    package_content_digest TEXT NOT NULL,
    requested_authority_digest TEXT NOT NULL,
    granted_authority_digest TEXT NOT NULL,
    session_ref TEXT NOT NULL,
    authentication_revision INTEGER NOT NULL CHECK (authentication_revision > 0),
    expires_at TEXT NOT NULL,
    consumed_at TEXT,
    consumed_installation_revision INTEGER,
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (approval_id, revision),
    FOREIGN KEY (attempt_id)
        REFERENCES app_lifecycle_attempts(attempt_id)
) STRICT;

CREATE TABLE app_lifecycle_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    idempotency_key TEXT NOT NULL UNIQUE,
    installation_id TEXT NOT NULL,
    installation_generation INTEGER NOT NULL CHECK (installation_generation > 0),
    event_kind TEXT NOT NULL,
    payload_json BLOB NOT NULL,
    delivery_state TEXT NOT NULL CHECK (delivery_state IN ('pending', 'leased', 'delivered')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    available_at TEXT NOT NULL,
    lease_owner TEXT,
    lease_token TEXT,
    lease_expires_at TEXT,
    delivery_receipt_id TEXT,
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE INDEX app_grant_revisions_active_idx
    ON app_grant_revisions(installation_id, revision, revoked_at);
CREATE INDEX app_schema_revisions_active_idx
    ON app_schema_revisions(installation_id, revision);
CREATE INDEX app_surface_bindings_active_idx
    ON app_surface_bindings(installation_id, revision, status);
CREATE INDEX app_installation_approvals_attempt_idx
    ON app_installation_approvals(attempt_id, revision, consumed_at);
CREATE INDEX app_lifecycle_outbox_ready_idx
    ON app_lifecycle_outbox(delivery_state, available_at, sequence);
CREATE UNIQUE INDEX app_package_revision_version_immutable_idx
    ON app_package_revisions(package_id, semantic_version);
"#;

// Phase 2 extends the existing per-scope app store. These tables deliberately
// live in the registry-owned database: opening one database or WAL per
// installation would multiply idle resources and introduce a second scope
// authority. The schema is additive and no entity operation is route-reachable
// until its own Phase-2 gate lands.
const APP_REGISTRY_SCHEMA_V3: &str = r#"
CREATE TABLE app_record_revisions (
    installation_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    record_revision INTEGER NOT NULL CHECK (record_revision > 0),
    dataset_generation INTEGER NOT NULL CHECK (dataset_generation > 0),
    schema_revision INTEGER NOT NULL CHECK (schema_revision > 0),
    payload_digest TEXT NOT NULL,
    payload_json BLOB NOT NULL,
    handling_policy_digest TEXT NOT NULL,
    handling_policy_json BLOB NOT NULL,
    provenance_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT,
    PRIMARY KEY (installation_id, entity_name, record_id, record_revision),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_record_heads (
    installation_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    record_revision INTEGER NOT NULL CHECK (record_revision > 0),
    dataset_generation INTEGER NOT NULL CHECK (dataset_generation > 0),
    schema_revision INTEGER NOT NULL CHECK (schema_revision > 0),
    change_seq INTEGER NOT NULL CHECK (change_seq > 0),
    deleted_at TEXT,
    PRIMARY KEY (installation_id, entity_name, record_id),
    FOREIGN KEY (installation_id, entity_name, record_id, record_revision)
        REFERENCES app_record_revisions(
            installation_id, entity_name, record_id, record_revision
        )
) STRICT;

CREATE TABLE app_scalar_indexes (
    installation_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    field_path TEXT NOT NULL,
    record_id TEXT NOT NULL,
    record_revision INTEGER NOT NULL CHECK (record_revision > 0),
    value_kind TEXT NOT NULL CHECK (
        value_kind IN ('null', 'text', 'enum', 'integer', 'decimal', 'boolean', 'timestamp', 'reference')
    ),
    text_value TEXT,
    integer_value INTEGER,
    real_value REAL,
    PRIMARY KEY (installation_id, entity_name, field_path, record_id),
    FOREIGN KEY (installation_id, entity_name, record_id, record_revision)
        REFERENCES app_record_revisions(
            installation_id, entity_name, record_id, record_revision
        )
) STRICT;

CREATE TABLE app_text_search (
    installation_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    field_path TEXT NOT NULL,
    record_id TEXT NOT NULL,
    record_revision INTEGER NOT NULL CHECK (record_revision > 0),
    search_text TEXT NOT NULL,
    PRIMARY KEY (installation_id, entity_name, field_path, record_id),
    FOREIGN KEY (installation_id, entity_name, record_id, record_revision)
        REFERENCES app_record_revisions(
            installation_id, entity_name, record_id, record_revision
        )
) STRICT;

CREATE TABLE app_mutation_receipts (
    receipt_id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    mutation_digest TEXT NOT NULL,
    first_change_seq INTEGER NOT NULL CHECK (first_change_seq > 0),
    last_change_seq INTEGER NOT NULL CHECK (last_change_seq >= first_change_seq),
    record_json BLOB NOT NULL,
    committed_at TEXT NOT NULL,
    UNIQUE (installation_id, idempotency_key),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_installation_sequences (
    installation_id TEXT PRIMARY KEY,
    next_change_seq INTEGER NOT NULL CHECK (next_change_seq > 0),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_dataset_generations (
    installation_id TEXT PRIMARY KEY,
    current_generation INTEGER NOT NULL CHECK (current_generation > 0),
    updated_at TEXT NOT NULL,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_query_cursors (
    cursor_ref TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    schema_revision INTEGER NOT NULL CHECK (schema_revision > 0),
    dataset_generation INTEGER NOT NULL CHECK (dataset_generation > 0),
    evidence_json BLOB NOT NULL,
    snapshot_json BLOB NOT NULL,
    next_offset INTEGER NOT NULL CHECK (next_offset >= 0),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_storage_usage (
    installation_id TEXT PRIMARY KEY,
    record_count INTEGER NOT NULL CHECK (record_count >= 0),
    revision_count INTEGER NOT NULL CHECK (revision_count >= 0),
    payload_bytes INTEGER NOT NULL CHECK (payload_bytes >= 0),
    attachment_bytes INTEGER NOT NULL CHECK (attachment_bytes >= 0),
    updated_at TEXT NOT NULL,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_resource_usage_projection (
    installation_id TEXT PRIMARY KEY,
    authority_revision INTEGER NOT NULL CHECK (authority_revision > 0),
    projection_json BLOB NOT NULL,
    projected_at TEXT NOT NULL,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_entity_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    installation_id TEXT NOT NULL,
    first_change_seq INTEGER NOT NULL CHECK (first_change_seq > 0),
    last_change_seq INTEGER NOT NULL CHECK (last_change_seq >= first_change_seq),
    payload_json BLOB NOT NULL,
    delivery_state TEXT NOT NULL CHECK (delivery_state IN ('pending', 'leased', 'delivered')),
    available_at TEXT NOT NULL,
    lease_token TEXT,
    lease_expires_at TEXT,
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_migration_runs (
    migration_run_id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    source_schema_revision INTEGER NOT NULL CHECK (source_schema_revision > 0),
    target_schema_revision INTEGER NOT NULL CHECK (target_schema_revision > 0),
    state TEXT NOT NULL,
    next_batch INTEGER NOT NULL CHECK (next_batch >= 0),
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE INDEX app_record_revisions_history_idx
    ON app_record_revisions(installation_id, entity_name, record_id, record_revision DESC);
CREATE INDEX app_record_heads_query_idx
    ON app_record_heads(installation_id, entity_name, dataset_generation, change_seq, record_id);
CREATE INDEX app_scalar_indexes_text_idx
    ON app_scalar_indexes(installation_id, entity_name, field_path, text_value, record_id);
CREATE INDEX app_scalar_indexes_integer_idx
    ON app_scalar_indexes(installation_id, entity_name, field_path, integer_value, record_id);
CREATE INDEX app_scalar_indexes_real_idx
    ON app_scalar_indexes(installation_id, entity_name, field_path, real_value, record_id);
CREATE INDEX app_scalar_indexes_reference_target_idx
    ON app_scalar_indexes(installation_id, value_kind, text_value, entity_name, field_path, record_id);
CREATE INDEX app_text_search_lookup_idx
    ON app_text_search(installation_id, entity_name, field_path, record_id);
CREATE INDEX app_entity_outbox_ready_idx
    ON app_entity_outbox(delivery_state, available_at, sequence);
CREATE INDEX app_migration_runs_installation_idx
    ON app_migration_runs(installation_id, state, updated_at);
CREATE INDEX app_query_cursors_expiry_idx
    ON app_query_cursors(expires_at, installation_id);
"#;

// Phase 2D keeps data transfer, reviewed import and retention evidence in the
// same scoped owner as records. These rows are evidence only: they never carry
// grants, credentials, package authority or source-scope identities.
const APP_REGISTRY_SCHEMA_V4: &str = r#"
CREATE TABLE app_data_import_receipts (
    receipt_ref TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    import_batch_key TEXT NOT NULL,
    source_archive_digest TEXT NOT NULL,
    preview_digest TEXT NOT NULL,
    record_json BLOB NOT NULL,
    committed_at TEXT NOT NULL,
    UNIQUE (installation_id, import_batch_key),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_purge_receipts (
    receipt_ref TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    approval_ref TEXT NOT NULL UNIQUE,
    preview_digest TEXT NOT NULL,
    selection_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending_checkpoint', 'completed')),
    record_json BLOB NOT NULL,
    started_at TEXT NOT NULL,
    committed_at TEXT,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_retention_policies (
    installation_id TEXT NOT NULL,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    policy_digest TEXT NOT NULL,
    record_json BLOB NOT NULL,
    activated_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, policy_revision),
    UNIQUE (installation_id, policy_digest),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_retention_runs (
    receipt_ref TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    policy_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending_checkpoint', 'completed')),
    record_json BLOB NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (installation_id, policy_revision)
        REFERENCES app_retention_policies(installation_id, policy_revision)
) STRICT;

CREATE INDEX app_data_import_receipts_installation_idx
    ON app_data_import_receipts(installation_id, committed_at, receipt_ref);
CREATE INDEX app_purge_receipts_installation_idx
    ON app_purge_receipts(installation_id, state, started_at, receipt_ref);
CREATE INDEX app_retention_runs_installation_idx
    ON app_retention_runs(installation_id, state, started_at, receipt_ref);
"#;

// Phase 2E entity-change delivery uses the same move-only lease semantics as
// lifecycle delivery. These columns make exact owner/token/attempt/receipt
// replay durable without placing any authority in the event payload.
const APP_REGISTRY_SCHEMA_V5: &str = r#"
ALTER TABLE app_entity_outbox ADD COLUMN lease_owner TEXT;
ALTER TABLE app_entity_outbox ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0
    CHECK (attempt_count >= 0);
ALTER TABLE app_entity_outbox ADD COLUMN delivery_receipt_id TEXT;

UPDATE app_purge_receipts
   SET record_json = CAST(
       json_set(
           json_remove(CAST(record_json AS TEXT), '$.selection'),
           '$.protocol_version', 2,
           '$.selection_kind',
               CASE
                   WHEN json_type(CAST(record_json AS TEXT), '$.selection') = 'text'
                    AND json_extract(CAST(record_json AS TEXT), '$.selection') = 'whole_installation'
                   THEN 'whole_installation'
                   ELSE 'records'
               END,
           '$.selection_digest', selection_digest
       ) AS BLOB
   )
 WHERE state = 'completed'
   AND json_extract(CAST(record_json AS TEXT), '$.protocol_version') = 1;
"#;

// A cursor chain retains one immutable snapshot root. Continuation rows carry
// only their evidence and offset, avoiding one full snapshot copy per page.
const APP_REGISTRY_SCHEMA_V6: &str = r#"
ALTER TABLE app_query_cursors ADD COLUMN snapshot_ref TEXT;
UPDATE app_query_cursors SET snapshot_ref = cursor_ref;

CREATE INDEX app_query_cursors_snapshot_idx
    ON app_query_cursors(installation_id, snapshot_ref, created_at);

CREATE TRIGGER app_query_cursors_snapshot_ref_insert_guard
BEFORE INSERT ON app_query_cursors
WHEN NEW.snapshot_ref IS NULL OR length(NEW.snapshot_ref) = 0
BEGIN
    SELECT RAISE(ABORT, 'app query cursor snapshot_ref is required');
END;

CREATE TRIGGER app_query_cursors_snapshot_ref_update_guard
BEFORE UPDATE OF snapshot_ref ON app_query_cursors
WHEN NEW.snapshot_ref IS NULL OR length(NEW.snapshot_ref) = 0
BEGIN
    SELECT RAISE(ABORT, 'app query cursor snapshot_ref is required');
END;
"#;

// Phase 3 activates one immutable, complete surface generation. A generation
// owns every declared view; individual rows are members and can never become
// the installation's active surface revision on their own.
const APP_REGISTRY_SCHEMA_V7: &str = r#"
CREATE UNIQUE INDEX app_schema_revision_package_identity_idx
    ON app_schema_revisions(installation_id, revision, package_revision_ref);

CREATE TABLE app_surface_generations (
    installation_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    package_revision_ref TEXT NOT NULL,
    schema_revision INTEGER NOT NULL CHECK (schema_revision > 0),
    compiled_set_digest TEXT NOT NULL,
    member_count INTEGER NOT NULL CHECK (member_count > 0),
    created_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, revision),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id),
    FOREIGN KEY (package_revision_ref)
        REFERENCES app_package_revisions(package_revision_ref),
    FOREIGN KEY (installation_id, schema_revision, package_revision_ref)
        REFERENCES app_schema_revisions(installation_id, revision, package_revision_ref)
) STRICT;

CREATE TABLE app_surface_generation_members (
    installation_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    view_id TEXT NOT NULL,
    app_local_route TEXT NOT NULL,
    canonical_host_route TEXT NOT NULL,
    compiled_view_digest TEXT NOT NULL,
    binding_json BLOB NOT NULL,
    envelope_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, revision, view_id),
    UNIQUE (installation_id, revision, app_local_route),
    UNIQUE (installation_id, revision, canonical_host_route),
    FOREIGN KEY (installation_id, revision)
        REFERENCES app_surface_generations(installation_id, revision)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX app_surface_generation_package_idx
    ON app_surface_generations(installation_id, package_revision_ref, schema_revision);
CREATE INDEX app_surface_generation_route_idx
    ON app_surface_generation_members(installation_id, revision, app_local_route);
"#;

// Phase 4B's canonical resource authority remains inside the registry-owned
// per-scope database. The event journal is the durable tree authority; trigger-
// maintained period totals are its installation authority and scalar tree
// state permits exact root exclusion/reconciliation. The pre-existing usage
// table remains a rebuildable presentation projection and is never consulted
// for admission.
const APP_REGISTRY_SCHEMA_V8: &str = r#"
CREATE TABLE app_resource_periods (
    installation_id TEXT NOT NULL,
    installation_generation INTEGER NOT NULL CHECK (installation_generation > 0),
    period_ref TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    committed_tokens INTEGER NOT NULL DEFAULT 0 CHECK (committed_tokens >= 0),
    outstanding_tokens INTEGER NOT NULL DEFAULT 0 CHECK (outstanding_tokens >= 0),
    committed_cost_microusd INTEGER NOT NULL DEFAULT 0 CHECK (committed_cost_microusd >= 0),
    outstanding_cost_microusd INTEGER NOT NULL DEFAULT 0 CHECK (outstanding_cost_microusd >= 0),
    background_starts INTEGER NOT NULL DEFAULT 0 CHECK (background_starts >= 0),
    foreground_runs INTEGER NOT NULL DEFAULT 0 CHECK (foreground_runs >= 0),
    background_runs INTEGER NOT NULL DEFAULT 0 CHECK (background_runs >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, installation_generation, period_ref),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_resource_trees (
    budget_ledger_ref TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    installation_generation INTEGER NOT NULL CHECK (installation_generation > 0),
    root_execution_id TEXT NOT NULL,
    period_ref TEXT NOT NULL,
    admitted_period_revision INTEGER NOT NULL CHECK (admitted_period_revision > 0),
    lane TEXT NOT NULL CHECK (lane IN ('foreground', 'background')),
    identity_json BLOB NOT NULL,
    last_event_sequence INTEGER NOT NULL CHECK (last_event_sequence > 0),
    evaluated_at_elapsed_ms INTEGER NOT NULL CHECK (evaluated_at_elapsed_ms >= 0),
    committed_tokens INTEGER NOT NULL DEFAULT 0 CHECK (committed_tokens >= 0),
    outstanding_tokens INTEGER NOT NULL DEFAULT 0 CHECK (outstanding_tokens >= 0),
    committed_cost_microusd INTEGER NOT NULL DEFAULT 0 CHECK (committed_cost_microusd >= 0),
    outstanding_cost_microusd INTEGER NOT NULL DEFAULT 0 CHECK (outstanding_cost_microusd >= 0),
    terminally_settled INTEGER NOT NULL DEFAULT 0 CHECK (terminally_settled IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (installation_id, root_execution_id),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id),
    FOREIGN KEY (installation_id, installation_generation, period_ref)
        REFERENCES app_resource_periods(
            installation_id, installation_generation, period_ref
        )
) STRICT;

CREATE TABLE app_resource_tree_events (
    budget_ledger_ref TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    event_digest TEXT NOT NULL,
    event_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (budget_ledger_ref, sequence),
    FOREIGN KEY (budget_ledger_ref)
        REFERENCES app_resource_trees(budget_ledger_ref)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX app_resource_period_tree_idx
    ON app_resource_trees(
        installation_id, installation_generation, period_ref,
        terminally_settled, lane
    );

CREATE TRIGGER app_resource_tree_identity_update_guard
BEFORE UPDATE OF
    budget_ledger_ref, installation_id, installation_generation,
    root_execution_id, period_ref, admitted_period_revision, lane, identity_json
ON app_resource_trees
BEGIN
    SELECT RAISE(ABORT, 'app resource tree identity is immutable');
END;

CREATE TRIGGER app_resource_tree_state_regression_guard
BEFORE UPDATE OF last_event_sequence, evaluated_at_elapsed_ms, terminally_settled
ON app_resource_trees
WHEN NEW.last_event_sequence < OLD.last_event_sequence
  OR NEW.evaluated_at_elapsed_ms < OLD.evaluated_at_elapsed_ms
  OR (OLD.terminally_settled = 1 AND NEW.terminally_settled = 0)
BEGIN
    SELECT RAISE(ABORT, 'app resource tree state cannot regress');
END;

CREATE TRIGGER app_resource_tree_period_insert
AFTER INSERT ON app_resource_trees
BEGIN
    UPDATE app_resource_periods
       SET revision = revision + 1,
           committed_tokens = committed_tokens + NEW.committed_tokens,
           outstanding_tokens = outstanding_tokens + NEW.outstanding_tokens,
           committed_cost_microusd = committed_cost_microusd + NEW.committed_cost_microusd,
           outstanding_cost_microusd = outstanding_cost_microusd + NEW.outstanding_cost_microusd,
           background_starts = background_starts
               + CASE WHEN NEW.lane = 'background' THEN 1 ELSE 0 END,
           foreground_runs = foreground_runs
               + CASE WHEN NEW.lane = 'foreground' AND NEW.terminally_settled = 0 THEN 1 ELSE 0 END,
           background_runs = background_runs
               + CASE WHEN NEW.lane = 'background' AND NEW.terminally_settled = 0 THEN 1 ELSE 0 END,
           updated_at = NEW.updated_at
     WHERE installation_id = NEW.installation_id
       AND installation_generation = NEW.installation_generation
       AND period_ref = NEW.period_ref;
    SELECT CASE WHEN changes() != 1
        THEN RAISE(ABORT, 'app resource period insert projection failed') END;
END;

CREATE TRIGGER app_resource_tree_period_update
AFTER UPDATE OF
    committed_tokens, outstanding_tokens, committed_cost_microusd,
    outstanding_cost_microusd, terminally_settled
ON app_resource_trees
BEGIN
    UPDATE app_resource_periods
       SET revision = revision + 1,
           committed_tokens = committed_tokens - OLD.committed_tokens + NEW.committed_tokens,
           outstanding_tokens = outstanding_tokens - OLD.outstanding_tokens + NEW.outstanding_tokens,
           committed_cost_microusd = committed_cost_microusd
               - OLD.committed_cost_microusd + NEW.committed_cost_microusd,
           outstanding_cost_microusd = outstanding_cost_microusd
               - OLD.outstanding_cost_microusd + NEW.outstanding_cost_microusd,
           foreground_runs = foreground_runs
               - CASE WHEN OLD.lane = 'foreground' AND OLD.terminally_settled = 0 THEN 1 ELSE 0 END
               + CASE WHEN NEW.lane = 'foreground' AND NEW.terminally_settled = 0 THEN 1 ELSE 0 END,
           background_runs = background_runs
               - CASE WHEN OLD.lane = 'background' AND OLD.terminally_settled = 0 THEN 1 ELSE 0 END
               + CASE WHEN NEW.lane = 'background' AND NEW.terminally_settled = 0 THEN 1 ELSE 0 END,
           updated_at = NEW.updated_at
     WHERE installation_id = NEW.installation_id
       AND installation_generation = NEW.installation_generation
       AND period_ref = NEW.period_ref;
    SELECT CASE WHEN changes() != 1
        THEN RAISE(ABORT, 'app resource period update projection failed') END;
END;

CREATE TRIGGER app_resource_tree_delete_guard
BEFORE DELETE ON app_resource_trees
BEGIN
    SELECT RAISE(ABORT, 'canonical app resource trees require retained settlement evidence');
END;

CREATE TRIGGER app_resource_tree_event_update_guard
BEFORE UPDATE ON app_resource_tree_events
BEGIN
    SELECT RAISE(ABORT, 'canonical app resource events are append-only');
END;

CREATE TRIGGER app_resource_tree_event_delete_guard
BEFORE DELETE ON app_resource_tree_events
BEGIN
    SELECT RAISE(ABORT, 'canonical app resource events require retained settlement evidence');
END;

CREATE TRIGGER app_resource_tree_event_revision_guard
BEFORE INSERT ON app_resource_tree_events
WHEN NEW.sequence != COALESCE((
    SELECT last_event_sequence
      FROM app_resource_trees
     WHERE budget_ledger_ref = NEW.budget_ledger_ref
), 0)
BEGIN
    SELECT RAISE(ABORT, 'app resource event must match the tree journal revision');
END;
"#;

// Phase 3E directory metadata and user placement remain bounded read models
// inside the existing scoped registry. They grant no execution/data authority
// and contain no entity payloads.
const APP_REGISTRY_SCHEMA_DIRECTORY: &str = r#"
CREATE TABLE app_package_directory_metadata (
    package_revision_ref TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    metadata_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (package_revision_ref)
        REFERENCES app_package_revisions(package_revision_ref)
) STRICT;

CREATE TABLE app_package_directory_actions (
    package_revision_ref TEXT NOT NULL,
    action_id TEXT NOT NULL,
    PRIMARY KEY (package_revision_ref, action_id),
    FOREIGN KEY (package_revision_ref)
        REFERENCES app_package_directory_metadata(package_revision_ref)
        ON DELETE CASCADE
) STRICT;

CREATE TABLE app_directory_state (
    installation_id TEXT PRIMARY KEY,
    last_opened_at TEXT,
    last_view_id TEXT,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_directory_pins (
    installation_id TEXT NOT NULL,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('view', 'action')),
    target_id TEXT NOT NULL,
    pinned_at TEXT NOT NULL,
    PRIMARY KEY (installation_id, target_kind, target_id),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE INDEX app_package_directory_metadata_search_idx
    ON app_package_directory_metadata(name);
CREATE INDEX app_directory_state_recent_idx
    ON app_directory_state(last_opened_at, installation_id);
CREATE INDEX app_directory_pins_recent_idx
    ON app_directory_pins(pinned_at, installation_id);
"#;

// Phase 4B period lifecycle, trusted crash-recovery evidence and bounded
// retired-tree tombstones. Canonical event/tree rows remain the live authority;
// retirement is permitted only after a terminal tree has first produced an
// immutable tombstone, so retention can never make an accepted execution
// replayable as new work.
const APP_REGISTRY_SCHEMA_V10: &str = r#"
ALTER TABLE app_resource_periods ADD COLUMN admissions_closed_at TEXT;
ALTER TABLE app_resource_periods ADD COLUMN retention_until TEXT;
ALTER TABLE app_resource_trees ADD COLUMN terminal_state_json BLOB;
ALTER TABLE app_resource_trees ADD COLUMN period_ends_at_elapsed_ms INTEGER;
ALTER TABLE app_resource_trees ADD COLUMN package_bytes INTEGER;
ALTER TABLE app_resource_usage_projection ADD COLUMN period_ref TEXT;
ALTER TABLE app_resource_usage_projection ADD COLUMN period_created_at TEXT;

CREATE TABLE app_resource_recovery_evidence (
    budget_ledger_ref TEXT NOT NULL,
    observation_id TEXT NOT NULL,
    reservation_id TEXT NOT NULL,
    reconciliation_ref TEXT NOT NULL,
    reconciliation_revision INTEGER NOT NULL CHECK (reconciliation_revision > 0),
    reconciled_at_elapsed_ms INTEGER NOT NULL CHECK (reconciled_at_elapsed_ms >= 0),
    evidence_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (budget_ledger_ref, observation_id),
    UNIQUE (budget_ledger_ref, reconciliation_ref),
    FOREIGN KEY (budget_ledger_ref)
        REFERENCES app_resource_trees(budget_ledger_ref)
        ON DELETE CASCADE
) STRICT;

CREATE TABLE app_resource_retired_trees (
    budget_ledger_ref TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    installation_generation INTEGER NOT NULL CHECK (installation_generation > 0),
    root_execution_id TEXT NOT NULL,
    period_ref TEXT NOT NULL,
    identity_json BLOB NOT NULL,
    final_state_json BLOB NOT NULL,
    final_state_digest TEXT NOT NULL,
    final_event_sequence INTEGER NOT NULL CHECK (final_event_sequence > 0),
    final_journal_digest TEXT NOT NULL,
    retired_at TEXT NOT NULL,
    UNIQUE (installation_id, root_execution_id),
    FOREIGN KEY (installation_id)
        REFERENCES app_installations(installation_id)
) STRICT;

CREATE INDEX app_resource_retention_period_idx
    ON app_resource_periods(
        installation_id, installation_generation,
        admissions_closed_at, retention_until
    );

CREATE INDEX app_resource_retired_installation_idx
    ON app_resource_retired_trees(
        installation_id, installation_generation, period_ref, retired_at
    );

CREATE INDEX app_resource_cleanup_keyset_idx
    ON app_resource_trees(
        installation_id, installation_generation,
        terminally_settled, budget_ledger_ref
    );

CREATE INDEX app_resource_retention_keyset_idx
    ON app_resource_trees(
        installation_id, installation_generation, period_ref,
        terminally_settled, budget_ledger_ref
    );

CREATE TRIGGER app_resource_recovery_evidence_update_guard
BEFORE UPDATE ON app_resource_recovery_evidence
BEGIN
    SELECT RAISE(ABORT, 'app resource recovery evidence is immutable');
END;

CREATE TRIGGER app_resource_recovery_evidence_delete_guard
BEFORE DELETE ON app_resource_recovery_evidence
WHEN NOT EXISTS (
    SELECT 1 FROM app_resource_retired_trees
     WHERE budget_ledger_ref = OLD.budget_ledger_ref
)
BEGIN
    SELECT RAISE(ABORT, 'app resource recovery evidence is retained until tree retirement');
END;

CREATE TRIGGER app_resource_period_identity_guard
BEFORE UPDATE OF installation_id, installation_generation, period_ref
ON app_resource_periods
BEGIN
    SELECT RAISE(ABORT, 'app resource period identity is immutable');
END;

CREATE TRIGGER app_resource_period_insert_guard
BEFORE INSERT ON app_resource_periods
WHEN NEW.revision != 1
  OR NEW.committed_tokens != 0
  OR NEW.outstanding_tokens != 0
  OR NEW.committed_cost_microusd != 0
  OR NEW.outstanding_cost_microusd != 0
  OR NEW.background_starts != 0
  OR NEW.foreground_runs != 0
  OR NEW.background_runs != 0
  OR NEW.admissions_closed_at IS NOT NULL
  OR NEW.retention_until IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'app resource period must begin as an empty open revision');
END;

CREATE TRIGGER app_resource_period_lifecycle_regression_guard
BEFORE UPDATE OF revision, admissions_closed_at, retention_until
ON app_resource_periods
WHEN NEW.revision < OLD.revision
  OR NEW.revision > OLD.revision + 1
  OR (OLD.admissions_closed_at IS NOT NULL
      AND NEW.admissions_closed_at IS NOT OLD.admissions_closed_at)
  OR (OLD.retention_until IS NOT NULL
      AND NEW.retention_until IS NOT OLD.retention_until)
  OR ((NEW.admissions_closed_at IS NULL) != (NEW.retention_until IS NULL))
BEGIN
    SELECT RAISE(ABORT, 'app resource period lifecycle cannot regress');
END;

CREATE TRIGGER app_resource_terminal_state_insert_guard
BEFORE INSERT ON app_resource_trees
WHEN (NEW.terminally_settled = 1 AND NEW.terminal_state_json IS NULL)
  OR (NEW.terminally_settled = 0 AND NEW.terminal_state_json IS NOT NULL)
  OR NEW.period_ends_at_elapsed_ms IS NULL
  OR NEW.period_ends_at_elapsed_ms <= 0
  OR NEW.package_bytes IS NULL
  OR NEW.package_bytes < 0
BEGIN
    SELECT RAISE(ABORT, 'app resource terminal state is total at creation');
END;

CREATE TRIGGER app_resource_runtime_baseline_guard
BEFORE UPDATE OF period_ends_at_elapsed_ms, package_bytes
ON app_resource_trees
BEGIN
    SELECT RAISE(ABORT, 'app resource root runtime baseline is immutable');
END;

CREATE TRIGGER app_resource_terminal_state_guard
BEFORE UPDATE OF terminally_settled, terminal_state_json
ON app_resource_trees
WHEN (NEW.terminally_settled = 1 AND NEW.terminal_state_json IS NULL)
  OR (NEW.terminally_settled = 0 AND NEW.terminal_state_json IS NOT NULL)
  OR (OLD.terminally_settled = 1
      AND NEW.terminal_state_json IS NOT OLD.terminal_state_json)
BEGIN
    SELECT RAISE(ABORT, 'app resource terminal state is immutable and total');
END;

CREATE TRIGGER app_resource_retired_tree_insert_guard
BEFORE INSERT ON app_resource_retired_trees
WHEN NOT EXISTS (
    SELECT 1 FROM app_resource_trees tree
     WHERE tree.budget_ledger_ref = NEW.budget_ledger_ref
       AND tree.installation_id = NEW.installation_id
       AND tree.installation_generation = NEW.installation_generation
       AND tree.root_execution_id = NEW.root_execution_id
       AND tree.period_ref = NEW.period_ref
       AND tree.identity_json = NEW.identity_json
       AND tree.terminal_state_json = NEW.final_state_json
       AND tree.last_event_sequence = NEW.final_event_sequence
       AND tree.terminally_settled = 1
)
BEGIN
    SELECT RAISE(ABORT, 'app resource retirement requires the exact terminal tree');
END;

CREATE TRIGGER app_resource_retired_tree_update_guard
BEFORE UPDATE ON app_resource_retired_trees
BEGIN
    SELECT RAISE(ABORT, 'app resource retired tree is immutable');
END;

CREATE TRIGGER app_resource_retired_tree_delete_guard
BEFORE DELETE ON app_resource_retired_trees
BEGIN
    SELECT RAISE(ABORT, 'app resource retired tree preserves replay denial');
END;

DROP TRIGGER app_resource_tree_delete_guard;
CREATE TRIGGER app_resource_tree_delete_guard
BEFORE DELETE ON app_resource_trees
WHEN NOT EXISTS (
    SELECT 1 FROM app_resource_retired_trees
     WHERE budget_ledger_ref = OLD.budget_ledger_ref
)
BEGIN
    SELECT RAISE(ABORT, 'canonical app resource trees require a retained tombstone');
END;

DROP TRIGGER app_resource_tree_event_delete_guard;
CREATE TRIGGER app_resource_tree_event_delete_guard
BEFORE DELETE ON app_resource_tree_events
WHEN NOT EXISTS (
    SELECT 1 FROM app_resource_retired_trees
     WHERE budget_ledger_ref = OLD.budget_ledger_ref
)
BEGIN
    SELECT RAISE(ABORT, 'canonical app resource events require a retained tombstone');
END;
"#;

// Phase 4D immutable standalone procedure revisions. Exact bytes are retained
// by digest inside the authenticated app registry only; they are never
// projected into global skill discovery. Both identity rows and blobs are
// write-once so a package lock always reconstructs the same procedure bytes.
const APP_REGISTRY_SCHEMA_V11: &str = r#"
CREATE TABLE app_skill_revision_blobs (
    content_digest TEXT PRIMARY KEY,
    byte_count INTEGER NOT NULL CHECK (byte_count > 0),
    skill_document BLOB NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE app_skill_revisions (
    immutable_revision_ref TEXT PRIMARY KEY,
    dependency_ref TEXT NOT NULL,
    semantic_version TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    content_digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (dependency_ref, semantic_version),
    UNIQUE (dependency_ref, revision),
    FOREIGN KEY (content_digest)
        REFERENCES app_skill_revision_blobs(content_digest)
) STRICT;

CREATE INDEX app_skill_revisions_dependency_idx
    ON app_skill_revisions(dependency_ref, semantic_version, revision);

CREATE TRIGGER app_skill_revision_blob_update_guard
BEFORE UPDATE ON app_skill_revision_blobs
BEGIN
    SELECT RAISE(ABORT, 'app skill revision bytes are immutable');
END;

CREATE TRIGGER app_skill_revision_blob_delete_guard
BEFORE DELETE ON app_skill_revision_blobs
BEGIN
    SELECT RAISE(ABORT, 'app skill revision bytes are retained');
END;

CREATE TRIGGER app_skill_revision_update_guard
BEFORE UPDATE ON app_skill_revisions
BEGIN
    SELECT RAISE(ABORT, 'app skill revision identity is immutable');
END;

CREATE TRIGGER app_skill_revision_delete_guard
BEFORE DELETE ON app_skill_revisions
BEGIN
    SELECT RAISE(ABORT, 'app skill revision identity is retained');
END;
"#;

// Phase 4A monotonic protected workflow control state. Exact generation blobs
// are immutable while a CAS head or prepared proposal authorizes them; full
// payload bytes are pruned after supersession so progress snapshots do not
// create unbounded protected-data retention. Copying an older valid sidecar or
// pause file cannot move the registry authority backwards or resurrect a
// Consumed continuation because only the monotonic head is executable.
const APP_REGISTRY_SCHEMA_V12: &str = r#"
CREATE TABLE app_workflow_control_blobs (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (
        control_kind IN ('task_binding', 'run_state', 'pause', 'interactive_stop')
    ),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    byte_count INTEGER NOT NULL CHECK (byte_count > 0),
    sealed_blob BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (task_id, execution_id, control_kind, generation),
    UNIQUE (task_id, execution_id, control_kind, generation, content_digest)
) STRICT;

CREATE TABLE app_workflow_control_heads (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (
        control_kind IN ('task_binding', 'run_state', 'pause', 'interactive_stop')
    ),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL CHECK (
        lifecycle_state IN ('prepared', 'active', 'claimed', 'consumed')
    ),
    claim_ref TEXT,
    claim_expires_at TEXT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    updated_at TEXT NOT NULL,
    CHECK (
        (lifecycle_state = 'claimed'
            AND claim_ref IS NOT NULL
            AND claim_expires_at IS NOT NULL)
        OR (lifecycle_state IN ('prepared', 'active', 'consumed')
            AND claim_ref IS NULL
            AND claim_expires_at IS NULL)
    ),
    PRIMARY KEY (task_id, execution_id, control_kind),
    FOREIGN KEY (task_id, execution_id, control_kind, generation, content_digest)
        REFERENCES app_workflow_control_blobs(
            task_id, execution_id, control_kind, generation, content_digest
        )
) STRICT;

CREATE TABLE app_workflow_control_prepared (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (control_kind = 'pause'),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    base_revision INTEGER NOT NULL CHECK (base_revision >= 0),
    base_lifecycle TEXT CHECK (
        base_lifecycle IS NULL OR base_lifecycle IN ('active', 'claimed', 'consumed')
    ),
    base_claim_ref TEXT,
    proposal_ref TEXT NOT NULL,
    prepared_at TEXT NOT NULL,
    CHECK (
        (base_lifecycle = 'claimed' AND base_claim_ref IS NOT NULL)
        OR (base_lifecycle IS NULL AND base_revision = 0 AND base_claim_ref IS NULL)
        OR (base_lifecycle IN ('active', 'consumed') AND base_claim_ref IS NULL)
    ),
    PRIMARY KEY (task_id, execution_id, control_kind),
    FOREIGN KEY (task_id, execution_id, control_kind, generation, content_digest)
        REFERENCES app_workflow_control_blobs(
            task_id, execution_id, control_kind, generation, content_digest
        )
) STRICT;

CREATE TRIGGER app_workflow_control_blob_update_guard
BEFORE UPDATE ON app_workflow_control_blobs
BEGIN
    SELECT RAISE(ABORT, 'app workflow control generations are immutable');
END;

CREATE TRIGGER app_workflow_control_blob_delete_guard
BEFORE DELETE ON app_workflow_control_blobs
WHEN EXISTS (
        SELECT 1 FROM app_workflow_control_heads h
         WHERE h.task_id = OLD.task_id
           AND h.execution_id = OLD.execution_id
           AND h.control_kind = OLD.control_kind
           AND h.generation = OLD.generation
           AND h.content_digest = OLD.content_digest
    )
    OR EXISTS (
        SELECT 1 FROM app_workflow_control_prepared p
         WHERE p.task_id = OLD.task_id
           AND p.execution_id = OLD.execution_id
           AND p.control_kind = OLD.control_kind
           AND p.generation = OLD.generation
           AND p.content_digest = OLD.content_digest
    )
BEGIN
    SELECT RAISE(ABORT, 'authoritative app workflow control blobs are retained');
END;

CREATE TRIGGER app_workflow_control_head_delete_guard
BEFORE DELETE ON app_workflow_control_heads
BEGIN
    SELECT RAISE(ABORT, 'app workflow control authority is retained');
END;

CREATE TRIGGER app_workflow_control_head_cas_guard
BEFORE UPDATE ON app_workflow_control_heads
WHEN NEW.revision != OLD.revision + 1
  OR NEW.generation < OLD.generation
  OR (OLD.lifecycle_state = 'consumed'
      AND NEW.generation = OLD.generation)
BEGIN
    SELECT RAISE(ABORT, 'app workflow control head requires monotonic CAS');
END;
"#;

pub const APP_REGISTRY_SCHEMA_V13: &str = r#"
CREATE TABLE app_memory_candidates (
    candidate_id TEXT PRIMARY KEY,
    candidate_revision INTEGER NOT NULL CHECK (candidate_revision > 0),
    candidate_fingerprint TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN ('proposed', 'accepted', 'rejected', 'stale', 'tombstoned')
    ),
    record_json BLOB NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE app_memory_candidate_sources (
    candidate_id TEXT NOT NULL,
    installation_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    record_revision INTEGER NOT NULL CHECK (record_revision > 0),
    PRIMARY KEY (candidate_id, installation_id, entity_name, record_id),
    FOREIGN KEY (candidate_id) REFERENCES app_memory_candidates(candidate_id)
) STRICT;

CREATE INDEX app_memory_candidate_sources_record_idx
    ON app_memory_candidate_sources(installation_id, entity_name, record_id);
CREATE INDEX app_memory_candidate_sources_installation_idx
    ON app_memory_candidate_sources(installation_id);

CREATE TRIGGER app_memory_candidates_delete_guard
BEFORE DELETE ON app_memory_candidates
BEGIN
    SELECT RAISE(ABORT, 'app memory candidates are retained');
END;
"#;

pub const APP_REGISTRY_SCHEMA_V14: &str = r#"
-- Phase 6 reuses the v3 app_migration_runs table. A second CREATE TABLE
-- with the same name fails closed on every fresh store. Widen the existing
-- row with the plan/generation columns the update kernel records.
ALTER TABLE app_migration_runs ADD COLUMN plan_digest TEXT;
ALTER TABLE app_migration_runs ADD COLUMN source_generation INTEGER;
ALTER TABLE app_migration_runs ADD COLUMN destination_generation INTEGER;
"#;

const APP_REGISTRY_SCHEMA_V15: &str = r#"
-- Bounded personal-agent discovery scans immutable directory names in
-- normalized prefix order, then joins exact installations without sorting an
-- unbounded scoped registry. These are read accelerators, not authority.
CREATE INDEX app_package_directory_metadata_discovery_idx
    ON app_package_directory_metadata(LOWER(name), package_revision_ref);
CREATE INDEX app_installations_package_discovery_idx
    ON app_installations(package_revision_ref, principal, workspace, installation_id);
"#;

const APP_REGISTRY_SCHEMA_V16: &str = r#"
-- Keep the exact-installation side of discovery bounded while filtering live
-- rows. V15 remains an immutable historical migration; V16 replaces only its
-- read accelerator so existing V15 authorities upgrade transactionally.
DROP INDEX app_installations_package_discovery_idx;
CREATE INDEX app_installations_package_discovery_idx
    ON app_installations(
        package_revision_ref, principal, workspace, lifecycle_status, installation_id
    );
"#;

const APP_REGISTRY_SCHEMA_V17: &str = r#"
-- Inert P6 source contribution journals. Rows are destination-specific,
-- append-only until an exact destination receipt acknowledges the move-only
-- lease. Nothing in this schema grants an app memory or retrieval authority.
CREATE TABLE app_memory_contribution_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE CHECK(length(event_id) BETWEEN 1 AND 192),
    proposal_id TEXT NOT NULL CHECK(length(proposal_id) BETWEEN 1 AND 192),
    proposal_revision INTEGER NOT NULL CHECK(proposal_revision > 0),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    source_event_ref TEXT NOT NULL,
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    dedupe_key TEXT NOT NULL CHECK(length(dedupe_key) BETWEEN 1 AND 192),
    settlement_ref TEXT NOT NULL CHECK(length(settlement_ref) BETWEEN 1 AND 192),
    proposal_expires_at TEXT NOT NULL,
    proposal_json BLOB NOT NULL CHECK(length(proposal_json) BETWEEN 1 AND 262144),
    delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(delivery_state IN ('pending', 'leased', 'dispatching', 'delivered')),
    available_at TEXT NOT NULL,
    lease_owner TEXT,
    lease_token TEXT,
    lease_expires_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    lease_epoch INTEGER NOT NULL DEFAULT 0 CHECK(lease_epoch >= 0),
    ingress_receipt_json BLOB CHECK(ingress_receipt_json IS NULL OR length(ingress_receipt_json) <= 65536),
    ingress_receipt_digest TEXT CHECK(ingress_receipt_digest IS NULL OR length(ingress_receipt_digest) = 71),
    destination_generation INTEGER CHECK(destination_generation IS NULL OR destination_generation > 0),
    destination_receipt_digest TEXT CHECK(destination_receipt_digest IS NULL OR length(destination_receipt_digest) = 71),
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    UNIQUE(installation_id, proposal_id, proposal_revision),
    CHECK((delivery_state IN ('leased', 'dispatching')) =
          (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK((delivery_state = 'delivered') =
          (delivered_at IS NOT NULL AND ingress_receipt_json IS NOT NULL
           AND ingress_receipt_digest IS NOT NULL)),
    CHECK((delivery_state = 'delivered') =
          (destination_generation IS NOT NULL AND destination_receipt_digest IS NOT NULL))
) STRICT;
CREATE INDEX app_memory_contribution_outbox_ready_idx
    ON app_memory_contribution_outbox(delivery_state, available_at, sequence);
CREATE INDEX app_memory_contribution_outbox_installation_idx
    ON app_memory_contribution_outbox(installation_id, delivered_at, sequence);
CREATE INDEX app_memory_contribution_outbox_dedupe_idx
    ON app_memory_contribution_outbox(installation_id, dedupe_key, proposal_revision);

CREATE TABLE app_memory_contribution_sources (
    event_id TEXT NOT NULL,
    installation_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    record_revision INTEGER NOT NULL CHECK(record_revision > 0),
    canonical_source_ref TEXT NOT NULL CHECK(length(canonical_source_ref) BETWEEN 1 AND 192),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    PRIMARY KEY(event_id, installation_id, entity_name, record_id),
    FOREIGN KEY(event_id) REFERENCES app_memory_contribution_outbox(event_id)
) STRICT;
CREATE INDEX app_memory_contribution_sources_lookup_idx
    ON app_memory_contribution_sources(installation_id, entity_name, record_id, record_revision);

CREATE TABLE app_memory_invalidation_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE CHECK(length(event_id) BETWEEN 1 AND 192),
    invalidation_id TEXT NOT NULL CHECK(length(invalidation_id) BETWEEN 1 AND 192),
    invalidation_digest TEXT NOT NULL CHECK(length(invalidation_digest) = 71),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    proposal_id TEXT NOT NULL CHECK(length(proposal_id) BETWEEN 1 AND 192),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    dedupe_key TEXT NOT NULL CHECK(length(dedupe_key) BETWEEN 1 AND 192),
    source_event_ref TEXT NOT NULL,
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    source_entity_name TEXT NOT NULL CHECK(length(source_entity_name) BETWEEN 1 AND 192),
    source_record_id TEXT NOT NULL CHECK(length(source_record_id) BETWEEN 1 AND 192),
    source_proposal_json BLOB NOT NULL CHECK(length(source_proposal_json) BETWEEN 1 AND 262144),
    invalidation_json BLOB NOT NULL CHECK(length(invalidation_json) BETWEEN 1 AND 65536),
    delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(delivery_state IN ('pending', 'leased', 'dispatching', 'delivered')),
    available_at TEXT NOT NULL,
    lease_owner TEXT,
    lease_token TEXT,
    lease_expires_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    lease_epoch INTEGER NOT NULL DEFAULT 0 CHECK(lease_epoch >= 0),
    invalidation_receipt_json BLOB CHECK(invalidation_receipt_json IS NULL OR length(invalidation_receipt_json) <= 65536),
    invalidation_receipt_digest TEXT CHECK(invalidation_receipt_digest IS NULL OR length(invalidation_receipt_digest) = 71),
    destination_generation INTEGER CHECK(destination_generation IS NULL OR destination_generation > 0),
    destination_receipt_digest TEXT CHECK(destination_receipt_digest IS NULL OR length(destination_receipt_digest) = 71),
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    UNIQUE(installation_id, invalidation_id),
    CHECK((delivery_state IN ('leased', 'dispatching')) =
          (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK((delivery_state = 'delivered') =
          (delivered_at IS NOT NULL AND invalidation_receipt_json IS NOT NULL
           AND invalidation_receipt_digest IS NOT NULL)),
    CHECK((delivery_state = 'delivered') =
          (destination_generation IS NOT NULL AND destination_receipt_digest IS NOT NULL))
) STRICT;
CREATE INDEX app_memory_invalidation_outbox_ready_idx
    ON app_memory_invalidation_outbox(delivery_state, available_at, sequence);

CREATE TABLE app_memory_contribution_heads (
    installation_id TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    event_id TEXT NOT NULL,
    proposal_id TEXT NOT NULL,
    proposal_revision INTEGER NOT NULL CHECK(proposal_revision > 0),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    source_event_ref TEXT NOT NULL,
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    source_entity_name TEXT NOT NULL CHECK(length(source_entity_name) BETWEEN 1 AND 192),
    source_record_id TEXT NOT NULL CHECK(length(source_record_id) BETWEEN 1 AND 192),
    proposal_expires_at TEXT NOT NULL,
    proposal_json BLOB CHECK(proposal_json IS NULL OR length(proposal_json) BETWEEN 1 AND 262144),
    lifecycle_state TEXT NOT NULL CHECK(lifecycle_state IN ('live', 'invalidating', 'settled')),
    expiration_invalidation_event_id TEXT CHECK(expiration_invalidation_event_id IS NULL OR length(expiration_invalidation_event_id) BETWEEN 1 AND 192),
    latest_invalidation_id TEXT CHECK(latest_invalidation_id IS NULL OR length(latest_invalidation_id) BETWEEN 1 AND 192),
    latest_invalidation_digest TEXT CHECK(latest_invalidation_digest IS NULL OR length(latest_invalidation_digest) = 71),
    latest_invalidation_revision INTEGER CHECK(latest_invalidation_revision IS NULL OR latest_invalidation_revision > 0),
    superseded_invalidation_id TEXT CHECK(superseded_invalidation_id IS NULL OR length(superseded_invalidation_id) BETWEEN 1 AND 192),
    superseded_invalidation_digest TEXT CHECK(superseded_invalidation_digest IS NULL OR length(superseded_invalidation_digest) = 71),
    superseded_invalidation_revision INTEGER CHECK(superseded_invalidation_revision IS NULL OR superseded_invalidation_revision > 0),
    destination_generation INTEGER CHECK(destination_generation IS NULL OR destination_generation > 0),
    destination_receipt_digest TEXT CHECK(destination_receipt_digest IS NULL OR length(destination_receipt_digest) = 71),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, dedupe_key)
) STRICT;

CREATE TABLE app_memory_contribution_terminal (
    event_id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    proposal_id TEXT NOT NULL,
    proposal_revision INTEGER NOT NULL CHECK(proposal_revision > 0),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    source_event_ref TEXT NOT NULL,
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    source_entity_name TEXT NOT NULL CHECK(length(source_entity_name) BETWEEN 1 AND 192),
    source_record_id TEXT NOT NULL CHECK(length(source_record_id) BETWEEN 1 AND 192),
    terminal_state TEXT NOT NULL CHECK(terminal_state IN ('delivered', 'expired', 'superseded', 'invalidated')),
    source_ack_receipt_digest TEXT CHECK(source_ack_receipt_digest IS NULL OR length(source_ack_receipt_digest) = 71),
    destination_generation INTEGER CHECK(destination_generation IS NULL OR destination_generation > 0),
    destination_receipt_digest TEXT CHECK(destination_receipt_digest IS NULL OR length(destination_receipt_digest) = 71),
    terminalized_at TEXT NOT NULL,
    UNIQUE(installation_id, proposal_id, proposal_revision),
    CHECK((destination_generation IS NULL) = (destination_receipt_digest IS NULL))
) STRICT;
CREATE INDEX app_memory_contribution_terminal_settlement_idx
    ON app_memory_contribution_terminal(installation_id, dedupe_key, proposal_revision);

CREATE TABLE app_memory_invalidation_terminal (
    event_id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    invalidation_id TEXT NOT NULL,
    invalidation_digest TEXT NOT NULL CHECK(length(invalidation_digest) = 71),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    proposal_id TEXT NOT NULL CHECK(length(proposal_id) BETWEEN 1 AND 192),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    dedupe_key TEXT NOT NULL CHECK(length(dedupe_key) BETWEEN 1 AND 192),
    source_event_ref TEXT NOT NULL CHECK(length(source_event_ref) BETWEEN 1 AND 192),
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    source_entity_name TEXT NOT NULL CHECK(length(source_entity_name) BETWEEN 1 AND 192),
    source_record_id TEXT NOT NULL CHECK(length(source_record_id) BETWEEN 1 AND 192),
    source_ack_receipt_digest TEXT NOT NULL CHECK(length(source_ack_receipt_digest) = 71),
    destination_generation INTEGER NOT NULL CHECK(destination_generation > 0),
    destination_receipt_digest TEXT NOT NULL CHECK(length(destination_receipt_digest) = 71),
    terminalized_at TEXT NOT NULL,
    UNIQUE(installation_id, invalidation_id)
) STRICT;

-- One immutable source acknowledgement high-water survives bounded terminal
-- replay-window compaction and detects whole-prefix destination rollback.
CREATE TABLE app_memory_destination_ack_high_water (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    destination_generation INTEGER NOT NULL CHECK(destination_generation > 0),
    destination_receipt_digest TEXT NOT NULL CHECK(length(destination_receipt_digest) = 71),
    updated_at TEXT NOT NULL
) STRICT;

CREATE TRIGGER app_memory_contribution_delivered_immutable
BEFORE UPDATE ON app_memory_contribution_outbox
WHEN OLD.delivery_state = 'delivered'
BEGIN
    SELECT RAISE(ABORT, 'delivered memory contribution is immutable');
END;
CREATE TRIGGER app_memory_invalidation_delivered_immutable
BEFORE UPDATE ON app_memory_invalidation_outbox
WHEN OLD.delivery_state = 'delivered'
BEGIN
    SELECT RAISE(ABORT, 'delivered memory invalidation is immutable');
END;
"#;

const APP_REGISTRY_SCHEMA_V18: &str = r#"
-- P6.5 personal-agent retrieval delivery is a distinct, ordered journal. It
-- intentionally shares neither rows nor acknowledgement state with the memory
-- destination. The terminal producer and current reviewed-authority resolver
-- are wired separately; creating these inert tables grants no delivery power.
CREATE TABLE app_contribution_frequency_buckets (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    workflow_id TEXT NOT NULL CHECK(length(workflow_id) BETWEEN 1 AND 192),
    action_id TEXT NOT NULL CHECK(length(action_id) BETWEEN 1 AND 192),
    contribution_port_id TEXT NOT NULL CHECK(length(contribution_port_id) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    window_seconds INTEGER NOT NULL CHECK(window_seconds BETWEEN 1 AND 2592000),
    window_start_epoch INTEGER NOT NULL CHECK(window_start_epoch >= 0),
    reviewed_max_proposals INTEGER NOT NULL CHECK(reviewed_max_proposals BETWEEN 1 AND 10000),
    consumed_count INTEGER NOT NULL CHECK(consumed_count BETWEEN 1 AND 10000),
    event_ledger_json BLOB NOT NULL CHECK(length(event_ledger_json) BETWEEN 1 AND 4194304),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(
        installation_id, workflow_id, action_id, contribution_port_id,
        scope_binding_ref, window_seconds, window_start_epoch
    ),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    CHECK(consumed_count <= reviewed_max_proposals)
) STRICT;
CREATE INDEX app_contribution_frequency_prune_idx
    ON app_contribution_frequency_buckets(window_start_epoch, window_seconds);

CREATE TABLE app_contribution_frequency_compaction_heads (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    workflow_id TEXT NOT NULL CHECK(length(workflow_id) BETWEEN 1 AND 192),
    action_id TEXT NOT NULL CHECK(length(action_id) BETWEEN 1 AND 192),
    contribution_port_id TEXT NOT NULL CHECK(length(contribution_port_id) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    compacted_issued_through_ms INTEGER NOT NULL CHECK(compacted_issued_through_ms >= 0),
    compacted_at TEXT NOT NULL,
    PRIMARY KEY(
        installation_id, workflow_id, action_id, contribution_port_id,
        scope_binding_ref
    ),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_retrieval_delivery_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE CHECK(length(event_id) BETWEEN 1 AND 192),
    event_kind TEXT NOT NULL CHECK(event_kind IN ('proposal', 'invalidation', 'expiration')),
    proposal_id TEXT NOT NULL CHECK(length(proposal_id) BETWEEN 1 AND 192),
    proposal_revision INTEGER NOT NULL CHECK(proposal_revision > 0),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    invalidation_id TEXT CHECK(invalidation_id IS NULL OR length(invalidation_id) BETWEEN 1 AND 192),
    invalidation_digest TEXT CHECK(invalidation_digest IS NULL OR length(invalidation_digest) = 71),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    dedupe_key TEXT NOT NULL CHECK(length(dedupe_key) BETWEEN 1 AND 192),
    source_event_ref TEXT NOT NULL CHECK(length(source_event_ref) BETWEEN 1 AND 192),
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    source_entity_name TEXT NOT NULL CHECK(length(source_entity_name) BETWEEN 1 AND 192),
    source_record_id TEXT NOT NULL CHECK(length(source_record_id) BETWEEN 1 AND 192),
    proposal_expires_at TEXT NOT NULL,
    proposal_json BLOB NOT NULL CHECK(length(proposal_json) BETWEEN 1 AND 262144),
    invalidation_json BLOB CHECK(invalidation_json IS NULL OR length(invalidation_json) BETWEEN 1 AND 65536),
    delivery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(delivery_state IN ('pending', 'leased', 'dispatching')),
    available_at TEXT NOT NULL,
    lease_owner TEXT,
    lease_token TEXT,
    lease_expires_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    lease_epoch INTEGER NOT NULL DEFAULT 0 CHECK(lease_epoch >= 0),
    dispatch_started INTEGER NOT NULL DEFAULT 0 CHECK(dispatch_started IN (0, 1)),
    expected_destination_generation INTEGER CHECK(expected_destination_generation IS NULL OR expected_destination_generation > 0),
    expected_destination_receipt_digest TEXT CHECK(expected_destination_receipt_digest IS NULL OR length(expected_destination_receipt_digest) = 71),
    created_at TEXT NOT NULL,
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    UNIQUE(installation_id, proposal_id, proposal_revision, event_kind),
    CHECK((event_kind = 'invalidation' AND invalidation_id IS NOT NULL
           AND invalidation_digest IS NOT NULL AND invalidation_json IS NOT NULL)
       OR (event_kind <> 'invalidation' AND invalidation_id IS NULL
           AND invalidation_digest IS NULL AND invalidation_json IS NULL)),
    CHECK((delivery_state IN ('leased', 'dispatching')) =
          (lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK((expected_destination_generation IS NULL) =
          (expected_destination_receipt_digest IS NULL))
) STRICT;
CREATE UNIQUE INDEX app_retrieval_delivery_invalidation_idx
    ON app_retrieval_delivery_outbox(installation_id, invalidation_id)
    WHERE invalidation_id IS NOT NULL;
CREATE INDEX app_retrieval_delivery_ready_idx
    ON app_retrieval_delivery_outbox(delivery_state, available_at, sequence);
CREATE INDEX app_retrieval_delivery_installation_idx
    ON app_retrieval_delivery_outbox(installation_id, dedupe_key, sequence);

CREATE TABLE app_retrieval_projection_heads (
    installation_id TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    proposal_id TEXT NOT NULL CHECK(length(proposal_id) BETWEEN 1 AND 192),
    proposal_revision INTEGER NOT NULL CHECK(proposal_revision > 0),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    proposal_payload_digest TEXT NOT NULL CHECK(length(proposal_payload_digest) = 71),
    source_event_ref TEXT NOT NULL CHECK(length(source_event_ref) BETWEEN 1 AND 192),
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    source_entity_name TEXT NOT NULL CHECK(length(source_entity_name) BETWEEN 1 AND 192),
    source_record_id TEXT NOT NULL CHECK(length(source_record_id) BETWEEN 1 AND 192),
    proposal_expires_at TEXT NOT NULL,
    proposal_json BLOB CHECK(proposal_json IS NULL OR length(proposal_json) BETWEEN 1 AND 262144),
    lifecycle_state TEXT NOT NULL CHECK(lifecycle_state IN ('pending', 'live', 'invalidating', 'expiring', 'settled')),
    latest_event_id TEXT NOT NULL CHECK(length(latest_event_id) BETWEEN 1 AND 192),
    latest_invalidation_id TEXT CHECK(latest_invalidation_id IS NULL OR length(latest_invalidation_id) BETWEEN 1 AND 192),
    latest_invalidation_digest TEXT CHECK(latest_invalidation_digest IS NULL OR length(latest_invalidation_digest) = 71),
    destination_generation INTEGER CHECK(destination_generation IS NULL OR destination_generation > 0),
    destination_receipt_digest TEXT CHECK(destination_receipt_digest IS NULL OR length(destination_receipt_digest) = 71),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, dedupe_key),
    CHECK((destination_generation IS NULL) = (destination_receipt_digest IS NULL))
) STRICT;

CREATE TABLE app_retrieval_delivery_terminal (
    sequence INTEGER NOT NULL UNIQUE,
    event_id TEXT PRIMARY KEY,
    event_kind TEXT NOT NULL CHECK(event_kind IN ('proposal', 'invalidation', 'expiration')),
    proposal_id TEXT NOT NULL CHECK(length(proposal_id) BETWEEN 1 AND 192),
    proposal_revision INTEGER NOT NULL CHECK(proposal_revision > 0),
    proposal_digest TEXT NOT NULL CHECK(length(proposal_digest) = 71),
    invalidation_id TEXT CHECK(invalidation_id IS NULL OR length(invalidation_id) BETWEEN 1 AND 192),
    invalidation_digest TEXT CHECK(invalidation_digest IS NULL OR length(invalidation_digest) = 71),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    principal TEXT NOT NULL CHECK(length(principal) BETWEEN 1 AND 192),
    workspace TEXT NOT NULL CHECK(length(workspace) BETWEEN 1 AND 192),
    scope_binding_ref TEXT NOT NULL CHECK(length(scope_binding_ref) BETWEEN 1 AND 192),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    dedupe_key TEXT NOT NULL CHECK(length(dedupe_key) BETWEEN 1 AND 192),
    source_event_ref TEXT NOT NULL CHECK(length(source_event_ref) BETWEEN 1 AND 192),
    source_event_revision INTEGER NOT NULL CHECK(source_event_revision > 0),
    source_identity_digest TEXT NOT NULL CHECK(length(source_identity_digest) = 71),
    payload_json BLOB CHECK(payload_json IS NULL OR length(payload_json) BETWEEN 1 AND 393216),
    terminal_state TEXT NOT NULL CHECK(terminal_state IN (
        'delivered', 'expired_before_delivery', 'invalidated_before_delivery'
    )),
    source_ack_receipt_id TEXT CHECK(source_ack_receipt_id IS NULL OR length(source_ack_receipt_id) BETWEEN 1 AND 192),
    source_ack_owner_identity_digest TEXT CHECK(source_ack_owner_identity_digest IS NULL OR length(source_ack_owner_identity_digest) = 71),
    source_ack_operation_digest TEXT CHECK(source_ack_operation_digest IS NULL OR length(source_ack_operation_digest) = 71),
    destination_generation INTEGER CHECK(destination_generation IS NULL OR destination_generation > 0),
    destination_receipt_digest TEXT CHECK(destination_receipt_digest IS NULL OR length(destination_receipt_digest) = 71),
    resulting_projection_digest TEXT CHECK(resulting_projection_digest IS NULL OR length(resulting_projection_digest) = 71),
    destination_recorded_at_ms INTEGER CHECK(destination_recorded_at_ms IS NULL OR destination_recorded_at_ms > 0),
    invalidation_disposition TEXT,
    terminalized_at TEXT NOT NULL,
    UNIQUE(installation_id, proposal_id, proposal_revision, event_kind),
    CHECK((destination_generation IS NULL) = (destination_receipt_digest IS NULL)),
    CHECK((terminal_state = 'delivered') =
          (source_ack_receipt_id IS NOT NULL
           AND source_ack_owner_identity_digest IS NOT NULL
           AND source_ack_operation_digest IS NOT NULL
           AND destination_generation IS NOT NULL
           AND destination_receipt_digest IS NOT NULL
           AND resulting_projection_digest IS NOT NULL
           AND destination_recorded_at_ms IS NOT NULL)),
    CHECK((event_kind = 'invalidation' AND invalidation_id IS NOT NULL
           AND invalidation_digest IS NOT NULL)
       OR (event_kind <> 'invalidation' AND invalidation_id IS NULL
           AND invalidation_digest IS NULL))
) STRICT;
CREATE UNIQUE INDEX app_retrieval_terminal_invalidation_idx
    ON app_retrieval_delivery_terminal(installation_id, invalidation_id)
    WHERE invalidation_id IS NOT NULL;
CREATE INDEX app_retrieval_terminal_recent_idx
    ON app_retrieval_delivery_terminal(sequence DESC);

CREATE TABLE app_retrieval_destination_ack_high_water (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    destination_generation INTEGER NOT NULL CHECK(destination_generation > 0),
    destination_receipt_digest TEXT NOT NULL CHECK(length(destination_receipt_digest) = 71),
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE app_retrieval_compaction_high_water (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    compacted_through_sequence INTEGER NOT NULL CHECK(compacted_through_sequence > 0),
    compacted_at TEXT NOT NULL
) STRICT;

CREATE TRIGGER app_retrieval_dispatch_identity_immutable
BEFORE UPDATE ON app_retrieval_delivery_outbox
WHEN OLD.delivery_state = 'dispatching' AND (
    NEW.event_id <> OLD.event_id OR NEW.event_kind <> OLD.event_kind OR
    NEW.payload_digest <> OLD.payload_digest OR NEW.proposal_json <> OLD.proposal_json OR
    NEW.invalidation_json IS NOT OLD.invalidation_json OR
    NEW.dispatch_started <> OLD.dispatch_started OR
    NEW.expected_destination_generation IS NOT OLD.expected_destination_generation OR
    NEW.expected_destination_receipt_digest IS NOT OLD.expected_destination_receipt_digest
)
BEGIN
    SELECT RAISE(ABORT, 'dispatching retrieval delivery identity is immutable');
END;
"#;

// P5.13 adds a payload-free interactive stop control. V12's control-kind
// CHECK is load-bearing, so existing registries require a transactional table
// rebuild rather than relying on application-side enum validation.
const APP_REGISTRY_SCHEMA_V19: &str = r#"
DROP TRIGGER app_workflow_control_blob_update_guard;
DROP TRIGGER app_workflow_control_blob_delete_guard;
DROP TRIGGER app_workflow_control_head_delete_guard;
DROP TRIGGER app_workflow_control_head_cas_guard;

CREATE TABLE app_workflow_control_blobs_v19 (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (
        control_kind IN ('task_binding', 'run_state', 'pause', 'interactive_stop')
    ),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    byte_count INTEGER NOT NULL CHECK (byte_count > 0),
    sealed_blob BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (task_id, execution_id, control_kind, generation),
    UNIQUE (task_id, execution_id, control_kind, generation, content_digest)
) STRICT;

CREATE TABLE app_workflow_control_heads_v19 (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (
        control_kind IN ('task_binding', 'run_state', 'pause', 'interactive_stop')
    ),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL CHECK (
        lifecycle_state IN ('prepared', 'active', 'claimed', 'consumed')
    ),
    claim_ref TEXT,
    claim_expires_at TEXT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    updated_at TEXT NOT NULL,
    CHECK (
        (lifecycle_state = 'claimed'
            AND claim_ref IS NOT NULL
            AND claim_expires_at IS NOT NULL)
        OR (lifecycle_state IN ('prepared', 'active', 'consumed')
            AND claim_ref IS NULL
            AND claim_expires_at IS NULL)
    ),
    PRIMARY KEY (task_id, execution_id, control_kind),
    FOREIGN KEY (task_id, execution_id, control_kind, generation, content_digest)
        REFERENCES app_workflow_control_blobs_v19(
            task_id, execution_id, control_kind, generation, content_digest
        )
) STRICT;

CREATE TABLE app_workflow_control_prepared_v19 (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (control_kind = 'pause'),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    base_revision INTEGER NOT NULL CHECK (base_revision >= 0),
    base_lifecycle TEXT CHECK (
        base_lifecycle IS NULL OR base_lifecycle IN ('active', 'claimed', 'consumed')
    ),
    base_claim_ref TEXT,
    proposal_ref TEXT NOT NULL,
    prepared_at TEXT NOT NULL,
    CHECK (
        (base_lifecycle = 'claimed' AND base_claim_ref IS NOT NULL)
        OR (base_lifecycle IS NULL AND base_revision = 0 AND base_claim_ref IS NULL)
        OR (base_lifecycle IN ('active', 'consumed') AND base_claim_ref IS NULL)
    ),
    PRIMARY KEY (task_id, execution_id, control_kind),
    FOREIGN KEY (task_id, execution_id, control_kind, generation, content_digest)
        REFERENCES app_workflow_control_blobs_v19(
            task_id, execution_id, control_kind, generation, content_digest
        )
) STRICT;

INSERT INTO app_workflow_control_blobs_v19
SELECT task_id, execution_id, control_kind, generation, content_digest,
       byte_count, sealed_blob, created_at
  FROM app_workflow_control_blobs;
INSERT INTO app_workflow_control_heads_v19
SELECT task_id, execution_id, control_kind, generation, content_digest,
       lifecycle_state, claim_ref, claim_expires_at, revision, updated_at
  FROM app_workflow_control_heads;
INSERT INTO app_workflow_control_prepared_v19
SELECT task_id, execution_id, control_kind, generation, content_digest,
       base_revision, base_lifecycle, base_claim_ref, proposal_ref, prepared_at
  FROM app_workflow_control_prepared;

DROP TABLE app_workflow_control_prepared;
DROP TABLE app_workflow_control_heads;
DROP TABLE app_workflow_control_blobs;
ALTER TABLE app_workflow_control_blobs_v19 RENAME TO app_workflow_control_blobs;
ALTER TABLE app_workflow_control_heads_v19 RENAME TO app_workflow_control_heads;
ALTER TABLE app_workflow_control_prepared_v19 RENAME TO app_workflow_control_prepared;

CREATE TRIGGER app_workflow_control_blob_update_guard
BEFORE UPDATE ON app_workflow_control_blobs
BEGIN
    SELECT RAISE(ABORT, 'app workflow control generations are immutable');
END;
CREATE TRIGGER app_workflow_control_blob_delete_guard
BEFORE DELETE ON app_workflow_control_blobs
WHEN EXISTS (
        SELECT 1 FROM app_workflow_control_heads h
         WHERE h.task_id = OLD.task_id
           AND h.execution_id = OLD.execution_id
           AND h.control_kind = OLD.control_kind
           AND h.generation = OLD.generation
           AND h.content_digest = OLD.content_digest
    )
    OR EXISTS (
        SELECT 1 FROM app_workflow_control_prepared p
         WHERE p.task_id = OLD.task_id
           AND p.execution_id = OLD.execution_id
           AND p.control_kind = OLD.control_kind
           AND p.generation = OLD.generation
           AND p.content_digest = OLD.content_digest
    )
BEGIN
    SELECT RAISE(ABORT, 'authoritative app workflow control blobs are retained');
END;
CREATE TRIGGER app_workflow_control_head_delete_guard
BEFORE DELETE ON app_workflow_control_heads
BEGIN
    SELECT RAISE(ABORT, 'app workflow control authority is retained');
END;
CREATE TRIGGER app_workflow_control_head_cas_guard
BEFORE UPDATE ON app_workflow_control_heads
WHEN NEW.revision != OLD.revision + 1
  OR NEW.generation < OLD.generation
  OR (OLD.lifecycle_state = 'consumed' AND NEW.generation = OLD.generation)
BEGIN
    SELECT RAISE(ABORT, 'app workflow control head requires monotonic CAS');
END;
"#;

// V20 makes the physical encryption posture an inspected part of the store,
// rather than a retention-contract promise. The row intentionally contains
// key identity and algorithm metadata only; key material remains in the OS
// keychain and derived process memory.
const APP_REGISTRY_SCHEMA_V20: &str = r#"
CREATE TABLE app_data_encryption_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL CHECK (format_version = 1),
    algorithm TEXT NOT NULL CHECK (
        algorithm = 'sqlcipher-4-aes256cbc-hmac-sha512'
    ),
    key_id TEXT NOT NULL CHECK (length(key_id) = 76),
    encrypted_from TEXT NOT NULL,
    last_rekeyed_at TEXT NOT NULL
) STRICT;

CREATE TRIGGER app_data_encryption_metadata_delete_guard
BEFORE DELETE ON app_data_encryption_metadata
BEGIN
    SELECT RAISE(ABORT, 'app data encryption metadata is required');
END;

CREATE TRIGGER app_data_encryption_metadata_identity_guard
BEFORE UPDATE OF singleton, format_version, algorithm, encrypted_from
ON app_data_encryption_metadata
BEGIN
    SELECT RAISE(ABORT, 'app data encryption format identity is immutable');
END;
"#;

// V21 turns the existing migration-run journal into the sole update
// coordinator owner. Target payloads are immutable and invisible to ordinary
// reads until registry_lifecycle promotes them in the same transaction as the
// installation-generation CAS and lifecycle outbox append.
const APP_REGISTRY_SCHEMA_V21: &str = r#"
CREATE TABLE app_migration_staged_records (
    migration_run_id TEXT NOT NULL,
    installation_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    record_id TEXT NOT NULL,
    source_record_revision INTEGER NOT NULL CHECK(source_record_revision > 0),
    source_change_seq INTEGER NOT NULL CHECK(source_change_seq > 0),
    target_dataset_generation INTEGER NOT NULL CHECK(target_dataset_generation > 0),
    target_schema_revision INTEGER NOT NULL CHECK(target_schema_revision > 0),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    record_json BLOB NOT NULL CHECK(length(record_json) BETWEEN 1 AND 16777216),
    created_at TEXT NOT NULL,
    PRIMARY KEY(migration_run_id, entity_name, record_id),
    FOREIGN KEY(migration_run_id) REFERENCES app_migration_runs(migration_run_id),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id)
) STRICT;
CREATE INDEX app_migration_staged_records_installation_idx
    ON app_migration_staged_records(installation_id, migration_run_id, entity_name, record_id);

CREATE TRIGGER app_migration_staged_records_update_guard
BEFORE UPDATE ON app_migration_staged_records
BEGIN
    SELECT RAISE(ABORT, 'staged migration records are immutable');
END;
"#;

// V22 owns the durable acceptance boundary for `app_behaviors_v1`. One
// current head per installation/behavior is enough for the single-host V1:
// package/grant changes fence and replace stale pending work, while a pending
// fire retains its deterministic identity across crash retries. Scope pause is
// a separate revision-CAS row so it cannot be confused with a manifest grant.
const APP_REGISTRY_SCHEMA_V22: &str = r#"
CREATE TABLE app_behavior_scope_policy (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL CHECK(revision > 0),
    paused INTEGER NOT NULL CHECK(paused IN (0, 1)),
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE app_behavior_scan_cursor (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    behavior_id TEXT NOT NULL CHECK(length(behavior_id) BETWEEN 1 AND 192),
    updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE app_behavior_heads (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    installation_generation INTEGER NOT NULL CHECK(installation_generation > 0),
    package_revision_ref TEXT NOT NULL CHECK(length(package_revision_ref) BETWEEN 1 AND 192),
    schema_revision INTEGER NOT NULL CHECK(schema_revision > 0),
    grant_revision INTEGER NOT NULL CHECK(grant_revision > 0),
    behavior_id TEXT NOT NULL CHECK(length(behavior_id) BETWEEN 1 AND 192),
    behavior_digest TEXT NOT NULL CHECK(length(behavior_digest) = 71),
    state TEXT NOT NULL CHECK(state IN ('idle', 'pending')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    fence INTEGER NOT NULL CHECK(fence >= 0),
    effective_interval_seconds INTEGER NOT NULL
        CHECK(effective_interval_seconds BETWEEN 60 AND 2678400),
    next_due_at TEXT NOT NULL,
    available_at TEXT NOT NULL,
    pending_scheduled_at TEXT,
    pending_fire_ref TEXT CHECK(
        pending_fire_ref IS NULL OR length(pending_fire_ref) BETWEEN 1 AND 192
    ),
    lease_owner TEXT CHECK(lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 192),
    lease_token TEXT CHECK(lease_token IS NULL OR length(lease_token) = 71),
    lease_expires_at TEXT,
    accepted_count INTEGER NOT NULL CHECK(accepted_count >= 0),
    attempt_count INTEGER NOT NULL CHECK(attempt_count >= 0),
    period_started_at TEXT NOT NULL,
    period_seconds INTEGER NOT NULL CHECK(period_seconds BETWEEN 60 AND 2678400),
    period_starts INTEGER NOT NULL CHECK(period_starts >= 0),
    max_starts_per_period INTEGER NOT NULL CHECK(max_starts_per_period BETWEEN 1 AND 10000),
    last_launch_ref TEXT CHECK(
        last_launch_ref IS NULL OR length(last_launch_ref) BETWEEN 1 AND 256
    ),
    last_error TEXT CHECK(last_error IS NULL OR length(last_error) <= 1024),
    invocation_json BLOB CHECK(
        invocation_json IS NULL OR length(invocation_json) BETWEEN 1 AND 1048576
    ),
    invocation_digest TEXT CHECK(
        invocation_digest IS NULL OR length(invocation_digest) = 71
    ),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, behavior_id),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    CHECK(period_starts <= max_starts_per_period),
    CHECK((state = 'pending') =
          (pending_scheduled_at IS NOT NULL AND pending_fire_ref IS NOT NULL
           AND invocation_json IS NOT NULL)),
    CHECK((lease_owner IS NULL) = (lease_token IS NULL)),
    CHECK((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK((invocation_json IS NULL) = (invocation_digest IS NULL)),
    CHECK(state = 'pending' OR invocation_json IS NULL),
    CHECK(state = 'pending' OR
          (lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL))
) STRICT;

CREATE INDEX app_behavior_heads_due_idx
    ON app_behavior_heads(state, available_at, next_due_at, installation_id, behavior_id);
CREATE INDEX app_behavior_heads_health_idx
    ON app_behavior_heads(updated_at DESC, installation_id, behavior_id);

CREATE TRIGGER app_behavior_scope_policy_cas_guard
BEFORE UPDATE ON app_behavior_scope_policy
WHEN NEW.revision != OLD.revision + 1
BEGIN
    SELECT RAISE(ABORT, 'app behavior scope policy requires revision CAS');
END;

CREATE TRIGGER app_behavior_head_cas_guard
BEFORE UPDATE ON app_behavior_heads
WHEN NEW.revision != OLD.revision + 1
  OR NEW.fence < OLD.fence
BEGIN
    SELECT RAISE(ABORT, 'app behavior head requires revision CAS and monotonic fence');
END;
"#;

// V23 adds payload-free retry/backoff health, scoped reconciliation history,
// and the behavior-local resource-ledger discriminator. It intentionally
// clears V22 pending invocation blobs while rebuilding the head table: older
// scheduler versions could persist selected entity payloads there, and those
// bytes must not survive as an unbounded retry copy.
const APP_REGISTRY_SCHEMA_V23: &str = r#"
-- Scheduler-minted behavior roots retain the installation-wide period row,
-- but also carry one immutable, digest-bound discriminator used for indexed
-- behavior-local monthly aggregation. Ordinary app roots keep NULL and their
-- existing accounting path is unchanged.
DROP TRIGGER app_resource_tree_identity_update_guard;
ALTER TABLE app_resource_trees ADD COLUMN behavior_ledger_digest TEXT
    CHECK(behavior_ledger_digest IS NULL OR length(behavior_ledger_digest) = 71);
CREATE INDEX app_resource_behavior_period_tree_idx
    ON app_resource_trees(
        installation_id, installation_generation, period_ref,
        behavior_ledger_digest
    ) WHERE behavior_ledger_digest IS NOT NULL;
CREATE TRIGGER app_resource_tree_identity_update_guard
BEFORE UPDATE OF
    budget_ledger_ref, installation_id, installation_generation,
    root_execution_id, period_ref, admitted_period_revision, lane,
    identity_json, behavior_ledger_digest
ON app_resource_trees
BEGIN
    SELECT RAISE(ABORT, 'app resource tree identity is immutable');
END;

CREATE TABLE app_behavior_reconcile_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    reconciled_at TEXT NOT NULL
) STRICT;

CREATE TABLE app_behavior_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    behavior_id TEXT CHECK(behavior_id IS NULL OR length(behavior_id) BETWEEN 1 AND 192),
    kind TEXT NOT NULL CHECK(kind IN ('retired', 'scan_fault')),
    reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 1024),
    observed_at TEXT NOT NULL
) STRICT;

CREATE INDEX app_behavior_events_recent_idx
    ON app_behavior_events(event_id DESC);

ALTER TABLE app_behavior_heads RENAME TO app_behavior_heads_v22;

CREATE TABLE app_behavior_heads (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    installation_generation INTEGER NOT NULL CHECK(installation_generation > 0),
    package_revision_ref TEXT NOT NULL CHECK(length(package_revision_ref) BETWEEN 1 AND 192),
    schema_revision INTEGER NOT NULL CHECK(schema_revision > 0),
    grant_revision INTEGER NOT NULL CHECK(grant_revision > 0),
    behavior_id TEXT NOT NULL CHECK(length(behavior_id) BETWEEN 1 AND 192),
    behavior_digest TEXT NOT NULL CHECK(length(behavior_digest) = 71),
    state TEXT NOT NULL CHECK(state IN ('idle', 'pending', 'blocked')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    fence INTEGER NOT NULL CHECK(fence >= 0),
    effective_interval_seconds INTEGER NOT NULL
        CHECK(effective_interval_seconds BETWEEN 60 AND 2678400),
    next_due_at TEXT NOT NULL,
    available_at TEXT NOT NULL,
    pending_scheduled_at TEXT,
    pending_fire_ref TEXT CHECK(
        pending_fire_ref IS NULL OR length(pending_fire_ref) BETWEEN 1 AND 192
    ),
    lease_owner TEXT CHECK(lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 192),
    lease_token TEXT CHECK(lease_token IS NULL OR length(lease_token) = 71),
    lease_expires_at TEXT,
    accepted_count INTEGER NOT NULL CHECK(accepted_count >= 0),
    attempt_count INTEGER NOT NULL CHECK(attempt_count BETWEEN 0 AND 4294967295),
    consecutive_failures INTEGER NOT NULL CHECK(consecutive_failures BETWEEN 0 AND 64),
    period_started_at TEXT NOT NULL,
    period_seconds INTEGER NOT NULL CHECK(period_seconds BETWEEN 60 AND 2678400),
    period_starts INTEGER NOT NULL CHECK(period_starts >= 0),
    max_starts_per_period INTEGER NOT NULL CHECK(max_starts_per_period BETWEEN 1 AND 10000),
    last_launch_ref TEXT CHECK(
        last_launch_ref IS NULL OR length(last_launch_ref) BETWEEN 1 AND 256
    ),
    last_error TEXT CHECK(last_error IS NULL OR length(last_error) <= 1024),
    invocation_json BLOB CHECK(
        invocation_json IS NULL OR length(invocation_json) BETWEEN 1 AND 1048576
    ),
    invocation_digest TEXT CHECK(
        invocation_digest IS NULL OR length(invocation_digest) = 71
    ),
    updated_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, behavior_id),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    CHECK(period_starts <= max_starts_per_period),
    CHECK((state = 'pending') =
          (pending_scheduled_at IS NOT NULL AND pending_fire_ref IS NOT NULL
           AND invocation_json IS NOT NULL)),
    CHECK((lease_owner IS NULL) = (lease_token IS NULL)),
    CHECK((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK((invocation_json IS NULL) = (invocation_digest IS NULL)),
    CHECK(state = 'pending' OR invocation_json IS NULL),
    CHECK(state = 'pending' OR
          (pending_scheduled_at IS NULL AND pending_fire_ref IS NULL)),
    CHECK(state = 'pending' OR
          (lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL))
) STRICT;

INSERT INTO app_behavior_heads(
    installation_id, installation_generation, package_revision_ref,
    schema_revision, grant_revision, behavior_id, behavior_digest,
    state, revision, fence, effective_interval_seconds, next_due_at,
    available_at, pending_scheduled_at, pending_fire_ref, lease_owner,
    lease_token, lease_expires_at, accepted_count, attempt_count,
    consecutive_failures, period_started_at, period_seconds, period_starts,
    max_starts_per_period, last_launch_ref, last_error, invocation_json,
    invocation_digest, updated_at
)
SELECT
    installation_id, installation_generation, package_revision_ref,
    schema_revision, grant_revision, behavior_id, behavior_digest,
    'idle', revision + CASE WHEN state = 'pending' THEN 1 ELSE 0 END,
    fence + CASE WHEN state = 'pending' THEN 1 ELSE 0 END,
    effective_interval_seconds, next_due_at, available_at,
    NULL, NULL, NULL, NULL, NULL, accepted_count, attempt_count,
    CASE WHEN state = 'pending' THEN 1 ELSE 0 END,
    period_started_at, period_seconds, period_starts, max_starts_per_period,
    last_launch_ref,
    CASE WHEN state = 'pending'
         THEN 'legacy_pending_payload_cleared'
         WHEN last_error IS NOT NULL THEN 'legacy_error_redacted'
         ELSE NULL END,
    NULL, NULL, updated_at
FROM app_behavior_heads_v22;

DROP TABLE app_behavior_heads_v22;

CREATE INDEX app_behavior_heads_due_idx
    ON app_behavior_heads(state, available_at, next_due_at, installation_id, behavior_id);
CREATE INDEX app_behavior_heads_health_idx
    ON app_behavior_heads(updated_at DESC, installation_id, behavior_id);

CREATE TRIGGER app_behavior_head_cas_guard
BEFORE UPDATE ON app_behavior_heads
WHEN NEW.revision != OLD.revision + 1
  OR NEW.fence < OLD.fence
BEGIN
    SELECT RAISE(ABORT, 'app behavior head requires revision CAS and monotonic fence');
END;
"#;

// V24 replaces the broad state/availability index with two due-time spines.
// Idle availability normally remains in the past, so leading with it forced
// every scheduler tick to walk nearly the whole head table. Pending lease
// recovery has a different due key and therefore owns an independent index.
const APP_REGISTRY_SCHEMA_V24: &str = r#"
UPDATE app_behavior_heads
   SET last_error = 'legacy_error_redacted',
       revision = revision + 1
 WHERE last_error IS NOT NULL
   AND last_error NOT IN (
       'execution_binding_missing', 'installation_scan_failed',
       'behavior_scan_failed', 'workflow_launch_retryable',
       'workflow_launch_blocked', 'source_record_removed',
       'source_record_missing', 'source_binding_changed',
       'legacy_pending_payload_cleared', 'legacy_error_redacted'
   );
UPDATE app_behavior_events
   SET reason = CASE kind
       WHEN 'scan_fault' THEN 'legacy_scan_fault_redacted'
       ELSE 'legacy_retirement_redacted'
   END
 WHERE reason NOT IN (
       'installation_disabled', 'behavior_binding_removed',
       'installation_scan_failed', 'behavior_scan_failed',
       'legacy_scan_fault_redacted', 'legacy_retirement_redacted'
   );
DROP INDEX app_behavior_heads_due_idx;
CREATE INDEX app_behavior_heads_idle_due_idx
    ON app_behavior_heads(next_due_at, available_at, installation_id, behavior_id)
    WHERE state = 'idle';
CREATE INDEX app_behavior_heads_pending_due_idx
    ON app_behavior_heads(lease_expires_at, available_at, installation_id, behavior_id)
    WHERE state = 'pending';
"#;

// V25 makes background-behavior inventory reconciliation resumable. The last
// fully completed timestamp remains separate from the active pass/cursor, so a
// cancelled partial pass cannot postpone reconciliation for another interval.
const APP_REGISTRY_SCHEMA_V25: &str = r#"
ALTER TABLE app_behavior_reconcile_state ADD COLUMN pass_started_at TEXT;
ALTER TABLE app_behavior_reconcile_state ADD COLUMN cursor_installation_id TEXT
    CHECK(cursor_installation_id IS NULL OR
          (pass_started_at IS NOT NULL AND
           length(cursor_installation_id) BETWEEN 1 AND 128));
ALTER TABLE app_behavior_reconcile_state ADD COLUMN deferred_installation_ids_json BLOB
    NOT NULL DEFAULT X'5b5d'
    CHECK(length(deferred_installation_ids_json) BETWEEN 2 AND 65536);
"#;

// V26 owns the two item-4 acceptance ledgers. Event fires are keyed by the
// host's canonical source-event reference, never by app bytes; notification
// rows combine idempotency, rate admission and delivery debt in one registry
// transaction so a caller cannot observe `accepted` before durable work exists.
const APP_REGISTRY_SCHEMA_V26: &str = r#"
CREATE TABLE app_event_ingress_receipts (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    event_ref TEXT NOT NULL CHECK(length(event_ref) BETWEEN 1 AND 512),
    event_kind TEXT NOT NULL CHECK(length(event_kind) BETWEEN 1 AND 96),
    projection_json BLOB NOT NULL CHECK(length(projection_json) BETWEEN 2 AND 65536),
    projection_digest TEXT NOT NULL CHECK(length(projection_digest) = 71),
    accepted_fanout INTEGER NOT NULL CHECK(accepted_fanout BETWEEN 0 AND 32),
    created_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, event_ref)
) STRICT;

CREATE TRIGGER app_event_ingress_receipt_immutable
BEFORE UPDATE ON app_event_ingress_receipts
BEGIN
    SELECT RAISE(ABORT, 'app event ingress receipt is immutable');
END;

CREATE TABLE app_event_behavior_fires (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    event_behavior_id TEXT NOT NULL CHECK(length(event_behavior_id) BETWEEN 1 AND 192),
    event_ref TEXT NOT NULL CHECK(length(event_ref) BETWEEN 1 AND 512),
    installation_generation INTEGER NOT NULL CHECK(installation_generation > 0),
    package_revision_ref TEXT NOT NULL CHECK(length(package_revision_ref) BETWEEN 1 AND 192),
    schema_revision INTEGER NOT NULL CHECK(schema_revision > 0),
    grant_revision INTEGER NOT NULL CHECK(grant_revision > 0),
    reviewed_request_digest TEXT NOT NULL CHECK(length(reviewed_request_digest) = 71),
    event_kind TEXT NOT NULL CHECK(length(event_kind) BETWEEN 1 AND 96),
    projection_json BLOB NOT NULL CHECK(length(projection_json) BETWEEN 2 AND 65536),
    projection_digest TEXT NOT NULL CHECK(length(projection_digest) = 71),
    causation_depth INTEGER NOT NULL CHECK(causation_depth BETWEEN 0 AND 16),
    causation_path_digest TEXT NOT NULL CHECK(length(causation_path_digest) = 71),
    state TEXT NOT NULL CHECK(state IN ('pending', 'leased', 'accepted', 'dead_letter')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    fence INTEGER NOT NULL CHECK(fence >= 0),
    attempt_count INTEGER NOT NULL CHECK(attempt_count BETWEEN 0 AND 64),
    rate_admitted_at TEXT,
    available_at TEXT NOT NULL,
    lease_owner TEXT CHECK(lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 192),
    lease_token TEXT CHECK(lease_token IS NULL OR length(lease_token) = 71),
    lease_expires_at TEXT,
    launch_ref TEXT CHECK(launch_ref IS NULL OR length(launch_ref) BETWEEN 1 AND 512),
    last_error TEXT CHECK(last_error IS NULL OR length(last_error) BETWEEN 1 AND 1024),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, event_behavior_id, event_ref),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    FOREIGN KEY(installation_id, event_ref)
        REFERENCES app_event_ingress_receipts(installation_id, event_ref),
    CHECK((state = 'leased' AND lease_owner IS NOT NULL
           AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)
       OR (state <> 'leased' AND lease_owner IS NULL
           AND lease_token IS NULL AND lease_expires_at IS NULL)),
    CHECK(state <> 'accepted'
       OR (launch_ref IS NOT NULL AND rate_admitted_at IS NOT NULL))
) STRICT;

CREATE INDEX app_event_behavior_fires_due_idx
    ON app_event_behavior_fires(
        attempt_count, fence, available_at,
        installation_id, event_behavior_id, event_ref
    )
    WHERE state = 'pending';
CREATE INDEX app_event_behavior_fires_lease_idx
    ON app_event_behavior_fires(lease_expires_at, installation_id, event_behavior_id, event_ref)
    WHERE state = 'leased';
CREATE INDEX app_event_behavior_fires_health_idx
    ON app_event_behavior_fires(updated_at DESC, installation_id, event_behavior_id, event_ref);
CREATE INDEX app_event_behavior_fires_event_idx
    ON app_event_behavior_fires(installation_id, event_ref);

CREATE TRIGGER app_event_behavior_fire_cas_guard
BEFORE UPDATE ON app_event_behavior_fires
WHEN NEW.revision != OLD.revision + 1 OR NEW.fence < OLD.fence
BEGIN
    SELECT RAISE(ABORT, 'app event-behavior fire requires revision CAS and monotonic fence');
END;

CREATE TRIGGER app_event_behavior_fire_identity_guard
BEFORE UPDATE OF
    installation_id, event_behavior_id, event_ref, installation_generation,
    package_revision_ref, schema_revision, grant_revision,
    reviewed_request_digest, event_kind, projection_json, projection_digest,
    causation_depth, causation_path_digest, created_at
ON app_event_behavior_fires
BEGIN
    SELECT RAISE(ABORT, 'app event-behavior fire identity is immutable');
END;

CREATE TABLE app_event_behavior_periods (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    event_behavior_id TEXT NOT NULL CHECK(length(event_behavior_id) BETWEEN 1 AND 192),
    reviewed_request_digest TEXT NOT NULL CHECK(length(reviewed_request_digest) = 71),
    period_seconds INTEGER NOT NULL CHECK(period_seconds BETWEEN 60 AND 2678400),
    period_started_at TEXT NOT NULL,
    starts INTEGER NOT NULL CHECK(starts BETWEEN 0 AND 10000),
    last_started_at TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, event_behavior_id),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id)
) STRICT;

CREATE TABLE app_owner_notification_outbox (
    correlation_id TEXT PRIMARY KEY CHECK(length(correlation_id) BETWEEN 1 AND 192),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    installation_generation INTEGER NOT NULL CHECK(installation_generation > 0),
    workflow_id TEXT NOT NULL CHECK(length(workflow_id) BETWEEN 1 AND 192),
    port_id TEXT NOT NULL CHECK(length(port_id) BETWEEN 1 AND 192),
    reviewed_request_digest TEXT NOT NULL CHECK(length(reviewed_request_digest) = 71),
    period_seconds INTEGER NOT NULL CHECK(period_seconds BETWEEN 60 AND 2678400),
    effect_ref TEXT NOT NULL CHECK(length(effect_ref) BETWEEN 1 AND 512),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    payload_json BLOB NOT NULL CHECK(length(payload_json) BETWEEN 2 AND 32768),
    severity TEXT NOT NULL CHECK(severity IN ('info', 'warning')),
    state TEXT NOT NULL CHECK(state IN ('pending', 'leased', 'delivered', 'dead_letter')),
    revision INTEGER NOT NULL CHECK(revision > 0),
    fence INTEGER NOT NULL CHECK(fence >= 0),
    attempt_count INTEGER NOT NULL CHECK(attempt_count BETWEEN 0 AND 64),
    available_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    lease_owner TEXT CHECK(lease_owner IS NULL OR length(lease_owner) BETWEEN 1 AND 192),
    lease_token TEXT CHECK(lease_token IS NULL OR length(lease_token) = 71),
    lease_expires_at TEXT,
    user_request_id TEXT CHECK(user_request_id IS NULL OR length(user_request_id) BETWEEN 1 AND 192),
    last_error TEXT CHECK(last_error IS NULL OR length(last_error) BETWEEN 1 AND 1024),
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    UNIQUE(installation_id, workflow_id, port_id, effect_ref),
    CHECK((state = 'leased' AND lease_owner IS NOT NULL
           AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)
       OR (state <> 'leased' AND lease_owner IS NULL
           AND lease_token IS NULL AND lease_expires_at IS NULL)),
    CHECK((state = 'delivered' AND user_request_id IS NOT NULL
           AND delivered_at IS NOT NULL)
       OR (state <> 'delivered' AND user_request_id IS NULL
           AND delivered_at IS NULL))
) STRICT;

CREATE INDEX app_owner_notification_outbox_due_idx
    ON app_owner_notification_outbox(
        attempt_count, fence, available_at, created_at, correlation_id
    )
    WHERE state = 'pending';
CREATE INDEX app_owner_notification_outbox_lease_idx
    ON app_owner_notification_outbox(
        lease_expires_at, installation_id, workflow_id, port_id, correlation_id
    )
    WHERE state = 'leased';
CREATE INDEX app_owner_notification_outbox_volume_idx
    ON app_owner_notification_outbox(
        installation_id, workflow_id, port_id, created_at
    );
CREATE INDEX app_owner_notification_outbox_pending_idx
    ON app_owner_notification_outbox(installation_id, workflow_id, port_id, state, expires_at);

CREATE TRIGGER app_owner_notification_outbox_cas_guard
BEFORE UPDATE ON app_owner_notification_outbox
WHEN NEW.revision != OLD.revision + 1 OR NEW.fence < OLD.fence
BEGIN
    SELECT RAISE(ABORT, 'app owner-notification outbox requires revision CAS and monotonic fence');
END;

CREATE TRIGGER app_owner_notification_outbox_identity_guard
BEFORE UPDATE OF
    correlation_id, installation_id, installation_generation, workflow_id,
    port_id, reviewed_request_digest, period_seconds, effect_ref, payload_digest,
    payload_json, severity, expires_at, created_at
ON app_owner_notification_outbox
BEGIN
    SELECT RAISE(ABORT, 'app owner-notification identity is immutable');
END;
"#;

// V27 is the compatibility boundary for the item-4 schema after V26 had
// already escaped into development stores. Older V26 stores did not carry the
// sealed notification period or the event-ref lookup spine. The Rust migration
// adds the column only when absent; these idempotent objects then make the
// upgraded shape structurally distinguishable from every V26 variant.
const APP_REGISTRY_SCHEMA_V27: &str = r#"
CREATE INDEX IF NOT EXISTS app_event_behavior_fires_event_idx
    ON app_event_behavior_fires(installation_id, event_ref);

CREATE TRIGGER app_owner_notification_outbox_period_immutable
BEFORE UPDATE OF period_seconds ON app_owner_notification_outbox
BEGIN
    SELECT RAISE(ABORT, 'app owner-notification reviewed period is immutable');
END;
"#;

// V28 bounds protected terminal payload retention without weakening replay
// identity. Tombstones preserve only host identity and cryptographic digests;
// live payload rows can be deleted only by the transactional compactors after
// their complete terminal fanout/window proofs succeed.
const APP_REGISTRY_SCHEMA_V28: &str = r#"
CREATE TABLE app_event_ingress_tombstones (
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    event_ref TEXT NOT NULL CHECK(length(event_ref) BETWEEN 1 AND 512),
    event_kind TEXT NOT NULL CHECK(length(event_kind) BETWEEN 1 AND 96),
    projection_digest TEXT NOT NULL CHECK(length(projection_digest) = 71),
    accepted_fanout INTEGER NOT NULL CHECK(accepted_fanout BETWEEN 0 AND 32),
    receipt_created_at TEXT NOT NULL,
    compacted_at TEXT NOT NULL,
    PRIMARY KEY(installation_id, event_ref),
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id)
) STRICT;

CREATE TRIGGER app_event_ingress_tombstone_immutable
BEFORE UPDATE ON app_event_ingress_tombstones
BEGIN
    SELECT RAISE(ABORT, 'app event ingress tombstone is immutable');
END;

CREATE INDEX app_event_ingress_receipts_retention_idx
    ON app_event_ingress_receipts(created_at, installation_id, event_ref);

CREATE TABLE app_owner_notification_tombstones (
    correlation_id TEXT PRIMARY KEY CHECK(length(correlation_id) BETWEEN 1 AND 192),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    installation_generation INTEGER NOT NULL CHECK(installation_generation > 0),
    workflow_id TEXT NOT NULL CHECK(length(workflow_id) BETWEEN 1 AND 192),
    port_id TEXT NOT NULL CHECK(length(port_id) BETWEEN 1 AND 192),
    reviewed_request_digest TEXT NOT NULL CHECK(length(reviewed_request_digest) = 71),
    period_seconds INTEGER NOT NULL CHECK(period_seconds BETWEEN 60 AND 2678400),
    effect_ref TEXT NOT NULL CHECK(length(effect_ref) BETWEEN 1 AND 512),
    payload_digest TEXT NOT NULL CHECK(length(payload_digest) = 71),
    severity TEXT NOT NULL CHECK(severity IN ('info', 'warning')),
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    compacted_at TEXT NOT NULL,
    FOREIGN KEY(installation_id) REFERENCES app_installations(installation_id),
    UNIQUE(installation_id, workflow_id, port_id, effect_ref)
) STRICT;

CREATE TRIGGER app_owner_notification_tombstone_immutable
BEFORE UPDATE ON app_owner_notification_tombstones
BEGIN
    SELECT RAISE(ABORT, 'app owner-notification tombstone is immutable');
END;

CREATE INDEX app_owner_notification_tombstones_effect_idx
    ON app_owner_notification_tombstones(
        installation_id, workflow_id, port_id, effect_ref
    );
CREATE INDEX app_owner_notification_outbox_retention_idx
    ON app_owner_notification_outbox(expires_at, created_at, correlation_id)
    WHERE state IN ('delivered', 'dead_letter');

DROP INDEX IF EXISTS app_event_behavior_fires_due_idx;
CREATE INDEX app_event_behavior_fires_due_idx
    ON app_event_behavior_fires(
        attempt_count, fence, available_at,
        installation_id, event_behavior_id, event_ref
    )
    WHERE state = 'pending';

CREATE INDEX app_event_behavior_fires_expired_priority_idx
    ON app_event_behavior_fires(
        attempt_count, fence, lease_expires_at,
        installation_id, event_behavior_id, event_ref
    )
    WHERE state = 'leased';

DROP INDEX IF EXISTS app_owner_notification_outbox_due_idx;
CREATE INDEX app_owner_notification_outbox_due_idx
    ON app_owner_notification_outbox(
        attempt_count, fence, available_at, created_at, correlation_id
    )
    WHERE state = 'pending';
"#;

// V29 gives bounded terminal compactors a content-free, immutable place to
// isolate poison rows. The original payload-bearing ledger remains untouched
// for forensic/conflict authority, while the marker lets later raw cursor pages
// skip repeated integrity work without treating the row as clean authority.
const APP_REGISTRY_SCHEMA_V29: &str = r#"
CREATE TABLE app_terminal_compaction_quarantine (
    candidate_kind TEXT NOT NULL
        CHECK(candidate_kind IN ('event_ingress', 'owner_notification')),
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    candidate_ref TEXT NOT NULL CHECK(length(candidate_ref) BETWEEN 1 AND 512),
    reason_code TEXT NOT NULL
        CHECK(reason_code IN ('integrity_verification_failed', 'compaction_state_conflict')),
    quarantined_at TEXT NOT NULL,
    PRIMARY KEY(candidate_kind, installation_id, candidate_ref)
) STRICT;

CREATE TRIGGER app_terminal_compaction_quarantine_immutable
BEFORE UPDATE ON app_terminal_compaction_quarantine
BEGIN
    SELECT RAISE(ABORT, 'app terminal-compaction quarantine marker is immutable');
END;

CREATE INDEX app_terminal_compaction_quarantine_installation_idx
    ON app_terminal_compaction_quarantine(
        installation_id, candidate_kind, candidate_ref
    );

CREATE INDEX app_owner_notification_outbox_expiry_idx
    ON app_owner_notification_outbox(expires_at, fence, correlation_id)
    WHERE state IN ('pending', 'leased');

DROP INDEX IF EXISTS app_event_behavior_fires_expired_priority_idx;
CREATE INDEX app_event_behavior_fires_expired_priority_idx
    ON app_event_behavior_fires(
        attempt_count, fence, lease_expires_at,
        installation_id, event_behavior_id, event_ref
    )
    WHERE state = 'leased';
"#;

// V30 makes terminal-compaction discovery physically bounded. Each scoped
// registry owns two content-free identity cursors per candidate kind: the
// forward rail walks a fixed epoch snapshot, while the revisit rail receives
// one page after a bounded number of forward pages. New ingress cannot extend
// either active epoch, deleted cursor keys remain valid tuple seek positions,
// and reaching an epoch end resets that rail for the following sweep.
const APP_REGISTRY_SCHEMA_V30: &str = r#"
CREATE TABLE app_terminal_compaction_cursors (
    candidate_kind TEXT PRIMARY KEY
        CHECK(candidate_kind IN ('event_ingress', 'owner_notification')),
    forward_after_installation_id TEXT
        CHECK(forward_after_installation_id IS NULL OR
              length(forward_after_installation_id) BETWEEN 1 AND 128),
    forward_after_candidate_ref TEXT
        CHECK(forward_after_candidate_ref IS NULL OR
              length(forward_after_candidate_ref) BETWEEN 1 AND 512),
    forward_epoch_end_installation_id TEXT
        CHECK(forward_epoch_end_installation_id IS NULL OR
              length(forward_epoch_end_installation_id) BETWEEN 1 AND 128),
    forward_epoch_end_candidate_ref TEXT
        CHECK(forward_epoch_end_candidate_ref IS NULL OR
              length(forward_epoch_end_candidate_ref) BETWEEN 1 AND 512),
    revisit_after_installation_id TEXT
        CHECK(revisit_after_installation_id IS NULL OR
              length(revisit_after_installation_id) BETWEEN 1 AND 128),
    revisit_after_candidate_ref TEXT
        CHECK(revisit_after_candidate_ref IS NULL OR
              length(revisit_after_candidate_ref) BETWEEN 1 AND 512),
    revisit_epoch_end_installation_id TEXT
        CHECK(revisit_epoch_end_installation_id IS NULL OR
              length(revisit_epoch_end_installation_id) BETWEEN 1 AND 128),
    revisit_epoch_end_candidate_ref TEXT
        CHECK(revisit_epoch_end_candidate_ref IS NULL OR
              length(revisit_epoch_end_candidate_ref) BETWEEN 1 AND 512),
    forward_pages_since_revisit INTEGER NOT NULL DEFAULT 0
        CHECK(forward_pages_since_revisit BETWEEN 0 AND 64),
    updated_at TEXT NOT NULL,
    CHECK(
        (candidate_kind = 'event_ingress'
         AND ((forward_after_installation_id IS NULL
               AND forward_after_candidate_ref IS NULL)
              OR (forward_after_installation_id IS NOT NULL
                  AND forward_after_candidate_ref IS NOT NULL))
         AND ((forward_epoch_end_installation_id IS NULL
               AND forward_epoch_end_candidate_ref IS NULL)
              OR (forward_epoch_end_installation_id IS NOT NULL
                  AND forward_epoch_end_candidate_ref IS NOT NULL))
         AND ((revisit_after_installation_id IS NULL
               AND revisit_after_candidate_ref IS NULL)
              OR (revisit_after_installation_id IS NOT NULL
                  AND revisit_after_candidate_ref IS NOT NULL))
         AND ((revisit_epoch_end_installation_id IS NULL
               AND revisit_epoch_end_candidate_ref IS NULL)
              OR (revisit_epoch_end_installation_id IS NOT NULL
                  AND revisit_epoch_end_candidate_ref IS NOT NULL))
         AND (forward_after_installation_id IS NULL
              OR forward_epoch_end_installation_id IS NOT NULL)
         AND (revisit_after_installation_id IS NULL
              OR revisit_epoch_end_installation_id IS NOT NULL))
        OR
        (candidate_kind = 'owner_notification'
         AND forward_after_installation_id IS NULL
         AND forward_epoch_end_installation_id IS NULL
         AND revisit_after_installation_id IS NULL
         AND revisit_epoch_end_installation_id IS NULL
         AND (forward_after_candidate_ref IS NULL
              OR forward_epoch_end_candidate_ref IS NOT NULL)
         AND (revisit_after_candidate_ref IS NULL
              OR revisit_epoch_end_candidate_ref IS NOT NULL))
    )
) STRICT;
"#;

// V31 durably records attempt refunds owed after UserRequest capacity
// backpressure. The row contains only outbox identity/CAS metadata, never a
// notification payload. A nullable application timestamp makes creation and
// application restartable while the primary key makes every charged attempt
// idempotent; the parent outbox retention policy bounds the receipts by
// deleting them through the foreign-key cascade.
const APP_REGISTRY_SCHEMA_V31: &str = r#"
CREATE TABLE app_owner_notification_attempt_refunds (
    correlation_id TEXT NOT NULL CHECK(length(correlation_id) BETWEEN 1 AND 192),
    charged_revision INTEGER NOT NULL CHECK(charged_revision > 0),
    charged_fence INTEGER NOT NULL CHECK(charged_fence >= 0),
    charged_lease_owner TEXT NOT NULL
        CHECK(length(charged_lease_owner) BETWEEN 1 AND 192),
    charged_lease_token TEXT NOT NULL CHECK(length(charged_lease_token) = 71),
    created_at TEXT NOT NULL,
    applied_at TEXT,
    PRIMARY KEY(correlation_id, charged_revision, charged_fence),
    FOREIGN KEY(correlation_id) REFERENCES app_owner_notification_outbox(correlation_id)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX app_owner_notification_attempt_refunds_pending_idx
    ON app_owner_notification_attempt_refunds(
        created_at, correlation_id, charged_revision, charged_fence
    ) WHERE applied_at IS NULL;

CREATE TRIGGER app_owner_notification_attempt_refunds_guard
BEFORE UPDATE ON app_owner_notification_attempt_refunds
WHEN NEW.correlation_id != OLD.correlation_id
  OR NEW.charged_revision != OLD.charged_revision
  OR NEW.charged_fence != OLD.charged_fence
  OR NEW.charged_lease_owner != OLD.charged_lease_owner
  OR NEW.charged_lease_token != OLD.charged_lease_token
  OR NEW.created_at != OLD.created_at
  OR NEW.applied_at IS NULL
  OR OLD.applied_at IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'app owner-notification refund receipt is immutable after application');
END;
"#;

// V32 adds the correlation-first partial spine used by every hot pending-count
// and correlated-NOT-EXISTS probe. The V31 created-at-first index remains the
// ordered global repair rail; neither index includes applied receipts.
const APP_REGISTRY_SCHEMA_V32: &str = r#"
CREATE INDEX app_owner_notification_attempt_refunds_correlation_idx
    ON app_owner_notification_attempt_refunds(
        correlation_id, charged_revision, charged_fence
    ) WHERE applied_at IS NULL;
"#;

// V33 admits the workflow cancellation owner already used by the runtime.
// Copy all blobs, heads and prepared pauses transactionally, preserving their
// exact bytes, generations, claims and retention/CAS triggers.
const APP_REGISTRY_SCHEMA_V33: &str = r#"
DROP TRIGGER app_workflow_control_blob_update_guard;
DROP TRIGGER app_workflow_control_blob_delete_guard;
DROP TRIGGER app_workflow_control_head_delete_guard;
DROP TRIGGER app_workflow_control_head_cas_guard;

CREATE TABLE app_workflow_control_blobs_v33 (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (
        control_kind IN ('task_binding', 'run_state', 'pause', 'interactive_stop', 'action_cancellation')
    ),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    byte_count INTEGER NOT NULL CHECK (byte_count > 0),
    sealed_blob BLOB NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (task_id, execution_id, control_kind, generation),
    UNIQUE (task_id, execution_id, control_kind, generation, content_digest)
) STRICT;

CREATE TABLE app_workflow_control_heads_v33 (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (
        control_kind IN ('task_binding', 'run_state', 'pause', 'interactive_stop', 'action_cancellation')
    ),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    lifecycle_state TEXT NOT NULL CHECK (
        lifecycle_state IN ('prepared', 'active', 'claimed', 'consumed')
    ),
    claim_ref TEXT,
    claim_expires_at TEXT,
    revision INTEGER NOT NULL CHECK (revision > 0),
    updated_at TEXT NOT NULL,
    CHECK (
        (lifecycle_state = 'claimed'
            AND claim_ref IS NOT NULL
            AND claim_expires_at IS NOT NULL)
        OR (lifecycle_state IN ('prepared', 'active', 'consumed')
            AND claim_ref IS NULL
            AND claim_expires_at IS NULL)
    ),
    PRIMARY KEY (task_id, execution_id, control_kind),
    FOREIGN KEY (task_id, execution_id, control_kind, generation, content_digest)
        REFERENCES app_workflow_control_blobs_v33(
            task_id, execution_id, control_kind, generation, content_digest
        )
) STRICT;

CREATE TABLE app_workflow_control_prepared_v33 (
    task_id TEXT NOT NULL,
    execution_id TEXT NOT NULL,
    control_kind TEXT NOT NULL CHECK (control_kind = 'pause'),
    generation INTEGER NOT NULL CHECK (generation > 0),
    content_digest TEXT NOT NULL,
    base_revision INTEGER NOT NULL CHECK (base_revision >= 0),
    base_lifecycle TEXT CHECK (
        base_lifecycle IS NULL OR base_lifecycle IN ('active', 'claimed', 'consumed')
    ),
    base_claim_ref TEXT,
    proposal_ref TEXT NOT NULL,
    prepared_at TEXT NOT NULL,
    CHECK (
        (base_lifecycle = 'claimed' AND base_claim_ref IS NOT NULL)
        OR (base_lifecycle IS NULL AND base_revision = 0 AND base_claim_ref IS NULL)
        OR (base_lifecycle IN ('active', 'consumed') AND base_claim_ref IS NULL)
    ),
    PRIMARY KEY (task_id, execution_id, control_kind),
    FOREIGN KEY (task_id, execution_id, control_kind, generation, content_digest)
        REFERENCES app_workflow_control_blobs_v33(
            task_id, execution_id, control_kind, generation, content_digest
        )
) STRICT;

INSERT INTO app_workflow_control_blobs_v33
SELECT task_id, execution_id, control_kind, generation, content_digest,
       byte_count, sealed_blob, created_at
  FROM app_workflow_control_blobs;
INSERT INTO app_workflow_control_heads_v33
SELECT task_id, execution_id, control_kind, generation, content_digest,
       lifecycle_state, claim_ref, claim_expires_at, revision, updated_at
  FROM app_workflow_control_heads;
INSERT INTO app_workflow_control_prepared_v33
SELECT task_id, execution_id, control_kind, generation, content_digest,
       base_revision, base_lifecycle, base_claim_ref, proposal_ref, prepared_at
  FROM app_workflow_control_prepared;

DROP TABLE app_workflow_control_prepared;
DROP TABLE app_workflow_control_heads;
DROP TABLE app_workflow_control_blobs;
ALTER TABLE app_workflow_control_blobs_v33 RENAME TO app_workflow_control_blobs;
ALTER TABLE app_workflow_control_heads_v33 RENAME TO app_workflow_control_heads;
ALTER TABLE app_workflow_control_prepared_v33 RENAME TO app_workflow_control_prepared;

CREATE TRIGGER app_workflow_control_blob_update_guard
BEFORE UPDATE ON app_workflow_control_blobs
BEGIN
    SELECT RAISE(ABORT, 'app workflow control generations are immutable');
END;
CREATE TRIGGER app_workflow_control_blob_delete_guard
BEFORE DELETE ON app_workflow_control_blobs
WHEN EXISTS (
        SELECT 1 FROM app_workflow_control_heads h
         WHERE h.task_id = OLD.task_id
           AND h.execution_id = OLD.execution_id
           AND h.control_kind = OLD.control_kind
           AND h.generation = OLD.generation
           AND h.content_digest = OLD.content_digest
    )
    OR EXISTS (
        SELECT 1 FROM app_workflow_control_prepared p
         WHERE p.task_id = OLD.task_id
           AND p.execution_id = OLD.execution_id
           AND p.control_kind = OLD.control_kind
           AND p.generation = OLD.generation
           AND p.content_digest = OLD.content_digest
    )
BEGIN
    SELECT RAISE(ABORT, 'authoritative app workflow control blobs are retained');
END;
CREATE TRIGGER app_workflow_control_head_delete_guard
BEFORE DELETE ON app_workflow_control_heads
BEGIN
    SELECT RAISE(ABORT, 'app workflow control authority is retained');
END;
CREATE TRIGGER app_workflow_control_head_cas_guard
BEFORE UPDATE ON app_workflow_control_heads
WHEN NEW.revision != OLD.revision + 1
  OR NEW.generation < OLD.generation
  OR (OLD.lifecycle_state = 'consumed' AND NEW.generation = OLD.generation)
BEGIN
    SELECT RAISE(ABORT, 'app workflow control head requires monotonic CAS');
END;
"#;

// V34 adds the owner retry audit event. Preserve event ids and the AUTOINCREMENT
// high-water mark, including ids whose old events have already been retained out.
const APP_REGISTRY_SCHEMA_V34: &str = r#"
ALTER TABLE app_behavior_events RENAME TO app_behavior_events_v33;
CREATE TABLE app_behavior_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    installation_id TEXT NOT NULL CHECK(length(installation_id) BETWEEN 1 AND 128),
    behavior_id TEXT CHECK(behavior_id IS NULL OR length(behavior_id) BETWEEN 1 AND 192),
    kind TEXT NOT NULL CHECK(kind IN ('retired', 'scan_fault', 'retry_requested')),
    reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 1024),
    observed_at TEXT NOT NULL
) STRICT;
INSERT INTO app_behavior_events(event_id, installation_id, behavior_id, kind, reason, observed_at)
SELECT event_id, installation_id, behavior_id, kind, reason, observed_at
  FROM app_behavior_events_v33;
UPDATE sqlite_sequence
   SET seq = MAX(seq, COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'app_behavior_events_v33'), 0))
 WHERE name = 'app_behavior_events';
DROP TABLE app_behavior_events_v33;
CREATE INDEX app_behavior_events_recent_idx ON app_behavior_events(event_id DESC);
"#;

/// Objects whose presence makes a claimed V26 registry structurally readable.
/// `user_version` alone is not authority: a copied, hand-edited, or partially
/// provisioned database must fail at open rather than surface missing-table
/// errors after request admission.
const APP_REGISTRY_SCHEMA_V26_REQUIRED_OBJECTS: &[(&str, &str)] = &[
    ("table", "app_event_ingress_receipts"),
    ("trigger", "app_event_ingress_receipt_immutable"),
    ("table", "app_event_behavior_fires"),
    ("index", "app_event_behavior_fires_due_idx"),
    ("index", "app_event_behavior_fires_lease_idx"),
    ("index", "app_event_behavior_fires_health_idx"),
    ("index", "app_event_behavior_fires_event_idx"),
    ("trigger", "app_event_behavior_fire_cas_guard"),
    ("trigger", "app_event_behavior_fire_identity_guard"),
    ("table", "app_event_behavior_periods"),
    ("table", "app_owner_notification_outbox"),
    ("index", "app_owner_notification_outbox_due_idx"),
    ("index", "app_owner_notification_outbox_lease_idx"),
    ("index", "app_owner_notification_outbox_volume_idx"),
    ("index", "app_owner_notification_outbox_pending_idx"),
    ("trigger", "app_owner_notification_outbox_cas_guard"),
    ("trigger", "app_owner_notification_outbox_identity_guard"),
];

const APP_REGISTRY_SCHEMA_V27_REQUIRED_OBJECTS: &[(&str, &str)] =
    &[("trigger", "app_owner_notification_outbox_period_immutable")];

const APP_REGISTRY_SCHEMA_V28_REQUIRED_OBJECTS: &[(&str, &str)] = &[
    ("table", "app_event_ingress_tombstones"),
    ("trigger", "app_event_ingress_tombstone_immutable"),
    ("index", "app_event_ingress_receipts_retention_idx"),
    ("table", "app_owner_notification_tombstones"),
    ("trigger", "app_owner_notification_tombstone_immutable"),
    ("index", "app_owner_notification_tombstones_effect_idx"),
    ("index", "app_owner_notification_outbox_retention_idx"),
    ("index", "app_event_behavior_fires_expired_priority_idx"),
];

const APP_REGISTRY_SCHEMA_V29_REQUIRED_OBJECTS: &[(&str, &str)] = &[
    ("table", "app_terminal_compaction_quarantine"),
    ("trigger", "app_terminal_compaction_quarantine_immutable"),
    (
        "index",
        "app_terminal_compaction_quarantine_installation_idx",
    ),
    ("index", "app_owner_notification_outbox_expiry_idx"),
];

const APP_REGISTRY_SCHEMA_V30_REQUIRED_OBJECTS: &[(&str, &str)] =
    &[("table", "app_terminal_compaction_cursors")];

const APP_REGISTRY_SCHEMA_V31_REQUIRED_OBJECTS: &[(&str, &str)] = &[
    ("table", "app_owner_notification_attempt_refunds"),
    (
        "index",
        "app_owner_notification_attempt_refunds_pending_idx",
    ),
    ("trigger", "app_owner_notification_attempt_refunds_guard"),
];

const APP_REGISTRY_SCHEMA_V32_REQUIRED_OBJECTS: &[(&str, &str)] = &[(
    "index",
    "app_owner_notification_attempt_refunds_correlation_idx",
)];

/// A fully correlated initial-install publication.
///
/// Fields are private and the type has no `Deserialize` implementation. The
/// only constructor consumes already validated package/conformance records and
/// rechecks the load-bearing correlations before the store can see them.
#[derive(Debug)]
pub struct AppReadyForReviewPublication {
    package_revision_ref: AppReference,
    package_revision: AppPackageRevision,
    dependency_lock_json: Vec<u8>,
    staged_candidate: super::manifest::AppPackageCandidate,
    attempt: AppLifecycleAttempt,
    installation: AppInstallation,
}

impl AppReadyForReviewPublication {
    pub fn from_verified_conformance(
        package_revision: AppPackageRevision,
        staged_package: StagedAppPackage,
        dependency_lock: &AppPackageLock,
        attempt: AppLifecycleAttempt,
        installation: AppInstallation,
    ) -> Result<Self, AppRegistryError> {
        let limits = AppContractLimits::default();
        attempt.validate_app_contract(&limits)?;
        installation.validate_app_contract(&limits)?;
        let (package_revision_ref, package, dependency_lock_json) =
            correlate_staged_package(&package_revision, staged_package, dependency_lock)?;
        if attempt.kind != AppLifecycleAttemptKind::InitialInstall
            || attempt.state != AppLifecycleAttemptState::ReadyForReview
            || attempt.installation_id.is_some()
            || attempt.source_installation_generation.is_some()
            || attempt.approval_ref.is_some()
            || attempt.candidate_package_revision_ref != package_revision_ref
        {
            return Err(AppRegistryError::InvalidPublication(
                "initial attempt is not an unapproved ready-for-review publication",
            ));
        }
        if attempt.conformance_attestation_ref.as_ref()
            != Some(&package_revision.conformance_attestation_ref)
        {
            return Err(AppRegistryError::InvalidPublication(
                "attempt and package revision do not share one conformance attestation",
            ));
        }
        if installation.package_revision_ref != package_revision_ref
            || installation.lifecycle.status != AppInstallationStatus::ReadyForReview
            || installation.lifecycle.generation != 1
        {
            return Err(AppRegistryError::InvalidPublication(
                "installation is not generation-one ready-for-review state for the package",
            ));
        }

        Ok(Self {
            package_revision_ref,
            package_revision,
            dependency_lock_json,
            staged_candidate: package,
            attempt,
            installation,
        })
    }

    pub fn package_revision_ref(&self) -> &AppReference {
        &self.package_revision_ref
    }

    pub fn package_revision(&self) -> &AppPackageRevision {
        &self.package_revision
    }

    pub fn attempt_id(&self) -> &AppReference {
        &self.attempt.attempt_id
    }

    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation.installation_id
    }
}

fn correlate_staged_package(
    package_revision: &AppPackageRevision,
    staged_package: StagedAppPackage,
    dependency_lock: &AppPackageLock,
) -> Result<(AppReference, super::manifest::AppPackageCandidate, Vec<u8>), AppRegistryError> {
    let limits = AppContractLimits::default();
    package_revision.validate_app_contract(&limits)?;
    let package = staged_package.into_candidate();
    let manifest = package.manifest().manifest();
    if package_revision.content_digest != *package.bundle_digest() {
        return Err(AppRegistryError::InvalidPublication(
            "package revision content digest does not match the admitted bundle",
        ));
    }
    if package_revision.semantic_version != manifest.version {
        return Err(AppRegistryError::InvalidPublication(
            "package revision version does not match the admitted manifest",
        ));
    }
    if package_revision.manifest_schema_version != manifest.metadata.magician.app_manifest_version {
        return Err(AppRegistryError::InvalidPublication(
            "package revision manifest schema does not match the admitted manifest",
        ));
    }
    if package_revision.authoring_sdk_version != manifest.metadata.magician.app_sdk_version {
        return Err(AppRegistryError::InvalidPublication(
            "package revision SDK version does not match the admitted manifest",
        ));
    }
    if package_revision.compatibility.len() != manifest.app.compatibility.len()
        || package_revision.compatibility.iter().any(|declared| {
            manifest.app.compatibility.get(&declared.contract) != Some(&declared.requirement)
        })
    {
        return Err(AppRegistryError::InvalidPublication(
            "package revision compatibility does not match the admitted manifest",
        ));
    }
    if dependency_lock.manifest_digest() != package.manifest().manifest_digest()
        || dependency_lock.bundle_digest() != package.bundle_digest()
    {
        return Err(AppRegistryError::InvalidPublication(
            "dependency lock does not bind the admitted package",
        ));
    }
    if package_revision.dependency_lock_digest != *dependency_lock.lock_digest() {
        return Err(AppRegistryError::InvalidPublication(
            "package revision dependency digest does not match the immutable lock",
        ));
    }
    let entity_schema_digest = canonical_entity_schema_digest(manifest).map_err(|_| {
        AppRegistryError::InvalidPublication(
            "admitted package entity schema could not be canonically digested",
        )
    })?;
    if package_revision.entity_schema_digest != entity_schema_digest {
        return Err(AppRegistryError::InvalidPublication(
            "package revision entity schema digest does not match the admitted manifest",
        ));
    }
    let view_schema_digest = canonical_view_schema_digest(manifest).map_err(|_| {
        AppRegistryError::InvalidPublication(
            "admitted package view schema could not be canonically digested",
        )
    })?;
    if package_revision.view_schema_digest != view_schema_digest {
        return Err(AppRegistryError::InvalidPublication(
            "package revision view schema digest does not match the admitted manifest",
        ));
    }
    let package_revision_ref = canonical_package_revision_ref(package_revision)?;
    let dependency_lock_json = encode_bounded_json(dependency_lock, &limits)?;
    Ok((package_revision_ref, package, dependency_lock_json))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppRegistryPublicationOutcome {
    Created,
    AlreadyPresent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRegistryPublicationReceipt {
    pub package_revision_ref: AppReference,
    pub attempt_id: AppReference,
    pub installation_id: AppInstallationId,
    pub outcome: AppRegistryPublicationOutcome,
}

/// Receipt for the write-once registry publication of one exact standalone
/// procedure revision. It contains identity metadata only, never procedure
/// instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSkillRevisionPublicationReceipt {
    pub dependency_ref: AppReference,
    pub semantic_version: String,
    pub immutable_revision_ref: AppReference,
    pub revision: AppRevision,
    pub content_digest: AppDigest,
    pub outcome: AppRegistryPublicationOutcome,
}

/// One-row lifecycle identity revalidation for final dispatch boundaries. It
/// confirms current registry state only; it grants no policy/provider rights.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRegistryAuthorityRevalidationReceipt {
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_revision_ref: AppReference,
    pub grant_revision: super::models::AppRevision,
    pub grant_authority_digest: AppDigest,
    pub schema_revision: super::models::AppRevision,
    pub authority_digest: AppDigest,
}

/// Server-owned monotonic namespace for protected workflow control records.
/// These values are never accepted from package/model/API payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppWorkflowControlKind {
    TaskBinding,
    RunState,
    ActionCancellation,
    Pause,
    InteractiveStop,
}

impl AppWorkflowControlKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::TaskBinding => "task_binding",
            Self::RunState => "run_state",
            Self::ActionCancellation => "action_cancellation",
            Self::Pause => "pause",
            Self::InteractiveStop => "interactive_stop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppWorkflowControlLifecycle {
    Prepared,
    Active,
    Claimed,
    Consumed,
}

impl AppWorkflowControlLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Active => "active",
            Self::Claimed => "claimed",
            Self::Consumed => "consumed",
        }
    }

    fn parse(value: &str) -> Result<Self, AppRegistryError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "active" => Ok(Self::Active),
            "claimed" => Ok(Self::Claimed),
            "consumed" => Ok(Self::Consumed),
            _ => Err(AppRegistryError::StateConflict(
                "invalid protected workflow control lifecycle".to_owned(),
            )),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AppWorkflowControlRecord {
    generation: u64,
    revision: u64,
    lifecycle: AppWorkflowControlLifecycle,
    claim_ref: Option<AppReference>,
    claim_expires_at: Option<DateTime<Utc>>,
    proposal_ref: Option<AppReference>,
    content_digest: AppDigest,
    sealed_blob: Vec<u8>,
}

impl std::fmt::Debug for AppWorkflowControlRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppWorkflowControlRecord")
            .field("generation", &self.generation)
            .field("revision", &self.revision)
            .field("lifecycle", &self.lifecycle)
            .field("claim_ref", &self.claim_ref)
            .field("claim_expires_at", &self.claim_expires_at)
            .field("proposal_ref", &self.proposal_ref)
            .field("content_digest", &self.content_digest)
            .field("byte_count", &self.sealed_blob.len())
            .finish()
    }
}

impl AppWorkflowControlRecord {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn lifecycle(&self) -> AppWorkflowControlLifecycle {
        self.lifecycle
    }

    pub fn claim_ref(&self) -> Option<&AppReference> {
        self.claim_ref.as_ref()
    }

    pub fn claim_expires_at(&self) -> Option<DateTime<Utc>> {
        self.claim_expires_at
    }

    pub fn proposal_ref(&self) -> Option<&AppReference> {
        self.proposal_ref.as_ref()
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn sealed_blob(&self) -> &[u8] {
        &self.sealed_blob
    }
}

/// A conformance-complete update/reinstall package and attempt. Unlike an
/// initial publication it does not create a second installation row; the
/// transaction binds the attempt's relational link to the exact existing
/// installation generation.
#[derive(Debug)]
pub struct AppReviewableRevisionPublication {
    package_revision_ref: AppReference,
    package_revision: AppPackageRevision,
    dependency_lock_json: Vec<u8>,
    staged_candidate: super::manifest::AppPackageCandidate,
    attempt: AppLifecycleAttempt,
    source_fence: AppReviewableRevisionSourceFence,
}

/// Exact durable source identity captured before an update/reinstall package
/// is admitted. The registry recomputes this inside the publication
/// transaction, so an API caller cannot attach a candidate to a stale package,
/// grant, schema, surface, dependency lock or data high-water.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewableRevisionSourceFence {
    pub installation_id: AppInstallationId,
    pub attempt_kind: AppLifecycleAttemptKind,
    pub parked_lifecycle_generation: u64,
    pub source_installation_generation: u64,
    pub source_package_revision_ref: AppReference,
    pub source_dependency_lock_digest: AppDigest,
    pub source_grant_revision: AppRevision,
    pub source_schema_revision: AppRevision,
    pub source_surface_revision: AppRevision,
    pub source_change_seq_high_water: u64,
    pub fence_digest: AppDigest,
}

impl AppReviewableRevisionSourceFence {
    pub fn permission_migration_diff_ref(&self) -> Result<AppReference, AppRegistryError> {
        AppReference::parse(format!(
            "diff:app-update:{}",
            self.fence_digest.as_str().trim_start_matches("blake3:")
        ))
        .map_err(Into::into)
    }

    fn seal(mut self) -> Result<Self, AppRegistryError> {
        self.fence_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value((
                &self.installation_id,
                self.attempt_kind,
                self.parked_lifecycle_generation,
                self.source_installation_generation,
                &self.source_package_revision_ref,
                &self.source_dependency_lock_digest,
                self.source_grant_revision,
                self.source_schema_revision,
                self.source_surface_revision,
                self.source_change_seq_high_water,
            ))
            .map_err(|error| AppRegistryError::StateConflict(error.to_string()))?,
        )?;
        Ok(self)
    }

    fn verify(&self) -> Result<(), AppRegistryError> {
        if self.clone().seal()?.fence_digest != self.fence_digest {
            return Err(AppRegistryError::StateConflict(
                "reviewable revision source fence digest changed".to_owned(),
            ));
        }
        Ok(())
    }
}

impl AppReviewableRevisionPublication {
    pub fn from_verified_conformance(
        package_revision: AppPackageRevision,
        staged_package: StagedAppPackage,
        dependency_lock: &AppPackageLock,
        attempt: AppLifecycleAttempt,
        source_fence: AppReviewableRevisionSourceFence,
    ) -> Result<Self, AppRegistryError> {
        attempt.validate_app_contract(&AppContractLimits::default())?;
        let source_diff_ref = source_fence.permission_migration_diff_ref()?;
        let (package_revision_ref, package, dependency_lock_json) =
            correlate_staged_package(&package_revision, staged_package, dependency_lock)?;
        if attempt.kind == AppLifecycleAttemptKind::InitialInstall
            || attempt.state != AppLifecycleAttemptState::ReadyForReview
            || attempt.installation_id.is_none()
            || attempt.source_installation_generation.is_none()
            || attempt.approval_ref.is_some()
            || attempt.candidate_package_revision_ref != package_revision_ref
            || attempt.conformance_attestation_ref.as_ref()
                != Some(&package_revision.conformance_attestation_ref)
            || attempt.installation_id.as_ref() != Some(&source_fence.installation_id)
            || attempt.kind != source_fence.attempt_kind
            || attempt.source_installation_generation
                != Some(source_fence.source_installation_generation)
            || attempt.permission_migration_diff_ref.as_ref() != Some(&source_diff_ref)
        {
            return Err(AppRegistryError::InvalidPublication(
                "update/reinstall attempt is not an unapproved exact-generation review publication",
            ));
        }
        source_fence.verify()?;
        Ok(Self {
            package_revision_ref,
            package_revision,
            dependency_lock_json,
            staged_candidate: package,
            attempt,
            source_fence,
        })
    }

    pub fn package_revision_ref(&self) -> &AppReference {
        &self.package_revision_ref
    }

    pub fn attempt_id(&self) -> &AppReference {
        &self.attempt.attempt_id
    }

    pub fn installation_id(&self) -> &AppInstallationId {
        self.attempt
            .installation_id
            .as_ref()
            .expect("validated reviewable update/reinstall attempt")
    }
}

#[derive(Debug, Error)]
pub enum AppRegistryError {
    #[error(transparent)]
    Authentication(#[from] AppAuthorityError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Approval(#[from] AppInstallationApprovalError),
    #[error(transparent)]
    Lifecycle(#[from] AppLifecycleError),
    #[error(transparent)]
    PackageLock(#[from] AppPackageLockError),
    #[error("invalid ready-for-review publication: {0}")]
    InvalidPublication(&'static str),
    #[error("app registry is at its blocking-operation capacity")]
    Overloaded,
    #[error("app registry blocking worker terminated: {0}")]
    WorkerTerminated(String),
    #[error("app registry path is unsafe: {0}")]
    UnsafePath(String),
    #[error("staged app package failed registry revalidation: {0}")]
    StagedPackageInvalid(String),
    #[error("app registry database belongs to another scope")]
    ScopeCollision,
    #[error("durable app-data encryption key is unavailable")]
    AtRestEncryptionKeyUnavailable,
    #[error("app registry authenticated-at-rest encryption verification failed")]
    AtRestEncryptionFailed,
    #[error("app registry scope-binding publication completed but durability is unknown")]
    ScopeBindingCommitStateUnknown,
    #[error(
        "app registry database has an incompatible owner or schema (application={application_id}, \
         schema={schema_version})"
    )]
    IncompatibleDatabase {
        application_id: i32,
        schema_version: i32,
    },
    #[error(
        "app registry v10 resource migration requires explicit recovery or retirement of \
         {tree_count} legacy resource tree(s)"
    )]
    LegacyResourceTreesRequireExplicitMigration { tree_count: u64 },
    #[error(
        "app registry v11 migration requires explicit recovery or retirement of {tree_count} \
         resource tree(s) missing immutable v10 baselines"
    )]
    IncompleteResourceBaselinesRequireExplicitMigration { tree_count: u64 },
    #[error("app registry identity conflict for {entity} `{identity}`")]
    IdentityConflict {
        entity: &'static str,
        identity: String,
    },
    #[error("app registry control-plane input is invalid: {0}")]
    InvalidControlPlane(String),
    #[error("app registry state conflict: {0}")]
    StateConflict(String),
    #[error("app registry record {entity} `{identity}` does not exist")]
    MissingRecord {
        entity: &'static str,
        identity: String,
    },
    #[error("app registry compare-and-swap lost for {0}")]
    CompareAndSwapLost(&'static str),
    #[error("installation generation conflict (expected={expected}, actual={actual})")]
    GenerationConflict { expected: u64, actual: u64 },
    #[error("app lifecycle outbox lease is stale, expired or no longer owned")]
    OutboxLeaseStale,
    #[error("failed to access app registry filesystem: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to access app registry SQLite store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("failed to encode app registry record: {0}")]
    Encoding(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct AppRegistryService {
    workspace: ArtifactV2Workspace,
    blocking_slots: Arc<Semaphore>,
    background_write_slots: Arc<Semaphore>,
    schema_ready_scopes: Arc<StdMutex<HashSet<(String, String)>>>,
    computed_capability_hide: Arc<StdMutex<Option<Arc<dyn Fn(&str, &str) + Send + Sync>>>>,
}

impl std::fmt::Debug for AppRegistryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppRegistryService")
            .field("workspace_root", &self.workspace.base_root())
            .field(
                "available_blocking_slots",
                &self.blocking_slots.available_permits(),
            )
            .finish_non_exhaustive()
    }
}

// Preserve the calling App/task span across Tokio's blocking-pool boundary.
// The captured span contains host correlation fields, never SQL or App rows.
fn spawn_registry_work<F, T>(operation: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || {
        let _entered = span.enter();
        operation()
    })
}

impl AppRegistryService {
    pub fn connection_pool_stats(&self) -> ConnectionPoolStats {
        registry_connections().stats()
    }

    pub fn workspace_layout(&self) -> &ArtifactV2Workspace {
        &self.workspace
    }

    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self::with_blocking_capacity(workspace, DEFAULT_BLOCKING_OPERATIONS)
            .expect("the default app-registry blocking capacity preserves a foreground lane")
    }

    pub fn with_blocking_capacity(
        workspace: ArtifactV2Workspace,
        blocking_capacity: usize,
    ) -> Result<Self, AppRegistryError> {
        if blocking_capacity < 2 {
            return Err(AppRegistryError::InvalidPublication(
                "blocking capacity must be at least two so background work cannot consume the \
                 foreground reserve",
            ));
        }
        Ok(Self {
            workspace,
            blocking_slots: Arc::new(Semaphore::new(blocking_capacity)),
            background_write_slots: Arc::new(Semaphore::new(blocking_capacity.saturating_sub(1))),
            schema_ready_scopes: Arc::new(StdMutex::new(HashSet::new())),
            computed_capability_hide: Arc::new(StdMutex::new(None)),
        })
    }

    pub fn set_computed_capability_hide(&self, hide: Arc<dyn Fn(&str, &str) + Send + Sync>) {
        *self
            .computed_capability_hide
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(hide);
    }

    /// Append a durable app-data root generation and immediately rekey this
    /// authenticated scope. Other scopes retain recoverability through the
    /// keychain's historical generations and rekey on their next writable
    /// open. No generation is retired automatically.
    pub async fn rotate_at_rest_key_generation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<String, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let scope = authenticated_scope.scope().clone();
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let database_path =
                workspace.app_store_db_path(scope.principal.as_str(), scope.workspace.as_str());
            registry_connections().exclusive_key_rotation(&database_path, || {
                let new_key_id =
                    rotate_app_data_root_key(scope.principal.as_str(), scope.workspace.as_str())
                        .map_err(|_| AppRegistryError::AtRestEncryptionKeyUnavailable)?;
                ensure_registry_parent_tree(workspace.base_root(), &database_path)?;
                validate_existing_registry_paths(workspace.base_root(), &database_path)?;
                ensure_app_scope_binding(&workspace, &scope).map_err(map_package_staging_error)?;
                let connection = open_registry_connection_unpooled(&database_path, &scope, true)?;
                if load_app_data_encryption_key_id(&connection)? != new_key_id {
                    return Err(AppRegistryError::AtRestEncryptionFailed);
                }
                Ok(new_key_id)
            })
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Storage maintenance stays inside the authenticated App database owner.
    /// It cannot create a missing store, cross scopes or delete logical records.
    pub async fn maintain_store(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        action: AppStoreMaintenanceAction,
        now: DateTime<Utc>,
    ) -> Result<Option<AppStoreMaintenanceReport>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let authenticated = authenticated_scope.clone();
        let scope = authenticated.scope().clone();
        // Drain the scope before claiming a blocking worker. Queued ordinary
        // operations await this async gate without hoarding process slots.
        let maintenance_guard = self.scope_maintenance_lock(&scope).write_owned().await;
        let (permit, write_guard) = admission::write(
            Arc::clone(&self.blocking_slots),
            self.scope_write_lock(&scope),
        )
        .await
        .map_err(|_| AppRegistryError::Overloaded)?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _ownership = (permit, write_guard, maintenance_guard);
            authenticated.ensure_live_at(&Utc::now())?;
            let path = workspace.app_store_db_path(scope.principal.as_str(), scope.workspace.as_str());
            if let Err(error) = validate_existing_registry_paths(workspace.base_root(), &path) {
                if matches!(&error, AppRegistryError::Io(error) if error.kind() == std::io::ErrorKind::NotFound) {
                    return Ok(None);
                }
                return Err(error);
            }
            if !path.try_exists()? { return Ok(None); }
            registry_connections().exclusive_database_maintenance(&path, || {
                let started = std::time::Instant::now();
                validate_existing_registry_paths(workspace.base_root(), &path)?;
                let before = fs::metadata(&path)?.len();
                let connection = open_registry_connection_unpooled(
                    &path, &scope, action != AppStoreMaintenanceAction::Verify,
                )?;
                let outcome = maintenance::maintain(&connection, action)?;
                drop(connection);
                let sidecar_size = |suffix: &str| -> Result<u64, AppRegistryError> {
                    let mut sidecar = path.as_os_str().to_owned();
                    sidecar.push(suffix);
                    match fs::symlink_metadata(Path::new(&sidecar)) {
                        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
                        Ok(_) => Err(AppRegistryError::UnsafePath("App database sidecar is not a regular file".to_owned())),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
                        Err(error) => Err(error.into()),
                    }
                };
                Ok(Some(AppStoreMaintenanceReport {
                    principal: scope.principal.to_string(), workspace: scope.workspace.to_string(),
                    relative_path: "apps/app_store.sqlite3", operation: action,
                    encrypted: true, integrity_ok: true, completed_at: Utc::now().to_rfc3339(),
                    duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    database_bytes_before: before, database_bytes_after: fs::metadata(&path)?.len(),
                    wal_bytes_after: sidecar_size("-wal")?, shm_bytes_after: sidecar_size("-shm")?,
                    page_count: outcome.page_count, free_pages: outcome.free_pages,
                    checkpoint_busy: outcome.checkpoint_busy,
                }))
            })
        }).await.map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    pub fn hide_computed_capability_scope(&self, authenticated: &AuthenticatedAppScope) {
        let Some(hide) = self
            .computed_capability_hide
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        else {
            return;
        };
        hide(
            authenticated.scope().principal.as_str(),
            authenticated.scope().workspace.as_str(),
        );
    }

    /// Publish one immutable protected workflow-control generation and move
    /// the canonical head forward in the same FULL-synchronous SQLite
    /// transaction. The filesystem sidecar is only a cache/mirror; callers
    /// must accept bytes solely when they equal this current record.
    pub async fn publish_workflow_control_generation(
        &self,
        scope: AppScope,
        task_id: String,
        execution_id: String,
        kind: AppWorkflowControlKind,
        sealed_blob: Vec<u8>,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(&task_id, &execution_id, &sealed_blob)?;
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            publish_workflow_control_generation_blocking(
                &mut connection,
                &scope,
                &task_id,
                &execution_id,
                kind,
                sealed_blob,
                &[],
                None,
                None,
                None,
                None,
                now,
            )
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Compare-and-publish the immutable cancellation head for one exact app
    /// execution. The caller owns the canonical Artifact task flock while it
    /// computes the pre-I/O proof; this registry CAS prevents a stale process
    /// from advancing a head after that proof was sampled.
    pub(crate) async fn compare_and_publish_action_cancellation_generation(
        &self,
        scope: AppScope,
        task_id: String,
        execution_id: String,
        expected_current_content_digest: Option<AppDigest>,
        sealed_blob: Vec<u8>,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(&task_id, &execution_id, &sealed_blob)?;
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            publish_workflow_control_generation_blocking(
                &mut connection,
                &scope,
                &task_id,
                &execution_id,
                AppWorkflowControlKind::ActionCancellation,
                sealed_blob,
                &[],
                None,
                None,
                Some(expected_current_content_digest),
                None,
                now,
            )
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Publish one payload-free interactive-stop generation only while both
    /// protected heads still match the exact state inspected by the caller.
    /// The run-state dependency prevents a session settlement or replacement
    /// from racing a stale stop request into the durable control plane.
    pub(crate) async fn compare_and_publish_interactive_stop_generation(
        &self,
        scope: AppScope,
        task_id: String,
        execution_id: String,
        expected_run_state_content_digest: AppDigest,
        expected_stop_content_digest: Option<AppDigest>,
        sealed_blob: Vec<u8>,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(&task_id, &execution_id, &sealed_blob)?;
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            publish_workflow_control_generation_blocking(
                &mut connection,
                &scope,
                &task_id,
                &execution_id,
                AppWorkflowControlKind::InteractiveStop,
                sealed_blob,
                &[],
                None,
                None,
                Some(expected_stop_content_digest),
                Some(expected_run_state_content_digest),
                now,
            )
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Atomically publish one terminal run-state generation and every memory
    /// or personal-agent retrieval proposal derived from that exact sealed
    /// result. The run-state remains the sole workflow completion owner;
    /// destination rows are replayable projections which cannot become
    /// visible without the same transaction.
    pub(crate) async fn publish_terminal_run_state_with_contributions(
        &self,
        scope: AppScope,
        task_id: String,
        execution_id: String,
        sealed_blob: Vec<u8>,
        contributions: Vec<AppPreparedTerminalContribution>,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(&task_id, &execution_id, &sealed_blob)?;
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            publish_workflow_control_generation_blocking(
                &mut connection,
                &scope,
                &task_id,
                &execution_id,
                AppWorkflowControlKind::RunState,
                sealed_blob,
                &contributions,
                None,
                None,
                None,
                None,
                now,
            )
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Load and re-hash the exact immutable blob named by the canonical head.
    /// Opening through the write initializer is intentional: a development
    /// scope is upgraded once to the current schema before any
    /// security-bearing sidecar can be read; steady-state read-only opens
    /// accept only that current schema.
    pub async fn current_workflow_control(
        &self,
        scope: AppScope,
        task_id: String,
        execution_id: String,
        kind: AppWorkflowControlKind,
    ) -> Result<Option<AppWorkflowControlRecord>, AppRegistryError> {
        validate_workflow_control_identity(&task_id, &execution_id, b"x")?;
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            let execution_id =
                recurring::current_binding_control_id(&transaction, &task_id, &execution_id, kind)?;
            let record = load_workflow_control_record(&transaction, &task_id, &execution_id, kind)?;
            transaction.commit()?;
            Ok(record)
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn publish_workflow_control_generation_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        kind: AppWorkflowControlKind,
        sealed_blob: Vec<u8>,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, &sealed_blob)?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        publish_workflow_control_generation_blocking(
            &mut connection,
            &scope,
            task_id,
            execution_id,
            kind,
            sealed_blob,
            &[],
            None,
            None,
            None,
            None,
            now,
        )
    }

    pub fn prepare_workflow_pause_generation_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        sealed_blob: Vec<u8>,
        expected_claim_ref: Option<&AppReference>,
        proposal_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, &sealed_blob)?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        publish_workflow_control_generation_blocking(
            &mut connection,
            &scope,
            task_id,
            execution_id,
            AppWorkflowControlKind::Pause,
            sealed_blob,
            &[],
            expected_claim_ref,
            Some(proposal_ref),
            None,
            None,
            now,
        )
    }

    pub fn current_workflow_control_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        kind: AppWorkflowControlKind,
    ) -> Result<Option<AppWorkflowControlRecord>, AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, b"x")?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let control_id =
            recurring::current_binding_control_id(&transaction, task_id, execution_id, kind)?;
        let record = load_workflow_control_record(&transaction, task_id, &control_id, kind)?;
        transaction.commit()?;
        Ok(record)
    }

    pub fn prepared_workflow_pause_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
    ) -> Result<Option<AppWorkflowControlRecord>, AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, b"x")?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let record = load_prepared_workflow_pause_record(&transaction, task_id, execution_id)?;
        transaction.commit()?;
        Ok(record)
    }

    pub async fn renew_workflow_pause_claim(
        &self,
        scope: AppScope,
        task_id: String,
        execution_id: String,
        expected_generation: u64,
        expected_digest: AppDigest,
        claim_ref: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        self.transition_workflow_pause(
            scope,
            task_id,
            execution_id,
            expected_generation,
            expected_digest,
            AppWorkflowControlLifecycle::Claimed,
            Some(claim_ref.clone()),
            AppWorkflowControlLifecycle::Claimed,
            Some(claim_ref),
            now,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub fn transition_workflow_pause_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        expected_generation: u64,
        expected_digest: &AppDigest,
        expected_lifecycle: AppWorkflowControlLifecycle,
        expected_claim_ref: Option<&AppReference>,
        next_lifecycle: AppWorkflowControlLifecycle,
        next_claim_ref: Option<&AppReference>,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, b"x")?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        transition_workflow_pause_blocking(
            &mut connection,
            task_id,
            execution_id,
            expected_generation,
            expected_digest,
            expected_lifecycle,
            expected_claim_ref,
            next_lifecycle,
            next_claim_ref,
            now,
        )
    }

    /// Publish the commit point for a pause only after the exact envelope that
    /// names this immutable generation has been renamed and directory-synced.
    pub fn activate_prepared_workflow_pause_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        expected_generation: u64,
        expected_digest: &AppDigest,
        expected_proposal_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, b"x")?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        activate_prepared_workflow_pause_blocking(
            &mut connection,
            task_id,
            execution_id,
            expected_generation,
            expected_digest,
            expected_proposal_ref,
            now,
        )
    }

    /// Retire an exact proposal owned by this writer after a known failure
    /// before the replacement envelope was renamed. The prior Active/Claimed
    /// head is left untouched and becomes usable again.
    pub fn abort_prepared_workflow_pause_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        expected_generation: u64,
        expected_digest: &AppDigest,
        expected_proposal_ref: &AppReference,
    ) -> Result<(), AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, b"x")?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        retire_prepared_workflow_pause_blocking(
            &mut connection,
            task_id,
            execution_id,
            expected_generation,
            expected_digest,
            expected_proposal_ref,
            None,
        )
    }

    /// Cold-load repair for a writer that died before publishing its envelope.
    /// A fresh proposal is never stolen: retirement is possible only after its
    /// bounded prepare lease elapsed and the exact base head is still current.
    pub fn retire_expired_prepared_workflow_pause_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        expected_generation: u64,
        expected_digest: &AppDigest,
        expected_proposal_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<(), AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, b"x")?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        retire_prepared_workflow_pause_blocking(
            &mut connection,
            task_id,
            execution_id,
            expected_generation,
            expected_digest,
            expected_proposal_ref,
            Some(now),
        )
    }

    /// Release an orphaned claim only after its bounded lease elapsed. The
    /// caller must first verify the exact current envelope/body bytes and
    /// freshly re-authorize the app continuation; this method supplies the
    /// monotonic CAS and never steals a live claim.
    pub fn release_expired_workflow_pause_claim_sync(
        &self,
        scope: AppScope,
        task_id: &str,
        execution_id: &str,
        expected_generation: u64,
        expected_digest: &AppDigest,
        expected_claim_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(task_id, execution_id, b"x")?;
        let mut connection = open_scoped_registry_for_write(&self.workspace, &scope)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if load_prepared_workflow_pause_record(&transaction, task_id, execution_id)?.is_some() {
            return Err(AppRegistryError::CompareAndSwapLost(
                "protected workflow pause has an unresolved prepared proposal",
            ));
        }
        let current = load_workflow_control_record(
            &transaction,
            task_id,
            execution_id,
            AppWorkflowControlKind::Pause,
        )?
        .ok_or(AppRegistryError::MissingRecord {
            entity: "protected workflow pause",
            identity: format!("{task_id}:{execution_id}"),
        })?;
        if current.generation != expected_generation
            || current.content_digest != *expected_digest
            || current.lifecycle != AppWorkflowControlLifecycle::Claimed
            || current.claim_ref.as_ref() != Some(expected_claim_ref)
            || current
                .claim_expires_at
                .is_none_or(|expires_at| now < expires_at)
        {
            return Err(AppRegistryError::CompareAndSwapLost(
                "protected workflow pause expired claim",
            ));
        }
        let observed_expiry = current.claim_expires_at.ok_or_else(|| {
            AppRegistryError::StateConflict("protected pause claim expiry is missing".to_owned())
        })?;
        let next_revision = current.revision.checked_add(1).ok_or_else(|| {
            AppRegistryError::StateConflict("protected pause revision overflow".to_owned())
        })?;
        let affected = transaction.execute(
            "UPDATE app_workflow_control_heads
                SET lifecycle_state = 'active', claim_ref = NULL,
                    claim_expires_at = NULL, revision = ?1, updated_at = ?2
              WHERE task_id = ?3 AND execution_id = ?4 AND control_kind = 'pause'
                AND generation = ?5 AND content_digest = ?6 AND revision = ?7
                AND lifecycle_state = 'claimed' AND claim_ref = ?8
                AND claim_expires_at = ?9 AND claim_expires_at <= ?2",
            params![
                i64::try_from(next_revision).unwrap_or(i64::MAX),
                now.to_rfc3339_opts(SecondsFormat::Micros, true),
                task_id,
                execution_id,
                i64::try_from(expected_generation).unwrap_or(i64::MAX),
                expected_digest.as_str(),
                i64::try_from(current.revision).unwrap_or(i64::MAX),
                expected_claim_ref.to_string(),
                observed_expiry.to_rfc3339_opts(SecondsFormat::Micros, true),
            ],
        )?;
        if affected != 1 {
            return Err(AppRegistryError::CompareAndSwapLost(
                "protected workflow pause expired claim",
            ));
        }
        let released = load_workflow_control_record(
            &transaction,
            task_id,
            execution_id,
            AppWorkflowControlKind::Pause,
        )?
        .ok_or_else(|| {
            AppRegistryError::StateConflict("released protected pause disappeared".to_owned())
        })?;
        transaction.commit()?;
        Ok(released)
    }

    #[allow(clippy::too_many_arguments)]
    async fn transition_workflow_pause(
        &self,
        scope: AppScope,
        task_id: String,
        execution_id: String,
        expected_generation: u64,
        expected_digest: AppDigest,
        expected_lifecycle: AppWorkflowControlLifecycle,
        expected_claim_ref: Option<AppReference>,
        next_lifecycle: AppWorkflowControlLifecycle,
        next_claim_ref: Option<AppReference>,
        now: DateTime<Utc>,
    ) -> Result<AppWorkflowControlRecord, AppRegistryError> {
        validate_workflow_control_identity(&task_id, &execution_id, b"x")?;
        if expected_generation == 0 {
            return Err(AppRegistryError::InvalidControlPlane(
                "protected pause generation must be positive".to_owned(),
            ));
        }
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            transition_workflow_pause_blocking(
                &mut connection,
                &task_id,
                &execution_id,
                expected_generation,
                &expected_digest,
                expected_lifecycle,
                expected_claim_ref.as_ref(),
                next_lifecycle,
                next_claim_ref.as_ref(),
                now,
            )
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Atomically publish an initial package attempt and its inert reviewable
    /// installation. Identical replay is accepted; an identity whose durable
    /// bytes differ fails and rolls the complete transaction back.
    pub async fn publish_ready_for_review(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        publication: AppReadyForReviewPublication,
        now: DateTime<Utc>,
    ) -> Result<AppRegistryPublicationReceipt, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if &publication.installation.scope != authenticated_scope.scope() {
            return Err(AppRegistryError::InvalidPublication(
                "installation scope does not match authenticated scope",
            ));
        }

        let scope = authenticated_scope.scope().clone();
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            publish_ready_for_review_blocking(&workspace, &scope, publication)
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Atomically publish an immutable candidate revision and a reviewable
    /// update/reinstall attempt against one existing installation generation.
    pub async fn publish_reviewable_revision(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        publication: AppReviewableRevisionPublication,
        now: DateTime<Utc>,
    ) -> Result<AppRegistryPublicationReceipt, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let scope = authenticated_scope.scope().clone();
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            publish_reviewable_revision_blocking(&workspace, &scope, publication)
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    pub async fn installation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<AppInstallation>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let scope = authenticated_scope.scope().clone();
        let installation_id = installation_id.clone();
        let installation: Option<AppInstallation> = self
            .read_record(
                scope.clone(),
                "SELECT record_json FROM app_installations WHERE installation_id = ?1",
                installation_id.to_string(),
            )
            .await?;
        if installation
            .as_ref()
            .is_some_and(|installation| installation.scope != scope)
        {
            return Err(AppRegistryError::ScopeCollision);
        }
        Ok(installation)
    }

    /// Reopen exact requested installations in one short read transaction.
    /// The batch bound limits one request, never the number of installed Apps.
    pub async fn installations_by_ids(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_ids: &[AppInstallationId],
        now: DateTime<Utc>,
    ) -> Result<Vec<AppInstallation>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if installation_ids.len() > 1_024 {
            return Err(AppRegistryError::StateConflict(
                "installation read batch exceeds its request bound".to_owned(),
            ));
        }
        if installation_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = installation_ids.to_vec();
        ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        ids.dedup();
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                let transaction = connection.unchecked_transaction()?;
                let mut installations = Vec::with_capacity(ids.len());
                {
                    let mut statement = transaction.prepare_cached(
                        "SELECT record_json FROM app_installations WHERE installation_id = ?1",
                    )?;
                    let limits = AppContractLimits::default();
                    for id in ids {
                        let row: Option<Vec<u8>> = statement
                            .query_row(params![id.as_str()], |row| row.get(0))
                            .optional()?;
                        if let Some(row) = row {
                            let installation: AppInstallation = decode_app_contract(&row, &limits)?;
                            if &installation.scope != scope || installation.installation_id != id {
                                return Err(AppRegistryError::ScopeCollision);
                            }
                            installations.push(installation);
                        }
                    }
                }
                transaction.commit()?;
                Ok(installations)
            })
            .await?;
        Ok(loaded.unwrap_or_default())
    }

    /// Return at most `limit + 1` enabled installations in stable identity
    /// order. The extra row lets bounded runtime consumers distinguish a
    /// complete inventory from a scope that exceeds their fixed capacity
    /// without opening the much heavier Apps-directory projection.
    pub async fn enabled_installations_bounded(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppInstallation>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if limit == 0 || limit > 1_024 {
            return Err(AppRegistryError::StateConflict(
                "enabled installation inventory limit is outside the supported range".to_owned(),
            ));
        }
        let fetch_limit = limit.checked_add(1).ok_or_else(|| {
            AppRegistryError::StateConflict(
                "enabled installation inventory limit overflowed".to_owned(),
            )
        })?;
        let fetch_limit = i64::try_from(fetch_limit).map_err(|_| {
            AppRegistryError::StateConflict(
                "enabled installation inventory limit is outside the SQLite range".to_owned(),
            )
        })?;
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                let mut statement = connection.prepare(
                    "SELECT record_json FROM app_installations
                      WHERE principal = ?1
                        AND workspace = ?2
                        AND lifecycle_status = 'enabled'
                      ORDER BY installation_id ASC
                      LIMIT ?3",
                )?;
                let rows = statement
                    .query_map(
                        params![
                            scope.principal.as_str(),
                            scope.workspace.as_str(),
                            fetch_limit,
                        ],
                        |row| row.get::<_, Vec<u8>>(0),
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                let limits = AppContractLimits::default();
                let mut installations = Vec::with_capacity(rows.len());
                for row in rows {
                    let installation: AppInstallation = decode_app_contract(&row, &limits)?;
                    if &installation.scope != scope
                        || installation.lifecycle.status != AppInstallationStatus::Enabled
                    {
                        return Err(AppRegistryError::ScopeCollision);
                    }
                    installations.push(installation);
                }
                Ok(installations)
            })
            .await?;
        Ok(loaded.unwrap_or_default())
    }

    /// Every installation in the scope, whatever its lifecycle status.
    ///
    /// `enabled_installations_bounded` deliberately answers only what is
    /// serving. Boot admission needs the wider view: it must not mint a second
    /// installation for a package this scope already has, and the one it would
    /// collide with may be `disabled` or parked `update_pending` rather than
    /// `enabled`.
    pub async fn installations_bounded(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppInstallation>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if limit == 0 || limit > 1_024 {
            return Err(AppRegistryError::StateConflict(
                "installation inventory limit is outside the supported range".to_owned(),
            ));
        }
        let fetch_limit = limit.checked_add(1).ok_or_else(|| {
            AppRegistryError::StateConflict("installation inventory limit overflowed".to_owned())
        })?;
        let fetch_limit = i64::try_from(fetch_limit).map_err(|_| {
            AppRegistryError::StateConflict(
                "installation inventory limit is outside the SQLite range".to_owned(),
            )
        })?;
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                let mut statement = connection.prepare(
                    "SELECT record_json FROM app_installations
                      WHERE principal = ?1
                        AND workspace = ?2
                      ORDER BY installation_id ASC
                      LIMIT ?3",
                )?;
                let rows = statement
                    .query_map(
                        params![
                            scope.principal.as_str(),
                            scope.workspace.as_str(),
                            fetch_limit,
                        ],
                        |row| row.get::<_, Vec<u8>>(0),
                    )?
                    .collect::<Result<Vec<_>, _>>()?;
                let limits = AppContractLimits::default();
                let mut installations = Vec::with_capacity(rows.len());
                for row in rows {
                    let installation: AppInstallation = decode_app_contract(&row, &limits)?;
                    // Status is intentionally unconstrained here, but scope is
                    // not: a row from another scope is a collision either way.
                    if &installation.scope != scope {
                        return Err(AppRegistryError::ScopeCollision);
                    }
                    installations.push(installation);
                }
                Ok(installations)
            })
            .await?;
        Ok(loaded.unwrap_or_default())
    }

    /// Reopen the latest durable owner approval for core launch/recovery
    /// revalidation. Publication and lifecycle mutation remain in the
    /// `magician-apps` owner; this is the registry's read-only truth seam.
    pub async fn installation_approval(
        &self,
        authenticated: &AuthenticatedAppScope,
        approval_id: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppInstallationApproval>, AppRegistryError> {
        let approval_id = approval_id.clone();
        let loaded = self
            .execute_scoped_read(authenticated, &now, move |connection, _| {
                let bytes: Option<Vec<u8>> = connection
                    .query_row(
                        "SELECT record_json FROM app_installation_approvals
                         WHERE approval_id = ?1
                         ORDER BY revision DESC
                         LIMIT 1",
                        params![approval_id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                bytes
                    .map(|record| decode_app_contract(&record, &AppContractLimits::default()))
                    .transpose()
                    .map_err(AppRegistryError::from)
            })
            .await?;
        Ok(loaded.flatten())
    }

    /// Capture the only source identity an update/reinstall candidate may
    /// target. Update publication is possible only after `BeginUpdate` parked
    /// the installation; reinstall publication is possible only while the
    /// retained installation is inert.
    pub async fn reviewable_revision_source_fence(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        attempt_kind: AppLifecycleAttemptKind,
        now: DateTime<Utc>,
    ) -> Result<AppReviewableRevisionSourceFence, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        if attempt_kind == AppLifecycleAttemptKind::InitialInstall {
            return Err(AppRegistryError::InvalidPublication(
                "initial install has no existing-installation source fence",
            ));
        }
        let installation_id = installation_id.clone();
        self.execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
            reviewable_revision_source_fence_blocking(
                connection,
                scope,
                &installation_id,
                attempt_kind,
            )
        })
        .await?
        .ok_or_else(|| AppRegistryError::MissingRecord {
            entity: "scope registry",
            identity: authenticated_scope.scope_binding_ref().to_string(),
        })
    }

    /// Revalidate the lifecycle identity carried by an admitted authority at
    /// the last registry boundary before dispatch. This is a bounded one-row
    /// read under authenticated scope; stale, revoked, disabled or missing
    /// state fails closed and no projection is consulted.
    pub async fn revalidate_current_authority(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        admitted: &ResolvedAppAuthority,
        now: DateTime<Utc>,
    ) -> Result<AppRegistryAuthorityRevalidationReceipt, AppRegistryError> {
        let canonical_authority_digest = admitted
            .canonical_authority_digest()
            .map_err(|error| AppRegistryError::StateConflict(error.to_string()))?;
        if admitted.resolved_at > now
            || admitted.resolved_at < *authenticated_scope.issued_at()
            || admitted.resolved_at >= *authenticated_scope.expires_at()
            || &admitted.scope_binding_ref != authenticated_scope.scope_binding_ref()
            || &admitted.actor_ref != authenticated_scope.actor_ref()
            || &admitted.session_ref != authenticated_scope.session_ref()
            || admitted.authentication != authenticated_scope.authentication()
            || admitted.authentication_revision != authenticated_scope.authentication_revision()
            || admitted.authority_digest != canonical_authority_digest
        {
            return Err(AppRegistryError::StateConflict(
                "admitted authority is stale, future-dated, mutated or belongs to another \
                 authenticated session"
                    .to_owned(),
            ));
        }
        let admitted = admitted.clone();
        self.execute_scoped_typed_read(authenticated_scope, &now, move |connection, scope| {
            let grant_revision = i64::try_from(admitted.grant_revision.get()).map_err(|_| {
                AppRegistryError::StateConflict(
                    "grant revision is outside the SQLite range".to_owned(),
                )
            })?;
            let schema_revision = i64::try_from(admitted.schema_revision.get()).map_err(|_| {
                AppRegistryError::StateConflict(
                    "schema revision is outside the SQLite range".to_owned(),
                )
            })?;
            let row = connection
                .query_row(
                    "SELECT i.lifecycle_status, i.lifecycle_generation,
                            i.package_revision_ref, i.record_json,
                            g.authority_digest, g.revoked_at,
                            g.package_revision_ref, g.record_json,
                            s.package_revision_ref, s.record_json
                       FROM app_installations i
                       LEFT JOIN app_grant_revisions g
                         ON g.installation_id = i.installation_id AND g.revision = ?2
                       LEFT JOIN app_schema_revisions s
                         ON s.installation_id = i.installation_id AND s.revision = ?3
                      WHERE i.installation_id = ?1
                      LIMIT 1",
                    params![
                        admitted.installation_id.as_str(),
                        grant_revision,
                        schema_revision,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Vec<u8>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, Option<Vec<u8>>>(7)?,
                            row.get::<_, Option<String>>(8)?,
                            row.get::<_, Option<Vec<u8>>>(9)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| AppRegistryError::MissingRecord {
                    entity: "installation",
                    identity: admitted.installation_id.to_string(),
                })?;
            let installation: AppInstallation =
                decode_app_contract(&row.3, &AppContractLimits::default())?;
            let grant: Option<AppGrantRevision> = row
                .7
                .as_deref()
                .map(|record| decode_app_contract(record, &AppContractLimits::default()))
                .transpose()?;
            let schema: Option<AppSchemaRevision> = row
                .9
                .as_deref()
                .map(|record| decode_app_contract(record, &AppContractLimits::default()))
                .transpose()?;
            let generation = u64::try_from(row.1).map_err(|_| {
                AppRegistryError::StateConflict(
                    "installation generation is outside the canonical range".to_owned(),
                )
            })?;
            if installation.scope != *scope
                || row.0 != "enabled"
                || generation != admitted.installation_generation
                || row.2 != admitted.package_revision_ref.as_str()
                || row.4.as_deref() != Some(admitted.grant_authority_digest.as_str())
                || row.5.is_some()
                || row.6.as_deref() != Some(admitted.package_revision_ref.as_str())
                || row.8.as_deref() != Some(admitted.package_revision_ref.as_str())
                || installation.lifecycle.status != AppInstallationStatus::Enabled
                || installation.lifecycle.generation != admitted.installation_generation
                || installation.package_revision_ref != admitted.package_revision_ref
                || installation.grant_revision != Some(admitted.grant_revision)
                || installation.active_schema_revision != Some(admitted.schema_revision)
                || grant.as_ref().is_none_or(|grant| {
                    grant.installation_id != admitted.installation_id
                        || grant.revision != admitted.grant_revision
                        || grant.package_revision_ref != admitted.package_revision_ref
                        || grant.authority_digest != admitted.grant_authority_digest
                        || grant.revoked_at.is_some()
                })
                || schema.as_ref().is_none_or(|schema| {
                    schema.installation_id != admitted.installation_id
                        || schema.revision != admitted.schema_revision
                        || schema.package_revision_ref != admitted.package_revision_ref
                })
            {
                return Err(AppRegistryError::StateConflict(
                    "admitted app authority is no longer current".to_owned(),
                ));
            }
            Ok(AppRegistryAuthorityRevalidationReceipt {
                installation_id: admitted.installation_id,
                installation_generation: generation,
                package_revision_ref: admitted.package_revision_ref,
                grant_revision: admitted.grant_revision,
                grant_authority_digest: admitted.grant_authority_digest,
                schema_revision: admitted.schema_revision,
                authority_digest: admitted.authority_digest,
            })
        })
        .await?
        .ok_or_else(|| AppRegistryError::MissingRecord {
            entity: "scope registry",
            identity: authenticated_scope.scope_binding_ref().to_string(),
        })
    }

    pub async fn ready_for_review_attempt_for_installation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<AppLifecycleAttempt>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let installation_id = installation_id.clone();
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                let installation_bytes: Option<Vec<u8>> = connection
                    .query_row(
                        "SELECT record_json FROM app_installations WHERE installation_id = ?1",
                        params![installation_id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(installation_bytes) = installation_bytes else {
                    return Ok(None);
                };
                let installation: AppInstallation = decode_app_contract(
                    &installation_bytes,
                    &AppContractLimits::default(),
                )?;
                if installation.scope != *scope || installation.installation_id != installation_id {
                    return Err(AppRegistryError::ScopeCollision);
                }
                if installation.lifecycle.status == AppInstallationStatus::ReadyForReview
                    && installation.lifecycle.generation == 1
                {
                    // Initial attempts have no source fence. Their registry
                    // linkage and exact candidate revision identify the review.
                    let bytes: Option<Vec<u8>> = connection
                        .query_row(
                            "SELECT record_json FROM app_lifecycle_attempts
                              WHERE installation_id = ?1 AND state = 'ready_for_review'
                                AND kind = 'initial_install'
                                AND candidate_package_revision_ref = ?2
                              ORDER BY created_at DESC LIMIT 1",
                            params![installation_id.as_str(), installation.package_revision_ref.as_str()],
                            |row| row.get(0),
                        )
                        .optional()?;
                    return bytes
                        .map(|record| decode_app_contract(&record, &AppContractLimits::default()))
                        .transpose()
                        .map_err(AppRegistryError::from);
                }
                let (kind, kind_label) = match installation.lifecycle.status {
                    AppInstallationStatus::UpdatePending => (AppLifecycleAttemptKind::Update, "update"),
                    AppInstallationStatus::UninstalledRetained => (AppLifecycleAttemptKind::Reinstall, "reinstall"),
                    _ => return Ok(None),
                };
                let source_fence = reviewable_revision_source_fence_blocking(
                    connection, scope, &installation_id, kind,
                )?;
                let source_diff_ref = source_fence.permission_migration_diff_ref()?;
                // Cancelled attempts remain as evidence. They must not hide a
                // new review or stop boot from publishing one for this source.
                let bytes: Option<Vec<u8>> = connection
                    .query_row(
                        "SELECT record_json FROM app_lifecycle_attempts
                          WHERE installation_id = ?1 AND state = 'ready_for_review'
                            AND kind = ?2
                            AND json_extract(CAST(record_json AS TEXT), '$.permission_migration_diff_ref') = ?3
                          ORDER BY created_at DESC
                          LIMIT 1",
                        params![installation_id.as_str(), kind_label, source_diff_ref.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                bytes
                    .map(|record| decode_app_contract(&record, &AppContractLimits::default()))
                    .transpose()
                    .map_err(AppRegistryError::from)
            })
            .await?;
        Ok(loaded.flatten())
    }

    pub async fn committed_attempt_for_installation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<AppLifecycleAttempt>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let installation_id = installation_id.clone();
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, _| {
                let bytes: Option<Vec<u8>> = connection
                    .query_row(
                        "SELECT record_json FROM app_lifecycle_attempts
                          WHERE installation_id = ?1 AND state = 'committed'
                          ORDER BY created_at DESC
                          LIMIT 1",
                        params![installation_id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?;
                bytes
                    .map(|record| decode_app_contract(&record, &AppContractLimits::default()))
                    .transpose()
                    .map_err(AppRegistryError::from)
            })
            .await?;
        Ok(loaded.flatten())
    }

    pub async fn lifecycle_attempt(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        attempt_id: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppLifecycleAttempt>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let scope = authenticated_scope.scope().clone();
        self.read_record(
            scope,
            "SELECT record_json FROM app_lifecycle_attempts WHERE attempt_id = ?1",
            attempt_id.to_string(),
        )
        .await
    }

    pub async fn package_revision(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        package_revision_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppPackageRevision>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let scope = authenticated_scope.scope().clone();
        self.read_record(
            scope,
            "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
            package_revision_ref.to_string(),
        )
        .await
    }

    /// Read many package revisions under a single blocking slot.
    ///
    /// Town Square asks for the revision behind every enabled installation.
    /// Doing that one call at a time took one of the four blocking slots *per
    /// installation* and paid the read-admission wait per installation, so a
    /// workspace with a handful of apps could exhaust its own budget during the
    /// boot burst and report itself unavailable — which is what left the agent
    /// roster empty. Widening the timeout only moved that threshold; taking one
    /// slot for the whole set removes it.
    ///
    /// Missing refs are simply absent from the map, matching
    /// [`Self::package_revision`] returning `None`. An absent scope yields an
    /// empty map rather than an error, for the same reason.
    pub async fn package_revisions(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        package_revision_refs: &[AppReference],
        now: DateTime<Utc>,
    ) -> Result<HashMap<String, AppPackageRevision>, AppRegistryError> {
        if package_revision_refs.is_empty() {
            return Ok(HashMap::new());
        }
        // Deduplicated: several installations can share one package revision,
        // and re-reading it per installation is the cost this method exists to
        // remove.
        let mut identities: Vec<String> = package_revision_refs
            .iter()
            .map(|reference| reference.to_string())
            .collect();
        identities.sort();
        identities.dedup();

        let found = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, _scope| {
                let mut statement = connection.prepare(
                    "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
                )?;
                let mut revisions: HashMap<String, AppPackageRevision> = HashMap::new();
                for identity in identities {
                    let bytes = statement
                        .query_row(params![identity], |row| row.get::<_, Vec<u8>>(0))
                        .optional()?;
                    let Some(bytes) = bytes else { continue };
                    let revision: AppPackageRevision =
                        decode_app_contract(&bytes, &AppContractLimits::default())
                            .map_err(AppRegistryError::from)?;
                    revisions.insert(identity, revision);
                }
                Ok(revisions)
            })
            .await?;
        Ok(found.unwrap_or_default())
    }

    pub async fn computed_capability_scope_overlay(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<AppComputedCapabilityScopeOverlay, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, _| {
                load_computed_capability_overlay_blocking(connection)
            })
            .await?;
        Ok(loaded.unwrap_or_else(AppComputedCapabilityScopeOverlay::empty))
    }

    pub async fn package_dependency_lock(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        package_revision_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<Option<AppPackageLock>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let scope = authenticated_scope.scope().clone();
        self.read_encoded_record(
            scope,
            "SELECT dependency_lock_json FROM app_package_revisions WHERE package_revision_ref = \
             ?1",
            package_revision_ref.to_string(),
            |bytes| decode_persisted_package_lock(bytes).map_err(AppRegistryError::from),
        )
        .await
    }

    /// The revision already published for one package version in this scope,
    /// if any. Boot admission asks before minting a second revision for a
    /// version the immutable-version index would refuse, so it can tell "same
    /// bytes, drifted dependency lock" from "changed bytes without a version
    /// bump" and say which.
    pub async fn package_revision_for_version(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        package_id: &AppReference,
        semantic_version: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<AppPackageRevision>, AppRegistryError> {
        let package_id = package_id.clone();
        let semantic_version = semantic_version.to_owned();
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, _| {
                let bytes: Option<Vec<u8>> = connection
                    .query_row(
                        "SELECT record_json FROM app_package_revisions
                         WHERE package_id = ?1 AND semantic_version = ?2
                         ORDER BY created_at DESC
                         LIMIT 1",
                        params![package_id.as_str(), semantic_version],
                        |row| row.get(0),
                    )
                    .optional()?;
                bytes
                    .map(|record| decode_app_contract(&record, &AppContractLimits::default()))
                    .transpose()
                    .map_err(AppRegistryError::from)
            })
            .await?;
        Ok(loaded.flatten())
    }

    /// Admit one externally authored standalone procedure into the immutable
    /// scoped registry. Identity and semver come from the already bounded
    /// `SKILL.md`; the next revision and immutable reference are allocated by
    /// this immediate transaction, never by the caller.
    pub async fn publish_standalone_procedure_candidate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        candidate: &AppStandaloneProcedureCandidate,
        now: DateTime<Utc>,
    ) -> Result<AppSkillRevisionPublicationReceipt, AppRegistryError> {
        self.publish_write_once_skill_document(
            authenticated_scope,
            candidate.dependency_ref().clone(),
            candidate.semantic_version().to_owned(),
            candidate.content_digest().clone(),
            candidate.skill_document_bytes().to_vec(),
            "immutable procedure",
            now,
        )
        .await
    }

    pub async fn publish_standalone_capability_candidate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        candidate: &super::skill_dependencies::AppStandaloneCapabilityCandidate,
        now: DateTime<Utc>,
    ) -> Result<AppSkillRevisionPublicationReceipt, AppRegistryError> {
        self.publish_write_once_skill_document(
            authenticated_scope,
            candidate.dependency_ref().clone(),
            candidate.semantic_version().to_owned(),
            candidate.content_digest().clone(),
            candidate.skill_document_bytes().to_vec(),
            "immutable capability",
            now,
        )
        .await
    }

    async fn publish_write_once_skill_document(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        dependency_ref: AppReference,
        semantic_version: String,
        content_digest: crate::magician_v2::apps::models::AppDigest,
        skill_document: Vec<u8>,
        entity_label: &'static str,
        now: DateTime<Utc>,
    ) -> Result<AppSkillRevisionPublicationReceipt, AppRegistryError> {
        let byte_count = i64::try_from(skill_document.len()).map_err(|_| {
            AppRegistryError::InvalidControlPlane(
                "immutable procedure byte count exceeds the SQLite range".to_owned(),
            )
        })?;
        let capped_read_len = i64::try_from(
            tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES.saturating_add(1),
        )
        .map_err(|_| {
            AppRegistryError::InvalidControlPlane(
                "immutable procedure read ceiling exceeds the SQLite range".to_owned(),
            )
        })?;
        let created_at = now.to_rfc3339_opts(SecondsFormat::Micros, true);

        self.execute_scoped_write(authenticated_scope, &now, move |connection, _| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing = transaction
                .query_row(
                    "SELECT revision.immutable_revision_ref,
                            revision.revision,
                            revision.content_digest,
                            blob.byte_count,
                            substr(blob.skill_document, 1, ?3)
                       FROM app_skill_revisions revision
                       JOIN app_skill_revision_blobs blob
                         ON blob.content_digest = revision.content_digest
                      WHERE revision.dependency_ref = ?1
                        AND revision.semantic_version = ?2",
                    params![dependency_ref.as_str(), semantic_version, capped_read_len],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Vec<u8>>(4)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((stored_ref, stored_revision, stored_digest, stored_count, stored_bytes)) =
                existing
            {
                if stored_digest != content_digest.as_str()
                    || stored_count != byte_count
                    || stored_bytes != skill_document
                {
                    return Err(AppRegistryError::IdentityConflict {
                        entity: entity_label,
                        identity: format!("{dependency_ref}@{semantic_version}"),
                    });
                }
                let stored_revision = u64::try_from(stored_revision)
                    .ok()
                    .and_then(|revision| AppRevision::new(revision).ok())
                    .ok_or_else(|| {
                        AppRegistryError::InvalidControlPlane(
                            "stored immutable procedure revision is invalid".to_owned(),
                        )
                    })?;
                transaction.commit()?;
                return Ok(AppSkillRevisionPublicationReceipt {
                    dependency_ref,
                    semantic_version,
                    immutable_revision_ref: AppReference::parse(stored_ref)?,
                    revision: stored_revision,
                    content_digest,
                    outcome: AppRegistryPublicationOutcome::AlreadyPresent,
                });
            }

            let latest_revision = transaction.query_row(
                "SELECT COALESCE(MAX(revision), 0)
                   FROM app_skill_revisions
                  WHERE dependency_ref = ?1",
                params![dependency_ref.as_str()],
                |row| row.get::<_, i64>(0),
            )?;
            let next_revision = latest_revision.checked_add(1).ok_or_else(|| {
                AppRegistryError::InvalidControlPlane(
                    "immutable procedure revision sequence is exhausted".to_owned(),
                )
            })?;
            let next_revision = u64::try_from(next_revision)
                .ok()
                .and_then(|revision| AppRevision::new(revision).ok())
                .ok_or_else(|| {
                    AppRegistryError::InvalidControlPlane(
                        "immutable procedure revision sequence is invalid".to_owned(),
                    )
                })?;
            let immutable_revision_ref = canonical_skill_revision_ref(
                &dependency_ref,
                &semantic_version,
                next_revision,
                &content_digest,
            )?;

            let existing_blob = transaction
                .query_row(
                    "SELECT byte_count, substr(skill_document, 1, ?2)
                       FROM app_skill_revision_blobs
                      WHERE content_digest = ?1",
                    params![content_digest.as_str(), capped_read_len],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            match existing_blob {
                Some((stored_count, stored_bytes))
                    if stored_count == byte_count && stored_bytes == skill_document => {},
                Some(_) => {
                    return Err(AppRegistryError::IdentityConflict {
                        entity: "immutable procedure content digest",
                        identity: content_digest.to_string(),
                    });
                },
                None => {
                    transaction.execute(
                        "INSERT INTO app_skill_revision_blobs (
                             content_digest, byte_count, skill_document, created_at
                         ) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            content_digest.as_str(),
                            byte_count,
                            skill_document.as_slice(),
                            created_at,
                        ],
                    )?;
                },
            }
            transaction.execute(
                "INSERT INTO app_skill_revisions (
                     immutable_revision_ref, dependency_ref, semantic_version,
                     revision, content_digest, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    immutable_revision_ref.as_str(),
                    dependency_ref.as_str(),
                    semantic_version,
                    i64::try_from(next_revision.get()).map_err(|_| {
                        AppRegistryError::InvalidControlPlane(
                            "immutable procedure revision exceeds the SQLite range".to_owned(),
                        )
                    })?,
                    content_digest.as_str(),
                    created_at,
                ],
            )?;
            transaction.commit()?;
            Ok(AppSkillRevisionPublicationReceipt {
                dependency_ref,
                semantic_version,
                immutable_revision_ref,
                revision: next_revision,
                content_digest,
                outcome: AppRegistryPublicationOutcome::Created,
            })
        })
        .await
    }

    /// Load one write-once skill document by its immutable revision reference.
    /// A mutable `capability:` or `skill:` name is not a revision and fails
    /// closed without consulting ordinary skill discovery.
    pub async fn resolve_standalone_skill_document(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        immutable_revision_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<(AppSkillRevisionPublicationReceipt, Vec<u8>), AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let identity = immutable_revision_ref.to_string();
        let immutable_revision_ref = immutable_revision_ref.clone();
        let max_bytes = tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
            .min(AppPackageLimits::default().max_bundle_file_bytes());
        let capped_read_len = i64::try_from(max_bytes.saturating_add(1)).map_err(|_| {
            AppRegistryError::InvalidControlPlane(
                "immutable skill read ceiling exceeds the SQLite range".to_owned(),
            )
        })?;
        self.execute_scoped_read(authenticated_scope, &now, move |connection, _| {
            let row = connection
                .query_row(
                    "SELECT revision.dependency_ref, revision.semantic_version,
                            revision.revision, revision.content_digest,
                            blob.byte_count, substr(blob.skill_document, 1, ?2)
                       FROM app_skill_revisions revision
                       JOIN app_skill_revision_blobs blob
                         ON blob.content_digest = revision.content_digest
                      WHERE revision.immutable_revision_ref = ?1",
                    params![immutable_revision_ref.as_str(), capped_read_len],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, Vec<u8>>(5)?,
                        ))
                    },
                )
                .optional()?;
            let Some((
                dependency_ref,
                semantic_version,
                revision,
                content_digest,
                byte_count,
                bytes,
            )) = row
            else {
                return Err(AppRegistryError::MissingRecord {
                    entity: "immutable skill revision",
                    identity: immutable_revision_ref.to_string(),
                });
            };
            if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != byte_count
                || usize::try_from(byte_count).unwrap_or(usize::MAX) > max_bytes
            {
                return Err(AppRegistryError::InvalidControlPlane(
                    "immutable skill revision bytes do not match the stored digest length"
                        .to_owned(),
                ));
            }
            let revision = u64::try_from(revision)
                .ok()
                .and_then(|revision| AppRevision::new(revision).ok())
                .ok_or_else(|| {
                    AppRegistryError::InvalidControlPlane(
                        "stored immutable skill revision is invalid".to_owned(),
                    )
                })?;
            Ok((
                AppSkillRevisionPublicationReceipt {
                    dependency_ref: AppReference::parse(dependency_ref)?,
                    semantic_version,
                    immutable_revision_ref,
                    revision,
                    content_digest: crate::magician_v2::apps::models::AppDigest::parse(
                        content_digest,
                    )?,
                    outcome: AppRegistryPublicationOutcome::AlreadyPresent,
                },
                bytes,
            ))
        })
        .await?
        .ok_or_else(|| AppRegistryError::MissingRecord {
            entity: "immutable skill revision",
            identity,
        })
    }

    /// Publish one exact standalone procedure revision into the scoped app
    /// registry. The trusted registry adapter must first parse and attest the
    /// bytes as [`AppVerifiedRegistryProcedureRevision`]; this boundary then
    /// stores them once by digest and refuses every identity rewrite.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Phase 4D trusted immutable writer; adopted by Phase 5A reviewed \
                      standalone-skill publication"
        )
    )]
    pub async fn publish_immutable_procedure_revision(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        revision: &AppVerifiedRegistryProcedureRevision,
        now: DateTime<Utc>,
    ) -> Result<AppSkillRevisionPublicationReceipt, AppRegistryError> {
        let dependency_ref = revision.dependency_ref().clone();
        let semantic_version = revision.semantic_version().to_owned();
        let immutable_revision_ref = revision.immutable_revision_ref().clone();
        let revision_number = revision.revision();
        let receipt_revision = revision_number;
        let content_digest = revision.content_digest().clone();
        let skill_document = revision.skill_document_bytes().to_vec();
        let max_bytes = tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
            .min(AppPackageLimits::default().max_bundle_file_bytes());
        if skill_document.is_empty() || skill_document.len() > max_bytes {
            return Err(AppRegistryError::InvalidControlPlane(
                "immutable procedure bytes are empty or exceed the standalone skill ceiling"
                    .to_owned(),
            ));
        }
        if AppDigest::blake3(&skill_document) != content_digest {
            return Err(AppRegistryError::InvalidControlPlane(
                "immutable procedure bytes do not match their trusted digest".to_owned(),
            ));
        }
        let byte_count = i64::try_from(skill_document.len()).map_err(|_| {
            AppRegistryError::InvalidControlPlane(
                "immutable procedure byte count exceeds the SQLite range".to_owned(),
            )
        })?;
        let revision_number = i64::try_from(revision_number.get()).map_err(|_| {
            AppRegistryError::InvalidControlPlane(
                "immutable procedure revision exceeds the SQLite range".to_owned(),
            )
        })?;
        let created_at = now.to_rfc3339_opts(SecondsFormat::Micros, true);
        let receipt_ref = immutable_revision_ref.clone();
        let receipt_digest = content_digest.clone();

        self.execute_scoped_write(authenticated_scope, &now, move |connection, _| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing_blob = transaction
                .query_row(
                    "SELECT byte_count, skill_document
                       FROM app_skill_revision_blobs
                      WHERE content_digest = ?1",
                    params![content_digest.as_str()],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            match existing_blob {
                Some((stored_count, stored_bytes))
                    if stored_count == byte_count && stored_bytes == skill_document => {},
                Some(_) => {
                    return Err(AppRegistryError::IdentityConflict {
                        entity: "immutable procedure content digest",
                        identity: content_digest.to_string(),
                    });
                },
                None => {
                    transaction.execute(
                        "INSERT INTO app_skill_revision_blobs (
                             content_digest, byte_count, skill_document, created_at
                         ) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            content_digest.as_str(),
                            byte_count,
                            skill_document.as_slice(),
                            created_at,
                        ],
                    )?;
                },
            }

            let existing_revision = transaction
                .query_row(
                    "SELECT dependency_ref, semantic_version, revision, content_digest
                       FROM app_skill_revisions
                      WHERE immutable_revision_ref = ?1",
                    params![immutable_revision_ref.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?;
            let exact_replay = existing_revision.as_ref().is_some_and(|stored| {
                stored.0 == dependency_ref.as_str()
                    && stored.1 == semantic_version
                    && stored.2 == revision_number
                    && stored.3 == content_digest.as_str()
            });
            if existing_revision.is_some() && !exact_replay {
                return Err(AppRegistryError::IdentityConflict {
                    entity: "immutable procedure revision",
                    identity: immutable_revision_ref.to_string(),
                });
            }
            if !exact_replay {
                let conflicting_ref = transaction
                    .query_row(
                        "SELECT immutable_revision_ref
                           FROM app_skill_revisions
                          WHERE dependency_ref = ?1
                            AND (semantic_version = ?2 OR revision = ?3)
                          LIMIT 1",
                        params![dependency_ref.as_str(), semantic_version, revision_number,],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if let Some(conflicting_ref) = conflicting_ref {
                    return Err(AppRegistryError::IdentityConflict {
                        entity: "immutable procedure dependency version",
                        identity: conflicting_ref,
                    });
                }
                transaction.execute(
                    "INSERT INTO app_skill_revisions (
                         immutable_revision_ref, dependency_ref, semantic_version,
                         revision, content_digest, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        immutable_revision_ref.as_str(),
                        dependency_ref.as_str(),
                        semantic_version,
                        revision_number,
                        content_digest.as_str(),
                        created_at,
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(AppSkillRevisionPublicationReceipt {
                dependency_ref,
                semantic_version,
                immutable_revision_ref: receipt_ref,
                revision: receipt_revision,
                content_digest: receipt_digest,
                outcome: if exact_replay {
                    AppRegistryPublicationOutcome::AlreadyPresent
                } else {
                    AppRegistryPublicationOutcome::Created
                },
            })
        })
        .await
    }

    /// Reconstruct only the registry-backed procedure revisions named by one
    /// workflow from their exact package-lock identities. Vendored procedures
    /// remain package-owned and are intentionally skipped. Missing, rewritten
    /// or corrupt registry bytes fail the whole read; mutable scoped skill
    /// discovery is never consulted as a fallback.
    pub async fn locked_procedure_revisions(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        lock: &AppPackageLock,
        workflow_procedures: &[AppReference],
        now: DateTime<Utc>,
    ) -> Result<Vec<AppVerifiedRegistryProcedureRevision>, AppRegistryError> {
        authenticated_scope.ensure_live_at(&now)?;
        let limits = AppPackageLimits::default();
        if workflow_procedures.len() > limits.max_dependencies() {
            return Err(AppRegistryError::InvalidControlPlane(
                "workflow procedure count exceeds the app dependency ceiling".to_owned(),
            ));
        }
        let mut seen = HashSet::with_capacity(workflow_procedures.len());
        let mut expected = Vec::with_capacity(workflow_procedures.len());
        for dependency_ref in workflow_procedures {
            if !seen.insert(dependency_ref.clone()) {
                return Err(AppRegistryError::InvalidControlPlane(format!(
                    "workflow procedure `{dependency_ref}` is duplicated"
                )));
            }
            let locked = lock
                .dependencies()
                .iter()
                .find(|dependency| dependency.dependency_ref() == dependency_ref)
                .ok_or_else(|| AppRegistryError::MissingRecord {
                    entity: "locked procedure dependency",
                    identity: dependency_ref.to_string(),
                })?;
            if locked.kind() != AppDependencyKind::ProcedureSkill {
                return Err(AppRegistryError::StateConflict(format!(
                    "locked dependency `{dependency_ref}` is not a procedure skill"
                )));
            }
            let AppLockedDependencySource::RegistryRevision {
                immutable_revision_ref,
                revision,
            } = locked.source()
            else {
                continue;
            };
            expected.push((
                dependency_ref.clone(),
                locked.semantic_version().to_owned(),
                immutable_revision_ref.clone(),
                *revision,
                locked.content_digest().clone(),
            ));
        }
        if expected.is_empty() {
            return Ok(Vec::new());
        }
        let max_revision_bytes = tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES
            .min(limits.max_bundle_file_bytes());
        let capped_read_len =
            i64::try_from(max_revision_bytes.saturating_add(1)).map_err(|_| {
                AppRegistryError::InvalidControlPlane(
                    "immutable procedure read ceiling exceeds the SQLite range".to_owned(),
                )
            })?;

        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, _| {
                let mut statement = connection.prepare(
                    "SELECT revision.dependency_ref, revision.semantic_version,
                            revision.revision, revision.content_digest,
                            blob.byte_count, substr(blob.skill_document, 1, ?2)
                       FROM app_skill_revisions revision
                       JOIN app_skill_revision_blobs blob
                         ON blob.content_digest = revision.content_digest
                      WHERE revision.immutable_revision_ref = ?1",
                )?;
                let mut total_bytes = 0usize;
                let mut revisions = Vec::with_capacity(expected.len());
                for (
                    dependency_ref,
                    semantic_version,
                    immutable_revision_ref,
                    expected_revision,
                    content_digest,
                ) in expected
                {
                    let row = statement
                        .query_row(
                            params![immutable_revision_ref.as_str(), capped_read_len],
                            |row| {
                                Ok((
                                    row.get::<_, String>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, i64>(2)?,
                                    row.get::<_, String>(3)?,
                                    row.get::<_, i64>(4)?,
                                    row.get::<_, Vec<u8>>(5)?,
                                ))
                            },
                        )
                        .optional()?
                        .ok_or_else(|| AppRegistryError::MissingRecord {
                            entity: "immutable procedure revision",
                            identity: immutable_revision_ref.to_string(),
                        })?;
                    let stored_revision = u64::try_from(row.2)
                        .ok()
                        .and_then(|value| AppRevision::new(value).ok());
                    let stored_byte_count = usize::try_from(row.4).ok();
                    if row.5.len() > max_revision_bytes
                        || stored_byte_count.is_some_and(|count| count > max_revision_bytes)
                    {
                        return Err(AppRegistryError::InvalidControlPlane(format!(
                            "locked procedure `{dependency_ref}` exceeds the immutable skill byte \
                             ceiling"
                        )));
                    }
                    if row.0 != dependency_ref.as_str()
                        || row.1 != semantic_version
                        || stored_revision != Some(expected_revision)
                        || row.3 != content_digest.as_str()
                        || stored_byte_count != Some(row.5.len())
                        || AppDigest::blake3(&row.5) != content_digest
                    {
                        return Err(AppRegistryError::IdentityConflict {
                            entity: "locked immutable procedure revision",
                            identity: immutable_revision_ref.to_string(),
                        });
                    }
                    total_bytes = total_bytes.checked_add(row.5.len()).ok_or_else(|| {
                        AppRegistryError::InvalidControlPlane(
                            "locked procedure byte count overflowed".to_owned(),
                        )
                    })?;
                    if total_bytes > limits.max_bundle_bytes() {
                        return Err(AppRegistryError::InvalidControlPlane(
                            "locked procedure bytes exceed the aggregate app package ceiling"
                                .to_owned(),
                        ));
                    }
                    let verified =
                        AppVerifiedRegistryProcedureRevision::from_trusted_registry_bytes(
                            dependency_ref,
                            semantic_version,
                            immutable_revision_ref.clone(),
                            expected_revision,
                            &row.5,
                        )
                        .map_err(|error| AppRegistryError::StateConflict(error.to_string()))?;
                    revisions.push(verified);
                }
                Ok(revisions)
            })
            .await?;
        loaded.ok_or_else(|| AppRegistryError::MissingRecord {
            entity: "scope registry",
            identity: authenticated_scope.scope_binding_ref().to_string(),
        })
    }

    async fn read_record<T>(
        &self,
        scope: AppScope,
        sql: &'static str,
        identity: String,
    ) -> Result<Option<T>, AppRegistryError>
    where
        T: ValidateAppContract + serde::de::DeserializeOwned + Send + 'static,
    {
        self.read_encoded_record(scope, sql, identity, |bytes| {
            decode_app_contract(bytes, &AppContractLimits::default())
                .map_err(AppRegistryError::from)
        })
        .await
    }

    async fn read_encoded_record<T, F>(
        &self,
        scope: AppScope,
        sql: &'static str,
        identity: String,
        decode: F,
    ) -> Result<Option<T>, AppRegistryError>
    where
        T: Send + 'static,
        F: FnOnce(&[u8]) -> Result<T, AppRegistryError> + Send + 'static,
    {
        if !self.ensure_existing_scope_schema_current(&scope).await? {
            return Ok(None);
        }
        let permit = self.acquire_read_slot(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _permit = permit;
            let database_path = crate::magician_v2::database_owners::database_file_path(
                &workspace,
                scope.principal.as_str(),
                scope.workspace.as_str(),
                crate::magician_v2::database_owners::DatabaseOwner::AppStoreSqlite,
            );
            if let Err(error) =
                validate_existing_registry_paths(workspace.base_root(), &database_path)
            {
                if matches!(
                    &error,
                    AppRegistryError::Io(io_error)
                        if io_error.kind() == std::io::ErrorKind::NotFound
                ) {
                    return Ok(None);
                }
                return Err(error);
            }
            match fs::symlink_metadata(&database_path) {
                Ok(metadata) if metadata.is_file() => {},
                Ok(_) => {
                    return Err(AppRegistryError::UnsafePath(format!(
                        "database path '{}' is not a regular file",
                        database_path.display()
                    )));
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            let connection = open_registry_connection(&database_path, &scope, false)?;
            let bytes = connection
                .query_row(sql, params![identity], |row| row.get::<_, Vec<u8>>(0))
                .optional()?;
            bytes.map(|bytes| decode(&bytes)).transpose()
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Run one bounded read against the existing scoped app database without
    /// materializing an absent scope. Phase-2 store adapters use this owner
    /// instead of opening a competing connection pool or deriving paths.
    pub async fn execute_scoped_read<T, F>(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
        operation: F,
    ) -> Result<Option<T>, AppRegistryError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection, &AppScope) -> Result<T, AppRegistryError> + Send + 'static,
    {
        authenticated_scope.ensure_live_at(now)?;
        let scope = authenticated_scope.scope().clone();
        if !self.ensure_existing_scope_schema_current(&scope).await? {
            return Ok(None);
        }
        let permit = self.acquire_read_slot(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _permit = permit;
            let database_path = crate::magician_v2::database_owners::database_file_path(
                &workspace,
                scope.principal.as_str(),
                scope.workspace.as_str(),
                crate::magician_v2::database_owners::DatabaseOwner::AppStoreSqlite,
            );
            if let Err(error) =
                validate_existing_registry_paths(workspace.base_root(), &database_path)
            {
                if matches!(
                    &error,
                    AppRegistryError::Io(io_error)
                        if io_error.kind() == std::io::ErrorKind::NotFound
                ) {
                    return Ok(None);
                }
                return Err(error);
            }
            match fs::symlink_metadata(&database_path) {
                Ok(metadata) if metadata.is_file() => {},
                Ok(_) => {
                    return Err(AppRegistryError::UnsafePath(format!(
                        "database path '{}' is not a regular file",
                        database_path.display()
                    )));
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            let connection = open_registry_connection(&database_path, &scope, false)?;
            operation(&connection, &scope).map(Some)
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Read the scoped behavior kill switch without materializing an absent
    /// registry. Workflow resume/provider boundaries use this in addition to
    /// the scheduler's claim-time check so pausing a scope also fences work
    /// which was accepted but has not crossed its next effect boundary.
    pub async fn background_behavior_scope_paused(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<bool, AppRegistryError> {
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, |connection, _| {
                let paused = connection
                    .query_row(
                        "SELECT paused FROM app_behavior_scope_policy WHERE singleton = 1",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                match paused {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(AppRegistryError::StateConflict(
                        "app background-behavior scope policy is corrupt".to_owned(),
                    )),
                }
            })
            .await?;
        Ok(loaded.unwrap_or(false))
    }

    /// Final scheduler-side proof for a background workflow root. In one
    /// scoped snapshot this joins pause, current install/grant/schema authority
    /// and the exact live pending fire. Callers hold Artifact's canonical task
    /// start-admission lock, so a destructive scheduler transition cannot
    /// concurrently invalidate that fire before the immediately following
    /// root commit.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn background_behavior_launch_is_live(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        schema_revision: AppRevision,
        grant_revision: AppRevision,
        behavior_id: AppName,
        behavior_digest: AppDigest,
        fire_ref: AppReference,
        now: DateTime<Utc>,
    ) -> Result<bool, AppRegistryError> {
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                let schema_revision_value = schema_revision.get();
                let grant_revision_value = grant_revision.get();
                let live = connection
                    .query_row(
                        "SELECT i.record_json, COALESCE(p.paused, 0)
                           FROM app_behavior_heads h
                           JOIN app_installations i USING(installation_id)
                           LEFT JOIN app_behavior_scope_policy p ON p.singleton = 1
                          WHERE h.installation_id = ?1
                            AND h.installation_generation = ?2
                            AND h.package_revision_ref = ?3
                            AND h.schema_revision = ?4 AND h.grant_revision = ?5
                            AND h.behavior_id = ?6 AND h.behavior_digest = ?7
                            AND h.pending_fire_ref = ?8 AND h.state = 'pending'
                            AND h.lease_token IS NOT NULL
                            AND h.lease_expires_at IS NOT NULL
                            AND h.lease_expires_at > ?9
                            AND i.lifecycle_status = 'enabled'
                            AND i.lifecycle_generation = ?2
                            AND i.package_revision_ref = ?3
                            AND COALESCE(p.paused, 0) = 0
                            AND EXISTS (
                                SELECT 1 FROM app_grant_revisions g
                                 WHERE g.installation_id = ?1 AND g.revision = ?5
                                   AND g.package_revision_ref = ?3
                                   AND g.revoked_at IS NULL
                            )
                            AND EXISTS (
                                SELECT 1 FROM app_schema_revisions s
                                 WHERE s.installation_id = ?1 AND s.revision = ?4
                                   AND s.package_revision_ref = ?3
                            )",
                        params![
                            installation_id.as_str(),
                            i64::try_from(installation_generation).map_err(|_| {
                                AppRegistryError::StateConflict(
                                    "background behavior installation generation overflow"
                                        .to_owned(),
                                )
                            })?,
                            package_revision_ref.as_str(),
                            i64::try_from(schema_revision_value).map_err(|_| {
                                AppRegistryError::StateConflict(
                                    "background behavior schema revision overflow".to_owned(),
                                )
                            })?,
                            i64::try_from(grant_revision_value).map_err(|_| {
                                AppRegistryError::StateConflict(
                                    "background behavior grant revision overflow".to_owned(),
                                )
                            })?,
                            behavior_id.as_str(),
                            behavior_digest.as_str(),
                            fire_ref.as_str(),
                            now.to_rfc3339_opts(SecondsFormat::Micros, true),
                        ],
                        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .optional()?;
                let Some((record, paused)) = live else {
                    return Ok(false);
                };
                let installation: AppInstallation =
                    decode_app_contract(&record, &AppContractLimits::default())?;
                Ok(paused == 0
                    && installation.scope == *scope
                    && installation.installation_id == installation_id
                    && installation.lifecycle.status == AppInstallationStatus::Enabled
                    && installation.lifecycle.generation == installation_generation
                    && installation.package_revision_ref == package_revision_ref
                    && installation.active_schema_revision.map(AppRevision::get)
                        == Some(schema_revision_value)
                    && installation.grant_revision.map(AppRevision::get)
                        == Some(grant_revision_value))
            })
            .await?;
        Ok(loaded.unwrap_or(false))
    }

    /// Final event-router proof immediately before an Artifact workflow root
    /// is committed. The server-sealed launch reference names one exact fire;
    /// current installation, grant, schema, pause, lease, and rate admission
    /// must all still agree in the same scoped snapshot.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn event_behavior_launch_is_live(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_revision_ref: AppReference,
        schema_revision: AppRevision,
        grant_revision: AppRevision,
        event_behavior_id: AppName,
        reviewed_request_digest: AppDigest,
        launch_ref: AppReference,
        now: DateTime<Utc>,
    ) -> Result<bool, AppRegistryError> {
        let loaded = self
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                let schema_revision_value = schema_revision.get();
                let grant_revision_value = grant_revision.get();
                let live = connection
                    .query_row(
                        "SELECT i.record_json, COALESCE(p.paused, 0)
                           FROM app_event_behavior_fires f
                           JOIN app_installations i USING(installation_id)
                           LEFT JOIN app_behavior_scope_policy p ON p.singleton = 1
                          WHERE f.installation_id = ?1
                            AND f.installation_generation = ?2
                            AND f.package_revision_ref = ?3
                            AND f.schema_revision = ?4 AND f.grant_revision = ?5
                            AND f.event_behavior_id = ?6
                            AND f.reviewed_request_digest = ?7
                            AND f.launch_ref = ?8 AND f.state = 'leased'
                            AND f.rate_admitted_at IS NOT NULL
                            AND f.lease_token IS NOT NULL
                            AND f.lease_expires_at IS NOT NULL
                            AND f.lease_expires_at > ?9
                            AND i.lifecycle_status = 'enabled'
                            AND i.lifecycle_generation = ?2
                            AND i.package_revision_ref = ?3
                            AND COALESCE(p.paused, 0) = 0
                            AND EXISTS (
                                SELECT 1 FROM app_grant_revisions g
                                 WHERE g.installation_id = ?1 AND g.revision = ?5
                                   AND g.package_revision_ref = ?3
                                   AND g.revoked_at IS NULL
                            )
                            AND EXISTS (
                                SELECT 1 FROM app_schema_revisions s
                                 WHERE s.installation_id = ?1 AND s.revision = ?4
                                   AND s.package_revision_ref = ?3
                            )",
                        params![
                            installation_id.as_str(),
                            i64::try_from(installation_generation).map_err(|_| {
                                AppRegistryError::StateConflict(
                                    "event behavior installation generation overflow".to_owned(),
                                )
                            })?,
                            package_revision_ref.as_str(),
                            i64::try_from(schema_revision_value).map_err(|_| {
                                AppRegistryError::StateConflict(
                                    "event behavior schema revision overflow".to_owned(),
                                )
                            })?,
                            i64::try_from(grant_revision_value).map_err(|_| {
                                AppRegistryError::StateConflict(
                                    "event behavior grant revision overflow".to_owned(),
                                )
                            })?,
                            event_behavior_id.as_str(),
                            reviewed_request_digest.as_str(),
                            launch_ref.as_str(),
                            now.to_rfc3339_opts(SecondsFormat::Micros, true),
                        ],
                        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .optional()?;
                let Some((record, paused)) = live else {
                    return Ok(false);
                };
                let installation: AppInstallation =
                    decode_app_contract(&record, &AppContractLimits::default())?;
                Ok(paused == 0
                    && installation.scope == *scope
                    && installation.installation_id == installation_id
                    && installation.lifecycle.status == AppInstallationStatus::Enabled
                    && installation.lifecycle.generation == installation_generation
                    && installation.package_revision_ref == package_revision_ref
                    && installation.active_schema_revision.map(AppRevision::get)
                        == Some(schema_revision_value)
                    && installation.grant_revision.map(AppRevision::get)
                        == Some(grant_revision_value))
            })
            .await?;
        Ok(loaded.unwrap_or(false))
    }

    /// Typed counterpart to [`Self::execute_scoped_read`]. Missing scopes stay
    /// lazy while domain-specific corruption and policy failures retain their
    /// structured error type.
    pub async fn execute_scoped_typed_read<T, E, F>(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
        operation: F,
    ) -> Result<Option<T>, E>
    where
        T: Send + 'static,
        E: From<AppRegistryError> + Send + 'static,
        F: FnOnce(&Connection, &AppScope) -> Result<T, E> + Send + 'static,
    {
        authenticated_scope
            .ensure_live_at(now)
            .map_err(AppRegistryError::from)
            .map_err(E::from)?;
        let scope = authenticated_scope.scope().clone();
        if !self
            .ensure_existing_scope_schema_current(&scope)
            .await
            .map_err(E::from)?
        {
            return Ok(None);
        }
        let permit = self.acquire_read_slot(&scope).await.map_err(E::from)?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _permit = permit;
            let database_path = crate::magician_v2::database_owners::database_file_path(
                &workspace,
                scope.principal.as_str(),
                scope.workspace.as_str(),
                crate::magician_v2::database_owners::DatabaseOwner::AppStoreSqlite,
            );
            if let Err(error) =
                validate_existing_registry_paths(workspace.base_root(), &database_path)
            {
                if matches!(
                    &error,
                    AppRegistryError::Io(io_error)
                        if io_error.kind() == std::io::ErrorKind::NotFound
                ) {
                    return Ok(None);
                }
                return Err(E::from(error));
            }
            match fs::symlink_metadata(&database_path) {
                Ok(metadata) if metadata.is_file() => {},
                Ok(_) => {
                    return Err(E::from(AppRegistryError::UnsafePath(format!(
                        "database path '{}' is not a regular file",
                        database_path.display()
                    ))));
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(E::from(AppRegistryError::Io(error))),
            }
            let connection =
                open_registry_connection(&database_path, &scope, false).map_err(E::from)?;
            operation(&connection, &scope).map(Some)
        })
        .await
        .map_err(|error| E::from(AppRegistryError::WorkerTerminated(error.to_string())))?
    }

    pub async fn execute_scoped_write<T, F>(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
        operation: F,
    ) -> Result<T, AppRegistryError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &AppScope) -> Result<T, AppRegistryError> + Send + 'static,
    {
        authenticated_scope.ensure_live_at(now)?;
        let scope = authenticated_scope.scope().clone();
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            operation(&mut connection, &scope)
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Test-only corruption/transition seam for API boundary tests outside the
    /// `apps` module. Production callers must use a typed registry owner rather
    /// than receiving a reusable raw SQLite connection callback.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn execute_scoped_test_write<T, F>(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
        operation: F,
    ) -> Result<T, AppRegistryError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &AppScope) -> Result<T, AppRegistryError> + Send + 'static,
    {
        self.execute_scoped_write(authenticated_scope, now, operation)
            .await
    }

    /// Execute a registry-owned scoped write while preserving a caller's
    /// domain error. Canonical subsystems that share this SQLite authority use
    /// this adapter so contract denials are not flattened into registry text.
    /// It intentionally exposes neither paths nor a reusable connection.
    pub async fn execute_scoped_typed_write<T, E, F>(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
        operation: F,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<AppRegistryError> + Send + 'static,
        F: FnOnce(&mut Connection, &AppScope) -> Result<T, E> + Send + 'static,
    {
        authenticated_scope
            .ensure_live_at(now)
            .map_err(AppRegistryError::from)
            .map_err(E::from)?;
        let scope = authenticated_scope.scope().clone();
        let (permit, write_guard) = self
            .acquire_write_admission(&scope)
            .await
            .map_err(E::from)?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let mut connection =
                open_scoped_registry_for_write(&workspace, &scope).map_err(E::from)?;
            operation(&mut connection, &scope)
        })
        .await
        .map_err(|error| E::from(AppRegistryError::WorkerTerminated(error.to_string())))?
    }

    /// Settlement-only write lane for an operation that was admitted while
    /// its authenticated credential was live. The non-Serde permit is minted
    /// by the resource owner from the retained root + operation capabilities;
    /// this bypasses credential *liveness* only. The resource transaction
    /// still validates exact historical scope/actor/session, tree identity,
    /// reservation identity and CAS fence, and this adapter exposes no reserve
    /// or start primitive.
    pub(super) async fn execute_scoped_typed_accepted_settlement_write<T, E, F>(
        &self,
        permit: &AppResourceAcceptedSettlementPermit,
        operation: F,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<AppRegistryError> + Send + 'static,
        F: FnOnce(&mut Connection, &AppScope) -> Result<T, E> + Send + 'static,
    {
        let scope = permit.authenticated_scope().scope().clone();
        let (blocking_permit, write_guard) = self
            .acquire_write_admission(&scope)
            .await
            .map_err(E::from)?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _blocking_permit = blocking_permit;
            let mut connection =
                open_scoped_registry_for_write(&workspace, &scope).map_err(E::from)?;
            operation(&mut connection, &scope)
        })
        .await
        .map_err(|error| E::from(AppRegistryError::WorkerTerminated(error.to_string())))?
    }

    /// Background maintenance shares the same per-scope serialization but may
    /// consume at most `blocking_capacity - 1` workers, preserving one lane for
    /// foreground reads/writes. Admission is cancellation-safe and remains
    /// bounded by the owning worker's scope deadline.
    pub async fn execute_scoped_background_write<T, F>(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
        operation: F,
    ) -> Result<T, AppRegistryError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &AppScope) -> Result<T, AppRegistryError> + Send + 'static,
    {
        authenticated_scope.ensure_live_at(now)?;
        let scope = authenticated_scope.scope().clone();
        let (background_turnstile, background_permit, permit, write_guard) =
            self.acquire_background_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _background_turnstile = background_turnstile;
            let _write_guard = write_guard;
            let _background_permit = background_permit;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            operation(&mut connection, &scope)
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?
    }

    /// Typed background counterpart used by bounded canonical maintenance.
    /// It preserves one foreground blocking lane and the subsystem's structured
    /// error while exposing neither a database path nor a reusable connection.
    pub async fn execute_scoped_typed_background_write<T, E, F>(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
        operation: F,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<AppRegistryError> + Send + 'static,
        F: FnOnce(&mut Connection, &AppScope) -> Result<T, E> + Send + 'static,
    {
        authenticated_scope
            .ensure_live_at(now)
            .map_err(AppRegistryError::from)
            .map_err(E::from)?;
        let scope = authenticated_scope.scope().clone();
        let (background_turnstile, background_permit, permit, write_guard) = self
            .acquire_background_write_admission(&scope)
            .await
            .map_err(E::from)?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _background_turnstile = background_turnstile;
            let _write_guard = write_guard;
            let _background_permit = background_permit;
            let _permit = permit;
            let mut connection =
                open_scoped_registry_for_write(&workspace, &scope).map_err(E::from)?;
            operation(&mut connection, &scope)
        })
        .await
        .map_err(|error| E::from(AppRegistryError::WorkerTerminated(error.to_string())))?
    }

    async fn acquire_background_write_admission(
        &self,
        scope: &AppScope,
    ) -> Result<
        (
            tokio::sync::OwnedMutexGuard<()>,
            tokio::sync::OwnedSemaphorePermit,
            admission::ScopedPermit,
            tokio::sync::OwnedMutexGuard<()>,
        ),
        AppRegistryError,
    > {
        let mut measurement = metrics::AdmissionMeasurement::new(
            scope.principal.as_str(),
            scope.workspace.as_str(),
            "background_write",
        );
        // Serialize sibling background lanes without excluding foreground work.
        // The guard is retained by the blocking closure, so a cancelled caller
        // cannot admit another same-scope background writer over detached work.
        let maintenance = self.scope_maintenance_lock(scope).read_owned().await;
        let background_turnstile = self.scope_background_turnstile(scope).lock_owned().await;
        loop {
            // Background admission stays before the general pool so these jobs
            // can never consume this owner's foreground reserve while
            // queued for their narrower allowance.
            let background_permit = Arc::clone(&self.background_write_slots)
                .acquire_owned()
                .await
                .map_err(|_| {
                    measurement.finish(false);
                    AppRegistryError::Overloaded
                })?;
            let permit = Arc::clone(&self.blocking_slots)
                .acquire_owned()
                .await
                .map_err(|_| {
                    measurement.finish(false);
                    AppRegistryError::Overloaded
                })?;
            let write_lock = self.scope_write_lock(scope);
            if let Ok(write_guard) = Arc::clone(&write_lock).try_lock_owned() {
                measurement.finish(true);
                return Ok((
                    background_turnstile,
                    background_permit,
                    admission::ScopedPermit::new(permit, maintenance),
                    write_guard,
                ));
            }

            // A foreground owner won this scope. Do not hoard either global
            // permit behind it: release both, wait cancellation-safely for the
            // foreground turn to finish, and retry from the fair permit queues.
            drop(permit);
            drop(background_permit);
            let foreground_turn = write_lock.lock_owned().await;

            // We now own the fair scope turn without holding a global permit.
            // Take the two permits only if both are immediately available; this
            // lets bounded maintenance make progress behind a continuous
            // foreground queue without ever parking a foreground scope behind
            // an awaited process-wide permit.
            if let Ok(background_permit) =
                Arc::clone(&self.background_write_slots).try_acquire_owned()
            {
                if let Ok(permit) = Arc::clone(&self.blocking_slots).try_acquire_owned() {
                    measurement.finish(true);
                    return Ok((
                        background_turnstile,
                        background_permit,
                        admission::ScopedPermit::new(permit, maintenance),
                        foreground_turn,
                    ));
                }
            }
            drop(foreground_turn);
            tokio::task::yield_now().await;
        }
    }

    /// Take one blocking slot for a read. Dropping the caller cancels a queued
    /// acquisition without consuming a slot or starting database work.
    async fn acquire_read_slot(
        &self,
        scope: &AppScope,
    ) -> Result<admission::ScopedPermit, AppRegistryError> {
        let mut measurement = metrics::AdmissionMeasurement::new(
            scope.principal.as_str(),
            scope.workspace.as_str(),
            "read",
        );
        let result = admission::scoped_read(
            Arc::clone(&self.blocking_slots),
            self.scope_maintenance_lock(scope),
        )
        .await
        .map_err(|_| AppRegistryError::Overloaded);
        measurement.finish(result.is_ok());
        result
    }

    async fn acquire_write_admission(
        &self,
        scope: &AppScope,
    ) -> Result<(admission::ScopedPermit, tokio::sync::OwnedMutexGuard<()>), AppRegistryError> {
        let mut measurement = metrics::AdmissionMeasurement::new(
            scope.principal.as_str(),
            scope.workspace.as_str(),
            "write",
        );
        let result = admission::scoped_write(
            Arc::clone(&self.blocking_slots),
            self.scope_write_lock(scope),
            self.scope_maintenance_lock(scope),
        )
        .await
        .map_err(|_| AppRegistryError::Overloaded);
        measurement.finish(result.is_ok());
        result
    }

    async fn ensure_existing_scope_schema_current(
        &self,
        scope: &AppScope,
    ) -> Result<bool, AppRegistryError> {
        let key = Self::scope_key(scope);
        if self
            .schema_ready_scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&key)
        {
            return Ok(true);
        }

        let (permit, write_guard) = self.acquire_write_admission(scope).await?;
        if self
            .schema_ready_scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&key)
        {
            return Ok(true);
        }

        let workspace = self.workspace.clone();
        let scope = scope.clone();
        let exists = spawn_registry_work(move || {
            let _write_guard = write_guard;
            let _permit = permit;
            let database_path = crate::magician_v2::database_owners::database_file_path(
                &workspace,
                scope.principal.as_str(),
                scope.workspace.as_str(),
                crate::magician_v2::database_owners::DatabaseOwner::AppStoreSqlite,
            );
            if let Err(error) =
                validate_existing_registry_paths(workspace.base_root(), &database_path)
            {
                if matches!(
                    &error,
                    AppRegistryError::Io(io_error)
                        if io_error.kind() == std::io::ErrorKind::NotFound
                ) {
                    return Ok(false);
                }
                return Err(error);
            }
            match fs::symlink_metadata(&database_path) {
                Ok(metadata) if metadata.is_file() => {},
                Ok(_) => {
                    return Err(AppRegistryError::UnsafePath(format!(
                        "database path '{}' is not a regular file",
                        database_path.display()
                    )));
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.into()),
            }
            open_registry_connection(&database_path, &scope, true)?;
            Ok(true)
        })
        .await
        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))??;

        if exists {
            let mut ready_scopes = self
                .schema_ready_scopes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if ready_scopes.len() >= MAX_SCHEMA_READY_SCOPES {
                ready_scopes.clear();
            }
            ready_scopes.insert(key);
        }
        Ok(exists)
    }

    fn scope_key(scope: &AppScope) -> (String, String) {
        (
            scope.principal.as_str().to_owned(),
            scope.workspace.as_str().to_owned(),
        )
    }

    fn scope_maintenance_lock(&self, scope: &AppScope) -> Arc<tokio::sync::RwLock<()>> {
        admission::database_maintenance_lock(
            &self
                .workspace
                .app_store_db_path(scope.principal.as_str(), scope.workspace.as_str()),
        )
    }

    fn scope_write_lock(&self, scope: &AppScope) -> Arc<AsyncMutex<()>> {
        admission::database_write_lock(
            &self
                .workspace
                .app_store_db_path(scope.principal.as_str(), scope.workspace.as_str()),
        )
    }

    fn scope_background_turnstile(&self, scope: &AppScope) -> Arc<AsyncMutex<()>> {
        admission::database_background_turnstile(
            &self
                .workspace
                .app_store_db_path(scope.principal.as_str(), scope.workspace.as_str()),
        )
    }
}

fn load_computed_capability_overlay_blocking(
    connection: &Connection,
) -> Result<AppComputedCapabilityScopeOverlay, AppRegistryError> {
    let limits = AppContractLimits::default();
    let mut install_statement = connection
        .prepare("SELECT record_json FROM app_installations WHERE lifecycle_status = 'enabled'")?;
    let install_rows = install_statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut catalogs = Vec::new();
    for record in install_rows {
        let installation: AppInstallation = decode_app_contract(&record, &limits)?;
        if installation.lifecycle.status != AppInstallationStatus::Enabled {
            continue;
        }
        let Some(grant_revision) = installation.grant_revision else {
            continue;
        };
        let grant_revision_i64 = i64::try_from(grant_revision.get()).map_err(|_| {
            AppRegistryError::StateConflict("grant revision is outside the SQLite range".to_owned())
        })?;
        let grant_bytes: Option<Vec<u8>> = connection
            .query_row(
                "SELECT record_json FROM app_grant_revisions
                  WHERE installation_id = ?1 AND revision = ?2",
                params![installation.installation_id.as_str(), grant_revision_i64],
                |row| row.get(0),
            )
            .optional()?;
        let Some(grant_bytes) = grant_bytes else {
            continue;
        };
        let grant: AppGrantRevision = decode_app_contract(&grant_bytes, &limits)?;
        if grant.revoked_at.is_some() {
            continue;
        }
        let lock_bytes: Option<Vec<u8>> = connection
            .query_row(
                "SELECT dependency_lock_json FROM app_package_revisions
                  WHERE package_revision_ref = ?1",
                params![installation.package_revision_ref.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(lock_bytes) = lock_bytes else {
            continue;
        };
        let lock = decode_persisted_package_lock(&lock_bytes)?;
        let permitted = grant.granted_tools.iter().cloned().collect();
        let catalog = match compile_computed_capability_catalog(
            &lock,
            AppComputedCapabilityAdmission {
                installation_id: installation.installation_id.clone(),
                installation_status: installation.lifecycle.status,
                grant_revoked: grant.revoked_at.is_some(),
                package_revision_ref: &installation.package_revision_ref,
                live_package_revision_ref: &installation.package_revision_ref,
                lock_digest: lock.lock_digest(),
                live_lock_digest: lock.lock_digest(),
                grant_revision,
                live_grant_revision: grant.revision,
                permitted_tools: &permitted,
            },
        ) {
            Ok(catalog) => catalog,
            Err(_) => continue,
        };
        let mut documents = Vec::new();
        for capability in &catalog.capabilities {
            let row: Option<(i64, Vec<u8>)> = connection
                .query_row(
                    "SELECT byte_count, skill_document FROM app_skill_revision_blobs
                      WHERE content_digest = ?1",
                    params![capability.content_digest.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((byte_count, skill_document)) = row else {
                continue;
            };
            if usize::try_from(byte_count).ok() != Some(skill_document.len())
                || AppDigest::blake3(&skill_document) != capability.content_digest
            {
                continue;
            }
            documents.push(AppComputedCapabilityDocument {
                dependency_ref: capability.dependency_ref.clone(),
                description: capability.dependency_ref.as_str().to_owned(),
                skill_document,
            });
        }
        catalogs.push((catalog, documents));
    }
    Ok(compile_scope_computed_capability_overlay(&catalogs))
}

pub fn canonical_package_revision_ref(
    package_revision: &AppPackageRevision,
) -> Result<AppReference, AppRegistryError> {
    package_revision.validate_app_contract(&AppContractLimits::default())?;
    canonical_package_revision_ref_from_identity(
        &package_revision.package_id,
        &package_revision.semantic_version,
        &package_revision.content_digest,
        &package_revision.dependency_lock_digest,
    )
}

/// Derive the same bounded package-revision identity before a full registry
/// record exists. Dependency-lock identity is load-bearing: changing an exact
/// standalone revision must create a distinct reviewable package revision even
/// when the package bundle and semantic version are unchanged. Callers must
/// already hold strict validated package identity; this helper creates no
/// registry row and grants no authority.
pub fn canonical_package_revision_ref_from_identity(
    package_id: &AppReference,
    semantic_version: &str,
    content_digest: &AppDigest,
    dependency_lock_digest: &AppDigest,
) -> Result<AppReference, AppRegistryError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        package_id: &'a AppReference,
        semantic_version: &'a str,
        content_digest: &'a AppDigest,
        dependency_lock_digest: &'a AppDigest,
    }

    let identity = Identity {
        package_id,
        semantic_version,
        content_digest,
        dependency_lock_digest,
    };
    let bytes = encode_bounded_json(&identity, &AppContractLimits::default())?;
    let digest = AppDigest::blake3(&bytes);
    AppReference::parse(format!(
        "package-revision:{}",
        digest
            .as_str()
            .strip_prefix("blake3:")
            .expect("AppDigest::blake3 always emits its canonical prefix")
    ))
    .map_err(AppRegistryError::from)
}

fn canonical_skill_revision_ref(
    dependency_ref: &AppReference,
    semantic_version: &str,
    revision: AppRevision,
    content_digest: &AppDigest,
) -> Result<AppReference, AppRegistryError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        dependency_ref: &'a AppReference,
        semantic_version: &'a str,
        revision: AppRevision,
        content_digest: &'a AppDigest,
    }

    let bytes = encode_bounded_json(
        &Identity {
            dependency_ref,
            semantic_version,
            revision,
            content_digest,
        },
        &AppContractLimits::default(),
    )?;
    let digest = AppDigest::blake3(&bytes);
    let suffix = digest.as_str().strip_prefix("blake3:").ok_or_else(|| {
        AppRegistryError::InvalidControlPlane(
            "canonical procedure digest has an invalid prefix".to_owned(),
        )
    })?;
    AppReference::parse(format!("skill-revision:{suffix}")).map_err(AppRegistryError::from)
}

fn publish_ready_for_review_blocking(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    publication: AppReadyForReviewPublication,
) -> Result<AppRegistryPublicationReceipt, AppRegistryError> {
    verify_staged_package_snapshot(workspace, scope, &publication.staged_candidate)
        .map_err(map_package_staging_error)?;
    let mut connection = open_scoped_registry_for_write(workspace, scope)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let limits = AppContractLimits::default();
    let package_json = encode_bounded_json(&publication.package_revision, &limits)?;
    let attempt_json = encode_bounded_json(&publication.attempt, &limits)?;
    let installation_json = encode_bounded_json(&publication.installation, &limits)?;
    let directory_metadata = package_directory_metadata(
        &publication.package_revision_ref,
        &publication.staged_candidate,
        publication.package_revision.created_at,
    )?;
    let directory_metadata_json = encode_bounded_json(&directory_metadata, &limits)?;

    let package_inserted = transaction.execute(
        "INSERT INTO app_package_revisions (
             package_revision_ref, package_id, semantic_version, content_digest,
             publisher_identity, dependency_lock_digest, record_json,
             dependency_lock_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(package_revision_ref) DO NOTHING",
        params![
            publication.package_revision_ref.as_str(),
            publication.package_revision.package_id.as_str(),
            publication.package_revision.semantic_version,
            publication.package_revision.content_digest.as_str(),
            publication.package_revision.publisher_identity.as_str(),
            publication.package_revision.dependency_lock_digest.as_str(),
            package_json,
            publication.dependency_lock_json,
            format_timestamp(&publication.package_revision.created_at),
        ],
    )?;
    verify_exact_payload(
        &transaction,
        "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
        publication.package_revision_ref.as_str(),
        &package_json,
        "package revision",
    )?;
    publish_package_directory_metadata(
        &transaction,
        &directory_metadata,
        &directory_metadata_json,
    )?;

    let attempt_inserted = transaction.execute(
        "INSERT INTO app_lifecycle_attempts (
             attempt_id, kind, installation_id, candidate_package_revision_ref,
             state, record_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(attempt_id) DO NOTHING",
        params![
            publication.attempt.attempt_id.as_str(),
            enum_json_label(&publication.attempt.kind)?,
            publication.installation.installation_id.as_str(),
            publication.package_revision_ref.as_str(),
            enum_json_label(&publication.attempt.state)?,
            attempt_json,
            format_timestamp(&publication.attempt.created_at),
            format_timestamp(&publication.attempt.updated_at),
        ],
    )?;
    verify_exact_payload(
        &transaction,
        "SELECT record_json FROM app_lifecycle_attempts WHERE attempt_id = ?1",
        publication.attempt.attempt_id.as_str(),
        &attempt_json,
        "lifecycle attempt",
    )?;
    let linked_installation: Option<String> = transaction
        .query_row(
            "SELECT installation_id FROM app_lifecycle_attempts WHERE attempt_id = ?1",
            params![publication.attempt.attempt_id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    if linked_installation.as_deref() != Some(publication.installation.installation_id.as_str()) {
        return Err(AppRegistryError::IdentityConflict {
            entity: "lifecycle attempt installation link",
            identity: publication.attempt.attempt_id.to_string(),
        });
    }

    let installation_inserted = transaction.execute(
        "INSERT INTO app_installations (
             installation_id, principal, workspace, package_revision_ref,
             lifecycle_status, lifecycle_generation, record_json, created_at,
             updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(installation_id) DO NOTHING",
        params![
            publication.installation.installation_id.as_str(),
            scope.principal.as_str(),
            scope.workspace.as_str(),
            publication.package_revision_ref.as_str(),
            enum_json_label(&publication.installation.lifecycle.status)?,
            i64::try_from(publication.installation.lifecycle.generation).map_err(|_| {
                AppRegistryError::InvalidPublication(
                    "installation generation exceeds SQLite integer range",
                )
            })?,
            installation_json,
            format_timestamp(&publication.installation.created_at),
            format_timestamp(&publication.installation.updated_at),
        ],
    )?;
    verify_exact_payload(
        &transaction,
        "SELECT record_json FROM app_installations WHERE installation_id = ?1",
        publication.installation.installation_id.as_str(),
        &installation_json,
        "installation",
    )?;

    transaction.commit()?;
    Ok(AppRegistryPublicationReceipt {
        package_revision_ref: publication.package_revision_ref,
        attempt_id: publication.attempt.attempt_id,
        installation_id: publication.installation.installation_id,
        outcome: if package_inserted + attempt_inserted + installation_inserted == 0 {
            AppRegistryPublicationOutcome::AlreadyPresent
        } else {
            AppRegistryPublicationOutcome::Created
        },
    })
}

fn publish_reviewable_revision_blocking(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
    publication: AppReviewableRevisionPublication,
) -> Result<AppRegistryPublicationReceipt, AppRegistryError> {
    verify_staged_package_snapshot(workspace, scope, &publication.staged_candidate)
        .map_err(map_package_staging_error)?;
    let mut connection = open_scoped_registry_for_write(workspace, scope)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let installation_id = publication
        .attempt
        .installation_id
        .as_ref()
        .expect("validated update/reinstall installation")
        .clone();
    let installation_json: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let installation: AppInstallation = decode_app_contract(
        installation_json
            .as_deref()
            .ok_or_else(|| AppRegistryError::MissingRecord {
                entity: "installation",
                identity: installation_id.to_string(),
            })?,
        &AppContractLimits::default(),
    )?;
    if &installation.scope != scope {
        return Err(AppRegistryError::ScopeCollision);
    }
    let current_source_fence = reviewable_revision_source_fence_blocking(
        &transaction,
        scope,
        &installation_id,
        publication.attempt.kind,
    )?;
    if current_source_fence != publication.source_fence {
        return Err(AppRegistryError::StateConflict(
            "update/reinstall source package, lock, authority, schema, surface or data tail changed"
                .to_owned(),
        ));
    }
    let source_generation = publication
        .attempt
        .source_installation_generation
        .expect("validated update/reinstall generation");
    let generation_and_state_match = match publication.attempt.kind {
        AppLifecycleAttemptKind::Update => {
            source_generation.checked_add(1) == Some(installation.lifecycle.generation)
                && installation.lifecycle.status == AppInstallationStatus::UpdatePending
        },
        AppLifecycleAttemptKind::Reinstall => {
            source_generation == installation.lifecycle.generation
                && installation.lifecycle.status == AppInstallationStatus::UninstalledRetained
        },
        AppLifecycleAttemptKind::InitialInstall => false,
    };
    if !generation_and_state_match {
        return Err(AppRegistryError::GenerationConflict {
            expected: source_generation,
            actual: installation.lifecycle.generation,
        });
    }

    let limits = AppContractLimits::default();
    let package_json = encode_bounded_json(&publication.package_revision, &limits)?;
    let attempt_json = encode_bounded_json(&publication.attempt, &limits)?;
    let directory_metadata = package_directory_metadata(
        &publication.package_revision_ref,
        &publication.staged_candidate,
        publication.package_revision.created_at,
    )?;
    let directory_metadata_json = encode_bounded_json(&directory_metadata, &limits)?;
    let package_inserted = transaction.execute(
        "INSERT INTO app_package_revisions (
             package_revision_ref, package_id, semantic_version, content_digest,
             publisher_identity, dependency_lock_digest, record_json,
             dependency_lock_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(package_revision_ref) DO NOTHING",
        params![
            publication.package_revision_ref.as_str(),
            publication.package_revision.package_id.as_str(),
            publication.package_revision.semantic_version,
            publication.package_revision.content_digest.as_str(),
            publication.package_revision.publisher_identity.as_str(),
            publication.package_revision.dependency_lock_digest.as_str(),
            package_json,
            publication.dependency_lock_json,
            format_timestamp(&publication.package_revision.created_at),
        ],
    )?;
    verify_exact_payload(
        &transaction,
        "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
        publication.package_revision_ref.as_str(),
        &package_json,
        "package revision",
    )?;
    publish_package_directory_metadata(
        &transaction,
        &directory_metadata,
        &directory_metadata_json,
    )?;
    let attempt_inserted = transaction.execute(
        "INSERT INTO app_lifecycle_attempts (
             attempt_id, kind, installation_id, candidate_package_revision_ref,
             state, record_json, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(attempt_id) DO NOTHING",
        params![
            publication.attempt.attempt_id.as_str(),
            enum_json_label(&publication.attempt.kind)?,
            installation_id.as_str(),
            publication.package_revision_ref.as_str(),
            enum_json_label(&publication.attempt.state)?,
            attempt_json,
            format_timestamp(&publication.attempt.created_at),
            format_timestamp(&publication.attempt.updated_at),
        ],
    )?;
    verify_exact_payload(
        &transaction,
        "SELECT record_json FROM app_lifecycle_attempts WHERE attempt_id = ?1",
        publication.attempt.attempt_id.as_str(),
        &attempt_json,
        "lifecycle attempt",
    )?;
    let linked_installation: Option<String> = transaction
        .query_row(
            "SELECT installation_id FROM app_lifecycle_attempts WHERE attempt_id = ?1",
            params![publication.attempt.attempt_id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    if linked_installation.as_deref() != Some(installation_id.as_str()) {
        return Err(AppRegistryError::IdentityConflict {
            entity: "lifecycle attempt installation link",
            identity: publication.attempt.attempt_id.to_string(),
        });
    }
    transaction.commit()?;
    Ok(AppRegistryPublicationReceipt {
        package_revision_ref: publication.package_revision_ref,
        attempt_id: publication.attempt.attempt_id,
        installation_id,
        outcome: if package_inserted + attempt_inserted == 0 {
            AppRegistryPublicationOutcome::AlreadyPresent
        } else {
            AppRegistryPublicationOutcome::Created
        },
    })
}

pub fn reviewable_revision_source_fence_blocking(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    attempt_kind: AppLifecycleAttemptKind,
) -> Result<AppReviewableRevisionSourceFence, AppRegistryError> {
    let installation_bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppRegistryError::MissingRecord {
            entity: "installation",
            identity: installation_id.to_string(),
        })?;
    let installation: AppInstallation =
        decode_app_contract(&installation_bytes, &AppContractLimits::default())?;
    if installation.scope != *scope || installation.installation_id != *installation_id {
        return Err(AppRegistryError::ScopeCollision);
    }
    let source_installation_generation = match attempt_kind {
        AppLifecycleAttemptKind::Update
            if installation.lifecycle.status == AppInstallationStatus::UpdatePending =>
        {
            installation
                .lifecycle
                .generation
                .checked_sub(1)
                .filter(|generation| *generation > 0)
                .ok_or_else(|| {
                    AppRegistryError::StateConflict(
                        "parked update has no valid source generation".to_owned(),
                    )
                })?
        },
        AppLifecycleAttemptKind::Reinstall
            if installation.lifecycle.status == AppInstallationStatus::UninstalledRetained =>
        {
            installation.lifecycle.generation
        },
        AppLifecycleAttemptKind::InitialInstall => {
            return Err(AppRegistryError::InvalidPublication(
                "initial install cannot capture an update/reinstall source fence",
            ));
        },
        _ => {
            return Err(AppRegistryError::StateConflict(
                "installation is not parked for the requested update/reinstall kind".to_owned(),
            ));
        },
    };
    let source_grant_revision = installation.grant_revision.ok_or_else(|| {
        AppRegistryError::StateConflict(
            "update/reinstall source has no retained grant revision".to_owned(),
        )
    })?;
    let source_schema_revision = installation.active_schema_revision.ok_or_else(|| {
        AppRegistryError::StateConflict(
            "update/reinstall source has no retained schema revision".to_owned(),
        )
    })?;
    let source_surface_revision = installation.active_surface_revision.ok_or_else(|| {
        AppRegistryError::StateConflict(
            "update/reinstall source has no retained surface revision".to_owned(),
        )
    })?;
    let source_dependency_lock_digest: String = connection
        .query_row(
            "SELECT dependency_lock_digest FROM app_package_revisions
             WHERE package_revision_ref = ?1",
            params![installation.package_revision_ref.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppRegistryError::MissingRecord {
            entity: "source package revision",
            identity: installation.package_revision_ref.to_string(),
        })?;
    let source_change_seq_high_water: i64 = connection.query_row(
        "SELECT COALESCE(MAX(change_seq), 0) FROM app_record_heads
         WHERE installation_id = ?1",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    let source_change_seq_high_water =
        u64::try_from(source_change_seq_high_water).map_err(|_| {
            AppRegistryError::StateConflict("negative app record change high-water".to_owned())
        })?;
    AppReviewableRevisionSourceFence {
        installation_id: installation_id.clone(),
        attempt_kind,
        parked_lifecycle_generation: installation.lifecycle.generation,
        source_installation_generation,
        source_package_revision_ref: installation.package_revision_ref,
        source_dependency_lock_digest: AppDigest::parse(source_dependency_lock_digest)?,
        source_grant_revision,
        source_schema_revision,
        source_surface_revision,
        source_change_seq_high_water,
        fence_digest: AppDigest::blake3(b"pending-reviewable-revision-source-fence"),
    }
    .seal()
}

fn package_directory_metadata(
    package_revision_ref: &AppReference,
    candidate: &super::manifest::AppPackageCandidate,
    created_at: DateTime<Utc>,
) -> Result<AppPackageDirectoryMetadata, AppRegistryError> {
    let manifest = candidate.manifest().manifest();
    let metadata = AppPackageDirectoryMetadata {
        package_revision_ref: package_revision_ref.clone(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        actions: manifest.app.actions.keys().cloned().collect(),
        custom_surface_entry_count: manifest
            .app
            .custom_surface
            .as_ref()
            .map_or(0, |declaration| declaration.entry_points().len()),
        // Empty for everything but a host-seeded system package: the manifest
        // validator refuses navigation on a non-system distribution, and only
        // digest-pinned boot admission admits a system distribution at all.
        navigation: manifest.app.navigation.clone(),
        manifest_digest: candidate.manifest().manifest_digest().clone(),
        created_at,
    };
    metadata.validate_app_contract(&AppContractLimits::default())?;
    Ok(metadata)
}

fn publish_package_directory_metadata(
    transaction: &Transaction<'_>,
    metadata: &AppPackageDirectoryMetadata,
    metadata_json: &[u8],
) -> Result<(), AppRegistryError> {
    transaction.execute(
        "INSERT INTO app_package_directory_metadata (
             package_revision_ref, name, description, metadata_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(package_revision_ref) DO NOTHING",
        params![
            metadata.package_revision_ref.as_str(),
            metadata.name.as_str(),
            metadata.description,
            metadata_json,
            format_timestamp(&metadata.created_at),
        ],
    )?;
    let durable: Option<(String, String, Vec<u8>)> = transaction
        .query_row(
            "SELECT name, description, metadata_json
               FROM app_package_directory_metadata
              WHERE package_revision_ref = ?1",
            params![metadata.package_revision_ref.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if durable.as_ref().is_none_or(|(name, description, bytes)| {
        name != metadata.name.as_str()
            || description != &metadata.description
            || bytes.as_slice() != metadata_json
    }) {
        return Err(AppRegistryError::IdentityConflict {
            entity: "package directory metadata",
            identity: metadata.package_revision_ref.to_string(),
        });
    }
    for action in &metadata.actions {
        transaction.execute(
            "INSERT INTO app_package_directory_actions (package_revision_ref, action_id)
             VALUES (?1, ?2)
             ON CONFLICT(package_revision_ref, action_id) DO NOTHING",
            params![metadata.package_revision_ref.as_str(), action.as_str()],
        )?;
    }
    let mut statement = transaction.prepare(
        "SELECT action_id FROM app_package_directory_actions
          WHERE package_revision_ref = ?1 ORDER BY action_id ASC",
    )?;
    let durable_actions = statement
        .query_map(params![metadata.package_revision_ref.as_str()], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if durable_actions
        != metadata
            .actions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    {
        return Err(AppRegistryError::IdentityConflict {
            entity: "package directory actions",
            identity: metadata.package_revision_ref.to_string(),
        });
    }
    Ok(())
}

fn map_package_staging_error(error: AppPackageStagingError) -> AppRegistryError {
    match error {
        AppPackageStagingError::ScopeCollision => AppRegistryError::ScopeCollision,
        AppPackageStagingError::CommitStateUnknown => {
            AppRegistryError::ScopeBindingCommitStateUnknown
        },
        path_error @ (AppPackageStagingError::UnsafeDestination(_)
        | AppPackageStagingError::DestinationChanged
        | AppPackageStagingError::Io(_)) => AppRegistryError::UnsafePath(path_error.to_string()),
        error => AppRegistryError::StagedPackageInvalid(error.to_string()),
    }
}

fn validate_workflow_control_identity(
    task_id: &str,
    execution_id: &str,
    sealed_blob: &[u8],
) -> Result<(), AppRegistryError> {
    if task_id.is_empty()
        || execution_id.is_empty()
        || task_id.len() > 256
        || execution_id.len() > 256
        || !task_id.is_ascii()
        || !execution_id.is_ascii()
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "protected workflow control identity is invalid".to_owned(),
        ));
    }
    if sealed_blob.is_empty() || sealed_blob.len() > MAX_WORKFLOW_CONTROL_BLOB_BYTES {
        return Err(AppRegistryError::InvalidControlPlane(
            "protected workflow control blob exceeds its bounded size".to_owned(),
        ));
    }
    Ok(())
}

fn load_workflow_control_record(
    connection: &Connection,
    task_id: &str,
    execution_id: &str,
    kind: AppWorkflowControlKind,
) -> Result<Option<AppWorkflowControlRecord>, AppRegistryError> {
    let row: Option<(
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        String,
        i64,
        i64,
    )> = connection
        .query_row(
            "SELECT h.generation, h.revision, h.lifecycle_state, h.claim_ref,
                    h.claim_expires_at, h.content_digest, b.byte_count,
                    length(b.sealed_blob)
               FROM app_workflow_control_heads h
               JOIN app_workflow_control_blobs b
                 ON b.task_id = h.task_id
                AND b.execution_id = h.execution_id
                AND b.control_kind = h.control_kind
                AND b.generation = h.generation
                AND b.content_digest = h.content_digest
              WHERE h.task_id = ?1 AND h.execution_id = ?2 AND h.control_kind = ?3",
            params![task_id, execution_id, kind.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        generation,
        revision,
        lifecycle,
        claim_ref,
        claim_expires_at,
        digest,
        byte_count,
        blob_length,
    )) = row
    else {
        return Ok(None);
    };
    let generation = u64::try_from(generation).map_err(|_| {
        AppRegistryError::StateConflict("negative workflow-control generation".to_owned())
    })?;
    let revision = u64::try_from(revision).map_err(|_| {
        AppRegistryError::StateConflict("negative workflow-control revision".to_owned())
    })?;
    let byte_count = usize::try_from(byte_count).map_err(|_| {
        AppRegistryError::StateConflict("negative workflow-control byte count".to_owned())
    })?;
    let blob_length = usize::try_from(blob_length).map_err(|_| {
        AppRegistryError::StateConflict("negative workflow-control blob length".to_owned())
    })?;
    if generation == 0
        || revision == 0
        || byte_count != blob_length
        || blob_length == 0
        || blob_length > MAX_WORKFLOW_CONTROL_BLOB_BYTES
    {
        return Err(AppRegistryError::StateConflict(
            "protected workflow control metadata does not match its immutable blob".to_owned(),
        ));
    }
    let content_digest = AppDigest::parse(digest)?;
    let sealed_blob: Vec<u8> = connection.query_row(
        "SELECT sealed_blob
           FROM app_workflow_control_blobs
          WHERE task_id = ?1 AND execution_id = ?2 AND control_kind = ?3
            AND generation = ?4 AND content_digest = ?5
            AND byte_count = ?6 AND length(sealed_blob) = ?6",
        params![
            task_id,
            execution_id,
            kind.as_str(),
            i64::try_from(generation).unwrap_or(i64::MAX),
            content_digest.as_str(),
            i64::try_from(blob_length).unwrap_or(i64::MAX),
        ],
        |row| row.get(0),
    )?;
    if sealed_blob.len() != blob_length {
        return Err(AppRegistryError::StateConflict(
            "protected workflow control blob changed during bounded read".to_owned(),
        ));
    }
    if content_digest != AppDigest::blake3(&sealed_blob) {
        return Err(AppRegistryError::StateConflict(
            "protected workflow control blob digest mismatch".to_owned(),
        ));
    }
    let lifecycle = AppWorkflowControlLifecycle::parse(&lifecycle)?;
    let claim_ref = claim_ref.map(AppReference::parse).transpose()?;
    let claim_expires_at = claim_expires_at
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|_| {
                    AppRegistryError::StateConflict(
                        "protected workflow control claim expiry is invalid".to_owned(),
                    )
                })
        })
        .transpose()?;
    if (lifecycle == AppWorkflowControlLifecycle::Claimed)
        != (claim_ref.is_some() && claim_expires_at.is_some())
        || (lifecycle != AppWorkflowControlLifecycle::Claimed
            && (claim_ref.is_some() || claim_expires_at.is_some()))
    {
        return Err(AppRegistryError::StateConflict(
            "protected workflow control claim state is incoherent".to_owned(),
        ));
    }
    Ok(Some(AppWorkflowControlRecord {
        generation,
        revision,
        lifecycle,
        claim_ref,
        claim_expires_at,
        proposal_ref: None,
        content_digest,
        sealed_blob,
    }))
}

fn load_prepared_workflow_pause_record(
    connection: &Connection,
    task_id: &str,
    execution_id: &str,
) -> Result<Option<AppWorkflowControlRecord>, AppRegistryError> {
    let row: Option<(i64, String, i64, String, i64, i64)> = connection
        .query_row(
            "SELECT p.generation, p.content_digest, p.base_revision, p.proposal_ref,
                    b.byte_count, length(b.sealed_blob)
               FROM app_workflow_control_prepared p
               JOIN app_workflow_control_blobs b
                 ON b.task_id = p.task_id
                AND b.execution_id = p.execution_id
                AND b.control_kind = p.control_kind
                AND b.generation = p.generation
                AND b.content_digest = p.content_digest
              WHERE p.task_id = ?1 AND p.execution_id = ?2
                AND p.control_kind = 'pause'",
            params![task_id, execution_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((generation, digest, base_revision, proposal_ref, byte_count, blob_length)) = row
    else {
        return Ok(None);
    };
    let generation = u64::try_from(generation).map_err(|_| {
        AppRegistryError::StateConflict("negative prepared pause generation".to_owned())
    })?;
    let base_revision = u64::try_from(base_revision).map_err(|_| {
        AppRegistryError::StateConflict("negative prepared pause base revision".to_owned())
    })?;
    let revision = base_revision.checked_add(1).ok_or_else(|| {
        AppRegistryError::StateConflict("prepared pause revision overflow".to_owned())
    })?;
    let byte_count = usize::try_from(byte_count).map_err(|_| {
        AppRegistryError::StateConflict("negative prepared pause byte count".to_owned())
    })?;
    let blob_length = usize::try_from(blob_length).map_err(|_| {
        AppRegistryError::StateConflict("negative prepared pause blob length".to_owned())
    })?;
    if generation == 0
        || byte_count != blob_length
        || blob_length == 0
        || blob_length > MAX_WORKFLOW_CONTROL_BLOB_BYTES
    {
        return Err(AppRegistryError::StateConflict(
            "prepared pause metadata exceeds its bounded immutable blob".to_owned(),
        ));
    }
    let content_digest = AppDigest::parse(digest)?;
    let proposal_ref = AppReference::parse(proposal_ref)?;
    let sealed_blob: Vec<u8> = connection.query_row(
        "SELECT sealed_blob
           FROM app_workflow_control_blobs
          WHERE task_id = ?1 AND execution_id = ?2 AND control_kind = 'pause'
            AND generation = ?3 AND content_digest = ?4
            AND byte_count = ?5 AND length(sealed_blob) = ?5",
        params![
            task_id,
            execution_id,
            i64::try_from(generation).unwrap_or(i64::MAX),
            content_digest.as_str(),
            i64::try_from(blob_length).unwrap_or(i64::MAX),
        ],
        |row| row.get(0),
    )?;
    if sealed_blob.len() != blob_length || AppDigest::blake3(&sealed_blob) != content_digest {
        return Err(AppRegistryError::StateConflict(
            "prepared pause immutable blob digest mismatch".to_owned(),
        ));
    }
    Ok(Some(AppWorkflowControlRecord {
        generation,
        revision,
        lifecycle: AppWorkflowControlLifecycle::Prepared,
        claim_ref: None,
        claim_expires_at: None,
        proposal_ref: Some(proposal_ref),
        content_digest,
        sealed_blob,
    }))
}

fn prune_superseded_workflow_control_blobs(
    connection: &Connection,
    task_id: &str,
    execution_id: &str,
    kind: AppWorkflowControlKind,
) -> Result<(), AppRegistryError> {
    connection.execute(
        "DELETE FROM app_workflow_control_blobs
          WHERE task_id = ?1 AND execution_id = ?2 AND control_kind = ?3
            AND NOT EXISTS (
                SELECT 1 FROM app_workflow_control_heads h
                 WHERE h.task_id = app_workflow_control_blobs.task_id
                   AND h.execution_id = app_workflow_control_blobs.execution_id
                   AND h.control_kind = app_workflow_control_blobs.control_kind
                   AND h.generation = app_workflow_control_blobs.generation
                   AND h.content_digest = app_workflow_control_blobs.content_digest
            )
            AND NOT EXISTS (
                SELECT 1 FROM app_workflow_control_prepared p
                 WHERE p.task_id = app_workflow_control_blobs.task_id
                   AND p.execution_id = app_workflow_control_blobs.execution_id
                   AND p.control_kind = app_workflow_control_blobs.control_kind
                   AND p.generation = app_workflow_control_blobs.generation
                   AND p.content_digest = app_workflow_control_blobs.content_digest
            )",
        params![task_id, execution_id, kind.as_str()],
    )?;
    Ok(())
}

fn publish_workflow_control_generation_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    task_id: &str,
    execution_id: &str,
    kind: AppWorkflowControlKind,
    sealed_blob: Vec<u8>,
    terminal_contributions: &[AppPreparedTerminalContribution],
    expected_pause_claim_ref: Option<&AppReference>,
    pause_proposal_ref: Option<&AppReference>,
    expected_current_content_digest: Option<Option<AppDigest>>,
    expected_run_state_content_digest: Option<AppDigest>,
    now: DateTime<Utc>,
) -> Result<AppWorkflowControlRecord, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = load_workflow_control_record(&transaction, task_id, execution_id, kind)?;
    if let Some(expected) = expected_current_content_digest.as_ref() {
        let current_digest = current.as_ref().map(|record| record.content_digest.clone());
        if &current_digest != expected {
            return Err(AppRegistryError::CompareAndSwapLost(
                "protected workflow control head",
            ));
        }
    }
    if let Some(expected) = expected_run_state_content_digest.as_ref() {
        if kind != AppWorkflowControlKind::InteractiveStop {
            return Err(AppRegistryError::InvalidControlPlane(
                "only interactive-stop publication may depend on a protected run-state head"
                    .to_owned(),
            ));
        }
        let run_state = load_workflow_control_record(
            &transaction,
            task_id,
            execution_id,
            AppWorkflowControlKind::RunState,
        )?;
        let run_state_matches = run_state.as_ref().is_some_and(|record| {
            record.lifecycle == AppWorkflowControlLifecycle::Active
                && &record.content_digest == expected
        });
        if !run_state_matches {
            return Err(AppRegistryError::CompareAndSwapLost(
                "protected workflow run-state head",
            ));
        }
    }
    if !terminal_contributions.is_empty() && kind != AppWorkflowControlKind::RunState {
        return Err(AppRegistryError::InvalidControlPlane(
            "contributions may accompany only a protected run-state generation".to_owned(),
        ));
    }
    if kind != AppWorkflowControlKind::Pause
        && (expected_pause_claim_ref.is_some() || pause_proposal_ref.is_some())
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "only a protected pause proposal may carry claim ownership".to_owned(),
        ));
    }
    if kind == AppWorkflowControlKind::Pause && pause_proposal_ref.is_none() {
        return Err(AppRegistryError::InvalidControlPlane(
            "protected pause publication requires a move-only proposal owner".to_owned(),
        ));
    }
    if kind == AppWorkflowControlKind::Pause
        && (current.as_ref().is_some_and(|record| {
            record.lifecycle == AppWorkflowControlLifecycle::Claimed
                && record.claim_ref.as_ref() != expected_pause_claim_ref
        }) || current.as_ref().is_some_and(|record| {
            record.lifecycle != AppWorkflowControlLifecycle::Claimed
                && expected_pause_claim_ref.is_some()
        }))
    {
        return Err(AppRegistryError::CompareAndSwapLost(
            "protected pause proposal claim owner",
        ));
    }
    let content_digest = AppDigest::blake3(&sealed_blob);
    if kind == AppWorkflowControlKind::Pause {
        if let Some(prepared) =
            load_prepared_workflow_pause_record(&transaction, task_id, execution_id)?
        {
            if prepared.content_digest == content_digest
                && prepared.proposal_ref.as_ref() == pause_proposal_ref
            {
                if prepared.sealed_blob != sealed_blob {
                    return Err(AppRegistryError::StateConflict(
                        "prepared workflow control digest collision".to_owned(),
                    ));
                }
                let (base_revision, base_lifecycle, base_claim_ref): (
                    i64,
                    Option<String>,
                    Option<String>,
                ) = transaction.query_row(
                    "SELECT base_revision, base_lifecycle, base_claim_ref
                       FROM app_workflow_control_prepared
                      WHERE task_id = ?1 AND execution_id = ?2
                        AND control_kind = 'pause' AND generation = ?3
                        AND content_digest = ?4 AND proposal_ref = ?5",
                    params![
                        task_id,
                        execution_id,
                        i64::try_from(prepared.generation).unwrap_or(i64::MAX),
                        prepared.content_digest.as_str(),
                        pause_proposal_ref.map(ToString::to_string),
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                if u64::try_from(base_revision).ok()
                    != Some(current.as_ref().map_or(0, |record| record.revision))
                    || base_lifecycle.as_deref()
                        != current.as_ref().map(|record| record.lifecycle.as_str())
                    || base_claim_ref
                        != current
                            .as_ref()
                            .and_then(|record| record.claim_ref.as_ref())
                            .map(ToString::to_string)
                {
                    return Err(AppRegistryError::CompareAndSwapLost(
                        "stale protected pause proposal base",
                    ));
                }
                prune_superseded_workflow_control_blobs(&transaction, task_id, execution_id, kind)?;
                transaction.commit()?;
                return Ok(prepared);
            }
            return Err(AppRegistryError::CompareAndSwapLost(
                "protected pause proposal already exists",
            ));
        }
    }
    if let Some(current) = current.as_ref() {
        if current.content_digest == content_digest
            && !(kind == AppWorkflowControlKind::Pause
                && current.lifecycle == AppWorkflowControlLifecycle::Claimed)
        {
            if current.sealed_blob != sealed_blob {
                return Err(AppRegistryError::StateConflict(
                    "workflow control digest collision".to_owned(),
                ));
            }
            append_terminal_contributions(&transaction, scope, terminal_contributions, &now)?;
            prune_superseded_workflow_control_blobs(&transaction, task_id, execution_id, kind)?;
            transaction.commit()?;
            return Ok(current.clone());
        }
        if kind == AppWorkflowControlKind::TaskBinding {
            return Err(AppRegistryError::StateConflict(
                "immutable workflow task binding changed".to_owned(),
            ));
        }
    }
    let highest_generation: Option<i64> = transaction.query_row(
        "SELECT MAX(generation) FROM app_workflow_control_blobs
          WHERE task_id = ?1 AND execution_id = ?2 AND control_kind = ?3",
        params![task_id, execution_id, kind.as_str()],
        |row| row.get(0),
    )?;
    let generation = highest_generation.map_or(Ok(1_u64), |value| {
        u64::try_from(value)
            .ok()
            .and_then(|generation| generation.checked_add(1))
            .ok_or_else(|| {
                AppRegistryError::StateConflict("workflow-control generation overflow".to_owned())
            })
    })?;
    let revision = current.as_ref().map_or(Ok(1_u64), |record| {
        record.revision.checked_add(1).ok_or_else(|| {
            AppRegistryError::StateConflict("workflow-control revision overflow".to_owned())
        })
    })?;
    let generation_sql = i64::try_from(generation).map_err(|_| {
        AppRegistryError::StateConflict(
            "workflow-control generation outside SQLite range".to_owned(),
        )
    })?;
    let revision_sql = i64::try_from(revision).map_err(|_| {
        AppRegistryError::StateConflict("workflow-control revision outside SQLite range".to_owned())
    })?;
    let byte_count = i64::try_from(sealed_blob.len()).map_err(|_| {
        AppRegistryError::InvalidControlPlane("workflow-control blob is too large".to_owned())
    })?;
    let timestamp = now.to_rfc3339_opts(SecondsFormat::Micros, true);
    let initial_lifecycle = AppWorkflowControlLifecycle::Active;
    transaction.execute(
        "INSERT INTO app_workflow_control_blobs
             (task_id, execution_id, control_kind, generation, content_digest,
              byte_count, sealed_blob, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            task_id,
            execution_id,
            kind.as_str(),
            generation_sql,
            content_digest.as_str(),
            byte_count,
            &sealed_blob,
            &timestamp,
        ],
    )?;
    if kind == AppWorkflowControlKind::Pause {
        let base_revision = current.as_ref().map_or(0, |record| record.revision);
        transaction.execute(
            "INSERT INTO app_workflow_control_prepared
                 (task_id, execution_id, control_kind, generation, content_digest,
                  base_revision, base_lifecycle, base_claim_ref, proposal_ref, prepared_at)
             VALUES (?1, ?2, 'pause', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                task_id,
                execution_id,
                generation_sql,
                content_digest.as_str(),
                i64::try_from(base_revision).unwrap_or(i64::MAX),
                current.as_ref().map(|record| record.lifecycle.as_str()),
                current
                    .as_ref()
                    .and_then(|record| record.claim_ref.as_ref())
                    .map(ToString::to_string),
                pause_proposal_ref.map(ToString::to_string),
                &timestamp,
            ],
        )?;
        let prepared = load_prepared_workflow_pause_record(&transaction, task_id, execution_id)?
            .ok_or_else(|| {
                AppRegistryError::StateConflict("prepared pause proposal disappeared".to_owned())
            })?;
        prune_superseded_workflow_control_blobs(&transaction, task_id, execution_id, kind)?;
        transaction.commit()?;
        return Ok(prepared);
    }
    let affected = if let Some(current) = current.as_ref() {
        transaction.execute(
            "UPDATE app_workflow_control_heads
                SET generation = ?1, content_digest = ?2, lifecycle_state = ?3,
                    claim_ref = NULL, claim_expires_at = NULL,
                    revision = ?4, updated_at = ?5
              WHERE task_id = ?6 AND execution_id = ?7 AND control_kind = ?8
                AND generation = ?9 AND content_digest = ?10 AND revision = ?11
                AND lifecycle_state = ?12",
            params![
                generation_sql,
                content_digest.as_str(),
                initial_lifecycle.as_str(),
                revision_sql,
                &timestamp,
                task_id,
                execution_id,
                kind.as_str(),
                i64::try_from(current.generation).unwrap_or(i64::MAX),
                current.content_digest.as_str(),
                i64::try_from(current.revision).unwrap_or(i64::MAX),
                current.lifecycle.as_str(),
            ],
        )?
    } else {
        transaction.execute(
            "INSERT INTO app_workflow_control_heads
                 (task_id, execution_id, control_kind, generation, content_digest,
                  lifecycle_state, claim_ref, claim_expires_at, revision, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8)",
            params![
                task_id,
                execution_id,
                kind.as_str(),
                generation_sql,
                content_digest.as_str(),
                initial_lifecycle.as_str(),
                revision_sql,
                &timestamp,
            ],
        )?
    };
    if affected != 1 {
        return Err(AppRegistryError::CompareAndSwapLost(
            "protected workflow control head",
        ));
    }
    let record = load_workflow_control_record(&transaction, task_id, execution_id, kind)?
        .ok_or_else(|| {
            AppRegistryError::StateConflict(
                "published workflow control head disappeared".to_owned(),
            )
        })?;
    append_terminal_contributions(&transaction, scope, terminal_contributions, &now)?;
    prune_superseded_workflow_control_blobs(&transaction, task_id, execution_id, kind)?;
    transaction.commit()?;
    Ok(record)
}

fn append_terminal_contributions(
    transaction: &Transaction<'_>,
    scope: &AppScope,
    contributions: &[AppPreparedTerminalContribution],
    now: &DateTime<Utc>,
) -> Result<(), AppRegistryError> {
    use magician_app_contract::contribution::AppContributionSettlementRefV1;

    for contribution in contributions {
        match contribution {
            AppPreparedTerminalContribution::Memory {
                proposal,
                frequency,
                fallback_invalidation_reason,
            } => {
                let reviewed = super::contribution_frequency::AppReviewedContributionFrequencyV1::from_reviewed_limit(
                    u32::from(frequency.max_proposals),
                    frequency.window_seconds,
                )
                .map_err(|error| AppRegistryError::InvalidControlPlane(error.to_string()))?;
                let frequency_outcome = super::contribution_frequency::consume_contribution_frequency_in_transaction(
                    transaction,
                    scope,
                    &proposal.header.scope_binding_ref,
                    &proposal.header.installation_id,
                    &proposal.header.workflow_id,
                    &proposal.header.action_id,
                    &proposal.header.contribution_port_id,
                    reviewed,
                    &proposal.header.proposal_id,
                    proposal.header.proposal_revision,
                    &proposal.proposal_digest,
                    proposal.header.issued_at_ms,
                    now,
                );
                match frequency_outcome {
                    Ok(_) => {},
                    Err(super::contribution::AppContributionError::HistoryCompacted)
                    | Err(super::contribution::AppContributionError::Quota(
                        "reviewed contribution proposals per window",
                    )) => {
                        if let (Some(reason), Some(source)) = (
                            fallback_invalidation_reason,
                            proposal.header.sources.first(),
                        ) {
                            super::memory_contribution_outbox::append_memory_source_invalidation_in_transaction(
                                transaction,
                                scope,
                                &proposal.header.scope_binding_ref,
                                &proposal.header.installation_id,
                                &proposal.header.workflow_id,
                                &proposal.header.action_id,
                                &proposal.header.contribution_port_id,
                                &source.entity_name,
                                &source.record_id,
                                source.record_revision,
                                *reason,
                                now,
                            )
                            .map_err(|error| AppRegistryError::InvalidControlPlane(format!(
                                "rate-limited memory replacement invalidation could not be published: {error}"
                            )))?;
                        }
                        continue;
                    },
                    Err(error) => {
                        return Err(AppRegistryError::InvalidControlPlane(format!(
                            "terminal memory contribution frequency could not be consumed: {error}"
                        )));
                    },
                }
                let settlement_ref = match &proposal.header.settlement {
                    AppContributionSettlementRefV1::Mutation {
                        mutation_receipt_id,
                        ..
                    } => mutation_receipt_id.as_str(),
                    AppContributionSettlementRefV1::TypedResult { result_ref, .. } => {
                        result_ref.as_str()
                    },
                };
                super::memory_contribution_outbox::append_memory_contribution_in_transaction(
                    transaction,
                    scope,
                    &proposal.header.scope_binding_ref,
                    proposal.clone(),
                    settlement_ref,
                    *fallback_invalidation_reason,
                    now,
                )
                .map_err(|error| AppRegistryError::InvalidControlPlane(format!(
                    "terminal memory contribution could not be published: {error}"
                )))?;
            },
            AppPreparedTerminalContribution::PersonalAgentRetrieval {
                proposal,
                frequency,
                fallback_invalidation_reason,
            } => {
                let reviewed = super::contribution_frequency::AppReviewedContributionFrequencyV1::from_reviewed_limit(
                    u32::from(frequency.max_proposals),
                    frequency.window_seconds,
                )
                .map_err(|error| AppRegistryError::InvalidControlPlane(error.to_string()))?;
                let frequency_outcome = super::contribution_frequency::consume_contribution_frequency_in_transaction(
                    transaction,
                    scope,
                    &proposal.header.scope_binding_ref,
                    &proposal.header.installation_id,
                    &proposal.header.workflow_id,
                    &proposal.header.action_id,
                    &proposal.header.contribution_port_id,
                    reviewed,
                    &proposal.header.proposal_id,
                    proposal.header.proposal_revision,
                    &proposal.proposal_digest,
                    proposal.header.issued_at_ms,
                    now,
                );
                match frequency_outcome {
                    Ok(_) => {},
                    Err(super::contribution::AppContributionError::HistoryCompacted)
                    | Err(super::contribution::AppContributionError::Quota(
                        "reviewed contribution proposals per window",
                    )) => {
                        if let (Some(reason), Some(source)) = (
                            fallback_invalidation_reason,
                            proposal.header.sources.first(),
                        ) {
                            super::retrieval_contribution_outbox::append_retrieval_source_invalidation_in_transaction(
                                transaction,
                                scope,
                                &proposal.header.scope_binding_ref,
                                &proposal.header.installation_id,
                                &proposal.header.workflow_id,
                                &proposal.header.action_id,
                                &proposal.header.contribution_port_id,
                                &source.entity_name,
                                &source.record_id,
                                source.record_revision,
                                *reason,
                                now,
                            )
                            .map_err(|error| AppRegistryError::InvalidControlPlane(format!(
                                "rate-limited retrieval replacement invalidation could not be published: {error}"
                            )))?;
                        }
                        continue;
                    },
                    Err(error) => {
                        return Err(AppRegistryError::InvalidControlPlane(format!(
                            "terminal retrieval contribution frequency could not be consumed: {error}"
                        )));
                    },
                }
                if let (Some(reason), Some(source)) = (
                    fallback_invalidation_reason,
                    proposal.header.sources.first(),
                ) {
                    super::retrieval_contribution_outbox::append_retrieval_source_invalidation_in_transaction(
                        transaction,
                        scope,
                        &proposal.header.scope_binding_ref,
                        &proposal.header.installation_id,
                        &proposal.header.workflow_id,
                        &proposal.header.action_id,
                        &proposal.header.contribution_port_id,
                        &source.entity_name,
                        &source.record_id,
                        source.record_revision,
                        *reason,
                        now,
                    )
                    .map_err(|error| AppRegistryError::InvalidControlPlane(format!(
                        "retrieval replacement invalidation could not be published: {error}"
                    )))?;
                }
                super::retrieval_contribution_outbox::append_retrieval_projection_in_transaction(
                    transaction,
                    scope,
                    &proposal.header.scope_binding_ref,
                    proposal.clone(),
                    reviewed,
                    now,
                )
                .map_err(|error| AppRegistryError::InvalidControlPlane(format!(
                    "terminal retrieval contribution could not be published: {error}"
                )))?;
            },
            AppPreparedTerminalContribution::SourceInvalidation {
                destination,
                installation_id,
                scope_binding_ref,
                workflow_id,
                action_id,
                port_id,
                entity_name,
                record_id,
                source_event_revision,
                reason,
            } => match destination {
                super::records::AppContributionDestinationBinding::MemoryUserKnowledge => {
                    super::memory_contribution_outbox::append_memory_source_invalidation_in_transaction(
                        transaction,
                        scope,
                        scope_binding_ref,
                        installation_id,
                        workflow_id,
                        action_id,
                        port_id,
                        entity_name,
                        record_id,
                        *source_event_revision,
                        *reason,
                        now,
                    )
                    .map_err(|error| AppRegistryError::InvalidControlPlane(format!(
                        "terminal memory source invalidation could not be published: {error}"
                    )))?;
                },
                super::records::AppContributionDestinationBinding::PersonalAssistantRetrievalNoGoal => {
                    super::retrieval_contribution_outbox::append_retrieval_source_invalidation_in_transaction(
                        transaction,
                        scope,
                        scope_binding_ref,
                        installation_id,
                        workflow_id,
                        action_id,
                        port_id,
                        entity_name,
                        record_id,
                        *source_event_revision,
                        *reason,
                        now,
                    )
                    .map_err(|error| AppRegistryError::InvalidControlPlane(format!(
                        "terminal retrieval source invalidation could not be published: {error}"
                    )))?;
                },
            },
        }
    }
    Ok(())
}

fn activate_prepared_workflow_pause_blocking(
    connection: &mut Connection,
    task_id: &str,
    execution_id: &str,
    expected_generation: u64,
    expected_digest: &AppDigest,
    expected_proposal_ref: &AppReference,
    now: DateTime<Utc>,
) -> Result<AppWorkflowControlRecord, AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prepared = load_prepared_workflow_pause_record(&transaction, task_id, execution_id)?
        .ok_or(AppRegistryError::MissingRecord {
            entity: "prepared protected workflow pause",
            identity: format!("{task_id}:{execution_id}"),
        })?;
    if prepared.generation != expected_generation
        || &prepared.content_digest != expected_digest
        || prepared.proposal_ref.as_ref() != Some(expected_proposal_ref)
        || prepared.lifecycle != AppWorkflowControlLifecycle::Prepared
    {
        return Err(AppRegistryError::CompareAndSwapLost(
            "prepared protected workflow pause",
        ));
    }
    let current = load_workflow_control_record(
        &transaction,
        task_id,
        execution_id,
        AppWorkflowControlKind::Pause,
    )?;
    let base_revision = prepared.revision.checked_sub(1).ok_or_else(|| {
        AppRegistryError::StateConflict("prepared pause base revision underflow".to_owned())
    })?;
    if current.as_ref().map_or(0, |record| record.revision) != base_revision {
        return Err(AppRegistryError::CompareAndSwapLost(
            "prepared protected workflow pause base",
        ));
    }
    let (base_lifecycle, base_claim_ref): (Option<String>, Option<String>) = transaction
        .query_row(
            "SELECT base_lifecycle, base_claim_ref
               FROM app_workflow_control_prepared
              WHERE task_id = ?1 AND execution_id = ?2 AND control_kind = 'pause'
                AND generation = ?3 AND content_digest = ?4 AND base_revision = ?5
                AND proposal_ref = ?6",
            params![
                task_id,
                execution_id,
                i64::try_from(prepared.generation).unwrap_or(i64::MAX),
                prepared.content_digest.as_str(),
                i64::try_from(base_revision).unwrap_or(i64::MAX),
                expected_proposal_ref.as_str(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    if current.as_ref().map(|record| record.lifecycle.as_str()) != base_lifecycle.as_deref()
        || current
            .as_ref()
            .and_then(|record| record.claim_ref.as_ref())
            .map(ToString::to_string)
            != base_claim_ref
    {
        return Err(AppRegistryError::CompareAndSwapLost(
            "prepared protected workflow pause owner",
        ));
    }
    let timestamp = now.to_rfc3339_opts(SecondsFormat::Micros, true);
    let affected = if let Some(current) = current.as_ref() {
        transaction.execute(
            "UPDATE app_workflow_control_heads
                SET generation = ?1, content_digest = ?2, lifecycle_state = 'active',
                    claim_ref = NULL, claim_expires_at = NULL,
                    revision = ?3, updated_at = ?4
              WHERE task_id = ?5 AND execution_id = ?6 AND control_kind = 'pause'
                AND generation = ?7 AND content_digest = ?8 AND revision = ?9
                AND lifecycle_state = ?10
                AND ((claim_ref IS NULL AND ?11 IS NULL) OR claim_ref = ?11)",
            params![
                i64::try_from(prepared.generation).unwrap_or(i64::MAX),
                prepared.content_digest.as_str(),
                i64::try_from(prepared.revision).unwrap_or(i64::MAX),
                &timestamp,
                task_id,
                execution_id,
                i64::try_from(current.generation).unwrap_or(i64::MAX),
                current.content_digest.as_str(),
                i64::try_from(current.revision).unwrap_or(i64::MAX),
                current.lifecycle.as_str(),
                current.claim_ref.as_ref().map(ToString::to_string),
            ],
        )?
    } else {
        transaction.execute(
            "INSERT INTO app_workflow_control_heads
                 (task_id, execution_id, control_kind, generation, content_digest,
                  lifecycle_state, claim_ref, claim_expires_at, revision, updated_at)
             VALUES (?1, ?2, 'pause', ?3, ?4, 'active', NULL, NULL, ?5, ?6)",
            params![
                task_id,
                execution_id,
                i64::try_from(prepared.generation).unwrap_or(i64::MAX),
                prepared.content_digest.as_str(),
                i64::try_from(prepared.revision).unwrap_or(i64::MAX),
                &timestamp,
            ],
        )?
    };
    if affected != 1 {
        return Err(AppRegistryError::CompareAndSwapLost(
            "prepared protected workflow pause activation",
        ));
    }
    let removed = transaction.execute(
        "DELETE FROM app_workflow_control_prepared
          WHERE task_id = ?1 AND execution_id = ?2 AND control_kind = 'pause'
            AND generation = ?3 AND content_digest = ?4 AND base_revision = ?5
            AND proposal_ref = ?6",
        params![
            task_id,
            execution_id,
            i64::try_from(prepared.generation).unwrap_or(i64::MAX),
            prepared.content_digest.as_str(),
            i64::try_from(base_revision).unwrap_or(i64::MAX),
            expected_proposal_ref.as_str(),
        ],
    )?;
    if removed != 1 {
        return Err(AppRegistryError::CompareAndSwapLost(
            "prepared protected workflow pause retirement",
        ));
    }
    let active = load_workflow_control_record(
        &transaction,
        task_id,
        execution_id,
        AppWorkflowControlKind::Pause,
    )?
    .ok_or_else(|| {
        AppRegistryError::StateConflict("activated protected pause disappeared".to_owned())
    })?;
    prune_superseded_workflow_control_blobs(
        &transaction,
        task_id,
        execution_id,
        AppWorkflowControlKind::Pause,
    )?;
    transaction.commit()?;
    Ok(active)
}

#[allow(clippy::too_many_arguments)]
fn retire_prepared_workflow_pause_blocking(
    connection: &mut Connection,
    task_id: &str,
    execution_id: &str,
    expected_generation: u64,
    expected_digest: &AppDigest,
    expected_proposal_ref: &AppReference,
    require_expired_at: Option<DateTime<Utc>>,
) -> Result<(), AppRegistryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prepared = load_prepared_workflow_pause_record(&transaction, task_id, execution_id)?
        .ok_or(AppRegistryError::MissingRecord {
            entity: "prepared protected workflow pause",
            identity: format!("{task_id}:{execution_id}"),
        })?;
    if prepared.generation != expected_generation
        || &prepared.content_digest != expected_digest
        || prepared.proposal_ref.as_ref() != Some(expected_proposal_ref)
    {
        return Err(AppRegistryError::CompareAndSwapLost(
            "prepared protected workflow pause retirement",
        ));
    }
    let expiry_cutoff = require_expired_at
        .map(|now| {
            now.checked_sub_signed(chrono::Duration::seconds(
                WORKFLOW_PAUSE_PREPARE_LEASE_SECONDS,
            ))
            .ok_or_else(|| {
                AppRegistryError::StateConflict(
                    "protected pause prepare expiry underflow".to_owned(),
                )
            })
        })
        .transpose()?
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Micros, true));
    let removed = transaction.execute(
        "DELETE FROM app_workflow_control_prepared
          WHERE task_id = ?1 AND execution_id = ?2 AND control_kind = 'pause'
            AND generation = ?3 AND content_digest = ?4 AND proposal_ref = ?5
            AND (?6 IS NULL OR prepared_at <= ?6)
            AND (
                (base_revision = 0 AND NOT EXISTS (
                    SELECT 1 FROM app_workflow_control_heads h
                     WHERE h.task_id = ?1 AND h.execution_id = ?2
                       AND h.control_kind = 'pause'
                ))
                OR EXISTS (
                    SELECT 1 FROM app_workflow_control_heads h
                     WHERE h.task_id = ?1 AND h.execution_id = ?2
                       AND h.control_kind = 'pause'
                       AND h.revision = app_workflow_control_prepared.base_revision
                       AND h.lifecycle_state = app_workflow_control_prepared.base_lifecycle
                       AND ((h.claim_ref IS NULL
                             AND app_workflow_control_prepared.base_claim_ref IS NULL)
                            OR h.claim_ref = app_workflow_control_prepared.base_claim_ref)
                )
            )",
        params![
            task_id,
            execution_id,
            i64::try_from(expected_generation).unwrap_or(i64::MAX),
            expected_digest.as_str(),
            expected_proposal_ref.as_str(),
            expiry_cutoff,
        ],
    )?;
    if removed != 1 {
        return Err(AppRegistryError::CompareAndSwapLost(
            "prepared protected workflow pause retirement",
        ));
    }
    prune_superseded_workflow_control_blobs(
        &transaction,
        task_id,
        execution_id,
        AppWorkflowControlKind::Pause,
    )?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn transition_workflow_pause_blocking(
    connection: &mut Connection,
    task_id: &str,
    execution_id: &str,
    expected_generation: u64,
    expected_digest: &AppDigest,
    expected_lifecycle: AppWorkflowControlLifecycle,
    expected_claim_ref: Option<&AppReference>,
    next_lifecycle: AppWorkflowControlLifecycle,
    next_claim_ref: Option<&AppReference>,
    now: DateTime<Utc>,
) -> Result<AppWorkflowControlRecord, AppRegistryError> {
    if (expected_lifecycle == AppWorkflowControlLifecycle::Claimed) != expected_claim_ref.is_some()
        || (next_lifecycle == AppWorkflowControlLifecycle::Claimed) != next_claim_ref.is_some()
        || !matches!(
            (expected_lifecycle, next_lifecycle),
            (
                AppWorkflowControlLifecycle::Prepared,
                AppWorkflowControlLifecycle::Active
            ) | (
                AppWorkflowControlLifecycle::Active,
                AppWorkflowControlLifecycle::Claimed
            ) | (
                AppWorkflowControlLifecycle::Claimed,
                AppWorkflowControlLifecycle::Active
            ) | (
                AppWorkflowControlLifecycle::Claimed,
                AppWorkflowControlLifecycle::Consumed
            ) | (
                AppWorkflowControlLifecycle::Claimed,
                AppWorkflowControlLifecycle::Claimed
            )
        )
    {
        return Err(AppRegistryError::InvalidControlPlane(
            "protected pause claim transition is incoherent".to_owned(),
        ));
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if load_prepared_workflow_pause_record(&transaction, task_id, execution_id)?.is_some() {
        return Err(AppRegistryError::CompareAndSwapLost(
            "protected workflow pause has an unresolved prepared proposal",
        ));
    }
    let current = load_workflow_control_record(
        &transaction,
        task_id,
        execution_id,
        AppWorkflowControlKind::Pause,
    )?
    .ok_or(AppRegistryError::MissingRecord {
        entity: "protected workflow pause",
        identity: format!("{task_id}:{execution_id}"),
    })?;
    if current.generation != expected_generation
        || &current.content_digest != expected_digest
        || current.lifecycle != expected_lifecycle
        || current.claim_ref.as_ref() != expected_claim_ref
    {
        return Err(AppRegistryError::CompareAndSwapLost(
            "protected workflow pause",
        ));
    }
    let next_revision = current.revision.checked_add(1).ok_or_else(|| {
        AppRegistryError::StateConflict("protected pause revision overflow".to_owned())
    })?;
    let next_claim_expires_at = if next_lifecycle == AppWorkflowControlLifecycle::Claimed {
        Some(
            now.checked_add_signed(chrono::Duration::seconds(
                WORKFLOW_PAUSE_CLAIM_LEASE_SECONDS,
            ))
            .ok_or_else(|| {
                AppRegistryError::StateConflict("protected pause claim expiry overflow".to_owned())
            })?,
        )
    } else {
        None
    };
    let affected = transaction.execute(
        "UPDATE app_workflow_control_heads
            SET lifecycle_state = ?1, claim_ref = ?2, claim_expires_at = ?3,
                revision = ?4, updated_at = ?5
          WHERE task_id = ?6 AND execution_id = ?7 AND control_kind = 'pause'
            AND generation = ?8 AND content_digest = ?9 AND revision = ?10
            AND lifecycle_state = ?11
            AND ((claim_ref IS NULL AND ?12 IS NULL) OR claim_ref = ?12)",
        params![
            next_lifecycle.as_str(),
            next_claim_ref.map(ToString::to_string),
            next_claim_expires_at.map(|value| value.to_rfc3339_opts(SecondsFormat::Micros, true)),
            i64::try_from(next_revision).unwrap_or(i64::MAX),
            now.to_rfc3339_opts(SecondsFormat::Micros, true),
            task_id,
            execution_id,
            i64::try_from(expected_generation).unwrap_or(i64::MAX),
            expected_digest.as_str(),
            i64::try_from(current.revision).unwrap_or(i64::MAX),
            expected_lifecycle.as_str(),
            expected_claim_ref.map(ToString::to_string),
        ],
    )?;
    if affected != 1 {
        return Err(AppRegistryError::CompareAndSwapLost(
            "protected workflow pause",
        ));
    }
    let record = load_workflow_control_record(
        &transaction,
        task_id,
        execution_id,
        AppWorkflowControlKind::Pause,
    )?
    .ok_or_else(|| {
        AppRegistryError::StateConflict("protected pause head disappeared".to_owned())
    })?;
    transaction.commit()?;
    Ok(record)
}

fn open_scoped_registry_for_write(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
) -> Result<RegistryConnectionLease, AppRegistryError> {
    let database_path =
        workspace.app_store_db_path(scope.principal.as_str(), scope.workspace.as_str());
    ensure_registry_parent_tree(workspace.base_root(), &database_path)?;
    validate_existing_registry_paths(workspace.base_root(), &database_path)?;
    ensure_app_scope_binding(workspace, scope).map_err(map_package_staging_error)?;
    validate_existing_registry_paths(workspace.base_root(), &database_path)?;
    open_registry_connection(&database_path, scope, true)
}

fn open_registry_connection(
    database_path: &Path,
    scope: &AppScope,
    writable: bool,
) -> Result<RegistryConnectionLease, AppRegistryError> {
    registry_connections().checkout(
        database_path,
        writable,
        || registry_connection_identity(database_path, scope),
        || open_registry_connection_unpooled(database_path, scope, writable),
        |connection, schema_changed| {
            verify_reused_registry_connection(connection, scope, writable, schema_changed)
        },
    )
}

fn registry_connections() -> &'static Arc<connection_pool::ConnectionPool> {
    static CONNECTIONS: OnceLock<Arc<connection_pool::ConnectionPool>> = OnceLock::new();
    CONNECTIONS.get_or_init(|| connection_pool::ConnectionPool::new(MAX_IDLE_REGISTRY_CONNECTIONS))
}

fn registry_connection_identity(
    path: &Path,
    scope: &AppScope,
) -> Result<connection_pool::ConnectionIdentity, AppRegistryError> {
    let file = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            #[cfg(unix)]
            let (device, inode) = {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            };
            #[cfg(not(unix))]
            let (device, inode) = (0, 0);
            Some(connection_pool::FileIdentity {
                device,
                inode,
                created: metadata.created().ok(),
            })
        },
        Ok(_) => {
            return Err(AppRegistryError::UnsafePath(
                "registry database is not a regular file".to_owned(),
            ))
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let keys = app_data_scope_key_candidates(scope.principal.as_str(), scope.workspace.as_str())
        .map_err(|_| AppRegistryError::AtRestEncryptionKeyUnavailable)?;
    let active = keys
        .first()
        .ok_or(AppRegistryError::AtRestEncryptionKeyUnavailable)?;
    Ok(connection_pool::ConnectionIdentity {
        scope: (scope.principal.to_string(), scope.workspace.to_string()),
        key_generation: active.key_id().to_owned(),
        file,
    })
}

fn verify_reused_registry_connection(
    connection: &Connection,
    scope: &AppScope,
    writable: bool,
    schema_changed: bool,
) -> Result<(), AppRegistryError> {
    let application_id = pragma_i32(connection, "application_id")?;
    let schema_version = pragma_i32(connection, "user_version")?;
    if application_id != APP_REGISTRY_APPLICATION_ID
        || schema_version != APP_REGISTRY_SCHEMA_VERSION
    {
        return Err(AppRegistryError::IncompatibleDatabase {
            application_id,
            schema_version,
        });
    }
    // Whole-schema shape validation belongs to connection initialization or
    // a changed SQLite schema cookie, not every record read. Scope and active
    // encryption evidence are checked on each reuse; grants remain live in
    // the registry operation's existing authorization/transaction boundary.
    if schema_changed {
        verify_current_schema_objects(connection, application_id, schema_version)?;
    }
    verify_scope_binding(connection, scope)?;
    let (format, algorithm, stored_key): (i32, String, String) = connection.query_row(
        "SELECT format_version, algorithm, key_id FROM app_data_encryption_metadata WHERE singleton=1",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let keys = app_data_scope_key_candidates(scope.principal.as_str(), scope.workspace.as_str())
        .map_err(|_| AppRegistryError::AtRestEncryptionKeyUnavailable)?;
    let key_matches = if writable {
        keys.first().is_some_and(|key| key.key_id() == stored_key)
    } else {
        keys.iter().any(|key| key.key_id() == stored_key)
    };
    if format != APP_REGISTRY_CIPHER_FORMAT_VERSION
        || algorithm != APP_REGISTRY_CIPHER_ALGORITHM
        || !key_matches
    {
        return Err(AppRegistryError::AtRestEncryptionFailed);
    }
    if connection.is_readonly(rusqlite::DatabaseName::Main)? == writable {
        return Err(AppRegistryError::StateConflict(
            "cached registry connection mode changed".to_owned(),
        ));
    }
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn open_registry_connection_unpooled(
    database_path: &Path,
    scope: &AppScope,
    writable: bool,
) -> Result<Connection, AppRegistryError> {
    let scope_keys =
        app_data_scope_key_candidates(scope.principal.as_str(), scope.workspace.as_str())
            .map_err(|_| AppRegistryError::AtRestEncryptionKeyUnavailable)?;
    let active_scope_key = scope_keys
        .first()
        .ok_or(AppRegistryError::AtRestEncryptionKeyUnavailable)?;
    if writable {
        migrate_plaintext_registry_to_sqlcipher(database_path, scope, active_scope_key)?;
    }
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } | OpenFlags::SQLITE_OPEN_FULL_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let mut opened = None;
    for (index, scope_key) in scope_keys.iter().enumerate() {
        let connection = Connection::open_with_flags(database_path, flags)?;
        if configure_registry_cipher(&connection, scope_key).is_ok() {
            opened = Some((connection, index));
            break;
        }
    }
    let (mut connection, opened_key_index) =
        opened.ok_or(AppRegistryError::AtRestEncryptionFailed)?;
    let opened_scope_key = &scope_keys[opened_key_index];
    connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "secure_delete", "ON")?;
    if writable {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "wal_autocheckpoint", 1_000_i64)?;
        initialize_or_verify_schema(
            &mut connection,
            scope,
            opened_scope_key.key_id(),
            active_scope_key.key_id(),
        )?;
        if opened_scope_key.key_id() != active_scope_key.key_id() {
            rekey_registry_to_active_generation(
                &mut connection,
                scope,
                opened_scope_key,
                active_scope_key,
            )?;
        }
        set_private_sqlite_permissions(database_path)?;
    } else {
        verify_schema_scope_and_encryption(&connection, scope, opened_scope_key.key_id())?;
    }
    Ok(connection)
}

fn configure_registry_cipher(
    connection: &Connection,
    scope_key: &AppDataScopeKey,
) -> Result<(), AppRegistryError> {
    let cipher_version = connection
        .query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)?
        .filter(|version| !version.is_empty())
        .ok_or(AppRegistryError::AtRestEncryptionFailed)?;
    let _ = cipher_version;

    // The already-domain-separated 256-bit value is supplied as a passphrase
    // to SQLCipher. SQLCipher v4 applies its own per-file salt/KDF and uses
    // authenticated encrypted pages; no key bytes enter SQL text, errors or
    // tracing fields.
    let mut passphrase = hex::encode(scope_key.expose_for_sqlcipher());
    let key_result = connection.pragma_update(None, "key", &passphrase);
    use zeroize::Zeroize;
    passphrase.zeroize();
    key_result.map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    connection
        .pragma_update(None, "cipher_compatibility", 4_i64)
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    connection
        .pragma_update(None, "cipher_memory_security", "ON")
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    // Force authentication now. SQLCipher otherwise defers a wrong/missing-key
    // failure until the first later schema access.
    connection
        .query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|_| ())
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)
}

fn migrate_plaintext_registry_to_sqlcipher(
    database_path: &Path,
    scope: &AppScope,
    scope_key: &AppDataScopeKey,
) -> Result<(), AppRegistryError> {
    if !registry_has_plaintext_header(database_path)? {
        return Ok(());
    }

    let parent = database_path.parent().ok_or_else(|| {
        AppRegistryError::UnsafePath("database path has no parent directory".to_owned())
    })?;
    let file_name = database_path
        .file_name()
        .ok_or_else(|| AppRegistryError::UnsafePath("database path has no file name".to_owned()))?;
    let temporary_path = parent.join(format!(
        ".{}.app-data-encryption-v20",
        file_name.to_string_lossy()
    ));
    remove_stale_cipher_migration_file(&temporary_path)?;
    // Create the destination ourselves so its identity and permissions are
    // fixed before SQLCipher opens the attached database. This also avoids
    // inheriting a platform/VFS-specific create posture from the plaintext
    // main connection.
    let mut temporary_options = fs::OpenOptions::new();
    temporary_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        temporary_options.mode(0o600);
    }
    let temporary_file = temporary_options.open(&temporary_path)?;
    set_private_sqlite_permissions(&temporary_path)?;
    temporary_file.sync_all()?;
    drop(temporary_file);

    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_FULL_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let plaintext = Connection::open_with_flags(database_path, flags)?;
    plaintext.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    plaintext.pragma_update(None, "foreign_keys", "ON")?;
    let application_id = pragma_i32(&plaintext, "application_id")?;
    let schema_version = pragma_i32(&plaintext, "user_version")?;
    if application_id != APP_REGISTRY_APPLICATION_ID || !(1..=19).contains(&schema_version) {
        return Err(AppRegistryError::IncompatibleDatabase {
            application_id,
            schema_version,
        });
    }
    verify_scope_binding(&plaintext, scope)?;
    plaintext.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;

    let mut passphrase = hex::encode(scope_key.expose_for_sqlcipher());
    let attach_result = plaintext.execute(
        "ATTACH DATABASE ?1 AS app_data_encrypted KEY ?2",
        params![
            temporary_path.to_string_lossy().as_ref(),
            passphrase.as_str()
        ],
    );
    use zeroize::Zeroize;
    passphrase.zeroize();
    attach_result.map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    plaintext
        .execute_batch(
            "PRAGMA app_data_encrypted.cipher_compatibility=4;
             SELECT sqlcipher_export('app_data_encrypted');",
        )
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    plaintext.pragma_update(
        Some(rusqlite::DatabaseName::Attached("app_data_encrypted")),
        "application_id",
        application_id,
    )?;
    plaintext.pragma_update(
        Some(rusqlite::DatabaseName::Attached("app_data_encrypted")),
        "user_version",
        schema_version,
    )?;
    plaintext
        .execute_batch("DETACH DATABASE app_data_encrypted")
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    drop(plaintext);

    set_private_sqlite_permissions(&temporary_path)?;
    // The checkpoint above makes the old sidecars disposable. Removing them
    // before the same-directory atomic replacement prevents stale plaintext
    // WAL frames from being associated with the encrypted database.
    for sidecar in sqlite_owned_paths(database_path).into_iter().skip(1) {
        match fs::remove_file(&sidecar) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    // SQLCipher, rather than this process, produces the staged database bytes.
    // Publish that already-synced engine output through the shared durable
    // staged-file boundary so the rename and parent-directory sync cannot
    // drift from other filesystem stores.
    publish_staged_file_durably_sync(&temporary_path, database_path)?;
    set_private_sqlite_permissions(database_path)?;
    Ok(())
}

fn registry_has_plaintext_header(database_path: &Path) -> Result<bool, AppRegistryError> {
    let mut file = match fs::File::open(database_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut header = [0_u8; 16];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(&header == b"SQLite format 3\0"),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn remove_stale_cipher_migration_file(path: &Path) -> Result<(), AppRegistryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(AppRegistryError::UnsafePath(
                    "app-data encryption migration path is not a regular file".to_owned(),
                ));
            }
            ensure_sqlite_file_metadata_is_safe(path, &metadata)?;
            fs::remove_file(path)?;
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    for sidecar in sqlite_owned_paths(path).into_iter().skip(1) {
        match fs::remove_file(sidecar) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn initialize_or_verify_schema(
    connection: &mut Connection,
    scope: &AppScope,
    opened_key_id: &str,
    active_key_id: &str,
) -> Result<(), AppRegistryError> {
    let application_id = pragma_i32(connection, "application_id")?;
    let schema_version = pragma_i32(connection, "user_version")?;
    if application_id == APP_REGISTRY_APPLICATION_ID
        && schema_version == APP_REGISTRY_SCHEMA_VERSION
    {
        return verify_schema_scope_and_opened_encryption(
            connection,
            scope,
            opened_key_id,
            active_key_id,
        );
    }
    // One-time development upgrade path. A writable open may convert an
    // existing historical store, but no historical version is accepted by
    // read-only callers and every successful path ends at the current schema.
    if application_id == APP_REGISTRY_APPLICATION_ID
        && matches!(
            schema_version,
            1 | 2
                | 3
                | 4
                | 5
                | 6
                | 7
                | 8
                | 9
                | 10
                | 11
                | 12
                | 13
                | 14
                | 15
                | 16
                | 17
                | 18
                | 19
                | 20
                | 21
                | 22
                | 23
                | 24
                | 25
                | 26
                | 27
                | 28
                | 29
                | 30
                | 31
                | 32
                | 33
                | 34
                | 35
                | 36
                | 37
        )
    {
        verify_scope_binding(connection, scope)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let locked_application_id = pragma_i32(&transaction, "application_id")?;
        let locked_schema_version = pragma_i32(&transaction, "user_version")?;
        if locked_application_id == APP_REGISTRY_APPLICATION_ID
            && locked_schema_version == APP_REGISTRY_SCHEMA_VERSION
        {
            transaction.commit()?;
            return verify_schema_scope_and_opened_encryption(
                connection,
                scope,
                opened_key_id,
                active_key_id,
            );
        }
        if locked_application_id != application_id || locked_schema_version != schema_version {
            return Err(AppRegistryError::IncompatibleDatabase {
                application_id: locked_application_id,
                schema_version: locked_schema_version,
            });
        }
        // Scope authority is rechecked under the same write lock as the
        // migration so a concurrent or external binding change cannot race the
        // pre-lock compatibility read.
        verify_scope_binding(&transaction, scope)?;
        if schema_version == 1 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V2)?;
        }
        if schema_version <= 2 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V3)?;
        }
        if schema_version <= 3 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V4)?;
        }
        if schema_version <= 4 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V5)?;
        }
        if schema_version <= 5 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V6)?;
        }
        if schema_version <= 6 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V7)?;
        }
        if schema_version <= 7 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V8)?;
        }
        if schema_version <= 8 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_DIRECTORY)?;
        }
        if schema_version <= 9 {
            ensure_v10_resource_baselines_are_migratable(&transaction)?;
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V10)?;
        }
        if schema_version == 10 {
            ensure_v10_resource_baselines_are_complete(&transaction)?;
        }
        if schema_version <= 10 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V11)?;
        }
        if schema_version <= 11 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V12)?;
        }
        if schema_version <= 12 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V13)?;
        }
        if schema_version <= 13 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V14)?;
        }
        if schema_version <= 14 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V15)?;
        }
        if schema_version <= 15 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V16)?;
        }
        if schema_version <= 16 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V17)?;
        }
        if schema_version <= 17 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V18)?;
        }
        if schema_version <= 18 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V19)?;
        }
        if schema_version <= 19 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V20)?;
            insert_app_data_encryption_metadata(&transaction, active_key_id, "legacy_plaintext")?;
        }
        if schema_version <= 20 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V21)?;
        }
        if schema_version <= 21 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V22)?;
        }
        if schema_version <= 22 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V23)?;
        }
        if schema_version <= 23 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V24)?;
        }
        if schema_version <= 24 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V25)?;
        }
        if schema_version <= 25 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V26)?;
        }
        ensure_v27_owner_notification_period_column(&transaction)?;
        if schema_version <= 26 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V27)?;
        }
        if schema_version <= 27 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V28)?;
        }
        if schema_version <= 28 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V29)?;
        }
        if schema_version <= 29 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V30)?;
        }
        if schema_version <= 30 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V31)?;
        } else {
            // V31 is a claimed compatibility boundary, not merely a version
            // number. Refuse to bless a counterfeit/partial refund authority as
            // V32 before adding the correlation-first index.
            verify_refund_schema_shape(
                &transaction,
                locked_application_id,
                locked_schema_version,
                false,
            )?;
        }
        if schema_version <= 31 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V32)?;
        }
        if schema_version <= 32 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V33)?;
        }
        if schema_version <= 33 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V34)?;
        }
        if schema_version <= 34 {
            super::indexed_snapshot::migrate_keyset(&transaction)?;
        }
        if schema_version <= 35 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V36)?;
        }
        if schema_version <= 36 {
            transaction.execute_batch(APP_REGISTRY_SCHEMA_V37)?;
        }
        transaction.execute_batch(APP_REGISTRY_SCHEMA_V38)?;
        transaction.pragma_update(None, "user_version", APP_REGISTRY_SCHEMA_VERSION)?;
        transaction.commit()?;
        // The database can still be encrypted with a retained historical key.
        // Validate the key that actually opened it here; the caller performs
        // the rekey only after this version migration succeeds. Requiring the
        // active key at this boundary would strand every V20-V31 registry first
        // opened after a root-key rotation.
        return verify_schema_scope_and_opened_encryption(
            connection,
            scope,
            opened_key_id,
            active_key_id,
        );
    }
    if application_id != 0 || schema_version != 0 {
        return Err(AppRegistryError::IncompatibleDatabase {
            application_id,
            schema_version,
        });
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let locked_application_id = pragma_i32(&transaction, "application_id")?;
    let locked_schema_version = pragma_i32(&transaction, "user_version")?;
    if locked_application_id == APP_REGISTRY_APPLICATION_ID
        && locked_schema_version == APP_REGISTRY_SCHEMA_VERSION
    {
        transaction.commit()?;
        return verify_schema_scope_and_opened_encryption(
            connection,
            scope,
            opened_key_id,
            active_key_id,
        );
    }
    let existing_tables: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if locked_application_id != 0 || locked_schema_version != 0 || existing_tables != 0 {
        return Err(AppRegistryError::IncompatibleDatabase {
            application_id: locked_application_id,
            schema_version: locked_schema_version,
        });
    }
    transaction.execute_batch(APP_REGISTRY_SCHEMA)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V2)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V3)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V4)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V5)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V6)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V7)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V8)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_DIRECTORY)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V10)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V11)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V12)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V13)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V14)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V15)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V16)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V17)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V18)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V19)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V20)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V21)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V22)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V23)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V24)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V25)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V26)?;
    ensure_v27_owner_notification_period_column(&transaction)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V27)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V28)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V29)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V30)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V31)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V32)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V33)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V34)?;
    super::indexed_snapshot::migrate_keyset(&transaction)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V36)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V37)?;
    transaction.execute_batch(APP_REGISTRY_SCHEMA_V38)?;
    transaction.execute(
        "INSERT INTO app_registry_scope(singleton, principal, workspace)
         VALUES (1, ?1, ?2)",
        params![scope.principal.as_str(), scope.workspace.as_str()],
    )?;
    insert_app_data_encryption_metadata(&transaction, active_key_id, "encrypted_birth")?;
    transaction.pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", APP_REGISTRY_SCHEMA_VERSION)?;
    transaction.commit()?;
    verify_schema_scope_and_encryption(connection, scope, active_key_id)
}

fn ensure_v27_owner_notification_period_column(
    transaction: &Transaction<'_>,
) -> Result<(), AppRegistryError> {
    let column = transaction
        .query_row(
            "SELECT type, \"notnull\"
               FROM pragma_table_info('app_owner_notification_outbox')
              WHERE name = 'period_seconds'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    match column {
        None => {
            // A legacy V26 row has already consumed capacity under an unknown
            // reviewed window. Seed the longest supported period so migration
            // cannot create a fresh, more permissive delivery allowance.
            transaction.execute_batch(
                "ALTER TABLE app_owner_notification_outbox
                     ADD COLUMN period_seconds INTEGER NOT NULL DEFAULT 2678400
                     CHECK(period_seconds BETWEEN 60 AND 2678400);",
            )?;
        },
        Some((column_type, not_null))
            if column_type.eq_ignore_ascii_case("INTEGER") && not_null == 1 => {},
        Some(_) => {
            return Err(AppRegistryError::StateConflict(
                "owner-notification reviewed period column is incompatible".to_owned(),
            ));
        },
    }
    Ok(())
}

/// V10 added immutable runtime baselines which cannot be reconstructed from
/// the v8/v9 registry alone. In particular, package byte accounting and the
/// installation-period monotonic deadline have no trustworthy historical
/// source in those schemas. An empty authority can migrate directly; a
/// populated authority must first be retired or recovered by an explicit
/// version-aware operator rather than silently inventing permissive values.
fn ensure_v10_resource_baselines_are_migratable(
    transaction: &Transaction<'_>,
) -> Result<(), AppRegistryError> {
    let legacy_tree_count: i64 =
        transaction.query_row("SELECT COUNT(*) FROM app_resource_trees", [], |row| {
            row.get(0)
        })?;
    let tree_count = u64::try_from(legacy_tree_count).map_err(|_| {
        AppRegistryError::StateConflict("negative legacy resource-tree count".into())
    })?;
    if tree_count != 0 {
        return Err(AppRegistryError::LegacyResourceTreesRequireExplicitMigration { tree_count });
    }
    Ok(())
}

/// Some development builds could materialize the v10 columns over pre-existing
/// resource rows before the fail-closed v9 migration fence shipped. V11 must
/// not make those rows look current: neither immutable period deadline nor
/// package measurement can be reconstructed from the remaining record.
fn ensure_v10_resource_baselines_are_complete(
    transaction: &Transaction<'_>,
) -> Result<(), AppRegistryError> {
    let incomplete_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_resource_trees
          WHERE period_ends_at_elapsed_ms IS NULL
             OR period_ends_at_elapsed_ms <= 0
             OR package_bytes IS NULL
             OR package_bytes < 0",
        [],
        |row| row.get(0),
    )?;
    let tree_count = u64::try_from(incomplete_count).map_err(|_| {
        AppRegistryError::StateConflict(
            "negative incomplete v10 resource-baseline count".to_owned(),
        )
    })?;
    if tree_count != 0 {
        return Err(
            AppRegistryError::IncompleteResourceBaselinesRequireExplicitMigration { tree_count },
        );
    }
    Ok(())
}

fn verify_schema_and_scope(
    connection: &Connection,
    scope: &AppScope,
) -> Result<(), AppRegistryError> {
    let application_id = pragma_i32(connection, "application_id")?;
    let schema_version = pragma_i32(connection, "user_version")?;
    if application_id != APP_REGISTRY_APPLICATION_ID
        || !(APP_REGISTRY_MINIMUM_READABLE_SCHEMA_VERSION..=APP_REGISTRY_SCHEMA_VERSION)
            .contains(&schema_version)
    {
        return Err(AppRegistryError::IncompatibleDatabase {
            application_id,
            schema_version,
        });
    }
    verify_current_schema_objects(connection, application_id, schema_version)?;
    verify_scope_binding(connection, scope)
}

fn verify_current_schema_objects(
    connection: &Connection,
    application_id: i32,
    schema_version: i32,
) -> Result<(), AppRegistryError> {
    for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V26_REQUIRED_OBJECTS
        .iter()
        .chain(APP_REGISTRY_SCHEMA_V27_REQUIRED_OBJECTS.iter())
        .chain(APP_REGISTRY_SCHEMA_V28_REQUIRED_OBJECTS.iter())
        .chain(APP_REGISTRY_SCHEMA_V29_REQUIRED_OBJECTS.iter())
        .chain(APP_REGISTRY_SCHEMA_V30_REQUIRED_OBJECTS.iter())
        .chain(APP_REGISTRY_SCHEMA_V31_REQUIRED_OBJECTS.iter())
        .chain(APP_REGISTRY_SCHEMA_V32_REQUIRED_OBJECTS.iter())
        .chain(
            [
                ("table", "app_data_cleanup_jobs"),
                ("table", "app_data_cleanup_candidates"),
                ("index", "app_data_cleanup_jobs_installation_idx"),
                ("index", "app_data_cleanup_candidates_pending_idx"),
            ]
            .iter(),
        )
        .chain(
            [
                ("table", "app_behavior_execution_state"),
                ("index", "app_behavior_execution_observation_idx"),
            ]
            .iter(),
        )
        .chain([("table", "app_memory_read_grant_heads")].iter())
        .chain(
            [
                ("table", "app_recurring_task_heads"),
                ("table", "app_recurring_occurrence_locators"),
                ("trigger", "app_recurring_occurrence_immutable"),
            ]
            .iter(),
        )
        .chain(
            [
                ("index", "app_scalar_order_asc_idx"),
                ("index", "app_scalar_order_desc_idx"),
                ("table", "app_keyset_cursors"),
                ("trigger", "app_scalar_order_insert_guard"),
                ("trigger", "app_scalar_order_update_guard"),
                ("index", "app_keyset_cursors_chain_idx"),
                ("index", "app_keyset_cursors_expiry_idx"),
            ]
            .iter(),
        )
    {
        let present: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = ?1 AND name = ?2",
            params![object_type, object_name],
            |row| row.get(0),
        )?;
        if present != 1 {
            return Err(AppRegistryError::IncompatibleDatabase {
                application_id,
                schema_version,
            });
        }
    }
    let notification_period_column: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('app_owner_notification_outbox')
          WHERE name = 'period_seconds' AND lower(type) = 'integer'
            AND \"notnull\" = 1",
        [],
        |row| row.get(0),
    )?;
    if notification_period_column != 1 {
        return Err(AppRegistryError::IncompatibleDatabase {
            application_id,
            schema_version,
        });
    }
    verify_refund_schema_shape(connection, application_id, schema_version, true)?;
    Ok(())
}

fn verify_refund_schema_shape(
    connection: &Connection,
    application_id: i32,
    schema_version: i32,
    require_correlation_index: bool,
) -> Result<(), AppRegistryError> {
    let incompatible = || AppRegistryError::IncompatibleDatabase {
        application_id,
        schema_version,
    };

    let table_shape: Vec<(String, String, i64, i64, i64)> = {
        let mut statement = connection.prepare(
            "SELECT name, upper(type), \"notnull\", pk, hidden
               FROM pragma_table_xinfo('app_owner_notification_attempt_refunds')
              ORDER BY cid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let expected_table_shape = [
        ("correlation_id", "TEXT", 1, 1, 0),
        ("charged_revision", "INTEGER", 1, 2, 0),
        ("charged_fence", "INTEGER", 1, 3, 0),
        ("charged_lease_owner", "TEXT", 1, 0, 0),
        ("charged_lease_token", "TEXT", 1, 0, 0),
        ("created_at", "TEXT", 1, 0, 0),
        ("applied_at", "TEXT", 0, 0, 0),
    ];
    if table_shape.len() != expected_table_shape.len()
        || table_shape.iter().zip(expected_table_shape).any(
            |((name, column_type, not_null, primary_key, hidden), expected)| {
                (
                    name.as_str(),
                    column_type.as_str(),
                    *not_null,
                    *primary_key,
                    *hidden,
                ) != expected
            },
        )
    {
        return Err(incompatible());
    }
    let strict_table: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_table_list
          WHERE schema = 'main'
            AND name = 'app_owner_notification_attempt_refunds'
            AND type = 'table' AND ncol = 7 AND wr = 0 AND strict = 1",
        [],
        |row| row.get(0),
    )?;
    if strict_table != 1 {
        return Err(incompatible());
    }
    let refund_table_sql: String = connection.query_row(
        "SELECT sql FROM sqlite_schema
          WHERE type = 'table'
            AND name = 'app_owner_notification_attempt_refunds'",
        [],
        |row| row.get(0),
    )?;
    let refund_table_sql = refund_table_sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let expected_refund_table_sql = "create table app_owner_notification_attempt_refunds ( \
correlation_id text not null check(length(correlation_id) between 1 and 192), \
charged_revision integer not null check(charged_revision > 0), \
charged_fence integer not null check(charged_fence >= 0), \
charged_lease_owner text not null check(length(charged_lease_owner) between 1 and 192), \
charged_lease_token text not null check(length(charged_lease_token) = 71), \
created_at text not null, applied_at text, \
primary key(correlation_id, charged_revision, charged_fence), \
foreign key(correlation_id) references app_owner_notification_outbox(correlation_id) \
on delete cascade ) strict";
    if refund_table_sql != expected_refund_table_sql {
        return Err(incompatible());
    }

    let foreign_key_shape: (i64, i64) = connection.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE
                    WHEN \"table\" = 'app_owner_notification_outbox'
                     AND \"from\" = 'correlation_id' AND \"to\" = 'correlation_id'
                     AND upper(on_update) = 'NO ACTION'
                     AND upper(on_delete) = 'CASCADE'
                     AND upper(\"match\") = 'NONE'
                    THEN 1 ELSE 0 END), 0)
           FROM pragma_foreign_key_list('app_owner_notification_attempt_refunds')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if foreign_key_shape != (1, 1) {
        return Err(incompatible());
    }

    let pending_index_shape: (i64, i64) = connection.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE
                    WHEN \"unique\" = 0 AND origin = 'c' AND partial = 1
                    THEN 1 ELSE 0 END), 0)
           FROM pragma_index_list('app_owner_notification_attempt_refunds')
          WHERE name = 'app_owner_notification_attempt_refunds_pending_idx'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if pending_index_shape != (1, 1) {
        return Err(incompatible());
    }
    let pending_index_columns: Vec<String> = {
        let mut statement = connection.prepare(
            "SELECT name
               FROM pragma_index_info('app_owner_notification_attempt_refunds_pending_idx')
              ORDER BY seqno",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if pending_index_columns.len() != 4
        || pending_index_columns[0] != "created_at"
        || pending_index_columns[1] != "correlation_id"
        || pending_index_columns[2] != "charged_revision"
        || pending_index_columns[3] != "charged_fence"
    {
        return Err(incompatible());
    }
    let pending_index_sql: String = connection.query_row(
        "SELECT sql FROM sqlite_schema
          WHERE type = 'index'
            AND name = 'app_owner_notification_attempt_refunds_pending_idx'",
        [],
        |row| row.get(0),
    )?;
    let pending_index_sql = pending_index_sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if pending_index_sql
        != "create index app_owner_notification_attempt_refunds_pending_idx \
on app_owner_notification_attempt_refunds( created_at, correlation_id, charged_revision, \
charged_fence ) where applied_at is null"
    {
        return Err(incompatible());
    }

    if require_correlation_index {
        let correlation_index_shape: (i64, i64) = connection.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE
                        WHEN \"unique\" = 0 AND origin = 'c' AND partial = 1
                        THEN 1 ELSE 0 END), 0)
               FROM pragma_index_list('app_owner_notification_attempt_refunds')
              WHERE name = 'app_owner_notification_attempt_refunds_correlation_idx'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if correlation_index_shape != (1, 1) {
            return Err(incompatible());
        }
        let correlation_index_columns: Vec<String> = {
            let mut statement = connection.prepare(
                "SELECT name
                   FROM pragma_index_info('app_owner_notification_attempt_refunds_correlation_idx')
                  ORDER BY seqno",
            )?;
            let rows = statement.query_map([], |row| row.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if correlation_index_columns.len() != 3
            || correlation_index_columns[0] != "correlation_id"
            || correlation_index_columns[1] != "charged_revision"
            || correlation_index_columns[2] != "charged_fence"
        {
            return Err(incompatible());
        }
        let correlation_index_sql: String = connection.query_row(
            "SELECT sql FROM sqlite_schema
              WHERE type = 'index'
                AND name = 'app_owner_notification_attempt_refunds_correlation_idx'",
            [],
            |row| row.get(0),
        )?;
        let correlation_index_sql = correlation_index_sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        if correlation_index_sql
            != "create index app_owner_notification_attempt_refunds_correlation_idx \
on app_owner_notification_attempt_refunds( correlation_id, charged_revision, charged_fence ) \
where applied_at is null"
        {
            return Err(incompatible());
        }
    }

    let refund_guard_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_schema
              WHERE type = 'trigger'
                AND name = 'app_owner_notification_attempt_refunds_guard'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| incompatible())?;
    let refund_guard_sql = refund_guard_sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let expected_refund_guard_sql = "create trigger app_owner_notification_attempt_refunds_guard \
before update on app_owner_notification_attempt_refunds when new.correlation_id != old.correlation_id \
or new.charged_revision != old.charged_revision or new.charged_fence != old.charged_fence \
or new.charged_lease_owner != old.charged_lease_owner or new.charged_lease_token != old.charged_lease_token \
or new.created_at != old.created_at or new.applied_at is null or old.applied_at is not null \
begin select raise(abort, 'app owner-notification refund receipt is immutable after application'); end";
    if refund_guard_sql != expected_refund_guard_sql {
        return Err(incompatible());
    }
    Ok(())
}

fn insert_app_data_encryption_metadata(
    transaction: &Transaction<'_>,
    key_id: &str,
    encrypted_from: &'static str,
) -> Result<(), AppRegistryError> {
    let now = format_timestamp(&Utc::now());
    transaction.execute(
        "INSERT INTO app_data_encryption_metadata (
             singleton, format_version, algorithm, key_id,
             encrypted_from, last_rekeyed_at
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![
            APP_REGISTRY_CIPHER_FORMAT_VERSION,
            APP_REGISTRY_CIPHER_ALGORITHM,
            key_id,
            encrypted_from,
            now,
        ],
    )?;
    Ok(())
}

fn load_app_data_encryption_key_id(connection: &Connection) -> Result<String, AppRegistryError> {
    connection
        .query_row(
            "SELECT key_id FROM app_data_encryption_metadata WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(AppRegistryError::AtRestEncryptionFailed)
}

fn rekey_registry_to_active_generation(
    connection: &mut Connection,
    scope: &AppScope,
    opened_key: &AppDataScopeKey,
    active_key: &AppDataScopeKey,
) -> Result<(), AppRegistryError> {
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored_key_id: String = transaction
        .query_row(
            "SELECT key_id FROM app_data_encryption_metadata WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    if stored_key_id != opened_key.key_id() && stored_key_id != active_key.key_id() {
        return Err(AppRegistryError::AtRestEncryptionFailed);
    }
    transaction.execute(
        "UPDATE app_data_encryption_metadata
            SET key_id=?1, last_rekeyed_at=?2 WHERE singleton=1",
        params![active_key.key_id(), format_timestamp(&Utc::now())],
    )?;
    transaction.commit()?;

    let mut passphrase = hex::encode(active_key.expose_for_sqlcipher());
    let result = connection.pragma_update(None, "rekey", &passphrase);
    use zeroize::Zeroize;
    passphrase.zeroize();
    result.map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection
        .query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|_| AppRegistryError::AtRestEncryptionFailed)?;
    verify_schema_scope_and_encryption(connection, scope, active_key.key_id())
}

fn verify_schema_scope_and_encryption(
    connection: &Connection,
    scope: &AppScope,
    key_id: &str,
) -> Result<(), AppRegistryError> {
    verify_schema_and_scope(connection, scope)?;
    let metadata = connection
        .query_row(
            "SELECT format_version, algorithm, key_id
               FROM app_data_encryption_metadata WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    if metadata.as_ref()
        != Some(&(
            APP_REGISTRY_CIPHER_FORMAT_VERSION,
            APP_REGISTRY_CIPHER_ALGORITHM.to_owned(),
            key_id.to_owned(),
        ))
    {
        return Err(AppRegistryError::AtRestEncryptionFailed);
    }
    Ok(())
}

fn verify_schema_scope_and_opened_encryption(
    connection: &Connection,
    scope: &AppScope,
    opened_key_id: &str,
    active_key_id: &str,
) -> Result<(), AppRegistryError> {
    verify_schema_and_scope(connection, scope)?;
    let metadata = connection
        .query_row(
            "SELECT format_version, algorithm, key_id
               FROM app_data_encryption_metadata WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((format_version, algorithm, stored_key_id)) = metadata else {
        return Err(AppRegistryError::AtRestEncryptionFailed);
    };
    if format_version != APP_REGISTRY_CIPHER_FORMAT_VERSION
        || algorithm != APP_REGISTRY_CIPHER_ALGORITHM
        || !opened_registry_key_metadata_is_compatible(
            opened_key_id,
            active_key_id,
            stored_key_id.as_str(),
        )
    {
        return Err(AppRegistryError::AtRestEncryptionFailed);
    }
    Ok(())
}

fn opened_registry_key_metadata_is_compatible(
    opened_key_id: &str,
    active_key_id: &str,
    stored_key_id: &str,
) -> bool {
    stored_key_id == opened_key_id
        || (opened_key_id != active_key_id && stored_key_id == active_key_id)
}

fn verify_scope_binding(connection: &Connection, scope: &AppScope) -> Result<(), AppRegistryError> {
    let stored = connection
        .query_row(
            "SELECT principal, workspace FROM app_registry_scope WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if stored.as_ref()
        != Some(&(
            scope.principal.as_str().to_owned(),
            scope.workspace.as_str().to_owned(),
        ))
    {
        return Err(AppRegistryError::ScopeCollision);
    }
    Ok(())
}

fn pragma_i32(connection: &Connection, pragma: &str) -> Result<i32, rusqlite::Error> {
    connection.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
}

fn verify_exact_payload(
    transaction: &Transaction<'_>,
    sql: &str,
    identity: &str,
    expected: &[u8],
    entity: &'static str,
) -> Result<(), AppRegistryError> {
    let stored = transaction
        .query_row(sql, params![identity], |row| row.get::<_, Vec<u8>>(0))
        .optional()?;
    if stored.as_deref() != Some(expected) {
        return Err(AppRegistryError::IdentityConflict {
            entity,
            identity: identity.to_owned(),
        });
    }
    Ok(())
}

pub fn encode_bounded_json<T: Serialize>(
    value: &T,
    limits: &AppContractLimits,
) -> Result<Vec<u8>, AppRegistryError> {
    let value = serde_json::to_value(value)?;
    validate_json_value(&value, limits)?;
    let bytes = canonical_json_bytes(&value)?;
    if bytes.len() > limits.max_document_bytes() {
        return Err(AppRegistryError::Contract(
            AppContractError::DocumentTooLarge {
                limit: limits.max_document_bytes(),
            },
        ));
    }
    Ok(bytes)
}

pub fn enum_json_label<T: Serialize>(value: &T) -> Result<String, AppRegistryError> {
    let encoded = serde_json::to_string(value)?;
    encoded
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or(AppRegistryError::InvalidPublication(
            "registry enum did not serialize as a string",
        ))
}

pub fn format_timestamp(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Open an existing scoped registry for a trusted server-internal read.
/// Missing stores stay lazy and return `None` instead of creating a database.
pub fn open_existing_scoped_registry_read_only(
    workspace: &ArtifactV2Workspace,
    scope: &AppScope,
) -> Result<Option<RegistryConnectionLease>, AppRegistryError> {
    let database_path =
        workspace.app_store_db_path(scope.principal.as_str(), scope.workspace.as_str());
    if let Err(error) = validate_existing_registry_paths(workspace.base_root(), &database_path) {
        if matches!(
            &error,
            AppRegistryError::Io(io_error) if io_error.kind() == std::io::ErrorKind::NotFound
        ) {
            return Ok(None);
        }
        return Err(error);
    }
    match fs::symlink_metadata(&database_path) {
        Ok(metadata) if metadata.is_file() => {},
        Ok(_) => {
            return Err(AppRegistryError::UnsafePath(format!(
                "database path '{}' is not a regular file",
                database_path.display()
            )));
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Ok(Some(open_registry_connection(
        &database_path,
        scope,
        false,
    )?))
}

fn ensure_registry_parent_tree(
    provider_root: &Path,
    database_path: &Path,
) -> Result<(), AppRegistryError> {
    let parent = database_path.parent().ok_or_else(|| {
        AppRegistryError::UnsafePath("database path has no parent directory".to_owned())
    })?;
    let relative = parent.strip_prefix(provider_root).map_err(|_| {
        AppRegistryError::UnsafePath("database path escapes the workspace root".to_owned())
    })?;
    fs::create_dir_all(provider_root)?;
    ensure_directory_metadata_is_safe(provider_root, &fs::symlink_metadata(provider_root)?)?;
    let mut current = provider_root.to_path_buf();
    for component in relative.components() {
        use std::path::Component;
        let Component::Normal(segment) = component else {
            return Err(AppRegistryError::UnsafePath(
                "database parent contains a non-normal path component".to_owned(),
            ));
        };
        current.push(segment);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure_directory_metadata_is_safe(&current, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => set_private_directory_permissions(&current)?,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
                    Err(error) => return Err(error.into()),
                }
                let metadata = fs::symlink_metadata(&current)?;
                ensure_directory_metadata_is_safe(&current, &metadata)?;
            },
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn validate_existing_registry_paths(
    provider_root: &Path,
    database_path: &Path,
) -> Result<(), AppRegistryError> {
    let parent = database_path.parent().ok_or_else(|| {
        AppRegistryError::UnsafePath("database path has no parent directory".to_owned())
    })?;
    let relative = parent.strip_prefix(provider_root).map_err(|_| {
        AppRegistryError::UnsafePath("database path escapes the workspace root".to_owned())
    })?;
    ensure_directory_metadata_is_safe(provider_root, &fs::symlink_metadata(provider_root)?)?;
    let mut current = provider_root.to_path_buf();
    for component in relative.components() {
        use std::path::Component;
        let Component::Normal(segment) = component else {
            return Err(AppRegistryError::UnsafePath(
                "database parent contains a non-normal path component".to_owned(),
            ));
        };
        current.push(segment);
        ensure_directory_metadata_is_safe(&current, &fs::symlink_metadata(&current)?)?;
    }
    for path in sqlite_owned_paths(database_path) {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AppRegistryError::UnsafePath(format!(
                    "SQLite-owned path '{}' is a symlink",
                    path.display()
                )));
            },
            Ok(metadata) if !metadata.is_file() => {
                return Err(AppRegistryError::UnsafePath(format!(
                    "SQLite-owned path '{}' is not a regular file",
                    path.display()
                )));
            },
            Ok(metadata) => ensure_sqlite_file_metadata_is_safe(&path, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn sqlite_owned_paths(database_path: &Path) -> [PathBuf; 3] {
    [
        database_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", database_path.display())),
        PathBuf::from(format!("{}-shm", database_path.display())),
    ]
}

fn ensure_directory_metadata_is_safe(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), AppRegistryError> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppRegistryError::UnsafePath(format!(
            "registry parent '{}' is not a real directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err(AppRegistryError::UnsafePath(format!(
                "registry parent '{}' is not owned privately by the current user",
                path.display()
            )));
        }
    }
    Ok(())
}

fn ensure_sqlite_file_metadata_is_safe(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), AppRegistryError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
        {
            return Err(AppRegistryError::UnsafePath(format!(
                "SQLite-owned file '{}' has unsafe owner, links or write permissions",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, metadata);
    Ok(())
}

#[cfg(unix)]
fn set_private_sqlite_permissions(database_path: &Path) -> Result<(), AppRegistryError> {
    use std::os::unix::fs::PermissionsExt;
    for path in sqlite_owned_paths(database_path) {
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure_sqlite_file_metadata_is_safe(&path, &metadata)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_private_sqlite_permissions(_database_path: &Path) -> Result<(), AppRegistryError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), AppRegistryError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), AppRegistryError> {
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod tests {
    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::apps::{
        authority::AuthenticatedAppScope,
        manifest::{
            build_app_package_candidate, tests::valid_bundle, AppDependencyKind, AppPackageLimits,
        },
        models::{AppRevision, AppScopeBindingRef},
        package_lock::{lock_app_package_dependencies, AppVerifiedRegistryDependency},
        package_staging::materialize_test_staged_package,
        records::{AppCompatibilityRequirement, AppPackageSourceKind},
    };

    pub fn canonical_tempdir() -> TempDir {
        let root = fs::canonicalize(std::env::temp_dir()).expect("canonical temporary root");
        tempfile::tempdir_in(root).expect("temporary directory")
    }

    pub fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 0, 0, second)
            .single()
            .unwrap()
    }

    pub fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    #[test]
    fn migration_v24_redaction_honors_behavior_head_revision_cas() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE app_behavior_heads (
                    installation_id TEXT NOT NULL,
                    behavior_id TEXT NOT NULL,
                    state TEXT NOT NULL,
                    revision INTEGER NOT NULL,
                    fence INTEGER NOT NULL,
                    next_due_at TEXT NOT NULL,
                    available_at TEXT NOT NULL,
                    lease_expires_at TEXT,
                    last_error TEXT,
                    PRIMARY KEY (installation_id, behavior_id)
                );
                CREATE TABLE app_behavior_events (
                    kind TEXT NOT NULL,
                    reason TEXT NOT NULL
                );
                CREATE INDEX app_behavior_heads_due_idx
                    ON app_behavior_heads(
                        state, available_at, next_due_at,
                        installation_id, behavior_id
                    );
                CREATE TRIGGER app_behavior_heads_revision_guard
                BEFORE UPDATE ON app_behavior_heads
                WHEN NEW.revision != OLD.revision + 1
                  OR NEW.fence < OLD.fence
                BEGIN
                    SELECT RAISE(
                        ABORT,
                        'app behavior head requires revision CAS and monotonic fence'
                    );
                END;
                INSERT INTO app_behavior_heads(
                    installation_id, behavior_id, state, revision, fence,
                    next_due_at, available_at, lease_expires_at, last_error
                ) VALUES
                    ('install_unknown', 'digest', 'idle', 7, 2,
                     '2026-09-02T00:00:00Z', '2026-09-02T00:00:00Z', NULL,
                     'unbounded upstream failure text'),
                    ('install_closed', 'digest', 'pending', 11, 4,
                     '2026-09-02T00:00:00Z', '2026-09-02T00:00:00Z',
                     '2026-09-02T00:01:00Z', 'workflow_launch_retryable');
                "#,
            )
            .unwrap();

        connection.execute_batch(APP_REGISTRY_SCHEMA_V24).unwrap();

        let redacted = connection
            .query_row(
                "SELECT revision, fence, last_error FROM app_behavior_heads
                  WHERE installation_id = 'install_unknown'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(redacted, (8, 2, "legacy_error_redacted".to_owned()));

        let closed = connection
            .query_row(
                "SELECT revision, fence, last_error FROM app_behavior_heads
                  WHERE installation_id = 'install_closed'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(closed, (11, 4, "workflow_launch_retryable".to_owned()));
    }

    #[test]
    fn migration_v25_adds_resumable_reconciliation_progress() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_behavior_reconcile_state (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     reconciled_at TEXT NOT NULL
                 ) STRICT;
                 INSERT INTO app_behavior_reconcile_state(singleton, reconciled_at)
                 VALUES (1, '2026-09-02T00:00:00.000000Z');",
            )
            .unwrap();

        connection.execute_batch(APP_REGISTRY_SCHEMA_V25).unwrap();
        connection
            .execute(
                "UPDATE app_behavior_reconcile_state
                    SET pass_started_at = ?1, cursor_installation_id = ?2,
                        deferred_installation_ids_json = ?3
                  WHERE singleton = 1",
                params![
                    "2026-09-02T00:05:00.000000Z",
                    "install_reconcile_cursor",
                    br#"["install_deferred"]"#.as_slice(),
                ],
            )
            .unwrap();
        let progress = connection
            .query_row(
                "SELECT reconciled_at, pass_started_at, cursor_installation_id,
                        deferred_installation_ids_json
                   FROM app_behavior_reconcile_state WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            progress,
            (
                "2026-09-02T00:00:00.000000Z".to_owned(),
                "2026-09-02T00:05:00.000000Z".to_owned(),
                "install_reconcile_cursor".to_owned(),
                br#"["install_deferred"]"#.to_vec(),
            )
        );
    }

    #[test]
    fn v26_acceptance_ledgers_require_real_leases_and_immutable_identities() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON;
                 CREATE TABLE app_installations (
                     installation_id TEXT PRIMARY KEY
                 ) STRICT;
                 INSERT INTO app_installations(installation_id)
                 VALUES ('install-v26');",
            )
            .unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V26).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V27).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V28).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V29).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V30).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V31).unwrap();
        verify_refund_schema_shape(&connection, APP_REGISTRY_APPLICATION_ID, 31, false).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V32).unwrap();

        for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V26_REQUIRED_OBJECTS {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type=?1 AND name=?2",
                    params![object_type, object_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V26 {object_type} {object_name}");
        }
        for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V27_REQUIRED_OBJECTS {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type=?1 AND name=?2",
                    params![object_type, object_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V27 {object_type} {object_name}");
        }
        for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V28_REQUIRED_OBJECTS {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type=?1 AND name=?2",
                    params![object_type, object_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V28 {object_type} {object_name}");
        }
        for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V29_REQUIRED_OBJECTS {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type=?1 AND name=?2",
                    params![object_type, object_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V29 {object_type} {object_name}");
        }
        for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V30_REQUIRED_OBJECTS {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type=?1 AND name=?2",
                    params![object_type, object_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V30 {object_type} {object_name}");
        }
        for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V31_REQUIRED_OBJECTS {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type=?1 AND name=?2",
                    params![object_type, object_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V31 {object_type} {object_name}");
        }
        for &(object_type, object_name) in APP_REGISTRY_SCHEMA_V32_REQUIRED_OBJECTS {
            let present: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type=?1 AND name=?2",
                    params![object_type, object_name],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V32 {object_type} {object_name}");
        }
        connection
            .execute(
                "INSERT INTO app_terminal_compaction_quarantine (
                     candidate_kind, installation_id, candidate_ref,
                     reason_code, quarantined_at
                 ) VALUES ('event_ingress', 'install-v26', 'event-v29-poison',
                           'integrity_verification_failed',
                           '2026-09-02T00:00:00.000000Z')",
                [],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE app_terminal_compaction_quarantine
                        SET reason_code='compaction_state_conflict'
                      WHERE candidate_kind='event_ingress'
                        AND installation_id='install-v26'
                        AND candidate_ref='event-v29-poison'",
                    [],
                )
                .is_err(),
            "terminal-compaction quarantine marker was mutable"
        );
        assert_eq!(
            connection
                .execute(
                    "DELETE FROM app_terminal_compaction_quarantine
                      WHERE candidate_kind='event_ingress'
                        AND installation_id='install-v26'
                        AND candidate_ref='event-v29-poison'",
                    [],
                )
                .unwrap(),
            1,
            "installation purge could not delete the quarantine marker"
        );

        let reviewed_digest = AppDigest::blake3(b"reviewed-event-behavior").to_string();
        let projection_digest = AppDigest::blake3(b"projection").to_string();
        let causation_digest = AppDigest::blake3(b"causation").to_string();
        connection
            .execute(
                "INSERT INTO app_event_ingress_tombstones (
                     installation_id, event_ref, event_kind, projection_digest,
                     accepted_fanout, receipt_created_at, compacted_at
                 ) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)",
                params![
                    "install-v26",
                    "evt-v28-compacted",
                    "installation_execution_terminal_v1",
                    projection_digest,
                    "2026-09-02T00:00:00.000000Z",
                ],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE app_event_ingress_tombstones
                        SET compacted_at='2026-09-03T00:00:00.000000Z'
                      WHERE installation_id='install-v26'
                        AND event_ref='evt-v28-compacted'",
                    [],
                )
                .is_err(),
            "event replay tombstone was mutable"
        );
        for event_ref in ["evt_loop_v1_source", "evt_loop_v1_pending"] {
            connection
                .execute(
                    "INSERT INTO app_event_ingress_receipts (
                        installation_id, event_ref, event_kind, projection_json,
                        projection_digest, accepted_fanout, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
                    params![
                        "install-v26",
                        event_ref,
                        "execution.status.changed.v1",
                        br#"{}"#.as_slice(),
                        projection_digest,
                        "2026-09-02T00:00:00.000000Z",
                    ],
                )
                .unwrap();
        }
        let invalid_lease = connection.execute(
            "INSERT INTO app_event_behavior_fires (
                 installation_id, event_behavior_id, event_ref,
                 installation_generation, package_revision_ref,
                 schema_revision, grant_revision, reviewed_request_digest,
                 event_kind, projection_json, projection_digest,
                 causation_depth, causation_path_digest, state, revision,
                 fence, attempt_count, available_at, created_at, updated_at
             ) VALUES (
                 ?1, ?2, ?3, 1, ?4, 1, 1, ?5, ?6, ?7, ?8,
                 0, ?9, 'leased', 1, 0, 0, ?10, ?10, ?10
             )",
            params![
                "install-v26",
                "behavior-v26",
                "evt_loop_v1_source",
                "package:revision:v26",
                reviewed_digest,
                "execution.status.changed.v1",
                br#"{}"#.as_slice(),
                projection_digest,
                causation_digest,
                "2026-09-02T00:00:00.000000Z",
            ],
        );
        assert!(
            invalid_lease.is_err(),
            "leased fire without a lease was accepted"
        );

        connection
            .execute(
                "INSERT INTO app_event_behavior_fires (
                     installation_id, event_behavior_id, event_ref,
                     installation_generation, package_revision_ref,
                     schema_revision, grant_revision, reviewed_request_digest,
                     event_kind, projection_json, projection_digest,
                     causation_depth, causation_path_digest, state, revision,
                     fence, attempt_count, available_at, created_at, updated_at
                 ) VALUES (
                     ?1, ?2, ?3, 1, ?4, 1, 1, ?5, ?6, ?7, ?8,
                     0, ?9, 'pending', 1, 0, 0, ?10, ?10, ?10
                 )",
                params![
                    "install-v26",
                    "behavior-v26",
                    "evt_loop_v1_pending",
                    "package:revision:v26",
                    reviewed_digest,
                    "execution.status.changed.v1",
                    br#"{}"#.as_slice(),
                    projection_digest,
                    causation_digest,
                    "2026-09-02T00:00:00.000000Z",
                ],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE app_event_behavior_fires
                        SET event_kind='execution.relabelled.v1', revision=revision+1
                      WHERE installation_id='install-v26'
                        AND event_behavior_id='behavior-v26'
                        AND event_ref='evt_loop_v1_pending'",
                    [],
                )
                .is_err(),
            "event fire identity was mutable"
        );

        let payload_digest = AppDigest::blake3(b"notification").to_string();
        connection
            .execute(
                "INSERT INTO app_owner_notification_tombstones (
                     correlation_id, installation_id, installation_generation,
                     workflow_id, port_id, reviewed_request_digest, period_seconds,
                     effect_ref, payload_digest, severity, expires_at, created_at,
                     compacted_at
                 ) VALUES (?1, ?2, 1, ?3, ?4, ?5, 60, ?6, ?7, 'info', ?8, ?9, ?9)",
                params![
                    "notify-v28-compacted",
                    "install-v26",
                    "workflow-v26",
                    "notify-port-v26",
                    AppDigest::blake3(b"reviewed-notification").to_string(),
                    "effect-v28",
                    payload_digest,
                    "2026-09-03T00:00:00.000000Z",
                    "2026-09-02T00:00:00.000000Z",
                ],
            )
            .unwrap();
        assert!(
            connection
                .execute(
                    "UPDATE app_owner_notification_tombstones SET severity='warning'
                      WHERE correlation_id='notify-v28-compacted'",
                    [],
                )
                .is_err(),
            "owner-notification replay tombstone was mutable"
        );
        let invalid_notification_lease = connection.execute(
            "INSERT INTO app_owner_notification_outbox (
                 correlation_id, installation_id, installation_generation,
                 workflow_id, port_id, reviewed_request_digest, period_seconds, effect_ref,
                 payload_digest, payload_json, severity, state, revision,
                 fence, attempt_count, available_at, expires_at, created_at,
                 updated_at
             ) VALUES (
                 ?1, ?2, 1, ?3, ?4, ?5, 60, ?6, ?7, ?8, 'info', 'leased',
                 1, 0, 0, ?9, ?10, ?9, ?9
             )",
            params![
                "notify-v26",
                "install-v26",
                "workflow-v26",
                "notify-port-v26",
                AppDigest::blake3(b"reviewed-notification").to_string(),
                "effect-v26",
                payload_digest,
                br#"{}"#.as_slice(),
                "2026-09-02T00:00:00.000000Z",
                "2026-09-03T00:00:00.000000Z",
            ],
        );
        assert!(
            invalid_notification_lease.is_err(),
            "leased notification without a lease was accepted"
        );
    }

    #[test]
    fn v27_upgrades_legacy_notification_period_fail_closed() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_event_behavior_fires (
                     installation_id TEXT NOT NULL,
                     event_ref TEXT NOT NULL
                 );
                 CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY
                 );
                 INSERT INTO app_owner_notification_outbox(correlation_id)
                 VALUES ('legacy-notification');",
            )
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        ensure_v27_owner_notification_period_column(&transaction).unwrap();
        transaction.execute_batch(APP_REGISTRY_SCHEMA_V27).unwrap();
        transaction.commit().unwrap();

        let migrated_period: i64 = connection
            .query_row(
                "SELECT period_seconds FROM app_owner_notification_outbox
                  WHERE correlation_id = 'legacy-notification'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated_period, 2_678_400);
        assert!(connection
            .execute(
                "UPDATE app_owner_notification_outbox SET period_seconds = 60
                      WHERE correlation_id = 'legacy-notification'",
                [],
            )
            .is_err());
    }

    #[test]
    fn claimed_v26_without_v26_objects_is_not_schema_compatible() {
        let connection = Connection::open_in_memory().unwrap();
        let expected_scope = scope("anonymous", "default");
        connection
            .execute_batch(
                "CREATE TABLE app_registry_scope (
                     singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                     principal TEXT NOT NULL,
                     workspace TEXT NOT NULL
                 ) STRICT;
                 INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, 'anonymous', 'default');",
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection
            .pragma_update(None, "user_version", APP_REGISTRY_SCHEMA_VERSION)
            .unwrap();

        assert!(matches!(
            verify_schema_and_scope(&connection, &expected_scope),
            Err(AppRegistryError::IncompatibleDatabase { .. })
        ));
    }

    #[test]
    fn v32_refund_schema_verification_rejects_a_counterfeit_pending_index() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_owner_notification_outbox (
                     correlation_id TEXT PRIMARY KEY
                 ) STRICT;",
            )
            .unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V31).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V32).unwrap();
        verify_refund_schema_shape(
            &connection,
            APP_REGISTRY_APPLICATION_ID,
            APP_REGISTRY_SCHEMA_VERSION,
            true,
        )
        .unwrap();

        connection
            .execute_batch(
                "DROP INDEX app_owner_notification_attempt_refunds_correlation_idx;
                 CREATE INDEX app_owner_notification_attempt_refunds_correlation_idx
                     ON app_owner_notification_attempt_refunds(correlation_id)
                     WHERE applied_at IS NOT NULL;",
            )
            .unwrap();
        assert!(matches!(
            verify_refund_schema_shape(
                &connection,
                APP_REGISTRY_APPLICATION_ID,
                APP_REGISTRY_SCHEMA_VERSION,
                true,
            ),
            Err(AppRegistryError::IncompatibleDatabase { .. })
        ));
        connection
            .execute_batch("DROP INDEX app_owner_notification_attempt_refunds_correlation_idx;")
            .unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V32).unwrap();

        connection
            .execute_batch(
                "DROP INDEX app_owner_notification_attempt_refunds_pending_idx;
                 CREATE INDEX app_owner_notification_attempt_refunds_pending_idx
                     ON app_owner_notification_attempt_refunds(created_at)
                     WHERE applied_at IS NOT NULL;",
            )
            .unwrap();
        assert!(matches!(
            verify_refund_schema_shape(
                &connection,
                APP_REGISTRY_APPLICATION_ID,
                APP_REGISTRY_SCHEMA_VERSION,
                true,
            ),
            Err(AppRegistryError::IncompatibleDatabase { .. })
        ));
    }

    #[test]
    fn historical_opened_registry_key_is_accepted_until_post_migration_rekey() {
        assert!(opened_registry_key_metadata_is_compatible(
            "previous-key",
            "active-key",
            "previous-key",
        ));
        assert!(opened_registry_key_metadata_is_compatible(
            "previous-key",
            "active-key",
            "active-key",
        ));
        assert!(!opened_registry_key_metadata_is_compatible(
            "previous-key",
            "active-key",
            "unrelated-key",
        ));
        assert!(!opened_registry_key_metadata_is_compatible(
            "active-key",
            "active-key",
            "previous-key",
        ));
    }

    fn scope(principal: &str, workspace: &str) -> AppScope {
        AppScope {
            principal: reference(principal),
            workspace: reference(workspace),
        }
    }

    fn seed_legacy_installation(connection: &Connection, scope: &AppScope) {
        connection
            .execute(
                "INSERT INTO app_package_revisions (
                     package_revision_ref, package_id, semantic_version, content_digest,
                     publisher_identity, dependency_lock_digest, record_json,
                     dependency_lock_json, created_at
                 ) VALUES (
                     'package-revision:legacy', 'app:legacy', '0.1.0', 'digest:legacy',
                     'publisher:legacy', 'digest:legacy-lock', ?1, ?1, ?2
                 )",
                params![b"{}".as_slice(), time(1).to_rfc3339()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO app_installations (
                     installation_id, principal, workspace, package_revision_ref,
                     lifecycle_status, lifecycle_generation, record_json,
                     created_at, updated_at
                 ) VALUES (
                     'install_legacy', ?1, ?2, 'package-revision:legacy',
                     'enabled', 1, ?3, ?4, ?4
                 )",
                params![
                    scope.principal.as_str(),
                    scope.workspace.as_str(),
                    b"{}".as_slice(),
                    time(1).to_rfc3339(),
                ],
            )
            .unwrap();
    }

    fn open_encrypted_historical_fixture(database_path: &Path, scope: &AppScope) -> Connection {
        let scope_key =
            app_data_scope_key_candidates(scope.principal.as_str(), scope.workspace.as_str())
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
        let connection = Connection::open_with_flags(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        configure_registry_cipher(&connection, &scope_key).unwrap();
        connection
    }

    pub fn background_scheduler_database_fixture(
    ) -> (TempDir, impl std::ops::DerefMut<Target = Connection>) {
        let root = canonical_tempdir();
        let scope = scope("scheduler-fixture", "default");
        let connection =
            open_registry_connection(&root.path().join("app_store.sqlite3"), &scope, true).unwrap();
        seed_legacy_installation(&connection, &scope);
        (root, connection)
    }

    fn remove_recurring_fixture_schema(connection: &Connection) {
        connection
            .execute_batch(
                "DROP TABLE app_recurring_task_heads;
            DROP TABLE app_recurring_occurrence_locators; DROP TABLE app_behavior_execution_state;",
            )
            .unwrap();
    }

    fn remove_keyset_fixture_schema(connection: &Connection) {
        remove_recurring_fixture_schema(connection);
        connection
            .execute_batch(
                "DROP TABLE app_data_cleanup_candidates; DROP TABLE app_data_cleanup_jobs;
            DROP TABLE app_keyset_cursors;
            DROP TRIGGER app_scalar_order_insert_guard; DROP TRIGGER app_scalar_order_update_guard;
            DROP INDEX app_scalar_order_asc_idx; DROP INDEX app_scalar_order_desc_idx;
            ALTER TABLE app_scalar_indexes DROP COLUMN order_key_asc;
            ALTER TABLE app_scalar_indexes DROP COLUMN order_key_desc;",
            )
            .unwrap();
    }

    #[test]
    fn recurring_registry_migrates_v36_preserving_installations_and_indexes() {
        let root = canonical_tempdir();
        let scope = scope("recurring-upgrade", "default");
        let path = root.path().join("app_store.sqlite3");
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        seed_legacy_installation(&connection, &scope);
        remove_recurring_fixture_schema(&connection);
        connection.pragma_update(None, "user_version", 36).unwrap();
        drop(connection);
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&connection, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        assert_eq!(connection.query_row("SELECT lifecycle_status FROM app_installations WHERE installation_id = 'install_legacy'", [], |row| row.get::<_, String>(0)).unwrap(), "enabled");
        verify_current_schema_objects(
            &connection,
            APP_REGISTRY_APPLICATION_ID,
            APP_REGISTRY_SCHEMA_VERSION,
        )
        .unwrap();
        let plan: String = connection.query_row("EXPLAIN QUERY PLAN SELECT installation_id FROM app_behavior_execution_state WHERE needs_observation = 1", [], |row| row.get(3)).unwrap();
        assert!(
            plan.contains("app_behavior_execution_observation_idx"),
            "{plan}"
        );
    }

    #[test]
    fn memory_grant_registry_migrates_v37_preserving_installations() {
        let root = canonical_tempdir();
        let scope = scope("memory-grant-upgrade", "default");
        let path = root.path().join("app_store.sqlite3");
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        seed_legacy_installation(&connection, &scope);
        connection
            .execute_batch("DROP TABLE app_memory_read_grant_heads;")
            .unwrap();
        connection.pragma_update(None, "user_version", 37).unwrap();
        drop(connection);
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&connection, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        assert_eq!(connection.query_row("SELECT lifecycle_status FROM app_installations WHERE installation_id = 'install_legacy'", [], |row| row.get::<_, String>(0)).unwrap(), "enabled");
        verify_current_schema_objects(
            &connection,
            APP_REGISTRY_APPLICATION_ID,
            APP_REGISTRY_SCHEMA_VERSION,
        )
        .unwrap();
    }

    #[test]
    fn age_cleanup_registry_migrates_v35_without_replaying_keyset_migration() {
        let root = canonical_tempdir();
        let scope = scope("cleanup-upgrade", "default");
        let path = root.path().join("app_store.sqlite3");
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        remove_recurring_fixture_schema(&connection);
        connection
            .execute_batch(
                "DROP TABLE app_data_cleanup_candidates; DROP TABLE app_data_cleanup_jobs;",
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 35).unwrap();
        drop(connection);
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&connection, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM app_data_cleanup_jobs", [], |row| row
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        verify_current_schema_objects(
            &connection,
            APP_REGISTRY_APPLICATION_ID,
            APP_REGISTRY_SCHEMA_VERSION,
        )
        .unwrap();
    }

    #[test]
    fn keyset_registry_migrates_v34_without_replaying_the_v34_event_migration() {
        let root = canonical_tempdir();
        let scope = scope("keyset-upgrade", "default");
        let path = root.path().join("app_store.sqlite3");
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        remove_keyset_fixture_schema(&connection);
        connection.pragma_update(None, "user_version", 34).unwrap();
        drop(connection);
        let connection = open_registry_connection_unpooled(&path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&connection, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM app_keyset_cursors", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        verify_current_schema_objects(
            &connection,
            APP_REGISTRY_APPLICATION_ID,
            APP_REGISTRY_SCHEMA_VERSION,
        )
        .unwrap();
    }

    #[test]
    fn app_runtime_concurrency_regression_owner_retry_migration_preserves_audit_history() {
        for retain_old_event in [false, true] {
            let root = canonical_tempdir();
            let fixture_scope = scope("scheduler-fixture", "default");
            // Historical files are prepared before the production pool sees
            // them, as at boot. Downgrading an already pooled live connection
            // correctly triggers its schema-substitution rejection instead.
            let connection = open_registry_connection_unpooled(
                &root.path().join("app_store.sqlite3"),
                &fixture_scope,
                true,
            )
            .unwrap();
            seed_legacy_installation(&connection, &fixture_scope);
            connection
                .execute_batch("DROP TABLE app_behavior_events;")
                .unwrap();
            // Use the actual historical production DDL, including its kind CHECK.
            let historical = APP_REGISTRY_SCHEMA_V23
                .split("CREATE TABLE app_behavior_events (")
                .nth(1)
                .unwrap()
                .split("ALTER TABLE app_behavior_heads")
                .next()
                .unwrap();
            connection
                .execute_batch(&format!("CREATE TABLE app_behavior_events ({historical}"))
                .unwrap();
            connection.execute_batch("INSERT INTO app_behavior_events VALUES(41, 'install_legacy', 'daily_digest', 'scan_fault', 'old_failure', '2026-09-10T00:00:00Z');
                INSERT INTO app_behavior_events VALUES(99, 'install_legacy', NULL, 'retired', 'old_retired', '2026-09-10T00:00:01Z');
                DELETE FROM app_behavior_events WHERE event_id=99;").unwrap();
            if !retain_old_event {
                connection
                    .execute("DELETE FROM app_behavior_events", [])
                    .unwrap();
            }
            remove_keyset_fixture_schema(&connection);
            connection.pragma_update(None, "user_version", 33).unwrap();
            assert!(connection.execute("INSERT INTO app_behavior_events(installation_id,kind,reason,observed_at) VALUES('install_legacy','retry_requested','owner_requested','2026-09-10T01:00:00Z')", []).is_err());
            drop(connection);
            let connection = open_registry_connection(
                &root.path().join("app_store.sqlite3"),
                &scope("scheduler-fixture", "default"),
                true,
            )
            .unwrap();
            assert_eq!(
                pragma_i32(&connection, "user_version").unwrap(),
                APP_REGISTRY_SCHEMA_VERSION
            );
            let retained: usize = connection.query_row("SELECT COUNT(*) FROM app_behavior_events WHERE event_id=41 AND kind='scan_fault' AND reason='old_failure'", [], |row| row.get(0)).unwrap();
            assert_eq!(retained, usize::from(retain_old_event));
            connection.execute("INSERT INTO app_behavior_events(installation_id,kind,reason,observed_at) VALUES('install_legacy','retry_requested','owner_requested','2026-09-10T01:00:00Z')", []).unwrap();
            assert_eq!(connection.last_insert_rowid(), 100);
            assert!(connection.execute("INSERT INTO app_behavior_events(installation_id,kind,reason,observed_at) VALUES('install_legacy','invalid_kind','owner_requested','2026-09-10T01:00:00Z')", []).is_err());
            assert_eq!(connection.query_row("SELECT count(*) FROM app_installations WHERE installation_id='install_legacy'", [], |row| row.get::<_, usize>(0)).unwrap(), 1);
        }
    }

    pub fn authenticated_scope(principal: &str, workspace: &str) -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            scope(principal, workspace),
            AppScopeBindingRef::parse("scope_1").unwrap(),
            reference("actor:owner"),
            reference("session:1"),
            AppRevision::new(1).unwrap(),
            time(0),
            time(30),
        )
        .unwrap()
    }

    fn test_package_lock(
        package: &crate::magician_v2::apps::manifest::AppPackageCandidate,
    ) -> AppPackageLock {
        let limits = AppPackageLimits::default();
        let registry_evidence = vec![
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Contract,
                reference("contract:magician_contract"),
                "1.2.0".to_owned(),
                reference("contract-revision:42"),
                AppRevision::new(42).unwrap(),
                b"contract-v1.2.0",
            )
            .unwrap(),
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Capability,
                reference("capability:content_search"),
                "1.4.3".to_owned(),
                reference("capability-revision:99"),
                AppRevision::new(99).unwrap(),
                b"content-search-v1.4.3",
            )
            .unwrap(),
        ];
        lock_app_package_dependencies(package, registry_evidence, &limits).unwrap()
    }

    const REGISTRY_PROCEDURE_BYTES: &[u8] = b"---\nname: summarize\nversion: 2.1.0\ndescription: Exact registry procedure.\nallowed-tools: capability:content_search\nmetadata:\n  magician:\n    skill_type: procedure\n---\nSummarize only the supplied app records.\n";

    fn registry_procedure_package_and_lock(
        procedure_bytes: &[u8],
    ) -> (
        crate::magician_v2::apps::manifest::AppPackageCandidate,
        AppPackageLock,
    ) {
        let limits = AppPackageLimits::default();
        let mut bundle = valid_bundle();
        let manifest = String::from_utf8(
            bundle
                .iter()
                .find(|member| member.path.as_str() == "SKILL.md")
                .unwrap()
                .bytes
                .clone(),
        )
        .unwrap()
        .replace(
            "        vendored_path: vendor/skills/summarize/SKILL.md\n",
            "",
        );
        bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap()
            .bytes = manifest.into_bytes();
        let package = build_app_package_candidate(bundle, &limits).unwrap();
        let mut evidence = vec![
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Contract,
                reference("contract:magician_contract"),
                "1.2.0".to_owned(),
                reference("contract-revision:42"),
                AppRevision::new(42).unwrap(),
                b"contract-v1.2.0",
            )
            .unwrap(),
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::Capability,
                reference("capability:content_search"),
                "1.4.3".to_owned(),
                reference("capability-revision:99"),
                AppRevision::new(99).unwrap(),
                b"content-search-v1.4.3",
            )
            .unwrap(),
        ];
        evidence.push(
            AppVerifiedRegistryDependency::from_trusted_registry_bytes(
                AppDependencyKind::ProcedureSkill,
                reference("skill:summarize"),
                "2.1.0".to_owned(),
                reference("skill-revision:summarize-21"),
                AppRevision::new(21).unwrap(),
                procedure_bytes,
            )
            .unwrap(),
        );
        let lock = lock_app_package_dependencies(&package, evidence, &limits).unwrap();
        (package, lock)
    }

    fn verified_registry_procedure(bytes: &[u8]) -> AppVerifiedRegistryProcedureRevision {
        AppVerifiedRegistryProcedureRevision::from_trusted_registry_bytes(
            reference("skill:summarize"),
            "2.1.0".to_owned(),
            reference("skill-revision:summarize-21"),
            AppRevision::new(21).unwrap(),
            bytes,
        )
        .unwrap()
    }

    fn test_package_revision(
        package: &crate::magician_v2::apps::manifest::AppPackageCandidate,
        lock: &AppPackageLock,
    ) -> AppPackageRevision {
        let manifest = package.manifest().manifest();
        AppPackageRevision {
            package_id: reference("app:learning-plan"),
            semantic_version: manifest.version.clone(),
            content_digest: package.bundle_digest().clone(),
            manifest_schema_version: manifest.metadata.magician.app_manifest_version.clone(),
            authoring_sdk_version: manifest.metadata.magician.app_sdk_version.clone(),
            publisher_identity: reference("publisher:local-owner"),
            source_kind: AppPackageSourceKind::LocalVibedev,
            compatibility: vec![AppCompatibilityRequirement {
                contract: super::super::models::AppName::parse("magician_contract").unwrap(),
                requirement: "1".to_owned(),
            }],
            requested_authority_digest: AppDigest::blake3(b"authority"),
            requested_data_policy_digest: AppDigest::blake3(b"policy"),
            dependency_lock_digest: lock.lock_digest().clone(),
            entity_schema_digest: canonical_entity_schema_digest(manifest).unwrap(),
            view_schema_digest: canonical_view_schema_digest(manifest).unwrap(),
            workflow_digest: AppDigest::blake3(b"workflows"),
            verification_attestation_ref: Some(reference("attestation:vibedev-1")),
            conformance_attestation_ref: reference("attestation:conformance-1"),
            created_at: time(1),
        }
    }

    #[test]
    fn canonical_package_revision_identity_includes_the_dependency_lock() {
        let package =
            build_app_package_candidate(valid_bundle(), &AppPackageLimits::default()).unwrap();
        let lock = test_package_lock(&package);
        let first = test_package_revision(&package, &lock);
        let mut changed = first.clone();
        changed.dependency_lock_digest = AppDigest::blake3(b"updated immutable dependency lock");

        assert_ne!(
            canonical_package_revision_ref(&first).unwrap(),
            canonical_package_revision_ref(&changed).unwrap()
        );
    }

    pub fn publication(
        artifact_workspace: &ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
        attempt_id: &str,
        installation_id: &str,
    ) -> AppReadyForReviewPublication {
        let limits = AppPackageLimits::default();
        let package = build_app_package_candidate(valid_bundle(), &limits).unwrap();
        let lock = test_package_lock(&package);
        let package_revision = test_package_revision(&package, &lock);
        let package_revision_ref = canonical_package_revision_ref(&package_revision).unwrap();
        let attempt = AppLifecycleAttempt {
            attempt_id: reference(attempt_id),
            kind: AppLifecycleAttemptKind::InitialInstall,
            installation_id: None,
            source_installation_generation: None,
            candidate_package_revision_ref: package_revision_ref.clone(),
            state: AppLifecycleAttemptState::ReadyForReview,
            conformance_attestation_ref: Some(reference("attestation:conformance-1")),
            permission_migration_diff_ref: Some(reference("diff:permission-1")),
            approval_ref: None,
            failure_code: None,
            created_at: time(1),
            updated_at: time(2),
        };
        let installation = AppInstallation {
            scope: scope(principal, workspace_name),
            installation_id: AppInstallationId::parse(installation_id).unwrap(),
            package_revision_ref,
            lifecycle: super::super::lifecycle::AppInstallationLifecycle::ready_for_review(),
            grant_revision: None,
            active_schema_revision: None,
            active_surface_revision: None,
            created_at: time(1),
            updated_at: time(2),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: None,
            purged_at: None,
        };
        let staged_package = materialize_test_staged_package(
            artifact_workspace,
            &scope(principal, workspace_name),
            package,
        );
        AppReadyForReviewPublication::from_verified_conformance(
            package_revision,
            staged_package,
            &lock,
            attempt,
            installation,
        )
        .unwrap()
    }

    pub fn publication_with_requested_policy_digest(
        artifact_workspace: &ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
        attempt_id: &str,
        installation_id: &str,
        requested_policy_digest: AppDigest,
    ) -> AppReadyForReviewPublication {
        let mut publication = publication(
            artifact_workspace,
            principal,
            workspace_name,
            attempt_id,
            installation_id,
        );
        publication.package_revision.requested_data_policy_digest = requested_policy_digest;
        publication
    }

    pub fn reviewable_update_publication(
        artifact_workspace: &ArtifactV2Workspace,
        requested_policy_digest: AppDigest,
        attempt_id: &str,
        source_generation: u64,
    ) -> AppReviewableRevisionPublication {
        let initial = publication_with_requested_policy_digest(
            artifact_workspace,
            "anonymous",
            "default",
            "attempt:unused-update-fixture",
            "install_1",
            requested_policy_digest,
        );
        let package_revision = initial.package_revision;
        let candidate = initial.staged_candidate;
        let lock = test_package_lock(&candidate);
        let package_revision_ref = canonical_package_revision_ref(&package_revision).unwrap();
        let staged_package = materialize_test_staged_package(
            artifact_workspace,
            &scope("anonymous", "default"),
            candidate,
        );
        let fixture_scope = scope("anonymous", "default");
        let connection =
            open_scoped_registry_for_write(artifact_workspace, &fixture_scope).unwrap();
        let source_fence = reviewable_revision_source_fence_blocking(
            &connection,
            &fixture_scope,
            &AppInstallationId::parse("install_1").unwrap(),
            AppLifecycleAttemptKind::Update,
        )
        .unwrap();
        assert_eq!(
            source_fence.source_installation_generation, source_generation,
            "the fixture must name the exact enabled generation parked by BeginUpdate"
        );
        let permission_migration_diff_ref = source_fence.permission_migration_diff_ref().unwrap();
        let attempt = AppLifecycleAttempt {
            attempt_id: reference(attempt_id),
            kind: AppLifecycleAttemptKind::Update,
            installation_id: Some(AppInstallationId::parse("install_1").unwrap()),
            source_installation_generation: Some(source_generation),
            candidate_package_revision_ref: package_revision_ref,
            state: AppLifecycleAttemptState::ReadyForReview,
            conformance_attestation_ref: Some(reference("attestation:conformance-1")),
            permission_migration_diff_ref: Some(permission_migration_diff_ref),
            approval_ref: None,
            failure_code: None,
            created_at: time(7),
            updated_at: time(8),
        };
        AppReviewableRevisionPublication::from_verified_conformance(
            package_revision,
            staged_package,
            &lock,
            attempt,
            source_fence,
        )
        .unwrap()
    }

    #[test]
    fn package_correlation_rejects_entity_and_view_schema_digest_substitution() {
        for corrupt_entity_digest in [true, false] {
            let temporary = canonical_tempdir();
            let workspace = ArtifactV2Workspace::new(temporary.path());
            let package =
                build_app_package_candidate(valid_bundle(), &AppPackageLimits::default()).unwrap();
            let lock = test_package_lock(&package);
            let mut revision = test_package_revision(&package, &lock);
            if corrupt_entity_digest {
                revision.entity_schema_digest = AppDigest::blake3(b"substituted-entity-schema");
            } else {
                revision.view_schema_digest = AppDigest::blake3(b"substituted-view-schema");
            }
            let staged = materialize_test_staged_package(
                &workspace,
                &scope("anonymous", "default"),
                package,
            );

            let result = correlate_staged_package(&revision, staged, &lock);
            assert!(matches!(
                result,
                Err(AppRegistryError::InvalidPublication(
                    "package revision entity schema digest does not match the admitted manifest"
                        | "package revision view schema digest does not match the admitted \
                           manifest"
                ))
            ));
        }
    }

    #[tokio::test]
    async fn exact_installation_batch_deduplicates_and_keeps_scopes_isolated() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        service
            .publish_ready_for_review(
                &authenticated,
                publication(
                    &service.workspace,
                    "anonymous",
                    "default",
                    "attempt:batch",
                    "install_batch",
                ),
                time(3),
            )
            .await
            .unwrap();
        let id = AppInstallationId::parse("install_batch").unwrap();
        let missing = AppInstallationId::parse("install_missing").unwrap();
        let rows = service
            .installations_by_ids(&authenticated, &[missing, id.clone(), id.clone()], time(3))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].installation_id, id);
        assert_eq!(rows[0].scope, *authenticated.scope());
        let other = authenticated_scope("anonymous", "unopened");
        assert!(service
            .installations_by_ids(&other, &[id], time(3))
            .await
            .unwrap()
            .is_empty());
        assert!(!temporary.path().join("scopes/anonymous/unopened").exists());
    }

    #[tokio::test]
    async fn initial_publication_is_atomic_readable_and_idempotent() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let first_publication = publication(
            &service.workspace,
            "anonymous",
            "default",
            "attempt:1",
            "install_1",
        );
        let package_ref = first_publication.package_revision_ref().clone();

        let first = service
            .publish_ready_for_review(&authenticated, first_publication, time(3))
            .await
            .unwrap();
        assert_eq!(first.outcome, AppRegistryPublicationOutcome::Created);
        let binding = fs::read(
            temporary
                .path()
                .join("scopes/anonymous/default/apps/scope-binding.json"),
        )
        .unwrap();
        assert!(String::from_utf8(binding)
            .unwrap()
            .contains("\"principal\":\"anonymous\""));
        assert_eq!(
            service
                .installation(&authenticated, &first.installation_id, time(3))
                .await
                .unwrap()
                .unwrap()
                .lifecycle
                .status,
            AppInstallationStatus::ReadyForReview
        );
        assert!(service
            .lifecycle_attempt(&authenticated, &first.attempt_id, time(3))
            .await
            .unwrap()
            .is_some());
        let package_revision = service
            .package_revision(&authenticated, &package_ref, time(3))
            .await
            .unwrap()
            .unwrap();
        let dependency_lock = service
            .package_dependency_lock(&authenticated, &package_ref, time(3))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            dependency_lock.lock_digest(),
            &package_revision.dependency_lock_digest
        );
        let connection = open_registry_connection(
            &service.workspace.app_store_db_path("anonymous", "default"),
            authenticated.scope(),
            false,
        )
        .unwrap();
        let directory_metadata: Vec<u8> = connection
            .query_row(
                "SELECT metadata_json FROM app_package_directory_metadata
                  WHERE package_revision_ref = ?1",
                params![package_ref.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let directory_metadata: AppPackageDirectoryMetadata =
            decode_app_contract(&directory_metadata, &AppContractLimits::default()).unwrap();
        assert_eq!(directory_metadata.name.as_str(), "learning-plan");
        drop(connection);

        let replay = service
            .publish_ready_for_review(
                &authenticated,
                publication(
                    &service.workspace,
                    "anonymous",
                    "default",
                    "attempt:1",
                    "install_1",
                ),
                time(3),
            )
            .await
            .unwrap();
        assert_eq!(
            replay.outcome,
            AppRegistryPublicationOutcome::AlreadyPresent
        );
    }

    #[tokio::test]
    async fn late_identity_conflict_rolls_back_earlier_inserts() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        service
            .publish_ready_for_review(
                &authenticated,
                publication(
                    &service.workspace,
                    "anonymous",
                    "default",
                    "attempt:existing",
                    "install_1",
                ),
                time(3),
            )
            .await
            .unwrap();

        let mut conflicting = publication(
            &service.workspace,
            "anonymous",
            "default",
            "attempt:new",
            "install_1",
        );
        conflicting.package_revision.package_id = reference("app:another-learning-plan");
        let new_package_ref =
            canonical_package_revision_ref(&conflicting.package_revision).unwrap();
        conflicting.package_revision_ref = new_package_ref.clone();
        conflicting.attempt.candidate_package_revision_ref = new_package_ref.clone();
        conflicting.installation.package_revision_ref = new_package_ref.clone();
        let conflict_package_ref = conflicting.package_revision_ref().clone();
        let connection = open_registry_connection(
            &service.workspace.app_store_db_path("anonymous", "default"),
            authenticated.scope(),
            true,
        )
        .unwrap();
        connection
            .execute(
                "UPDATE app_installations SET record_json = X'00' WHERE installation_id = \
                 'install_1'",
                [],
            )
            .unwrap();
        drop(connection);

        let error = service
            .publish_ready_for_review(&authenticated, conflicting, time(3))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppRegistryError::IdentityConflict {
                entity: "installation",
                ..
            }
        ));
        let connection = open_registry_connection(
            &service.workspace.app_store_db_path("anonymous", "default"),
            authenticated.scope(),
            false,
        )
        .unwrap();
        let new_attempts: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_lifecycle_attempts WHERE attempt_id = 'attempt:new'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_attempts, 0);
        let new_packages: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_package_revisions WHERE package_revision_ref = ?1",
                params![conflict_package_ref.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_packages, 0);
    }

    #[tokio::test]
    async fn authenticated_scope_is_required_and_cross_scope_rows_are_concealed() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let owner = authenticated_scope("owner-a", "default");
        let other = authenticated_scope("owner-b", "default");
        let created = service
            .publish_ready_for_review(
                &owner,
                publication(
                    &service.workspace,
                    "owner-a",
                    "default",
                    "attempt:a",
                    "install_a",
                ),
                time(3),
            )
            .await
            .unwrap();
        assert!(service
            .installation(&other, &created.installation_id, time(3))
            .await
            .unwrap()
            .is_none());

        let wrong_scope = publication(
            &service.workspace,
            "owner-a",
            "default",
            "attempt:b",
            "install_b",
        );
        assert!(matches!(
            service
                .publish_ready_for_review(&other, wrong_scope, time(3))
                .await,
            Err(AppRegistryError::InvalidPublication(_))
        ));
    }

    #[tokio::test]
    async fn missing_scope_read_is_lazy_and_does_not_create_app_storage() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");

        assert!(service
            .installation(
                &authenticated,
                &AppInstallationId::parse("install_missing").unwrap(),
                time(3),
            )
            .await
            .unwrap()
            .is_none());
        assert!(!temporary.path().join("scopes").exists());
    }

    #[test]
    fn app_data_encryption_fresh_registry_hides_pages_and_rejects_cross_scope_swap() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let owner = authenticated_scope("owner-a", "default");
        let other = authenticated_scope("owner-b", "default");

        let owner_connection = open_scoped_registry_for_write(&workspace, owner.scope()).unwrap();
        owner_connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(owner_connection);
        let owner_path = workspace.app_store_db_path("owner-a", "default");
        let bytes = fs::read(&owner_path).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3\0"));
        assert!(!bytes
            .windows(b"owner-a".len())
            .any(|window| window == b"owner-a"));

        let other_connection = open_scoped_registry_for_write(&workspace, other.scope()).unwrap();
        other_connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(other_connection);
        let other_path = workspace.app_store_db_path("owner-b", "default");
        fs::copy(&owner_path, &other_path).unwrap();
        for sidecar in sqlite_owned_paths(&other_path).into_iter().skip(1) {
            let _ = fs::remove_file(sidecar);
        }

        assert!(matches!(
            open_registry_connection(&other_path, other.scope(), false),
            Err(AppRegistryError::AtRestEncryptionFailed)
        ));

        let tampered_path = temporary.path().join("tampered-app-store.sqlite3");
        let mut tampered_bytes = bytes;
        tampered_bytes[64] ^= 0x80;
        fs::write(&tampered_path, tampered_bytes).unwrap();
        assert!(matches!(
            open_registry_connection(&tampered_path, owner.scope(), false),
            Err(AppRegistryError::AtRestEncryptionFailed)
        ));
    }

    #[test]
    fn app_data_encryption_atomically_rewrites_version_nineteen_plaintext_registry() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let authenticated = authenticated_scope("anonymous", "default");
        let database_path = workspace.app_store_db_path("anonymous", "default");
        ensure_registry_parent_tree(workspace.base_root(), &database_path).unwrap();
        let plaintext = Connection::open(&database_path).unwrap();
        // A REAL v19 database, not a hand-rolled subset of one.
        //
        // This fixture used to create just the `app_workflow_control_*` objects
        // that the v20 migration drops. Every later migration that touches an
        // object v19 also had then failed -- v23 drops
        // `app_resource_tree_identity_update_guard`, created back in v8, and a
        // partial fixture never had it. Applying the real schema prefix means
        // the fixture cannot drift away from what a v19 deployment holds, and a
        // new migration touching older objects does not silently need this list
        // extended.
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
            APP_REGISTRY_SCHEMA_V10,
            APP_REGISTRY_SCHEMA_V11,
            APP_REGISTRY_SCHEMA_V12,
            APP_REGISTRY_SCHEMA_V13,
            APP_REGISTRY_SCHEMA_V14,
            APP_REGISTRY_SCHEMA_V15,
            APP_REGISTRY_SCHEMA_V16,
            APP_REGISTRY_SCHEMA_V17,
            APP_REGISTRY_SCHEMA_V18,
            APP_REGISTRY_SCHEMA_V19,
        ] {
            plaintext.execute_batch(schema).unwrap();
        }
        // The scope binding the original fixture wrote by hand. A registry
        // without it reads as a different scope and the open is refused with
        // `ScopeCollision`.
        plaintext
            .execute_batch("INSERT INTO app_registry_scope VALUES (1, 'anonymous', 'default');")
            .unwrap();

        plaintext
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        plaintext.pragma_update(None, "user_version", 19).unwrap();
        drop(plaintext);
        assert!(registry_has_plaintext_header(&database_path).unwrap());

        let migrated =
            open_registry_connection(&database_path, authenticated.scope(), true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        assert_eq!(
            migrated
                .query_row(
                    "SELECT algorithm FROM app_data_encryption_metadata WHERE singleton=1",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            APP_REGISTRY_CIPHER_ALGORITHM
        );
        drop(migrated);
        assert!(!registry_has_plaintext_header(&database_path).unwrap());
    }

    #[tokio::test]
    async fn app_data_encryption_rotation_rekeys_an_existing_registry() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let service = AppRegistryService::new(workspace.clone());
        let authenticated = authenticated_scope("rotation-owner", "default");
        let original = open_scoped_registry_for_write(&workspace, authenticated.scope()).unwrap();
        let original_key_id = load_app_data_encryption_key_id(&original).unwrap();
        drop(original);

        let rotated_key_id = service
            .rotate_at_rest_key_generation(&authenticated, time(3))
            .await
            .unwrap();
        assert_ne!(rotated_key_id, original_key_id);

        let reopened = open_registry_connection(
            &workspace.app_store_db_path("rotation-owner", "default"),
            authenticated.scope(),
            false,
        )
        .unwrap();
        assert_eq!(
            load_app_data_encryption_key_id(&reopened).unwrap(),
            rotated_key_id
        );
    }

    #[tokio::test]
    async fn immutable_procedure_publication_is_exact_replayable_and_scope_bound() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let (_, lock) = registry_procedure_package_and_lock(REGISTRY_PROCEDURE_BYTES);
        let procedure = verified_registry_procedure(REGISTRY_PROCEDURE_BYTES);

        let created = service
            .publish_immutable_procedure_revision(&authenticated, &procedure, time(3))
            .await
            .unwrap();
        assert_eq!(created.outcome, AppRegistryPublicationOutcome::Created);
        let replay = service
            .publish_immutable_procedure_revision(&authenticated, &procedure, time(4))
            .await
            .unwrap();
        assert_eq!(
            replay.outcome,
            AppRegistryPublicationOutcome::AlreadyPresent
        );

        let procedures = service
            .locked_procedure_revisions(
                &authenticated,
                &lock,
                &[reference("skill:summarize")],
                time(5),
            )
            .await
            .unwrap();
        assert_eq!(procedures.len(), 1);
        assert_eq!(
            procedures[0].skill_document_bytes(),
            REGISTRY_PROCEDURE_BYTES
        );

        let wrong_scope = authenticated_scope("anonymous", "other");
        assert!(matches!(
            service
                .locked_procedure_revisions(
                    &wrong_scope,
                    &lock,
                    &[reference("skill:summarize")],
                    time(5),
                )
                .await,
            Err(AppRegistryError::MissingRecord {
                entity: "scope registry",
                ..
            })
        ));
        assert!(!temporary.path().join("scopes/anonymous/other").exists());
    }

    #[tokio::test]
    async fn immutable_procedure_identity_collision_and_byte_tamper_fail_closed() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let procedure = verified_registry_procedure(REGISTRY_PROCEDURE_BYTES);
        service
            .publish_immutable_procedure_revision(&authenticated, &procedure, time(3))
            .await
            .unwrap();

        let changed = verified_registry_procedure(
            b"---\nname: summarize\nversion: 2.1.0\ndescription: Rewritten registry procedure.\nmetadata:\n  magician:\n    skill_type: procedure\n---\nUse different instructions.\n",
        );
        assert!(matches!(
            service
                .publish_immutable_procedure_revision(&authenticated, &changed, time(4))
                .await,
            Err(AppRegistryError::IdentityConflict { .. })
        ));

        let digest = procedure.content_digest().to_string();
        let tamper = service
            .execute_scoped_test_write(&authenticated, &time(5), move |connection, _| {
                connection.execute(
                    "UPDATE app_skill_revision_blobs
                        SET skill_document = ?2
                      WHERE content_digest = ?1",
                    params![digest, b"tampered".as_slice()],
                )?;
                Ok(())
            })
            .await;
        assert!(matches!(tamper, Err(AppRegistryError::Sqlite(_))));

        let collision_root = canonical_tempdir();
        let collision_service =
            AppRegistryService::new(ArtifactV2Workspace::new(collision_root.path()));
        let collision_scope = authenticated_scope("anonymous", "default");
        let claimed_digest = procedure.content_digest().to_string();
        collision_service
            .execute_scoped_test_write(&collision_scope, &time(5), move |connection, _| {
                connection.execute(
                    "INSERT INTO app_skill_revision_blobs (
                         content_digest, byte_count, skill_document, created_at
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        claimed_digest,
                        8_i64,
                        b"collision".as_slice(),
                        time(5).to_rfc3339(),
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            collision_service
                .publish_immutable_procedure_revision(&collision_scope, &procedure, time(6))
                .await,
            Err(AppRegistryError::IdentityConflict {
                entity: "immutable procedure content digest",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn locked_procedure_read_rejects_missing_and_oversized_revision_bytes() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let (_, lock) = registry_procedure_package_and_lock(REGISTRY_PROCEDURE_BYTES);
        service
            .execute_scoped_test_write(&authenticated, &time(2), |_, _| Ok(()))
            .await
            .unwrap();
        assert!(matches!(
            service
                .locked_procedure_revisions(
                    &authenticated,
                    &lock,
                    &[reference("skill:summarize")],
                    time(3),
                )
                .await,
            Err(AppRegistryError::MissingRecord {
                entity: "immutable procedure revision",
                ..
            })
        ));

        let mut oversized = REGISTRY_PROCEDURE_BYTES.to_vec();
        oversized.resize(
            tool_runtime_core::manifest_parser::MAX_SKILL_MARKDOWN_BYTES + 1,
            b'x',
        );
        let (_, oversized_lock) = registry_procedure_package_and_lock(&oversized);
        let digest = AppDigest::blake3(&oversized);
        let byte_count = i64::try_from(oversized.len()).unwrap();
        service
            .execute_scoped_test_write(&authenticated, &time(4), move |connection, _| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                transaction.execute(
                    "INSERT INTO app_skill_revision_blobs (
                         content_digest, byte_count, skill_document, created_at
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![digest.as_str(), byte_count, oversized, time(4).to_rfc3339(),],
                )?;
                transaction.execute(
                    "INSERT INTO app_skill_revisions (
                         immutable_revision_ref, dependency_ref, semantic_version,
                         revision, content_digest, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        "skill-revision:summarize-21",
                        "skill:summarize",
                        "2.1.0",
                        21_i64,
                        digest.as_str(),
                        time(4).to_rfc3339(),
                    ],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            service
                .locked_procedure_revisions(
                    &authenticated,
                    &oversized_lock,
                    &[reference("skill:summarize")],
                    time(5),
                )
                .await,
            Err(AppRegistryError::InvalidControlPlane(_))
        ));
    }

    #[test]
    fn version_sixteen_registry_migrates_through_distinct_contribution_journals() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
            APP_REGISTRY_SCHEMA_V10,
            APP_REGISTRY_SCHEMA_V11,
            APP_REGISTRY_SCHEMA_V12,
            APP_REGISTRY_SCHEMA_V13,
            APP_REGISTRY_SCHEMA_V14,
            APP_REGISTRY_SCHEMA_V15,
            APP_REGISTRY_SCHEMA_V16,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace) VALUES (1, ?1, \
                 ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 16).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        for table in [
            "app_memory_contribution_outbox",
            "app_memory_contribution_sources",
            "app_memory_invalidation_outbox",
            "app_memory_contribution_heads",
            "app_memory_contribution_terminal",
            "app_memory_invalidation_terminal",
            "app_contribution_frequency_buckets",
            "app_contribution_frequency_compaction_heads",
            "app_retrieval_delivery_outbox",
            "app_retrieval_projection_heads",
            "app_retrieval_delivery_terminal",
            "app_retrieval_destination_ack_high_water",
            "app_retrieval_compaction_high_water",
        ] {
            let present: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing contribution table {table}");
        }
    }

    #[test]
    fn version_seventeen_registry_adds_inert_retrieval_and_frequency_owners() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
            APP_REGISTRY_SCHEMA_V10,
            APP_REGISTRY_SCHEMA_V11,
            APP_REGISTRY_SCHEMA_V12,
            APP_REGISTRY_SCHEMA_V13,
            APP_REGISTRY_SCHEMA_V14,
            APP_REGISTRY_SCHEMA_V15,
            APP_REGISTRY_SCHEMA_V16,
            APP_REGISTRY_SCHEMA_V17,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace) VALUES (1, ?1, \
                 ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 17).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        for table in [
            "app_contribution_frequency_buckets",
            "app_contribution_frequency_compaction_heads",
            "app_retrieval_delivery_outbox",
            "app_retrieval_projection_heads",
            "app_retrieval_delivery_terminal",
            "app_retrieval_destination_ack_high_water",
            "app_retrieval_compaction_high_water",
        ] {
            let present: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing V18 table {table}");
        }
    }

    #[test]
    fn v33_cancellation_migration_preserves_claimed_heads_and_prepared_pauses() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V12).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V19).unwrap();
        connection
            .execute_batch(
                "INSERT INTO app_workflow_control_blobs VALUES
               ('task','exec','pause',1,'digest1',3,x'000102','now'),
               ('task','exec','pause',2,'digest2',3,x'030405','now');
             INSERT INTO app_workflow_control_heads VALUES
               ('task','exec','pause',1,'digest1','claimed','claim','later',7,'now');
             INSERT INTO app_workflow_control_prepared VALUES
               ('task','exec','pause',2,'digest2',7,'claimed','claim','proposal','now');",
            )
            .unwrap();
        let cancellation_blob = "INSERT INTO app_workflow_control_blobs VALUES
            ('task','exec','action_cancellation',1,'cancel',3,x'060708','now')";
        assert!(connection.execute(cancellation_blob, []).is_err());
        connection.execute_batch(APP_REGISTRY_SCHEMA_V33).unwrap();
        let retained: (Vec<u8>, Vec<u8>, String, String, i64, String) = connection.query_row(
            "SELECT b.sealed_blob, pblob.sealed_blob, h.lifecycle_state, h.claim_ref,
                    h.revision, p.proposal_ref
               FROM app_workflow_control_heads h
               JOIN app_workflow_control_blobs b USING(task_id,execution_id,control_kind,generation,content_digest)
               JOIN app_workflow_control_prepared p USING(task_id,execution_id,control_kind)
               JOIN app_workflow_control_blobs pblob
                 ON pblob.task_id=p.task_id AND pblob.execution_id=p.execution_id
                AND pblob.control_kind=p.control_kind AND pblob.generation=p.generation
                AND pblob.content_digest=p.content_digest",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        ).unwrap();
        assert_eq!(
            retained,
            (
                vec![0, 1, 2],
                vec![3, 4, 5],
                "claimed".into(),
                "claim".into(),
                7,
                "proposal".into()
            )
        );
        connection.execute(cancellation_blob, []).unwrap();
        connection
            .execute_batch(
                "INSERT INTO app_workflow_control_heads VALUES
            ('task','exec','action_cancellation',1,'cancel','active',NULL,NULL,1,'now');",
            )
            .unwrap();
        for rejected in [
            "UPDATE app_workflow_control_blobs SET sealed_blob=x'00'",
            "DELETE FROM app_workflow_control_blobs",
            "DELETE FROM app_workflow_control_heads",
            "UPDATE app_workflow_control_heads SET revision=revision+2",
        ] {
            assert!(connection.execute(rejected, []).is_err(), "{rejected}");
        }
        let foreign_key_errors: i64 = connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(foreign_key_errors, 0);
    }

    #[test]
    fn version_eighteen_registry_migrates_the_interactive_stop_control_kind() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let app_scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
            APP_REGISTRY_SCHEMA_V10,
            APP_REGISTRY_SCHEMA_V11,
            APP_REGISTRY_SCHEMA_V12,
            APP_REGISTRY_SCHEMA_V13,
            APP_REGISTRY_SCHEMA_V14,
            APP_REGISTRY_SCHEMA_V15,
            APP_REGISTRY_SCHEMA_V16,
            APP_REGISTRY_SCHEMA_V17,
            APP_REGISTRY_SCHEMA_V18,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace) VALUES (1, ?1, \
                 ?2)",
                params![app_scope.principal.as_str(), app_scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 18).unwrap();
        drop(connection);

        let service = AppRegistryService::new(workspace);
        let migrated = service
            .publish_workflow_control_generation_sync(
                app_scope.clone(),
                "interactive-stop-task",
                "interactive-stop-execution",
                AppWorkflowControlKind::InteractiveStop,
                b"payload-free-stop-receipt".to_vec(),
                time(1),
            )
            .unwrap();
        assert_eq!(migrated.generation(), 1);
        let current = service
            .current_workflow_control_sync(
                app_scope.clone(),
                "interactive-stop-task",
                "interactive-stop-execution",
                AppWorkflowControlKind::InteractiveStop,
            )
            .unwrap()
            .unwrap();
        assert_eq!(current.content_digest(), migrated.content_digest());
        let run_state = service
            .publish_workflow_control_generation_sync(
                app_scope.clone(),
                "interactive-stop-task",
                "interactive-stop-execution",
                AppWorkflowControlKind::RunState,
                b"current-run-state".to_vec(),
                time(1),
            )
            .unwrap();

        let mut connection =
            open_scoped_registry_for_write(&service.workspace, &app_scope).unwrap();
        let stale = publish_workflow_control_generation_blocking(
            &mut connection,
            &app_scope,
            "interactive-stop-task",
            "interactive-stop-execution",
            AppWorkflowControlKind::InteractiveStop,
            b"different-stop".to_vec(),
            &[],
            None,
            None,
            Some(None),
            Some(run_state.content_digest().clone()),
            time(2),
        );
        assert!(matches!(
            stale,
            Err(AppRegistryError::CompareAndSwapLost(
                "protected workflow control head"
            ))
        ));
        let stale_run_state = publish_workflow_control_generation_blocking(
            &mut connection,
            &app_scope,
            "interactive-stop-task",
            "interactive-stop-execution",
            AppWorkflowControlKind::InteractiveStop,
            b"different-stop".to_vec(),
            &[],
            None,
            None,
            Some(Some(current.content_digest().clone())),
            Some(AppDigest::blake3(b"substituted-run-state")),
            time(2),
        );
        assert!(matches!(
            stale_run_state,
            Err(AppRegistryError::CompareAndSwapLost(
                "protected workflow run-state head"
            ))
        ));
        let next = publish_workflow_control_generation_blocking(
            &mut connection,
            &app_scope,
            "interactive-stop-task",
            "interactive-stop-execution",
            AppWorkflowControlKind::InteractiveStop,
            b"different-stop".to_vec(),
            &[],
            None,
            None,
            Some(Some(current.content_digest().clone())),
            Some(run_state.content_digest().clone()),
            time(2),
        )
        .unwrap();
        assert_eq!(next.generation(), 2);
    }

    #[test]
    fn version_ten_registry_migrates_to_write_once_procedure_revisions() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
            APP_REGISTRY_SCHEMA_V10,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 10).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        for table in ["app_skill_revision_blobs", "app_skill_revisions"] {
            let present: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                      WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing {table}");
        }
    }

    #[test]
    fn version_fifteen_registry_migrates_discovery_index_to_live_rows() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
            APP_REGISTRY_SCHEMA_V10,
            APP_REGISTRY_SCHEMA_V11,
            APP_REGISTRY_SCHEMA_V12,
            APP_REGISTRY_SCHEMA_V13,
            APP_REGISTRY_SCHEMA_V14,
            APP_REGISTRY_SCHEMA_V15,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 15).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        let mut statement = migrated
            .prepare("PRAGMA index_info(app_installations_package_discovery_idx)")
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap();
        let columns = rows.collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(
            columns.iter().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "package_revision_ref",
                "principal",
                "workspace",
                "lifecycle_status",
                "installation_id",
            ]
        );
    }

    #[test]
    fn populated_version_ten_with_incomplete_resource_baselines_fails_before_v11_mutation() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        seed_legacy_installation(&connection, &scope);
        connection
            .execute(
                "INSERT INTO app_resource_periods(
                    installation_id, installation_generation, period_ref, revision,
                    created_at, updated_at
                 ) VALUES ('install_legacy', 1, 'period:legacy-v10', 1, ?1, ?1)",
                params![time(1).to_rfc3339()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO app_resource_trees(
                    budget_ledger_ref, installation_id, installation_generation,
                    root_execution_id, period_ref, admitted_period_revision, lane,
                    identity_json, last_event_sequence, evaluated_at_elapsed_ms,
                    created_at, updated_at
                 ) VALUES (
                    'ledger:legacy-v10', 'install_legacy', 1,
                    'execution:legacy-v10', 'period:legacy-v10', 1, 'foreground',
                    X'7B7D', 1, 0, ?1, ?1
                 )",
                params![time(1).to_rfc3339()],
            )
            .unwrap();
        // Reproduce the already-materialized v10 shape: ALTER TABLE leaves
        // the new immutable baselines NULL for legacy rows, and triggers do not
        // retroactively validate them.
        connection.execute_batch(APP_REGISTRY_SCHEMA_V10).unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 10).unwrap();
        drop(connection);

        let error = match open_registry_connection(&database_path, &scope, true) {
            Ok(_) => panic!("incomplete v10 resource baselines must not migrate implicitly"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AppRegistryError::IncompleteResourceBaselinesRequireExplicitMigration { tree_count: 1 }
        ));

        let unchanged = open_encrypted_historical_fixture(&database_path, &scope);
        assert_eq!(pragma_i32(&unchanged, "user_version").unwrap(), 10);
        let v11_tables: i64 = unchanged
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                  WHERE type = 'table'
                    AND name IN ('app_skill_revision_blobs', 'app_skill_revisions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v11_tables, 0, "v11 schema must roll back atomically");
        let incomplete: i64 = unchanged
            .query_row(
                "SELECT COUNT(*) FROM app_resource_trees
                  WHERE period_ends_at_elapsed_ms IS NULL OR package_bytes IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(incomplete, 1, "failed migration must not invent baselines");
    }

    #[test]
    fn version_one_registry_migrates_additively_without_rewriting_base_rows() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA).unwrap();
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        let v2_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_grant_revisions', 'app_schema_revisions',
                    'app_surface_bindings', 'app_installation_approvals',
                    'app_lifecycle_outbox'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v2_tables, 5);
        let v3_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_record_revisions', 'app_record_heads',
                    'app_scalar_indexes', 'app_text_search',
                    'app_mutation_receipts', 'app_installation_sequences',
                    'app_dataset_generations', 'app_query_cursors',
                    'app_storage_usage', 'app_resource_usage_projection',
                    'app_entity_outbox', 'app_migration_runs'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v3_tables, 12);
        let v4_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_data_import_receipts', 'app_purge_receipts',
                    'app_retention_policies', 'app_retention_runs'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v4_tables, 4);
        let v7_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_surface_generations',
                    'app_surface_generation_members'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v7_tables, 2);
        let v8_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_resource_periods', 'app_resource_trees',
                    'app_resource_tree_events'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v8_tables, 3);
        let v9_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_package_directory_metadata',
                    'app_package_directory_actions',
                    'app_directory_state', 'app_directory_pins'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v9_tables, 4);
        let v10_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_resource_recovery_evidence',
                    'app_resource_retired_trees'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v10_tables, 2);
        let v11_tables: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema
                 WHERE type = 'table' AND name IN (
                    'app_skill_revision_blobs', 'app_skill_revisions'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v11_tables, 2);
        verify_scope_binding(&migrated, &scope).unwrap();
    }

    #[test]
    fn version_nine_registry_migrates_resource_lifecycle_without_rewriting_directory_rows() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 9).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        for table in [
            "app_resource_recovery_evidence",
            "app_resource_retired_trees",
        ] {
            let present: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing {table}");
        }
        let resource_period_columns: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('app_resource_periods')
                  WHERE name IN ('admissions_closed_at', 'retention_until')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(resource_period_columns, 2);
        let resource_tree_columns: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('app_resource_trees')
                  WHERE name IN (
                    'terminal_state_json', 'period_ends_at_elapsed_ms', 'package_bytes'
                  )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(resource_tree_columns, 3);
    }

    #[test]
    fn populated_version_nine_resource_authority_fails_closed_before_v10_mutation() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
            APP_REGISTRY_SCHEMA_DIRECTORY,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        seed_legacy_installation(&connection, &scope);
        connection
            .execute(
                "INSERT INTO app_resource_periods(
                    installation_id, installation_generation, period_ref, revision,
                    created_at, updated_at
                 ) VALUES ('install_legacy', 1, 'period:legacy', 1, ?1, ?1)",
                params![time(1).to_rfc3339()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO app_resource_trees(
                    budget_ledger_ref, installation_id, installation_generation,
                    root_execution_id, period_ref, admitted_period_revision, lane,
                    identity_json, last_event_sequence, evaluated_at_elapsed_ms,
                    created_at, updated_at
                 ) VALUES (
                    'ledger:legacy', 'install_legacy', 1, 'execution:legacy',
                    'period:legacy', 1, 'foreground', X'7B7D', 1, 0, ?1, ?1
                 )",
                params![time(1).to_rfc3339()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 9).unwrap();
        drop(connection);

        let error = match open_registry_connection(&database_path, &scope, true) {
            Ok(_) => panic!("populated legacy resource authority must not migrate implicitly"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AppRegistryError::LegacyResourceTreesRequireExplicitMigration { tree_count: 1 }
        ));

        let unchanged = open_encrypted_historical_fixture(&database_path, &scope);
        assert_eq!(pragma_i32(&unchanged, "user_version").unwrap(), 9);
        let v10_baseline_columns: i64 = unchanged
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('app_resource_trees')
                  WHERE name IN ('period_ends_at_elapsed_ms', 'package_bytes')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(v10_baseline_columns, 0);
    }

    #[test]
    fn version_eight_registry_migrates_to_directory_read_models() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        for schema in [
            APP_REGISTRY_SCHEMA,
            APP_REGISTRY_SCHEMA_V2,
            APP_REGISTRY_SCHEMA_V3,
            APP_REGISTRY_SCHEMA_V4,
            APP_REGISTRY_SCHEMA_V5,
            APP_REGISTRY_SCHEMA_V6,
            APP_REGISTRY_SCHEMA_V7,
            APP_REGISTRY_SCHEMA_V8,
        ] {
            connection.execute_batch(schema).unwrap();
        }
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 8).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        for table in [
            "app_package_directory_metadata",
            "app_package_directory_actions",
            "app_directory_state",
            "app_directory_pins",
        ] {
            let present: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing {table}");
        }
    }

    #[test]
    fn version_seven_registry_migrates_to_one_canonical_resource_authority() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V2).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V3).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V4).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V5).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V6).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V7).unwrap();
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 7).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        for table in [
            "app_resource_periods",
            "app_resource_trees",
            "app_resource_tree_events",
        ] {
            let present: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing {table}");
        }
    }

    #[test]
    fn version_six_registry_migrates_to_complete_surface_generations() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V2).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V3).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V4).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V5).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V6).unwrap();
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 6).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        assert_eq!(
            pragma_i32(&migrated, "user_version").unwrap(),
            APP_REGISTRY_SCHEMA_VERSION
        );
        for table in ["app_surface_generations", "app_surface_generation_members"] {
            let present: i64 = migrated
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "missing {table}");
        }
    }

    #[test]
    fn version_four_migration_redacts_legacy_purge_selection_and_preserves_its_kind() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V2).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V3).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V4).unwrap();
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        seed_legacy_installation(&connection, &scope);
        for (receipt_ref, approval_ref, selection, digest) in [
            (
                "receipt:legacy-whole",
                "approval:legacy-whole",
                serde_json::json!("whole_installation"),
                AppDigest::blake3(b"whole-selection"),
            ),
            (
                "receipt:legacy-records",
                "approval:legacy-records",
                serde_json::json!({
                    "records": {"entity_name": "item", "record_ids": ["private_record_id"]}
                }),
                AppDigest::blake3(b"record-selection"),
            ),
        ] {
            let record = serde_json::json!({
                "protocol_version": 1,
                "selection": selection,
                "unrelated": "preserved"
            });
            connection
                .execute(
                    "INSERT INTO app_purge_receipts (
                         receipt_ref, installation_id, approval_ref, preview_digest,
                         selection_digest, state, record_json, started_at, committed_at
                     ) VALUES (?1, 'install_legacy', ?2, ?3, ?4, 'completed', ?5, ?6, ?6)",
                    params![
                        receipt_ref,
                        approval_ref,
                        AppDigest::blake3(b"preview").as_str(),
                        digest.as_str(),
                        serde_json::to_vec(&record).unwrap(),
                        time(1).to_rfc3339(),
                    ],
                )
                .unwrap();
        }
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 4).unwrap();
        drop(connection);

        let migrated = open_registry_connection(&database_path, &scope, true).unwrap();
        for (receipt_ref, expected_kind, forbidden) in [
            ("receipt:legacy-whole", "whole_installation", None),
            (
                "receipt:legacy-records",
                "records",
                Some("private_record_id"),
            ),
        ] {
            let bytes: Vec<u8> = migrated
                .query_row(
                    "SELECT record_json FROM app_purge_receipts WHERE receipt_ref = ?1",
                    params![receipt_ref],
                    |row| row.get(0),
                )
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["protocol_version"], 2);
            assert_eq!(value["selection_kind"], expected_kind);
            assert!(value.get("selection").is_none());
            if let Some(forbidden) = forbidden {
                assert!(!String::from_utf8_lossy(&bytes).contains(forbidden));
            }
            assert_eq!(value["unrelated"], "preserved");
        }
    }

    #[tokio::test]
    async fn first_read_migrates_version_five_cursors_without_materializing_an_absent_scope() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let database_path = workspace.app_store_db_path("anonymous", "default");
        fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let scope = scope("anonymous", "default");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V2).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V3).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V4).unwrap();
        connection.execute_batch(APP_REGISTRY_SCHEMA_V5).unwrap();
        connection
            .execute(
                "INSERT INTO app_registry_scope(singleton, principal, workspace)
                 VALUES (1, ?1, ?2)",
                params![scope.principal.as_str(), scope.workspace.as_str()],
            )
            .unwrap();
        seed_legacy_installation(&connection, &scope);
        connection
            .execute(
                "INSERT INTO app_query_cursors (
                     cursor_ref, installation_id, schema_revision, dataset_generation,
                     evidence_json, snapshot_json, next_offset, created_at, expires_at
                 ) VALUES ('cursor:legacy', 'install_legacy', 1, 1, ?1, ?2, 1, ?3, ?4)",
                params![
                    b"{}".as_slice(),
                    b"ASNP\x01".as_slice(),
                    time(1).to_rfc3339(),
                    time(30).to_rfc3339(),
                ],
            )
            .unwrap();
        connection
            .pragma_update(None, "application_id", APP_REGISTRY_APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 5).unwrap();
        drop(connection);

        let service = AppRegistryService::new(workspace);
        let snapshot_ref = service
            .execute_scoped_read(
                &authenticated_scope("anonymous", "default"),
                &time(2),
                |connection, _| {
                    connection
                        .query_row(
                            "SELECT snapshot_ref FROM app_query_cursors
                              WHERE cursor_ref = 'cursor:legacy'",
                            [],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(AppRegistryError::from)
                },
            )
            .await
            .unwrap();
        assert_eq!(snapshot_ref.as_deref(), Some("cursor:legacy"));

        let absent = service
            .execute_scoped_read(
                &authenticated_scope("anonymous", "another"),
                &time(2),
                |_, _| Ok(()),
            )
            .await
            .unwrap();
        assert!(absent.is_none());
        assert!(!temporary.path().join("scopes/anonymous/another").exists());
    }

    #[test]
    fn registry_capacity_requires_a_real_foreground_reserve() {
        let temporary = canonical_tempdir();
        assert!(matches!(
            AppRegistryService::with_blocking_capacity(
                ArtifactV2Workspace::new(temporary.path()),
                1,
            ),
            Err(AppRegistryError::InvalidPublication(_))
        ));
    }

    #[tokio::test]
    async fn saturated_scope_writer_obeys_caller_cancellation_without_internal_expiry() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let first_service = service.clone();
        let first_scope = authenticated.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first = tokio::spawn(async move {
            first_service
                .execute_scoped_write(&first_scope, &time(3), move |_, _| {
                    let _ = entered_tx.send(());
                    release_rx
                        .recv()
                        .map_err(|error| AppRegistryError::WorkerTerminated(error.to_string()))?;
                    Ok(())
                })
                .await
        });
        entered_rx.await.unwrap();

        let saturated = tokio::time::timeout(
            Duration::from_millis(300),
            service.execute_scoped_write(&authenticated, &time(3), |_, _| Ok(())),
        )
        .await;
        release_tx.send(()).unwrap();
        first.await.unwrap().unwrap();

        assert!(
            saturated.is_err(),
            "only the caller's deadline cancels queued work"
        );
        service
            .execute_scoped_write(&authenticated, &time(3), |_, _| Ok(()))
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_scope_component_is_rejected_before_sqlite_open() {
        use std::os::unix::fs::symlink;

        let temporary = canonical_tempdir();
        let outside = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let publication = publication(
            &service.workspace,
            "anonymous",
            "default",
            "attempt:1",
            "install_1",
        );
        fs::remove_dir_all(temporary.path().join("scopes/anonymous")).unwrap();
        symlink(outside.path(), temporary.path().join("scopes/anonymous")).unwrap();
        let error = service
            .publish_ready_for_review(&authenticated, publication, time(3))
            .await
            .unwrap_err();
        assert!(matches!(error, AppRegistryError::UnsafePath(_)));
        assert!(!outside
            .path()
            .join("default/apps/app_store.sqlite3")
            .exists());
        assert!(!outside
            .path()
            .join("default/apps/scope-binding.json")
            .exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_workspace_root_is_not_treated_as_registry_authority() {
        use std::os::unix::fs::symlink;

        let parent = canonical_tempdir();
        let outside = canonical_tempdir();
        let linked_root = parent.path().join("workspace-link");
        symlink(outside.path(), &linked_root).unwrap();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(&linked_root));
        let authenticated = authenticated_scope("anonymous", "default");
        let error = service
            .installation(
                &authenticated,
                &AppInstallationId::parse("install_missing").unwrap(),
                time(3),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppRegistryError::UnsafePath(_)));
        assert!(!outside.path().join("scopes").exists());
    }

    #[test]
    fn protected_pause_generation_rejects_copied_claimed_and_consumed_replay() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let app_scope = scope("anonymous", "default");
        let first_proposal_ref = reference("pause-prepare:first");
        let prepared = service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "pause-key",
                "pause-key",
                b"exact protected continuation".to_vec(),
                None,
                &first_proposal_ref,
                time(1),
            )
            .unwrap();
        assert_eq!(prepared.lifecycle(), AppWorkflowControlLifecycle::Prepared);
        let first = service
            .activate_prepared_workflow_pause_sync(
                app_scope.clone(),
                "pause-key",
                "pause-key",
                prepared.generation(),
                prepared.content_digest(),
                &first_proposal_ref,
                time(1),
            )
            .unwrap();
        let claim_ref = reference("pause-claim:test");
        service
            .transition_workflow_pause_sync(
                app_scope.clone(),
                "pause-key",
                "pause-key",
                first.generation(),
                first.content_digest(),
                AppWorkflowControlLifecycle::Active,
                None,
                AppWorkflowControlLifecycle::Claimed,
                Some(&claim_ref),
                time(2),
            )
            .unwrap();
        let replacement_proposal_ref = reference("pause-prepare:replacement");
        let replacement_prepared = service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "pause-key",
                "pause-key",
                b"exact protected continuation".to_vec(),
                Some(&claim_ref),
                &replacement_proposal_ref,
                time(3),
            )
            .unwrap();
        assert_eq!(
            replacement_prepared.lifecycle(),
            AppWorkflowControlLifecycle::Prepared
        );
        let replacement = service
            .activate_prepared_workflow_pause_sync(
                app_scope.clone(),
                "pause-key",
                "pause-key",
                replacement_prepared.generation(),
                replacement_prepared.content_digest(),
                &replacement_proposal_ref,
                time(3),
            )
            .unwrap();
        assert!(replacement.generation() > first.generation());
        assert_eq!(replacement.lifecycle(), AppWorkflowControlLifecycle::Active);
        assert!(service
            .transition_workflow_pause_sync(
                app_scope.clone(),
                "pause-key",
                "pause-key",
                first.generation(),
                first.content_digest(),
                AppWorkflowControlLifecycle::Claimed,
                Some(&claim_ref),
                AppWorkflowControlLifecycle::Active,
                None,
                time(4),
            )
            .is_err());
        let replacement_claim = reference("pause-claim:replacement");
        let claimed = service
            .transition_workflow_pause_sync(
                app_scope.clone(),
                "pause-key",
                "pause-key",
                replacement.generation(),
                replacement.content_digest(),
                AppWorkflowControlLifecycle::Active,
                None,
                AppWorkflowControlLifecycle::Claimed,
                Some(&replacement_claim),
                time(4),
            )
            .unwrap();
        service
            .transition_workflow_pause_sync(
                app_scope,
                "pause-key",
                "pause-key",
                claimed.generation(),
                claimed.content_digest(),
                AppWorkflowControlLifecycle::Claimed,
                Some(&replacement_claim),
                AppWorkflowControlLifecycle::Consumed,
                None,
                time(5),
            )
            .unwrap();
        assert!(service
            .transition_workflow_pause_sync(
                scope("anonymous", "default"),
                "pause-key",
                "pause-key",
                claimed.generation(),
                claimed.content_digest(),
                AppWorkflowControlLifecycle::Active,
                None,
                AppWorkflowControlLifecycle::Claimed,
                Some(&reference("pause-claim:replay")),
                time(6),
            )
            .is_err());
    }

    #[test]
    fn protected_pause_claim_lease_cannot_be_stolen_before_expiry() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let app_scope = scope("anonymous", "default");
        let proposal_ref = reference("pause-prepare:lease");
        let prepared = service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "pause-lease-key",
                "pause-lease-key",
                b"exact protected continuation".to_vec(),
                None,
                &proposal_ref,
                time(1),
            )
            .unwrap();
        let active = service
            .activate_prepared_workflow_pause_sync(
                app_scope.clone(),
                "pause-lease-key",
                "pause-lease-key",
                prepared.generation(),
                prepared.content_digest(),
                &proposal_ref,
                time(2),
            )
            .unwrap();
        let claim_ref = reference("pause-claim:live-owner");
        let claimed = service
            .transition_workflow_pause_sync(
                app_scope.clone(),
                "pause-lease-key",
                "pause-lease-key",
                active.generation(),
                active.content_digest(),
                AppWorkflowControlLifecycle::Active,
                None,
                AppWorkflowControlLifecycle::Claimed,
                Some(&claim_ref),
                time(3),
            )
            .unwrap();
        assert!(claimed.claim_expires_at().is_some());
        assert!(service
            .release_expired_workflow_pause_claim_sync(
                app_scope.clone(),
                "pause-lease-key",
                "pause-lease-key",
                claimed.generation(),
                claimed.content_digest(),
                &claim_ref,
                time(4),
            )
            .is_err());
        let released = service
            .release_expired_workflow_pause_claim_sync(
                app_scope.clone(),
                "pause-lease-key",
                "pause-lease-key",
                claimed.generation(),
                claimed.content_digest(),
                &claim_ref,
                time(3) + chrono::Duration::seconds(WORKFLOW_PAUSE_CLAIM_LEASE_SECONDS + 1),
            )
            .unwrap();
        assert_eq!(released.lifecycle(), AppWorkflowControlLifecycle::Active);
        assert!(service
            .release_expired_workflow_pause_claim_sync(
                app_scope,
                "pause-lease-key",
                "pause-lease-key",
                claimed.generation(),
                claimed.content_digest(),
                &claim_ref,
                time(3) + chrono::Duration::seconds(WORKFLOW_PAUSE_CLAIM_LEASE_SECONDS + 2),
            )
            .is_err());
    }

    #[test]
    fn protected_pause_prepared_proposal_is_owned_and_retires_without_poisoning_base() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let app_scope = scope("anonymous", "default");
        let proposal_ref = reference("pause-prepare:writer-a");
        let prepared = service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "pause-prepare-key",
                "pause-prepare-key",
                b"exact protected continuation".to_vec(),
                None,
                &proposal_ref,
                time(1),
            )
            .unwrap();
        assert!(service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "pause-prepare-key",
                "pause-prepare-key",
                b"exact protected continuation".to_vec(),
                None,
                &reference("pause-prepare:writer-b"),
                time(2),
            )
            .is_err());
        assert!(service
            .retire_expired_prepared_workflow_pause_sync(
                app_scope.clone(),
                "pause-prepare-key",
                "pause-prepare-key",
                prepared.generation(),
                prepared.content_digest(),
                &proposal_ref,
                time(2),
            )
            .is_err());
        service
            .abort_prepared_workflow_pause_sync(
                app_scope.clone(),
                "pause-prepare-key",
                "pause-prepare-key",
                prepared.generation(),
                prepared.content_digest(),
                &proposal_ref,
            )
            .unwrap();
        assert!(service
            .current_workflow_control_sync(
                app_scope.clone(),
                "pause-prepare-key",
                "pause-prepare-key",
                AppWorkflowControlKind::Pause,
            )
            .unwrap()
            .is_none());
        let replacement_ref = reference("pause-prepare:writer-b");
        let replacement = service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "pause-prepare-key",
                "pause-prepare-key",
                b"replacement continuation".to_vec(),
                None,
                &replacement_ref,
                time(3),
            )
            .unwrap();
        service
            .retire_expired_prepared_workflow_pause_sync(
                app_scope,
                "pause-prepare-key",
                "pause-prepare-key",
                replacement.generation(),
                replacement.content_digest(),
                &replacement_ref,
                time(3) + chrono::Duration::seconds(WORKFLOW_PAUSE_PREPARE_LEASE_SECONDS + 1),
            )
            .unwrap();
    }

    #[test]
    fn workflow_control_retains_only_authoritative_and_prepared_payloads() {
        let temporary = canonical_tempdir();
        let service = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let app_scope = scope("anonymous", "default");
        for (offset, bytes) in [
            b"run-state:one".as_slice(),
            b"run-state:two",
            b"run-state:three",
        ]
        .into_iter()
        .enumerate()
        {
            service
                .publish_workflow_control_generation_sync(
                    app_scope.clone(),
                    "retained-task",
                    "retained-execution",
                    AppWorkflowControlKind::RunState,
                    bytes.to_vec(),
                    time(u32::try_from(offset).unwrap() + 1),
                )
                .unwrap();
        }

        let first_proposal = reference("pause-prepare:retained-first");
        let first_prepared = service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "retained-pause",
                "retained-pause",
                b"pause-state:one".to_vec(),
                None,
                &first_proposal,
                time(4),
            )
            .unwrap();
        service
            .activate_prepared_workflow_pause_sync(
                app_scope.clone(),
                "retained-pause",
                "retained-pause",
                first_prepared.generation(),
                first_prepared.content_digest(),
                &first_proposal,
                time(5),
            )
            .unwrap();
        let second_proposal = reference("pause-prepare:retained-second");
        let second_prepared = service
            .prepare_workflow_pause_generation_sync(
                app_scope.clone(),
                "retained-pause",
                "retained-pause",
                b"pause-state:two".to_vec(),
                None,
                &second_proposal,
                time(6),
            )
            .unwrap();

        let connection = open_registry_connection(
            &service.workspace.app_store_db_path("anonymous", "default"),
            &app_scope,
            false,
        )
        .unwrap();
        let run_blob_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_workflow_control_blobs
                  WHERE task_id = 'retained-task'
                    AND execution_id = 'retained-execution'
                    AND control_kind = 'run_state'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let pause_blob_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_workflow_control_blobs
                  WHERE task_id = 'retained-pause'
                    AND execution_id = 'retained-pause'
                    AND control_kind = 'pause'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_blob_count, 1);
        assert_eq!(pause_blob_count, 2);
        drop(connection);

        service
            .activate_prepared_workflow_pause_sync(
                app_scope.clone(),
                "retained-pause",
                "retained-pause",
                second_prepared.generation(),
                second_prepared.content_digest(),
                &second_proposal,
                time(7),
            )
            .unwrap();
        let connection = open_registry_connection(
            &service.workspace.app_store_db_path("anonymous", "default"),
            &app_scope,
            false,
        )
        .unwrap();
        let pause_blob_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_workflow_control_blobs
                  WHERE task_id = 'retained-pause'
                    AND execution_id = 'retained-pause'
                    AND control_kind = 'pause'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pause_blob_count, 1);
    }
}
