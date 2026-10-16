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
//! exceeded does the least recently used family go (a re-select or a call
//! refreshes recency), and the projection names what was unloaded so the
//! caller can tell the model.

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

/// TEST/EVAL FIXTURE limits (2 families). The runtime default is the
/// configured `agent_surface_runtime.working_set.max_loaded_families` (8).
impl Default for WorkingSetLimits {
    fn default() -> Self {
        Self {
            // A small FIXTURE value for unit tests and evals that exercise
            // eviction. No runtime surface reads this: Chat and voice take
            // `agent_surface_runtime.working_set` (default 8 families, see
            // `config.rs::default_surface_loaded_family_limit`), autonomous
            // runs `autonomous_compatibility`.
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
    /// Each loaded tool's identity digest ([`tool_identity_digest`]) at load
    /// time, so a later reconcile can tell whether the name still denotes
    /// the same tool.
    #[serde(default)]
    pub tool_digests: std::collections::BTreeMap<String, String>,
}

/// The identity of one loaded tool, as the surface working set compares it
/// across authority revisions. It covers exactly:
///
/// - the pack name and the tool (leaf or surface) name;
/// - the provider-visible parameter schema, canonicalized;
/// - the pack definition digest
///   ([`super::tool_index::pack_definition_digest`]): the whole
///   `CapabilityPackDefinition` — implementation block (provider, command,
///   governed runtime package), execution, auth, reliability, retry,
///   verification and preflight policies, parameters, action schemas and
///   result projection — minus prose (pack `description`/`guide`, action
///   `description`);
/// - the entry's result projection contract, canonicalized;
/// - the entry's provenance in its scope (`app:<installation id>`,
///   `embedded` fallback, or `scope` definition).
///
/// It does NOT cover description or guide text (prose grants nothing, and
/// the next catalog re-advertises it anyway), nor bytes outside the pack
/// definition, such as a script the definition's command runs. Two catalogs
/// that give a name the same digest give the model the same tool to call,
/// whatever else (other skills, the agent definition) changed around it.
pub fn tool_identity_digest(entry: &super::ToolIndexEntry) -> Option<String> {
    let mut schema = String::new();
    super::tool_index::write_canonical_json(&entry.parameters_schema, &mut schema);
    let mut projection = String::new();
    super::tool_index::write_canonical_json(
        &serde_json::to_value(&entry.result_projection).ok()?,
        &mut projection,
    );
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.working-set.tool-identity.v2\0");
    for part in [
        entry.pack_name.as_str(),
        entry.name.as_str(),
        schema.as_str(),
        entry.pack_definition_digest.as_str(),
        projection.as_str(),
        entry.provenance.as_str(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    Some(hasher.finalize().to_hex().to_string())
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
    let tool_digests = loaded_tools
        .iter()
        .filter_map(|name| {
            index
                .get(name)
                .and_then(tool_identity_digest)
                .map(|digest| (name.clone(), digest))
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
        tool_digests,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolWorkingSet {
    pub base_policy_snapshot_id: String,
    pub generation: u64,
    pub loaded_families: BTreeSet<String>,
    pub loaded_tools: BTreeSet<String>,
    pub schema_bytes: usize,
    /// Families from least to most recently used (loaded, re-selected, or
    /// called); the front is the first to go when a merge exceeds the
    /// family limit.
    pub family_order: Vec<String>,
    /// Each loaded tool's pack and priced schema size (from its projection).
    pub tool_packs: std::collections::BTreeMap<String, String>,
    pub tool_bytes: std::collections::BTreeMap<String, usize>,
    /// Each loaded tool's identity digest ([`tool_identity_digest`]) as it
    /// was when loaded. A lineage change keeps a name only while the current
    /// catalog still gives it this digest.
    pub tool_digests: std::collections::BTreeMap<String, String>,
    /// The authority lineage the loaded set was chosen under: a digest of
    /// the capability registry revision and the agent definition. Empty
    /// when the caller reconciles without one (every revision change then
    /// clears). See [`ToolWorkingSet::reconcile_in_lineage`].
    pub lineage_id: String,
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
/// Both interactive surfaces use this two-phase transition. Realtime voice
/// prepares the catalog, installs it at the provider, and commits only after
/// the provider acknowledges the update. Chat prepares, builds its next
/// provider catalog from [`Self::prospective_loaded_tools`], and commits in
/// the same step. [`SurfaceWorkingSetStore::select`] (prepare and commit in
/// one call) remains for the autonomous loop.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PreparedFamilyLoad {
    pub base_policy_snapshot_id: String,
    pub expected_generation: u64,
    /// What THIS select named. Not what the caller's next catalog holds —
    /// that is [`Self::prospective_loaded_tools`].
    pub projection: FamilyLoadProjection,
    /// The limits the commit merges under.
    pub limits: WorkingSetLimits,
    /// Families the merge evicts to stay within the limits, so the caller
    /// can tell the model its earlier tools are gone.
    unloaded_families: Vec<String>,
    /// The binding instance the prepare read (see
    /// `SurfaceWorkingSetEntry::instance`). Part of the commit's CAS so a
    /// binding removed and recreated at the same revision and generation
    /// (ABA) cannot accept a stale prepare.
    expected_instance: u64,
    /// The exact state `commit_prepared` installs. The commit is
    /// compare-and-swap on binding instance, policy and generation, so the
    /// committed set is always this prospective one.
    #[serde(skip)]
    prospective_state: ToolWorkingSet,
}

impl PreparedFamilyLoad {
    /// The complete loaded set once this selection merges into the current
    /// one: the families already loaded plus the selected ones, minus any
    /// evicted over the limits. A caller builds its next provider catalog
    /// from THIS set. Building it from `projection.loaded_tools` showed the
    /// model only the newest select while the store (and the select result)
    /// said both families were loaded; on 2026-10-08 a chat turn re-selected
    /// one tau2 tool six times because every select hid the other.
    pub fn prospective_loaded_tools(&self) -> Vec<String> {
        self.prospective_state.loaded_tool_names()
    }

    /// Families the merge evicts to stay within the limits.
    pub fn unloaded_families(&self) -> &[String] {
        &self.unloaded_families
    }

    /// The working-set generation once this selection commits. Equal to
    /// `expected_generation` when the selection loads nothing new (a
    /// re-select only refreshes recency).
    pub fn prospective_generation(&self) -> u64 {
        self.prospective_state.generation
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PreparedFamilyLoadCommitError {
    WorkingSetMissing,
    PolicyChanged {
        expected: String,
        actual: String,
    },
    GenerationChanged {
        expected: u64,
        actual: u64,
    },
    /// The binding was removed and recreated since the prepare.
    BindingReplaced {
        expected: u64,
        actual: u64,
    },
}

#[derive(Debug, Clone)]
struct SurfaceWorkingSetEntry {
    state: ToolWorkingSet,
    last_updated_at_ms: u64,
    last_access_sequence: u64,
    /// A store-unique number minted when this binding is created. Removal
    /// followed by recreation yields a new instance even though the policy
    /// id and generation (0) can repeat.
    instance: u64,
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
/// policy snapshot and authorized-name ceiling supplied by the caller, before
/// a subsequent provider request. A same-revision narrowing removes revoked
/// leaves immediately. A policy revision change keeps only loaded names that
/// are still authorized AND still denote the same tool (see
/// [`ToolWorkingSet::reconcile_in_lineage`]); a caller that supplies no
/// lineage or identity index gets the conservative full clear.
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
    /// Any revision change clears the loaded set; see
    /// [`Self::reconcile_in_lineage`] for the identity-aware form.
    pub fn reconcile(
        &self,
        key: &SurfaceWorkingSetKey,
        policy_snapshot_id: &str,
        authorized_names: &HashSet<String>,
    ) -> SurfaceWorkingSetSnapshot {
        self.reconcile_in_lineage(key, policy_snapshot_id, None, None, authorized_names)
    }

    /// [`Self::reconcile`] with an authority lineage (registry revision +
    /// agent definition digest) and the current surface tool index. On a
    /// revision change a loaded name survives only while it is still
    /// authorized and the index still gives it the identity digest it was
    /// loaded with. See [`ToolWorkingSet::reconcile_in_lineage`] for the
    /// rule and why it is safe.
    pub fn reconcile_in_lineage(
        &self,
        key: &SurfaceWorkingSetKey,
        policy_snapshot_id: &str,
        lineage_id: Option<&str>,
        index: Option<&ToolIndex>,
        authorized_names: &HashSet<String>,
    ) -> SurfaceWorkingSetSnapshot {
        self.reconcile_entry(
            key,
            policy_snapshot_id,
            lineage_id,
            index,
            authorized_names,
            |entry, created| entry.snapshot_marked(created),
        )
    }

    /// Reconcile the binding and read it under ONE shard lock, creating it
    /// when absent. `read` sees the entry as reconciled and whether this call
    /// created it. Anything a caller pins for a later compare-and-swap must
    /// come from here: a value read under a second lock could belong to a
    /// state a concurrent reconcile already replaced.
    fn reconcile_entry<T>(
        &self,
        key: &SurfaceWorkingSetKey,
        policy_snapshot_id: &str,
        lineage_id: Option<&str>,
        index: Option<&ToolIndex>,
        authorized_names: &HashSet<String>,
        read: impl FnOnce(&SurfaceWorkingSetEntry, bool) -> T,
    ) -> T {
        self.metrics.reads.fetch_add(1, Ordering::Relaxed);
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let now = now_epoch_ms();
        match self.entries.entry(key.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                let prior_lineage = entry.state.lineage_id.clone();
                let prior_tools = entry.state.loaded_tools.len();
                if entry.state.reconcile_in_lineage(
                    policy_snapshot_id,
                    lineage_id,
                    index,
                    authorized_names,
                ) {
                    entry.last_updated_at_ms = now;
                    self.metrics.mutations.fetch_add(1, Ordering::Relaxed);
                }
                // A lineage change while tools are loaded is what a session
                // feels as "my tools vanished". It should be rare (a skill or
                // owner-definition change); at INFO so an unexpected one is
                // visible without debug logging.
                // Compare what was stored before and after: an entry with no
                // lineage yet (created by a select, or reset by a policy-only
                // reconcile) gaining one is not a change.
                if prior_tools > 0
                    && !prior_lineage.is_empty()
                    && entry.state.lineage_id != prior_lineage
                {
                    tracing::info!(
                        principal = %key.principal,
                        workspace = %key.workspace,
                        agent_id = %key.agent_id,
                        binding_id = %key.binding_id,
                        prior_lineage = %prior_lineage,
                        lineage = %entry.state.lineage_id,
                        policy_snapshot_id,
                        prior_tools,
                        kept_tools = entry.state.loaded_tools.len(),
                        "[WORKING-SET] lineage changed for a session with loaded tools"
                    );
                }
                entry.last_access_sequence = sequence;
                read(entry, false)
            },
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                self.metrics.creates.fetch_add(1, Ordering::Relaxed);
                let mut state = ToolWorkingSet::new(policy_snapshot_id);
                state.lineage_id = lineage_id.unwrap_or_default().to_string();
                let entry = SurfaceWorkingSetEntry {
                    state,
                    last_updated_at_ms: now,
                    last_access_sequence: sequence,
                    instance: sequence,
                };
                let value = read(&entry, true);
                vacant.insert(entry);
                self.evict_to_limit();
                value
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
                instance: sequence,
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
        self.prepare_select_in_lineage(
            key,
            policy_snapshot_id,
            None,
            index,
            selected_names,
            authorized_names,
            limits,
        )
    }

    /// [`Self::prepare_select`] reconciling under an authority lineage and
    /// the selection's own index (see [`Self::reconcile_in_lineage`]). The
    /// prepared value carries the prospective MERGED set, computed on a copy
    /// of the reconciled state, so a caller can build its next catalog from
    /// exactly what the commit will install.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_select_in_lineage(
        &self,
        key: &SurfaceWorkingSetKey,
        policy_snapshot_id: &str,
        lineage_id: Option<&str>,
        index: &ToolIndex,
        selected_names: &[String],
        authorized_names: &HashSet<String>,
        limits: WorkingSetLimits,
    ) -> Result<PreparedFamilyLoad, FamilyLoadError> {
        // Reconcile and copy under one lock, and pin the compare-and-swap to
        // that copy. Reading the copy under a second lock let a concurrent
        // reconcile land in between: the prepare was authorized at one
        // revision while its CAS pinned the next, so it could commit into a
        // policy it was never checked against.
        let (expected_instance, mut prospective_state) = self.reconcile_entry(
            key,
            policy_snapshot_id,
            lineage_id,
            Some(index),
            authorized_names,
            |entry, _| (entry.instance, entry.state.clone()),
        );
        let base_policy_snapshot_id = prospective_state.base_policy_snapshot_id.clone();
        let expected_generation = prospective_state.generation;
        let projection = project_family_selection(index, selected_names, authorized_names, limits)?;
        let unloaded_families = prospective_state
            .merge(&projection, limits)
            .unwrap_or_default();
        Ok(PreparedFamilyLoad {
            base_policy_snapshot_id,
            expected_generation,
            projection,
            limits,
            unloaded_families,
            expected_instance,
            prospective_state,
        })
    }

    /// Mark the family that owns `tool_name` as the most recently used, so
    /// a later merge over the family limit evicts a family the model is NOT
    /// using. Recency only: the loaded set and the generation are unchanged,
    /// so a pending prepared commit is not invalidated. Returns whether a
    /// loaded family was touched.
    pub fn touch_tool(&self, key: &SurfaceWorkingSetKey, tool_name: &str) -> bool {
        let Some(mut entry) = self.entries.get_mut(key) else {
            return false;
        };
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let touched = entry.state.touch_tool(tool_name);
        if touched {
            entry.last_access_sequence = sequence;
        }
        touched
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
        if entry.instance != prepared.expected_instance {
            return Err(PreparedFamilyLoadCommitError::BindingReplaced {
                expected: prepared.expected_instance,
                actual: entry.instance,
            });
        }
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
        // Install the exact state the caller built its catalog from. The CAS
        // above pins binding instance, policy and generation; recency touches
        // since the prepare do not bump the generation, and are superseded
        // here so the committed set can never differ from the advertised one.
        entry.state = prepared.prospective_state.clone();
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
            tool_digests: std::collections::BTreeMap::new(),
            lineage_id: String::new(),
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
        self.reconcile_in_lineage(policy_snapshot_id, None, None, authorized_names)
    }

    /// Reconcile under an authority lineage and the current tool index.
    ///
    /// Same revision: remove only names the ceiling no longer authorizes.
    ///
    /// Revision changed — the identity rule, used whenever a lineage AND the
    /// current `index` are supplied and every loaded name carries a load-time
    /// identity digest: keep a name only if (a) the NEW `authorized_names`
    /// still contains it and (b) `index` still gives it the same
    /// [`tool_identity_digest`] (pack, name, parameter schema) it was loaded
    /// with; drop everything else. Without digests: same non-empty lineage
    /// keeps what is still authorized, anything else clears. With no lineage
    /// (autonomous runs, legacy callers): clear.
    ///
    /// Why keeping is safe: the working set holds tool NAMES and nothing
    /// else — no schema, grant, approval rule or trust decision. Every
    /// provider catalog and dispatch ceiling is re-resolved from the current
    /// policy snapshot, and loaded names only pick which of the CURRENTLY
    /// authorized tools are shown. Intersecting with the new ceiling
    /// therefore can never widen authority; a revoked tool goes the moment
    /// its revision lands. The remaining risk is name identity — after a
    /// registry or definition change a retained name might denote a
    /// different tool than the one the model selected — and the digest check
    /// closes it per name: a tool whose pack, name or schema moved is dropped
    /// and must be re-selected knowingly, while an unrelated skill or
    /// definition edit no longer unloads everything. Before this, ANY hashed
    /// input change cleared the set — a rebuilt catalog with re-ordered
    /// delegation targets made a chat turn forget every tool it had loaded.
    pub fn reconcile_in_lineage(
        &mut self,
        policy_snapshot_id: &str,
        lineage_id: Option<&str>,
        index: Option<&ToolIndex>,
        authorized_names: &HashSet<String>,
    ) -> bool {
        let changed = if self.base_policy_snapshot_id != policy_snapshot_id {
            let lineage = lineage_id.filter(|lineage| !lineage.is_empty());
            let same_lineage = lineage.is_some_and(|lineage| lineage == self.lineage_id);
            let identity_index = index.filter(|_| {
                lineage.is_some()
                    && self
                        .loaded_tools
                        .iter()
                        .all(|name| self.tool_digests.contains_key(name))
            });
            self.base_policy_snapshot_id = policy_snapshot_id.to_string();
            self.lineage_id = lineage_id.unwrap_or_default().to_string();
            if let Some(index) = identity_index {
                let keep = self
                    .loaded_tools
                    .iter()
                    .filter(|name| {
                        authorized_names.contains(name.as_str())
                            && index
                                .get(name.as_str())
                                .and_then(tool_identity_digest)
                                .as_ref()
                                == self.tool_digests.get(name.as_str())
                    })
                    .cloned()
                    .collect::<HashSet<String>>();
                self.retain_tools(&keep);
            } else if same_lineage {
                self.retain_tools(authorized_names);
            } else {
                self.loaded_families.clear();
                self.loaded_tools.clear();
                self.family_order.clear();
                self.tool_packs.clear();
                self.tool_bytes.clear();
                self.tool_digests.clear();
                self.schema_bytes = 0;
            }
            true
        } else {
            if let Some(lineage) = lineage_id {
                if self.lineage_id.is_empty() {
                    self.lineage_id = lineage.to_string();
                }
            }
            self.retain_tools(authorized_names)
        };
        if changed {
            self.generation = self.generation.saturating_add(1);
        }
        changed
    }

    /// Drop every loaded tool outside `keep`, and every family, pack
    /// mapping, price and digest that no longer has a loaded tool. Returns
    /// whether any tool was dropped.
    fn retain_tools(&mut self, keep: &HashSet<String>) -> bool {
        let before = self.loaded_tools.len();
        self.loaded_tools
            .retain(|name| keep.contains(name.as_str()));
        if before == self.loaded_tools.len() {
            return false;
        }
        let tools = &self.loaded_tools;
        self.tool_packs.retain(|name, _| tools.contains(name));
        self.tool_bytes.retain(|name, _| tools.contains(name));
        self.tool_digests.retain(|name, _| tools.contains(name));
        let live_families: BTreeSet<String> = self
            .loaded_tools
            .iter()
            .map(|name| family_of(name, &self.tool_packs))
            .collect();
        self.loaded_families
            .retain(|family| live_families.contains(family));
        self.family_order
            .retain(|family| live_families.contains(family));
        self.schema_bytes = self.tool_bytes.values().sum();
        true
    }

    /// Move the family that owns `tool_name` to the most-recent end of the
    /// eviction order. Recency only — no generation bump.
    pub fn touch_tool(&mut self, tool_name: &str) -> bool {
        if !self.loaded_tools.contains(tool_name) {
            return false;
        }
        let family = family_of(tool_name, &self.tool_packs);
        let Some(position) = self.family_order.iter().position(|f| *f == family) else {
            return false;
        };
        if position + 1 != self.family_order.len() {
            let family = self.family_order.remove(position);
            self.family_order.push(family);
        }
        true
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
        self.tool_digests = projection.tool_digests.clone();
        self.schema_bytes = projection.schema_bytes;
        self.generation = self.generation.saturating_add(1);
        true
    }

    /// Merge a successful selection into the loaded set. Families already
    /// loaded stay; when the union exceeds the family limit (or the tool or
    /// schema budget), the least recently used families are evicted with
    /// every tool of theirs until it fits, and the evicted names are returned
    /// so the caller
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
        let mut tool_digests = self.tool_digests.clone();
        tool_digests.extend(
            projection
                .tool_digests
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
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
        tool_digests.retain(|name, _| tools.contains(name));
        let families: BTreeSet<String> = order.iter().cloned().collect();
        if self.loaded_families == families
            && self.loaded_tools == tools
            && self.schema_bytes == schema_bytes
        {
            // Nothing loaded or unloaded, but a re-selected family is now the
            // most recent. Returning before this kept the LOAD order, so the
            // family the model kept re-selecting (and using) was the next
            // one evicted. Recency only: the generation stays put. The
            // re-select also re-records the identity it was chosen at.
            self.family_order = order;
            self.tool_digests = tool_digests;
            return Some(unloaded);
        }
        self.loaded_families = families;
        self.family_order = order;
        self.loaded_tools = tools;
        self.tool_packs = tool_packs;
        self.tool_bytes = tool_bytes;
        self.tool_digests = tool_digests;
        self.schema_bytes = schema_bytes;
        self.generation = self.generation.saturating_add(1);
        Some(unloaded)
    }

    pub fn loaded_tool_names(&self) -> Vec<String> {
        self.loaded_tools.iter().cloned().collect()
    }
}

/// The family (pack) a loaded tool belongs to: its recorded pack, else the
/// `<pack>__<action>` prefix, else the name itself (a whole-pack entry).
fn family_of(name: &str, packs: &std::collections::BTreeMap<String, String>) -> String {
    packs.get(name).cloned().unwrap_or_else(|| {
        name.split_once("__")
            .map_or(name, |(pack, _)| pack)
            .to_string()
    })
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
        assert_eq!(
            prepared.prospective_loaded_tools(),
            vec!["browser__click", "browser__open"]
        );
        assert!(prepared.unloaded_families().is_empty());
        assert_eq!(prepared.prospective_generation(), 1);
        assert!(store
            .reconcile(&key, "policy-a", &allowed)
            .loaded_tools
            .is_empty());

        let committed = store
            .commit_prepared(&key, &prepared)
            .expect("commit acknowledged catalog");
        assert_eq!(committed.loaded_families, vec!["browser"]);
        assert_eq!(committed.generation, 1);
        assert_eq!(committed.loaded_tools, prepared.prospective_loaded_tools());

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

    /// The catalog a caller builds after a select must hold every loaded
    /// family, not just the one this select named. On 2026-10-08 a chat turn
    /// was shown only its newest select while the store (and the select
    /// result) said two families were loaded; it re-selected one tau2 tool
    /// six times.
    #[test]
    fn prepare_select_projects_the_merged_set() {
        let store = SurfaceWorkingSetStore::new(16);
        let index = fixture_index();
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "merged");
        let allowed = HashSet::from(["browser__open".to_string(), "memory__search".to_string()]);
        let two = WorkingSetLimits {
            max_loaded_families: 2,
            ..WorkingSetLimits::default()
        };
        let first = store
            .prepare_select(
                &key,
                "policy-a",
                &index,
                &["browser__open".to_string()],
                &allowed,
                two,
            )
            .expect("prepare browser");
        store.commit_prepared(&key, &first).expect("commit browser");

        let second = store
            .prepare_select(
                &key,
                "policy-a",
                &index,
                &["memory__search".to_string()],
                &allowed,
                two,
            )
            .expect("prepare memory");
        assert_eq!(second.projection.loaded_tools, vec!["memory__search"]);
        assert_eq!(
            second.prospective_loaded_tools(),
            vec!["browser__open", "memory__search"],
            "the next catalog keeps the family already loaded"
        );
        assert!(second.unloaded_families().is_empty());
        let committed = store.commit_prepared(&key, &second).expect("commit memory");
        assert_eq!(committed.loaded_tools, second.prospective_loaded_tools());
        assert_eq!(committed.generation, second.prospective_generation());

        // Over the limit the prospective set already shows the eviction, and
        // names it, so the advertised catalog and the commit agree.
        let one = WorkingSetLimits {
            max_loaded_families: 1,
            ..WorkingSetLimits::default()
        };
        let third = store
            .prepare_select(
                &key,
                "policy-a",
                &index,
                &["browser__open".to_string()],
                &allowed,
                one,
            )
            .expect("prepare browser again");
        assert_eq!(third.prospective_loaded_tools(), vec!["browser__open"]);
        assert_eq!(third.unloaded_families(), ["memory".to_string()]);
        let committed = store.commit_prepared(&key, &third).expect("commit");
        assert_eq!(committed.loaded_tools, third.prospective_loaded_tools());
    }

    /// A prepare is pinned to the exact state it was merged from, read under
    /// the same lock as its reconcile. Two prepares taken at one generation
    /// cannot both commit, and a reconcile that lands between a prepare and
    /// its commit (a new revision) fails the commit closed.
    #[test]
    fn a_prepare_commits_only_into_the_state_it_was_prepared_against() {
        let store = SurfaceWorkingSetStore::new(16);
        let index = fixture_index();
        let key = binding(InvocationSurface::RealtimeVoice, FeatureMode::None, "cas");
        let allowed = HashSet::from(["browser__open".to_string(), "memory__search".to_string()]);
        let limits = WorkingSetLimits::default();

        let browser = store
            .prepare_select(
                &key,
                "rev-1",
                &index,
                &["browser__open".to_string()],
                &allowed,
                limits,
            )
            .expect("prepare browser");
        let memory = store
            .prepare_select(
                &key,
                "rev-1",
                &index,
                &["memory__search".to_string()],
                &allowed,
                limits,
            )
            .expect("prepare memory");
        assert_eq!(browser.expected_generation, memory.expected_generation);
        store
            .commit_prepared(&key, &browser)
            .expect("first commit wins");
        assert!(matches!(
            store.commit_prepared(&key, &memory),
            Err(PreparedFamilyLoadCommitError::GenerationChanged { .. })
        ));

        // A reconcile at a new revision between prepare and commit.
        let pending = store
            .prepare_select(
                &key,
                "rev-1",
                &index,
                &["memory__search".to_string()],
                &allowed,
                limits,
            )
            .expect("prepare memory at rev-1");
        store.reconcile(&key, "rev-2", &allowed);
        assert!(matches!(
            store.commit_prepared(&key, &pending),
            Err(PreparedFamilyLoadCommitError::PolicyChanged { .. })
        ));

        // ABA: the binding is removed and recreated at the same revision and
        // generation; a prepare against the old binding must not commit.
        let stale = store
            .prepare_select(
                &key,
                "rev-2",
                &index,
                &["memory__search".to_string()],
                &allowed,
                limits,
            )
            .expect("prepare against the old binding");
        assert!(store.remove(&key));
        let recreated = store.reconcile(&key, "rev-2", &allowed);
        assert!(recreated.created);
        assert_eq!(recreated.generation, 0);
        assert!(matches!(
            store.commit_prepared(&key, &stale),
            Err(PreparedFamilyLoadCommitError::BindingReplaced { .. })
        ));
    }

    /// Eviction must follow use, not load order. The live turn re-selected
    /// (and kept calling) `order_details`, then a third select evicted
    /// exactly that family because it had been LOADED first.
    #[test]
    fn reselecting_a_loaded_family_refreshes_its_recency() {
        let index = build_tool_index(&[
            pack("alpha", &["run"]),
            pack("beta", &["run"]),
            pack("gamma", &["run"]),
        ]);
        let allowed = HashSet::from([
            "alpha__run".to_string(),
            "beta__run".to_string(),
            "gamma__run".to_string(),
        ]);
        let limits = WorkingSetLimits {
            max_loaded_families: 2,
            ..WorkingSetLimits::default()
        };
        let select = |name: &str| {
            project_family_selection(&index, &[name.to_string()], &allowed, limits)
                .expect("projection")
        };

        let mut state = ToolWorkingSet::new("policy");
        state.merge(&select("alpha__run"), limits);
        state.merge(&select("beta__run"), limits);
        let generation = state.generation;
        // Re-select alpha: nothing loads, but alpha is now the most recent.
        assert_eq!(state.merge(&select("alpha__run"), limits), Some(vec![]));
        assert_eq!(state.generation, generation, "recency is not a load");
        assert_eq!(
            state.merge(&select("gamma__run"), limits),
            Some(vec!["beta".to_string()])
        );
        assert_eq!(state.loaded_tool_names(), vec!["alpha__run", "gamma__run"]);

        // Dispatching a loaded tool refreshes recency the same way.
        assert!(state.touch_tool("alpha__run"));
        assert!(!state.touch_tool("beta__run"), "not loaded");
        assert_eq!(
            state.merge(&select("beta__run"), limits),
            Some(vec!["gamma".to_string()])
        );
        assert_eq!(state.loaded_tool_names(), vec!["alpha__run", "beta__run"]);

        // The store form touches without bumping the generation.
        let store = SurfaceWorkingSetStore::new(4);
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "touch");
        store
            .select(
                &key,
                "policy",
                &index,
                &["alpha__run".to_string()],
                &allowed,
                limits,
            )
            .expect("alpha");
        let (_, before) = store
            .select(
                &key,
                "policy",
                &index,
                &["beta__run".to_string()],
                &allowed,
                limits,
            )
            .expect("beta");
        assert!(store.touch_tool(&key, "alpha__run"));
        let (projection, after) = store
            .select(
                &key,
                "policy",
                &index,
                &["gamma__run".to_string()],
                &allowed,
                limits,
            )
            .expect("gamma");
        assert_eq!(projection.unloaded_families, vec!["beta"]);
        assert_eq!(after.generation, before.generation + 1);
    }

    fn pack_with_param(name: &str, actions: &[&str], param: &str) -> CapabilityPackDefinition {
        let actions = actions
            .iter()
            .map(|action| {
                format!(
                    "  {action}:\n    description: {action}\n    parameters: [{param}]\n    required: [{param}]\n    parameter_overrides:\n      {param}: {{type: string}}\n"
                )
            })
            .collect::<String>();
        serde_yaml::from_str(&format!(
            "name: {name}\ndescription: {name}\nparameters: []\nnative_action_schemas:\n{actions}implementation:\n  type: primitive\n  provider_name: {name}\n"
        ))
        .expect("pack")
    }

    /// The long-term identity rule. A lineage change (registry or definition
    /// edit) keeps a loaded name only while it is still authorized AND the
    /// current index gives it the identity digest it was loaded with: an
    /// unrelated skill change keeps everything, a schema change of a loaded
    /// tool drops that tool, a revocation drops it, and a set without
    /// recorded digests falls back to the full clear.
    #[test]
    fn a_lineage_change_keeps_a_name_only_while_its_identity_holds() {
        let store = SurfaceWorkingSetStore::new(8);
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "identity");
        let original = fixture_index();
        let broad = HashSet::from(["browser__open".to_string(), "memory__search".to_string()]);
        let limits = WorkingSetLimits::default();
        for name in ["browser__open", "memory__search"] {
            let prepared = store
                .prepare_select_in_lineage(
                    &key,
                    "rev-1",
                    Some("lineage-a"),
                    &original,
                    &[name.to_string()],
                    &broad,
                    limits,
                )
                .expect("prepare");
            store.commit_prepared(&key, &prepared).expect("commit");
        }

        // An unrelated skill was installed: new registry revision (new
        // lineage), but both loaded tools are byte-for-byte the same tool.
        let unrelated = build_tool_index(&[
            pack("browser", &["open", "click", "snapshot"]),
            pack("memory", &["search", "save"]),
            pack("weather", &["forecast"]),
        ]);
        let kept =
            store.reconcile_in_lineage(&key, "rev-2", Some("lineage-b"), Some(&unrelated), &broad);
        assert_eq!(kept.loaded_tools, vec!["browser__open", "memory__search"]);

        // The browser skill changed its schema: `browser__open` now denotes a
        // different tool and goes; memory stays.
        let reshaped = build_tool_index(&[
            pack_with_param("browser", &["open", "click", "snapshot"], "address"),
            pack("memory", &["search", "save"]),
        ]);
        let narrowed =
            store.reconcile_in_lineage(&key, "rev-3", Some("lineage-c"), Some(&reshaped), &broad);
        assert_eq!(narrowed.loaded_tools, vec!["memory__search"]);
        assert_eq!(narrowed.loaded_families, vec!["memory"]);

        // Authority revoked: dropped even though its identity is unchanged.
        let revoked = store.reconcile_in_lineage(
            &key,
            "rev-4",
            Some("lineage-d"),
            Some(&reshaped),
            &HashSet::new(),
        );
        assert!(revoked.loaded_tools.is_empty());

        // No recorded digest for a loaded name: identity cannot be checked,
        // so a lineage change clears.
        let mut state = ToolWorkingSet::new("rev-1");
        state.lineage_id = "lineage-a".to_string();
        state.loaded_families.insert("memory".to_string());
        state.loaded_tools.insert("memory__search".to_string());
        assert!(state.reconcile_in_lineage("rev-2", Some("lineage-b"), Some(&original), &broad));
        assert!(state.loaded_tools.is_empty());
        assert!(state.loaded_families.is_empty());
    }

    /// `pack(name, actions)` with a different provider behind it: identical
    /// actions and parameters, only the implementation block differs.
    fn pack_with_provider(
        name: &str,
        actions: &[&str],
        provider: &str,
    ) -> CapabilityPackDefinition {
        let actions = actions
            .iter()
            .map(|action| {
                format!(
                    "  {action}:\n    description: {action}\n    parameters: [value]\n    required: [value]\n    parameter_overrides:\n      value: {{type: string}}\n"
                )
            })
            .collect::<String>();
        serde_yaml::from_str(&format!(
            "name: {name}\ndescription: {name}\nparameters: []\nnative_action_schemas:\n{actions}implementation:\n  type: primitive\n  provider_name: {provider}\n"
        ))
        .expect("pack")
    }

    /// Same name, same schema is not the same tool. A change confined to the
    /// implementation block (the provider behind `browser__open`) or to the
    /// origin (a first-party `memory` now served by an app installation)
    /// drops that name on a lineage change; an unrelated install still keeps
    /// everything.
    #[test]
    fn identity_covers_the_implementation_and_the_origin_not_just_the_schema() {
        let store = SurfaceWorkingSetStore::new(8);
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "identity-v2");
        let scope = |_: &str| "scope".to_string();
        let mut original = fixture_index();
        original.stamp_provenance(scope);
        let broad = HashSet::from(["browser__open".to_string(), "memory__search".to_string()]);
        let limits = WorkingSetLimits::default();
        for name in ["browser__open", "memory__search"] {
            let prepared = store
                .prepare_select_in_lineage(
                    &key,
                    "rev-1",
                    Some("lineage-a"),
                    &original,
                    &[name.to_string()],
                    &broad,
                    limits,
                )
                .expect("prepare");
            store.commit_prepared(&key, &prepared).expect("commit");
        }

        let mut unrelated = build_tool_index(&[
            pack("browser", &["open", "click", "snapshot"]),
            pack("memory", &["search", "save"]),
            pack("weather", &["forecast"]),
        ]);
        unrelated.stamp_provenance(scope);
        let kept =
            store.reconcile_in_lineage(&key, "rev-2", Some("lineage-b"), Some(&unrelated), &broad);
        assert_eq!(kept.loaded_tools, vec!["browser__open", "memory__search"]);

        // Implementation-only: the provider behind browser changed, the
        // provider-visible schema did not.
        let mut rewired = build_tool_index(&[
            pack_with_provider("browser", &["open", "click", "snapshot"], "browser_v2"),
            pack("memory", &["search", "save"]),
        ]);
        rewired.stamp_provenance(scope);
        assert_eq!(
            rewired
                .get("browser__open")
                .expect("rewired")
                .parameters_schema,
            original
                .get("browser__open")
                .expect("original")
                .parameters_schema,
            "the change must be implementation-only for this case to mean anything"
        );
        let after_rewire =
            store.reconcile_in_lineage(&key, "rev-3", Some("lineage-c"), Some(&rewired), &broad);
        assert_eq!(after_rewire.loaded_tools, vec!["memory__search"]);

        // Origin-only: the identical memory definition is now an app's.
        let mut app_served = build_tool_index(&[
            pack_with_provider("browser", &["open", "click", "snapshot"], "browser_v2"),
            pack("memory", &["search", "save"]),
        ]);
        app_served.stamp_provenance(|pack_name: &str| {
            if pack_name == "memory" {
                "app:install_1".to_string()
            } else {
                "scope".to_string()
            }
        });
        let after_origin =
            store.reconcile_in_lineage(&key, "rev-4", Some("lineage-d"), Some(&app_served), &broad);
        assert!(after_origin.loaded_tools.is_empty());
    }

    /// Without an identity index: a revision change inside one authority
    /// lineage keeps what the new ceiling still authorizes and drops the
    /// rest; a new lineage — or a caller with no lineage — still clears.
    #[test]
    fn revision_change_keeps_still_authorized_and_drops_revoked() {
        let index = fixture_index();
        let store = SurfaceWorkingSetStore::new(8);
        let key = binding(InvocationSurface::Chat, FeatureMode::None, "lineage");
        let broad = HashSet::from(["browser__open".to_string(), "memory__search".to_string()]);
        let limits = WorkingSetLimits::default();
        let load = |revision: &str, lineage: &str, name: &str, allowed: &HashSet<String>| {
            let prepared = store
                .prepare_select_in_lineage(
                    &key,
                    revision,
                    Some(lineage),
                    &index,
                    &[name.to_string()],
                    allowed,
                    limits,
                )
                .expect("prepare");
            store.commit_prepared(&key, &prepared).expect("commit")
        };
        load("rev-1", "lineage-a", "browser__open", &broad);
        let both = load("rev-1", "lineage-a", "memory__search", &broad);
        assert_eq!(both.loaded_tools, vec!["browser__open", "memory__search"]);

        // New revision, same lineage, browser revoked: memory survives.
        let narrowed = HashSet::from(["memory__search".to_string()]);
        let kept = store.reconcile_in_lineage(&key, "rev-2", Some("lineage-a"), None, &narrowed);
        assert_eq!(kept.base_policy_snapshot_id, "rev-2");
        assert_eq!(kept.loaded_tools, vec!["memory__search"]);
        assert_eq!(kept.loaded_families, vec!["memory"]);
        assert!(kept.generation > both.generation);

        // New revision AND new lineage, no index to check identity: clear.
        let cleared = store.reconcile_in_lineage(&key, "rev-3", Some("lineage-b"), None, &broad);
        assert!(cleared.loaded_tools.is_empty());
        assert!(cleared.loaded_families.is_empty());

        // A caller with no lineage keeps the old clear-on-any-change rule.
        load("rev-3", "lineage-b", "memory__search", &broad);
        let legacy = store.reconcile(&key, "rev-4", &broad);
        assert!(legacy.loaded_tools.is_empty());

        // The ToolWorkingSet form, directly.
        let mut state = ToolWorkingSet::new("rev-1");
        state.lineage_id = "lineage-a".to_string();
        state.merge(
            &project_family_selection(&index, &["browser__open".to_string()], &broad, limits)
                .expect("browser"),
            limits,
        );
        assert!(state.reconcile_in_lineage("rev-2", Some("lineage-a"), None, &broad));
        assert_eq!(state.loaded_tool_names(), vec!["browser__open"]);
        assert!(state.reconcile_in_lineage("rev-3", Some("lineage-a"), None, &narrowed));
        assert!(state.loaded_tools.is_empty());
        assert!(state.family_order.is_empty());
        assert!(state.tool_packs.is_empty());
        assert!(state.tool_digests.is_empty());
        assert_eq!(state.schema_bytes, 0);
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
