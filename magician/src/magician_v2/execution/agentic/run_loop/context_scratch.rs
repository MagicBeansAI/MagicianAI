//! The interior-mutable scratch on `AgenticContext`.
//!
//! Eight fields, and the last group of the scratch extraction. See
//! `docs/archive/plans/2026-08-25-stateless-loop-design.md`; the field audit that placed
//! them is `docs/archive/plans/2026-08-26-scratch-extraction-field-audit.md`.
//!
//! # Why every `Arc` stays, and why that is more load-bearing here than anywhere
//!
//! The `ActionExecutors` groups keep their `Arc`s because `PrimitiveExecCtx` is
//! handed clones and writes through them. These keep theirs for a different and
//! sharper reason: **`AgenticContext` is cloned per owner transition and per
//! inline delegation, and several of these are supposed to be shared by those
//! clones.** Their own declarations said so before this move —
//! *"shared across cloned contexts so owner transitions and inline delegation can
//! feed one run-level memory utility ledger"*, *"the map is shared by cloned owner
//! contexts"*.
//!
//! So the interior mutability is not an implementation detail that a refactor may
//! flatten. A group that owned its data outright would give every owner
//! transition a private copy, and the run-level ledgers those two comments
//! describe would silently become per-owner ones — each delegate accumulating its
//! own memory-candidate list that the post-run reviewer never sees whole.
//!
//! Two more explain their `Arc` as an ergonomic choice rather than a sharing one:
//! `focused_tool` is updated mid-loop before the DECIDE call, and
//! `active_procedure_skill` is refreshed through a shared `&AgenticContext`
//! without forcing `&mut ctx` on every path that builds a prompt. Both still need
//! the `Arc`; only the reason differs.
//!
//! # What does not cross a boundary
//!
//! `continuation_section_fingerprints` is explicitly runtime-only: a cold resume
//! re-sends one bounded state snapshot and establishes a fresh checkpoint, which
//! is correct and cheap. Carrying stale fingerprints would be worse than carrying
//! none — the provider would skip sections it believes unchanged, against a
//! checkpoint the far side never established.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::ownership_runtime::OwnerExecutionProfile;
use super::super::types::AutonomousSurfaceRuntimeBinding;

/// How long a loaded owner execution profile stands in for a fresh load.
///
/// The profile is reloaded before every decide and again in every resolve so
/// a mid-run policy change is noticed; the load (definition record, merged
/// tools against the capability catalog, delegation targets, trust policies)
/// cost ~1.0 s each time — ~2 s of every iteration, 22% of a run, paid by
/// loop-issued steps whose own decide body is 0 ms. Three minutes: a
/// mid-run policy or tool change reaches the loop within that window
/// (run 12 with a 30 s TTL still spent 7 s reloading — one expiry every
/// ~8 iterations — for a change that never happens mid-run in practice).
pub const OWNER_PROFILE_TTL: Duration = Duration::from_secs(180);

/// The scope a cached owner profile was loaded for, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerProfileCacheKey {
    pub agent_id: String,
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub loaded_at: Instant,
}

impl OwnerProfileCacheKey {
    /// Whether a profile loaded under this key still stands in for a fresh
    /// load for `agent_id`/`principal`/`workspace` at `now`: the exact same
    /// scope (a different owner never hits the cache) and within `ttl`.
    pub fn is_fresh_for(
        &self,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        now: Instant,
        ttl: Duration,
    ) -> bool {
        self.agent_id == agent_id
            && self.principal.as_deref() == principal
            && self.workspace.as_deref() == workspace
            && now.saturating_duration_since(self.loaded_at) < ttl
    }
}

/// A loaded owner execution profile and the scope it was loaded for.
#[derive(Debug, Clone)]
pub struct OwnerProfileCacheEntry {
    pub key: OwnerProfileCacheKey,
    pub profile: OwnerExecutionProfile,
}

impl OwnerProfileCacheEntry {
    /// The cached profile, when it is for this scope and still within the TTL.
    pub fn fresh_for(
        &self,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        now: Instant,
        ttl: Duration,
    ) -> Option<&OwnerExecutionProfile> {
        self.key
            .is_fresh_for(agent_id, principal, workspace, now, ttl)
            .then_some(&self.profile)
    }
}

/// Per-execution scratch that lives on the context rather than the executors.
///
/// Cloning this clones the `Arc`s, so cloned contexts keep sharing — which is the
/// documented behaviour of at least three of these fields.
#[derive(Debug, Clone, Default)]
pub struct ContextScratch {
    /// Focused tool for the current runtime decision: `(capability_name, yaml_spec)`.
    ///
    /// When set, the LLM gets the full guide and params for this tool only; other
    /// tools stay listed by name and description. Updated mid-loop before the
    /// DECIDE call, which the prompt builder then reads.
    pub focused_tool: Arc<Mutex<Option<(String, String)>>>,

