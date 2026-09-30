use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use anyhow::{Context, Result};
use serde::Serialize;

use super::{
    api_replay_reader::VerifiedApiReplayReader,
    browser::{BrowserContentReader, BrowserHandoffAdapter},
    cache::ScopedContentCache,
    capability_adapter::{
        optional_validated_capability_discovery_manifest, CapabilityDiscoveryAdapter,
    },
    capability_reader::{
        optional_validated_capability_reader_manifest, CapabilityStaticContentReader,
    },
    retrieval::{global_retrieval_runtime_state, RetrievalRuntimeState},
    shipped_content_source_registry,
    types::validate_content_scope_component,
    BrowserRetrievalSettings, ContentAcquisitionService, ContentAcquisitionSettings,
    ContentAdapterKind, ContentRegistrationErrorClass, ContentRegistrationIssue,
    ContentSourceRegistry,
};
use crate::{
    config::ApiMiningConfig,
    magician_v2::{
        artifact_v2::CapabilityWorkspaceManager,
        execution::{
            capability::ImplementationType,
            compiled_dispatch::CompiledDispatchAuthority,
            primitive_dispatch::{
                DeterministicCapabilityInvoker, PrimitiveExecCtx,
                ScopedDeterministicCapabilityInvoker,
            },
            ScopedCapabilityResolver,
        },
        skills::embedded_extensions::{
            discover_skill_markdown_paths_isolated, SkillDiscoveryIssueClass,
        },
    },
};

const DEFAULT_CONTENT_SERVICE_CACHE_LIMIT: usize = 64;
const MAX_COHERENT_BUILD_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ContentScopeKey {
    principal: String,
    workspace: String,
}

impl ContentScopeKey {
    fn new(principal: &str, workspace: &str) -> Self {
        Self {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ContentServiceCacheKey {
    scope: ContentScopeKey,
    capability_revision: String,
}

#[derive(Debug, Default)]
struct ContentServiceCacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    builds: AtomicU64,
    invalidations: AtomicU64,
    coalesced_waiters: AtomicU64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContentServiceCacheStatus {
    pub entry_count: usize,
    pub build_lock_count: usize,
    pub hits: u64,
    pub misses: u64,
    pub builds: u64,
    pub invalidations: u64,
    pub coalesced_waiters: u64,
}

/// Process-owned resolver for exact-scope acquisition services. Capability
/// revisions come from `ScopedCapabilityResolver`, so skill edits invalidate
/// executable tools and content adapters as one coherent catalog.
pub struct ContentAcquisitionResolver {
    capability_resolver: Arc<ScopedCapabilityResolver>,
    workspace_manager: Arc<CapabilityWorkspaceManager>,
    compiled_dispatch_authority: CompiledDispatchAuthority,
    secret_store_resolver: Option<Arc<crate::magician_v2::secrets::SecretStoreResolver>>,
    cache_root: PathBuf,
    content_cache: ScopedContentCache,
    services: RwLock<HashMap<ContentServiceCacheKey, Arc<ContentAcquisitionService>>>,
    build_locks: Mutex<HashMap<ContentScopeKey, Arc<Mutex<()>>>>,
    cache_generation: AtomicU64,
    cache_metrics: ContentServiceCacheMetrics,
    cache_limit: usize,
    content_settings: ContentAcquisitionSettings,
    api_mining_config: ApiMiningConfig,
    #[cfg(any(test, feature = "test-fixtures"))]
    build_observer: Mutex<Option<Arc<dyn Fn(usize) + Send + Sync>>>,
    #[cfg(any(test, feature = "test-fixtures"))]
    reader_fetcher: Mutex<Option<super::public_http::PublicHttpFetcher>>,
}

impl std::fmt::Debug for ContentAcquisitionResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ContentAcquisitionResolver")
            .field("cache_root", &self.cache_root)
            .field("cache_status", &self.cache_status())
            .finish_non_exhaustive()
    }
}

impl ContentAcquisitionResolver {
    pub fn configured_browser_engine(&self) -> Option<String> {
        self.content_settings.browser.engine.clone()
    }

    pub fn configured_browser_capture_limit_bytes(&self) -> usize {
        self.content_settings.browser.max_capture_bytes
    }

    pub fn configured_browser_cdp_url(&self) -> String {
        if let Some(url) = crate::magician_v2::runtime::device_transport::current()
            .and_then(|transport| transport.browser_cdp_url())
        {
            return url;
        }
        self.content_settings.browser.cdp_url.clone()
    }

