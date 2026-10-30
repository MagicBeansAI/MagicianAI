//! Managed-bot accounts on Service accounts (Google Workspace per
//! `gmail-{profile}` bot, Telegram, WhatsApp): rows and a connect flow that
//! works from a phone. The supervisor logins stay where they are
//! (`magician_learning::bots`); this module only steers them and hands the
//! person what they wait for. Login material (the Google authorization URL,
//! Telegram codes and passwords, WhatsApp pairing codes) is never logged.
use super::{browser, cli, response, system_browser, BrowserSignInApi};
use crate::bot_api::BotApi;
use crate::skills_api::SkillsApi;
use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician_components::google_project_access::{self, AccessReport, AccessStatus};
use magician_components::setup::{ResolvedSetup, SetupDriver};
use magician_learning::bots::{
    BotAuthAvailability, BotAuthConnectMaterial, BotAuthFlowState, BotAuthMethod,
    BotAuthPendingInput, BotAuthSnapshot, BotAuthStartOptions, BotAuthStatus, ScopedBotRuntime,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};
use tool_runtime_core::credential_profiles::CredentialScope;
use zeroize::Zeroizing;

/// Each status probe is bounded so one slow bot cannot stall the list.
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);
/// How long `start` waits for `gws auth login` to print its URL.
const LOGIN_URL_WAIT: Duration = Duration::from_secs(15);
/// Past the supervisor's own 10-minute Google login timeout.
const BOT_LOGIN_LIFETIME: Duration = Duration::from_secs(11 * 60);
/// A WhatsApp bot pairs inside its own running process, outside any login
/// flow; its QR or pairing code counts as an in-progress sign-in only this
/// long after a `start` from here.
const RECENT_START: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectMethod {
    LoginUrl,
    Code,
    PairingCode,
    Qr,
}

impl ConnectMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::LoginUrl => "login_url",
            Self::Code => "code",
            Self::PairingCode => "pairing_code",
            Self::Qr => "qr",
        }
    }

    fn runtime(self) -> BotAuthMethod {
        match self {
            Self::LoginUrl => BotAuthMethod::LoginUrl,
            Self::Code => BotAuthMethod::Code,
            Self::PairingCode => BotAuthMethod::PairingCode,
            Self::Qr => BotAuthMethod::Qr,
        }
    }
}

/// One Service-accounts row for a managed bot configured in the scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedBotRow {
    pub bot: String,
    pub profile: Option<String>,
    pub label: String,
    pub methods: Vec<ConnectMethod>,
    pub login_url_hosts: Vec<String>,
}

/// Display names live here rather than in `setup_catalog.yaml`: Desktop
/// reads the engine's catalog with `deny_unknown_fields`, so a new catalog
/// field would break every older Desktop.
fn account_label(setup: &ResolvedSetup) -> String {
    match setup.definition.as_str() {
        "google-workspace" => "Google Workspace",
        "telegram-self" => "Telegram",
        "whatsapp" => "WhatsApp",
        _ => setup.label.as_str(),
    }
    .to_string()
}

fn connect_methods(bot: &str, login_url_hosts: &[String]) -> Vec<ConnectMethod> {
    if !login_url_hosts.is_empty() {
        return vec![ConnectMethod::LoginUrl];
    }
    match bot {
        "telegram-self" => vec![ConnectMethod::Code, ConnectMethod::Qr],
        "whatsapp" => vec![ConnectMethod::PairingCode, ConnectMethod::Qr],
        _ => Vec::new(),
    }
}

fn valid_bot_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// Rows for the installed managed-bot setups whose bots exist in the scope's
/// bot config: one per `gmail-{profile}` bot, one each for fixed names.
pub(crate) fn managed_bot_rows(
    setups: &[ResolvedSetup],
    bot_names: &[String],
) -> Vec<ManagedBotRow> {
    let mut rows = Vec::new();
    for setup in setups {
        let SetupDriver::ManagedBot {
            bot: template,
            login_url_hosts,
            ..
        } = &setup.driver
        else {
            continue;
        };
        let label = account_label(setup);
        let mut push = |bot: &str, profile: Option<&str>| {
            rows.push(ManagedBotRow {
                bot: bot.to_string(),
                profile: profile.map(str::to_string),
                label: match profile {
                    Some(profile) => format!("{label} ({profile})"),
                    None => label.clone(),
                },
                methods: connect_methods(bot, login_url_hosts),
                login_url_hosts: login_url_hosts.clone(),
            });
        };
        match template.split_once("{profile}") {
            Some((prefix, suffix)) => {
                for name in bot_names {
                    let Some(profile) = name
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.strip_suffix(suffix))
                    else {
                        continue;
                    };
                    if valid_bot_name(name) && valid_bot_name(profile) {
                        push(name, Some(profile));
                    }
                }
            },
            None => {
                if bot_names.iter().any(|name| name == template) {
                    push(template, None);
                }
            },
        }
    }
    rows.sort_by(|a, b| a.bot.cmp(&b.bot));
    rows.dedup_by(|a, b| a.bot == b.bot);
    rows
}

