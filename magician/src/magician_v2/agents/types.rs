//! Core type definitions for TRUE_AGENTS Phase 2.
//!
//! Phase 2 is data-layer only: declarative contracts + parsing + validation.
//! Runtime interpretation is intentionally deferred to Phase 3+.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::Path,
};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::memory_defaults::default_memory_config_for_personal_agent;
use super::memory_tiers::{
    ConsolidationTransform, ConsolidationTrigger, MemoryConsolidationRule, MemoryTierDefinition,
    RetentionMode, SourceRef, TierScope,
};
use super::storage::{slugify_name, validate_agent_identifier, AgentStorageError};
use crate::magician_v2::apps::manifest::{AppManifestField, AppManifestInputSchema};

/// Storage key prefix for agent-scoped entries. Prevents collision with execution keys
/// (`{tid}:{pid}:{sid}`) in `FullPauseStore` and `routing_key()`.
pub const AGENT_KEY_PREFIX: &str = "agent:";
pub const SYSTEM_AGENT_ID_PREFIX: &str = "system:";

pub fn tool_name_matches_block_entry(tool_name: &str, entry: &str) -> bool {
    let tool_name = tool_name.trim();
    let entry = entry.trim();
    tool_name == entry || (tool_name == "time_math" && entry == "core_utility")
}

pub fn is_system_agent_id(agent_id: &str) -> bool {
    agent_id.trim().starts_with(SYSTEM_AGENT_ID_PREFIX)
}

/// Unique identifier for an agent instance.
pub type AgentId = String;

/// Unique identifier for a goal assigned to an agent.
pub type GoalId = String;

/// Unique identifier for a single observe-decide-execute cycle within a goal.
pub type CycleId = String;

/// Classifies agents by their autonomy level and interaction model.
///
/// - `Personal` (default): user-facing, conversational agents.
/// - `Worker`: headless agents invoked by delegation or workflow steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    #[default]
    Personal,
    Worker,
}

/// Product/runtime boundary through which an agent is being invoked.
///
/// This is authorization data, not free-form provenance. Entry points must
/// derive it from the authenticated route or session they own rather than
/// trusting an arbitrary client-supplied agent id.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum InvocationSurface {
    #[default]
    Chat,
    RealtimeVoice,
    Task,
    Delegation,
    Handover,
    ThinkingMap,
    Tutor,
    AppCopilot,
    ContextualAssist,
    PublicEnvoy,
    /// A shared room the owner does not control the membership of.
    ///
    /// Deliberately **not** a narrowing of `RealtimeVoice`: a meeting is a
    /// distinct audience that happens to arrive over the voice transport, and
    /// collapsing the two is what let the meeting responder register as a tray
    /// session and answer a room as the full personal assistant.
    ///
    /// It is also deliberately absent from [`Self::is_default_direct_surface`],
    /// which is the whole fail-closed property: an agent with an empty
    /// `allowed_direct_surfaces` is reachable from the default surfaces, so
    /// leaving `Meeting` out makes every existing agent unreachable from a room
    /// without editing a single agent definition.
    Meeting,
    /// A foreign harness or MCP client bound through the Magician plane.
    ///
    /// A plane grant is minted from the owner's authenticated session, so
    /// speaker identity is established before any `tools/call` — that is the
    /// definition of [`SurfaceAudience::Owner`]. [`SurfaceAudience::Untrusted`]
    /// is for rooms and public channels where identity is not a trustworthy
    /// authorization signal. The far-side harness is not Magician-chosen and
    /// its native tools are ungoverned; that is handled by leaving `Plane` out
    /// of [`Self::is_default_direct_surface`], by the plane grant's denied-family
    /// floor, and by routing every call through `execute_action`. Collapsing
    /// those into `Untrusted` would also refuse delegation
    /// (`surface_may_delegate`) and cap memory at the room posture for a
    /// credential the owner minted. [`Self::ThinkingMap`] is the precedent: an
    /// owner surface that still requires explicit opt-in.
    ///
    /// Deliberately absent from [`Self::is_default_direct_surface`]: an agent
    /// with an empty `allowed_direct_surfaces` is reachable from the default
    /// surfaces, so leaving `Plane` out makes every existing agent unreachable
    /// from a terminal until someone opts it in.
    Plane,
}

/// Who is on the other end of a surface — the one property authority actually
/// turns on.
///
/// Named because it was previously *implicit*: whether an agent could face
/// strangers was inferable only from which channels its
/// `allowed_direct_surfaces` happened to list, so a reviewer reading
/// `[meeting]` had to already know what a meeting is. Channels multiply
/// (a room, a public inbox, a group chat, an SMS thread); this property does
/// not, and it is what every authority rule should key on so a new channel
/// does not mean re-deciding posture per agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceAudience {
    /// The authenticated owner, on a first-party surface. Speaker identity is
    /// established before the turn reaches an agent.
    Owner,
    /// Anyone may be present or speaking, and identity is **not** a trustworthy
    /// authorization signal. A room, a public channel, a group thread.
    Untrusted,
}

impl SurfaceAudience {
    /// Stable label for diagnostics. Nothing branches on this string — the
    /// enum is the authority — so it can be shown to an operator or a client
    /// without becoming a second authorization axis.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Untrusted => "untrusted",
        }
    }
}

impl InvocationSurface {
    /// Every surface, so invariants can be asserted over the set rather than
    /// restating a list that then drifts from the one it mirrors.
    pub const ALL: [Self; 12] = [
        Self::Chat,
        Self::RealtimeVoice,
        Self::Task,
        Self::Delegation,
        Self::Handover,
        Self::ThinkingMap,
        Self::Tutor,
        Self::AppCopilot,
        Self::ContextualAssist,
        Self::PublicEnvoy,
        Self::Meeting,
        Self::Plane,
    ];

    /// Deliberately an **exhaustive** match, not a `matches!` against a list.
    /// Adding a surface must not silently inherit an audience: it breaks the
    /// build until someone states who is on the other end, which is the one
    /// decision that must never be made by omission.
    pub fn audience(self) -> SurfaceAudience {
        match self {
            Self::PublicEnvoy | Self::Meeting => SurfaceAudience::Untrusted,
            Self::Chat
            | Self::RealtimeVoice
            | Self::Task
            | Self::Delegation
            | Self::Handover
            | Self::ThinkingMap
            | Self::Tutor
            | Self::AppCopilot
            | Self::ContextualAssist
            | Self::Plane => SurfaceAudience::Owner,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::RealtimeVoice => "realtime_voice",
            Self::Task => "task",
            Self::Delegation => "delegation",
            Self::Handover => "handover",
            Self::ThinkingMap => "thinking_map",
            Self::Tutor => "tutor",
            Self::AppCopilot => "app_copilot",
            Self::ContextualAssist => "contextual_assist",
            Self::PublicEnvoy => "public_envoy",
            Self::Meeting => "meeting",
            Self::Plane => "plane",
        }
    }

    /// Which surfaces an agent with an empty `allowed_direct_surfaces` serves.
    ///
    /// **Deliberately not written as `self.audience() == Owner`,** tempting as
    /// that is. This list is narrower than the audience property: `ThinkingMap`
    /// and `Plane` are owner surfaces that still require an explicit opt-in, so
    /// deriving this from audience would silently widen every ordinary agent
    /// onto them. The two are related by an invariant instead — no untrusted
    /// surface may ever appear here — which is asserted over `ALL` rather than
    /// restated, so a new untrusted surface is covered without anyone
    /// remembering. Owner-but-opt-in surfaces still have to be omitted here by
    /// hand; `ThinkingMap` and `Plane` are the two.
    pub fn is_default_direct_surface(self) -> bool {
        matches!(
            self,
            Self::Chat
                | Self::RealtimeVoice
                | Self::Task
                | Self::Delegation
                | Self::Handover
                | Self::Tutor
                | Self::AppCopilot
                | Self::ContextualAssist
        )
    }
}

/// Feature lane selected by a trusted product route.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum FeatureMode {
    #[default]
    None,
    Tutor,
    AppCopilot,
    Brainstorm,
    Vibedev,
}

impl FeatureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Tutor => "tutor",
            Self::AppCopilot => "app_copilot",
            Self::Brainstorm => "brainstorm",
            Self::Vibedev => "vibedev",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum InvocationSourceKind {
    #[default]
    Direct,
    ChatInline,
    Autonomous,
    Delegated,
    Handover,
    ProductFeature,
    Public,
}

impl InvocationSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::ChatInline => "chat_inline",
            Self::Autonomous => "autonomous",
            Self::Delegated => "delegated",
            Self::Handover => "handover",
            Self::ProductFeature => "product_feature",
            Self::Public => "public",
        }
    }
}

/// Immutable identity and surface context used to resolve one effective tool
/// policy snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AgentInvocationContext {
    pub principal: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    pub target_agent_id: String,
    pub surface: InvocationSurface,
    #[serde(default)]
    pub feature_mode: FeatureMode,
    #[serde(default)]
    pub source_kind: InvocationSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
}

/// Whether an agent may be found outside an exact authorized route.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentDiscoverability {
    #[default]
    Ambient,
    Explicit,
    SurfaceOnly,
}

/// How the agent may appear as a delegation target.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelegationPolicy {
    #[default]
    Wildcard,
    Explicit,
    None,
}

/// Backward-compatible invocation boundary. Existing definitions retain the
/// ambient/wildcard behavior until they deliberately opt into a narrower
/// policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentInvocationPolicy {
    #[serde(default)]
    pub discoverability: AgentDiscoverability,
    #[serde(default)]
    pub delegation: AgentDelegationPolicy,
    /// Empty = the permissive default (every `is_default_direct_surface`).
    /// Non-empty = an EXACT allowlist: `is_default_direct_surface` is bypassed
    /// entirely, so any surface omitted here is denied (`permits_direct_surface`
    /// below).
    ///
    /// Setting this on the PRIMARY agent silently breaks product surfaces that
    /// reach it on more than one route. The Magican keyboard is the sharpest case —
    /// its three lanes each arrive on a different surface:
    ///
    /// - Write (rewrite/reply/continue) → `ContextualAssist`
    /// - Ask (streamed answer)          → `Chat`
    /// - Act (confirmed task run)       → `Task`
    ///
    /// They fail INDEPENDENTLY, so omitting one looks like "that one keyboard
    /// button is broken" rather than a policy change. A primary agent that
    /// narrows this must list `chat`, `task`, and `contextual_assist` at minimum.
    /// See `docs/components/magios/magican-keyboard.md`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_direct_surfaces: Vec<InvocationSurface>,
}

impl AgentInvocationPolicy {
    pub fn is_ambient(&self) -> bool {
        self.discoverability == AgentDiscoverability::Ambient
    }

    /// Exact lookup is available for ambient and explicitly-addressable
    /// agents. Surface-only agents remain invisible unless the caller already
    /// holds the typed product route that authorizes the surface.
    pub fn is_discoverable_by_exact_lookup(&self) -> bool {
        self.discoverability != AgentDiscoverability::SurfaceOnly
    }

    pub fn permits_wildcard_delegation(&self) -> bool {
        self.delegation == AgentDelegationPolicy::Wildcard
    }

    pub fn permits_explicit_delegation(&self) -> bool {
        self.delegation != AgentDelegationPolicy::None
    }

    pub fn permits_direct_surface(&self, surface: InvocationSurface) -> bool {
        if self.allowed_direct_surfaces.is_empty() {
            return self.discoverability != AgentDiscoverability::SurfaceOnly
                && surface.is_default_direct_surface();
        }
        self.allowed_direct_surfaces.contains(&surface)
    }
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Personal => write!(f, "personal"),
            Self::Worker => write!(f, "worker"),
        }
    }
}

/// Priority level for autonomous focus areas.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum FocusAreaPriority {
    Low,
    #[serde(alias = "med")]
    #[default]
    Medium,
    High,
}

/// A named area of focus for autonomous agent cycles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FocusArea {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub priority: FocusAreaPriority,
    /// Optional per-area cron schedule override. When None, uses top-level
    /// `AutonomousConfig.schedule`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Optional alternate program document path for this focus area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Optional harness scope override for this focus area.
    /// `["*"]` means the resolved harness scope, not the entire workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<String>>,
}

impl FocusArea {
    /// Returns the effective schedule for this focus area: per-area override if
    /// present, otherwise the top-level schedule.
    pub fn effective_schedule<'a>(&'a self, top_level: &'a str) -> &'a str {
        self.schedule.as_deref().unwrap_or(top_level)
    }

    pub fn goal_slug(&self) -> String {
        slugify_name(&self.name)
    }
}

/// Additional capability configuration for a harness-enabled personal agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HarnessConfig {
    /// Default program.md section injected for focus areas that do not override
    /// the program source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_section: Option<String>,
}

/// Configuration for autonomous execution cycles on a Personal agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutonomousConfig {
    /// Cron expression for autonomous cycle schedule (e.g. "0 */4 * * *").
    pub schedule: String,
    /// Areas of focus the autonomous cycle should address.
    pub focus_areas: Vec<FocusArea>,
    /// Maximum tasks the agent can create per autonomous cycle.
    #[serde(default = "default_max_tasks_per_cycle_autonomous")]
    pub max_tasks_per_cycle: u32,
    /// Maximum steps allowed per plan in an autonomous cycle.
    #[serde(default = "default_max_steps_per_plan")]
    pub max_steps_per_plan: u32,
}

fn default_max_tasks_per_cycle_autonomous() -> u32 {
    3
}

fn default_max_steps_per_plan() -> u32 {
    10
}

/// Memory isolation mode for personal agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserMemoryIsolation {
    /// Agent shares user's memory tiers (default for backward compat).
    #[default]
    Shared,
    /// Agent has fully isolated memory — cannot read user's personal tiers.
    FullyIsolated,
}

fn default_delegation_targets() -> Vec<String> {
    Vec::new()
}

pub fn disabled_agent_hierarchy<'a, I>(definitions: I) -> HashSet<String>
where
    I: IntoIterator<Item = &'a AgentDefinition>,
{
    let definitions = definitions.into_iter().collect::<Vec<_>>();
    let all_agent_ids = definitions
        .iter()
        .map(|definition| definition.agent_id.clone())
        .collect::<HashSet<_>>();
    let delegation_targets_by_agent = definitions
        .iter()
        .map(|definition| {
            (
                definition.agent_id.clone(),
                definition.delegation_targets.clone(),
            )
        })
        .collect::<HashMap<_, _>>();

    let explicitly_disabled_agent_ids = definitions
        .iter()
        .filter(|definition| definition.disabled)
        .map(|definition| definition.agent_id.clone())
        .collect::<HashSet<_>>();

    let explicitly_referenced_agent_ids = definitions
        .iter()
        .flat_map(|definition| definition.delegation_targets.iter())
        .filter(|target| target.as_str() != "*")
        .filter(|target| all_agent_ids.contains(*target) && !is_system_agent_id(target))
        .cloned()
        .collect::<HashSet<_>>();

    let enabled_root_agent_ids = definitions
        .iter()
        .filter(|definition| {
            !definition.disabled
                && !is_system_agent_id(&definition.agent_id)
                && (definition.kind == AgentKind::Personal
                    || !explicitly_referenced_agent_ids.contains(&definition.agent_id))
        })
        .map(|definition| definition.agent_id.clone())
        .collect::<Vec<_>>();

    let enabled_reachable_agent_ids = reachable_agent_ids_from_roots(
        enabled_root_agent_ids,
        &all_agent_ids,
        &delegation_targets_by_agent,
        Some(&explicitly_disabled_agent_ids),
    );
    let disabled_reachable_agent_ids = reachable_agent_ids_from_roots(
        explicitly_disabled_agent_ids.iter().cloned().collect(),
        &all_agent_ids,
        &delegation_targets_by_agent,
        None,
    );

    explicitly_disabled_agent_ids
        .union(&disabled_reachable_agent_ids)
        .filter(|agent_id| {
            explicitly_disabled_agent_ids.contains(*agent_id)
                || !enabled_reachable_agent_ids.contains(*agent_id)
        })
        .cloned()
        .collect()
}

/// Resolve the source definition's delegation declaration into the exact
/// authorized target set. This is the shared pure boundary used by chat and
/// autonomous execution; provider schemas are projections of this result and
/// never authorization by themselves.
pub fn resolve_effective_delegation_target_ids<'a, I>(
    source: &AgentDefinition,
    definitions: I,
    disabled_agent_ids: &HashSet<String>,
) -> Vec<String>
where
    I: IntoIterator<Item = &'a AgentDefinition>,
{
    resolve_effective_delegation_target_ids_for_surface(
        source,
        definitions,
        disabled_agent_ids,
        InvocationSurface::Delegation,
    )
}

/// Resolve the exact target set for one structural owner transition.
///
/// `delegation_targets` is the source-side relationship declaration; the
/// target's typed invocation policy is an independent admission boundary. A
/// target that accepts handovers but not delegated children must therefore be
/// present only in the handover schema and vice versa.
pub fn resolve_effective_delegation_target_ids_for_surface<'a, I>(
    source: &AgentDefinition,
    definitions: I,
    disabled_agent_ids: &HashSet<String>,
    target_surface: InvocationSurface,
) -> Vec<String>
where
    I: IntoIterator<Item = &'a AgentDefinition>,
{
    if !matches!(
        target_surface,
        InvocationSurface::Delegation | InvocationSurface::Handover
    ) {
        return Vec::new();
    }
    if source.disabled
        || source.is_system_agent()
        || disabled_agent_ids.contains(&source.agent_id)
        || !source.invocation_policy.permits_explicit_delegation()
        || source.delegation_targets.is_empty()
    {
        return Vec::new();
    }

    let definitions = definitions
        .into_iter()
        .map(|definition| (definition.agent_id.as_str(), definition))
        .collect::<HashMap<_, _>>();
    let mut resolved = Vec::new();

    for requested in source
        .delegation_targets
        .iter()
        .filter(|target| target.as_str() != "*")
    {
        let Some(target) = definitions.get(requested.as_str()).copied() else {
            continue;
        };
        if target.agent_id == source.agent_id
            || target.disabled
            || target.is_system_agent()
            || disabled_agent_ids.contains(&target.agent_id)
            || !target.invocation_policy.permits_explicit_delegation()
            || !target
                .invocation_policy
                .permits_direct_surface(target_surface)
        {
            continue;
        }
        if !resolved.contains(&target.agent_id) {
            resolved.push(target.agent_id.clone());
        }
    }

    if source.invocation_policy.permits_wildcard_delegation()
        && source.delegation_targets.iter().any(|target| target == "*")
    {
        let mut wildcard = definitions
            .values()
            .copied()
            .filter(|target| {
                target.agent_id != source.agent_id
                    && !target.disabled
                    && !target.is_system_agent()
                    && !disabled_agent_ids.contains(&target.agent_id)
                    && target.invocation_policy.permits_wildcard_delegation()
                    && target
                        .invocation_policy
                        .permits_direct_surface(target_surface)
                    && !resolved.contains(&target.agent_id)
            })
            .map(|target| target.agent_id.clone())
            .collect::<Vec<_>>();
        wildcard.sort();
        resolved.extend(wildcard);
    }

    resolved
}

fn reachable_agent_ids_from_roots(
    roots: Vec<String>,
    all_agent_ids: &HashSet<String>,
    delegation_targets_by_agent: &HashMap<String, Vec<String>>,
    blocked_agent_ids: Option<&HashSet<String>>,
) -> HashSet<String> {
    let mut reachable_agent_ids = HashSet::new();
    let mut queue = VecDeque::new();

    for agent_id in roots {
        if is_system_agent_id(&agent_id)
            || blocked_agent_ids
                .map(|blocked| blocked.contains(&agent_id))
                .unwrap_or(false)
        {
            continue;
        }
        if reachable_agent_ids.insert(agent_id.clone()) {
            queue.push_back(agent_id);
        }
    }

    while let Some(agent_id) = queue.pop_front() {
        let Some(delegation_targets) = delegation_targets_by_agent.get(&agent_id) else {
            continue;
        };
        for target in delegation_targets {
            if target == "*" {
                for candidate in all_agent_ids {
                    if candidate == &agent_id || is_system_agent_id(candidate) {
                        continue;
                    }
                    if blocked_agent_ids
                        .map(|blocked| blocked.contains(candidate))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    if reachable_agent_ids.insert(candidate.clone()) {
                        queue.push_back(candidate.clone());
                    }
                }
                continue;
            }

            if all_agent_ids.contains(target)
                && !is_system_agent_id(target)
                && !blocked_agent_ids
                    .map(|blocked| blocked.contains(target))
                    .unwrap_or(false)
                && reachable_agent_ids.insert(target.clone())
            {
                queue.push_back(target.clone());
            }
        }
    }

    reachable_agent_ids
}

/// Routing context that identifies an agent's current execution scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentRoutingContext {
    pub agent_id: AgentId,
    pub goal_id: GoalId,
    pub cycle_id: CycleId,
}

impl AgentRoutingContext {
    pub fn new(
        agent_id: impl Into<AgentId>,
        goal_id: impl Into<GoalId>,
        cycle_id: impl Into<CycleId>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            goal_id: goal_id.into(),
            cycle_id: cycle_id.into(),
        }
    }

    /// Storage/routing key with `agent:` prefix to avoid collision with execution keys.
    pub fn storage_key(&self) -> String {
        format!(
            "{}{}:{}:{}",
            AGENT_KEY_PREFIX, self.agent_id, self.goal_id, self.cycle_id
        )
    }
}

#[derive(Debug, Error)]
pub enum AgentDefinitionError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("agent definition validation error: {0}")]
    Validation(String),
}

/// Policy controlling automatic surface publication from agent artifacts.
///
/// When attached to an `AgentDefinition`, artifacts produced by this agent that
/// carry `RenderHints` are automatically assembled into a surface and published
/// to the specified route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoSurfacePolicy {
    /// Whether auto-surface publication is enabled.
    pub enabled: bool,
    /// Target route for the published surface (e.g., "/briefing").
    pub route: String,
    /// Optional V3 render materialization mode (for example `muij_surface`).
    #[serde(default)]
    pub materialize_as: Option<String>,
    /// Optional published surface kind override (for example `dashboard`).
    #[serde(default)]
    pub surface_kind: Option<String>,
    /// Optional placement kind override (`task`, `workspace`, `thread`, `global`).
    #[serde(default)]
    pub placement_kind: Option<String>,
    /// Optional pinned override for the publication placement.
    #[serde(default)]
    pub pinned: Option<bool>,
    /// Template for generating the surface title.
    /// May contain placeholders like `{agent_name}`, `{goal}`, etc.
    pub title_template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SocialPersonaConfig {
    #[serde(default = "default_introversion")]
    pub introversion: f64,
    #[serde(default = "default_social_tokens")]
    pub daily_tokens: u64,
    #[serde(default)]
    pub opted_out: bool,
}

/// Closed declaration that makes one agent definition callable from an
/// installed app as a typed child task. This is declaration material only:
/// the app installation/task owner must still seal the exact definition,
/// prompt, effective authority, schemas and byte ceilings before a child can
/// be created. Keeping it on the agent definition makes a schema or ceiling
/// edit advance the same reviewed definition digest as persona/tool changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentAppToolContract {
    pub input: AppManifestInputSchema,
    pub result: AppManifestInputSchema,
    pub max_input_bytes: u64,
    pub max_result_bytes: u64,
}

fn default_introversion() -> f64 {
    0.5
}
fn default_social_tokens() -> u64 {
    2_000
}

