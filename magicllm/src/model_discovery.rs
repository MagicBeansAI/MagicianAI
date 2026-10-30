//! Runtime model availability.
//!
//! The authored catalog (`llm.router.catalog`) stays the source of metadata
//! a model-list API does not return: prices, request-shape `wire` facts,
//! limits and capabilities. Whether a model is *offered right now* is learned
//! at runtime, per source, from:
//!
//! - provider model-list endpoints where a key is configured
//!   ([`refresh_provider_listings`]);
//! - harness CLIs' own listings (recorded by the host with [`record_listing`]);
//! - a CLI or provider error that rejects a model by name
//!   ([`record_rejection`]); a rejection that names the accepted model ids is
//!   recorded as a listing too.
//!
//! A source is the provider of a catalog connection (`openai`,
//! `harness-agy`, ...), or `provider@host` when the connection names its own
//! base URL ([`source_key`]). Listings are cached with their fetch time and
//! refreshed in the background by the host; a failed refresh keeps the last
//! good listing. Nothing here runs on a request's hot path except reads.
//!
//! Effects, all derived on read:
//! - a catalogued model its source no longer offers is
//!   [`ModelAvailability::Unavailable`] and pickers hide it;
//! - a model the source offers but the catalog lacks is *uncatalogued*
//!   ([`uncatalogued_models`]): default request shape, price unknown;
//! - [`publish_drift`] keeps one [`SourceDrift`] per drifted source for the
//!   host's health notice.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock, RwLock,
    },
    time::Duration,
};

use serde::Serialize;
use serde_json::Value;

use crate::{
    catalog::{ModelCatalog, ProviderConnection},
    LLMProviderKind,
};

/// The opaque CLI default: always available, never in a listing.
pub const DEFAULT_MODEL: &str = "default";
/// A rejection without a later listing stops hiding the model after this.
pub const REJECTION_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// A failed refresh is retried after this, even when the TTL is longer.
pub const FAILURE_RETRY: Duration = Duration::from_secs(5 * 60);
/// Provider listing request bound.
pub const LISTING_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_ID_LEN: usize = 128;
const MAX_LISTED_MODELS: usize = 2_000;
const MAX_SCANNED_ERROR_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingOrigin {
    /// A provider's model-list endpoint.
    ProviderApi,
    /// A harness CLI's own model listing command.
    CliListing,
    /// A harness CLI's own model cache file.
    CliCache,
    /// The accepted ids a CLI or provider named while rejecting a model.
    Rejection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    /// The source offers it now (or it is the CLI's opaque default).
    Available,
    /// The source's current listing lacks it, or the source rejected it.
    Unavailable,
    /// No listing is known for the source.
    Unknown,
}

/// Additive API projection for one catalogued model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AvailabilityView {
    pub state: ModelAvailability,
    pub source: String,
    pub catalogued: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at_ms: Option<i64>,
}

/// Public status of one source. No keys, URLs or raw error bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceStatus {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<ListingOrigin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at_ms: Option<i64>,
    pub model_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub rejected: Vec<String>,
}

/// One drifted source: catalogued models it no longer offers and offered
/// models the catalog lacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceDrift {
    pub source: String,
    /// Catalog model ids (`connection/model` keys).
    pub missing: Vec<String>,
    /// The physical names of `missing`, as the source would be asked for them.
    pub missing_models: Vec<String>,
    /// Offered model names with no catalog entry.
    pub uncatalogued: Vec<String>,
}

impl SourceDrift {
    /// Stable identity of this drift, for notice de-duplication.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}|{}|{}",
            self.source,
            self.missing.join(","),
            self.uncatalogued.join(",")
        )
    }
}

