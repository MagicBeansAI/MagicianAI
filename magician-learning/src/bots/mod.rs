pub mod auth_hitl_broker;
pub mod config_store;
pub mod scoped_runtime;

use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsStr,
    fs,
    path::Path,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, Signal, System};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, RwLock},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use magician::config::BotProcessConfig;
use magician::magician_v2::artifact_v2::CapabilityScopePaths;
use runtime_core::process::resolve_program;

pub use config_store::{BotConfigStore, BotConfigStoreError};
pub use scoped_runtime::{ScopedBotRuntime, ScopedBotRuntimeError};

const MAX_LOG_LINES: usize = 500;
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Extra time a stop waits for the supervisor task beyond `STOP_TIMEOUT`, so
/// the task's own SIGTERM -> SIGKILL escalation normally finishes first.
const STOP_JOIN_SLACK: Duration = Duration::from_secs(1);
/// How long a process group recorded by a previous Magician run gets after
/// SIGTERM before it is SIGKILLed.
const STALE_GROUP_GRACE: Duration = Duration::from_millis(1500);
/// Clock tolerance when matching a recorded group against the live leader
/// and the boot it was spawned in.
const PROCESS_GROUP_RECORD_CLOCK_SLACK_SECS: u64 = 5;
const TELEGRAM_AUTH_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// An unfinished browser sign-in otherwise holds the single managed-auth slot
/// (and the bot's "Authenticating" state) forever.
const GOOGLE_AUTH_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const GOOGLE_AUTH_SCOPES: [&str; 9] = [
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/pubsub",
    "https://www.googleapis.com/auth/drive",
    "https://www.googleapis.com/auth/calendar",
    "https://www.googleapis.com/auth/documents",
    "https://www.googleapis.com/auth/spreadsheets",
    "https://www.googleapis.com/auth/tasks",
    "openid",
    "https://www.googleapis.com/auth/userinfo.email",
];

