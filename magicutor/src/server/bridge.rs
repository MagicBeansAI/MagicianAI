use actix::{Actor, ActorContext, Addr, AsyncContext, Handler, Message, StreamHandler};
use actix_http::ws::Item as WsItem;
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_web_actors::ws;
use bytes::BytesMut;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::{DefaultHasher, RandomState};
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, Hasher};
use std::net::IpAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, trace, warn};

use crate::bridge_protocol::{ExtensionRequest, ExtensionResponse};
use crate::server::browser_store::BrowserStore;
use crate::server::page_signals;
use crate::types::{ExecutionError, Result};

/// How many browsers (profiles, Chromium forks) may hold the bridge at the
/// same time when `bridge.max_connections` is not configured.
pub const MAX_BRIDGE_CONNECTIONS: usize = 4;

/// Id prefix for an extension build that predates per-install ids. Each such
/// connection gets its own id: without an id and key there is no proof two
/// sockets are the same browser, so legacy connections never replace one
/// another, are never pinned, and inherit nothing from an earlier one.
const LEGACY_BROWSER_ID: &str = "legacy-browser";
const MAX_BROWSER_ID_LEN: usize = 64;
const MIN_BROWSER_KEY_LEN: usize = 16;
const MAX_BROWSER_KEY_LEN: usize = 128;
const MAX_BROWSER_LABEL_CHARS: usize = 48;
const DEFAULT_BROWSER_LABEL: &str = "Browser";
/// A same-browser reconnect may replace the existing socket only after that
/// socket has been silent this long. A live socket answers the 15 s server
/// pings, so it is never quiet this long; a half-open one is. This also stops
/// two copies of one profile directory (same id and key) evicting each other
/// in a loop.
const REPLACE_QUIET_PERIOD: Duration = Duration::from_secs(20);

type PendingSender = oneshot::Sender<std::result::Result<ExtensionResponse, String>>;

/// Per-process key for hashing browser keys. Only digests are kept, and
/// they never leave the process.
static BROWSER_KEY_HASHER: Lazy<RandomState> = Lazy::new(RandomState::new);

fn browser_key_digest(key: &str) -> u64 {
    let mut hasher = BROWSER_KEY_HASHER.build_hasher();
    key.hash(&mut hasher);
    hasher.finish()
}

/// Short, non-secret name for a browser id, shown in `/health` and error
/// bodies instead of the id. Selectors accept it as well as the full id.
pub fn browser_fingerprint(browser_id: &str) -> String {
    let mut hasher = DefaultHasher::new();
    browser_id.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn is_legacy_browser(browser_id: &str) -> bool {
    browser_id.starts_with(LEGACY_BROWSER_ID)
}

fn idle_for(last_traffic: &Mutex<Instant>) -> Duration {
    match last_traffic.lock() {
        Ok(last) => last.elapsed(),
        Err(poisoned) => poisoned.into_inner().elapsed(),
    }
}

fn mark_traffic(last_traffic: &Mutex<Instant>) {
    match last_traffic.lock() {
        Ok(mut last) => *last = Instant::now(),
        Err(poisoned) => *poisoned.into_inner() = Instant::now(),
    }
}

/// Open thread-scoped CDP sessions per thread, maintained by the cdp_proxy
/// session actors. A thread with an open session is never moved to another
/// browser; one without may be re-pinned once its browser is gone.
static THREAD_LIVE_SESSIONS: Lazy<Mutex<HashMap<String, usize>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Record a thread-scoped CDP session opening (`true`) or closing (`false`).
pub fn note_thread_session(thread_id: &str, opened: bool) {
    let mut map = match THREAD_LIVE_SESSIONS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if opened {
        *map.entry(thread_id.to_string()).or_insert(0) += 1;
    } else if let Some(count) = map.get_mut(thread_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            map.remove(thread_id);
        }
    }
}

/// Whether any thread-scoped CDP session is open on `thread_id`.
pub fn thread_has_live_session(thread_id: &str) -> bool {
    THREAD_LIVE_SESSIONS
        .lock()
        .map(|map| map.contains_key(thread_id))
        .unwrap_or(false)
}

/// Whether more than one thread-scoped CDP session is open on `thread_id`:
/// asked by a session about the others, which share the thread's tabs and
/// their one `chrome.debugger` attachment each.
pub fn thread_has_other_live_session(thread_id: &str) -> bool {
    THREAD_LIVE_SESSIONS
        .lock()
        .map(|map| map.get(thread_id).is_some_and(|count| *count > 1))
        .unwrap_or(false)
}

/// One browser connected over the bridge: an extension install in one
/// profile. Tab ids, in-flight requests and CDP events belong to it alone.
struct BridgeConnection {
    session_id: u64,
    browser_id: String,
    fingerprint: String,
    label: String,
    /// Digest of the install's secret `browser_key`; `None` for builds that
    /// send no key, whose connections can never be replaced.
    key_digest: Option<u64>,
    /// Owner accepted this browser (pairing), or it was accepted
    /// automatically (upgrade window, first-run bootstrap, or a legacy build
    /// during the upgrade window). A pending browser holds a slot but gets no
    /// work: never the default, never selectable, never pinned. Change it
    /// only through [`BridgeConnection::set_accepted`].
    accepted: bool,
    /// Mirror of `accepted` read by the session actor, which drops every
    /// frame but consent, label and ping/pong while it is false.
    routable_flag: Arc<AtomicBool>,
    /// Restart-stable hash of the browser key (`BrowserStore::key_hash`),
    /// kept so an accept from the panel can be persisted.
    stable_key_hash: Option<String>,
    connected_at: DateTime<Utc>,
    /// Last inbound frame on this socket, shared with its session actor.
    last_traffic: Arc<Mutex<Instant>>,
    tx: mpsc::UnboundedSender<BridgeOutbound>,
    /// Set when the session actor starts. Used to close a stale socket that
    /// a reconnect of the same browser replaces.
    addr: Option<Addr<BridgeSession>>,
    /// Requests sent over this connection. Only this connection's responses
    /// can resolve them and only its disconnect can fail them.
    pending: HashMap<String, PendingSender>,
}

impl BridgeConnection {
    fn describe(&self) -> String {
        format!("{} [{}]", self.label, self.fingerprint)
    }

    /// Whether work may be routed here.
    fn routable(&self) -> bool {
        self.accepted
    }

    fn set_accepted(&mut self, accepted: bool) {
        self.accepted = accepted;
        self.routable_flag.store(accepted, Ordering::SeqCst);
    }

    /// Whether this browser can show pairing-era cards (an accepted,
    /// identified build). Legacy builds predate cards.
    fn shows_cards(&self) -> bool {
        self.accepted && !is_legacy_browser(&self.browser_id)
    }

    fn state(&self) -> &'static str {
        if !self.accepted {
            "pending"
        } else if is_legacy_browser(&self.browser_id) {
            "legacy"
        } else if self.accepted {
            "accepted"
        } else {
            "pending"
        }
    }

    /// Queue a server notice for this browser's extension.
    fn notify(&self, envelope: BridgeEnvelope) {
        let _ = self.tx.send(BridgeOutbound::Envelope(envelope));
    }

    fn notify_state(&self) {
        self.notify(BridgeEnvelope::BrowserState {
            state: self.state().to_string(),
            fingerprint: self.fingerprint.clone(),
        });
    }

    /// Fail this connection's in-flight requests and close its socket.
    fn retire(self, reason: &str) {
        for (_, sender) in self.pending {
            let _ = sender.send(Err(reason.to_string()));
        }
        if let Some(addr) = self.addr {
            addr.do_send(CloseBridgeSession);
        }
    }
}

/// The browser a CDP thread runs in. Kept across that browser's disconnects
/// so the thread re-binds when the same browser reconnects instead of moving.
#[derive(Debug, Clone)]
struct ThreadPin {
    browser_id: String,
    fingerprint: String,
    label: String,
}

impl ThreadPin {
    fn for_connection(connection: &BridgeConnection) -> Self {
        Self {
            browser_id: connection.browser_id.clone(),
            fingerprint: connection.fingerprint.clone(),
            label: connection.label.clone(),
        }
    }

    fn describe(&self) -> String {
        format!("{} [{}]", self.label, self.fingerprint)
    }

    /// Whether a selector names exactly this pin's browser: its id, its
    /// fingerprint, or its whole label (case-insensitive). A prefix is not
    /// enough — it could name another browser just as well.
    fn matches_exactly(&self, selector: &str) -> bool {
        let selector = selector.trim();
        self.browser_id == selector
            || self.fingerprint == selector
            || self.label.to_lowercase() == selector.to_lowercase()
    }
}

/// Up to `bridge.max_connections` browsers share the bridge. Admission is
/// decided before WebSocket OPEN under this hub's lock, so concurrent
/// handshakes cannot overfill it; no browser can evict another.
#[derive(Default)]
struct BridgeHub {
    /// Admission order, oldest first. The default browser is the "Always"
    /// browser when connected, else the first accepted one.
    connections: Vec<BridgeConnection>,
    /// thread id -> the browser its CDP work goes to.
    thread_pins: HashMap<String, ThreadPin>,
    /// Accepted browsers and the "Always use this browser" choice.
    store: BrowserStore,
    /// Open per-run browser choices, by choice id.
    choices: HashMap<String, PendingChoice>,
}

/// A run waiting for the owner to pick its browser.
struct PendingChoice {
    thread_id: String,
    /// Browsers shown the card.
    recipients: Vec<String>,
    /// Taken by the first answer; later answers are no-ops.
    outcome: Option<oneshot::Sender<ChoiceOutcome>>,
}

#[derive(Debug)]
enum ChoiceOutcome {
    /// Picked in a browser's card; the thread is already pinned there.
    Browser { browser_id: String },
    /// Settled elsewhere (Magician's card, `PUT /cdp/threads/{id}/browser`).
    Owner,
}

/// How a per-run browser choice ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserChoiceOutcome {
    /// Nothing to ask: one accepted browser, a pinned thread, or a choice
    /// already open for the thread.
    NotAsked,
    /// The owner picked this browser in its card.
    ChosenInBrowser(String),
    /// The owner answered elsewhere; the thread was pinned by that answer.
    ChosenByOwner,
    /// Nobody answered in time; the thread went to the default browser.
    TimedOut,
}

impl BridgeHub {
    fn with_store(store: BrowserStore) -> Self {
        Self {
            store,
            ..Self::default()
        }
    }
}

#[cfg(test)]
fn process_browser_store() -> BrowserStore {
    BrowserStore::default()
}

#[cfg(not(test))]
fn process_browser_store() -> BrowserStore {
    BrowserStore::load(BrowserStore::default_path())
}

static HUB: Lazy<Arc<RwLock<BridgeHub>>> =
    Lazy::new(|| Arc::new(RwLock::new(BridgeHub::with_store(process_browser_store()))));
static NEXT_BRIDGE_SESSION_ID: AtomicU64 = AtomicU64::new(1);
const BRIDGE_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Which connected browser a bridge request goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeRoute {
    /// The default browser: the one holding slot 1 (the longest-held slot).
    Default,
    /// One browser by its install id.
    Browser(String),
    /// The browser a CDP thread is pinned to. An unpinned thread is pinned
    /// to the default browser by its first request (never to a legacy
    /// browser, whose threads follow the default as before).
    Thread(String),
}

/// Why a browser selector or thread pin could not be honoured. Browsers are
/// named by label and fingerprint, never by install id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserSelectError {
    NoBrowsers,
    NotFound {
        selector: String,
        connected: Vec<String>,
    },
    Ambiguous {
        selector: String,
        matches: Vec<String>,
    },
    PinnedElsewhere {
        thread_id: String,
        pinned: String,
    },
}

impl BrowserSelectError {
    /// Stable machine-readable code for HTTP error bodies.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoBrowsers => "bridge_not_connected",
            Self::NotFound { .. } => "browser_not_found",
            Self::Ambiguous { .. } => "browser_ambiguous",
            Self::PinnedElsewhere { .. } => "thread_pinned_to_other_browser",
        }
    }
}

impl std::fmt::Display for BrowserSelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBrowsers => write!(f, "no browser is connected to the bridge"),
            Self::NotFound {
                selector,
                connected,
            } => write!(
                f,
                "no connected browser matches \"{selector}\" (connected: {})",
                connected.join(", ")
            ),
            Self::Ambiguous { selector, matches } => write!(
                f,
                "\"{selector}\" matches several connected browsers ({}); select one by the fingerprint in brackets, or give each browser its own name in the Magican panel's Settings",
                matches.join(", ")
            ),
            Self::PinnedElsewhere { thread_id, pinned } => write!(
                f,
                "thread {thread_id} already runs in browser {pinned}; clear the thread before moving it to another browser"
            ),
        }
    }
}

impl std::error::Error for BrowserSelectError {}

/// A connected browser as reported by `/health`. The install id is kept
/// internal; `fingerprint` names the browser in public payloads.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectedBrowser {
    #[serde(skip_serializing)]
    pub id: String,
    pub fingerprint: String,
    pub label: String,
    pub connected_at: DateTime<Utc>,
    /// 1-based slot in admission order; slot 1 is the default browser.
    pub slot: usize,
    pub default: bool,
    pub pinned_threads: usize,
    /// `accepted`, `pending` (awaiting the owner's pairing decision; gets no
    /// work) or `legacy` (a build without an identity; routed as before).
    pub state: &'static str,
    /// True for the browser whose id the caller named (`/health?browser_id=`),
    /// so an extension can find its own slot without ids being listed.
    pub this_browser: bool,
}

/// A bridge response together with the browser that produced it.
#[derive(Debug, Clone)]
pub struct RoutedResponse {
    pub browser_id: String,
    pub response: ExtensionResponse,
}

impl BridgeHub {
    fn index_of_session(&self, session_id: u64) -> Option<usize> {
        self.connections
            .iter()
            .position(|c| c.session_id == session_id)
    }

    fn index_of_browser(&self, browser_id: &str) -> Option<usize> {
        self.connections
            .iter()
            .position(|c| c.browser_id == browser_id)
    }

    /// A connected browser that may receive work.
    fn routable_index_of(&self, browser_id: &str) -> Option<usize> {
        self.connections
            .iter()
            .position(|c| c.browser_id == browser_id && c.routable())
    }

    /// The "Always use this browser" choice while it is connected and
    /// accepted, else the longest-held accepted (or legacy) browser. A
    /// pending browser is never the default.
    fn default_index(&self) -> Option<usize> {
        if let Some(always) = self.store.always_browser() {
            if let Some(index) = self.routable_index_of(always) {
                return Some(index);
            }
        }
        self.connections.iter().position(BridgeConnection::routable)
    }

