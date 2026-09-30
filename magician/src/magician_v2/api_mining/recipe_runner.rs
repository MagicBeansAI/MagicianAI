//! Rail 1: replay a task recipe over HTTP without starting a browser.

use super::capability::{classify_side_effects_for_request, SideEffects};
use super::origin_policy::{OriginPolicyStore, OriginReplayMode};
use super::recipe::*;
use super::recipe_observer::{RecipeEvent, RunObserver};
use super::replay_grants::{is_denylisted_url_template, GrantKey, ReplayGrantStore};
use super::types::SessionContext;
use super::workflow::BrowserFallbackStep;
use super::workflow_replay::jsonpath::extract_jsonpath;
use crate::magician_v2::secrets::{
    cookie_domain_matches_host, filter_cookies_for_url, CookieWithMetadata, SameSite,
};
use aho_corasick::AhoCorasick;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const DEFAULT_RECIPE_TIMEOUT_MS: u64 = 15_000;
pub const MAX_RECIPE_TIMEOUT_MS: u64 = 120_000;
const MAX_RECIPE_INPUTS: usize = 64;
const MAX_RECIPE_INPUT_VALUE_BYTES: usize = 64 * 1024;
const MAX_RECIPE_STEPS: usize = 64;
const MAX_RECIPE_DATA_FLOWS: usize = 256;
const MAX_RECIPE_ANSWER_FIELDS: usize = 64;
const MAX_RECIPE_STEP_PARAMS: usize = 128;
const MAX_RECIPE_HEADERS: usize = 128;
const MAX_RECIPE_URL_BYTES: usize = 64 * 1024;
const MAX_RECIPE_HEADER_BYTES: usize = 64 * 1024;
const MAX_RECIPE_RESPONSE_HEADER_BYTES: usize = 256 * 1024;
const MAX_RECIPE_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECIPE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECIPE_TOTAL_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EXTRACTED_VALUE_BYTES: usize = 256 * 1024;
const MAX_VERIFICATION_JSON_NODES: usize = 65_536;
const MAX_VERIFICATION_TEXT_MATCHES: usize = 65_536;
const MAX_RECIPE_COOKIES: usize = 256;
const MAX_RECIPE_COOKIE_BYTES: usize = 16 * 1024;
const CHALLENGE_MARKERS: &[&str] = &[
    "just a moment",
    "cf-chl",
    "captcha",
    "challenge-platform",
    "access denied",
    "akamai",
    "perimeterx",
    "_incapsula_",
];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecipeRunInputs {
    #[serde(default)]
    pub inputs: HashMap<String, String>,
    /// Total workflow budget, not a per-step budget.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// One-run approvals obtained by the caller's HITL rail.
    /// This is a trusted in-process capability and is deliberately never
    /// accepted from JSON-facing replay endpoints.
    #[serde(skip)]
    pub approved_write_steps: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    Auth,
    AntiBot,
    SchemaDrift,
    Http,
    Network,
    PolicyBlocked,
    InputMissing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeStepOutcome {
    pub step_id: String,
    pub method: String,
    /// Request template with query values redacted from observability output.
    /// Dynamic path inputs are therefore never copied into ledgers/events.
    pub url: String,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub transport: Transport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipeRunFailure {
    pub step_id: String,
    pub class: FailureClass,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipeFallback {
    pub step_id: String,
    pub class: FailureClass,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserFallbackStep>,
    pub replayed: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingApproval {
    pub step_id: String,
    pub grant_key: GrantKey,
    pub origin: String,
    pub method: String,
    pub url_template: String,
    pub side_effects: SideEffects,
    pub always_ask: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipeRunResult {
    pub success: bool,
    pub auth_heals: u32,
    pub answer: serde_json::Map<String, serde_json::Value>,
    pub steps: Vec<RecipeStepOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RecipeRunFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<RecipeFallback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval: Option<PendingApproval>,
}

impl RecipeRunResult {
    pub fn write_outcome_uncertain(&self, recipe: &TaskRecipe) -> bool {
        self.failure.as_ref().is_some_and(|failure| {
            recipe.current().is_some_and(|version| {
                version.steps.iter().any(|step| {
                    step.id == failure.step_id && step.side_effects == SideEffects::Write
                })
            })
        })
    }
}

#[derive(Debug, Clone)]
pub struct TransportRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct TransportResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
}

#[async_trait::async_trait]
pub trait StepTransport: Send + Sync {
    fn kind(&self) -> Transport;
    async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String>;
}

pub struct ReqwestTransport {
    client: Result<reqwest::Client, String>,
}

static RECIPE_HTTP_CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
    std::sync::OnceLock::new();

impl Default for ReqwestTransport {
    fn default() -> Self {
        // Client clones share the bounded connection pool across warm runs.
        // Credentials and cookies remain explicit per-request values; the
        // shared client has no cookie jar or scope-specific default headers.
        let client = RECIPE_HTTP_CLIENT
            .get_or_init(|| {
                reqwest::Client::builder()
                    // Authentication redirects must be classified, not silently
                    // followed into a login HTML page.
                    .redirect(reqwest::redirect::Policy::none())
                    .pool_idle_timeout(Duration::from_secs(90))
                    .pool_max_idle_per_host(8)
                    .build()
                    .map_err(|error| {
                        format!("could not initialize the recipe HTTP transport: {error}")
                    })
            })
            .clone();
        Self { client }
    }
}

#[async_trait::async_trait]
impl StepTransport for ReqwestTransport {
    fn kind(&self) -> Transport {
        Transport::Reqwest
    }

    async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| error.to_string())?;
        let mut builder = self
            .client
            .as_ref()
            .map_err(|error| error.clone())?
            .request(method, &request.url)
            .timeout(request.timeout);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        let mut response = builder
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RECIPE_RESPONSE_BYTES as u64)
        {
            return Err("response body exceeded the Task Recipe limit".into());
        }
        let status = response.status().as_u16();
        // Enforce the response-header budget while copying out of reqwest's
        // HeaderMap. Waiting until after this copy would let a hostile origin
        // amplify the runner's retained memory before the shared transport
        // boundary rejects the response.
        let headers = collect_response_headers(response.headers())?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| error.without_url().to_string())?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RECIPE_RESPONSE_BYTES {
                return Err("response body exceeded the Task Recipe limit".into());
            }
            body.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&body).into_owned();
        Ok(TransportResponse {
            status,
            headers,
            body,
        })
    }
}

#[async_trait::async_trait]
pub trait AuthHealer: Send + Sync {
    async fn heal(&self, origin: &str) -> Result<bool, String>;
}

pub type StepFeedback<'a> = &'a (dyn Fn(&str, &str, &str, bool, bool, u16, &str) + Send + Sync);

pub struct RecipeRunner<'a> {
    /// Ordered transport ladder. Rail 1 normally supplies only reqwest;
    /// browser-context callers may append in-page fetch. Writes use exactly
    /// one transport attempt even when more are available.
    pub transports: Vec<Box<dyn StepTransport>>,
    pub grants: &'a ReplayGrantStore,
    pub origin_policy: &'a OriginPolicyStore,
    /// Live scope/process fence, rechecked immediately before every send.
    pub can_continue: Option<&'a (dyn Fn() -> bool + Send + Sync)>,
    pub session_lookup: &'a (dyn Fn(&str, &str) -> Option<SessionContext> + Send + Sync),
    pub auth_healer: Option<&'a dyn AuthHealer>,
    pub max_auth_heals: u32,
    pub step_feedback: Option<StepFeedback<'a>>,
    pub observer: Option<&'a dyn RunObserver>,
}

struct StoredResponse {
    json: Option<serde_json::Value>,
    text: String,
    headers: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct VerificationExpectation {
    value: String,
    schema: TaskInputSchema,
}

#[derive(Default)]
struct JsonScalarIndex<'a> {
    strings: HashSet<&'a str>,
    numbers: HashSet<String>,
    booleans: HashSet<bool>,
}

#[derive(Default)]
struct JsonVerificationRecord<'a> {
    scalars: JsonScalarIndex<'a>,
    parent: Option<usize>,
}

enum VerificationValue<'a> {
    String(&'a str),
    Number(String),
    Boolean(bool),
}

#[derive(Default)]
struct CookieJar {
    response_cookies: Vec<CookieWithMetadata>,
}

impl CookieJar {
    fn absorb(
        &mut self,
        request_url: &str,
        response_headers: &HashMap<String, String>,
    ) -> Result<(), String> {
        let Some(set_cookie) = response_headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value)
        else {
            return Ok(());
        };
        let Ok(url) = url::Url::parse(request_url) else {
            return Ok(());
        };
        let Some(request_host) = url.host_str() else {
            return Ok(());
        };
        // Reqwest values are joined with newlines above. This intentionally
        // avoids splitting on commas, which occur in Expires attributes.
        for line in set_cookie.lines() {
            let Some(cookie) = parse_set_cookie(line, &url) else {
                continue;
            };
            if !cookie_domain_matches_host(&cookie.domain, request_host) {
                continue;
            }
            let position = self
                .response_cookies
                .iter()
                .position(|candidate| same_cookie_identity(candidate, &cookie));
            // Keep expired entries as tombstones. Dropping them would let the
            // next SecretStore snapshot resurrect the captured old cookie.
            if let Some(position) = position {
                self.response_cookies[position] = cookie;
            } else if self.response_cookies.len() < MAX_RECIPE_COOKIES {
                self.response_cookies.push(cookie);
            } else {
                return Err("response cookie count exceeds the Task Recipe limit".into());
            }
        }
        Ok(())
    }

    fn forget_refreshed_origin(&mut self, origin: &str) {
        if let Ok(url) = url::Url::parse(origin) {
            if let Some(host) = url.host_str() {
                self.response_cookies
                    .retain(|cookie| !cookie_domain_matches_host(&cookie.domain, host));
            }
        }
    }

    fn merge_session(&self, session: &mut SessionContext, request_url: &str) -> Result<(), String> {
        let url = url::Url::parse(request_url).map_err(|_| "invalid cookie request URL")?;
        let count = session
            .cookie_metadata
            .len()
            .max(session.cookie_header_values.len())
            .max(session.cookies.len());
        if count > MAX_RECIPE_COOKIES {
            return Err("session cookie count exceeds the Task Recipe limit".into());
        }
        let now = chrono::Utc::now().timestamp();
        if !session.cookie_metadata.is_empty() {
            let updated: HashSet<_> = self.response_cookies.iter().map(cookie_identity).collect();
            let mut cookies: Vec<_> = session
                .cookie_metadata
                .iter()
                .filter(|captured| !updated.contains(&cookie_identity(captured)))
                .cloned()
                .collect();
            cookies.extend(self.response_cookies.iter().cloned());
            // Expiry is an inclusive boundary; tombstones shadow captured
            // identities but must never be serialized as outbound cookies.
            cookies.retain(|cookie| cookie.expires.is_none_or(|expires| expires > now));
            session.cookie_metadata = filter_cookies_for_url(&cookies, &url);
            session.cookie_header_values = session
                .cookie_metadata
                .iter()
                .map(|cookie| super::types::SessionCookie {
                    name: cookie.name.clone(),
                    value: cookie.value.clone(),
                })
                .collect();
        } else {
            // Legacy/in-process callers may supply only already-filtered pairs.
            // They lack identity metadata, so preserve the existing name-based
            // override, including deletion, only in the update's URL scope.
            let routing: Vec<_> = self
                .response_cookies
                .iter()
                .cloned()
                .map(|mut cookie| {
                    cookie.expires = None;
                    cookie
                })
                .collect();
            let replaced: HashSet<_> = filter_cookies_for_url(&routing, &url)
                .into_iter()
                .map(|cookie| cookie.name)
                .collect();
            if session.cookie_header_values.is_empty() {
                session.cookie_header_values = session
                    .cookies
                    .iter()
                    .map(|(name, value)| super::types::SessionCookie {
                        name: name.clone(),
                        value: value.clone(),
                    })
                    .collect();
                session
                    .cookie_header_values
                    .sort_by(|left, right| left.name.cmp(&right.name));
            }
            session
                .cookie_header_values
                .retain(|cookie| !replaced.contains(&cookie.name));
            let live: Vec<_> = self
                .response_cookies
                .iter()
                .filter(|cookie| cookie.expires.is_none_or(|expires| expires > now))
                .cloned()
                .collect();
            session.cookie_header_values.extend(
                filter_cookies_for_url(&live, &url)
                    .into_iter()
                    .map(|cookie| super::types::SessionCookie {
                        name: cookie.name,
                        value: cookie.value,
                    }),
            );
        }
        if session.cookie_header_values.len() > MAX_RECIPE_COOKIES {
            return Err("session cookie count exceeds the Task Recipe limit".into());
        }
        session.cookies.clear();
        for cookie in &session.cookie_header_values {
            session
                .cookies
                .entry(cookie.name.clone())
                .or_insert_with(|| cookie.value.clone());
        }
        Ok(())
    }
}

fn same_cookie_identity(left: &CookieWithMetadata, right: &CookieWithMetadata) -> bool {
    left.name == right.name
        && left
            .domain
            .trim_start_matches('.')
            .eq_ignore_ascii_case(right.domain.trim_start_matches('.'))
        && left.path == right.path
}

fn cookie_identity(cookie: &CookieWithMetadata) -> (&str, String, &str) {
    (
        &cookie.name,
        cookie.domain.trim_start_matches('.').to_ascii_lowercase(),
        &cookie.path,
    )
}

fn parse_set_cookie(header: &str, request_url: &url::Url) -> Option<CookieWithMetadata> {
    if header.len() > MAX_RECIPE_HEADER_BYTES {
        return None;
    }
    let mut fields = header.split(';');
    let (name, value) = fields.next()?.trim().split_once('=')?;
    let name = name.trim();
    if name.is_empty()
        || name.len().saturating_add(value.len()) > MAX_RECIPE_COOKIE_BYTES
        || name.chars().any(char::is_control)
    {
        return None;
    }
    let request_host = request_url.host_str()?;
    let mut cookie = CookieWithMetadata {
        name: name.to_owned(),
        value: value.trim().to_owned(),
        domain: request_host.to_ascii_lowercase(),
        path: default_cookie_path(request_url.path()),
        secure: false,
        http_only: false,
        same_site: SameSite::Lax,
        expires: None,
    };
    let mut max_age = None;
    for field in fields {
        let field = field.trim();
        if field.len() > MAX_RECIPE_COOKIE_BYTES {
            return None;
        }
        let (attribute, value) = field
            .split_once('=')
            .map(|(name, value)| (name.trim(), Some(value.trim())))
            .unwrap_or((field, None));
        match attribute.to_ascii_lowercase().as_str() {
            "domain" => {
                let domain = value?.trim_start_matches('.');
                if domain.is_empty() {
                    return None;
                }
                let domain_cookie = format!(".{domain}");
                if !cookie_domain_matches_host(&domain_cookie, request_host) {
                    return None;
                }
                cookie.domain = domain_cookie;
            },
            "path" => {
                if let Some(path) = value.filter(|path| path.starts_with('/')) {
                    cookie.path = path.to_owned();
                }
            },
            "secure" => cookie.secure = true,
            "httponly" => cookie.http_only = true,
            "samesite" => {
                cookie.same_site = match value.unwrap_or_default().to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "none" => SameSite::None,
                    _ => SameSite::Lax,
                };
            },
            "max-age" => {
                if let Ok(seconds) = value.unwrap_or_default().parse::<i64>() {
                    max_age = Some(
                        chrono::Utc::now()
                            .timestamp()
                            .saturating_add(seconds.max(0)),
                    );
                }
            },
            "expires" => {
                if let Some(expires) = value.and_then(parse_cookie_expiry) {
                    cookie.expires = Some(expires);
                }
            },
            _ => {},
        }
    }
    // Max-Age takes precedence independently of the attribute order.
    cookie.expires = max_age.or(cookie.expires);
    if cookie.secure && request_url.scheme() != "https" {
        return None;
    }
    Some(cookie)
}

fn parse_cookie_expiry(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.timestamp())
        .or_else(|| {
            [
                "%a, %d-%b-%Y %H:%M:%S GMT",
                "%A, %d-%b-%y %H:%M:%S GMT",
                "%a %b %e %H:%M:%S %Y",
            ]
            .iter()
            .find_map(|format| chrono::NaiveDateTime::parse_from_str(value, format).ok())
            .map(|date| date.and_utc().timestamp())
        })
}

fn default_cookie_path(request_path: &str) -> String {
    if !request_path.starts_with('/') || request_path == "/" {
        return "/".into();
    }
    match request_path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(position) => request_path[..position].to_owned(),
    }
}

