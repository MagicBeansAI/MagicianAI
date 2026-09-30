//! Canonical effective tool-policy resolution.
//!
//! One immutable snapshot is resolved for a provider decision boundary. The
//! provider catalog, dispatch ceiling, deferred index, delegation schema and
//! introspection projection must all be derived from this value. A provider
//! schema is never treated as authorization by itself.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::magician_v2::agents::types::{
    tool_name_matches_block_entry, AgentDefinition, AgentInvocationContext, ApprovalRule,
    FeatureMode, InvocationSurface,
};
use crate::magician_v2::chat::invoke_grammar::parse_vibedev_rail_invocation;
use crate::magician_v2::chat::lane_seam::BRAINSTORM_FACILITATION_AGENT_ID;

use super::native_types::NativeExecutionTool;

const SNAPSHOT_CACHE_LIMIT: usize = 256;
const STRUCTURAL_TOOL_NAMES: &[&str] = &[
    "spawn_sub_goal",
    "delegate_to_agent",
    "handover_to_agent",
    "orchestrate_pipeline",
    "yield",
    "need_user_input",
    "goal_reached",
    "cannot_proceed",
    "delegate_to_chat",
];

/// Serialize policy-bearing data with recursively sorted object keys.
///
/// `AgentDefinition` and tool schemas contain maps whose iteration order may
/// differ between processes. Their semantic value is unchanged, so cache and
/// snapshot identities must not depend on that incidental order. Arrays stay
/// ordered because provider catalog order is part of the actual model input.
fn canonical_json_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    fn canonicalize(value: Value) -> Value {
        match value {
            Value::Object(values) => {
                let mut entries = values.into_iter().collect::<Vec<_>>();
                entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
                let mut canonical = serde_json::Map::new();
                for (key, value) in entries {
                    canonical.insert(key, canonicalize(value));
                }
                Value::Object(canonical)
            },
            Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
            scalar => scalar,
        }
    }

    serde_json::to_vec(&canonicalize(serde_json::to_value(value)?))
}

/// Stable identity for policy-bearing agent definitions.
///
/// Definition structs contain hash maps, so hashing a direct serde encoding can
/// produce a different digest each time the same YAML is deserialized. Every
/// authorization boundary must share this canonical digest; otherwise a valid
/// tool call can be rejected as a policy change even though no policy changed.
pub fn canonical_definition_digest(
    definition: &AgentDefinition,
) -> Result<String, serde_json::Error> {
    canonical_json_bytes(definition).map(|bytes| blake3::hash(&bytes).to_hex().to_string())
}

/// Typed feature markers are accepted only as the first token of a trusted
/// lane request. Incidental handles, email addresses, quoted source text and
/// graph/webpage content therefore cannot activate a protected feature.
///
/// **`@vibedev` is deliberately absent from the match below and delegated to
/// [`crate::magician_v2::chat::invoke_grammar::parse_vibedev_rail_invocation`]
/// instead.** The split here breaks on whitespace/`:`/`,` only, so
/// `@vibedev-review this diff` arrives as the single token `@vibedev-review` —
/// safe — but the rail also has to carry its own `#discuss` flag and hand the
/// caller a verbatim prompt, which this function has no shape for. Listing
/// `"@vibedev"` here as well would create a SECOND leading-marker decision
/// for the same marker, and two such decisions that disagree on one input is
/// precisely how a turn gets classified as a build and then parsed as
/// something else. So the rail's parser is the only judge of `@vibedev`, and
/// this function reports what it decided.
///
/// That delegation is also why the rail's SPOKEN invoke ("start a vibedev build
/// …") needed nothing here: it is a leading invoke like any other, and the one
/// parser that reads it already answers for both spellings.
///
/// The marker spellings in the match below are deliberately inline literals.
/// `magician/tests/invoke_grammar_agreement.rs` pins their exact match-arm
/// syntax in this file and cross-pins the same spellings to the exported
/// constants the published catalog is generated from
/// (`tutor::TUTOR_MARKER_INVOKE_WORDS`, `tutor::APP_COPILOT_MARKER_INVOKE_WORDS`,
/// `chat::invoke_catalog::BRAINSTORM_MARKER_INVOKE_WORDS`). A match arm cannot
/// read from those constants without moving a pinned literal out of its pinned
/// shape, so for this table the frozen oracle is the single-sourcing join —
/// do not re-express the arms as constants. The spoken arms of
/// [`parse_leading_feature_invocation`] are pinned the same way, and the
/// in-file tests below pin constants → parse outcomes at unit scope.
pub fn parse_leading_feature_marker(text: &str) -> FeatureMode {
    let trimmed = text.trim_start();
    let Some(first) = trimmed
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ':' | ','))
        .next()
        .filter(|token| !token.is_empty())
    else {
        return FeatureMode::None;
    };
    match first.to_ascii_lowercase().as_str() {
        "@tutor" | "@tutur" => FeatureMode::Tutor,
        "@copilot" | "@appcopilot" | "@app-copilot" | "@app_copilot" => FeatureMode::AppCopilot,
        "@brainstorm" => FeatureMode::Brainstorm,
        // Reached only when no named marker opened the turn, so the VibeDev rail
        // can never shadow one of them (none of their spellings is `@vibedev`).
        _ if parse_vibedev_rail_invocation(text).is_some() => FeatureMode::Vibedev,
        _ => FeatureMode::None,
    }
}

fn matches_leading_spoken_command(text: &str, words: &[&str]) -> bool {
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let mut cursor = 0;
    for (index, expected) in words.iter().enumerate() {
        if index > 0 {
            let separator_start = cursor;
            while cursor < bytes.len()
                && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b',')
            {
                cursor += 1;
            }
            if cursor == separator_start {
                return false;
            }
        }
        let Some(candidate) = text.get(cursor..cursor.saturating_add(expected.len())) else {
            return false;
        };
        if !candidate.eq_ignore_ascii_case(expected) {
            return false;
        }
        cursor += expected.len();
    }
    text.get(cursor..).is_some_and(|remainder| {
        remainder.is_empty()
            || remainder
                .chars()
                .next()
                .is_some_and(|ch| ch.is_whitespace() || matches!(ch, ':' | ','))
    })
}

/// Boundary-safe spoken/typed lane invocation parser. Unlike the legacy
/// Tutor intent helpers this is suitable for authorization: the command must
/// begin the utterance, and longer handles or incidental later mentions do
/// not match.
///
/// The spoken windows are inline slices for the same reason the marker arms
/// of [`parse_leading_feature_marker`] are inline literals: the agreement
/// test pins two of these call sites (`&["hey", "tutor"]` and
/// `&["hey", "app", "copilot"]`) verbatim and cross-pins the words to the
/// catalog constants (`SPOKEN_WAKE_WORD`, `tutor::TUTOR_SPOKEN_INVOKE_WORDS`,
/// `tutor::APP_COPILOT_SPOKEN_INVOKE_WORDS`, `APP_COPILOT_SPOKEN_PREFIX_WORD`).
/// Deriving only the unpinned arms from constants would leave every spoken
/// word with two live spellings (constant plus pinned arm) instead of one,
/// so the table stays inline and oracle-pinned.
pub fn parse_leading_feature_invocation(text: &str) -> FeatureMode {
    let marker = parse_leading_feature_marker(text);
    if marker != FeatureMode::None {
        return marker;
    }
    if matches_leading_spoken_command(text, &["hey", "tutor"])
        || matches_leading_spoken_command(text, &["hey", "tutur"])
    {
        FeatureMode::Tutor
    } else if matches_leading_spoken_command(text, &["hey", "copilot"])
        || matches_leading_spoken_command(text, &["hey", "app", "copilot"])
    {
        FeatureMode::AppCopilot
    } else {
        FeatureMode::None
    }
}

/// Which [`InvocationSurface`] a [`FeatureMode`] may travel on — the lane
/// admission matrix.
///
/// Deliberately an exhaustive match, not a data table. A new `FeatureMode`
/// variant cannot compile without a pairing decision right here, which is
/// the fail-closed guard an authorizer wants: a forgotten row in a const
/// table would instead silently deny the new lane on every surface at
/// runtime. The `None` arm is policy of its own — the complement of the
/// conversational product surfaces — and is not derivable from per-lane
/// rows (Chat is Vibedev-admitted yet `None`-admitted), so a row-shaped
/// table could not have replaced this match without a second hand-maintained
/// constant anyway. Batch 7 fold unit F10 judged this a stay.
pub fn feature_surface_is_authorized(feature: FeatureMode, surface: InvocationSurface) -> bool {
    match feature {
        FeatureMode::None => !matches!(
            surface,
            InvocationSurface::ThinkingMap
                | InvocationSurface::Tutor
                | InvocationSurface::AppCopilot
        ),
        FeatureMode::Tutor => surface == InvocationSurface::Tutor,
        FeatureMode::AppCopilot => surface == InvocationSurface::AppCopilot,
        FeatureMode::Brainstorm => surface == InvocationSurface::ThinkingMap,
        // The VibeDev rail owns no surface of its own: it is typed or spoken into
        // an ordinary conversation and hands the work to a task. Those are the
        // two places the lane can legitimately appear.
        //
        // `RealtimeVoice` is the *server-minted* label for a hands-free voice
        // turn — `rejects_client_selected_protected_surface` in `chat_api`
        // refuses it from a client body — so admitting it here widens the rail to
        // one trusted transport, not to anything that can claim a name. Every
        // other surface stays closed: a Tutor or App Copilot turn is a different
        // product lane, a task is already an agent doing work, and
        // `PublicEnvoy` is a stranger.
        FeatureMode::Vibedev => matches!(
            surface,
            InvocationSurface::Chat | InvocationSurface::RealtimeVoice
        ),
    }
}

