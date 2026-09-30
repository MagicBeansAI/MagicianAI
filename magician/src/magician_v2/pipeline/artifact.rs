//! # Pipeline Artifacts
//!
//! Typed artifact definitions and an in-memory artifact store for the
//! agent pipeline. Each agent stage produces artifacts that downstream
//! stages can consume via the [`ArtifactStore`].

use chrono::{DateTime, Utc};
use serde::{ser::SerializeStruct, Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// ArtifactType
// ---------------------------------------------------------------------------

/// Enumerates the kinds of artifacts that pipeline agents produce.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArtifactType {
    QueryAnalysis,
    IntentClassification,
    SlotGraph,
    ElicitationResult,
    InterpretedAnswer,
    ClarifiedTask,
    PlanGraph,
    AgentError,
    /// Escape-hatch for user-defined artifact kinds.
    Custom(String),
}

impl serde::Serialize for ArtifactType {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ArtifactType {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(raw.parse().unwrap()) // FromStr is infallible
    }
}

impl fmt::Display for ArtifactType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactType::QueryAnalysis => write!(f, "query_analysis"),
            ArtifactType::IntentClassification => write!(f, "intent_classification"),
            ArtifactType::SlotGraph => write!(f, "slot_graph"),
            ArtifactType::ElicitationResult => write!(f, "elicitation_result"),
            ArtifactType::InterpretedAnswer => write!(f, "interpreted_answer"),
            ArtifactType::ClarifiedTask => write!(f, "clarified_task"),
            ArtifactType::PlanGraph => write!(f, "plan_graph"),
            ArtifactType::AgentError => write!(f, "agent_error"),
            // Prefix custom types to avoid collision with known variant tokens.
            ArtifactType::Custom(s) => write!(f, "custom:{}", s),
        }
    }
}

impl FromStr for ArtifactType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "query_analysis" => ArtifactType::QueryAnalysis,
            "intent_classification" => ArtifactType::IntentClassification,
            "slot_graph" => ArtifactType::SlotGraph,
            "elicitation_result" => ArtifactType::ElicitationResult,
            "interpreted_answer" => ArtifactType::InterpretedAnswer,
            "clarified_task" => ArtifactType::ClarifiedTask,
            "plan_graph" => ArtifactType::PlanGraph,
            "agent_error" => ArtifactType::AgentError,
            // "custom:" prefix is canonical; also accept bare strings for
            // backwards compatibility with data written before the prefix was added.
            other => {
                ArtifactType::Custom(other.strip_prefix("custom:").unwrap_or(other).to_string())
            },
        })
    }
}

// ---------------------------------------------------------------------------
// AgentArtifact
// ---------------------------------------------------------------------------

/// Current schema version for all AgentArtifact instances.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;
pub const SURFACE_MANIFEST_CUSTOM_ARTIFACT_NAME: &str = "surface_manifest";

pub fn surface_manifest_artifact_type() -> ArtifactType {
    ArtifactType::Custom(SURFACE_MANIFEST_CUSTOM_ARTIFACT_NAME.to_string())
}

/// A single artifact produced by an agent during a pipeline cycle.
#[derive(Debug, Deserialize)]
pub struct AgentArtifact {
    pub artifact_id: String,
    pub artifact_type: ArtifactType,
    pub producer_agent_id: String,
    pub producer_cycle_id: String,
    pub content: serde_json::Value,
    pub schema_version: u32,
    pub produced_at: DateTime<Utc>,
    /// Optional render hints for auto-surface publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_hints: Option<crate::magician_v2::artifacts::types::RenderHints>,
}

impl Clone for AgentArtifact {
    fn clone(&self) -> Self {
        Self {
            artifact_id: self.artifact_id.clone(),
            artifact_type: self.artifact_type.clone(),
            producer_agent_id: self.producer_agent_id.clone(),
            producer_cycle_id: self.producer_cycle_id.clone(),
            content: crate::magician_v2::json_traversal::clone_json_iteratively(&self.content),
            schema_version: self.schema_version,
            produced_at: self.produced_at,
            render_hints: self.render_hints.clone(),
        }
    }
}

impl Drop for AgentArtifact {
    fn drop(&mut self) {
        crate::magician_v2::json_traversal::discard_json_iteratively(std::mem::replace(
            &mut self.content,
            serde_json::Value::Null,
        ));
    }
}

impl Serialize for AgentArtifact {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let metrics =
            crate::magician_v2::json_traversal::inspect_json_bounded(&self.content, 1_000_000)
                .ok_or_else(|| {
                    <S::Error as serde::ser::Error>::custom(
                        "artifact content exceeds 1000000 JSON nodes",
                    )
                })?;
        if metrics.max_depth > crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH {
            return Err(<S::Error as serde::ser::Error>::custom(format!(
                "artifact content exceeds JSON depth {}",
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH
            )));
        }

