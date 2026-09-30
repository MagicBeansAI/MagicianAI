//! Capability evolution pack format, persistence, and promotion bridge.
//!
//! This module introduces a persisted catalog of generated capability packs and
//! a bridge that promotes API-mined capabilities into executable pack records.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::capability::{
    CapabilityPackDefinition, CommandArgMapping, CompositeStep, ExecutionMetadata,
    ImplementationType, ParameterDef, ParameterType,
};
use super::capability_eval::{
    evaluate_transition, CapabilityEvaluationDecision, CapabilityEvaluationInput,
    CapabilityEvaluationThresholds, CapabilityLifecycleStatus,
};
use crate::magician_v2::api_mining::body_template::{
    extract_body_template_params, BodyPlaceholderKind,
};
use crate::magician_v2::api_mining::capability::{ApiCapability, ConfidenceLevel, SideEffects};
use crate::magician_v2::system_owners::persist_system_file_sync;

const CATALOG_SCHEMA_VERSION: &str = "1.0.0";
const CATALOG_FILENAME: &str = "capability_packs.json";
const CATALOG_LOCK_FILENAME: &str = ".catalog.lock";
const AUDIT_FILENAME: &str = "pack_promotion_audit.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityPackSource {
    ApiMined,
    TaskRecipe,
    Generated,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPackMetadata {
    pub source: CapabilityPackSource,
    pub status: CapabilityLifecycleStatus,
    pub attempts: u32,
    pub successes: u32,
    pub last_score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPackRecord {
    pub definition: CapabilityPackDefinition,
    pub metadata: CapabilityPackMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPackCatalog {
    pub schema_version: String,
    #[serde(default)]
    pub packs: Vec<CapabilityPackRecord>,
    pub updated_at: i64,
}

impl Default for CapabilityPackCatalog {
    fn default() -> Self {
        Self {
            schema_version: CATALOG_SCHEMA_VERSION.to_string(),
            packs: Vec::new(),
            updated_at: Utc::now().timestamp(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPromotionAuditEvent {
    pub timestamp: i64,
    pub pack_name: String,
    pub source: CapabilityPackSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<CapabilityLifecycleStatus>,
    pub next_status: CapabilityLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_file: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPackUpsertOutcome {
    pub created: bool,
    pub pack_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<CapabilityLifecycleStatus>,
    pub next_status: CapabilityLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_file: Option<String>,
    pub definition: CapabilityPackDefinition,
}

#[derive(Debug, Clone)]
pub struct CapabilityPackStore {
    workspace: crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    root: PathBuf,
}

impl CapabilityPackStore {
    pub fn with_base_path<P: AsRef<Path>>(base_path: P) -> Self {
        let workspace_layout = crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::resolve_scoped_root(
                base_path.as_ref(),
            ),
        );
        Self::with_workspace_layout(
            &workspace_layout,
            crate::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_PRINCIPAL,
            crate::magician_v2::artifact_v2::workspace::DEFAULT_SCOPE_WORKSPACE,
        )
    }

    pub fn with_scope<P: AsRef<Path>>(base_path: P, principal: &str, workspace: &str) -> Self {
        let workspace_layout = crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::resolve_scoped_root(
                base_path.as_ref(),
            ),
        );
        Self::with_workspace_layout(&workspace_layout, principal, workspace)
    }

    pub fn with_workspace_layout(
        workspace_layout: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
    ) -> Self {
        Self {
            workspace: workspace_layout.clone(),
            root: workspace_layout.capability_evolution_root(principal, workspace),
        }
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.root.join(CATALOG_FILENAME)
    }

    fn catalog_lock_path(&self) -> PathBuf {
        self.root.join(CATALOG_LOCK_FILENAME)
    }

    pub fn rollback_dir(&self) -> PathBuf {
        self.root.join("rollback")
    }

    pub fn audit_path(&self) -> PathBuf {
        self.root.join(AUDIT_FILENAME)
    }

    fn ensure_layout(&self) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| {
            format!(
                "Failed to create capability_evolution dir '{}': {}",
                self.root.display(),
                e
            )
        })?;
        fs::create_dir_all(self.rollback_dir()).map_err(|e| {
            format!(
                "Failed to create rollback dir '{}': {}",
                self.rollback_dir().display(),
                e
            )
        })?;
        Ok(())
    }

    fn acquire_catalog_lock(&self) -> Result<std::fs::File, String> {
        self.ensure_layout()?;
        let lock_path = self.catalog_lock_path();
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                format!(
                    "Failed opening capability catalog lock '{}': {}",
                    lock_path.display(),
                    e
                )
            })?;
        lock_file.lock_exclusive().map_err(|e| {
            format!(
                "Failed acquiring capability catalog lock '{}': {}",
                lock_path.display(),
                e
            )
        })?;
        Ok(lock_file)
    }

    fn acquire_catalog_shared_lock(&self) -> Result<std::fs::File, String> {
        self.ensure_layout()?;
        let lock_path = self.catalog_lock_path();
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                format!(
                    "Failed opening capability catalog lock '{}': {}",
                    lock_path.display(),
                    e
                )
            })?;
        lock_file.lock_shared().map_err(|e| {
            format!(
                "Failed acquiring shared capability catalog lock '{}': {}",
                lock_path.display(),
                e
            )
        })?;
        Ok(lock_file)
    }

    fn acquire_catalog_shared_lock_nonblocking(&self) -> Result<Option<std::fs::File>, String> {
        self.ensure_layout()?;
        let lock_path = self.catalog_lock_path();
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                format!(
                    "Failed opening capability catalog lock '{}': {}",
                    lock_path.display(),
                    e
                )
            })?;
        match lock_file.try_lock_shared() {
            Ok(()) => Ok(Some(lock_file)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(format!(
                "Failed acquiring shared capability catalog lock '{}': {}",
                lock_path.display(),
                e
            )),
        }
    }

    fn load_catalog_unlocked(&self) -> Result<CapabilityPackCatalog, String> {
        let path = self.catalog_path();
        if !path.exists() {
            return Ok(CapabilityPackCatalog::default());
        }
        let raw = fs::read_to_string(&path).map_err(|e| {
            format!(
                "Failed reading capability catalog '{}': {}",
                path.display(),
                e
            )
        })?;
        serde_json::from_str::<CapabilityPackCatalog>(&raw).map_err(|e| {
            format!(
                "Failed parsing capability catalog '{}': {}",
                path.display(),
                e
            )
        })
    }

    pub fn load_catalog(&self) -> Result<CapabilityPackCatalog, String> {
        let _catalog_lock = self.acquire_catalog_shared_lock()?;
        self.load_catalog_unlocked()
    }

    pub fn try_load_catalog(&self) -> Result<Option<CapabilityPackCatalog>, String> {
        let Some(_catalog_lock) = self.acquire_catalog_shared_lock_nonblocking()? else {
            return Ok(None);
        };
        self.load_catalog_unlocked().map(Some)
    }

    /// Publish the catalog through the shared durable writer.
    ///
    /// The hand-rolled version fsynced nothing at all: an unclean shutdown
    /// could publish a rename whose contents never reached the platter, and a
    /// failure between write and rename left the staging file behind. Its
    /// fixed `capability_packs.json.tmp` was covered in practice by the
    /// exclusive flock `upsert_record` holds — the only caller — but the
    /// missing fsync was not covered by anything.
    fn save_catalog(&self, catalog: &CapabilityPackCatalog) -> Result<(), String> {
        self.ensure_layout()?;
        let path = self.catalog_path();
        let serialized = serde_json::to_string_pretty(catalog)
            .map_err(|e| format!("Failed serializing capability catalog: {}", e))?;
        persist_system_file_sync(&self.workspace, &path, serialized.as_bytes()).map_err(|e| {
            format!(
                "Failed writing capability catalog '{}': {}",
                path.display(),
                e
            )
        })?;
        Ok(())
    }

    fn write_rollback_snapshot(
        &self,
        prior: &CapabilityPackRecord,
    ) -> Result<Option<String>, String> {
        self.ensure_layout()?;
        let timestamp = Utc::now().timestamp_millis();
        let name = sanitize_name_fragment(&prior.definition.name, 48);
        let filename = format!("{}_{}.json", timestamp, name);
        let path = self.rollback_dir().join(filename);
        let payload = serde_json::to_string_pretty(prior)
            .map_err(|e| format!("Failed serializing rollback payload: {}", e))?;
        persist_system_file_sync(&self.workspace, &path, payload.as_bytes()).map_err(|e| {
            format!(
                "Failed writing rollback snapshot '{}': {}",
                path.display(),
                e
            )
        })?;
        Ok(Some(path.to_string_lossy().to_string()))
    }

    fn append_audit(&self, event: &CapabilityPromotionAuditEvent) -> Result<(), String> {
        self.ensure_layout()?;
        let path = self.audit_path();
        let line = serde_json::to_string(event)
            .map_err(|e| format!("Failed serializing audit event: {}", e))?;
        let mut bytes = fs::read(&path).unwrap_or_default();
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        persist_system_file_sync(&self.workspace, &path, &bytes)
            .map_err(|e| format!("Failed appending audit event '{}': {}", path.display(), e))?;
        Ok(())
    }

    pub fn validate_definition(def: &CapabilityPackDefinition) -> Result<(), String> {
        if def.name.trim().is_empty() {
            return Err("Capability pack name cannot be empty".to_string());
        }

        let mut seen = HashSet::new();
        for param in &def.parameters {
            if param.name.trim().is_empty() {
                return Err(format!(
                    "Capability pack '{}' has empty parameter name",
                    def.name
                ));
            }
            if !seen.insert(param.name.clone()) {
                return Err(format!(
                    "Capability pack '{}' has duplicate parameter '{}'",
                    def.name, param.name
                ));
            }
        }

        match &def.implementation {
            ImplementationType::Composite { steps } => {
                if steps.is_empty() {
                    return Err(format!(
                        "Capability pack '{}' has empty composite steps",
                        def.name
                    ));
                }
                for (idx, step) in steps.iter().enumerate() {
                    if step.tool.trim().is_empty() {
                        return Err(format!(
                            "Capability pack '{}' has composite step {} with empty tool name",
                            def.name, idx
                        ));
                    }
                }
            },
            ImplementationType::Compiled { provider_name } => {
                if provider_name.trim().is_empty() {
                    return Err(format!(
                        "Capability pack '{}' has empty compiled provider name",
                        def.name
                    ));
                }
            },
            ImplementationType::Command {
                program,
                arg_mappings,
                env,
                ..
            } => {
                if program.trim().is_empty() {
                    return Err(format!(
                        "Capability pack '{}' has empty command program",
                        def.name
                    ));
                }
                // Validate that arg_mapping params reference declared parameters.
                let declared: std::collections::HashSet<&str> =
                    def.parameters.iter().map(|p| p.name.as_str()).collect();
                for mapping in arg_mappings {
                    let param_name = match mapping {
                        CommandArgMapping::Positional { param }
                        | CommandArgMapping::Flag { param, .. }
                        | CommandArgMapping::BoolFlag { param, .. }
                        | CommandArgMapping::SplitPositional { param }
                        | CommandArgMapping::Passthrough { param } => Some(param.as_str()),
                        CommandArgMapping::EnvFlag { .. } | CommandArgMapping::FixedArgs { .. } => {
                            None
                        },
                    };
                    if let Some(name) = param_name {
                        if !declared.contains(name) {
                            return Err(format!(
                                "Capability pack '{}' command arg_mapping references undeclared parameter '{}'",
                                def.name, name
                            ));
                        }
                    }
                }
                // Validate that env value templates reference declared parameters.
                for (env_key, env_template) in env {
                    let mut rest = env_template.as_str();
                    while let Some(start) = rest.find('{') {
                        if let Some(end) = rest[start..].find('}') {
                            let param = &rest[start + 1..start + end];
                            if !param.is_empty()
                                && !param.contains(' ')
                                && !declared.contains(param)
                                && !is_scope_template_var(param)
                            {
                                return Err(format!(
                                    "Capability pack '{}' command env '{}' references undeclared parameter '{}'",
                                    def.name, env_key, param
                                ));
                            }
                            rest = &rest[start + end + 1..];
                        } else {
                            break;
                        }
                    }
                }
            },
            ImplementationType::Primitive { .. } => {
                // Inner-loop packs route through the inner-loop runtime; no
                // structural validation needed here (synthesised params come
                // from the runtime, not the YAML).
            },
        }

        Ok(())
    }

    pub fn upsert_record(
        &self,
        mut record: CapabilityPackRecord,
        reason: impl Into<String>,
    ) -> Result<CapabilityPackUpsertOutcome, String> {
        Self::validate_definition(&record.definition)?;
        let _catalog_lock = self.acquire_catalog_lock()?;

        let now = Utc::now().timestamp();
        record.metadata.updated_at = now;

        let mut catalog = self.load_catalog_unlocked()?;
        let name = record.definition.name.clone();
        let reason = reason.into();

        let (created, previous_status, rollback_file) =
            if let Some(pos) = catalog.packs.iter().position(|p| p.definition.name == name) {
                let prior = catalog.packs[pos].clone();
                let created_at = prior.metadata.created_at;
                record.metadata.created_at = created_at;
                let rollback_file = self.write_rollback_snapshot(&prior)?;
                let previous_status = Some(prior.metadata.status);
                catalog.packs[pos] = record.clone();
                (false, previous_status, rollback_file)
            } else {
                if record.metadata.created_at == 0 {
                    record.metadata.created_at = now;
                }
                catalog.packs.push(record.clone());
                (true, None, None)
            };

        catalog.updated_at = now;
        self.save_catalog(&catalog)?;

        let event = CapabilityPromotionAuditEvent {
            timestamp: now,
            pack_name: record.definition.name.clone(),
            source: record.metadata.source.clone(),
            source_ref: record.metadata.source_ref.clone(),
            previous_status,
            next_status: record.metadata.status,
            rollback_file: rollback_file.clone(),
            reason,
        };
        if let Err(err) = self.append_audit(&event) {
            warn!(
                "[CAPABILITY_EVOLUTION] failed to append promotion audit for '{}': {}; continuing with persisted catalog update",
                event.pack_name,
                err
            );
        }

        Ok(CapabilityPackUpsertOutcome {
            created,
            pack_name: record.definition.name.clone(),
            previous_status: event.previous_status,
            next_status: event.next_status,
            rollback_file,
            definition: record.definition,
        })
    }

    /// Remove generated records owned by one source, optionally restricted to
    /// concrete source references. Destructive API-mining cleanup uses this
    /// under the same catalog lock as promotion so deleted recipe tools cannot
    /// race an in-flight catalog upsert into a malformed file.
    pub fn remove_source_records(
        &self,
        source: CapabilityPackSource,
        source_refs: Option<&HashSet<String>>,
    ) -> Result<Vec<CapabilityPackRecord>, String> {
        if !self.catalog_path().exists() {
            return Ok(Vec::new());
        }
        let _catalog_lock = self.acquire_catalog_lock()?;
        let mut catalog = self.load_catalog_unlocked()?;
        let mut removed = Vec::new();
        catalog.packs.retain(|record| {
            let selected = record.metadata.source == source
                && source_refs.is_none_or(|refs| {
                    record
                        .metadata
                        .source_ref
                        .as_ref()
                        .is_some_and(|source_ref| refs.contains(source_ref))
                });
            if selected {
                removed.push(record.clone());
            }
            !selected
        });
        if removed.is_empty() {
            return Ok(removed);
        }
        catalog.updated_at = Utc::now().timestamp();
        self.save_catalog(&catalog)?;
        Ok(removed)
    }

    pub fn load_runtime_pack_defs(
        &self,
        include_trial: bool,
        max_packs: usize,
    ) -> Result<Vec<CapabilityPackDefinition>, String> {
        let catalog = self.load_catalog()?;
        Ok(self.runtime_pack_defs_from_catalog(catalog, include_trial, max_packs))
    }

    pub fn try_load_runtime_pack_defs(
        &self,
        include_trial: bool,
        max_packs: usize,
    ) -> Result<Option<Vec<CapabilityPackDefinition>>, String> {
        let Some(catalog) = self.try_load_catalog()? else {
            return Ok(None);
        };
        Ok(Some(self.runtime_pack_defs_from_catalog(
            catalog,
            include_trial,
            max_packs,
        )))
    }

    fn runtime_pack_defs_from_catalog(
        &self,
        catalog: CapabilityPackCatalog,
        include_trial: bool,
        max_packs: usize,
    ) -> Vec<CapabilityPackDefinition> {
        let mut candidates = catalog
            .packs
            .into_iter()
            .filter(|record| match record.metadata.status {
                CapabilityLifecycleStatus::Trial => include_trial,
                CapabilityLifecycleStatus::Validated | CapabilityLifecycleStatus::Trusted => true,
                CapabilityLifecycleStatus::Deprecated => false,
            })
            .filter(|record| {
                if is_legacy_browser_api_replay_definition(&record.definition) {
                    tracing::debug!(
                        target: "magician::capability_evolution",
                        pack_name = %record.definition.name,
                        status = ?record.metadata.status,
                        "capability_evolution.runtime_pack.skipped_legacy_api_replay_surface"
                    );
                    false
                } else {
                    true
                }
            })
            .filter_map(|record| {
                if let Err(err) = Self::validate_definition(&record.definition) {
                    warn!(
                        "[CAPABILITY_EVOLUTION] skipping invalid runtime pack '{}': {}",
                        record.definition.name, err
                    );
                    None
                } else {
                    Some(record)
                }
            })
            .collect::<Vec<_>>();

        // Prefer the most trustworthy and highest-quality generated packs when
        // runtime cap pressure exists.
        candidates.sort_by(|a, b| {
            status_priority(b.metadata.status)
                .cmp(&status_priority(a.metadata.status))
                .then_with(|| b.metadata.last_score.total_cmp(&a.metadata.last_score))
                .then_with(|| b.metadata.successes.cmp(&a.metadata.successes))
                .then_with(|| b.metadata.updated_at.cmp(&a.metadata.updated_at))
                .then_with(|| a.definition.name.cmp(&b.definition.name))
        });

        candidates
            .into_iter()
            .take(max_packs)
            .map(|record| record.definition)
            .collect()
    }
}

