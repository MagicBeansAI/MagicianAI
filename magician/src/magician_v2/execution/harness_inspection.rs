//! Shared runtime metadata for harness inspection and explicitly granted observers.
//! This path never widens delegation or reads tasks, episodes or goal documents.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::magician_v2::{
    agents::{
        is_system_agent_id, scheduler::SchedulerState, AgentDefinition, AgentDiscoverability,
    },
    harness::{HarnessExecutionContext, HarnessServices},
};

use super::error::ExecutionError;
use crate::magician_v2::{
    agents::{autonomous_goal::focus_area_goal_id, resolve_focus_area_for_goal_id},
    harness::{program_doc::content_sha256, ProgramLoader},
    query_analysis::{operation_llm_router::OperationRoutingOverrides, parent_engine},
};

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RoutingContext {
    operations: Option<Vec<String>>,
    has_tools: bool,
    has_images: bool,
    parent_engine: Option<String>,
    model_override: Option<String>,
    run_overrides: Option<OperationRoutingOverrides>,
}

impl Default for RoutingContext {
    fn default() -> Self {
        Self {
            operations: None,
            has_tools: true,
            has_images: false,
            parent_engine: None,
            model_override: None,
            run_overrides: None,
        }
    }
}

async fn inspect_programs(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    definition: &AgentDefinition,
    shared_defaults: &[String],
) -> Vec<Value> {
    if definition.harness.is_none()
        && !definition.autonomous_config.as_ref().is_some_and(|config| {
            config.focus_areas.iter().any(|area| {
                area.program
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
            })
        })
    {
        return Vec::new();
    }
    let loader = ProgramLoader::new(services.artifact_service.workspace().clone());
    let focuses = definition
        .autonomous_config
        .as_ref()
        .map(|config| config.focus_areas.iter().map(Some).collect::<Vec<_>>())
        .filter(|areas| !areas.is_empty())
        .unwrap_or_else(|| vec![None]);
    let mut result = Vec::new();
    for focus in focuses {
        let goal_id = focus
            .map(|area| focus_area_goal_id(&definition.agent_id, area))
            .or_else(|| {
                (definition.agent_id == context.owner_record.definition.agent_id)
                    .then(|| context.goal_id.clone())
                    .flatten()
            })
            .unwrap_or_else(|| format!("harness:{}", definition.agent_id));
        let loaded = loader
            .load_for_focus_area(&context.principal, &context.workspace, definition, focus)
            .await;
        let (source, error) = match loaded {
            Ok(Some(program)) => (
                Some(loader.source_metadata(&context.principal, &context.workspace, &program)),
                None,
            ),
            Ok(None) => (None, Some("Program source is missing or empty".to_string())),
            Err(error) => (None, Some(error)),
        };
        let shared_names = focus
            .and_then(|area| area.shared_programs.as_deref())
            .unwrap_or(shared_defaults);
        let shared_sources =
            crate::magician_v2::harness::program_doc::load_shared_program_context_with_sources(
                services.artifact_service.workspace(),
                &context.principal,
                &context.workspace,
                shared_names,
                source
                    .as_ref()
                    .map(|source| source.relative_path.as_str())
                    .unwrap_or(""),
            )
            .await
            .map(|(_, sources)| sources)
            .unwrap_or_default();
        let mut shared = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        for name in shared_names.iter().map(|name| name.trim()) {
            if name.is_empty()
                || !seen_names.insert(name)
                || source
                    .as_ref()
                    .is_some_and(|source| source.relative_path == name)
            {
                continue;
            }
            match shared_sources.iter().find(|source| source.relative_path == name) {
                Some(source) => shared.push(json!({"source": source})),
                None => shared.push(json!({"relative_path": name, "error": "Shared source is missing, invalid, empty or omitted by the context budget"})),
            }
        }
        let receipt = loader
            .read_context_ingestion(
                &context.principal,
                &context.workspace,
                &definition.agent_id,
                &goal_id,
            )
            .await;
        let definition_hash = ProgramLoader::definition_sha256(definition).ok();
        let ingestion = match receipt {
            Ok(Some(receipt)) => {
                let matches_current_source = source.as_ref().map(|source| {
                    receipt.sources.iter().any(|prior| {
                        prior.relative_path == source.relative_path
                            && prior.sha256 == source.sha256
                            && prior.section == source.section
                            && prior.included_sha256 == source.included_sha256
                    })
                });
                let matches_current_shared =
                    shared.iter().all(|entry| entry.get("source").is_some())
                        && receipt
                            .sources
                            .iter()
                            .filter(|prior| {
                                !source.as_ref().is_some_and(|source| {
                                    source.relative_path == prior.relative_path
                                        && source.section == prior.section
                                })
                            })
                            .count()
                            == shared_sources.len()
                        && shared_sources.iter().all(|source| {
                            receipt.sources.iter().any(|prior| {
                                prior.relative_path == source.relative_path
                                    && prior.sha256 == source.sha256
                                    && prior.included_sha256 == source.included_sha256
                            })
                        });
                json!({"status": "recorded", "matches_current_program": matches_current_source,
                    "matches_current_shared_sources": matches_current_shared,
                    "matches_current_definition": definition_hash.as_deref().map(|hash| receipt.definition_sha256 == hash), "receipt": receipt})
            },
            Ok(None) => {
                json!({"status": "not_recorded", "note": "No prompt-assembly receipt exists for this binding; historical execution is unknown."})
            },
            Err(_) => {
                json!({"status": "unavailable", "error": "Prompt-assembly receipt is unreadable or invalid"})
            },
        };
        result.push(
            json!({"focus_area": focus.map(|area| &area.name), "source": source,
            "source_error": error, "shared_sources": shared, "ingestion": ingestion}),
        );
    }
    result
}