fn apply_cookie_header(
    request: &mut TransportRequest,
    session: &SessionContext,
) -> Result<(), String> {
    // resolve_step_context already merged and URL-filtered cookies. Do not
    // clone credentials and repeat the jar merge on every send.
    let pairs: Vec<_> = session
        .cookie_header_values
        .iter()
        .map(|cookie| (&cookie.name, &cookie.value))
        .collect();
    let cookie_bytes = pairs.iter().try_fold(0usize, |total, (name, value)| {
        if name.len().saturating_add(value.len()) > MAX_RECIPE_COOKIE_BYTES {
            return None;
        }
        total
            .checked_add(name.len())?
            .checked_add(value.len())?
            .checked_add(2)
    });
    if cookie_bytes.is_none_or(|length| length > MAX_RECIPE_HEADER_BYTES) {
        return Err("session cookie header exceeds the Task Recipe limit".into());
    }
    if !request.headers.contains_key("cookie") && request.headers.len() >= MAX_RECIPE_HEADERS {
        return Err("session cookies exceed the Task Recipe header-count limit".into());
    }
    if pairs.is_empty() {
        request.headers.remove("cookie");
    } else {
        request.headers.insert(
            "cookie".into(),
            pairs
                .into_iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
    Ok(())
}

impl<'a> RecipeRunner<'a> {
    pub async fn run(&self, recipe: &mut TaskRecipe, inputs: &RecipeRunInputs) -> RecipeRunResult {
        let run_started = Instant::now();
        let recipe_id = recipe.id.clone();
        let mut result = RecipeRunResult {
            success: false,
            auth_heals: 0,
            answer: Default::default(),
            steps: Vec::new(),
            failure: None,
            fallback: None,
            pending_approval: None,
        };
        let preflight_validation = validate_recipe_preflight(recipe, inputs);
        let task_input_schemas: HashMap<_, _> = recipe
            .shape
            .inputs
            .iter()
            .map(|input| (input.name.clone(), input.schema))
            .collect();
        let Some(version) = recipe.current_mut() else {
            result.failure = Some(RecipeRunFailure {
                step_id: String::new(),
                class: FailureClass::SchemaDrift,
                detail: "recipe has no current version".into(),
            });
            return result;
        };
        if let Some(observer) = self.observer {
            observer.observe(RecipeEvent::Started {
                recipe_id: &recipe_id,
                version: version.version,
                inputs: &inputs.inputs,
            });
        }

        if let Err((class, detail)) = preflight_validation {
            bump_failure(version, class);
            if let Some(observer) = self.observer {
                observer.observe(RecipeEvent::StepFailed {
                    step_id: "",
                    class,
                    detail: &detail,
                });
            }
            result.failure = Some(RecipeRunFailure {
                step_id: String::new(),
                class,
                detail,
            });
            return result;
        }

        if version.steps.is_empty() {
            bump_stats(version, false);
            if let Some(observer) = self.observer {
                observer.observe(RecipeEvent::StepFailed {
                    step_id: "",
                    class: FailureClass::SchemaDrift,
                    detail: "recipe current version has no steps",
                });
            }
            result.failure = Some(RecipeRunFailure {
                step_id: String::new(),
                class: FailureClass::SchemaDrift,
                detail: "recipe current version has no steps".into(),
            });
            return result;
        }

        // Reject malformed/blocked graphs before approval or transport work. The
        // same policy is checked again per step so a mid-run policy change remains
        // a hard boundary.
        let write_step_ids: HashSet<_> = version
            .steps
            .iter()
            .filter(|step| step.side_effects == SideEffects::Write)
            .map(|step| step.id.clone())
            .collect();
        if let Some(step) = version.steps.iter().find(|step| {
            step.side_effects == SideEffects::Unknown
                || self.origin_policy.is_blocked(&step.origin)
                || matches!(
                    self.origin_policy.live_replay_mode_for_origin(&step.origin),
                    OriginReplayMode::ObserveOnly | OriginReplayMode::ValidateOnly
                )
        }) {
            let step = step.clone();
            bump_stats(version, false);
            return fall_back(
                result,
                &step,
                FailureClass::PolicyBlocked,
                "origin policy or unknown side effects block replay",
                &HashMap::new(),
                &write_step_ids,
                self.observer,
            );
        }

        // Authorize the entire mutation set before step 1. Waiting until a
        // write step is reached permits earlier reads to partially execute a
        // workflow the user has not approved as a whole.
        let write_steps: Vec<_> = version
            .steps
            .iter()
            .filter(|step| step.side_effects == SideEffects::Write)
            .cloned()
            .collect();
        for step in write_steps {
            if self.origin_policy.is_blocked(&step.origin)
                || matches!(
                    self.origin_policy.live_replay_mode_for_origin(&step.origin),
                    OriginReplayMode::ObserveOnly | OriginReplayMode::ValidateOnly
                )
            {
                bump_stats(version, false);
                return fall_back(
                    result,
                    &step,
                    FailureClass::PolicyBlocked,
                    "origin policy blocks write replay",
                    &HashMap::new(),
                    &write_step_ids,
                    self.observer,
                );
            }
            let grant_key = GrantKey {
                recipe_id: Some(recipe_id.clone()),
                step_id: Some(step.id.clone()),
                capability_id: step.capability_id.clone(),
                request_shape_fingerprint: step.effective_request_shape_fingerprint(),
            };
            let always_ask = is_denylisted_url_template(&step.url_template)
                || !request_body_shape_is_grantable(step.body_template.as_deref());
            let approved_once = inputs.approved_write_steps.contains(&step.id);
            if !approved_once && (always_ask || self.grants.lookup(&grant_key).is_none()) {
                result.pending_approval = Some(PendingApproval {
                    step_id: step.id.clone(),
                    grant_key,
                    origin: step.origin.clone(),
                    method: step.method.clone(),
                    url_template: super::approval::approval_url_shape(&step.url_template),
                    side_effects: step.side_effects,
                    always_ask,
                });
                return result;
            }
        }

        let flows: HashMap<_, _> = version
            .data_flows
            .iter()
            .map(|flow| (flow.id.clone(), flow.clone()))
            .collect();
        let mut prior = HashMap::<String, StoredResponse>::new();
        let mut jar = CookieJar::default();
        // A single later read may verify more than one write. Keep one value
        // group per write so an echo from write B cannot accidentally verify
        // write A. Current task inputs and earlier-response data flows qualify
        // because both are actual dynamic values bound into this write. Auth,
        // timestamps, and stable literals never count as mutation evidence.
        let mut verify_expectations = HashMap::<String, Vec<Vec<VerificationExpectation>>>::new();
        let mut heals_used = 0;
        let mut retained_response_bytes = 0usize;
        let deadline = Instant::now()
            + Duration::from_millis(
                inputs
                    .timeout_ms
                    .unwrap_or(DEFAULT_RECIPE_TIMEOUT_MS)
                    .clamp(1, MAX_RECIPE_TIMEOUT_MS),
            );
        let mut must_verify = HashSet::new();
        let mut verify_failed = false;
        // Older cookie-only recipes retained only this aggregate hint, not
        // per-step cookie requirements. Fail safely if their captured session
        // is absent; recompilation permits unauthenticated bootstrap steps to
        // remain separate from explicitly cookie-dependent steps.
        let legacy_cookie_only = version.auth.requires_session
            && version.steps.iter().all(|step| {
                step.param_sources
                    .values()
                    .all(|source| !matches!(source, RecipeParamSource::SessionAuth { .. }))
            });

        for index in 0..version.steps.len() {
            let step = version.steps[index].clone();
            let require_legacy_cookies = legacy_cookie_only
                && (version.auth.origins_needing_auth.is_empty()
                    || version.auth.origins_needing_auth.contains(&step.origin));
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bump_stats(version, false);
                return fall_back(
                    result,
                    &step,
                    FailureClass::Network,
                    "recipe timeout budget exhausted",
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            }

            if self.origin_policy.is_blocked(&step.origin)
                || matches!(
                    self.origin_policy.live_replay_mode_for_origin(&step.origin),
                    OriginReplayMode::ObserveOnly | OriginReplayMode::ValidateOnly
                )
                || step.side_effects == SideEffects::Unknown
            {
                bump_stats(version, false);
                return fall_back(
                    result,
                    &step,
                    FailureClass::PolicyBlocked,
                    "origin policy or unknown side effects block replay",
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            }

            let (mut params, mut session) = match resolve_step_context(
                &step,
                &flows,
                &prior,
                &inputs.inputs,
                &jar,
                self.session_lookup,
                require_legacy_cookies,
            ) {
                Ok(context) => context,
                Err((class, detail)) => {
                    // Missing or expired captured auth is recoverable before a
                    // browser task starts. Only reads are retried: a write may
                    // have reached the server even when its response is 401.
                    let healed = if class == FailureClass::Auth
                        && step.side_effects != SideEffects::Write
                        && heals_used < self.max_auth_heals
                    {
                        if let Some(healer) = self.auth_healer {
                            heals_used += 1;
                            let heal_remaining = deadline.saturating_duration_since(Instant::now());
                            if heal_remaining.is_zero() {
                                false
                            } else {
                                tokio::time::timeout(heal_remaining, healer.heal(&step.origin))
                                    .await
                                    .ok()
                                    .and_then(Result::ok)
                                    .unwrap_or(false)
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if healed {
                        result.auth_heals = result.auth_heals.saturating_add(1);
                        if let Some(observer) = self.observer {
                            observer.observe(RecipeEvent::AuthHealed {
                                origin: &step.origin,
                            });
                        }
                    }
                    if !healed {
                        bump_failure(version, class);
                        // Resolving credentials/dependencies has not sent this
                        // request. Missing auth must allow browser recovery
                        // unless an earlier write already crossed transport.
                        return fall_back_before_send(
                            result,
                            &step,
                            class,
                            &detail,
                            &prior,
                            &write_step_ids,
                            self.observer,
                        );
                    }
                    jar.forget_refreshed_origin(&step.origin);
                    match resolve_step_context(
                        &step,
                        &flows,
                        &prior,
                        &inputs.inputs,
                        &jar,
                        self.session_lookup,
                        require_legacy_cookies,
                    ) {
                        Ok(context) => context,
                        Err((retry_class, retry_detail)) => {
                            bump_failure(version, retry_class);
                            return fall_back_before_send(
                                result,
                                &step,
                                retry_class,
                                &retry_detail,
                                &prior,
                                &write_step_ids,
                                self.observer,
                            );
                        },
                    }
                },
            };

            if step.side_effects == SideEffects::Write
                && !write_has_current_verification_value(&step, &params)
            {
                bump_failure(version, FailureClass::InputMissing);
                return fall_back_before_send(
                    result,
                    &step,
                    FailureClass::InputMissing,
                    "recipe write has no non-empty dynamic value to verify",
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            }

            let mut request = match build_request(&step, &params, &session, remaining) {
                Ok(request) => request,
                Err(detail) => {
                    bump_stats(version, false);
                    return fall_back_before_send(
                        result,
                        &step,
                        FailureClass::SchemaDrift,
                        &detail,
                        &prior,
                        &write_step_ids,
                        self.observer,
                    );
                },
            };
            if let Err(detail) = apply_cookie_header(&mut request, &session) {
                bump_failure(version, FailureClass::Auth);
                return fall_back_before_send(
                    result,
                    &step,
                    FailureClass::Auth,
                    &detail,
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            }

            if self.transports.is_empty() {
                bump_stats(version, false);
                return fall_back(
                    result,
                    &step,
                    FailureClass::Network,
                    "recipe has no configured HTTP transport",
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            }

            let started = Instant::now();
            let mut transport_index = transport_start_index(&self.transports, &step);
            let mut auth_attempted = false;
            let response = loop {
                if self.can_continue.is_some_and(|allowed| !allowed())
                    || self.origin_policy.is_blocked(&step.origin)
                    || matches!(
                        self.origin_policy.live_replay_mode_for_origin(&step.origin),
                        OriginReplayMode::ObserveOnly | OriginReplayMode::ValidateOnly
                    )
                {
                    return fall_back_before_send(
                        result,
                        &step,
                        FailureClass::PolicyBlocked,
                        "replay was disabled before the next request",
                        &prior,
                        &write_step_ids,
                        self.observer,
                    );
                }
                if step.side_effects == SideEffects::Write
                    && !inputs.approved_write_steps.contains(&step.id)
                    && self
                        .grants
                        .lookup(&GrantKey {
                            recipe_id: Some(recipe_id.clone()),
                            step_id: Some(step.id.clone()),
                            capability_id: step.capability_id.clone(),
                            request_shape_fingerprint: step.effective_request_shape_fingerprint(),
                        })
                        .is_none()
                {
                    // A grant revoked while earlier reads were running is not
                    // authority for this write. Never fall through to a browser
                    // mutation after the user revoked that authority.
                    result.failure = Some(RecipeRunFailure {
                        step_id: step.id.clone(),
                        class: FailureClass::PolicyBlocked,
                        detail: "write replay grant was revoked before dispatch".into(),
                    });
                    return result;
                }
                let transport = &self.transports[transport_index];
                // Enforce the workflow deadline independently of transport
                // implementations, including browser eval and auth-heal time.
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bump_stats(version, false);
                    return fall_back_before_send(
                        result,
                        &step,
                        FailureClass::Network,
                        "recipe timeout budget exhausted before transport",
                        &prior,
                        &write_step_ids,
                        self.observer,
                    );
                }
                request.timeout = remaining;
                let sent = tokio::time::timeout(remaining, transport.send(&request))
                    .await
                    .unwrap_or_else(|_| Err("recipe transport deadline exceeded".into()));
                match sent {
                    Err(detail) => {
                        if step.side_effects != SideEffects::Write
                            && transport_index + 1 < self.transports.len()
                        {
                            let retry_remaining =
                                deadline.saturating_duration_since(Instant::now());
                            if retry_remaining.is_zero() {
                                bump_stats(version, false);
                                return fall_back(
                                    result,
                                    &step,
                                    FailureClass::Network,
                                    "recipe timeout budget exhausted during transport downgrade",
                                    &prior,
                                    &write_step_ids,
                                    self.observer,
                                );
                            }
                            request.timeout = retry_remaining;
                            transport_index += 1;
                            if let Some(observer) = self.observer {
                                observer.observe(RecipeEvent::TransportDowngraded {
                                    step_id: &step.id,
                                    to: self.transports[transport_index].kind(),
                                });
                            }
                            continue;
                        }
                        tracing::warn!(
                            recipe_id = %recipe_id,
                            step_id = %step.id,
                            transport = ?transport.kind(),
                            error_bytes = detail.len(),
                            "Task Recipe transport request failed"
                        );
                        bump_stats(version, false);
                        return fall_back(
                            result,
                            &step,
                            FailureClass::Network,
                            "transport request failed",
                            &prior,
                            &write_step_ids,
                            self.observer,
                        );
                    },
                    Ok(response) => {
                        // All transports share the same memory boundary. The
                        // reqwest implementation also stops reading at this
                        // limit, while in-page transports are checked here.
                        if response.body.len() > MAX_RECIPE_RESPONSE_BYTES
                            || !response_headers_within_limits(&response.headers)
                        {
                            bump_stats(version, false);
                            return fall_back(
                                result,
                                &step,
                                FailureClass::Network,
                                "transport response exceeded Task Recipe limits",
                                &prior,
                                &write_step_ids,
                                self.observer,
                            );
                        }
                        if classify_auth_stale(&response)
                            && step.side_effects != SideEffects::Write
                            && !auth_attempted
                        {
                            auth_attempted = true;
                            if let Some(healer) = self.auth_healer {
                                if heals_used < self.max_auth_heals {
                                    heals_used += 1;
                                    let heal_remaining =
                                        deadline.saturating_duration_since(Instant::now());
                                    let healed = if heal_remaining.is_zero() {
                                        false
                                    } else {
                                        tokio::time::timeout(
                                            heal_remaining,
                                            healer.heal(&step.origin),
                                        )
                                        .await
                                        .ok()
                                        .and_then(Result::ok)
                                        .unwrap_or(false)
                                    };
                                    if healed {
                                        result.auth_heals = result.auth_heals.saturating_add(1);
                                        if let Some(observer) = self.observer {
                                            observer.observe(RecipeEvent::AuthHealed {
                                                origin: &step.origin,
                                            });
                                        }
                                        jar.forget_refreshed_origin(&step.origin);
                                        let retry_remaining =
                                            deadline.saturating_duration_since(Instant::now());
                                        if !retry_remaining.is_zero() {
                                            if let Ok((refreshed_params, refreshed_session)) =
                                                resolve_step_context(
                                                    &step,
                                                    &flows,
                                                    &prior,
                                                    &inputs.inputs,
                                                    &jar,
                                                    self.session_lookup,
                                                    require_legacy_cookies,
                                                )
                                            {
                                                params = refreshed_params;
                                                session = refreshed_session;
                                                if let Ok(retry) = build_request(
                                                    &step,
                                                    &params,
                                                    &session,
                                                    retry_remaining,
                                                ) {
                                                    request = retry;
                                                    if apply_cookie_header(&mut request, &session)
                                                        .is_ok()
                                                    {
                                                        continue;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if classify_failure(&response) == Some(FailureClass::AntiBot)
                            && step.side_effects != SideEffects::Write
                            && transport_index + 1 < self.transports.len()
                        {
                            let retry_remaining =
                                deadline.saturating_duration_since(Instant::now());
                            if retry_remaining.is_zero() {
                                bump_stats(version, false);
                                return fall_back(
                                    result,
                                    &step,
                                    FailureClass::Network,
                                    "recipe timeout budget exhausted during transport downgrade",
                                    &prior,
                                    &write_step_ids,
                                    self.observer,
                                );
                            }
                            request.timeout = retry_remaining;
                            transport_index += 1;
                            if let Some(observer) = self.observer {
                                observer.observe(RecipeEvent::TransportDowngraded {
                                    step_id: &step.id,
                                    to: self.transports[transport_index].kind(),
                                });
                            }
                            continue;
                        }
                        break response;
                    },
                }
            };
            let duration_ms = started.elapsed().as_millis() as u64;
            let transport_kind = self.transports[transport_index].kind();

            result.steps.push(RecipeStepOutcome {
                step_id: step.id.clone(),
                method: step.method.clone(),
                url: redact_url(&step.url_template),
                status: Some(response.status),
                duration_ms,
                transport: transport_kind,
                // Raw response previews would create a second, less-governed
                // persistence path beside redacted traces and extracted
                // answers. Keep lifecycle outcomes structural.
                preview: None,
            });
            if let (Some(feedback), Some(capability_id)) =
                (self.step_feedback, step.capability_id.as_deref())
            {
                feedback(
                    &step.origin,
                    capability_id,
                    &step.url_template,
                    classify_failure(&response).is_none(),
                    classify_auth_stale(&response),
                    response.status,
                    &response.body,
                );
            }
            if let Some(class) = classify_failure(&response) {
                if class == FailureClass::AntiBot {
                    if let Some(stored_step) = version.steps.get_mut(index) {
                        stored_step.transport_hint = Some(next_transport_hint(transport_kind));
                    }
                }
                bump_failure(version, class);
                if step.side_effects == SideEffects::Write {
                    if let Some(observer) = self.observer {
                        observer.observe(RecipeEvent::StepFailed {
                            step_id: &step.id,
                            class,
                            detail: &format!("write returned HTTP {}", response.status),
                        });
                    }
                    result.failure = Some(RecipeRunFailure {
                        step_id: step.id.clone(),
                        class,
                        detail: format!("write returned HTTP {}", response.status),
                    });
                    return result;
                }
                return fall_back(
                    result,
                    &step,
                    class,
                    &format!("HTTP {}", response.status),
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            }

            let Some(next_response_bytes) =
                accumulate_response_budget(retained_response_bytes, response.body.len())
            else {
                bump_stats(version, false);
                return fall_back(
                    result,
                    &step,
                    FailureClass::SchemaDrift,
                    "recipe cumulative response budget exceeded",
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            };
            retained_response_bytes = next_response_bytes;

            if let Some(observer) = self.observer {
                observer.observe(RecipeEvent::StepCompleted {
                    step_id: &step.id,
                    status: response.status,
                    duration_ms,
                    transport: transport_kind,
                });
            }

            if let Some(stored_step) = version.steps.get_mut(index) {
                stored_step.transport_hint = Some(transport_kind);
            }

            if let Err(detail) = jar.absorb(&request.url, &response.headers) {
                bump_failure(version, FailureClass::Auth);
                return fall_back(
                    result,
                    &step,
                    FailureClass::Auth,
                    &detail,
                    &prior,
                    &write_step_ids,
                    self.observer,
                );
            }
            // Parse once and reuse for both exact post-write verification and
            // downstream extraction. JSON verification is scalar- and
            // type-aware; non-JSON responses use one bounded multi-pattern
            // scan instead of rescanning an 8 MiB body for every input.
            let response_json = serde_json::from_str(&response.body).ok();
            if must_verify.contains(&step.id) {
                let reflects = verify_expectations.get(&step.id).is_some_and(|writes| {
                    response_reflects_writes(&response.body, response_json.as_ref(), writes)
                });
                if !reflects {
                    verify_failed = true;
                }
            }
            if let Some(verify_step) = &step.verify_with {
                must_verify.insert(verify_step.clone());
                let expected_values: Vec<_> = step
                    .param_sources
                    .iter()
                    .filter(|(parameter, _)| verification_parameter(&step, parameter))
                    .filter_map(|(parameter, source)| match source {
                        RecipeParamSource::TaskInput { name } => Some(VerificationExpectation {
                            value: params.get(parameter)?.clone(),
                            schema: task_input_schemas.get(name).copied()?,
                        }),
                        RecipeParamSource::DataFlow { .. } => Some(VerificationExpectation {
                            value: params.get(parameter)?.clone(),
                            schema: step
                                .body_param_types
                                .get(parameter)
                                .copied()
                                .unwrap_or(TaskInputSchema::String),
                        }),
                        _ => None,
                    })
                    .filter(|expected| !expected.value.is_empty())
                    .collect();
                verify_expectations
                    .entry(verify_step.clone())
                    .or_default()
                    .push(expected_values);
            }
            prior.insert(
                step.id.clone(),
                StoredResponse {
                    json: response_json,
                    text: response.body,
                    headers: response.headers,
                },
            );
        }

        for field in &version.answer_spec {
            if let Some(value) = prior
                .get(&field.step_id)
                .and_then(|stored| extract_answer(&field.extractor, stored))
            {
                result.answer.insert(field.field.clone(), value);
            }
        }
        let answer_missing =
            !version.answer_spec.is_empty() && result.answer.len() < version.answer_spec.len();
        let verify_missing = must_verify
            .iter()
            .any(|step_id| !prior.contains_key(step_id));
        if verify_failed || verify_missing || answer_missing {
            let uncertain_write_step = result
                .steps
                .iter()
                .rev()
                .find(|outcome| write_step_ids.contains(&outcome.step_id))
                .map(|outcome| outcome.step_id.clone());
            let failed_step_id = uncertain_write_step.clone().unwrap_or_else(|| {
                version
                    .answer_spec
                    .first()
                    .map(|answer| answer.step_id.clone())
                    .unwrap_or_default()
            });
            let detail = if uncertain_write_step.is_some() {
                "a write was sent but the recipe outcome could not be verified"
            } else if verify_failed || verify_missing {
                "post-write verification read failed"
            } else {
                "one or more answer fields were not found in replayed responses"
            };
            if let Some(observer) = self.observer {
                observer.observe(RecipeEvent::StepFailed {
                    step_id: &failed_step_id,
                    class: FailureClass::SchemaDrift,
                    detail,
                });
            }
            result.failure = Some(RecipeRunFailure {
                step_id: failed_step_id,
                class: FailureClass::SchemaDrift,
                detail: detail.into(),
            });
            bump_stats(version, false);
            return result;
        }

        result.success = true;
        bump_stats(version, true);
        if let Some(observer) = self.observer {
            observer.observe(RecipeEvent::Completed {
                steps: result.steps.len(),
                duration_ms: run_started.elapsed().as_millis() as u64,
            });
        }
        result
    }
}

fn accumulate_response_budget(current: usize, next: usize) -> Option<usize> {
    current
        .checked_add(next)
        .filter(|total| *total <= MAX_RECIPE_TOTAL_RESPONSE_BYTES)
}

fn write_has_current_verification_value(
    step: &RecipeStep,
    params: &HashMap<String, String>,
) -> bool {
    step.param_sources.iter().any(|(parameter, source)| {
        verification_parameter(step, parameter)
            && matches!(
                source,
                RecipeParamSource::TaskInput { .. } | RecipeParamSource::DataFlow { .. }
            )
            && params.get(parameter).is_some_and(|value| !value.is_empty())
    })
}

fn verification_parameter(step: &RecipeStep, parameter: &str) -> bool {
    let placeholder = format!("{{{parameter}}}");
    step.url_template.contains(&placeholder)
        || step
            .body_template
            .as_ref()
            .is_some_and(|body| body.contains(&placeholder))
}

fn response_headers_within_limits(headers: &HashMap<String, String>) -> bool {
    if headers.len() > MAX_RECIPE_HEADERS {
        return false;
    }
    headers
        .iter()
        .try_fold(0usize, |total, (name, value)| {
            if name.len() > MAX_RECIPE_HEADER_BYTES || value.len() > MAX_RECIPE_HEADER_BYTES {
                return None;
            }
            total.checked_add(name.len())?.checked_add(value.len())
        })
        .is_some_and(|total| total <= MAX_RECIPE_RESPONSE_HEADER_BYTES)
}

fn collect_response_headers(
    source: &reqwest::header::HeaderMap,
) -> Result<HashMap<String, String>, String> {
    if source.len() > MAX_RECIPE_HEADERS {
        return Err("response headers exceeded the Task Recipe count limit".into());
    }
    let mut copied = HashMap::<String, String>::with_capacity(source.len());
    let mut total_bytes = 0usize;
    for (name, value) in source {
        let name = name.as_str();
        let value = value
            .to_str()
            .map_err(|_| "response contained a non-text header value".to_string())?;
        if name.len() > MAX_RECIPE_HEADER_BYTES || value.len() > MAX_RECIPE_HEADER_BYTES {
            return Err("response header exceeded the Task Recipe size limit".into());
        }
        total_bytes = total_bytes
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value.len()))
            .filter(|total| *total <= MAX_RECIPE_RESPONSE_HEADER_BYTES)
            .ok_or_else(|| "response headers exceeded the Task Recipe size limit".to_string())?;
        copied
            .entry(name.to_owned())
            .and_modify(|current| {
                current.push('\n');
                current.push_str(value);
            })
            .or_insert_with(|| value.to_owned());
    }
    Ok(copied)
}

fn response_reflects_writes(
    body: &str,
    json: Option<&serde_json::Value>,
    writes: &[Vec<VerificationExpectation>],
) -> bool {
    if writes.is_empty() || writes.iter().any(Vec::is_empty) {
        return false;
    }

    if let Some(json) = json {
        let Some(records) = index_json_records(json) else {
            // Fail closed instead of spending unbounded CPU on a pathological
            // verification document after a mutation.
            return false;
        };
        // Values from two different rows cannot jointly prove one mutation:
        // an unchanged target id plus another row's new label is not success.
        return writes.iter().all(|values| {
            let Some(values) = values
                .iter()
                .map(verification_value)
                .collect::<Option<Vec<_>>>()
            else {
                return false;
            };
            (0..records.len()).any(|record| {
                values.iter().all(|expected| {
                    let mut current = Some(record);
                    while let Some(index) = current {
                        if records[index].scalars.contains(expected) {
                            return true;
                        }
                        current = records[index].parent;
                    }
                    false
                })
            })
        });
    }

    let mut patterns: Vec<_> = writes
        .iter()
        .flat_map(|values| values.iter().map(|expected| expected.value.as_str()))
        .collect();
    patterns.sort_unstable();
    patterns.dedup();
    let Ok(matcher) = AhoCorasick::new(&patterns) else {
        return false;
    };
    let mut matched = HashSet::new();
    for (count, found) in matcher.find_overlapping_iter(body).enumerate() {
        if count >= MAX_VERIFICATION_TEXT_MATCHES {
            return false;
        }
        let pattern = patterns[found.pattern().as_usize()];
        if text_match_has_scalar_boundaries(body, found.start(), found.end(), pattern) {
            matched.insert(pattern);
            if matched.len() == patterns.len() {
                return true;
            }
        }
    }
    false
}

fn text_match_has_scalar_boundaries(body: &str, start: usize, end: usize, expected: &str) -> bool {
    let starts_word = expected.chars().next().is_some_and(char::is_alphanumeric);
    let ends_word = expected
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric);
    let left_ok = !starts_word
        || body[..start]
            .chars()
            .next_back()
            .is_none_or(|character| !character.is_alphanumeric());
    let right_ok = !ends_word
        || body[end..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_alphanumeric());
    left_ok && right_ok
}

fn index_json_records(value: &serde_json::Value) -> Option<Vec<JsonVerificationRecord<'_>>> {
    let mut records = vec![JsonVerificationRecord::default()];
    let mut pending = vec![(value, 0)];
    let mut visited = 0usize;
    while let Some((value, record)) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_VERIFICATION_JSON_NODES {
            return None;
        }
        match value {
            serde_json::Value::Object(object) => {
                if visited + pending.len() + object.len() > MAX_VERIFICATION_JSON_NODES {
                    return None;
                }
                for value in object.values() {
                    let record = if value.is_object() {
                        records.push(JsonVerificationRecord {
                            scalars: JsonScalarIndex::default(),
                            parent: Some(record),
                        });
                        records.len() - 1
                    } else {
                        record
                    };
                    pending.push((value, record));
                }
            },
            serde_json::Value::Array(array) => {
                if visited + pending.len() + array.len() > MAX_VERIFICATION_JSON_NODES {
                    return None;
                }
                for value in array {
                    // Object/array entries are independent records. Scalar
                    // arrays (e.g. tags) remain properties of their owner.
                    let record = if value.is_object() || value.is_array() {
                        records.push(JsonVerificationRecord::default());
                        records.len() - 1
                    } else {
                        record
                    };
                    pending.push((value, record));
                }
            },
            serde_json::Value::String(value) => {
                records[record].scalars.strings.insert(value.as_str());
            },
            serde_json::Value::Number(value) => {
                if let Some(key) = number_key(&value.to_string()) {
                    records[record].scalars.numbers.insert(key);
                }
            },
            serde_json::Value::Bool(value) => {
                records[record].scalars.booleans.insert(*value);
            },
            serde_json::Value::Null => {},
        }
    }
    Some(records)
}

impl JsonScalarIndex<'_> {
    fn contains(&self, expected: &VerificationValue<'_>) -> bool {
        match expected {
            VerificationValue::String(value) => self.strings.contains(value),
            VerificationValue::Number(value) => self.numbers.contains(value),
            VerificationValue::Boolean(value) => self.booleans.contains(value),
        }
    }
}

fn verification_value(expected: &VerificationExpectation) -> Option<VerificationValue<'_>> {
    match expected.schema {
        TaskInputSchema::String => Some(VerificationValue::String(&expected.value)),
        TaskInputSchema::Number => number_key(&expected.value).map(VerificationValue::Number),
        TaskInputSchema::Boolean if expected.value.eq_ignore_ascii_case("true") => {
            Some(VerificationValue::Boolean(true))
        },
        TaskInputSchema::Boolean if expected.value.eq_ignore_ascii_case("false") => {
            Some(VerificationValue::Boolean(false))
        },
        TaskInputSchema::Boolean => None,
    }
}

fn number_key(value: &str) -> Option<String> {
    // Normalize decimal notation without converting integral IDs to f64:
    // adjacent u64 values above 2^53 must never verify as the same id.
    let value = value.trim();
    let negative = value.starts_with('-');
    let value = value
        .strip_prefix('-')
        .or_else(|| value.strip_prefix('+'))
        .unwrap_or(value);
    let (mantissa, exponent) = match value.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (value, 0),
    };
    let (whole, fractional) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fractional.is_empty()
        || !whole
            .bytes()
            .chain(fractional.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let digits = format!("{whole}{fractional}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some("0".into());
    }
    let significant = digits.trim_end_matches('0');
    let exponent = exponent
        .checked_sub(i64::try_from(fractional.len()).ok()?)?
        .checked_add(i64::try_from(digits.len() - significant.len()).ok()?)?;
    Some(format!(
        "{}{significant}e{exponent}",
        if negative { "-" } else { "" }
    ))
}

fn transport_start_index(transports: &[Box<dyn StepTransport>], step: &RecipeStep) -> usize {
    step.transport_hint
        .and_then(|hint| {
            transports
                .iter()
                .position(|transport| transport.kind() == hint)
        })
        .unwrap_or(0)
}

fn next_transport_hint(current: Transport) -> Transport {
    match current {
        Transport::Reqwest => Transport::InPageFetch,
        Transport::InPageFetch => Transport::Browser,
        Transport::Browser => Transport::Browser,
    }
}

/// Validate every caller-controlled task input before the first request. This
/// prevents a later malformed/missing value from producing a partially
/// executed multi-step workflow, and bounds input work on every replay rail.
fn validate_run_inputs(
    recipe: &TaskRecipe,
    inputs: &RecipeRunInputs,
) -> Result<(), (FailureClass, String)> {
    if recipe.shape.inputs.len() > MAX_RECIPE_INPUTS {
        return Err((
            FailureClass::SchemaDrift,
            format!("recipe declares more than {MAX_RECIPE_INPUTS} task inputs"),
        ));
    }
    if inputs.inputs.len() > MAX_RECIPE_INPUTS {
        return Err((
            FailureClass::InputMissing,
            format!("recipe replay accepts at most {MAX_RECIPE_INPUTS} task inputs"),
        ));
    }

    let mut expected = HashMap::with_capacity(recipe.shape.inputs.len());
    for input in &recipe.shape.inputs {
        if input.name.is_empty()
            || input.name.len() > 128
            || input.name.chars().any(char::is_control)
            || expected.insert(input.name.as_str(), input.schema).is_some()
        {
            return Err((
                FailureClass::SchemaDrift,
                "recipe contains an empty or duplicate task input name".into(),
            ));
        }
    }
    if recipe.shape.template.len() > 16 * 1024
        || recipe
            .shape
            .description_template
            .as_ref()
            .is_some_and(|value| value.len() > 16 * 1024)
    {
        return Err((
            FailureClass::SchemaDrift,
            "recipe task template exceeds the Task Recipe limit".into(),
        ));
    }
    let Some(template_expression) =
        super::recipe_compiler::shape::template_regex(&recipe.shape.template)
    else {
        return Err((
            FailureClass::SchemaDrift,
            "recipe contains an invalid task template".into(),
        ));
    };
    let captured_inputs: HashSet<_> = template_expression.capture_names().flatten().collect();
    if captured_inputs
        .iter()
        .any(|name| !expected.contains_key(*name))
    {
        return Err((
            FailureClass::SchemaDrift,
            "recipe task template and declared inputs do not agree".into(),
        ));
    }
    if let Some(description) = recipe.shape.description_template.as_deref() {
        let Some(expression) = super::recipe_compiler::shape::template_regex(description) else {
            return Err((
                FailureClass::SchemaDrift,
                "recipe contains an invalid description template".into(),
            ));
        };
        if expression
            .capture_names()
            .flatten()
            .any(|name| !expected.contains_key(name))
        {
            return Err((
                FailureClass::SchemaDrift,
                "recipe description contains an undeclared input".into(),
            ));
        }
    }

    if inputs
        .inputs
        .keys()
        .any(|name| !expected.contains_key(name.as_str()))
    {
        return Err((
            FailureClass::InputMissing,
            "request supplied an unknown recipe input".into(),
        ));
    }

    for input in &recipe.shape.inputs {
        let Some(value) = inputs.inputs.get(&input.name) else {
            return Err((
                FailureClass::InputMissing,
                format!("missing recipe input: {}", input.name),
            ));
        };
        if value.len() > MAX_RECIPE_INPUT_VALUE_BYTES {
            return Err((
                FailureClass::InputMissing,
                format!("recipe input is too large: {}", input.name),
            ));
        }
        if !input.schema.accepts(value) {
            return Err((
                FailureClass::InputMissing,
                format!("recipe input has the wrong scalar type: {}", input.name),
            ));
        }
    }

    if let Some(version) = recipe.current() {
        let mut input_uses = HashMap::<&str, usize>::new();
        for step in &version.steps {
            for (parameter, source) in &step.param_sources {
                if let RecipeParamSource::TaskInput { name } = source {
                    *input_uses.entry(name.as_str()).or_default() += 1;
                    if !expected.contains_key(name.as_str()) {
                        return Err((
                            FailureClass::SchemaDrift,
                            format!(
                                "recipe step {} references an undeclared task input",
                                step.id
                            ),
                        ));
                    }
                    if step
                        .body_param_types
                        .get(parameter)
                        .is_some_and(|schema| !schema.accepts(&inputs.inputs[name]))
                    {
                        return Err((
                            FailureClass::InputMissing,
                            format!(
                                "recipe input cannot render the body parameter in step {}",
                                step.id
                            ),
                        ));
                    }
                }
            }
            validate_known_request_inputs(step, &inputs.inputs)?;
            if step.side_effects == SideEffects::Write && step.verify_with.is_some() {
                let has_data_flow = step.param_sources.iter().any(|(parameter, source)| {
                    verification_parameter(step, parameter)
                        && matches!(source, RecipeParamSource::DataFlow { .. })
                });
                let task_inputs: Vec<_> = step
                    .param_sources
                    .iter()
                    .filter(|(parameter, _)| verification_parameter(step, parameter))
                    .filter_map(|(_, source)| match source {
                        RecipeParamSource::TaskInput { name } => Some(name),
                        _ => None,
                    })
                    .collect();
                if !has_data_flow && task_inputs.is_empty() {
                    return Err((
                        FailureClass::SchemaDrift,
                        format!(
                            "recipe write step {} has no verifiable dynamic request value",
                            step.id
                        ),
                    ));
                }
                if !has_data_flow
                    && !task_inputs.iter().any(|name| {
                        inputs
                            .inputs
                            .get(*name)
                            .is_some_and(|value| !value.is_empty())
                    })
                {
                    return Err((
                        FailureClass::InputMissing,
                        format!(
                            "recipe write step {} has no current verification value",
                            step.id
                        ),
                    ));
                }
            }
        }
        // Older recipes replaced only the first occurrence of an example in
        // task text, despite sharing that input across request parameters.
        // Reject edited values before any HTTP call on every replay surface;
        // otherwise "compare ETH with BTC" could issue two ETH requests.
        for input in &recipe.shape.inputs {
            if input_uses.get(input.name.as_str()).copied().unwrap_or(0) > 1
                && inputs.inputs[&input.name] != input.example_value
                && std::iter::once(recipe.shape.template.as_str())
                    .chain(recipe.shape.description_template.as_deref())
                    .any(|template| {
                        super::recipe_compiler::shape::template_has_literal_value(
                            template,
                            &input.example_value,
                        )
                    })
            {
                return Err((
                    FailureClass::InputMissing,
                    "recipe has ambiguous shared inputs; recompile before changing them".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Shared preflight for every replay surface. Task-start callers use this
/// before opening HITL so a corrupt recipe cannot ask for an approval it is
/// structurally incapable of executing safely.
pub fn validate_recipe_preflight(
    recipe: &TaskRecipe,
    inputs: &RecipeRunInputs,
) -> Result<(), (FailureClass, String)> {
    validate_recipe_structure(recipe).and_then(|()| validate_run_inputs(recipe, inputs))
}

/// Validate all graph references and origin bindings before replay starts.
/// Stored recipes are durable input: this prevents corruption or a stale
/// compiler artifact from turning into a cross-origin request or a workflow
/// that fails only after earlier steps have already run.
fn validate_recipe_structure(recipe: &TaskRecipe) -> Result<(), (FailureClass, String)> {
    let Some(version) = recipe.current() else {
        return Ok(());
    };
    if version.steps.len() > MAX_RECIPE_STEPS
        || version.data_flows.len() > MAX_RECIPE_DATA_FLOWS
        || version.answer_spec.len() > MAX_RECIPE_ANSWER_FIELDS
        || version.origins.len() > MAX_RECIPE_STEPS
    {
        return Err((
            FailureClass::SchemaDrift,
            "recipe graph exceeds Task Recipe cardinality limits".into(),
        ));
    }
    let mut step_positions = HashMap::with_capacity(version.steps.len());
    for (position, step) in version.steps.iter().enumerate() {
        if step.id.is_empty()
            || step.id.len() > 128
            || step.id.chars().any(char::is_control)
            || step_positions.insert(step.id.as_str(), position).is_some()
        {
            return Err((
                FailureClass::SchemaDrift,
                "recipe contains an empty or duplicate step id".into(),
            ));
        }
        if step.url_template.len() > MAX_RECIPE_URL_BYTES
            || step.headers_template.len() > MAX_RECIPE_HEADERS
            || step.param_sources.len() > MAX_RECIPE_STEP_PARAMS
            || step.body_param_types.len() > MAX_RECIPE_STEP_PARAMS
            || step.headers_template.iter().any(|(name, value)| {
                name.len() > MAX_RECIPE_HEADER_BYTES || value.len() > MAX_RECIPE_HEADER_BYTES
            })
            || step
                .body_template
                .as_ref()
                .is_some_and(|body| body.len() > MAX_RECIPE_BODY_BYTES)
        {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe step {} exceeds Task Recipe size limits", step.id),
            ));
        }
        let mut normalized_header_names = HashSet::with_capacity(step.headers_template.len());
        if step.headers_template.iter().any(|(name, value)| {
            let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
                return true;
            };
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()).is_err()
                || !normalized_header_names.insert(name.as_str().to_owned())
        }) {
            return Err((
                FailureClass::SchemaDrift,
                format!(
                    "recipe step {} contains an invalid or duplicate header",
                    step.id
                ),
            ));
        }
        for (name, source) in &step.param_sources {
            if matches!(source, RecipeParamSource::Literal { volatile: true, .. }) {
                // This dependency is known to be unresolved before execution.
                // Discovering it after an earlier write would needlessly leave
                // an approved workflow partially applied with no safe retry.
                return Err((
                    FailureClass::InputMissing,
                    format!(
                        "recipe step {} has an unresolved volatile parameter {name}",
                        step.id
                    ),
                ));
            }
            if name.is_empty()
                || name.len() > 128
                || name.chars().any(char::is_control)
                || name.contains(['{', '}'])
                || matches!(source, RecipeParamSource::Literal { value, .. } if value.len() > MAX_RECIPE_INPUT_VALUE_BYTES)
                || matches!(source, RecipeParamSource::SessionAuth { scheme } if scheme.is_empty() || scheme.len() > 256 || scheme.chars().any(char::is_control))
            {
                return Err((
                    FailureClass::SchemaDrift,
                    format!("recipe step {} contains an unreplayable parameter", step.id),
                ));
            }
            if !matches!(
                source,
                RecipeParamSource::SessionAuth { .. }
                    | RecipeParamSource::Literal {
                        volatile: false,
                        ..
                    }
            ) && !template_uses_parameter(step, name)
            {
                return Err((
                    FailureClass::SchemaDrift,
                    format!("recipe step {} contains an unused parameter", step.id),
                ));
            }
        }
        if step
            .body_param_types
            .keys()
            .any(|name| !step.param_sources.contains_key(name))
        {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe step {} has an unbound body parameter type", step.id),
            ));
        }
        let target = url::Url::parse(&step.url_template).map_err(|_| {
            (
                FailureClass::SchemaDrift,
                format!("recipe step {} has an invalid URL template", step.id),
            )
        })?;
        let declared = url::Url::parse(&step.origin).map_err(|_| {
            (
                FailureClass::SchemaDrift,
                format!("recipe step {} has an invalid declared origin", step.id),
            )
        })?;
        let canonical_origin = declared.origin().ascii_serialization();
        if !matches!(target.scheme(), "http" | "https")
            || target.origin() != declared.origin()
            || !target.username().is_empty()
            || target.password().is_some()
            || !declared.username().is_empty()
            || declared.password().is_some()
            || step.origin.trim_end_matches('/') != canonical_origin
        {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe step {} does not match its declared origin", step.id),
            ));
        }
        if step.method.is_empty()
            || step.method.len() > 32
            || step.method != step.method.to_ascii_uppercase()
            || reqwest::Method::from_bytes(step.method.as_bytes()).is_err()
        {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe step {} has an invalid HTTP method", step.id),
            ));
        }
        let content_type = step
            .headers_template
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.as_str());
        if request_body_format(content_type) == RequestBodyFormat::Json
            && step.body_template.as_deref().is_some_and(|body| {
                !body.trim().is_empty() && serde_json::from_str::<serde_json::Value>(body).is_err()
            })
        {
            return Err((
                FailureClass::SchemaDrift,
                format!(
                    "recipe step {} has an invalid JSON body for its Content-Type",
                    step.id
                ),
            ));
        }
        let graphql_kind = super::miner::detect_graphql_request_info_standalone(
            step.body_template.as_deref(),
            content_type.or(Some("application/json")),
        )
        .map(|info| info.operation_kind);
        let inferred_side_effects = classify_side_effects_for_request(
            &step.method,
            &step.url_template,
            step.body_template.as_deref(),
            graphql_kind,
        );
        if step.side_effects == SideEffects::ReadOnly
            && inferred_side_effects != SideEffects::ReadOnly
        {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe step {} is not structurally read-only", step.id),
            ));
        }
    }

    let declared_origins: HashSet<_> = version.origins.iter().map(String::as_str).collect();
    let step_origins: HashSet<_> = version
        .steps
        .iter()
        .map(|step| step.origin.as_str())
        .collect();
    if declared_origins.len() != version.origins.len() || declared_origins != step_origins {
        return Err((
            FailureClass::SchemaDrift,
            "recipe origin metadata does not match its executable steps".into(),
        ));
    }

    let mut flows = HashMap::with_capacity(version.data_flows.len());
    for flow in &version.data_flows {
        if flow.id.is_empty()
            || flow.id.len() > 128
            || flow.target_param.is_empty()
            || flow.target_param.len() > 128
            || flows.insert(flow.id.as_str(), flow).is_some()
        {
            return Err((
                FailureClass::SchemaDrift,
                "recipe contains an empty or duplicate data-flow id".into(),
            ));
        }
        let Some(source_position) = step_positions.get(flow.source_step.as_str()) else {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe data flow {} has an unknown source step", flow.id),
            ));
        };
        let Some(target_position) = step_positions.get(flow.target_step.as_str()) else {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe data flow {} has an unknown target step", flow.id),
            ));
        };
        if source_position >= target_position
            || !flow.confidence.is_finite()
            || !(0.0..=1.0).contains(&flow.confidence)
            || !valid_extractor(&flow.extractor)
        {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe data flow {} is not a forward edge", flow.id),
            ));
        }
    }

    for (position, step) in version.steps.iter().enumerate() {
        if step.side_effects == SideEffects::Write && step.verify_with.is_none() {
            return Err((
                FailureClass::SchemaDrift,
                format!("recipe write step {} has no verification read", step.id),
            ));
        }
        if let Some(verify_id) = step.verify_with.as_deref() {
            if step.side_effects != SideEffects::Write {
                return Err((
                    FailureClass::SchemaDrift,
                    format!(
                        "recipe read step {} contains a write-verification edge",
                        step.id
                    ),
                ));
            }
            let Some(verify_position) = step_positions.get(verify_id) else {
                return Err((
                    FailureClass::SchemaDrift,
                    format!("recipe step {} has an unknown verification step", step.id),
                ));
            };
            if *verify_position <= position
                || version.steps[*verify_position].side_effects != SideEffects::ReadOnly
            {
                return Err((
                    FailureClass::SchemaDrift,
                    format!("recipe step {} has an unsafe verification edge", step.id),
                ));
            }
        }
        for (parameter, source) in &step.param_sources {
            let RecipeParamSource::DataFlow { flow_id } = source else {
                continue;
            };
            let Some(flow) = flows.get(flow_id.as_str()) else {
                return Err((
                    FailureClass::SchemaDrift,
                    format!("recipe step {} references an unknown data flow", step.id),
                ));
            };
            if flow.target_step != step.id || flow.target_param != *parameter {
                return Err((
                    FailureClass::SchemaDrift,
                    format!("recipe step {} has a misbound data flow", step.id),
                ));
            }
        }
    }

    let mut answer_fields = HashSet::with_capacity(version.answer_spec.len());
    for answer in &version.answer_spec {
        if answer.field.is_empty()
            || answer.field.len() > 128
            || answer.field.chars().any(char::is_control)
            || !answer_fields.insert(answer.field.as_str())
            || !step_positions.contains_key(answer.step_id.as_str())
            || !valid_extractor(&answer.extractor)
        {
            return Err((
                FailureClass::SchemaDrift,
                "recipe contains an invalid answer specification".into(),
            ));
        }
    }
    Ok(())
}