    /// The agent's current loaded working set — deferred tools pulled in via
    /// `tool_search` this execution.
    ///
    /// A `select:` load is **whole-pack** and **replaces** the set, so loading a
    /// different pack unloads the previous one; cleared on owner transition. The
    /// decision transport is native function-calling, so a tool is callable only
    /// once it is in the request's `tools:` array — `tool_search` returning a
    /// schema does not by itself make one callable, which is why this record
    /// exists at all.
    pub loaded_tools: Arc<Mutex<HashSet<String>>>,

    /// Per-iteration render cache for the currently-active procedure skill.
    ///
    /// **Not a source of truth** — writes here are clobbered on the next
    /// iteration. The compiled `activate_skill` / `deactivate_skill` handlers
    /// write the agent-scope memory tier; this tracks that tier on a
    /// one-iteration delay so the decision-prompt builder can read it without an
    /// async tier read inside synchronous prompt assembly.
    pub active_procedure_skill:
        Arc<Mutex<Option<crate::magician_v2::skills::ActiveProcedureSkill>>>,

    /// Current autonomous owner-frame binding, replaced only after a current
    /// policy snapshot has produced the stable authority revision.
    pub autonomous_surface_binding: Arc<Mutex<Option<AutonomousSurfaceRuntimeBinding>>>,

    /// Memory candidate keys injected into this execution's prompt context.
    ///
    /// **Shared across cloned contexts on purpose**, so owner transitions and
    /// inline delegation feed one run-level memory utility ledger rather than one
    /// per owner.
    pub injected_memory_candidate_keys: Arc<Mutex<BTreeSet<String>>>,

    /// The candidates themselves, kept beside the keys.
    ///
    /// The text sidecar is what the post-run utility reviewer reads; keeping it
    /// separate from the prompt strings means review logic never has to parse a
    /// rendered prompt.
    pub injected_memory_candidates:
        Arc<Mutex<BTreeMap<String, crate::magician_v2::agents::MemoryPromptSelectedCandidate>>>,

    /// Environment knowledge accumulated during execution via lazy domain-change
    /// lookups. Append-only; read by the decision prompt builders to augment the
    /// pre-rendered `prior_environment_knowledge`.
    pub supplemental_environment_knowledge: Arc<Mutex<String>>,

    /// Last dynamic continuation sections accepted by the decision provider.
    ///
    /// The bootstrap prompt carries these once; later provider-owned continuation
    /// calls compare fingerprints and send only what changed. Shared by cloned
    /// owner contexts, and **runtime-only** — see the module docs for why a cold
    /// resume must start from none rather than from stale ones.
    pub continuation_section_fingerprints: Arc<Mutex<BTreeMap<String, String>>>,
    /// The loaded working set (`loaded_tools`) the last accepted decision call
    /// was built with. When it differs, the next call is a full send rather
    /// than a continuation delta: the new tool definitions invalidate every
    /// provider's cached prefix either way, and a full send warms the new
    /// prefix, where a delta left it cold until the chain's next rebuild,
    /// which then read 0% cached. Runtime-only, like the fingerprints above.
    pub decision_loaded_tools: Arc<Mutex<Option<BTreeSet<String>>>>,
    /// The changing part of the prompt a decision just sent on a provider with
    /// no server-side chain, and the index of the `live_messages` entry it
    /// followed. `Resolve` appends it there once the decision is accepted, so
    /// the conversation holds what the model was sent and the next turn can
    /// send only what changed (`decision::local_continuation_base_present`).
    /// Runtime-only with the fingerprints it is diffed against: a cold resume
    /// starts from none of either.
    pub pending_local_turn: Arc<Mutex<Option<(usize, String)>>>,
    /// The owner execution profile last loaded for this execution, reused
    /// within [`OWNER_PROFILE_TTL`] by `refresh_owner_profile_for_decision`.
    /// Shared by cloned owner contexts (one run, one owner at a time) and
    /// runtime-only: a cold resume loads afresh. Cleared on owner transition
    /// through the scope key mismatch — a different owner never hits it.
    pub owner_profile_cache: Arc<Mutex<Option<OwnerProfileCacheEntry>>>,
}

impl ContextScratch {
    // `loaded_tool_names` lived here and had no production caller. The live read
    // is `native_integration::snapshot_loaded_tools`, which maps a poisoned lock
    // to an EMPTY set — the opposite of what this method did. Keeping a
    // recovering reader nothing called, beside a test asserting the recovery,
    // documented a guarantee the runtime does not give. The read policy is left
    // as it is: reading "no tools loaded" after a panic narrows the run, and
    // narrowing is the safe direction.
    //
    // The WRITES are the other direction, and they are fixed below.