    /// Match a selector against connected, routable browsers: exact install
    /// id or fingerprint, then case-insensitive label, then a unique
    /// case-insensitive label prefix. Pending browsers are not selectable.
    fn select(&self, selector: &str) -> std::result::Result<usize, BrowserSelectError> {
        let selector = selector.trim();
        let candidates: Vec<usize> = (0..self.connections.len())
            .filter(|&i| self.connections[i].routable())
            .collect();
        if candidates.is_empty() {
            return Err(BrowserSelectError::NoBrowsers);
        }
        if let Some(&index) = candidates
            .iter()
            .find(|&&i| self.connections[i].browser_id == selector)
        {
            return Ok(index);
        }
        let by_fingerprint: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|&i| self.connections[i].fingerprint == selector)
            .collect();
        match by_fingerprint.len() {
            0 => {},
            1 => return Ok(by_fingerprint[0]),
            _ => {
                return Err(BrowserSelectError::Ambiguous {
                    selector: selector.to_string(),
                    matches: by_fingerprint
                        .iter()
                        .map(|&i| self.connections[i].describe())
                        .collect(),
                })
            },
        }
        let wanted = selector.to_lowercase();
        let pick = |matches: Vec<usize>| -> Option<std::result::Result<usize, BrowserSelectError>> {
            match matches.len() {
                0 => None,
                1 => Some(Ok(matches[0])),
                _ => Some(Err(BrowserSelectError::Ambiguous {
                    selector: selector.to_string(),
                    matches: matches
                        .iter()
                        .map(|&i| self.connections[i].describe())
                        .collect(),
                })),
            }
        };
        let exact: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|&i| self.connections[i].label.to_lowercase() == wanted)
            .collect();
        if let Some(result) = pick(exact) {
            return result;
        }
        if !wanted.is_empty() {
            let prefixed: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|&i| {
                    self.connections[i]
                        .label
                        .to_lowercase()
                        .starts_with(&wanted)
                })
                .collect();
            if let Some(result) = pick(prefixed) {
                return result;
            }
        }
        Err(BrowserSelectError::NotFound {
            selector: selector.to_string(),
            connected: candidates
                .iter()
                .map(|&i| self.connections[i].describe())
                .collect(),
        })
    }

    /// Pin `thread_id` to the connection at `index`, unless it is a legacy
    /// browser (no stable identity: its threads stay unpinned and follow the
    /// default browser) or a pending one (gets no work at all).
    fn pin_to(&mut self, thread_id: &str, index: usize) {
        let connection = &self.connections[index];
        if is_legacy_browser(&connection.browser_id) || !connection.routable() {
            return;
        }
        let pin = ThreadPin::for_connection(connection);
        debug!(thread_id, browser = %pin.describe(), "pinned CDP thread to a browser");
        self.thread_pins.insert(thread_id.to_string(), pin);
        // However the thread got its browser, an open choice for it is over:
        // its cards are withdrawn.
        self.release_choice_for_thread(thread_id);
    }

    /// Resolve a route to a connection. A thread whose browser is gone is
    /// refused while a CDP session is open on it — its tabs and windows exist
    /// only in that browser — and re-pinned to the default browser once no
    /// session is open.
    fn route_index(&mut self, route: &BridgeRoute) -> std::result::Result<usize, String> {
        match route {
            BridgeRoute::Default => self
                .default_index()
                .ok_or_else(|| "Bridge not connected".to_string()),
            BridgeRoute::Browser(browser_id) => {
                self.routable_index_of(browser_id).ok_or_else(|| {
                    format!(
                        "Browser [{}] is not connected to the bridge",
                        browser_fingerprint(browser_id)
                    )
                })
            },
            BridgeRoute::Thread(thread_id) => {
                if let Some(pin) = self.thread_pins.get(thread_id) {
                    if let Some(index) = self.routable_index_of(&pin.browser_id) {
                        return Ok(index);
                    }
                    if thread_has_live_session(thread_id) {
                        return Err(format!(
                            "Browser {} that thread {thread_id} runs in is not connected; the thread resumes when that browser reconnects",
                            pin.describe()
                        ));
                    }
                    info!(
                        thread_id = %thread_id,
                        browser = %pin.describe(),
                        "releasing an idle thread from a browser that is gone"
                    );
                    self.thread_pins.remove(thread_id);
                }
                let index = self
                    .default_index()
                    .ok_or_else(|| "Bridge not connected".to_string())?;
                self.pin_to(thread_id, index);
                Ok(index)
            },
        }
    }

    /// The browser the thread's next request will go to, without pinning.
    fn predicted_thread_browser(&self, thread_id: &str) -> Option<String> {
        if let Some(pin) = self.thread_pins.get(thread_id) {
            if self.routable_index_of(&pin.browser_id).is_some() {
                return Some(pin.browser_id.clone());
            }
            if thread_has_live_session(thread_id) {
                return None;
            }
        }
        self.default_index()
            .map(|index| self.connections[index].browser_id.clone())
    }

    /// Settle which browser a thread runs in before work starts: the
    /// selector's browser, else its current pin, else the default browser.
    /// A thread is moved off its pin only when that browser is gone and no
    /// CDP session is open on the thread; a conflicting selector is refused
    /// otherwise. Returns the browser id, or `None` with no browser.
    fn claim_thread(
        &mut self,
        thread_id: &str,
        selector: Option<&str>,
    ) -> std::result::Result<Option<String>, BrowserSelectError> {
        if let Some(pin) = self.thread_pins.get(thread_id).cloned() {
            let present = self.routable_index_of(&pin.browser_id).is_some();
            let movable = !present && !thread_has_live_session(thread_id);
            match selector {
                Some(selector) if pin.matches_exactly(selector) => {
                    return Ok(Some(pin.browser_id));
                },
                Some(selector) if !movable => {
                    let index = self.select(selector)?;
                    if self.connections[index].browser_id == pin.browser_id {
                        return Ok(Some(pin.browser_id));
                    }
                    return Err(BrowserSelectError::PinnedElsewhere {
                        thread_id: thread_id.to_string(),
                        pinned: pin.describe(),
                    });
                },
                None if !movable => return Ok(Some(pin.browser_id)),
                _ => {},
            }
            // Its browser is gone and nothing runs on the thread: move it to
            // the selected (or default) browser. With nowhere to go, keep it.
            let index = match selector {
                Some(selector) => self.select(selector)?,
                None => match self.default_index() {
                    Some(index) => index,
                    None => return Ok(Some(pin.browser_id)),
                },
            };
            info!(
                thread_id,
                from = %pin.describe(),
                to = %self.connections[index].describe(),
                "re-pinning an idle thread whose browser is gone"
            );
            self.thread_pins.remove(thread_id);
            self.pin_to(thread_id, index);
            return Ok(Some(self.connections[index].browser_id.clone()));
        }
        let index = match selector {
            Some(selector) => self.select(selector)?,
            None => match self.default_index() {
                Some(index) => index,
                None => return Ok(None),
            },
        };
        self.pin_to(thread_id, index);
        Ok(Some(self.connections[index].browser_id.clone()))
    }

    fn relabel(&mut self, browser_id: &str, label: &str) {
        for connection in self.connections.iter_mut() {
            if connection.browser_id == browser_id {
                connection.label = label.to_string();
            }
        }
        for pin in self.thread_pins.values_mut() {
            if pin.browser_id == browser_id {
                pin.label = label.to_string();
            }
        }
        self.store.relabel(browser_id, label);
    }

    fn status(&self, this_browser_id: Option<&str>) -> Vec<ConnectedBrowser> {
        let default_index = self.default_index();
        self.connections
            .iter()
            .enumerate()
            .map(|(index, c)| ConnectedBrowser {
                id: c.browser_id.clone(),
                fingerprint: c.fingerprint.clone(),
                label: c.label.clone(),
                connected_at: c.connected_at,
                slot: index + 1,
                default: default_index == Some(index),
                state: c.state(),
                pinned_threads: self
                    .thread_pins
                    .values()
                    .filter(|pin| pin.browser_id == c.browser_id)
                    .count(),
                this_browser: this_browser_id == Some(c.browser_id.as_str()),
            })
            .collect()
    }
}

/// A browser as the owner manages it remotely (`GET /bridge/browsers`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnownBrowser {
    pub fingerprint: String,
    pub label: String,
    /// `accepted`, `pending` or `legacy`.
    pub state: &'static str,
    pub connected: bool,
    /// The "Always use this browser" choice.
    pub always: bool,
    /// Accepted without an owner decision (upgrade window or first-run
    /// bootstrap); revoke it if that was not wanted.
    pub auto_accepted: bool,
}

/// Why a remote accept/revoke could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserManageError {
    /// No connected (for accept) or known (for revoke) browser has this
    /// fingerprint. Only a connected browser can be accepted: its handshake
    /// key is what the acceptance is bound to.
    NotFound,
    /// More than one browser has this fingerprint; nothing was changed.
    Ambiguous,
}

impl BrowserManageError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "browser_not_found",
            Self::Ambiguous => "browser_fingerprint_ambiguous",
        }
    }
}

/// What a run about to open a CDP session needs to know about its browser.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadBrowserInfo {
    /// Fingerprint of the browser the thread is pinned to, if any.
    pub pinned: Option<String>,
    /// Whether a `?choose=ask` connect would ask the owner which browser.
    pub would_ask: bool,
    pub browsers: Vec<ConnectedBrowser>,
}

impl BridgeHub {
    /// Whether a new connection starts accepted. Pairing is owner UX on top
    /// of the real boundary (token + loopback): an identified browser is
    /// accepted when the store already accepted its id and key; otherwise,
    /// only from a trusted connection (a trusted peer, or one that presented
    /// the configured token) and only with a store that can record it, as the
    /// very first browser ever (bootstrap) or during the upgrade window (at
    /// most `max_connections` automatic acceptances). A legacy build (no id)
    /// from a trusted connection is routable only during the upgrade window,
    /// so an upgrade breaks nothing, and pending afterwards.
    fn initial_acceptance(
        &mut self,
        identity: &BrowserIdentity,
        stable_key_hash: Option<&str>,
        trusted: bool,
        max_connections: usize,
    ) -> bool {
        let (id, key, hash) = match (
            identity.id.as_deref(),
            identity.key.as_deref(),
            stable_key_hash,
        ) {
            (None, _, _) => return trusted && self.store.migration_open(),
            (Some(id), Some(key), Some(hash)) => (id, key, hash),
            _ => return false,
        };
        if self.store.is_accepted(id, key) {
            return true;
        }
        if !trusted {
            return false;
        }
        let bootstrap = !self.store.bootstrapped();
        let window =
            self.store.migration_open() && self.store.auto_accepted_count() < max_connections;
        if !(bootstrap || window) {
            return false;
        }
        let recorded = self.store.accept_auto(id, hash, &identity.label);
        if recorded {
            let reason = if bootstrap {
                "first browser"
            } else {
                "upgrade window"
            };
            info!(
                browser = %browser_fingerprint(id),
                label = %identity.label,
                reason,
                "browser accepted automatically"
            );
        }
        recorded
    }

    /// The owner answered a connection's in-panel pairing prompt. Accept
    /// counts only from a trusted connection (trusted peer or matching
    /// token); Decline always disconnects. Returns the connection to retire on
    /// Decline.
    fn apply_consent(
        &mut self,
        session_id: u64,
        accepted: bool,
        trusted: bool,
    ) -> Option<BridgeConnection> {
        let index = self.index_of_session(session_id)?;
        if accepted {
            if !trusted || is_legacy_browser(&self.connections[index].browser_id) {
                warn!(
                    browser = %self.connections[index].describe(),
                    "ignored an in-panel accept from an untrusted or legacy connection"
                );
                return None;
            }
            info!(browser = %self.connections[index].describe(), "browser accepted in its panel");
            self.accept_at(index);
            None
        } else {
            let connection = self.connections.remove(index);
            self.store.close_migration();
            info!(browser = %connection.describe(), "browser declined in its panel");
            Some(connection)
        }
    }

    /// Mark a connection accepted and persist it when its key is known.
    fn accept_at(&mut self, index: usize) {
        let connection = &mut self.connections[index];
        connection.set_accepted(true);
        let record = connection.stable_key_hash.clone().map(|hash| {
            (
                connection.browser_id.clone(),
                hash,
                connection.label.clone(),
            )
        });
        connection.notify_state();
        if let Some((browser_id, hash, label)) = record {
            self.store.accept(&browser_id, &hash, &label);
        }
        // The owner has made a pairing decision: the upgrade window is over.
        self.store.close_migration();
    }

    fn known_browsers(&self) -> Vec<KnownBrowser> {
        let always = self.store.always_browser();
        let auto = |browser_id: &str| {
            self.store
                .accepted()
                .iter()
                .any(|record| record.browser_id == browser_id && record.auto_accepted)
        };
        let mut known: Vec<KnownBrowser> = self
            .connections
            .iter()
            .map(|c| KnownBrowser {
                fingerprint: c.fingerprint.clone(),
                label: c.label.clone(),
                state: c.state(),
                connected: true,
                always: always == Some(c.browser_id.as_str()),
                auto_accepted: c.accepted && auto(&c.browser_id),
            })
            .collect();
        for record in self.store.accepted() {
            if self.index_of_browser(&record.browser_id).is_none() {
                known.push(KnownBrowser {
                    fingerprint: browser_fingerprint(&record.browser_id),
                    label: record.label.clone(),
                    state: "accepted",
                    connected: false,
                    always: always == Some(record.browser_id.as_str()),
                    auto_accepted: record.auto_accepted,
                });
            }
        }
        known
    }

    /// The one connected browser with this fingerprint.
    fn connection_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> std::result::Result<Option<usize>, BrowserManageError> {
        let matches: Vec<usize> = (0..self.connections.len())
            .filter(|&i| self.connections[i].fingerprint == fingerprint)
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches[0])),
            _ => Err(BrowserManageError::Ambiguous),
        }
    }

    fn known_browser(&self, fingerprint: &str) -> Option<KnownBrowser> {
        self.known_browsers()
            .into_iter()
            .find(|browser| browser.fingerprint == fingerprint)
    }

    fn accept_by_fingerprint(
        &mut self,
        fingerprint: &str,
    ) -> std::result::Result<KnownBrowser, BrowserManageError> {
        let index = self
            .connection_by_fingerprint(fingerprint)?
            .ok_or(BrowserManageError::NotFound)?;
        if !self.connections[index].accepted {
            info!(browser = %self.connections[index].describe(), "browser accepted remotely");
            if is_legacy_browser(&self.connections[index].browser_id) {
                // No identity to persist: accepted for this connection only.
                self.connections[index].set_accepted(true);
            } else {
                self.accept_at(index);
            }
        }
        self.known_browser(fingerprint)
            .ok_or(BrowserManageError::NotFound)
    }

    /// Withdraw acceptance. A connected browser drops back to pending (no
    /// work, a new pairing prompt). Idle threads pinned to it are released;
    /// a thread with a live CDP session keeps its pin, so its requests are
    /// refused rather than moved to another browser's tab of the same id.
    fn revoke_by_fingerprint(
        &mut self,
        fingerprint: &str,
    ) -> std::result::Result<KnownBrowser, BrowserManageError> {
        let connected = self
            .connection_by_fingerprint(fingerprint)?
            .map(|index| self.connections[index].browser_id.clone());
        let browser_id = match connected {
            Some(browser_id) => browser_id,
            None => {
                let stored: Vec<String> = self
                    .store
                    .accepted()
                    .iter()
                    .filter(|record| browser_fingerprint(&record.browser_id) == fingerprint)
                    .map(|record| record.browser_id.clone())
                    .collect();
                match stored.len() {
                    0 => return Err(BrowserManageError::NotFound),
                    1 => stored[0].clone(),
                    _ => return Err(BrowserManageError::Ambiguous),
                }
            },
        };
        self.store.revoke(&browser_id);
        if let Some(index) = self.index_of_browser(&browser_id) {
            self.connections[index].set_accepted(false);
            self.connections[index].notify_state();
        }
        self.thread_pins.retain(|thread_id, pin| {
            pin.browser_id != browser_id || thread_has_live_session(thread_id)
        });
        info!(browser = %fingerprint, "browser acceptance revoked");
        self.known_browser(fingerprint)
            .ok_or(BrowserManageError::NotFound)
    }

    /// Browsers that can show a per-run choice card.
    fn card_browsers(&self) -> Vec<usize> {
        (0..self.connections.len())
            .filter(|&i| self.connections[i].shows_cards())
            .collect()
    }

    /// Ask only when the owner could pick between browsers: at least two
    /// accepted browsers connected, no usable pin, no live session that
    /// must stay put, and no choice already open for the thread.
    fn should_ask(&self, thread_id: &str) -> bool {
        let usable_pin = self
            .thread_pins
            .get(thread_id)
            .is_some_and(|pin| self.routable_index_of(&pin.browser_id).is_some());
        !usable_pin
            && !thread_has_live_session(thread_id)
            && self.card_browsers().len() >= 2
            && !self
                .choices
                .values()
                .any(|choice| choice.thread_id == thread_id)
    }

    /// Open a choice: send every accepted browser a card.
    fn open_choice(
        &mut self,
        thread_id: &str,
        task: &str,
        timeout_secs: u64,
    ) -> Option<(String, oneshot::Receiver<ChoiceOutcome>)> {
        if !self.should_ask(thread_id) {
            return None;
        }
        let choice_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let mut recipients = Vec::new();
        for index in self.card_browsers() {
            let connection = &self.connections[index];
            connection.notify(BridgeEnvelope::BrowserChoiceRequest {
                choice_id: choice_id.clone(),
                task: task.to_string(),
                timeout_secs,
            });
            recipients.push(connection.browser_id.clone());
        }
        info!(
            thread_id,
            browsers = recipients.len(),
            "asking the owner which browser to use"
        );
        self.choices.insert(
            choice_id.clone(),
            PendingChoice {
                thread_id: thread_id.to_string(),
                recipients,
                outcome: Some(tx),
            },
        );
        Some((choice_id, rx))
    }

    /// A browser's card answered. The first answer wins: it pins the thread
    /// to that browser (and, with "Always", makes it the default).
    fn answer_choice(&mut self, choice_id: &str, session_id: u64, always: bool) -> bool {
        let Some(index) = self.index_of_session(session_id) else {
            return false;
        };
        if !self.connections[index].shows_cards() {
            return false;
        }
        let browser_id = self.connections[index].browser_id.clone();
        let (outcome, thread_id) = match self.choices.get_mut(choice_id) {
            Some(choice) if choice.recipients.contains(&browser_id) => {
                match choice.outcome.take() {
                    Some(outcome) => (outcome, choice.thread_id.clone()),
                    None => return false,
                }
            },
            _ => return false,
        };
        self.thread_pins.remove(&thread_id);
        self.pin_to(&thread_id, index);
        if always {
            self.store.set_always_browser(Some(&browser_id));
        }
        let _ = outcome.send(ChoiceOutcome::Browser { browser_id });
        true
    }

    /// Settle a thread's open choice from outside the browsers (Magician's
    /// card, an explicit pin). Returns whether a choice was waiting.
    fn release_choice_for_thread(&mut self, thread_id: &str) -> bool {
        let mut released = false;
        for choice in self.choices.values_mut() {
            if choice.thread_id == thread_id {
                if let Some(outcome) = choice.outcome.take() {
                    let _ = outcome.send(ChoiceOutcome::Owner);
                    released = true;
                }
            }
        }
        released
    }

    /// Drop a finished choice and withdraw its cards everywhere.
    fn close_choice(&mut self, choice_id: &str) {
        let Some(choice) = self.choices.remove(choice_id) else {
            return;
        };
        for browser_id in &choice.recipients {
            if let Some(index) = self.index_of_browser(browser_id) {
                self.connections[index].notify(BridgeEnvelope::BrowserChoiceWithdrawn {
                    choice_id: choice_id.to_string(),
                });
            }
        }
    }

    fn thread_browser_info(&self, thread_id: &str) -> ThreadBrowserInfo {
        ThreadBrowserInfo {
            pinned: self
                .thread_pins
                .get(thread_id)
                .map(|pin| pin.fingerprint.clone()),
            would_ask: self.should_ask(thread_id),
            browsers: self.status(None),
        }
    }
}

/// Ask the owner which browser a run should use, holding the caller until
/// the first answer or `timeout`. Every accepted browser shows a card; the
/// first Accept pins the thread there ("Always" also makes it the default),
/// an answer elsewhere ([`release_browser_choice`]) settles it too, and on
/// timeout the thread goes to the default browser. Cards are withdrawn
/// everywhere once it is settled.
pub async fn choose_browser_for_thread(
    thread_id: &str,
    task: &str,
    timeout: Duration,
) -> BrowserChoiceOutcome {
    choose_with_hub(&HUB, thread_id, task, timeout).await
}

async fn choose_with_hub(
    hub: &Arc<RwLock<BridgeHub>>,
    thread_id: &str,
    task: &str,
    timeout: Duration,
) -> BrowserChoiceOutcome {
    let opened = hub
        .write()
        .await
        .open_choice(thread_id, task, timeout.as_secs());
    let Some((choice_id, rx)) = opened else {
        return BrowserChoiceOutcome::NotAsked;
    };
    finish_choice(hub, &choice_id, thread_id, rx, timeout).await
}

/// How `POST /cdp/threads/{id}/choose` left the thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceStart {
    /// Cards were sent; the choice closes on the first answer, on any pin of
    /// the thread, or at the timeout (which pins the default browser).
    Asked,
    /// A choice for this thread is already open; nothing new was sent.
    AlreadyOpen,
    /// Nothing to ask (one accepted browser, a usable pin, a live session).
    NotAsked,
}

