//! API-first routing layer
//!
//! Routes learned browser network capabilities for replay. The browser runtime
//! now executes through the `browser` skill and pinned `agent-browser` CLI;
//! this layer consumes action-binding metadata from capture/correlation rather
//! than owning a Magicutor browser-action execution lane.
//!
//! ## Decision Logic
//!
//! ```text
//! browser/capture action binding
//!   ├─ enable_replay == false → pass through (browser)
//!   ├─ request template missing → pass through (browser)
//!   ├─ no registry match → pass through (browser)
//!   ├─ match below confidence threshold → pass through (browser)
//!   └─ Candidate+ match → emit ApiReplay action
//! ```
//!
//! ## Fallback
//!
//! When an ApiReplay fails (verification mismatch, network error), the
//! executor falls back to normal browser automation for that step. The
//! failure is recorded on the capability for demotion tracking.

use super::action_binding::{params_from_action_binding, ActionContext};
use super::capability::{ConfidenceLevel, GraphqlOperationKind, SideEffects};
use super::origin_policy::{OriginPolicyStore, OriginReplayCheck};
use super::registry::CapabilityRegistry;
use super::replay::{build_replay_request, build_replay_request_without_body, ReplayRequest};
use super::types::SessionContext;
use crate::config::ApiMiningConfig;
use std::collections::HashMap;
use std::path::Path;

/// Typed classification of *why* a `RouteDecision::PassThrough` was
/// produced. Executor and metrics paths switch on this rather than
/// substring-matching the human-readable `reason` field; the `reason`
/// remains for logging and operator-visible diagnostics.
///
/// New variants should be added rather than overloading `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassThroughKind {
    /// Router is disabled (no config, no registry, or `enable_replay: false`).
    RouterDisabled,
    /// Browser action isn't bound to any learned action signature.
    NoBindingForAction,
    /// No registered capability matches this request (URL/method/body).
    NoCapabilityForRequest,
    /// Capability exists but its confidence is below the per-method
    /// minimum required for replay.
    LowConfidence,
    /// Capability matched but the replay machinery can't build a
    /// request from it (unknown side-effects, malformed template, etc.).
    NotReplayable,
    /// Session context (cookies / auth) isn't ready for this replay.
    SessionNotReady,
    /// Router code panicked or hit an unexpected error path.
    RouterPanic,
    /// Anything not covered by the typed variants above. Carries the
    /// human-readable `reason` for triage; promote to a typed variant
    /// when a new pattern recurs.
    Other,
}

/// Decision from the API router about how to execute an action.
#[derive(Debug, Clone)]
pub enum RouteDecision {
    /// Use the API replay path — a validated capability matches.
    Replay {
        /// The fully resolved replay request
        request: ReplayRequest,
        /// Runtime parameters used to resolve the replay request.
        request_params: HashMap<String, String>,
        /// Capability ID for tracking and promotion
        capability_id: String,
        /// Origin URL
        origin: String,
        /// Whether this capability is Trusted (preferred over UI)
        is_preferred: bool,
        /// Current confidence level
        confidence: ConfidenceLevel,
        /// Whether the effective replay is read-only even if the HTTP verb is POST.
        read_only_hint: bool,
    },
    /// A write-like replay candidate is otherwise ready, but the per-origin
    /// policy requires an operator approval before firing the HTTP request.
    ReplayRequiresHitl {
        /// The fully resolved replay request
        request: ReplayRequest,
        /// Runtime parameters used to resolve the replay request.
        request_params: HashMap<String, String>,
        /// Capability ID for tracking and promotion
        capability_id: String,
        /// Origin URL
        origin: String,
        /// Current confidence level
        confidence: ConfidenceLevel,
        /// Effective side-effect classification that triggered HITL.
        side_effects: SideEffects,
        /// Policy reason carried into the operator-visible request.
        reason: String,
    },
    /// No match or not eligible — fall through to browser execution.
    PassThrough {
        /// Reason the action was not routed to API replay
        reason: String,
        /// Typed classification for executor + metrics dispatch. The
        /// `reason` field stays human-readable for logs; `kind` is
        /// what code switches on.
        kind: PassThroughKind,
    },
}

impl RouteDecision {
    /// Whether this decision routes to API replay
    pub fn is_replay(&self) -> bool {
        matches!(self, RouteDecision::Replay { .. })
    }

    /// Whether this decision has a ready replay request that still needs
    /// explicit operator approval before execution.
    pub fn requires_hitl(&self) -> bool {
        matches!(self, RouteDecision::ReplayRequiresHitl { .. })
    }

    /// Map this decision to the metrics `RouterOutcome` that should be
    /// recorded. Used by the executor's single grouped wiring (PL Task
    /// 4) so per-branch recording inside the router itself isn't
    /// needed — every decision flows through the executor's
    /// `attempt_api_route_replay` regardless of how it was produced,
    /// giving us one canonical recording point.
    pub fn outcome(&self) -> super::metrics::RouterOutcome {
        use super::metrics::RouterOutcome;
        match self {
            RouteDecision::Replay { .. } => RouterOutcome::Replayed,
            RouteDecision::ReplayRequiresHitl { .. } => RouterOutcome::PassThroughNotReplayable,
            RouteDecision::PassThrough { kind, .. } => match kind {
                PassThroughKind::RouterDisabled => RouterOutcome::PassThroughRouterDisabled,
                PassThroughKind::NoBindingForAction => RouterOutcome::PassThroughNoBinding,
                PassThroughKind::NoCapabilityForRequest => RouterOutcome::PassThroughNoCapability,
                PassThroughKind::LowConfidence => RouterOutcome::PassThroughLowConfidence,
                PassThroughKind::NotReplayable => RouterOutcome::PassThroughNotReplayable,
                PassThroughKind::SessionNotReady => RouterOutcome::PassThroughSessionNotReady,
                PassThroughKind::RouterPanic => RouterOutcome::PassThroughRouterPanic,
                PassThroughKind::Other => RouterOutcome::PassThroughOther,
            },
        }
    }
}

/// Decision from the API router about passive XHR validation.
///
/// Unlike `RouteDecision`, validation never replaces browser execution — it only
/// fires a background API replay to compare against the browser's response.
#[derive(Debug, Clone)]
pub enum ValidationDecision {
    /// Fire a background validation replay for this capability.
    Validate {
        /// The fully resolved replay request
        request: ReplayRequest,
        /// Capability ID for tracking and promotion
        capability_id: String,
        /// Origin URL
        origin: String,
        /// Current confidence level
        confidence: ConfidenceLevel,
    },
    /// Skip validation for this trace.
    Skip {
        /// Reason validation was skipped
        reason: String,
    },
}

/// The API router — checks incoming browser actions against the capability
/// registry and decides whether to use API replay or browser automation.
pub struct ApiRouter {
    /// Feature flag configuration
    config: ApiMiningConfig,
    /// Capability registry for lookups
    registry: Option<CapabilityRegistry>,
    /// Base path used for registry initialization (remembered for refresh).
    /// None means use the default path.
    base_path: Option<std::path::PathBuf>,
    /// Per-scope router-outcome counters. Wrapped in `Arc` so multiple
    /// router clones (template / live / replay-only) share one counter
    /// pool; readers (Forge tile, HTTP endpoint) snapshot via
    /// `metrics()`. Default-constructed when not supplied, so every
    /// router has somewhere to record without callers needing to wire
    /// the field explicitly.
    metrics: std::sync::Arc<super::metrics::RouterMetrics>,
    /// Per-scope operator replay policy. When absent, the router keeps the
    /// historical read-only behavior; scoped live routers install this from the
    /// same base path as the capability registry.
    origin_policy: Option<std::sync::Arc<OriginPolicyStore>>,
}

impl ApiRouter {
    /// Create a shared template that carries config only.
    ///
    /// Live replay/validation routers are created per execution scope via
    /// [`Self::with_base_path`]; the shared orchestrator copy must not bind
    /// itself to any unscoped registry.
    pub fn template(config: &ApiMiningConfig) -> Self {
        Self {
            config: config.clone(),
            registry: None,
            base_path: None,
            metrics: std::sync::Arc::new(super::metrics::RouterMetrics::default()),
            origin_policy: None,
        }
    }

