//! `AgentBrowserSession` — subprocess driver for the `agent-browser` CLI.
//!
//! Each session spawns short-lived `agent-browser --session <id> <command>`
//! processes. The session lazy-connects on first use:
//! - CDP mode: `agent-browser --session <id> connect <url>` against the
//!   magicutor CDP proxy.
//! - `Headed` / `Headless` modes: `agent-browser --session <id> open
//!   <initial-url|about:blank> [--headed]` to start a fresh browser.
//!
//! Tested with the patched fork (e.g. `0.38.1-Magician.0`) shipped at
//! `skillshub/browser/_vendor/v<version>/bin/` and codesigned + deployed
//! by `make setup-agent-browser`. `make -C skillshub install-scope
//! SCOPE=<principal>/<workspace>` mirrors the patched binary into the
//! workspace skills tree at
//! `$MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/browser/bin/
//! agent-browser`, which is where this session resolves the CLI from. Extras
//! declared in `tool-runtime-config.yaml :: registry.paths` provide an
//! alternate source when a workspace install isn't present.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::OnceCell;
use tokio::time::timeout;
use tokio::time::Instant as TokioInstant;
use tracing::{debug, info, warn};

use crate::magician_v2::browser_engine_analytics::{
    sanitize_browser_analytics_url, BrowserEngineAnalyticsContext, BrowserEngineUsageInput,
};

/// Default WebSocket URL for the magicutor CDP proxy. Used when the agent
/// connection mode is CDP and no override is provided.
pub const DEFAULT_MAGICUTOR_PROXY_URL: &str =
    "ws://127.0.0.1:3003/devtools/browser/magicutor-proxy";

/// Environment variable to override the CLI path only for detached/test usage
/// where no storage root is available. Runtime browser execution receives a
/// storage root and must use the pinned vendored binary from that root.
pub const ENV_AGENT_BROWSER_CLI: &str = "MAGICIAN_AGENT_BROWSER_CLI";

/// Environment variable to force browser connection mode.
///
/// Accepted values: `cdp`, `headed`, `headless`.
pub const ENV_AGENT_BROWSER_MODE: &str = "MAGICIAN_AGENT_BROWSER_MODE";

/// Environment variable to override the Magicutor CDP proxy URL in CDP mode.
pub const ENV_AGENT_BROWSER_CDP_URL: &str = "MAGICIAN_AGENT_BROWSER_CDP_URL";

/// Relative path of the patched agent-browser CLI inside a skills
/// root. Appended to either the scope's `<scope>/skills/` or an
/// extras root's `<extra>/skills/` (declared via
/// `tool-runtime-config.yaml :: registry.paths`) when resolving the
/// runtime binary. The binary is the patched fork (e.g.
/// `0.38.1-Magician.0`), not the vanilla npm release.
const BROWSER_BIN_RELATIVE_PATH: &str = "browser/bin/agent-browser";

/// Upstream headless daemons default to a one-hour idle shutdown.
/// Magician owns browser-session lifecycle explicitly, so preserve the prior
/// no-timeout behavior unless an engine resolver or operator opts in.
const AGENT_BROWSER_IDLE_TIMEOUT_ENV: &str = "AGENT_BROWSER_IDLE_TIMEOUT_MS";

fn should_disable_upstream_idle_timeout(
    engine_env: &HashMap<String, String>,
    inherited_override_present: bool,
) -> bool {
    !inherited_override_present && !engine_env.contains_key(AGENT_BROWSER_IDLE_TIMEOUT_ENV)
}

/// Workspace-layer path. Resolved when the runtime knows the active
/// principal+workspace; takes precedence over extras-layer paths,
/// mirroring the SkillLoader's workspace-first contract.
fn scope_vendored_relative_path(principal: &str, workspace: &str) -> String {
    format!("scopes/{principal}/{workspace}/skills/{BROWSER_BIN_RELATIVE_PATH}")
}

/// Per-command wall-clock cap. Most browser primitive invocations finish in well under
/// a second; this is a generous safety net for `wait`-style commands.
pub const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;

/// Config name used in logs/receipts when agent-browser's own Chrome for
/// Testing is selected explicitly as the capacity fallback.
pub const BUNDLED_BROWSER_ENGINE_NAME: &str = "bundled_chrome";

/// Engine whose free/current binary has a server-enforced concurrent-session
/// allowance. Only this engine is eligible for the narrow exit-code-76 retry.
const CLOAK_BROWSER_ENGINE_NAME: &str = "cloak-browser";

/// Native headless DOM engine supported by the pinned agent-browser CLI.
/// Magician drives it through CDP; Lightpanda's own Agent/LLM mode is never
/// started.
pub const LIGHTPANDA_BROWSER_ENGINE_NAME: &str = "lightpanda";

/// How the inner-loop session reaches a Chrome instance.
///
/// Selected once at session creation from the outer browser tool call, with
/// optional operator/test env overrides. All browser primitive calls within a
/// session share one mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionMode {
    /// Connect to magicutor's CDP proxy. **Default.** Preserves the user's
    /// signed-in profile and extension features.
    Cdp { url: String },
    /// `agent-browser` launches its own headed Chrome (sandboxed).
    Headed,
    /// `agent-browser` launches its own headless Chrome. CI default.
    Headless,
}

/// Exact browser modes available to the progressive retrieval controller.
///
/// Unlike [`ConnectionMode::from_call_arguments`], these modes never consult
/// `MAGICIAN_AGENT_BROWSER_MODE`. Retrieval policy selects the authority first
/// and then resolves one exact transport, so an operator diagnostic override
/// cannot turn an isolated public read into an identity-bearing CDP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalBrowserMode {
    PublicHeadlessRead,
    PublicHeadlessInteract,
    PublicHeadedInteract,
    AuthenticatedCdpRead,
    AuthenticatedCdpInteract,
}

impl RetrievalBrowserMode {
    pub fn connection_mode(self, cdp_url: &str) -> ConnectionMode {
        match self {
            Self::PublicHeadlessRead | Self::PublicHeadlessInteract => ConnectionMode::Headless,
            Self::PublicHeadedInteract => ConnectionMode::Headed,
            Self::AuthenticatedCdpRead | Self::AuthenticatedCdpInteract => ConnectionMode::Cdp {
                url: cdp_url.to_string(),
            },
        }
    }

    pub fn is_read_only(self) -> bool {
        matches!(self, Self::PublicHeadlessRead | Self::AuthenticatedCdpRead)
    }

    pub fn is_identity_bearing(self) -> bool {
        matches!(
            self,
            Self::AuthenticatedCdpRead | Self::AuthenticatedCdpInteract
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PublicHeadlessRead => "public_headless_read",
            Self::PublicHeadlessInteract => "public_headless_interact",
            Self::PublicHeadedInteract => "public_headed_interact",
            Self::AuthenticatedCdpRead => "authenticated_cdp_read",
            Self::AuthenticatedCdpInteract => "authenticated_cdp_interact",
        }
    }

    pub fn connection_mode_label(self) -> &'static str {
        match self {
            Self::PublicHeadlessRead | Self::PublicHeadlessInteract => "headless",
            Self::PublicHeadedInteract => "headed",
            Self::AuthenticatedCdpRead | Self::AuthenticatedCdpInteract => "cdp",
        }
    }
}

impl Default for ConnectionMode {
    fn default() -> Self {
        ConnectionMode::Cdp {
            url: DEFAULT_MAGICUTOR_PROXY_URL.to_string(),
        }
    }
}

impl ConnectionMode {
    pub fn from_env_or_default() -> Self {
        Self::from_runtime_selection(None, None, DEFAULT_MAGICUTOR_PROXY_URL).unwrap_or_else(
            |err| {
                warn!(
                    error = %err,
                    "invalid agent-browser env mode; falling back to cdp mode"
                );
                ConnectionMode::default()
            },
        )
    }

    pub fn from_outer_params(params: &HashMap<String, Value>) -> Result<Self> {
        let requested_mode = params
            .get("connection_mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let requested_cdp_url = params
            .get("cdp_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        Self::from_runtime_selection(
            requested_mode,
            requested_cdp_url,
            DEFAULT_MAGICUTOR_PROXY_URL,
        )
    }

    /// Resolve the session connection mode from a browser tool call's arguments
    /// object — the `connection_mode` / `cdp_url` top-level keys (sibling to
    /// `args`). Same env-override precedence as the rest of the runtime
    /// (`MAGICIAN_AGENT_BROWSER_MODE` / `_CDP_URL` win), then the call's
    /// request, then the `cdp` default. Falls back to the default on an
    /// invalid value. Used by the flat browser dispatch so the FIRST
    /// browser call (the one that creates the session, usually `open`) can
    /// select `headed`/`headless`/`cdp`, instead of always defaulting to
    /// env-or-cdp.
    pub fn from_call_arguments(arguments: &Value, configured_cdp_url: &str) -> Self {
        let requested_mode = arguments
            .get("connection_mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let requested_cdp_url = arguments
            .get("cdp_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        Self::from_runtime_selection(requested_mode, requested_cdp_url, configured_cdp_url)
            .unwrap_or_else(|err| {
                warn!(
                    error = %err,
                    "invalid agent-browser connection_mode in browser call args; falling back to configured cdp"
                );
                ConnectionMode::Cdp {
                    url: configured_cdp_url.to_string(),
                }
            })
    }

    fn from_runtime_selection(
        requested_mode: Option<&str>,
        requested_cdp_url: Option<&str>,
        configured_cdp_url: &str,
    ) -> Result<Self> {
        let env_mode = std::env::var(ENV_AGENT_BROWSER_MODE)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let env_cdp_url = std::env::var(ENV_AGENT_BROWSER_CDP_URL)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let mode = env_mode
            .as_deref()
            .or(requested_mode)
            .unwrap_or("cdp")
            .trim()
            .to_ascii_lowercase();
        let cdp_url = env_cdp_url
            .as_deref()
            .or(requested_cdp_url)
            .unwrap_or(configured_cdp_url)
            .to_string();

        match mode.as_str() {
            "cdp" => Ok(ConnectionMode::Cdp { url: cdp_url }),
            "headed" => Ok(ConnectionMode::Headed),
            "headless" => Ok(ConnectionMode::Headless),
            other => bail!(
                "unsupported agent-browser connection_mode `{}`; expected cdp, headed, or headless",
                other
            ),
        }
    }

    /// Resolve an exact request mode without consulting process environment.
    /// Retrieval handoffs use this after validating their mode-bearing session
    /// ID, while ordinary browser calls retain env/request/default precedence.
    pub fn from_exact_request(arguments: &Value, configured_cdp_url: &str) -> Result<Self> {
        let mode = arguments
            .get("connection_mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("retrieval browser handoff requires connection_mode"))?;
        let cdp_url = arguments
            .get("cdp_url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(configured_cdp_url);
        match mode {
            "cdp" => Ok(ConnectionMode::Cdp {
                url: cdp_url.to_string(),
            }),
            "headed" => Ok(ConnectionMode::Headed),
            "headless" => Ok(ConnectionMode::Headless),
            other => bail!(
                "unsupported retrieval connection_mode `{other}`; expected cdp, headed, or \
                 headless"
            ),
        }
    }
}

/// One browser transport, named the way an agent definition names it.
///
/// The distinction that matters is identity, not window visibility:
/// [`Self::Cdp`] attaches to the OWNER's own signed-in Chrome through the
/// magicutor proxy, while [`Self::Headed`] and [`Self::Headless`] launch
/// `agent-browser`'s own Chrome against a per-work-context profile and carry
/// no owner identity at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserTransport {
    Cdp,
    Headed,
    Headless,
}

impl BrowserTransport {
    /// Every transport there is. Declaring all of them is the same as declaring
    /// none, which is why `session_namespace` consults this.
    pub const ALL: [Self; 3] = [Self::Cdp, Self::Headed, Self::Headless];

    /// Parse one declared transport name. Unrecognised names are an error, never
    /// a silent skip — see [`BrowserTransportCeiling::parse`].
    pub fn parse(name: &str) -> Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "cdp" => Ok(Self::Cdp),
            "headed" => Ok(Self::Headed),
            "headless" => Ok(Self::Headless),
            other => {
                bail!("unsupported browser transport `{other}`; expected cdp, headed, or headless")
            },
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cdp => "cdp",
            Self::Headed => "headed",
            Self::Headless => "headless",
        }
    }

    /// True only for the transport that acts as the owner. Mirrors
    /// [`RetrievalBrowserMode::is_identity_bearing`] on the retrieval path.
    pub fn is_identity_bearing(self) -> bool {
        matches!(self, Self::Cdp)
    }

    pub fn of_connection_mode(mode: &ConnectionMode) -> Self {
        match mode {
            ConnectionMode::Cdp { .. } => Self::Cdp,
            ConnectionMode::Headed => Self::Headed,
            ConnectionMode::Headless => Self::Headless,
        }
    }
}

/// A per-agent ceiling on which browser transports the agent may use.
///
/// Built from `AgentDefinition::browser_transports`. **Empty means all three**
/// — the restriction is opt-in, so an agent that declares nothing keeps the
/// behaviour it had before ceilings existed.
///
/// The ceiling sits ABOVE the tool call and above the operator env override
/// (`MAGICIAN_AGENT_BROWSER_MODE`), both of which
/// [`ConnectionMode::from_call_arguments`] folds into the requested mode before
/// this type ever sees it. That ordering is the point: the model asks, the
/// runtime decides.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserTransportCeiling {
    /// `None` = unrestricted (nothing declared). `Some` is always non-empty.
    allowed: Option<Vec<BrowserTransport>>,
}

impl BrowserTransportCeiling {
    /// Unrestricted — every transport. What an agent that declares nothing gets.
    pub fn unrestricted() -> Self {
        Self { allowed: None }
    }

    /// Read a declared ceiling.
    ///
    /// An unrecognised name is refused HERE rather than dropped, and the refusal
    /// propagates to the browser call as a hard error: a typo that silently
    /// dropped `cdp` would be a quiet demotion of a working agent, and one that
    /// silently widened the set would hand out the owner's Chrome by
    /// misspelling. Blank entries are the one exception — they carry no
    /// intent — and a ceiling of nothing but blanks reads as unrestricted,
    /// identical to declaring nothing.
    pub fn parse<S: AsRef<str>>(declared: &[S]) -> Result<Self> {
        let mut allowed: Vec<BrowserTransport> = Vec::new();
        for entry in declared {
            let name = entry.as_ref().trim();
            if name.is_empty() {
                continue;
            }
            let transport = BrowserTransport::parse(name).with_context(|| {
                format!("invalid browser_transports entry `{name}` in this agent's definition")
            })?;
            if !allowed.contains(&transport) {
                allowed.push(transport);
            }
        }
        Ok(Self {
            allowed: (!allowed.is_empty()).then_some(allowed),
        })
    }

