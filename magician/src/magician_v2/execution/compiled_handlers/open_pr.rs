use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

use serde_json::{json, Value};

use super::shared::{optional_string, require_scope_str};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::coding_engine::resolve_coding_repo_binding;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::secrets::{SecretAuditEvent, SecretRef, SecretStoreError};

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "open_pr")?;
    let workspace = require_scope_str(&args, "__workspace", "open_pr")?;
    let workspace_root = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);

    let title = require_scope_str(&args, "title", "open_pr")?;
    let body = require_scope_str(&args, "body", "open_pr")?;
    let destination_repo = optional_string(&args, "destination_repo");
    let base_branch = optional_string(&args, "base_branch");
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let repo_binding = match resolve_coding_repo_binding(
        &workspace_root,
        optional_string(&args, "repo_path")
            .or_else(|| optional_string(&args, "project_repo_path"))
            .as_deref(),
    ) {
        Ok(binding) => binding,
        Err(reason) => {
            return Ok(json!({
                "status": "error",
                "reason": reason,
            }));
        },
    };

    let real_repo = &repo_binding.real_path;
    let branch = {
        let repo_slug = optional_string(&args, "project_id")
            .or_else(|| {
                repo_binding
                    .repo_path
                    .split('/')
                    .last()
                    .filter(|value| !value.is_empty())
                    .map(std::string::ToString::to_string)
                    .or_else(|| {
                        real_repo
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(std::string::ToString::to_string)
                    })
            })
            .map(|value| sanitize_slug(value.trim()))
            .unwrap_or_else(|| "repo".to_string());

        let short = uuid::Uuid::new_v4().to_string();
        let short = short.split('-').next().unwrap_or("0000");
        format!("vibedev/{repo_slug}-{short}")
    };

    // Fallbacks
    let mut resolved_dest = destination_repo.clone();
    if resolved_dest.is_none() || resolved_dest.as_ref().unwrap().trim().is_empty() {
        if let Ok(output) = Command::new("git")
            .arg("-C")
            .arg(real_repo)
            .args(&["remote", "get-url", "origin"])
            .output()
            .await
        {
            if output.status.success() {
                resolved_dest = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
            }
        }
    }
    let resolved_dest = resolved_dest.unwrap_or_else(|| "origin".to_string());

    let mut resolved_base = base_branch.clone();
    if resolved_base.is_none() || resolved_base.as_ref().unwrap().trim().is_empty() {
        if let Ok(output) = Command::new("git")
            .arg("-C")
            .arg(real_repo)
            .args(&["remote", "show", "origin"])
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if line.trim().starts_with("HEAD branch:") {
                        if let Some(raw_branch) = line.splitn(2, ':').nth(1) {
                            resolved_base = Some(raw_branch.trim().to_string());
                        }
                        break;
                    }
                }
            }
        }
    }
    let resolved_base = resolved_base.unwrap_or_else(|| "main".to_string());

    // Execute dry run
    if dry_run {
        return Ok(json!({
            "status": "ok",
            "branch": branch,
            "dry_run": true,
            "commands": [
                format!("git checkout -b {}", branch),
                "git add -A".to_string(),
                format!("git commit -m \"{}\"", title),
                format!("git push -u {} {}", resolved_dest, branch),
                format!("gh pr create --base {} --title \"{}\" --body-file -", resolved_base, title),
            ]
        }));
    }

    // Actual execution
    let github_token = resolve_github_token(&resources, &principal, &workspace)?;
    macro_rules! run_cmd {
        ($program:expr, $args:expr, $cwd:expr, $stdin_val:expr) => {{
            use tokio::io::AsyncWriteExt;
            let mut cmd = Command::new($program);
            cmd.args($args).current_dir($cwd);

            if let Some(token) = &github_token {
                cmd.env("GITHUB_TOKEN", token);
            }

            if let Some(val) = $stdin_val {
                cmd.stdin(Stdio::piped());
                cmd.stdout(Stdio::piped());
                cmd.stderr(Stdio::piped());
                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => return Ok(json!({ "status": "error", "reason": format!("failed to spawn {}: {}", $program, e) })),
                };
                if let Some(mut stdin) = child.stdin.take() {
                    let val_bytes = val.as_bytes().to_vec();
                    if let Err(error) = stdin.write_all(&val_bytes).await {
                        let _ = child.kill();
                        return Ok(json!({
                            "status": "error",
                            "reason": format!("failed to feed stdin for {}: {}", $program, error)
                        }));
                    }
                    let _ = stdin.shutdown().await;
                }
                match child.wait_with_output().await {
                    Ok(out) => out,
                    Err(e) => return Ok(json!({ "status": "error", "reason": format!("failed to wait on {}: {}", $program, e) })),
                }
            } else {
                match cmd.output().await {
                    Ok(out) => out,
                    Err(e) => return Ok(json!({ "status": "error", "reason": format!("failed to spawn {}: {}", $program, e) })),
                }
            }
        }}
    }

    let checkout = run_cmd!("git", &["checkout", "-b", &branch], real_repo, None::<&str>);
    if !checkout.status.success() {
        return Ok(
            json!({ "status": "error", "reason": format!("git checkout failed: {}", String::from_utf8_lossy(&checkout.stderr)) }),
        );
    }

    let add = run_cmd!("git", &["add", "-A"], real_repo, None::<&str>);
    if !add.status.success() {
        return Ok(
            json!({ "status": "error", "reason": format!("git add failed: {}", String::from_utf8_lossy(&add.stderr)) }),
        );
    }

    // G19 Secret Scan on staged changes
    let diff = run_cmd!("git", &["diff", "--cached"], real_repo, None::<&str>);
    if diff.status.success() {
        let diff_text = String::from_utf8_lossy(&diff.stdout);
        if let Some(_) =
            crate::magician_v2::agents::memory_consolidator::redact_obvious_secrets(&diff_text)
        {
            // Restore index to avoid leaving secrets staged if we abort
            let _ = run_cmd!("git", &["reset"], real_repo, None::<&str>);
            return Ok(json!({
                "status": "error",
                "reason": "G19 Secret Scan failed: High-confidence secrets were detected in the staged changes. Remove the secrets from the codebase before opening a PR."
            }));
        }
    }

    let commit = run_cmd!("git", &["commit", "-m", &title], real_repo, None::<&str>);
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        let stdout = String::from_utf8_lossy(&commit.stdout);
        let combined = format!("{stderr}{stdout}");
        if combined.contains("nothing to commit") || combined.contains("nothing added to commit") {
            return Ok(json!({
                "status": "error",
                "reason": "No staged changes found. Run apply_code_proposal before opening PR so there is content to commit."
            }));
        }
        return Ok(json!({
            "status": "error",
            "reason": format!("git commit failed: {}", combined)
        }));
    }

    let push = run_cmd!(
        "git",
        &["push", "-u", &resolved_dest, &branch],
        real_repo,
        None::<&str>
    );
    if !push.status.success() {
        return Ok(
            json!({ "status": "error", "reason": format!("git push failed: {}", String::from_utf8_lossy(&push.stderr)) }),
        );
    }

    let pr = run_cmd!(
        "gh",
        &[
            "pr",
            "create",
            "--base",
            &resolved_base,
            "--title",
            &title,
            "--body-file",
            "-"
        ],
        real_repo,
        Some(body.as_str())
    );
    if !pr.status.success() {
        return Ok(
            json!({ "status": "error", "reason": format!("gh pr create failed: {}", String::from_utf8_lossy(&pr.stderr)) }),
        );
    }

    let pr_url = String::from_utf8_lossy(&pr.stdout).trim().to_string();

    Ok(json!({
        "status": "ok",
        "pr_url": pr_url,
        "branch": branch,
    }))
}