fn routing(definition: &AgentDefinition) -> Value {
    json!({
        "agent_overrides": definition.llm_routing,
        "unconfigured_operations": "inherit_router_defaults",
    })
}

fn configuration(definition: &AgentDefinition) -> Value {
    json!({"version": definition.version, "disabled": definition.disabled,
    "harness_enabled": definition.harness.is_some(),
    "autonomous": definition.autonomous_config.as_ref().map(|config| json!({
        "schedule": config.schedule, "max_tasks_per_cycle": config.max_tasks_per_cycle,
        "max_steps_per_plan": config.max_steps_per_plan,
        "focus_areas": config.focus_areas.iter().map(|area| json!({
            "name": area.name, "schedule": area.schedule, "program": area.program,
            "shared_programs": area.shared_programs,
        })).collect::<Vec<_>>()
    }))})
}

pub(super) async fn inspect(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    agent_id: Option<&str>,
    routing_context: Option<&Value>,
) -> Result<Value, ExecutionError> {
    let route_context: RoutingContext = routing_context
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            ExecutionError::Step(format!("runtime inspection routing_context: {error}"))
        })?
        .unwrap_or_default();
    let agent_id = agent_id.map(str::trim).filter(|id| !id.is_empty());
    let operations = route_context.operations.clone().unwrap_or_else(|| {
        [
            "agentic_decision",
            "primitive",
            "plan_generation",
            "tool_evaluation",
        ]
        .map(str::to_string)
        .to_vec()
    });
    if operations.is_empty()
        || operations.len() > 32
        || operations
            .iter()
            .any(|operation| operation.trim().is_empty() || operation.len() > 128)
    {
        return Err(ExecutionError::Step(
            "runtime inspection routing_context.operations requires 1–32 bounded operation names"
                .into(),
        ));
    }
    let store = context.scoped_definition_store(services);
    let records = store
        .list_persisted_definitions()
        .await
        .map_err(|error| ExecutionError::Step(format!("runtime inspection: {error}")))?;
    let disabled = crate::magician_v2::agents::types::disabled_agent_hierarchy(
        records.iter().map(|record| &record.definition),
    );
    let mut harness_status = crate::magician_v2::harness::config::load_harness_runtime_status();
    if harness_status.config_error.is_some() {
        harness_status.config_error =
            Some("Runtime configuration is unavailable or invalid".into());
    }
    let shared_defaults =
        crate::config::load_magician_config_from_path(&crate::config::magician_config_path())
            .map(|config| config.harness.shared_program_docs)
            .unwrap_or_default();
    let paused_path = store.storage().agents_root().join("paused_agents.json");
    let paused_agents = match tokio::fs::read(&paused_path).await {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("paused_agent_ids")
                    .and_then(Value::as_array)
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => None,
    };
    // The API scheduler and runtime have separate caches over this shared store.
    // Read strictly, without snapshot()'s recovery writes or creating a scheduler.
    // Corrupt/missing state stays unavailable, never a healthy empty roster.
    let scheduler = store
        .storage()
        .read_json::<SchedulerState>(store.storage().scheduler_state_path())
        .await;
    let scheduler_error = scheduler
        .as_ref()
        .err()
        .map(|_| "Scheduler registration state is unavailable or invalid.");
    let scheduler = scheduler.ok();
    let mut agents = Vec::new();
    for record in records {
        let definition = &record.definition;
        let visible = if context.owner_record.definition.harness.is_some() {
            // Richer inspection must not widen the harness's delegation/focus scope.
            context.scope.contains(&definition.agent_id)
                || (agent_id == Some(context.owner_record.definition.agent_id.as_str())
                    && definition.agent_id == context.owner_record.definition.agent_id)
        } else {
            !is_system_agent_id(&definition.agent_id)
                && definition.invocation_policy.discoverability != AgentDiscoverability::SurfaceOnly
        };
        if !visible || agent_id.is_some_and(|id| id != definition.agent_id) {
            continue;
        }
        let loaded_snapshot = services
            .runtime
            .cached_definition_with_load_time(
                &context.principal,
                &context.workspace,
                &definition.agent_id,
            )
            .await;
        let (loaded, loaded_at) = match loaded_snapshot {
            Some((definition, at)) => (Some(definition), at),
            None => (None, None),
        };
        let source_path = store
            .storage()
            .agent_definition_path(&definition.agent_id)
            .map_err(|error| ExecutionError::Step(format!("runtime inspection: {error}")))?;
        let source_bytes = tokio::fs::read(&source_path).await.ok();
        let definition_source = json!({
            "absolute_path": std::fs::canonicalize(&source_path).unwrap_or(source_path),
            "file_sha256": source_bytes.as_ref().map(|bytes| { use sha2::{Digest, Sha256}; format!("{:x}", Sha256::digest(bytes)) }),
            "definition_sha256": ProgramLoader::definition_sha256(definition).ok(),
            "updated_at": record.updated_at,
        });
        let cached_paused = if let Some(index) = &services.scoped_paused_agents {
            let paused = index
                .read()
                .await
                .get(&(context.principal.clone(), context.workspace.clone()))
                .cloned();
            match paused {
                Some(paused) => Some(paused.read().await.contains(&definition.agent_id)),
                None => None,
            }
        } else {
            None
        };
        let agent_paused = paused_agents
            .as_ref()
            .map(|ids| {
                ids.iter().any(|id| id.trim() == definition.agent_id) || cached_paused == Some(true)
            })
            .or_else(|| (cached_paused == Some(true)).then_some(true));
        let admission = json!({"harness_paused": harness_status.paused,
            "disabled_by_hierarchy": disabled.contains(&definition.agent_id), "agent_paused": agent_paused,
            "configuration_allows_start": agent_paused.map(|paused| !paused && !harness_status.paused && !disabled.contains(&definition.agent_id)),
            "evidence": "Current configuration and durable pause state; execution-time lifecycle and resource gates still revalidate."});
        let effective_definition = loaded.as_ref().unwrap_or(definition);
        let programs =
            inspect_programs(services, context, effective_definition, &shared_defaults).await;
        let resolved_routing = match services.runtime.inspection_router() {
            Some(router) => {
                let overrides = OperationRoutingOverrides::merge(
                    effective_definition
                        .llm_routing
                        .as_ref()
                        .map(OperationRoutingOverrides::from_llm_routing_config),
                    route_context.run_overrides.clone(),
                );
                let router = router.with_routing_overrides(overrides);
                parent_engine::with_parent_engine(route_context.parent_engine.as_deref(), async {
                    operations
                        .iter()
                        .map(|operation| operation.trim())
                        .map(|operation| {
                            (
                                operation.to_string(),
                                match router.inspect_operation_route(
                                    operation,
                                    route_context.has_tools,
                                    route_context.has_images,
                                    route_context.model_override.as_deref(),
                                ) {
                                    Ok(route) => route,
                                    Err(error) => json!({"error": error.to_string()}),
                                },
                            )
                        })
                        .collect::<serde_json::Map<String, Value>>()
                })
                .await
                .into()
            },
            None => Value::Null,
        };
        let registrations = scheduler.as_ref().map(|state| {
            let mut entries = state.entries.iter()
                .filter(|(_, entry)| entry.agent_id == definition.agent_id)
                .map(|(key, entry)| {
                    // Goal/task ids and event filters may describe private work.
                    let trigger = match &entry.registration {
                        crate::magician_v2::agents::scheduler::SchedulerTriggerRegistration::Cron {
                            schedule, timezone, next_run_at,
                        } => json!({"kind": "cron", "schedule": schedule,
                            "timezone": timezone, "next_run_at": next_run_at}),
                        crate::magician_v2::agents::scheduler::SchedulerTriggerRegistration::Event { .. } => json!({"kind": "event"}),
                        crate::magician_v2::agents::scheduler::SchedulerTriggerRegistration::Idle => json!({"kind": "idle"}),
                    };
                    let focus = resolve_focus_area_for_goal_id(effective_definition, &entry.goal_id);
                    let matches_focus = match (&entry.registration, focus) {
                        (crate::magician_v2::agents::scheduler::SchedulerTriggerRegistration::Cron { schedule, .. }, Some(focus)) => {
                            let expected = focus.schedule.as_ref().or_else(|| effective_definition.autonomous_config.as_ref().map(|config| &config.schedule));
                            expected.map(|expected| expected == schedule)
                        }, _ => None,
                    };
                    let duplicate_count = state.entries.values().filter(|other|
                        other.agent_id == entry.agent_id && other.goal_id == entry.goal_id &&
                        match (&other.registration, &entry.registration) {
                            (crate::magician_v2::agents::scheduler::SchedulerTriggerRegistration::Cron { schedule: a, timezone: at, .. },
                             crate::magician_v2::agents::scheduler::SchedulerTriggerRegistration::Cron { schedule: b, timezone: bt, .. }) => a == b && at == bt,
                            (a, b) => a == b,
                        }).count();
                    json!({"registration_id": content_sha256(&format!("{}/{}/{key}", context.principal, context.workspace)), "trigger": trigger,
                        "last_triggered": entry.last_triggered, "matches_current_focus_schedule": matches_focus,
                        "duplicate_count": duplicate_count.saturating_sub(1), "admission": admission})
                }).collect::<Vec<_>>();
            entries.sort_by_key(Value::to_string);
            entries
        });
        let focus_registration_status = scheduler.as_ref().map(|state|
            effective_definition.autonomous_config.as_ref().map(|config| config.focus_areas.iter().map(|focus| {
                let goal = focus_area_goal_id(&definition.agent_id, focus);
                let expected = focus.schedule.as_ref().unwrap_or(&config.schedule);
                let count = state.entries.values().filter(|entry| entry.agent_id == definition.agent_id && entry.goal_id == goal &&
                    matches!(&entry.registration, crate::magician_v2::agents::scheduler::SchedulerTriggerRegistration::Cron { schedule, .. } if schedule == expected)).count();
                json!({"focus_area": focus.name, "expected_schedule": expected, "matching_registration_count": count})
            }).collect::<Vec<_>>()).unwrap_or_default());
        let active_count = services
            .runtime
            .active_cycles_for_agent_in_scope(
                &context.principal,
                &context.workspace,
                &definition.agent_id,
            )
            .await
            .len();
        agents.push(json!({
            "agent_id": definition.agent_id,
            "name": definition.name,
            "saved": {
                "version": record.version(), "disabled": definition.disabled,
                "harness_enabled": definition.harness.is_some(),
                "schedule": definition.autonomous_config.as_ref().map(|config| &config.schedule),
                "routing": routing(definition),
                "source": definition_source,
                "configuration": configuration(definition),
            },
            "runtime": {
                "definition_loaded": loaded.is_some(),
                "definition_loaded_at": loaded_at,
                "configuration": loaded.as_ref().map(configuration),
                "definition_sha256": loaded.as_ref().and_then(|definition| ProgramLoader::definition_sha256(definition).ok()),
                "matches_saved_definition": loaded.as_ref().map(|live|
                    serde_json::to_value(live).ok() == serde_json::to_value(definition).ok()),
                "routing": loaded.as_ref().map(routing),
                "resolved_routing": resolved_routing,
                "routing_definition_source": if loaded.is_some() { "runtime_cache" } else { "saved_definition_only" },
                "programs": programs,
                "admission": admission,
                "scheduler_available": scheduler.is_some(),
                "scheduler_source": "durable_registration_state",
                "scheduler_error": scheduler_error,
                "scheduler_last_tick": scheduler.as_ref().and_then(|state| state.last_tick),
                "scheduler_tick_age_seconds": scheduler.as_ref().and_then(|state| state.last_tick)
                    .map(|tick| (chrono::Utc::now() - tick).num_seconds()),
                "registrations": registrations,
                "registration_count": registrations.as_ref().map(Vec::len),
                "registration_list_complete_for_agent": scheduler.is_some(),
                "focus_registration_status": focus_registration_status,
                "active_cycles": active_count,
            },
        }));
    }
    if agent_id.is_some() && agents.is_empty() {
        return Err(ExecutionError::Step(
            "runtime inspection: agent was not found in the active scope".into(),
        ));
    }
    agents.sort_by_key(|agent| agent["agent_id"].as_str().unwrap_or_default().to_string());
    // Report configured defaults, not a prediction of a future model call:
    // parent engine, request shape and run-level overrides can still apply.
    let router_defaults = services
        .runtime
        .inspection_router()
        .and_then(|router| router.router_config_snapshot())
        .map(|config| {
            [
                "agentic_decision",
                "primitive",
                "plan_generation",
                "tool_evaluation",
            ]
            .into_iter()
            .map(|operation| {
                let profile_name = config
                    .operation_mapping
                    .get(operation)
                    .map(|selector| selector.default_profile())
                    .unwrap_or(config.default_profile.as_str());
                let profile = config.profiles.get(profile_name);
                (
                    operation.to_string(),
                    json!({"profile": profile_name,
                        "provider": profile.map(|profile| &profile.provider),
                        "model": profile.map(|profile| &profile.model)}),
                )
            })
            .collect::<serde_json::Map<String, Value>>()
        });
    Ok(json!({
        "inspection_mode": "runtime_metadata",
        "principal": context.principal, "workspace": context.workspace,
        "total_count": agents.len(), "agents": agents,
        "configured_router_defaults": router_defaults,
        "harness_runtime": harness_status,
        "observed_at": chrono::Utc::now(),
        "routing_evidence": "resolved_routing uses the dispatch resolver with loaded agent settings, current owner overrides and the supplied routing_context. Default context: tools=true, images=false, no parent/run/request override. This is a read-only resolution, not a provider call or a prediction of unspecified future inputs.",
    }))
}
