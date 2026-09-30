use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Instant,
};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use super::{
    public_http::validate_public_browser_url,
    retrieval::{
        BrowserRetrievalSettings, RetrievalHandoff, RetrievalHandoffKind, RetrievalHandoffRequired,
        RetrievalRuntimeState, RetrievalTransportFailure, RetrievalTransportTrace,
        META_ACTUAL_TRANSPORT, META_BROWSER_ENGINE, META_BROWSER_SESSION_FINGERPRINT,
        META_EXTRACT_MS, META_RENDER_MS, META_REQUESTED_BROWSER_MODE, META_RESOLVED_BROWSER_MODE,
        META_SESSION_OUTCOME,
    },
    AdapterAuth, AdapterExecution, ContentDocument, ContentPrivacy, ContentProvenance,
    ContentSourceCapabilities, ContentSourceClass, ContentSourceDescriptor, DiscoveryAdapter,
    DiscoveryPage, DiscoveryRequest, ReadRequest, RetrievalActionMetadata, RetrievalAuthority,
    RetrievalOutputKind, RetrievalRung, CONTENT_SOURCE_SCHEMA_VERSION,
};
use crate::magician_v2::{
    browser_engine_analytics::BrowserEngineAnalyticsContext,
    execution::primitive_dispatch::browser::{
        resolve_browser_engine_plan, AgentBrowserSession, ControllerOwnedBrowserSession,
        RetrievalBrowserMode, BUNDLED_BROWSER_ENGINE_NAME, LIGHTPANDA_BROWSER_ENGINE_NAME,
    },
};

pub const HEADLESS_READER_ID: &str = "browser-headless-reader";
pub const HEADLESS_READ_ACTION: &str = "browser.headless.read";
pub const CDP_READER_ID: &str = "browser-cdp-reader";
pub const CDP_READ_ACTION: &str = "browser.cdp.read";
const META_BROWSER_ENGINE_FALLBACK_FROM: &str = "_retrieval_browser_engine_fallback_from";
const META_BROWSER_ENGINE_FALLBACK: &str = "_retrieval_browser_engine_fallback";
const META_BROWSER_ENGINE_ATTEMPTS: &str = "_retrieval_browser_engine_attempts";

/// Lightpanda currently owns one process-wide browser service and rejects
/// overlapping sessions. Keep this limit at the engine boundary: vectorized
/// reads can still perform cache/static work concurrently, while only branches
/// that actually reach Lightpanda wait for its single permit.
fn lightpanda_retrieval_gate() -> Arc<Semaphore> {
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(Semaphore::new(1))))
}

fn retrieval_engine_is_serial(engine: &str) -> bool {
    engine == LIGHTPANDA_BROWSER_ENGINE_NAME
}

#[derive(Debug, Clone)]
pub struct BrowserContentReader {
    descriptor: ContentSourceDescriptor,
    mode: RetrievalBrowserMode,
    settings: BrowserRetrievalSettings,
    cli_path: PathBuf,
    storage_root: PathBuf,
    principal: String,
    workspace: String,
    authority_state: Arc<RetrievalRuntimeState>,
    #[cfg(any(test, feature = "test-fixtures"))]
    validate_public_dns: bool,
}

impl BrowserContentReader {
    pub fn public_headless(
        settings: BrowserRetrievalSettings,
        cli_path: PathBuf,
        storage_root: PathBuf,
        principal: &str,
        workspace: &str,
        authority_state: Arc<RetrievalRuntimeState>,
    ) -> Self {
        Self {
            descriptor: browser_reader_descriptor(
                HEADLESS_READER_ID,
                "Isolated rendered web reader",
                HEADLESS_READ_ACTION,
                RetrievalRung::PublicRendered,
                RetrievalAuthority::PublicBrowserRead,
                AdapterAuth::None,
            ),
            mode: RetrievalBrowserMode::PublicHeadlessRead,
            settings,
            cli_path,
            storage_root,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            authority_state,
            #[cfg(any(test, feature = "test-fixtures"))]
            validate_public_dns: true,
        }
    }

    pub fn authenticated_cdp(
        settings: BrowserRetrievalSettings,
        cli_path: PathBuf,
        storage_root: PathBuf,
        principal: &str,
        workspace: &str,
        authority_state: Arc<RetrievalRuntimeState>,
    ) -> Self {
        Self {
            descriptor: browser_reader_descriptor(
                CDP_READER_ID,
                "Approved authenticated browser reader",
                CDP_READ_ACTION,
                RetrievalRung::Authenticated,
                RetrievalAuthority::AuthenticatedRead,
                AdapterAuth::Required,
            ),
            mode: RetrievalBrowserMode::AuthenticatedCdpRead,
            settings,
            cli_path,
            storage_root,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            authority_state,
            #[cfg(any(test, feature = "test-fixtures"))]
            validate_public_dns: true,
        }
    }

