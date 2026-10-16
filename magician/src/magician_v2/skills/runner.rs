//! Ephemeral-tool runner with runtime-primitive env injection.
//!
//! When a skill's script is invoked, magician spawns the script with:
//! - `MAGICIAN_SKILL_DIR` = the calling tool's resolved skill root
//! - `MAGICIAN_SCOPE_ID`, `MAGICIAN_WORKSPACE` = identity
//! - `MAGICIAN_VAULT_TOKEN`, `MAGICIAN_LEDGER_TOKEN` = runtime primitives
//! - `PATH` prepended with every active skill's `bin/` directory
//!
//! The script reads its own config from `$MAGICIAN_SKILL_DIR/config/`.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;

use super::activation::{EphemeralTool, ScriptRuntime};

/// Per-invocation context — runtime primitives + active-skill bin paths.
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub scope_id: String,
    pub workspace: String,
    pub vault_token: String,
    pub ledger_token: String,
    /// Every active skill's `bin/` directory. Prepended to PATH (in order)
    /// so cross-skill binary invocation works.
    pub active_skill_bins: Vec<PathBuf>,
    /// The scope's tool-bin dirs (venv / node_modules / node), prepended AFTER
    /// the per-skill bins so `python3`/`node`/npm CLIs resolve to the scope's
    /// shipped runtime — the same augmentation the CLI dispatcher / preflight
    /// probe use (`CapabilityScopePaths::subprocess_bin_path`). `None` keeps
    /// today's per-skill-bins + inherited-PATH behavior.
    pub scope_paths: Option<crate::magician_v2::artifact_v2::capabilities::CapabilityScopePaths>,
    /// Additional env vars (e.g. resolved `requires.env` from vault).
    pub extra_env: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RunOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
}

/// Spawn the ephemeral tool's script with the configured runtime + env.
pub fn run_ephemeral(tool: &EphemeralTool, args: &[String], ctx: &RunContext) -> Result<RunOutput> {
    let mut cmd = build_ephemeral_command(tool, ctx);
    cmd.args(args);
    let out = cmd
        .output()
        .with_context(|| format!("spawn ephemeral tool '{}'", tool.name))?;
    Ok(RunOutput {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        exit: out.status.code().unwrap_or(-1),
    })
}

/// The PATH the ephemeral tool's child will see: per-skill bins first, then
/// the scope's tool bins, then the inherited PATH.
fn child_path(ctx: &RunContext) -> String {
    let parent = std::env::var("PATH").unwrap_or_default();
    // Scope tool-bins (venv/node_modules/node) + inherited PATH, via the shared
    // helper — fail-safe (None scope_paths / no existing bins → inherited PATH
    // unchanged, i.e. today's behavior).
    let with_scope = ctx
        .scope_paths
        .as_ref()
        .and_then(|sp| sp.subprocess_bin_path(None, &parent))
        .unwrap_or(parent);
    // Per-skill bins win over the scope bins (mirrors the dispatcher's skill_bin
    // ordering).
    let active_bins = ctx
        .active_skill_bins
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":");
    if active_bins.is_empty() {
        with_scope
    } else {
        format!("{active_bins}:{with_scope}")
    }
}

