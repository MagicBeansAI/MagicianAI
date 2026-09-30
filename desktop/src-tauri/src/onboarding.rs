//! Capability and Skillshub onboarding shared by first run and maintenance.
//!
//! The connected Magician service owns discovery and probes. This matters for
//! remote engines: looking for a server binary or env file on the desktop would
//! report the wrong machine. Desktop persists only choices and explicit manual
//! confirmations; credentials remain write-only through the existing env seam.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use magician_components::setup::{
    render_template, ManagedBotStart, ResolvedSetup, SetupDriver, SetupFieldKind, SetupModeChoice,
};
use magician_components::{Component, Graph, Host, InstallAction, Observed, ProbeSpec, Report};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use zeroize::Zeroizing;

const STATE_SCHEMA_VERSION: u32 = 1;
const SETUP_TOKEN_HEADER: &str = "X-Magician-Setup-Token";
const SETUP_TOKEN_KEYRING_SERVICE: &str = "ai.magicbeans.magican.desktop.setup-admin.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OnboardingState {
    #[serde(default = "state_schema_version")]
    schema_version: u32,
    #[serde(default)]
    selected_features: BTreeSet<String>,
    #[serde(default)]
    manual_confirmations: BTreeSet<String>,
    #[serde(default)]
    configured_setups: BTreeSet<String>,
    #[serde(default)]
    engine_origin: Option<String>,
    #[serde(default)]
    workspace_scope: Option<String>,
    #[serde(default)]
    completed: bool,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            selected_features: BTreeSet::new(),
            manual_confirmations: BTreeSet::new(),
            configured_setups: BTreeSet::new(),
            engine_origin: None,
            workspace_scope: None,
            completed: false,
        }
    }
}

