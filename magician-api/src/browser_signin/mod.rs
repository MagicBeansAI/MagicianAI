//! Owner-only launch and short-lived, single-controller browser sign-in.
//! Launch material and pixels never enter HITL projections or model history.
mod bots;
mod browser;
mod cli;
mod process;

use actix_web::{web, HttpRequest, HttpResponse};
use browser::{HumanInput, PrivateBrowser};
use magician::{
    config::BrowserSignInConfig,
    magician_v2::{auth::middleware::AuthRuntime, mcp_oauth::McpOAuthApi},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use tool_runtime_core::credential_profiles::CredentialScope;
use uuid::Uuid;
use zeroize::Zeroizing;

const MAX_SESSIONS: usize = 4;
const LIFETIME: Duration = Duration::from_secs(10 * 60);
const IDLE: Duration = Duration::from_secs(90);
const TICKET_LIFETIME: Duration = Duration::from_secs(60);

pub(crate) fn cli_credential_ready(path: &std::path::Path) -> bool {
    cli::saved_credential(path, false)
}

pub struct BrowserSignInApi {
    config: BrowserSignInConfig,
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    janitor_started: AtomicBool,
    starting_cli: Arc<Mutex<HashSet<String>>>,
    /// Viewer logins for managed-bot (supervisor-run) Google logins.
    bot_logins: bots::BotLogins,
}

struct Session {
    launch_key: String,
    scope: CredentialScope,
    correlation: String,
    url: Zeroizing<String>,
    cli: Option<Arc<cli::CliLogin>>,
    created: Instant,
    touched: Mutex<Instant>,
    claimed: AtomicBool,
    session_key: Mutex<Option<String>>,
    closed: AtomicBool,
    manual: AtomicBool,
    stop: CancellationToken,
    state: tokio::sync::Mutex<BrowserState>,
}

#[derive(Default)]
struct BrowserState {
    browser: Option<PrivateBrowser>,
    last_input: Option<(u64, [u8; 32], browser::Result<()>)>,
    last_frame: Option<Instant>,
}

fn token() -> String {
    format!("bsi_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn key(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

impl Session {
    fn revoke(&self, cancel_cli: bool) {
        self.closed.store(true, Ordering::SeqCst);
        self.stop.cancel();
        if cancel_cli {
            if let Some(cli) = &self.cli {
                cli.cancel();
            }
        }
    }

    async fn run<T>(
        &self,
        operation: impl std::future::Future<Output = browser::Result<T>>,
    ) -> browser::Result<T> {
        tokio::select! { biased;
            _ = self.stop.cancelled() => Err("signin_session_expired"),
            result = operation => result,
        }
    }

    fn active(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
            && self.created.elapsed() < LIFETIME
            && (self.manual.load(Ordering::SeqCst)
                || ((self.claimed.load(Ordering::SeqCst)
                    || self.created.elapsed() < TICKET_LIFETIME)
                    && self
                        .touched
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .elapsed()
                        < IDLE))
    }
}

impl BrowserSignInApi {
    pub fn new(config: BrowserSignInConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            janitor_started: AtomicBool::new(false),
            starting_cli: Arc::new(Mutex::new(HashSet::new())),
            bot_logins: bots::BotLogins::default(),
        }
    }

    fn start_janitor(&self) {
        if self.janitor_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let sessions = Arc::downgrade(&self.sessions);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let Some(sessions) = sessions.upgrade() else {
                    break;
                };
                sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|_, session| {
                        let done = if let Some(cli) = &session.cli {
                            cli.state.load(Ordering::SeqCst) != 0
                        } else {
                            McpOAuthApi::shared()
                                .is_some_and(|oauth| completion(session, &oauth).is_some())
                        };
                        let keep =
                            session.active() && !(session.manual.load(Ordering::SeqCst) && done);
                        if !keep {
                            session.revoke(true);
                        }
                        if keep {
                            if done {
                                session.stop.cancel();
                                if let Ok(mut state) = session.state.try_lock() {
                                    state.browser.take();
                                }
                            }
                        }
                        keep
                    });
            }
        });
    }

    fn create(
        &self,
        scope: CredentialScope,
        correlation: String,
        url: Zeroizing<String>,
        cli: Option<Arc<cli::CliLogin>>,
    ) -> Result<Value, &'static str> {
        let stop = cli
            .as_ref()
            .map(|login| login.viewer_cancellation())
            .unwrap_or_else(CancellationToken::new);
        self.create_with_stop(scope, correlation, url, cli, stop)
    }

    fn create_with_stop(
        &self,
        scope: CredentialScope,
        correlation: String,
        url: Zeroizing<String>,
        cli: Option<Arc<cli::CliLogin>>,
        stop: CancellationToken,
    ) -> Result<Value, &'static str> {
        self.start_janitor();
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        // There can be only one controller of a pending sign-in, including
        // across web and the owner's two phones.
        sessions.retain(|_, session| {
            let keep =
                session.active() && !(session.scope == scope && session.correlation == correlation);
            if !keep {
                let transferring_cli = session.cli.as_ref().is_some_and(|old_cli| {
                    cli.as_ref()
                        .is_some_and(|new_cli| Arc::ptr_eq(new_cli, old_cli))
                });
                session.revoke(!transferring_cli);
            }
            keep
        });
        if sessions.len() >= MAX_SESSIONS {
            return Err("signin_capacity_reached");
        }
        let ticket = token();
        sessions.insert(
            key(&ticket),
            Arc::new(Session {
                launch_key: key(&ticket),
                scope,
                correlation,
                url,
                cli,
                created: Instant::now(),
                touched: Mutex::new(Instant::now()),
                claimed: AtomicBool::new(false),
                session_key: Mutex::new(None),
                closed: AtomicBool::new(false),
                manual: AtomicBool::new(false),
                stop,
                state: tokio::sync::Mutex::new(BrowserState::default()),
            }),
        );
        Ok(
            json!({"mode":"computer","ticket":ticket,"viewer_path":"/api/magician/v2/auth/browser-signin/view","expires_in":60}),
        )
    }

    fn session(&self, req: &HttpRequest, claimed: bool) -> Result<Arc<Session>, HttpResponse> {
        let supplied = req
            .headers()
            .get("X-Magician-Signin")
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.starts_with("bsi_") && v.len() == 68)
            .ok_or_else(|| failure("signin_session_required"))?;
        let supplied_key = key(supplied);
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&supplied_key)
            .cloned()
            .filter(|session| {
                session.active()
                    && if claimed {
                        session
                            .session_key
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_deref()
                            == Some(supplied_key.as_str())
                    } else {
                        !session.claimed.load(Ordering::SeqCst)
                    }
            })
            .ok_or_else(|| failure("signin_session_expired"))?;
        *session.touched.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        Ok(session)
    }
}

