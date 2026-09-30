//! LanceDB hybrid index for notes.
//!
//! The Markdown files stay the source of the text. This table stores one row
//! per note: the searchable text, a content hash, and an embedding. Search
//! reads that on-disk table and the full-text index stored with it. Changed
//! notes are embedded and merged in the background. A note is embedded only
//! when its file content changed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use anyhow::Context;
use blake3::Hasher;
use magician_vector_index::vector_toolkit::{
    HybridAdmissionHit, OllamaEmbedder, VectorItem, VectorTable,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::notes_search_index::{tree_stamp, IndexedNote, NotesIndexRoot};

const HASH_DIMS: usize = 32;
const MAX_EMBED_CHARS: usize = 8_000;
const MAX_EMBED_NOTES_PER_SEARCH: usize = 200;
/// L2 distance. Identical stored vectors are 0. Unrelated unit vectors sit
/// near sqrt(2). Notes closer than this are treated as the same meaning.
const MAX_SEMANTIC_DISTANCE: f32 = 0.8;

#[derive(Debug, Clone)]
pub(crate) struct HybridAdmission {
    pub provider: String,
    pub relative_path: String,
    pub semantic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Manifest {
    embedder: String,
    dims: usize,
    notes: HashMap<String, String>,
}

struct RefreshSlot {
    running: bool,
    pending: Option<Vec<IndexedNote>>,
}

pub(crate) struct RefreshStats {
    changed: usize,
    removed: usize,
}

fn watch_slots() -> &'static std::sync::Mutex<HashSet<String>> {
    static SLOTS: OnceLock<std::sync::Mutex<HashSet<String>>> = OnceLock::new();
    SLOTS.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

fn watch_timing() -> (Duration, Duration) {
    #[cfg(test)]
    {
        (Duration::from_millis(40), Duration::from_millis(80))
    }
    #[cfg(not(test))]
    {
        (Duration::from_millis(500), Duration::from_millis(800))
    }
}

/// Watch one notes index. A quiet tree costs a metadata stamp. After a write
/// settles, changed notes go through the same refresh a search would schedule.
pub(crate) fn ensure_notes_watch(index_dir: PathBuf, roots: Vec<NotesIndexRoot>) -> bool {
    let key = index_dir.to_string_lossy().to_string();
    {
        let mut slots = watch_slots().lock().unwrap_or_else(|error| error.into_inner());
        if !slots.insert(key) {
            return false;
        }
    }
    tokio::spawn(async move {
        let (poll, quiet) = watch_timing();
        let mut stable = tree_stamp(&roots).unwrap_or_default();
        loop {
            tokio::time::sleep(poll).await;
            let noticed = Instant::now();
            let Ok(mut latest) = tree_stamp(&roots) else {
                continue;
            };
            if latest == stable {
                continue;
            }
            loop {
                tokio::time::sleep(quiet).await;
                let Ok(again) = tree_stamp(&roots) else {
                    break;
                };
                if again == latest {
                    break;
                }
                latest = again;
            }
            let detect_ms = noticed.elapsed().as_millis();
            let snap_started = Instant::now();
            let snapshot = match super::notes_search_index::snapshot_for(roots.clone()).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    tracing::warn!(error = %error, "notes watch could not read the folder");
                    stable = latest;
                    continue;
                }
            };
            let snapshot_ms = snap_started.elapsed().as_millis();
            let refresh_started = Instant::now();
            match refresh_watched(&index_dir, &snapshot.notes).await {
                Ok(stats) => {
                    tracing::info!(
                        detect_ms,
                        snapshot_ms,
                        refresh_ms = refresh_started.elapsed().as_millis(),
                        notes = snapshot.notes.len(),
                        changed = stats.changed,
                        removed = stats.removed,
                        "notes watch refreshed"
                    );
                }
                Err(error) => {
                    tracing::warn!(error = %error, "notes watch refresh failed");
                }
            }
            stable = tree_stamp(&roots).unwrap_or(latest);
        }
    });
    true
}

async fn refresh_watched(index_dir: &Path, notes: &[IndexedNote]) -> anyhow::Result<RefreshStats> {
    #[cfg(test)]
    {
        let mode = EmbedMode {
            name: "hash".to_string(),
            dims: HASH_DIMS,
            semantic: false,
            embed: None,
        };
        return sync_index(index_dir, notes, &mode).await;
    }
    #[cfg(not(test))]
    {
        refresh_index(index_dir, notes).await
    }
}

