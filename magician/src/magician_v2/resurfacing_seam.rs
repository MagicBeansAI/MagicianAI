//! The resurfacing seam: the DTO vocabulary, wake handle, and vector math the
//! lib's content-source observation runtime needs, plus the sink trait the
//! resurfacing store implements.
//!
//! Plan workstream 3.0 moved the engine itself lib-side
//! (`magician_v2::attention::resurfacing`), so the store's `ResurfacingSink`
//! impl is now in-crate. The seam vocabulary is unchanged and remains the
//! stable surface `magician-comms` re-exports; comms-coupled adapters (the
//! comms corpus source, curation, actions, interaction) still approach the
//! engine only through it and the lib-side traits.

use std::sync::Arc;

use tokio::sync::Notify;

use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Stable candidate identifier: a hex blake3 hash of `"{kind}:{source_ref}"`.
/// Deterministic across runs so re-scans update the same row, and the source
/// kind participates so the same ref from two substrates stays distinct.
pub fn candidate_id(kind: SourceKind, source_ref: &str) -> String {
    blake3::hash(format!("{kind}:{source_ref}").as_bytes())
        .to_hex()
        .to_string()
}

/// The durable resurfacing candidate. Mirrors the future SQLite store row;
/// later tasks read/write this via the store.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub candidate_id: String,
    pub source_kind: SourceKind,
    pub source_ref: String,
    pub title: String,
    pub content_digest: String,
    pub content_details: Option<ResurfacingContentDetails>,
    pub content_revision: Option<String>,
    /// Revision-bound structured attention semantics. The envelope carries
    /// schema/prompt/model/profile identity and its exact source revision.
    /// Raw/rendered content is never stored here.
    pub semantic_features: Option<serde_json::Value>,
    pub salience_score: f32,
    pub signals: SalienceSignals,
    /// Closest supported temporal anchor in epoch milliseconds. Date-only
    /// values are UTC date markers and must not be treated as reminder times.
    pub temporal_anchor_at: Option<i64>,
    pub embedding_id: Option<String>,
    pub state: CandidateState,
    pub first_seen_at: i64,
    pub last_scored_at: i64,
    pub last_surfaced_at: Option<i64>,
    pub cooldown_until: i64,
    pub surface_count: u32,
    pub dismiss_count: u32,
}

/// Lifecycle of a candidate as it moves from freshly scored to surfaced and
/// through the owner's feedback. Persisted by the (later) SQLite store as a
/// lowercase string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateState {
    Candidate,
    Surfaced,
    Acted,
    Dismissed,
    Snoozed,
}

/// Versioned, provider-neutral details that can safely cross from a source
/// adapter into resurfacing persistence and broad list reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResurfacingContentDetails {
    pub schema_version: u32,
    #[serde(default)]
    pub key_facts: Vec<String>,
    #[serde(default)]
    pub changes: Vec<ResurfacingChangeFact>,
    #[serde(default)]
    pub temporal_facts: Vec<ResurfacingTemporalFact>,
    #[serde(default)]
    pub detail_status: ResurfacingDetailStatus,
    #[serde(default)]
    pub missing_details: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResurfacingDetailStatus {
    Complete,
    #[default]
    Partial,
    SourceOmitsDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResurfacingTemporalFact {
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// The salience signal bundle feeding the scorer. Each field is a normalized
/// contribution; persisted as JSON alongside the candidate so scoring is
/// auditable after the fact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SalienceSignals {
    pub recency: f32,
    pub frequency: f32,
    pub centrality: f32,
    pub cooccurrence: f32,
    pub temporal_anchor: f32,
    pub dormancy: f32,
    /// Explicit owner interest in the source or a subscription-intent match.
    /// Native resurfacing corpus scanners leave this at zero; observed web
    /// sources use it so a deliberate subscription is not misclassified as a
    /// recency-only signal.
    #[serde(default)]
    pub source_affinity: f32,
}

/// Origin substrate a resurfacing candidate was scanned from. Participates in
/// the candidate id so the same `source_ref` seen through two lanes stays
/// distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Memory,
    Task,
    Episode,
    Comm,
    Calendar,
    Note,
    Web,
}

