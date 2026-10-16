//! Primitive-dispatch contract: the dispatcher trait + its result type.
//!
//! The nested per-tool LLM loop this file once drove was retired in the flatten
//! migration (flat-loop Phase 7). What remains is the contract the flat
//! per-action loop speaks: [`PrimitiveDispatcher`] (implemented by the browser /
//! CLI-template / compiled-provider dispatchers) and the [`PrimitiveToolResult`]
//! it returns for one `<pack>__<action>` primitive call.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use super::artifacts::PrimitiveArtifact;

/// Dispatches one non-control tool primitive (`<pack>__<action>`).
#[async_trait]
pub trait PrimitiveDispatcher: Send + Sync {
    async fn dispatch(&self, tool_name: &str, arguments: &Value) -> Result<PrimitiveToolResult>;

    /// Optional always-on contextual header a dispatcher can surface ahead of
    /// the result (open browser tabs, inbox status, …) so the caller doesn't
    /// have to issue a discovery call. Default `None`; the browser dispatcher
    /// overrides to render the owned-tab inventory from
    /// `magicutor::server::cdp_proxy::list_owned_tab_inventory`.
    async fn iteration_header(&self) -> Option<String> {
        None
    }

    /// Optional viewport dimensions for the active session, in CSS pixels.
    /// Browser packs return their session's `(width, height)` so the LLM
    /// router can inject `extra.viewport` for vision-cohort providers
    /// (Yutori N1) that emit normalized 1000×1000 coordinates and need
    /// caller-side denormalization to actual viewport pixels. Non-browser
    /// dispatchers return `None`; the router skips the injection.
    ///
    /// Async because browser packs query the live session via CDP/eval so
    /// user-driven window resizes are honoured.
    async fn viewport(&self) -> Option<(u32, u32)> {
        None
    }

    /// Extra tool names this dispatcher recognises beyond its declared catalog.
    /// Lets vendor-emitted action names (Yutori N1.5's `left_click`,
    /// `mouse_move`, `drag`, `scroll`, `goto_url`, …) flow through to
    /// `dispatch()` instead of being rejected at the catalog-name gate; the
    /// dispatcher does the actual translation. Default: empty.
    fn extra_tool_names(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Result returned by a primitive dispatcher for one tool call.
#[derive(Debug, Clone, Default)]
pub struct PrimitiveToolResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub parsed_json: Option<Value>,
    pub artifacts: Vec<PrimitiveArtifact>,
    pub elapsed_ms: u64,
}
