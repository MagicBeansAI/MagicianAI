//! Browser pairing and per-run browser choice for runs that drive the owner's
//! own browsers through Magicutor's CDP proxy.
//!
//! Several browsers (Chrome profiles, Comet, Dia, …) can share Magicutor's
//! bridge. Two owner decisions are raised here, as ordinary decision cards
//! through the [`UserRequestService`], so they can be answered from the phone
//! or web as well as in the browser:
//!
//! * **Pairing** — Magicutor admits a browser it has not seen before as
//!   `pending` (a slot, no work). "A new browser wants to connect: <label>
//!   (<fingerprint>) - Accept?" Accept calls Magicutor's owner route
//!   `POST /bridge/browsers/{fingerprint}/accept`.
//! * **Per-run choice** — when an *attended* run first opens a CDP session,
//!   more than one accepted browser is connected and the thread has no pin,
//!   the choice is made BEFORE the session connects: `POST
//!   /cdp/threads/{id}/choose` makes Magicutor show a card in every accepted
//!   browser and returns at once; the same choice is raised here as a
//!   decision card, whose answer pins the thread with `PUT
//!   /cdp/threads/{id}/browser`. Magician then polls `GET
//!   /cdp/threads/{id}/browser` until the thread is pinned (bounded by the
//!   choice timeout plus [`CHOICE_WAIT_MARGIN`], stopping early when the run
//!   is cancelled) and only then lets the session connect normally. When a
//!   browser answers first, this card is resolved (channel
//!   [`CHOICE_SETTLED_CHANNEL`]) so it does not linger; a late answer is a
//!   no-op because Magicutor keeps the existing pin. Any Magicutor error just
//!   proceeds: the connect then uses the default browser.
//!
//! Unattended runs (scheduled, ambient, autonomous, delegated without a chat,
//! automated harnesses) never ask: they go to the default browser.
//!
//! Wiring: [`prepare_browser_choice`] runs once a flat browser session exists
//! and before its first command connects it. It never fails the run.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

use super::session::{AgentBrowserSession, ConnectionMode};
use crate::magician_v2::agents::{AgentInvocationContext, InvocationSourceKind, InvocationSurface};
use crate::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use crate::magician_v2::execution::primitive_dispatch::exec_ctx::PrimitiveExecCtx;
use crate::magician_v2::user_requests::{
    RequestOption, UserRequest, UserRequestService, UserResponse,
};

/// `never` turns per-run browser questions off for this process (operators,
/// automated harnesses). Pairing cards are unaffected.
pub const ENV_BROWSER_CHOICE: &str = "MAGICIAN_BROWSER_CHOICE";
/// Magicutor's bridge token, when it sets one; needed only to accept a
/// pending browser from Magician (an empty Magicutor token admits any caller).
pub const ENV_MAGICUTOR_BRIDGE_TOKEN: &str = "MAGICIAN_MAGICUTOR_BRIDGE_TOKEN";
/// Channel used to resolve a choice card the browser side already settled.
pub const CHOICE_SETTLED_CHANNEL: &str = "magicutor_browser";
/// Option id for "use the default browser".
pub const DEFAULT_BROWSER_OPTION: &str = "default";
const BROWSER_OPTION_PREFIX: &str = "browser:";
const PAIRING_ACCEPT_OPTION: &str = "accept";
const PAIRING_DECLINE_OPTION: &str = "not_now";
/// Used when Magicutor does not report its own choice timeout.
const FALLBACK_CHOICE_TIMEOUT_SECS: u64 = 20;
const MAX_TASK_TITLE_CHARS: usize = 80;
/// Longest choice timeout honoured, matching Magicutor's own cap.
const MAX_CHOICE_TIMEOUT_SECS: u64 = 120;
const MAGICUTOR_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the pin is polled while the owner chooses.
pub const PIN_POLL_INTERVAL: Duration = Duration::from_millis(400);
/// Extra wait past the choice timeout, so Magicutor's own timeout (which
/// pins the default browser) is normally observed before connecting.
pub const CHOICE_WAIT_MARGIN: Duration = Duration::from_secs(3);
/// Consecutive failed polls after which Magicutor counts as unavailable.
const MAX_POLL_FAILURES: u32 = 2;

/// One browser as Magicutor reports it (`GET /cdp/threads/{id}/browser`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BridgeBrowser {
    pub fingerprint: String,
    pub label: String,
    /// `accepted`, `pending` or `legacy`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub default: bool,
}

/// Magicutor's view of a thread before its CDP session connects.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ThreadBrowserInfo {
    #[serde(default)]
    pub pinned: Option<String>,
    #[serde(default)]
    pub would_ask: bool,
    #[serde(default)]
    pub browsers: Vec<BridgeBrowser>,
    #[serde(default)]
    pub choice_timeout_secs: Option<u64>,
}