fn refresh_slots() -> &'static Mutex<HashMap<String, RefreshSlot>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, RefreshSlot>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Query the on-disk table. This does not embed notes or rebuild the
/// full-text index; both happen in [`schedule_index_refresh`].
pub(crate) async fn query_stored_index(
    index_dir: &Path,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<HybridAdmission>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mode = embed_mode().await;
    let query_vector = if mode.semantic {
        match tokio::time::timeout(Duration::from_millis(600), embed_query(&mode, query)).await {
            Ok(Ok(vector)) => Some(vector),
            _ => None,
        }
    } else {
        None
    };
    let hits = VectorTable::at(index_dir, mode.dims)
        .search_bm25_fuzzy_and_vector(query, query_vector.as_deref(), limit, MAX_SEMANTIC_DISTANCE)
        .await?;
    Ok(admissions_from_hits(hits))
}

/// Merge changed notes into the on-disk table after the search has returned.
/// A refresh already in flight keeps the newest snapshot and runs it next.
pub(crate) async fn schedule_index_refresh(index_dir: PathBuf, notes: Vec<IndexedNote>) {
    let key = index_dir.to_string_lossy().to_string();
    {
        let mut slots = refresh_slots().lock().await;
        let slot = slots.entry(key.clone()).or_insert(RefreshSlot {
            running: false,
            pending: None,
        });
        if slot.running {
            slot.pending = Some(notes);
            return;
        }
        slot.running = true;
    }
    tokio::spawn(async move {
        let mut current = notes;
        loop {
            if let Err(error) = refresh_index(&index_dir, &current).await {
                tracing::warn!(error = %error, "notes index refresh failed");
            }
            let mut slots = refresh_slots().lock().await;
            let Some(slot) = slots.get_mut(&key) else {
                break;
            };
            match slot.pending.take() {
                Some(next) => {
                    current = next;
                },
                None => {
                    slot.running = false;
                    break;
                },
            }
        }
    });
}

