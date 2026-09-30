//! Scoped, read-only Apps binder for the owner's notes.
//!
//! The seventh host-read binder. `search_notes` and `open_note` are
//! agent-facing tools with no app implementation identity, and `open_note`
//! hands back an absolute host path, which an app must never see. This binder
//! is the narrow face an app may use: search the notes space, then read one
//! note it found.
//!
//! Both actions go through `NotesSettingsStore`'s observation-safe paths:
//! `search_notes` scans the same boundary-safe roots the Observe traversal
//! uses (which keeps model caches, credentials and worktrees that share the
//! notes root out of results), and `read_note` resolves a `source_ref` that a
//! search emitted through `read_observation_note`, which re-confines the path
//! to the provider root, refuses symlinks and reads markdown only. No caller
//! ever supplies a host path, and none is ever returned: `open_url` is dropped
//! because a local provider's link can carry one.
//!
//! Read-only (the binder-family invariant): no path here creates, appends to,
//! publishes or deletes a note, and a compile-time assertion below refuses an
//! action name that reads like one.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use magicllm::LlmScope;
use serde_json::{json, Value};
use tokio::time::timeout;

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::notes::{is_readable_note_path, NoteSearchRequest, NotesSettingsStore};
use crate::magician_v2::resource_authority::gated_action::MaybeGatedAction;
use crate::magician_v2::strategy::plan::PlanStep;

pub const NOTES_DATA_TOOL_NAME: &str = "notes_data";

pub(crate) const APP_BOUND_NOTES_DATA_INPUT_CEILING: u64 = 4 * 1024;
/// A note body is capped by the store at 256 KiB; the envelope adds headroom
/// for escaping and the surrounding fields.
pub(crate) const APP_BOUND_NOTES_DATA_RESULT_CEILING: u64 = 768 * 1024;

const MAX_SEARCH_LIMIT: u64 = 50;
const MAX_QUERY_BYTES: usize = 256;
const MIN_QUERY_CHARS: usize = 2;
const MAX_ID_BYTES: usize = 255;
const MAX_SOURCE_REF_BYTES: usize = 1024;
const MAX_TITLE_BYTES: usize = 512;
const MAX_MATCHES_PER_HIT: usize = 8;
const MAX_MATCH_TEXT_BYTES: usize = 512;

/// The providers a notes space can have. A `source_ref` naming anything else
/// can never resolve, so the proof refuses it up front.
const PROVIDERS: &[&str] = &["local_markdown", "silverbullet"];

const ACTIONS: &[&str] = &["search_notes", "read_note"];

#[derive(Clone)]
pub struct NotesDataProvider {
    workspace_layout: ArtifactV2Workspace,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for NotesDataProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotesDataProvider")
            .field("workspace_layout", &self.workspace_layout)
            .field("pack_def", &self.pack_def.as_ref().map(|pack| &pack.name))
            .finish()
    }
}

impl NotesDataProvider {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    fn store(&self) -> NotesSettingsStore {
        NotesSettingsStore::with_workspace_layout(self.workspace_layout.clone())
    }
}

#[async_trait]
impl CapabilityProvider for NotesDataProvider {
    fn tool_name(&self) -> &str {
        NOTES_DATA_TOOL_NAME
    }

