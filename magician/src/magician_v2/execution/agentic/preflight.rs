//! Explicit capability-readiness diagnostics.
//!
//! These probes are intentionally **not** part of task bootstrap or resume.
//! A tool catalog describes what an agent may choose; it does not predict what
//! the current request will invoke. Catalog-wide probes can perform network,
//! OAuth, keychain, or CLI startup work for unrelated tools and therefore must
//! never delay answer readiness. Normal execution discovers readiness when the
//! chosen capability is invoked and surfaces that tool's real error. This
//! module remains available to explicit setup/diagnostic surfaces that want a
//! consolidated, actionable readiness report.
//!
//! Diagnostic probes come from two sources:
//!
//! 1. **`reliability.preflight.probe`** — the new first-class field
//!    declared per capability (see [`crate::magician_v2::execution::
//!    capability::CapabilityPreflightPolicy`]). This is the preferred
//!    shape: it carries an actionable `blocker_hint` and a `required`
//!    flag distinguishing hard blockers from advisory warnings.
//! 2. **`auth.check_command`** — the legacy auth-probe field on
//!    [`crate::magician_v2::execution::capability::CapabilityAuthConfig`].
//!    Used as the fallback probe when `reliability.preflight` is not
//!    declared, so existing gws/macos-ui-automation packs participate
//!    without YAML changes.
//!
//! Capabilities without any probe are skipped, and probe execution errors are
//! reported as warnings unless explicitly required. Callers must treat this as
//! user-requested diagnostic work, never automatic execution admission.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::magician_v2::artifact_v2::capabilities::{resolve_probe_string, CapabilityScopePaths};
use crate::magician_v2::execution::capability::CapabilityPackDefinition;

/// Maximum wall-clock time per individual capability probe. Probes are
/// supposed to be cheap connectivity / auth checks — anything slower
/// than this is itself a signal the capability is unhealthy.
const PREFLIGHT_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// One capability's probe failure, with enough context for the user
/// (or an upstream HITL prompt) to act on it directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightBlocker {
    /// Capability name (matches `CapabilityPackDefinition.name`).
    pub capability_name: String,
    /// The probe command actually invoked. Empty when probing was
    /// skipped because no probe was declared.
    pub probe: String,
    /// One-line failure reason — short, user-facing.
    pub reason: String,
    /// Authored hint (when available) describing how to unblock the
    /// capability. Pulled from `reliability.preflight.blocker_hint`.
    pub blocker_hint: Option<String>,
    /// Last ~400 chars of probe stderr/stdout for diagnosis.
    pub diagnostic_excerpt: Option<String>,
    /// Probe process exit code, when the probe ran to completion.
    /// `None` when the probe timed out or failed to spawn.
    pub exit_code: Option<i32>,
    /// `true` when the probe was declared (or defaulted to) required.
    /// Explicit diagnostics classify required failures as blockers and
    /// optional failures as warnings.
    pub required: bool,
}

/// Aggregate result of probing a capability set.
#[derive(Debug, Default, Clone, Serialize)]
pub struct PreflightReport {
    /// Names of every capability the gate inspected (probe-or-skip).
    pub checked: Vec<String>,
    /// Names of capabilities that had no probe declared and were
    /// silently passed through.
    pub skipped: Vec<String>,
    /// Probe failures with `required=true` — these are hard blockers.
    pub blockers: Vec<PreflightBlocker>,
    /// Probe failures with `required=false` — surface as advisory
    /// warnings.
    pub warnings: Vec<PreflightBlocker>,
}

impl PreflightReport {
    /// `true` when an explicit readiness report contains blockers.
    pub fn has_blockers(&self) -> bool {
        !self.blockers.is_empty()
    }

    /// Render the report into a human-readable message suitable for a
    /// HITL prompt or error surface. Returns `None` when the report is
    /// clean (no blockers and no warnings).
    pub fn format_user_message(&self) -> Option<String> {
        if self.blockers.is_empty() && self.warnings.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(512);
        if !self.blockers.is_empty() {
            out.push_str("Capability readiness blockers:\n");
            for blocker in &self.blockers {
                out.push_str(&format!(
                    "- **{}**: {}",
                    blocker.capability_name, blocker.reason
                ));
                if let Some(hint) = blocker.blocker_hint.as_deref() {
                    out.push_str(&format!(" ({hint})"));
                }
                out.push('\n');
            }
        }
        if !self.warnings.is_empty() {
            if !self.blockers.is_empty() {
                out.push('\n');
            }
            out.push_str("Capability readiness warnings:\n");
            for warning in &self.warnings {
                out.push_str(&format!(
                    "- {}: {}",
                    warning.capability_name, warning.reason
                ));
                if let Some(hint) = warning.blocker_hint.as_deref() {
                    out.push_str(&format!(" ({hint})"));
                }
                out.push('\n');
            }
        }
        Some(out)
    }
}

