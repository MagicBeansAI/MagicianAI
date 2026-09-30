//! `contribute_to_project` — publish a clean, finished artifact into a VibeDev
//! project's repo contributions area (`docs/` | `specs/` | `design/`) so a later
//! Pi coding run can see it (Pi's shadow is re-synced real→shadow at run start, so
//! the contribution shows up on the next run and contributes nothing to the proposal
//! diff). It is an explicit publish/promote gate: only the named artifact lands;
//! the agent's work-in-progress + failures stay in its own task outputs.
//!
//! Target project resolution: explicit `project_id` arg, else the project of the
//! current task (the `VibeDev project: <uuid>` line, turn-invariant) — so an agent
//! delegated as part of a project's work auto-contributes to the right project.
//!
//! Write mode is config-switchable (`coding.contribute_direct`, default true):
//! DIRECT writes straight into the real repo; otherwise the contribution is staged
//! behind diff-approval HITL (text only).

use std::collections::HashMap;
use std::path::{Component, Path};
use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};

use super::shared::{require_scope_str, scope_arg_str};
use super::staged_file_edit::{self, normalize_relative_path};
use crate::magician_v2::agents::memory_tiers::TierScope;
use crate::magician_v2::agents::project_knowledge::{
    code_knowledge_tier_def, MAX_RECALL_FACTS, PROJECT_KNOWLEDGE_AGENT,
};
use crate::magician_v2::artifact_v2::memory::V3MemoryTierRecord;
use crate::magician_v2::artifact_v2::service::{ScopeRef, V3ReadApi};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::coding_engine::resolve_coding_repo_binding;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::file_edit::snapshot::MAX_FILE_READ_BYTES;
use crate::magician_v2::execution::file_edit::transaction::ProposedEdit;
use crate::magician_v2::vibedev::projects::{parse_vibedev_project_id, read_vibedev_projects};

/// Top-level dirs a non-Pi agent may contribute into. Source code is deliberately
/// excluded — it must go through the engineering (Pi) loop + diff-approval.
const ALLOWED_DIRS: &[&str] = &["docs", "specs", "design"];

