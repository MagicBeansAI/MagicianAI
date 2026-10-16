//! Social login — Google and GitHub, authorization-code + PKCE.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md` §3.2.
//! The server holds the PKCE verifier and the state ticket (bounded,
//! 10-minute TTL, single-use); the browser only carries `code` and `state`
//! back. Identities bind on the provider's **subject id, never the email**;
//! no auto-link by email — a second provider attaches only through the
//! explicit `link_to` flow.
//!
//! Protocol note (deliberate deviation from the design's "oauth2 crate"
//! row): that crate is compiled without its HTTP feature in this workspace
//! and needs an adapter to do what four plain `reqwest` calls already do.
//! PKCE here follows RFC 7636 directly — `challenge =
//! BASE64URL(SHA256(verifier))` — over `sha2`/`base64`/`rand` already in
//! the dependency tree: no new dependency, no second HTTP stack.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use parking_lot::Mutex;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::credentials::Provider;
use crate::magician_v2::auth::validate_principal_name;

/// Bound and lifetime for start tickets — the `mcp_oauth.rs` shape: lazy
/// sweep on every access, hard capacity, replay protection by consumption.
const MAX_LIVE_TICKETS: usize = 64;
const TICKET_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, thiserror::Error)]
pub enum SocialError {
    #[error("unknown or expired state ticket")]
    UnknownTicket,
    #[error("too many concurrent login flows; retry shortly")]
    RegistryFull,
    #[error("token exchange failed: {0}")]
    Exchange(String),
    #[error("profile fetch failed: {0}")]
    Profile(String),
    #[error("provider returned no usable subject")]
    NoSubject,
}

/// The minimal provider profile auth cares about: the stable subject and a
/// display-name hint. Email is informational only — never an identity key.
#[derive(Debug, Clone)]
pub struct SocialProfile {
    pub subject: String,
    pub display_name: String,
    pub email_hint: Option<String>,
}

/// One in-flight authorize flow. The verifier never leaves the server.
#[derive(Debug, Clone)]
pub struct SocialTicket {
    pub provider: Provider,
    pub state: String,
    pub pkce_verifier: String,
    /// `Some(identity)` for the explicit link flow — the callback attaches
    /// a credential instead of creating an identity.
    pub link_to: Option<String>,
    pub created_at: Instant,
}

#[derive(Default)]
pub struct SocialFlows {
    tickets: Mutex<HashMap<String, SocialTicket>>,
}

impl SocialFlows {
    /// Mint a single-use ticket, sweeping expired entries and refusing at
    /// capacity (an attacker cannot flood the registry to lock out logins:
    /// capacity is 64 and TTL is 10 minutes).
    pub fn mint(
        &self,
        provider: Provider,
        link_to: Option<String>,
    ) -> Result<SocialTicket, SocialError> {
        let mut tickets = self.tickets.lock();
        let now = Instant::now();
        tickets.retain(|_, ticket| now.duration_since(ticket.created_at) < TICKET_TTL);
        if tickets.len() >= MAX_LIVE_TICKETS {
            return Err(SocialError::RegistryFull);
        }
        let ticket = SocialTicket {
            provider,
            state: random_url_token(32),
            pkce_verifier: pkce_verifier(),
            link_to,
            created_at: now,
        };
        tickets.insert(ticket.state.clone(), ticket.clone());
        Ok(ticket)
    }

    /// Consume a ticket by state — single-use by construction; an unknown,
    /// expired, or already-used state is the same error. A *wrong-provider*
    /// consume does not burn the ticket: it peeks, refuses, and leaves the
    /// real holder's flow intact.
    pub fn consume(&self, provider: Provider, state: &str) -> Result<SocialTicket, SocialError> {
        if state.len() > 256
            || !state
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(SocialError::UnknownTicket);
        }
        let mut tickets = self.tickets.lock();
        let now = Instant::now();
        tickets.retain(|_, ticket| now.duration_since(ticket.created_at) < TICKET_TTL);
        if let Some(ticket) = tickets.get(state) {
            if ticket.provider != provider {
                return Err(SocialError::UnknownTicket);
            }
        }
        match tickets.remove(state) {
            Some(ticket) => Ok(ticket),
            None => Err(SocialError::UnknownTicket),
        }
    }
}

fn random_url_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

/// RFC 7636: verifier = 43+ unreserved chars; challenge =
/// BASE64URL(SHA256(verifier)).
fn pkce_verifier() -> String {
    // 32 bytes → 43 base64url chars — the minimum legal verifier length.
    random_url_token(32)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The provider's authorize URL. Scopes per the design: Google
/// `openid email profile`; GitHub `read:user user:email`.
pub fn authorize_url(
    provider: Provider,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    match provider {
        Provider::Google => format!(
            "https://accounts.google.com/o/oauth2/v2/auth?response_type=code\
             &client_id={client_id}&redirect_uri={redirect_uri}\
             &scope=openid%20email%20profile&state={state}\
             &code_challenge={code_challenge}&code_challenge_method=S256"
        ),
        Provider::Github => format!(
            "https://github.com/login/oauth/authorize?response_type=code\
             &client_id={client_id}&redirect_uri={redirect_uri}\
             &scope=read:user%20user:email&state={state}\
             &code_challenge={code_challenge}&code_challenge_method=S256"
        ),
    }
}

pub fn token_url(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "https://oauth2.googleapis.com/token",
        Provider::Github => "https://github.com/login/oauth/access_token",
    }
}

