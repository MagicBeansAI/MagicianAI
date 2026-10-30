//! Scoped stdio launch and native-tool argument governance, shared by providers.
use super::{
    exec_ctx::PrimitiveExecCtx,
    governed_runtime::{
        bind_runtime_workspace_path, governed_executable_directory, ExecutableRole,
    },
    mcp_bindings,
};
use crate::magician_v2::{
    agents::outward_gate,
    artifact_v2::{workspace::ArtifactV2Workspace, CapabilityScopePaths},
    evidence::{OutwardAssertionStore, OutwardScope},
    execution::{capability::GovernedRuntimeImplementation, shell_box},
};
use chrono::Utc;
use magician_mcp_client::{McpClientConfig, McpTransportConfig, StdioTransportConfig};
use serde_json::Value;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};
use tool_runtime_core::manifest::{AuthKind, RuntimeProtocol};

pub(super) fn client_config(
    runtime: &GovernedRuntimeImplementation,
    ctx: &PrimitiveExecCtx,
    paths: &CapabilityScopePaths,
    executable: &str,
    args: &[String],
) -> Result<McpClientConfig, String> {
    if !matches!(
        runtime.package.contract.auth.kind,
        AuthKind::None | AuthKind::CliProfile
    ) {
        return Err(
            "stdio MCP supports scoped CLI-owned authentication or no authentication".into(),
        );
    }
    let root = super::governed_runtime::resolve_workspace_file_root(&paths.capabilities_root)?;
    let home = paths.home_root.clone();
    std::fs::create_dir_all(&home).map_err(|_| "MCP scoped home is unavailable")?;
    let home = std::fs::canonicalize(home).map_err(|_| "MCP scoped home is unavailable")?;
    if !home.starts_with(&root) {
        return Err("MCP scoped home escaped its workspace".into());
    }
    let mut dirs = Vec::new();
    for name in &runtime.package.contract.requires.bins {
        let dir = governed_executable_directory(
            ctx,
            runtime,
            paths,
            name,
            ExecutableRole::PathCompanion,
        )?;
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    let directory = governed_executable_directory(
        ctx,
        runtime,
        paths,
        executable,
        ExecutableRole::PathCompanion,
    )?;
    let program = std::fs::canonicalize(directory.join(executable))
        .map_err(|_| "MCP executable is unavailable")?;
    dirs.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    let path = std::env::join_paths(dirs).map_err(|_| "MCP PATH is invalid")?;
    let mut environment = BTreeMap::<OsString, OsString>::from([
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), path),
    ]);
    if let Some(bindings) = &runtime.mcp_bindings {
        for (key, value) in &bindings.environment {
            if matches!(
                key.as_str(),
                "HOME"
                    | "PATH"
                    | "LD_PRELOAD"
                    | "DYLD_INSERT_LIBRARIES"
                    | "NODE_OPTIONS"
                    | "PYTHONPATH"
            ) {
                return Err("MCP environment cannot override process execution authority".into());
            }
            environment.insert(
                key.into(),
                value.replace("{home}", &home.to_string_lossy()).into(),
            );
        }
    }
    let scope_root =
        std::fs::canonicalize(&paths.capabilities_root).map_err(|_| "MCP scope is unavailable")?;
    let cwd = ctx
        .governed_work_root
        .clone()
        .or_else(|| ctx.task_work_root.clone())
        .unwrap_or_else(|| root.clone());
    let cwd = std::fs::canonicalize(cwd).map_err(|_| "MCP workdir is unavailable")?;
    if !cwd.is_dir() || (ctx.governed_work_root.is_none() && !cwd.starts_with(&scope_root)) {
        return Err("MCP workdir escaped its workspace".into());
    }
    if let Some(refusal) = crate::magician_v2::execution::goal_workspace_access::refusal_for_current(
        &cwd,
        crate::magician_v2::execution::goal_workspace_access::Op::Read,
    ) {
        return Err(refusal);
    }
    let sandbox = runtime_core::ShellSandboxConfig {
        os_enforce: true,
        allowed_working_dirs: vec![
            cwd.to_string_lossy().into_owned(),
            home.to_string_lossy().into_owned(),
        ],
        ..Default::default()
    };
    let args: Vec<OsString> = args.iter().map(Into::into).collect();
    // No unboxed fallback, including callers in a coding context. The MCP child
    // must not inherit a harness's broader filesystem or credential authority.
    let boxed = shell_box::wrap_required(program.as_os_str(), &args, &sandbox)
        .ok_or("governed stdio MCP requires an available OS sandbox")?;
    for key in boxed.env_remove {
        environment.remove(&OsString::from(key));
    }
    for (key, value) in boxed.env {
        environment.insert(key.into(), value.into());
    }
    let transport = StdioTransportConfig::new(PathBuf::from(boxed.program))
        .with_args(boxed.args)
        .with_environment(environment)
        .with_working_directory(cwd);
    let mut config = McpClientConfig::new(McpTransportConfig::Stdio(transport));
    if let RuntimeProtocol::Mcp { limits, .. } = &runtime.package.contract.runtime {
        if let Some(timeout) = limits.timeout_secs {
            config.connect_timeout = std::time::Duration::from_secs(u64::from(timeout).min(120));
            config.request_timeout = std::time::Duration::from_secs(
                u64::from(timeout).min(super::MAX_MCP_REQUEST_TIMEOUT_SECS),
            );
        }
    }
    Ok(config)
}

