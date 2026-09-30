//! Passive auth-capture helpers shared by API mining.
//!
//! Auth material arrives through Magicutor's one-shot transient auth drain,
//! separately from redacted network traces, and is written directly to the
//! encrypted captured-secret store. This module owns the filtering and merge
//! rules for that narrow channel and explicit browser auth snapshots.

use std::collections::HashMap;
use std::collections::HashSet;

use super::router::extract_origin;
use super::trace_storage::is_sensitive_header_name;
use super::types::CapturedAuthEvent;
use crate::magician_v2::secrets::{CookieWithMetadata, SameSite, SecretStore};

// ─────────────────────── Constants ──────────────────────────

/// Header names that carry authentication material we want to capture.
/// Every entry here MUST also appear in `trace_storage::SENSITIVE_HEADERS`
/// so the values are redacted from persisted traces.
pub const AUTH_CAPTURE_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "x-auth-token",
    "x-access-token",
    "x-csrf-token",
    "x-xsrf-token",
    "x-framework-xsrf-token",
    "x-google-btd",
    "x-gmail-btai",
    "x-xsrf-asfe-token",
];

/// Auth names learned from capabilities for one browser snapshot. Exact names
/// supplement conservative name detection so opaque application cookies and
/// storage keys are captured without treating analytics state as login state.
#[derive(Debug, Clone, Default)]
pub struct BrowserAuthCaptureRequirements {
    pub cookies: HashSet<String>,
    pub local_storage_keys: HashSet<String>,
    pub session_storage_keys: HashSet<String>,
}

impl BrowserAuthCaptureRequirements {
    fn requires_cookie(&self, name: &str) -> bool {
        contains_name_case_insensitive(&self.cookies, name)
    }

    fn requires_local_storage_key(&self, name: &str) -> bool {
        contains_name_case_insensitive(&self.local_storage_keys, name)
    }

    fn requires_session_storage_key(&self, name: &str) -> bool {
        contains_name_case_insensitive(&self.session_storage_keys, name)
    }
}

/// Extract auth-relevant headers from a raw header map.
pub fn extract_auth_headers(headers: &HashMap<String, String>) -> HashMap<String, String> {
    let mut result = HashMap::new();
    for (key, value) in headers {
        let lower = key.to_lowercase();
        if lower == "cookie" || lower == "set-cookie" || !is_usable_captured_value(value) {
            continue;
        }
        if AUTH_CAPTURE_HEADERS.contains(&lower.as_str()) || is_sensitive_header_name(key) {
            result.insert(key.clone(), value.clone());
        }
    }
    result
}

/// Persist one drained batch of transient CDP auth events into the encrypted
/// captured-secret store. `target_origin` limits an explicit refresh session to
/// the requested origin; `None` is used by normal browser-task capture.
pub fn persist_captured_auth_events(
    events: &[CapturedAuthEvent],
    secret_store: &SecretStore,
    target_origin: Option<&str>,
) -> Result<HashSet<String>, String> {
    #[derive(Default)]
    struct OriginAuthBatch {
        headers: HashMap<String, String>,
        query_params: HashMap<String, String>,
        cookies: HashMap<String, CookieWithMetadata>,
    }

    let mut batches: HashMap<String, OriginAuthBatch> = HashMap::new();
    for event in events {
        let origin = extract_origin(&event.url);
        if origin.is_empty() || target_origin.is_some_and(|target| target != origin) {
            continue;
        }

        let batch = batches.entry(origin).or_default();
        for (name, value) in &event.auth_headers {
            if is_usable_captured_value(value) {
                batch.headers.insert(name.clone(), value.clone());
            }
        }
        batch
            .query_params
            .extend(extract_auth_query_params(&event.url));
        if let Some(cookie_header) = event.cookie_header.as_deref() {
            for cookie in cookies_from_request_header(&event.url, cookie_header) {
                batch.cookies.insert(cookie.name.clone(), cookie);
            }
        }
    }

    let mut captured_origins = HashSet::new();
    for (origin, batch) in batches {
        if batch.headers.is_empty() && batch.query_params.is_empty() && batch.cookies.is_empty() {
            continue;
        }
        secret_store
            .store_captured_deferred_with_query_params(
                &origin,
                batch.headers,
                batch.query_params,
                batch.cookies.into_values().collect(),
                HashMap::new(),
                HashMap::new(),
            )
            .map_err(|error| format!("storing captured auth for {origin}: {error}"))?;
        captured_origins.insert(origin);
    }

    if !captured_origins.is_empty() {
        secret_store
            .flush_captured()
            .map_err(|error| format!("flushing captured auth: {error}"))?;
    }
    Ok(captured_origins)
}