/// Whether a run counts as attended — the owner started it from a chat or a
/// device and can answer now. Scheduled, ambient, autonomous, public and
/// harness-driven runs, and delegated children without a chat, do not.
pub fn run_is_attended(
    chat_session_id: Option<&str>,
    invocation: Option<&AgentInvocationContext>,
) -> bool {
    if let Some(invocation) = invocation {
        if matches!(
            invocation.source_kind,
            InvocationSourceKind::Autonomous
                | InvocationSourceKind::ProductFeature
                | InvocationSourceKind::Public
        ) || matches!(
            invocation.surface,
            InvocationSurface::Plane | InvocationSurface::Meeting | InvocationSurface::PublicEnvoy
        ) {
            return false;
        }
    }
    let chat = chat_session_id
        .or_else(|| invocation.and_then(|invocation| invocation.chat_session_id.as_deref()))
        .is_some_and(|chat| !chat.trim().is_empty());
    let interactive = invocation.is_some_and(|invocation| {
        matches!(
            invocation.source_kind,
            InvocationSourceKind::Direct | InvocationSourceKind::ChatInline
        ) && matches!(
            invocation.surface,
            InvocationSurface::Chat
                | InvocationSurface::RealtimeVoice
                | InvocationSurface::AppCopilot
                | InvocationSurface::ContextualAssist
                | InvocationSurface::Tutor
                | InvocationSurface::ThinkingMap
        )
    });
    chat || interactive
}

fn choice_disabled_by_env() -> bool {
    std::env::var(ENV_BROWSER_CHOICE)
        .map(|value| value.trim().eq_ignore_ascii_case("never"))
        .unwrap_or(false)
}

/// Whether this execution may ask the owner which browser to use.
pub fn exec_ctx_is_attended(exec_ctx: &PrimitiveExecCtx) -> bool {
    !choice_disabled_by_env()
        && run_is_attended(
            exec_ctx.chat_session_id.as_deref(),
            exec_ctx.invocation_context.as_ref(),
        )
}

fn short_task_title(task: &str) -> String {
    let cleaned: String = task.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "this task".to_string();
    }
    let mut short: String = trimmed.chars().take(MAX_TASK_TITLE_CHARS).collect();
    if trimmed.chars().count() > MAX_TASK_TITLE_CHARS {
        short.push('…');
    }
    short
}

/// A short title for the run: the caller's, else the page it opens.
pub fn task_title(explicit: Option<&str>, initial_url: Option<&str>) -> String {
    if let Some(title) = explicit.map(str::trim).filter(|title| !title.is_empty()) {
        return short_task_title(title);
    }
    initial_url
        .and_then(|url| Url::parse(url).ok())
        .and_then(|url| url.host_str().map(|host| format!("browse {host}")))
        .unwrap_or_else(|| "this task".to_string())
}

/// The CDP thread id in a Magicutor proxy URL (`…/devtools/browser/<id>`),
/// unless the URL already names a browser or is the unscoped proxy.
pub fn cdp_thread_from_url(cdp_url: &str) -> Option<String> {
    let url = Url::parse(cdp_url).ok()?;
    if url
        .query_pairs()
        .any(|(key, _)| key == "browser" || key == "choose")
    {
        return None;
    }
    let id = url.path().strip_prefix("/devtools/browser/")?;
    if id.is_empty() || id == "magicutor-proxy" || id.contains('/') {
        return None;
    }
    Some(id.to_string())
}

fn http_base_from_cdp_url(cdp_url: &str) -> Option<Url> {
    let parsed = Url::parse(cdp_url).ok()?;
    let scheme = match parsed.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return None,
    };
    let host = parsed.host_str()?;
    let port = parsed.port_or_known_default()?;
    Url::parse(&format!("{scheme}://{host}:{port}/")).ok()
}

struct Scope {
    principal: String,
    workspace: String,
    execution_id: Option<String>,
    task_id: Option<String>,
    agent_id: Option<String>,
}

impl Scope {
    fn of(exec_ctx: &PrimitiveExecCtx) -> Self {
        Self {
            principal: exec_ctx
                .principal
                .clone()
                .unwrap_or_else(|| DEFAULT_SCOPE_PRINCIPAL.to_owned()),
            workspace: exec_ctx
                .workspace
                .clone()
                .unwrap_or_else(|| DEFAULT_SCOPE_WORKSPACE.to_owned()),
            execution_id: exec_ctx.execution_id.clone(),
            task_id: exec_ctx.task_id.clone(),
            agent_id: exec_ctx.agent_id.clone(),
        }
    }
}

