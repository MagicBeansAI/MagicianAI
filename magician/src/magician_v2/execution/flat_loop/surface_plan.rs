//! Immutable, revisioned surface-plan cache shared by Chat and realtime voice.
//!
//! A surface plan is a provider projection of an already-resolved authority
//! ceiling. It cannot grant tools: the canonical Effective Tool Policy
//! Snapshot remains the dispatch authority. This layer only divides the
//! authorized direct universe into initial-hot, loaded, and deferred tiers.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use serde::Serialize;

use super::{DeferredEntry, ToolIndex};
use crate::magician_v2::agents::{FeatureMode, InvocationSurface};
use crate::magician_v2::execution::agentic::{
    native_types::NativeExecutionTool, EffectiveToolPolicySnapshot,
};

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

pub fn provider_schema_bytes(tools: &[NativeExecutionTool]) -> usize {
    serde_json::to_vec(tools)
        .map(|encoded| encoded.len())
        .unwrap_or_default()
}

/// Build the stable L3/L2 authority revision for an autonomous owner frame.
///
/// `EffectiveToolPolicySnapshot::snapshot_id` deliberately includes the
/// provider-visible/deferred split, so it changes when `tool_search` loads a
/// family. A working set cannot use that id as its base revision or every
/// successful selection would invalidate itself on the next decision. This
/// digest keeps every authorization-bearing input while canonicalizing direct
/// and deferred business grants into one universe.
pub fn autonomous_authority_revision(
    registry_revision: &str,
    snapshot: &EffectiveToolPolicySnapshot,
) -> Result<String, serde_json::Error> {
    let business_tools = snapshot
        .direct_tools
        .keys()
        .chain(snapshot.deferred_tools.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "registry_revision": registry_revision,
        "agent_id": snapshot.agent_id,
        "definition_version": snapshot.definition_version,
        "definition_digest": snapshot.definition_digest,
        "principal": snapshot.invocation.principal,
        "workspace": snapshot.invocation.workspace,
        "source_agent_id": snapshot.invocation.source_agent_id,
        "source_kind": snapshot.invocation.source_kind,
        "surface": snapshot.invocation.surface,
        "feature_mode": snapshot.invocation.feature_mode,
        "trust_level": snapshot.trust_level,
        "business_tools": business_tools,
        "runtime_tools": snapshot.runtime_tools,
        "implicit_tools": snapshot.implicit_tools,
        "structural_tools": snapshot.structural_tools,
        "delegation_targets": snapshot.delegation_targets,
        "handover_targets": snapshot.handover_targets,
        "denied_tool_names": snapshot.denied_tool_names,
        "denied_tool_params": snapshot.denied_tool_params,
        "approval_rules": snapshot.approval_rules,
        "delegate_owned_tool_names": snapshot.delegate_owned_tool_names,
    }))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Expand pack-level grants into their exact leaf names. Legacy/single-leaf
/// grants remain unchanged when the index has no pack mapping.
pub fn expand_direct_grants_to_leaves<I, S>(index: &ToolIndex, grants: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut names = BTreeSet::new();
    for grant in grants {
        let grant = grant.as_ref();
        let leaves = index.leaf_names_for_pack(grant);
        if leaves.is_empty() {
            names.insert(grant.to_string());
        } else {
            names.extend(leaves);
        }
    }
    names
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct SurfacePlanKey {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    /// Stable authority/cache revision. It excludes mutable loaded-family
    /// state so selecting a family does not invalidate itself.
    pub authority_revision: String,
}

/// Stable prompt-prefix cache key. Dynamic memory, current-turn text, files,
/// and transcript history are deliberately excluded by callers and remain in
/// the user/context portion of the provider request.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct StaticPromptKey {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    pub prompt_name: String,
    /// Digest of prompt version plus static render variables (persona,
    /// effective tool projection, and stable surface policy).
    pub context_revision: String,
}