fn template_uses_parameter(step: &RecipeStep, name: &str) -> bool {
    let placeholder = format!("{{{name}}}");
    step.url_template.contains(&placeholder)
        || step
            .headers_template
            .values()
            .any(|value| value.contains(&placeholder))
        || step
            .body_template
            .as_deref()
            .is_some_and(|body| body.contains(&placeholder))
}

fn valid_extractor(extractor: &Extractor) -> bool {
    match extractor {
        Extractor::JsonPath { path } => {
            path.len() <= 256 && super::workflow_replay::jsonpath::is_supported_jsonpath(path)
        },
        Extractor::Regex { pattern, group } => {
            pattern.len() <= 4_096
                && regex::Regex::new(pattern)
                    .ok()
                    .is_some_and(|expression| *group < expression.captures_len())
        },
        Extractor::Header { name } | Extractor::Cookie { name } => {
            !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
        },
    }
}

fn fall_back(
    result: RecipeRunResult,
    step: &RecipeStep,
    class: FailureClass,
    detail: &str,
    prior: &HashMap<String, StoredResponse>,
    write_step_ids: &HashSet<String>,
    observer: Option<&dyn RunObserver>,
) -> RecipeRunResult {
    fall_back_with_write_state(
        result,
        step,
        class,
        detail,
        prior,
        write_step_ids,
        true,
        observer,
    )
}

