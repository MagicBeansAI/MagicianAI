//! Scoped, read-only Apps binder for the agent roster's social-participation
//! face.
//!
//! The fifth host-read binder, and the narrowest. It exists because a package
//! that owns a membership corpus must keep it reconciled with the live agent
//! definitions — a newly created agent has to be able to join, and a retired
//! one has to stop being admitted — and a package has no reach into
//! `AgentDefinitionStore`. `list_agents` is not app-bindable
//! (`compiled_app_provider_implementation_identity` returns `None` for it), so
//! without this a package's roster would freeze at whatever the migration
//! copied.
//!
//! **What it deliberately does NOT project.** An agent definition carries the
//! prompt, the tool grants, the model routing, the delegation graph and the
//! app-tool contract. None of that crosses this seam. The projection is
//! identity, display name, enabled state, the three `social_persona` fields a
//! participation policy needs, and one bit of runtime state — whether the
//! agent has work in flight — a strictly smaller face than
//! `get_agent_details`, which is why this is its own binder rather than a
//! widening of an existing one.
//!
//! The busy bit is the one thing here that is not read off a definition. It
//! comes from the artifact service's working-agent lookup (owners of running
//! tasks plus the agents of in-flight executions under them, app-workflow
//! tasks excluded) and exists so a participation round can hand turns to idle
//! agents only, without a model call. When no service is attached the field is
//! `null`, never a guess: "not known" is a different answer from "idle".
//!
//! The embedded pack definition (`embedded_pack_defs/agent_roster_data.yaml`)
//! deliberately does not describe this field. Its exact bytes are the
//! platform primitive's identity — every recipe that binds this tool pins
//! `primitive:platform:<blake3 of those bytes>` and an action ref derived from
//! it, and every installed package's lock seals the same digest — so a doc
//! edit there re-keys every consumer. The field is documented here and in the
//! component docs instead.
//!
//! Read-only with respect to what it projects (the binder-family invariant):
//! there is no path here that edits a definition, and a compile-time assertion
//! below refuses an action name that reads like one.
//!
//! One honest caveat, because "read-only" would otherwise overstate it.
//! Resolving a definition through `AgentDefinitionStore` can materialize a
//! *system template* into the scope — `get_definition` falls back to
//! `materialize_template_into_scope`, and `list_definitions` materializes the
//! whole template set — so a read here can create the scope's copy of a
//! first-party agent. That is the definition store's behaviour on any read
//! rather than something this binder adds, and it can only ever instantiate a
//! system-provided template, never author a new agent or alter an existing
//! one. It is recorded because the alternative is a doc comment that quietly
//! isn't true.
//!
//! Freshness rides on the runtime's shared definition-store cache, which is
//! invalidated by writes through that same handle. In-process agent CRUD goes
//! through it, so a created or retired agent shows up on the next read; an
//! out-of-band edit to the YAML on disk would not.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use magicllm::LlmScope;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::time::timeout;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::agents::definition_store::{AgentDefinitionStore, DefinitionRecord};
use crate::magician_v2::agents::storage::validate_agent_identifier;
use crate::magician_v2::artifact_v2::service::ArtifactV2Service;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::auth::ScopeRef;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::PlanStep;

pub const AGENT_ROSTER_DATA_TOOL_NAME: &str = "agent_roster_data";

pub(crate) const APP_BOUND_AGENT_ROSTER_INPUT_CEILING: u64 = 4 * 1024;
pub(crate) const APP_BOUND_AGENT_ROSTER_RESULT_CEILING: u64 = 256 * 1024;

const DEFAULT_LIST_LIMIT: usize = 50;
const MAX_LIST_LIMIT: u64 = 200;
const MAX_ID_BYTES: usize = 255;
/// Rows beyond a page's cursor before it reports `scan_truncated`.
///
/// This is a size signal, not a reachability one, and the distinction matters
/// to a consumer. The budget is applied to what follows the cursor, so paging
/// walks straight past it: `scan_truncated: true` means "a lot more remains",
/// never "you cannot reach the rest". A reconciler that stops on it is wrong.
/// It is not a resource bound either — the store hands back the whole scope's
/// roster before this is applied — it is the family-uniform signal the other
/// four host-read binders emit, kept so a package reads one vocabulary.
const ROSTER_SCAN_BUDGET: usize = 4_096;
const MAX_DISPLAY_NAME_BYTES: usize = 256;