fn err(reason: impl Into<String>) -> Value {
    json!({ "status": "error", "reason": reason.into() })
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "contribute_to_project")?;
    let workspace = require_scope_str(&args, "__workspace", "contribute_to_project")?;

    // --- dest_path: required, restricted to docs/|specs/|design/, no traversal -----------------
    let Some(dest_raw) = scope_arg_str(&args, "dest_path") else {
        return Ok(err(
            "contribute_to_project requires `dest_path` (a path under docs/, specs/, or design/).",
        ));
    };
    let dest_rel = match normalize_relative_path(Path::new(&dest_raw)) {
        Ok(path) => path,
        Err(error) => return Ok(err(format!("invalid `dest_path`: {error:#}"))),
    };
    let first_ok = matches!(
        dest_rel.components().next(),
        Some(Component::Normal(part)) if ALLOWED_DIRS.iter().any(|dir| part == *dir)
    );
    if !first_ok {
        return Ok(err(
            "`dest_path` must be under docs/, specs/, or design/ (a non-Pi agent cannot contribute source code).",
        ));
    }

    // --- source bytes: `content` (inline) OR `source_path` (a file in this task's outputs) ------
    let (bytes, text): (Vec<u8>, Option<String>) = if let Some(content) =
        scope_arg_str(&args, "content")
    {
        let as_text = content.clone();
        (content.into_bytes(), Some(as_text))
    } else if let Some(source_raw) = scope_arg_str(&args, "source_path") {
        let Some(task_id) = scope_arg_str(&args, "__task_id") else {
            return Ok(err(
                "`source_path` needs a task context (__task_id) to locate your outputs; pass `content` instead.",
            ));
        };
        let source_rel = match normalize_relative_path(Path::new(&source_raw)) {
            Ok(path) => path,
            Err(error) => return Ok(err(format!("invalid `source_path`: {error:#}"))),
        };
        let outputs_root = resources
            .artifact_workspace
            .task_outputs_dir(&principal, &workspace, &task_id);
        let source_abs = outputs_root.join(&source_rel);
        // Containment: refuse to follow a symlink that escapes this task's outputs (the lexical
        // `normalize_relative_path` above already blocks `..`/absolute components).
        if let (Ok(file_canon), Ok(root_canon)) = (
            std::fs::canonicalize(&source_abs),
            std::fs::canonicalize(&outputs_root),
        ) {
            if !file_canon.starts_with(&root_canon) {
                return Ok(err("`source_path` resolves outside this task's outputs."));
            }
        }
        if let Ok(meta) = std::fs::metadata(&source_abs) {
            if meta.len() > MAX_FILE_READ_BYTES as u64 {
                return Ok(err(format!(
                    "`source_path` is too large to contribute ({} bytes; max {MAX_FILE_READ_BYTES}).",
                    meta.len(),
                )));
            }
        }
        match std::fs::read(&source_abs) {
            Ok(read_bytes) => {
                let as_text = String::from_utf8(read_bytes.clone()).ok();
                (read_bytes, as_text)
            },
            Err(error) => {
                return Ok(err(format!(
                    "could not read `source_path` `{source_raw}` from this task's outputs: {error}"
                )))
            },
        }
    } else {
        return Ok(err(
            "contribute_to_project requires `content` (inline) or `source_path` (a file in this task's outputs).",
        ));
    };

    // --- resolve the target project: explicit arg, else the current task's project -------------
    let project_id = match scope_arg_str(&args, "project_id") {
        Some(id) => id,
        None => {
            let Some(task_id) = scope_arg_str(&args, "__task_id") else {
                return Ok(err(
                    "no `project_id` and no task context to derive it from; pass an explicit `project_id`.",
                ));
            };
            let Some(service) = resources.artifact_v2_service.as_ref() else {
                return Ok(err(
                    "artifact service unavailable to derive the project; pass an explicit `project_id`.",
                ));
            };
            let scope =
                ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
            match service.get_task(&scope, &task_id).await {
                Ok(task) => match parse_vibedev_project_id(&task.manifest.description) {
                    Some(id) => id,
                    None => {
                        return Ok(err(
                            "this task is not bound to a VibeDev project (no `VibeDev project:` line); pass an explicit `project_id`.",
                        ))
                    },
                },
                Err(error) => {
                    return Ok(err(format!("could not load the task to derive its project: {error}")))
                },
            }
        },
    };

    // --- locate the project's repo (default ".") ----------------------------------------------
    let repo_path = read_project_repo_path(&resources, &principal, &workspace, &project_id)
        .unwrap_or_else(|| ".".to_string());
    let workspace_root = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);
    let _ = std::fs::create_dir_all(&workspace_root);

    let summary = scope_arg_str(&args, "summary");

    if resources.coding_contribute_direct() {
        // Mode 1 — DIRECT. Resolve the project's real repo the SAME way the coding loop binds it
        // (handles an in-workspace repo OR an external absolute repo), then write into it. Pi sees
        // the file on its NEXT run (the shadow is re-synced real→shadow at run start) and, being
        // byte-identical in real+shadow, it contributes nothing to Pi's proposal diff.
        let real_repo = match resolve_coding_repo_binding(&workspace_root, Some(repo_path.as_str()))
        {
            Ok(binding) => binding.real_path,
            Err(error) => {
                return Ok(err(format!(
                    "could not resolve the repo for project `{project_id}`: {error}"
                )))
            },
        };
        let abs_dest = real_repo.join(&dest_rel);
        if let Err(error) = assert_within_repo(&real_repo, &abs_dest) {
            return Ok(err(error));
        }
        // Reject a symlinked top-level contribution dir (e.g. `docs/` → `src/`): it would let a
        // contribution escape the docs/specs/design restriction (into source code) while staying
        // inside the repo, which `assert_within_repo` (repo-containment only) wouldn't catch.
        if let Some(Component::Normal(top)) = dest_rel.components().next() {
            let top_dir = real_repo.join(top);
            if std::fs::symlink_metadata(&top_dir)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Ok(err(
                    "the contribution's top-level directory is a symlink; refusing to write through it.",
                ));
            }
        }
        if let Some(parent) = abs_dest.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                return Ok(err(format!(
                    "could not create the contribution directory: {error}"
                )));
            }
        }
        if let Err(error) = std::fs::write(&abs_dest, &bytes) {
            return Ok(err(format!("could not write the contribution: {error}")));
        }
        // Phase B — best-effort project-knowledge recall fact so the coding loop can find this
        // contribution via `magician_code_knowledge` (never fails the contribution). Only on a
        // landed DIRECT write — a staged contribution is still pending approval.
        write_recall_fact(
            &resources,
            &principal,
            &workspace,
            &project_id,
            &dest_rel.to_string_lossy(),
            summary.as_deref(),
        )
        .await;
        Ok(json!({
            "status": "ok",
            "project_id": project_id,
            "dest_path": dest_rel.to_string_lossy(),
            "bytes": bytes.len(),
            "mode": "direct",
            "summary": summary,
        }))
    } else {
        // Mode 2 — STAGED behind diff-approval HITL (`coding.contribute_direct = false`). Text only,
        // and requires an in-workspace project repo (the transaction/diff machinery jails to the
        // scoped workspace) — an external repo must use direct mode.
        let Some(content) = text else {
            return Ok(err(
                "binary contributions require direct mode (set `coding.contribute_direct = true`).",
            ));
        };
        let contribution_rel = if repo_path == "." || repo_path.trim().is_empty() {
            dest_rel.clone()
        } else {
            Path::new(&repo_path).join(&dest_rel)
        };
        let scoped = match staged_file_edit::resolve_path(
            &resources,
            &args,
            &contribution_rel.to_string_lossy(),
        ) {
            Ok(scoped) => scoped,
            Err(_) => {
                return Ok(err(
                    "staged contribution mode (`coding.contribute_direct = false`) requires an in-workspace project repo; this project's repo is external — use direct mode.",
                ))
            },
        };
        // Update an existing artifact in place (Modify) rather than failing the Create-apply.
        let edit = if scoped.absolute_path.exists() {
            ProposedEdit::Modify {
                path: scoped.relative_path.clone(),
                new_content: content,
            }
        } else {
            ProposedEdit::Create {
                path: scoped.relative_path.clone(),
                content,
            }
        };
        match staged_file_edit::stage_one_edit(
            &resources,
            &scoped,
            format!(
                "Contribute {} to project {}",
                scoped.relative_path.display(),
                project_id
            ),
            edit,
        ) {
            Ok(mut value) => {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("project_id".to_string(), Value::String(project_id));
                    obj.insert("mode".to_string(), Value::String("staged".to_string()));
                }
                Ok(value)
            },
            Err(error) => Ok(err(format!(
                "could not stage the contribution for approval: {error:#}"
            ))),
        }
    }
}