/// Fail before a request is handed to a transport. The current write is safe
/// to hand off, but a write already represented in `result.steps` is not.
fn fall_back_before_send(
    result: RecipeRunResult,
    step: &RecipeStep,
    class: FailureClass,
    detail: &str,
    prior: &HashMap<String, StoredResponse>,
    write_step_ids: &HashSet<String>,
    observer: Option<&dyn RunObserver>,
) -> RecipeRunResult {
    fall_back_with_write_state(
        result,
        step,
        class,
        detail,
        prior,
        write_step_ids,
        false,
        observer,
    )
}

fn fall_back_with_write_state(
    mut result: RecipeRunResult,
    step: &RecipeStep,
    class: FailureClass,
    detail: &str,
    prior: &HashMap<String, StoredResponse>,
    write_step_ids: &HashSet<String>,
    current_write_may_have_been_sent: bool,
    observer: Option<&dyn RunObserver>,
) -> RecipeRunResult {
    let uncertain_write_step = (current_write_may_have_been_sent
        && step.side_effects == SideEffects::Write)
        .then(|| step.id.clone())
        .or_else(|| {
            result
                .steps
                .iter()
                .rev()
                .find(|outcome| write_step_ids.contains(&outcome.step_id))
                .map(|outcome| outcome.step_id.clone())
        });
    if let Some(write_step_id) = uncertain_write_step {
        let terminal_detail = if write_step_id == step.id {
            detail.to_owned()
        } else {
            format!(
                "write outcome could not be completed safely because downstream step {} failed: {detail}",
                step.id
            )
        };
        result.fallback = None;
        result.failure = Some(RecipeRunFailure {
            step_id: write_step_id.clone(),
            class,
            detail: terminal_detail.clone(),
        });
        if let Some(observer) = observer {
            observer.observe(RecipeEvent::StepFailed {
                step_id: &write_step_id,
                class,
                detail: &terminal_detail,
            });
        }
        return result;
    }
    let replayed: Vec<_> = result
        .steps
        .iter()
        .filter_map(|outcome| {
            prior
                .get(&outcome.step_id)
                .map(|stored| (outcome.step_id.clone(), safe_replayed_summary(stored)))
        })
        .collect();
    let replayed_steps = replayed.len();
    result.fallback = Some(RecipeFallback {
        step_id: step.id.clone(),
        class,
        detail: detail.to_owned(),
        browser: step.browser_fallback.clone(),
        replayed,
    });
    if let Some(observer) = observer {
        observer.observe(RecipeEvent::StepFailed {
            step_id: &step.id,
            class,
            detail,
        });
        observer.observe(RecipeEvent::FallbackHandoff {
            step_id: &step.id,
            class,
            replayed_steps,
        });
    }
    result
}

/// Tell the browser continuation which reads completed without copying raw
/// response values into a model prompt or run ledger. Values may contain
/// access tokens, signed URLs, or private account data even when the source
/// trace was correctly redacted.
fn safe_replayed_summary(response: &StoredResponse) -> String {
    match response.json.as_ref() {
        // Even object keys may be user-controlled identifiers. Counts retain
        // enough shape for a browser handoff without creating a side channel
        // for account names, tokens, or private resource IDs.
        Some(serde_json::Value::Object(map)) => {
            format!("JSON object response received ({} field(s))", map.len())
        },
        Some(serde_json::Value::Array(items)) => {
            format!("JSON array response received ({} item(s))", items.len())
        },
        Some(_) => "JSON scalar response received".into(),
        None => format!("HTTP response received ({} bytes)", response.text.len()),
    }
}

fn bump_stats(version: &mut RecipeVersion, success: bool) {
    if success {
        version.replay_stats.successful_replays =
            version.replay_stats.successful_replays.saturating_add(1);
    } else {
        version.replay_stats.failed_replays = version.replay_stats.failed_replays.saturating_add(1);
    }
    version.last_replayed_at_ms = Some(chrono::Utc::now().timestamp_millis());
    let total = version
        .replay_stats
        .successful_replays
        .saturating_add(version.replay_stats.failed_replays);
    let failure_rate = if total == 0 {
        0.0
    } else {
        version.replay_stats.failed_replays as f32 / total as f32
    };
    version.maturity = match (version.replay_stats.successful_replays, failure_rate) {
        (successes, rate) if successes >= 10 && rate < 0.10 => RecipeMaturity::Trusted,
        (successes, _) if successes >= 3 => RecipeMaturity::Validated,
        (successes, _) if successes >= 1 => RecipeMaturity::Candidate,
        _ => RecipeMaturity::Draft,
    };
}

/// Authentication state and caller input quality do not measure the learned
/// request shape. They remain visible through run ledgers and failure-class
/// metrics, but must not poison the recipe's later success/failure ratio.
fn bump_failure(version: &mut RecipeVersion, class: FailureClass) {
    if matches!(class, FailureClass::Auth | FailureClass::InputMissing) {
        version.last_replayed_at_ms = Some(chrono::Utc::now().timestamp_millis());
    } else {
        bump_stats(version, false);
    }
}

fn resolve_step_context(
    step: &RecipeStep,
    flows: &HashMap<String, RecipeDataFlow>,
    prior: &HashMap<String, StoredResponse>,
    inputs: &HashMap<String, String>,
    jar: &CookieJar,
    lookup: &(dyn Fn(&str, &str) -> Option<SessionContext> + Send + Sync),
    require_legacy_cookies: bool,
) -> Result<(HashMap<String, String>, SessionContext), (FailureClass, String)> {
    let mut session = lookup(&step.origin, &step.url_template).unwrap_or_default();
    jar.merge_session(&mut session, &step.url_template)
        .map_err(|detail| (FailureClass::Auth, detail))?;
    // Resolve only the URL first. A cookie-backed header/body may require a
    // path-specific cookie that the unresolved URL cannot possibly select.
    let url_params = resolve_params(step, flows, prior, inputs, &session, true)?;
    let concrete_url = fill_url_template(&step.url_template, &url_params)
        .map_err(|detail| (FailureClass::SchemaDrift, detail))?;
    if concrete_url.len() > MAX_RECIPE_URL_BYTES {
        return Err((
            FailureClass::SchemaDrift,
            "resolved URL exceeds the Task Recipe limit".into(),
        ));
    }
    if concrete_url != step.url_template {
        session = lookup(&step.origin, &concrete_url).unwrap_or_default();
        jar.merge_session(&mut session, &concrete_url)
            .map_err(|detail| (FailureClass::Auth, detail))?;
    }
    let mut params = resolve_params(step, flows, prior, inputs, &session, false)?;
    if require_legacy_cookies && session.cookie_header_values.is_empty() {
        return Err((
            FailureClass::Auth,
            "cookie-dependent recipe has no current session cookies".into(),
        ));
    }
    // A two-pass lookup must not advance a URL timestamp between cookie
    // selection and request construction.
    for (name, value) in url_params {
        if matches!(
            step.param_sources.get(&name),
            Some(RecipeParamSource::Now { .. })
        ) {
            params.insert(name, value);
        }
    }
    if fill_url_template(&step.url_template, &params)
        .map_err(|detail| (FailureClass::SchemaDrift, detail))?
        != concrete_url
    {
        return Err((
            FailureClass::Auth,
            "URL-scoped session values changed the request target".into(),
        ));
    }
    Ok((params, session))
}

fn resolve_params(
    step: &RecipeStep,
    flows: &HashMap<String, RecipeDataFlow>,
    prior: &HashMap<String, StoredResponse>,
    inputs: &HashMap<String, String>,
    session: &SessionContext,
    url_only: bool,
) -> Result<HashMap<String, String>, (FailureClass, String)> {
    let mut output = HashMap::with_capacity(step.param_sources.len());
    for (name, source) in &step.param_sources {
        if url_only && !step.url_template.contains(&format!("{{{name}}}")) {
            continue;
        }
        let value = match source {
            RecipeParamSource::Literal { volatile: true, .. } => {
                return Err((
                    FailureClass::InputMissing,
                    format!("volatile parameter {name} was not resolved"),
                ));
            },
            RecipeParamSource::Literal { value, .. } => value.clone(),
            RecipeParamSource::TaskInput { name } => inputs.get(name).cloned().ok_or((
                FailureClass::InputMissing,
                format!("input {name} not provided"),
            ))?,
            RecipeParamSource::DataFlow { flow_id } => {
                let flow = flows
                    .get(flow_id)
                    .ok_or((FailureClass::SchemaDrift, format!("unknown flow {flow_id}")))?;
                let stored = prior.get(&flow.source_step).ok_or((
                    FailureClass::SchemaDrift,
                    format!("source step {} has no response", flow.source_step),
                ))?;
                extract(&flow.extractor, stored).ok_or((
                    FailureClass::SchemaDrift,
                    format!("{:?} missed in step {}", flow.extractor, flow.source_step),
                ))?
            },
            RecipeParamSource::SessionAuth { scheme } => if scheme == "cookie_header" {
                session
                    .cookie_header_string()
                    .filter(|value| !value.is_empty())
            } else if let Some(query_name) = scheme.strip_prefix("query:") {
                session
                    .auth_query_params
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(query_name))
                    .map(|(_, value)| value.clone())
            } else if let Some(cookie_name) = scheme.strip_prefix("cookie:") {
                session
                    .cookie_header_values
                    .iter()
                    .find(|cookie| cookie.name == cookie_name)
                    .map(|cookie| cookie.value.clone())
                    .or_else(|| session.cookies.get(cookie_name).cloned())
            } else if let Some(name) = scheme.strip_prefix("local_storage:") {
                session.local_storage.get(name).cloned()
            } else if let Some(name) = scheme.strip_prefix("session_storage:") {
                session.session_storage.get(name).cloned()
            } else if let Some(body_name) = scheme.strip_prefix("body:") {
                session
                    .auth_query_params
                    .iter()
                    .chain(&session.auth_headers)
                    .chain(&session.local_storage)
                    .chain(&session.session_storage)
                    .find(|(name, _)| name.eq_ignore_ascii_case(body_name))
                    .map(|(_, value)| value.clone())
            } else {
                session
                    .auth_headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(scheme))
                    .map(|(_, value)| value.clone())
                    .or_else(|| session.local_storage.get(scheme).cloned())
                    .or_else(|| session.session_storage.get(scheme).cloned())
            }
            .ok_or((
                FailureClass::Auth,
                format!("no captured session value for {scheme}"),
            ))?,
            RecipeParamSource::Now { unit } => match unit {
                NowUnit::Millis => chrono::Utc::now().timestamp_millis().to_string(),
                NowUnit::Seconds => chrono::Utc::now().timestamp().to_string(),
            },
        };
        output.insert(name.clone(), value);
    }
    Ok(output)
}

fn extract(extractor: &Extractor, stored: &StoredResponse) -> Option<String> {
    let value = match extractor {
        Extractor::JsonPath { path } => stored
            .json
            .as_ref()
            .and_then(|json| extract_jsonpath(json, path)),
        Extractor::Regex { pattern, group } => {
            extract_unique_regex(&stored.text, pattern, *group).map(str::to_owned)
        },
        Extractor::Header { name } => stored
            .headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone()),
        Extractor::Cookie { name } => stored.headers.get("set-cookie").and_then(|header| {
            header.lines().find_map(|cookie| {
                let (cookie_name, value) = cookie.trim().split_once('=')?;
                (cookie_name.trim() == name).then(|| {
                    value
                        .split(';')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_owned()
                })
            })
        }),
    }?;
    (value.len() <= MAX_EXTRACTED_VALUE_BYTES).then_some(value)
}

fn extract_answer(extractor: &Extractor, stored: &StoredResponse) -> Option<serde_json::Value> {
    let value = match extractor {
        Extractor::JsonPath { path } => stored.json.as_ref().and_then(|json| {
            super::workflow_replay::jsonpath::extract_jsonpath_value(json, path).cloned()
        }),
        _ => extract(extractor, stored).map(serde_json::Value::String),
    }?;
    (serde_json::to_string(&value).ok()?.len() <= MAX_EXTRACTED_VALUE_BYTES).then_some(value)
}

pub fn fill_template(template: &str, params: &HashMap<String, String>) -> Result<String, String> {
    fill_template_with(template, params, MAX_RECIPE_BODY_BYTES, str::to_owned)
}

fn fill_url_template<V: AsRef<str>>(
    template: &str,
    params: &HashMap<String, V>,
) -> Result<String, String> {
    fill_template_with(template, params, MAX_RECIPE_URL_BYTES, |value| {
        urlencoding::encode(value).into_owned()
    })
}

fn fill_template_with<V: AsRef<str>>(
    template: &str,
    params: &HashMap<String, V>,
    max_bytes: usize,
    encode: impl Fn(&str) -> String,
) -> Result<String, String> {
    fn append(output: &mut String, part: &str, max_bytes: usize) -> Result<(), String> {
        if part.len() > max_bytes.saturating_sub(output.len()) {
            return Err("resolved template exceeds the Task Recipe limit".into());
        }
        output.push_str(part);
        Ok(())
    }
    let mut output = String::with_capacity(template.len().min(max_bytes));
    let mut literal_start = 0;
    let mut open = None;
    // Scan only the original template, never substituted values. Repeated
    // global replace rewrites caller data containing another slot and can
    // amplify memory in HashMap-dependent order. Nested literal braces (JSON,
    // GraphQL, etc.) must not hide genuine slots inside the literal structure.
    for (offset, character) in template.char_indices() {
        match character {
            '{' => open = Some(offset),
            '}' => {
                if let Some(start) = open.take() {
                    if let Some(value) = params.get(&template[start + 1..offset]) {
                        append(&mut output, &template[literal_start..start], max_bytes)?;
                        append(&mut output, &encode(value.as_ref()), max_bytes)?;
                        literal_start = offset + 1;
                    }
                }
            },
            _ => {},
        }
    }
    // Replay supplies every resolved source; known-input preflight supplies
    // only task inputs. Preserve other braces in either case, including
    // stable JSON/GraphQL text and dependencies still awaiting resolution.
    append(&mut output, &template[literal_start..], max_bytes)?;
    Ok(output)
}

/// Check the caller-controlled portion of URL/header rendering across the
/// whole graph. No session lookup or response dependency is evaluated here;
/// unresolved slots remain literal until execution. Known invalid header
/// bytes/expansion must not be discovered only after an earlier mutation.
fn validate_known_request_inputs(
    step: &RecipeStep,
    inputs: &HashMap<String, String>,
) -> Result<(), (FailureClass, String)> {
    let known: HashMap<_, _> = step
        .param_sources
        .iter()
        .filter_map(|(parameter, source)| match source {
            RecipeParamSource::TaskInput { name } => inputs
                .get(name)
                .map(|value| (parameter.clone(), value.as_str())),
            _ => None,
        })
        .collect();
    if known.is_empty() {
        return Ok(());
    }
    let invalid = || {
        (
            FailureClass::InputMissing,
            format!("recipe input cannot render the request in step {}", step.id),
        )
    };
    fill_url_template(&step.url_template, &known).map_err(|_| invalid())?;
    for template in step.headers_template.values() {
        let value = fill_template_with(template, &known, MAX_RECIPE_HEADER_BYTES, str::to_owned)
            .map_err(|_| invalid())?;
        reqwest::header::HeaderValue::from_bytes(value.as_bytes()).map_err(|_| invalid())?;
    }
    Ok(())
}