pub(crate) async fn refresh_index(
    index_dir: &Path,
    notes: &[IndexedNote],
) -> anyhow::Result<RefreshStats> {
    let mut mode = embed_mode().await;
    match sync_index(index_dir, notes, &mode).await {
        Ok(stats) => Ok(stats),
        Err(error) if mode.semantic => {
            // Ollama can answer a health check and still have no embedding model
            // configured. The stored keyword index still has to be updated.
            mode = EmbedMode {
                name: "hash".to_string(),
                dims: HASH_DIMS,
                semantic: false,
                embed: None,
            };
            sync_index(index_dir, notes, &mode).await
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(crate) async fn hybrid_admissions_with_embedder(
    index_dir: PathBuf,
    notes: &[IndexedNote],
    query: &str,
    limit: usize,
    embed: fn(&[String]) -> Vec<Vec<f32>>,
    dims: usize,
) -> anyhow::Result<Vec<HybridAdmission>> {
    let mode = EmbedMode {
        name: "test".to_string(),
        dims,
        semantic: true,
        embed: Some(embed),
    };
    sync_index(&index_dir, notes, &mode).await?;
    let table = VectorTable::at(&index_dir, dims);
    let query_vector = embed(&[query.to_string()])
        .into_iter()
        .next()
        .context("test embedder returned no query vector")?;
    let hits = table
        .search_bm25_fuzzy_and_vector(
            query,
            Some(query_vector.as_slice()),
            limit,
            MAX_SEMANTIC_DISTANCE,
        )
        .await?;
    Ok(admissions_from_hits(hits))
}

fn admissions_from_hits(hits: Vec<HybridAdmissionHit>) -> Vec<HybridAdmission> {
    hits.into_iter()
        .filter_map(|hit| {
            let (provider, relative_path) = hit.id.split_once('\n')?;
            Some(HybridAdmission {
                provider: provider.to_string(),
                relative_path: relative_path.to_string(),
                semantic: hit.semantic,
            })
        })
        .collect()
}

struct EmbedMode {
    name: String,
    dims: usize,
    semantic: bool,
    embed: Option<fn(&[String]) -> Vec<Vec<f32>>>,
}

async fn embed_mode() -> EmbedMode {
    if ollama_ready().await {
        let cfg = OllamaEmbedder::from_env().config().clone();
        EmbedMode {
            name: format!("ollama:{}", cfg.embedding_contract_id()),
            dims: cfg.dims,
            semantic: true,
            embed: None,
        }
    } else {
        EmbedMode {
            name: "hash".to_string(),
            dims: HASH_DIMS,
            semantic: false,
            embed: None,
        }
    }
}

async fn ollama_ready() -> bool {
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    struct Cached(Instant, bool);
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    let slot = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = slot.lock() {
        if let Some(Cached(at, ready)) = *guard {
            if at.elapsed() < Duration::from_secs(60) {
                return ready;
            }
        }
    }
    let ready = tokio::time::timeout(
        Duration::from_millis(200),
        OllamaEmbedder::from_env().health_check(),
    )
    .await
    .ok()
    .and_then(|result| result.ok())
    .is_some();
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(Cached(Instant::now(), ready));
    }
    ready
}

async fn embed_texts(mode: &EmbedMode, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
    if let Some(embed) = mode.embed {
        return Ok(embed(texts));
    }
    if mode.semantic {
        return OllamaEmbedder::from_env().embed_documents(texts).await;
    }
    Ok(texts.iter().map(|text| hash_vector(text)).collect())
}

async fn embed_query(mode: &EmbedMode, query: &str) -> anyhow::Result<Vec<f32>> {
    if let Some(embed) = mode.embed {
        return embed(&[query.to_string()])
            .into_iter()
            .next()
            .context("test embedder returned no query vector");
    }
    OllamaEmbedder::from_env().embed_query(query).await
}

fn hash_vector(text: &str) -> Vec<f32> {
    let hash = blake3::hash(text.as_bytes());
    let bytes = hash.as_bytes();
    let mut vector = Vec::with_capacity(HASH_DIMS);
    for chunk in bytes.chunks(4).take(HASH_DIMS) {
        let mut padded = [0u8; 4];
        padded[..chunk.len()].copy_from_slice(chunk);
        let value = i32::from_le_bytes(padded) as f32 / i32::MAX as f32;
        vector.push(value);
    }
    while vector.len() < HASH_DIMS {
        vector.push(0.0);
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn note_id(note: &IndexedNote) -> String {
    format!("{}\n{}", note.provider, note.relative_path)
}

fn content_hash(text: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn searchable_text(note: &IndexedNote) -> String {
    let path = Path::new(&note.relative_path)
        .with_extension("")
        .to_string_lossy()
        .replace('\\', "/");
    let mut text = format!("{path}\n{}", note.markdown);
    if text.chars().count() > MAX_EMBED_CHARS {
        text = text.chars().take(MAX_EMBED_CHARS).collect();
    }
    text
}

fn manifest_path(index_dir: &Path) -> PathBuf {
    index_dir.join("notes-hybrid-manifest.json")
}

fn load_manifest(index_dir: &Path) -> Manifest {
    let Ok(bytes) = std::fs::read(manifest_path(index_dir)) else {
        return Manifest::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn store_manifest(index_dir: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    std::fs::create_dir_all(index_dir)?;
    let bytes = serde_json::to_vec_pretty(manifest)?;
    std::fs::write(manifest_path(index_dir), bytes)?;
    Ok(())
}

async fn sync_index(
    index_dir: &Path,
    notes: &[IndexedNote],
    mode: &EmbedMode,
) -> anyhow::Result<RefreshStats> {
    let mut manifest = load_manifest(index_dir);
    if manifest.embedder != mode.name || manifest.dims != mode.dims {
        if index_dir.exists() {
            std::fs::remove_dir_all(index_dir).ok();
        }
        manifest = Manifest {
            embedder: mode.name.clone(),
            dims: mode.dims,
            notes: HashMap::new(),
        };
    }
    let mut live = HashSet::new();
    let mut changed = Vec::new();
    for note in notes {
        let id = note_id(note);
        let text = searchable_text(note);
        let hash = content_hash(&text);
        live.insert(id.clone());
        if manifest.notes.get(&id) != Some(&hash) {
            changed.push((id, hash, text, note));
        }
    }
    let removed = manifest
        .notes
        .keys()
        .filter(|id| !live.contains(*id))
        .cloned()
        .collect::<Vec<_>>();
    let removed_count = removed.len();
    if !removed.is_empty() {
        VectorTable::at(index_dir, mode.dims)
            .delete_ids(&removed)
            .await
            .context("deleting removed notes from the hybrid index")?;
        for id in &removed {
            manifest.notes.remove(id);
        }
    }
    let batch = changed
        .into_iter()
        .take(MAX_EMBED_NOTES_PER_SEARCH)
        .collect::<Vec<_>>();
    if !batch.is_empty() {
        let texts = batch
            .iter()
            .map(|(_, _, text, _)| text.clone())
            .collect::<Vec<_>>();
        let vectors = embed_texts(mode, &texts).await?;
        let items = batch
            .iter()
            .map(|(id, _, text, note)| VectorItem {
                id: id.clone(),
                text: text.clone(),
                metadata: json!({
                    "provider": note.provider,
                    "relative_path": note.relative_path,
                }),
            })
            .collect::<Vec<_>>();
        VectorTable::at(index_dir, mode.dims)
            .index_with_embeddings(&items, &vectors)
            .await
            .context("writing notes into the hybrid index")?;
        VectorTable::at(index_dir, mode.dims)
            .rebuild_fts()
            .await
            .context("refreshing the notes BM25 index")?;
        for (id, hash, _, _) in &batch {
            manifest.notes.insert(id.clone(), hash.clone());
        }
        store_manifest(index_dir, &manifest)?;
    } else if removed_count > 0 {
        VectorTable::at(index_dir, mode.dims)
            .rebuild_fts()
            .await
            .ok();
        store_manifest(index_dir, &manifest)?;
    }
    Ok(RefreshStats {
        changed: batch.len(),
        removed: removed_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embed_topic(texts: &[String]) -> Vec<Vec<f32>> {
        texts
            .iter()
            .map(|text| {
                let lowered = text.to_lowercase();
                let mut vector = vec![0f32; 4];
                if lowered.contains("orion") || lowered.contains("rocket program") {
                    vector[0] = 1.0;
                } else {
                    vector[1] = 1.0;
                }
                vector
            })
            .collect()
    }

    #[tokio::test]
    async fn watch_refreshes_after_an_outside_edit() {
        let temp = tempfile::tempdir().unwrap();
        let notes = temp.path().join("notes");
        std::fs::create_dir_all(notes.join("Inbox")).unwrap();
        let index = temp.path().join("hybrid-index");
        let roots = vec![NotesIndexRoot {
            provider: "silverbullet".into(),
            root: notes.clone(),
        }];
        ensure_notes_watch(index.clone(), roots);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let file = notes.join("Inbox/watch.md");
        std::fs::write(&file, "# Watch\n\nwatcher-token-one\n").unwrap();
        let id = "silverbullet\nInbox/watch.md";
        let mut first = String::new();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(hash) = load_manifest(&index).notes.get(id) {
                first = hash.clone();
                break;
            }
        }
        assert!(!first.is_empty(), "watcher did not index the new note");
        std::fs::write(&file, "# Watch\n\nwatcher-token-two\n").unwrap();
        let mut second = first.clone();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(hash) = load_manifest(&index).notes.get(id) {
                if hash != &first {
                    second = hash.clone();
                    break;
                }
            }
        }
        assert_ne!(second, first, "watcher did not pick up the edit");
    }

    #[tokio::test]
    async fn semantic_query_finds_a_note_that_does_not_share_its_words() {
        let temp = tempfile::tempdir().unwrap();
        let notes = vec![IndexedNote {
            provider: "local_markdown".into(),
            relative_path: "Inbox/orion.md".into(),
            absolute_path: temp.path().join("orion.md"),
            markdown: "# Project\n\nPROJECT-ORION".into(),
            modified_at_ms: 1,
        }];
        let hits = hybrid_admissions_with_embedder(
            temp.path().join("index"),
            &notes,
            "rocket program",
            5,
            embed_topic,
            4,
        )
        .await
        .unwrap();
        assert!(
            hits.iter()
                .any(|hit| hit.relative_path == "Inbox/orion.md" && hit.semantic),
            "vector side should admit the note, got {hits:?}"
        );
    }

    #[tokio::test]
    async fn stored_full_text_index_finds_a_note_without_a_memory_copy() {
        let temp = tempfile::tempdir().unwrap();
        let index = temp.path().join("index");
        let notes = vec![IndexedNote {
            provider: "local_markdown".into(),
            relative_path: "index.md".into(),
            absolute_path: temp.path().join("index.md"),
            markdown: "# Notes\n\nThe curated links above are convenience.".into(),
            modified_at_ms: 1,
        }];
        refresh_index(&index, &notes).await.unwrap();
        let hits = query_stored_index(&index, "curated links", 5)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|hit| hit.relative_path == "index.md"),
            "stored index should find the note, got {hits:?}"
        );
    }
}
