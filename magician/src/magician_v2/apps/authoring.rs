//! Provider-free, contract-derived app authoring loop.
//!
//! `magician app` deliberately shares the strict package admission, manifest,
//! dependency-lock and physical transfer codecs used by the server. Its
//! reports and lock are reproducibility evidence only: none of these types can
//! mint a scope, grant, approval, conformance attestation or installation.

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use super::{
    artifact_selection::{
        select_authoring_artifact, AppArtifactRequirement, AppArtifactSelectionDecision,
        AppArtifactSelectionInput,
    },
    authoring_catalog::{
        list_authoring_agents, list_authoring_personalities, list_authoring_procedures,
        list_authoring_tools, resolve_authoring_primitive_catalog, show_authoring_tool,
        AuthoringDiscoveryRoots, AuthoringToolKind, AuthoringToolListFilter,
    },
    capability_publication::{inspect_standalone_capability, APP_CAPABILITY_SKILL_MAX_BYTES},
    component_contract::validate_app_data_plane_component_contract,
    manifest::{
        AppBundlePath, AppManifestFeature, AppManifestField, AppManifestRunner,
        AppManifestWorkflow, AppPackageCandidate, AppPackageLimits, APP_AUTHORING_SDK_SEMVER,
        APP_AUTHORING_SDK_VERSION, APP_MANIFEST_SCHEMA_VERSION,
    },
    models::{
        AppDigest, AppName, AppReference, AppRevision, APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
    },
    package_lock::{
        lock_app_package_dependencies, AppPackageLock, AppPortablePackageLockClaim,
        AppVerifiedRegistryDependency,
    },
    package_staging::admit_package_directory,
    package_transfer::encode_package_archive,
    portability::{
        AppPackageArchiveManifest, AppPortablePackageMember, APP_PORTABLE_ARCHIVE_VERSION,
    },
    procedure_publication::{inspect_standalone_procedure, APP_PROCEDURE_SKILL_MAX_BYTES},
    recipe_ir::{
        compile_recipe_bundle, AppCompiledRecipeBundle, AppCompiledWorkflowValueSchema,
        AppRecipeBundleSource, AppWorkflowValueTypeNode,
    },
    registry::canonical_package_revision_ref_from_identity,
    surface_compiler::compile_app_surface_preview,
    tool_catalog::{complete_declared_tool_evidence, AppReviewedToolCatalog},
};
use crate::magician_v2::json_traversal::{
    canonical_json_bytes, json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded,
};

const DERIVED_JSON_PATH: &str = ".magician/app-derived.json";
const GENERATED_TYPESCRIPT_PATH: &str = "sdk/app.generated.ts";
const FIXTURE_PATH: &str = "fixtures/app-fixtures.json";
const AUTHORING_DERIVATION_VERSION: u8 = 1;
const AUTHORING_TYPESCRIPT_GENERATION_VERSION: u8 = 2;
const AUTHORING_FIXTURE_VERSION: u8 = 1;
const AUTHORING_RESOLUTION_VERSION: u8 = 1;
const APP_AUTHORING_GENERATOR_NAME: &str = "magician_app_cli";
const SCAFFOLD_APP_REQUIRED_FEATURES: &[AppManifestFeature] = &[
    AppManifestFeature::TypedEntitiesV1,
    AppManifestFeature::DeclarativeViewsV1,
    AppManifestFeature::OwnerDataPlaneV1,
];
const MAX_FIXTURE_BYTES: usize = 1_048_576;
const MAX_FIXTURE_ITEMS: usize = 1_024;
const MAX_FIXTURE_DEPTH: usize = 16;
const MAX_FIXTURE_NODES: usize = 20_000;
const BUILTIN_CONTRACT_REVISION: u64 = 1;
pub const APP_AUTHORING_CLI_JSON_PROTOCOL_VERSION: &str = "1.0.0";
const BUILTIN_CONTRACT_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../docs/contracts/app-platform/components/v1/contract.json"
));

#[derive(Subcommand, Debug)]
pub enum AppAuthoringCommand {
    /// Inspect or recover a stopped runtime's legacy fixture-encrypted Apps
    /// database. Requires local filesystem ownership and OS Keychain access.
    RecoverLegacyFixtureEncryption(AppEncryptionRecoveryArgs),
    /// Select the least-powerful durable artifact for structured product
    /// requirements.
    Select(AppSelectArgs),
    /// Scaffold a strict declarative app with empty `dependencies.tools`.
    Init(AppInitArgs),
    /// Strictly admit the package and verify generated derivations.
    Check(AppCheckArgs),
    /// Run provider-free entity, workflow-input and view fixtures.
    Test(AppTestArgs),
    /// Compile deterministic, fixture-backed MUIJ previews without activation
    /// authority.
    Preview(AppPreviewArgs),
    /// Check, test, lock compiled/catalog tools, digest and pack a package-only
    /// archive.
    Pack(AppPackArgs),
    /// List or show tools a package may declare in `dependencies.tools`.
    Tools {
        #[command(subcommand)]
        command: AppToolsAuthoringCommand,
    },
    /// List hireable workflow agents (`workflows.<id>.agent`).
    Agents {
        #[command(subcommand)]
        command: AppAgentsAuthoringCommand,
    },
    /// List personality runner selections (`workflows.<id>.personality`).
    Personalities {
        #[command(subcommand)]
        command: AppPersonalitiesAuthoringCommand,
    },
    /// Inspect standalone procedure skills without publishing or activating
    /// them.
    Procedure {
        #[command(subcommand)]
        command: AppProcedureAuthoringCommand,
    },
    /// Inspect a *new* typed tool SKILL.md. Existing skills and compiled packs
    /// do not need this.
    Capability {
        #[command(subcommand)]
        command: AppCapabilityAuthoringCommand,
    },
    /// Approve and enable a ready-for-review installation in the live
    /// workspace.
    Approve(AppApproveArgs),
    /// List a bounded page from the live Apps directory.
    List(AppListArgs),
    /// Read one installation from the live Apps owner.
    Detail(AppInstallationReadArgs),
    /// Read the exact install/update/reinstall review material.
    Review(AppInstallationReadArgs),
    /// Disable an enabled installation at an exact generation.
    Disable(AppLifecycleControlArgs),
    /// Quarantine an installation at an exact generation.
    Quarantine(AppLifecycleControlArgs),
    /// Uninstall an installation while retaining recoverable data.
    #[command(name = "uninstall-retain")]
    UninstallRetain(AppLifecycleControlArgs),
    /// Revoke the active grant with a caller-retained replay identity.
    #[command(name = "grant-revoke")]
    GrantRevoke(AppLifecycleControlArgs),
    /// Park an installation for a reviewed update.
    #[command(name = "update-begin")]
    UpdateBegin(AppLifecycleControlArgs),
    /// Abort a pending update and restore the prior lifecycle state.
    #[command(name = "update-abort")]
    UpdateAbort(AppLifecycleControlArgs),
    /// Compile and dry-run one exact update/reinstall migration plan.
    #[command(name = "update-plan")]
    UpdatePlan(AppUpdatePlanArgs),
    /// Write the exact encrypted pre-update backup and stage its generation.
    #[command(name = "update-backup")]
    UpdateBackup(AppUpdateBackupArgs),
    /// Roll back one switched code-only update without restoring permissions.
    #[command(name = "rollback-code")]
    RollbackCode(AppCodeRollbackArgs),
    /// Review conflict/new-local-ID decisions for an exact backup rewind.
    #[command(name = "rewind-preview")]
    RewindPreview(AppDataRewindPreviewArgs),
    /// Commit one explicitly confirmed reviewed data rewind.
    #[command(name = "rewind-commit")]
    RewindCommit(AppDataRewindCommitArgs),
    /// Read the exact current reviewed re-enable material.
    #[command(name = "reenable-review")]
    ReenableReview(AppInstallationReadArgs),
    /// Commit an exact reviewed re-enable transition.
    Reenable(AppReenableArgs),
    /// Export a package-only portable archive without overwriting a path.
    #[command(name = "package-export")]
    PackageExport(AppPackageExportArgs),
    /// Import a bounded package-only archive as inert local staging.
    #[command(name = "package-import")]
    PackageImport(AppPackageImportArgs),
    /// Re-run local conformance and publish an inert review candidate.
    #[command(name = "candidate-publish")]
    CandidatePublish(AppCandidatePublishArgs),
    /// Export data or a combined package+data archive.
    #[command(name = "data-export")]
    DataExport(AppDataExportArgs),
    /// Authenticate an archive and create a destination-bound import preview.
    #[command(name = "data-import-preview")]
    DataImportPreview(AppDataImportPreviewArgs),
    /// Approve one exact server-produced data import preview.
    #[command(name = "data-import-approve")]
    DataImportApprove(AppDataImportApproveArgs),
    /// Commit one exact reviewed data import without overwriting conflicts.
    #[command(name = "data-import-commit")]
    DataImportCommit(AppDataImportCommitArgs),
    /// Create a short-lived evidence-bound whole-installation purge preview.
    #[command(name = "purge-preview")]
    PurgePreview(AppPurgePreviewArgs),
    /// Commit the exact server-issued purge preview from a bounded JSON file.
    #[command(name = "purge-commit")]
    PurgeCommit(AppPurgeCommitArgs),
    /// Read a completed purge receipt by its caller-retained idempotency key.
    #[command(name = "purge-status")]
    PurgeStatus(AppPurgeStatusArgs),
}

impl AppAuthoringCommand {
    pub fn requires_live_workspace(&self) -> bool {
        matches!(
            self,
            Self::Approve(_)
                | Self::List(_)
                | Self::Detail(_)
                | Self::Review(_)
                | Self::Disable(_)
                | Self::Quarantine(_)
                | Self::UninstallRetain(_)
                | Self::GrantRevoke(_)
                | Self::UpdateBegin(_)
                | Self::UpdateAbort(_)
                | Self::UpdatePlan(_)
                | Self::UpdateBackup(_)
                | Self::RollbackCode(_)
                | Self::RewindPreview(_)
                | Self::RewindCommit(_)
                | Self::ReenableReview(_)
                | Self::Reenable(_)
                | Self::PackageExport(_)
                | Self::PackageImport(_)
                | Self::CandidatePublish(_)
                | Self::DataExport(_)
                | Self::DataImportPreview(_)
                | Self::DataImportApprove(_)
                | Self::DataImportCommit(_)
                | Self::PurgePreview(_)
                | Self::PurgeCommit(_)
                | Self::PurgeStatus(_)
        )
    }
}

#[derive(Args, Debug)]
pub struct AppEncryptionRecoveryArgs {
    #[arg(long)]
    pub root: PathBuf,
    #[arg(long)]
    pub principal: String,
    #[arg(long)]
    pub workspace: String,
    /// Commit recovery; without this flag the command only authenticates.
    #[arg(long, requires = "backup")]
    pub commit: bool,
    /// New encrypted backup file; never overwrites an existing path.
    #[arg(long)]
    pub backup: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum AppToolsAuthoringCommand {
    /// List tools plus the exact sealed agent/browser leaves with
    /// app-eligibility.
    List(AppToolsListArgs),
    /// Show one tool's eligibility, version and YAML declaration.
    Show(AppToolsShowArgs),
}

#[derive(Args, Debug)]
pub struct AppCatalogDiscoverArgs {
    /// Scope principal. Defaults to `anonymous` or `MAGICIAN_PRINCIPAL`.
    #[arg(long, default_value_t = default_catalog_principal())]
    pub principal: String,
    /// Scope workspace. Defaults to `default` or `MAGICIAN_WORKSPACE`.
    #[arg(long, default_value_t = default_catalog_workspace())]
    pub workspace: String,
    /// Override the scope skills root. Repo `skillshub/` is not searched.
    #[arg(long = "skills-dir")]
    pub skills_dirs: Vec<PathBuf>,
    /// Override agent-template roots
    /// (`agent_templates/agents/<id>/definition.agent.yaml`).
    #[arg(long = "templates-dir")]
    pub templates_dirs: Vec<PathBuf>,
}

impl Default for AppCatalogDiscoverArgs {
    fn default() -> Self {
        Self {
            principal: default_catalog_principal(),
            workspace: default_catalog_workspace(),
            skills_dirs: Vec::new(),
            templates_dirs: Vec::new(),
        }
    }
}

#[derive(Args, Debug)]
pub struct AppToolsListArgs {
    /// Only emit tools with an app-compatible typed shape. Mutable skills
    /// still require immutable reviewed evidence when `pack` builds the lock.
    #[arg(long)]
    pub app_eligible: bool,
    /// Restrict to compiled, skill, agent, or interactive authoring entries.
    #[arg(long, value_enum)]
    pub kind: Option<AuthoringToolKind>,
    #[command(flatten)]
    pub discover: AppCatalogDiscoverArgs,
}

#[derive(Args, Debug)]
pub struct AppToolsShowArgs {
    /// Real tool name (`content_read`, `next-step`, …).
    pub name: String,
    #[command(flatten)]
    pub discover: AppCatalogDiscoverArgs,
}

#[derive(Subcommand, Debug)]
pub enum AppAgentsAuthoringCommand {
    /// List the default runner plus discovered agent templates.
    List(AppCatalogDiscoverArgs),
}

#[derive(Subcommand, Debug)]
pub enum AppPersonalitiesAuthoringCommand {
    /// List personality-mode skills for `workflows.<id>.personality`.
    List(AppCatalogDiscoverArgs),
}

#[derive(Subcommand, Debug)]
pub enum AppProcedureAuthoringCommand {
    /// Validate one bounded SKILL.md through the registry publication parser.
    Check(AppProcedureCheckArgs),
    /// List procedure skills for `dependencies.procedure_skills`.
    List(AppCatalogDiscoverArgs),
}

#[derive(Args, Debug)]
pub struct AppApproveArgs {
    /// Ready-for-review installation id from candidate publication.
    pub installation_id: String,
    /// Exact `workflow_material_digest` returned by `app review`.
    #[arg(long)]
    pub review_digest: String,
    /// Grant only these requested tools. Omit to grant every requested tool.
    #[arg(long = "grant-tool")]
    pub grant_tools: Vec<String>,
    /// Grant only these requested agents. Omit to grant every requested agent.
    #[arg(long = "grant-agent")]
    pub grant_agents: Vec<String>,
    /// Grant only these requested personalities. Omit to grant every requested
    /// personality.
    #[arg(long = "grant-personality")]
    pub grant_personalities: Vec<String>,
    /// Exact update coordinator run for update/reinstall approval.
    #[arg(long, requires = "update_plan_digest")]
    pub migration_run_id: Option<String>,
    /// Exact digest returned by `app update-plan`/`app update-backup`.
    #[arg(long, requires = "migration_run_id")]
    pub update_plan_digest: Option<String>,
    /// Confirm the exact destructive operation bodies shown in the plan.
    #[arg(long)]
    pub confirm_destructive_migration: bool,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug, Clone)]
pub struct AppLiveScopeArgs {
    /// Trusted loopback Magician API base URL.
    #[arg(long, default_value = "http://127.0.0.1:3002")]
    pub api_base: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AppListSectionArg {
    Installed,
    Pinned,
    Recent,
    NeedsAttention,
    Disabled,
    Recovery,
}

impl AppListSectionArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Pinned => "pinned",
            Self::Recent => "recent",
            Self::NeedsAttention => "needs_attention",
            Self::Disabled => "disabled",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Args, Debug)]
