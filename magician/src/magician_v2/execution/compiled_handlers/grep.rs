//! `grep` — content search via ripgrep.
//!
//! Shells out to `rg` with output-mode-aware flags. Returns matching
//! lines / file paths / per-file counts depending on `output_mode`.
//!
//! The search root is checked like a `read_file` of it (the file sandbox,
//! the goal workspace and notes-index fences), `rg` runs inside the shell's
//! OS box when one works here, and every path it prints is checked again:
//! a file the acting agent may not read is dropped from the results.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::execution::goal_workspace_access as access;

const DEFAULT_HEAD_LIMIT: u64 = 250;

/// The file sandbox a search reads under: the same one `read_file` uses
/// (the boot sandbox, the roots the owner approved this session, the task
/// workdir).
fn search_sandbox(resources: &AgentResources) -> runtime_core::FileSandboxConfig {
    let mut sandbox = resources.file_sandbox.clone();
    if let Some(roots) =
        crate::magician_v2::execution::compiled_dispatch::current_session_file_sandbox_roots()
    {
        if let Ok(guard) = roots.lock() {
            sandbox.extra_writable_roots.extend(guard.iter().cloned());
        }
    }
    crate::magician_v2::execution::native_executors::task_file_sandbox(&sandbox)
}

/// Check `root` as a read of it. A pure out-of-roots denial propagates (the
/// executor raises the sandbox-override question); any other refusal is a
/// tool error for the model.
pub(super) async fn check_search_root(
    resources: &AgentResources,
    root: &str,
) -> Result<Option<Value>, ExecutionError> {
    match crate::magician_v2::execution::native_executors::authorize_file_read_path(
        Path::new(root),
        &search_sandbox(resources),
    )
    .await
    {
        Ok(()) => Ok(None),
        Err(error @ ExecutionError::PathAccessDenied { .. }) => Err(error),
        Err(error) => Ok(Some(json!({
            "status": "error",
            "reason": error.to_string(),
        }))),
    }
}

/// `rg args`, inside the shell's OS box when it applies here (outside a
/// coding run, with a working launcher), so the kernel keeps it out of the
/// goal workspaces the acting agent may not reach and out of secret stores.
pub(super) fn ripgrep(args: &[OsString]) -> tokio::process::Command {
    let sandbox = runtime_core::ShellSandboxConfig {
        os_enforce: true,
        ..runtime_core::ShellSandboxConfig::default()
    };
    let mut command =
        match crate::magician_v2::execution::shell_box::wrap(OsStr::new("rg"), args, &sandbox) {
            Some(boxed) => {
                let mut command = tokio::process::Command::new(&boxed.program);
                command.args(&boxed.args);
                for name in &boxed.env_remove {
                    command.env_remove(name);
                }
                for (key, value) in &boxed.env {
                    command.env(key, value);
                }
                command
            },
            None => {
                let mut command = tokio::process::Command::new("rg");
                command.args(args);
                command
            },
        };
    command.kill_on_drop(true);
    command
}

/// Whether a path `rg` printed may be shown to the acting agent. `rg` does
/// not follow symlinks and its root was checked resolved, so the printed
/// path is where the file is.
pub(super) fn admit(printed: &str, cache: &mut HashMap<String, bool>) -> bool {
    if let Some(known) = cache.get(printed) {
        return *known;
    }
    let path = Path::new(printed);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let admitted = !access::is_hybrid_index_path(&absolute)
        && access::refusal_for_current(&absolute, access::Op::Read).is_none();
    if admitted {
        // A result from a goal workspace stays with it (the task's evidence).
        access::record_read(&absolute);
    }
    cache.insert(printed.to_string(), admitted);
    admitted
}