const ACTIONS: &[&str] = &["list_members", "read_member"];

#[derive(Clone)]
pub struct AgentRosterDataProvider {
    workspace_layout: ArtifactV2Workspace,
    /// The runtime's shared definition store, when the registering caller has
    /// one.
    ///
    /// This is a performance seam that is also a correctness one. An
    /// `AgentDefinitionStore` owns an `Arc<DefinitionCache>` which its own
    /// write paths invalidate. A store constructed per call owns a private
    /// cache that nothing invalidates and that misses cold every time, so each
    /// roster read re-runs the whole builtin-template materialization pass — a
    /// directory scan and a YAML parse per template, then a mkdir, a stat and a
    /// re-read per template id — for what is a polled read behind a 90-second
    /// behavior cadence. Sharing the runtime's handle keeps the cache warm and
    /// keeps it fresh, because agent edits invalidate through that same handle.
    definition_store: Option<Arc<AgentDefinitionStore>>,
    /// Where the busy bit comes from. Optional for the same reason the
    /// definition store is: control-plane and test boots register this binder
    /// before an artifact service exists. Without it every row reports
    /// `busy: null`.
    activity: Option<Arc<ArtifactV2Service>>,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for AgentRosterDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRosterDataProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("shares_definition_store", &self.definition_store.is_some())
            .field("reports_activity", &self.activity.is_some())
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl AgentRosterDataProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            definition_store: None,
            activity: None,
            pack_def: None,
        }
    }

    /// Attach the artifact service that answers "which agents have work in
    /// flight", so rows carry a real `busy` bit instead of `null`.
    pub fn with_activity_source(mut self, activity: Arc<ArtifactV2Service>) -> Self {
        self.activity = Some(activity);
        self
    }

    /// Share the runtime's definition store rather than building one per read.
    /// See the field comment for why this matters beyond speed.
    pub fn with_definition_store(mut self, definition_store: Arc<AgentDefinitionStore>) -> Self {
        self.definition_store = Some(definition_store);
        self
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    /// A scope-bound view of the definition store.
    ///
    /// `for_scope` re-points storage at the scope's own agent runtime root and
    /// inherits the cache handle, so this is a read of the scope's real agents
    /// — never the seed templates that a bare `with_workspace_layout` would
    /// address.
    fn scoped_store(&self, scope: &Scope) -> AgentDefinitionStore {
        if let Some(shared) = &self.definition_store {
            let scoped = shared.for_scope(&scope.principal, &scope.workspace);
            // `for_scope` is a no-op on a store built without a workspace
            // layout — it returns `self.clone()` and silently keeps whatever
            // root that store points at. For a binder whose entire contract is
            // executor-owned scope, "silently unscoped" is the one outcome that
            // must never ship, so the scope is verified rather than assumed.
            if scoped
                .current_scope()
                .is_some_and(|(principal, workspace)| {
                    principal == scope.principal && workspace == scope.workspace
                })
            {
                return scoped;
            }
        }
        // Cold, and correct: reached when no store was shared, or when the
        // shared one could not be re-pointed at this scope.
        AgentDefinitionStore::with_workspace_layout(self.workspace_layout.clone())
            .for_scope(&scope.principal, &scope.workspace)
    }
}