async fn managed_rows(
    req: &HttpRequest,
    skills: &SkillsApi,
    runtime: &ScopedBotRuntime,
    principal: &str,
    workspace: &str,
) -> Vec<ManagedBotRow> {
    let Ok(setups) = crate::skills_api::installed_managed_bot_setups(req, skills) else {
        return Vec::new();
    };
    if setups.is_empty() {
        return Vec::new();
    }
    let Ok(bots) = runtime.list(principal, workspace).await else {
        return Vec::new();
    };
    let names: Vec<String> = bots.into_iter().map(|bot| bot.name).collect();
    managed_bot_rows(&setups, &names)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingKind {
    Phone,
    Code,
    Password,
    PairingCode,
    LoginUrl,
    Qr,
}

impl PendingKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Code => "code",
            Self::Password => "password",
            Self::PairingCode => "pairing_code",
            Self::LoginUrl => "login_url",
            Self::Qr => "qr",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Phone => "Phone number",
            Self::Code => "Telegram code",
            Self::Password => "Two-step verification password",
            Self::PairingCode => "WhatsApp pairing code",
            Self::LoginUrl => "Google sign-in",
            Self::Qr => "QR code",
        }
    }

    fn from_input(input: BotAuthPendingInput) -> Self {
        match input {
            BotAuthPendingInput::Phone => Self::Phone,
            BotAuthPendingInput::Code => Self::Code,
            BotAuthPendingInput::Password => Self::Password,
        }
    }
}

