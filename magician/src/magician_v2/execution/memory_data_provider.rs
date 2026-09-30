//! Owner-controlled, read-only Apps binder for the owner's memory
//! (`app_memory_read_v1`).
//!
//! An app reads only what its owner granted for the run mode it is in:
//! named user tiers (facts about the owner) and named agents' learned memory
//! (agent and agent-goal scope), plus the memory the app itself contributed.
//! The grant is re-read from the app registry on every call, so an owner who
//! narrows or revokes access is obeyed on the app's next read.
//!
//! Identity and run mode come only from the executor, which stamps
//! `__app_installation_id` / `__app_run_mode` from the task's own app workflow
//! binding after stripping every model-supplied `__*` key. Outside an app
//! workflow nothing is stamped and every call is refused.
//!
//! Containment, per candidate:
//! - owner-only engagement binding: engagement- and meeting-labelled memory is
//!   never returned, whatever the grant says;
//! - app-sourced memory: only this app's own records, and only while still
//!   eligible — never another app's promoted memory, even in a granted tier;
//! - owner memory: only tiers / agents in this run mode's grant;
//! - superseded records are dropped (lifecycle and the temperature overlay,
//!   read as a snapshot).
//!
//! Read-only: unlike `search_memory`, this never syncs the temperature overlay
//! or records retrieval usage, so an app cannot skew what agents recall. It
//! never returns source paths, JSON pointers or raw metadata.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use magicllm::LlmScope;
use serde_json::{json, Value};
use tokio::time::timeout;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::agents::definition_store::AgentDefinitionStore;
use crate::magician_v2::agents::memory_candidates::{
    load_memory_candidate_documents, MemoryCandidateDocument, MemoryCandidateRequest,
};
use crate::magician_v2::agents::memory_temperature::{
    load_memory_temperature_overlay_snapshot, memory_candidate_has_superseded_lifecycle,
    memory_temperature_candidate_key, memory_temperature_entry_is_superseded,
    parse_memory_temperature_candidate_key,
};
use crate::magician_v2::agents::memory_tiers::TierScope;
use crate::magician_v2::agents::retrieval_scope::{
    label_from_metadata, ContextLabel, RetrievalScope,
};
use crate::magician_v2::agents::AgentMemoryResolver;
use crate::magician_v2::apps::memory_access::{AppMemoryReadSelection, AppMemoryRunMode};
use crate::magician_v2::apps::memory_access_store::effective_memory_read_grant_blocking;
use crate::magician_v2::apps::memory_bridge::parse_source_eligibility_envelope;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::chat::service::normalized_user_memory_tier_name;
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::PlanStep;

pub const MEMORY_DATA_TOOL_NAME: &str = "memory_data";

pub(crate) const APP_BOUND_MEMORY_DATA_INPUT_CEILING: u64 = 4 * 1024;
pub(crate) const APP_BOUND_MEMORY_DATA_RESULT_CEILING: u64 = 512 * 1024;

const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: u64 = 50;
const MAX_QUERY_BYTES: usize = 256;
const MIN_QUERY_CHARS: usize = 2;
const MAX_ID_BYTES: usize = 255;
const MAX_KEY_BYTES: usize = 2048;
const MAX_VALUE_BYTES: usize = 8 * 1024;

const ACTIONS: &[&str] = &["search_memory", "read_entry"];

#[derive(Clone)]
pub struct MemoryDataProvider {
    workspace_layout: ArtifactV2Workspace,
    definition_store: Option<Arc<AgentDefinitionStore>>,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for MemoryDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryDataProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("shares_definition_store", &self.definition_store.is_some())
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl MemoryDataProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            definition_store: None,
            pack_def: None,
        }
    }

    pub fn with_definition_store(mut self, definition_store: Arc<AgentDefinitionStore>) -> Self {
        self.definition_store = Some(definition_store);
        self
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    fn scoped_definitions(&self, scope: &Scope) -> AgentDefinitionStore {
        match &self.definition_store {
            Some(shared) => shared.for_scope(&scope.principal, &scope.workspace),
            None => AgentDefinitionStore::with_workspace_layout(self.workspace_layout.clone())
                .for_scope(&scope.principal, &scope.workspace),
        }
    }
}

