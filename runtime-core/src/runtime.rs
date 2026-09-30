//! Shared runtime configuration boundary for Magician V2.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Shell sandbox mode for native command execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellSandboxMode {
    /// Allow only read-oriented commands and block obvious mutations.
    ReadOnly,
    /// Allow commands in workspace-like directories with policy checks.
    #[default]
    WorkspaceWrite,
    /// Disable sandbox checks (intended only for trusted local environments).
    Unrestricted,
}

/// What to do when a shell sandbox violation is detected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnViolation {
    /// Reject the command with an error (default).
    Fail,
    /// Pause execution and ask the user whether to allow the command.
    Ask,
}

impl Default for OnViolation {
    fn default() -> Self {
        Self::Fail
    }
}

/// Runtime shell sandbox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSandboxConfig {
    /// Sandbox strictness mode.
    #[serde(default)]
    pub mode: ShellSandboxMode,
    /// Whether commands with network-facing binaries are allowed.
    #[serde(default = "default_true")]
    pub allow_network: bool,
    /// Allow-list of command binaries that can be invoked.
    ///
    /// Empty means "no binary allow-list enforcement".
    #[serde(default = "default_shell_allowed_binaries")]
    pub allowed_binaries: Vec<String>,
    /// Command fragments that are always rejected in sandboxed modes.
    #[serde(default = "default_blocked_command_fragments")]
    pub blocked_command_fragments: Vec<String>,
    /// Working directory roots allowed for shell execution in sandboxed modes.
    /// Relative paths are resolved from current process directory.
    #[serde(default = "default_allowed_working_dirs")]
    pub allowed_working_dirs: Vec<String>,
    /// What to do when a sandbox violation is detected:
    /// "fail" (default) rejects the command, "ask" pauses and asks the user.
    #[serde(default)]
    pub on_violation: OnViolation,
}

impl Default for ShellSandboxConfig {
    fn default() -> Self {
        Self {
            mode: ShellSandboxMode::default(),
            allow_network: true,
            allowed_binaries: default_shell_allowed_binaries(),
            blocked_command_fragments: default_blocked_command_fragments(),
            allowed_working_dirs: default_allowed_working_dirs(),
            on_violation: OnViolation::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_shell_allowed_binaries() -> Vec<String> {
    // Empty = no binary allow-list enforcement; all binaries permitted.
    // Safety relies on `blocked_command_fragments` instead.
    vec![]
    //uncomment below if you want to restrict the binaries
    /*vec![
        "curl", "jq", "python3", "python", "rg", "grep", "sed", "awk", "cat", "ls", "pwd", "echo",
        "printf", "find", "head", "tail", "wc", "sort", "uniq", "xargs", "cut", "tr", "sleep",
        "true", "false", "exit",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
    */
}

fn default_blocked_command_fragments() -> Vec<String> {
    vec![
        "rm -rf /",
        "mkfs",
        "shutdown",
        "reboot",
        "poweroff",
        "dd if=/dev/zero",
        "chmod -R 777 /",
        "chown -R /",
        "curl | sh",
        "wget | sh",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn default_allowed_working_dirs() -> Vec<String> {
    vec![".".to_string()]
}

/// File sandbox mode for native filesystem execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileSandboxMode {
    /// Permit only non-mutating operations (read, exists, list).
    ReadOnly,
    /// Permit read/write operations within configured roots.
    #[default]
    WorkspaceWrite,
    /// Disable file sandbox checks (trusted local environments only).
    Unrestricted,
}

/// Runtime file sandbox configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSandboxConfig {
    /// Sandbox strictness mode.
    #[serde(default)]
    pub mode: FileSandboxMode,
    /// Allowed filesystem roots for file actions.
    /// Relative paths are resolved from current process directory.
    #[serde(default = "default_allowed_file_roots")]
    pub allowed_roots: Vec<String>,
    /// Whether delete operations are permitted in workspace_write mode.
    #[serde(default = "default_true")]
    pub allow_delete: bool,
}

impl Default for FileSandboxConfig {
    fn default() -> Self {
        Self {
            mode: FileSandboxMode::default(),
            allowed_roots: default_allowed_file_roots(),
            allow_delete: true,
        }
    }
}

impl FileSandboxConfig {
    /// Add the V3 scoped-data tree (`<base_root>/scopes`) to the allowed
    /// roots so file actions can reach durable artifacts that live under
    /// `<base_root>/scopes/<p>/<w>/durable_artifacts/...` (outside the
    /// default `.`/`/tmp` roots). Idempotent: a no-op if the scopes root is
    /// already present. Kept in lockstep with the native-action path in
    /// `v2_orchestrator::build_action_executors_with_router_override`.
    pub fn augment_with_scopes_root(&mut self, base_root: &std::path::Path) {
        let scopes_root = base_root.join("scopes").to_string_lossy().to_string();
        if !self.allowed_roots.contains(&scopes_root) {
            self.allowed_roots.push(scopes_root);
        }
    }
}

fn default_allowed_file_roots() -> Vec<String> {
    vec![".".to_string()]
}

/// Read-only configuration surface exposed to Magician V2.
///
/// Implementors (for example, the Magician binary) can supply dynamic runtime
/// values sourced from config files, environment variables, or feature flags.
pub trait RuntimeConfig: Send + Sync {
    /// Whether realtime events streaming is enabled.
    fn realtime_events_enabled(&self) -> bool;

    /// Root storage path for Magician data.
    fn storage_path(&self) -> &str;

    /// Maximum number of conversations to keep in memory.
    fn max_conversations(&self) -> usize;

    /// Conversation idle timeout.
    fn conversation_timeout(&self) -> Duration;

    /// Whether to allow LLM to generate consent_flags in atomic plans.
    /// When false (default), consent gating is handled by runtime policies.
    fn allow_consent_slots(&self) -> bool {
        false // Default: consent flags disabled
    }

    /// Shell sandbox policy for native shell command execution.
    fn shell_sandbox(&self) -> ShellSandboxConfig {
        ShellSandboxConfig::default()
    }

    /// File sandbox policy for native filesystem actions.
    fn file_sandbox(&self) -> FileSandboxConfig {
        FileSandboxConfig::default()
    }

    /// Whether consumer mode is enabled (forces AtomicComposition strategy only).
    fn consumer_mode_enabled(&self) -> bool {
        false
    }

    /// What to do when the agent hits CannotProceed or LoopDetected.
    /// Returns "ask_user" (default) or "fail".
    fn on_failure_mode(&self) -> &str {
        "ask_user"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn augment_with_scopes_root_adds_scopes_once_and_is_idempotent() {
        let mut cfg = FileSandboxConfig {
            mode: FileSandboxMode::default(),
            allowed_roots: vec![".".to_string(), "/tmp".to_string()],
            allow_delete: true,
        };
        let base = Path::new("/home/user/MagicianNotes");
        let expected = "/home/user/MagicianNotes/scopes".to_string();

        cfg.augment_with_scopes_root(base);
        assert!(cfg.allowed_roots.contains(&expected));
        // Default roots are preserved.
        assert!(cfg.allowed_roots.contains(&".".to_string()));
        assert!(cfg.allowed_roots.contains(&"/tmp".to_string()));
        let count_after_first = cfg.allowed_roots.iter().filter(|r| **r == expected).count();
        assert_eq!(count_after_first, 1);

        // Second call is a no-op — no duplicate scopes root.
        cfg.augment_with_scopes_root(base);
        let count_after_second = cfg.allowed_roots.iter().filter(|r| **r == expected).count();
        assert_eq!(count_after_second, 1);
        assert_eq!(cfg.allowed_roots.len(), 3);
    }
}
