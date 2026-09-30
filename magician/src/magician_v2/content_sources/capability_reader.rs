use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tool_runtime_core::{
    action_overrides::WorkspacePathAccess, manifest_parser::parse_skill_runtime_package,
};

use super::{
    cache::ScopedContentCache,
    public_http::{
        ConditionalHttpRequest, PublicHttpFetch, PublicHttpFetchPolicy, PublicHttpFetcher,
    },
    traits::ContentReader,
    types::{
        canonicalize_http_url, AdapterAuth, AdapterExecution, ContentDocument,
        ContentInvocationSource, ContentSourceCapabilities, ContentSourceClass,
        ContentSourceDescriptor, FreshnessPolicy, ReadDepth, ReadRequest, RetrievalActionMetadata,
        RetrievalAuthority, RetrievalOutputKind, RetrievalRung, CONTENT_SOURCE_SCHEMA_VERSION,
        MAX_DOCUMENT_TEXT_CHARS,
    },
};
#[cfg(any(test, feature = "test-fixtures"))]
use crate::magician_v2::skills::embedded_extensions::discover_skill_markdown_paths;
use crate::magician_v2::{
    artifact_v2::io::write_bytes_atomic,
    execution::{
        capability::NativeActionSchemaDef,
        compiled_providers::project_runtime_package_to_pack,
        primitive_dispatch::{
            DeterministicCapabilityInvocation, DeterministicCapabilityInvocationSource,
            DeterministicCapabilityInvoker,
        },
        CapabilityPackDefinition, ImplementationType,
    },
    skills::embedded_extensions::{
        load_skill_magician_extension, owning_skill_name, read_bounded_skill_markdown,
    },
};

#[derive(Debug, thiserror::Error)]
#[error("content extraction capability failed ({code}): {message}")]
pub struct CapabilityReaderExecutionFailure {
    pub code: String,
    pub message: String,
}

#[cfg(any(test, feature = "test-fixtures"))]
use super::registry::ContentSourceRegistry;

