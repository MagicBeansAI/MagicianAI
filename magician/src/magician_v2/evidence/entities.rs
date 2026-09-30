//! Domain-agnostic entity anchors for the work-evidence graph (Slice 2).
//!
//! Entities are the canonical "who/what" referenced across evidence — projects,
//! PRs, tickets, docs, repos, people. They are **derived deterministically** from
//! the `entity_keys` / `people_keys` the distiller already emits on each
//! [`EvidenceRecord`]: a key is a `type:slug` pair (`project:onboarding-migration`,
//! `person:alex-pm`), which gives the type and a humanizable canonical name with
//! no extra LLM call.
//!
//! Resolution is **conservative + reversible**: candidates merge into an existing
//! entity only on an exact key/alias match (no fuzzy merge → no fragmentation,
//! no spurious merges). A user can then [`merge_entities`] two anchors that the
//! deterministic pass kept apart, and [`split_entity`] to undo it — merges are
//! durable (re-distillation routes an absorbed key to its canonical) and
//! reversible (the absorbed record is tombstoned with `merged_into`, not lost).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{EvidenceProposal, EvidenceRecord, EvidenceStatus, Facet};

/// An LLM-proposed entity, emitted by the distiller alongside the bare
/// `entity_keys`. Carries a cleaner canonical name + aliases than the
/// key-derived defaults. All fields beyond the key are optional — a model that
/// only emits keys still works (the deterministic derivation fills the rest).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityProposal {
    pub entity_key: String,
    #[serde(default)]
    pub entity_type: Option<String>,
    #[serde(default)]
    pub canonical_name: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// A canonical entity anchor. `entity_key` is the stable identity (lowercased
/// `type:slug`); `aliases` holds other keys merged into it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityRecord {
    pub entity_key: String,
    pub entity_type: String,
    pub canonical_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub facets: Vec<Facet>,
    pub confidence: f64,
    pub first_seen_at: String,
    pub last_seen_at: String,
    #[serde(default)]
    pub status: EvidenceStatus,
    /// When set, this entity was merged into the named canonical key by a user.
    /// It is tombstoned (hidden, never auto-resurrected) but kept so the merge
    /// can be reversed.
    #[serde(default)]
    pub merged_into: Option<String>,
    /// True once a user has merged/renamed/suppressed this anchor. Re-distillation
    /// will not overwrite a user-curated entity.
    #[serde(default)]
    pub user_curated: bool,
    /// Producer lane that created this anchor (`task_episode` default |
    /// `ambient_browser` | …) — self-describing discovery tag for consumers.
    #[serde(default = "super::default_producer")]
    pub producer: String,
}

/// Normalize a raw key to its stable identity form (`type:slug`, lowercased).
fn normalize_key(key: &str) -> Option<String> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some(key.to_lowercase())
}

/// Split a normalized key into `(type, slug)`; keys without a `:` are `other`.
fn split_key(key: &str) -> (String, String) {
    match key.split_once(':') {
        Some((t, s)) if !s.trim().is_empty() => (t.trim().to_string(), s.trim().to_string()),
        _ => ("other".to_string(), key.to_string()),
    }
}