/// Full declarative agent definition. Parsed in Phase 2, interpreted in Phase 3+.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    /// Stable identity. Assigned once at creation, never changes.
    /// When omitted (empty), the backend auto-generates a kebab-case slug from `name`.
    #[serde(default)]
    pub agent_id: String,
    /// Monotonically increasing version number.
    #[serde(default = "default_version")]
    pub version: u32,
    pub name: String,
    /// Alternative names/handles this agent responds to (e.g., ["magican", "tia"]).
    /// Used for @-mention and greeting-based resolution in chat. Case-insensitive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// In-lexicon spellings used to ARM the on-device wake spotter, for names a
    /// speech model cannot recognise.
    ///
    /// The spotter is grammar-constrained: it silently drops any word missing
    /// from the model's vocabulary, so a coined name can never fire no matter
    /// how it is pronounced. Naming a spelling the lexicon does contain and that
    /// the model hears the same way lets the wake arm anyway; the hit is mapped
    /// back to this agent.
    ///
    /// Arming and wake-admission only. Display, chat/@-mention resolution, and
    /// open-vocabulary assistants (Siri App Intents, cloud realtime) are
    /// unaffected and keep using `name` + `aliases`. A non-empty list replaces
    /// the advertised names for the constrained wake recogniser. Empty means
    /// "arm `name` + aliases", which is correct when those names are in-lexicon.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_spellings: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// Optional typed app-facing child-task contract. Absence means this
    /// definition is not an `agent_as_tool` target, even if it is otherwise
    /// delegatable or runnable as an ordinary task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_tool: Option<AgentAppToolContract>,
    pub persona: String,
    /// Agent classification — personal (default) or worker.
    #[serde(default)]
    pub kind: AgentKind,
    /// Disables this agent and every reachable delegation descendant.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Tools this agent has access to (matches tool name or category). Empty = all tools.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Tools to exclude from the effective provider, deferred, structural,
    /// introspection, and dispatch projections (matches tool name or category).
    #[serde(default)]
    pub excluded_tools: Vec<String>,
    /// Tools to structurally deny at the dispatch level (denylist — always wins).
    ///
    /// Both exclusion lists narrow the effective snapshot; this field also
    /// records an explicit hard deny for audit and pre-side-effect rejection.
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Which browser transports this agent may use. **Empty means all three** —
    /// this is an opt-in restriction, not a default-deny.
    ///
    /// `cdp` attaches to the OWNER's own signed-in Chrome through the magicutor
    /// proxy: his cookies, his sessions, his logged-in accounts. A browser act
    /// under `cdp` is therefore not *"an agent visited a page"* but *"somebody
    /// visited a page as the owner"*. `headed` and `headless` launch
    /// `agent-browser`'s own Chrome against a per-work-context profile and carry
    /// no owner identity at all.
    ///
    /// # Why a ceiling on the AGENT rather than a rule about the work
    ///
    /// Deriving the transport from what an execution carries has a hole that
    /// delegation walks straight through: a delegated child inherits its
    /// parent's carrier, an autonomous cycle is created carrying none, and an
    /// execution carrying no work is treated as unconfined. So an agent an
    /// outsider can steer, holding no browser itself, reaches the owner's Chrome
    /// by delegating to one that does. A ceiling declared here survives that,
    /// because it is a fact about the agent rather than about the run.
    ///
    /// Unrecognised names are refused rather than ignored: a typo that silently
    /// dropped `cdp` would be a quiet demotion, and one that silently widened
    /// the set would be worse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_transports: Vec<String>,
    /// Per-tool parameter prefix deny patterns.
    ///
    /// Outer key = tool name, inner key = parameter name, value = list of
    /// denied prefixes. Before dispatch, if a resolved parameter value
    /// starts with any denied prefix (case-insensitive), the action is
    /// rejected. Implementation-agnostic — works for composite, command,
    /// shell, and javascript providers alike.
    ///
    /// Example:
    /// ```yaml
    /// denied_tool_params:
    ///   gmail:
    ///     command:
    ///       - "users messages trash"
    ///       - "users messages delete"
    ///       - "users messages batchDelete"
    /// ```
    #[serde(default)]
    pub denied_tool_params: HashMap<String, HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub constraints: AgentConstraints,
    #[serde(default)]
    pub trust_level: TrustLevel,
    /// Declarative memory tiers.
    #[serde(default)]
    pub memory_tiers: Vec<MemoryTierDefinition>,
    /// Declarative memory consolidation rules.
    #[serde(default)]
    pub memory_consolidation: Vec<MemoryConsolidationRule>,

    // Declarative configuration surfaces.
    #[serde(default)]
    pub prompt_pipeline: Option<PromptPipelineConfig>,
    #[serde(default)]
    pub circuit_breaker: Option<CircuitBreakerPolicy>,
    #[serde(default)]
    pub feedback_loops: Vec<FeedbackLoopDefinition>,
    #[serde(default)]
    pub notification_rules: Vec<NotificationRule>,
    #[serde(default)]
    pub retention: Option<RetentionPolicy>,
    #[serde(default)]
    pub llm_routing: Option<LlmRoutingConfig>,
    #[serde(default)]
    pub strategy: Option<StrategyPreference>,
    /// Parsed in Phase 2, interpreted in Phase 4.
    #[serde(default)]
    pub state_machines: HashMap<String, Value>,
    /// Optional declared principal identity for this agent.
    /// When set, pipelines spawned on behalf of this agent run under this
    /// principal instead of the env-var default, preserving the agent's own
    /// identity for access control and audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,

    /// Optional declared workspace ownership for this agent definition.
    /// Mutable/user-created agents are persisted under a concrete
    /// `(principal, workspace)` scope so visibility is isolated to that scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// Autonomous execution configuration. Only valid on Personal agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous_config: Option<AutonomousConfig>,

    /// Social persona configuration for the fleet social network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_persona: Option<SocialPersonaConfig>,

    /// Optional harness capability configuration. Only valid on Personal agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessConfig>,

    /// Whether this is the primary personal agent (exactly one per workspace).
    #[serde(default)]
    pub is_primary: bool,

    /// Legacy onboarding completion marker for persisted definitions.
    /// The first-run welcome modal was retired; keep this field readable so
    /// older definition files continue to round-trip without migration churn.
    #[serde(default)]
    pub onboarding_completed: bool,

    /// Agent IDs whose memory this agent can read (Personal agents only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readable_agents: Vec<String>,

    /// Default personality-mode skill name. Resolved against installed
    /// skills via `lookup_personality_mode` (workspace layer first, then
    /// system layer). Used to seed the `personality_profile` memory tier
    /// on first use. Falls back to "witty" if not set. Underscore form
    /// (e.g. `true_friend`) is bridged to kebab (`true-friend`) for legacy
    /// agent definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_personality: Option<String>,

    /// Memory isolation mode for this agent.
    #[serde(default)]
    pub user_memory_isolation: UserMemoryIsolation,

    /// Agent IDs this agent may delegate to. An explicit `"*"` means every
    /// eligible target in scope; an empty list means no delegation targets.
    #[serde(default = "default_delegation_targets")]
    pub delegation_targets: Vec<String>,

    /// Product discovery, direct-surface, and delegation eligibility policy.
    /// Defaults preserve existing ambient/wildcard behavior.
    #[serde(default)]
    pub invocation_policy: AgentInvocationPolicy,

    /// Auto-surface publication policy. When set, artifacts with render_hints
    /// are automatically assembled into a surface and published to the
    /// specified route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_surface_policy: Option<AutoSurfacePolicy>,

    /// Chat inline delegation policy. Controls whether this agent can be
    /// invoked directly from chat via `delegate_to_agent` as an ephemeral
    /// task-backed execution whose successful outputs are preserved before
    /// the backing task is deleted.
    ///
    /// - `None` / `"off"` — not available for inline delegation (default)
    /// - `"auto"` — auto-execute without confirmation (good for cheap agents)
    /// - `"confirm"` — requires user confirmation before running
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_inline: Option<ChatInlinePolicy>,
}

impl PartialEq for AgentDefinition {
    fn eq(&self, other: &Self) -> bool {
        let lhs = serde_json::to_value(self)
            .expect("AgentDefinition serialization should never fail for equality checks");
        let rhs = serde_json::to_value(other)
            .expect("AgentDefinition serialization should never fail for equality checks");
        lhs == rhs
    }
}

impl Eq for AgentDefinition {}

fn default_version() -> u32 {
    1
}

#[allow(dead_code)] // Will be used by Task trigger validation in a later phase.
fn parse_fixed_offset_seconds(raw: &str) -> Option<i32> {
    if raw.len() < 3 {
        return None;
    }
    let (sign, rest) = match raw.as_bytes()[0] {
        b'+' => (1_i32, &raw[1..]),
        b'-' => (-1_i32, &raw[1..]),
        _ => return None,
    };
    let (hours_str, minutes_str) = if let Some((h, m)) = rest.split_once(':') {
        (h, m)
    } else if rest.len() == 4 {
        (&rest[0..2], &rest[2..4])
    } else {
        (rest, "0")
    };
    let hours: i32 = hours_str.parse().ok()?;
    let minutes: i32 = minutes_str.parse().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

#[allow(dead_code)] // Will be used by Task trigger validation in a later phase.
fn is_valid_timezone_identifier(raw: &str) -> bool {
    let candidate = raw.trim();
    if candidate.is_empty() {
        return false;
    }
    if candidate.eq_ignore_ascii_case("UTC") {
        return true;
    }
    if parse_fixed_offset_seconds(candidate).is_some() {
        return true;
    }
    candidate.contains('/') && candidate.parse::<Tz>().is_ok()
}

fn action_pattern_has_empty_entry(pattern: &ActionPattern) -> bool {
    match pattern {
        ActionPattern::Single(value) => value.trim().is_empty(),
        ActionPattern::Multiple(values) => {
            values.is_empty() || values.iter().any(|value| value.trim().is_empty())
        },
    }
}

fn action_pattern_has_whitespace_padding(pattern: &ActionPattern) -> bool {
    match pattern {
        ActionPattern::Single(value) => value != value.trim(),
        ActionPattern::Multiple(values) => values.iter().any(|value| value != value.trim()),
    }
}

fn has_path_unsafe_chars(value: &str) -> bool {
    value == "." || value.contains("..") || value.contains('/') || value.contains('\\')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptPipelineSourceKind {
    DefinitionPersona,
    DefinitionGoal,
    UserProfile,
    UserKnowledge,
    Corrections,
    Episodes,
    Tier,
    Tiers,
    DerivedFailureAnalysis,
    DerivedSuccessPatterns,
    /// Feedback loop transformer output injected into next planning cycle (P6-10).
    FeedbackContext,
}

fn parse_prompt_definition_goal_source(source_ref: &str) -> Option<&str> {
    let inner = source_ref
        .strip_prefix("definition.goals[")
        .and_then(|rest| rest.strip_suffix(']'))?;
    let selector = inner.trim();
    (!selector.is_empty()).then_some(selector)
}

fn parse_prompt_memory_tier_source(source_ref: &str) -> Option<(&str, Option<&str>)> {
    let inner = source_ref.strip_prefix("memory.tier[")?;
    let close_idx = inner.find(']')?;
    let tier_name = inner[..close_idx].trim();
    if tier_name.is_empty() {
        return None;
    }
    let suffix = &inner[close_idx + 1..];
    if suffix.is_empty() {
        return Some((tier_name, None));
    }
    let field_path = suffix.strip_prefix('.')?.trim();
    if field_path.is_empty() {
        return None;
    }
    Some((tier_name, Some(field_path)))
}

fn prompt_pipeline_episodes_source_uses_unprocessed(source_ref: &str) -> bool {
    let canonical = source_ref
        .strip_prefix("memory.")
        .unwrap_or(source_ref)
        .trim();
    let Some(inner) = canonical
        .strip_prefix("episodes(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };

    inner
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .any(|token| {
            token
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "unprocessed")
        })
}

fn prompt_pipeline_source_kind(source_ref: &str) -> Option<PromptPipelineSourceKind> {
    let source_ref = source_ref.trim();
    if source_ref == "definition.persona" {
        return Some(PromptPipelineSourceKind::DefinitionPersona);
    }
    if parse_prompt_definition_goal_source(source_ref).is_some() {
        return Some(PromptPipelineSourceKind::DefinitionGoal);
    }
    if source_ref == "memory.user_profile" {
        return Some(PromptPipelineSourceKind::UserProfile);
    }
    if source_ref == "memory.user_knowledge" {
        return Some(PromptPipelineSourceKind::UserKnowledge);
    }
    if source_ref == "memory.corrections" {
        return Some(PromptPipelineSourceKind::Corrections);
    }

    let canonical = source_ref.strip_prefix("memory.").unwrap_or(source_ref);
    if matches!(
        SourceRef::parse(canonical),
        Some(SourceRef::Episodes { .. })
    ) {
        return Some(PromptPipelineSourceKind::Episodes);
    }
    if parse_prompt_memory_tier_source(source_ref).is_some() {
        return Some(PromptPipelineSourceKind::Tier);
    }
    if matches!(SourceRef::parse(source_ref), Some(SourceRef::Tiers { .. })) {
        return Some(PromptPipelineSourceKind::Tiers);
    }
    if source_ref == "derived.failure_analysis" {
        return Some(PromptPipelineSourceKind::DerivedFailureAnalysis);
    }
    if source_ref == "derived.success_patterns" {
        return Some(PromptPipelineSourceKind::DerivedSuccessPatterns);
    }
    if source_ref.starts_with("feedback.") || source_ref == "strategy_context" {
        return Some(PromptPipelineSourceKind::FeedbackContext);
    }

    None
}

fn validate_prompt_pipeline_goal_selector(
    section_name: &str,
    source_ref: &str,
    selector: &str,
) -> Result<(), AgentDefinitionError> {
    if matches!(selector, "triggered_goal_id" | "goal_id") {
        return Ok(());
    }

    validate_goal_identifier(selector).map_err(|err| match err {
        AgentDefinitionError::Validation(message) => AgentDefinitionError::Validation(format!(
            "prompt pipeline section `{section_name}` source `{source_ref}` has invalid goal selector `{selector}`: {message}"
        )),
        other => other,
    })?;

    // NOTE: Goal cross-reference validation removed — goals are no longer part of
    // AgentDefinition (moved to Task in Unified Agentic Architecture).
    Ok(())
}

const MAX_GOAL_ID_BYTES: usize = 255;
/// Per-string field cap for `AgentDefinition` deserialization (persona,
/// description, prompt-pipeline blocks, etc.). Raised from 16 KiB → 32
/// KiB after `internal-system-analyst` and `simple-data-analyst` grew
/// past 16 KiB; the older cap was a prompt-budget heuristic from a
/// time when chat-side personas were shorter and reasoning models had
/// tighter context windows. 32 KiB still bounds the worst case at
/// roughly 8K tokens (rule-of-thumb 4 bytes / token) — well below
/// every model's per-turn budget.
///
/// If this hits again, prefer externalizing the persona body to a
/// sibling file referenced from the YAML (`persona_file: persona.md`)
/// rather than continuing to raise the cap.
const MAX_STRING_FIELD_BYTES: usize = 32 * 1024;
const MAX_APP_TOOL_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_APP_TOOL_FIELDS: usize = 256;
const MAX_APP_TOOL_VALUE_BYTES: u64 = 256 * 1024;
const MULTI_VALIDATION_ERROR_PREFIX: &str = "multiple validation errors";

fn validate_goal_identifier(goal_id: &str) -> Result<(), AgentDefinitionError> {
    if goal_id.trim().is_empty() {
        return Err(AgentDefinitionError::Validation(
            "goal id must not be empty".to_string(),
        ));
    }
    if goal_id != goal_id.trim() {
        return Err(AgentDefinitionError::Validation(format!(
            "goal id `{goal_id}` must not contain leading or trailing whitespace"
        )));
    }
    if goal_id.len() > MAX_GOAL_ID_BYTES {
        return Err(AgentDefinitionError::Validation(format!(
            "goal id `{goal_id}` must be <= {MAX_GOAL_ID_BYTES} bytes"
        )));
    }
    if !goal_id.is_ascii() {
        return Err(AgentDefinitionError::Validation(format!(
            "goal id `{goal_id}` must be ASCII"
        )));
    }
    if has_path_unsafe_chars(goal_id) {
        return Err(AgentDefinitionError::Validation(format!(
            "goal id `{goal_id}` must not contain `..`, `/`, or `\\`"
        )));
    }
    if let Err(AgentStorageError::InvalidIdentifier(_)) = validate_agent_identifier(goal_id) {
        return Err(AgentDefinitionError::Validation(format!(
            "goal id `{goal_id}` uses reserved infrastructure names"
        )));
    }
    Ok(())
}

fn validate_string_length_limits(definition: &AgentDefinition) -> Result<(), AgentDefinitionError> {
    let value = serde_json::to_value(definition).map_err(|err| {
        AgentDefinitionError::Validation(format!(
            "failed to inspect definition for string length limits: {err}"
        ))
    })?;
    validate_string_length_limits_in_value(&value, "definition")
}

fn validate_string_length_limits_in_value(
    value: &Value,
    path: &str,
) -> Result<(), AgentDefinitionError> {
    match value {
        Value::String(text) => {
            if text.len() > MAX_STRING_FIELD_BYTES {
                return Err(AgentDefinitionError::Validation(format!(
                    "`{path}` exceeds maximum length of {MAX_STRING_FIELD_BYTES} bytes"
                )));
            }
        },
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_string_length_limits_in_value(item, &format!("{path}[{index}]"))?;
            }
        },
        Value::Object(map) => {
            for (key, item) in map {
                if key.len() > MAX_STRING_FIELD_BYTES {
                    return Err(AgentDefinitionError::Validation(format!(
                        "object key at `{path}` exceeds maximum length of {MAX_STRING_FIELD_BYTES} bytes"
                    )));
                }
                validate_string_length_limits_in_value(item, &format!("{path}.{key}"))?;
            }
        },
        _ => {},
    }

    Ok(())
}

fn push_unique_validation_error(errors: &mut Vec<String>, message: String) {
    if !errors.iter().any(|existing| existing == &message) {
        errors.push(message);
    }
}

fn format_validation_error_message(errors: &[String]) -> String {
    if errors.len() == 1 {
        return errors[0].clone();
    }
    let mut combined = String::from(MULTI_VALIDATION_ERROR_PREFIX);
    for error in errors {
        combined.push_str("\n- ");
        combined.push_str(error);
    }
    combined
}

fn collect_coarse_validation_errors(definition: &AgentDefinition) -> Vec<String> {
    let mut errors = Vec::new();

    if definition.agent_id.trim().is_empty() {
        push_unique_validation_error(&mut errors, "agent_id must not be empty".to_string());
    }
    if validate_agent_identifier(&definition.agent_id).is_err() {
        push_unique_validation_error(
            &mut errors,
            "agent_id must be ASCII, <= 255 bytes, trimmed, not `.`/`..`, without `/` or `\\`, and must not use reserved infrastructure names"
                .to_string(),
        );
    }

    if definition.version == 0 {
        push_unique_validation_error(&mut errors, "version must be >= 1".to_string());
    }

    if let Err(AgentDefinitionError::Validation(msg)) = validate_string_length_limits(definition) {
        push_unique_validation_error(&mut errors, msg);
    }

    if definition.name.trim().is_empty() {
        push_unique_validation_error(&mut errors, "name must not be empty".to_string());
    }
    if definition.persona.trim().is_empty() {
        push_unique_validation_error(&mut errors, "persona must not be empty".to_string());
    }
    if definition.trust_level.0.trim().is_empty() {
        push_unique_validation_error(&mut errors, "trust_level must not be empty".to_string());
    } else if !definition.trust_level.is_recognized() {
        push_unique_validation_error(
            &mut errors,
            format!(
                "trust_level '{}' is not a recognized level (expected one of: {}, {}, {}, {}; \
                 legacy 'standard' maps to '{}'). An unrecognized level matches no trust policy \
                 and denies EVERY action at runtime.",
                definition.trust_level.0.trim(),
                TrustLevel::BUILTIN,
                TrustLevel::LOCAL,
                TrustLevel::REVIEWED,
                TrustLevel::UNTRUSTED,
                TrustLevel::LOCAL,
            ),
        );
    }
    if definition.trust_level.is_untrusted()
        && definition.tools.is_empty()
        && !is_strictly_tool_free_public_surface(definition)
    {
        push_unique_validation_error(
            &mut errors,
            "untrusted agents must declare an explicit non-empty tools allowlist".to_string(),
        );
    }

    let mut tool_names = HashSet::new();
    for tool in &definition.tools {
        if tool.trim().is_empty() {
            push_unique_validation_error(&mut errors, "tool name must not be empty".to_string());
        }
        if !tool_names.insert(tool.clone()) {
            push_unique_validation_error(&mut errors, format!("duplicate tool `{}`", tool));
        }
    }

    if let Some(contract) = definition.app_tool.as_ref() {
        let input_bytes = serde_json::to_vec(&contract.input)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        let result_bytes = serde_json::to_vec(&contract.result)
            .map(|bytes| bytes.len())
            .unwrap_or(usize::MAX);
        let empty_enum = contract
            .input
            .fields
            .values()
            .chain(contract.result.fields.values())
            .any(
                |field| matches!(field, AppManifestField::Enum { values, .. } if values.is_empty()),
            );
        if contract.max_input_bytes == 0
            || contract.max_result_bytes == 0
            || contract.max_input_bytes > MAX_APP_TOOL_VALUE_BYTES
            || contract.max_result_bytes > MAX_APP_TOOL_VALUE_BYTES
            || contract.input.fields.len() > MAX_APP_TOOL_FIELDS
            || contract.result.fields.len() > MAX_APP_TOOL_FIELDS
            || input_bytes > MAX_APP_TOOL_SCHEMA_BYTES
            || result_bytes > MAX_APP_TOOL_SCHEMA_BYTES
            || empty_enum
        {
            push_unique_validation_error(
                &mut errors,
                "app_tool must declare valid bounded schemas and 1..=262144 byte input/result ceilings"
                    .to_string(),
            );
        }
    }

    if definition.constraints.max_iterations == 0 {
        push_unique_validation_error(
            &mut errors,
            "constraints.max_iterations must be > 0".to_string(),
        );
    }
    if definition.constraints.max_tokens_per_cycle == 0 {
        push_unique_validation_error(
            &mut errors,
            "constraints.max_tokens_per_cycle must be > 0".to_string(),
        );
    }
    if definition.constraints.max_consecutive_failures == 0 {
        push_unique_validation_error(
            &mut errors,
            "constraints.max_consecutive_failures must be > 0".to_string(),
        );
    }
    if definition.constraints.coordination.delegation_timeout_secs == 0 {
        push_unique_validation_error(
            &mut errors,
            "constraints.coordination.delegation_timeout_secs must be > 0".to_string(),
        );
    }
    if definition.constraints.approval_ttl_secs == 0 {
        push_unique_validation_error(
            &mut errors,
            "constraints.approval_ttl_secs must be > 0".to_string(),
        );
    }

    errors
}

