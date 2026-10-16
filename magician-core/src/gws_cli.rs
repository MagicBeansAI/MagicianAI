//! Shared helpers for shelling out to the `gws` (Google Workspace) CLI.
//!
//! Extracted from `api/meetings_api.rs` so the meetings surface and the
//! channel-assist Gmail client resolve the binary and interpret failures the
//! same way. Auth stays entirely inside the CLI: callers point
//! `GOOGLE_WORKSPACE_CLI_CONFIG_DIR` at a per-account profile under the
//! scope capability auth root and `CLOUDSDK_CONFIG` at a subdirectory of
//! it — Rust never touches Google tokens.

use std::path::PathBuf;
use std::process::Output;
use std::time::Duration;

/// One `gws` invocation is a network round-trip to a Google API; cap it so
/// a wedged CLI can never hang a request handler or worker loop.
pub const GWS_TIMEOUT: Duration = Duration::from_secs(20);

/// Resolve the gws binary: explicit env override, then the skillshub-local
/// install (the server runs from the repo root), then PATH.
pub fn gws_binary() -> PathBuf {
    if let Ok(p) = std::env::var("GWS_BINARY") {
        if !p.trim().is_empty() {
            return PathBuf::from(p);
        }
    }
    let local = PathBuf::from("skillshub/node_modules/.bin/gws");
    if local.exists() {
        return local;
    }
    PathBuf::from("gws")
}

/// The API error message gws prints as JSON on STDOUT
/// (`{"error":{"message":...}}`), if the buffer holds one.
fn stdout_error_message(stdout: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    v.pointer("/error/message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
}

/// Extract the REAL failure from a failed gws invocation: gws prints its
/// error as JSON on STDOUT (`{"error":{"message":...}}`) and informational
/// noise ("Using keyring backend …") on stderr — preferring stderr buries
/// the actual error behind the noise. `context` names the surface for the
/// message prefix (e.g. "calendar", "gmail").
pub fn gws_error_detail(context: &str, output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(msg) = stdout_error_message(&stdout) {
        return format!("gws {context} failed: {msg}");
    }
    let detail = if stdout.trim().is_empty() {
        String::from_utf8_lossy(&output.stderr).trim().to_string()
    } else {
        stdout.trim().to_string()
    };
    format!(
        "gws {context} failed: {}",
        detail.chars().take(400).collect::<String>()
    )
}

/// The HTTP status code from a failed invocation's stdout error JSON
/// (`{"error":{"code":404,...}}`), when present. Callers use this to
/// branch on specific API failures — e.g. Gmail `history.list` returning
/// 404 for an expired `startHistoryId` means "fall back to a full re-list".
pub fn gws_error_api_code(output: &Output) -> Option<i64> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|v| v.pointer("/error/code").and_then(|c| c.as_i64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_error_message_extracts_api_error() {
        let msg = stdout_error_message(
            r#"{"error":{"code":404,"message":"Requested entity was not found.","reason":"notFound"}}"#,
        );
        assert_eq!(msg.as_deref(), Some("Requested entity was not found."));
    }

    #[test]
    fn stdout_error_message_ignores_non_error_json_and_garbage() {
        assert_eq!(stdout_error_message(r#"{"messages":[]}"#), None);
        assert_eq!(stdout_error_message("not json at all"), None);
        assert_eq!(stdout_error_message(""), None);
    }
}