#[derive(Debug, Clone)]
struct Listing {
    models: BTreeSet<String>,
    origin: ListingOrigin,
    fetched_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
struct SourceState {
    listing: Option<Listing>,
    /// Normalized model -> rejected at (ms).
    rejected: BTreeMap<String, (String, i64)>,
    last_attempt_ms: Option<i64>,
    last_error: Option<String>,
    /// A rejection asks for an early refresh.
    stale: bool,
}

/// The availability registry. The process uses one global instance through
/// the free functions; tests construct their own.
#[derive(Debug, Default)]
pub struct ModelDiscovery {
    sources: BTreeMap<String, SourceState>,
}

fn normalize(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    let lower = lower.strip_prefix("models/").unwrap_or(&lower);
    lower.strip_suffix(":latest").unwrap_or(lower).to_string()
}

/// Whether a listed id is the catalogued name: equal, or its dated snapshot
/// (`name-20250929`, `name-2025-09-29`) or `name-latest`. Providers such as
/// Anthropic list only dated ids while accepting the alias.
fn same_model(listed: &str, name: &str) -> bool {
    if listed == name {
        return true;
    }
    let Some(suffix) = listed
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let bytes = suffix.as_bytes();
    suffix == "latest"
        || (bytes.len() == 8 && bytes.iter().all(u8::is_ascii_digit))
        || (bytes.len() == 10
            && bytes.iter().enumerate().all(|(index, byte)| {
                if index == 4 || index == 7 {
                    *byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            }))
}

fn is_default(model: &str) -> bool {
    let model = model.trim();
    model.is_empty() || model.eq_ignore_ascii_case(DEFAULT_MODEL)
}

impl ModelDiscovery {
    /// Record a successful listing. An empty listing is a failure: it keeps
    /// the last good list. Returns whether the offered set changed.
    pub fn record_listing<I, S>(
        &mut self,
        source: &str,
        models: I,
        origin: ListingOrigin,
        now_ms: i64,
    ) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let models: BTreeSet<String> = models
            .into_iter()
            .filter_map(|model| id_token(model.as_ref()))
            .take(MAX_LISTED_MODELS)
            .collect();
        if models.is_empty() {
            self.record_failure(source, "the source listed no models", now_ms);
            return false;
        }
        let state = self.sources.entry(source.to_string()).or_default();
        let listed: BTreeSet<String> = models.iter().map(|model| normalize(model)).collect();
        state.rejected.retain(|model, _| !listed.contains(model));
        let changed = state
            .listing
            .as_ref()
            .is_none_or(|previous| previous.models != models);
        state.listing = Some(Listing {
            models,
            origin,
            fetched_at_ms: now_ms,
        });
        state.last_attempt_ms = Some(now_ms);
        state.last_error = None;
        state.stale = false;
        changed
    }

    /// Record a failed refresh. The last good listing stays in force.
    pub fn record_failure(&mut self, source: &str, error: &str, now_ms: i64) {
        let state = self.sources.entry(source.to_string()).or_default();
        state.last_attempt_ms = Some(now_ms);
        state.last_error = Some(error.chars().take(160).collect());
        state.stale = false;
    }

    /// Record a model the source refused by name. Returns `false` when the
    /// text is not such a rejection. Ids the error lists as accepted become
    /// the source's listing; otherwise the source is refreshed early.
    pub fn record_rejection(&mut self, source: &str, model: &str, text: &str, now_ms: i64) -> bool {
        if !is_model_rejection(text, model) {
            return false;
        }
        let offered = parse_model_listing(text);
        let state = self.sources.entry(source.to_string()).or_default();
        state
            .rejected
            .insert(normalize(model), (model.trim().to_string(), now_ms));
        state.stale = true;
        if !offered.is_empty() {
            self.record_listing(source, offered, ListingOrigin::Rejection, now_ms);
        }
        true
    }

    /// Whether `source` is due for a refresh.
    pub fn needs_refresh(&self, source: &str, ttl: Duration, now_ms: i64) -> bool {
        let Some(state) = self.sources.get(source) else {
            return true;
        };
        if state.stale {
            return true;
        }
        let Some(last) = state.last_attempt_ms else {
            return true;
        };
        let wait = if state.last_error.is_some() {
            ttl.min(FAILURE_RETRY)
        } else {
            ttl
        };
        now_ms.saturating_sub(last) >= wait.as_millis() as i64
    }

    pub fn availability(&self, source: &str, model: &str, now_ms: i64) -> ModelAvailability {
        if is_default(model) {
            return ModelAvailability::Available;
        }
        let Some(state) = self.sources.get(source) else {
            return ModelAvailability::Unknown;
        };
        let key = normalize(model);
        if state
            .rejected
            .get(&key)
            .is_some_and(|(_, at)| now_ms.saturating_sub(*at) < REJECTION_TTL.as_millis() as i64)
        {
            return ModelAvailability::Unavailable;
        }
        match state.listing.as_ref() {
            Some(listing)
                if listing
                    .models
                    .iter()
                    .any(|offered| same_model(&normalize(offered), &key)) =>
            {
                ModelAvailability::Available
            },
            Some(_) => ModelAvailability::Unavailable,
            None => ModelAvailability::Unknown,
        }
    }

    /// Availability of one catalog entry by its catalog id.
    pub fn view(
        &self,
        catalog: &ModelCatalog,
        model_id: &str,
        now_ms: i64,
    ) -> Option<AvailabilityView> {
        let model = catalog.models.get(model_id)?;
        let source = source_key(catalog.connections.get(&model.connection)?);
        let checked_at_ms = self
            .sources
            .get(&source)
            .and_then(|state| state.listing.as_ref())
            .map(|listing| listing.fetched_at_ms);
        Some(AvailabilityView {
            state: self.availability(&source, &model.model, now_ms),
            source,
            catalogued: true,
            checked_at_ms,
        })
    }

    /// Offered models of `source` with no catalog entry, in listing order.
    pub fn uncatalogued(&self, catalog: &ModelCatalog, source: &str) -> Vec<String> {
        let Some(listing) = self
            .sources
            .get(source)
            .and_then(|state| state.listing.as_ref())
        else {
            return Vec::new();
        };
        let catalogued: BTreeSet<String> = catalog
            .models
            .values()
            .filter(|model| {
                catalog
                    .connections
                    .get(&model.connection)
                    .is_some_and(|connection| source_key(connection) == source)
            })
            .map(|model| normalize(&model.model))
            .collect();
        listing
            .models
            .iter()
            .filter(|model| {
                let listed = normalize(model);
                !catalogued.iter().any(|name| same_model(&listed, name))
            })
            .cloned()
            .collect()
    }

    /// Every drifted source of `catalog`, sorted by source.
    pub fn drift(&self, catalog: &ModelCatalog, now_ms: i64) -> Vec<SourceDrift> {
        let mut out = Vec::new();
        for source in self.sources.keys() {
            let mut missing = Vec::new();
            let mut missing_models = Vec::new();
            for (id, model) in &catalog.models {
                if !model.selectable || model.embedding || is_default(&model.model) {
                    continue;
                }
                let Some(connection) = catalog.connections.get(&model.connection) else {
                    continue;
                };
                if source_key(connection) != *source {
                    continue;
                }
                if self.availability(source, &model.model, now_ms) == ModelAvailability::Unavailable
                {
                    missing.push(id.clone());
                    missing_models.push(model.model.clone());
                }
            }
            let uncatalogued = self.uncatalogued(catalog, source);
            if missing.is_empty() && uncatalogued.is_empty() {
                continue;
            }
            out.push(SourceDrift {
                source: source.clone(),
                missing,
                missing_models,
                uncatalogued,
            });
        }
        out
    }

    pub fn statuses(&self) -> Vec<SourceStatus> {
        self.sources
            .iter()
            .map(|(source, state)| SourceStatus {
                source: source.clone(),
                origin: state.listing.as_ref().map(|listing| listing.origin),
                fetched_at_ms: state.listing.as_ref().map(|listing| listing.fetched_at_ms),
                model_count: state
                    .listing
                    .as_ref()
                    .map(|listing| listing.models.len())
                    .unwrap_or(0),
                last_error: state.last_error.clone(),
                rejected: state
                    .rejected
                    .values()
                    .map(|(model, _)| model.clone())
                    .collect(),
            })
            .collect()
    }
}

/// The discovery source a catalog connection belongs to.
pub fn source_key(connection: &ProviderConnection) -> String {
    let provider = connection.provider.as_str();
    match connection.api_base_url.as_deref().and_then(url_authority) {
        Some(authority) => format!("{provider}@{authority}"),
        None => provider.to_string(),
    }
}

fn url_authority(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    (!authority.is_empty()).then(|| authority.to_ascii_lowercase())
}

fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    (!authority.is_empty()).then(|| format!("{scheme}://{authority}"))
}