/// Exit code reserved for "bot cannot start because the underlying account
/// needs authentication". When the supervisor observes a bot child exiting
/// with this code, it parks the bot in `BotRuntimeState::Failed` with a
/// needs-auth message and stops the auto-restart loop. The operator triggers
/// the OAuth flow via the UI's "Authenticate" button, which posts to
/// `/bots/{name}/auth/start`; on success the supervisor restarts the bot.
///
/// 79 is unassigned in BSD `sysexits.h` (which spans 64..=78), giving us a
/// signal no real CLI tool returns by accident.
pub const EX_NEEDS_AUTH: i32 = 79;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BotRuntimeState {
    Stopped,
    Running,
    Restarting,
    StopRequested,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotLogLine {
    pub timestamp: DateTime<Utc>,
    pub stream: String,
    pub line: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotExitSnapshot {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotStatusSnapshot {
    pub name: String,
    pub enabled: bool,
    pub auto_restart: bool,
    pub qr_supported: bool,
    pub desired_running: bool,
    pub state: BotRuntimeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<i64>,
    pub restart_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restart_backoff_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<BotExitSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BotAuthStatus {
    Unsupported,
    Ok,
    NeedsAuth,
    AccountMismatch,
    Error,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BotAuthFlowState {
    Idle,
    Active,
    Queued,
}

#[derive(Debug, Clone)]
pub struct GoogleWorkspaceAccount {
    /// The gws profile directory; its `client_secret.json` names the project.
    pub config_dir: PathBuf,
    pub expected_account: Option<String>,
    /// Project override from the bot env (`GOOGLE_WORKSPACE_PROJECT_ID`).
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotAuthSnapshot {
    pub name: String,
    pub supported: bool,
    pub provider: Option<String>,
    pub status: BotAuthStatus,
    pub flow_state: BotAuthFlowState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Sidecar payload a bot adapter writes to `MAGICIAN_BOT_AUTH_SIDECAR_PATH`
/// just before exiting with `EX_NEEDS_AUTH` (79). The supervisor reads it to
/// surface a provider-agnostic "needs auth" escalation in the attention bar
/// (see `BotAuthProvider::Generic`). Every field except `provider` is
/// optional so adapters with sparse metadata (e.g. an API-key bot with no
/// "profile") can still publish a useful escalation.
///
/// The file is rewritten on every needs-auth event and cleared by the
/// supervisor on a successful bot start. We deliberately do NOT persist this
/// across magician restarts as a source of truth — the magician process is
/// expected to clear/rewrite it as it observes bot lifecycle events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotNeedsAuthSidecar {
    /// Stable provider id the adapter advertises (e.g. `google_workspace`,
    /// `telegram_self`, `whatsapp_web`, `kapso`). Used by the supervisor to
    /// dispatch the right `start_auth` flow on operator click.
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_account: Option<String>,
    /// Human-readable reason the probe failed (e.g. "no refresh token
    /// stored", "session expired"). Goes into the attention bar subtitle
    /// when no other detail is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Epoch ms when the sidecar was written. Used purely for staleness
    /// debugging; the supervisor doesn't reason about it.
    #[serde(default)]
    pub written_at_ms: i64,
}

/// Provider name + reason whose `start_auth` button on the UI bot card. Some
/// providers require an out-of-band supervisor-spawned login (gws → opens
/// browser via `gws auth login`); others just need the bot restarted
/// (whatsapp shows its QR via the wu-cli `onQr` callback, kapso retries the
/// API key, etc.). The dispatch lives in `BotManager::start_auth`.
pub const NEEDS_AUTH_PROVIDER_GOOGLE_WORKSPACE: &str = "google_workspace";

/// The Magician API credential. Never accepted from configuration — the runtime
/// mints a scoped in-memory token per spawn and injects it under this name.
const MAGICIAN_BEARER_TOKEN_ENV: &str = "MAGICIAN_BEARER_TOKEN";
pub const NEEDS_AUTH_PROVIDER_TELEGRAM_SELF: &str = "telegram_self";
pub const NEEDS_AUTH_PROVIDER_WHATSAPP_WEB: &str = "whatsapp_web";
pub const NEEDS_AUTH_PROVIDER_KAPSO: &str = "kapso";

#[derive(Debug, Clone, Serialize)]
pub struct BotAuthSessionSnapshot {
    pub name: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_account: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BotAuthStateSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<BotAuthSessionSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<BotAuthSessionSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BotAuthStartResponse {
    pub name: String,
    pub flow_state: BotAuthFlowState,
    pub auth_state: BotAuthStateSnapshot,
}

/// What a running managed login is waiting for from the person. Only the
/// kind is published; the value goes to the login process's stdin and is
/// never stored or logged.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BotAuthPendingInput {
    Phone,
    Code,
    Password,
}

/// How a managed login reaches the person completing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotAuthMethod {
    /// Google: publish the authorization URL for a computer-mode sign-in.
    LoginUrl,
    /// Telegram: phone, then the code Telegram sends, then an optional
    /// two-step password, each over the login's stdin.
    Code,
    /// WhatsApp: link with a phone number instead of scanning a QR.
    PairingCode,
    /// Telegram or WhatsApp QR.
    Qr,
}

/// Options for [`BotManager::start_auth_with`]. The default reproduces
/// [`BotManager::start_auth`] (Desktop onboarding).
#[derive(Clone, Default)]
pub struct BotAuthStartOptions {
    pub method: Option<BotAuthMethod>,
    /// Google: never open the engine host's browser; publish the login URL
    /// through [`BotAuthConnectMaterial::login_url`] instead.
    pub headless: bool,
    /// Telegram code login: seeded into tgcli's own store config. WhatsApp
    /// pairing: handed to the next bot spawn only, never persisted.
    pub phone: Option<String>,
}

impl std::fmt::Debug for BotAuthStartOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BotAuthStartOptions")
            .field("method", &self.method)
            .field("headless", &self.headless)
            .field("phone", &self.phone.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Login material a managed login hands to the person who completes it.
/// Deliberately not `Debug` or `Serialize`: the API decides which part
/// reaches which client, and none of it belongs in logs.
#[derive(Clone)]
pub struct BotAuthConnectMaterial {
    /// Authorization URL scraped from a headless `gws auth login`.
    pub login_url: Option<String>,
    /// WhatsApp pairing code published by the running bot.
    pub pairing_code: Option<String>,
    pub pending_input: Option<BotAuthPendingInput>,
    /// The bot's pairing QR artifact, when one currently exists.
    pub qr_path: Option<PathBuf>,
    /// Whether the bot itself is meant to be running (WhatsApp pairs inside
    /// the running bot rather than in a separate login process).
    pub worker_running: bool,
    pub flow_state: BotAuthFlowState,
}

impl Default for BotAuthConnectMaterial {
    fn default() -> Self {
        Self {
            login_url: None,
            pairing_code: None,
            pending_input: None,
            qr_path: None,
            worker_running: false,
            flow_state: BotAuthFlowState::Idle,
        }
    }
}

/// Whether a managed login can start at all on this engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotAuthAvailability {
    Available,
    /// Secret-free reason naming what is missing.
    Unavailable(&'static str),
}

/// A shared slot whose value must never reach `Debug` output.
#[derive(Default)]
struct RedactedSlot<T>(Arc<std::sync::Mutex<T>>);

impl<T> std::fmt::Debug for RedactedSlot<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RedactedSlot(..)")
    }
}

impl<T> RedactedSlot<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn handle(&self) -> Arc<std::sync::Mutex<T>> {
        Arc::clone(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct BotConfigStateSnapshot {
    pub config: BotProcessConfig,
    pub desired_running: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BotManagerError {
    #[error("bot `{0}` is not configured")]
    UnknownBot(String),
    #[error("bot `{name}` failed to start: {reason}")]
    StartFailed { name: String, reason: String },
    #[error("bot `{name}` does not support managed auth")]
    AuthUnsupported { name: String },
    #[error("bot `{name}` auth failed: {reason}")]
    AuthFailed { name: String, reason: String },
}

#[derive(Debug, Clone)]
pub struct BotManager {
    bots: Arc<RwLock<BTreeMap<String, Arc<ManagedBot>>>>,
    auth_state: Arc<Mutex<BotAuthCoordinatorState>>,
    scope_paths: Option<CapabilityScopePaths>,
}

impl BotManager {
    pub fn new(configs: BTreeMap<String, BotProcessConfig>) -> Self {
        Self::new_with_scope_paths(configs, None)
    }

    pub fn new_scoped(
        configs: BTreeMap<String, BotProcessConfig>,
        scope_paths: CapabilityScopePaths,
    ) -> Self {
        Self::new_with_scope_paths(configs, Some(scope_paths))
    }

    fn new_with_scope_paths(
        configs: BTreeMap<String, BotProcessConfig>,
        scope_paths: Option<CapabilityScopePaths>,
    ) -> Self {
        let bots = configs
            .into_iter()
            .map(|(name, config)| {
                let bot = Arc::new(ManagedBot::new(name.clone(), config, scope_paths.clone()));
                (name, bot)
            })
            .collect();
        Self {
            bots: Arc::new(RwLock::new(bots)),
            auth_state: Arc::new(Mutex::new(BotAuthCoordinatorState::default())),
            scope_paths,
        }
    }

    pub async fn start_enabled(&self) {
        // Reap every bot's leftover group first, enabled or not: a bot that
        // was disabled after a crashed run still left its tree running.
        for bot in self.bots().await {
            bot.reap_recorded_process_group().await;
        }
        for bot in self.bots().await {
            if !bot.config.enabled {
                continue;
            }
            if let Err(err) = bot.start().await {
                warn!(bot = %bot.name, error = %err, "Configured bot failed to start");
            }
        }
    }

    pub async fn shutdown(&self) {
        let pending = self.begin_shutdown().await;
        finish_bot_stops(pending).await;
    }

    /// Ask every bot to stop without waiting for any of them, and end any
    /// in-flight managed sign-in. Each bot runs in its own process group, so
    /// a signal to Magician's group does not reach it; this is what stops
    /// the bots (and the CLIs they fork) when Magician shuts down.
    async fn begin_shutdown(&self) -> Vec<PendingBotStop> {
        let mut pending = Vec::new();
        for bot in self.bots().await {
            bot.kill_auth_process_group();
            pending.push(bot.begin_stop().await);
        }
        pending
    }

    pub async fn list(&self) -> Vec<BotStatusSnapshot> {
        let bots = self.bots().await;
        let mut snapshots = Vec::with_capacity(bots.len());
        for bot in bots {
            snapshots.push(bot.snapshot().await);
        }
        snapshots
    }

    pub async fn start(&self, name: &str) -> Result<BotStatusSnapshot, BotManagerError> {
        self.bot(name).await?.start().await
    }

    pub async fn stop(&self, name: &str) -> Result<BotStatusSnapshot, BotManagerError> {
        self.bot(name).await?.stop().await
    }

    pub async fn restart(&self, name: &str) -> Result<BotStatusSnapshot, BotManagerError> {
        self.bot(name).await?.restart().await
    }

    pub async fn logs(&self, name: &str, limit: usize) -> Result<Vec<BotLogLine>, BotManagerError> {
        Ok(self.bot(name).await?.logs(limit).await)
    }

    pub async fn snapshot(&self, name: &str) -> Result<BotStatusSnapshot, BotManagerError> {
        Ok(self.bot(name).await?.snapshot().await)
    }

    pub async fn qr_code_path(&self, name: &str) -> Result<Option<PathBuf>, BotManagerError> {
        Ok(self.bot(name).await?.qr_code_path())
    }

    pub async fn config(&self, name: &str) -> Result<BotProcessConfig, BotManagerError> {
        Ok(self.bot(name).await?.config())
    }

    pub async fn env_file_path(&self, name: &str) -> Result<Option<PathBuf>, BotManagerError> {
        let bot = self.bot(name).await?;
        let bot_cwd = resolve_bot_cwd(&bot.config, bot.scope_paths.as_ref()).map_err(|error| {
            BotManagerError::AuthFailed {
                name: name.to_string(),
                reason: error.to_string(),
            }
        })?;
        resolve_bot_env_file(&bot.config, bot.scope_paths.as_ref(), &bot_cwd).map_err(|error| {
            BotManagerError::AuthFailed {
                name: name.to_string(),
                reason: error.to_string(),
            }
        })
    }

    /// The Google account a Google Workspace bot signs in as, for checking that
    /// account's access to the Cloud project. `None` for any other bot.
    pub async fn google_workspace_account(
        &self,
        name: &str,
    ) -> Result<Option<GoogleWorkspaceAccount>, BotManagerError> {
        let bot = self.bot(name).await?;
        let config =
            bot.google_workspace_auth_config()
                .map_err(|error| BotManagerError::AuthFailed {
                    name: name.to_string(),
                    reason: error.to_string(),
                })?;
        Ok(config.map(|config| GoogleWorkspaceAccount {
            config_dir: config.gws_config_dir,
            expected_account: config.expected_account,
            project_id: config.project_id,
        }))
    }

    pub async fn auth_info(&self, name: &str) -> Result<BotAuthSnapshot, BotManagerError> {
        let bot = self.bot(name).await?;
        let flow_state = self.auth_flow_state(name).await;
        bot.auth_snapshot(flow_state).await
    }

    /// Snapshot the auth state for every bot in this manager. Used by the
    /// attention surface to fan out "needs auth" escalations without N
    /// round-trips from the UI. Bots whose auth_snapshot computation fails
    /// are silently skipped — attention bar items must not crash the whole
    /// list, and per-bot failures already surface via `GET /bots/{name}/auth`.
    pub async fn list_auth(&self) -> Vec<BotAuthSnapshot> {
        let bots = self.bots().await;
        let mut snapshots = Vec::with_capacity(bots.len());
        for bot in bots {
            let flow_state = self.auth_flow_state(&bot.name).await;
            if let Ok(snapshot) = bot.auth_snapshot(flow_state).await {
                snapshots.push(snapshot);
            }
        }
        snapshots
    }

    pub async fn auth_state(&self) -> BotAuthStateSnapshot {
        self.auth_state.lock().await.snapshot()
    }

    pub async fn start_auth(&self, name: &str) -> Result<BotAuthStartResponse, BotManagerError> {
        self.start_auth_with(name, BotAuthStartOptions::default())
            .await
    }

    /// [`Self::start_auth`] with a chosen method (Service accounts on a
    /// phone): a headless Google login, Telegram code login, or WhatsApp
    /// pairing code.
    pub async fn start_auth_with(
        &self,
        name: &str,
        options: BotAuthStartOptions,
    ) -> Result<BotAuthStartResponse, BotManagerError> {
        let bot = self.bot(name).await?;

        // Provider dispatch. Read the needs-auth sidecar to learn which
        // provider the adapter is asking us to authenticate against; fall
        // back to Google Workspace for legacy callers that don't have a
        // sidecar (e.g. UI that pre-dates the generic protocol).
        let sidecar_provider = bot
            .read_needs_auth_sidecar()
            .map(|sidecar| sidecar.provider);

        if sidecar_provider.as_deref() == Some(NEEDS_AUTH_PROVIDER_TELEGRAM_SELF)
            || bot.name == "telegram-self"
        {
            let login = TelegramLogin {
                code: options.method == Some(BotAuthMethod::Code),
                phone: options
                    .phone
                    .as_deref()
                    .and_then(normalized_phone)
                    .map(|digits| format!("+{digits}")),
            };
            return self.start_telegram_auth(&bot, login).await;
        }

        if sidecar_provider.as_deref() == Some(NEEDS_AUTH_PROVIDER_WHATSAPP_WEB)
            || bot.name == "whatsapp"
        {
            match options.method {
                Some(BotAuthMethod::PairingCode) => {
                    let digits = options
                        .phone
                        .as_deref()
                        .and_then(normalized_phone)
                        .ok_or_else(|| BotManagerError::AuthFailed {
                            name: name.to_string(),
                            reason: "a phone number is required to link with a pairing code"
                                .to_string(),
                        })?;
                    bot.set_one_shot_env(WHATSAPP_PHONE_ENV, digits);
                },
                // An empty value outranks a WHATSAPP_PHONE left in the bot's
                // env file (node lets the inherited environment win), so an
                // explicit QR request really shows a QR.
                Some(BotAuthMethod::Qr) => bot.set_one_shot_env(WHATSAPP_PHONE_ENV, String::new()),
                _ => bot.clear_one_shot_env(),
            }
            bot.remove_pairing_code();
            return self
                .start_auth_generic(&bot, NEEDS_AUTH_PROVIDER_WHATSAPP_WEB)
                .await;
        }

        if let Some(provider) = sidecar_provider.as_deref() {
            if provider != NEEDS_AUTH_PROVIDER_GOOGLE_WORKSPACE {
                return self.start_auth_generic(&bot, provider).await;
            }
        }

        // Read by `google_workspace_auth_config`, so a queued login that
        // starts later keeps the choice too.
        bot.auth_headless
            .store(options.headless, std::sync::atomic::Ordering::SeqCst);
        let auth_config = bot
            .google_workspace_auth_config()
            .map_err(|reason| BotManagerError::AuthFailed {
                name: name.to_string(),
                reason: reason.to_string(),
            })?
            .ok_or_else(|| BotManagerError::AuthUnsupported {
                name: name.to_string(),
            })?;
        let session = auth_config.session(name);

        let (flow_state, auth_state, should_start_now) = {
            let mut auth_state = self.auth_state.lock().await;
            if auth_state
                .active
                .as_ref()
                .is_some_and(|active| active.name == name)
            {
                (BotAuthFlowState::Active, auth_state.snapshot(), false)
            } else if auth_state.queue.iter().any(|queued| queued.name == name) {
                (BotAuthFlowState::Queued, auth_state.snapshot(), false)
            } else if auth_state.active.is_none() {
                auth_state.active = Some(session.clone());
                (BotAuthFlowState::Active, auth_state.snapshot(), true)
            } else {
                auth_state.queue.push_back(session.clone());
                (BotAuthFlowState::Queued, auth_state.snapshot(), false)
            }
        };

        if should_start_now {
            spawn_auth_task(self.clone(), session, auth_config);
        }

        Ok(BotAuthStartResponse {
            name: name.to_string(),
            flow_state,
            auth_state,
        })
    }

    /// Generic Authenticate flow for providers whose auth is owned by bot
    /// startup (whatsapp_web → wu-cli publishes a QR during connect; kapso →
    /// API key check on next request). We just clear the
    /// needs-auth sidecar and restart the bot. The bot's own startup
    /// surfaces whatever interactive prompt it needs (QR, etc.) in its
    /// logs, visible in the BotAuthMonitor.
    async fn start_auth_generic(
        &self,
        bot: &Arc<ManagedBot>,
        provider: &str,
    ) -> Result<BotAuthStartResponse, BotManagerError> {
        bot.push_log(
            "auth",
            format!("starting generic auth restart for provider `{provider}`"),
        )
        .await;
        // Clear the sidecar so a successful restart doesn't immediately
        // re-surface as needs-auth. The adapter will rewrite it on the next
        // probe failure if auth is genuinely still broken.
        bot.clear_needs_auth_sidecar();
        if let Err(err) = bot.stop().await {
            bot.push_log("auth", format!("failed to stop bot before auth: {err}"))
                .await;
        }
        match bot.start().await {
            Ok(_) => {
                bot.push_log(
                    "auth",
                    "restarted bot; check logs for provider-specific prompt",
                )
                .await;
            },
            Err(err) => {
                bot.push_log(
                    "auth",
                    format!("failed to restart bot after auth click: {err}"),
                )
                .await;
                return Err(err);
            },
        }
        let auth_state_snapshot = {
            let auth_state = self.auth_state.lock().await;
            auth_state.snapshot()
        };
        Ok(BotAuthStartResponse {
            name: bot.name.clone(),
            flow_state: BotAuthFlowState::Idle,
            auth_state: auth_state_snapshot,
        })
    }

    async fn start_telegram_auth(
        &self,
        bot: &Arc<ManagedBot>,
        login: TelegramLogin,
    ) -> Result<BotAuthStartResponse, BotManagerError> {
        let mut auth_config =
            bot.telegram_auth_config()
                .map_err(|reason| BotManagerError::AuthFailed {
                    name: bot.name.clone(),
                    reason: reason.to_string(),
                })?;
        auth_config.login = login;
        let session = auth_config.session(&bot.name);

        let (flow_state, auth_state, should_start_now) = {
            let mut auth_state = self.auth_state.lock().await;
            if auth_state
                .active
                .as_ref()
                .is_some_and(|active| active.name == bot.name)
            {
                (BotAuthFlowState::Active, auth_state.snapshot(), false)
            } else if auth_state.active.is_some() {
                return Err(BotManagerError::AuthFailed {
                    name: bot.name.clone(),
                    reason: "another managed account login is already active".to_string(),
                });
            } else {
                auth_state.active = Some(session.clone());
                (BotAuthFlowState::Active, auth_state.snapshot(), true)
            }
        };

        if should_start_now {
            spawn_telegram_auth_task(self.clone(), session, auth_config);
        }

        Ok(BotAuthStartResponse {
            name: bot.name.clone(),
            flow_state,
            auth_state,
        })
    }

    pub async fn submit_auth_input(&self, name: &str, input: &str) -> Result<(), BotManagerError> {
        let input = input.trim_end_matches(['\r', '\n']);
        if input.is_empty()
            || input.len() > 4096
            || input
                .chars()
                .any(|character| matches!(character, '\r' | '\n'))
        {
            return Err(BotManagerError::AuthFailed {
                name: name.to_string(),
                reason: "interactive auth input is empty or invalid".to_string(),
            });
        }
        self.bot(name).await?.submit_auth_input(input).await
    }

    /// The login material of this bot's managed login, if any is waiting.
    pub async fn auth_connect_material(
        &self,
        name: &str,
    ) -> Result<BotAuthConnectMaterial, BotManagerError> {
        let bot = self.bot(name).await?;
        let flow_state = self.auth_flow_state(name).await;
        let worker_running = bot.is_desired_running().await;
        // Std guards are taken only after the last await.
        let login_url = bot.auth_login_url.lock().clone();
        let pending_input = *bot.auth_pending_input.lock();
        Ok(BotAuthConnectMaterial {
            login_url,
            pairing_code: bot.read_pairing_code(),
            pending_input,
            qr_path: bot.qr_code_path().filter(|path| path.is_file()),
            worker_running,
            flow_state,
        })
    }

    /// End this bot's managed login: a queued one leaves the queue, a running
    /// Telegram or Google login process is killed, and a WhatsApp pairing
    /// phone that has not reached a spawn yet is dropped.
    pub async fn cancel_auth(&self, name: &str) -> Result<(), BotManagerError> {
        let bot = self.bot(name).await?;
        {
            let mut auth_state = self.auth_state.lock().await;
            auth_state.queue.retain(|queued| queued.name != name);
        }
        bot.cancel_auth_login();
        Ok(())
    }

    /// Whether this bot's managed login has what it needs on this engine
    /// (its CLI, API credentials, OAuth client, or bot bundle).
    pub async fn auth_availability(
        &self,
        name: &str,
    ) -> Result<BotAuthAvailability, BotManagerError> {
        Ok(self.bot(name).await?.auth_availability())
    }

    pub async fn config_state(&self, name: &str) -> Option<BotConfigStateSnapshot> {
        let bot = self.bot_optional(name).await?;
        Some(BotConfigStateSnapshot {
            config: bot.config(),
            desired_running: bot.is_desired_running().await,
        })
    }

    pub async fn upsert_config(
        &self,
        name: String,
        config: BotProcessConfig,
    ) -> Result<BotStatusSnapshot, BotManagerError> {
        let desired_running = match self.bot_optional(&name).await.as_ref() {
            Some(bot) => bot.is_desired_running().await,
            None => false,
        };

        self.replace_config(name, config, desired_running).await
    }

    pub async fn restore_config(
        &self,
        name: String,
        config: BotProcessConfig,
        desired_running: bool,
    ) -> Result<BotStatusSnapshot, BotManagerError> {
        self.replace_config(name, config, desired_running).await
    }

    pub async fn delete_config(&self, name: &str) -> Result<(), BotManagerError> {
        let bot = {
            let mut bots = self.bots.write().await;
            bots.remove(name)
        }
        .ok_or_else(|| BotManagerError::UnknownBot(name.to_string()))?;

        bot.stop().await?;
        Ok(())
    }

    async fn bots(&self) -> Vec<Arc<ManagedBot>> {
        self.bots.read().await.values().cloned().collect()
    }

    async fn bot(&self, name: &str) -> Result<Arc<ManagedBot>, BotManagerError> {
        self.bot_optional(name)
            .await
            .ok_or_else(|| BotManagerError::UnknownBot(name.to_string()))
    }

    async fn bot_optional(&self, name: &str) -> Option<Arc<ManagedBot>> {
        self.bots.read().await.get(name).cloned()
    }

    async fn auth_flow_state(&self, name: &str) -> BotAuthFlowState {
        let auth_state = self.auth_state.lock().await;
        if auth_state
            .active
            .as_ref()
            .is_some_and(|active| active.name == name)
        {
            return BotAuthFlowState::Active;
        }
        if auth_state.queue.iter().any(|queued| queued.name == name) {
            return BotAuthFlowState::Queued;
        }
        BotAuthFlowState::Idle
    }

    async fn replace_config(
        &self,
        name: String,
        config: BotProcessConfig,
        desired_running: bool,
    ) -> Result<BotStatusSnapshot, BotManagerError> {
        let previous = self.bot_optional(&name).await;

        if let Some(bot) = previous.as_ref() {
            bot.stop().await?;
        }

        let new_bot = Arc::new(ManagedBot::new(
            name.clone(),
            config,
            self.scope_paths.clone(),
        ));
        {
            let mut bots = self.bots.write().await;
            bots.insert(name.clone(), Arc::clone(&new_bot));
        }

        if !desired_running {
            return Ok(new_bot.snapshot().await);
        }

        match new_bot.start().await {
            Ok(snapshot) => Ok(snapshot),
            Err(err) => {
                if let Err(rollback_err) = self
                    .rollback_failed_replace(&name, previous, desired_running)
                    .await
                {
                    return Err(start_failed_with_rollback(name, err, rollback_err));
                }
                Err(err)
            },
        }
    }

    async fn run_google_workspace_auth_flow(
        self,
        session: BotAuthSessionSnapshot,
        auth_config: GoogleWorkspaceAuthConfig,
    ) {
        let bot = match self.bot(&session.name).await {
            Ok(bot) => bot,
            Err(err) => {
                warn!(bot = %session.name, error = %err, "Managed auth bot disappeared");
                self.finish_auth_session(&session.name).await;
                return;
            },
        };

        let was_running = bot.is_desired_running().await;
        if was_running {
            if let Err(err) = bot.stop().await {
                bot.push_log("auth", format!("failed to stop bot before auth: {err}"))
                    .await;
            }
        }

        let profile_label = session
            .profile_label
            .clone()
            .unwrap_or_else(|| format_bot_name(&session.name));
        let expected_suffix = session
            .expected_account
            .as_ref()
            .map(|value| format!(" as {value}"))
            .unwrap_or_default();
        bot.push_log(
            "auth",
            format!("starting Google Workspace auth for {profile_label}{expected_suffix}"),
        )
        .await;

        let result = bot.run_google_workspace_auth(&auth_config).await;
        // The authorization URL is single-use material; it never outlives
        // the login that printed it.
        *bot.auth_login_url.lock() = None;
        match result {
            Ok(account) => {
                // As for Telegram: a bot that is not restarted below would
                // otherwise keep reporting the needs-auth it exited with.
                bot.clear_needs_auth_sidecar();
                bot.push_log("auth", format!("authenticated successfully as {account}"))
                    .await;
                if was_running {
                    match bot.start().await {
                        Ok(_) => {
                            bot.push_log("auth", "restarted bot after successful auth")
                                .await;
                        },
                        Err(err) => {
                            bot.push_log(
                                "auth",
                                format!("failed to restart bot after auth: {err}"),
                            )
                            .await;
                        },
                    }
                }
            },
            Err(err) => {
                bot.push_log("auth", format!("managed auth failed: {err}"))
                    .await;
            },
        }

        self.finish_auth_session(&session.name).await;
    }

    async fn run_telegram_auth_flow(
        self,
        session: BotAuthSessionSnapshot,
        auth_config: TelegramAuthConfig,
    ) {
        let bot = match self.bot(&session.name).await {
            Ok(bot) => bot,
            Err(error) => {
                warn!(bot = %session.name, error = %error, "Managed Telegram auth bot disappeared");
                self.finish_auth_session(&session.name).await;
                return;
            },
        };
        let was_running = bot.is_desired_running().await;
        if was_running {
            if let Err(error) = bot.stop().await {
                bot.push_log("auth", format!("failed to stop bot before auth: {error}"))
                    .await;
            }
        }
        bot.push_log(
            "auth",
            if auth_config.login.code {
                "starting Telegram code login; Telegram sends a code to your account"
            } else {
                "starting Telegram QR login; scan it in Settings → Devices → Link Desktop Device"
            },
        )
        .await;
        match bot.run_telegram_auth(&auth_config).await {
            Ok(account) => {
                bot.clear_needs_auth_sidecar();
                bot.push_log("auth", format!("authenticated successfully as {account}"))
                    .await;
                if was_running {
                    match bot.start().await {
                        Ok(_) => {
                            bot.push_log("auth", "restarted bot after successful auth")
                                .await;
                        },
                        Err(error) => {
                            bot.push_log(
                                "auth",
                                format!("failed to restart bot after auth: {error}"),
                            )
                            .await;
                        },
                    }
                }
            },
            Err(error) => {
                bot.push_log("auth", format!("managed Telegram auth failed: {error}"))
                    .await;
            },
        }
        self.finish_auth_session(&session.name).await;
    }

    async fn finish_auth_session(&self, completed_name: &str) {
        {
            let mut auth_state = self.auth_state.lock().await;
            if auth_state
                .active
                .as_ref()
                .is_some_and(|active| active.name == completed_name)
            {
                auth_state.active = None;
            }
        }
        loop {
            let next_session = {
                let mut auth_state = self.auth_state.lock().await;
                if auth_state.active.is_some() {
                    return;
                }
                auth_state.queue.pop_front()
            };

            let Some(next_session) = next_session else {
                return;
            };

            let bot = match self.bot(&next_session.name).await {
                Ok(bot) => bot,
                Err(err) => {
                    warn!(bot = %next_session.name, error = %err, "Queued auth bot disappeared");
                    continue;
                },
            };
            let auth_config = match bot.google_workspace_auth_config() {
                Ok(Some(config)) => config,
                Ok(None) => {
                    bot.push_log(
                        "auth",
                        "queued managed auth skipped: bot no longer supports managed auth",
                    )
                    .await;
                    continue;
                },
                Err(err) => {
                    bot.push_log("auth", format!("queued managed auth skipped: {err}"))
                        .await;
                    continue;
                },
            };

            {
                let mut auth_state = self.auth_state.lock().await;
                if auth_state.active.is_some() {
                    return;
                }
                auth_state.active = Some(next_session.clone());
            }

            spawn_auth_task(self.clone(), next_session, auth_config);
            return;
        }
    }

    async fn rollback_failed_replace(
        &self,
        name: &str,
        previous: Option<Arc<ManagedBot>>,
        desired_running: bool,
    ) -> Result<(), BotManagerError> {
        match previous {
            Some(previous_bot) => {
                {
                    let mut bots = self.bots.write().await;
                    bots.insert(name.to_string(), Arc::clone(&previous_bot));
                }

                if desired_running {
                    previous_bot.start().await.map(|_| ())
                } else {
                    Ok(())
                }
            },
            None => {
                let mut bots = self.bots.write().await;
                bots.remove(name);
                Ok(())
            },
        }
    }
}

#[derive(Debug)]
struct ManagedBot {
    name: String,
    config: BotProcessConfig,
    scope_paths: Option<CapabilityScopePaths>,
    runtime: Arc<Mutex<ManagedBotState>>,
    /// Keep starts and the full stop/join transition exclusive for this bot.
    lifecycle: Arc<Mutex<()>>,
    auth_input: Arc<Mutex<Option<ChildStdin>>>,
    /// Process group of an in-flight `gws auth login`, so a Magician
    /// shutdown can end the sign-in together with the bots.
    auth_process_group: std::sync::Mutex<Option<u32>>,
    /// The group this process spawned for the bot and recorded on disk, while
    /// it runs. Guards the record: it is written and cleared under this lock,
    /// and the stale-group reap reads it under the same lock, so a concurrent
    /// `start()` can never reap the group it just spawned.
    live_process_group: std::sync::Mutex<Option<u32>>,
    /// What the running Telegram login's last prompt asked for.
    auth_pending_input: RedactedSlot<Option<BotAuthPendingInput>>,
    /// Authorization URL of a headless `gws auth login`. Never logged; only
    /// the API's computer-mode sign-in session receives it.
    auth_login_url: RedactedSlot<Option<String>>,
    /// Google: publish the login URL instead of opening the host browser.
    auth_headless: std::sync::atomic::AtomicBool,
    /// Ends a running Telegram login (cancel, or tgcli asking for API keys).
    auth_cancel: std::sync::Mutex<Option<CancellationToken>>,
    /// Environment for the next spawn only, then dropped (the WhatsApp
    /// pairing phone). Never written to disk.
    one_shot_env: RedactedSlot<BTreeMap<String, String>>,
}

/// On-disk record of a running bot's process group (`<bots_root>/.run/`).
/// Bots run in their own groups, which a Magician shutdown signal no longer
/// reaches, so the next run reads this to reap a group a crashed or killed
/// run left behind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct BotProcessGroupRecord {
    /// Group id, equal to the bot's pid (spawned with `process_group(0)`).
    pgid: u32,
    /// `System::boot_time()` at spawn; pids from an earlier boot mean nothing.
    boot_time: u64,
    /// Unix seconds at spawn, matched against the live leader's start time.
    spawned_at: u64,
}

/// A stop that has been requested but not yet joined: the supervisor task
/// was cancelled (which SIGTERMs the bot's group) and is winding down.
/// Splitting request from join lets a shutdown signal every bot at once and
/// then wait for all of them against one deadline.
struct PendingBotStop {
    bot: Arc<ManagedBot>,
    handle: Option<JoinHandle<()>>,
    pid: Option<u32>,
    _lifecycle: tokio::sync::OwnedMutexGuard<()>,
}

#[derive(Debug)]
struct ManagedBotState {
    desired_running: bool,
    state: BotRuntimeState,
    pid: Option<u32>,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
    restart_count: u64,
    restart_backoff_secs: Option<u64>,
    last_exit: Option<BotExitSnapshot>,
    last_error: Option<String>,
    logs: VecDeque<BotLogLine>,
    stop_token: Option<CancellationToken>,
    supervisor_handle: Option<JoinHandle<()>>,
}

impl ManagedBot {
    fn new(
        name: String,
        config: BotProcessConfig,
        scope_paths: Option<CapabilityScopePaths>,
    ) -> Self {
        Self {
            name,
            config,
            scope_paths,
            runtime: Arc::new(Mutex::new(ManagedBotState {
                desired_running: false,
                state: BotRuntimeState::Stopped,
                pid: None,
                started_at: None,
                stopped_at: Some(Utc::now()),
                restart_count: 0,
                restart_backoff_secs: None,
                last_exit: None,
                last_error: None,
                logs: VecDeque::new(),
                stop_token: None,
                supervisor_handle: None,
            })),
            lifecycle: Arc::new(Mutex::new(())),
            auth_input: Arc::new(Mutex::new(None)),
            auth_process_group: std::sync::Mutex::new(None),
            live_process_group: std::sync::Mutex::new(None),
            auth_pending_input: RedactedSlot::default(),
            auth_login_url: RedactedSlot::default(),
            auth_headless: std::sync::atomic::AtomicBool::new(false),
            auth_cancel: std::sync::Mutex::new(None),
            one_shot_env: RedactedSlot::default(),
        }
    }

    async fn snapshot(&self) -> BotStatusSnapshot {
        let mut runtime = self.runtime.lock().await;
        runtime.clear_finished_handle();
        runtime.snapshot(&self.name, &self.config)
    }

    /// Path at which this bot's needs-auth sidecar lives. Co-located with
    /// other auth artefacts under the scope's `auth_root/needs_auth/` so
    /// it inherits the same backup / wipe semantics as other auth state.
    /// Returns `None` when the bot isn't scoped — we don't have a sensible
    /// fallback location and the sidecar plumbing simply no-ops.
    fn needs_auth_sidecar_path(&self) -> Option<PathBuf> {
        let scope = self.scope_paths.as_ref()?;
        Some(
            scope
                .auth_root
                .join("needs_auth")
                .join(format!("{}.json", self.name)),
        )
    }

    /// Read the sidecar a previous bot run left behind, if any. Used by
    /// `auth_snapshot` to surface needs-auth escalations without depending
    /// on any provider-specific probe. Soft-fails on decode error — a
    /// corrupt sidecar should not crash the auth surface.
    fn read_needs_auth_sidecar(&self) -> Option<BotNeedsAuthSidecar> {
        let path = self.needs_auth_sidecar_path()?;
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice::<BotNeedsAuthSidecar>(&bytes).ok()
    }

    /// Remove the sidecar (idempotent — missing-file is success). Called on
    /// every spawn so a successful start clears any prior escalation, and
    /// from the `start_auth` success path so an authenticate-then-restart
    /// cycle doesn't leave a stale escalation banner.
    fn clear_needs_auth_sidecar(&self) {
        if let Some(path) = self.needs_auth_sidecar_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    async fn logs(&self, limit: usize) -> Vec<BotLogLine> {
        let runtime = self.runtime.lock().await;
        let capped_limit = limit.max(1).min(MAX_LOG_LINES);
        let len = runtime.logs.len();
        let start = len.saturating_sub(capped_limit);
        runtime.logs.iter().skip(start).cloned().collect()
    }

    fn config(&self) -> BotProcessConfig {
        self.config.clone()
    }

    fn qr_code_path(&self) -> Option<PathBuf> {
        let configured_key = match self.name.as_str() {
            "whatsapp" => Some("WHATSAPP_QR_FILE"),
            "telegram-self" => Some("TELEGRAM_QR_FILE"),
            _ => None,
        };
        if let Some(configured) = configured_key.and_then(|key| self.config.env.get(key)) {
            let candidate =
                PathBuf::from(expand_runtime_value(configured, self.scope_paths.as_ref()).ok()?);
            if candidate.is_absolute() {
                return Some(candidate);
            }

            if let Some(cwd) = self.config.cwd.as_deref() {
                return Some(
                    PathBuf::from(expand_runtime_value(cwd, self.scope_paths.as_ref()).ok()?)
                        .join(candidate),
                );
            }

            return std::env::current_dir().ok().map(|cwd| cwd.join(candidate));
        }

        let scope = self.scope_paths.as_ref()?;
        match self.name.as_str() {
            "whatsapp" => Some(scope.auth_root.join("pairing").join("whatsapp.png")),
            "telegram-self" => Some(scope.auth_root.join("pairing").join("telegram-self.txt")),
            _ => None,
        }
    }

    fn whatsapp_status_path(&self) -> Option<PathBuf> {
        if self.name != "whatsapp" {
            return None;
        }
        if let Some(configured) = self.config.env.get("WHATSAPP_STATUS_FILE") {
            let candidate =
                PathBuf::from(expand_runtime_value(configured, self.scope_paths.as_ref()).ok()?);
            if candidate.is_absolute() {
                return Some(candidate);
            }
            let cwd = resolve_bot_cwd(&self.config, self.scope_paths.as_ref()).ok()?;
            return Some(cwd.join(candidate));
        }
        Some(
            self.scope_paths
                .as_ref()?
                .auth_root
                .join("pairing")
                .join("whatsapp.connected"),
        )
    }

    /// Where the WhatsApp bot publishes a pairing code (beside its QR).
    fn whatsapp_pairing_code_path(&self) -> Option<PathBuf> {
        if self.name != "whatsapp" {
            return None;
        }
        if let Some(configured) = self.config.env.get(WHATSAPP_PAIRING_CODE_FILE_ENV) {
            let candidate =
                PathBuf::from(expand_runtime_value(configured, self.scope_paths.as_ref()).ok()?);
            if candidate.is_absolute() {
                return Some(candidate);
            }
            let cwd = resolve_bot_cwd(&self.config, self.scope_paths.as_ref()).ok()?;
            return Some(cwd.join(candidate));
        }
        Some(self.qr_code_path()?.with_file_name("whatsapp.pairing-code"))
    }

    fn read_pairing_code(&self) -> Option<String> {
        let path = self.whatsapp_pairing_code_path()?;
        let metadata = fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > 64 {
            return None;
        }
        normalize_pairing_code(&fs::read_to_string(path).ok()?)
    }

    fn remove_pairing_code(&self) {
        if let Some(path) = self.whatsapp_pairing_code_path() {
            let _ = fs::remove_file(path);
        }
    }

    fn set_one_shot_env(&self, key: &str, value: String) {
        self.one_shot_env.lock().insert(key.to_string(), value);
    }

    fn clear_one_shot_env(&self) {
        self.one_shot_env.lock().clear();
    }

    fn take_one_shot_env(&self) -> BTreeMap<String, String> {
        std::mem::take(&mut *self.one_shot_env.lock())
    }

    fn set_auth_cancel(&self, token: Option<CancellationToken>) {
        *self
            .auth_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = token;
    }

    /// See [`BotManager::cancel_auth`].
    fn cancel_auth_login(&self) {
        if let Some(token) = self
            .auth_cancel
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            token.cancel();
        }
        self.kill_auth_process_group();
        self.clear_one_shot_env();
        self.remove_pairing_code();
        *self.auth_pending_input.lock() = None;
        *self.auth_login_url.lock() = None;
    }

    fn auth_availability(&self) -> BotAuthAvailability {
        match self.name.as_str() {
            "telegram-self" => match self.telegram_auth_config() {
                Ok(config) if program_available(&config.binary, self.scope_paths.as_ref()) => {
                    BotAuthAvailability::Available
                },
                Ok(_) => BotAuthAvailability::Unavailable("The tgcli program is not installed"),
                Err(_) => BotAuthAvailability::Unavailable(
                    "Telegram API ID and hash (TGCLI_API_ID, TGCLI_API_HASH) are not configured",
                ),
            },
            "whatsapp" => match self.bot_entry_point() {
                Some(entry) if !Path::new(&entry).is_file() => {
                    BotAuthAvailability::Unavailable("The WhatsApp bot is not installed")
                },
                _ => BotAuthAvailability::Available,
            },
            _ => match self.google_workspace_auth_config() {
                Ok(Some(config)) => {
                    if !program_available(&config.gws_binary, self.scope_paths.as_ref()) {
                        BotAuthAvailability::Unavailable("The gws program is not installed")
                    } else if !google_oauth_client_available(&config, self.scope_paths.as_ref()) {
                        BotAuthAvailability::Unavailable(
                            "The Google OAuth Desktop client is not uploaded yet",
                        )
                    } else {
                        BotAuthAvailability::Available
                    }
                },
                Ok(None) => BotAuthAvailability::Unavailable(
                    "This Google profile is not set up yet; finish it in Magican Desktop",
                ),
                Err(_) => BotAuthAvailability::Unavailable(
                    "This Google profile's settings could not be read",
                ),
            },
        }
    }

    async fn submit_auth_input(&self, input: &str) -> Result<(), BotManagerError> {
        let mut auth_input = self.auth_input.lock().await;
        let Some(stdin) = auth_input.as_mut() else {
            return Err(BotManagerError::AuthFailed {
                name: self.name.clone(),
                reason: "this bot is not waiting for interactive auth input".to_string(),
            });
        };
        stdin
            .write_all(format!("{input}\n").as_bytes())
            .await
            .map_err(|error| BotManagerError::AuthFailed {
                name: self.name.clone(),
                reason: format!("failed to send interactive auth input: {error}"),
            })?;
        stdin
            .flush()
            .await
            .map_err(|error| BotManagerError::AuthFailed {
                name: self.name.clone(),
                reason: format!("failed to flush interactive auth input: {error}"),
            })?;
        // The prompt was answered; the next one (if any) sets it again.
        *self.auth_pending_input.lock() = None;
        self.push_log("auth", "submitted interactive auth input")
            .await;
        Ok(())
    }

    async fn is_desired_running(&self) -> bool {
        self.runtime.lock().await.desired_running
    }

    fn google_workspace_auth_config(&self) -> anyhow::Result<Option<GoogleWorkspaceAuthConfig>> {
        let bot_cwd = resolve_bot_cwd(&self.config, self.scope_paths.as_ref())?;
        let env_file =
            match resolve_bot_env_file(&self.config, self.scope_paths.as_ref(), &bot_cwd)? {
                Some(path) => path,
                None => return Ok(None),
            };
        if !env_file.exists() {
            return Ok(None);
        }
        let env_map = load_env_file(&env_file)
            .with_context(|| format!("failed to read env file `{}`", env_file.display()))?;
        let gws_config_dir = match env_map.get("GWS_CONFIG_DIR").map(|value| value.trim()) {
            Some(value) if !value.is_empty() => resolve_relative_path(value, &bot_cwd),
            _ => return Ok(None),
        };
        let gws_binary =
            resolve_google_workspace_binary(&bot_cwd, &env_map, self.scope_paths.as_ref());
        let expected_account = env_map
            .get("GWS_EXPECTED_EMAIL")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let profile_label = env_map
            .get("GWS_PROFILE_LABEL")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format_bot_name(&self.name));
        let project_id = env_map
            .get("GOOGLE_WORKSPACE_PROJECT_ID")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        Ok(Some(GoogleWorkspaceAuthConfig {
            gws_binary,
            gws_config_dir,
            profile_label,
            expected_account,
            project_id,
            open_browser: !self.auth_headless.load(std::sync::atomic::Ordering::SeqCst),
        }))
    }

    fn telegram_auth_config(&self) -> anyhow::Result<TelegramAuthConfig> {
        let bot_cwd = resolve_bot_cwd(&self.config, self.scope_paths.as_ref())?;
        let env_file = resolve_bot_env_file(&self.config, self.scope_paths.as_ref(), &bot_cwd)?
            .ok_or_else(|| anyhow!("telegram-self has no configured env file"))?;
        if !env_file.exists() {
            return Err(anyhow!(
                "telegram-self credentials are not configured at `{}`",
                env_file.display()
            ));
        }
        let env = load_env_file(&env_file)
            .with_context(|| format!("failed to read env file `{}`", env_file.display()))?;
        for key in ["TGCLI_API_ID", "TGCLI_API_HASH"] {
            if !env.get(key).is_some_and(|value| !value.trim().is_empty()) {
                return Err(anyhow!("{key} is required before Telegram pairing"));
            }
        }
        let binary = resolve_telegram_binary(&bot_cwd, &env, self.scope_paths.as_ref());
        let qr_path = self
            .qr_code_path()
            .ok_or_else(|| anyhow!("telegram-self has no scoped QR artifact path"))?;
        let profile_label = env
            .get("TGCLI_PROFILE_LABEL")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Telegram".to_string());

        Ok(TelegramAuthConfig {
            binary,
            bot_cwd,
            env,
            qr_path,
            profile_label,
            login: TelegramLogin::default(),
        })
    }

    async fn auth_snapshot(
        &self,
        flow_state: BotAuthFlowState,
    ) -> Result<BotAuthSnapshot, BotManagerError> {
        // Generic path: if the bot left a needs-auth sidecar behind on its
        // last run (any provider), surface that as the canonical "needs
        // auth" escalation. The sidecar is provider-agnostic, so this
        // works for gws / tgcli / wu-cli / kapso / any future adapter
        // without per-provider supervisor knowledge.
        if let Some(sidecar) = self.read_needs_auth_sidecar() {
            let mismatched = sidecar.current_account.as_ref().is_some_and(|current| {
                sidecar
                    .expected_account
                    .as_ref()
                    .is_some_and(|expected| !current.eq_ignore_ascii_case(expected))
            });
            return Ok(BotAuthSnapshot {
                name: self.name.clone(),
                supported: true,
                provider: Some(sidecar.provider),
                status: if mismatched {
                    BotAuthStatus::AccountMismatch
                } else {
                    BotAuthStatus::NeedsAuth
                },
                flow_state,
                profile_label: sidecar.profile_label,
                expected_account: sidecar.expected_account,
                current_account: sidecar.current_account,
                detail: sidecar.detail,
            });
        }

        if self.name == "telegram-self" {
            let config = match self.telegram_auth_config() {
                Ok(config) => config,
                Err(error) => {
                    return Ok(BotAuthSnapshot {
                        name: self.name.clone(),
                        supported: true,
                        provider: Some(NEEDS_AUTH_PROVIDER_TELEGRAM_SELF.to_string()),
                        status: BotAuthStatus::NeedsAuth,
                        flow_state,
                        profile_label: Some("Telegram".to_string()),
                        expected_account: None,
                        current_account: None,
                        detail: Some(error.to_string()),
                    });
                },
            };
            let status = match read_telegram_status(&config, self.scope_paths.as_ref(), false).await
            {
                Ok(status) => status,
                Err(error) => TelegramStatus {
                    current_account: None,
                    status: BotAuthStatus::Error,
                    detail: Some(error.to_string()),
                },
            };
            return Ok(BotAuthSnapshot {
                name: self.name.clone(),
                supported: true,
                provider: Some(NEEDS_AUTH_PROVIDER_TELEGRAM_SELF.to_string()),
                status: status.status,
                flow_state,
                profile_label: Some(config.profile_label),
                expected_account: None,
                current_account: status.current_account,
                detail: status.detail,
            });
        }

        if self.name == "whatsapp" {
            return Ok(self.whatsapp_auth_snapshot(flow_state).await);
        }

        // Fallback: probe Google Workspace status via `gws auth status`. This
        // is the "is the bot healthy" path used by the bot card while the
        // bot is running. Once every adapter writes the sidecar on real
        // failure, this branch will only return `Ok` (auth is fine) — it
        // is not the source of NeedsAuth escalations.
        let Some(config) =
            self.google_workspace_auth_config()
                .map_err(|reason| BotManagerError::AuthFailed {
                    name: self.name.clone(),
                    reason: reason.to_string(),
                })?
        else {
            return Ok(BotAuthSnapshot {
                name: self.name.clone(),
                supported: false,
                provider: None,
                status: BotAuthStatus::Unsupported,
                flow_state,
                profile_label: None,
                expected_account: None,
                current_account: None,
                detail: None,
            });
        };

        let status =
            match read_google_workspace_status(&config, self.scope_paths.as_ref(), false).await {
                Ok(status) => status,
                Err(err) => GoogleWorkspaceStatus {
                    current_account: None,
                    status: BotAuthStatus::Error,
                    detail: Some(err.to_string()),
                },
            };

        Ok(BotAuthSnapshot {
            name: self.name.clone(),
            supported: true,
            provider: Some(NEEDS_AUTH_PROVIDER_GOOGLE_WORKSPACE.to_string()),
            status: status.status,
            flow_state,
            profile_label: Some(config.profile_label),
            expected_account: config.expected_account,
            current_account: status.current_account,
            detail: status.detail,
        })
    }

    async fn whatsapp_auth_snapshot(&self, flow_state: BotAuthFlowState) -> BotAuthSnapshot {
        let status = if self
            .whatsapp_status_path()
            .is_some_and(|path| path.is_file())
        {
            BotAuthStatus::Ok
        } else {
            BotAuthStatus::NeedsAuth
        };
        let detail = match status {
            BotAuthStatus::Ok => None,
            _ if self.qr_code_path().is_some_and(|path| path.is_file()) => {
                Some("Scan the QR code in WhatsApp under Linked devices".to_string())
            },
            _ => Some("Waiting for WhatsApp to publish a pairing QR code".to_string()),
        };
        BotAuthSnapshot {
            name: self.name.clone(),
            supported: true,
            provider: Some(NEEDS_AUTH_PROVIDER_WHATSAPP_WEB.to_string()),
            status,
            flow_state,
            profile_label: Some("WhatsApp".to_string()),
            expected_account: None,
            current_account: None,
            detail,
        }
    }

    async fn start(self: &Arc<Self>) -> Result<BotStatusSnapshot, BotManagerError> {
        // Publishing a spawned child and its supervisor is one transition.
        // A disconnected API caller must not drop it halfway through.
        let bot = Arc::clone(self);
        tokio::spawn(async move { bot.start_serialized().await })
            .await
            .map_err(|error| BotManagerError::StartFailed {
                name: self.name.clone(),
                reason: format!("bot start task failed: {error}"),
            })?
    }

    async fn start_serialized(self: &Arc<Self>) -> Result<BotStatusSnapshot, BotManagerError> {
        let _lifecycle = self.lifecycle.lock().await;
        {
            let mut runtime = self.runtime.lock().await;
            runtime.clear_finished_handle();
            if runtime.supervisor_handle.is_some() {
                runtime.desired_running = true;
                return Ok(runtime.snapshot(&self.name, &self.config));
            }
            runtime.desired_running = true;
        }

        // Clear any stale needs-auth sidecar from the previous failed run so
        // a fresh start doesn't carry an old escalation. If the bot exits
        // 79 again it will rewrite the sidecar before exiting.
        self.clear_needs_auth_sidecar();

        // Reap orphan daemons left over from prior magician runs that
        // detached when their parent died. Without this, each restart
        // accumulates another node child holding stale env (we found 18
        // kapso zombies in the wild). Match by argv signature: same
        // entry-point .js file AND same --env-file path uniquely identify
        // a bot instance, so multi-account bots (gmail-business vs
        // gmail-work) don't clobber each other.
        //
        // The argv match misses a group whose bot already died while the CLIs
        // it forked (e.g. `gws gmail +watch`) live on; the recorded group id
        // reaches those.
        self.reap_recorded_process_group().await;
        let reaped = self.reap_orphan_instances();
        for pid in &reaped {
            self.push_log(
                "supervisor",
                format!("reaped orphan daemon (pid={pid}) before spawn"),
            )
            .await;
        }

        // Try the initial spawn. Two outcomes:
        //   1. Success → mark Running, hand the live `Child` to the
        //      supervisor.
        //   2. Failure with `auto_restart: true` → mark Restarting and hand
        //      `None` to the supervisor, which retries from zero via the
        //      same backoff loop a mid-life spawn failure would. This
        //      makes transient startup failures (e.g., `node` not yet on
        //      PATH because the supervisor / magician was launched from a
        //      non-interactive shell that hadn't sourced nvm) self-heal
        //      without requiring a manual `POST /bots/{name}/restart`.
        //   3. Failure with `auto_restart: false` → mark Failed and return
        //      Err immediately (legacy behavior; no surprise auto-recovery
        //      for bots explicitly configured against it).
        //
        // `{err:#}` walks the anyhow source chain so the actual OS-level
        // cause (`No such file or directory`, `Permission denied`, …)
        // shows up in logs and in the runtime's `last_error` field. Plain
        // `err.to_string()` dropped the cause and made transient startup
        // failures impossible to diagnose ("`node` failed to spawn" — but
        // why?).
        let (initial_child, initial_state, initial_log) = match self.spawn_child() {
            Ok(child) => {
                let pid = child.id();
                let log = format!(
                    "started bot command {}{}",
                    self.config.command,
                    pid.map(|value| format!(" (pid={value})"))
                        .unwrap_or_default()
                );
                (Some(child), BotRuntimeState::Running, log)
            },
            Err(err) if self.config.auto_restart => {
                let reason = format!("{err:#}");
                let log =
                    format!("initial spawn failed ({reason}); supervisor will retry with backoff");
                {
                    let mut runtime = self.runtime.lock().await;
                    runtime.last_error = Some(reason);
                }
                (None, BotRuntimeState::Restarting, log)
            },
            Err(err) => {
                let reason = format!("{err:#}");
                let mut runtime = self.runtime.lock().await;
                runtime.state = BotRuntimeState::Failed;
                runtime.pid = None;
                runtime.stopped_at = Some(Utc::now());
                runtime.restart_backoff_secs = None;
                runtime.last_error = Some(reason.clone());
                return Err(BotManagerError::StartFailed {
                    name: self.name.clone(),
                    reason,
                });
            },
        };

        let pid = initial_child.as_ref().and_then(Child::id);
        let stop_token = CancellationToken::new();
        {
            let mut runtime = self.runtime.lock().await;
            runtime.state = initial_state;
            runtime.pid = pid;
            runtime.started_at = if initial_child.is_some() {
                Some(Utc::now())
            } else {
                None
            };
            runtime.stopped_at = None;
            runtime.restart_backoff_secs = None;
            if initial_child.is_some() {
                runtime.last_error = None;
            }
            runtime.stop_token = Some(stop_token.clone());
        }

        self.push_log("supervisor", initial_log).await;

        let managed = Arc::clone(self);
        let handle = tokio::spawn(async move {
            managed.supervise(initial_child, stop_token).await;
        });

        let mut runtime = self.runtime.lock().await;
        runtime.supervisor_handle = Some(handle);
        Ok(runtime.snapshot(&self.name, &self.config))
    }

    async fn stop(self: &Arc<Self>) -> Result<BotStatusSnapshot, BotManagerError> {
        let pending = self.begin_stop().await;
        Ok(pending.finish(stop_join_deadline()).await)
    }

    /// Request a stop and return without waiting for the bot to exit.
    async fn begin_stop(self: &Arc<Self>) -> PendingBotStop {
        let lifecycle = Arc::clone(&self.lifecycle).lock_owned().await;
        // Revoke first: the credential must not outlive the process that holds
        // it, even briefly, and even if the teardown below fails partway.
        //
        // Scope-qualified: a bot name alone is not unique across scopes, so an
        // unqualified revoke would sign out another workspace's daemon of the
        // same name. No scope paths means none was ever minted.
        if let Some(scope_paths) = self.scope_paths.as_ref() {
            magician::magician_v2::auth::bot_token_registry().revoke_bot(
                &scope_paths.principal,
                &scope_paths.workspace,
                &self.name,
            );
        }

        let mut runtime = self.runtime.lock().await;
        runtime.clear_finished_handle();
        runtime.desired_running = false;
        runtime.restart_backoff_secs = None;

        if runtime.supervisor_handle.is_none() {
            return PendingBotStop {
                bot: Arc::clone(self),
                handle: None,
                pid: None,
                _lifecycle: lifecycle,
            };
        }

        runtime.state = BotRuntimeState::StopRequested;
        // Captured before the supervisor clears it, so a join that times out
        // can still reach the bot's process group.
        let pid = runtime.pid;
        if let Some(token) = runtime.stop_token.as_ref() {
            token.cancel();
        }
        PendingBotStop {
            bot: Arc::clone(self),
            handle: runtime.supervisor_handle.take(),
            pid,
            _lifecycle: lifecycle,
        }
    }

    async fn restart(self: &Arc<Self>) -> Result<BotStatusSnapshot, BotManagerError> {
        self.stop().await?;
        self.start().await
    }

    async fn run_google_workspace_auth(
        &self,
        config: &GoogleWorkspaceAuthConfig,
    ) -> anyhow::Result<String> {
        if let Some(parent) = config.gws_config_dir.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create Google Workspace config dir parent `{}`",
                    parent.display()
                )
            })?;
        }
        let cloudsdk_config_dir = config.ensure_cloudsdk_config_dir()?;
        install_google_oauth_client(config, self.scope_paths.as_ref())?;

        // Augment PATH with the scope's tool-bin dirs so `gws auth login`
        // (absolute gws, but `#!/usr/bin/env node` shebang needs node on
        // PATH) resolves instead of exiting 127. Fail-safe: no existing bin
        // dirs → leave the inherited PATH. Computed first so the program is
        // resolved against the PATH the child will see (see
        // `runtime_core::process`).
        let child_path = self.scope_paths.as_ref().and_then(|scope_paths| {
            let parent = std::env::var("PATH").unwrap_or_default();
            scope_paths.subprocess_bin_path(None, &parent)
        });
        let mut command = Command::new(resolve_program(
            config.gws_binary.as_os_str(),
            child_path.as_deref().map(OsStr::new),
        ));
        command
            .arg("auth")
            .arg("login")
            .arg("--scopes")
            .arg(GOOGLE_AUTH_SCOPES.join(","))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("GOOGLE_WORKSPACE_CLI_CONFIG_DIR", &config.gws_config_dir)
            .env("CLOUDSDK_CONFIG", &cloudsdk_config_dir);
        // `gws` is a node shim that forks the real binary (the one holding the
        // OAuth callback port); a group lets the timeout below reach both.
        #[cfg(unix)]
        command.process_group(0);

        if let Some(scope_paths) = self.scope_paths.as_ref() {
            command.env("HOME", &scope_paths.home_root);
            if let Some(p) = child_path.as_deref() {
                command.env("PATH", p);
            }
        }

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn Google Workspace auth for bot `{}` using `{}`",
                self.name,
                config.gws_binary.display()
            )
        })?;
        self.set_auth_process_group(child.id());
        *self.auth_login_url.lock() = None;

        let url_sink = GoogleLoginUrlSink {
            open_browser: config.open_browser,
            published: self.auth_login_url.handle(),
        };
        let bot_name = self.name.clone();
        let stdout_task = child.stdout.take().map(|stdout| {
            let sink = Arc::new(self.clone_for_logs());
            tokio::spawn(capture_auth_output(
                stdout,
                sink,
                "auth",
                bot_name.clone(),
                config.profile_label.clone(),
                url_sink.clone(),
            ))
        });
        let bot_name = self.name.clone();
        let stderr_task = child.stderr.take().map(|stderr| {
            let sink = Arc::new(self.clone_for_logs());
            tokio::spawn(capture_auth_output(
                stderr,
                sink,
                "auth",
                bot_name.clone(),
                config.profile_label.clone(),
                url_sink.clone(),
            ))
        });

        let status = match tokio::time::timeout(GOOGLE_AUTH_TIMEOUT, child.wait()).await {
            Ok(result) => result
                .with_context(|| format!("failed to wait for auth login for bot `{}`", self.name)),
            Err(_) => {
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    signal_process_group(pid, libc::SIGKILL);
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(anyhow!(
                    "Google sign-in was not completed within 10 minutes; click Re-authenticate to try again"
                ))
            },
        };
        self.set_auth_process_group(None);

        if let Some(task) = stdout_task {
            let _ = task.await;
        }
        if let Some(task) = stderr_task {
            let _ = task.await;
        }

        let status = status?;
        if !status.success() {
            let code = status
                .code()
                .map(|value| format!("code {value}"))
                .unwrap_or_else(|| "no exit code".to_string());
            return Err(anyhow!("Google Workspace auth login exited with {code}"));
        }

        let post_login =
            read_google_workspace_status(config, self.scope_paths.as_ref(), true).await?;
        if post_login.status == BotAuthStatus::AccountMismatch {
            purge_google_workspace_credentials(config);
        }
        if post_login.status != BotAuthStatus::Ok {
            return Err(anyhow!(post_login.detail.unwrap_or_else(|| {
                "Google Workspace auth did not reach a valid state".to_string()
            })));
        }

        Ok(post_login
            .current_account
            .unwrap_or_else(|| "unknown".to_string()))
    }

    async fn run_telegram_auth(&self, config: &TelegramAuthConfig) -> anyhow::Result<String> {
        if let Some(parent) = config.qr_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create Telegram pairing dir `{}`",
                    parent.display()
                )
            })?;
        }
        let _ = fs::remove_file(&config.qr_path);
        // tgcli reads its API id, hash and phone only from its store config
        // and otherwise prompts for them on stdin, which stalled both the QR
        // and the code login. A phone it still lacks is asked for (and
        // answered) as a `phone` prompt.
        seed_telegram_store_config(config, self.scope_paths.as_ref())?;
        *self.auth_pending_input.lock() = None;

        let mut command = telegram_command(config, self.scope_paths.as_ref());
        command.arg("auth");
        if !config.login.code {
            command.arg("--qr");
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn Telegram auth for bot `{}` using `{}`",
                self.name,
                config.binary.display()
            )
        })?;
        *self.auth_input.lock().await = child.stdin.take();
        let cancel = CancellationToken::new();
        self.set_auth_cancel(Some(cancel.clone()));

        let stdout_task = child.stdout.take().map(|stdout| {
            tokio::spawn(capture_telegram_auth_output(
                stdout,
                Arc::new(self.clone_for_logs()),
                self.name.clone(),
                config.qr_path.clone(),
                self.auth_pending_input.handle(),
                cancel.clone(),
            ))
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            let bot_name = self.name.clone();
            let sink = Arc::new(self.clone_for_logs());
            tokio::spawn(async move {
                capture_output(stderr, sink, "auth", &bot_name).await;
            })
        });

        enum LoginEnd {
            Exited(std::io::Result<std::process::ExitStatus>),
            TimedOut,
            Cancelled,
        }
        let end = tokio::select! {
            result = child.wait() => LoginEnd::Exited(result),
            _ = tokio::time::sleep(TELEGRAM_AUTH_TIMEOUT) => LoginEnd::TimedOut,
            _ = cancel.cancelled() => LoginEnd::Cancelled,
        };
        let status = match end {
            LoginEnd::Exited(result) => result
                .with_context(|| format!("failed to wait for Telegram auth for `{}`", self.name)),
            LoginEnd::TimedOut => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(anyhow!("Telegram pairing timed out after 10 minutes"))
            },
            LoginEnd::Cancelled => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err(anyhow!("Telegram sign-in was cancelled"))
            },
        };
        self.set_auth_cancel(None);
        *self.auth_pending_input.lock() = None;
        *self.auth_input.lock().await = None;
        if let Some(task) = stdout_task {
            let _ = task.await;
        }
        if let Some(task) = stderr_task {
            let _ = task.await;
        }
        let status = status?;
        if !status.success() {
            return Err(anyhow!(
                "Telegram auth exited {}",
                status
                    .code()
                    .map(|code| format!("with code {code}"))
                    .unwrap_or_else(|| "without an exit code".to_string())
            ));
        }

        let post_login = read_telegram_status(config, self.scope_paths.as_ref(), true).await?;
        if post_login.status != BotAuthStatus::Ok {
            return Err(anyhow!(post_login.detail.unwrap_or_else(|| {
                "Telegram auth did not reach a valid state".to_string()
            })));
        }
        let _ = fs::remove_file(&config.qr_path);
        Ok(post_login
            .current_account
            .unwrap_or_else(|| "Telegram account".to_string()))
    }

    async fn supervise(self: Arc<Self>, first_child: Option<Child>, stop_token: CancellationToken) {
        // `first_child` is `Some` when the initial `spawn_child()` succeeded
        // and the caller wants the supervisor to monitor the running child.
        // It is `None` when the caller hit a transient initial-spawn failure
        // (e.g., `node` not yet on PATH at boot) and asked the supervisor to
        // retry-from-zero. The loop body below handles both shapes uniformly
        // — `None` falls through to the `spawn_child()` branch on first
        // iteration, which goes through `handle_spawn_failure` → backoff →
        // sleep → retry, exactly like a mid-life spawn failure would.
        let mut restart_attempt = 0u32;
        let mut next_child = first_child;

        loop {
            let mut child = if let Some(child) = next_child.take() {
                child
            } else {
                match self.spawn_child() {
                    Ok(child) => {
                        self.mark_running(child.id()).await;
                        child
                    },
                    Err(err) => {
                        let backoff = match self
                            .handle_spawn_failure(err, restart_attempt, stop_token.is_cancelled())
                            .await
                        {
                            Some(backoff) => backoff,
                            None => break,
                        };
                        restart_attempt = restart_attempt.saturating_add(1);
                        self.sleep_for_restart(backoff, &stop_token).await;
                        if stop_token.is_cancelled() {
                            break;
                        }
                        continue;
                    },
                }
            };

            let exit = self.monitor_child(&mut child, stop_token.clone()).await;
            let backoff = match self
                .handle_exit(exit, restart_attempt, stop_token.is_cancelled())
                .await
            {
                Some(backoff) => backoff,
                None => break,
            };

            restart_attempt = restart_attempt.saturating_add(1);
            self.sleep_for_restart(backoff, &stop_token).await;
            if stop_token.is_cancelled() {
                break;
            }
        }

        let mut runtime = self.runtime.lock().await;
        runtime.pid = None;
        runtime.restart_backoff_secs = None;
        runtime.stop_token = None;
        runtime.supervisor_handle = None;
        if !runtime.desired_running && runtime.state != BotRuntimeState::Failed {
            runtime.state = BotRuntimeState::Stopped;
            runtime.stopped_at = Some(Utc::now());
        }
    }

    async fn handle_spawn_failure(
        &self,
        err: anyhow::Error,
        restart_attempt: u32,
        stop_requested: bool,
    ) -> Option<u64> {
        let mut runtime = self.runtime.lock().await;
        runtime.pid = None;
        runtime.stopped_at = Some(Utc::now());
        runtime.last_error = Some(err.to_string());

        if stop_requested || !runtime.desired_running {
            runtime.state = BotRuntimeState::Stopped;
            runtime.restart_backoff_secs = None;
            return None;
        }

        if !self.config.auto_restart {
            runtime.state = BotRuntimeState::Failed;
            runtime.restart_backoff_secs = None;
            return None;
        }

        let backoff = restart_backoff_secs(restart_attempt, self.config.restart_max_backoff_secs);
        runtime.state = BotRuntimeState::Restarting;
        runtime.restart_count = runtime.restart_count.saturating_add(1);
        runtime.restart_backoff_secs = Some(backoff);
        drop(runtime);

        self.push_log(
            "supervisor",
            format!("bot start failed, retrying in {backoff}s: {}", err),
        )
        .await;
        Some(backoff)
    }

    async fn handle_exit(
        &self,
        exit: BotExitSnapshot,
        restart_attempt: u32,
        stop_requested: bool,
    ) -> Option<u64> {
        let mut runtime = self.runtime.lock().await;
        runtime.pid = None;
        runtime.last_exit = Some(exit.clone());
        runtime.stopped_at = Some(exit.finished_at);
        runtime.restart_backoff_secs = None;

        if stop_requested || !runtime.desired_running {
            runtime.state = BotRuntimeState::Stopped;
            return None;
        }

        // Bot signalled "needs auth" via the dedicated exit code. Park in
        // Failed without auto-restart — the operator triggers the OAuth flow
        // via the UI's Authenticate button, which posts to
        // `/bots/{name}/auth/start`. Auto-restart here would just respawn the
        // bot, re-probe, and exit 79 again in a tight loop.
        if exit.code == Some(EX_NEEDS_AUTH) {
            runtime.state = BotRuntimeState::Failed;
            runtime.last_error = Some("bot requires authentication".to_string());
            drop(runtime);
            self.push_log(
                "supervisor",
                "bot reports needs-auth (exit code 79) — click Authenticate to run OAuth"
                    .to_string(),
            )
            .await;
            return None;
        }

        if !self.config.auto_restart {
            runtime.state = BotRuntimeState::Failed;
            runtime.last_error = Some(format!(
                "bot process exited unexpectedly{}",
                exit.code
                    .map(|code| format!(" with code {code}"))
                    .unwrap_or_else(|| " without an exit code".to_string())
            ));
            return None;
        }

        let backoff = restart_backoff_secs(restart_attempt, self.config.restart_max_backoff_secs);
        runtime.state = BotRuntimeState::Restarting;
        runtime.restart_count = runtime.restart_count.saturating_add(1);
        runtime.restart_backoff_secs = Some(backoff);
        drop(runtime);

        self.push_log(
            "supervisor",
            format!(
                "bot exited unexpectedly, restarting in {backoff}s{}",
                exit.code
                    .map(|code| format!(" (code={code})"))
                    .unwrap_or_default()
            ),
        )
        .await;

        Some(backoff)
    }

    async fn sleep_for_restart(&self, backoff: u64, stop_token: &CancellationToken) {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(backoff.max(1))) => {}
            _ = stop_token.cancelled() => {}
        }
    }

    async fn mark_running(&self, pid: Option<u32>) {
        let mut runtime = self.runtime.lock().await;
        runtime.state = BotRuntimeState::Running;
        runtime.pid = pid;
        runtime.started_at = Some(Utc::now());
        runtime.stopped_at = None;
        runtime.restart_backoff_secs = None;
        runtime.last_error = None;
    }

    async fn monitor_child(
        &self,
        child: &mut Child,
        stop_token: CancellationToken,
    ) -> BotExitSnapshot {
        // `wait` clears Child::id, but descendants can keep its pipes open
        // after the leader exits. Retain the group until cleanup completes.
        let pid = child.id();
        let stdout_task = child.stdout.take().map(|stdout| {
            let bot = Arc::new(self.clone_for_logs());
            let bot_name = self.name.clone();
            tokio::spawn(async move {
                capture_output(stdout, bot, "stdout", &bot_name).await;
            })
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            let bot = Arc::new(self.clone_for_logs());
            let bot_name = self.name.clone();
            tokio::spawn(async move {
                capture_output(stderr, bot, "stderr", &bot_name).await;
            })
        });

        let status_result = tokio::select! {
            status = child.wait() => status,
            _ = stop_token.cancelled() => {
                self.push_log("supervisor", "stop requested; terminating bot process").await;
                // SIGTERM the bot's whole group so its shutdown handler runs and
                // the CLIs it forked (e.g. `gws gmail +watch`) exit with it. A
                // SIGKILL to the bot alone left those running as orphans that
                // kept sharing the bot's credential files.
                #[cfg(unix)]
                let terminate_result = match child.id() {
                    Some(pid) => {
                        signal_process_group(pid, libc::SIGTERM);
                        Ok(())
                    },
                    None => child.start_kill(),
                };
                #[cfg(not(unix))]
                let terminate_result = child.start_kill();
                if let Err(err) = terminate_result {
                    self.push_log("supervisor", format!("failed to terminate child process: {err}")).await;
                }
                match tokio::time::timeout(STOP_TIMEOUT, child.wait()).await {
                    Ok(result) => result,
                    Err(_) => {
                        self.push_log("supervisor", "graceful stop timed out; force killing child process").await;
                        #[cfg(unix)]
                        if let Some(pid) = child.id() {
                            signal_process_group(pid, libc::SIGKILL);
                        }
                        if let Err(err) = child.kill().await {
                            self.push_log("supervisor", format!("failed to force kill child process: {err}")).await;
                        }
                        child.wait().await
                    }
                }
            }
        };

        self.end_process_group(pid, stop_token.is_cancelled()).await;
        for mut task in [stdout_task, stderr_task].into_iter().flatten() {
            // A child that escaped the group must not hold recovery forever
            // merely by retaining an inherited output descriptor.
            if tokio::time::timeout(STOP_JOIN_SLACK, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }

        match status_result {
            Ok(status) => BotExitSnapshot {
                success: status.success(),
                code: status.code(),
                finished_at: Utc::now(),
            },
            Err(err) => {
                self.push_log(
                    "supervisor",
                    format!("failed to wait for child process: {err}"),
                )
                .await;
                BotExitSnapshot {
                    success: false,
                    code: None,
                    finished_at: Utc::now(),
                }
            },
        }
    }

    /// Signal what is left of an exited bot's process group and drop its
    /// record. A stopped bot's group already had its SIGTERM grace while the
    /// bot shut down, so leftovers are SIGKILLed; after an unrequested exit
    /// they get a bounded SIGTERM grace before SIGKILL. The group id is the
    /// leader's pid, which is reserved while any group member is alive.
    async fn end_process_group(&self, pid: Option<u32>, stop_requested: bool) {
        #[cfg(unix)]
        if let Some(pid) = pid {
            if !stop_requested {
                signal_process_group(pid, libc::SIGTERM);
                let deadline = tokio::time::Instant::now() + STALE_GROUP_GRACE;
                while process_group_alive(pid) && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            if process_group_alive(pid) {
                signal_process_group(pid, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = (pid, stop_requested);
        self.clear_process_group_record();
    }

    fn set_auth_process_group(&self, pgid: Option<u32>) {
        if let Ok(mut slot) = self.auth_process_group.lock() {
            *slot = pgid;
        }
    }

    /// SIGKILL an in-flight `gws auth login` group. Its sign-in runs on a
    /// thread of its own whose `Child` is never dropped when Magician exits,
    /// so without this the login (and the callback port it holds) outlives
    /// Magician.
    fn kill_auth_process_group(&self) {
        let pgid = match self.auth_process_group.lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        #[cfg(unix)]
        if let Some(pgid) = pgid {
            signal_process_group(pgid, libc::SIGKILL);
        }
        #[cfg(not(unix))]
        let _ = pgid;
    }

    fn process_group_record_path(&self) -> Option<PathBuf> {
        let scope = self.scope_paths.as_ref()?;
        Some(
            scope
                .bots_root
                .join(".run")
                .join(format!("{}.pgid.json", self.name)),
        )
    }

    /// Best effort: a missing record only costs the next run its reap.
    fn write_process_group_record(&self, pgid: u32) {
        let Some(path) = self.process_group_record_path() else {
            return;
        };
        // Held until the record is on disk; see `live_process_group`.
        let mut live = self
            .live_process_group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *live = Some(pgid);
        let record = BotProcessGroupRecord {
            pgid,
            boot_time: System::boot_time(),
            spawned_at: unix_now_secs(),
        };
        let result = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| {
                serde_json::to_vec(&record)
                    .map_err(std::io::Error::other)
                    .and_then(|bytes| fs::write(&path, bytes))
            });
        if let Err(err) = result {
            warn!(
                bot = %self.name,
                path = %path.display(),
                error = %err,
                "[BOTS] failed to record bot process group"
            );
        }
    }

    fn clear_process_group_record(&self) {
        let mut live = self
            .live_process_group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *live = None;
        if let Some(path) = self.process_group_record_path() {
            let _ = fs::remove_file(path);
        }
    }

    /// Read and consume the on-disk record, unless it names the group this
    /// process is running for the bot. Under the `live_process_group` lock, so
    /// it cannot interleave with a concurrent spawn writing that record.
    fn take_stale_process_group_record(&self) -> Option<BotProcessGroupRecord> {
        let path = self.process_group_record_path()?;
        let live = self
            .live_process_group
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = fs::read(&path).ok()?;
        let record = serde_json::from_slice::<BotProcessGroupRecord>(&bytes).ok();
        if record.is_some_and(|record| *live == Some(record.pgid)) {
            return None;
        }
        let _ = fs::remove_file(&path);
        record
    }

    /// Reap the process group a previous Magician run recorded for this bot
    /// and never stopped (it crashed, or was SIGKILLed before its shutdown
    /// reached the bots). SIGTERM, then SIGKILL after `STALE_GROUP_GRACE`.
    /// Returns the reaped group id. Never touches a running bot of this
    /// process, a record from another boot, or a group whose live leader
    /// started at a different time than the one recorded (a reused pid).
    async fn reap_recorded_process_group(&self) -> Option<u32> {
        {
            let runtime = self.runtime.lock().await;
            if runtime.supervisor_handle.is_some() {
                return None;
            }
        }
        let record = self.take_stale_process_group_record()?;
        self.reap_process_group(record).await
    }

    #[cfg(unix)]
    async fn reap_process_group(&self, record: BotProcessGroupRecord) -> Option<u32> {
        if !recorded_group_is_ours(&record) {
            return None;
        }
        signal_process_group(record.pgid, libc::SIGTERM);
        let deadline = tokio::time::Instant::now() + STALE_GROUP_GRACE;
        while process_group_alive(record.pgid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        signal_process_group(record.pgid, libc::SIGKILL);
        self.push_log(
            "supervisor",
            format!(
                "reaped process group {} left by a previous run",
                record.pgid
            ),
        )
        .await;
        Some(record.pgid)
    }

    #[cfg(not(unix))]
    async fn reap_process_group(&self, _record: BotProcessGroupRecord) -> Option<u32> {
        None
    }

    fn spawn_child(&self) -> anyhow::Result<Child> {
        if let Some(status_path) = self.whatsapp_status_path() {
            let _ = fs::remove_file(status_path);
        }
        // A pairing code belongs to the process that requested it.
        self.remove_pairing_code();
        // A bundle link into a deleted checkout fails every restart with
        // MODULE_NOT_FOUND; repair it before each spawn, not only at boot.
        if let Some(scope_paths) = self.scope_paths.as_ref() {
            scope_paths.heal_bot_bundle_links();
        }
        let mut cmd = self.build_bot_command()?;
        // Taken, not read: a supervisor restart after this spawn never sees
        // the value again (the WhatsApp pairing phone is one-shot).
        for (key, value) in self.take_one_shot_env() {
            cmd.env(key, value);
        }
        let program = cmd.as_std().get_program().to_os_string();
        let child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn bot `{}` with command `{}`",
                self.name,
                program.to_string_lossy()
            )
        })?;
        if let Some(pid) = child.id() {
            self.write_process_group_record(pid);
        }
        Ok(child)
    }

    /// Everything `spawn_child` decides short of spawning: program, args,
    /// cwd, stdio and the child's environment (including the minted
    /// credential). Kept pure so the spawn contract can be asserted without
    /// starting a process.
    fn build_bot_command(&self) -> anyhow::Result<Command> {
        // A deleted workspace's bot is never (re)spawned and never minted a
        // credential — auto-restart included.
        if let Some(scope_paths) = self.scope_paths.as_ref() {
            let retired = scope_paths
                .capabilities_root
                .parent()
                .and_then(std::path::Path::parent)
                .is_some_and(|scopes_root| {
                    magician::magician_v2::auth::workspace_registry::is_retired_workspace(
                        scopes_root,
                        &scope_paths.principal,
                        &scope_paths.workspace,
                    )
                });
            if retired {
                return Err(anyhow!(
                    "workspace {}/{} was deleted; bot `{}` is not started",
                    scope_paths.principal,
                    scope_paths.workspace,
                    self.name
                ));
            }
        }
        let command = expand_runtime_value(&self.config.command, self.scope_paths.as_ref())
            .with_context(|| format!("failed to expand command for bot `{}`", self.name))?;
        if command.trim().is_empty() {
            return Err(anyhow!("command cannot be empty"));
        }

        // Augment PATH with the scope's tool-bin dirs so a bot daemon
        // launched by bare `node`/`npm` (or one that shells out to a
        // node-shebang tool) resolves the project-local runtime instead of
        // exiting 127 (see this launcher's missing-node failure note).
        // Applied before `config.env` below so an explicit per-bot PATH
        // still wins. Fail-safe: no existing bin dirs → inherited PATH.
        // Computed first so the program is resolved against the PATH the
        // child will see; a bare program plus an overridden PATH would
        // otherwise force `fork` instead of `posix_spawn` (see
        // `runtime_core::process`).
        let child_path = self.scope_paths.as_ref().and_then(|scope_paths| {
            let parent = std::env::var("PATH").unwrap_or_default();
            scope_paths.subprocess_bin_path(None, &parent)
        });
        // `config.env` is applied last, so a per-bot PATH there is the one
        // the child actually receives; resolve against it, not against the
        // augmentation it overrides — a program that exists only on the
        // per-bot PATH would otherwise stay bare and fork.
        let final_path = match self.config.env.get("PATH") {
            Some(value) => Some(
                expand_runtime_value(value, self.scope_paths.as_ref()).with_context(|| {
                    format!("failed to expand env var `PATH` for bot `{}`", self.name)
                })?,
            ),
            None => child_path.clone(),
        };
        let mut cmd = Command::new(resolve_program(
            OsStr::new(&command),
            final_path.as_deref().map(OsStr::new),
        ));
        cmd.kill_on_drop(true);
        // Own process group (pgid == pid), so a stop or the orphan reaper can
        // signal the bot together with the CLIs it forks.
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(scope_paths) = self.scope_paths.as_ref() {
            cmd.env("HOME", &scope_paths.home_root);
            if let Some(p) = child_path.as_deref() {
                cmd.env("PATH", p);
            }
        }

        // Tell the bot adapter where to publish its needs-auth sidecar (see
        // `BotNeedsAuthSidecar`). Adapters write here just before exiting
        // with `EX_NEEDS_AUTH` (79); supervisor reads it back to populate
        // the attention bar escalation. Parent dir is created lazily by
        // the adapter so we don't churn the filesystem on every spawn.
        if let Some(sidecar) = self.needs_auth_sidecar_path() {
            cmd.env("MAGICIAN_BOT_AUTH_SIDECAR_PATH", sidecar.as_os_str());
        }

        // Pairing artifacts are scope-local runtime state. Seed canonical
        // paths even when an older bot_configs.yaml predates these variables;
        // an explicit per-bot value below still wins.
        if !self.config.env.contains_key("WHATSAPP_QR_FILE") {
            if let Some(qr_path) = self.qr_code_path() {
                match self.name.as_str() {
                    "whatsapp" => {
                        cmd.env("WHATSAPP_QR_FILE", qr_path);
                    },
                    "telegram-self" => {
                        cmd.env("TELEGRAM_QR_FILE", qr_path);
                    },
                    _ => {},
                }
            }
        }
        if !self.config.env.contains_key("WHATSAPP_STATUS_FILE") {
            if let Some(status_path) = self.whatsapp_status_path() {
                cmd.env("WHATSAPP_STATUS_FILE", status_path);
            }
        }
        if !self.config.env.contains_key(WHATSAPP_PAIRING_CODE_FILE_ENV) {
            if let Some(code_path) = self.whatsapp_pairing_code_path() {
                cmd.env(WHATSAPP_PAIRING_CODE_FILE_ENV, code_path);
            }
        }

        for arg in &self.config.args {
            cmd.arg(
                expand_runtime_value(arg, self.scope_paths.as_ref()).with_context(|| {
                    format!("failed to expand arg `{arg}` for bot `{}`", self.name)
                })?,
            );
        }

        if let Some(cwd) = &self.config.cwd {
            cmd.current_dir(
                expand_runtime_value(cwd, self.scope_paths.as_ref()).with_context(|| {
                    format!("failed to expand cwd `{cwd}` for bot `{}`", self.name)
                })?,
            );
        }

        for (key, value) in &self.config.env {
            // The Magician credential is minted below, never configured. Skipped
            // here rather than merely overwritten afterwards: relying on write
            // order would make an auth property depend on statement sequence,
            // and a later refactor could silently reverse it.
            if key.eq_ignore_ascii_case(MAGICIAN_BEARER_TOKEN_ENV) {
                warn!(
                    bot = %self.name,
                    "[BOTS] ignoring {MAGICIAN_BEARER_TOKEN_ENV} from bot config: the runtime \
                     mints and injects a scoped token per spawn"
                );
                continue;
            }
            cmd.env(
                key,
                expand_runtime_value(value, self.scope_paths.as_ref()).with_context(|| {
                    format!("failed to expand env var `{key}` for bot `{}`", self.name)
                })?,
            );
        }

        // `--env-file` is read by node, not by us, so a stale key there cannot be
        // stripped. It IS overridden — node lets the inherited environment win
        // over the file (verified on v22) — but an operator who set it deserves
        // to be told it is inert rather than left to wonder.
        if let Some(env_file) = self.bot_env_file_arg() {
            if let Ok(contents) = std::fs::read_to_string(&env_file) {
                if contents.lines().any(|line| {
                    line.trim_start()
                        .to_ascii_uppercase()
                        .starts_with(&format!("{MAGICIAN_BEARER_TOKEN_ENV}="))
                }) {
                    warn!(
                        bot = %self.name,
                        env_file = %env_file,
                        "[BOTS] {MAGICIAN_BEARER_TOKEN_ENV} in the env file is ignored: the \
                         runtime injects a scoped token that takes precedence"
                    );
                }
            }
        }

        // The bot's Magician credential is DERIVED, never configured: minted
        // here for the scope whose `bots/` tree this config came from, injected
        // for exactly this process, and revoked when the bot stops. Nothing is
        // written to disk, so there is no credential at rest and no rotation.
        //
        // Two independent guarantees, so neither has to be trusted alone: the
        // config loop above REFUSES this key, and this write lands last anyway.
        // For `--env-file` the guarantee is node's own precedence (inherited
        // environment beats the file), which is why that path only warns.
        //
        // Minting also revokes any token still held by a previous incarnation of
        // this bot, so a crashed process's credential cannot outlive it.
        //
        // No scope paths means no scope to bind to; the bot then starts without
        // a credential and the API applies its own rules (bootstrap
        // anonymous/default while the identity store is empty, 401 after).
        if let Some(scope_paths) = self.scope_paths.as_ref() {
            let token = magician::magician_v2::auth::bot_token_registry().mint(
                magician::magician_v2::auth::BotGrant {
                    principal: scope_paths.principal.clone(),
                    workspace: scope_paths.workspace.clone(),
                    bot_name: self.name.clone(),
                },
            );
            cmd.env(MAGICIAN_BEARER_TOKEN_ENV, token);
        }

        Ok(cmd)
    }

    /// Find and SIGTERM (then SIGKILL after 500ms grace) any daemon whose
    /// argv signature matches this bot. Signature = both the entry-point
    /// path (.js / .mjs file in args) and, if present, the --env-file path.
    /// Skips our own PID. Used at start() to clean up orphans from prior
    /// magician runs that were detached on parent death.
    fn reap_orphan_instances(&self) -> Vec<u32> {
        let entry = match self.bot_entry_point() {
            Some(s) => s,
            None => return Vec::new(),
        };
        let env_file = self.bot_env_file_arg();
        let our_pid = std::process::id();

        let mut sys = System::new_with_specifics(
            RefreshKind::new().with_processes(ProcessRefreshKind::new()),
        );
        sys.refresh_processes();

        let mut targets: Vec<Pid> = Vec::new();
        for (pid, proc) in sys.processes() {
            if pid.as_u32() == our_pid {
                continue;
            }
            let cmd_str: String = proc.cmd().join(" ");
            if !cmd_str.contains(&entry) {
                continue;
            }
            if let Some(ref env_arg) = env_file {
                if !cmd_str.contains(env_arg) {
                    continue;
                }
            }
            targets.push(*pid);
        }

        if targets.is_empty() {
            return Vec::new();
        }

        let killed: Vec<u32> = targets.iter().map(|p| p.as_u32()).collect();
        for pid in &targets {
            if let Some(proc) = sys.process(*pid) {
                let _ = proc.kill_with(Signal::Term);
            }
            // An orphan spawned as a group leader takes its forked CLIs along.
            #[cfg(unix)]
            signal_process_group(pid.as_u32(), libc::SIGTERM);
        }
        thread::sleep(Duration::from_millis(500));

        let mut sys2 = System::new_with_specifics(
            RefreshKind::new().with_processes(ProcessRefreshKind::new()),
        );
        sys2.refresh_processes();
        for pid in &targets {
            if let Some(proc) = sys2.process(*pid) {
                let _ = proc.kill_with(Signal::Kill);
            }
            #[cfg(unix)]
            signal_process_group(pid.as_u32(), libc::SIGKILL);
        }

        killed
    }

    /// First arg ending in .js or .mjs (the node entry point). The bot
    /// configs put the entry point as the last positional arg after
    /// `--env-file=...`, so we just scan for the .js suffix.
    fn bot_entry_point(&self) -> Option<String> {
        for arg in &self.config.args {
            if arg.ends_with(".js") || arg.ends_with(".mjs") {
                if let Ok(expanded) = expand_runtime_value(arg, self.scope_paths.as_ref()) {
                    return Some(expanded);
                }
            }
        }
        None
    }

    /// The expanded `--env-file=<path>` argument, if the bot uses one.
    /// Returned in its full `--env-file=<path>` form so substring match
    /// against `ps` output is unambiguous.
    fn bot_env_file_arg(&self) -> Option<String> {
        for arg in &self.config.args {
            if arg.starts_with("--env-file=") {
                if let Ok(expanded) = expand_runtime_value(arg, self.scope_paths.as_ref()) {
                    return Some(expanded);
                }
            }
        }
        None
    }

    async fn push_log(&self, stream: &str, line: impl Into<String>) {
        let mut runtime = self.runtime.lock().await;
        runtime.push_log(stream, line.into());
    }

    fn clone_for_logs(&self) -> LogSink {
        LogSink {
            runtime: self.runtime.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct LogSink {
    runtime: Arc<Mutex<ManagedBotState>>,
}

impl LogSink {
    async fn push(&self, stream: &str, line: String) {
        let mut runtime = self.runtime.lock().await;
        runtime.push_log(stream, line);
    }
}

impl PendingBotStop {
    async fn finish(self, deadline: tokio::time::Instant) -> BotStatusSnapshot {
        // Keep the lifecycle lease through cleanup even if the request which
        // initiated the stop is cancelled while awaiting this join.
        let bot = Arc::clone(&self.bot);
        match tokio::spawn(self.finish_serialized(deadline)).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                bot.push_log("supervisor", format!("bot stop task failed: {error}"))
                    .await;
                bot.snapshot().await
            },
        }
    }

    /// Join the supervisor task by `deadline`. On timeout the task is
    /// aborted and the bot's whole group is SIGKILLed: the aborted task drops
    /// its `Child`, and `kill_on_drop` reaches only the bot itself.
    async fn finish_serialized(self, deadline: tokio::time::Instant) -> BotStatusSnapshot {
        let Self {
            bot,
            handle,
            pid,
            _lifecycle,
        } = self;
        if let Some(mut handle) = handle {
            match tokio::time::timeout_at(deadline, &mut handle).await {
                Ok(Ok(())) => {},
                Ok(Err(err)) => {
                    bot.push_log(
                        "supervisor",
                        format!("bot supervisor join failed during stop: {err}"),
                    )
                    .await;
                },
                Err(_) => {
                    handle.abort();
                    #[cfg(unix)]
                    if let Some(pid) = pid {
                        signal_process_group(pid, libc::SIGKILL);
                    }
                    bot.clear_process_group_record();
                    // Abort is a request, not a join. Let its final poll/drop
                    // finish before another start can own this runtime state.
                    let _ = handle.await;
                    bot.push_log(
                        "supervisor",
                        format!(
                            "stop timed out after {}s; aborting supervisor task",
                            STOP_TIMEOUT.as_secs()
                        ),
                    )
                    .await;
                },
            }
        }
        #[cfg(not(unix))]
        let _ = pid;

        let mut runtime = bot.runtime.lock().await;
        runtime.state = BotRuntimeState::Stopped;
        runtime.pid = None;
        runtime.restart_backoff_secs = None;
        runtime.stop_token = None;
        runtime.supervisor_handle = None;
        runtime.stopped_at = Some(Utc::now());
        runtime.snapshot(&bot.name, &bot.config)
    }
}

/// When a stop requested now must have finished joining.
fn stop_join_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + STOP_TIMEOUT + STOP_JOIN_SLACK
}

/// Join stops that were all requested up front against one shared deadline,
/// so shutting down N bots costs one grace period, not N.
async fn finish_bot_stops(pending: Vec<PendingBotStop>) {
    let deadline = stop_join_deadline();
    for stop in pending {
        stop.finish(deadline).await;
    }
}

impl ManagedBotState {
    fn snapshot(&self, name: &str, config: &BotProcessConfig) -> BotStatusSnapshot {
        let uptime_secs = self.started_at.as_ref().map(|started| {
            let duration = Utc::now().signed_duration_since(*started);
            duration.num_seconds().max(0)
        });

        BotStatusSnapshot {
            name: name.to_string(),
            enabled: config.enabled,
            auto_restart: config.auto_restart,
            qr_supported: config.env.contains_key("WHATSAPP_QR_FILE")
                || matches!(name, "whatsapp" | "telegram-self"),
            desired_running: self.desired_running,
            state: self.state.clone(),
            pid: self.pid,
            started_at: self.started_at,
            stopped_at: self.stopped_at,
            uptime_secs,
            restart_count: self.restart_count,
            restart_backoff_secs: self.restart_backoff_secs,
            last_exit: self.last_exit.clone(),
            last_error: self.last_error.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            cwd: config.cwd.clone(),
        }
    }

    fn clear_finished_handle(&mut self) {
        if self
            .supervisor_handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            self.supervisor_handle = None;
            self.stop_token = None;
        }
    }

    fn push_log(&mut self, stream: &str, line: String) {
        self.logs.push_back(BotLogLine {
            timestamp: Utc::now(),
            stream: stream.to_string(),
            line,
        });
        while self.logs.len() > MAX_LOG_LINES {
            self.logs.pop_front();
        }
    }
}

