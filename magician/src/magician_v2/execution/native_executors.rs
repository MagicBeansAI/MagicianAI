//! # Native Executors for Non-Browser Actions
//!
//! This module provides native Rust executors for File, HTTP, and Bash actions.
//! These execute directly without MCP overhead, providing 10-100x faster performance.
//!
//! ## Performance Comparison
//! | Operation | RPC Tool Call | Native Rust |
//! |-----------|---------------|-------------|
//! | File write | ~50-100ms | ~1-5ms |
//! | HTTP GET | ~100-200ms | ~10-50ms |
//! | Bash command | ~100-150ms | ~10-30ms |

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use runtime_core::{FileSandboxConfig, FileSandboxMode, ShellSandboxConfig, ShellSandboxMode};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::actions::{ActionResult, BashAction, FileAction, HttpAction, HttpMethod};
use super::ExecutionError;
use crate::magician_v2::analytics::llm_tool_lineage::LlmToolLineageIdentity;
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

// ============================================================================
// File Executor
// ============================================================================

/// Execute a file action using tokio::fs
pub async fn execute_file_action(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<ActionResult, ExecutionError> {
    debug!("Executing file action: {}", action.description());
    validate_file_action(action, sandbox)?;
    validate_file_action_repo_fence(action)?;
    validate_file_action_runtime_store_fence(action, sandbox)?;

    match action {
        FileAction::Read { path, encoding: _ } => {
            // Read file contents
            let content = tokio::fs::read_to_string(path).await.map_err(|e| {
                ExecutionError::Step(format!("Failed to read file '{}': {}", path.display(), e))
            })?;
            info!("Read {} bytes from {}", content.len(), path.display());
            Ok(ActionResult::text(content))
        },

        FileAction::Write {
            path,
            content,
            create_dirs,
        } => {
            // Create parent directories if needed
            if *create_dirs {
                if let Some(parent) = path.parent() {
                    if !parent.exists() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            ExecutionError::Step(format!(
                                "Failed to create parent directories for '{}': {}",
                                path.display(),
                                e
                            ))
                        })?;
                    }
                }
            }

            // Write content
            tokio::fs::write(path, content).await.map_err(|e| {
                ExecutionError::Step(format!("Failed to write file '{}': {}", path.display(), e))
            })?;
            info!("Wrote {} bytes to {}", content.len(), path.display());
            Ok(ActionResult::success())
        },

        FileAction::Append { path, content } => {
            // Open file in append mode
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|e| {
                    ExecutionError::Step(format!(
                        "Failed to open file '{}' for append: {}",
                        path.display(),
                        e
                    ))
                })?;

            file.write_all(content.as_bytes()).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to append to file '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            info!("Appended {} bytes to {}", content.len(), path.display());
            Ok(ActionResult::success())
        },

        FileAction::Delete { path, recursive } => {
            if *recursive {
                // Delete directory recursively
                tokio::fs::remove_dir_all(path).await.map_err(|e| {
                    ExecutionError::Step(format!(
                        "Failed to delete directory '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                info!("Deleted directory recursively: {}", path.display());
            } else {
                // Delete single file
                tokio::fs::remove_file(path).await.map_err(|e| {
                    ExecutionError::Step(format!(
                        "Failed to delete file '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                info!("Deleted file: {}", path.display());
            }
            Ok(ActionResult::success())
        },

        FileAction::Copy {
            source,
            destination,
        } => {
            // Create parent directories for destination if needed
            if let Some(parent) = destination.parent() {
                if !parent.exists() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        ExecutionError::Step(format!(
                            "Failed to create parent directories for '{}': {}",
                            destination.display(),
                            e
                        ))
                    })?;
                }
            }

            // Copy file
            tokio::fs::copy(source, destination).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to copy '{}' to '{}': {}",
                    source.display(),
                    destination.display(),
                    e
                ))
            })?;
            info!("Copied {} to {}", source.display(), destination.display());
            Ok(ActionResult::success())
        },

        FileAction::Move {
            source,
            destination,
        } => {
            // Create parent directories for destination if needed
            if let Some(parent) = destination.parent() {
                if !parent.exists() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        ExecutionError::Step(format!(
                            "Failed to create parent directories for '{}': {}",
                            destination.display(),
                            e
                        ))
                    })?;
                }
            }

            // Move/rename file
            tokio::fs::rename(source, destination).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to move '{}' to '{}': {}",
                    source.display(),
                    destination.display(),
                    e
                ))
            })?;
            info!("Moved {} to {}", source.display(), destination.display());
            Ok(ActionResult::success())
        },

        FileAction::Exists { path } => {
            let exists = path.exists();
            debug!("Path '{}' exists: {}", path.display(), exists);
            Ok(ActionResult::bool(exists))
        },

        FileAction::List { path, pattern } => {
            let mut entries = Vec::new();
            let mut read_dir = tokio::fs::read_dir(path).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to read directory '{}': {}",
                    path.display(),
                    e
                ))
            })?;

            while let Some(entry) = read_dir.next_entry().await.map_err(|e| {
                ExecutionError::Step(format!("Failed to read directory entry: {}", e))
            })? {
                let name = entry.file_name().to_string_lossy().to_string();

                // Apply pattern filter if specified
                if let Some(pat) = pattern {
                    if !matches_glob_pattern(&name, pat) {
                        continue;
                    }
                }

                entries.push(name);
            }

            info!("Listed {} entries in {}", entries.len(), path.display());
            Ok(ActionResult::list(entries))
        },

        FileAction::CreateDir { path } => {
            tokio::fs::create_dir_all(path).await.map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to create directory '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            info!("Created directory: {}", path.display());
            Ok(ActionResult::success())
        },
    }
}

fn validate_file_action(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    if matches!(sandbox.mode, FileSandboxMode::Unrestricted) {
        return Ok(());
    }

    let allowed_roots = resolve_allowed_file_roots(sandbox)?;
    if allowed_roots.is_empty() {
        return Err(ExecutionError::Step(
            "File sandbox has no allowed roots configured".to_string(),
        ));
    }

    let block_mutation = || {
        Err(ExecutionError::Step(
            "File action is blocked in read-only file sandbox mode".to_string(),
        ))
    };

    match action {
        FileAction::Read { path, .. }
        | FileAction::Exists { path }
        | FileAction::List { path, .. } => ensure_path_allowed(path, &allowed_roots),
        FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::CreateDir { path } => {
            if matches!(sandbox.mode, FileSandboxMode::ReadOnly) {
                return block_mutation();
            }
            ensure_path_allowed(path, &allowed_roots)
        },
        FileAction::Delete { path, .. } => {
            if matches!(sandbox.mode, FileSandboxMode::ReadOnly) {
                return block_mutation();
            }
            if !sandbox.allow_delete {
                return Err(ExecutionError::Step(
                    "Delete operations are disabled by file sandbox policy".to_string(),
                ));
            }
            ensure_path_allowed(path, &allowed_roots)
        },
        FileAction::Copy {
            source,
            destination,
        } => {
            if matches!(sandbox.mode, FileSandboxMode::ReadOnly) {
                return block_mutation();
            }
            ensure_path_allowed(source, &allowed_roots)?;
            ensure_path_allowed(destination, &allowed_roots)
        },
        FileAction::Move {
            source,
            destination,
        } => {
            if matches!(sandbox.mode, FileSandboxMode::ReadOnly) {
                return block_mutation();
            }
            if !sandbox.allow_delete {
                return Err(ExecutionError::Step(
                    "Move operations are disabled when deletes are disallowed by file sandbox policy"
                        .to_string(),
                ));
            }
            ensure_path_allowed(source, &allowed_roots)?;
            ensure_path_allowed(destination, &allowed_roots)
        },
    }
}

/// Apply the native file executor's complete read policy to a path without
/// decoding it as text. Binary consumers (for example, a vision provider)
/// must call this before reading bytes so they cannot bypass allowed roots or
/// future read-side isolation fences merely because `FileAction::Read` returns
/// UTF-8 text.
pub fn validate_file_read_path(
    path: &Path,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    let action = FileAction::Read {
        path: path.to_path_buf(),
        encoding: None,
    };
    validate_file_action(&action, sandbox)?;
    validate_file_action_repo_fence(&action)?;
    validate_file_action_runtime_store_fence(&action, sandbox)
}

/// Isolation deny-fence for native file actions: a MUTATING file action must not
/// resolve into the live magician repo source tree. Mirrors the shell working_dir
/// fence (see `execute_bash_action_*`) so file writes/moves/deletes/dir-creates
/// cannot touch the user's real repo even when the file sandbox policy would
/// otherwise allow the path. Only mutating variants are fenced; pure reads
/// (Read/Exists/List) and the SOURCE of a Copy (read-only) are left untouched.
/// Targets are resolved via `resolve_path_for_policy` rather than `canonicalize`
/// because a freshly created file's target does not exist yet (canonicalize would
/// fail and skip the fence).
///
/// CODING-SCOPED: armed only when `coding_context_active()` — i.e. for a coding
/// agent's own file tools (and the coding engine's spawns), never for general
/// agents/skills. File actions are in-process `tokio::fs`, NOT subprocesses, so
/// the OS sandbox cannot cover them; this declarative fence is their isolation.
/// Scoping to coding context keeps the DEFAULT path byte-equivalent: the shipped
/// file sandbox permits `.` (the repo root) as writable, and legitimate general
/// flows write to in-repo data dirs outside the storage base (e.g. the prompt
/// store under `data/`) — an always-on fence would wrongly reject those. Fails
/// open when not in a coding context, unarmed, or a path cannot be resolved.
fn validate_file_action_repo_fence(action: &FileAction) -> Result<(), ExecutionError> {
    if !crate::magician_v2::execution::coding_engine::coding_context_active() {
        return Ok(());
    }
    let Some((repo_source_root, sandbox_base)) =
        crate::magician_v2::execution::coding_engine::live_repo_source_fence()
    else {
        return Ok(());
    };

    let mut targets: Vec<&Path> = Vec::new();
    match action {
        FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::Delete { path, .. }
        | FileAction::CreateDir { path } => targets.push(path),
        FileAction::Copy { destination, .. } => targets.push(destination),
        FileAction::Move {
            source,
            destination,
        } => {
            targets.push(source);
            targets.push(destination);
        },
        FileAction::Read { .. } | FileAction::Exists { .. } | FileAction::List { .. } => {},
    }

    for target in targets {
        if let Ok(resolved) = resolve_path_for_policy(target) {
            crate::magician_v2::execution::coding_engine::reject_repo_source_tree(
                &resolved,
                &repo_source_root,
                &sandbox_base,
            )
            .map_err(ExecutionError::Step)?;
        }
    }

    Ok(())
}