    /// Create a new router with the given config.
    ///
    /// If `enable_replay` is false, the router is a no-op (always PassThrough).
    /// The registry is lazily initialized only when replay is enabled.
    pub fn new(config: &ApiMiningConfig) -> Self {
        let needs_registry = config.enable_replay || config.enable_xhr_validation;
        let registry = if needs_registry {
            match CapabilityRegistry::new() {
                Ok(reg) => Some(reg),
                Err(e) => {
                    tracing::warn!("API router: failed to initialize registry: {}", e);
                    None
                },
            }
        } else {
            None
        };

        Self {
            config: config.clone(),
            registry,
            base_path: None,
            metrics: std::sync::Arc::new(super::metrics::RouterMetrics::default()),
            origin_policy: None,
        }
    }

    /// Create a router with a custom base path (for testing or custom storage).
    pub fn with_base_path<P: AsRef<Path>>(config: &ApiMiningConfig, base: P) -> Self {
        let base_path = base.as_ref().to_path_buf();
        let needs_registry = config.enable_replay || config.enable_xhr_validation;
        let registry = if needs_registry {
            match CapabilityRegistry::with_base_path(&base_path) {
                Ok(reg) => Some(reg),
                Err(e) => {
                    tracing::warn!("API router: failed to initialize registry: {}", e);
                    None
                },
            }
        } else {
            None
        };

        Self {
            config: config.clone(),
            registry,
            base_path: Some(base_path),
            metrics: std::sync::Arc::new(super::metrics::RouterMetrics::default()),
            origin_policy: Some(std::sync::Arc::new(OriginPolicyStore::open(base.as_ref()))),
        }
    }

    /// Disabled no-op router (for when API mining is off).
    pub fn disabled() -> Self {
        Self {
            config: ApiMiningConfig::default(),
            registry: None,
            base_path: None,
            metrics: std::sync::Arc::new(super::metrics::RouterMetrics::default()),
            origin_policy: None,
        }
    }

    /// Shared handle to per-scope router-outcome counters. Reads
    /// (Forge tile / HTTP `/router-metrics`) call `.snapshot()` on
    /// the returned handle. Multiple router clones share the same
    /// counter pool via `Arc`, so reads see every write.
    pub fn metrics(&self) -> std::sync::Arc<super::metrics::RouterMetrics> {
        std::sync::Arc::clone(&self.metrics)
    }