#[derive(Debug, Default)]
struct BotAuthCoordinatorState {
    active: Option<BotAuthSessionSnapshot>,
    queue: VecDeque<BotAuthSessionSnapshot>,
}

impl BotAuthCoordinatorState {
    fn snapshot(&self) -> BotAuthStateSnapshot {
        BotAuthStateSnapshot {
            active: self.active.clone(),
            queue: self.queue.iter().cloned().collect(),
        }
    }
}

#[derive(Debug, Clone)]
struct GoogleWorkspaceAuthConfig {
    gws_binary: PathBuf,
    gws_config_dir: PathBuf,
    profile_label: String,
    expected_account: Option<String>,
    /// `GOOGLE_WORKSPACE_PROJECT_ID` from the bot env, which overrides the
    /// project in the profile's `client_secret.json`.
    project_id: Option<String>,
    /// False for a headless login: the URL is published, not opened.
    open_browser: bool,
}

#[derive(Clone)]
struct TelegramAuthConfig {
    binary: PathBuf,
    bot_cwd: PathBuf,
    env: BTreeMap<String, String>,
    qr_path: PathBuf,
    profile_label: String,
    login: TelegramLogin,
}

/// QR login unless `code`; `phone` is `+digits` for tgcli's store config.
#[derive(Clone, Default)]
struct TelegramLogin {
    code: bool,
    phone: Option<String>,
}