pub const CAPABILITY_READER_EXTENSION: &str = "content_reader";
pub const CAPABILITY_READER_MANIFEST_SCHEMA_VERSION: u32 = 1;
const MAX_CACHE_TTL_SECS: u64 = 30 * 24 * 60 * 60;
const DEFAULT_CACHE_TTL_SECS: u64 = 15 * 60;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_TEXT_CHARS: usize = MAX_DOCUMENT_TEXT_CHARS;
const DEFAULT_MIN_GIST_CHARS: usize = 80;
const DEFAULT_MIN_FULL_TEXT_CHARS: usize = 200;
const MAX_MIN_QUALITY_CHARS: usize = 32 * 1024;
const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 20;
const DEFAULT_MAX_REDIRECTS: usize = 5;
pub const CONTENT_CACHE_OUTCOME_METADATA_KEY: &str = "content_cache_outcome";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReaderManifest {
    pub schema_version: u32,
    pub reader: CapabilityReaderAdapterManifest,
    pub capability: CapabilityReaderBindingManifest,
    #[serde(default)]
    pub input: CapabilityReaderInputManifest,
    pub output: CapabilityReaderOutputManifest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReaderAdapterManifest {
    pub id: String,
    pub display_name: String,
    #[serde(default = "default_web_page_class")]
    pub class: ContentSourceClass,
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
    #[serde(default = "default_max_text_chars")]
    pub max_text_chars: usize,
    #[serde(default = "default_min_gist_chars")]
    pub min_gist_chars: usize,
    #[serde(default = "default_min_full_text_chars")]
    pub min_full_text_chars: usize,
    #[serde(default = "default_fetch_timeout_secs")]
    pub fetch_timeout_secs: u64,
    #[serde(default = "default_max_redirects")]
    pub max_redirects: usize,
    #[serde(default = "default_reader_media_types")]
    pub accepted_media_types: Vec<String>,
    /// Optional Phase 7 controller projection. Existing reader manifests use
    /// the public-static, local-extraction defaults.
    #[serde(default)]
    pub retrieval: Option<RetrievalActionMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReaderBindingManifest {
    pub name: String,
    pub action: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReaderInputManifest {
    #[serde(default = "default_input_file_argument")]
    pub input_file_argument: String,
    #[serde(default = "default_max_chars_argument")]
    pub max_chars_argument: Option<String>,
    #[serde(default)]
    pub fixed_arguments: BTreeMap<String, Value>,
}

impl Default for CapabilityReaderInputManifest {
    fn default() -> Self {
        Self {
            input_file_argument: default_input_file_argument(),
            max_chars_argument: default_max_chars_argument(),
            fixed_arguments: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReaderOutputManifest {
    #[serde(default)]
    pub mode: CapabilityReaderOutputMode,
    #[serde(default)]
    pub text_pointer: Option<String>,
    #[serde(default)]
    pub title_pointer: Option<String>,
    #[serde(default)]
    pub method_pointer: Option<String>,
    #[serde(default)]
    pub truncated_pointer: Option<String>,
    #[serde(default)]
    pub error_pointer: Option<String>,
    /// A boolean the extractor sets when the page rendered its content on
    /// the client rather than the server — content shipped for hydration
    /// that never reached the DOM, or an empty application root beside a
    /// script bundle. A character count cannot see that; the extractor
    /// can, and its verdict fails the read as a JavaScript shell so the
    /// ladder hands off to a browser.
    #[serde(default)]
    pub shell_pointer: Option<String>,
    /// The sentence that says what the extractor measured.
    #[serde(default)]
    pub shell_reason_pointer: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityReaderOutputMode {
    #[default]
    Json,
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionQuality {
    pub char_count: usize,
    pub word_count: usize,
    pub score: f64,
    pub sufficient: bool,
}

impl CapabilityReaderManifest {
    pub fn from_yaml_str(contents: &str) -> Result<Self> {
        let manifest: Self =
            serde_yaml::from_str(contents).context("decoding capability reader manifest")?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CAPABILITY_READER_MANIFEST_SCHEMA_VERSION {
            bail!(
                "unsupported capability reader manifest schema version {}",
                self.schema_version
            );
        }
        if self.reader.id.trim().is_empty() || self.reader.display_name.trim().is_empty() {
            bail!("capability reader requires non-empty id and display_name");
        }
        if self.capability.name.trim().is_empty() || self.capability.action.trim().is_empty() {
            bail!("capability reader requires non-empty capability name and action");
        }
        if self.reader.cache_ttl_secs == 0 || self.reader.cache_ttl_secs > MAX_CACHE_TTL_SECS {
            bail!("capability reader cache_ttl_secs must be between 1 and {MAX_CACHE_TTL_SECS}");
        }
        if self.reader.max_text_chars == 0 || self.reader.max_text_chars > MAX_DOCUMENT_TEXT_CHARS {
            bail!(
                "capability reader max_text_chars must be between 1 and {MAX_DOCUMENT_TEXT_CHARS}"
            );
        }
        if self.reader.min_gist_chars == 0
            || self.reader.min_full_text_chars == 0
            || self.reader.min_gist_chars > self.reader.min_full_text_chars
            || self.reader.min_full_text_chars > MAX_MIN_QUALITY_CHARS
            || self.reader.min_full_text_chars > self.reader.max_text_chars
        {
            bail!("capability reader extraction quality thresholds are invalid");
        }
        if self.input.input_file_argument.trim().is_empty() {
            bail!("capability reader input_file_argument must not be empty");
        }
        if self
            .input
            .max_chars_argument
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("capability reader max_chars_argument must not be empty when present");
        }
        if self.input.max_chars_argument.as_deref() == Some(self.input.input_file_argument.as_str())
        {
            bail!("capability reader dynamic input arguments must be distinct");
        }
        if self
            .input
            .fixed_arguments
            .contains_key(&self.input.input_file_argument)
            || self
                .input
                .max_chars_argument
                .as_ref()
                .is_some_and(|argument| self.input.fixed_arguments.contains_key(argument))
        {
            bail!("capability reader fixed arguments must not override dynamic inputs");
        }
        match self.output.mode {
            CapabilityReaderOutputMode::Json => validate_pointer(
                self.output.text_pointer.as_deref().ok_or_else(|| {
                    anyhow!("JSON capability reader output requires text_pointer")
                })?,
                "text_pointer",
            )?,
            CapabilityReaderOutputMode::Text => {
                if self.output.text_pointer.is_some()
                    || self.output.title_pointer.is_some()
                    || self.output.method_pointer.is_some()
                    || self.output.truncated_pointer.is_some()
                    || self.output.error_pointer.is_some()
                {
                    bail!("text capability reader output cannot declare JSON pointers");
                }
            },
        }
        for (label, pointer) in [
            ("title_pointer", self.output.title_pointer.as_deref()),
            ("method_pointer", self.output.method_pointer.as_deref()),
            (
                "truncated_pointer",
                self.output.truncated_pointer.as_deref(),
            ),
            ("error_pointer", self.output.error_pointer.as_deref()),
        ] {
            if let Some(pointer) = pointer {
                validate_pointer(pointer, label)?;
            }
        }
        let policy = self.http_policy()?;
        policy.validate()
    }

    fn http_policy(&self) -> Result<PublicHttpFetchPolicy> {
        let accepted_media_types = self
            .reader
            .accepted_media_types
            .iter()
            .map(|value| value.trim().to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        if accepted_media_types.is_empty() {
            bail!("capability reader requires at least one accepted media type");
        }
        if accepted_media_types.len() != self.reader.accepted_media_types.len() {
            bail!("capability reader accepted_media_types contains duplicates");
        }
        Ok(PublicHttpFetchPolicy {
            max_response_bytes: self.reader.max_response_bytes,
            max_redirects: self.reader.max_redirects,
            timeout: Duration::from_secs(self.reader.fetch_timeout_secs),
            user_agent: "MagicianStaticContentReader/1".into(),
            accepted_media_types,
            allow_missing_media_type: false,
        })
    }
}

#[derive(Debug)]
struct ExtractedContent {
    text: String,
    title: Option<String>,
    method: Option<String>,
    truncated: Option<bool>,
    /// `Some(reason)` when the extractor judged the page a client-rendered
    /// shell; the text is then a blurb over an empty page, not the page.
    shell_reason: Option<String>,
}

struct ScratchFileGuard {
    path: PathBuf,
}

impl ScratchFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for ScratchFileGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    error = %error,
                    path = %self.path.display(),
                    "failed to remove static-reader scratch file"
                );
            }
        }
    }
}

pub struct CapabilityStaticContentReader {
    manifest: CapabilityReaderManifest,
    descriptor: ContentSourceDescriptor,
    capability_revision: String,
    invoker: Arc<dyn DeterministicCapabilityInvoker>,
    fetcher: PublicHttpFetcher,
    cache: ScopedContentCache,
    read_locks: Arc<Mutex<BTreeMap<String, Weak<AsyncMutex<()>>>>>,
}

impl CapabilityStaticContentReader {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn new(
        manifest: CapabilityReaderManifest,
        invoker: Arc<dyn DeterministicCapabilityInvoker>,
        cache_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::new_with_revision(
            manifest,
            invoker,
            ScopedContentCache::new(cache_root),
            "unversioned",
            None,
        )
    }

    pub fn new_with_revision(
        manifest: CapabilityReaderManifest,
        invoker: Arc<dyn DeterministicCapabilityInvoker>,
        cache: ScopedContentCache,
        capability_revision: impl Into<String>,
        #[cfg(any(test, feature = "test-fixtures"))] fetcher_override: Option<PublicHttpFetcher>,
    ) -> Result<Self> {
        manifest.validate()?;
        let capability_revision = capability_revision.into();
        if capability_revision.trim().is_empty() {
            bail!("capability reader revision must not be empty");
        }
        let mut retrieval = manifest.reader.retrieval.clone().unwrap_or_else(|| {
            RetrievalActionMetadata::reader(
                format!("{}.read", manifest.reader.id),
                RetrievalRung::PublicStatic,
                RetrievalAuthority::PublicRemoteRead,
                vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
            )
        });
        retrieval.accepted_media_types = manifest
            .reader
            .accepted_media_types
            .iter()
            .map(|media_type| media_type.trim().to_ascii_lowercase())
            .collect();
        let descriptor = ContentSourceDescriptor {
            adapter_id: manifest.reader.id.clone(),
            display_name: manifest.reader.display_name.clone(),
            class: manifest.reader.class,
            capabilities: ContentSourceCapabilities {
                discovery: false,
                full_content: true,
                cursor: false,
                conditional_fetch: true,
                // Fetching the public origin is distinct from transmitting
                // page content to a third-party extraction service. The bound
                // capability receives only a local file.
                execution: AdapterExecution::LocalProcess,
                auth: AdapterAuth::None,
                sends_user_intent: false,
                metered: false,
            },
            retrieval,
        };
        descriptor.validate()?;
        let fetcher = PublicHttpFetcher::new(manifest.http_policy()?)?;
        #[cfg(any(test, feature = "test-fixtures"))]
        let fetcher = fetcher_override.unwrap_or(fetcher);
        Ok(Self {
            manifest,
            descriptor,
            capability_revision,
            invoker,
            fetcher,
            cache,
            read_locks: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn with_fetcher(
        manifest: CapabilityReaderManifest,
        invoker: Arc<dyn DeterministicCapabilityInvoker>,
        cache_root: impl Into<PathBuf>,
        fetcher: PublicHttpFetcher,
    ) -> Result<Self> {
        Self::new_with_revision(
            manifest,
            invoker,
            ScopedContentCache::new(cache_root),
            "test-capability-revision",
            Some(fetcher),
        )
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn with_fetcher_and_revision(
        manifest: CapabilityReaderManifest,
        invoker: Arc<dyn DeterministicCapabilityInvoker>,
        cache_root: impl Into<PathBuf>,
        fetcher: PublicHttpFetcher,
        capability_revision: impl Into<String>,
    ) -> Result<Self> {
        Self::new_with_revision(
            manifest,
            invoker,
            ScopedContentCache::new(cache_root),
            capability_revision,
            Some(fetcher),
        )
    }

    async fn read_locked(
        &self,
        request: &ReadRequest,
        request_url: &str,
    ) -> Result<ContentDocument> {
        let now_ms = Utc::now().timestamp_millis();
        let cached = self
            .cache
            .get(
                &request.principal,
                &request.workspace,
                &self.descriptor.adapter_id,
                &self.capability_revision,
                request.depth,
                request_url,
            )
            .await?;
        let cached = cached.filter(|hit| {
            let quality = evaluate_extraction_quality(
                &hit.document.text,
                request.depth,
                self.manifest.reader.min_gist_chars,
                self.manifest.reader.min_full_text_chars,
            );
            let shell_codes = super::retrieval::noncontent_shell_codes(&hit.document.text);
            if !quality.sufficient || !shell_codes.is_empty() {
                tracing::warn!(
                    reader_id = %self.descriptor.adapter_id,
                    capability_revision = %self.capability_revision,
                    ?request.depth,
                    shell_codes = ?shell_codes,
                    "ignoring cached content that fails the static-reader quality boundary"
                );
            }
            quality.sufficient && shell_codes.is_empty()
        });
        let ttl_ms = (self.manifest.reader.cache_ttl_secs as i64).saturating_mul(1000);
        if request.freshness == FreshnessPolicy::CachedOk
            && cached
                .as_ref()
                .is_some_and(|hit| hit.is_fresh_at(now_ms, ttl_ms))
        {
            return self.bind_document_to_candidate(
                cached.expect("fresh cache checked above").document,
                request,
                "fresh_hit",
            );
        }

        let validators = cached
            .as_ref()
            .map(|hit| hit.validators.clone())
            .unwrap_or_default();
        let fetch_url = cached
            .as_ref()
            .and_then(|hit| hit.document.canonical_url.as_deref())
            .unwrap_or(request_url);
        let fetched = match self.fetcher.fetch(fetch_url, &validators).await {
            Ok(fetched) => fetched,
            Err(error) if request.freshness == FreshnessPolicy::CachedOk && cached.is_some() => {
                tracing::warn!(
                    error = %error,
                    reader_id = %self.descriptor.adapter_id,
                    "static content refresh failed; returning permitted stale cache entry"
                );
                return self.bind_document_to_candidate(
                    cached.expect("stale cache checked above").document,
                    request,
                    "stale_fallback",
                );
            },
            Err(error) => return Err(error),
        };

        match fetched {
            PublicHttpFetch::NotModified {
                etag,
                last_modified,
                ..
            } => {
                let expected_content_hash = cached
                    .as_ref()
                    .map(|hit| hit.document.content_hash.clone())
                    .ok_or_else(|| anyhow!("origin returned 304 without a usable cache entry"))?;
                let revalidation = self
                    .cache
                    .mark_revalidated(
                        &request.principal,
                        &request.workspace,
                        &self.descriptor.adapter_id,
                        &self.capability_revision,
                        request.depth,
                        request_url,
                        &expected_content_hash,
                        now_ms,
                        &ConditionalHttpRequest {
                            etag,
                            last_modified,
                        },
                    )
                    .await?
                    .ok_or_else(|| anyhow!("origin returned 304 without a usable cache entry"))?;
                let cache_outcome = if revalidation.revalidated {
                    "revalidated"
                } else {
                    "concurrent_refresh"
                };
                self.bind_document_to_candidate(revalidation.document, request, cache_outcome)
            },
            PublicHttpFetch::Modified {
                final_url,
                media_type,
                etag,
                last_modified,
                body,
            } => {
                let extracted = match self.extract(request, body).await {
                    Ok(extracted) => extracted,
                    Err(error) if request.freshness == FreshnessPolicy::CachedOk => {
                        if let Some(hit) = cached.as_ref() {
                            tracing::warn!(
                                error = %error,
                                reader_id = %self.descriptor.adapter_id,
                                "static content extraction failed; returning permitted stale cache entry"
                            );
                            return self.bind_document_to_candidate(
                                hit.document.clone(),
                                request,
                                "stale_fallback",
                            );
                        }
                        return Err(error);
                    },
                    Err(error) => return Err(error),
                };
                let mut shell_codes: Vec<String> =
                    super::retrieval::noncontent_shell_codes(&extracted.text)
                        .into_iter()
                        .map(str::to_string)
                        .collect();
                if let Some(reason) = extracted.shell_reason.as_deref() {
                    // The extractor's verdict, from what the count cannot see:
                    // content shipped for hydration that the server never
                    // rendered. The same code the phrase detector uses, so
                    // the ladder's escalation to a browser handoff applies.
                    shell_codes.push(format!("javascript_shell ({reason})"));
                }
                if !shell_codes.is_empty() {
                    let error = anyhow!(
                        "static extraction returned a non-content shell: {}",
                        shell_codes.join(", ")
                    );
                    if request.freshness == FreshnessPolicy::CachedOk {
                        if let Some(hit) = cached.as_ref() {
                            tracing::warn!(
                                error = %error,
                                reader_id = %self.descriptor.adapter_id,
                                "static content shell detected; returning permitted stale cache entry"
                            );
                            return self.bind_document_to_candidate(
                                hit.document.clone(),
                                request,
                                "stale_fallback",
                            );
                        }
                    }
                    return Err(error);
                }
                let quality = evaluate_extraction_quality(
                    &extracted.text,
                    request.depth,
                    self.manifest.reader.min_gist_chars,
                    self.manifest.reader.min_full_text_chars,
                );
                if !quality.sufficient {
                    let error = anyhow!(
                        "static extraction produced insufficient content: {} chars, {} words",
                        quality.char_count,
                        quality.word_count
                    );
                    if request.freshness == FreshnessPolicy::CachedOk {
                        if let Some(hit) = cached.as_ref() {
                            tracing::warn!(
                                error = %error,
                                reader_id = %self.descriptor.adapter_id,
                                "static content quality refresh failed; returning permitted stale cache entry"
                            );
                            return self.bind_document_to_candidate(
                                hit.document.clone(),
                                request,
                                "stale_fallback",
                            );
                        }
                    }
                    return Err(error);
                }
                let content_hash = blake3::hash(extracted.text.as_bytes()).to_hex().to_string();
                let canonical_url = canonicalize_http_url(&final_url)
                    .context("canonicalizing final static-reader URL")?;
                let mut metadata = BTreeMap::new();
                metadata.insert("reader_id".into(), json!(self.descriptor.adapter_id));
                metadata.insert(
                    "extractor_capability".into(),
                    json!(self.manifest.capability.name),
                );
                metadata.insert(
                    "extraction_quality".into(),
                    json!({
                        "chars": quality.char_count,
                        "words": quality.word_count,
                        "score": quality.score,
                    }),
                );
                if let Some(method) = extracted.method {
                    metadata.insert("extraction_method".into(), json!(method));
                }
                if let Some(truncated) = extracted.truncated {
                    metadata.insert("extraction_truncated".into(), json!(truncated));
                }
                if let Some(title) = extracted.title.as_ref() {
                    metadata.insert("reader_extracted_title".into(), json!(title));
                }
                let document = ContentDocument {
                    schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                    identity: request.candidate.identity.clone(),
                    title: extracted
                        .title
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| request.candidate.title.clone()),
                    text: extracted.text,
                    canonical_url: Some(canonical_url.clone()),
                    media_type,
                    fetched_at_ms: now_ms,
                    privacy: request.candidate.privacy,
                    content_hash,
                    provenance: super::types::ContentProvenance {
                        source_label: request.candidate.provenance.source_label.clone(),
                        source_url: Some(canonical_url),
                        retrieved_by: self.descriptor.adapter_id.clone(),
                    },
                    metadata,
                };
                document.validate()?;
                let response_validators = ConditionalHttpRequest {
                    etag,
                    last_modified,
                };
                self.cache
                    .put(
                        &request.principal,
                        &request.workspace,
                        &self.descriptor.adapter_id,
                        &self.capability_revision,
                        request.depth,
                        request_url,
                        &document,
                        &response_validators,
                    )
                    .await?;
                if document.canonical_url.as_deref() != Some(request_url) {
                    if let Some(final_url) = document.canonical_url.as_deref() {
                        self.cache
                            .put(
                                &request.principal,
                                &request.workspace,
                                &self.descriptor.adapter_id,
                                &self.capability_revision,
                                request.depth,
                                final_url,
                                &document,
                                &response_validators,
                            )
                            .await?;
                    }
                }
                let cache_outcome = if cached.is_some() {
                    "refreshed"
                } else {
                    "miss"
                };
                self.bind_document_to_candidate(document, request, cache_outcome)
            },
        }
    }

    fn bind_document_to_candidate(
        &self,
        mut document: ContentDocument,
        request: &ReadRequest,
        cache_outcome: &'static str,
    ) -> Result<ContentDocument> {
        document.identity = request.candidate.identity.clone();
        document.title = document
            .metadata
            .get("reader_extracted_title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| request.candidate.title.clone());
        document.privacy = request.candidate.privacy;
        document.provenance = super::types::ContentProvenance {
            source_label: request.candidate.provenance.source_label.clone(),
            source_url: document.canonical_url.clone(),
            retrieved_by: self.descriptor.adapter_id.clone(),
        };
        let reader_metadata = document.metadata;
        document.metadata = request.candidate.metadata.clone();
        for (key, value) in reader_metadata {
            document.metadata.insert(key, value);
        }
        document.metadata.insert(
            CONTENT_CACHE_OUTCOME_METADATA_KEY.to_string(),
            Value::String(cache_outcome.to_string()),
        );
        document.validate()?;
        Ok(document)
    }

    async fn extract(&self, request: &ReadRequest, body: Vec<u8>) -> Result<ExtractedContent> {
        let scratch = self
            .cache
            .scratch_path(&request.principal, &request.workspace);
        write_bytes_atomic(&scratch, &body).await.with_context(|| {
            format!("writing static-reader scratch file `{}`", scratch.display())
        })?;
        // The synchronous unlink in `Drop` also runs when this future is
        // cancelled while the extractor subprocess is still in flight.
        let _scratch_guard = ScratchFileGuard::new(scratch.clone());

        let invocation_result = async {
            let mut arguments = self
                .manifest
                .input
                .fixed_arguments
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Map<_, _>>();
            arguments.insert(
                self.manifest.input.input_file_argument.clone(),
                Value::String(scratch.to_string_lossy().into_owned()),
            );
            if let Some(argument) = &self.manifest.input.max_chars_argument {
                arguments.insert(
                    argument.clone(),
                    Value::from(self.manifest.reader.max_text_chars as u64),
                );
            }
            let invocation = DeterministicCapabilityInvocation::new(
                &self.manifest.capability.name,
                &self.manifest.capability.action,
                Value::Object(arguments),
                invocation_source(request.invocation_source),
            )
            .with_scope(&request.principal, &request.workspace)
            .with_brokered_workspace_path(
                &self.manifest.input.input_file_argument,
                &scratch,
                WorkspacePathAccess::ReadFile,
            );
            let result = self
                .invoker
                .invoke(invocation)
                .await
                .map_err(|error| match error {
                    crate::magician_v2::execution::ExecutionError::CapabilityFailure {
                        code,
                        message,
                    } => anyhow!(CapabilityReaderExecutionFailure { code, message }),
                    other => anyhow!(other.to_string()),
                })?;
            match self.manifest.output.mode {
                CapabilityReaderOutputMode::Json => {
                    let payload = result
                        .parsed_json()
                        .context("decoding capability reader output as JSON")?;
                    normalize_extractor_output(&self.manifest, &payload)
                },
                CapabilityReaderOutputMode::Text => normalize_text_extractor_output(
                    &self.manifest,
                    result
                        .output_text()
                        .ok_or_else(|| anyhow!("capability reader returned non-text output"))?,
                ),
            }
        }
        .await;
        invocation_result
    }

    async fn acquire_read_lock(&self, key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .read_locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(AsyncMutex::new(()));
                locks.insert(key.to_string(), Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }
}

#[async_trait]
impl ContentReader for CapabilityStaticContentReader {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn read(&self, request: &ReadRequest) -> Result<ContentDocument> {
        let request_url = request
            .candidate
            .canonical_url
            .as_deref()
            .ok_or_else(|| anyhow!("static content read requires a candidate canonical URL"))?;
        let request_url = canonicalize_http_url(request_url)?;
        let lock_key = format!(
            "{}\0{}\0{}",
            request.principal, request.workspace, request_url
        );
        let _guard = self.acquire_read_lock(&lock_key).await;
        self.read_locked(request, &request_url).await
    }
}

pub fn evaluate_extraction_quality(
    text: &str,
    depth: ReadDepth,
    min_gist_chars: usize,
    min_full_text_chars: usize,
) -> ExtractionQuality {
    let char_count = text.trim().chars().count();
    let word_count = text.split_whitespace().count();
    let required = match depth {
        ReadDepth::Gist => min_gist_chars,
        ReadDepth::FullText => min_full_text_chars,
    };
    let char_score = (char_count as f64 / required.max(1) as f64).min(1.0);
    let expected_words = (required / 6).max(1);
    let word_score = (word_count as f64 / expected_words as f64).min(1.0);
    let minimum_words = (required / 16).max(3);
    ExtractionQuality {
        char_count,
        word_count,
        score: (char_score * 0.75 + word_score * 0.25).clamp(0.0, 1.0),
        sufficient: char_count >= required && word_count >= minimum_words,
    }
}

pub fn load_capability_reader_manifest(path: &Path) -> Result<CapabilityReaderManifest> {
    load_optional_capability_reader_manifest(path)?.ok_or_else(|| {
        anyhow!(
            "governed skill `{}` does not declare metadata.magician.{CAPABILITY_READER_EXTENSION}",
            path.display()
        )
    })
}

pub fn load_optional_capability_reader_manifest(
    path: &Path,
) -> Result<Option<CapabilityReaderManifest>> {
    let Some(manifest) = load_skill_magician_extension::<CapabilityReaderManifest>(
        path,
        CAPABILITY_READER_EXTENSION,
    )?
    else {
        return Ok(None);
    };
    manifest.validate().with_context(|| {
        format!(
            "validating metadata.magician.{CAPABILITY_READER_EXTENSION} in `{}`",
            path.display()
        )
    })?;
    Ok(Some(manifest))
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn register_capability_reader_manifests(
    registry: &mut ContentSourceRegistry,
    invoker: Arc<dyn DeterministicCapabilityInvoker>,
    roots: &[PathBuf],
    cache_root: impl Into<PathBuf>,
) -> Result<Vec<String>> {
    let cache_root = cache_root.into();
    let paths = capability_reader_manifest_paths(roots)?;
    let existing = registry
        .reader_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.adapter_id)
        .collect::<BTreeSet<_>>();
    let mut manifests = Vec::new();
    let mut discovered = BTreeSet::new();
    for path in paths {
        let manifest = validated_capability_reader_manifest(&path)?;
        let id = manifest.reader.id.clone();
        if existing.contains(&id) || !discovered.insert(id.clone()) {
            bail!("duplicate capability content reader `{id}`");
        }
        manifests.push(manifest);
    }
    let ids = manifests
        .iter()
        .map(|manifest| manifest.reader.id.clone())
        .collect::<Vec<_>>();
    for manifest in manifests {
        registry.register_reader(Arc::new(CapabilityStaticContentReader::new(
            manifest,
            invoker.clone(),
            cache_root.clone(),
        )?))?;
    }
    Ok(ids)
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn validated_capability_reader_manifest(path: &Path) -> Result<CapabilityReaderManifest> {
    optional_validated_capability_reader_manifest(path)?.ok_or_else(|| {
        anyhow!(
            "governed skill `{}` does not declare metadata.magician.{CAPABILITY_READER_EXTENSION}",
            path.display()
        )
    })
}

pub fn optional_validated_capability_reader_manifest(
    path: &Path,
) -> Result<Option<CapabilityReaderManifest>> {
    let Some(manifest) = load_optional_capability_reader_manifest(path)? else {
        return Ok(None);
    };
    let owner = owning_skill_name(path)?;
    if owner != manifest.capability.name {
        bail!(
            "skill `{}` binds content reader capability `{}` but is owned by `{owner}`",
            path.display(),
            manifest.capability.name
        );
    }
    validate_owned_skill_files(path, &owner, &manifest)?;
    Ok(Some(manifest))
}

fn normalize_extractor_output(
    manifest: &CapabilityReaderManifest,
    payload: &Value,
) -> Result<ExtractedContent> {
    if let Some(pointer) = &manifest.output.error_pointer {
        if let Some(value) = payload.pointer(pointer) {
            let meaningful = match value {
                Value::Null => false,
                Value::Bool(value) => *value,
                Value::String(value) => !value.trim().is_empty(),
                Value::Array(values) => !values.is_empty(),
                Value::Object(values) => !values.is_empty(),
                Value::Number(_) => true,
            };
            if meaningful {
                bail!(
                    "content extraction capability failed: {}",
                    bounded_value(value)
                );
            }
        }
    }
    let text = payload
        .pointer(
            manifest
                .output
                .text_pointer
                .as_deref()
                .expect("JSON reader manifest validation requires text_pointer"),
        )
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("content extraction capability omitted readable text"))?
        .to_string();
    if text.chars().count() > manifest.reader.max_text_chars {
        bail!(
            "content extraction capability exceeded {} character limit",
            manifest.reader.max_text_chars
        );
    }
    let shell_reason = manifest
        .output
        .shell_pointer
        .as_deref()
        .and_then(|pointer| payload.pointer(pointer))
        .and_then(Value::as_bool)
        .filter(|shell| *shell)
        .map(|_| {
            pointer_string(payload, manifest.output.shell_reason_pointer.as_deref())
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or_else(|| {
                    "the extractor judged the page a client-rendered shell".to_string()
                })
        });
    Ok(ExtractedContent {
        text,
        title: pointer_string(payload, manifest.output.title_pointer.as_deref()),
        method: pointer_string(payload, manifest.output.method_pointer.as_deref()),
        shell_reason,
        truncated: manifest
            .output
            .truncated_pointer
            .as_deref()
            .and_then(|pointer| payload.pointer(pointer))
            .and_then(Value::as_bool),
    })
}

fn normalize_text_extractor_output(
    manifest: &CapabilityReaderManifest,
    output: &str,
) -> Result<ExtractedContent> {
    let text = output.trim();
    if text.is_empty() {
        bail!("content extraction capability omitted readable text");
    }
    if text.chars().count() > manifest.reader.max_text_chars {
        bail!(
            "content extraction capability exceeded {} character limit",
            manifest.reader.max_text_chars
        );
    }
    Ok(ExtractedContent {
        text: text.to_string(),
        title: None,
        method: Some(manifest.capability.name.clone()),
        truncated: None,
        shell_reason: None,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn capability_reader_manifest_paths(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    discover_skill_markdown_paths(roots)?
        .into_iter()
        .filter_map(|path| {
            match load_skill_magician_extension::<CapabilityReaderManifest>(
                &path,
                CAPABILITY_READER_EXTENSION,
            ) {
                Ok(Some(_)) => Some(Ok(path)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}

fn validate_owned_skill_files(
    path: &Path,
    owner: &str,
    manifest: &CapabilityReaderManifest,
) -> Result<()> {
    let skill_dir = path.parent().expect("SKILL.md path has parent");
    let skill_path = skill_dir.join("SKILL.md");
    let source = read_bounded_skill_markdown(&skill_path)?;
    let resolved_skill_path =
        crate::magician_v2::skills::path_rewrite::resolve_skill_path(&skill_path);
    let skill_manifest = crate::magician_v2::skills::loader::parse_manifest(&source, skill_dir)
        .with_context(|| format!("validating governed skill `{}`", skill_path.display()))?;
    let runtime_package = parse_skill_runtime_package(&source)
        .with_context(|| format!("decoding governed skill `{}`", skill_path.display()))?
        .ok_or_else(|| {
            anyhow!(
                "content reader skill `{owner}` requires runtime_contract and runtime_actions in \
                 the same SKILL.md"
            )
        })?;

    let pack = project_runtime_package_to_pack(
        &skill_manifest.name,
        &skill_manifest.description,
        None,
        skill_dir,
        resolved_skill_path.parent().unwrap_or(skill_dir),
        runtime_package,
    )
    .map_err(|error| anyhow!("invalid governed runtime contract for `{owner}`: {error}"))?;
    validate_governed_reader_owner(&pack, owner, manifest, &skill_path)
}

fn validate_governed_reader_owner(
    pack: &CapabilityPackDefinition,
    owner: &str,
    manifest: &CapabilityReaderManifest,
    skill_path: &Path,
) -> Result<()> {
    let action = manifest.capability.action.as_str();
    let source = format!("governed skill `{}`", skill_path.display());
    let action_schema: &NativeActionSchemaDef = pack
        .native_action_schemas
        .get(action)
        .ok_or_else(|| anyhow!("{source} does not declare native action `{action}`"))?;
    let deterministic_cli = matches!(
        &pack.implementation,
        ImplementationType::Primitive {
            provider_name: None,
            runtime_package: Some(_),
            ..
        }
    );
    validate_reader_action_contract(
        &source,
        owner,
        &pack.name,
        deterministic_cli,
        &action_schema.parameters,
        &action_schema.required,
        manifest,
    )
}

fn validate_reader_action_contract(
    source: &str,
    owner: &str,
    actual_name: &str,
    deterministic_cli: bool,
    parameters: &[String],
    required_parameters: &[String],
    manifest: &CapabilityReaderManifest,
) -> Result<()> {
    let action = manifest.capability.action.as_str();
    if actual_name != owner {
        bail!("{source} declares `{actual_name}` instead of owning skill `{owner}`");
    }
    if !deterministic_cli {
        bail!("{source} must use a deterministic CLI primitive implementation");
    }
    let declared = parameters.iter().cloned().collect::<BTreeSet<_>>();
    if declared.len() != parameters.len() {
        bail!("capability reader action `{action}` declares duplicate parameters");
    }
    let required = required_parameters.iter().cloned().collect::<BTreeSet<_>>();
    if required.len() != required_parameters.len() {
        bail!("capability reader action `{action}` declares duplicate required parameters");
    }
    if let Some(unknown) = required.difference(&declared).next() {
        bail!("capability reader action `{action}` requires undeclared parameter `{unknown}`");
    }
    let mut supplied = manifest
        .input
        .fixed_arguments
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    supplied.insert(manifest.input.input_file_argument.clone());
    if let Some(argument) = manifest.input.max_chars_argument.as_deref() {
        supplied.insert(argument.to_string());
    }
    if let Some(unknown) = supplied.difference(&declared).next() {
        bail!("capability reader supplies undeclared action parameter `{unknown}` for `{action}`");
    }
    if let Some(missing) = required
        .iter()
        .find(|argument| !supplied.contains(argument.as_str()))
    {
        bail!("capability reader omits required action parameter `{missing}` for `{action}`");
    }
    Ok(())
}

fn pointer_string(payload: &Value, pointer: Option<&str>) -> Option<String> {
    pointer
        .and_then(|pointer| payload.pointer(pointer))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn validate_pointer(pointer: &str, label: &str) -> Result<()> {
    if !pointer.starts_with('/') {
        bail!("capability reader {label} must be an RFC 6901 JSON pointer");
    }
    Ok(())
}

fn bounded_value(value: &Value) -> String {
    let serialized = value.to_string();
    serialized.chars().take(512).collect()
}

fn invocation_source(source: ContentInvocationSource) -> DeterministicCapabilityInvocationSource {
    match source {
        ContentInvocationSource::UserFeed => DeterministicCapabilityInvocationSource::UserFeed,
        ContentInvocationSource::RecurringMonitor => {
            DeterministicCapabilityInvocationSource::RecurringMonitor
        },
        ContentInvocationSource::ObservedSource => {
            DeterministicCapabilityInvocationSource::ObservedSource
        },
        ContentInvocationSource::InteractiveRead => {
            DeterministicCapabilityInvocationSource::InteractiveRead
        },
        ContentInvocationSource::InternalSystem => {
            DeterministicCapabilityInvocationSource::InternalSystem
        },
    }
}

fn default_web_page_class() -> ContentSourceClass {
    ContentSourceClass::WebPage
}
fn default_cache_ttl_secs() -> u64 {
    DEFAULT_CACHE_TTL_SECS
}
fn default_max_response_bytes() -> usize {
    DEFAULT_MAX_RESPONSE_BYTES
}
fn default_max_text_chars() -> usize {
    DEFAULT_MAX_TEXT_CHARS
}
fn default_min_gist_chars() -> usize {
    DEFAULT_MIN_GIST_CHARS
}
fn default_min_full_text_chars() -> usize {
    DEFAULT_MIN_FULL_TEXT_CHARS
}
fn default_fetch_timeout_secs() -> u64 {
    DEFAULT_FETCH_TIMEOUT_SECS
}
fn default_max_redirects() -> usize {
    DEFAULT_MAX_REDIRECTS
}
fn default_input_file_argument() -> String {
    "input_file".into()
}
fn default_max_chars_argument() -> Option<String> {
    Some("max_chars".into())
}
fn default_reader_media_types() -> Vec<String> {
    vec![
        "text/html".into(),
        "application/xhtml+xml".into(),
        "text/plain".into(),
    ]
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        collections::VecDeque,
        net::SocketAddr,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use reqwest::header::{HeaderMap, HeaderValue};
    use url::Url;

    use super::*;
    use crate::magician_v2::{
        content_sources::{
            public_http::{HttpHopResponse, HttpHopTransport, PublicAddressResolver},
            ContentCandidate, ContentPrivacy, ContentProvenance, ReadSelectionEvidence,
            ReadSelectionReason, RemoteDataPolicy, RetrievalOperation, RetrievalRung,
            SourceIdentity,
        },
        execution::{
            actions::ActionResult, primitive_dispatch::DeterministicCapabilityInvocationResult,
            ExecutionError,
        },
    };

    struct FakeInvoker {
        calls: AtomicUsize,
        output: Mutex<String>,
    }

    struct FailingInvoker;

    struct StaticResolver;

    #[async_trait]
    impl PublicAddressResolver for StaticResolver {
        async fn resolve(&self, _url: &Url) -> Result<Vec<SocketAddr>> {
            Ok(vec!["93.184.216.34:443".parse().unwrap()])
        }
    }

    struct QueueTransport {
        responses: Mutex<VecDeque<HttpHopResponse>>,
        conditionals: Mutex<Vec<ConditionalHttpRequest>>,
    }

    #[async_trait]
    impl HttpHopTransport for QueueTransport {
        async fn send(
            &self,
            _url: &Url,
            _pinned_addresses: &[SocketAddr],
            conditional: &ConditionalHttpRequest,
            _policy: &PublicHttpFetchPolicy,
        ) -> Result<HttpHopResponse> {
            self.conditionals.lock().unwrap().push(conditional.clone());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow!("unexpected HTTP fetch"))
        }
    }

    #[async_trait]
    impl DeterministicCapabilityInvoker for FakeInvoker {
        async fn invoke(
            &self,
            invocation: DeterministicCapabilityInvocation,
        ) -> std::result::Result<DeterministicCapabilityInvocationResult, ExecutionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let args = invocation.arguments.as_object().unwrap();
            assert!(args.contains_key("input_file"));
            if invocation.capability_name == "pdftotext" {
                assert_eq!(args.get("output_file"), Some(&json!("-")));
                assert_eq!(args.get("flags"), Some(&json!("-nopgbrk")));
                assert!(!args.contains_key("max_chars"));
            } else {
                assert_eq!(args.get("max_chars").and_then(Value::as_u64), Some(4096));
            }
            Ok(DeterministicCapabilityInvocationResult {
                output: ActionResult::text(self.output.lock().unwrap().clone()),
                duration_ms: 1,
            })
        }
    }

    #[async_trait]
    impl DeterministicCapabilityInvoker for FailingInvoker {
        async fn invoke(
            &self,
            _invocation: DeterministicCapabilityInvocation,
        ) -> std::result::Result<DeterministicCapabilityInvocationResult, ExecutionError> {
            Err(ExecutionError::CapabilityFailure {
                code: "unsupported".to_string(),
                message: "scanned PDF requires OCR".to_string(),
            })
        }
    }

    fn manifest() -> CapabilityReaderManifest {
        CapabilityReaderManifest::from_yaml_str(
            r#"
schema_version: 1
reader:
  id: static-http
  display_name: Static HTTP
  max_response_bytes: 4096
  max_text_chars: 4096
  min_gist_chars: 20
  min_full_text_chars: 40
capability:
  name: htmltotext
  action: run
input:
  fixed_arguments:
    include_links: true
output:
  text_pointer: /content
  method_pointer: /method
  truncated_pointer: /truncated
  error_pointer: /error
  shell_pointer: /client_rendered/shell
  shell_reason_pointer: /client_rendered/reason
"#,
        )
        .unwrap()
    }

    fn embedded_reader(source: &str) -> CapabilityReaderManifest {
        tool_runtime_core::manifest_parser::parse_skill_magician_extension(
            source,
            CAPABILITY_READER_EXTENSION,
        )
        .unwrap()
        .unwrap()
    }

    fn hop_response(status: u16, etag: Option<&str>, body: &[u8]) -> HttpHopResponse {
        hop_response_with_media(status, etag, "text/html; charset=utf-8", body)
    }

    fn hop_response_with_media(
        status: u16,
        etag: Option<&str>,
        media_type: &'static str,
        body: &[u8],
    ) -> HttpHopResponse {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static(media_type),
        );
        if let Some(etag) = etag {
            headers.insert(reqwest::header::ETAG, HeaderValue::from_str(etag).unwrap());
        }
        HttpHopResponse {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    fn read_request(freshness: FreshnessPolicy) -> ReadRequest {
        ReadRequest {
            principal: "p".into(),
            workspace: "w".into(),
            candidate: ContentCandidate {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: SourceIdentity::new("exa", "story").unwrap(),
                title: "Story".into(),
                cheap_text: "A selected story".into(),
                canonical_url: Some("https://example.com/story".into()),
                published_at_ms: None,
                observed_at_ms: 1,
                privacy: ContentPrivacy::Public,
                content_hash: None,
                provenance: ContentProvenance {
                    source_label: "Example".into(),
                    source_url: Some("https://example.com/story".into()),
                    retrieved_by: "exa".into(),
                },
                metadata: BTreeMap::new(),
            },
            depth: ReadDepth::FullText,
            freshness,
            remote_content_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::UserFeed,
            selection: Some(ReadSelectionEvidence {
                selected_at_ms: 1,
                reason: ReadSelectionReason::FeedMatch,
                relevance_score: Some(0.9),
            }),
            authority_grant_id: None,
        }
    }

    #[test]
    fn quality_gate_is_depth_sensitive_without_content_specific_terms() {
        let text = "A concise paragraph with enough neutral words to provide a useful gist.";
        let gist = evaluate_extraction_quality(text, ReadDepth::Gist, 20, 120);
        let full = evaluate_extraction_quality(text, ReadDepth::FullText, 20, 120);
        assert!(gist.sufficient);
        assert!(!full.sufficient);
        assert!(gist.score > full.score);
    }

    #[tokio::test]
    async fn capability_failure_survives_reader_context_for_typed_routing() {
        let temp = tempfile::tempdir().unwrap();
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([hop_response(
                200,
                None,
                b"%PDF-1.4 fixture",
            )])),
            conditionals: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            transport,
        )
        .unwrap();
        let reader = CapabilityStaticContentReader::with_fetcher(
            manifest(),
            Arc::new(FailingInvoker),
            temp.path(),
            fetcher,
        )
        .unwrap();

        let error = reader
            .read(&read_request(FreshnessPolicy::Fresh))
            .await
            .unwrap_err();
        let failure = error
            .downcast_ref::<CapabilityReaderExecutionFailure>()
            .expect("stable capability failure type should survive anyhow context");
        assert_eq!(failure.code, "unsupported");
        assert_eq!(failure.message, "scanned PDF requires OCR");
    }

    #[test]
    fn quality_gate_rejects_long_non_prose_shells() {
        let text = "x".repeat(300);
        let quality = evaluate_extraction_quality(&text, ReadDepth::FullText, 20, 200);
        assert!(!quality.sufficient);
    }

    #[test]
    fn manifest_rejects_unknown_fields_and_invalid_pointers() {
        let unknown = r#"
schema_version: 1
reader: {id: static, display_name: Static, surprise: true}
capability: {name: htmltotext, action: run}
output: {text_pointer: /content}
"#;
        assert!(CapabilityReaderManifest::from_yaml_str(unknown).is_err());
        let mut invalid = manifest();
        invalid.output.text_pointer = Some("content".into());
        assert!(invalid.validate().is_err());
        let mut unbounded_media = manifest();
        unbounded_media.reader.accepted_media_types.clear();
        assert!(unbounded_media.validate().is_err());
        let mut colliding_inputs = manifest();
        colliding_inputs
            .input
            .fixed_arguments
            .insert("input_file".into(), json!("/tmp/override"));
        assert!(colliding_inputs.validate().is_err());
    }

    #[test]
    fn extractor_output_is_bounded_and_surfaces_errors() {
        let manifest = manifest();
        assert!(normalize_extractor_output(
            &manifest,
            &json!({"error": "provider failed", "content": "ignored"})
        )
        .is_err());
        assert!(normalize_extractor_output(&manifest, &json!({"content": ""})).is_err());
        let output = normalize_extractor_output(
            &manifest,
            &json!({"content": "useful extracted text", "method": "trafilatura"}),
        )
        .unwrap();
        assert_eq!(output.method.as_deref(), Some("trafilatura"));

        let pdf = embedded_reader(include_str!("../../../../skillshub/pdftotext/SKILL.md"));
        let output = normalize_text_extractor_output(
            &pdf,
            "Extracted PDF text with enough deterministic content.",
        )
        .unwrap();
        assert_eq!(output.method.as_deref(), Some("pdftotext"));
        assert!(normalize_text_extractor_output(&pdf, "  ").is_err());
    }

    #[test]
    fn anydoc_reader_manifest_routes_supported_documents_to_json_content() {
        let manifest = embedded_reader(include_str!(
            "../../../../skillshub/document-to-markdown/SKILL.md"
        ));

        assert_eq!(manifest.capability.name, "document-to-markdown");
        assert_eq!(manifest.capability.action, "convert");
        assert_eq!(
            manifest
                .reader
                .retrieval
                .as_ref()
                .map(|retrieval| retrieval.action_id.as_str()),
            Some("document_markdown.read")
        );
        assert!(manifest
            .reader
            .accepted_media_types
            .iter()
            .any(|media_type| media_type == "application/pdf"));
        assert!(manifest
            .reader
            .accepted_media_types
            .iter()
            .any(|media_type| {
                media_type
                    == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }));
        assert_eq!(manifest.output.mode, CapabilityReaderOutputMode::Json);
        assert_eq!(manifest.output.text_pointer.as_deref(), Some("/content"));
        assert_eq!(
            manifest.input.max_chars_argument.as_deref(),
            Some("max_chars")
        );
        assert_eq!(
            manifest.input.fixed_arguments.get("format"),
            Some(&json!("auto"))
        );
    }

    #[test]
    fn loader_registers_typed_reader_without_provider_specific_rust() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("htmltotext");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            include_str!("../../../../skillshub/htmltotext/SKILL.md"),
        )
        .unwrap();
        let paths = capability_reader_manifest_paths(&[temp.path().to_path_buf()]).unwrap();
        assert_eq!(paths, vec![skill.join("SKILL.md")]);
        assert!(load_capability_reader_manifest(&paths[0]).is_ok());

        let invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(json!({"content": "unused"}).to_string()),
        });
        let mut registry = ContentSourceRegistry::new();
        let ids = register_capability_reader_manifests(
            &mut registry,
            invoker.clone(),
            &[temp.path().to_path_buf()],
            temp.path().join("cache"),
        )
        .unwrap();
        assert_eq!(ids, vec!["static-http"]);

        std::fs::write(
            skill.join("SKILL.md"),
            include_str!("../../../../skillshub/htmltotext/SKILL.md").replacen(
                "action: run",
                "action: missing",
                1,
            ),
        )
        .unwrap();
        let mut invalid_registry = ContentSourceRegistry::new();
        assert!(register_capability_reader_manifests(
            &mut invalid_registry,
            invoker,
            &[temp.path().to_path_buf()],
            temp.path().join("invalid-cache"),
        )
        .is_err());
    }

    #[test]
    fn shipped_reader_skills_have_valid_retrieval_contracts() {
        let skill_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../skillshub");
        let paths = capability_reader_manifest_paths(&[skill_root]).unwrap();
        assert_eq!(paths.len(), 4);

        let actions = paths
            .iter()
            .map(|path| {
                let manifest = validated_capability_reader_manifest(path).unwrap();
                let retrieval = manifest.reader.retrieval.as_ref().unwrap();
                assert_eq!(retrieval.operation, RetrievalOperation::Read);
                assert_eq!(retrieval.rung, RetrievalRung::PublicStatic);
                retrieval.action_id.clone()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actions,
            BTreeSet::from([
                "document_markdown.read".into(),
                "pdf_text.read".into(),
                "static_http.read".into(),
                "structured_schema.read".into(),
            ])
        );
    }

    #[test]
    fn scoped_skill_without_reader_manifest_suppresses_lower_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let scoped_root = temp.path().join("scoped");
        let system_root = temp.path().join("system");
        let scoped_skill = scoped_root.join("htmltotext");
        let system_skill = system_root.join("htmltotext");
        std::fs::create_dir_all(&scoped_skill).unwrap();
        std::fs::create_dir_all(&system_skill).unwrap();
        std::fs::write(
            scoped_skill.join("SKILL.md"),
            "---\nname: htmltotext\ndescription: scoped override\n---\n",
        )
        .unwrap();
        std::fs::write(
            system_skill.join("SKILL.md"),
            include_str!("../../../../skillshub/htmltotext/SKILL.md"),
        )
        .unwrap();

        let paths = capability_reader_manifest_paths(&[scoped_root, system_root]).unwrap();
        assert!(paths.is_empty());
    }

    #[tokio::test]
    async fn selected_read_uses_cache_then_conditionally_revalidates_without_reextracting() {
        let temp = tempfile::tempdir().unwrap();
        let invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(json!({
                "content": "This is a sufficiently long extracted article body for the full text quality gate.",
                "method": "trafilatura",
                "truncated": false
            })
            .to_string()),
        });
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                hop_response(200, Some("\"v1\""), b"<html>first</html>"),
                hop_response(304, Some("\"v1\""), b""),
            ])),
            conditionals: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            transport.clone(),
        )
        .unwrap();
        let reader = Arc::new(
            CapabilityStaticContentReader::with_fetcher(
                manifest(),
                invoker.clone(),
                temp.path(),
                fetcher,
            )
            .unwrap(),
        );
        let mut registry = ContentSourceRegistry::new();
        registry.register_reader(reader).unwrap();

        let first = registry
            .read("static-http", &read_request(FreshnessPolicy::CachedOk))
            .await
            .unwrap();
        let cached = registry
            .read("static-http", &read_request(FreshnessPolicy::CachedOk))
            .await
            .unwrap();
        let mut rebound_request = read_request(FreshnessPolicy::CachedOk);
        rebound_request.candidate.identity = SourceIdentity::new("rss", "story-rss").unwrap();
        rebound_request
            .candidate
            .metadata
            .insert("source_lane".into(), json!("rss"));
        let rebound = registry
            .read("static-http", &rebound_request)
            .await
            .unwrap();
        let revalidated = registry
            .read("static-http", &read_request(FreshnessPolicy::Fresh))
            .await
            .unwrap();

        assert_eq!(first.content_hash, cached.content_hash);
        assert_eq!(rebound.identity.adapter_id, "rss");
        assert_eq!(rebound.metadata["source_lane"], json!("rss"));
        assert_eq!(cached.content_hash, revalidated.content_hash);
        assert_eq!(
            first.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            json!("miss")
        );
        assert_eq!(
            cached.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            json!("fresh_hit")
        );
        assert_eq!(
            revalidated.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            json!("revalidated")
        );
        assert_eq!(invoker.calls.load(Ordering::SeqCst), 1);
        let conditionals = transport.conditionals.lock().unwrap();
        assert_eq!(conditionals.len(), 2);
        assert_eq!(conditionals[1].etag.as_deref(), Some("\"v1\""));
    }

    #[tokio::test]
    async fn pdf_reader_uses_the_same_secure_fetch_cache_and_local_extractor_path() {
        let temp = tempfile::tempdir().unwrap();
        let invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(
                "A public PDF report documents deterministic retrieval, bounded fallback, \
                 cancellation, provenance, cache behavior, policy enforcement, extraction \
                 quality, and operational evidence. "
                    .repeat(4),
            ),
        });
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([hop_response_with_media(
                200,
                Some("\"pdf-v1\""),
                "application/pdf",
                b"%PDF-1.7 fixture",
            )])),
            conditionals: Mutex::new(Vec::new()),
        });
        let manifest = embedded_reader(include_str!("../../../../skillshub/pdftotext/SKILL.md"));
        let fetcher = PublicHttpFetcher::with_components(
            manifest.http_policy().unwrap(),
            Arc::new(StaticResolver),
            transport,
        )
        .unwrap();
        let reader = CapabilityStaticContentReader::with_fetcher(
            manifest,
            invoker.clone(),
            temp.path(),
            fetcher,
        )
        .unwrap();
        let mut request = read_request(FreshnessPolicy::CachedOk);
        request.candidate.canonical_url = Some("https://example.com/report.pdf".into());

        let document = reader.read(&request).await.unwrap();

        assert_eq!(document.media_type.as_deref(), Some("application/pdf"));
        assert_eq!(document.provenance.retrieved_by, "pdf-text");
        assert_eq!(
            document.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            json!("miss")
        );
        assert_eq!(invoker.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn capability_revision_change_forces_refetch_and_reextraction() {
        let temp = tempfile::tempdir().unwrap();
        let first_invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(json!({
                "content": "The first revision produced a sufficiently long extracted article body for cache isolation."
            })
            .to_string()),
        });
        let first_transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([hop_response(
                200,
                Some("\"v1\""),
                b"<html>revision one</html>",
            )])),
            conditionals: Mutex::new(Vec::new()),
        });
        let first_fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            first_transport,
        )
        .unwrap();
        let first = CapabilityStaticContentReader::with_fetcher_and_revision(
            manifest(),
            first_invoker.clone(),
            temp.path(),
            first_fetcher,
            "revision-1",
        )
        .unwrap();
        let request = read_request(FreshnessPolicy::CachedOk);
        let first_document = first.read(&request).await.unwrap();

        let second_invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(json!({
                "content": "The second revision produced a different sufficiently long extracted article body for cache isolation."
            })
            .to_string()),
        });
        let second_transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([hop_response(
                200,
                Some("\"v2\""),
                b"<html>revision two</html>",
            )])),
            conditionals: Mutex::new(Vec::new()),
        });
        let second_fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            second_transport.clone(),
        )
        .unwrap();
        let second = CapabilityStaticContentReader::with_fetcher_and_revision(
            manifest(),
            second_invoker.clone(),
            temp.path(),
            second_fetcher,
            "revision-2",
        )
        .unwrap();
        let second_document = second.read(&request).await.unwrap();

        assert_ne!(first_document.content_hash, second_document.content_hash);
        assert_eq!(first_invoker.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_invoker.calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_transport.conditionals.lock().unwrap().len(), 1);
        assert_eq!(
            second_document.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            json!("miss")
        );
    }

    #[tokio::test]
    async fn cached_ok_uses_stale_content_when_refresh_extraction_fails_but_fresh_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(
                json!({
                    "content": "This is a sufficiently long cached article body retained across a failed refresh."
                })
                .to_string(),
            ),
        });
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                hop_response(200, Some("\"v1\""), b"<html>first</html>"),
                hop_response(200, Some("\"v2\""), b"<html>changed</html>"),
                hop_response(200, Some("\"v2\""), b"<html>changed</html>"),
            ])),
            conditionals: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            transport,
        )
        .unwrap();
        let reader = Arc::new(
            CapabilityStaticContentReader::with_fetcher(
                manifest(),
                invoker.clone(),
                temp.path(),
                fetcher,
            )
            .unwrap(),
        );
        let request = read_request(FreshnessPolicy::CachedOk);
        let first = reader.read(&request).await.unwrap();
        let mut stale = reader
            .cache
            .get(
                "p",
                "w",
                &reader.descriptor.adapter_id,
                &reader.capability_revision,
                request.depth,
                "https://example.com/story",
            )
            .await
            .unwrap()
            .unwrap()
            .document;
        stale.fetched_at_ms = 1;
        reader
            .cache
            .put(
                "p",
                "w",
                &reader.descriptor.adapter_id,
                &reader.capability_revision,
                request.depth,
                "https://example.com/story",
                &stale,
                &ConditionalHttpRequest::default(),
            )
            .await
            .unwrap();
        *invoker.output.lock().unwrap() =
            json!({"error": "extractor unavailable", "content": ""}).to_string();

        let fallback = reader.read(&request).await.unwrap();
        assert_eq!(fallback.content_hash, first.content_hash);

        let fresh = read_request(FreshnessPolicy::Fresh);
        assert!(reader.read(&fresh).await.is_err());
        assert_eq!(invoker.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn noncontent_shell_is_rejected_before_cache_write() {
        let temp = tempfile::tempdir().unwrap();
        let invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(
                json!({"content": "Subscribe to continue. ".repeat(20)}).to_string(),
            ),
        });
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                hop_response(200, Some("\"shell\""), b"<html>shell</html>"),
                hop_response(200, Some("\"article\""), b"<html>article</html>"),
            ])),
            conditionals: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            transport,
        )
        .unwrap();
        let reader = CapabilityStaticContentReader::with_fetcher(
            manifest(),
            invoker.clone(),
            temp.path(),
            fetcher,
        )
        .unwrap();
        let request = read_request(FreshnessPolicy::CachedOk);

        assert!(reader.read(&request).await.is_err());
        assert!(reader
            .cache
            .get(
                "p",
                "w",
                &reader.descriptor.adapter_id,
                &reader.capability_revision,
                request.depth,
                "https://example.com/story",
            )
            .await
            .unwrap()
            .is_none());

        *invoker.output.lock().unwrap() = json!({
            "content": "A complete article with enough distinct public information, implementation detail, provenance, context, and supporting evidence for the full-text quality boundary."
        })
        .to_string();
        let recovered = reader.read(&request).await.unwrap();
        assert_eq!(
            recovered.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            json!("miss")
        );
        assert_eq!(invoker.calls.load(Ordering::SeqCst), 2);
    }

    /// The extractor can see what a character count cannot: a page that
    /// shipped its content for client-side hydration and rendered a blurb.
    /// Its verdict must become the JavaScript-shell failure the retrieval
    /// ladder already escalates to a browser handoff — with the reason in
    /// the message — and nothing of the shell may be cached as a page.
    #[tokio::test]
    async fn a_client_rendered_shell_reported_by_the_extractor_fails_as_javascript_required_and_is_not_cached(
    ) {
        let temp = tempfile::tempdir().unwrap();
        let blurb = "Buy credits once and use them anywhere across every supported API from one shared balance. ".repeat(6);
        let invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(
                json!({
                    "content": blurb,
                    "client_rendered": {
                        "shell": true,
                        "reason": "1949 characters of content in client hydration payloads never rendered on the server, against 458 rendered",
                    },
                })
                .to_string(),
            ),
        });
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([hop_response(
                200,
                Some("\"island\""),
                b"<html><astro-island props='{}'></astro-island></html>",
            )])),
            conditionals: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            transport,
        )
        .unwrap();
        let reader = CapabilityStaticContentReader::with_fetcher(
            manifest(),
            invoker.clone(),
            temp.path(),
            fetcher,
        )
        .unwrap();
        let request = read_request(FreshnessPolicy::CachedOk);

        let error = reader
            .read(&request)
            .await
            .expect_err("a shell is not a page");
        let message = format!("{error:#}");
        assert!(message.contains("javascript_shell"), "{message}");
        assert!(message.contains("client hydration payloads"), "{message}");
        assert!(reader
            .cache
            .get(
                "p",
                "w",
                &reader.descriptor.adapter_id,
                &reader.capability_revision,
                request.depth,
                "https://example.com/story",
            )
            .await
            .unwrap()
            .is_none());
    }

    /// An extractor that says the page is not a shell, or says nothing
    /// (an older extractor without the field), is read as before.
    #[test]
    fn an_absent_or_negative_shell_verdict_changes_nothing() {
        let manifest = manifest();
        let body = "A complete article with enough distinct public information for the boundary.";
        let without = normalize_extractor_output(&manifest, &json!({"content": body})).unwrap();
        assert!(without.shell_reason.is_none());
        let negative = normalize_extractor_output(
            &manifest,
            &json!({"content": body, "client_rendered": {"shell": false, "reason": ""}}),
        )
        .unwrap();
        assert!(negative.shell_reason.is_none());
        let positive = normalize_extractor_output(
            &manifest,
            &json!({"content": body, "client_rendered": {"shell": true, "reason": "empty application root"}}),
        )
        .unwrap();
        assert_eq!(
            positive.shell_reason.as_deref(),
            Some("empty application root")
        );
    }

    #[tokio::test]
    async fn concurrent_reads_for_one_scope_and_url_are_coalesced() {
        let temp = tempfile::tempdir().unwrap();
        let invoker = Arc::new(FakeInvoker {
            calls: AtomicUsize::new(0),
            output: Mutex::new(json!({
                "content": "This is another sufficiently long extracted article body for coalescing coverage."
            })
            .to_string()),
        });
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([hop_response(
                200,
                None,
                b"<html>coalesced</html>",
            )])),
            conditionals: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            manifest().http_policy().unwrap(),
            Arc::new(StaticResolver),
            transport.clone(),
        )
        .unwrap();
        let reader = CapabilityStaticContentReader::with_fetcher(
            manifest(),
            invoker.clone(),
            temp.path(),
            fetcher,
        )
        .unwrap();
        let request = read_request(FreshnessPolicy::CachedOk);
        let (left, right) = tokio::join!(reader.read(&request), reader.read(&request));
        assert_eq!(left.unwrap().content_hash, right.unwrap().content_hash);
        assert_eq!(invoker.calls.load(Ordering::SeqCst), 1);
        assert_eq!(transport.conditionals.lock().unwrap().len(), 1);
    }

    #[test]
    fn scratch_guard_unlinks_file_when_extraction_future_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("cancelled.html");
        std::fs::write(&scratch, "downloaded page").unwrap();
        {
            let _guard = ScratchFileGuard::new(scratch.clone());
            assert!(scratch.exists());
        }
        assert!(!scratch.exists());
    }
}
