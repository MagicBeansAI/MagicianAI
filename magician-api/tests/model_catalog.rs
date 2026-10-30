//! Provider-free public contracts, linked against the production library.

use magician::config::{
    dry_run_magician_config_from_path, validate_magician_config_dry_run, MagicianConfig,
};
use magician::magician_v2::{
    llm_chunking::validate_router_chunking_config,
    query_analysis::{
        multi_llm_service::MultiLLMService, operation_llm_router::OperationLlmRouter,
    },
    vibedev::run_service::VibeDevCodingCatalog,
};
use magicllm::{
    catalog::CATALOG_SETTINGS_KEY,
    config::{LLMProfile, LLMRouterConfig},
};
use serde_json::json;

fn shipped() -> MagicianConfig {
    dry_run_magician_config_from_path(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician-config.yaml"),
    )
    .expect("shipped config must satisfy all owned invariants")
}

#[test]
fn shipped_catalog_preserves_app_trust_and_logical_context_bindings() {
    magician_chunking::register_builtin_chunk_adapters().expect("production boot adapters");
    let config = shipped();
    validate_router_chunking_config(config.router_config().expect("router"))
        .expect("migration must preserve adapter-bound selector identities");
}

#[test]
fn operation_catalog_publishes_concrete_assignment_handles() {
    let config = shipped();
    let router = config.router_config().unwrap();
    let overview = magician_api::llm_routing_api::routing_overview(
        router,
        &magician_api::llm_routing_api::RoutingState::default(),
    );
    let models = overview["models"].as_array().expect("model roster");
    assert!(models.len() > 20);
    for model in models {
        for route in model["operation_choices"]
            .as_object()
            .expect("assignment choices")
            .values()
        {
            assert!(
                router.profiles.contains_key(route.as_str().unwrap()),
                "operation assignments must name concrete routes"
            );
        }
    }
}