/// Where a Google login's authorization URL goes: the host browser, or (for
/// a headless login) a slot only the API's private sign-in session reads.
#[derive(Clone)]
struct GoogleLoginUrlSink {
    open_browser: bool,
    published: Arc<std::sync::Mutex<Option<String>>>,
}

/// One-shot WhatsApp pairing phone, read by the bot adapter.
const WHATSAPP_PHONE_ENV: &str = "WHATSAPP_PHONE";
/// File the WhatsApp bot writes its pairing code to (beside the QR).
const WHATSAPP_PAIRING_CODE_FILE_ENV: &str = "WHATSAPP_PAIRING_CODE_FILE";

/// Digits of an E.164-style phone number (`+`, spaces, dashes, dots and
/// parentheses allowed around them), or `None`.
fn normalized_phone(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 20 {
        return None;
    }
    let mut digits = String::new();
    for (index, character) in trimmed.chars().enumerate() {
        match character {
            '0'..='9' => digits.push(character),
            '+' if index == 0 => {},
            ' ' | '-' | '.' | '(' | ')' => {},
            _ => return None,
        }
    }
    (6..=15).contains(&digits.len()).then_some(digits)
}

/// A WhatsApp pairing code as the bot published it (8 characters, an
/// optional dash in the middle), upper-cased; anything else is ignored.
fn normalize_pairing_code(raw: &str) -> Option<String> {
    let code = raw.trim();
    let compact: String = code.chars().filter(|character| *character != '-').collect();
    (compact.len() == 8
        && code.len() <= 9
        && compact
            .chars()
            .all(|character| character.is_ascii_alphanumeric()))
    .then(|| compact.to_ascii_uppercase())
}