fn response(value: Value) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, private"))
        .insert_header(("Referrer-Policy", "no-referrer"))
        .json(value)
}

fn failure(code: &'static str) -> HttpResponse {
    HttpResponse::BadRequest().insert_header(("Cache-Control", "no-store"))
        .json(json!({"error":code,"message":"Sign-in could not continue. Reopen the sign-in request, or finish on the computer."}))
}

fn oauth_launch_failure(
    error: magician::magician_v2::mcp_oauth::McpOAuthProductError,
) -> HttpResponse {
    // The product error code is secret-free; without it every refusal below
    // reads the same and a provider-side failure cannot be told apart.
    tracing::warn!(code = ?error.code, "browser sign-in launch refused by MCP OAuth");
    if error.code == magician::magician_v2::mcp_oauth::McpOAuthProductErrorCode::RegistrationFailed
    {
        return HttpResponse::BadGateway()
            .insert_header(("Cache-Control", "no-store"))
            .json(json!({
                "error": "signin_registration_failed",
                "message": "The service could not register Magican for sign-in. No sign-in window was opened. Retry later or use the service's approved client setup."
            }));
    }
    failure("signin_request_unavailable")
}

async fn accounts(
    req: HttpRequest,
    skills: web::Data<crate::skills_api::SkillsApi>,
    vault: web::Data<crate::secret_vault_api::SecretVaultApi>,
    oauth: web::Data<McpOAuthApi>,
    auth: Option<web::Data<AuthRuntime>>,
    api: web::Data<BrowserSignInApi>,
    bot_api: Option<web::Data<crate::bot_api::BotApi>>,
) -> HttpResponse {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        return response;
    }
    let rows = match crate::skills_api::browser_signin_accounts(&req, &skills) {
        Ok(rows) => rows,
        Err(error) => return error,
    };
    // Each check is bounded (3 s) and they run together, so one slow
    // provider cannot stall the list; a slow or failed check reads `unknown`.
    let statuses = futures_util::future::join_all(rows.iter().map(|row| {
        crate::skills_api::skill_account_status(
            &req,
            &skills,
            &vault,
            &oauth,
            &row.name,
            row.endpoint_alias.as_deref(),
        )
    }))
    .await;
    let accounts: Vec<Value> = rows
        .into_iter()
        .zip(statuses)
        .map(|(row, (status, detail))| {
            let mut account = json!({
                "name": row.name,
                "label": row.label,
                "kind": row.kind,
                "status": status,
                "detail": detail,
            });
            if let Some(alias) = row.endpoint_alias {
                account["endpoint_alias"] = json!(alias);
            }
            account
        })
        .collect();
    // Managed bots (Google Workspace per profile, Telegram, WhatsApp) come
    // from the scope's bot config, after the skill-bound accounts.
    let mut accounts = accounts;
    accounts.extend(
        bots::account_values(
            &api,
            &req,
            &skills,
            bot_api.as_ref().map(|data| data.get_ref()),
        )
        .await,
    );
    response(json!({"accounts":accounts}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchRequest {
    correlation_id: Option<String>,
    skill: Option<String>,
    /// One declared MCP endpoint alias of a multi-endpoint governed skill
    /// (Service-accounts row). Omitted = the manifest's default alias.
    #[serde(default)]
    endpoint_alias: Option<String>,
    #[serde(default)]
    open_on_computer: bool,
}

async fn launch(
    req: HttpRequest,
    body: web::Json<LaunchRequest>,
    api: web::Data<BrowserSignInApi>,
    oauth: web::Data<McpOAuthApi>,
    auth: Option<web::Data<AuthRuntime>>,
    skills: web::Data<crate::skills_api::SkillsApi>,
    vault: web::Data<crate::secret_vault_api::SecretVaultApi>,
) -> HttpResponse {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|v| v.get_ref()),
    )
    .await
    {
        return response;
    }
    let (principal, workspace) = match crate::scope::resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(error) => return error,
    };
    let scope = match CredentialScope::new(principal, workspace) {
        Ok(scope) => scope,
        Err(_) => return failure("signin_scope_invalid"),
    };
    let (correlation, url) = match (&body.correlation_id, &body.skill) {
        (Some(correlation), None) if body.endpoint_alias.is_none() => {
            if oauth.correlation_completed(correlation, &scope) {
                return response(json!({"mode":"connected"}));
            }
            if body.open_on_computer {
                return match oauth
                    .retry_browser_for_correlation(correlation, &scope)
                    .await
                {
                    Ok(()) => response(json!({"mode":"computer_opened"})),
                    Err(_) => failure("signin_browser_unavailable"),
                };
            }
            let url = match oauth.launch_for_correlation(correlation, &scope) {
                Ok(url) => url,
                Err(_) => return failure("signin_request_expired"),
            };
            (correlation.clone(), url)
        },
        (None, Some(name)) => {
            match crate::skills_api::browser_cli_setup(&req, &skills, name, true) {
                Ok(Some(_)) if body.endpoint_alias.is_some() => {
                    return failure("signin_request_invalid")
                },
                Ok(Some(setup)) => {
                    return match api.start_cli(scope, name, setup).await {
                        Ok(value) if body.open_on_computer => {
                            match api.open_created_on_computer(value).await {
                                Ok(value) => response(value),
                                Err(error) => failure(error),
                            }
                        },
                        Ok(value) => response(value),
                        Err(error) => failure(error),
                    };
                },
                Ok(None) => {},
                Err(response) => return response,
            }
            let (coordinator, _) = match crate::skills_api::skill_oauth_coordinator(
                &req,
                &skills,
                &vault,
                &oauth,
                name,
                body.endpoint_alias.as_deref(),
            ) {
                Ok(result) => result,
                Err(response) => return response,
            };
            let launch = match oauth.register_and_begin_for_native_setup(coordinator).await {
                Ok(launch) => launch,
                Err(error) => return oauth_launch_failure(error),
            };
            if body.open_on_computer {
                return match oauth
                    .retry_browser_for_correlation(&launch.pending().correlation_id(), &scope)
                    .await
                {
                    Ok(()) => response(json!({"mode":"computer_opened"})),
                    Err(_) => failure("signin_browser_unavailable"),
                };
            }
            (
                launch.pending().correlation_id(),
                Zeroizing::new(launch.authorization_url().to_owned()),
            )
        },
        _ => return failure("signin_request_invalid"),
    };
    match api.oauth_handoff(scope, correlation, url, &oauth) {
        Ok(value) => response(value),
        Err(error) => failure(error),
    }
}

