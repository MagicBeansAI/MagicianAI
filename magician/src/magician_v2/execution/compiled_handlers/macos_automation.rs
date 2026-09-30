//! `macos_automation` — run an AppleScript / JXA script on the host Mac.
//!
//! Relays through the Tauri host gateway (`HostAutomationProvider`), so the
//! SAME tool works natively and from a container — one code path. macOS
//! automation can only run on the host (Automation TCC), never in a Linux
//! container, so there is no local fallback: a gateway that is down (no desktop
//! app) surfaces as a structured `status: error` rather than a hard failure.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::media_seam::{
    host_gateway_url_from_env, AppleScriptRequest, HostAutomationProvider,
};

pub async fn handle(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(source) = source else {
        return Ok(json!({
            "status": "error",
            "reason": "macos_automation requires a non-empty `source` (AppleScript or JXA).",
        }));
    };
    let language = args
        .get("language")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timeout_secs = args.get("timeout_secs").and_then(Value::as_u64);

    let provider = HostAutomationProvider::new(host_gateway_url_from_env());
    match provider
        .run_applescript(AppleScriptRequest {
            source: source.to_string(),
            language,
            timeout_secs,
        })
        .await
    {
        Ok(result) => Ok(json!({
            "status": "ok",
            "stdout": result.stdout,
            "stderr": result.stderr,
            "exit_code": result.exit_code,
        })),
        Err(error) => Ok(json!({
            "status": "error",
            "reason": format!("host automation failed: {error}"),
        })),
    }
}