fn is_scope_template_var(value: &str) -> bool {
    // Allowlist matches the live entries in
    // `CapabilityScopePaths::apply_vars`. Removed-as-dead alongside the
    // capabilities/ umbrella refactor: `scope_capability_config_root`,
    // `scope_home_root`, `scope_tools_root`, `scope_packs_root`.
    matches!(
        value,
        "scope_capabilities_root" | "scope_capability_auth_root" | "scope_capability_workdir_root"
    )
}

/// Legacy Capability Evolution replay packs wrapped direct HTTP replay as a
/// `browser` composite step with `action=api_replay`. The active browser tool no
/// longer exposes that primitive; live takeover uses `ApiRouter` + `ApiRunner`
/// directly at the browser primitive boundary. Until a real generated-pack
/// provider exists, these definitions remain catalog/audit records only and must
/// not enter the runtime tool catalog or evolved-skill folder.
pub fn is_legacy_browser_api_replay_definition(def: &CapabilityPackDefinition) -> bool {
    let execution_marks_replay = def
        .execution
        .as_ref()
        .and_then(|execution| execution.composition_category.as_deref())
        == Some("api_replay");

    let has_browser_api_replay_step = match &def.implementation {
        ImplementationType::Composite { steps } => steps.iter().any(|step| {
            step.tool == "browser"
                && step
                    .parameters
                    .get("action")
                    .is_some_and(|action| action == "api_replay")
        }),
        _ => false,
    };

    execution_marks_replay || has_browser_api_replay_step
}