impl BrowserSignInApi {
    pub(crate) fn oauth_handoff(
        &self,
        scope: CredentialScope,
        correlation: String,
        url: Zeroizing<String>,
        oauth: &McpOAuthApi,
    ) -> Result<Value, &'static str> {
        let stop = match oauth.viewer_cancellation(&correlation, &scope) {
            Ok(stop) => stop,
            Err(_) if oauth.correlation_completed(&correlation, &scope) => {
                return Ok(json!({"mode":"connected"}))
            },
            Err(_) => return Err("signin_request_expired"),
        };
        let authorization = url::Url::parse(&url).map_err(|_| "signin_url_invalid")?;
        if !browser::allowed_page(&url) || url.as_str() == "about:blank" {
            return Err("signin_url_invalid");
        }
        let redirect = url::Url::parse(&url)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "redirect_uri")
                    .map(|(_, value)| value.into_owned())
            })
            .and_then(|value| url::Url::parse(&value).ok());
        if device_browser_url(&authorization) && redirect.as_ref().is_some_and(device_browser_url) {
            return Ok(json!({"mode":"direct","authorization_url":url.as_str()}));
        }
        self.create_with_stop(scope, correlation, url, None, stop)
    }
}

// A public callback alone says nothing about whether a phone can reach the
// authorization page. Local providers must stay on the engine's browser.
fn device_browser_url(url: &url::Url) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            host.contains('.')
                && host != "localhost"
                && ![
                    ".localhost",
                    ".local",
                    ".localdomain",
                    ".internal",
                    ".lan",
                    ".home.arpa",
                ]
                .iter()
                .any(|suffix| host.ends_with(*suffix))
        },
        Some(url::Host::Ipv4(ip)) => public_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_unique_local()
                && !ip.is_unicast_link_local()
                && !ip.is_multicast()
                && ip.to_ipv4().is_none_or(public_ipv4)
        },
        None => false,
    }
}

