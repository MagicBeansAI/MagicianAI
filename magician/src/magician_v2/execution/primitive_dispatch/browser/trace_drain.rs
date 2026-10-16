//! Bridge between the magicutor CDP-proxy trace buffer and magician's
//! `TraceManager`.
//!
//! When `agent-browser` runs in CDP-proxy mode, the magicutor proxy buffers
//! `NetworkTraceEvent`s per magician thread (see
//! `magicutor::server::trace_capture`). Magician calls
//! [`drain_and_persist_cdp_proxy_traces`] at inner-loop boundaries to pull
//! those events through `GET /trace/drain/{thread_id}` and flush them to the
//! V3 API-mining trace store via `TraceManager`.
//!
//! Headed/headless modes use an agent-browser HAR and converge on the same
//! redacting trace sink.

use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::Arc;

use magicutor::types::AmbientPageSignal;
use tracing::{debug, warn};
use url::Url;

use super::{AgentBrowserSession, ConnectionMode};
use crate::magician_v2::api_mining::auth_capture::persist_captured_auth_events;
use crate::magician_v2::api_mining::path_safe::ensure_safe_record_id;
use crate::magician_v2::api_mining::trace_manager::TraceManager;
use crate::magician_v2::api_mining::trace_storage::redact_trace;
use crate::magician_v2::api_mining::types::NetworkTraceEvent;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::execution::magicutor_client::{ExecutionConfig, MagicutorClient};
use crate::magician_v2::execution::primitive_dispatch::exec_ctx::PrimitiveExecCtx;

#[derive(Debug, Clone, Default)]
pub struct CdpTraceDrain {
    pub written: usize,
    pub traces: Vec<NetworkTraceEvent>,
}