/// Dedicated feature-to-agent binding. This is intentionally the only generic
/// code that binds a feature mode to an agent id, and it owns no spelling of
/// its own: the Brainstorm binding is the lane seam's
/// [`BRAINSTORM_FACILITATION_AGENT_ID`] — one constant, shared with the
/// session-admission path that mints thinking-map sessions (batch 7 fold
/// unit F13 removed this function's private duplicate of the literal).
pub fn feature_agent_id(feature: FeatureMode) -> Option<&'static str> {
    match feature {
        FeatureMode::Brainstorm => Some(BRAINSTORM_FACILITATION_AGENT_ID),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveToolKind {
    Direct,
    Deferred,
    Runtime,
    Structural,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectiveToolGrant {
    pub name: String,
    pub kind: EffectiveToolKind,
    pub provider_visible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectiveStructuralGrant {
    pub name: String,
    pub allowed_targets: BTreeSet<String>,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectiveDelegationTarget {
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectiveToolPolicySnapshot {
    pub snapshot_id: String,
    pub agent_id: String,
    pub definition_version: u32,
    pub definition_digest: String,
    pub invocation: AgentInvocationContext,
    pub trust_level: String,
    pub direct_tools: BTreeMap<String, EffectiveToolGrant>,
    pub deferred_tools: BTreeMap<String, EffectiveToolGrant>,
    pub runtime_tools: BTreeMap<String, EffectiveToolGrant>,
    /// Server-side actions that are authorized for orchestration metadata but
    /// are never advertised as provider-callable tools (currently task state).
    pub implicit_tools: BTreeSet<String>,
    pub structural_tools: BTreeMap<String, EffectiveStructuralGrant>,
    pub delegation_targets: BTreeMap<String, EffectiveDelegationTarget>,
    pub handover_targets: BTreeMap<String, EffectiveDelegationTarget>,
    pub denied_tool_names: BTreeSet<String>,
    pub denied_tool_params: HashMap<String, HashMap<String, Vec<String>>>,
    pub approval_rules: Vec<ApprovalRule>,
    pub provider_specs: Vec<NativeExecutionTool>,
    pub dispatch_tool_names: BTreeSet<String>,
    pub delegate_owned_tool_names: BTreeSet<String>,
    /// The engagement this snapshot narrowed under: the engagement id plus the
    /// LIVE authority revision the projection was intersected with (§4.2c).
    /// `None` when the execution carries no engagement. Recording the pair is
    /// what lets the dispatch boundary observe staleness (row 9): a revoke or
    /// narrow bumps the store's revision, so a snapshot recorded at revision N
    /// is detectably stale afterwards.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engagement_authority: Option<crate::magician_v2::engagements::EngagementAuthorityRef>,
}

impl EffectiveToolPolicySnapshot {
    pub fn permits_tool(&self, tool_name: &str) -> bool {
        self.dispatch_tool_names.contains(tool_name)
            && !self.delegate_owned_tool_names.contains(tool_name)
            && !self.denied_tool_names.contains(tool_name)
    }

    pub fn permits_delegation_target(&self, agent_id: &str) -> bool {
        self.structural_tools
            .get("delegate_to_agent")
            .is_some_and(|grant| grant.allowed_targets.contains(agent_id))
    }

    pub fn permits_handover_target(&self, agent_id: &str) -> bool {
        self.structural_tools
            .get("handover_to_agent")
            .is_some_and(|grant| grant.allowed_targets.contains(agent_id))
    }

    pub fn permits_implicit_tool(&self, tool_name: &str) -> bool {
        self.implicit_tools.contains(tool_name)
            && !self.delegate_owned_tool_names.contains(tool_name)
            && !self.denied_tool_names.contains(tool_name)
    }

    pub fn visible_now(&self) -> impl Iterator<Item = &str> {
        self.provider_specs.iter().map(|tool| tool.name.as_str())
    }
}

fn log_policy_snapshot(snapshot: &EffectiveToolPolicySnapshot, cache_hit: bool) {
    let provider_schema_bytes = serde_json::to_vec(&snapshot.provider_specs)
        .map(|bytes| bytes.len())
        .unwrap_or_default();
    debug!(
        snapshot_id = %snapshot.snapshot_id,
        definition_digest = %snapshot.definition_digest,
        principal = %snapshot.invocation.principal,
        workspace = %snapshot.invocation.workspace,
        source_agent_id = snapshot.invocation.source_agent_id.as_deref().unwrap_or("<direct>"),
        target_agent_id = %snapshot.agent_id,
        surface = %snapshot.invocation.surface.as_str(),
        feature_mode = %snapshot.invocation.feature_mode.as_str(),
        trust_level = %snapshot.trust_level,
        direct_tool_count = snapshot.direct_tools.len(),
        deferred_tool_count = snapshot.deferred_tools.len(),
        runtime_tool_count = snapshot.runtime_tools.len(),
        structural_tool_count = snapshot.structural_tools.len(),
        provider_tool_count = snapshot.provider_specs.len(),
        provider_schema_bytes,
        delegation_target_count = snapshot.delegation_targets.len(),
        handover_target_count = snapshot.handover_targets.len(),
        approval_rule_count = snapshot.approval_rules.len(),
        engagement_id = snapshot
            .engagement_authority
            .as_ref()
            .map(|engagement| engagement.engagement_id.as_str())
            .unwrap_or("<none>"),
        cache_hit,
        "[TOOL-POLICY] effective policy snapshot resolved"
    );
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct SnapshotCacheKey {
    principal: String,
    workspace: String,
    agent_id: String,
    source_agent_id: Option<String>,
    source_kind: crate::magician_v2::agents::InvocationSourceKind,
    definition_digest: String,
    catalog_digest: String,
    surface: InvocationSurface,
    feature_mode: FeatureMode,
    /// §4.2c row 9: a revoke/narrow bumps the engagement's authority revision,
    /// so a snapshot resolved before the mutation carries a different key and
    /// a stale pre-narrow snapshot can never be served from cache. The
    /// resolved engagement *content* (ceiling/team) is additionally part of
    /// `catalog_digest`, which covers the case the revision pair alone cannot:
    /// a denial (expiry, revocation, missing store) yields a fail-closed empty
    /// ceiling while still carrying the execution's grant-time revision, which
    /// may equal the revision a live full-ceiling snapshot was cached under.
    engagement_id: Option<String>,
    engagement_revision: Option<u64>,
}

static SNAPSHOT_CACHE: OnceLock<
    Mutex<HashMap<SnapshotCacheKey, Arc<EffectiveToolPolicySnapshot>>>,
> = OnceLock::new();

/// The engagement authority a snapshot narrows to (§4.2c layer 1) — the LIVE
/// authority resolved by the caller immediately before resolution, never the
/// (possibly stale) ref the execution carries. The snapshot intersects its
/// projection with `tool_ceiling`/`team` and records which
/// `(engagement_id, authority_revision)` it narrowed to.
///
/// An empty `tool_ceiling` together with an empty `team` is the fail-closed
/// shape: the caller could not obtain live authority (unknown / revoked /
/// expired / store missing), so the projection advertises no engagement-scoped
/// business tools and no delegation targets rather than the un-narrowed static
/// grant.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EngagementSnapshotInput {
    pub engagement_id: String,
    /// The store revision the ceiling/team were read at (LIVE, per the doc
    /// above). Named inside an `Engagement*` type on purpose: a bare
    /// `authority_revision` field on a runtime-binding struct would collide
    /// with `AutonomousSurfaceRuntimeBinding`'s unrelated working-set field.
    pub authority_revision: u64,
    pub tool_ceiling: std::collections::BTreeSet<String>,
    pub team: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SnapshotResolutionInput {
    pub provider_tools: Vec<NativeExecutionTool>,
    pub runtime_tool_names: BTreeSet<String>,
    pub deferred_tool_names: BTreeSet<String>,
    pub direct_reachable_tool_names: BTreeSet<String>,
    pub delegate_owned_tool_names: BTreeSet<String>,
    pub delegation_target_ids: Vec<String>,
    pub handover_target_ids: Vec<String>,
    /// Non-provider actions emitted as bounded decision metadata.
    pub implicit_tool_names: BTreeSet<String>,
    /// Exact approval rules used at dispatch, including harness-owned rules.
    pub effective_approval_rules: Option<Vec<ApprovalRule>>,
    /// Digest of external policy inputs such as scoped trust-policy YAML.
    pub external_policy_digest: Option<String>,
    /// Current runtime feasibility for sub-goal creation. `None` is used by
    /// non-autonomous surfaces that do not carry depth/iteration state.
    pub spawn_sub_goal_allowed: Option<bool>,
    /// Engagement narrowing (§4.2c layer 1): the LIVE authority resolved by
    /// the caller immediately before resolution. When present, the snapshot
    /// narrows every projected collection to the engagement's ceiling/team and
    /// records which revision it narrowed to.
    pub engagement: Option<EngagementSnapshotInput>,
}

fn constrain_structural_target_schema(
    tool: &mut NativeExecutionTool,
    allowed_targets: &BTreeSet<String>,
) -> bool {
    let values = allowed_targets
        .iter()
        .cloned()
        .map(serde_json::Value::String)
        .collect::<Vec<_>>();
    let pointer = match tool.name.as_str() {
        "delegate_to_agent" => "/properties/delegation_targets/items/properties/target_agent_id",
        "handover_to_agent" => "/properties/target_agent_id",
        _ => return true,
    };
    let Some(target_schema) = tool
        .parameters
        .pointer_mut(pointer)
        .and_then(serde_json::Value::as_object_mut)
    else {
        // An unexpected structural schema must not be advertised without its
        // exact target ceiling. Dispatch still enforces membership, but the
        // provider projection is required to agree with that boundary.
        return false;
    };
    target_schema.insert("enum".to_string(), serde_json::Value::Array(values));
    true
}

/// Validate only the typed owner/surface binding. Entry points use this before
/// any provider or owner-transition side effect; full snapshot resolution then
/// narrows the concrete catalog for the decision boundary.
pub fn validate_agent_invocation(
    definition: &AgentDefinition,
    invocation: &AgentInvocationContext,
) -> Result<(), String> {
    if definition.disabled {
        return Err(format!("definition_disabled:{}", definition.agent_id));
    }
    if definition.agent_id != invocation.target_agent_id {
        return Err("invocation_target_definition_mismatch".to_string());
    }
    if !definition
        .invocation_policy
        .permits_direct_surface(invocation.surface)
    {
        return Err(format!(
            "agent_not_surface_eligible:{}:{}",
            definition.agent_id,
            invocation.surface.as_str()
        ));
    }
    if !feature_surface_is_authorized(invocation.feature_mode, invocation.surface) {
        return Err("feature_surface_mismatch".to_string());
    }
    if let Some(feature_agent_id) = feature_agent_id(invocation.feature_mode) {
        if feature_agent_id != definition.agent_id {
            return Err("feature_agent_binding_mismatch".to_string());
        }
    }
    Ok(())
}

pub fn resolve_effective_tool_policy_snapshot(
    definition: &AgentDefinition,
    invocation: AgentInvocationContext,
    input: SnapshotResolutionInput,
) -> Result<Arc<EffectiveToolPolicySnapshot>, String> {
    validate_agent_invocation(definition, &invocation)?;

    let definition_digest = canonical_definition_digest(definition)
        .map_err(|error| format!("definition_digest_failed:{error}"))?;
    let effective_approval_rules = input.effective_approval_rules.clone().unwrap_or_else(|| {
        crate::magician_v2::agents::approval::harness_merged_approval_rules(definition)
    });
    let external_policy_digest = input.external_policy_digest.clone().unwrap_or_default();
    // The cache identity includes every dynamic authorization input, not only
    // provider schemas. In particular a target becoming disabled or losing
    // wildcard eligibility must invalidate a previously cached delegation
    // grant even when the caller's definition and tool catalog are unchanged.
    let catalog_bytes = canonical_json_bytes(&serde_json::json!({
        "provider_tools": &input.provider_tools,
        "runtime_tools": &input.runtime_tool_names,
        "deferred_tools": &input.deferred_tool_names,
        "direct_reachable_tools": &input.direct_reachable_tool_names,
        "delegate_owned_tools": &input.delegate_owned_tool_names,
        "delegation_targets": &input.delegation_target_ids,
        "handover_targets": &input.handover_target_ids,
        "implicit_tools": &input.implicit_tool_names,
        "effective_approval_rules": &effective_approval_rules,
        "external_policy_digest": &external_policy_digest,
        "spawn_sub_goal_allowed": &input.spawn_sub_goal_allowed,
        // The resolved engagement content, not just its (id, revision) pair:
        // see the field comment on `SnapshotCacheKey.engagement_id` for why
        // the fail-closed denial shape needs content in the identity.
        "engagement": &input.engagement,
    }))
    .map_err(|error| format!("catalog_digest_failed:{error}"))?;
    let catalog_digest = blake3::hash(&catalog_bytes).to_hex().to_string();
    let cache_key = SnapshotCacheKey {
        principal: invocation.principal.clone(),
        workspace: invocation.workspace.clone(),
        agent_id: definition.agent_id.clone(),
        source_agent_id: invocation.source_agent_id.clone(),
        source_kind: invocation.source_kind,
        definition_digest: definition_digest.clone(),
        catalog_digest: catalog_digest.clone(),
        surface: invocation.surface,
        feature_mode: invocation.feature_mode,
        engagement_id: input
            .engagement
            .as_ref()
            .map(|engagement| engagement.engagement_id.clone()),
        engagement_revision: input
            .engagement
            .as_ref()
            .map(|engagement| engagement.authority_revision),
    };
    let cache = SNAPSHOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(snapshot) = cache
        .lock()
        .map_err(|_| "snapshot_cache_poisoned".to_string())?
        .get(&cache_key)
        .cloned()
    {
        if snapshot.invocation == invocation {
            log_policy_snapshot(&snapshot, true);
            return Ok(snapshot);
        }
        // Session/turn identifiers are audit provenance rather than policy
        // inputs. Reuse the computed authorization projection and stable
        // snapshot id, but never return the prior turn's provenance.
        let mut current_turn_snapshot = (*snapshot).clone();
        current_turn_snapshot.invocation = invocation;
        log_policy_snapshot(&current_turn_snapshot, true);
        return Ok(Arc::new(current_turn_snapshot));
    }

    let denied_tool_names = definition
        .denied_tools
        .iter()
        .chain(definition.excluded_tools.iter())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>();
    let is_surface_only = matches!(
        definition.invocation_policy.discoverability,
        crate::magician_v2::agents::AgentDiscoverability::SurfaceOnly
    );
    let is_denied = |name: &str| {
        denied_tool_names.iter().any(|entry| {
            tool_name_matches_block_entry(name, entry)
                || (entry.trim() == "orchestrator" && STRUCTURAL_TOOL_NAMES.contains(&name))
        })
    };
    let is_explicitly_granted = |name: &str| {
        definition
            .tools
            .iter()
            .any(|grant| tool_name_matches_block_entry(name, grant))
    };

    let implicit_tools = input
        .implicit_tool_names
        .iter()
        .filter(|name| {
            !matches!(invocation.surface, InvocationSurface::PublicEnvoy)
                && input.direct_reachable_tool_names.contains(*name)
                && !input.delegate_owned_tool_names.contains(*name)
                && (!is_surface_only || is_explicitly_granted(name))
                && !denied_tool_names
                    .iter()
                    .any(|denied| tool_name_matches_block_entry(name.as_str(), denied.as_str()))
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    let feature_allows = |name: &str| {
        if name == "screen-draw" || name == "screen_draw" {
            return matches!(
                invocation.feature_mode,
                FeatureMode::Tutor | FeatureMode::AppCopilot
            ) && matches!(
                invocation.surface,
                InvocationSurface::Tutor | InvocationSurface::AppCopilot
            );
        }
        true
    };

    // Engagement narrowing (§4.2c layer 1). It applies at the choke points
    // every later derivation already flows through — the provider/deferred
    // admission filters below and the effective target-id computation — so
    // `dispatch_tool_names`, `provider_specs`, the structural grants'
    // `allowed_targets` and their constrained enum schemas all inherit the
    // intersection instead of being patched post-hoc. Structural tools
    // (`yield`, `need_user_input`, `spawn_sub_goal`, …) and `implicit_tools`
    // are runtime-control surfaces, not business capability, and stay outside
    // the tool ceiling; the delegation-bearing structural tools
    // (`delegate_to_agent` / `handover_to_agent` / `orchestrate_pipeline`)
    // are governed by `team` instead — an empty team empties their target
    // sets, and the existing empty-target admission check then removes them
    // from the catalog entirely.
    //
    // NOTE (§4.2c "layer 1 is not the boundary"): this projection-level
    // narrowing is UX — the model never sees what it cannot use. Dispatch
    // enforcement (slice 5) re-checks LIVE engagement authority independently
    // on every consequential action, so nothing here is load-bearing for
    // security.
    let engagement = input.engagement.clone();
    let engagement_permits_tool = |name: &str| {
        engagement
            .as_ref()
            .is_none_or(|engagement| engagement.tool_ceiling.contains(name))
    };
    let engagement_permits_target = |agent_id: &str| {
        engagement
            .as_ref()
            .is_none_or(|engagement| engagement.team.contains(agent_id))
    };

    let mut provider_specs = Vec::new();
    let mut direct_tools = BTreeMap::new();
    let mut runtime_tools = BTreeMap::new();
    let mut structural_tools = BTreeMap::new();
    let effective_delegation_target_ids =
        if !matches!(invocation.surface, InvocationSurface::PublicEnvoy)
            && definition.invocation_policy.permits_explicit_delegation()
        {
            input
                .delegation_target_ids
                .iter()
                .filter(|agent_id| engagement_permits_target(agent_id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
    let delegation_targets = effective_delegation_target_ids
        .iter()
        .map(|agent_id| {
            (
                agent_id.clone(),
                EffectiveDelegationTarget {
                    agent_id: agent_id.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let effective_handover_target_ids =
        if !matches!(invocation.surface, InvocationSurface::PublicEnvoy)
            && definition.invocation_policy.permits_explicit_delegation()
        {
            input
                .handover_target_ids
                .iter()
                .filter(|agent_id| engagement_permits_target(agent_id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
    let handover_targets = effective_handover_target_ids
        .iter()
        .map(|agent_id| {
            (
                agent_id.clone(),
                EffectiveDelegationTarget {
                    agent_id: agent_id.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    for mut tool in input.provider_tools {
        let name = tool.name.clone();
        let is_runtime = input.runtime_tool_names.contains(&name);
        let is_structural = STRUCTURAL_TOOL_NAMES.contains(&name.as_str()) || tool.is_control_tool;
        if is_denied(&name)
            || (name == "spawn_sub_goal" && input.spawn_sub_goal_allowed == Some(false))
            // Implicit tools are server-owned decision metadata. They may be
            // authorized for orchestration without ever becoming provider
            // schemas or dispatchable calls.
            || input.implicit_tool_names.contains(&name)
            || input.delegate_owned_tool_names.contains(&name)
            || !feature_allows(&name)
            || (is_surface_only && !is_explicitly_granted(&name))
            || (!is_runtime && !is_structural && !input.direct_reachable_tool_names.contains(&name))
            // Engagement tool ceiling: business (direct/runtime) capability
            // outside the ceiling is not advertised. Structural runtime
            // controls are exempt here; the delegation-bearing ones are
            // narrowed through their target sets instead.
            || (!is_structural && !engagement_permits_tool(&name))
        {
            continue;
        }
        if matches!(invocation.surface, InvocationSurface::PublicEnvoy) {
            continue;
        }
        let grant = EffectiveToolGrant {
            name: name.clone(),
            kind: if is_runtime {
                EffectiveToolKind::Runtime
            } else if is_structural {
                EffectiveToolKind::Structural
            } else {
                EffectiveToolKind::Direct
            },
            provider_visible: true,
        };
        match grant.kind {
            EffectiveToolKind::Runtime => {
                runtime_tools.insert(name.clone(), grant);
            },
            EffectiveToolKind::Structural => {
                let allowed_targets = match name.as_str() {
                    "delegate_to_agent" | "orchestrate_pipeline" => {
                        delegation_targets.keys().cloned().collect()
                    },
                    "handover_to_agent" => handover_targets.keys().cloned().collect(),
                    _ => BTreeSet::new(),
                };
                if matches!(
                    name.as_str(),
                    "delegate_to_agent" | "handover_to_agent" | "orchestrate_pipeline"
                ) && allowed_targets.is_empty()
                {
                    continue;
                }
                if !constrain_structural_target_schema(&mut tool, &allowed_targets) {
                    continue;
                }
                structural_tools.insert(
                    name.clone(),
                    EffectiveStructuralGrant {
                        name: name.clone(),
                        allowed_targets,
                        requires_approval: crate::magician_v2::agents::approval::structural_action_may_require_approval(
                            &effective_approval_rules,
                            &name,
                        ),
                    },
                );
            },
            _ => {
                direct_tools.insert(name.clone(), grant);
            },
        }
        provider_specs.push(tool);
    }

    let mut deferred_tools = BTreeMap::new();
    for name in input.deferred_tool_names {
        if is_denied(&name)
            || input.implicit_tool_names.contains(&name)
            || input.delegate_owned_tool_names.contains(&name)
            || !input.direct_reachable_tool_names.contains(&name)
            || !feature_allows(&name)
            || (is_surface_only && !is_explicitly_granted(&name))
            || !engagement_permits_tool(&name)
        {
            continue;
        }
        deferred_tools.insert(
            name.clone(),
            EffectiveToolGrant {
                name,
                kind: EffectiveToolKind::Deferred,
                provider_visible: false,
            },
        );
    }

    let dispatch_tool_names = direct_tools
        .keys()
        .chain(runtime_tools.keys())
        .chain(structural_tools.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    // Session and turn ids are audit provenance, not authorization inputs. A
    // cache eviction must not change the snapshot id for the same effective
    // policy, so hash the typed policy-bearing invocation fields only.
    let digest_material = serde_json::json!({
        "definition_digest": definition_digest,
        "invocation": {
            "principal": invocation.principal,
            "workspace": invocation.workspace,
            "source_agent_id": invocation.source_agent_id,
            "target_agent_id": invocation.target_agent_id,
            "surface": invocation.surface,
            "feature_mode": invocation.feature_mode,
            "source_kind": invocation.source_kind,
        },
        "provider": provider_specs.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
        // Bind realtime/external dispatch to schema content as well as names.
        // A hot reload that narrows an enum or changes a required parameter
        // must revoke the advertised snapshot even when function names stay
        // unchanged.
        "catalog_digest": catalog_digest,
        "deferred": deferred_tools.keys().collect::<Vec<_>>(),
        "implicit": &implicit_tools,
        "delegation": delegation_targets.keys().collect::<Vec<_>>(),
        "handover": handover_targets.keys().collect::<Vec<_>>(),
        "denied": denied_tool_names,
        "effective_approval_rules": &effective_approval_rules,
        "external_policy_digest": &external_policy_digest,
        // Two engagement states can project identical collections (e.g. a
        // ceiling that happens to cover the whole static grant); the recorded
        // (id, revision) pair still distinguishes the snapshots.
        "engagement_authority": engagement.as_ref().map(|engagement| {
            serde_json::json!({
                "engagement_id": engagement.engagement_id,
                "authority_revision": engagement.authority_revision,
            })
        }),
    });
    let snapshot_id = blake3::hash(
        &canonical_json_bytes(&digest_material)
            .map_err(|error| format!("snapshot_digest_failed:{error}"))?,
    )
    .to_hex()
    .to_string();
    let snapshot = Arc::new(EffectiveToolPolicySnapshot {
        snapshot_id,
        agent_id: definition.agent_id.clone(),
        definition_version: definition.version,
        definition_digest,
        invocation,
        trust_level: definition.trust_level.canonicalized().0,
        direct_tools,
        deferred_tools,
        runtime_tools,
        implicit_tools,
        structural_tools,
        delegation_targets,
        handover_targets,
        denied_tool_names,
        denied_tool_params: definition.denied_tool_params.clone(),
        approval_rules: effective_approval_rules,
        provider_specs,
        dispatch_tool_names,
        delegate_owned_tool_names: input.delegate_owned_tool_names,
        engagement_authority: engagement.as_ref().map(|engagement| {
            crate::magician_v2::engagements::EngagementAuthorityRef {
                engagement_id: engagement.engagement_id.clone(),
                authority_revision: engagement.authority_revision,
            }
        }),
    });

    let mut guard = cache
        .lock()
        .map_err(|_| "snapshot_cache_poisoned".to_string())?;
    if guard.len() >= SNAPSHOT_CACHE_LIMIT {
        guard.clear();
    }
    guard.insert(cache_key, snapshot.clone());
    drop(guard);
    log_policy_snapshot(&snapshot, false);
    Ok(snapshot)
}

/// Add a server-owned, non-side-effecting runtime control after a turn's model
/// profile has been selected. This keeps adaptive controls inside the same
/// provider/dispatch/introspection snapshot instead of appending an untracked
/// schema. The derived snapshot id binds the exact control schema.
pub fn extend_snapshot_with_runtime_tool(
    snapshot: &Arc<EffectiveToolPolicySnapshot>,
    tool: NativeExecutionTool,
) -> Result<Arc<EffectiveToolPolicySnapshot>, String> {
    let name = tool.name.clone();
    if matches!(snapshot.invocation.surface, InvocationSurface::PublicEnvoy) {
        return Err("runtime_tool_not_allowed_on_public_envoy".to_string());
    }
    if snapshot.delegate_owned_tool_names.contains(&name)
        || snapshot
            .denied_tool_names
            .iter()
            .any(|denied| tool_name_matches_block_entry(name.as_str(), denied.as_str()))
    {
        return Err(format!("runtime_tool_denied:{name}"));
    }
    if snapshot
        .provider_specs
        .iter()
        .any(|existing| existing.name == name)
    {
        return Err(format!("runtime_tool_name_collision:{name}"));
    }

    let mut derived = (**snapshot).clone();
    derived.runtime_tools.insert(
        name.clone(),
        EffectiveToolGrant {
            name: name.clone(),
            kind: EffectiveToolKind::Runtime,
            provider_visible: true,
        },
    );
    derived.dispatch_tool_names.insert(name);
    let schema_digest = blake3::hash(
        &canonical_json_bytes(&tool)
            .map_err(|error| format!("runtime_tool_digest_failed:{error}"))?,
    )
    .to_hex()
    .to_string();
    derived.provider_specs.push(tool);
    derived.snapshot_id = blake3::hash(
        &canonical_json_bytes(&serde_json::json!({
            "base_snapshot_id": snapshot.snapshot_id,
            "runtime_tool_schema_digest": schema_digest,
        }))
        .map_err(|error| format!("derived_snapshot_digest_failed:{error}"))?,
    )
    .to_hex()
    .to_string();
    Ok(Arc::new(derived))
}

pub fn remove_snapshot_runtime_tool(
    snapshot: &Arc<EffectiveToolPolicySnapshot>,
    tool_name: &str,
) -> Result<Arc<EffectiveToolPolicySnapshot>, String> {
    if !snapshot.runtime_tools.contains_key(tool_name) {
        return Err(format!("runtime_tool_not_in_snapshot:{tool_name}"));
    }
    let mut derived = (**snapshot).clone();
    derived.runtime_tools.remove(tool_name);
    derived.dispatch_tool_names.remove(tool_name);
    derived.provider_specs.retain(|tool| tool.name != tool_name);
    derived.snapshot_id = blake3::hash(
        &canonical_json_bytes(&serde_json::json!({
            "base_snapshot_id": snapshot.snapshot_id,
            "removed_runtime_tool": tool_name,
        }))
        .map_err(|error| format!("derived_snapshot_digest_failed:{error}"))?,
    )
    .to_hex()
    .to_string();
    Ok(Arc::new(derived))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn definition(extra: &str) -> AgentDefinition {
        AgentDefinition::from_yaml_str(&format!(
            "agent_id: test-agent\nname: Test Agent\npersona: Test\ntrust_level: local\n{extra}"
        ))
        .expect("definition")
    }

    fn tool(name: &str, control: bool) -> NativeExecutionTool {
        NativeExecutionTool {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            is_control_tool: control,
        }
    }

    fn invocation(surface: InvocationSurface, feature_mode: FeatureMode) -> AgentInvocationContext {
        AgentInvocationContext {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            source_agent_id: None,
            target_agent_id: "test-agent".to_string(),
            surface,
            feature_mode,
            source_kind: crate::magician_v2::agents::InvocationSourceKind::Direct,
            chat_session_id: Some("session-1".to_string()),
            chat_turn_id: Some("turn-1".to_string()),
        }
    }

    fn input() -> SnapshotResolutionInput {
        SnapshotResolutionInput::default()
    }

    #[test]
    fn leading_feature_marker_is_boundary_safe() {
        assert_eq!(
            parse_leading_feature_marker("@tutor explain this"),
            FeatureMode::Tutor
        );
        assert_eq!(
            parse_leading_feature_marker("@tutor@example.com"),
            FeatureMode::None
        );
        assert_eq!(
            parse_leading_feature_marker("please ask @tutor"),
            FeatureMode::None
        );
        assert_eq!(
            parse_leading_feature_marker("\"@tutor\" from the page"),
            FeatureMode::None
        );
        assert_eq!(
            parse_leading_feature_marker("@tutoring help"),
            FeatureMode::None
        );
        assert_eq!(
            parse_leading_feature_marker("@app-copilot show me"),
            FeatureMode::AppCopilot
        );
        assert_eq!(
            parse_leading_feature_marker("@tutor:explain this"),
            FeatureMode::Tutor
        );
        assert_eq!(
            parse_leading_feature_marker("@copilot,show me"),
            FeatureMode::AppCopilot
        );
        assert_eq!(
            parse_leading_feature_marker("@appcopilot show me"),
            FeatureMode::AppCopilot
        );
        assert_eq!(
            parse_leading_feature_invocation("hey, tutor explain this"),
            FeatureMode::Tutor
        );
        assert_eq!(
            parse_leading_feature_invocation("hey app copilot show me"),
            FeatureMode::AppCopilot
        );
        assert_eq!(
            parse_leading_feature_invocation("please ask hey tutor"),
            FeatureMode::None
        );
        assert_eq!(
            parse_leading_feature_invocation("email@tutor.com"),
            FeatureMode::None
        );
        assert_eq!(
            parse_leading_feature_invocation("hey tutor@example.com"),
            FeatureMode::None
        );
        assert_eq!(
            parse_leading_feature_invocation("hey copilot@example.com"),
            FeatureMode::None
        );
    }

    /// The classifier chat routes on now recognizes the VibeDev rail. Task 2 left
    /// this returning `None` on purpose; wiring the rail is what changes it.
    #[test]
    fn leading_feature_marker_recognizes_the_vibedev_rail() {
        assert_eq!(
            parse_leading_feature_marker("@vibedev fix the footer"),
            FeatureMode::Vibedev
        );
        assert_eq!(
            parse_leading_feature_marker("@vibedev #discuss should we use X"),
            FeatureMode::Vibedev
        );
        assert_eq!(
            parse_leading_feature_marker("@VIBEDEV Fix It"),
            FeatureMode::Vibedev
        );
        // A bare marker is still the rail — the caller refuses the empty request
        // in the user's own thread rather than classifying it away here.
        assert_eq!(
            parse_leading_feature_marker("@vibedev"),
            FeatureMode::Vibedev
        );

        // `parse_leading_feature_invocation` adds spoken commands on top of the
        // marker; the VibeDev rail has none, so the two must agree exactly.
        for text in [
            "@vibedev fix the footer",
            "@vibedev",
            "@vibedevops is the deploy bot",
            "",
        ] {
            assert_eq!(
                parse_leading_feature_invocation(text),
                parse_leading_feature_marker(text),
                "{text}"
            );
        }
    }

    /// The marker is matched as a whole token in leading position. Nothing
    /// collides with `@vibedev` today; the guard is what keeps a longer handle, a
    /// quoted example and a mid-sentence mention out of the rail — the shapes
    /// that turn up whenever someone is *explaining* the feature rather than
    /// using it.
    #[test]
    fn leading_feature_marker_does_not_hand_longer_handles_to_the_vibedev_rail() {
        for text in [
            "@vibedevops is the deploy bot",
            "@vibedev-review this diff please",
            "@vibedeveloper what changed today",
            "vibedev fix the footer",
            "please @vibedev fix the footer",
            "\"@vibedev fix the footer\" is how you start a build",
            "vibedev is stuck again",
        ] {
            assert_eq!(
                parse_leading_feature_marker(text),
                FeatureMode::None,
                "{text}"
            );
        }
    }

    /// The two `@vibedev` readers cannot disagree because there is only one: the
    /// classifier delegates to the rail's own parser. This pins that delegation
    /// so a future edit cannot quietly add `"@vibedev"` to the whitespace-split
    /// match and reintroduce a second, subtly different judge.
    #[test]
    fn the_vibedev_rail_marker_has_exactly_one_reader() {
        for text in [
            "@vibedev fix the footer",
            "@vibedev #discuss should we use X",
            "@vibedev",
            "@vibedevops is the deploy bot",
            "@vibedev-review this diff",
            "  @vibedev   leading space",
            "please @vibedev fix the footer",
            "\"@vibedev\" is the marker",
            "@tutor explain @vibedev",
            "",
            "   ",
        ] {
            let classified = parse_leading_feature_marker(text) == FeatureMode::Vibedev;
            let parsed = parse_vibedev_rail_invocation(text).is_some();
            assert_eq!(
                classified, parsed,
                "classifier and payload parser must agree on {text:?}"
            );
        }
    }

    // --- Lane vocabulary single-sourcing (batch 7 fold units F10/F13) --------
    //
    // The marker and spoken literals above are pinned verbatim by
    // `magician/tests/invoke_grammar_agreement.rs`, so the parser cannot read
    // them from constants without breaking that frozen oracle. What is pinned
    // HERE, at unit scope, is the agreement the fold was after: every word in
    // the canonical constants — the same constants the published
    // invoke-grammar catalog is generated from — parses to exactly its lane
    // through this file's entry points, and the Brainstorm agent binding is
    // the lane seam's constant rather than a second spelling.

    /// Constants → parse outcomes, typed markers: every marker word the
    /// constants carry (and therefore every marker the wire catalog
    /// publishes) must classify as exactly its lane.
    #[test]
    fn marker_constants_parse_to_exactly_their_lanes() {
        for (words, mode) in [
            (
                crate::magician_v2::tutor::TUTOR_MARKER_INVOKE_WORDS.as_slice(),
                FeatureMode::Tutor,
            ),
            (
                crate::magician_v2::tutor::APP_COPILOT_MARKER_INVOKE_WORDS.as_slice(),
                FeatureMode::AppCopilot,
            ),
            (
                crate::magician_v2::chat::invoke_catalog::BRAINSTORM_MARKER_INVOKE_WORDS.as_slice(),
                FeatureMode::Brainstorm,
            ),
        ] {
            for word in words {
                assert_eq!(
                    parse_leading_feature_marker(&format!("{word} explain this")),
                    mode,
                    "constant marker `{word}` must classify as {mode:?}"
                );
            }
        }
    }

    /// Constants → parse outcomes, spoken phrases: the shared wake word
    /// crossed with the lanes' spoken words (plus the App Copilot prefix
    /// window) — the same cross-product `invoke_grammar_catalog` publishes —
    /// must parse as exactly its lane.
    #[test]
    fn spoken_constants_parse_to_exactly_their_lanes() {
        use crate::magician_v2::chat::invoke_catalog::{
            APP_COPILOT_SPOKEN_PREFIX_WORD, SPOKEN_WAKE_WORD,
        };
        use crate::magician_v2::tutor::{
            APP_COPILOT_SPOKEN_INVOKE_WORDS, TUTOR_SPOKEN_INVOKE_WORDS,
        };

        for word in TUTOR_SPOKEN_INVOKE_WORDS {
            let phrase = format!("{SPOKEN_WAKE_WORD} {word}");
            assert_eq!(
                parse_leading_feature_invocation(&format!("{phrase} explain this")),
                FeatureMode::Tutor,
                "constant spoken phrase `{phrase}` must classify as Tutor"
            );
        }
        for word in APP_COPILOT_SPOKEN_INVOKE_WORDS {
            for phrase in [
                format!("{SPOKEN_WAKE_WORD} {word}"),
                format!("{SPOKEN_WAKE_WORD} {APP_COPILOT_SPOKEN_PREFIX_WORD} {word}"),
            ] {
                assert_eq!(
                    parse_leading_feature_invocation(&format!("{phrase} explain this")),
                    FeatureMode::AppCopilot,
                    "constant spoken phrase `{phrase}` must classify as AppCopilot"
                );
            }
        }
    }

    /// F13: the Brainstorm agent binding is the lane seam's constant — one
    /// spelling shared with the thinking-map session-admission path, with no
    /// second literal here. Every other mode binds no agent.
    #[test]
    fn brainstorm_agent_binding_is_the_lane_seam_constant() {
        assert_eq!(
            feature_agent_id(FeatureMode::Brainstorm),
            Some(crate::magician_v2::chat::lane_seam::BRAINSTORM_FACILITATION_AGENT_ID)
        );
        for mode in [
            FeatureMode::Tutor,
            FeatureMode::AppCopilot,
            FeatureMode::Vibedev,
            FeatureMode::None,
        ] {
            assert_eq!(feature_agent_id(mode), None, "{mode:?}");
        }
    }

    /// The rail owns no surface of its own: it rides an ordinary conversation,
    /// typed or spoken, and hands the work to a task. Those two transports are
    /// authorized and every other surface stays closed.
    #[test]
    fn the_vibedev_rail_is_authorized_on_chat_and_hands_free_voice_only() {
        for surface in [InvocationSurface::Chat, InvocationSurface::RealtimeVoice] {
            assert!(
                feature_surface_is_authorized(FeatureMode::Vibedev, surface),
                "{surface:?}"
            );
        }
        // `PublicEnvoy` above all: a stranger on a public channel must never be
        // able to spend the deployment's compute on a build.
        for surface in [
            InvocationSurface::PublicEnvoy,
            InvocationSurface::Tutor,
            InvocationSurface::AppCopilot,
            InvocationSurface::ThinkingMap,
            InvocationSurface::ContextualAssist,
            InvocationSurface::Task,
            InvocationSurface::Delegation,
            InvocationSurface::Handover,
            InvocationSurface::Meeting,
            InvocationSurface::Plane,
        ] {
            assert!(
                !feature_surface_is_authorized(FeatureMode::Vibedev, surface),
                "{surface:?}"
            );
        }
    }

    #[test]
    fn screen_draw_is_absent_from_ordinary_chat_and_present_on_typed_tutor_surface() {
        let definition = definition("");
        let input = || SnapshotResolutionInput {
            provider_tools: vec![tool("screen-draw", false), tool("search_memory", false)],
            direct_reachable_tool_names: BTreeSet::from([
                "screen-draw".to_string(),
                "search_memory".to_string(),
            ]),
            ..Default::default()
        };
        let ordinary = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Chat, FeatureMode::None),
            input(),
        )
        .expect("ordinary snapshot");
        assert!(!ordinary.permits_tool("screen-draw"));
        assert!(ordinary.permits_tool("search_memory"));

        let tutor = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Tutor, FeatureMode::Tutor),
            input(),
        )
        .expect("tutor snapshot");
        assert!(tutor.permits_tool("screen-draw"));
    }

    #[test]
    fn surface_only_agent_is_allowlist_only_and_has_no_delegation() {
        let mut definition = definition(
            "tools:\n  - search_memory\ninvocation_policy:\n  discoverability: surface_only\n  delegation: none\n  allowed_direct_surfaces:\n    - thinking_map\n",
        );
        definition.agent_id = BRAINSTORM_FACILITATION_AGENT_ID.to_string();
        let mut thinking_map_invocation =
            invocation(InvocationSurface::ThinkingMap, FeatureMode::Brainstorm);
        thinking_map_invocation.target_agent_id = BRAINSTORM_FACILITATION_AGENT_ID.to_string();
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            thinking_map_invocation,
            SnapshotResolutionInput {
                provider_tools: vec![
                    tool("search_memory", false),
                    tool("shell", false),
                    tool("delegate_to_agent", true),
                ],
                direct_reachable_tool_names: BTreeSet::from([
                    "search_memory".to_string(),
                    "shell".to_string(),
                ]),
                delegation_target_ids: vec!["worker".to_string()],
                ..Default::default()
            },
        )
        .expect("surface-only snapshot");
        assert!(snapshot.permits_tool("search_memory"));
        assert!(!snapshot.permits_tool("shell"));
        assert!(!snapshot.permits_tool("delegate_to_agent"));
        assert!(snapshot.delegation_targets.is_empty());
    }

    #[test]
    fn surface_only_and_public_envoy_do_not_gain_unlisted_implicit_task_state() {
        let mut surface_only = definition(
            "tools:\n  - search_memory\ninvocation_policy:\n  discoverability: surface_only\n  delegation: none\n  allowed_direct_surfaces:\n    - thinking_map\n",
        );
        surface_only.agent_id = BRAINSTORM_FACILITATION_AGENT_ID.to_string();
        let mut thinking_map_invocation =
            invocation(InvocationSurface::ThinkingMap, FeatureMode::Brainstorm);
        thinking_map_invocation.target_agent_id = BRAINSTORM_FACILITATION_AGENT_ID.to_string();
        let mut surface_input = input();
        surface_input
            .direct_reachable_tool_names
            .insert("task_state".to_string());
        surface_input
            .implicit_tool_names
            .insert("task_state".to_string());
        let surface_snapshot = resolve_effective_tool_policy_snapshot(
            &surface_only,
            thinking_map_invocation,
            surface_input,
        )
        .expect("surface snapshot");
        assert!(!surface_snapshot.permits_implicit_tool("task_state"));

        let envoy = definition(
            "invocation_policy:\n  discoverability: surface_only\n  delegation: none\n  allowed_direct_surfaces:\n    - public_envoy\n",
        );
        let mut envoy_input = input();
        envoy_input
            .direct_reachable_tool_names
            .insert("task_state".to_string());
        envoy_input
            .implicit_tool_names
            .insert("task_state".to_string());
        let envoy_snapshot = resolve_effective_tool_policy_snapshot(
            &envoy,
            invocation(InvocationSurface::PublicEnvoy, FeatureMode::None),
            envoy_input,
        )
        .expect("public envoy snapshot");
        assert!(!envoy_snapshot.permits_implicit_tool("task_state"));
    }

    #[test]
    fn delegate_owned_tools_cannot_enter_provider_or_deferred_sets() {
        let definition = definition("");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![tool("own_tool", false), tool("delegate_tool", false)],
                deferred_tool_names: BTreeSet::from([
                    "own_deferred".to_string(),
                    "delegate_tool".to_string(),
                ]),
                direct_reachable_tool_names: BTreeSet::from([
                    "own_tool".to_string(),
                    "own_deferred".to_string(),
                    "delegate_tool".to_string(),
                ]),
                delegate_owned_tool_names: BTreeSet::from(["delegate_tool".to_string()]),
                ..Default::default()
            },
        )
        .expect("snapshot");
        assert!(snapshot.permits_tool("own_tool"));
        assert!(snapshot.deferred_tools.contains_key("own_deferred"));
        assert!(!snapshot.permits_tool("own_deferred"));
        assert!(!snapshot.permits_tool("delegate_tool"));
        assert!(!snapshot.deferred_tools.contains_key("delegate_tool"));
    }

    fn engagement(ceiling: &[&str], team: &[&str], revision: u64) -> EngagementSnapshotInput {
        EngagementSnapshotInput {
            engagement_id: "eng-1".to_string(),
            authority_revision: revision,
            tool_ceiling: ceiling.iter().map(|name| name.to_string()).collect(),
            team: team.iter().map(|name| name.to_string()).collect(),
        }
    }

    /// §4.2c layer 1, row 1: a worker whose static grant is strictly broader
    /// than the engagement ceiling sees only the intersection in the provider
    /// schema. This is the projection layer — necessary, but NOT the security
    /// boundary; dispatch (layer 2) re-checks live authority independently.
    #[test]
    fn engagement_ceiling_narrows_the_projected_grant() {
        let definition = definition("");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![
                    tool("research", false),
                    tool("restricted_email", false),
                    tool("raw_email", false),
                    tool("browser", false),
                ],
                direct_reachable_tool_names: BTreeSet::from([
                    "research".to_string(),
                    "restricted_email".to_string(),
                    "raw_email".to_string(),
                    "browser".to_string(),
                ]),
                engagement: Some(engagement(&["research", "restricted_email"], &[], 3)),
                ..Default::default()
            },
        )
        .expect("engagement snapshot");
        assert!(snapshot.permits_tool("research"), "in ceiling");
        assert!(snapshot.permits_tool("restricted_email"), "in ceiling");
        assert!(
            !snapshot.permits_tool("raw_email"),
            "outside ceiling, static grant ignored"
        );
        assert!(!snapshot.permits_tool("browser"), "outside ceiling");
        // The provider schema the model sees agrees with the dispatch set.
        let visible: BTreeSet<&str> = snapshot.visible_now().collect();
        assert!(!visible.contains("raw_email"));
        assert!(!visible.contains("browser"));
        // The snapshot records the authority it narrowed under (row 9 input).
        assert_eq!(
            snapshot
                .engagement_authority
                .as_ref()
                .map(|e| e.authority_revision),
            Some(3)
        );
    }

    /// §4.2c: the fail-closed shape. An engagement resolving to an empty
    /// ceiling (the projection's answer on any authority denial) admits no
    /// business tool and no delegation target — the loop keeps only its
    /// pure control surface so it can still terminate.
    #[test]
    fn empty_engagement_ceiling_denies_every_business_tool() {
        let definition = definition("");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![tool("research", false), tool("delegate_to_agent", true)],
                direct_reachable_tool_names: BTreeSet::from(["research".to_string()]),
                delegation_target_ids: vec!["worker".to_string()],
                engagement: Some(engagement(&[], &[], 1)),
                ..Default::default()
            },
        )
        .expect("empty-ceiling snapshot");
        assert!(
            !snapshot.permits_tool("research"),
            "empty ceiling admits nothing"
        );
        assert!(
            snapshot.delegation_targets.is_empty(),
            "empty team removes every delegation target"
        );
    }

    /// §4.2c row 4: delegation targets are intersected with the engagement
    /// team; a target the agent could otherwise reach is removed when it is
    /// outside team[].
    #[test]
    fn engagement_team_intersects_delegation_targets() {
        // The agent's own policy permits delegating to anyone (`*`); the
        // engagement team[] is what narrows the reachable set. Uses the real
        // delegate tool schema so the target-ceiling enum is constrained the
        // same way production does.
        let definition = definition("delegation_targets:\n  - '*'\n");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![
                    crate::magician_v2::execution::agentic::native_catalog::build_delegate_to_agent_tool(),
                ],
                delegation_target_ids: vec!["in-team".to_string(), "off-team".to_string()],
                engagement: Some(engagement(&["delegate_to_agent"], &["in-team"], 1)),
                ..Default::default()
            },
        )
        .expect("team snapshot");
        assert!(
            snapshot.permits_delegation_target("in-team"),
            "an agent-reachable target inside team[] stays reachable"
        );
        assert!(
            !snapshot.permits_delegation_target("off-team"),
            "a target outside team[] is removed even though the agent could reach it"
        );
        // The provider schema the model sees agrees with the dispatch ceiling.
        assert_eq!(
            snapshot.provider_specs[0]
                .parameters
                .pointer("/properties/delegation_targets/items/properties/target_agent_id/enum"),
            Some(&serde_json::json!(["in-team"])),
            "the constrained enum is the team intersection, not the agent's full reach"
        );
    }

    /// Row 9 at the cache: bumping the authority revision must not serve the
    /// snapshot cached under the prior revision. Same definition, same
    /// catalog, different engagement revision → distinct snapshot identity.
    #[test]
    fn engagement_revision_change_invalidates_cached_snapshot() {
        let definition = definition("");
        let build = |engagement: EngagementSnapshotInput| {
            resolve_effective_tool_policy_snapshot(
                &definition,
                invocation(InvocationSurface::Task, FeatureMode::None),
                SnapshotResolutionInput {
                    provider_tools: vec![tool("research", false), tool("raw_email", false)],
                    direct_reachable_tool_names: BTreeSet::from([
                        "research".to_string(),
                        "raw_email".to_string(),
                    ]),
                    engagement: Some(engagement),
                    ..Default::default()
                },
            )
            .expect("snapshot")
        };
        // Revision 1 grants both; a narrow-then-bump to revision 2 drops
        // raw_email. If the cache keyed only on definition+catalog it would
        // return the revision-1 snapshot here.
        let broad = build(engagement(&["research", "raw_email"], &[], 1));
        let narrowed = build(engagement(&["research"], &[], 2));
        assert!(broad.permits_tool("raw_email"));
        assert!(
            !narrowed.permits_tool("raw_email"),
            "the bump is not served from cache"
        );
        assert_ne!(broad.snapshot_id, narrowed.snapshot_id);
    }

    #[test]
    fn provider_candidate_without_direct_runtime_or_structural_grant_is_rejected() {
        let definition = definition("");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![
                    tool("direct_tool", false),
                    tool("injected_tool", false),
                    tool("runtime_tool", false),
                    tool("yield", true),
                ],
                runtime_tool_names: BTreeSet::from(["runtime_tool".to_string()]),
                direct_reachable_tool_names: BTreeSet::from(["direct_tool".to_string()]),
                ..Default::default()
            },
        )
        .expect("snapshot");

        assert!(snapshot.permits_tool("direct_tool"));
        assert!(snapshot.permits_tool("runtime_tool"));
        assert!(snapshot.permits_tool("yield"));
        assert!(!snapshot.permits_tool("injected_tool"));
    }

    #[test]
    fn disabled_or_surface_ineligible_definition_fails_before_catalog_projection() {
        let mut disabled = definition("");
        disabled.disabled = true;
        let disabled_error = resolve_effective_tool_policy_snapshot(
            &disabled,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![tool("shell", false)],
                direct_reachable_tool_names: BTreeSet::from(["shell".to_string()]),
                ..Default::default()
            },
        )
        .expect_err("disabled definition must fail closed");
        assert!(disabled_error.starts_with("definition_disabled:"));

        let surface_only = definition(
            "tools:\n  - search_memory\ninvocation_policy:\n  discoverability: surface_only\n  delegation: none\n  allowed_direct_surfaces:\n    - thinking_map\n",
        );
        let surface_error = resolve_effective_tool_policy_snapshot(
            &surface_only,
            invocation(InvocationSurface::Chat, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![tool("search_memory", false)],
                direct_reachable_tool_names: BTreeSet::from(["search_memory".to_string()]),
                ..Default::default()
            },
        )
        .expect_err("surface-only definition must reject ordinary chat");
        assert!(surface_error.starts_with("agent_not_surface_eligible:"));
    }

    #[test]
    fn provider_schema_change_revokes_snapshot_even_when_tool_name_is_stable() {
        let definition = definition("");
        let resolve = |required: bool| {
            let mut candidate = tool("search_memory", false);
            candidate.parameters = serde_json::json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": if required { vec!["query"] } else { Vec::<&str>::new() },
            });
            resolve_effective_tool_policy_snapshot(
                &definition,
                invocation(InvocationSurface::RealtimeVoice, FeatureMode::None),
                SnapshotResolutionInput {
                    provider_tools: vec![candidate],
                    direct_reachable_tool_names: BTreeSet::from(["search_memory".to_string()]),
                    ..Default::default()
                },
            )
            .expect("snapshot")
        };

        let first = resolve(false);
        let narrowed = resolve(true);
        assert_ne!(first.snapshot_id, narrowed.snapshot_id);
    }

    #[test]
    fn orchestrator_deny_removes_every_structural_control() {
        let definition = definition("denied_tools:\n  - orchestrator\n");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![
                    tool("spawn_sub_goal", true),
                    tool("delegate_to_agent", true),
                    tool("handover_to_agent", true),
                ],
                delegation_target_ids: vec!["worker".to_string()],
                ..Default::default()
            },
        )
        .expect("snapshot");
        assert!(snapshot.structural_tools.is_empty());
        assert!(snapshot.provider_specs.is_empty());
    }

    #[test]
    fn delegation_roster_change_invalidates_cached_snapshot() {
        let definition = definition("delegation_targets:\n  - '*'\n");
        let resolve = |target: &str| {
            resolve_effective_tool_policy_snapshot(
                &definition,
                invocation(InvocationSurface::Task, FeatureMode::None),
                SnapshotResolutionInput {
                    provider_tools: vec![
                        crate::magician_v2::execution::agentic::native_catalog::build_delegate_to_agent_tool(),
                    ],
                    delegation_target_ids: vec![target.to_string()],
                    ..Default::default()
                },
            )
            .expect("snapshot")
        };
        let first = resolve("worker-a");
        let second = resolve("worker-b");
        assert_ne!(first.snapshot_id, second.snapshot_id);
        assert!(first.permits_delegation_target("worker-a"));
        assert!(second.permits_delegation_target("worker-b"));
        assert_eq!(
            first.provider_specs[0]
                .parameters
                .pointer("/properties/delegation_targets/items/properties/target_agent_id/enum"),
            Some(&serde_json::json!(["worker-a"]))
        );
    }

    #[test]
    fn delegation_and_handover_schemas_keep_distinct_target_ceilings() {
        let definition = definition("delegation_targets:\n  - '*'\n");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Chat, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![
                    crate::magician_v2::execution::agentic::native_catalog::build_delegate_to_agent_tool(),
                    crate::magician_v2::execution::agentic::native_catalog::build_handover_to_agent_tool(),
                ],
                delegation_target_ids: vec!["delegate-only".to_string()],
                handover_target_ids: vec!["handover-only".to_string()],
                ..Default::default()
            },
        )
        .expect("surface-specific snapshot");

        assert!(snapshot.permits_delegation_target("delegate-only"));
        assert!(!snapshot.permits_delegation_target("handover-only"));
        assert!(snapshot.permits_handover_target("handover-only"));
        assert!(!snapshot.permits_handover_target("delegate-only"));
        assert_eq!(
            snapshot.structural_tools["delegate_to_agent"].allowed_targets,
            BTreeSet::from(["delegate-only".to_string()])
        );
        assert_eq!(
            snapshot.structural_tools["handover_to_agent"].allowed_targets,
            BTreeSet::from(["handover-only".to_string()])
        );
        assert_eq!(
            snapshot
                .provider_specs
                .iter()
                .find(|tool| tool.name == "handover_to_agent")
                .and_then(|tool| { tool.parameters.pointer("/properties/target_agent_id/enum") }),
            Some(&serde_json::json!(["handover-only"]))
        );
    }

    #[test]
    fn cached_policy_keeps_stable_id_but_uses_current_turn_provenance() {
        let definition = definition("");
        let input = || SnapshotResolutionInput {
            provider_tools: vec![tool("search_memory", false)],
            direct_reachable_tool_names: BTreeSet::from(["search_memory".to_string()]),
            ..Default::default()
        };
        let first = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Chat, FeatureMode::None),
            input(),
        )
        .expect("first snapshot");
        let mut next_invocation = invocation(InvocationSurface::Chat, FeatureMode::None);
        next_invocation.chat_session_id = Some("session-2".to_string());
        next_invocation.chat_turn_id = Some("turn-2".to_string());
        let second = resolve_effective_tool_policy_snapshot(&definition, next_invocation, input())
            .expect("cached snapshot");

        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(
            second.invocation.chat_session_id.as_deref(),
            Some("session-2")
        );
        assert_eq!(second.invocation.chat_turn_id.as_deref(), Some("turn-2"));
    }

    #[test]
    fn snapshot_identity_is_independent_of_nested_hash_map_iteration_order() {
        fn denied_params(
            indices: impl Iterator<Item = usize>,
        ) -> HashMap<String, HashMap<String, Vec<String>>> {
            let mut outer = HashMap::new();
            for index in indices {
                let mut inner = HashMap::new();
                inner.insert(
                    format!("argument_{:02}", 23 - index),
                    vec![format!("blocked/{index}")],
                );
                inner.insert(
                    format!("argument_{index:02}"),
                    vec![format!("private/{index}")],
                );
                outer.insert(format!("tool_{index:02}"), inner);
            }
            outer
        }

        let mut forward = definition("");
        forward.denied_tool_params = denied_params(0..24);
        let mut reverse = definition("");
        reverse.denied_tool_params = denied_params((0..24).rev());
        assert_eq!(forward, reverse, "fixtures must be semantically identical");
        assert_eq!(
            canonical_json_bytes(&forward).expect("forward canonical JSON"),
            canonical_json_bytes(&reverse).expect("reverse canonical JSON")
        );
        assert_eq!(
            canonical_definition_digest(&forward).expect("forward definition digest"),
            canonical_definition_digest(&reverse).expect("reverse definition digest")
        );

        let input = || SnapshotResolutionInput {
            provider_tools: vec![tool("search_memory", false)],
            direct_reachable_tool_names: BTreeSet::from(["search_memory".to_string()]),
            ..Default::default()
        };
        let forward_snapshot = resolve_effective_tool_policy_snapshot(
            &forward,
            invocation(InvocationSurface::Chat, FeatureMode::None),
            input(),
        )
        .expect("forward snapshot");
        let reverse_snapshot = resolve_effective_tool_policy_snapshot(
            &reverse,
            invocation(InvocationSurface::Chat, FeatureMode::None),
            input(),
        )
        .expect("reverse snapshot");

        assert_eq!(
            forward_snapshot.definition_digest,
            reverse_snapshot.definition_digest
        );
        assert_eq!(forward_snapshot.snapshot_id, reverse_snapshot.snapshot_id);
    }

    #[test]
    fn public_envoy_has_no_tools_or_delegation_targets() {
        let definition = definition(
            "tools:\n  - notify_owner\ninvocation_policy:\n  discoverability: surface_only\n  delegation: none\n  allowed_direct_surfaces:\n    - public_envoy\n",
        );
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::PublicEnvoy, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![
                    tool("search_memory", false),
                    tool("delegate_to_agent", true),
                ],
                direct_reachable_tool_names: BTreeSet::from(["search_memory".to_string()]),
                delegation_target_ids: vec!["worker".to_string()],
                ..Default::default()
            },
        )
        .expect("public envoy snapshot");

        assert!(snapshot.provider_specs.is_empty());
        assert!(snapshot.dispatch_tool_names.is_empty());
        assert!(snapshot.delegation_targets.is_empty());
    }

    #[test]
    fn implicit_task_state_is_authorized_without_becoming_provider_callable() {
        let definition = definition("tools:\n  - task_state\n");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                deferred_tool_names: BTreeSet::from(["task_state".to_string()]),
                direct_reachable_tool_names: BTreeSet::from(["task_state".to_string()]),
                implicit_tool_names: BTreeSet::from(["task_state".to_string()]),
                ..Default::default()
            },
        )
        .expect("task-state snapshot");

        assert!(snapshot.permits_implicit_tool("task_state"));
        assert!(!snapshot.permits_tool("task_state"));
        assert!(!snapshot.direct_tools.contains_key("task_state"));
        assert!(!snapshot.deferred_tools.contains_key("task_state"));
        assert!(!snapshot.runtime_tools.contains_key("task_state"));
        assert!(!snapshot.dispatch_tool_names.contains("task_state"));
        assert!(!snapshot
            .provider_specs
            .iter()
            .any(|tool| tool.name == "task_state"));
        assert!(!snapshot.visible_now().any(|name| name == "task_state"));
    }

    #[test]
    fn implicit_task_state_respects_whole_tool_denies() {
        let definition = definition("tools:\n  - task_state\ndenied_tools:\n  - task_state\n");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                direct_reachable_tool_names: BTreeSet::from(["task_state".to_string()]),
                implicit_tool_names: BTreeSet::from(["task_state".to_string()]),
                ..Default::default()
            },
        )
        .expect("task-state snapshot");
        assert!(!snapshot.permits_implicit_tool("task_state"));
    }

    #[test]
    fn runtime_feasibility_hides_spawn_sub_goal_before_provider_dispatch() {
        let definition = definition("");
        let snapshot = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Task, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![tool("spawn_sub_goal", true), tool("yield", true)],
                spawn_sub_goal_allowed: Some(false),
                ..Default::default()
            },
        )
        .expect("snapshot");
        assert!(!snapshot.permits_tool("spawn_sub_goal"));
        assert!(!snapshot.visible_now().any(|name| name == "spawn_sub_goal"));
        assert!(snapshot.permits_tool("yield"));
    }

    #[test]
    fn trust_and_effective_approval_changes_invalidate_snapshot_identity() {
        let definition = definition("");
        let resolve = |trust_digest: &str, action: &str| {
            resolve_effective_tool_policy_snapshot(
                &definition,
                invocation(InvocationSurface::RealtimeVoice, FeatureMode::None),
                SnapshotResolutionInput {
                    provider_tools: vec![
                        crate::magician_v2::execution::agentic::native_catalog::build_delegate_to_agent_tool(),
                    ],
                    delegation_target_ids: vec!["worker".to_string()],
                    effective_approval_rules: Some(vec![ApprovalRule {
                        tool: "orchestrator".to_string(),
                        action: crate::magician_v2::agents::types::ActionPattern::Single(
                            action.to_string(),
                        ),
                        when: None,
                        ttl_secs: None,
                    }]),
                    external_policy_digest: Some(trust_digest.to_string()),
                    ..Default::default()
                },
            )
            .expect("snapshot")
        };

        let first = resolve("trust-a", "delegate_to_agent");
        let trust_changed = resolve("trust-b", "delegate_to_agent");
        let approval_changed = resolve("trust-a", "handover_to_agent");
        assert_ne!(first.snapshot_id, trust_changed.snapshot_id);
        assert_ne!(first.snapshot_id, approval_changed.snapshot_id);
        assert!(first.structural_tools["delegate_to_agent"].requires_approval);
        assert!(!approval_changed.structural_tools["delegate_to_agent"].requires_approval);
    }

    #[test]
    fn adaptive_runtime_control_stays_inside_derived_snapshot() {
        let definition = definition("");
        let base = resolve_effective_tool_policy_snapshot(
            &definition,
            invocation(InvocationSurface::Chat, FeatureMode::None),
            SnapshotResolutionInput {
                provider_tools: vec![tool("search_memory", false)],
                direct_reachable_tool_names: BTreeSet::from(["search_memory".to_string()]),
                ..Default::default()
            },
        )
        .expect("base snapshot");
        let extended =
            extend_snapshot_with_runtime_tool(&base, tool("request_thinking_mode", false))
                .expect("extended snapshot");
        assert_ne!(base.snapshot_id, extended.snapshot_id);
        assert!(extended.permits_tool("request_thinking_mode"));
        assert!(extended
            .visible_now()
            .any(|name| name == "request_thinking_mode"));

        let removed = remove_snapshot_runtime_tool(&extended, "request_thinking_mode")
            .expect("removed runtime tool");
        assert!(!removed.permits_tool("request_thinking_mode"));
        assert!(!removed
            .visible_now()
            .any(|name| name == "request_thinking_mode"));
    }

    #[test]
    fn effective_policy_table_matrix_is_monotonic_across_every_authorization_dimension() {
        #[derive(Clone, Copy, Debug)]
        enum DefinitionState {
            Present,
            Missing,
            Disabled,
            TargetMismatch,
            HotReloadedDeny,
        }

        let trust_levels = ["builtin", "local", "reviewed", "untrusted"];
        let surfaces = [
            (InvocationSurface::Chat, FeatureMode::None, "test-agent"),
            (
                InvocationSurface::RealtimeVoice,
                FeatureMode::None,
                "test-agent",
            ),
            (InvocationSurface::Task, FeatureMode::None, "test-agent"),
            (
                InvocationSurface::Delegation,
                FeatureMode::None,
                "test-agent",
            ),
            (InvocationSurface::Handover, FeatureMode::None, "test-agent"),
            (InvocationSurface::Tutor, FeatureMode::Tutor, "test-agent"),
            (
                InvocationSurface::AppCopilot,
                FeatureMode::AppCopilot,
                "test-agent",
            ),
            (
                InvocationSurface::ThinkingMap,
                FeatureMode::Brainstorm,
                BRAINSTORM_FACILITATION_AGENT_ID,
            ),
            (
                InvocationSurface::ContextualAssist,
                FeatureMode::None,
                "test-agent",
            ),
            (
                InvocationSurface::PublicEnvoy,
                FeatureMode::None,
                "public-envoy-eval",
            ),
            (InvocationSurface::Plane, FeatureMode::None, "test-agent"),
        ];
        let source_kinds = [
            crate::magician_v2::agents::InvocationSourceKind::Direct,
            crate::magician_v2::agents::InvocationSourceKind::ChatInline,
            crate::magician_v2::agents::InvocationSourceKind::Autonomous,
            crate::magician_v2::agents::InvocationSourceKind::Delegated,
            crate::magician_v2::agents::InvocationSourceKind::Handover,
            crate::magician_v2::agents::InvocationSourceKind::ProductFeature,
        ];
        let definition_states = [
            DefinitionState::Present,
            DefinitionState::Missing,
            DefinitionState::Disabled,
            DefinitionState::TargetMismatch,
            DefinitionState::HotReloadedDeny,
        ];
        let mut case_count = 0usize;

        for trust_level in trust_levels {
            for (surface, feature_mode, target_agent_id) in surfaces {
                for source_kind in source_kinds {
                    for definition_state in definition_states {
                        case_count += 1;
                        let surface_only = matches!(
                            surface,
                            InvocationSurface::ThinkingMap
                                | InvocationSurface::PublicEnvoy
                                | InvocationSurface::Plane
                        );
                        let direct_surface = surface.as_str();
                        let explicit_tools = if surface == InvocationSurface::ThinkingMap {
                            "tools:\n  - search_memory\n  - yield\n"
                        } else if surface == InvocationSurface::PublicEnvoy {
                            ""
                        } else {
                            "tools:\n  - search_memory\n  - runtime_probe\n  - deferred_probe\n"
                        };
                        let invocation_policy = if surface_only {
                            format!(
                                "invocation_policy:\n  discoverability: surface_only\n  delegation: none\n  allowed_direct_surfaces:\n    - {direct_surface}\n"
                            )
                        } else {
                            "delegation_targets:\n  - '*'\n".to_string()
                        };
                        let denies = if matches!(definition_state, DefinitionState::HotReloadedDeny)
                        {
                            "denied_tools:\n  - search_memory\n  - orchestrator\n"
                        } else {
                            ""
                        };
                        let definition_agent_id =
                            if matches!(definition_state, DefinitionState::TargetMismatch) {
                                "wrong-agent"
                            } else {
                                target_agent_id
                            };
                        let definition_yaml = format!(
                            "agent_id: {definition_agent_id}\nname: Matrix agent\npersona: Matrix fixture\ntrust_level: {trust_level}\n{explicit_tools}{invocation_policy}{denies}"
                        );
                        let definition = if matches!(definition_state, DefinitionState::Missing) {
                            None
                        } else {
                            let mut definition = AgentDefinition::from_yaml_str(&definition_yaml)
                                .expect("matrix definition");
                            definition.disabled =
                                matches!(definition_state, DefinitionState::Disabled);
                            Some(definition)
                        };

                        let mut invocation = invocation(surface, feature_mode);
                        invocation.target_agent_id = target_agent_id.to_string();
                        invocation.source_kind = source_kind;
                        invocation.source_agent_id = matches!(
                            source_kind,
                            crate::magician_v2::agents::InvocationSourceKind::Delegated
                                | crate::magician_v2::agents::InvocationSourceKind::Handover
                        )
                        .then(|| "source-agent".to_string());

                        let Some(definition) = definition else {
                            let error = "definition_missing";
                            assert_eq!(error, "definition_missing");
                            continue;
                        };
                        let result = resolve_effective_tool_policy_snapshot(
                            &definition,
                            invocation,
                            SnapshotResolutionInput {
                                provider_tools: vec![
                                    tool("search_memory", false),
                                    tool("runtime_probe", false),
                                    tool("delegate_probe", false),
                                    tool("screen-draw", false),
                                    tool("task_state", false),
                                    crate::magician_v2::execution::agentic::native_catalog::build_delegate_to_agent_tool(),
                                    crate::magician_v2::execution::agentic::native_catalog::build_handover_to_agent_tool(),
                                    tool("spawn_sub_goal", true),
                                    tool("yield", true),
                                ],
                                runtime_tool_names: BTreeSet::from([
                                    "runtime_probe".to_string(),
                                    "screen-draw".to_string(),
                                ]),
                                deferred_tool_names: BTreeSet::from([
                                    "deferred_probe".to_string(),
                                    "delegate_probe".to_string(),
                                ]),
                                direct_reachable_tool_names: BTreeSet::from([
                                    "search_memory".to_string(),
                                    "runtime_probe".to_string(),
                                    "deferred_probe".to_string(),
                                    "delegate_probe".to_string(),
                                    "screen-draw".to_string(),
                                    "task_state".to_string(),
                                ]),
                                delegate_owned_tool_names: BTreeSet::from([
                                    "delegate_probe".to_string(),
                                ]),
                                delegation_target_ids: vec!["worker".to_string()],
                                handover_target_ids: vec!["new-owner".to_string()],
                                implicit_tool_names: BTreeSet::from(["task_state".to_string()]),
                                spawn_sub_goal_allowed: Some(true),
                                ..Default::default()
                            },
                        );

                        if matches!(definition_state, DefinitionState::Disabled) {
                            assert!(result
                                .expect_err("disabled definition")
                                .starts_with("definition_disabled:"));
                            continue;
                        }
                        if matches!(definition_state, DefinitionState::TargetMismatch) {
                            assert_eq!(
                                result.expect_err("target mismatch"),
                                "invocation_target_definition_mismatch"
                            );
                            continue;
                        }

                        let snapshot = result.unwrap_or_else(|error| {
                            panic!(
                                "matrix case {case_count} failed: trust={trust_level} surface={surface:?} source={source_kind:?} state={definition_state:?}: {error}"
                            )
                        });
                        let visible = snapshot
                            .visible_now()
                            .map(str::to_string)
                            .collect::<BTreeSet<_>>();
                        assert_eq!(visible, snapshot.dispatch_tool_names);
                        assert!(!snapshot.permits_tool("deferred_probe"));
                        assert!(!snapshot.permits_tool("delegate_probe"));
                        assert!(!snapshot.permits_tool("task_state"));
                        assert!(!snapshot.provider_specs.iter().any(|tool| {
                            matches!(
                                tool.name.as_str(),
                                "deferred_probe" | "delegate_probe" | "task_state"
                            )
                        }));

                        if surface == InvocationSurface::PublicEnvoy {
                            assert!(snapshot.provider_specs.is_empty());
                            assert!(snapshot.dispatch_tool_names.is_empty());
                            assert!(snapshot.delegation_targets.is_empty());
                            assert!(snapshot.handover_targets.is_empty());
                            continue;
                        }
                        if surface == InvocationSurface::ThinkingMap {
                            let hot_denied =
                                matches!(definition_state, DefinitionState::HotReloadedDeny);
                            assert_eq!(snapshot.permits_tool("search_memory"), !hot_denied);
                            assert_eq!(snapshot.permits_tool("yield"), !hot_denied);
                            assert!(!snapshot.permits_tool("runtime_probe"));
                            assert!(snapshot.delegation_targets.is_empty());
                            assert!(snapshot.handover_targets.is_empty());
                            continue;
                        }

                        let hot_denied =
                            matches!(definition_state, DefinitionState::HotReloadedDeny);
                        assert_eq!(snapshot.permits_tool("search_memory"), !hot_denied);
                        assert_eq!(
                            snapshot.permits_tool("screen-draw"),
                            matches!(
                                surface,
                                InvocationSurface::Tutor | InvocationSurface::AppCopilot
                            )
                        );
                        let agent_transfer_allowed =
                            !hot_denied && surface != InvocationSurface::Plane;
                        assert_eq!(
                            snapshot.permits_delegation_target("worker"),
                            agent_transfer_allowed
                        );
                        assert_eq!(
                            snapshot.permits_handover_target("new-owner"),
                            agent_transfer_allowed
                        );
                        if agent_transfer_allowed {
                            assert_eq!(
                                snapshot.structural_tools["delegate_to_agent"].allowed_targets,
                                BTreeSet::from(["worker".to_string()])
                            );
                            assert_eq!(
                                snapshot.structural_tools["handover_to_agent"].allowed_targets,
                                BTreeSet::from(["new-owner".to_string()])
                            );
                        }
                    }
                }
            }
        }

        assert_eq!(case_count, 1_320);
    }
}
