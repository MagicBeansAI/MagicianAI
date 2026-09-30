//! Operator workflow over the dormant migration coordinator.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use magician_storage::{MigrationPhase, StorageCatalogId, StorageScope};
use magician_storage_migration::{
    MigrationCoordinator, OwnerMigrationHandler, OwnerMigrationRegistry, PlanRequest,
};
use serde_json::json;
use uuid::Uuid;

use super::preconditions::{evaluate_preconditions, ActivationContext, PreconditionReport};
use super::report::{ActivationOperation, ActivationReport};
use crate::magician_v2::track_a_acceptance::load_catalog;

pub const CUTOVER_CONFIRM: &str = "CUT OVER STORAGE";
pub const ROLLBACK_CONFIRM: &str = "ROLLBACK STORAGE";

pub struct OperatorWorkflow {
    coordinator: MigrationCoordinator,
    ctx: ActivationContext,
}

impl OperatorWorkflow {
    pub async fn open(
        ledger_root: impl AsRef<Path>,
        registry: OwnerMigrationRegistry,
        ctx: ActivationContext,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            coordinator: MigrationCoordinator::open(ledger_root, registry).await?,
            ctx,
        })
    }

    pub async fn with_handler(
        ledger_root: impl AsRef<Path>,
        handler: Arc<dyn OwnerMigrationHandler>,
        ctx: ActivationContext,
    ) -> anyhow::Result<Self> {
        let mut registry = OwnerMigrationRegistry::new();
        registry.register(handler)?;
        Self::open(ledger_root, registry, ctx).await
    }

    pub fn preconditions(&self) -> anyhow::Result<PreconditionReport> {
        evaluate_preconditions(&self.ctx)
    }

    pub fn inventory(&self, principal: &str, workspace: &str) -> anyhow::Result<ActivationReport> {
        let pre = self.preconditions()?;
        let catalog = load_catalog()?;
        let owners: Vec<_> = catalog
            .owners
            .iter()
            .map(|owner| {
                json!({
                    "id": owner.id,
                    "tier": owner.tier_label(),
                    "class": owner.class,
                    "readiness": owner.readiness.state,
                    "authority": owner.authority.state,
                })
            })
            .collect();
        let mut report = ActivationReport::from_preconditions(
            ActivationOperation::Inventory,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        report.ok = true;
        report.detail = json!({
            "owner_count": owners.len(),
            "owners": owners,
        });
        Ok(report.redact())
    }

    pub async fn status(
        &self,
        principal: &str,
        workspace: &str,
        migration_id: Option<Uuid>,
    ) -> anyhow::Result<ActivationReport> {
        let pre = self.preconditions()?;
        let mut report = ActivationReport::from_preconditions(
            ActivationOperation::Status,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        report.ok = true;
        if let Some(id) = migration_id {
            let entry = self.coordinator.status(id).await?;
            report.migration_id = Some(id.to_string());
            report.phase = Some(entry.record.phase.as_str().into());
            report.detail = json!({
                "fence_generation": entry.fence_generation,
                "source_generation": entry.record.source_generation,
            });
        }
        Ok(report.redact())
    }

    pub async fn plan(
        &self,
        owner_id: &str,
        scope: StorageScope,
        dry_run: bool,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        let pre = self.preconditions()?;
        let mut report = ActivationReport::from_preconditions(
            ActivationOperation::Plan,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        if !pre.tier1_remote_ready || !pre.source_target_ok {
            report.ok = false;
            return Ok(report.redact());
        }
        let catalog = load_catalog()?;
        if let Some(owner) = catalog.owners.iter().find(|row| row.id == owner_id) {
            if owner.is_device_local() {
                report.ok = false;
                report.blocking.push("device_local_not_migrated".into());
                return Ok(report.redact());
            }
        }
        if dry_run {
            report.ok = true;
            report.dry_run = true;
            report.detail = json!({
                "owner": owner_id,
                "note": "dry-run does not write a ledger or copy bytes",
            });
            return Ok(report.redact());
        }
        let planned = self
            .coordinator
            .plan(PlanRequest {
                store_id: StorageCatalogId::parse(owner_id)?,
                scope,
                source_profile: self.ctx.source_profile.clone(),
                target_profile: self.ctx.target_profile.clone(),
            })
            .await?;
        report.ok = true;
        report.migration_id = Some(planned.migration_id.to_string());
        report.phase = Some(planned.phase.as_str().into());
        report.detail = json!({ "source_generation": planned.source_generation });
        Ok(report.redact())
    }

    pub async fn export(
        &self,
        migration_id: Uuid,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        self.advance(
            ActivationOperation::Export,
            migration_id,
            principal,
            workspace,
            |coord, id| Box::pin(async move { coord.export(id).await.map_err(Into::into) }),
        )
        .await
    }

    pub async fn import(
        &self,
        migration_id: Uuid,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        self.advance(
            ActivationOperation::Import,
            migration_id,
            principal,
            workspace,
            |coord, id| Box::pin(async move { coord.import(id).await.map_err(Into::into) }),
        )
        .await
    }

    pub async fn verify(
        &self,
        migration_id: Uuid,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        self.advance(
            ActivationOperation::Verify,
            migration_id,
            principal,
            workspace,
            |coord, id| {
                Box::pin(async move { coord.semantic_verify(id).await.map_err(Into::into) })
            },
        )
        .await
    }

    pub async fn checkpoint(
        &self,
        migration_id: Uuid,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        self.advance(
            ActivationOperation::Checkpoint,
            migration_id,
            principal,
            workspace,
            |coord, id| Box::pin(async move { coord.checkpoint(id).await.map_err(Into::into) }),
        )
        .await
    }

    pub async fn resume(
        &self,
        migration_id: Uuid,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        let pre = self.preconditions()?;
        let mut report = ActivationReport::from_preconditions(
            ActivationOperation::Resume,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        let status = self.coordinator.resume(migration_id).await?;
        report.ok = true;
        report.migration_id = Some(migration_id.to_string());
        report.phase = Some(status.record.phase.as_str().into());
        report.detail = json!({
            "needs_retry": status.needs_retry,
            "fence_generation": status.fence_generation,
        });
        Ok(report.redact())
    }

    pub async fn cancel(
        &self,
        migration_id: Uuid,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        let pre = self.preconditions()?;
        let mut report = ActivationReport::from_preconditions(
            ActivationOperation::Cancel,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        let entry = self.coordinator.status(migration_id).await?;
        if matches!(
            entry.record.phase,
            MigrationPhase::Cutover | MigrationPhase::Settled
        ) {
            report.ok = false;
            report.blocking.push("already_cut_over".into());
            report.local_canonical = false;
            return Ok(report.redact());
        }
        report.ok = true;
        report.local_canonical = true;
        report.migration_id = Some(migration_id.to_string());
        report.phase = Some(entry.record.phase.as_str().into());
        report.detail = json!({ "cancelled": true, "local_canonical": true });
        Ok(report.redact())
    }

    pub async fn cutover(
        &self,
        migration_id: Uuid,
        fence: u64,
        confirmation: &str,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        let pre = self.preconditions()?;
        let mut report = ActivationReport::from_preconditions(
            ActivationOperation::Cutover,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        if confirmation != CUTOVER_CONFIRM {
            report.ok = false;
            report.blocking.push("confirmation_mismatch".into());
            return Ok(report.redact());
        }
        if !pre.cutover_allowed() {
            report.ok = false;
            report.local_canonical = true;
            return Ok(report.redact());
        }
        match self.coordinator.cutover(migration_id, fence).await {
            Ok(record) => {
                report.ok = true;
                report.local_canonical = false;
                report.migration_id = Some(record.migration_id.to_string());
                report.phase = Some(record.phase.as_str().into());
            },
            Err(error) => {
                report.ok = false;
                report.local_canonical = true;
                report.detail = json!({ "error": error.to_string() });
            },
        }
        Ok(report.redact())
    }

    pub async fn rollback(
        &self,
        migration_id: Uuid,
        fence: u64,
        confirmation: &str,
        principal: &str,
        workspace: &str,
    ) -> anyhow::Result<ActivationReport> {
        let pre = self.preconditions()?;
        let mut report = ActivationReport::from_preconditions(
            ActivationOperation::Rollback,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        if confirmation != ROLLBACK_CONFIRM {
            report.ok = false;
            report.blocking.push("confirmation_mismatch".into());
            return Ok(report.redact());
        }
        if !pre.cutover_allowed() {
            report.ok = false;
            return Ok(report.redact());
        }
        match self.coordinator.rollback(migration_id, fence).await {
            Ok(record) => {
                report.ok = true;
                report.local_canonical = true;
                report.migration_id = Some(record.migration_id.to_string());
                report.phase = Some(record.phase.as_str().into());
            },
            Err(error) => {
                report.ok = false;
                report.local_canonical = true;
                report.detail = json!({ "error": error.to_string() });
            },
        }
        Ok(report.redact())
    }

    async fn advance<F>(
        &self,
        operation: ActivationOperation,
        migration_id: Uuid,
        principal: &str,
        workspace: &str,
        step: F,
    ) -> anyhow::Result<ActivationReport>
    where
        F: for<'a> FnOnce(
            &'a MigrationCoordinator,
            Uuid,
        ) -> Pin<
            Box<
                dyn Future<Output = anyhow::Result<magician_storage::StorageMigrationRecord>>
                    + Send
                    + 'a,
            >,
        >,
    {
        let pre = self.preconditions()?;
        let mut report = ActivationReport::from_preconditions(
            operation,
            principal,
            workspace,
            &pre,
            &self.ctx.source_profile,
            &self.ctx.target_profile,
        );
        match step(&self.coordinator, migration_id).await {
            Ok(record) => {
                report.ok = true;
                report.migration_id = Some(record.migration_id.to_string());
                report.phase = Some(record.phase.as_str().into());
                report.local_canonical = record.phase != MigrationPhase::Cutover
                    && record.phase != MigrationPhase::Settled;
            },
            Err(error) => {
                report.ok = false;
                report.local_canonical = true;
                report.detail = json!({ "error": error.to_string() });
            },
        }
        Ok(report.redact())
    }
}