impl AgentDefinition {
    /// Parse and validate an agent definition from YAML text.
    ///
    /// Applies defaults (e.g. memory tiers for autonomous personal agents)
    /// before validation so that default-injected tiers pass tier-reference checks.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, AgentDefinitionError> {
        let mut definition: Self = serde_yaml::from_str(yaml)?;
        definition.apply_defaults();
        definition.validate()?;
        // Soft validation pass — warn-log template/schema mismatches
        // once at load time instead of every render. Definition load
        // succeeds even if mismatches are present (the renderer's
        // structured fallback still produces sensible output).
        super::memory_tier_interpreter::warn_template_schema_mismatches(&definition);
        Ok(definition)
    }

    /// Parse and validate an agent definition from a YAML file.
    pub fn from_yaml_file(path: impl AsRef<Path>) -> Result<Self, AgentDefinitionError> {
        let content = fs::read_to_string(path)?;
        Self::from_yaml_str(&content)
    }

    /// Inject default memory configuration for personal agents.
    ///
    /// When `kind == Personal` and both `memory_tiers` and
    /// `memory_consolidation` are empty, this method populates them with the
    /// standard 6-tier / 9-rule defaults and sets episode retention to
    /// consolidate-before-delete with a 7-day window.
    ///
    /// Memory is useful for all personal agents (entity tracking, insights,
    /// activity history) — not just autonomous ones.
    ///
    /// Explicit configurations are never overridden — if either `memory_tiers`
    /// or `memory_consolidation` is non-empty the definition is left as-is.
    /// Worker agents never receive defaults.
    pub fn apply_defaults(&mut self) {
        if self.kind != AgentKind::Personal {
            return;
        }

        // ── Memory defaults ──
        if self.memory_tiers.is_empty() && self.memory_consolidation.is_empty() {
            let (tiers, rules, episode_retention) = default_memory_config_for_personal_agent();
            self.memory_tiers = tiers;
            self.memory_consolidation = rules;

            // Inject episode retention into the retention policy, creating one if absent.
            let retention = self.retention.get_or_insert_with(RetentionPolicy::default);
            retention.episodes = episode_retention;
        }

        // ── Retention (even if memory was already present) ──
        if self.retention.is_none() {
            self.retention = Some(RetentionPolicy::default());
        }

        // ── Prompt pipeline & feedback loops (coupled — default loops reference
        //    default pipeline sections so they must be injected together) ──
        if self.prompt_pipeline.is_none() {
            self.prompt_pipeline = Some(PromptPipelineConfig {
                sections: vec![
                    PromptSection {
                        name: "persona".to_string(),
                        source: "definition.persona".to_string(),
                        content: None,
                        required: true,
                        condition: None,
                        format: None,
                        filter: None,
                    },
                    PromptSection {
                        name: "memory_context".to_string(),
                        source: "memory.episodes(limit=5)".to_string(),
                        content: None,
                        required: false,
                        condition: None,
                        format: None,
                        filter: None,
                    },
                    PromptSection {
                        name: "task_progress".to_string(),
                        source: "memory.tier[task_progress].context_summary".to_string(),
                        content: None,
                        required: false,
                        condition: None,
                        format: None,
                        filter: None,
                    },
                    PromptSection {
                        name: "failure_context".to_string(),
                        source: "feedback.failure_context".to_string(),
                        content: None,
                        required: false,
                        condition: Some("has_recent_failures".to_string()),
                        format: None,
                        filter: None,
                    },
                    PromptSection {
                        name: "success_patterns".to_string(),
                        source: "feedback.success_patterns".to_string(),
                        content: None,
                        required: false,
                        condition: Some("has_recent_successes".to_string()),
                        format: None,
                        filter: None,
                    },
                ],
                output_rules: PromptOutputRules {
                    max_context_tokens: 4000,
                    truncation_priority: vec![
                        "task_progress".to_string(),
                        "success_patterns".to_string(),
                        "failure_context".to_string(),
                        "memory_context".to_string(),
                    ],
                },
            });

            // Default feedback loops reference sections in the default pipeline above
            // (failure_context, success_patterns), so only inject them when we also
            // inject the default pipeline.
            if self.feedback_loops.is_empty() {
                self.feedback_loops = FeedbackLoopDefinition::defaults();
            }
        }

        // ── Circuit breaker ──
        if self.circuit_breaker.is_none() {
            self.circuit_breaker = Some(CircuitBreakerPolicy::default());
        }

        // ── Notification rules ──
        if self.notification_rules.is_empty() {
            self.notification_rules = vec![
                NotificationRule {
                    r#match: "agent.cycle.failed".to_string(),
                    severity: NotificationSeverity::Medium,
                    channels: vec!["chat".to_string()],
                    condition: None,
                    message: None,
                },
                // Canonical `hitl.requested` covers every source.
                // Replaces the legacy `approval.requested` projection
                // retired in v0.6.505.
                NotificationRule {
                    r#match: "hitl.requested".to_string(),
                    severity: NotificationSeverity::High,
                    channels: vec!["chat".to_string()],
                    condition: None,
                    message: None,
                },
            ];
        }

        // ── Strategy ──
        if self.strategy.is_none() {
            self.strategy = Some(StrategyPreference::default());
        }

        // ── State machines ──
        if self.state_machines.is_empty() {
            self.state_machines = std::collections::HashMap::from([(
                "task_lifecycle".to_string(),
                serde_json::json!({
                    "initial_state": "idle",
                    "states": ["idle", "planning", "executing", "reviewing", "completed", "failed"],
                    "transitions": [
                        { "from": "idle",      "to": "planning",  "on": "start",    "actions": ["persist_state"] },
                        { "from": "planning",  "to": "executing", "on": "plan_ready","actions": ["persist_state"] },
                        { "from": "executing", "to": "reviewing", "on": "done",     "actions": ["persist_state"] },
                        { "from": "reviewing", "to": "completed", "on": "accept",   "actions": ["persist_state"] },
                        { "from": "reviewing", "to": "executing", "on": "retry",    "actions": ["persist_state"] },
                        { "from": "executing", "to": "failed",    "on": "error",    "actions": ["persist_state"] },
                        { "from": "failed",    "to": "planning",  "on": "retry",    "actions": ["persist_state"] }
                    ]
                }),
            )]);
        }
    }

    /// Serialize the definition back to YAML.
    pub fn to_yaml_string(&self) -> Result<String, AgentDefinitionError> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// Structural validation only. Runtime semantics are intentionally out-of-scope
    /// for Phase 2.
    pub fn validate(&self) -> Result<(), AgentDefinitionError> {
        let mut errors = collect_coarse_validation_errors(self);
        match self.validate_fail_fast() {
            Ok(()) => {},
            Err(AgentDefinitionError::Validation(message)) => {
                push_unique_validation_error(&mut errors, message);
            },
            Err(other) => return Err(other),
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(AgentDefinitionError::Validation(
                format_validation_error_message(&errors),
            ))
        }
    }

    fn validate_fail_fast(&self) -> Result<(), AgentDefinitionError> {
        if self.agent_id.trim().is_empty() {
            return Err(AgentDefinitionError::Validation(
                "agent_id must not be empty".to_string(),
            ));
        }
        if validate_agent_identifier(&self.agent_id).is_err() {
            return Err(AgentDefinitionError::Validation(
                "agent_id must be ASCII, <= 255 bytes, trimmed, not `.`/`..`, without `/` or `\\`, and must not use reserved infrastructure names".to_string(),
            ));
        }

        if self.version == 0 {
            return Err(AgentDefinitionError::Validation(
                "version must be >= 1".to_string(),
            ));
        }
        validate_string_length_limits(self)?;

        if self.name.trim().is_empty() {
            return Err(AgentDefinitionError::Validation(
                "name must not be empty".to_string(),
            ));
        }

        if self.persona.trim().is_empty() {
            return Err(AgentDefinitionError::Validation(
                "persona must not be empty".to_string(),
            ));
        }

        if let Some(social) = self.social_persona.as_ref() {
            if !social.introversion.is_finite() || !(0.0..=1.0).contains(&social.introversion) {
                return Err(AgentDefinitionError::Validation(
                    "social_persona.introversion must be a finite value between 0 and 1"
                        .to_string(),
                ));
            }
            if social.daily_tokens > 10_000_000 {
                return Err(AgentDefinitionError::Validation(
                    "social_persona.daily_tokens must not exceed 10000000".to_string(),
                ));
            }
        }

        if self.trust_level.0.trim().is_empty() {
            return Err(AgentDefinitionError::Validation(
                "trust_level must not be empty".to_string(),
            ));
        }
        if self.trust_level.is_untrusted()
            && self.tools.is_empty()
            && !is_strictly_tool_free_public_surface(self)
        {
            return Err(AgentDefinitionError::Validation(
                "untrusted agents must declare an explicit non-empty tools allowlist".to_string(),
            ));
        }

        let mut tool_names = HashSet::new();
        for tool in &self.tools {
            if tool.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "tool name must not be empty".to_string(),
                ));
            }
            if !tool_names.insert(tool.clone()) {
                return Err(AgentDefinitionError::Validation(format!(
                    "duplicate tool `{}`",
                    tool
                )));
            }
        }

        for key in self.state_machines.keys() {
            if key.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "state_machines keys must not be empty".to_string(),
                ));
            }
        }

        let mut tier_names = HashSet::new();
        for tier in &self.memory_tiers {
            let tier_name = tier.name.trim();
            if tier_name.is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "memory tier name must not be empty".to_string(),
                ));
            }
            if tier.name != tier_name {
                return Err(AgentDefinitionError::Validation(format!(
                    "memory tier `{}` name must not contain leading or trailing whitespace",
                    tier.name
                )));
            }
            if tier.name.contains('.') {
                return Err(AgentDefinitionError::Validation(format!(
                    "memory tier `{}` name must not contain `.` (reserved for consolidation DSL)",
                    tier.name
                )));
            }
            if !tier_names.insert(tier_name.to_string()) {
                return Err(AgentDefinitionError::Validation(format!(
                    "duplicate memory tier `{}`",
                    tier.name
                )));
            }
            if tier.description.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(format!(
                    "memory tier `{}` description must not be empty",
                    tier.name
                )));
            }
            if tier.render.format.trim().is_empty() || tier.render.template.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(format!(
                    "memory tier `{}` render format/template must not be empty",
                    tier.name
                )));
            }
            if matches!(tier.retention, RetentionMode::Days(0)) {
                return Err(AgentDefinitionError::Validation(format!(
                    "memory tier `{}` retention.days must be > 0",
                    tier.name
                )));
            }
            for field in tier.schema.keys() {
                if field.trim().is_empty() {
                    return Err(AgentDefinitionError::Validation(format!(
                        "memory tier `{}` contains an empty schema field key",
                        tier.name
                    )));
                }
            }
        }

        let mut rule_names = HashSet::new();
        for rule in &self.memory_consolidation {
            if rule.name.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "memory consolidation rule name must not be empty".to_string(),
                ));
            }
            if !rule_names.insert(rule.name.clone()) {
                return Err(AgentDefinitionError::Validation(format!(
                    "duplicate consolidation rule `{}`",
                    rule.name
                )));
            }
            if rule.source.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(format!(
                    "consolidation rule `{}` source must not be empty",
                    rule.name
                )));
            }
            if let ConsolidationTrigger::Batch {
                interval_hours,
                interval_days,
                min_episodes,
                max_staleness_hours,
            } = &rule.trigger
            {
                if interval_hours.is_some_and(|v| v == 0) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` batch trigger interval_hours must be > 0 when provided",
                        rule.name
                    )));
                }
                if interval_days.is_some_and(|v| v == 0) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` batch trigger interval_days must be > 0 when provided",
                        rule.name
                    )));
                }
                if min_episodes.is_some_and(|v| v == 0) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` batch trigger min_episodes must be > 0 when provided",
                        rule.name
                    )));
                }
                if max_staleness_hours.is_some_and(|v| v == 0) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` batch trigger max_staleness_hours must be > 0 when provided",
                        rule.name
                    )));
                }
                if interval_hours.is_none()
                    && interval_days.is_none()
                    && min_episodes.is_none()
                    && max_staleness_hours.is_none()
                {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` batch trigger must set at least one of interval_hours, interval_days, min_episodes, or max_staleness_hours",
                        rule.name
                    )));
                }
            }
            let parsed_source = SourceRef::parse(&rule.source);
            if parsed_source.is_none() {
                return Err(AgentDefinitionError::Validation(format!(
                    "consolidation rule `{}` source `{}` is invalid; expected episodes(...) or tiers(...)",
                    rule.name, rule.source
                )));
            }

            if let Some(SourceRef::Episodes {
                goal_id: Some(source_goal_id),
                ..
            }) = &parsed_source
            {
                validate_goal_identifier(source_goal_id)?;
            }
            if let ConsolidationTrigger::Batch {
                interval_hours,
                interval_days,
                min_episodes,
                ..
            } = &rule.trigger
            {
                if min_episodes.is_some()
                    && !matches!(parsed_source, Some(SourceRef::Episodes { .. }))
                {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` batch trigger min_episodes is only supported for episodes(...) sources",
                        rule.name
                    )));
                }
                let min_only = interval_hours.is_none() && interval_days.is_none();
                if min_only {
                    if let Some(SourceRef::Episodes { unprocessed, .. }) = &parsed_source {
                        if !unprocessed {
                            return Err(AgentDefinitionError::Validation(format!(
                                "consolidation rule `{}` batch min_episodes-only trigger requires episodes(..., unprocessed=true) to avoid replaying identical inputs",
                                rule.name
                            )));
                        }
                    }
                }
            }

            if let Some(SourceRef::Tiers { tier_refs }) = &parsed_source {
                for tier_ref in tier_refs {
                    if let Some(agent) = &tier_ref.agent {
                        if has_path_unsafe_chars(agent) {
                            return Err(AgentDefinitionError::Validation(format!(
                                "consolidation rule `{}` source references agent `{}` \
                                 with path-unsafe characters",
                                rule.name, agent
                            )));
                        }
                        if agent != &self.agent_id {
                            return Err(AgentDefinitionError::Validation(format!(
                                "consolidation rule `{}` source references cross-agent tier `{}`; cross-agent tier sources are not supported",
                                rule.name, rule.source
                            )));
                        }
                    }

                    let is_local_ref = match &tier_ref.agent {
                        Some(agent) => agent == &self.agent_id,
                        None => true,
                    };

                    let normalized_tier_name = tier_ref.tier_name.trim();
                    if normalized_tier_name.is_empty() {
                        return Err(AgentDefinitionError::Validation(format!(
                            "consolidation rule `{}` source tier references must not be empty",
                            rule.name
                        )));
                    }
                    if tier_ref.tier_name != normalized_tier_name {
                        return Err(AgentDefinitionError::Validation(format!(
                            "consolidation rule `{}` source tier `{}` must not contain leading or trailing whitespace",
                            rule.name, tier_ref.tier_name
                        )));
                    }

                    if is_local_ref && !tier_names.contains(normalized_tier_name) {
                        return Err(AgentDefinitionError::Validation(format!(
                            "consolidation rule `{}` source references unknown local tier `{}`",
                            rule.name, tier_ref.tier_name
                        )));
                    }

                    if is_local_ref
                        && matches!(
                            rule.trigger,
                            ConsolidationTrigger::Batch { .. }
                                | ConsolidationTrigger::RetentionExpiry
                        )
                    {
                        if let Some(source_tier_definition) = self
                            .memory_tiers
                            .iter()
                            .find(|tier| tier.name == normalized_tier_name)
                        {
                            if matches!(source_tier_definition.scope, TierScope::AgentGoal) {
                                return Err(AgentDefinitionError::Validation(format!(
                                    "consolidation rule `{}` source `{}` is unsupported for {:?} trigger because tier `{}` is agent-goal scoped",
                                    rule.name,
                                    rule.source,
                                    rule.trigger,
                                    source_tier_definition.name
                                )));
                            }
                        }
                    }
                }
            }
            if rule.target.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(format!(
                    "consolidation rule `{}` target must not be empty",
                    rule.name
                )));
            }

            let mut target_root: Option<String> = None;
            if let Some(channel) = rule.target.strip_prefix("report:") {
                if channel.trim().is_empty() {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` report target channel must not be empty",
                        rule.name
                    )));
                }
            } else {
                let target_segments = rule.target.split('.').map(str::trim).collect::<Vec<_>>();
                if target_segments.iter().any(|segment| segment.is_empty()) {
                    if target_segments
                        .first()
                        .map(|segment| segment.is_empty())
                        .unwrap_or(true)
                    {
                        return Err(AgentDefinitionError::Validation(format!(
                            "consolidation rule `{}` target must not begin with `.`",
                            rule.name
                        )));
                    }
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` target must not contain empty path segments",
                        rule.name
                    )));
                }
                let target_root_segment = target_segments.first().copied().unwrap_or_default();
                if target_root_segment.is_empty() {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` target must not begin with `.`",
                        rule.name
                    )));
                }
                if target_root_segment != "user" && !tier_names.contains(target_root_segment) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` target references unknown tier root `{}`",
                        rule.name, target_root_segment
                    )));
                }
                if target_root_segment == "user" && target_segments.len() < 2 {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` user target must include a non-empty path after `user.`",
                        rule.name
                    )));
                }
                if target_root_segment != "user"
                    && matches!(
                        rule.trigger,
                        ConsolidationTrigger::Batch { .. } | ConsolidationTrigger::RetentionExpiry
                    )
                {
                    if let Some(target_tier_definition) = self
                        .memory_tiers
                        .iter()
                        .find(|tier| tier.name == target_root_segment)
                    {
                        if matches!(target_tier_definition.scope, TierScope::AgentGoal) {
                            return Err(AgentDefinitionError::Validation(format!(
                                "consolidation rule `{}` target `{}` is unsupported for {:?} trigger because tier `{}` is agent-goal scoped",
                                rule.name,
                                rule.target,
                                rule.trigger,
                                target_tier_definition.name
                            )));
                        }
                    }
                }
                target_root = Some(target_root_segment.to_string());
            }

            // Phase 3 runtime only applies cycle_completed+structured rules against episode inputs
            // and tier-backed targets. Reject unsupported-yet-accepted combinations at definition
            // time to keep validation/runtime semantics aligned.
            if matches!(rule.trigger, ConsolidationTrigger::CycleCompleted)
                && matches!(rule.transform, ConsolidationTransform::Structured { .. })
            {
                if !matches!(parsed_source, Some(SourceRef::Episodes { .. })) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` source `{}` is unsupported for cycle_completed structured transforms in phase 3; expected episodes(...)",
                        rule.name, rule.source
                    )));
                }
                if rule.target.starts_with("report:") {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` target `{}` is unsupported for cycle_completed structured transforms in phase 3",
                        rule.name, rule.target
                    )));
                }
                if matches!(target_root.as_deref(), Some("user")) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "consolidation rule `{}` target `{}` is unsupported for cycle_completed structured transforms in phase 3",
                        rule.name, rule.target
                    )));
                }
            }

            match &rule.transform {
                ConsolidationTransform::Structured { .. } => {},
                ConsolidationTransform::Llm { prompt, .. } => {
                    if prompt.trim().is_empty() {
                        return Err(AgentDefinitionError::Validation(format!(
                            "consolidation rule `{}` llm prompt must not be empty",
                            rule.name
                        )));
                    }
                },
                ConsolidationTransform::Render { template } => {
                    if template.trim().is_empty() {
                        return Err(AgentDefinitionError::Validation(format!(
                            "consolidation rule `{}` render template must not be empty",
                            rule.name
                        )));
                    }
                },
            }
        }

        let mut prompt_pipeline_section_names: Option<HashSet<String>> = None;
        if let Some(prompt_pipeline) = &self.prompt_pipeline {
            let mut section_names = HashSet::new();
            for section in &prompt_pipeline.sections {
                if section.name.trim().is_empty() {
                    return Err(AgentDefinitionError::Validation(
                        "prompt pipeline section name must not be empty".to_string(),
                    ));
                }
                let has_source = !section.source.trim().is_empty();
                let has_content = section
                    .content
                    .as_ref()
                    .is_some_and(|c| !c.trim().is_empty());
                if has_source && has_content {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt pipeline section `{}` must have either `source` or `content`, not both",
                        section.name
                    )));
                }
                if !has_source && !has_content {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt pipeline section `{}` must have either `source` or `content`",
                        section.name
                    )));
                }
                if section
                    .condition
                    .as_ref()
                    .is_some_and(|c| c.trim().is_empty())
                {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt pipeline section `{}` condition must not be empty when provided",
                        section.name
                    )));
                }
                if section.filter.as_ref().is_some_and(|f| f.trim().is_empty()) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt pipeline section `{}` filter must not be empty when provided",
                        section.name
                    )));
                }
                if section.format.as_ref().is_some_and(|f| f.trim().is_empty()) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt pipeline section `{}` format must not be empty when provided",
                        section.name
                    )));
                }
                // Inline content sections skip source-kind validation entirely.
                if has_content {
                    if section.filter.is_some() {
                        return Err(AgentDefinitionError::Validation(format!(
                            "prompt pipeline section `{}` filter is not supported for inline content sections",
                            section.name
                        )));
                    }
                    if !section_names.insert(section.name.clone()) {
                        return Err(AgentDefinitionError::Validation(format!(
                            "duplicate prompt pipeline section `{}`",
                            section.name
                        )));
                    }
                    continue;
                }
                let Some(source_kind) = prompt_pipeline_source_kind(&section.source) else {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt pipeline section `{}` source `{}` is invalid or unsupported",
                        section.name, section.source
                    )));
                };
                if section.filter.is_some()
                    && matches!(
                        source_kind,
                        PromptPipelineSourceKind::Tier | PromptPipelineSourceKind::Tiers
                    )
                {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt pipeline section `{}` filter is not supported for tier sources",
                        section.name
                    )));
                }
                let source_ref = section.source.trim();
                match source_kind {
                    PromptPipelineSourceKind::DefinitionGoal => {
                        let goal_selector = parse_prompt_definition_goal_source(source_ref)
                            .ok_or_else(|| {
                                AgentDefinitionError::Validation(format!(
                                    "prompt pipeline section `{}` source `{}` is invalid or unsupported",
                                    section.name, section.source
                                ))
                            })?;
                        validate_prompt_pipeline_goal_selector(
                            &section.name,
                            source_ref,
                            goal_selector,
                        )?;
                    },
                    PromptPipelineSourceKind::Episodes => {
                        let canonical = source_ref.strip_prefix("memory.").unwrap_or(source_ref);
                        if prompt_pipeline_episodes_source_uses_unprocessed(source_ref) {
                            return Err(AgentDefinitionError::Validation(format!(
                                "prompt pipeline section `{}` source `{}` uses `unprocessed` selector, which is not supported",
                                section.name, section.source
                            )));
                        }
                        if let Some(SourceRef::Episodes {
                            goal_id: Some(goal_selector),
                            ..
                        }) = SourceRef::parse(canonical)
                        {
                            validate_prompt_pipeline_goal_selector(
                                &section.name,
                                source_ref,
                                &goal_selector,
                            )?;
                        }
                    },
                    PromptPipelineSourceKind::Tier => {
                        let (tier_name, _) = parse_prompt_memory_tier_source(source_ref)
                            .ok_or_else(|| {
                                AgentDefinitionError::Validation(format!(
                                    "prompt pipeline section `{}` source `{}` is invalid or unsupported",
                                    section.name, section.source
                                ))
                            })?;
                        if !tier_names.contains(tier_name) {
                            return Err(AgentDefinitionError::Validation(format!(
                                "prompt pipeline section `{}` source references unknown local tier `{}`",
                                section.name, tier_name
                            )));
                        }
                    },
                    PromptPipelineSourceKind::Tiers => {
                        let Some(SourceRef::Tiers { tier_refs }) = SourceRef::parse(source_ref)
                        else {
                            return Err(AgentDefinitionError::Validation(format!(
                                "prompt pipeline section `{}` source `{}` is invalid or unsupported",
                                section.name, section.source
                            )));
                        };

                        for tier_ref in tier_refs {
                            if let Some(agent) = tier_ref.agent.as_deref() {
                                if has_path_unsafe_chars(agent) {
                                    return Err(AgentDefinitionError::Validation(format!(
                                        "prompt pipeline section `{}` source references agent `{}` with path-unsafe characters",
                                        section.name, agent
                                    )));
                                }
                                if agent != self.agent_id {
                                    return Err(AgentDefinitionError::Validation(format!(
                                        "prompt pipeline section `{}` source references cross-agent tier `{}` which is not supported",
                                        section.name, source_ref
                                    )));
                                }
                            }

                            let tier_name = tier_ref.tier_name.trim();
                            if tier_name.is_empty() {
                                return Err(AgentDefinitionError::Validation(format!(
                                    "prompt pipeline section `{}` source tier references must not be empty",
                                    section.name
                                )));
                            }
                            if !tier_names.contains(tier_name) {
                                return Err(AgentDefinitionError::Validation(format!(
                                    "prompt pipeline section `{}` source references unknown local tier `{}`",
                                    section.name, tier_ref.tier_name
                                )));
                            }
                        }
                    },
                    _ => {},
                }
                if !section_names.insert(section.name.clone()) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "duplicate prompt pipeline section `{}`",
                        section.name
                    )));
                }
            }

            if prompt_pipeline.output_rules.max_context_tokens == 0 {
                return Err(AgentDefinitionError::Validation(
                    "prompt output_rules max_context_tokens must be > 0".to_string(),
                ));
            }

            let mut truncation_seen = HashSet::new();
            for section_name in &prompt_pipeline.output_rules.truncation_priority {
                if !section_names.contains(section_name) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt output_rules truncation_priority references unknown section `{}`",
                        section_name
                    )));
                }
                if !truncation_seen.insert(section_name.clone()) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "prompt output_rules truncation_priority has duplicate entry `{}`",
                        section_name
                    )));
                }
            }

            prompt_pipeline_section_names = Some(section_names);
        }

        for approval_rule in &self.constraints.requires_approval {
            if approval_rule.tool.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "approval rule tool must not be empty".to_string(),
                ));
            }
            if approval_rule.tool != approval_rule.tool.trim() {
                return Err(AgentDefinitionError::Validation(format!(
                    "approval rule tool `{}` must not contain leading or trailing whitespace",
                    approval_rule.tool
                )));
            }
            if approval_rule.tool != "*" && !tool_names.contains(&approval_rule.tool) {
                return Err(AgentDefinitionError::Validation(format!(
                    "approval rule tool `{}` must be `*` or one of declared tools",
                    approval_rule.tool
                )));
            }
            if action_pattern_has_empty_entry(&approval_rule.action) {
                return Err(AgentDefinitionError::Validation(
                    "approval rule action must not be empty".to_string(),
                ));
            }
            if action_pattern_has_whitespace_padding(&approval_rule.action) {
                return Err(AgentDefinitionError::Validation(
                    "approval rule action entries must not contain leading or trailing whitespace"
                        .to_string(),
                ));
            }
            if approval_rule.ttl_secs.is_some_and(|ttl| ttl == 0) {
                return Err(AgentDefinitionError::Validation(
                    "approval rule ttl_secs must be > 0 when provided".to_string(),
                ));
            }
            if let Some(condition) = &approval_rule.when {
                for (param, patterns) in &condition.param_matches {
                    if param.trim().is_empty() || patterns.is_empty() {
                        return Err(AgentDefinitionError::Validation(
                            "approval rule param_matches must have non-empty key and patterns"
                                .to_string(),
                        ));
                    }
                    if patterns.iter().any(|p| p.trim().is_empty()) {
                        return Err(AgentDefinitionError::Validation(
                            "approval rule param_matches patterns must be non-empty".to_string(),
                        ));
                    }
                }
                if condition.url_contains.iter().any(|u| u.trim().is_empty()) {
                    return Err(AgentDefinitionError::Validation(
                        "approval rule url_contains entries must not be empty".to_string(),
                    ));
                }
            }
        }

        for agent_id in &self.delegation_targets {
            if agent_id.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "delegation_targets entries must not be empty".to_string(),
                ));
            }
        }
        if self.invocation_policy.discoverability == AgentDiscoverability::SurfaceOnly
            && self.invocation_policy.allowed_direct_surfaces.is_empty()
        {
            return Err(AgentDefinitionError::Validation(
                "surface_only agents must declare at least one allowed_direct_surface".to_string(),
            ));
        }
        let mut direct_surfaces = HashSet::new();
        for surface in &self.invocation_policy.allowed_direct_surfaces {
            if !direct_surfaces.insert(*surface) {
                return Err(AgentDefinitionError::Validation(format!(
                    "duplicate invocation_policy allowed_direct_surface `{}`",
                    surface.as_str()
                )));
            }
        }
        if self.constraints.max_iterations == 0 {
            return Err(AgentDefinitionError::Validation(
                "constraints.max_iterations must be > 0".to_string(),
            ));
        }
        if self.constraints.max_tokens_per_cycle == 0 {
            return Err(AgentDefinitionError::Validation(
                "constraints.max_tokens_per_cycle must be > 0".to_string(),
            ));
        }
        if self.constraints.max_consecutive_failures == 0 {
            return Err(AgentDefinitionError::Validation(
                "constraints.max_consecutive_failures must be > 0".to_string(),
            ));
        }
        if self.constraints.coordination.delegation_timeout_secs == 0 {
            return Err(AgentDefinitionError::Validation(
                "constraints.coordination.delegation_timeout_secs must be > 0".to_string(),
            ));
        }
        if self.constraints.approval_ttl_secs == 0 {
            return Err(AgentDefinitionError::Validation(
                "constraints.approval_ttl_secs must be > 0".to_string(),
            ));
        }

        if let Some(circuit_breaker) = &self.circuit_breaker {
            if circuit_breaker.thresholds.is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "circuit_breaker thresholds must not be empty".to_string(),
                ));
            }
            let mut thresholds = HashSet::new();
            let mut last_failures = 0_usize;
            for threshold in &circuit_breaker.thresholds {
                if threshold.failures == 0 {
                    return Err(AgentDefinitionError::Validation(
                        "circuit_breaker threshold failures must be > 0".to_string(),
                    ));
                }
                if threshold.failures <= last_failures {
                    return Err(AgentDefinitionError::Validation(
                        "circuit_breaker thresholds must be strictly increasing by failures"
                            .to_string(),
                    ));
                }
                if !thresholds.insert(threshold.failures) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "duplicate circuit_breaker threshold for failures={}",
                        threshold.failures
                    )));
                }
                if threshold
                    .escalation
                    .as_ref()
                    .is_some_and(|e| e.trim().is_empty())
                {
                    return Err(AgentDefinitionError::Validation(format!(
                        "circuit_breaker threshold (failures={}) escalation must not be empty when provided",
                        threshold.failures
                    )));
                }
                if threshold.notify.iter().any(|n| n.trim().is_empty()) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "circuit_breaker threshold (failures={}) notify entries must not be empty",
                        threshold.failures
                    )));
                }
                let mut notify_names = HashSet::new();
                for notify_target in &threshold.notify {
                    let normalized = notify_target.trim();
                    if !notify_names.insert(normalized.to_string()) {
                        return Err(AgentDefinitionError::Validation(format!(
                            "circuit_breaker threshold (failures={}) notify entries must not contain duplicates (`{normalized}`)",
                            threshold.failures
                        )));
                    }
                    if !SUPPORTED_NOTIFICATION_CHANNELS.contains(&normalized) {
                        return Err(AgentDefinitionError::Validation(format!(
                            "unsupported circuit_breaker notify channel `{normalized}`; supported channels are: {}",
                            SUPPORTED_NOTIFICATION_CHANNELS.join(", ")
                        )));
                    }
                }
                last_failures = threshold.failures;
            }

            if let CircuitRecovery::TimeBased { cooldown_hours } = &circuit_breaker.recovery {
                if *cooldown_hours == 0 {
                    return Err(AgentDefinitionError::Validation(
                        "circuit_breaker recovery cooldown_hours must be > 0".to_string(),
                    ));
                }
            }

            for (goal, override_cfg) in &circuit_breaker.per_goal_override {
                let normalized_goal = goal.trim();
                if normalized_goal.is_empty() {
                    return Err(AgentDefinitionError::Validation(
                        "circuit_breaker per_goal_override keys must not be empty".to_string(),
                    ));
                }
                if goal != normalized_goal {
                    return Err(AgentDefinitionError::Validation(format!(
                        "circuit_breaker per_goal_override key `{}` must not contain leading or trailing whitespace",
                        goal
                    )));
                }
                validate_goal_identifier(normalized_goal)?;
                if override_cfg.max_failures == 0 {
                    return Err(AgentDefinitionError::Validation(format!(
                        "circuit_breaker per_goal_override `{goal}` max_failures must be > 0"
                    )));
                }
            }
        }

        let mut feedback_names = HashSet::new();
        for loop_def in &self.feedback_loops {
            if loop_def.name.trim().is_empty()
                || loop_def.trigger.trim().is_empty()
                || loop_def.extract.source.trim().is_empty()
                || loop_def.transform.trim().is_empty()
                || loop_def.inject_into.trim().is_empty()
            {
                return Err(AgentDefinitionError::Validation(
                    "feedback loop name/trigger/source/transform/inject_into must be non-empty"
                        .to_string(),
                ));
            }
            if loop_def
                .extract
                .filter
                .as_ref()
                .is_some_and(|f| f.trim().is_empty())
            {
                return Err(AgentDefinitionError::Validation(format!(
                    "feedback loop `{}` extract.filter must not be empty when provided",
                    loop_def.name
                )));
            }
            if loop_def.extract.fields.iter().any(|f| f.trim().is_empty()) {
                return Err(AgentDefinitionError::Validation(format!(
                    "feedback loop `{}` extract.fields entries must not be empty",
                    loop_def.name
                )));
            }
            if let Some(raw_target) = loop_def.inject_into.trim().strip_prefix("prompt_pipeline.") {
                let section_name = raw_target.trim();
                if section_name.is_empty() {
                    return Err(AgentDefinitionError::Validation(format!(
                        "feedback loop `{}` inject_into must reference a prompt pipeline section name",
                        loop_def.name
                    )));
                }
                let Some(section_names) = prompt_pipeline_section_names.as_ref() else {
                    return Err(AgentDefinitionError::Validation(format!(
                        "feedback loop `{}` inject_into references prompt_pipeline section `{}` but prompt_pipeline is not configured",
                        loop_def.name, section_name
                    )));
                };
                if !section_names.contains(section_name) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "feedback loop `{}` inject_into references unknown prompt pipeline section `{}`",
                        loop_def.name, section_name
                    )));
                }
            }
            if !feedback_names.insert(loop_def.name.clone()) {
                return Err(AgentDefinitionError::Validation(format!(
                    "duplicate feedback loop `{}`",
                    loop_def.name
                )));
            }
        }

        for rule in &self.notification_rules {
            if rule.r#match.trim().is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "notification rule match must not be empty".to_string(),
                ));
            }
            if rule.channels.is_empty() {
                return Err(AgentDefinitionError::Validation(
                    "notification rule must specify at least one channel".to_string(),
                ));
            }
            if rule
                .channels
                .iter()
                .any(|channel| channel.trim().is_empty())
            {
                return Err(AgentDefinitionError::Validation(
                    "notification rule channels must not include empty entries".to_string(),
                ));
            }
            let mut channel_names = HashSet::new();
            for channel in &rule.channels {
                let normalized = channel.trim();
                if !channel_names.insert(normalized.to_string()) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "notification rule channels must not contain duplicates (`{normalized}`)"
                    )));
                }
                if !SUPPORTED_NOTIFICATION_CHANNELS.contains(&normalized) {
                    return Err(AgentDefinitionError::Validation(format!(
                        "unsupported notification rule channel `{normalized}`; supported channels are: {}",
                        SUPPORTED_NOTIFICATION_CHANNELS.join(", ")
                    )));
                }
            }
            if rule.condition.as_ref().is_some_and(|c| c.trim().is_empty()) {
                return Err(AgentDefinitionError::Validation(
                    "notification rule condition must not be empty when provided".to_string(),
                ));
            }
            if rule.message.as_ref().is_some_and(|m| m.trim().is_empty()) {
                return Err(AgentDefinitionError::Validation(
                    "notification rule message must not be empty when provided".to_string(),
                ));
            }
        }

        if let Some(retention) = &self.retention {
            if retention.episodes.default_days == 0 {
                return Err(AgentDefinitionError::Validation(
                    "retention.episodes.default_days must be > 0".to_string(),
                ));
            }
            if retention.episodes.on_failure.is_some_and(|days| days == 0) {
                return Err(AgentDefinitionError::Validation(
                    "retention.episodes.on_failure must be > 0 when provided".to_string(),
                ));
            }
            if retention
                .episodes
                .per_goal_override
                .values()
                .any(|days| *days == 0)
            {
                return Err(AgentDefinitionError::Validation(
                    "retention.episodes.per_goal_override values must be > 0".to_string(),
                ));
            }
            for goal_id in retention.episodes.per_goal_override.keys() {
                let normalized_goal_id = goal_id.trim();
                if normalized_goal_id.is_empty() {
                    return Err(AgentDefinitionError::Validation(
                        "retention.episodes.per_goal_override keys must not be empty".to_string(),
                    ));
                }
                if goal_id != normalized_goal_id {
                    return Err(AgentDefinitionError::Validation(format!(
                        "retention.episodes.per_goal_override key `{}` must not contain leading or trailing whitespace",
                        goal_id
                    )));
                }
                validate_goal_identifier(normalized_goal_id)?;
            }
            if retention.corrections.resolved_days == 0 {
                return Err(AgentDefinitionError::Validation(
                    "retention.corrections.resolved_days must be > 0".to_string(),
                ));
            }
            if retention.corrections.active.is_some_and(|days| days == 0) {
                return Err(AgentDefinitionError::Validation(
                    "retention.corrections.active must be > 0 when provided".to_string(),
                ));
            }
            if retention.definition_versions.keep_last == 0 {
                return Err(AgentDefinitionError::Validation(
                    "retention.definition_versions.keep_last must be > 0".to_string(),
                ));
            }
        }

        if let Some(llm_routing) = &self.llm_routing {
            for endpoint in [
                llm_routing.planning.as_ref(),
                llm_routing.evaluation.as_ref(),
                llm_routing.correction_extraction.as_ref(),
                llm_routing.memory_consolidation.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                let has_profile = endpoint
                    .profile
                    .as_ref()
                    .is_some_and(|p| !p.trim().is_empty());
                let has_provider_model =
                    !endpoint.provider.trim().is_empty() && !endpoint.model.trim().is_empty();
                if !has_profile && !has_provider_model {
                    return Err(AgentDefinitionError::Validation(
                        "llm_routing endpoint must specify either profile or provider+model"
                            .to_string(),
                    ));
                }
            }
        }

        if let Some(strategy) = &self.strategy {
            match strategy {
                StrategyPreference::Fixed(name) if name.trim().is_empty() => {
                    return Err(AgentDefinitionError::Validation(
                        "strategy.fixed value must not be empty".to_string(),
                    ));
                },
                StrategyPreference::Ordered(names) => {
                    if names.is_empty() || names.iter().any(|name| name.trim().is_empty()) {
                        return Err(AgentDefinitionError::Validation(
                            "strategy.ordered must contain non-empty strategy names".to_string(),
                        ));
                    }
                    let mut strategy_names = HashSet::new();
                    for name in names {
                        let normalized = name.trim();
                        if !strategy_names.insert(normalized.to_string()) {
                            return Err(AgentDefinitionError::Validation(format!(
                                "strategy.ordered must not contain duplicates (`{normalized}`)"
                            )));
                        }
                    }
                },
                _ => {},
            }
        }

        Ok(())
    }

    /// Returns the effective set of tools for this agent.
    ///
    /// If `tools` is non-empty, returns its canonicalized names minus both
    /// exclusion and denial rules. If `tools` is empty, applies the same rules
    /// to `all_tools`, except for the exact tool-free Public Envoy shape whose
    /// empty list is a deliberate deny-all contract. Duplicate and
    /// whitespace-only names are removed.
    pub fn resolved_tools(&self, all_tools: &[String]) -> Vec<String> {
        let base: &[String] = if is_strictly_tool_free_public_surface(self) {
            &[]
        } else if self.tools.is_empty() {
            all_tools
        } else {
            &self.tools
        };
        let blocked = |tool: &str| {
            self.excluded_tools
                .iter()
                .chain(self.denied_tools.iter())
                .any(|entry| tool_name_matches_block_entry(tool, entry))
        };
        let mut seen = HashSet::new();
        base.iter()
            .map(|tool| tool.trim())
            .filter(|tool| !tool.is_empty() && !blocked(tool))
            .filter(|tool| seen.insert((*tool).to_string()))
            .map(str::to_string)
            .collect()
    }

    /// Whether one explicitly declared tool may run under this definition.
    ///
    /// This answers only the blocking question — `excluded_tools` and
    /// `denied_tools`, matched exactly as [`AgentDefinition::resolved_tools`]
    /// matches them. Membership in `tools` is deliberately not required: that
    /// list layers named work tools over the auto-injected universal
    /// substrate rather than enumerating every callable capability, so an app
    /// workflow declaring a reviewed dependency must not fail because the
    /// runner did not also list it by name. The two deny-all shapes mirror
    /// [`AgentDefinition::resolved_tools`]: the strictly tool-free public
    /// surface (whose empty list is a deliberate deny-all contract) and an
    /// untrusted definition with no explicit allowlist permit nothing. Other
    /// callers that need the full effective roster keep using
    /// `resolved_tools`.
    pub fn permits_declared_tool(&self, tool: &str) -> bool {
        if is_strictly_tool_free_public_surface(self)
            || (self.trust_level.is_untrusted() && self.tools.is_empty())
        {
            return false;
        }
        let trimmed = tool.trim();
        if trimmed.is_empty() {
            return false;
        }
        !self
            .excluded_tools
            .iter()
            .chain(self.denied_tools.iter())
            .any(|entry| tool_name_matches_block_entry(trimmed, entry))
    }

    /// Returns `true` if this agent is a personal (user-facing) agent.
    pub fn is_personal(&self) -> bool {
        self.kind == AgentKind::Personal
    }

    /// Returns `true` if this agent is a headless worker agent.
    pub fn is_worker(&self) -> bool {
        self.kind == AgentKind::Worker
    }

    /// Returns `true` for built-in system agents such as scheduler/meta-agent.
    pub fn is_system_agent(&self) -> bool {
        is_system_agent_id(&self.agent_id)
    }

    /// Returns `true` when this worker can be surfaced as a normal delegate target.
    pub fn is_runtime_delegate_worker(&self) -> bool {
        self.is_worker() && !self.is_system_agent()
    }
}

fn capability_accepts_vector_requests(tool: &str) -> bool {
    matches!(tool.trim(), "content_search" | "content_read")
}

/// Return policy-visible values for one logical parameter. Unified content
/// tools apply scalar policy to `common` and every request branch; read-candidate
/// canonical URLs are also treated as `url` so switching input shapes cannot
/// bypass a domain restriction.
pub fn effective_tool_parameter_values<'a>(
    tool_name: &str,
    parameters: &'a HashMap<String, Value>,
    parameter_name: &str,
) -> Vec<&'a Value> {
    let parameter_name = parameter_name.trim();
    let mut values = parameters
        .iter()
        .filter_map(|(name, value)| (name.trim() == parameter_name).then_some(value))
        .collect::<Vec<_>>();
    if !capability_accepts_vector_requests(tool_name) {
        return values;
    }
    let include_candidate_urls = tool_name.trim() == "content_read" && parameter_name == "url";
    if include_candidate_urls {
        values.extend(
            parameters
                .iter()
                .find_map(|(name, value)| (name.trim() == "candidate").then_some(value))
                .and_then(Value::as_object)
                .and_then(|candidate| {
                    candidate
                        .get("canonical_url")
                        .or_else(|| candidate.get("url"))
                }),
        );
    }
    if let Some(common) = parameters
        .iter()
        .find_map(|(name, value)| (name.trim() == "common").then_some(value))
        .and_then(Value::as_object)
    {
        values.extend(
            common
                .iter()
                .filter_map(|(name, value)| (name.trim() == parameter_name).then_some(value)),
        );
        if include_candidate_urls {
            values.extend(common.get("candidate").and_then(Value::as_object).and_then(
                |candidate| {
                    candidate
                        .get("canonical_url")
                        .or_else(|| candidate.get("url"))
                },
            ));
        }
    }
    if let Some(requests) = parameters
        .iter()
        .find_map(|(name, value)| (name.trim() == "requests").then_some(value))
        .and_then(Value::as_array)
    {
        for request in requests.iter().filter_map(Value::as_object) {
            values.extend(
                request
                    .iter()
                    .filter_map(|(name, value)| (name.trim() == parameter_name).then_some(value)),
            );
            if include_candidate_urls {
                values.extend(
                    request
                        .get("candidate")
                        .and_then(Value::as_object)
                        .and_then(|candidate| {
                            candidate
                                .get("canonical_url")
                                .or_else(|| candidate.get("url"))
                        }),
                );
            }
        }
    }
    values
}

/// An outward agent is intentionally stricter than the ordinary untrusted-agent
/// contract: it has no provider catalogue at all. Keep this exception narrow so
/// an empty allowlist cannot be reused by a delegatable or ordinary agent.
///
/// Widened 2026-08-18 from an exact `== [PublicEnvoy]` to *every listed surface
/// faces an untrusted audience*. The old form named one surface, so a second
/// outward channel could not reuse the exception without editing this function
/// — the coupling being removed here. The guarantee is unchanged and now rests
/// on the property: the allowlist must be non-empty (so it is a real allowlist,
/// not the permissive default) and must contain nothing owner-facing, which is
/// what stops an ordinary agent claiming a tool-free pass onto owner surfaces.
/// The delegation and discoverability conjuncts are untouched and still carry
/// the rest of the seal.
fn is_strictly_tool_free_public_surface(definition: &AgentDefinition) -> bool {
    let surfaces = &definition.invocation_policy.allowed_direct_surfaces;
    definition.kind == AgentKind::Personal
        && definition.tools.is_empty()
        && definition.delegation_targets.is_empty()
        && definition.invocation_policy.discoverability == AgentDiscoverability::SurfaceOnly
        && definition.invocation_policy.delegation == AgentDelegationPolicy::None
        && !surfaces.is_empty()
        && surfaces
            .iter()
            .all(|surface| surface.audience() == SurfaceAudience::Untrusted)
}

/// Validate an `AgentDefinition` against kind-specific constraints.
///
/// Returns `Ok(())` if all checks pass, or `Err` with a list of validation
/// error strings describing every violated constraint.
pub fn validate_agent_definition(def: &AgentDefinition) -> Result<(), Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    if def.trust_level.is_untrusted()
        && def.tools.is_empty()
        && !is_strictly_tool_free_public_surface(def)
    {
        errors.push(
            "Untrusted agents must declare an explicit non-empty tools allowlist".to_string(),
        );
    }

    match def.kind {
        AgentKind::Worker => {
            let has_principal = def
                .principal
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            let has_workspace = def
                .workspace
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            if has_principal != has_workspace {
                errors.push(
                    "Worker agents must declare both principal and workspace together".to_string(),
                );
            }
            // Workers must NOT have autonomous_config.
            if def.autonomous_config.is_some() {
                errors.push("Worker agents must not have autonomous_config".to_string());
            }
            if def.harness.is_some() {
                errors.push("Worker agents must not have harness".to_string());
            }
            // Workers must have explicit tools (not empty, no lone wildcard).
            if def.tools.is_empty() {
                errors.push("Worker agents must have explicit tools (non-empty)".to_string());
            } else if def.tools.len() == 1 && def.tools[0] == "*" {
                errors.push(
                    "Worker agents must have explicit tools (wildcard \"*\" alone is not allowed)"
                        .to_string(),
                );
            }
            // Workers cannot be primary.
            if def.is_primary {
                errors.push("Worker agents must not set is_primary to true".to_string());
            }
            // Workers cannot have readable_agents.
            if !def.readable_agents.is_empty() {
                errors.push("Worker agents must not have readable_agents".to_string());
            }
        },
        AgentKind::Personal => {
            let has_principal = def
                .principal
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            let has_workspace = def
                .workspace
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            if has_principal != has_workspace {
                errors.push(
                    "Personal agents must declare both principal and workspace together"
                        .to_string(),
                );
            }
            // Personal agents may have autonomous_config; validate it if present.
            if let Some(ac) = &def.autonomous_config {
                if ac.focus_areas.is_empty() {
                    errors.push("autonomous_config must have at least one focus_area".to_string());
                }
                // Basic cron validation: at least 5 space-separated fields.
                let fields: Vec<&str> = ac.schedule.split_whitespace().collect();
                if fields.len() < 5 {
                    errors.push(format!(
                        "autonomous_config schedule \"{}\" is not a valid cron expression (expected at least 5 space-separated fields)",
                        ac.schedule
                    ));
                }
                // Validate per-area schedule overrides.
                let mut seen_focus_area_goal_slugs = HashSet::new();
                for area in &ac.focus_areas {
                    if area.name.trim().is_empty() {
                        errors.push("focus_area name must not be empty".to_string());
                    }
                    if area.description.trim().is_empty() {
                        errors.push(format!(
                            "focus_area \"{}\" description must not be empty",
                            area.name
                        ));
                    }
                    if let Some(ref sched) = area.schedule {
                        let fields: Vec<&str> = sched.split_whitespace().collect();
                        if fields.len() < 5 {
                            errors.push(format!(
                                "focus_area \"{}\" schedule \"{}\" is not a valid cron expression \
                                 (expected at least 5 space-separated fields)",
                                area.name, sched
                            ));
                        }
                    }
                    if let Some(program) = area.program.as_deref() {
                        if program.trim().is_empty() {
                            errors.push(format!(
                                "focus_area \"{}\" program must not be empty when provided",
                                area.name
                            ));
                        }
                    }
                    if let Some(scope) = area.scope.as_ref() {
                        if scope.is_empty() {
                            errors.push(format!(
                                "focus_area \"{}\" scope must not be empty when provided",
                                area.name
                            ));
                        }
                        for scope_entry in scope {
                            if scope_entry.trim().is_empty() {
                                errors.push(format!(
                                    "focus_area \"{}\" scope entries must not be empty",
                                    area.name
                                ));
                            }
                        }
                    }
                    let goal_slug = area.goal_slug();
                    if !seen_focus_area_goal_slugs.insert(goal_slug.clone()) {
                        errors.push(format!(
                            "focus_area \"{}\" collides with another focus area goal slug \"{}\"",
                            area.name, goal_slug
                        ));
                    }
                }
            }
            if let Some(harness) = &def.harness {
                if harness
                    .program_section
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
                {
                    errors.push(
                        "harness program_section must not be empty when provided".to_string(),
                    );
                }
            }
            // is_primary is allowed for Personal agents — no check needed.
        },
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Indicates what originated a goal cycle — used for observability and routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalSource {
    #[default]
    User,
    Schedule,
    /// Inline delegation from a chat session. This creates an ephemeral
    /// task-backed execution in the caller's scope.
    ChatInline,
}

/// Controls whether an agent can be invoked inline from chat via `delegate_to_agent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatInlinePolicy {
    /// Not available for inline delegation.
    Off,
    /// Auto-execute without user confirmation (good for cheap, read-only agents).
    Auto,
    /// Requires user confirmation before the agent runs.
    Confirm,
}

// GoalDefinition, TriggerDefinition, TriggerKind, CronTrigger, EventTrigger, IdleTrigger
// removed — goals and triggers moved to Task in the unified architecture.
// Scheduling is now on Task.schedule (TaskSchedule in task_models.rs).

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConstraints {
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default = "default_max_tokens_per_cycle")]
    pub max_tokens_per_cycle: u64,
    #[serde(default = "default_max_consecutive_failures")]
    pub max_consecutive_failures: usize,
    #[serde(default)]
    pub requires_approval: Vec<ApprovalRule>,
    #[serde(default = "default_approval_ttl_secs")]
    pub approval_ttl_secs: u64,
    #[serde(default)]
    pub coordination: CoordinationConfig,
    #[serde(default)]
    pub allow_self_modification: bool,
    /// Maximum wall-clock duration (seconds) for a single goal pipeline execution.
    /// Overrides the global `GOAL_PIPELINE_TIMEOUT_SECS` (10800s) when set.
    #[serde(default)]
    pub max_duration_secs: Option<u64>,
}