fn pending_detail(bot: &str, kind: PendingKind) -> &'static str {
    match kind {
        PendingKind::Phone => "Enter the phone number of your Telegram account",
        PendingKind::Code => "Enter the code Telegram sent you",
        PendingKind::Password => "Enter your Telegram two-step verification password",
        PendingKind::PairingCode => {
            "In WhatsApp, open Linked devices → Link a device → Link with phone number instead, and enter this code"
        },
        PendingKind::LoginUrl => "Finish Google sign-in in the private browser",
        PendingKind::Qr if bot == "telegram-self" => {
            "Scan this QR code in Telegram: Settings → Devices → Link Desktop Device"
        },
        PendingKind::Qr => "Scan this QR code in WhatsApp under Linked devices",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectView {
    pub status: &'static str,
    pub detail: String,
    pub pending: Option<PendingKind>,
}

impl ConnectView {
    fn new(status: &'static str, detail: &str, pending: Option<PendingKind>) -> Self {
        Self {
            status,
            detail: detail.to_string(),
            pending,
        }
    }
}

/// Pure status mapping shared by the rows and `BotConnectState`. Details are
/// fixed strings: supervisor details can name account addresses.
pub(crate) fn connect_view(
    bot: &str,
    availability: &BotAuthAvailability,
    auth: Option<&BotAuthSnapshot>,
    material: &BotAuthConnectMaterial,
    recently_started: bool,
) -> ConnectView {
    if let BotAuthAvailability::Unavailable(detail) = availability {
        return ConnectView::new("unavailable", detail, None);
    }
    let Some(auth) = auth else {
        return ConnectView::new(
            "unknown",
            crate::skills_api::ACCOUNT_STATUS_UNAVAILABLE,
            None,
        );
    };
    let active = auth.flow_state == BotAuthFlowState::Active
        || material.flow_state == BotAuthFlowState::Active;
    let queued = !active
        && (auth.flow_state == BotAuthFlowState::Queued
            || material.flow_state == BotAuthFlowState::Queued);
    let connected = auth.status == BotAuthStatus::Ok;
    let pending = if active {
        material
            .pending_input
            .map(PendingKind::from_input)
            .or_else(|| material.login_url.as_ref().map(|_| PendingKind::LoginUrl))
            .or_else(|| material.qr_path.as_ref().map(|_| PendingKind::Qr))
    } else if !connected && material.worker_running && recently_started {
        material
            .pairing_code
            .as_ref()
            .map(|_| PendingKind::PairingCode)
            .or_else(|| material.qr_path.as_ref().map(|_| PendingKind::Qr))
    } else {
        None
    };
    if let Some(kind) = pending {
        return ConnectView::new("authenticating", pending_detail(bot, kind), Some(kind));
    }
    if active || (!connected && material.worker_running && recently_started) {
        return ConnectView::new("authenticating", "Waiting for the sign-in to finish", None);
    }
    if queued {
        return ConnectView::new(
            "authenticating",
            "Waiting for another account sign-in to finish",
            None,
        );
    }
    match auth.status {
        BotAuthStatus::Ok => ConnectView::new("connected", "Connected", None),
        BotAuthStatus::NeedsAuth => {
            ConnectView::new("needs_sign_in", "Sign in to connect this account", None)
        },
        BotAuthStatus::AccountMismatch => ConnectView::new(
            "needs_sign_in",
            "Signed in to a different account than this profile expects",
            None,
        ),
        BotAuthStatus::Error | BotAuthStatus::Unsupported => ConnectView::new(
            "unknown",
            crate::skills_api::ACCOUNT_STATUS_UNAVAILABLE,
            None,
        ),
    }
}

/// `BotConnectState`.
fn render_state(
    bot: &str,
    view: &ConnectView,
    material: &BotAuthConnectMaterial,
    sign_in: Option<Value>,
) -> Value {
    // "Open on computer" before the URL is known: the host browser has it.
    let pending = view.pending.or_else(|| {
        (sign_in.is_some() && view.status == "authenticating").then_some(PendingKind::LoginUrl)
    });
    let pending = pending.map(|kind| {
        json!({
            "kind": kind.as_str(),
            "label": kind.label(),
            "pairing_code": if kind == PendingKind::PairingCode {
                material.pairing_code.clone()
            } else {
                None
            },
            "qr_data_url": if kind == PendingKind::Qr {
                material
                    .qr_path
                    .as_deref()
                    .and_then(crate::bot_api::qr_png_data_url)
            } else {
                None
            },
            "sign_in": if kind == PendingKind::LoginUrl { sign_in.clone() } else { None },
        })
    });
    json!({
        "bot": bot,
        "status": view.status,
        "detail": view.detail,
        "pending": pending,
    })
}

async fn observe(
    runtime: &ScopedBotRuntime,
    principal: &str,
    workspace: &str,
    bot: &str,
) -> (
    BotAuthAvailability,
    Option<BotAuthSnapshot>,
    BotAuthConnectMaterial,
) {
    let availability = runtime
        .auth_availability(principal, workspace, bot)
        .await
        .unwrap_or(BotAuthAvailability::Unavailable(
            "This account's bot is not configured",
        ));
    let auth = tokio::time::timeout(STATUS_TIMEOUT, runtime.auth_info(principal, workspace, bot))
        .await
        .ok()
        .and_then(Result::ok);
    let material = runtime
        .auth_connect_material(principal, workspace, bot)
        .await
        .unwrap_or_default();
    (availability, auth, material)
}

fn login_key(principal: &str, workspace: &str, bot: &str) -> String {
    format!("{principal}\u{1f}{workspace}\u{1f}{bot}")
}

fn bot_correlation(bot: &str) -> String {
    format!("bot:{bot}")
}

/// Viewer logins for supervisor-run Google logins, and recent starts.
#[derive(Default)]
pub(super) struct BotLogins {
    logins: Arc<Mutex<HashMap<String, Arc<cli::CliLogin>>>>,
    starts: Mutex<HashMap<String, Instant>>,
}

impl BotLogins {
    fn mark_started(&self, key: &str) {
        let mut starts = self.starts.lock().unwrap_or_else(|e| e.into_inner());
        starts.retain(|_, at| at.elapsed() < RECENT_START);
        starts.insert(key.to_string(), Instant::now());
    }

    fn clear_started(&self, key: &str) {
        self.starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }

    fn recently_started(&self, key: &str) -> bool {
        self.starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .is_some_and(|at| at.elapsed() < RECENT_START)
    }
}

/// The scraped URL must be an HTTPS page on a host the setup reviewed.
fn login_url_allowed(url: &str, hosts: &[String]) -> bool {
    browser::allowed_page(url)
        && url::Url::parse(url).is_ok_and(|parsed| {
            parsed.scheme() == "https"
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed
                    .host_str()
                    .is_some_and(|host| hosts.iter().any(|allowed| allowed == host))
        })
}

struct BotContext {
    runtime: Arc<ScopedBotRuntime>,
    principal: String,
    workspace: String,
    scope: CredentialScope,
    row: ManagedBotRow,
}

impl BotContext {
    fn key(&self) -> String {
        login_key(&self.principal, &self.workspace, &self.row.bot)
    }

    async fn material(&self) -> BotAuthConnectMaterial {
        self.runtime
            .auth_connect_material(&self.principal, &self.workspace, &self.row.bot)
            .await
            .unwrap_or_default()
    }

    async fn auth(&self) -> Option<BotAuthSnapshot> {
        tokio::time::timeout(
            STATUS_TIMEOUT,
            self.runtime
                .auth_info(&self.principal, &self.workspace, &self.row.bot),
        )
        .await
        .ok()
        .and_then(Result::ok)
    }
}

async fn bot_context(
    req: &HttpRequest,
    skills: &SkillsApi,
    bots: Option<&BotApi>,
    auth: Option<&AuthRuntime>,
    bot: &str,
) -> Result<BotContext, HttpResponse> {
    crate::programs_api::require_owner_session_or_phone(req, auth).await?;
    let Some(bots) = bots else {
        return Err(bot_failure("bot_runtime_unavailable"));
    };
    if !valid_bot_name(bot) {
        return Err(bot_failure("bot_request_invalid"));
    }
    let (principal, workspace) = crate::scope::resolve_required_scope(req.headers(), None)?;
    let scope = CredentialScope::new(principal.clone(), workspace.clone())
        .map_err(|_| bot_failure("bot_request_invalid"))?;
    let runtime = Arc::clone(&bots.runtime);
    // Only the scope's configured managed-bot setups can be driven here.
    let row = managed_rows(req, skills, &runtime, &principal, &workspace)
        .await
        .into_iter()
        .find(|row| row.bot == bot)
        .ok_or_else(|| bot_failure("bot_not_available"))?;
    Ok(BotContext {
        runtime,
        principal,
        workspace,
        scope,
        row,
    })
}

/// `{error, message}`; clients display `message`.
pub(crate) fn bot_failure(code: &'static str) -> HttpResponse {
    let (status, message) = match code {
        "bot_not_available" => (
            StatusCode::NOT_FOUND,
            "This account is not set up in this workspace.",
        ),
        "bot_runtime_unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Account sign-in is not available on this engine.",
        ),
        "bot_method_unsupported" => (
            StatusCode::BAD_REQUEST,
            "This account cannot connect that way.",
        ),
        "bot_phone_required" => (
            StatusCode::BAD_REQUEST,
            "Enter the phone number of the account.",
        ),
        "bot_phone_invalid" => (
            StatusCode::BAD_REQUEST,
            "Enter the phone number with its country code, for example +44 7700 900123.",
        ),
        "bot_code_invalid" => (
            StatusCode::BAD_REQUEST,
            "Enter the code exactly as it was sent.",
        ),
        "bot_password_invalid" => (
            StatusCode::BAD_REQUEST,
            "That password cannot be sent to the sign-in.",
        ),
        "bot_input_not_expected" => (
            StatusCode::CONFLICT,
            "The sign-in is not waiting for that right now. Refresh and try again.",
        ),
        "bot_input_rejected" => (
            StatusCode::CONFLICT,
            "The sign-in did not accept that. Start again if it has ended.",
        ),
        "bot_start_failed" => (
            StatusCode::CONFLICT,
            "The sign-in could not start. Another account sign-in may still be running.",
        ),
        "signin_browser_unavailable" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "The browser on the computer could not be opened.",
        ),
        "project_access_email_missing" => (
            StatusCode::CONFLICT,
            "Enter the Google email this account must use first.",
        ),
        "project_access_client_missing" => (
            StatusCode::CONFLICT,
            "Sign in to this account first; its Google client file names the Cloud project.",
        ),
        "project_access_grant_failed" => (
            StatusCode::CONFLICT,
            "The Cloud project roles could not be granted from this computer.",
        ),
        _ => (StatusCode::BAD_REQUEST, "The request was not valid."),
    };
    HttpResponse::build(status)
        .insert_header(("Cache-Control", "no-store"))
        .json(json!({"error": code, "message": message}))
}

