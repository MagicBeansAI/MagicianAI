//! Magician owns whether a managed engine uses the Decision Engine.
//! The engine process owns all selection policy after that routing boundary.
pub(crate) mod classification;

/// Prime real policy discovery for an external integration replay. This grants
/// no synthetic authority and leaves normal cold-invocation fallback intact.
#[cfg(feature = "test-fixtures")]
pub async fn prime_classification_for_eval(
    operation: &str,
    principal: &str,
    workspace: &str,
) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if matches!(
            classification::policy(operation, principal, workspace),
            classification::PolicyLookup::Participating(_)
        ) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
}

use decision_engine_contract::client::EngineClient;
use decision_engine_contract::wire::Locality;
use std::sync::{LazyLock, OnceLock, RwLock};

use crate::config::{DecisionHostConfig, DecisionMode};

struct DecisionHostSnapshot {
    generation: u64,
    mode: DecisionMode,
    client: EngineClient,
}

impl DecisionHostSnapshot {
    fn from_config(config: &DecisionHostConfig) -> Self {
        let socket = config
            .socket
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                super::process_storage::runtime_root()
                    .join("run")
                    .join("decision-engine.sock")
            });
        Self {
            generation: NEXT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            mode: config.mode,
            client: EngineClient::new(socket, std::time::Duration::from_millis(config.timeout_ms)),
        }
    }

    fn backend_for(&self, engine: &str) -> Option<EngineClient> {
        self.mode.allows(engine).then(|| self.client.clone())
    }
}

static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

static SERVICE: LazyLock<RwLock<Option<DecisionHostSnapshot>>> =
    LazyLock::new(|| RwLock::new(None));
static GLOBAL_DECISION_LOCALITY: OnceLock<Locality> = OnceLock::new();

/// Install or reload the host policy and transport together. No network call:
/// switching Off works even when the service cannot be reached. An in-flight
/// decision retains its captured client; subsequent entries use this snapshot.
pub fn configure(config: &DecisionHostConfig) {
    *SERVICE.write().unwrap_or_else(|p| p.into_inner()) =
        Some(DecisionHostSnapshot::from_config(config));
    classification::invalidate();
}

/// Background memory operations belong to Magician under both enabled modes.
fn classification_backend() -> Option<(u64, EngineClient)> {
    let service = SERVICE.read().unwrap_or_else(|p| p.into_inner());
    let snapshot = service.as_ref()?;
    snapshot
        .backend_for("magician")
        .map(|client| (snapshot.generation, client))
}

pub(crate) fn generation() -> u64 {
    SERVICE
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map_or(0, |s| s.generation)
}

pub fn decision_mode() -> DecisionMode {
    SERVICE
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|snapshot| snapshot.mode)
        .unwrap_or(DecisionMode::Off)
}

/// Owner settings remain accessible even when inference routing is Off.
pub fn settings_backend() -> Option<EngineClient> {
    SERVICE
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|s| s.client.with_timeout(std::time::Duration::from_secs(10)))
}

/// Check the actual selected engine before any socket call or prompt building.
pub fn decision_backend_for(engine: &str) -> Option<EngineClient> {
    SERVICE
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()?
        .backend_for(engine)
}

/// Record the operator's processing locality, carried on every request so
/// the engine keeps body-seeing state on local models in local mode.
pub fn set_global_decision_locality(is_local_mode: bool) {
    let _ = GLOBAL_DECISION_LOCALITY.set(if is_local_mode {
        Locality::Local
    } else {
        Locality::Cloud
    });
}

pub fn global_decision_locality() -> Locality {
    current_locality(
        super::query_analysis::operation_llm_router::global_operation_router().as_deref(),
    )
}

