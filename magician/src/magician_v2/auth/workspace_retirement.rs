//! Retiring a deleted workspace's state that lives OUTSIDE its directory.
//!
//! `workspace_registry::drain_pending_purges` removes `scopes/<p>/<w>/` at
//! boot, but a workspace also leaves rows in host-level databases and files:
//! the attention learning, resurfacing and funnel databases, the list index,
//! the HITL lifecycle authority and delivery log, bearers, paired devices,
//! push registrations, and digest-named caches. Rows left there kept
//! re-proposing the scope to workers that re-created its directory, and a
//! re-created slug would silently inherit them.
//!
//! Two boot phases, both best effort (a failure is logged and never blocks
//! boot), both driven by one predicate — `workspace_registry::is_deleted_workspace`,
//! which requires a TOMBSTONE (the registry's `deleted` list or
//! `pending_purge`). Absence from the registry is never enough to delete
//! anything: a workspace can be missing from it for reasons that are not a
//! deletion (pre-registry data, a damaged file). Tombstoned scopes left by
//! older deletions are cleaned too:
//!
//! 1. [`retire_host_state_before_stores_open`], right after the purge drain
//!    and before any store or the event broadcaster opens: files, the HITL
//!    lifecycle authority, the HITL delivery log, and auth bearers.
//! 2. [`retire_store_rows`], once the databases are open and before their
//!    background workers start: every store's rows for retired scopes.
//!
//! Kept on purpose: `llm_dispatch/*.jsonl` (accounting, aged out by its own
//! retention), `system/device-actions.jsonl` (audit), and the notes folder
//! `spaces/<p>/<w>` (the owner's notes).

use std::collections::BTreeSet;
use std::path::Path;

use super::workspace_registry::{self, is_deleted_workspace, PurgeOutcome};

type Scope = (String, String);

/// Deleted (tombstoned) scopes known without opening any store: the workspaces
/// purged at this boot plus leftover directories of tombstoned workspaces.
/// Used only for per-scope FILE cleanup; database stores act on the scopes
/// they actually hold rows for.
pub fn known_retired_scopes(scopes_root: &Path, purge_outcomes: &[PurgeOutcome]) -> Vec<Scope> {
    let mut scopes = purge_outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            PurgeOutcome::Removed {
                principal,
                workspace,
            } => Some((principal.clone(), workspace.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    scopes.extend(workspace_registry::enumerate_live_scopes(scopes_root).residue);
    workspace_registry::deleted_scopes_among(scopes_root, scopes)
}

/// Remove the digest- or name-keyed per-scope files a retired workspace owns
/// under the host root: taste-capture watermark and proposals, the content
/// source cache, and the actionability training snapshot. Each scope is
/// re-checked for a tombstone first. Returns how many files or directories
/// were removed.
pub fn retire_scope_files(base_root: &Path, scopes: &[Scope]) -> usize {
    let scopes_root = base_root.join("scopes");
    let mut removed = 0usize;
    for (principal, workspace) in scopes {
        if !is_deleted_workspace(&scopes_root, principal, workspace) {
            continue;
        }
        let mut files =
            crate::magician_v2::taste_capture::scope_state_file_names(principal, workspace)
                .into_iter()
                .map(|name| base_root.join("system").join(name))
                .collect::<Vec<_>>();
        // `actionability-{p}-{w}.json` is ambiguous (`a-b`/`c` and `a`/`b-c`
        // name the same file): delete it only when no other non-deleted scope
        // on disk maps to the same name.
        let snapshot_name = format!("actionability-{principal}-{workspace}.json");
        if !snapshot_name_is_shared(&scopes_root, principal, workspace, &snapshot_name) {
            files.push(
                base_root
                    .join("attention_learning")
                    .join("snapshots")
                    .join(snapshot_name),
            );
        }
        for file in files {
            match std::fs::remove_file(&file) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    path = %file.display(),
                    error = %error,
                    "could not remove a deleted workspace's file"
                ),
            }
        }
        let cache_dir = crate::magician_v2::content_sources::scope_cache_dir(
            &base_root.join("content_sources").join("cache"),
            principal,
            workspace,
        );
        match std::fs::remove_dir_all(&cache_dir) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => tracing::warn!(
                principal = %principal,
                workspace = %workspace,
                path = %cache_dir.display(),
                error = %error,
                "could not remove a deleted workspace's content cache"
            ),
        }
    }
    removed
}