/// Drain network traces buffered for `session.session_id()` on the magicutor
/// CDP proxy and append them to the per-execution V3 API-mining trace file.
///
/// No-op (returns 0) when:
/// * the session is not in CDP-proxy mode
/// * execution/task identity is absent — there is no meaningful place to
///   flush partial drains
/// * the magicutor build does not expose `/trace/drain/{thread_id}` (the
///   client converts 404 → empty drain)
///
/// Errors are logged at WARN and swallowed; trace draining must never
/// surface as an inner-loop failure.
pub async fn drain_and_persist_cdp_proxy_traces(
    session: &Arc<AgentBrowserSession>,
    exec_ctx: &PrimitiveExecCtx,
) -> CdpTraceDrain {
    let enabled = exec_ctx.api_router.as_ref().is_some_and(|router| {
        router
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .config()
            .any_enabled()
    });
    if !enabled {
        return CdpTraceDrain::default();
    }
    let process_enabled = crate::magician_v2::api_mining::switch::runtime_api_mining_config()
        .map(|config| config.enabled)
        .unwrap_or(false);
    let Some(base) = exec_ctx.api_mining_base_path.as_deref() else {
        return CdpTraceDrain::default();
    };
    if !crate::magician_v2::api_mining::switch::ApiMiningSwitch::effective_from_disk(
        process_enabled,
        base,
    ) {
        return CdpTraceDrain::default();
    }
    let url = match session.mode() {
        ConnectionMode::Cdp { url } => url,
        ConnectionMode::Headed | ConnectionMode::Headless => return CdpTraceDrain::default(),
    };

    let Some(http_base) = http_base_from_cdp_url(url) else {
        warn!(
            session = session.session_id(),
            cdp_url = url,
            "trace_drain: could not derive magicutor HTTP base; skipping drain"
        );
        return CdpTraceDrain::default();
    };

    let Some(task_id) = exec_ctx.task_id.as_deref() else {
        debug!(
            session = session.session_id(),
            "trace_drain: task_id missing from exec ctx; skipping persist"
        );
        return CdpTraceDrain::default();
    };
    let trace_record_id = exec_ctx
        .execution_id
        .as_deref()
        .or(exec_ctx.legacy_execution_id.as_deref())
        .unwrap_or(task_id);

    let mut config = ExecutionConfig::default();
    config.base_url = http_base;
    let client = match MagicutorClient::new(config) {
        Ok(c) => c,
        Err(err) => {
            warn!(
                session = session.session_id(),
                error = %err,
                "trace_drain: failed to build magicutor client; skipping drain"
            );
            return CdpTraceDrain::default();
        },
    };

    if let Some(secret_store) = exec_ctx.secret_store.as_ref() {
        match client.drain_captured_auth(session.session_id()).await {
            Ok(events) if !events.is_empty() => {
                if let Err(error) =
                    persist_captured_auth_events(&events, secret_store.as_ref(), None)
                {
                    warn!(
                        session = session.session_id(),
                        error = %error,
                        "trace_drain: failed to persist transient CDP auth material"
                    );
                }
            },
            Ok(_) => {},
            Err(error) => {
                debug!(
                    session = session.session_id(),
                    error = %error,
                    "trace_drain: transient CDP auth drain unavailable"
                );
            },
        }
    }

    let page_signals = match client.drain_page_signals(session.session_id()).await {
        Ok(signals) => signals,
        Err(err) => {
            warn!(
                session = session.session_id(),
                error = %err,
                "trace_drain: passive page signal drain HTTP call failed"
            );
            Vec::new()
        },
    };
    if !page_signals.is_empty() {
        let principal = exec_ctx
            .principal
            .as_deref()
            .unwrap_or("anonymous")
            .to_string();
        let workspace = exec_ctx
            .workspace
            .as_deref()
            .unwrap_or("default")
            .to_string();
        match persist_page_signals(
            exec_ctx,
            &principal,
            &workspace,
            task_id,
            session.session_id(),
            &page_signals,
        ) {
            Ok(path) => debug!(
                session = session.session_id(),
                count = page_signals.len(),
                path = %path.display(),
                "trace_drain: persisted passive page signals"
            ),
            Err(err) => warn!(
                session = session.session_id(),
                error = %err,
                "trace_drain: failed to persist passive page signals"
            ),
        }
    }

    let traces = match client.drain_network_traces(session.session_id()).await {
        // Treat Magicutor as an external capture boundary. Sanitizing here as
        // well as at the producer keeps future capture transports from
        // exposing credentials to correlation, sequence compilation, or disk.
        Ok(traces) => traces
            .into_iter()
            .map(|trace| redact_trace(&trace))
            .collect::<Vec<_>>(),
        Err(err) => {
            warn!(
                session = session.session_id(),
                error = %err,
                "trace_drain: drain HTTP call failed"
            );
            return CdpTraceDrain::default();
        },
    };
    if traces.is_empty() {
        return CdpTraceDrain::default();
    }
    let mut traces = traces;
    scrub_traces_with_delivered_values(&mut traces, exec_ctx.delivered_secret_values.as_ref());
    let drained_count = traces.len();
    let drained_traces = traces.clone();

    let principal = exec_ctx
        .principal
        .as_deref()
        .unwrap_or("anonymous")
        .to_string();
    let workspace = exec_ctx
        .workspace
        .as_deref()
        .unwrap_or("default")
        .to_string();

    match persist_traces_for_task(
        &exec_ctx.storage_base_path,
        &principal,
        &workspace,
        trace_record_id,
        session.session_id(),
        traces,
    ) {
        Ok(written) => {
            debug!(
                session = session.session_id(),
                drained = drained_count,
                written,
                "trace_drain: persisted CDP-proxy traces"
            );
            CdpTraceDrain {
                written,
                traces: drained_traces,
            }
        },
        Err(err) => {
            warn!(
                session = session.session_id(),
                error = err,
                "trace_drain: TraceManager.flush_traces failed"
            );
            CdpTraceDrain {
                written: 0,
                traces: drained_traces,
            }
        },
    }
}

/// Common redacting sink for every capture transport.
/// Replace every value the run delivered with `[REDACTED]` wherever a trace
/// could carry it: the URL (a query string), request and response headers,
/// request and response bodies. Value-based, so a form-encoded login POST or
/// a JSON body under any key is covered (P4 observation filtering).
pub fn scrub_traces_with_delivered_values(
    traces: &mut [NetworkTraceEvent],
    delivered: Option<&Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
) {
    let Some(delivered) = delivered else { return };
    let Ok(delivered) = delivered.lock() else {
        return;
    };
    let replacements = crate::magician_v2::secrets::known_value_replacements(&delivered);
    drop(delivered);
    if replacements.is_empty() {
        return;
    }
    let scrub = |text: &mut String| {
        for value in &replacements {
            if text.contains(value.as_str()) {
                *text = text.replace(value.as_str(), "[REDACTED]");
            }
        }
    };
    for trace in traces.iter_mut() {
        scrub(&mut trace.url);
        for value in trace.request_headers.values_mut() {
            scrub(value);
        }
        for value in trace.response_headers.values_mut() {
            scrub(value);
        }
        if let Some(body) = trace.request_body.as_mut() {
            scrub(body);
        }
        if let Some(body) = trace.response_body.as_mut() {
            scrub(body);
        }
    }
}