pub(super) fn bind_paths(
    runtime: &GovernedRuntimeImplementation,
    ctx: &PrimitiveExecCtx,
    tool: &str,
    arguments: &mut Value,
) -> Result<(), String> {
    let Some(binding) = runtime
        .mcp_bindings
        .as_ref()
        .and_then(|b| b.tools.get(tool))
    else {
        if matches!(
            runtime.package.contract.runtime,
            RuntimeProtocol::Mcp {
                transport: tool_runtime_core::manifest::McpTransport::Stdio { .. },
                ..
            }
        ) {
            return Err("stdio MCP tool has no reviewed local argument bindings; update the skill policy before invoking it".into());
        }
        return Ok(());
    };
    if binding.paths.is_empty() {
        return Ok(());
    }
    let (Some(principal), Some(workspace)) = (ctx.principal.as_deref(), ctx.workspace.as_deref())
    else {
        return Err("MCP path binding requires scope".into());
    };
    let layout = ArtifactV2Workspace::new(ctx.storage_base_path.clone());
    let scope_root = layout.scope_root(principal, workspace);
    for (pointer, access) in &binding.paths {
        let Some(value) = arguments.pointer_mut(pointer) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let raw = value.as_str().ok_or("MCP path binding requires a string")?;
        let path = bind_runtime_workspace_path(&scope_root, ctx, raw, *access)?;
        if let Some(refusal) =
            crate::magician_v2::execution::goal_workspace_access::refusal_for_current(
                &path,
                if matches!(
                    access,
                    tool_runtime_core::action_overrides::WorkspacePathAccess::ReadFile
                ) {
                    crate::magician_v2::execution::goal_workspace_access::Op::Read
                } else {
                    crate::magician_v2::execution::goal_workspace_access::Op::Write
                },
            )
        {
            return Err(refusal);
        }
        *value = Value::String(path.to_string_lossy().into_owned());
    }
    Ok(())
}

pub(super) struct OutwardAttempt {
    store: OutwardAssertionStore,
    scope: OutwardScope,
    act_ref: String,
    binding: mcp_bindings::McpOutwardBinding,
    pub captured: bool,
}
impl OutwardAttempt {
    pub fn settle(self, result: &Value) {
        if result.get("remote_error").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let Some(id) = self
            .binding
            .receipt
            .as_deref()
            .and_then(|pointer| mcp_bindings::receipt_id(result, pointer))
        else {
            return;
        };
        if let Err(error) = self.store.record_provider_message(
            &self.scope,
            &self.act_ref,
            &self.binding.provider,
            &id,
            &Utc::now().to_rfc3339(),
        ) {
            // The operation has already left; a recording failure must never invite a retry.
            tracing::error!(%error, act_ref = %self.act_ref, "MCP receipt persistence failed after completion; do not retry");
        }
    }

    /// The request was handed to the transport but no result can be read back
    /// (a transport error, or a result the local limits refuse). The send may
    /// have happened, so the act stays an open question — recorded with why —
    /// and the caller must answer with a non-retry envelope, never an `Err`.
    pub fn settle_unknown(self, reason: &str) {
        if let Err(error) = self.store.mark_dispatch_unknown(
            &self.scope,
            &self.act_ref,
            &format!("MCP result unreadable after dispatch: {reason}"),
            &Utc::now().to_rfc3339(),
        ) {
            // Same rule as `settle`: the operation may have left, so a recording
            // failure must never invite a retry.
            tracing::error!(%error, act_ref = %self.act_ref, "MCP dispatch-unknown persistence failed after dispatch; do not retry");
        }
    }