/// Always-on isolation deny-fence: a MUTATING native file action must not target a
/// scope's runtime-INTERNAL stores — the diff-approval transaction records, the
/// code-change proposals, and the pause/approval records under
/// `<scopes>/<principal>/<workspace>/{transactions,code_change_proposals,pause_states}`.
/// Those records carry operator-approval + apply-destination data the runtime later
/// TRUSTS (e.g. a transaction's `apply_root`, which decides where an approved write
/// lands). The scopes tree is otherwise an allowed root (`augment_with_scopes_root`),
/// so without this fence an agent could overwrite a staged transaction to forge an
/// approved external destination and redirect an operator-approved write to an
/// arbitrary path. Unlike the repo fence this is armed for ALL agents (not just
/// coding runs). Reads are unaffected; only writes/moves/deletes/dir-creates are
/// fenced. The runtime writes these stores via `std::fs` directly (not through this
/// path), so legitimate runtime writes are untouched.
fn validate_file_action_runtime_store_fence(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<(), ExecutionError> {
    // Store sub-paths relative to `<scopes_root>/<principal>/<workspace>/`. Two are
    // direct children (`transactions`, `code_change_proposals`); the pause/approval
    // store is nested under `runtime/` — matched component-wise so its real depth is
    // covered (a flat 3rd-component check would silently miss it).
    const RESERVED_STORE_SUBPATHS: &[&str] = &[
        "transactions",
        "code_change_proposals",
        "runtime/pause_states",
    ];

    let mut targets: Vec<&Path> = Vec::new();
    match action {
        FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::Delete { path, .. }
        | FileAction::CreateDir { path } => targets.push(path),
        FileAction::Copy { destination, .. } => targets.push(destination),
        FileAction::Move {
            source,
            destination,
        } => {
            targets.push(source);
            targets.push(destination);
        },
        FileAction::Read { .. } | FileAction::Exists { .. } | FileAction::List { .. } => {},
    }
    if targets.is_empty() {
        return Ok(());
    }

    // The scope roots the sandbox exposes are the dirs `augment_with_scopes_root`
    // added — basename `scopes`. A reserved store sits at
    // `<scopes_root>/<principal>/<workspace>/<store>`, so the store name is the 3rd
    // path component of the target relative to the scopes root. Resolving both sides
    // the same way `ensure_path_allowed` does keeps the prefix check symlink-safe.
    let scopes_roots: Vec<PathBuf> = resolve_allowed_file_roots(sandbox)
        .unwrap_or_default()
        .into_iter()
        .filter(|root| root.file_name().and_then(|name| name.to_str()) == Some("scopes"))
        .collect();
    if scopes_roots.is_empty() {
        return Ok(());
    }

    for target in targets {
        let Ok(resolved) = resolve_path_for_policy(target) else {
            continue;
        };
        for scopes_root in &scopes_roots {
            if let Ok(rel) = resolved.strip_prefix(scopes_root) {
                // rel == <principal>/<workspace>/<store-relative-path>. Skip the
                // scope segments and match the store dir component-wise
                // (PathBuf::starts_with), so a NESTED store like `runtime/pause_states`
                // is caught, not only the depth-2 ones.
                let store_path: PathBuf = rel.components().skip(2).collect();
                if RESERVED_STORE_SUBPATHS
                    .iter()
                    .any(|reserved| store_path.starts_with(reserved))
                {
                    return Err(ExecutionError::Step(format!(
                        "File action targets a runtime-internal store `{}` — the scope's transaction / approval / pause records are not agent-writable",
                        resolved.display()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn resolve_allowed_file_roots(sandbox: &FileSandboxConfig) -> Result<Vec<PathBuf>, ExecutionError> {
    let cwd = std::env::current_dir().map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to resolve current working directory: {}",
            e
        ))
    })?;

    sandbox
        .allowed_roots
        .iter()
        .map(|root| {
            let root_path = PathBuf::from(root);
            let absolute = if root_path.is_absolute() {
                root_path
            } else {
                cwd.join(root_path)
            };
            resolve_path_for_policy(&absolute)
        })
        .collect()
}

/// Return action paths that fall outside the configured roots. This only
/// reports the recoverable root mismatch; read-only/delete policy and the
/// coding source-tree hard fence remain enforced by `execute_file_action`.
pub fn file_action_outside_allowed_roots(
    action: &FileAction,
    sandbox: &FileSandboxConfig,
) -> Result<Vec<String>, ExecutionError> {
    if matches!(sandbox.mode, FileSandboxMode::Unrestricted) {
        return Ok(Vec::new());
    }
    let allowed_roots = resolve_allowed_file_roots(sandbox)?;
    let paths: Vec<&Path> = match action {
        FileAction::Read { path, .. }
        | FileAction::Exists { path }
        | FileAction::List { path, .. }
        | FileAction::Write { path, .. }
        | FileAction::Append { path, .. }
        | FileAction::Delete { path, .. }
        | FileAction::CreateDir { path } => vec![path.as_path()],
        FileAction::Copy {
            source,
            destination,
        }
        | FileAction::Move {
            source,
            destination,
        } => {
            vec![source.as_path(), destination.as_path()]
        },
    };
    let mut outside = Vec::new();
    for path in paths {
        let resolved = resolve_path_for_policy(path)?;
        if !allowed_roots.iter().any(|root| resolved.starts_with(root)) {
            outside.push(resolved.to_string_lossy().to_string());
        }
    }
    outside.sort();
    outside.dedup();
    Ok(outside)
}

fn ensure_path_allowed(path: &Path, allowed_roots: &[PathBuf]) -> Result<(), ExecutionError> {
    let resolved = resolve_path_for_policy(path)?;
    let allowed = allowed_roots.iter().any(|root| resolved.starts_with(root));
    if allowed {
        Ok(())
    } else {
        // Pure out-of-roots mismatch: the path is otherwise a valid target,
        // it just falls outside the currently-allowed sandbox roots. Surface
        // the RECOVERABLE `PathAccessDenied` so the executor chokepoint can
        // route it into the sandbox-override HITL (approve a folder → merge
        // into `session_file_sandbox_roots` → retry) instead of terminally
        // failing the iteration. Read-only / delete-policy / repo-fence
        // violations are handled BEFORE this call (in `validate_file_action`
        // and `validate_file_action_repo_fence`) and keep their hard
        // `ExecutionError::Step` classification.
        Err(ExecutionError::PathAccessDenied {
            paths: vec![resolved.to_string_lossy().to_string()],
        })
    }
}

fn resolve_path_for_policy(path: &Path) -> Result<PathBuf, ExecutionError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| {
                ExecutionError::Step(format!(
                    "Failed to resolve current working directory for '{}': {}",
                    path.display(),
                    e
                ))
            })?
            .join(path)
    };

    if absolute.exists() {
        return absolute.canonicalize().map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to canonicalize path '{}': {}",
                path.display(),
                e
            ))
        });
    }

    // Canonicalize the nearest existing parent, then append unresolved segments.
    let mut cursor = absolute.as_path();
    let mut unresolved = Vec::new();
    while !cursor.exists() {
        let segment = cursor.file_name().ok_or_else(|| {
            ExecutionError::Step(format!(
                "Path '{}' has no existing ancestor for sandbox checks",
                path.display()
            ))
        })?;
        unresolved.push(segment.to_os_string());
        cursor = cursor.parent().ok_or_else(|| {
            ExecutionError::Step(format!(
                "Path '{}' has no existing ancestor for sandbox checks",
                path.display()
            ))
        })?;
    }

    let mut canonical = cursor.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to canonicalize parent path for '{}': {}",
            path.display(),
            e
        ))
    })?;
    for segment in unresolved.iter().rev() {
        canonical.push(segment);
    }
    Ok(canonical)
}

/// Simple glob pattern matching (supports * and ?)
fn matches_glob_pattern(name: &str, pattern: &str) -> bool {
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let name_chars: Vec<char> = name.chars().collect();
    matches_glob_recursive(&pattern_chars, &name_chars, 0, 0)
}

fn matches_glob_recursive(pattern: &[char], name: &[char], pi: usize, ni: usize) -> bool {
    if pi >= pattern.len() && ni >= name.len() {
        return true;
    }
    if pi >= pattern.len() {
        return false;
    }

    match pattern[pi] {
        '*' => {
            // Try matching zero or more characters
            for i in ni..=name.len() {
                if matches_glob_recursive(pattern, name, pi + 1, i) {
                    return true;
                }
            }
            false
        },
        '?' => {
            // Match exactly one character
            if ni < name.len() {
                matches_glob_recursive(pattern, name, pi + 1, ni + 1)
            } else {
                false
            }
        },
        c => {
            // Match literal character
            if ni < name.len() && name[ni] == c {
                matches_glob_recursive(pattern, name, pi + 1, ni + 1)
            } else {
                false
            }
        },
    }
}

// ============================================================================
// HTTP Executor
// ============================================================================

/// The `Idempotency-Key` this request should carry, if any.
///
/// Pure so the decision is testable without a network. The two ways it answers
/// `None` are each a deliberate policy, not an oversight:
///
/// 1. No `effect_id` — the dispatch is not attributable, so there is no honest
///    key to send. That means "unattributable", never "safe to repeat".
/// 2. A safe method. The key exists so a remote can tell a lost response from a
///    request that never arrived; for GET/HEAD/OPTIONS that distinction costs
///    nothing, since re-issuing is free. Sending one anyway would be a
///    per-attempt header on every read the agent performs — noise to the remote
///    and enough to defeat intermediary caching.
///
/// When it answers `Some`, the derived key REPLACES any `Idempotency-Key`
/// already on the action. That is not the obvious choice, so the reasoning: an
/// `http_post` step takes its headers from step parameters, and for an agent
/// tool call those parameters are the model's. A model-authored key is exactly
/// what this header must never be — reusing one across two different requests
/// makes a remote answer the second from the first's cached response, and the
/// second effect disappears with no error raised anywhere. Nothing at this
/// layer can tell a model-authored header from a pack author's, so the runtime
/// value wins whenever there is one, and a pack author loses nothing real: the
/// derived key is already stable across a crash-replay of one attempt and
/// distinct across a deliberate retry, which is all a correct scheme needs.
///
/// A dispatch with no `effect_id` still forwards whatever header it was given.
/// There is no runtime identity to substitute, and stripping the header would
/// break a pack that manages its own without putting anything in its place.
fn derived_idempotency_key(action: &HttpAction, effect_id: Option<&str>) -> Option<String> {
    let effect_id = effect_id?;

    let can_commit = matches!(
        action.method,
        HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch | HttpMethod::Delete
    );
    if !can_commit {
        return None;
    }

    Some(LlmToolLineageIdentity::far_side_idempotency_key(effect_id))
}

/// Execute one HTTP action.
///
/// `effect_id` identifies the dispatch attempt this request belongs to. On a
/// request that can commit a change, a DERIVED key travels as `Idempotency-Key`
/// so a remote that honours it can collapse a repeat caused by a lost response
/// — see `derived_idempotency_key` for exactly when one is sent, and why the
/// raw id never is.
/// Whether the request an action describes can be constructed at all —
/// URL, method, header names and values, body. Checked before one-time
/// material is consumed for it (P4): a request that cannot be built is a
/// proven pre-dispatch failure, so the reservation is released rather than
/// spent. A request that builds may still fail to send; that is never
/// released, because nothing proves the destination did not receive it.
pub fn validate_http_request_buildable(action: &HttpAction) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|error| format!("Failed to create HTTP client: {error}"))?;
    build_http_request(&client, action, None)
        .build()
        .map(|_| ())
        .map_err(|error| format!("HTTP request for '{}' cannot be built: {error}", action.url))
}

fn build_http_request(
    client: &reqwest::Client,
    action: &HttpAction,
    effect_id: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut request = match action.method {
        HttpMethod::Get => client.get(&action.url),
        HttpMethod::Post => client.post(&action.url),
        HttpMethod::Put => client.put(&action.url),
        HttpMethod::Patch => client.patch(&action.url),
        HttpMethod::Delete => client.delete(&action.url),
        HttpMethod::Head => client.head(&action.url),
        HttpMethod::Options => client.request(reqwest::Method::OPTIONS, &action.url),
    };

    let derived_key = derived_idempotency_key(action, effect_id);

    // Add headers
    for (key, value) in &action.headers {
        // Skipped, not overwritten later: `header()` APPENDS, so leaving this
        // in would send the request with two `Idempotency-Key` values and no
        // remote is obliged to make sense of that. Matched case-insensitively
        // because HTTP field names are.
        if derived_key.is_some() && key.eq_ignore_ascii_case("idempotency-key") {
            continue;
        }
        request = request.header(key, value);
    }

    if let Some(key) = derived_key {
        request = request.header("Idempotency-Key", key);
    }

    // Add content-type if specified
    if let Some(content_type) = &action.content_type {
        request = request.header("Content-Type", content_type);
    }

    // Add body if present
    if let Some(body) = &action.body {
        request = request.body(body.clone());
    }
    request
}

/// The origin (`scheme://host[:port]`) of a URL: the redirect check, and the
/// destination vocabulary an HTTP claim and an HTTP challenge share with the
/// browser lane's canonical origin.
pub fn http_origin(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

/// The header under which an HTTP result names the URL its response actually
/// came from, when redirects moved it. A colon-initial name no real response
/// can carry.
pub const FINAL_URL_PSEUDO_HEADER: &str = ":final-url";

pub async fn execute_http_action(
    action: &HttpAction,
    effect_id: Option<&str>,
) -> Result<ActionResult, ExecutionError> {
    debug!("Executing HTTP action: {}", action.description());

    // A request carrying a user-typed credential (P4) is delivered to its
    // bound origin and nowhere else: redirects are never followed for it,
    // whatever the action asked, and a cross-origin one below ends the
    // attempt with the credential unforwarded.
    let follow_redirects = action.follow_redirects && !action.carries_credential;

    // Build client with timeout and redirect settings
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(action.timeout_secs.unwrap_or(30)))
        .redirect(if follow_redirects {
            reqwest::redirect::Policy::default()
        } else {
            reqwest::redirect::Policy::none()
        })
        .build()
        .map_err(|e| ExecutionError::Step(format!("Failed to create HTTP client: {}", e)))?;

    let request = build_http_request(&client, action, effect_id);

    // Send request
    let response = request.send().await.map_err(|e| {
        ExecutionError::Step(format!("HTTP request failed for '{}': {}", action.url, e))
    })?;

    if action.carries_credential && response.status().is_redirection() {
        let target = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| {
                url::Url::parse(&action.url)
                    .ok()
                    .and_then(|base| base.join(location).ok())
                    .map(|joined| joined.to_string())
            });
        let same_origin = target
            .as_deref()
            .and_then(http_origin)
            .is_some_and(|origin| http_origin(&action.url).as_deref() == Some(origin.as_str()));
        if !same_origin {
            return Err(ExecutionError::Step(format!(
                "destination changed: '{}' answered {} with a redirect to another origin; the \
                 credential was delivered to the original destination only and was not forwarded. \
                 Re-observe the authentication state before trying again.",
                action.url,
                response.status().as_u16()
            )));
        }
    }

    let status = response.status().as_u16();
    // Where the response actually came from, when redirects moved the request
    // (a followed hop): a pseudo-header no real response can carry, so the
    // challenge detector binds the answering host, not the one asked.
    let final_url = response.url().to_string();

    // Collect headers
    let mut headers = HashMap::new();
    for (key, value) in response.headers() {
        if let Ok(v) = value.to_str() {
            headers.insert(key.as_str().to_string(), v.to_string());
        }
    }
    if final_url != action.url {
        headers.insert(FINAL_URL_PSEUDO_HEADER.to_string(), final_url);
    }

    // Get response body
    let content_type = headers
        .get("content-type")
        .cloned()
        .or_else(|| headers.get("Content-Type").cloned());
    let body = response.text().await.unwrap_or_default();
    let raw_len = body.len();
    let body = visible_http_body(content_type.as_deref(), body);

    info!(
        "HTTP {:?} {} -> {} (raw {} bytes, stored {} bytes)",
        action.method,
        action.url,
        status,
        raw_len,
        body.len()
    );

    Ok(ActionResult::Http {
        status,
        headers,
        body,
    })
}