/// Probe a single capability. Returns:
/// - `Ok(None)` when the probe passed OR no probe was declared (the
///   gate is permissive — silence on no-config).
/// - `Ok(Some(blocker))` when a probe was declared and failed. The
///   caller decides whether to treat it as a blocker or warning based
///   on `blocker.required`.
/// - `Err(_)` only when the probe could not be spawned at all (e.g.,
///   shell unavailable). Caller treats this as a soft warning.
pub async fn run_capability_preflight(
    pack: &CapabilityPackDefinition,
    scope_paths: Option<&CapabilityScopePaths>,
) -> Result<Option<PreflightBlocker>, std::io::Error> {
    // 1. Prefer the first-class `reliability.preflight.probe`. This
    //    field carries the actionable `blocker_hint` and the explicit
    //    `required` flag.
    if let Some(policy) = pack.preflight_policy() {
        if let Some(probe) = policy
            .probe
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // Substitute scope-path + `{param}`-default template vars so
            // the probe runs a real command and the hint is copy-pasteable
            // — the same interpolation the dispatcher / provider auth do.
            let probe = resolve_probe_string(pack, scope_paths, probe);
            let hint = policy
                .blocker_hint
                .as_deref()
                .map(|hint| resolve_probe_string(pack, scope_paths, hint));
            return execute_probe(&pack.name, &probe, hint, policy.required, scope_paths).await;
        }
    }

    // 2. Fall back to the legacy `auth.check_command`. Carries a
    //    generic hint pointing at `setup_command` when available.
    if let Some(auth) = pack.auth.as_ref() {
        if let Some(check) = auth
            .check_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let check = resolve_probe_string(pack, scope_paths, check);
            let hint = auth.setup_command.as_deref().map(|setup| {
                let setup = resolve_probe_string(pack, scope_paths, setup);
                format!("Authenticate via `{setup}` (or check the capability docs).")
            });
            return execute_probe(&pack.name, &check, hint, auth.required, scope_paths).await;
        }
    }

    // 3. No probe declared anywhere — silent pass.
    Ok(None)
}

/// Execute a probe and translate exit-0 into `Ok(None)`, exit-non-zero
/// into `Ok(Some(blocker))`. Wraps [`run_probe`] which always returns a
/// blocker shape for diagnostic uniformity.
async fn execute_probe(
    capability: &str,
    probe: &str,
    blocker_hint: Option<String>,
    required: bool,
    scope_paths: Option<&CapabilityScopePaths>,
) -> Result<Option<PreflightBlocker>, std::io::Error> {
    let blocker = run_probe(capability, probe, blocker_hint, required, scope_paths).await?;
    if blocker.exit_code == Some(0) {
        Ok(None)
    } else {
        Ok(Some(blocker))
    }
}

/// Run the candidate set through preflight and aggregate the result.
///
/// Order-independent: capabilities are probed sequentially because
/// most probes hit a shell login, OAuth refresh endpoint, or local
/// keychain — parallelising them risks rate-limit / lock contention
/// that the legacy serial setup already avoids.
pub async fn run_preflight_for_capabilities(
    packs: &[&CapabilityPackDefinition],
    scope_paths: Option<&CapabilityScopePaths>,
) -> PreflightReport {
    let mut report = PreflightReport::default();
    for pack in packs {
        report.checked.push(pack.name.clone());
        match run_capability_preflight(pack, scope_paths).await {
            Ok(None) => {
                if pack.preflight_policy().is_none() && pack.auth.is_none() {
                    report.skipped.push(pack.name.clone());
                }
            },
            Ok(Some(blocker)) => {
                if blocker.required {
                    report.blockers.push(blocker);
                } else {
                    report.warnings.push(blocker);
                }
            },
            Err(err) => {
                warn!(
                    capability = %pack.name,
                    error = %err,
                    "[PREFLIGHT] probe spawn failed; recording as warning"
                );
                report.warnings.push(PreflightBlocker {
                    capability_name: pack.name.clone(),
                    probe: "<spawn-failed>".to_string(),
                    reason: format!("probe could not be spawned: {err}"),
                    blocker_hint: None,
                    diagnostic_excerpt: None,
                    exit_code: None,
                    required: false,
                });
            },
        }
    }
    if !report.blockers.is_empty() || !report.warnings.is_empty() {
        info!(
            checked = report.checked.len(),
            blockers = report.blockers.len(),
            warnings = report.warnings.len(),
            "[PREFLIGHT] capability probes completed"
        );
    }
    report
}

