//! The fixture behind the secure-HITL qualification lane (plan §8, P7
//! Task 7.1): a controllable fake login service, generated canary
//! credentials, and the sweep that proves a canary reached nothing but the
//! destination.
//!
//! The service is `wiremock`, in-process and deterministic: `/login` takes
//! an identifier and a password and records every receipt; `/otp` takes a
//! one-time code; `/api/private` answers `401` with `WWW-Authenticate: Basic`
//! until the bound password arrives. A mode makes it reject the first code
//! (the fresh-challenge retry) or answer uncertainly. No network beyond
//! loopback, no third-party account.
//!
//! Destination receipt is asserted from the service's own record, never from
//! a redacted transcript — redaction must not be able to pass as a working
//! login (§8).
#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use base64::Engine as _;
use serde_json::Value;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, Request, ResponseTemplate,
};

/// Generated once per test: alphanumeric plus a dash, so every encoding the
/// sweep checks is exact.
#[derive(Debug, Clone)]
pub struct Canaries {
    pub identifier: String,
    pub password: String,
    pub code: String,
}

impl Canaries {
    pub fn generate() -> Self {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        Self {
            identifier: format!("p7-user-{}@example.test", &tag[..8]),
            password: format!("p7-pw-{}-canary", &tag[8..20]),
            // A six-digit code that no fixture text contains by accident.
            code: format!(
                "{:06}",
                (u32::from_str_radix(&tag[20..25], 16).unwrap_or(482913) % 900_000) + 100_000
            ),
        }
    }

    /// Every secret the sweep must not find (the identifier is a login
    /// identifier: sensitive by the spec too).
    pub fn secrets(&self) -> Vec<&str> {
        vec![
            self.identifier.as_str(),
            self.password.as_str(),
            self.code.as_str(),
        ]
    }
}

/// The encodings a leaked secret could hide behind.
pub fn encodings(secret: &str) -> Vec<String> {
    let mut out = vec![secret.to_string()];
    out.push(base64::engine::general_purpose::STANDARD.encode(secret));
    out.push(base64::engine::general_purpose::STANDARD_NO_PAD.encode(secret));
    out.push(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret));
    out.push(urlencoding::encode(secret).into_owned());
    out.push(secret.replace('@', "\\u0040"));
    out.push(hex::encode(secret.as_bytes()));
    out.sort();
    out.dedup();
    out
}

/// Where a secret was found: the file (or stream) and the encoding.
#[derive(Debug)]
pub struct Sighting {
    pub location: String,
    pub encoding: String,
}

/// Read every file under `root` and report where any secret (in any
/// encoding) appears. Binary files are searched as bytes.
pub fn sweep_tree(root: &Path, secrets: &[&str]) -> Vec<Sighting> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            found.extend(sweep_bytes(&path.display().to_string(), &bytes, secrets));
        }
    }
    found
}

/// The same over one text or byte blob (an event stream, a transcript, an
/// API body).
pub fn sweep_bytes(location: &str, bytes: &[u8], secrets: &[&str]) -> Vec<Sighting> {
    let mut found = Vec::new();
    for secret in secrets {
        for encoded in encodings(secret) {
            if contains(bytes, encoded.as_bytes()) {
                found.push(Sighting {
                    location: location.to_string(),
                    encoding: encoded.clone(),
                });
            }
        }
    }
    found
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Assert no sighting, with every location listed.
pub fn assert_clean(what: &str, sightings: &[Sighting]) {
    assert!(
        sightings.is_empty(),
        "{what} carries a secret: {}",
        sightings
            .iter()
            .map(|s| format!("{} ({} bytes of encoding)", s.location, s.encoding.len()))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Files under `root` whose path contains `needle` — for asserting what the
/// permitted custody wrote (encrypted at rest) without reading it as text.
pub fn files_under(root: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.display().to_string().contains(needle) {
                out.push(path);
            }
        }
    }
    out
}

/// How the fixture service behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceMode {
    /// Every correct credential is accepted at once.
    Accept,
    /// The first correct code is rejected (`401 invalid_code`); the second
    /// is accepted — the fresh-challenge retry.
    RejectFirstCode,
}

/// The fake login service.
pub struct LoginService {
    server: MockServer,
    pub canaries: Canaries,
}