/// An id-like model name: one token of `[A-Za-z0-9._:/@+-]`, with a
/// trailing parenthetical (`(default)`) dropped. Display labels with spaces
/// are refused.
fn id_token(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    if text.ends_with(')') {
        if let Some(open) = text.rfind('(') {
            text = text[..open].trim_end();
        }
    }
    let valid = !text.is_empty()
        && text.len() <= MAX_ID_LEN
        && !text.ends_with(':')
        && text.chars().any(|ch| ch.is_ascii_alphabetic())
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "._:/@+-".contains(ch));
    valid.then(|| text.to_string())
}

/// Whether `text` is an error refusing `model` by name: some line names the
/// model next to an unknown/unsupported-model phrase. Transient capacity
/// errors are not rejections.
pub fn is_model_rejection(text: &str, model: &str) -> bool {
    const MARKERS: &[&str] = &[
        "not recognized",
        "unknown model",
        "unsupported model",
        "invalid model",
        "model not found",
        "model_not_found",
        "no such model",
        "not a valid model",
        "does not exist",
        "is not supported",
        "not found",
    ];
    const TRANSIENT: &[&str] = &[
        "overloaded",
        "temporarily",
        "capacity",
        "rate limit",
        "quota",
        "try again",
    ];
    if is_default(model) {
        return false;
    }
    let model = model.trim().to_ascii_lowercase();
    let mut end = text.len().min(MAX_SCANNED_ERROR_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains(&model)
            && MARKERS.iter().any(|marker| line.contains(marker))
            && !TRANSIENT.iter().any(|marker| line.contains(marker))
    })
}

