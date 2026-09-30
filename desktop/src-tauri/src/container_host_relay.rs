//! Local managed-container transport. Authority is the desktop-owned runtime
//! exec pipe, not a container IP, bearer token on HTTP, or a new host listener.
//! Typed Apps identity/pairing endpoints deliberately do not cross this relay.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::json;
use std::{process::Stdio, time::Duration};
use tauri::Manager;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const WORKER: &str = include_str!("container_host_relay.py");
const REQUEST_LIMIT: usize = 1024 * 1024;
const RESPONSE_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    method: String,
    path: String,
    body: String,
}

fn allowed(method: &str, path: &str) -> bool {
    match method {
        "GET" => matches!(
            path,
            "/health"
                | "/host/runtime/endpoints"
                | "/host/status"
                | "/host/presence/status"
                | "/host/automation/status"
                | "/host/speech/status"
        ),
        "POST" => {
            matches!(
                path,
                "/host/screen/capture"
                    | "/host/applescript"
                    | "/host/imessage/query"
                    | "/host/overlay/draw"
                    | "/host/contextual-assist/open"
                    | "/host/app/open"
                    | "/host/reminders/create"
                    | "/host/speech/transcribe"
                    | "/host/speech/synthesize"
            ) || path.strip_prefix("/host/ax/").is_some_and(|action| {
                !action.is_empty()
                    && action.len() <= 64
                    && action
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'_')
            })
        },
        _ => false,
    }
}

async fn forward(client: &reqwest::Client, request: Request) -> Result<(u16, Vec<u8>), String> {
    if !allowed(&request.method, &request.path) {
        return Ok((
            403,
            br#"{"error":"route not available through container relay"}"#.to_vec(),
        ));
    }
    let body = STANDARD
        .decode(&request.body)
        .map_err(|_| "invalid relay body")?;
    if body.len() > REQUEST_LIMIT {
        return Err("relay body too large".into());
    }
    if request.path == "/host/imessage/query" {
        // This capability belongs exclusively to the private managed-runtime
        // channel. Do not add it to the host's public HTTP route table.
        return match crate::host_imessage::handle(&body).await {
            Ok(value) => Ok((
                200,
                serde_json::to_vec(&value).map_err(|_| "query encoding failed")?,
            )),
            Err(error) => Ok((400, serde_json::to_vec(&json!({"error": error})).unwrap())),
        };
    }
    let method =
        reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| "invalid method")?;
    // Never use a caller-provided origin, headers or redirects.
    let mut response = client
        .request(method, format!("http://127.0.0.1:3017{}", request.path))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| "desktop gateway unavailable")?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "gateway response failed")?
    {
        if body.len() + chunk.len() > RESPONSE_LIMIT {
            return Err("gateway response too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}

async fn session(mut command: tokio::process::Command) -> Result<(), String> {
    let mut child = command
        .args(["/usr/bin/python3", "-u", "-c", WORKER])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("relay exec failed: {e}"))?;
    let mut input = child.stdin.take().ok_or("relay stdin unavailable")?;
    let output = child.stdout.take().ok_or("relay stdout unavailable")?;
    let mut output = BufReader::new(output);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(130))
        .build()
        .map_err(|_| "relay client unavailable")?;
    let mut ready = false;
    loop {
        let mut line = Vec::new();
        // Includes base64 overhead and JSON framing, before allocating an
        // unbounded line from an untrusted container.
        let mut limited = (&mut output).take((REQUEST_LIMIT * 2) as u64);
        let read = limited.read_until(b'\n', &mut line);
        let size = if ready {
            read.await
        } else {
            tokio::time::timeout(Duration::from_secs(20), read)
                .await
                .map_err(|_| "relay startup timed out")?
        }
        .map_err(|_| "relay pipe read failed")?;
        if size == 0 || line.last() != Some(&b'\n') {
            return Err("relay pipe closed or frame exceeded limit".into());
        }
        if !ready {
            if serde_json::from_slice::<serde_json::Value>(&line).ok() != Some(json!({"ready": 1}))
            {
                return Err("relay readiness handshake failed".into());
            }
            ready = true;
            tracing::info!("Managed container host relay ready on Linux loopback:3017");
            continue;
        }
        let request: Request = serde_json::from_slice(&line).map_err(|_| "invalid relay frame")?;
        let (status, body) = match forward(&client, request).await {
            Ok(response) => response,
            Err(error) => (502, serde_json::to_vec(&json!({"error": error})).unwrap()),
        };
        let mut response =
            serde_json::to_vec(&json!({"status": status, "body": STANDARD.encode(body)}))
                .map_err(|_| "relay response encoding failed")?;
        response.push(b'\n');
        tokio::time::timeout(Duration::from_secs(15), input.write_all(&response))
            .await
            .map_err(|_| "relay response write timed out")?
            .map_err(|_| "relay response pipe closed")?;
    }
}

pub async fn monitor(app: tauri::AppHandle) {
    loop {
        let state = app.state::<crate::AppState>();
        let cfg = state.config.lock().await.clone();
        let runtime = state.runtime.lock().await.clone();
        if cfg.host_gateway.enabled
            && crate::manage_runtime_stack_enabled(&cfg)
            && crate::engine_roots::should_supervise_local_engine(&cfg)
        {
            if let Some(runtime) = runtime {
                if let Ok(command) = runtime.host_relay_command(&cfg.general.container_name) {
                    let run = session(command);
                    tokio::pin!(run);
                    loop {
                        tokio::select! {
                            result = &mut run => {
                                if let Err(error) = result { tracing::warn!(%error, "Container host relay disconnected; will retry"); }
                                break;
                            }
                            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                                let latest = state.config.lock().await.clone();
                                if latest.general.container_name != cfg.general.container_name
                                    || !latest.host_gateway.enabled
                                    || !crate::manage_runtime_stack_enabled(&latest)
                                    || !crate::engine_roots::should_supervise_local_engine(&latest) {
                                    break; // Dropping session closes stdin and kills only its exec.
                                }
                            }
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn container_routing_relay_excludes_typed_authority_and_noncanonical_paths() {
        for path in [
            "/host/apps/macos/pairing/start",
            "/host/apps/android/owner/status",
            "/host/ax/../apps/macos/action",
            "/host/ax/%2e%2e",
            "/host/applescript?x=1",
            "http://example.test/host/applescript",
            "//example.test",
            "/host/ax/click\r\nX: y",
        ] {
            assert!(!allowed("POST", path), "{path}");
        }
        assert!(!allowed("GET", "/host/applescript"));
        assert!(allowed("POST", "/host/ax/snapshot"));
        assert!(allowed("POST", "/host/applescript"));
        assert!(allowed("GET", "/host/automation/status"));
    }
}