impl LoginService {
    pub async fn start(canaries: Canaries, mode: ServiceMode) -> Self {
        let server = MockServer::start().await;
        let password = canaries.password.clone();
        let identifier = canaries.identifier.clone();
        let code = canaries.code.clone();
        // POST /login {"username", "password"}: 200 on the fixture
        // credentials, 401 otherwise. The record is the receipt.
        Mock::given(method("POST"))
            .and(path("/login"))
            .respond_with(move |request: &Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                let ok = body.get("username").and_then(Value::as_str) == Some(identifier.as_str())
                    && body.get("password").and_then(Value::as_str) == Some(password.as_str());
                if ok {
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"status": "code_sent"}))
                } else {
                    ResponseTemplate::new(401)
                        .set_body_json(serde_json::json!({"error": "invalid_credentials"}))
                }
            })
            .mount(&server)
            .await;
        // POST /otp {"code"}: 200 on the fixture code; in RejectFirstCode
        // the first correct code is refused once.
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/otp"))
            .respond_with(move |request: &Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
                let ok = body.get("code").and_then(Value::as_str) == Some(code.as_str());
                if !ok {
                    return ResponseTemplate::new(401)
                        .set_body_json(serde_json::json!({"error": "invalid_code"}));
                }
                let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if mode == ServiceMode::RejectFirstCode && n == 0 {
                    ResponseTemplate::new(401)
                        .set_body_json(serde_json::json!({"error": "invalid_code", "retry": true}))
                } else {
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"status": "logged_in"}))
                }
            })
            .mount(&server)
            .await;
        // GET /api/private: a Basic challenge until the bound password. The
        // fixture takes the raw password after the scheme: the header sink
        // substitutes a value, it does not build the base64 identifier:password
        // pair (recorded as a limit in the support matrix).
        let raw_password = canaries.password.clone();
        Mock::given(method("GET"))
            .and(path("/api/private"))
            .respond_with(move |request: &Request| {
                let expected = format!("Basic {raw_password}");
                let authorized = request.headers.iter().any(|(name, values)| {
                    name.as_str().eq_ignore_ascii_case("authorization")
                        && values.iter().any(|v| v.as_str() == expected)
                });
                if authorized {
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"secret_page": "private-data"}))
                } else {
                    ResponseTemplate::new(401)
                        .insert_header("WWW-Authenticate", "Basic realm=\"fixture\"")
                }
            })
            .mount(&server)
            .await;
        Self { server, canaries }
    }

    pub fn origin(&self) -> String {
        self.server.uri()
    }

    /// The values of `header` on every request that carried it, in order.
    /// The header sink's own receipt: proof that the credential reached the
    /// destination, not merely that a request arrived.
    pub async fn header_values(&self, header: &str) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .flat_map(|request| {
                request
                    .headers
                    .iter()
                    .filter(|(name, _)| name.as_str().eq_ignore_ascii_case(header))
                    .flat_map(|(_, values)| {
                        values
                            .iter()
                            .map(|value| value.as_str().to_owned())
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Every request line the service received, in order: the method and the
    /// full path with its query — what a URL sink would have leaked into.
    pub async fn request_lines(&self) -> Vec<(String, String)> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                let query = r.url.query().map(|q| format!("?{q}")).unwrap_or_default();
                (r.method.to_string(), format!("{}{query}", r.url.path()))
            })
            .collect()
    }

    /// Every request the service received, in order: `(method, path, body)`.
    pub async fn receipts(&self) -> Vec<(String, String, String)> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                (
                    r.method.to_string(),
                    r.url.path().to_string(),
                    String::from_utf8_lossy(&r.body).into_owned(),
                )
            })
            .collect()
    }

    /// The bodies posted to `path`.
    pub async fn posted_to(&self, wanted: &str) -> Vec<String> {
        self.receipts()
            .await
            .into_iter()
            .filter(|(m, p, _)| m == "POST" && p == wanted)
            .map(|(_, _, b)| b)
            .collect()
    }
}

/// A set for de-duplicating sightings by location in reports.
pub fn locations(sightings: &[Sighting]) -> BTreeSet<String> {
    sightings.iter().map(|s| s.location.clone()).collect()
}
