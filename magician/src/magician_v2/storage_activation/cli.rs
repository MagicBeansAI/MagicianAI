//! `magician storage inventory|plan|export|import|verify|cutover|rollback|status`

use std::path::PathBuf;

use clap::{Args, Subcommand};
use magician_storage::{PrincipalId, ScopeId, StorageScope, WorkspaceId};
use magician_storage_migration::{OwnerMigrationRegistry, SyntheticOwner, SYNTHETIC_OWNER_ID};
use uuid::Uuid;

use super::preconditions::ActivationContext;
use super::report::ActivationReport;
use super::workflow::OperatorWorkflow;
use super::{CUTOVER_CONFIRM, ROLLBACK_CONFIRM};

#[derive(Debug, Clone, Args)]
pub struct StorageScopeArgs {
    #[arg(long, default_value = "anonymous")]
    pub principal: String,
    #[arg(long, default_value = "default")]
    pub workspace: String,
    #[arg(long, default_value = "local_embedded")]
    pub source: String,
    #[arg(long, default_value = "remote_durable")]
    pub target: String,
    /// Ledger root. Defaults to a temp-ineligible operator path under cwd.
    #[arg(long)]
    pub ledger: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct PlanArgs {
    #[command(flatten)]
    pub scope: StorageScopeArgs,
    #[arg(long)]
    pub owner: String,
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Args)]
pub struct MigrationArgs {
    #[command(flatten)]
    pub scope: StorageScopeArgs,
    #[arg(long)]
    pub migration_id: Uuid,
}

#[derive(Debug, Clone, Args)]
pub struct CutoverArgs {
    #[command(flatten)]
    pub migration: MigrationArgs,
    #[arg(long)]
    pub fence: u64,
    #[arg(long)]
    pub confirm: String,
}

#[derive(Debug, Clone, Subcommand)]
pub enum StorageCommand {
    Inventory(StorageScopeArgs),
    Status {
        #[command(flatten)]
        scope: StorageScopeArgs,
        #[arg(long)]
        migration_id: Option<Uuid>,
    },
    Plan(PlanArgs),
    Export(MigrationArgs),
    Import(MigrationArgs),
    Verify(MigrationArgs),
    Checkpoint(MigrationArgs),
    Resume(MigrationArgs),
    Cancel(MigrationArgs),
    Cutover(CutoverArgs),
    Rollback(CutoverArgs),
}

pub async fn run_storage_command(command: StorageCommand) -> anyhow::Result<ActivationReport> {
    match command {
        StorageCommand::Inventory(scope) => {
            let flow = open_flow(&scope).await?;
            flow.inventory(&scope.principal, &scope.workspace)
        },
        StorageCommand::Status {
            scope,
            migration_id,
        } => {
            let flow = open_flow(&scope).await?;
            flow.status(&scope.principal, &scope.workspace, migration_id)
                .await
        },
        StorageCommand::Plan(args) => {
            let flow = open_flow(&args.scope).await?;
            flow.plan(
                &args.owner,
                tenant(&args.scope)?,
                args.dry_run,
                &args.scope.principal,
                &args.scope.workspace,
            )
            .await
        },
        StorageCommand::Export(args) => {
            let flow = open_flow(&args.scope).await?;
            flow.export(
                args.migration_id,
                &args.scope.principal,
                &args.scope.workspace,
            )
            .await
        },
        StorageCommand::Import(args) => {
            let flow = open_flow(&args.scope).await?;
            flow.import(
                args.migration_id,
                &args.scope.principal,
                &args.scope.workspace,
            )
            .await
        },
        StorageCommand::Verify(args) => {
            let flow = open_flow(&args.scope).await?;
            flow.verify(
                args.migration_id,
                &args.scope.principal,
                &args.scope.workspace,
            )
            .await
        },
        StorageCommand::Checkpoint(args) => {
            let flow = open_flow(&args.scope).await?;
            flow.checkpoint(
                args.migration_id,
                &args.scope.principal,
                &args.scope.workspace,
            )
            .await
        },
        StorageCommand::Resume(args) => {
            let flow = open_flow(&args.scope).await?;
            flow.resume(
                args.migration_id,
                &args.scope.principal,
                &args.scope.workspace,
            )
            .await
        },
        StorageCommand::Cancel(args) => {
            let flow = open_flow(&args.scope).await?;
            flow.cancel(
                args.migration_id,
                &args.scope.principal,
                &args.scope.workspace,
            )
            .await
        },
        StorageCommand::Cutover(args) => {
            let flow = open_flow(&args.migration.scope).await?;
            let _ = CUTOVER_CONFIRM;
            flow.cutover(
                args.migration.migration_id,
                args.fence,
                &args.confirm,
                &args.migration.scope.principal,
                &args.migration.scope.workspace,
            )
            .await
        },
        StorageCommand::Rollback(args) => {
            let flow = open_flow(&args.migration.scope).await?;
            let _ = ROLLBACK_CONFIRM;
            flow.rollback(
                args.migration.migration_id,
                args.fence,
                &args.confirm,
                &args.migration.scope.principal,
                &args.migration.scope.workspace,
            )
            .await
        },
    }
}

fn tenant(scope: &StorageScopeArgs) -> anyhow::Result<StorageScope> {
    Ok(StorageScope::Tenant(ScopeId::new(
        PrincipalId::parse(&scope.principal)?,
        WorkspaceId::parse(&scope.workspace)?,
    )))
}

fn ctx(scope: &StorageScopeArgs) -> ActivationContext {
    ActivationContext {
        source_profile: scope.source.clone(),
        target_profile: scope.target.clone(),
        ..ActivationContext::default()
    }
}

async fn open_flow(scope: &StorageScopeArgs) -> anyhow::Result<OperatorWorkflow> {
    let ledger = scope
        .ledger
        .clone()
        .unwrap_or_else(|| PathBuf::from(".magician-storage"));
    let mut registry = OwnerMigrationRegistry::new();
    let synthetic = std::sync::Arc::new(SyntheticOwner::new()?);
    let handler: std::sync::Arc<dyn magician_storage_migration::OwnerMigrationHandler> = synthetic;
    let _ = SYNTHETIC_OWNER_ID;
    registry.register(handler)?;
    OperatorWorkflow::open(ledger, registry, ctx(scope)).await
}