/// The HTTP seam social login needs — production uses `reqwest` (see
/// `ReqwestSocialHttp`); tests fake it so no flow test touches the network.
#[async_trait::async_trait]
pub trait SocialHttp: Send + Sync {
    /// POST form-encoded with client credentials; `Err` covers transport
    /// and non-2xx responses.
    async fn post_form_json(
        &self,
        url: &str,
        client_id: &str,
        client_secret: &str,
        form: &[(&'static str, String)],
    ) -> Result<Value, String>;
    /// GET with a bearer token; `Err` covers transport and non-2xx.
    async fn get_bearer_json(&self, url: &str, token: &str) -> Result<Value, String>;
}

pub struct ReqwestSocialHttp;

#[async_trait::async_trait]
impl SocialHttp for ReqwestSocialHttp {
    async fn post_form_json(
        &self,
        url: &str,
        client_id: &str,
        client_secret: &str,
        form: &[(&'static str, String)],
    ) -> Result<Value, String> {
        let mut params: Vec<(&str, String)> = vec![
            ("client_id", client_id.to_string()),
            ("client_secret", client_secret.to_string()),
        ];
        for (key, value) in form {
            params.push((key, value.clone()));
        }
        let response = reqwest::Client::new()
            .post(url)
            .header("Accept", "application/json")
            .form(&params)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|error| error.to_string())?;
        if !status.is_success() {
            let detail = body["error"].as_str().unwrap_or("unknown error");
            return Err(format!("HTTP {status}: {detail}"));
        }
        Ok(body)
    }

    async fn get_bearer_json(&self, url: &str, token: &str) -> Result<Value, String> {
        let response = reqwest::Client::new()
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .header("User-Agent", "magician-auth")
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|error| error.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        Ok(body)
    }
}

/// Exchange the authorization code for an access token, server-side — the
/// client never sees it (identity doc §9's property).
pub async fn exchange_code(
    http: &dyn SocialHttp,
    provider: Provider,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<String, SocialError> {
    let form: Vec<(&'static str, String)> = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("code_verifier", verifier.to_string()),
    ];
    let body = http
        .post_form_json(token_url(provider), client_id, client_secret, &form)
        .await
        .map_err(SocialError::Exchange)?;
    body["access_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| SocialError::Exchange("no access_token in provider response".to_string()))
}

/// Fetch the profile and extract the stable subject. Google:
/// `/v3/userinfo` (`sub`). GitHub: `/user` (`id`) plus `/user/emails` for
/// the verified primary address — informational only.
pub async fn fetch_profile(
    http: &dyn SocialHttp,
    provider: Provider,
    token: &str,
) -> Result<SocialProfile, SocialError> {
    match provider {
        Provider::Google => {
            let body = http
                .get_bearer_json("https://openidconnect.googleapis.com/v3/userinfo", token)
                .await
                .map_err(SocialError::Profile)?;
            let subject = body["sub"]
                .as_str()
                .filter(|subject| !subject.is_empty())
                .ok_or(SocialError::NoSubject)?;
            Ok(SocialProfile {
                subject: subject.to_string(),
                display_name: body["name"].as_str().unwrap_or("Google user").to_string(),
                email_hint: body["email"].as_str().map(str::to_string),
            })
        },
        Provider::Github => {
            let user = http
                .get_bearer_json("https://api.github.com/user", token)
                .await
                .map_err(SocialError::Profile)?;
            let Some(subject) = user["id"].as_i64() else {
                return Err(SocialError::NoSubject);
            };
            let display_name = user["name"]
                .as_str()
                .or_else(|| user["login"].as_str())
                .unwrap_or("GitHub user")
                .to_string();
            // Email is a hint only: verified primary, ignored on failure.
            let email_hint = http
                .get_bearer_json("https://api.github.com/user/emails", token)
                .await
                .ok()
                .and_then(|emails| emails.as_array().cloned())
                .and_then(|entries| {
                    entries
                        .into_iter()
                        .find(|entry| {
                            entry["primary"].as_bool().unwrap_or(false)
                                && entry["verified"].as_bool().unwrap_or(false)
                        })
                        .and_then(|entry| entry["email"].as_str().map(str::to_string))
                });
            Ok(SocialProfile {
                subject: subject.to_string(),
                display_name,
                email_hint,
            })
        },
    }
}

/// Derive a principal name for a new social identity (§13 open Q1's
/// default proposal): provider-prefixed subject slug, sanitized to the
/// principal pattern, falling back to a hash tail when sanitizing cannot
/// produce a legal name.
pub fn derive_principal_name(provider: Provider, subject: &str) -> String {
    let prefix = provider.as_str();
    let slug: String = subject
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let max_subject_len = 32usize.saturating_sub(prefix.len() + 1);
    let candidate: String = format!(
        "{prefix}-{}",
        slug.chars().take(max_subject_len).collect::<String>()
    )
    .trim_matches('-')
    .to_string();
    let candidate: String = candidate.chars().take(32).collect();
    if validate_principal_name(&candidate).is_ok() && candidate.len() >= prefix.len() + 2 {
        return candidate;
    }
    // Sanitizing cannot produce a legal name (too short after trimming, or
    // multi-byte truncation broke the pattern): hash tail instead.
    let digest = Sha256::digest(subject.as_bytes());
    format!(
        "{prefix}-{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

/// The suffix loop for collisions, separated so it is testable without a
/// store: `name`, `name-2`, `name-3`, … always within 32 chars.
pub fn collision_variant(name: &str, attempt: u32) -> String {
    if attempt <= 1 {
        return name.to_string();
    }
    let suffix = format!("-{attempt}");
    let keep = 32usize.saturating_sub(suffix.len());
    format!("{}{}", name.chars().take(keep).collect::<String>(), suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_shape() {
        let verifier = pkce_verifier();
        assert!(verifier.len() >= 43, "verifier must be at least 43 chars");
        assert!(verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
        let challenge = pkce_challenge(&verifier);
        assert_eq!(challenge.len(), 43, "SHA-256 base64url is 43 chars");
        assert_ne!(challenge, verifier);
    }

    #[test]
    fn tickets_are_single_use_and_provider_bound() {
        let flows = SocialFlows::default();
        let ticket = flows.mint(Provider::Google, None).expect("mint");
        assert!(matches!(
            flows.consume(Provider::Github, &ticket.state),
            Err(SocialError::UnknownTicket)
        ));
        assert!(flows.consume(Provider::Google, &ticket.state).is_ok());
        assert!(matches!(
            flows.consume(Provider::Google, &ticket.state),
            Err(SocialError::UnknownTicket)
        ));
        assert!(matches!(
            flows.consume(Provider::Google, "../../etc"),
            Err(SocialError::UnknownTicket)
        ));
    }

    #[test]
    fn derived_names_fit_the_pattern_and_collisions_stay_in_bounds() {
        for (provider, subject) in [
            (Provider::Google, "1234567890"),
            (Provider::Github, "63647"),
            (Provider::Google, "ünïcode-_subject-exotic-very-long-thing"),
        ] {
            let name = derive_principal_name(provider, subject);
            assert!(
                validate_principal_name(&name).is_ok(),
                "{name} must match the pattern"
            );
        }
        let variant = collision_variant("owner-company-workspace-name-long", 7);
        assert!(variant.len() <= 32);
        assert!(variant.ends_with("-7"));
        assert_eq!(collision_variant("plain", 1), "plain");
    }

    /// A fake transport for flow tests: one token response, then profiles.
    struct FakeHttp {
        exchange_response: Result<Value, String>,
        profile_response: Result<Value, String>,
    }

    #[async_trait::async_trait]
    impl SocialHttp for FakeHttp {
        async fn post_form_json(
            &self,
            _url: &str,
            _client_id: &str,
            _client_secret: &str,
            _form: &[(&'static str, String)],
        ) -> Result<Value, String> {
            self.exchange_response.clone()
        }
        async fn get_bearer_json(&self, _url: &str, _token: &str) -> Result<Value, String> {
            self.profile_response.clone()
        }
    }

    #[tokio::test]
    async fn exchange_uses_the_verifier_and_extracts_the_token() {
        let http = FakeHttp {
            exchange_response: Ok(
                serde_json::json!({"access_token": "at-1", "token_type": "Bearer"}),
            ),
            profile_response: Ok(serde_json::json!({"sub": "sub-1", "name": "Ada"})),
        };
        let token = exchange_code(
            &http,
            Provider::Google,
            "id",
            "secret",
            "code",
            "http://127.0.0.1/cb",
            "verifier",
        )
        .await
        .expect("exchange");
        assert_eq!(token, "at-1");
        let profile = fetch_profile(&http, Provider::Google, &token)
            .await
            .expect("profile");
        assert_eq!(profile.subject, "sub-1");
        assert_eq!(profile.display_name, "Ada");
    }

    #[tokio::test]
    async fn github_profile_takes_numeric_id_as_the_subject() {
        let http = FakeHttp {
            exchange_response: Err("unused".into()),
            profile_response: Ok(serde_json::json!({"id": 63647, "login": "ada", "name": null})),
        };
        let profile = fetch_profile(&http, Provider::Github, "t")
            .await
            .expect("profile");
        assert_eq!(profile.subject, "63647");
        assert_eq!(profile.display_name, "ada", "login is the display fallback");
        assert_eq!(profile.email_hint, None);
    }
}