const HTTP_BODY_MODEL_CHARS: usize = 12_000;

/// Model-visible HTTP body. Raw HTML dumps (Bing/DDG scrapes) were being
/// stored as 200KB+ strings; `read_result` then paged them as an empty JSON
/// container. Strip tags for HTML and cap length so the first page is text.
fn visible_http_body(content_type: Option<&str>, body: String) -> String {
    let html = content_type.is_some_and(|value| {
        let lower = value.to_ascii_lowercase();
        lower.contains("text/html") || lower.contains("application/xhtml")
    });
    let text = if html {
        html_to_visible_text(&body)
    } else {
        body
    };
    let count = text.chars().count();
    if count <= HTTP_BODY_MODEL_CHARS {
        return text;
    }
    let truncated: String = text.chars().take(HTTP_BODY_MODEL_CHARS).collect();
    format!("{truncated}\n…[truncated {count} chars]")
}

fn html_to_visible_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(HTTP_BODY_MODEL_CHARS * 2));
    let mut in_tag = false;
    let mut skipping = false;
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !in_tag && bytes[i] == b'<' {
            in_tag = true;
            if lower[i..].starts_with("<script") || lower[i..].starts_with("<style") {
                skipping = true;
            }
            i += 1;
            continue;
        }
        if in_tag && bytes[i] == b'>' {
            in_tag = false;
            if skipping
                && i >= 7
                && (lower_bytes
                    .get(i.saturating_sub(7)..i)
                    .is_some_and(|w| w == b"/script")
                    || lower_bytes
                        .get(i.saturating_sub(6)..i)
                        .is_some_and(|w| w == b"/style"))
            {
                skipping = false;
            }
            i += 1;
            continue;
        }
        if in_tag || skipping {
            i += 1;
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        if ch.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
        i += ch.len_utf8();
    }
    out.trim().to_string()
}

// ============================================================================
// Bash Executor
// ============================================================================

/// Context for streaming shell output via the event broadcaster.
///
/// When provided to `execute_bash_action`, output lines are batched and
/// broadcast as `ShellOutputChunk` events every 100ms. The function still
/// returns `ActionResult::Text` with the full output (backward compatible).
#[derive(Clone)]
pub struct ShellStreamContext {
    pub broadcaster: Arc<RuntimeTransportBroadcaster>,
    pub execution_id: String,
    pub principal: Option<String>,
    pub workspace: Option<String>,
    /// Immutable plan/runtime step identity. Iteration attribution is always
    /// derived from this field; parsing a prior rendered `step_id` would collide
    /// with perfectly legal identifiers that already contain `-iter-`.
    pub base_step_id: String,
    pub step_id: String,
    pub step_index: usize,
    /// The run's delivered-values set (P4): every streamed chunk and the
    /// returned output are scrubbed with it before they leave this path, so
    /// a process that echoes a secret it was fed never reaches the event bus
    /// or the model with it. `None` in tests that stream nothing secret.
    pub scrub: Option<Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
}

impl ShellStreamContext {
    /// The values to scrub, computed once per flush: the delivered set can
    /// grow while a process runs, and every flush reads the current set.
    fn scrub_replacements(&self) -> Vec<String> {
        self.scrub
            .as_ref()
            .and_then(|delivered| delivered.lock().ok())
            .map(|delivered| crate::magician_v2::secrets::known_value_replacements(&delivered))
            .unwrap_or_default()
    }

    fn scrub_text(&self, text: &str) -> String {
        let replacements = self.scrub_replacements();
        if replacements.is_empty() {
            return text.to_string();
        }
        replacements.iter().fold(text.to_string(), |acc, value| {
            acc.replace(value, "[REDACTED]")
        })
    }
}

/// Streaming caps to prevent runaway output from overwhelming the event bus.
const STREAM_BATCH_INTERVAL_MS: u64 = 100;
const STREAM_MAX_CHUNK_BYTES: usize = 50 * 1024; // 50KB per chunk
const STREAM_MAX_TOTAL_BYTES: usize = 1024 * 1024; // 1MB total
const STREAM_MAX_TOTAL_LINES: usize = 10_000;

/// Execute a bash action using tokio::process
pub async fn execute_bash_action(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
    stream_ctx: Option<ShellStreamContext>,
    // The run's delivered-values set (P4), for the path that has no streaming
    // context. A shell's stdin is a credential sink, so what the process
    // echoes back must be scrubbed on BOTH paths — the blocking one wrote the
    // raw child output to `magician.log` and into the returned error, and it is
    // the path taken whenever the run has no broadcaster, no runtime execution
    // id or no step id.
    scrub: Option<Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
) -> Result<ActionResult, ExecutionError> {
    debug!("Executing bash action: {}", action.description());

    // Hard violations always block; soft violations (unlisted binaries) also block here
    // because this path is reached only when on_violation == Fail (the Ask path is
    // handled by the executor pre-check before calling execute_bash_action).
    validate_shell_action_hard(action, sandbox)?;
    validate_shell_action_soft(action, sandbox)?;

    // If no streaming context or no output capture, use the original blocking path.
    if stream_ctx.is_none() || !action.capture_output {
        let scrub = scrub.or_else(|| stream_ctx.as_ref().and_then(|ctx| ctx.scrub.clone()));
        return execute_bash_action_blocking(action, scrub).await;
    }

    let ctx = stream_ctx.unwrap();
    execute_bash_action_streaming(action, ctx).await
}

/// The `sh -c <command>` invocation both bash paths spawn, short of stdio
/// and the timeout: the shell resolved against the PATH the child will
/// receive, the working directory validated, the action's env applied
/// verbatim, then the git ceiling fence. Kept spawn-free so the spawn
/// contract can be asserted without starting a process.
fn build_bash_command(action: &BashAction) -> Result<tokio::process::Command, ExecutionError> {
    use std::ffi::{OsStr, OsString};

    // The action's env is model-supplied and applied unfiltered below. When
    // it carries `PATH`, a bare shell name would force std onto `fork`
    // instead of `posix_spawn` — and `spawn` is synchronous, so the timeout
    // around the wait could not rescue a forked copy hung in an atfork
    // handler. Resolve the shell against the PATH the child actually gets:
    // the model's when it supplied one, else the process PATH (see
    // `runtime_core::process`).
    let shell = runtime_core::process::resolve_program(
        OsStr::new("sh"),
        action.env.get("PATH").map(OsStr::new),
    );
    // Wrapped in the OS-sandbox gate when enabled (repo read-only); identical to
    // `sh -c <command>` when the gate is off (default). See `os_sandbox_command`.
    let mut cmd = crate::magician_v2::execution::coding_engine::os_sandbox_command(
        shell.as_os_str(),
        &[
            OsString::from("-c"),
            OsString::from(action.command.as_str()),
        ],
    );
    cmd.kill_on_drop(true);

    if let Some(dir) = &action.working_dir {
        if !dir.exists() {
            return Err(ExecutionError::Step(format!(
                "Working directory '{}' does not exist",
                dir.display()
            )));
        }
        cmd.current_dir(dir);
    }

    for (key, value) in &action.env {
        cmd.env(key, value);
    }

    // Isolation: stop git from walking above the sandbox base to discover the
    // live repo's .git (defense-in-depth for shadow workspaces nested inside the
    // repo tree). Set after the action env so the fence value is authoritative.
    if let Some((_, sandbox_base)) =
        crate::magician_v2::execution::coding_engine::live_repo_source_fence()
    {
        cmd.env("GIT_CEILING_DIRECTORIES", &sandbox_base);
    }

    Ok(cmd)
}

/// Original blocking execution path (no streaming).
async fn execute_bash_action_blocking(
    action: &BashAction,
    scrub: Option<Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
) -> Result<ActionResult, ExecutionError> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let mut cmd = build_bash_command(action)?;

    let timeout_secs = action
        .timeout_secs
        .unwrap_or(DEFAULT_SHELL_TIMEOUT_SECS)
        .max(1);
    let timeout_duration = Duration::from_secs(timeout_secs);

    // When the action supplies stdin, we have to spawn manually instead
    // of using `cmd.output()` so we can pipe input before waiting. The
    // child's stdin is closed (EOF) after the payload is written so
    // tools that read-until-EOF (jq, gh --body-file -, etc.) terminate
    // their input parse.
    let output = if let Some(stdin_payload) = action.stdin.as_ref() {
        cmd.stdin(Stdio::piped());
        if action.capture_output {
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());
        } else {
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| ExecutionError::Step(format!("Failed to spawn command: {}", e)))?;
        if let Some(mut child_stdin) = child.stdin.take() {
            // Drop on error — the wait below will surface a meaningful
            // exit status. Logging here keeps the failure visible.
            if let Err(e) = child_stdin.write_all(stdin_payload.as_bytes()).await {
                warn!("Failed to write stdin to child: {}", e);
            }
            // Explicit drop closes the write end so the child sees EOF.
            drop(child_stdin);
        }
        match timeout(timeout_duration, child.wait_with_output()).await {
            Ok(result) => result
                .map_err(|e| ExecutionError::Step(format!("Failed to wait for command: {}", e)))?,
            Err(_) => {
                return Err(ExecutionError::Step(format!(
                    "Command timed out after {} seconds",
                    timeout_secs
                )));
            },
        }
    } else if action.capture_output {
        match timeout(timeout_duration, cmd.output()).await {
            Ok(result) => result
                .map_err(|e| ExecutionError::Step(format!("Failed to execute command: {}", e)))?,
            Err(_) => {
                return Err(ExecutionError::Step(format!(
                    "Command timed out after {} seconds",
                    timeout_secs
                )));
            },
        }
    } else {
        let status = match timeout(timeout_duration, cmd.status()).await {
            Ok(result) => result
                .map_err(|e| ExecutionError::Step(format!("Failed to execute command: {}", e)))?,
            Err(_) => {
                return Err(ExecutionError::Step(format!(
                    "Command timed out after {} seconds",
                    timeout_secs
                )));
            },
        };
        std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    };

    let exit_code = output.status.code().unwrap_or(-1);
    // Scrubbed before anything reads it: the log line, the returned error and
    // the result all carry whatever the process wrote, and a tool fed a
    // credential on stdin commonly echoes it.
    let replacements = scrub
        .as_ref()
        .and_then(|delivered| delivered.lock().ok())
        .map(|delivered| crate::magician_v2::secrets::known_value_replacements(&delivered))
        .unwrap_or_default();
    let scrubbed = |text: String| -> String {
        replacements
            .iter()
            .fold(text, |acc, value| acc.replace(value, "[REDACTED]"))
    };
    let stdout = scrubbed(String::from_utf8_lossy(&output.stdout).to_string());
    let stderr = scrubbed(String::from_utf8_lossy(&output.stderr).to_string());

    if !output.status.success() {
        warn!(
            "Command failed with exit code {}: {}",
            exit_code,
            if stderr.is_empty() { &stdout } else { &stderr }
        );
        return Err(ExecutionError::Step(format!(
            "Command failed with exit code {}: {}",
            exit_code,
            if stderr.is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        )));
    }

    info!(
        "Command completed with exit code {}, {} bytes output",
        exit_code,
        stdout.len()
    );

    Ok(ActionResult::text(stdout))
}