fn default_max_iterations() -> u32 {
    4000
}

fn default_max_tokens_per_cycle() -> u64 {
    20_000_000
}

fn default_max_consecutive_failures() -> usize {
    3
}

fn default_approval_ttl_secs() -> u64 {
    86_400
}

impl Default for AgentConstraints {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            max_tokens_per_cycle: default_max_tokens_per_cycle(),
            max_consecutive_failures: default_max_consecutive_failures(),
            requires_approval: Vec::new(),
            approval_ttl_secs: default_approval_ttl_secs(),
            coordination: CoordinationConfig::default(),
            allow_self_modification: false,
            max_duration_secs: None,
        }
    }
}

/// Goal priority is a string newtype for declarative YAML ergonomics.
///
/// Semantic ordering: critical > high > medium > low.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalPriority(pub String);

impl GoalPriority {
    pub const CRITICAL: &str = "critical";
    pub const HIGH: &str = "high";
    pub const MEDIUM: &str = "medium";
    pub const LOW: &str = "low";

    fn normalized_level(&self) -> String {
        self.0.trim().to_ascii_lowercase()
    }

    fn is_known_level(level: &str) -> bool {
        matches!(
            level,
            Self::CRITICAL | Self::HIGH | Self::MEDIUM | Self::LOW
        )
    }