/// The per-run choice card: one option per accepted browser plus "Default".
fn choice_request(
    task: &str,
    browsers: &[BridgeBrowser],
    timeout_secs: u64,
    scope: &Scope,
) -> UserRequest {
    let mut options: Vec<RequestOption> = browsers
        .iter()
        .filter(|browser| browser.state == "accepted")
        .map(|browser| RequestOption {
            id: format!("{BROWSER_OPTION_PREFIX}{}", browser.fingerprint),
            label: format!("{} ({})", browser.label, browser.fingerprint),
            requires_input: false,
        })
        .collect();
    options.push(RequestOption {
        id: DEFAULT_BROWSER_OPTION.to_owned(),
        label: "Default".to_owned(),
        requires_input: false,
    });
    UserRequest {
        id: String::new(),
        request_type: "browser_choice".to_owned(),
        question: format!("Which browser should Magician use for \u{201c}{task}\u{201d}?"),
        options,
        principal: scope.principal.clone(),
        workspace: scope.workspace.clone(),
        context: json!({
            "kind": "browser_choice",
            "task": task,
            "browsers": browsers
                .iter()
                .map(|browser| json!({ "fingerprint": browser.fingerprint, "label": browser.label }))
                .collect::<Vec<_>>(),
            "owner_agent_id": scope.agent_id.clone(),
        }),
        source: "browser_choice".to_owned(),
        execution_id: scope.execution_id.clone(),
        task_id: scope.task_id.clone(),
        timeout_secs,
        default_on_timeout: DEFAULT_BROWSER_OPTION.to_owned(),
        created_at: 0,
        sensitive: None,
    }
}

/// The pairing card for a browser Magicutor holds as pending.
fn pairing_request(browser: &BridgeBrowser, scope: &Scope) -> UserRequest {
    UserRequest {
        id: String::new(),
        request_type: "browser_pairing".to_owned(),
        question: format!(
            "A new browser wants to connect: {} ({}) - Accept?",
            browser.label, browser.fingerprint
        ),
        options: vec![
            RequestOption {
                id: PAIRING_ACCEPT_OPTION.to_owned(),
                label: "Accept".to_owned(),
                requires_input: false,
            },
            RequestOption {
                id: PAIRING_DECLINE_OPTION.to_owned(),
                label: "Not now".to_owned(),
                requires_input: false,
            },
        ],
        principal: scope.principal.clone(),
        workspace: scope.workspace.clone(),
        context: json!({
            "kind": "browser_pairing",
            "fingerprint": browser.fingerprint,
            "label": browser.label,
        }),
        source: "browser_pairing".to_owned(),
        execution_id: scope.execution_id.clone(),
        task_id: scope.task_id.clone(),
        // The browser keeps waiting in its own panel too; this card is a
        // convenience, so an unanswered one simply lapses.
        timeout_secs: 1800,
        default_on_timeout: PAIRING_DECLINE_OPTION.to_owned(),
        created_at: 0,
        sensitive: None,
    }
}

/// The Magicutor selector a choice-card decision stands for.
pub fn decision_selector(decision: &str) -> Option<String> {
    if decision == DEFAULT_BROWSER_OPTION {
        return Some(DEFAULT_BROWSER_OPTION.to_owned());
    }
    decision
        .strip_prefix(BROWSER_OPTION_PREFIX)
        .filter(|fingerprint| !fingerprint.is_empty())
        .map(str::to_owned)
}

#[derive(Clone)]
struct BridgeClient {
    http: reqwest::Client,
    base: Url,
    token: Option<String>,
}

impl BridgeClient {
    fn from_cdp_url(cdp_url: &str) -> Option<Self> {
        let base = http_base_from_cdp_url(cdp_url)?;
        let http = reqwest::Client::builder()
            .timeout(MAGICUTOR_HTTP_TIMEOUT)
            .build()
            .ok()?;
        let token = std::env::var(ENV_MAGICUTOR_BRIDGE_TOKEN)
            .ok()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        Some(Self { http, base, token })
    }

    fn url(&self, path: &str) -> Option<Url> {
        self.base.join(path).ok()
    }

    async fn thread_info(&self, thread_id: &str) -> Option<ThreadBrowserInfo> {
        let url = self.url(&format!(
            "cdp/threads/{}/browser",
            urlencoding::encode(thread_id)
        ))?;
        let response = self.http.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json::<ThreadBrowserInfo>().await.ok()
    }

    async fn pin(&self, thread_id: &str, selector: &str) -> bool {
        let Some(url) = self.url(&format!(
            "cdp/threads/{}/browser",
            urlencoding::encode(thread_id)
        )) else {
            return false;
        };
        match self
            .http
            .put(url)
            .json(&json!({ "browserId": selector }))
            .send()
            .await
        {
            Ok(response) => response.status().is_success(),
            Err(error) => {
                warn!(thread_id, %error, "browser choice: pinning the thread failed");
                false
            },
        }
    }