pub fn persist_traces_for_task(
    storage_base: &std::path::Path,
    principal: &str,
    workspace: &str,
    record_id: &str,
    buffer_key: &str,
    traces: Vec<NetworkTraceEvent>,
) -> Result<usize, String> {
    let manager = TraceManager::with_workspace_root(storage_base);
    manager
        .set_trace_target(
            principal.to_owned(),
            workspace.to_owned(),
            record_id.to_owned(),
        )
        .map_err(|error| error.to_string())?;
    for trace in traces {
        manager
            .add_trace(buffer_key, redact_trace(&trace))
            .map_err(|error| error.to_string())?;
    }
    manager
        .flush_traces(buffer_key)
        .map_err(|error| error.to_string())
}

/// Stop and ingest a headed/headless session's HAR before its transient
/// staging directory is removed by browser cleanup.
pub async fn drain_har_for_task(
    session: &Arc<AgentBrowserSession>,
    storage_base: &std::path::Path,
    principal: &str,
    workspace: &str,
    task_id: &str,
    secret_store: Option<&crate::magician_v2::secrets::SecretStore>,
    delivered: Option<&Arc<std::sync::Mutex<crate::magician_v2::secrets::KnownSecretValues>>>,
) -> CdpTraceDrain {
    if matches!(session.mode(), ConnectionMode::Cdp { .. }) {
        return CdpTraceDrain::default();
    }
    let Some(path) = session.har_path() else {
        return CdpTraceDrain::default();
    };
    let Some(path_str) = path.to_str() else {
        return CdpTraceDrain::default();
    };
    // `har stop <path>` is what writes the archive; `har start` only records.
    match session
        .run_command(&super::har::har_stop_args(path_str))
        .await
    {
        Ok(result) if result.success => {},
        // A failed stop means this run taught the miner nothing, and the task
        // still succeeds, so nothing else will say so. Debug-level hid exactly
        // this once already (the archive path went to `har start`, and every
        // browser run captured zero traces for weeks).
        Ok(result) => warn!(
            session = session.session_id(),
            stderr = %result.stderr.trim(),
            "trace_drain: HAR stop reported failure; this run captured nothing to mine"
        ),
        Err(error) => warn!(
            session = session.session_id(),
            %error,
            "trace_drain: HAR stop failed; this run captured nothing to mine"
        ),
    }
    // Bound the archive before reading/parsing it; per-body limits alone do
    // not bound a busy browser's total HAR allocation.
    const MAX_HAR_BYTES: u64 = 64 * 1024 * 1024;
    let bytes = match std::fs::File::open(path).and_then(|file| {
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(MAX_HAR_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_HAR_BYTES {
            return Err(std::io::Error::other(
                "HAR exceeded the 64 MiB capture limit",
            ));
        }
        Ok(bytes)
    }) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(
                session = session.session_id(),
                path = %path.display(),
                %error,
                "trace_drain: could not read staged HAR"
            );
            return CdpTraceDrain::default();
        },
    };
    let har: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(har) => har,
        Err(error) => {
            warn!(
                session = session.session_id(),
                path = %path.display(),
                %error,
                "trace_drain: staged HAR was malformed"
            );
            return CdpTraceDrain::default();
        },
    };
    let mut traces = super::har::har_to_traces(&har, Some(session.session_id()));
    if traces.is_empty() {
        return CdpTraceDrain::default();
    }
    // The archive carries no cookies at all — not in `headers`, not in the
    // `cookies` array (measured 2026-09-14 against the bundled CLI) — so the
    // session is invisible to everything downstream: nothing is captured for
    // replay, and the recipe compiler cannot tell an authenticated read from
    // an anonymous one, which compiles a cookie-gated endpoint as anonymous
    // and makes every replay 401. Ask the live session for its cookies and put
    // them back on the requests they were sent with. `redact_trace` replaces
    // the value before anything reaches disk; only the presence survives.
    attach_session_cookies(session, &mut traces).await;
    // HAR is the transient auth channel for non-CDP sessions. Persist only
    // auth material into the encrypted scope store before redacting traces.
    if let Some(store) = secret_store {
        if let Err(error) = persist_captured_auth_events(&har_auth_events(&traces), store, None) {
            warn!(session = session.session_id(), %error, "trace_drain: HAR auth capture failed");
        }
    }
    // Every value this run delivered (a typed password, a filled code) is
    // scrubbed out of the archive before it reaches disk or the model —
    // `redact_trace` knows header names and JSON keys, not what was typed.
    scrub_traces_with_delivered_values(&mut traces, delivered);
    let visible = traces
        .iter()
        .map(redact_trace)
        .collect::<Vec<NetworkTraceEvent>>();
    match persist_traces_for_task(
        storage_base,
        principal,
        workspace,
        task_id,
        session.session_id(),
        traces,
    ) {
        Ok(written) => CdpTraceDrain {
            written,
            traces: visible,
        },
        Err(error) => {
            warn!(
                session = session.session_id(),
                %error,
                "trace_drain: failed to persist HAR traces"
            );
            CdpTraceDrain {
                written: 0,
                traces: visible,
            }
        },
    }
}

