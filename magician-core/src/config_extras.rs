//! Process-wide registry of extra "system-root" directories declared by
//! `tool-runtime-config.yaml :: registry.paths`.
//!
//! Each entry is a directory expected to (optionally) contain
//! `skills/<skill>/tool_schema.yaml` and/or
//! `agent_templates/agents/<id>/definition.agent.yaml` — mirroring the
//! layout of `<storage_root>/system/`. Layered onto per-scope content
//! at every consumer site. **Per-scope content always wins on
//! collision; within extras, first-listed wins** (matches
//! `SkillLoader::discover` + PATH / XDG_DATA_DIRS).
//!
//! Installed once at process startup from `bin/magician.rs::main()`
//! via [`set_extra_system_roots`]. Reads are cheap atomic loads.
//!
//! Why a process-wide singleton: the alternative is threading the path
//! list through every constructor (`AgentDefinitionStore`,
//! `CapabilityWorkspaceManager`, `ChatService`, every API handler that
//! lists skills). Skills + agent templates are loaded from ~10 sites
//! across the binary; plumbing a `Vec<PathBuf>` through each of them
//! mutates dozens of public function signatures for what is genuinely
//! one piece of startup-resolved deployment config. A `OnceLock` keeps
//! the change surface tight while still being explicit (every consumer
//! that calls [`extra_skills_dirs`] or [`extra_agent_template_dirs`]
//! is grep-discoverable).

use std::{path::PathBuf, sync::OnceLock};

use tracing::warn;

/// Resolved, existing extra-system roots. Each entry is a directory
/// that exists at install time; consumers derive `<root>/skills` and
/// `<root>/agent_templates` subpaths.
static EXTRA_SYSTEM_ROOTS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Install the process-wide list of extra system roots. Called once
/// from `bin/magician.rs::main()` after `tool-runtime-config.yaml` is
/// parsed. Idempotent: the first caller wins; subsequent calls log
/// and are ignored (matches the OnceLock semantics).
///
/// Filters input to existing directories so consumers don't have to
/// re-check. Empty / missing entries are dropped silently.
pub fn set_extra_system_roots(roots: Vec<PathBuf>) {
    let normalized: Vec<PathBuf> = roots.into_iter().filter(|p| p.exists()).collect();
    if EXTRA_SYSTEM_ROOTS.set(normalized).is_err() {
        warn!("extra system roots already installed; ignoring re-init attempt");
    }
}

/// All registered extra system roots, in the order declared in
/// `tool-runtime-config.yaml :: registry.paths`. Empty slice when the
/// OnceLock has not been initialized (tests, CLI subcommands, etc).
pub fn extra_system_roots() -> &'static [PathBuf] {
    EXTRA_SYSTEM_ROOTS
        .get()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// Process-wide flag: is the macOS host gateway (the Tauri desktop app)
/// reachable? Probed once at boot in `bin/magician.rs`. Skills that declare
/// `requires.host_gateway` are suppressed from agent catalogs when this is
/// false (headless server / desktop app not running).
static HOST_GATEWAY_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Install the boot-time host-gateway availability verdict. First caller wins.
pub fn set_host_gateway_available(available: bool) {
    if HOST_GATEWAY_AVAILABLE.set(available).is_err() {
        warn!("host gateway availability already installed; ignoring re-init attempt");
    }
}

/// Whether the macOS host gateway is available. **Defaults to `true` when
/// unprobed** (tests, CLI subcommands, any path that never called
/// `set_host_gateway_available`) so host-gateway skills are not hidden by a
/// missing probe — the gate only suppresses when a boot probe explicitly found
/// the gateway down.
pub fn host_gateway_available() -> bool {
    *HOST_GATEWAY_AVAILABLE.get().unwrap_or(&true)
}

/// CUA has its own provider: local Windows/Linux/macOS or a desktop relay.
/// This flag must never enable AppleScript or iMessage on a non-Mac host.
static CUA_AVAILABLE: OnceLock<bool> = OnceLock::new();

pub fn set_cua_available(available: bool) {
    if CUA_AVAILABLE.set(available).is_err() {
        warn!("CUA availability already installed; ignoring re-init attempt");
    }
}

pub fn cua_available() -> bool {
    *CUA_AVAILABLE.get().unwrap_or(&false)
}

/// Each extra root's `<root>/skills` subdirectory, filtered to those
/// that exist. Use as overlay sources for skill loaders.
pub fn extra_skills_dirs() -> Vec<PathBuf> {
    extra_system_roots()
        .iter()
        .map(|root| root.join("skills"))
        .filter(|p| p.exists())
        .collect()
}

/// Each extra root's `<root>/agent_templates` subdirectory, filtered
/// to those that exist. Consumed by `AgentDefinitionStore` for the
/// extras template-storage layer.
pub fn extra_agent_template_dirs() -> Vec<PathBuf> {
    extra_system_roots()
        .iter()
        .map(|root| root.join("agent_templates"))
        .filter(|p| p.exists())
        .collect()
}