    /// Install an externally-owned metrics handle. Used by the
    /// orchestrator/factory when wiring multiple router instances
    /// (e.g., a template + a live router) into one shared counter pool
    /// for a given scope. No-op for the default case where the router
    /// owns its counters.
    pub fn with_metrics(mut self, metrics: std::sync::Arc<super::metrics::RouterMetrics>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Bind this router's metrics handle to the shared per-scope
    /// counter pool resolved via `router_metrics_for_scope`. Use this
    /// at construction time so HTTP endpoints (`GET /router-metrics`)
    /// see every record emitted by any router instance in the same
    /// `(principal, workspace)` scope.
    pub fn bind_metrics_to_scope(mut self, principal: &str, workspace: &str) -> Self {
        self.metrics = super::metrics::router_metrics_for_scope(principal, workspace);
        self
    }

    pub fn with_origin_policy(mut self, origin_policy: std::sync::Arc<OriginPolicyStore>) -> Self {
        self.origin_policy = Some(origin_policy);
        self
    }

    pub fn config(&self) -> &ApiMiningConfig {
        &self.config
    }

    /// Whether the router is active (replay or xhr_validation enabled + registry loaded).
    pub fn is_active(&self) -> bool {
        (self.config.enable_replay || self.config.enable_xhr_validation) && self.registry.is_some()
    }

    /// Route a Navigate action: check if the URL matches a replayable capability.
    ///
    /// This is the primary entry point called before executing a Navigate action.
    /// Other action types (Click, Type, etc.) always pass through.
    /// Delegates to `route_request("GET", url, None, ...)`.
    pub fn route_navigate(
        &self,
        url: &str,
        session: &SessionContext,
        params: &HashMap<String, String>,
    ) -> RouteDecision {
        self.route_request("GET", url, None, session, params)
    }

    /// Generalized routing for any HTTP method, with optional body fingerprint.
    ///
    /// This is the core routing logic. `route_navigate()` delegates here with
    /// method="GET" and no fingerprint. As of v0.6.514 the tier policy
    /// lowered every replayable side-effect class to Candidate so the
    /// visit-2 takeover behavior holds uniformly across methods. See
    /// `capability::min_replay_confidence_for` for the policy table and
    /// the remaining safety floors (URL denylist, body fingerprint,
    /// per-origin opt-in).
    pub fn route_request(
        &self,
        method: &str,
        url: &str,
        body_fingerprint: Option<&str>,
        session: &SessionContext,
        params: &HashMap<String, String>,
    ) -> RouteDecision {
        self.route_request_with_context(method, url, body_fingerprint, None, None, session, params)
    }

    /// Generalized routing with additional GraphQL operation disambiguation.
    pub fn route_request_with_context(
        &self,
        method: &str,
        url: &str,
        body_fingerprint: Option<&str>,
        graphql_operation: Option<&str>,
        graphql_operation_kind: Option<GraphqlOperationKind>,
        session: &SessionContext,
        params: &HashMap<String, String>,
    ) -> RouteDecision {
        // Fast exit if replay is disabled
        if !self.config.enable_replay {
            return RouteDecision::PassThrough {
                reason: "API replay disabled in config".to_string(),
                kind: PassThroughKind::RouterDisabled,
            };
        }

        let registry = match &self.registry {
            Some(r) => r,
            None => {
                // v0.6.515 (Phase 0 Gap 4): elevated from silent debug
                // to warn. The orchestrator constructs the router via
                // `ApiRouter::template()` (registry: None) and only
                // binds the scoped registry inside the executor
                // factory via `with_base_path`. If the binding step
                // is missing or fails, every routing decision lands
                // here and the operator sees no replays at all.
                // Surfacing it as a warn makes the gap discoverable
                // from `tail -f` instead of a debug-log archeology.
                //
                // Logged once per call site (OnceLock::set returns Ok
                // on the first call only) to keep noise bounded.
                static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
                if WARNED.set(()).is_ok() {
                    tracing::warn!(
                        target: "magician::api_mining::router",
                        "ApiRouter consulted with no registry — every request will PassThrough. \
                         Check that `ApiRouter::with_base_path` binds the scoped registry inside \
                         the executor factory (see `executor_factory.rs`)."
                    );
                }
                return RouteDecision::PassThrough {
                    reason: "Registry not initialized".to_string(),
                    kind: PassThroughKind::RouterDisabled,
                };
            },
        };

        // Look up capability by method + URL + optional fingerprint
        let summary = match registry.find_by_url_with_request_context(
            method,
            url,
            body_fingerprint,
            graphql_operation,
            graphql_operation_kind,
        ) {
            Some(s) => s,
            None => {
                return RouteDecision::PassThrough {
                    kind: PassThroughKind::NoCapabilityForRequest,
                    reason: format!(
                        "No capability found for {} {}",
                        method.to_uppercase(),
                        truncate_url(url)
                    ),
                };
            },
        };

        // Tiered eligibility: min confidence depends on method + side effects
        match summary.min_replay_confidence() {
            None => {
                return RouteDecision::PassThrough {
                    kind: PassThroughKind::NotReplayable,
                    reason: format!("Capability '{}' has unknown side effects", summary.name),
                };
            },
            Some(ref min) if summary.confidence < *min => {
                return RouteDecision::PassThrough {
                    kind: PassThroughKind::LowConfidence,
                    reason: format!(
                        "Capability '{}' confidence={:?}, needs {:?} for {} replay",
                        summary.name, summary.confidence, min, summary.method
                    ),
                };
            },
            _ => {}, // proceed to replay
        }

        // Load full capability to build the replay request
        let origin = extract_origin(url);
        let capability = match registry.get_capability(&origin, &summary.id) {
            Ok(cap) => cap,
            Err(e) => {
                return RouteDecision::PassThrough {
                    kind: PassThroughKind::Other,
                    reason: format!("Failed to load capability '{}': {}", summary.id, e),
                };
            },
        };

        // Auto-extract template parameters from the concrete URL.
        let mut merged_params = extract_template_params(&capability.url_template, url);
        for (k, v) in params {
            merged_params.insert(k.clone(), v.clone());
        }

        let request = match build_replay_request(&capability, &merged_params, session) {
            Ok(request) => request,
            Err(e) => {
                return RouteDecision::PassThrough {
                    kind: PassThroughKind::NotReplayable,
                    reason: format!(
                        "Failed to build replay request '{}': {}",
                        capability.name, e
                    ),
                };
            },
        };
        let is_preferred = capability.is_preferred();
        let confidence = capability.confidence.clone();
        let side_effects = capability.effective_side_effects();
        match self.policy_replay_gate(&origin, &side_effects, &confidence) {
            PolicyReplayGate::Allowed => {},
            PolicyReplayGate::RequiresHitl { reason } => {
                return RouteDecision::ReplayRequiresHitl {
                    request,
                    request_params: merged_params,
                    capability_id: capability.id,
                    origin,
                    confidence,
                    side_effects,
                    reason,
                };
            },
            PolicyReplayGate::Denied { kind, reason } => {
                return RouteDecision::PassThrough { reason, kind };
            },
        }
        let read_only_hint = side_effects == SideEffects::ReadOnly;

        RouteDecision::Replay {
            request,
            request_params: merged_params,
            capability_id: capability.id,
            origin,
            is_preferred,
            confidence,
            read_only_hint,
        }
    }

    /// Route a supported browser action using a previously learned action binding.
    ///
    /// Navigate actions continue to use URL-based routing. Other supported UI actions
    /// (click/type/select/fill form/toggle checkbox/press key) can be rewritten to API
    /// replay once the same binding has been learned consistently. Eligibility
    /// still flows through the capability confidence, side-effect, and origin
    /// policy gates used by request routing.
    pub fn route_action_context(
        &self,
        context: &ActionContext,
        session: &SessionContext,
    ) -> RouteDecision {
        if !self.config.enable_replay {
            return RouteDecision::PassThrough {
                reason: "API replay disabled in config".to_string(),
                kind: PassThroughKind::RouterDisabled,
            };
        }

        let registry = match &self.registry {
            Some(r) => r,
            None => {
                return RouteDecision::PassThrough {
                    reason: "Registry not initialized".to_string(),
                    kind: PassThroughKind::RouterDisabled,
                };
            },
        };

        let candidates = registry.find_action_context_candidates(context);
        if candidates.is_empty() {
            return RouteDecision::PassThrough {
                reason: format!(
                    "No learned action binding for {} {}",
                    context.action_type, context.action_signature
                ),
                kind: PassThroughKind::NoBindingForAction,
            };
        }

        let mut last_reason = None;
        for candidate in candidates {
            match candidate.summary.min_replay_confidence() {
                None => {
                    last_reason = Some((
                        PassThroughKind::NotReplayable,
                        format!(
                            "Capability '{}' has unknown side effects",
                            candidate.summary.name
                        ),
                    ));
                    continue;
                },
                Some(ref min) if candidate.summary.confidence < *min => {
                    last_reason = Some((
                        PassThroughKind::LowConfidence,
                        format!(
                            "Capability '{}' confidence={:?}, needs {:?} for {} replay",
                            candidate.summary.name,
                            candidate.summary.confidence,
                            min,
                            candidate.summary.method
                        ),
                    ));
                    continue;
                },
                _ => {},
            }

            let capability = match registry.get_capability(&candidate.origin, &candidate.summary.id)
            {
                Ok(cap) => cap,
                Err(err) => {
                    last_reason = Some((
                        PassThroughKind::Other,
                        format!(
                            "Failed to load capability '{}': {}",
                            candidate.summary.id, err
                        ),
                    ));
                    continue;
                },
            };

            let request_params = match params_from_action_binding(&candidate.binding, context) {
                Ok(params) => params,
                Err(err) => {
                    last_reason = Some((
                        PassThroughKind::NotReplayable,
                        format!(
                            "Failed to bind action params for capability '{}': {}",
                            capability.name, err
                        ),
                    ));
                    continue;
                },
            };

            let request = match build_replay_request(&capability, &request_params, session) {
                Ok(request) => request,
                Err(err) => {
                    last_reason = Some((
                        PassThroughKind::NotReplayable,
                        format!(
                            "Failed to build replay request '{}': {}",
                            capability.name, err
                        ),
                    ));
                    continue;
                },
            };

            let is_preferred = capability.is_preferred();
            let confidence = capability.confidence.clone();
            let side_effects = capability.effective_side_effects();
            match self.policy_replay_gate(&candidate.origin, &side_effects, &confidence) {
                PolicyReplayGate::Allowed => {},
                PolicyReplayGate::RequiresHitl { reason } => {
                    return RouteDecision::ReplayRequiresHitl {
                        request,
                        request_params,
                        capability_id: capability.id,
                        origin: candidate.origin,
                        confidence,
                        side_effects,
                        reason,
                    };
                },
                PolicyReplayGate::Denied { kind, reason } => {
                    last_reason = Some((kind, reason));
                    continue;
                },
            }
            let read_only_hint = side_effects == SideEffects::ReadOnly;

            return RouteDecision::Replay {
                request,
                request_params,
                capability_id: capability.id,
                origin: candidate.origin,
                is_preferred,
                confidence,
                read_only_hint,
            };
        }

        let (kind, reason) = last_reason.unwrap_or((
            PassThroughKind::NoBindingForAction,
            format!(
                "No replayable learned action binding for {} {}",
                context.action_type, context.action_signature
            ),
        ));
        RouteDecision::PassThrough { reason, kind }
    }

    /// Route an XHR/Fetch trace for passive validation (background comparison).
    ///
    /// Unlike `route_request()`, this uses relaxed eligibility:
    /// - Allows any HTTP method (not just ReadOnly)
    /// - Uses `is_validatable()` (Candidate+ any method) instead of `is_replayable()`
    /// - Returns `ValidationDecision` (never replaces browser execution)
    pub fn route_xhr_for_validation(
        &self,
        method: &str,
        url: &str,
        body_fingerprint: Option<&str>,
        session: &SessionContext,
    ) -> ValidationDecision {
        self.route_xhr_for_validation_with_context(
            method,
            url,
            body_fingerprint,
            None,
            None,
            session,
        )
    }

    /// Validation routing with GraphQL operation disambiguation.
    pub fn route_xhr_for_validation_with_context(
        &self,
        method: &str,
        url: &str,
        body_fingerprint: Option<&str>,
        graphql_operation: Option<&str>,
        graphql_operation_kind: Option<GraphqlOperationKind>,
        session: &SessionContext,
    ) -> ValidationDecision {
        let registry = match &self.registry {
            Some(r) => r,
            None => {
                return ValidationDecision::Skip {
                    reason: "Registry not initialized".to_string(),
                };
            },
        };

        let summary = match registry.find_by_url_with_request_context(
            method,
            url,
            body_fingerprint,
            graphql_operation,
            graphql_operation_kind,
        ) {
            Some(s) => s,
            None => {
                return ValidationDecision::Skip {
                    reason: format!(
                        "No capability for {} {}",
                        method.to_uppercase(),
                        truncate_url(url)
                    ),
                };
            },
        };

        // Relaxed check: Candidate+ with a known effective side-effect tier.
        if summary.confidence < ConfidenceLevel::Candidate {
            return ValidationDecision::Skip {
                reason: format!(
                    "Capability '{}' confidence={:?}, below Candidate",
                    summary.name, summary.confidence
                ),
            };
        }

        let origin = extract_origin(url);
        let capability = match registry.get_capability(&origin, &summary.id) {
            Ok(cap) => cap,
            Err(e) => {
                return ValidationDecision::Skip {
                    reason: format!("Failed to load capability '{}': {}", summary.id, e),
                };
            },
        };

        if !capability.is_validatable() {
            return ValidationDecision::Skip {
                reason: format!(
                    "Capability '{}' not validatable (confidence={:?})",
                    summary.name, summary.confidence
                ),
            };
        }

        let effective_side_effects = capability.effective_side_effects();
        if effective_side_effects == SideEffects::Unknown {
            return ValidationDecision::Skip {
                reason: format!(
                    "Capability '{}' has unknown side effects for validation",
                    summary.name
                ),
            };
        }

        // Validation safety: read-like requests are always safe to compare. Write-like
        // requests require an explicit idempotent allowlist match.
        if effective_side_effects != SideEffects::ReadOnly {
            let url_lower = url.to_lowercase();
            let is_safe = self
                .config
                .xhr_validation_idempotent_patterns
                .iter()
                .any(|pattern| url_lower.contains(&pattern.to_lowercase()));
            if !is_safe {
                return ValidationDecision::Skip {
                    reason: format!(
                        "Capability '{}' not safe for validation (no idempotent pattern matched for {})",
                        summary.name,
                        truncate_url(url)
                    ),
                };
            }
        }

        let merged_params = extract_template_params(&capability.url_template, url);
        let request = match build_replay_request_without_body(&capability, &merged_params, session)
        {
            Ok(request) => request,
            Err(error) => {
                return ValidationDecision::Skip {
                    reason: format!(
                        "Failed to build validation request '{}': {}",
                        capability.name, error
                    ),
                };
            },
        };

        ValidationDecision::Validate {
            request,
            capability_id: capability.id,
            origin,
            confidence: capability.confidence,
        }
    }

    /// Process a replay result and update capability promotion/demotion.
    ///
    /// Called after extension or reqwest replay completes. Returns whether
    /// the replay was successful (determines if browser fallback is needed).
    pub fn record_replay_outcome(
        &mut self,
        origin: &str,
        capability_id: &str,
        success: bool,
    ) -> Result<(), String> {
        let registry = match &mut self.registry {
            Some(r) => r,
            None => return Err("Registry not initialized".to_string()),
        };

        let mut capability = registry.get_capability(origin, capability_id)?;

        if success {
            capability.record_replay_success();
        } else {
            capability.record_replay_failure();
        }

        registry.register(&capability)
    }

    /// Record an auth-related replay failure (401/403) on a capability.
    ///
    /// Unlike `record_replay_outcome(false)`, this does NOT demote the
    /// capability — the endpoint works, only the credentials are stale.
    pub fn record_auth_outcome(&mut self, origin: &str, capability_id: &str) -> Result<(), String> {
        let registry = match &mut self.registry {
            Some(r) => r,
            None => return Err("Registry not initialized".to_string()),
        };

        let mut capability = registry.get_capability(origin, capability_id)?;
        capability.record_auth_failure();
        registry.register(&capability)
    }

    /// Get the inner registry (for inspection in tests).
    pub fn registry(&self) -> Option<&CapabilityRegistry> {
        self.registry.as_ref()
    }

    /// Get mutable access to the inner registry.
    pub fn registry_mut(&mut self) -> Option<&mut CapabilityRegistry> {
        self.registry.as_mut()
    }

    /// Refresh the router's capability registry from disk.
    ///
    /// Call this after a mining pipeline run to pick up newly registered
    /// capabilities without restarting the service. If replay is disabled
    /// or the registry cannot be reloaded, this is a no-op.
    pub fn refresh_registry(&mut self) {
        if !self.config.enable_replay && !self.config.enable_xhr_validation {
            return;
        }
        // Use the same base path that was used during initialization.
        // This prevents the refresh from loading a different registry when
        // a custom storage path is configured.
        let result = match &self.base_path {
            Some(path) => CapabilityRegistry::with_base_path(path),
            None => CapabilityRegistry::new(),
        };
        match result {
            Ok(reg) => {
                let s = reg.stats();
                tracing::info!(
                    "[API_MINING] Router registry refreshed ({} capabilities across {} origins)",
                    s.total_capabilities,
                    s.total_origins
                );
                self.registry = Some(reg);
            },
            Err(e) => {
                tracing::warn!(
                    "[API_MINING] Failed to refresh router registry: {}. Keeping stale cache.",
                    e
                );
            },
        }
    }

    fn policy_replay_gate(
        &self,
        origin: &str,
        side_effects: &SideEffects,
        confidence: &ConfidenceLevel,
    ) -> PolicyReplayGate {
        let Some(policy) = self.origin_policy.as_ref() else {
            return PolicyReplayGate::Allowed;
        };
        match policy.check_live_replay(origin, side_effects, confidence) {
            OriginReplayCheck::Allowed => PolicyReplayGate::Allowed,
            OriginReplayCheck::RequiresHitl { reason } => PolicyReplayGate::RequiresHitl {
                reason: format!(
                    "API replay for origin {origin} requires HITL approval before direct write replay: {reason}"
                ),
            },
            OriginReplayCheck::Denied { reason } => PolicyReplayGate::Denied {
                kind: PassThroughKind::NotReplayable,
                reason: format!("API replay blocked by origin policy for {origin}: {reason}"),
            },
        }
    }
}

enum PolicyReplayGate {
    Allowed,
    RequiresHitl {
        reason: String,
    },
    Denied {
        kind: PassThroughKind,
        reason: String,
    },
}

// ─────────────────────────── Helpers ────────────────────────────────────────

/// Extract the origin (scheme + host + port) from a URL.
///
/// Includes port when non-default so `localhost:3001` ≠ `localhost:3002`.
/// Used as the canonical origin key for both capability registry and captured auth state.
pub fn extract_origin(url: &str) -> String {
    // Try to parse as URL, fallback to returning as-is.
    // Includes port when non-default to avoid conflating localhost:3001 vs :3002.
    if let Ok(parsed) = url::Url::parse(url) {
        let host = parsed.host_str().unwrap_or("unknown");
        match parsed.port() {
            Some(port) => format!("{}://{}:{}", parsed.scheme(), host, port),
            None => format!("{}://{}", parsed.scheme(), host),
        }
    } else {
        url.to_string()
    }
}

/// Extract template parameters by comparing a URL against a template.
///
/// Given template `https://example.com/api/users/{id}/posts` and
/// URL `https://example.com/api/users/42/posts`, returns `{"id": "42"}`.
/// Returns an empty map if the template has no parameters or the URL doesn't match.
pub fn extract_template_params(template: &str, url: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();

    let template_path = template.split('?').next().unwrap_or(template);
    let template_parts: Vec<&str> = template_path.split('/').collect();

    let url_parts: Vec<String> = if let Ok(parsed) = url::Url::parse(url) {
        let mut parts = vec![format!("{}:", parsed.scheme()), String::new()];
        let host = match parsed.port() {
            Some(port) => format!("{}:{}", parsed.host_str().unwrap_or_default(), port),
            None => parsed.host_str().unwrap_or_default().to_string(),
        };
        parts.push(host);
        if let Some(segments) = parsed.path_segments() {
            parts.extend(segments.map(|segment| segment.to_string()));
        }
        parts
    } else {
        url.split('/').map(|segment| segment.to_string()).collect()
    };

    if template_parts.len() != url_parts.len() {
        return params;
    }

    for (t, u) in template_parts.iter().zip(url_parts.iter()) {
        if t.starts_with('{') && t.ends_with('}') {
            let param_name = &t[1..t.len() - 1];
            params.insert(param_name.to_string(), u.to_string());
        } else if t != u {
            // Mismatch on a literal segment — not a valid match
            return HashMap::new();
        }
    }

    // Also extract query string parameters from the URL that match template query params
    if let Some(template_query) = template.split('?').nth(1) {
        if let Ok(parsed) = url::Url::parse(url) {
            let url_query_map: HashMap<String, String> = parsed
                .query_pairs()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();

            for pair in template_query.split('&') {
                let mut parts = pair.splitn(2, '=');
                if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                    if val.starts_with('{') && val.ends_with('}') {
                        let param_name = &val[1..val.len() - 1];
                        if let Some(url_val) = url_query_map.get(key) {
                            params.insert(param_name.to_string(), url_val.to_string());
                        }
                    }
                }
            }
        } else if let Some(url_query) = url.split('?').nth(1) {
            let url_query_map: HashMap<&str, &str> = url_query
                .split('&')
                .filter_map(|pair| {
                    let mut parts = pair.splitn(2, '=');
                    Some((parts.next()?, parts.next().unwrap_or("")))
                })
                .collect();

            for pair in template_query.split('&') {
                let mut parts = pair.splitn(2, '=');
                if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                    if val.starts_with('{') && val.ends_with('}') {
                        let param_name = &val[1..val.len() - 1];
                        if let Some(&url_val) = url_query_map.get(key) {
                            params.insert(param_name.to_string(), url_val.to_string());
                        }
                    }
                }
            }
        }
    }

    params
}