    pub fn new(
        capability_resolver: Arc<ScopedCapabilityResolver>,
        workspace_manager: Arc<CapabilityWorkspaceManager>,
        compiled_dispatch_authority: CompiledDispatchAuthority,
        secret_store_resolver: Arc<crate::magician_v2::secrets::SecretStoreResolver>,
        content_settings: ContentAcquisitionSettings,
        api_mining_config: ApiMiningConfig,
    ) -> Self {
        let cache_root = workspace_manager
            .workspace_layout()
            .base_root()
            .join("content_sources")
            .join("cache");
        let mut resolver = Self::from_parts(
            capability_resolver,
            workspace_manager,
            compiled_dispatch_authority,
            cache_root,
            content_settings,
            api_mining_config,
        );
        resolver.secret_store_resolver = Some(secret_store_resolver);
        resolver
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn with_cache_root(
        capability_resolver: Arc<ScopedCapabilityResolver>,
        workspace_manager: Arc<CapabilityWorkspaceManager>,
        compiled_dispatch_authority: CompiledDispatchAuthority,
        cache_root: impl Into<PathBuf>,
    ) -> Self {
        let mut content_settings = ContentAcquisitionSettings::default();
        content_settings.browser.enabled = false;
        Self::from_parts(
            capability_resolver,
            workspace_manager,
            compiled_dispatch_authority,
            cache_root,
            content_settings,
            ApiMiningConfig::default(),
        )
    }

    fn from_parts(
        capability_resolver: Arc<ScopedCapabilityResolver>,
        workspace_manager: Arc<CapabilityWorkspaceManager>,
        compiled_dispatch_authority: CompiledDispatchAuthority,
        cache_root: impl Into<PathBuf>,
        content_settings: ContentAcquisitionSettings,
        api_mining_config: ApiMiningConfig,
    ) -> Self {
        let cache_root = cache_root.into();
        Self {
            capability_resolver,
            workspace_manager,
            compiled_dispatch_authority,
            secret_store_resolver: None,
            content_cache: ScopedContentCache::new(cache_root.clone()),
            cache_root,
            services: RwLock::new(HashMap::new()),
            build_locks: Mutex::new(HashMap::new()),
            cache_generation: AtomicU64::new(0),
            cache_metrics: ContentServiceCacheMetrics::default(),
            cache_limit: DEFAULT_CONTENT_SERVICE_CACHE_LIMIT,
            content_settings,
            api_mining_config,
            #[cfg(any(test, feature = "test-fixtures"))]
            build_observer: Mutex::new(None),
            #[cfg(any(test, feature = "test-fixtures"))]
            reader_fetcher: Mutex::new(None),
        }
    }

    /// Async-safe resolution for API and background consumers. Blocking skill
    /// catalog reads and registry construction stay off the async worker.
    pub async fn resolve(
        self: &Arc<Self>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Result<Arc<ContentAcquisitionService>> {
        let principal = principal.into();
        let workspace = workspace.into();
        let resolver = Arc::clone(self);
        tokio::task::spawn_blocking(move || resolver.resolve_blocking(&principal, &workspace))
            .await
            .context("joining content acquisition resolver task")?
    }

    pub fn resolve_blocking(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Arc<ContentAcquisitionService>> {
        validate_content_scope_component(principal, "content acquisition principal")?;
        validate_content_scope_component(workspace, "content acquisition workspace")?;
        let scope = ContentScopeKey::new(principal, workspace);
        let snapshot = self
            .capability_resolver
            .capability_snapshot_for_scope(principal, workspace)?;
        let mut cache_key = ContentServiceCacheKey {
            scope: scope.clone(),
            capability_revision: snapshot.revision.clone(),
        };
        if let Some(service) = self.cached(&cache_key) {
            self.cache_metrics.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(service);
        }
        self.cache_metrics.misses.fetch_add(1, Ordering::Relaxed);

        let build_lock = self.build_lock_for(&scope);
        let _guard = match build_lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                self.cache_metrics
                    .coalesced_waiters
                    .fetch_add(1, Ordering::Relaxed);
                build_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            },
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        };

        for attempt in 1..=MAX_COHERENT_BUILD_ATTEMPTS {
            // A waiter may have built the service, or a skill refresh may have
            // changed the revision while this caller waited.
            let snapshot = self
                .capability_resolver
                .capability_snapshot_for_scope(principal, workspace)?;
            cache_key.capability_revision = snapshot.revision.clone();
            if let Some(service) = self.cached(&cache_key) {
                self.cache_metrics.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(service);
            }
            let cache_generation = self.cache_generation.load(Ordering::Acquire);

            let scope_paths = self.workspace_manager.scope_paths(principal, workspace);
            let mut base_exec_ctx = PrimitiveExecCtx::default_for_runtime()
                .with_scope(
                    Some(principal.to_string()),
                    Some(workspace.to_string()),
                    None,
                )
                .with_artifact_workspace(Some(self.workspace_manager.workspace_layout().clone()))
                .with_compiled_dispatch_authority(Some(self.compiled_dispatch_authority.clone()));
            if let Some(resolver) = self.secret_store_resolver.as_ref() {
                let store = resolver
                    .resolve_for_scope(principal, workspace)
                    .context("resolving governed content-source audit authority")?;
                base_exec_ctx = base_exec_ctx.with_secret_context(Some(store), None);
                base_exec_ctx =
                    base_exec_ctx.with_secret_store_resolver(Some(Arc::clone(resolver)));
            }
            base_exec_ctx.storage_base_path = self
                .workspace_manager
                .workspace_layout()
                .base_root()
                .to_path_buf();
            let invoker: Arc<dyn DeterministicCapabilityInvoker> = Arc::new(
                ScopedDeterministicCapabilityInvoker::new(snapshot.registry.clone(), base_exec_ctx)
                    .with_scope_paths(scope_paths),
            );
            let skill_roots = self.skill_roots(principal, workspace);
            let retrieval_state = global_retrieval_runtime_state();
            let storage_root = self.workspace_manager.workspace_layout().base_root();
            let api_mining_base_path =
                crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(storage_root)
                    .api_mining_root(principal, workspace);
            let (registry, issues) = build_runtime_registry(
                invoker,
                &skill_roots,
                self.content_cache.clone(),
                &snapshot.registry,
                &snapshot.revision,
                storage_root,
                principal,
                workspace,
                &self.content_settings.browser,
                &self.api_mining_config,
                api_mining_base_path,
                Arc::clone(&retrieval_state),
                #[cfg(any(test, feature = "test-fixtures"))]
                self.reader_fetcher
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
            )?;
            #[cfg(any(test, feature = "test-fixtures"))]
            if let Some(observer) = self
                .build_observer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
            {
                observer(attempt);
            }
            let confirmed = self
                .capability_resolver
                .capability_snapshot_for_scope(principal, workspace)?;
            if confirmed.revision != snapshot.revision {
                tracing::info!(
                    principal,
                    workspace,
                    attempt,
                    "content adapter catalog changed during construction; rebuilding"
                );
                continue;
            }
            let service = Arc::new(ContentAcquisitionService::new_with_retrieval_state(
                principal,
                workspace,
                &snapshot.revision,
                registry,
                issues,
                retrieval_state,
            )?);
            if !self.publish(
                scope.clone(),
                cache_key.clone(),
                Arc::clone(&service),
                cache_generation,
            ) {
                tracing::info!(
                    principal,
                    workspace,
                    attempt,
                    "content service cache cleared during construction; rebuilding"
                );
                continue;
            }
            self.cache_metrics.builds.fetch_add(1, Ordering::Relaxed);
            return Ok(service);
        }
        anyhow::bail!(
            "content adapter catalog for {principal}/{workspace} changed during all \
             {MAX_COHERENT_BUILD_ATTEMPTS} build attempts"
        )
    }

    pub fn invalidate_scope(&self, principal: &str, workspace: &str) -> usize {
        if validate_content_scope_component(principal, "content acquisition principal").is_err()
            || validate_content_scope_component(workspace, "content acquisition workspace").is_err()
        {
            return 0;
        }
        let scope = ContentScopeKey::new(principal, workspace);
        let build_lock = self.build_lock_for(&scope);
        let _guard = build_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.capability_resolver
            .invalidate_scope(principal, workspace);
        let mut services = self
            .services
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = services.len();
        services.retain(|key, _| key.scope != scope);
        let removed = before.saturating_sub(services.len());
        if removed > 0 {
            self.cache_metrics
                .invalidations
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    pub fn clear_cache(&self) -> usize {
        let mut services = self
            .services
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Increment while holding the publication lock. A build that captured
        // the old generation can then neither publish after this clear nor
        // race between its generation check and insertion.
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
        let removed = services.len();
        services.clear();
        if removed > 0 {
            self.cache_metrics
                .invalidations
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
        drop(services);
        self.prune_idle_build_locks();
        removed
    }

    pub fn cache_status(&self) -> ContentServiceCacheStatus {
        ContentServiceCacheStatus {
            entry_count: self
                .services
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            build_lock_count: self
                .build_locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            hits: self.cache_metrics.hits.load(Ordering::Relaxed),
            misses: self.cache_metrics.misses.load(Ordering::Relaxed),
            builds: self.cache_metrics.builds.load(Ordering::Relaxed),
            invalidations: self.cache_metrics.invalidations.load(Ordering::Relaxed),
            coalesced_waiters: self.cache_metrics.coalesced_waiters.load(Ordering::Relaxed),
        }
    }

    fn cached(&self, key: &ContentServiceCacheKey) -> Option<Arc<ContentAcquisitionService>> {
        self.services
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
    }

    fn build_lock_for(&self, scope: &ContentScopeKey) -> Arc<Mutex<()>> {
        let mut locks = self
            .build_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if locks.len() >= self.cache_limit && !locks.contains_key(scope) {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        locks
            .entry(scope.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn prune_idle_build_locks(&self) {
        self.build_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|_, lock| Arc::strong_count(lock) > 1);
    }

    fn publish(
        &self,
        scope: ContentScopeKey,
        cache_key: ContentServiceCacheKey,
        service: Arc<ContentAcquisitionService>,
        expected_generation: u64,
    ) -> bool {
        let mut services = self
            .services
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.cache_generation.load(Ordering::Acquire) != expected_generation {
            return false;
        }
        let removed = services
            .keys()
            .filter(|key| key.scope == scope)
            .cloned()
            .collect::<Vec<_>>();
        for key in removed {
            services.remove(&key);
            self.cache_metrics
                .invalidations
                .fetch_add(1, Ordering::Relaxed);
        }
        if services.len() >= self.cache_limit && !services.contains_key(&cache_key) {
            let removed = services.len() as u64;
            services.clear();
            self.cache_metrics
                .invalidations
                .fetch_add(removed, Ordering::Relaxed);
        }
        services.insert(cache_key, service);
        true
    }

    fn skill_roots(&self, principal: &str, workspace: &str) -> Vec<PathBuf> {
        let mut roots = vec![self
            .workspace_manager
            .workspace_layout()
            .scope_skills_root(principal, workspace)];
        roots.extend(crate::magician_v2::config_extras::extra_skills_dirs());
        roots.retain(|root| root.is_dir());
        roots
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn set_build_observer(&self, observer: Arc<dyn Fn(usize) + Send + Sync>) {
        *self
            .build_observer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(observer);
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn set_reader_fetcher(&self, fetcher: super::public_http::PublicHttpFetcher) {
        *self
            .reader_fetcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fetcher);
    }
}

fn build_runtime_registry(
    invoker: Arc<dyn DeterministicCapabilityInvoker>,
    roots: &[PathBuf],
    content_cache: ScopedContentCache,
    capability_registry: &crate::magician_v2::execution::CapabilityRegistry,
    capability_revision: &str,
    storage_root: &Path,
    principal: &str,
    workspace: &str,
    browser_settings: &BrowserRetrievalSettings,
    api_mining_config: &ApiMiningConfig,
    api_mining_base_path: PathBuf,
    retrieval_state: Arc<RetrievalRuntimeState>,
    #[cfg(any(test, feature = "test-fixtures"))] reader_fetcher: Option<
        super::public_http::PublicHttpFetcher,
    >,
) -> Result<(ContentSourceRegistry, Vec<ContentRegistrationIssue>)> {
    let mut registry = shipped_content_source_registry()?;
    let mut issues = Vec::new();

    if browser_settings.enabled
        && browser_settings.verified_api_replay
        && api_mining_config.enable_replay
    {
        let replay = VerifiedApiReplayReader::new(
            api_mining_config.clone(),
            api_mining_base_path,
            browser_settings.max_document_chars,
        );
        if let Err(error) = registry.register_reader(Arc::new(replay)) {
            record_registration_issue(
                &mut issues,
                ContentAdapterKind::Reader,
                "api_replay.read".into(),
                ContentRegistrationErrorClass::AdapterInitializationFailed,
                &error,
            );
        }
    }

    if browser_settings.enabled {
        match crate::magician_v2::execution::primitive_dispatch::browser::AgentBrowserSession::resolve_cli_path_for_scope(
            Some(storage_root),
            Some(principal),
            Some(workspace),
        ) {
            Ok(cli_path) => register_browser_actions(
                &mut registry,
                &mut issues,
                browser_settings,
                cli_path,
                storage_root.to_path_buf(),
                principal,
                workspace,
                retrieval_state,
            ),
            Err(error) => record_registration_issue(
                &mut issues,
                ContentAdapterKind::Reader,
                "browser".into(),
                ContentRegistrationErrorClass::CapabilityUnavailable,
                &error,
            ),
        }
    }

    let (skill_paths, discovery_issues) = discover_skill_markdown_paths_isolated(roots);
    for issue in discovery_issues {
        let error = anyhow::anyhow!("{} at `{}`", issue.message, issue.path.display());
        record_registration_issue(
            &mut issues,
            ContentAdapterKind::SkillInstallation,
            issue.skill_name,
            match issue.class {
                SkillDiscoveryIssueClass::BrokenMarker => {
                    ContentRegistrationErrorClass::BrokenSkillInstallation
                },
                SkillDiscoveryIssueClass::ScanFailed => {
                    ContentRegistrationErrorClass::ManifestScanFailed
                },
            },
            &error,
        );
    }

    for path in &skill_paths {
        let source_id = manifest_source_id(&path);
        let manifest = match optional_validated_capability_discovery_manifest(path) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => continue,
            Err(error) => {
                record_registration_issue(
                    &mut issues,
                    ContentAdapterKind::Discovery,
                    source_id,
                    ContentRegistrationErrorClass::ManifestInvalid,
                    &error,
                );
                continue;
            },
        };
        if !supports_deterministic_cli_capability(capability_registry, &manifest.capability.name) {
            let error = anyhow::anyhow!(
                "capability `{}` is not an executable deterministic CLI-template capability",
                manifest.capability.name
            );
            record_registration_issue(
                &mut issues,
                ContentAdapterKind::Discovery,
                source_id,
                ContentRegistrationErrorClass::CapabilityUnavailable,
                &error,
            );
            continue;
        }
        let adapter_id = manifest.adapter.id.clone();
        let adapter = match CapabilityDiscoveryAdapter::new(manifest, invoker.clone()) {
            Ok(adapter) => adapter,
            Err(error) => {
                record_registration_issue(
                    &mut issues,
                    ContentAdapterKind::Discovery,
                    source_id,
                    ContentRegistrationErrorClass::AdapterInitializationFailed,
                    &error,
                );
                continue;
            },
        };
        if let Err(error) = registry.register_discovery(Arc::new(adapter)) {
            let error_class = if registry.discovery_descriptor(&adapter_id).is_some() {
                ContentRegistrationErrorClass::DuplicateAdapter
            } else {
                ContentRegistrationErrorClass::AdapterInitializationFailed
            };
            record_registration_issue(
                &mut issues,
                ContentAdapterKind::Discovery,
                source_id,
                error_class,
                &error,
            );
        }
    }

    for path in &skill_paths {
        let source_id = manifest_source_id(&path);
        let manifest = match optional_validated_capability_reader_manifest(path) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => continue,
            Err(error) => {
                record_registration_issue(
                    &mut issues,
                    ContentAdapterKind::Reader,
                    source_id,
                    ContentRegistrationErrorClass::ManifestInvalid,
                    &error,
                );
                continue;
            },
        };
        if !supports_deterministic_cli_capability(capability_registry, &manifest.capability.name) {
            let error = anyhow::anyhow!(
                "capability `{}` is not an executable deterministic CLI-template capability",
                manifest.capability.name
            );
            record_registration_issue(
                &mut issues,
                ContentAdapterKind::Reader,
                source_id,
                ContentRegistrationErrorClass::CapabilityUnavailable,
                &error,
            );
            continue;
        }
        let reader_id = manifest.reader.id.clone();
        let reader = match CapabilityStaticContentReader::new_with_revision(
            manifest,
            invoker.clone(),
            content_cache.clone(),
            capability_revision,
            #[cfg(any(test, feature = "test-fixtures"))]
            reader_fetcher.clone(),
        ) {
            Ok(reader) => reader,
            Err(error) => {
                record_registration_issue(
                    &mut issues,
                    ContentAdapterKind::Reader,
                    source_id,
                    ContentRegistrationErrorClass::AdapterInitializationFailed,
                    &error,
                );
                continue;
            },
        };
        if let Err(error) = registry.register_reader(Arc::new(reader)) {
            let error_class = if registry.reader_descriptor(&reader_id).is_some() {
                ContentRegistrationErrorClass::DuplicateAdapter
            } else {
                ContentRegistrationErrorClass::AdapterInitializationFailed
            };
            record_registration_issue(
                &mut issues,
                ContentAdapterKind::Reader,
                source_id,
                error_class,
                &error,
            );
        }
    }

    Ok((registry, issues))
}

#[allow(clippy::too_many_arguments)]
fn register_browser_actions(
    registry: &mut ContentSourceRegistry,
    issues: &mut Vec<ContentRegistrationIssue>,
    settings: &BrowserRetrievalSettings,
    cli_path: PathBuf,
    storage_root: PathBuf,
    principal: &str,
    workspace: &str,
    retrieval_state: Arc<RetrievalRuntimeState>,
) {
    if settings.public_headless_reads {
        let reader = BrowserContentReader::public_headless(
            settings.clone(),
            cli_path.clone(),
            storage_root.clone(),
            principal,
            workspace,
            Arc::clone(&retrieval_state),
        );
        if let Err(error) = registry.register_reader(Arc::new(reader)) {
            record_registration_issue(
                issues,
                ContentAdapterKind::Reader,
                "browser.headless.read".into(),
                ContentRegistrationErrorClass::AdapterInitializationFailed,
                &error,
            );
        }
    }
    if settings.public_handoffs {
        for adapter in [
            BrowserHandoffAdapter::public_discovery(
                settings.approval_ttl_secs,
                Arc::clone(&retrieval_state),
            ),
            BrowserHandoffAdapter::owner_assisted_read(
                settings.approval_ttl_secs,
                Arc::clone(&retrieval_state),
            ),
        ] {
            let descriptor = adapter.descriptor_ref().clone();
            let result = if descriptor.capabilities.discovery {
                registry.register_discovery(Arc::new(adapter))
            } else {
                registry.register_reader(Arc::new(adapter))
            };
            if let Err(error) = result {
                record_registration_issue(
                    issues,
                    if descriptor.capabilities.discovery {
                        ContentAdapterKind::Discovery
                    } else {
                        ContentAdapterKind::Reader
                    },
                    descriptor.retrieval.action_id,
                    ContentRegistrationErrorClass::AdapterInitializationFailed,
                    &error,
                );
            }
        }
    }
    if settings.authenticated_cdp {
        let reader = BrowserContentReader::authenticated_cdp(
            settings.clone(),
            cli_path,
            storage_root,
            principal,
            workspace,
            Arc::clone(&retrieval_state),
        );
        if let Err(error) = registry.register_reader(Arc::new(reader)) {
            record_registration_issue(
                issues,
                ContentAdapterKind::Reader,
                "browser.cdp.read".into(),
                ContentRegistrationErrorClass::AdapterInitializationFailed,
                &error,
            );
        }
        for adapter in [BrowserHandoffAdapter::authenticated_interaction(
            settings.approval_ttl_secs,
            Arc::clone(&retrieval_state),
        )] {
            let descriptor = adapter.descriptor_ref().clone();
            let result = if descriptor.capabilities.discovery {
                registry.register_discovery(Arc::new(adapter))
            } else {
                registry.register_reader(Arc::new(adapter))
            };
            if let Err(error) = result {
                record_registration_issue(
                    issues,
                    if descriptor.capabilities.discovery {
                        ContentAdapterKind::Discovery
                    } else {
                        ContentAdapterKind::Reader
                    },
                    descriptor.retrieval.action_id,
                    ContentRegistrationErrorClass::AdapterInitializationFailed,
                    &error,
                );
            }
        }
    }
}

fn supports_deterministic_cli_capability(
    registry: &crate::magician_v2::execution::CapabilityRegistry,
    capability_name: &str,
) -> bool {
    registry
        .get_pack_definition(capability_name)
        .is_some_and(|pack| {
            matches!(
                pack.implementation,
                ImplementationType::Primitive {
                    provider_name: None,
                    ..
                }
            )
        })
}

fn manifest_source_id(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("unknown-skill")
        .to_string()
}

fn record_registration_issue(
    issues: &mut Vec<ContentRegistrationIssue>,
    kind: ContentAdapterKind,
    source_id: String,
    error_class: ContentRegistrationErrorClass,
    error: &anyhow::Error,
) {
    tracing::warn!(
        ?kind,
        %source_id,
        ?error_class,
        error = %error,
        "content adapter unavailable during scoped runtime registration"
    );
    issues.push(ContentRegistrationIssue {
        kind,
        source_id,
        error_class,
    });
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc, Barrier, Condvar, Mutex as StdMutex, OnceLock,
    };

    use runtime_core::{FileSandboxConfig, ShellSandboxConfig};
    use tempfile::tempdir;

    use super::*;
    use crate::magician_v2::{
        artifact_v2::workspace::ArtifactV2Workspace,
        execution::{
            compiled_dispatch::CompiledDispatchAuthority, ExecutionConfig, MagicutorClient,
        },
        resource_authority::{
            config::ResourceAuthorityConfig, scoped_authority::DiskBackedScopedResolver,
        },
    };

    static SKILLSHUB_ENV_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

    #[test]
    fn browser_registration_exposes_each_exact_mode_as_a_distinct_action() {
        let temp = tempdir().unwrap();
        let cli = temp.path().join("agent-browser");
        std::fs::write(&cli, "fixture").unwrap();
        let mut registry = ContentSourceRegistry::new();
        let mut issues = Vec::new();
        register_browser_actions(
            &mut registry,
            &mut issues,
            &BrowserRetrievalSettings {
                engine: Some("custom-browser".into()),
                ..BrowserRetrievalSettings::default()
            },
            cli,
            temp.path().to_path_buf(),
            "owner",
            "default",
            Arc::new(RetrievalRuntimeState::default()),
        );

        assert!(issues.is_empty());
        let actions = registry
            .discovery_descriptors()
            .into_iter()
            .chain(registry.reader_descriptors())
            .map(|descriptor| descriptor.retrieval.action_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            actions,
            std::collections::BTreeSet::from([
                "browser.cdp.interact_handoff".to_string(),
                "browser.cdp.read".to_string(),
                "browser.headed.read_handoff".to_string(),
                "browser.headless.discover_handoff".to_string(),
                "browser.headless.read".to_string(),
            ])
        );
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn resolver_for(
        root: &Path,
    ) -> (
        Arc<ContentAcquisitionResolver>,
        Arc<CapabilityWorkspaceManager>,
    ) {
        let root = std::fs::canonicalize(root).expect("temporary runtime root should canonicalize");
        let workspace = ArtifactV2Workspace::new(root.join("runtime"));
        let manager = Arc::new(CapabilityWorkspaceManager::new(workspace, &root));
        let magicutor = Arc::new(
            MagicutorClient::new(ExecutionConfig::default())
                .expect("test Magicutor client should construct"),
        );
        let capability_resolver = Arc::new(ScopedCapabilityResolver::new(
            Arc::clone(&manager),
            magicutor,
            FileSandboxConfig::default(),
            ShellSandboxConfig::default(),
            None,
            None,
            None,
        ));
        let authority =
            CompiledDispatchAuthority::from_resolver(Arc::new(DiskBackedScopedResolver::new(
                ResourceAuthorityConfig::default(),
                manager.workspace_layout().clone(),
            )));
        let mut resolver = ContentAcquisitionResolver::with_cache_root(
            capability_resolver,
            Arc::clone(&manager),
            authority,
            root.join("content-cache"),
        );
        resolver.secret_store_resolver = Some(Arc::new(
            crate::magician_v2::secrets::SecretStoreResolver::new_with_capabilities(
                Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                root.join("audit-runtime"),
                crate::magician_v2::secrets::SecretRuntimeCapabilities::fully_available("test"),
            ),
        ));
        (Arc::new(resolver), manager)
    }

    fn write_skill(
        manager: &CapabilityWorkspaceManager,
        principal: &str,
        workspace: &str,
        name: &str,
        skill_md: &str,
        executable: Option<&str>,
    ) -> PathBuf {
        let skill_dir = manager
            .workspace_layout()
            .scope_skills_root(principal, workspace)
            .join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
        if let Some(executable) = executable {
            let bin_dir = skill_dir.join("bin");
            std::fs::create_dir_all(&bin_dir).unwrap();
            let bin = bin_dir.join(name);
            std::fs::write(&bin, executable).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&bin, permissions).unwrap();
            }
        }
        skill_dir
    }

    fn write_exa_skill(
        manager: &CapabilityWorkspaceManager,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        write_skill(
            manager,
            principal,
            workspace,
            "semantic-websearch-via-exa",
            include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md"),
            None,
        )
    }

    fn write_fixture_reader_skill(
        manager: &CapabilityWorkspaceManager,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        let skill = include_str!("../../../../skillshub/htmltotext/SKILL.md")
            .replace("htmltotext", "fixture-reader");
        write_skill(
            manager,
            principal,
            workspace,
            "fixture-reader",
            &skill,
            Some(
                r#"#!/bin/sh
printf '%s\n' '{"content":"A deterministic extracted article body with enough words and characters to pass the full text quality gate without any provider or network dependency. It preserves the selected source identity and exercises the real scoped capability invoker.","method":"fixture","truncated":false,"error":null}'
"#,
            ),
        )
    }

    fn write_fixture_search_skill(
        manager: &CapabilityWorkspaceManager,
        principal: &str,
        workspace: &str,
    ) -> PathBuf {
        let skill = include_str!("../../../../skillshub/arxiv-search/SKILL.md")
            .replace("arxiv-search", "fixture-search")
            .replace("arxiv.discover", "fixture_search.discover")
            .replace("id: arxiv", "id: fixture-search")
            .replace(
                "display_name: arXiv",
                "display_name: Fixture deterministic search",
            );
        write_skill(
            manager,
            principal,
            workspace,
            "fixture-search",
            &skill,
            Some(
                r#"#!/bin/sh
printf '%s\n' '{"items":[{"source_item_id":"fixture-1","title":"Fixture result","cheap_text":"Deterministic fixture summary","canonical_url":"https://example.com/story","source_label":"fixture.test","metadata":{"score":0.95}}]}'
"#,
            ),
        )
    }

    #[test]
    fn unsafe_scope_components_are_rejected_before_workspace_resolution() {
        let temp = tempdir().unwrap();
        let (resolver, _) = resolver_for(temp.path());

        assert!(resolver.resolve_blocking("alice/admin", "default").is_err());
        assert!(resolver
            .resolve_blocking("alice_admin", "main..backup")
            .is_err());
        assert_eq!(resolver.cache_status().entry_count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn container_portable_file_links_register_discovery_and_reader_manifests() {
        use std::os::unix::fs::symlink;

        use crate::magician_v2::skills::path_rewrite::SKILLSHUB_ROOT_ENV;

        let _env_lock = SKILLSHUB_ENV_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempdir().unwrap();
        let portable_skillshub = temp.path().join("portable").join("skillshub");
        let missing_host_skillshub = Path::new("/missing-host/repo/skillshub");
        let (resolver, manager) = resolver_for(temp.path());
        let installed_root = manager
            .workspace_layout()
            .scope_skills_root("owner", "default");

        for (skill, files) in [
            (
                "semantic-websearch-via-exa",
                vec![(
                    "SKILL.md",
                    include_str!("../../../../skillshub/semantic-websearch-via-exa/SKILL.md"),
                )],
            ),
            (
                "htmltotext",
                vec![(
                    "SKILL.md",
                    include_str!("../../../../skillshub/htmltotext/SKILL.md"),
                )],
            ),
        ] {
            let source_dir = portable_skillshub.join(skill);
            let installed_dir = installed_root.join(skill);
            std::fs::create_dir_all(&source_dir).unwrap();
            std::fs::create_dir_all(&installed_dir).unwrap();
            for (name, contents) in files {
                std::fs::write(source_dir.join(name), contents).unwrap();
                symlink(
                    missing_host_skillshub.join(skill).join(name),
                    installed_dir.join(name),
                )
                .unwrap();
            }
        }
        let _env = EnvVarGuard::set(SKILLSHUB_ROOT_ENV, &portable_skillshub);

        let catalog = resolver
            .resolve_blocking("owner", "default")
            .unwrap()
            .catalog();

        assert!(catalog
            .discovery
            .iter()
            .any(|descriptor| descriptor.adapter_id == "exa"));
        assert!(catalog
            .readers
            .iter()
            .any(|descriptor| descriptor.adapter_id == "static-http"));
        assert!(catalog.unavailable.is_empty());
    }

    #[test]
    fn production_constructor_requires_and_retains_dispatch_authority() {
        let temp = tempdir().unwrap();
        let (test_resolver, manager) = resolver_for(temp.path());
        let authority =
            CompiledDispatchAuthority::from_resolver(Arc::new(DiskBackedScopedResolver::new(
                ResourceAuthorityConfig::default(),
                manager.workspace_layout().clone(),
            )));
        let resolver = ContentAcquisitionResolver::new(
            Arc::clone(&test_resolver.capability_resolver),
            manager,
            authority,
            Arc::new(
                crate::magician_v2::secrets::SecretStoreResolver::new_with_capabilities(
                    Box::new(crate::magician_v2::secrets::InMemoryKeyProvider::new()),
                    temp.path().join("audit-runtime"),
                    crate::magician_v2::secrets::SecretRuntimeCapabilities::fully_available("test"),
                ),
            ),
            ContentAcquisitionSettings::default(),
            ApiMiningConfig::default(),
        );

        assert!(!resolver.compiled_dispatch_authority.is_enabled());
        assert!(resolver.secret_store_resolver.is_some());
    }

    #[test]
    fn unchanged_revision_reuses_scope_service_and_isolates_other_scopes() {
        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        write_exa_skill(&manager, "owner", "default");

        let first = resolver.resolve_blocking("owner", "default").unwrap();
        let second = resolver.resolve_blocking("owner", "default").unwrap();
        let other = resolver.resolve_blocking("guest", "default").unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&first, &other));
        assert_eq!(
            first.catalog().discovery.len(),
            3,
            "RSS, DuckDuckGo, and Exa"
        );
        assert_eq!(other.catalog().discovery.len(), 2, "RSS and DuckDuckGo");
        assert!(other.catalog().unavailable.is_empty());
        let status = resolver.cache_status();
        assert_eq!(status.entry_count, 2);
        assert_eq!(status.builds, 2);
        assert!(status.hits >= 1);
    }

    #[test]
    fn embedded_content_contract_revision_rebuilds_service_without_explicit_invalidation() {
        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        let skill_dir = write_exa_skill(&manager, "owner", "default");
        let first = resolver.resolve_blocking("owner", "default").unwrap();

        let manifest_path = skill_dir.join("SKILL.md");
        let mut manifest = std::fs::read_to_string(&manifest_path).unwrap();
        manifest.push_str("\n# catalog revision fixture\n");
        std::fs::write(manifest_path, manifest).unwrap();
        assert!(
            resolver
                .capability_resolver
                .validate_cached_scope_revision_for_test("owner", "default"),
            "the background revision check should swap the stale capability snapshot for the rebuilt one"
        );
        let second = resolver.resolve_blocking("owner", "default").unwrap();

        assert_ne!(first.capability_revision(), second.capability_revision());
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(resolver.cache_status().entry_count, 1);
        assert_eq!(resolver.cache_status().builds, 2);
    }

    #[test]
    fn catalog_mutation_during_build_is_retried_before_publication() {
        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        let skill_dir = write_exa_skill(&manager, "owner", "default");
        let manifest_path = skill_dir.join("SKILL.md");
        let mutated = Arc::new(AtomicBool::new(false));
        let mutated_for_hook = Arc::clone(&mutated);
        resolver.set_build_observer(Arc::new(move |attempt| {
            if attempt == 1 && !mutated_for_hook.swap(true, AtomicOrdering::SeqCst) {
                let mut manifest = std::fs::read_to_string(&manifest_path).unwrap();
                manifest.push_str("\n# changed during runtime construction\n");
                std::fs::write(&manifest_path, manifest).unwrap();
            }
        }));

        let service = resolver.resolve_blocking("owner", "default").unwrap();
        let confirmed = resolver
            .capability_resolver
            .capability_snapshot_for_scope("owner", "default")
            .unwrap();

        assert!(mutated.load(AtomicOrdering::SeqCst));
        assert_eq!(service.capability_revision(), confirmed.revision);
        assert_eq!(resolver.cache_status().builds, 1);
    }

    #[test]
    fn process_clear_during_build_rejects_preclear_publication() {
        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        write_exa_skill(&manager, "owner", "default");
        let cleared = Arc::new(AtomicBool::new(false));
        let cleared_for_hook = Arc::clone(&cleared);
        let weak_resolver = Arc::downgrade(&resolver);
        resolver.set_build_observer(Arc::new(move |attempt| {
            if attempt == 1 && !cleared_for_hook.swap(true, AtomicOrdering::SeqCst) {
                weak_resolver
                    .upgrade()
                    .expect("resolver remains alive during construction")
                    .clear_cache();
            }
        }));

        let service = resolver.resolve_blocking("owner", "default").unwrap();

        assert!(cleared.load(AtomicOrdering::SeqCst));
        assert_eq!(resolver.cache_status().builds, 1);
        assert_eq!(resolver.cache_status().entry_count, 1);
        assert!(Arc::ptr_eq(
            &service,
            &resolver.resolve_blocking("owner", "default").unwrap()
        ));
    }

    #[test]
    fn invalid_optional_manifest_does_not_hide_healthy_sources() {
        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        write_exa_skill(&manager, "owner", "default");
        let broken_skill = include_str!("../../../../skillshub/arxiv-search/SKILL.md")
            .replace("arxiv-search", "broken-source")
            .replacen("schema_version: 1", "schema_version: 999", 1);
        write_skill(
            &manager,
            "owner",
            "default",
            "broken-source",
            &broken_skill,
            None,
        );

        let service = resolver.resolve_blocking("owner", "default").unwrap();
        let catalog = service.catalog();

        assert!(catalog
            .discovery
            .iter()
            .any(|descriptor| descriptor.adapter_id == "rss"));
        assert!(catalog
            .discovery
            .iter()
            .any(|descriptor| descriptor.adapter_id == "exa"));
        assert!(catalog.unavailable.iter().any(|issue| {
            issue.source_id == "broken-source"
                && issue.error_class == ContentRegistrationErrorClass::ManifestInvalid
        }));
    }

    #[cfg(unix)]
    #[test]
    fn broken_generic_skill_installation_is_reported_once_without_adapter_kind() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        write_exa_skill(&manager, "owner", "default");
        let broken = manager
            .workspace_layout()
            .scope_skills_root("owner", "default")
            .join("retired-skill");
        std::fs::create_dir_all(&broken).unwrap();
        symlink(broken.join("missing-SKILL.md"), broken.join("SKILL.md")).unwrap();

        let catalog = resolver
            .resolve_blocking("owner", "default")
            .unwrap()
            .catalog();
        let issues = catalog
            .unavailable
            .iter()
            .filter(|issue| issue.source_id == "retired-skill")
            .collect::<Vec<_>>();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, ContentAdapterKind::SkillInstallation);
        assert_eq!(
            issues[0].error_class,
            ContentRegistrationErrorClass::BrokenSkillInstallation
        );
        assert!(catalog
            .discovery
            .iter()
            .any(|descriptor| descriptor.adapter_id == "exa"));
    }

    #[test]
    fn availability_requires_cli_template_implementation() {
        use crate::magician_v2::execution::{load_pack_defs_from_skills_dir, CapabilityRegistry};

        let registry = CapabilityRegistry::new();
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let mut pack = load_pack_defs_from_skills_dir(&workspace.join("skillshub"))
            .into_iter()
            .find(|pack| pack.name == "semantic-websearch-via-exa")
            .expect("governed Exa pack");
        registry.set_pack_definition(&pack.name.clone(), pack.clone());
        assert!(supports_deterministic_cli_capability(
            &registry,
            "semantic-websearch-via-exa"
        ));

        pack.name = "compiled-only".into();
        pack.implementation = ImplementationType::Compiled {
            provider_name: "compiled-only".into(),
        };
        registry.set_pack_definition("compiled-only", pack);
        assert!(!supports_deterministic_cli_capability(
            &registry,
            "compiled-only"
        ));
    }

    #[test]
    fn concurrent_cold_resolves_coalesce_to_one_service() {
        const CALLERS: usize = 8;
        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        write_exa_skill(&manager, "owner", "default");

        // Gate the sole builder inside the build so `coalesced_waiters` is
        // DETERMINISTIC. The `#[cfg(any(test, feature = "test-fixtures"))]`
        // build_observer runs on the builder thread while it holds the build
        // lock and BEFORE the service is published, so blocking there holds
        // every other caller on the in-flight build. Without this gate the
        // builder races the waiters: under CPU starvation (a full parallel test
        // run) it can finish and publish before the other callers even resume
        // from the barrier, so they observe the warm cache instead of blocking
        // and `coalesced_waiters` is 0 — a spurious failure that has nothing to
        // do with coalescing being broken.
        let gate = Arc::new((StdMutex::new(false), Condvar::new()));
        let gate_for_observer = Arc::clone(&gate);
        resolver.set_build_observer(Arc::new(move |_attempt| {
            let (lock, cvar) = &*gate_for_observer;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cvar.wait(released).unwrap();
            }
        }));

        let barrier = Arc::new(Barrier::new(CALLERS));
        let services = std::thread::scope(|scope| {
            let handles = (0..CALLERS)
                .map(|_| {
                    let resolver = Arc::clone(&resolver);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        resolver.resolve_blocking("owner", "default").unwrap()
                    })
                })
                .collect::<Vec<_>>();

            // Wait until the other CALLERS-1 callers have all coalesced onto the
            // gated build, then release the builder. The counter reaching
            // CALLERS-1 is the synchronization point — the builder cannot publish
            // until then, so no caller can slip through on a cache hit.
            while resolver.cache_status().coalesced_waiters < (CALLERS - 1) as u64 {
                std::thread::yield_now();
            }
            let (lock, cvar) = &*gate;
            *lock.lock().unwrap() = true;
            cvar.notify_all();

            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert!(services
            .iter()
            .skip(1)
            .all(|service| Arc::ptr_eq(&services[0], service)));
        let status = resolver.cache_status();
        assert_eq!(status.builds, 1);
        assert_eq!(status.coalesced_waiters, (CALLERS - 1) as u64);
    }