#[async_trait]
impl CapabilityProvider for MemoryDataProvider {
    fn tool_name(&self) -> &str {
        MEMORY_DATA_TOOL_NAME
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_memory_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: MEMORY_DATA_TOOL_NAME.to_owned(),
            implementation: ImplementationType::Compiled {
                provider_name: MEMORY_DATA_TOOL_NAME.to_owned(),
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
                    "memory_data: unexpected action type".to_owned(),
                ))
            },
        };
        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "search_memory".to_owned());
        let mut params = authorize_runtime_scope(params)?;
        params.insert(
            "__action_name".to_owned(),
            Value::String(action_name.clone()),
        );
        if !prove_app_memory_args(&params) {
            return Err(ExecutionError::Step(
                "memory_data arguments are outside the closed action schema".to_owned(),
            ));
        }
        let input_bytes = serde_json::to_vec(&params).map_err(|error| {
            ExecutionError::Step(format!(
                "memory_data argument serialization failed: {error}"
            ))
        })?;
        if input_bytes.len() as u64 > APP_BOUND_MEMORY_DATA_INPUT_CEILING {
            return Err(ExecutionError::Step(format!(
                "memory_data arguments exceeded the {APP_BOUND_MEMORY_DATA_INPUT_CEILING} byte ceiling"
            )));
        }
        let effective_timeout = timeout_secs.max(1);
        let value = timeout(
            Duration::from_secs(effective_timeout),
            self.execute_memory_action(&action_name, &params),
        )
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "memory_data action `{action_name}` timed out after {effective_timeout}s"
            ))
        })??;
        let rendered = serde_json::to_string_pretty(&value).map_err(|error| {
            ExecutionError::Step(format!("memory_data result serialization failed: {error}"))
        })?;
        if rendered.len() as u64 > APP_BOUND_MEMORY_DATA_RESULT_CEILING {
            return Err(ExecutionError::Step(format!(
                "memory_data result exceeded the {APP_BOUND_MEMORY_DATA_RESULT_CEILING} byte ceiling"
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

fn prove_app_memory_args(parameters: &HashMap<String, Value>) -> bool {
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
                        MEMORY_DATA_TOOL_NAME,
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
            "query" if operation == "search_memory" => {
                if !value.as_str().is_some_and(admissible_query) {
                    return false;
                }
            },
            "limit" if operation == "search_memory" => {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=MAX_SEARCH_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            "key" if operation == "read_entry" => {
                if !value.as_str().is_some_and(admissible_key) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }
    match operation {
        "search_memory" => parameters.contains_key("query"),
        "read_entry" => parameters.contains_key("key"),
        _ => false,
    }
}

fn bounded_nonblank_string(value: &Value, max_bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= max_bytes)
}

fn admissible_query(query: &str) -> bool {
    query.trim().chars().count() >= MIN_QUERY_CHARS
        && query.len() <= MAX_QUERY_BYTES
        && !query.chars().any(char::is_control)
}

/// Keys are exactly what a search emits: the length-prefixed candidate key.
fn admissible_key(key: &str) -> bool {
    key.len() <= MAX_KEY_BYTES && parse_memory_temperature_candidate_key(key).is_some()
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Granted,
    OwnApp,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::OwnApp => "own_app",
        }
    }
}

/// Owner-only engagement binding: the owner's ordinary (unlabelled) and
/// explicitly neutral memory; never engagement- or meeting-bound memory.
fn owner_only_admits(candidate: &MemoryCandidateDocument) -> bool {
    matches!(
        label_from_metadata(&candidate.metadata_json),
        ContextLabel::Neutral | ContextLabel::Unlabelled
    )
}

/// Why a native (non-app-sourced) candidate is readable, or `None`.
fn native_candidate_granted(
    candidate: &MemoryCandidateDocument,
    selection: &AppMemoryReadSelection,
) -> bool {
    match candidate.scope {
        TierScope::User => normalized_user_memory_tier_name(&candidate.tier_name)
            .is_some_and(|tier| selection.grants_tier(&tier)),
        TierScope::Agent | TierScope::AgentGoal => candidate
            .agent_id
            .as_deref()
            .is_some_and(|agent| selection.grants_agent(agent)),
    }
}

/// Whether an app-sourced candidate was contributed by this installation
/// alone. A record with any other source is never this app's to read.
fn is_own_app_record(candidate: &MemoryCandidateDocument, installation_id: &str) -> Option<bool> {
    match parse_source_eligibility_envelope(&candidate.metadata_json)? {
        Err(_) => Some(false),
        Ok(envelope) => Some(
            !envelope.sources.is_empty()
                && envelope
                    .sources
                    .iter()
                    .all(|source| source.installation_id.as_str() == installation_id),
        ),
    }
}

#[derive(Debug)]
struct AppIdentity {
    installation_id: String,
    run_mode: AppMemoryRunMode,
}

fn app_identity(params: &HashMap<String, Value>) -> Result<AppIdentity, ExecutionError> {
    let refused = || {
        ExecutionError::Step(
            "memory_data is app-only: it reads the owner's memory under an app's grant, and this \
             call carries no app identity"
                .to_owned(),
        )
    };
    let installation_id = string_param(params, "__app_installation_id").ok_or_else(refused)?;
    let run_mode = string_param(params, "__app_run_mode")
        .as_deref()
        .and_then(AppMemoryRunMode::parse)
        .ok_or_else(refused)?;
    Ok(AppIdentity {
        installation_id,
        run_mode,
    })
}

impl MemoryDataProvider {
    async fn execute_memory_action(
        &self,
        action: &str,
        params: &HashMap<String, Value>,
    ) -> Result<Value, ExecutionError> {
        match action {
            "search_memory" => self.search_memory(params).await,
            "read_entry" => self.read_entry(params).await,
            other => Err(ExecutionError::Step(format!(
                "memory_data: unknown action `{other}`"
            ))),
        }
    }

    /// Every candidate this call may read, each with its origin.
    async fn admitted_candidates(
        &self,
        scope: &Scope,
        identity: &AppIdentity,
        params: &HashMap<String, Value>,
    ) -> Result<(Vec<(MemoryCandidateDocument, Origin)>, bool), ExecutionError> {
        // Owner-only binding: an engagement-bound execution never reads owner
        // memory through an app.
        if params.contains_key("__engagement_id") {
            return Err(ExecutionError::Step(
                "memory_data: this app is bound to the owner only and cannot read memory inside \
                 an engagement"
                    .to_owned(),
            ));
        }
        let workspace = self.workspace_layout.clone();
        let (principal, workspace_name, installation_id) = (
            scope.principal.clone(),
            scope.workspace.clone(),
            identity.installation_id.clone(),
        );
        let grant = tokio::task::spawn_blocking(move || {
            effective_memory_read_grant_blocking(
                &workspace,
                &principal,
                &workspace_name,
                &installation_id,
            )
        })
        .await
        .map_err(|error| ExecutionError::Step(format!("memory_data grant lookup failed: {error}")))?
        .map_err(|error| {
            ExecutionError::Step(format!("memory_data grant lookup failed: {error}"))
        })?;
        let empty = AppMemoryReadSelection::default();
        let selection = grant
            .as_ref()
            .map(|(_, grant)| grant.selection(identity.run_mode).clone())
            .unwrap_or(empty);

        let memory = AgentMemoryResolver::with_workspace_layout(self.workspace_layout.clone())
            .resolve_for_scope(&scope.principal, &scope.workspace)
            .map_err(|error| ExecutionError::Step(format!("memory_data: {error}")))?;
        let storage = memory.storage();
        let sentinel = format!("app:{}", identity.installation_id);
        let mut loaded = Vec::new();
        // User scope is always loaded: the app's own contributions can live
        // there even when no user tier is granted.
        let user = load_memory_candidate_documents(
            storage,
            &sentinel,
            &[],
            &MemoryCandidateRequest {
                scope: TierScope::User,
                goal_id: None,
                recency_cutoff: None,
                include_environment_knowledge: false,
                retrieval_scope: RetrievalScope::default(),
            },
        )
        .await
        .map_err(|error| {
            ExecutionError::Step(format!("memory_data: loading user memory failed: {error}"))
        })?;
        loaded.extend(user);
        for agent_id in &selection.agents {
            let Some(record) = self
                .scoped_definitions(scope)
                .get_definition(agent_id)
                .await
                .map_err(|error| {
                    ExecutionError::Step(format!(
                        "memory_data: reading agent `{agent_id}` failed: {error}"
                    ))
                })?
            else {
                continue;
            };
            let tiers = &record.definition.memory_tiers;
            let agent = load_memory_candidate_documents(
                storage,
                agent_id,
                tiers,
                &MemoryCandidateRequest {
                    scope: TierScope::Agent,
                    goal_id: None,
                    recency_cutoff: None,
                    include_environment_knowledge: true,
                    retrieval_scope: RetrievalScope::default(),
                },
            )
            .await
            .map_err(|error| ExecutionError::Step(format!("memory_data: {error}")))?;
            loaded.extend(agent);
            for tier in tiers
                .iter()
                .filter(|tier| matches!(tier.scope, TierScope::AgentGoal))
            {
                let goal_ids =
                    crate::magician_v2::agents::memory_index::discover_goal_ids_for_tier(
                        storage, agent_id, tier,
                    )
                    .await
                    .unwrap_or_default();
                for goal_id in goal_ids {
                    let goal = load_memory_candidate_documents(
                        storage,
                        agent_id,
                        std::slice::from_ref(tier),
                        &MemoryCandidateRequest {
                            scope: TierScope::AgentGoal,
                            goal_id: Some(&goal_id),
                            recency_cutoff: None,
                            include_environment_knowledge: true,
                            retrieval_scope: RetrievalScope::default(),
                        },
                    )
                    .await
                    .map_err(|error| ExecutionError::Step(format!("memory_data: {error}")))?;
                    loaded.extend(goal);
                }
            }
        }

        // App-sourced records: live eligibility for the ones that could be
        // this app's own. Read-only overlay snapshot for supersession.
        let own_metadata = loaded
            .iter()
            .filter(|candidate| {
                is_own_app_record(candidate, &identity.installation_id) == Some(true)
            })
            .map(|candidate| candidate.metadata_json.clone())
            .collect::<Vec<_>>();
        let eligibility = if own_metadata.is_empty() {
            HashMap::new()
        } else {
            memory
                .app_memory_prompt_eligibility(own_metadata, chrono::Utc::now())
                .await
        };
        let overlay = load_memory_temperature_overlay_snapshot(storage).await.ok();

        let mut seen = std::collections::HashSet::new();
        let mut admitted = Vec::new();
        for candidate in loaded {
            if !owner_only_admits(&candidate)
                || memory_candidate_has_superseded_lifecycle(&candidate)
            {
                continue;
            }
            let key = memory_temperature_candidate_key(&candidate);
            if overlay
                .as_ref()
                .and_then(|overlay| overlay.entries.get(&key))
                .is_some_and(memory_temperature_entry_is_superseded)
            {
                continue;
            }
            let origin = match is_own_app_record(&candidate, &identity.installation_id) {
                Some(true) => {
                    let eligible = parse_source_eligibility_envelope(&candidate.metadata_json)
                        .and_then(Result::ok)
                        .is_some_and(|envelope| {
                            eligibility.get(&envelope.candidate_id.to_string()) == Some(&true)
                        });
                    if !eligible {
                        continue;
                    }
                    Origin::OwnApp
                },
                // Another app's (or a malformed) app-sourced record.
                Some(false) => continue,
                None if native_candidate_granted(&candidate, &selection) => Origin::Granted,
                None => continue,
            };
            if seen.insert(key) {
                admitted.push((candidate, origin));
            }
        }
        Ok((admitted, grant.is_some()))
    }

    async fn search_memory(
        &self,
        params: &HashMap<String, Value>,
    ) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let identity = app_identity(params)?;
        let query = string_param(params, "query").ok_or_else(|| {
            ExecutionError::Step("memory_data: `query` must be a non-empty string".to_owned())
        })?;
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_SEARCH_LIMIT, |limit| {
                limit.clamp(1, MAX_SEARCH_LIMIT) as usize
            });
        let (admitted, has_grant) = self.admitted_candidates(&scope, &identity, params).await?;
        let tokens = query_tokens(&query);
        let mut scored = admitted
            .into_iter()
            .filter_map(|(candidate, origin)| {
                let score = keyword_score(&candidate, &tokens, &query.to_ascii_lowercase());
                (score > 0).then_some((score, candidate, origin))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.last_updated.cmp(&left.1.last_updated))
        });
        let matches = scored
            .into_iter()
            .take(limit)
            .map(|(score, candidate, origin)| visible_entry(&candidate, origin, Some(score)))
            .collect::<Vec<_>>();
        Ok(json!({
            "scope": scope.as_json(),
            "run_mode": identity.run_mode.as_str(),
            "granted": has_grant,
            "matches": matches,
        }))
    }

    async fn read_entry(&self, params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let identity = app_identity(params)?;
        let key = string_param(params, "key").ok_or_else(|| {
            ExecutionError::Step("memory_data: `key` must be a non-empty string".to_owned())
        })?;
        let (admitted, _) = self.admitted_candidates(&scope, &identity, params).await?;
        let entry = admitted
            .iter()
            .find(|(candidate, _)| memory_temperature_candidate_key(candidate) == key)
            .map(|(candidate, origin)| visible_entry(candidate, *origin, None));
        let present = entry.is_some();
        Ok(json!({
            "scope": scope.as_json(),
            "run_mode": identity.run_mode.as_str(),
            // Absent covers "gone" and "not granted" alike: an app must not be
            // able to probe for memory it cannot read.
            "entry": entry,
            "present": present,
        }))
    }
}