/// Whether another scope that is NOT deleted maps to the same ambiguous
/// actionability snapshot name — any `<p'>-<w'>` split of the same string.
fn snapshot_name_is_shared(
    scopes_root: &Path,
    principal: &str,
    workspace: &str,
    snapshot_name: &str,
) -> bool {
    let Some(joined) = snapshot_name
        .strip_prefix("actionability-")
        .and_then(|rest| rest.strip_suffix(".json"))
    else {
        return true;
    };
    joined.match_indices('-').any(|(index, _)| {
        let (other_principal, other_workspace) = (&joined[..index], &joined[index + 1..]);
        (other_principal, other_workspace) != (principal, workspace)
            && !is_deleted_workspace(scopes_root, other_principal, other_workspace)
            && (scopes_root
                .join(other_principal)
                .join(other_workspace)
                .is_dir()
                || workspace_registry::is_live_workspace(
                    scopes_root,
                    other_principal,
                    other_workspace,
                ))
    })
}

/// What [`retire_host_state_before_stores_open`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostRetirementReport {
    pub files_removed: usize,
    pub hitl_lifecycle_rows: usize,
    pub hitl_deliveries: usize,
    pub bearers: usize,
}

/// Boot phase 1 — call after `drain_pending_purges` and BEFORE any store, the
/// HITL delivery coordinator, or the event broadcaster opens. Never fails.
pub fn retire_host_state_before_stores_open(
    base_root: &Path,
    known_retired: &[Scope],
) -> HostRetirementReport {
    let scopes_root = base_root.join("scopes");
    let retired =
        |principal: &str, workspace: &str| is_deleted_workspace(&scopes_root, principal, workspace);
    let mut report = HostRetirementReport {
        files_removed: retire_scope_files(base_root, known_retired),
        ..HostRetirementReport::default()
    };
    match crate::magician_v2::realtime_events::retire_hitl_lifecycle_scopes(base_root, &retired) {
        Ok(rows) => report.hitl_lifecycle_rows = rows,
        Err(error) => tracing::warn!(
            error = %error,
            "could not retire deleted workspaces' HITL lifecycle rows"
        ),
    }
    match retire_hitl_delivery_rows(base_root, &retired) {
        Ok(rows) => report.hitl_deliveries = rows,
        Err(error) => tracing::warn!(
            error = %error,
            "could not retire deleted workspaces' HITL delivery rows"
        ),
    }
    match super::AuthStore::open(base_root)
        .and_then(|store| store.revoke_bearers_for_retired_workspaces())
    {
        Ok(revoked) => report.bearers = revoked.total(),
        Err(error) => tracing::warn!(
            error = %error,
            "could not revoke deleted workspaces' bearers"
        ),
    }
    if report != HostRetirementReport::default() {
        tracing::info!(
            files_removed = report.files_removed,
            hitl_lifecycle_rows = report.hitl_lifecycle_rows,
            hitl_deliveries = report.hitl_deliveries,
            bearers = report.bearers,
            "retired host state left by deleted workspaces"
        );
    }
    report
}

