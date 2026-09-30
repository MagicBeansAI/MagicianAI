//! Portable auth refresh for task-recipe replay.
//!
//! This lives in `magician` (rather than the HTTP crate) so Rail 1 can refresh
//! the same signed-in CDP profile before handing a task down to a browser run.

use super::auth_capture::{
    persist_browser_auth_snapshot, persist_captured_auth_events, BrowserAuthCaptureRequirements,
};
use super::recipe::{RecipeParamSource, TaskRecipe};
use super::recipe_runner::AuthHealer;
use super::router::extract_origin;
use crate::magician_v2::browser_engine_analytics::BrowserEngineAnalyticsContext;
use crate::magician_v2::execution::primitive_dispatch::browser::{
    AgentBrowserSession, ConnectionMode,
};
use crate::magician_v2::execution::MagicutorClient;
use crate::magician_v2::secrets::SecretStore;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const STORAGE_SNAPSHOT_SCRIPT: &str = "JSON.stringify((() => { const read = (s) => { const out = {}; for (let i = 0; i < s.length; i++) { const k = s.key(i); if (k !== null) out[k] = s.getItem(k) ?? ''; } return out; }; return { origin: location.origin, local_storage: read(localStorage), session_storage: read(sessionStorage) }; })())";

/// Capture cookies (including HttpOnly) plus auth-shaped browser storage from
/// the session's current origin.
pub async fn capture_browser_auth_snapshot(
    session: &AgentBrowserSession,
    secret_store: &SecretStore,
    origin_url: &str,
    requirements: &BrowserAuthCaptureRequirements,
) -> Result<bool, String> {
    let cookies = session
        .run_command(&["cookies", "get", "--json"])
        .await
        .map_err(|error| format!("reading browser cookies: {error}"))?;
    let cookie_payload = cookies.success.then_some(cookies.parsed_json).flatten();

    let storage = session
        .run_command(&["eval", STORAGE_SNAPSHOT_SCRIPT])
        .await
        .map_err(|error| format!("reading browser storage: {error}"))?;
    let (local_storage, session_storage) = storage
        .parsed_json
        .as_ref()
        .and_then(parse_browser_storage_payload)
        .filter(|(storage_origin, _, _)| extract_origin(storage_origin) == origin_url)
        .map(|(_, local_storage, session_storage)| (local_storage, session_storage))
        .unwrap_or_default();

    persist_browser_auth_snapshot(
        cookie_payload.as_ref(),
        local_storage,
        session_storage,
        requirements,
        secret_store,
        origin_url,
    )
}

/// Construct the capture hints encoded in one recipe. Headers and query tokens
/// arrive through Magicutor's transient auth drain; cookie/storage names also
/// guide the explicit snapshot.
pub fn requirements_for_recipe(
    recipe: &TaskRecipe,
    origin: &str,
) -> BrowserAuthCaptureRequirements {
    let mut requirements = BrowserAuthCaptureRequirements::default();
    let Some(version) = recipe.current() else {
        return requirements;
    };
    for source in version
        .steps
        .iter()
        .filter(|step| step.origin == origin)
        .flat_map(|step| step.param_sources.values())
    {
        let RecipeParamSource::SessionAuth { scheme } = source else {
            continue;
        };
        if let Some(name) = scheme.strip_prefix("cookie:") {
            requirements.cookies.insert(name.to_owned());
        } else if let Some(name) = scheme.strip_prefix("local_storage:") {
            requirements.local_storage_keys.insert(name.to_owned());
        } else if let Some(name) = scheme.strip_prefix("session_storage:") {
            requirements.session_storage_keys.insert(name.to_owned());
        }
    }
    requirements
}

fn parse_browser_storage_payload(
    payload: &serde_json::Value,
) -> Option<(String, HashMap<String, String>, HashMap<String, String>)> {
    if let Some(raw) = payload.as_str() {
        return parse_browser_storage_payload(&serde_json::from_str(raw).ok()?);
    }
    for key in ["data", "result", "value"] {
        if let Some(parsed) = payload.get(key).and_then(parse_browser_storage_payload) {
            return Some(parsed);
        }
    }
    let origin = payload.get("origin")?.as_str()?.to_owned();
    let local = payload
        .get("local_storage")
        .or_else(|| payload.get("localStorage"))
        .and_then(storage_value_map)?;
    let session = payload
        .get("session_storage")
        .or_else(|| payload.get("sessionStorage"))
        .and_then(storage_value_map)
        .unwrap_or_default();
    Some((origin, local, session))
}

fn storage_value_map(value: &serde_json::Value) -> Option<HashMap<String, String>> {
    value.as_object().map(|object| {
        object
            .iter()
            .filter_map(|(key, value)| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(|value| (key.clone(), value.to_owned()))
            })
            .collect()
    })
}

