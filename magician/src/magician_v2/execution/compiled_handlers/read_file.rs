//! `read_file` — read a file from the local filesystem.
//!
//! Thin wrapper that builds a `FileAction::Read` and dispatches it
//! through `execute_file_action` with the per-scope `FileSandboxConfig`
//! on `AgentResources`. Same dispatch path as the multi-action `files`
//! pack (`action: "read"`), exposed as a named single-purpose tool
//! for ergonomics.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::actions::{ActionResult, FileAction};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::native_executors::execute_file_action;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(file_path) = args.get("file_path").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "read_file requires `file_path` (absolute path string).",
        }));
    };
    let encoding = args
        .get("encoding")
        .and_then(Value::as_str)
        .map(str::to_string);
    let offset = byte_argument(&args, "offset");
    let limit = byte_argument(&args, "limit");

    let action = FileAction::Read {
        path: PathBuf::from(file_path),
        encoding,
    };

    // P0.4 — widen the boot sandbox with any roots the operator approved this
    // session via the sandbox-override HITL, so a previously-denied path is
    // readable on the retry (mirrors the native `files` pack merge at the
    // `ExecutableAction::File` gate). `None` outside a scoped dispatch → the
    // boot `file_sandbox` is used unchanged.
    let mut effective_sandbox = resources.file_sandbox.clone();
    if let Some(roots) =
        crate::magician_v2::execution::compiled_dispatch::current_session_file_sandbox_roots()
    {
        if let Ok(guard) = roots.lock() {
            effective_sandbox
                .extra_writable_roots
                .extend(guard.iter().cloned());
        }
    }

    match execute_file_action(&action, &effective_sandbox).await {
        Ok(ActionResult::Text { content }) if offset.is_none() && limit.is_none() => Ok(json!({
            "status": "ok",
            "file_path": file_path,
            "content": content,
            "byte_count": content.len(),
        })),
        Ok(ActionResult::Text { content }) => {
            let slice = slice_content(&content, offset.unwrap_or(0), limit);
            let mut value = json!({
                "status": "ok",
                "file_path": file_path,
                "content": &content[slice.start..slice.end],
                "byte_count": slice.end - slice.start,
                "offset": slice.start,
                "total_bytes": content.len(),
            });
            if slice.end < content.len() {
                value["next_offset"] = json!(slice.end);
            }
            Ok(value)
        },
        Ok(other) => Ok(json!({
            "status": "ok",
            "file_path": file_path,
            "result": format!("{other:?}"),
        })),
        // Propagate a pure out-of-roots denial so the executor chokepoint can
        // raise the sandbox-override HITL (approve a folder → retry). Swallowing
        // it into a soft `{status:error}` here would hide the recoverable case
        // from the escalation path and hard-fail the read for the LLM instead.
        Err(e @ ExecutionError::PathAccessDenied { .. }) => Err(e),
        Err(e) => Ok(json!({
            "status": "error",
            "reason": format!("{e}"),
        })),
    }
}

/// A non-negative byte argument. Models sometimes send numbers as strings.
fn byte_argument(args: &Value, key: &str) -> Option<usize> {
    let value = args.get(key)?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .and_then(|bytes| usize::try_from(bytes).ok())
}