/// Model ids in a CLI's listing or error text: tab-separated rows
/// (`id\tLabel`), and bullet or plain rows under an `Available models`
/// header (`* id (default)`), or a comma list after that header's colon.
/// Rows that are display labels rather than ids are skipped.
pub fn parse_model_listing(text: &str) -> Vec<String> {
    fn push(id: String, out: &mut Vec<String>) {
        if !out.contains(&id) && out.len() < MAX_LISTED_MODELS {
            out.push(id);
        }
    }
    let mut out = Vec::<String>::new();
    let mut in_list = false;
    for line in text.lines().take(4_000) {
        let lower = line.to_ascii_lowercase();
        if let Some(position) = lower.find("available models") {
            in_list = true;
            let after = &line[position..];
            if let Some(colon) = after.find(':') {
                for part in after[colon + 1..].split(',') {
                    if let Some(id) = id_token(part) {
                        push(id, &mut out);
                    }
                }
            }
            continue;
        }
        if let Some((first, _)) = line.split_once('\t') {
            if let Some(id) = id_token(first) {
                push(id, &mut out);
            }
            continue;
        }
        if in_list {
            let row = line.trim();
            if row.is_empty() {
                in_list = false;
                continue;
            }
            let row = row.trim_start_matches(['*', '-', '•', '>']).trim_start();
            if let Some(id) = id_token(row) {
                push(id, &mut out);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Provider model-list endpoints
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListingShape {
    /// `{"data":[{"id":..}]}` (OpenAI-compatible, Anthropic).
    DataIds,
    /// `{"models":[{"name":"models/.."}]}` (Gemini).
    GeminiModels,
    /// `{"models":[{"name":..}]}` (Ollama `/api/tags`).
    OllamaTags,
}

#[derive(Clone)]
enum ListingAuth {
    None,
    Bearer(String),
    AnthropicKey(String),
    GoogleKey(String),
}

/// One provider endpoint to list. Its credential is never printed.
#[derive(Clone)]
pub struct ListingTarget {
    pub source: String,
    pub url: String,
    pub shape: ListingShape,
    auth: ListingAuth,
}

impl std::fmt::Debug for ListingTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListingTarget")
            .field("source", &self.source)
            .field("url", &self.url)
            .field("shape", &self.shape)
            .finish_non_exhaustive()
    }
}

fn default_key_env(provider: &LLMProviderKind) -> Option<&'static str> {
    Some(match provider {
        LLMProviderKind::OpenAI => "OPENAI_API_KEY",
        LLMProviderKind::Anthropic => "ANTHROPIC_API_KEY",
        LLMProviderKind::Gemini => "GEMINI_API_KEY",
        LLMProviderKind::Xai => "XAI_API_KEY",
        LLMProviderKind::DeepSeek => "DEEPSEEK_API_KEY",
        LLMProviderKind::OpenRouter => "OPENROUTER_API_KEY",
        _ => return None,
    })
}

/// `…/v1/models` for an OpenAI-compatible base URL (`https://h/v1/chat/completions`
/// → `https://h/v1/models`; a base without `/v1` gains it).
fn openai_compatible_models_url(base: &str) -> Option<String> {
    let origin = url_origin(base)?;
    let path = base
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base)
        .split_once('/')
        .map(|(_, path)| format!("/{path}"))
        .unwrap_or_default();
    let prefix = match path.find("/v1") {
        Some(index) => &path[..index + 3],
        None => "/v1",
    };
    Some(format!("{origin}{prefix}/models"))
}

/// The listing endpoint of every catalog connection with one, de-duplicated
/// by source. A keyed provider is skipped while its key is unset.
pub fn listing_targets(catalog: &ModelCatalog) -> Vec<ListingTarget> {
    listing_targets_with(catalog, |name| std::env::var(name).ok())
}

