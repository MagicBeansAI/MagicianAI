use magician_storage::StorageError;
use serde::Serialize;

use crate::store::{Gate1Store, TaskRow};

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioReport {
    pub backend: String,
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

pub async fn run_matrix(store: &dyn Gate1Store) -> Vec<ScenarioReport> {
    let mut reports = Vec::new();
    store.migrate().await.expect("migrate");
    push(
        &mut reports,
        store,
        "task_cas_and_idempotency",
        task_cas(store).await,
    );
    push(
        &mut reports,
        store,
        "chat_session_and_page",
        chat_page(store).await,
    );
    push(
        &mut reports,
        store,
        "attention_retention_query",
        attention(store).await,
    );
    push(
        &mut reports,
        store,
        "outbox_claim_and_lease_fence",
        outbox_and_lease(store).await,
    );
    push(
        &mut reports,
        store,
        "scope_isolation",
        isolation(store).await,
    );
    push(
        &mut reports,
        store,
        "cursor_order",
        cursor_order(store).await,
    );
    push(
        &mut reports,
        store,
        "schema_migration",
        migration(store).await,
    );
    push(
        &mut reports,
        store,
        "uncommitted_drop_rolls_back",
        uncommitted(store).await,
    );
    push(
        &mut reports,
        store,
        "committed_survives_reconnect_shape",
        committed(store).await,
    );
    push(&mut reports, store, "backup_restore", backup(store).await);
    reports
}

fn push(
    reports: &mut Vec<ScenarioReport>,
    store: &dyn Gate1Store,
    name: &str,
    result: Result<(), String>,
) {
    match result {
        Ok(()) => reports.push(ScenarioReport {
            backend: store.name().into(),
            name: name.into(),
            passed: true,
            detail: "ok".into(),
        }),
        Err(detail) => reports.push(ScenarioReport {
            backend: store.name().into(),
            name: name.into(),
            passed: false,
            detail,
        }),
    }
}

async fn task_cas(store: &dyn Gate1Store) -> Result<(), String> {
    let task = TaskRow {
        principal: "alice".into(),
        workspace: "home".into(),
        task_id: "t1".into(),
        revision: 1,
        idempotency_key: "idemp-1".into(),
        payload: "create".into(),
    };
    let first = store.create_task(&task).await.map_err(err)?;
    let again = store
        .create_task(&TaskRow {
            payload: "ignored".into(),
            ..task.clone()
        })
        .await
        .map_err(err)?;
    if first.payload != "create" || again.task_id != first.task_id || again.revision != 1 {
        return Err("idempotency did not return the original task".into());
    }
    let updated = store
        .cas_task("alice", "home", "t1", 1, "patched")
        .await
        .map_err(err)?;
    if updated.revision != 2 || updated.payload != "patched" {
        return Err("cas did not advance revision".into());
    }
    let stale = store
        .cas_task("alice", "home", "t1", 1, "stale")
        .await
        .unwrap_err();
    if !matches!(stale, StorageError::Conflict { .. }) {
        return Err(format!("stale cas was {stale}"));
    }
    Ok(())
}

async fn chat_page(store: &dyn Gate1Store) -> Result<(), String> {
    store
        .create_session("alice", "home", "s1")
        .await
        .map_err(err)?;
    for body in ["a", "b", "c", "d", "e"] {
        store
            .append_message("alice", "home", "s1", body)
            .await
            .map_err(err)?;
    }
    let page = store
        .page_messages("alice", "home", "s1", 2, 2)
        .await
        .map_err(err)?;
    if page.seqs != [3, 4] || page.bodies != ["c", "d"] {
        return Err(format!("page was {:?}", page.seqs));
    }
    Ok(())
}

async fn attention(store: &dyn Gate1Store) -> Result<(), String> {
    store
        .put_attention("alice", "home", "hot", 0.9, 100)
        .await
        .map_err(err)?;
    store
        .put_attention("alice", "home", "expired", 1.0, 10)
        .await
        .map_err(err)?;
    store
        .put_attention("alice", "home", "warm", 0.5, 100)
        .await
        .map_err(err)?;
    let ids = store
        .retained_attention("alice", "home", 50)
        .await
        .map_err(err)?;
    if ids != ["hot", "warm"] {
        return Err(format!("retention order {ids:?}"));
    }
    Ok(())
}

async fn outbox_and_lease(store: &dyn Gate1Store) -> Result<(), String> {
    store
        .enqueue_outbox("alice", "home", "o1", "send")
        .await
        .map_err(err)?;
    let gen = store
        .claim_outbox("alice", "home", "o1", "worker-a", 10, 30)
        .await
        .map_err(err)?;
    if gen != 1 {
        return Err(format!("first claim generation {gen}"));
    }
    let lost = store
        .claim_outbox("alice", "home", "o1", "worker-b", 10, 30)
        .await
        .unwrap_err();
    if !matches!(lost, StorageError::Conflict { .. }) {
        return Err(format!("second claim was {lost}"));
    }
    let lease = store
        .acquire_lease("scope.alice.home", "proc-a", 10, 30)
        .await
        .map_err(err)?;
    if lease != 1 {
        return Err(format!("lease generation {lease}"));
    }
    let refused = store
        .acquire_lease("scope.alice.home", "proc-b", 10, 30)
        .await
        .unwrap_err();
    if !matches!(refused, StorageError::Conflict { .. }) {
        return Err(format!("second lease was {refused}"));
    }
    store
        .renew_lease("scope.alice.home", "proc-a", 1, 20, 30)
        .await
        .map_err(err)?;
    let stale = store
        .renew_lease("scope.alice.home", "proc-a", 99, 20, 30)
        .await
        .unwrap_err();
    if !matches!(stale, StorageError::LeaseLost { .. }) {
        return Err(format!("stale renew was {stale}"));
    }
    Ok(())
}

async fn isolation(store: &dyn Gate1Store) -> Result<(), String> {
    store
        .create_task(&TaskRow {
            principal: "bob".into(),
            workspace: "work".into(),
            task_id: "secret".into(),
            revision: 1,
            idempotency_key: "bob-secret".into(),
            payload: "hidden".into(),
        })
        .await
        .map_err(err)?;
    let leaked = store
        .get_task("alice", "home", "secret")
        .await
        .map_err(err)?;
    if leaked.is_some() {
        return Err("scope leaked".into());
    }
    Ok(())
}

async fn cursor_order(store: &dyn Gate1Store) -> Result<(), String> {
    store
        .create_session("alice", "home", "ordered")
        .await
        .map_err(err)?;
    let a = store
        .append_message("alice", "home", "ordered", "one")
        .await
        .map_err(err)?;
    let b = store
        .append_message("alice", "home", "ordered", "two")
        .await
        .map_err(err)?;
    if a >= b {
        return Err(format!("seq {a} then {b}"));
    }
    Ok(())
}

async fn migration(store: &dyn Gate1Store) -> Result<(), String> {
    store.migrate_v2_add_task_status().await.map_err(err)?;
    store.migrate_v2_add_task_status().await.map_err(err)
}

async fn uncommitted(store: &dyn Gate1Store) -> Result<(), String> {
    if store.uncommitted_insert_dropped().await.map_err(err)? {
        Ok(())
    } else {
        Err("uncommitted row survived".into())
    }
}

async fn committed(store: &dyn Gate1Store) -> Result<(), String> {
    if store.committed_insert_survives().await.map_err(err)? {
        Ok(())
    } else {
        Err("committed row missing".into())
    }
}

async fn backup(store: &dyn Gate1Store) -> Result<(), String> {
    match store.backup_restore_round_trip().await {
        Ok(true) => Ok(()),
        Ok(false) => Err("backup counts diverged".into()),
        Err(StorageError::UnsupportedCapability) => Err("operator dump/PITR required".into()),
        Err(err) => Err(err.to_string()),
    }
}

fn err(err: StorageError) -> String {
    err.to_string()
}