/// Execute a single probe via `/bin/sh -c <probe>`. Returns:
/// - `Ok(blocker)` when the probe ran to completion AND failed
///   (non-zero exit). The blocker carries exit code + stderr excerpt
///   so the caller can render an actionable message.
/// - `Err(io_error)` when the probe couldn't be spawned at all.
///
/// A clean exit is signalled by `Ok` with `exit_code = Some(0)` — but
/// to keep the call-site shape simple, [`run_capability_preflight`]
/// translates that case into `Ok(None)` upstream by inspecting the
/// returned blocker's exit code.
async fn run_probe(
    capability: &str,
    probe: &str,
    blocker_hint: Option<String>,
    required: bool,
    scope_paths: Option<&CapabilityScopePaths>,
) -> Result<PreflightBlocker, std::io::Error> {
    let probe_owned = probe.to_string();
    // Augment PATH with the scope's tool-bin dirs so the probe's bare shell
    // resolves `gws`/`node`/`python3` the same way real skill subprocesses
    // do. Without this a bare `gws auth status` exits 127 (command not found)
    // and falsely reports auth broken. `None` extra_bin — the probe runs the
    // pack's `check_command`, not a specific skill CLI. Fail-safe: no
    // scope_paths / no existing bin dirs → leave PATH as inherited. Computed
    // first and threaded through the shared resolver like every other
    // PATH-overriding spawn site (see `runtime_core::process`); an absolute
    // shell passes through unchanged.
    let child_path = scope_paths.and_then(|sp| {
        let parent = std::env::var("PATH").unwrap_or_default();
        sp.subprocess_bin_path(None, &parent)
    });
    let mut spawn_command = Command::new(runtime_core::process::resolve_program_str(
        "/bin/sh",
        child_path.as_deref(),
    ));
    spawn_command
        .arg("-c")
        .arg(&probe_owned)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = child_path.as_deref() {
        spawn_command.env("PATH", path);
    }
    let spawn = spawn_command.output();

    let result = match timeout(PREFLIGHT_PROBE_TIMEOUT, spawn).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => return Err(err),
        Err(_timeout_elapsed) => {
            return Ok(PreflightBlocker {
                capability_name: capability.to_string(),
                probe: probe_owned,
                reason: format!(
                    "preflight probe timed out after {}s",
                    PREFLIGHT_PROBE_TIMEOUT.as_secs()
                ),
                blocker_hint,
                diagnostic_excerpt: None,
                exit_code: None,
                required,
            });
        },
    };

    let exit_code = result.status.code();
    if result.status.success() {
        // Clean pass — surface a sentinel blocker with exit 0 so the
        // upstream caller can recognise "probe ran cleanly" via the
        // `exit_code == Some(0)` shape. `run_capability_preflight`
        // converts this back to `None` before returning to the caller.
        return Ok(PreflightBlocker {
            capability_name: capability.to_string(),
            probe: probe_owned,
            reason: "probe passed".to_string(),
            blocker_hint: None,
            diagnostic_excerpt: None,
            exit_code,
            required,
        });
    }

    let combined = combine_probe_output(&result.stdout, &result.stderr);
    Ok(PreflightBlocker {
        capability_name: capability.to_string(),
        probe: probe_owned,
        reason: format!(
            "probe exited with code {:?}",
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<signal>".to_string())
        ),
        blocker_hint,
        diagnostic_excerpt: combined,
        exit_code,
        required,
    })
}

fn combine_probe_output(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let stderr_str = String::from_utf8_lossy(stderr);
    let stderr_trim = stderr_str.trim();
    if !stderr_trim.is_empty() {
        return Some(tail_chars(stderr_trim, 400));
    }
    let stdout_str = String::from_utf8_lossy(stdout);
    let stdout_trim = stdout_str.trim();
    if !stdout_trim.is_empty() {
        return Some(tail_chars(stdout_trim, 400));
    }
    None
}