/// The byte range `[offset, offset + limit)` of `content`, moved onto UTF-8
/// boundaries: the start forward (an offset inside a character begins at the
/// next one), the end back. A non-empty remainder always yields at least one
/// character, so `next_offset` always advances.
fn slice_content(content: &str, offset: usize, limit: Option<usize>) -> std::ops::Range<usize> {
    let mut start = offset.min(content.len());
    while !content.is_char_boundary(start) {
        start += 1;
    }
    let mut end = limit
        .map_or(content.len(), |limit| start.saturating_add(limit))
        .min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    if end <= start && start < content.len() {
        end = content[start..]
            .char_indices()
            .nth(1)
            .map_or(content.len(), |(next, _)| start + next);
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::capability::CapabilityPackDefinition;
    use crate::magician_v2::tool_result_projection::{
        ConservativeTokenEstimator, DisplayResultProjection, ProjectionBudget,
        ProjectionContractRegistry, ProjectionOmissionKind, ProjectionStrategy,
        RawResultDescriptor, ResultRetentionClass, ScopedResultRef, ToolOutcome,
        ToolResultIdentity, ToolResultProjectionRequest, ToolResultProjector,
    };

    fn shipped_contract() -> crate::magician_v2::tool_result_projection::ProjectionContractSpec {
        let definition: CapabilityPackDefinition =
            serde_yaml::from_str(include_str!("../embedded_pack_defs/read_file.yaml"))
                .expect("the embedded read_file pack parses");
        definition
            .result_projection
            .expect("read_file declares a projection contract")
    }

    /// The autonomous surface, as shipped in the config seed.
    fn autonomous_budget() -> ProjectionBudget {
        ProjectionBudget {
            max_serialized_bytes: 24_576,
            max_estimated_tokens: 6_144,
            max_records: 20,
            max_depth: 8,
            max_scalar_bytes: 8_192,
            max_spoken_chars: 420,
        }
    }

    fn project(
        raw: &Value,
        contract: Option<crate::magician_v2::tool_result_projection::ProjectionContractSpec>,
    ) -> crate::magician_v2::tool_result_projection::ProjectedToolResultV1 {
        let descriptor = RawResultDescriptor {
            content_ref: ScopedResultRef {
                result_ref: "result_ref_v1_read".to_string(),
                cursor: None,
            },
            content_hash: "hash".to_string(),
            media_type: "application/json".to_string(),
            size_bytes: 20_000,
            retention_class: ResultRetentionClass::TaskExecution,
        };
        let mut registry = ProjectionContractRegistry::default();
        let contract_id = contract.as_ref().map(|c| c.contract_id.clone());
        if let Some(contract) = contract {
            registry
                .register_override(contract)
                .expect("the contract registers");
        }
        ToolResultProjector::new(registry, ConservativeTokenEstimator)
            .project(ToolResultProjectionRequest {
                identity: ToolResultIdentity {
                    tool_name: "read_file".to_string(),
                    tool_call_id: "call-1".to_string(),
                    execution_id: None,
                    task_id: None,
                    scope_digest: "scope".to_string(),
                    authority_revision: "revision-1".to_string(),
                },
                outcome: ToolOutcome::succeeded(),
                raw_result: raw,
                display: DisplayResultProjection::referenced(&descriptor),
                raw: descriptor,
                spoken_hint: None,
                contract_id: contract_id.as_ref(),
                budget: autonomous_budget(),
            })
            .expect("projection succeeds")
    }

    fn markdown(bytes: usize) -> String {
        let paragraph = "## Section\n\nThe program doc explains one goal in a complete sentence. \
                         It names the owner, the deadline and the next step.\n\n";
        paragraph.repeat(bytes / paragraph.len() + 1)[..bytes].to_string()
    }

    /// An oversized file reaches the model as its head — a prefix of the file
    /// up to the scalar budget — plus the ref to the whole result, instead of
    /// an omission with nothing to read.
    #[test]
    fn an_oversized_file_projects_to_a_head_excerpt_and_a_ref() {
        let content = markdown(16_000);
        let raw = json!({
            "status": "ok",
            "file_path": "/notes/program.md",
            "content": content,
            "byte_count": content.len(),
            "success": true,
        });
        let result = project(&raw, Some(shipped_contract()));
        assert_eq!(
            result.model.strategy,
            ProjectionStrategy::ContractStructured
        );
        let projection = &result.model.value["projection"];
        let excerpt = &projection["excerpts"][0];
        assert_eq!(excerpt["path"], "/content");
        assert_eq!(excerpt["start_byte"], 0);
        assert_eq!(excerpt["original_bytes"], content.len());
        let text = excerpt["text"].as_str().expect("excerpt text");
        assert!(!text.is_empty() && text.len() <= 8_192);
        assert!(content.starts_with(text), "the excerpt is the file's head");
        assert_eq!(excerpt["end_byte"], text.len());
        assert_eq!(projection["complete"], false);
        assert_eq!(
            projection["full_result_ref"]["result_ref"],
            "result_ref_v1_read"
        );
        let data = &result.model.value["data"];
        assert_eq!(data["file_path"], "/notes/program.md");
        assert_eq!(data["byte_count"], content.len());
        assert!(data.get("content").is_none());

        // Without the contract the same file is an omission with no text.
        let generic = project(&raw, None);
        assert!(generic.model.value["projection"]["excerpts"].is_null());
        assert!(serde_json::from_value::<
            crate::magician_v2::tool_result_projection::ModelProjectionMetadata,
        >(generic.model.value["projection"].clone())
        .expect("metadata")
        .omissions
        .iter()
        .any(|omission| omission.kind == ProjectionOmissionKind::OversizedScalar));
    }

    #[test]
    fn a_small_file_still_reaches_the_model_whole() {
        let content = markdown(3_000);
        let raw = json!({
            "status": "ok",
            "file_path": "/notes/short.md",
            "content": content,
            "byte_count": content.len(),
            "success": true,
        });
        let result = project(&raw, Some(shipped_contract()));
        assert_eq!(result.model.strategy, ProjectionStrategy::Complete);
        assert_eq!(result.model.value["data"]["content"], content);
    }

    #[test]
    fn slices_land_on_utf8_boundaries_and_always_advance() {
        let text = "héllo wörld";
        // Start inside 'é' moves forward; end inside 'ö' moves back.
        assert_eq!(slice_content(text, 2, Some(6)), 3..8);
        assert_eq!(&text[3..8], "llo w");
        // A limit too small for one character still yields that character.
        assert_eq!(slice_content(text, 1, Some(1)), 1..3);
        // No limit reads to the end; past the end is empty.
        assert_eq!(slice_content(text, 3, None), 3..text.len());
        assert_eq!(slice_content(text, 99, Some(5)), text.len()..text.len());
    }

    #[test]
    fn offsets_accept_numbers_and_numeric_strings() {
        let args = json!({"offset": 8150, "limit": "8000", "bad": "x"});
        assert_eq!(byte_argument(&args, "offset"), Some(8_150));
        assert_eq!(byte_argument(&args, "limit"), Some(8_000));
        assert_eq!(byte_argument(&args, "bad"), None);
        assert_eq!(byte_argument(&args, "missing"), None);
    }
}

/// `read_file` applies goal workspace access for the acting agent.
#[cfg(test)]
mod goal_workspace_tests {
    use super::*;
    use crate::magician_v2::execution::goal_workspace_access as access;
    use crate::magician_v2::execution::native_executors::ACTING_AGENT;

    #[tokio::test]
    async fn read_file_refuses_a_goal_workspace_the_agent_is_not_a_member_of() {
        let temp = tempfile::tempdir().unwrap();
        let resources = access::test_resources(temp.path());
        let root = temp.path().canonicalize().unwrap().join("Workspaces");
        access::write_fixture(&root);
        let _roots = access::install_test_roots(vec![root.clone()]);
        let read = |agent: &'static str, file: &'static str| {
            let resources = Arc::clone(&resources);
            let path = root.join(file).display().to_string();
            ACTING_AGENT.scope(Some(agent.to_string()), async move {
                handle(resources, json!({"file_path": path})).await.unwrap()
            })
        };
        let outsider = read("ambassador", "Magican/Goals/X/x.md").await;
        assert_eq!(outsider["status"], "error", "{outsider}");
        assert!(outsider["reason"]
            .as_str()
            .unwrap()
            .contains("not a member"));
        let member = read("cmo", "Magican/Goals/X/x.md").await;
        assert_eq!(member["status"], "ok", "{member}");
        let private = read("cmo", "Magican/Goals/X/secret.md").await;
        assert_eq!(private["status"], "error", "{private}");
        assert!(!private.to_string().contains("needle"));
    }
}