pub fn magicutor_cdp_connection_mode(
    base_url: &url::Url,
    session_id: &str,
) -> Result<ConnectionMode, String> {
    let mut base = base_url.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.set_query(None);
    base.set_fragment(None);
    let mut cdp_url = base
        .join(&format!("devtools/browser/{session_id}"))
        .map_err(|error| error.to_string())?;
    let websocket_scheme = match base.scheme() {
        "http" => "ws",
        "https" => "wss",
        scheme => return Err(format!("unsupported Magicutor URL scheme {scheme}")),
    };
    cdp_url
        .set_scheme(websocket_scheme)
        .map_err(|_| "could not convert Magicutor URL to a WebSocket URL".to_owned())?;
    Ok(ConnectionMode::Cdp {
        url: cdp_url.to_string(),
    })
}

pub struct AuthRefreshTarget {
    pub origin_url: String,
    pub requirements: BrowserAuthCaptureRequirements,
}

/// RecipeRunner can cancel healing at its absolute deadline. Stack unwinding
/// must still release the profile/proxy session created by this attempt.
struct AuthRefreshCleanup {
    session: Arc<AgentBrowserSession>,
    magicutor: Arc<MagicutorClient>,
}

impl Drop for AuthRefreshCleanup {
    fn drop(&mut self) {
        let session = Arc::clone(&self.session);
        let magicutor = Arc::clone(&self.magicutor);
        tokio::spawn(async move {
            let _ = tokio::join!(
                tokio::time::timeout(Duration::from_secs(10), session.shutdown()),
                tokio::time::timeout(
                    Duration::from_secs(10),
                    magicutor.delete_session(session.session_id())
                ),
            );
        });
    }
}

/// Open the signed-in profile, load the origin, and consume auth material into
/// the encrypted scoped store. The bounded wait is part of the caller's recipe
/// deadline; cleanup is best-effort on every exit.
pub async fn refresh_origin_auth(
    cli_path: PathBuf,
    magicutor: Arc<MagicutorClient>,
    secret_store: Arc<SecretStore>,
    target: &AuthRefreshTarget,
    session_id: &str,
    analytics: Option<BrowserEngineAnalyticsContext>,
    timeout: Duration,
) -> Result<bool, String> {
    let connection_mode = magicutor_cdp_connection_mode(magicutor.base_url(), session_id)?;
    let mut session =
        AgentBrowserSession::new_with_session_id(session_id.to_owned(), connection_mode, cli_path)
            .map_err(|error| error.to_string())?
            .with_initial_url(Some(target.origin_url.clone()));
    if let Some(analytics) = analytics {
        session = session.with_analytics_context(analytics);
    }
    let session = Arc::new(session);
    let _cleanup = AuthRefreshCleanup {
        session: Arc::clone(&session),
        magicutor: Arc::clone(&magicutor),
    };
    if let Err(error) = session.ensure_connected().await {
        return Err(format!("opening signed-in browser profile: {error}"));
    }

    let started = Instant::now();
    let mut last_error = None;
    let mut captured = false;
    while started.elapsed() < timeout {
        match magicutor.drain_captured_auth(session.session_id()).await {
            Ok(events) => match persist_captured_auth_events(
                &events,
                secret_store.as_ref(),
                Some(&target.origin_url),
            ) {
                Ok(origins) => captured |= origins.contains(&target.origin_url),
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(error.to_string()),
        }
        match capture_browser_auth_snapshot(
            &session,
            secret_store.as_ref(),
            &target.origin_url,
            &target.requirements,
        )
        .await
        {
            Ok(found) => captured |= found,
            Err(error) => last_error = Some(error),
        }
        if captured {
            break;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    if captured {
        Ok(true)
    } else if let Some(error) = last_error {
        Err(error)
    } else {
        Ok(false)
    }
}

pub struct OrchestratorAuthHealer {
    pub cli_path: PathBuf,
    pub magicutor: Arc<MagicutorClient>,
    pub secret_store: Arc<SecretStore>,
    pub requirements: HashMap<String, BrowserAuthCaptureRequirements>,
    pub analytics_root: PathBuf,
    pub principal: String,
    pub workspace: String,
    pub timeout: Duration,
}

#[async_trait::async_trait]
impl AuthHealer for OrchestratorAuthHealer {
    async fn heal(&self, origin: &str) -> Result<bool, String> {
        let target = AuthRefreshTarget {
            origin_url: origin.to_owned(),
            requirements: self.requirements.get(origin).cloned().unwrap_or_default(),
        };
        let session_id = format!("recipe-auth-{}", ulid::Ulid::new());
        refresh_origin_auth(
            self.cli_path.clone(),
            Arc::clone(&self.magicutor),
            Arc::clone(&self.secret_store),
            &target,
            &session_id,
            Some(
                BrowserEngineAnalyticsContext::for_scope(
                    &self.analytics_root,
                    &self.principal,
                    &self.workspace,
                    None,
                    None,
                )
                .with_work("recipe_auth_heal", session_id.clone()),
            ),
            self.timeout,
        )
        .await
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn websocket_mode_preserves_magicutor_base_path() {
        let base = url::Url::parse("https://magicutor.test/proxy/").unwrap();
        let mode = magicutor_cdp_connection_mode(&base, "auth-1").unwrap();
        assert_eq!(
            mode,
            ConnectionMode::Cdp {
                url: "wss://magicutor.test/proxy/devtools/browser/auth-1".into()
            }
        );
    }
}