/// Truncate a URL for log messages (max 80 chars).
pub fn truncate_url(url: &str) -> String {
    if url.len() <= 80 {
        url.to_string()
    } else {
        // Find a safe UTF-8 char boundary at or before byte 77
        let pos = (0..=77)
            .rev()
            .find(|&i| url.is_char_boundary(i))
            .unwrap_or(0);
        format!("{}...", &url[..pos])
    }
}

// ─────────────────────────── Tests ─────────────────────────────────────────

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::capability::ApiCapability;
    use super::*;
    use tempfile::TempDir;

    fn enabled_config() -> ApiMiningConfig {
        ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            ..Default::default()
        }
    }

    fn router_with_replay_mode(
        config: &ApiMiningConfig,
        temp: &TempDir,
        origin: &str,
        mode: super::super::origin_policy::OriginReplayMode,
    ) -> ApiRouter {
        let policy = std::sync::Arc::new(super::super::origin_policy::OriginPolicyStore::open(
            temp.path(),
        ));
        policy.set_replay_mode(origin, mode).unwrap();
        ApiRouter::with_base_path(config, temp.path()).with_origin_policy(policy)
    }

    fn disabled_config() -> ApiMiningConfig {
        ApiMiningConfig::default() // All false
    }

    /// Smoke test for the typed `PassThroughKind` on `RouteDecision`.
    /// When `enable_replay` is false, the router must short-circuit
    /// every routing path to `PassThrough { kind: RouterDisabled }` so
    /// downstream metrics + executors don't have to substring-match
    /// the `reason` text to know what happened.
    #[test]
    fn pass_through_carries_typed_router_disabled_kind() {
        let temp = TempDir::new().unwrap();
        let router = ApiRouter::with_base_path(&disabled_config(), temp.path());

        let decision = router.route_navigate(
            "https://example.com/api/anything",
            &SessionContext::default(),
            &HashMap::new(),
        );
        match decision {
            RouteDecision::PassThrough { kind, .. } => {
                assert_eq!(kind, PassThroughKind::RouterDisabled);
            },
            other => panic!("expected PassThrough, got {:?}", other),
        }
    }

    fn make_candidate_capability(origin: &str, method: &str, url: &str) -> ApiCapability {
        let mut cap = ApiCapability::new(
            "test_api".to_string(),
            origin.to_string(),
            method.to_string(),
            url.to_string(),
        );
        // Promote to Candidate (3 samples)
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        cap
    }

    fn make_trusted_capability(origin: &str, method: &str, url: &str) -> ApiCapability {
        let mut cap = make_candidate_capability(origin, method, url);
        // 5 successful replays → Trusted
        for _ in 0..5 {
            cap.record_replay_success();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);
        cap
    }

    #[test]
    fn route_action_context_replays_takeover_ready_binding() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());
        let mut cap = make_candidate_capability(
            "https://api.example.com",
            "GET",
            "https://api.example.com/products/{sku}",
        );
        let cap_id = cap.id.clone();
        cap.record_action_binding(super::super::action_binding::ActionBinding {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: Some("click:button[data-sku]".to_string()),
            page_origin: Some("https://shop.example.com".to_string()),
            page_path_template: Some("/products/{id}".to_string()),
            param_bindings: vec![super::super::action_binding::ActionParamBinding {
                action_param: "target".to_string(),
                capability_param: "sku".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(1),
        });
        router.registry_mut().unwrap().register(&cap).unwrap();

        let context = super::super::action_binding::ActionContext {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: Some("click:button[data-sku]".to_string()),
            page_origin: Some("https://shop.example.com".to_string()),
            page_path_template: Some("/products/{id}".to_string()),
            param_values: HashMap::from([("target".to_string(), "abc-123".to_string())]),
            user_values: Vec::new(),
        };

        let decision = router.route_action_context(&context, &SessionContext::default());

        match decision {
            RouteDecision::Replay {
                capability_id,
                request,
                request_params,
                ..
            } => {
                assert_eq!(capability_id, cap_id);
                assert_eq!(request.method, "GET");
                assert_eq!(request.url, "https://api.example.com/products/abc-123");
                assert_eq!(
                    request_params.get("sku").map(String::as_str),
                    Some("abc-123")
                );
            },
            other => panic!("expected action-context replay, got {:?}", other),
        }
    }

    #[test]
    fn route_action_context_replays_semantic_fallback_binding() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());
        let mut cap = make_candidate_capability(
            "https://api.example.com",
            "GET",
            "https://api.example.com/products/{sku}",
        );
        cap.record_action_binding(super::super::action_binding::ActionBinding {
            action_type: "Click".to_string(),
            action_signature: "selector=.rotating-123|button=Left|count=1|iframe=".to_string(),
            semantic_signature: Some("click:button:data-testid=product-card".to_string()),
            page_origin: Some("https://shop.example.com".to_string()),
            page_path_template: Some("/products/{id}".to_string()),
            param_bindings: vec![super::super::action_binding::ActionParamBinding {
                action_param: "target".to_string(),
                capability_param: "sku".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(1),
        });
        router.registry_mut().unwrap().register(&cap).unwrap();

        let context = super::super::action_binding::ActionContext {
            action_type: "Click".to_string(),
            action_signature: "selector=.rotating-456|button=Left|count=1|iframe=".to_string(),
            semantic_signature: Some("click:button:data-testid=product-card".to_string()),
            page_origin: Some("https://shop.example.com".to_string()),
            page_path_template: Some("/products/{id}".to_string()),
            param_values: HashMap::from([("target".to_string(), "abc-123".to_string())]),
            user_values: Vec::new(),
        };

        let decision = router.route_action_context(&context, &SessionContext::default());

        match decision {
            RouteDecision::Replay {
                request,
                request_params,
                ..
            } => {
                assert_eq!(request.url, "https://api.example.com/products/abc-123");
                assert_eq!(
                    request_params.get("sku").map(String::as_str),
                    Some("abc-123")
                );
            },
            other => panic!("expected semantic action-context replay, got {:?}", other),
        }
    }

    #[test]
    fn route_action_context_requires_takeover_ready_binding() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());
        let mut cap = make_candidate_capability(
            "https://api.example.com",
            "GET",
            "https://api.example.com/products/{sku}",
        );
        cap.record_action_binding(super::super::action_binding::ActionBinding {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_bindings: vec![super::super::action_binding::ActionParamBinding {
                action_param: "target".to_string(),
                capability_param: "sku".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 1,
            last_seen_at: Some(1),
        });
        router.registry_mut().unwrap().register(&cap).unwrap();

        let context = super::super::action_binding::ActionContext {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_values: HashMap::from([("target".to_string(), "abc-123".to_string())]),
            user_values: Vec::new(),
        };

        let decision = router.route_action_context(&context, &SessionContext::default());

        match decision {
            RouteDecision::PassThrough { kind, .. } => {
                assert_eq!(kind, PassThroughKind::NoBindingForAction);
            },
            other => panic!("expected no-binding pass-through, got {:?}", other),
        }
    }

    #[test]
    fn route_action_context_skips_unresolvable_url_binding() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());
        let mut cap = make_candidate_capability(
            "https://api.example.com",
            "GET",
            "https://api.example.com/products/{sku}",
        );
        cap.record_action_binding(super::super::action_binding::ActionBinding {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_bindings: Vec::new(),
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(1),
        });
        router.registry_mut().unwrap().register(&cap).unwrap();

        let context = super::super::action_binding::ActionContext {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: None,
            page_origin: None,
            page_path_template: None,
            param_values: HashMap::new(),
            user_values: Vec::new(),
        };

        let decision = router.route_action_context(&context, &SessionContext::default());

        match decision {
            RouteDecision::PassThrough { kind, .. } => {
                assert_eq!(kind, PassThroughKind::NoBindingForAction);
            },
            other => panic!("expected no-binding pass-through, got {:?}", other),
        }
    }

    #[test]
    fn test_disabled_router_always_pass_through() {
        let router = ApiRouter::disabled();
        assert!(!router.is_active());

        let decision = router.route_navigate(
            "https://example.com/api/data",
            &SessionContext::default(),
            &HashMap::new(),
        );
        assert!(!decision.is_replay());
    }

    #[test]
    fn test_config_disabled_pass_through() {
        let temp = TempDir::new().unwrap();
        let config = disabled_config();
        let router = ApiRouter::with_base_path(&config, temp.path());
        assert!(!router.is_active());

        let decision = router.route_navigate(
            "https://example.com/api/data",
            &SessionContext::default(),
            &HashMap::new(),
        );
        assert!(!decision.is_replay());
    }

    #[test]
    fn test_no_matching_capability() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let router = ApiRouter::with_base_path(&config, temp.path());
        assert!(router.is_active());

        let decision = router.route_navigate(
            "https://example.com/api/unknown",
            &SessionContext::default(),
            &HashMap::new(),
        );
        assert!(!decision.is_replay());
        if let RouteDecision::PassThrough { reason, .. } = &decision {
            assert!(reason.contains("No capability found"));
        }
    }

    #[test]
    fn test_observed_capability_not_routed() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        // Register an Observed capability (not yet Candidate)
        let cap = ApiCapability::new(
            "test_api".to_string(),
            "https://example.com".to_string(),
            "GET".to_string(),
            "https://example.com/api/data".to_string(),
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_navigate(
            "https://example.com/api/data",
            &SessionContext::default(),
            &HashMap::new(),
        );
        assert!(!decision.is_replay());
        if let RouteDecision::PassThrough { reason, .. } = &decision {
            assert!(reason.contains("needs Candidate for GET replay"));
        }
    }

    #[test]
    fn test_candidate_capability_routes_to_replay() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "GET",
            "https://example.com/api/users/{id}",
        );
        let cap_id = cap.id.clone();
        router.registry_mut().unwrap().register(&cap).unwrap();

        let mut params = HashMap::new();
        params.insert("id".to_string(), "42".to_string());

        let decision = router.route_navigate(
            "https://example.com/api/users/42",
            &SessionContext::default(),
            &params,
        );

        assert!(decision.is_replay());
        if let RouteDecision::Replay {
            request,
            capability_id,
            is_preferred,
            confidence,
            ..
        } = &decision
        {
            assert_eq!(*capability_id, cap_id);
            assert_eq!(request.method, "GET");
            assert!(request.url.contains("/api/users/"));
            assert!(!is_preferred);
            assert_eq!(*confidence, ConfidenceLevel::Candidate);
        }
    }

    #[test]
    fn test_trusted_capability_is_preferred() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap =
            make_trusted_capability("https://example.com", "GET", "https://example.com/api/data");
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_navigate(
            "https://example.com/api/data",
            &SessionContext::default(),
            &HashMap::new(),
        );

        assert!(decision.is_replay());
        if let RouteDecision::Replay {
            is_preferred,
            confidence,
            ..
        } = &decision
        {
            assert!(is_preferred);
            assert_eq!(*confidence, ConfidenceLevel::Trusted);
        }
    }

    #[test]
    fn test_write_capability_not_routed() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        // POST capability — even at Candidate, should not be routed
        let cap = make_candidate_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/submit",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_navigate(
            "https://example.com/api/submit",
            &SessionContext::default(),
            &HashMap::new(),
        );
        // POST won't match a GET lookup
        assert!(!decision.is_replay());
    }

    #[test]
    fn write_capability_requires_hitl_when_origin_policy_opts_in() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = router_with_replay_mode(
            &config,
            &temp,
            "https://example.com",
            super::super::origin_policy::OriginReplayMode::ReplayWritesWithHitl,
        );

        let mut cap = make_candidate_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/cart/{id}",
        );
        cap.body_template = Some(r#"{"quantity":{{number:body_quantity}}}"#.to_string());
        let cap_id = cap.id.clone();
        router.registry_mut().unwrap().register(&cap).unwrap();

        let params = HashMap::from([
            ("id".to_string(), "item-123".to_string()),
            ("body_quantity".to_string(), "2".to_string()),
        ]);

        let decision = router.route_request(
            "POST",
            "https://example.com/api/cart/item-123",
            None,
            &SessionContext::default(),
            &params,
        );

        assert!(decision.requires_hitl());
        match decision {
            RouteDecision::ReplayRequiresHitl {
                capability_id,
                request,
                request_params,
                side_effects,
                reason,
                ..
            } => {
                assert_eq!(capability_id, cap_id);
                assert_eq!(request.method, "POST");
                assert_eq!(request.url, "https://example.com/api/cart/item-123");
                assert_eq!(request.body.as_deref(), Some(r#"{"quantity":2}"#));
                assert_eq!(
                    request_params.get("id").map(String::as_str),
                    Some("item-123")
                );
                assert_eq!(side_effects, SideEffects::Write);
                assert!(reason.contains("requires HITL approval"));
            },
            other => panic!("expected HITL-required replay, got {other:?}"),
        }
    }

    #[test]
    fn write_action_binding_requires_hitl_when_origin_policy_opts_in() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = router_with_replay_mode(
            &config,
            &temp,
            "https://api.example.com",
            super::super::origin_policy::OriginReplayMode::ReplayWritesWithHitl,
        );
        let mut cap = make_candidate_capability(
            "https://api.example.com",
            "POST",
            "https://api.example.com/cart/{sku}",
        );
        cap.body_template = Some(r#"{"sku":{{string:sku}}}"#.to_string());
        let cap_id = cap.id.clone();
        cap.record_action_binding(super::super::action_binding::ActionBinding {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: Some("click:button[data-sku]".to_string()),
            page_origin: Some("https://shop.example.com".to_string()),
            page_path_template: Some("/products/{id}".to_string()),
            param_bindings: vec![super::super::action_binding::ActionParamBinding {
                action_param: "target".to_string(),
                capability_param: "sku".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(1),
        });
        router.registry_mut().unwrap().register(&cap).unwrap();

        let context = super::super::action_binding::ActionContext {
            action_type: "Click".to_string(),
            action_signature: "selector=button[data-sku]|button=Left|count=1|iframe=".to_string(),
            semantic_signature: Some("click:button[data-sku]".to_string()),
            page_origin: Some("https://shop.example.com".to_string()),
            page_path_template: Some("/products/{id}".to_string()),
            param_values: HashMap::from([("target".to_string(), "abc-123".to_string())]),
            user_values: Vec::new(),
        };

        let decision = router.route_action_context(&context, &SessionContext::default());

        match decision {
            RouteDecision::ReplayRequiresHitl {
                capability_id,
                request,
                side_effects,
                ..
            } => {
                assert_eq!(capability_id, cap_id);
                assert_eq!(request.method, "POST");
                assert_eq!(request.url, "https://api.example.com/cart/abc-123");
                assert_eq!(side_effects, SideEffects::Write);
            },
            other => panic!("expected HITL-required action replay, got {other:?}"),
        }
    }

    #[test]
    fn test_record_replay_outcome_success() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap =
            make_candidate_capability("https://example.com", "GET", "https://example.com/api/data");
        let cap_id = cap.id.clone();
        router.registry_mut().unwrap().register(&cap).unwrap();

        // Record success
        router
            .record_replay_outcome("https://example.com", &cap_id, true)
            .unwrap();

        // Verify replay count incremented
        let updated = router
            .registry()
            .unwrap()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.replay_success_count, 1);
        assert_eq!(updated.consecutive_failures, 0);
    }

    #[test]
    fn test_record_replay_outcome_failure() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap =
            make_candidate_capability("https://example.com", "GET", "https://example.com/api/data");
        let cap_id = cap.id.clone();
        router.registry_mut().unwrap().register(&cap).unwrap();

        // Record failure
        router
            .record_replay_outcome("https://example.com", &cap_id, false)
            .unwrap();

        let updated = router
            .registry()
            .unwrap()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.replay_failure_count, 1);
        assert_eq!(updated.consecutive_failures, 1);
    }

    #[test]
    fn test_record_replay_demotion_on_consecutive_failures() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap =
            make_trusted_capability("https://example.com", "GET", "https://example.com/api/data");
        let cap_id = cap.id.clone();
        router.registry_mut().unwrap().register(&cap).unwrap();

        // 3 consecutive failures → demote from Trusted
        for _ in 0..3 {
            router
                .record_replay_outcome("https://example.com", &cap_id, false)
                .unwrap();
        }

        let updated = router
            .registry()
            .unwrap()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.confidence, ConfidenceLevel::Validated);
    }

    #[test]
    fn test_route_with_session_cookies() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap =
            make_candidate_capability("https://example.com", "GET", "https://example.com/api/data");
        router.registry_mut().unwrap().register(&cap).unwrap();

        let mut session = SessionContext::default();
        session
            .cookies
            .insert("SID".to_string(), "abc123".to_string());

        let decision =
            router.route_navigate("https://example.com/api/data", &session, &HashMap::new());

        assert!(decision.is_replay());
        if let RouteDecision::Replay { request, .. } = &decision {
            let cookie_header = request.headers.get("cookie").unwrap();
            assert!(cookie_header.contains("SID=abc123"));
        }
    }

    #[test]
    fn test_extract_origin() {
        assert_eq!(
            extract_origin("https://mail.google.com/sync/u/0/i/s"),
            "https://mail.google.com"
        );
        // Non-default port is preserved to avoid conflating localhost:3000 vs :3001
        assert_eq!(
            extract_origin("http://localhost:3000/api/v1/data"),
            "http://localhost:3000"
        );
        assert_eq!(
            extract_origin("https://example.com:8443/path"),
            "https://example.com:8443"
        );
        // Default ports (443 for https, 80 for http) are omitted by url::Url parser
        assert_eq!(
            extract_origin("https://example.com/path"),
            "https://example.com"
        );
    }

    #[test]
    fn test_truncate_url() {
        let short = "https://example.com/api/data";
        assert_eq!(truncate_url(short), short);

        let long = "https://example.com/api/v1/users/12345/profile/settings/preferences/theme/colors?include=all&format=json&version=2";
        let truncated = truncate_url(long);
        assert!(truncated.len() <= 80);
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn test_extract_template_params_basic() {
        let params = extract_template_params(
            "https://example.com/api/users/{id}/posts",
            "https://example.com/api/users/42/posts",
        );
        assert_eq!(params.get("id").unwrap(), "42");
    }

    #[test]
    fn test_extract_template_params_multiple() {
        let params = extract_template_params(
            "https://example.com/api/{org}/repos/{repo_id}",
            "https://example.com/api/acme/repos/12345",
        );
        assert_eq!(params.get("org").unwrap(), "acme");
        assert_eq!(params.get("repo_id").unwrap(), "12345");
    }

    #[test]
    fn test_extract_template_params_no_params() {
        let params = extract_template_params(
            "https://example.com/api/data",
            "https://example.com/api/data",
        );
        assert!(params.is_empty());
    }

    #[test]
    fn test_extract_template_params_mismatch() {
        let params = extract_template_params(
            "https://example.com/api/users/{id}",
            "https://example.com/api/posts/42",
        );
        assert!(params.is_empty());
    }

    #[test]
    fn test_auto_param_extraction_in_route() {
        // Verify that route_navigate auto-extracts params without caller supplying them
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "GET",
            "https://example.com/api/users/{id}",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        // Pass empty params — router should auto-extract {id} = "42" from URL
        let decision = router.route_navigate(
            "https://example.com/api/users/42",
            &SessionContext::default(),
            &HashMap::new(),
        );

        assert!(decision.is_replay());
        if let RouteDecision::Replay { request, .. } = &decision {
            // URL should be resolved: /api/users/42, NOT /api/users/{id}
            assert!(
                request.url.contains("/api/users/42"),
                "URL should be resolved but got: {}",
                request.url
            );
            assert!(
                !request.url.contains("{id}"),
                "URL should not contain {{id}} placeholder"
            );
        }
    }

    // ── route_request tests ──

    fn xhr_validation_config() -> ApiMiningConfig {
        ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            xhr_validation_idempotent_patterns: vec!["/sync".to_string(), "/search".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn test_route_request_get_same_as_route_navigate() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap =
            make_candidate_capability("https://example.com", "GET", "https://example.com/api/data");
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision_nav = router.route_navigate(
            "https://example.com/api/data",
            &SessionContext::default(),
            &HashMap::new(),
        );
        let decision_req = router.route_request(
            "GET",
            "https://example.com/api/data",
            None,
            &SessionContext::default(),
            &HashMap::new(),
        );

        // Both should produce Replay decisions
        assert!(decision_nav.is_replay());
        assert!(decision_req.is_replay());
    }

    #[test]
    fn test_route_request_post_at_candidate_requires_explicit_write_policy() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/submit",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_request(
            "POST",
            "https://example.com/api/submit",
            None,
            &SessionContext::default(),
            &HashMap::new(),
        );

        match decision {
            RouteDecision::PassThrough { reason, kind } => {
                assert_eq!(kind, PassThroughKind::NotReplayable);
                assert!(
                    reason.contains("replay_reads_blocks_write"),
                    "reason: {reason}"
                );
            },
            other => panic!("POST at Candidate must not replay by default: {other:?}"),
        }
    }

    fn make_validated_capability(origin: &str, method: &str, url: &str) -> ApiCapability {
        let mut cap = make_candidate_capability(origin, method, url);
        // 3 successful replays → Validated
        for _ in 0..3 {
            cap.record_replay_success();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Validated);
        cap
    }

    #[test]
    fn test_route_request_post_at_validated() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = router_with_replay_mode(
            &config,
            &temp,
            "https://example.com",
            super::super::origin_policy::OriginReplayMode::ReplayWritesWithHitl,
        );

        let cap = make_validated_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/submit",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_request(
            "POST",
            "https://example.com/api/submit",
            None,
            &SessionContext::default(),
            &HashMap::new(),
        );

        match decision {
            RouteDecision::ReplayRequiresHitl {
                request,
                side_effects,
                reason,
                ..
            } => {
                assert_eq!(request.method, "POST");
                assert_eq!(side_effects, SideEffects::Write);
                assert!(
                    reason.contains("requires HITL approval"),
                    "reason: {reason}"
                );
            },
            other => panic!("POST at Validated must wait for HITL instead of replaying: {other:?}"),
        }
    }

    #[test]
    fn test_route_request_delete_at_validated_requires_hitl_or_trust() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = router_with_replay_mode(
            &config,
            &temp,
            "https://example.com",
            super::super::origin_policy::OriginReplayMode::ReplayTrustedWrites,
        );

        let cap = make_validated_capability(
            "https://example.com",
            "DELETE",
            "https://example.com/api/items/{id}",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_request(
            "DELETE",
            "https://example.com/api/items/42",
            None,
            &SessionContext::default(),
            &HashMap::new(),
        );

        assert!(
            matches!(decision, RouteDecision::ReplayRequiresHitl { .. }),
            "DELETE at Validated must require HITL until trusted"
        );
    }

    #[test]
    fn test_route_request_delete_at_trusted() {
        let temp = TempDir::new().unwrap();
        let config = enabled_config();
        let mut router = router_with_replay_mode(
            &config,
            &temp,
            "https://example.com",
            super::super::origin_policy::OriginReplayMode::ReplayTrustedWrites,
        );

        let cap = make_trusted_capability(
            "https://example.com",
            "DELETE",
            "https://example.com/api/items/{id}",
        );
        let cap_id = cap.id.clone();
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_request(
            "DELETE",
            "https://example.com/api/items/42",
            None,
            &SessionContext::default(),
            &HashMap::new(),
        );

        // DELETE at Trusted should be routed to replay
        assert!(decision.is_replay());
        if let RouteDecision::Replay {
            capability_id,
            confidence,
            is_preferred,
            ..
        } = &decision
        {
            assert_eq!(*capability_id, cap_id);
            assert_eq!(*confidence, ConfidenceLevel::Trusted);
            assert!(is_preferred);
        }
    }

    // ── route_xhr_for_validation tests ──

    #[test]
    fn test_route_xhr_validation_post_at_candidate() {
        let temp = TempDir::new().unwrap();
        let config = xhr_validation_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/sync",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation(
            "POST",
            "https://example.com/api/sync",
            None,
            &SessionContext::default(),
        );

        // POST at Candidate should be eligible for validation (relaxed rules)
        assert!(matches!(decision, ValidationDecision::Validate { .. }));
    }

    #[test]
    fn test_route_xhr_validation_with_fingerprint() {
        let temp = TempDir::new().unwrap();
        let config = xhr_validation_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let mut cap_read = ApiCapability::new(
            "sync_read".to_string(),
            "https://example.com".to_string(),
            "POST".to_string(),
            "https://example.com/api/sync".to_string(),
        );
        cap_read.body_fingerprint = Some("json_keys:action,ids".to_string());
        cap_read.add_sample("req-2".to_string());
        cap_read.add_sample("req-3".to_string());

        let mut cap_archive = ApiCapability::new(
            "sync_archive".to_string(),
            "https://example.com".to_string(),
            "POST".to_string(),
            "https://example.com/api/sync".to_string(),
        );
        cap_archive.body_fingerprint = Some("json_keys:action,ids,labels".to_string());
        cap_archive.add_sample("req-4".to_string());
        cap_archive.add_sample("req-5".to_string());

        router.registry_mut().unwrap().register(&cap_read).unwrap();
        router
            .registry_mut()
            .unwrap()
            .register(&cap_archive)
            .unwrap();

        let decision = router.route_xhr_for_validation(
            "POST",
            "https://example.com/api/sync",
            Some("json_keys:action,ids,labels"),
            &SessionContext::default(),
        );

        if let ValidationDecision::Validate { capability_id, .. } = &decision {
            assert_eq!(*capability_id, cap_archive.id);
        } else {
            panic!("Expected Validate decision, got Skip");
        }
    }

    #[test]
    fn test_route_xhr_validation_observed_skipped() {
        let temp = TempDir::new().unwrap();
        let config = xhr_validation_config();
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        // Register an Observed capability (below Candidate)
        let cap = ApiCapability::new(
            "test_api".to_string(),
            "https://example.com".to_string(),
            "POST".to_string(),
            "https://example.com/api/sync".to_string(),
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation(
            "POST",
            "https://example.com/api/sync",
            None,
            &SessionContext::default(),
        );

        assert!(matches!(decision, ValidationDecision::Skip { .. }));
    }

    // ── Method safety gate tests ──

    #[test]
    fn test_xhr_validation_post_blocked_without_pattern() {
        let temp = TempDir::new().unwrap();
        let config = ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            // No patterns → all POST/PUT/PATCH blocked
            xhr_validation_idempotent_patterns: vec![],
            ..Default::default()
        };
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/submit",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation(
            "POST",
            "https://example.com/api/submit",
            None,
            &SessionContext::default(),
        );

        assert!(matches!(decision, ValidationDecision::Skip { .. }));
        if let ValidationDecision::Skip { reason } = &decision {
            assert!(
                reason.contains("not safe for validation"),
                "reason: {}",
                reason
            );
        }
    }

    #[test]
    fn test_xhr_validation_post_allowed_with_matching_pattern() {
        let temp = TempDir::new().unwrap();
        let config = ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            xhr_validation_idempotent_patterns: vec!["/search".to_string()],
            ..Default::default()
        };
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/search/users",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation(
            "POST",
            "https://example.com/api/search/users",
            None,
            &SessionContext::default(),
        );

        assert!(
            matches!(decision, ValidationDecision::Validate { .. }),
            "POST to /search URL should be allowed with /search pattern"
        );
    }

    #[test]
    fn test_xhr_validation_graphql_query_post_allowed_without_pattern() {
        let temp = TempDir::new().unwrap();
        let config = ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            xhr_validation_idempotent_patterns: vec![],
            ..Default::default()
        };
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let mut cap = make_candidate_capability(
            "https://api.example.com",
            "POST",
            "https://api.example.com/graphql",
        );
        cap.graphql_operation = Some("GetUser".to_string());
        cap.graphql_operation_kind = Some(GraphqlOperationKind::Query);
        cap.refresh_side_effects();
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation_with_context(
            "POST",
            "https://api.example.com/graphql",
            None,
            Some("GetUser"),
            Some(GraphqlOperationKind::Query),
            &SessionContext::default(),
        );

        assert!(
            matches!(decision, ValidationDecision::Validate { .. }),
            "GraphQL POST queries should be treated as read-like for validation"
        );
    }

    #[test]
    fn test_xhr_validation_graphql_mutation_post_still_requires_pattern() {
        let temp = TempDir::new().unwrap();
        let config = ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            xhr_validation_idempotent_patterns: vec![],
            ..Default::default()
        };
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let mut cap = make_candidate_capability(
            "https://api.example.com",
            "POST",
            "https://api.example.com/graphql",
        );
        cap.graphql_operation = Some("ArchiveEmail".to_string());
        cap.graphql_operation_kind = Some(GraphqlOperationKind::Mutation);
        cap.refresh_side_effects();
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation_with_context(
            "POST",
            "https://api.example.com/graphql",
            None,
            Some("ArchiveEmail"),
            Some(GraphqlOperationKind::Mutation),
            &SessionContext::default(),
        );

        assert!(matches!(decision, ValidationDecision::Skip { .. }));
    }

    #[test]
    fn test_xhr_validation_get_always_allowed() {
        let temp = TempDir::new().unwrap();
        let config = ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            // No patterns — but GET should still work
            xhr_validation_idempotent_patterns: vec![],
            ..Default::default()
        };
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap =
            make_candidate_capability("https://example.com", "GET", "https://example.com/api/data");
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation(
            "GET",
            "https://example.com/api/data",
            None,
            &SessionContext::default(),
        );

        assert!(
            matches!(decision, ValidationDecision::Validate { .. }),
            "GET should always be allowed regardless of idempotent patterns"
        );
    }

    #[test]
    fn test_xhr_validation_options_always_allowed() {
        let temp = TempDir::new().unwrap();
        let config = ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            xhr_validation_idempotent_patterns: vec![],
            ..Default::default()
        };
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "OPTIONS",
            "https://example.com/api/capabilities",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation(
            "OPTIONS",
            "https://example.com/api/capabilities",
            None,
            &SessionContext::default(),
        );

        assert!(
            matches!(decision, ValidationDecision::Validate { .. }),
            "OPTIONS should be treated as read-only for validation"
        );
    }

    #[test]
    fn test_xhr_validation_pattern_case_insensitive() {
        let temp = TempDir::new().unwrap();
        let config = ApiMiningConfig {
            enable_trace_capture: true,
            enable_mining: true,
            enable_replay: true,
            enable_xhr_validation: true,
            xhr_validation_idempotent_patterns: vec!["/Search".to_string()],
            ..Default::default()
        };
        let mut router = ApiRouter::with_base_path(&config, temp.path());

        let cap = make_candidate_capability(
            "https://example.com",
            "POST",
            "https://example.com/api/search/users",
        );
        router.registry_mut().unwrap().register(&cap).unwrap();

        let decision = router.route_xhr_for_validation(
            "POST",
            "https://example.com/api/search/users",
            None,
            &SessionContext::default(),
        );

        assert!(
            matches!(decision, ValidationDecision::Validate { .. }),
            "Pattern '/Search' should match URL '/api/search/users' case-insensitively"
        );
    }
}