impl ChoiceStart {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asked => "asked",
            Self::AlreadyOpen => "open",
            Self::NotAsked => "not_asked",
        }
    }
}

/// Open a browser choice for a thread without waiting for it. Idempotent
/// while a choice for the thread is open. The caller (Magician) waits for the
/// pin itself, then connects normally.
pub async fn start_browser_choice(thread_id: &str, task: &str, timeout: Duration) -> ChoiceStart {
    start_choice_with_hub(HUB.clone(), thread_id, task, timeout).await
}

async fn start_choice_with_hub(
    hub: Arc<RwLock<BridgeHub>>,
    thread_id: &str,
    task: &str,
    timeout: Duration,
) -> ChoiceStart {
    let opened = {
        let mut guard = hub.write().await;
        if guard
            .choices
            .values()
            .any(|choice| choice.thread_id == thread_id)
        {
            return ChoiceStart::AlreadyOpen;
        }
        guard.open_choice(thread_id, task, timeout.as_secs())
    };
    let Some((choice_id, rx)) = opened else {
        return ChoiceStart::NotAsked;
    };
    let thread_id = thread_id.to_string();
    actix_web::rt::spawn(async move {
        let outcome = finish_choice(&hub, &choice_id, &thread_id, rx, timeout).await;
        info!(thread_id = %thread_id, ?outcome, "browser choice closed");
    });
    ChoiceStart::Asked
}

/// Wait for a choice's first answer or its timeout, then withdraw its cards;
/// a timeout pins the default browser.
async fn finish_choice(
    hub: &Arc<RwLock<BridgeHub>>,
    choice_id: &str,
    thread_id: &str,
    rx: oneshot::Receiver<ChoiceOutcome>,
    timeout: Duration,
) -> BrowserChoiceOutcome {
    let outcome = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(ChoiceOutcome::Browser { browser_id })) => {
            BrowserChoiceOutcome::ChosenInBrowser(browser_id)
        },
        Ok(Ok(ChoiceOutcome::Owner)) => BrowserChoiceOutcome::ChosenByOwner,
        Ok(Err(_)) | Err(_) => BrowserChoiceOutcome::TimedOut,
    };
    let mut guard = hub.write().await;
    guard.close_choice(choice_id);
    if outcome == BrowserChoiceOutcome::TimedOut {
        // Nobody picked: the default browser ("Always", else the
        // longest-held accepted one).
        let _ = guard.claim_thread(thread_id, None);
    }
    outcome
}

/// Settle a thread's waiting browser choice from outside the browsers.
pub async fn release_browser_choice(thread_id: &str) -> bool {
    HUB.write().await.release_choice_for_thread(thread_id)
}

/// The thread's pin, whether a connect would ask, and the browsers.
pub async fn thread_browser_info(thread_id: &str) -> ThreadBrowserInfo {
    HUB.read().await.thread_browser_info(thread_id)
}

/// Connected browsers and accepted ones that are away.
pub async fn known_browsers() -> Vec<KnownBrowser> {
    HUB.read().await.known_browsers()
}

/// Accept a connected pending browser (remote pairing).
pub async fn accept_browser(
    fingerprint: &str,
) -> std::result::Result<KnownBrowser, BrowserManageError> {
    HUB.write().await.accept_by_fingerprint(fingerprint)
}

/// Revoke a browser's acceptance.
pub async fn revoke_browser(
    fingerprint: &str,
) -> std::result::Result<KnownBrowser, BrowserManageError> {
    HUB.write().await.revoke_by_fingerprint(fingerprint)
}

/// Extra trusted peers from the environment (comma, semicolon or space
/// separated addresses/CIDRs); containers set it to their default gateway.
pub const ENV_TRUSTED_PEERS: &str = "MAGICUTOR_TRUSTED_PEERS";

/// Origins a browser extension's own pages and workers send. Any other
/// Origin is a website (cross-origin WebSockets and fetches are allowed by
/// browsers), which must never act as a browser or as the owner.
const EXTENSION_ORIGIN_PREFIXES: &[&str] = &[
    "chrome-extension://",
    "moz-extension://",
    "safari-web-extension://",
];

/// An address or CIDR (`192.168.64.1`, `10.0.0.0/8`, `fd00::/8`).
fn parse_peer_cidr(entry: &str) -> Option<(IpAddr, u8)> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    let (addr, prefix) = match entry.split_once('/') {
        Some((addr, prefix)) => (addr.trim(), Some(prefix.trim())),
        None => (entry, None),
    };
    let ip = addr.parse::<IpAddr>().ok()?.to_canonical();
    let max: u8 = if ip.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(prefix) => prefix.parse::<u8>().ok().filter(|prefix| *prefix <= max)?,
        None => max,
    };
    Some((ip, prefix))
}

fn cidr_contains(network: IpAddr, prefix: u8, addr: IpAddr) -> bool {
    match (network, addr.to_canonical()) {
        (IpAddr::V4(network), IpAddr::V4(addr)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            u32::from(network) & mask == u32::from(addr) & mask
        },
        (IpAddr::V6(network), IpAddr::V6(addr)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            u128::from(network) & mask == u128::from(addr) & mask
        },
        _ => false,
    }
}

/// Whether `addr` is loopback or inside a configured/env trusted entry.
fn addr_is_trusted(addr: IpAddr, configured: &[String], env: &str) -> bool {
    let addr = addr.to_canonical();
    if addr.is_loopback() {
        return true;
    }
    configured
        .iter()
        .map(String::as_str)
        .chain(env.split(|c: char| c == ',' || c == ';' || c.is_whitespace()))
        .filter_map(parse_peer_cidr)
        .any(|(network, prefix)| cidr_contains(network, prefix, addr))
}

/// A trusted peer: this machine (loopback, including IPv4-mapped IPv6), or
/// an address in `bridge.trusted_peers` / `MAGICUTOR_TRUSTED_PEERS`.
pub(crate) fn peer_is_trusted(req: &HttpRequest, cfg: Option<&crate::MagicutorConfig>) -> bool {
    let Some(addr) = req.peer_addr() else {
        return false;
    };
    let configured = cfg
        .map(|cfg| cfg.bridge.trusted_peers.as_slice())
        .unwrap_or(&[]);
    addr_is_trusted(
        addr.ip(),
        configured,
        &std::env::var(ENV_TRUSTED_PEERS).unwrap_or_default(),
    )
}

/// Extra host names allowed in `Host` for trusted-peer requests (comma,
/// semicolon or space separated).
pub const ENV_ALLOWED_HOSTS: &str = "MAGICUTOR_ALLOWED_HOSTS";

/// The host name in a `Host` header value, without its port or IPv6
/// brackets, lowercased.
fn host_name(host: &str) -> String {
    let host = host.trim();
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => host
            .rsplit_once(':')
            .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
            .map(|(name, _)| name)
            .unwrap_or(host),
    };
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Whether `host` names this service directly rather than through some
/// DNS name: `localhost`, any IP literal (a page cannot rebind an IP), or a
/// configured name.
fn host_is_allowed(host: &str, configured: &[String], env: &str) -> bool {
    let name = host_name(host);
    if name == "localhost" || name.parse::<IpAddr>().is_ok() {
        return true;
    }
    configured
        .iter()
        .map(String::as_str)
        .chain(env.split(|c: char| c == ',' || c == ';' || c.is_whitespace()))
        .map(host_name)
        .any(|allowed| !allowed.is_empty() && allowed == name)
}

/// DNS-rebinding guard for trust that comes from the peer address: a page
/// served from `http://evil.example:3003` that rebinds to 127.0.0.1 makes
/// same-origin requests (no `Origin` on a GET) from loopback, but its `Host`
/// is still `evil.example`. A request without `Host` (not a browser) passes.
pub(crate) fn host_allowed(req: &HttpRequest, cfg: Option<&crate::MagicutorConfig>) -> bool {
    let Some(host) = req.headers().get("host") else {
        return true;
    };
    let Ok(host) = host.to_str() else {
        return false;
    };
    let configured = cfg
        .map(|cfg| cfg.bridge.allowed_hosts.as_slice())
        .unwrap_or(&[]);
    host_is_allowed(
        host,
        configured,
        &std::env::var(ENV_ALLOWED_HOSTS).unwrap_or_default(),
    )
}

/// No `Origin` (agent-browser, CDP clients, Magician's own HTTP client) or a
/// browser-extension origin. A website's `Origin` is refused.
pub(crate) fn origin_allowed(req: &HttpRequest) -> bool {
    match req.headers().get("origin") {
        None => true,
        Some(value) => value.to_str().is_ok_and(|origin| {
            EXTENSION_ORIGIN_PREFIXES
                .iter()
                .any(|prefix| origin.starts_with(prefix))
        }),
    }
}

/// Owner and automation routes (pairing management, CDP threads, devtools,
/// drains, contextual assist): no website `Origin`, and either a configured,
/// matching token (any `Host`) or a trusted peer addressing Magicutor by an
/// allowed `Host`.
pub(crate) fn owner_request_ok(req: &HttpRequest, cfg: Option<&crate::MagicutorConfig>) -> bool {
    if !origin_allowed(req) {
        return false;
    }
    let token_ok =
        cfg.is_some_and(|cfg| !cfg.bridge.auth_token.is_empty() && bridge_token_ok(req, cfg));
    token_ok || trusted_peer_request(req, cfg)
}

/// A trusted peer using an allowed `Host` (see [`host_allowed`]).
pub(crate) fn trusted_peer_request(
    req: &HttpRequest,
    cfg: Option<&crate::MagicutorConfig>,
) -> bool {
    peer_is_trusted(req, cfg) && host_allowed(req, cfg)
}

/// Whether a request carries the bridge's owner token (header
/// `Authorization: Bearer <token>` or `?token=`). An empty configured token
/// admits every request — see the hardening note in the component docs.
pub(crate) fn bridge_token_ok(req: &HttpRequest, cfg: &crate::MagicutorConfig) -> bool {
    if cfg.bridge.auth_token.is_empty() {
        return true;
    }
    let header_ok = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .map(|v| {
            v.trim_start_matches("Bearer ")
                .eq(cfg.bridge.auth_token.as_str())
        })
        .unwrap_or(false);
    let query_ok = req
        .query_string()
        .split('&')
        .find_map(|kv| {
            let mut parts = kv.split('=');
            match (parts.next(), parts.next()) {
                (Some("token"), Some(val)) => Some(val == cfg.bridge.auth_token),
                _ => None,
            }
        })
        .unwrap_or(false);
    header_ok || query_ok
}

/// `GET /bridge/browsers` — owner view of connected and accepted browsers.
pub async fn bridge_browsers_list(
    req: HttpRequest,
    cfg: web::Data<crate::MagicutorConfig>,
) -> HttpResponse {
    if !owner_request_ok(&req, Some(&cfg)) {
        return HttpResponse::Unauthorized().finish();
    }
    HttpResponse::Ok().json(serde_json::json!({ "browsers": known_browsers().await }))
}

fn manage_response(result: std::result::Result<KnownBrowser, BrowserManageError>) -> HttpResponse {
    match result {
        Ok(browser) => HttpResponse::Ok().json(serde_json::json!({ "browser": browser })),
        Err(error) => {
            let body = serde_json::json!({ "error": error.code() });
            match error {
                BrowserManageError::NotFound => HttpResponse::NotFound().json(body),
                BrowserManageError::Ambiguous => HttpResponse::Conflict().json(body),
            }
        },
    }
}

/// `POST /bridge/browsers/{fingerprint}/accept` — accept a pending browser.
pub async fn bridge_browser_accept(
    req: HttpRequest,
    path: web::Path<String>,
    cfg: web::Data<crate::MagicutorConfig>,
) -> HttpResponse {
    if !owner_request_ok(&req, Some(&cfg)) {
        return HttpResponse::Unauthorized().finish();
    }
    manage_response(accept_browser(path.into_inner().trim()).await)
}

/// `POST /bridge/browsers/{fingerprint}/revoke` — withdraw acceptance.
pub async fn bridge_browser_revoke(
    req: HttpRequest,
    path: web::Path<String>,
    cfg: web::Data<crate::MagicutorConfig>,
) -> HttpResponse {
    if !owner_request_ok(&req, Some(&cfg)) {
        return HttpResponse::Unauthorized().finish();
    }
    manage_response(revoke_browser(path.into_inner().trim()).await)
}

/// Whether any browser extension is connected. Health and Edge capability
/// discovery use this signal so a running proxy without an extension is not
/// advertised as usable browser control.
pub async fn is_connected() -> bool {
    !HUB.read().await.connections.is_empty()
}

/// Connected browsers in slot order, with how many CDP threads each holds.
/// `this_browser_id` marks the caller's own entry.
pub async fn connected_browsers(this_browser_id: Option<&str>) -> Vec<ConnectedBrowser> {
    HUB.read().await.status(this_browser_id)
}

/// Resolve an optional selector (install id, fingerprint or label) to a
/// connected browser id. `None` selects the default browser, or nothing when
/// no browser is connected.
pub async fn select_browser(
    selector: Option<&str>,
) -> std::result::Result<Option<String>, BrowserSelectError> {
    let hub = HUB.read().await;
    match selector {
        Some(selector) => hub
            .select(selector)
            .map(|index| Some(hub.connections[index].browser_id.clone())),
        None => Ok(hub
            .default_index()
            .map(|index| hub.connections[index].browser_id.clone())),
    }
}

/// Settle a CDP thread's browser before its work starts (see
/// [`BridgeHub::claim_thread`]). Returns the browser's install id.
pub async fn claim_thread(
    thread_id: &str,
    selector: Option<&str>,
) -> std::result::Result<Option<String>, BrowserSelectError> {
    HUB.write().await.claim_thread(thread_id, selector)
}

/// The install id of the browser a thread is pinned to, if any.
pub async fn thread_browser(thread_id: &str) -> Option<String> {
    HUB.read()
        .await
        .thread_pins
        .get(thread_id)
        .map(|pin| pin.browser_id.clone())
}

/// The browser a thread's next request would reach, without pinning it.
pub async fn predicted_thread_browser(thread_id: &str) -> Option<String> {
    HUB.read().await.predicted_thread_browser(thread_id)
}

/// Forget a thread's browser pin. Returns the browser it was pinned to.
pub async fn unpin_thread(thread_id: &str) -> Option<String> {
    HUB.write()
        .await
        .thread_pins
        .remove(thread_id)
        .map(|pin| pin.browser_id)
}

/// Process-wide CDP event subscribers, keyed by (browser install id, tab
/// id): tab ids are only unique within one browser. The cdp_proxy CdpSession
/// actors register here when they attach to a tab and receive
/// chrome.debugger events forwarded by that browser's extension. Multiple
/// subscribers per tab are supported (e.g. browser-level + page-level WS for
/// the same tab) — each gets a clone of every event.
type CdpSubscriberKey = (String, i32);

static CDP_EVENT_SUBSCRIBERS: Lazy<Mutex<HashMap<CdpSubscriberKey, Vec<CdpEventSubscriber>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Subscriber callback. Returns false if the receiver should be dropped
/// (e.g. WS closed); the dispatcher prunes the registry on next event.
type CdpEventSubscriber = Arc<dyn Fn(CdpEventPayload) -> bool + Send + Sync>;

/// Register a closure to receive CDP events for a tab of one browser.
/// Returns a SubscriberHandle that unregisters on drop. The closure is
/// invoked from the bridge dispatcher; it must be cheap and non-blocking —
/// typically `addr.do_send(event)` to an Actix actor.
pub fn subscribe_cdp_events(
    browser_id: &str,
    tab_id: i32,
    callback: impl Fn(CdpEventPayload) -> bool + Send + Sync + 'static,
) -> CdpSubscriberHandle {
    let arc: CdpEventSubscriber = Arc::new(callback);
    let key = (browser_id.to_string(), tab_id);
    if let Ok(mut map) = CDP_EVENT_SUBSCRIBERS.lock() {
        map.entry(key.clone()).or_default().push(arc.clone());
    }
    CdpSubscriberHandle { key, arc }
}

/// RAII handle that unregisters the subscriber when dropped.
pub struct CdpSubscriberHandle {
    key: CdpSubscriberKey,
    arc: CdpEventSubscriber,
}

impl Drop for CdpSubscriberHandle {
    fn drop(&mut self) {
        if let Ok(mut map) = CDP_EVENT_SUBSCRIBERS.lock() {
            if let Some(list) = map.get_mut(&self.key) {
                list.retain(|s| !Arc::ptr_eq(s, &self.arc));
                if list.is_empty() {
                    map.remove(&self.key);
                }
            }
        }
    }
}

/// Fan a CDP event out to the subscribers of the browser it arrived from.
/// The browser id is stamped from the connection, never taken from the
/// extension's payload. Returns how many subscribers received it.
fn dispatch_cdp_event(browser_id: &str, mut data: CdpEventPayload) -> usize {
    data.browser_id = browser_id.to_string();
    let key = (data.browser_id.clone(), data.tab_id);
    // Cheap clones — the closures typically just `do_send` to an actor.
    let subscribers: Vec<CdpEventSubscriber> = match CDP_EVENT_SUBSCRIBERS.lock() {
        Ok(map) => map.get(&key).cloned().unwrap_or_default(),
        Err(e) => {
            warn!("CDP_EVENT_SUBSCRIBERS poisoned: {}", e);
            return 0;
        },
    };
    debug!(
        "[bridge] cdp_event browser={} tab={} method={} subscribers={}",
        browser_fingerprint(browser_id),
        data.tab_id,
        data.method,
        subscribers.len()
    );
    for cb in &subscribers {
        let _ = cb(data.clone());
    }
    subscribers.len()
}

