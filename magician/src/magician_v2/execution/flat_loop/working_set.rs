//! Surface-neutral deferred-tool working-set primitives.
//!
//! Authorization remains owned by `EffectiveToolPolicySnapshot`. Callers pass
//! the exact authorized deferred/direct ceiling into this module; it never
//! derives or broadens authority from the complete scope index. A successful
//! `select:` loads the **authorized leaves of every pack it names** — by a
//! leaf (`browser__open`) or by the pack's own name (`browser`) — because a
//! pack is one tool with many verbs, and a model that has to name each verb
//! forgets the one it sees with (on 2026-09-20 a browser task re-selected six
//! leaves after a family switch, left `snapshot` out, and drove a page it
//! could no longer see for nineteen minutes). A pack whose whole set would
//! exceed the tool or schema budget loads the named leaves only. The select
//! then **merges** into the loaded set; only when the family limit is
//! exceeded does the oldest family go, and the projection names what was
//! unloaded so the caller can tell the model.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use serde::Serialize;

use super::ToolIndex;
use crate::magician_v2::agents::{FeatureMode, InvocationSurface};

const DEFAULT_SURFACE_WORKING_SET_LIMIT: usize = 1024;

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkingSetLimits {
    pub max_loaded_families: usize,
    pub max_loaded_tools: usize,
    pub max_schema_bytes: usize,
}

impl Default for WorkingSetLimits {
    fn default() -> Self {
        Self {
            // Two, not one: a browser task needs `time_math` for "next
            // Friday" and must not lose the browser to get it.
            max_loaded_families: 2,
            max_loaded_tools: 128,
            max_schema_bytes: 256 * 1024,
        }
    }
}

/// Families an autonomous run keeps loaded at once. The evidence that chose
/// it: a browser task holds `browser`, a date pack, a content pack and files.
pub const AUTONOMOUS_MAX_LOADED_FAMILIES: usize = 4;