        let mut state = serializer.serialize_struct(
            "AgentArtifact",
            if self.render_hints.is_some() { 8 } else { 7 },
        )?;
        state.serialize_field("artifact_id", &self.artifact_id)?;
        state.serialize_field("artifact_type", &self.artifact_type)?;
        state.serialize_field("producer_agent_id", &self.producer_agent_id)?;
        state.serialize_field("producer_cycle_id", &self.producer_cycle_id)?;
        state.serialize_field("content", &self.content)?;
        state.serialize_field("schema_version", &self.schema_version)?;
        state.serialize_field("produced_at", &self.produced_at)?;
        if let Some(render_hints) = &self.render_hints {
            state.serialize_field("render_hints", render_hints)?;
        }
        state.end()
    }
}

impl AgentArtifact {
    /// Decode a typed projection after the shared retained-JSON admission
    /// contract, avoiding recursive `Value::clone` at every pipeline stage.
    pub fn deserialize_content<T>(&self) -> Result<T, serde_json::Error>
    where
        T: serde::de::DeserializeOwned,
    {
        crate::magician_v2::json_traversal::deserialize_json_bounded(
            &self.content,
            1_000_000,
            crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
        )
    }
}

// ---------------------------------------------------------------------------
// ArtifactStore
// ---------------------------------------------------------------------------

/// In-memory store that indexes artifacts by ID and by type.
///
/// # Invariant
/// `by_type[T]` contains exactly the IDs of all artifacts in `artifacts` whose
/// `artifact_type == T`, sorted ascending by `produced_at` so that
/// `ids.last()` is always the most recently produced artifact of that type.
/// This invariant must be maintained by all mutation paths (`put`,
/// `remove_all_of_type`) and is restored by `restore_from_snapshot` and the
/// custom `Deserialize` impl below.
#[derive(Debug, Serialize)]
pub struct ArtifactStore {
    artifacts: HashMap<String, AgentArtifact>,
    by_type: HashMap<ArtifactType, Vec<String>>,
    chain_id: String,
    #[serde(skip)]
    lifecycle_service: Option<Arc<crate::magician_v2::artifacts::service::LifecycleService>>,
    #[serde(skip)]
    lifecycle_ownership: crate::magician_v2::artifacts::types::OwnershipScope,
}

impl Clone for ArtifactStore {
    fn clone(&self) -> Self {
        Self {
            artifacts: self.snapshot(),
            by_type: self.by_type.clone(),
            chain_id: self.chain_id.clone(),
            lifecycle_service: self.lifecycle_service.clone(),
            lifecycle_ownership: self.lifecycle_ownership.clone(),
        }
    }
}

impl Drop for ArtifactStore {
    fn drop(&mut self) {
        // `AgentArtifact::content` can originate in a provider/tool response.
        // Never let ordinary recursive `Value` destruction run on whichever
        // executor or HTTP worker happens to release the store last.
        for artifact in self.artifacts.values_mut() {
            crate::magician_v2::json_traversal::discard_json_iteratively(std::mem::replace(
                &mut artifact.content,
                serde_json::Value::Null,
            ));
        }
    }
}

impl<'de> serde::Deserialize<'de> for ArtifactStore {
    /// Custom Deserialize that rebuilds `by_type` from `artifacts`.
    ///
    /// # Note on production persistence path
    /// The V3 execution-scoped pipeline store does not serialize an
    /// `ArtifactStore` directly. It persists a document that carries
    /// `{execution_id, artifacts, saved_at}` and reconstructs via
    /// `ArtifactStore::restore_from_snapshot`.
    /// This impl is the correct path for any code that round-trips an `ArtifactStore`
    /// through serde directly (e.g. `PlanningOutcome`, test fixtures).  Both paths
    /// rebuild `by_type` identically so the store is equivalent regardless of which
    /// reconstruction route is taken.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Deserialize artifacts and chain_id, then rebuild by_type entirely from
        // the artifacts map — identical to restore_from_snapshot.  Rebuilding
        // rather than re-sorting the serialised by_type eliminates any dangling
        // IDs (present in by_type but absent from artifacts) that could arise
        // from manual file edits or future schema migrations, and ensures both
        // restoration paths are strictly equivalent.
        // by_type is serialised by Serialize but intentionally omitted here:
        // the index is always rebuilt from artifacts so that dangling IDs
        // (in by_type but absent from artifacts) can never survive a round-trip.
        // serde ignores the field in JSON when it is absent from the struct.
        #[derive(serde::Deserialize)]
        struct Raw {
            artifacts: HashMap<String, AgentArtifact>,
            chain_id: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        // Rebuild by_type from artifacts (same logic as restore_from_snapshot).
        let mut by_type: HashMap<ArtifactType, Vec<String>> = HashMap::new();
        for (id, artifact) in &raw.artifacts {
            by_type
                .entry(artifact.artifact_type.clone())
                .or_default()
                .push(id.clone());
        }
        for ids in by_type.values_mut() {
            ids.sort_by_key(|id| raw.artifacts.get(id.as_str()).map(|a| a.produced_at));
        }
        Ok(Self {
            artifacts: raw.artifacts,
            by_type,
            chain_id: raw.chain_id,
            lifecycle_service: None,
            lifecycle_ownership: Default::default(),
        })
    }
}

impl ArtifactStore {
    fn discard_artifact(mut artifact: AgentArtifact) {
        crate::magician_v2::json_traversal::discard_json_iteratively(std::mem::replace(
            &mut artifact.content,
            serde_json::Value::Null,
        ));
    }