/// Drop `system/hitl-deliveries.json` rows of retired scopes. Edited as JSON
/// values, not typed records, so a field this module does not know survives.
/// Runs before `DeliveryStore::open`, whose restart recovery would otherwise
/// re-drive a deleted workspace's alerts.
fn retire_hitl_delivery_rows(
    base_root: &Path,
    retired: &dyn Fn(&str, &str) -> bool,
) -> std::io::Result<usize> {
    let path = base_root.join("system").join("hitl-deliveries.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    // An unreadable log is the delivery store's to set aside; never rewrite it.
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_slice::<serde_json::Value>(&bytes)
    else {
        return Ok(0);
    };
    let before = rows.len();
    let kept = rows
        .into_iter()
        .filter(|row| {
            let principal = row.get("principal").and_then(|value| value.as_str());
            let workspace = row.get("workspace").and_then(|value| value.as_str());
            !matches!((principal, workspace), (Some(p), Some(w)) if retired(p, w))
        })
        .collect::<Vec<_>>();
    let removed = before - kept.len();
    if removed == 0 {
        return Ok(0);
    }
    let bytes = serde_json::to_vec_pretty(&kept).map_err(std::io::Error::other)?;
    crate::magician_v2::artifact_v2::io::write_bytes_durably_with_mode_sync(
        &path,
        &bytes,
        Some(0o600),
    )?;
    Ok(removed)
}

/// The host databases [`retire_store_rows`] cleans. Any may be absent.
#[derive(Default)]
pub struct RetirementStores<'a> {
    pub learning: Option<&'a crate::magician_v2::attention::learning::AttentionLearningStore>,
    pub resurfacing:
        Option<&'a crate::magician_v2::attention::resurfacing::store::ResurfacingStore>,
    pub funnel: Option<&'a crate::magician_v2::attention_funnel_store::AttentionFunnelStore>,
    pub list_index: Option<&'a crate::magician_v2::storage::ListIndex>,
    pub device_pairing: Option<&'a crate::magician_v2::device_pairing::DevicePairingStore>,
    pub mobile_push: Option<&'a crate::magician_v2::mobile_push::MobilePushStore>,
}

/// Rows removed per store by [`retire_store_rows`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreRetirementReport {
    /// Every retired scope that held state somewhere, sorted.
    pub scopes: Vec<Scope>,
    /// `store name -> rows (or records) removed`.
    pub removed: std::collections::BTreeMap<&'static str, u64>,
}