#[async_trait]
impl CapabilityProvider for AgentRosterDataProvider {
    fn tool_name(&self) -> &str {
        AGENT_ROSTER_DATA_TOOL_NAME
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_agent_roster_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: AGENT_ROSTER_DATA_TOOL_NAME.to_owned(),
            implementation: ImplementationType::Compiled {
                provider_name: AGENT_ROSTER_DATA_TOOL_NAME.to_owned(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params.clone(),
            _ => {
                return Err(ExecutionError::Step(
                    "agent_roster_data: unexpected action type".to_owned(),
                ))
            },
        };
        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "list_members".to_owned());
        let mut params = authorize_runtime_scope(params)?;
        params.insert(
            "__action_name".to_owned(),
            Value::String(action_name.clone()),
        );
        if !prove_app_agent_roster_args(&params) {
            return Err(ExecutionError::Step(
                "agent_roster_data arguments are outside the closed action schema".to_owned(),
            ));
        }
        let input_bytes = serde_json::to_vec(&params).map_err(|error| {
            ExecutionError::Step(format!(
                "agent_roster_data argument serialization failed: {error}"
            ))
        })?;
        if input_bytes.len() as u64 > APP_BOUND_AGENT_ROSTER_INPUT_CEILING {
            return Err(ExecutionError::Step(format!(
                "agent_roster_data arguments exceeded the {} byte ceiling",
                APP_BOUND_AGENT_ROSTER_INPUT_CEILING
            )));
        }
        let effective_timeout = timeout_secs.max(1);
        let value = timeout(
            Duration::from_secs(effective_timeout),
            self.execute_agent_roster_action(&action_name, &params),
        )
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "agent_roster_data action `{action_name}` timed out after {effective_timeout}s"
            ))
        })??;
        let rendered = serde_json::to_string_pretty(&value).map_err(|error| {
            ExecutionError::Step(format!(
                "agent_roster_data result serialization failed: {error}"
            ))
        })?;
        if rendered.len() as u64 > APP_BOUND_AGENT_ROSTER_RESULT_CEILING {
            return Err(ExecutionError::Step(format!(
                "agent_roster_data result exceeded the {} byte ceiling",
                APP_BOUND_AGENT_ROSTER_RESULT_CEILING
            )));
        }
        Ok(ActionResult::text(rendered))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.pack_def
            .as_ref()
            .and_then(|pack| pack.execution.as_ref())
            .and_then(|execution| execution.default_timeout_secs)
            .unwrap_or(30)
    }
}

// ---------------------------------------------------------------------------
// Closed argument proof
// ---------------------------------------------------------------------------