    /// Create a new empty store scoped to the given `chain_id`.
    pub fn new(chain_id: impl Into<String>) -> Self {
        Self {
            artifacts: HashMap::new(),
            by_type: HashMap::new(),
            chain_id: chain_id.into(),
            lifecycle_service: None,
            lifecycle_ownership: Default::default(),
        }
    }

    /// Set the lifecycle service for artifact registration.
    pub fn with_lifecycle(
        mut self,
        svc: Arc<crate::magician_v2::artifacts::service::LifecycleService>,
        ownership: crate::magician_v2::artifacts::types::OwnershipScope,
    ) -> Self {
        self.lifecycle_service = Some(svc);
        self.lifecycle_ownership = ownership;
        self
    }

    /// Returns the chain ID this store is scoped to.
    pub fn chain_id(&self) -> &str {
        &self.chain_id
    }

    /// Insert (or replace) an artifact.
    pub fn put(&mut self, artifact: AgentArtifact) {
        let id = artifact.artifact_id.clone();
        let artifact_type = artifact.artifact_type.clone();
        // If an artifact with the same ID already exists, remove its old type
        // mapping before inserting the replacement so by_type never holds
        // stale references (avoids phantom entries and pre/post-restore divergence).
        if let Some(old) = self.artifacts.insert(id.clone(), artifact) {
            let old_type = old.artifact_type.clone();
            if let Some(ids) = self.by_type.get_mut(&old_type) {
                ids.retain(|existing| existing != &id);
            }
            Self::discard_artifact(old);
        }
        let type_vec = self.by_type.entry(artifact_type).or_default();
        type_vec.push(id.clone());
        // Maintain the produced_at-sorted invariant: sort after every push so
        // that ids.last() is always the artifact with the greatest produced_at,
        // matching the guarantee established by restore_from_snapshot and the
        // custom Deserialize impl.  Vecs are typically 1-3 entries per type so
        // the sort cost is negligible.
        let artifacts = &self.artifacts;
        type_vec.sort_by_key(|id| artifacts.get(id.as_str()).map(|a| a.produced_at));

        // Register in lifecycle catalog (fire-and-forget).
        if let Some(ref svc) = self.lifecycle_service {
            crate::magician_v2::artifacts::bridge::register_pipeline(
                svc,
                &self.artifacts[&id],
                &self.chain_id,
                &self.lifecycle_ownership,
            );
        }
    }

    /// Look up an artifact by its ID.
    pub fn get(&self, id: &str) -> Option<&AgentArtifact> {
        self.artifacts.get(id)
    }

    /// Return the most recently inserted artifact of the given type, if any.
    ///
    /// Uses `by_type.last()` which is O(1) because `by_type` is maintained
    /// sorted ascending by `produced_at` in `put()` and `restore_from_snapshot()`.
    pub fn latest_of_type(&self, artifact_type: &ArtifactType) -> Option<&AgentArtifact> {
        self.by_type
            .get(artifact_type)?
            .last() // maintained sorted ascending by produced_at
            .and_then(|id| self.artifacts.get(id))
    }

    /// Return all artifacts of the given type sorted ascending by `produced_at`.
    /// (The `by_type` index is kept sorted by `put()`, `restore_from_snapshot`,
    /// and the custom `Deserialize` impl — insertion order is not preserved.)
    pub fn all_of_type(&self, artifact_type: &ArtifactType) -> Vec<&AgentArtifact> {
        self.by_type
            .get(artifact_type)
            .map(|ids| ids.iter().filter_map(|id| self.artifacts.get(id)).collect())
            .unwrap_or_default()
    }

