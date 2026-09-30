//! Memory corpus source: the owner-facing tiers of the scope's user memory.
//!
//! **Granularity — per tier, by allowlist.** The knowledge store holds both
//! things the owner would want back (a research finding, a contact, something
//! seen on screen) and the agent's own operating material — derived workflows,
//! an org model, inferred patterns. Only the first belongs in the owner's lane,
//! so the eligible tiers are named in `resurfacing.memory_tiers` rather than
//! being whatever the store happens to contain. This adapter previously mapped
//! every tier: measured against the live store, internal memory held every one
//! of the top 30 Worth-a-look slots by salience, outranking real communications
//! 0.72 to 0.45. `workflows` (481 entries) and `organization` (57) are the two
//! now excluded; `preferences` (138) shares their scale but is retained by
//! owner decision, so it stays eligible. An allowlist also fails closed — a
//! tier added later cannot reach the owner without being named.
//!
//! Reads the scope's user-knowledge store (`knowledge.json`, a JSON object
//! mapping each tier name → an array of entry objects) through
//! [`AgentMemoryResolver::resolve_for_scope`] +
//! [`AgentMemoryService::load_user_knowledge`], and yields one [`CorpusItem`]
//! per memory *entry* newer than the watermark.
//!
//! **Granularity — per entry.** The user-knowledge store keeps individually
//! timestamped entries: every entry carries its own `updated_at` RFC3339 stamp
//! (see `merge_user_memory_tier_fields` / `user_memory_fields_to_array` on the
//! write side, and the retention sweep that sorts entries by per-entry
//! `updated_at`). So the adapter works at entry granularity rather than
//! whole-tier blobs — a single new fact in a large tier resurfaces only that
//! fact, not the whole tier. Entries without a parseable `updated_at` are
//! skipped: without a stamp we cannot tell whether they changed since the
//! watermark, and re-emitting them on every scan would defeat the watermark.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::attention::resurfacing::types::{CorpusItem, SourceKind};

use super::ResurfacingSource;

/// Adapts the owner-facing tiers of a scope's user memory into corpus items.
#[derive(Debug, Clone)]
pub struct MemorySource {
    resolver: AgentMemoryResolver,
    /// Tiers permitted to reach the owner's lane. Empty means none: an
    /// unconfigured source must surface nothing rather than everything, since
    /// surfacing everything is the failure this allowlist exists to prevent.
    tiers: BTreeSet<String>,
}

impl MemorySource {
    /// Construct over the same `AgentMemoryResolver` the rest of the runtime
    /// uses to read scoped memory (e.g. `AgentMemoryResolver::new(base_root)`
    /// or `::with_workspace_layout(..)`), restricted to `tiers`
    /// (`resurfacing.memory_tiers`).
    pub fn new<I, S>(resolver: AgentMemoryResolver, tiers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            resolver,
            tiers: tiers.into_iter().map(Into::into).collect(),
        }
    }
}

#[async_trait]
impl ResurfacingSource for MemorySource {
    fn corpus_kind(&self) -> &'static str {
        // == SourceKind::Memory.as_str(); the store's watermark row keys on this.
        SourceKind::Memory.as_str()
    }

    async fn list_changed_since(
        &self,
        principal: &str,
        workspace: &str,
        watermark: i64,
    ) -> Result<Vec<CorpusItem>> {
        let service = self
            .resolver
            .resolve_for_scope(principal, workspace)
            .with_context(|| {
                format!("resolve memory scope {principal}/{workspace} for resurfacing")
            })?;
        let knowledge = service
            .load_user_knowledge()
            .await
            .with_context(|| format!("load user knowledge for {principal}/{workspace}"))?;
        Ok(map_knowledge_tiers(&knowledge, watermark, &self.tiers))
    }
}