#[test]
fn chat_catalog_publishes_one_model_and_resolves_every_native_choice() {
    let config = shipped();
    let service = MultiLLMService::from_router_config(config.router_config().unwrap());
    let profiles = service.list_chat_profiles();
    let public_catalog = magician_api::chat_api::chat_model_catalog(&profiles, None);
    assert!(
        public_catalog.get("profiles").is_none(),
        "typed catalog has no legacy selector rows"
    );
    assert!(public_catalog["models"].as_array().unwrap().len() > 10);
    assert_eq!(public_catalog["default_selection"]["thinking"], "auto");
    let choices = profiles
        .iter()
        .filter_map(|profile| profile.model_choice.as_ref())
        .collect::<Vec<_>>();
    assert!(choices.len() > 10);
    let ids = choices
        .iter()
        .map(|choice| &choice.id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(choices.len(), ids.len());
    for choice in choices {
        let encoded = serde_json::to_value(choice).unwrap();
        assert!(
            matches!(
                encoded["feedback"].as_str(),
                Some("fast" | "standard" | "premium")
            ),
            "every provider must publish catalog feedback: {}",
            choice.id
        );
        for (thinking, route) in &choice.choices {
            let selected = profiles
                .iter()
                .find(|profile| &profile.name == route)
                .unwrap_or_else(|| panic!("missing chat choice {route}"))
                .model_selection
                .as_ref()
                .expect("choice metadata");
            assert_eq!(selected.model_id, choice.id);
            assert_eq!(&selected.thinking, thinking);
        }
    }
    assert!(profiles
        .iter()
        .find(|profile| profile.is_default)
        .expect("default")
        .model_selection
        .is_some());
    assert!(profiles
        .iter()
        .any(|profile| profile.name == "model/openai/gpt-6-luna/auto"
            && profile.model_selection.is_some()));
}

#[test]
fn public_coding_catalog_and_typed_requests_need_no_authored_selector_rows() {
    use magician::magician_v2::execution::coding_engine::discovery::{
        self, AgyReadinessSnapshot, ClaudeReadinessSnapshot, CodexReadinessSnapshot,
        GrokReadinessSnapshot,
    };
    use magician::magician_v2::vibedev::run_service::ClientVibeDevCodingChoice;

    let config = shipped();
    let mut rows = discovery::project_coding_profiles(
        &config.coding_profile_catalog(),
        &CodexReadinessSnapshot::default(),
        &GrokReadinessSnapshot::default(),
        &ClaudeReadinessSnapshot::default(),
        &AgyReadinessSnapshot::default(),
    );
    magician::magician_v2::execution::coding_engine::catalog::project(
        config.router_config(),
        &mut rows,
    );
    let catalog = magician_api::coding_model_selection::model_catalog(&rows);
    assert!(catalog.get("profiles").is_none());
    assert!(catalog.get("aliases").is_none());
    let models = catalog["models"].as_array().unwrap();
    assert!(!models.is_empty());
    let authority = VibeDevCodingCatalog::from_config(&config);
    for model in models {
        assert!(model["limits"].is_null());
        for (thinking, route) in model["choices"].as_object().unwrap() {
            let choice = magician_api::coding_model_selection::parse_choice(json!({
                "kind":"model", "engine":model["engine"], "model_id":model["id"], "thinking":thinking,
            })).unwrap();
            let ClientVibeDevCodingChoice::Profile { profile_id } = choice else {
                panic!("explicit models stay fixed")
            };
            assert_eq!(profile_id, route.as_str().unwrap());
            assert!(authority
                .entries
                .iter()
                .any(|entry| entry.eligible && entry.definition.id() == profile_id));
        }
    }
    let engines = catalog["engines"].as_array().unwrap();
    for engine in ["codex_app_server", "claude_code", "grok_acp", "agy_cli"] {
        assert_eq!(
            engines.iter().find(|row| row["id"] == engine).unwrap()["selectable"],
            false,
            "no readiness receipt may be invented for {engine}"
        );
    }
}

#[test]
fn coding_catalog_pins_every_native_choice_without_persisting_generated_profiles() {
    let config = shipped();
    let router = config.router_config().unwrap();
    let rows = config.coding_profile_catalog();
    let models = rows
        .iter()
        .filter_map(|row| row.model_choice.as_ref())
        .collect::<Vec<_>>();
    assert!(!models.is_empty());
    assert_eq!(
        models.len(),
        router
            .catalog
            .models
            .values()
            .filter(|model| model.coding)
            .count()
    );
    let authority = VibeDevCodingCatalog::from_coding_settings(&config.coding);
    for model in models {
        assert!(
            model.limits.is_none(),
            "Pi's admitted context window is SDK-owned"
        );
        for (thinking, selector) in &model.choices {
            let resolved = config
                .resolve_coding_profile(Some(selector))
                .unwrap()
                .unwrap();
            assert_eq!(resolved.model, model.model);
            assert!(authority
                .entries
                .iter()
                .any(|entry| entry.definition.id() == selector.as_str()));
            let selected = rows
                .iter()
                .find(|row| &row.id == selector)
                .unwrap()
                .model_selection
                .as_ref()
                .unwrap();
            assert_eq!(&selected.thinking, thinking);
            match thinking.as_str() {
                "none" => assert_eq!(resolved.thinking_level.as_deref(), Some("off")),
                "on" => assert_eq!(resolved.thinking_level.as_deref(), Some("high")),
                "max" => assert_eq!(resolved.thinking_level.as_deref(), Some("max")),
                _ => {},
            }
        }
    }
    assert_eq!(
        config
            .resolve_coding_profile(None)
            .unwrap()
            .unwrap()
            .thinking_level
            .as_deref(),
        Some("medium")
    );
    assert!(config
        .resolve_coding_profile(Some("pi/model/unknown/auto"))
        .is_err());
    let persisted = serde_json::to_value(&config.coding).unwrap();
    assert!(persisted.get("profiles").is_none());
    let revalidated = validate_magician_config_dry_run(config.clone()).unwrap();
    assert_eq!(
        revalidated.coding.profiles.len(),
        config.coding.profiles.len()
    );
}

#[test]
fn bounded_catalog_metadata_allows_the_reviewed_transport_and_refuses_payload_knobs() {
    let mut profile: LLMProfile = serde_json::from_value(json!({
        "provider":"openai", "model":"gpt-4o-mini", "context_window_tokens":128000,
        "max_output_tokens":1024, "timeout_secs":5, "supports_tool_calling":true
    }))
    .unwrap();
    for (key, value, allowed) in [
        (CATALOG_SETTINGS_KEY, json!({"local_only":true}), true),
        ("openai_api_mode", json!("responses"), true),
        ("openai_api_mode", json!("chat"), false),
        ("tool_choice", json!("auto"), true),
        ("tool_choice", json!("any"), false),
        ("candidate_count", json!(2), false),
    ] {
        profile.metadata = Some(std::collections::HashMap::from([
            ("model_family".to_string(), json!("gpt")),
            (key.to_string(), value),
        ]));
        let mut router = LLMRouterConfig::default();
        router.profiles.insert("bounded".into(), profile.clone());
        assert_eq!(
            OperationLlmRouter::new(Some(router))
                .bounded_route("bounded")
                .is_ok(),
            allowed,
            "{key}"
        );
    }
}

#[test]
fn coding_image_admission_follows_capabilities_for_catalog_and_saved_choices() {
    let mut config = shipped();
    // The current qualified roster is entirely multimodal. Add an explicitly
    // narrower test model to exercise text-only admission without changing the
    // capabilities shipped to users or assuming a particular provider roster.
    let router = config.llm.router.as_mut().unwrap();
    let mut text_only_model = router.catalog.models["openai/gpt-6-luna"].clone();
    text_only_model.capabilities.vision = false;
    text_only_model.label = "Text-only fixture".into();
    router
        .catalog
        .models
        .insert("openai/text-only-fixture".into(), text_only_model);
    router.profiles.clear();
    router.catalog.clone().materialize(router).unwrap();
    let config = validate_magician_config_dry_run(config).unwrap();
    let rows = config.coding_profile_catalog();
    let mut vision = 0;
    let mut text_only = 0;
    for row in rows {
        let accepted =
            magician_api::vibedev_api::coding_profile_accepts_images(&config, Some(&row.id));
        assert_eq!(accepted, row.supports_user_image_inputs, "{}", row.id);
        if row.id.starts_with("pi/model/") {
            if accepted {
                vision += 1
            } else {
                text_only += 1
            }
        }
    }
    assert!(vision > 0 && text_only > 0);
    assert!(magician_api::vibedev_api::coding_profile_accepts_images(
        &config, None
    ));
    assert!(!magician_api::vibedev_api::coding_profile_accepts_images(
        &config,
        Some("pi/model/unknown/auto")
    ));
    assert!(!magician_api::vibedev_api::coding_profile_accepts_images(
        &config,
        Some("codex-default")
    ));
}

#[test]
fn native_coding_catalog_carries_models_and_effort_into_sealed_definitions() {
    use magician::magician_v2::execution::coding_engine::{
        catalog,
        selection::{CodingProfileDefinition, ProfileCatalogEntry},
    };
    let config = shipped();
    let base = vec![
        CodingProfileDefinition::synthesized_codex_default("gpt-6.1-sol", "high").unwrap(),
        CodingProfileDefinition::synthesized_claude_default().unwrap(),
        CodingProfileDefinition::synthesized_grok_default("grok-build").unwrap(),
        CodingProfileDefinition::synthesized_agy_default().unwrap(),
    ]
    .into_iter()
    .map(|definition| ProfileCatalogEntry::new(definition, true).unwrap())
    .collect::<Vec<_>>();
    let models = catalog::models(config.router_config());
    assert!(models.len() >= 15);
    let entries = catalog::entries(config.router_config(), &base);
    for model in &models {
        for (level, id) in &model.choice.choices {
            let entry = entries
                .iter()
                .find(|entry| entry.definition.id() == id)
                .expect("every visible choice is admissible when its engine is eligible");
            let expected = if level == "auto" {
                if model.engine == "codex_app_server" && model.choice.model == "default" {
                    Some("high")
                } else {
                    model.choice.thinking.default.as_deref()
                }
            } else {
                Some(level.as_str())
            };
            assert_eq!(entry.definition.reasoning_effort(), expected, "{id}");
            if model.choice.model != "default" {
                assert_eq!(entry.definition.model(), Some(model.choice.model.as_str()));
            }
            assert!(model.choice.limits.is_none());
        }
    }
    assert!(
        catalog::entries(config.router_config(), &[]).is_empty(),
        "catalog metadata never grants an unqualified engine eligibility"
    );
    let one = &entries[0].definition;
    let changed = one.clone().with_model_settings(
        one.id().to_string(),
        one.model().unwrap().to_string(),
        Some("low".into()),
    );
    let high = one.clone().with_model_settings(
        one.id().to_string(),
        one.model().unwrap().to_string(),
        Some("high".into()),
    );
    assert_ne!(changed.definition_digest(), high.definition_digest());
    let mut rows = base
        .iter()
        .map(|entry| json!({"id": entry.definition.id(), "selectable": true}))
        .collect::<Vec<_>>();
    catalog::project(config.router_config(), &mut rows);
    assert_eq!(
        rows.iter()
            .filter(|row| row["model_choice"].is_object())
            .count(),
        models.len()
    );
}

#[test]
fn native_coding_argv_keeps_model_and_effort_separate_and_rejects_invalid_values() {
    use magician::magician_v2::execution::coding_engine::factory::native_model_args;
    let args = native_model_args(Some("sonnet"), Some("high"), "--effort").unwrap();
    assert_eq!(
        args,
        ["--model", "sonnet", "--effort", "high"].map(std::ffi::OsString::from)
    );
    assert!(native_model_args(Some("default"), None, "--effort")
        .unwrap()
        .is_empty());
    assert!(native_model_args(Some("--other-option"), None, "--effort").is_err());
    assert!(native_model_args(Some("sonnet"), Some("made-up"), "--effort").is_err());
}