    async fn accept(&self, fingerprint: &str) -> bool {
        let Some(url) = self.url(&format!(
            "bridge/browsers/{}/accept",
            urlencoding::encode(fingerprint)
        )) else {
            return false;
        };
        let mut request = self.http.post(url);
        if let Some(token) = self.token.as_deref() {
            request = request.bearer_auth(token);
        }
        match request.send().await {
            Ok(response) => response.status().is_success(),
            Err(error) => {
                warn!(fingerprint, %error, "browser pairing: accepting the browser failed");
                false
            },
        }
    }

    /// Ask Magicutor to show the choice cards without waiting. True when a
    /// choice is now open (`asked`, or `open` from an earlier call).
    async fn start_choice(&self, thread_id: &str, task: &str, timeout_secs: u64) -> bool {
        let Some(url) = self.url(&format!(
            "cdp/threads/{}/choose",
            urlencoding::encode(thread_id)
        )) else {
            return false;
        };
        let response = match self
            .http
            .post(url)
            .json(&json!({ "task": task, "timeout_secs": timeout_secs }))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                warn!(thread_id, status = %response.status(), "browser choice: Magicutor refused to ask");
                return false;
            },
            Err(error) => {
                warn!(thread_id, %error, "browser choice: Magicutor unreachable");
                return false;
            },
        };
        let status = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| {
                body.get("status")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned)
            });
        matches!(status.as_deref(), Some("asked" | "open"))
    }

    async fn poll_pin(&self, thread_id: &str, stale_pin: Option<&str>) -> PinPoll {
        match self.thread_info(thread_id).await {
            Some(info) if pin_settled(&info, stale_pin) => PinPoll::Pinned,
            Some(_) => PinPoll::Waiting,
            None => PinPoll::Unavailable,
        }
    }
}

/// Whether the owner's choice has landed. Magicutor keeps reporting a stale
/// pin to a browser that is gone while it asks again, so a pin counts only
/// when it names an accepted, connected browser, or differs from the pin
/// seen before the choice started.
pub fn pin_settled(info: &ThreadBrowserInfo, stale_pin: Option<&str>) -> bool {
    let Some(pinned) = info.pinned.as_deref() else {
        return false;
    };
    let to_connected = info
        .browsers
        .iter()
        .any(|browser| browser.fingerprint == pinned && browser.state == "accepted");
    to_connected || stale_pin != Some(pinned)
}

/// One look at whether the thread has its browser yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPoll {
    Pinned,
    Waiting,
    /// Magicutor could not be asked.
    Unavailable,
}

/// How waiting for the owner's browser choice ended. Every outcome proceeds
/// with the connect; only `Pinned` means the owner (or Magicutor's timeout)
/// already settled the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinWait {
    Pinned,
    TimedOut,
    MagicutorUnavailable,
    Cancelled,
}

/// Poll until the thread is pinned, `cap` passes, Magicutor stops answering,
/// or the run is cancelled. Never waits longer than `cap` plus one poll.
pub async fn wait_for_pin<P, Fut>(
    mut poll: P,
    cap: Duration,
    interval: Duration,
    cancel: Option<CancellationToken>,
) -> PinWait
where
    P: FnMut() -> Fut,
    Fut: std::future::Future<Output = PinPoll>,
{
    let deadline = tokio::time::Instant::now() + cap;
    let mut failures = 0u32;
    loop {
        if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
            return PinWait::Cancelled;
        }
        match poll().await {
            PinPoll::Pinned => return PinWait::Pinned,
            PinPoll::Waiting => failures = 0,
            PinPoll::Unavailable => {
                failures += 1;
                if failures >= MAX_POLL_FAILURES {
                    return PinWait::MagicutorUnavailable;
                }
            },
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return PinWait::TimedOut;
        }
        let pause = interval.min(deadline - now);
        match cancel.as_ref() {
            Some(token) => {
                tokio::select! {
                    _ = token.cancelled() => return PinWait::Cancelled,
                    _ = tokio::time::sleep(pause) => {},
                }
            },
            None => tokio::time::sleep(pause).await,
        }
    }
}

/// The browser each attended run settled on, by run (execution, else task).
/// A run can open more than one CDP thread (its thread id changes across a
/// pause and resume), and the owner chooses once per run, not per thread.
fn run_choices() -> &'static Mutex<HashMap<String, String>> {
    static CHOICES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CHOICES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remembered runs kept in memory; past this the oldest-known set is dropped.
const MAX_REMEMBERED_RUN_CHOICES: usize = 512;