/// Streaming execution path: reads stdout/stderr line-by-line, batches output
/// every 100ms, and broadcasts `ShellOutputChunk` events via the event bus.
async fn execute_bash_action_streaming(
    action: &BashAction,
    ctx: ShellStreamContext,
) -> Result<ActionResult, ExecutionError> {
    use std::process::Stdio;

    let mut cmd = build_bash_command(action)?;
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // The action's stdin payload is the process's only credential sink
    // (P4): piped and closed after the write exactly as the blocking path
    // does, never inherited from the service.
    if action.stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let timeout_secs = action
        .timeout_secs
        .unwrap_or(DEFAULT_SHELL_TIMEOUT_SECS)
        .max(1);
    let timeout_duration = Duration::from_secs(timeout_secs);

    let mut child = cmd
        .spawn()
        .map_err(|e| ExecutionError::Step(format!("Failed to spawn command: {}", e)))?;

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    if let (Some(payload), Some(mut child_stdin)) = (action.stdin.clone(), child.stdin.take()) {
        // Written on its own task so a child that never reads cannot stall
        // the output readers; the write end drops (EOF) either way.
        tokio::spawn(async move {
            if let Err(e) = child_stdin.write_all(payload.as_bytes()).await {
                warn!("Failed to write stdin to streamed child: {}", e);
            }
            drop(child_stdin);
        });
    }

    // Channel for reader tasks to send lines to the main batching loop.
    // Tagged with stream name ("stdout" / "stderr").
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<(String, String)>(256);

    // Spawn stdout reader task (store handle for cleanup on early return)
    // Uses raw byte reading with `read_until` to handle invalid UTF-8 gracefully
    // (AsyncBufReadExt::lines() silently drops lines containing non-UTF-8 bytes).
    let stdout_task = if let Some(stdout) = stdout_handle {
        let tx = line_tx.clone();
        Some(tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf)
                            .trim_end_matches('\n')
                            .to_string();
                        if tx.send(("stdout".to_string(), line)).await.is_err() {
                            break;
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Error reading stdout stream: {}", e);
                        break;
                    },
                }
            }
        }))
    } else {
        None
    };

    // Spawn stderr reader task (store handle for cleanup on early return)
    // Uses raw byte reading with `read_until` to handle invalid UTF-8 gracefully.
    let stderr_task = if let Some(stderr) = stderr_handle {
        let tx = line_tx.clone();
        Some(tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf)
                            .trim_end_matches('\n')
                            .to_string();
                        if tx.send(("stderr".to_string(), line)).await.is_err() {
                            break;
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Error reading stderr stream: {}", e);
                        break;
                    },
                }
            }
        }))
    } else {
        None
    };

    // Drop the extra sender so the channel closes when both reader tasks finish.
    drop(line_tx);

    // Batching loop: accumulate lines and flush every 100ms.
    let mut interval = tokio::time::interval(Duration::from_millis(STREAM_BATCH_INTERVAL_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut sequence: u32 = 0;
    let mut total_bytes: usize = 0;
    let mut total_lines: usize = 0;
    let mut truncated = false;
    let mut is_first_chunk = true;
    let command_str = action.command.clone();

    // Pending buffers: separate stdout and stderr batches.
    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();

    // Full output collection for the return value.
    let mut all_stdout = String::new();
    let mut all_stderr = String::new();

    let mut readers_done = false;

    let stream_result = timeout(timeout_duration, async {
        loop {
            tokio::select! {
                maybe_line = line_rx.recv() => {
                    match maybe_line {
                        Some((stream, line)) => {
                            // Always collect for the final return value.
                            if stream == "stdout" {
                                if !all_stdout.is_empty() {
                                    all_stdout.push('\n');
                                }
                                all_stdout.push_str(&line);
                            } else {
                                if !all_stderr.is_empty() {
                                    all_stderr.push('\n');
                                }
                                all_stderr.push_str(&line);
                            }

                            if !truncated {
                                total_lines += 1;
                                let line_bytes = line.len() + 1; // +1 for newline
                                total_bytes += line_bytes;

                                if total_bytes > STREAM_MAX_TOTAL_BYTES || total_lines > STREAM_MAX_TOTAL_LINES {
                                    truncated = true;
                                    let warning = if total_bytes > STREAM_MAX_TOTAL_BYTES {
                                        "[Output truncated at 1MB]".to_string()
                                    } else {
                                        "[Output truncated at 10,000 lines]".to_string()
                                    };
                                    // Flush whatever we have, then emit truncation warning.
                                    flush_stream_buffer(
                                        &ctx, &mut stdout_buf, "stdout", &mut sequence,
                                        &command_str, &mut is_first_chunk,
                                    );
                                    flush_stream_buffer(
                                        &ctx, &mut stderr_buf, "stderr", &mut sequence,
                                        &command_str, &mut is_first_chunk,
                                    );
                                    // Emit truncation as stderr chunk
                                    broadcast_chunk(
                                        &ctx, "stderr", &warning, &mut sequence,
                                        &command_str, &mut is_first_chunk, false, None,
                                    );
                                    continue;
                                }

                                let buf = if stream == "stdout" { &mut stdout_buf } else { &mut stderr_buf };
                                if !buf.is_empty() {
                                    buf.push('\n');
                                }
                                buf.push_str(&line);

                                // Immediate flush if chunk exceeds size cap
                                if buf.len() >= STREAM_MAX_CHUNK_BYTES {
                                    let stream_name = if stream == "stdout" { "stdout" } else { "stderr" };
                                    flush_stream_buffer(
                                        &ctx, buf, stream_name, &mut sequence,
                                        &command_str, &mut is_first_chunk,
                                    );
                                }
                            }
                        },
                        None => {
                            // Both reader tasks finished (channel closed).
                            readers_done = true;
                            break;
                        },
                    }
                },
                _ = interval.tick() => {
                    // Periodic flush of accumulated buffers.
                    if !truncated {
                        flush_stream_buffer(
                            &ctx, &mut stdout_buf, "stdout", &mut sequence,
                            &command_str, &mut is_first_chunk,
                        );
                        flush_stream_buffer(
                            &ctx, &mut stderr_buf, "stderr", &mut sequence,
                            &command_str, &mut is_first_chunk,
                        );
                    }
                },
            }
        }
        Ok(())
    })
    .await;

    // Convert the nested Result: outer Err = timeout, inner Ok = stream_result.
    let timed_out = stream_result.is_err();
    let stream_err: Option<ExecutionError> = match stream_result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_elapsed) => Some(ExecutionError::Step(format!(
            "Command timed out after {} seconds",
            timeout_secs
        ))),
    };

    // Abort reader tasks so they don't leak if we exited early (C2 fix).
    if let Some(h) = stdout_task {
        h.abort();
    }
    if let Some(h) = stderr_task {
        h.abort();
    }

    // Flush any remaining buffered data.
    if !truncated {
        flush_stream_buffer(
            &ctx,
            &mut stdout_buf,
            "stdout",
            &mut sequence,
            &command_str,
            &mut is_first_chunk,
        );
        flush_stream_buffer(
            &ctx,
            &mut stderr_buf,
            "stderr",
            &mut sequence,
            &command_str,
            &mut is_first_chunk,
        );
    }

    // Wait for the child process to exit (or kill on timeout).
    let exit_code = if readers_done && !timed_out {
        // Readers are done, just wait for exit status.
        match timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => None,
        }
    } else {
        // Timeout or error path — kill the child.
        let _ = child.kill().await;
        match child.wait().await {
            Ok(status) => status.code(),
            Err(_) => None,
        }
    };

    // ALWAYS emit final chunk with exit code, even on error paths (C1 fix).
    broadcast_chunk(
        &ctx,
        "stdout",
        "",
        &mut sequence,
        &command_str,
        &mut is_first_chunk,
        true,
        exit_code,
    );

    // Now propagate any stream/timeout error.
    if let Some(err) = stream_err {
        return Err(err);
    }

    let code = exit_code.unwrap_or(-1);
    // What leaves this path — the failure text the model and the journal see,
    // the log line, the result — is scrubbed with the run's delivered set.
    let all_stdout = ctx.scrub_text(&all_stdout);
    let all_stderr = ctx.scrub_text(&all_stderr);

    if code != 0 {
        warn!(
            "Command failed with exit code {}: {}",
            code,
            if all_stderr.is_empty() {
                &all_stdout
            } else {
                &all_stderr
            }
        );
        return Err(ExecutionError::Step(format!(
            "Command failed with exit code {}: {}",
            code,
            if all_stderr.is_empty() {
                all_stdout.trim()
            } else {
                all_stderr.trim()
            }
        )));
    }

    info!(
        "Command completed with exit code {}, {} bytes output (streamed)",
        code,
        all_stdout.len()
    );

    Ok(ActionResult::text(all_stdout))
}

/// Flush a stream buffer as a `ShellOutputChunk` event if non-empty.
fn flush_stream_buffer(
    ctx: &ShellStreamContext,
    buf: &mut String,
    stream: &str,
    sequence: &mut u32,
    command: &str,
    is_first_chunk: &mut bool,
) {
    if buf.is_empty() {
        return;
    }
    let data = std::mem::take(buf);
    broadcast_chunk(
        ctx,
        stream,
        &data,
        sequence,
        command,
        is_first_chunk,
        false,
        None,
    );
}

/// Broadcast a single `ShellOutputChunk` event.
fn broadcast_chunk(
    ctx: &ShellStreamContext,
    stream: &str,
    data: &str,
    sequence: &mut u32,
    command: &str,
    is_first_chunk: &mut bool,
    is_final: bool,
    exit_code: Option<i32>,
) {
    let cmd_field = if *is_first_chunk {
        *is_first_chunk = false;
        command.to_string()
    } else {
        String::new()
    };

    let seq = *sequence;
    *sequence += 1;
    let data = ctx.scrub_text(data);
    let data = data.as_str();

    let event = RuntimeTransportEvent::ShellOutputChunk {
        execution_id: ctx.execution_id.clone(),
        principal: ctx.principal.clone(),
        workspace: ctx.workspace.clone(),
        step_id: ctx.step_id.clone(),
        step_index: ctx.step_index,
        command: cmd_field,
        stream: stream.to_string(),
        data: data.to_string(),
        sequence: seq,
        is_final,
        exit_code,
        timestamp: chrono::Utc::now().timestamp_millis(),
    };

    ctx.broadcaster.emit_transport_only(event);
}

const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 60;

/// Check for soft sandbox violations (unlisted binaries in `allowed_binaries`).
///
/// Returns `Some(violation_description)` when the command uses a binary not in
/// the allowlist. Returns `None` when the command is clean or `allowed_binaries`
/// is empty. Hard violations (blocked fragments, syntax, etc.) are not checked
/// here — use `validate_shell_action_hard` for those.
///
/// This is used by the executor to decide whether to pause and ask the user
/// (when `on_violation == Ask`) before the action reaches `execute_bash_action`,
/// which would hard-fail on the same violation.
/// Check for "soft" sandbox violations that respect `on_violation` (e.g. unlisted binaries).
/// Returns `None` when the command is clean or only has hard violations (which are enforced
/// unconditionally by `validate_shell_action_hard`).
pub fn check_shell_sandbox_violation(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Option<String> {
    match validate_shell_action_soft(action, sandbox) {
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// Hard violations — always blocked regardless of `on_violation`.
/// Covers: blocked_command_fragments, syntax errors, empty commands, working dir.
pub fn validate_shell_action_hard(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Result<(), ExecutionError> {
    // Isolation deny-fence: an EXPLICIT working_dir must not resolve into the live
    // magician repo source tree. Sandboxed shells / dugite would otherwise mutate
    // the user's real repo (M3 live test: a delegated `shell working_dir=<live
    // repo>` branched HEAD + git-add'd; DelegationShellProvider even pushes the
    // requested working_dir into allowed_working_dirs, so the allowlist below
    // would pass it). Only the explicit case is fenced — a None working_dir falls
    // through to validate_working_dir (fencing None here would reject every no-cwd
    // command, since the process CWD is the repo). Allows the scope sandbox +
    // external dirs.
    //
    // Always armed for SANDBOXED modes (the pre-existing baseline). For
    // Unrestricted mode it arms ONLY inside a coding context, so the DEFAULT path
    // stays equivalent — a general Unrestricted `working_dir=<repo>` shell (e.g.
    // running make/cargo/npm whose target/ writes land in-repo) is not newly
    // rejected; a coding agent's Unrestricted shell into the repo is fenced (and
    // the OS sandbox is the real boundary there anyway).
    let unrestricted = matches!(sandbox.mode, ShellSandboxMode::Unrestricted);
    if !unrestricted || crate::magician_v2::execution::coding_engine::coding_context_active() {
        if let Some(explicit) = action.working_dir.as_deref() {
            if let Some((repo_source_root, sandbox_base)) =
                crate::magician_v2::execution::coding_engine::live_repo_source_fence()
            {
                if let Ok(canonical) = explicit.canonicalize() {
                    crate::magician_v2::execution::coding_engine::reject_repo_source_tree(
                        &canonical,
                        &repo_source_root,
                        &sandbox_base,
                    )
                    .map_err(ExecutionError::Step)?;
                }
            }
        }
    }

    if unrestricted {
        return Ok(());
    }

    validate_shell_syntax(&action.command)?;

    let command_for_validation = strip_heredoc_body(&action.command);

    // Blocked command fragments — always hard-block, no exceptions.
    let command_lower = command_for_validation.to_lowercase();
    for blocked in &sandbox.blocked_command_fragments {
        let blocked_lower = blocked.to_lowercase();
        if !blocked_lower.is_empty() && command_lower.contains(&blocked_lower) {
            return Err(ExecutionError::Step(format!(
                "Command blocked by sandbox policy (matched '{}')",
                blocked
            )));
        }
    }

    let segments = split_pipeline_segments(&command_for_validation);
    if segments.is_empty() {
        return Err(ExecutionError::Step(
            "Shell command is empty or missing executable".to_string(),
        ));
    }

    for segment in segments {
        // Verify the segment has a recognisable binary.
        let _binary = first_command_binary(segment).ok_or_else(|| {
            ExecutionError::Step("Shell command is empty or missing executable".to_string())
        })?;
    }

    let working_dir = action
        .working_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    validate_working_dir(&working_dir, sandbox)?;

    Ok(())
}

/// Soft violations — policy-controlled checks that respect `on_violation`.
/// Covers: unlisted binaries (allowed_binaries), network restrictions, read-only mutations.
fn validate_shell_action_soft(
    action: &BashAction,
    sandbox: &ShellSandboxConfig,
) -> Result<(), ExecutionError> {
    if matches!(sandbox.mode, ShellSandboxMode::Unrestricted) {
        return Ok(());
    }

    let command_for_validation = strip_heredoc_body(&action.command);
    let segments = split_pipeline_segments(&command_for_validation);

    for segment in segments {
        if let Some(first_binary) = first_command_binary(segment) {
            if !sandbox.allowed_binaries.is_empty()
                && !sandbox
                    .allowed_binaries
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(&first_binary))
            {
                return Err(ExecutionError::Step(format!(
                    "Executable '{}' is not allowed by sandbox policy",
                    first_binary
                )));
            }
        }

        let segment_lower = segment.to_lowercase();
        if !sandbox.allow_network && uses_network_binary(&segment_lower) {
            return Err(ExecutionError::Step(
                "Network commands are disabled by sandbox policy".to_string(),
            ));
        }

        if matches!(sandbox.mode, ShellSandboxMode::ReadOnly) && is_mutating_command(&segment_lower)
        {
            return Err(ExecutionError::Step(
                "Command appears to modify state and is blocked in read-only mode".to_string(),
            ));
        }
    }

    Ok(())
}

fn validate_shell_syntax(command: &str) -> Result<(), ExecutionError> {
    // Only validate the command portion, not heredoc bodies (which are just data).
    let cmd_portion = strip_heredoc_body(command);
    // Block subshell escapes but allow control-flow operators (&&, ||, ;) that the
    // LLM uses legitimately for error handling and sequential chaining.
    const DISALLOWED_TOKENS: [&str; 5] = ["`", "$(", "${", "<(", ">("];
    for token in DISALLOWED_TOKENS {
        if cmd_portion.contains(token) {
            return Err(ExecutionError::Step(format!(
                "Shell command contains disallowed token '{}'",
                token
            )));
        }
    }
    Ok(())
}

/// Strip heredoc bodies from a command string, returning only the command portion.
///
/// Heredocs (`<< DELIM`, `<< 'DELIM'`, `<<- DELIM`) feed data to stdin and are
/// not executable code. The sandbox should only validate the command itself,
/// not the heredoc content which may contain arbitrary text.
fn strip_heredoc_body(command: &str) -> String {
    // Find heredoc operator: << or <<-
    // The delimiter follows, optionally quoted with ' or "
    let Some(heredoc_pos) = command.find("<<") else {
        return command.to_string();
    };

    // Extract just the first line (command line) up to the newline after heredoc start
    let first_newline = command[heredoc_pos..].find('\n');
    match first_newline {
        Some(nl) => command[..heredoc_pos + nl].to_string(),
        None => command.to_string(), // no newline = no heredoc body yet
    }
}

/// Split a shell command on unquoted, unescaped `|` pipe characters.
///
/// Respects single quotes (`'...'`), double quotes (`"..."`), and backslash
/// escapes so that patterns like `grep -i "pdf\|invoice"` are not split on the
/// `|` inside the quoted string or after a backslash.
fn split_pipeline_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    for (i, ch) in command.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                escaped = true;
            },
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            },
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            },
            '|' if !in_single_quote && !in_double_quote => {
                let seg = command[start..i].trim();
                if !seg.is_empty() {
                    segments.push(seg);
                }
                start = i + 1;
            },
            _ => {},
        }
    }
    let seg = command[start..].trim();
    if !seg.is_empty() {
        segments.push(seg);
    }
    segments
}

