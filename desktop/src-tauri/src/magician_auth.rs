//! Shared bearer transport for native calls back into the Magician API.

mod storage;

use futures_util::StreamExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use std::{collections::HashMap, time::Duration};
use storage::{EngineSession, Origin, OsSessionStore};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::{header::AUTHORIZATION, HeaderValue, Request},
};

static ENGINE_SESSION: OnceLock<Mutex<EngineSession>> = OnceLock::new();
static ENGINE_IS_REMOTE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) static ENGINE_LOCATION_TEST_LOCK: Mutex<()> = Mutex::new(());

fn engine_session() -> std::sync::MutexGuard<'static, EngineSession> {
    ENGINE_SESSION
        .get_or_init(|| Mutex::new(EngineSession::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn apply_engine_location(is_remote: bool, base_url: &str) {
    ENGINE_IS_REMOTE.store(is_remote, Ordering::Relaxed);
    set_trusted_engine_base(base_url);
}

pub(crate) fn is_remote_engine_process() -> bool {
    ENGINE_IS_REMOTE.load(Ordering::Relaxed)
}

pub(crate) fn set_trusted_engine_base(url: &str) {
    engine_session().select(url, std::env::var("MAGICIAN_BEARER_TOKEN").ok());
}

/// Snapshot the selected engine origin and its current user session for native
/// background services such as Magician Edge. This bypasses the WebView caller
/// check because no WebView initiated the read; origin selection and keyring
/// lookup still use the same `EngineSession` authority as UI requests.
pub(crate) fn process_connection_auth() -> Result<(String, Option<String>, u64), String> {
    let mut session = engine_session();
    let origin = session
        .origin
        .as_ref()
        .ok_or("No valid Magician server is selected")?
        .as_str()
        .to_owned();
    let token = session.token(&OsSessionStore)?;
    Ok((origin, token, session.revision))
}

#[derive(serde::Serialize)]
pub(crate) struct ConnectionAuth {
    origin: String,
    token: Option<String>,
    revision: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MagicianHttpRequest {
    url: String,
    method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MagicianHttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(serde::Deserialize)]
struct LoginGrant {
    token: String,
}

#[derive(serde::Deserialize)]
struct LoginError {
    message: Option<String>,
}

// Bundled native views and the selected backend may hydrate the native session.
// The existing debug overlay origin is allowed only in a debug build.
fn trusted_caller(url: &reqwest::Url, origin: &Origin) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.host_str() == Some("localhost")
        && matches!(url.scheme(), "tauri" | "magician-desktop")
        && url.port().is_none()
    {
        return true;
    }
    if matches!(
        url.as_str(),
        "http://tauri.localhost/" | "https://tauri.localhost/"
    ) {
        return true;
    }
    if Origin::parse(url.as_str()).as_ref() == Some(origin) {
        return true;
    }
    #[cfg(debug_assertions)]
    if matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
        && url.scheme() == "http"
        && url.port() == Some(5173)
    {
        return true;
    }
    false
}

fn caller_origin(window: &tauri::WebviewWindow, session: &EngineSession) -> Result<Origin, String> {
    let origin = session
        .origin
        .clone()
        .ok_or("No valid Magician server is selected")?;
    let url = window
        .url()
        .map_err(|_| "The desktop view origin is unavailable")?;
    if !trusted_caller(&url, &origin) {
        return Err("This view cannot access the selected server's desktop session".into());
    }
    Ok(origin)
}

#[tauri::command]
pub(crate) fn get_magician_connection_auth(
    window: tauri::WebviewWindow,
) -> Result<ConnectionAuth, String> {
    let mut session = engine_session();
    let origin = caller_origin(&window, &session)?;
    Ok(ConnectionAuth {
        origin: origin.as_str().to_owned(),
        token: session.token(&OsSessionStore)?,
        revision: session.revision,
    })
}

#[tauri::command]
pub(crate) fn get_magician_bearer_token(
    window: tauri::WebviewWindow,
) -> Result<Option<String>, String> {
    Ok(get_magician_connection_auth(window)?.token)
}

/// Bind login/logout to its issuing server and snapshot. An older view cannot
/// overwrite a later login from another window, even on the same origin.
#[tauri::command]
pub(crate) fn set_magician_bearer_token(
    window: tauri::WebviewWindow,
    token: Option<String>,
    expected_origin: String,
    expected_revision: u64,
) -> Result<u64, String> {
    let mut session = engine_session();
    caller_origin(&window, &session)?;
    let expected = Origin::parse(&expected_origin).ok_or("Invalid desktop session origin")?;
    let revision = session.install(&expected, expected_revision, token, &OsSessionStore)?;
    tracing::info!(
        origin = expected.as_str(),
        window = window.label(),
        revision,
        "Desktop session updated"
    );
    Ok(revision)
}

async fn bounded_response_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err("The Magician API response exceeds the Desktop size limit".into());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Desktop could not read the Magician API response")?;
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or("The Magician API response exceeds the Desktop size limit")?;
        if next_len > max_bytes {
            return Err("The Magician API response exceeds the Desktop size limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[tauri::command]
pub(crate) async fn sign_in_magician(
    window: tauri::WebviewWindow,
    username: String,
    password: String,
) -> Result<(), String> {
    const MAX_LOGIN_RESPONSE_BYTES: usize = 64 * 1024;
    let username = username.trim();
    if username.is_empty() || password.is_empty() {
        return Err("Enter a username and password".into());
    }
    let (origin, expected_revision) = {
        let session = engine_session();
        let origin = caller_origin(&window, &session)?;
        (origin.as_str().to_owned(), session.revision)
    };
    if is_remote_engine_process() {
        crate::edge_client::hydrate_outer_access(&origin).await?;
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "Desktop could not create the Magician sign-in client")?;
    let login_url = reqwest::Url::parse(&format!(
        "{}/api/magician/v2/auth/login",
        origin.trim_end_matches('/')
    ))
    .map_err(|_| "The selected Magician server address is invalid")?;
    let response = crate::edge_client::authorize_outer_http(
        client
            .post(login_url.clone())
            .json(&serde_json::json!({ "username": username, "password": password })),
        &login_url,
    )
    .send()
    .await
    .map_err(|error| format!("Desktop could not reach Magician sign-in: {error}"))?;
    let status = response.status();
    let body = bounded_response_body(response, MAX_LOGIN_RESPONSE_BYTES).await?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<LoginError>(&body)
            .ok()
            .and_then(|body| body.message)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "Check your account details.".to_owned());
        return Err(format!("Sign-in failed ({}). {detail}", status.as_u16()));
    }
    let grant: LoginGrant =
        serde_json::from_slice(&body).map_err(|_| "The server did not return a valid session")?;
    if grant.token.trim().is_empty() {
        return Err("The server did not return a session".into());
    }

    let session_url = reqwest::Url::parse(&format!(
        "{}/api/magician/v2/auth/session",
        origin.trim_end_matches('/')
    ))
    .map_err(|_| "The selected Magician server address is invalid")?;
    let verified = crate::edge_client::authorize_outer_http(
        client.get(session_url.clone()).bearer_auth(&grant.token),
        &session_url,
    )
    .send()
    .await
    .map_err(|error| format!("Desktop could not verify the Magician session: {error}"))?;
    if !verified.status().is_success() {
        return Err("The new session could not be verified".into());
    }

    let mut session = engine_session();
    caller_origin(&window, &session)?;
    let expected = Origin::parse(&origin).ok_or("Invalid desktop session origin")?;
    session.install(
        &expected,
        expected_revision,
        Some(grant.token),
        &OsSessionStore,
    )?;
    Ok(())
}

#[tauri::command]
pub(crate) async fn logout_magician_session(window: tauri::WebviewWindow) -> Result<(), String> {
    let (origin, token) = {
        let mut session = engine_session();
        let origin = caller_origin(&window, &session)?;
        let token = session.token(&OsSessionStore)?;
        let revision = session.revision;
        session.install(&origin, revision, None, &OsSessionStore)?;
        (origin.as_str().to_owned(), token)
    };
    let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
        return Ok(());
    };

    // Local authority is already cleared. Server revocation is best-effort so
    // an offline backend cannot leave the desktop signed in.
    if is_remote_engine_process() {
        if let Err(error) = crate::edge_client::hydrate_outer_access(&origin).await {
            tracing::warn!("Could not load outer access for server logout: {error}");
            return Ok(());
        }
    }
    let Ok(url) = reqwest::Url::parse(&format!(
        "{}/api/magician/v2/auth/logout",
        origin.trim_end_matches('/')
    )) else {
        return Ok(());
    };
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return Ok(());
    };
    let request =
        crate::edge_client::authorize_outer_http(client.post(url.clone()).bearer_auth(token), &url);
    if let Err(error) = request.send().await {
        tracing::warn!("Magician server logout could not be completed: {error}");
    }
    Ok(())
}

