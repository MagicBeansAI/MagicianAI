use super::*;

fn config() -> LLMRouterConfig {
    serde_yaml::from_str(
        r#"
catalog:
  connections:
    cloud: { provider: anthropic, api_key_env: EXAMPLE_KEY, max_output_tokens: 24000 }
    local: { provider: ollama, api_base_url: 'http://localhost:11434/api/chat' }
  models:
    modern:
      connection: cloud
      model: claude-sonnet-5-5
      label: Claude Sonnet
      limits: { context_window_tokens: 1000000, min_output_tokens: 16, max_output_tokens: 128000 }
      capabilities: { vision: true, tools: true, streaming: true }
      thinking: { mode: effort, levels: [low, medium, high], default: medium }
    legacy:
      connection: cloud
      model: claude-haiku-4-5
      label: Claude Haiku
      limits: { context_window_tokens: 200000, max_output_tokens: 64000 }
      capabilities: { tools: true }
      thinking:
        mode: budget
        levels: [none, low, medium, high]
        default: low
        budget: { min_tokens: 1024, max_tokens: 32000, step_tokens: 1024 }
    binary:
      connection: local
      model: qwen3
      label: Local Qwen
      limits: { context_window_tokens: 32768, max_output_tokens: 16384 }
      thinking: { mode: toggle, levels: [none, 'on'], default: none }
    fixed:
      connection: local
      model: plain
      label: Local plain
      limits: { context_window_tokens: 32768, max_output_tokens: 16384 }
  policies:
    background: { output_tokens: 4096, thinking: low, answer_reserve_tokens: 1024 }
  assignments:
    classify: { model: legacy, policy: background }
    private: { model: modern, thinking: low, vision: false }
    off_only: { model: binary, thinking: none, reasoning: false }
default_profile: model/modern/auto
"#,
    )
    .unwrap()
}

#[test]
fn assignment_fallback_is_explicit_and_does_not_need_an_alias() {
    let mut catalog = config().catalog;
    catalog.assignments.get_mut("private").unwrap().fallback = Some("classify".into());
    let router: LLMRouterConfig =
        serde_json::from_value(json!({"catalog":catalog,"default_profile":"private"})).unwrap();
    assert_eq!(
        router.profiles["private"].metadata.as_ref().unwrap()["fallback_profile"],
        "classify"
    );
    assert_eq!(router.profiles["classify"].model, "claude-haiku-4-5");
    assert!(
        router.profiles["model/modern/auto"]
            .metadata
            .as_ref()
            .unwrap()
            .get("fallback_profile")
            .is_none(),
        "an explicit model pin must not inherit an operation's fallback"
    );
    let serialized = serde_json::to_value(&router.catalog.assignments["private"]).unwrap();
    assert_eq!(serialized["fallback"], "classify");
    assert!(serialized["metadata"].get("fallback_profile").is_none());
}

#[test]
fn invalid_and_cyclic_assignment_fallbacks_fail_before_dispatch() {
    let mut catalog = config().catalog;
    catalog.assignments.get_mut("private").unwrap().fallback = Some("missing".into());
    assert!(catalog
        .validate()
        .unwrap_err()
        .to_string()
        .contains("unknown fallback assignment"));
    catalog.assignments.get_mut("private").unwrap().fallback = Some("private".into());
    assert!(catalog
        .validate()
        .unwrap_err()
        .to_string()
        .contains("cyclic"));
    catalog.assignments.get_mut("private").unwrap().fallback = Some("classify".into());
    catalog.assignments.get_mut("classify").unwrap().fallback = Some("private".into());
    assert!(catalog
        .validate()
        .unwrap_err()
        .to_string()
        .contains("cyclic"));
}

#[test]
fn catalog_refuses_legacy_fallback_metadata() {
    let mut catalog = config().catalog;
    catalog
        .assignments
        .get_mut("private")
        .unwrap()
        .metadata
        .insert("fallback_profile".into(), json!("classify"));
    assert!(catalog
        .validate()
        .unwrap_err()
        .to_string()
        .contains("assignment.fallback"));
}