/// Map a `knowledge.json` object (tier_name → array of entry objects) into
/// corpus items whose tier is in `allowed` and whose `occurred_at` (the entry's
/// `updated_at`, unix seconds) is strictly greater than `watermark`.
///
/// Pure over the loaded JSON so the tier→[`CorpusItem`] mapping is unit-testable
/// without seeding the store. Tiers outside `allowed`, non-array tier values (a
/// stray blob) and entries without a parseable `updated_at` are skipped.
fn map_knowledge_tiers(
    knowledge: &Value,
    watermark: i64,
    allowed: &BTreeSet<String>,
) -> Vec<CorpusItem> {
    let Some(tiers) = knowledge.as_object() else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for (tier, entries) in tiers {
        if !allowed.contains(tier.as_str()) {
            continue; // agent-internal or unrecognized tier: never the owner's
        }
        let Some(entries) = entries.as_array() else {
            continue; // non-array tier carries no per-entry stamps
        };
        for (idx, entry) in entries.iter().enumerate() {
            if crate::magician_v2::agents::memory_temperature::candidate_metadata_has_superseded_lifecycle(entry)
                || crate::magician_v2::agents::memory_lifecycle::state(entry)=="unresolved" {
                continue;
            }
            let Some(occurred_at) = entry_updated_at(entry) else {
                continue; // undatable entry: cannot compare against watermark
            };
            if occurred_at <= watermark {
                continue;
            }
            let key = entry_key(entry).unwrap_or_else(|| idx.to_string());
            let source_key = entry
                .get("memory_record_id")
                .and_then(Value::as_str)
                .map(|id| format!("record:{id}"))
                .unwrap_or_else(|| key.clone());
            let text = entry_text(entry);
            items.push(CorpusItem {
                source_kind: SourceKind::Memory,
                source_ref: format!("{tier}#{source_key}"),
                title: format!("{tier}: {key}"),
                digest: text.clone(),
                content_details: None,
                content_revision: None,
                occurred_at,
                watermark_cursor: occurred_at,
                embedding_text: text,
            });
        }
    }
    items
}