/// Send a bounded request to the exact selected Magician API origin. Keeping
/// this transport native lets Desktop Edge apply an outer Cloudflare Access
/// service credential without exposing it to WebView JavaScript.
#[tauri::command]
pub(crate) async fn magician_http_request(
    window: tauri::WebviewWindow,
    request: MagicianHttpRequest,
) -> Result<MagicianHttpResponse, String> {
    const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
    const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

    let url =
        reqwest::Url::parse(request.url.trim()).map_err(|_| "The Magician API URL is invalid")?;
    let selected_origin = {
        let session = engine_session();
        let origin = caller_origin(&window, &session)?;
        if !trusted_api_url(&url, Some(&origin)) {
            return Err("Refusing a desktop request outside the selected Magician API".into());
        }
        origin.as_str().to_owned()
    };

    let method = reqwest::Method::from_bytes(request.method.trim().as_bytes())
        .map_err(|_| "The Magician API method is invalid")?;
    if !matches!(
        method,
        reqwest::Method::GET
            | reqwest::Method::POST
            | reqwest::Method::PUT
            | reqwest::Method::PATCH
            | reqwest::Method::DELETE
    ) {
        return Err("The Magician API method is not allowed from Desktop".into());
    }
    if request
        .body
        .as_ref()
        .is_some_and(|body| body.len() > MAX_REQUEST_BYTES)
    {
        return Err("The Desktop API request exceeds its size limit".into());
    }

    if is_remote_engine_process() {
        crate::edge_client::hydrate_outer_access(&selected_origin).await?;
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "Desktop could not create the Magician API client")?;
    let mut outgoing = client.request(method, url);
    for (name, value) in request.headers {
        let name = name.trim().to_ascii_lowercase();
        if !matches!(name.as_str(), "accept" | "content-type") {
            return Err(format!("Desktop API header is not allowed: {name}"));
        }
        outgoing = outgoing.header(name, value);
    }
    if let Some(body) = request.body {
        outgoing = outgoing.body(body);
    }
    let response = authorize(outgoing)
        .send()
        .await
        .map_err(|error| format!("Desktop could not reach the Magician API: {error}"))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| matches!(name.as_str(), "content-type" | "etag" | "location"))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.to_string(), value.to_string()))
        })
        .collect();
    let body = bounded_response_body(response, MAX_RESPONSE_BYTES).await?;
    Ok(MagicianHttpResponse {
        status,
        headers,
        body,
    })
}