/// `+digits` from an E.164-style number; at most 20 characters, 6–15
/// digits, with spaces, dashes, dots and parentheses allowed.
pub(crate) fn normalize_phone(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 20 {
        return Err("bot_phone_invalid");
    }
    let mut digits = String::new();
    for (index, character) in trimmed.chars().enumerate() {
        match character {
            '0'..='9' => digits.push(character),
            '+' if index == 0 => {},
            ' ' | '-' | '.' | '(' | ')' => {},
            _ => return Err("bot_phone_invalid"),
        }
    }
    if !(6..=15).contains(&digits.len()) {
        return Err("bot_phone_invalid");
    }
    Ok(format!("+{digits}"))
}

/// A login code: at most 16 characters, letters and digits once spaces and
/// dashes are dropped.
pub(crate) fn normalize_code(raw: &str) -> Result<String, &'static str> {
    let raw = raw.trim();
    let code: String = raw
        .chars()
        .filter(|character| !matches!(character, ' ' | '-'))
        .collect();
    if code.is_empty()
        || raw.len() > 16
        || !code
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("bot_code_invalid");
    }
    Ok(code)
}

/// One stdin line of at most 4096 bytes; never logged.
pub(crate) fn validate_password(raw: &str) -> Result<(), &'static str> {
    if raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("bot_password_invalid");
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BotStartRequest {
    bot: String,
    method: ConnectMethod,
    #[serde(default)]
    phone: Option<String>,
    /// Google only: open the URL in the engine host's own browser instead
    /// of returning a private-viewer ticket.
    #[serde(default)]
    open_on_computer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InputStep {
    Phone,
    Code,
    Password,
}

impl InputStep {
    fn pending(self) -> BotAuthPendingInput {
        match self {
            Self::Phone => BotAuthPendingInput::Phone,
            Self::Code => BotAuthPendingInput::Code,
            Self::Password => BotAuthPendingInput::Password,
        }
    }
}

/// Deliberately not `Debug`: `value` can be a password.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BotInputRequest {
    bot: String,
    step: InputStep,
    value: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BotRequest {
    bot: String,
}

impl BrowserSignInApi {
    /// A computer-mode ticket for the Google login URL. A fresh URL gets a
    /// viewer login (and a watcher that ends it with the supervisor login);
    /// a known one only gets a new ticket when `reissue` (an explicit start).
    fn bot_viewer(&self, ctx: &BotContext, url: &str, reissue: bool) -> Option<Value> {
        if !login_url_allowed(url, &ctx.row.login_url_hosts) {
            return None;
        }
        let key = ctx.key();
        let (login, fresh) = {
            let mut logins = self
                .bot_logins
                .logins
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match logins.get(&key) {
                Some(existing)
                    if existing.state.load(Ordering::SeqCst) == 0
                        && !existing.is_cancelled()
                        && existing.url.as_str() == url =>
                {
                    (Arc::clone(existing), false)
                },
                _ => {
                    let login = Arc::new(cli::CliLogin::external(Zeroizing::new(url.to_owned())));
                    if let Some(previous) = logins.insert(key.clone(), Arc::clone(&login)) {
                        previous.cancel();
                    }
                    (login, true)
                },
            }
        };
        if fresh {
            watch_bot_login(
                Arc::clone(&ctx.runtime),
                ctx.principal.clone(),
                ctx.workspace.clone(),
                ctx.row.bot.clone(),
                Arc::clone(&login),
                Arc::clone(&self.bot_logins.logins),
                key,
            );
        } else if !reissue {
            return None;
        }
        self.create(
            ctx.scope.clone(),
            bot_correlation(&ctx.row.bot),
            Zeroizing::new(url.to_owned()),
            Some(login),
        )
        .ok()
    }

    fn end_bot_viewer(&self, ctx: &BotContext) {
        let key = ctx.key();
        self.bot_logins.clear_started(&key);
        let login = self
            .bot_logins
            .logins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        if let Some(login) = login {
            login.cancel();
        }
        let correlation = bot_correlation(&ctx.row.bot);
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, session| {
                let ours = session.scope == ctx.scope && session.correlation == correlation;
                if ours {
                    session.revoke(true);
                }
                !ours
            });
    }

    async fn bot_state(&self, ctx: &BotContext, sign_in: Option<Value>) -> Value {
        let (availability, auth, material) =
            observe(&ctx.runtime, &ctx.principal, &ctx.workspace, &ctx.row.bot).await;
        let view = connect_view(
            &ctx.row.bot,
            &availability,
            auth.as_ref(),
            &material,
            self.bot_logins.recently_started(&ctx.key()),
        );
        let mut sign_in = sign_in;
        if sign_in.is_none() && view.pending == Some(PendingKind::LoginUrl) {
            if let Some(url) = material.login_url.as_deref() {
                sign_in = self.bot_viewer(ctx, url, false);
            }
        }
        render_state(&ctx.row.bot, &view, &material, sign_in)
    }
}