fn public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !ip.is_loopback()
        && !ip.is_private()
        && !ip.is_link_local()
        && !ip.is_unspecified()
        && !ip.is_broadcast()
        && !ip.is_multicast()
        && a != 0
        && a < 240
        && !(a == 100 && (64..128).contains(&b))
        && !(a == 198 && (b == 18 || b == 19))
}

impl BrowserSignInApi {
    async fn open_created_on_computer(&self, value: Value) -> Result<Value, &'static str> {
        if value["mode"] == "connected" {
            return Ok(value);
        }
        let ticket = value["ticket"].as_str().ok_or("signin_request_invalid")?;
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key(ticket))
            .cloned()
            .ok_or("signin_session_expired")?;
        session.manual.store(true, Ordering::SeqCst);
        if session.run(system_browser(&session.url)).await.is_err() {
            session.revoke(true);
            return Err("signin_browser_unavailable");
        }
        Ok(json!({"mode":"computer_opened"}))
    }
    pub(crate) async fn start_cli(
        &self,
        scope: CredentialScope,
        name: &str,
        setup: crate::skills_api::BrowserCliSetup,
    ) -> Result<Value, &'static str> {
        let correlation = format!("cli:{name}");
        let magician_components::setup::SetupDriver::BrowserCli { executable, .. } =
            &setup.setup.driver
        else {
            return Err("signin_cli_setup_invalid");
        };
        let existing = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|session| {
                session.scope == scope && session.correlation == correlation && session.active()
            })
            .and_then(|session| session.cli.clone())
            .filter(|login| login.state.load(Ordering::SeqCst) == 0);
        if let Some(login) = existing {
            return self.create(
                scope,
                correlation,
                Zeroizing::new(login.url.to_string()),
                Some(login),
            );
        }
        // CLI callbacks can use a fixed localhost port. A second workspace
        // must wait rather than start a competing listener with another HOME.
        // Aliases of the same reviewed CLI still share its callback listener.
        let start_key = format!("cli-program:{executable}");
        {
            let mut starting = self.starting_cli.lock().unwrap_or_else(|e| e.into_inner());
            if starting.contains(&start_key) {
                return Err("signin_cli_already_starting");
            }
            if self
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .any(|session| {
                    session.active()
                        && session.correlation == correlation
                        && session
                            .cli
                            .as_ref()
                            .is_some_and(|login| login.state.load(Ordering::SeqCst) == 0)
                })
            {
                return Err("signin_cli_already_starting");
            }
            if starting.len()
                + self
                    .sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .len()
                >= MAX_SESSIONS
            {
                return Err("signin_capacity_reached");
            }
            starting.insert(start_key.clone());
        }
        let reservation = CliReservation {
            keys: Arc::clone(&self.starting_cli),
            key: start_key,
        };
        let magician_components::setup::SetupDriver::BrowserCli {
            args,
            login_url_hosts,
            ..
        } = setup.setup.driver
        else {
            return Err("signin_cli_setup_invalid");
        };
        let login = match cli::start(
            &setup.executable,
            &args,
            &login_url_hosts,
            &setup.home,
            &setup.credential,
            &setup.path,
            reservation,
        )
        .await
        {
            Ok(login) => login,
            Err("signin_already_connected") => return Ok(json!({"mode":"connected"})),
            Err(error) => return Err(error),
        };
        self.create(
            scope,
            correlation,
            Zeroizing::new(login.url.to_string()),
            Some(login),
        )
    }
}