    /// Refused locally before the request left. Nothing was sent, so the act is
    /// failed rather than left reading as a possible send.
    pub fn settle_not_sent(self, reason: &str) {
        if let Err(error) = self.store.mark_failed(
            &self.scope,
            &self.act_ref,
            &format!("MCP request not sent: {reason}"),
            &Utc::now().to_rfc3339(),
        ) {
            tracing::error!(%error, act_ref = %self.act_ref, "MCP not-sent persistence failed");
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn for_test(
        store: OutwardAssertionStore,
        scope: OutwardScope,
        act_ref: String,
        binding: mcp_bindings::McpOutwardBinding,
    ) -> Self {
        Self {
            store,
            scope,
            act_ref,
            binding,
            captured: false,
        }
    }
}

pub(super) async fn prepare_outward(
    runtime: &GovernedRuntimeImplementation,
    ctx: &PrimitiveExecCtx,
    capability: &str,
    tool: &str,
    arguments: &Value,
) -> Result<Option<OutwardAttempt>, String> {
    let Some(binding) = runtime
        .mcp_bindings
        .as_ref()
        .and_then(|b| b.tools.get(tool))
        .and_then(|b| b.outward.as_ref())
    else {
        return Ok(None);
    };
    let recipients = mcp_bindings::recipients(binding, arguments)?;
    let (Some(principal), Some(workspace)) = (ctx.principal.as_deref(), ctx.workspace.as_deref())
    else {
        return Err("MCP outward action requires scope".into());
    };
    if let Some(carried) = &ctx.work_authority {
        carried.work.guard_id()?;
        if let crate::magician_v2::work_context::WorkContextKind::Engagement(id) = &carried.work {
            let store = crate::magician_v2::engagements::global_engagement_store()
                .ok_or("MCP work authority store is unavailable")?;
            let live = store
                .live_authority(principal, workspace, id, Utc::now().timestamp_millis())
                .await
                .map_err(|e| format!("MCP work authority was refused: {e:?}"))?;
            if !live
                .tool_ceiling
                .iter()
                .any(|name| name == capability || name == "*")
            {
                return Err("MCP capability is outside the live work authority ceiling".into());
            }
        }
    }
    let layout = ArtifactV2Workspace::new(ctx.storage_base_path.clone());
    if let Some(refusal) = outward_gate::contact_refusal(
        Some(&layout),
        Some(principal),
        Some(workspace),
        &recipients,
        if binding.audience_optional {
            outward_gate::Addressing::MayReachNobody
        } else {
            outward_gate::Addressing::RecipientRequired
        },
        Utc::now(),
    ) {
        return Err(refusal);
    }
    let store = OutwardAssertionStore::new(layout.clone()).with_effect_id(ctx.effect_id.clone());
    let scope = OutwardScope::new(principal, workspace);
    let now = Utc::now().to_rfc3339();
    let act = store
        .prepare_dispatch(
            &scope,
            capability,
            tool,
            binding.channel,
            ctx.agent_id.as_deref().unwrap_or("unknown"),
            &serde_json::to_vec(arguments).map_err(|e| e.to_string())?,
            recipients.clone(),
            ctx.work_authority.as_ref().map(|w| &w.work),
            binding.consequence_class,
            &now,
        )
        .map_err(|e| e.to_string())?;
    if crate::magician_v2::recipient_compliance::recipient_compliance_mode().blocks() {
        if let Some(refusal) = crate::magician_v2::recipient_compliance::compliance_refusal(
            Some(&layout),
            Some(principal),
            Some(workspace),
            &recipients,
            Some(binding.channel),
            ctx.work_authority.as_ref().map(|w| &w.work),
            Some(&act.outward_act_ref),
            &crate::magician_v2::recipient_compliance::CompliancePolicy::standard(),
            &crate::magician_v2::recipient_compliance::NoJurisdictionRule,
            Utc::now(),
        ) {
            let _ = store.mark_failed(&scope, &act.outward_act_ref, &refusal, &now);
            return Err(refusal);
        }
    }
    let captured = match crate::magician_v2::agents::outward_policy::capture_for_task(
        &layout,
        Some(principal),
        Some(workspace),
        ctx.task_id.as_deref(),
    )
    .await
    {
        Ok(captured) => captured,
        Err(error) => {
            let _ = store.mark_failed(&scope, &act.outward_act_ref, &error, &now);
            return Err(error);
        },
    };
    if !captured {
        if binding.tracks_delivery {
            outward_gate::DispatchLog::new(layout)
                .record(
                    &crate::magician_v2::delivery::DeliveryScope::new(principal, workspace),
                    &act.outward_act_ref,
                    Utc::now(),
                )
                .map_err(|e| e.to_string())?;
        }
        store
            .mark_dispatching(&scope, &act.outward_act_ref, &now)
            .map_err(|e| e.to_string())?;
        store
            .mark_dispatch_unknown(
                &scope,
                &act.outward_act_ref,
                "MCP dispatched; awaiting a provider receipt",
                &now,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(Some(OutwardAttempt {
        store,
        scope,
        act_ref: act.outward_act_ref,
        binding: binding.clone(),
        captured,
    }))
}

/// Promote only provider paths inside a reviewed scope output root or the
/// current task workdir. A server cannot publish a host/auth or sibling-task file.
pub(super) fn result_artifacts(
    runtime: &GovernedRuntimeImplementation,
    ctx: &PrimitiveExecCtx,
    tool: &str,
    result: &Value,
) -> Vec<super::artifacts::PrimitiveArtifact> {
    use std::path::{Component, Path};
    let mut artifacts = Vec::new();
    if result.get("remote_error").and_then(Value::as_bool) == Some(true) {
        return artifacts;
    }
    let Some(binding) = runtime
        .mcp_bindings
        .as_ref()
        .and_then(|bindings| bindings.tools.get(tool))
    else {
        return artifacts;
    };
    if binding.artifacts.is_empty() {
        return artifacts;
    }
    let (Some(principal), Some(workspace)) = (ctx.principal.as_deref(), ctx.workspace.as_deref())
    else {
        return artifacts;
    };
    let layout = ArtifactV2Workspace::new(&ctx.storage_base_path);
    let scope_root = layout.scope_root(principal, workspace);
    let Ok(workdirs) = super::governed_runtime::resolve_workspace_file_root(&scope_root) else {
        return artifacts;
    };
    let documents = mcp_bindings::native_documents(result);
    let mut seen = std::collections::BTreeSet::new();
    for binding in &binding.artifacts {
        if !Path::new(&binding.root)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        {
            continue;
        }
        let root = std::fs::canonicalize(workdirs.join(&binding.root))
            .ok()
            .filter(|root| root.starts_with(&workdirs));
        for document in &documents {
            let values: Vec<&Value> =
                if let Some((prefix, suffix)) = binding.pointer.split_once("/*/") {
                    document
                        .pointer(prefix)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|item| item.pointer(&format!("/{suffix}")))
                        .collect()
                } else {
                    document.pointer(&binding.pointer).into_iter().collect()
                };
            for value in values {
                let Some(raw) = value.as_str() else { continue };
                let candidate = Path::new(raw);
                let candidate = if candidate.is_absolute() {
                    candidate.to_owned()
                } else if let Some(root) = &root {
                    root.join(candidate)
                } else {
                    continue;
                };
                let Ok(path) = std::fs::canonicalize(candidate) else {
                    continue;
                };
                let scope_output = root.as_ref().is_some_and(|root| path.starts_with(root));
                let task_output = ctx.governed_work_root.is_none()
                    && ctx.task_work_root.as_ref().is_some_and(|root| {
                        super::super::native_executors::relative_within(&path, root).is_some()
                    })
                    && bind_runtime_workspace_path(
                        &scope_root,
                        ctx,
                        raw,
                        tool_runtime_core::action_overrides::WorkspacePathAccess::ReadFile,
                    )
                    .is_ok();
                if (!scope_output && !task_output) || !path.is_file() || !seen.insert(path.clone())
                {
                    continue;
                }
                if crate::magician_v2::execution::goal_workspace_access::refusal_for_current(
                    &path,
                    crate::magician_v2::execution::goal_workspace_access::Op::Read,
                )
                .is_some()
                {
                    continue;
                }
                artifacts.push(super::artifacts::PrimitiveArtifact::from_path(
                    "file",
                    tool,
                    ctx.effect_id.as_deref().unwrap_or("mcp-call"),
                    path,
                ));
            }
        }
    }
    artifacts
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn whatsapp_runtime() -> GovernedRuntimeImplementation {
        let package = tool_runtime_core::manifest_parser::parse_skill_runtime_package(
            include_str!("../../../../../skillshub/whatsapp-mcp/SKILL.md"),
        )
        .unwrap()
        .unwrap();
        GovernedRuntimeImplementation {
            oauth_callback_mode: None,
            package,
            actions: None,
            executable_directory: None,
            legacy_secret_environment: None,
            mcp_bindings: Some(
                serde_json::from_value(serde_json::json!({
                    "tools": {
                        "wu_messages_send": {"paths": {"/media_path": "read_file"}},
                        "wu_media_download": {
                            "paths": {"/out_dir": "create_directory"},
                            "artifacts": [{"pointer": "/path", "root": "home/.wu/media"}]
                        }
                    }
                }))
                .unwrap(),
            ),
        }
    }

    #[test]
    fn native_mcp_attachment_paths_share_the_task_workdir_without_changing_other_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let storage = std::fs::canonicalize(temp.path()).unwrap();
        let scope = storage.join("scopes/owner/default");
        let task = scope.join("internal_tasks/current/workdir");
        std::fs::create_dir_all(&task).unwrap();
        let pdf = task.join("review.pdf");
        std::fs::write(&pdf, b"PDF").unwrap();
        let runtime = whatsapp_runtime();
        let mut ctx = PrimitiveExecCtx::default_for_runtime()
            .with_scope(
                Some("owner".into()),
                Some("default".into()),
                Some("current".into()),
            )
            .with_task_work_root(Some(task));
        ctx.storage_base_path = storage;
        for raw in ["review.pdf".to_owned(), pdf.display().to_string()] {
            let mut args =
                serde_json::json!({"to": "recipient", "media_path": raw, "caption": "review"});
            bind_paths(&runtime, &ctx, "wu_messages_send", &mut args).unwrap();
            assert_eq!(args["media_path"], pdf.to_string_lossy().as_ref());
            assert_eq!(args["to"], "recipient");
            assert_eq!(args["caption"], "review");
        }
    }

    #[test]
    fn mcp_downloads_publish_task_outputs_and_keep_host_and_sibling_files_private() {
        let temp = tempfile::tempdir().unwrap();
        let storage = std::fs::canonicalize(temp.path()).unwrap();
        let scope = storage.join("scopes/owner/default");
        let task = scope.join("internal_tasks/current/workdir");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::create_dir_all(scope.join("workdirs")).unwrap();
        let mut ctx = PrimitiveExecCtx::default_for_runtime()
            .with_scope(
                Some("owner".into()),
                Some("default".into()),
                Some("current".into()),
            )
            .with_task_work_root(Some(task));
        ctx.storage_base_path = storage;
        let runtime = whatsapp_runtime();
        let mut args = serde_json::json!({"message_id": "receipt", "out_dir": "downloads"});
        bind_paths(&runtime, &ctx, "wu_media_download", &mut args).unwrap();
        let directory = PathBuf::from(args["out_dir"].as_str().unwrap());
        std::fs::create_dir(&directory).unwrap();
        let pdf = directory.join("download.pdf");
        std::fs::write(&pdf, b"PDF").unwrap();
        let result = |path: &std::path::Path| {
            serde_json::json!({
                "result": {"content": [{"type": "text", "text": serde_json::json!({"path": path}).to_string()}]}
            })
        };
        let artifacts = result_artifacts(&runtime, &ctx, "wu_media_download", &result(&pdf));
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].original_path.as_deref(), Some(pdf.as_path()));
        let media = scope.join("workdirs/home/.wu/media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("default.pdf"), b"PDF").unwrap();
        assert_eq!(
            result_artifacts(
                &runtime,
                &ctx,
                "wu_media_download",
                &result(&media.join("default.pdf"))
            )
            .len(),
            1
        );
        for relative in [
            "auth/private.json",
            "internal_tasks/sibling/workdir/private.pdf",
            "workdirs/unreviewed.pdf",
        ] {
            let path = scope.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"private").unwrap();
            assert!(
                result_artifacts(&runtime, &ctx, "wu_media_download", &result(&path)).is_empty()
            );
        }
    }
}