    pub fn is_unrestricted(&self) -> bool {
        self.allowed.is_none()
    }

    /// Whether this ceiling allows acting as the OWNER in a browser.
    ///
    /// Named rather than left as `permits(Cdp)` because callers outside the
    /// browser dispatch are not asking about a transport — they are asking
    /// whether this agent may read a page as the owner. The authenticated
    /// retrieval reader is one: it never resolves a `ConnectionMode` at all,
    /// but `RetrievalBrowserMode::AuthenticatedCdpRead` reaches the owner's
    /// signed-in Chrome by the same route and must answer to the same ceiling.
    pub fn permits_owner_browser(&self) -> bool {
        self.permits(BrowserTransport::Cdp)
    }

    pub fn permits(&self, transport: BrowserTransport) -> bool {
        match self.allowed.as_ref() {
            None => true,
            Some(allowed) => allowed.contains(&transport),
        }
    }

    /// Human-readable ceiling for error messages and logs. Unrestricted renders
    /// as the full set rather than as "none", because empty means all three and
    /// an operator reading "none" would conclude the opposite.
    pub fn label(&self) -> String {
        match self.allowed.as_ref() {
            None => "cdp, headed, headless (unrestricted)".to_string(),
            Some(allowed) => allowed
                .iter()
                .map(|transport| transport.label())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }

    /// The ceiling's segment of a browser session id, or `None` when
    /// unrestricted.
    ///
    /// # Why the CEILING and not the resolved transport
    ///
    /// Keying on the transport was wrong, and wrong in the dangerous direction.
    /// The transport is re-derived from the call arguments EVERY call and
    /// defaults to `cdp` when a call names none — which is the normal shape:
    /// `open` names a mode, and the `snapshot` / `click` / `eval` that follow do
    /// not. Two in-repo callers dispatch exactly that way. So an execution that
    /// opened `headless` and issued any follow-up call would have keyed
    /// `<base>--cdp`, missed its own session, and built a NEW one attached to
    /// the owner's signed-in Chrome — handing his logged-in tabs back to the
    /// model, one call after it had deliberately chosen isolation.
    ///
    /// The ceiling does not have that problem: it is a fact about the agent,
    /// identical on every call of an execution. So an agent's calls all land on
    /// one session, and two agents with DIFFERENT ceilings can never land on the
    /// same one — which is the whole property the suffix exists for.
    ///
    /// `None` for unrestricted, deliberately: the overwhelming majority of
    /// executions are unrestricted, and giving them no suffix keeps their
    /// session ids byte-identical to what teardown, the socket-path budget and
    /// every existing artifact directory already expect.
    pub fn session_namespace(&self) -> Option<String> {
        let allowed = self.allowed.as_ref()?;
        // A ceiling that names all three permits everything, which is what
        // declaring none means. Same authority, so the same session — otherwise
        // two agents that are equivalent under `permits` get two windows, and
        // the one that spelled its ceiling out is quietly denied the shared
        // window the one that stayed silent gets.
        if BrowserTransport::ALL
            .iter()
            .all(|transport| allowed.contains(transport))
        {
            return None;
        }
        let mut labels: Vec<&str> = allowed.iter().map(|transport| transport.label()).collect();
        // Sorted and de-duplicated so two spellings of one ceiling — `[headed,
        // headless]` and `[headless, headed, headless]` — are one namespace and
        // not two windows.
        labels.sort_unstable();
        labels.dedup();
        Some(labels.join("-"))
    }

    /// Apply the ceiling to the transport a browser call resolved to.
    ///
    /// Unrestricted → the request passes through untouched. Permitted → the
    /// request passes through untouched. Otherwise the call is REFUSED, not
    /// substituted.
    ///
    /// # Why refuse rather than fall back to an isolated transport
    ///
    /// A substitution rule cannot be written coherently in both directions. The
    /// `cdp`-requested-under-a-`[headless, headed]`-ceiling case has an obvious
    /// isolated fallback; the `headless`-requested-under-a-`[cdp]`-ceiling case
    /// has none, and the only mode left to substitute is the owner's own
    /// Chrome — a silent PROMOTION to owner identity, which is exactly what
    /// this ceiling exists to prevent. Refusal is the single rule that behaves
    /// the same way in both directions.
    ///
    /// It is also the only one an operator can diagnose. A substituted run does
    /// not fail; it succeeds against logged-out pages and login walls, and the
    /// result is a research answer nobody can account for. A refusal names the
    /// agent's ceiling in the error text, and the agentic loop can re-issue the
    /// call naming a permitted transport.
    ///
    /// This is the same choice the §5A.2 CDP confinement check in
    /// `primitive_dispatch::dispatch` already makes for the work carrier, and
    /// the same one the retrieval path makes by resolving one exact transport
    /// from the authority instead of accepting an override.
    pub fn resolve(&self, requested: ConnectionMode) -> Result<ConnectionMode> {
        let transport = BrowserTransport::of_connection_mode(&requested);
        if self.permits(transport) {
            return Ok(requested);
        }
        bail!(
            "NOT BROWSED — this agent may only use the browser transports [{}], and this call \
             resolved to `{}`.{} Re-issue the browse with `connection_mode` set to one of the \
             permitted transports.",
            self.label(),
            transport.label(),
            if transport.is_identity_bearing() {
                " `cdp` attaches to the owner's own signed-in Chrome, so the visit would carry \
                 the owner's identity, cookies and logged-in accounts."
            } else {
                ""
            }
        )
    }
}

/// Output of one short-lived `agent-browser` invocation.
///
/// Every CLI command returns text or JSON on stdout — `screenshot` writes
/// a file and reports the path, `snapshot` emits the accessibility tree
/// as text, and `eval` returns the JS result as JSON. Screenshot files are
/// captured as artifacts and may be fed back to later LLM calls as image
/// blocks by the generic runtime-context media window.
#[derive(Debug, Clone)]
pub struct AgentBrowserToolResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub parsed_json: Option<Value>,
    pub elapsed_ms: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub struct AgentBrowserSession {
    /// `--session <id>` argument used for state continuity across calls.
    session_id: String,
    mode: ConnectionMode,
    cli_path: PathBuf,
    /// Set after the first successful `connect`/`open` call so subsequent
    /// commands skip re-connection.
    connected: OnceCell<()>,
    /// Headed/headless network archive started after browser bootstrap. CDP
    /// sessions use Magicutor's richer transient trace channel instead.
    har_path: OnceCell<PathBuf>,
    /// Live scope gate captured when this session is created. False means no
    /// HAR command is started and cleanup has nothing to ingest.
    api_mining_capture_enabled: bool,
    /// Optional sink for subprocess stderr lines emitted by `agent-browser`
    /// while a primitive runs. The chat path sets this to a closure that
    /// republishes each line as an `ActionProgress` `ProgressMessage` so the
    /// chat UI's inline_pack card updates with the CLI's mid-flight output.
    /// `None` for autonomous-loop dispatch and tests.
    progress_publisher: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,
    /// Engine-resolver-derived env vars applied to every `agent-browser`
    /// subprocess this session spawns. Resolved at session-construction
    /// time by walking `<scope>/skills/<engine>/scripts/resolve.py` per
    /// convention — see [`resolve_browser_engine_plan`]. Empty when no engine
    /// is selected, or after the bounded CloakBrowser capacity fallback (then
    /// agent-browser uses its default Chrome for Testing).
    engine_env: RwLock<HashMap<String, String>>,
    /// Name of the initially selected engine. Kept separately from the env so
    /// an arbitrary Chromium process exiting with code 76 cannot activate the
    /// CloakBrowser-specific capacity gate.
    primary_engine_name: Option<String>,
    /// One bounded alternate selected at construction time. For CloakBrowser
    /// this is agent-browser's bundled Chrome for Testing; other engines do not
    /// receive an implicit retry.
    capacity_fallback: Option<ResolvedBrowserEngine>,
    capacity_fallback_used: AtomicBool,
    /// Optional page URL selected by the outer browser pack call. Used only
    /// for session bootstrap so a headed/headless run opens directly on the
    /// target page instead of flashing an extra `about:blank` window.
    initial_url: Option<String>,
    /// Optional per-stream capture limit used by deterministic retrieval.
    /// The subprocess pipes are always drained fully; only retained bytes are
    /// bounded, preventing a page from forcing unbounded controller memory.
    capture_limit_bytes: Option<usize>,
    /// Optional scope-owned analytics sink. It is attached by runtime call
    /// sites that have authoritative principal/workspace provenance.
    analytics: Option<BrowserEngineAnalyticsContext>,
    /// Immediate higher-level engine that failed before this independently
    /// constructed session was attempted. This complements the in-session
    /// CloakBrowser capacity fallback and keeps public-read fallback lineage
    /// explicit rather than inferred from adjacent timestamps.
    analytics_fallback_from: Option<String>,
    /// Last successfully opened HTTP(S) page, used to attribute click/read
    /// commands without persisting command arguments or sensitive query data.
    analytics_last_url: RwLock<Option<String>>,
}

impl std::fmt::Debug for AgentBrowserSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBrowserSession")
            .field("session_id", &self.session_id)
            .field("mode", &self.mode)
            .field("cli_path", &self.cli_path)
            .field("connected", &self.connected.get().is_some())
            .field("har_path", &self.har_path.get())
            .field("primary_engine_name", &self.primary_engine_name)
            .field(
                "capacity_fallback_used",
                &self.capacity_fallback_used.load(Ordering::Acquire),
            )
            .field("progress_publisher", &self.progress_publisher.is_some())
            .field("initial_url", &self.initial_url)
            .field("capture_limit_bytes", &self.capture_limit_bytes)
            .field("analytics", &self.analytics.is_some())
            .finish()
    }
}

impl AgentBrowserSession {
    /// Resolve the agent-browser CLI path. Precedence:
    ///
    /// 1. Workspace-layer install (when `principal` and `workspace` are
    ///    set): `<storage_root>/scopes/<principal>/<workspace>/skills/browser/bin/agent-browser`.
    ///    Mirrors SkillLoader's workspace-first contract — a workspace
    ///    that pins its own browser binary wins over extras.
    /// 2. Each extras root from `tool-runtime-config.yaml ::
    ///    registry.paths`, checking `<extra>/skills/browser/bin/agent-browser`
    ///    in declared order.
    /// 3. `MAGICIAN_AGENT_BROWSER_CLI` env var (full path), only for
    ///    callers without a storage root.
    ///
    /// **No `PATH` fallback** — version pinning happens at install time via
    /// `make setup-agent-browser`; relying on a globally-installed copy would
    /// silently bypass that pin. Runtime calls pass a storage root, so a
    /// missing vendored CLI is an error instead of falling through to an env
    /// override.
    /// Returns an error with remediation guidance if no source resolves
    /// to an existing file.
    pub fn resolve_cli_path(storage_root: Option<&Path>) -> Result<PathBuf> {
        Self::resolve_cli_path_for_scope(storage_root, None, None)
    }