fn run_choice_key(scope: &Scope) -> Option<String> {
    let run = scope.execution_id.as_ref().or(scope.task_id.as_ref())?;
    Some(format!("{}/{}/{run}", scope.principal, scope.workspace))
}

/// The browser this run already uses, if it is still connected and accepted.
fn remembered_run_browser(scope: &Scope, browsers: &[BridgeBrowser]) -> Option<String> {
    let key = run_choice_key(scope)?;
    let fingerprint = run_choices().lock().ok()?.get(&key).cloned()?;
    browsers
        .iter()
        .any(|browser| browser.fingerprint == fingerprint && browser.state == "accepted")
        .then_some(fingerprint)
}

fn remember_run_browser(scope: &Scope, fingerprint: String) {
    let Some(key) = run_choice_key(scope) else {
        return;
    };
    if let Ok(mut choices) = run_choices().lock() {
        if choices.len() >= MAX_REMEMBERED_RUN_CHOICES && !choices.contains_key(&key) {
            choices.clear();
        }
        choices.insert(key, fingerprint);
    }
}

/// Fingerprints this process already raised a pairing card for.
fn asked_pairing() -> &'static Mutex<HashSet<String>> {
    static ASKED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ASKED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Raise a pairing card for each pending browser not asked about yet.
fn raise_pairing_cards(
    service: &Arc<UserRequestService>,
    client: &BridgeClient,
    browsers: &[BridgeBrowser],
    scope: &Scope,
) {
    for browser in browsers.iter().filter(|browser| browser.state == "pending") {
        let first_time = asked_pairing()
            .lock()
            .map(|mut asked| asked.insert(browser.fingerprint.clone()))
            .unwrap_or(false);
        if !first_time {
            continue;
        }
        let request = pairing_request(browser, scope);
        let service = Arc::clone(service);
        let client = client.clone();
        let fingerprint = browser.fingerprint.clone();
        tokio::spawn(async move {
            let response = service.ask(request).await;
            if response.decision == PAIRING_ACCEPT_OPTION {
                let accepted = client.accept(&fingerprint).await;
                info!(fingerprint = %fingerprint, accepted, "browser pairing answered from Magician");
            } else if let Ok(mut asked) = asked_pairing().lock() {
                // Ask again on a later run.
                asked.remove(&fingerprint);
            }
        });
    }
}