fn status_priority(status: CapabilityLifecycleStatus) -> u8 {
    match status {
        CapabilityLifecycleStatus::Trusted => 3,
        CapabilityLifecycleStatus::Validated => 2,
        CapabilityLifecycleStatus::Trial => 1,
        CapabilityLifecycleStatus::Deprecated => 0,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilityPromotionReport {
    pub examined: usize,
    pub promoted: usize,
    pub deprecated: usize,
    pub runtime_loaded: usize,
    #[serde(default)]
    pub outcomes: Vec<CapabilityPackUpsertOutcome>,
    #[serde(default)]
    pub runtime_pack_defs: Vec<CapabilityPackDefinition>,
}

#[derive(Debug, Clone)]
pub struct CapabilityPromotionBridge {
    store: CapabilityPackStore,
    thresholds: CapabilityEvaluationThresholds,
    /// Per-scope skills root. When set, the bridge emits a SKILL.md
    /// for each promoted capability under `<skills_root>/evolved/`
    /// (CE Phase 3). `None` falls back to legacy catalog-only
    /// behaviour for tests / non-scoped callers.
    scope_skills_root: Option<std::path::PathBuf>,
}

impl CapabilityPromotionBridge {
    pub fn with_base_path<P: AsRef<Path>>(
        base_path: P,
        thresholds: CapabilityEvaluationThresholds,
    ) -> Self {
        Self {
            store: CapabilityPackStore::with_base_path(base_path),
            thresholds,
            scope_skills_root: None,
        }
    }

    pub fn with_scope<P: AsRef<Path>>(
        base_path: P,
        principal: &str,
        workspace: &str,
        thresholds: CapabilityEvaluationThresholds,
    ) -> Self {
        let workspace_layout = crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::resolve_scoped_root(
                base_path.as_ref(),
            ),
        );
        Self {
            store: CapabilityPackStore::with_scope(base_path.as_ref(), principal, workspace),
            thresholds,
            scope_skills_root: Some(workspace_layout.scope_skills_root(principal, workspace)),
        }
    }

    pub fn with_workspace_layout(
        workspace_layout: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
        principal: &str,
        workspace: &str,
        thresholds: CapabilityEvaluationThresholds,
    ) -> Self {
        Self {
            store: CapabilityPackStore::with_workspace_layout(
                workspace_layout,
                principal,
                workspace,
            ),
            thresholds,
            scope_skills_root: Some(workspace_layout.scope_skills_root(principal, workspace)),
        }
    }

    pub fn store(&self) -> &CapabilityPackStore {
        &self.store
    }

    pub fn promote_api_capabilities(
        &self,
        capabilities: &[ApiCapability],
        include_trial_runtime: bool,
        max_runtime_packs: usize,
    ) -> Result<CapabilityPromotionReport, String> {
        let mut report = CapabilityPromotionReport::default();

        for capability in capabilities {
            report.examined += 1;

            let current_status = lifecycle_from_confidence(&capability.confidence);
            let eval_input = build_eval_input(capability);
            let decision = evaluate_transition(current_status, &eval_input, &self.thresholds);
            let next_status = apply_eval_decision(current_status, &decision);

            if matches!(
                next_status,
                CapabilityLifecycleStatus::Validated | CapabilityLifecycleStatus::Trusted
            ) && next_status != current_status
            {
                report.promoted += 1;
            }
            if next_status == CapabilityLifecycleStatus::Deprecated {
                report.deprecated += 1;
            }

            let record = api_capability_to_record(capability, next_status, eval_input.score())?;
            // Catalog is authoritative: write it first. If upsert
            // fails, we error out before touching the skills folder,
            // so the on-disk state stays consistent (no orphan
            // SKILL.md). If catalog write succeeds but skill emit
            // fails, the next bridge run re-emits — emission is
            // idempotent (Refreshed vs Emitted both update the
            // SKILL.md to the latest record state).
            let outcome = self
                .store
                .upsert_record(record.clone(), decision_reason(&decision))?;

            // CE Phase 3: emit a SKILL.md for promoted records so the
            // evolved pack appears in the AgentSkills catalog
            // alongside hand-authored skills. Best-effort — a write
            // failure logs warn but doesn't fail the bridge run.
            if let Some(skills_root) = &self.scope_skills_root {
                if matches!(
                    next_status,
                    CapabilityLifecycleStatus::Validated | CapabilityLifecycleStatus::Trusted
                ) {
                    let emit_outcome =
                        crate::magician_v2::api_mining::skill_emitter::emit_evolved_skill(
                            skills_root,
                            &record,
                        );
                    match emit_outcome {
                        crate::magician_v2::api_mining::skill_emitter::EmitOutcome::Emitted {
                            path,
                        }
                        | crate::magician_v2::api_mining::skill_emitter::EmitOutcome::Refreshed {
                            path,
                        } => {
                            tracing::info!(
                                target: "magician::capability_evolution",
                                skill_path = %path.display(),
                                capability_id = %record.metadata.source_ref.clone().unwrap_or_default(),
                                "capability_evolution.skill.emitted"
                            );
                        },
                        crate::magician_v2::api_mining::skill_emitter::EmitOutcome::Skipped {
                            reason,
                        } => {
                            tracing::debug!(
                                target: "magician::capability_evolution",
                                reason = %reason,
                                capability_id = %record.metadata.source_ref.clone().unwrap_or_default(),
                                "capability_evolution.skill.skipped"
                            );
                        },
                        crate::magician_v2::api_mining::skill_emitter::EmitOutcome::Failed {
                            reason,
                        } => {
                            tracing::warn!(
                                target: "magician::capability_evolution",
                                reason = %reason,
                                capability_id = %record.metadata.source_ref.clone().unwrap_or_default(),
                                "capability_evolution.skill.emit_failed"
                            );
                        },
                    }
                }
            }
            report.outcomes.push(outcome);
        }

        report.runtime_pack_defs = self
            .store
            .load_runtime_pack_defs(include_trial_runtime, max_runtime_packs)?;
        report.runtime_loaded = report.runtime_pack_defs.len();

        Ok(report)
    }
}

fn lifecycle_from_confidence(confidence: &ConfidenceLevel) -> CapabilityLifecycleStatus {
    match confidence {
        ConfidenceLevel::Observed | ConfidenceLevel::Candidate => CapabilityLifecycleStatus::Trial,
        ConfidenceLevel::Validated => CapabilityLifecycleStatus::Validated,
        ConfidenceLevel::Trusted => CapabilityLifecycleStatus::Trusted,
    }
}

fn build_eval_input(cap: &ApiCapability) -> CapabilityEvaluationInput {
    let attempts = (cap.replay_success_count + cap.replay_failure_count) as u32;
    let attempts = if attempts == 0 {
        cap.sample_count as u32
    } else {
        attempts
    };
    let successes = cap.replay_success_count as u32;
    let verification_pass_rate = if attempts == 0 {
        0.0
    } else {
        (successes as f64 / attempts as f64).clamp(0.0, 1.0)
    };
    let safety_violations = if cap.effective_side_effects() == SideEffects::ReadOnly {
        0
    } else {
        1
    };
    CapabilityEvaluationInput {
        attempts,
        successes,
        verification_pass_rate,
        safety_violations,
    }
}

fn apply_eval_decision(
    current: CapabilityLifecycleStatus,
    decision: &CapabilityEvaluationDecision,
) -> CapabilityLifecycleStatus {
    match decision {
        CapabilityEvaluationDecision::NoChange => current,
        CapabilityEvaluationDecision::PromoteToValidated => CapabilityLifecycleStatus::Validated,
        CapabilityEvaluationDecision::PromoteToTrusted => CapabilityLifecycleStatus::Trusted,
        CapabilityEvaluationDecision::DemoteToTrial => CapabilityLifecycleStatus::Trial,
        CapabilityEvaluationDecision::DemoteToValidated => CapabilityLifecycleStatus::Validated,
        CapabilityEvaluationDecision::Deprecate => CapabilityLifecycleStatus::Deprecated,
    }
}

fn decision_reason(decision: &CapabilityEvaluationDecision) -> &'static str {
    match decision {
        CapabilityEvaluationDecision::NoChange => "status_unchanged",
        CapabilityEvaluationDecision::PromoteToValidated => "promoted_trial_to_validated",
        CapabilityEvaluationDecision::PromoteToTrusted => "promoted_validated_to_trusted",
        CapabilityEvaluationDecision::DemoteToTrial => "demoted_to_trial_due_to_failures",
        CapabilityEvaluationDecision::DemoteToValidated => "demoted_to_validated_due_to_failures",
        CapabilityEvaluationDecision::Deprecate => "deprecated_due_to_safety_violation",
    }
}