fn token_for_url(url: &reqwest::Url) -> Result<Option<String>, String> {
    let mut session = engine_session();
    if !trusted_api_url(url, session.origin.as_ref()) {
        return Ok(None);
    }
    session.token(&OsSessionStore)
}

fn trusted_api_url(url: &reqwest::Url, origin: Option<&Origin>) -> bool {
    matches!(url.scheme(), "http" | "https" | "ws" | "wss")
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().starts_with("/api/magician/")
        && origin.is_some()
        && Origin::parse(url.as_str()).as_ref() == origin
}

pub(crate) fn authorize(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let url = request
        .try_clone()
        .and_then(|request| request.build().ok())
        .map(|request| request.url().clone());
    let token = url.as_ref().and_then(|url| match token_for_url(url) {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!("Native Magician authentication unavailable: {error}");
            None
        },
    });
    let request = match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    match url {
        Some(url) => crate::edge_client::authorize_outer_http(request, &url),
        None => request,
    }
}

pub(crate) fn websocket_request(url: &str) -> Result<Request<()>, String> {
    let token = {
        let mut session = engine_session();
        session.token(&OsSessionStore)?
    };
    websocket_request_with_bearer(url, token.as_deref())
}

/// Build a selected-origin WebSocket request with an explicit credential.
/// Paired Edge tokens are distinct from the interactive desktop session but
/// receive the same exact-origin and `/api/magician/` path protection.
pub(crate) fn websocket_request_with_bearer(
    url: &str,
    bearer: Option<&str>,
) -> Result<Request<()>, String> {
    let mut request = url
        .into_client_request()
        .map_err(|_| "Invalid Magician WebSocket URL".to_owned())?;
    let parsed = reqwest::Url::parse(url).map_err(|_| "Invalid Magician WebSocket URL")?;
    let session = engine_session();
    if !matches!(parsed.scheme(), "ws" | "wss")
        || !trusted_api_url(&parsed, session.origin.as_ref())
    {
        return Err("Refusing to attach the Magician bearer to an untrusted WebSocket URL".into());
    }
    if let Some(token) = bearer {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "Invalid Magician bearer token".to_owned())?;
        request.headers_mut().insert(AUTHORIZATION, value);
    }
    crate::edge_client::authorize_outer_websocket(&mut request, &parsed)?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_transport_uses_only_the_selected_origin() {
        let origin = Origin::parse("http://127.0.0.1:13002").unwrap();
        for url in [
            "http://127.0.0.1:13002/api/magician/v2/health",
            "ws://127.0.0.1:13002/api/magician/v2/realtime/ws",
        ] {
            assert!(trusted_api_url(
                &reqwest::Url::parse(url).unwrap(),
                Some(&origin)
            ));
        }
        for url in [
            "http://127.0.0.1:3002/api/magician/v2/health",
            "http://localhost:13002/api/magician/v2/health",
            "http://127.0.0.1:13002/not-api",
            "http://user:pass@127.0.0.1:13002/api/magician/v2/health",
        ] {
            assert!(!trusted_api_url(
                &reqwest::Url::parse(url).unwrap(),
                Some(&origin)
            ));
        }
    }

    #[test]
    fn configured_remote_engine_origin_is_trusted() {
        let _serialized = ENGINE_LOCATION_TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        set_trusted_engine_base("https://engine.example:8443");
        websocket_request("wss://engine.example:8443/api/magician/v2/realtime/ws").unwrap();
        assert!(websocket_request("wss://evil.example/api/magician/v2/realtime/ws").is_err());
        assert!(websocket_request("ws://engine.example:8443/api/magician/v2/realtime/ws").is_err());
        assert!(websocket_request("ws://127.0.0.1:3002/api/magician/v2/realtime/ws").is_err());
        set_trusted_engine_base("http://127.0.0.1:3002");
    }

    #[test]
    fn caller_cannot_inherit_credentials_from_another_backend() {
        let origin = Origin::parse("http://127.0.0.1:13002").unwrap();
        assert!(trusted_caller(
            &reqwest::Url::parse("magician-desktop://localhost/").unwrap(),
            &origin
        ));
        assert!(trusted_caller(
            &reqwest::Url::parse("http://127.0.0.1:13002/presto").unwrap(),
            &origin
        ));
        assert!(!trusted_caller(
            &reqwest::Url::parse("http://127.0.0.1:3002/presto").unwrap(),
            &origin
        ));
        assert!(!trusted_caller(
            &reqwest::Url::parse("https://unapproved.example/").unwrap(),
            &origin
        ));
    }
}