/// The phone/web choice card, raised while Magician waits for the pin.
struct ChoiceCard {
    service: Arc<UserRequestService>,
    request_id: Arc<Mutex<Option<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ChoiceCard {
    /// Raise the card on its own task: an owner answer pins the thread (a
    /// pin that already exists is kept by Magicutor, so a late answer changes
    /// nothing); a timeout or a settled answer does nothing.
    fn raise(
        service: Arc<UserRequestService>,
        client: BridgeClient,
        thread_id: String,
        request: UserRequest,
    ) -> Self {
        let request_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let id_slot = Arc::clone(&request_id);
        let asker = Arc::clone(&service);
        // `ask_with_acceptance_hook`'s future is `Send` (the service's own
        // tests spawn `ask`), and the hook only captures an `Arc<Mutex<_>>`.
        let task = tokio::spawn(async move {
            let response = asker
                .ask_with_acceptance_hook(request, move |accepted| {
                    if let Ok(mut slot) = id_slot.lock() {
                        *slot = Some(accepted.id.clone());
                    }
                })
                .await;
            if response.channel == "timeout" || response.channel == CHOICE_SETTLED_CHANNEL {
                return;
            }
            if let Some(selector) = decision_selector(&response.decision) {
                let pinned = client.pin(&thread_id, &selector).await;
                info!(thread_id = %thread_id, selector = %selector, pinned, "browser chosen from Magician");
            }
        });
        Self {
            service,
            request_id,
            task,
        }
    }

    /// The browser is settled (or Magician stopped waiting): resolve the card
    /// if nobody answered it, so it does not linger. Bounded to ~2 s.
    async fn settle(mut self) {
        if self.task.is_finished() {
            return;
        }
        let id = self.request_id.lock().ok().and_then(|slot| slot.clone());
        if let Some(request_id) = id {
            let _ = self
                .service
                .respond(UserResponse {
                    request_id,
                    decision: DEFAULT_BROWSER_OPTION.to_owned(),
                    input: None,
                    channel: CHOICE_SETTLED_CHANNEL.to_owned(),
                    sensitive: Vec::new(),
                })
                .await;
        }
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }
}

/// Before a flat browser session's first `connect`: raise pairing cards for
/// pending browsers and, for an attended run Magicutor would ask about, have
/// the owner choose the browser — cards in every accepted browser plus a
/// decision card here — and wait for the pin. The session then connects
/// normally. Does nothing for headed/headless sessions, connected sessions,
/// or sessions already naming a browser. Never fails the run: on any
/// Magicutor error it returns and the connect uses the default browser. Waits
/// at most the choice timeout plus [`CHOICE_WAIT_MARGIN`] (and its own short
/// HTTP calls).
pub async fn prepare_browser_choice(
    exec_ctx: &PrimitiveExecCtx,
    session: &AgentBrowserSession,
    task: Option<&str>,
) {
    let ConnectionMode::Cdp { url } = session.mode() else {
        return;
    };
    if session.is_connected() {
        return;
    }
    let Some(thread_id) = cdp_thread_from_url(url) else {
        return;
    };
    let Some(client) = BridgeClient::from_cdp_url(url) else {
        return;
    };
    let Some(info) = client.thread_info(&thread_id).await else {
        return;
    };
    let scope = Scope::of(exec_ctx);
    if let Some(service) = exec_ctx.user_request_service.as_ref() {
        raise_pairing_cards(service, &client, &info.browsers, &scope);
    }
    if !info.would_ask || !exec_ctx_is_attended(exec_ctx) {
        return;
    }
    // This run already chose (on another of its CDP threads): use that
    // browser again rather than asking twice.
    if let Some(fingerprint) = remembered_run_browser(&scope, &info.browsers) {
        if client.pin(&thread_id, &fingerprint).await {
            info!(thread_id = %thread_id, browser = %fingerprint, "browser reused from this run's earlier choice");
            return;
        }
    }
    let title = task_title(task, None);
    let timeout_secs = info
        .choice_timeout_secs
        .unwrap_or(FALLBACK_CHOICE_TIMEOUT_SECS)
        .clamp(1, MAX_CHOICE_TIMEOUT_SECS);
    if !client.start_choice(&thread_id, &title, timeout_secs).await {
        return;
    }
    let card = exec_ctx.user_request_service.clone().map(|service| {
        let request = choice_request(&title, &info.browsers, timeout_secs, &scope);
        ChoiceCard::raise(service, client.clone(), thread_id.clone(), request)
    });
    let stale_pin = info.pinned.clone();
    let outcome = wait_for_pin(
        || client.poll_pin(&thread_id, stale_pin.as_deref()),
        Duration::from_secs(timeout_secs) + CHOICE_WAIT_MARGIN,
        PIN_POLL_INTERVAL,
        exec_ctx.cancellation_token.clone(),
    )
    .await;
    if let Some(card) = card {
        card.settle().await;
    }
    // Whatever the thread ended pinned to (the owner's pick or the default
    // after the timeout) is this run's browser from now on.
    if let Some(pinned) = client
        .thread_info(&thread_id)
        .await
        .and_then(|after| after.pinned)
    {
        remember_run_browser(&scope, pinned);
    }
    debug!(thread_id = %thread_id, ?outcome, "browser choice wait finished");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(
        surface: InvocationSurface,
        source_kind: InvocationSourceKind,
        chat: Option<&str>,
    ) -> AgentInvocationContext {
        AgentInvocationContext {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            source_agent_id: None,
            target_agent_id: "assistant".to_string(),
            surface,
            feature_mode: Default::default(),
            source_kind,
            chat_session_id: chat.map(str::to_string),
            chat_turn_id: None,
        }
    }

    fn scope() -> Scope {
        Scope {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            execution_id: Some("exec-1".to_string()),
            task_id: Some("task-1".to_string()),
            agent_id: None,
        }
    }

    fn browser(fingerprint: &str, label: &str, state: &str) -> BridgeBrowser {
        BridgeBrowser {
            fingerprint: fingerprint.to_string(),
            label: label.to_string(),
            state: state.to_string(),
            default: false,
        }
    }

    #[test]
    fn chat_and_device_runs_are_attended() {
        assert!(run_is_attended(Some("chat-1"), None));
        let chat = invocation(
            InvocationSurface::Chat,
            InvocationSourceKind::ChatInline,
            Some("chat-1"),
        );
        assert!(run_is_attended(None, Some(&chat)));
        let voice = invocation(
            InvocationSurface::RealtimeVoice,
            InvocationSourceKind::Direct,
            None,
        );
        assert!(run_is_attended(None, Some(&voice)));
        let delegated_with_chat = invocation(
            InvocationSurface::Delegation,
            InvocationSourceKind::Delegated,
            Some("chat-1"),
        );
        assert!(run_is_attended(None, Some(&delegated_with_chat)));
    }

    #[test]
    fn scheduled_autonomous_and_harness_runs_never_ask() {
        assert!(!run_is_attended(None, None));
        assert!(!run_is_attended(Some("  "), None));
        let autonomous = invocation(
            InvocationSurface::Task,
            InvocationSourceKind::Autonomous,
            Some("chat-1"),
        );
        assert!(!run_is_attended(Some("chat-1"), Some(&autonomous)));
        let delegated = invocation(
            InvocationSurface::Delegation,
            InvocationSourceKind::Delegated,
            None,
        );
        assert!(!run_is_attended(None, Some(&delegated)));
        let task = invocation(InvocationSurface::Task, InvocationSourceKind::Direct, None);
        assert!(!run_is_attended(None, Some(&task)));
        let plane = invocation(
            InvocationSurface::Plane,
            InvocationSourceKind::Direct,
            Some("chat-1"),
        );
        assert!(!run_is_attended(None, Some(&plane)));
        let public = invocation(
            InvocationSurface::Chat,
            InvocationSourceKind::Public,
            Some("chat-1"),
        );
        assert!(!run_is_attended(None, Some(&public)));
    }

    fn counting_poll(
        answers: Vec<PinPoll>,
    ) -> (
        Arc<std::sync::atomic::AtomicUsize>,
        impl FnMut() -> std::future::Ready<PinPoll>,
    ) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let poll = move || {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::ready(
                *answers
                    .get(n)
                    .or(answers.last())
                    .unwrap_or(&PinPoll::Waiting),
            )
        };
        (calls, poll)
    }