fn first_command_binary(command: &str) -> Option<String> {
    let mut tokens = command.split_whitespace().peekable();

    while let Some(token) = tokens.peek().copied() {
        if is_env_assignment(token) {
            tokens.next();
            continue;
        }
        break;
    }

    let token = tokens.next()?;
    let stripped = token.trim_matches(|c: char| c == '"' || c == '\'' || c == ';' || c == '|');
    if stripped.is_empty() {
        return None;
    }
    let binary = stripped
        .rsplit('/')
        .next()
        .map(ToString::to_string)
        .unwrap_or_else(|| stripped.to_string());
    Some(binary)
}

fn is_env_assignment(token: &str) -> bool {
    if token.starts_with('-') || token.starts_with('/') || token.contains('/') {
        return false;
    }
    let mut split = token.splitn(2, '=');
    let key = split.next().unwrap_or_default();
    let value = split.next();
    !key.is_empty()
        && value.is_some()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn uses_network_binary(command_lower: &str) -> bool {
    const NETWORK_HINTS: [&str; 9] = [
        "curl ", "wget ", "http ", "https ", "nc ", "ncat ", "ping ", "telnet ", "nmap ",
    ];
    NETWORK_HINTS
        .iter()
        .any(|hint| command_lower.contains(hint))
}

fn is_mutating_command(command_lower: &str) -> bool {
    const MUTATING_HINTS: [&str; 18] = [
        "rm ",
        "mv ",
        "cp ",
        "mkdir ",
        "touch ",
        "chmod ",
        "chown ",
        "truncate ",
        "tee ",
        "sed -i",
        "perl -pi",
        ">",
        ">>",
        "git commit",
        "git push",
        "pip install",
        "npm install",
        "cargo add",
    ];
    MUTATING_HINTS
        .iter()
        .any(|hint| command_lower.contains(hint))
}

fn validate_working_dir(dir: &Path, sandbox: &ShellSandboxConfig) -> Result<(), ExecutionError> {
    let canonical_dir = dir.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to canonicalize working directory '{}': {}",
            dir.display(),
            e
        ))
    })?;

    let mut allowed_roots = Vec::new();
    for configured in &sandbox.allowed_working_dirs {
        let candidate = PathBuf::from(configured);
        let resolved = if candidate.is_absolute() {
            candidate
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(candidate)
        };
        if let Ok(canonical_root) = resolved.canonicalize() {
            allowed_roots.push(canonical_root);
        }
    }

    if allowed_roots.is_empty() {
        return Ok(());
    }

    let allowed = allowed_roots
        .iter()
        .any(|root| canonical_dir.starts_with(root));

    if allowed {
        Ok(())
    } else {
        Err(ExecutionError::Step(format!(
            "Working directory '{}' is outside sandbox allowed roots",
            dir.display()
        )))
    }
}

// ============================================================================
// Security Utilities
// ============================================================================

/// Validate a path is within allowed boundaries (prevent directory traversal)
pub fn validate_path_security(path: &Path, allowed_roots: &[&Path]) -> Result<(), ExecutionError> {
    let canonical = path.canonicalize().map_err(|e| {
        ExecutionError::Step(format!(
            "Failed to canonicalize path '{}': {}",
            path.display(),
            e
        ))
    })?;

    // Check if path is under any allowed root
    for root in allowed_roots {
        if let Ok(root_canonical) = root.canonicalize() {
            if canonical.starts_with(&root_canonical) {
                return Ok(());
            }
        }
    }

    Err(ExecutionError::Step(format!(
        "Path '{}' is not within allowed directories",
        path.display()
    )))
}

// ============================================================================
// DuckDB Executor
// ============================================================================

use super::actions::DuckDbAction;
use duckdb::types::Value as DuckValue;
use std::sync::Mutex;

use crate::magician_v2::analytics::duckdb_safety::{
    configure_analytics_connection_checked, duckdb_value_ref_output_bytes,
    json_string_encoded_bytes, run_analytics_query_with_interrupt_timeout,
    try_analytics_duckdb_guard_for, AnalyticsDuckDbQueryError, ANALYTICS_DUCKDB_MAX_RESULT_BYTES,
    ANALYTICS_DUCKDB_MAX_RESULT_ROWS,
};

const DEFAULT_DUCKDB_TIMEOUT_SECS: u64 = 120;
const DUCKDB_EXTENSION_INIT_TIMEOUT_SECS: u64 = 30;

/// Session-scoped DuckDB connection holder.
///
/// Wraps a persistent in-memory DuckDB connection so tables/views created
/// in one query survive across subsequent queries within the same agent session.
pub struct DuckDbSession {
    conn: Mutex<duckdb::Connection>,
}

impl DuckDbSession {
    /// Create a new session with an in-memory database.
    ///
    /// Pre-loads the sqlite_scanner extension so tools like `imessage` can
    /// query SQLite databases without per-query setup.
    pub fn new() -> Result<Self, ExecutionError> {
        let init_timeout = Duration::from_secs(DEFAULT_DUCKDB_TIMEOUT_SECS);
        let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(init_timeout) else {
            return Err(duckdb_capacity_timeout_error(init_timeout));
        };
        let conn = duckdb::Connection::open_in_memory().map_err(|e| {
            ExecutionError::Step(format!("Failed to open DuckDB in-memory connection: {}", e))
        })?;
        configure_analytics_connection_checked(&conn, "generic_duckdb_session").map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to apply DuckDB session safety settings: {e}"
            ))
        })?;

        // Pre-load sqlite_scanner so sqlite_scan() is available immediately.
        // Failures are non-fatal — the extension may not be installed yet.
        match run_analytics_query_with_interrupt_timeout(
            &conn,
            Duration::from_secs(DUCKDB_EXTENSION_INIT_TIMEOUT_SECS),
            || conn.execute_batch("INSTALL sqlite_scanner; LOAD sqlite_scanner;"),
        ) {
            Ok(()) => {},
            Err(AnalyticsDuckDbQueryError::Query(error)) => {
                tracing::debug!("sqlite_scanner pre-load skipped: {}", error);
            },
            Err(AnalyticsDuckDbQueryError::TimedOut(source)) => {
                tracing::warn!(
                    source = ?source,
                    "sqlite_scanner pre-load timed out; continuing without the extension"
                );
            },
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn connection(&self) -> &Mutex<duckdb::Connection> {
        &self.conn
    }
}

impl std::fmt::Debug for DuckDbSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDbSession").finish()
    }
}

/// Execute a DuckDB action against a session-scoped connection.
///
/// If `action.database` is set, opens a file-backed connection for that query.
/// Otherwise uses the shared session connection for persistent in-memory state.
pub fn execute_duckdb_action(
    action: &DuckDbAction,
    session: &DuckDbSession,
) -> Result<ActionResult, ExecutionError> {
    let timeout = Duration::from_secs(action.timeout_secs.unwrap_or(DEFAULT_DUCKDB_TIMEOUT_SECS));
    execute_duckdb_action_with_timeout(action, session, timeout)
}

pub fn execute_duckdb_action_with_timeout(
    action: &DuckDbAction,
    session: &DuckDbSession,
    timeout: Duration,
) -> Result<ActionResult, ExecutionError> {
    let started_at = Instant::now();
    let Some(_duckdb_guard) = try_analytics_duckdb_guard_for(timeout) else {
        return Err(duckdb_capacity_timeout_error(timeout));
    };
    let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
        return Err(duckdb_capacity_timeout_error(timeout));
    };
    execute_duckdb_action_inner(action, session, Some((Instant::now(), remaining)))
}

fn execute_duckdb_action_inner(
    action: &DuckDbAction,
    session: &DuckDbSession,
    budget: Option<(Instant, Duration)>,
) -> Result<ActionResult, ExecutionError> {
    debug!("Executing DuckDB action: {}", action.description());
    let normalized_sql = normalize_duckdb_sql_paths(&action.sql);
    if normalized_sql != action.sql {
        debug!("Normalized DuckDB sqlite_scan() path literals before execution");
    }

    // If a specific database file is requested, open a separate connection.
    // Otherwise use the session's persistent in-memory connection.
    if let Some(ref db_path) = action.database {
        // Sandbox: restrict database paths to magician_data_v3/ using the same
        // resolve_path_for_policy infrastructure used by file action sandboxing.
        // This handles symlinks, path traversal (..), non-existent files, and absolute paths.
        let resolved = resolve_path_for_policy(std::path::Path::new(db_path))?;
        // Canonicalize the allowed root the same way resolve_path_for_policy does,
        // so both sides of the starts_with check use the same path representation.
        // Without this, macOS symlink layers (/var vs /private/var) cause false rejections.
        let raw_root = std::env::current_dir()
            .map_err(|e| {
                ExecutionError::Step(format!("Failed to get cwd for DuckDB sandbox check: {}", e))
            })?
            .join("magician_data_v3");
        let allowed_root = resolve_path_for_policy(&raw_root)?;
        if !resolved.starts_with(&allowed_root) {
            return Err(ExecutionError::Step(format!(
                "DuckDB database path '{}' resolves outside magician_data_v3/ — \
                 file-backed databases must be under magician_data_v3/",
                db_path
            )));
        }
        // Open using the resolved (canonicalized) path, not the raw user-provided string,
        // to ensure we open exactly the path that was validated.
        let conn = duckdb::Connection::open(&resolved).map_err(|e| {
            ExecutionError::Step(format!(
                "Failed to open DuckDB database '{}': {}",
                db_path, e
            ))
        })?;
        configure_analytics_connection_checked(&conn, "generic_duckdb_file").map_err(|e| {
            ExecutionError::Step(format!("Failed to apply DuckDB file safety settings: {e}"))
        })?;
        execute_sql_on_connection_with_deadline(
            &conn,
            &normalized_sql,
            &action.output_format,
            budget,
        )
    } else {
        let conn = session.connection().lock().map_err(|e| {
            ExecutionError::Step(format!("Failed to acquire DuckDB session lock: {}", e))
        })?;
        execute_sql_on_connection_with_deadline(
            &conn,
            &normalized_sql,
            &action.output_format,
            budget,
        )
    }
}

fn execute_sql_on_connection_with_deadline(
    conn: &duckdb::Connection,
    sql: &str,
    output_format: &str,
    budget: Option<(Instant, Duration)>,
) -> Result<ActionResult, ExecutionError> {
    let Some((started_at, timeout)) = budget else {
        return execute_sql_on_connection(conn, sql, output_format);
    };
    let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
        return Err(duckdb_timeout_error(timeout, None));
    };

    match run_analytics_query_with_interrupt_timeout(conn, remaining, || {
        execute_sql_on_connection(conn, sql, output_format)
    }) {
        Ok(result) => Ok(result),
        Err(AnalyticsDuckDbQueryError::Query(error)) => Err(error),
        Err(AnalyticsDuckDbQueryError::TimedOut(source)) => {
            Err(duckdb_timeout_error(timeout, source))
        },
    }
}

fn duckdb_timeout_error(timeout: Duration, source: Option<ExecutionError>) -> ExecutionError {
    let detail = source.map(|error| format!(": {error}")).unwrap_or_default();
    ExecutionError::Step(format!(
        "DuckDB action timed out after {}s{detail}",
        timeout.as_secs()
    ))
}