fn sanitize_slug(raw: &str) -> String {
    let slug = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches('-').to_string();
    let slug = slug
        .chars()
        .take(32)
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "repo".to_string()
    } else {
        slug
    }
}

fn resolve_github_token(
    resources: &AgentResources,
    principal: &str,
    workspace: &str,
) -> Result<Option<String>, ExecutionError> {
    if let Some(resolver) = resources.secret_store_resolver.as_ref() {
        let store = resolver
            .resolve_for_scope(principal, workspace)
            .map_err(|error| {
                ExecutionError::Step(format!(
                    "Could not open scoped secret store for {principal}/{workspace}: {error}"
                ))
            })?;
        store.audit_event(
            SecretAuditEvent::new("open_pr_secret_resolution_attempt")
                .with_secret_id("GITHUB_TOKEN")
                .with_tool("open_pr")
                .with_action("gh"),
        );

        let grant = match store.issue_grant("GITHUB_TOKEN", "open_pr", "gh", None, Some(60)) {
            Ok(SecretRef::Grant(token)) => token,
            Ok(other) => {
                return Err(ExecutionError::Step(format!(
                    "Secret broker returned unsupported reference while resolving `GITHUB_TOKEN`: {other:?}"
                )));
            },
            Err(SecretStoreError::SecretNotFound(_)) => return Ok(None),
            Err(SecretStoreError::PolicyDenied(reason)) => {
                store.audit_event(
                    SecretAuditEvent::new("open_pr_secret_access_denied")
                        .with_secret_id("GITHUB_TOKEN")
                        .with_tool("open_pr")
                        .with_action("gh")
                        .with_detail(reason.clone()),
                );
                return Err(ExecutionError::Step(format!(
                    "Provisioned `GITHUB_TOKEN` denied access for open_pr:gh: {reason}"
                )));
            },
            Err(SecretStoreError::ApprovalRequired(challenge)) => {
                store.audit_event(
                    SecretAuditEvent::new("open_pr_secret_approval_needed")
                        .with_secret_id("GITHUB_TOKEN")
                        .with_tool("open_pr")
                        .with_action("gh")
                        .with_challenge_id(challenge.clone()),
                );
                return Err(ExecutionError::Step(format!(
                    "Provisioned `GITHUB_TOKEN` requires approval challenge `{challenge}`"
                )));
            },
            Err(error) => {
                store.audit_event(
                    SecretAuditEvent::new("open_pr_secret_resolution_failed")
                        .with_secret_id("GITHUB_TOKEN")
                        .with_tool("open_pr")
                        .with_action("gh")
                        .with_detail(error.to_string()),
                );
                return Err(ExecutionError::Step(format!(
                    "Failed to resolve `GITHUB_TOKEN` for open_pr: {error}"
                )));
            },
        };

        let redemption = store.redeem_grant(&grant).map_err(|error| {
            store.audit_event(
                SecretAuditEvent::new("open_pr_secret_grant_redeem_failed")
                    .with_secret_id("GITHUB_TOKEN")
                    .with_tool("open_pr")
                    .with_action("gh")
                    .with_detail(error.to_string()),
            );
            ExecutionError::Step(format!(
                "Could not redeem provisioned secret grant for `GITHUB_TOKEN`: {error}"
            ))
        })?;

        let value = redemption
            .fields()
            .get("GITHUB_TOKEN")
            .or_else(|| redemption.fields().get("value"))
            .or_else(|| redemption.fields().get("api_key"))
            .or_else(|| redemption.fields().get("apiKey"))
            .or_else(|| redemption.fields().get("token"))
            .or_else(|| redemption.fields().get("key"))
            .or_else(|| redemption.fields().get("secret"))
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        let value = match value {
            Some(value) => value,
            None => {
                return Err(ExecutionError::Step(
                    "Provisioned `GITHUB_TOKEN` has no usable token field. Expected one of: `GITHUB_TOKEN`, `value`, `api_key`, `apiKey`, `token`, `key`, or `secret`."
                        .to_string(),
                ));
            },
        };

        let _ = store.record_usage(redemption.secret_id(), None);
        store.audit_event(
            SecretAuditEvent::new("open_pr_secret_injected")
                .with_secret_id(redemption.secret_id().to_string())
                .with_tool("open_pr")
                .with_action("gh"),
        );
        return Ok(Some(value));
    }

    Ok(std::env::var("GITHUB_TOKEN")
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|value: &&str| !value.is_empty())
        .map(ToString::to_string))
}