    #[test]
    fn a_stale_pin_to_a_departed_browser_is_not_a_choice() {
        let mut info = ThreadBrowserInfo {
            pinned: Some("aaaaaaaaaaaaaaaa".to_string()),
            would_ask: true,
            browsers: vec![browser("bbbbbbbbbbbbbbbb", "Comet", "accepted")],
            choice_timeout_secs: Some(20),
        };
        // The pin seen before the choice, to a browser no longer connected.
        assert!(!pin_settled(&info, Some("aaaaaaaaaaaaaaaa")));
        // A pin to an accepted, connected browser has landed.
        info.pinned = Some("bbbbbbbbbbbbbbbb".to_string());
        assert!(pin_settled(&info, Some("aaaaaaaaaaaaaaaa")));
        // A new pin counts even before the browser list catches up.
        info.pinned = Some("cccccccccccccccc".to_string());
        assert!(pin_settled(&info, Some("aaaaaaaaaaaaaaaa")));
        // A pending browser does not count as the stale pin's replacement.
        info.browsers = vec![browser("aaaaaaaaaaaaaaaa", "Dia", "pending")];
        info.pinned = Some("aaaaaaaaaaaaaaaa".to_string());
        assert!(!pin_settled(&info, Some("aaaaaaaaaaaaaaaa")));
        info.pinned = None;
        assert!(!pin_settled(&info, None));
    }