/// Whether `program` resolves to a file on the PATH a managed login gets.
fn program_available(program: &Path, scope_paths: Option<&CapabilityScopePaths>) -> bool {
    let parent = std::env::var("PATH").unwrap_or_default();
    let search = scope_paths
        .and_then(|paths| paths.subprocess_bin_path(None, &parent))
        .unwrap_or(parent);
    resolve_program(program.as_os_str(), Some(OsStr::new(&search))).is_file()
}

/// Mirrors `install_google_oauth_client`: the data-root client, or one
/// already installed into this profile's config dir.
fn google_oauth_client_available(
    config: &GoogleWorkspaceAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> bool {
    config.gws_config_dir.join("client_secret.json").is_file()
        || scope_paths
            .and_then(|paths| paths.capabilities_root.ancestors().nth(3))
            .is_some_and(|root| root.join("client_secret.json").is_file())
}

/// tgcli's store directory, resolved exactly as tgcli does
/// (`core/store.js`): `TGCLI_STORE` relative to the bot's cwd, else the
/// per-platform app-data dir under the HOME the login runs with.
fn telegram_store_dir(
    config: &TelegramAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> Option<PathBuf> {
    if let Some(store) = config
        .env
        .get("TGCLI_STORE")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Some(resolve_relative_path(store, &config.bot_cwd));
    }
    let home = match scope_paths {
        Some(paths) => paths.home_root.clone(),
        None => PathBuf::from(std::env::var_os("HOME")?),
    };
    Some(if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("tgcli")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("tgcli")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local").join("share"))
            .join("tgcli")
    })
}

/// tgcli's `config.json` with the API id and hash (and a phone, when one was
/// given) set; every other key the operator or tgcli wrote is kept.
fn merged_tgcli_config(
    existing: Option<serde_json::Value>,
    api_id: &str,
    api_hash: &str,
    phone: Option<&str>,
) -> serde_json::Value {
    let mut object = existing
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    object.insert("apiId".to_string(), serde_json::json!(api_id.trim()));
    object.insert("apiHash".to_string(), serde_json::json!(api_hash.trim()));
    if let Some(phone) = phone {
        object.insert("phoneNumber".to_string(), serde_json::json!(phone));
    }
    serde_json::Value::Object(object)
}