    /// Scope-aware resolution. Pass the active `principal` + `workspace`
    /// to consult the workspace skills layer first; both `None` skips
    /// straight to the extras layer (legacy callers / tests).
    pub fn resolve_cli_path_for_scope(
        storage_root: Option<&Path>,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<PathBuf> {
        if let Some(root) = storage_root {
            // Workspace layer first.
            if let (Some(p), Some(w)) = (principal, workspace) {
                let scope = root.join(scope_vendored_relative_path(p, w));
                if scope.exists() {
                    return Ok(scope);
                }
            }
            for extra_skills_root in crate::magician_v2::config_extras::extra_skills_dirs() {
                let candidate = extra_skills_root.join(BROWSER_BIN_RELATIVE_PATH);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
            bail!(
                "pinned agent-browser CLI not found in workspace skills layer or any `registry.paths` extras. \
                 Run `make -C skillshub install-scope SCOPE=<principal>/<workspace>` to install into the active workspace, \
                 or add an extras root containing `skills/{BROWSER_BIN_RELATIVE_PATH}`. \
                 Runtime browser execution does not use {ENV_AGENT_BROWSER_CLI} or PATH when a storage root is available."
            );
        }
        if let Ok(p) = std::env::var(ENV_AGENT_BROWSER_CLI) {
            let path = PathBuf::from(&p);
            if path.exists() {
                return Ok(path);
            }
            bail!(
                "{} is set to `{}` but no file exists at that path.",
                ENV_AGENT_BROWSER_CLI,
                p,
            );
        }
        bail!(
            "agent-browser CLI not found. Runtime execution must install the pinned version under <scope>/skills/{BROWSER_BIN_RELATIVE_PATH}. Detached/test callers without a storage root may set {ENV_AGENT_BROWSER_CLI} to an absolute path."
        );
    }

    /// Construct a new session. Does not connect; the first `run_command`
    /// will lazy-connect via `ensure_connected`.
    ///
    /// Hard-fails if `cli_path` does not exist — every callable path comes
    /// from `resolve_cli_path` and must point to a real binary. There is
    /// no `PATH` fallback: version pinning is enforced at install time.
    pub fn new(thread_id: &str, mode: ConnectionMode, cli_path: PathBuf) -> Result<Self> {
        Self::new_with_session_id(
            format!("magician-{}", sanitize_session_id(thread_id)),
            mode,
            cli_path,
        )
    }

    /// Construct a session with an explicit `--session` id. Bypasses the
    /// default `magician-{thread_id}` derivation. Used by inner-loop
    /// dispatch when `PrimitiveExecCtx.browser_session_id_override` is
    /// set, so a child execution can attach to its parent's existing
    /// Chrome window instead of spawning a fresh one keyed on its own
    /// `execution_id`. The session_id is used verbatim as the
    /// `agent-browser --session <id>` argument; callers must pre-
    /// sanitise to `[a-zA-Z0-9_-]`.
    pub fn new_with_session_id(
        session_id: String,
        mode: ConnectionMode,
        cli_path: PathBuf,
    ) -> Result<Self> {
        if !cli_path.exists() {
            bail!(
                "agent-browser CLI not found at {}. Run `make setup-agent-browser`.",
                cli_path.display(),
            );
        }
        // For CDP mode, route to a per-session URL path. The proxy uses
        // this segment as the session id and owns one dedicated tab per
        // session, so agent-browser only ever sees the agent's tab —
        // never the user's other 100+ tabs.
        let mode = match mode {
            ConnectionMode::Cdp { url } if url == DEFAULT_MAGICUTOR_PROXY_URL => {
                ConnectionMode::Cdp {
                    url: format!("ws://127.0.0.1:3003/devtools/browser/{session_id}"),
                }
            },
            other => other,
        };
        Ok(Self {
            session_id,
            mode,
            cli_path,
            connected: OnceCell::new(),
            har_path: OnceCell::new(),
            api_mining_capture_enabled: false,
            progress_publisher: None,
            engine_env: RwLock::new(HashMap::new()),
            primary_engine_name: None,
            capacity_fallback: None,
            capacity_fallback_used: AtomicBool::new(false),
            initial_url: None,
            capture_limit_bytes: None,
            analytics: None,
            analytics_fallback_from: None,
            analytics_last_url: RwLock::new(None),
        })
    }

    pub fn with_api_mining_capture_enabled(mut self, enabled: bool) -> Self {
        self.api_mining_capture_enabled = enabled;
        self
    }

    /// Construct a retrieval-owned session with an exact mode. This bypasses
    /// all generic browser request/env mode selection by design.
    pub fn new_retrieval(
        session_id: String,
        mode: RetrievalBrowserMode,
        cdp_url: &str,
        cli_path: PathBuf,
    ) -> Result<Self> {
        let connection_mode = if mode.is_identity_bearing() {
            ConnectionMode::Cdp {
                url: scoped_retrieval_cdp_url(cdp_url, &session_id)?,
            }
        } else {
            mode.connection_mode(cdp_url)
        };
        Self::new_with_session_id(session_id, connection_mode, cli_path)
    }

    /// Attach the initial page URL selected by the outer browser pack call.
    /// Empty and blank-browser URLs are ignored so the legacy bootstrap remains
    /// unchanged when no target is known.
    pub fn with_initial_url(mut self, initial_url: Option<String>) -> Self {
        self.initial_url = initial_url.and_then(normalize_initial_url);
        self
    }

    /// Attach engine-resolver-derived env vars (e.g. `AGENT_BROWSER_EXECUTABLE_PATH`,
    /// `AGENT_BROWSER_ARGS`, `AGENT_BROWSER_INIT_SCRIPTS`, `TZ`) to every
    /// subprocess this session spawns. Prefer [`resolve_browser_engine_plan`]
    /// for runtime paths that need the bounded CloakBrowser capacity fallback.
    /// Empty map = no engine override (agent-browser uses its default Chrome
    /// for Testing).
    pub fn with_engine_env(mut self, env: HashMap<String, String>) -> Self {
        self.engine_env = RwLock::new(env);
        self
    }

    /// Attach the resolved primary engine and its single capacity fallback.
    /// The fallback remains dormant unless CloakBrowser's launch reports its
    /// documented session-limit exit code.
    pub fn with_engine_plan(mut self, plan: BrowserEnginePlan) -> Self {
        self.primary_engine_name = plan.primary.name;
        self.engine_env = RwLock::new(plan.primary.env);
        self.capacity_fallback = plan.capacity_fallback;
        self
    }

    pub fn with_capture_limit_bytes(mut self, limit: usize) -> Self {
        self.capture_limit_bytes = Some(limit.max(1));
        self
    }

    pub fn with_analytics_context(mut self, analytics: BrowserEngineAnalyticsContext) -> Self {
        self.analytics = Some(analytics);
        self
    }

    pub fn with_analytics_fallback_from(mut self, engine: Option<String>) -> Self {
        self.analytics_fallback_from = engine.and_then(|engine| {
            let engine = engine.trim().to_string();
            (!engine.is_empty()).then_some(engine)
        });
        self
    }

    /// Attach a progress sink that receives each line of `agent-browser`
    /// stderr while the CLI runs. Returns `self` for builder chaining.
    pub fn with_progress_publisher(
        mut self,
        publisher: Option<Arc<dyn Fn(String) + Send + Sync + 'static>>,
    ) -> Self {
        self.progress_publisher = publisher;
        self
    }

    /// Read-only accessor for the resolved `--session` id (`magician-<thread>`).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Read-only accessor for the connection mode.
    pub fn mode(&self) -> &ConnectionMode {
        &self.mode
    }

    pub fn har_path(&self) -> Option<&Path> {
        self.har_path.get().map(PathBuf::as_path)
    }

    /// Snapshot one env value from the engine that is currently active. This is
    /// primarily used by native integrations that must identify the launched
    /// application after a capacity fallback has occurred.
    pub fn active_engine_env_value(&self, key: &str) -> Option<String> {
        self.engine_env
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
    }

    pub fn active_engine_name(&self) -> Option<String> {
        if self.capacity_fallback_used.load(Ordering::Acquire) {
            self.capacity_fallback
                .as_ref()
                .and_then(|engine| engine.name.clone())
        } else {
            self.primary_engine_name.clone()
        }
    }

    fn effective_engine_name(&self) -> String {
        if matches!(self.mode, ConnectionMode::Cdp { .. }) {
            return "magicutor_cdp".to_string();
        }
        self.active_engine_name()
            .unwrap_or_else(|| BUNDLED_BROWSER_ENGINE_NAME.to_string())
    }

    fn active_fallback_source(&self, engine: &str) -> Option<String> {
        if self.capacity_fallback_used.load(Ordering::Acquire) {
            return self
                .primary_engine_name
                .as_deref()
                .filter(|primary| *primary != engine)
                .map(str::to_string)
                .or_else(|| self.analytics_fallback_from.clone());
        }
        self.analytics_fallback_from.clone()
    }

    fn analytics_url_for_command(&self, args: &[&str]) -> Option<String> {
        if args.first().copied() == Some("open") {
            if let Some(url) = args
                .get(1)
                .and_then(|url| sanitize_browser_analytics_url(Some(url)))
            {
                return Some(url);
            }
        }
        self.analytics_last_url
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .or_else(|| sanitize_browser_analytics_url(self.initial_url.as_deref()))
    }

    fn observe_successful_navigation(&self, args: &[&str]) {
        if args.first().copied() != Some("open") {
            return;
        }
        let Some(url) = args
            .get(1)
            .and_then(|url| sanitize_browser_analytics_url(Some(url)))
        else {
            return;
        };
        *self
            .analytics_last_url
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(url);
    }

    async fn record_command_attempt(
        &self,
        args: &[&str],
        include_session: bool,
        engine: String,
        success: bool,
        elapsed_ms: u64,
        error_class: Option<&str>,
    ) {
        if !include_session {
            return;
        }
        let Some(analytics) = self.analytics.as_ref() else {
            return;
        };
        let fallback_from = self.active_fallback_source(&engine);
        analytics
            .record(BrowserEngineUsageInput {
                session_id: self.session_id.clone(),
                engine,
                fallback_from,
                connection_mode: describe_mode(&self.mode).to_string(),
                operation: browser_operation_label(args),
                url: self.analytics_url_for_command(args),
                success,
                elapsed_ms,
                error_class: error_class.map(str::to_string),
            })
            .await;
    }

    /// Lazily connect to Chrome. Idempotent: safe to call before every
    /// `run_command`; only the first call dispatches the connect/open.
    pub async fn ensure_connected(&self) -> Result<()> {
        self.connected
            .get_or_try_init(|| async { self.connect_initial().await })
            .await?;
        Ok(())
    }

    /// First-time connection bootstrap, dispatched on `mode`.
    async fn connect_initial(&self) -> Result<()> {
        let initial_url = self.initial_open_target();
        let args: Vec<&str> = match &self.mode {
            ConnectionMode::Cdp { url } => vec!["connect", url],
            ConnectionMode::Headed => vec!["open", initial_url, "--headed"],
            ConnectionMode::Headless => vec!["open", initial_url],
        };
        let mut result = self
            .run_command_internal(&args, true, DEFAULT_COMMAND_TIMEOUT_SECS, &[], None)
            .await?;
        if !result.success && self.should_use_capacity_fallback(&result) {
            let fallback = self
                .capacity_fallback
                .as_ref()
                .expect("capacity fallback predicate requires a configured fallback");
            warn!(
                session = %self.session_id,
                primary_engine = CLOAK_BROWSER_ENGINE_NAME,
                fallback_engine = fallback.name.as_deref().unwrap_or(BUNDLED_BROWSER_ENGINE_NAME),
                "CloakBrowser concurrent-session limit reached; retrying once with alternate engine"
            );

            // `agent-browser` keeps one daemon per session and captures launch
            // env when that daemon starts. Close it before changing env so the
            // retry cannot accidentally reuse the failed Cloak-backed daemon.
            let close = self
                .run_command_internal(&["close"], true, DEFAULT_COMMAND_TIMEOUT_SECS, &[], None)
                .await?;
            if !close.success {
                bail!(
                    "agent-browser could not retire the capacity-limited CloakBrowser daemon: stderr={}",
                    close.stderr.trim()
                );
            }
            {
                let mut active_env = self
                    .engine_env
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *active_env = fallback.env.clone();
            }
            self.capacity_fallback_used.store(true, Ordering::Release);
            result = self
                .run_command_internal(&args, true, DEFAULT_COMMAND_TIMEOUT_SECS, &[], None)
                .await?;
        }
        if !result.success {
            bail!(
                "agent-browser initial {} failed: stderr={}",
                describe_mode(&self.mode),
                result.stderr.trim()
            );
        }
        debug!(
            session = %self.session_id,
            mode = describe_mode(&self.mode),
            initial_url = %initial_url,
            elapsed_ms = result.elapsed_ms,
            "agent-browser session connected"
        );
        if matches!(self.mode, ConnectionMode::Cdp { .. }) {
            if let Some(target_url) = self.initial_url.as_deref() {
                let navigate = self
                    .run_command_internal(
                        &["open", target_url],
                        true,
                        DEFAULT_COMMAND_TIMEOUT_SECS,
                        &[],
                        None,
                    )
                    .await?;
                if !navigate.success {
                    bail!(
                        "agent-browser initial navigation to {} failed: stderr={}",
                        target_url,
                        navigate.stderr.trim()
                    );
                }
                debug!(
                    session = %self.session_id,
                    initial_url = %target_url,
                    elapsed_ms = navigate.elapsed_ms,
                    "agent-browser session navigated to initial URL"
                );
            }
        } else if self.api_mining_capture_enabled {
            let artifact_dir = flat_browser_artifact_dir(&self.session_id);
            if let Err(error) = std::fs::create_dir_all(&artifact_dir) {
                warn!(
                    session = %self.session_id,
                    %error,
                    "could not create HAR capture directory; browser execution continues"
                );
            } else {
                let har_path = artifact_dir.join("api-mining.har");
                if har_path.to_str().is_some() {
                    // The path is handed to `har stop`; `har start` records only.
                    match self
                        .run_command_internal(
                            &super::har::har_start_args(),
                            true,
                            DEFAULT_COMMAND_TIMEOUT_SECS,
                            &[],
                            None,
                        )
                        .await
                    {
                        Ok(started) if started.success => {
                            let _ = self.har_path.set(har_path);
                        },
                        Ok(started) => warn!(
                            session = %self.session_id,
                            stderr = %started.stderr.trim(),
                            "HAR capture could not start; browser execution continues"
                        ),
                        Err(error) => warn!(
                            session = %self.session_id,
                            %error,
                            "HAR capture command failed; browser execution continues"
                        ),
                    }
                }
            }
        }
        Ok(())
    }

    fn should_use_capacity_fallback(&self, result: &AgentBrowserToolResult) -> bool {
        !self.capacity_fallback_used.load(Ordering::Acquire)
            && self.primary_engine_name.as_deref() == Some(CLOAK_BROWSER_ENGINE_NAME)
            && self.capacity_fallback.is_some()
            && looks_like_cloak_capacity_error(result)
    }

    fn initial_open_target(&self) -> &str {
        self.initial_url.as_deref().unwrap_or("about:blank")
    }

    /// Whether a browser command may be transparently re-run after a CDP
    /// disconnect.
    ///
    /// A disconnect is not proof the command did nothing: the click can land
    /// and the connection drop immediately after. Re-running unconditionally
    /// therefore submits some forms twice, with no record that it happened.
    ///
    /// Only commands that read state or move the viewport are safe to repeat.
    /// Anything that can commit input — click, fill, type, press, select,
    /// upload, drag, or arbitrary `eval` — surfaces the disconnect instead, so
    /// the caller can decide rather than the transport deciding for it.
    ///
    /// Default-deny: an unrecognised verb is treated as unsafe.
    fn command_is_retry_safe(args: &[&str]) -> bool {
        match args.first().copied() {
            // Pure reads and viewport moves. Repeating one costs a round trip
            // and nothing else.
            Some("snapshot" | "screenshot" | "find" | "get" | "wait" | "scroll") => true,

            // Navigation. Not an obvious member of the list, so the reasoning:
            // `connect_initial` puts the tab back on `about:blank` or the
            // session's initial URL, which DESTROYS the evidence an agent would
            // need to judge an indeterminate result. Refusing the retry means
            // returning "this may have taken effect, go re-observe" about a
            // page the reconnect just navigated away from — so the agent
            // observes a blank tab and re-issues the same navigation anyway.
            // The effect happens twice either way; declining merely spends a
            // decision turn and attaches a warning that misdescribes what
            // happened. Re-issuing a navigation is a GET, the same act the
            // browser itself repeats on reload.
            Some("open" | "goto") => true,

            // `tab` is a family, not a verb: listing is a read, while
            // `tab new` / `tab close` change what exists. Only the read is safe,
            // so this one is matched on the subcommand rather than the family.
            Some("tab") => matches!(args.get(1).copied(), Some("list")),

            // Everything else, INCLUDING verbs this list has never seen. A new
            // agent-browser subcommand is unsafe until someone decides it is
            // not.
            _ => false,
        }
    }

    /// Run a single `agent-browser <args...>` invocation. Lazy-connects
    /// automatically on the first call.
    pub async fn run_command(&self, args: &[&str]) -> Result<AgentBrowserToolResult> {
        self.run_command_with_options(args, DEFAULT_COMMAND_TIMEOUT_SECS, &[])
            .await
    }

    /// Run one session command with caller-provided timeout and environment.
    pub async fn run_command_with_options(
        &self,
        args: &[&str],
        timeout_secs: u64,
        envs: &[(&str, &Path)],
    ) -> Result<AgentBrowserToolResult> {
        self.run_command_with_options_and_stdin(args, timeout_secs, envs, None)
            .await
    }

    /// Run one session command with caller-provided timeout, environment, and
    /// optional stdin.
    pub async fn run_command_with_options_and_stdin(
        &self,
        args: &[&str],
        timeout_secs: u64,
        envs: &[(&str, &Path)],
        stdin: Option<&str>,
    ) -> Result<AgentBrowserToolResult> {
        self.ensure_connected().await?;
        let first = self
            .run_command_internal(args, true, timeout_secs, envs, stdin)
            .await?;
        if first.success || !looks_like_cdp_disconnect(&first) {
            return Ok(first);
        }

        if !Self::command_is_retry_safe(args) {
            // The command may already have committed: a disconnect can arrive
            // after the click landed. Reconnect so the session is usable again,
            // but do not repeat an effect that cannot be undone.
            //
            // Returning this as a plain failure is not enough. An ordinary
            // failure invites the agent to re-decide and issue the same click
            // again — which double-submits if the first one landed, and merely
            // moves the duplicate from the transport to the model. So the
            // result is annotated as INDETERMINATE, with an instruction the
            // next decision can act on. Mirrors the governed-MCP terminal-audit
            // path, which preserves its outcome and says "do not retry this
            // operation automatically" for the same reason.
            warn!(
                session = %self.session_id,
                args = ?args,
                "agent-browser command saw a CDP disconnect and is not safe to \
                 repeat; reconnecting without re-running"
            );
            // Deliberately not `?`. If the browser is genuinely gone — the
            // usual reason the command disconnected in the first place — then
            // propagating the reconnect error would discard `first` along with
            // the annotation below, and hand the agent a plain transport
            // failure. That is the exact shape that invites it to re-issue the
            // click. A session we could not restore makes the "may already have
            // committed" warning MORE important to deliver, not less.
            if let Err(error) = self.connect_initial().await {
                warn!(
                    session = %self.session_id,
                    %error,
                    "agent-browser reconnect failed after an indeterminate \
                     command; returning the annotated result anyway"
                );
            }
            let mut indeterminate = first;
            if !indeterminate.stderr.is_empty() {
                indeterminate.stderr.push('\n');
            }
            indeterminate.stderr.push_str(
                "INDETERMINATE: the browser connection dropped around this \
                 command, so it may already have taken effect. It was NOT \
                 retried. Re-observe the page to establish what actually \
                 happened before issuing this action again.",
            );
            return Ok(indeterminate);
        }

        warn!(
            session = %self.session_id,
            args = ?args,
            "agent-browser command saw a CDP disconnect; reconnecting and \
             re-running a read-only command"
        );
        self.connect_initial().await?;
        self.run_command_internal(args, true, timeout_secs, envs, stdin)
            .await
    }

    /// Run a read-only CLI command that does not need a browser session.
    ///
    /// Used for documentation/discovery commands such as `--help` and
    /// `skills get core --full`; these should not trigger lazy browser
    /// connection.
    pub async fn run_cli_command(&self, args: &[&str]) -> Result<AgentBrowserToolResult> {
        self.run_command_internal(args, false, DEFAULT_COMMAND_TIMEOUT_SECS, &[], None)
            .await
    }

    /// Spawn one short-lived `agent-browser --session <id> <args...>` and
    /// capture stdout/stderr.
    async fn run_command_internal(
        &self,
        args: &[&str],
        include_session: bool,
        timeout_secs: u64,
        envs: &[(&str, &Path)],
        stdin: Option<&str>,
    ) -> Result<AgentBrowserToolResult> {
        let start = Instant::now();
        let analytics_engine = self.effective_engine_name();
        let mut cmd = Command::new(&self.cli_path);
        if include_session {
            cmd.arg("--session").arg(&self.session_id);
        }
        for arg in args {
            cmd.arg(arg);
        }
        for (key, value) in envs {
            cmd.env(key, value);
        }
        // Engine env (AGENT_BROWSER_EXECUTABLE_PATH, AGENT_BROWSER_ARGS, …) —
        // applied AFTER `envs` so per-call overrides still win. Empty when
        // no engine was selected (then agent-browser uses Chrome for Testing).
        let engine_env = self
            .engine_env
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if should_disable_upstream_idle_timeout(
            &engine_env,
            std::env::var_os(AGENT_BROWSER_IDLE_TIMEOUT_ENV).is_some(),
        ) {
            cmd.env(AGENT_BROWSER_IDLE_TIMEOUT_ENV, "0");
        }
        for (key, value) in &engine_env {
            cmd.env(key, value);
        }
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let timeout_secs = timeout_secs.max(1);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.record_command_attempt(
                    args,
                    include_session,
                    analytics_engine,
                    false,
                    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    Some("spawn_failed"),
                )
                .await;
                return Err(anyhow::Error::from(error).context(format!(
                    "agent-browser failed to spawn {} for args {:?}",
                    self.cli_path.display(),
                    args
                )));
            },
        };
        if let Some(stdin) = stdin {
            let mut child_stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("agent-browser stdin pipe unavailable"))?;
            child_stdin
                .write_all(stdin.as_bytes())
                .await
                .with_context(|| {
                    format!("agent-browser failed to write stdin for args {:?}", args)
                })?;
            child_stdin.shutdown().await.with_context(|| {
                format!("agent-browser failed to close stdin for args {:?}", args)
            })?;
        }