fn build_request(
    step: &RecipeStep,
    params: &HashMap<String, String>,
    session: &SessionContext,
    timeout: Duration,
) -> Result<TransportRequest, String> {
    let url = fill_url_template(&step.url_template, params)?;
    if url.len() > MAX_RECIPE_URL_BYTES {
        return Err("resolved URL exceeds the Task Recipe limit".into());
    }
    url::Url::parse(&url).map_err(|error| format!("invalid resolved URL: {error}"))?;
    let mut headers = HashMap::new();
    for (name, value) in &step.headers_template {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("invalid recipe header name {name:?}"))?;
        let value = fill_template_with(value, params, MAX_RECIPE_HEADER_BYTES, str::to_owned)?;
        reqwest::header::HeaderValue::from_bytes(value.as_bytes())
            .map_err(|_| format!("invalid resolved value for recipe header {name}"))?;
        headers.insert(name.as_str().to_owned(), value);
    }
    let mut session_header_names = HashSet::with_capacity(session.auth_headers.len());
    for (name, value) in &session.auth_headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("invalid captured authentication header name {name:?}"))?;
        reqwest::header::HeaderValue::from_bytes(value.as_bytes())
            .map_err(|_| format!("invalid captured authentication value for header {name}"))?;
        if !session_header_names.insert(name.as_str().to_owned()) {
            return Err("captured authentication contains duplicate header names".into());
        }
        // HTTP header names are case-insensitive. Canonicalizing keys makes
        // the session credential deterministically override a stale template
        // instead of emitting two semantic copies in HashMap iteration order.
        let is_current_data_flow = step.headers_template.iter().any(|(header, template)| {
            header.eq_ignore_ascii_case(name.as_str())
                && step.param_sources.iter().any(|(parameter, source)| {
                    matches!(source, RecipeParamSource::DataFlow { .. })
                        && template.contains(&format!("{{{parameter}}}"))
                })
        });
        if !is_current_data_flow {
            headers.insert(name.as_str().to_owned(), value.clone());
        }
    }
    if headers.len() > MAX_RECIPE_HEADERS
        || headers.iter().any(|(name, value)| {
            name.len() > MAX_RECIPE_HEADER_BYTES || value.len() > MAX_RECIPE_HEADER_BYTES
        })
    {
        return Err("resolved headers exceed the Task Recipe limit".into());
    }
    let body = step
        .body_template
        .as_ref()
        .map(|template| {
            fill_body_template(
                template,
                params,
                &step.body_param_types,
                headers.get("content-type").map(String::as_str),
            )
        })
        .transpose()?;
    if body
        .as_ref()
        .is_some_and(|body| body.len() > MAX_RECIPE_BODY_BYTES)
    {
        return Err("resolved body exceeds the Task Recipe limit".into());
    }
    Ok(TransportRequest {
        method: step.method.clone(),
        url,
        headers,
        body,
        timeout,
    })
}

fn fill_body_template(
    template: &str,
    params: &HashMap<String, String>,
    types: &HashMap<String, TaskInputSchema>,
    content_type: Option<&str>,
) -> Result<String, String> {
    let format = request_body_format(content_type);
    // Clients often attach a default Content-Type to a bodyless action too.
    // Preserve an empty captured payload; it is not a malformed JSON document
    // that should be rewritten or block an otherwise valid request.
    if template.trim().is_empty() {
        return Ok(template.to_owned());
    }
    if matches!(format, RequestBodyFormat::Json | RequestBodyFormat::Infer) {
        match serde_json::from_str::<serde_json::Value>(template) {
            Ok(mut json) => {
                fill_json_placeholders(&mut json, params, types)?;
                return serde_json::to_string(&json)
                    .map_err(|error| format!("could not serialize resolved JSON body: {error}"));
            },
            Err(_) if format == RequestBodyFormat::Json => {
                return Err("recipe body is not valid JSON for its Content-Type".into())
            },
            Err(_) => {},
        }
    }

    if crate::magician_v2::api_mining::recipe::is_urlencoded_form_body(template)
        && (format == RequestBodyFormat::Form
            || (format == RequestBodyFormat::Infer && template.contains('=')))
    {
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(template.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in pairs {
            let value = if let Some(name) = exact_placeholder_name(&value) {
                if params.contains_key(name) || types.contains_key(name) {
                    params
                        .get(name)
                        .cloned()
                        .ok_or_else(|| format!("unfilled body placeholder {{{name}}}"))?
                } else {
                    value
                }
            } else {
                value
            };
            serializer.append_pair(&key, &value);
        }
        return Ok(serializer.finish());
    }

    fill_template(template, params)
}

fn fill_json_placeholders(
    value: &mut serde_json::Value,
    params: &HashMap<String, String>,
    types: &HashMap<String, TaskInputSchema>,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(template) => {
            let Some(name) = exact_placeholder_name(template) else {
                return Ok(());
            };
            if !params.contains_key(name) && !types.contains_key(name) {
                // Stable literals can themselves be shaped like `{label}`.
                // Only declared body parameters participate in substitution.
                return Ok(());
            }
            let raw = params
                .get(name)
                .ok_or_else(|| format!("unfilled body placeholder {{{name}}}"))?;
            *value = match types.get(name).copied().unwrap_or(TaskInputSchema::String) {
                TaskInputSchema::String => serde_json::Value::String(raw.clone()),
                TaskInputSchema::Number => {
                    let number = parse_task_number(raw).ok_or_else(|| {
                        format!("body parameter {name} is not a finite JSON number")
                    })?;
                    serde_json::Value::Number(number)
                },
                TaskInputSchema::Boolean => {
                    serde_json::Value::Bool(match raw.trim().to_ascii_lowercase().as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(format!("body parameter {name} is not a boolean")),
                    })
                },
            };
        },
        serde_json::Value::Array(items) => {
            for item in items {
                fill_json_placeholders(item, params, types)?;
            }
        },
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                fill_json_placeholders(item, params, types)?;
            }
        },
        _ => {},
    }
    Ok(())
}

fn exact_placeholder_name(value: &str) -> Option<&str> {
    let name = value.strip_prefix('{')?.strip_suffix('}')?;
    (!name.is_empty() && !name.contains(['{', '}'])).then_some(name)
}

pub fn classify_auth_stale(response: &TransportResponse) -> bool {
    if response.status == 401 {
        return true;
    }
    if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        if let Some(location) = response.headers.get("location") {
            let location = location.to_ascii_lowercase();
            return location.contains("login")
                || location.contains("signin")
                || location.contains("/auth");
        }
    }
    response.status == 403 && {
        let body = utf8_prefix(&response.body, 4096).to_ascii_lowercase();
        body.contains("log in") || body.contains("login") || body.contains("sign in")
    }
}

