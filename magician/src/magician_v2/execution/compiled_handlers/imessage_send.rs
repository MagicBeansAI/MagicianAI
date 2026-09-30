//! `imessage_send` — send an iMessage / SMS from the user's Mac (Messages app).
//!
//! A NARROW companion to `macos_automation`: it builds a FIXED AppleScript
//! template with the recipient + body escaped (it never runs arbitrary script),
//! then relays it through the Tauri host gateway (`HostAutomationProvider`).
//! Works natively and from a container — one code path. Sends from the USER's
//! own Messages account on the host; a down gateway surfaces as `status: error`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::media_seam::{
    host_gateway_url_from_env, AppleScriptRequest, HostAutomationProvider,
};

/// Escape a value for safe embedding inside an AppleScript double-quoted string
/// literal: backslash first, then the quote. Prevents the recipient/body from
/// breaking out of the fixed template (AppleScript injection).
pub fn applescript_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub async fn handle(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let to = args
        .get("to")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(to) = to else {
        return Ok(json!({
            "status": "error",
            "reason": "imessage_send requires a non-empty `to` (phone number, Apple ID email, or buddy handle).",
        }));
    };
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(text) = text else {
        return Ok(json!({
            "status": "error",
            "reason": "imessage_send requires a non-empty `text` (message body).",
        }));
    };
    // `service` is restricted to two known AppleScript enum constants in Rust —
    // it is NEVER interpolated from arbitrary input, so no escaping is needed
    // (and it must stay unquoted in the script: `service type = iMessage`).
    let service = match args.get("service").and_then(Value::as_str) {
        Some(s) if s.eq_ignore_ascii_case("sms") => "SMS",
        _ => "iMessage",
    };

    let script = format!(
        "tell application \"Messages\"\n\
         \tset targetService to 1st account whose service type = {service}\n\
         \tset targetParticipant to participant \"{to}\" of targetService\n\
         \tsend \"{text}\" to targetParticipant\n\
         end tell",
        service = service,
        to = applescript_escape(to),
        text = applescript_escape(text),
    );

    let provider = HostAutomationProvider::new(host_gateway_url_from_env());
    match provider
        .run_applescript(AppleScriptRequest {
            source: script,
            language: None,
            timeout_secs: Some(30),
        })
        .await
    {
        Ok(result) if result.exit_code == 0 => Ok(json!({
            "status": "ok",
            "to": to,
            "service": service,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "exit_code": result.exit_code,
        })),
        Ok(result) => Ok(json!({
            "status": "error",
            "reason": format!(
                "Messages returned exit {} sending to {to} via {service}: {}",
                result.exit_code,
                result.stderr.trim(),
            ),
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

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_and_backslashes() {
        assert_eq!(applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        // A benign handle/text round-trips unchanged.
        assert_eq!(applescript_escape("+14155551234"), "+14155551234");
    }
}