/// Turn a slug into a human canonical name. IDs (`pr`, `ticket`) read better
/// uppercased with the raw slug; everything else is title-cased.
fn humanize_entity_name(entity_type: &str, slug: &str) -> String {
    match entity_type {
        "pr" | "ticket" => format!("{} {}", entity_type.to_uppercase(), slug),
        _ => slug
            .split(['-', '_', ' '])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

impl EntityRecord {
    /// Build a fresh anchor from one raw key + the evidence that referenced it.
    /// Returns `None` for an empty key.
    pub fn from_key(
        key: &str,
        facets: &[Facet],
        source_refs: &[String],
        seen_at: &str,
    ) -> Option<Self> {
        let entity_key = normalize_key(key)?;
        let (entity_type, slug) = split_key(&entity_key);
        let canonical_name = humanize_entity_name(&entity_type, &slug);
        Some(Self {
            entity_key,
            entity_type,
            canonical_name,
            aliases: Vec::new(),
            source_refs: source_refs.to_vec(),
            facets: facets.to_vec(),
            confidence: 0.6,
            first_seen_at: seen_at.to_string(),
            last_seen_at: seen_at.to_string(),
            status: EvidenceStatus::Active,
            merged_into: None,
            user_curated: false,
            producer: super::default_producer(),
        })
    }

    /// Build an anchor from an LLM [`EntityProposal`], preferring its
    /// canonical name / type / aliases over the key-derived defaults. Free-text
    /// aliases are kept for display + manual-merge reference; they are NOT used
    /// for auto-merge (resolution stays exact-key — see [`matches_key`]).
    pub fn from_proposal(
        proposal: &EntityProposal,
        facets: &[Facet],
        source_refs: &[String],
        seen_at: &str,
    ) -> Option<Self> {
        let entity_key = normalize_key(&proposal.entity_key)?;
        let (derived_type, slug) = split_key(&entity_key);
        let entity_type = proposal
            .entity_type
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase)
            .unwrap_or(derived_type);
        let canonical_name = proposal
            .canonical_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| humanize_entity_name(&entity_type, &slug));
        let aliases = proposal
            .aliases
            .iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty() && a != &entity_key)
            .collect();
        Some(Self {
            entity_key,
            entity_type,
            canonical_name,
            aliases,
            source_refs: source_refs.to_vec(),
            facets: facets.to_vec(),
            confidence: 0.7,
            first_seen_at: seen_at.to_string(),
            last_seen_at: seen_at.to_string(),
            status: EvidenceStatus::Active,
            merged_into: None,
            user_curated: false,
            producer: super::default_producer(),
        })
    }

    /// Whether this anchor matches a raw key by its own key or any alias.
    fn matches_key(&self, normalized: &str) -> bool {
        self.entity_key == normalized || self.aliases.iter().any(|a| a == normalized)
    }
}

/// Derive candidate entity anchors from one evidence record's keys (entities +
/// people), carrying that record's facets / provenance / timestamp. Key-derived
/// only (no LLM names/aliases) — used for backfill / re-resolution from stored
/// records.
pub fn entity_candidates_from_evidence(record: &EvidenceRecord) -> Vec<EntityRecord> {
    record
        .entity_keys
        .iter()
        .chain(record.people_keys.iter())
        .filter_map(|key| {
            EntityRecord::from_key(
                key,
                &record.facets,
                &record.source_refs,
                &record.last_seen_at,
            )
        })
        .collect()
}

/// Derive candidate anchors from a distillation [`EvidenceProposal`], preferring
/// the LLM-enriched `entities` (canonical name + aliases) and falling back to
/// key-derived anchors for any bare `entity_keys`/`people_keys` the model listed
/// but did not enrich.
pub fn entity_candidates_from_proposal(
    proposal: &EvidenceProposal,
    facets: &[Facet],
    source_refs: &[String],
    seen_at: &str,
) -> Vec<EntityRecord> {
    let mut out: Vec<EntityRecord> = Vec::new();
    let mut covered: HashSet<String> = HashSet::new();
    for ep in &proposal.entities {
        if let Some(rec) = EntityRecord::from_proposal(ep, facets, source_refs, seen_at) {
            covered.insert(rec.entity_key.clone());
            out.push(rec);
        }
    }
    for key in proposal
        .entity_keys
        .iter()
        .chain(proposal.people_keys.iter())
    {
        match normalize_key(key) {
            Some(norm) if !covered.contains(&norm) => {
                if let Some(rec) = EntityRecord::from_key(key, facets, source_refs, seen_at) {
                    covered.insert(rec.entity_key.clone());
                    out.push(rec);
                }
            },
            _ => {},
        }
    }
    out
}

/// Merge a facet list into another, deduped by label (user-assigned wins; else
/// the higher confidence wins).
fn merge_facets(into: &mut Vec<Facet>, incoming: &[Facet]) {
    for facet in incoming {
        match into.iter_mut().find(|f| f.label == facet.label) {
            Some(existing) => {
                if existing.assigned_by != "user"
                    && (facet.assigned_by == "user" || facet.confidence > existing.confidence)
                {
                    existing.confidence = facet.confidence.max(existing.confidence);
                    if facet.assigned_by == "user" {
                        existing.assigned_by = "user".to_string();
                    }
                }
            },
            None => into.push(facet.clone()),
        }
    }
}