/// `rg --null` output with the paths the agent may not read removed,
/// rebuilt in `rg`'s ordinary form (`path:line:text`, `path-line-text`,
/// `path:count`, one path per line).
fn filter_output(stdout: &str, output_mode: &str) -> Vec<String> {
    let mut cache = HashMap::new();
    if output_mode == "files_with_matches" {
        return stdout
            .split('\0')
            .map(|path| path.trim_matches('\n'))
            .filter(|path| !path.is_empty() && admit(path, &mut cache))
            .map(str::to_string)
            .collect();
    }
    let mut lines: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let Some((path, rest)) = line.split_once('\0') else {
            // A single-file search prints no path; its root was checked.
            // `--` separates context groups.
            if line == "--" && lines.last().is_none_or(|last| last == "--") {
                continue;
            }
            lines.push(line.to_string());
            continue;
        };
        if !admit(path, &mut cache) {
            continue;
        }
        let separator = rest
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .chars()
            .next()
            .filter(|c| *c == '-' && output_mode == "content")
            .unwrap_or(':');
        lines.push(format!("{path}{separator}{rest}"));
    }
    while lines.last().is_some_and(|last| last == "--") {
        lines.pop();
    }
    lines
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "grep requires `pattern` (regex string).",
        }));
    };
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(".")
        .to_string();
    let glob = args.get("glob").and_then(Value::as_str);
    let output_mode = args
        .get("output_mode")
        .and_then(Value::as_str)
        .unwrap_or("files_with_matches");
    let case_insensitive = args
        .get("case_insensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let context = args.get("context").and_then(Value::as_u64);
    let head_limit = args
        .get("head_limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_HEAD_LIMIT);
    let type_filter = args.get("type").and_then(Value::as_str);

    if let Some(refused) = check_search_root(&resources, &path).await? {
        return Ok(refused);
    }

    // `--null` so every printed path can be checked unambiguously.
    let mut rg_args: Vec<OsString> = vec![OsString::from("--null")];
    match output_mode {
        "files_with_matches" => {
            rg_args.push("--files-with-matches".into());
        },
        "count" => {
            rg_args.push("--count".into());
        },
        "content" => {
            rg_args.push("-n".into());
            if let Some(c) = context {
                rg_args.push(format!("-C{c}").into());
            }
        },
        other => {
            return Ok(json!({
                "status": "error",
                "reason": format!(
                    "grep `output_mode` `{other}` is invalid. Use one of: content | files_with_matches | count."
                ),
            }));
        },
    }
    if case_insensitive {
        rg_args.push("-i".into());
    }
    if let Some(g) = glob {
        rg_args.push("--glob".into());
        rg_args.push(g.into());
    }
    if let Some(t) = type_filter {
        rg_args.push("--type".into());
        rg_args.push(t.into());
    }
    // `--` so a pattern starting with `-` stays a pattern.
    rg_args.push("--".into());
    rg_args.push(pattern.into());
    rg_args.push(path.clone().into());

    let output = match ripgrep(&rg_args).output().await {
        Ok(o) => o,
        Err(e) => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Failed to spawn ripgrep: {e}"),
            }));
        },
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut lines = filter_output(&stdout, output_mode);
    let total = lines.len();
    let truncated = (total as u64) > head_limit;
    if truncated {
        lines.truncate(head_limit as usize);
    }

    Ok(json!({
        "status": "ok",
        "pattern": pattern,
        "path": path,
        "output_mode": output_mode,
        "total_matches": total,
        "result_count": lines.len(),
        "results": lines,
        "truncated": truncated,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn god_mode_search_asks_for_global_files_and_reuses_the_chosen_folder() {
        use crate::magician_v2::execution::goal_workspace_access as access;
        use crate::magician_v2::god_mode::{self, GodSession};
        use crate::magician_v2::user_requests::service::{UserRequestService, UserResponse};
        use std::time::Duration;
        let _god = god_mode::test_scope(true);
        let _roots = access::install_test_roots(Vec::new());
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("magician-config.yaml");
        std::fs::write(&target, "harness:\n  paused: true\n").unwrap();
        let mut resources = access::test_resources(allowed.path());
        Arc::make_mut(&mut resources).file_sandbox = runtime_core::FileSandboxConfig {
            mode: runtime_core::FileSandboxMode::WorkspaceWrite,
            allowed_roots: vec![allowed.path().display().to_string()],
            ..Default::default()
        };
        let args = json!({"pattern": "paused", "path": target, "output_mode": "content"});
        assert!(matches!(
            handle(resources.clone(), args.clone()).await,
            Err(ExecutionError::PathAccessDenied { .. })
        ));
        let session =
            GodSession::for_chat("owner", "default", "global-config-search-test").unwrap();
        let service = Arc::new(UserRequestService::new(Arc::new(
            crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(64),
        )));
        god_mode::with_test_approver(service.clone(), async {
            for decision in ["deny", "allow_once", "deny", "session:folder"] {
                let answer = async {
                    let request = tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let Some(request) = service.list_pending().await.into_iter().next() {
                                break request;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("search must offer the same God-mode read approval as read_file");
                    for id in ["allow_once", "session:file", "always:file", "deny"] {
                        assert!(request.options.iter().any(|option| option.id == id));
                    }
                    assert!(
                        service
                            .respond(UserResponse {
                                request_id: request.id,
                                decision: decision.into(),
                                input: None,
                                channel: "web".into(),
                                sensitive: Vec::new()
                            })
                            .await
                    );
                };
                let (result, ()) = tokio::join!(
                    god_mode::with_session(
                        Some(session.clone()),
                        handle(resources.clone(), args.clone())
                    ),
                    answer
                );
                let result = result.unwrap();
                if decision == "deny" {
                    assert_eq!(result["status"], "error");
                    assert!(!result.to_string().contains("paused: true"));
                } else {
                    assert_eq!(result["status"], "ok", "{result}");
                    assert!(result["results"].to_string().contains("paused: true"));
                }
            }
            let repeat = tokio::time::timeout(
                Duration::from_secs(2),
                god_mode::with_session(
                    Some(session.clone()),
                    handle(resources.clone(), args.clone()),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(repeat["status"], "ok");
            let found = tokio::time::timeout(
                Duration::from_secs(2),
                god_mode::with_session(
                    Some(session),
                    super::super::glob::handle(
                        resources,
                        json!({"pattern": "*.yaml", "path": outside.path()}),
                    ),
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(found["num_files"], 1, "{found}");
            assert!(service.list_pending().await.is_empty());
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                "harness:\n  paused: true\n"
            );
        })
        .await;
    }

    #[test]
    fn output_paths_an_agent_may_not_read_are_dropped() {
        let fx = crate::magician_v2::execution::goal_workspace_access::test_fixture();
        let ws = fx.root.join("Magican");
        let goal = ws.join("Goals/X/x.md").display().to_string();
        let outside = fx
            .root
            .parent()
            .unwrap()
            .join("open.md")
            .display()
            .to_string();
        let stdout = format!("{goal}\01:hit\n--\n{outside}\01:hit\n{outside}\02-ctx\n");
        crate::magician_v2::execution::goal_workspace_access::with_test_roots(
            vec![fx.root.clone()],
            || {
                let outsider = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("ambassador".to_string()), || {
                        filter_output(&stdout, "content")
                    });
                assert_eq!(
                    outsider,
                    vec![format!("{outside}:1:hit"), format!("{outside}-2-ctx")]
                );
                let member = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("cmo".to_string()), || {
                        filter_output(&stdout, "content")
                    });
                assert_eq!(member.len(), 4);
                let files = format!("{goal}\0{outside}\0");
                let listed = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("ambassador".to_string()), || {
                        filter_output(&files, "files_with_matches")
                    });
                assert_eq!(listed, vec![outside.clone()]);
                let counts = format!("{goal}\03\n{outside}\01\n");
                let counted = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(None, || filter_output(&counts, "count"));
                assert_eq!(counted, vec![format!("{outside}:1")]);
            },
        );
    }

    #[test]
    fn a_search_root_in_a_workspace_the_agent_is_not_a_member_of_is_refused() {
        let fx = crate::magician_v2::execution::goal_workspace_access::test_fixture();
        let ws = fx.root.join("Magican");
        crate::magician_v2::execution::goal_workspace_access::with_test_roots(
            vec![fx.root.clone()],
            || {
                let sandbox = runtime_core::FileSandboxConfig {
                    mode: runtime_core::FileSandboxMode::Unrestricted,
                    ..runtime_core::FileSandboxConfig::default()
                };
                let refused = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("ambassador".to_string()), || {
                        crate::magician_v2::execution::native_executors::validate_file_read_path(
                            &ws.join("Goals"),
                            &sandbox,
                        )
                    });
                assert!(refused.is_err());
                let allowed = crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("cmo".to_string()), || {
                        crate::magician_v2::execution::native_executors::validate_file_read_path(
                            &ws.join("Goals"),
                            &sandbox,
                        )
                    });
                assert!(allowed.is_ok(), "{allowed:?}");
            },
        );
    }

    /// End to end with a real `rg`, where one is installed.
    #[tokio::test]
    async fn grep_never_returns_a_non_members_workspace_hit() {
        if std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let fx = crate::magician_v2::execution::goal_workspace_access::test_fixture();
        let parent = fx.root.parent().unwrap().to_path_buf();
        std::fs::write(parent.join("open.md"), "needle\n").unwrap();
        std::fs::write(
            fx.root.join("Magican/Goals/X/x.md"),
            "---\nmagician_kind: goal\nowner: cmo\nvisibility: internal\n---\nneedle\n",
        )
        .unwrap();
        let args: Vec<OsString> = ["--null", "--files-with-matches", "--", "needle"]
            .into_iter()
            .map(OsString::from)
            .chain(std::iter::once(parent.clone().into_os_string()))
            .collect();
        let output = tokio::process::Command::new("rg")
            .args(&args)
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let filtered = crate::magician_v2::execution::goal_workspace_access::with_test_roots(
            vec![fx.root.clone()],
            || {
                crate::magician_v2::execution::native_executors::ACTING_AGENT
                    .sync_scope(Some("ambassador".to_string()), || {
                        filter_output(&stdout, "files_with_matches")
                    })
            },
        );
        assert!(
            filtered.iter().all(|path| !path.contains("Workspaces")),
            "{filtered:?}"
        );
        assert!(
            filtered.iter().any(|path| path.ends_with("open.md")),
            "{filtered:?}"
        );
    }
}
