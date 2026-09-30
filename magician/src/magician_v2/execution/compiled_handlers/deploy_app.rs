use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;

use serde_json::{json, Value};

use super::shared::{optional_string, require_scope_str};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::coding_engine::{
    os_sandbox_command, resolve_coding_repo_binding,
};
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::media_seam::{detect_deploy_target, DeployTargetInfo};
use crate::magician_v2::secrets::{SecretAuditEvent, SecretRef, SecretStoreError};

const DEPLOY_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

fn resolve_deploy_secret(
    resources: &AgentResources,
    principal: &str,
    workspace: &str,
    secret_id: &str,
) -> Result<Option<String>, String> {
    let Some(resolver) = resources.secret_store_resolver.as_ref() else {
        return Ok(std::env::var(secret_id)
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|value: &&str| !value.is_empty())
            .map(ToString::to_string));
    };
    let store = resolver
        .resolve_for_scope(principal, workspace)
        .map_err(|error| {
            format!("Could not open scoped secret store for {principal}/{workspace}: {error}")
        })?;

    store.audit_event(
        SecretAuditEvent::new("deploy_secret_resolution_attempt")
            .with_secret_id(secret_id.to_string())
            .with_tool("deploy_app")
            .with_action("deploy"),
    );

    let grant = match store.issue_grant(secret_id, "deploy_app", "deploy", None, Some(60)) {
        Ok(SecretRef::Grant(token)) => token,
        Ok(other) => {
            return Err(format!(
                "Secret broker returned unsupported reference: {other:?}"
            ))
        },
        Err(SecretStoreError::SecretNotFound(_)) => return Ok(None),
        Err(SecretStoreError::PolicyDenied(reason)) => {
            store.audit_event(
                SecretAuditEvent::new("deploy_secret_access_denied")
                    .with_secret_id(secret_id.to_string())
                    .with_tool("deploy_app")
                    .with_action("deploy")
                    .with_detail(reason.clone()),
            );
            return Err(format!("Access denied: {reason}"));
        },
        Err(SecretStoreError::ApprovalRequired(challenge)) => {
            store.audit_event(
                SecretAuditEvent::new("deploy_secret_approval_needed")
                    .with_secret_id(secret_id.to_string())
                    .with_tool("deploy_app")
                    .with_action("deploy")
                    .with_challenge_id(challenge.clone()),
            );
            return Err(format!("Approval required: {challenge}"));
        },
        Err(error) => {
            store.audit_event(
                SecretAuditEvent::new("deploy_secret_resolution_failed")
                    .with_secret_id(secret_id.to_string())
                    .with_tool("deploy_app")
                    .with_action("deploy")
                    .with_detail(error.to_string()),
            );
            return Err(format!("Failed to resolve secret: {error}"));
        },
    };

    let redemption = store.redeem_grant(&grant).map_err(|error| {
        store.audit_event(
            SecretAuditEvent::new("deploy_secret_grant_redeem_failed")
                .with_secret_id(secret_id.to_string())
                .with_tool("deploy_app")
                .with_action("deploy")
                .with_detail(error.to_string()),
        );
        format!("Could not redeem secret grant for `{secret_id}`: {error}")
    })?;

    let value = redemption
        .fields()
        .get(secret_id)
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
            return Err("Provisioned secret has no usable token field. Expected one of: `value`, `api_key`, `apiKey`, `token`, `key`, or `secret`."
                .to_string());
        },
    };

    let _ = store.record_usage(redemption.secret_id(), None);
    store.audit_event(
        SecretAuditEvent::new("deploy_secret_injected")
            .with_secret_id(redemption.secret_id().to_string())
            .with_tool("deploy_app")
            .with_action("deploy"),
    );

    Ok(Some(value))
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(&args, "__principal", "deploy_app")?;
    let workspace = require_scope_str(&args, "__workspace", "deploy_app")?;
    let workspace_root = resources
        .artifact_workspace
        .capability_home_root(&principal, &workspace);

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

    let project_id = optional_string(&args, "project_id")
        .filter(|value| !value.trim().is_empty())
        .map(|value| sanitize_project_slug(value.as_str()))
        .unwrap_or_else(|| {
            repo_binding
                .repo_path
                .split('/')
                .last()
                .filter(|value| !value.is_empty())
                .map(|value| sanitize_project_slug(value))
                .unwrap_or_else(|| "project".to_string())
        });

    let real_repo = repo_binding.real_path.as_path();

    let deploy_target = match detect_deploy_target(real_repo) {
        Some(target) => target,
        None => {
            return Ok(json!({
                "status": "error",
                "reason": "Project is not a recognized web app or static site capable of deployment."
            }))
        },
    };

    let DeployTargetInfo {
        framework,
        build_command,
        output_dir,
    } = deploy_target;

    let (deploy_cmd_program, deploy_cmd_args, token_env_var) = match framework.as_str() {
        "static" | "astro" | "vite" => (
            "npx",
            vec![
                "wrangler",
                "pages",
                "deploy",
                &output_dir,
                "--project-name",
                &project_id,
                "--commit-dirty",
                "true",
            ],
            "CLOUDFLARE_API_TOKEN",
        ),
        "nextjs" | "sveltekit" => (
            "npx",
            vec!["vercel", "deploy", "--prod", "--yes", "--cwd", "."],
            "VERCEL_TOKEN",
        ),
        other => {
            return Ok(json!({
                "status": "error",
                "reason": format!("Unsupported deploy framework: {}", other)
            }))
        },
    };

    let mut commands = Vec::new();
    if let Some(bc) = &build_command {
        let mut args = vec![bc.program.clone()];
        args.extend(bc.args.clone());
        commands.push(args.join(" "));
    }

    let mut d_args = vec![deploy_cmd_program.to_string()];
    d_args.extend(deploy_cmd_args.iter().map(|s| s.to_string()));
    commands.push(d_args.join(" "));

    if dry_run {
        return Ok(json!({
            "status": "ok",
            "dry_run": true,
            "framework": framework,
            "commands": commands
        }));
    }

    let token = match resolve_deploy_secret(&resources, &principal, &workspace, token_env_var) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Ok(json!({
                "status": "needs_token",
                "platform": token_env_var.split('_').next().unwrap_or("").to_lowercase(),
                "reason": format!("Deployment requires {} in secret vault or environment", token_env_var)
            }))
        },
        Err(e) => return Ok(json!({ "status": "error", "reason": e })),
    };

    // Run build command
    if let Some(bc) = build_command {
        let sandbox_program = std::ffi::OsString::from(bc.program.as_str());
        let sandbox_args: Vec<std::ffi::OsString> = bc
            .args
            .iter()
            .map(|a| std::ffi::OsString::from(a.as_str()))
            .collect();
        let mut cmd = os_sandbox_command(sandbox_program.as_os_str(), &sandbox_args);

        cmd.current_dir(real_repo)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(
                    json!({ "status": "error", "reason": format!("Failed to spawn build: {e}") }),
                )
            },
        };

        match timeout(DEPLOY_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
                    combined.push_str(&String::from_utf8_lossy(&output.stderr));
                    return Ok(
                        json!({ "status": "error", "reason": format!("Build failed:\n{combined}") }),
                    );
                }
            },
            Ok(Err(e)) => {
                return Ok(json!({ "status": "error", "reason": format!("Build error: {e}") }))
            },
            Err(_) => return Ok(json!({ "status": "error", "reason": "Build timed out" })),
        }
    }

    // Run deploy command
    let sandbox_program = std::ffi::OsString::from(deploy_cmd_program);
    let sandbox_args: Vec<std::ffi::OsString> = deploy_cmd_args
        .iter()
        .map(|a| std::ffi::OsString::from(*a))
        .collect();
    let mut cmd = os_sandbox_command(sandbox_program.as_os_str(), &sandbox_args);

    cmd.current_dir(real_repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env(token_env_var, token)
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(
                json!({ "status": "error", "reason": format!("Failed to spawn deploy: {e}") }),
            )
        },
    };

    let output = match timeout(DEPLOY_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Ok(json!({ "status": "error", "reason": format!("Deploy error: {e}") }))
        },
        Err(_) => return Ok(json!({ "status": "error", "reason": "Deploy timed out" })),
    };

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return Ok(json!({ "status": "error", "reason": format!("Deploy failed:\n{combined}") }));
    }

    // Attempt to extract deploy URL
    let deploy_url = combined
        .lines()
        .find(|l| l.contains("https://") && (l.contains(".vercel.app") || l.contains(".pages.dev")))
        .and_then(|l| l.split_whitespace().find(|w| w.starts_with("https://")))
        .map(|s| s.to_string());

    Ok(json!({
        "status": "ok",
        "deploy_url": deploy_url,
        "logs": combined
    }))
}

fn sanitize_project_slug(raw: &str) -> String {
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
        .take(64)
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
}