fn seed_telegram_store_config(
    config: &TelegramAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> anyhow::Result<()> {
    let (Some(api_id), Some(api_hash)) = (
        config
            .env
            .get("TGCLI_API_ID")
            .filter(|v| !v.trim().is_empty()),
        config
            .env
            .get("TGCLI_API_HASH")
            .filter(|v| !v.trim().is_empty()),
    ) else {
        return Err(anyhow!(
            "TGCLI_API_ID and TGCLI_API_HASH are required before Telegram pairing"
        ));
    };
    let store = telegram_store_dir(config, scope_paths)
        .ok_or_else(|| anyhow!("the tgcli store directory could not be resolved"))?;
    let path = store.join("config.json");
    let existing = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(anyhow!("failed to read the tgcli store config: {error}"));
        },
    };
    let merged = merged_tgcli_config(existing, api_id, api_hash, config.login.phone.as_deref());
    let bytes = serde_json::to_vec_pretty(&merged)
        .map_err(|error| anyhow!("failed to encode the tgcli store config: {error}"))?;
    fs::create_dir_all(&store)
        .map_err(|error| anyhow!("failed to create the tgcli store directory: {error}"))?;
    write_private_text(&path, &bytes)
        .map_err(|error| anyhow!("failed to write the tgcli store config: {error}"))
}

/// What a tgcli prompt (stdout text without a trailing newline) asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TelegramPrompt {
    Input(BotAuthPendingInput),
    /// tgcli fell back to asking for its API id or hash; seeding failed.
    ApiCredentials,
}

fn telegram_auth_prompt(text: &str) -> Option<TelegramPrompt> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("enter the code you received") {
        Some(TelegramPrompt::Input(BotAuthPendingInput::Code))
    } else if lower.contains("2fa password") {
        Some(TelegramPrompt::Input(BotAuthPendingInput::Password))
    } else if lower.contains("telegram phone number") {
        Some(TelegramPrompt::Input(BotAuthPendingInput::Phone))
    } else if lower.contains("telegram api id") || lower.contains("telegram api hash") {
        Some(TelegramPrompt::ApiCredentials)
    } else {
        None
    }
}

impl TelegramAuthConfig {
    fn session(&self, name: &str) -> BotAuthSessionSnapshot {
        BotAuthSessionSnapshot {
            name: name.to_string(),
            provider: NEEDS_AUTH_PROVIDER_TELEGRAM_SELF.to_string(),
            profile_label: Some(self.profile_label.clone()),
            expected_account: None,
        }
    }
}

#[derive(Debug, Clone)]
struct TelegramStatus {
    current_account: Option<String>,
    status: BotAuthStatus,
    detail: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramStatusPayload {
    #[serde(default)]
    authenticated: bool,
    #[serde(default)]
    configured: bool,
    #[serde(default)]
    phone_number: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

impl GoogleWorkspaceAuthConfig {
    fn cloudsdk_config_dir(&self) -> PathBuf {
        self.gws_config_dir.join("cloudsdk")
    }

    fn ensure_cloudsdk_config_dir(&self) -> anyhow::Result<PathBuf> {
        let dir = self.cloudsdk_config_dir();
        fs::create_dir_all(&dir).with_context(|| {
            format!(
                "failed to create Google Cloud SDK config dir `{}`",
                dir.display()
            )
        })?;
        Ok(dir)
    }

    fn session(&self, name: &str) -> BotAuthSessionSnapshot {
        BotAuthSessionSnapshot {
            name: name.to_string(),
            provider: "google_workspace".to_string(),
            profile_label: Some(self.profile_label.clone()),
            expected_account: self.expected_account.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct GoogleWorkspaceStatus {
    current_account: Option<String>,
    status: BotAuthStatus,
    detail: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct GoogleWorkspaceStatusPayload {
    auth_method: Option<String>,
    storage: Option<String>,
    token_valid: Option<bool>,
    token_error: Option<String>,
    user: Option<String>,
    scopes: Option<Vec<String>>,
}

async fn capture_output<R>(reader: R, sink: Arc<LogSink>, stream: &'static str, bot_name: &str)
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                magician::magician_v2::analytics::emit(
                    magician::magician_v2::analytics::event_sink::AnalyticsEvent::bot_log(
                        bot_name, stream, &line,
                    ),
                );
                sink.push(stream, line).await;
            },
            Ok(None) => break,
            Err(err) => {
                sink.push(stream, format!("[log read error] {err}")).await;
                break;
            },
        }
    }
}

async fn capture_auth_output<R>(
    reader: R,
    sink: Arc<LogSink>,
    stream: &'static str,
    bot_name: String,
    profile_label: String,
    url_sink: GoogleLoginUrlSink,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut browser_opened = false;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Some(url) = extract_google_auth_url(&line) {
                    if !url_sink.open_browser {
                        // Headless: the URL goes only to the private sign-in
                        // session, and never into the bot's logs.
                        let fresh = {
                            let mut published = url_sink
                                .published
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let fresh = published.is_none();
                            *published = Some(url);
                            fresh
                        };
                        if fresh {
                            sink.push(
                                stream,
                                format!(
                                    "{profile_label} sign-in link is ready for the private browser"
                                ),
                            )
                            .await;
                        }
                        continue;
                    }
                    if !browser_opened {
                        browser_opened = true;
                        sink.push(stream, format!("opening browser for {profile_label} auth"))
                            .await;
                        open_auth_url(&url, &bot_name, &sink, stream).await;
                    }
                }
                sink.push(stream, line).await;
            },
            Ok(None) => break,
            Err(err) => {
                sink.push(stream, format!("[auth log read error] {err}"))
                    .await;
                break;
            },
        }
    }
}

async fn capture_telegram_auth_output<R>(
    reader: R,
    sink: Arc<LogSink>,
    bot_name: String,
    qr_path: PathBuf,
    pending_input: Arc<std::sync::Mutex<Option<BotAuthPendingInput>>>,
    cancel: CancellationToken,
) where
    R: AsyncRead + Unpin,
{
    // tgcli prompts with readline's `question`, which prints no newline and
    // then waits on stdin, so prompts are found in the unterminated tail
    // as well as in whole lines.
    let mut reader = reader;
    let mut chunk = [0u8; 4096];
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                sink.push("auth", format!("[auth log read error] {error}"))
                    .await;
                break;
            },
        };
        buffer.extend_from_slice(&chunk[..read]);
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&raw)
                .trim_end_matches(['\r', '\n'])
                .to_string();
            handle_telegram_auth_line(line, &sink, &bot_name, &qr_path, &pending_input, &cancel)
                .await;
        }
        let tail = String::from_utf8_lossy(&buffer).to_string();
        if tail.trim_end().ends_with(':') && telegram_auth_prompt(&tail).is_some() {
            buffer.clear();
            handle_telegram_auth_line(tail, &sink, &bot_name, &qr_path, &pending_input, &cancel)
                .await;
        } else if buffer.len() > 64 * 1024 {
            buffer.clear();
            handle_telegram_auth_line(tail, &sink, &bot_name, &qr_path, &pending_input, &cancel)
                .await;
        }
    }
    if !buffer.is_empty() {
        let tail = String::from_utf8_lossy(&buffer).to_string();
        handle_telegram_auth_line(tail, &sink, &bot_name, &qr_path, &pending_input, &cancel).await;
    }
}

async fn handle_telegram_auth_line(
    line: String,
    sink: &Arc<LogSink>,
    bot_name: &str,
    qr_path: &Path,
    pending_input: &Arc<std::sync::Mutex<Option<BotAuthPendingInput>>>,
    cancel: &CancellationToken,
) {
    if let Some(url) = extract_telegram_qr_url(&line) {
        match write_private_text(qr_path, url.as_bytes()) {
            Ok(()) => {
                sink.push("auth", "Telegram pairing QR is ready".to_string())
                    .await
            },
            Err(error) => {
                sink.push(
                    "auth",
                    format!("failed to publish Telegram pairing QR: {error}"),
                )
                .await
            },
        }
        return;
    }
    match telegram_auth_prompt(&line) {
        Some(TelegramPrompt::Input(kind)) => {
            *pending_input
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kind);
        },
        Some(TelegramPrompt::ApiCredentials) => {
            sink.push(
                "auth",
                "tgcli asked for its API id or hash; set TGCLI_API_ID and TGCLI_API_HASH and retry"
                    .to_string(),
            )
            .await;
            cancel.cancel();
            return;
        },
        None => {},
    }
    if line.trim().is_empty() {
        return;
    }
    magician::magician_v2::analytics::emit(
        magician::magician_v2::analytics::event_sink::AnalyticsEvent::bot_log(
            bot_name, "auth", &line,
        ),
    );
    sink.push("auth", line).await;
}

fn extract_telegram_qr_url(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("QR login URL:")
        .map(str::trim)
        .filter(|value| value.starts_with("tg://"))
}

fn write_private_text(path: &Path, value: &[u8]) -> std::io::Result<()> {
    magician::magician_v2::artifact_v2::io::write_bytes_durably_with_mode_sync(
        path,
        value,
        Some(0o600),
    )
}

fn install_google_oauth_client(
    config: &GoogleWorkspaceAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> anyhow::Result<()> {
    let destination = config.gws_config_dir.join("client_secret.json");
    let root = scope_paths
        .and_then(|paths| paths.capabilities_root.ancestors().nth(3))
        .ok_or_else(|| anyhow!("Magician data root is unavailable for this scoped bot"))?;
    let source = root.join("client_secret.json");
    if !source.is_file() {
        if destination.is_file() {
            return Ok(());
        }
        return Err(anyhow!(
            "Google OAuth Desktop client is missing; upload client_secret.json in Desktop setup"
        ));
    }
    let content = fs::read(&source)
        .with_context(|| format!("failed to read OAuth client `{}`", source.display()))?;
    write_private_text(&destination, &content).with_context(|| {
        format!(
            "failed to install OAuth client into `{}`",
            destination.display()
        )
    })
}

async fn open_auth_url(url: &str, bot_name: &str, sink: &Arc<LogSink>, stream: &'static str) {
    let mut open = Command::new("open");
    open.arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    match open.spawn() {
        Ok(_) => {},
        Err(err) => {
            sink.push(
                stream,
                format!("failed to open browser for {bot_name} auth URL: {err}"),
            )
            .await;
        },
    }
}

fn extract_google_auth_url(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|part| {
        let trimmed = part.trim_matches(|char| char == '"' || char == '\'' || char == ',');
        if trimmed.starts_with("https://accounts.google.com/") {
            Some(trimmed.to_string())
        } else {
            None
        }
    })
}

/// Google Workspace auth status is a *setup-time* property: a bot's stored
/// OAuth credentials do not change between routine status polls, yet probing it
/// live spawns a `gws auth status` subprocess that in turn shells out to
/// `gcloud`. The attention bar polls bot auth every few seconds, so an
/// un-throttled probe becomes a tight `gcloud` loop (thousands of invocations
/// an hour). We therefore cache the probe result and only re-run the live probe
/// when the cached entry is older than this TTL, or when a caller forces a
/// refresh (e.g. right after an interactive login). Real auth failures are
/// still surfaced in real time by the per-bot auth sidecar, which the poll path
/// consults *before* ever falling back to this probe.
const GWS_STATUS_PROBE_TTL: Duration = Duration::from_secs(600);

#[allow(clippy::type_complexity)]
fn gws_status_cache() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, (std::time::Instant, GoogleWorkspaceStatus)>,
> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<String, (std::time::Instant, GoogleWorkspaceStatus)>,
        >,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

async fn read_google_workspace_status(
    config: &GoogleWorkspaceAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
    force_fresh: bool,
) -> anyhow::Result<GoogleWorkspaceStatus> {
    // Cache key is the distinct auth context: same credentials dir + same
    // expected account => same status, so sharing one entry is correct.
    let cache_key = format!(
        "{}|{}",
        config.gws_config_dir.display(),
        config.expected_account.as_deref().unwrap_or("*")
    );

    if !force_fresh {
        if let Ok(cache) = gws_status_cache().lock() {
            if let Some((stored_at, status)) = cache.get(&cache_key) {
                if stored_at.elapsed() < GWS_STATUS_PROBE_TTL {
                    return Ok(status.clone());
                }
            }
        }
        // Guard dropped here; never held across the probe await below.
    }

    let status = probe_google_workspace_status(config, scope_paths).await?;

    if let Ok(mut cache) = gws_status_cache().lock() {
        cache.insert(cache_key, (std::time::Instant::now(), status.clone()));
    }

    Ok(status)
}

async fn probe_google_workspace_status(
    config: &GoogleWorkspaceAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> anyhow::Result<GoogleWorkspaceStatus> {
    let cloudsdk_config_dir = config.ensure_cloudsdk_config_dir()?;
    // Augment PATH with the scope's tool-bin dirs so `gws auth status`
    // (absolute gws, but `#!/usr/bin/env node` shebang needs node on PATH)
    // resolves instead of exiting 127 and falsely reporting auth broken.
    // Fail-safe: no existing bin dirs → leave the inherited PATH. Computed
    // first so the program is resolved against the PATH the child will see
    // (see `runtime_core::process`).
    let child_path = scope_paths.and_then(|paths| {
        let parent = std::env::var("PATH").unwrap_or_default();
        paths.subprocess_bin_path(None, &parent)
    });
    let mut command = Command::new(resolve_program(
        config.gws_binary.as_os_str(),
        child_path.as_deref().map(OsStr::new),
    ));
    command
        .arg("auth")
        .arg("status")
        .arg("--format")
        .arg("json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GOOGLE_WORKSPACE_CLI_CONFIG_DIR", &config.gws_config_dir)
        .env("CLOUDSDK_CONFIG", &cloudsdk_config_dir);

    if let Some(paths) = scope_paths {
        command.env("HOME", &paths.home_root);
        if let Some(p) = child_path.as_deref() {
            command.env("PATH", p);
        }
    }

    let output = command.output().await.with_context(|| {
        format!(
            "failed to run `{}` auth status --format json",
            config.gws_binary.display()
        )
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("gws auth status exited with {}", output.status)
        } else {
            stderr
        };
        return Ok(GoogleWorkspaceStatus {
            current_account: None,
            status: BotAuthStatus::Error,
            detail: Some(detail),
        });
    }

    let payload: GoogleWorkspaceStatusPayload =
        serde_json::from_str(&stdout).with_context(|| "failed to parse gws auth status json")?;

    if payload.auth_method.as_deref() == Some("none") || payload.storage.as_deref() == Some("none")
    {
        return Ok(GoogleWorkspaceStatus {
            current_account: payload.user,
            status: BotAuthStatus::NeedsAuth,
            detail: Some("no stored Google Workspace credentials".to_string()),
        });
    }

    if payload.token_valid == Some(false) {
        return Ok(GoogleWorkspaceStatus {
            current_account: payload.user,
            status: BotAuthStatus::NeedsAuth,
            detail: Some(
                payload
                    .token_error
                    .unwrap_or_else(|| "Google Workspace token is invalid".to_string()),
            ),
        });
    }

    let granted_scopes = payload.scopes.unwrap_or_default();
    let missing_scopes: Vec<&str> = GOOGLE_AUTH_SCOPES
        .iter()
        .copied()
        .filter(|scope| !granted_scopes.iter().any(|granted| granted == scope))
        .collect();
    if !missing_scopes.is_empty() {
        return Ok(GoogleWorkspaceStatus {
            current_account: payload.user,
            status: BotAuthStatus::NeedsAuth,
            detail: Some(format!(
                "missing required scopes: {}",
                missing_scopes.join(", ")
            )),
        });
    }

    let current_account = payload.user.filter(|value| !value.trim().is_empty());
    if let Some(expected_account) = config.expected_account.as_ref() {
        match current_account.as_ref() {
            Some(current) if current.eq_ignore_ascii_case(expected_account) => {},
            Some(current) => {
                return Ok(GoogleWorkspaceStatus {
                    current_account: Some(current.clone()),
                    status: BotAuthStatus::AccountMismatch,
                    detail: Some(format!(
                        "authenticated as {current}; expected {expected_account}"
                    )),
                });
            },
            None => {
                return Ok(GoogleWorkspaceStatus {
                    current_account: None,
                    status: BotAuthStatus::NeedsAuth,
                    detail: Some(format!(
                        "no authenticated Google account; expected {expected_account}"
                    )),
                });
            },
        }
    }

    Ok(GoogleWorkspaceStatus {
        current_account,
        status: BotAuthStatus::Ok,
        detail: None,
    })
}

const TELEGRAM_STATUS_PROBE_TTL: Duration = Duration::from_secs(60);

#[allow(clippy::type_complexity)]
fn telegram_status_cache() -> &'static std::sync::Mutex<
    std::collections::HashMap<String, (std::time::Instant, TelegramStatus)>,
> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, TelegramStatus)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

async fn read_telegram_status(
    config: &TelegramAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
    force_fresh: bool,
) -> anyhow::Result<TelegramStatus> {
    let cache_key = format!(
        "{}|{}",
        config.binary.display(),
        config
            .env
            .get("TGCLI_STORE")
            .map(String::as_str)
            .unwrap_or("*")
    );
    if !force_fresh {
        if let Ok(cache) = telegram_status_cache().lock() {
            if let Some((stored_at, status)) = cache.get(&cache_key) {
                if stored_at.elapsed() < TELEGRAM_STATUS_PROBE_TTL {
                    return Ok(status.clone());
                }
            }
        }
    }

    let status = probe_telegram_status(config, scope_paths).await?;
    if let Ok(mut cache) = telegram_status_cache().lock() {
        cache.insert(cache_key, (std::time::Instant::now(), status.clone()));
    }
    Ok(status)
}

async fn probe_telegram_status(
    config: &TelegramAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> anyhow::Result<TelegramStatus> {
    let mut command = telegram_command(config, scope_paths);
    command
        .arg("--json")
        .arg("auth")
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| anyhow!("tgcli auth status timed out"))?
        .with_context(|| {
            format!(
                "failed to run `{}` --json auth status",
                config.binary.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Ok(TelegramStatus {
            current_account: None,
            status: BotAuthStatus::Error,
            detail: Some(if stderr.is_empty() {
                format!("tgcli auth status exited with {}", output.status)
            } else {
                stderr
            }),
        });
    }
    let payload: TelegramStatusPayload = serde_json::from_slice(&output.stdout)
        .with_context(|| "failed to parse tgcli auth status json")?;
    let current_account = payload
        .username
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("@{value}"))
        .or_else(|| {
            payload
                .phone_number
                .filter(|value| !value.trim().is_empty())
        });
    if !payload.configured || !payload.authenticated {
        return Ok(TelegramStatus {
            current_account,
            status: BotAuthStatus::NeedsAuth,
            detail: Some("Telegram account is not paired".to_string()),
        });
    }
    Ok(TelegramStatus {
        current_account,
        status: BotAuthStatus::Ok,
        detail: None,
    })
}

fn purge_google_workspace_credentials(config: &GoogleWorkspaceAuthConfig) {
    for filename in ["credentials.enc", "credentials.json", "token_cache.json"] {
        let path = config.gws_config_dir.join(filename);
        if let Err(err) = fs::remove_file(&path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %path.display(), error = %err, "Failed to purge mismatched Google Workspace credential artifact");
            }
        }
    }
}

fn load_env_file(path: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for entry in dotenvy::from_path_iter(path)
        .with_context(|| format!("failed to open env file `{}`", path.display()))?
    {
        let (key, value) =
            entry.with_context(|| format!("failed to parse env entry in `{}`", path.display()))?;
        values.insert(key, value);
    }
    Ok(values)
}

fn resolve_bot_cwd(
    config: &BotProcessConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> anyhow::Result<PathBuf> {
    if let Some(cwd) = config.cwd.as_deref() {
        let expanded = expand_runtime_value(cwd, scope_paths)?;
        return Ok(PathBuf::from(expanded));
    }
    std::env::current_dir().map_err(Into::into)
}

fn resolve_bot_env_file(
    config: &BotProcessConfig,
    scope_paths: Option<&CapabilityScopePaths>,
    bot_cwd: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(arg) = config
        .args
        .iter()
        .find_map(|value| value.strip_prefix("--env-file="))
    else {
        return Ok(None);
    };
    let expanded = expand_runtime_value(arg, scope_paths)?;
    Ok(Some(resolve_relative_path(&expanded, bot_cwd)))
}

fn resolve_relative_path(value: &str, base_dir: &Path) -> PathBuf {
    let candidate = PathBuf::from(value);
    if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    }
}

