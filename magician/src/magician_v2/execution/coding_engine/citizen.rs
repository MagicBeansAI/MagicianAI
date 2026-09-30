//! Magician Citizen — per-run token broker + the bundled Pi extension (M6).
//!
//! When a coding run spawns Pi with the `magician-citizen` extension, it mints
//! a short-lived, scope-qualified token here and injects it (+ the loopback
//! Citizen API base URL) as env. The extension presents the token as a `Bearer`
//! header on every capability call; the Citizen API (`api::vibedev_api`)
//! resolves it back to the run's scope so a capability only ever acts within
//! that run's principal/workspace (and, when known, project). Mirrors
//! [`super::control::CodingControlRegistry`]: a process-global map whose entries
//! live for exactly the run and are revoked the moment it settles.
//!
//! Phase 0 exposes ONE capability (`preview_url`) over the shipped R3
//! dev-server manager, to prove the extension → tool → loopback-HTTP → Pi-turn
//! round-trip.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use once_cell::sync::{Lazy, OnceCell};
use tokio::sync::Mutex;
use uuid::Uuid;

/// The bundled citizen extension source, embedded so the binary is
/// self-contained (no runtime asset-path assumptions). Written to a per-scope
/// dir at spawn and passed to `pi --extension <path>`.
const CITIZEN_EXTENSION_TS: &str =
    include_str!("../../../../assets/pi-extensions/magician-citizen.ts");
const CITIZEN_EXTENSION_FILE: &str = "magician-citizen.ts";

/// What a citizen token resolves to — the scope a capability call runs within.
#[derive(Debug, Clone)]
pub struct CitizenGrant {
    pub principal: String,
    pub workspace: String,
    /// The run's VibeDev project, when known directly at spawn. Usually `None`
    /// — the Citizen API resolves the project from `root_task_id` instead.
    pub project_id: Option<String>,
    /// The run's root VibeDev task id (== the cockpit's `active_root_task_id`
    /// for the project). The Citizen API resolves the project by matching
    /// this against each project's `active_root_task_id`, so the citizen
    /// tools need no explicit `project_id` argument. `None` for runs not
    /// bound to a project.
    pub root_task_id: Option<String>,
    /// The agent actually executing this coding run (the run's `__agent_id`).
    /// Code memory is written under the EXECUTING engineer agent, not the root
    /// task's owner — in the cockpit the root task is owned by a coordinator
    /// (engineering-manager) that delegates coding to a worker engineer. The
    /// `magician_code_knowledge` tool reads from this agent so the recall hits
    /// the agent whose memory the distillation wrote. `None` for legacy grants
    /// (the handler then falls back to the root task's `manifest.agent_id`).
    pub agent_id: Option<String>,
    /// P3 least-privilege: the citizen capabilities this run's Pi may call
    /// (bare capability names, e.g. `["code_knowledge"]`). EMPTY ==
    /// unscoped (legacy / build runs) == ALL capabilities permitted, so the
    /// default stays byte-identical; a populated list permits ONLY those.
    /// Enforced server-side in `authorize_citizen` (the authoritative gate),
    /// and mirrored to Pi via `MAGICIAN_CITIZEN_TOOLS` so the extension
    /// doesn't register the rest.
    pub allowed_tools: Vec<String>,
}

#[derive(Default)]
pub struct CitizenTokenRegistry {
    grants: Mutex<HashMap<String, CitizenGrant>>,
}

static REGISTRY: Lazy<Arc<CitizenTokenRegistry>> =
    Lazy::new(|| Arc::new(CitizenTokenRegistry::default()));

/// The process-global citizen token registry.
pub fn citizen_token_registry() -> Arc<CitizenTokenRegistry> {
    REGISTRY.clone()
}

impl CitizenTokenRegistry {
    /// Mint a fresh opaque token for `grant` and register it. Returns the token
    /// to inject into Pi's env. Revoke with [`Self::revoke`] when the run
    /// settles.
    pub async fn mint(&self, grant: CitizenGrant) -> String {
        let token = format!("czt_{}", Uuid::new_v4().simple());
        self.grants.lock().await.insert(token.clone(), grant);
        token
    }

    /// Resolve a Bearer token to its grant (`None` = unknown / already
    /// revoked).
    pub async fn resolve(&self, token: &str) -> Option<CitizenGrant> {
        self.grants.lock().await.get(token.trim()).cloned()
    }

    /// Drop a token the moment its run settles, so a stale token can never act.
    pub async fn revoke(&self, token: &str) {
        self.grants.lock().await.remove(token.trim());
    }
}

/// The loopback base URL of the Citizen API, set once at server startup
/// (`http://127.0.0.1:<port>/api/magician/v2/vibedev`). `None` when the HTTP
/// server isn't running (e.g. a CLI subcommand) → the citizen env is NOT
/// injected and the extension's tools report "not configured" rather than
/// failing a run.
static BASE_URL: OnceCell<String> = OnceCell::new();

/// Record the Citizen API base URL at server startup. Idempotent (first wins).
pub fn set_citizen_base_url(url: String) {
    let _ = BASE_URL.set(url);
}

/// The Citizen API base URL, if the HTTP server set it at startup.
pub fn citizen_base_url() -> Option<String> {
    BASE_URL.get().cloned()
}

/// Materialize the bundled extension into `dir` and return its absolute path
/// for `pi --extension <path>`. `dir` should be OUTSIDE the run's shadow
/// working tree so the file never enters the proposal diff. Idempotent
/// (rewrites the source).
pub fn write_citizen_extension(dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(CITIZEN_EXTENSION_FILE);
    std::fs::write(&path, CITIZEN_EXTENSION_TS)?;
    Ok(path)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mint_resolve_revoke_roundtrip() {
        let reg = CitizenTokenRegistry::default();
        let token = reg
            .mint(CitizenGrant {
                principal: "anonymous".into(),
                workspace: "default".into(),
                project_id: Some("proj-1".into()),
                root_task_id: None,
                agent_id: Some("senior-software-developer".into()),
                allowed_tools: vec!["code_knowledge".into()],
            })
            .await;
        assert!(token.starts_with("czt_"));
        let grant = reg
            .resolve(&token)
            .await
            .expect("token resolves to its grant");
        assert_eq!(grant.principal, "anonymous");
        assert_eq!(grant.project_id.as_deref(), Some("proj-1"));
        assert_eq!(grant.agent_id.as_deref(), Some("senior-software-developer"));
        assert_eq!(grant.allowed_tools, vec!["code_knowledge".to_string()]);
        reg.revoke(&token).await;
        assert!(
            reg.resolve(&token).await.is_none(),
            "revoked token no longer resolves"
        );
    }

    #[tokio::test]
    async fn unknown_token_resolves_to_none() {
        let reg = CitizenTokenRegistry::default();
        assert!(reg.resolve("czt_nope").await.is_none());
    }

    #[test]
    fn embedded_extension_registers_the_preview_tool() {
        assert!(CITIZEN_EXTENSION_TS.contains("magician_preview_url"));
        assert!(CITIZEN_EXTENSION_TS.contains("registerTool"));
    }
}