impl WorkingSetLimits {
    /// Limits for the autonomous flat loop: at least
    /// [`AUTONOMOUS_MAX_LOADED_FAMILIES`] families (more when one select names
    /// more than that), tool and schema budgets unbounded as before.
    pub fn autonomous_compatibility(selected_count: usize) -> Self {
        Self {
            max_loaded_families: selected_count.max(AUTONOMOUS_MAX_LOADED_FAMILIES),
            max_loaded_tools: usize::MAX,
            max_schema_bytes: usize::MAX,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FamilyLoadProjection {
    pub selected_count: usize,
    pub accepted_selected_count: usize,
    pub unavailable_selected_count: usize,
    pub loaded_families: Vec<String>,
    pub loaded_tools: Vec<String>,
    pub schema_bytes: usize,
    /// Families the merge had to evict to stay within the family limit.
    /// Empty from `project_family_selection`; filled by a store `select`.
    #[serde(default)]
    pub unloaded_families: Vec<String>,
    /// Each loaded tool's pack and priced schema size, so a later merge can
    /// evict by family and re-price without the index.
    #[serde(default)]
    pub tool_packs: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub tool_bytes: std::collections::BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FamilyLoadError {
    TooManyFamilies { requested: usize, limit: usize },
    TooManyTools { requested: usize, limit: usize },
    SchemaBudgetExceeded { requested: usize, limit: usize },
}

/// Parse the state-changing `tool_search` form without accepting lookalike
/// prefixes. Tool names are de-duplicated in first-seen order so retries and
/// repeated names cannot inflate family/tool limits.
pub fn selected_tool_names_from_query(query: &str) -> Option<Vec<String>> {
    let (mode, rest) = query.trim().split_once(':')?;
    if !mode.eq_ignore_ascii_case("select") {
        return None;
    }

    let mut seen = HashSet::new();
    Some(
        rest.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .filter(|name| seen.insert((*name).to_string()))
            .map(str::to_string)
            .collect(),
    )
}

/// Resolve selected leaf names into an exact authorized leaf set.
///
/// Unknown and unauthorized names are counted but never echoed, preventing
/// the complete scope index from becoming a metadata side channel. When no
/// selection is accepted the projection is empty; stateful callers should
/// retain their previous working set.
pub fn project_family_selection(
    index: &ToolIndex,
    selected_names: &[String],
    authorized_names: &HashSet<String>,
    limits: WorkingSetLimits,
) -> Result<FamilyLoadProjection, FamilyLoadError> {
    let mut families = BTreeSet::new();
    let mut named_leaves = BTreeSet::new();
    let mut accepted = 0usize;
    for name in selected_names {
        match index.get(name) {
            Some(entry) => {
                if !authorized_names.contains(entry.name.as_str()) {
                    continue;
                }
                accepted += 1;
                families.insert(entry.pack_name.clone());
                named_leaves.insert(entry.name.clone());
            },
            None => {
                // The pack's own name, or a leaf name this index collapsed
                // into its pack. It counts as accepted only if the pack has
                // an authorized leaf: an unknown name must not look loaded.
                let Some(pack) = index.pack_for_selected_name(name) else {
                    continue;
                };
                let leaves: Vec<String> = index
                    .leaf_names_for_pack(pack)
                    .into_iter()
                    .filter(|leaf| authorized_names.contains(leaf.as_str()))
                    .collect();
                if !leaves.is_empty() {
                    accepted += 1;
                    families.insert(pack.to_string());
                }
            },
        }
    }

    if families.len() > limits.max_loaded_families {
        return Err(FamilyLoadError::TooManyFamilies {
            requested: families.len(),
            limit: limits.max_loaded_families,
        });
    }

    // Whole-pack: every authorized leaf of every named pack. Fall back to
    // the named leaves alone when the whole set would not fit the budget,
    // so a large pack still loads what was asked for.
    let whole: BTreeSet<String> = families
        .iter()
        .flat_map(|pack| index.leaf_names_for_pack(pack))
        .filter(|leaf| authorized_names.contains(leaf.as_str()))
        .chain(named_leaves.iter().cloned())
        .collect();
    let schema_bytes_of = |tools: &BTreeSet<String>| {
        tools
            .iter()
            .filter_map(|name| index.get(name))
            .map(|entry| {
                entry.name.len()
                    + entry.description.len()
                    + serde_json::to_vec(&entry.parameters_schema)
                        .map(|encoded| encoded.len())
                        .unwrap_or(0)
            })
            .sum::<usize>()
    };
    let (loaded_tools, schema_bytes) = {
        let whole_bytes = schema_bytes_of(&whole);
        if whole.len() <= limits.max_loaded_tools && whole_bytes <= limits.max_schema_bytes {
            (whole, whole_bytes)
        } else {
            let bytes = schema_bytes_of(&named_leaves);
            (named_leaves, bytes)
        }
    };

    if loaded_tools.len() > limits.max_loaded_tools {
        return Err(FamilyLoadError::TooManyTools {
            requested: loaded_tools.len(),
            limit: limits.max_loaded_tools,
        });
    }
    if schema_bytes > limits.max_schema_bytes {
        return Err(FamilyLoadError::SchemaBudgetExceeded {
            requested: schema_bytes,
            limit: limits.max_schema_bytes,
        });
    }

    let tool_packs = loaded_tools
        .iter()
        .filter_map(|name| {
            index
                .get(name)
                .map(|entry| (name.clone(), entry.pack_name.clone()))
        })
        .collect();
    let tool_bytes = loaded_tools
        .iter()
        .filter_map(|name| {
            index.get(name).map(|entry| {
                (
                    name.clone(),
                    entry.name.len()
                        + entry.description.len()
                        + serde_json::to_vec(&entry.parameters_schema)
                            .map(|encoded| encoded.len())
                            .unwrap_or(0),
                )
            })
        })
        .collect();
    Ok(FamilyLoadProjection {
        selected_count: selected_names.len(),
        accepted_selected_count: accepted,
        unavailable_selected_count: selected_names.len().saturating_sub(accepted),
        loaded_families: families.into_iter().collect(),
        loaded_tools: loaded_tools.into_iter().collect(),
        schema_bytes,
        unloaded_families: Vec::new(),
        tool_packs,
        tool_bytes,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolWorkingSet {
    pub base_policy_snapshot_id: String,
    pub generation: u64,
    pub loaded_families: BTreeSet<String>,
    pub loaded_tools: BTreeSet<String>,
    pub schema_bytes: usize,
    /// Families in the order they were loaded; the front is the first to go
    /// when a merge exceeds the family limit.
    pub family_order: Vec<String>,
    /// Each loaded tool's pack and priced schema size (from its projection).
    pub tool_packs: std::collections::BTreeMap<String, String>,
    pub tool_bytes: std::collections::BTreeMap<String, usize>,
}

/// Identity of one mutable tool working set.
///
/// The feature mode is deliberately part of the key: Tutor, App Copilot, and
/// Thinking Map sessions must never inherit a family loaded by ordinary Chat.
/// `binding_id` is the chat session, realtime call, or autonomous owner-frame
/// id and is always minted by the authenticated runtime surface.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SurfaceWorkingSetKey {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    pub binding_id: String,
}

impl SurfaceWorkingSetKey {
    pub fn new(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        agent_id: impl Into<String>,
        surface: InvocationSurface,
        feature_mode: FeatureMode,
        binding_id: impl Into<String>,
    ) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
            agent_id: agent_id.into(),
            surface,
            feature_mode,
            binding_id: binding_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SurfaceWorkingSetSnapshot {
    pub base_policy_snapshot_id: String,
    pub generation: u64,
    pub loaded_families: Vec<String>,
    pub loaded_tools: Vec<String>,
    pub schema_bytes: usize,
    pub last_updated_at_ms: u64,
    /// Whether this read had to invent the binding rather than find it.
    ///
    /// An empty working set and an ABSENT one produce the same snapshot
    /// otherwise, and they mean opposite things. Empty is a real state a session
    /// starts in. Absent means the selection was lost — evicted under
    /// `max_entries`, or never carried to whichever holder is asking — and a
    /// caller that reads "nothing loaded" will act on it instead of reselecting.
    ///
    /// Additive on purpose: `reconcile` still creates the binding, because first
    /// use legitimately has none. This only lets a caller that expected one tell
    /// the difference.
    ///
    /// Meaningful on a READ only. A snapshot returned by
    /// [`SurfaceWorkingSetStore::select`] always reports `false`, because the
    /// caller of a write is installing a selection rather than asking whether one
    /// survived — `false` there means "not asked", never "the binding pre-existed".
    pub created: bool,
}

/// A family selection projected against one exact working-set generation.
///
/// Realtime providers use this as a two-phase transition: prepare the catalog,
/// install it at the provider, then commit only after the provider acknowledges
/// the update. Direct Chat can continue to use [`SurfaceWorkingSetStore::select`]
/// because its next provider request is made in-process after the mutation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedFamilyLoad {
    pub base_policy_snapshot_id: String,
    pub expected_generation: u64,
    pub projection: FamilyLoadProjection,
    /// The limits the commit merges under.
    pub limits: WorkingSetLimits,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PreparedFamilyLoadCommitError {
    WorkingSetMissing,
    PolicyChanged { expected: String, actual: String },
    GenerationChanged { expected: u64, actual: u64 },
}

#[derive(Debug, Clone)]
struct SurfaceWorkingSetEntry {
    state: ToolWorkingSet,
    last_updated_at_ms: u64,
    last_access_sequence: u64,
}

impl SurfaceWorkingSetEntry {
    fn snapshot(&self) -> SurfaceWorkingSetSnapshot {
        self.snapshot_marked(false)
    }

    /// `created` is set only by the vacant branch of `reconcile`, which is the
    /// one place a binding comes into existence on a read.
    fn snapshot_marked(&self, created: bool) -> SurfaceWorkingSetSnapshot {
        SurfaceWorkingSetSnapshot {
            base_policy_snapshot_id: self.state.base_policy_snapshot_id.clone(),
            generation: self.state.generation,
            loaded_families: self.state.loaded_families.iter().cloned().collect(),
            loaded_tools: self.state.loaded_tool_names(),
            schema_bytes: self.state.schema_bytes,
            last_updated_at_ms: self.last_updated_at_ms,
            created,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SurfaceWorkingSetStoreStatus {
    pub entry_count: usize,
    pub reads: u64,
    pub creates: u64,
    pub mutations: u64,
    pub removals: u64,
    pub evictions: u64,
}

#[derive(Debug, Default)]
struct SurfaceWorkingSetStoreMetrics {
    reads: AtomicU64,
    creates: AtomicU64,
    mutations: AtomicU64,
    removals: AtomicU64,
    evictions: AtomicU64,
}

/// Bounded process-local L3 working-set store shared by interactive surfaces.
///
/// It owns no authority: every read reconciles against the current immutable
/// policy snapshot and authorized-name ceiling supplied by the caller. A
/// policy revision clears loaded state before a subsequent provider request;
/// a same-revision narrowing removes revoked leaves immediately.
#[derive(Debug, Clone)]
pub struct SurfaceWorkingSetStore {
    entries: Arc<DashMap<SurfaceWorkingSetKey, SurfaceWorkingSetEntry>>,
    max_entries: usize,
    access_sequence: Arc<AtomicU64>,
    metrics: Arc<SurfaceWorkingSetStoreMetrics>,
}

impl Default for SurfaceWorkingSetStore {
    fn default() -> Self {
        Self::new(DEFAULT_SURFACE_WORKING_SET_LIMIT)
    }
}

impl SurfaceWorkingSetStore {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
            max_entries: max_entries.max(1),
            access_sequence: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(SurfaceWorkingSetStoreMetrics::default()),
        }
    }

    /// Return a policy-reconciled view, creating an empty binding when needed.
    pub fn reconcile(
        &self,
        key: &SurfaceWorkingSetKey,
        policy_snapshot_id: &str,
        authorized_names: &HashSet<String>,
    ) -> SurfaceWorkingSetSnapshot {
        self.metrics.reads.fetch_add(1, Ordering::Relaxed);
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let now = now_epoch_ms();
        match self.entries.entry(key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                if entry.state.reconcile(policy_snapshot_id, authorized_names) {
                    entry.last_updated_at_ms = now;
                    self.metrics.mutations.fetch_add(1, Ordering::Relaxed);
                }
                entry.last_access_sequence = sequence;
                entry.snapshot()
            },
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                self.metrics.creates.fetch_add(1, Ordering::Relaxed);
                let entry = SurfaceWorkingSetEntry {
                    state: ToolWorkingSet::new(policy_snapshot_id),
                    last_updated_at_ms: now,
                    last_access_sequence: sequence,
                };
                let snapshot = entry.snapshot_marked(true);
                vacant.insert(entry);
                self.evict_to_limit();
                snapshot
            },
        }
    }

    /// Atomically project and apply an explicit family selection. Invalid,
    /// unauthorized, or over-budget requests never replace the current set.
    pub fn select(
        &self,
        key: &SurfaceWorkingSetKey,
        policy_snapshot_id: &str,
        index: &ToolIndex,
        selected_names: &[String],
        authorized_names: &HashSet<String>,
        limits: WorkingSetLimits,
    ) -> Result<(FamilyLoadProjection, SurfaceWorkingSetSnapshot), FamilyLoadError> {
        // Reconcile first so a failed select can never preserve state from a
        // revoked policy revision.
        self.reconcile(key, policy_snapshot_id, authorized_names);
        let mut projection =
            project_family_selection(index, selected_names, authorized_names, limits)?;
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let now = now_epoch_ms();
        let snapshot = if let Some(mut entry) = self.entries.get_mut(key) {
            let before = entry.state.generation;
            if let Some(unloaded) = entry.state.merge(&projection, limits) {
                projection.unloaded_families = unloaded;
            }
            if entry.state.generation != before {
                entry.last_updated_at_ms = now;
                self.metrics.mutations.fetch_add(1, Ordering::Relaxed);
            }
            entry.last_access_sequence = sequence;
            entry.snapshot()
        } else {
            // A concurrent clear is allowed to win. Recreate only from the
            // current policy and this already-authorized projection.
            let mut state = ToolWorkingSet::new(policy_snapshot_id);
            state.replace(&projection);
            let entry = SurfaceWorkingSetEntry {
                state,
                last_updated_at_ms: now,
                last_access_sequence: sequence,
            };
            let snapshot = entry.snapshot();
            self.entries.insert(key.clone(), entry);
            snapshot
        };
        self.evict_to_limit();
        Ok((projection, snapshot))
    }

    /// Project a family switch without mutating the loaded set. Reconciliation
    /// still happens first so a revoked policy can never survive while a
    /// provider catalog update is pending.
    pub fn prepare_select(
        &self,
        key: &SurfaceWorkingSetKey,
        policy_snapshot_id: &str,
        index: &ToolIndex,
        selected_names: &[String],
        authorized_names: &HashSet<String>,
        limits: WorkingSetLimits,
    ) -> Result<PreparedFamilyLoad, FamilyLoadError> {
        let current = self.reconcile(key, policy_snapshot_id, authorized_names);
        let projection = project_family_selection(index, selected_names, authorized_names, limits)?;
        Ok(PreparedFamilyLoad {
            base_policy_snapshot_id: current.base_policy_snapshot_id,
            expected_generation: current.generation,
            projection,
            limits,
        })
    }

    /// Commit a previously prepared family switch using compare-and-swap
    /// semantics. A policy refresh, another selection, session teardown, or
    /// cache clear wins over the pending provider update and fails closed.
    pub fn commit_prepared(
        &self,
        key: &SurfaceWorkingSetKey,
        prepared: &PreparedFamilyLoad,
    ) -> Result<SurfaceWorkingSetSnapshot, PreparedFamilyLoadCommitError> {
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let now = now_epoch_ms();
        let Some(mut entry) = self.entries.get_mut(key) else {
            return Err(PreparedFamilyLoadCommitError::WorkingSetMissing);
        };
        if entry.state.base_policy_snapshot_id != prepared.base_policy_snapshot_id {
            return Err(PreparedFamilyLoadCommitError::PolicyChanged {
                expected: prepared.base_policy_snapshot_id.clone(),
                actual: entry.state.base_policy_snapshot_id.clone(),
            });
        }
        if entry.state.generation != prepared.expected_generation {
            return Err(PreparedFamilyLoadCommitError::GenerationChanged {
                expected: prepared.expected_generation,
                actual: entry.state.generation,
            });
        }
        let before = entry.state.generation;
        entry.state.merge(&prepared.projection, prepared.limits);
        if entry.state.generation != before {
            entry.last_updated_at_ms = now;
            self.metrics.mutations.fetch_add(1, Ordering::Relaxed);
        }
        entry.last_access_sequence = sequence;
        Ok(entry.snapshot())
    }

    pub fn remove(&self, key: &SurfaceWorkingSetKey) -> bool {
        let removed = self.entries.remove(key).is_some();
        if removed {
            self.metrics.removals.fetch_add(1, Ordering::Relaxed);
        }
        removed
    }

    /// Remove every feature/surface binding for one authenticated session or
    /// call id. This is used by Chat deletion and realtime call teardown.
    pub fn remove_binding(&self, binding_id: &str) -> usize {
        let keys = self
            .entries
            .iter()
            .filter(|entry| entry.key().binding_id == binding_id)
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        let mut removed = 0usize;
        for key in keys {
            if self.entries.remove(&key).is_some() {
                removed += 1;
            }
        }
        if removed > 0 {
            self.metrics
                .removals
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    pub fn remove_agent(&self, principal: &str, workspace: &str, agent_id: &str) -> usize {
        let keys = self
            .entries
            .iter()
            .filter(|entry| {
                let key = entry.key();
                key.principal == principal && key.workspace == workspace && key.agent_id == agent_id
            })
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        let mut removed = 0usize;
        for key in keys {
            if self.entries.remove(&key).is_some() {
                removed += 1;
            }
        }
        if removed > 0 {
            self.metrics
                .removals
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    pub fn status(&self) -> SurfaceWorkingSetStoreStatus {
        SurfaceWorkingSetStoreStatus {
            entry_count: self.entries.len(),
            reads: self.metrics.reads.load(Ordering::Relaxed),
            creates: self.metrics.creates.load(Ordering::Relaxed),
            mutations: self.metrics.mutations.load(Ordering::Relaxed),
            removals: self.metrics.removals.load(Ordering::Relaxed),
            evictions: self.metrics.evictions.load(Ordering::Relaxed),
        }
    }

    fn evict_to_limit(&self) {
        while self.entries.len() > self.max_entries {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|entry| entry.value().last_access_sequence)
                .map(|entry| entry.key().clone());
            let Some(oldest) = oldest else {
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                self.metrics.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl ToolWorkingSet {
    pub fn new(base_policy_snapshot_id: impl Into<String>) -> Self {
        Self {
            base_policy_snapshot_id: base_policy_snapshot_id.into(),
            generation: 0,
            loaded_families: BTreeSet::new(),
            loaded_tools: BTreeSet::new(),
            schema_bytes: 0,
            family_order: Vec::new(),
            tool_packs: std::collections::BTreeMap::new(),
            tool_bytes: std::collections::BTreeMap::new(),
        }
    }

    /// Reconcile at a decision boundary. A policy revision change clears the
    /// complete loaded set; same-revision narrowing removes only unauthorized
    /// leaves. Both changes advance the monotonic generation.
    pub fn reconcile(
        &mut self,
        policy_snapshot_id: &str,
        authorized_names: &HashSet<String>,
    ) -> bool {
        let changed = if self.base_policy_snapshot_id != policy_snapshot_id {
            self.base_policy_snapshot_id = policy_snapshot_id.to_string();
            self.loaded_families.clear();
            self.loaded_tools.clear();
            self.family_order.clear();
            self.tool_packs.clear();
            self.tool_bytes.clear();
            self.schema_bytes = 0;
            true
        } else {
            let before = self.loaded_tools.len();
            self.loaded_tools
                .retain(|name| authorized_names.contains(name.as_str()));
            before != self.loaded_tools.len()
        };
        if changed {
            self.generation = self.generation.saturating_add(1);
        }
        changed
    }

    /// Replace the loaded set after a successful explicit selection. Empty
    /// accepted projections leave the prior state untouched.
    pub fn replace(&mut self, projection: &FamilyLoadProjection) -> bool {
        if projection.accepted_selected_count == 0 {
            return false;
        }
        let families = projection.loaded_families.iter().cloned().collect();
        let tools = projection.loaded_tools.iter().cloned().collect();
        if self.loaded_families == families
            && self.loaded_tools == tools
            && self.schema_bytes == projection.schema_bytes
        {
            return false;
        }
        self.loaded_families = families;
        self.family_order = projection.loaded_families.clone();
        self.loaded_tools = tools;
        self.tool_packs = projection.tool_packs.clone();
        self.tool_bytes = projection.tool_bytes.clone();
        self.schema_bytes = projection.schema_bytes;
        self.generation = self.generation.saturating_add(1);
        true
    }

    /// Merge a successful selection into the loaded set. Families already
    /// loaded stay; when the union exceeds the family limit (or the tool or
    /// schema budget), the oldest families are evicted with every tool of
    /// theirs until it fits, and the evicted names are returned so the caller
    /// can tell the model. Empty accepted projections leave the state
    /// untouched and return `None`.
    pub fn merge(
        &mut self,
        projection: &FamilyLoadProjection,
        limits: WorkingSetLimits,
    ) -> Option<Vec<String>> {
        if projection.accepted_selected_count == 0 {
            return None;
        }
        let mut order: Vec<String> = self
            .family_order
            .iter()
            .filter(|family| !projection.loaded_families.contains(family))
            .cloned()
            .collect();
        order.extend(projection.loaded_families.iter().cloned());
        let mut tool_packs = self.tool_packs.clone();
        tool_packs.extend(
            projection
                .tool_packs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        let mut tool_bytes = self.tool_bytes.clone();
        tool_bytes.extend(projection.tool_bytes.iter().map(|(k, v)| (k.clone(), *v)));
        let mut tools: BTreeSet<String> = self.loaded_tools.iter().cloned().collect();
        tools.extend(projection.loaded_tools.iter().cloned());
        let pack_of = |name: &str, packs: &std::collections::BTreeMap<String, String>| {
            packs.get(name).cloned().unwrap_or_else(|| {
                name.split_once("__")
                    .map_or(name, |(pack, _)| pack)
                    .to_string()
            })
        };
        let price = |tools: &BTreeSet<String>,
                     bytes: &std::collections::BTreeMap<String, usize>| {
            tools
                .iter()
                .map(|name| bytes.get(name).copied().unwrap_or(0))
                .sum::<usize>()
        };
        let mut schema_bytes = price(&tools, &tool_bytes);
        let mut unloaded = Vec::new();
        while order.len() > 1
            && (order.len() > limits.max_loaded_families
                || tools.len() > limits.max_loaded_tools
                || schema_bytes > limits.max_schema_bytes)
        {
            let evicted = order.remove(0);
            tools.retain(|name| pack_of(name, &tool_packs) != evicted);
            schema_bytes = price(&tools, &tool_bytes);
            unloaded.push(evicted);
        }
        tool_packs.retain(|name, _| tools.contains(name));
        tool_bytes.retain(|name, _| tools.contains(name));
        let families: BTreeSet<String> = order.iter().cloned().collect();
        if self.loaded_families == families
            && self.loaded_tools == tools
            && self.schema_bytes == schema_bytes
        {
            return Some(unloaded);
        }
        self.loaded_families = families;
        self.family_order = order;
        self.loaded_tools = tools;
        self.tool_packs = tool_packs;
        self.tool_bytes = tool_bytes;
        self.schema_bytes = schema_bytes;
        self.generation = self.generation.saturating_add(1);
        Some(unloaded)
    }

    pub fn loaded_tool_names(&self) -> Vec<String> {
        self.loaded_tools.iter().cloned().collect()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::magician_v2::execution::capability::CapabilityPackDefinition;
    use crate::magician_v2::execution::flat_loop::build_tool_index;

    #[test]
    fn a_read_says_whether_it_found_the_binding_or_invented_it() {
        // An empty working set and an ABSENT one produced the same snapshot, and
        // they mean opposite things. Empty is a real state a session starts in.
        // Absent means the selection was lost — evicted under `max_entries`, or
        // never carried to whichever holder is asking — and a caller reading
        // "nothing loaded" acts on it instead of reselecting.
        //
        // `reconcile` still creates the binding: first use legitimately has
        // none. This only lets a caller that expected one tell the difference.
        let store = SurfaceWorkingSetStore::new(8);
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "session-1");
        let authorized = HashSet::new();

        let first = store.reconcile(&key, "policy-1", &authorized);
        assert!(
            first.created,
            "the first read has no binding to find and must say so"
        );
        assert!(first.loaded_tools.is_empty());

        let second = store.reconcile(&key, "policy-1", &authorized);
        assert!(
            !second.created,
            "a binding that exists must not be reported as invented"
        );
        assert!(
            second.loaded_tools.is_empty(),
            "still empty — which is exactly why `created` is the only way to \
             tell these two reads apart"
        );

        // After eviction the flag reappears: the in-process form of the same
        // loss a second holder would see.
        assert!(store.remove(&key));
        assert!(store.reconcile(&key, "policy-1", &authorized).created);
    }

    fn pack(name: &str, actions: &[&str]) -> CapabilityPackDefinition {
        let actions = actions
            .iter()
            .map(|action| {
                format!(
                    "  {action}:\n    description: {action}\n    parameters: [value]\n    required: [value]\n    parameter_overrides:\n      value: {{type: string}}\n"
                )
            })
            .collect::<String>();
        serde_yaml::from_str(&format!(
            "name: {name}\ndescription: {name}\nparameters: []\nnative_action_schemas:\n{actions}implementation:\n  type: primitive\n  provider_name: {name}\n"
        ))
        .expect("pack")
    }

    fn fixture_index() -> ToolIndex {
        build_tool_index(&[
            pack("browser", &["open", "click", "snapshot"]),
            pack("memory", &["search", "save"]),
        ])
    }

    /// Selecting one leaf loads the pack's authorized leaves, never an
    /// unauthorized sibling. On 2026-09-20 a browser task re-selected six
    /// leaves after a family switch, left `snapshot` out, and drove a page it
    /// could no longer see for nineteen minutes; a pack is one tool with
    /// many verbs, and the loop's own contract had promised whole-pack load.
    #[test]
    fn selecting_one_leaf_loads_the_packs_authorized_leaves() {
        let index = fixture_index();
        let allowed = HashSet::from([
            "browser__open".to_string(),
            "browser__click".to_string(),
            "memory__search".to_string(),
        ]);
        let projection = project_family_selection(
            &index,
            &["browser__open".to_string()],
            &allowed,
            WorkingSetLimits::default(),
        )
        .expect("projection");

        assert_eq!(projection.loaded_families, vec!["browser"]);
        assert_eq!(
            projection.loaded_tools,
            vec!["browser__click", "browser__open"]
        );
        assert!(
            !projection
                .loaded_tools
                .contains(&"browser__snapshot".to_string()),
            "an unauthorized sibling never leaks in"
        );
    }

    /// The name a model or harness knows a tool by is the pack's.
    #[test]
    fn selecting_a_pack_by_name_loads_its_authorized_leaves() {
        let index = fixture_index();
        let allowed = HashSet::from(["browser__open".to_string(), "browser__snapshot".to_string()]);
        let projection = project_family_selection(
            &index,
            &["browser".to_string()],
            &allowed,
            WorkingSetLimits::default(),
        )
        .expect("projection");
        assert_eq!(projection.accepted_selected_count, 1);
        assert_eq!(projection.loaded_families, vec!["browser"]);
        assert_eq!(
            projection.loaded_tools,
            vec!["browser__open", "browser__snapshot"]
        );
    }

    /// A pack too large for the budget still loads what was named rather
    /// than refusing the whole select.
    #[test]
    fn a_pack_over_the_tool_budget_falls_back_to_the_named_leaves() {
        let index = fixture_index();
        let allowed = HashSet::from([
            "browser__open".to_string(),
            "browser__click".to_string(),
            "browser__snapshot".to_string(),
        ]);
        let limits = WorkingSetLimits {
            max_loaded_tools: 2,
            ..WorkingSetLimits::default()
        };
        let projection = project_family_selection(
            &index,
            &["browser__open".to_string(), "browser__snapshot".to_string()],
            &allowed,
            limits,
        )
        .expect("named leaves fit");
        assert_eq!(
            projection.loaded_tools,
            vec!["browser__open", "browser__snapshot"]
        );
    }

    /// A second select merges while the limits allow it: loading `memory` to
    /// look something up must not unload `browser` mid-task.
    #[test]
    fn a_select_under_the_family_limit_keeps_the_families_already_loaded() {
        let index = fixture_index();
        let store = SurfaceWorkingSetStore::new(8);
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "merge");
        let allowed = HashSet::from([
            "browser__open".to_string(),
            "browser__snapshot".to_string(),
            "memory__search".to_string(),
        ]);
        let limits = WorkingSetLimits {
            max_loaded_families: 2,
            ..WorkingSetLimits::default()
        };
        store
            .select(
                &key,
                "policy-1",
                &index,
                &["browser__open".to_string()],
                &allowed,
                limits,
            )
            .expect("first select");
        let (projection, snapshot) = store
            .select(
                &key,
                "policy-1",
                &index,
                &["memory__search".to_string()],
                &allowed,
                limits,
            )
            .expect("second select");
        assert!(projection.unloaded_families.is_empty(), "{projection:?}");
        assert_eq!(snapshot.loaded_families, vec!["browser", "memory"]);
        assert!(snapshot
            .loaded_tools
            .contains(&"browser__snapshot".to_string()));
        assert!(snapshot
            .loaded_tools
            .contains(&"memory__search".to_string()));
    }

    /// Over the family limit the oldest family goes, and the projection says
    /// so — the model must be told its earlier tools are gone.
    #[test]
    fn a_select_over_the_family_limit_evicts_the_oldest_family_and_says_which() {
        let index = fixture_index();
        let store = SurfaceWorkingSetStore::new(8);
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "evict");
        let allowed = HashSet::from(["browser__open".to_string(), "memory__search".to_string()]);
        let limits = WorkingSetLimits {
            max_loaded_families: 1,
            ..WorkingSetLimits::default()
        };
        store
            .select(
                &key,
                "policy-1",
                &index,
                &["browser__open".to_string()],
                &allowed,
                limits,
            )
            .expect("first select");
        let (projection, snapshot) = store
            .select(
                &key,
                "policy-1",
                &index,
                &["memory__search".to_string()],
                &allowed,
                limits,
            )
            .expect("second select");
        assert_eq!(projection.unloaded_families, vec!["browser"]);
        assert_eq!(snapshot.loaded_families, vec!["memory"]);
        assert!(!snapshot.loaded_tools.contains(&"browser__open".to_string()));
    }

    #[test]
    fn unknown_and_denied_names_do_not_leak_or_replace_state() {
        let index = fixture_index();
        let allowed = HashSet::from(["memory__search".to_string()]);
        let projection = project_family_selection(
            &index,
            &["missing".to_string(), "browser__open".to_string()],
            &allowed,
            WorkingSetLimits::default(),
        )
        .expect("bounded empty projection");
        assert_eq!(projection.accepted_selected_count, 0);
        assert_eq!(projection.unavailable_selected_count, 2);
        assert!(projection.loaded_families.is_empty());
        assert!(projection.loaded_tools.is_empty());
        assert!(!serde_json::to_string(&projection)
            .expect("serialize")
            .contains("browser__open"));

        let mut state = ToolWorkingSet::new("snapshot-a");
        state.loaded_families.insert("memory".to_string());
        state.loaded_tools.insert("memory__search".to_string());
        assert!(!state.replace(&projection));
        assert_eq!(state.loaded_tool_names(), vec!["memory__search"]);
    }

    /// On a whole-pack surface the pack is one entry named `browser`; a model
    /// that remembers the executor's leaf names selects `browser__open` and
    /// must load that entry, not nothing.
    #[test]
    fn a_collapsed_pack_loads_when_one_of_its_leaves_is_selected() {
        let index = crate::magician_v2::execution::flat_loop::build_surface_tool_index(&[
            pack("browser", &["open", "click", "snapshot"]),
            pack("memory", &["search", "save"]),
        ]);
        let allowed = HashSet::from(["browser".to_string(), "memory".to_string()]);
        let projection = project_family_selection(
            &index,
            &["browser__open".to_string(), "browser__snapshot".to_string()],
            &allowed,
            WorkingSetLimits::default(),
        )
        .expect("bounded projection");
        assert_eq!(projection.accepted_selected_count, 2);
        assert_eq!(projection.unavailable_selected_count, 0);
        assert_eq!(projection.loaded_families, vec!["browser".to_string()]);
        assert_eq!(projection.loaded_tools, vec!["browser".to_string()]);
    }

    #[test]
    fn a_selection_over_the_family_limit_is_rejected_atomically() {
        let index = fixture_index();
        let allowed = HashSet::from(["browser__open".to_string(), "memory__search".to_string()]);
        let error = project_family_selection(
            &index,
            &["browser__open".to_string(), "memory__search".to_string()],
            &allowed,
            WorkingSetLimits {
                max_loaded_families: 1,
                ..WorkingSetLimits::default()
            },
        )
        .expect_err("two families exceed a one-family limit");
        assert_eq!(
            error,
            FamilyLoadError::TooManyFamilies {
                requested: 2,
                limit: 1
            }
        );
    }

    #[test]
    fn select_query_parser_is_case_insensitive_but_rejects_lookalikes() {
        assert_eq!(
            selected_tool_names_from_query(
                "  SeLeCt: browser__open, memory__search, browser__open "
            ),
            Some(vec![
                "browser__open".to_string(),
                "memory__search".to_string()
            ])
        );
        assert_eq!(
            selected_tool_names_from_query("selected:browser__open"),
            None
        );
        assert_eq!(selected_tool_names_from_query("browser"), None);
        assert_eq!(
            selected_tool_names_from_query("select: , "),
            Some(Vec::new())
        );
    }

    #[test]
    fn same_family_reselection_is_a_generation_noop_and_switch_replaces() {
        let index = fixture_index();
        let allowed = HashSet::from([
            "browser__open".to_string(),
            "browser__click".to_string(),
            "memory__search".to_string(),
        ]);
        let browser = project_family_selection(
            &index,
            &["browser__open".to_string()],
            &allowed,
            WorkingSetLimits::default(),
        )
        .expect("browser");
        let memory = project_family_selection(
            &index,
            &["memory__search".to_string()],
            &allowed,
            WorkingSetLimits::default(),
        )
        .expect("memory");
        let mut state = ToolWorkingSet::new("snapshot-a");

        assert!(state.replace(&browser));
        assert_eq!(state.generation, 1);
        assert!(!state.replace(&browser));
        assert_eq!(state.generation, 1);
        assert!(state.replace(&memory));
        assert_eq!(state.generation, 2);
        assert_eq!(state.loaded_tool_names(), vec!["memory__search"]);

        // `merge` is what a store select applies: under the family limit the
        // earlier family stays; at the limit the oldest goes and is named.
        let mut merged = ToolWorkingSet::new("snapshot-b");
        let two = WorkingSetLimits {
            max_loaded_families: 2,
            ..WorkingSetLimits::default()
        };
        assert_eq!(merged.merge(&browser, two), Some(vec![]));
        assert_eq!(merged.merge(&memory, two), Some(vec![]));
        assert_eq!(merged.loaded_families.len(), 2);
        let one = WorkingSetLimits {
            max_loaded_families: 1,
            ..WorkingSetLimits::default()
        };
        let mut single = ToolWorkingSet::new("snapshot-c");
        single.merge(&browser, one);
        assert_eq!(
            single.merge(&memory, one),
            Some(vec!["browser".to_string()])
        );
        assert_eq!(single.loaded_tool_names(), vec!["memory__search"]);
    }

    #[test]
    fn policy_revision_clears_and_same_revision_narrowing_prunes() {
        let mut state = ToolWorkingSet::new("snapshot-a");
        state.loaded_families.insert("browser".to_string());
        state
            .loaded_tools
            .extend(["browser__click".to_string(), "browser__open".to_string()]);
        let narrowed = HashSet::from(["browser__open".to_string()]);
        assert!(state.reconcile("snapshot-a", &narrowed));
        assert_eq!(state.loaded_tool_names(), vec!["browser__open"]);
        assert!(state.reconcile("snapshot-b", &narrowed));
        assert!(state.loaded_tools.is_empty());
        assert_eq!(state.base_policy_snapshot_id, "snapshot-b");
        assert_eq!(state.generation, 2);
        assert_eq!(json!(state.loaded_families), json!([]));
    }

    fn binding(surface: InvocationSurface, feature: FeatureMode, id: &str) -> SurfaceWorkingSetKey {
        SurfaceWorkingSetKey::new("owner", "default", "presto", surface, feature, id)
    }

    #[test]
    fn surface_store_isolates_chat_voice_and_feature_bindings() {
        let store = SurfaceWorkingSetStore::new(16);
        let index = fixture_index();
        let allowed = HashSet::from(["browser__open".to_string(), "browser__click".to_string()]);
        let chat = binding(InvocationSurface::Chat, FeatureMode::None, "shared-id");
        let voice = binding(
            InvocationSurface::RealtimeVoice,
            FeatureMode::None,
            "shared-id",
        );
        let tutor = binding(InvocationSurface::Chat, FeatureMode::Tutor, "shared-id");

        let (_, loaded) = store
            .select(
                &chat,
                "policy-a",
                &index,
                &["browser__open".to_string()],
                &allowed,
                WorkingSetLimits::default(),
            )
            .expect("chat selection");
        assert_eq!(loaded.loaded_families, vec!["browser"]);
        assert!(store
            .reconcile(&voice, "policy-a", &allowed)
            .loaded_tools
            .is_empty());
        assert!(store
            .reconcile(&tutor, "policy-a", &allowed)
            .loaded_tools
            .is_empty());
    }

    #[test]
    fn surface_store_failed_selection_preserves_current_policy_state_only() {
        let store = SurfaceWorkingSetStore::new(16);
        let index = fixture_index();
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "chat-a");
        let broad = HashSet::from([
            "browser__open".to_string(),
            "browser__click".to_string(),
            "memory__search".to_string(),
        ]);
        store
            .select(
                &key,
                "policy-a",
                &index,
                &["browser__open".to_string()],
                &broad,
                WorkingSetLimits::default(),
            )
            .expect("initial selection");

        let narrowed = HashSet::from(["memory__search".to_string()]);
        let error = store
            .select(
                &key,
                "policy-b",
                &index,
                &["missing".to_string(), "browser__open".to_string()],
                &narrowed,
                WorkingSetLimits::default(),
            )
            .expect("unavailable names produce an empty projection");
        assert_eq!(error.0.accepted_selected_count, 0);
        assert_eq!(error.1.base_policy_snapshot_id, "policy-b");
        assert!(error.1.loaded_tools.is_empty());
    }

    #[test]
    fn prepared_selection_commits_only_at_the_exact_policy_generation() {
        let store = SurfaceWorkingSetStore::new(16);
        let index = fixture_index();
        let key = binding(
            InvocationSurface::RealtimeVoice,
            FeatureMode::None,
            "voice-a",
        );
        let allowed = HashSet::from([
            "browser__open".to_string(),
            "browser__click".to_string(),
            "memory__search".to_string(),
        ]);
        let prepared = store
            .prepare_select(
                &key,
                "policy-a",
                &index,
                &["browser__open".to_string()],
                &allowed,
                WorkingSetLimits::default(),
            )
            .expect("prepare browser");
        assert!(store
            .reconcile(&key, "policy-a", &allowed)
            .loaded_tools
            .is_empty());

        let committed = store
            .commit_prepared(&key, &prepared)
            .expect("commit acknowledged catalog");
        assert_eq!(committed.loaded_families, vec!["browser"]);
        assert_eq!(committed.generation, 1);

        let stale = store
            .prepare_select(
                &key,
                "policy-a",
                &index,
                &["memory__search".to_string()],
                &allowed,
                WorkingSetLimits::default(),
            )
            .expect("prepare memory");
        store.reconcile(&key, "policy-b", &allowed);
        assert!(matches!(
            store.commit_prepared(&key, &stale),
            Err(PreparedFamilyLoadCommitError::PolicyChanged { .. })
                | Err(PreparedFamilyLoadCommitError::GenerationChanged { .. })
        ));
        assert!(store
            .reconcile(&key, "policy-b", &allowed)
            .loaded_tools
            .is_empty());
    }

    #[test]
    fn surface_store_eviction_and_binding_cleanup_are_bounded() {
        let store = SurfaceWorkingSetStore::new(2);
        let allowed = HashSet::new();
        for id in ["one", "two", "three"] {
            store.reconcile(
                &binding(InvocationSurface::Chat, FeatureMode::None, id),
                "policy",
                &allowed,
            );
        }
        let status = store.status();
        assert_eq!(status.entry_count, 2);
        assert_eq!(status.evictions, 1);
        assert_eq!(store.remove_binding("three"), 1);
        assert_eq!(store.status().entry_count, 1);
    }
}