fn resolve_google_workspace_binary(
    bot_cwd: &Path,
    env_map: &BTreeMap<String, String>,
    scope_paths: Option<&CapabilityScopePaths>,
) -> PathBuf {
    if let Some(binary) = env_map
        .get("GWS_BINARY")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return resolve_relative_path(binary, bot_cwd);
    }

    let local_binary = bot_cwd.join("node_modules").join(".bin").join("gws");
    if local_binary.exists() {
        return local_binary;
    }

    // Post-umbrella-collapse layout: gws lives in the shared skillshub
    // npm workspace, not under any per-scope node_modules. The legacy
    // `<bot_cwd>/../../node_modules/.bin/gws` fallback resolved to
    // `<scope>/node_modules/.bin/gws` under the old umbrella but is
    // stale now and would ENOENT.
    if let Some(paths) = scope_paths {
        let skillshub_binary = paths.node_modules_bin.join("gws");
        if skillshub_binary.exists() {
            return skillshub_binary;
        }
    }

    bot_cwd
        .join("..")
        .join("..")
        .join("node_modules")
        .join(".bin")
        .join("gws")
}

fn resolve_telegram_binary(
    bot_cwd: &Path,
    env_map: &BTreeMap<String, String>,
    scope_paths: Option<&CapabilityScopePaths>,
) -> PathBuf {
    if let Some(binary) = env_map
        .get("TGCLI_BINARY")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let candidate = PathBuf::from(binary);
        return if candidate.is_absolute() || candidate.components().count() == 1 {
            candidate
        } else {
            bot_cwd.join(candidate)
        };
    }

    let local_binary = bot_cwd.join("node_modules").join(".bin").join("tgcli");
    if local_binary.exists() {
        return local_binary;
    }
    if let Some(paths) = scope_paths {
        let skillshub_binary = paths.node_modules_bin.join("tgcli");
        if skillshub_binary.exists() {
            return skillshub_binary;
        }
    }
    let legacy = bot_cwd
        .join("..")
        .join("..")
        .join("node_modules")
        .join(".bin")
        .join("tgcli");
    if legacy.exists() {
        legacy
    } else {
        PathBuf::from("tgcli")
    }
}

fn telegram_command(
    config: &TelegramAuthConfig,
    scope_paths: Option<&CapabilityScopePaths>,
) -> Command {
    let child_path = scope_paths.and_then(|paths| {
        let parent = std::env::var("PATH").unwrap_or_default();
        paths.subprocess_bin_path(None, &parent)
    });
    let mut command = Command::new(resolve_program(
        config.binary.as_os_str(),
        child_path.as_deref().map(OsStr::new),
    ));
    command.current_dir(&config.bot_cwd);
    if let Some(paths) = scope_paths {
        command.env("HOME", &paths.home_root);
        if let Some(path) = child_path.as_deref() {
            command.env("PATH", path);
        }
    }
    for key in ["TGCLI_API_ID", "TGCLI_API_HASH", "TGCLI_STORE"] {
        if let Some(value) = config.env.get(key).filter(|value| !value.trim().is_empty()) {
            command.env(key, value);
        }
    }
    command
}

fn format_bot_name(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn spawn_auth_task(
    manager: BotManager,
    session: BotAuthSessionSnapshot,
    auth_config: GoogleWorkspaceAuthConfig,
) {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("auth runtime");
        runtime.block_on(async move {
            manager
                .run_google_workspace_auth_flow(session, auth_config)
                .await;
        });
    });
}

fn spawn_telegram_auth_task(
    manager: BotManager,
    session: BotAuthSessionSnapshot,
    auth_config: TelegramAuthConfig,
) {
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("telegram auth runtime");
        runtime.block_on(async move {
            manager.run_telegram_auth_flow(session, auth_config).await;
        });
    });
}

fn expand_env(value: &str) -> anyhow::Result<String> {
    shellexpand::env(value)
        .map(|expanded| expanded.into_owned())
        .map_err(|err| anyhow!(err))
}

fn expand_runtime_value(
    value: &str,
    scope_paths: Option<&CapabilityScopePaths>,
) -> anyhow::Result<String> {
    let value = match scope_paths {
        Some(paths) => paths.apply_vars(value),
        None => value.to_string(),
    };
    expand_env(&value)
}

/// Signals the process group led by `pid` (bots and `gws auth login` are
/// spawned with `process_group(0)`).
#[cfg(unix)]
fn signal_process_group(pid: u32, signal: libc::c_int) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    // `kill(0)` would signal our own group and `kill(-1)` every process we
    // may signal; a recorded id read back from disk must never become either.
    if pid <= 1 {
        return;
    }
    // SAFETY: `kill(2)` with a negative pid signals a process group and touches
    // no Rust memory; a group that no longer exists yields ESRCH.
    unsafe {
        libc::kill(-pid, signal);
    }
}