/// Convert a CDP `ws://host:port/devtools/browser/<id>` URL into the
/// matching `http://host:port/` magicutor HTTP base. Falls through (returns
/// `None`) for unparseable URLs or non-`ws[s]` schemes — the caller treats
/// `None` as "no drain endpoint reachable".
/// Read the live session's cookie jar and stamp each trace with the cookies
/// that request would have carried. Best effort: a session that cannot answer
/// leaves the traces exactly as the archive had them.
async fn attach_session_cookies(
    session: &Arc<AgentBrowserSession>,
    traces: &mut [NetworkTraceEvent],
) {
    let Ok(result) = session.run_command(&["cookies", "get", "--json"]).await else {
        return;
    };
    let Some(payload) = result.success.then_some(result.parsed_json).flatten() else {
        return;
    };
    let cookies = jar_from_payload(&payload);
    if cookies.is_empty() {
        return;
    }
    for trace in traces.iter_mut() {
        if trace
            .request_headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("cookie"))
        {
            continue;
        }
        let header = cookie_header_for_url(&trace.url, &cookies);
        if !header.is_empty() {
            trace.request_headers.insert("cookie".to_owned(), header);
        }
    }
}

/// `{name, value, domain, path}` rows from the CLI's cookie payload.
fn jar_from_payload(payload: &serde_json::Value) -> Vec<(String, String, String, String)> {
    let rows = payload
        .pointer("/data/cookies")
        .or_else(|| payload.get("cookies"))
        .and_then(serde_json::Value::as_array);
    rows.into_iter()
        .flatten()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?.to_owned();
            let value = cookie.get("value")?.as_str()?.to_owned();
            if name.is_empty() {
                return None;
            }
            let domain = cookie
                .get("domain")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim_start_matches('.')
                .to_ascii_lowercase();
            let path = cookie
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("/")
                .to_owned();
            Some((name, value, domain, path))
        })
        .collect()
}

/// The `Cookie` header a browser would send to `url` from this jar: host match
/// (exact or parent-domain) plus path prefix, in jar order.
fn cookie_header_for_url(url: &str, jar: &[(String, String, String, String)]) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return String::new();
    };
    let Some(host) = parsed.host_str().map(str::to_ascii_lowercase) else {
        return String::new();
    };
    let request_path = parsed.path();
    jar.iter()
        .filter(|(_, _, domain, path)| {
            let host_matches =
                domain.is_empty() || host == *domain || host.ends_with(&format!(".{domain}"));
            let path_matches = path.is_empty()
                || path == "/"
                || request_path == path
                || request_path.starts_with(&format!("{}/", path.trim_end_matches('/')));
            host_matches && path_matches
        })
        .map(|(name, value, _, _)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn har_auth_events(
    traces: &[NetworkTraceEvent],
) -> Vec<crate::magician_v2::api_mining::types::CapturedAuthEvent> {
    traces
        .iter()
        .map(
            |trace| crate::magician_v2::api_mining::types::CapturedAuthEvent {
                request_id: trace.request_id.clone(),
                url: trace.url.clone(),
                timestamp: trace.timestamp,
                auth_headers: crate::magician_v2::api_mining::auth_capture::extract_auth_headers(
                    &trace.request_headers,
                ),
                cookie_header: trace
                    .request_headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
                    .map(|(_, value)| value.clone()),
            },
        )
        .collect()
}

fn http_base_from_cdp_url(cdp_url: &str) -> Option<Url> {
    let parsed = Url::parse(cdp_url).ok()?;
    let http_scheme = match parsed.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return None,
    };
    let host = parsed.host_str()?;
    let port = parsed.port_or_known_default()?;
    let base = format!("{http_scheme}://{host}:{port}/");
    Url::parse(&base).ok()
}