/// Merge a browser-session snapshot into the encrypted captured-auth entry for
/// `target_origin`. Cookies are read through CDP (including HttpOnly cookies),
/// while storage values come from the current page only.
pub fn persist_browser_auth_snapshot(
    cookie_payload: Option<&serde_json::Value>,
    mut local_storage: HashMap<String, String>,
    mut session_storage: HashMap<String, String>,
    requirements: &BrowserAuthCaptureRequirements,
    secret_store: &SecretStore,
    target_origin: &str,
) -> Result<bool, String> {
    let cookies = cookie_payload
        .map(cookies_from_browser_payload)
        .unwrap_or_default()
        .into_iter()
        .filter(|cookie| {
            cookie_matches_origin(cookie, target_origin)
                && (requirements.requires_cookie(&cookie.name) || is_auth_cookie_name(&cookie.name))
        })
        .collect::<Vec<_>>();
    local_storage.retain(|key, value| {
        (requirements.requires_local_storage_key(key) || is_auth_storage_key(key))
            && is_usable_captured_value(value)
    });
    session_storage.retain(|key, value| {
        (requirements.requires_session_storage_key(key) || is_auth_storage_key(key))
            && is_usable_captured_value(value)
    });

    if cookies.is_empty() && local_storage.is_empty() && session_storage.is_empty() {
        return Ok(false);
    }

    secret_store
        .store_captured_deferred_with_query_params(
            target_origin,
            HashMap::new(),
            HashMap::new(),
            cookies,
            local_storage,
            session_storage,
        )
        .map_err(|error| format!("storing browser auth snapshot for {target_origin}: {error}"))?;
    secret_store
        .flush_captured()
        .map_err(|error| format!("flushing browser auth snapshot for {target_origin}: {error}"))?;
    Ok(true)
}

fn is_auth_storage_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    let compact = normalized
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|segment| {
            matches!(
                segment,
                "auth" | "token" | "jwt" | "session" | "credential" | "csrf" | "xsrf" | "identity"
            )
        })
        || compact.ends_with("token")
        || compact.ends_with("jwt")
        || compact.ends_with("session")
        || compact.ends_with("credential")
        || compact.ends_with("csrf")
        || compact.ends_with("xsrf")
        || matches!(compact.as_str(), "authorization" | "authentication")
}

fn is_auth_cookie_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    let compact = normalized
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    is_auth_storage_key(name)
        || matches!(
            compact.as_str(),
            "sid" | "connectsid" | "jsessionid" | "phpsessid" | "sso"
        )
        || normalized.ends_with("_sid")
        || normalized.ends_with("-sid")
        || normalized.ends_with(".sid")
}

fn contains_name_case_insensitive(names: &HashSet<String>, candidate: &str) -> bool {
    names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(candidate))
}

fn cookies_from_browser_payload(payload: &serde_json::Value) -> Vec<CookieWithMetadata> {
    let candidates = payload
        .as_array()
        .or_else(|| payload.get("cookies").and_then(serde_json::Value::as_array))
        .or_else(|| {
            payload
                .get("data")
                .and_then(|value| value.get("cookies"))
                .and_then(serde_json::Value::as_array)
        })
        .or_else(|| payload.get("data").and_then(serde_json::Value::as_array))
        .or_else(|| {
            payload
                .get("result")
                .and_then(|value| value.get("cookies"))
                .and_then(serde_json::Value::as_array)
        });

    candidates
        .into_iter()
        .flatten()
        .filter_map(cookie_from_browser_value)
        .collect()
}

