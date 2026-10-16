//! Scheduler occurrence indexes. These records are not Artifact tasks.
use super::*;

pub(super) fn current_binding_control_id(
    connection: &rusqlite::Connection,
    task_id: &str,
    execution_id: &str,
    kind: AppWorkflowControlKind,
) -> Result<String, AppRegistryError> {
    if kind != AppWorkflowControlKind::TaskBinding || task_id != execution_id {
        return Ok(execution_id.to_owned());
    }
    Ok(connection
        .query_row(
            "SELECT occurrence_id FROM app_recurring_task_heads WHERE task_id = ?1",
            [task_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| execution_id.to_owned()))
}

impl AppRegistryService {
    pub(crate) async fn recurring_locator(
        &self,
        scope: AppScope,
        identity: String,
        latest_for_task: bool,
    ) -> Result<Option<(String, String, String, Vec<u8>)>, AppRegistryError> {
        validate_workflow_control_identity(&identity, &identity, b"x")?;
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _guard = write_guard;
            let _permit = permit;
            let connection = open_scoped_registry_for_write(&workspace, &scope)?;
            let predicate = if latest_for_task {
                "task_id = ?1 AND occurrence_id = (SELECT occurrence_id FROM app_recurring_task_heads WHERE task_id = ?1)"
            } else { "occurrence_id = ?1" };
            let query = format!("SELECT task_id, occurrence_id, execution_id, sealed_blob FROM app_recurring_occurrence_locators WHERE {predicate} AND length(sealed_blob) <= ?2");
            Ok(connection.query_row(&query, params![identity, MAX_WORKFLOW_CONTROL_BLOB_BYTES as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?)
        }).await.map_err(|e| AppRegistryError::WorkerTerminated(e.to_string()))?
    }

    pub(crate) async fn publish_recurring_locator(
        &self,
        scope: AppScope,
        task_id: String,
        occurrence_id: String,
        execution_id: String,
        sealed_blob: Vec<u8>,
    ) -> Result<(), AppRegistryError> {
        validate_workflow_control_identity(&task_id, &execution_id, &sealed_blob)?;
        let (permit, write_guard) = self.acquire_write_admission(&scope).await?;
        let workspace = self.workspace.clone();
        spawn_registry_work(move || {
            let _guard = write_guard;
            let _permit = permit;
            let mut connection = open_scoped_registry_for_write(&workspace, &scope)?;
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let existing: Option<(String, String, Vec<u8>)> = transaction.query_row(
                "SELECT task_id, execution_id, sealed_blob FROM app_recurring_occurrence_locators WHERE occurrence_id = ?1 AND length(sealed_blob) <= ?2",
                params![&occurrence_id, MAX_WORKFLOW_CONTROL_BLOB_BYTES as i64], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).optional()?;
            if let Some(existing) = existing {
                if existing != (task_id.clone(), execution_id.clone(), sealed_blob.clone()) {
                    return Err(AppRegistryError::StateConflict("recurring occurrence identity conflict".into()));
                }
                // Exact replay must never move the current head backwards.
                transaction.commit()?;
                return Ok(());
            } else {
                transaction.execute("INSERT INTO app_recurring_occurrence_locators (occurrence_id, task_id, execution_id, sealed_blob) VALUES (?1, ?2, ?3, ?4)",
                    params![occurrence_id, task_id, execution_id, sealed_blob])?;
            }
            transaction.execute("INSERT INTO app_recurring_task_heads (task_id, occurrence_id) VALUES (?1, ?2) ON CONFLICT(task_id) DO UPDATE SET occurrence_id = excluded.occurrence_id",
                params![task_id, occurrence_id])?;
            transaction.commit()?;
            Ok(())
        }).await.map_err(|e| AppRegistryError::WorkerTerminated(e.to_string()))?
    }
}
