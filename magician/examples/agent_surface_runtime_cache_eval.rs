//! Provider-free production evaluator for the shared surface runtime cache and
//! deferred-family working-set core.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::Arc;

use magician::magician_v2::agents::{FeatureMode, InvocationSurface};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use magician::magician_v2::execution::agentic::native_types::NativeExecutionTool;
use magician::magician_v2::execution::agentic::AgenticContext;
use magician::magician_v2::execution::flat_loop::{
    build_tool_index, project_family_selection, selected_tool_names_from_query,
    static_prompt_context_revision, DeferredEntry, EffectiveSurfacePlan, FamilyLoadError,
    PreparedFamilyLoadCommitError, StaticPromptKey, SurfacePlanCache, SurfacePlanKey,
    SurfaceWorkingSetKey, SurfaceWorkingSetStore, ToolWorkingSet, WorkingSetLimits,
};
use magician::magician_v2::execution::{
    embedded_compiled_pack_defs, ExecutionConfig, MagicutorClient, ScopedCapabilityResolver,
};
use magicllm::config::RealtimeVoiceProfile;
use magicllm::realtime::RealtimeAudioControl;
use runtime_core::{FileSandboxConfig, ShellSandboxConfig};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct EvalExport {
    schema_version: u32,
    generated_by: &'static str,
    gate: &'static str,
    scenario_count: usize,
    passed_count: usize,
    scenarios: Vec<EvalScenario>,
}

#[derive(Debug, Serialize)]
struct EvalScenario {
    name: String,
    passed: bool,
    assertions: usize,
    details: serde_json::Value,
}

fn scenario(
    name: impl Into<String>,
    passed: bool,
    assertions: usize,
    details: serde_json::Value,
) -> EvalScenario {
    EvalScenario {
        name: name.into(),
        passed,
        assertions,
        details,
    }
}

fn cache_resolver(root: &Path) -> ScopedCapabilityResolver {
    let workspace = ArtifactV2Workspace::new(root.join("runtime"));
    let manager = Arc::new(CapabilityWorkspaceManager::new(workspace, root));
    let magicutor =
        Arc::new(MagicutorClient::new(ExecutionConfig::default()).expect("eval Magicutor client"));
    ScopedCapabilityResolver::new(
        manager,
        magicutor,
        FileSandboxConfig::default(),
        ShellSandboxConfig::default(),
        None,
        None,
        None,
    )
}