fn prove_app_agent_roster_args(parameters: &HashMap<String, Value>) -> bool {
    let Some(operation) = parameters.get("__action_name").and_then(Value::as_str) else {
        return false;
    };
    if !ACTIONS.contains(&operation) {
        return false;
    }

    for (key, value) in parameters {
        match key.as_str() {
            "__action_name" => {},
            "operation" | "action" | "method" => {
                let agrees = value.as_str().is_some_and(|alias| {
                    crate::magician_v2::apps::app_tool_bind::normalize_app_action_selector(
                        AGENT_ROSTER_DATA_TOOL_NAME,
                        alias,
                    )
                    .as_deref()
                        == Some(operation)
                });
                if !agrees {
                    return false;
                }
            },
            "principal" | "workspace" => {
                if !bounded_nonblank_string(value, MAX_ID_BYTES) {
                    return false;
                }
            },
            "limit" if operation == "list_members" => {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=MAX_LIST_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            "after_agent_id" if operation == "list_members" => {
                if !bounded_agent_id(value) {
                    return false;
                }
            },
            "agent_id" if operation == "read_member" => {
                if !bounded_agent_id(value) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }

    match operation {
        // An exact read has no defaultable target: "read whichever member" is
        // not a meaningful request.
        "read_member" => parameters.contains_key("agent_id"),
        _ => true,
    }
}

fn bounded_nonblank_string(value: &Value, max_bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= max_bytes)
}

/// The id space this binder can address.
///
/// This must be exactly the id space the definition store can *produce*, and
/// the reason is the paging contract: `list_members` emits a member's
/// `agent_id` as the cursor, and this same proof admits it on the way back in.
/// Any narrower rule stalls paging permanently on the first id it refuses —
/// `system:scheduler` is a seed agent and the ready-made counterexample, since
/// `validate_agent_identifier` bars traversal, separators, reserved names and
/// non-ASCII but says nothing about `:`, spaces or most punctuation.
///
/// So the rule is the store's own check plus a refusal of ASCII control
/// characters, which have no business in a routing key and which no reachable
/// id generator emits (`slugify_name` produces `[a-z0-9-]` only). `scoped_roster`
/// drops anything this refuses, so the emit/admit invariant holds by
/// construction rather than by the two id spaces happening to line up.
fn admissible_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && !id.bytes().any(|byte| byte.is_ascii_control())
        && validate_agent_identifier(id).is_ok()
}

fn bounded_agent_id(value: &Value) -> bool {
    value.as_str().is_some_and(admissible_agent_id)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

impl AgentRosterDataProvider {
    async fn execute_agent_roster_action(
        &self,
        action: &str,
        params: &HashMap<String, Value>,
    ) -> Result<Value, ExecutionError> {
        match action {
            "list_members" => self.list_members(params).await,
            "read_member" => self.read_member(params).await,
            other => Err(ExecutionError::Step(format!(
                "agent_roster_data: unknown action `{other}`"
            ))),
        }
    }
}

/// The participation face of one agent definition. Every field here is either
/// identity or a `social_persona` value; nothing about the agent's prompt,
/// tools, routing or delegation is projected.
#[derive(Debug, Serialize)]
struct VisibleRosterMember {
    agent_id: String,
    display_name: String,
    /// The definition's own `disabled` flag, inverted once here so every
    /// consumer reads the same polarity. A disabled agent stays visible on
    /// purpose: a package reconciling membership must be able to tell
    /// "disabled" from "gone".
    enabled: bool,
    /// `None` when the definition declares no `social_persona` block at all —
    /// which is NOT the same as declaring one with default values, and a
    /// participation policy must be able to tell those apart before it opts
    /// an agent into a public square.
    declares_social_persona: bool,
    introversion: Option<f64>,
    daily_tokens: Option<u64>,
    opted_out: Option<bool>,
    /// Whether the agent has work in flight at the moment of the read: it owns
    /// a running task, or is running an execution (a delegated child counts)
    /// under one. App-workflow tasks never count — a behavior round runs under
    /// its host agent, and that agent must still be able to take a turn in the
    /// round it hosts. `None` only when the binder has no activity source; a
    /// consumer must treat that as "not known", not as idle.
    busy: Option<bool>,
}

/// The working-agent set for one read, or `None` when it could not be
/// determined. Every row of the read shares one answer, so a page never mixes
/// two moments' worth of activity.
type WorkingAgents = Option<std::collections::HashSet<String>>;

fn visible_member(record: &DefinitionRecord, working: &WorkingAgents) -> VisibleRosterMember {
    let definition = &record.definition;
    let persona = definition.social_persona.as_ref();
    VisibleRosterMember {
        agent_id: definition.agent_id.clone(),
        display_name: bounded_text(&definition.name, MAX_DISPLAY_NAME_BYTES),
        enabled: !definition.disabled,
        declares_social_persona: persona.is_some(),
        introversion: persona.map(|persona| persona.introversion),
        daily_tokens: persona.map(|persona| persona.daily_tokens),
        opted_out: persona.map(|persona| persona.opted_out),
        busy: working
            .as_ref()
            .map(|working| working.contains(&definition.agent_id)),
    }
}

impl AgentRosterDataProvider {
    /// The scope's definitions, borrowed from the store's cached `Arc`.
    ///
    /// Deliberately not `list_definitions`: that hands back an owned `Vec`, and
    /// because the cache holds the `Arc` its `try_unwrap` always fails, so every
    /// poll would deep-copy every record — each carrying a whole prompt, tool
    /// grants and a delegation graph — to project seven scalars off it.
    async fn scoped_roster(
        &self,
        scope: &Scope,
    ) -> Result<Arc<Vec<DefinitionRecord>>, ExecutionError> {
        self.scoped_store(scope)
            .list_definitions_shared()
            .await
            .map_err(|error| {
                ExecutionError::Step(format!(
                    "agent_roster_data reading the scope's agent roster failed: {error}"
                ))
            })
    }
}

impl AgentRosterDataProvider {
    /// The agents with work in flight in this scope, or `None` when there is
    /// no activity source or the lookup failed. A failure degrades the read to
    /// `busy: null` rather than failing it: the roster is still correct, only
    /// this one bit is unknown, and the warning says so.
    async fn working_agents(&self, scope: &Scope) -> WorkingAgents {
        let activity = self.activity.as_ref()?;
        let scope_ref =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        match activity.working_agent_ids(&scope_ref).await {
            Ok(agents) => Some(agents),
            Err(error) => {
                tracing::warn!(
                    principal = %scope.principal,
                    workspace = %scope.workspace,
                    %error,
                    "agent_roster_data could not determine which agents are busy; rows report busy: null"
                );
                None
            },
        }
    }

    async fn list_members(&self, params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let records = self.scoped_roster(&scope).await?;
        let working = self.working_agents(&scope).await;
        // Borrowed, sorted, and filtered to what this binder can address. The
        // filter is what makes the paging invariant structural: a row whose id
        // the argument proof would refuse can never become the cursor that
        // pages past it. Total order on a stable key, because the store's own
        // ordering is not guaranteed and a page boundary over an unstable order
        // silently skips or repeats.
        let mut order = records
            .iter()
            .filter(|record| admissible_agent_id(&record.definition.agent_id))
            .collect::<Vec<_>>();
        order.sort_by(|left, right| left.definition.agent_id.cmp(&right.definition.agent_id));
        let records = order;

        // The cursor is applied to the total order FIRST and the scan budget to
        // whatever follows it. Budgeting before the cursor caps the *listing*
        // rather than the page: every cursor past the budget would then yield an
        // empty page with a null `next_cursor`, which a reconciler reads as "you
        // have the whole roster" rather than "truncated" — silently dropping the
        // tail of a large roster. Applied after, paging walks past the budget a
        // page at a time and `scan_truncated` means what it says.
        let start = match string_param(params, "after_agent_id") {
            Some(cursor) => records.partition_point(|record| record.definition.agent_id <= cursor),
            None => 0,
        };
        let after_cursor = &records[start..];
        let scan_truncated = after_cursor.len() > ROSTER_SCAN_BUDGET;
        let scanned = &after_cursor[..after_cursor.len().min(ROSTER_SCAN_BUDGET)];
        let limit = bounded_limit(params);
        let has_more = scanned.len() > limit;
        let page = &scanned[..scanned.len().min(limit)];
        let members = page
            .iter()
            .map(|record| visible_member(record, &working))
            .collect::<Vec<_>>();
        // Keyset over a total order: the cursor is compared positionally, so a
        // member removed between two pages still defines a valid boundary. The
        // emitted cursor is an agent id that already passed `bounded_agent_id`
        // on the way in, so it is admissible on the way back.
        let next_cursor = has_more
            .then(|| members.last().map(|member| member.agent_id.clone()))
            .flatten();

        Ok(json!({
            "scope": scope.as_json(),
            "members": members,
            "next_cursor": next_cursor,
            "scan_truncated": scan_truncated,
            "scan_budget": ROSTER_SCAN_BUDGET,
        }))
    }

    async fn read_member(&self, params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let agent_id = required_string(params, "agent_id")?;
        // An exact read is an exact read: one keyed lookup, no full listing and
        // no sort. This also removes a budget asymmetry a scan-then-find would
        // carry, where a member past the listing's scan budget was readable by
        // id but never listable.
        let record = self
            .scoped_store(&scope)
            .get_definition(&agent_id)
            .await
            .map_err(|error| {
                ExecutionError::Step(format!(
                    "agent_roster_data reading agent `{agent_id}` failed: {error}"
                ))
            })?;
        let working = match record.as_ref() {
            Some(_) => self.working_agents(&scope).await,
            None => None,
        };
        let member = record
            .as_ref()
            .map(|record| visible_member(record, &working));
        let present = member.is_some();

        Ok(json!({
            "scope": scope.as_json(),
            // Absent is a first-class answer, not an error: a package
            // reconciling its membership needs to learn that an agent it holds a
            // row for is gone, and an error would make "retired"
            // indistinguishable from "the read failed".
            "member": member,
            "present": present,
        }))
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Truncate on a character boundary and mark the cut. Never slices bytes.
fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[derive(Debug, Clone)]
struct Scope {
    principal: String,
    workspace: String,
}

impl Scope {
    fn as_json(&self) -> Value {
        json!({"principal": self.principal, "workspace": self.workspace})
    }
}

fn scope_from_params(params: &HashMap<String, Value>) -> Result<Scope, ExecutionError> {
    Ok(Scope {
        principal: required_runtime_scope_value(params, "__principal")?,
        workspace: required_runtime_scope_value(params, "__workspace")?,
    })
}

fn authorize_runtime_scope(
    mut params: HashMap<String, Value>,
) -> Result<HashMap<String, Value>, ExecutionError> {
    let principal = required_runtime_scope_value(&params, "__principal")?;
    let workspace = required_runtime_scope_value(&params, "__workspace")?;
    if !LlmScope::new(&principal, &workspace).is_valid() {
        return Err(ExecutionError::Step(
            "agent_roster_data runtime scope contains an unsafe principal or workspace component"
                .to_owned(),
        ));
    }
    for (public_key, trusted_value) in [
        ("principal", principal.as_str()),
        ("workspace", workspace.as_str()),
    ] {
        if let Some(value) = params.get(public_key) {
            let Value::String(value) = value else {
                return Err(ExecutionError::Step(format!(
                    "agent_roster_data: `{public_key}` is an optional scope assertion and must be a string"
                )));
            };
            if !value.is_empty() && value != trusted_value {
                return Err(ExecutionError::Step(format!(
                    "agent_roster_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
                )));
            }
        }
    }
    params.insert("principal".to_owned(), Value::String(principal));
    params.insert("workspace".to_owned(), Value::String(workspace));
    Ok(params)
}

fn required_runtime_scope_value(
    params: &HashMap<String, Value>,
    key: &str,
) -> Result<String, ExecutionError> {
    let value = params.get(key).and_then(Value::as_str).ok_or_else(|| {
        ExecutionError::Step(format!(
            "agent_roster_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "agent_roster_data runtime-owned scope `{key}` must be a nonblank canonical component"
        )));
    }
    Ok(value.to_owned())
}

fn bounded_limit(params: &HashMap<String, Value>) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_LIST_LIMIT as u64)
        .clamp(1, MAX_LIST_LIMIT) as usize
}

fn string_param(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn required_string(params: &HashMap<String, Value>, key: &str) -> Result<String, ExecutionError> {
    string_param(params, key).ok_or_else(|| {
        ExecutionError::Step(format!(
            "agent_roster_data: `{key}` must be a non-empty string"
        ))
    })
}

/// Compile-time reminder that the roster binder never grows a write verb.
/// Editing an agent definition is the agent-definition owner's job; a
/// participation policy reads the roster and owns nothing about it.
const _: () = {
    let mut index = 0;
    while index < ACTIONS.len() {
        let action = ACTIONS[index].as_bytes();
        assert!(
            !starts_with(action, b"set")
                && !starts_with(action, b"update")
                && !starts_with(action, b"create")
                && !starts_with(action, b"delete")
                && !starts_with(action, b"retire")
                && !starts_with(action, b"disable"),
            "agent_roster_data is a read binder; definition edits belong to the agent owner"
        );
        index += 1;
    }
};

const fn starts_with(value: &[u8], prefix: &[u8]) -> bool {
    if value.len() < prefix.len() {
        return false;
    }
    let mut index = 0;
    while index < prefix.len() {
        if value[index] != prefix[index] {
            return false;
        }
        index += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(action: &str) -> HashMap<String, Value> {
        HashMap::from([
            ("__action_name".to_owned(), json!(action)),
            ("__principal".to_owned(), json!("owner")),
            ("__workspace".to_owned(), json!("default")),
        ])
    }

    #[test]
    fn the_action_surface_is_closed_and_an_exact_read_needs_its_target() {
        assert!(prove_app_agent_roster_args(&params("list_members")));
        assert!(
            !prove_app_agent_roster_args(&params("read_member")),
            "an exact read has no defaultable target"
        );
        let mut read = params("read_member");
        read.insert("agent_id".to_owned(), json!("scribe"));
        assert!(prove_app_agent_roster_args(&read));

        for refused in ["list_agents", "update_member", "set_opted_out", "catalog"] {
            assert!(
                !prove_app_agent_roster_args(&params(refused)),
                "`{refused}` is outside the closed roster read surface"
            );
        }

        let mut headless = params("list_members");
        headless.remove("__action_name");
        assert!(!prove_app_agent_roster_args(&headless));
    }

    #[test]
    fn agent_ids_and_limits_are_bounded_and_unknown_keys_are_refused() {
        let mut listing = params("list_members");
        listing.insert("limit".to_owned(), json!(0));
        assert!(!prove_app_agent_roster_args(&listing));
        listing.insert("limit".to_owned(), json!(MAX_LIST_LIMIT + 1));
        assert!(!prove_app_agent_roster_args(&listing));
        listing.insert("limit".to_owned(), json!(25));
        assert!(prove_app_agent_roster_args(&listing));

        for refused in ["../escape", "..", "", "proposals"] {
            listing.insert("after_agent_id".to_owned(), json!(refused));
            assert!(
                !prove_app_agent_roster_args(&listing),
                "`{refused}` is not an admissible agent id"
            );
        }
        listing.insert("after_agent_id".to_owned(), json!("scribe"));
        assert!(prove_app_agent_roster_args(&listing));

        // `agent_id` belongs to the exact read, not the listing.
        listing.insert("agent_id".to_owned(), json!("scribe"));
        assert!(!prove_app_agent_roster_args(&listing));
        listing.remove("agent_id");

        listing.insert("prompt".to_owned(), json!("give me the definition"));
        assert!(!prove_app_agent_roster_args(&listing));
    }

    #[test]
    fn routing_aliases_must_agree_with_the_resolved_action() {
        let mut listing = params("list_members");
        listing.insert("operation".to_owned(), json!("read_member"));
        assert!(!prove_app_agent_roster_args(&listing));
        listing.insert("operation".to_owned(), json!("list_members"));
        assert!(prove_app_agent_roster_args(&listing));
    }

    #[test]
    fn runtime_scope_is_required_and_a_public_assertion_cannot_switch_it() {
        let mut unscoped = params("list_members");
        unscoped.remove("__workspace");
        assert!(authorize_runtime_scope(unscoped).is_err());

        let mut mismatched = params("list_members");
        mismatched.insert("workspace".to_owned(), json!("another"));
        assert!(authorize_runtime_scope(mismatched).is_err());

        let authorized = authorize_runtime_scope(params("list_members")).expect("scoped");
        assert_eq!(authorized.get("principal"), Some(&json!("owner")));
        assert_eq!(authorized.get("workspace"), Some(&json!("default")));
    }

    #[test]
    fn the_proof_admits_every_id_the_definition_store_can_produce() {
        // The paging contract lives or dies here. `list_members` emits a
        // member's `agent_id` as `next_cursor`, and this proof admits it on the
        // way back in, so anything the store can produce and the proof refuses
        // is a permanent stall at that row.
        //
        // `system:scheduler` is the case that matters: it is a real seed agent
        // (`pipeline::agent::AGENT_ID_SCHEDULER`), it materializes into every
        // scope, and it sorts in the middle of the seed roster — so a page
        // boundary lands on it and the next request dies. A charset that "looks
        // like an agent id" excludes it.
        for produced in [
            "scribe",
            "agent-7",
            "a_b",
            "assistant@example.com",
            "v1.2",
            "system:scheduler",
            "with space",
            "a+b",
        ] {
            assert!(
                validate_agent_identifier(produced).is_ok(),
                "`{produced}` must really be storable, or this test proves nothing"
            );
            assert!(
                bounded_agent_id(&json!(produced)),
                "`{produced}` is storable, so it is emittable as a cursor and must be admissible"
            );
        }

        // Refused: traversal, separators, reserved names, non-ASCII, control
        // characters and the length bound.
        for refused in [
            "..",
            ".",
            "a..b",
            "a/b",
            "a\\b",
            "proposals",
            "",
            " lead",
            "trail ",
            "é",
            "a\u{7}b",
        ] {
            assert!(
                !bounded_agent_id(&json!(refused)),
                "`{refused:?}` must not be addressable"
            );
        }
        assert!(!bounded_agent_id(&json!("a".repeat(MAX_ID_BYTES + 1))));
    }

    #[test]
    fn a_cursor_this_binder_emits_is_a_cursor_it_will_accept() {
        // The same invariant stated from the argument-proof side, over the ids
        // the seed roster actually contains.
        let mut listing = params("list_members");
        for emitted in ["scribe", "agent-7", "assistant@example.com", "system:scheduler"] {
            listing.insert("after_agent_id".to_owned(), json!(emitted));
            assert!(
                prove_app_agent_roster_args(&listing),
                "`{emitted}` is emittable as a cursor and must be admissible as one"
            );
        }
        // And a row the proof would refuse never reaches a response in the
        // first place, so the invariant holds by construction, not by luck.
        assert!(!admissible_agent_id("bad\u{7}id"));
    }

    #[test]
    fn display_names_truncate_on_character_boundaries() {
        let wide = "é".repeat(400);
        let bounded = bounded_text(&wide, 32);
        assert!(bounded.len() <= 32);
        assert!(bounded.ends_with('…'));
    }
}