fn cookie_from_browser_value(value: &serde_json::Value) -> Option<CookieWithMetadata> {
    let name = value.get("name")?.as_str()?.trim();
    let cookie_value = value.get("value")?.as_str()?;
    if name.is_empty() || !is_usable_captured_value(cookie_value) {
        return None;
    }

    let same_site = match value
        .get("sameSite")
        .or_else(|| value.get("same_site"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("lax")
        .to_ascii_lowercase()
        .as_str()
    {
        "strict" => SameSite::Strict,
        "none" => SameSite::None,
        _ => SameSite::Lax,
    };
    let expires = value
        .get("expires")
        .or_else(|| value.get("expirationDate"))
        .or_else(|| value.get("expiration_date"))
        .and_then(serde_json::Value::as_f64)
        .filter(|value| *value > 0.0)
        .map(|value| value as i64);

    Some(CookieWithMetadata {
        name: name.to_string(),
        value: cookie_value.to_string(),
        domain: value
            .get("domain")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        path: value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("/")
            .to_string(),
        secure: value
            .get("secure")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        http_only: value
            .get("httpOnly")
            .or_else(|| value.get("http_only"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        same_site,
        expires,
    })
}

fn cookie_matches_origin(cookie: &CookieWithMetadata, origin: &str) -> bool {
    let Ok(origin) = url::Url::parse(origin) else {
        return false;
    };
    let Some(host) = origin.host_str() else {
        return false;
    };
    let domain = cookie.domain.trim_start_matches('.');
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
}

fn is_usable_captured_value(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "[REDACTED]"
}

fn extract_auth_query_params(url: &str) -> HashMap<String, String> {
    let Ok(parsed) = url::Url::parse(url) else {
        return HashMap::new();
    };
    parsed
        .query_pairs()
        .filter_map(|(key, value)| {
            (is_auth_query_key(&key) && is_usable_captured_value(&value))
                .then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

/// Whether a query parameter carries auth material. This is the CAPTURE
/// boundary's definition, and the recipe compiler resolves against the same
/// one on purpose: a compiler that calls a parameter session-auth while the
/// capture never stores it compiles a recipe that can only fail `class: auth`
/// before it sends anything (measured against a live search UI, whose
/// `…-application-id` the compiler demanded and the capture ignored).
pub(crate) fn is_auth_query_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    let compact = normalized
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|segment| {
            matches!(
                segment,
                "key" | "auth" | "token" | "secret" | "csrf" | "xsrf"
            )
        })
        || matches!(
            compact.as_str(),
            "apikey"
                | "accesskey"
                | "secretkey"
                | "subscriptionkey"
                | "authorization"
                | "authentication"
        )
        || compact.ends_with("token")
        || compact.ends_with("secret")
        || compact.ends_with("csrf")
        || compact.ends_with("xsrf")
}

fn cookies_from_request_header(url: &str, cookie_header: &str) -> Vec<CookieWithMetadata> {
    let Ok(parsed) = url::Url::parse(url) else {
        return Vec::new();
    };
    let Some(domain) = parsed.host_str() else {
        return Vec::new();
    };

    cookie_header
        .split(';')
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || !is_usable_captured_value(value) {
                return None;
            }
            Some(CookieWithMetadata {
                name: name.to_string(),
                value: value.to_string(),
                domain: domain.to_string(),
                path: "/".to_string(),
                secure: parsed.scheme() == "https",
                http_only: false,
                same_site: SameSite::Lax,
                expires: None,
            })
        })
        .collect()
}

// ─────────────────────── Tests ──────────────────────────────

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::trace_storage::{is_sensitive_header_name, SENSITIVE_HEADERS};
    use super::*;
    use crate::magician_v2::secrets::InMemoryKeyProvider;

    #[test]
    fn auth_capture_headers_subset_of_sensitive_headers() {
        for header in AUTH_CAPTURE_HEADERS {
            assert!(
                SENSITIVE_HEADERS.contains(header) || is_sensitive_header_name(header),
                "AUTH_CAPTURE_HEADERS entry {:?} is not redacted by trace storage",
                header
            );
        }
    }

    #[test]
    fn extract_auth_headers_filters_correctly() {
        let headers = HashMap::from([
            ("Authorization".to_string(), "Bearer token".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-CSRF-Token".to_string(), "csrf".to_string()),
        ]);

        let auth = extract_auth_headers(&headers);
        assert_eq!(auth.len(), 2);
        assert_eq!(
            auth.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
        assert_eq!(auth.get("X-CSRF-Token").map(String::as_str), Some("csrf"));
        assert!(!auth.contains_key("Content-Type"));
    }

    #[test]
    fn extract_auth_headers_captures_vendor_api_key_headers() {
        let headers = HashMap::from([
            (
                "x-algolia-api-key".to_string(),
                "public-search-key".to_string(),
            ),
            ("x-algolia-application-id".to_string(), "APPID".to_string()),
        ]);

        let auth = extract_auth_headers(&headers);
        assert_eq!(
            auth.get("x-algolia-api-key").map(String::as_str),
            Some("public-search-key")
        );
        assert!(!auth.contains_key("x-algolia-application-id"));
    }

    #[test]
    fn cookies_from_request_header_preserves_values_containing_equals() {
        let cookies = cookies_from_request_header(
            "https://app.example.com/api/me",
            "session=abc==; theme=light",
        );
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "session");
        assert_eq!(cookies[0].value, "abc==");
        assert_eq!(cookies[0].domain, "app.example.com");
        assert!(cookies[0].secure);
    }

    #[test]
    fn extract_auth_query_params_ignores_normal_query_values() {
        let values = extract_auth_query_params(
            "https://api.example.com/search?q=rust&keyboard=compact&author=alice&monkey=capuchin&api_key=secret&csrf_token=csrf",
        );
        assert_eq!(values.len(), 2);
        assert_eq!(values.get("api_key").map(String::as_str), Some("secret"));
        assert!(!values.contains_key("q"));
        assert!(!values.contains_key("keyboard"));
        assert!(!values.contains_key("author"));
        assert!(!values.contains_key("monkey"));
    }

    #[test]
    fn transient_auth_event_round_trips_through_encrypted_store() {
        let temp = tempfile::tempdir().expect("temporary secret store");
        let store = SecretStore::new_empty(
            Box::new(InMemoryKeyProvider::new()),
            temp.path().to_path_buf(),
        );
        let origin = "https://app.example.com";
        let events = vec![CapturedAuthEvent {
            request_id: "request-1".to_string(),
            url: format!("{origin}/api/me"),
            auth_headers: HashMap::from([(
                "Authorization".to_string(),
                "Bearer fresh-token".to_string(),
            )]),
            cookie_header: Some("session=fresh-cookie".to_string()),
            timestamp: 42,
        }];

        let captured = persist_captured_auth_events(&events, &store, Some(origin))
            .expect("transient auth should persist");
        assert!(captured.contains(origin));
        let status = store.captured_status(origin);
        assert!(status.has_auth);
        assert!(!status.is_stale);
        assert!(status.has_headers);
        assert!(status.has_cookies);

        let (session, _) = store
            .get_session(origin, &format!("{origin}/api/me"))
            .expect("captured session should be available");
        assert_eq!(
            session
                .auth_headers
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer fresh-token")
        );
        assert_eq!(
            session.cookies.get("session").map(String::as_str),
            Some("fresh-cookie")
        );
    }

    #[test]
    fn browser_snapshot_keeps_target_cookies_and_auth_like_storage_only() {
        let temp = tempfile::tempdir().expect("temporary secret store");
        let store = SecretStore::new_empty(
            Box::new(InMemoryKeyProvider::new()),
            temp.path().to_path_buf(),
        );
        let origin = "https://app.example.com";
        let payload = serde_json::json!({
            "success": true,
            "data": {
                "cookies": [
                    {
                        "name": "session",
                        "value": "fresh-cookie",
                        "domain": ".example.com",
                        "path": "/",
                        "secure": true,
                        "httpOnly": true,
                        "sameSite": "Lax"
                    },
                    {
                        "name": "other",
                        "value": "other-cookie",
                        "domain": "other.test",
                        "path": "/"
                    }
                ]
            }
        });

        let captured = persist_browser_auth_snapshot(
            Some(&payload),
            HashMap::from([
                ("theme".to_string(), "dark".to_string()),
                ("accessToken".to_string(), "fresh-access-token".to_string()),
            ]),
            HashMap::from([("csrfToken".to_string(), "fresh-csrf".to_string())]),
            &BrowserAuthCaptureRequirements::default(),
            &store,
            origin,
        )
        .expect("browser snapshot should persist");
        assert!(captured);

        let (session, _) = store
            .get_session(origin, &format!("{origin}/api/me"))
            .expect("captured browser session should be available");
        assert_eq!(
            session.cookies.get("session").map(String::as_str),
            Some("fresh-cookie")
        );
        assert!(!session.cookies.contains_key("other"));
        assert_eq!(
            session.local_storage.get("accessToken").map(String::as_str),
            Some("fresh-access-token")
        );
        assert!(!session.local_storage.contains_key("theme"));
        assert_eq!(
            session.session_storage.get("csrfToken").map(String::as_str),
            Some("fresh-csrf")
        );
    }

    #[test]
    fn browser_snapshot_uses_declared_opaque_names_without_accepting_analytics_cookies() {
        let temp = tempfile::tempdir().expect("temporary secret store");
        let store = SecretStore::new_empty(
            Box::new(InMemoryKeyProvider::new()),
            temp.path().to_path_buf(),
        );
        let origin = "https://app.example.com";
        let payload = serde_json::json!({
            "cookies": [
                {"name": "_ga", "value": "analytics", "domain": ".example.com"},
                {"name": "opaque_app_state", "value": "credential", "domain": ".example.com"}
            ]
        });
        let requirements = BrowserAuthCaptureRequirements {
            cookies: HashSet::from(["opaque_app_state".to_string()]),
            local_storage_keys: HashSet::from(["opaque_local".to_string()]),
            session_storage_keys: HashSet::new(),
        };

        assert!(persist_browser_auth_snapshot(
            Some(&payload),
            HashMap::from([
                ("keyboard".to_string(), "compact".to_string()),
                ("opaque_local".to_string(), "local-credential".to_string()),
            ]),
            HashMap::new(),
            &requirements,
            &store,
            origin,
        )
        .expect("declared browser auth should persist"));

        let (session, _) = store
            .get_session(origin, &format!("{origin}/api/me"))
            .expect("captured browser session should be available");
        assert!(!session.cookies.contains_key("_ga"));
        assert_eq!(
            session.cookies.get("opaque_app_state").map(String::as_str),
            Some("credential")
        );
        assert!(!session.local_storage.contains_key("keyboard"));
        assert_eq!(
            session
                .local_storage
                .get("opaque_local")
                .map(String::as_str),
            Some("local-credential")
        );
    }
}