        // Stream stderr line-by-line through `progress_publisher` while the
        // child runs. Behavior on success/failure is byte-identical to the
        // previous `child.wait_with_output()` path: we still buffer both
        // streams in full and feed them to `AgentBrowserToolResult`.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let publisher = self.progress_publisher.clone();

        let capture_limit = self.capture_limit_bytes;
        let stderr_handle: tokio::task::JoinHandle<(Vec<u8>, bool)> = if let Some(pipe) =
            stderr_pipe
        {
            tokio::spawn(async move {
                let mut reader = BufReader::new(pipe);
                let mut accumulated = Vec::<u8>::new();
                let mut truncated = false;
                let mut line = Vec::<u8>::new();
                loop {
                    line.clear();
                    match reader.read_until(b'\n', &mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            append_bounded(&mut accumulated, &line, capture_limit, &mut truncated);
                            if let Some(publisher) = publisher.as_ref() {
                                let trimmed = String::from_utf8_lossy(&line).trim().to_string();
                                if !trimmed.is_empty() {
                                    publisher(trimmed);
                                }
                            }
                        },
                        Err(_) => break,
                    }
                }
                (accumulated, truncated)
            })
        } else {
            tokio::spawn(async { (Vec::new(), false) })
        };

        let stdout_handle: tokio::task::JoinHandle<(Vec<u8>, bool)> =
            if let Some(mut pipe) = stdout_pipe {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut scratch = [0_u8; 16 * 1024];
                    let mut truncated = false;
                    loop {
                        match pipe.read(&mut scratch).await {
                            Ok(0) => break,
                            Ok(read) => append_bounded(
                                &mut buf,
                                &scratch[..read],
                                capture_limit,
                                &mut truncated,
                            ),
                            Err(_) => break,
                        }
                    }
                    (buf, truncated)
                })
            } else {
                tokio::spawn(async { (Vec::new(), false) })
            };

        let deadline = TokioInstant::now() + Duration::from_secs(timeout_secs);
        let status = match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                let _ = child.kill().await;
                self.record_command_attempt(
                    args,
                    include_session,
                    analytics_engine,
                    false,
                    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    Some("wait_failed"),
                )
                .await;
                return Err(anyhow::Error::from(error).context(format!(
                    "agent-browser failed while waiting on {} for args {:?}",
                    self.cli_path.display(),
                    args
                )));
            },
            Err(_) => {
                let _ = child.kill().await;
                self.record_command_attempt(
                    args,
                    include_session,
                    analytics_engine,
                    false,
                    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    Some("timeout"),
                )
                .await;
                return Err(anyhow!(
                    "agent-browser command timed out after {}s: {:?}",
                    timeout_secs,
                    args
                ));
            },
        };

        let (stdout_bytes, stdout_truncated) = stdout_handle.await.unwrap_or_default();
        let (stderr_bytes, stderr_truncated) = stderr_handle.await.unwrap_or_default();

        let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
        let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
        let parsed_json = serde_json::from_str(stdout.trim()).ok();
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        if !status.success() {
            warn!(
                session = %self.session_id,
                args = ?args,
                stderr = %stderr.trim(),
                "agent-browser command failed"
            );
        }

        let result = AgentBrowserToolResult {
            success: status.success(),
            stdout,
            stderr,
            parsed_json,
            elapsed_ms,
            stdout_truncated,
            stderr_truncated,
        };
        let error_class = browser_command_error_class(&result);
        self.record_command_attempt(
            args,
            include_session,
            analytics_engine,
            result.success,
            result.elapsed_ms,
            error_class,
        )
        .await;
        if result.success {
            self.observe_successful_navigation(args);
        }
        Ok(result)
    }

    /// Best-effort `agent-browser --session <id> close`. Only runs if a
    /// connection was actually established.
    pub async fn shutdown(&self) -> Result<()> {
        if self.connected.get().is_some() {
            let _ = self
                .run_command_internal(&["close"], true, DEFAULT_COMMAND_TIMEOUT_SECS, &[], None)
                .await;
        }
        Ok(())
    }

    async fn shutdown_controller_owned(&self) -> Result<()> {
        let args: &[&str] = if matches!(self.mode, ConnectionMode::Cdp { .. }) {
            // CDP retrieval sessions attach to the owner's Magicutor browser.
            // Tear down the agent-browser daemon/session but never send
            // Browser.close to the attached browser process.
            &["close", "--keep-browser"]
        } else {
            &["close"]
        };
        let result = self
            .run_command_internal(args, true, DEFAULT_COMMAND_TIMEOUT_SECS, &[], None)
            .await?;
        if !result.success {
            bail!("controller-owned browser session close failed");
        }
        Ok(())
    }
}

fn append_bounded(
    destination: &mut Vec<u8>,
    bytes: &[u8],
    limit: Option<usize>,
    truncated: &mut bool,
) {
    let Some(limit) = limit else {
        destination.extend_from_slice(bytes);
        return;
    };
    let remaining = limit.saturating_sub(destination.len());
    destination.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
    if bytes.len() > remaining {
        *truncated = true;
    }
}

fn browser_operation_label(args: &[&str]) -> String {
    let first = args.first().copied().unwrap_or("unknown");
    match first {
        "mouse" | "get" | "set" | "tab" | "storage" | "cookies" => args
            .get(1)
            .map(|second| format!("{first} {second}"))
            .unwrap_or_else(|| first.to_string()),
        _ => first.to_string(),
    }
}

fn browser_command_error_class(result: &AgentBrowserToolResult) -> Option<&'static str> {
    if result.success {
        return None;
    }
    if looks_like_cloak_capacity_error(result) {
        return Some("capacity_limit");
    }
    if looks_like_cdp_disconnect(result) {
        return Some("cdp_disconnect");
    }
    if result.stdout_truncated || result.stderr_truncated {
        return Some("output_truncated");
    }
    Some("command_failed")
}

/// RAII ownership for retrieval-created browser sessions. Normal completion
/// calls [`Self::shutdown`]; cancellation drops the in-flight read future and
/// schedules the same best-effort close without touching cached agent sessions.
pub struct ControllerOwnedBrowserSession {
    session: Arc<AgentBrowserSession>,
    closed: Arc<AtomicBool>,
}

impl ControllerOwnedBrowserSession {
    pub fn new(session: AgentBrowserSession) -> Self {
        Self {
            session: Arc::new(session),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn session(&self) -> &AgentBrowserSession {
        self.session.as_ref()
    }

    pub async fn shutdown(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let result = self.session.shutdown_controller_owned().await;
        if result.is_ok() {
            self.closed.store(true, Ordering::Release);
        }
        result
    }
}

impl Drop for ControllerOwnedBrowserSession {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let session = Arc::clone(&self.session);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = session.shutdown_controller_owned().await {
                    warn!(
                        session = session.session_id(),
                        error = %error,
                        "retrieval browser session teardown failed after cancellation"
                    );
                }
            });
        }
    }
}

/// Resolve `agent-browser` env vars from the requested browser engine.
///
/// Convention: each engine lives at
/// `<storage_root>/scopes/<principal>/<workspace>/skills/<engine_name>/scripts/
/// resolve.py`, falling back to
/// `<extra>/skills/<engine_name>/scripts/resolve.py` for each entry in
/// `tool-runtime-config.yaml :: registry.paths` (first-listed wins). When
/// invoked it emits a JSON envelope on stdout (see any installed browser-engine
/// skill's `scripts/resolve.py` for the canonical shape).
/// The runtime translates that envelope into agent-browser env vars
/// (`AGENT_BROWSER_EXECUTABLE_PATH`, `AGENT_BROWSER_ARGS`, etc.).
///
/// Selection precedence:
/// 1. Explicit `engine_name` (when caller passes one) — used if its resolver
///    exists and runs successfully.
/// 2. No engine — `agent-browser` uses its bundled/default browser.
///
/// CDP mode never uses an engine; the user's real Chrome is the engine.
/// Returns an empty map when no engine resolves — `agent-browser` then
/// falls back to its default (Chrome for Testing).
#[derive(Debug, Clone, Default)]
pub struct ResolvedBrowserEngine {
    pub name: Option<String>,
    pub env: HashMap<String, String>,
}

/// Primary browser-engine selection plus the only automatic retry the runtime
/// permits. The fallback is deliberately absent for every engine except
/// CloakBrowser, and is consumed at most once by [`AgentBrowserSession`].
#[derive(Debug, Clone, Default)]
pub struct BrowserEnginePlan {
    pub primary: ResolvedBrowserEngine,
    pub capacity_fallback: Option<ResolvedBrowserEngine>,
}

pub fn is_valid_browser_engine_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub fn resolve_browser_engine_env(
    storage_root: &Path,
    principal: &str,
    workspace: &str,
    mode: &ConnectionMode,
    engine_name: Option<&str>,
) -> Result<HashMap<String, String>> {
    Ok(resolve_browser_engine(storage_root, principal, workspace, mode, engine_name)?.env)
}

pub fn resolve_browser_engine_plan(
    storage_root: &Path,
    principal: &str,
    workspace: &str,
    mode: &ConnectionMode,
    engine_name: Option<&str>,
) -> Result<BrowserEnginePlan> {
    let primary = resolve_browser_engine(storage_root, principal, workspace, mode, engine_name)?;
    let capacity_fallback =
        (primary.name.as_deref() == Some(CLOAK_BROWSER_ENGINE_NAME)).then(|| {
            ResolvedBrowserEngine {
                name: Some(BUNDLED_BROWSER_ENGINE_NAME.to_string()),
                env: HashMap::new(),
            }
        });
    Ok(BrowserEnginePlan {
        primary,
        capacity_fallback,
    })
}