/// Cheap cross-pipeline signal used when another durable producer admits new
/// resurfacing candidates. It avoids coupling that producer to curator internals
/// while ensuring fresh candidates are reviewed promptly.
#[derive(Clone, Default)]
pub struct ResurfacingWakeHandle {
    pub notify: Option<Arc<Notify>>,
}

impl ResurfacingWakeHandle {
    pub fn wake(&self) {
        if let Some(notify) = &self.notify {
            notify.notify_one();
        }
    }
}

/// L2-normalize a vector in place-then-return. A zero vector is returned
/// unchanged (its norm is `0`, so no division happens). Shared with the store's
/// dismiss neighbor-suppression so both cosine paths normalize identically.
pub fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if norm.is_finite() && norm > 0.0 {
        for x in &mut v {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
    v
}

/// Cosine similarity of two L2-normalized vectors (a plain dot product).
/// Mismatched lengths score `0.0` rather than panicking. Shared with the store's
/// dismiss neighbor-suppression (which pairs it with [`normalize`]).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The one operation the observation runtime performs on the resurfacing
/// engine: upsert a candidate. Implemented by the comms crate's
/// `ResurfacingStore`; test doubles implement it in-memory.
#[async_trait::async_trait]
pub trait ResurfacingSink: Send + Sync {
    async fn upsert_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate: &Candidate,
    ) -> anyhow::Result<()>;

    async fn get_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> anyhow::Result<Option<Candidate>>;
}

/// In-memory sink for lib tests of the observation pipeline.
#[cfg(any(test, feature = "test-fixtures"))]
pub struct FakeResurfacingSink {
    store: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, Candidate>>>,
}

#[cfg(any(test, feature = "test-fixtures"))]
impl FakeResurfacingSink {
    pub fn new() -> Self {
        Self {
            store: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
impl Clone for FakeResurfacingSink {
    fn clone(&self) -> Self {
        Self {
            store: std::sync::Arc::clone(&self.store),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
#[async_trait::async_trait]
impl ResurfacingSink for FakeResurfacingSink {
    async fn upsert_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate: &Candidate,
    ) -> anyhow::Result<()> {
        self.store.lock().await.insert(
            format!("{principal}/{workspace}/{}", candidate.candidate_id),
            candidate.clone(),
        );
        Ok(())
    }
    async fn get_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> anyhow::Result<Option<Candidate>> {
        Ok(self
            .store
            .lock()
            .await
            .get(&format!("{principal}/{workspace}/{candidate_id}"))
            .cloned())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResurfacingChangeFact {
    pub aspect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_text: Option<String>,
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Task => "task",
            Self::Episode => "episode",
            Self::Comm => "comm",
            Self::Calendar => "calendar",
            Self::Note => "note",
            Self::Web => "web",
        }
    }
}

impl std::fmt::Display for CandidateState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CandidateState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Surfaced => "surfaced",
            Self::Acted => "acted",
            Self::Dismissed => "dismissed",
            Self::Snoozed => "snoozed",
        }
    }
}

impl FromStr for SourceKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "memory" => Ok(Self::Memory),
            "task" => Ok(Self::Task),
            "episode" => Ok(Self::Episode),
            "comm" => Ok(Self::Comm),
            "calendar" => Ok(Self::Calendar),
            "note" => Ok(Self::Note),
            "web" => Ok(Self::Web),
            other => Err(format!("unknown SourceKind: {other}")),
        }
    }
}

impl FromStr for CandidateState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "candidate" => Ok(Self::Candidate),
            "surfaced" => Ok(Self::Surfaced),
            "acted" => Ok(Self::Acted),
            "dismissed" => Ok(Self::Dismissed),
            "snoozed" => Ok(Self::Snoozed),
            other => Err(format!("unknown CandidateState: {other}")),
        }
    }
}