async fn system_browser(url: &str) -> Result<(), &'static str> {
    let parsed = url::Url::parse(url).map_err(|_| "signin_url_invalid")?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("signin_url_invalid");
    }
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "linux")]
    let mut command =
        tokio::process::Command::new(runtime_core::process::resolve_program_str("xdg-open", None));
    #[cfg(target_os = "windows")]
    let mut command = {
        let path = std::env::var_os("SYSTEMROOT")
            .map(std::path::PathBuf::from)
            .ok_or("signin_browser_unavailable")?
            .join("System32/rundll32.exe");
        let mut command = tokio::process::Command::new(path);
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err("signin_browser_unavailable");
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        command.env_clear();
        for name in [
            "PATH",
            "HOME",
            "USERPROFILE",
            "USER",
            "LANG",
            "LC_ALL",
            "SYSTEMROOT",
            "WINDIR",
            "TMP",
            "TEMP",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(5), command.status()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            _ => Err("signin_browser_unavailable"),
        }
    }
}

struct CliReservation {
    keys: Arc<Mutex<HashSet<String>>>,
    key: String,
}
impl Drop for CliReservation {
    fn drop(&mut self) {
        self.keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

async fn claim(
    req: HttpRequest,
    api: web::Data<BrowserSignInApi>,
    oauth: web::Data<McpOAuthApi>,
) -> HttpResponse {
    let session = match api.session(&req, false) {
        Ok(session) => session,
        Err(error) => return error,
    };
    if let Some(done) = completed_claim(&session, &oauth) {
        return done;
    }
    if session.created.elapsed() > TICKET_LIFETIME || session.claimed.swap(true, Ordering::SeqCst) {
        return failure("signin_ticket_expired");
    }
    let mut state = session.state.lock().await;
    if let Some(done) = completed_claim(&session, &oauth) {
        return done;
    }
    let browser = match session
        .run(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                PrivateBrowser::start(
                    &session.url,
                    api.config.browser_executable.as_deref(),
                    session.stop.clone(),
                ),
            )
            .await
            .unwrap_or(Err("signin_browser_timeout"))
        })
        .await
    {
        Ok(browser) => browser,
        Err(error) => {
            if let Some(done) = completed_claim(&session, &oauth) {
                return done;
            }
            session.revoke(true);
            return failure(error);
        },
    };
    if let Some(done) = completed_claim(&session, &oauth) {
        return done;
    }
    let secret = token();
    let ticket_key = req
        .headers()
        .get("X-Magician-Signin")
        .and_then(|v| v.to_str().ok())
        .map(key)
        .unwrap_or_default();
    let mut sessions = api.sessions.lock().unwrap_or_else(|e| e.into_inner());
    if !session.active()
        || !sessions
            .get(&ticket_key)
            .is_some_and(|registered| Arc::ptr_eq(registered, &session))
    {
        return failure("signin_session_expired");
    }
    state.browser = Some(browser);
    sessions.remove(&ticket_key);
    let session_key = key(&secret);
    *session
        .session_key
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(session_key.clone());
    sessions.insert(session_key, Arc::clone(&session));
    response(
        json!({"session_token":secret,"width":browser::WIDTH,"height":browser::HEIGHT,
        "principal":session.scope.principal.as_str(),"workspace":session.scope.workspace.as_str()}),
    )
}

fn completed_claim(session: &Session, oauth: &McpOAuthApi) -> Option<HttpResponse> {
    completion(session, oauth).map(|done| {
        session.revoke(true);
        if done["status"] == "connected" {
            response(done)
        } else {
            failure("signin_ticket_expired")
        }
    })
}

fn completion(session: &Session, oauth: &McpOAuthApi) -> Option<Value> {
    if let Some(cli) = &session.cli {
        return match cli.state.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(json!({"status":"connected"})),
            _ => Some(json!({"status":"expired"})),
        };
    }
    if oauth.correlation_is_live(&session.correlation, &session.scope) {
        return None;
    }
    Some(
        json!({"status": if oauth.correlation_completed(&session.correlation, &session.scope) {"connected"} else {"expired"}}),
    )
}