pub fn resolve_browser_engine(
    storage_root: &Path,
    principal: &str,
    workspace: &str,
    mode: &ConnectionMode,
    engine_name: Option<&str>,
) -> Result<ResolvedBrowserEngine> {
    if matches!(mode, ConnectionMode::Cdp { .. }) {
        return Ok(ResolvedBrowserEngine::default());
    }

    let Some(engine_name) = engine_name else {
        return Ok(ResolvedBrowserEngine::default());
    };
    if !is_valid_browser_engine_name(engine_name) {
        bail!("configured browser engine name `{engine_name}` is invalid");
    }
    if engine_name == LIGHTPANDA_BROWSER_ENGINE_NAME && matches!(mode, ConnectionMode::Headed) {
        bail!(
            "browser engine `{LIGHTPANDA_BROWSER_ENGINE_NAME}` is headless-only; use \
             cloak-browser, bundled_chrome, or CDP for headed work"
        );
    }
    if engine_name == BUNDLED_BROWSER_ENGINE_NAME {
        return Ok(ResolvedBrowserEngine {
            name: Some(BUNDLED_BROWSER_ENGINE_NAME.to_string()),
            env: HashMap::new(),
        });
    }
    let candidates = [engine_name];

    let headed = matches!(mode, ConnectionMode::Headed);
    let venv_python = resolve_venv_python();
    let mut attempted_at_least_one = false;

    for name in candidates {
        // Browser engines live under the active
        // scope's skills tree as symlinks back to skillshub/<name>/. The
        // system-shared install layer was retired — every install goes
        // through `make install-scope SCOPE=<principal>/<workspace>`.
        // Falls through to extras roots declared via
        // `tool-runtime-config.yaml :: registry.paths` (first-listed
        // wins, matching the rest of the extras consumers) so a user
        // can pin a browser engine without writing it into every scope.
        // `resolve.py` may be a host-absolute symlink into skillshub on a
        // materialized scope; rewrite the target for this environment (identity
        // on a native host) so the existence check AND the spawn target resolve
        // in a container — otherwise the raw scope symlink dangles and the
        // configured engine silently never applies (mirror dispatcher.rs / scripts.rs).
        let scope_resolver = crate::magician_v2::skills::path_rewrite::resolve_skill_path(
            &storage_root
                .join("scopes")
                .join(principal)
                .join(workspace)
                .join("skills")
                .join(name)
                .join("scripts")
                .join("resolve.py"),
        );
        let resolver = if scope_resolver.exists() {
            scope_resolver
        } else {
            let extras_match = crate::magician_v2::config_extras::extra_skills_dirs()
                .into_iter()
                .map(|extra_skills_root| {
                    crate::magician_v2::skills::path_rewrite::resolve_skill_path(
                        &extra_skills_root
                            .join(name)
                            .join("scripts")
                            .join("resolve.py"),
                    )
                })
                .find(|candidate| candidate.exists());
            match extras_match {
                Some(extras_resolver) => extras_resolver,
                None => {
                    // Skip silently when an engine isn't installed at
                    // all. Only the explicit-engine case is loud — fallback
                    // candidates are expected to be optional.
                    warn!(
                        "browser engine `{}` requested but resolver not found in scope ({}) or \
                         any `registry.paths` extras; falling back to default Chrome for Testing",
                        name,
                        scope_resolver.display()
                    );
                    continue;
                },
            }
        };
        attempted_at_least_one = true;

        let mut cmd = std::process::Command::new(&venv_python);
        cmd.arg(&resolver);
        if headed {
            cmd.arg("--headed");
        }

        match cmd.output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                match parse_engine_envelope(&stdout) {
                    Ok(env) => {
                        // Surface success at INFO so internal_data /
                        // tail_logs / find_errors can confirm which
                        // engine actually applied for a given session.
                        let key_count = env.len();
                        info!(
                            "browser engine `{}` resolved: applied {} env var(s) to agent-browser \
                             subprocess (binary launched via AGENT_BROWSER_EXECUTABLE_PATH)",
                            name, key_count
                        );
                        return Ok(ResolvedBrowserEngine {
                            name: Some(name.to_string()),
                            env,
                        });
                    },
                    Err(err) => {
                        warn!(
                            "browser engine `{}` resolver returned unparseable JSON (error: {}); \
                             trying next candidate",
                            name, err
                        );
                        continue;
                    },
                }
            },
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    "browser engine `{}` resolver at {} exited code={:?} (stderr: {}); trying \
                     next candidate",
                    name,
                    resolver.display(),
                    output.status.code(),
                    stderr.trim()
                );
                continue;
            },
            Err(err) => {
                warn!(
                    "browser engine `{}` resolver at {} failed to spawn (error: {}); check that \
                     skillshub/.venv exists relative to magician's cwd (run `make -C skillshub \
                     setup-python`); trying next candidate",
                    name,
                    resolver.display(),
                    err
                );
                continue;
            },
        }
    }

    let detail = if attempted_at_least_one {
        "its resolver failed"
    } else {
        "its resolver was not installed"
    };
    bail!(
        "configured browser engine `{engine_name}` could not be resolved because {detail}; \
         install its skill with scripts/resolve.py or clear content_acquisition.browser.engine to \
         use agent-browser's bundled browser"
    )
}

fn resolve_venv_python() -> PathBuf {
    // Browser engine resolvers may need
    // python imports (cloakbrowser, etc.) that only the skillshub venv has.
    // Magician is launched from the repo root (via run-supervisor.sh which
    // cd's there), so the venv sits at `./skillshub/.venv/bin/python3`. If
    // that file is missing we fall back to `python3` from PATH — the
    // resolver will then likely fail to import its packages, the per-attempt
    // warning will fire, and the runtime falls back to the next candidate
    // (eventually agent-browser's default Chrome).
    let cwd_relative = std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join("skillshub/.venv/bin/python3"));
    if let Some(path) = cwd_relative {
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("python3")
}