fn listing_targets_with(
    catalog: &ModelCatalog,
    env: impl Fn(&str) -> Option<String>,
) -> Vec<ListingTarget> {
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for connection in catalog.connections.values() {
        let source = source_key(connection);
        if seen.contains(&source) {
            continue;
        }
        let base = connection.api_base_url.as_deref();
        let key = || {
            let name = connection
                .api_key_env
                .as_deref()
                .or_else(|| default_key_env(&connection.provider))?;
            env(name).filter(|value| !value.trim().is_empty())
        };
        let target = match &connection.provider {
            LLMProviderKind::OpenAI
            | LLMProviderKind::Xai
            | LLMProviderKind::DeepSeek
            | LLMProviderKind::OpenRouter => {
                let url = match base {
                    Some(base) => openai_compatible_models_url(base),
                    None => Some(
                        match connection.provider {
                            LLMProviderKind::Xai => "https://api.x.ai/v1/models",
                            LLMProviderKind::DeepSeek => "https://api.deepseek.com/models",
                            LLMProviderKind::OpenRouter => "https://openrouter.ai/api/v1/models",
                            _ => "https://api.openai.com/v1/models",
                        }
                        .to_string(),
                    ),
                };
                url.zip(key())
                    .map(|(url, key)| (url, ListingShape::DataIds, ListingAuth::Bearer(key)))
            },
            LLMProviderKind::Anthropic => {
                let url = match base {
                    Some(base) => {
                        url_origin(base).map(|origin| format!("{origin}/v1/models?limit=1000"))
                    },
                    None => Some("https://api.anthropic.com/v1/models?limit=1000".to_string()),
                };
                url.zip(key())
                    .map(|(url, key)| (url, ListingShape::DataIds, ListingAuth::AnthropicKey(key)))
            },
            LLMProviderKind::Gemini if base.is_none() => key().map(|key| {
                (
                    "https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000"
                        .to_string(),
                    ListingShape::GeminiModels,
                    ListingAuth::GoogleKey(key),
                )
            }),
            LLMProviderKind::Ollama => {
                url_origin(base.unwrap_or("http://localhost:11434")).map(|origin| {
                    (
                        format!("{origin}/api/tags"),
                        ListingShape::OllamaTags,
                        ListingAuth::None,
                    )
                })
            },
            _ => None,
        };
        if let Some((url, shape, auth)) = target {
            seen.insert(source.clone());
            targets.push(ListingTarget {
                source,
                url,
                shape,
                auth,
            });
        }
    }
    targets
}

/// Model ids in a provider listing body.
pub fn parse_listing_body(shape: ListingShape, body: &Value) -> Vec<String> {
    let (array, field) = match shape {
        ListingShape::DataIds => ("data", "id"),
        ListingShape::GeminiModels | ListingShape::OllamaTags => ("models", "name"),
    };
    body.get(array)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(field).and_then(Value::as_str))
                .map(|id| id.strip_prefix("models/").unwrap_or(id).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Fetch one listing. Errors carry only a status class, never a body.
pub async fn fetch_listing(
    client: &reqwest::Client,
    target: &ListingTarget,
) -> Result<Vec<String>, String> {
    let mut request = client.get(&target.url).timeout(LISTING_TIMEOUT);
    request = match &target.auth {
        ListingAuth::None => request,
        ListingAuth::Bearer(key) => request.bearer_auth(key),
        ListingAuth::AnthropicKey(key) => request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        ListingAuth::GoogleKey(key) => request.header("x-goog-api-key", key),
    };
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "listing request timed out"
        } else if error.is_connect() {
            "listing endpoint unreachable"
        } else {
            "listing request failed"
        }
        .to_string()
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("listing returned HTTP {}", status.as_u16()));
    }
    let text = crate::providers::read_bounded_response_text(response)
        .await
        .map_err(|_| "listing body was too large or unreadable".to_string())?;
    let body: Value =
        serde_json::from_str(&text).map_err(|_| "listing body was not JSON".to_string())?;
    Ok(parse_listing_body(target.shape, &body))
}

// ---------------------------------------------------------------------------
// Process-wide registry
// ---------------------------------------------------------------------------

fn registry() -> &'static RwLock<ModelDiscovery> {
    static REGISTRY: OnceLock<RwLock<ModelDiscovery>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(ModelDiscovery::default()))
}

fn published() -> &'static RwLock<Vec<SourceDrift>> {
    static DRIFT: OnceLock<RwLock<Vec<SourceDrift>>> = OnceLock::new();
    DRIFT.get_or_init(|| RwLock::new(Vec::new()))
}

static DRIFT_REVISION: AtomicU64 = AtomicU64::new(0);

/// The registry's clock (Unix ms).
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn read<T>(f: impl FnOnce(&ModelDiscovery) -> T) -> T {
    f(&registry()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// Read the process-wide registry, for callers that take a
/// [`ModelDiscovery`] so tests can pass their own.
pub fn with_registry<T>(f: impl FnOnce(&ModelDiscovery) -> T) -> T {
    read(f)
}

fn write<T>(f: impl FnOnce(&mut ModelDiscovery) -> T) -> T {
    f(&mut registry()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()))
}

pub fn record_listing<I, S>(source: &str, models: I, origin: ListingOrigin) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    write(|registry| registry.record_listing(source, models, origin, now_ms()))
}

pub fn record_failure(source: &str, error: &str) {
    write(|registry| registry.record_failure(source, error, now_ms()))
}