fn api_capability_to_record(
    cap: &ApiCapability,
    status: CapabilityLifecycleStatus,
    score: f64,
) -> Result<CapabilityPackRecord, String> {
    let definition = api_capability_to_pack_definition(cap);
    CapabilityPackStore::validate_definition(&definition)?;
    let now = Utc::now().timestamp();

    let metadata = CapabilityPackMetadata {
        source: CapabilityPackSource::ApiMined,
        status,
        attempts: (cap.replay_success_count + cap.replay_failure_count) as u32,
        successes: cap.replay_success_count as u32,
        last_score: score,
        source_ref: Some(cap.id.clone()),
        created_at: now,
        updated_at: now,
    };

    Ok(CapabilityPackRecord {
        definition,
        metadata,
    })
}

fn api_capability_to_pack_definition(cap: &ApiCapability) -> CapabilityPackDefinition {
    let mut step_params: HashMap<String, String> = HashMap::new();
    step_params.insert("action".to_string(), "api_replay".to_string());
    step_params.insert("capability_id".to_string(), cap.id.clone());
    step_params.insert("method".to_string(), cap.method.to_uppercase());
    step_params.insert("url".to_string(), cap.url_template.clone());
    step_params.insert("timeout_ms".to_string(), "10000".to_string());
    step_params.insert(
        "read_only_hint".to_string(),
        (cap.effective_side_effects() == SideEffects::ReadOnly).to_string(),
    );
    if !cap.headers_template.is_empty() {
        if let Ok(raw) = serde_json::to_string(&cap.headers_template) {
            step_params.insert("headers".to_string(), raw);
        }
    }
    if let Some(body_template) = &cap.body_template {
        step_params.insert("body".to_string(), body_template.clone());
    }

    let mut seen_params = HashSet::new();
    let mut parameters = Vec::new();
    for name in extract_url_template_params(&cap.url_template) {
        if seen_params.insert(name.clone()) {
            parameters.push(ParameterDef {
                name,
                required: true,
                default: None,
                description: Some(
                    "Template parameter extracted from API capability URL template".to_string(),
                ),
                param_type: Some(ParameterType::String),
                aliases: Vec::new(),
                enum_values: None,
                schema: serde_json::Value::Null,
            });
        }
    }
    if let Some(body_template) = &cap.body_template {
        for param in extract_body_template_params(body_template) {
            if seen_params.insert(param.name.clone()) {
                parameters.push(ParameterDef {
                    name: param.name,
                    required: true,
                    default: None,
                    description: Some(
                        "Template parameter extracted from API capability request body".to_string(),
                    ),
                    param_type: Some(body_placeholder_kind_to_parameter_type(param.kind)),
                    aliases: Vec::new(),
                    enum_values: None,
                    schema: serde_json::Value::Null,
                });
            }
        }
    }

    CapabilityPackDefinition {
        name: generated_pack_name(cap),
        description: Some(format!(
            "Catalog-only mined API replay candidate for {} {}",
            cap.method.to_uppercase(),
            cap.url_template
        )),
        version: Some("0.1.0".to_string()),
        guide: Some(
            "Auto-generated API replay record from API mining. This legacy generated-pack surface is catalog-only until Capability Evolution has a real direct replay provider; live takeover uses ApiRouter and ApiRunner at the browser primitive boundary.".to_string(),
        ),
        native_action_schemas: HashMap::new(),
        parameters,
        implementation: ImplementationType::Composite {
            steps: vec![CompositeStep {
                tool: "browser".to_string(),
                parameters: step_params,
            }],
        },
        execution: Some(ExecutionMetadata {
            requires_browser_session: Some(true),
            default_timeout_secs: Some(30),
            chat_inline_adapter: None,
            categories: vec![
                "capability_evolution".to_string(),
                "api_replay".to_string(),
                "generated".to_string(),
            ],
            sandbox: Some("none".to_string()),
            composition_category: Some("api_replay".to_string()),
            spend: None,
        }),
        auth: None,
        reliability: None,
        result_projection: None,
    }
}

