//! Owner-approved, run-scoped grants that let the internal analyst read the
//! wording of the owner's preferences.
//!
//! The analyst reads preferences redacted ([`super::redact`]). When it needs
//! the words, it asks (`request_preference_wording`): a user request of type
//! [`WORDING_UNLOCK_REQUEST_TYPE`] the owner answers on a private surface
//! (web, iOS). An approval records a grant bound to that analyst run's
//! execution id; reads in that run return wording, reads in any other run do
//! not. A grant also lapses after [`GRANT_TTL_MS`], whatever the run.
//!
//! Grants live in `memory/users/alignment_wording_grants.json`, written whole
//! and atomically under their own lock; the analyst's provider reads them
//! without one. Each grant is journaled in the preference history.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::actor::WriteActor;
use super::history::{append_event, event, HistoryOp};
use crate::magician_v2::agents::storage::AgentStorage;

pub const WORDING_UNLOCK_REQUEST_TYPE: &str = "alignment.wording_unlock";
/// The option an owner picks to grant.
pub const WORDING_UNLOCK_APPROVE: &str = "approve";
pub const WORDING_UNLOCK_DECLINE: &str = "decline";
pub const WORDING_GRANTS_FILE: &str = "alignment_wording_grants.json";
/// A grant lapses after a day even if its run never ends.
pub const GRANT_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordingGrant {
    pub execution_id: String,
    pub request_id: String,
    pub surface: String,
    pub granted_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrantFile {
    #[serde(default)]
    grants: Vec<WordingGrant>,
}

pub fn wording_grants_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(WORDING_GRANTS_FILE)
}

fn read_grants(path: &std::path::Path) -> Vec<WordingGrant> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<GrantFile>(&bytes).ok())
        .map(|file| file.grants)
        .unwrap_or_default()
}

/// Whether the owner granted wording to the analyst run `execution_id` and
/// the grant has not lapsed. Synchronous: the analyst's data provider runs
/// on a blocking thread. An unreadable file grants nothing.
pub fn grant_for(storage: &AgentStorage, execution_id: &str, now_ms: i64) -> bool {
    !execution_id.is_empty()
        && read_grants(&wording_grants_path(storage))
            .iter()
            .any(|grant| grant.execution_id == execution_id && grant.expires_at_ms > now_ms)
}

/// Record the owner's approval for `execution_id`, dropping lapsed grants,
/// and journal it. The caller checked the answer came from a private surface.
pub async fn record_grant(
    storage: &AgentStorage,
    execution_id: &str,
    request_id: &str,
    surface: &str,
    now_ms: i64,
) -> std::io::Result<()> {
    if execution_id.is_empty() {
        return Err(std::io::Error::other(
            "a wording grant needs the analyst run's execution id",
        ));
    }
    let path = wording_grants_path(storage);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    {
        let _flock = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(std::io::Error::other)?;
        let mut grants: Vec<WordingGrant> = read_grants(&path)
            .into_iter()
            .filter(|grant| grant.expires_at_ms > now_ms)
            .collect();
        grants.push(WordingGrant {
            execution_id: execution_id.to_string(),
            request_id: request_id.to_string(),
            surface: surface.to_string(),
            granted_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(GRANT_TTL_MS),
        });
        let body =
            serde_json::to_vec_pretty(&GrantFile { grants }).map_err(std::io::Error::other)?;
        magician_core::durable_io::write_bytes_durably(&path, &body).await?;
    }
    // After the grant lock: the history takes the knowledge lock.
    append_event(
        storage,
        event(
            &format!("analyst_wording:{execution_id}"),
            HistoryOp::WordingGranted,
            WriteActor::surface(surface).with_cause(request_id.to_string()),
            now_ms,
        ),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::agents::AgentMemoryService;
    use crate::magician_v2::preference_alignment::history::read_history;

    #[tokio::test]
    async fn a_grant_covers_its_run_only_until_it_lapses_and_is_journaled() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let storage = service.storage();
        assert!(!grant_for(storage, "exec-analyst", 0), "no grant yet");
        record_grant(storage, "exec-analyst", "req-1", "web", 1_000)
            .await
            .unwrap();
        assert!(grant_for(storage, "exec-analyst", 2_000));
        assert!(
            !grant_for(storage, "exec-other", 2_000),
            "another run is not covered"
        );
        assert!(
            !grant_for(storage, "exec-analyst", 1_000 + GRANT_TTL_MS),
            "lapsed"
        );
        assert!(!grant_for(storage, "", 2_000));
        let history = read_history(storage, None, 0, 10).await.unwrap();
        assert_eq!(history[0].op, HistoryOp::WordingGranted);
        assert_eq!(history[0].actor.surface.as_deref(), Some("web"));
    }

    #[tokio::test]
    async fn lapsed_grants_are_dropped_on_the_next_write() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let storage = service.storage();
        record_grant(storage, "old", "req-1", "ios", 0)
            .await
            .unwrap();
        record_grant(storage, "new", "req-2", "web", GRANT_TTL_MS + 1)
            .await
            .unwrap();
        let grants = read_grants(&wording_grants_path(storage));
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].execution_id, "new");
    }
}