    async fn validated_public_url(&self, raw: &str) -> Result<String> {
        #[cfg(any(test, feature = "test-fixtures"))]
        if !self.validate_public_dns {
            return super::public_http::validate_public_http_url(raw).map(|url| url.to_string());
        }
        validate_public_browser_url(raw)
            .await
            .map(|url| url.to_string())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn without_dns_resolution_for_test(mut self) -> Self {
        self.validate_public_dns = false;
        self
    }

    fn claim_request_authority(
        &self,
        request: &ReadRequest,
        target_url: &str,
        operation_id: &str,
    ) -> Result<()> {
        if self.mode == RetrievalBrowserMode::PublicHeadlessRead {
            if request.candidate.privacy != ContentPrivacy::Public {
                bail!("public headless reader cannot open non-public content");
            }
            return Ok(());
        }
        let Some(claim) = self.authority_state.claim_authority_grant(
            request.authority_grant_id.as_deref(),
            &request.principal,
            &request.workspace,
            self.descriptor.retrieval.authority,
            &self.descriptor.retrieval.action_id,
            target_url,
            operation_id,
        ) else {
            bail!(
                "authentication required: scoped browser authority grant is absent, stale, or \
                 already used"
            );
        };
        if !claim.private_content_to_assistant {
            bail!(
                "authentication required: approval does not permit returning private page content \
                 to the assistant"
            );
        }
        Ok(())
    }

    fn engine_attempts(&self) -> Vec<Option<String>> {
        if self.mode != RetrievalBrowserMode::PublicHeadlessRead {
            return vec![self.settings.engine.clone()];
        }
        let Some(preferred) = self.settings.public_read_engine.clone() else {
            return vec![self.settings.engine.clone()];
        };

        let mut attempts: Vec<Option<String>> = Vec::new();
        for candidate in [Some(preferred), self.settings.engine.clone(), None] {
            let label = browser_engine_label(candidate.as_deref());
            if attempts
                .iter()
                .all(|engine| browser_engine_label(engine.as_deref()) != label)
            {
                attempts.push(candidate);
            }
        }
        attempts
    }

    async fn read_with_engine(
        &self,
        request: &ReadRequest,
        target_url: &str,
        initial_url: &str,
        requested_engine: Option<&str>,
        fallback_from: Option<String>,
    ) -> Result<ContentDocument> {
        let session_id = format!(
            "retrieval-{}-{}",
            self.mode.label().replace('_', "-"),
            uuid::Uuid::new_v4().simple()
        );
        self.claim_request_authority(request, target_url, &session_id)?;
        let connection_mode = self.mode.connection_mode(&self.settings.cdp_url);
        let engine_plan = resolve_browser_engine_plan(
            &self.storage_root,
            &self.principal,
            &self.workspace,
            &connection_mode,
            requested_engine,
        )
        .map_err(|error| {
            browser_setup_failure(
                error,
                self.mode,
                requested_engine.map(str::to_string),
                "engine_resolution_failed",
            )
        })?;
        let initial_engine = if self.mode.is_identity_bearing() {
            "magicutor_cdp".to_string()
        } else {
            engine_plan
                .primary
                .name
                .clone()
                .unwrap_or_else(|| "bundled_chrome".to_string())
        };
        let _engine_permit = if retrieval_engine_is_serial(&initial_engine) {
            Some(
                lightpanda_retrieval_gate()
                    .acquire_owned()
                    .await
                    .map_err(|_| anyhow!("Lightpanda retrieval concurrency gate closed"))?,
            )
        } else {
            None
        };
        let session = AgentBrowserSession::new_retrieval(
            session_id.clone(),
            self.mode,
            &self.settings.cdp_url,
            self.cli_path.clone(),
        )
        .map_err(|error| {
            browser_setup_failure(
                error,
                self.mode,
                Some(initial_engine.clone()),
                "session_initialization_failed",
            )
        })?
        .with_initial_url(Some(initial_url.to_string()))
        .with_engine_plan(engine_plan)
        .with_analytics_context(
            BrowserEngineAnalyticsContext::for_scope(
                &self.storage_root,
                &self.principal,
                &self.workspace,
                None,
                None,
            )
            .with_work("content_read", request.candidate.identity.item_id.clone()),
        )
        .with_analytics_fallback_from(fallback_from)
        .with_capture_limit_bytes(self.settings.max_capture_bytes);
        let owned = ControllerOwnedBrowserSession::new(session);
        let session_fingerprint = blake3::hash(session_id.as_bytes()).to_hex().to_string();
        let extraction = async {
            let render_started = Instant::now();
            owned.session().ensure_connected().await?;
            run_required(
                owned.session(),
                &["wait", "--load", "networkidle"],
                self.settings.command_timeout_secs,
            )
            .await?;
            let render_wait = self.settings.render_wait_ms.to_string();
            run_required(
                owned.session(),
                &["wait", &render_wait],
                self.settings.command_timeout_secs,
            )
            .await?;
            let render_ms = elapsed_ms(render_started);

            let extract_started = Instant::now();
            let title = run_required(
                owned.session(),
                &["get", "title"],
                self.settings.command_timeout_secs,
            )
            .await?;
            let final_url = run_required(
                owned.session(),
                &["get", "url"],
                self.settings.command_timeout_secs,
            )
            .await?;
            let text = run_required(
                owned.session(),
                &["get", "text", "body"],
                self.settings.command_timeout_secs,
            )
            .await?;
            let extract_ms = elapsed_ms(extract_started);

            let final_url = final_url.stdout.trim();
            let canonical_url = if final_url.is_empty() {
                initial_url.to_string()
            } else if self.mode == RetrievalBrowserMode::PublicHeadlessRead {
                self.validated_public_url(final_url).await?
            } else {
                let final_domain = url::Url::parse(final_url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
                let granted_domain = url::Url::parse(target_url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
                if final_domain != granted_domain {
                    bail!("authenticated browser read redirected outside its approved domain");
                }
                final_url.to_string()
            };
            let text = bounded_text(text.stdout.trim(), self.settings.max_document_chars);
            if text.trim().is_empty() {
                bail!("browser extraction returned empty page text");
            }
            let title = bounded_text(title.stdout.trim(), super::MAX_CANDIDATE_TITLE_CHARS);
            let title = if title.trim().is_empty() {
                request.candidate.title.clone()
            } else {
                title
            };
            Ok::<_, anyhow::Error>((title, text, canonical_url, render_ms, extract_ms))
        }
        .await;
        let cleanup = owned.shutdown().await;
        let actual_engine = if self.mode.is_identity_bearing() {
            "magicutor_cdp".to_string()
        } else {
            owned
                .session()
                .active_engine_name()
                .unwrap_or_else(|| "bundled_chrome".to_string())
        };
        let (title, text, canonical_url, render_ms, extract_ms) = match (extraction, cleanup) {
            (Ok(extraction), Ok(())) => extraction,
            (outcome, cleanup) => {
                let message = match (outcome.as_ref().err(), cleanup.as_ref().err()) {
                    (Some(error), Some(cleanup)) => {
                        format!("{error}; browser cleanup also failed: {cleanup}")
                    },
                    (Some(error), None) => error.to_string(),
                    (None, Some(cleanup)) => format!("browser cleanup failed: {cleanup}"),
                    (None, None) => unreachable!(),
                };
                return Err(RetrievalTransportFailure {
                    message,
                    trace: RetrievalTransportTrace {
                        actual_transport: Some("browser".into()),
                        requested_browser_mode: Some(self.mode.label().into()),
                        resolved_browser_mode: Some(self.mode.label().into()),
                        browser_engine: Some(actual_engine),
                        browser_session_fingerprint: Some(session_fingerprint),
                        session_outcome: Some(if cleanup.is_ok() {
                            "closed".into()
                        } else {
                            "cleanup_failed".into()
                        }),
                        render_ms: None,
                        extract_ms: None,
                    },
                }
                .into());
            },
        };
        let privacy = if self.mode.is_identity_bearing() {
            ContentPrivacy::Private
        } else {
            ContentPrivacy::Public
        };
        let mut metadata = BTreeMap::new();
        metadata.insert(META_ACTUAL_TRANSPORT.into(), json!("browser"));
        metadata.insert(META_REQUESTED_BROWSER_MODE.into(), json!(self.mode.label()));
        metadata.insert(META_RESOLVED_BROWSER_MODE.into(), json!(self.mode.label()));
        metadata.insert(META_BROWSER_ENGINE.into(), json!(actual_engine));
        metadata.insert(
            META_BROWSER_SESSION_FINGERPRINT.into(),
            json!(session_fingerprint),
        );
        metadata.insert(META_SESSION_OUTCOME.into(), json!("closed"));
        metadata.insert(META_RENDER_MS.into(), json!(render_ms));
        metadata.insert(META_EXTRACT_MS.into(), json!(extract_ms));
        metadata.insert("rendered".into(), Value::Bool(true));

        Ok(ContentDocument {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: request.candidate.identity.clone(),
            title,
            text: text.clone(),
            canonical_url: Some(canonical_url.clone()),
            media_type: Some("text/plain; source=rendered-browser".into()),
            fetched_at_ms: Utc::now().timestamp_millis(),
            privacy,
            content_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            provenance: ContentProvenance {
                source_label: url::Url::parse(&canonical_url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .unwrap_or_else(|| "rendered web page".into()),
                source_url: Some(canonical_url),
                retrieved_by: self.descriptor.retrieval.action_id.clone(),
            },
            metadata,
        })
    }
}

#[async_trait]
impl super::ContentReader for BrowserContentReader {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn read(&self, request: &ReadRequest) -> Result<ContentDocument> {
        let target_url = request
            .candidate
            .canonical_url
            .as_deref()
            .ok_or_else(|| anyhow!("browser reader requires a canonical URL"))?;
        let preferred_engine = self.engine_attempts().first().cloned().flatten();
        let initial_url = if self.mode == RetrievalBrowserMode::PublicHeadlessRead {
            self.validated_public_url(target_url)
                .await
                .map_err(|error| {
                    browser_setup_failure(
                        error,
                        self.mode,
                        preferred_engine.clone(),
                        "target_validation_failed",
                    )
                })?
        } else {
            let parsed =
                url::Url::parse(target_url).context("parsing authenticated browser URL")?;
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.host_str().is_none()
            {
                bail!("authenticated browser URL must be an absolute credential-free HTTP URL");
            }
            parsed.to_string()
        };

        let attempts = self.engine_attempts();
        let mut prior_failures = Vec::new();
        for (index, engine) in attempts.iter().enumerate() {
            let fallback_from = index
                .checked_sub(1)
                .map(|prior| browser_engine_label(attempts[prior].as_deref()).to_string());
            match self
                .read_with_engine(
                    request,
                    target_url,
                    &initial_url,
                    engine.as_deref(),
                    fallback_from,
                )
                .await
            {
                Ok(mut document) => {
                    if index > 0 {
                        if let Some(from) = preferred_engine.as_deref() {
                            document
                                .metadata
                                .insert(META_BROWSER_ENGINE_FALLBACK_FROM.into(), json!(from));
                        }
                        document
                            .metadata
                            .insert(META_BROWSER_ENGINE_FALLBACK.into(), Value::Bool(true));
                        document.metadata.insert(
                            META_BROWSER_ENGINE_ATTEMPTS.into(),
                            json!(attempts[..=index]
                                .iter()
                                .map(|engine| browser_engine_label(engine.as_deref()))
                                .collect::<Vec<_>>()),
                        );
                    }
                    return Ok(document);
                },
                Err(error) if index + 1 < attempts.len() => {
                    let failed_engine = browser_engine_label(engine.as_deref());
                    let next_engine = browser_engine_label(attempts[index + 1].as_deref());
                    tracing::warn!(
                        failed_engine,
                        fallback_engine = next_engine,
                        error = %error,
                        "soft public-read browser engine failed; trying the next bounded fallback"
                    );
                    prior_failures.push(format!("{failed_engine}: {error}"));
                },
                Err(error) => {
                    if !prior_failures.is_empty() {
                        return Err(error.context(format!(
                            "public-read browser fallback chain exhausted after {}",
                            prior_failures.join(" | ")
                        )));
                    }
                    return Err(error);
                },
            }
        }
        unreachable!("browser engine attempt list is never empty")
    }
}

fn browser_engine_label(engine: Option<&str>) -> &str {
    engine.unwrap_or(BUNDLED_BROWSER_ENGINE_NAME)
}

fn browser_setup_failure(
    error: anyhow::Error,
    mode: RetrievalBrowserMode,
    browser_engine: Option<String>,
    outcome: &'static str,
) -> anyhow::Error {
    RetrievalTransportFailure {
        message: error.to_string(),
        trace: RetrievalTransportTrace {
            actual_transport: None,
            requested_browser_mode: Some(mode.label().into()),
            resolved_browser_mode: Some(mode.label().into()),
            browser_engine,
            browser_session_fingerprint: None,
            session_outcome: Some(outcome.into()),
            render_ms: None,
            extract_ms: None,
        },
    }
    .into()
}

async fn run_required(
    session: &AgentBrowserSession,
    args: &[&str],
    timeout_secs: u64,
) -> Result<crate::magician_v2::execution::primitive_dispatch::browser::AgentBrowserToolResult> {
    let result = session
        .run_command_with_options(args, timeout_secs, &[])
        .await?;
    if !result.success {
        bail!("deterministic browser command failed");
    }
    if result.stdout_truncated || result.stderr_truncated {
        bail!("deterministic browser command exceeded its capture limit");
    }
    Ok(result)
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn browser_reader_descriptor(
    adapter_id: &str,
    display_name: &str,
    action_id: &str,
    rung: RetrievalRung,
    authority: RetrievalAuthority,
    auth: AdapterAuth,
) -> ContentSourceDescriptor {
    ContentSourceDescriptor {
        adapter_id: adapter_id.to_string(),
        display_name: display_name.to_string(),
        class: ContentSourceClass::WebPage,
        capabilities: ContentSourceCapabilities {
            discovery: false,
            full_content: true,
            cursor: false,
            conditional_fetch: false,
            execution: AdapterExecution::LocalProcess,
            auth,
            sends_user_intent: false,
            metered: false,
        },
        retrieval: RetrievalActionMetadata::reader(
            action_id,
            rung,
            authority,
            vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
        ),
    }
}

#[derive(Debug, Clone)]
pub struct BrowserHandoffAdapter {
    descriptor: ContentSourceDescriptor,
    kind: RetrievalHandoffKind,
    mode: RetrievalBrowserMode,
    authority: RetrievalAuthority,
    requires_approval: bool,
    ttl_secs: u64,
    authority_state: Arc<RetrievalRuntimeState>,
}

impl BrowserHandoffAdapter {
    pub fn descriptor_ref(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    pub fn public_discovery(ttl_secs: u64, authority_state: Arc<RetrievalRuntimeState>) -> Self {
        Self::discovery(
            "browser-headless-discovery-handoff",
            "browser.headless.discover_handoff",
            RetrievalRung::PublicBrowserHandoff,
            RetrievalHandoffKind::PublicHeadlessNavigation,
            RetrievalBrowserMode::PublicHeadlessInteract,
            RetrievalAuthority::PublicBrowserInteract,
            false,
            ttl_secs,
            authority_state,
        )
    }

    pub fn owner_assisted_read(ttl_secs: u64, authority_state: Arc<RetrievalRuntimeState>) -> Self {
        Self::reader(
            "browser-headed-read-handoff",
            "browser.headed.read_handoff",
            RetrievalRung::OwnerAssisted,
            RetrievalHandoffKind::OwnerAssistedHeaded,
            RetrievalBrowserMode::PublicHeadedInteract,
            RetrievalAuthority::PublicBrowserInteract,
            false,
            ttl_secs,
            authority_state,
        )
    }

    pub fn authenticated_interaction(
        ttl_secs: u64,
        authority_state: Arc<RetrievalRuntimeState>,
    ) -> Self {
        Self::reader(
            "browser-cdp-interaction-handoff",
            "browser.cdp.interact_handoff",
            RetrievalRung::InteractionHandoff,
            RetrievalHandoffKind::AuthenticatedInteraction,
            RetrievalBrowserMode::AuthenticatedCdpInteract,
            RetrievalAuthority::AuthenticatedInteract,
            true,
            ttl_secs,
            authority_state,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn discovery(
        adapter_id: &str,
        action_id: &str,
        rung: RetrievalRung,
        kind: RetrievalHandoffKind,
        mode: RetrievalBrowserMode,
        authority: RetrievalAuthority,
        requires_approval: bool,
        ttl_secs: u64,
        authority_state: Arc<RetrievalRuntimeState>,
    ) -> Self {
        Self {
            descriptor: ContentSourceDescriptor {
                adapter_id: adapter_id.into(),
                display_name: "Browser navigation handoff".into(),
                class: ContentSourceClass::WebSearch,
                capabilities: ContentSourceCapabilities {
                    discovery: true,
                    full_content: false,
                    cursor: false,
                    conditional_fetch: false,
                    execution: AdapterExecution::LocalProcess,
                    auth: if requires_approval {
                        AdapterAuth::Required
                    } else {
                        AdapterAuth::None
                    },
                    sends_user_intent: false,
                    metered: false,
                },
                retrieval: RetrievalActionMetadata::discovery(action_id, rung, authority, false),
            },
            kind,
            mode,
            authority,
            requires_approval,
            ttl_secs,
            authority_state,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn reader(
        adapter_id: &str,
        action_id: &str,
        rung: RetrievalRung,
        kind: RetrievalHandoffKind,
        mode: RetrievalBrowserMode,
        authority: RetrievalAuthority,
        requires_approval: bool,
        ttl_secs: u64,
        authority_state: Arc<RetrievalRuntimeState>,
    ) -> Self {
        let mut retrieval = RetrievalActionMetadata::reader(
            action_id,
            rung,
            authority,
            vec![RetrievalOutputKind::Handoff],
        );
        retrieval.parallel_safe = false;
        Self {
            descriptor: ContentSourceDescriptor {
                adapter_id: adapter_id.into(),
                display_name: "Browser interaction handoff".into(),
                class: ContentSourceClass::WebPage,
                capabilities: ContentSourceCapabilities {
                    discovery: false,
                    full_content: true,
                    cursor: false,
                    conditional_fetch: false,
                    execution: AdapterExecution::LocalProcess,
                    auth: if requires_approval {
                        AdapterAuth::Required
                    } else {
                        AdapterAuth::None
                    },
                    sends_user_intent: false,
                    metered: false,
                },
                retrieval,
            },
            kind,
            mode,
            authority,
            requires_approval,
            ttl_secs,
            authority_state,
        }
    }

    fn handoff(
        &self,
        principal: &str,
        workspace: &str,
        target_url: Option<String>,
        query: Option<String>,
        authority_grant_id: Option<&str>,
    ) -> Result<RetrievalHandoff> {
        let id = format!("rh_{}", uuid::Uuid::new_v4().simple());
        let transport = self.mode.connection_mode_label();
        let handoff = RetrievalHandoff {
            browser_session_id: format!("retrieval-{transport}-{id}"),
            id,
            kind: self.kind,
            requested_mode: self.mode.connection_mode_label().to_string(),
            action_id: self.descriptor.retrieval.action_id.clone(),
            required_authority: self.authority,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            target_url,
            query: query.map(|query| bounded_text(&query, 4_096)),
            requires_approval: self.requires_approval && authority_grant_id.is_none(),
            expires_at_ms: Utc::now().timestamp_millis().saturating_add(
                i64::try_from(self.ttl_secs)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1_000),
            ),
        };
        self.authority_state
            .register_handoff_session(&handoff, authority_grant_id)?;
        Ok(handoff)
    }
}

#[async_trait]
impl DiscoveryAdapter for BrowserHandoffAdapter {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryPage> {
        let handoff = self.handoff(
            &request.principal,
            &request.workspace,
            None,
            request.query.clone(),
            None,
        )?;
        Err(RetrievalHandoffRequired { handoff }.into())
    }
}

#[async_trait]
impl super::ContentReader for BrowserHandoffAdapter {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn read(&self, request: &ReadRequest) -> Result<ContentDocument> {
        let handoff = self.handoff(
            &request.principal,
            &request.workspace,
            request.candidate.canonical_url.clone(),
            None,
            request.authority_grant_id.as_deref(),
        )?;
        Err(RetrievalHandoffRequired { handoff }.into())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;

    #[test]
    fn only_lightpanda_is_serialized_at_the_retrieval_engine_boundary() {
        assert!(retrieval_engine_is_serial(LIGHTPANDA_BROWSER_ENGINE_NAME));
        assert!(!retrieval_engine_is_serial("bundled_chrome"));
        assert!(!retrieval_engine_is_serial("cloak-browser"));
    }

    #[tokio::test]
    async fn lightpanda_retrieval_gate_allows_one_session_at_a_time() {
        let first = lightpanda_retrieval_gate()
            .acquire_owned()
            .await
            .expect("first Lightpanda session");
        assert!(lightpanda_retrieval_gate().try_acquire_owned().is_err());
        drop(first);
        assert!(lightpanda_retrieval_gate().try_acquire_owned().is_ok());
    }

    #[test]
    fn setup_failure_retains_sanitized_transport_stage_without_claiming_a_launch() {
        let error = browser_setup_failure(
            anyhow!("sensitive local setup detail"),
            RetrievalBrowserMode::PublicHeadlessRead,
            Some("configured-engine".into()),
            "engine_resolution_failed",
        );
        let failure = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<RetrievalTransportFailure>())
            .expect("typed transport failure");

        assert_eq!(failure.trace.actual_transport, None);
        assert_eq!(
            failure.trace.requested_browser_mode.as_deref(),
            Some("public_headless_read")
        );
        assert_eq!(
            failure.trace.browser_engine.as_deref(),
            Some("configured-engine")
        );
        assert_eq!(
            failure.trace.session_outcome.as_deref(),
            Some("engine_resolution_failed")
        );
    }
    use crate::magician_v2::content_sources::{
        ContentCandidate, ContentInvocationSource, FreshnessPolicy, ReadDepth, RemoteDataPolicy,
        SourceIdentity,
    };

    fn fake_cli(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let cli = temp.path().join("agent-browser");
        let log = temp.path().join("calls.log");
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$*" in
  *"get title"*) printf '%s\n' 'Rendered Fixture' ;;
  *"get url"*) printf '%s\n' 'https://example.test/rendered' ;;
  *"get text body"*) printf '%s\n' 'This rendered fixture has enough deterministic body text to satisfy a full-content retrieval quality gate without any model or interactive browser action.' ;;
esac
exit 0
"#,
            log.display()
        );
        fs::write(&cli, script).unwrap();
        let mut permissions = fs::metadata(&cli).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&cli, permissions).unwrap();
        (cli, log)
    }

    fn fake_cli_with_commands(temp: &tempfile::TempDir, commands: &str) -> (PathBuf, PathBuf) {
        let cli = temp.path().join("agent-browser");
        let log = temp.path().join("calls.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{}\n",
            log.display(),
            commands,
        );
        fs::write(&cli, script).unwrap();
        let mut permissions = fs::metadata(&cli).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&cli, permissions).unwrap();
        (cli, log)
    }

    fn read_request() -> ReadRequest {
        ReadRequest {
            principal: "owner".into(),
            workspace: "default".into(),
            candidate: ContentCandidate {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: SourceIdentity::new("fixture", "rendered").unwrap(),
                title: "Fixture".into(),
                cheap_text: "A JavaScript rendered fixture".into(),
                canonical_url: Some("https://example.test/rendered".into()),
                published_at_ms: None,
                observed_at_ms: 1,
                privacy: ContentPrivacy::Public,
                content_hash: None,
                provenance: ContentProvenance {
                    source_label: "Fixture".into(),
                    source_url: Some("https://example.test/rendered".into()),
                    retrieved_by: "fixture".into(),
                },
                metadata: BTreeMap::new(),
            },
            depth: ReadDepth::FullText,
            freshness: FreshnessPolicy::Fresh,
            remote_content_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::InteractiveRead,
            selection: None,
            authority_grant_id: None,
        }
    }

    #[tokio::test]
    async fn deterministic_reader_uses_fixed_headless_sequence_and_closes() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, log) = fake_cli(&temp);
        let reader = BrowserContentReader::public_headless(
            BrowserRetrievalSettings {
                render_wait_ms: 1,
                engine: None,
                ..BrowserRetrievalSettings::default()
            },
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        )
        .without_dns_resolution_for_test();

        let request = read_request();
        let document = super::super::ContentReader::read(&reader, &request)
            .await
            .unwrap();
        assert_eq!(document.identity, request.candidate.identity);
        assert_eq!(document.title, "Rendered Fixture");
        assert_eq!(document.metadata[META_ACTUAL_TRANSPORT], json!("browser"));
        assert_eq!(
            document.metadata[META_REQUESTED_BROWSER_MODE],
            json!("public_headless_read")
        );
        let calls = fs::read_to_string(log).unwrap();
        assert!(calls.contains("open https://example.test/rendered"));
        assert!(calls.contains("wait --load networkidle"));
        assert!(calls.contains("get text body"));
        assert!(calls.lines().any(|line| line.ends_with(" close")));
        assert!(!calls.contains(" connect "));
    }

    #[tokio::test]
    async fn deterministic_reader_reports_the_configured_resolved_engine() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, _) = fake_cli(&temp);
        let resolver_dir = temp
            .path()
            .join("scopes/owner/default/skills/custom-browser/scripts");
        fs::create_dir_all(&resolver_dir).unwrap();
        fs::write(
            resolver_dir.join("resolve.py"),
            "import json\nprint(json.dumps({'args': ['--fixture-engine']}))\n",
        )
        .unwrap();
        let reader = BrowserContentReader::public_headless(
            BrowserRetrievalSettings {
                render_wait_ms: 1,
                engine: Some("custom-browser".into()),
                ..BrowserRetrievalSettings::default()
            },
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        )
        .without_dns_resolution_for_test();