fn generated_pack_name(cap: &ApiCapability) -> String {
    let material = format!(
        "{}|{}|{}|{}",
        cap.method.to_uppercase(),
        cap.url_template,
        cap.body_fingerprint.clone().unwrap_or_default(),
        cap.graphql_operation.clone().unwrap_or_default(),
    ) + &format!(
        "|{}",
        cap.graphql_operation_kind
            .map(|kind| kind.as_str())
            .unwrap_or_default()
    );
    let hash = blake3::hash(material.as_bytes()).to_hex();
    let fragment = sanitize_name_fragment(&cap.name, 24);
    format!("api_replay_{}_{}", fragment, &hash[..10])
}

fn sanitize_name_fragment(input: &str, max_len: usize) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        out.push(normalized);
        if out.len() >= max_len {
            break;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "generated".to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_url_template_params(url_template: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let bytes = url_template.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'}' {
                end += 1;
            }
            if end < bytes.len() && end > start {
                let raw = &url_template[start..end];
                let name = raw.trim();
                if !name.is_empty() && seen.insert(name.to_string()) {
                    out.push(name.to_string());
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
    }

    out
}

fn body_placeholder_kind_to_parameter_type(kind: BodyPlaceholderKind) -> ParameterType {
    match kind {
        BodyPlaceholderKind::String => ParameterType::String,
        BodyPlaceholderKind::Number => ParameterType::Number,
        BodyPlaceholderKind::Boolean => ParameterType::Boolean,
        BodyPlaceholderKind::Json => ParameterType::String,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::{ApiCapability, GraphqlOperationKind};

    fn tmp_root() -> PathBuf {
        std::env::temp_dir().join(format!("capability-pack-test-{}", uuid::Uuid::new_v4()))
    }

    fn basic_record(name: &str) -> CapabilityPackRecord {
        CapabilityPackRecord {
            definition: CapabilityPackDefinition {
                name: name.to_string(),
                description: Some("test".to_string()),
                version: Some("0.1.0".to_string()),
                guide: None,
                native_action_schemas: HashMap::new(),
                parameters: vec![ParameterDef {
                    name: "cursor".to_string(),
                    required: true,
                    default: None,
                    description: Some("cursor".to_string()),
                    param_type: Some(ParameterType::String),
                    aliases: Vec::new(),
                    enum_values: None,
                    schema: serde_json::Value::Null,
                }],
                implementation: ImplementationType::Composite {
                    steps: vec![CompositeStep {
                        tool: "browser".to_string(),
                        parameters: HashMap::from([
                            ("action".to_string(), "open".to_string()),
                            (
                                "url".to_string(),
                                "https://example.com/sync?cursor={cursor}".to_string(),
                            ),
                        ]),
                    }],
                },
                execution: Some(ExecutionMetadata {
                    requires_browser_session: Some(true),
                    default_timeout_secs: Some(30),
                    chat_inline_adapter: None,
                    categories: vec!["generated".to_string()],
                    sandbox: Some("none".to_string()),
                    composition_category: Some("browser_automation".to_string()),
                    spend: None,
                }),
                auth: None,
                reliability: None,
                result_projection: None,
            },
            metadata: CapabilityPackMetadata {
                source: CapabilityPackSource::Generated,
                status: CapabilityLifecycleStatus::Trial,
                attempts: 0,
                successes: 0,
                last_score: 0.0,
                source_ref: None,
                created_at: 0,
                updated_at: 0,
            },
        }
    }

    #[test]
    fn capability_pack_load_reload_validation_coverage() {
        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);
        let record = basic_record("generated_sync");
        store
            .upsert_record(record, "seed")
            .expect("upsert should succeed");

        let catalog = store.load_catalog().expect("catalog should load");
        assert_eq!(catalog.packs.len(), 1);

        let runtime_without_trial = store
            .load_runtime_pack_defs(false, 32)
            .expect("runtime defs should load");
        assert_eq!(runtime_without_trial.len(), 0);

        let runtime_with_trial = store
            .load_runtime_pack_defs(true, 32)
            .expect("runtime defs should load");
        assert_eq!(runtime_with_trial.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remove_source_records_is_source_and_reference_scoped() {
        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);
        let mut selected = basic_record("recipe_selected");
        selected.metadata.source = CapabilityPackSource::TaskRecipe;
        selected.metadata.source_ref = Some("rcp_selected".into());
        let mut retained_recipe = basic_record("recipe_retained");
        retained_recipe.metadata.source = CapabilityPackSource::TaskRecipe;
        retained_recipe.metadata.source_ref = Some("rcp_retained".into());
        let retained_generated = basic_record("generated_retained");
        store.upsert_record(selected, "seed").unwrap();
        store.upsert_record(retained_recipe, "seed").unwrap();
        store.upsert_record(retained_generated, "seed").unwrap();

        let refs = HashSet::from(["rcp_selected".to_string()]);
        let removed = store
            .remove_source_records(CapabilityPackSource::TaskRecipe, Some(&refs))
            .unwrap();
        let catalog = store.load_catalog().unwrap();

        assert_eq!(removed.len(), 1);
        assert_eq!(
            removed[0].metadata.source_ref.as_deref(),
            Some("rcp_selected")
        );
        assert_eq!(catalog.packs.len(), 2);
        assert!(catalog
            .packs
            .iter()
            .any(|record| record.definition.name == "recipe_retained"));
        assert!(catalog
            .packs
            .iter()
            .any(|record| record.definition.name == "generated_retained"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_pack_trial_to_validated_requires_eval_thresholds() {
        let root = tmp_root();
        let bridge = CapabilityPromotionBridge::with_base_path(
            &root,
            CapabilityEvaluationThresholds::default(),
        );

        let mut capability = ApiCapability::new(
            "gmail_sync".to_string(),
            "https://mail.google.com".to_string(),
            "GET".to_string(),
            "https://mail.google.com/sync?cursor={cursor}".to_string(),
        );
        capability.sample_count = 5;
        capability.confidence = ConfidenceLevel::Candidate;
        capability.replay_success_count = 4;
        capability.replay_failure_count = 1;

        let report = bridge
            .promote_api_capabilities(&[capability], false, 16)
            .expect("promotion should succeed");
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(
            report.outcomes[0].next_status,
            CapabilityLifecycleStatus::Validated
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_pack_promotion_emits_audit_and_rollback_metadata() {
        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);

        let first = basic_record("generated_sync");
        store
            .upsert_record(first, "first")
            .expect("first upsert should succeed");

        let mut second = basic_record("generated_sync");
        second.metadata.status = CapabilityLifecycleStatus::Validated;
        let outcome = store
            .upsert_record(second, "promote")
            .expect("second upsert should succeed");
        assert!(!outcome.created);
        assert!(outcome.rollback_file.is_some());
        assert_eq!(
            outcome.previous_status,
            Some(CapabilityLifecycleStatus::Trial)
        );

        let audit_raw = fs::read_to_string(store.audit_path()).expect("audit file should exist");
        assert!(
            audit_raw.lines().count() >= 2,
            "expected at least two audit entries"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_pack_runtime_selection_prioritizes_trust_under_cap() {
        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);

        let mut trial = basic_record("trial_pack");
        trial.metadata.status = CapabilityLifecycleStatus::Trial;
        trial.metadata.last_score = 0.2;
        store
            .upsert_record(trial, "seed_trial")
            .expect("trial upsert should succeed");

        let mut trusted = basic_record("trusted_pack");
        trusted.metadata.status = CapabilityLifecycleStatus::Trusted;
        trusted.metadata.last_score = 0.95;
        store
            .upsert_record(trusted, "seed_trusted")
            .expect("trusted upsert should succeed");

        let runtime = store
            .load_runtime_pack_defs(true, 1)
            .expect("runtime defs should load");
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].name, "trusted_pack");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn api_mined_replay_packs_are_catalog_only_until_provider_exists() {
        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);

        let mut capability = ApiCapability::new(
            "search_api".to_string(),
            "https://api.example.com".to_string(),
            "GET".to_string(),
            "https://api.example.com/search?q={query}".to_string(),
        );
        capability.confidence = ConfidenceLevel::Trusted;
        capability.sample_count = 5;
        capability.replay_success_count = 5;

        let record =
            api_capability_to_record(&capability, CapabilityLifecycleStatus::Trusted, 0.99)
                .expect("api-mined record should build");
        assert!(is_legacy_browser_api_replay_definition(&record.definition));

        store
            .upsert_record(record, "seed_api_mined_replay")
            .expect("upsert should succeed");

        let runtime = store
            .load_runtime_pack_defs(true, 32)
            .expect("runtime defs should load");
        assert!(
            runtime.is_empty(),
            "legacy browser api_replay generated packs must stay out of runtime"
        );

        let catalog = store.load_catalog().expect("catalog should load");
        assert_eq!(catalog.packs.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn api_mined_replay_skill_emission_removes_stale_legacy_skill() {
        let root = tmp_root();
        let skills_root = root.join("skills");

        let mut capability = ApiCapability::new(
            "search_api".to_string(),
            "https://api.example.com".to_string(),
            "GET".to_string(),
            "https://api.example.com/search?q={query}".to_string(),
        );
        capability.confidence = ConfidenceLevel::Trusted;
        capability.sample_count = 5;
        capability.replay_success_count = 5;

        let record =
            api_capability_to_record(&capability, CapabilityLifecycleStatus::Trusted, 0.99)
                .expect("api-mined record should build");

        let stale_skill_dir = skills_root
            .join("evolved")
            .join(record.definition.name.clone());
        fs::create_dir_all(stale_skill_dir.join("scripts")).expect("stale dir should be writable");
        fs::write(stale_skill_dir.join("SKILL.md"), "stale").expect("stale skill should write");
        fs::write(stale_skill_dir.join("scripts").join("run.sh"), "stale")
            .expect("stale script should write");

        let outcome = crate::magician_v2::api_mining::skill_emitter::emit_evolved_skill(
            &skills_root,
            &record,
        );
        match outcome {
            crate::magician_v2::api_mining::skill_emitter::EmitOutcome::Skipped { reason } => {
                assert_eq!(reason, "api_mined_replay_skill_emission_disabled");
            },
            other => panic!("expected skipped emission, got {:?}", other),
        }
        assert!(
            !stale_skill_dir.exists(),
            "legacy evolved API replay skill folders should be removed on refresh"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_pack_upsert_waits_for_catalog_lock() {
        use fs2::FileExt;
        use std::time::Duration;

        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);
        let lock_path = store.catalog_path().with_file_name(".catalog.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).expect("lock directory should be creatable");
        }
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("lock file should open");
        lock_file
            .lock_exclusive()
            .expect("test should hold lock exclusively");
        let handle = std::thread::spawn({
            let store = store.clone();
            move || store.upsert_record(basic_record("locked_pack"), "lock_test")
        });

        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !handle.is_finished(),
            "upsert should block while catalog lock is held"
        );

        lock_file.unlock().expect("test lock should be releasable");

        let outcome = handle
            .join()
            .expect("upsert execution should join")
            .expect("upsert should succeed");
        assert_eq!(outcome.pack_name, "locked_pack");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_pack_load_catalog_waits_for_catalog_lock() {
        use fs2::FileExt;
        use std::time::Duration;

        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);
        let lock_path = store.catalog_path().with_file_name(".catalog.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).expect("lock directory should be creatable");
        }
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("lock file should open");
        lock_file
            .lock_exclusive()
            .expect("test should hold lock exclusively");
        let handle = std::thread::spawn({
            let store = store.clone();
            move || store.load_catalog()
        });

        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !handle.is_finished(),
            "load_catalog should block while catalog lock is held"
        );

        lock_file.unlock().expect("test lock should be releasable");

        let catalog = handle
            .join()
            .expect("load execution should join")
            .expect("load should succeed");
        assert_eq!(catalog.packs.len(), 0);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capability_pack_upsert_survives_audit_append_failure() {
        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);

        let audit_path = store.audit_path();
        if let Some(parent) = audit_path.parent() {
            fs::create_dir_all(parent).expect("audit parent should be creatable");
        }
        fs::create_dir(&audit_path).expect("audit path should be a directory to force append fail");

        let outcome = store
            .upsert_record(basic_record("audit_failure_pack"), "seed_audit_failure")
            .expect("upsert should succeed even when audit append fails");
        assert_eq!(outcome.pack_name, "audit_failure_pack");

        let catalog = store
            .load_catalog()
            .expect("catalog should remain persisted");
        assert!(
            catalog
                .packs
                .iter()
                .any(|record| record.definition.name == "audit_failure_pack"),
            "catalog should contain upserted pack despite audit append failure"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn api_capability_pack_definition_preserves_read_only_hint() {
        let mut capability = ApiCapability::new(
            "graphql_get_user".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/graphql".to_string(),
        );
        capability.graphql_operation = Some("GetUser".to_string());
        capability.graphql_operation_kind = Some(GraphqlOperationKind::Query);
        capability.refresh_side_effects();

        let definition = api_capability_to_pack_definition(&capability);
        let ImplementationType::Composite { steps } = definition.implementation else {
            panic!("expected composite implementation");
        };
        let params = &steps[0].parameters;
        assert_eq!(
            params.get("read_only_hint").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn generated_pack_name_distinguishes_graphql_operation_kind() {
        let mut query_capability = ApiCapability::new(
            "graphql_get_user".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/graphql".to_string(),
        );
        query_capability.graphql_operation = Some("GetUser".to_string());
        query_capability.graphql_operation_kind = Some(GraphqlOperationKind::Query);
        query_capability.refresh_side_effects();

        let mut mutation_capability = query_capability.clone();
        mutation_capability.graphql_operation_kind = Some(GraphqlOperationKind::Mutation);
        mutation_capability.refresh_side_effects();

        assert_ne!(
            generated_pack_name(&query_capability),
            generated_pack_name(&mutation_capability)
        );
    }

    /// Two threads upserting into the same catalog must leave a parseable
    /// `capability_packs.json` and no staging file.
    ///
    /// `upsert_record` takes an exclusive flock, so both packs are expected to
    /// survive; the assertion that matters here is that the durable writer's
    /// staging file never outlives a successful publish.
    #[test]
    fn capability_catalog_concurrent_upsert_leaves_no_staging_file() {
        use std::thread;

        let root = tmp_root();
        let store = CapabilityPackStore::with_base_path(&root);

        let handles: Vec<_> = ["concurrent_alpha", "concurrent_beta"]
            .into_iter()
            .map(|name| {
                let store = store.clone();
                thread::spawn(move || {
                    store
                        .upsert_record(basic_record(name), "concurrency probe")
                        .expect("upsert should succeed");
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let catalog_path = store.catalog_path();
        let catalog_dir = catalog_path.parent().expect("catalog parent directory");
        let staging: Vec<String> = fs::read_dir(catalog_dir)
            .expect("capability_evolution listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            staging.is_empty(),
            "durable writes must leave no staging file, found {staging:?}"
        );

        let published =
            fs::read_to_string(&catalog_path).expect("capability catalog should be readable");
        let parsed: CapabilityPackCatalog =
            serde_json::from_str(&published).expect("published catalog should parse");
        assert_eq!(parsed.packs.len(), 2);

        let _ = fs::remove_dir_all(root);
    }
}