fn state_schema_version() -> u32 {
    STATE_SCHEMA_VERSION
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentCatalogSnapshot {
    pub host: Host,
    pub graph: Graph,
    pub observed: BTreeMap<String, Observed>,
    pub report: Report,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillCatalogResponse {
    #[serde(default)]
    pub skills: Vec<SkillCatalogItem>,
    #[serde(default = "default_scope")]
    pub current_scope: String,
}

fn default_scope() -> String {
    "anonymous/default".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillCatalogItem {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub origin: String,
    #[serde(default = "default_true")]
    pub installable: bool,
    #[serde(default)]
    pub requires_bins: Vec<String>,
    #[serde(default)]
    pub missing_bins: Vec<String>,
    #[serde(default)]
    pub requires_env: Vec<String>,
    #[serde(default)]
    pub install_hint: Option<String>,
    #[serde(default)]
    pub auth: Option<SkillCatalogAuth>,
    #[serde(default)]
    pub installed_scopes: Vec<String>,
    #[serde(default)]
    pub setup: Option<SkillSetupStatus>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillCatalogAuth {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub requirement: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub profile_selection: SkillProfileSelection,
    #[serde(default)]
    pub secret_refs: Vec<String>,
    #[serde(default)]
    pub has_status: bool,
    #[serde(default)]
    pub has_login: bool,
    #[serde(default)]
    pub login_interaction: Option<String>,
    #[serde(default)]
    pub has_logout: bool,
    #[serde(default)]
    pub setup: Option<ResolvedSetup>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SkillProfileSelection {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillSetupStatus {
    pub required: bool,
    pub ready: bool,
    #[serde(default)]
    pub missing: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillEnvStatus {
    #[serde(default)]
    keys: Vec<SkillEnvKey>,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillEnvKey {
    key: String,
    #[serde(default)]
    set: bool,
}

#[derive(Deserialize)]
struct SkillOAuthStartResponse {
    authorization_url: String,
}

#[derive(Clone, Deserialize)]
struct SkillOAuthStatusResponse {
    ready: bool,
    status: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingCatalog {
    pub components: ComponentCatalogSnapshot,
    pub skills: Vec<SkillCatalogItem>,
    pub current_scope: String,
    pub selected_features: Vec<String>,
    pub preferences_exist: bool,
    pub setup_access: SetupAccess,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupAccess {
    pub authorized: bool,
    pub token_stored: bool,
    pub local_bootstrap: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetupTokenStatus {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    pending_token: Option<String>,
    #[serde(default)]
    unavailable_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSetupToken {
    version: u8,
    origin: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeEnvStatus {
    active_mode: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ComponentPlanResponse {
    host: Host,
    graph: Graph,
    observed: BTreeMap<String, Observed>,
    plan: magician_components::selection::SelectionPlan,
    projected: Report,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingPlan {
    pub selected_features: Vec<String>,
    pub steps: Vec<OnboardingStep>,
    pub unsupported: Vec<(String, String)>,
    pub unresolved: Vec<String>,
    pub left_off: Vec<(String, String)>,
    pub projected: Report,
    pub ready: bool,
    pub blocking_count: usize,
    pub host: Host,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingStep {
    pub id: String,
    pub name: String,
    pub required: bool,
    pub cost: String,
    pub pricing: String,
    pub install: InstallAction,
    pub observed: Observed,
    pub ready: bool,
    pub can_confirm: bool,
    pub confirmed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<ResolvedSetup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingSelectionRequest {
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingConfirmationRequest {
    pub component_id: String,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingSecretRequest {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingSetupTokenRequest {
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingConfigurationFileRequest {
    pub component_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingSkillRequest {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingComponentRequest {
    pub component_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigureModelRuntimeRequest {
    pub component_id: String,
    pub mode: String,
    #[serde(default)]
    pub selected_model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalGenerationModelSnapshot {
    pub id: String,
    pub label: String,
    pub ollama: String,
    pub min_memory_gb: u32,
    pub resident_gb: Option<f64>,
    pub disk_gb: Option<f64>,
    pub notes: Option<String>,
    pub selected: bool,
    pub recommended: bool,
    pub rule_ok: bool,
    pub installed: Option<bool>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LocalGenerationSettingsSnapshot {
    selected: Option<String>,
    recommended: Option<String>,
    processing_mode: String,
    models: Vec<LocalGenerationModelSnapshot>,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRuntimeSnapshot {
    pub component_id: String,
    pub modes: Vec<SetupModeChoice>,
    pub local_feature: String,
    pub mode: String,
    pub selected: Option<String>,
    pub recommended: Option<String>,
    pub models: Vec<LocalGenerationModelSnapshot>,
    pub warnings: Vec<String>,
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserExtensionPreparation {
    pub source_path: String,
    pub management_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CuaDriverSetupResult {
    pub installed: bool,
    pub ready: bool,
    pub detail: String,
    pub installer_output: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CodingProfilesResponse {
    #[serde(default)]
    profiles: Vec<CodingProfileSnapshot>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodingProfileSnapshot {
    #[serde(default)]
    engine: String,
    #[serde(default)]
    selectable: bool,
    #[serde(default)]
    readiness: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlaneEnginesResponse {
    #[serde(default)]
    engines: Vec<PlaneEngineSnapshot>,
    current: String,
    chat_current: String,
    #[serde(default = "default_harness_model")]
    chat_model: String,
    #[serde(default = "default_harness_model")]
    run_model: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PlaneEngineSnapshot {
    name: String,
    #[serde(default)]
    installed: bool,
    #[serde(default)]
    models: Vec<String>,
}

fn default_harness_model() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingHarnessCatalog {
    pub harnesses: Vec<OnboardingHarness>,
    pub chat_current: String,
    pub run_current: String,
    pub chat_model: String,
    pub run_model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingHarness {
    pub id: String,
    pub order: u16,
    pub label: String,
    pub binary: String,
    pub coding_engine: String,
    pub plane_engine: String,
    pub config_key: String,
    pub install_url: String,
    pub install_hint: String,
    pub surfaces: Vec<magician_components::setup::HarnessSurface>,
    pub readiness: String,
    pub reason: String,
    pub version: Option<String>,
    pub selectable: bool,
    pub installed: bool,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RefreshOnboardingHarnessRequest {
    pub harness_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConfigureOnboardingHarnessRequest {
    pub target: String,
    pub engine: String,
    #[serde(default = "default_harness_model")]
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingAuthInputRequest {
    pub component_id: String,
    pub input: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingSkillAuthInputRequest {
    pub name: String,
    pub input: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingSkillAuthConfigureRequest {
    pub name: String,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OnboardingComponentAuthConfigureRequest {
    pub component_id: String,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentInstallJob {
    pub id: String,
    pub component_id: String,
    pub component_name: String,
    pub phase: String,
    pub output: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub observed: Option<Observed>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotPairingSnapshot {
    pub component_id: String,
    pub bot_name: String,
    pub status: String,
    pub flow_state: String,
    pub detail: Option<String>,
    pub ready: bool,
    pub log: String,
    pub qr_data_url: Option<String>,
    pub login_url: Option<String>,
}

fn state_path() -> PathBuf {
    crate::config::data_dir()
        .join("config")
        .join("onboarding.json")
}

fn load_state() -> Result<(OnboardingState, bool), String> {
    let path = state_path();
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((OnboardingState::default(), false));
        },
        Err(error) => {
            return Err(format!(
                "Failed to read onboarding choices at {}: {error}",
                path.display()
            ));
        },
    };
    let state: OnboardingState = serde_json::from_str(&source)
        .map_err(|error| format!("Failed to parse onboarding choices: {error}"))?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported onboarding state version {}",
            state.schema_version
        ));
    }
    Ok((state, true))
}

fn save_state(state: &OnboardingState) -> Result<(), String> {
    let path = state_path();
    let parent = path
        .parent()
        .ok_or_else(|| "Onboarding state path has no parent".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
    let source = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("Failed to serialize onboarding choices: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, source)
        .map_err(|error| format!("Failed to write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("Failed to replace {}: {error}", path.display()))?;
    Ok(())
}

fn normalized_engine_origin(engine_url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(engine_url)
        .map_err(|_| "The selected Magician server address is invalid".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("The selected Magician server must be an HTTP(S) origin".to_string());
    }
    Ok(parsed.origin().ascii_serialization())
}

pub fn mark_incomplete_for_engine(engine_url: &str) -> Result<(), String> {
    let origin = normalized_engine_origin(engine_url)?;
    let (mut state, _) = load_state()?;
    if state.engine_origin.as_deref() != Some(origin.as_str()) {
        state.configured_setups.clear();
    }
    state.engine_origin = Some(origin);
    state.workspace_scope = None;
    state.completed = false;
    save_state(&state)
}

pub fn completion_pending_for_engine(engine_url: &str) -> bool {
    let Ok(origin) = normalized_engine_origin(engine_url) else {
        return true;
    };
    let Ok((state, exists)) = load_state() else {
        return true;
    };
    exists && !state_completed_for_origin(&state, &origin)
}

fn state_completed_for_origin(state: &OnboardingState, origin: &str) -> bool {
    state.completed && state.engine_origin.as_deref() == Some(origin)
}

async fn engine_base_url(app: &AppHandle) -> String {
    app.state::<crate::AppState>()
        .config
        .lock()
        .await
        .engine_base_url()
}

async fn engine_origin(app: &AppHandle) -> Result<String, String> {
    normalized_engine_origin(&engine_base_url(app).await)
}

fn setup_token_entry(origin: &str) -> Result<keyring::Entry, String> {
    let account = blake3::hash(origin.as_bytes()).to_hex().to_string();
    keyring::Entry::new(SETUP_TOKEN_KEYRING_SERVICE, &account)
        .map_err(|_| "The Desktop setup credential store is unavailable".to_string())
}

async fn load_setup_token(origin: &str) -> Result<Option<Zeroizing<String>>, String> {
    let origin = origin.to_string();
    tokio::task::spawn_blocking(move || {
        let encoded = match setup_token_entry(&origin)?.get_password() {
            Ok(encoded) => Zeroizing::new(encoded),
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(_) => {
                return Err("The Desktop setup credential could not be read".to_string());
            },
        };
        if encoded.len() > 32_768 {
            return Err("The saved Desktop setup credential is invalid".to_string());
        }
        let stored: StoredSetupToken = serde_json::from_str(&encoded)
            .map_err(|_| "The saved Desktop setup credential is invalid".to_string())?;
        if stored.version != 1 || stored.origin != origin || !valid_setup_token(&stored.token) {
            return Err("The saved Desktop setup credential belongs to another server".to_string());
        }
        Ok(Some(Zeroizing::new(stored.token)))
    })
    .await
    .map_err(|_| "The Desktop setup credential reader stopped".to_string())?
}

async fn save_setup_token(origin: &str, token: &str) -> Result<(), String> {
    if !valid_setup_token(token) {
        return Err("The setup token is empty or invalid".to_string());
    }
    let stored = StoredSetupToken {
        version: 1,
        origin: origin.to_string(),
        token: token.trim().to_string(),
    };
    tokio::task::spawn_blocking(move || {
        let encoded = Zeroizing::new(
            serde_json::to_string(&stored)
                .map_err(|_| "The Desktop setup credential could not be encoded".to_string())?,
        );
        setup_token_entry(&stored.origin)?
            .set_password(&encoded)
            .map_err(|_| "The Desktop setup credential could not be saved securely".to_string())
    })
    .await
    .map_err(|_| "The Desktop setup credential writer stopped".to_string())?
}

async fn delete_setup_token(origin: &str) -> Result<(), String> {
    let origin = origin.to_string();
    tokio::task::spawn_blocking(
        move || match setup_token_entry(&origin)?.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("The Desktop setup credential could not be removed".to_string()),
        },
    )
    .await
    .map_err(|_| "The Desktop setup credential remover stopped".to_string())?
}

fn valid_setup_token(token: &str) -> bool {
    let token = token.trim();
    !token.is_empty() && token.len() <= 8192 && !token.chars().any(char::is_control)
}

fn is_loopback_origin(origin: &str) -> bool {
    reqwest::Url::parse(origin).is_ok_and(|url| {
        matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
        )
    })
}

async fn get_json<T: DeserializeOwned>(app: &AppHandle, path: &str) -> Result<T, String> {
    let url = format!("{}{path}", engine_base_url(app).await);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let response = crate::magician_auth::authorize(client.get(&url))
        .send()
        .await
        .map_err(|error| format!("Could not reach the Magician setup catalog: {error}"))?;
    decode_response(response).await
}

async fn post_json<T: DeserializeOwned, B: Serialize + ?Sized>(
    app: &AppHandle,
    path: &str,
    body: &B,
) -> Result<T, String> {
    let url = format!("{}{path}", engine_base_url(app).await);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let response = crate::magician_auth::authorize(client.post(&url).json(body))
        .send()
        .await
        .map_err(|error| format!("Could not build the Magician setup plan: {error}"))?;
    decode_response(response).await
}

async fn put_json<T: DeserializeOwned, B: Serialize + ?Sized>(
    app: &AppHandle,
    path: &str,
    body: &B,
) -> Result<T, String> {
    let url = format!("{}{path}", engine_base_url(app).await);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let response = crate::magician_auth::authorize(client.put(&url).json(body))
        .send()
        .await
        .map_err(|error| format!("Could not update the Magician harness selection: {error}"))?;
    decode_response(response).await
}

async fn admin_send<T: DeserializeOwned>(
    app: &AppHandle,
    request: reqwest::RequestBuilder,
) -> Result<T, String> {
    let origin = engine_origin(app).await?;
    let request_url = request
        .try_clone()
        .ok_or_else(|| "The setup request could not be inspected".to_string())?
        .build()
        .map_err(|_| "The setup request is invalid".to_string())?
        .url()
        .clone();
    if request_url.origin().ascii_serialization() != origin
        || !request_url.path().starts_with("/api/magician/")
        || !request_url.username().is_empty()
        || request_url.password().is_some()
    {
        return Err(
            "Refusing to attach the setup token outside the selected Magician server".to_string(),
        );
    }
    let token = load_setup_token(&origin)
        .await?
        .ok_or_else(|| "This Magician server's setup token is required".to_string())?;
    let response = crate::magician_auth::authorize(request)
        .header(SETUP_TOKEN_HEADER, token.as_str())
        .send()
        .await
        .map_err(|error| format!("Could not reach the Magician setup service: {error}"))?;
    decode_response(response).await
}

async fn check_setup_access_with_token(app: &AppHandle, token: &str) -> Result<(), String> {
    let url = format!(
        "{}/api/magician/v2/components/admin-access",
        engine_origin(app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let response = crate::magician_auth::authorize(client.get(url))
        .header(SETUP_TOKEN_HEADER, token)
        .send()
        .await
        .map_err(|error| format!("Could not verify the setup token: {error}"))?;
    let _: serde_json::Value = decode_response(response).await?;
    Ok(())
}

async fn ensure_setup_access(app: &AppHandle) -> Result<SetupAccess, String> {
    let origin = engine_origin(app).await?;
    let local_bootstrap = is_loopback_origin(&origin);
    let stored = load_setup_token(&origin).await?;
    if let Some(token) = stored.as_ref() {
        if check_setup_access_with_token(app, token.as_str())
            .await
            .is_ok()
        {
            return Ok(SetupAccess {
                authorized: true,
                token_stored: true,
                local_bootstrap,
                unavailable_reason: None,
            });
        }
    }

    if !local_bootstrap {
        return Ok(SetupAccess {
            authorized: false,
            token_stored: stored.is_some(),
            local_bootstrap: false,
            unavailable_reason: None,
        });
    }

    let status =
        match get_json::<SetupTokenStatus>(app, "/api/magician/v2/secrets/setup-token").await {
            Ok(status) => status,
            Err(error) => {
                return Ok(SetupAccess {
                    authorized: false,
                    token_stored: stored.is_some(),
                    local_bootstrap: true,
                    unavailable_reason: Some(error),
                });
            },
        };
    if !status.available {
        return Ok(SetupAccess {
            authorized: false,
            token_stored: stored.is_some(),
            local_bootstrap: true,
            unavailable_reason: status.unavailable_reason,
        });
    }
    let Some(token) = status
        .pending_token
        .filter(|token| valid_setup_token(token))
    else {
        return Ok(SetupAccess {
            authorized: false,
            token_stored: stored.is_some(),
            local_bootstrap: true,
            unavailable_reason: Some(
                "The server has already displayed its setup token; enter or rotate it to continue"
                    .to_string(),
            ),
        });
    };

    save_setup_token(&origin, &token).await?;
    let _: SetupTokenStatus = post_json(
        app,
        "/api/magician/v2/secrets/setup-token/acknowledge",
        &serde_json::json!({"token": token}),
    )
    .await?;
    check_setup_access_with_token(app, &token).await?;
    Ok(SetupAccess {
        authorized: true,
        token_stored: true,
        local_bootstrap: true,
        unavailable_reason: None,
    })
}

async fn decode_response<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read setup response: {error}"))?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                ["message", "error", "details"]
                    .into_iter()
                    .find_map(|key| value.get(key).and_then(|item| item.as_str()))
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| body.chars().take(300).collect());
        return Err(format!(
            "Magician setup request failed ({status}): {message}"
        ));
    }
    serde_json::from_str(&body).map_err(|error| format!("Invalid setup response: {error}"))
}

async fn get_optional_qr_data_url(
    app: &AppHandle,
    bot_name: &str,
) -> Result<Option<String>, String> {
    let url = format!(
        "{}/api/magician/v2/bots/{bot_name}/qr",
        engine_origin(app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let response = crate::magician_auth::authorize(client.get(url))
        .send()
        .await
        .map_err(|error| format!("Could not read the pairing QR: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("Pairing QR request failed ({})", response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("image/"))
        .unwrap_or("image/png")
        .to_string();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Could not read the pairing QR: {error}"))?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("The pairing QR is too large".to_string());
    }
    Ok(Some(format!(
        "data:{content_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )))
}

#[tauri::command]
pub async fn get_onboarding_catalog(app: AppHandle) -> Result<OnboardingCatalog, String> {
    let (components, mut skills, setup_access) = tokio::try_join!(
        get_json::<ComponentCatalogSnapshot>(&app, "/api/magician/v2/components/catalog"),
        get_json::<SkillCatalogResponse>(&app, "/api/magician/v2/skills/catalog"),
        ensure_setup_access(&app),
    )?;
    enrich_skill_setup_status(
        &app,
        &mut skills.skills,
        &skills.current_scope,
        setup_access.authorized,
    )
    .await;
    enrich_skill_profile_status(
        &app,
        &mut skills.skills,
        &skills.current_scope,
        setup_access.authorized,
    )
    .await;
    apply_missing_binary_status(&mut skills.skills, &skills.current_scope);
    let (mut state, preferences_exist) = load_state()?;
    let known: BTreeSet<&str> = components
        .graph
        .features
        .iter()
        .map(|feature| feature.id.as_str())
        .collect();
    state
        .selected_features
        .retain(|feature| known.contains(feature.as_str()));
    state.manual_confirmations.retain(|component| {
        components.graph.component(component).is_some_and(|item| {
            matches!(item.probe, ProbeSpec::Manual { .. })
                && !item.setup.as_ref().is_some_and(setup_owns_readiness)
        })
    });
    state.configured_setups.retain(|component| {
        components
            .graph
            .component(component)
            .and_then(|item| item.setup.as_ref())
            .is_some_and(|setup| matches!(&setup.driver, SetupDriver::ModelRuntime { .. }))
    });
    let current_origin = engine_origin(&app).await?;
    if state.engine_origin.as_deref() != Some(current_origin.as_str()) {
        state.configured_setups.clear();
    }
    state.engine_origin = Some(current_origin);
    state.workspace_scope = Some(skills.current_scope.clone());
    state.completed = false;
    save_state(&state)?;
    Ok(OnboardingCatalog {
        components,
        skills: skills.skills,
        current_scope: skills.current_scope,
        selected_features: state.selected_features.into_iter().collect(),
        preferences_exist,
        setup_access,
    })
}

pub async fn complete_onboarding(app: &AppHandle) -> Result<(), String> {
    let catalog = get_onboarding_catalog(app.clone()).await?;
    let plan = plan_onboarding(
        app.clone(),
        OnboardingSelectionRequest {
            features: catalog.selected_features.clone(),
        },
    )
    .await?;
    if !plan.ready {
        return Err(format!(
            "Setup still has {} capability requirement{} to complete",
            plan.blocking_count,
            if plan.blocking_count == 1 { "" } else { "s" }
        ));
    }
    let blockers = catalog
        .skills
        .iter()
        .filter(|skill| {
            skill
                .installed_scopes
                .iter()
                .any(|scope| scope == &catalog.current_scope)
                && skill
                    .setup
                    .as_ref()
                    .is_some_and(|setup| setup.required && !setup.ready)
        })
        .map(|skill| skill.name.as_str())
        .collect::<Vec<_>>();
    if !blockers.is_empty() {
        let shown = blockers
            .iter()
            .take(5)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let remainder = blockers.len().saturating_sub(5);
        return Err(if remainder == 0 {
            format!("Complete setup for installed skills: {shown}")
        } else {
            format!("Complete setup for installed skills: {shown}, and {remainder} more")
        });
    }

    let (mut state, _) = load_state()?;
    state.engine_origin = Some(engine_origin(app).await?);
    state.workspace_scope = Some(catalog.current_scope);
    state.completed = true;
    save_state(&state)
}

fn apply_missing_binary_status(skills: &mut [SkillCatalogItem], current_scope: &str) {
    for skill in skills {
        if skill
            .installed_scopes
            .iter()
            .any(|scope| scope == current_scope)
            && !skill.missing_bins.is_empty()
        {
            skill.setup = Some(SkillSetupStatus {
                required: true,
                ready: false,
                missing: skill.missing_bins.clone(),
                detail: "Required programs are not installed on this Magician server".to_string(),
            });
        }
    }
}

fn auth_is_hard_requirement(requirement: &str) -> bool {
    matches!(requirement, "required" | "at_least_one")
}

fn managed_skill_auth_bot(skill: &SkillCatalogItem) -> Option<String> {
    let auth = skill.auth.as_ref()?;
    if !auth_is_hard_requirement(&auth.requirement) {
        return None;
    }
    managed_bot_name(auth.setup.as_ref()?).ok()
}

fn managed_bot_name(setup: &ResolvedSetup) -> Result<String, String> {
    let SetupDriver::ManagedBot { bot, .. } = &setup.driver else {
        return Err("This setup does not use a managed account worker".to_string());
    };
    let bot_name = render_template(bot, setup.profile.as_deref())?;
    validate_skill_name(&bot_name)?;
    Ok(bot_name)
}

async fn enrich_skill_profile_status(
    app: &AppHandle,
    skills: &mut [SkillCatalogItem],
    current_scope: &str,
    setup_authorized: bool,
) {
    let mut managed = BTreeMap::<String, Vec<usize>>::new();
    let mut oauth = Vec::<(usize, String)>::new();
    for (index, skill) in skills.iter_mut().enumerate() {
        if !skill
            .installed_scopes
            .iter()
            .any(|scope| scope == current_scope)
        {
            continue;
        }
        let Some(auth) = skill
            .auth
            .as_ref()
            .filter(|auth| auth_is_hard_requirement(&auth.requirement))
        else {
            continue;
        };
        match auth.setup.as_ref().map(|setup| &setup.driver) {
            Some(SetupDriver::GovernedOauth) => {
                if setup_authorized {
                    oauth.push((index, skill.name.clone()));
                } else {
                    skill.setup = Some(SkillSetupStatus {
                        required: true,
                        ready: false,
                        missing: Vec::new(),
                        detail: "Setup access is required to verify this account login".to_string(),
                    });
                }
            },
            Some(SetupDriver::ManagedBot { .. }) => {
                if let Some(bot) = managed_skill_auth_bot(skill) {
                    managed.entry(bot).or_default().push(index);
                }
            },
            Some(SetupDriver::ConfigurationFile { .. })
            | Some(SetupDriver::ModelRuntime { .. })
            | Some(SetupDriver::BrowserExtension { .. })
            | Some(SetupDriver::CuaDriver { .. })
            | None => {
                skill.setup = Some(SkillSetupStatus {
                    required: true,
                    ready: false,
                    missing: Vec::new(),
                    detail: if auth.has_login {
                        "This provider login has not been completed".to_string()
                    } else {
                        "This provider does not declare a verifiable managed login".to_string()
                    },
                });
            },
        }
    }

    let checks = futures_util::future::join_all(managed.keys().map(|bot| async move {
        let path = format!("/api/magician/v2/bots/{bot}/auth");
        (bot.clone(), get_json::<serde_json::Value>(app, &path).await)
    }))
    .await;
    for (bot, result) in checks {
        let Some(indices) = managed.get(&bot) else {
            continue;
        };
        let setup = match result {
            Ok(payload) => {
                let auth = payload.get("auth").unwrap_or(&payload);
                let status = auth
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("error");
                let flow = auth
                    .get("flow_state")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("idle");
                let ready = status == "ok";
                let detail = auth
                    .get("detail")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        if ready {
                            "Account login is active".to_string()
                        } else if flow == "active" || flow == "queued" {
                            format!("Account login is {flow}")
                        } else {
                            format!("Account login status: {status}")
                        }
                    });
                SkillSetupStatus {
                    required: true,
                    ready,
                    missing: Vec::new(),
                    detail,
                }
            },
            Err(error) => SkillSetupStatus {
                required: true,
                ready: false,
                missing: Vec::new(),
                detail: error,
            },
        };
        for index in indices {
            skills[*index].setup = Some(setup.clone());
        }
    }

    if oauth.is_empty() {
        return;
    }
    let origin = match engine_origin(app).await {
        Ok(origin) => origin,
        Err(error) => {
            for (index, _) in oauth {
                skills[index].setup = Some(SkillSetupStatus {
                    required: true,
                    ready: false,
                    missing: Vec::new(),
                    detail: error.clone(),
                });
            }
            return;
        },
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            for (index, _) in oauth {
                skills[index].setup = Some(SkillSetupStatus {
                    required: true,
                    ready: false,
                    missing: Vec::new(),
                    detail: format!("Failed to create account status client: {error}"),
                });
            }
            return;
        },
    };
    let statuses = futures_util::future::join_all(oauth.iter().map(|(_, name)| {
        let url = format!("{origin}/api/magician/v2/skills/catalog/{name}/oauth/status");
        admin_send::<SkillOAuthStatusResponse>(app, client.get(url))
    }))
    .await;
    for ((index, _), status) in oauth.into_iter().zip(statuses) {
        skills[index].setup = Some(match status {
            Ok(status) => SkillSetupStatus {
                required: true,
                ready: status.ready,
                missing: Vec::new(),
                detail: status.detail,
            },
            Err(error) => SkillSetupStatus {
                required: true,
                ready: false,
                missing: Vec::new(),
                detail: error,
            },
        });
    }
}

async fn enrich_skill_setup_status(
    app: &AppHandle,
    skills: &mut [SkillCatalogItem],
    current_scope: &str,
    setup_authorized: bool,
) {
    struct SkillSetupCheck {
        index: usize,
        name: String,
        required_keys: BTreeSet<String>,
        required_env: Vec<String>,
        secret_refs: Vec<String>,
        at_least_one_secret: bool,
    }

    let mut checks = Vec::new();
    for (index, skill) in skills.iter().enumerate() {
        if !skill
            .installed_scopes
            .iter()
            .any(|scope| scope == current_scope)
        {
            continue;
        }
        let mut required_keys: BTreeSet<String> = skill.requires_env.iter().cloned().collect();
        let mut secret_refs = Vec::new();
        let mut at_least_one_secret = false;
        if let Some(auth) = skill
            .auth
            .as_ref()
            .filter(|auth| auth.kind == "secrets" && auth_is_hard_requirement(&auth.requirement))
        {
            secret_refs = auth.secret_refs.clone();
            at_least_one_secret = auth.requirement == "at_least_one";
            required_keys.extend(auth.secret_refs.iter().cloned());
        }
        if required_keys.is_empty() {
            continue;
        }
        checks.push(SkillSetupCheck {
            index,
            name: skill.name.clone(),
            required_keys,
            required_env: skill.requires_env.clone(),
            secret_refs,
            at_least_one_secret,
        });
    }

    if !setup_authorized {
        for check in checks {
            skills[check.index].setup = Some(SkillSetupStatus {
                required: true,
                ready: false,
                missing: check.required_keys.into_iter().collect(),
                detail: "Setup access is required to verify write-only credentials".to_string(),
            });
        }
        return;
    }

    let origin = match engine_origin(app).await {
        Ok(origin) => origin,
        Err(error) => {
            for check in checks {
                skills[check.index].setup = Some(SkillSetupStatus {
                    required: true,
                    ready: false,
                    missing: check.required_keys.into_iter().collect(),
                    detail: error.clone(),
                });
            }
            return;
        },
    };
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            for check in checks {
                skills[check.index].setup = Some(SkillSetupStatus {
                    required: true,
                    ready: false,
                    missing: check.required_keys.into_iter().collect(),
                    detail: format!("Failed to create skill setup client: {error}"),
                });
            }
            return;
        },
    };
    let scope_query = current_scope.replace('/', "%2F");
    let statuses = futures_util::future::join_all(checks.iter().map(|check| {
        let url = format!(
            "{origin}/api/magician/v2/skills/catalog/{}/env?scope={scope_query}",
            check.name
        );
        admin_send::<SkillEnvStatus>(app, client.get(url))
    }))
    .await;

    for (check, status) in checks.into_iter().zip(statuses) {
        let skill = &mut skills[check.index];
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                skill.setup = Some(SkillSetupStatus {
                    required: true,
                    ready: false,
                    missing: check.required_keys.into_iter().collect(),
                    detail: error,
                });
                continue;
            },
        };
        let set: BTreeSet<String> = status
            .keys
            .into_iter()
            .filter(|key| key.set)
            .map(|key| key.key)
            .collect();
        let all_missing: Vec<String> = check
            .required_keys
            .iter()
            .filter(|key| !set.contains(*key))
            .cloned()
            .collect();
        let required_env_ready = check.required_env.iter().all(|key| set.contains(key));
        let has_any_secret = check.secret_refs.iter().any(|key| set.contains(key));
        let ready = if check.at_least_one_secret {
            required_env_ready && has_any_secret
        } else {
            all_missing.is_empty()
        };
        let missing = if check.at_least_one_secret {
            check
                .required_env
                .iter()
                .filter(|key| !set.contains(*key))
                .cloned()
                .chain(
                    (!has_any_secret)
                        .then_some(check.secret_refs)
                        .into_iter()
                        .flatten(),
                )
                .collect()
        } else {
            all_missing
        };
        skill.setup = Some(SkillSetupStatus {
            required: true,
            ready,
            detail: if ready {
                "Required credentials are configured".to_string()
            } else if check.at_least_one_secret {
                "At least one credential must be configured".to_string()
            } else {
                "Required credentials are missing".to_string()
            },
            missing,
        });
    }
}

#[tauri::command]
pub async fn plan_onboarding(
    app: AppHandle,
    selection: OnboardingSelectionRequest,
) -> Result<OnboardingPlan, String> {
    let mut features = selection.features;
    features.sort();
    features.dedup();
    let response: ComponentPlanResponse = post_json(
        &app,
        "/api/magician/v2/components/plan",
        &serde_json::json!({"features": features}),
    )
    .await?;
    let (mut state, _) = load_state()?;
    state.selected_features = features.iter().cloned().collect();
    let mut plan = plan_view(features, response, &state)?;
    apply_setup_status(&app, &mut plan).await;
    save_state(&state)?;
    Ok(plan)
}

#[tauri::command]
pub async fn set_onboarding_confirmation(
    app: AppHandle,
    request: OnboardingConfirmationRequest,
) -> Result<OnboardingPlan, String> {
    let catalog: ComponentCatalogSnapshot =
        get_json(&app, "/api/magician/v2/components/catalog").await?;
    let component = catalog
        .graph
        .component(&request.component_id)
        .ok_or_else(|| format!("Unknown component {}", request.component_id))?;
    if !matches!(component.probe, ProbeSpec::Manual { .. }) {
        return Err(format!(
            "{} has a machine-verifiable probe and cannot be confirmed manually",
            component.name
        ));
    }
    if component.setup.as_ref().is_some_and(setup_owns_readiness) {
        return Err(format!(
            "{} must pass its managed setup check",
            component.name
        ));
    }
    let (mut state, _) = load_state()?;
    if request.confirmed {
        state
            .manual_confirmations
            .insert(request.component_id.clone());
    } else {
        state.manual_confirmations.remove(&request.component_id);
    }
    save_state(&state)?;
    plan_onboarding(
        app,
        OnboardingSelectionRequest {
            features: state.selected_features.into_iter().collect(),
        },
    )
    .await
}

#[tauri::command]
pub async fn save_onboarding_secret(
    app: AppHandle,
    request: OnboardingSecretRequest,
) -> Result<OnboardingPlan, String> {
    if request.value.trim().is_empty() {
        return Err("Credential value cannot be empty".to_string());
    }
    let catalog: ComponentCatalogSnapshot =
        get_json(&app, "/api/magician/v2/components/catalog").await?;
    let mut declared_bots = BTreeSet::new();
    let mut declared = false;
    for component in &catalog.graph.components {
        let InstallAction::Manual { secrets, .. } = &component.install else {
            continue;
        };
        for prompt in secrets
            .iter()
            .filter(|prompt| prompt.variable == request.key)
        {
            declared = true;
            if let Some(bot) = prompt.bot.as_ref() {
                declared_bots.insert(bot.clone());
            }
        }
    }
    if !declared {
        return Err("This credential is not declared by the component catalog".to_string());
    }
    if declared_bots.len() > 1 {
        return Err("This credential has conflicting component destinations".to_string());
    }
    let (state, _) = load_state()?;
    let status: RuntimeEnvStatus = get_json(&app, "/api/magician/v2/runtime/env").await?;
    let file = if status.active_mode == "development" {
        ".env.development"
    } else {
        ".env"
    };
    let url = format!("{}/api/magician/v2/runtime/env", engine_origin(&app).await?);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let key = request.key;
    let value = request.value;
    let updates = BTreeMap::from([(key.clone(), value.clone())]);
    let _: serde_json::Value = admin_send(
        &app,
        client.put(url).json(&serde_json::json!({
            "file": file,
            "updates": updates,
        })),
    )
    .await?;
    if let Some(bot_name) = declared_bots.into_iter().next() {
        validate_skill_name(&bot_name)?;
        let bot_url = format!(
            "{}/api/magician/v2/bots/{bot_name}/env",
            engine_origin(&app).await?
        );
        let bot_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| format!("Failed to create bot setup client: {error}"))?;
        let bot_updates = BTreeMap::from([(key.clone(), value)]);
        let _: serde_json::Value = admin_send(
            &app,
            bot_client.put(bot_url).json(&serde_json::json!({
                "updates": bot_updates,
            })),
        )
        .await?;
    }
    plan_onboarding(
        app,
        OnboardingSelectionRequest {
            features: state.selected_features.into_iter().collect(),
        },
    )
    .await
}

#[tauri::command]
pub async fn save_onboarding_setup_token(
    app: AppHandle,
    request: OnboardingSetupTokenRequest,
) -> Result<SetupAccess, String> {
    let token = Zeroizing::new(request.token.trim().to_string());
    if !valid_setup_token(&token) {
        return Err("The setup token is empty or invalid".to_string());
    }
    check_setup_access_with_token(&app, &token).await?;
    let origin = engine_origin(&app).await?;
    save_setup_token(&origin, &token).await?;
    Ok(SetupAccess {
        authorized: true,
        token_stored: true,
        local_bootstrap: is_loopback_origin(&origin),
        unavailable_reason: None,
    })
}

#[tauri::command]
pub async fn clear_onboarding_setup_token(app: AppHandle) -> Result<SetupAccess, String> {
    let origin = engine_origin(&app).await?;
    delete_setup_token(&origin).await?;
    Ok(SetupAccess {
        authorized: false,
        token_stored: false,
        local_bootstrap: is_loopback_origin(&origin),
        unavailable_reason: None,
    })
}

#[tauri::command]
pub async fn save_onboarding_configuration_file(
    app: AppHandle,
    request: OnboardingConfigurationFileRequest,
) -> Result<OnboardingPlan, String> {
    let component_id = validate_component_id(&request.component_id)?;
    let catalog: ComponentCatalogSnapshot =
        get_json(&app, "/api/magician/v2/components/catalog").await?;
    let component = catalog
        .graph
        .component(component_id)
        .ok_or_else(|| "This component is not in the connected Magician catalog".to_string())?;
    let Some(ResolvedSetup {
        driver: SetupDriver::ConfigurationFile { max_bytes, .. },
        ..
    }) = component.setup.as_ref()
    else {
        return Err("This component does not accept a configuration file".to_string());
    };
    if request.content.is_empty() || request.content.len() > *max_bytes {
        return Err("Configuration file is empty or too large".to_string());
    }
    let url = format!(
        "{}/api/magician/v2/components/{component_id}/configuration-file",
        engine_origin(&app).await?,
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let _: serde_json::Value = admin_send(
        &app,
        client
            .put(url)
            .json(&serde_json::json!({"content": request.content})),
    )
    .await?;
    let (state, _) = load_state()?;
    plan_onboarding(
        app,
        OnboardingSelectionRequest {
            features: state.selected_features.into_iter().collect(),
        },
    )
    .await
}

fn validate_component_id(component_id: &str) -> Result<&str, String> {
    validate_skill_name(component_id).map_err(|_| "The component id is invalid".to_string())
}

fn validate_skill_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("The skill name is invalid".to_string());
    }
    Ok(name)
}

async fn current_skill_scope(app: &AppHandle) -> Result<String, String> {
    get_json::<SkillCatalogResponse>(app, "/api/magician/v2/skills/catalog")
        .await
        .map(|catalog| catalog.current_scope)
}

#[tauri::command]
pub async fn install_onboarding_skill(
    app: AppHandle,
    request: OnboardingSkillRequest,
) -> Result<OnboardingCatalog, String> {
    let name = validate_skill_name(&request.name)?;
    let scope = current_skill_scope(&app).await?;
    let url = format!(
        "{}/api/magician/v2/skills/install",
        engine_origin(&app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let _: serde_json::Value = admin_send(
        &app,
        client.post(url).json(&serde_json::json!({
            "source": format!("skillshub:{name}"),
            "target": {"workspaces": [scope]},
        })),
    )
    .await?;
    get_onboarding_catalog(app).await
}

#[tauri::command]
pub async fn uninstall_onboarding_skill(
    app: AppHandle,
    request: OnboardingSkillRequest,
) -> Result<OnboardingCatalog, String> {
    let name = validate_skill_name(&request.name)?;
    let scope = current_skill_scope(&app).await?;
    let url = format!(
        "{}/api/magician/v2/skills/{name}/uninstall",
        engine_origin(&app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let _: serde_json::Value = admin_send(
        &app,
        client.post(url).json(&serde_json::json!({
            "workspaces": [scope],
            "purge": false,
        })),
    )
    .await?;
    get_onboarding_catalog(app).await
}

#[tauri::command]
pub async fn save_onboarding_skill_secret(
    app: AppHandle,
    skill: OnboardingSkillRequest,
    request: OnboardingSecretRequest,
) -> Result<OnboardingCatalog, String> {
    let name = validate_skill_name(&skill.name)?;
    if request.value.trim().is_empty() {
        return Err("Credential value cannot be empty".to_string());
    }
    let scope = current_skill_scope(&app).await?;
    let url = format!(
        "{}/api/magician/v2/skills/catalog/{name}/env",
        engine_origin(&app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    let updates = BTreeMap::from([(request.key, request.value)]);
    let _: serde_json::Value = admin_send(
        &app,
        client.post(url).json(&serde_json::json!({
            "workspaces": [scope],
            "updates": updates,
        })),
    )
    .await?;
    get_onboarding_catalog(app).await
}

#[tauri::command]
pub async fn start_onboarding_component_install(
    app: AppHandle,
    request: OnboardingComponentRequest,
) -> Result<ComponentInstallJob, String> {
    let component_id = validate_skill_name(&request.component_id)?;
    let url = format!(
        "{}/api/magician/v2/components/{component_id}/install",
        engine_origin(&app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    admin_send(&app, client.post(url)).await
}

#[tauri::command]
pub async fn get_onboarding_component_install(
    app: AppHandle,
    job_id: String,
) -> Result<ComponentInstallJob, String> {
    let job_id = validate_job_id(&job_id)?;
    let url = format!(
        "{}/api/magician/v2/components/installations/{job_id}",
        engine_origin(&app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    admin_send(&app, client.get(url)).await
}

fn validate_job_id(job_id: &str) -> Result<&str, String> {
    let job_id = job_id.trim();
    if job_id.len() != 36
        || !job_id
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-')
    {
        return Err("The installation job id is invalid".to_string());
    }
    Ok(job_id)
}

async fn component_setup(app: &AppHandle, component_id: &str) -> Result<ResolvedSetup, String> {
    let component_id = validate_component_id(component_id)?;
    let catalog =
        get_json::<ComponentCatalogSnapshot>(app, "/api/magician/v2/components/catalog").await?;
    catalog
        .graph
        .component(component_id)
        .and_then(|component| component.setup.clone())
        .ok_or_else(|| "This component has no managed setup flow".to_string())
}

fn model_runtime_parts(
    setup: &ResolvedSetup,
) -> Result<(&str, &str, &str, &[SetupModeChoice]), String> {
    let SetupDriver::ModelRuntime {
        privacy_path,
        generation_path,
        local_feature,
        modes,
    } = &setup.driver
    else {
        return Err("This component does not configure processing locality".to_string());
    };
    Ok((privacy_path, generation_path, local_feature, modes))
}

#[tauri::command]
pub async fn get_onboarding_model_runtime(
    app: AppHandle,
    request: OnboardingComponentRequest,
) -> Result<ModelRuntimeSnapshot, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    let (_, generation_path, local_feature, modes) = model_runtime_parts(&setup)?;
    let settings = get_json::<LocalGenerationSettingsSnapshot>(&app, generation_path).await?;
    let (state, _) = load_state()?;
    let current_origin = engine_origin(&app).await?;
    Ok(ModelRuntimeSnapshot {
        component_id: request.component_id.clone(),
        modes: modes.to_vec(),
        local_feature: local_feature.to_string(),
        mode: settings.processing_mode,
        selected: settings.selected,
        recommended: settings.recommended,
        models: settings.models,
        warnings: settings.warnings,
        configured: state.engine_origin.as_deref() == Some(current_origin.as_str())
            && state.configured_setups.contains(&request.component_id),
    })
}

#[tauri::command]
pub async fn configure_onboarding_model_runtime(
    app: AppHandle,
    request: ConfigureModelRuntimeRequest,
) -> Result<ModelRuntimeSnapshot, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    let (privacy_path, generation_path, local_feature, modes) = model_runtime_parts(&setup)?;
    let mode = modes
        .iter()
        .find(|candidate| candidate.id == request.mode)
        .ok_or_else(|| "Choose one of the declared processing modes".to_string())?;
    let current = get_json::<LocalGenerationSettingsSnapshot>(&app, generation_path).await?;
    let selected_model = if mode.local_generation {
        let selected = request
            .selected_model
            .as_deref()
            .map(str::trim)
            .filter(|selected| !selected.is_empty())
            .ok_or_else(|| "Choose a local generation model".to_string())?;
        if !current.models.iter().any(|model| model.id == selected) {
            return Err("Choose a model from the Magician local-generation catalog".to_string());
        }
        Some(selected.to_string())
    } else {
        None
    };

    let base = engine_origin(&app).await?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| format!("Failed to create setup client: {error}"))?;
    if let Some(selected) = selected_model.as_ref() {
        let _: serde_json::Value = admin_send(
            &app,
            client
                .put(format!("{base}{generation_path}"))
                .json(&serde_json::json!({
                    "selected": selected,
                    "reload_ollama": false
                })),
        )
        .await?;
    }
    let _: serde_json::Value = admin_send(
        &app,
        client
            .put(format!("{base}{privacy_path}"))
            .json(&serde_json::json!({"processing": {"mode": mode.id.as_str()}})),
    )
    .await?;

    let (mut state, _) = load_state()?;
    state.engine_origin = Some(base);
    state.configured_setups.insert(request.component_id.clone());
    if mode.local_generation {
        state.selected_features.insert(local_feature.to_string());
    } else {
        state.selected_features.remove(local_feature);
    }
    state.completed = false;
    save_state(&state)?;
    get_onboarding_model_runtime(
        app,
        OnboardingComponentRequest {
            component_id: request.component_id,
        },
    )
    .await
}

#[tauri::command]
pub async fn get_onboarding_harnesses(app: AppHandle) -> Result<OnboardingHarnessCatalog, String> {
    onboarding_harness_catalog(&app).await
}

async fn onboarding_harness_catalog(app: &AppHandle) -> Result<OnboardingHarnessCatalog, String> {
    let (coding, plane) = tokio::try_join!(
        get_json::<CodingProfilesResponse>(app, "/api/magician/v2/coding/profiles"),
        get_json::<PlaneEnginesResponse>(app, "/api/magician/v2/plane/engines"),
    )?;
    let definitions = magician_components::setup::harnesses()?;
    let harnesses = definitions
        .into_iter()
        .map(|definition| {
            let coding = coding
                .profiles
                .iter()
                .find(|profile| profile.engine == definition.coding_engine);
            let plane_engine = plane
                .engines
                .iter()
                .find(|engine| engine.name == definition.plane_engine);
            OnboardingHarness {
                id: definition.id,
                order: definition.order,
                label: definition.label,
                binary: definition.binary,
                coding_engine: definition.coding_engine,
                plane_engine: definition.plane_engine,
                config_key: definition.config_key,
                install_url: definition.install_url,
                install_hint: definition.install_hint,
                surfaces: definition.surfaces,
                readiness: coding
                    .map(|profile| profile.readiness.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                reason: coding
                    .and_then(|profile| profile.reason.clone())
                    .unwrap_or_else(|| {
                        "The connected Magician did not report this harness".to_string()
                    }),
                version: coding.and_then(|profile| profile.version.clone()),
                selectable: coding.is_some_and(|profile| profile.selectable)
                    && plane_engine.is_some_and(|engine| engine.installed),
                installed: plane_engine.is_some_and(|engine| engine.installed),
                models: plane_engine
                    .map(|engine| engine.models.clone())
                    .filter(|models| !models.is_empty())
                    .unwrap_or_else(|| vec!["default".to_string()]),
            }
        })
        .collect();
    Ok(OnboardingHarnessCatalog {
        harnesses,
        chat_current: plane.chat_current,
        run_current: plane.current,
        chat_model: plane.chat_model,
        run_model: plane.run_model,
    })
}

#[tauri::command]
pub async fn refresh_onboarding_harness(
    app: AppHandle,
    request: RefreshOnboardingHarnessRequest,
) -> Result<OnboardingHarnessCatalog, String> {
    let definition = magician_components::setup::harnesses()?
        .into_iter()
        .find(|definition| definition.id == request.harness_id)
        .ok_or_else(|| "This harness is not in the reviewed setup catalog".to_string())?;
    let _: serde_json::Value =
        post_json(&app, &definition.refresh_path, &serde_json::json!({})).await?;
    onboarding_harness_catalog(&app).await
}

#[tauri::command]
pub async fn configure_onboarding_harness(
    app: AppHandle,
    request: ConfigureOnboardingHarnessRequest,
) -> Result<OnboardingHarnessCatalog, String> {
    if !matches!(request.target.as_str(), "chat" | "run") {
        return Err("Harness target must be chat or run".to_string());
    }
    let model = request.model.trim();
    if model.is_empty() || model.len() > 128 || model.chars().any(char::is_control) {
        return Err("Harness model is invalid".to_string());
    }
    if request.engine != "magician" {
        let snapshot = onboarding_harness_catalog(&app).await?;
        let harness = snapshot
            .harnesses
            .iter()
            .find(|harness| harness.plane_engine == request.engine)
            .ok_or_else(|| "This engine is not in the reviewed harness catalog".to_string())?;
        if !harness.selectable {
            return Err(format!(
                "{} is not Ready on the connected Magician host: {}",
                harness.label, harness.reason
            ));
        }
        if !harness.models.iter().any(|candidate| candidate == model) {
            return Err("This model is not offered by the selected harness".to_string());
        }
    } else if model != "default" {
        return Err("The built-in Magician engine uses its configured model routing".to_string());
    }
    let path = if request.target == "chat" {
        "/api/magician/v2/plane/chat-engine"
    } else {
        "/api/magician/v2/plane/engine"
    };
    let _: serde_json::Value = put_json(
        &app,
        path,
        &serde_json::json!({
            "harness_engine": request.engine,
            "harness_model": model,
        }),
    )
    .await?;
    onboarding_harness_catalog(&app).await
}

fn copy_unpacked_extension(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir_all(destination).map_err(|error| {
        format!(
            "Could not create the browser extension folder {}: {error}",
            destination.display()
        )
    })?;
    for entry in std::fs::read_dir(source)
        .map_err(|error| format!("Could not read the bundled browser extension: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("Could not read the bundled browser extension: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect the bundled browser extension: {error}"))?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_unpacked_extension(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target).map_err(|error| {
                format!(
                    "Could not copy browser extension file {}: {error}",
                    entry.path().display()
                )
            })?;
        } else {
            return Err("The bundled browser extension contains an unsupported link".to_string());
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn prepare_onboarding_browser_extension(
    app: AppHandle,
    request: OnboardingComponentRequest,
) -> Result<BrowserExtensionPreparation, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    let SetupDriver::BrowserExtension {
        bundle_resource,
        management_url,
        ..
    } = &setup.driver
    else {
        return Err("This component does not install a browser extension".to_string());
    };
    let bundled = app
        .path()
        .resource_dir()
        .map_err(|error| format!("Could not locate Desktop resources: {error}"))?
        .join(bundle_resource);
    let source = if bundled.join("manifest.json").is_file() {
        bundled
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("magicutor/extension")
    };
    if !source.join("manifest.json").is_file() {
        return Err("The Magican browser extension was not bundled with Desktop".to_string());
    }
    let destination = app
        .path()
        .download_dir()
        .map_err(|error| format!("Could not locate the Downloads folder: {error}"))?
        .join(format!(
            "Magican Browser Extension v{}",
            env!("CARGO_PKG_VERSION")
        ));
    copy_unpacked_extension(&source, &destination)?;
    if !destination.join("manifest.json").is_file() {
        return Err("The copied Magican browser extension is incomplete".to_string());
    }
    open::that(&destination)
        .map_err(|error| format!("Could not reveal the browser extension folder: {error}"))?;
    open::that(management_url)
        .map_err(|error| format!("Could not open Chrome's extension page: {error}"))?;
    Ok(BrowserExtensionPreparation {
        source_path: destination.display().to_string(),
        management_url: management_url.clone(),
    })
}

#[tauri::command]
pub async fn install_onboarding_cua_driver(
    app: AppHandle,
    request: OnboardingComponentRequest,
) -> Result<CuaDriverSetupResult, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    if !matches!(setup.driver, SetupDriver::CuaDriver { .. }) {
        return Err("This component does not install CuaDriver".to_string());
    }
    // The engine may be remote; what runs on this desktop comes from the
    // catalog compiled into Desktop, never from the engine's copy of it.
    let release = magician_components::setup::cua_driver_release()?;
    if !runtime_core::cua::has_desktop_session() {
        return Err(
            "CuaDriver must be installed from Magican Desktop in the signed-in graphical session"
                .to_string(),
        );
    }
    let installer_output = crate::cua_setup::install(&release).await?;
    let permission_error = crate::permissions::request_cua_driver_permission()
        .await
        .err();
    let readiness = crate::permissions::cua_setup_readiness().await;
    let detail = if readiness.ready {
        readiness.detail
    } else {
        permission_error.unwrap_or(readiness.detail)
    };
    Ok(CuaDriverSetupResult {
        installed: readiness.installed,
        ready: readiness.ready,
        detail,
        installer_output,
    })
}

async fn installed_skill_for_auth(app: &AppHandle, name: &str) -> Result<SkillCatalogItem, String> {
    let name = validate_skill_name(name)?;
    let catalog = get_json::<SkillCatalogResponse>(app, "/api/magician/v2/skills/catalog").await?;
    let skill = catalog
        .skills
        .iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| "The skill is not in this Magician catalog".to_string())?;
    if !skill
        .installed_scopes
        .iter()
        .any(|scope| scope == &catalog.current_scope)
    {
        return Err(
            "Install the skill in this workspace before connecting its account".to_string(),
        );
    }
    Ok(skill.clone())
}

async fn skill_auth_setup(app: &AppHandle, name: &str) -> Result<ResolvedSetup, String> {
    let skill = installed_skill_for_auth(app, name).await?;
    skill
        .auth
        .and_then(|auth| auth.setup)
        .ok_or_else(|| "This skill does not expose a Desktop-managed account login".to_string())
}

fn oauth_skill_snapshot(name: String, status: SkillOAuthStatusResponse) -> BotPairingSnapshot {
    BotPairingSnapshot {
        component_id: name.clone(),
        bot_name: name,
        flow_state: if status.ready {
            "completed".to_string()
        } else if status.status == "authenticating" {
            "active".to_string()
        } else {
            "idle".to_string()
        },
        status: status.status,
        detail: Some(status.detail),
        ready: status.ready,
        log: String::new(),
        qr_data_url: None,
        login_url: None,
    }
}

async fn get_skill_oauth_snapshot(
    app: &AppHandle,
    name: String,
) -> Result<BotPairingSnapshot, String> {
    let origin = engine_origin(app).await?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create account status client: {error}"))?;
    let status = admin_send::<SkillOAuthStatusResponse>(
        app,
        client.get(format!(
            "{origin}/api/magician/v2/skills/catalog/{name}/oauth/status"
        )),
    )
    .await?;
    Ok(oauth_skill_snapshot(name, status))
}

fn validate_oauth_authorization_url(target: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(target)
        .map_err(|_| "The account provider returned an invalid authorization URL".to_string())?;
    let loopback_http = parsed.scheme() == "http"
        && parsed.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
    if !(parsed.scheme() == "https" || loopback_http)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none()
    {
        return Err("The account provider returned an unsafe authorization URL".to_string());
    }
    Ok(())
}

fn open_oauth_authorization_url(target: Zeroizing<String>) -> Result<(), String> {
    validate_oauth_authorization_url(target.as_str())?;
    open::that(target.as_str())
        .map_err(|error| format!("Failed to open account authorization: {error}"))
}

async fn start_managed_bot_auth(
    app: &AppHandle,
    subject_id: String,
    setup: &ResolvedSetup,
) -> Result<BotPairingSnapshot, String> {
    let SetupDriver::ManagedBot { start, .. } = &setup.driver else {
        return Err("This setup does not use a managed account worker".to_string());
    };
    let bot_name = managed_bot_name(setup)?;
    let origin = engine_origin(app).await?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| format!("Failed to create account login client: {error}"))?;
    for action in start {
        let suffix = match action {
            ManagedBotStart::Bot => "start",
            ManagedBotStart::Auth => "auth/start",
        };
        let response = crate::magician_auth::authorize(
            client.post(format!("{origin}/api/magician/v2/bots/{bot_name}/{suffix}")),
        )
        .send()
        .await
        .map_err(|error| format!("Could not start account setup: {error}"))?;
        let _: serde_json::Value = decode_response(response).await?;
    }
    bot_pairing_snapshot(app, subject_id, setup).await
}

async fn configure_managed_setup(
    app: &AppHandle,
    setup: &ResolvedSetup,
    values: BTreeMap<String, String>,
) -> Result<(), String> {
    let SetupDriver::ManagedBot {
        fields, fixed_env, ..
    } = &setup.driver
    else {
        return Err("This setup does not accept account fields".to_string());
    };
    if values
        .keys()
        .any(|id| !fields.iter().any(|field| field.id == *id))
    {
        return Err("The setup request contains an undeclared field".to_string());
    }
    let mut updates = BTreeMap::new();
    for field in fields {
        let raw_value = values
            .get(&field.id)
            .map(String::as_str)
            .unwrap_or_default();
        let value = if field.kind == SetupFieldKind::Password {
            raw_value
        } else {
            raw_value.trim()
        };
        if field.required && value.is_empty() {
            return Err(format!("{} is required", field.label));
        }
        if value.is_empty() {
            continue;
        }
        if value.len() > 16 * 1024 || value.chars().any(char::is_control) {
            return Err(format!("{} is invalid", field.label));
        }
        if field.kind == SetupFieldKind::Email
            && (value.len() > 254
                || value.chars().any(char::is_whitespace)
                || value.split('@').count() != 2
                || value.starts_with('@')
                || value.ends_with('@'))
        {
            return Err(format!("Enter a valid value for {}", field.label));
        }
        updates.insert(field.env.clone(), value.to_string());
    }
    for (key, value) in fixed_env {
        updates.insert(
            key.clone(),
            render_template(value, setup.profile.as_deref())?,
        );
    }
    if updates.is_empty() {
        return Ok(());
    }
    let bot_name = managed_bot_name(setup)?;
    let url = format!(
        "{}/api/magician/v2/bots/{bot_name}/env",
        engine_origin(app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create account setup client: {error}"))?;
    let _: serde_json::Value = admin_send(
        app,
        client
            .put(url)
            .json(&serde_json::json!({"updates": updates})),
    )
    .await?;
    Ok(())
}

async fn submit_managed_auth_input(
    app: &AppHandle,
    subject_id: String,
    setup: &ResolvedSetup,
    input: String,
) -> Result<BotPairingSnapshot, String> {
    let SetupDriver::ManagedBot {
        input: Some(spec), ..
    } = &setup.driver
    else {
        return Err("This setup does not accept interactive account input".to_string());
    };
    let input = Zeroizing::new(if spec.kind == SetupFieldKind::Password {
        input
    } else {
        input.trim().to_string()
    });
    if input.is_empty() || input.len() > spec.max_bytes || input.chars().any(char::is_control) {
        return Err(format!("{} is empty or invalid", spec.label));
    }
    let bot_name = managed_bot_name(setup)?;
    let url = format!(
        "{}/api/magician/v2/bots/{bot_name}/auth/input",
        engine_origin(app).await?
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| format!("Failed to create account login client: {error}"))?;
    let _: serde_json::Value = admin_send(
        app,
        client
            .post(url)
            .json(&serde_json::json!({"input": input.as_str()})),
    )
    .await?;
    bot_pairing_snapshot(app, subject_id, setup).await
}

#[tauri::command]
pub async fn start_onboarding_bot_pairing(
    app: AppHandle,
    request: OnboardingComponentRequest,
) -> Result<BotPairingSnapshot, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    start_managed_bot_auth(&app, request.component_id, &setup).await
}

#[tauri::command]
pub async fn submit_onboarding_bot_auth_input(
    app: AppHandle,
    request: OnboardingAuthInputRequest,
) -> Result<BotPairingSnapshot, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    submit_managed_auth_input(&app, request.component_id, &setup, request.input).await
}

#[tauri::command]
pub async fn get_onboarding_bot_pairing(
    app: AppHandle,
    request: OnboardingComponentRequest,
) -> Result<BotPairingSnapshot, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    bot_pairing_snapshot(&app, request.component_id, &setup).await
}

#[tauri::command]
pub async fn configure_onboarding_component_auth(
    app: AppHandle,
    request: OnboardingComponentAuthConfigureRequest,
) -> Result<BotPairingSnapshot, String> {
    let setup = component_setup(&app, &request.component_id).await?;
    configure_managed_setup(&app, &setup, request.values).await?;
    bot_pairing_snapshot(&app, request.component_id, &setup).await
}

#[tauri::command]
pub async fn start_onboarding_skill_auth(
    app: AppHandle,
    request: OnboardingSkillRequest,
) -> Result<BotPairingSnapshot, String> {
    let skill = installed_skill_for_auth(&app, &request.name).await?;
    if skill
        .auth
        .as_ref()
        .and_then(|auth| auth.setup.as_ref())
        .is_some_and(|setup| matches!(&setup.driver, SetupDriver::GovernedOauth))
    {
        let name = validate_skill_name(&request.name)?.to_string();
        let origin = engine_origin(&app).await?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| format!("Failed to create account login client: {error}"))?;
        let response = admin_send::<SkillOAuthStartResponse>(
            &app,
            client.post(format!(
                "{origin}/api/magician/v2/skills/catalog/{name}/oauth/start"
            )),
        )
        .await?;
        open_oauth_authorization_url(Zeroizing::new(response.authorization_url))?;
        return get_skill_oauth_snapshot(&app, name).await;
    }
    let setup = skill
        .auth
        .and_then(|auth| auth.setup)
        .ok_or_else(|| "This skill does not expose a Desktop-managed account login".to_string())?;
    start_managed_bot_auth(&app, request.name, &setup).await
}

#[tauri::command]
pub async fn get_onboarding_skill_auth(
    app: AppHandle,
    request: OnboardingSkillRequest,
) -> Result<BotPairingSnapshot, String> {
    let skill = installed_skill_for_auth(&app, &request.name).await?;
    if skill
        .auth
        .as_ref()
        .and_then(|auth| auth.setup.as_ref())
        .is_some_and(|setup| matches!(&setup.driver, SetupDriver::GovernedOauth))
    {
        let name = validate_skill_name(&request.name)?.to_string();
        return get_skill_oauth_snapshot(&app, name).await;
    }
    let setup = skill
        .auth
        .and_then(|auth| auth.setup)
        .ok_or_else(|| "This skill does not expose a Desktop-managed account login".to_string())?;
    bot_pairing_snapshot(&app, request.name, &setup).await
}

#[tauri::command]
pub async fn configure_onboarding_skill_auth(
    app: AppHandle,
    request: OnboardingSkillAuthConfigureRequest,
) -> Result<BotPairingSnapshot, String> {
    let setup = skill_auth_setup(&app, &request.name).await?;
    configure_managed_setup(&app, &setup, request.values).await?;
    bot_pairing_snapshot(&app, request.name, &setup).await
}

#[tauri::command]
pub async fn submit_onboarding_skill_auth_input(
    app: AppHandle,
    request: OnboardingSkillAuthInputRequest,
) -> Result<BotPairingSnapshot, String> {
    let setup = skill_auth_setup(&app, &request.name).await?;
    submit_managed_auth_input(&app, request.name, &setup, request.input).await
}

async fn bot_pairing_snapshot(
    app: &AppHandle,
    subject_id: String,
    setup: &ResolvedSetup,
) -> Result<BotPairingSnapshot, String> {
    let bot_name = managed_bot_name(setup)?;
    let auth_path = format!("/api/magician/v2/bots/{bot_name}/auth");
    let logs_path = format!("/api/magician/v2/bots/{bot_name}/logs?limit=400");
    let (auth, logs, qr_data_url) = tokio::try_join!(
        get_json::<serde_json::Value>(&app, &auth_path),
        get_json::<serde_json::Value>(&app, &logs_path),
        get_optional_qr_data_url(&app, &bot_name),
    )?;
    let auth = auth.get("auth").unwrap_or(&auth);
    let status = auth
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let flow_state = auth
        .get("flow_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("idle")
        .to_string();
    let detail = auth
        .get("detail")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let log = logs
        .get("lines")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|line| line.get("line").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let login_url_hosts = match &setup.driver {
        SetupDriver::ManagedBot {
            login_url_hosts, ..
        } => login_url_hosts,
        _ => return Err("This setup does not use a managed account worker".to_string()),
    };
    let login_url = extract_login_url(&log, login_url_hosts);
    Ok(BotPairingSnapshot {
        component_id: subject_id,
        bot_name,
        ready: status == "ok",
        status,
        flow_state,
        detail,
        log,
        qr_data_url,
        login_url,
    })
}

fn extract_login_url(log: &str, allowed_hosts: &[String]) -> Option<String> {
    log.split_whitespace().find_map(|part| {
        let candidate = part.trim_matches(|character| {
            matches!(character, '"' | '\'' | ',' | '(' | ')' | '[' | ']')
        });
        let parsed = reqwest::Url::parse(candidate).ok()?;
        (parsed.scheme() == "https"
            && parsed
                .host_str()
                .is_some_and(|host| allowed_hosts.iter().any(|allowed| allowed == host)))
        .then(|| parsed.to_string())
    })
}

#[tauri::command]
pub fn open_onboarding_target(target: String) -> Result<(), String> {
    let parsed =
        reqwest::Url::parse(&target).map_err(|_| "Setup link must be a valid URL".to_string())?;
    if !matches!(
        parsed.scheme(),
        "https" | "http" | "chrome" | "x-apple.systempreferences"
    ) {
        return Err("Setup link uses an unsupported URL scheme".to_string());
    }
    open::that(&target).map_err(|error| format!("Failed to open setup link: {error}"))
}

fn plan_view(
    selected_features: Vec<String>,
    response: ComponentPlanResponse,
    state: &OnboardingState,
) -> Result<OnboardingPlan, String> {
    let mut step_ids = response.plan.install.clone();
    for component in response.graph.components.iter().rev().filter(|component| {
        component
            .setup
            .as_ref()
            .is_some_and(|setup| matches!(&setup.driver, SetupDriver::ModelRuntime { .. }))
    }) {
        if !step_ids.contains(&component.id) {
            step_ids.insert(0, component.id.clone());
        }
    }
    let mut steps = Vec::with_capacity(step_ids.len());
    for id in &step_ids {
        let component = response
            .graph
            .component(id)
            .ok_or_else(|| format!("Setup plan references unknown component {id}"))?;
        let observed = response
            .observed
            .get(id)
            .cloned()
            .unwrap_or_else(|| Observed::Unknown("not probed".to_string()));
        steps.push(step_view(
            component,
            observed,
            state.manual_confirmations.contains(id),
            state.configured_setups.contains(id),
        ));
    }
    let blocking_count = steps.iter().filter(|step| !step.ready).count()
        + response.plan.unsupported.len()
        + response.plan.unresolved.len();
    Ok(OnboardingPlan {
        selected_features,
        steps,
        unsupported: response.plan.unsupported,
        unresolved: response.plan.unresolved,
        left_off: response.plan.left_off,
        projected: response.projected,
        ready: blocking_count == 0,
        blocking_count,
        host: response.host,
    })
}

async fn apply_setup_status(app: &AppHandle, plan: &mut OnboardingPlan) {
    for step in &mut plan.steps {
        let Some(setup) = step.setup.as_ref() else {
            continue;
        };
        match &setup.driver {
            SetupDriver::ManagedBot { .. } => {
                let Ok(bot_name) = managed_bot_name(setup) else {
                    continue;
                };
                step.can_confirm = false;
                step.confirmed = false;
                let path = format!("/api/magician/v2/bots/{bot_name}/auth");
                match get_json::<serde_json::Value>(app, &path).await {
                    Ok(payload) => {
                        let auth = payload.get("auth").unwrap_or(&payload);
                        let status = auth
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        let detail = auth
                            .get("detail")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_else(|| {
                                if status == "ok" {
                                    "Account pairing is active"
                                } else {
                                    "Account pairing is required"
                                }
                            })
                            .to_string();
                        step.ready = status == "ok";
                        step.observed = if step.ready {
                            Observed::Present(detail)
                        } else {
                            Observed::Absent(detail)
                        };
                    },
                    Err(error) => {
                        step.ready = false;
                        step.observed = Observed::Unknown(error);
                    },
                }
            },
            SetupDriver::BrowserExtension {
                probe_path,
                probe_needle,
                ..
            } => {
                let port = app
                    .state::<crate::AppState>()
                    .config
                    .lock()
                    .await
                    .network
                    .magicutor_port;
                let url = format!("http://127.0.0.1:{port}{probe_path}");
                let result = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(std::time::Duration::from_secs(3))
                    .build()
                    .map_err(|error| error.to_string());
                step.can_confirm = false;
                step.confirmed = false;
                match result {
                    Ok(client) => match client.get(&url).send().await {
                        Ok(response) if response.status().is_success() => {
                            let body = response.text().await.unwrap_or_default();
                            step.ready = body.contains(probe_needle);
                            step.observed = if step.ready {
                                Observed::Present(
                                    "The browser extension is connected to this Desktop"
                                        .to_string(),
                                )
                            } else {
                                Observed::Absent(
                                    "Load the extension in Chrome and keep its service worker connected"
                                        .to_string(),
                                )
                            };
                        },
                        Ok(response) => {
                            step.ready = false;
                            step.observed = Observed::Absent(format!(
                                "Desktop Magicutor answered {}",
                                response.status()
                            ));
                        },
                        Err(_) => {
                            step.ready = false;
                            step.observed = Observed::Absent(
                                "Desktop Magicutor is not reachable on this machine".to_string(),
                            );
                        },
                    },
                    Err(error) => {
                        step.ready = false;
                        step.observed = Observed::Unknown(error);
                    },
                }
            },
            SetupDriver::CuaDriver { .. } => {
                step.can_confirm = false;
                step.confirmed = false;
                let readiness = crate::permissions::cua_setup_readiness().await;
                step.ready = readiness.ready;
                step.observed = if readiness.ready {
                    Observed::Present(readiness.detail)
                } else if readiness.installed {
                    Observed::Absent(readiness.detail)
                } else {
                    Observed::Absent(
                        "Install CuaDriver on this desktop, then start it and verify access"
                            .to_string(),
                    )
                };
            },
            SetupDriver::ModelRuntime { .. }
            | SetupDriver::GovernedOauth
            | SetupDriver::ConfigurationFile { .. } => {},
        }
    }
    plan.blocking_count = plan.steps.iter().filter(|step| !step.ready).count()
        + plan.unsupported.len()
        + plan.unresolved.len();
    plan.ready = plan.blocking_count == 0;
}

fn setup_owns_readiness(setup: &ResolvedSetup) -> bool {
    matches!(
        &setup.driver,
        SetupDriver::ManagedBot { .. }
            | SetupDriver::ModelRuntime { .. }
            | SetupDriver::BrowserExtension { .. }
            | SetupDriver::CuaDriver { .. }
    )
}

fn step_view(
    component: &Component,
    observed: Observed,
    confirmed: bool,
    configured: bool,
) -> OnboardingStep {
    let can_confirm = matches!(component.probe, ProbeSpec::Manual { .. })
        && !component.setup.as_ref().is_some_and(setup_owns_readiness);
    let configured_required = component
        .setup
        .as_ref()
        .is_some_and(|setup| matches!(&setup.driver, SetupDriver::ModelRuntime { .. }));
    let ready = (observed.is_present() && (!configured_required || configured))
        || (can_confirm && confirmed);
    OnboardingStep {
        id: component.id.clone(),
        name: component.name.clone(),
        required: component.required || component.core,
        cost: component.cost.clone(),
        pricing: component.pricing.label().to_string(),
        install: component.install.clone(),
        observed,
        ready,
        can_confirm,
        confirmed,
        setup: component.setup.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_components::{Pricing, SecretPrompt};

    fn component(probe: ProbeSpec) -> Component {
        Component {
            id: "paired".to_string(),
            name: "Paired account".to_string(),
            core: false,
            required: false,
            provides: Vec::new(),
            requires: Vec::new(),
            cost: String::new(),
            pricing: Pricing::Free,
            install: InstallAction::Manual {
                steps: vec!["Scan the QR code".to_string()],
                open: None,
                secrets: vec![SecretPrompt {
                    variable: "TOKEN".to_string(),
                    label: "Token".to_string(),
                    bot: None,
                }],
            },
            probe,
            setup: None,
            host: None,
        }
    }

    #[test]
    fn manual_steps_are_hard_gates_until_confirmed() {
        let item = component(ProbeSpec::Manual {
            hint: "scan".to_string(),
        });
        assert!(!step_view(&item, Observed::Unknown("scan".to_string()), false, false).ready);
        assert!(step_view(&item, Observed::Unknown("scan".to_string()), true, false).ready);
    }

    #[test]
    fn machine_probes_cannot_be_bypassed_by_confirmation() {
        let item = component(ProbeSpec::EnvKeyPresent {
            keys: vec!["TOKEN".to_string()],
            files: Vec::new(),
        });
        let step = step_view(
            &item,
            Observed::Absent("TOKEN is missing".to_string()),
            true,
            false,
        );
        assert!(!step.can_confirm);
        assert!(!step.ready);
    }

    #[test]
    fn managed_pairing_steps_cannot_be_confirmed_manually() {
        let mut item = component(ProbeSpec::Manual {
            hint: "connect".to_string(),
        });
        item.setup = Some(
            magician_components::setup::resolve(&magician_components::setup::SetupBinding {
                definition: "whatsapp".to_string(),
                profile: None,
            })
            .expect("managed setup"),
        );
        let step = step_view(&item, Observed::Unknown("connect".to_string()), true, false);
        assert!(!step.can_confirm);
        assert!(!step.ready);
    }

    #[test]
    fn cua_setup_requires_desktop_verification() {
        let mut item = component(ProbeSpec::Manual {
            hint: "verify desktop access".to_string(),
        });
        item.setup = Some(
            magician_components::setup::resolve(&magician_components::setup::SetupBinding {
                definition: "cua-driver".to_string(),
                profile: None,
            })
            .expect("CuaDriver setup"),
        );
        let step = step_view(&item, Observed::Unknown("verify".to_string()), true, false);
        assert!(!step.can_confirm);
        assert!(!step.ready);
    }

    #[test]
    fn model_runtime_requires_an_explicit_choice_even_when_pplx_is_present() {
        let mut item = component(ProbeSpec::HttpOk {
            url: "http://127.0.0.1:11435/health".to_string(),
            timeout_ms: 100,
        });
        item.setup = Some(
            magician_components::setup::resolve(&magician_components::setup::SetupBinding {
                definition: "ollama-runtime".to_string(),
                profile: None,
            })
            .expect("model runtime setup"),
        );
        assert!(
            !step_view(
                &item,
                Observed::Present("PPLX is ready".to_string()),
                false,
                false
            )
            .ready
        );
        assert!(
            step_view(
                &item,
                Observed::Present("PPLX is ready".to_string()),
                false,
                true
            )
            .ready
        );
    }

    #[test]
    fn only_mandatory_auth_requirements_block_setup() {
        assert!(auth_is_hard_requirement("required"));
        assert!(auth_is_hard_requirement("at_least_one"));
        assert!(!auth_is_hard_requirement("conditional"));
        assert!(!auth_is_hard_requirement("optional"));
        assert!(!auth_is_hard_requirement("none"));
    }

    #[test]
    fn completion_is_bound_to_the_exact_engine_origin() {
        let mut state = OnboardingState::default();
        state.completed = true;
        state.engine_origin = Some("https://engine.example".to_string());
        assert!(state_completed_for_origin(&state, "https://engine.example"));
        assert!(!state_completed_for_origin(&state, "https://other.example"));
    }

    #[test]
    fn extracts_only_catalog_allowed_account_login_urls() {
        let expected = "https://accounts.google.com/o/oauth2/v2/auth?client_id=test";
        let hosts = vec!["accounts.google.com".to_string()];
        assert_eq!(
            extract_login_url(&format!("Open ({expected}) to continue"), &hosts),
            Some(expected.to_string())
        );
        assert_eq!(
            extract_login_url("https://evil.example/o/oauth2/v2/auth", &hosts),
            None
        );
    }

    #[test]
    fn oauth_authorization_urls_require_https_or_loopback_http() {
        assert!(validate_oauth_authorization_url(
            "https://provider.example/authorize?state=opaque"
        )
        .is_ok());
        assert!(
            validate_oauth_authorization_url("http://127.0.0.1:4317/authorize?state=opaque")
                .is_ok()
        );
        assert!(
            validate_oauth_authorization_url("http://provider.example/authorize?state=opaque")
                .is_err()
        );
        assert!(validate_oauth_authorization_url(
            "https://user:password@provider.example/authorize"
        )
        .is_err());
    }

    #[test]
    fn unpacked_extension_is_materialized_with_nested_files() {
        let root =
            std::env::temp_dir().join(format!("magican-extension-test-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination = root.join("downloads/Magican Browser Extension");
        std::fs::create_dir_all(source.join("assets")).expect("source folders");
        std::fs::write(source.join("manifest.json"), "{\"manifest_version\":3}").expect("manifest");
        std::fs::write(source.join("assets/icon.txt"), "icon").expect("nested file");

        copy_unpacked_extension(&source, &destination).expect("copy extension");

        assert!(destination.join("manifest.json").is_file());
        assert_eq!(
            std::fs::read_to_string(destination.join("assets/icon.txt")).expect("copied file"),
            "icon"
        );
        std::fs::remove_dir_all(root).expect("clean test folder");
    }
}