pub struct AppListArgs {
    /// Directory section to read.
    #[arg(long, value_enum, default_value = "installed")]
    pub section: AppListSectionArg,
    /// Optional bounded metadata search.
    #[arg(long)]
    pub search: Option<String>,
    /// Maximum rows in this page (1-100).
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: u16,
    /// Opaque cursor returned by a prior list call.
    #[arg(long)]
    pub cursor: Option<String>,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppInstallationReadArgs {
    pub installation_id: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppLifecycleControlArgs {
    pub installation_id: String,
    /// Exact generation displayed by `app detail` or `app list`.
    #[arg(long)]
    pub expected_generation: u64,
    /// Caller-retained identity; exact retries must reuse this value.
    #[arg(long)]
    pub request_id: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppReenableArgs {
    pub installation_id: String,
    #[arg(long)]
    pub expected_generation: u64,
    #[arg(long)]
    pub request_id: String,
    /// Exact `review_digest` returned by `app reenable-review`.
    #[arg(long)]
    pub review_digest: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppPackageExportArgs {
    pub installation_id: String,
    /// New archive path. Existing files and symlinks are never overwritten.
    #[arg(long)]
    pub output: PathBuf,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppPackageImportArgs {
    /// Bounded regular package-only ZIP archive; symlinks are rejected.
    pub archive: PathBuf,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppCandidatePublishArgs {
    /// Exact bounded package-only ZIP archive to conform and publish.
    pub archive: PathBuf,
    /// Caller-retained request identity; exact retries reuse the same value.
    #[arg(long)]
    pub request_id: String,
    /// Parked existing installation for update/reinstall publication.
    #[arg(long, requires = "attempt_kind")]
    pub installation_id: Option<String>,
    /// Existing-installation publication kind; initial install omits both.
    #[arg(long, value_enum, requires = "installation_id")]
    pub attempt_kind: Option<AppRevisionCandidateKindArg>,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AppRevisionCandidateKindArg {
    Update,
    Reinstall,
}

impl AppRevisionCandidateKindArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Reinstall => "reinstall",
        }
    }
}

#[derive(Args, Debug)]
pub struct AppUpdatePlanArgs {
    pub installation_id: String,
    #[arg(long)]
    pub attempt_id: String,
    #[arg(long)]
    pub expected_generation: u64,
    /// Optional JSON array containing exact migration operation bodies.
    #[arg(long)]
    pub operations_file: Option<PathBuf>,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppUpdateBackupArgs {
    pub migration_run_id: String,
    #[arg(long)]
    pub passphrase_file: PathBuf,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppCodeRollbackArgs {
    pub installation_id: String,
    #[arg(long)]
    pub migration_run_id: String,
    #[arg(long)]
    pub expected_generation: u64,
    #[arg(long)]
    pub request_id: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppDataRewindPreviewArgs {
    pub migration_run_id: String,
    #[arg(long)]
    pub passphrase_file: PathBuf,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppDataRewindCommitArgs {
    pub migration_run_id: String,
    #[arg(long)]
    pub preview_digest: String,
    #[arg(long)]
    pub request_id: String,
    #[arg(long)]
    pub passphrase_file: PathBuf,
    #[arg(long)]
    pub confirm_data_rewind: bool,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AppPortableExportKindArg {
    Data,
    Combined,
}

impl AppPortableExportKindArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Combined => "combined",
        }
    }
}

#[derive(Args, Debug)]
pub struct AppDataExportArgs {
    pub installation_id: String,
    #[arg(long, value_enum, default_value = "data")]
    pub kind: AppPortableExportKindArg,
    #[arg(long)]
    pub request_id: String,
    /// New destination path. Existing paths and symlinks are never overwritten.
    #[arg(long)]
    pub output: PathBuf,
    /// Regular file containing the archive passphrase. Required by default.
    #[arg(long, conflicts_with = "explicit_plaintext")]
    pub passphrase_file: Option<PathBuf>,
    /// Separate warned plaintext action. Secret-class data is still denied.
    #[arg(long)]
    pub explicit_plaintext: bool,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppDataImportPreviewArgs {
    pub installation_id: String,
    pub archive: PathBuf,
    #[arg(long)]
    pub request_id: String,
    #[arg(long)]
    pub passphrase_file: Option<PathBuf>,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppDataImportApproveArgs {
    pub installation_id: String,
    #[arg(long)]
    pub preview_digest: String,
    #[arg(long)]
    pub request_id: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppDataImportCommitArgs {
    pub installation_id: String,
    #[arg(long)]
    pub preview_digest: String,
    #[arg(long)]
    pub approval_ref: String,
    #[arg(long)]
    pub request_id: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppPurgePreviewArgs {
    pub installation_id: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppPurgeCommitArgs {
    pub installation_id: String,
    /// JSON file containing either the raw preview or a prior CLI envelope.
    #[arg(long)]
    pub preview: PathBuf,
    /// Caller-retained `blake3:<hex>` key; exact retries must reuse it.
    #[arg(long)]
    pub idempotency_key: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppPurgeStatusArgs {
    /// Caller-retained `blake3:<hex>` key used by `purge-commit`.
    pub idempotency_key: String,
    #[command(flatten)]
    pub live: AppLiveScopeArgs,
}

#[derive(Args, Debug)]
pub struct AppProcedureCheckArgs {
    /// Standalone procedure SKILL.md.
    #[arg(default_value = "SKILL.md")]
    pub path: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum AppCapabilityAuthoringCommand {
    /// Validate a new tool SKILL.md (USR + typed actions). Not required to
    /// reuse an existing tool.
    Check(AppCapabilityCheckArgs),
}

#[derive(Args, Debug)]
pub struct AppCapabilityCheckArgs {
    /// New tool SKILL.md. Existing skillshub skills and compiled packs are
    /// declared by name instead.
    #[arg(default_value = "SKILL.md")]
    pub path: PathBuf,
}

#[derive(Args, Debug)]
pub struct AppSelectArgs {
    /// One structured product requirement. Repeat for every required behavior.
    /// `new-executable-integration` is only for a tool that does not already
    /// exist.
    #[arg(long = "require", value_enum, required = true)]
    pub requirements: Vec<AppArtifactRequirement>,
}

#[derive(Args, Debug)]
pub struct AppInitArgs {
    /// Canonical package name. The scaffold starts with `dependencies.tools:
    /// []`.
    pub name: String,
    /// Destination directory. Defaults to the package name.
    #[arg(long)]
    pub path: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct AppCheckArgs {
    /// App package directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Replace generated derivations with the canonical current output.
    #[arg(long)]
    pub write_generated: bool,
}

#[derive(Args, Debug)]
pub struct AppTestArgs {
    /// App package directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

#[derive(Args, Debug)]
pub struct AppPreviewArgs {
    /// App package directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Compile only one declared view.
    #[arg(long)]
    pub view: Option<String>,
}

#[derive(Args, Debug)]
pub struct AppPackArgs {
    /// App package directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Package publisher identity. Import still requires local identity review.
    #[arg(long, default_value = "publisher:local-author")]
    pub publisher: String,
    /// Optional fixture for registry/procedure revisions. Compiled packs and
    /// catalog skills are snapshotted automatically.
    #[arg(long)]
    pub resolutions: Option<PathBuf>,
    /// Output package path. Defaults beside the package directory.
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Use the same scoped/explicit primitive resolver as `tools list/show`.
    #[command(flatten)]
    pub discover: AppCatalogDiscoverArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAuthoringCliExitStatus {
    Success,
    CommandFailed,
}

#[derive(Serialize)]
struct AppAuthoringCliSuccess<'a> {
    schema_version: u32,
    cli_protocol_version: &'static str,
    command: &'static str,
    ok: bool,
    result: &'a serde_json::Value,
}

#[derive(Serialize)]
struct AppAuthoringCliFailure<'a> {
    schema_version: u32,
    cli_protocol_version: &'static str,
    command: &'static str,
    ok: bool,
    error: AppAuthoringCliError<'a>,
}

#[derive(Serialize)]
struct AppAuthoringCliError<'a> {
    code: &'a str,
    message: &'a str,
}

pub fn app_authoring_command_name(command: &AppAuthoringCommand) -> &'static str {
    match command {
        AppAuthoringCommand::RecoverLegacyFixtureEncryption(_) => {
            "recover-legacy-fixture-encryption"
        },
        AppAuthoringCommand::Select(_) => "select",
        AppAuthoringCommand::Init(_) => "init",
        AppAuthoringCommand::Check(_) => "check",
        AppAuthoringCommand::Test(_) => "test",
        AppAuthoringCommand::Preview(_) => "preview",
        AppAuthoringCommand::Pack(_) => "pack",
        AppAuthoringCommand::Tools { command } => match command {
            AppToolsAuthoringCommand::List(_) => "tools.list",
            AppToolsAuthoringCommand::Show(_) => "tools.show",
        },
        AppAuthoringCommand::Agents { .. } => "agents.list",
        AppAuthoringCommand::Personalities { .. } => "personalities.list",
        AppAuthoringCommand::Procedure { command } => match command {
            AppProcedureAuthoringCommand::Check(_) => "procedure.check",
            AppProcedureAuthoringCommand::List(_) => "procedure.list",
        },
        AppAuthoringCommand::Capability { .. } => "capability.check",
        AppAuthoringCommand::Approve(_) => "approve",
        AppAuthoringCommand::List(_) => "list",
        AppAuthoringCommand::Detail(_) => "detail",
        AppAuthoringCommand::Review(_) => "review",
        AppAuthoringCommand::Disable(_) => "disable",
        AppAuthoringCommand::Quarantine(_) => "quarantine",
        AppAuthoringCommand::UninstallRetain(_) => "uninstall-retain",
        AppAuthoringCommand::GrantRevoke(_) => "grant-revoke",
        AppAuthoringCommand::UpdateBegin(_) => "update-begin",
        AppAuthoringCommand::UpdateAbort(_) => "update-abort",
        AppAuthoringCommand::UpdatePlan(_) => "update-plan",
        AppAuthoringCommand::UpdateBackup(_) => "update-backup",
        AppAuthoringCommand::RollbackCode(_) => "rollback-code",
        AppAuthoringCommand::RewindPreview(_) => "rewind-preview",
        AppAuthoringCommand::RewindCommit(_) => "rewind-commit",
        AppAuthoringCommand::ReenableReview(_) => "reenable-review",
        AppAuthoringCommand::Reenable(_) => "reenable",
        AppAuthoringCommand::PackageExport(_) => "package-export",
        AppAuthoringCommand::PackageImport(_) => "package-import",
        AppAuthoringCommand::CandidatePublish(_) => "candidate-publish",
        AppAuthoringCommand::DataExport(_) => "data-export",
        AppAuthoringCommand::DataImportPreview(_) => "data-import-preview",
        AppAuthoringCommand::DataImportApprove(_) => "data-import-approve",
        AppAuthoringCommand::DataImportCommit(_) => "data-import-commit",
        AppAuthoringCommand::PurgePreview(_) => "purge-preview",
        AppAuthoringCommand::PurgeCommit(_) => "purge-commit",
        AppAuthoringCommand::PurgeStatus(_) => "purge-status",
    }
}

fn app_authoring_report(command: &AppAuthoringCommand) -> Result<serde_json::Value> {
    let report = match command {
        AppAuthoringCommand::RecoverLegacyFixtureEncryption(args) => {
            super::registry::recover_legacy_fixture_encryption(args)?
        },
        AppAuthoringCommand::Select(args) => serde_json::to_value(select_project_artifact(args)?)?,
        AppAuthoringCommand::Init(args) => serde_json::to_value(init_project(args)?)?,
        AppAuthoringCommand::Check(args) => {
            serde_json::to_value(check_project(&args.path, args.write_generated)?)?
        },
        AppAuthoringCommand::Test(args) => serde_json::to_value(test_project(&args.path)?)?,
        AppAuthoringCommand::Preview(args) => serde_json::to_value(preview_project(args)?)?,
        AppAuthoringCommand::Pack(args) => serde_json::to_value(pack_project(args)?)?,
        AppAuthoringCommand::Tools { command } => match command {
            AppToolsAuthoringCommand::List(args) => serde_json::to_value(list_authoring_tools(
                &discovery_roots(&args.discover),
                AuthoringToolListFilter {
                    app_eligible_only: args.app_eligible,
                    kind: args.kind,
                },
            ))?,
            AppToolsAuthoringCommand::Show(args) => serde_json::to_value(show_authoring_tool(
                &discovery_roots(&args.discover),
                &args.name,
            )?)?,
        },
        AppAuthoringCommand::Agents { command } => match command {
            AppAgentsAuthoringCommand::List(args) => {
                serde_json::to_value(list_authoring_agents(&discovery_roots(args)))?
            },
        },
        AppAuthoringCommand::Personalities { command } => match command {
            AppPersonalitiesAuthoringCommand::List(args) => {
                serde_json::to_value(list_authoring_personalities(&discovery_roots(args)))?
            },
        },
        AppAuthoringCommand::Procedure { command } => match command {
            AppProcedureAuthoringCommand::Check(args) => {
                let bytes = read_bounded_file(&args.path, APP_PROCEDURE_SKILL_MAX_BYTES)?;
                serde_json::to_value(inspect_standalone_procedure(&bytes)?)?
            },
            AppProcedureAuthoringCommand::List(args) => {
                serde_json::to_value(list_authoring_procedures(&discovery_roots(args)))?
            },
        },
        AppAuthoringCommand::Capability { command } => match command {
            AppCapabilityAuthoringCommand::Check(args) => {
                let bytes = read_bounded_file(&args.path, APP_CAPABILITY_SKILL_MAX_BYTES)?;
                serde_json::to_value(inspect_standalone_capability(&bytes)?)?
            },
        },
        AppAuthoringCommand::Approve(_)
        | AppAuthoringCommand::List(_)
        | AppAuthoringCommand::Detail(_)
        | AppAuthoringCommand::Review(_)
        | AppAuthoringCommand::Disable(_)
        | AppAuthoringCommand::Quarantine(_)
        | AppAuthoringCommand::UninstallRetain(_)
        | AppAuthoringCommand::GrantRevoke(_)
        | AppAuthoringCommand::UpdateBegin(_)
        | AppAuthoringCommand::UpdateAbort(_)
        | AppAuthoringCommand::UpdatePlan(_)
        | AppAuthoringCommand::UpdateBackup(_)
        | AppAuthoringCommand::RollbackCode(_)
        | AppAuthoringCommand::RewindPreview(_)
        | AppAuthoringCommand::RewindCommit(_)
        | AppAuthoringCommand::ReenableReview(_)
        | AppAuthoringCommand::Reenable(_)
        | AppAuthoringCommand::PackageExport(_)
        | AppAuthoringCommand::PackageImport(_)
        | AppAuthoringCommand::CandidatePublish(_)
        | AppAuthoringCommand::DataExport(_)
        | AppAuthoringCommand::DataImportPreview(_)
        | AppAuthoringCommand::DataImportApprove(_)
        | AppAuthoringCommand::DataImportCommit(_)
        | AppAuthoringCommand::PurgePreview(_)
        | AppAuthoringCommand::PurgeCommit(_)
        | AppAuthoringCommand::PurgeStatus(_) => {
            anyhow::bail!(
                "this `magician app` command needs the authenticated live API after Tokio starts; \
                 it is not a provider-free authoring command"
            )
        },
    };
    Ok(report)
}

fn public_cli_error(error: &anyhow::Error) -> (&'static str, &'static str) {
    if let Some(error) = error.downcast_ref::<AppAuthoringError>() {
        return match error {
            AppAuthoringError::InvalidName(_) => {
                ("invalid_app_name", "The app package name is invalid.")
            },
            AppAuthoringError::DestinationExists(_) => (
                "app_destination_exists",
                "The app authoring destination already exists.",
            ),
            AppAuthoringError::UnsafePath(_) => (
                "unsafe_app_path",
                "The app authoring path is unsafe or unavailable.",
            ),
            AppAuthoringError::GeneratedDrift(_, _) => (
                "generated_artifact_drift",
                "A generated app artifact is missing or stale; run app check with \
                 --write-generated.",
            ),
            AppAuthoringError::InvalidFixture(_) => (
                "invalid_app_fixture",
                "The provider-free app fixture suite is invalid.",
            ),
            AppAuthoringError::InvalidResolution(_) => (
                "invalid_dependency_resolution",
                "The dependency resolution fixture is invalid.",
            ),
            AppAuthoringError::MissingContractCompatibility => (
                "missing_contract_compatibility",
                "The package is missing the required Magician contract compatibility entry.",
            ),
            AppAuthoringError::OutputExists(_) => (
                "app_output_exists",
                "The requested app package output already exists.",
            ),
        };
    }
    if let Some(error) = error.downcast_ref::<std::io::Error>() {
        return match error.kind() {
            std::io::ErrorKind::NotFound => (
                "app_input_not_found",
                "A required local app input or Magician configuration was not found.",
            ),
            std::io::ErrorKind::PermissionDenied => (
                "app_input_permission_denied",
                "Magician could not access a required local app input.",
            ),
            _ => (
                "app_authoring_io_failed",
                "A bounded local app authoring I/O operation failed.",
            ),
        };
    }
    (
        "app_authoring_command_failed",
        "The app authoring command failed without granting runtime authority.",
    )
}

fn render_app_authoring_json_result(
    command: &AppAuthoringCommand,
    result: Result<serde_json::Value>,
) -> Result<(String, AppAuthoringCliExitStatus)> {
    let command = app_authoring_command_name(command);
    match result {
        Ok(result) => Ok((
            serde_json::to_string(&AppAuthoringCliSuccess {
                schema_version: 1,
                cli_protocol_version: APP_AUTHORING_CLI_JSON_PROTOCOL_VERSION,
                command,
                ok: true,
                result: &result,
            })?,
            AppAuthoringCliExitStatus::Success,
        )),
        Err(error) => {
            let (code, message) = public_cli_error(&error);
            Ok((
                serde_json::to_string(&AppAuthoringCliFailure {
                    schema_version: 1,
                    cli_protocol_version: APP_AUTHORING_CLI_JSON_PROTOCOL_VERSION,
                    command,
                    ok: false,
                    error: AppAuthoringCliError { code, message },
                })?,
                AppAuthoringCliExitStatus::CommandFailed,
            ))
        },
    }
}

pub fn emit_app_authoring_result(
    command: &AppAuthoringCommand,
    json: bool,
    result: Result<serde_json::Value>,
) -> Result<AppAuthoringCliExitStatus> {
    if json {
        let (line, status) = render_app_authoring_json_result(command, result)?;
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{line}")?;
        stdout.flush()?;
        return Ok(status);
    }
    let report = result?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", serde_json::to_string_pretty(&report)?)?;
    stdout.flush()?;
    Ok(AppAuthoringCliExitStatus::Success)
}

pub fn run_app_authoring_command_with_output(
    command: &AppAuthoringCommand,
    json: bool,
) -> Result<AppAuthoringCliExitStatus> {
    emit_app_authoring_result(command, json, app_authoring_report(command))
}

pub fn run_app_authoring_command(command: &AppAuthoringCommand) -> Result<()> {
    let status = run_app_authoring_command_with_output(command, false)?;
    debug_assert_eq!(status, AppAuthoringCliExitStatus::Success);
    Ok(())
}

fn default_catalog_principal() -> String {
    std::env::var("MAGICIAN_PRINCIPAL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "anonymous".to_string())
}

fn default_catalog_workspace() -> String {
    std::env::var("MAGICIAN_WORKSPACE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

fn discovery_roots(args: &AppCatalogDiscoverArgs) -> AuthoringDiscoveryRoots {
    if args.skills_dirs.is_empty() && args.templates_dirs.is_empty() {
        return AuthoringDiscoveryRoots::for_live_scope(&args.principal, &args.workspace);
    }
    AuthoringDiscoveryRoots::from_explicit(
        args.skills_dirs.iter().cloned(),
        args.templates_dirs.iter().cloned(),
    )
}

fn select_project_artifact(args: &AppSelectArgs) -> Result<AppArtifactSelectionDecision> {
    select_authoring_artifact(AppArtifactSelectionInput {
        requirements: args.requirements.iter().copied().collect(),
    })
    .map_err(Into::into)
}

#[derive(Debug, Serialize)]
struct AppInitReport {
    status: &'static str,
    package_name: String,
    path: PathBuf,
    manifest_version: &'static str,
    /// Deprecated compatibility field retained for existing CLI consumers.
    sdk_version: &'static str,
    sdk_version_is_authority: bool,
    generated_by_sdk: &'static str,
    generated_by_version: &'static str,
    required_features: &'static [AppManifestFeature],
    fixture_path: &'static str,
}

#[derive(Debug, Serialize)]
struct AppCheckReport {
    status: &'static str,
    package_name: String,
    semantic_version: String,
    manifest_digest: AppDigest,
    bundle_digest: AppDigest,
    generated_artifacts: &'static str,
    entity_count: usize,
    view_count: usize,
    workflow_count: usize,
    action_count: usize,
    policy_diff: Value,
    runtime_evidence: Value,
}

#[derive(Debug, Serialize)]
struct AppTestReport {
    status: &'static str,
    package_name: String,
    fixture_file: &'static str,
    entity_fixtures: usize,
    workflow_fixtures: usize,
    view_fixtures: usize,
}

#[derive(Debug, Serialize)]
struct AppPreviewReport {
    status: &'static str,
    package_name: String,
    authoritative: bool,
    activation_capable: bool,
    previews: Vec<AppViewPreview>,
}

#[derive(Debug, Serialize)]
struct AppViewPreview {
    view_id: String,
    route: String,
    fixture_records: Vec<Value>,
    surface: Value,
}

#[derive(Debug, Serialize)]
struct AppPackReport {
    status: &'static str,
    package_name: String,
    semantic_version: String,
    output: PathBuf,
    archive_bytes: usize,
    manifest_digest: AppDigest,
    bundle_digest: AppDigest,
    dependency_lock_digest: AppDigest,
    package_payload_digest: AppDigest,
    server_revalidation_required: bool,
    authority_transferred: bool,
}

#[derive(Debug, Error)]
enum AppAuthoringError {
    #[error("invalid app package name: {0}")]
    InvalidName(String),
    #[error("app authoring destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("app authoring path is unsafe: {0}")]
    UnsafePath(String),
    #[error(
        "generated app artifact is missing or stale: {0}; run `magician app check \
         --write-generated {1}`"
    )]
    GeneratedDrift(&'static str, PathBuf),
    #[error("fixture suite is invalid: {0}")]
    InvalidFixture(String),
    #[error("dependency resolution fixture is invalid: {0}")]
    InvalidResolution(String),
    #[error("app package requires the canonical `magician_contract` compatibility entry")]
    MissingContractCompatibility,
    #[error("output already exists: {0}")]
    OutputExists(PathBuf),
}

fn init_project(args: &AppInitArgs) -> Result<AppInitReport> {
    let name = AppName::parse(args.name.clone())
        .map_err(|error| AppAuthoringError::InvalidName(error.to_string()))?;
    let destination = args
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from(name.as_str()));
    if destination.exists() {
        return Err(AppAuthoringError::DestinationExists(destination).into());
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(AppAuthoringError::UnsafePath(format!(
            "destination parent '{}' is not an existing directory",
            parent.display()
        ))
        .into());
    }
    fs::create_dir(&destination).with_context(|| format!("creating {}", destination.display()))?;
    create_directory(&destination.join("fixtures"))?;
    create_directory(&destination.join("sdk"))?;
    create_directory(&destination.join(".magician"))?;
    write_new_file(
        &destination.join("SKILL.md"),
        scaffold_skill_document(&name).as_bytes(),
    )?;
    write_new_file(
        &destination.join(FIXTURE_PATH),
        scaffold_fixture_document(&name).as_bytes(),
    )?;
    let _ = check_project(&destination, true)?;
    Ok(AppInitReport {
        status: "created",
        package_name: name.to_string(),
        path: destination,
        manifest_version: APP_MANIFEST_SCHEMA_VERSION,
        sdk_version: APP_AUTHORING_SDK_VERSION,
        sdk_version_is_authority: false,
        generated_by_sdk: APP_AUTHORING_GENERATOR_NAME,
        generated_by_version: APP_AUTHORING_SDK_SEMVER,
        required_features: SCAFFOLD_APP_REQUIRED_FEATURES,
        fixture_path: FIXTURE_PATH,
    })
}

fn check_project(path: &Path, write_generated: bool) -> Result<AppCheckReport> {
    let package_root = canonical_package_root(path)?;
    let mut candidate = admit_package_directory(&package_root)
        .with_context(|| format!("admitting app package {}", package_root.display()))?;
    ensure_contract_compatibility(&candidate)?;
    let generated = generated_artifacts(&candidate)?;
    if write_generated {
        for artifact in &generated {
            atomic_replace(&package_root.join(artifact.path), &artifact.bytes)?;
        }
        candidate = admit_package_directory(&package_root).with_context(|| {
            format!(
                "re-admitting app package after generated output {}",
                package_root.display()
            )
        })?;
        ensure_contract_compatibility(&candidate)?;
        ensure_generated_current(&package_root, &candidate)?;
    } else {
        ensure_generated_current(&package_root, &candidate)?;
    }
    let manifest = candidate.manifest().manifest();
    Ok(AppCheckReport {
        status: "valid",
        package_name: manifest.name.to_string(),
        semantic_version: manifest.version.clone(),
        manifest_digest: candidate.manifest().manifest_digest().clone(),
        bundle_digest: candidate.bundle_digest().clone(),
        generated_artifacts: if write_generated {
            "updated"
        } else {
            "current"
        },
        entity_count: manifest.app.entities.len(),
        view_count: manifest.app.views.len(),
        workflow_count: manifest.app.workflows.len(),
        action_count: manifest.app.actions.len(),
        policy_diff: authoring_policy_diff(&candidate)?,
        runtime_evidence: json!({
            "recipe_contract": super::runtime_contract::RECIPE_CONTRACT_V1,
            "recipe_source": super::recipe_lowering::app_recipe_runtime_source_digest()?,
            "compiled_owner_contract": super::runtime_contract::COMPILED_OWNER_CONTRACT_V1,
            "compiled_owner_source": super::app_tool_bind::compiled_runtime_source_digest(),
        }),
    })
}

fn test_project(path: &Path) -> Result<AppTestReport> {
    let package_root = canonical_package_root(path)?;
    let candidate = admit_package_directory(&package_root)
        .with_context(|| format!("admitting app package {}", package_root.display()))?;
    ensure_generated_current(&package_root, &candidate)?;
    test_candidate(&candidate)
}

fn test_candidate(candidate: &AppPackageCandidate) -> Result<AppTestReport> {
    ensure_contract_compatibility(candidate)?;
    let fixtures = load_fixture_suite(candidate)?;
    validate_fixture_suite(candidate, &fixtures)?;
    Ok(AppTestReport {
        status: "passed",
        package_name: candidate.manifest().manifest().name.to_string(),
        fixture_file: FIXTURE_PATH,
        entity_fixtures: fixtures.records.len(),
        workflow_fixtures: fixtures.workflows.len(),
        view_fixtures: fixtures.views.len(),
    })
}

/// Server-side provider-free conformance shared by SDK uploads and VibeDev
/// candidates. This deliberately returns only success/failure: the CLI report
/// is presentation, while the reviewed-candidate service mints the trusted
/// conformance identity after rerunning these exact bounded fixtures.
pub fn verify_provider_free_candidate(
    candidate: &AppPackageCandidate,
) -> std::result::Result<(), String> {
    ensure_generated_current(Path::new("."), candidate).map_err(|error| error.to_string())?;
    test_candidate(candidate)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn load_fixture_suite(candidate: &AppPackageCandidate) -> Result<AppFixtureSuite> {
    let fixture_path = AppBundlePath::parse(FIXTURE_PATH)?;
    let fixture_bytes = candidate
        .member(&fixture_path)
        .ok_or_else(|| {
            AppAuthoringError::InvalidFixture(format!(
                "required package member `{FIXTURE_PATH}` is missing"
            ))
        })?
        .bytes();
    if fixture_bytes.len() > MAX_FIXTURE_BYTES {
        return Err(AppAuthoringError::InvalidFixture(format!(
            "fixture suite exceeds its {MAX_FIXTURE_BYTES} byte limit"
        ))
        .into());
    }
    if !json_bytes_nesting_is_bounded(fixture_bytes, MAX_FIXTURE_DEPTH)
        || !json_bytes_nodes_are_bounded(fixture_bytes, MAX_FIXTURE_NODES)
    {
        return Err(
            AppAuthoringError::InvalidFixture("JSON depth/node limit exceeded".to_owned()).into(),
        );
    }
    let fixtures: AppFixtureSuite = serde_json::from_slice(fixture_bytes)
        .map_err(|error| AppAuthoringError::InvalidFixture(error.to_string()))?;
    Ok(fixtures)
}

fn preview_project(args: &AppPreviewArgs) -> Result<AppPreviewReport> {
    let package_root = canonical_package_root(&args.path)?;
    let candidate = admit_package_directory(&package_root)
        .with_context(|| format!("admitting app package {}", package_root.display()))?;
    ensure_generated_current(&package_root, &candidate)?;
    ensure_contract_compatibility(&candidate)?;
    let fixtures = load_fixture_suite(&candidate)?;
    validate_fixture_suite(&candidate, &fixtures)?;
    let selected_view = args
        .view
        .as_ref()
        .map(|view| AppName::parse(view.clone()))
        .transpose()
        .context("parsing preview view")?;
    let manifest = candidate.manifest();
    if selected_view
        .as_ref()
        .is_some_and(|view| !manifest.manifest().app.views.contains_key(view))
    {
        return Err(AppAuthoringError::InvalidFixture(format!(
            "preview names unknown view `{}`",
            selected_view.as_ref().expect("checked selected view")
        ))
        .into());
    }
    let digest = manifest
        .manifest_digest()
        .as_str()
        .strip_prefix("blake3:")
        .context("canonical manifest digest has no prefix")?;
    let installation_id =
        super::models::AppInstallationId::parse(format!("preview_{}", &digest[..24]))?;
    let revision = AppRevision::new(1)?;
    let compiled_at = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0)
        .context("constructing deterministic preview time")?;
    let mut previews = Vec::new();
    for (view_id, view) in &manifest.manifest().app.views {
        if selected_view
            .as_ref()
            .is_some_and(|selected| selected != view_id)
        {
            continue;
        }
        let surface = compile_app_surface_preview(
            manifest,
            installation_id.clone(),
            revision,
            revision,
            view_id,
            compiled_at,
        )?;
        previews.push(AppViewPreview {
            view_id: view_id.to_string(),
            route: view.route.as_str().to_owned(),
            fixture_records: fixtures
                .records
                .iter()
                .filter(|fixture| fixture.entity == view.entity)
                .map(|fixture| fixture.value.clone())
                .collect(),
            surface: serde_json::to_value(surface)?,
        });
    }
    Ok(AppPreviewReport {
        status: "preview",
        package_name: manifest.manifest().name.to_string(),
        authoritative: false,
        activation_capable: false,
        previews,
    })
}

fn pack_project(args: &AppPackArgs) -> Result<AppPackReport> {
    let package_root = canonical_package_root(&args.path)?;
    let candidate = admit_package_directory(&package_root)
        .with_context(|| format!("admitting app package {}", package_root.display()))?;
    ensure_contract_compatibility(&candidate)?;
    ensure_generated_current(&package_root, &candidate)?;
    let _tests = test_candidate(&candidate)?;
    let evidence =
        dependency_resolution_evidence(&package_root, args.resolutions.as_deref(), &candidate)?;
    let primitive_snapshot = resolve_authoring_primitive_catalog(&discovery_roots(&args.discover));
    let tool_catalog = AppReviewedToolCatalog::from_primitive_snapshot(&primitive_snapshot)
        .context("resolving the scoped immutable primitive catalog")?;
    let evidence = complete_declared_tool_evidence(&candidate, evidence, &tool_catalog)
        .context("snapshotting scoped or externally resolved tools into the authoring lock")?;
    let lock = lock_app_package_dependencies(&candidate, evidence, &AppPackageLimits::default())
        .context("locking exact app dependencies")?;
    let publisher = AppReference::parse(args.publisher.clone())
        .context("parsing package publisher identity")?;
    let manifest = build_authoring_archive_manifest(&candidate, &lock, publisher)?;
    let archive = encode_package_archive(&manifest, &candidate)
        .context("encoding package-only app archive")?;
    let output = resolve_pack_output(&package_root, args.output.as_deref(), &candidate)?;
    publish_new_file(&output, &archive)?;
    let package = candidate.manifest().manifest();
    Ok(AppPackReport {
        status: "packed",
        package_name: package.name.to_string(),
        semantic_version: package.version.clone(),
        output,
        archive_bytes: archive.len(),
        manifest_digest: candidate.manifest().manifest_digest().clone(),
        bundle_digest: candidate.bundle_digest().clone(),
        dependency_lock_digest: lock.lock_digest().clone(),
        package_payload_digest: manifest.logical_payload_digest,
        server_revalidation_required: true,
        authority_transferred: false,
    })
}

fn canonical_package_root(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading app package path {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppAuthoringError::UnsafePath(format!(
            "'{}' must be a real directory, not a symlink",
            path.display()
        ))
        .into());
    }
    fs::canonicalize(path).with_context(|| format!("canonicalizing {}", path.display()))
}

fn ensure_contract_compatibility(candidate: &AppPackageCandidate) -> Result<()> {
    let manifest = candidate.manifest().manifest();
    let name = AppName::parse("magician_contract").expect("built-in contract name is valid");
    let Some(requirement) = manifest.app.compatibility.get(&name) else {
        return Err(AppAuthoringError::MissingContractCompatibility.into());
    };
    let requirement = semver::VersionReq::parse(requirement)?;
    let version = semver::Version::parse(APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION)?;
    if !requirement.matches(&version) {
        return Err(AppAuthoringError::InvalidResolution(format!(
            "package requires magician_contract `{requirement}`, but this platform contract \
             supports `{version}`"
        ))
        .into());
    }
    Ok(())
}

struct GeneratedArtifact {
    path: &'static str,
    bytes: Vec<u8>,
}

fn generated_artifacts(candidate: &AppPackageCandidate) -> Result<Vec<GeneratedArtifact>> {
    Ok(vec![
        GeneratedArtifact {
            path: DERIVED_JSON_PATH,
            bytes: render_derived_json(candidate)?,
        },
        GeneratedArtifact {
            path: GENERATED_TYPESCRIPT_PATH,
            bytes: render_generated_typescript(candidate)?,
        },
    ])
}

fn ensure_generated_current(root: &Path, candidate: &AppPackageCandidate) -> Result<()> {
    for artifact in generated_artifacts(candidate)? {
        let path = AppBundlePath::parse(artifact.path)?;
        let observed = candidate
            .member(&path)
            .map(|member| member.bytes())
            .unwrap_or_default();
        if observed != artifact.bytes {
            return Err(
                AppAuthoringError::GeneratedDrift(artifact.path, root.to_path_buf()).into(),
            );
        }
    }
    Ok(())
}

fn render_derived_json(candidate: &AppPackageCandidate) -> Result<Vec<u8>> {
    let manifest = candidate.manifest().manifest();
    let generated_typescript = render_generated_typescript(candidate)?;
    let generated_typescript_digest = AppDigest::blake3(&generated_typescript);
    let workflow_schema_identities = generated_workflow_schema_identities(candidate)?;
    let mut core = json!({
        "schema_version": AUTHORING_DERIVATION_VERSION,
        "typescript_generation_version": AUTHORING_TYPESCRIPT_GENERATION_VERSION,
        "contract_version": APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
        "manifest_version": APP_MANIFEST_SCHEMA_VERSION,
        "required_features": manifest.metadata.magician.required_features,
        "generated_by": manifest.metadata.magician.generated_by,
        "package_name": manifest.name,
        "semantic_version": manifest.version,
        "manifest_digest": candidate.manifest().manifest_digest(),
        "data_policy": manifest.app.data_policy,
        "entities": manifest.app.entities,
        "views": manifest.app.views,
        "workflows": manifest.app.workflows,
        "actions": manifest.app.actions,
        "resources": manifest.app.resources,
        "workflow_schema_identities": workflow_schema_identities,
        "generated_typescript_digest": generated_typescript_digest,
        "dependency_requirements": manifest.dependency_requirements(&AppPackageLimits::default())?,
    });
    // Only a package that requests owner memory carries the key, so every
    // existing package's derivation digest (and bundle digest) is unchanged.
    if let Some(memory) = &manifest.app.memory {
        core.as_object_mut()
            .context("derived app contract must be an object")?
            .insert("memory".to_owned(), serde_json::to_value(memory)?);
    }
    let digest = AppDigest::blake3(&canonical_json_bytes(&core)?);
    let mut object = core
        .as_object()
        .cloned()
        .context("derived app contract must be an object")?;
    object.insert(
        "derivation_digest".to_owned(),
        serde_json::to_value(digest)?,
    );
    let mut bytes = canonical_json_bytes(&Value::Object(object))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn compiled_recipe_for_workflow(
    candidate: &AppPackageCandidate,
    workflow_id: &AppName,
    workflow: &AppManifestWorkflow,
) -> Result<Option<AppCompiledRecipeBundle>> {
    if workflow.runner != AppManifestRunner::Recipe {
        return Ok(None);
    }
    let path = workflow.recipe.as_ref().with_context(|| {
        format!("recipe workflow `{workflow_id}` has no immutable recipe member")
    })?;
    let member = candidate
        .member(path)
        .with_context(|| format!("recipe workflow `{workflow_id}` member `{path}` is absent"))?;
    let source: AppRecipeBundleSource = serde_json::from_slice(member.bytes())
        .with_context(|| format!("decoding recipe workflow `{workflow_id}`"))?;
    Ok(Some(compile_recipe_bundle(source).with_context(|| {
        format!("compiling recipe workflow `{workflow_id}`")
    })?))
}

fn workflow_result_schema(
    candidate: &AppPackageCandidate,
    workflow_id: &AppName,
    workflow: &AppManifestWorkflow,
) -> Result<Option<AppCompiledWorkflowValueSchema>> {
    if let Some(schema) = workflow.result.output_schema.as_ref() {
        return Ok(Some(schema.compiled_value_schema()?));
    }
    let Some(recipe) = compiled_recipe_for_workflow(candidate, workflow_id, workflow)? else {
        return Ok(None);
    };
    let schema_ref = &recipe.recipe().source().output.schema_ref;
    Ok(Some(
        recipe
            .schema(schema_ref)
            .with_context(|| {
                format!("recipe workflow `{workflow_id}` output schema `{schema_ref}` is absent")
            })?
            .clone(),
    ))
}

fn generated_workflow_schema_identities(candidate: &AppPackageCandidate) -> Result<Value> {
    let mut workflows = serde_json::Map::new();
    for (workflow_id, workflow) in &candidate.manifest().manifest().app.workflows {
        let input = workflow.input.compiled_value_schema()?;
        let result = workflow_result_schema(candidate, workflow_id, workflow)?;
        workflows.insert(
            workflow_id.to_string(),
            json!({
                "input": {
                    "schema_ref": input.schema_ref(),
                    "content_digest": input.content_digest(),
                },
                "result": result.as_ref().map(|schema| json!({
                    "schema_ref": schema.schema_ref(),
                    "content_digest": schema.content_digest(),
                })),
                "result_kind": workflow.result.kind,
            }),
        );
    }
    Ok(Value::Object(workflows))
}

fn render_generated_typescript(candidate: &AppPackageCandidate) -> Result<Vec<u8>> {
    let manifest = candidate.manifest().manifest();
    let required_features = serde_json::to_string(&manifest.metadata.magician.required_features)
        .context("serializing required app-manifest features")?;
    let generated_by = serde_json::to_string(&manifest.metadata.magician.generated_by)
        .context("serializing app-manifest generator identity")?;
    let mut output = String::from(
        "// Generated by `magician app check --write-generated`; do not edit.\n\
         import { defineAppValueCodec, recipeNode } from \"@magician/apps\";\n\
         import type {\n\
           AppActionCancellationReceipt, AppActionLaunchResponse, AppArtifactHandle as SdkArtifactHandle,\n\
           AppCustomBridgeRunControlRequest, AppEntityHandle as SdkEntityHandle,\n\
           AppObservationHandle as SdkObservationHandle, AppOpaqueReference as SdkOpaqueReference,\n\
           AppReceiptHandle as SdkReceiptHandle, AppResourceHandle as SdkResourceHandle,\n\
           AppRunReference as SdkRunHandle, AppRunSnapshot, AppSessionHandle as SdkSessionHandle,\n\
           AppGeneratedValueSchema, AppValueCodec, JsonValue\n\
         } from \"@magician/apps\";\n\n",
    );
    output.push_str(&format!(
        "export const APP_CONTRACT_VERSION = {:?} as const;\nexport const APP_MANIFEST_VERSION = \
         {:?} as const;\nexport const APP_TYPESCRIPT_GENERATION_VERSION = {} as const;\nexport \
         const APP_REQUIRED_FEATURES = {} as const;\nexport const APP_GENERATED_BY = {} as \
         const;\n\n",
        APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
        APP_MANIFEST_SCHEMA_VERSION,
        AUTHORING_TYPESCRIPT_GENERATION_VERSION,
        required_features,
        generated_by,
    ));
    output.push_str(&format!(
        "export type AppEntityName = {};\nexport type AppViewName = {};\nexport type \
         AppWorkflowName = {};\nexport type AppActionName = {};\n\n",
        typescript_name_union(manifest.app.entities.keys()),
        typescript_name_union(manifest.app.views.keys()),
        typescript_name_union(manifest.app.workflows.keys()),
        typescript_name_union(manifest.app.actions.keys()),
    ));
    output.push_str(
        "export type AppRunHandle = SdkRunHandle;\n\
         export type AppSessionHandle = SdkSessionHandle;\n\
         export type AppObservationHandle = SdkObservationHandle;\n\
         export type AppArtifactHandle = SdkArtifactHandle;\n\
         export type AppReceiptHandle<Kind extends string = string> = SdkReceiptHandle<Kind>;\n\
         export type AppResourceHandle<Kind extends string = string> = SdkResourceHandle<Kind>;\n\
         export type AppEntityHandle<Entity extends string = string> = SdkEntityHandle<Entity>;\n\n",
    );
    for (name, entity) in &manifest.app.entities {
        output.push_str(&format!(
            "export interface {} {{\n",
            generated_type_name("Entity", name)
        ));
        for (field_name, field) in &entity.fields {
            let (required, nullable, field_type) = typescript_field_type(field);
            output.push_str(&format!(
                "  {}{}: {}{};\n",
                serde_json::to_string(field_name.as_str())?,
                if required { "" } else { "?" },
                field_type,
                if nullable { " | null" } else { "" }
            ));
        }
        output.push_str("}\n\n");
    }
    let mut workflow_types = BTreeMap::<AppName, (String, String)>::new();
    let mut form_metadata = serde_json::Map::new();
    let mut legacy_result_gaps = Vec::new();
    for (name, workflow) in &manifest.app.workflows {
        let input = workflow.input.compiled_value_schema()?;
        let input_name = generated_type_name("WorkflowInput", name);
        let input_schema_name = generated_type_name("WorkflowInputSchema", name);
        output.push_str(&format!(
            "export type {input_name} = {};\nexport const {input_schema_name} = {} as const \
             satisfies AppGeneratedValueSchema;\nexport const {input_name}Codec: \
             AppValueCodec<{input_name}> = defineAppValueCodec<{input_name}>({:?}, {:?}, \
             {input_schema_name}, \"input\");\n\n",
            typescript_schema_type(&input)?,
            serde_json::to_string(input.source())?,
            input.schema_ref().as_str(),
            input.content_digest().as_str(),
        ));

        let result_name = generated_type_name("WorkflowResult", name);
        if let Some(result) = workflow_result_schema(candidate, name, workflow)? {
            let result_schema_name = generated_type_name("WorkflowResultSchema", name);
            output.push_str(&format!(
                "export type {result_name} = {};\nexport const {result_schema_name} = {} as const \
                 satisfies AppGeneratedValueSchema;\nexport const {result_name}Codec: \
                 AppValueCodec<{result_name}> = defineAppValueCodec<{result_name}>({:?}, {:?}, \
                 {result_schema_name}, \"result\");\n\n",
                typescript_schema_type(&result)?,
                serde_json::to_string(result.source())?,
                result.schema_ref().as_str(),
                result.content_digest().as_str(),
            ));
        } else {
            legacy_result_gaps.push(name.as_str().to_owned());
            output.push_str(&format!(
                "/** Legacy result without a reviewed value schema; migrate the manifest before composition. */\n\
                 export type {result_name} = JsonValue;\n\n"
            ));
        }
        form_metadata.insert(name.to_string(), workflow_form_metadata(&input));
        workflow_types.insert(name.clone(), (input_name, result_name));
    }
    output.push_str(&format!(
        "export const APP_WORKFLOW_FORMS = {} as const;\nexport const \
         APP_LEGACY_UNTYPED_RESULT_WORKFLOWS = {} as const;\n\n",
        serde_json::to_string(&form_metadata)?,
        serde_json::to_string(&legacy_result_gaps)?,
    ));

    output.push_str("export interface AppActionContracts {\n");
    for (action_name, action) in &manifest.app.actions {
        let (input, result) = workflow_types
            .get(&action.workflow)
            .with_context(|| format!("action `{action_name}` references no generated workflow"))?;
        output.push_str(&format!(
            "  readonly {:?}: {{ readonly workflow: {:?}; readonly input: {input}; readonly result: \
             {result} }};\n",
            action_name.as_str(),
            action.workflow.as_str(),
        ));
    }
    output.push_str(
        "}\n\
         export type AppActionInput<Action extends AppActionName> = AppActionContracts[Action][\"input\"];\n\
         export type AppActionResultValue<Action extends AppActionName> = AppActionContracts[Action][\"result\"];\n\
         export type AppActionLaunch<Action extends AppActionName> = AppActionLaunchResponse<AppActionResultValue<Action>>;\n\n\
         export interface AppCustomSurfaceBridge {\n\
           invoke<Action extends AppActionName>(action: Action, input: AppActionInput<Action>): Promise<AppActionLaunch<Action>>;\n\
           getRun<Action extends AppActionName>(action: Action, run: AppRunHandle): Promise<AppRunSnapshot<AppActionResultValue<Action>>>;\n\
           waitRun<Action extends AppActionName>(action: Action, run: AppRunHandle, maxPolls?: number, pollIntervalMs?: number): Promise<AppRunSnapshot<AppActionResultValue<Action>>>;\n\
           cancelRun<Action extends AppActionName>(action: Action, run: AppRunHandle, expectedGeneration: number, idempotencyKey: string): Promise<AppActionCancellationReceipt>;\n\
         }\n\
         export type AppCustomSurfaceRunControl = AppCustomBridgeRunControlRequest;\n\n\
         /** Exact supported Recipe builder set; no generic/effectful escape hatch is generated. */\n\
         export const AppRecipe = Object.freeze({\n\
           query: recipeNode.query, get: recipeNode.get, map: recipeNode.map,\n\
           validate: recipeNode.validate, emitValue: recipeNode.emitValue,\n\
           sequence: recipeNode.sequence, parallel: recipeNode.parallel, switch: recipeNode.switch, reconcile: recipeNode.reconcile,\n\
         });\n",
    );
    Ok(output.into_bytes())
}

fn typescript_schema_type(schema: &AppCompiledWorkflowValueSchema) -> Result<String> {
    fn render(schema: &AppCompiledWorkflowValueSchema, index: u16, depth: usize) -> Result<String> {
        if depth > 32 {
            anyhow::bail!("workflow schema exceeds TypeScript generation depth");
        }
        let node = schema
            .source()
            .nodes
            .get(usize::from(index))
            .context("workflow schema node is absent during TypeScript generation")?;
        Ok(match node {
            AppWorkflowValueTypeNode::Unit => "null".to_owned(),
            AppWorkflowValueTypeNode::Boolean => "boolean".to_owned(),
            AppWorkflowValueTypeNode::Integer | AppWorkflowValueTypeNode::Decimal => {
                "number".to_owned()
            },
            AppWorkflowValueTypeNode::Text { .. }
            | AppWorkflowValueTypeNode::Markdown { .. }
            | AppWorkflowValueTypeNode::Timestamp
            | AppWorkflowValueTypeNode::EntityReference { .. } => "string".to_owned(),
            AppWorkflowValueTypeNode::Enum { values } => values
                .iter()
                .map(|value| serde_json::to_string(value.as_str()))
                .collect::<std::result::Result<Vec<_>, _>>()?
                .join(" | "),
            AppWorkflowValueTypeNode::OpaqueReference => "SdkOpaqueReference".to_owned(),
            AppWorkflowValueTypeNode::EntityProjectionRef { entity, .. } => {
                format!("AppEntityHandle<{:?}>", entity.as_str())
            },
            AppWorkflowValueTypeNode::ArtifactRef { .. } => "AppArtifactHandle".to_owned(),
            AppWorkflowValueTypeNode::ReceiptRef { receipt_kind } => format!(
                "AppReceiptHandle<{:?}>",
                match receipt_kind {
                    super::recipe_ir::AppWorkflowReceiptKind::Mutation => "mutation",
                    super::recipe_ir::AppWorkflowReceiptKind::ExternalEffect => "external_effect",
                }
            ),
            AppWorkflowValueTypeNode::ResourceRef { resource_kind } => {
                format!("AppResourceHandle<{:?}>", resource_kind.as_str())
            },
            AppWorkflowValueTypeNode::Nullable { value_type } => {
                format!("({}) | null", render(schema, *value_type, depth + 1)?)
            },
            AppWorkflowValueTypeNode::Array { items, .. } => {
                format!("ReadonlyArray<{}>", render(schema, *items, depth + 1)?)
            },
            AppWorkflowValueTypeNode::Record { fields } => {
                let mut value = String::from("Readonly<{ ");
                for (name, field) in fields {
                    value.push_str(&format!(
                        "readonly {}{}: {}; ",
                        serde_json::to_string(name.as_str())?,
                        if field.required { "" } else { "?" },
                        render(schema, field.value_type, depth + 1)?,
                    ));
                }
                value.push_str("}>");
                value
            },
            AppWorkflowValueTypeNode::TaggedUnion {
                discriminator,
                variants,
            } => variants
                .iter()
                .map(|(tag, value_type)| {
                    Ok(format!(
                        "Readonly<{{ readonly {}: {:?}; readonly value: {} }}>",
                        serde_json::to_string(discriminator.as_str())?,
                        tag.as_str(),
                        render(schema, *value_type, depth + 1)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?
                .join(" | "),
        })
    }
    render(schema, schema.source().root, 0)
}

fn workflow_form_metadata(schema: &AppCompiledWorkflowValueSchema) -> Value {
    let AppWorkflowValueTypeNode::Record { fields } = schema.root_node() else {
        return json!({
            "schema_ref": schema.schema_ref(),
            "schema_digest": schema.content_digest(),
            "fields": [],
        });
    };
    let fields = fields
        .iter()
        .map(|(name, field)| {
            let mut node = schema.source().nodes.get(usize::from(field.value_type));
            let nullable = matches!(node, Some(AppWorkflowValueTypeNode::Nullable { .. }));
            if let Some(AppWorkflowValueTypeNode::Nullable { value_type }) = node {
                node = schema.source().nodes.get(usize::from(*value_type));
            }
            let (control, values) = match node {
                Some(AppWorkflowValueTypeNode::Boolean) => ("checkbox", None),
                Some(AppWorkflowValueTypeNode::Integer | AppWorkflowValueTypeNode::Decimal) => {
                    ("number", None)
                },
                Some(AppWorkflowValueTypeNode::Timestamp) => ("datetime", None),
                Some(AppWorkflowValueTypeNode::Markdown { .. }) => ("markdown", None),
                Some(AppWorkflowValueTypeNode::Enum { values }) => (
                    "select",
                    Some(values.iter().map(ToString::to_string).collect::<Vec<_>>()),
                ),
                Some(AppWorkflowValueTypeNode::Array { .. }) => ("array", None),
                Some(AppWorkflowValueTypeNode::Record { .. }) => ("fieldset", None),
                Some(AppWorkflowValueTypeNode::TaggedUnion { .. }) => ("tagged_union", None),
                Some(
                    AppWorkflowValueTypeNode::EntityProjectionRef { .. }
                    | AppWorkflowValueTypeNode::ArtifactRef { .. }
                    | AppWorkflowValueTypeNode::ReceiptRef { .. }
                    | AppWorkflowValueTypeNode::ResourceRef { .. },
                ) => ("readonly_handle", None),
                _ => ("text", None),
            };
            json!({
                "name": name,
                "required": field.required,
                "nullable": nullable,
                "control": control,
                "values": values,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_ref": schema.schema_ref(),
        "schema_digest": schema.content_digest(),
        "fields": fields,
    })
}

fn typescript_name_union<'a>(names: impl Iterator<Item = &'a AppName>) -> String {
    let values = names
        .map(|name| serde_json::to_string(name.as_str()).expect("app names serialize"))
        .collect::<Vec<_>>();
    if values.is_empty() {
        "never".to_owned()
    } else {
        values.join(" | ")
    }
}

fn generated_type_name(prefix: &str, name: &AppName) -> String {
    let mut pascal = String::new();
    let mut uppercase = true;
    for character in name.as_str().chars() {
        if character.is_ascii_alphanumeric() {
            if uppercase {
                pascal.push(character.to_ascii_uppercase());
                uppercase = false;
            } else {
                pascal.push(character);
            }
        } else {
            uppercase = true;
        }
    }
    if pascal.is_empty() || pascal.as_bytes()[0].is_ascii_digit() {
        pascal.insert(0, 'N');
    }
    let digest = blake3::hash(name.as_str().as_bytes()).to_hex();
    format!("App{prefix}{pascal}_{}", &digest.as_str()[..8])
}

fn typescript_field_type(field: &AppManifestField) -> (bool, bool, String) {
    match field {
        AppManifestField::Text {
            required, nullable, ..
        }
        | AppManifestField::Markdown {
            required, nullable, ..
        }
        | AppManifestField::Timestamp {
            required, nullable, ..
        }
        | AppManifestField::Reference {
            required, nullable, ..
        } => (*required, *nullable, "string".to_owned()),
        AppManifestField::Integer {
            required, nullable, ..
        }
        | AppManifestField::Decimal {
            required, nullable, ..
        } => (*required, *nullable, "number".to_owned()),
        AppManifestField::Boolean {
            required, nullable, ..
        } => (*required, *nullable, "boolean".to_owned()),
        AppManifestField::Enum {
            values,
            required,
            nullable,
            ..
        } => (
            *required,
            *nullable,
            values
                .iter()
                .map(|value| serde_json::to_string(value.as_str()).expect("enum name serializes"))
                .collect::<Vec<_>>()
                .join(" | "),
        ),
    }
}

fn authoring_policy_diff(candidate: &AppPackageCandidate) -> Result<Value> {
    let manifest = candidate.manifest().manifest();
    let dependencies = manifest.dependency_requirements(&AppPackageLimits::default())?;
    Ok(json!({
        "data_policy": manifest.app.data_policy.defaults,
        "resource_ceilings": manifest.app.resources,
        "dependency_requirements": dependencies,
        "workflow_capabilities": manifest.app.workflows.values()
            .flat_map(|workflow| workflow.uses.iter())
            .collect::<BTreeSet<_>>(),
        "workflow_procedures": manifest.app.workflows.values()
            .flat_map(|workflow| workflow.procedures.iter())
            .collect::<BTreeSet<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppFixtureSuite {
    schema_version: u8,
    #[serde(default)]
    records: Vec<AppEntityFixture>,
    #[serde(default)]
    workflows: Vec<AppWorkflowFixture>,
    #[serde(default)]
    views: Vec<AppViewFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppEntityFixture {
    name: String,
    entity: AppName,
    value: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppWorkflowFixture {
    name: String,
    workflow: AppName,
    input: Value,
    expected_entities: Vec<AppName>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppViewFixture {
    name: String,
    view: AppName,
    minimum_records: usize,
}

fn validate_fixture_suite(
    candidate: &AppPackageCandidate,
    fixtures: &AppFixtureSuite,
) -> Result<()> {
    if fixtures.schema_version != AUTHORING_FIXTURE_VERSION {
        return Err(AppAuthoringError::InvalidFixture(format!(
            "unsupported schema_version {}",
            fixtures.schema_version
        ))
        .into());
    }
    let count = fixtures
        .records
        .len()
        .saturating_add(fixtures.workflows.len())
        .saturating_add(fixtures.views.len());
    if count == 0 || count > MAX_FIXTURE_ITEMS {
        return Err(AppAuthoringError::InvalidFixture(format!(
            "fixture count must be between 1 and {MAX_FIXTURE_ITEMS}"
        ))
        .into());
    }
    let manifest = candidate.manifest().manifest();
    let mut names = BTreeSet::new();
    let mut records_by_entity = BTreeMap::<&AppName, usize>::new();
    for fixture in &fixtures.records {
        validate_fixture_name(&fixture.name, &mut names)?;
        let entity = manifest.app.entities.get(&fixture.entity).ok_or_else(|| {
            AppAuthoringError::InvalidFixture(format!(
                "record fixture `{}` names unknown entity `{}`",
                fixture.name, fixture.entity
            ))
        })?;
        validate_fixture_object(
            &fixture.value,
            &entity.fields,
            &format!("record fixture `{}`", fixture.name),
        )?;
        *records_by_entity.entry(&fixture.entity).or_default() += 1;
    }
    for fixture in &fixtures.workflows {
        validate_fixture_name(&fixture.name, &mut names)?;
        let workflow = manifest
            .app
            .workflows
            .get(&fixture.workflow)
            .ok_or_else(|| {
                AppAuthoringError::InvalidFixture(format!(
                    "workflow fixture `{}` names unknown workflow `{}`",
                    fixture.name, fixture.workflow
                ))
            })?;
        workflow
            .input
            .validate_value(&fixture.input)
            .map_err(|error| {
                AppAuthoringError::InvalidFixture(format!(
                    "workflow fixture `{}` input does not match its canonical schema: {error}",
                    fixture.name
                ))
            })?;
        if fixture.expected_entities != workflow.result.entities {
            return Err(AppAuthoringError::InvalidFixture(format!(
                "workflow fixture `{}` expected_entities differ from the derived result contract",
                fixture.name
            ))
            .into());
        }
    }
    for fixture in &fixtures.views {
        validate_fixture_name(&fixture.name, &mut names)?;
        let view = manifest.app.views.get(&fixture.view).ok_or_else(|| {
            AppAuthoringError::InvalidFixture(format!(
                "view fixture `{}` names unknown view `{}`",
                fixture.name, fixture.view
            ))
        })?;
        let available = records_by_entity.get(&view.entity).copied().unwrap_or(0);
        if available < fixture.minimum_records {
            return Err(AppAuthoringError::InvalidFixture(format!(
                "view fixture `{}` requires {} records for `{}`, but only {available} exist",
                fixture.name, fixture.minimum_records, view.entity
            ))
            .into());
        }
    }
    Ok(())
}

fn validate_fixture_name(name: &str, names: &mut BTreeSet<String>) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AppAuthoringError::InvalidFixture(format!(
            "fixture name `{name}` is not a bounded ASCII identifier"
        ))
        .into());
    }
    if !names.insert(name.to_ascii_lowercase()) {
        return Err(
            AppAuthoringError::InvalidFixture(format!("duplicate fixture name `{name}`")).into(),
        );
    }
    Ok(())
}

fn validate_fixture_object(
    value: &Value,
    fields: &BTreeMap<AppName, AppManifestField>,
    label: &str,
) -> Result<()> {
    let object = value.as_object().ok_or_else(|| {
        AppAuthoringError::InvalidFixture(format!("{label} must be a JSON object"))
    })?;
    for key in object.keys() {
        let name = AppName::parse(key.clone()).map_err(|_| {
            AppAuthoringError::InvalidFixture(format!("{label} has invalid field `{key}`"))
        })?;
        if !fields.contains_key(&name) {
            return Err(AppAuthoringError::InvalidFixture(format!(
                "{label} has unknown field `{key}`"
            ))
            .into());
        }
    }
    for (name, field) in fields {
        let (required, nullable) = field_presence(field);
        let Some(observed) = object.get(name.as_str()) else {
            if required {
                return Err(AppAuthoringError::InvalidFixture(format!(
                    "{label} is missing required field `{name}`"
                ))
                .into());
            }
            continue;
        };
        if observed.is_null() {
            if nullable {
                continue;
            }
            return Err(AppAuthoringError::InvalidFixture(format!(
                "{label} field `{name}` is not nullable"
            ))
            .into());
        }
        let valid = match field {
            AppManifestField::Text { .. } | AppManifestField::Markdown { .. } => {
                observed.as_str().is_some_and(|text| text.len() <= 4_096)
            },
            AppManifestField::Integer { .. } => {
                observed.as_i64().is_some() || observed.as_u64().is_some()
            },
            AppManifestField::Decimal { .. } => observed.as_f64().is_some(),
            AppManifestField::Boolean { .. } => observed.is_boolean(),
            AppManifestField::Timestamp { .. } => observed.as_str().is_some_and(|text| {
                text.len() <= 128 && chrono::DateTime::parse_from_rfc3339(text).is_ok()
            }),
            AppManifestField::Enum { values, .. } => observed
                .as_str()
                .is_some_and(|text| values.iter().any(|value| value.as_str() == text)),
            AppManifestField::Reference { .. } => observed
                .as_str()
                .is_some_and(|text| AppReference::parse(text.to_owned()).is_ok()),
        };
        if !valid {
            return Err(AppAuthoringError::InvalidFixture(format!(
                "{label} field `{name}` does not match its derived type"
            ))
            .into());
        }
    }
    Ok(())
}

fn field_presence(field: &AppManifestField) -> (bool, bool) {
    match field {
        AppManifestField::Text {
            required, nullable, ..
        }
        | AppManifestField::Markdown {
            required, nullable, ..
        }
        | AppManifestField::Integer {
            required, nullable, ..
        }
        | AppManifestField::Decimal {
            required, nullable, ..
        }
        | AppManifestField::Boolean {
            required, nullable, ..
        }
        | AppManifestField::Timestamp {
            required, nullable, ..
        }
        | AppManifestField::Enum {
            required, nullable, ..
        }
        | AppManifestField::Reference {
            required, nullable, ..
        } => (*required, *nullable),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppResolutionFile {
    schema_version: u8,
    dependencies: Vec<AppResolutionEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppResolutionEntry {
    kind: super::manifest::AppDependencyKind,
    dependency_ref: AppReference,
    semantic_version: String,
    immutable_revision_ref: AppReference,
    revision: u64,
    content_path: String,
}

fn dependency_resolution_evidence(
    package_root: &Path,
    resolution_path: Option<&Path>,
    candidate: &AppPackageCandidate,
) -> Result<Vec<AppVerifiedRegistryDependency>> {
    ensure_contract_compatibility(candidate)?;
    let mut evidence = vec![AppVerifiedRegistryDependency::from_authoring_fixture_bytes(
        super::manifest::AppDependencyKind::Contract,
        AppReference::parse("contract:magician_contract")?,
        APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION.to_owned(),
        AppReference::parse("contract-revision:magician-contract-v2")?,
        AppRevision::new(BUILTIN_CONTRACT_REVISION)?,
        current_builtin_contract_bytes()?,
    )?];
    let Some(resolution_path) = resolution_path else {
        return Ok(evidence);
    };
    let resolution_path = canonical_external_file(package_root, resolution_path, "resolution")?;
    let bytes = read_bounded_file(&resolution_path, MAX_FIXTURE_BYTES)?;
    if !json_bytes_nesting_is_bounded(&bytes, MAX_FIXTURE_DEPTH)
        || !json_bytes_nodes_are_bounded(&bytes, MAX_FIXTURE_NODES)
    {
        return Err(AppAuthoringError::InvalidResolution(
            "JSON depth/node limit exceeded".to_owned(),
        )
        .into());
    }
    let resolution: AppResolutionFile = serde_json::from_slice(&bytes)
        .map_err(|error| AppAuthoringError::InvalidResolution(error.to_string()))?;
    if resolution.schema_version != AUTHORING_RESOLUTION_VERSION
        || resolution.dependencies.len() > AppPackageLimits::default().max_dependencies()
    {
        return Err(AppAuthoringError::InvalidResolution(
            "unsupported schema version or dependency count".to_owned(),
        )
        .into());
    }
    let base = resolution_path
        .parent()
        .context("resolution file has no parent")?;
    let mut total_bytes = 0usize;
    for entry in resolution.dependencies {
        let relative_content_path = safe_relative_resolution_path(&entry.content_path)?;
        if entry.dependency_ref.as_str() == "contract:magician_contract" {
            return Err(AppAuthoringError::InvalidResolution(
                "the built-in magician_contract resolution cannot be overridden".to_owned(),
            )
            .into());
        }
        let content_path = canonical_external_file(
            package_root,
            &base.join(relative_content_path),
            "dependency content",
        )?;
        if !content_path.starts_with(base) {
            return Err(AppAuthoringError::InvalidResolution(
                "dependency content must remain beneath the resolution directory".to_owned(),
            )
            .into());
        }
        let content = read_bounded_file(
            &content_path,
            AppPackageLimits::default().max_bundle_file_bytes(),
        )?;
        total_bytes = total_bytes.saturating_add(content.len());
        if total_bytes > AppPackageLimits::default().max_bundle_bytes() {
            return Err(AppAuthoringError::InvalidResolution(
                "dependency fixture bytes exceed the aggregate ceiling".to_owned(),
            )
            .into());
        }
        evidence.push(AppVerifiedRegistryDependency::from_authoring_fixture_bytes(
            entry.kind,
            entry.dependency_ref,
            entry.semantic_version,
            entry.immutable_revision_ref,
            AppRevision::new(entry.revision)?,
            &content,
        )?);
    }
    Ok(evidence)
}

fn current_builtin_contract_bytes() -> Result<&'static [u8]> {
    validate_app_data_plane_component_contract(BUILTIN_CONTRACT_BYTES).map_err(|error| {
        AppAuthoringError::InvalidResolution(format!(
            "embedded app data-plane component contract is invalid: {error}"
        ))
    })?;
    Ok(BUILTIN_CONTRACT_BYTES)
}

fn safe_relative_resolution_path(raw: &str) -> Result<PathBuf> {
    if raw.is_empty() || raw.len() > 512 || !raw.is_ascii() {
        return Err(AppAuthoringError::InvalidResolution(
            "dependency content path must be a non-empty ASCII path of at most 512 bytes"
                .to_owned(),
        )
        .into());
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppAuthoringError::InvalidResolution(
            "dependency content path must be a normal relative path without traversal".to_owned(),
        )
        .into());
    }
    Ok(path.to_path_buf())
}

fn canonical_external_file(package_root: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading {label} path {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppAuthoringError::UnsafePath(format!(
            "{label} '{}' must be a real regular file",
            path.display()
        ))
        .into());
    }
    let canonical = fs::canonicalize(path)?;
    if canonical.starts_with(package_root) {
        return Err(AppAuthoringError::UnsafePath(format!(
            "{label} '{}' must remain outside the transferable package",
            path.display()
        ))
        .into());
    }
    Ok(canonical)
}

fn build_authoring_archive_manifest(
    candidate: &AppPackageCandidate,
    lock: &AppPackageLock,
    publisher: AppReference,
) -> Result<AppPackageArchiveManifest> {
    let package = candidate.manifest().manifest();
    let package_id = AppReference::parse(format!("app:{}", package.name))?;
    let package_revision_ref = canonical_package_revision_ref_from_identity(
        &package_id,
        &package.version,
        candidate.bundle_digest(),
        lock.lock_digest(),
    )?;
    let members = candidate
        .members()
        .iter()
        .map(|member| AppPortablePackageMember {
            path: member.path().clone(),
            content_digest: member.content_digest().clone(),
            byte_len: u64::try_from(member.bytes().len()).unwrap_or(u64::MAX),
        })
        .collect();
    let mut manifest = AppPackageArchiveManifest {
        archive_version: APP_PORTABLE_ARCHIVE_VERSION,
        package_revision_ref,
        package_id,
        publisher_identity: publisher,
        semantic_version: package.version.clone(),
        package_content_digest: candidate.bundle_digest().clone(),
        manifest_digest: candidate.manifest().manifest_digest().clone(),
        dependency_lock: AppPortablePackageLockClaim::from_trusted(lock),
        dependency_lock_digest: lock.lock_digest().clone(),
        members,
        advisory_verification_evidence: Vec::new(),
        logical_payload_digest: AppDigest::blake3(b"pending"),
    };
    manifest.logical_payload_digest = manifest.recompute_digest()?;
    Ok(manifest)
}

fn resolve_pack_output(
    package_root: &Path,
    requested: Option<&Path>,
    candidate: &AppPackageCandidate,
) -> Result<PathBuf> {
    let manifest = candidate.manifest().manifest();
    let requested = requested.map(Path::to_path_buf).unwrap_or_else(|| {
        package_root
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}-{}.app.zip", manifest.name, manifest.version))
    });
    let parent = requested.parent().unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent)
        .with_context(|| format!("canonicalizing output parent {}", parent.display()))?;
    let file_name = requested.file_name().ok_or_else(|| {
        AppAuthoringError::UnsafePath("package output has no file name".to_owned())
    })?;
    let output = parent.join(file_name);
    if output.starts_with(package_root) {
        return Err(AppAuthoringError::UnsafePath(
            "package output must remain outside the package directory".to_owned(),
        )
        .into());
    }
    if output.exists() {
        return Err(AppAuthoringError::OutputExists(output).into());
    }
    Ok(output)
}

pub fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let named_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    if named_metadata.file_type().is_symlink() || !named_metadata.is_file() {
        return Err(AppAuthoringError::UnsafePath(format!(
            "'{}' must be a real regular file",
            path.display()
        ))
        .into());
    }
    if named_metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(AppAuthoringError::UnsafePath(format!(
            "'{}' exceeds its {limit} byte limit",
            path.display()
        ))
        .into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() || !same_file_identity(&named_metadata, &opened_metadata) {
        return Err(AppAuthoringError::UnsafePath(format!(
            "'{}' changed before it could be opened",
            path.display()
        ))
        .into());
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_metadata.len())
            .unwrap_or(0)
            .min(limit),
    );
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    let after_metadata = file.metadata()?;
    if bytes.len() > limit || !same_file_snapshot(&opened_metadata, &after_metadata) {
        return Err(AppAuthoringError::UnsafePath(format!(
            "'{}' changed or exceeded its limit while being read",
            path.display()
        ))
        .into());
    }
    Ok(bytes)
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
        && left.created().ok() == right.created().ok()
}

#[cfg(unix)]
fn same_file_snapshot(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    same_file_identity(left, right)
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(not(unix))]
fn same_file_snapshot(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    same_file_identity(left, right) && left.modified().ok() == right.modified().ok()
}

fn create_directory(path: &Path) -> Result<()> {
    fs::create_dir(path).with_context(|| format!("creating {}", path.display()))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("generated path has no parent")?;
    if !parent.exists() {
        fs::create_dir(parent)?;
    }
    let parent_metadata = fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(AppAuthoringError::UnsafePath(format!(
            "generated output parent '{}' must be a real directory",
            parent.display()
        ))
        .into());
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("generated path is not UTF-8")?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    write_new_file(&temporary, bytes)?;
    // Hand-rolled rather than the shared durable writer, deliberately: the
    // parent checks above are the point. App authoring writes content the owner
    // did not necessarily author into a scope directory, so this path refuses a
    // symlinked or non-directory parent before writing anything —
    // `write_bytes_atomic_sync` does not, and routing through it would drop the
    // guard. Every durability property the shared writer provides is present:
    // a unique temp name, `create_new` so it cannot clobber, `sync_all` on the
    // file, this rename, and the parent-directory sync below.
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    sync_parent(parent)?;
    Ok(())
}

fn publish_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("output path is not UTF-8")?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));
    write_new_file(&temporary, bytes)?;
    match fs::hard_link(&temporary, path) {
        Ok(()) => {
            fs::remove_file(&temporary)?;
            sync_parent(parent)?;
            Ok(())
        },
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Err(AppAuthoringError::OutputExists(path.to_path_buf()).into())
            } else {
                Err(error.into())
            }
        },
    }
}

fn sync_parent(parent: &Path) -> Result<()> {
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn scaffold_skill_document(name: &AppName) -> String {
    format!(
        r#"---
name: {name}
version: 0.1.0
description: A private app created with the Magician app authoring CLI.
metadata:
  magician:
    skill_type: app
    app_manifest_version: "{APP_MANIFEST_SCHEMA_VERSION}"
    app_sdk_version: "{APP_AUTHORING_SDK_VERSION}"
    required_features:
      - typed_entities_v1
      - declarative_views_v1
      - owner_data_plane_v1
    generated_by:
      sdk: {APP_AUTHORING_GENERATOR_NAME}
      version: "{APP_AUTHORING_SDK_SEMVER}"
app:
  compatibility:
    magician_contract: "1"
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: local_only
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    item:
      fields:
        title: {{ type: text, required: true }}
        status: {{ type: enum, values: [open, done], required: true }}
  views:
    items:
      entity: item
      kind: list
      route: /
  workflows: {{}}
  actions: {{}}
  resources:
    per_run:
      max_tokens: 1000
      max_cost_usd: 0.25
      max_active_seconds: 60
    monthly:
      max_tokens: 10000
      max_cost_usd: 5.00
    storage:
      max_records: 10000
      max_bytes: 10485760
  dependencies:
    procedure_skills: []
    tools: []
  assets: []
---
# {name}
"#
    )
}

fn scaffold_fixture_document(_name: &AppName) -> String {
    format!(
        r#"{{
  "schema_version": {AUTHORING_FIXTURE_VERSION},
  "records": [
    {{
      "name": "open_item",
      "entity": "item",
      "value": {{ "title": "A useful first item", "status": "open" }}
    }}
  ],
  "workflows": [],
  "views": [
    {{ "name": "items_with_one_record", "view": "items", "minimum_records": 1 }}
  ]
}}
"#
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn json_cli_envelopes_are_closed_versioned_and_one_line() {
        let command = AppAuthoringCommand::Select(AppSelectArgs {
            requirements: vec![AppArtifactRequirement::DurableTypedRecords],
        });
        let (success, success_status) =
            render_app_authoring_json_result(&command, Ok(json!({ "artifact": "app" }))).unwrap();
        assert_eq!(success_status, AppAuthoringCliExitStatus::Success);
        assert!(!success.contains('\n'));
        assert_eq!(
            serde_json::from_str::<Value>(&success).unwrap(),
            json!({
                "schema_version": 1,
                "cli_protocol_version": "1.0.0",
                "command": "select",
                "ok": true,
                "result": { "artifact": "app" }
            })
        );

        let (failure, failure_status) = render_app_authoring_json_result(
            &command,
            Err(anyhow::anyhow!(
                "failed /Users/private/app token=secret https://internal.example.test"
            )),
        )
        .unwrap();
        assert_eq!(failure_status, AppAuthoringCliExitStatus::CommandFailed);
        let failure = serde_json::from_str::<Value>(&failure).unwrap();
        assert_eq!(failure["schema_version"], 1);
        assert_eq!(failure["cli_protocol_version"], "1.0.0");
        assert_eq!(failure["command"], "select");
        assert_eq!(failure["ok"], false);
        assert_eq!(failure["error"]["code"], "app_authoring_command_failed");
        assert!(failure["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.contains('\n')));
        let serialized = serde_json::to_string(&failure).unwrap();
        assert!(!serialized.contains("/Users/private"));
        assert!(!serialized.contains("token=secret"));
        assert!(!serialized.contains("internal.example.test"));
        assert_eq!(failure.as_object().unwrap().len(), 5);
    }

    #[test]
    fn live_command_failure_uses_the_closed_redacted_json_envelope() {
        let command = AppAuthoringCommand::Approve(AppApproveArgs {
            installation_id: "install_fixture".to_owned(),
            review_digest: format!("blake3:{}", "a".repeat(64)),
            grant_tools: Vec::new(),
            grant_agents: Vec::new(),
            grant_personalities: Vec::new(),
            migration_run_id: None,
            update_plan_digest: None,
            confirm_destructive_migration: false,
            live: AppLiveScopeArgs {
                api_base: "http://127.0.0.1:3002".to_owned(),
            },
        });
        let (line, status) = render_app_authoring_json_result(
            &command,
            Err(anyhow::anyhow!(
                "connection failed at http://private.test token=secret"
            )),
        )
        .expect("live failures render");
        assert_eq!(status, AppAuthoringCliExitStatus::CommandFailed);
        let envelope = serde_json::from_str::<Value>(&line).expect("closed failure JSON");
        assert_eq!(envelope["command"], "approve");
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], "app_authoring_command_failed");
        assert!(!line.contains("private.test"));
        assert!(!line.contains("token=secret"));
        assert!(!line.contains('\n'));
    }

    fn initialized_project() -> (TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("reading-list");
        init_project(&AppInitArgs {
            name: "reading-list".to_owned(),
            path: Some(path.clone()),
        })
        .unwrap();
        (temporary, path)
    }

    #[test]
    fn authoring_cli_uses_the_shared_least_powerful_artifact_selector() {
        let procedure = select_project_artifact(&AppSelectArgs {
            requirements: vec![AppArtifactRequirement::ReusableInstructions],
        })
        .unwrap();
        assert_eq!(
            procedure.primary(),
            super::super::artifact_selection::AppAuthoringArtifactKind::ProcedureSkill
        );

        let app = select_project_artifact(&AppSelectArgs {
            requirements: vec![
                AppArtifactRequirement::InteractivePersonalSurface,
                AppArtifactRequirement::AppPrivateProcedure,
            ],
        })
        .unwrap();
        assert_eq!(
            app.primary(),
            super::super::artifact_selection::AppAuthoringArtifactKind::AppWithPrivateProcedures
        );
    }

    #[test]
    fn procedure_check_uses_the_same_strict_publication_parser() {
        let temporary = tempfile::tempdir().unwrap();
        let skill = temporary.path().join("SKILL.md");
        fs::write(
            &skill,
            b"---\nname: external-summary\nversion: 1.2.0\ndescription: Summarize reviewed input.\nmetadata:\n  magician:\n    skill_type: procedure\n---\nProduce one concise summary.\n",
        )
        .unwrap();
        let bytes = read_bounded_file(&skill, APP_PROCEDURE_SKILL_MAX_BYTES).unwrap();
        let inspection = inspect_standalone_procedure(&bytes).unwrap();
        assert_eq!(inspection.dependency_ref.as_str(), "skill:external-summary");
        assert_eq!(inspection.semantic_version, "1.2.0");
        assert!(inspection.publication_required);
        assert!(!inspection.activation_authority_granted);

        fs::write(
            &skill,
            b"---\nname: external-summary\nversion: 1.2.0\ndescription: Not a procedure.\nmetadata:\n  magician:\n    skill_type: tool\n---\nRun something.\n",
        )
        .unwrap();
        let bytes = read_bounded_file(&skill, APP_PROCEDURE_SKILL_MAX_BYTES).unwrap();
        assert!(inspect_standalone_procedure(&bytes).is_err());
    }

    #[test]
    fn catalog_list_commands_emit_json_for_coding_agents() {
        let temporary = tempfile::tempdir().unwrap();
        let tools_dir = temporary.path().join("skills");
        fs::create_dir_all(&tools_dir).unwrap();
        fs::create_dir_all(tools_dir.join("next-step")).unwrap();
        fs::write(
            tools_dir.join("next-step/SKILL.md"),
            crate::magician_v2::apps::tool_eligibility::typed_app_tool_document(
                "next-step",
                "1.0.0",
                "    expose:\n      apps: true\n",
            ),
        )
        .unwrap();
        let discover = AppCatalogDiscoverArgs {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            skills_dirs: vec![tools_dir],
            templates_dirs: Vec::new(),
        };
        let tools = list_authoring_tools(
            &discovery_roots(&discover),
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: Some(AuthoringToolKind::Skill),
            },
        );
        assert_eq!(tools.count, 1);
        assert_eq!(tools.items[0].name, "next-step");

        let agents = list_authoring_agents(&discovery_roots(&discover));
        assert!(agents
            .items
            .iter()
            .any(|entry| entry.name == "personal-assistant"));
    }

    #[test]
    fn init_scaffolds_tools_not_capability_wrappers() {
        let (_temporary, path) = initialized_project();
        let skill = fs::read_to_string(path.join("SKILL.md")).unwrap();
        assert!(skill.contains("tools: []"));
        assert!(!skill.contains("capabilities: []"));
    }

    #[test]
    fn init_check_and_provider_free_tests_share_one_strict_package() {
        let (_temporary, path) = initialized_project();
        let checked = check_project(&path, false).unwrap();
        assert_eq!(checked.package_name, "reading-list");
        assert_eq!(checked.generated_artifacts, "current");
        let tested = test_project(&path).unwrap();
        assert_eq!(tested.entity_fixtures, 1);
        assert_eq!(tested.view_fixtures, 1);
    }

    #[test]
    fn preview_compiles_fixture_backed_muij_without_activation_authority() {
        let (_temporary, path) = initialized_project();
        let preview = preview_project(&AppPreviewArgs {
            path: path.clone(),
            view: Some("items".to_owned()),
        })
        .unwrap();
        assert!(!preview.authoritative);
        assert!(!preview.activation_capable);
        assert_eq!(preview.previews.len(), 1);
        assert_eq!(preview.previews[0].view_id, "items");
        assert_eq!(preview.previews[0].fixture_records.len(), 1);
        assert_eq!(preview.previews[0].surface["interaction_mode"], "app");

        assert!(preview_project(&AppPreviewArgs {
            path,
            view: Some("missing".to_owned()),
        })
        .is_err());
    }

    #[test]
    fn generated_drift_fails_until_explicit_regeneration() {
        let (_temporary, path) = initialized_project();
        fs::write(path.join(GENERATED_TYPESCRIPT_PATH), b"stale").unwrap();
        assert!(check_project(&path, false).is_err());
        assert!(check_project(&path, true).is_ok());
        assert!(check_project(&path, false).is_ok());
    }

    #[test]
    fn generated_typescript_is_deterministic_and_correlates_exact_action_schemas() {
        let (_temporary, path) = initialized_project();
        fs::create_dir(path.join("workflows")).unwrap();
        fs::write(
            path.join("workflows/build.md"),
            "Return one reviewed item projection.",
        )
        .unwrap();
        let manifest_path = path.join("SKILL.md");
        let manifest = fs::read_to_string(&manifest_path)
            .unwrap()
            .replace(
                "  workflows: {}",
                r#"  workflows:
    build:
      prompt: workflows/build.md
      runner: auto
      uses: []
      procedures: []
      input:
        type: object
        fields:
          request: { type: text, required: true }
      result:
        kind: entity_projection
        entities: [item]
        output_schema:
          type: object
          fields:
            summary: { type: markdown, required: true }
      may_mutate: [item]
      trigger: user"#,
            )
            .replace(
                "  actions: {}",
                r#"  actions:
    build:
      workflow: build
      input_from: build.input
      result_from: build.result"#,
            );
        fs::write(&manifest_path, manifest).unwrap();

        let candidate = admit_package_directory(&path.canonicalize().unwrap()).unwrap();
        let first = render_generated_typescript(&candidate).unwrap();
        let second = render_generated_typescript(&candidate).unwrap();
        assert_eq!(first, second);
        let generated = String::from_utf8(first).unwrap();
        for expected in [
            "export interface AppActionContracts",
            "readonly \"build\"",
            "AppWorkflowInputBuild_",
            "AppWorkflowResultBuild_",
            "defineAppValueCodec",
            "APP_WORKFLOW_FORMS",
            "AppCustomSurfaceBridge",
            "export const AppRecipe",
        ] {
            assert!(
                generated.contains(expected),
                "missing generated surface: {expected}"
            );
        }

        check_project(&path, true).unwrap();
        let derived: Value =
            serde_json::from_slice(&fs::read(path.join(DERIVED_JSON_PATH)).unwrap()).unwrap();
        assert_eq!(derived["typescript_generation_version"], 2);
        assert_eq!(
            derived["workflow_schema_identities"]["build"]["result_kind"],
            "entity_projection"
        );
        assert!(derived["generated_typescript_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("blake3:")));
        assert!(check_project(&path, false).is_ok());
    }

    #[test]
    fn fixture_unknown_fields_and_missing_view_records_fail_closed() {
        let (_temporary, path) = initialized_project();
        let fixture = path.join(FIXTURE_PATH);
        fs::write(
            &fixture,
            br#"{"schema_version":1,"records":[{"name":"bad","entity":"item","value":{"title":"x","status":"open","secret":true}}],"workflows":[],"views":[]}"#,
        )
        .unwrap();
        assert!(test_project(&path).is_err());

        fs::write(
            fixture,
            br#"{"schema_version":1,"records":[],"workflows":[],"views":[{"name":"empty","view":"items","minimum_records":1}]}"#,
        )
        .unwrap();
        assert!(test_project(&path).is_err());
    }

    #[test]
    fn provider_free_test_consumes_one_admitted_snapshot() {
        let (_temporary, path) = initialized_project();
        let candidate = admit_package_directory(&path.canonicalize().unwrap()).unwrap();
        fs::write(
            path.join(FIXTURE_PATH),
            br#"{"schema_version":1,"records":[],"workflows":[],"views":[]}"#,
        )
        .unwrap();

        let tested = test_candidate(&candidate).unwrap();

        assert_eq!(tested.entity_fixtures, 1);
        assert!(test_project(&path).is_err());
    }

    #[test]
    fn pack_is_create_only_package_only_and_transfers_no_authority() {
        let (temporary, path) = initialized_project();
        let output = temporary.path().join("reading-list.app.zip");
        let report = pack_project(&AppPackArgs {
            path: path.clone(),
            publisher: "publisher:test".to_owned(),
            resolutions: None,
            output: Some(output.clone()),
            discover: AppCatalogDiscoverArgs::default(),
        })
        .unwrap();
        assert!(output.is_file());
        assert!(!report.authority_transferred);
        assert!(report.server_revalidation_required);
        let admitted =
            super::super::package_transfer::admit_package_archive(&fs::read(&output).unwrap())
                .unwrap();
        assert_eq!(admitted.package().package_id.as_str(), "app:reading-list");
        let candidate = admit_package_directory(&path.canonicalize().unwrap()).unwrap();
        let expected_revision = canonical_package_revision_ref_from_identity(
            &AppReference::parse("app:reading-list").unwrap(),
            &candidate.manifest().manifest().version,
            candidate.bundle_digest(),
            &admitted.package().dependency_lock_digest,
        )
        .unwrap();
        assert_eq!(admitted.package().package_revision_ref, expected_revision);
        assert!(pack_project(&AppPackArgs {
            path,
            publisher: "publisher:test".to_owned(),
            resolutions: None,
            output: Some(output),
            discover: AppCatalogDiscoverArgs::default(),
        })
        .is_err());
    }

    #[test]
    fn explicit_discovery_roots_are_shared_by_tool_listing_and_pack() {
        let (temporary, path) = initialized_project();
        let skills_root = temporary.path().join("reviewed-skills");
        let skill_root = skills_root.join("next-step");
        fs::create_dir_all(&skill_root).unwrap();
        fs::write(
            skill_root.join("SKILL.md"),
            crate::magician_v2::apps::tool_eligibility::typed_app_tool_document(
                "next-step",
                "1.4.0",
                "    expose:\n      apps: true\n",
            ),
        )
        .unwrap();
        fs::create_dir(path.join("workflows")).unwrap();
        fs::write(
            path.join("workflows/next-step.md"),
            "Return one reviewed next-step item projection.",
        )
        .unwrap();
        let manifest_path = path.join("SKILL.md");
        let manifest = fs::read_to_string(&manifest_path)
            .unwrap()
            .replace(
                "    tools: []",
                "    tools:\n      - name: next-step\n        version_requirement: \"^1\"",
            )
            .replace(
                "  workflows: {}",
                r#"  workflows:
    next_step:
      prompt: workflows/next-step.md
      runner: auto
      uses: [next-step]
      procedures: []
      input:
        type: object
        fields:
          request: { type: text, required: true }
      result:
        kind: entity_projection
        entities: [item]
      may_mutate: [item]
      trigger: user"#,
            );
        fs::write(&manifest_path, manifest).unwrap();
        check_project(&path, true).unwrap();

        let discover = AppCatalogDiscoverArgs {
            principal: "author".to_owned(),
            workspace: "project".to_owned(),
            skills_dirs: vec![skills_root],
            templates_dirs: Vec::new(),
        };
        let listed = list_authoring_tools(
            &discovery_roots(&discover),
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: Some(AuthoringToolKind::Skill),
            },
        );
        assert!(listed.items.iter().any(|tool| tool.name == "next-step"));

        let output = temporary.path().join("reading-list-with-skill.app.zip");
        let packed = pack_project(&AppPackArgs {
            path,
            publisher: "publisher:test".to_owned(),
            resolutions: None,
            output: Some(output.clone()),
            discover,
        })
        .expect("the listed scoped skill is projected into the package lock");
        assert!(output.is_file());
        assert!(!packed.dependency_lock_digest.as_str().is_empty());
    }

    #[test]
    fn external_authoring_checks_and_packs_only_explicit_agent_and_browser_leaves() {
        let (temporary, path) = initialized_project();
        let skills_root = temporary.path().join("reviewed-skills");
        let browser_root = skills_root.join("browser");
        fs::create_dir_all(&browser_root).unwrap();
        let browser = String::from_utf8(
            crate::magician_v2::apps::tool_eligibility::typed_app_tool_document(
                "browser", "1.0.0", "",
            ),
        )
        .unwrap()
        .replace(
            "---\nReturn one ranked next step.",
            "    runtime_catalog:\n      categories: [browser]\n      composition_category: \
             web_operations\n---\nReturn one ranked next step.",
        );
        fs::write(browser_root.join("SKILL.md"), browser).unwrap();

        let templates_root = temporary.path().join("reviewed-agents");
        let agent_root = templates_root.join("agents/reviewed-child");
        fs::create_dir_all(&agent_root).unwrap();
        fs::write(
            agent_root.join("definition.agent.yaml"),
            r#"agent_id: reviewed-child
version: 1
name: Reviewed child
description: One exact callable child.
persona: Return only the reviewed typed result.
tools: [content_read]
app_tool:
  input:
    type: object
    fields:
      request: { type: text, required: true }
  result:
    type: object
    fields:
      answer: { type: markdown, required: true }
  max_input_bytes: 16384
  max_result_bytes: 32768
"#,
        )
        .unwrap();

        fs::create_dir_all(path.join("workflows")).unwrap();
        fs::write(
            path.join("workflows/sealed.md"),
            "Use only the two declared leaves.",
        )
        .unwrap();
        let manifest_path = path.join("SKILL.md");
        let manifest = fs::read_to_string(&manifest_path)
            .unwrap()
            .replace(
                "  workflows: {}",
                r#"  workflows:
    sealed:
      prompt: workflows/sealed.md
      runner: auto
      uses: [reviewed-child, browser]
      input:
        type: object
        fields:
          request: { type: text, required: true }
      result:
        kind: typed_value
        entities: []
        output_schema:
          type: object
          fields:
            answer: { type: markdown, required: true }
      may_mutate: []
      trigger: user"#,
            )
            .replace(
                "    tools: []",
                r#"    tools:
      - name: reviewed-child
        actions: [agent_as_tool]
        version_requirement: "^1"
      - name: browser
        actions: [snapshot]
        version_requirement: "^1""#,
            );
        fs::write(&manifest_path, manifest).unwrap();
        check_project(&path, true).expect("explicit sealed declarations check");

        let discover = AppCatalogDiscoverArgs {
            principal: "author".to_owned(),
            workspace: "project".to_owned(),
            skills_dirs: vec![skills_root],
            templates_dirs: vec![templates_root],
        };
        let listed = list_authoring_tools(
            &discovery_roots(&discover),
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: None,
            },
        );
        for (name, action) in [
            ("reviewed-child", "actions: [agent_as_tool]"),
            ("browser", "actions: [snapshot]"),
        ] {
            let entry = listed
                .items
                .iter()
                .find(|entry| entry.name == name)
                .expect("sealed leaf is discoverable");
            assert!(entry.yaml_declaration.contains(action));
        }

        let output = temporary.path().join("reading-list-sealed.app.zip");
        let packed = pack_project(&AppPackArgs {
            path,
            publisher: "publisher:test".to_owned(),
            resolutions: None,
            output: Some(output.clone()),
            discover,
        })
        .expect("explicit sealed leaves pack into one immutable lock");
        assert!(output.is_file());
        assert!(!packed.dependency_lock_digest.as_str().is_empty());
    }

    #[test]
    fn resolution_and_output_files_cannot_be_smuggled_into_package() {
        let (temporary, path) = initialized_project();
        let inside = path.join("resolutions.json");
        fs::write(&inside, br#"{"schema_version":1,"dependencies":[]}"#).unwrap();
        assert!(dependency_resolution_evidence(
            &path.canonicalize().unwrap(),
            Some(&inside),
            &admit_package_directory(&path.canonicalize().unwrap()).unwrap(),
        )
        .is_err());
        assert!(resolve_pack_output(
            &path.canonicalize().unwrap(),
            Some(&path.join("bad.app.zip")),
            &admit_package_directory(&path.canonicalize().unwrap()).unwrap(),
        )
        .is_err());
        drop(temporary);
    }

    #[test]
    fn dependency_content_paths_reject_absolute_and_parent_traversal() {
        assert!(safe_relative_resolution_path("../secret.json").is_err());
        assert!(safe_relative_resolution_path("fixtures/../../secret.json").is_err());
        assert!(safe_relative_resolution_path("/tmp/secret.json").is_err());
        assert!(safe_relative_resolution_path("fixtures/dependency.json").is_ok());
    }

    /// Repo seed location of the first-party plan-2.2 package and the agent
    /// template it wraps. The package is colocated with the template so the
    /// dogfood cannot drift from the definition it declares.
    fn harness_sre_template_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../magician_data_v3/system/agent_templates/agents/harness-sre")
    }

    fn copy_package_tree(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_package_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[test]
    fn first_party_harness_sre_app_package_admits_checks_and_seals_the_system_template() {
        let template_root = harness_sre_template_root();
        let temporary = tempfile::tempdir().unwrap();
        // The exact staging admission the candidate API uses runs against a
        // copy so generated-output checks can write without touching the seed.
        let package_root = temporary.path().join("harness-sre-reliability");
        copy_package_tree(&template_root.join("app"), &package_root);
        let package_root = package_root.canonicalize().unwrap();

        let candidate = admit_package_directory(&package_root).unwrap();
        let manifest = candidate.manifest().manifest();
        assert_eq!(manifest.name.as_str(), "harness-sre-reliability");
        assert_eq!(
            manifest
                .declared_agents()
                .iter()
                .map(AppName::as_str)
                .collect::<Vec<_>>(),
            vec!["harness-sre"]
        );
        let workflow = manifest
            .app
            .workflows
            .get(&AppName::parse("run_audit").unwrap())
            .expect("run_audit workflow");
        assert_eq!(
            workflow.agent.as_ref().map(AppName::as_str),
            Some("harness-sre")
        );
        assert!(matches!(workflow.runner, AppManifestRunner::Auto));
        assert!(workflow.uses.is_empty());
        assert!(workflow.procedures.is_empty());
        assert!(workflow.contribution_ports.is_empty());
        assert!(manifest.declared_tools().unwrap().is_empty());
        assert!(manifest.app.dependencies.procedure_skills.is_empty());
        assert!(manifest.app.llm_operations.is_empty());
        assert_eq!(
            manifest.metadata.magician.required_features,
            vec![
                AppManifestFeature::TypedEntitiesV1,
                AppManifestFeature::DeclarativeViewsV1,
                AppManifestFeature::GovernedActionsV1,
                AppManifestFeature::ImmutableDependenciesV1,
                AppManifestFeature::OwnerDataPlaneV1,
                AppManifestFeature::DurableActionRunsV1,
            ]
        );
        assert_eq!(manifest.app.entities.len(), 1);
        assert_eq!(manifest.app.views.len(), 1);
        assert_eq!(manifest.app.actions.len(), 1);
        assert_eq!(manifest.app.assets.len(), 0);

        // Full authoring conformance on the copy: generated artifacts are
        // written by the same `--write-generated` path the operator flow uses,
        // then stay current, and the provider-free fixture suite passes.
        let checked = check_project(&package_root, true).unwrap();
        assert_eq!(checked.status, "valid");
        assert_eq!(checked.workflow_count, 1);
        assert!(check_project(&package_root, false).is_ok());
        assert_eq!(test_project(&package_root).unwrap().status, "passed");

        // The binding half installation review performs: the existing system
        // template itself must parse, admit the Task surface, and seal as the
        // reviewed workflow material for the declared runner.
        let definition_source =
            fs::read_to_string(template_root.join("definition.agent.yaml")).unwrap();
        let definition =
            crate::magician_v2::agents::AgentDefinition::from_yaml_str(&definition_source)
                .expect("the system harness-sre template parses");
        assert_eq!(definition.agent_id, "harness-sre");
        assert!(
            crate::magician_v2::apps::agent_capability::agent_definition_permits_app_task(
                &definition
            )
        );
        let sealed = crate::magician_v2::apps::agent_capability::seal_reviewed_workflow_material(
            &AppName::parse("run_audit").unwrap(),
            &AppReference::parse("agent:harness-sre").unwrap(),
            &definition,
            None,
        )
        .unwrap();
        assert_eq!(sealed.agent_ref.as_str(), "agent:harness-sre");
        assert_eq!(sealed.workflow_id.as_str(), "run_audit");
        assert!(sealed.binding_digest.as_str().len() > "blake3:".len());
        crate::magician_v2::apps::agent_capability::revalidate_reviewed_workflow_material(
            &sealed,
            &definition,
            None,
        )
        .unwrap();
    }

    #[test]
    fn agent_template_discovery_ignores_the_colocated_first_party_app_package() {
        // .../agent_templates/agents/harness-sre -> .../agent_templates
        let templates_root = harness_sre_template_root()
            .ancestors()
            .nth(2)
            .map(Path::to_path_buf)
            .expect("the harness-sre template sits under agent_templates");
        let roots = AuthoringDiscoveryRoots::from_explicit(
            Vec::<PathBuf>::new(),
            [templates_root.canonicalize().unwrap()],
        );
        let listed = list_authoring_agents(&roots);
        let harness_entries = listed
            .items
            .iter()
            .filter(|entry| entry.name == "harness-sre")
            .collect::<Vec<_>>();
        assert_eq!(harness_entries.len(), 1, "harness-sre stays discoverable");
        assert_eq!(
            harness_entries[0].yaml_declaration, "agent: harness-sre",
            "the template remains the authoring recommendation"
        );
        assert!(
            listed.items.iter().all(|entry| entry.name != "app"),
            "the colocated package directory is not an agent entry"
        );
    }

    /// Repo seed location of the plan-2.4 exemplar: the published skillshub
    /// pack and the first-party wrapper package colocated beside it, so the
    /// dogfood cannot drift from the pack it declares.
    fn youtube_search_pack_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../skillshub/youtube-search")
    }

    #[test]
    fn first_party_youtube_search_app_package_admits_and_locks_the_published_tool_skill() {
        let pack_root = youtube_search_pack_root();
        let temporary = tempfile::tempdir().unwrap();
        // The exact staging admission the candidate API uses runs against a
        // copy so generated-output checks can write without touching the seed.
        let package_root = temporary.path().join("youtube-search-coverage");
        copy_package_tree(&pack_root.join("app"), &package_root);
        let package_root = package_root.canonicalize().unwrap();

        let candidate = admit_package_directory(&package_root).unwrap();
        let manifest = candidate.manifest().manifest();
        assert_eq!(manifest.name.as_str(), "youtube-search-coverage");
        let declared_tools = manifest.declared_tools().unwrap();
        assert_eq!(declared_tools.len(), 1);
        assert_eq!(declared_tools[0].name.as_str(), "youtube-search");
        assert_eq!(declared_tools[0].version_requirement, "^0.3");
        assert!(declared_tools[0].primitive_ref.is_none());
        let workflow = manifest
            .app
            .workflows
            .get(&AppName::parse("find_videos").unwrap())
            .expect("find_videos workflow");
        assert!(matches!(workflow.runner, AppManifestRunner::Auto));
        assert!(workflow.agent.is_none());
        assert_eq!(
            workflow
                .uses
                .iter()
                .map(AppName::as_str)
                .collect::<Vec<_>>(),
            vec!["youtube-search"]
        );
        assert!(workflow.procedures.is_empty());
        assert!(manifest.app.dependencies.procedure_skills.is_empty());
        assert!(manifest.app.llm_operations.is_empty());
        assert_eq!(manifest.app.entities.len(), 1);
        assert_eq!(manifest.app.views.len(), 1);
        assert_eq!(manifest.app.actions.len(), 1);
        assert_eq!(manifest.app.assets.len(), 0);

        // Full authoring conformance on the copy: generated artifacts are
        // written by the same `--write-generated` path the operator flow uses,
        // then stay current, and the provider-free fixture suite passes.
        let checked = check_project(&package_root, true).unwrap();
        assert_eq!(checked.status, "valid");
        assert_eq!(checked.workflow_count, 1);
        assert!(check_project(&package_root, false).is_ok());
        assert_eq!(test_project(&package_root).unwrap().status, "passed");

        // The binding half installation review performs for a ToolSkill: the
        // real skillshub pack is scanned exactly as the resolver scans it,
        // projects one lockable ToolSkill descriptor carrying the plan-2.4
        // publication metadata, and snapshots into `capability:youtube-search`
        // lock evidence whose exact bytes and immutable revision the package
        // lock then seals.
        let skill_bytes = fs::read(pack_root.join("SKILL.md")).unwrap();
        let roots = AuthoringDiscoveryRoots::from_explicit(
            [pack_root.canonicalize().unwrap()],
            Vec::<PathBuf>::new(),
        );
        let snapshot = resolve_authoring_primitive_catalog(&roots);
        assert!(snapshot.complete(), "the exemplar pack scans clean");
        let descriptor = snapshot.resolve("youtube-search").unwrap();
        assert_eq!(
            descriptor.kind(),
            crate::magician_v2::apps::primitive_catalog::AppPrimitiveKind::ToolSkill
        );
        assert_eq!(descriptor.display_name(), Some("YouTube Search"));
        assert_eq!(descriptor.semantic_version(), Some("0.3.1"));

        let tool_catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete primitive snapshot");
        // The exact evidence `app pack` assembles: the builtin contract claim,
        // then the scoped ToolSkill snapshot filled by the same resolver call.
        let builtin_evidence = dependency_resolution_evidence(&package_root, None, &candidate)
            .expect("builtin contract evidence");
        let evidence = complete_declared_tool_evidence(&candidate, builtin_evidence, &tool_catalog)
            .expect("scoped ToolSkill evidence");
        assert_eq!(evidence.len(), 2);
        let tool_evidence = evidence
            .iter()
            .find(|claim| claim.dependency_ref().as_str() == "capability:youtube-search")
            .expect("the declared ToolSkill snapshots into lock evidence");
        assert_eq!(tool_evidence.semantic_version(), "0.3.1");
        assert_eq!(
            tool_evidence.content_digest(),
            &AppDigest::blake3(&skill_bytes),
            "the locked bytes are the pack's exact SKILL.md"
        );
        assert!(tool_evidence
            .immutable_revision_ref()
            .as_str()
            .starts_with("primitive-source:skill:"));
        let reviewed_binding = tool_catalog.primitive_binding("youtube-search");
        assert_eq!(
            tool_evidence.primitive_binding(),
            reviewed_binding,
            "the evidence retains the exact reviewed primitive binding"
        );

        let lock =
            lock_app_package_dependencies(&candidate, evidence, &AppPackageLimits::default())
                .expect("immutable package lock");
        assert!(lock
            .capability(&AppReference::parse("capability:youtube-search").unwrap())
            .is_some());
        assert!(!lock.lock_digest().as_str().is_empty());
    }

    #[test]
    fn skill_scanner_ignores_the_colocated_first_party_app_package() {
        // The resolver reads only `<pack-root>/SKILL.md`; a nested app package
        // directory is invisible to skill discovery by construction, and the
        // `skill_type: app` guard additionally keeps any package manifest out
        // of the primitive scanner.
        let roots = AuthoringDiscoveryRoots::from_explicit(
            [youtube_search_pack_root().canonicalize().unwrap()],
            Vec::<PathBuf>::new(),
        );
        let snapshot = resolve_authoring_primitive_catalog(&roots);
        let listed = list_authoring_tools(
            &roots,
            AuthoringToolListFilter {
                app_eligible_only: true,
                kind: None,
            },
        );
        let youtube_entries = listed
            .items
            .iter()
            .filter(|entry| entry.name == "youtube-search")
            .collect::<Vec<_>>();
        assert_eq!(youtube_entries.len(), 1, "the pack stays discoverable");
        assert_eq!(
            youtube_entries[0].yaml_declaration,
            "- name: youtube-search\n  version_requirement: \"^0\"",
            "the friendly-name tool declaration remains the authoring recommendation"
        );
        assert!(
            listed
                .items
                .iter()
                .all(|entry| entry.name != "youtube-search-coverage"),
            "the colocated wrapper package is not a tool entry"
        );
        assert!(
            snapshot
                .descriptors()
                .iter()
                .all(|descriptor| descriptor.name() != "youtube-search-coverage"),
            "no primitive is projected from the package manifest"
        );
    }

    /// Repo seed location of the plan-2.5 package: the memory & learning
    /// review console colocated with the learning subsystem's system-data
    /// home, so the console cannot drift from the substrate it reviews.
    fn memory_review_package_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3/system/learning/app")
    }

    #[test]
    fn first_party_memory_review_package_admits_and_locks_the_learning_read_port() {
        let package_seed = memory_review_package_root();
        let temporary = tempfile::tempdir().unwrap();
        // The exact staging admission the candidate API uses runs against a
        // copy so generated-output checks can write without touching the seed.
        let package_root = temporary.path().join("memory-learning-review");
        copy_package_tree(&package_seed, &package_root);
        let package_root = package_root.canonicalize().unwrap();

        let candidate = admit_package_directory(&package_root).unwrap();
        let manifest = candidate.manifest().manifest();
        assert_eq!(manifest.name.as_str(), "memory-learning-review");
        let declared_tools = manifest.declared_tools().unwrap();
        assert_eq!(declared_tools.len(), 1);
        assert_eq!(declared_tools[0].name.as_str(), "internal_data");
        assert_eq!(declared_tools[0].version_requirement, "^1.13");
        assert_eq!(
            declared_tools[0].actions,
            vec![
                "list_learning_candidates".to_owned(),
                "read_learning_candidate".to_owned()
            ],
            "the dependency narrows to exactly the two reviewed learning reads"
        );
        assert!(declared_tools[0].primitive_ref.is_none());
        assert_eq!(manifest.app.entities.len(), 2);
        assert_eq!(manifest.app.views.len(), 2);
        assert_eq!(manifest.app.actions.len(), 4);
        assert_eq!(manifest.app.assets.len(), 0);
        let sync = manifest
            .app
            .workflows
            .get(&AppName::parse("sync_queue").unwrap())
            .expect("sync_queue workflow");
        assert!(matches!(sync.runner, AppManifestRunner::Recipe));
        assert!(sync.recipe.is_some());
        assert!(sync.agent.is_none());
        assert_eq!(
            sync.uses.iter().map(AppName::as_str).collect::<Vec<_>>(),
            vec!["internal_data"]
        );
        assert!(sync.contribution_ports.is_empty());
        for decision in ["approve_candidate", "reject_candidate", "snooze_candidate"] {
            let workflow = manifest
                .app
                .workflows
                .get(&AppName::parse(decision).unwrap())
                .unwrap_or_else(|| panic!("{decision} workflow"));
            assert!(workflow.uses.is_empty(), "{decision} runs tool-free");
            assert_eq!(
                workflow.may_mutate,
                vec![AppName::parse("review_decision").unwrap()],
                "{decision} writes only the decision ledger"
            );
        }

        // Full authoring conformance on the copy: generated artifacts are
        // written by the same `--write-generated` path the operator flow uses,
        // then stay current, and the provider-free fixture suite passes.
        let checked = check_project(&package_root, true).unwrap();
        assert_eq!(checked.status, "valid");
        assert_eq!(checked.workflow_count, 4);
        assert!(check_project(&package_root, false).is_ok());
        assert_eq!(test_project(&package_root).unwrap().status, "passed");

        // The binding half installation review performs for the compiled
        // platform tool (plan 2.5): the embedded `internal_data` pack projects
        // exactly the two learning review reads as dispatch-Ready app actions,
        // the dependency's action subset selects exactly those leaves, and the
        // package lock seals the embedded pack bytes behind
        // `capability:internal_data`.
        let roots =
            AuthoringDiscoveryRoots::from_explicit(Vec::<PathBuf>::new(), Vec::<PathBuf>::new());
        let snapshot = resolve_authoring_primitive_catalog(&roots);
        assert!(
            snapshot.complete(),
            "the embedded platform catalog scans clean without scoped roots"
        );
        let descriptor = snapshot.resolve("internal_data").unwrap();
        assert_eq!(
            descriptor.kind(),
            crate::magician_v2::apps::primitive_catalog::AppPrimitiveKind::CompiledTool
        );
        assert_eq!(
            descriptor
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            vec!["list_learning_candidates", "read_learning_candidate"]
        );

        let tool_catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete primitive snapshot");
        // The exact evidence `app pack` assembles: the builtin contract claim,
        // then the scoped compiled-tool evidence filled by the same resolver
        // call.
        let builtin_evidence = dependency_resolution_evidence(&package_root, None, &candidate)
            .expect("builtin contract evidence");
        let evidence = complete_declared_tool_evidence(&candidate, builtin_evidence, &tool_catalog)
            .expect("scoped learning-read evidence");
        assert_eq!(evidence.len(), 2);
        let tool_evidence = evidence
            .iter()
            .find(|claim| claim.dependency_ref().as_str() == "capability:internal_data")
            .expect("the declared learning-read tool snapshots into lock evidence");
        assert_eq!(tool_evidence.semantic_version(), "1.13.0");
        let embedded_bytes =
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_yaml(
                "internal_data",
            )
            .expect("embedded internal_data pack")
            .as_bytes();
        assert_eq!(
            tool_evidence.content_digest(),
            &AppDigest::blake3(embedded_bytes),
            "the locked bytes are the exact embedded pack definition"
        );
        assert_eq!(
            tool_evidence
                .primitive_binding()
                .expect("the evidence snapshots the reviewed primitive binding")
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            vec!["list_learning_candidates", "read_learning_candidate"],
            "the reviewed binding keeps exactly the two selected actions"
        );

        let lock =
            lock_app_package_dependencies(&candidate, evidence, &AppPackageLimits::default())
                .expect("immutable package lock");
        assert!(lock
            .capability(&AppReference::parse("capability:internal_data").unwrap())
            .is_some());
        assert!(!lock.lock_digest().as_str().is_empty());
    }

    #[test]
    fn learning_package_home_stays_outside_authoring_discovery() {
        // `magician_data_v3/system/learning` sits outside every agent-template
        // and skill discovery root, so the colocated package is invisible to
        // both scanners by construction; this pins that the new system-data
        // directory does not leak into authoring discovery as a side effect.
        let templates_root = harness_sre_template_root()
            .ancestors()
            .nth(2)
            .map(Path::to_path_buf)
            .expect("the harness-sre template sits under agent_templates");
        let roots = AuthoringDiscoveryRoots::from_explicit(
            Vec::<PathBuf>::new(),
            [templates_root.canonicalize().unwrap()],
        );
        let listed = list_authoring_agents(&roots);
        assert!(
            listed
                .items
                .iter()
                .all(|entry| entry.name != "memory-learning-review"),
            "the console package is not an agent entry"
        );
        assert!(
            listed.items.iter().any(|entry| entry.name == "harness-sre"),
            "agent discovery is unaffected by the learning package"
        );

        // The skill-side scan runs the same system-data home as an explicit
        // skill root through the same catalog/resolver entry point the
        // youtube-search discovery dogfood uses: the `skill_type: app` guard
        // keeps the colocated console package out of the primitive catalog,
        // so no `memory-learning-review` primitive can appear from it.
        let learning_home = memory_review_package_root()
            .parent()
            .expect("the console package sits inside the learning system-data home")
            .canonicalize()
            .unwrap();
        let skill_roots =
            AuthoringDiscoveryRoots::from_explicit([learning_home], Vec::<PathBuf>::new());
        let snapshot = resolve_authoring_primitive_catalog(&skill_roots);
        assert!(
            snapshot.complete(),
            "the learning system-data home scans clean as a skill root"
        );
        assert!(
            snapshot
                .descriptors()
                .iter()
                .all(|descriptor| descriptor.name() != "memory-learning-review"),
            "no primitive is projected from the console package"
        );
    }

    /// Repo seed location of the plan-4 Brainstorm reference package: the
    /// thinking-map canvas colocated with the thinking-map subsystem's
    /// system-data home, so the reference consumer cannot drift from the
    /// substrate it renders.
    fn brainstorm_canvas_package_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3/system/thinking_map/app")
    }

    #[test]
    fn first_party_brainstorm_canvas_package_admits_and_locks_the_thinking_map_read_port() {
        let package_seed = brainstorm_canvas_package_root();
        let temporary = tempfile::tempdir().unwrap();
        // The exact staging admission the candidate API uses runs against a
        // copy so generated-output checks can write without touching the seed.
        let package_root = temporary.path().join("brainstorm-canvas");
        copy_package_tree(&package_seed, &package_root);
        let package_root = package_root.canonicalize().unwrap();

        let candidate = admit_package_directory(&package_root).unwrap();
        let manifest = candidate.manifest().manifest();
        assert_eq!(manifest.name.as_str(), "brainstorm-canvas");
        // The custom-surface declaration is the full coherent 1.6 block:
        // feature + permission + one entry point over a shipped HTML member.
        assert!(manifest
            .metadata
            .magician
            .required_features
            .contains(&AppManifestFeature::CustomSurfacesV1));
        assert_eq!(
            manifest.app.permissions,
            vec![crate::magician_v2::apps::manifest::AppManifestPermission::CustomSurface]
        );
        let declaration = manifest
            .app
            .custom_surface
            .as_ref()
            .expect("custom_surface declaration block");
        assert_eq!(declaration.entry_points.len(), 1);
        assert_eq!(declaration.entry_points[0].route.as_str(), "/canvas");
        assert_eq!(
            declaration.entry_points[0].document.as_str(),
            "surfaces/canvas.html"
        );
        // The surfaces tree ships both members the CSP posture requires: the
        // HTML entry document and the separate script member (inline script
        // is forbidden; the script is its own reviewed executable member).
        let canvas_html = candidate
            .member(&AppBundlePath::parse("surfaces/canvas.html").unwrap())
            .expect("entry document member");
        assert!(canvas_html.bytes().starts_with(b"<!DOCTYPE html>"));
        let canvas_js = candidate
            .member(&AppBundlePath::parse("surfaces/canvas.js").unwrap())
            .expect("script member");
        let canvas_js_text = std::str::from_utf8(canvas_js.bytes()).unwrap();
        assert!(
            canvas_js_text.contains("magician-surface-bridge"),
            "the script speaks the bridge channel"
        );
        assert!(
            !canvas_js_text.contains("innerHTML"),
            "the script renders through DOM construction, not raw HTML"
        );

        // The thinking-map host-read dependency narrows to exactly the two
        // reviewed scoped reads.
        let declared_tools = manifest.declared_tools().unwrap();
        assert_eq!(declared_tools.len(), 1);
        assert_eq!(declared_tools[0].name.as_str(), "thinking_maps_data");
        assert_eq!(declared_tools[0].version_requirement, "^1.0");
        assert_eq!(
            declared_tools[0].actions,
            vec!["list_maps".to_owned(), "read_map".to_owned()],
            "the dependency narrows to exactly the two scoped thinking-map reads"
        );
        assert!(declared_tools[0].primitive_ref.is_none());
        assert_eq!(manifest.app.entities.len(), 2);
        assert_eq!(manifest.app.views.len(), 1);
        assert_eq!(manifest.app.actions.len(), 1);
        let sync = manifest
            .app
            .workflows
            .get(&AppName::parse("sync_maps").unwrap())
            .expect("sync_maps workflow");
        assert!(matches!(sync.runner, AppManifestRunner::Recipe));
        assert!(sync.recipe.is_some());
        assert!(sync.agent.is_none());
        assert_eq!(
            sync.uses.iter().map(AppName::as_str).collect::<Vec<_>>(),
            vec!["thinking_maps_data"]
        );
        assert!(sync.contribution_ports.is_empty());
        assert_eq!(
            sync.may_mutate
                .iter()
                .map(AppName::as_str)
                .collect::<Vec<_>>(),
            vec!["thinking_map_summary", "thinking_map_snapshot"]
        );
        assert_eq!(
            sync.result.kind,
            crate::magician_v2::apps::manifest::AppManifestResultKind::TypedValue
        );
        assert!(sync.result.entities.is_empty());

        // Full authoring conformance on the copy: generated artifacts are
        // written by the same `--write-generated` path the operator flow
        // uses, then stay current, and the provider-free fixture suite
        // passes.
        let checked = check_project(&package_root, true).unwrap();
        assert_eq!(checked.status, "valid");
        assert_eq!(checked.workflow_count, 1);
        assert!(check_project(&package_root, false).is_ok());
        assert_eq!(test_project(&package_root).unwrap().status, "passed");

        // The binding half installation review performs for the compiled
        // platform tool (the 2.5 pattern over the thinking-map substrate):
        // the embedded `thinking_maps_data` pack projects exactly the two
        // scoped reads as dispatch-Ready app actions, the dependency's
        // action subset selects exactly those leaves, and the package lock
        // seals the embedded pack bytes behind `capability:thinking_maps_data`.
        let roots =
            AuthoringDiscoveryRoots::from_explicit(Vec::<PathBuf>::new(), Vec::<PathBuf>::new());
        let snapshot = resolve_authoring_primitive_catalog(&roots);
        assert!(
            snapshot.complete(),
            "the embedded platform catalog scans clean without scoped roots"
        );
        let descriptor = snapshot.resolve("thinking_maps_data").unwrap();
        assert_eq!(
            descriptor.kind(),
            crate::magician_v2::apps::primitive_catalog::AppPrimitiveKind::CompiledTool
        );
        assert_eq!(
            descriptor
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            vec!["list_maps", "read_map"]
        );

        let tool_catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete primitive snapshot");
        let builtin_evidence = dependency_resolution_evidence(&package_root, None, &candidate)
            .expect("builtin contract evidence");
        let evidence = complete_declared_tool_evidence(&candidate, builtin_evidence, &tool_catalog)
            .expect("scoped thinking-map read evidence");
        assert_eq!(evidence.len(), 2);
        let tool_evidence = evidence
            .iter()
            .find(|claim| claim.dependency_ref().as_str() == "capability:thinking_maps_data")
            .expect("the declared thinking-map read tool snapshots into lock evidence");
        // 1.1.0 = the additive keyset-cursor parameter (`after_map_id`) on
        // list_maps; the package's `^1.0` requirement accepts it unchanged.
        assert_eq!(tool_evidence.semantic_version(), "1.1.0");
        let embedded_bytes =
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_yaml(
                "thinking_maps_data",
            )
            .expect("embedded thinking_maps_data pack")
            .as_bytes();
        assert_eq!(
            tool_evidence.content_digest(),
            &AppDigest::blake3(embedded_bytes),
            "the locked bytes are the exact embedded pack definition"
        );
        assert_eq!(
            tool_evidence
                .primitive_binding()
                .expect("the evidence snapshots the reviewed primitive binding")
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            vec!["list_maps", "read_map"],
            "the reviewed binding keeps exactly the two scoped reads"
        );

        let lock =
            lock_app_package_dependencies(&candidate, evidence, &AppPackageLimits::default())
                .expect("immutable package lock");
        assert!(lock
            .capability(&AppReference::parse("capability:thinking_maps_data").unwrap())
            .is_some());
        assert!(!lock.lock_digest().as_str().is_empty());
    }

    /// Repo seed location of the queue-item-5 meetings console, colocated
    /// with the meeting subsystem's system-data home so the console cannot
    /// drift from the rails it renders.
    fn meetings_console_package_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3/system/meetings/app")
    }

    #[test]
    fn first_party_meetings_package_admits_and_locks_the_meetings_read_port() {
        let package_seed = meetings_console_package_root();
        let temporary = tempfile::tempdir().unwrap();
        let package_root = temporary.path().join("meetings");
        copy_package_tree(&package_seed, &package_root);
        let package_root = package_root.canonicalize().unwrap();

        let candidate = admit_package_directory(&package_root).unwrap();
        let manifest = candidate.manifest().manifest();
        assert_eq!(manifest.name.as_str(), "meetings");
        assert!(manifest
            .metadata
            .magician
            .required_features
            .contains(&AppManifestFeature::CustomSurfacesV1));
        assert_eq!(
            manifest.app.permissions,
            vec![crate::magician_v2::apps::manifest::AppManifestPermission::CustomSurface]
        );
        let declaration = manifest
            .app
            .custom_surface
            .as_ref()
            .expect("custom_surface declaration block");
        assert_eq!(declaration.entry_points.len(), 1);
        assert_eq!(declaration.entry_points[0].route.as_str(), "/console");
        assert_eq!(
            declaration.entry_points[0].document.as_str(),
            "surfaces/console.html"
        );

        // Surface authoring admission: HTML entry document plus a separate
        // script member (inline script is forbidden), speaking the reviewed
        // bridge and rendering through DOM construction.
        let console_html = candidate
            .member(&AppBundlePath::parse("surfaces/console.html").unwrap())
            .expect("entry document member");
        assert!(console_html.bytes().starts_with(b"<!DOCTYPE html>"));
        let console_js = candidate
            .member(&AppBundlePath::parse("surfaces/console.js").unwrap())
            .expect("script member");
        let console_js_text = std::str::from_utf8(console_js.bytes()).unwrap();
        assert!(
            console_js_text.contains("magician-surface-bridge"),
            "the script speaks the bridge channel"
        );
        for forbidden in [
            "innerHTML",
            "document.write",
            "eval(",
            "fetch(",
            "XMLHttpRequest",
            "WebSocket",
            "window.open",
        ] {
            assert!(
                !console_js_text.contains(forbidden),
                "`{forbidden}` is outside the reviewed surface posture"
            );
        }

        // The host-read dependency narrows to exactly the six reviewed
        // meeting reads. No control verb is reachable through the binder: the
        // five controls are a separate signed destination, not a tool action.
        let declared_tools = manifest.declared_tools().unwrap();
        assert_eq!(declared_tools.len(), 1);
        assert_eq!(declared_tools[0].name.as_str(), "meetings_data");
        assert_eq!(declared_tools[0].version_requirement, "^1.0");
        assert_eq!(
            declared_tools[0].actions,
            vec![
                "active_session".to_owned(),
                "list_threads".to_owned(),
                "read_thread".to_owned(),
                "read_takeaways".to_owned(),
                "upcoming_meetings".to_owned(),
                "search_meeting_memory".to_owned(),
            ],
            "the dependency narrows to exactly the six scoped meeting reads"
        );
        assert!(declared_tools[0].primitive_ref.is_none());

        // Every control workflow is binder-free by construction and mutates
        // only the local request ledger.
        for verb in ["listen", "join", "pause", "resume", "stop"] {
            let workflow = manifest
                .app
                .workflows
                .get(&AppName::parse(verb).unwrap())
                .unwrap_or_else(|| panic!("{verb} workflow"));
            assert!(
                workflow.uses.is_empty(),
                "`{verb}` records a request; it reaches no tool"
            );
            assert!(workflow.contribution_ports.is_empty());
            assert_eq!(
                workflow
                    .may_mutate
                    .iter()
                    .map(AppName::as_str)
                    .collect::<Vec<_>>(),
                vec!["control_request"],
                "`{verb}` writes only the local control ledger"
            );
            let action = manifest
                .app
                .actions
                .get(&AppName::parse(verb).unwrap())
                .unwrap_or_else(|| panic!("{verb} action"));
            assert_eq!(
                action.workflow.as_str(),
                verb,
                "the signed command verb, its workflow and its action are one name"
            );
        }

        // Every read workflow reaches the binder and nothing else.
        for read in [
            "sync_sessions",
            "sync_threads",
            "read_transcript",
            "sync_takeaways",
            "sync_upcoming",
            "search_meetings",
        ] {
            let workflow = manifest
                .app
                .workflows
                .get(&AppName::parse(read).unwrap())
                .unwrap_or_else(|| panic!("{read} workflow"));
            assert!(matches!(workflow.runner, AppManifestRunner::Recipe));
            assert!(workflow.agent.is_none());
            assert_eq!(
                workflow
                    .uses
                    .iter()
                    .map(AppName::as_str)
                    .collect::<Vec<_>>(),
                vec!["meetings_data"]
            );
            assert!(workflow.contribution_ports.is_empty());
            assert!(
                workflow.procedures.is_empty(),
                "a meetings read reaches the binder and nothing else"
            );
            assert!(workflow.notification_ports.is_empty());
        }

        // Inline script is forbidden by the surface CSP posture: the entry
        // document loads its reviewed script member and carries none of its own.
        let console_html_text = std::str::from_utf8(console_html.bytes()).unwrap();
        assert!(
            console_html_text.contains("<script src=\"console.js\"></script>"),
            "the entry document loads the separate reviewed script member"
        );
        assert_eq!(
            console_html_text.matches("<script").count(),
            1,
            "the entry document carries no inline script"
        );

        // Every field a surface orders on must be indexed, and only a TABLE
        // view's columns index a field. A list view over an entity the console
        // sorts would be refused at query admission, so every view here is a
        // table and every ordered field is one of its columns.
        for (view_name, ordered_field) in [
            ("sessions", "started_seconds_ago"),
            ("threads", "updated_at"),
            ("transcript", "line_at"),
            ("takeaways", "synced_at"),
            ("upcoming", "starts_at"),
            ("search", "synced_at"),
            ("controls", "requested_at"),
            ("receipts", "applied_at"),
        ] {
            let view = manifest
                .app
                .views
                .get(&AppName::parse(view_name).unwrap())
                .unwrap_or_else(|| panic!("{view_name} view"));
            let columns = view.columns.iter().map(AppName::as_str).collect::<Vec<_>>();
            assert!(
                columns.contains(&ordered_field),
                "`{view_name}` must declare `{ordered_field}` as a column for the console to order on it"
            );
        }

        // Surfacing declarations: two bounded native widgets and one state
        // indicator. The indicator is a convenience — capture visibility rests
        // on the host-rendered dot, and a mini-frame widget would be a second
        // live session on the busiest page.
        assert_eq!(manifest.app.widgets.len(), 2);
        for widget in &manifest.app.widgets {
            assert!(matches!(
                widget.rendering,
                crate::magician_v2::apps::manifest::AppManifestWidgetRendering::Native
            ));
        }
        assert_eq!(manifest.app.indicators.len(), 1);
        assert_eq!(manifest.app.navigation.len(), 1);
        assert_eq!(
            manifest.app.navigation[0].route.as_str(),
            "/meetings-console"
        );

        // Full authoring conformance on the copy.
        let checked = check_project(&package_root, true).unwrap();
        assert_eq!(checked.status, "valid");
        assert_eq!(checked.workflow_count, 11);
        assert!(check_project(&package_root, false).is_ok());
        assert_eq!(test_project(&package_root).unwrap().status, "passed");

        // The binding half installation review performs: the embedded
        // `meetings_data` pack projects exactly the six reads as dispatch-Ready
        // app actions, and the lock seals the embedded bytes.
        let roots =
            AuthoringDiscoveryRoots::from_explicit(Vec::<PathBuf>::new(), Vec::<PathBuf>::new());
        let snapshot = resolve_authoring_primitive_catalog(&roots);
        assert!(snapshot.complete());
        let descriptor = snapshot.resolve("meetings_data").unwrap();
        assert_eq!(
            descriptor.kind(),
            crate::magician_v2::apps::primitive_catalog::AppPrimitiveKind::CompiledTool
        );
        assert_eq!(
            descriptor
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            // The primitive catalog sorts a generic compiled pack's actions
            // alphabetically; declaration order is not preserved.
            vec![
                "active_session",
                "list_threads",
                "read_takeaways",
                "read_thread",
                "search_meeting_memory",
                "upcoming_meetings",
            ]
        );

        let tool_catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete primitive snapshot");
        let builtin_evidence = dependency_resolution_evidence(&package_root, None, &candidate)
            .expect("builtin contract evidence");
        let evidence = complete_declared_tool_evidence(&candidate, builtin_evidence, &tool_catalog)
            .expect("scoped meeting read evidence");
        let tool_evidence = evidence
            .iter()
            .find(|claim| claim.dependency_ref().as_str() == "capability:meetings_data")
            .expect("the declared meetings read tool snapshots into lock evidence");
        let embedded_bytes =
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_yaml(
                "meetings_data",
            )
            .expect("embedded meetings_data pack")
            .as_bytes();
        assert_eq!(
            tool_evidence.content_digest(),
            &AppDigest::blake3(embedded_bytes),
            "the locked bytes are the exact embedded pack definition"
        );
        assert_eq!(
            tool_evidence
                .primitive_binding()
                .expect("the evidence snapshots the reviewed primitive binding")
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            // `select_actions` filters the catalog list in place, so the
            // binding is alphabetical for the same reason.
            vec![
                "active_session",
                "list_threads",
                "read_takeaways",
                "read_thread",
                "search_meeting_memory",
                "upcoming_meetings",
            ],
            "the reviewed binding keeps exactly the six scoped reads"
        );

        let lock =
            lock_app_package_dependencies(&candidate, evidence, &AppPackageLimits::default())
                .expect("immutable package lock");
        assert!(lock
            .capability(&AppReference::parse("capability:meetings_data").unwrap())
            .is_some());
        assert!(!lock.lock_digest().as_str().is_empty());
    }

    /// Repo seed location of the queue-item-6 Town Square package, colocated
    /// with the system-data home like its three siblings.
    fn town_square_package_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician_data_v3/system/town_square/app")
    }

    /// This package is the FIRST manifest in the repo to declare behaviors,
    /// app LLM operations and an owner-notification port, so admitting it is
    /// also the first executable proof of that vocabulary — not just a check
    /// on one package.
    #[test]
    fn town_square_package_is_the_first_admitted_behavior_and_notify_manifest() {
        let package_seed = town_square_package_root();
        let temporary = tempfile::tempdir().unwrap();
        let package_root = temporary.path().join("town-square");
        copy_package_tree(&package_seed, &package_root);
        let package_root = package_root.canonicalize().unwrap();

        let candidate = admit_package_directory(&package_root).unwrap();
        let manifest = candidate.manifest().manifest();
        assert_eq!(manifest.name.as_str(), "town-square");
        assert_eq!(
            manifest.app.distribution,
            crate::magician_v2::apps::manifest::AppManifestDistribution::System
        );

        // The three vocabularies this package proves, each gated on its own
        // feature and each inert without a grant.
        for feature in [
            AppManifestFeature::AppBehaviorsV1,
            AppManifestFeature::LlmOperationsV1,
            AppManifestFeature::AppOwnerNotificationsV1,
        ] {
            assert!(
                manifest
                    .metadata
                    .magician
                    .required_features
                    .contains(&feature),
                "{feature:?} must be declared for the block that needs it"
            );
        }

        // One scheduled contextual round with one reviewed semantic operation;
        // the native recipe enforces per-participant and aggregate ceilings.
        assert_eq!(manifest.app.behaviors.len(), 1);
        let behavior = &manifest.app.behaviors[0];
        assert_eq!(behavior.id.as_str(), "ambient_turn");
        assert_eq!(behavior.input.entity.as_str(), "turn_cursor");
        assert_eq!(behavior.input.record_id.as_str(), "singleton");
        assert_eq!(
            behavior
                .operations
                .iter()
                .map(AppName::as_str)
                .collect::<Vec<_>>(),
            vec!["compose_post"]
        );
        // The composition envelope is structured output, not prose. It now
        // lives on the recipe STEPS rather than on the behavior: the ordered
        // operation contract made the two mutually exclusive, because a
        // behavior-level schema alongside per-step ones is a second source of
        // truth that drifts from the step actually producing the bytes.
        assert!(
            behavior.output_schema.is_none(),
            "steps own their schemas; a behavior-level one would be a second source of truth"
        );
        assert!(
            !behavior.steps.is_empty()
                && behavior
                    .steps
                    .iter()
                    .all(|step| step.output_schema.value_schema.is_some()),
            "every reviewed step declares the structured output it must produce"
        );
        let action = manifest
            .app
            .actions
            .get(&behavior.action)
            .expect("behavior action");
        let workflow = manifest
            .app
            .workflows
            .get(&action.workflow)
            .expect("behavior workflow");
        assert_eq!(
            workflow.trigger,
            crate::magician_v2::apps::manifest::AppManifestTrigger::Schedule
        );
        assert!(matches!(workflow.runner, AppManifestRunner::Recipe));
        assert!(
            manifest.app.resources.behaviors.contains_key(&behavior.id),
            "a behavior without its exact resource ceiling is inadmissible"
        );

        // Every semantic operation is declared.
        assert_eq!(manifest.app.llm_operations.len(), 1);
        for operation in &behavior.operations {
            assert!(manifest.app.llm_operations.contains_key(operation));
        }

        // One one-way briefing port. V1 has no questions and no response
        // authority, so the ceiling is the vocabulary, not a preference.
        assert_eq!(workflow.notification_ports.len(), 1);
        let port = workflow
            .notification_ports
            .get(&AppName::parse("owner_mentioned").unwrap())
            .expect("owner_mentioned port");
        assert_eq!(
            port.kind,
            crate::magician_v2::apps::records::AppNotificationKindV1::Briefing
        );
        assert_eq!(
            port.severity_ceiling,
            crate::magician_v2::apps::records::AppNotificationSeverityV1::Info
        );

        // The corpus really is the package's: every table the retired SQLite
        // store owned has a typed entity here, and the two singletons the
        // behavior needs exist.
        for entity in [
            "member",
            "self_state",
            "post",
            "reaction",
            "group",
            "group_membership",
            "mention",
            "policy",
            "turn_cursor",
        ] {
            assert!(
                manifest
                    .app
                    .entities
                    .contains_key(&AppName::parse(entity).unwrap()),
                "`{entity}` must be owned by the package after the corpus migration"
            );
        }

        // The roster binder is the only tool dependency, and it is narrowed to
        // the ONE action a prompt actually calls. A declared action nothing
        // calls is a grant, not documentation. Membership reconciliation is the
        // only reason a package that owns its corpus still needs a host read.
        let declared_tools = manifest.declared_tools().unwrap();
        assert_eq!(declared_tools.len(), 1);
        assert_eq!(declared_tools[0].name.as_str(), "agent_roster_data");
        assert_eq!(declared_tools[0].actions, vec!["list_members".to_owned()]);

        // The behavior's source record must have a writer that is NOT the
        // behavior. A behavior cannot bootstrap its own input: the scheduler
        // fails to resolve the record, backs off, and never reaches the step
        // that would have created it — so the square would be silent forever on
        // every fresh installation. Same argument for the policy singleton the
        // gate reads.
        for singleton_owner in ["turn_cursor", "self_state"] {
            let entity = AppName::parse(singleton_owner).unwrap();
            let writers = manifest
                .app
                .workflows
                .iter()
                .filter(|(_, workflow)| workflow.may_mutate.contains(&entity))
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>();
            assert!(
                writers.iter().any(|name| *name != "take_ambient_turn"),
                "`{singleton_owner}` needs a writer other than the behavior that reads it, \
                 found {writers:?}"
            );
        }

        // Cadence and spending are independent ceilings. A round need not
        // consume its maximum; monthly exhaustion is enforced by the owner.
        // Keep the reviewed amounts explicit instead of treating every allowed
        // start as a promise to spend a full round's budget.
        let ceiling = &manifest.app.resources.behaviors[&behavior.id];
        assert_eq!(ceiling.max_starts_per_period, 12);
        assert_eq!(ceiling.period_seconds, 3600);
        assert_eq!(ceiling.per_run.max_tokens, 2_097_152);
        assert_eq!(ceiling.per_run.max_cost_usd.microusd(), 4_000_000);
        assert_eq!(ceiling.monthly.max_tokens, 134_217_728);
        assert_eq!(ceiling.monthly.max_cost_usd.microusd(), 500_000_000);
        assert!(ceiling.per_run.max_tokens <= ceiling.monthly.max_tokens);
        assert!(ceiling.per_run.max_cost_usd.microusd() <= ceiling.monthly.max_cost_usd.microusd());
        assert!(ceiling.monthly.max_tokens <= manifest.app.resources.monthly.max_tokens);
        assert!(
            ceiling.monthly.max_cost_usd.microusd()
                <= manifest.app.resources.monthly.max_cost_usd.microusd()
        );

        // The autonomy switch is one of the three independent offs, so the only
        // thing that may write it is the operator's own explicit action. A
        // reconciler that runs unattended must never be able to turn it on.
        let policy = AppName::parse("policy").unwrap();
        let policy_writers = manifest
            .app
            .workflows
            .iter()
            .filter(|(_, workflow)| workflow.may_mutate.contains(&policy))
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            policy_writers,
            vec!["set_policy"],
            "only the operator's explicit policy action may write `autonomy_state`"
        );

        // Surface posture, same class as the other seed packages.
        let square_js = candidate
            .member(&AppBundlePath::parse("surfaces/square.js").unwrap())
            .expect("script member");
        let square_js_text = std::str::from_utf8(square_js.bytes()).unwrap();
        assert!(square_js_text.contains("magician-surface-bridge"));
        for forbidden in [
            "innerHTML",
            "document.write",
            "eval(",
            "fetch(",
            "XMLHttpRequest",
            "WebSocket",
            "window.open",
        ] {
            assert!(
                !square_js_text.contains(forbidden),
                "`{forbidden}` is outside the reviewed surface posture"
            );
        }

        // Every field the surface orders on must be a table view's column, or
        // the read is refused at query admission.
        for (view_name, ordered_field) in [
            ("feed", "created_at"),
            ("members", "synced_at"),
            ("groups", "created_at"),
            ("mentions", "created_at"),
            ("policy", "updated_at"),
        ] {
            let view = manifest
                .app
                .views
                .get(&AppName::parse(view_name).unwrap())
                .unwrap_or_else(|| panic!("{view_name} view"));
            assert!(
                view.columns
                    .iter()
                    .map(AppName::as_str)
                    .any(|column| column == ordered_field),
                "`{view_name}` must declare `{ordered_field}` as a column"
            );
        }

        let checked = check_project(&package_root, true).unwrap();
        assert_eq!(checked.status, "valid");
        assert_eq!(checked.workflow_count, 7);
        assert!(check_project(&package_root, false).is_ok());
        assert_eq!(test_project(&package_root).unwrap().status, "passed");

        // The binding half installation review performs, same as the four
        // sibling binders: the embedded `agent_roster_data` pack projects its
        // reads as dispatch-Ready app actions, and the lock seals the exact
        // embedded bytes.
        let roots =
            AuthoringDiscoveryRoots::from_explicit(Vec::<PathBuf>::new(), Vec::<PathBuf>::new());
        let snapshot = resolve_authoring_primitive_catalog(&roots);
        assert!(snapshot.complete());
        let descriptor = snapshot.resolve("agent_roster_data").unwrap();
        assert_eq!(
            descriptor.kind(),
            crate::magician_v2::apps::primitive_catalog::AppPrimitiveKind::CompiledTool
        );
        assert_eq!(
            descriptor
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            // Alphabetical, as the catalog sorts a generic compiled pack.
            vec!["list_members", "read_member"],
            "the binder itself keeps both reads even though this package binds one"
        );

        let tool_catalog = AppReviewedToolCatalog::from_primitive_snapshot(&snapshot)
            .expect("complete primitive snapshot");
        let builtin_evidence = dependency_resolution_evidence(&package_root, None, &candidate)
            .expect("builtin contract evidence");
        let evidence = complete_declared_tool_evidence(&candidate, builtin_evidence, &tool_catalog)
            .expect("scoped roster read evidence");
        let tool_evidence = evidence
            .iter()
            .find(|claim| claim.dependency_ref().as_str() == "capability:agent_roster_data")
            .expect("the declared roster read tool snapshots into lock evidence");
        let embedded_bytes =
            crate::magician_v2::execution::compiled_providers::embedded_compiled_pack_yaml(
                "agent_roster_data",
            )
            .expect("embedded agent_roster_data pack")
            .as_bytes();
        assert_eq!(
            tool_evidence.content_digest(),
            &AppDigest::blake3(embedded_bytes),
            "the locked bytes are the exact embedded pack definition"
        );
        assert_eq!(
            tool_evidence
                .primitive_binding()
                .expect("the evidence snapshots the reviewed primitive binding")
                .actions()
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            // `select_actions` filters the catalog list in place, so the
            // narrowing to the single declared action shows up here.
            vec!["list_members"],
            "the reviewed binding narrows to the one action the package declares"
        );

        let lock =
            lock_app_package_dependencies(&candidate, evidence, &AppPackageLimits::default())
                .expect("immutable package lock");
        assert!(lock
            .capability(&AppReference::parse("capability:agent_roster_data").unwrap())
            .is_some());
        assert!(!lock.lock_digest().as_str().is_empty());
    }

    #[test]
    fn brainstorm_canvas_package_home_stays_outside_authoring_discovery() {
        // `magician_data_v3/system/thinking_map` sits outside every
        // agent-template and skill discovery root, so the colocated reference
        // package is invisible to both scanners by construction; this pins
        // that the new system-data directory does not leak into authoring
        // discovery as a side effect.
        let templates_root = harness_sre_template_root()
            .ancestors()
            .nth(2)
            .map(Path::to_path_buf)
            .expect("the harness-sre template sits under agent_templates");
        let roots = AuthoringDiscoveryRoots::from_explicit(
            Vec::<PathBuf>::new(),
            [templates_root.canonicalize().unwrap()],
        );
        let listed = list_authoring_agents(&roots);
        assert!(
            listed
                .items
                .iter()
                .all(|entry| entry.name != "brainstorm-canvas"),
            "the reference package is not an agent entry"
        );

        // The skill-side scan runs the same system-data home as an explicit
        // skill root: the `skill_type: app` guard keeps the colocated
        // package out of the primitive catalog, so no `brainstorm-canvas`
        // primitive can appear from it.
        let thinking_map_home = brainstorm_canvas_package_root()
            .parent()
            .expect("the reference package sits inside the thinking-map system-data home")
            .canonicalize()
            .unwrap();
        let skill_roots =
            AuthoringDiscoveryRoots::from_explicit([thinking_map_home], Vec::<PathBuf>::new());
        let snapshot = resolve_authoring_primitive_catalog(&skill_roots);
        assert!(
            snapshot.complete(),
            "the thinking-map system-data home scans clean as a skill root"
        );
        assert!(
            snapshot
                .descriptors()
                .iter()
                .all(|descriptor| descriptor.name() != "brainstorm-canvas"),
            "no primitive is projected from the reference package"
        );
    }
}