    #[test]
    fn explicit_scope_invalidation_evicts_capability_and_content_snapshots() {
        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        write_exa_skill(&manager, "owner", "default");
        let first = resolver.resolve_blocking("owner", "default").unwrap();

        assert_eq!(resolver.invalidate_scope("owner", "default"), 1);
        let second = resolver.resolve_blocking("owner", "default").unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(first.capability_revision(), second.capability_revision());
    }

    #[test]
    fn idle_build_locks_are_bounded_and_clearable() {
        let temp = tempdir().unwrap();
        let (resolver, _) = resolver_for(temp.path());
        for index in 0..(DEFAULT_CONTENT_SERVICE_CACHE_LIMIT + 17) {
            let principal = format!("owner-{index}");
            resolver.resolve_blocking(&principal, "default").unwrap();
            resolver.invalidate_scope(&principal, "default");
        }
        assert!(resolver.cache_status().build_lock_count <= DEFAULT_CONTENT_SERVICE_CACHE_LIMIT);
        resolver.clear_cache();
        assert_eq!(resolver.cache_status().build_lock_count, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn production_resolver_runs_discovery_transport_extraction_and_revalidation() {
        use std::{collections::VecDeque, net::SocketAddr, time::Duration};

        use anyhow::anyhow;
        use chrono::Utc;
        use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, ETAG};

        use crate::magician_v2::content_sources::{
            capability_reader::CONTENT_CACHE_OUTCOME_METADATA_KEY,
            public_http::{
                ConditionalHttpRequest, HttpHopResponse, HttpHopTransport, PublicAddressResolver,
                PublicHttpFetchPolicy, PublicHttpFetcher,
            },
            ContentInvocationSource, DiscoveryRequest, FreshnessPolicy, ReadDepth, ReadRequest,
            ReadSelectionEvidence, ReadSelectionReason, RemoteDataPolicy,
        };

        struct StaticResolver;

        #[async_trait::async_trait]
        impl PublicAddressResolver for StaticResolver {
            async fn resolve(&self, _url: &url::Url) -> Result<Vec<SocketAddr>> {
                Ok(vec!["93.184.216.34:443".parse().unwrap()])
            }
        }

        struct QueueTransport {
            responses: StdMutex<VecDeque<HttpHopResponse>>,
            conditionals: StdMutex<Vec<ConditionalHttpRequest>>,
        }

        #[async_trait::async_trait]
        impl HttpHopTransport for QueueTransport {
            async fn send(
                &self,
                _url: &url::Url,
                _pinned_addresses: &[SocketAddr],
                conditional: &ConditionalHttpRequest,
                _policy: &PublicHttpFetchPolicy,
            ) -> Result<HttpHopResponse> {
                self.conditionals
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(conditional.clone());
                self.responses
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .pop_front()
                    .ok_or_else(|| anyhow!("no queued public HTTP response"))
            }
        }

        fn response(status: u16, content_type: Option<&str>, etag: &str) -> HttpHopResponse {
            let mut headers = HeaderMap::new();
            headers.insert(ETAG, HeaderValue::from_str(etag).unwrap());
            if let Some(content_type) = content_type {
                headers.insert(CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap());
            }
            HttpHopResponse {
                status,
                headers,
                body: (status == 200)
                    .then(|| b"<html><article>fixture</article></html>".to_vec())
                    .unwrap_or_default(),
            }
        }

        let transport = Arc::new(QueueTransport {
            responses: StdMutex::new(VecDeque::from([
                response(200, Some("text/html"), "\"v1\""),
                response(304, None, "\"v1\""),
            ])),
            conditionals: StdMutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            PublicHttpFetchPolicy {
                max_response_bytes: 1024,
                max_redirects: 2,
                timeout: Duration::from_secs(5),
                user_agent: "phase6-fixture".into(),
                accepted_media_types: ["text/html".to_string()].into_iter().collect(),
                allow_missing_media_type: false,
            },
            Arc::new(StaticResolver),
            transport.clone(),
        )
        .unwrap();

        let temp = tempdir().unwrap();
        let (resolver, manager) = resolver_for(temp.path());
        write_fixture_search_skill(&manager, "owner", "default");
        write_fixture_reader_skill(&manager, "owner", "default");
        resolver.set_reader_fetcher(fetcher);
        let service = resolver.resolve_blocking("owner", "default").unwrap();
        let catalog = service.catalog();
        assert!(catalog
            .discovery
            .iter()
            .any(|descriptor| descriptor.adapter_id == "fixture-search"));
        assert!(catalog
            .readers
            .iter()
            .any(|descriptor| descriptor.adapter_id == "static-http"));

        let discovery = service
            .discover(
                "fixture-search",
                &DiscoveryRequest {
                    principal: "owner".into(),
                    workspace: "default".into(),
                    intent: Some("fixture intent".into()),
                    query: Some("fixture query".into()),
                    targets: Vec::new(),
                    cursor: None,
                    validators: Default::default(),
                    limit: 1,
                    freshness: FreshnessPolicy::Fresh,
                    remote_query_policy: RemoteDataPolicy::Allow,
                    invocation_source: ContentInvocationSource::UserFeed,
                    options: Default::default(),
                },
            )
            .await
            .unwrap();
        let candidate = discovery.items.into_iter().next().unwrap();
        let read_request = ReadRequest {
            principal: "owner".into(),
            workspace: "default".into(),
            candidate,
            depth: ReadDepth::FullText,
            freshness: FreshnessPolicy::Fresh,
            remote_content_policy: RemoteDataPolicy::Deny,
            invocation_source: ContentInvocationSource::UserFeed,
            selection: Some(ReadSelectionEvidence {
                selected_at_ms: Utc::now().timestamp_millis(),
                reason: ReadSelectionReason::FeedMatch,
                relevance_score: Some(0.95),
            }),
            authority_grant_id: None,
        };
        let first = service.read("static-http", &read_request).await.unwrap();
        let second = service.read("static-http", &read_request).await.unwrap();

        assert!(first.text.contains("real scoped capability invoker"));
        assert_eq!(first.content_hash, second.content_hash);
        assert_eq!(
            first.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            serde_json::json!("miss")
        );
        assert_eq!(
            second.metadata[CONTENT_CACHE_OUTCOME_METADATA_KEY],
            serde_json::json!("revalidated")
        );
        let conditionals = transport
            .conditionals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(conditionals.len(), 2);
        assert_eq!(conditionals[0], ConditionalHttpRequest::default());
        assert_eq!(conditionals[1].etag.as_deref(), Some("\"v1\""));
        let metrics = service.metrics();
        assert_eq!(metrics.discovery["fixture-search"].calls, 1);
        assert_eq!(metrics.reads["static-http"].calls, 2);
        assert_eq!(metrics.reads["static-http"].cache_outcomes["miss"], 1);
        assert_eq!(
            metrics.reads["static-http"].cache_outcomes["revalidated"],
            1
        );
    }
}