// Use the same live router state as LLM operations. The startup value is
// only a bootstrap fallback before that shared router has been published.
fn current_locality(
    router: Option<&super::query_analysis::operation_llm_router::OperationLlmRouter>,
) -> Locality {
    match router.map(|router| router.processing_locality()) {
        Some(magicllm::ProcessingLocality::Local) => Locality::Local,
        Some(magicllm::ProcessingLocality::Cloud) => Locality::Cloud,
        None => GLOBAL_DECISION_LOCALITY.get().copied().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_routing_locality_follows_the_shared_router_across_live_mode_changes() {
        use super::super::query_analysis::operation_llm_router::OperationLlmRouter;
        let config = |mode: &str| {
            serde_yaml::from_str::<magicllm::config::LLMRouterConfig>(&format!(
                "locality: {mode}\ndefault_profile: fixture\nprofiles:\n  fixture: {{provider: ollama, model: fixture}}\n"
            ))
            .unwrap()
        };
        let router = OperationLlmRouter::new(Some(config("local")));
        let shared = router.clone();
        assert_eq!(current_locality(Some(&shared)), Locality::Local);
        assert!(router.reload_from_config(Some(config("cloud"))));
        assert_eq!(current_locality(Some(&shared)), Locality::Cloud);
        assert!(router.reload_from_config(Some(config("local"))));
        assert_eq!(current_locality(Some(&shared)), Locality::Local);
    }

    #[test]
    fn decision_rail_mode_filters_every_supported_engine_before_transport() {
        for mode in [
            DecisionMode::AllEngines,
            DecisionMode::MagicianOnly,
            DecisionMode::Off,
        ] {
            let snapshot = DecisionHostSnapshot::from_config(&DecisionHostConfig {
                mode,
                socket: Some("/nonexistent/decision-mode-test.sock".into()),
                ..Default::default()
            });
            for engine in [
                "magician",
                "pi",
                "claude_code",
                "codex",
                "codex_app_server",
                "grok",
                "agy",
            ] {
                let expected = mode == DecisionMode::AllEngines
                    || (mode == DecisionMode::MagicianOnly && engine == "magician");
                assert_eq!(
                    snapshot.backend_for(engine).is_some(),
                    expected,
                    "{mode:?}: {engine}"
                );
            }
        }
    }

    #[test]
    fn decision_rail_config_migrates_boolean_and_rejects_ambiguous_controls() {
        for (yaml, expected) in [
            ("{}", DecisionMode::AllEngines),
            ("enabled: true", DecisionMode::AllEngines),
            ("enabled: false", DecisionMode::Off),
            ("mode: all_engines", DecisionMode::AllEngines),
            ("mode: magician_only", DecisionMode::MagicianOnly),
            ("mode: off", DecisionMode::Off),
        ] {
            let config: DecisionHostConfig = serde_yaml::from_str(yaml).unwrap();
            assert_eq!(config.mode, expected);
            let written = serde_yaml::to_string(&config).unwrap();
            assert!(!written.contains("enabled:"));
            assert_eq!(
                serde_yaml::from_str::<DecisionHostConfig>(&written).unwrap(),
                config
            );
        }
        for yaml in ["mode: sometimes", "mode: off\nenabled: true", "models: {}"] {
            assert!(
                serde_yaml::from_str::<DecisionHostConfig>(yaml).is_err(),
                "{yaml}"
            );
        }
    }
}

static HEALTH_BROADCASTER: LazyLock<
    RwLock<std::sync::Weak<super::realtime_events::RuntimeTransportBroadcaster>>,
> = LazyLock::new(|| RwLock::new(std::sync::Weak::new()));

pub fn set_health_broadcaster(
    bus: &std::sync::Arc<super::realtime_events::RuntimeTransportBroadcaster>,
) {
    *HEALTH_BROADCASTER
        .write()
        .unwrap_or_else(|p| p.into_inner()) = std::sync::Arc::downgrade(bus);
}

pub(crate) fn telemetry_broadcaster(
) -> Option<std::sync::Arc<super::realtime_events::RuntimeTransportBroadcaster>> {
    HEALTH_BROADCASTER
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .upgrade()
}

pub(crate) fn emit_model_event(event: super::realtime_events::RuntimeTransportEvent) {
    if let Some(bus) = HEALTH_BROADCASTER
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .upgrade()
    {
        bus.emit_transport_only(event);
    }
}

/// Keep the bounded transport alive after a UI cancellation long enough to
/// receive and account for already-issued model calls. No tool is dispatched
/// here; a cancelled caller still discards the returned action.
pub(crate) async fn action_with_telemetry(
    client: &EngineClient,
    request: &decision_engine_contract::action::ActionRequest,
    mut scope: magicllm::LlmTraceContext,
    agent: Option<String>,
) -> Result<
    decision_engine_contract::action::ActionResponse,
    decision_engine_contract::client::ClientError,
> {
    if scope.activity_id.is_none() {
        scope.set_activity_id(
            super::analytics::runtime_activity_layer::current_activity_id()
                .map(|id| id.to_string()),
        );
    }
    let (client, request) = (client.clone(), request.clone());
    tokio::spawn(async move {
        let response = client.action(&request).await?;
        super::analytics::decision_model_telemetry::record(
            &scope,
            &response.model_calls,
            agent.as_deref(),
        );
        Ok(response)
    })
    .await
    .map_err(|_| {
        decision_engine_contract::client::ClientError::Http(
            "decision telemetry task stopped".into(),
        )
    })?
}

pub fn report_health(
    principal: &str,
    workspace: &str,
    service: &str,
    result: Result<(), super::realtime_events::ServiceFailure>,
) {
    let bus = HEALTH_BROADCASTER
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .upgrade();
    if let Some(bus) = bus {
        bus.report_service_health(principal, workspace, service, result);
    }
}

pub(crate) fn report_reply_health(
    principal: &str,
    workspace: &str,
    reply: &decision_engine_contract::action::ActionResponse,
) {
    use super::realtime_events::ServiceFailure;
    report_health(principal, workspace, "Decision Engine", Ok(()));
    for health in &reply.model_health {
        let status = match health.issue.as_deref() {
            None => Ok(()),
            Some("structured_provider_authentication") => Err(ServiceFailure::Authentication),
            Some("structured_provider_credit") => Err(ServiceFailure::Credit),
            Some("structured_provider_rate_limit") => Err(ServiceFailure::RateLimit),
            Some("structured_provider_unavailable" | "structured_provider_timeout") => {
                Err(ServiceFailure::Unavailable)
            },
            _ => continue,
        };
        report_health(
            principal,
            workspace,
            &format!("Decision model:{}", health.model),
            status,
        );
    }
    if reply.model.is_some() && !reply.reason.starts_with("structured_provider_") {
        report_health(principal, workspace, "Decision model", Ok(()));
    }
    // Older engine responses, and host timeout wrappers, carry only a reason.
    if !reply.model_health.is_empty() && reply.reason != "structured_provider_timeout" {
        return;
    }
    let failure = match reply.reason.as_str() {
        "structured_provider_authentication" => Some(ServiceFailure::Authentication),
        "structured_provider_credit" => Some(ServiceFailure::Credit),
        "structured_provider_rate_limit" => Some(ServiceFailure::RateLimit),
        "structured_provider_unavailable" | "structured_provider_timeout" => {
            Some(ServiceFailure::Unavailable)
        },
        _ => None,
    };
    if let Some(failure) = failure {
        report_health(principal, workspace, "Decision model", Err(failure));
    } else if reply.model.is_some() {
        report_health(principal, workspace, "Decision model", Ok(()));
    }
}