/// Boot phase 2 — call once the stores are open and before their background
/// workers start. For each store, finds the scopes it actually holds rows for
/// and deletes the rows of the TOMBSTONED ones only (a store with no rows for
/// a deleted workspace costs one scan, nothing more); then removes per-scope
/// files for those scopes and `known_retired`. Conservative: only a principal
/// whose registry exists, is readable and plausible, and tombstones the id;
/// never a default workspace or a reserved sink. Never fails; logs counts per
/// store and the elapsed time.
pub async fn retire_store_rows(
    base_root: &Path,
    stores: RetirementStores<'_>,
    known_retired: &[Scope],
) -> StoreRetirementReport {
    let started = std::time::Instant::now();
    let scopes_root = base_root.join("scopes");
    let mut report = StoreRetirementReport::default();
    let mut all_scopes = known_retired.iter().cloned().collect::<BTreeSet<_>>();
    let candidates =
        |found: Vec<Scope>| workspace_registry::deleted_scopes_among(&scopes_root, found);

    if let Some(store) = stores.learning {
        let found = match store.scopes_with_rows().await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "could not scan attention learning scopes");
                Vec::new()
            },
        };
        for (principal, workspace) in candidates(found) {
            match store.delete_scope(&principal, &workspace, true).await {
                Ok(deleted) => {
                    *report.removed.entry("attention_learning").or_default() +=
                        deleted.affected_rows.values().sum::<u64>();
                },
                Err(error) => tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %format!("{error:#}"),
                    "could not retire attention learning rows"
                ),
            }
            all_scopes.insert((principal, workspace));
        }
    }
    if let Some(store) = stores.resurfacing {
        let found = store.scopes_with_rows().await.unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "could not scan resurfacing scopes");
            Vec::new()
        });
        for (principal, workspace) in candidates(found) {
            match store.retire_scope(&principal, &workspace).await {
                Ok(rows) => *report.removed.entry("resurfacing").or_default() += rows,
                Err(error) => tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %format!("{error:#}"),
                    "could not retire resurfacing rows"
                ),
            }
            all_scopes.insert((principal, workspace));
        }
    }
    if let Some(store) = stores.funnel {
        let found = store.scopes_with_rows().await.unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "could not scan attention funnel scopes");
            Vec::new()
        });
        for (principal, workspace) in candidates(found) {
            match store.delete_scope(&principal, &workspace).await {
                Ok(rows) => *report.removed.entry("attention_funnel").or_default() += rows,
                Err(error) => tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %format!("{error:#}"),
                    "could not retire attention funnel rows"
                ),
            }
            all_scopes.insert((principal, workspace));
        }
    }
    if let Some(index) = stores.list_index {
        let found = index.indexed_scopes().unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "could not scan list index scopes");
            Vec::new()
        });
        for (principal, workspace) in candidates(found) {
            let scope = crate::magician_v2::storage::list_index::ListScope::new(
                principal.clone(),
                workspace.clone(),
            );
            match index.remove_scope_all_kinds(&scope) {
                Ok(rows) => *report.removed.entry("list_index").or_default() += rows as u64,
                Err(error) => tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %format!("{error:#}"),
                    "could not retire list index rows"
                ),
            }
            all_scopes.insert((principal, workspace));
        }
    }
    if let Some(store) = stores.device_pairing {
        for (principal, workspace) in candidates(store.paired_scopes().await) {
            match store.unpair_scope(&principal, &workspace).await {
                Ok(devices) => {
                    *report.removed.entry("paired_devices").or_default() += devices as u64
                },
                Err(error) => tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    "could not unpair a deleted workspace's devices"
                ),
            }
            all_scopes.insert((principal, workspace));
        }
    }
    if let Some(store) = stores.mobile_push {
        for (principal, workspace) in candidates(store.registered_scopes().await) {
            match store.remove_for_scope(&principal, &workspace).await {
                Ok(rows) => *report.removed.entry("mobile_push").or_default() += rows as u64,
                Err(error) => tracing::warn!(
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    "could not remove a deleted workspace's push registrations"
                ),
            }
            all_scopes.insert((principal, workspace));
        }
    }

    report.scopes = all_scopes.into_iter().collect();
    let files = retire_scope_files(base_root, &report.scopes);
    if files > 0 {
        report.removed.insert("files", files as u64);
    }
    report.removed.retain(|_, count| *count > 0);
    for (store, rows) in &report.removed {
        tracing::info!(
            store = *store,
            rows = *rows,
            "retired rows left by deleted workspaces"
        );
    }
    tracing::info!(
        scopes = report.scopes.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "deleted-workspace store retirement pass finished"
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::auth::identity::Identity;
    use crate::magician_v2::auth::workspace_registry::{
        create, delete_and_schedule_purge, drain_pending_purges, ensure_default,
    };

    fn owner() -> Identity {
        Identity {
            name: "owner".into(),
            display_name: "Owner".into(),
            scope_root: "anonymous".into(),
            created_at: chrono::Utc::now(),
        }
    }

    fn funnel_event(
        event_id: &str,
        workspace: &str,
    ) -> crate::magician_v2::attention_funnel::AttentionRouteEvent {
        use crate::magician_v2::attention_funnel::*;
        AttentionRouteEvent {
            event_id: event_id.to_string(),
            scope: AttentionScope {
                principal: "anonymous".to_string(),
                workspace: workspace.to_string(),
            },
            source: AttentionSource {
                kind: AttentionSourceKind::Comm,
                source_ref: "thread-1".to_string(),
                provider: None,
                account_alias: None,
            },
            source_family: AttentionSourceFamily::Promise,
            candidate_key: format!("candidate-{event_id}"),
            stage: AttentionFunnelStage::Dropped,
            outcome: RouteOutcome::Dropped {
                reason: DropReason::WeakSignal,
            },
            occurred_at: 1_000,
            created_at: chrono::Utc::now().timestamp_millis(),
            confidence: None,
            metadata: serde_json::json!({}),
        }
    }

    fn list_entry(id: &str, workspace: &str) -> crate::magician_v2::storage::ListEntry {
        crate::magician_v2::storage::ListEntry {
            kind: crate::magician_v2::storage::ListKind::Task,
            id: id.to_string(),
            scope: crate::magician_v2::storage::ListScope::new("anonymous", workspace),
            updated_at: 1,
            created_at: 1,
            status: "completed".to_string(),
            agent_id: "agent".to_string(),
            lifecycle: None,
            due_date: None,
            tags: Vec::new(),
            title: None,
            paused: false,
        }
    }

    /// Delete + purge + store retirement leaves nothing a re-created slug
    /// would inherit; a live workspace's rows and files, and an UNLISTED
    /// (absent but never tombstoned) workspace's rows, are untouched.
    #[tokio::test]
    async fn delete_purge_and_retirement_leave_nothing_for_a_recreated_slug() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let scopes = base.join("scopes");
        let identity = owner();
        ensure_default(&scopes, &identity).unwrap();
        create(&scopes, &identity, "eval-1", "Eval", None).unwrap();

        let learning =
            crate::magician_v2::attention::learning::AttentionLearningStore::open(base).unwrap();
        let funnel =
            crate::magician_v2::attention_funnel_store::AttentionFunnelStore::open(base).unwrap();
        let list_index = crate::magician_v2::storage::ListIndex::open(base).unwrap();
        let resurfacing =
            crate::magician_v2::attention::resurfacing::store::ResurfacingStore::open(base)
                .unwrap();
        // Rows for the doomed workspace, the live default, and `ghost` — a
        // workspace absent from the registry but never tombstoned.
        for (n, workspace) in ["eval-1", "default", "ghost"].into_iter().enumerate() {
            learning
                .initialize_historical_bootstrap("anonymous", workspace, &["mail"], 1)
                .await
                .unwrap();
            resurfacing
                .set_watermark("anonymous", workspace, "memory", 1)
                .await
                .unwrap();
            funnel
                .append_event(funnel_event(&format!("e{n}"), workspace))
                .await
                .unwrap();
            list_index
                .upsert(&list_entry(&format!("t-{n}"), workspace))
                .unwrap();
        }
        std::fs::create_dir_all(base.join("system")).unwrap();
        for workspace in ["eval-1", "default", "ghost"] {
            for name in
                crate::magician_v2::taste_capture::scope_state_file_names("anonymous", workspace)
            {
                std::fs::write(base.join("system").join(name), b"{}").unwrap();
            }
        }
        std::fs::write(
            base.join("system").join("hitl-deliveries.json"),
            serde_json::to_vec(&serde_json::json!([
                {"id": "a", "principal": "anonymous", "workspace": "eval-1"},
                {"id": "b", "principal": "anonymous", "workspace": "default"},
                {"id": "c", "principal": "anonymous", "workspace": "ghost"}
            ]))
            .unwrap(),
        )
        .unwrap();

        delete_and_schedule_purge(&scopes, &identity, "eval-1").unwrap();
        let outcomes = drain_pending_purges(&scopes);
        let known = known_retired_scopes(&scopes, &outcomes);
        let eval = ("anonymous".to_string(), "eval-1".to_string());
        assert_eq!(known, vec![eval.clone()]);

        let host = retire_host_state_before_stores_open(base, &known);
        assert_eq!(host.files_removed, 2);
        assert_eq!(host.hitl_deliveries, 1);

        let report = retire_store_rows(
            base,
            RetirementStores {
                learning: Some(&learning),
                funnel: Some(&funnel),
                list_index: Some(&list_index),
                resurfacing: Some(&resurfacing),
                ..RetirementStores::default()
            },
            &[],
        )
        .await;
        assert_eq!(report.scopes, vec![eval.clone()]);
        for store in [
            "attention_learning",
            "resurfacing",
            "attention_funnel",
            "list_index",
        ] {
            assert!(
                report.removed.get(store).copied().unwrap_or(0) > 0,
                "{store} removed nothing"
            );
        }
        let survivors = vec![
            ("anonymous".to_string(), "default".to_string()),
            ("anonymous".to_string(), "ghost".to_string()),
        ];
        assert_eq!(learning.scopes_with_rows().await.unwrap(), survivors);
        assert_eq!(resurfacing.scopes_with_rows().await.unwrap(), survivors);
        assert_eq!(funnel.scopes_with_rows().await.unwrap(), survivors);
        assert_eq!(list_index.indexed_scopes().unwrap(), survivors);
        for workspace in ["default", "ghost"] {
            for name in
                crate::magician_v2::taste_capture::scope_state_file_names("anonymous", workspace)
            {
                assert!(
                    base.join("system").join(name).exists(),
                    "{workspace} files survive"
                );
            }
        }

        // The slug can be created again and starts from nothing.
        create(&scopes, &identity, "eval-1", "Eval again", None).unwrap();
        assert!(!scopes.join("anonymous").join("eval-1").exists());
        for name in crate::magician_v2::taste_capture::scope_state_file_names("anonymous", "eval-1")
        {
            assert!(!base.join("system").join(name).exists());
        }
        assert!(!learning.scopes_with_rows().await.unwrap().contains(&eval));
    }

    /// The orphan sweep acts only on tombstoned scopes: never a live scope, a
    /// default workspace, a principal without a registry, or a workspace that
    /// is merely absent from its registry.
    #[test]
    fn known_retired_scopes_require_a_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let scopes = dir.path().join("scopes");
        let identity = owner();
        ensure_default(&scopes, &identity).unwrap();
        create(&scopes, &identity, "work", "Work", None).unwrap();
        create(&scopes, &identity, "old", "Old", None).unwrap();
        workspace_registry::delete(&scopes, &identity, "old").unwrap();
        for (principal, workspace) in [
            ("anonymous", "default"),
            ("anonymous", "work"),
            ("anonymous", "old"),
            ("anonymous", "never-registered"),
            ("fixture", "ws"),
        ] {
            std::fs::create_dir_all(scopes.join(principal).join(workspace)).unwrap();
        }
        assert_eq!(
            known_retired_scopes(&scopes, &[]),
            vec![("anonymous".to_string(), "old".to_string())]
        );
        assert_eq!(
            workspace_registry::deleted_scopes_among(
                &scopes,
                [
                    ("anonymous".to_string(), "default".to_string()),
                    ("anonymous".to_string(), "work".to_string()),
                    ("anonymous".to_string(), "never-registered".to_string()),
                    ("fixture".to_string(), "ws".to_string()),
                    ("system".to_string(), "system".to_string()),
                ]
            ),
            Vec::<Scope>::new()
        );
    }

    /// An empty registry file is not evidence of anything: it fails open and
    /// nothing is retired.
    #[tokio::test]
    async fn an_empty_registry_retires_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let scopes = base.join("scopes");
        std::fs::create_dir_all(scopes.join("anonymous").join("work")).unwrap();
        std::fs::write(
            workspace_registry::registry_path_for_principal_dir(&scopes, "anonymous"),
            b"",
        )
        .unwrap();
        let funnel =
            crate::magician_v2::attention_funnel_store::AttentionFunnelStore::open(base).unwrap();
        funnel
            .append_event(funnel_event("e1", "work"))
            .await
            .unwrap();
        assert!(!workspace_registry::is_retired_workspace(
            &scopes,
            "anonymous",
            "work"
        ));
        assert!(workspace_registry::is_live_workspace(
            &scopes,
            "anonymous",
            "work"
        ));
        let report = retire_store_rows(
            base,
            RetirementStores {
                funnel: Some(&funnel),
                ..RetirementStores::default()
            },
            &[],
        )
        .await;
        assert!(report.scopes.is_empty());
        assert_eq!(funnel.scopes_with_rows().await.unwrap().len(), 1);
    }
}