fn tail_chars(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        s.to_string()
    } else {
        let skip = total - max;
        s.chars().skip(skip).collect::<String>()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn pack_with_probe(name: &str, probe: &str, required: bool) -> CapabilityPackDefinition {
        let yaml = format!(
            r#"
name: {name}
implementation:
  type: composite
  steps: []
reliability:
  preflight:
    probe: "{probe}"
    required: {required}
    blocker_hint: "Run setup."
"#,
            name = name,
            probe = probe,
            required = required,
        );
        serde_yaml::from_str(&yaml).expect("test yaml parses")
    }

    #[tokio::test]
    async fn preflight_passes_when_probe_exits_zero() {
        let pack = pack_with_probe("ok-cap", "true", true);
        let result = run_capability_preflight(&pack, None).await.unwrap();
        assert!(
            result.is_none(),
            "exit-0 probe must translate to Ok(None); got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn preflight_records_failure_with_stderr_excerpt() {
        let pack = pack_with_probe("bad-cap", "echo boom 1>&2; exit 42", true);
        let blocker = run_capability_preflight(&pack, None)
            .await
            .unwrap()
            .expect("failed probe must yield a blocker");
        assert_eq!(blocker.exit_code, Some(42));
        assert!(
            blocker
                .diagnostic_excerpt
                .as_deref()
                .map(|s| s.contains("boom"))
                .unwrap_or(false),
            "expected stderr excerpt to contain 'boom': {:?}",
            blocker.diagnostic_excerpt
        );
        assert!(blocker.required, "required flag must round-trip");
        assert_eq!(blocker.blocker_hint.as_deref(), Some("Run setup."));
    }

    #[tokio::test]
    async fn preflight_skips_capability_with_no_probe() {
        let yaml = r#"
name: silent-cap
implementation:
  type: composite
  steps: []
"#;
        let pack: CapabilityPackDefinition = serde_yaml::from_str(yaml).unwrap();
        let result = run_capability_preflight(&pack, None).await.unwrap();
        assert!(
            result.is_none(),
            "capability with no probe must silent-pass; got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn run_preflight_for_capabilities_partitions_blockers_and_warnings() {
        let required_fail = pack_with_probe("req-cap", "exit 1", true);
        let optional_fail = pack_with_probe("opt-cap", "exit 1", false);
        let passing = pack_with_probe("ok-cap", "true", true);
        let report =
            run_preflight_for_capabilities(&[&required_fail, &optional_fail, &passing], None).await;
        assert_eq!(report.checked, vec!["req-cap", "opt-cap", "ok-cap"]);
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.blockers[0].capability_name, "req-cap");
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].capability_name, "opt-cap");
        assert!(report.has_blockers());
    }

    #[tokio::test]
    async fn format_user_message_renders_blockers_and_hints() {
        let pack = pack_with_probe("auth-cap", "exit 1", true);
        let report = run_preflight_for_capabilities(&[&pack], None).await;
        let msg = report.format_user_message().expect("non-empty message");
        assert!(msg.contains("**auth-cap**"));
        assert!(msg.contains("Run setup."));
    }

    /// Regression: the preflight gate must substitute `{scope_*}` and
    /// `{param}` template vars BEFORE running the probe or building the
    /// blocker hint. A live gws/calendar preflight previously ran (and
    /// rendered) a literal `{scope_capability_auth_root}/gws-{account}`
    /// via `/bin/sh -c`, which exited 127 and produced an un-pasteable
    /// remediation.
    #[test]
    fn resolve_probe_string_substitutes_scope_and_param_vars() {
        // A pack that declares an `account` param (default `work`) and a
        // probe/setup that spell both `{scope_capability_auth_root}` and
        // `{account}` — the exact gws/calendar shape.
        let yaml = r#"
name: calendar
parameters:
  - name: account
    default: work
implementation:
  type: composite
  steps: []
auth:
  check_command: "CLOUDSDK_CONFIG={scope_capability_auth_root}/gws-{account}/cloudsdk gws auth status"
  setup_command: "CLOUDSDK_CONFIG={scope_capability_auth_root}/gws-{account}/cloudsdk gws auth login"
"#;
        let pack: CapabilityPackDefinition = serde_yaml::from_str(yaml).unwrap();

        // Build scope paths with a known auth root by loading a scope via
        // the workspace manager over a temp base — mirrors how providers
        // build them (auth_root is derived from the scope layout).
        let tmp = std::env::temp_dir().join(format!("preflight-vars-{}", std::process::id()));
        let workspace =
            crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(tmp.clone());
        let mgr = crate::magician_v2::artifact_v2::CapabilityWorkspaceManager::new(workspace, &tmp);
        let scope_paths = mgr.scope_paths("test-principal", "test-workspace");
        let auth_root = scope_paths.auth_root.to_string_lossy().to_string();
        assert!(
            !auth_root.contains("{scope_capability_auth_root}"),
            "test scope_paths must have a concrete auth root"
        );

        let check = pack.auth.as_ref().unwrap().check_command.clone().unwrap();
        let resolved = resolve_probe_string(&pack, Some(&scope_paths), &check);
        assert!(
            !resolved.contains("{scope_capability_auth_root}"),
            "scope var must be substituted: {resolved}"
        );
        assert!(
            !resolved.contains("{account}"),
            "param default must be substituted: {resolved}"
        );
        assert!(
            resolved.contains(&format!("{auth_root}/gws-work/cloudsdk")),
            "resolved probe must contain the real auth root + account default: {resolved}"
        );

        // Fail-open: no scope_paths leaves the scope var verbatim (never
        // panics) but still fills the `{param}` default.
        let no_scope = resolve_probe_string(&pack, None, &check);
        assert!(no_scope.contains("{scope_capability_auth_root}"));
        assert!(!no_scope.contains("{account}"));
    }
}