/// Conservatively resolve candidate anchors into the stored set. Exact key/alias
/// match → merge (provenance, facets, recency); otherwise a new anchor is added.
/// Tombstoned (deleted / merged-away) anchors are never resurrected — a key that
/// was merged into a canonical routes to that canonical instead.
pub fn resolve_entities(stored: &mut Vec<EntityRecord>, candidates: Vec<EntityRecord>) {
    for candidate in candidates {
        // A key merged into a canonical routes there; a deleted tombstone for
        // the exact key is left dormant (re-add only if no tombstone exists).
        if let Some(idx) = stored
            .iter()
            .position(|e| e.matches_key(&candidate.entity_key))
        {
            let existing = &mut stored[idx];
            if existing.status == EvidenceStatus::Deleted && existing.merged_into.is_none() {
                // A user-deleted anchor stays deleted.
                continue;
            }
            for r in candidate.source_refs {
                if !existing.source_refs.contains(&r) {
                    existing.source_refs.push(r);
                }
            }
            // Accrue any new aliases the LLM proposed (canonical_name is
            // first-write-wins, so the name doesn't thrash across episodes).
            for alias in candidate.aliases {
                if alias != existing.entity_key && !existing.aliases.contains(&alias) {
                    existing.aliases.push(alias);
                }
            }
            merge_facets(&mut existing.facets, &candidate.facets);
            if candidate.last_seen_at > existing.last_seen_at {
                existing.last_seen_at = candidate.last_seen_at;
            }
            if candidate.first_seen_at < existing.first_seen_at {
                existing.first_seen_at = candidate.first_seen_at;
            }
        } else {
            stored.push(candidate);
        }
    }
}

/// User correction: merge `absorbed_key` into `canonical_key`. The absorbed
/// anchor is tombstoned with `merged_into` (reversible) and its key + aliases +
/// provenance fold into the canonical. Returns `false` if either is missing or
/// they are the same.
pub fn merge_entities(
    stored: &mut [EntityRecord],
    canonical_key: &str,
    absorbed_key: &str,
) -> bool {
    let canonical_key = match normalize_key(canonical_key) {
        Some(k) => k,
        None => return false,
    };
    let absorbed_key = match normalize_key(absorbed_key) {
        Some(k) => k,
        None => return false,
    };
    if canonical_key == absorbed_key {
        return false;
    }
    let Some(canonical_idx) = stored.iter().position(|e| e.entity_key == canonical_key) else {
        return false;
    };
    let Some(absorbed_idx) = stored.iter().position(|e| e.entity_key == absorbed_key) else {
        return false;
    };
    let absorbed = stored[absorbed_idx].clone();
    let canonical = &mut stored[canonical_idx];
    // The absorbed key + its own aliases become aliases of the canonical.
    for alias in std::iter::once(absorbed.entity_key.clone()).chain(absorbed.aliases.clone()) {
        if !canonical.aliases.contains(&alias) && alias != canonical.entity_key {
            canonical.aliases.push(alias);
        }
    }
    for r in absorbed.source_refs {
        if !canonical.source_refs.contains(&r) {
            canonical.source_refs.push(r);
        }
    }
    merge_facets(&mut canonical.facets, &absorbed.facets);
    if absorbed.last_seen_at > canonical.last_seen_at {
        canonical.last_seen_at = absorbed.last_seen_at.clone();
    }
    canonical.user_curated = true;
    let canonical_key_owned = canonical.entity_key.clone();
    let absorbed = &mut stored[absorbed_idx];
    absorbed.status = EvidenceStatus::Deleted;
    absorbed.merged_into = Some(canonical_key_owned);
    absorbed.user_curated = true;
    true
}

/// User correction: reverse a merge — restore the anchor previously merged into
/// `absorbed_key`'s canonical, removing it from the canonical's aliases. Returns
/// `false` if the key is not a merged-away anchor.
pub fn split_entity(stored: &mut [EntityRecord], absorbed_key: &str) -> bool {
    let absorbed_key = match normalize_key(absorbed_key) {
        Some(k) => k,
        None => return false,
    };
    let Some(absorbed_idx) = stored
        .iter()
        .position(|e| e.entity_key == absorbed_key && e.merged_into.is_some())
    else {
        return false;
    };
    let canonical_key = stored[absorbed_idx].merged_into.clone();
    stored[absorbed_idx].status = EvidenceStatus::Active;
    stored[absorbed_idx].merged_into = None;
    if let Some(canonical_key) = canonical_key {
        if let Some(canonical) = stored.iter_mut().find(|e| e.entity_key == canonical_key) {
            canonical.aliases.retain(|a| a != &absorbed_key);
        }
    }
    true
}