/// Payload for a CDP event pushed from the extension. Mirrors the shape the
/// extension serialises: `{tabId, sessionId?, method, params}`; `browserId`
/// is added by the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpEventPayload {
    pub tab_id: i32,
    /// Chrome's session id when a child target of the tab (an auto-attached
    /// OOPIF iframe) emitted the event; `None` for the tab's own page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    /// Install id of the browser that emitted the event.
    #[serde(default)]
    pub browser_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeEnvelope {
    Request {
        data: ExtensionRequest,
    },
    Response {
        data: ExtensionResponse,
    },
    Ping {
        timestamp: i64,
    },
    Pong {
        timestamp: i64,
    },
    /// Fire-and-forget chrome.debugger event pushed by the extension.
    /// Routed to subscribers registered via [`subscribe_cdp_events`].
    CdpEvent {
        data: CdpEventPayload,
    },
    /// Fire-and-forget passive page identity/change signal pushed by the
    /// extension. Buffered by thread until Magician drains it.
    AmbientPageSignal {
        data: crate::types::AmbientPageSignal,
    },
    /// The owner renamed this browser in the extension panel. Applies to the
    /// live connection without a reconnect, so in-flight work is kept.
    BrowserLabel {
        label: String,
    },
    /// The owner answered this browser's pairing prompt. Decline closes the
    /// connection; the extension stays away until the owner reconnects it.
    BrowserConsent {
        accepted: bool,
    },
    /// The owner picked this browser in a per-run choice card.
    BrowserChoiceAnswer {
        choice_id: String,
        #[serde(default)]
        always: bool,
    },
    /// Server notice: this browser's pairing state (`pending` shows the
    /// pairing prompt) and its fingerprint.
    BrowserState {
        state: String,
        fingerprint: String,
    },
    /// Server notice: a run asks which browser to use.
    BrowserChoiceRequest {
        choice_id: String,
        task: String,
        timeout_secs: u64,
    },
    /// Server notice: the choice was settled or timed out; drop its card.
    BrowserChoiceWithdrawn {
        choice_id: String,
    },
}

/// Frames a pending (not yet accepted) browser may send: its pairing answer,
/// a rename, and keep-alives. Everything else — responses, CDP events, page
/// signals, choice answers — is dropped until the owner accepts it.
fn frame_allowed_while_pending(envelope: &BridgeEnvelope) -> bool {
    matches!(
        envelope,
        BridgeEnvelope::BrowserConsent { .. }
            | BridgeEnvelope::BrowserLabel { .. }
            | BridgeEnvelope::Ping { .. }
            | BridgeEnvelope::Pong { .. }
    )
}

#[derive(Debug)]
enum BridgeOutbound {
    Request(ExtensionRequest),
    /// A server notice (pairing state, choice cards).
    Envelope(BridgeEnvelope),
}

/// The identity an extension states in its handshake query string:
/// `browser_id` (a per-install UUID), `browser_key` (a per-install secret,
/// sent only here and never echoed) and `browser_label` (a display name).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserIdentity {
    id: Option<String>,
    key: Option<String>,
    label: String,
}

fn valid_token(value: &str, min_len: usize, max_len: usize) -> bool {
    value.len() >= min_len
        && value.len() <= max_len
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_browser_id(id: &str) -> bool {
    valid_token(id, 1, MAX_BROWSER_ID_LEN) && !is_legacy_browser(id)
}

fn sanitize_browser_label(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let bounded: String = cleaned
        .trim()
        .chars()
        .take(MAX_BROWSER_LABEL_CHARS)
        .collect();
    let bounded = bounded.trim();
    if bounded.is_empty() {
        DEFAULT_BROWSER_LABEL.to_string()
    } else {
        bounded.to_string()
    }
}

fn browser_identity_from_query(query: &str) -> std::result::Result<BrowserIdentity, &'static str> {
    let params = web::Query::<HashMap<String, String>>::from_query(query)
        .map(|query| query.into_inner())
        .unwrap_or_default();
    let id = match params.get("browser_id").map(|id| id.trim()) {
        None | Some("") => None,
        Some(id) if valid_browser_id(id) => Some(id.to_string()),
        Some(_) => return Err("invalid_browser_id"),
    };
    let key = match params.get("browser_key").map(|key| key.trim()) {
        None | Some("") => None,
        Some(key) if valid_token(key, MIN_BROWSER_KEY_LEN, MAX_BROWSER_KEY_LEN) => {
            Some(key.to_string())
        },
        Some(_) => return Err("invalid_browser_key"),
    };
    let label = sanitize_browser_label(
        params
            .get("browser_label")
            .map(String::as_str)
            .unwrap_or(""),
    );
    Ok(BrowserIdentity { id, key, label })
}

/// What admission does with a handshake.
#[derive(Debug, PartialEq, Eq)]
enum Admission {
    /// Take a new slot.
    New,
    /// Replace the browser's own stale connection at this index.
    Replace(usize),
    /// Refuse: the id is held by a connection this handshake cannot replace.
    IdInUse,
    /// Refuse: every slot is held by other browsers.
    Full,
}

impl BridgeHub {
    /// A same-id handshake replaces the existing connection only when it
    /// proves the install's secret key and the existing socket has gone
    /// quiet. Anything else that reuses an id is refused, so knowing an id
    /// is never enough to take over a browser's connection or its threads.
    fn admission(
        &self,
        identity: &BrowserIdentity,
        max_connections: usize,
        quiet_period: Duration,
    ) -> Admission {
        if let Some(index) = identity
            .id
            .as_deref()
            .and_then(|id| self.index_of_browser(id))
        {
            let existing = &self.connections[index];
            let proves_key = match (existing.key_digest, identity.key.as_deref()) {
                (Some(expected), Some(key)) => browser_key_digest(key) == expected,
                _ => false,
            };
            return if proves_key && idle_for(&existing.last_traffic) >= quiet_period {
                Admission::Replace(index)
            } else {
                Admission::IdInUse
            };
        }
        if self.connections.len() >= max_connections {
            Admission::Full
        } else {
            Admission::New
        }
    }
}

pub async fn bridge_route(
    req: HttpRequest,
    stream: web::Payload,
    cfg: web::Data<crate::MagicutorConfig>,
) -> std::result::Result<HttpResponse, ActixError> {
    bridge_route_with_hub(req, stream, cfg, HUB.clone()).await
}

async fn bridge_route_with_hub(
    req: HttpRequest,
    stream: web::Payload,
    cfg: web::Data<crate::MagicutorConfig>,
    hub: Arc<RwLock<BridgeHub>>,
) -> std::result::Result<HttpResponse, ActixError> {
    info!("Bridge connection attempt from {:?}", req.peer_addr());

    if !cfg.bridge.enabled {
        warn!("Bridge connection rejected: bridge disabled in config");
        return Ok(HttpResponse::NotFound().finish());
    }

    // A website can open a cross-origin WebSocket to loopback; only an
    // extension (or a client sending no Origin) may be a browser.
    if !origin_allowed(&req) {
        warn!("Bridge connection rejected: website Origin");
        return Ok(HttpResponse::Forbidden().finish());
    }
    let peer_trusted = trusted_peer_request(&req, Some(&cfg));
    if cfg.bridge.auth_token.is_empty() && !peer_trusted {
        // No token is configured: the only boundary left is the peer.
        warn!("Bridge connection rejected: no token configured and the peer is not trusted");
        return Ok(HttpResponse::Forbidden().finish());
    }
    if !bridge_token_ok(&req, &cfg) {
        warn!("Bridge connection rejected: missing/invalid token");
        return Ok(HttpResponse::Unauthorized().finish());
    }
    // Trusted for automatic acceptance and in-panel consent: a trusted peer,
    // or a connection that presented the configured token (checked above).
    let trusted_connection = peer_trusted || !cfg.bridge.auth_token.is_empty();

    let identity = match browser_identity_from_query(req.query_string()) {
        Ok(identity) => identity,
        Err(code) => {
            warn!("Bridge connection rejected: {}", code);
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": code,
                "message": "browser_id must be 1-64 and browser_key 16-128 letters, digits, '-' or '_'"
            })));
        },
    };
    let max_connections = cfg.bridge.max_connections.max(1);

    // Admit under the same lock as the upgrade. Concurrent handshakes cannot
    // overfill the slots; a failed upgrade takes no slot and evicts nothing.
    // Refuse before WebSocket OPEN so a waiting extension never briefly
    // reports connected.
    let mut guard = hub.write().await;
    let session_id = NEXT_BRIDGE_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let browser_id = match identity.id.as_deref() {
        Some(id) => id.to_string(),
        None => format!("{LEGACY_BROWSER_ID}-{session_id}"),
    };
    let fingerprint = browser_fingerprint(&browser_id);
    let replaces = match guard.admission(&identity, max_connections, REPLACE_QUIET_PERIOD) {
        Admission::New => None,
        Admission::Replace(index) => Some(index),
        Admission::IdInUse => {
            warn!(
                browser = %fingerprint,
                "Bridge connection rejected: browser id held by an active connection or key mismatch"
            );
            return Ok(HttpResponse::Conflict().json(serde_json::json!({
                "error": "browser_id_in_use",
                "message": format!(
                    "A connection for browser [{fingerprint}] is already open and active. A reconnect of the same browser is admitted once that connection goes quiet; a copied profile must not share its extension identity."
                ),
            })));
        },
        Admission::Full => {
            let labels: Vec<String> = guard.connections.iter().map(|c| c.describe()).collect();
            warn!(
                browser = %fingerprint,
                label = %identity.label,
                "Bridge connection rejected: all {} browser slots are in use",
                max_connections
            );
            return Ok(HttpResponse::Conflict().json(serde_json::json!({
                "error": "bridge_full",
                "message": format!(
                    "All {max_connections} browser slots are in use ({}). Disconnect the Magican extension in one of those browsers before connecting another.",
                    labels.join(", ")
                ),
                "max_browsers": max_connections,
                "browsers": labels,
            })));
        },
    };

    let (tx, rx) = mpsc::unbounded_channel::<BridgeOutbound>();
    let last_traffic = Arc::new(Mutex::new(Instant::now()));
    let routable_flag = Arc::new(AtomicBool::new(false));
    // Use WsResponseBuilder to configure larger frame sizes for DOM snapshots
    // Default is 64KB which is too small for large page content
    let response = ws::WsResponseBuilder::new(
        BridgeSession {
            session_id,
            browser_id: browser_id.clone(),
            hub: hub.clone(),
            last_traffic: last_traffic.clone(),
            routable: routable_flag.clone(),
            trusted_connection,
            _config: cfg.clone(),
            outbound: Some(rx),
            continuation_buffer: BytesMut::new(),
            continuation_is_text: false,
        },
        &req,
        stream,
    )
    .frame_size(16 * 1024 * 1024) // 16MB max frame size
    .start();
    if response.is_ok() {
        // Pairing: see `BridgeHub::initial_acceptance`.
        let stable_key_hash = identity.key.as_deref().map(|key| guard.store.key_hash(key));
        let accepted = guard.initial_acceptance(
            &identity,
            stable_key_hash.as_deref(),
            trusted_connection,
            max_connections,
        );
        routable_flag.store(accepted, Ordering::SeqCst);
        let connection = BridgeConnection {
            session_id,
            browser_id: browser_id.clone(),
            fingerprint: fingerprint.clone(),
            label: identity.label.clone(),
            key_digest: identity.key.as_deref().map(browser_key_digest),
            accepted,
            routable_flag,
            stable_key_hash,
            connected_at: Utc::now(),
            last_traffic,
            tx,
            addr: None,
            pending: HashMap::new(),
        };
        // Queued until the socket opens: tells the extension whether to show
        // the pairing prompt.
        connection.notify_state();
        let slot = match replaces {
            Some(index) => {
                // The replacement keeps the browser's slot, so the default
                // browser does not change because it reconnected.
                let stale = std::mem::replace(&mut guard.connections[index], connection);
                info!(
                    browser = %fingerprint,
                    stale_session_id = stale.session_id,
                    session_id,
                    "Bridge reconnect replaced the same browser's stale session"
                );
                stale.retire(
                    "bridge session disconnected: replaced by a reconnect of the same browser",
                );
                index + 1
            },
            None => {
                guard.connections.push(connection);
                guard.connections.len()
            },
        };
        guard.relabel(&browser_id, &identity.label);
        let state = if accepted { "accepted" } else { "pending" };
        info!(
            session_id,
            browser = %fingerprint,
            label = %identity.label,
            state,
            slot,
            max_connections,
            "Bridge browser connected"
        );
    }
    response
}

struct BridgeSession {
    session_id: u64,
    /// Install id of the browser on the other end; stamped on its CDP events.
    browser_id: String,
    hub: Arc<RwLock<BridgeHub>>,
    /// Last inbound frame; shared with the hub so admission can tell a live
    /// socket from a half-open one.
    last_traffic: Arc<Mutex<Instant>>,
    /// False while the browser is pending: only consent, label and
    /// ping/pong frames are processed.
    routable: Arc<AtomicBool>,
    /// The socket's peer is on this machine; an in-panel accept counts only
    /// then.
    trusted_connection: bool,
    _config: web::Data<crate::MagicutorConfig>,
    /// Receiver for requests routed to this connection. Its sender is in the
    /// hub from admission on, so requests sent before the actor starts queue.
    outbound: Option<mpsc::UnboundedReceiver<BridgeOutbound>>,
    /// Buffer for reassembling continuation frames (large messages)
    continuation_buffer: BytesMut,
    /// Whether we're currently accumulating a text message (vs binary)
    continuation_is_text: bool,
}

impl Actor for BridgeSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        info!(
            session_id = self.session_id,
            browser = %browser_fingerprint(&self.browser_id),
            "Bridge session started"
        );

        // Start heartbeat to keep connection alive
        ctx.run_interval(Duration::from_secs(15), |act, ctx| {
            if idle_for(&act.last_traffic) >= BRIDGE_IDLE_TIMEOUT {
                warn!(session_id = act.session_id, "Bridge heartbeat timed out");
                ctx.close(None);
                ctx.stop();
                return;
            }
            ctx.ping(b"");
        });

        // Register this actor so a same-browser reconnect can close it. A
        // session already replaced (or never admitted) closes itself.
        {
            let hub = self.hub.clone();
            let session_id = self.session_id;
            let addr = ctx.address();
            actix_web::rt::spawn(async move {
                let mut guard = hub.write().await;
                match guard.index_of_session(session_id) {
                    Some(index) => {
                        guard.connections[index].addr = Some(addr);
                        drop(guard);
                        info!(session_id, "Bridge session registered in HUB");
                    },
                    None => {
                        drop(guard);
                        addr.do_send(CloseBridgeSession);
                        debug!(session_id, "discarded stale bridge session registration");
                    },
                }
            });
        }

        // Outgoing sender loop
        if let Some(mut rx) = self.outbound.take() {
            let addr = ctx.address();
            actix_web::rt::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    match msg {
                        BridgeOutbound::Request(req) => {
                            let env = BridgeEnvelope::Request { data: req };
                            if let Ok(text) = serde_json::to_string(&env) {
                                addr.do_send(WsText(text));
                            }
                        },
                        BridgeOutbound::Envelope(env) => {
                            if let Ok(text) = serde_json::to_string(&env) {
                                addr.do_send(WsText(text));
                            }
                        },
                    }
                }
                debug!("Bridge outgoing sender loop ended");
            });
        }
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        info!(
            session_id = self.session_id,
            browser = %browser_fingerprint(&self.browser_id),
            "Bridge session stopped"
        );
        let session_id = self.session_id;
        let hub = self.hub.clone();
        actix_web::rt::spawn(async move {
            let mut hub = hub.write().await;
            // A session that a reconnect replaced is no longer in the hub;
            // it must not touch the replacement or any other browser.
            if let Some(index) = hub.index_of_session(session_id) {
                let connection = hub.connections.remove(index);
                if is_legacy_browser(&connection.browser_id) {
                    // A legacy id is never reused, so nothing may stay
                    // attached to it.
                    let gone = connection.browser_id.clone();
                    hub.thread_pins.retain(|_, pin| pin.browser_id != gone);
                }
                drop(hub);
                info!(
                    session_id,
                    browser = %connection.describe(),
                    pending = connection.pending.len(),
                    "Bridge browser disconnected"
                );
                for (_, sender) in connection.pending {
                    let _ = sender.send(Err("bridge session disconnected".to_string()));
                }
            }
        });
    }
}

struct CloseBridgeSession;

impl Message for CloseBridgeSession {
    type Result = ();
}

impl Handler<CloseBridgeSession> for BridgeSession {
    type Result = ();

    fn handle(&mut self, _msg: CloseBridgeSession, ctx: &mut Self::Context) {
        ctx.close(None);
        ctx.stop();
    }
}

struct WsText(String);

impl Message for WsText {
    type Result = ();
}

impl Handler<WsText> for BridgeSession {
    type Result = ();
    fn handle(&mut self, msg: WsText, ctx: &mut Self::Context) -> Self::Result {
        ctx.text(msg.0);
    }
}