#[test]
fn model_catalog_generates_controls_without_old_aliases() {
    let router = config();
    let choices = router.catalog.choices();
    assert_eq!(choices.len(), 4, "one choice per model, not one per level");
    let modern = choices.iter().find(|m| m.id == "modern").unwrap();
    assert_eq!(modern.thinking.levels, ["low", "medium", "high"]);
    assert!(!modern.choices.contains_key("none"));
    assert!(!router.profiles.contains_key("older-profile"));
    assert_eq!(
        router.profiles["model/modern/medium"].model,
        "claude-sonnet-5-5"
    );
    assert_eq!(
        router.profiles["model/modern/medium"]
            .reasoning
            .as_ref()
            .unwrap()
            .effort,
        "medium"
    );
    assert_eq!(router.profiles["private"].supports_vision, Some(false));
    assert!(router.resolve_profile("model/modern/auto").is_some());
    assert_eq!(
        router.profiles["model/modern/auto"]
            .reasoning
            .as_ref()
            .unwrap()
            .effort,
        "medium"
    );
    assert!(!router.profiles.contains_key("model/modern/auto/initial"));
}

#[test]
fn coding_qualification_requires_a_selectable_tool_model() {
    let mut catalog = config().catalog;
    catalog.models.get_mut("modern").unwrap().coding = true;
    catalog.validate().expect("qualified model");
    catalog.models.get_mut("modern").unwrap().capabilities.tools = false;
    assert!(catalog.validate().is_err());
    catalog.models.get_mut("modern").unwrap().capabilities.tools = true;
    catalog.models.get_mut("modern").unwrap().selectable = false;
    assert!(catalog.validate().is_err());
}

#[test]
fn automatic_background_budget_leaves_room_for_the_answer() {
    let router = config();
    let profile = &router.profiles["classify"];
    assert_eq!(profile.max_output_tokens, Some(4096));
    assert_eq!(profile.reasoning.as_ref().unwrap().effort, "low");
    assert_eq!(
        profile.reasoning.as_ref().unwrap().max_reasoning_tokens,
        Some(1024)
    );
}