fn parse_engine_envelope(json: &str) -> Result<HashMap<String, String>> {
    let value: Value = serde_json::from_str(json.trim())
        .with_context(|| format!("engine envelope is not valid JSON: {}", json.trim()))?;
    let mut env = HashMap::new();
    if let Some(binary) = value.get("binary_path").and_then(Value::as_str) {
        if !binary.is_empty() {
            env.insert(
                "AGENT_BROWSER_EXECUTABLE_PATH".to_string(),
                binary.to_string(),
            );
        }
    }
    if let Some(headed) = value.get("headed").and_then(Value::as_bool) {
        env.insert("AGENT_BROWSER_HEADED".to_string(), headed.to_string());
    }
    if let Some(args) = value.get("args").and_then(Value::as_array) {
        let joined: Vec<String> = args
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if !joined.is_empty() {
            env.insert("AGENT_BROWSER_ARGS".to_string(), joined.join(","));
        }
    }
    if let Some(scripts) = value.get("init_scripts").and_then(Value::as_array) {
        let joined: Vec<String> = scripts
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if !joined.is_empty() {
            env.insert("AGENT_BROWSER_INIT_SCRIPTS".to_string(), joined.join(","));
        }
    }
    if let Some(ua) = value.get("user_agent").and_then(Value::as_str) {
        if !ua.is_empty() {
            env.insert("AGENT_BROWSER_USER_AGENT".to_string(), ua.to_string());
        }
    }
    if let Some(extras) = value.get("env").and_then(Value::as_object) {
        for (k, v) in extras {
            if let Some(s) = v.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }
    Ok(env)
}

/// Process-global cache of live agent-browser sessions, keyed by the
/// `--session` id (stable per execution via `effective_browser_session_id`).
///
/// Flat-mode browser primitives (Phase 6) dispatch one command per call, each
/// building a fresh dispatcher. Without this cache every `browser__*` primitive
/// would construct a new [`AgentBrowserSession`] with its own `connected`
/// `OnceCell` and reconnect to Chrome before the command — doubling subprocess
/// spawns and risking duplicate `open` in headed mode. Caching the session by
/// its stable id makes the lazy connection persist across primitives, so the
/// whole execution connects once — mirroring how a provider-backed pack (e.g.
/// duckdb) shares one provider/session across flat calls. Evicted by
/// [`close_session_for_thread_with_options`] when the session is actually
/// closed (the keep-alive paths skip that call and retain the entry for the
/// next execution that attaches to the same window).
static FLAT_BROWSER_SESSIONS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Arc<AgentBrowserSession>>>,
> = std::sync::OnceLock::new();

fn flat_browser_sessions(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, Arc<AgentBrowserSession>>> {
    FLAT_BROWSER_SESSIONS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Resolve (or lazily create + cache) the browser session for `session_id`.
/// `build` runs only on a cache miss and must NOT connect — connection stays
/// lazy via [`AgentBrowserSession::ensure_connected`]. See
/// [`FLAT_BROWSER_SESSIONS`].
pub fn get_or_create_flat_browser_session(
    session_id: &str,
    build: impl FnOnce() -> Result<AgentBrowserSession>,
) -> Result<Arc<AgentBrowserSession>> {
    let mut map = flat_browser_sessions()
        .lock()
        .map_err(|_| anyhow!("flat browser session cache mutex poisoned"))?;
    if let Some(existing) = map.get(session_id) {
        return Ok(Arc::clone(existing));
    }
    let session = Arc::new(build()?);
    map.insert(session_id.to_string(), Arc::clone(&session));
    Ok(session)
}

/// Transient on-disk staging dir where a flat-mode `browser__screenshot` writes
/// its PNG before it is read into `screenshot_storage` and deleted. Keyed by the
/// session id (derivable without other context) and rooted in the OS temp dir so
/// it is clearly throwaway and OS-reclaimable; [`forget_flat_browser_session`]
/// removes it on session teardown.
pub fn flat_browser_artifact_dir(session_id: &str) -> PathBuf {
    std::env::temp_dir()
        .join("magician_flat_browser")
        .join(session_id)
}

/// Whether a cached session is headless, from the SESSION rather than its id.
///
/// The id used to encode the transport, and a caller could ask by suffix. It
/// encodes the CEILING now — `--headed-headless` for an agent permitted both —
/// so the id no longer says which browser was actually launched, and
/// `ends_with("--headless")` silently stopped matching anything.
///
/// `None` when the session is not cached: teardown must not then assume
/// headless and force a real close on a window the user can see, nor assume
/// headed and strand a windowless Chromium. The caller decides what an unknown
/// session gets.
pub fn flat_browser_session_is_headless(session_id: &str) -> Option<bool> {
    let map = flat_browser_sessions().lock().ok()?;
    map.get(session_id)
        .map(|session| matches!(session.mode(), ConnectionMode::Headless))
}

/// Whether a cached session drives the owner's own Chrome over Magicutor's
/// CDP proxy (`None` when this process never cached it). Such a session's
/// browser is not ours to close: teardown detaches it and has Magicutor
/// close only the window the automation opened.
pub fn flat_browser_session_is_cdp(session_id: &str) -> Option<bool> {
    let map = flat_browser_sessions().lock().ok()?;
    map.get(session_id)
        .map(|session| matches!(session.mode(), ConnectionMode::Cdp { .. }))
}

/// Every cached session id belonging to one thread, including its
/// ceiling-suffixed variants.
///
/// A browser session id is `magician-<thread>` for an unrestricted agent and
/// `magician-<thread>--<ceiling>` for one that declared `browser_transports`.
/// The CEILING is part of the identity — not the transport, which is re-derived
/// per call — so an agent's calls all land on one session while two agents with
/// different ceilings never share one.
///
/// Teardown therefore cannot compute a single id and close it. This returns
/// what the thread opened UNDER THAT BASE, so a restricted agent's window is
/// closed by the same sweep that closes the unrestricted one beside it.
///
/// One exception, and it predates ceilings entirely: an execution bound to a
/// work carrier gets a PREFIXED id — `scope-<kind>-<id>-magician-<thread>`, see
/// `engagement_browser_session_id` — which matches neither the base nor
/// `base--`, so this sweep returns none of them and teardown closes nothing for
/// a bound execution. That is a pre-existing leak with its own cause: teardown
/// DERIVES an id while dispatch USES one that may have been overridden or
/// prefixed. Widening the match here would not fix it and would risk closing a
/// window belonging to a different thread; the repair is for dispatch to
/// register the id it actually used.
pub fn flat_browser_session_ids_for_thread(base_session_id: &str) -> Vec<String> {
    let Ok(map) = flat_browser_sessions().lock() else {
        // A poisoned cache must not silently report "this thread opened
        // nothing" — that reads as a clean teardown over a window still on the
        // screen. The caller still closes the base id unconditionally.
        return Vec::new();
    };
    let prefix = format!("{base_session_id}--");
    map.keys()
        .filter(|id| id.as_str() == base_session_id || id.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Snapshot cached session handles for the ordinary thread id and all of its
/// transport-ceiling variants. Used to drain headed/headless HARs before the
/// static close path evicts their handles and staging directories.
pub fn flat_browser_sessions_for_thread(thread_id: &str) -> Vec<Arc<AgentBrowserSession>> {
    let base = format!("magician-{}", sanitize_session_id(thread_id));
    let prefix = format!("{base}--");
    flat_browser_sessions()
        .lock()
        .map(|sessions| {
            sessions
                .iter()
                .filter(|(id, _)| id.as_str() == base || id.starts_with(&prefix))
                .map(|(_, session)| Arc::clone(session))
                .collect()
        })
        .unwrap_or_default()
}

/// Drop the cached session handle for `session_id` and remove its transient
/// screenshot staging dir. No-op if absent. The browser process itself is torn
/// down by the `agent-browser close` subprocess; this only releases the cached
/// Rust handle (so it can't be reused after the window is gone) and the
/// throwaway staging files.
pub fn forget_flat_browser_session(session_id: &str) {
    if let Ok(mut map) = flat_browser_sessions().lock() {
        map.remove(session_id);
    }
    let _ = std::fs::remove_dir_all(flat_browser_artifact_dir(session_id));
}

/// Static cleanup: terminate the agent-browser session for `thread_id`
/// without needing a constructed [`AgentBrowserSession`]. Used at outer-
/// loop terminal time to close the dedicated Chrome window when the task
/// execution finishes.
///
/// Idempotent: if no session exists for the id, the CLI returns quickly
/// without effect. Best-effort — callers swallow the result.
pub async fn close_session_for_thread(thread_id: &str, cli_path: &Path) -> Result<()> {
    close_session_for_thread_with_options(thread_id, cli_path, false).await
}

/// `keep_browser=true` hands the browser off to the user: the daemon
/// shuts down without sending `Browser.close` and without killing the
/// Chrome process group. Chromium continues running as an
/// ownerless user-visible window; closing the window normally exits
/// Chrome cleanly. Used when the LLM signals
/// `keep_browser_window_open: true` on a terminal control call.
///
/// It applies to the base id and to every ceiling-suffixed variant this thread
/// opened, EXCEPT any whose cached session is actually headless — those are
/// always closed for real, because a headless browser has no window to hand over
/// and detaching would strand the Chromium process with no daemon and no
/// addressable session.
///
/// That test asks the SESSION, not the id: the id carries the agent's ceiling,
/// so an agent permitted both `headed` and `headless` has one id whichever it
/// launched, and an agent that declared no ceiling has the bare base id
/// whichever it launched.
///
/// Requires the Magician-patched agent-browser CLI; the current
/// `0.38.1-Magician.0` vendor build preserves this handoff contract. On
/// older binaries the flag is silently ignored and the CLI performs a normal
/// close.
pub async fn close_session_for_thread_with_options(
    thread_id: &str,
    cli_path: &Path,
    keep_browser: bool,
) -> Result<()> {
    let session_id = format!("magician-{}", sanitize_session_id(thread_id));

    // The base id is closed unconditionally, exactly as before: `agent-browser`
    // may hold a session this process never cached (a restart, another entry
    // point), and the old behaviour covered that.
    //
    // `--keep-browser` is decided for it the same way it is decided per variant
    // below, and for the same reason. The base id is NOT the cdp-only case: an
    // agent that declared no ceiling has no suffix, so its session IS the base
    // id whichever transport it chose — and `connection_mode: headless` on the
    // opening call is an ordinary thing for it to choose. Handing that off would
    // strand a windowless Chromium with no daemon and no addressable session, on
    // the common path rather than the rare one.
    let base_keep_browser =
        keep_browser && !flat_browser_session_is_headless(&session_id).unwrap_or(false);
    let base = close_session_by_id_with_options(&session_id, cli_path, base_keep_browser).await;

    // Then every ceiling-suffixed variant this thread actually opened. Without
    // this the suffix leaks a Chrome daemon per distinct ceiling per thread:
    // teardown computes `magician-<thread>` while a restricted agent's live
    // session is `magician-<thread>--headed-headless`, and nothing ever matches
    // it.
    //
    // An UNRESTRICTED agent has no suffix, so its session is the base id and is
    // closed by the line above — which is why the common case needs none of
    // this. Only an agent that declared a ceiling adds a variant.
    //
    // Failures are collected rather than short-circuited — stopping at the first
    // one would leave the remaining windows open, which is the same leak with
    // one extra step.
    let mut failures: Vec<String> = base
        .err()
        .map(|error| format!("{error:#}"))
        .into_iter()
        .collect();
    for variant in flat_browser_session_ids_for_thread(&session_id) {
        if variant == session_id {
            continue;
        }
        // `--keep-browser` is decided per variant, not once for the sweep.
        //
        // The flag means "hand the window to the human": the daemon exits
        // without sending `Browser.close` and `std::mem::forget`s the launched
        // Chrome handle, so the process outlives the session that owned it. That
        // is a handoff only when there is something to hand over. A `headless`
        // variant has no window, so the same flag leaves a Chromium process
        // group alive with no daemon, no window and no session id anyone can
        // address again — it survives until the machine is rebooted or somebody
        // kills the pid by hand. The suffix did not create that hazard, it
        // multiplied it: one thread can now hold several sessions, so the sweep
        // reaches several browsers with one flag that described only one of them.
        //
        // Same rule the retrieval-handoff sweep already applies in
        // `cleanup::additional_session_keeps_browser`, which hands off
        // `retrieval-cdp-` and (on owner handoff) `retrieval-headed-` and never
        // `retrieval-headless-`.
        // Asked of the SESSION, not of the id. The id carries the ceiling now, so
        // a `[headed, headless]` agent's session ends `--headed-headless` and the
        // old `ends_with("--headless")` test matched nothing — handing off a
        // browser that may have no window at all.
        //
        // An uncached session keeps the caller's intent: it is the same answer
        // the base id gets one line above, and inventing a different one for a
        // session we cannot inspect would be a guess.
        let variant_keep_browser =
            keep_browser && !flat_browser_session_is_headless(&variant).unwrap_or(false);
        if let Err(error) =
            close_session_by_id_with_options(&variant, cli_path, variant_keep_browser).await
        {
            failures.push(format!("{variant}: {error:#}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!(
            "failed to close {} browser session(s) for this thread: {}",
            failures.len(),
            failures.join("; ")
        )
    }
}

pub async fn close_session_by_id_with_options(
    session_id: &str,
    cli_path: &Path,
    keep_browser: bool,
) -> Result<()> {
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("agent-browser session id is invalid");
    }
    // The window is being torn down — drop any cached flat-mode session handle
    // for this id so a later execution can't reuse a session whose browser is
    // gone. (Keep-alive / window-handoff paths skip this fn entirely, so they
    // retain the cached entry.)
    forget_flat_browser_session(session_id);
    let mut cmd = Command::new(cli_path);
    cmd.arg("--session").arg(session_id).arg("close");
    if keep_browser {
        cmd.arg("--keep-browser");
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let output = timeout(Duration::from_secs(10), cmd.output())
        .await
        .map_err(|_| {
            anyhow!(
                "agent-browser close timed out after 10s for session {}",
                session_id
            )
        })?
        .with_context(|| {
            format!(
                "agent-browser failed to spawn close for session {}",
                session_id
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!(
            session = %session_id,
            stderr = %stderr.trim(),
            keep_browser,
            "agent-browser close exited non-zero (likely already-closed session)"
        );
    } else {
        debug!(session = %session_id, keep_browser, "agent-browser session closed");
    }
    Ok(())
}

fn describe_mode(mode: &ConnectionMode) -> &'static str {
    match mode {
        ConnectionMode::Cdp { .. } => "cdp/connect",
        ConnectionMode::Headed => "headed/open",
        ConnectionMode::Headless => "headless/open",
    }
}

fn looks_like_cdp_disconnect(result: &AgentBrowserToolResult) -> bool {
    let combined = format!("{}\n{}", result.stdout, result.stderr).to_lowercase();
    // Application-level CDP errors are NOT transport disconnects: the command
    // reached Chrome and got a structured rejection. The common case is a
    // `dialog accept/dismiss` that finds no dialog open (alert/beforeunload are
    // auto-accepted, so the dialog is usually already gone) — "CDP error
    // (Page.handleJavaScriptDialog): No dialog is showing". These must NOT
    // trigger a reconnect (which, for a headed session, re-opens about:blank).
    if combined.contains("no dialog is showing") || combined.contains("page.handlejavascriptdialog")
    {
        return false;
    }
    // Match only true transport-level signals. The bare needle "cdp" was removed
    // because it matched every "CDP error (...)" application rejection; genuine
    // CDP transport drops still match via the specific phrases below.
    [
        "websocket",
        "target closed",
        "browser has been closed",
        "connection closed",
        "cdp connection",
        "socket hang up",
        "econnreset",
        "disconnected",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

/// CloakBrowser's current binary reserves exit code 76 for a concurrent-session
/// denial. The wrapper also exposes the canonical human message, so accept both
/// representations while rejecting every other license code and browser error.
fn looks_like_cloak_capacity_error(result: &AgentBrowserToolResult) -> bool {
    let combined = format!("{}\n{}", result.stdout, result.stderr).to_lowercase();
    combined.contains("chrome exited early (exit code: 76)")
        || combined.contains("process did exit: exitcode=76")
        || combined.contains("cloakbrowser pro: session limit reached")
}

fn normalize_initial_url(raw: String) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("about:blank")
        || trimmed.eq_ignore_ascii_case("chrome://newtab/")
        || trimmed.eq_ignore_ascii_case("chrome://newtab")
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Replace anything that isn't `[a-zA-Z0-9_-]` with `_` so the thread_id
/// flows safely into the `--session` arg.
///
/// `pub` so `PrimitiveExecCtx::effective_browser_session_id`
/// (sibling module) can apply the same rule when constructing a
/// session id from the override.
pub fn sanitize_session_id(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn scoped_retrieval_cdp_url(base: &str, session_id: &str) -> Result<String> {
    let mut url = url::Url::parse(base).context("parsing retrieval Magicutor CDP URL")?;
    let local_host = matches!(
        url.host_str().map(str::to_ascii_lowercase).as_deref(),
        Some("127.0.0.1" | "::1" | "localhost")
    );
    if !matches!(url.scheme(), "ws" | "wss")
        || !local_host
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/devtools/browser/magicutor-proxy"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("retrieval CDP must use the credential-free local Magicutor proxy endpoint");
    }
    url.set_path(&format!("/devtools/browser/{session_id}"));
    Ok(url.to_string())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{os::unix::fs::PermissionsExt, sync::Mutex};

    use super::*;

    #[test]
    fn only_commands_that_cannot_commit_input_are_retry_safe() {
        // A CDP disconnect is not proof the command did nothing — the click can
        // land and the connection drop immediately after. So the transport may
        // only re-run commands that read state or move the viewport.
        for safe in [
            "snapshot",
            "screenshot",
            "find",
            "get",
            "wait",
            "scroll",
            "open",
            "goto",
        ] {
            assert!(
                AgentBrowserSession::command_is_retry_safe(&[safe]),
                "{safe} reads, moves the viewport, or re-issues a GET"
            );
        }

        for committing in [
            "click", "fill", "type", "press", "select", "upload", "drag", "eval",
        ] {
            assert!(
                !AgentBrowserSession::command_is_retry_safe(&[committing]),
                "{committing} can commit input and must never be auto-repeated"
            );
        }

        // `tab` is a family, so the gate reads the subcommand. Listing is a
        // read; creating and closing tabs are not. A bare `tab` names no
        // subcommand and so cannot be shown safe.
        assert!(AgentBrowserSession::command_is_retry_safe(&["tab", "list"]));
        for mutating in ["new", "close", "select"] {
            assert!(
                !AgentBrowserSession::command_is_retry_safe(&["tab", mutating]),
                "tab {mutating} changes what exists"
            );
        }
        assert!(!AgentBrowserSession::command_is_retry_safe(&["tab"]));

        // Default-deny. A verb this list has not seen — a new agent-browser
        // subcommand, say — is treated as unsafe rather than assumed harmless.
        assert!(
            !AgentBrowserSession::command_is_retry_safe(&["some-future-verb"]),
            "an unrecognised command must default to unsafe"
        );
        assert!(
            !AgentBrowserSession::command_is_retry_safe(&[]),
            "an empty argv must default to unsafe"
        );
    }

    /// Serialize tests that mutate `MAGICIAN_AGENT_BROWSER_CLI`. Cargo runs
    /// tests in parallel within a single process, and env-var state is
    /// process-global, so concurrent set/remove leaks across tests.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        // poisoned guard is fine — we just want serial access
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Pins the opt-in rule: an agent that declares nothing keeps every
    /// transport. A default-deny here would silently take the browser away
    /// from every agent in the fleet that never heard of ceilings.
    #[test]
    fn an_empty_ceiling_changes_nothing() {
        let ceiling = BrowserTransportCeiling::parse::<String>(&[]).expect("empty ceiling parses");
        assert!(ceiling.is_unrestricted());
        for requested in [
            ConnectionMode::Cdp {
                url: "ws://127.0.0.1:3003/x".to_string(),
            },
            ConnectionMode::Headed,
            ConnectionMode::Headless,
        ] {
            assert_eq!(
                ceiling.resolve(requested.clone()).expect("unrestricted"),
                requested,
                "an unrestricted ceiling must return the requested mode untouched"
            );
        }
    }

    /// Entries that are only whitespace carry no intent, so a ceiling made of
    /// nothing but blanks must read as "declared nothing" rather than as a
    /// ceiling permitting nothing (which would brick the browser entirely).
    #[test]
    fn a_blank_only_ceiling_reads_as_unrestricted() {
        let ceiling =
            BrowserTransportCeiling::parse(&["".to_string(), "   ".to_string()]).expect("parses");
        assert!(ceiling.is_unrestricted());
        assert_eq!(
            ceiling.resolve(ConnectionMode::default()).expect("allowed"),
            ConnectionMode::default()
        );
    }

    /// THE failure this whole ceiling exists to prevent: an agent whose ceiling
    /// omits `cdp` must not reach the owner's signed-in Chrome no matter how the
    /// call arrives — asked for explicitly, arrived at as the runtime default,
    /// or forced by the operator env override.
    #[test]
    fn a_ceiling_without_cdp_cannot_obtain_cdp_however_the_call_asks() {
        let _guard = env_lock();
        let ceiling =
            BrowserTransportCeiling::parse(&["headless".to_string(), "headed".to_string()])
                .expect("parses");
        let configured = "ws://127.0.0.1:3003/devtools/browser/magicutor-proxy";

        // 1. asked for explicitly
        let previous = std::env::var(ENV_AGENT_BROWSER_MODE).ok();
        std::env::remove_var(ENV_AGENT_BROWSER_MODE);
        let explicit = ConnectionMode::from_call_arguments(
            &serde_json::json!({ "connection_mode": "cdp" }),
            configured,
        );
        assert!(matches!(explicit, ConnectionMode::Cdp { .. }));
        let error = ceiling.resolve(explicit).unwrap_err().to_string();
        assert!(error.contains("NOT BROWSED"), "{error}");
        assert!(error.contains("headless, headed"), "{error}");

        // 2. arrived at as the runtime default — nothing requested at all
        let defaulted = ConnectionMode::from_call_arguments(&serde_json::json!({}), configured);
        assert!(
            matches!(defaulted, ConnectionMode::Cdp { .. }),
            "the pre-ceiling default really is the owner's Chrome; that is the hole"
        );
        assert!(ceiling.resolve(defaulted).is_err());

        // 3. forced by the operator diagnostic env override
        std::env::set_var(ENV_AGENT_BROWSER_MODE, "cdp");
        let forced = ConnectionMode::from_call_arguments(
            &serde_json::json!({ "connection_mode": "headless" }),
            configured,
        );
        assert!(matches!(forced, ConnectionMode::Cdp { .. }));
        assert!(
            ceiling.resolve(forced).is_err(),
            "an operator override must not widen an agent past its ceiling"
        );
        match previous {
            Some(value) => std::env::set_var(ENV_AGENT_BROWSER_MODE, value),
            None => std::env::remove_var(ENV_AGENT_BROWSER_MODE),
        }
    }

    /// The refusal is symmetric. A `[cdp]`-only ceiling refuses `headless`
    /// rather than substituting `cdp`, which is why the rule is "refuse" and
    /// not "fall back to something isolated": there is no isolated mode to fall
    /// back TO here, and the only substitution available is a silent promotion
    /// to owner identity.
    #[test]
    fn a_cdp_only_ceiling_refuses_headless_rather_than_promoting_to_cdp() {
        let ceiling = BrowserTransportCeiling::parse(&["cdp".to_string()]).expect("parses");
        let error = ceiling
            .resolve(ConnectionMode::Headless)
            .unwrap_err()
            .to_string();
        assert!(error.contains("NOT BROWSED"), "{error}");
        assert!(error.contains("headless"), "{error}");
    }

    /// A typo must not be read as policy. Silently dropping `cdp` would demote
    /// a working agent with no trace; silently adding it would hand out the
    /// owner's Chrome by misspelling. Both are refused at parse.
    #[test]
    fn an_unknown_transport_name_is_refused_not_ignored() {
        let error = BrowserTransportCeiling::parse(&["headles".to_string()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("browser_transports"), "{error}");

        let error = BrowserTransportCeiling::parse(&["headless".to_string(), "cpd".to_string()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("browser_transports"), "{error}");
    }

    /// Casing and stray whitespace in a hand-written YAML list are formatting,
    /// not intent, and must not turn into a refusal.
    #[test]
    fn ceiling_names_are_case_and_whitespace_insensitive() {
        let ceiling = BrowserTransportCeiling::parse(&[" HEADLESS ".to_string()]).expect("parses");
        assert!(ceiling.permits(BrowserTransport::Headless));
        assert!(!ceiling.permits(BrowserTransport::Cdp));
        assert_eq!(ceiling.label(), "headless");
    }

    /// Only `cdp` acts as the owner. If `headed` were ever classified as
    /// identity-bearing, the officer ceilings would be wrong; if `cdp` were
    /// ever classified as isolated, every ceiling would be.
    #[test]
    fn only_cdp_is_identity_bearing() {
        assert!(BrowserTransport::Cdp.is_identity_bearing());
        assert!(!BrowserTransport::Headed.is_identity_bearing());
        assert!(!BrowserTransport::Headless.is_identity_bearing());
        assert_eq!(
            BrowserTransport::of_connection_mode(&ConnectionMode::default()),
            BrowserTransport::Cdp
        );
        assert_eq!(
            BrowserTransport::of_connection_mode(&ConnectionMode::Headed),
            BrowserTransport::Headed
        );
        assert_eq!(
            BrowserTransport::of_connection_mode(&ConnectionMode::Headless),
            BrowserTransport::Headless
        );
    }

    /// The retrieval path resolves one exact transport from the retrieval
    /// AUTHORITY, never from an agent's tool call, and this change must not
    /// have moved it. Pins that `RetrievalBrowserMode::connection_mode` still
    /// answers the same three transports it always did — the ceiling is applied
    /// on the ordinary branch only.
    #[test]
    fn the_retrieval_path_is_unchanged_by_the_ceiling() {
        let url = "ws://127.0.0.1:3003/devtools/browser/magicutor-proxy";
        assert_eq!(
            RetrievalBrowserMode::PublicHeadlessRead.connection_mode(url),
            ConnectionMode::Headless
        );
        assert_eq!(
            RetrievalBrowserMode::PublicHeadedInteract.connection_mode(url),
            ConnectionMode::Headed
        );
        assert_eq!(
            RetrievalBrowserMode::AuthenticatedCdpInteract.connection_mode(url),
            ConnectionMode::Cdp {
                url: url.to_string()
            }
        );
    }

    #[test]
    fn upstream_idle_timeout_is_disabled_only_without_an_override() {
        let empty = HashMap::new();
        assert!(should_disable_upstream_idle_timeout(&empty, false));
        assert!(!should_disable_upstream_idle_timeout(&empty, true));

        let configured = HashMap::from([(
            AGENT_BROWSER_IDLE_TIMEOUT_ENV.to_string(),
            "15m".to_string(),
        )]);
        assert!(!should_disable_upstream_idle_timeout(&configured, false));
    }

    /// Set up a temp file that looks like a valid CLI binary.
    fn make_fake_cli(tmp: &tempfile::TempDir, rel: &str) -> PathBuf {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        path
    }

    fn make_recording_cli(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let log = tmp.path().join("calls.log");
        let path = tmp.path().join("agent-browser");
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        (path, log)
    }

    fn make_capacity_recording_cli(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let log = tmp.path().join("capacity-calls.log");
        let path = tmp.path().join("agent-browser-capacity");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s|%s\\n' \"${{MAGICIAN_TEST_BROWSER_ENGINE:-unset}}\" \"$*\" >> '{}'\ncase \" $* \" in\n  *' open '*)\n    if [ \"${{MAGICIAN_TEST_BROWSER_ENGINE:-}}\" = cloak ]; then\n      code=\"${{MAGICIAN_TEST_CLOAK_EXIT_CODE:-76}}\"\n      echo \"Chrome exited early (exit code: $code) without writing DevToolsActivePort\" >&2\n      exit 1\n    fi\n    if [ \"${{MAGICIAN_TEST_BROWSER_ENGINE:-}}\" = bundled ] && [ \"${{MAGICIAN_TEST_BUNDLED_FAIL:-}}\" = 1 ]; then\n      echo \"Chrome exited early (exit code: 76) without writing DevToolsActivePort\" >&2\n      exit 1\n    fi\n    ;;\nesac\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        (path, log)
    }

    #[test]
    fn retrieval_mode_is_exact_even_when_generic_env_requests_cdp() {
        let _guard = env_lock();
        let previous = std::env::var(ENV_AGENT_BROWSER_MODE).ok();
        std::env::set_var(ENV_AGENT_BROWSER_MODE, "cdp");
        let temp = tempfile::tempdir().unwrap();
        let cli = make_fake_cli(&temp, "agent-browser");

        let session = AgentBrowserSession::new_retrieval(
            "retrieval-exact-headless".into(),
            RetrievalBrowserMode::PublicHeadlessRead,
            DEFAULT_MAGICUTOR_PROXY_URL,
            cli,
        )
        .unwrap();
        assert_eq!(session.mode(), &ConnectionMode::Headless);

        match previous {
            Some(value) => std::env::set_var(ENV_AGENT_BROWSER_MODE, value),
            None => std::env::remove_var(ENV_AGENT_BROWSER_MODE),
        }
    }

    #[test]
    fn retrieval_cdp_sessions_are_unique_and_isolated_by_proxy_path() {
        let temp = tempfile::tempdir().unwrap();
        let cli = make_fake_cli(&temp, "agent-browser");
        let first = AgentBrowserSession::new_retrieval(
            "retrieval-first".into(),
            RetrievalBrowserMode::AuthenticatedCdpRead,
            DEFAULT_MAGICUTOR_PROXY_URL,
            cli.clone(),
        )
        .unwrap();
        let second = AgentBrowserSession::new_retrieval(
            "retrieval-second".into(),
            RetrievalBrowserMode::AuthenticatedCdpRead,
            DEFAULT_MAGICUTOR_PROXY_URL,
            cli,
        )
        .unwrap();
        assert_ne!(first.session_id(), second.session_id());
        assert_eq!(
            first.mode(),
            &ConnectionMode::Cdp {
                url: "ws://127.0.0.1:3003/devtools/browser/retrieval-first".into()
            }
        );
        assert_eq!(
            second.mode(),
            &ConnectionMode::Cdp {
                url: "ws://127.0.0.1:3003/devtools/browser/retrieval-second".into()
            }
        );
        assert!(AgentBrowserSession::new_retrieval(
            "retrieval-remote".into(),
            RetrievalBrowserMode::AuthenticatedCdpRead,
            "wss://remote.example/devtools/browser/magicutor-proxy",
            make_fake_cli(&temp, "other-agent-browser"),
        )
        .is_err());
    }

    #[test]
    fn bounded_capture_retains_prefix_and_reports_truncation() {
        let mut output = Vec::new();
        let mut truncated = false;
        append_bounded(&mut output, b"abcd", Some(5), &mut truncated);
        append_bounded(&mut output, b"efgh", Some(5), &mut truncated);
        assert_eq!(output, b"abcde");
        assert!(truncated);
    }

    #[tokio::test]
    async fn controller_owned_cdp_shutdown_keeps_attached_browser_alive() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, log) = make_recording_cli(&temp);
        let session = AgentBrowserSession::new_retrieval(
            "retrieval-cdp-close-test".into(),
            RetrievalBrowserMode::AuthenticatedCdpRead,
            DEFAULT_MAGICUTOR_PROXY_URL,
            cli,
        )
        .unwrap();
        ControllerOwnedBrowserSession::new(session)
            .shutdown()
            .await
            .unwrap();
        let calls = std::fs::read_to_string(log).unwrap();
        assert!(calls.contains("close --keep-browser"));
    }

    #[test]
    fn flat_browser_session_cache_reuses_within_id_then_evicts() {
        // Construction is connection-free (lazy connect via ensure_connected),
        // so this exercises the Phase-6 session cache without launching a
        // browser. `new_with_session_id` only checks that the CLI path exists,
        // so a fake CLI file suffices — no agent-browser binary needed.
        let tmp = tempfile::tempdir().expect("temp dir");
        let cli = make_fake_cli(&tmp, "agent-browser");
        let id = "magician-flat-cache-unit-test";
        forget_flat_browser_session(id); // clean slate

        let first = get_or_create_flat_browser_session(id, || {
            AgentBrowserSession::new_with_session_id(
                id.to_string(),
                ConnectionMode::default(),
                cli.clone(),
            )
        })
        .expect("first build");

        // Same id → cached Arc; the build closure must NOT run again.
        let second = get_or_create_flat_browser_session(id, || {
            panic!("build closure must not run on a cache hit")
        })
        .expect("cache hit");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the same session id must return the cached session handle"
        );

        // Eviction (what close_session_for_thread_with_options does) drops it,
        // so a later execution can't reuse a session whose browser is gone.
        forget_flat_browser_session(id);
        let third = get_or_create_flat_browser_session(id, || {
            AgentBrowserSession::new_with_session_id(
                id.to_string(),
                ConnectionMode::default(),
                cli.clone(),
            )
        })
        .expect("rebuild after eviction");
        let _ = &tmp; // keep the fake-CLI tempdir alive for the whole test
        assert!(
            !Arc::ptr_eq(&first, &third),
            "after eviction a fresh session must be built"
        );
        forget_flat_browser_session(id);
    }

    #[test]
    fn resolve_cli_path_uses_env_override_when_vendored_absent() {
        let _guard = env_lock();
        let prev = std::env::var(ENV_AGENT_BROWSER_CLI).ok();
        let tmp = tempfile::tempdir().expect("temp dir");
        let cli = make_fake_cli(&tmp, "agent-browser");
        std::env::set_var(ENV_AGENT_BROWSER_CLI, &cli);

        let resolved = AgentBrowserSession::resolve_cli_path(None).expect("env override resolves");
        assert_eq!(resolved, cli);

        match prev {
            Some(v) => std::env::set_var(ENV_AGENT_BROWSER_CLI, v),
            None => std::env::remove_var(ENV_AGENT_BROWSER_CLI),
        }
    }

    #[test]
    fn resolve_cli_path_rejects_env_override_when_storage_root_lacks_workspace_cli() {
        let _guard = env_lock();
        let prev = std::env::var(ENV_AGENT_BROWSER_CLI).ok();
        let tmp = tempfile::tempdir().expect("temp dir");
        let env_cli = make_fake_cli(&tmp, "global/agent-browser");
        let storage_root = tempfile::tempdir().expect("storage root");
        std::env::set_var(ENV_AGENT_BROWSER_CLI, &env_cli);

        let err = AgentBrowserSession::resolve_cli_path(Some(storage_root.path()))
            .expect_err("runtime storage root must require pinned CLI");
        let msg = format!("{err}");
        assert!(msg.contains("pinned agent-browser CLI"), "got: {msg}");
        assert!(msg.contains(ENV_AGENT_BROWSER_CLI), "got: {msg}");

        match prev {
            Some(v) => std::env::set_var(ENV_AGENT_BROWSER_CLI, v),
            None => std::env::remove_var(ENV_AGENT_BROWSER_CLI),
        }
    }

    #[test]
    fn resolve_cli_path_rejects_env_override_pointing_at_missing_file() {
        let _guard = env_lock();
        let prev = std::env::var(ENV_AGENT_BROWSER_CLI).ok();
        std::env::set_var(ENV_AGENT_BROWSER_CLI, "/nonexistent/agent-browser");

        let err = AgentBrowserSession::resolve_cli_path(None).expect_err("missing file must error");
        let msg = format!("{err}");
        assert!(msg.contains(ENV_AGENT_BROWSER_CLI), "got: {msg}");

        match prev {
            Some(v) => std::env::set_var(ENV_AGENT_BROWSER_CLI, v),
            None => std::env::remove_var(ENV_AGENT_BROWSER_CLI),
        }
    }

    #[test]
    fn resolve_cli_path_for_scope_prefers_workspace_layer() {
        let _guard = env_lock();
        let prev = std::env::var(ENV_AGENT_BROWSER_CLI).ok();
        std::env::remove_var(ENV_AGENT_BROWSER_CLI);

        let tmp = tempfile::tempdir().expect("temp dir");
        let principal = "alice";
        let workspace = "wsA";
        let workspace_cli = make_fake_cli(
            &tmp,
            &super::scope_vendored_relative_path(principal, workspace),
        );

        let resolved = AgentBrowserSession::resolve_cli_path_for_scope(
            Some(tmp.path()),
            Some(principal),
            Some(workspace),
        )
        .expect("workspace layer resolves");
        assert_eq!(resolved, workspace_cli);

        if let Some(v) = prev {
            std::env::set_var(ENV_AGENT_BROWSER_CLI, v);
        }
    }

    #[test]
    fn resolve_cli_path_for_scope_errors_when_workspace_and_extras_both_absent() {
        let _guard = env_lock();
        let prev = std::env::var(ENV_AGENT_BROWSER_CLI).ok();
        std::env::remove_var(ENV_AGENT_BROWSER_CLI);

        // No workspace cli, no extras configured (OnceLock unset in test).
        // System-shared tier was retired — error must surface remediation.
        let storage_root = tempfile::tempdir().expect("storage root");
        let err = AgentBrowserSession::resolve_cli_path_for_scope(
            Some(storage_root.path()),
            Some("alice"),
            Some("wsA"),
        )
        .expect_err("missing workspace install must error after system retirement");
        let msg = format!("{err}");
        assert!(msg.contains("pinned agent-browser CLI"), "got: {msg}");
        assert!(msg.contains("registry.paths"), "got: {msg}");

        if let Some(v) = prev {
            std::env::set_var(ENV_AGENT_BROWSER_CLI, v);
        }
    }

    #[test]
    fn resolve_cli_path_errors_when_neither_env_nor_vendored_present() {
        let _guard = env_lock();
        let prev = std::env::var(ENV_AGENT_BROWSER_CLI).ok();
        std::env::remove_var(ENV_AGENT_BROWSER_CLI);

        let tmp = tempfile::tempdir().expect("temp dir");
        let err = AgentBrowserSession::resolve_cli_path(Some(tmp.path()))
            .expect_err("must hard-fail without PATH fallback");
        let msg = format!("{err}");
        assert!(msg.contains("install-scope"), "got: {msg}");
        assert!(msg.contains("registry.paths"), "got: {msg}");

        if let Some(v) = prev {
            std::env::set_var(ENV_AGENT_BROWSER_CLI, v);
        }
    }

    #[test]
    fn new_rejects_nonexistent_path() {
        let err = AgentBrowserSession::new(
            "thread-1",
            ConnectionMode::default(),
            PathBuf::from("/definitely/not/here/agent-browser"),
        )
        .expect_err("missing path must fail");
        let msg = format!("{err}");
        assert!(msg.contains("not found"), "got: {msg}");
    }

    #[test]
    fn new_accepts_existing_path() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cli = make_fake_cli(&tmp, "agent-browser");
        let s = AgentBrowserSession::new("thread-1", ConnectionMode::default(), cli)
            .expect("existing path must construct OK");
        assert_eq!(s.session_id(), "magician-thread-1");
    }

    #[test]
    fn initial_url_is_used_for_session_bootstrap_when_present() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cli = make_fake_cli(&tmp, "agent-browser");
        let s = AgentBrowserSession::new("thread-1", ConnectionMode::Headed, cli)
            .expect("existing path must construct OK")
            .with_initial_url(Some(
                " http://127.0.0.1:5173/tests/sota-tests/23-download-upload-cycles.html "
                    .to_string(),
            ));

        assert_eq!(
            s.initial_open_target(),
            "http://127.0.0.1:5173/tests/sota-tests/23-download-upload-cycles.html"
        );
    }

    #[test]
    fn blank_initial_url_keeps_legacy_about_blank_bootstrap() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cli = make_fake_cli(&tmp, "agent-browser");
        let s = AgentBrowserSession::new("thread-1", ConnectionMode::Headed, cli)
            .expect("existing path must construct OK")
            .with_initial_url(Some("about:blank".to_string()));

        assert_eq!(s.initial_open_target(), "about:blank");
    }

    #[test]
    fn session_id_sanitizes_unsafe_chars() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let cli = make_fake_cli(&tmp, "agent-browser");
        let s = AgentBrowserSession::new(
            "thread/with spaces;and stuff",
            ConnectionMode::default(),
            cli,
        )
        .expect("constructs");
        assert_eq!(s.session_id(), "magician-thread_with_spaces_and_stuff");
    }

    #[test]
    fn default_connection_mode_is_cdp_with_proxy_url() {
        match ConnectionMode::default() {
            ConnectionMode::Cdp { url } => assert_eq!(url, DEFAULT_MAGICUTOR_PROXY_URL),
            other => panic!("default must be CDP, got {other:?}"),
        }
    }

    #[test]
    fn ordinary_call_uses_configured_cdp_url_without_a_call_override() {
        let configured = "ws://127.0.0.1:3999/devtools/browser/magicutor-proxy";
        match ConnectionMode::from_call_arguments(
            &serde_json::json!({"connection_mode": "cdp"}),
            configured,
        ) {
            ConnectionMode::Cdp { url } => assert_eq!(url, configured),
            other => panic!("configured default must remain CDP, got {other:?}"),
        }
    }

    #[test]
    fn engine_names_are_path_safe_and_no_engine_has_no_implicit_fallback() {
        assert!(is_valid_browser_engine_name("custom-browser.v2"));
        for invalid in [
            "",
            ".",
            "..",
            "../browser",
            "nested/browser",
            "browser name",
        ] {
            assert!(!is_valid_browser_engine_name(invalid), "{invalid}");
        }

        let root = tempfile::tempdir().unwrap();
        let resolved = resolve_browser_engine(
            root.path(),
            "owner",
            "default",
            &ConnectionMode::Headless,
            None,
        )
        .unwrap();
        assert!(resolved.name.is_none());
        assert!(resolved.env.is_empty());

        let explicit_bundled = resolve_browser_engine(
            root.path(),
            "owner",
            "default",
            &ConnectionMode::Headless,
            Some(BUNDLED_BROWSER_ENGINE_NAME),
        )
        .unwrap();
        assert_eq!(
            explicit_bundled.name.as_deref(),
            Some(BUNDLED_BROWSER_ENGINE_NAME)
        );
        assert!(explicit_bundled.env.is_empty());
    }

    #[test]
    fn explicit_missing_engine_does_not_silently_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let error = resolve_browser_engine(
            root.path(),
            "owner",
            "default",
            &ConnectionMode::Headless,
            Some("not-installed-browser"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not-installed-browser"));
    }

    #[test]
    fn lightpanda_is_rejected_for_headed_mode_before_resolver_launch() {
        let root = tempfile::tempdir().unwrap();
        let error = resolve_browser_engine(
            root.path(),
            "owner",
            "default",
            &ConnectionMode::Headed,
            Some(LIGHTPANDA_BROWSER_ENGINE_NAME),
        )
        .unwrap_err();
        assert!(error.to_string().contains("headless-only"));
    }

    #[test]
    fn cloak_engine_plan_has_one_bundled_capacity_fallback() {
        let root = tempfile::tempdir().unwrap();
        let resolver = root
            .path()
            .join("scopes/owner/default/skills/cloak-browser/scripts/resolve.py");
        std::fs::create_dir_all(resolver.parent().unwrap()).unwrap();
        std::fs::write(
            resolver,
            "import json\nprint(json.dumps({'binary_path': '/tmp/cloak-browser'}))\n",
        )
        .unwrap();

        let plan = resolve_browser_engine_plan(
            root.path(),
            "owner",
            "default",
            &ConnectionMode::Headless,
            Some(CLOAK_BROWSER_ENGINE_NAME),
        )
        .unwrap();
        assert_eq!(
            plan.primary.name.as_deref(),
            Some(CLOAK_BROWSER_ENGINE_NAME)
        );
        assert_eq!(
            plan.primary
                .env
                .get("AGENT_BROWSER_EXECUTABLE_PATH")
                .map(String::as_str),
            Some("/tmp/cloak-browser")
        );
        let fallback = plan.capacity_fallback.expect("capacity fallback");
        assert_eq!(fallback.name.as_deref(), Some(BUNDLED_BROWSER_ENGINE_NAME));
        assert!(fallback.env.is_empty());
    }

    #[test]
    fn cloak_engine_resolves_both_headless_and_headed_modes() {
        let root = tempfile::tempdir().unwrap();
        let resolver = root
            .path()
            .join("scopes/owner/default/skills/cloak-browser/scripts/resolve.py");
        std::fs::create_dir_all(resolver.parent().unwrap()).unwrap();
        std::fs::write(
            resolver,
            "import json, sys\nprint(json.dumps({'headed': '--headed' in sys.argv}))\n",
        )
        .unwrap();

        let headless = resolve_browser_engine(
            root.path(),
            "owner",
            "default",
            &ConnectionMode::Headless,
            Some(CLOAK_BROWSER_ENGINE_NAME),
        )
        .unwrap();
        assert_eq!(
            headless.env.get("AGENT_BROWSER_HEADED").map(String::as_str),
            Some("false")
        );

        let headed = resolve_browser_engine(
            root.path(),
            "owner",
            "default",
            &ConnectionMode::Headed,
            Some(CLOAK_BROWSER_ENGINE_NAME),
        )
        .unwrap();
        assert_eq!(
            headed.env.get("AGENT_BROWSER_HEADED").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn capacity_classifier_is_exact_to_cloak_session_limit() {
        let result = |stderr: &str| AgentBrowserToolResult {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
            parsed_json: None,
            elapsed_ms: 1,
            stdout_truncated: false,
            stderr_truncated: false,
        };
        assert!(looks_like_cloak_capacity_error(&result(
            "Chrome exited early (exit code: 76) without writing DevToolsActivePort"
        )));
        assert!(looks_like_cloak_capacity_error(&result(
            "CloakBrowser Pro: session limit reached for your plan"
        )));
        assert!(!looks_like_cloak_capacity_error(&result(
            "Chrome exited early (exit code: 77) without writing DevToolsActivePort"
        )));
        assert!(!looks_like_cloak_capacity_error(&result(
            "navigation failed with HTTP 429"
        )));
    }

    #[test]
    fn analytics_labels_never_copy_sensitive_command_arguments() {
        assert_eq!(
            browser_operation_label(&["open", "https://example.test/?token=secret"]),
            "open"
        );
        assert_eq!(browser_operation_label(&["click", "@e17"]), "click");
        assert_eq!(
            browser_operation_label(&["get", "text", "body"]),
            "get text"
        );
        assert_eq!(
            browser_operation_label(&["storage", "local", "secret-key"]),
            "storage local"
        );
    }

    #[tokio::test]
    async fn cloak_capacity_failure_retires_daemon_and_retries_once_with_bundled_engine() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, log) = make_capacity_recording_cli(&temp);
        let mut primary_env = HashMap::new();
        primary_env.insert("MAGICIAN_TEST_BROWSER_ENGINE".into(), "cloak".into());
        let mut fallback_env = HashMap::new();
        fallback_env.insert("MAGICIAN_TEST_BROWSER_ENGINE".into(), "bundled".into());
        let plan = BrowserEnginePlan {
            primary: ResolvedBrowserEngine {
                name: Some(CLOAK_BROWSER_ENGINE_NAME.into()),
                env: primary_env,
            },
            capacity_fallback: Some(ResolvedBrowserEngine {
                name: Some(BUNDLED_BROWSER_ENGINE_NAME.into()),
                env: fallback_env,
            }),
        };
        let session = AgentBrowserSession::new("capacity", ConnectionMode::Headless, cli)
            .unwrap()
            .with_engine_plan(plan)
            .with_analytics_context(BrowserEngineAnalyticsContext::for_scope(
                temp.path(),
                "owner",
                "default",
                Some("execution-1".to_string()),
                Some("task-1".to_string()),
            ));

        session.ensure_connected().await.unwrap();
        let command = session.run_command(&["get", "url"]).await.unwrap();
        assert!(command.success);
        assert_eq!(
            session.active_engine_name().as_deref(),
            Some(BUNDLED_BROWSER_ENGINE_NAME)
        );
        let calls = std::fs::read_to_string(log).unwrap();
        let lines = calls.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 4, "{calls}");
        assert!(lines[0].starts_with("cloak|"));
        assert!(lines[0].contains(" open "));
        assert!(lines[1].starts_with("cloak|"));
        assert!(lines[1].ends_with(" close"));
        assert!(lines[2].starts_with("bundled|"));
        assert!(lines[2].contains(" open "));
        assert!(lines[3].starts_with("bundled|"));
        assert!(lines[3].ends_with(" get url"));

        let analytics = crate::magician_v2::browser_engine_analytics::list_browser_engine_usage(
            temp.path(),
            "owner",
            "default",
            crate::magician_v2::browser_engine_analytics::BrowserEngineUsageFilter::default(),
        )
        .await
        .unwrap();
        assert_eq!(analytics.total_count, 4);
        assert!(analytics.items.iter().any(|row| {
            row.engine == CLOAK_BROWSER_ENGINE_NAME
                && !row.success
                && row.error_class.as_deref() == Some("capacity_limit")
        }));
        assert!(analytics.items.iter().any(|row| {
            row.engine == BUNDLED_BROWSER_ENGINE_NAME
                && row.success
                && row.fallback_from.as_deref() == Some(CLOAK_BROWSER_ENGINE_NAME)
        }));
    }

    #[tokio::test]
    async fn non_capacity_cloak_license_failure_does_not_switch_engines() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, log) = make_capacity_recording_cli(&temp);
        let mut primary_env = HashMap::new();
        primary_env.insert("MAGICIAN_TEST_BROWSER_ENGINE".into(), "cloak".into());
        primary_env.insert("MAGICIAN_TEST_CLOAK_EXIT_CODE".into(), "77".into());
        let plan = BrowserEnginePlan {
            primary: ResolvedBrowserEngine {
                name: Some(CLOAK_BROWSER_ENGINE_NAME.into()),
                env: primary_env,
            },
            capacity_fallback: Some(ResolvedBrowserEngine {
                name: Some(BUNDLED_BROWSER_ENGINE_NAME.into()),
                env: HashMap::new(),
            }),
        };
        let session = AgentBrowserSession::new("invalid-key", ConnectionMode::Headless, cli)
            .unwrap()
            .with_engine_plan(plan);

        let error = session.ensure_connected().await.unwrap_err().to_string();
        assert!(error.contains("exit code: 77"), "{error}");
        assert_eq!(
            session.active_engine_name().as_deref(),
            Some(CLOAK_BROWSER_ENGINE_NAME)
        );
        let calls = std::fs::read_to_string(log).unwrap();
        assert_eq!(calls.lines().count(), 1, "{calls}");
        assert!(!calls.contains(" close"), "{calls}");
        assert!(!calls.contains("bundled|"), "{calls}");
    }

    #[tokio::test]
    async fn non_cloak_engine_cannot_activate_capacity_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, log) = make_capacity_recording_cli(&temp);
        let mut primary_env = HashMap::new();
        // Make the fake CLI emit Cloak's exact capacity text while declaring a
        // different resolved engine. The engine identity must win this gate.
        primary_env.insert("MAGICIAN_TEST_BROWSER_ENGINE".into(), "cloak".into());
        let plan = BrowserEnginePlan {
            primary: ResolvedBrowserEngine {
                name: Some("another-browser".into()),
                env: primary_env,
            },
            capacity_fallback: Some(ResolvedBrowserEngine {
                name: Some(BUNDLED_BROWSER_ENGINE_NAME.into()),
                env: HashMap::new(),
            }),
        };
        let session = AgentBrowserSession::new("non-cloak", ConnectionMode::Headless, cli)
            .unwrap()
            .with_engine_plan(plan);

        let error = session.ensure_connected().await.unwrap_err().to_string();
        assert!(error.contains("exit code: 76"), "{error}");
        assert_eq!(
            session.active_engine_name().as_deref(),
            Some("another-browser")
        );
        let calls = std::fs::read_to_string(log).unwrap();
        assert_eq!(calls.lines().count(), 1, "{calls}");
        assert!(!calls.contains(" close"), "{calls}");
        assert!(!calls.contains("bundled|"), "{calls}");
    }

    #[tokio::test]
    async fn failed_capacity_fallback_is_bounded_to_one_retry() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, log) = make_capacity_recording_cli(&temp);
        let mut primary_env = HashMap::new();
        primary_env.insert("MAGICIAN_TEST_BROWSER_ENGINE".into(), "cloak".into());
        let mut fallback_env = HashMap::new();
        fallback_env.insert("MAGICIAN_TEST_BROWSER_ENGINE".into(), "bundled".into());
        fallback_env.insert("MAGICIAN_TEST_BUNDLED_FAIL".into(), "1".into());
        let plan = BrowserEnginePlan {
            primary: ResolvedBrowserEngine {
                name: Some(CLOAK_BROWSER_ENGINE_NAME.into()),
                env: primary_env,
            },
            capacity_fallback: Some(ResolvedBrowserEngine {
                name: Some(BUNDLED_BROWSER_ENGINE_NAME.into()),
                env: fallback_env,
            }),
        };
        let session = AgentBrowserSession::new("bounded", ConnectionMode::Headless, cli)
            .unwrap()
            .with_engine_plan(plan);

        let error = session.ensure_connected().await.unwrap_err().to_string();
        assert!(error.contains("exit code: 76"), "{error}");
        assert_eq!(
            session.active_engine_name().as_deref(),
            Some(BUNDLED_BROWSER_ENGINE_NAME)
        );
        let calls = std::fs::read_to_string(log).unwrap();
        let lines = calls.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{calls}");
        assert!(lines[0].starts_with("cloak|"));
        assert!(lines[1].starts_with("cloak|"));
        assert!(lines[1].ends_with(" close"));
        assert!(lines[2].starts_with("bundled|"));
        assert!(lines[2].contains(" open "));
    }
}