fn duckdb_capacity_timeout_error(timeout: Duration) -> ExecutionError {
    ExecutionError::Step(format!(
        "DuckDB action timed out after {}s waiting for analytics capacity",
        timeout.as_secs()
    ))
}

fn normalize_duckdb_sql_paths(sql: &str) -> String {
    let home_dir = current_home_dir();
    let current_user = current_os_username(home_dir.as_deref());
    normalize_duckdb_sql_paths_with_context(sql, home_dir.as_deref(), current_user.as_deref())
}

fn normalize_duckdb_sql_paths_with_context(
    sql: &str,
    home_dir: Option<&str>,
    current_user: Option<&str>,
) -> String {
    const SQLITE_SCAN_FN: &[u8] = b"sqlite_scan";

    let sql_bytes = sql.as_bytes();
    let lower = sql.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    let mut normalized = String::with_capacity(sql.len());
    let mut last_copied = 0usize;
    let mut i = 0usize;

    while i + SQLITE_SCAN_FN.len() <= sql_bytes.len() {
        if !lower_bytes[i..].starts_with(SQLITE_SCAN_FN) {
            i += 1;
            continue;
        }
        if i > 0 && is_sql_identifier_byte(sql_bytes[i - 1]) {
            i += 1;
            continue;
        }

        let after_name = i + SQLITE_SCAN_FN.len();
        if after_name < sql_bytes.len() && is_sql_identifier_byte(sql_bytes[after_name]) {
            i += 1;
            continue;
        }

        let mut cursor = after_name;
        while cursor < sql_bytes.len() && sql_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= sql_bytes.len() || sql_bytes[cursor] != b'(' {
            i = after_name;
            continue;
        }

        cursor += 1;
        while cursor < sql_bytes.len() && sql_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= sql_bytes.len() || sql_bytes[cursor] != b'\'' {
            i = after_name;
            continue;
        }

        let Some((path_end, path_literal)) = parse_single_quoted_sql_string(sql, cursor) else {
            i = after_name;
            continue;
        };

        let expanded =
            expand_duckdb_path_literal_with_context(&path_literal, home_dir, current_user);
        if expanded != path_literal {
            normalized.push_str(&sql[last_copied..cursor + 1]);
            normalized.push_str(&escape_sql_single_quoted_string(&expanded));
            normalized.push('\'');
            last_copied = path_end + 1;
        }

        i = path_end + 1;
    }

    if last_copied == 0 {
        return sql.to_string();
    }

    normalized.push_str(&sql[last_copied..]);
    normalized
}

fn current_home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|value| !value.is_empty())
}

fn current_os_username(home_dir: Option<&str>) -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            home_dir
                .and_then(|path| Path::new(path).file_name())
                .and_then(|segment| segment.to_str())
                .map(|segment| segment.to_string())
        })
}

fn expand_duckdb_path_literal_with_context(
    path: &str,
    home_dir: Option<&str>,
    current_user: Option<&str>,
) -> String {
    let mut expanded = path.to_string();

    if let Some(home_dir) = home_dir {
        expanded = expanded.replace("${HOME}", home_dir);
        expanded = replace_bare_shell_var(&expanded, "$HOME", home_dir);
        if expanded == "~" {
            expanded = home_dir.to_string();
        } else if let Some(stripped) = expanded.strip_prefix("~/") {
            expanded = format!("{}/{}", home_dir.trim_end_matches('/'), stripped);
        }
    }

    if let Some(current_user) = current_user {
        // Replace braced variants first (exact delimiters, no substring risk).
        expanded = expanded.replace("${CURRENT_USER}", current_user);
        expanded = expanded.replace("{CURRENT_USER}", current_user);
        expanded = expanded.replace("${USER}", current_user);
        // Replace bare $VAR only when followed by a non-identifier char to avoid
        // false substring matches (e.g., "$USER_cache" should NOT become "name_cache").
        expanded = replace_bare_shell_var(&expanded, "$CURRENT_USER", current_user);
        expanded = replace_bare_shell_var(&expanded, "$USER", current_user);
    }

    expanded
}

/// Replace bare `$VAR` only when it is NOT followed by an identifier character
/// (alphanumeric or underscore), preventing false substring matches like
/// `$USER_cache` → `name_cache`.
fn replace_bare_shell_var(input: &str, var: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut remaining = input;
    while let Some(pos) = remaining.find(var) {
        let after = pos + var.len();
        let next_char = remaining.as_bytes().get(after).copied();
        let is_boundary = match next_char {
            None => true,
            Some(ch) => !(ch.is_ascii_alphanumeric() || ch == b'_'),
        };
        result.push_str(&remaining[..pos]);
        if is_boundary {
            result.push_str(replacement);
        } else {
            result.push_str(var);
        }
        remaining = &remaining[after..];
    }
    result.push_str(remaining);
    result
}

fn parse_single_quoted_sql_string(sql: &str, quote_start: usize) -> Option<(usize, String)> {
    let bytes = sql.as_bytes();
    if bytes.get(quote_start) != Some(&b'\'') {
        return None;
    }

    let mut value = String::new();
    let mut segment_start = quote_start + 1;
    let mut cursor = quote_start + 1;

    while cursor < bytes.len() {
        if bytes[cursor] == b'\'' {
            if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'\'' {
                value.push_str(&sql[segment_start..cursor]);
                value.push('\'');
                cursor += 2;
                segment_start = cursor;
                continue;
            }

            value.push_str(&sql[segment_start..cursor]);
            return Some((cursor, value));
        }

        cursor += 1;
    }

    None
}

fn escape_sql_single_quoted_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn is_sql_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Run SQL on a connection and format the results.
///
/// Uses statement type detection to choose the correct execution path upfront,
/// avoiding the double-execution bug where `query_map` + `execute_batch` fallback
/// would run DML/DDL statements twice.
fn execute_sql_on_connection(
    conn: &duckdb::Connection,
    sql: &str,
    output_format: &str,
) -> Result<ActionResult, ExecutionError> {
    // Detect whether the SQL is a query (returns rows) or a statement (DDL/DML).
    // This avoids the double-execution bug: prepare+query_map executes the statement,
    // then on failure the fallback execute_batch would run it again.
    let stripped = strip_leading_sql_comments(sql);
    let trimmed = stripped.trim_start();

    // Check for RETURNING clause (DML that returns rows)
    let upper_full: String = trimmed.to_uppercase();
    let has_returning = upper_full.contains(" RETURNING ") || upper_full.ends_with(" RETURNING");

    let is_query = has_returning
        || (trimmed.len() >= 4 && {
            let upper: String = trimmed.chars().take(10).collect::<String>().to_uppercase();
            upper.starts_with("SELECT")
            || upper.starts_with("WITH")
            || upper.starts_with("SHOW")
            || upper.starts_with("DESCRIBE")
            || upper.starts_with("EXPLAIN")
            || upper.starts_with("PRAGMA")
            || upper.starts_with("FROM")      // DuckDB FROM-first syntax
            || upper.starts_with("VALUES")    // standalone VALUES returns rows
            || upper.starts_with("CALL")      // table-valued function calls
            || upper.starts_with("TABLE")     // DuckDB TABLE shorthand
            || upper.starts_with("PIVOT")     // DuckDB PIVOT returns result set
            || upper.starts_with("UNPIVOT")   // DuckDB UNPIVOT returns result set
            || upper.starts_with("SUMMARIZE") // DuckDB introspection query
        });

    if !is_query {
        // DDL/DML: execute without attempting to collect rows.
        conn.execute_batch(sql)
            .map_err(|e| ExecutionError::Step(format!("DuckDB execution error: {}", e)))?;
        return Ok(ActionResult::text("OK"));
    }

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| ExecutionError::Step(format!("DuckDB prepare error: {}", e)))?;

    let mut rows_iter = stmt
        .query([])
        .map_err(|e| ExecutionError::Step(format!("DuckDB query error: {}", e)))?;
    let stmt_ref = rows_iter
        .as_ref()
        .ok_or_else(|| ExecutionError::Step("DuckDB query did not expose metadata".to_string()))?;
    let column_count = stmt_ref.column_count();
    let column_names: Vec<String> = (0..column_count)
        .map(|i| {
            stmt_ref
                .column_name(i)
                .map_or("?".to_string(), |s| s.to_string())
        })
        .collect();

    let mut collected_rows: Vec<Vec<DuckValue>> = Vec::new();
    let repeated_column_bytes = column_names.iter().fold(0usize, |size, column| {
        size.saturating_add(json_string_encoded_bytes(column.as_bytes()))
            .saturating_add(4)
    });
    if repeated_column_bytes > ANALYTICS_DUCKDB_MAX_RESULT_BYTES {
        return Err(ExecutionError::Step(format!(
            "DuckDB result metadata exceeds the {} byte output limit",
            ANALYTICS_DUCKDB_MAX_RESULT_BYTES
        )));
    }
    let mut materialized_bytes = repeated_column_bytes;
    let mut truncated_by_rows = false;
    let mut truncated_by_bytes = false;
    'rows: while let Some(row) = rows_iter
        .next()
        .map_err(|e| ExecutionError::Step(format!("DuckDB row error: {}", e)))?
    {
        if collected_rows.len() >= ANALYTICS_DUCKDB_MAX_RESULT_ROWS {
            truncated_by_rows = true;
            break;
        }

        let mut row_bytes = repeated_column_bytes.saturating_add(8);
        let mut values: Vec<DuckValue> = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let value_ref = row.get_ref(i).map_err(|error| {
                ExecutionError::Step(format!("DuckDB column {i} decode error: {error}"))
            })?;
            if let Some(value_bytes) = duckdb_value_ref_output_bytes(value_ref) {
                if materialized_bytes
                    .saturating_add(row_bytes)
                    .saturating_add(value_bytes)
                    > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
                {
                    truncated_by_bytes = true;
                    break 'rows;
                }
            }

            let value = value_ref.to_owned();
            let value_bytes = duckvalue_output_bytes(&value);
            if materialized_bytes
                .saturating_add(row_bytes)
                .saturating_add(value_bytes)
                > ANALYTICS_DUCKDB_MAX_RESULT_BYTES
            {
                truncated_by_bytes = true;
                break 'rows;
            }
            row_bytes = row_bytes.saturating_add(value_bytes);
            values.push(value);
        }
        materialized_bytes = materialized_bytes.saturating_add(row_bytes);
        collected_rows.push(values);
    }

    let output = match output_format {
        "csv" => format_as_csv(&column_names, &collected_rows),
        "table" => format_as_table(&column_names, &collected_rows),
        _ => format_as_json(&column_names, &collected_rows),
    };
    let output = finalize_duckdb_output(
        output,
        truncated_by_rows,
        truncated_by_bytes,
        ANALYTICS_DUCKDB_MAX_RESULT_BYTES,
    );

    Ok(ActionResult::text(output))
}

fn finalize_duckdb_output(
    mut output: String,
    truncated_by_rows: bool,
    truncated_by_bytes: bool,
    max_bytes: usize,
) -> String {
    let truncated_by_bytes = truncated_by_bytes || output.len() > max_bytes;
    if truncated_by_rows || truncated_by_bytes {
        let reason = if truncated_by_bytes {
            format!("{max_bytes} bytes")
        } else {
            format!("{} rows", ANALYTICS_DUCKDB_MAX_RESULT_ROWS)
        };
        let notice =
            format!("\n\n[Output truncated at {reason}. Use LIMIT or WHERE to narrow results.]");
        truncate_utf8_to_len(&mut output, max_bytes.saturating_sub(notice.len()));
        if notice.len() <= max_bytes {
            output.push_str(&notice);
        }
    }

    debug_assert!(output.len() <= max_bytes);
    output
}

fn truncate_utf8_to_len(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
}

fn duckvalue_output_bytes(value: &DuckValue) -> usize {
    match value {
        DuckValue::Text(text) => json_string_encoded_bytes(text.as_bytes()),
        DuckValue::Blob(bytes) => bytes
            .len()
            .saturating_add(2)
            .checked_div(3)
            .unwrap_or(usize::MAX)
            .saturating_mul(4)
            .saturating_add(2),
        DuckValue::List(values) | DuckValue::Array(values) => {
            values.iter().fold(2usize, |size, value| {
                size.saturating_add(duckvalue_output_bytes(value))
                    .saturating_add(1)
            })
        },
        DuckValue::Struct(values) => values.iter().fold(2usize, |size, (key, value)| {
            size.saturating_add(json_string_encoded_bytes(key.as_bytes()))
                .saturating_add(duckvalue_output_bytes(value))
                .saturating_add(2)
        }),
        DuckValue::Map(values) => values.iter().fold(2usize, |size, (key, value)| {
            size.saturating_add(duckvalue_output_bytes(key))
                .saturating_add(duckvalue_output_bytes(value))
                .saturating_add(2)
        }),
        DuckValue::Union(value) => duckvalue_output_bytes(value),
        _ => 48,
    }
}

/// Format DuckDB results as JSON (array of objects).
fn format_as_json(columns: &[String], rows: &[Vec<DuckValue>]) -> String {
    let json_rows: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in columns.iter().enumerate() {
                let val = row
                    .get(i)
                    .map(duckvalue_to_json)
                    .unwrap_or(serde_json::Value::Null);
                obj.insert(col.clone(), val);
            }
            serde_json::Value::Object(obj)
        })
        .collect();

    serde_json::to_string_pretty(&json_rows).unwrap_or_else(|_| "[]".to_string())
}