async fn frame(
    req: HttpRequest,
    api: web::Data<BrowserSignInApi>,
    oauth: web::Data<McpOAuthApi>,
) -> HttpResponse {
    let session = match api.session(&req, true) {
        Ok(session) => session,
        Err(error) => return error,
    };
    let mut state = session.state.lock().await;
    if !session.active() {
        return failure("signin_session_expired");
    }
    if let Some(done) = completion(&session, &oauth) {
        state.browser.take();
        return response(done);
    }
    if state
        .last_frame
        .is_some_and(|last| last.elapsed() < Duration::from_millis(250))
    {
        return response(json!({"status":"waiting","retry_after_ms":250}));
    }
    state.last_frame = Some(Instant::now());
    let Some(browser) = state.browser.as_mut() else {
        return failure("signin_browser_closed");
    };
    let result = session.run(browser.frame()).await;
    if session.stop.is_cancelled() {
        state.browser.take();
    }
    if !session.active() {
        state.browser.take();
        return failure("signin_session_expired");
    }
    if let Some(done) = completion(&session, &oauth) {
        state.browser.take();
        return response(done);
    }
    match result {
        Ok(frame) => response(frame),
        Err("signin_page_loading" | "signin_browser_command_failed") => {
            response(json!({"status":"waiting","origin":"Loading…","frame_revision":""}))
        },
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputRequest {
    sequence: u64,
    frame_revision: String,
    input: HumanInput,
}

async fn input(
    req: HttpRequest,
    body: web::Json<InputRequest>,
    api: web::Data<BrowserSignInApi>,
    oauth: web::Data<McpOAuthApi>,
) -> HttpResponse {
    let session = match api.session(&req, true) {
        Ok(session) => session,
        Err(error) => return error,
    };
    let mut state = session.state.lock().await;
    if !session.active() {
        return failure("signin_request_closed");
    }
    if let Some(done) = completion(&session, &oauth) {
        state.browser.take();
        return response(done);
    }
    let request = body.into_inner();
    let secret = session
        .session_key
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let bytes = match serde_json::to_vec(&(request.frame_revision.as_str(), &request.input)) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(_) => return failure("signin_input_invalid"),
    };
    let fingerprint =
        *blake3::keyed_hash(blake3::hash(secret.as_bytes()).as_bytes(), &bytes).as_bytes();
    if let Some((sequence, binding, result)) = state.last_input {
        if request.sequence == sequence {
            if binding != fingerprint {
                return failure("signin_input_sequence_reused");
            }
            return match result {
                Ok(()) => response(json!({"ok":true,"duplicate":true})),
                Err(error) => failure(error),
            };
        }
    }
    if request.sequence
        != state
            .last_input
            .map(|(sequence, _, _)| sequence + 1)
            .unwrap_or(1)
    {
        return failure("signin_input_out_of_order");
    }
    state.last_input = Some((request.sequence, fingerprint, Err("signin_input_uncertain")));
    let Some(browser) = state.browser.as_mut() else {
        return failure("signin_browser_closed");
    };
    let result = session
        .run(browser.input(&request.frame_revision, request.input))
        .await;
    state.last_input = Some((request.sequence, fingerprint, result));
    if session.stop.is_cancelled() {
        state.browser.take();
    }
    if !session.active() {
        return failure("signin_session_expired");
    }
    if let Some(done) = completion(&session, &oauth) {
        state.browser.take();
        return response(done);
    }
    match result {
        Ok(()) => response(json!({"ok":true})),
        Err(error) => failure(error),
    }
}

async fn close(req: HttpRequest, api: web::Data<BrowserSignInApi>) -> HttpResponse {
    let supplied = match req
        .headers()
        .get("X-Magician-Signin")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) if value.starts_with("bsi_") && value.len() == 68 => key(value),
        _ => return failure("signin_session_required"),
    };
    let session = {
        let sessions = api.sessions.lock().unwrap_or_else(|e| e.into_inner());
        // A consumed launch ticket may revoke its own viewer, but never drive
        // it. This also covers closing while claim is rotating the ticket.
        let session = sessions.get(&supplied).or_else(|| {
            sessions
                .values()
                .find(|session| session.launch_key == supplied)
        });
        let Some(session) = session.filter(|session| session.active()).cloned() else {
            return response(json!({"closed":true}));
        };
        session.revoke(true);
        session
    };
    session.state.lock().await.browser.take();
    response(json!({"closed":true}))
}

async fn viewer() -> HttpResponse {
    HttpResponse::Ok().content_type("text/html; charset=utf-8")
        .insert_header(("Cache-Control", "no-store"))
        .insert_header(("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self'; img-src data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"))
        .insert_header(("Referrer-Policy", "no-referrer"))
        .insert_header(("X-Content-Type-Options", "nosniff"))
        .body(include_str!("viewer.html"))
}
async fn viewer_js() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/javascript; charset=utf-8")
        .insert_header(("Cache-Control", "no-store"))
        .body(include_str!("viewer.js"))
}
async fn viewer_css() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/css; charset=utf-8")
        .insert_header(("Cache-Control", "no-store"))
        .body(include_str!("viewer.css"))
}