fn persist_page_signals(
    exec_ctx: &PrimitiveExecCtx,
    principal: &str,
    workspace: &str,
    task_id: &str,
    session_id: &str,
    signals: &[AmbientPageSignal],
) -> Result<std::path::PathBuf, String> {
    if signals.is_empty() {
        return Err("no page signals to persist".to_string());
    }
    ensure_safe_record_id(task_id, "task_id").map_err(|err| err.to_string())?;

    let layout = ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(
        &exec_ctx.storage_base_path,
    ));
    let task_dir = layout
        .api_mining_root(principal, workspace)
        .join("page_signals")
        .join(task_id);
    fs::create_dir_all(&task_dir)
        .map_err(|err| format!("failed to create page signal directory: {err}"))?;

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f").to_string();
    let path = task_dir.join(format!("page_signals_{timestamp}.jsonl"));
    let file = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .map_err(|err| format!("failed to create page signal file: {err}"))?
        }
        #[cfg(not(unix))]
        {
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)
                .map_err(|err| format!("failed to create page signal file: {err}"))?
        }
    };
    let mut writer = BufWriter::new(file);
    for signal in signals {
        let row = serde_json::json!({
            "session_id": session_id,
            "signal": signal,
        });
        serde_json::to_writer(&mut writer, &row)
            .map_err(|err| format!("failed to serialize page signal: {err}"))?;
        writeln!(writer).map_err(|err| format!("failed to write page signal: {err}"))?;
    }
    writer
        .flush()
        .map_err(|err| format!("failed to flush page signals: {err}"))?;
    Ok(path)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    // P4: a delivered value is scrubbed out of every trace field, whatever
    // key or encoding it rode under — `redact_trace` alone knows only names.
    #[test]
    fn delivered_values_are_scrubbed_from_traces_by_value() {
        let har = serde_json::json!({"log": {"entries": [{
            "request": {"method": "POST", "url": "https://app.example.test/login?code=042917", "headers": [
                {"name": "X-Custom", "value": "p4-trace-canary"}
            ], "postData": {"text": "user=ada&password=p4-trace-canary&otp=042917"}},
            "response": {"status": 200, "headers": [{"name": "X-Echo", "value": "042917"}], "content": {"text": "{\"welcome\":\"p4-trace-canary\"}"}}
        }]}});
        let mut traces = super::super::har::har_to_traces(&har, None);
        let delivered = Arc::new(std::sync::Mutex::new(
            crate::magician_v2::secrets::KnownSecretValues::new(),
        ));
        {
            let mut set = delivered.lock().unwrap();
            set.insert("password@0".into(), "p4-trace-canary".into());
            set.insert("otp@1".into(), "042917".into());
        }
        scrub_traces_with_delivered_values(&mut traces, Some(&delivered));
        let rendered = serde_json::to_string(&traces).unwrap();
        assert!(!rendered.contains("p4-trace-canary"), "{rendered}");
        assert!(!rendered.contains("042917"), "{rendered}");
        assert!(traces[0].url.contains("code=[REDACTED]"));
        assert_eq!(
            traces[0]
                .request_headers
                .get("x-custom")
                .or(traces[0].request_headers.get("X-Custom"))
                .map(String::as_str),
            Some("[REDACTED]")
        );
        // Nothing delivered, nothing changes.
        let mut untouched = super::super::har::har_to_traces(&har, None);
        scrub_traces_with_delivered_values(&mut untouched, None);
        assert!(serde_json::to_string(&untouched)
            .unwrap()
            .contains("p4-trace-canary"));
    }

    #[test]
    fn har_auth_reaches_the_encrypted_store_before_trace_redaction() {
        let har = serde_json::json!({"log": {"entries": [{
            "request": {"method": "GET", "url": "https://app.example.test/me?api_key=query-secret", "headers": [
                {"name": "Authorization", "value": "Bearer header-secret"},
                {"name": "Cookie", "value": "session=cookie-secret"}
            ]}, "response": {"status": 200, "content": {"text": "{}"}}
        }]}});
        let traces = super::super::har::har_to_traces(&har, None);
        let temp = tempfile::tempdir().unwrap();
        let store = crate::magician_v2::secrets::SecretStore::new_empty(
            Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
            temp.path().to_path_buf(),
        );
        let origins =
            persist_captured_auth_events(&har_auth_events(&traces), &store, None).unwrap();
        assert!(origins.contains("https://app.example.test"));
        let (session, _lease) = store
            .get_session("https://app.example.test", "https://app.example.test/me")
            .unwrap();
        assert_eq!(
            session.auth_headers["authorization"],
            "Bearer header-secret"
        );
        let persisted = serde_json::to_string(&redact_trace(&traces[0])).unwrap();
        assert!(!persisted.contains("header-secret"));
        assert!(!persisted.contains("cookie-secret"));
        assert!(!persisted.contains("query-secret"));
    }

    #[test]
    fn http_base_from_cdp_url_handles_default_proxy() {
        let base =
            http_base_from_cdp_url("ws://127.0.0.1:3003/devtools/browser/magician-thread123")
                .expect("default URL parses");
        assert_eq!(base.as_str(), "http://127.0.0.1:3003/");
    }

    #[test]
    fn http_base_from_cdp_url_handles_secure_proxy() {
        let base =
            http_base_from_cdp_url("wss://proxy.example.com:8443/devtools/browser/magician-x")
                .expect("wss URL parses");
        assert_eq!(base.as_str(), "https://proxy.example.com:8443/");
    }

    #[test]
    fn http_base_from_cdp_url_rejects_non_websocket_scheme() {
        assert!(http_base_from_cdp_url("http://127.0.0.1:3003/").is_none());
        assert!(http_base_from_cdp_url("not a url").is_none());
    }

    #[test]
    fn persist_page_signals_writes_api_mining_jsonl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut exec_ctx = PrimitiveExecCtx::default_for_runtime();
        exec_ctx.storage_base_path = temp.path().join("magician_data_v3");

        let signal = AmbientPageSignal {
            event_id: "event-1".to_string(),
            thread_id: "magician-thread-1".to_string(),
            tab_id: 42,
            session_id: Some("session-1".to_string()),
            event_kind: "automation_cdp_command".to_string(),
            timestamp: 1_718_000_000_000,
            url: Some("https://example.com/search?q=ok".to_string()),
            origin: Some("https://example.com".to_string()),
            capture_source: Some("automation_cdp_mirror".to_string()),
            ..Default::default()
        };

        let path = persist_page_signals(
            &exec_ctx,
            "principal-a",
            "workspace-b",
            "task-123",
            "magician-thread-1",
            std::slice::from_ref(&signal),
        )
        .expect("page signals persist");

        assert_eq!(
            path.parent().unwrap(),
            exec_ctx
                .storage_base_path
                .join("scopes/principal-a/workspace-b/api_mining/page_signals/task-123")
        );
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("page_signals_"));
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("jsonl"));

        let body = fs::read_to_string(&path).expect("page signal jsonl readable");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 1);
        let row: serde_json::Value = serde_json::from_str(lines[0]).expect("valid jsonl row");
        assert_eq!(row["session_id"], "magician-thread-1");
        assert_eq!(row["signal"]["eventId"], "event-1");
        assert_eq!(row["signal"]["eventKind"], "automation_cdp_command");
        assert_eq!(row["signal"]["captureSource"], "automation_cdp_mirror");
        assert_eq!(row["signal"]["origin"], "https://example.com");
    }

    #[test]
    fn session_cookies_ride_the_requests_their_host_and_path_match() {
        let payload = serde_json::json!({"success": true, "data": {"cookies": [
            {"name": "sid", "value": "abc123", "domain": "127.0.0.1", "path": "/"},
            {"name": "scoped", "value": "s1", "domain": "127.0.0.1", "path": "/api"},
            {"name": "elsewhere", "value": "nope", "domain": "other.test", "path": "/"},
        ]}});
        let jar = jar_from_payload(&payload);
        assert_eq!(jar.len(), 3);
        assert_eq!(
            cookie_header_for_url("http://127.0.0.1:57385/api/me/orders", &jar),
            "sid=abc123; scoped=s1"
        );
        // `/api` must not match `/apifoo`, and a foreign host gets nothing.
        assert_eq!(
            cookie_header_for_url("http://127.0.0.1:57385/apifoo", &jar),
            "sid=abc123"
        );
        assert_eq!(
            cookie_header_for_url("http://other.test/x", &jar),
            "elsewhere=nope"
        );
        assert_eq!(cookie_header_for_url("not a url", &jar), "");
        assert!(jar_from_payload(&serde_json::json!({"data": {}})).is_empty());
    }
}