/// Format DuckDB results as CSV (RFC 4180 compliant).
fn format_as_csv(columns: &[String], rows: &[Vec<DuckValue>]) -> String {
    let mut output = columns
        .iter()
        .map(|c| csv_escape(c))
        .collect::<Vec<_>>()
        .join(",");
    output.push('\n');
    for row in rows {
        let vals: Vec<String> = row
            .iter()
            .map(|v| csv_escape(&duckvalue_to_string(v)))
            .collect();
        output.push_str(&vals.join(","));
        output.push('\n');
    }
    output
}

/// RFC 4180: quote a field if it contains comma, double-quote, or newline.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Format DuckDB results as a text table.
fn format_as_table(columns: &[String], rows: &[Vec<DuckValue>]) -> String {
    // Calculate column widths using char count (safe for multi-byte UTF-8)
    let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                let len = duckvalue_to_string(val).chars().count();
                if len > widths[i] {
                    widths[i] = len;
                }
            }
        }
    }

    // Cap column widths at 40 chars
    for w in &mut widths {
        if *w > 40 {
            *w = 40;
        }
    }

    let mut output = String::new();

    // Header
    let header: Vec<String> = columns
        .iter()
        .zip(&widths)
        .map(|(c, w)| format!("{:width$}", c, width = *w))
        .collect();
    output.push_str(&header.join(" | "));
    output.push('\n');

    // Separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    output.push_str(&sep.join("-+-"));
    output.push('\n');

    // Rows
    for row in rows {
        let vals: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(v, w)| {
                let s = duckvalue_to_string(v);
                let char_count = s.chars().count();
                if char_count > *w && *w > 3 {
                    let truncated: String = s.chars().take(*w - 3).collect();
                    format!("{truncated}...")
                } else if char_count > *w {
                    s.chars().take(*w).collect()
                } else {
                    format!("{:width$}", s, width = *w)
                }
            })
            .collect();
        output.push_str(&vals.join(" | "));
        output.push('\n');
    }

    if rows.is_empty() {
        output.push_str("(0 rows)\n");
    } else {
        output.push_str(&format!("({} rows)\n", rows.len()));
    }

    output
}

/// Convert a DuckDB Value to a serde_json Value.
fn duckvalue_to_json(val: &DuckValue) -> serde_json::Value {
    match val {
        DuckValue::Null => serde_json::Value::Null,
        DuckValue::Boolean(b) => serde_json::Value::Bool(*b),
        DuckValue::TinyInt(n) => serde_json::json!(n),
        DuckValue::SmallInt(n) => serde_json::json!(n),
        DuckValue::Int(n) => serde_json::json!(n),
        DuckValue::BigInt(n) => serde_json::json!(n),
        DuckValue::HugeInt(n) => serde_json::json!(n.to_string()),
        DuckValue::UTinyInt(n) => serde_json::json!(n),
        DuckValue::USmallInt(n) => serde_json::json!(n),
        DuckValue::UInt(n) => serde_json::json!(n),
        DuckValue::UBigInt(n) => serde_json::json!(n),
        DuckValue::Float(f) => serde_json::json!(f),
        DuckValue::Double(f) => serde_json::json!(f),
        DuckValue::Text(s) => serde_json::Value::String(s.clone()),
        DuckValue::Blob(b) => serde_json::Value::String(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b,
        )),
        _ => serde_json::Value::String(format!("{:?}", val)),
    }
}

/// Convert a DuckDB Value to a display string.
fn duckvalue_to_string(val: &DuckValue) -> String {
    match val {
        DuckValue::Null => "NULL".to_string(),
        DuckValue::Boolean(b) => b.to_string(),
        DuckValue::TinyInt(n) => n.to_string(),
        DuckValue::SmallInt(n) => n.to_string(),
        DuckValue::Int(n) => n.to_string(),
        DuckValue::BigInt(n) => n.to_string(),
        DuckValue::HugeInt(n) => n.to_string(),
        DuckValue::UTinyInt(n) => n.to_string(),
        DuckValue::USmallInt(n) => n.to_string(),
        DuckValue::UInt(n) => n.to_string(),
        DuckValue::UBigInt(n) => n.to_string(),
        DuckValue::Float(f) => f.to_string(),
        DuckValue::Double(f) => f.to_string(),
        DuckValue::Text(s) => s.clone(),
        DuckValue::Blob(b) => format!("<blob {} bytes>", b.len()),
        _ => format!("{:?}", val),
    }
}