    fn prove_app_tool_args(&self, parameters: &HashMap<String, Value>) -> bool {
        prove_app_notes_args(parameters)
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        let action = ExecutableAction::Pack {
            capability_name: NOTES_DATA_TOOL_NAME.to_owned(),
            implementation: ImplementationType::Compiled {
                provider_name: NOTES_DATA_TOOL_NAME.to_owned(),
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
                    "notes_data: unexpected action type".to_owned(),
                ))
            },
        };
        let action_name = string_param(&params, "__action_name")
            .or_else(|| string_param(&params, "action"))
            .unwrap_or_else(|| "search_notes".to_owned());
        let mut params = authorize_runtime_scope(params)?;
        params.insert(
            "__action_name".to_owned(),
            Value::String(action_name.clone()),
        );
        if !prove_app_notes_args(&params) {
            return Err(ExecutionError::Step(
                "notes_data arguments are outside the closed action schema".to_owned(),
            ));
        }
        let input_bytes = serde_json::to_vec(&params).map_err(|error| {
            ExecutionError::Step(format!("notes_data argument serialization failed: {error}"))
        })?;
        if input_bytes.len() as u64 > APP_BOUND_NOTES_DATA_INPUT_CEILING {
            return Err(ExecutionError::Step(format!(
                "notes_data arguments exceeded the {APP_BOUND_NOTES_DATA_INPUT_CEILING} byte ceiling"
            )));
        }
        let effective_timeout = timeout_secs.max(1);
        let value = timeout(
            Duration::from_secs(effective_timeout),
            self.execute_notes_action(&action_name, &params),
        )
        .await
        .map_err(|_| {
            ExecutionError::Step(format!(
                "notes_data action `{action_name}` timed out after {effective_timeout}s"
            ))
        })??;
        let rendered = serde_json::to_string_pretty(&value).map_err(|error| {
            ExecutionError::Step(format!("notes_data result serialization failed: {error}"))
        })?;
        if rendered.len() as u64 > APP_BOUND_NOTES_DATA_RESULT_CEILING {
            return Err(ExecutionError::Step(format!(
                "notes_data result exceeded the {APP_BOUND_NOTES_DATA_RESULT_CEILING} byte ceiling"
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

fn prove_app_notes_args(parameters: &HashMap<String, Value>) -> bool {
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
                        NOTES_DATA_TOOL_NAME,
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
            "query" if operation == "search_notes" => {
                if !value.as_str().is_some_and(admissible_query) {
                    return false;
                }
            },
            "limit" if operation == "search_notes" => {
                if value
                    .as_u64()
                    .is_none_or(|limit| !(1..=MAX_SEARCH_LIMIT).contains(&limit))
                {
                    return false;
                }
            },
            "provider" if operation == "search_notes" => {
                if !value
                    .as_str()
                    .is_some_and(|provider| PROVIDERS.contains(&provider))
                {
                    return false;
                }
            },
            "source_ref" if operation == "read_note" => {
                if !value.as_str().is_some_and(admissible_source_ref) {
                    return false;
                }
            },
            hidden if hidden.starts_with("__") => {},
            _ => return false,
        }
    }

    match operation {
        // Neither action has a defaultable target: "search for nothing" and
        // "read whichever note" are not requests.
        "search_notes" => parameters.contains_key("query"),
        "read_note" => parameters.contains_key("source_ref"),
        _ => false,
    }
}

fn bounded_nonblank_string(value: &Value, max_bytes: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= max_bytes)
}

fn admissible_query(query: &str) -> bool {
    let trimmed = query.trim();
    trimmed.chars().count() >= MIN_QUERY_CHARS
        && query.len() <= MAX_QUERY_BYTES
        && !query.chars().any(char::is_control)
}

/// `notes:<provider>:<relative markdown path>`, exactly what a search hit
/// emits. The relative path must pass the store's own readable-note check
/// (relative, no parent escape, markdown extension); the store re-confines it
/// to the provider root on read, so this is the first of two fences.
fn admissible_source_ref(source_ref: &str) -> bool {
    if source_ref.len() > MAX_SOURCE_REF_BYTES || source_ref.chars().any(char::is_control) {
        return false;
    }
    let Some(rest) = source_ref.strip_prefix("notes:") else {
        return false;
    };
    let Some((provider, relative_path)) = rest.split_once(':') else {
        return false;
    };
    PROVIDERS.contains(&provider) && is_readable_note_path(relative_path)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

impl NotesDataProvider {
    async fn execute_notes_action(
        &self,
        action: &str,
        params: &HashMap<String, Value>,
    ) -> Result<Value, ExecutionError> {
        match action {
            "search_notes" => self.search_notes(params).await,
            "read_note" => self.read_note(params).await,
            other => Err(ExecutionError::Step(format!(
                "notes_data: unknown action `{other}`"
            ))),
        }
    }

    async fn search_notes(&self, params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let query = string_param(params, "query").ok_or_else(|| {
            ExecutionError::Step("notes_data: `query` must be a non-empty string".to_owned())
        })?;
        let request = NoteSearchRequest {
            query,
            limit: params
                .get("limit")
                .and_then(Value::as_u64)
                .map(|limit| limit.clamp(1, MAX_SEARCH_LIMIT) as usize),
            provider: string_param(params, "provider"),
        };
        let results = self
            .store()
            .search_notes(&scope.principal, &scope.workspace, request)
            .await
            .map_err(|error| ExecutionError::Step(format!("notes_data search failed: {error}")))?;
        let hits = results
            .hits
            .iter()
            .filter(|hit| admissible_source_ref(&hit.source_ref))
            .map(|hit| {
                json!({
                    "source_ref": hit.source_ref,
                    "provider": hit.provider,
                    "relative_path": hit.relative_path,
                    "title": bounded_text(&hit.title, MAX_TITLE_BYTES),
                    "modified_at_ms": hit.modified_at_ms,
                    "matched_in_title": hit.matched_in_title,
                    "match_count": hit.match_count,
                    "matches": hit
                        .matches
                        .iter()
                        .take(MAX_MATCHES_PER_HIT)
                        .map(|matched| json!({
                            "line": matched.line,
                            "text": bounded_text(&matched.text, MAX_MATCH_TEXT_BYTES),
                        }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "scope": scope.as_json(),
            "hits": hits,
            "scanned_notes": results.scanned_notes,
            // Reported, not hidden: "nothing found" means something different
            // when the scan stopped early or the cap trimmed results.
            "scan_truncated": results.scan_truncated,
            "more_available": results.more_available,
        }))
    }

    async fn read_note(&self, params: &HashMap<String, Value>) -> Result<Value, ExecutionError> {
        let scope = scope_from_params(params)?;
        let source_ref = string_param(params, "source_ref").ok_or_else(|| {
            ExecutionError::Step("notes_data: `source_ref` must be a non-empty string".to_owned())
        })?;
        let note = self
            .store()
            .read_observation_note(&scope.principal, &scope.workspace, &source_ref)
            .await
            .map_err(|error| ExecutionError::Step(format!("notes_data read failed: {error}")))?;
        let present = note.is_some();
        Ok(json!({
            "scope": scope.as_json(),
            // Absent is an answer, not an error: a note an app holds a ref to
            // can have been moved or deleted since the search.
            "note": note.map(|note| json!({
                "source_ref": note.source_ref,
                "provider": note.provider,
                "relative_path": note.relative_path,
                "title": bounded_text(&note.title, MAX_TITLE_BYTES),
                "markdown": note.markdown,
                "modified_at_ms": note.modified_at_ms,
                "content_hash": note.content_hash,
            })),
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
            "notes_data runtime scope contains an unsafe principal or workspace component"
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
                    "notes_data: `{public_key}` is an optional scope assertion and must be a string"
                )));
            };
            if !value.is_empty() && value != trusted_value {
                return Err(ExecutionError::Step(format!(
                    "notes_data: model-supplied `{public_key}` does not match the runtime-authorized scope"
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
            "notes_data requires runtime-owned scope `{key}`; unscoped execution is denied"
        ))
    })?;
    if value.is_empty() || value.trim() != value {
        return Err(ExecutionError::Step(format!(
            "notes_data runtime-owned scope `{key}` must be a nonblank canonical component"
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

/// Compile-time reminder that the notes binder never grows a write verb.
const _: () = {
    let mut index = 0;
    while index < ACTIONS.len() {
        let action = ACTIONS[index].as_bytes();
        assert!(
            !starts_with(action, b"create")
                && !starts_with(action, b"append")
                && !starts_with(action, b"write")
                && !starts_with(action, b"save")
                && !starts_with(action, b"publish")
                && !starts_with(action, b"delete")
                && !starts_with(action, b"open"),
            "notes_data is a read binder; note changes belong to the notes owner"
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
    fn the_action_surface_is_closed_and_both_reads_need_a_target() {
        assert!(
            !prove_app_notes_args(&params("search_notes")),
            "search needs a query"
        );
        assert!(
            !prove_app_notes_args(&params("read_note")),
            "read needs a source_ref"
        );
        let mut search = params("search_notes");
        search.insert("query".to_owned(), json!("launch plan"));
        assert!(prove_app_notes_args(&search));
        let mut read = params("read_note");
        read.insert(
            "source_ref".to_owned(),
            json!("notes:local_markdown:Inbox/plan.md"),
        );
        assert!(prove_app_notes_args(&read));
        for refused in [
            "create_note",
            "append_note",
            "open_note",
            "publish_task_to_note",
            "catalog",
        ] {
            assert!(
                !prove_app_notes_args(&params(refused)),
                "`{refused}` is not a notes_data read"
            );
        }
    }

    #[test]
    fn source_refs_are_confined_to_known_providers_and_relative_markdown() {
        for accepted in [
            "notes:local_markdown:Inbox/plan.md",
            "notes:silverbullet:Projects/Q4 plan.markdown",
        ] {
            assert!(admissible_source_ref(accepted), "{accepted}");
        }
        for refused in [
            "notes:local_markdown:../../.ssh/id_rsa.md",
            "notes:local_markdown:/etc/passwd.md",
            "notes:local_markdown:Inbox/secrets.txt",
            "notes:dropbox:Inbox/plan.md",
            "local_markdown:Inbox/plan.md",
            "notes:local_markdown",
            "notes:local_markdown:Inbox/pl\u{7}an.md",
        ] {
            assert!(!admissible_source_ref(refused), "{refused:?}");
        }
        assert!(!admissible_source_ref(&format!(
            "notes:local_markdown:{}.md",
            "a".repeat(MAX_SOURCE_REF_BYTES)
        )));
    }

    #[test]
    fn search_arguments_are_bounded_and_scoped_to_search() {
        let mut search = params("search_notes");
        for query in [
            "",
            "a",
            "   ",
            &"q".repeat(MAX_QUERY_BYTES + 1),
            "bad\u{0}query",
        ] {
            search.insert("query".to_owned(), json!(query));
            assert!(!prove_app_notes_args(&search), "query {query:?}");
        }
        search.insert("query".to_owned(), json!("ok"));
        for limit in [json!(0), json!(MAX_SEARCH_LIMIT + 1)] {
            search.insert("limit".to_owned(), limit.clone());
            assert!(!prove_app_notes_args(&search), "limit {limit}");
        }
        search.insert("limit".to_owned(), json!(10));
        search.insert("provider".to_owned(), json!("icloud"));
        assert!(!prove_app_notes_args(&search));
        search.insert("provider".to_owned(), json!("silverbullet"));
        assert!(prove_app_notes_args(&search));
        search.insert("source_ref".to_owned(), json!("notes:local_markdown:a.md"));
        assert!(
            !prove_app_notes_args(&search),
            "source_ref belongs to read_note"
        );
        search.remove("source_ref");
        search.insert("path".to_owned(), json!("/Users/owner/notes/a.md"));
        assert!(
            !prove_app_notes_args(&search),
            "host paths are never an argument"
        );
    }

    #[test]
    fn runtime_scope_is_required_and_a_public_assertion_cannot_switch_it() {
        let mut unscoped = params("search_notes");
        unscoped.remove("__workspace");
        assert!(authorize_runtime_scope(unscoped).is_err());
        let mut mismatched = params("search_notes");
        mismatched.insert("workspace".to_owned(), json!("other"));
        assert!(authorize_runtime_scope(mismatched).is_err());
    }
}