/// Ends the viewer login with the supervisor login: connected when the bot's
/// flow went idle with a working account, expired otherwise.
fn watch_bot_login(
    runtime: Arc<ScopedBotRuntime>,
    principal: String,
    workspace: String,
    bot: String,
    login: Arc<cli::CliLogin>,
    logins: Arc<Mutex<HashMap<String, Arc<cli::CliLogin>>>>,
    key: String,
) {
    let stop = login.viewer_cancellation();
    // On the request's worker (actix `LocalSet`), so the bot runtime's
    // status future need not be `Send`.
    actix_web::rt::spawn(async move {
        let started = Instant::now();
        let outcome = loop {
            let cancelled = tokio::select! {
                _ = stop.cancelled() => true,
                _ = tokio::time::sleep(Duration::from_secs(2)) => false,
            };
            if cancelled {
                break None;
            }
            if started.elapsed() > BOT_LOGIN_LIFETIME {
                break Some(2);
            }
            match tokio::time::timeout(
                Duration::from_secs(5),
                runtime.auth_info(&principal, &workspace, &bot),
            )
            .await
            {
                Ok(Ok(snapshot)) if snapshot.flow_state == BotAuthFlowState::Idle => {
                    break Some(if snapshot.status == BotAuthStatus::Ok {
                        1
                    } else {
                        2
                    });
                },
                Ok(Err(_)) => break Some(2),
                _ => {},
            }
        };
        if let Some(state) = outcome {
            login.state.store(state, Ordering::SeqCst);
        }
        let mut logins = logins.lock().unwrap_or_else(|e| e.into_inner());
        if logins
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &login))
        {
            logins.remove(&key);
        }
    });
}