/// Strip leading SQL comments (both `--` line comments and `/* */` block comments)
/// so the `is_query` keyword detection sees the actual statement.
///
/// Known limitation: nested block comments (`/* outer /* inner */ still comment */`)
/// are not handled — the first `*/` terminates stripping, leaving residual comment
/// text visible to the keyword detector. DuckDB supports nested comments but LLMs
/// don't generate them in practice, so this is accepted as-is.
fn strip_leading_sql_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            // Line comment: skip to end of line
            match s.find('\n') {
                Some(pos) => s = s[pos + 1..].trim_start(),
                None => return "", // entire string is a comment
            }
        } else if s.starts_with("/*") {
            // Block comment: skip to closing */
            match s.find("*/") {
                Some(pos) => s = s[pos + 2..].trim_start(),
                None => return "", // unclosed block comment
            }
        } else {
            return s;
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use runtime_core::{FileSandboxConfig, FileSandboxMode};
    use tempfile::TempDir;

    // ========================================================================
    // Far-side idempotency — docs/components/magician/effect-identity.md
    // ========================================================================

    fn action_with_method(method: HttpMethod) -> HttpAction {
        HttpAction {
            method,
            ..HttpAction::get("https://example.invalid/resource")
        }
    }

    // ========================================================================
    // P4 Task 4.3 — authenticated dispatch delivers to the bound origin only
    // ========================================================================

    fn credential_post(url: &str, follow_redirects: bool, carries_credential: bool) -> HttpAction {
        HttpAction {
            method: HttpMethod::Post,
            url: url.to_string(),
            headers: HashMap::from([(
                "Authorization".to_string(),
                "Bearer p4-token-canary".to_string(),
            )]),
            body: Some(r#"{"code":"042917"}"#.to_string()),
            content_type: Some("application/json".to_string()),
            timeout_secs: Some(5),
            follow_redirects,
            carries_credential,
        }
    }

    #[tokio::test]
    async fn a_request_carrying_a_credential_never_follows_a_redirect() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let origin = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/landing", elsewhere.uri()).as_str()),
            )
            .mount(&origin)
            .await;
        Mock::given(method("GET"))
            .and(path("/landing"))
            .respond_with(ResponseTemplate::new(200).set_body_string("landed"))
            .mount(&elsewhere)
            .await;

        // The action asked to follow redirects; the credential mark overrides
        // it and the cross-origin hop ends the attempt with nothing forwarded.
        let error = execute_http_action(
            &credential_post(&format!("{}/login", origin.uri()), true, true),
            None,
        )
        .await
        .expect_err("a cross-origin redirect ends an authenticated attempt");
        assert!(error.to_string().contains("destination changed"), "{error}");
        assert!(!error.to_string().contains("p4-token-canary"));
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "the other origin saw nothing"
        );
        assert_eq!(
            origin.received_requests().await.unwrap().len(),
            1,
            "delivered once to the bound origin"
        );

        // The same request without the mark follows as the action asked.
        let result = execute_http_action(
            &credential_post(&format!("{}/login", origin.uri()), true, false),
            None,
        )
        .await
        .expect("an ordinary request follows its redirect");
        let ActionResult::Http { status, body, .. } = result else {
            panic!("http result")
        };
        assert_eq!(status, 200);
        assert_eq!(body, "landed");
    }

    #[tokio::test]
    async fn a_same_origin_redirect_is_returned_not_followed_for_a_credential() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let origin = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(ResponseTemplate::new(303).insert_header("Location", "/home"))
            .mount(&origin)
            .await;
        let result = execute_http_action(
            &credential_post(&format!("{}/login", origin.uri()), true, true),
            None,
        )
        .await
        .expect("a same-origin redirect is an ordinary observation");
        let ActionResult::Http {
            status, headers, ..
        } = result
        else {
            panic!("http result")
        };
        assert_eq!(status, 303);
        assert_eq!(headers.get("location").map(String::as_str), Some("/home"));
        assert_eq!(
            origin.received_requests().await.unwrap().len(),
            1,
            "not followed: the credential is sent once"
        );
    }

    // P4 Task 4.5 — the shell's only sink is stdin, on the streaming path too,
    // and what the process echoes is scrubbed before it streams or returns.
    #[tokio::test]
    async fn the_streaming_shell_path_feeds_stdin_and_scrubs_what_the_process_echoes() {
        let delivered = Arc::new(std::sync::Mutex::new(
            crate::magician_v2::secrets::KnownSecretValues::new(),
        ));
        delivered
            .lock()
            .unwrap()
            .insert("password".to_string(), "p4-stdin-canary".to_string());
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut events = broadcaster.subscribe();
        let ctx = ShellStreamContext {
            broadcaster,
            execution_id: "exec-stdin".into(),
            principal: None,
            workspace: None,
            base_step_id: "step".into(),
            step_id: "step".into(),
            step_index: 0,
            scrub: Some(Arc::clone(&delivered)),
        };
        let action = BashAction {
            command: "read pw; echo \"got:$pw\"; echo \"again:$pw\" >&2; exit 3".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(10),
            capture_output: true,
            stdin: Some("p4-stdin-canary\n".to_string()),
        };
        let error = execute_bash_action(&action, &ShellSandboxConfig::default(), Some(ctx), None)
            .await
            .expect_err("exit 3 fails the action");
        let text = error.to_string();
        assert!(
            text.contains("again:[REDACTED]"),
            "the process received its stdin and the echo is scrubbed: {text}"
        );
        assert!(!text.contains("p4-stdin-canary"), "{text}");
        // Every streamed chunk is scrubbed too.
        let mut streamed = String::new();
        while let Ok(event) = events.try_recv() {
            if let RuntimeTransportEvent::ShellOutputChunk { data, .. } = event {
                streamed.push_str(&data);
            }
        }
        assert!(streamed.contains("got:[REDACTED]"), "{streamed}");
        assert!(!streamed.contains("p4-stdin-canary"), "{streamed}");
    }

    #[test]
    fn a_request_that_cannot_be_built_is_a_proven_pre_dispatch_failure() {
        let mut action = credential_post("https://login.example.test/session", false, true);
        assert!(validate_http_request_buildable(&action).is_ok());
        action.headers.insert(
            "Authorization".to_string(),
            "Bearer bad\u{0}value".to_string(),
        );
        let reason = validate_http_request_buildable(&action)
            .expect_err("an invalid header value cannot be sent");
        assert!(reason.contains("cannot be built"), "{reason}");
        let mut action = credential_post("not a url", false, true);
        action.headers.clear();
        assert!(validate_http_request_buildable(&action).is_err());
    }

    #[test]
    fn only_requests_that_can_commit_carry_a_derived_key() {
        const EFFECT_ID: &str = "llm_call_abc:tool:toolu_01";

        for committing in [
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Delete,
        ] {
            assert!(
                derived_idempotency_key(&action_with_method(committing), Some(EFFECT_ID)).is_some(),
                "{committing:?} can commit a change, so a lost response is ambiguous"
            );
        }

        // A lost GET response costs nothing to re-issue, so a key buys the
        // caller nothing — and a per-attempt header on every read would be
        // noise to the remote and enough to defeat intermediary caching.
        for safe in [HttpMethod::Get, HttpMethod::Head, HttpMethod::Options] {
            assert!(
                derived_idempotency_key(&action_with_method(safe), Some(EFFECT_ID)).is_none(),
                "{safe:?} is safe to repeat and must not carry a key"
            );
        }
    }

    #[test]
    fn an_unattributable_dispatch_sends_no_key() {
        // No attempt behind the request means there is no honest key to send.
        // It does NOT mean the request is safe to repeat.
        assert!(derived_idempotency_key(&action_with_method(HttpMethod::Post), None).is_none());
    }

    #[test]
    fn a_key_that_arrived_in_the_action_does_not_outrank_the_runtime() {
        // An `http_post` step takes its headers from step parameters, and for
        // an agent tool call those parameters are the MODEL's. A model-authored
        // idempotency key is the one thing this header must never be: reuse one
        // across two different requests and a remote answers the second from
        // the first's cached response, losing an effect with no error raised.
        // Nothing at this layer can tell a model's header from a pack author's,
        // so the runtime value wins. HTTP field names are case-insensitive, so
        // no spelling escapes it.
        for spelling in ["Idempotency-Key", "idempotency-key", "IDEMPOTENCY-KEY"] {
            let mut action = action_with_method(HttpMethod::Post);
            action
                .headers
                .insert(spelling.to_string(), "supplied-in-the-action".to_string());

            let key = derived_idempotency_key(&action, Some("llm_call_abc:tool:toolu_01"))
                .expect("the runtime still derives its own key");
            assert_ne!(
                key, "supplied-in-the-action",
                "a key spelled {spelling} must not survive as the one we send"
            );
        }
    }

    #[test]
    fn an_unattributable_dispatch_leaves_the_actions_own_header_alone() {
        // With no attempt there is no runtime identity to substitute, and
        // stripping the header would break a pack that manages its own without
        // putting anything in its place. `execute_http_action` forwards
        // `action.headers` untouched whenever this returns `None`.
        let mut action = action_with_method(HttpMethod::Post);
        action
            .headers
            .insert("Idempotency-Key".to_string(), "pack-authored".to_string());

        assert!(derived_idempotency_key(&action, None).is_none());
    }

    #[test]
    fn the_key_sent_to_a_remote_is_derived_never_the_raw_effect_id() {
        let action = action_with_method(HttpMethod::Post);
        let key = derived_idempotency_key(&action, Some("llm_call_secret123:tool:toolu_01"))
            .expect("a POST with an attempt behind it carries a key");

        assert!(
            !key.contains("llm_call_secret123"),
            "the internal call id must not leave the process: {key}"
        );
        assert!(
            key.starts_with("mag-"),
            "expected a namespaced key, got {key}"
        );
    }

    #[test]
    fn standalone_duckdb_execution_respects_process_guard() {
        let session = DuckDbSession::new().expect("DuckDB session");
        let _guard = crate::magician_v2::analytics::duckdb_safety::analytics_duckdb_guard();
        let action = DuckDbAction {
            sql: "SELECT 1".to_string(),
            database: None,
            output_format: "json".to_string(),
            timeout_secs: Some(0),
        };

        let error = execute_duckdb_action(&action, &session).expect_err("capacity timeout");
        assert!(error.to_string().contains("waiting for analytics capacity"));
    }

    #[test]
    fn formatted_duckdb_output_is_bounded_after_encoding() {
        let output = finalize_duckdb_output("é".repeat(100), false, false, 128);

        assert!(output.len() <= 128);
        assert!(output.contains("Output truncated"));
        assert!(std::str::from_utf8(output.as_bytes()).is_ok());
    }

    #[test]
    fn test_normalize_duckdb_sql_paths_expands_sqlite_scan_path_literals() {
        let sql = "SELECT * FROM sqlite_scan('~/Library/Messages/chat.db', 'message')";
        let normalized =
            normalize_duckdb_sql_paths_with_context(sql, Some("/Users/owner"), Some("owner"));

        assert_eq!(
            normalized,
            "SELECT * FROM sqlite_scan('/Users/owner/Library/Messages/chat.db', 'message')"
        );
    }

    #[test]
    fn test_normalize_duckdb_sql_paths_expands_home_and_user_placeholders() {
        let sql = "SELECT CURRENT_USER, * FROM sqlite_scan('$HOME/Library/${CURRENT_USER}/chat.db', 'message')";
        let normalized =
            normalize_duckdb_sql_paths_with_context(sql, Some("/Users/owner"), Some("owner"));

        assert_eq!(
            normalized,
            "SELECT CURRENT_USER, * FROM sqlite_scan('/Users/owner/Library/owner/chat.db', 'message')"
        );
    }

    #[test]
    fn test_normalize_duckdb_sql_paths_leaves_non_sqlite_scan_sql_unchanged() {
        let sql = "SELECT CURRENT_USER, '~/Library/Messages/chat.db' AS path";
        let normalized =
            normalize_duckdb_sql_paths_with_context(sql, Some("/Users/owner"), Some("owner"));

        assert_eq!(normalized, sql);
    }

    #[test]
    fn test_glob_pattern_matching() {
        assert!(matches_glob_pattern("test.txt", "*.txt"));
        assert!(matches_glob_pattern("test.txt", "test.*"));
        assert!(matches_glob_pattern("test.txt", "t?st.txt"));
        assert!(matches_glob_pattern("test.txt", "*"));
        assert!(!matches_glob_pattern("test.txt", "*.rs"));
        assert!(!matches_glob_pattern("test.txt", "foo*"));
    }

    #[tokio::test]
    async fn test_file_write_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![temp_dir.path().display().to_string()],
            allow_delete: true,
        };

        // Write
        let write_action = FileAction::Write {
            path: file_path.clone(),
            content: "Hello, World!".to_string(),
            create_dirs: true,
        };
        let result = execute_file_action(&write_action, &sandbox).await.unwrap();
        assert!(result.is_success());

        // Read
        let read_action = FileAction::Read {
            path: file_path,
            encoding: None,
        };
        let result = execute_file_action(&read_action, &sandbox).await.unwrap();
        assert_eq!(result.as_text(), Some("Hello, World!"));
    }

    #[test]
    fn file_action_reports_only_paths_outside_allowed_roots() {
        let allowed = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![allowed.path().display().to_string()],
            allow_delete: true,
        };
        let action = FileAction::Copy {
            source: allowed.path().join("source.txt"),
            destination: outside.path().join("destination.txt"),
        };

        let violations = file_action_outside_allowed_roots(&action, &sandbox).unwrap();

        assert_eq!(
            violations,
            vec![
                resolve_path_for_policy(&outside.path().join("destination.txt"))
                    .unwrap()
                    .display()
                    .to_string()
            ]
        );
    }

    #[tokio::test]
    async fn runtime_store_fence_denies_agent_writes_to_trusted_stores() {
        // The trusted runtime stores (transactions / code_change_proposals /
        // runtime/pause_states) sit under the agent-writable `scopes` tree; the
        // deny-fence must keep agent file writes out of them so a staged
        // transaction's trusted `apply_root` cannot be forged. Covers the nested
        // `runtime/pause_states` depth that a flat 3rd-component check missed.
        let base = TempDir::new().unwrap();
        let scopes_root = base.path().join("scopes");
        let scope = scopes_root.join("anonymous").join("default");
        for sub in [
            "transactions",
            "code_change_proposals",
            "runtime/pause_states",
            "workdirs/home",
        ] {
            std::fs::create_dir_all(scope.join(sub)).unwrap();
        }
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![scopes_root.display().to_string()],
            allow_delete: true,
        };
        let write = |path: PathBuf| FileAction::Write {
            path,
            content: "x".to_string(),
            create_dirs: true,
        };

        for denied in [
            "transactions/t.json",
            "code_change_proposals/c.json",
            "runtime/pause_states/p.json",
        ] {
            let action = write(scope.join(denied));
            assert!(
                execute_file_action(&action, &sandbox).await.is_err(),
                "runtime-store fence must deny agent write to `{denied}`"
            );
        }

        // A legitimate in-workspace write under the same scope stays allowed.
        let allowed = write(scope.join("workdirs/home/file.txt"));
        assert!(
            execute_file_action(&allowed, &sandbox).await.is_ok(),
            "in-workspace write must not be fenced"
        );
    }

    #[tokio::test]
    async fn test_file_exists() {
        let temp_dir = TempDir::new().unwrap();
        let existing = temp_dir.path().join("exists.txt");
        let non_existing = temp_dir.path().join("not_exists.txt");
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![temp_dir.path().display().to_string()],
            allow_delete: true,
        };

        // Create file
        std::fs::write(&existing, "test").unwrap();

        // Check existing
        let action = FileAction::Exists { path: existing };
        let result = execute_file_action(&action, &sandbox).await.unwrap();
        if let ActionResult::Bool { value } = result {
            assert!(value);
        } else {
            panic!("Expected Bool result");
        }

        // Check non-existing
        let action = FileAction::Exists { path: non_existing };
        let result = execute_file_action(&action, &sandbox).await.unwrap();
        if let ActionResult::Bool { value } = result {
            assert!(!value);
        } else {
            panic!("Expected Bool result");
        }
    }

    #[tokio::test]
    async fn test_file_sandbox_blocks_paths_outside_allowed_roots() {
        let temp_dir = TempDir::new().unwrap();
        let sandbox = FileSandboxConfig {
            mode: FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![temp_dir.path().display().to_string()],
            allow_delete: true,
        };

        let action = FileAction::Read {
            path: PathBuf::from("/etc/hosts"),
            encoding: None,
        };

        let result = execute_file_action(&action, &sandbox).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let action = BashAction {
            command: "echo 'Hello from bash'".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None)
            .await
            .unwrap();
        let text = result.as_text().unwrap();
        assert!(text.contains("Hello from bash"));
    }

    fn executable_shell_in(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let shell = dir.join("sh");
        std::fs::write(&shell, "#!/bin/sh\nexit 0\n").expect("script");
        let mut perms = std::fs::metadata(&shell).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&shell, perms).expect("chmod");
        shell
    }

    /// A bash step's env is model-supplied. When it carries `PATH`, the
    /// shell must reach the OS as the absolute file on THAT path — left
    /// bare, std would `fork` to search it in the child (see
    /// `runtime_core::process`) — while the env itself reaches the child
    /// untouched.
    #[test]
    fn bash_shell_is_resolved_against_the_step_env_path() {
        use std::ffi::OsStr;
        let temp = TempDir::new().unwrap();
        let shell = executable_shell_in(temp.path());
        let action = BashAction::new("true").with_env("PATH", temp.path().display().to_string());

        let cmd = build_bash_command(&action).expect("build");
        let std_cmd = cmd.as_std();

        assert_eq!(std_cmd.get_program(), shell.as_os_str());
        let child_path = std_cmd
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("the model's PATH is still set on the child");
        assert_eq!(child_path, temp.path().as_os_str());
    }

    /// Without a step-supplied `PATH` the shell resolves on the process PATH
    /// and is still handed over as an absolute file.
    #[test]
    fn bash_shell_is_absolute_on_the_process_path_without_a_step_env_path() {
        use std::ffi::OsStr;
        let action = BashAction::new("true");
        let cmd = build_bash_command(&action).expect("build");
        let program = PathBuf::from(cmd.as_std().get_program());
        assert!(program.is_absolute(), "{}", program.display());
        assert!(program.is_file(), "{}", program.display());
        assert_eq!(program.file_name(), Some(OsStr::new("sh")));
        assert!(cmd
            .as_std()
            .get_envs()
            .all(|(key, _)| key != OsStr::new("PATH")));
    }

    #[tokio::test]
    async fn test_bash_stdin_writes_payload_and_closes() {
        // `cat` reads stdin until EOF and echoes it back. With the
        // stdin payload set, we should see it on stdout (proves the
        // bytes were written) and the process should exit (proves
        // stdin was closed).
        let action = BashAction {
            command: "cat".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: Some("hello from stdin\n".to_string()),
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None)
            .await
            .unwrap();
        let text = result.as_text().unwrap();
        assert!(
            text.contains("hello from stdin"),
            "expected stdin echo, got: {text}"
        );
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let action = BashAction {
            command: "exit 1".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_bash_timeout_enforced() {
        let action = BashAction {
            command: "sleep 2".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(1),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None).await;
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("timed out"));
    }

    #[tokio::test]
    async fn test_shell_sandbox_blocks_subshell_escape() {
        let action = BashAction {
            command: "echo $(whoami)".to_string(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(5),
            capture_output: true,
            stdin: None,
        };

        let result = execute_bash_action(&action, &ShellSandboxConfig::default(), None, None).await;
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("disallowed token"));
    }

    #[test]
    fn test_split_pipeline_respects_quotes() {
        // Simple pipe
        assert_eq!(
            split_pipeline_segments("ls | grep foo"),
            vec!["ls", "grep foo"]
        );

        // Pipe inside double quotes — must NOT split
        assert_eq!(
            split_pipeline_segments(r#"grep -i "pdf\|invoice\|go" file.txt"#),
            vec![r#"grep -i "pdf\|invoice\|go" file.txt"#]
        );

        // Mixed: real pipe + quoted pipe
        assert_eq!(
            split_pipeline_segments(r#"ls -la ~/Downloads/ | grep -i "pdf\|invoice""#),
            vec!["ls -la ~/Downloads/", r#"grep -i "pdf\|invoice""#]
        );

        // Pipe inside single quotes
        assert_eq!(
            split_pipeline_segments("echo 'a|b' | cat"),
            vec!["echo 'a|b'", "cat"]
        );

        // Backslash-escaped pipe outside quotes
        assert_eq!(
            split_pipeline_segments(r"echo hello\|world"),
            vec![r"echo hello\|world"]
        );
    }

    #[test]
    fn test_strip_heredoc_body() {
        // No heredoc — unchanged
        assert_eq!(strip_heredoc_body("ls -la"), "ls -la");

        // Heredoc with body — only command line kept
        let cmd = "osascript << 'EOF'\n-- Check for dialog\ntell app \"Finder\"\nEOF";
        assert_eq!(strip_heredoc_body(cmd), "osascript << 'EOF'");

        // Heredoc with pipe in body should not affect pipeline splitting
        let cmd = "cat << EOF\nsome|data\nEOF";
        let stripped = strip_heredoc_body(cmd);
        assert_eq!(stripped, "cat << EOF");
        assert_eq!(split_pipeline_segments(&stripped), vec!["cat << EOF"]);

        // Command with pipe BEFORE heredoc
        let cmd = "generate_data | osascript << 'EOF'\nscript body\nEOF";
        let stripped = strip_heredoc_body(cmd);
        assert_eq!(
            split_pipeline_segments(&stripped),
            vec!["generate_data", "osascript << 'EOF'"]
        );
    }

    #[test]
    fn html_http_body_is_stripped_and_capped_for_the_model() {
        let html = "<html><head><style>p{color:red}</style></head><body><script>alert(1)</script><h1>Instinct AI</h1><p>Raises more money.</p></body></html>";
        let text = visible_http_body(Some("text/html; charset=utf-8"), html.to_string());
        assert!(text.contains("Instinct AI"));
        assert!(text.contains("Raises more money."));
        assert!(!text.contains("alert"));
        assert!(!text.contains("<h1>"));
        let huge = format!("<p>{}</p>", "x".repeat(20_000));
        let capped = visible_http_body(Some("text/html"), huge);
        assert!(capped.contains("[truncated"));
        assert!(capped.len() < 20_000);
    }
}