pub fn static_prompt_context_revision(
    prompt_name: &str,
    prompt_version: &str,
    variables: &std::collections::HashMap<String, String>,
) -> String {
    let stable_variables = variables
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let bytes =
        serde_json::to_vec(&(prompt_name, prompt_version, stable_variables)).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectiveSurfacePlan {
    pub authority_revision: String,
    pub registry_revision: String,
    pub invocation_surface: InvocationSurface,
    pub feature_mode: FeatureMode,
    pub initial_hot: Vec<NativeExecutionTool>,
    pub loaded_tools: Vec<NativeExecutionTool>,
    pub deferred: Vec<DeferredEntry>,
    pub runtime_tool_names: BTreeSet<String>,
    pub structural_tool_names: BTreeSet<String>,
    pub authorized_business_tool_names: BTreeSet<String>,
    pub provider_schema_bytes: usize,
    pub built_at_ms: u64,
}

impl EffectiveSurfacePlan {
    pub fn provider_tools(&self) -> Vec<NativeExecutionTool> {
        let mut seen = HashSet::new();
        self.initial_hot
            .iter()
            .chain(self.loaded_tools.iter())
            .filter(|tool| seen.insert(tool.name.clone()))
            .cloned()
            .collect()
    }

    pub fn visible_business_names(&self) -> BTreeSet<String> {
        self.initial_hot
            .iter()
            .chain(self.loaded_tools.iter())
            .map(|tool| tool.name.clone())
            .filter(|name| {
                !self.runtime_tool_names.contains(name)
                    && !self.structural_tool_names.contains(name)
            })
            .collect()
    }

    pub fn deferred_names(&self) -> BTreeSet<String> {
        self.deferred
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    pub fn parity_report(&self) -> SurfacePlanParityReport {
        let mut projected = self.visible_business_names();
        projected.extend(self.deferred_names());
        let missing_authorized = self
            .authorized_business_tool_names
            .difference(&projected)
            .cloned()
            .collect::<Vec<_>>();
        let unauthorized_projected = projected
            .difference(&self.authorized_business_tool_names)
            .cloned()
            .collect::<Vec<_>>();
        let duplicate_provider_names = duplicate_names(
            self.initial_hot
                .iter()
                .chain(self.loaded_tools.iter())
                .map(|tool| tool.name.as_str()),
        );
        SurfacePlanParityReport {
            exact_business_universe: missing_authorized.is_empty()
                && unauthorized_projected.is_empty(),
            missing_authorized,
            unauthorized_projected,
            duplicate_provider_names,
        }
    }
}

fn duplicate_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut duplicates = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            duplicates.insert(name.to_string());
        }
    }
    duplicates.into_iter().collect()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SurfacePlanParityReport {
    pub exact_business_universe: bool,
    pub missing_authorized: Vec<String>,
    pub unauthorized_projected: Vec<String>,
    pub duplicate_provider_names: Vec<String>,
}

impl SurfacePlanParityReport {
    pub fn is_exact(&self) -> bool {
        self.exact_business_universe && self.duplicate_provider_names.is_empty()
    }
}

#[derive(Debug, Clone)]
struct SurfacePlanEntry {
    plan: Arc<EffectiveSurfacePlan>,
    last_access_sequence: u64,
}

#[derive(Debug, Clone)]
struct StaticPromptEntry {
    prompt: Arc<String>,
    last_access_sequence: u64,
}

#[derive(Debug, Default)]
struct SurfacePlanCacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    inserts: AtomicU64,
    evictions: AtomicU64,
    invalidations: AtomicU64,
    static_prompt_hits: AtomicU64,
    static_prompt_misses: AtomicU64,
    static_prompt_inserts: AtomicU64,
    static_prompt_evictions: AtomicU64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SurfacePlanCacheStatus {
    pub entry_count: usize,
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    pub invalidations: u64,
    pub static_prompt_entry_count: usize,
    pub static_prompt_hits: u64,
    pub static_prompt_misses: u64,
    pub static_prompt_inserts: u64,
    pub static_prompt_evictions: u64,
}

#[derive(Debug, Clone)]
pub struct SurfacePlanCache {
    entries: Arc<DashMap<SurfacePlanKey, SurfacePlanEntry>>,
    static_prompts: Arc<DashMap<StaticPromptKey, StaticPromptEntry>>,
    max_entries: usize,
    access_sequence: Arc<AtomicU64>,
    metrics: Arc<SurfacePlanCacheMetrics>,
}