#[test]
fn explicit_settings_are_validated_against_model_and_account_limits() {
    let router = config();
    let profile = &router.profiles["model/modern/medium"];
    let mut request = LLMRequest {
        model: profile.model.clone(),
        max_output_tokens: Some(20000),
        reasoning: Some(ReasoningConfig {
            effort: Some("low".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    resolve_request_settings(profile, &mut request)
        .expect("explicit cap can exceed the soft policy default");
    request.max_output_tokens = Some(24001);
    assert!(
        resolve_request_settings(profile, &mut request).is_err(),
        "account ceiling applies"
    );
    request.max_output_tokens = Some(4096);
    request.reasoning.as_mut().unwrap().effort = Some("none".into());
    assert!(
        resolve_request_settings(profile, &mut request).is_err(),
        "mandatory thinking cannot be disabled"
    );
    request.reasoning.as_mut().unwrap().effort = Some("low".into());
    request.model = "another-model".into();
    assert!(
        resolve_request_settings(profile, &mut request).is_err(),
        "limits cannot be carried to another model"
    );
}

#[test]
fn legacy_budgets_are_derived_from_the_actual_request_allowance() {
    let router = config();
    let profile = &router.profiles["model/legacy/medium"];
    let mut request = LLMRequest {
        model: profile.model.clone(),
        max_output_tokens: Some(4096),
        reasoning: Some(ReasoningConfig {
            effort: Some("medium".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    resolve_request_settings(profile, &mut request).unwrap();
    assert_eq!(
        request.reasoning.as_ref().unwrap().max_reasoning_tokens,
        Some(1024)
    );
    request.reasoning.as_mut().unwrap().max_reasoning_tokens = Some(4096);
    assert!(resolve_request_settings(profile, &mut request).is_err());
    request.reasoning.as_mut().unwrap().max_reasoning_tokens = None;
    request.max_output_tokens = Some(1024);
    assert!(
        resolve_request_settings(profile, &mut request).is_err(),
        "a budget too small to produce an answer fails before dispatch"
    );
}

#[test]
fn inherited_thinking_is_recomputed_for_a_smaller_bounded_call() {
    let router = config();
    let profile = &router.profiles["model/legacy/high"];
    assert!(
        profile
            .reasoning
            .as_ref()
            .unwrap()
            .max_reasoning_tokens
            .unwrap()
            > 4096
    );
    let mut request = LLMRequest {
        model: profile.model.clone(),
        max_output_tokens: Some(4096),
        reasoning: request_reasoning_defaults(profile),
        ..Default::default()
    };
    assert!(request
        .reasoning
        .as_ref()
        .unwrap()
        .max_reasoning_tokens
        .is_none());
    resolve_request_settings(profile, &mut request).unwrap();
    assert_eq!(
        request.reasoning.as_ref().unwrap().max_reasoning_tokens,
        Some(1024)
    );
}

#[test]
fn invalid_catalogs_fail_during_deserialization() {
    for fragment in [
        "aliases: {}",
        "aliases: { previous: model/modern/medium }",
        "aliases: { a: b, b: a }",
        "assignments: { bad: { model: unknown } }",
        "policies: { bad: { output_tokens: 0 } }",
        "models: { bad: { connection: missing, model: x, label: X, limits: { context_window_tokens: 1000, max_output_tokens: 100 } } }",
        "connections: { bad: { provider: openai, max_output_tokens: 0 } }",
    ] {
        let yaml = format!("catalog:\n  {fragment}\n");
        assert!(serde_yaml::from_str::<LLMRouterConfig>(&yaml).is_err(), "{fragment}");
    }
    let mut catalog = config().catalog;
    catalog
        .models
        .get_mut("modern")
        .unwrap()
        .thinking
        .levels
        .push("medium".into());
    assert!(catalog.validate().is_err());
    catalog
        .models
        .get_mut("modern")
        .unwrap()
        .thinking
        .levels
        .pop();
    catalog
        .assignments
        .insert("bad".into(), ModelAssignment::choice("binary", "high"));
    assert!(
        catalog
            .materialize(&mut LLMRouterConfig::default())
            .is_err(),
        "binary model must not pretend to support intensity"
    );
}

#[test]
fn assignment_reasoning_restrictions_survive_model_capability_expansion() {
    let router = config();
    assert_eq!(router.profiles["off_only"].supports_reasoning, Some(false));
    assert_eq!(
        router.profiles["model/binary/on"].supports_reasoning,
        Some(true)
    );
    let mut assignment = ModelAssignment::choice("binary", "on");
    assignment.reasoning = Some(false);
    assert!(router.catalog.resolve_assignment(&assignment).is_err());
}

#[test]
fn unrelated_config_writers_never_inline_catalog_or_generated_routes() {
    let serialized = serde_yaml::to_string(&config()).unwrap();
    assert!(!serialized.contains("\nprofiles:"));
    assert!(!serialized.contains("catalog:"));
    assert!(!serialized.contains(CATALOG_SETTINGS_KEY));
}

#[test]
fn native_coding_controls_are_validated_but_do_not_change_provider_thinking() {
    let mut router = config();
    let model = router.catalog.models.get_mut("modern").unwrap();
    model.coding_thinking = Some(ThinkingControl {
        mode: ThinkingMode::Effort,
        levels: vec!["low".into(), "high".into()],
        default: Some("low".into()),
        budget: None,
    });
    router.catalog.validate().unwrap();
    assert_eq!(
        router.catalog.models["modern"].thinking.default.as_deref(),
        Some("medium")
    );
    router
        .catalog
        .models
        .get_mut("modern")
        .unwrap()
        .coding_thinking
        .as_mut()
        .unwrap()
        .levels
        .push("made-up".into());
    assert!(router.catalog.validate().is_err());
}

#[test]
fn gpt_6_1_terra_choices_use_responses_and_preserve_native_efforts() {
    let yaml = format!(
        "local_model: &local_generation_model fixture-local\nrouter:\n{}",
        include_str!("../../../llm-router.yaml")
    );
    let root: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let router: LLMRouterConfig = serde_yaml::from_value(root["router"].clone()).unwrap();
    let model = &router.catalog.models["openai/gpt-6.1-terra"];
    assert!(model.selectable && model.coding);
    assert!(model.capabilities.vision && model.capabilities.tools);
    assert!(model.capabilities.streaming && model.capabilities.structured_output);
    for effort in ["auto", "none", "low", "medium", "high", "xhigh", "max"] {
        let profile = router
            .catalog
            .resolve_assignment(&ModelAssignment::choice("openai/gpt-6.1-terra", effort))
            .unwrap();
        assert_eq!(profile.model, "gpt-6.1-terra");
        assert_eq!(profile.context_window_tokens, Some(1_050_000));
        assert_eq!(
            profile.metadata.as_ref().unwrap()["openai_api_mode"],
            "responses"
        );
        let expected = if effort == "auto" { "medium" } else { effort };
        assert_eq!(profile.reasoning.as_ref().unwrap().effort, expected);
    }
    assert!(router
        .catalog
        .resolve_assignment(&ModelAssignment::choice("openai/gpt-6.1-terra", "ultra",))
        .is_err());
}

#[test]
fn shipped_models_publish_off_only_when_the_selected_transport_supports_it() {
    let yaml = format!(
        "local_model: &local_generation_model fixture-local\nrouter:\n{}",
        include_str!("../../../llm-router.yaml")
    );
    let root: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let router: LLMRouterConfig = serde_yaml::from_value(root["router"].clone()).unwrap();
    for (id, model) in &router.catalog.models {
        let supports_off = model.thinking.levels.iter().any(|level| level == "none");
        let provider = router.catalog.connections[&model.connection]
            .provider
            .to_string();
        let expected_off = match provider.as_str() {
            "openai" => !matches!(model.model.as_str(), "gpt-6.1-sol" | "gpt-6-astra"),
            "anthropic" => matches!(
                model.model.as_str(),
                "claude-haiku-4-5" | "claude-haiku-5-5" | "claude-opus-4-7" | "claude-opus-5"
            ),
            "deepseek" | "sarvam" => true,
            "minimax" => model.model == "MiniMax-M3",
            "ollama" => !model.embedding,
            _ => false,
        };
        assert_eq!(
            supports_off, expected_off,
            "provider-qualified Off controls: {id}"
        );
        let off = ModelAssignment::choice(id, "none");
        if model.thinking.mode == ThinkingMode::None {
            assert!(model.thinking.levels.is_empty(), "{id}");
            continue;
        }
        if !supports_off {
            assert!(
                router.catalog.resolve_assignment(&off).is_err(),
                "{id} must reject unsupported Off"
            );
            continue;
        }
        let profile = router.catalog.resolve_assignment(&off).unwrap();
        let mut request = LLMRequest {
            model: profile.model.clone(),
            max_output_tokens: profile.max_output_tokens,
            reasoning: request_reasoning_defaults(&profile),
            ..Default::default()
        };
        resolve_request_settings(&profile, &mut request)
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        let reasoning = request.reasoning.as_ref().unwrap();
        assert_eq!(reasoning.effort.as_deref(), Some("none"), "{id}");
        assert_eq!(
            reasoning.max_reasoning_tokens, None,
            "Off cannot reserve a thinking budget: {id}"
        );
    }
}

#[test]
fn model_feedback_is_catalog_metadata_and_never_provider_request_metadata() {
    let mut catalog = config().catalog;
    catalog.models.get_mut("modern").unwrap().feedback = ModelFeedback::Fast;
    let descriptor = catalog
        .choices()
        .into_iter()
        .find(|model| model.id == "modern")
        .unwrap();
    assert_eq!(descriptor.feedback, ModelFeedback::Fast);
    let profile = catalog
        .resolve_assignment(&ModelAssignment::choice("modern", "auto"))
        .unwrap();
    assert!(!profile.metadata.as_ref().unwrap().contains_key("feedback"));
    assert_eq!(
        serde_json::to_value(descriptor).unwrap()["feedback"],
        "fast"
    );
}

#[test]
fn wire_facts_parse_resolve_and_reach_the_request() {
    let mut router: LLMRouterConfig = serde_yaml::from_str(
        r#"
catalog:
  connections:
    cloud: { provider: anthropic, api_key_env: EXAMPLE_KEY }
  models:
    shaped:
      connection: cloud
      model: acme-small-7
      label: Acme Small
      limits: { context_window_tokens: 200000, max_output_tokens: 64000 }
      thinking: { mode: effort, levels: [none, low, high], default: low }
      wire: { sampling: false, thinking_off_effort: low }
    budgeted:
      connection: cloud
      model: acme-budget-1
      label: Acme Budget
      limits: { context_window_tokens: 200000, max_output_tokens: 64000 }
      thinking:
        mode: effort
        levels: [low, high]
        default: low
      wire: { thinking: budget }
    plain:
      connection: cloud
      model: acme-plain
      label: Acme Plain
      limits: { context_window_tokens: 200000, max_output_tokens: 64000 }
default_profile: model/shaped/auto
"#,
    )
    .unwrap();
    let models = &router.catalog.models;
    assert_eq!(
        models["shaped"].wire.resolve(&models["shaped"].thinking),
        ModelWire {
            thinking: Some(ThinkingWire::Adaptive),
            thinking_off: true,
            thinking_off_effort: Some("low".into()),
            sampling: false,
        }
    );
    let budgeted = models["budgeted"]
        .wire
        .resolve(&models["budgeted"].thinking);
    assert_eq!(budgeted.thinking, Some(ThinkingWire::Budget));
    assert!(budgeted.sampling && !budgeted.thinking_off);
    let plain = models["plain"].wire.resolve(&models["plain"].thinking);
    assert_eq!(plain.thinking, None);
    assert!(models["plain"].wire.is_default());
    let serialized = serde_json::to_value(&models["plain"]).unwrap();
    assert!(serialized.get("wire").is_none(), "defaults stay unauthored");

    let profile = router.profiles["model/shaped/low"].clone();
    let mut request = LLMRequest {
        model: profile.model.clone(),
        max_output_tokens: Some(4096),
        reasoning: request_reasoning_defaults(&profile),
        ..Default::default()
    };
    resolve_request_settings(&profile, &mut request).unwrap();
    assert_eq!(request.model_wire.as_ref().map(|w| w.sampling), Some(false));
    assert!(
        serde_json::to_value(&request)
            .unwrap()
            .get("model_wire")
            .is_none(),
        "resolver facts are never serialized toward a provider"
    );

    // A fallback hop onto a legacy profile drops the previous model's facts.
    let legacy = LLMProfile {
        metadata: None,
        ..profile
    };
    resolve_request_settings(&legacy, &mut request).unwrap();
    assert!(request.model_wire.is_none());

    let model = router.catalog.models.get_mut("plain").unwrap();
    model.wire.thinking = Some(ThinkingWire::Adaptive);
    assert!(
        router.catalog.validate().is_err(),
        "a wire shape needs thinking controls"
    );
    let model = router.catalog.models.get_mut("plain").unwrap();
    model.wire.thinking = None;
    let model = router.catalog.models.get_mut("budgeted").unwrap();
    model.wire.thinking_off_effort = Some("low".into());
    assert!(
        router.catalog.validate().is_err(),
        "an Off effort needs a supported Off"
    );
    assert!(serde_yaml::from_str::<ModelWireTraits>("{sampling: false, made_up: 1}").is_err());
}

/// The seed's `wire` facts reproduce the model-name rules they replaced, so
/// moving these models onto catalog facts changed no request. A model added
/// later is described by the catalog alone and may differ from the fallback;
/// list it in `NEWER_THAN_FALLBACK`.
#[test]
fn shipped_wire_facts_match_the_name_rules_they_replaced() {
    const NEWER_THAN_FALLBACK: &[&str] = &[];
    let yaml = format!(
        "local_model: &local_generation_model fixture-local\nrouter:\n{}",
        include_str!("../../../llm-router.yaml")
    );
    let root: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let router: LLMRouterConfig = serde_yaml::from_value(root["router"].clone()).unwrap();
    for (id, model) in &router.catalog.models {
        if NEWER_THAN_FALLBACK.contains(&id.as_str()) {
            continue;
        }
        let connection = &router.catalog.connections[&model.connection];
        let wire = model.wire.resolve(&model.thinking);
        let name = model.model.as_str();
        match connection.provider {
            LLMProviderKind::Anthropic => {
                let legacy = crate::providers::anthropic_messages::uncatalogued_wire(
                    &connection.provider,
                    name,
                );
                assert_eq!(wire.thinking, legacy.thinking, "{id} thinking shape");
                assert_eq!(wire.sampling, legacy.sampling, "{id} sampling");
                assert_eq!(wire.thinking_off, legacy.thinking_off, "{id} Off");
                if wire.thinking_off {
                    assert_eq!(
                        wire.thinking_off_effort, legacy.thinking_off_effort,
                        "{id} Off effort"
                    );
                }
            },
            LLMProviderKind::Gemini => assert_eq!(
                wire.sampling,
                crate::providers::GeminiProvider::accepts_sampling_parameters(name),
                "{id} sampling"
            ),
            LLMProviderKind::OpenAI => {
                let chat = connection
                    .metadata
                    .get("openai_api_mode")
                    .and_then(Value::as_str)
                    == Some("chat");
                let rejects = if chat {
                    crate::providers::openai_chat::uncatalogued_rejects_sampling(name)
                } else {
                    crate::providers::openai_responses::uncatalogued_rejects_sampling(name)
                };
                assert_eq!(wire.sampling, !rejects, "{id} sampling");
            },
            _ => {},
        }
    }
}