/// Whether a process group we may signal still has members.
#[cfg(unix)]
fn process_group_alive(pgid: u32) -> bool {
    let Ok(pgid) = i32::try_from(pgid) else {
        return false;
    };
    if pgid <= 1 {
        return false;
    }
    // SAFETY: signal 0 only probes for existence and permission.
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// Whether a group recorded by a previous run is still that run's bot tree:
/// same boot, not our own group, still alive, and — when its leader is still
/// running — a leader that started when the record was written. A leaderless
/// group is accepted: its id stays reserved while any member lives, so it
/// cannot have been handed to an unrelated process.
#[cfg(unix)]
fn recorded_group_is_ours(record: &BotProcessGroupRecord) -> bool {
    if record.pgid <= 1 || record.pgid == std::process::id() {
        return false;
    }
    // SAFETY: `getpgrp` has no preconditions and cannot fail.
    let own_group = unsafe { libc::getpgrp() };
    if i32::try_from(record.pgid).is_ok_and(|pgid| pgid == own_group) {
        return false;
    }
    if System::boot_time().abs_diff(record.boot_time) > PROCESS_GROUP_RECORD_CLOCK_SLACK_SECS {
        return false;
    }
    if !process_group_alive(record.pgid) {
        return false;
    }
    let leader = Pid::from_u32(record.pgid);
    let mut sys = System::new();
    if sys.refresh_process(leader) {
        if let Some(process) = sys.process(leader) {
            return process.start_time().abs_diff(record.spawned_at)
                <= PROCESS_GROUP_RECORD_CLOCK_SLACK_SECS;
        }
    }
    true
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn restart_backoff_secs(restart_attempt: u32, max_backoff_secs: u64) -> u64 {
    let shift = restart_attempt.min(5);
    let candidate = 1u64 << shift;
    candidate.min(max_backoff_secs.max(1))
}

fn start_failed_with_rollback(
    name: String,
    original: BotManagerError,
    rollback: BotManagerError,
) -> BotManagerError {
    BotManagerError::StartFailed {
        name,
        reason: format!(
            "{}; rollback failed: {}",
            bot_manager_error_reason(&original),
            bot_manager_error_reason(&rollback)
        ),
    }
}

fn bot_manager_error_reason(err: &BotManagerError) -> String {
    match err {
        BotManagerError::UnknownBot(name) => format!("bot `{name}` is not configured"),
        BotManagerError::StartFailed { reason, .. } => reason.clone(),
        BotManagerError::AuthUnsupported { name } => {
            format!("bot `{name}` does not support managed auth")
        },
        BotManagerError::AuthFailed { reason, .. } => reason.clone(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use super::*;
    use tempfile::tempdir;

    fn test_scope_paths(data_root: &Path) -> CapabilityScopePaths {
        // Mirror production: capabilities_root is the scope root, four
        // levels deep below `data_root`, so `ancestors().nth(3)` walks
        // up to data_root for system-skills resolution.
        let capabilities_root = data_root.join("scopes").join("anonymous").join("default");
        CapabilityScopePaths {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            capabilities_root: capabilities_root.clone(),
            bots_root: capabilities_root.join("bots"),
            auth_root: capabilities_root.join("auth"),
            workdirs_root: capabilities_root.join("workdirs"),
            home_root: capabilities_root.join("workdirs").join("home"),
            node_modules_bin: std::path::PathBuf::from("/dev/null/skillshub_bin"),
            node_bin: std::path::PathBuf::from("/dev/null/skillshub_node_bin"),
            venv_bin: std::path::PathBuf::from("/dev/null/skillshub_venv_bin"),
        }
    }

    /// A deleted workspace's bot is never spawned or minted a token — the
    /// supervisor's auto-restart goes through the same builder.
    #[test]
    fn a_deleted_workspaces_bot_is_not_built_or_minted() {
        let temp = tempdir().expect("tempdir");
        let mut scope_paths = test_scope_paths(temp.path());
        scope_paths.workspace = "gone".to_string();
        scope_paths.capabilities_root = temp.path().join("scopes").join("anonymous").join("gone");
        let principal_dir = temp.path().join("scopes").join("anonymous");
        fs::create_dir_all(&principal_dir).expect("principal dir");
        fs::write(
            principal_dir.join("workspaces.json"),
            br#"{"schema_version":1,"workspaces":[{"id":"default","display_name":"Personal","created_at":"2026-10-01T00:00:00Z","is_default":true}],"deleted":["gone"]}"#,
        )
        .expect("registry");
        let bot = ManagedBot::new(
            "gone-bot".to_string(),
            BotProcessConfig {
                command: "/bin/true".to_string(),
                ..BotProcessConfig::default()
            },
            Some(scope_paths),
        );
        // The refusal precedes the mint in `build_bot_command`, so an error
        // here means no credential was issued.
        let error = bot.build_bot_command().expect_err("refused");
        assert!(error.to_string().contains("was deleted"));
    }

    #[test]
    fn telegram_qr_url_extraction_ignores_unrelated_output() {
        assert_eq!(
            extract_telegram_qr_url("QR login URL: tg://login?token=abc"),
            Some("tg://login?token=abc")
        );
        assert_eq!(extract_telegram_qr_url("https://example.com"), None);
    }

    #[test]
    fn private_pairing_artifacts_are_written_with_private_permissions() {
        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("pairing").join("telegram.txt");
        write_private_text(&target, b"tg://login?token=secret").expect("write artifact");
        assert_eq!(
            fs::read_to_string(&target).expect("read artifact"),
            "tg://login?token=secret"
        );
        assert_eq!(
            fs::metadata(&target)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    /// A bare bot command plus a per-scope PATH override must reach the OS
    /// as an absolute program: that combination is what makes std choose
    /// `fork` over `posix_spawn`, and a forked copy of this process can hang
    /// in macOS atfork handlers before exec (see `runtime_core::process`).
    #[test]
    fn bot_command_program_is_resolved_against_the_scoped_path() {
        let temp = tempdir().expect("tempdir");
        let node_bin = temp.path().join("node-bin");
        fs::create_dir_all(&node_bin).expect("node bin dir");
        let bare = "magician-bot-spawn-probe";
        let executable = node_bin.join(bare);
        fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("script");
        let mut perms = fs::metadata(&executable).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&executable, perms).expect("chmod");

        let mut scope_paths = test_scope_paths(temp.path());
        scope_paths.node_bin = node_bin.clone();
        let bot = ManagedBot::new(
            "probe".to_string(),
            BotProcessConfig {
                command: bare.to_string(),
                ..BotProcessConfig::default()
            },
            Some(scope_paths),
        );

        let command = bot.build_bot_command().expect("build command");
        let std_command = command.as_std();
        assert_eq!(std_command.get_program(), executable.as_os_str());
        assert!(Path::new(std_command.get_program()).is_absolute());

        let child_path = std_command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("child PATH is still overridden");
        let first_dir = std::env::split_paths(child_path)
            .next()
            .expect("PATH has at least one dir");
        assert_eq!(first_dir, node_bin);
    }

    /// A bundle link into a checkout deleted while magician runs (the
    /// 2026-10-02 merged-worktree cleanup) is repaired by the next spawn — the
    /// supervisor's 30 s restart — not left failing `MODULE_NOT_FOUND` until a
    /// magician restart.
    #[tokio::test]
    async fn spawning_a_bot_repairs_a_dangling_bundle_link() {
        let temp = tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let hub = base.join("checkout").join("skillshub");
        let bundle = hub.join("bots/kapso/dist/index.js");
        fs::create_dir_all(bundle.parent().unwrap()).expect("hub bundle dir");
        fs::write(&bundle, "// kapso").expect("hub bundle");

        let mut scope_paths = test_scope_paths(&base);
        scope_paths.node_modules_bin = hub.join("node_modules").join(".bin");
        let scoped = scope_paths.bots_root.join("kapso/dist/index.js");
        fs::create_dir_all(scoped.parent().unwrap()).expect("scope bundle dir");
        std::os::unix::fs::symlink(
            base.join("deleted-worktree/skillshub/bots/kapso/dist/index.js"),
            &scoped,
        )
        .expect("dangling link");

        let bot = ManagedBot::new(
            "kapso".to_string(),
            BotProcessConfig {
                command: "/usr/bin/true".to_string(),
                ..BotProcessConfig::default()
            },
            Some(scope_paths),
        );
        let mut child = bot.spawn_child().expect("spawn");
        let _ = child.wait().await;

        assert_eq!(fs::read_link(&scoped).expect("link"), bundle);
        assert_eq!(fs::read_to_string(&scoped).expect("bundle"), "// kapso");
    }

    /// `config.env` is the last env layer, so a per-bot `PATH` there is what
    /// the child receives; a program that exists only on it must resolve
    /// there, not against the scoped augmentation it overrides.
    #[test]
    fn bot_command_program_is_resolved_against_a_per_bot_path_override() {
        let temp = tempdir().expect("tempdir");
        let bot_bin = temp.path().join("bot-bin");
        fs::create_dir_all(&bot_bin).expect("bot bin dir");
        let bare = "magician-bot-path-override-probe";
        let executable = bot_bin.join(bare);
        fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("script");
        let mut perms = fs::metadata(&executable).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&executable, perms).expect("chmod");

        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), bot_bin.display().to_string());
        let bot = ManagedBot::new(
            "override".to_string(),
            BotProcessConfig {
                command: bare.to_string(),
                env,
                ..BotProcessConfig::default()
            },
            Some(test_scope_paths(temp.path())),
        );

        let command = bot.build_bot_command().expect("build command");
        let std_command = command.as_std();
        assert_eq!(std_command.get_program(), executable.as_os_str());

        let child_path = std_command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("the per-bot PATH reaches the child");
        assert_eq!(child_path, bot_bin.as_os_str());
    }

    /// A command that already names a location is left alone, so an
    /// operator's explicit relative or absolute path is spawned verbatim.
    #[test]
    fn bot_command_with_a_path_is_spawned_verbatim() {
        let temp = tempdir().expect("tempdir");
        let script = temp.path().join("bot.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").expect("script");
        let bot = ManagedBot::new(
            "verbatim".to_string(),
            BotProcessConfig {
                command: script.display().to_string(),
                ..BotProcessConfig::default()
            },
            Some(test_scope_paths(temp.path())),
        );
        let command = bot.build_bot_command().expect("build command");
        assert_eq!(command.as_std().get_program(), script.as_os_str());
    }

    #[tokio::test]
    async fn bot_manager_runs_and_stops_configured_process() {
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("bot.sh");
        fs::write(
            &script_path,
            "#!/bin/sh\n\
             echo ready\n\
             echo noisy >&2\n\
             trap 'exit 0' TERM INT\n\
             while :; do sleep 1; done\n",
        )
        .expect("script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let mut configs = BTreeMap::new();
        configs.insert(
            "telegram".to_string(),
            BotProcessConfig {
                command: script_path.display().to_string(),
                auto_restart: false,
                ..BotProcessConfig::default()
            },
        );

        let manager = BotManager::new(configs);
        let started = manager.start("telegram").await.expect("start bot");
        assert_eq!(started.state, BotRuntimeState::Running);

        // The full workspace suite deliberately runs thousands of subprocess
        // and I/O-heavy tests together. Keep this readiness assertion bounded,
        // but allow the child and both pipe readers enough scheduling room
        // under that load; the isolated test normally completes in under 2s.
        let logs = wait_for_logs(&manager, "telegram", Duration::from_secs(15))
            .await
            .expect("logs");
        assert!(logs.iter().any(|line| line.line.contains("ready")));
        assert!(logs.iter().any(|line| line.line.contains("noisy")));

        let stopped = manager.stop("telegram").await.expect("stop bot");
        assert_eq!(stopped.state, BotRuntimeState::Stopped);
        assert!(stopped.pid.is_none());
    }

    #[tokio::test]
    async fn a_start_waits_for_the_previous_stop_to_finish() {
        let bot = Arc::new(ManagedBot::new(
            "lifecycle-race".into(),
            BotProcessConfig {
                command: "/bin/sleep".into(),
                args: vec!["300".into()],
                auto_restart: false,
                ..BotProcessConfig::default()
            },
            None,
        ));
        bot.start().await.expect("initial start");
        let pending = bot.begin_stop().await;
        let starting = bot.start();
        tokio::pin!(starting);
        let before_finish = tokio::time::timeout(Duration::from_millis(100), &mut starting).await;
        let overlapped = before_finish.is_ok();
        pending.finish(stop_join_deadline()).await;
        let restarted = match before_finish {
            Ok(result) => result,
            Err(_) => starting.await,
        }
        .expect("start after stop");
        let current_pid = bot.snapshot().await.pid;
        // Always clean the fixture before asserting, including on regression.
        bot.stop().await.expect("fixture cleanup");
        if overlapped {
            if let Some(pid) = restarted.pid {
                signal_process_group(pid, libc::SIGKILL);
            }
        }
        assert!(
            !overlapped,
            "a start must not replace a stop's process tracking"
        );
        assert_eq!(restarted.state, BotRuntimeState::Running);
        assert_eq!(current_pid, restarted.pid);
        assert!(current_pid.is_some());
    }

    #[tokio::test]
    async fn an_exited_bot_cleans_children_before_draining_inherited_pipes() {
        let temp = tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let watcher_pid = base.join("watcher.pid");
        let script = base.join("exits-with-watcher.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 /bin/sh -c 'trap \"\" TERM; echo $$ > \"{pid}\"; while :; do sleep 30; done' &\n\
                 while [ ! -f \"{pid}\" ]; do sleep 0.01; done\n\
                 echo ready\n\
                 exit 7\n",
                pid = watcher_pid.display()
            ),
        )
        .expect("script");
        let mut perms = fs::metadata(&script).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).expect("chmod");
        let bot = ManagedBot::new(
            "pipe-recovery".into(),
            BotProcessConfig {
                command: script.display().to_string(),
                auto_restart: false,
                ..BotProcessConfig::default()
            },
            Some(test_scope_paths(&base)),
        );
        let mut child = bot.spawn_child().expect("spawn bot");
        let group = child.id().expect("group leader");
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            bot.monitor_child(&mut child, CancellationToken::new()),
        )
        .await;
        // Cleanup even when the assertion fails, so this regression cannot
        // leave the very orphan it is meant to catch.
        signal_process_group(group, libc::SIGKILL);
        let _ = child.wait().await;
        let result = result.expect("child-held pipes must not prevent bot recovery");
        assert_eq!(result.code, Some(7));
        let watcher: i32 = fs::read_to_string(watcher_pid)
            .expect("watcher pid")
            .trim()
            .parse()
            .expect("numeric pid");
        assert_pid_exits(watcher, Duration::from_secs(5)).await;
        assert!(!bot
            .process_group_record_path()
            .expect("record path")
            .exists());
    }

    #[tokio::test]
    async fn stopping_a_bot_also_stops_the_processes_it_forked() {
        // The gmail bot forks `gws gmail +watch`; a stop that killed only the
        // bot left that watcher running as an orphan sharing its credentials.
        let temp = tempdir().expect("tempdir");
        let child_pid_path = temp.path().join("child.pid");
        let script_path = temp.path().join("bot.sh");
        fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n\
                 sleep 300 &\n\
                 echo $! > '{}'\n\
                 echo ready\n\
                 echo noisy >&2\n\
                 while :; do sleep 1; done\n",
                child_pid_path.display()
            ),
        )
        .expect("script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let mut configs = BTreeMap::new();
        configs.insert(
            "gmail".to_string(),
            BotProcessConfig {
                command: script_path.display().to_string(),
                auto_restart: false,
                ..BotProcessConfig::default()
            },
        );
        let manager = BotManager::new(configs);
        manager.start("gmail").await.expect("start bot");
        wait_for_logs(&manager, "gmail", Duration::from_secs(15))
            .await
            .expect("logs");
        let child_pid: i32 = fs::read_to_string(&child_pid_path)
            .expect("child pid")
            .trim()
            .parse()
            .expect("numeric child pid");

        manager.stop("gmail").await.expect("stop bot");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only probes whether the pid still exists.
        while unsafe { libc::kill(child_pid, 0) } == 0 {
            if tokio::time::Instant::now() >= deadline {
                unsafe {
                    libc::kill(child_pid, libc::SIGKILL);
                }
                panic!("forked child {child_pid} outlived the stopped bot");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Bots run in their own process groups, out of reach of a signal to
    /// Magician's group, so the manager's shutdown must stop them itself:
    /// every bot and every CLI it forked is gone afterwards.
    #[tokio::test]
    async fn shutting_down_the_manager_stops_every_bot_and_its_forks() {
        let temp = tempdir().expect("tempdir");
        let names = ["gmail", "kapso"];
        let mut configs = BTreeMap::new();
        for name in names {
            let child_pid_path = temp.path().join(format!("{name}.child.pid"));
            let script_path = temp.path().join(format!("{name}.sh"));
            fs::write(
                &script_path,
                format!(
                    "#!/bin/sh\n\
                     sleep 300 &\n\
                     echo $! > '{}'\n\
                     echo ready\n\
                     echo noisy >&2\n\
                     while :; do sleep 1; done\n",
                    child_pid_path.display()
                ),
            )
            .expect("script");
            let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).expect("chmod");
            configs.insert(
                name.to_string(),
                BotProcessConfig {
                    command: script_path.display().to_string(),
                    auto_restart: false,
                    ..BotProcessConfig::default()
                },
            );
        }
        let manager = BotManager::new(configs);
        let mut child_pids = Vec::new();
        for name in names {
            manager.start(name).await.expect("start bot");
            wait_for_logs(&manager, name, Duration::from_secs(15))
                .await
                .expect("logs");
            let child_pid: i32 = fs::read_to_string(temp.path().join(format!("{name}.child.pid")))
                .expect("child pid")
                .trim()
                .parse()
                .expect("numeric child pid");
            child_pids.push(child_pid);
        }

        manager.shutdown().await;

        for snapshot in manager.list().await {
            assert_eq!(
                snapshot.state,
                BotRuntimeState::Stopped,
                "{}",
                snapshot.name
            );
            assert!(snapshot.pid.is_none());
        }
        for child_pid in child_pids {
            assert_pid_exits(child_pid, Duration::from_secs(5)).await;
        }
    }

    /// A Magician run that died before stopping its bots leaves their groups
    /// running; the record `spawn_child` writes lets the next run reap the
    /// whole group, including a forked CLI whose bot is no longer there.
    #[tokio::test]
    async fn a_bot_group_recorded_by_a_previous_run_is_reaped() {
        use std::os::unix::process::CommandExt as _;

        let temp = tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let bot = ManagedBot::new(
            "gmail".to_string(),
            BotProcessConfig {
                command: "/usr/bin/true".to_string(),
                ..BotProcessConfig::default()
            },
            Some(test_scope_paths(&base)),
        );

        // Stand-in for the previous run's bot: a group leader with a forked
        // watcher, recorded the way `spawn_child` records a bot.
        let fork_pid_path = base.join("fork.pid");
        let mut leader = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "sleep 300 & echo $! > '{}'; wait",
                fork_pid_path.display()
            ))
            .process_group(0)
            .spawn()
            .expect("spawn leader");
        // Written to disk only: the previous run's in-memory state is gone.
        let record_path = bot.process_group_record_path().expect("record path");
        fs::create_dir_all(record_path.parent().expect("record dir")).expect("record dir");
        let record = BotProcessGroupRecord {
            pgid: leader.id(),
            boot_time: System::boot_time(),
            spawned_at: unix_now_secs(),
        };
        fs::write(
            &record_path,
            serde_json::to_vec(&record).expect("record json"),
        )
        .expect("write record");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let fork_pid: i32 = loop {
            if let Some(pid) = fs::read_to_string(&fork_pid_path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = leader.kill();
                panic!("leader never forked its watcher");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        assert_eq!(bot.reap_recorded_process_group().await, Some(leader.id()));
        assert!(!record_path.exists(), "the record is consumed");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while leader.try_wait().expect("try_wait").is_none() {
            if tokio::time::Instant::now() >= deadline {
                let _ = leader.kill();
                panic!("recorded group leader outlived the reap");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_pid_exits(fork_pid, Duration::from_secs(5)).await;
    }

    /// The record of a group this process spawned and still runs is not
    /// stale: a concurrent `start()` of the same bot must never reap it.
    #[tokio::test]
    async fn the_group_this_process_runs_for_a_bot_is_never_reaped() {
        use std::os::unix::process::CommandExt as _;

        let temp = tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let bot = ManagedBot::new(
            "gmail".to_string(),
            BotProcessConfig {
                command: "/usr/bin/true".to_string(),
                ..BotProcessConfig::default()
            },
            Some(test_scope_paths(&base)),
        );
        let mut live = std::process::Command::new("/bin/sleep")
            .arg("300")
            .process_group(0)
            .spawn()
            .expect("spawn live group");
        bot.write_process_group_record(live.id());
        let record_path = bot.process_group_record_path().expect("record path");

        assert_eq!(bot.reap_recorded_process_group().await, None);
        assert!(record_path.exists(), "the live group's record is kept");
        assert!(
            live.try_wait().expect("try_wait").is_none(),
            "the live group survives"
        );

        bot.clear_process_group_record();
        assert!(!record_path.exists());
        let _ = live.kill();
        let _ = live.wait();
    }

    /// A record whose boot or leader start time does not match names a pid
    /// that may since belong to someone else: it is discarded, never reaped.
    #[tokio::test]
    async fn a_mismatched_bot_group_record_is_discarded_not_reaped() {
        use std::os::unix::process::CommandExt as _;

        let temp = tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical temp");
        let bot = ManagedBot::new(
            "gmail".to_string(),
            BotProcessConfig {
                command: "/usr/bin/true".to_string(),
                ..BotProcessConfig::default()
            },
            Some(test_scope_paths(&base)),
        );
        let mut unrelated = std::process::Command::new("/bin/sleep")
            .arg("300")
            .process_group(0)
            .spawn()
            .expect("spawn unrelated group");
        let record_path = bot.process_group_record_path().expect("record path");
        fs::create_dir_all(record_path.parent().expect("record dir")).expect("record dir");

        let now = unix_now_secs();
        let boot_time = System::boot_time();
        let mismatched = [
            BotProcessGroupRecord {
                pgid: unrelated.id(),
                boot_time: boot_time.saturating_sub(86_400),
                spawned_at: now,
            },
            BotProcessGroupRecord {
                pgid: unrelated.id(),
                boot_time,
                spawned_at: now.saturating_sub(3_600),
            },
        ];
        for record in mismatched {
            fs::write(
                &record_path,
                serde_json::to_vec(&record).expect("record json"),
            )
            .expect("write record");
            assert_eq!(bot.reap_recorded_process_group().await, None, "{record:?}");
            assert!(
                !record_path.exists(),
                "a mismatched record is still consumed"
            );
            assert!(
                unrelated.try_wait().expect("try_wait").is_none(),
                "a group the record does not prove ours must survive: {record:?}"
            );
        }

        let _ = unrelated.kill();
        let _ = unrelated.wait();
    }

    #[tokio::test]
    async fn bot_manager_restarts_crashing_process_when_enabled() {
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("crash.sh");
        let counter_path = temp.path().join("counter.txt");
        fs::write(
            &script_path,
            format!(
                "#!/bin/sh\n\
                 count=0\n\
                 if [ -f \"{counter}\" ]; then count=$(cat \"{counter}\"); fi\n\
                 count=$((count + 1))\n\
                 echo \"$count\" > \"{counter}\"\n\
                 echo attempt-$count\n\
                 exit 1\n",
                counter = counter_path.display()
            ),
        )
        .expect("script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let mut configs = BTreeMap::new();
        configs.insert(
            "whatsapp".to_string(),
            BotProcessConfig {
                command: script_path.display().to_string(),
                auto_restart: true,
                restart_max_backoff_secs: 1,
                ..BotProcessConfig::default()
            },
        );

        let manager = BotManager::new(configs);
        manager.start("whatsapp").await.expect("start bot");

        // The full workspace suite deliberately runs thousands of subprocess
        // and I/O-heavy tests together. Match the neighboring pipe-readiness
        // allowance so a healthy restart is not mistaken for a failure while
        // the child process is waiting for scheduler time.
        wait_for_counter_at_least(&counter_path, 2, Duration::from_secs(15))
            .await
            .expect("counter indicates restart");

        let status = wait_for_restart(&manager, "whatsapp", Duration::from_secs(15))
            .await
            .expect("restart");
        assert!(status.restart_count >= 1);

        manager.stop("whatsapp").await.expect("stop bot");
    }

    #[tokio::test]
    async fn bot_manager_restores_previous_bot_when_config_update_fails() {
        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("bot.sh");
        fs::write(
            &script_path,
            "#!/bin/sh\n\
             trap 'exit 0' TERM INT\n\
             while :; do sleep 1; done\n",
        )
        .expect("script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let original_command = script_path.display().to_string();
        let manager = BotManager::new(BTreeMap::from([(
            "telegram".to_string(),
            BotProcessConfig {
                command: original_command.clone(),
                auto_restart: false,
                ..BotProcessConfig::default()
            },
        )]));

        manager.start("telegram").await.expect("start bot");

        let err = manager
            .upsert_config(
                "telegram".to_string(),
                BotProcessConfig {
                    command: "/definitely/missing-command".to_string(),
                    auto_restart: false,
                    ..BotProcessConfig::default()
                },
            )
            .await
            .expect_err("replacement config should fail to start");

        assert!(matches!(err, BotManagerError::StartFailed { .. }));

        let restored = manager.snapshot("telegram").await.expect("snapshot");
        assert_eq!(restored.command, original_command);
        assert_eq!(restored.state, BotRuntimeState::Running);
        assert!(restored.desired_running);

        manager.stop("telegram").await.expect("stop bot");
    }

    #[tokio::test]
    async fn managed_bot_qr_code_path_uses_expanded_configured_env() {
        let temp = tempdir().expect("tempdir");
        let scope_paths = test_scope_paths(temp.path());
        let bot = ManagedBot::new(
            "whatsapp".to_string(),
            BotProcessConfig {
                env: BTreeMap::from([(
                    "WHATSAPP_QR_FILE".to_string(),
                    "{scope_capabilities_root}/bots/whatsapp/qr.png".to_string(),
                )]),
                ..BotProcessConfig::default()
            },
            Some(scope_paths.clone()),
        );

        assert_eq!(
            bot.qr_code_path(),
            Some(scope_paths.capabilities_root.join("bots/whatsapp/qr.png"))
        );
        assert!(bot.snapshot().await.qr_supported);
    }

    #[tokio::test]
    async fn telegram_qr_code_path_uses_its_own_configured_env_key() {
        let temp = tempdir().expect("tempdir");
        let scope_paths = test_scope_paths(temp.path());
        let bot = ManagedBot::new(
            "telegram-self".to_string(),
            BotProcessConfig {
                env: BTreeMap::from([(
                    "TELEGRAM_QR_FILE".to_string(),
                    "{scope_capability_auth_root}/pairing/telegram.txt".to_string(),
                )]),
                ..BotProcessConfig::default()
            },
            Some(scope_paths.clone()),
        );

        assert_eq!(
            bot.qr_code_path(),
            Some(scope_paths.auth_root.join("pairing/telegram.txt"))
        );
        assert!(bot.snapshot().await.qr_supported);
    }

    /// Onboarding offers Telegram pairing for a bot with no credentials yet,
    /// so a missing env file is "needs auth" with the reason, not unsupported.
    #[tokio::test]
    async fn auth_snapshot_reports_missing_env_file_as_needs_auth() {
        let temp = tempdir().expect("tempdir");
        let scope_paths = test_scope_paths(temp.path());
        let bot_dir = scope_paths.capabilities_root.join("bots/telegram-self");
        fs::create_dir_all(&bot_dir).expect("create bot dir");

        let bot = ManagedBot::new(
            "telegram-self".to_string(),
            BotProcessConfig {
                args: vec![
                    "--env-file={scope_capabilities_root}/bots/telegram-self/.env.development"
                        .to_string(),
                ],
                cwd: Some("{scope_capabilities_root}/bots/telegram-self".to_string()),
                ..BotProcessConfig::default()
            },
            Some(scope_paths),
        );

        let auth = bot
            .auth_snapshot(BotAuthFlowState::Idle)
            .await
            .expect("auth snapshot");

        assert!(auth.supported);
        assert_eq!(auth.status, BotAuthStatus::NeedsAuth);
        assert_eq!(
            auth.provider.as_deref(),
            Some(NEEDS_AUTH_PROVIDER_TELEGRAM_SELF)
        );
        assert!(
            auth.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("credentials are not configured")),
            "{:?}",
            auth.detail
        );
    }

    #[test]
    fn tgcli_prompts_map_to_pending_inputs() {
        assert_eq!(
            telegram_auth_prompt("Enter the code you received: "),
            Some(TelegramPrompt::Input(BotAuthPendingInput::Code))
        );
        assert_eq!(
            telegram_auth_prompt("Enter your 2FA password (leave empty if not enabled): "),
            Some(TelegramPrompt::Input(BotAuthPendingInput::Password))
        );
        assert_eq!(
            telegram_auth_prompt("Telegram phone number (+...): "),
            Some(TelegramPrompt::Input(BotAuthPendingInput::Phone))
        );
        assert_eq!(
            telegram_auth_prompt("Telegram API ID: "),
            Some(TelegramPrompt::ApiCredentials)
        );
        assert_eq!(
            telegram_auth_prompt("The confirmation code has been sent via app."),
            None
        );
        assert_eq!(telegram_auth_prompt("Logged in successfully!"), None);
    }

    /// A prompt without a newline is still seen; a prompt for tgcli's own
    /// API keys ends the login instead of stalling it.
    #[tokio::test]
    async fn an_unterminated_tgcli_prompt_sets_the_pending_input() {
        let temp = tempdir().expect("tempdir");
        let bot = ManagedBot::new(
            "telegram-self".to_string(),
            BotProcessConfig::default(),
            Some(test_scope_paths(temp.path())),
        );
        let output: &[u8] =
            b"The confirmation code has been sent via app.\nEnter the code you received: ";
        let cancel = CancellationToken::new();
        capture_telegram_auth_output(
            output,
            Arc::new(bot.clone_for_logs()),
            bot.name.clone(),
            temp.path().join("qr.txt"),
            bot.auth_pending_input.handle(),
            cancel.clone(),
        )
        .await;
        assert_eq!(
            *bot.auth_pending_input.lock(),
            Some(BotAuthPendingInput::Code)
        );
        assert!(!cancel.is_cancelled());

        let asked_for_keys: &[u8] = b"Telegram API ID: ";
        capture_telegram_auth_output(
            asked_for_keys,
            Arc::new(bot.clone_for_logs()),
            bot.name.clone(),
            temp.path().join("qr.txt"),
            bot.auth_pending_input.handle(),
            cancel.clone(),
        )
        .await;
        assert!(cancel.is_cancelled(), "a key prompt ends the login");
    }

    #[test]
    fn tgcli_store_config_is_seeded_without_dropping_operator_keys() {
        let merged = merged_tgcli_config(
            Some(serde_json::json!({"apiId":"old","mcp":{"enabled":true}})),
            " 12345 ",
            "hash",
            Some("+15550001111"),
        );
        assert_eq!(merged["apiId"], "12345");
        assert_eq!(merged["apiHash"], "hash");
        assert_eq!(merged["phoneNumber"], "+15550001111");
        assert_eq!(merged["mcp"]["enabled"], true);
        let without_phone = merged_tgcli_config(None, "1", "h", None);
        assert!(without_phone.get("phoneNumber").is_none());
    }

    #[test]
    fn tgcli_store_config_lands_where_tgcli_reads_it_with_private_mode() {
        let temp = tempdir().expect("tempdir");
        let store = temp.path().join("store");
        let config = TelegramAuthConfig {
            binary: PathBuf::from("tgcli"),
            bot_cwd: temp.path().to_path_buf(),
            env: BTreeMap::from([
                ("TGCLI_API_ID".to_string(), "12345".to_string()),
                ("TGCLI_API_HASH".to_string(), "hash".to_string()),
                ("TGCLI_STORE".to_string(), "store".to_string()),
            ]),
            qr_path: temp.path().join("qr.txt"),
            profile_label: "Telegram".to_string(),
            login: TelegramLogin {
                code: true,
                phone: Some("+15550001111".to_string()),
            },
        };
        seed_telegram_store_config(&config, None).expect("seed");
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(store.join("config.json")).expect("config"))
                .expect("json");
        assert_eq!(written["phoneNumber"], "+15550001111");
        assert_eq!(
            fs::metadata(store.join("config.json"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn phones_and_pairing_codes_are_bounded() {
        assert_eq!(
            normalized_phone("+1 (555) 000-1111").as_deref(),
            Some("15550001111")
        );
        assert!(normalized_phone("12345").is_none());
        assert!(normalized_phone("+1234567890123456").is_none());
        assert!(normalized_phone("1555;rm").is_none());
        assert!(normalized_phone("1+5550001111").is_none());
        assert_eq!(
            normalize_pairing_code("abcd-1234\n").as_deref(),
            Some("ABCD1234")
        );
        assert!(normalize_pairing_code("ABC").is_none());
        assert!(normalize_pairing_code("ABCD 1234").is_none());
    }

    /// The pairing phone reaches exactly one spawn and is never written to
    /// the bot's config; a fresh spawn also drops a stale pairing code.
    #[tokio::test]
    async fn the_whatsapp_pairing_phone_is_one_shot() {
        let temp = tempdir().expect("tempdir");
        let scope_paths = test_scope_paths(temp.path());
        let bot = ManagedBot::new(
            "whatsapp".to_string(),
            BotProcessConfig {
                command: "/usr/bin/true".to_string(),
                ..BotProcessConfig::default()
            },
            Some(scope_paths.clone()),
        );
        let code_path = bot.whatsapp_pairing_code_path().expect("code path");
        assert_eq!(
            code_path,
            scope_paths
                .auth_root
                .join("pairing")
                .join("whatsapp.pairing-code")
        );
        fs::create_dir_all(code_path.parent().unwrap()).expect("pairing dir");
        fs::write(&code_path, "WXYZ5678\n").expect("code");
        assert_eq!(bot.read_pairing_code().as_deref(), Some("WXYZ5678"));

        bot.set_one_shot_env(WHATSAPP_PHONE_ENV, "15550001111".to_string());
        let mut child = bot.spawn_child().expect("spawn");
        let _ = child.wait().await;
        assert!(bot.take_one_shot_env().is_empty());
        assert!(!bot.config.env.contains_key(WHATSAPP_PHONE_ENV));
        assert!(
            bot.read_pairing_code().is_none(),
            "a spawn drops a stale code"
        );

        let command = bot.build_bot_command().expect("command");
        let envs: BTreeMap<_, _> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
            .collect();
        assert!(!envs.contains_key(OsStr::new(WHATSAPP_PHONE_ENV)));
        assert_eq!(
            envs.get(OsStr::new(WHATSAPP_PAIRING_CODE_FILE_ENV))
                .cloned()
                .flatten(),
            Some(code_path.into_os_string())
        );
    }

    async fn assert_pid_exits(pid: i32, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        // SAFETY: signal 0 only probes whether the pid still exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            if tokio::time::Instant::now() >= deadline {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                panic!("process {pid} outlived its stopped bot");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_logs(
        manager: &BotManager,
        bot_name: &str,
        timeout: Duration,
    ) -> anyhow::Result<Vec<BotLogLine>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let lines = manager.logs(bot_name, 20).await?;
            if lines.iter().any(|line| line.line.contains("ready"))
                && lines.iter().any(|line| line.line.contains("noisy"))
            {
                return Ok(lines);
            }
            if tokio::time::Instant::now() >= deadline {
                let rendered = lines
                    .iter()
                    .map(|line| format!("{}: {}", line.stream, line.line))
                    .collect::<Vec<_>>()
                    .join(" | ");
                anyhow::bail!(
                    "timed out waiting for ready/noisy logs after {:?}; captured logs: [{}]",
                    timeout,
                    rendered
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_restart(
        manager: &BotManager,
        bot_name: &str,
        timeout: Duration,
    ) -> anyhow::Result<BotStatusSnapshot> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = manager
                .list()
                .await
                .into_iter()
                .find(|snapshot| snapshot.name == bot_name)
                .ok_or_else(|| anyhow!("missing bot status for `{bot_name}`"))?;
            if status.restart_count >= 1 {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(status);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_counter_at_least(
        counter_path: &Path,
        minimum: u64,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(contents) = fs::read_to_string(counter_path) {
                let trimmed = contents.trim();
                if let Ok(current) = trimmed.parse::<u64>() {
                    if current >= minimum {
                        return Ok(());
                    }
                } else if !trimmed.is_empty() {
                    anyhow::bail!(
                        "parse counter at {}: invalid contents `{trimmed}`",
                        counter_path.display()
                    );
                }
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for counter {} to reach {}; last contents: {}",
                    counter_path.display(),
                    minimum,
                    fs::read_to_string(counter_path)
                        .unwrap_or_else(|_| "<missing>".to_string())
                        .trim()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