impl SurfacePlanCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
            static_prompts: Arc::new(DashMap::new()),
            max_entries: max_entries.max(1),
            access_sequence: Arc::new(AtomicU64::new(0)),
            metrics: Arc::new(SurfacePlanCacheMetrics::default()),
        }
    }

    pub fn get(&self, key: &SurfacePlanKey) -> Option<Arc<EffectiveSurfacePlan>> {
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(mut entry) = self.entries.get_mut(key) {
            entry.last_access_sequence = sequence;
            self.metrics.hits.fetch_add(1, Ordering::Relaxed);
            Some(Arc::clone(&entry.plan))
        } else {
            self.metrics.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    pub fn insert(
        &self,
        key: SurfacePlanKey,
        mut plan: EffectiveSurfacePlan,
    ) -> Arc<EffectiveSurfacePlan> {
        if plan.built_at_ms == 0 {
            plan.built_at_ms = now_epoch_ms();
        }
        let plan = Arc::new(plan);
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        self.entries.insert(
            key,
            SurfacePlanEntry {
                plan: Arc::clone(&plan),
                last_access_sequence: sequence,
            },
        );
        self.metrics.inserts.fetch_add(1, Ordering::Relaxed);
        self.evict_to_limit();
        plan
    }

    pub fn get_static_prompt(&self, key: &StaticPromptKey) -> Option<Arc<String>> {
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(mut entry) = self.static_prompts.get_mut(key) {
            entry.last_access_sequence = sequence;
            self.metrics
                .static_prompt_hits
                .fetch_add(1, Ordering::Relaxed);
            Some(Arc::clone(&entry.prompt))
        } else {
            self.metrics
                .static_prompt_misses
                .fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    pub fn insert_static_prompt(&self, key: StaticPromptKey, prompt: String) -> Arc<String> {
        let prompt = Arc::new(prompt);
        let sequence = self.access_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        self.static_prompts.insert(
            key,
            StaticPromptEntry {
                prompt: Arc::clone(&prompt),
                last_access_sequence: sequence,
            },
        );
        self.metrics
            .static_prompt_inserts
            .fetch_add(1, Ordering::Relaxed);
        self.evict_static_prompts_to_limit();
        prompt
    }

    pub fn invalidate_agent(&self, principal: &str, workspace: &str, agent_id: &str) -> usize {
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
        let prompt_keys = self
            .static_prompts
            .iter()
            .filter(|entry| {
                let key = entry.key();
                key.principal == principal && key.workspace == workspace && key.agent_id == agent_id
            })
            .map(|entry| entry.key().clone())
            .collect::<Vec<_>>();
        for key in prompt_keys {
            if self.static_prompts.remove(&key).is_some() {
                removed += 1;
            }
        }
        if removed > 0 {
            self.metrics
                .invalidations
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    pub fn status(&self) -> SurfacePlanCacheStatus {
        SurfacePlanCacheStatus {
            entry_count: self.entries.len(),
            hits: self.metrics.hits.load(Ordering::Relaxed),
            misses: self.metrics.misses.load(Ordering::Relaxed),
            inserts: self.metrics.inserts.load(Ordering::Relaxed),
            evictions: self.metrics.evictions.load(Ordering::Relaxed),
            invalidations: self.metrics.invalidations.load(Ordering::Relaxed),
            static_prompt_entry_count: self.static_prompts.len(),
            static_prompt_hits: self.metrics.static_prompt_hits.load(Ordering::Relaxed),
            static_prompt_misses: self.metrics.static_prompt_misses.load(Ordering::Relaxed),
            static_prompt_inserts: self.metrics.static_prompt_inserts.load(Ordering::Relaxed),
            static_prompt_evictions: self.metrics.static_prompt_evictions.load(Ordering::Relaxed),
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

    fn evict_static_prompts_to_limit(&self) {
        while self.static_prompts.len() > self.max_entries {
            let oldest = self
                .static_prompts
                .iter()
                .min_by_key(|entry| entry.value().last_access_sequence)
                .map(|entry| entry.key().clone());
            let Some(oldest) = oldest else {
                break;
            };
            if self.static_prompts.remove(&oldest).is_some() {
                self.metrics
                    .static_prompt_evictions
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use serde_json::json;

    use super::*;
    use crate::magician_v2::execution::agentic::native_catalog::build_pack_capability_tool;
    use crate::magician_v2::execution::agentic::policy_snapshot::{
        EffectiveToolGrant, EffectiveToolKind,
    };
    use crate::magician_v2::execution::capability::CapabilityPackDefinition;
    use crate::magician_v2::execution::flat_loop::build_tool_index;

    fn index() -> ToolIndex {
        let pack: CapabilityPackDefinition = serde_yaml::from_str(
            "name: browser\ndescription: browser\nparameters: []\nnative_action_schemas:\n  open:\n    description: open\n    parameters: []\n  click:\n    description: click\n    parameters: []\nimplementation:\n  type: primitive\n  provider_name: browser\n",
        )
        .expect("pack");
        build_tool_index(&[pack])
    }

    fn tool(name: &str) -> NativeExecutionTool {
        build_pack_capability_tool(name, name, &json!({"type":"object"}))
            .expect("a fixture tool reaches nobody, so it is never withheld")
    }

    fn plan(authority_revision: &str) -> EffectiveSurfacePlan {
        EffectiveSurfacePlan {
            authority_revision: authority_revision.to_string(),
            registry_revision: "registry".to_string(),
            invocation_surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            initial_hot: vec![tool("tool_search"), tool("browser__open")],
            loaded_tools: Vec::new(),
            deferred: vec![DeferredEntry {
                name: "browser__click".to_string(),
                group: "pack",
                search_hint: None,
            }],
            runtime_tool_names: BTreeSet::from(["tool_search".to_string()]),
            structural_tool_names: BTreeSet::new(),
            authorized_business_tool_names: BTreeSet::from([
                "browser__open".to_string(),
                "browser__click".to_string(),
            ]),
            provider_schema_bytes: 1,
            built_at_ms: 0,
        }
    }

    fn policy_snapshot(browser_loaded: bool) -> EffectiveToolPolicySnapshot {
        let browser = EffectiveToolGrant {
            name: "browser__open".to_string(),
            kind: if browser_loaded {
                EffectiveToolKind::Direct
            } else {
                EffectiveToolKind::Deferred
            },
            provider_visible: browser_loaded,
        };
        let mut direct_tools = BTreeMap::new();
        let mut deferred_tools = BTreeMap::new();
        if browser_loaded {
            direct_tools.insert(browser.name.clone(), browser);
        } else {
            deferred_tools.insert(browser.name.clone(), browser);
        }
        EffectiveToolPolicySnapshot {
            snapshot_id: if browser_loaded { "loaded" } else { "deferred" }.to_string(),
            agent_id: "presto".to_string(),
            definition_version: 7,
            definition_digest: "definition-a".to_string(),
            invocation: crate::magician_v2::agents::AgentInvocationContext {
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                source_agent_id: None,
                target_agent_id: "presto".to_string(),
                surface: InvocationSurface::Task,
                feature_mode: FeatureMode::None,
                source_kind: crate::magician_v2::agents::InvocationSourceKind::Autonomous,
                chat_session_id: None,
                chat_turn_id: None,
            },
            trust_level: "local".to_string(),
            direct_tools,
            deferred_tools,
            runtime_tools: BTreeMap::new(),
            implicit_tools: BTreeSet::from(["task_state".to_string()]),
            structural_tools: BTreeMap::new(),
            delegation_targets: BTreeMap::new(),
            handover_targets: BTreeMap::new(),
            denied_tool_names: BTreeSet::new(),
            denied_tool_params: HashMap::new(),
            approval_rules: Vec::new(),
            provider_specs: if browser_loaded {
                vec![tool("browser__open")]
            } else {
                Vec::new()
            },
            dispatch_tool_names: BTreeSet::from(["browser__open".to_string()]),
            delegate_owned_tool_names: BTreeSet::new(),
            engagement_authority: None,
        }
    }

    #[test]
    fn autonomous_authority_revision_ignores_loaded_split_but_tracks_authority() {
        let deferred = policy_snapshot(false);
        let loaded = policy_snapshot(true);
        let base = autonomous_authority_revision("registry-a", &deferred).expect("revision");
        assert_eq!(
            base,
            autonomous_authority_revision("registry-a", &loaded).expect("loaded revision")
        );
        assert_ne!(
            base,
            autonomous_authority_revision("registry-b", &loaded).expect("registry revision")
        );
        let mut denied = loaded;
        denied.denied_tool_names.insert("browser__open".to_string());
        assert_ne!(
            base,
            autonomous_authority_revision("registry-a", &denied).expect("denied revision")
        );
    }

    #[test]
    fn pack_grants_expand_to_exact_leaf_universe() {
        assert_eq!(
            expand_direct_grants_to_leaves(&index(), ["browser"]),
            BTreeSet::from(["browser__click".to_string(), "browser__open".to_string()])
        );
    }

    #[test]
    fn parity_distinguishes_runtime_from_business_tools() {
        let plan = plan("authority-a");
        let report = plan.parity_report();
        assert!(report.is_exact(), "{report:?}");
    }

    #[test]
    fn parity_reports_missing_and_unauthorized_without_hiding_duplicates() {
        let mut plan = plan("authority-a");
        plan.deferred.clear();
        plan.initial_hot.push(tool("browser__open"));
        plan.initial_hot.push(tool("browser__delete"));
        let report = plan.parity_report();
        assert_eq!(report.missing_authorized, vec!["browser__click"]);
        assert_eq!(report.unauthorized_projected, vec!["browser__delete"]);
        assert_eq!(report.duplicate_provider_names, vec!["browser__open"]);
    }

    #[test]
    fn cache_reuses_exact_revision_and_evicts_oldest() {
        let cache = SurfacePlanCache::new(2);
        for revision in ["one", "two", "three"] {
            cache.insert(
                SurfacePlanKey {
                    principal: "owner".to_string(),
                    workspace: "default".to_string(),
                    agent_id: "presto".to_string(),
                    surface: InvocationSurface::Chat,
                    feature_mode: FeatureMode::None,
                    authority_revision: revision.to_string(),
                },
                plan(revision),
            );
        }
        assert_eq!(cache.status().entry_count, 2);
        assert_eq!(cache.status().evictions, 1);
        let key = SurfacePlanKey {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            agent_id: "presto".to_string(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            authority_revision: "three".to_string(),
        };
        assert!(cache.get(&key).is_some());
        assert!(cache.invalidate_agent("owner", "default", "presto") > 0);
        assert_eq!(cache.status().entry_count, 0);
    }

    #[test]
    fn static_prompt_revision_is_order_independent_and_content_sensitive() {
        let first = HashMap::from([
            ("persona".to_string(), "Presto".to_string()),
            ("tools".to_string(), "search_memory".to_string()),
        ]);
        let second = HashMap::from([
            ("tools".to_string(), "search_memory".to_string()),
            ("persona".to_string(), "Presto".to_string()),
        ]);
        let revision = static_prompt_context_revision("chat", "v1", &first);
        assert_eq!(
            revision,
            static_prompt_context_revision("chat", "v1", &second)
        );
        assert_ne!(
            revision,
            static_prompt_context_revision(
                "chat",
                "v1",
                &HashMap::from([
                    ("persona".to_string(), "Presto".to_string()),
                    ("tools".to_string(), "search_memory, browser".to_string()),
                ])
            )
        );
    }

    #[test]
    fn static_prompt_cache_is_surface_scoped_bounded_and_refreshable() {
        let cache = SurfacePlanCache::new(2);
        let key = |surface, revision: &str| StaticPromptKey {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            agent_id: "presto".to_string(),
            surface,
            feature_mode: FeatureMode::None,
            prompt_name: "outer-loop".to_string(),
            context_revision: revision.to_string(),
        };
        let chat = key(InvocationSurface::Chat, "one");
        cache.insert_static_prompt(chat.clone(), "stable chat prefix".to_string());
        assert_eq!(
            cache
                .get_static_prompt(&chat)
                .as_deref()
                .map(String::as_str),
            Some("stable chat prefix")
        );
        assert!(cache
            .get_static_prompt(&key(InvocationSurface::RealtimeVoice, "one"))
            .is_none());
        cache.insert_static_prompt(
            key(InvocationSurface::RealtimeVoice, "one"),
            "stable voice prefix".to_string(),
        );
        cache.insert_static_prompt(
            key(InvocationSurface::Task, "one"),
            "stable task prefix".to_string(),
        );
        let status = cache.status();
        assert_eq!(status.static_prompt_entry_count, 2);
        assert_eq!(status.static_prompt_evictions, 1);
        assert_eq!(status.static_prompt_hits, 1);
        assert_eq!(status.static_prompt_misses, 1);
        assert_eq!(cache.invalidate_agent("owner", "default", "presto"), 2);
        assert_eq!(cache.status().static_prompt_entry_count, 0);
    }
}
