//! Browser-specific primitive dispatcher.
//!
//! Owns the `agent-browser` CLI subprocess driver ([`session`]) and the
//! per-primitive argv translator ([`dispatch`]). Wired into the flat
//! dispatch path via [`super::dispatch::dispatch_browser_primitive`].

pub mod dispatch;
pub mod har;
pub mod in_page_fetch;
pub mod owned_tabs_header;
pub(crate) mod secure_prompt_fill;
pub mod session;
pub mod trace_drain;
pub mod yutori_translator;

pub use dispatch::BrowserDispatcher;
pub use session::{
    close_session_by_id_with_options, close_session_for_thread,
    close_session_for_thread_with_options, flat_browser_artifact_dir,
    flat_browser_sessions_for_thread, get_or_create_flat_browser_session,
    is_valid_browser_engine_name, resolve_browser_engine, resolve_browser_engine_env,
    resolve_browser_engine_plan, AgentBrowserSession, AgentBrowserToolResult, BrowserEnginePlan,
    BrowserTransport, BrowserTransportCeiling, ConnectionMode, ControllerOwnedBrowserSession,
    ResolvedBrowserEngine, RetrievalBrowserMode, BUNDLED_BROWSER_ENGINE_NAME,
    DEFAULT_COMMAND_TIMEOUT_SECS, DEFAULT_MAGICUTOR_PROXY_URL, LIGHTPANDA_BROWSER_ENGINE_NAME,
};