    /// Number of artifacts currently stored.
    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    /// True when the store contains no artifacts.
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }

    /// Iterate over the retained artifacts without cloning their JSON bodies.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &AgentArtifact)> {
        self.artifacts.iter()
    }

    /// Clone the full artifact map as a snapshot. Artifact bodies use the
    /// shared heap-framed JSON copier so a programmatically deep value cannot
    /// recurse through `Value::clone` on the caller's stack.
    pub fn snapshot(&self) -> HashMap<String, AgentArtifact> {
        self.artifacts
            .iter()
            .map(|(id, artifact)| (id.clone(), artifact.clone()))
            .collect()
    }

    /// Rebuild an `ArtifactStore` from a previously captured snapshot,
    /// reconstructing the `by_type` index from the snapshot entries.
    ///
    /// The `by_type` index is sorted by `produced_at` so that
    /// `latest_of_type()` returns the most recently produced artifact —
    /// HashMap iteration order is non-deterministic, so explicit sorting is
    /// required for correctness after restoration.
    pub fn restore_from_snapshot(
        chain_id: String,
        snapshot: HashMap<String, AgentArtifact>,
    ) -> Self {
        let mut by_type: HashMap<ArtifactType, Vec<String>> = HashMap::new();
        for (id, artifact) in &snapshot {
            by_type
                .entry(artifact.artifact_type.clone())
                .or_default()
                .push(id.clone());
        }
        // Sort each type's ID list by produced_at (ascending) so that
        // `ids.last()` reliably points to the most recently produced artifact.
        for ids in by_type.values_mut() {
            ids.sort_by_key(|id| snapshot.get(id).map(|a| a.produced_at).unwrap_or_default());
        }
        Self {
            artifacts: snapshot,
            by_type,
            chain_id,
            lifecycle_service: None,
            lifecycle_ownership: Default::default(),
        }
    }

    /// Return the latest artifact of the given type with `produced_at` strictly
    /// after `after`. Used for causal freshness checks.
    pub fn latest_of_type_after(
        &self,
        artifact_type: &ArtifactType,
        after: DateTime<Utc>,
    ) -> Option<&AgentArtifact> {
        self.by_type
            .get(artifact_type)
            .into_iter()
            .flatten()
            .filter_map(|id| self.artifacts.get(id))
            .filter(|a| a.produced_at > after)
            .max_by_key(|a| a.produced_at)
    }

    /// Return the latest artifact of the given type with `produced_at >= since`.
    /// Used for `run_started_at` freshness checks.
    pub fn latest_of_type_since(
        &self,
        artifact_type: &ArtifactType,
        since: DateTime<Utc>,
    ) -> Option<&AgentArtifact> {
        self.by_type
            .get(artifact_type)
            .into_iter()
            .flatten()
            .filter_map(|id| self.artifacts.get(id))
            .filter(|a| a.produced_at >= since)
            .max_by_key(|a| a.produced_at)
    }

    /// Remove all artifacts of the given type from both the `artifacts` map
    /// and the `by_type` index.
    pub fn remove_all_of_type(&mut self, artifact_type: &ArtifactType) {
        if let Some(ids) = self.by_type.remove(artifact_type) {
            for id in ids {
                if let Some(artifact) = self.artifacts.remove(&id) {
                    Self::discard_artifact(artifact);
                }
            }
        }
    }

    /// Register a lifecycle reference from the current chain to the latest
    /// PlanGraph artifact.
    ///
    /// Fire-and-forget: errors are logged but not propagated.
    pub fn register_plan_graph_reference(&self, chain_id: &str) {
        if let (Some(ref svc), Some(plan_art)) = (
            &self.lifecycle_service,
            self.latest_of_type(&ArtifactType::PlanGraph),
        ) {
            let uid = format!("pipeline-{}-{}", chain_id, plan_art.artifact_id);
            let reference = crate::magician_v2::artifacts::types::ArtifactReference {
                referrer_id: format!("chain-{}", chain_id),
                referrer_type: "pipeline_chain".to_string(),
                established_at: chrono::Utc::now(),
            };
            if let Err(e) = svc.add_reference(&uid, reference) {
                tracing::debug!(error = %e, "failed to add plan graph reference (non-fatal)");
            }
        }
    }

    /// Like `latest_of_type`, but skips artifacts whose lifecycle state is not
    /// consumable (Stale, Expired, Quarantined, etc.).
    ///
    /// Falls back to `latest_of_type` when no lifecycle service is configured.
    pub fn latest_consumable_of_type(
        &self,
        artifact_type: &ArtifactType,
    ) -> Option<&AgentArtifact> {
        let candidate = self.latest_of_type(artifact_type)?;
        if let Some(ref svc) = self.lifecycle_service {
            let uid = format!("pipeline-{}-{}", self.chain_id, candidate.artifact_id);
            if !svc.is_consumable(&uid) {
                tracing::debug!(
                    artifact_id = %candidate.artifact_id,
                    artifact_type = %artifact_type,
                    "skipping non-consumable artifact (lifecycle state)"
                );
                return None;
            }
        }
        Some(candidate)
    }

    /// Mark all artifacts of the given types as stale in the lifecycle catalog.
    ///
    /// Fire-and-forget: errors are logged but not propagated.
    pub fn mark_stale_in_lifecycle(&self, artifact_types: &[ArtifactType], reason: &str) {
        let Some(ref svc) = self.lifecycle_service else {
            return;
        };
        for artifact_type in artifact_types {
            for art in self.all_of_type(artifact_type) {
                let uid = format!("pipeline-{}-{}", self.chain_id, art.artifact_id);
                if let Err(e) = svc.mark_stale(&uid) {
                    tracing::debug!(uid = %uid, error = %e, "mark_stale_in_lifecycle: {reason} (non-fatal)");
                }
            }
        }
    }

    /// Remove all artifacts from the store.
    pub fn clear_all(&mut self) {
        for (_, artifact) in self.artifacts.drain() {
            Self::discard_artifact(artifact);
        }
        self.by_type.clear();
    }

    /// Remove a single artifact by ID from both the `artifacts` map and the
    /// `by_type` index.
    ///
    /// No-op if the ID is not present.
    pub fn remove(&mut self, artifact_id: &str) {
        if let Some(artifact) = self.artifacts.remove(artifact_id) {
            let artifact_type = artifact.artifact_type.clone();
            if let Some(ids) = self.by_type.get_mut(&artifact_type) {
                ids.retain(|id| id != artifact_id);
                if ids.is_empty() {
                    self.by_type.remove(&artifact_type);
                }
            }
            Self::discard_artifact(artifact);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;

    /// Helper: build a minimal artifact for testing.
    fn make_artifact(id: &str, artifact_type: ArtifactType) -> AgentArtifact {
        AgentArtifact {
            artifact_id: id.to_string(),
            artifact_type,
            producer_agent_id: "test-agent".to_string(),
            producer_cycle_id: "cycle-1".to_string(),
            content: json!({"hello": "world"}),
            schema_version: 1,
            produced_at: Utc::now(),
            render_hints: None,
        }
    }

    #[test]
    fn artifact_type_display_roundtrip() {
        let variants = vec![
            ArtifactType::QueryAnalysis,
            ArtifactType::IntentClassification,
            ArtifactType::SlotGraph,
            ArtifactType::ElicitationResult,
            ArtifactType::InterpretedAnswer,
            ArtifactType::ClarifiedTask,
            ArtifactType::PlanGraph,
            ArtifactType::AgentError,
        ];

        for variant in variants {
            let s = variant.to_string();
            let parsed: ArtifactType = s.parse().expect("infallible");
            assert_eq!(variant, parsed, "roundtrip failed for {s}");
        }
    }

    #[test]
    fn artifact_type_custom_display_roundtrip() {
        let custom = ArtifactType::Custom("my_type".to_string());
        let s = custom.to_string();
        // Custom types are now serialised with a "custom:" prefix to prevent
        // collision with known variant tokens (e.g. "query_analysis").
        assert_eq!(s, "custom:my_type");
        let parsed: ArtifactType = s.parse().expect("infallible");
        assert_eq!(parsed, ArtifactType::Custom("my_type".to_string()));

        // Backwards compatibility: bare strings without "custom:" prefix
        // should still deserialise as Custom.
        let legacy: ArtifactType = "my_type".parse().expect("infallible");
        assert_eq!(legacy, ArtifactType::Custom("my_type".to_string()));
    }

    #[test]
    fn agent_artifact_serde_roundtrip() {
        let artifact = make_artifact("a-1", ArtifactType::SlotGraph);
        let json_str = serde_json::to_string(&artifact).expect("serialize");
        let recovered: AgentArtifact = serde_json::from_str(&json_str).expect("deserialize");

        assert_eq!(artifact.artifact_id, recovered.artifact_id);
        assert_eq!(artifact.artifact_type, recovered.artifact_type);
        assert_eq!(artifact.producer_agent_id, recovered.producer_agent_id);
        assert_eq!(artifact.producer_cycle_id, recovered.producer_cycle_id);
        assert_eq!(artifact.content, recovered.content);
        assert_eq!(artifact.schema_version, recovered.schema_version);
        assert_eq!(artifact.produced_at, recovered.produced_at);
    }

    #[test]
    fn store_put_then_get() {
        let mut store = ArtifactStore::new("chain-1");
        let artifact = make_artifact("a-1", ArtifactType::QueryAnalysis);
        store.put(artifact.clone());

        let got = store.get("a-1").expect("should exist");
        assert_eq!(got.artifact_id, "a-1");
        assert_eq!(got.artifact_type, ArtifactType::QueryAnalysis);
    }

    #[test]
    fn store_put_same_id_type_change() {
        // Re-inserting the same ID with a different type must move the ID from
        // the old type's by_type list to the new type's list, leaving len() == 1.
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::milliseconds(10);

        let mut store = ArtifactStore::new("chain-retype");
        store.put(make_artifact_at("a-1", ArtifactType::QueryAnalysis, t1));
        // Re-insert same ID but reclassified as SlotGraph.
        store.put(make_artifact_at("a-1", ArtifactType::SlotGraph, t2));

        assert_eq!(store.len(), 1, "store must have exactly one artifact");
        // Old type bucket must be empty.
        assert!(store.latest_of_type(&ArtifactType::QueryAnalysis).is_none());
        // New type bucket must have exactly the reclassified artifact.
        let slot = store
            .latest_of_type(&ArtifactType::SlotGraph)
            .expect("should exist under new type");
        assert_eq!(slot.artifact_id, "a-1");
        assert_eq!(slot.produced_at, t2, "replacement value must be stored");
    }

    #[test]
    fn store_put_same_id_same_type_no_duplicate() {
        // Re-inserting an artifact with the same ID and same type must NOT create
        // a duplicate entry in the by_type index. After two puts of "a-1" as
        // QueryAnalysis, there must be exactly one entry: all_of_type returns [a-1]
        // and len() == 1.
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::milliseconds(10);

        let mut store = ArtifactStore::new("chain-dedup");
        store.put(make_artifact_at("a-1", ArtifactType::QueryAnalysis, t1));
        store.put(make_artifact_at("a-1", ArtifactType::QueryAnalysis, t2));

        assert_eq!(store.len(), 1, "store must have exactly one artifact");
        let all = store.all_of_type(&ArtifactType::QueryAnalysis);
        assert_eq!(all.len(), 1, "by_type must not contain duplicate IDs");
        assert_eq!(all[0].artifact_id, "a-1");
        // The replacement value (t2) must be the one stored.
        assert_eq!(all[0].produced_at, t2);
    }

    #[test]
    fn store_latest_of_type() {
        // Use explicit distinct timestamps so the test validates produced_at
        // ordering, not just insertion order.
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::milliseconds(50);

        let mut store = ArtifactStore::new("chain-1");
        store.put(make_artifact_at("a-1", ArtifactType::SlotGraph, t1));
        store.put(make_artifact_at("a-2", ArtifactType::SlotGraph, t2));

        let latest = store
            .latest_of_type(&ArtifactType::SlotGraph)
            .expect("should exist");
        assert_eq!(latest.artifact_id, "a-2");
    }

    #[test]
    fn store_all_of_type() {
        // Use explicit distinct timestamps so the test validates produced_at
        // ordering, not just insertion order.
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::milliseconds(50);
        let t3 = t1 + chrono::Duration::milliseconds(25);

        let mut store = ArtifactStore::new("chain-1");
        store.put(make_artifact_at("a-1", ArtifactType::PlanGraph, t1));
        store.put(make_artifact_at("a-2", ArtifactType::PlanGraph, t2));
        store.put(make_artifact_at(
            "a-3",
            ArtifactType::Custom("observation".to_string()),
            t3,
        ));

        let all = store.all_of_type(&ArtifactType::PlanGraph);
        assert_eq!(all.len(), 2);
        // Results are sorted ascending by produced_at.
        assert_eq!(all[0].artifact_id, "a-1"); // t1 < t2
        assert_eq!(all[1].artifact_id, "a-2");
        // Different artifact kinds live in separate buckets.
        assert_eq!(
            store
                .all_of_type(&ArtifactType::Custom("observation".to_string()))
                .len(),
            1
        );
    }

    #[test]
    fn store_get_nonexistent() {
        let store = ArtifactStore::new("chain-1");
        assert!(store.get("does-not-exist").is_none());
    }

    #[test]
    fn store_empty_latest_of_type() {
        let store = ArtifactStore::new("chain-1");
        assert!(store.latest_of_type(&ArtifactType::QueryAnalysis).is_none());
    }

    #[test]
    fn store_is_empty() {
        let mut store = ArtifactStore::new("chain-1");
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);

        store.put(make_artifact(
            "a-1",
            ArtifactType::Custom("observation".to_string()),
        ));
        assert!(!store.is_empty());
        assert_eq!(store.len(), 1);
    }

    // -----------------------------------------------------------------------
    // P5.5-B-02 — AgentError, snapshot, freshness, remove
    // -----------------------------------------------------------------------

    /// Helper: build an artifact with a specific timestamp.
    fn make_artifact_at(
        id: &str,
        artifact_type: ArtifactType,
        produced_at: DateTime<Utc>,
    ) -> AgentArtifact {
        AgentArtifact {
            artifact_id: id.to_string(),
            artifact_type,
            producer_agent_id: "test-agent".to_string(),
            producer_cycle_id: "cycle-1".to_string(),
            content: json!({"hello": "world"}),
            schema_version: 1,
            produced_at,
            render_hints: None,
        }
    }

    #[test]
    fn agent_error_display_roundtrip() {
        let variant = ArtifactType::AgentError;
        let s = variant.to_string();
        assert_eq!(s, "agent_error");
        let parsed: ArtifactType = s.parse().expect("infallible");
        assert_eq!(parsed, ArtifactType::AgentError);
    }

    #[test]
    fn snapshot_captures_all_artifacts() {
        let mut store = ArtifactStore::new("chain-snap");
        store.put(make_artifact("a-1", ArtifactType::QueryAnalysis));
        store.put(make_artifact("a-2", ArtifactType::SlotGraph));
        store.put(make_artifact("a-3", ArtifactType::PlanGraph));

        let snap = store.snapshot();
        assert_eq!(snap.len(), 3);
        assert!(snap.contains_key("a-1"));
        assert!(snap.contains_key("a-2"));
        assert!(snap.contains_key("a-3"));
    }

    #[test]
    fn store_clone_and_drop_keep_deep_artifact_traversal_off_the_native_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut content = serde_json::Value::Null;
                for _ in 0..10_000 {
                    content = serde_json::Value::Array(vec![content]);
                }
                let mut store = ArtifactStore::new("deep-chain");
                store.put(AgentArtifact {
                    artifact_id: "deep".to_string(),
                    artifact_type: ArtifactType::Custom("deep".to_string()),
                    producer_agent_id: "test-agent".to_string(),
                    producer_cycle_id: "cycle-1".to_string(),
                    content,
                    schema_version: 1,
                    produced_at: Utc::now(),
                    render_hints: None,
                });

                let cloned = store.clone();
                assert_eq!(
                    crate::magician_v2::json_traversal::inspect_json(
                        &cloned.get("deep").unwrap().content,
                    )
                    .max_depth,
                    10_000
                );
                assert!(cloned
                    .get("deep")
                    .unwrap()
                    .deserialize_content::<serde_json::Value>()
                    .is_err());
                assert!(serde_json::to_vec(cloned.get("deep").unwrap()).is_err());
                drop(cloned);
                drop(store);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn restore_from_snapshot_rebuilds_indexes() {
        // Use explicit, distinct timestamps so restore_from_snapshot's sort is
        // deterministic on fast hardware where Utc::now() can return the same
        // value for two consecutive calls.
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::milliseconds(50);
        let t3 = t2 + chrono::Duration::milliseconds(50);

        let mut store = ArtifactStore::new("chain-orig");
        store.put(make_artifact_at("a-1", ArtifactType::SlotGraph, t1));
        store.put(make_artifact_at("a-2", ArtifactType::SlotGraph, t2));
        store.put(make_artifact_at("a-3", ArtifactType::QueryAnalysis, t3));

        let snap = store.snapshot();
        let restored = ArtifactStore::restore_from_snapshot("chain-restored".to_string(), snap);

        // latest_of_type should return a-2 (has the later timestamp among SlotGraph)
        let latest_sg = restored
            .latest_of_type(&ArtifactType::SlotGraph)
            .expect("should find SlotGraph");
        assert_eq!(
            latest_sg.artifact_id, "a-2",
            "a-2 must be latest SlotGraph (t2 > t1)"
        );

        // all_of_type ordering: restored by_type vecs are sorted by produced_at
        let all_sg = restored.all_of_type(&ArtifactType::SlotGraph);
        assert_eq!(all_sg.len(), 2);
        assert_eq!(
            all_sg[0].artifact_id, "a-1",
            "a-1 should be first (earlier timestamp)"
        );
        assert_eq!(
            all_sg[1].artifact_id, "a-2",
            "a-2 should be second (later timestamp)"
        );

        let latest_qa = restored
            .latest_of_type(&ArtifactType::QueryAnalysis)
            .expect("should find QueryAnalysis");
        assert_eq!(latest_qa.artifact_id, "a-3");

        assert_eq!(restored.len(), 3);
        assert_eq!(restored.chain_id(), "chain-restored");
    }

    #[test]
    fn latest_of_type_after_filters_by_timestamp() {
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::milliseconds(100);

        let mut store = ArtifactStore::new("chain-after");
        store.put(make_artifact_at("a-1", ArtifactType::SlotGraph, t1));
        store.put(make_artifact_at("a-2", ArtifactType::SlotGraph, t2));

        // after(t1) should return only a-2 (strictly greater than t1)
        let result = store
            .latest_of_type_after(&ArtifactType::SlotGraph, t1)
            .expect("should find a-2");
        assert_eq!(result.artifact_id, "a-2");

        // after(t2) should return None (nothing strictly after t2)
        assert!(store
            .latest_of_type_after(&ArtifactType::SlotGraph, t2)
            .is_none());
    }

    #[test]
    fn latest_of_type_after_returns_none_when_no_fresh() {
        let t1 = Utc::now();

        let mut store = ArtifactStore::new("chain-after-none");
        store.put(make_artifact_at("a-1", ArtifactType::SlotGraph, t1));

        // after(t1) with only one artifact at t1 → None
        assert!(store
            .latest_of_type_after(&ArtifactType::SlotGraph, t1)
            .is_none());
    }

    #[test]
    fn latest_of_type_since_includes_exact_match() {
        let t1 = Utc::now();

        let mut store = ArtifactStore::new("chain-since-exact");
        store.put(make_artifact_at("a-1", ArtifactType::SlotGraph, t1));

        // since(t1) should return a-1 (>= semantics)
        let result = store
            .latest_of_type_since(&ArtifactType::SlotGraph, t1)
            .expect("should find a-1 with >= semantics");
        assert_eq!(result.artifact_id, "a-1");

        // since(t1 + 1ms) should return None
        let t1_plus = t1 + chrono::Duration::milliseconds(1);
        assert!(store
            .latest_of_type_since(&ArtifactType::SlotGraph, t1_plus)
            .is_none());
    }

    #[test]
    fn latest_of_type_since_returns_latest() {
        let t1 = Utc::now();
        let t2 = t1 + chrono::Duration::milliseconds(100);

        let mut store = ArtifactStore::new("chain-since-latest");
        store.put(make_artifact_at("a-1", ArtifactType::SlotGraph, t1));
        store.put(make_artifact_at("a-2", ArtifactType::SlotGraph, t2));

        // since(t1) should return a-2 (the latest of the two)
        let result = store
            .latest_of_type_since(&ArtifactType::SlotGraph, t1)
            .expect("should find a-2 as latest");
        assert_eq!(result.artifact_id, "a-2");
    }

    #[test]
    fn remove_all_of_type_clears_matching_artifacts() {
        let mut store = ArtifactStore::new("chain-remove");
        store.put(make_artifact("sg-1", ArtifactType::SlotGraph));
        store.put(make_artifact("sg-2", ArtifactType::SlotGraph));
        store.put(make_artifact("sg-3", ArtifactType::SlotGraph));
        store.put(make_artifact("qa-1", ArtifactType::QueryAnalysis));

        assert_eq!(store.len(), 4);

        store.remove_all_of_type(&ArtifactType::SlotGraph);

        assert_eq!(store.all_of_type(&ArtifactType::SlotGraph).len(), 0);
        assert_eq!(store.all_of_type(&ArtifactType::QueryAnalysis).len(), 1);
        assert_eq!(store.len(), 1);
        assert_eq!(
            store
                .get("qa-1")
                .expect("QueryAnalysis should still exist")
                .artifact_id,
            "qa-1"
        );
    }

    #[test]
    fn remove_all_of_type_noop_when_empty() {
        let mut store = ArtifactStore::new("chain-remove-noop");
        store.put(make_artifact("qa-1", ArtifactType::QueryAnalysis));
        store.put(make_artifact("sg-1", ArtifactType::SlotGraph));

        let len_before = store.len();

        // Remove a type that has no artifacts — should not error or change store
        store.remove_all_of_type(&ArtifactType::PlanGraph);

        assert_eq!(store.len(), len_before);
        assert!(store.get("qa-1").is_some());
        assert!(store.get("sg-1").is_some());
    }

    #[test]
    fn custom_artifact_type_serde_roundtrip() {
        // Custom variant serializes with the "custom:" prefix (added by Display impl).
        let custom = ArtifactType::Custom("my_type".to_string());
        let json = serde_json::to_string(&custom).expect("serialize");
        assert_eq!(json, r#""custom:my_type""#);
        // Deserialize strips the prefix: Custom("custom:my_type") → Custom("my_type").
        let back: ArtifactType = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(back, ArtifactType::Custom(s) if s == "my_type"));

        // Known variants should NOT deserialize as Custom
        let slot_graph_json = r#""slot_graph""#;
        let deserialized: ArtifactType =
            serde_json::from_str(slot_graph_json).expect("deserialize known");
        assert!(
            matches!(deserialized, ArtifactType::SlotGraph),
            "slot_graph must not become Custom"
        );
    }

    #[test]
    fn surface_manifest_helper_returns_canonical_custom_type() {
        let artifact_type = surface_manifest_artifact_type();
        assert_eq!(
            artifact_type,
            ArtifactType::Custom(SURFACE_MANIFEST_CUSTOM_ARTIFACT_NAME.to_string())
        );
        assert_eq!(artifact_type.to_string(), "custom:surface_manifest");
    }

    #[test]
    fn all_known_artifact_type_variants_serde_roundtrip() {
        // Verify every named variant serialises to its snake_case string and
        // deserialises back to the same variant (not Custom).
        let known_variants: &[(&str, ArtifactType)] = &[
            ("slot_graph", ArtifactType::SlotGraph),
            ("query_analysis", ArtifactType::QueryAnalysis),
            ("intent_classification", ArtifactType::IntentClassification),
            ("elicitation_result", ArtifactType::ElicitationResult),
            ("clarified_task", ArtifactType::ClarifiedTask),
            ("plan_graph", ArtifactType::PlanGraph),
            ("interpreted_answer", ArtifactType::InterpretedAnswer),
            ("agent_error", ArtifactType::AgentError),
        ];
        for (expected_str, variant) in known_variants {
            let json = serde_json::to_string(variant).unwrap();
            assert_eq!(
                json,
                format!(r#""{expected_str}""#),
                "variant {expected_str} serialization mismatch"
            );
            let back: ArtifactType = serde_json::from_str(&json).unwrap();
            assert_eq!(
                std::mem::discriminant(&back),
                std::mem::discriminant(variant),
                "variant {expected_str} deserialization mismatch"
            );
            // Must NOT deserialise as Custom
            assert!(
                !matches!(back, ArtifactType::Custom(_)),
                "known variant {expected_str} must not deserialize as Custom"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Freshness-aware retrieval tests
    // -----------------------------------------------------------------------

    #[test]
    fn latest_consumable_returns_artifact_without_lifecycle_service() {
        let mut store = ArtifactStore::new("chain-consumable-no-svc");
        store.put(make_artifact("pg-1", ArtifactType::PlanGraph));

        // No lifecycle service — should behave like latest_of_type
        let result = store.latest_consumable_of_type(&ArtifactType::PlanGraph);
        assert!(result.is_some());
        assert_eq!(result.unwrap().artifact_id, "pg-1");
    }

    #[test]
    fn latest_consumable_returns_artifact_when_active() {
        use crate::magician_v2::artifacts::service::LifecycleService;
        use crate::magician_v2::artifacts::types::OwnershipScope;

        let svc = Arc::new(LifecycleService::new());
        let mut store = ArtifactStore::new("chain-consumable-active")
            .with_lifecycle(Arc::clone(&svc), OwnershipScope::default());
        // put() auto-registers in the lifecycle catalog via the bridge
        store.put(make_artifact("pg-1", ArtifactType::PlanGraph));

        let result = store.latest_consumable_of_type(&ArtifactType::PlanGraph);
        assert!(result.is_some());
        assert_eq!(result.unwrap().artifact_id, "pg-1");
    }

    #[test]
    fn latest_consumable_returns_none_when_stale() {
        use crate::magician_v2::artifacts::service::LifecycleService;
        use crate::magician_v2::artifacts::types::OwnershipScope;

        let svc = Arc::new(LifecycleService::new());
        let mut store = ArtifactStore::new("chain-consumable-stale")
            .with_lifecycle(Arc::clone(&svc), OwnershipScope::default());
        // put() auto-registers in the lifecycle catalog via the bridge
        store.put(make_artifact("pg-1", ArtifactType::PlanGraph));

        // Mark stale after auto-registration
        svc.mark_stale("pipeline-chain-consumable-stale-pg-1")
            .unwrap();

        let result = store.latest_consumable_of_type(&ArtifactType::PlanGraph);
        assert!(result.is_none(), "stale artifact should not be consumable");
    }
}