    #[tokio::test]
    async fn the_run_waits_until_the_thread_is_pinned() {
        let (calls, poll) =
            counting_poll(vec![PinPoll::Waiting, PinPoll::Waiting, PinPoll::Pinned]);
        let outcome =
            wait_for_pin(poll, Duration::from_secs(5), Duration::from_millis(5), None).await;
        assert_eq!(outcome, PinWait::Pinned);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn the_run_proceeds_when_nobody_chooses_in_time() {
        let (_calls, poll) = counting_poll(vec![PinPoll::Waiting]);
        let started = std::time::Instant::now();
        let outcome = wait_for_pin(
            poll,
            Duration::from_millis(60),
            Duration::from_millis(10),
            None,
        )
        .await;
        assert_eq!(outcome, PinWait::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn the_run_proceeds_when_magicutor_stops_answering() {
        let (calls, poll) = counting_poll(vec![PinPoll::Unavailable]);
        let outcome = wait_for_pin(
            poll,
            Duration::from_secs(30),
            Duration::from_millis(5),
            None,
        )
        .await;
        assert_eq!(outcome, PinWait::MagicutorUnavailable);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            MAX_POLL_FAILURES as usize
        );
        // One failed poll between answers is tolerated.
        let (_calls, poll) = counting_poll(vec![
            PinPoll::Unavailable,
            PinPoll::Waiting,
            PinPoll::Pinned,
        ]);
        let outcome = wait_for_pin(
            poll,
            Duration::from_secs(30),
            Duration::from_millis(5),
            None,
        )
        .await;
        assert_eq!(outcome, PinWait::Pinned);
    }

    #[tokio::test]
    async fn a_cancelled_run_stops_waiting_at_once() {
        let token = CancellationToken::new();
        let (_calls, poll) = counting_poll(vec![PinPoll::Waiting]);
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });
        let started = std::time::Instant::now();
        let outcome = wait_for_pin(
            poll,
            Duration::from_secs(30),
            Duration::from_secs(10),
            Some(token),
        )
        .await;
        assert_eq!(outcome, PinWait::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn only_thread_scoped_proxy_urls_without_a_browser_are_choosable() {
        assert_eq!(
            cdp_thread_from_url("ws://127.0.0.1:3003/devtools/browser/magician-t1"),
            Some("magician-t1".to_string())
        );
        assert_eq!(
            cdp_thread_from_url("ws://127.0.0.1:3003/devtools/browser/magicutor-proxy"),
            None
        );
        assert_eq!(
            cdp_thread_from_url("ws://127.0.0.1:3003/devtools/browser/magician-t1?browser=comet"),
            None
        );
        assert_eq!(
            cdp_thread_from_url("ws://127.0.0.1:3003/devtools/page/7"),
            None
        );
        assert_eq!(cdp_thread_from_url("not a url"), None);
    }

    #[test]
    fn the_choice_card_offers_each_accepted_browser_and_default() {
        let browsers = vec![
            browser("a1b2c3d4", "Chrome", "accepted"),
            browser("e5f6a7b8", "Comet", "accepted"),
            browser("99990000", "Dia", "pending"),
        ];
        let request = choice_request("Book a table", &browsers, 20, &scope());
        let ids: Vec<&str> = request
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        assert_eq!(ids, vec!["browser:a1b2c3d4", "browser:e5f6a7b8", "default"]);
        assert_eq!(request.options[1].label, "Comet (e5f6a7b8)");
        assert_eq!(request.default_on_timeout, "default");
        assert_eq!(request.timeout_secs, 20);
        assert!(request.question.contains("Book a table"));
        assert_eq!(
            decision_selector("browser:e5f6a7b8").as_deref(),
            Some("e5f6a7b8")
        );
        assert_eq!(decision_selector("default").as_deref(), Some("default"));
        assert_eq!(decision_selector("browser:"), None);
        assert_eq!(decision_selector("deny"), None);
    }

    #[test]
    fn the_pairing_card_names_the_browser_and_defaults_to_not_now() {
        let request = pairing_request(&browser("a1b2c3d4", "Comet", "pending"), &scope());
        assert_eq!(
            request.question,
            "A new browser wants to connect: Comet (a1b2c3d4) - Accept?"
        );
        let ids: Vec<&str> = request
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        assert_eq!(ids, vec!["accept", "not_now"]);
        assert_eq!(request.default_on_timeout, "not_now");
    }

    #[test]
    fn a_run_reuses_its_choice_while_that_browser_stays_accepted() {
        let run = Scope {
            execution_id: Some("exec-reuse".to_string()),
            ..scope()
        };
        let browsers = vec![
            browser("aaaa", "Chrome", "accepted"),
            browser("bbbb", "Opera", "accepted"),
        ];
        assert_eq!(remembered_run_browser(&run, &browsers), None);
        remember_run_browser(&run, "bbbb".to_string());
        assert_eq!(
            remembered_run_browser(&run, &browsers),
            Some("bbbb".to_string())
        );
        // Another run asks for itself.
        let other = Scope {
            execution_id: Some("exec-other".to_string()),
            ..scope()
        };
        assert_eq!(remembered_run_browser(&other, &browsers), None);
        // A browser that left or was revoked is not reused.
        let revoked = vec![
            browser("aaaa", "Chrome", "accepted"),
            browser("bbbb", "Opera", "pending"),
        ];
        assert_eq!(remembered_run_browser(&run, &revoked), None);
    }

    #[test]
    fn task_titles_prefer_the_caller_then_the_page_host() {
        assert_eq!(task_title(Some(" Book dinner "), None), "Book dinner");
        assert_eq!(
            task_title(None, Some("https://www.opentable.com/r/x")),
            "browse www.opentable.com"
        );
        assert_eq!(task_title(None, None), "this task");
        assert!(task_title(Some(&"z".repeat(300)), None).chars().count() <= 81);
    }

    #[test]
    fn thread_info_parses_magicutor_payloads_leniently() {
        let info: ThreadBrowserInfo = serde_json::from_value(json!({
            "thread_id": "magician-t1",
            "pinned": null,
            "would_ask": true,
            "browsers": [
                { "fingerprint": "a1b2c3d4", "label": "Chrome", "state": "accepted", "default": true,
                  "slot": 1, "connected_at": "2026-10-23T00:00:00Z", "pinned_threads": 0, "this_browser": false }
            ],
            "choice_timeout_secs": 20
        }))
        .unwrap();
        assert!(info.would_ask);
        assert_eq!(info.browsers[0].label, "Chrome");
        assert_eq!(info.choice_timeout_secs, Some(20));
        let empty: ThreadBrowserInfo = serde_json::from_value(json!({})).unwrap();
        assert!(!empty.would_ask && empty.browsers.is_empty());
    }
}