        let document = super::super::ContentReader::read(&reader, &read_request())
            .await
            .unwrap();
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE],
            json!("custom-browser")
        );
    }

    #[tokio::test]
    async fn lightpanda_public_reader_soft_preference_retries_once_with_full_fidelity_engine() {
        let temp = tempfile::tempdir().unwrap();
        let commands = r#"case "$*" in
  *"get title"*) printf '%s\n' 'Fallback Fixture' ;;
  *"get url"*) printf '%s\n' 'https://example.test/rendered' ;;
  *"get text body"*)
    if [ "${MAGICIAN_TEST_BROWSER_ENGINE:-}" = "lightpanda" ]; then
      echo 'unsupported Web API' >&2
      exit 9
    fi
    printf '%s\n' 'The full fidelity fallback returned enough deterministic body text to prove the bounded public read retry completed successfully.'
    ;;
esac
exit 0"#;
        let (cli, log) = fake_cli_with_commands(&temp, commands);
        for (engine, value) in [("lightpanda", "lightpanda"), ("cloak-browser", "cloak")] {
            let resolver = temp.path().join(format!(
                "scopes/owner/default/skills/{engine}/scripts/resolve.py"
            ));
            fs::create_dir_all(resolver.parent().unwrap()).unwrap();
            fs::write(
                resolver,
                format!(
                    "import json\nprint(json.dumps({{'env': {{'MAGICIAN_TEST_BROWSER_ENGINE': \
                     '{value}'}}}}))\n"
                ),
            )
            .unwrap();
        }
        let reader = BrowserContentReader::public_headless(
            BrowserRetrievalSettings {
                render_wait_ms: 1,
                public_read_engine: Some("lightpanda".into()),
                engine: Some("cloak-browser".into()),
                ..BrowserRetrievalSettings::default()
            },
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        )
        .without_dns_resolution_for_test();

        let document = super::super::ContentReader::read(&reader, &read_request())
            .await
            .unwrap();
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE],
            json!("cloak-browser")
        );
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE_FALLBACK_FROM],
            json!("lightpanda")
        );
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE_FALLBACK],
            Value::Bool(true)
        );
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE_ATTEMPTS],
            json!(["lightpanda", "cloak-browser"])
        );
        let calls = fs::read_to_string(log).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("get text body"))
                .count(),
            2
        );
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.ends_with(" close"))
                .count(),
            2
        );
        let analytics = crate::magician_v2::browser_engine_analytics::list_browser_engine_usage(
            temp.path(),
            "owner",
            "default",
            crate::magician_v2::browser_engine_analytics::BrowserEngineUsageFilter {
                engine: Some("cloak-browser".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(!analytics.items.is_empty());
        assert!(analytics
            .items
            .iter()
            .all(|row| row.fallback_from.as_deref() == Some("lightpanda")));
    }

    #[tokio::test]
    async fn lightpanda_public_reader_reaches_bundled_chrome_when_full_engine_is_absent() {
        let temp = tempfile::tempdir().unwrap();
        let commands = r#"case "$*" in
  *"get title"*) printf '%s\n' 'Fallback Fixture' ;;
  *"get url"*) printf '%s\n' 'https://example.test/rendered' ;;
  *"get text body"*)
    if [ "${MAGICIAN_TEST_BROWSER_ENGINE:-}" = "lightpanda" ]; then
      echo 'engine could not complete public read' >&2
      exit 9
    fi
    printf '%s\n' 'Bundled Chrome for Testing completed the final bounded public read fallback successfully.'
    ;;