async fn wait_for_login_url(ctx: &BotContext) -> Option<String> {
    let deadline = Instant::now() + LOGIN_URL_WAIT;
    while Instant::now() < deadline {
        let material = ctx.material().await;
        if let Some(url) = material.login_url {
            return Some(url);
        }
        if material.flow_state != BotAuthFlowState::Active {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

/// Managed-bot rows with live status for `accounts`.
pub(super) async fn account_values(
    api: &BrowserSignInApi,
    req: &HttpRequest,
    skills: &SkillsApi,
    bots: Option<&BotApi>,
) -> Vec<Value> {
    let Some(bots) = bots else {
        return Vec::new();
    };
    let Ok((principal, workspace)) = crate::scope::resolve_required_scope(req.headers(), None)
    else {
        return Vec::new();
    };
    let runtime = &bots.runtime;
    let rows = managed_rows(req, skills, runtime, &principal, &workspace).await;
    let views = futures_util::future::join_all(rows.iter().map(|row| {
        let (principal, workspace) = (principal.as_str(), workspace.as_str());
        async move {
            let (availability, auth, material) =
                observe(runtime, principal, workspace, &row.bot).await;
            // A Google account also needs roles on the client's Cloud
            // project; clients offer that check through `bot/project-access`.
            let project_access = matches!(
                runtime
                    .google_workspace_account(principal, workspace, &row.bot)
                    .await,
                Ok(Some(account)) if account.expected_account.is_some()
            );
            let view = connect_view(
                &row.bot,
                &availability,
                auth.as_ref(),
                &material,
                api.bot_logins
                    .recently_started(&login_key(principal, workspace, &row.bot)),
            );
            (view, project_access)
        }
    }))
    .await;
    rows.into_iter()
        .zip(views)
        .map(|(row, (view, project_access))| {
            let mut value = json!({
                "name": row.bot,
                "label": row.label,
                "kind": "managed_bot",
                "bot": row.bot,
                "methods": row.methods.iter().map(|method| method.as_str()).collect::<Vec<_>>(),
                "status": view.status,
                "detail": view.detail,
            });
            if let Some(profile) = row.profile {
                value["profile"] = json!(profile);
            }
            if project_access {
                value["project_access"] = json!(true);
            }
            value
        })
        .collect()
}

pub(super) async fn start(
    req: HttpRequest,
    body: web::Json<BotStartRequest>,
    api: web::Data<BrowserSignInApi>,
    skills: web::Data<SkillsApi>,
    bots: Option<web::Data<BotApi>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    let request = body.into_inner();
    let ctx = match bot_context(
        &req,
        &skills,
        bots.as_ref().map(|data| data.get_ref()),
        auth.as_ref().map(|data| data.get_ref()),
        &request.bot,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let phone = match request.phone.as_deref().map(normalize_phone) {
        Some(Ok(phone)) => Some(Zeroizing::new(phone)),
        Some(Err(code)) => return bot_failure(code),
        None => None,
    };
    if !ctx.row.methods.contains(&request.method) {
        return bot_failure("bot_method_unsupported");
    }
    match (request.method, phone.is_some()) {
        (ConnectMethod::PairingCode, false) => return bot_failure("bot_phone_required"),
        (ConnectMethod::LoginUrl | ConnectMethod::Qr, true) => {
            return bot_failure("bot_request_invalid")
        },
        _ => {},
    }
    if request.open_on_computer && request.method != ConnectMethod::LoginUrl {
        return bot_failure("bot_request_invalid");
    }
    let availability = ctx
        .runtime
        .auth_availability(&ctx.principal, &ctx.workspace, &ctx.row.bot)
        .await;
    if !matches!(availability, Ok(BotAuthAvailability::Available)) {
        // The state names what is missing.
        return response(api.bot_state(&ctx, None).await);
    }
    let key = ctx.key();
    let mut sign_in = None;
    if request.method == ConnectMethod::LoginUrl {
        let material = ctx.material().await;
        let live_url = material
            .login_url
            .filter(|_| material.flow_state == BotAuthFlowState::Active);
        match (live_url, request.open_on_computer) {
            (Some(url), true) => {
                if system_browser(&url).await.is_err() {
                    return bot_failure("signin_browser_unavailable");
                }
                sign_in = Some(json!({"mode": "computer_opened"}));
            },
            (Some(url), false) => sign_in = api.bot_viewer(&ctx, &url, true),
            (None, open_on_computer) => {
                let options = BotAuthStartOptions {
                    method: Some(BotAuthMethod::LoginUrl),
                    headless: !open_on_computer,
                    phone: None,
                };
                if let Err(error) = ctx
                    .runtime
                    .start_auth_with(&ctx.principal, &ctx.workspace, &ctx.row.bot, options)
                    .await
                {
                    tracing::warn!(bot = %ctx.row.bot, error = %error, "managed bot sign-in did not start");
                    return bot_failure("bot_start_failed");
                }
                api.bot_logins.mark_started(&key);
                if open_on_computer {
                    sign_in = Some(json!({"mode": "computer_opened"}));
                } else if let Some(url) = wait_for_login_url(&ctx).await {
                    sign_in = api.bot_viewer(&ctx, &url, true);
                }
            },
        }
    } else {
        // Restarting an already linked WhatsApp bot would only drop its
        // connection for a moment; there is nothing to pair.
        let linked = ctx.row.bot == "whatsapp"
            && ctx
                .auth()
                .await
                .is_some_and(|auth| auth.status == BotAuthStatus::Ok);
        if !linked {
            let options = BotAuthStartOptions {
                method: Some(request.method.runtime()),
                headless: true,
                phone: phone.as_deref().cloned(),
            };
            if let Err(error) = ctx
                .runtime
                .start_auth_with(&ctx.principal, &ctx.workspace, &ctx.row.bot, options)
                .await
            {
                tracing::warn!(bot = %ctx.row.bot, error = %error, "managed bot sign-in did not start");
                return bot_failure("bot_start_failed");
            }
            api.bot_logins.mark_started(&key);
        }
    }
    response(api.bot_state(&ctx, sign_in).await)
}

pub(super) async fn input(
    req: HttpRequest,
    body: web::Json<BotInputRequest>,
    api: web::Data<BrowserSignInApi>,
    skills: web::Data<SkillsApi>,
    bots: Option<web::Data<BotApi>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    let request = body.into_inner();
    let raw = Zeroizing::new(request.value);
    let ctx = match bot_context(
        &req,
        &skills,
        bots.as_ref().map(|data| data.get_ref()),
        auth.as_ref().map(|data| data.get_ref()),
        &request.bot,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let value: Zeroizing<String> = match request.step {
        InputStep::Phone => match normalize_phone(&raw) {
            Ok(value) => Zeroizing::new(value),
            Err(code) => return bot_failure(code),
        },
        InputStep::Code => match normalize_code(&raw) {
            Ok(value) => Zeroizing::new(value),
            Err(code) => return bot_failure(code),
        },
        InputStep::Password => {
            if let Err(code) = validate_password(&raw) {
                return bot_failure(code);
            }
            raw
        },
    };
    let material = ctx.material().await;
    if material.flow_state != BotAuthFlowState::Active
        || material.pending_input != Some(request.step.pending())
    {
        return bot_failure("bot_input_not_expected");
    }
    if ctx
        .runtime
        .submit_auth_input(&ctx.principal, &ctx.workspace, &ctx.row.bot, &value)
        .await
        .is_err()
    {
        return bot_failure("bot_input_rejected");
    }
    response(api.bot_state(&ctx, None).await)
}

pub(super) async fn state(
    req: HttpRequest,
    body: web::Json<BotRequest>,
    api: web::Data<BrowserSignInApi>,
    skills: web::Data<SkillsApi>,
    bots: Option<web::Data<BotApi>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    let ctx = match bot_context(
        &req,
        &skills,
        bots.as_ref().map(|data| data.get_ref()),
        auth.as_ref().map(|data| data.get_ref()),
        &body.bot,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    response(api.bot_state(&ctx, None).await)
}

pub(super) async fn cancel(
    req: HttpRequest,
    body: web::Json<BotRequest>,
    api: web::Data<BrowserSignInApi>,
    skills: web::Data<SkillsApi>,
    bots: Option<web::Data<BotApi>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    let ctx = match bot_context(
        &req,
        &skills,
        bots.as_ref().map(|data| data.get_ref()),
        auth.as_ref().map(|data| data.get_ref()),
        &body.bot,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let _ = ctx
        .runtime
        .cancel_auth(&ctx.principal, &ctx.workspace, &ctx.row.bot)
        .await;
    api.end_bot_viewer(&ctx);
    response(api.bot_state(&ctx, None).await)
}

/// A project-access report stays fresh this long: `check` runs gcloud twice,
/// and Service accounts asks once per Google row it shows.
const PROJECT_ACCESS_TTL: Duration = Duration::from_secs(10 * 60);

type ProjectAccessCache = Mutex<HashMap<(String, String), (Instant, AccessReport)>>;

fn project_access_cache() -> &'static ProjectAccessCache {
    static CACHE: std::sync::OnceLock<ProjectAccessCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectAccessRequest {
    bot: String,
    /// Skip the cached report (after granting the roles elsewhere).
    #[serde(default)]
    fresh: bool,
}

/// `(project, email)` for a Google Workspace bot: the bot env's project
/// override, else the project in its profile's `client_secret.json`.
async fn project_access_target(ctx: &BotContext) -> Result<(String, String), HttpResponse> {
    let account = ctx
        .runtime
        .google_workspace_account(&ctx.principal, &ctx.workspace, &ctx.row.bot)
        .await
        .map_err(|_| bot_failure("bot_runtime_unavailable"))?
        .ok_or_else(|| bot_failure("bot_method_unsupported"))?;
    let email = account
        .expected_account
        .ok_or_else(|| bot_failure("project_access_email_missing"))?;
    let project = match account.project_id {
        Some(project) => project,
        None => google_project_access::project_id_from_client_secret(
            &account.config_dir.join("client_secret.json"),
        )
        .map_err(|_| bot_failure("project_access_client_missing"))?,
    };
    Ok((project, email.to_ascii_lowercase()))
}

/// The account's roles on the Cloud project; `can_grant` says whether this
/// computer's gcloud account can add the missing ones.
pub(super) async fn project_access(
    req: HttpRequest,
    body: web::Json<ProjectAccessRequest>,
    skills: web::Data<SkillsApi>,
    bots: Option<web::Data<BotApi>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    let ctx = match bot_context(
        &req,
        &skills,
        bots.as_ref().map(|data| data.get_ref()),
        auth.as_ref().map(|data| data.get_ref()),
        &body.bot,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let (project, email) = match project_access_target(&ctx).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let key = (project.clone(), email.clone());
    if !body.fresh {
        let cached = project_access_cache()
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).cloned())
            .filter(|(at, _)| at.elapsed() < PROJECT_ACCESS_TTL);
        if let Some((_, report)) = cached {
            return project_access_response(&ctx.row.bot, &report);
        }
    }
    let report =
        match tokio::task::spawn_blocking(move || google_project_access::check(&project, &email))
            .await
        {
            Ok(report) => report,
            Err(_) => return bot_failure("bot_runtime_unavailable"),
        };
    // Only a definite answer is worth keeping; an unreadable policy may be a
    // gcloud login the person is about to fix.
    if report.status != AccessStatus::Unknown {
        if let Ok(mut cache) = project_access_cache().lock() {
            cache.insert(key, (Instant::now(), report.clone()));
        }
    }
    project_access_response(&ctx.row.bot, &report)
}

/// Grant the missing roles with this computer's gcloud account. An owner
/// session asked for it explicitly; the account and roles are fixed here.
pub(super) async fn grant_project_access(
    req: HttpRequest,
    body: web::Json<BotRequest>,
    skills: web::Data<SkillsApi>,
    bots: Option<web::Data<BotApi>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    let ctx = match bot_context(
        &req,
        &skills,
        bots.as_ref().map(|data| data.get_ref()),
        auth.as_ref().map(|data| data.get_ref()),
        &body.bot,
    )
    .await
    {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let (project, email) = match project_access_target(&ctx).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let key = (project.clone(), email.clone());
    let granted =
        tokio::task::spawn_blocking(move || google_project_access::grant(&project, &email)).await;
    if let Ok(mut cache) = project_access_cache().lock() {
        cache.remove(&key);
    }
    match granted {
        Ok(Ok(report)) => {
            tracing::info!(
                bot = %ctx.row.bot,
                project = %report.project_id,
                status = ?report.status,
                "granted Google Cloud project roles"
            );
            project_access_response(&ctx.row.bot, &report)
        },
        Ok(Err(error)) => {
            tracing::warn!(bot = %ctx.row.bot, %error, "granting Google Cloud project roles failed");
            bot_failure("project_access_grant_failed")
        },
        Err(_) => bot_failure("bot_runtime_unavailable"),
    }
}

fn project_access_response(bot: &str, report: &AccessReport) -> HttpResponse {
    let mut value = serde_json::to_value(report).unwrap_or_else(|_| json!({}));
    value["bot"] = json!(bot);
    value["can_grant"] = json!(report.can_grant());
    response(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_learning::bots::BotAuthFlowState as Flow;

    fn snapshot(status: BotAuthStatus, flow_state: Flow) -> BotAuthSnapshot {
        BotAuthSnapshot {
            name: "bot".into(),
            supported: true,
            provider: None,
            status,
            flow_state,
            profile_label: None,
            expected_account: Some("owner@example.com".into()),
            current_account: Some("other@example.com".into()),
            detail: Some("authenticated as other@example.com; expected owner@example.com".into()),
        }
    }

    fn setups() -> Vec<ResolvedSetup> {
        ["google-workspace", "telegram-self", "whatsapp"]
            .into_iter()
            .map(|definition| magician_components::setup::resolve_template(definition).unwrap())
            .collect()
    }

    #[test]
    fn rows_follow_the_scopes_bot_config() {
        let names: Vec<String> = [
            "gmail-work",
            "gmail-",
            "kapso",
            "telegram-self",
            "gmail-Bad/x",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let rows = managed_bot_rows(&setups(), &names);
        let bots: Vec<_> = rows.iter().map(|row| row.bot.as_str()).collect();
        assert_eq!(bots, vec!["gmail-work", "telegram-self"]);
        assert_eq!(rows[0].label, "Google Workspace (work)");
        assert_eq!(rows[0].profile.as_deref(), Some("work"));
        assert_eq!(rows[0].methods, vec![ConnectMethod::LoginUrl]);
        assert_eq!(rows[1].label, "Telegram");
        assert_eq!(
            rows[1].methods,
            vec![ConnectMethod::Code, ConnectMethod::Qr]
        );
        let with_whatsapp = managed_bot_rows(&setups(), &["whatsapp".to_string()]);
        assert_eq!(with_whatsapp[0].label, "WhatsApp");
        assert_eq!(
            with_whatsapp[0].methods,
            vec![ConnectMethod::PairingCode, ConnectMethod::Qr]
        );
    }

    #[test]
    fn status_mapping_covers_every_state_without_addresses() {
        let material = BotAuthConnectMaterial::default();
        let view = |status, flow, material: &BotAuthConnectMaterial, recent| {
            connect_view(
                "gmail-work",
                &BotAuthAvailability::Available,
                Some(&snapshot(status, flow)),
                material,
                recent,
            )
        };
        assert_eq!(
            view(BotAuthStatus::Ok, Flow::Idle, &material, false).status,
            "connected"
        );
        assert_eq!(
            view(BotAuthStatus::NeedsAuth, Flow::Idle, &material, false).status,
            "needs_sign_in"
        );
        let mismatch = view(BotAuthStatus::AccountMismatch, Flow::Idle, &material, false);
        assert_eq!(mismatch.status, "needs_sign_in");
        assert!(!mismatch.detail.contains('@'), "{}", mismatch.detail);
        assert_eq!(
            view(BotAuthStatus::Error, Flow::Idle, &material, false).status,
            "unknown"
        );
        assert_eq!(
            view(BotAuthStatus::NeedsAuth, Flow::Queued, &material, false).status,
            "authenticating"
        );
        let unavailable = connect_view(
            "telegram-self",
            &BotAuthAvailability::Unavailable("The tgcli program is not installed"),
            None,
            &material,
            false,
        );
        assert_eq!(unavailable.status, "unavailable");
        assert_eq!(
            connect_view("x", &BotAuthAvailability::Available, None, &material, false).status,
            "unknown"
        );

        let asking = BotAuthConnectMaterial {
            pending_input: Some(BotAuthPendingInput::Code),
            flow_state: Flow::Active,
            ..BotAuthConnectMaterial::default()
        };
        let telegram = view(BotAuthStatus::NeedsAuth, Flow::Active, &asking, false);
        assert_eq!(telegram.status, "authenticating");
        assert_eq!(telegram.pending, Some(PendingKind::Code));

        let pairing = BotAuthConnectMaterial {
            pairing_code: Some("ABCD1234".into()),
            worker_running: true,
            ..BotAuthConnectMaterial::default()
        };
        let linking = view(BotAuthStatus::NeedsAuth, Flow::Idle, &pairing, true);
        assert_eq!(linking.pending, Some(PendingKind::PairingCode));
        let rendered = render_state("whatsapp", &linking, &pairing, None);
        assert_eq!(rendered["pending"]["pairing_code"], "ABCD1234");
        assert_eq!(rendered["pending"]["kind"], "pairing_code");
        assert!(rendered["pending"]["sign_in"].is_null());
        // A code left from an older start is not an in-progress sign-in.
        assert_eq!(
            view(BotAuthStatus::NeedsAuth, Flow::Idle, &pairing, false).status,
            "needs_sign_in"
        );
    }

    #[test]
    fn requests_are_closed_and_bounded() {
        for value in [
            json!({"bot":"telegram-self","method":"code","extra":true}),
            json!({"bot":"telegram-self","method":"sms"}),
            json!({"bot":"telegram-self"}),
        ] {
            assert!(serde_json::from_value::<BotStartRequest>(value).is_err());
        }
        let start: BotStartRequest = serde_json::from_value(
            json!({"bot":"gmail-work","method":"login_url","open_on_computer":true}),
        )
        .unwrap();
        assert!(start.open_on_computer);
        assert!(serde_json::from_value::<BotInputRequest>(
            json!({"bot":"telegram-self","step":"code","value":"1","echo":"x"})
        )
        .is_err());
        assert!(serde_json::from_value::<BotInputRequest>(
            json!({"bot":"telegram-self","step":"token","value":"1"})
        )
        .is_err());
        assert!(serde_json::from_value::<BotRequest>(json!({"bot":"x","workspace":"y"})).is_err());

        assert_eq!(
            normalize_phone(" +1 (555) 000-1111 ").unwrap(),
            "+15550001111"
        );
        for bad in [
            "",
            "12345",
            "+1555000111122223",
            "+1555abc",
            "+1 555 000 1111 2222 3",
        ] {
            assert!(normalize_phone(bad).is_err(), "{bad}");
        }
        assert_eq!(normalize_code("12 345").unwrap(), "12345");
        assert!(normalize_code("12345678901234567").is_err());
        assert!(normalize_code("12;45").is_err());
        assert!(validate_password(&"p".repeat(4096)).is_ok());
        assert!(validate_password(&"p".repeat(4097)).is_err());
        assert!(validate_password("line\nbreak").is_err());
        assert!(!valid_bot_name("../whatsapp"));
        assert!(!valid_bot_name(&"a".repeat(65)));
        assert!(valid_bot_name("gmail-work"));
    }

    #[test]
    fn only_reviewed_login_hosts_reach_the_viewer() {
        let hosts = vec!["accounts.google.com".to_string()];
        assert!(login_url_allowed(
            "https://accounts.google.com/o/oauth2/auth?client_id=x",
            &hosts
        ));
        for url in [
            "http://accounts.google.com/o/oauth2/auth",
            "https://accounts.google.com.attacker.test/",
            "https://user:pw@accounts.google.com/",
            "http://localhost:8080/",
        ] {
            assert!(!login_url_allowed(url, &hosts), "{url}");
        }
    }

    #[tokio::test]
    async fn failures_carry_a_code_and_a_message() {
        let failure = bot_failure("bot_input_not_expected");
        assert_eq!(failure.status(), StatusCode::CONFLICT);
        let body = actix_web::body::to_bytes(failure.into_body())
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"], "bot_input_not_expected");
        assert!(value["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn telegram_qr_links_render_as_png_data_urls() {
        use base64::Engine as _;
        let directory = tempfile::tempdir().unwrap();
        let text = directory.path().join("telegram-self.txt");
        std::fs::write(&text, "tg://login?token=opaque").unwrap();
        let url = crate::bot_api::qr_png_data_url(&text).expect("png");
        let encoded = url
            .strip_prefix("data:image/png;base64,")
            .expect("png data url");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let not_png = directory.path().join("whatsapp.png");
        std::fs::write(&not_png, "<svg/>").unwrap();
        assert!(crate::bot_api::qr_png_data_url(&not_png).is_none());
    }
}