    pub fn is_known(&self) -> bool {
        Self::is_known_level(&self.normalized_level())
    }

    /// Returns a numeric rank for semantic ordering (higher = more important).
    fn rank(&self) -> u8 {
        let normalized = self.normalized_level();
        match normalized.as_str() {
            Self::CRITICAL => 4,
            Self::HIGH => 3,
            Self::MEDIUM => 2,
            Self::LOW => 1,
            _ => 0, // unknown levels sort below low
        }
    }
}

impl PartialEq for GoalPriority {
    fn eq(&self, other: &Self) -> bool {
        self.normalized_level() == other.normalized_level()
    }
}

impl Eq for GoalPriority {}

impl PartialOrd for GoalPriority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GoalPriority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank()
            .cmp(&other.rank())
            .then_with(|| self.normalized_level().cmp(&other.normalized_level()))
    }
}

impl Default for GoalPriority {
    fn default() -> Self {
        Self(Self::MEDIUM.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRule {
    pub tool: String,
    pub action: ActionPattern,
    #[serde(default)]
    pub when: Option<ApprovalCondition>,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalCondition {
    #[serde(default)]
    pub param_matches: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub url_contains: Vec<String>,
}

// ── Declarative configuration types ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptPipelineConfig {
    #[serde(default)]
    pub sections: Vec<PromptSection>,
    #[serde(default = "default_output_rules")]
    pub output_rules: PromptOutputRules,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSection {
    pub name: String,
    /// Source reference for dynamic content (e.g. "definition.persona", "memory.episodes(...)").
    /// Mutually exclusive with `content`.
    #[serde(default)]
    pub source: String,
    /// Inline text content authored directly in the agent YAML.
    /// Mutually exclusive with `source`.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptOutputRules {
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: u32,
    #[serde(default)]
    pub truncation_priority: Vec<String>,
}

fn default_max_context_tokens() -> u32 {
    4000
}

fn default_output_rules() -> PromptOutputRules {
    PromptOutputRules {
        max_context_tokens: default_max_context_tokens(),
        truncation_priority: Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EvaluationCriterion {
    StructuredOutput {
        required_fields: Vec<String>,
        #[serde(default)]
        min_count: Option<CountConstraint>,
    },
    NoError,
    UnderBudget {
        max_actions: u32,
    },
    MemoryUpdated {
        key: String,
    },
}

impl<'de> Deserialize<'de> for EvaluationCriterion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Debug, Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum RawEvaluationCriterion {
            StructuredOutput {
                required_fields: Vec<String>,
                #[serde(default)]
                min_count: Option<CountConstraint>,
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            NoError {
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            UnderBudget {
                max_actions: u32,
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
            MemoryUpdated {
                key: String,
                #[serde(flatten)]
                extra: HashMap<String, Value>,
            },
        }

        fn reject_unknown_fields<E>(variant: &str, extra: HashMap<String, Value>) -> Result<(), E>
        where
            E: serde::de::Error,
        {
            if extra.is_empty() {
                return Ok(());
            }

            let mut keys = extra.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            Err(E::custom(format!(
                "unknown field(s) for evaluation criterion `{variant}`: {}",
                keys.join(", ")
            )))
        }

        match RawEvaluationCriterion::deserialize(deserializer)? {
            RawEvaluationCriterion::StructuredOutput {
                required_fields,
                min_count,
                extra,
            } => {
                reject_unknown_fields::<D::Error>("structured_output", extra)?;
                Ok(Self::StructuredOutput {
                    required_fields,
                    min_count,
                })
            },
            RawEvaluationCriterion::NoError { extra } => {
                reject_unknown_fields::<D::Error>("no_error", extra)?;
                Ok(Self::NoError)
            },
            RawEvaluationCriterion::UnderBudget { max_actions, extra } => {
                reject_unknown_fields::<D::Error>("under_budget", extra)?;
                Ok(Self::UnderBudget { max_actions })
            },
            RawEvaluationCriterion::MemoryUpdated { key, extra } => {
                reject_unknown_fields::<D::Error>("memory_updated", extra)?;
                Ok(Self::MemoryUpdated { key })
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountConstraint {
    pub field: String,
    pub value: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerPolicy {
    #[serde(default = "default_circuit_breaker_thresholds")]
    pub thresholds: Vec<CircuitBreakerThreshold>,
    #[serde(default)]
    pub recovery: CircuitRecovery,
    #[serde(default)]
    pub per_goal_override: HashMap<String, CircuitBreakerOverride>,
    /// Number of consecutive idle-goal failures before suppressing further idle attempts.
    #[serde(default = "default_idle_failure_threshold")]
    pub idle_failure_threshold: u32,
    /// How long (minutes) the idle circuit stays open before auto-resetting.
    #[serde(default = "default_idle_open_duration_minutes")]
    pub idle_open_duration_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerThreshold {
    pub failures: usize,
    pub action: CircuitAction,
    #[serde(default)]
    pub escalation: Option<String>,
    #[serde(default)]
    pub notify: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitAction {
    InjectFailureContext,
    OpenCircuit,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "trigger", rename_all = "snake_case")]
#[derive(Default)]
pub enum CircuitRecovery {
    #[default]
    UserReset,
    TimeBased {
        cooldown_hours: u32,
    },
}

impl<'de> Deserialize<'de> for CircuitRecovery {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawCircuitRecovery {
            trigger: String,
            #[serde(default)]
            cooldown_hours: Option<u32>,
        }

        let raw = RawCircuitRecovery::deserialize(deserializer)?;
        match raw.trigger.as_str() {
            "user_reset" => {
                if raw.cooldown_hours.is_some() {
                    return Err(serde::de::Error::custom(
                        "circuit recovery `user_reset` does not accept `cooldown_hours`",
                    ));
                }
                Ok(Self::UserReset)
            },
            "time_based" => Ok(Self::TimeBased {
                cooldown_hours: raw.cooldown_hours.ok_or_else(|| {
                    serde::de::Error::custom(
                        "circuit recovery `time_based` requires `cooldown_hours`",
                    )
                })?,
            }),
            other => Err(serde::de::Error::custom(format!(
                "circuit recovery trigger must be `user_reset` or `time_based`, got `{other}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerOverride {
    pub max_failures: usize,
}

fn default_idle_failure_threshold() -> u32 {
    3
}
fn default_idle_open_duration_minutes() -> u32 {
    60
}

impl Default for CircuitBreakerPolicy {
    fn default() -> Self {
        Self {
            thresholds: default_circuit_breaker_thresholds(),
            recovery: CircuitRecovery::default(),
            per_goal_override: HashMap::new(),
            idle_failure_threshold: default_idle_failure_threshold(),
            idle_open_duration_minutes: default_idle_open_duration_minutes(),
        }
    }
}

fn default_circuit_breaker_thresholds() -> Vec<CircuitBreakerThreshold> {
    vec![
        CircuitBreakerThreshold {
            failures: 1,
            action: CircuitAction::InjectFailureContext,
            escalation: None,
            notify: Vec::new(),
        },
        CircuitBreakerThreshold {
            failures: 3,
            action: CircuitAction::OpenCircuit,
            escalation: None,
            notify: vec!["chat".to_string()],
        },
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackLoopDefinition {
    pub name: String,
    pub trigger: String,
    pub extract: FeedbackExtract,
    pub transform: String,
    pub inject_into: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackExtract {
    pub source: String,
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub fields: Vec<String>,
}

impl FeedbackLoopDefinition {
    /// Returns the two built-in feedback loops (`failure_adaptation`, `success_reinforcement`).
    ///
    /// These are declarative definitions only. The Phase 3 interpreter is responsible for
    /// wiring them into the prompt pipeline at runtime.
    pub fn defaults() -> Vec<Self> {
        vec![
            Self {
                name: "failure_adaptation".to_string(),
                trigger: "episode.outcome.is_failed".to_string(),
                extract: FeedbackExtract {
                    source: "episodes(goal_id, limit=5)".to_string(),
                    filter: Some("outcome.is_failed".to_string()),
                    fields: Vec::new(),
                },
                transform: "failure_context".to_string(),
                inject_into: "prompt_pipeline.failure_context".to_string(),
            },
            Self {
                name: "success_reinforcement".to_string(),
                trigger: "episode.outcome.is_succeeded".to_string(),
                extract: FeedbackExtract {
                    source: "episodes(goal_id, limit=10)".to_string(),
                    filter: Some("outcome.is_succeeded".to_string()),
                    fields: Vec::new(),
                },
                transform: "success_patterns".to_string(),
                inject_into: "prompt_pipeline.success_patterns".to_string(),
            },
            Self {
                name: "strategy_effectiveness".to_string(),
                trigger: "episode.outcome.is_completed".to_string(),
                extract: FeedbackExtract {
                    source: "strategy_summary".to_string(),
                    filter: None,
                    fields: Vec::new(),
                },
                transform: "record_strategy".to_string(),
                inject_into: "strategy_context".to_string(),
            },
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationRule {
    pub r#match: String,
    pub severity: NotificationSeverity,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

const SUPPORTED_NOTIFICATION_CHANNELS: &[&str] = &["chat", "webhook", "agent_memory"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct RetentionPolicy {
    #[serde(default)]
    pub episodes: EpisodeRetention,
    #[serde(default)]
    pub corrections: CorrectionRetention,
    #[serde(default)]
    pub definition_versions: VersionRetention,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeRetention {
    #[serde(default = "default_episode_retention_days")]
    pub default_days: u32,
    #[serde(default)]
    pub on_failure: Option<u32>,
    #[serde(default)]
    pub per_goal_override: HashMap<String, u32>,
    #[serde(default)]
    pub consolidate_before_delete: bool,
}

fn default_episode_retention_days() -> u32 {
    90
}

impl Default for EpisodeRetention {
    fn default() -> Self {
        Self {
            default_days: default_episode_retention_days(),
            on_failure: None,
            per_goal_override: HashMap::new(),
            consolidate_before_delete: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionRetention {
    #[serde(default = "default_correction_resolved_days")]
    pub resolved_days: u32,
    /// None means "forever".
    #[serde(
        default,
        deserialize_with = "deserialize_forever_or_days",
        serialize_with = "serialize_forever_or_days"
    )]
    pub active: Option<u32>,
}

fn default_correction_resolved_days() -> u32 {
    30
}

impl Default for CorrectionRetention {
    fn default() -> Self {
        Self {
            resolved_days: default_correction_resolved_days(),
            active: None,
        }
    }
}

/// Custom deserializer: "forever" -> None, integer -> Some(days).
fn deserialize_forever_or_days<'de, D>(d: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    let value = serde_json::Value::deserialize(d)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) if s == "forever" => Ok(None),
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .and_then(|v| (v > 0).then_some(v))
            .map(Some)
            .ok_or_else(|| de::Error::custom("active must be `forever` or a positive integer")),
        _ => Err(de::Error::custom(
            "active must be `forever` or a positive integer",
        )),
    }
}

fn serialize_forever_or_days<S>(value: &Option<u32>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(days) => serializer.serialize_u32(*days),
        None => serializer.serialize_str("forever"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionRetention {
    #[serde(default = "default_versions_keep_last")]
    pub keep_last: u32,
}

fn default_versions_keep_last() -> u32 {
    10
}

impl Default for VersionRetention {
    fn default() -> Self {
        Self {
            keep_last: default_versions_keep_last(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmRoutingConfig {
    #[serde(default)]
    pub planning: Option<LlmEndpoint>,
    #[serde(default)]
    pub evaluation: Option<LlmEndpoint>,
    #[serde(default)]
    pub correction_extraction: Option<LlmEndpoint>,
    #[serde(default)]
    pub memory_consolidation: Option<LlmEndpoint>,
    /// Per-operation overrides keyed by operation name as used in
    /// `magician-config.yaml`'s `operation_mapping` (e.g. `primitive`,
    /// `agentic_decision`, `atomic_composition`). Takes precedence over
    /// the named lanes above for matching operations. Lets an agent
    /// bump *just* the operations that need it (e.g. high reasoning
    /// on the inner loop) without dragging cheap operations along.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub operations: std::collections::BTreeMap<String, LlmEndpoint>,
    /// The coding profile (a `coding.profiles[].id` from magician-config, e.g. `coding-premium`
    /// for GPT-5.6 Sol) this agent's `run_coding_task` (Pi) runs under when the tool call does NOT
    /// pass an explicit `coding_profile` arg. Makes per-agent coding-model tiering declarative +
    /// deterministic (principal/architect → premium, others → a cheaper/other profile) instead of
    /// relying on the prompt to emit the arg. `None` → the global `coding.default_profile`.
    /// NB: unlike the `LlmEndpoint` lanes above (which route the agent's OWN reasoning loop), this
    /// selects the Pi *coding* model that actually writes code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_profile: Option<String>,
}

impl LlmRoutingConfig {
    /// Every profile this routing names, labelled by where it is set
    /// (`planning`, `operations.agentic_decision`, `coding_profile`, …).
    pub fn profile_references(&self) -> Vec<(String, String)> {
        let mut references = Vec::new();
        for (lane, endpoint) in [
            ("planning", &self.planning),
            ("evaluation", &self.evaluation),
            ("correction_extraction", &self.correction_extraction),
            ("memory_consolidation", &self.memory_consolidation),
        ] {
            if let Some(profile) = endpoint.as_ref().and_then(|e| e.profile.as_deref()) {
                references.push((lane.to_string(), profile.trim().to_string()));
            }
        }
        for (operation, endpoint) in &self.operations {
            if let Some(profile) = endpoint.profile.as_deref() {
                references.push((
                    format!("operations.{operation}"),
                    profile.trim().to_string(),
                ));
            }
        }
        if let Some(coding) = self.coding_profile.as_deref() {
            references.push(("coding_profile".to_string(), coding.trim().to_string()));
        }
        references.retain(|(_, profile)| !profile.is_empty());
        references
    }
}

/// Profile names `next` introduces that the config does not define, as
/// `location: name`. Names `previous` already carried are exempt: a stale pin
/// that a write leaves untouched is surfaced elsewhere and must not block an
/// unrelated edit. LLM lanes resolve against `known_profiles` (router
/// profiles); `coding_profile` against `known_coding_profiles`.
pub fn unknown_llm_routing_references(
    next: Option<&LlmRoutingConfig>,
    previous: Option<&LlmRoutingConfig>,
    known_profiles: &std::collections::HashSet<String>,
    known_coding_profiles: &std::collections::HashSet<String>,
) -> Vec<String> {
    let Some(next) = next else {
        return Vec::new();
    };
    let carried: std::collections::HashSet<(String, String)> = previous
        .map(|previous| previous.profile_references().into_iter().collect())
        .unwrap_or_default();
    next.profile_references()
        .into_iter()
        .filter(|reference| !carried.contains(reference))
        .filter(|(location, profile)| {
            if location == "coding_profile" {
                !known_coding_profiles.contains(profile)
            } else {
                !known_profiles.contains(profile)
            }
        })
        .map(|(location, profile)| format!("{location}: {profile}"))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmEndpoint {
    /// Reference to an LLM profile defined in magician-config.yaml
    /// (e.g., "opus47-messages-toolsany-rnone").
    /// When set, provider/model are ignored — the profile provides all config
    /// (model, reasoning effort, timeout, metadata, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Direct provider override (used when profile is not set).
    #[serde(default)]
    pub provider: String,
    /// Direct model override (used when profile is not set).
    #[serde(default)]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustPolicy {
    pub level: String,
    #[serde(default)]
    pub allow: Vec<ToolActionPattern>,
    #[serde(default)]
    pub deny: Vec<ToolActionPattern>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolActionPattern {
    pub tool: String,
    pub action: ActionPattern,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ActionPattern {
    Single(String),
    Multiple(Vec<String>),
}

impl ActionPattern {
    pub fn matches(&self, action: &str) -> bool {
        match self {
            Self::Single(s) => s == "*" || s == action,
            Self::Multiple(values) => values.iter().any(|v| v == "*" || v == action),
        }
    }
}

impl ToolActionPattern {
    pub fn matches(&self, tool: &str, action: &str) -> bool {
        let tool_match = self.tool == "*" || self.tool.eq_ignore_ascii_case(tool);
        tool_match && self.action.matches(action)
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyPreference {
    Fixed(String),
    Ordered(Vec<String>),
    AutoSelect,
}

impl Default for StrategyPreference {
    fn default() -> Self {
        Self::Fixed("atomic_composition".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    pub name: String,
    pub adapter: String,
    #[serde(default)]
    pub settings: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

// ── Coordination + workflow + artifacts (types only in Phase 2) ───────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationConfig {
    /// Maximum depth of transitive delegation chains.
    #[serde(default = "default_max_delegation_depth")]
    pub max_delegation_depth: u8,
    /// Legacy compatibility value retained in serialized agent definitions.
    /// Delegated active-work budgets are resolved from the call's explicit
    /// `timeout_secs` / `depth` and are not clamped by this field.
    #[serde(default = "default_delegation_timeout")]
    pub delegation_timeout_secs: u64,
    /// Whether this agent permits transitive delegation (sub-delegation).
    /// When false (default), an agent that is already a delegation target
    /// cannot further delegate to other agents. Leaf workers should keep
    /// this false; coordinators that need to sub-delegate set it to true.
    #[serde(default)]
    pub allow_transitive_delegation: bool,
}

fn default_max_delegation_depth() -> u8 {
    2
}

fn default_delegation_timeout() -> u64 {
    300
}

impl Default for CoordinationConfig {
    fn default() -> Self {
        Self {
            max_delegation_depth: default_max_delegation_depth(),
            delegation_timeout_secs: default_delegation_timeout(),
            allow_transitive_delegation: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierRef {
    #[serde(default)]
    pub agent: Option<String>,
    pub tier_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDeclaration {
    pub name: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    #[serde(default)]
    pub source: Option<String>,
    /// MIME type hint for the artifact content (e.g., "application/json", "text/plain").
    /// Helps downstream steps interpret the content correctly.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Optional JSON Schema for validating the artifact content before storing.
    /// When present, artifacts are validated against this schema; when absent,
    /// artifacts pass through without validation.
    #[serde(default)]
    pub schema: Option<Value>,
    /// Optional render hints for auto-surface publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_hints: Option<crate::magician_v2::artifacts::types::RenderHints>,
    /// Advisory declaration: participates in artifact enrichment / render-hint
    /// copying (`enrich_artifacts_from_declarations`) but MUST NOT gate the
    /// deterministic refinement pass. Set for the generic
    /// `default_expected_artifact_declarations` (render/enrich scaffolding the
    /// loop is not contracted to emit). Caller-supplied, hard-required outputs
    /// leave this `false` and continue to gate refinement.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enrichment_only: bool,
}

impl ArtifactDeclaration {
    /// Create a minimal declaration where `name` and `artifact_type` are both
    /// set to the given snake_case token (e.g. `"plan_graph"`).
    pub fn simple(name_and_type: &str) -> Self {
        Self {
            name: name_and_type.to_string(),
            artifact_type: name_and_type.to_string(),
            source: None,
            content_type: None,
            schema: None,
            render_hints: None,
            enrichment_only: false,
        }
    }

    /// Attach render hints to this declaration.
    pub fn with_render_hints(
        mut self,
        hints: crate::magician_v2::artifacts::types::RenderHints,
    ) -> Self {
        self.render_hints = Some(hints);
        self
    }

    /// Set the MIME content-type hint (e.g. `"text/markdown"`) for this
    /// declaration. The refinement gate matches declarations by `name`, so this
    /// is advisory metadata used by enrich/render.
    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepArtifact {
    pub name: String,
    pub artifact_type: String,
    pub content: Value,
    pub provenance: ArtifactProvenance,
    /// MIME type hint copied from the ArtifactDeclaration (when available).
    #[serde(default)]
    pub content_type: Option<String>,
    /// Optional render hints for auto-surface publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_hints: Option<crate::magician_v2::artifacts::types::RenderHints>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactProvenance {
    #[serde(default)]
    pub workflow_instance_id: Option<String>,
    #[serde(default)]
    pub step_name: Option<String>,
    pub agent_id: String,
    #[serde(default)]
    pub source_agent_ids: Vec<String>,
    pub produced_at: DateTime<Utc>,
}

/// Trust level as string newtype. Unknown levels resolve via declarative policies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TrustLevel(pub String);

/// Return the first per-tool parameter-prefix deny match, using the exact
/// coercion semantics shared by autonomous and chat dispatch. Tool/parameter
/// keys are canonicalized for configuration whitespace; unified content
/// requests are inspected branch by branch. Prefix matching stays
/// case-insensitive and preserves the configured prefix for diagnostics.
pub fn denied_tool_parameter_match(
    rules: &HashMap<String, HashMap<String, Vec<String>>>,
    tool_name: &str,
    parameters: &HashMap<String, Value>,
) -> Option<(String, String, String)> {
    // Iterate over every canonical match instead of taking the first HashMap
    // entry. This makes differently padded duplicate configuration keys a
    // conservative union rather than a nondeterministic deny-rule bypass.
    for (configured_tool, parameter_rules) in rules {
        if configured_tool.trim() != tool_name.trim() {
            continue;
        }
        for (configured_parameter, denied_prefixes) in parameter_rules {
            let parameter_name = configured_parameter.trim();
            for value in effective_tool_parameter_values(tool_name, parameters, parameter_name) {
                let parameter_value = match value {
                    Value::String(value) => value.clone(),
                    Value::Number(value) => value.to_string(),
                    Value::Bool(value) => value.to_string(),
                    _ => continue,
                };
                let normalized_value = parameter_value.trim().to_ascii_lowercase();
                if let Some(prefix) = denied_prefixes
                    .iter()
                    .find(|prefix| normalized_value.starts_with(&prefix.to_ascii_lowercase()))
                {
                    return Some((parameter_name.to_string(), parameter_value, prefix.clone()));
                }
            }
        }
    }
    None
}

impl TrustLevel {
    pub const BUILTIN: &str = "builtin";
    pub const LOCAL: &str = "local";
    pub const REVIEWED: &str = "reviewed";
    pub const UNTRUSTED: &str = "untrusted";
    const LEGACY_STANDARD: &str = "standard";

    pub fn canonicalized_value(raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case(Self::LEGACY_STANDARD) {
            Self::LOCAL.to_string()
        } else {
            trimmed.to_string()
        }
    }

    pub fn canonicalized(&self) -> Self {
        Self(Self::canonicalized_value(&self.0))
    }

    pub fn is_untrusted(&self) -> bool {
        Self::canonicalized_value(&self.0).eq_ignore_ascii_case(Self::UNTRUSTED)
    }

    /// Whether this is a trust level the runtime recognizes —
    /// `builtin` / `local` / `reviewed` / `untrusted` (legacy `standard` → `local`).
    /// An UNrecognized level (e.g. a typo like `high`) matches no trust policy
    /// entry, so `is_action_allowed` returns false for everything and the agent
    /// silently denies every action. Callers validate against this to fail loud
    /// at load instead. Matching is case-insensitive (policy lookup is too).
    pub fn is_recognized(&self) -> bool {
        let canonical = Self::canonicalized_value(&self.0).to_ascii_lowercase();
        canonical == Self::BUILTIN
            || canonical == Self::LOCAL
            || canonical == Self::REVIEWED
            || canonical == Self::UNTRUSTED
    }

    pub fn is_action_allowed(&self, tool: &str, action: &str, policies: &[TrustPolicy]) -> bool {
        let canonical_level = self.canonicalized();
        let requested_level = canonical_level.0.as_str();
        let Some(policy) = policies
            .iter()
            .find(|p| p.level.trim().eq_ignore_ascii_case(requested_level))
        else {
            return false;
        };

        if policy.deny.iter().any(|p| p.matches(tool, action)) {
            return false;
        }

        policy.allow.iter().any(|p| p.matches(tool, action))
    }
}

impl Default for TrustLevel {
    fn default() -> Self {
        Self(Self::LOCAL.to_string())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {

    fn endpoint(profile: &str) -> LlmEndpoint {
        LlmEndpoint {
            profile: Some(profile.to_string()),
            provider: String::new(),
            model: String::new(),
        }
    }

    fn routing() -> LlmRoutingConfig {
        LlmRoutingConfig {
            planning: None,
            evaluation: None,
            correction_extraction: None,
            memory_consolidation: None,
            operations: Default::default(),
            coding_profile: None,
        }
    }

    /// A pin names a router profile or a coding profile that must exist when
    /// it is written: a missing name fails every run that reaches it, far from
    /// the edit that caused it. Only names a write INTRODUCES are checked, so
    /// renaming a profile in config cannot lock an agent out of unrelated edits.
    #[test]
    fn a_pin_must_name_an_existing_profile_but_an_untouched_stale_pin_does_not_block() {
        let known: std::collections::HashSet<String> =
            ["gpt6sol-a".to_string(), "grok47-b".to_string()].into();
        let coding: std::collections::HashSet<String> = ["coding-premium".to_string()].into();

        let mut next = routing();
        next.planning = Some(endpoint("gpt6sol-a"));
        next.evaluation = Some(endpoint("gone-profile"));
        next.operations
            .insert("agentic_decision".to_string(), endpoint("also-gone"));
        next.coding_profile = Some("coding-missing".to_string());
        let mut unknown = unknown_llm_routing_references(Some(&next), None, &known, &coding);
        unknown.sort();
        assert_eq!(
            unknown,
            vec![
                "coding_profile: coding-missing".to_string(),
                "evaluation: gone-profile".to_string(),
                "operations.agentic_decision: also-gone".to_string(),
            ]
        );

        let mut previous = routing();
        previous.evaluation = Some(endpoint("gone-profile"));
        let mut kept = routing();
        kept.evaluation = Some(endpoint("gone-profile"));
        kept.planning = Some(endpoint("grok47-b"));
        assert!(
            unknown_llm_routing_references(Some(&kept), Some(&previous), &known, &coding)
                .is_empty(),
            "a stale pin the write leaves as it was must not block the write"
        );
        assert!(unknown_llm_routing_references(None, Some(&previous), &known, &coding).is_empty());
    }

    use std::collections::BTreeMap;

    use super::*;
    use crate::magician_v2::agents::memory_tiers::{
        BuiltinTransform, ConsolidationTrigger, RenderConfig, RetentionMode, TierFieldSchema,
        TierScope,
    };

    #[allow(deprecated)]
    fn valid_definition() -> AgentDefinition {
        AgentDefinition {
            // Empty: every transport. The restriction is opt-in.
            browser_transports: Vec::new(),
            agent_id: "a1".to_string(),
            version: 1,
            name: "A1".to_string(),
            aliases: Vec::new(),
            wake_spellings: Vec::new(),
            description: "desc".to_string(),
            app_tool: None,
            persona: "P".to_string(),
            kind: AgentKind::Personal,
            disabled: false,
            tools: vec!["browser".to_string()],
            excluded_tools: Vec::new(),
            denied_tools: Vec::new(),
            denied_tool_params: HashMap::new(),
            constraints: AgentConstraints::default(),
            trust_level: TrustLevel::default(),
            memory_tiers: vec![MemoryTierDefinition {
                name: "task_progress".to_string(),
                scope: TierScope::AgentGoal,
                description: "Task tracking".to_string(),
                schema: BTreeMap::from([(
                    String::from("context_summary"),
                    TierFieldSchema::Text {},
                )]),
                render: RenderConfig {
                    format: "compact_summary".to_string(),
                    template: "{context_summary}".to_string(),
                },
                retention: RetentionMode::GoalLifetime,
            }],
            memory_consolidation: Vec::new(),
            prompt_pipeline: None,
            circuit_breaker: None,
            feedback_loops: Vec::new(),
            notification_rules: Vec::new(),
            retention: None,
            llm_routing: None,
            strategy: None,
            state_machines: HashMap::new(),
            principal: None,
            workspace: None,
            autonomous_config: None,
            harness: None,
            is_primary: false,
            onboarding_completed: false,
            readable_agents: Vec::new(),
            default_personality: None,
            user_memory_isolation: UserMemoryIsolation::Shared,
            delegation_targets: default_delegation_targets(),
            invocation_policy: AgentInvocationPolicy::default(),
            auto_surface_policy: None,
            chat_inline: None,
            social_persona: None,
        }
    }

    /// Minimal valid `AgentDefinition` for use in `validate_agent_definition` tests.
    fn test_agent_definition() -> AgentDefinition {
        valid_definition()
    }

    #[test]
    fn system_agent_helpers_exclude_system_workers_from_runtime_delegation() {
        let mut worker = valid_definition();
        worker.kind = AgentKind::Worker;
        worker.agent_id = "web-worker".to_string();
        assert!(!worker.is_system_agent());
        assert!(worker.is_runtime_delegate_worker());

        worker.agent_id = "system:internal-worker".to_string();
        assert!(worker.is_system_agent());
        assert!(!worker.is_runtime_delegate_worker());
        assert!(is_system_agent_id("system:meta-agent"));
    }

    /// The widened tool-free exception must not become a way for an ordinary
    /// agent to hold an empty tool list. Every listed surface has to face an
    /// untrusted audience — one owner surface in the list, and the exception is
    /// gone.
    #[test]
    fn the_tool_free_exception_covers_outward_agents_and_nothing_else() {
        let outward = |surfaces: Vec<InvocationSurface>| {
            let mut definition = valid_definition();
            definition.kind = AgentKind::Personal;
            definition.tools = Vec::new();
            definition.delegation_targets = Vec::new();
            definition.invocation_policy.discoverability = AgentDiscoverability::SurfaceOnly;
            definition.invocation_policy.delegation = AgentDelegationPolicy::None;
            definition.invocation_policy.allowed_direct_surfaces = surfaces;
            definition
        };

        // The envoy shape, and the multi-outward-surface shape it becomes.
        assert!(is_strictly_tool_free_public_surface(&outward(vec![
            InvocationSurface::PublicEnvoy
        ])));
        assert!(is_strictly_tool_free_public_surface(&outward(vec![
            InvocationSurface::PublicEnvoy,
            InvocationSurface::Meeting
        ])));

        // One owner surface in the list and the exception must not apply —
        // otherwise a tool-free agent could claim it while facing the owner.
        assert!(!is_strictly_tool_free_public_surface(&outward(vec![
            InvocationSurface::PublicEnvoy,
            InvocationSurface::Chat
        ])));
        // An empty allowlist is the permissive default, not an allowlist.
        assert!(!is_strictly_tool_free_public_surface(&outward(Vec::new())));
    }

    /// `permits_declared_tool` mirrors the two deny-all shapes
    /// `resolved_tools` carries: the strictly tool-free public surface and an
    /// untrusted definition with no explicit allowlist permit nothing, while
    /// every other definition keeps answering only the blocking question.
    #[test]
    fn permits_declared_tool_denies_the_deny_all_shapes() {
        // The strictly tool-free public surface: its empty list is a
        // deliberate deny-all contract, so no declared use is permitted.
        let mut envoy = valid_definition();
        envoy.kind = AgentKind::Personal;
        envoy.tools = Vec::new();
        envoy.delegation_targets = Vec::new();
        envoy.invocation_policy.discoverability = AgentDiscoverability::SurfaceOnly;
        envoy.invocation_policy.delegation = AgentDelegationPolicy::None;
        envoy.invocation_policy.allowed_direct_surfaces = vec![InvocationSurface::PublicEnvoy];
        assert!(is_strictly_tool_free_public_surface(&envoy));
        assert!(!envoy.permits_declared_tool("youtube-search"));

        // Untrusted with no explicit allowlist: the shape validation rejects
        // it, so the predicate must not admit a declared use either.
        let mut untrusted = valid_definition();
        untrusted.trust_level = TrustLevel("untrusted".to_string());
        untrusted.tools = Vec::new();
        assert!(untrusted.trust_level.is_untrusted());
        assert!(!is_strictly_tool_free_public_surface(&untrusted));
        assert!(!untrusted.permits_declared_tool("browser"));

        // A trusted definition with the same empty list still answers only
        // the blocking question: absence from `tools` is not refusal.
        let mut trusted = valid_definition();
        trusted.tools = Vec::new();
        assert!(trusted.permits_declared_tool("browser"));
    }

    /// The safety rule, derived from the named property rather than restated.
    ///
    /// This is the test that makes the next outward channel — a group thread, a
    /// public inbox, an SMS lane — safe by construction: whoever adds it must
    /// state its audience (the exhaustive `audience()` match will not compile
    /// otherwise), and if they say `Untrusted` this assertion covers it without
    /// anyone remembering to also omit it from the defaults list.
    #[test]
    fn no_untrusted_surface_is_ever_a_default_direct_surface() {
        for surface in InvocationSurface::ALL {
            if surface.audience() == SurfaceAudience::Untrusted {
                assert!(
                    !surface.is_default_direct_surface(),
                    "`{}` faces an untrusted audience but is a default surface — \
                     every agent with an empty allowlist is exposed on it",
                    surface.as_str()
                );
            }
        }
    }

    /// `ALL` exists to be iterated by the invariant above, so it going stale
    /// would silently shrink that test's coverage rather than fail it.
    #[test]
    fn the_surface_roster_covers_every_variant() {
        for surface in InvocationSurface::ALL {
            // Round-trips through the wire name, which only holds if the
            // variant is genuinely a member and not a duplicate.
            let wire = serde_json::to_string(&surface).expect("surface serializes");
            assert_eq!(wire.trim_matches('"'), surface.as_str());
        }
        let unique: std::collections::BTreeSet<&str> = InvocationSurface::ALL
            .iter()
            .map(|surface| surface.as_str())
            .collect();
        assert_eq!(
            unique.len(),
            InvocationSurface::ALL.len(),
            "ALL contains a duplicate, so the invariant test silently skips a surface"
        );
    }

    /// The fail-closed property the `Meeting` surface exists for.
    ///
    /// Almost every agent leaves `allowed_direct_surfaces` empty, which means
    /// "the default surfaces". If `Meeting` were one of those, adding the
    /// surface would silently make the full personal assistant answerable in a
    /// room — the exact exposure it is meant to close. So the assertion is not
    /// cosmetic: it is the reason a room cannot reach an ordinary agent without
    /// anyone editing an agent definition.
    #[test]
    fn meeting_is_not_a_default_surface_so_ordinary_agents_cannot_be_reached_in_a_room() {
        assert!(!InvocationSurface::Meeting.is_default_direct_surface());
        assert_eq!(
            serde_json::to_string(&InvocationSurface::Meeting).expect("surface"),
            "\"meeting\""
        );

        // An ordinary agent: no explicit allowlist, so it rides the defaults.
        let ordinary = AgentInvocationPolicy::default();
        assert!(
            ordinary.allowed_direct_surfaces.is_empty(),
            "this test is only meaningful for the empty-allowlist default"
        );
        assert!(
            ordinary.permits_direct_surface(InvocationSurface::RealtimeVoice),
            "a private voice call must keep reaching the ordinary agent"
        );
        assert!(
            !ordinary.permits_direct_surface(InvocationSurface::Meeting),
            "an ordinary agent must be unreachable from a shared room"
        );
        assert!(
            !ordinary.permits_direct_surface(InvocationSurface::Plane),
            "an ordinary agent must be unreachable from a terminal"
        );
    }

    /// The fail-closed property the `Plane` surface exists for.
    ///
    /// Same shape as `Meeting`: empty `allowed_direct_surfaces` means the
    /// default surfaces, and `Plane` is not one of them. A terminal Magician
    /// does not control must not reach every existing agent by inheriting the
    /// chat/voice/task defaults.
    #[test]
    fn plane_is_not_a_default_surface_so_ordinary_agents_cannot_be_reached_from_a_terminal() {
        assert!(!InvocationSurface::Plane.is_default_direct_surface());
        assert_eq!(
            InvocationSurface::Plane.audience(),
            SurfaceAudience::Owner,
            "a plane grant is minted from the owner's session"
        );
        assert_eq!(
            serde_json::to_string(&InvocationSurface::Plane).expect("surface"),
            "\"plane\""
        );

        let ordinary = AgentInvocationPolicy::default();
        assert!(
            ordinary.allowed_direct_surfaces.is_empty(),
            "this test is only meaningful for the empty-allowlist default"
        );
        assert!(
            ordinary.permits_direct_surface(InvocationSurface::Chat),
            "ordinary chat must keep reaching the ordinary agent"
        );
        assert!(
            !ordinary.permits_direct_surface(InvocationSurface::Plane),
            "an ordinary agent must be unreachable from the plane"
        );
    }

    /// Envoy is bound to exactly one surface today. Adding `Meeting` must not
    /// have widened it — an outward agent gains a surface by an explicit,
    /// reviewed edit to its definition, never as a side effect of the enum.
    #[test]
    fn an_explicit_allowlist_is_not_widened_by_the_new_surface() {
        let mut envoy_shaped = AgentInvocationPolicy::default();
        envoy_shaped.discoverability = AgentDiscoverability::SurfaceOnly;
        envoy_shaped.allowed_direct_surfaces = vec![InvocationSurface::PublicEnvoy];

        assert!(envoy_shaped.permits_direct_surface(InvocationSurface::PublicEnvoy));
        assert!(!envoy_shaped.permits_direct_surface(InvocationSurface::Meeting));
        assert!(!envoy_shaped.permits_direct_surface(InvocationSurface::Plane));
        assert!(!envoy_shaped.permits_direct_surface(InvocationSurface::RealtimeVoice));
    }

    #[test]
    fn shared_delegation_resolver_keeps_surface_only_none_target_dormant() {
        let mut source = valid_definition();
        source.agent_id = "presto".to_string();
        source.delegation_targets = vec!["*".to_string(), "brainstorm-facilitator".to_string()];

        let mut ordinary = valid_definition();
        ordinary.agent_id = "web-researcher".to_string();
        ordinary.kind = AgentKind::Worker;

        let mut loom = valid_definition();
        loom.agent_id = "brainstorm-facilitator".to_string();
        loom.invocation_policy = AgentInvocationPolicy {
            discoverability: AgentDiscoverability::SurfaceOnly,
            delegation: AgentDelegationPolicy::None,
            allowed_direct_surfaces: vec![InvocationSurface::ThinkingMap],
        };

        let resolved = resolve_effective_delegation_target_ids(
            &source,
            [&source, &ordinary, &loom],
            &HashSet::new(),
        );
        assert_eq!(resolved, vec!["web-researcher".to_string()]);
    }

    #[test]
    fn shared_delegation_resolver_does_not_expand_wildcard_for_explicit_source() {
        let mut source = valid_definition();
        source.agent_id = "presto".to_string();
        source.delegation_targets = vec!["named-worker".to_string(), "*".to_string()];
        source.invocation_policy.delegation = AgentDelegationPolicy::Explicit;

        let mut named = valid_definition();
        named.agent_id = "named-worker".to_string();
        named.kind = AgentKind::Worker;

        let mut ambient = valid_definition();
        ambient.agent_id = "ambient-worker".to_string();
        ambient.kind = AgentKind::Worker;

        let resolved = resolve_effective_delegation_target_ids(
            &source,
            [&source, &named, &ambient],
            &HashSet::new(),
        );
        assert_eq!(resolved, vec!["named-worker".to_string()]);
    }

    #[test]
    fn shared_delegation_resolver_admits_targets_per_transition_surface() {
        let mut source = valid_definition();
        source.agent_id = "presto".to_string();
        source.delegation_targets = vec!["delegate-only".to_string(), "handover-only".to_string()];

        let mut delegate_only = valid_definition();
        delegate_only.agent_id = "delegate-only".to_string();
        delegate_only.kind = AgentKind::Worker;
        delegate_only.invocation_policy.allowed_direct_surfaces =
            vec![InvocationSurface::Delegation];

        let mut handover_only = valid_definition();
        handover_only.agent_id = "handover-only".to_string();
        handover_only.kind = AgentKind::Worker;
        handover_only.invocation_policy.allowed_direct_surfaces = vec![InvocationSurface::Handover];

        let definitions = [&source, &delegate_only, &handover_only];
        assert_eq!(
            resolve_effective_delegation_target_ids_for_surface(
                &source,
                definitions,
                &HashSet::new(),
                InvocationSurface::Delegation,
            ),
            vec!["delegate-only".to_string()]
        );
        assert_eq!(
            resolve_effective_delegation_target_ids_for_surface(
                &source,
                definitions,
                &HashSet::new(),
                InvocationSurface::Handover,
            ),
            vec!["handover-only".to_string()]
        );
    }

    #[test]
    fn agent_routing_context_serde_roundtrip() {
        let ctx = AgentRoutingContext::new("agent-1", "goal-42", "cycle-7");
        let json = serde_json::to_string(&ctx).unwrap();
        let deserialized: AgentRoutingContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, deserialized);
    }

    #[test]
    fn storage_key_has_agent_prefix() {
        let ctx = AgentRoutingContext::new("a1", "g2", "c3");
        assert_eq!(ctx.storage_key(), "agent:a1:g2:c3");
    }

    #[test]
    fn action_pattern_wildcard_matches() {
        let single = ActionPattern::Single("*".to_string());
        assert!(single.matches("navigate"));

        let multiple = ActionPattern::Multiple(vec!["click".to_string(), "*".to_string()]);
        assert!(multiple.matches("type"));
    }

    #[test]
    fn correction_retention_deserializes_forever() {
        let yaml = "resolved_days: 30\nactive: forever\n";
        let retention: CorrectionRetention = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(retention.active, None);
    }

    #[test]
    fn correction_retention_rejects_zero_days() {
        let yaml = "resolved_days: 30\nactive: 0\n";
        let err = serde_yaml::from_str::<CorrectionRetention>(yaml).unwrap_err();
        assert!(err
            .to_string()
            .contains("active must be `forever` or a positive integer"));
    }

    #[test]
    fn correction_retention_serializes_none_as_forever() {
        let retention = CorrectionRetention {
            resolved_days: 30,
            active: None,
        };
        let yaml = serde_yaml::to_string(&retention).unwrap();
        assert!(yaml.contains("active: forever"));
    }

    #[test]
    fn parse_and_validate_agent_definition() {
        let yaml = r#"
agent_id: career-copilot
version: 1
name: Career Copilot
description: Helps with job search
persona: You are practical
kind: worker
constraints:
  max_iterations: 20
  max_tokens_per_cycle: 10000
  max_consecutive_failures: 3
tools: [browser, files]
memory_tiers:
  - name: task_progress
    scope: agent_goal
    description: Task tracking
    schema:
      context_summary: { type: text }
    render:
      format: compact_summary
      template: "{context_summary}"
    retention: goal_lifetime
"#;

        let parsed = AgentDefinition::from_yaml_str(yaml).unwrap();
        assert_eq!(parsed.agent_id, "career-copilot");
        assert_eq!(parsed.kind, AgentKind::Worker);
    }

    #[test]
    fn validate_rejects_empty_state_machine_key() {
        let mut definition = valid_definition();
        definition
            .state_machines
            .insert(" ".to_string(), Value::String("noop".to_string()));

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("state_machines keys must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_accumulates_multiple_errors() {
        let mut definition = valid_definition();
        definition.name = "   ".to_string();
        definition.persona = " ".to_string();

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.starts_with(MULTI_VALIDATION_ERROR_PREFIX));
                assert!(msg.contains("name must not be empty"));
                assert!(msg.contains("persona must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_path_unsafe_agent_id() {
        let mut definition = valid_definition();
        definition.agent_id = "../agent".to_string();

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("agent_id must be ASCII"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_reserved_agent_id() {
        let mut definition = valid_definition();
        definition.agent_id = "approvals".to_string();

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("reserved infrastructure names"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_whitespace_and_overlong_agent_id() {
        let mut definition = valid_definition();
        definition.agent_id = " agent-a ".to_string();

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("agent_id must be ASCII"));
            },
            _ => panic!("unexpected error variant"),
        }

        let mut definition = valid_definition();
        definition.agent_id = "a".repeat(256);
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("agent_id must be ASCII"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_overlong_agent_name() {
        let mut definition = valid_definition();
        definition.name = "a".repeat(MAX_STRING_FIELD_BYTES + 1);

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("definition.name"));
                assert!(msg.contains("exceeds maximum length"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_whitespace_padded_circuit_breaker_goal_override_key() {
        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 1,
                action: CircuitAction::InjectFailureContext,
                escalation: None,
                notify: Vec::new(),
            }],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::from([(
                " g1 ".to_string(),
                CircuitBreakerOverride { max_failures: 2 },
            )]),
            ..Default::default()
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("per_goal_override key"));
                assert!(msg.contains("leading or trailing whitespace"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_whitespace_padded_retention_goal_override_key() {
        let mut definition = valid_definition();
        definition.retention = Some(RetentionPolicy {
            episodes: EpisodeRetention {
                default_days: 30,
                on_failure: Some(90),
                per_goal_override: HashMap::from([(" g1 ".to_string(), 15)]),
                consolidate_before_delete: false,
            },
            corrections: CorrectionRetention {
                resolved_days: 7,
                active: Some(14),
            },
            definition_versions: VersionRetention { keep_last: 5 },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("retention.episodes.per_goal_override key"));
                assert!(msg.contains("leading or trailing whitespace"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_zero_core_constraints() {
        let mut definition = valid_definition();
        definition.constraints.max_iterations = 0;
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("max_iterations"));
            },
            _ => panic!("unexpected error variant"),
        }

        let mut definition = valid_definition();
        definition.constraints.max_tokens_per_cycle = 0;
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("max_tokens_per_cycle"));
            },
            _ => panic!("unexpected error variant"),
        }

        let mut definition = valid_definition();
        definition.constraints.max_consecutive_failures = 0;
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("max_consecutive_failures"));
            },
            _ => panic!("unexpected error variant"),
        }

        let mut definition = valid_definition();
        definition.constraints.coordination.delegation_timeout_secs = 0;
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("delegation_timeout_secs"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_invalid_circuit_breaker_recovery_and_overrides() {
        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::new(),
            ..Default::default()
        });
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("thresholds must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }

        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 1,
                action: CircuitAction::InjectFailureContext,
                escalation: None,
                notify: Vec::new(),
            }],
            recovery: CircuitRecovery::TimeBased { cooldown_hours: 0 },
            per_goal_override: HashMap::new(),
            ..Default::default()
        });
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("cooldown_hours"));
            },
            _ => panic!("unexpected error variant"),
        }

        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 1,
                action: CircuitAction::InjectFailureContext,
                escalation: None,
                notify: Vec::new(),
            }],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::from([(
                "g1".to_string(),
                CircuitBreakerOverride { max_failures: 0 },
            )]),
            ..Default::default()
        });
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("max_failures"));
            },
            _ => panic!("unexpected error variant"),
        }

        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![
                CircuitBreakerThreshold {
                    failures: 3,
                    action: CircuitAction::OpenCircuit,
                    escalation: None,
                    notify: Vec::new(),
                },
                CircuitBreakerThreshold {
                    failures: 1,
                    action: CircuitAction::InjectFailureContext,
                    escalation: None,
                    notify: Vec::new(),
                },
            ],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::new(),
            ..Default::default()
        });
        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("strictly increasing"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn parse_rejects_unknown_top_level_field() {
        let yaml = r#"
agent_id: career-copilot
name: Career Copilot
persona: test
unknown_top_level: true
"#;

        let err = AgentDefinition::from_yaml_str(yaml).unwrap_err();
        match err {
            AgentDefinitionError::Yaml(parse_err) => {
                assert!(parse_err.to_string().contains("unknown field"));
                assert!(parse_err.to_string().contains("unknown_top_level"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_unknown_nested_constraints_field() {
        let yaml = r#"
agent_id: career-copilot
name: Career Copilot
persona: test
constraints:
  max_iterations: 10
  max_tokens_per_cycle: 1000
  max_consecutive_failures_typo: 3
"#;

        let err = AgentDefinition::from_yaml_str(yaml).unwrap_err();
        match err {
            AgentDefinitionError::Yaml(parse_err) => {
                assert!(parse_err.to_string().contains("unknown field"));
                assert!(parse_err
                    .to_string()
                    .contains("max_consecutive_failures_typo"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parse_circuit_breaker_uses_default_thresholds_when_omitted() {
        let yaml = r#"
agent_id: career-copilot
name: Career Copilot
persona: test
circuit_breaker:
  recovery:
    trigger: user_reset
"#;

        let definition = AgentDefinition::from_yaml_str(yaml).unwrap();
        let policy = definition
            .circuit_breaker
            .expect("circuit_breaker must parse");
        assert_eq!(policy.thresholds.len(), 2);
        assert_eq!(policy.thresholds[0].failures, 1);
        assert_eq!(policy.thresholds[1].failures, 3);
        assert_eq!(policy.thresholds[1].notify, vec!["chat".to_string()]);
    }

    #[test]
    fn parse_rejects_unknown_circuit_recovery_field() {
        let yaml = r#"
agent_id: career-copilot
name: Career Copilot
persona: test
circuit_breaker:
  recovery:
    trigger: time_based
    cooldown_hours: 24
    typo: true
"#;

        let err = AgentDefinition::from_yaml_str(yaml).unwrap_err();
        match err {
            AgentDefinitionError::Yaml(parse_err) => {
                assert!(parse_err.to_string().contains("unknown field"));
                assert!(parse_err.to_string().contains("typo"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn validation_rejects_unknown_prompt_truncation_section() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "persona".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: true,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: vec!["missing_section".to_string()],
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("truncation_priority references unknown section"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_unknown_consolidation_target_root() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_target".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "episodes(g1, limit=1)".to_string(),
            target: "unknown_tier".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("unknown tier root"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_memory_tier_name_with_dot() {
        let mut definition = valid_definition();
        definition.memory_tiers[0].name = "price.history".to_string();

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("must not contain `.`"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_whitespace_padded_memory_tier_name() {
        let mut definition = valid_definition();
        definition.memory_tiers[0].name = " task_progress ".to_string();

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("memory tier"));
                assert!(msg.contains("leading or trailing whitespace"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_zero_memory_tier_retention_days() {
        let mut definition = valid_definition();
        definition.memory_tiers[0].retention = RetentionMode::Days(0);

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("retention.days must be > 0"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_invalid_consolidation_source() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_source".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "memory.episodes(goal_id, limit=1)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("source"));
                assert!(msg.contains("invalid"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_batch_trigger_without_parameters() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_batch".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: None,
                interval_days: None,
                min_episodes: None,
                max_staleness_hours: None,
            },
            source: "episodes(goal_id, limit=1)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("batch trigger"));
                assert!(msg.contains("at least one"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_batch_trigger_zero_values() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_batch_zero".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(0),
                interval_days: None,
                min_episodes: Some(10),
                max_staleness_hours: None,
            },
            source: "episodes(goal_id, limit=1)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("interval_hours"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_unknown_local_consolidation_source_tier() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_source_tier".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "tiers(missing_tier)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("unknown local tier"));
                assert!(msg.contains("missing_tier"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_dot_prefixed_consolidation_target() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "dot_target".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "episodes(g1, limit=1)".to_string(),
            target: ".sneaky".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("must not begin with `.`"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_cross_agent_consolidation_tier_source() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "cross_agent_tiers".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(1),
                interval_days: None,
                min_episodes: None,
                max_staleness_hours: None,
            },
            source: "tiers(other_agent.task_progress)".to_string(),
            target: "entities".to_string(),
            transform: ConsolidationTransform::Render {
                template: "{source_text}".to_string(),
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("cross-agent tier"));
                assert!(msg.contains("not supported"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_batch_min_episodes_with_tier_source() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "tier_batch_min".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(1),
                interval_days: None,
                min_episodes: Some(2),
                max_staleness_hours: None,
            },
            source: "tiers(entities)".to_string(),
            target: "knowledge".to_string(),
            transform: ConsolidationTransform::Render {
                template: "{source_text}".to_string(),
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("min_episodes"));
                assert!(msg.contains("episodes(...)"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_batch_min_only_without_unprocessed_episode_source() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "min_only_replay".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: None,
                interval_days: None,
                min_episodes: Some(1),
                max_staleness_hours: None,
            },
            source: "episodes(g1, limit=5)".to_string(),
            target: "knowledge".to_string(),
            transform: ConsolidationTransform::Render {
                template: "{source_text}".to_string(),
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("unprocessed=true"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_batch_tiers_source_for_agent_goal_tier() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "batch_goal_tier_source".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(1),
                interval_days: None,
                min_episodes: None,
                max_staleness_hours: None,
            },
            source: "tiers(task_progress)".to_string(),
            target: "entities".to_string(),
            transform: ConsolidationTransform::Render {
                template: "{source_text}".to_string(),
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("agent-goal scoped"));
                assert!(msg.contains("batch"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_batch_targeting_agent_goal_tier() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "batch_goal_tier_target".to_string(),
            trigger: ConsolidationTrigger::Batch {
                interval_hours: Some(1),
                interval_days: None,
                min_episodes: None,
                max_staleness_hours: None,
            },
            source: "episodes(g1, unprocessed=true, limit=5)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Render {
                template: "{source_text}".to_string(),
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("agent-goal scoped"));
                assert!(msg.contains("batch"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_consolidation_user_target_without_path() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_user_target".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "episodes(g1, limit=1)".to_string(),
            target: "user".to_string(),
            transform: ConsolidationTransform::Render {
                template: "{source_text}".to_string(),
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("user target"));
                assert!(msg.contains("user."));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_consolidation_target_with_empty_path_segments() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_path_segments".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "episodes(g1, limit=1)".to_string(),
            target: "user..knowledge".to_string(),
            transform: ConsolidationTransform::Render {
                template: "{source_text}".to_string(),
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("empty path segments"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_trust_level() {
        let mut definition = valid_definition();
        definition.trust_level = TrustLevel("".to_string());

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("trust_level must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_unrecognized_trust_level() {
        // `high` is the real-world miss: a GoalPriority value mistakenly used as a
        // trust level. It matches no policy entry → silent deny-all at runtime, so
        // validation must reject it loudly.
        let mut definition = valid_definition();
        definition.trust_level = TrustLevel("high".to_string());

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("not a recognized level"), "got: {msg}");
                assert!(msg.contains("high"), "got: {msg}");
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn trust_level_is_recognized_matches_canonical_vocabulary() {
        for ok in [
            "builtin",
            "local",
            "reviewed",
            "untrusted",
            "standard",
            " LOCAL ",
        ] {
            assert!(
                TrustLevel(ok.to_string()).is_recognized(),
                "{ok} should be recognized"
            );
        }
        for bad in ["high", "medium", "low", "critical", "trusted", ""] {
            assert!(
                !TrustLevel(bad.to_string()).is_recognized(),
                "{bad} should NOT be recognized"
            );
        }
    }

    #[test]
    fn trust_level_is_untrusted_is_trimmed_and_case_insensitive() {
        assert!(TrustLevel(" UnTrUsTeD ".to_string()).is_untrusted());
        assert!(!TrustLevel("local".to_string()).is_untrusted());
    }

    #[test]
    fn trust_level_canonicalizes_legacy_standard_alias() {
        assert_eq!(
            TrustLevel::canonicalized_value("standard"),
            TrustLevel::LOCAL
        );
        assert_eq!(
            TrustLevel::canonicalized_value(" Standard "),
            TrustLevel::LOCAL
        );
        assert_eq!(
            TrustLevel::canonicalized_value("reviewed"),
            "reviewed".to_string()
        );
        assert_eq!(
            TrustLevel("standard".to_string()).canonicalized(),
            TrustLevel(TrustLevel::LOCAL.to_string())
        );
    }

    #[test]
    fn denied_tool_parameter_match_canonicalizes_keys_and_value_case() {
        let rules = HashMap::from([
            (
                " shell ".to_string(),
                HashMap::from([(" command ".to_string(), vec!["rm -rf".to_string()])]),
            ),
            // A canonically duplicate entry cannot shadow the deny rule above,
            // irrespective of HashMap iteration order.
            (
                "shell".to_string(),
                HashMap::from([("command".to_string(), vec!["sudo".to_string()])]),
            ),
        ]);
        let parameters = HashMap::from([(
            "command".to_string(),
            Value::String("  RM -RF /tmp/example".to_string()),
        )]);

        assert_eq!(
            denied_tool_parameter_match(&rules, "shell", &parameters),
            Some((
                "command".to_string(),
                "  RM -RF /tmp/example".to_string(),
                "rm -rf".to_string(),
            ))
        );
        let sudo_parameters = HashMap::from([(
            " command ".to_string(),
            Value::String("SUDO reboot".to_string()),
        )]);
        assert_eq!(
            denied_tool_parameter_match(&rules, "shell", &sudo_parameters),
            Some((
                "command".to_string(),
                "SUDO reboot".to_string(),
                "sudo".to_string(),
            ))
        );
        assert!(denied_tool_parameter_match(&rules, "browser", &parameters).is_none());
    }

    #[test]
    fn denied_tool_parameter_match_checks_every_vector_content_branch() {
        let read_rules = HashMap::from([(
            "content_read".to_string(),
            HashMap::from([("url".to_string(), vec!["https://private.".to_string()])]),
        )]);
        let read_parameters = HashMap::from([(
            "requests".to_string(),
            serde_json::json!([
                {"url": "https://public.example.test/report"},
                {"candidate": {"canonical_url": "https://private.example.test/report"}}
            ]),
        )]);
        assert_eq!(
            denied_tool_parameter_match(&read_rules, "content_read", &read_parameters),
            Some((
                "url".to_string(),
                "https://private.example.test/report".to_string(),
                "https://private.".to_string(),
            ))
        );

        let search_rules = HashMap::from([(
            "content_search".to_string(),
            HashMap::from([("query".to_string(), vec!["secret".to_string()])]),
        )]);
        let search_parameters = HashMap::from([(
            "requests".to_string(),
            serde_json::json!([
                {"query": "public release notes"},
                {"query": "Secret roadmap"}
            ]),
        )]);
        assert!(
            denied_tool_parameter_match(&search_rules, "content_search", &search_parameters,)
                .is_some()
        );
    }

    #[test]
    fn validation_rejects_empty_tool_name() {
        let mut definition = valid_definition();
        definition.tools = vec!["".to_string()];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("tool name must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_untrusted_agent_with_implicit_all_tools() {
        let mut definition = valid_definition();
        definition.trust_level = TrustLevel("Untrusted".to_string());
        definition.tools.clear();

        let error = definition.validate().expect_err("must fail closed");
        assert!(error
            .to_string()
            .contains("explicit non-empty tools allowlist"));
        assert!(validate_agent_definition(&definition)
            .expect_err("kind validation must fail closed")
            .iter()
            .any(|message| message.contains("explicit non-empty tools allowlist")));
    }

    #[test]
    fn validation_allows_only_the_exact_tool_free_public_envoy_shape() {
        let mut definition = valid_definition();
        definition.kind = AgentKind::Personal;
        definition.trust_level = TrustLevel("untrusted".to_string());
        definition.tools.clear();
        definition.delegation_targets.clear();
        definition.invocation_policy = AgentInvocationPolicy {
            discoverability: AgentDiscoverability::SurfaceOnly,
            delegation: AgentDelegationPolicy::None,
            allowed_direct_surfaces: vec![InvocationSurface::PublicEnvoy],
        };

        definition
            .validate()
            .expect("an exact tool-free public surface is fail-closed by construction");
        validate_agent_definition(&definition)
            .expect("kind validation must accept the same exact public surface");
        assert!(definition
            .resolved_tools(&["time_math".to_string(), "search_memory".to_string()])
            .is_empty());

        definition
            .invocation_policy
            .allowed_direct_surfaces
            .push(InvocationSurface::Chat);
        let error = definition
            .validate()
            .expect_err("adding any other surface must restore the untrusted allowlist rule");
        assert!(error
            .to_string()
            .contains("explicit non-empty tools allowlist"));
    }

    #[test]
    fn resolved_tools_canonicalizes_names_and_applies_trimmed_deny_rules() {
        let mut definition = valid_definition();
        definition.tools = vec![
            " browser ".to_string(),
            "search_memory".to_string(),
            "browser".to_string(),
            " shell ".to_string(),
            "time_math".to_string(),
        ];
        definition.excluded_tools = vec![" shell".to_string(), " core_utility ".to_string()];
        definition.denied_tools = vec![" search_memory ".to_string()];

        assert_eq!(definition.resolved_tools(&[]), vec!["browser".to_string()]);
    }

    #[test]
    fn validation_rejects_empty_url_contains_entries() {
        let mut definition = valid_definition();
        definition.constraints.requires_approval = vec![ApprovalRule {
            tool: "browser".to_string(),
            action: ActionPattern::Single("*".to_string()),
            ttl_secs: None,
            when: Some(ApprovalCondition {
                param_matches: HashMap::new(),
                url_contains: vec!["".to_string()],
            }),
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("url_contains entries must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_approval_rule_tool() {
        let mut definition = valid_definition();
        definition.constraints.requires_approval = vec![ApprovalRule {
            tool: "".to_string(),
            action: ActionPattern::Single("*".to_string()),
            ttl_secs: None,
            when: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("approval rule tool must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_approval_rule_action() {
        let mut definition = valid_definition();
        definition.constraints.requires_approval = vec![ApprovalRule {
            tool: "browser".to_string(),
            action: ActionPattern::Single("".to_string()),
            ttl_secs: None,
            when: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("approval rule action must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_whitespace_padded_approval_rule_tool() {
        let mut definition = valid_definition();
        definition.constraints.requires_approval = vec![ApprovalRule {
            tool: " browser ".to_string(),
            action: ActionPattern::Single("*".to_string()),
            ttl_secs: None,
            when: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains(
                    "approval rule tool ` browser ` must not contain leading or trailing whitespace"
                ));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_whitespace_padded_approval_rule_action_entries() {
        let mut definition = valid_definition();
        definition.constraints.requires_approval = vec![ApprovalRule {
            tool: "browser".to_string(),
            action: ActionPattern::Multiple(vec![" click ".to_string(), "submit".to_string()]),
            ttl_secs: None,
            when: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains(
                    "approval rule action entries must not contain leading or trailing whitespace",
                ));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_delegation_targets_entry() {
        let mut definition = valid_definition();
        definition.delegation_targets = vec!["".to_string()];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("delegation_targets entries must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_approval_rule_tool_not_declared() {
        let mut definition = valid_definition();
        definition.constraints.requires_approval = vec![ApprovalRule {
            tool: "shell".to_string(),
            action: ActionPattern::Single("*".to_string()),
            ttl_secs: None,
            when: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(
                    msg.contains("approval rule tool `shell` must be `*` or one of declared tools")
                );
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_section_with_empty_name() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("section name must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_section_with_empty_source() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "persona".to_string(),
                source: " ".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("must have either `source` or `content`"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_accepts_inline_content_section() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "instructions".to_string(),
                source: String::new(),
                content: Some("Follow these steps to complete the task.".to_string()),
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        definition
            .validate()
            .expect("inline content section should be valid");
    }

    #[test]
    fn validation_rejects_section_with_both_source_and_content() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "persona".to_string(),
                source: "definition.persona".to_string(),
                content: Some("Inline text".to_string()),
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("must have either `source` or `content`, not both"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_section_with_neither_source_nor_content() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "empty".to_string(),
                source: String::new(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("must have either `source` or `content`"));
                assert!(!msg.contains("not both"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_filter_on_inline_content_section() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "instructions".to_string(),
                source: String::new(),
                content: Some("Do the thing.".to_string()),
                required: false,
                condition: None,
                format: None,
                filter: Some("some_filter".to_string()),
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("filter is not supported for inline content sections"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_zero_max_context_tokens() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: Vec::new(),
            output_rules: PromptOutputRules {
                max_context_tokens: 0,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("max_context_tokens must be > 0"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn channel_config_enabled_defaults_to_true() {
        let yaml = r#"
name: slack
adapter: slack
"#;
        let config: ChannelConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
    }

    #[test]
    fn validation_rejects_version_zero() {
        let mut definition = valid_definition();
        definition.version = 0;
        let err = definition.validate().unwrap_err();
        assert!(
            format!("{err:?}").contains("version must be >= 1"),
            "expected version error, got: {err:?}"
        );
    }

    #[test]
    fn validation_rejects_path_unsafe_remote_agent_in_tier_ref() {
        let mut definition = valid_definition();
        definition.memory_tiers = vec![MemoryTierDefinition {
            name: "local_tier".to_string(),
            scope: TierScope::Agent,
            description: "test tier".to_string(),
            schema: BTreeMap::from([(String::from("field"), TierFieldSchema::Text {})]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{field}".to_string(),
            },
            retention: RetentionMode::Forever,
        }];
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "bad_agent_ref".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "tiers(a/b.local_tier)".to_string(),
            target: "local_tier".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];
        let err = definition.validate().unwrap_err();
        assert!(
            format!("{err:?}").contains("path-unsafe"),
            "expected path-unsafe error, got: {err:?}"
        );
    }

    #[test]
    fn validation_rejects_zero_approval_ttl_secs() {
        let mut definition = valid_definition();
        definition.constraints.approval_ttl_secs = 0;

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("approval_ttl_secs must be > 0"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_duplicate_memory_tier_names() {
        let mut definition = valid_definition();
        let dup = definition.memory_tiers[0].clone();
        definition.memory_tiers.push(dup);

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("duplicate memory tier"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_notification_condition() {
        let mut definition = valid_definition();
        definition.notification_rules = vec![NotificationRule {
            r#match: "agent.cycle.failed".to_string(),
            severity: NotificationSeverity::Medium,
            channels: vec!["webhook".to_string()],
            condition: Some("".to_string()),
            message: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("condition must not be empty when provided"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_notification_message() {
        let mut definition = valid_definition();
        definition.notification_rules = vec![NotificationRule {
            r#match: "agent.cycle.failed".to_string(),
            severity: NotificationSeverity::Medium,
            channels: vec!["webhook".to_string()],
            condition: None,
            message: Some("  ".to_string()),
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("message must not be empty when provided"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_duplicate_notification_channels() {
        let mut definition = valid_definition();
        definition.notification_rules = vec![NotificationRule {
            r#match: "agent.cycle.failed".to_string(),
            severity: NotificationSeverity::Medium,
            channels: vec!["webhook".to_string(), "webhook".to_string()],
            condition: None,
            message: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("channels must not contain duplicates"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_unsupported_notification_channels() {
        let mut definition = valid_definition();
        definition.notification_rules = vec![NotificationRule {
            r#match: "agent.cycle.failed".to_string(),
            severity: NotificationSeverity::Medium,
            channels: vec!["execution_panel".to_string()],
            condition: None,
            message: None,
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("unsupported notification rule channel `execution_panel`"));
                assert!(msg.contains("chat, webhook, agent_memory"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_duplicate_strategy_ordered_entries() {
        let mut definition = valid_definition();
        definition.strategy = Some(StrategyPreference::Ordered(vec![
            "greedy".to_string(),
            "greedy".to_string(),
        ]));

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("strategy.ordered must not contain duplicates"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_feedback_filter() {
        let mut definition = valid_definition();
        definition.feedback_loops = vec![FeedbackLoopDefinition {
            name: "test_loop".to_string(),
            trigger: "cycle.completed".to_string(),
            extract: FeedbackExtract {
                source: "episodes".to_string(),
                filter: Some("".to_string()),
                fields: vec!["outcome".to_string()],
            },
            transform: "summarize".to_string(),
            inject_into: "prompt".to_string(),
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("extract.filter must not be empty when provided"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_feedback_fields_entry() {
        let mut definition = valid_definition();
        definition.feedback_loops = vec![FeedbackLoopDefinition {
            name: "test_loop".to_string(),
            trigger: "cycle.completed".to_string(),
            extract: FeedbackExtract {
                source: "episodes".to_string(),
                filter: None,
                fields: vec!["outcome".to_string(), "".to_string()],
            },
            transform: "summarize".to_string(),
            inject_into: "prompt".to_string(),
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("extract.fields entries must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_pipeline_feedback_target_without_prompt_pipeline() {
        let mut definition = valid_definition();
        definition.feedback_loops = vec![FeedbackLoopDefinition {
            name: "test_loop".to_string(),
            trigger: "cycle.completed".to_string(),
            extract: FeedbackExtract {
                source: "episodes".to_string(),
                filter: None,
                fields: vec!["outcome".to_string()],
            },
            transform: "summarize".to_string(),
            inject_into: "prompt_pipeline.context".to_string(),
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("prompt_pipeline is not configured"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_pipeline_feedback_target_for_unknown_section() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "known_context".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 200,
                truncation_priority: vec!["known_context".to_string()],
            },
        });
        definition.feedback_loops = vec![FeedbackLoopDefinition {
            name: "test_loop".to_string(),
            trigger: "cycle.completed".to_string(),
            extract: FeedbackExtract {
                source: "episodes".to_string(),
                filter: None,
                fields: vec!["outcome".to_string()],
            },
            transform: "summarize".to_string(),
            inject_into: "prompt_pipeline.unknown_context".to_string(),
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("unknown prompt pipeline section"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_accepts_prompt_pipeline_feedback_target_for_known_section() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "known_context".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 200,
                truncation_priority: vec!["known_context".to_string()],
            },
        });
        definition.feedback_loops = vec![FeedbackLoopDefinition {
            name: "test_loop".to_string(),
            trigger: "cycle.completed".to_string(),
            extract: FeedbackExtract {
                source: "episodes".to_string(),
                filter: None,
                fields: vec!["outcome".to_string()],
            },
            transform: "summarize".to_string(),
            inject_into: "prompt_pipeline.known_context".to_string(),
        }];

        assert!(definition.validate().is_ok());
    }

    #[test]
    fn goal_priority_cmp_uses_semantic_order() {
        let critical = GoalPriority(GoalPriority::CRITICAL.to_string());
        let high = GoalPriority(GoalPriority::HIGH.to_string());
        let medium = GoalPriority(GoalPriority::MEDIUM.to_string());
        let low = GoalPriority(GoalPriority::LOW.to_string());

        assert!(critical > high);
        assert!(high > medium);
        assert!(medium > low);
    }

    #[test]
    fn goal_priority_cmp_normalizes_case_and_whitespace() {
        let critical = GoalPriority(" Critical ".to_string());
        let high = GoalPriority("HIGH".to_string());
        assert!(critical > high);
    }

    #[test]
    fn goal_priority_eq_normalizes_case_and_whitespace() {
        let canonical = GoalPriority("high".to_string());
        let normalized = GoalPriority(" HIGH ".to_string());
        assert_eq!(canonical, normalized);
        assert_eq!(canonical.cmp(&normalized), std::cmp::Ordering::Equal);
    }

    #[test]
    fn goal_priority_unknown_is_below_low() {
        let unknown = GoalPriority("p0".to_string());
        let low = GoalPriority(GoalPriority::LOW.to_string());
        assert!(unknown < low);
    }

    #[test]
    fn goal_priority_unknown_levels_are_not_cmp_equal_when_values_differ() {
        let p0 = GoalPriority("p0".to_string());
        let q0 = GoalPriority("q0".to_string());
        assert_ne!(p0, q0);
        assert_ne!(p0.cmp(&q0), std::cmp::Ordering::Equal);
    }

    #[test]
    fn validation_rejects_empty_circuit_breaker_escalation() {
        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 1,
                action: CircuitAction::InjectFailureContext,
                escalation: Some("".to_string()),
                notify: Vec::new(),
            }],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::new(),
            ..Default::default()
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("escalation must not be empty when provided"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_circuit_breaker_notify_entry() {
        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 1,
                action: CircuitAction::InjectFailureContext,
                escalation: None,
                notify: vec!["chat".to_string(), "".to_string()],
            }],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::new(),
            ..Default::default()
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("notify entries must not be empty"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_unsupported_circuit_breaker_notify_channel() {
        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 1,
                action: CircuitAction::OpenCircuit,
                escalation: None,
                notify: vec!["ui".to_string()],
            }],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::new(),
            ..Default::default()
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("unsupported circuit_breaker notify channel `ui`"));
                assert!(msg.contains("chat, webhook, agent_memory"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_duplicate_circuit_breaker_notify_channels() {
        let mut definition = valid_definition();
        definition.circuit_breaker = Some(CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 1,
                action: CircuitAction::OpenCircuit,
                escalation: None,
                notify: vec!["chat".to_string(), "chat".to_string()],
            }],
            recovery: CircuitRecovery::UserReset,
            per_goal_override: HashMap::new(),
            ..Default::default()
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("notify entries must not contain duplicates (`chat`)"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_prompt_section_condition() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "persona".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: false,
                condition: Some("".to_string()),
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("condition must not be empty when provided"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_prompt_section_filter() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "persona".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: Some("  ".to_string()),
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("filter must not be empty when provided"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_empty_prompt_section_format() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "persona".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: false,
                condition: None,
                format: Some("   ".to_string()),
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("format must not be empty when provided"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_invalid_prompt_section_source() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "invalid_source".to_string(),
                source: "memory.not_supported".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("source"));
                assert!(msg.contains("unsupported"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_tier_source_filter() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "tier_section".to_string(),
                source: "memory.tier[task_progress]".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: Some("active_only".to_string()),
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("filter is not supported for tier sources"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_episodes_source_unprocessed_selector() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "history".to_string(),
                source: "memory.episodes(goal_id, unprocessed=false, limit=5)".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("uses `unprocessed` selector"));
                assert!(msg.contains("not supported"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_memory_tier_source_unknown_local_tier() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "tier_context".to_string(),
                source: "memory.tier[missing_tier].summary".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("unknown local tier"));
                assert!(msg.contains("missing_tier"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_prompt_tiers_source_cross_agent_reference() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "cross_agent_tier".to_string(),
                source: "tiers(other_agent.task_progress)".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("cross-agent tier"));
                assert!(msg.contains("not supported"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_accepts_prompt_tiers_source_self_reference_with_dotted_agent_id() {
        let mut definition = valid_definition();
        definition.agent_id = "agent.v2".to_string();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "self_tier".to_string(),
                source: "tiers(agent.v2.task_progress)".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: Vec::new(),
            },
        });

        definition.validate().unwrap();
    }

    #[test]
    fn validation_accepts_consolidation_tiers_source_self_reference_with_dotted_agent_id_for_llm() {
        let mut definition = valid_definition();
        definition.agent_id = "agent.v2".to_string();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "self_ref".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "tiers(agent.v2.task_progress)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Llm {
                prompt: "summarize".to_string(),
                operation: None,
                system_prompt: None,
                merge: None,
            },
        }];

        definition.validate().unwrap();
    }

    #[test]
    fn validation_rejects_cycle_completed_structured_tiers_source_even_when_self_referenced() {
        let mut definition = valid_definition();
        definition.agent_id = "agent.v2".to_string();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "self_ref_structured".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "tiers(agent.v2.task_progress)".to_string(),
            target: "task_progress".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg
                    .contains("unsupported for cycle_completed structured transforms in phase 3"));
                assert!(msg.contains("expected episodes(...)"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_cycle_completed_structured_user_target() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "user_target_structured".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "episodes(g1, limit=1)".to_string(),
            target: "user.knowledge".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("target `user.knowledge` is unsupported for cycle_completed structured transforms in phase 3"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_cycle_completed_structured_report_target() {
        let mut definition = valid_definition();
        definition.memory_consolidation = vec![MemoryConsolidationRule {
            name: "report_target_structured".to_string(),
            trigger: ConsolidationTrigger::CycleCompleted,
            source: "episodes(g1, limit=1)".to_string(),
            target: "report:in_app".to_string(),
            transform: ConsolidationTransform::Structured {
                builtin: BuiltinTransform::MapEpisodeToTask,
            },
        }];

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("target `report:in_app` is unsupported for cycle_completed structured transforms in phase 3"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn validation_rejects_duplicate_truncation_priority() {
        let mut definition = valid_definition();
        definition.prompt_pipeline = Some(PromptPipelineConfig {
            sections: vec![PromptSection {
                name: "persona".to_string(),
                source: "definition.persona".to_string(),
                content: None,
                required: false,
                condition: None,
                format: None,
                filter: None,
            }],
            output_rules: PromptOutputRules {
                max_context_tokens: 6000,
                truncation_priority: vec!["persona".to_string(), "persona".to_string()],
            },
        });

        let err = definition.validate().unwrap_err();
        match err {
            AgentDefinitionError::Validation(msg) => {
                assert!(msg.contains("truncation_priority has duplicate entry"));
            },
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn agent_definition_principal_defaults_to_none() {
        // YAML without `principal` should deserialize with principal == None.
        let yaml = r#"
agent_id: "test-agent"
version: 1
name: "Test"
persona: "A test agent"
"#;
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.principal, None);
    }

    #[test]
    fn agent_definition_principal_roundtrips() {
        // When principal is set it survives a JSON round-trip.
        let mut def = valid_definition();
        def.principal = Some("service-account:my-agent".to_string());
        let json = serde_json::to_string(&def).unwrap();
        let restored: AgentDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored.principal,
            Some("service-account:my-agent".to_string())
        );
    }

    // ── Phase 2 validate_agent_definition tests ─────────────────────────

    #[test]
    fn test_agent_kind_defaults_to_personal() {
        let yaml = "agent_id: test\nname: Test\npersona: test\ntrust_level: standard";
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.kind, AgentKind::Personal);
    }

    #[test]
    fn test_personal_allows_autonomous_config() {
        let def = AgentDefinition {
            kind: AgentKind::Personal,
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![FocusArea {
                    name: "inbox".to_string(),
                    description: "Process inbox".to_string(),
                    priority: FocusAreaPriority::High,
                    schedule: None,
                    program: None,
                    scope: None,
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            ..test_agent_definition()
        };
        assert!(validate_agent_definition(&def).is_ok());
    }

    #[test]
    fn test_autonomous_config_requires_at_least_one_focus_area() {
        let def = AgentDefinition {
            kind: AgentKind::Personal,
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            ..test_agent_definition()
        };
        let err = validate_agent_definition(&def).unwrap_err();
        assert!(err.iter().any(|e| e.contains("focus_area")));
    }

    #[test]
    fn test_autonomous_config_requires_valid_cron() {
        let def = AgentDefinition {
            kind: AgentKind::Personal,
            autonomous_config: Some(AutonomousConfig {
                schedule: "not-a-cron".to_string(),
                focus_areas: vec![FocusArea {
                    name: "test".to_string(),
                    description: "test".to_string(),
                    priority: FocusAreaPriority::Medium,
                    schedule: None,
                    program: None,
                    scope: None,
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            ..test_agent_definition()
        };
        let err = validate_agent_definition(&def).unwrap_err();
        assert!(err
            .iter()
            .any(|e| e.contains("cron") || e.contains("schedule")));
    }

    #[test]
    fn test_worker_rejects_autonomous_config() {
        let def = AgentDefinition {
            kind: AgentKind::Worker,
            tools: vec!["browser".to_string()],
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![FocusArea {
                    name: "test".to_string(),
                    description: "test".to_string(),
                    priority: FocusAreaPriority::Medium,
                    schedule: None,
                    program: None,
                    scope: None,
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            ..test_agent_definition()
        };
        let err = validate_agent_definition(&def).unwrap_err();
        assert!(err
            .iter()
            .any(|e| e.contains("Worker") && e.contains("autonomous_config")));
    }

    #[test]
    fn test_focus_area_invalid_per_area_schedule_rejected() {
        let def = AgentDefinition {
            kind: AgentKind::Personal,
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![FocusArea {
                    name: "bad-area".to_string(),
                    description: "has invalid schedule".to_string(),
                    priority: FocusAreaPriority::Medium,
                    schedule: Some("not-a-cron".to_string()),
                    program: None,
                    scope: None,
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            ..test_agent_definition()
        };
        let err = validate_agent_definition(&def).unwrap_err();
        assert!(err
            .iter()
            .any(|e| e.contains("focus_area") && e.contains("bad-area")));
    }

    #[test]
    fn test_focus_area_valid_per_area_schedule_accepted() {
        let def = AgentDefinition {
            kind: AgentKind::Personal,
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![FocusArea {
                    name: "good-area".to_string(),
                    description: "has valid schedule".to_string(),
                    priority: FocusAreaPriority::Medium,
                    schedule: Some("0 * * * *".to_string()),
                    program: None,
                    scope: None,
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            ..test_agent_definition()
        };
        assert!(validate_agent_definition(&def).is_ok());
    }

    #[test]
    fn test_worker_requires_tools() {
        let def = AgentDefinition {
            kind: AgentKind::Worker,
            tools: vec![],
            ..test_agent_definition()
        };
        let err = validate_agent_definition(&def).unwrap_err();
        assert!(err.iter().any(|e| e.contains("tools")));
    }

    #[test]
    fn test_delegation_targets_defaults_to_empty() {
        let yaml = "agent_id: test\nname: Test\npersona: test\ntrust_level: standard";
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert!(
            def.delegation_targets.is_empty(),
            "delegation_targets should default to empty"
        );
    }

    #[test]
    fn disabled_defaults_to_false() {
        let yaml = "agent_id: test\nname: Test\npersona: test\ntrust_level: standard";
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert!(!def.disabled);
    }

    #[test]
    fn disabled_agent_hierarchy_includes_reachable_descendants() {
        let mut root = valid_definition();
        root.agent_id = "root".to_string();
        root.disabled = true;
        root.delegation_targets = vec!["child".to_string()];

        let mut child = valid_definition();
        child.agent_id = "child".to_string();
        child.kind = AgentKind::Worker;
        child.delegation_targets = vec!["grandchild".to_string()];

        let mut grandchild = valid_definition();
        grandchild.agent_id = "grandchild".to_string();
        grandchild.kind = AgentKind::Worker;

        let mut sibling = valid_definition();
        sibling.agent_id = "sibling".to_string();

        let definitions = [root, child, grandchild, sibling];
        let disabled = disabled_agent_hierarchy(definitions.iter());

        assert!(disabled.contains("root"));
        assert!(disabled.contains("child"));
        assert!(disabled.contains("grandchild"));
        assert!(!disabled.contains("sibling"));
    }

    #[test]
    fn disabled_agent_hierarchy_keeps_shared_delegate_reachable_from_enabled_root() {
        let mut disabled_root = valid_definition();
        disabled_root.agent_id = "cmo".to_string();
        disabled_root.disabled = true;
        disabled_root.delegation_targets = vec!["web-researcher".to_string()];

        let mut enabled_root = valid_definition();
        enabled_root.agent_id = "ceo".to_string();
        enabled_root.delegation_targets = vec!["web-researcher".to_string()];

        let mut web_researcher = valid_definition();
        web_researcher.agent_id = "web-researcher".to_string();
        web_researcher.kind = AgentKind::Worker;

        let definitions = [disabled_root, enabled_root, web_researcher];
        let disabled = disabled_agent_hierarchy(definitions.iter());

        assert!(disabled.contains("cmo"));
        assert!(!disabled.contains("ceo"));
        assert!(!disabled.contains("web-researcher"));
    }

    #[test]
    fn disabled_agent_hierarchy_keeps_shared_delegate_descendants_reachable() {
        let mut disabled_root = valid_definition();
        disabled_root.agent_id = "cmo".to_string();
        disabled_root.disabled = true;
        disabled_root.delegation_targets = vec!["web-researcher".to_string()];

        let mut enabled_root = valid_definition();
        enabled_root.agent_id = "ceo".to_string();
        enabled_root.delegation_targets = vec!["web-researcher".to_string()];

        let mut web_researcher = valid_definition();
        web_researcher.agent_id = "web-researcher".to_string();
        web_researcher.kind = AgentKind::Worker;
        web_researcher.delegation_targets = vec!["source-reader".to_string()];

        let mut source_reader = valid_definition();
        source_reader.agent_id = "source-reader".to_string();
        source_reader.kind = AgentKind::Worker;

        let definitions = [disabled_root, enabled_root, web_researcher, source_reader];
        let disabled = disabled_agent_hierarchy(definitions.iter());

        assert!(disabled.contains("cmo"));
        assert!(!disabled.contains("web-researcher"));
        assert!(!disabled.contains("source-reader"));
    }

    #[test]
    fn test_focus_area_priority_defaults_to_medium() {
        let fa: FocusArea = serde_yaml::from_str("name: test\ndescription: test desc").unwrap();
        assert_eq!(fa.priority, FocusAreaPriority::Medium);
    }

    #[test]
    fn test_is_primary_defaults_to_false() {
        let yaml = "agent_id: test\nname: Test\npersona: test\ntrust_level: standard";
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert!(!def.is_primary);
    }

    #[test]
    fn test_user_memory_isolation_defaults_to_shared() {
        let yaml = "agent_id: test\nname: Test\npersona: test\ntrust_level: standard";
        let def: AgentDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(def.user_memory_isolation, UserMemoryIsolation::Shared);
    }

    #[test]
    fn test_worker_rejects_is_primary() {
        let def = AgentDefinition {
            kind: AgentKind::Worker,
            tools: vec!["browser".to_string()],
            is_primary: true,
            ..test_agent_definition()
        };
        let err = validate_agent_definition(&def).unwrap_err();
        assert!(err
            .iter()
            .any(|e| e.contains("is_primary") || e.contains("primary")));
    }

    // ── Phase 4.5: default memory injection tests ─────────────────────────

    fn autonomous_personal_agent() -> AgentDefinition {
        AgentDefinition {
            kind: AgentKind::Personal,
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![FocusArea {
                    name: "inbox".to_string(),
                    description: "Process inbox".to_string(),
                    priority: FocusAreaPriority::High,
                    schedule: None,
                    program: None,
                    scope: None,
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            memory_tiers: Vec::new(),
            memory_consolidation: Vec::new(),
            retention: None,
            ..test_agent_definition()
        }
    }

    #[test]
    fn test_personal_agent_with_autonomous_config_gets_defaults() {
        let mut def = autonomous_personal_agent();
        assert!(def.memory_tiers.is_empty());
        assert!(def.memory_consolidation.is_empty());

        def.apply_defaults();

        assert_eq!(def.memory_tiers.len(), 6, "should inject 6 default tiers");
        assert_eq!(
            def.memory_consolidation.len(),
            9,
            "should inject 9 default rules"
        );
    }

    #[test]
    fn test_personal_agent_explicit_config_not_overridden() {
        let explicit_tier = MemoryTierDefinition {
            name: "custom_tier".to_string(),
            scope: TierScope::Agent,
            description: "Custom user-defined tier".to_string(),
            schema: BTreeMap::from([("data".to_string(), TierFieldSchema::Text {})]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{data}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let mut def = AgentDefinition {
            memory_tiers: vec![explicit_tier],
            ..autonomous_personal_agent()
        };

        def.apply_defaults();

        assert_eq!(
            def.memory_tiers.len(),
            1,
            "explicit tiers must not be overridden"
        );
        assert_eq!(def.memory_tiers[0].name, "custom_tier");
        assert!(
            def.memory_consolidation.is_empty(),
            "explicit config prevents rule injection too"
        );
    }

    #[test]
    fn test_worker_agent_no_defaults() {
        let def = AgentDefinition {
            kind: AgentKind::Worker,
            tools: vec!["browser".to_string()],
            autonomous_config: Some(AutonomousConfig {
                schedule: "0 */4 * * *".to_string(),
                focus_areas: vec![FocusArea {
                    name: "test".to_string(),
                    description: "test".to_string(),
                    priority: FocusAreaPriority::Medium,
                    schedule: None,
                    program: None,
                    scope: None,
                }],
                max_tasks_per_cycle: 3,
                max_steps_per_plan: 10,
            }),
            ..test_agent_definition()
        };

        // Workers with autonomous_config should fail validation.
        let err = validate_agent_definition(&def).unwrap_err();
        assert!(err
            .iter()
            .any(|e| e.contains("Worker") && e.contains("autonomous_config")));
    }

    #[test]
    fn test_default_consolidate_before_delete_true() {
        let mut def = autonomous_personal_agent();
        def.apply_defaults();

        let retention = def.retention.expect("retention should be injected");
        assert!(
            retention.episodes.consolidate_before_delete,
            "default episode retention must have consolidate_before_delete = true"
        );
        assert_eq!(
            retention.episodes.default_days, 7,
            "default episode retention days should be 7 (168 hours)"
        );
    }

    #[test]
    fn test_personal_agent_without_autonomous_config_gets_defaults() {
        let mut def = AgentDefinition {
            autonomous_config: None,
            memory_tiers: vec![],
            memory_consolidation: vec![],
            ..autonomous_personal_agent()
        };
        assert!(def.memory_tiers.is_empty());

        def.apply_defaults();

        assert_eq!(
            def.memory_tiers.len(),
            6,
            "non-autonomous personal agents should also get default tiers"
        );
        assert_eq!(
            def.memory_consolidation.len(),
            9,
            "non-autonomous personal agents should also get default rules"
        );
    }

    #[test]
    fn test_default_prompt_pipeline_surfaces_goal_scoped_task_progress() {
        let mut def = autonomous_personal_agent();

        def.apply_defaults();

        let pipeline = def
            .prompt_pipeline
            .as_ref()
            .expect("default prompt pipeline should be injected");
        let section = pipeline
            .sections
            .iter()
            .find(|section| section.name == "task_progress")
            .expect("task_progress prompt section should be present");

        assert_eq!(section.source, "memory.tier[task_progress].context_summary");
        assert!(!section.required);
        assert!(pipeline
            .output_rules
            .truncation_priority
            .contains(&"task_progress".to_string()));
    }
}