fn visible_entry(
    candidate: &MemoryCandidateDocument,
    origin: Origin,
    score: Option<usize>,
) -> Value {
    let scope = match candidate.scope {
        TierScope::User => "user",
        TierScope::Agent => "agent",
        TierScope::AgentGoal => "agent_goal",
    };
    let tier = match candidate.scope {
        TierScope::User => normalized_user_memory_tier_name(&candidate.tier_name)
            .unwrap_or_else(|| candidate.tier_name.clone()),
        _ => candidate.tier_name.clone(),
    };
    let mut entry = json!({
        "key": memory_temperature_candidate_key(candidate),
        "scope": scope,
        "tier": tier,
        "value": bounded_text(&candidate.text, MAX_VALUE_BYTES),
        "last_updated": candidate.last_updated,
        "confidence": candidate.confidence,
        "origin": origin.as_str(),
    });
    if !matches!(candidate.scope, TierScope::User) {
        entry["agent_id"] = json!(candidate.agent_id);
    }
    if let Some(score) = score {
        entry["score"] = json!(score);
    }
    entry
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.chars().count() >= 2)
        .collect()
}

/// Same weighting as agent memory search's keyword pass: tier/key hits count
/// double, text hits once.
fn keyword_score(candidate: &MemoryCandidateDocument, tokens: &[String], needle: &str) -> usize {
    let text = candidate.text.to_ascii_lowercase();
    let tier = candidate.tier_name.to_ascii_lowercase();
    let key = candidate.item_key.to_ascii_lowercase();
    if tokens.is_empty() {
        return usize::from(text.contains(needle) || tier.contains(needle) || key.contains(needle));
    }
    tokens
        .iter()
        .map(|token| {
            let mut score = 0;
            if tier.contains(token.as_str()) || key.contains(token.as_str()) {
                score += 2;
            }
            if text.contains(token.as_str()) {
                score += 1;
            }
            score
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

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
            "memory_data runtime scope contains an unsafe principal or workspace component"
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
                    "memory_data: `{public_key}` is an optional scope assertion and must be a string"
                )));
            };
            if !value.is_empty() && value != trusted_value {
                return Err(ExecutionError::Step(format!(
                    "memory_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
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
            "memory_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "memory_data runtime-owned scope `{key}` must be a nonblank canonical component"
        )));
    }
    Ok(value.to_owned())
}