/// Read a project's `repo_path` from the scoped project store. `None` when the
/// project/record/`repo_path` is absent → the caller defaults to `"."`.
fn read_project_repo_path(
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    project_id: &str,
) -> Option<String> {
    let scope_root = resources
        .artifact_workspace
        .scope_root(principal, workspace);
    read_vibedev_projects(&scope_root)
        .into_iter()
        .find(|project| project.project_id == *project_id)
        .and_then(|project| project.repo_path)
        .filter(|path| !path.trim().is_empty())
}

/// Defense-in-depth containment for a DIRECT write: the deepest EXISTING ancestor of `abs_dest`
/// must canonicalize to within `real_repo` (already canonical from `resolve_coding_repo_binding`),
/// so a symlinked contribution dir (e.g. `docs/` → `/etc`) can't escape the project repo even
/// though the lexical path looks contained.
fn assert_within_repo(real_repo: &Path, abs_dest: &Path) -> Result<(), String> {
    let mut probe = abs_dest;
    let existing = loop {
        if probe.exists() {
            break probe;
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return Err("invalid contribution path".to_string()),
        }
    };
    let canonical = std::fs::canonicalize(existing)
        .map_err(|error| format!("could not resolve the contribution path: {error}"))?;
    if canonical.starts_with(real_repo) {
        Ok(())
    } else {
        Err("the contribution path escapes the project repo (symlinked directory?).".to_string())
    }
}

