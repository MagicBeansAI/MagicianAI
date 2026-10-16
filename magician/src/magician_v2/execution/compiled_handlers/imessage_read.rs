//! iMessage reads that cross to the signed desktop, which owns the durable Full
//! Disk Access grant: Linux containers over the private relay, and native macOS
//! backends — whose own read macOS refused — over the loopback gateway with the
//! desktop's per-launch token. The governed provider still owns
//! admission/formatting.
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct HostRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Whether macOS refused this process's own open of `chat.db` — the only case
/// that goes to the desktop. TCC refuses the open, not the lookup, so only an
/// open attempt answers this; an absent file is not a refusal.
pub fn local_read_denied(path: &std::path::Path) -> bool {
    matches!(
        std::fs::File::open(path),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied
    )
}

/// Header and runtime-root file shared with `desktop/src-tauri/src/host_imessage.rs`.
const LOCAL_TOKEN_HEADER: &str = "x-magician-imessage-token";
const LOCAL_TOKEN_FILE: &str = "run/host-imessage.token";

/// The desktop's token when it has published one. The container relay does not
/// need it, so its absence is not an error here; the gateway refuses instead.
fn local_token() -> Option<String> {
    let path = crate::magician_v2::process_storage::runtime_root().join(LOCAL_TOKEN_FILE);
    std::fs::read_to_string(path)
        .ok()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

pub async fn query(sql: &str, timeout_secs: u64) -> Result<HostRows, String> {
    let deadline = timeout_secs.clamp(1, 30);
    let base = crate::magician_v2::media_seam::host_gateway_url_from_env();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(deadline + 5))
        .build()
        .map_err(|_| "Cannot initialize Messages host client")?;
    let mut request = client
        .post(format!(
            "{}/host/imessage/query",
            base.trim_end_matches('/')
        ))
        .json(&json!({"sql": sql, "timeout_secs": deadline}));
    if let Some(token) = local_token() {
        request = request.header(LOCAL_TOKEN_HEADER, token);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "Messages host relay is unavailable; start Magican Desktop")?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Messages host response failed")?
    {
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err("Messages host result exceeded its limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let error = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("Messages host returned HTTP {status}"));
        return Err(error);
    }
    let result: HostRows =
        serde_json::from_slice(&body).map_err(|_| "Invalid Messages host result")?;
    if result.columns.len() > 128
        || result.rows.len() > 1000
        || result
            .rows
            .iter()
            .any(|row| row.len() != result.columns.len())
    {
        return Err("Invalid Messages host row shape".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn imessage_desktop_fallback_only_on_a_refused_open() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("chat.db");
        // Absent says nothing about the grant; the direct path reports it.
        assert!(!local_read_denied(&db));
        std::fs::write(&db, b"").unwrap();
        assert!(!local_read_denied(&db));
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root opens anything, so the refusal is only observable unprivileged.
        if std::fs::File::open(&db).is_err() {
            assert!(local_read_denied(&db));
        }
    }
}