/// Parse an entry's `updated_at` RFC3339 stamp into unix seconds.
fn entry_updated_at(entry: &Value) -> Option<i64> {
    let raw = entry
        .get("memory_reconciled_at")
        .or_else(|| entry.get("updated_at"))
        .and_then(Value::as_str)?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Stable per-entry key, mirroring the write-side identity fields
/// (`key`/`source_id`/`name`/`id`). Falls back (via the caller) to the entry's
/// index when none is present.
fn entry_key(entry: &Value) -> Option<String> {
    for field in ["key", "source_id", "name", "id"] {
        if let Some(raw) = entry.get(field).and_then(Value::as_str) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Human-facing memory text: prefer the entry's `value` string; otherwise the
/// `value` rendered compactly, else the whole entry — so structured facts still
/// carry content into the digest / embedding text.
fn entry_text(entry: &Value) -> String {
    match entry.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => entry.to_string(),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn rfc3339_secs(s: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339")
            .timestamp()
    }

    fn allow<const N: usize>(tiers: [&str; N]) -> BTreeSet<String> {
        tiers.into_iter().map(str::to_string).collect()
    }

    #[test]
    fn memory_lifecycle_attention_ingests_activation_after_its_old_write_watermark() {
        let knowledge = json!({"preferences":[
            {"key":"city","value":"Delhi","memory_record_id":"old","memory_lifecycle":"superseded","updated_at":"2026-09-01T00:00:00Z"},
            {"key":"city","value":"Mumbai","memory_record_id":"current","memory_lifecycle":"active","updated_at":"2026-09-01T00:00:00Z","memory_reconciled_at":"2026-09-12T00:00:00Z"},
            {"key":"city","value":"Unknown","memory_record_id":"pending","memory_lifecycle":"pending_review","updated_at":"2026-09-12T00:00:00Z"}
        ]});
        let items = map_knowledge_tiers(
            &knowledge,
            rfc3339_secs("2026-09-10T00:00:00Z"),
            &allow(["preferences"]),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source_ref, "preferences#record:current");
        assert_eq!(items[0].digest, "Mumbai");
        assert!(!items[0].title.contains("record:"));
    }

    #[test]
    fn map_filters_by_watermark_and_builds_stable_ref() {
        let knowledge = json!({
            "knowledge": [
                { "key": "old_fact", "value": "an old fact", "updated_at": "2026-01-01T00:00:00Z" },
                { "key": "new_fact", "value": "a fresh fact", "updated_at": "2026-06-01T00:00:00Z" },
                { "key": "no_stamp", "value": "undatable" }
            ],
            // A non-array tier value must be ignored, not panic.
            "profile_blob": { "name": "not an array" }
        });
        let watermark = rfc3339_secs("2026-03-01T00:00:00Z");

        let items = map_knowledge_tiers(&knowledge, watermark, &allow(["knowledge"]));

        assert_eq!(items.len(), 1, "only the newer, stamped entry survives");
        let item = &items[0];
        assert_eq!(item.source_kind, SourceKind::Memory);
        assert_eq!(item.source_ref, "knowledge#new_fact");
        assert_eq!(item.title, "knowledge: new_fact");
        assert_eq!(item.embedding_text, "a fresh fact");
        assert_eq!(item.digest, "a fresh fact");
        assert!(item.occurred_at > watermark);
    }

    /// The owner's lane must not carry the agent's own operating material. In
    /// the live store these three tiers were 676 of 747 eligible entries and
    /// every one of the top 30 slots by salience.
    #[test]
    fn agent_internal_tiers_never_reach_the_owner() {
        let stamp = "2026-06-01T00:00:00Z";
        let entry = |k: &str| json!({ "key": k, "value": "v", "updated_at": stamp });
        let knowledge = json!({
            "workflows": [entry("gtm_evidence_quality_gate")],
            "organization": [entry("functional_ownership_roster")],
            "preferences": [entry("marketing_positioning")],
            "research_findings": [entry("a_finding")],
        });
        let watermark = rfc3339_secs("2026-01-01T00:00:00Z");

        let items = map_knowledge_tiers(
            &knowledge,
            watermark,
            &allow(["preferences", "research_findings"]),
        );

        let refs: Vec<&str> = items.iter().map(|i| i.source_ref.as_str()).collect();
        assert_eq!(
            refs,
            vec![
                "preferences#marketing_positioning",
                "research_findings#a_finding"
            ],
            "only allowlisted tiers map, regardless of how fresh the others are"
        );
    }

    /// Fail closed. An unconfigured allowlist surfacing everything would be the
    /// original defect, reachable by deleting one config key.
    #[test]
    fn an_empty_allowlist_surfaces_nothing_rather_than_everything() {
        let knowledge = json!({
            "workflows": [{ "key": "w", "value": "v", "updated_at": "2026-06-01T00:00:00Z" }],
            "knowledge": [{ "key": "k", "value": "v", "updated_at": "2026-06-01T00:00:00Z" }],
        });

        let items = map_knowledge_tiers(&knowledge, 0, &BTreeSet::new());

        assert!(items.is_empty(), "no tier is allowed, so nothing maps");
    }

    /// The tier literally named `user` is a JSON object, not an array, so it is
    /// skipped on shape. Naming it must not resurrect the unfiltered behaviour.
    #[test]
    fn a_non_array_tier_is_skipped_even_when_allowed() {
        let knowledge = json!({
            "user": { "email_evidence": ["not an entry array"] },
            "_meta": { "learning_promotions": 3 },
        });

        let items = map_knowledge_tiers(&knowledge, 0, &allow(["user", "_meta"]));

        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn list_changed_since_returns_only_newer_entry() {
        let tmp = tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());

        // Seed two user-knowledge entries with different `updated_at` stamps
        // through the real scoped store.
        let seed = resolver
            .resolve_for_scope("anonymous", "default")
            .expect("resolve scope");
        let knowledge = json!({
            "knowledge": [
                { "key": "older", "value": "older memory", "updated_at": "2026-02-01T00:00:00Z" },
                { "key": "newer", "value": "newer memory", "updated_at": "2026-05-01T00:00:00Z" }
            ]
        });
        seed.save_user_knowledge(&knowledge)
            .await
            .expect("seed user knowledge");

        let watermark = rfc3339_secs("2026-03-15T00:00:00Z");
        let source = MemorySource::new(resolver, ["knowledge"]);
        assert_eq!(source.corpus_kind(), "memory");

        let items = source
            .list_changed_since("anonymous", "default", watermark)
            .await
            .expect("list changed");

        assert_eq!(items.len(), 1, "only the entry newer than the watermark");
        assert_eq!(items[0].source_kind, SourceKind::Memory);
        assert_eq!(items[0].source_ref, "knowledge#newer");
        assert_eq!(items[0].embedding_text, "newer memory");
    }
}