    /// Clear the working set, as an owner transition does.
    ///
    /// A new owner has its own catalog; inheriting the previous owner's loaded
    /// packs would make tools callable that the incoming agent was never granted.
    ///
    /// **Recovers from a poisoned lock**, and that is the whole point. The three
    /// write sites used `if let Ok(..)`, so a poisoning SKIPPED the clear and the
    /// incoming owner kept its predecessor's packs — a widening of exactly the
    /// shape the browser-transport ceiling recovers to prevent. Losing a clear
    /// grants tools nobody authorised; losing a read merely hides tools that were.
    pub fn clear_loaded_tools(&self) {
        self.loaded_tools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// Replace the working set wholesale — the unload-on-switch rule.
    ///
    /// A `select:` load is whole-pack and REPLACES: loading a different pack
    /// unloads the previous one. Recovers from poisoning for the same reason as
    /// [`Self::clear_loaded_tools`] — a skipped replace leaves the previous
    /// pack's tools callable alongside the new one's, which is strictly wider
    /// than either owner was granted.
    pub fn replace_loaded_tools(&self, tools: impl IntoIterator<Item = String>) {
        let mut guard = self
            .loaded_tools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = tools.into_iter().collect();
    }

    /// Drop the focused tool, as the start of an iteration does.
    pub fn clear_focused_tool(&self) {
        *self
            .focused_tool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cloned_context_shares_the_run_level_ledger() {
        // REGRESSION GUARD, and the reason this group keeps its `Arc`s. Owner
        // transitions and inline delegation clone the context; the memory ledger
        // is documented as shared across those clones so the post-run reviewer
        // sees one list. A group that owned its data would give each delegate a
        // private ledger and the reviewer would see only the last one.
        let parent = ContextScratch::default();
        let delegate = parent.clone();

        delegate
            .injected_memory_candidate_keys
            .lock()
            .unwrap()
            .insert("candidate-1".to_string());

        assert!(
            parent
                .injected_memory_candidate_keys
                .lock()
                .unwrap()
                .contains("candidate-1"),
            "a delegate's injection must reach the run-level ledger, not a copy"
        );
    }

    #[test]
    fn clearing_the_working_set_is_visible_to_every_holder() {
        // Owner transition clears the loaded tools. If a clone kept its own set,
        // the incoming owner would still be able to call the previous owner's
        // loaded packs — tools it was never granted.
        let ctx = ContextScratch::default();
        ctx.loaded_tools
            .lock()
            .unwrap()
            .insert("browser__open".to_string());
        let held_elsewhere = ctx.clone();

        ctx.clear_loaded_tools();

        assert!(
            held_elsewhere
                .loaded_tools
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty(),
            "an owner transition must not leave a stale working set reachable"
        );
    }

    #[test]
    fn a_focused_tool_survives_until_it_is_cleared() {
        let ctx = ContextScratch::default();
        *ctx.focused_tool.lock().unwrap() = Some(("browser".to_string(), "spec".to_string()));
        assert!(ctx.focused_tool.lock().unwrap().is_some());

        ctx.clear_focused_tool();
        assert!(ctx.focused_tool.lock().unwrap().is_none());
    }

    #[test]
    fn a_poisoned_lock_still_unloads_the_previous_owner() {
        // REGRESSION GUARD, and a correction. This test used to assert that a
        // poisoned lock must not hide loaded tools — while the only live READER
        // (`native_integration::snapshot_loaded_tools`) maps a poisoned lock to
        // an empty set, doing precisely what the test forbade. It asserted a
        // guarantee the runtime did not give.
        //
        // The direction that actually matters is the opposite one. All three
        // WRITE sites used `if let Ok(..)`, so a poisoning skipped the clear and
        // an incoming owner kept its predecessor's loaded packs — tools it was
        // never granted. Losing a clear WIDENS; losing a read narrows.
        let ctx = ContextScratch::default();
        ctx.loaded_tools
            .lock()
            .unwrap()
            .insert("browser__open".to_string());

        let poisoner = Arc::clone(&ctx.loaded_tools);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the working set");
        })
        .join();

        ctx.clear_loaded_tools();
        assert!(
            ctx.loaded_tools
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty(),
            "an owner transition must unload the previous owner's packs even \
             through a poisoned lock; skipping leaves tools nobody granted"
        );

        ctx.replace_loaded_tools(["shell__run".to_string()]);
        let after: Vec<String> = ctx
            .loaded_tools
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect();
        assert_eq!(
            after,
            vec!["shell__run".to_string()],
            "unload-on-switch must replace, not accumulate, even under poison"
        );
    }

    #[test]
    fn an_owner_profile_is_reused_only_for_the_same_scope_within_the_ttl() {
        let loaded_at = Instant::now();
        let key = OwnerProfileCacheKey {
            agent_id: "personal-assistant".to_string(),
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            loaded_at,
        };
        let ttl = OWNER_PROFILE_TTL;
        let within = loaded_at + ttl / 2;
        assert!(key.is_fresh_for(
            "personal-assistant",
            Some("anonymous"),
            Some("default"),
            within,
            ttl
        ));
        // A different owner, principal, or workspace never hits the cache.
        assert!(!key.is_fresh_for(
            "web-researcher",
            Some("anonymous"),
            Some("default"),
            within,
            ttl
        ));
        assert!(!key.is_fresh_for(
            "personal-assistant",
            Some("someone"),
            Some("default"),
            within,
            ttl
        ));
        assert!(!key.is_fresh_for("personal-assistant", Some("anonymous"), None, within, ttl));
        // And the TTL is a hard edge.
        assert!(!key.is_fresh_for(
            "personal-assistant",
            Some("anonymous"),
            Some("default"),
            loaded_at + ttl,
            ttl
        ));
    }
}