/// Record a source's refusal of `model`, when `text` is one. Cheap when it
/// is not: callers pass every failed turn's error.
pub fn record_rejection(source: &str, model: &str, text: &str) -> bool {
    if !is_model_rejection(text, model) {
        return false;
    }
    let recorded = write(|registry| registry.record_rejection(source, model, text, now_ms()));
    if recorded {
        tracing::warn!(
            target: "model_discovery",
            source,
            model,
            "the source rejected a model; it is hidden until the source lists it again"
        );
    }
    recorded
}

pub fn needs_refresh(source: &str, ttl: Duration) -> bool {
    read(|registry| registry.needs_refresh(source, ttl, now_ms()))
}

pub fn availability(source: &str, model: &str) -> ModelAvailability {
    read(|registry| registry.availability(source, model, now_ms()))
}

/// Availability of one catalog entry; `None` for an unknown id.
pub fn availability_view(catalog: &ModelCatalog, model_id: &str) -> Option<AvailabilityView> {
    read(|registry| registry.view(catalog, model_id, now_ms()))
}

pub fn uncatalogued_models(catalog: &ModelCatalog, source: &str) -> Vec<String> {
    read(|registry| registry.uncatalogued(catalog, source))
}

pub fn source_statuses() -> Vec<SourceStatus> {
    read(ModelDiscovery::statuses)
}

/// Recompute drift against `catalog` and publish it. Returns the drift
/// entries whose fingerprint is new since the last publication.
pub fn publish_drift(catalog: &ModelCatalog) -> Vec<SourceDrift> {
    let drift = read(|registry| registry.drift(catalog, now_ms()));
    let mut slot = published()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if *slot == drift {
        return Vec::new();
    }
    let previous: BTreeSet<String> = slot.iter().map(SourceDrift::fingerprint).collect();
    let changed = drift
        .iter()
        .filter(|entry| !previous.contains(&entry.fingerprint()))
        .cloned()
        .collect();
    *slot = drift;
    DRIFT_REVISION.fetch_add(1, Ordering::SeqCst);
    changed
}

/// Bumped on every published drift change; `0` = never published.
pub fn drift_revision() -> u64 {
    DRIFT_REVISION.load(Ordering::SeqCst)
}

pub fn published_drift() -> (u64, Vec<SourceDrift>) {
    let slot = published()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (drift_revision(), slot.clone())
}