fn string_param(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Compile-time reminder that the memory binder never grows a write verb.
const _: () = {
    let mut index = 0;
    while index < ACTIONS.len() {
        let action = ACTIONS[index].as_bytes();
        assert!(
            !starts_with(action, b"save")
                && !starts_with(action, b"update")
                && !starts_with(action, b"forget")
                && !starts_with(action, b"delete")
                && !starts_with(action, b"write")
                && !starts_with(action, b"record")
                && !starts_with(action, b"propose"),
            "memory_data is a read binder; memory changes belong to the memory owner"
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
    use chrono::Utc;

    fn params(action: &str) -> HashMap<String, Value> {
        HashMap::from([
            ("__action_name".to_owned(), json!(action)),
            ("__principal".to_owned(), json!("owner")),
            ("__workspace".to_owned(), json!("default")),
        ])
    }

    fn candidate(
        scope: TierScope,
        tier: &str,
        agent: Option<&str>,
        metadata: Value,
    ) -> MemoryCandidateDocument {
        MemoryCandidateDocument {
            principal: Some("owner".into()),
            workspace: Some("default".into()),
            agent_id: agent.map(str::to_owned),
            scope,
            tier_name: tier.to_owned(),
            semantic_memory_type: Default::default(),
            goal_id: None,
            item_key: "k".into(),
            source_path: Some("/Users/owner/secret/path.json".into()),
            json_pointer: "/items/0".into(),
            content_hash: "h".into(),
            last_updated: Utc::now(),
            confidence: None,
            text: "Prefers morning meetings".into(),
            metadata_json: metadata,
        }
    }

    #[test]
    fn the_action_surface_is_closed_and_both_reads_need_a_target() {
        assert!(!prove_app_memory_args(&params("search_memory")));
        let mut search = params("search_memory");
        search.insert("query".to_owned(), json!("meetings"));
        assert!(prove_app_memory_args(&search));
        for refused in [
            "save_preference",
            "forget_memory",
            "update_memory_tier",
            "catalog",
        ] {
            assert!(!prove_app_memory_args(&params(refused)), "{refused}");
        }
        search.insert("tier".to_owned(), json!("identity"));
        assert!(
            !prove_app_memory_args(&search),
            "callers cannot pick tiers; the grant does"
        );
        let mut read = params("read_entry");
        read.insert("key".to_owned(), json!("not-a-candidate-key"));
        assert!(!prove_app_memory_args(&read));
    }

    #[test]
    fn calls_without_a_stamped_app_identity_are_refused() {
        assert!(app_identity(&params("search_memory")).is_err());
        let mut forged_mode = params("search_memory");
        forged_mode.insert("__app_installation_id".to_owned(), json!("inst_1"));
        forged_mode.insert("__app_run_mode".to_owned(), json!("owner"));
        assert!(
            app_identity(&forged_mode).is_err(),
            "only interactive/background exist"
        );
        forged_mode.insert("__app_run_mode".to_owned(), json!("background"));
        assert_eq!(
            app_identity(&forged_mode).unwrap().run_mode,
            AppMemoryRunMode::Background
        );
    }

    #[test]
    fn only_granted_tiers_and_agents_are_readable() {
        let selection = AppMemoryReadSelection {
            user_tiers: vec!["preferences".into()],
            agents: vec!["scribe".into()],
        };
        assert!(native_candidate_granted(
            &candidate(TierScope::User, "preferences", None, json!({})),
            &selection
        ));
        assert!(native_candidate_granted(
            &candidate(TierScope::User, "user.preferences", None, json!({})),
            &selection
        ));
        assert!(!native_candidate_granted(
            &candidate(TierScope::User, "identity", None, json!({})),
            &selection
        ));
        assert!(native_candidate_granted(
            &candidate(TierScope::Agent, "facts", Some("scribe"), json!({})),
            &selection
        ));
        assert!(!native_candidate_granted(
            &candidate(TierScope::AgentGoal, "facts", Some("planner"), json!({})),
            &selection
        ));
        assert!(
            !native_candidate_granted(
                &candidate(TierScope::User, "preferences", None, json!({})),
                &AppMemoryReadSelection::default()
            ),
            "an empty (e.g. background) grant reads nothing"
        );
    }

    #[test]
    fn owner_only_binding_drops_engagement_and_meeting_memory() {
        assert!(owner_only_admits(&candidate(
            TierScope::User,
            "preferences",
            None,
            json!({})
        )));
        assert!(owner_only_admits(&candidate(
            TierScope::User,
            "preferences",
            None,
            json!({"engagement_scope": "neutral"})
        )));
        for bound in [
            json!({"engagement_scope": "engagement:acme"}),
            json!({"engagement_scope": "meeting:m1"}),
        ] {
            let bound_candidate = candidate(TierScope::User, "preferences", None, bound.clone());
            assert!(!owner_only_admits(&bound_candidate), "{bound}");
        }
    }

    #[test]
    fn visible_entries_never_carry_paths_pointers_or_raw_metadata() {
        let entry = visible_entry(
            &candidate(TierScope::User, "preferences", None, json!({"secret": "x"})),
            Origin::Granted,
            Some(3),
        );
        for hidden in ["source_path", "json_pointer", "metadata", "content_hash"] {
            assert!(
                entry.get(hidden).is_none(),
                "{hidden} must not be projected"
            );
        }
        assert_eq!(entry["origin"], "granted");
        assert_eq!(entry["tier"], "preferences");
    }

    #[test]
    fn keyword_scoring_matches_the_agent_search_weighting() {
        let item = candidate(TierScope::User, "preferences", None, json!({}));
        let tokens = query_tokens("morning preferences");
        // "preferences" hits the tier (2), "morning" hits the text (1).
        assert_eq!(keyword_score(&item, &tokens, "morning preferences"), 3);
        assert_eq!(keyword_score(&item, &query_tokens("zebra"), "zebra"), 0);
    }
}
