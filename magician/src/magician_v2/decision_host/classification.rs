//! Cached engine policy and bounded, cancellation-independent accounting.
use super::super::realtime_events::ServiceFailure;
use decision_engine_contract::{client::EngineClient, *};
use magicllm::LlmTraceContext;
use std::{
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

const POLICY_TTL: Duration = Duration::from_secs(2);
const NEGATIVE_TTL: Duration = Duration::from_millis(250);
const POLICY_LOOKUP: Duration = Duration::from_millis(50);
pub(crate) const RECEIPT_MARGIN: Duration = Duration::from_millis(250);

fn valid_policy(policy: &decision_engine_contract::classification::ClassificationPolicy) -> bool {
    use decision_engine_contract::classification::BatchStrategy;
    policy.limits.validate().is_ok()
        && policy.observation.validate().is_ok()
        && (policy.observation.gate_sample_rate == 0.0 || !policy.observation_revision.is_empty())
        && (policy.batch_strategy != BatchStrategy::SharedChunk
            || policy.shared_chunk_transform_version
                == Some(magician_decision::config::SHARED_CHUNK_TRANSFORM_VERSION))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyFailure {
    Cold,
    Expired,
    Transport,
    Version,
    Invalid,
}
#[derive(Clone)]
pub(crate) struct Participation {
    pub generation: u64,
    pub discovery_latency_ms: u64,
    pub policy: OperationPolicy,
    pub engine_instance: String,
    pub revision: String,
    scope: magicllm::LlmScope,
    client: EngineClient,
}
#[derive(Clone)]
pub(crate) enum PolicyLookup {
    Disabled,
    Unavailable(PolicyFailure),
    Participating(Participation),
}
impl PolicyLookup {
    /// Unknown policy must not turn an engine outage into a paid LLM decision.
    /// Explicit Off and shadow retain the legacy/reference path.
    pub fn allows_incumbent(&self) -> bool {
        matches!(self, Self::Disabled) || matches!(self, Self::Participating(p) if !p.policy.gate)
    }
}
/// No scoped authority means no paid decision while the engine is enabled.
pub(crate) fn unscoped_policy() -> PolicyLookup {
    if super::classification_backend().is_some() {
        PolicyLookup::Unavailable(PolicyFailure::Invalid)
    } else {
        PolicyLookup::Disabled
    }
}
#[derive(Default)]
struct Cache {
    generation: u64,
    refreshing: bool,
    entry: Option<(Instant, Result<Arc<OperationsResponse>, PolicyFailure>)>,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::default()));
static POLICY_UPDATED: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

pub(super) fn invalidate() {
    *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = Cache::default();
    POLICY_UPDATED.notify_waiters();
}

fn revoke_cached_authority(generation: u64, failure: PolicyFailure) {
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if cache.generation == generation {
        cache.entry = Some((Instant::now(), Err(failure)));
        POLICY_UPDATED.notify_waiters();
    }
}

/// A cold or expired policy never changes authority in this invocation. The
/// bounded refresh primes the next one; the incumbent can start immediately.
pub(crate) fn policy(operation: &str, principal: &str, workspace: &str) -> PolicyLookup {
    let started = Instant::now();
    let Some((generation, client)) = super::classification_backend() else {
        return PolicyLookup::Disabled;
    };
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if cache.generation != generation {
        *cache = Cache {
            generation,
            ..Default::default()
        };
    }
    if let Some((at, entry)) = &cache.entry {
        let ttl = if entry.is_ok() {
            POLICY_TTL
        } else {
            NEGATIVE_TTL
        };
        if at.elapsed() < ttl {
            return match entry {
                Ok(operations) => {
                    match operations.operations.iter().find(|p| p.name == operation) {
                        Some(policy) if policy.shadow || policy.gate => {
                            PolicyLookup::Participating(Participation {
                                generation,
                                discovery_latency_ms: started.elapsed().as_millis() as u64,
                                policy: policy.clone(),
                                engine_instance: operations.engine_instance.clone(),
                                revision: operations.policy_revision.clone(),
                                scope: magicllm::LlmScope::new(principal, workspace),
                                client,
                            })
                        },
                        _ => PolicyLookup::Disabled,
                    }
                },
                Err(failure) => {
                    super::report_health(
                        principal,
                        workspace,
                        "Decision Engine",
                        Err(ServiceFailure::Unavailable),
                    );
                    PolicyLookup::Unavailable(*failure)
                },
            };
        }
    }
    let failure = if cache.entry.is_some() {
        PolicyFailure::Expired
    } else {
        PolicyFailure::Cold
    };
    if !cache.refreshing {
        cache.refreshing = true;
        let (principal, workspace) = (principal.to_owned(), workspace.to_owned());
        tokio::spawn(async move {
            let entry = match client.with_timeout(POLICY_LOOKUP).operations().await {
                Ok(operations)
                    if !operations.engine_instance.is_empty()
                        && !operations.policy_revision.is_empty()
                        && operations
                            .operations
                            .iter()
                            .all(|p| valid_policy(&p.classification)) =>
                {
                    Ok(Arc::new(operations))
                },
                Ok(_) => Err(PolicyFailure::Invalid),
                Err(client::ClientError::ContractMismatch { .. }) => Err(PolicyFailure::Version),
                Err(client::ClientError::Decode(_)) => Err(PolicyFailure::Invalid),
                Err(_) => Err(PolicyFailure::Transport),
            };
            if super::generation() != generation {
                return;
            }
            super::report_health(
                &principal,
                &workspace,
                "Decision Engine",
                if entry.is_ok() {
                    Ok(())
                } else {
                    Err(ServiceFailure::Unavailable)
                },
            );
            let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
            if cache.generation == generation {
                cache.entry = Some((Instant::now(), entry));
                cache.refreshing = false;
                POLICY_UPDATED.notify_waiters();
            }
        });
    }
    PolicyLookup::Unavailable(failure)
}

/// Saved decisions are audited separately from new inference invocations. A
/// normal policy-cache expiry must not retire an otherwise current connection.
/// Wait only for the already-coalesced discovery refresh, never for inference.
pub(crate) async fn policy_for_reconciliation(
    operation: &str,
    principal: &str,
    workspace: &str,
) -> PolicyLookup {
    ready_policy(operation, principal, workspace).await
}

/// Normal gated invocations also await bounded discovery: otherwise every
/// infrequent background operation would miss the two-second policy cache.
pub(crate) async fn ready_policy(
    operation: &str,
    principal: &str,
    workspace: &str,
) -> PolicyLookup {
    let started = Instant::now();
    let updated = POLICY_UPDATED.notified();
    tokio::pin!(updated);
    updated.as_mut().enable();
    let current = policy(operation, principal, workspace);
    if !matches!(
        current,
        PolicyLookup::Unavailable(PolicyFailure::Cold | PolicyFailure::Expired)
    ) {
        return current;
    }
    let _ = tokio::time::timeout(POLICY_LOOKUP + Duration::from_millis(10), updated).await;
    let mut refreshed = policy(operation, principal, workspace);
    if let PolicyLookup::Participating(participation) = &mut refreshed {
        participation.discovery_latency_ms = started.elapsed().as_millis() as u64;
    }
    refreshed
}

impl Participation {
    /// Mutation boundaries require a fresh engine policy before applying.
    /// The short discovery cache only bounds ordinary lookup latency: an
    /// engine reload may happen inside its TTL, so it cannot authorize writes.
    /// This never grants authority to a different revision than the one that
    /// judged.
    pub async fn revalidate(&self) -> bool {
        if super::generation() != self.generation {
            return false;
        }
        let operations = match self.client.with_timeout(POLICY_LOOKUP).operations().await {
            Ok(operations) => operations,
            Err(error) => {
                let (reason, failure) = match error {
                    client::ClientError::ContractMismatch { .. } =>
                        ("version_mismatch", PolicyFailure::Version),
                    client::ClientError::Decode(_) => ("invalid_reply", PolicyFailure::Invalid),
                    _ => ("transport_failure", PolicyFailure::Transport),
                };
                tracing::warn!(reason, "decision classification policy revalidation failed");
                if super::generation() == self.generation {
                    revoke_cached_authority(self.generation, failure);
                    super::report_health(
                        &self.scope.principal,
                        &self.scope.workspace,
                        "Decision Engine",
                        Err(ServiceFailure::Unavailable),
                    );
                }
                return false;
            },
        };
        if super::generation() != self.generation
            || operations.engine_instance.is_empty()
            || operations.policy_revision.is_empty()
            || !operations
                .operations
                .iter()
                .all(|p| valid_policy(&p.classification))
        {
            revoke_cached_authority(self.generation, PolicyFailure::Invalid);
            return false;
        }
        let matches = operations.engine_instance == self.engine_instance
            && operations.policy_revision == self.revision
            && operations.operations.iter().any(|policy| policy == &self.policy);
        super::report_health(
            &self.scope.principal,
            &self.scope.workspace,
            "Decision Engine",
            Ok(()),
        );
        let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if cache.generation != self.generation {
            return false;
        }
        cache.entry = Some((Instant::now(), Ok(Arc::new(operations))));
        POLICY_UPDATED.notify_waiters();
        matches
    }

    /// Fast synchronous cache check for inference and local result assembly.
    /// Writes must call `revalidate` first, then check this again at commit.
    pub fn is_current(&self) -> bool {
        if super::generation() != self.generation || super::classification_backend().is_none() {
            return false;
        }
        let cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        matches!(&cache.entry, Some((at, Ok(current))) if at.elapsed() < POLICY_TTL
            && current.engine_instance == self.engine_instance
            && current.policy_revision == self.revision
            && current.operations.iter().any(|policy| policy == &self.policy))
    }

    pub async fn decide(
        &self,
        mut request: DecideRequest,
        mut scope: LlmTraceContext,
        agent: Option<String>,
        budget: Duration,
        lifetime: Option<Arc<dyn Send + Sync>>,
    ) -> Result<DecideResponse, client::ClientError> {
        if request.batch.items.is_empty() {
            return Ok(DecideResponse {
                batch: batch::BatchResponse {
                    request_id: request.batch.request_id,
                    engine_instance: self.engine_instance.clone(),
                    policy_revision: self.revision.clone(),
                    ..Default::default()
                },
                model_calls: vec![],
                contract_version: CONTRACT_VERSION,
                status: DecideStatus::Answered,
                response: None,
                thresholds: None,
                error: None,
                latency_ms: 0,
            });
        }
        if !self.is_current() || budget <= RECEIPT_MARGIN {
            return Err(client::ClientError::Decode(
                "classification policy expired or budget exhausted".into(),
            ));
        }
        request.batch.expected_policy_revision = self.revision.clone();
        request.batch.execution_budget_ms = budget
            .saturating_sub(RECEIPT_MARGIN)
            .as_millis()
            .min(240000) as u64;
        if scope.activity_id.is_none() {
            scope.set_activity_id(
                super::super::analytics::runtime_activity_layer::current_activity_id()
                    .map(|id| id.to_string()),
            );
        }
        let client = self.client.clone();
        let generation = self.generation;
        let until = tokio::time::Instant::now() + budget;
        // Own bounded inputs and accounting. Dropping this join handle never
        // abandons receipts; this task performs no memory mutation or cache write.
        let task = tokio::spawn(async move {
            let _lifetime = lifetime;
            let remaining = until.saturating_duration_since(tokio::time::Instant::now());
            // Expiry or Off before dispatch is not a service outage or an
            // unknown paid attempt. Do not raise an accounting-gap/HITL notice.
            if remaining <= RECEIPT_MARGIN || super::generation() != generation {
                return Err(client::ClientError::Timeout);
            }
            request.batch.execution_budget_ms = remaining
                .saturating_sub(RECEIPT_MARGIN)
                .as_millis()
                .min(240000) as u64;
            let result = client.decide_with_timeout(&request, remaining).await;
            match &result {
                Ok(reply) => {
                    super::super::analytics::decision_model_telemetry::record(
                        &scope,
                        &reply.model_calls,
                        agent.as_deref(),
                    );
                    super::report_health(
                        &scope.scope.principal,
                        &scope.scope.workspace,
                        "Decision Engine",
                        Ok(()),
                    );
                    for (model, issue) in &reply.batch.model_health {
                        let status = match issue.as_deref() {
                            None => Ok(()),
                            Some("structured_provider_authentication") => {
                                Err(ServiceFailure::Authentication)
                            },
                            Some("structured_provider_credit") => Err(ServiceFailure::Credit),
                            Some("structured_provider_rate_limit") => {
                                Err(ServiceFailure::RateLimit)
                            },
                            Some(_) => Err(ServiceFailure::Unavailable),
                        };
                        super::report_health(
                            &scope.scope.principal,
                            &scope.scope.workspace,
                            &format!("Decision model:{model}"),
                            status,
                        );
                    }
                },
                Err(error) => {
                    // No receipt is not a zero-cost inference. A crash/transport
                    // loss leaves an explicitly unknown accounting interval.
                    tracing::warn!(operation = %request.operation, request_id = %request.batch.request_id,
                        principal = %scope.scope.principal, workspace = %scope.scope.workspace,
                        accounting = "unknown", "decision classification accounting gap");
                    let reason = match error {
                        client::ClientError::ContractMismatch { .. } => "version_mismatch",
                        client::ClientError::Timeout => "timeout",
                        client::ClientError::Decode(_) => "invalid_reply",
                        _ => "transport_failure",
                    };
                    super::emit_model_event(super::super::realtime_events::RuntimeTransportEvent::DecisionAccountingGap {
                        principal: scope.scope.principal.clone(), workspace: scope.scope.workspace.clone(),
                        operation: request.operation.clone(), request_id: request.batch.request_id.clone(),
                        reason: reason.into(), timestamp: chrono::Utc::now().timestamp_millis(),
                    });
                    super::report_health(
                        &scope.scope.principal,
                        &scope.scope.workspace,
                        "Decision Engine",
                        Err(ServiceFailure::Unavailable),
                    );
                },
            }
            result
        });
        tokio::time::timeout_at(until, task)
            .await
            .map_err(|_| client::ClientError::Timeout)?
            .map_err(|_| {
                client::ClientError::Http("classification accounting task stopped".into())
            })?
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn test_expire_policy() {
    if let Some((at, _)) = CACHE.lock().unwrap().entry.as_mut() {
        *at = Instant::now() - POLICY_TTL;
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn test_participation(
    socket: &std::path::Path,
    operations: OperationsResponse,
) -> Participation {
    super::configure(&crate::config::DecisionHostConfig {
        mode: crate::config::DecisionMode::AllEngines,
        socket: Some(socket.display().to_string()),
        ..Default::default()
    });
    let (generation, client) = super::classification_backend().unwrap();
    let policy = operations.operations[0].clone();
    let engine_instance = operations.engine_instance.clone();
    let revision = operations.policy_revision.clone();
    *CACHE.lock().unwrap() = Cache {
        generation,
        refreshing: false,
        entry: Some((Instant::now(), Ok(Arc::new(operations)))),
    };
    Participation {
        generation,
        discovery_latency_ms: 0,
        policy,
        engine_instance,
        revision,
        scope: magicllm::LlmScope::new("test", "test"),
        client,
    }
}