impl BridgeSession {
    /// Process a complete text message (either received directly or reassembled from continuation frames)
    fn process_text_message(&self, text: &str, ctx: &mut ws::WebsocketContext<Self>) {
        let text_len = text.len();
        trace!("Bridge inbound: {} bytes", text_len);
        match serde_json::from_str::<BridgeEnvelope>(text) {
            Ok(env)
                if !self.routable.load(Ordering::SeqCst) && !frame_allowed_while_pending(&env) =>
            {
                debug!(
                    session_id = self.session_id,
                    "dropped a frame from a browser that is not accepted"
                );
            },
            Ok(env) => match env {
                BridgeEnvelope::Response { data } => {
                    let request_id = data.request_id.clone();
                    let hub = self.hub.clone();
                    let session_id = self.session_id;
                    debug!(
                        "Bridge response: id={}, success={}, size={} bytes",
                        request_id, data.success, text_len
                    );
                    actix_web::rt::spawn(async move {
                        let mut hub = hub.write().await;
                        // Only this connection's own requests can be answered
                        // by it; a response id from another browser is unknown.
                        let Some(index) = hub.index_of_session(session_id) else {
                            return;
                        };
                        let connection = &mut hub.connections[index];
                        if let Some(tx) = connection.pending.remove(&data.request_id) {
                            if tx.send(Ok(data)).is_err() {
                                warn!("Bridge response channel closed for {}", request_id);
                            }
                        } else {
                            warn!(
                                "Dropping unmatched bridge response {} from {} (pending: {:?})",
                                data.request_id,
                                connection.describe(),
                                connection.pending.keys().collect::<Vec<_>>()
                            );
                        }
                    });
                },
                BridgeEnvelope::Ping { timestamp } => {
                    // Respond with pong to keep extension alive
                    trace!("Bridge received ping, sending pong");
                    let pong = BridgeEnvelope::Pong { timestamp };
                    if let Ok(text) = serde_json::to_string(&pong) {
                        ctx.text(text);
                    }
                },
                BridgeEnvelope::Pong { .. } => {
                    trace!("Bridge received pong");
                },
                BridgeEnvelope::Request { .. } => {
                    // Server shouldn't receive requests from extension in normal flow
                    warn!("Unexpected request from extension");
                },
                BridgeEnvelope::CdpEvent { data } => {
                    dispatch_cdp_event(&self.browser_id, data);
                },
                BridgeEnvelope::AmbientPageSignal { data } => {
                    page_signals::record(data);
                },
                BridgeEnvelope::BrowserConsent { accepted } => {
                    let hub = self.hub.clone();
                    let session_id = self.session_id;
                    let trusted_connection = self.trusted_connection;
                    actix_web::rt::spawn(async move {
                        let declined = hub.write().await.apply_consent(
                            session_id,
                            accepted,
                            trusted_connection,
                        );
                        if let Some(connection) = declined {
                            connection.retire("bridge session disconnected: declined by the owner");
                        }
                    });
                },
                BridgeEnvelope::BrowserChoiceAnswer { choice_id, always } => {
                    let hub = self.hub.clone();
                    let session_id = self.session_id;
                    actix_web::rt::spawn(async move {
                        let answered = hub
                            .write()
                            .await
                            .answer_choice(&choice_id, session_id, always);
                        debug!(choice_id = %choice_id, answered, "browser choice answer");
                    });
                },
                BridgeEnvelope::BrowserState { .. }
                | BridgeEnvelope::BrowserChoiceRequest { .. }
                | BridgeEnvelope::BrowserChoiceWithdrawn { .. } => {
                    warn!("Unexpected server notice from extension");
                },
                BridgeEnvelope::BrowserLabel { label } => {
                    let label = sanitize_browser_label(&label);
                    let hub = self.hub.clone();
                    let session_id = self.session_id;
                    actix_web::rt::spawn(async move {
                        let mut hub = hub.write().await;
                        let Some(index) = hub.index_of_session(session_id) else {
                            return;
                        };
                        let browser_id = hub.connections[index].browser_id.clone();
                        hub.relabel(&browser_id, &label);
                        info!(
                            browser = %browser_fingerprint(&browser_id),
                            label = %label,
                            "Bridge browser renamed"
                        );
                    });
                },
            },
            Err(e) => {
                warn!(
                    "Failed to parse bridge message (error: {}), first 200 chars: {}",
                    e,
                    &text[..std::cmp::min(200, text.len())]
                );
            },
        }
    }
}

impl StreamHandler<std::result::Result<ws::Message, ws::ProtocolError>> for BridgeSession {
    fn handle(
        &mut self,
        item: std::result::Result<ws::Message, ws::ProtocolError>,
        ctx: &mut Self::Context,
    ) {
        if item.is_ok() {
            mark_traffic(&self.last_traffic);
        }
        match item {
            Ok(msg) => match msg {
                ws::Message::Text(text) => {
                    self.process_text_message(&text, ctx);
                },
                ws::Message::Binary(data) => {
                    info!(
                        "Bridge received binary data: {} bytes (unexpected - should be text)",
                        data.len()
                    );
                },
                ws::Message::Close(reason) => {
                    info!("Bridge received close frame: {:?}", reason);
                    ctx.close(reason);
                },
                ws::Message::Ping(data) => {
                    trace!("Bridge received WS ping, sending pong");
                    ctx.pong(&data);
                },
                ws::Message::Pong(_) => {
                    trace!("Bridge received WS pong");
                },
                ws::Message::Continuation(item) => {
                    // Handle fragmented messages (large payloads are split across frames)
                    match item {
                        WsItem::FirstText(bytes) => {
                            trace!("Bridge continuation: FirstText {} bytes", bytes.len());
                            self.continuation_buffer.clear();
                            self.continuation_buffer.extend_from_slice(&bytes);
                            self.continuation_is_text = true;
                        },
                        WsItem::FirstBinary(bytes) => {
                            trace!("Bridge continuation: FirstBinary {} bytes", bytes.len());
                            self.continuation_buffer.clear();
                            self.continuation_buffer.extend_from_slice(&bytes);
                            self.continuation_is_text = false;
                        },
                        WsItem::Continue(bytes) => {
                            trace!(
                                "Bridge continuation: {} bytes (total: {} bytes)",
                                bytes.len(),
                                self.continuation_buffer.len() + bytes.len()
                            );
                            self.continuation_buffer.extend_from_slice(&bytes);
                        },
                        WsItem::Last(bytes) => {
                            self.continuation_buffer.extend_from_slice(&bytes);
                            let total_len = self.continuation_buffer.len();
                            debug!("Bridge reassembled: {} bytes", total_len);

                            if self.continuation_is_text {
                                match std::str::from_utf8(&self.continuation_buffer) {
                                    Ok(text) => {
                                        self.process_text_message(text, ctx);
                                    },
                                    Err(e) => {
                                        warn!("Bridge continuation: invalid UTF-8: {}", e);
                                    },
                                }
                            } else {
                                warn!(
                                    "Bridge received binary data: {} bytes (unexpected)",
                                    total_len
                                );
                            }

                            self.continuation_buffer.clear();
                        },
                    }
                },
                ws::Message::Nop => {},
            },
            Err(e) => {
                let rendered = format!("{e:?}");
                if rendered.contains("UnexpectedEof")
                    || rendered.contains("ResetWithoutClosingHandshake")
                    || rendered.contains("ConnectionReset")
                {
                    debug!(session_id = self.session_id, error = %rendered, "Bridge WebSocket disconnected");
                } else {
                    warn!(session_id = self.session_id, error = %rendered, "Bridge WebSocket protocol error");
                }
            },
        }
    }
}

/// Send a request to the default browser over the bridge if connected.
pub async fn send_over_bridge(req: ExtensionRequest) -> Result<ExtensionResponse> {
    send_over_bridge_via(&BridgeRoute::Default, req).await
}

/// Send a request to the browser `route` names.
pub async fn send_over_bridge_via(
    route: &BridgeRoute,
    req: ExtensionRequest,
) -> Result<ExtensionResponse> {
    send_routed(&HUB, route, req).await.map(|r| r.response)
}

/// Send a request to the browser `route` names and report which browser
/// answered, so follow-up work (event subscriptions) stays in that browser.
pub async fn send_over_bridge_tracked(
    route: &BridgeRoute,
    req: ExtensionRequest,
) -> Result<RoutedResponse> {
    send_routed(&HUB, route, req).await
}

async fn send_routed(
    hub: &Arc<RwLock<BridgeHub>>,
    route: &BridgeRoute,
    req: ExtensionRequest,
) -> Result<RoutedResponse> {
    let action = req.action.clone();
    let request_id = req.request_id.clone();
    let timeout_secs = bridge_timeout_secs(&req);
    // Named in the failure lines: "Timeout waiting for response (10s)" alone
    // never said which action or CDP method the extension sat on.
    let method = req
        .params
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let tab_id = req.params.get("tabId").and_then(serde_json::Value::as_i64);
    let started = std::time::Instant::now();
    debug!(
        "Bridge request: action={}, id={}, route={:?}",
        action, request_id, route
    );

    let (tx, rx) = oneshot::channel();
    let (session_id, browser_id) = {
        let mut hub = hub.write().await;
        let index = match hub.route_index(route) {
            Ok(index) => index,
            Err(reason) => {
                warn!("Bridge request {} not sent: {}", action, reason);
                return Err(ExecutionError::BridgeError(reason));
            },
        };
        let connection = &mut hub.connections[index];
        connection.pending.insert(request_id.clone(), tx);
        if let Err(e) = connection.tx.send(BridgeOutbound::Request(req)) {
            connection.pending.remove(&request_id);
            return Err(ExecutionError::BridgeError(e.to_string()));
        }
        (connection.session_id, connection.browser_id.clone())
    };

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(Ok(response))) => {
            if !response.success {
                warn!(
                    "Bridge error response: id={}, action={}, method={}, tab={:?}, elapsed_ms={}, browser={}, error={:?}",
                    response.request_id,
                    action,
                    method,
                    tab_id,
                    started.elapsed().as_millis(),
                    browser_id,
                    response.error
                );
            }
            Ok(RoutedResponse {
                browser_id,
                response,
            })
        },
        Ok(Ok(Err(reason))) => {
            debug!(
                reason,
                browser = %browser_id,
                "send_over_bridge: bridge session ended before response"
            );
            Err(ExecutionError::BridgeError(reason))
        },
        Ok(Err(e)) => {
            warn!("send_over_bridge: Channel error: {}", e);
            Err(ExecutionError::BridgeError(e.to_string()))
        },
        Err(_) => {
            {
                let mut hub = hub.write().await;
                if let Some(index) = hub.index_of_session(session_id) {
                    hub.connections[index].pending.remove(&request_id);
                }
            }
            warn!(
                "send_over_bridge: Timeout waiting for response ({}s, action={}, method={}, tab={:?}, browser={})",
                timeout_secs, action, method, tab_id, browser_id
            );
            Err(ExecutionError::BridgeTimeout {
                action,
                timeout_secs,
            })
        },
    }
}