fn main() {
    let index = build_tool_index(&embedded_compiled_pack_defs());
    let all_names = index
        .pack_names()
        .into_iter()
        .flat_map(|pack| index.leaf_names_for_pack(&pack))
        .collect::<HashSet<_>>();
    let mut scenarios = Vec::new();

    let mut parity_failures = Vec::new();
    let mut parity_assertions = 0usize;
    for selected in &all_names {
        let result = project_family_selection(
            &index,
            std::slice::from_ref(selected),
            &all_names,
            WorkingSetLimits::default(),
        );
        parity_assertions += 1;
        let pack_name = index.get(selected).map(|entry| entry.pack_name.clone());
        let pack_leaves = pack_name
            .as_deref()
            .map(|pack| index.leaf_names_for_pack(pack))
            .unwrap_or_default();
        match result {
            // Selecting one leaf loads that pack's authorized leaves, not a
            // single schema and not a sibling pack.
            Ok(projection)
                if projection.loaded_tools.iter().any(|tool| tool == selected)
                    && projection
                        .loaded_tools
                        .iter()
                        .all(|tool| pack_leaves.iter().any(|leaf| leaf == tool)) => {}
            Ok(projection) => parity_failures.push(serde_json::json!({
                "selected": selected,
                "expected_pack_leaves": pack_leaves.len(),
                "actual_count": projection.loaded_tools.len(),
            })),
            Err(error) => parity_failures.push(serde_json::json!({
                "selected": selected,
                "error": error,
            })),
        }
    }
    scenarios.push(scenario(
        "all_embedded_leaves_load_only_the_matched_schema",
        parity_failures.is_empty(),
        parity_assertions,
        serde_json::json!({
            "leaf_count": all_names.len(),
            "pack_count": index.pack_names().len(),
            "failures": parity_failures,
        }),
    ));

    let multi_family = index
        .pack_names()
        .into_iter()
        .find(|pack| index.leaf_names_for_pack(pack).len() >= 3)
        .expect("embedded catalog should contain a multi-leaf family");
    let family_leaves = index.leaf_names_for_pack(&multi_family);
    let narrowed = family_leaves
        .iter()
        .enumerate()
        .filter(|(index, _)| index % 2 == 0)
        .map(|(_, name)| name.clone())
        .collect::<HashSet<_>>();
    let selected = narrowed.iter().next().expect("narrowed selection").clone();
    let narrowed_projection = project_family_selection(
        &index,
        std::slice::from_ref(&selected),
        &narrowed,
        WorkingSetLimits::default(),
    )
    .expect("narrowed family projection");
    let projected = narrowed_projection
        .loaded_tools
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    scenarios.push(scenario(
        "matched_loading_intersects_authorization_ceiling",
        projected.contains(&selected) && projected.is_subset(&narrowed),
        family_leaves.len(),
        serde_json::json!({
            "family": multi_family,
            "family_size": family_leaves.len(),
            "authorized_count": narrowed.len(),
            "loaded_count": projected.len(),
        }),
    ));

    let denied_projection = project_family_selection(
        &index,
        &[selected.clone(), "definitely_missing_tool".to_string()],
        &HashSet::new(),
        WorkingSetLimits::default(),
    )
    .expect("denied selection returns an opaque empty projection");
    let denied_json = serde_json::to_string(&denied_projection).expect("denied projection JSON");
    scenarios.push(scenario(
        "unknown_and_denied_selection_is_opaque_and_non_mutating",
        denied_projection.accepted_selected_count == 0
            && denied_projection.unavailable_selected_count == 2
            && denied_projection.loaded_tools.is_empty()
            && !denied_json.contains(&selected)
            && !denied_json.contains("definitely_missing_tool"),
        5,
        serde_json::json!({
            "accepted": denied_projection.accepted_selected_count,
            "unavailable": denied_projection.unavailable_selected_count,
            "response_bytes": denied_json.len(),
        }),
    ));

    let mut pack_iter = index
        .pack_names()
        .into_iter()
        .filter_map(|pack| index.leaf_names_for_pack(&pack).into_iter().next());
    let first = pack_iter.next().expect("first family");
    let second = pack_iter.next().expect("second family");
    let third = pack_iter.next().expect("third family");
    let limit_result = project_family_selection(
        &index,
        &[first, second, third],
        &all_names,
        WorkingSetLimits::default(),
    );
    scenarios.push(scenario(
        "default_multi_family_limit_is_atomic",
        matches!(
            limit_result,
            Err(FamilyLoadError::TooManyFamilies {
                requested: 3,
                limit: 2
            })
        ),
        1,
        serde_json::json!({ "result": limit_result }),
    ));

    let mut state = ToolWorkingSet::new("snapshot-a");
    let first_changed = state.replace(&narrowed_projection);
    let repeat_changed = state.replace(&narrowed_projection);
    let generation_before_revision = state.generation;
    let revision_changed = state.reconcile("snapshot-b", &narrowed);
    scenarios.push(scenario(
        "working_set_generation_and_policy_drift_are_monotonic",
        first_changed
            && !repeat_changed
            && generation_before_revision == 1
            && revision_changed
            && state.generation == 2
            && state.loaded_tools.is_empty(),
        6,
        serde_json::json!({
            "generation": state.generation,
            "base_policy_snapshot_id": state.base_policy_snapshot_id,
            "loaded_count": state.loaded_tools.len(),
        }),
    ));

    let surface_store = SurfaceWorkingSetStore::new(8);
    let chat_key = SurfaceWorkingSetKey::new(
        "owner",
        "default",
        "presto",
        InvocationSurface::Chat,
        FeatureMode::None,
        "shared-binding",
    );
    let voice_key = SurfaceWorkingSetKey::new(
        "owner",
        "default",
        "presto",
        InvocationSurface::RealtimeVoice,
        FeatureMode::None,
        "shared-binding",
    );
    let tutor_key = SurfaceWorkingSetKey::new(
        "owner",
        "default",
        "presto",
        InvocationSurface::Chat,
        FeatureMode::Tutor,
        "shared-binding",
    );
    let (_, chat_loaded) = surface_store
        .select(
            &chat_key,
            "authority-a",
            &index,
            std::slice::from_ref(&selected),
            &narrowed,
            WorkingSetLimits::default(),
        )
        .expect("chat working-set selection");
    let voice_empty = surface_store.reconcile(&voice_key, "authority-a", &narrowed);
    let tutor_empty = surface_store.reconcile(&tutor_key, "authority-a", &narrowed);
    let revision_cleared = surface_store.reconcile(&chat_key, "authority-b", &narrowed);
    scenarios.push(scenario(
        "surface_working_sets_are_isolated_and_revision_safe",
        !chat_loaded.loaded_tools.is_empty()
            && voice_empty.loaded_tools.is_empty()
            && tutor_empty.loaded_tools.is_empty()
            && revision_cleared.loaded_tools.is_empty()
            && revision_cleared.generation > chat_loaded.generation,
        5,
        serde_json::json!({
            "chat_generation_before_revision": chat_loaded.generation,
            "chat_generation_after_revision": revision_cleared.generation,
            "voice_loaded_count": voice_empty.loaded_tools.len(),
            "tutor_loaded_count": tutor_empty.loaded_tools.len(),
            "store": surface_store.status(),
        }),
    ));

    let voice_transition_store = SurfaceWorkingSetStore::new(8);
    let selected_query = format!("SeLeCt:{selected},{selected}");
    let parsed_selection = selected_tool_names_from_query(&selected_query)
        .expect("select query should parse and deduplicate");
    let prepared = voice_transition_store
        .prepare_select(
            &voice_key,
            "authority-a",
            &index,
            &parsed_selection,
            &narrowed,
            WorkingSetLimits::default(),
        )
        .expect("prepare realtime family");
    let before_ack = voice_transition_store.reconcile(&voice_key, "authority-a", &narrowed);
    let committed = voice_transition_store
        .commit_prepared(&voice_key, &prepared)
        .expect("acknowledged realtime family");
    let stale = voice_transition_store
        .prepare_select(
            &voice_key,
            "authority-a",
            &index,
            &parsed_selection,
            &narrowed,
            WorkingSetLimits::default(),
        )
        .expect("prepare stale transition");
    voice_transition_store.reconcile(&voice_key, "authority-b", &narrowed);
    let stale_commit = voice_transition_store.commit_prepared(&voice_key, &stale);
    scenarios.push(scenario(
        "realtime_catalog_transition_is_prepare_ack_commit_and_stale_safe",
        parsed_selection.len() == 1
            && before_ack.loaded_tools.is_empty()
            && !committed.loaded_tools.is_empty()
            && matches!(
                stale_commit,
                Err(PreparedFamilyLoadCommitError::PolicyChanged { .. })
                    | Err(PreparedFamilyLoadCommitError::GenerationChanged { .. })
            ),
        4,
        serde_json::json!({
            "parsed_selection_count": parsed_selection.len(),
            "before_ack_loaded_count": before_ack.loaded_tools.len(),
            "after_ack_loaded_count": committed.loaded_tools.len(),
            "stale_commit": stale_commit,
        }),
    ));

    let mut ordered_names = all_names.iter().cloned().collect::<Vec<_>>();
    ordered_names.sort();
    let full_tools = ordered_names
        .iter()
        .filter_map(|name| index.get(name))
        .map(|entry| NativeExecutionTool {
            name: entry.name.clone(),
            description: entry.description.clone(),
            parameters: entry.parameters_schema.clone(),
            is_control_tool: false,
        })
        .collect::<Vec<_>>();
    let hot_count = full_tools.len().min(10);
    let initial_hot = full_tools[..hot_count].to_vec();
    let deferred = full_tools[hot_count..]
        .iter()
        .map(|tool| DeferredEntry {
            name: tool.name.clone(),
            group: "pack",
            search_hint: None,
        })
        .collect::<Vec<_>>();
    let authorized_business = ordered_names.iter().cloned().collect::<BTreeSet<_>>();
    let candidate_plan = EffectiveSurfacePlan {
        authority_revision: "authority-production-index".to_string(),
        registry_revision: "registry-production-index".to_string(),
        invocation_surface: InvocationSurface::Chat,
        feature_mode: FeatureMode::None,
        initial_hot,
        loaded_tools: Vec::new(),
        deferred,
        runtime_tool_names: BTreeSet::new(),
        structural_tool_names: BTreeSet::new(),
        authorized_business_tool_names: authorized_business,
        provider_schema_bytes: magician::magician_v2::execution::flat_loop::provider_schema_bytes(
            &full_tools[..hot_count],
        ),
        built_at_ms: 0,
    };
    let plan_parity = candidate_plan.parity_report();
    let full_schema_bytes =
        magician::magician_v2::execution::flat_loop::provider_schema_bytes(&full_tools);
    scenarios.push(scenario(
        "production_index_plan_preserves_universe_and_reduces_eager_schema",
        plan_parity.is_exact()
            && candidate_plan.provider_schema_bytes < full_schema_bytes
            && candidate_plan.provider_tools().len() == hot_count,
        3,
        serde_json::json!({
            "full_tool_count": full_tools.len(),
            "hot_tool_count": hot_count,
            "deferred_tool_count": candidate_plan.deferred.len(),
            "full_schema_bytes": full_schema_bytes,
            "candidate_schema_bytes": candidate_plan.provider_schema_bytes,
            "schema_savings_bytes": full_schema_bytes.saturating_sub(candidate_plan.provider_schema_bytes),
            "parity": plan_parity,
        }),
    ));

    let plan_cache = SurfacePlanCache::new(4);
    let chat_plan_key = SurfacePlanKey {
        principal: "owner".to_string(),
        workspace: "default".to_string(),
        agent_id: "presto".to_string(),
        surface: InvocationSurface::Chat,
        feature_mode: FeatureMode::None,
        authority_revision: "authority-production-index".to_string(),
    };
    let cached_plan = plan_cache.insert(chat_plan_key.clone(), candidate_plan.clone());
    let reused_plan = plan_cache.get(&chat_plan_key).expect("cached plan");
    let voice_plan_key = SurfacePlanKey {
        surface: InvocationSurface::RealtimeVoice,
        ..chat_plan_key.clone()
    };
    let feature_plan_key = SurfacePlanKey {
        feature_mode: FeatureMode::Tutor,
        ..chat_plan_key.clone()
    };
    scenarios.push(scenario(
        "surface_plan_cache_reuses_exact_keys_without_cross_surface_leakage",
        Arc::ptr_eq(&cached_plan, &reused_plan)
            && plan_cache.get(&voice_plan_key).is_none()
            && plan_cache.get(&feature_plan_key).is_none()
            && plan_cache.status().entry_count == 1,
        4,
        serde_json::json!({ "cache": plan_cache.status() }),
    ));

    let stable_variables = std::collections::HashMap::from([
        ("persona".to_string(), "Presto".to_string()),
        ("tools".to_string(), "search_memory, list_tasks".to_string()),
    ]);
    let prefix_revision =
        static_prompt_context_revision("chat_outer_loop_system", "v1", &stable_variables);
    let static_key = StaticPromptKey {
        principal: "owner".to_string(),
        workspace: "default".to_string(),
        agent_id: "presto".to_string(),
        surface: InvocationSurface::Chat,
        feature_mode: FeatureMode::None,
        prompt_name: "chat_outer_loop_system".to_string(),
        context_revision: prefix_revision.clone(),
    };
    let inserted_prefix = plan_cache.insert_static_prompt(
        static_key.clone(),
        "stable persona + policy + tool prefix".to_string(),
    );
    let reused_prefix = plan_cache
        .get_static_prompt(&static_key)
        .expect("static prefix cached");
    let mut changed_variables = stable_variables.clone();
    changed_variables.insert("tools".to_string(), "search_memory".to_string());
    let changed_revision =
        static_prompt_context_revision("chat_outer_loop_system", "v1", &changed_variables);
    scenarios.push(scenario(
        "static_prompt_prefix_is_byte_stable_scoped_and_revision_invalidated",
        Arc::ptr_eq(&inserted_prefix, &reused_prefix)
            && prefix_revision != changed_revision
            && plan_cache.status().static_prompt_hits == 1
            && plan_cache.invalidate_agent("owner", "default", "presto") >= 2
            && plan_cache.get_static_prompt(&static_key).is_none(),
        5,
        serde_json::json!({
            "prefix_bytes": reused_prefix.len(),
            "prefix_revision": prefix_revision,
            "changed_revision": changed_revision,
            "cache": plan_cache.status(),
        }),
    ));

    let task_store = SurfaceWorkingSetStore::new(8);
    let mut task_family_samples = index
        .pack_names()
        .into_iter()
        .filter_map(|pack| index.leaf_names_for_pack(&pack).into_iter().next());
    let task_first = task_family_samples.next().expect("first task family");
    let task_second = task_family_samples.next().expect("second task family");
    let task_selection = vec![task_first, task_second];
    let task_key = SurfaceWorkingSetKey::new(
        "owner",
        "default",
        "presto",
        InvocationSurface::Task,
        FeatureMode::None,
        "execution-7",
    );
    let (_, task_loaded) = task_store
        .select(
            &task_key,
            "task-authority-a",
            &index,
            &task_selection,
            &all_names,
            WorkingSetLimits::autonomous_compatibility(task_selection.len()),
        )
        .expect("legacy-compatible task selection");
    let handover_key = SurfaceWorkingSetKey::new(
        "owner",
        "default",
        "researcher",
        InvocationSurface::Handover,
        FeatureMode::None,
        "execution-7",
    );
    let handover_empty = task_store.reconcile(&handover_key, "task-authority-b", &all_names);
    let old_owner_removed = task_store.remove(&task_key);
    let provider_projection = candidate_plan.provider_tools();
    scenarios.push(scenario(
        "autonomous_task_shared_projection_preserves_catalog_family_and_owner_boundaries",
        !task_loaded.loaded_tools.is_empty()
            && task_loaded.loaded_families.len() == 2
            && handover_empty.loaded_tools.is_empty()
            && old_owner_removed
            && provider_projection.len() == hot_count
            && magician::magician_v2::execution::flat_loop::provider_schema_bytes(
                &provider_projection,
            ) == candidate_plan.provider_schema_bytes,
        6,
        serde_json::json!({
            "selected_family_count": task_loaded.loaded_families.len(),
            "loaded_tool_count": task_loaded.loaded_tools.len(),
            "handover_loaded_tool_count": handover_empty.loaded_tools.len(),
            "old_owner_removed": old_owner_removed,
            "provider_tool_count": provider_projection.len(),
            "provider_schema_bytes": candidate_plan.provider_schema_bytes,
            "store": task_store.status(),
        }),
    ));

    let mut task_context = AgenticContext::new("Investigate latency", "Evidence-backed answer");
    task_context.mark_task_prompt_context_seeded();
    let ordinary_iteration_generation = task_context.task_prompt_context_generation;
    let ordinary_iteration_hydrated = task_context.task_prompt_context_hydrated_generation;
    let checkpoints = [
        "resume:user_input",
        "operator_steer:focus on provider latency",
        "step_completed:measure",
        "step_failed:verify",
        "owner_transition:researcher",
        "owner_policy_revision",
    ];
    for checkpoint in checkpoints {
        task_context.advance_task_prompt_context_checkpoint(checkpoint);
        task_context.mark_task_prompt_context_seeded();
    }
    scenarios.push(scenario(
        "task_semantic_retrieval_uses_explicit_checkpoints_not_internal_iterations",
        ordinary_iteration_generation == 0
            && ordinary_iteration_hydrated == Some(0)
            && task_context.task_prompt_context_generation == checkpoints.len() as u64
            && task_context.task_prompt_context_hydrated_generation
                == Some(task_context.task_prompt_context_generation)
            && task_context.task_prompt_context_checkpoint_hint.as_deref()
                == Some("owner_policy_revision"),
        5,
        serde_json::json!({
            "ordinary_iteration_generation": ordinary_iteration_generation,
            "checkpoint_count": checkpoints.len(),
            "final_generation": task_context.task_prompt_context_generation,
            "checkpoint_types": checkpoints,
        }),
    ));

    let temp = tempfile::tempdir().expect("cache eval tempdir");
    let resolver = cache_resolver(temp.path());
    let first_snapshot = resolver
        .capability_snapshot_for_scope("owner", "default")
        .expect("cold cache snapshot");
    let cached_snapshot = resolver
        .capability_snapshot_for_scope("owner", "default")
        .expect("warm cache snapshot");
    let refreshed_snapshot = resolver
        .refresh_scope("owner", "default")
        .expect("manual refresh");
    let isolated_snapshot = resolver
        .capability_snapshot_for_scope("guest", "default")
        .expect("isolated scope snapshot");
    let metrics = resolver.cache_status();
    scenarios.push(scenario(
        "scoped_cache_reuses_refreshes_and_isolates",
        Arc::ptr_eq(&first_snapshot, &cached_snapshot)
            && !Arc::ptr_eq(&first_snapshot, &refreshed_snapshot)
            && first_snapshot.revision == refreshed_snapshot.revision
            && first_snapshot.revision != isolated_snapshot.revision
            && metrics.builds == 3
            && metrics.entry_count == 2
            && metrics.invalidations == 1,
        7,
        serde_json::json!({
            "first_revision": first_snapshot.revision,
            "refreshed_revision": refreshed_snapshot.revision,
            "isolated_revision": isolated_snapshot.revision,
            "cache": metrics,
        }),
    ));

    let configure = serde_json::to_value(RealtimeAudioControl::ConfigureSession {
        instructions: "Stable session prefix".to_string(),
        tools: Vec::new(),
        input_transcription_model: None,
        update_id: None,
        defer_response_until_context: true,
    })
    .expect("serialize deferred realtime configuration");
    let respond = serde_json::to_value(RealtimeAudioControl::RespondWithTurnContext {
        context_item_id: "voice-context-generation-7".to_string(),
        context: Some("Bounded current-utterance context".to_string()),
    })
    .expect("serialize realtime context response");
    scenarios.push(scenario(
        "realtime_turn_context_protocol_is_explicit_bounded_and_response_scoped",
        configure["type"] == "configure_session"
            && configure["defer_response_until_context"] == true
            && respond["type"] == "respond_with_turn_context"
            && respond["context_item_id"] == "voice-context-generation-7"
            && respond["context"] == "Bounded current-utterance context"
            && respond.get("instructions").is_none()
            && respond.get("tools").is_none(),
        7,
        serde_json::json!({
            "configure_control": configure,
            "response_control": respond,
            "context_lifetime": "response_scoped_replaceable_item",
        }),
    ));

    let realtime_profile = |provider: &str| RealtimeVoiceProfile {
        provider: provider.to_string(),
        model: "eval".to_string(),
        display_name: None,
        selectable: false,
        mode: magicllm::config::RealtimeVoiceMode::Assistant,
        allow_without_turn_grounding: false,
        voice: None,
        max_session_duration_secs: None,
        compaction_token_watermark: None,
        base_url: None,
        fallback: Vec::new(),
        transcription_model: None,
        transcription_fallback_model: None,
        turn_detection_mode: None,
        context_window_tokens: None,
        verbatim_recent_turns: None,
        compaction_input_turn_limit: None,
        translation_target_language: None,
        translation_echo_target_language: false,
        thinking_level: None,
        tool_result_scheduling: None,
        display_order: None,
    };
    scenarios.push(scenario(
        "voice_provider_admission_requires_finalized_turn_grounding",
        magician_media::voice_orchestrator::profile_supports_mandatory_turn_grounding(
            &realtime_profile("openai_realtime"),
        ) && magician_media::voice_orchestrator::profile_supports_mandatory_turn_grounding(
            &realtime_profile("openai_realtime_backend"),
        ) && !magician_media::voice_orchestrator::profile_supports_mandatory_turn_grounding(
            &realtime_profile("gemini_live"),
        ),
        3,
        serde_json::json!({
            "native_grounded": ["openai_realtime", "openai_realtime_backend"],
            "requires_grounded_fallback": ["gemini_live"],
            "hands_free_semantics": "normal Chat retrieval path",
        }),
    ));

    let passed_count = scenarios.iter().filter(|entry| entry.passed).count();
    let gate = if passed_count == scenarios.len() {
        "PASS"
    } else {
        "FAIL"
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&EvalExport {
            schema_version: 1,
            generated_by: "magician::agent_surface_runtime_cache_eval",
            gate,
            scenario_count: scenarios.len(),
            passed_count,
            scenarios,
        })
        .expect("eval export JSON")
    );
}
