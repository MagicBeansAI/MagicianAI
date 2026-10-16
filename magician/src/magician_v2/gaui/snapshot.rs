//! Shared GAUI snapshot loading utilities.
//!
//! Used by both REST (`gaui_api`) and WebSocket snapshot handlers to keep
//! payload semantics aligned:
//! - same cache-first read path
//! - same validation gates
//! - same JSONPath materialization behavior
//! - same empty-document fallback

use tracing::warn;

use crate::magician_v2::gaui::{
    materialize_component_queries, DefaultComponentRegistry, MuijDocument, MuijDocumentCache,
    MuijQueryEngine, MuijStorage,
};

#[derive(Debug)]
pub enum SnapshotLoadError {
    InvalidLayout,
    StorageRead,
}

pub fn agent_snapshot_cache_key(
    agent_id: &str,
    principal: Option<&str>,
    workspace: Option<&str>,
) -> String {
    match (principal, workspace) {
        (Some(principal), Some(workspace)) => format!("{principal}\0{workspace}\0{agent_id}"),
        _ => format!("\0\0{agent_id}"),
    }
}

/// Load and materialize a GAUI snapshot document for an agent.
///
/// Behavior:
/// - read from `doc_cache` first when present
/// - fall back to storage
/// - return `MuijDocument::new(agent_id)` when no layout exists
/// - validate stored/cached docs with `DefaultComponentRegistry`
/// - materialize component queries into `static_snapshot`
pub async fn load_materialized_snapshot_document(
    storage: &MuijStorage,
    agent_id: &str,
    query_engine: &mut MuijQueryEngine,
    doc_cache: Option<&MuijDocumentCache>,
    cache_key: Option<&str>,
) -> Result<MuijDocument, SnapshotLoadError> {
    let cached_doc = if let Some(cache) = doc_cache {
        let guard = cache.read().await;
        let lookup_key = cache_key.unwrap_or(agent_id);
        guard.get(lookup_key).cloned()
    } else {
        None
    };

    let read_result = if let Some(doc) = cached_doc {
        Ok(Some(doc))
    } else {
        storage.read_layout(agent_id).await.map_err(|_| {
            warn!(agent_id = %agent_id, "snapshot load failed: storage read error");
            SnapshotLoadError::StorageRead
        })
    }?;

    let mut doc = match read_result {
        Some(doc) => doc,
        None => return Ok(MuijDocument::new(agent_id.to_string())),
    };

    let registry = DefaultComponentRegistry;
    if doc.agent_id != agent_id {
        warn!(
            expected_agent_id = %agent_id,
            document_agent_id = %doc.agent_id,
            "snapshot load failed: agent_id mismatch"
        );
        return Err(SnapshotLoadError::InvalidLayout);
    }

    if let Err(error) = doc.validate(&registry) {
        warn!(
            agent_id = %agent_id,
            error = %error,
            "snapshot load failed: document validation error"
        );
        return Err(SnapshotLoadError::InvalidLayout);
    }

    materialize_component_queries(&mut doc.layout, query_engine);
    Ok(doc)
}

/// Load and materialize a GAUI snapshot document for a published surface.
///
/// Behavior:
/// - read from surface layout storage
/// - return `Ok(None)` when no layout exists
/// - validate stored docs with `DefaultComponentRegistry`
/// - materialize component queries into `static_snapshot`
pub async fn load_materialized_surface_snapshot_document(
    storage: &MuijStorage,
    document_key: &str,
    query_engine: &mut MuijQueryEngine,
) -> Result<Option<MuijDocument>, SnapshotLoadError> {
    let read_result = storage
        .read_surface_layout(document_key)
        .await
        .map_err(|_| {
            warn!(
                document_key = %document_key,
                "surface snapshot load failed: storage read error"
            );
            SnapshotLoadError::StorageRead
        })?;

    let mut doc = match read_result {
        Some(doc) => doc,
        None => return Ok(None),
    };

    let registry = DefaultComponentRegistry;
    if doc.agent_id != document_key {
        warn!(
            expected_document_key = %document_key,
            document_agent_id = %doc.agent_id,
            "surface snapshot load failed: document_key mismatch"
        );
        return Err(SnapshotLoadError::InvalidLayout);
    }

    if let Err(error) = doc.validate(&registry) {
        warn!(
            document_key = %document_key,
            error = %error,
            "surface snapshot load failed: document validation error"
        );
        return Err(SnapshotLoadError::InvalidLayout);
    }

    materialize_component_queries(&mut doc.layout, query_engine);
    Ok(Some(doc))
}