// ─── Phase B: the shared project-knowledge recall lane ──────────────────────────────────────────
// The lane const + `code_knowledge_tier_def()` live in `agents::project_knowledge` (a low module the
// WRITE here, the citizen READ, and the memory-index REBUILD all reference — one source of truth).

/// Append a recall fact (stamped `project_id`) into the shared project-knowledge `code_knowledge`
/// lane so the coding loop recalls this contribution via `magician_code_knowledge`. Best-effort:
/// logs + returns on any error, never fails the contribution. Load-modify-save (`save_native_tier`
/// rewrites the whole tier record), deduped by `key` and capped.
async fn write_recall_fact(
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    project_id: &str,
    dest_path: &str,
    summary: Option<&str>,
) {
    let memory_service = match resources
        .memory_resolver
        .resolve_for_scope(principal, workspace)
    {
        Ok(service) => service,
        Err(error) => {
            tracing::warn!(target: "contribute_to_project", %error, "recall-fact: memory resolver failed; skipping");
            return;
        },
    };
    let tier = code_knowledge_tier_def();
    let mut record = match memory_service
        .load_native_tier(PROJECT_KNOWLEDGE_AGENT, &tier, None)
        .await
    {
        Ok(Some(record)) => record,
        Ok(None) => V3MemoryTierRecord {
            schema_version: "v3".to_string(),
            record_type: "memory_tier".to_string(),
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            agent_id: Some(PROJECT_KNOWLEDGE_AGENT.to_string()),
            tier_name: tier.name.clone(),
            tier_scope: TierScope::Agent,
            goal_id: None,
            last_updated: Utc::now(),
            fields: HashMap::new(),
        },
        Err(error) => {
            tracing::warn!(target: "contribute_to_project", %error, "recall-fact: tier load failed; skipping");
            return;
        },
    };

    let mut facts: Vec<Value> = record
        .fields
        .get("facts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let key = format!("contrib:{project_id}:{dest_path}");
    let text = match summary {
        Some(value) if !value.trim().is_empty() => format!("Contributed `{dest_path}`: {value}"),
        _ => format!("Contributed `{dest_path}`"),
    };
    // Dedup by `key`, append the fresh fact (stamped project_id — the read's per-fact project
    // filter keeps only this project's), then cap (drop oldest by updated_at).
    facts.retain(|item| item.get("key").and_then(Value::as_str) != Some(key.as_str()));
    facts.push(json!({
        "key": key,
        "text": text,
        "project_id": project_id,
        "updated_at": Utc::now().to_rfc3339(),
    }));
    if facts.len() > MAX_RECALL_FACTS {
        facts.sort_by(|a, b| {
            let left = a.get("updated_at").and_then(Value::as_str).unwrap_or("");
            let right = b.get("updated_at").and_then(Value::as_str).unwrap_or("");
            left.cmp(right)
        });
        let overflow = facts.len() - MAX_RECALL_FACTS;
        facts.drain(0..overflow);
    }

    record
        .fields
        .insert("facts".to_string(), Value::Array(facts));
    record.tier_name = tier.name.clone();
    record.tier_scope = TierScope::Agent;
    record.last_updated = Utc::now();
    record.principal = Some(principal.to_string());
    record.workspace = Some(workspace.to_string());
    record.agent_id = Some(PROJECT_KNOWLEDGE_AGENT.to_string());

    if let Err(error) = memory_service
        .save_native_tier(PROJECT_KNOWLEDGE_AGENT, &tier, None, &record)
        .await
    {
        tracing::warn!(target: "contribute_to_project", %error, "recall-fact: tier save failed; skipping");
    }
}