pub fn configure_routes(config: &mut web::ServiceConfig) {
    config.service(
        web::scope("/auth/browser-signin")
            .app_data(
                web::JsonConfig::default()
                    .limit(8192)
                    .error_handler(|_, _| {
                        actix_web::error::InternalError::from_response(
                            "invalid_signin_request",
                            failure("signin_request_invalid"),
                        )
                        .into()
                    }),
            )
            .route("/launch", web::post().to(launch))
            .route("/accounts", web::post().to(accounts))
            .route("/claim", web::post().to(claim))
            .route("/frame", web::post().to(frame))
            .route("/input", web::post().to(input))
            .route("/close", web::post().to(close))
            .route("/bot/start", web::post().to(bots::start))
            .route("/bot/input", web::post().to(bots::input))
            .route("/bot/state", web::post().to(bots::state))
            .route("/bot/cancel", web::post().to(bots::cancel))
            .route("/bot/project-access", web::post().to(bots::project_access))
            .route(
                "/bot/project-access/grant",
                web::post().to(bots::grant_project_access),
            )
            .route("/view", web::get().to(viewer))
            .route("/view.js", web::get().to(viewer_js))
            .route("/view.css", web::get().to(viewer_css)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[tokio::test]
    async fn registration_failure_returns_actionable_secret_free_launch_error() {
        let response =
            oauth_launch_failure(magician::magician_v2::mcp_oauth::McpOAuthProductError {
                code:
                    magician::magician_v2::mcp_oauth::McpOAuthProductErrorCode::RegistrationFailed,
                message: "https://provider.example/register?client_secret=canary-secret",
            });
        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_GATEWAY);
        assert_eq!(response.headers().get("Cache-Control").unwrap(), "no-store");
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"], "signin_registration_failed");
        assert!(value["message"]
            .as_str()
            .unwrap()
            .contains("No sign-in window was opened"));
        assert!(!String::from_utf8_lossy(&body).contains("canary-secret"));
        assert!(!String::from_utf8_lossy(&body).contains("provider.example"));
    }

    fn pending_session(api: &BrowserSignInApi) -> Arc<Session> {
        let issued = api
            .create(
                CredentialScope::new("owner", "default").unwrap(),
                "pending".into(),
                Zeroizing::new("https://provider.example/auth".into()),
                None,
            )
            .unwrap();
        let request = TestRequest::default()
            .insert_header(("X-Magician-Signin", issued["ticket"].as_str().unwrap()))
            .to_http_request();
        api.session(&request, false).ok().unwrap()
    }

    #[tokio::test]
    async fn revoked_controllers_never_start_a_queued_operation() {
        let api = BrowserSignInApi::new(BrowserSignInConfig::default());
        let session = pending_session(&api);
        session.revoke(false);
        let ran = AtomicBool::new(false);
        let result = session
            .run(async {
                ran.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert_eq!(result, Err("signin_session_expired"));
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn revocation_interrupts_an_operation_waiting_for_the_browser() {
        let api = BrowserSignInApi::new(BrowserSignInConfig::default());
        let session = pending_session(&api);
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = Arc::clone(&started);
        let worker = Arc::clone(&session);
        let task = tokio::spawn(async move {
            worker
                .run(async {
                    signal.notify_one();
                    std::future::pending::<browser::Result<()>>().await
                })
                .await
        });
        started.notified().await;
        session.revoke(false);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap(),
            Err("signin_session_expired")
        );
    }

    #[test]
    fn phone_routing_keeps_local_authorization_pages_on_the_engine() {
        for address in [
            "http://localhost:3000/auth",
            "https://localhost/auth",
            "https://metabase.local/auth",
            "https://metabase.internal/auth",
            "https://192.168.50.2/auth",
            "https://100.64.0.1/auth",
            "https://[::1]/auth",
            "https://[fd00::1]/auth",
            "https://[::ffff:127.0.0.1]/auth",
        ] {
            assert!(
                !device_browser_url(&url::Url::parse(address).unwrap()),
                "{address}"
            );
        }
        assert!(device_browser_url(
            &url::Url::parse("https://provider.example/authorize").unwrap()
        ));
    }

    #[tokio::test]
    async fn aliases_cannot_start_a_second_listener_for_the_same_cli() {
        let api = BrowserSignInApi::new(BrowserSignInConfig::default());
        api.starting_cli
            .lock()
            .unwrap()
            .insert("cli-program:higgsfield".into());
        let temporary = tempfile::tempdir().unwrap();
        for alias in ["first-alias", "second-alias"] {
            let binding: magician_components::setup::SetupBinding =
                serde_json::from_value(json!({"definition":"higgsfield-account"})).unwrap();
            let setup = crate::skills_api::BrowserCliSetup {
                setup: magician_components::setup::resolve(&binding).unwrap(),
                executable: temporary.path().join("not-installed"),
                home: temporary.path().to_path_buf(),
                credential: temporary.path().join("credentials.json"),
                path: std::ffi::OsString::new(),
            };
            let result = api
                .start_cli(
                    CredentialScope::new("owner", "default").unwrap(),
                    alias,
                    setup,
                )
                .await;
            assert_eq!(result.unwrap_err(), "signin_cli_already_starting");
        }
    }

    #[tokio::test]
    async fn a_launch_ticket_cannot_drive_a_browser_even_during_claim() {
        let api = BrowserSignInApi::new(BrowserSignInConfig::default());
        let scope = CredentialScope::new("owner", "default").unwrap();
        let issued = api
            .create(
                scope,
                "pending".into(),
                Zeroizing::new("https://provider.example/auth".into()),
                None,
            )
            .unwrap();
        let ticket = issued["ticket"].as_str().unwrap();
        let request = TestRequest::default()
            .insert_header(("X-Magician-Signin", ticket))
            .to_http_request();
        let session = api.session(&request, false).ok().unwrap();
        assert!(api.session(&request, true).is_err());
        session.claimed.store(true, Ordering::SeqCst);
        assert!(api.session(&request, true).is_err());
        assert!(api.session(&request, false).is_err());
    }

    #[tokio::test]
    async fn another_device_revokes_the_previous_controller_without_crossing_scopes() {
        let api = BrowserSignInApi::new(BrowserSignInConfig::default());
        let create = |workspace: &str| {
            api.create(
                CredentialScope::new("owner", workspace).unwrap(),
                "pending".into(),
                Zeroizing::new("https://provider.example/auth".into()),
                None,
            )
            .unwrap()
        };
        let first = create("first");
        let other = create("other");
        let first_request = TestRequest::default()
            .insert_header(("X-Magician-Signin", first["ticket"].as_str().unwrap()))
            .to_http_request();
        let other_request = TestRequest::default()
            .insert_header(("X-Magician-Signin", other["ticket"].as_str().unwrap()))
            .to_http_request();
        let original = api.session(&first_request, false).ok().unwrap();
        create("first");
        assert!(!original.active());
        assert!(api.session(&first_request, false).is_err());
        assert!(api.session(&other_request, false).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn saved_credentials_must_be_private_regular_files_without_links() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials.json");
        std::fs::write(&path, b"opaque-test-credential").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!cli::saved_credential(&path, false));
        assert!(cli::saved_credential(&path, true));
        assert!(cli_credential_ready(&path));
        let linked = directory.path().join("linked.json");
        symlink(&path, &linked).unwrap();
        assert!(!cli_credential_ready(&linked));
        std::fs::hard_link(&path, directory.path().join("hard-link.json")).unwrap();
        assert!(!cli_credential_ready(&path));
    }

    #[tokio::test]
    async fn the_launch_ticket_can_cancel_during_and_after_claim_but_cannot_control_it() {
        for rotated in [false, true] {
            let api = web::Data::new(BrowserSignInApi::new(BrowserSignInConfig::default()));
            let issued = api
                .create(
                    CredentialScope::new("owner", "default").unwrap(),
                    "pending".into(),
                    Zeroizing::new("https://provider.example/auth".into()),
                    None,
                )
                .unwrap();
            let ticket = issued["ticket"].as_str().unwrap();
            let request = TestRequest::default()
                .insert_header(("X-Magician-Signin", ticket))
                .to_http_request();
            let session = api.session(&request, false).ok().unwrap();
            session.claimed.store(true, Ordering::SeqCst);
            if rotated {
                let rotated_key = key(&token());
                *session.session_key.lock().unwrap() = Some(rotated_key.clone());
                let mut sessions = api.sessions.lock().unwrap();
                sessions.remove(&key(ticket));
                sessions.insert(rotated_key, Arc::clone(&session));
            }
            assert!(api.session(&request, true).is_err());
            assert_eq!(
                close(request, api).await.status(),
                actix_web::http::StatusCode::OK
            );
            assert!(!session.active());
        }
    }

    #[test]
    fn launch_never_accepts_a_client_supplied_url_or_executable() {
        for value in [
            json!({"correlation_id":"pending","url":"https://attacker.test"}),
            json!({"skill":"provider","executable":"sh"}),
            json!({"skill":"provider","endpoint_url":"https://attacker.test"}),
        ] {
            assert!(serde_json::from_value::<LaunchRequest>(value).is_err());
        }
        let request: LaunchRequest =
            serde_json::from_value(json!({"skill":"provider","endpoint_alias":"im"}))
                .expect("an endpoint alias is a declared, optional launch field");
        assert_eq!(request.endpoint_alias.as_deref(), Some("im"));
    }
}