esac
exit 0"#;
        let (cli, log) = fake_cli_with_commands(&temp, commands);
        let resolver = temp
            .path()
            .join("scopes/owner/default/skills/lightpanda/scripts/resolve.py");
        fs::create_dir_all(resolver.parent().unwrap()).unwrap();
        fs::write(
            resolver,
            "import json\nprint(json.dumps({'env': {'MAGICIAN_TEST_BROWSER_ENGINE': \
             'lightpanda'}}))\n",
        )
        .unwrap();
        let reader = BrowserContentReader::public_headless(
            BrowserRetrievalSettings {
                render_wait_ms: 1,
                public_read_engine: Some("lightpanda".into()),
                engine: Some("cloak-browser".into()),
                ..BrowserRetrievalSettings::default()
            },
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        )
        .without_dns_resolution_for_test();

        let document = super::super::ContentReader::read(&reader, &read_request())
            .await
            .unwrap();
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE],
            json!(BUNDLED_BROWSER_ENGINE_NAME)
        );
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE_ATTEMPTS],
            json!(["lightpanda", "cloak-browser", "bundled_chrome"])
        );
        let calls = fs::read_to_string(log).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.contains("get text body"))
                .count(),
            2
        );
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.ends_with(" close"))
                .count(),
            2
        );
    }

    #[test]
    fn lightpanda_public_read_engine_preference_cannot_change_authenticated_routing() {
        let temp = tempfile::tempdir().unwrap();
        let settings = BrowserRetrievalSettings {
            public_read_engine: Some("lightpanda".into()),
            engine: Some("cloak-browser".into()),
            ..BrowserRetrievalSettings::default()
        };
        let public_reader = BrowserContentReader::public_headless(
            settings.clone(),
            temp.path().join("agent-browser"),
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        );
        assert_eq!(
            public_reader.engine_attempts(),
            vec![
                Some("lightpanda".into()),
                Some("cloak-browser".into()),
                None,
            ]
        );

        let bundled_fallback_reader = BrowserContentReader::public_headless(
            BrowserRetrievalSettings {
                public_read_engine: Some("lightpanda".into()),
                engine: None,
                ..BrowserRetrievalSettings::default()
            },
            temp.path().join("agent-browser"),
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        );
        assert_eq!(
            bundled_fallback_reader.engine_attempts(),
            vec![Some("lightpanda".into()), None]
        );

        let authenticated_reader = BrowserContentReader::authenticated_cdp(
            settings,
            temp.path().join("agent-browser"),
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        );
        assert_eq!(
            authenticated_reader.engine_attempts(),
            vec![Some("cloak-browser".into())]
        );
    }

    #[tokio::test]
    async fn extraction_failure_and_capture_overflow_both_close_the_session() {
        let failure = "case \"$*\" in\n  *\"get title\"*) printf '%s\\n' 'Title' ;;\n  *\"get url\"*) printf '%s\\n' 'https://example.test/rendered' ;;\n  *\"get text body\"*) exit 9 ;;\nesac\nexit 0";
        let overflow = "case \"$*\" in\n  *\"get title\"*) printf '%s\\n' 'Title' ;;\n  *\"get url\"*) printf '%s\\n' 'https://example.test/rendered' ;;\n  *\"get text body\"*) yes x | head -c 4096 ;;\nesac\nexit 0";
        for (name, commands) in [("failure", failure), ("overflow", overflow)] {
            let temp = tempfile::tempdir().unwrap();
            let (cli, log) = fake_cli_with_commands(&temp, commands);
            let reader = BrowserContentReader::public_headless(
                BrowserRetrievalSettings {
                    render_wait_ms: 1,
                    max_capture_bytes: 128,
                    engine: None,
                    ..BrowserRetrievalSettings::default()
                },
                cli,
                temp.path().to_path_buf(),
                "owner",
                "default",
                Arc::new(RetrievalRuntimeState::default()),
            )
            .without_dns_resolution_for_test();
            let error = super::super::ContentReader::read(&reader, &read_request())
                .await
                .unwrap_err();
            assert!(
                error
                    .chain()
                    .any(|cause| cause.downcast_ref::<RetrievalTransportFailure>().is_some()),
                "{name}: {error}"
            );
            let calls = fs::read_to_string(log).unwrap();
            assert!(calls.lines().any(|line| line.ends_with(" close")), "{name}");
        }
    }

    #[tokio::test]
    async fn cancelled_reader_schedules_controller_owned_session_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let commands = "case \"$*\" in\n  *\"wait --load networkidle\"*) sleep 2 ;;\nesac\nexit 0";
        let (cli, log) = fake_cli_with_commands(&temp, commands);
        let reader = BrowserContentReader::public_headless(
            BrowserRetrievalSettings {
                render_wait_ms: 1,
                engine: None,
                ..BrowserRetrievalSettings::default()
            },
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        )
        .without_dns_resolution_for_test();
        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            super::super::ContentReader::read(&reader, &read_request()),
        )
        .await;
        assert!(timed_out.is_err());
        // The full content-source test filter runs many subprocess-heavy tests in
        // parallel. Give the spawned RAII cleanup enough scheduling headroom while
        // still bounding a genuine lifecycle leak.
        for _ in 0..200 {
            let calls = fs::read_to_string(&log).unwrap_or_default();
            if calls.lines().any(|line| line.ends_with(" close")) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("controller-owned browser cleanup did not run after cancellation");
    }

    #[tokio::test]
    async fn handoff_adapter_returns_typed_nonexecuting_transfer() {
        let state = Arc::new(RetrievalRuntimeState::default());
        let adapter = BrowserHandoffAdapter::public_discovery(60, Arc::clone(&state));
        let request = DiscoveryRequest {
            principal: "owner".into(),
            workspace: "default".into(),
            intent: None,
            query: Some("find a rendered source".into()),
            targets: Vec::new(),
            cursor: None,
            validators: BTreeMap::new(),
            limit: 5,
            freshness: FreshnessPolicy::Fresh,
            remote_query_policy: RemoteDataPolicy::Allow,
            invocation_source: ContentInvocationSource::InteractiveRead,
            options: BTreeMap::new(),
        };
        let error = DiscoveryAdapter::discover(&adapter, &request)
            .await
            .unwrap_err();
        let handoff = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<RetrievalHandoffRequired>())
            .unwrap();
        assert_eq!(
            handoff.handoff.kind,
            RetrievalHandoffKind::PublicHeadlessNavigation
        );
        assert_eq!(
            handoff.handoff.required_authority,
            RetrievalAuthority::PublicBrowserInteract
        );
        assert_eq!(handoff.handoff.requested_mode, "headless");
        assert!(!handoff.handoff.requires_approval);
        assert!(state.handoff_session_allows(
            &handoff.handoff.browser_session_id,
            "owner",
            "default",
            "headless",
            Some("browser.headless.discover_handoff"),
            Some("https://example.test/result"),
        ));
        assert!(!state.handoff_session_allows(
            &handoff.handoff.browser_session_id,
            "owner",
            "default",
            "headless",
            Some("browser.cdp.interact_handoff"),
            None,
        ));
    }

    #[tokio::test]
    async fn authenticated_interaction_handoff_requires_matching_grant_and_registers_session() {
        let state = Arc::new(RetrievalRuntimeState::default());
        let adapter = BrowserHandoffAdapter::authenticated_interaction(60, Arc::clone(&state));
        let request = read_request();
        let missing = super::super::ContentReader::read(&adapter, &request)
            .await
            .unwrap_err();
        assert!(missing.to_string().contains("approval is absent"));

        let grant = state
            .issue_authority_grant(
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedInteract,
                "example.test",
                "browser.cdp.interact_handoff",
                true,
                std::time::Duration::from_secs(60),
            )
            .unwrap();
        let mut approved = read_request();
        approved.authority_grant_id = Some(grant.id);
        let error = super::super::ContentReader::read(&adapter, &approved)
            .await
            .unwrap_err();
        let handoff = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<RetrievalHandoffRequired>())
            .unwrap();
        assert!(!handoff.handoff.requires_approval);
        assert!(state.handoff_session_allows(
            &handoff.handoff.browser_session_id,
            "owner",
            "default",
            "cdp",
            Some("browser.cdp.interact_handoff"),
            Some("https://example.test/rendered"),
        ));
    }

    #[test]
    fn authenticated_reader_rejects_missing_or_wrong_grant_before_launch() {
        let temp = tempfile::tempdir().unwrap();
        let (cli, _) = fake_cli(&temp);
        let state = Arc::new(RetrievalRuntimeState::default());
        let reader = BrowserContentReader::authenticated_cdp(
            BrowserRetrievalSettings::default(),
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::clone(&state),
        );
        let error = reader
            .claim_request_authority(
                &read_request(),
                "https://example.test/rendered",
                "test-operation",
            )
            .unwrap_err();
        assert!(error.to_string().contains("authentication required"));

        let grant = state
            .issue_authority_grant(
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedRead,
                "example.test",
                CDP_READ_ACTION,
                false,
                std::time::Duration::from_secs(60),
            )
            .unwrap();
        let mut unshareable = read_request();
        unshareable.authority_grant_id = Some(grant.id);
        let error = reader
            .claim_request_authority(
                &unshareable,
                "https://example.test/rendered",
                "test-unshareable-operation",
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("does not permit returning private"));
    }

    #[tokio::test]
    async fn authenticated_reader_is_private_domain_bound_and_disconnects_without_closing_browser()
    {
        let temp = tempfile::tempdir().unwrap();
        let (cli, log) = fake_cli(&temp);
        let state = Arc::new(RetrievalRuntimeState::default());
        let grant = state
            .issue_authority_grant(
                "owner",
                "default",
                RetrievalAuthority::AuthenticatedRead,
                "example.test",
                CDP_READ_ACTION,
                true,
                std::time::Duration::from_secs(60),
            )
            .unwrap();
        let reader = BrowserContentReader::authenticated_cdp(
            BrowserRetrievalSettings {
                render_wait_ms: 1,
                ..BrowserRetrievalSettings::default()
            },
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            state,
        );
        let mut request = read_request();
        request.authority_grant_id = Some(grant.id);
        let document = super::super::ContentReader::read(&reader, &request)
            .await
            .unwrap();
        assert_eq!(document.identity, request.candidate.identity);
        assert_eq!(document.privacy, ContentPrivacy::Private);
        assert_eq!(
            document.metadata[META_BROWSER_ENGINE],
            json!("magicutor_cdp")
        );
        let calls = fs::read_to_string(log).unwrap();
        assert!(calls.contains(" connect ws://127.0.0.1:3003/devtools/browser/retrieval-"));
        assert!(calls
            .lines()
            .any(|line| line.ends_with(" close --keep-browser")));
    }
}