/// Program, script argument and environment for the ephemeral tool — every
/// spawn decision short of the caller's own args. Kept separate so the
/// program-resolution contract can be asserted without running the script.
fn build_ephemeral_command(tool: &EphemeralTool, ctx: &RunContext) -> std::process::Command {
    // The script is a host-absolute symlink into skillshub on a materialized
    // scope; rewrite it for this environment (identity natively) so the spawn
    // resolves in a container too.
    let script_path = super::path_rewrite::resolve_skill_path(&tool.script_path);
    let new_path = child_path(ctx);
    // The bare `python3`/`node` interpreters resolve via, in order: the
    // per-skill `active_skill_bins`, then the scope's venv/node_modules/node
    // bins (through `subprocess_bin_path` when `scope_paths` is set — matching
    // the CLI dispatcher / preflight probe), then the inherited PATH. Resolved
    // here, against that same PATH, so the spawn stays on `posix_spawn` (see
    // `runtime_core::process`).
    let interpreter =
        |name: &str| runtime_core::process::resolve_program_str(name, Some(new_path.as_str()));
    let mut cmd = match tool.runtime {
        ScriptRuntime::Bash => std::process::Command::new(&script_path),
        ScriptRuntime::Python => {
            let mut c = std::process::Command::new(interpreter("python3"));
            c.arg(&script_path);
            c
        },
        ScriptRuntime::Node => {
            let mut c = std::process::Command::new(interpreter("node"));
            c.arg(&script_path);
            c
        },
    };

    // Mirror the dispatcher: MAGICIAN_SKILL_DIR resolves for THIS environment
    // (rewritten to the real skillshub dir in a container, identity natively) so
    // a script's `$MAGICIAN_SKILL_DIR/scripts/*` lookups resolve rather than
    // dangling. (Per-skill config/.env secrets are scope-local; this ephemeral
    // path does not inject them today — unchanged here.)
    let runtime_skill_dir = super::path_rewrite::rewrite_skill_dir(&tool.skill_dir);
    cmd.env("PATH", new_path)
        .env("MAGICIAN_SKILL_DIR", &runtime_skill_dir)
        .env("MAGICIAN_SCOPE_ID", &ctx.scope_id)
        .env("MAGICIAN_WORKSPACE", &ctx.workspace)
        .env("MAGICIAN_VAULT_TOKEN", &ctx.vault_token)
        .env("MAGICIAN_LEDGER_TOKEN", &ctx.ledger_token);

    for (k, v) in &ctx.extra_env {
        cmd.env(k, v);
    }
    cmd
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn make_executable(p: &std::path::Path) {
        let mut perms = fs::metadata(p).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(p, perms).unwrap();
    }

    #[test]
    fn injects_runtime_primitive_env_into_bash_script() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("envskill");
        fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        let script = skill_dir.join("scripts/show.sh");
        fs::write(
            &script,
            "#!/bin/sh\necho \"$MAGICIAN_SKILL_DIR:$MAGICIAN_SCOPE_ID:$MAGICIAN_WORKSPACE\"\n",
        )
        .unwrap();
        make_executable(&script);

        let tool = EphemeralTool {
            name: "envskill__show".into(),
            script_path: script,
            runtime: ScriptRuntime::Bash,
            skill_dir: skill_dir.clone(),
            description: String::new(),
        };
        let ctx = RunContext {
            scope_id: "principal/ws-foo".into(),
            workspace: "ws-foo".into(),
            vault_token: "vt".into(),
            ledger_token: "lt".into(),
            active_skill_bins: vec![],
            scope_paths: None,
            extra_env: Default::default(),
        };
        let out = run_ephemeral(&tool, &[], &ctx).unwrap();
        let expected = format!("{}:principal/ws-foo:ws-foo", skill_dir.display());
        assert_eq!(out.stdout.trim(), expected);
        assert_eq!(out.exit, 0);
    }

    /// A bare interpreter name plus the overridden PATH would make std
    /// `fork` instead of `posix_spawn`; the program handed to the OS must be
    /// the absolute file found on the child's PATH, and PATH must still be
    /// overridden.
    #[test]
    fn interpreter_program_is_absolute_when_path_is_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let skill_bin = dir.path().join("skill-bin");
        fs::create_dir_all(&skill_bin).unwrap();
        let fake_node = skill_bin.join("node");
        fs::write(&fake_node, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_node);
        let script = dir.path().join("tool.js");
        fs::write(&script, "").unwrap();

        let tool = EphemeralTool {
            name: "probe__node".into(),
            script_path: script.clone(),
            runtime: ScriptRuntime::Node,
            skill_dir: dir.path().to_path_buf(),
            description: String::new(),
        };
        let ctx = RunContext {
            scope_id: "p/w".into(),
            workspace: "w".into(),
            vault_token: "v".into(),
            ledger_token: "l".into(),
            active_skill_bins: vec![skill_bin.clone()],
            scope_paths: None,
            extra_env: Default::default(),
        };
        let cmd = build_ephemeral_command(&tool, &ctx);
        assert_eq!(cmd.get_program(), fake_node.as_os_str());
        assert!(std::path::Path::new(cmd.get_program()).is_absolute());

        let child_path = cmd
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("PATH is still overridden for the child");
        assert_eq!(
            std::env::split_paths(child_path).next().as_deref(),
            Some(skill_bin.as_path())
        );
        assert_eq!(
            cmd.get_args().next(),
            Some(script.as_os_str()),
            "the script stays the first argument"
        );
    }

    #[test]
    fn prepends_active_skill_bins_to_path() {
        let dir = tempfile::tempdir().unwrap();
        let skill_b = dir.path().join("skill-b");
        fs::create_dir_all(skill_b.join("bin")).unwrap();
        let custom_bin = skill_b.join("bin/my-custom-bin");
        fs::write(&custom_bin, "#!/bin/sh\necho \"hello-from-b\"\n").unwrap();
        make_executable(&custom_bin);

        let skill_a = dir.path().join("skill-a");
        fs::create_dir_all(skill_a.join("scripts")).unwrap();
        let caller = skill_a.join("scripts/use-b.sh");
        fs::write(&caller, "#!/bin/sh\nmy-custom-bin\n").unwrap();
        make_executable(&caller);

        let tool = EphemeralTool {
            name: "skill-a__use-b".into(),
            script_path: caller,
            runtime: ScriptRuntime::Bash,
            skill_dir: skill_a.clone(),
            description: String::new(),
        };
        let ctx = RunContext {
            scope_id: "p/w".into(),
            workspace: "w".into(),
            vault_token: "v".into(),
            ledger_token: "l".into(),
            active_skill_bins: vec![skill_b.join("bin")],
            scope_paths: None,
            extra_env: Default::default(),
        };
        let out = run_ephemeral(&tool, &[], &ctx).unwrap();
        assert!(
            out.stdout.contains("hello-from-b"),
            "expected cross-skill bin call to succeed; got stdout={:?} stderr={:?}",
            out.stdout,
            out.stderr
        );
    }
}