pub fn classify_failure(response: &TransportResponse) -> Option<FailureClass> {
    let head = utf8_prefix(&response.body, 4096).to_ascii_lowercase();
    let is_html = response
        .headers
        .get("content-type")
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        || head.trim_start().starts_with("<!doctype html")
        || head.trim_start().starts_with("<html");
    if response.status == 429
        || response.headers.contains_key("cf-mitigated")
        || !(200..300).contains(&response.status)
            && CHALLENGE_MARKERS.iter().any(|marker| head.contains(marker))
        || is_html
            && [
                "<title>just a moment",
                "cf-chl-",
                "/cdn-cgi/challenge-platform/",
            ]
            .iter()
            .any(|marker| head.contains(marker))
    {
        return Some(FailureClass::AntiBot);
    }
    if (200..300).contains(&response.status) {
        return None;
    }
    if classify_auth_stale(response) {
        return Some(FailureClass::Auth);
    }
    Some(FailureClass::Http)
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return "<invalid-url>".into();
    };
    if parsed.query().is_some() {
        let names: Vec<_> = parsed
            .query_pairs()
            .map(|(name, _)| name.into_owned())
            .collect();
        parsed.set_query(Some(
            &names
                .into_iter()
                .map(|name| format!("{name}=[REDACTED]"))
                .collect::<Vec<_>>()
                .join("&"),
        ));
    }
    parsed.to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn warm_replays_reuse_connections_without_sharing_auth_or_cookies() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            // Accept exactly one connection: the second transport must use
            // this keep-alive stream, not open a new connection per replay.
            let (mut stream, _) = listener.accept().await.unwrap();
            for token in ["first-scope", "second-scope"] {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192);
                    request.push(stream.read_u8().await.unwrap());
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.contains(&format!("authorization: bearer {token}\r\n")));
                assert_eq!(request.matches("\r\nauthorization:").count(), 1);
                assert!(!request.contains("\r\ncookie:"));
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nSet-Cookie: private=first-scope; Path=/\r\nConnection: keep-alive\r\n\r\n{}").await.unwrap();
            }
        });
        for token in ["first-scope", "second-scope"] {
            let request = TransportRequest {
                method: "GET".into(),
                url: format!("http://{address}/items"),
                headers: HashMap::from([("authorization".into(), format!("Bearer {token}"))]),
                body: None,
                timeout: Duration::from_secs(3),
            };
            assert_eq!(
                ReqwestTransport::default()
                    .send(&request)
                    .await
                    .unwrap()
                    .status,
                200
            );
        }
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn failed_client_initialization_cannot_use_a_redirect_following_fallback() {
        let transport = ReqwestTransport {
            client: Err("initialization failed".into()),
        };
        let request = TransportRequest {
            method: "GET".into(),
            url: "https://example.test/".into(),
            headers: HashMap::new(),
            body: None,
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            transport.send(&request).await.unwrap_err(),
            "initialization failed"
        );
    }
    use crate::magician_v2::api_mining::recipe_observer::{RecipeEvent, RunObserver};
    use crate::magician_v2::api_mining::workflow::{InferenceMethod, ReplayStats};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct MockTransport {
        kind: Transport,
        sends: AtomicUsize,
        responses: Mutex<VecDeque<Result<TransportResponse, String>>>,
    }

    impl MockTransport {
        fn new(kind: Transport, responses: Vec<Result<TransportResponse, String>>) -> Self {
            Self {
                kind,
                sends: AtomicUsize::new(0),
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait::async_trait]
    impl StepTransport for MockTransport {
        fn kind(&self) -> Transport {
            self.kind
        }

        async fn send(&self, _request: &TransportRequest) -> Result<TransportResponse, String> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("unexpected extra send".into()))
        }
    }

    #[derive(Default)]
    struct CollectingObserver(Mutex<Vec<&'static str>>);

    impl RunObserver for CollectingObserver {
        fn observe(&self, event: RecipeEvent<'_>) {
            self.0.lock().unwrap().push(event.kind());
        }
    }

    struct CountingHealer(AtomicUsize);

    #[async_trait::async_trait]
    impl AuthHealer for CountingHealer {
        async fn heal(&self, _origin: &str) -> Result<bool, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
    }

    fn response(status: u16, body: &str) -> TransportResponse {
        TransportResponse {
            status,
            headers: HashMap::new(),
            body: body.into(),
        }
    }

    fn one_step_recipe(side_effects: SideEffects) -> TaskRecipe {
        let origin = "https://fixture.example";
        let step = RecipeStep {
            id: "step_1".into(),
            origin: origin.into(),
            method: if side_effects == SideEffects::ReadOnly {
                "GET".into()
            } else {
                "POST".into()
            },
            url_template: format!("{origin}/api/items"),
            headers_template: HashMap::new(),
            body_template: None,
            capability_id: None,
            param_sources: HashMap::new(),
            body_param_types: HashMap::new(),
            side_effects,
            request_shape_fingerprint: "shape".into(),
            verify_with: None,
            browser_fallback: None,
            transport_hint: None,
        };
        TaskRecipe {
            id: "recipe_fixture".into(),
            scope_principal: "owner".into(),
            scope_workspace: "default".into(),
            agent_id: "assistant".into(),
            shape: RecipeShape {
                description_template: None,
                template: "fixture task".into(),
                fingerprint: "fixture-shape".into(),
                inputs: Vec::new(),
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec![origin.into()],
                steps: vec![step],
                data_flows: Vec::new(),
                answer_spec: Vec::new(),
                auth: RecipeAuth::default(),
                maturity: RecipeMaturity::Draft,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "task_fixture".into(),
                    execution_id: "exec_fixture".into(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: 0,
                last_replayed_at_ms: None,
            }],
        }
    }

    fn write_then_verify_recipe() -> TaskRecipe {
        let mut recipe = one_step_recipe(SideEffects::Write);
        recipe.shape.inputs.push(TaskInput {
            name: "label".into(),
            schema: TaskInputSchema::String,
            example_value: "old-label".into(),
            source: TaskInputSource::TaskText,
        });
        let version = recipe.current_mut().unwrap();
        version.steps[0].id = "write".into();
        version.steps[0].verify_with = Some("verify".into());
        version.steps[0].body_template = Some(r#"{"label":"{label}"}"#.into());
        version.steps[0]
            .body_param_types
            .insert("label".into(), TaskInputSchema::String);
        version.steps[0].param_sources.insert(
            "label".into(),
            RecipeParamSource::TaskInput {
                name: "label".into(),
            },
        );
        let mut verify = version.steps[0].clone();
        verify.id = "verify".into();
        verify.method = "GET".into();
        verify.side_effects = SideEffects::ReadOnly;
        verify.verify_with = None;
        verify.param_sources.clear();
        verify.body_param_types.clear();
        verify.body_template = None;
        version.steps.push(verify);
        recipe
    }

    async fn run_with<'a>(
        recipe: &'a mut TaskRecipe,
        transports: Vec<Box<dyn StepTransport>>,
        grants: &'a ReplayGrantStore,
        policy: &'a OriginPolicyStore,
        inputs: RecipeRunInputs,
        healer: Option<&'a dyn AuthHealer>,
        max_auth_heals: u32,
        observer: Option<&'a dyn RunObserver>,
    ) -> RecipeRunResult {
        RecipeRunner {
            transports,
            can_continue: None,
            grants,
            origin_policy: policy,
            session_lookup: &|_, _| Some(SessionContext::default()),
            auth_healer: healer,
            max_auth_heals,
            step_feedback: None,
            observer,
        }
        .run(recipe, &inputs)
        .await
    }

    #[test]
    fn successful_http_challenge_is_not_a_successful_recipe_response() {
        assert_eq!(
            classify_failure(&response(
                200,
                "<html><title>Just a moment...</title></html>"
            )),
            Some(FailureClass::AntiBot)
        );
        assert_eq!(
            classify_failure(&response(200, r#"{"title":"Akamai captcha research"}"#)),
            None
        );
        assert_eq!(
            classify_failure(&response(
                200,
                "<html><title>Captcha research</title></html>"
            )),
            None
        );
    }

    #[tokio::test]
    async fn disabling_replay_after_a_read_stops_the_next_request() {
        struct SwitchObserver(std::sync::atomic::AtomicBool);
        impl RunObserver for SwitchObserver {
            fn observe(&self, event: RecipeEvent<'_>) {
                if matches!(event, RecipeEvent::StepCompleted { .. }) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let observer = SwitchObserver(std::sync::atomic::AtomicBool::new(true));
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let version = recipe.current_mut().unwrap();
        let mut second = version.steps[0].clone();
        second.id = "second".into();
        version.steps.push(second);
        let allowed = || observer.0.load(Ordering::SeqCst);
        let runner = RecipeRunner {
            transports: vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![Ok(response(200, "{}"))],
            ))],
            grants: &grants,
            origin_policy: &policy,
            can_continue: Some(&allowed),
            session_lookup: &|_, _| None,
            auth_healer: None,
            max_auth_heals: 0,
            step_feedback: None,
            observer: Some(&observer),
        };
        let result = runner.run(&mut recipe, &RecipeRunInputs::default()).await;
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.fallback.unwrap().class, FailureClass::PolicyBlocked);
    }

    #[tokio::test]
    async fn grant_revoked_during_a_read_cannot_authorize_the_following_write() {
        struct RevokeObserver<'a> {
            grants: &'a ReplayGrantStore,
            id: String,
        }
        impl RunObserver for RevokeObserver<'_> {
            fn observe(&self, event: RecipeEvent<'_>) {
                if matches!(event, RecipeEvent::StepCompleted { .. }) {
                    self.grants.revoke(&self.id).unwrap();
                }
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        let step = &recipe.current().unwrap().steps[0];
        let key = GrantKey {
            recipe_id: Some(recipe.id.clone()),
            step_id: Some(step.id.clone()),
            capability_id: step.capability_id.clone(),
            request_shape_fingerprint: step.effective_request_shape_fingerprint(),
        };
        let grant = grants
            .grant_for_url(&key, &step.url_template, Some("approved"))
            .unwrap();
        let observer = RevokeObserver {
            grants: &grants,
            id: grant.id,
        };
        let mut read = recipe.current().unwrap().steps[1].clone();
        read.id = "read_first".into();
        recipe.current_mut().unwrap().steps.insert(0, read);
        let mut approved_write_steps = HashSet::new();
        if super::super::approval::decision_is_one_run_approval(
            super::super::approval::OPTION_APPROVE_ALWAYS_STEP,
        ) {
            approved_write_steps.insert("write".to_string());
        }
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![Ok(response(200, "{}"))],
            ))],
            &grants,
            &policy,
            RecipeRunInputs {
                inputs: HashMap::from([("label".into(), "new".into())]),
                approved_write_steps,
                ..Default::default()
            },
            None,
            0,
            Some(&observer),
        )
        .await;
        assert_eq!(result.steps.len(), 1);
        assert!(result.fallback.is_none());
        let failure = result.failure.unwrap();
        assert_eq!(failure.step_id, "write");
        assert_eq!(failure.class, FailureClass::PolicyBlocked);
    }

    #[test]
    fn current_response_csrf_overrides_stale_session_header() {
        let recipe = one_step_recipe(SideEffects::ReadOnly);
        let mut step = recipe.current().unwrap().steps[0].clone();
        step.headers_template
            .insert("X-CSRF-Token".into(), "{csrf}".into());
        step.param_sources.insert(
            "csrf".into(),
            RecipeParamSource::DataFlow {
                flow_id: "flow".into(),
            },
        );
        let mut session = SessionContext::default();
        session
            .auth_headers
            .insert("x-csrf-token".into(), "stale".into());
        let request = build_request(
            &step,
            &HashMap::from([("csrf".into(), "fresh".into())]),
            &session,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(request.headers["x-csrf-token"], "fresh");
    }

    #[tokio::test]
    async fn runner_bounds_a_transport_that_ignores_request_timeout() {
        struct HangingTransport;
        #[async_trait::async_trait]
        impl StepTransport for HangingTransport {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, _: &TransportRequest) -> Result<TransportResponse, String> {
                std::future::pending().await
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        for write in [false, true] {
            let mut recipe = if write {
                write_then_verify_recipe()
            } else {
                one_step_recipe(SideEffects::ReadOnly)
            };
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                run_with(
                    &mut recipe,
                    vec![Box::new(HangingTransport)],
                    &grants,
                    &policy,
                    RecipeRunInputs {
                        timeout_ms: Some(10),
                        inputs: if write {
                            HashMap::from([("label".into(), "new".into())])
                        } else {
                            HashMap::new()
                        },
                        approved_write_steps: HashSet::from(["write".into()]),
                    },
                    None,
                    0,
                    None,
                ),
            )
            .await
            .expect("runner must enforce its own deadline");
            assert!(!result.success);
            if write {
                assert_eq!(result.failure.unwrap().class, FailureClass::Network);
                assert!(
                    result.fallback.is_none(),
                    "timed out write must not repeat in browser"
                );
            } else {
                assert_eq!(result.fallback.unwrap().class, FailureClass::Network);
            }
        }
    }

    #[test]
    fn response_header_copy_enforces_budget_before_retaining_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-small",
            reqwest::header::HeaderValue::from_static("present"),
        );
        assert_eq!(
            collect_response_headers(&headers)
                .unwrap()
                .get("x-small")
                .map(String::as_str),
            Some("present")
        );

        headers.insert(
            "x-oversized",
            reqwest::header::HeaderValue::from_bytes(&vec![
                b'x';
                MAX_RECIPE_HEADER_BYTES
                    .saturating_add(1)
            ])
            .unwrap(),
        );
        assert!(collect_response_headers(&headers).is_err());
    }

    #[test]
    fn request_headers_are_case_normalized_and_session_auth_wins() {
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let step = &mut recipe.current_mut().unwrap().steps[0];
        step.headers_template
            .insert("X-Mode".into(), "template".into());
        let mut session = SessionContext::default();
        session
            .auth_headers
            .insert("x-mode".into(), "session".into());

        let request =
            build_request(step, &HashMap::new(), &session, Duration::from_secs(1)).unwrap();

        assert_eq!(request.headers.len(), 1);
        assert_eq!(
            request.headers.get("x-mode").map(String::as_str),
            Some("session")
        );
    }

    #[test]
    fn duplicate_case_insensitive_session_headers_fail_closed() {
        let recipe = one_step_recipe(SideEffects::ReadOnly);
        let step = &recipe.current().unwrap().steps[0];
        let mut session = SessionContext::default();
        session
            .auth_headers
            .insert("X-Session".into(), "first".into());
        session
            .auth_headers
            .insert("x-session".into(), "second".into());

        assert!(build_request(step, &HashMap::new(), &session, Duration::from_secs(1),).is_err());
    }

    #[tokio::test]
    async fn duplicate_case_insensitive_recipe_headers_fail_preflight() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let step = &mut recipe.current_mut().unwrap().steps[0];
        step.headers_template.insert("X-Mode".into(), "one".into());
        step.headers_template.insert("x-mode".into(), "two".into());

        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;

        assert!(result.steps.is_empty());
        assert_eq!(result.failure.unwrap().class, FailureClass::SchemaDrift);
    }

    #[tokio::test]
    async fn write_without_grant_is_never_sent() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let transport = MockTransport::new(Transport::Reqwest, vec![Ok(response(200, "{}"))]);
        let mut recipe = write_then_verify_recipe();
        let mut inputs = RecipeRunInputs::default();
        inputs.inputs.insert("label".into(), "new-label".into());
        let result = run_with(
            &mut recipe,
            vec![Box::new(transport)],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;
        assert!(result.pending_approval.is_some());
        assert!(result.steps.is_empty());
        assert!(result.fallback.is_none());
    }

    #[tokio::test]
    async fn unknown_side_effects_fail_before_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::Unknown);
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;
        assert_eq!(result.fallback.unwrap().class, FailureClass::PolicyBlocked);
    }

    #[tokio::test]
    async fn empty_current_version_fails_before_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        recipe.current_mut().unwrap().steps.clear();
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;
        assert_eq!(result.failure.unwrap().class, FailureClass::SchemaDrift);
    }

    #[tokio::test]
    async fn invalid_input_fails_before_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        {
            let version = recipe.current_mut().unwrap();
            version.maturity = RecipeMaturity::Candidate;
            version.replay_stats.successful_replays = 2;
        }
        recipe.shape.inputs.push(TaskInput {
            name: "limit".into(),
            schema: TaskInputSchema::Number,
            example_value: "10".into(),
            source: TaskInputSource::TaskText,
        });
        let mut inputs = RecipeRunInputs::default();
        inputs.inputs.insert("limit".into(), "NaN".into());
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;
        assert_eq!(result.failure.unwrap().class, FailureClass::InputMissing);
        let version = recipe.current().unwrap();
        assert_eq!(version.maturity, RecipeMaturity::Candidate);
        assert_eq!(version.replay_stats.successful_replays, 2);
        assert_eq!(version.replay_stats.failed_replays, 0);
        assert!(version.last_replayed_at_ms.is_some());
    }

    #[tokio::test]
    async fn missing_auth_does_not_poison_recipe_reliability_stats() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        {
            let version = recipe.current_mut().unwrap();
            version.maturity = RecipeMaturity::Candidate;
            version.replay_stats.successful_replays = 2;
            version.steps[0].param_sources.insert(
                "authorization".into(),
                RecipeParamSource::SessionAuth {
                    scheme: "authorization".into(),
                },
            );
        }

        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;

        assert_eq!(result.fallback.unwrap().class, FailureClass::Auth);
        let version = recipe.current().unwrap();
        assert_eq!(version.maturity, RecipeMaturity::Candidate);
        assert_eq!(version.replay_stats.successful_replays, 2);
        assert_eq!(version.replay_stats.failed_replays, 0);
        assert!(version.last_replayed_at_ms.is_some());
    }

    #[tokio::test]
    async fn missing_auth_before_the_first_write_allows_browser_recovery() {
        for include_read_prefix in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let grants = ReplayGrantStore::open(temp.path());
            let policy = OriginPolicyStore::open(temp.path());
            let mut recipe = write_then_verify_recipe();
            let version = recipe.current_mut().unwrap();
            version.steps[0].param_sources.insert(
                "authorization".into(),
                RecipeParamSource::SessionAuth {
                    scheme: "authorization".into(),
                },
            );
            if include_read_prefix {
                let mut read = version.steps[1].clone();
                read.id = "read_first".into();
                version.steps.insert(0, read);
            }
            let result = run_with(
                &mut recipe,
                vec![Box::new(MockTransport::new(
                    Transport::Reqwest,
                    vec![Ok(response(200, "{}"))],
                ))],
                &grants,
                &policy,
                RecipeRunInputs {
                    inputs: HashMap::from([("label".into(), "new-label".into())]),
                    approved_write_steps: HashSet::from(["write".into()]),
                    ..Default::default()
                },
                None,
                0,
                None,
            )
            .await;
            assert!(!result.success);
            assert!(result.failure.is_none());
            assert!(!result.write_outcome_uncertain(&recipe));
            assert_eq!(result.steps.len(), usize::from(include_read_prefix));
            let fallback = result.fallback.as_ref().unwrap();
            assert_eq!(fallback.class, FailureClass::Auth);
            assert_eq!(fallback.step_id, "write");
            assert_eq!(fallback.replayed.len(), usize::from(include_read_prefix));
            assert!(matches!(
                crate::magician_v2::artifact_v2::recipe_replay_hook::attempt_from_result(&recipe, &result),
                crate::magician_v2::artifact_v2::recipe_replay_hook::RecipeReplayAttempt::Fallback { .. }
            ));
        }
    }

    #[tokio::test]
    async fn missing_auth_after_an_earlier_write_still_forbids_browser_retry() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        let version = recipe.current_mut().unwrap();
        let mut later_write = version.steps[0].clone();
        later_write.id = "write_later".into();
        later_write.param_sources.insert(
            "authorization".into(),
            RecipeParamSource::SessionAuth {
                scheme: "authorization".into(),
            },
        );
        version.steps.insert(1, later_write);
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![Ok(response(200, "{}"))],
            ))],
            &grants,
            &policy,
            RecipeRunInputs {
                inputs: HashMap::from([("label".into(), "new-label".into())]),
                approved_write_steps: HashSet::from(["write".into(), "write_later".into()]),
                ..Default::default()
            },
            None,
            0,
            None,
        )
        .await;
        assert!(!result.success);
        assert!(result.fallback.is_none());
        assert_eq!(result.steps.len(), 1);
        assert!(result.write_outcome_uncertain(&recipe));
        let failure = result.failure.as_ref().unwrap();
        assert_eq!(failure.class, FailureClass::Auth);
        assert_eq!(failure.step_id, "write");
        assert!(matches!(
            crate::magician_v2::artifact_v2::recipe_replay_hook::attempt_from_result(
                &recipe, &result
            ),
            crate::magician_v2::artifact_v2::recipe_replay_hook::RecipeReplayAttempt::WriteFailed { .. }
        ));
    }

    #[tokio::test]
    async fn mismatched_declared_origin_fails_before_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        recipe.current_mut().unwrap().steps[0].origin = "https://other.example".into();
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;
        assert_eq!(result.failure.unwrap().class, FailureClass::SchemaDrift);
    }

    #[tokio::test]
    async fn mutating_method_cannot_be_relabelled_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        recipe.current_mut().unwrap().steps[0].method = "DELETE".into();
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;
        assert_eq!(result.failure.unwrap().class, FailureClass::SchemaDrift);
    }

    #[tokio::test]
    async fn unverifiable_write_fails_before_approval_or_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        let mut inputs = RecipeRunInputs::default();
        inputs.inputs.insert("label".into(), String::new());
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;
        assert!(result.pending_approval.is_none());
        assert_eq!(result.failure.unwrap().class, FailureClass::InputMissing);
    }

    #[tokio::test]
    async fn write_without_a_verification_read_fails_before_approval_or_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::Write);
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;

        assert!(result.pending_approval.is_none());
        assert_eq!(result.failure.unwrap().class, FailureClass::SchemaDrift);
    }

    #[tokio::test]
    async fn read_with_a_write_verification_edge_fails_before_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let mut later = recipe.current().unwrap().steps[0].clone();
        later.id = "later_read".into();
        recipe.current_mut().unwrap().steps[0].verify_with = Some(later.id.clone());
        recipe.current_mut().unwrap().steps.push(later);
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;

        assert_eq!(result.failure.unwrap().class, FailureClass::SchemaDrift);
    }

    #[tokio::test]
    async fn invalid_answer_extractor_fails_before_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        recipe.current_mut().unwrap().answer_spec.push(AnswerField {
            field: "value".into(),
            step_id: "step_1".into(),
            extractor: Extractor::Regex {
                pattern: "(".into(),
                group: 1,
            },
        });
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(Transport::Reqwest, vec![]))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;
        assert_eq!(result.failure.unwrap().class, FailureClass::SchemaDrift);
    }

    #[test]
    fn regex_answers_and_dependencies_reject_multiple_matches_even_if_equal() {
        let extractor = Extractor::Regex {
            pattern: r"<span>([^<]{1,200})</span>".into(),
            group: 1,
        };
        let mut stored = StoredResponse {
            json: None,
            text: "<span>current value</span>".into(),
            headers: HashMap::new(),
        };
        assert_eq!(
            extract(&extractor, &stored).as_deref(),
            Some("current value")
        );
        assert_eq!(
            extract_answer(&extractor, &stored),
            Some(serde_json::json!("current value"))
        );
        for body in [
            "<span>current value</span><span>other value</span>",
            "<span>current value</span><span>current value</span>",
        ] {
            stored.text = body.into();
            assert!(extract(&extractor, &stored).is_none());
            assert!(extract_answer(&extractor, &stored).is_none());
        }
    }

    #[tokio::test]
    async fn ambiguous_text_dependency_cannot_dispatch_an_approved_write() {
        struct BootstrapOnly;
        #[async_trait::async_trait]
        impl StepTransport for BootstrapOnly {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
                assert_eq!(
                    request.method, "GET",
                    "ambiguous extraction must not dispatch the write"
                );
                assert!(request.url.ends_with("/bootstrap"));
                Ok(response(
                    200,
                    "<span>current-label</span><span>unrelated</span>",
                ))
            }
        }
        let mut recipe = write_then_verify_recipe();
        recipe.shape.inputs.clear();
        let version = recipe.current_mut().unwrap();
        let mut bootstrap = version.steps[1].clone();
        bootstrap.id = "bootstrap".into();
        bootstrap.url_template = format!("{}/bootstrap", bootstrap.origin);
        version.steps[0].param_sources.insert(
            "label".into(),
            RecipeParamSource::DataFlow {
                flow_id: "label-flow".into(),
            },
        );
        version.data_flows.push(RecipeDataFlow {
            id: "label-flow".into(),
            source_step: "bootstrap".into(),
            extractor: Extractor::Regex {
                pattern: r"<span>([^<]{1,200})</span>".into(),
                group: 1,
            },
            target_step: "write".into(),
            target_param: "label".into(),
            confidence: 0.9,
            inference: InferenceMethod::AutoMatch,
        });
        version.steps.insert(0, bootstrap);
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let result = run_with(
            &mut recipe,
            vec![Box::new(BootstrapOnly)],
            &grants,
            &policy,
            RecipeRunInputs {
                approved_write_steps: HashSet::from(["write".into()]),
                ..Default::default()
            },
            None,
            0,
            None,
        )
        .await;
        assert!(!result.success);
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.steps[0].step_id, "bootstrap");
        assert_eq!(
            result.fallback.as_ref().unwrap().class,
            FailureClass::SchemaDrift
        );
        assert!(result.failure.is_none());
        assert!(!result.write_outcome_uncertain(&recipe));
    }

    #[test]
    fn wire_inputs_cannot_mint_one_run_write_approval() {
        let inputs: RecipeRunInputs = serde_json::from_value(serde_json::json!({
            "inputs": {},
            "approved_write_steps": ["step_1"]
        }))
        .unwrap();
        assert!(inputs.approved_write_steps.is_empty());
    }

    #[tokio::test]
    async fn failed_write_is_sent_once_and_never_falls_back() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        let mut inputs = RecipeRunInputs::default();
        inputs.inputs.insert("label".into(), "new-label".into());
        inputs.approved_write_steps.insert("write".into());
        let result = run_with(
            &mut recipe,
            vec![
                Box::new(MockTransport::new(
                    Transport::Reqwest,
                    vec![Ok(response(503, "failed"))],
                )),
                Box::new(MockTransport::new(
                    Transport::InPageFetch,
                    vec![Ok(response(200, "must not run"))],
                )),
            ],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;
        assert!(result.failure.is_some());
        assert!(result.fallback.is_none());
        assert_eq!(result.steps.len(), 1);
        assert_eq!(result.steps[0].transport, Transport::Reqwest);
    }

    #[tokio::test]
    async fn verification_read_must_reflect_the_current_write_task_input() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        let mut inputs = RecipeRunInputs::default();
        inputs
            .inputs
            .insert("label".into(), "new-private-label".into());
        inputs.approved_write_steps.insert("write".into());

        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![
                    Ok(response(200, "{}")),
                    // Incidental auth/literal data must not prove the write.
                    Ok(response(200, r#"{"authorization":"stable-token"}"#)),
                ],
            ))],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;

        assert!(!result.success);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.class),
            Some(FailureClass::SchemaDrift)
        );
        assert_eq!(
            result
                .failure
                .as_ref()
                .map(|failure| failure.step_id.as_str()),
            Some("write")
        );
        assert_eq!(
            result
                .failure
                .as_ref()
                .map(|failure| failure.detail.as_str()),
            Some("a write was sent but the recipe outcome could not be verified")
        );
        assert!(result.fallback.is_none());
    }

    #[tokio::test]
    async fn failed_verification_read_after_a_write_is_never_a_browser_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        let mut inputs = RecipeRunInputs::default();
        inputs.inputs.insert("label".into(), "new-label".into());
        inputs.approved_write_steps.insert("write".into());

        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![
                    Ok(response(200, "{}")),
                    Ok(response(503, "verification unavailable")),
                ],
            ))],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;

        assert!(!result.success);
        assert!(result.fallback.is_none());
        assert_eq!(
            result
                .failure
                .as_ref()
                .map(|failure| failure.step_id.as_str()),
            Some("write")
        );
        assert!(result
            .failure
            .as_ref()
            .is_some_and(|failure| failure.detail.contains("downstream step verify failed")));
    }

    #[tokio::test]
    async fn verification_read_can_prove_a_write_with_an_upstream_data_flow() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        recipe.shape.inputs.clear();
        let version = recipe.current_mut().unwrap();
        let mut source = version.steps[1].clone();
        source.id = "source".into();
        source.url_template = "https://fixture.example/api/source".into();
        version.steps[0].param_sources.insert(
            "label".into(),
            RecipeParamSource::DataFlow {
                flow_id: "flow_label".into(),
            },
        );
        version.data_flows.push(RecipeDataFlow {
            id: "flow_label".into(),
            source_step: "source".into(),
            extractor: Extractor::JsonPath {
                path: "$.id".into(),
            },
            target_step: "write".into(),
            target_param: "label".into(),
            confidence: 1.0,
            inference: InferenceMethod::AutoMatch,
        });
        version.steps.insert(0, source);
        let mut inputs = RecipeRunInputs::default();
        inputs.approved_write_steps.insert("write".into());

        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![
                    Ok(response(200, r#"{"id":"item-42"}"#)),
                    Ok(response(200, "{}")),
                    Ok(response(200, r#"{"items":[{"id":"item-42"}]}"#)),
                ],
            ))],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;

        assert!(result.success);
        assert_eq!(result.steps.len(), 3);
    }

    #[tokio::test]
    async fn empty_data_flow_value_never_reaches_a_write_transport() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        recipe.shape.inputs.clear();
        let version = recipe.current_mut().unwrap();
        let mut source = version.steps[1].clone();
        source.id = "source".into();
        source.url_template = "https://fixture.example/api/source".into();
        version.steps[0].param_sources.insert(
            "label".into(),
            RecipeParamSource::DataFlow {
                flow_id: "flow_label".into(),
            },
        );
        version.data_flows.push(RecipeDataFlow {
            id: "flow_label".into(),
            source_step: "source".into(),
            extractor: Extractor::JsonPath {
                path: "$.id".into(),
            },
            target_step: "write".into(),
            target_param: "label".into(),
            confidence: 1.0,
            inference: InferenceMethod::AutoMatch,
        });
        version.steps.insert(0, source);
        let mut inputs = RecipeRunInputs::default();
        inputs.approved_write_steps.insert("write".into());

        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![Ok(response(200, r#"{"id":""}"#))],
            ))],
            &grants,
            &policy,
            inputs,
            None,
            0,
            None,
        )
        .await;

        assert!(!result.success);
        assert!(result.failure.is_none());
        assert_eq!(result.steps.len(), 1);
        assert_eq!(
            result
                .fallback
                .as_ref()
                .map(|fallback| fallback.step_id.as_str()),
            Some("write")
        );
    }

    #[tokio::test]
    async fn read_transport_downgrade_is_observed_and_persisted_as_hint() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let observer = CollectingObserver::default();
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let result = run_with(
            &mut recipe,
            vec![
                Box::new(MockTransport::new(
                    Transport::Reqwest,
                    vec![Ok(response(403, "Just a moment cf-chl"))],
                )),
                Box::new(MockTransport::new(
                    Transport::InPageFetch,
                    vec![Ok(response(200, "{}"))],
                )),
            ],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            Some(&observer),
        )
        .await;
        assert!(result.success);
        assert_eq!(
            recipe.current().unwrap().steps[0].transport_hint,
            Some(Transport::InPageFetch)
        );
        assert_eq!(
            observer.0.lock().unwrap().as_slice(),
            [
                "recipe.replay.started",
                "recipe.replay.transport.downgraded",
                "recipe.replay.step.completed",
                "recipe.replay.completed",
            ]
        );
    }

    #[tokio::test]
    async fn auth_healing_respects_the_total_run_cap() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let healer = CountingHealer(AtomicUsize::new(0));
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![Ok(response(401, "expired")), Ok(response(401, "expired"))],
            ))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            Some(&healer),
            1,
            None,
        )
        .await;
        assert!(!result.success);
        assert_eq!(healer.0.load(Ordering::SeqCst), 1);
        assert_eq!(result.auth_heals, 1);
    }

    #[tokio::test]
    async fn oversized_in_page_response_fails_at_the_shared_transport_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let oversized = "x".repeat(MAX_RECIPE_RESPONSE_BYTES + 1);
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::InPageFetch,
                vec![Ok(response(200, &oversized))],
            ))],
            &grants,
            &policy,
            RecipeRunInputs::default(),
            None,
            0,
            None,
        )
        .await;

        assert!(!result.success);
        assert_eq!(
            result.fallback.as_ref().map(|fallback| fallback.class),
            Some(FailureClass::Network)
        );
        assert!(result.steps.is_empty());
    }

    #[test]
    fn cumulative_response_budget_is_checked_without_overflow() {
        assert_eq!(
            accumulate_response_budget(MAX_RECIPE_TOTAL_RESPONSE_BYTES - 1, 1),
            Some(MAX_RECIPE_TOTAL_RESPONSE_BYTES)
        );
        assert_eq!(
            accumulate_response_budget(MAX_RECIPE_TOTAL_RESPONSE_BYTES, 1),
            None
        );
        assert_eq!(accumulate_response_budget(usize::MAX, 1), None);
    }

    #[test]
    fn body_rendering_obeys_declared_media_type_instead_of_equals_heuristics() {
        let params = HashMap::from([("value".into(), "A&B + C".into())]);
        let types = HashMap::from([("value".into(), TaskInputSchema::String)]);
        for (media_type, body) in [
            ("application/xml", "<item value=\"fixed\" />"),
            ("application/graphql", "query Item($id: ID = 42) { item(id: $id) { name } }"),
            ("text/plain", "text=value&keep=+%20"),
            ("text/plain", "{ \"keep_spacing\" : true }"),
            ("multipart/form-data; boundary=upload", "--upload\r\nContent-Disposition: form-data; name=\"label\"\r\n\r\nAlice\r\n--upload--"),
        ] {
            assert_eq!(fill_body_template(body, &HashMap::new(), &HashMap::new(), Some(media_type)).unwrap(), body);
        }
        assert_eq!(
            fill_body_template("{value}", &params, &types, Some("text/plain")).unwrap(),
            "A&B + C"
        );
        let json = fill_body_template(
            r#"{"value":"{value}"}"#,
            &params,
            &types,
            Some("Application/Vnd.Example+Json; charset=UTF-8"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["value"],
            "A&B + C"
        );
        assert!(
            fill_body_template("value={value}", &params, &types, Some("application/json")).is_err()
        );
        assert_eq!(
            fill_body_template("", &params, &types, Some("application/json")).unwrap(),
            ""
        );
        let form = fill_body_template(
            "value={value}&flag",
            &params,
            &types,
            Some("application/x-www-form-urlencoded; charset=utf-8"),
        )
        .unwrap();
        assert_eq!(
            url::form_urlencoded::parse(form.as_bytes()).collect::<Vec<_>>(),
            vec![
                ("value".into(), "A&B + C".into()),
                ("flag".into(), "".into())
            ]
        );
    }

    #[test]
    fn template_values_are_never_reinterpreted_as_placeholders() {
        let params = HashMap::from([("a".into(), "{b}".into()), ("b".into(), "{a}".into())]);
        // A recursive global replacement corrupts one side in either map order.
        assert_eq!(
            fill_template("{a}|{b}|{a}", &params).unwrap(),
            "{b}|{a}|{b}"
        );
        assert_eq!(
            fill_template(
                r#"{"nested":{"value":"{a}"},"literal":"{unknown}"} 끝 {b}"#,
                &params
            )
            .unwrap(),
            r#"{"nested":{"value":"{b}"},"literal":"{unknown}"} 끝 {a}"#
        );
        assert_eq!(
            fill_template("{{a}} {unfinished", &params).unwrap(),
            "{{b}} {unfinished"
        );
    }

    #[test]
    fn request_rendering_preserves_literal_slots_in_headers_bodies_and_encoded_urls() {
        let mut recipe = one_step_recipe(SideEffects::Write);
        let step = &mut recipe.current_mut().unwrap().steps[0];
        step.url_template.push_str("?first={a}&second={b}");
        step.headers_template = HashMap::from([
            ("x-first".into(), "{a}".into()),
            ("x-second".into(), "{b}".into()),
            ("content-type".into(), "text/plain".into()),
        ]);
        step.body_template = Some("{a}|{b}".into());
        let params = HashMap::from([("a".into(), "{b}".into()), ("b".into(), "{a}".into())]);
        let request = build_request(
            step,
            &params,
            &SessionContext::default(),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(request.headers["x-first"], "{b}");
        assert_eq!(request.headers["x-second"], "{a}");
        assert_eq!(request.body.as_deref(), Some("{b}|{a}"));
        let url = url::Url::parse(&request.url).unwrap();
        let pairs: HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs["first"], "{b}");
        assert_eq!(pairs["second"], "{a}");
        let json = fill_body_template(
            r#"{"value":"{a}"}"#,
            &params,
            &HashMap::new(),
            Some("application/json"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["value"],
            "{b}"
        );
        let form = fill_body_template(
            "value={a}",
            &params,
            &HashMap::new(),
            Some("application/x-www-form-urlencoded"),
        )
        .unwrap();
        assert_eq!(
            url::form_urlencoded::parse(form.as_bytes())
                .next()
                .unwrap()
                .1,
            "{b}"
        );
    }

    #[test]
    fn template_expansion_stops_at_the_output_budget() {
        let params: HashMap<String, String> = HashMap::from([("a".into(), "abcd".into())]);
        let calls = std::cell::Cell::new(0);
        let result = fill_template_with(&"{a}".repeat(100), &params, 8, |value| {
            calls.set(calls.get() + 1);
            value.to_owned()
        });
        assert!(result.unwrap_err().contains("Task Recipe limit"));
        assert_eq!(calls.get(), 3);
        assert_eq!(
            fill_template_with("{a}{a}", &params, 8, str::to_owned).unwrap(),
            "abcdabcd"
        );
        assert!(fill_template_with("literal too long", &params, 8, str::to_owned).is_err());
    }

    #[tokio::test]
    async fn non_renderable_later_numbers_cannot_dispatch_an_approved_write_prefix() {
        struct NoDispatch;
        #[async_trait::async_trait]
        impl StepTransport for NoDispatch {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, _: &TransportRequest) -> Result<TransportResponse, String> {
                panic!("all known numeric inputs must validate before the first write");
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        for schema in [TaskInputSchema::Number, TaskInputSchema::String] {
            for value in ["+42", "01", "1.", ".5", "1e999"] {
                let mut recipe = write_then_verify_recipe();
                recipe.shape.inputs.push(TaskInput {
                    name: "count".into(),
                    schema,
                    example_value: "42".into(),
                    source: TaskInputSource::TaskText,
                });
                let version = recipe.current_mut().unwrap();
                let mut later = version.steps[0].clone();
                later.id = "numeric-write".into();
                later.verify_with = Some("numeric-verify".into());
                later.body_template = Some(r#"{"count":"{count}"}"#.into());
                later.body_param_types = HashMap::from([("count".into(), TaskInputSchema::Number)]);
                later.param_sources = HashMap::from([(
                    "count".into(),
                    RecipeParamSource::TaskInput {
                        name: "count".into(),
                    },
                )]);
                let mut verify = version.steps[1].clone();
                verify.id = "numeric-verify".into();
                version.steps.extend([later, verify]);
                let mut inputs = RecipeRunInputs {
                    inputs: HashMap::from([
                        ("label".into(), "current-label".into()),
                        ("count".into(), "42".into()),
                    ]),
                    approved_write_steps: HashSet::from(["write".into(), "numeric-write".into()]),
                    timeout_ms: None,
                };
                assert!(validate_recipe_preflight(&recipe, &inputs).is_ok());
                inputs.inputs.insert("count".into(), value.into());
                let result = run_with(
                    &mut recipe,
                    vec![Box::new(NoDispatch)],
                    &grants,
                    &policy,
                    inputs,
                    None,
                    0,
                    None,
                )
                .await;
                assert!(!result.success);
                assert!(result.steps.is_empty());
                assert!(result.pending_approval.is_none());
                assert_eq!(result.failure.unwrap().class, FailureClass::InputMissing);
            }
        }
    }

    #[tokio::test]
    async fn invalid_later_header_or_url_inputs_cannot_dispatch_an_approved_write_prefix() {
        struct NoDispatch;
        #[async_trait::async_trait]
        impl StepTransport for NoDispatch {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, _: &TransportRequest) -> Result<TransportResponse, String> {
                panic!("known URL/header input must validate before the first write");
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        for (url_input, value) in [
            (false, "first\r\nsecond".into()),
            (false, "x".repeat(MAX_RECIPE_HEADER_BYTES)),
            (true, " ".repeat(MAX_RECIPE_URL_BYTES / 2)),
        ] {
            let mut recipe = write_then_verify_recipe();
            recipe.shape.inputs.push(TaskInput {
                name: "later".into(),
                schema: TaskInputSchema::String,
                example_value: "old-value".into(),
                source: TaskInputSource::TaskText,
            });
            let version = recipe.current_mut().unwrap();
            let mut later = version.steps[1].clone();
            later.id = "later-read".into();
            later.param_sources.insert(
                "later".into(),
                RecipeParamSource::TaskInput {
                    name: "later".into(),
                },
            );
            if url_input {
                later.url_template.push_str("?q={later}");
            } else {
                later
                    .headers_template
                    .insert("x-search".into(), "{later}{later}".into());
            }
            // An unknown session value must not require auth during preflight.
            later.param_sources.insert(
                "auth".into(),
                RecipeParamSource::SessionAuth {
                    scheme: "authorization".into(),
                },
            );
            later
                .headers_template
                .insert("authorization".into(), "{auth}".into());
            version.steps.push(later);
            let mut inputs = RecipeRunInputs {
                inputs: HashMap::from([
                    ("label".into(), "current-label".into()),
                    ("later".into(), "valid".into()),
                ]),
                approved_write_steps: HashSet::from(["write".into()]),
                timeout_ms: None,
            };
            assert!(validate_recipe_preflight(&recipe, &inputs).is_ok());
            inputs.inputs.insert("later".into(), value);
            let result = run_with(
                &mut recipe,
                vec![Box::new(NoDispatch)],
                &grants,
                &policy,
                inputs,
                None,
                0,
                None,
            )
            .await;
            assert!(!result.success);
            assert!(result.steps.is_empty());
            assert!(result.pending_approval.is_none());
            let failure = result.failure.unwrap();
            assert_eq!(failure.class, FailureClass::InputMissing);
            assert!(failure.detail.contains("later-read"));
        }
    }

    fn legacy_shared_input_recipe() -> TaskRecipe {
        let mut recipe = write_then_verify_recipe();
        recipe.shape.template = "set {label} and old-label".into();
        let verify = &mut recipe.current_mut().unwrap().steps[1];
        verify.url_template.push_str("?label={label}");
        verify.param_sources.insert(
            "label".into(),
            RecipeParamSource::TaskInput {
                name: "label".into(),
            },
        );
        recipe
    }

    #[test]
    fn legacy_shared_input_guard_preserves_unambiguous_replay() {
        let mut recipe = legacy_shared_input_recipe();
        let mut inputs = RecipeRunInputs {
            inputs: HashMap::from([("label".into(), "old-label".into())]),
            ..Default::default()
        };
        assert!(validate_recipe_preflight(&recipe, &inputs).is_ok());
        inputs.inputs.insert("label".into(), "current-label".into());
        let error = validate_recipe_preflight(&recipe, &inputs).unwrap_err();
        assert_eq!(error.0, FailureClass::InputMissing);
        assert!(error.1.contains("ambiguous shared inputs"));
        inputs.inputs.insert("label".into(), " old-label ".into());
        assert!(validate_recipe_preflight(&recipe, &inputs).is_err());
        inputs.inputs.insert("label".into(), "current-label".into());
        // Newly learned repeated slots retain the equality constraint, and
        // both requests can safely receive the one supplied current value.
        recipe.shape.template = "set {label} and {label}".into();
        assert!(validate_recipe_preflight(&recipe, &inputs).is_ok());
        // A leftover sample in a title with just one request binding is not
        // evidence of the historical coalescing bug.
        let mut single = write_then_verify_recipe();
        single.shape.template = "set {label} and old-label".into();
        assert!(validate_recipe_preflight(&single, &inputs).is_ok());
    }

    #[tokio::test]
    async fn legacy_shared_input_edits_cannot_dispatch_even_an_approved_write() {
        struct NoDispatch;
        #[async_trait::async_trait]
        impl StepTransport for NoDispatch {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, _: &TransportRequest) -> Result<TransportResponse, String> {
                panic!("ambiguous shared input must fail before any request");
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        for in_description in [false, true] {
            let mut recipe = legacy_shared_input_recipe();
            if in_description {
                recipe.shape.template = "set {label}".into();
                recipe.shape.description_template = Some("also old-label".into());
            }
            let result = run_with(
                &mut recipe,
                vec![Box::new(NoDispatch)],
                &grants,
                &policy,
                RecipeRunInputs {
                    inputs: HashMap::from([("label".into(), "current-label".into())]),
                    approved_write_steps: HashSet::from(["write".into()]),
                    timeout_ms: None,
                },
                None,
                0,
                None,
            )
            .await;
            assert!(!result.success);
            assert!(result.steps.is_empty());
            assert!(result.pending_approval.is_none());
            let failure = result.failure.unwrap();
            assert_eq!(failure.class, FailureClass::InputMissing);
            assert!(failure.detail.contains("ambiguous shared inputs"));
        }
    }

    #[test]
    fn malformed_declared_json_is_rejected_before_an_earlier_write_can_run() {
        let mut recipe = write_then_verify_recipe();
        let version = recipe.current_mut().unwrap();
        let mut malformed = version.steps[0].clone();
        malformed.id = "malformed-later-write".into();
        malformed.verify_with = None;
        malformed.body_template = Some("label={label}".into());
        malformed
            .headers_template
            .insert("content-type".into(), "application/json".into());
        version.steps.push(malformed);
        let error = validate_recipe_preflight(
            &recipe,
            &RecipeRunInputs {
                inputs: HashMap::from([("label".into(), "current-label".into())]),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.0, FailureClass::SchemaDrift);
        assert!(error.1.contains("malformed-later-write"));
        assert!(error.1.contains("invalid JSON"));
    }

    #[test]
    fn unresolved_downstream_tokens_fail_whole_graph_preflight() {
        let mut recipe = write_then_verify_recipe();
        let version = recipe.current_mut().unwrap();
        let mut later = version.steps[1].clone();
        later.id = "unresolved-later-read".into();
        later.url_template.push_str("?sig={sig}");
        later.param_sources.insert(
            "sig".into(),
            RecipeParamSource::Literal {
                value: String::new(),
                volatile: true,
            },
        );
        version.steps.push(later);
        let error = validate_recipe_preflight(
            &recipe,
            &RecipeRunInputs {
                inputs: HashMap::from([("label".into(), "current-label".into())]),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.0, FailureClass::InputMissing);
        assert!(error.1.contains("unresolved-later-read"));
    }

    #[tokio::test]
    async fn invalid_later_body_cannot_dispatch_an_approved_write_prefix() {
        struct NoDispatch;
        #[async_trait::async_trait]
        impl StepTransport for NoDispatch {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, _: &TransportRequest) -> Result<TransportResponse, String> {
                panic!("invalid later body must be rejected before the first mutation");
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        for unresolved in [false, true] {
            let mut recipe = write_then_verify_recipe();
            let version = recipe.current_mut().unwrap();
            let mut later = version.steps[1].clone();
            later.id = "invalid-later".into();
            if unresolved {
                later.url_template.push_str("?nonce={nonce}");
                later.param_sources.insert(
                    "nonce".into(),
                    RecipeParamSource::Literal {
                        value: String::new(),
                        volatile: true,
                    },
                );
            } else {
                later.body_template = Some("name=not-json".into());
                later
                    .headers_template
                    .insert("content-type".into(), "application/json".into());
            }
            version.steps.push(later);
            let result = run_with(
                &mut recipe,
                vec![Box::new(NoDispatch)],
                &grants,
                &policy,
                RecipeRunInputs {
                    inputs: HashMap::from([("label".into(), "current-label".into())]),
                    approved_write_steps: HashSet::from(["write".into()]),
                    timeout_ms: None,
                },
                None,
                0,
                None,
            )
            .await;
            assert!(!result.success);
            assert!(result.steps.is_empty());
            assert!(result.pending_approval.is_none());
            assert_eq!(
                result.failure.unwrap().class,
                if unresolved {
                    FailureClass::InputMissing
                } else {
                    FailureClass::SchemaDrift
                }
            );
        }
    }

    #[test]
    fn json_body_parameters_are_escaped_and_restore_scalar_types() {
        let params = HashMap::from([
            ("label".into(), "quoted \"value\" & braces {ok}".into()),
            ("count".into(), "42.5".into()),
            ("enabled".into(), "true".into()),
        ]);
        let types = HashMap::from([
            ("label".into(), TaskInputSchema::String),
            ("count".into(), TaskInputSchema::Number),
            ("enabled".into(), TaskInputSchema::Boolean),
        ]);
        let filled = fill_body_template(
            r#"{"label":"{label}","count":"{count}","enabled":"{enabled}","stable":"42.5"}"#,
            &params,
            &types,
            Some("application/json"),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&filled).unwrap();

        assert_eq!(json["label"], "quoted \"value\" & braces {ok}");
        assert_eq!(json["count"], 42.5);
        assert_eq!(json["enabled"], true);
        assert_eq!(json["stable"], "42.5");
    }

    #[test]
    fn graphql_document_braces_remain_stable_while_variables_are_filled() {
        let params = HashMap::from([("id".into(), "item-42".into())]);
        let types = HashMap::from([("id".into(), TaskInputSchema::String)]);
        let filled = fill_body_template(
            r#"{"query":"query Item($id: ID!) { item(id: $id) { id title } }","variables":{"id":"{id}"},"literal":"{stable}"}"#,
            &params,
            &types,
            None,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&filled).unwrap();

        assert_eq!(
            json["query"],
            "query Item($id: ID!) { item(id: $id) { id title } }"
        );
        assert_eq!(json["variables"]["id"], "item-42");
        assert_eq!(json["literal"], "{stable}");
    }

    #[test]
    fn form_body_parameters_are_encoded_without_changing_stable_pairs() {
        let params = HashMap::from([("query".into(), "A&B + C".into())]);
        let filled = fill_body_template(
            "query={query}&stable=A%26B",
            &params,
            &HashMap::new(),
            Some("application/x-www-form-urlencoded"),
        )
        .unwrap();
        let pairs: Vec<_> = url::form_urlencoded::parse(filled.as_bytes()).collect();

        assert_eq!(pairs[0].0.as_ref(), "query");
        assert_eq!(pairs[0].1.as_ref(), "A&B + C");
        assert_eq!(pairs[1].0.as_ref(), "stable");
        assert_eq!(pairs[1].1.as_ref(), "A&B");
    }

    #[test]
    fn response_cookies_merge_with_session_and_remain_url_scoped() {
        let mut jar = CookieJar::default();
        jar.absorb(
            "https://fixture.example/api/private/bootstrap",
            &HashMap::from([(
                "set-cookie".into(),
                "csrf=fresh; Path=/api/private; Secure; HttpOnly".into(),
            )]),
        )
        .unwrap();
        let session = SessionContext {
            cookie_header_values: vec![super::super::types::SessionCookie {
                name: "session".into(),
                value: "captured".into(),
            }],
            ..Default::default()
        };
        let mut private_request = TransportRequest {
            method: "GET".into(),
            url: "https://fixture.example/api/private/items".into(),
            headers: HashMap::new(),
            body: None,
            timeout: Duration::from_secs(1),
        };
        let mut private_session = session.clone();
        jar.merge_session(&mut private_session, &private_request.url)
            .unwrap();
        apply_cookie_header(&mut private_request, &private_session).unwrap();
        assert_eq!(
            private_request.headers.get("cookie").map(String::as_str),
            Some("session=captured; csrf=fresh")
        );

        let mut public_request = TransportRequest {
            url: "https://fixture.example/public".into(),
            ..private_request
        };
        let mut public_session = session;
        jar.merge_session(&mut public_session, &public_request.url)
            .unwrap();
        apply_cookie_header(&mut public_request, &public_session).unwrap();
        assert_eq!(
            public_request.headers.get("cookie").map(String::as_str),
            Some("session=captured")
        );
    }

    fn captured_cookie(name: &str, value: &str, path: &str) -> CookieWithMetadata {
        CookieWithMetadata {
            name: name.into(),
            value: value.into(),
            path: path.into(),
            domain: "fixture.example".into(),
            secure: true,
            http_only: true,
            same_site: SameSite::Lax,
            expires: None,
        }
    }

    #[test]
    fn rotated_and_deleted_cookies_replace_only_their_exact_identity() {
        let captured = SessionContext {
            cookie_metadata: vec![
                captured_cookie("sid", "app-old", "/app"),
                captured_cookie("sid", "root-old", "/"),
            ],
            ..Default::default()
        };
        let url = "https://fixture.example/app/items";
        let mut jar = CookieJar::default();
        jar.absorb(
            url,
            &HashMap::from([(
                "set-cookie".into(),
                "sid=app-new; Domain=fixture.example; Path=/app; Secure".into(),
            )]),
        )
        .unwrap();
        let mut session = captured.clone();
        jar.merge_session(&mut session, url).unwrap();
        assert_eq!(
            session.cookie_header_string().as_deref(),
            Some("sid=app-new; sid=root-old")
        );
        jar.absorb(
            url,
            &HashMap::from([(
                "set-cookie".into(),
                "sid=; Path=/app; Max-Age=0; Secure".into(),
            )]),
        )
        .unwrap();
        // Each next step reloads the old durable capture. Deletions must still
        // shadow it, without deleting the distinct root-path cookie.
        let mut session = captured.clone();
        jar.merge_session(&mut session, url).unwrap();
        assert_eq!(
            session.cookie_header_string().as_deref(),
            Some("sid=root-old")
        );
        jar.absorb(
            url,
            &HashMap::from([(
                "set-cookie".into(),
                "sid=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Secure".into(),
            )]),
        )
        .unwrap();
        let mut session = captured;
        jar.merge_session(&mut session, url).unwrap();
        assert!(session.cookie_header_values.is_empty());
        assert!(session.cookies.is_empty());
    }

    #[test]
    fn expired_rotation_does_not_resurrect_an_older_captured_cookie() {
        let mut expired = captured_cookie("sid", "expired-rotation", "/");
        expired.expires = Some(chrono::Utc::now().timestamp());
        let jar = CookieJar {
            response_cookies: vec![expired],
        };
        let mut session = SessionContext {
            cookie_metadata: vec![captured_cookie("sid", "old-capture", "/")],
            ..Default::default()
        };
        jar.merge_session(&mut session, "https://fixture.example/app")
            .unwrap();
        assert!(session.cookie_header_values.is_empty());
        assert!(session.cookies.is_empty());
    }

    #[test]
    fn legacy_cookie_tombstones_do_not_cross_request_path_or_origin() {
        let mut jar = CookieJar::default();
        jar.absorb(
            "https://fixture.example/private/init",
            &HashMap::from([(
                "set-cookie".into(),
                "sid=; Path=/private; Max-Age=0; Secure".into(),
            )]),
        )
        .unwrap();
        for (url, expected) in [
            ("https://fixture.example/private/items", ""),
            ("https://fixture.example/public", "sid=captured"),
            ("https://other.example/private/items", "sid=captured"),
        ] {
            let mut session = SessionContext {
                cookies: HashMap::from([("sid".into(), "captured".into())]),
                ..Default::default()
            };
            jar.merge_session(&mut session, url).unwrap();
            assert_eq!(session.cookie_header_string().unwrap_or_default(), expected);
        }
    }

    #[test]
    fn cookie_expiry_and_max_age_precedence_are_independent_of_attribute_order() {
        let url = url::Url::parse("https://fixture.example/app").unwrap();
        for date in [
            "Thu, 01 Jan 1970 00:00:00 GMT",
            "Thu, 01-Jan-1970 00:00:00 GMT",
            "Thursday, 01-Jan-70 00:00:00 GMT",
            "Thu Jan  1 00:00:00 1970",
        ] {
            assert_eq!(parse_cookie_expiry(date), Some(0), "{date}");
        }
        for attrs in [
            "Max-Age=3600; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
            "Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=3600",
        ] {
            assert!(
                parse_set_cookie(&format!("sid=new; {attrs}"), &url)
                    .unwrap()
                    .expires
                    .unwrap()
                    > chrono::Utc::now().timestamp()
            );
        }
        for attrs in [
            "Max-Age=0; Expires=Tue, 19 Jan 2038 03:14:07 GMT",
            "Expires=Tue, 19 Jan 2038 03:14:07 GMT; Max-Age=0",
        ] {
            assert!(
                parse_set_cookie(&format!("sid=; {attrs}"), &url)
                    .unwrap()
                    .expires
                    .unwrap()
                    <= chrono::Utc::now().timestamp()
            );
        }
    }

    #[test]
    fn cookie_limit_cannot_silently_drop_a_deletion() {
        let mut jar = CookieJar {
            response_cookies: (0..MAX_RECIPE_COOKIES)
                .map(|index| captured_cookie(&format!("c{index}"), "value", "/"))
                .collect(),
        };
        assert!(jar
            .absorb(
                "https://fixture.example/app",
                &HashMap::from([("set-cookie".into(), "sid=; Max-Age=0".into())])
            )
            .is_err());
        assert_eq!(jar.response_cookies.len(), MAX_RECIPE_COOKIES);
    }

    #[test]
    fn concrete_url_lookup_resolves_path_scoped_cookie_parameters() {
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        let step = &mut recipe.current_mut().unwrap().steps[0];
        step.url_template = "https://fixture.example/tenants/{tenant}/items".into();
        step.param_sources = HashMap::from([
            (
                "tenant".into(),
                RecipeParamSource::TaskInput {
                    name: "tenant".into(),
                },
            ),
            (
                "csrf".into(),
                RecipeParamSource::SessionAuth {
                    scheme: "cookie:SID".into(),
                },
            ),
        ]);
        step.headers_template
            .insert("x-csrf-token".into(), "{csrf}".into());
        let lookup = |_: &str, url: &str| {
            Some(SessionContext {
                cookie_metadata: if url.contains("/tenants/blue/") {
                    vec![
                        captured_cookie("sid", "wrong-case", "/tenants/blue"),
                        captured_cookie("SID", "path-token", "/tenants/blue"),
                    ]
                } else {
                    vec![]
                },
                ..Default::default()
            })
        };
        let (params, session) = resolve_step_context(
            step,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::from([("tenant".into(), "blue".into())]),
            &CookieJar::default(),
            &lookup,
            false,
        )
        .unwrap();
        let mut request = build_request(step, &params, &session, Duration::from_secs(1)).unwrap();
        apply_cookie_header(&mut request, &session).unwrap();
        assert_eq!(request.url, "https://fixture.example/tenants/blue/items");
        assert_eq!(request.headers["x-csrf-token"], "path-token");
        assert_eq!(request.headers["cookie"], "sid=wrong-case; SID=path-token");
    }

    #[tokio::test]
    async fn missing_cookie_sessions_cannot_silently_replay_anonymously() {
        struct NoAnonymousRequest;
        #[async_trait::async_trait]
        impl StepTransport for NoAnonymousRequest {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, _: &TransportRequest) -> Result<TransportResponse, String> {
                panic!("missing required cookies must fail before anonymous HTTP");
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        for legacy in [false, true] {
            let mut recipe = one_step_recipe(SideEffects::ReadOnly);
            let version = recipe.current_mut().unwrap();
            version.auth.requires_session = true;
            version.auth.origins_needing_auth = version.origins.clone();
            if !legacy {
                version.steps[0].param_sources.insert(
                    "__session_cookies".into(),
                    RecipeParamSource::SessionAuth {
                        scheme: "cookie_header".into(),
                    },
                );
            }
            let result = run_with(
                &mut recipe,
                vec![Box::new(NoAnonymousRequest)],
                &grants,
                &policy,
                RecipeRunInputs::default(),
                None,
                0,
                None,
            )
            .await;
            assert!(!result.success);
            assert!(result.steps.is_empty());
            assert_eq!(result.fallback.unwrap().class, FailureClass::Auth);
            let result = RecipeRunner {
                transports: vec![Box::new(MockTransport::new(
                    Transport::Reqwest,
                    vec![Ok(response(200, "{}"))],
                ))],
                grants: &grants,
                origin_policy: &policy,
                can_continue: None,
                session_lookup: &|_, _| {
                    Some(SessionContext {
                        cookies: HashMap::from([("sid".into(), "current-session".into())]),
                        ..Default::default()
                    })
                },
                auth_healer: None,
                max_auth_heals: 0,
                step_feedback: None,
                observer: None,
            }
            .run(&mut recipe, &RecipeRunInputs::default())
            .await;
            assert!(
                result.success,
                "cookie-dependent recipe must remain runnable with a current session: {result:?}"
            );
        }
    }

    #[test]
    fn refreshed_origin_replaces_in_run_rotations_and_deletions_only_in_its_scope() {
        let mut jar = CookieJar::default();
        jar.absorb(
            "https://fixture.example/app",
            &HashMap::from([(
                "set-cookie".into(),
                "sid=rotation; Path=/; Secure\ncsrf=; Max-Age=0; Path=/; Secure".into(),
            )]),
        )
        .unwrap();
        jar.absorb(
            "https://other.example/app",
            &HashMap::from([("set-cookie".into(), "other=kept; Path=/; Secure".into())]),
        )
        .unwrap();
        jar.forget_refreshed_origin("https://fixture.example");
        let mut session = SessionContext {
            cookie_metadata: vec![
                captured_cookie("sid", "healed", "/"),
                captured_cookie("csrf", "healed-csrf", "/"),
            ],
            ..Default::default()
        };
        jar.merge_session(&mut session, "https://fixture.example/app")
            .unwrap();
        assert_eq!(
            session.cookie_header_string().as_deref(),
            Some("sid=healed; csrf=healed-csrf")
        );
        assert_eq!(jar.response_cookies.len(), 1);
        assert_eq!(jar.response_cookies[0].name, "other");
    }

    #[tokio::test]
    async fn replay_resolves_concrete_cookie_scope_and_replaces_rotation_after_auth_heal() {
        use std::sync::{atomic::AtomicBool, Arc};
        struct SessionHealer(Arc<AtomicBool>);
        #[async_trait::async_trait]
        impl AuthHealer for SessionHealer {
            async fn heal(&self, origin: &str) -> Result<bool, String> {
                assert_eq!(origin, "https://fixture.example");
                self.0.store(true, Ordering::SeqCst);
                Ok(true)
            }
        }
        struct CookieServer(Arc<AtomicUsize>);
        #[async_trait::async_trait]
        impl StepTransport for CookieServer {
            fn kind(&self) -> Transport {
                Transport::Reqwest
            }
            async fn send(&self, request: &TransportRequest) -> Result<TransportResponse, String> {
                assert_eq!(request.method, "GET");
                let attempt = self.0.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    assert!(request.url.ends_with("/bootstrap"));
                    return Ok(TransportResponse {
                        status: 200,
                        body: "{}".into(),
                        headers: HashMap::from([(
                            "set-cookie".into(),
                            "SID=rotation; Path=/tenants/blue; Secure".into(),
                        )]),
                    });
                }
                assert_eq!(request.url, "https://fixture.example/tenants/blue/items");
                let expected = match attempt {
                    1 => "rotation",
                    2 => "healed",
                    _ => panic!("unexpected retry"),
                };
                assert_eq!(request.headers["cookie"], format!("SID={expected}"));
                assert_eq!(request.headers["x-csrf-token"], expected);
                Ok(response(
                    if attempt == 1 { 401 } else { 200 },
                    if attempt == 1 {
                        "expired"
                    } else {
                        r#"{"value":"current"}"#
                    },
                ))
            }
        }
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        recipe.shape.inputs.push(TaskInput {
            name: "tenant".into(),
            schema: TaskInputSchema::String,
            example_value: "blue".into(),
            source: TaskInputSource::TaskText,
        });
        let version = recipe.current_mut().unwrap();
        version.steps[0].url_template = "https://fixture.example/bootstrap".into();
        let mut target = version.steps[0].clone();
        target.id = "target".into();
        target.url_template = "https://fixture.example/tenants/{tenant}/items".into();
        target.param_sources = HashMap::from([
            (
                "tenant".into(),
                RecipeParamSource::TaskInput {
                    name: "tenant".into(),
                },
            ),
            (
                "csrf".into(),
                RecipeParamSource::SessionAuth {
                    scheme: "cookie:SID".into(),
                },
            ),
        ]);
        target
            .headers_template
            .insert("x-csrf-token".into(), "{csrf}".into());
        version.steps.push(target);
        version.answer_spec.push(AnswerField {
            field: "value".into(),
            step_id: "target".into(),
            extractor: Extractor::JsonPath {
                path: "$.value".into(),
            },
        });
        let refreshed = Arc::new(AtomicBool::new(false));
        let healer = SessionHealer(Arc::clone(&refreshed));
        let lookup = |_: &str, request_url: &str| {
            let url = url::Url::parse(request_url).unwrap();
            Some(SessionContext {
                cookie_metadata: filter_cookies_for_url(
                    &[captured_cookie(
                        "SID",
                        if refreshed.load(Ordering::SeqCst) {
                            "healed"
                        } else {
                            "captured"
                        },
                        "/tenants/blue",
                    )],
                    &url,
                ),
                ..Default::default()
            })
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let result = RecipeRunner {
            transports: vec![Box::new(CookieServer(Arc::clone(&calls)))],
            grants: &grants,
            origin_policy: &policy,
            can_continue: None,
            session_lookup: &lookup,
            auth_healer: Some(&healer),
            max_auth_heals: 1,
            step_feedback: None,
            observer: None,
        }
        .run(
            &mut recipe,
            &RecipeRunInputs {
                inputs: HashMap::from([("tenant".into(), "blue".into())]),
                ..Default::default()
            },
        )
        .await;
        assert!(result.success, "{result:?}");
        assert_eq!(result.auth_heals, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(result.answer["value"], "current");
    }

    #[test]
    fn missing_structured_body_parameter_fails_closed() {
        let error = fill_body_template(
            r#"{"value":"{missing}"}"#,
            &HashMap::new(),
            &HashMap::from([("missing".into(), TaskInputSchema::String)]),
            None,
        )
        .unwrap_err();

        assert!(error.contains("missing"));
    }

    #[test]
    fn compiled_recipe_contains_no_session_value() {
        let mut recipe = one_step_recipe(SideEffects::ReadOnly);
        recipe.current_mut().unwrap().steps[0].param_sources.insert(
            "authorization".into(),
            RecipeParamSource::SessionAuth {
                scheme: "authorization".into(),
            },
        );
        let encoded = serde_json::to_string(&recipe).unwrap();
        assert!(!encoded.contains("Bearer secret-token"));
        assert!(!encoded.contains("session-cookie-value"));
    }

    #[test]
    fn data_flow_fixture_stays_shape_only() {
        let flow = RecipeDataFlow {
            id: "flow".into(),
            source_step: "search".into(),
            extractor: Extractor::JsonPath {
                path: "$.id".into(),
            },
            target_step: "detail".into(),
            target_param: "id".into(),
            confidence: 1.0,
            inference: InferenceMethod::AutoMatch,
        };
        let encoded = serde_json::to_string(&flow).unwrap();
        assert!(!encoded.contains("concrete-user-value"));
    }

    #[test]
    fn fallback_summary_exposes_only_counts_not_response_keys_or_values() {
        let response = StoredResponse {
            json: Some(serde_json::json!({
                "access_token": "response-secret-value",
                "profile": {"email": "private@example.test"},
            })),
            text: r#"{"access_token":"response-secret-value"}"#.into(),
            headers: HashMap::new(),
        };

        let summary = safe_replayed_summary(&response);

        assert!(summary.contains("2 field(s)"));
        assert!(!summary.contains("access_token"));
        assert!(!summary.contains("profile"));
        assert!(!summary.contains("response-secret-value"));
        assert!(!summary.contains("private@example.test"));
    }

    #[test]
    fn write_verification_cannot_succeed_from_an_unchanged_identifier_alone() {
        let writes = vec![vec![
            VerificationExpectation {
                value: "item-1234".into(),
                schema: TaskInputSchema::String,
            },
            VerificationExpectation {
                value: "new-label".into(),
                schema: TaskInputSchema::String,
            },
        ]];
        let stale = serde_json::json!({"id": "item-1234", "label": "old-label"});
        assert!(!response_reflects_writes(
            &stale.to_string(),
            Some(&stale),
            &writes
        ));
        assert!(!response_reflects_writes(
            "item-1234 old-label",
            None,
            &writes
        ));
        let updated = serde_json::json!({"id": "item-1234", "label": "new-label"});
        assert!(response_reflects_writes(
            &updated.to_string(),
            Some(&updated),
            &writes
        ));
    }

    #[test]
    fn json_write_verification_is_exact_and_type_aware() {
        let string_write = vec![vec![VerificationExpectation {
            value: "true".into(),
            schema: TaskInputSchema::String,
        }]];
        let boolean_write = vec![vec![VerificationExpectation {
            value: "TRUE".into(),
            schema: TaskInputSchema::Boolean,
        }]];
        let body = r#"{"label":"prefix-true-suffix","enabled":true}"#;
        let json: serde_json::Value = serde_json::from_str(body).unwrap();

        assert!(!response_reflects_writes(body, Some(&json), &string_write));
        assert!(response_reflects_writes(body, Some(&json), &boolean_write));
    }

    #[test]
    fn json_write_verification_keeps_record_evidence_together() {
        let writes = vec![vec![
            VerificationExpectation {
                value: "target".into(),
                schema: TaskInputSchema::String,
            },
            VerificationExpectation {
                value: "new-label".into(),
                schema: TaskInputSchema::String,
            },
        ]];
        for stale in [
            serde_json::json!({"items": [
                {"id": "target", "label": "old-label"},
                {"id": "other", "label": "new-label"}
            ]}),
            serde_json::json!({"items": {
                "first": {"id": "target", "label": "old-label"},
                "second": {"id": "other", "label": "new-label"}
            }}),
        ] {
            assert!(!response_reflects_writes(
                &stale.to_string(),
                Some(&stale),
                &writes
            ));
        }
        for updated in [
            serde_json::json!({"items": [{"id": "target", "label": "new-label"}]}),
            serde_json::json!({"id": "target", "profile": {"label": "new-label"}}),
            serde_json::json!({"id": "target", "labels": ["old-label", "new-label"]}),
        ] {
            assert!(response_reflects_writes(
                &updated.to_string(),
                Some(&updated),
                &writes
            ));
        }
        let mut two_writes = writes;
        two_writes.push(vec![
            VerificationExpectation {
                value: "other".into(),
                schema: TaskInputSchema::String,
            },
            VerificationExpectation {
                value: "second-label".into(),
                schema: TaskInputSchema::String,
            },
        ]);
        let both_updated = serde_json::json!({"items": [
            {"id": "target", "label": "new-label"},
            {"id": "other", "label": "second-label"}
        ]});
        assert!(response_reflects_writes(
            &both_updated.to_string(),
            Some(&both_updated),
            &two_writes
        ));
    }

    #[tokio::test]
    async fn unrelated_row_cannot_turn_an_unverified_write_into_success() {
        let temp = tempfile::tempdir().unwrap();
        let grants = ReplayGrantStore::open(temp.path());
        let policy = OriginPolicyStore::open(temp.path());
        let mut recipe = write_then_verify_recipe();
        recipe.shape.inputs.push(TaskInput {
            name: "id".into(),
            schema: TaskInputSchema::String,
            example_value: "target".into(),
            source: TaskInputSource::TaskText,
        });
        let write = &mut recipe.current_mut().unwrap().steps[0];
        write.body_template = Some(r#"{"id":"{id}","label":"{label}"}"#.into());
        write
            .body_param_types
            .insert("id".into(), TaskInputSchema::String);
        write.param_sources.insert(
            "id".into(),
            RecipeParamSource::TaskInput { name: "id".into() },
        );
        let result = run_with(
            &mut recipe,
            vec![Box::new(MockTransport::new(
                Transport::Reqwest,
                vec![
                    Ok(response(200, "{}")),
                    Ok(response(
                        200,
                        r#"{"items":[{"id":"target","label":"old"},{"id":"other","label":"new"}]}"#,
                    )),
                ],
            ))],
            &grants,
            &policy,
            RecipeRunInputs {
                inputs: HashMap::from([
                    ("id".into(), "target".into()),
                    ("label".into(), "new".into()),
                ]),
                timeout_ms: None,
                approved_write_steps: HashSet::from(["write".into()]),
            },
            None,
            0,
            None,
        )
        .await;
        assert_eq!(result.steps.len(), 2);
        assert!(!result.success);
        assert!(result.write_outcome_uncertain(&recipe));
        assert!(result.fallback.is_none());
    }

    #[test]
    fn json_write_verification_preserves_large_integer_identity() {
        for (expected, wrong) in [
            ("9007199254740993", "9007199254740992"),
            ("18446744073709551615", "18446744073709551614"),
            ("-9223372036854775808", "-9223372036854775807"),
        ] {
            let writes = vec![vec![VerificationExpectation {
                value: expected.into(),
                schema: TaskInputSchema::Number,
            }]];
            for (actual, matches) in [(expected, true), (wrong, false)] {
                let body = format!("{{\"id\":{actual}}}");
                let json: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(
                    response_reflects_writes(&body, Some(&json), &writes),
                    matches
                );
            }
        }
    }

    #[test]
    fn json_verification_bounds_pending_nodes_before_expanding_wide_documents() {
        let document =
            serde_json::Value::Array(vec![serde_json::Value::Null; MAX_VERIFICATION_JSON_NODES]);
        assert!(index_json_records(&document).is_none());
    }

    #[test]
    fn json_write_verification_normalizes_finite_numbers() {
        let writes = vec![vec![VerificationExpectation {
            value: "01.0".into(),
            schema: TaskInputSchema::Number,
        }]];
        let body = r#"{"count":1}"#;
        let json: serde_json::Value = serde_json::from_str(body).unwrap();

        assert!(response_reflects_writes(body, Some(&json), &writes));
        assert_eq!(number_key("+1.00e2"), number_key("100"));
        assert_eq!(number_key("-0.00"), number_key("0"));
        for invalid in ["NaN", "inf", "1.2.3", "1e999999999999999999999999"] {
            assert!(number_key(invalid).is_none());
        }
    }

    #[test]
    fn text_write_verification_finds_overlapping_values_in_one_scan() {
        let writes = vec![
            vec![VerificationExpectation {
                value: "a".into(),
                schema: TaskInputSchema::String,
            }],
            vec![VerificationExpectation {
                value: "ab".into(),
                schema: TaskInputSchema::String,
            }],
        ];

        assert!(response_reflects_writes("a ab", None, &writes));
        assert!(!response_reflects_writes(
            "alphabet",
            None,
            &[vec![VerificationExpectation {
                value: "alpha".into(),
                schema: TaskInputSchema::String,
            }]]
        ));
    }

    #[test]
    fn text_write_verification_bounds_matches_and_stops_after_complete_evidence() {
        let writes = vec![vec![VerificationExpectation {
            value: "a".into(),
            schema: TaskInputSchema::String,
        }]];
        let repeated = "a".repeat(MAX_VERIFICATION_TEXT_MATCHES + 1);
        assert!(!response_reflects_writes(
            &format!("{repeated} a "),
            None,
            &writes
        ));
        assert!(response_reflects_writes(
            &format!("a {repeated}"),
            None,
            &writes
        ));
    }
}