/// Refresh every due provider listing of `catalog`. Returns how many
/// endpoints were asked. Failures keep the last good listing.
pub async fn refresh_provider_listings(catalog: &ModelCatalog, ttl: Duration) -> usize {
    let due: Vec<ListingTarget> = listing_targets(catalog)
        .into_iter()
        .filter(|target| needs_refresh(&target.source, ttl))
        .collect();
    if due.is_empty() {
        return 0;
    }
    let client = crate::providers::default_http_client();
    for target in &due {
        match fetch_listing(&client, target).await {
            Ok(models) => {
                record_listing(&target.source, models, ListingOrigin::ProviderApi);
            },
            Err(error) => {
                tracing::debug!(target: "model_discovery", source = %target.source, %error, "model listing failed; keeping the last good list");
                record_failure(&target.source, &error);
            },
        }
    }
    due.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_000_000_000;

    fn catalog() -> ModelCatalog {
        serde_json::from_value(json!({
            "connections": {
                "harness-agy": {"provider": "harness-agy"},
                "openai": {"provider": "openai", "api_key_env": "TEST_OPENAI_KEY"},
                "openai-chat": {"provider": "openai", "api_key_env": "TEST_OPENAI_KEY"},
                "ollama-a": {"provider": "ollama", "api_base_url": "http://localhost:11434/api/chat"},
                "ollama-b": {"provider": "ollama", "api_base_url": "http://localhost:11434/api/generate"},
                "anthropic": {"provider": "anthropic"},
                "sarvam": {"provider": "sarvam", "api_key_env": "TEST_SARVAM_KEY"}
            },
            "models": {
                "harness-agy/default": {"connection": "harness-agy", "model": "default", "label": "Default"},
                "harness-agy/gemini-3.7-flash-high": {"connection": "harness-agy", "model": "gemini-3.7-flash-high", "label": "Flash High"},
                "harness-agy/gemini-3.8-flash-low": {"connection": "harness-agy", "model": "gemini-3.8-flash-low", "label": "Flash Low"},
                "openai/sol": {"connection": "openai", "model": "gpt-sol", "label": "Sol"}
            }
        }))
        .unwrap()
    }

    #[test]
    fn listing_marks_missing_catalog_models_unavailable_and_keeps_default() {
        let mut discovery = ModelDiscovery::default();
        assert_eq!(
            discovery.availability("harness-agy", "gemini-3.7-flash-high", NOW),
            ModelAvailability::Unknown
        );
        discovery.record_listing(
            "harness-agy",
            ["gemini-3.8-flash-high", "gemini-3.8-flash-low"],
            ListingOrigin::CliListing,
            NOW,
        );
        assert_eq!(
            discovery.availability("harness-agy", "gemini-3.7-flash-high", NOW),
            ModelAvailability::Unavailable
        );
        assert_eq!(
            discovery.availability("harness-agy", "GEMINI-3.8-flash-low", NOW),
            ModelAvailability::Available
        );
        assert_eq!(
            discovery.availability("harness-agy", "default", NOW),
            ModelAvailability::Available
        );
        let catalog = catalog();
        assert_eq!(
            discovery.uncatalogued(&catalog, "harness-agy"),
            vec!["gemini-3.8-flash-high".to_string()]
        );
        let drift = discovery.drift(&catalog, NOW);
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].missing, vec!["harness-agy/gemini-3.7-flash-high"]);
        assert_eq!(drift[0].missing_models, vec!["gemini-3.7-flash-high"]);
        assert_eq!(drift[0].uncatalogued, vec!["gemini-3.8-flash-high"]);
        let view = discovery
            .view(&catalog, "harness-agy/gemini-3.7-flash-high", NOW)
            .unwrap();
        assert_eq!(view.state, ModelAvailability::Unavailable);
        assert_eq!(view.checked_at_ms, Some(NOW));
    }

    #[test]
    fn a_failed_or_empty_refresh_keeps_the_last_good_listing() {
        let mut discovery = ModelDiscovery::default();
        discovery.record_listing("xai", ["grok-4.7"], ListingOrigin::ProviderApi, NOW);
        discovery.record_failure("xai", "listing returned HTTP 503", NOW + 1);
        assert!(!discovery.record_listing(
            "xai",
            Vec::<String>::new(),
            ListingOrigin::ProviderApi,
            NOW + 2
        ));
        assert_eq!(
            discovery.availability("xai", "grok-4.7", NOW + 3),
            ModelAvailability::Available
        );
        let status = &discovery.statuses()[0];
        assert_eq!(status.model_count, 1);
        assert!(status.last_error.is_some());
        // A failure retries sooner than the success TTL.
        let hour = Duration::from_secs(3600);
        assert!(!discovery.needs_refresh("xai", hour, NOW + 2));
        assert!(discovery.needs_refresh("xai", hour, NOW + 2 + FAILURE_RETRY.as_millis() as i64));
    }

    #[test]
    fn agy_rejection_hides_the_model_and_asks_for_a_refresh() {
        let text = "Error: invalid model selection (--model \"gemini-3.7-flash-high\" --effort \"\"): model gemini-3.7-flash-high is not recognized as a known model or custom model in settings\nAvailable models:\n  Gemini 3.8 Flash (High)\n  Claude Opus 4.6 (Thinking)";
        let mut discovery = ModelDiscovery::default();
        assert!(discovery.record_rejection("harness-agy", "gemini-3.7-flash-high", text, NOW));
        assert_eq!(
            discovery.availability("harness-agy", "gemini-3.7-flash-high", NOW),
            ModelAvailability::Unavailable
        );
        // Labels are not ids: no listing was invented from them.
        assert_eq!(discovery.statuses()[0].model_count, 0);
        assert!(discovery.needs_refresh("harness-agy", Duration::from_secs(3600), NOW));
        // The rejection expires without a listing.
        assert_eq!(
            discovery.availability(
                "harness-agy",
                "gemini-3.7-flash-high",
                NOW + REJECTION_TTL.as_millis() as i64
            ),
            ModelAvailability::Unknown
        );
        // A listing that offers it again clears the rejection.
        discovery.record_listing(
            "harness-agy",
            ["gemini-3.7-flash-high"],
            ListingOrigin::CliListing,
            NOW + 1,
        );
        assert_eq!(
            discovery.availability("harness-agy", "gemini-3.7-flash-high", NOW + 1),
            ModelAvailability::Available
        );
    }

    #[test]
    fn a_rejection_naming_accepted_ids_records_them_as_a_listing() {
        let text = "error: unknown model 'grok-4-fast'. Available models: grok-4.6, grok-4.5";
        let mut discovery = ModelDiscovery::default();
        assert!(discovery.record_rejection("harness-grok", "grok-4-fast", text, NOW));
        assert_eq!(
            discovery.availability("harness-grok", "grok-4.5", NOW),
            ModelAvailability::Available
        );
        assert_eq!(
            discovery.statuses()[0].origin,
            Some(ListingOrigin::Rejection)
        );
    }

    #[test]
    fn rejection_detection_needs_the_model_named_beside_a_rejection_phrase() {
        assert!(is_model_rejection(
            "model_not_found: The model `gpt-x` does not exist",
            "gpt-x"
        ));
        assert!(!is_model_rejection(
            "model gpt-x is overloaded, try again later (not found)",
            "gpt-x"
        ));
        assert!(!is_model_rejection("file not found\nusing gpt-x", "gpt-x"));
        assert!(!is_model_rejection("unknown model default", "default"));
        assert!(!is_model_rejection("authentication failed", "gpt-x"));
    }

    #[test]
    fn cli_listing_parser_reads_tab_rows_and_bulleted_lists() {
        let agy = "Fetching available models...\ngemini-3.8-flash-high\tGemini 3.8 Flash (High)\nclaude-sonnet-4-6\tClaude Sonnet 4.6 (Thinking)\n";
        assert_eq!(
            parse_model_listing(agy),
            vec!["gemini-3.8-flash-high", "claude-sonnet-4-6"]
        );
        let grok = "Default model: grok-4.6\n\nAvailable models:\n  * grok-4.6 (default)\n  - grok-4.5\n\nRun grok --help";
        assert_eq!(parse_model_listing(grok), vec!["grok-4.6", "grok-4.5"]);
        assert!(parse_model_listing("No models available. Use /login").is_empty());
    }

    #[test]
    fn provider_targets_follow_connections_keys_and_base_urls() {
        let catalog = catalog();
        let targets = listing_targets_with(&catalog, |name| {
            (name == "TEST_OPENAI_KEY" || name == "ANTHROPIC_API_KEY").then(|| "k".to_string())
        });
        let sources: Vec<_> = targets
            .iter()
            .map(|target| target.source.as_str())
            .collect();
        // openai + openai-chat share one source; both ollama connections share
        // one host; sarvam has no listing; harness sources are host-recorded.
        assert_eq!(
            sources,
            vec!["anthropic", "ollama@localhost:11434", "openai"]
        );
        let ollama = targets
            .iter()
            .find(|t| t.shape == ListingShape::OllamaTags)
            .unwrap();
        assert_eq!(ollama.url, "http://localhost:11434/api/tags");
        assert!(!format!("{:?}", targets).contains("\"k\""));
        let none = listing_targets_with(&catalog, |_| None);
        assert_eq!(none.len(), 1, "keyed providers wait for a key");
        assert_eq!(
            openai_compatible_models_url("https://gw.example/v1/chat/completions").as_deref(),
            Some("https://gw.example/v1/models")
        );
        assert_eq!(
            openai_compatible_models_url("http://127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080/v1/models")
        );
    }

    #[test]
    fn provider_bodies_parse_by_shape() {
        assert_eq!(
            parse_listing_body(
                ListingShape::DataIds,
                &json!({"data":[{"id":"gpt-sol"},{"id":"o9"}]})
            ),
            vec!["gpt-sol", "o9"]
        );
        assert_eq!(
            parse_listing_body(
                ListingShape::GeminiModels,
                &json!({"models":[{"name":"models/gemini-3.8-pro"}]})
            ),
            vec!["gemini-3.8-pro"]
        );
        assert_eq!(
            parse_listing_body(
                ListingShape::OllamaTags,
                &json!({"models":[{"name":"qwen3:8b"}]})
            ),
            vec!["qwen3:8b"]
        );
    }

    #[test]
    fn dated_snapshot_ids_count_as_their_catalogued_alias() {
        let mut catalog = catalog();
        catalog.models.insert(
            "anthropic/sonnet".into(),
            serde_json::from_value(
                json!({"connection": "anthropic", "model": "claude-sonnet-9-5", "label": "Sonnet"}),
            )
            .unwrap(),
        );
        let mut discovery = ModelDiscovery::default();
        discovery.record_listing(
            "anthropic",
            ["claude-sonnet-9-5-20261001", "claude-haiku-9-5"],
            ListingOrigin::ProviderApi,
            NOW,
        );
        assert_eq!(
            discovery.availability("anthropic", "claude-sonnet-9-5", NOW),
            ModelAvailability::Available
        );
        assert_eq!(
            discovery.uncatalogued(&catalog, "anthropic"),
            vec!["claude-haiku-9-5".to_string()]
        );
        assert!(same_model("gpt-x-2026-01-31", "gpt-x"));
        assert!(!same_model("gpt-x-mini", "gpt-x"));
        assert!(!same_model("gpt-x2", "gpt-x"));
    }

    #[test]
    fn ollama_latest_tags_match_untagged_catalog_names() {
        let mut discovery = ModelDiscovery::default();
        discovery.record_listing(
            "ollama@localhost:11434",
            ["llama3:latest"],
            ListingOrigin::ProviderApi,
            NOW,
        );
        assert_eq!(
            discovery.availability("ollama@localhost:11434", "llama3", NOW),
            ModelAvailability::Available
        );
    }
}