fn bridge_timeout_secs(req: &ExtensionRequest) -> u64 {
    match req.action.as_str() {
        "probe_contextual_assist" => 2,
        "capture_contextual_assist_tab" => 10,
        "list_tabs" => 10,
        "clear_session" | "session_ended" | "close_tab" | "close_window" | "close_session_tabs" => {
            15
        },
        "create_window" | "create_tab" | "attach_tab" | "attach_debugger" => 60,
        "debugger_command" => 75,
        "scroll" => 20,
        "click" | "click_coordinates" | "hover" | "press_key" | "slide" | "drag_and_drop"
        | "drag_path" => 60,
        _ => 600,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};

    type TestSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    fn spawn_bridge_server(
        hub: Arc<RwLock<BridgeHub>>,
        max_connections: usize,
    ) -> (String, actix_web::dev::ServerHandle) {
        let server = actix_web::HttpServer::new(move || {
            let route_hub = hub.clone();
            let mut config = crate::MagicutorConfig::default();
            config.bridge.auth_token.clear();
            config.bridge.max_connections = max_connections;
            actix_web::App::new()
                .app_data(web::Data::new(config))
                .route(
                    "/bridge",
                    web::get().to(move |req, stream, cfg| {
                        bridge_route_with_hub(req, stream, cfg, route_hub.clone())
                    }),
                )
        })
        .workers(1)
        .bind(("127.0.0.1", 0))
        .unwrap();
        let url = format!("ws://{}/bridge", server.addrs()[0]);
        let server = server.run();
        let handle = server.handle();
        actix_web::rt::spawn(server);
        (url, handle)
    }

    fn test_key(browser_id: &str) -> String {
        format!("{browser_id}-secret-key-0123456789")
    }

    async fn connect_with_key(
        url: &str,
        browser_id: &str,
        key: Option<&str>,
        label: &str,
    ) -> std::result::Result<TestSocket, WsError> {
        let key = key
            .map(|key| format!("&browser_key={key}"))
            .unwrap_or_default();
        tokio_tungstenite::connect_async(format!(
            "{url}?browser_id={browser_id}{key}&browser_label={label}"
        ))
        .await
        .map(|(socket, _)| socket)
    }

    async fn connect_browser(
        url: &str,
        browser_id: &str,
        label: &str,
    ) -> std::result::Result<TestSocket, WsError> {
        let key = test_key(browser_id);
        connect_with_key(url, browser_id, Some(key.as_str()), label).await
    }

    async fn wait_for(hub: &Arc<RwLock<BridgeHub>>, check: impl Fn(&BridgeHub) -> bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if check(&*hub.read().await) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("bridge hub did not reach the expected state");
    }

    fn all_registered(hub: &BridgeHub, count: usize) -> bool {
        hub.connections.len() == count && hub.connections.iter().all(|c| c.addr.is_some())
    }

    /// Pretend a connection's socket went silent a minute ago (half-open).
    fn make_quiet(connection: &BridgeConnection) {
        *connection.last_traffic.lock().unwrap() = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .expect("monotonic clock is past one minute");
    }

    fn assert_refused(attempt: std::result::Result<TestSocket, WsError>, code: &str) -> String {
        match attempt {
            Err(WsError::Http(response)) => {
                assert_eq!(response.status().as_u16(), 409);
                let body = response
                    .body()
                    .as_ref()
                    .map(|body| String::from_utf8_lossy(body).to_string())
                    .unwrap_or_default();
                if !body.is_empty() {
                    assert!(body.contains(code), "expected {code} in {body}");
                }
                body
            },
            _ => panic!("the handshake must be refused with {code} before WebSocket OPEN"),
        }
    }

    fn assert_refused_full(attempt: std::result::Result<TestSocket, WsError>) {
        assert_refused(attempt, "bridge_full");
    }

    fn response_frame(request_id: &str) -> WsMessage {
        WsMessage::Text(
            json!({
                "type": "response",
                "data": { "requestId": request_id, "success": true, "result": { "ok": true } }
            })
            .to_string(),
        )
    }

    async fn next_request_id(socket: &mut TestSocket) -> String {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("request frame deadline")
                .expect("socket open")
                .expect("frame");
            if let WsMessage::Text(text) = frame {
                let envelope: serde_json::Value = serde_json::from_str(&text).unwrap();
                if envelope["type"] == "request" {
                    return envelope["data"]["requestId"].as_str().unwrap().to_string();
                }
            }
        }
    }

    fn request_with_id(request_id: &str) -> ExtensionRequest {
        ExtensionRequest {
            request_id: request_id.to_string(),
            action: "list_tabs".to_string(),
            params: json!({}),
        }
    }

    fn test_connection(
        session_id: u64,
        browser_id: &str,
        label: &str,
    ) -> (BridgeConnection, mpsc::UnboundedReceiver<BridgeOutbound>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            BridgeConnection {
                session_id,
                browser_id: browser_id.to_string(),
                fingerprint: browser_fingerprint(browser_id),
                label: label.to_string(),
                key_digest: Some(browser_key_digest(&test_key(browser_id))),
                accepted: true,
                routable_flag: Arc::new(AtomicBool::new(true)),
                stable_key_hash: None,
                connected_at: Utc::now(),
                last_traffic: Arc::new(Mutex::new(Instant::now())),
                tx,
                addr: None,
                pending: HashMap::new(),
            },
            rx,
        )
    }

    fn unique_thread(label: &str) -> String {
        format!("{label}-{}", uuid::Uuid::new_v4())
    }

    /// A hub whose upgrade window is open, so keyed test browsers are
    /// accepted on connect like browsers that predate pairing.
    fn test_hub() -> Arc<RwLock<BridgeHub>> {
        Arc::new(RwLock::new(BridgeHub::with_store(BrowserStore::in_memory(
            true,
        ))))
    }

    fn temp_store(label: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("magicutor-bridge-{label}-{}", uuid::Uuid::new_v4()))
            .join("bridge_browsers.json")
    }

    /// A store past its upgrade window: new browsers start pending.
    /// A store past its upgrade window that already knows another browser,
    /// so the first-run bootstrap does not apply: new browsers start pending.
    fn closed_store(path: &std::path::Path) -> BrowserStore {
        let mut store = BrowserStore::load(path);
        store.close_migration();
        let other = store.key_hash(&test_key("someone-else"));
        store.accept("someone-else", &other, "Other");
        store
    }

    fn identity(id: Option<&str>, label: &str) -> BrowserIdentity {
        BrowserIdentity {
            id: id.map(str::to_string),
            key: id.map(test_key),
            label: label.to_string(),
        }
    }

    /// Types of the server notices queued for a test connection.
    fn notices(rx: &mut mpsc::UnboundedReceiver<BridgeOutbound>) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Ok(message) = rx.try_recv() {
            if let BridgeOutbound::Envelope(envelope) = message {
                out.push(serde_json::to_value(&envelope).unwrap());
            }
        }
        out
    }

    async fn next_notice(socket: &mut TestSocket, kind: &str) -> serde_json::Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("notice deadline")
                .expect("socket open")
                .expect("frame");
            if let WsMessage::Text(text) = frame {
                let envelope: serde_json::Value = serde_json::from_str(&text).unwrap();
                if envelope["type"] == kind {
                    return envelope;
                }
            }
        }
    }

    fn pending_connection(
        session_id: u64,
        browser_id: &str,
        label: &str,
        store: &BrowserStore,
    ) -> (BridgeConnection, mpsc::UnboundedReceiver<BridgeOutbound>) {
        let (mut connection, rx) = test_connection(session_id, browser_id, label);
        connection.set_accepted(false);
        connection.stable_key_hash = Some(store.key_hash(&test_key(browser_id)));
        (connection, rx)
    }

    #[test]
    fn a_pending_browser_holds_a_slot_but_never_gets_work() {
        let mut hub = BridgeHub::default();
        let (pending, _pending_rx) = pending_connection(1, "new-id", "Comet", &hub.store);
        hub.connections.push(pending);
        let thread = unique_thread("pending");

        assert_eq!(
            hub.route_index(&BridgeRoute::Default),
            Err("Bridge not connected".to_string())
        );
        assert_eq!(hub.select("comet"), Err(BrowserSelectError::NoBrowsers));
        assert_eq!(hub.claim_thread(&thread, None), Ok(None));
        assert!(hub
            .route_index(&BridgeRoute::Thread(thread.clone()))
            .is_err());
        assert!(hub
            .route_index(&BridgeRoute::Browser("new-id".to_string()))
            .is_err());
        assert!(hub.thread_pins.is_empty());

        let (chrome, _chrome_rx) = test_connection(2, "chrome", "Chrome");
        hub.connections.push(chrome);
        assert_eq!(hub.route_index(&BridgeRoute::Default), Ok(1));
        assert_eq!(hub.route_index(&BridgeRoute::Thread(thread.clone())), Ok(1));
        assert!(matches!(
            hub.select("comet"),
            Err(BrowserSelectError::NotFound { .. })
        ));
        let status = hub.status(None);
        assert_eq!(status[0].state, "pending");
        assert!(!status[0].default);
        assert!(status[1].default);
        // It still holds a slot.
        let other = BrowserIdentity {
            id: Some("dia".to_string()),
            key: Some(test_key("dia")),
            label: "Dia".to_string(),
        };
        assert_eq!(
            hub.admission(&other, 2, REPLACE_QUIET_PERIOD),
            Admission::Full
        );
    }

    #[actix_web::test]
    async fn an_accept_in_the_panel_persists_across_a_restart() {
        let path = temp_store("accept");
        let hub = Arc::new(RwLock::new(BridgeHub::with_store(closed_store(&path))));
        let (url, handle) = spawn_bridge_server(hub.clone(), MAX_BRIDGE_CONNECTIONS);
        let mut comet = connect_browser(&url, "comet", "Comet").await.unwrap();
        wait_for(&hub, |h| all_registered(h, 1)).await;
        assert_eq!(
            next_notice(&mut comet, "browser_state").await["state"],
            "pending"
        );
        assert!(!hub.read().await.connections[0].accepted);

        comet
            .send(WsMessage::Text(
                r#"{"type":"browser_consent","accepted":true}"#.to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            next_notice(&mut comet, "browser_state").await["state"],
            "accepted"
        );
        wait_for(&hub, |h| h.connections[0].accepted).await;
        assert!(hub
            .read()
            .await
            .store
            .is_accepted("comet", &test_key("comet")));
        drop(comet);
        handle.stop(false).await;

        // A restart reloads the store: the same install is accepted at once,
        // the same id with another key is not.
        let restarted = Arc::new(RwLock::new(BridgeHub::with_store(BrowserStore::load(
            &path,
        ))));
        let (url, handle) = spawn_bridge_server(restarted.clone(), MAX_BRIDGE_CONNECTIONS);
        let mut again = connect_browser(&url, "comet", "Comet").await.unwrap();
        wait_for(&restarted, |h| all_registered(h, 1)).await;
        assert_eq!(
            next_notice(&mut again, "browser_state").await["state"],
            "accepted"
        );
        assert!(restarted.read().await.connections[0].accepted);
        again.close(None).await.unwrap();
        drop(again);
        wait_for(&restarted, |h| h.connections.is_empty()).await;
        let imposter = connect_with_key(&url, "comet", Some("other-key-0123456789"), "Comet")
            .await
            .unwrap();
        wait_for(&restarted, |h| all_registered(h, 1)).await;
        assert!(!restarted.read().await.connections[0].accepted);
        drop(imposter);
        handle.stop(false).await;
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[actix_web::test]
    async fn declining_in_the_panel_disconnects_the_browser() {
        let path = temp_store("decline");
        let hub = Arc::new(RwLock::new(BridgeHub::with_store(closed_store(&path))));
        let (url, handle) = spawn_bridge_server(hub.clone(), MAX_BRIDGE_CONNECTIONS);
        let mut dia = connect_browser(&url, "dia", "Dia").await.unwrap();
        wait_for(&hub, |h| all_registered(h, 1)).await;
        dia.send(WsMessage::Text(
            r#"{"type":"browser_consent","accepted":false}"#.to_string(),
        ))
        .await
        .unwrap();
        wait_for(&hub, |h| h.connections.is_empty()).await;
        // `closed_store` already accepts another browser; only Dia's own
        // decline is under test.
        assert!(!hub
            .read()
            .await
            .store
            .accepted()
            .iter()
            .any(|record| record.browser_id == "dia"));
        drop(dia);
        handle.stop(false).await;
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn remote_accept_and_revoke_change_only_that_browser() {
        let mut hub = BridgeHub::default();
        let (pending, mut pending_rx) = pending_connection(1, "comet", "Comet", &hub.store);
        hub.connections.push(pending);
        let (legacy, _legacy_rx) = test_connection(2, "legacy-browser-2", "Browser");
        hub.connections.push(legacy);
        let comet = browser_fingerprint("comet");

        assert_eq!(
            hub.accept_by_fingerprint("ffffffff"),
            Err(BrowserManageError::NotFound)
        );
        // A legacy build has no identity to persist; accepting it is
        // in-memory and idempotent.
        assert_eq!(
            hub.accept_by_fingerprint(&browser_fingerprint("legacy-browser-2"))
                .unwrap()
                .state,
            "legacy"
        );
        let accepted = hub.accept_by_fingerprint(&comet).unwrap();
        assert_eq!(accepted.state, "accepted");
        assert!(hub.store.is_accepted("comet", &test_key("comet")));
        assert_eq!(
            notices(&mut pending_rx).last().unwrap()["state"],
            "accepted"
        );
        assert_eq!(hub.select("comet"), Ok(0));

        let thread = unique_thread("revoke");
        assert_eq!(
            hub.claim_thread(&thread, Some("comet")),
            Ok(Some("comet".to_string()))
        );
        let revoked = hub.revoke_by_fingerprint(&comet).unwrap();
        assert_eq!(revoked.state, "pending");
        assert!(!hub.store.is_accepted("comet", &test_key("comet")));
        assert!(!hub.thread_pins.contains_key(&thread));
        assert_eq!(notices(&mut pending_rx).last().unwrap()["state"], "pending");
        let listed = hub.known_browsers();
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .all(|browser| !browser.fingerprint.contains("comet")));
    }

    #[test]
    fn automatic_acceptance_needs_a_loopback_peer_and_a_recordable_store() {
        // First run: the first identified loopback browser is accepted even
        // with the upgrade window closed; the next one is not.
        let mut hub = BridgeHub::default();
        let chrome = identity(Some("chrome"), "Chrome");
        let chrome_hash = hub.store.key_hash(&test_key("chrome"));
        let comet = identity(Some("comet"), "Comet");
        let comet_hash = hub.store.key_hash(&test_key("comet"));
        assert!(
            !hub.initial_acceptance(&chrome, Some(&chrome_hash), false, 4),
            "not from the LAN"
        );
        assert!(hub.initial_acceptance(&chrome, Some(&chrome_hash), true, 4));
        assert!(hub.store.accepted()[0].auto_accepted);
        assert!(!hub.initial_acceptance(&comet, Some(&comet_hash), true, 4));
        // Once accepted, the same install is accepted from anywhere the
        // bridge admits it (the token is the boundary there).
        assert!(hub.initial_acceptance(&chrome, Some(&chrome_hash), false, 4));
        // Legacy builds are pending once the upgrade window is over.
        assert!(!hub.initial_acceptance(&identity(None, "Browser"), None, true, 4));

        // Upgrade window: loopback only, at most `max_connections`.
        let mut window = BridgeHub::with_store(BrowserStore::in_memory(true));
        assert!(window.initial_acceptance(&identity(None, "Browser"), None, true, 4));
        let hashes: Vec<String> = ["a", "b", "c"]
            .iter()
            .map(|id| window.store.key_hash(&test_key(id)))
            .collect();
        assert!(window.initial_acceptance(&identity(Some("a"), "A"), Some(&hashes[0]), true, 2));
        assert!(!window.initial_acceptance(&identity(Some("b"), "B"), Some(&hashes[1]), false, 2));
        assert!(window.initial_acceptance(&identity(Some("b"), "B"), Some(&hashes[1]), true, 2));
        assert!(!window.initial_acceptance(&identity(Some("c"), "C"), Some(&hashes[2]), true, 2));
        assert_eq!(window.store.auto_accepted_count(), 2);
        // Keyless identified builds are never accepted automatically.
        let keyless = BrowserIdentity {
            id: Some("d".to_string()),
            key: None,
            label: "D".to_string(),
        };
        assert!(!window.initial_acceptance(&keyless, None, true, 8));
    }

    #[test]
    fn an_in_panel_accept_counts_only_from_a_loopback_peer() {
        let mut hub = BridgeHub::default();
        let (pending, _rx) = pending_connection(1, "comet", "Comet", &hub.store);
        hub.connections.push(pending);
        assert!(hub.apply_consent(1, true, false).is_none());
        assert!(
            !hub.connections[0].accepted,
            "a LAN peer cannot accept itself"
        );
        assert!(hub.apply_consent(1, true, true).is_none());
        assert!(hub.connections[0].accepted);
        assert!(hub.connections[0].routable_flag.load(Ordering::SeqCst));
        // Decline disconnects from anywhere.
        assert!(hub.apply_consent(1, false, false).is_some());
        assert!(hub.connections.is_empty());
    }

    #[test]
    fn a_pending_browser_may_only_send_consent_label_and_keepalives() {
        assert!(frame_allowed_while_pending(
            &BridgeEnvelope::BrowserConsent { accepted: true }
        ));
        assert!(frame_allowed_while_pending(&BridgeEnvelope::BrowserLabel {
            label: "x".to_string()
        }));
        assert!(frame_allowed_while_pending(&BridgeEnvelope::Ping {
            timestamp: 1
        }));
        assert!(!frame_allowed_while_pending(
            &BridgeEnvelope::BrowserChoiceAnswer {
                choice_id: "c".to_string(),
                always: true,
            }
        ));
        assert!(!frame_allowed_while_pending(&BridgeEnvelope::CdpEvent {
            data: CdpEventPayload {
                tab_id: 1,
                session_id: None,
                method: "Page.loadEventFired".to_string(),
                params: json!({}),
                browser_id: String::new(),
            },
        }));
        let signal: crate::types::AmbientPageSignal = Default::default();
        assert!(!frame_allowed_while_pending(
            &BridgeEnvelope::AmbientPageSignal { data: signal }
        ));
        assert!(!frame_allowed_while_pending(&BridgeEnvelope::Response {
            data: ExtensionResponse::success("r".to_string(), json!({})),
        }));
    }

    #[test]
    fn revoking_keeps_a_live_threads_pin_so_its_work_is_refused_not_moved() {
        let mut hub = BridgeHub::default();
        let (chrome, _chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (comet, _comet_rx) = test_connection(2, "comet", "Comet");
        hub.connections.push(chrome);
        hub.connections.push(comet);
        let live = unique_thread("revoke-live");
        let idle = unique_thread("revoke-idle");
        assert_eq!(
            hub.claim_thread(&live, Some("comet")),
            Ok(Some("comet".to_string()))
        );
        assert_eq!(
            hub.claim_thread(&idle, Some("comet")),
            Ok(Some("comet".to_string()))
        );
        note_thread_session(&live, true);
        hub.revoke_by_fingerprint(&browser_fingerprint("comet"))
            .unwrap();
        assert!(hub.thread_pins.contains_key(&live));
        assert!(!hub.thread_pins.contains_key(&idle));
        let error = hub
            .route_index(&BridgeRoute::Thread(live.clone()))
            .unwrap_err();
        assert!(error.contains("not connected"));
        note_thread_session(&live, false);
        // Once idle, it moves to the default browser like any other.
        assert_eq!(hub.route_index(&BridgeRoute::Thread(live)), Ok(0));
    }

    #[test]
    fn an_ambiguous_fingerprint_selects_and_manages_nothing() {
        let mut hub = BridgeHub::default();
        let (mut first, _first_rx) = test_connection(1, "one", "Chrome");
        let (mut second, _second_rx) = test_connection(2, "two", "Comet");
        first.fingerprint = "0123456789abcdef".to_string();
        second.fingerprint = "0123456789abcdef".to_string();
        hub.connections.push(first);
        hub.connections.push(second);
        assert!(matches!(
            hub.select("0123456789abcdef"),
            Err(BrowserSelectError::Ambiguous { .. })
        ));
        assert_eq!(
            hub.accept_by_fingerprint("0123456789abcdef"),
            Err(BrowserManageError::Ambiguous)
        );
        assert_eq!(
            hub.revoke_by_fingerprint("0123456789abcdef"),
            Err(BrowserManageError::Ambiguous)
        );
        // The full id still selects exactly.
        assert_eq!(hub.select("two"), Ok(1));
    }

    fn peer(addr: &str) -> HttpRequest {
        actix_web::test::TestRequest::default()
            .peer_addr(addr.parse().unwrap())
            .to_http_request()
    }

    fn with_origin(addr: &str, origin: &str) -> HttpRequest {
        actix_web::test::TestRequest::default()
            .peer_addr(addr.parse().unwrap())
            .insert_header(("origin", origin))
            .to_http_request()
    }

    #[test]
    fn without_a_token_only_trusted_peers_may_pair_or_choose() {
        let mut open = crate::MagicutorConfig::default();
        open.bridge.auth_token.clear();
        assert!(peer_is_trusted(&peer("127.0.0.1:5000"), Some(&open)));
        assert!(peer_is_trusted(&peer("[::1]:5000"), Some(&open)));
        assert!(peer_is_trusted(
            &peer("[::ffff:127.0.0.1]:5000"),
            Some(&open)
        ));
        assert!(!peer_is_trusted(&peer("192.168.1.20:5000"), Some(&open)));
        assert!(!peer_is_trusted(
            &actix_web::test::TestRequest::default().to_http_request(),
            None
        ));

        assert!(owner_request_ok(&peer("127.0.0.1:5000"), Some(&open)));
        assert!(!owner_request_ok(&peer("192.168.1.20:5000"), Some(&open)));
        // A configured gateway (published container port) is trusted.
        open.bridge.trusted_peers = vec!["192.168.64.1".to_string()];
        assert!(owner_request_ok(&peer("192.168.64.1:5000"), Some(&open)));
        assert!(!owner_request_ok(&peer("192.168.64.2:5000"), Some(&open)));

        // With a token, a remote owner must present it.
        let mut tokened = crate::MagicutorConfig::default();
        tokened.bridge.auth_token = "owner-secret".to_string();
        let remote_with_token = actix_web::test::TestRequest::default()
            .peer_addr("192.168.1.20:5000".parse().unwrap())
            .insert_header(("authorization", "Bearer owner-secret"))
            .to_http_request();
        assert!(owner_request_ok(&remote_with_token, Some(&tokened)));
        assert!(!owner_request_ok(
            &peer("192.168.1.20:5000"),
            Some(&tokened)
        ));
        let wrong_token = actix_web::test::TestRequest::default()
            .peer_addr("192.168.1.20:5000".parse().unwrap())
            .insert_header(("authorization", "Bearer guess"))
            .to_http_request();
        assert!(!owner_request_ok(&wrong_token, Some(&tokened)));
    }

    #[test]
    fn websites_are_refused_and_extensions_or_no_origin_allowed() {
        let mut open = crate::MagicutorConfig::default();
        open.bridge.auth_token.clear();
        assert!(origin_allowed(&peer("127.0.0.1:5000")));
        for origin in [
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop",
            "moz-extension://6f1d3c0e-0000-4000-8000-000000000000",
        ] {
            assert!(origin_allowed(&with_origin("127.0.0.1:5000", origin)));
            assert!(owner_request_ok(
                &with_origin("127.0.0.1:5000", origin),
                Some(&open)
            ));
        }
        for origin in [
            "https://evil.example",
            "http://localhost:5173",
            "null",
            "file://",
        ] {
            assert!(!origin_allowed(&with_origin("127.0.0.1:5000", origin)));
            // Not even from loopback or with the token.
            assert!(!owner_request_ok(
                &with_origin("127.0.0.1:5000", origin),
                Some(&open)
            ));
        }
    }

    fn with_host(addr: &str, host: &str) -> HttpRequest {
        actix_web::test::TestRequest::default()
            .peer_addr(addr.parse().unwrap())
            .insert_header(("host", host))
            .to_http_request()
    }

    #[test]
    fn a_rebinding_host_is_refused_even_from_loopback() {
        let mut open = crate::MagicutorConfig::default();
        open.bridge.auth_token.clear();
        for host in [
            "localhost:3003",
            "127.0.0.1:3003",
            "[::1]:3003",
            "LOCALHOST",
            "127.0.0.1",
        ] {
            assert!(
                owner_request_ok(&with_host("127.0.0.1:5000", host), Some(&open)),
                "{host}"
            );
        }
        for host in [
            "evil.example:3003",
            "attacker.localhost.evil:3003",
            "evil.example",
        ] {
            assert!(
                !owner_request_ok(&with_host("127.0.0.1:5000", host), Some(&open)),
                "{host}"
            );
        }
        // A configured name is allowed (e.g. a container service name).
        open.bridge.allowed_hosts = vec!["magicutor.internal".to_string()];
        assert!(owner_request_ok(
            &with_host("127.0.0.1:5000", "magicutor.internal:3003"),
            Some(&open)
        ));
        // The token makes any Host acceptable.
        let mut tokened = crate::MagicutorConfig::default();
        tokened.bridge.auth_token = "owner-secret".to_string();
        let request = actix_web::test::TestRequest::default()
            .peer_addr("192.168.1.20:5000".parse().unwrap())
            .insert_header(("host", "evil.example:3003"))
            .insert_header(("authorization", "Bearer owner-secret"))
            .to_http_request();
        assert!(owner_request_ok(&request, Some(&tokened)));
    }

    #[test]
    fn host_names_are_parsed_without_ports_or_brackets() {
        assert_eq!(host_name("LocalHost:3003"), "localhost");
        assert_eq!(host_name("[::1]:3003"), "::1");
        assert_eq!(host_name("[::1]"), "::1");
        assert_eq!(host_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(host_name("example.com."), "example.com");
        assert!(host_is_allowed("192.168.64.1:13003", &[], ""));
        assert!(host_is_allowed("box:3003", &[], "other, box"));
        assert!(!host_is_allowed("box:3003", &[], ""));
    }

    #[test]
    fn trusted_peer_entries_match_v4_and_v6_cidrs() {
        let configured = vec!["10.0.0.0/8".to_string(), "fd00::/8".to_string()];
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(addr_is_trusted(ip("10.20.30.40"), &configured, ""));
        assert!(!addr_is_trusted(ip("11.0.0.1"), &configured, ""));
        assert!(addr_is_trusted(ip("fd12::5"), &configured, ""));
        assert!(!addr_is_trusted(ip("fe80::1"), &configured, ""));
        assert!(addr_is_trusted(ip("::ffff:10.1.2.3"), &configured, ""));
        assert!(addr_is_trusted(ip("127.0.0.1"), &[], ""));
        assert!(addr_is_trusted(ip("::1"), &[], ""));
        // Environment entries: single addresses and lists.
        assert!(addr_is_trusted(ip("192.168.64.1"), &[], "192.168.64.1"));
        assert!(addr_is_trusted(
            ip("172.17.0.1"),
            &[],
            "192.168.64.1, 172.17.0.0/16"
        ));
        assert!(!addr_is_trusted(
            ip("172.18.0.1"),
            &[],
            "192.168.64.1, 172.17.0.0/16"
        ));
        // Malformed entries trust nothing.
        assert!(!addr_is_trusted(
            ip("10.0.0.1"),
            &["10.0.0.0/33".to_string()],
            "nonsense"
        ));
        assert!(addr_is_trusted(
            ip("8.8.8.8"),
            &["0.0.0.0/0".to_string()],
            ""
        ));
        assert_eq!(
            parse_peer_cidr("192.168.1.1"),
            Some((ip("192.168.1.1"), 32))
        );
        assert_eq!(parse_peer_cidr(""), None);
    }

    #[test]
    fn a_token_makes_a_remote_connection_trusted_for_pairing() {
        let mut hub = BridgeHub::default();
        let remote = identity(Some("remote"), "Remote");
        let hash = hub.store.key_hash(&test_key("remote"));
        // `trusted` is true for a remote peer that presented the configured
        // token (see bridge_route_with_hub).
        assert!(hub.initial_acceptance(&remote, Some(&hash), true, 4));
        let (pending, _rx) = pending_connection(7, "other", "Other", &hub.store);
        hub.connections.push(pending);
        assert!(hub.apply_consent(7, true, true).is_none());
        assert!(hub.connections[0].accepted);
    }

    #[test]
    fn revoking_the_only_browser_does_not_re_arm_the_bootstrap() {
        let mut hub = BridgeHub::default();
        let (chrome, _chrome_rx) = test_connection(1, "chrome", "Chrome");
        hub.connections.push(chrome);
        let chrome_id = identity(Some("chrome"), "Chrome");
        let chrome_hash = hub.store.key_hash(&test_key("chrome"));
        assert!(hub.initial_acceptance(&chrome_id, Some(&chrome_hash), true, 4));
        hub.revoke_by_fingerprint(&browser_fingerprint("chrome"))
            .unwrap();
        assert!(hub.store.accepted().is_empty());
        let comet = identity(Some("comet"), "Comet");
        let comet_hash = hub.store.key_hash(&test_key("comet"));
        assert!(!hub.initial_acceptance(&comet, Some(&comet_hash), true, 4));
    }

    #[test]
    fn an_untrusted_legacy_client_is_not_routable_even_in_the_window() {
        let mut window = BridgeHub::with_store(BrowserStore::in_memory(true));
        assert!(!window.initial_acceptance(&identity(None, "Browser"), None, false, 4));
        assert!(window.initial_acceptance(&identity(None, "Browser"), None, true, 4));
    }

    #[actix_web::test]
    async fn without_a_token_a_lan_peer_cannot_open_the_bridge() {
        let hub = test_hub();
        let route_hub = hub.clone();
        let mut config = crate::MagicutorConfig::default();
        config.bridge.auth_token.clear();
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(config))
                .route(
                    "/bridge",
                    web::get().to(move |req, stream, cfg| {
                        bridge_route_with_hub(req, stream, cfg, route_hub.clone())
                    }),
                )
                .route("/bridge/browsers", web::get().to(bridge_browsers_list))
                .route(
                    "/bridge/browsers/{fingerprint}/accept",
                    web::post().to(bridge_browser_accept),
                ),
        )
        .await;
        // A website the owner visits, connecting from loopback.
        let request = actix_web::test::TestRequest::get()
            .uri("/bridge?browser_id=site&browser_key=site-key-0123456789")
            .peer_addr("127.0.0.1:5000".parse().unwrap())
            .insert_header(("origin", "https://evil.example"))
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
        // A cross-site POST to accept a browser (CSRF) is refused.
        let request = actix_web::test::TestRequest::post()
            .uri("/bridge/browsers/0123456789abcdef/accept")
            .peer_addr("127.0.0.1:5000".parse().unwrap())
            .insert_header(("origin", "https://evil.example"))
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::UNAUTHORIZED);
        // The extension's own origin passes the gate (then: no such browser).
        let request = actix_web::test::TestRequest::post()
            .uri("/bridge/browsers/0123456789abcdef/accept")
            .peer_addr("127.0.0.1:5000".parse().unwrap())
            .insert_header((
                "origin",
                "chrome-extension://abcdefghijklmnopabcdefghijklmnop",
            ))
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::NOT_FOUND);
        let request = actix_web::test::TestRequest::get()
            .uri("/bridge?browser_id=lan&browser_key=lan-key-0123456789")
            .peer_addr("192.168.1.20:5000".parse().unwrap())
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
        let request = actix_web::test::TestRequest::get()
            .uri("/bridge/browsers")
            .peer_addr("192.168.1.20:5000".parse().unwrap())
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::UNAUTHORIZED);
        let request = actix_web::test::TestRequest::get()
            .uri("/bridge/browsers")
            .peer_addr("127.0.0.1:5000".parse().unwrap())
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert!(response.status().is_success());
        assert!(hub.read().await.connections.is_empty());
    }

    #[test]
    fn the_first_browser_card_answer_wins_and_always_sets_the_default() {
        let mut hub = BridgeHub::default();
        let (chrome, mut chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (comet, mut comet_rx) = test_connection(2, "comet", "Comet");
        hub.connections.push(chrome);
        hub.connections.push(comet);
        let thread = unique_thread("choice");
        assert!(hub.should_ask(&thread));

        let (choice_id, _rx) = hub.open_choice(&thread, "Book a table", 20).unwrap();
        for rx in [&mut chrome_rx, &mut comet_rx] {
            let sent = notices(rx);
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0]["type"], "browser_choice_request");
            assert_eq!(sent[0]["task"], "Book a table");
        }
        // A second connect for the same thread does not open another choice.
        assert!(hub.open_choice(&thread, "Book a table", 20).is_none());

        assert!(hub.answer_choice(&choice_id, 2, true));
        assert!(
            !hub.answer_choice(&choice_id, 1, false),
            "later answers are no-ops"
        );
        assert_eq!(hub.thread_pins[&thread].browser_id, "comet");
        assert_eq!(hub.store.always_browser(), Some("comet"));
        assert_eq!(hub.default_index(), Some(1));

        hub.close_choice(&choice_id);
        for rx in [&mut chrome_rx, &mut comet_rx] {
            assert_eq!(notices(rx)[0]["type"], "browser_choice_withdrawn");
        }
        assert!(
            !hub.should_ask(&thread),
            "a pinned thread is not asked again"
        );
    }

    #[test]
    fn nothing_is_asked_with_one_accepted_browser() {
        let mut hub = BridgeHub::default();
        let (chrome, _chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (pending, _pending_rx) = pending_connection(2, "comet", "Comet", &hub.store);
        hub.connections.push(chrome);
        hub.connections.push(pending);
        let thread = unique_thread("one-browser");
        assert!(!hub.should_ask(&thread));
        assert!(hub.open_choice(&thread, "task", 20).is_none());
    }

    #[actix_web::test]
    async fn an_unanswered_choice_goes_to_the_default_browser() {
        let hub = test_hub();
        let (chrome, _chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (comet, _comet_rx) = test_connection(2, "comet", "Comet");
        hub.write().await.connections.push(chrome);
        hub.write().await.connections.push(comet);
        let thread = unique_thread("timeout");
        let outcome =
            choose_with_hub(&hub, &thread, "Search flights", Duration::from_millis(50)).await;
        assert_eq!(outcome, BrowserChoiceOutcome::TimedOut);
        let guard = hub.read().await;
        assert_eq!(guard.thread_pins[&thread].browser_id, "chrome");
        assert!(guard.choices.is_empty());
    }

    #[actix_web::test]
    async fn starting_a_choice_sends_cards_returns_at_once_and_is_idempotent() {
        let hub = test_hub();
        let (chrome, mut chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (comet, mut comet_rx) = test_connection(2, "comet", "Comet");
        hub.write().await.connections.push(chrome);
        hub.write().await.connections.push(comet);
        let thread = unique_thread("start");

        let started = tokio::time::timeout(
            Duration::from_millis(500),
            start_choice_with_hub(
                hub.clone(),
                &thread,
                "Book a table",
                Duration::from_secs(30),
            ),
        )
        .await
        .expect("starting a choice does not wait for the answer");
        assert_eq!(started, ChoiceStart::Asked);
        for rx in [&mut chrome_rx, &mut comet_rx] {
            assert_eq!(notices(rx)[0]["type"], "browser_choice_request");
        }
        assert_eq!(
            start_choice_with_hub(
                hub.clone(),
                &thread,
                "Book a table",
                Duration::from_secs(30)
            )
            .await,
            ChoiceStart::AlreadyOpen
        );
        assert!(
            notices(&mut chrome_rx).is_empty(),
            "no second card while one is open"
        );

        // The owner answers in Magician: the pin closes the choice everywhere.
        assert_eq!(
            hub.write().await.claim_thread(&thread, Some("comet")),
            Ok(Some("comet".to_string()))
        );
        wait_for(&hub, |h| h.choices.is_empty()).await;
        for rx in [&mut chrome_rx, &mut comet_rx] {
            assert_eq!(notices(rx)[0]["type"], "browser_choice_withdrawn");
        }
        assert_eq!(hub.read().await.thread_pins[&thread].browser_id, "comet");
        // A pinned thread is not asked again.
        assert_eq!(
            start_choice_with_hub(
                hub.clone(),
                &thread,
                "Book a table",
                Duration::from_secs(30)
            )
            .await,
            ChoiceStart::NotAsked
        );
    }

    #[actix_web::test]
    async fn a_started_choice_that_nobody_answers_pins_the_default_browser() {
        let hub = test_hub();
        let (chrome, _chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (comet, _comet_rx) = test_connection(2, "comet", "Comet");
        hub.write().await.connections.push(chrome);
        hub.write().await.connections.push(comet);
        let thread = unique_thread("start-timeout");
        assert_eq!(
            start_choice_with_hub(hub.clone(), &thread, "Pay", Duration::from_millis(50)).await,
            ChoiceStart::Asked
        );
        wait_for(&hub, |h| {
            h.choices.is_empty() && h.thread_pins.contains_key(&thread)
        })
        .await;
        assert_eq!(hub.read().await.thread_pins[&thread].browser_id, "chrome");
    }

    #[actix_web::test]
    async fn an_answer_elsewhere_releases_the_waiting_connect() {
        let hub = test_hub();
        let (chrome, _chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (comet, _comet_rx) = test_connection(2, "comet", "Comet");
        hub.write().await.connections.push(chrome);
        hub.write().await.connections.push(comet);
        let thread = unique_thread("release");
        let waiting = {
            let hub = hub.clone();
            let thread = thread.clone();
            actix_web::rt::spawn(async move {
                choose_with_hub(&hub, &thread, "Pay a bill", Duration::from_secs(5)).await
            })
        };
        wait_for(&hub, |h| !h.choices.is_empty()).await;
        {
            // Pinning the thread, by any route, settles its open choice.
            let mut guard = hub.write().await;
            assert_eq!(
                guard.claim_thread(&thread, Some("comet")),
                Ok(Some("comet".to_string()))
            );
            assert!(
                !guard.release_choice_for_thread(&thread),
                "the pin already released it"
            );
        }
        let outcome = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome, BrowserChoiceOutcome::ChosenByOwner);
        assert_eq!(hub.read().await.thread_pins[&thread].browser_id, "comet");
    }

    #[actix_web::test]
    async fn four_browsers_share_the_bridge_and_a_fifth_is_refused_before_open() {
        let hub = test_hub();
        let (url, handle) = spawn_bridge_server(hub.clone(), MAX_BRIDGE_CONNECTIONS);

        // Real concurrent WebSocket upgrades exercise admission before OPEN.
        let (work, home, comet, dia) = tokio::join!(
            connect_browser(&url, "chrome-work", "Chrome"),
            connect_browser(&url, "chrome-home", "Chrome"),
            connect_browser(&url, "comet", "Comet"),
            connect_browser(&url, "dia", "Dia")
        );
        let mut browsers = vec![work.unwrap(), home.unwrap(), comet.unwrap(), dia.unwrap()];
        wait_for(&hub, |h| all_registered(h, 4)).await;
        let (pending, mut response) = oneshot::channel();
        hub.write().await.connections[0]
            .pending
            .insert("in-flight".to_string(), pending);

        let (edge, brave) = tokio::join!(
            connect_browser(&url, "edge", "Edge"),
            connect_browser(&url, "brave", "Brave")
        );
        let body = assert_refused(edge, "bridge_full");
        assert!(
            !body.contains("chrome-work"),
            "refusals name browsers by fingerprint"
        );
        assert_refused_full(brave);
        assert_eq!(hub.read().await.connections.len(), 4);
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        // Every admitted browser is live: each answers its own ping.
        for socket in browsers.iter_mut() {
            socket
                .send(WsMessage::Text(
                    r#"{"type":"ping","timestamp":7}"#.to_string(),
                ))
                .await
                .unwrap();
            loop {
                let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                if let WsMessage::Text(text) = frame {
                    let envelope: serde_json::Value = serde_json::from_str(&text).unwrap();
                    // The pairing-state notice arrives first on a new socket.
                    if envelope["type"] == "pong" {
                        break;
                    }
                }
            }
        }

        // A browser leaving frees exactly its own slot.
        let mut leaving = browsers.pop().unwrap();
        leaving.close(None).await.unwrap();
        drop(leaving);
        wait_for(&hub, |h| h.connections.len() == 3).await;
        let edge = connect_browser(&url, "edge", "Edge").await.unwrap();
        wait_for(&hub, |h| all_registered(h, 4)).await;
        assert_refused_full(connect_browser(&url, "brave", "Brave").await);
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let status = hub.read().await.status(None);
        assert_eq!(status.len(), 4);
        assert!(status[0].default);
        assert_eq!(status[3].id, "edge");
        assert_eq!(status[3].slot, 4);

        drop(edge);
        drop(browsers);
        handle.stop(false).await;
    }

    #[actix_web::test]
    async fn a_browser_cannot_answer_or_cancel_another_browsers_requests() {
        let hub = test_hub();
        let (url, handle) = spawn_bridge_server(hub.clone(), MAX_BRIDGE_CONNECTIONS);
        let mut chrome = connect_browser(&url, "chrome", "Chrome").await.unwrap();
        let mut comet = connect_browser(&url, "comet", "Comet").await.unwrap();
        wait_for(&hub, |h| all_registered(h, 2)).await;

        let chrome_hub = hub.clone();
        let chrome_call = actix_web::rt::spawn(async move {
            send_routed(
                &chrome_hub,
                &BridgeRoute::Browser("chrome".to_string()),
                request_with_id("for-chrome"),
            )
            .await
        });
        let comet_hub = hub.clone();
        let comet_call = actix_web::rt::spawn(async move {
            send_routed(
                &comet_hub,
                &BridgeRoute::Browser("comet".to_string()),
                request_with_id("for-comet"),
            )
            .await
        });
        assert_eq!(next_request_id(&mut chrome).await, "for-chrome");
        assert_eq!(next_request_id(&mut comet).await, "for-comet");

        // Comet answers Chrome's request id: it is not Comet's to answer.
        comet.send(response_frame("for-chrome")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(hub.read().await.connections[0]
            .pending
            .contains_key("for-chrome"));
        assert!(!chrome_call.is_finished());

        // Comet disconnecting fails only Comet's own work.
        comet.close(None).await.unwrap();
        drop(comet);
        let comet_result = tokio::time::timeout(Duration::from_secs(2), comet_call)
            .await
            .unwrap()
            .unwrap();
        assert!(
            comet_result.is_err(),
            "a disconnect fails that browser's work"
        );
        wait_for(&hub, |h| h.connections.len() == 1).await;
        assert!(hub.read().await.connections[0]
            .pending
            .contains_key("for-chrome"));

        chrome.send(response_frame("for-chrome")).await.unwrap();
        let routed = tokio::time::timeout(Duration::from_secs(2), chrome_call)
            .await
            .unwrap()
            .unwrap()
            .expect("the owning browser answers its own request");
        assert_eq!(routed.browser_id, "chrome");
        assert!(routed.response.success);

        drop(chrome);
        handle.stop(false).await;
    }

    #[actix_web::test]
    async fn a_same_browser_reconnect_replaces_only_its_own_quiet_socket() {
        let hub = test_hub();
        let (url, handle) = spawn_bridge_server(hub.clone(), 2);
        // The stale socket stays open and silent: a half-open bridge.
        let stale_chrome = connect_browser(&url, "chrome", "Chrome").await.unwrap();
        let comet = connect_browser(&url, "comet", "Comet").await.unwrap();
        wait_for(&hub, |h| all_registered(h, 2)).await;
        let stale_session = hub.read().await.connections[0].session_id;
        let thread = unique_thread("reconnect");
        let (chrome_pending, chrome_response) = oneshot::channel();
        let (comet_pending, mut comet_response) = oneshot::channel();
        {
            let mut guard = hub.write().await;
            guard.connections[0]
                .pending
                .insert("chrome-in-flight".to_string(), chrome_pending);
            guard.connections[1]
                .pending
                .insert("comet-in-flight".to_string(), comet_pending);
            assert_eq!(
                guard.claim_thread(&thread, Some("chrome")),
                Ok(Some("chrome".to_string()))
            );
            make_quiet(&guard.connections[0]);
        }

        // All slots are full, yet the same browser is admitted at once.
        let fresh_chrome = connect_browser(&url, "chrome", "Chrome%20Work")
            .await
            .expect("a same-browser reconnect is admitted while its quiet socket is open");
        wait_for(&hub, |h| {
            all_registered(h, 2) && h.connections[0].session_id != stale_session
        })
        .await;
        assert!(
            tokio::time::timeout(Duration::from_secs(2), chrome_response)
                .await
                .unwrap()
                .unwrap()
                .is_err(),
            "the replaced socket's own work fails"
        );
        assert!(matches!(
            comet_response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        {
            let mut guard = hub.write().await;
            assert_eq!(guard.connections[0].browser_id, "chrome");
            assert_eq!(guard.connections[0].label, "Chrome Work");
            assert_eq!(
                guard.route_index(&BridgeRoute::Thread(thread.clone())),
                Ok(0)
            );
        }
        // A different browser is still refused when the slots are full.
        assert_refused_full(connect_browser(&url, "dia", "Dia").await);

        // The stale socket closing later cannot remove its replacement.
        let fresh_session = hub.read().await.connections[0].session_id;
        drop(stale_chrome);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(hub.read().await.connections.len(), 2);
        assert_eq!(hub.read().await.connections[0].session_id, fresh_session);

        drop(fresh_chrome);
        drop(comet);
        handle.stop(false).await;
    }

    #[actix_web::test]
    async fn knowing_a_browser_id_is_not_enough_to_take_over_its_connection() {
        let hub = test_hub();
        let (url, handle) = spawn_bridge_server(hub.clone(), MAX_BRIDGE_CONNECTIONS);
        let victim = connect_browser(&url, "victim-id", "Chrome").await.unwrap();
        wait_for(&hub, |h| all_registered(h, 1)).await;
        let victim_session = hub.read().await.connections[0].session_id;
        let (pending, mut response) = oneshot::channel();
        hub.write().await.connections[0]
            .pending
            .insert("victim-in-flight".to_string(), pending);

        // No key, a wrong key, and the right key while the socket is active
        // (a copied profile) are all refused.
        let body = assert_refused(
            connect_with_key(&url, "victim-id", None, "Chrome").await,
            "browser_id_in_use",
        );
        assert!(
            !body.contains("victim-id"),
            "the refusal does not echo the id"
        );
        assert_refused(
            connect_with_key(&url, "victim-id", Some("wrong-key-0123456789"), "Chrome").await,
            "browser_id_in_use",
        );
        assert_refused(
            connect_browser(&url, "victim-id", "Chrome").await,
            "browser_id_in_use",
        );
        // Even once the victim is quiet, a wrong key gets nothing.
        make_quiet(&hub.read().await.connections[0]);
        assert_refused(
            connect_with_key(&url, "victim-id", Some("wrong-key-0123456789"), "Chrome").await,
            "browser_id_in_use",
        );

        let guard = hub.read().await;
        assert_eq!(guard.connections.len(), 1);
        assert_eq!(guard.connections[0].session_id, victim_session);
        drop(guard);
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        drop(victim);
        handle.stop(false).await;
    }

    #[actix_web::test]
    async fn extensions_without_an_identity_never_replace_or_inherit_from_each_other() {
        let hub = test_hub();
        let (url, handle) = spawn_bridge_server(hub.clone(), MAX_BRIDGE_CONNECTIONS);
        let (first, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (second, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        wait_for(&hub, |h| all_registered(h, 2)).await;
        let thread = unique_thread("legacy");
        {
            let mut guard = hub.write().await;
            let first_id = guard.connections[0].browser_id.clone();
            assert!(is_legacy_browser(&first_id));
            assert!(is_legacy_browser(&guard.connections[1].browser_id));
            assert_ne!(first_id, guard.connections[1].browser_id);
            assert_eq!(guard.connections[0].label, DEFAULT_BROWSER_LABEL);
            // Legacy browsers carry no stable identity: threads that reach
            // them follow the default browser and are never pinned.
            assert_eq!(
                guard.route_index(&BridgeRoute::Thread(thread.clone())),
                Ok(0)
            );
            assert_eq!(
                guard.claim_thread(&thread, Some(first_id.as_str())),
                Ok(Some(first_id.clone()))
            );
            assert!(guard.thread_pins.is_empty());
        }
        drop(first);
        drop(second);
        handle.stop(false).await;
    }

    #[actix_web::test]
    async fn failed_upgrades_do_not_reserve_the_bridge_and_auth_is_still_required() {
        let hub = test_hub();
        let route_hub = hub.clone();
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(crate::MagicutorConfig::default()))
                .route(
                    "/bridge",
                    web::get().to(move |req, stream, cfg| {
                        bridge_route_with_hub(req, stream, cfg, route_hub.clone())
                    }),
                ),
        )
        .await;
        let request = actix_web::test::TestRequest::get()
            .uri("/bridge")
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::UNAUTHORIZED);
        let request = actix_web::test::TestRequest::get()
            .uri("/bridge?token=magicutor-bridge-dev-token&browser_id=chrome")
            .to_request();
        let response = actix_web::test::call_service(&app, request).await;
        assert!(response.status().is_client_error());
        for bad in ["browser_id=..%2Fetc", "browser_id=chrome&browser_key=short"] {
            let request = actix_web::test::TestRequest::get()
                .uri(&format!("/bridge?token=magicutor-bridge-dev-token&{bad}"))
                .to_request();
            let response = actix_web::test::call_service(&app, request).await;
            assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
        }
        assert!(hub.read().await.connections.is_empty());
    }

    #[test]
    fn admission_replaces_only_on_the_right_key_after_a_quiet_period() {
        let mut hub = BridgeHub::default();
        hub.connections
            .push(test_connection(1, "chrome", "Chrome").0);
        let same = |key: Option<String>| BrowserIdentity {
            id: Some("chrome".to_string()),
            key,
            label: "Chrome".to_string(),
        };
        let right = Some(test_key("chrome"));
        let wrong = Some("wrong-key-0123456789".to_string());
        assert_eq!(
            hub.admission(&same(right.clone()), 4, REPLACE_QUIET_PERIOD),
            Admission::IdInUse
        );
        make_quiet(&hub.connections[0]);
        assert_eq!(
            hub.admission(&same(right.clone()), 4, REPLACE_QUIET_PERIOD),
            Admission::Replace(0)
        );
        assert_eq!(
            hub.admission(&same(wrong), 4, REPLACE_QUIET_PERIOD),
            Admission::IdInUse
        );
        assert_eq!(
            hub.admission(&same(None), 4, REPLACE_QUIET_PERIOD),
            Admission::IdInUse
        );
        // A connection that sent no key can never be replaced.
        hub.connections[0].key_digest = None;
        assert_eq!(
            hub.admission(&same(right), 4, REPLACE_QUIET_PERIOD),
            Admission::IdInUse
        );
        let other = BrowserIdentity {
            id: Some("comet".to_string()),
            key: Some(test_key("comet")),
            label: "Comet".to_string(),
        };
        assert_eq!(
            hub.admission(&other, 2, REPLACE_QUIET_PERIOD),
            Admission::New
        );
        assert_eq!(
            hub.admission(&other, 1, REPLACE_QUIET_PERIOD),
            Admission::Full
        );
    }

    #[test]
    fn threads_pin_on_first_use_and_move_only_when_idle_and_their_browser_is_gone() {
        let mut hub = BridgeHub::default();
        let (chrome, _chrome_rx) = test_connection(1, "chrome", "Chrome");
        let (comet, _comet_rx) = test_connection(2, "comet", "Comet");
        hub.connections.push(chrome);
        hub.connections.push(comet);
        let default_thread = unique_thread("t-default");
        let comet_thread = unique_thread("t-comet");
        let comet_route = BridgeRoute::Thread(comet_thread.clone());

        assert_eq!(
            hub.route_index(&BridgeRoute::Thread(default_thread.clone())),
            Ok(0)
        );
        assert_eq!(
            hub.claim_thread(&comet_thread, Some("comet")),
            Ok(Some("comet".to_string()))
        );
        assert_eq!(hub.route_index(&comet_route), Ok(1));
        // Exact re-selection, or a selector that resolves to the pinned
        // browser, is fine; one that resolves elsewhere is refused.
        assert_eq!(
            hub.claim_thread(&comet_thread, Some("COMET")),
            Ok(Some("comet".to_string()))
        );
        assert_eq!(
            hub.claim_thread(&comet_thread, Some("com")),
            Ok(Some("comet".to_string()))
        );
        for other in ["chrome", "ch"] {
            assert!(matches!(
                hub.claim_thread(&comet_thread, Some(other)),
                Err(BrowserSelectError::PinnedElsewhere { .. })
            ));
        }

        // Comet leaves while a CDP session runs on its thread: the thread is
        // refused with a clear reason and is not moved.
        note_thread_session(&comet_thread, true);
        hub.connections.remove(1);
        let error = hub.route_index(&comet_route).unwrap_err();
        assert!(error.contains("Comet") && error.contains("not connected"));
        assert!(
            !error.contains("comet]"),
            "errors name the fingerprint, not the id"
        );
        assert_eq!(hub.predicted_thread_browser(&comet_thread), None);
        assert!(matches!(
            hub.claim_thread(&comet_thread, Some("chrome")),
            Err(BrowserSelectError::PinnedElsewhere { .. })
        ));
        assert_eq!(
            hub.claim_thread(&comet_thread, None),
            Ok(Some("comet".to_string()))
        );

        // Comet reconnecting under its own identity re-binds the thread.
        let (comet_again, _comet_again_rx) = test_connection(3, "comet", "Comet");
        hub.connections.push(comet_again);
        assert_eq!(hub.route_index(&comet_route), Ok(1));

        // Once the session ends and Comet is gone, the idle thread moves to
        // the default browser instead of being refused forever.
        note_thread_session(&comet_thread, false);
        hub.connections.remove(1);
        assert_eq!(
            hub.predicted_thread_browser(&comet_thread),
            Some("chrome".to_string())
        );
        assert_eq!(hub.route_index(&comet_route), Ok(0));
        assert_eq!(hub.thread_pins[&comet_thread].browser_id, "chrome");

        // An idle thread whose browser is gone also follows a new selector.
        let (dia, _dia_rx) = test_connection(4, "dia", "Dia");
        hub.connections.push(dia);
        let dia_thread = unique_thread("t-dia");
        assert_eq!(
            hub.claim_thread(&dia_thread, Some("dia")),
            Ok(Some("dia".to_string()))
        );
        hub.connections.remove(1);
        let (edge, _edge_rx) = test_connection(5, "edge", "Edge");
        hub.connections.push(edge);
        assert_eq!(
            hub.claim_thread(&dia_thread, Some("edge")),
            Ok(Some("edge".to_string()))
        );

        let status = hub.status(None);
        assert_eq!(status[0].pinned_threads, 2);
        assert_eq!(status[1].pinned_threads, 1);
        assert!(status[0].default && !status[1].default);
    }

    #[test]
    fn a_pin_conflict_needs_an_exact_match_not_a_prefix() {
        let pin = ThreadPin {
            browser_id: "install-1".to_string(),
            fingerprint: browser_fingerprint("install-1"),
            label: "Chrome Work".to_string(),
        };
        assert!(pin.matches_exactly("install-1"));
        assert!(pin.matches_exactly(&browser_fingerprint("install-1")));
        assert!(pin.matches_exactly(" chrome work "));
        assert!(!pin.matches_exactly("chrome"));
        assert!(!pin.matches_exactly("c"));
    }

    #[test]
    fn browser_selectors_match_id_or_fingerprint_then_label_then_unique_prefix() {
        let mut hub = BridgeHub::default();
        assert_eq!(hub.select("chrome"), Err(BrowserSelectError::NoBrowsers));
        for (session_id, id, label) in [
            (1, "id-work", "Chrome Work"),
            (2, "id-home", "Chrome Home"),
            (3, "id-comet", "Comet"),
        ] {
            hub.connections
                .push(test_connection(session_id, id, label).0);
        }
        assert_eq!(hub.select("id-home"), Ok(1));
        assert_eq!(hub.select(&browser_fingerprint("id-work")), Ok(0));
        assert_eq!(hub.select("comet"), Ok(2));
        assert_eq!(hub.select(" Chrome home "), Ok(1));
        assert_eq!(hub.select("com"), Ok(2));
        match hub.select("chrome") {
            Err(error @ BrowserSelectError::Ambiguous { .. }) => {
                let message = error.to_string();
                assert!(message.contains(&browser_fingerprint("id-work")));
                assert!(
                    !message.contains("id-work"),
                    "errors never show install ids"
                );
            },
            other => panic!("expected an ambiguous selector, got {other:?}"),
        }
        assert!(matches!(
            hub.select("dia"),
            Err(BrowserSelectError::NotFound { .. })
        ));
    }

    #[test]
    fn health_status_names_browsers_by_fingerprint_and_marks_the_caller() {
        let mut hub = BridgeHub::default();
        hub.connections
            .push(test_connection(1, "id-work", "Chrome").0);
        hub.connections
            .push(test_connection(2, "id-home", "Chrome").0);
        let status = serde_json::to_value(hub.status(Some("id-home"))).unwrap();
        let rendered = status.to_string();
        assert!(!rendered.contains("id-work") && !rendered.contains("id-home"));
        assert_eq!(
            status[0]["fingerprint"],
            json!(browser_fingerprint("id-work"))
        );
        assert_eq!(status[0]["this_browser"], json!(false));
        assert_eq!(status[1]["this_browser"], json!(true));
    }

    #[test]
    fn cdp_events_reach_only_subscribers_of_the_browser_that_sent_them() {
        let chrome = format!("chrome-{}", uuid::Uuid::new_v4());
        let comet = format!("comet-{}", uuid::Uuid::new_v4());
        let chrome_hits = Arc::new(AtomicU64::new(0));
        let comet_hits = Arc::new(AtomicU64::new(0));
        let chrome_handle = {
            let hits = chrome_hits.clone();
            let expected = chrome.clone();
            subscribe_cdp_events(&chrome, 7, move |payload| {
                assert_eq!(payload.browser_id, expected);
                hits.fetch_add(1, Ordering::SeqCst);
                true
            })
        };
        let _comet_handle = {
            let hits = comet_hits.clone();
            subscribe_cdp_events(&comet, 7, move |_| {
                hits.fetch_add(1, Ordering::SeqCst);
                true
            })
        };
        // Same tab id in both browsers; the payload claims to be Comet's.
        let event = CdpEventPayload {
            tab_id: 7,
            session_id: None,
            method: "Page.loadEventFired".to_string(),
            params: json!({}),
            browser_id: comet.clone(),
        };

        assert_eq!(dispatch_cdp_event(&chrome, event.clone()), 1);
        assert_eq!(chrome_hits.load(Ordering::SeqCst), 1);
        assert_eq!(comet_hits.load(Ordering::SeqCst), 0);
        assert_eq!(dispatch_cdp_event(&comet, event.clone()), 1);
        assert_eq!(comet_hits.load(Ordering::SeqCst), 1);

        drop(chrome_handle);
        assert_eq!(dispatch_cdp_event(&chrome, event), 0);
        assert_eq!(chrome_hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_thread_counts_as_live_while_any_cdp_session_is_open_on_it() {
        let thread = unique_thread("live-sessions");
        assert!(!thread_has_live_session(&thread));
        note_thread_session(&thread, true);
        assert!(!thread_has_other_live_session(&thread));
        note_thread_session(&thread, true);
        assert!(thread_has_other_live_session(&thread));
        note_thread_session(&thread, false);
        assert!(thread_has_live_session(&thread));
        assert!(!thread_has_other_live_session(&thread));
        note_thread_session(&thread, false);
        assert!(!thread_has_live_session(&thread));
        // A stray close never underflows into a phantom session.
        note_thread_session(&thread, false);
        assert!(!thread_has_live_session(&thread));
    }

    #[test]
    fn handshake_identity_is_validated_and_labels_are_sanitized() {
        assert_eq!(
            browser_identity_from_query(
                "token=t&browser_id=abc-123&browser_key=k3y-0123456789abcdef&browser_label=Comet%20Work"
            ),
            Ok(BrowserIdentity {
                id: Some("abc-123".to_string()),
                key: Some("k3y-0123456789abcdef".to_string()),
                label: "Comet Work".to_string(),
            })
        );
        let legacy = browser_identity_from_query("token=t").unwrap();
        assert_eq!(legacy.id, None);
        assert_eq!(legacy.key, None);
        assert_eq!(legacy.label, DEFAULT_BROWSER_LABEL);
        assert_eq!(
            browser_identity_from_query("browser_id=..%2Fetc"),
            Err("invalid_browser_id")
        );
        assert_eq!(
            browser_identity_from_query("browser_id=legacy-browser-7"),
            Err("invalid_browser_id")
        );
        assert_eq!(
            browser_identity_from_query("browser_id=abc&browser_key=short"),
            Err("invalid_browser_key")
        );
        assert_eq!(sanitize_browser_label("\n Dia\u{7} "), "Dia");
        assert_eq!(sanitize_browser_label("   "), DEFAULT_BROWSER_LABEL);
        assert_eq!(
            sanitize_browser_label(&"x".repeat(200)).chars().count(),
            MAX_BROWSER_LABEL_CHARS
        );
        assert_eq!(browser_fingerprint("abc-123").len(), 16);
        assert_eq!(
            browser_fingerprint("abc-123"),
            browser_fingerprint("abc-123")
        );
        assert_ne!(
            browser_fingerprint("abc-123"),
            browser_fingerprint("abc-124")
        );
    }

    fn bridge_request(action: &str) -> ExtensionRequest {
        ExtensionRequest {
            request_id: "test-request".to_string(),
            action: action.to_string(),
            params: json!({}),
        }
    }

    #[test]
    fn debugger_commands_do_not_wait_for_default_bridge_timeout() {
        assert_eq!(bridge_timeout_secs(&bridge_request("debugger_command")), 75);
    }

    #[test]
    fn non_browser_actions_keep_existing_default_timeout() {
        assert_eq!(bridge_timeout_secs(&bridge_request("api_replay")), 600);
    }

    #[test]
    fn target_discovery_does_not_exceed_agent_browser_connect_timeout() {
        assert_eq!(bridge_timeout_secs(&bridge_request("list_tabs")), 10);
    }
}
