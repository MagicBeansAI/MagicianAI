//! Model-routing API — the Settings panel behind per-operation profile
//! choice (2026-08-31 harness-LLM-provider plan) and, since 2026-09-14, the
//! per-operation parent-engine rule.
//!
//! Routes:
//! ```text
//! GET    /api/magician/v2/llm/routing                       -> operations, profiles, overrides, driving engines
//! PUT    /api/magician/v2/llm/routing/{operation}           -> set one profile override (validated)
//! DELETE /api/magician/v2/llm/routing/{operation}           -> revert one operation to its default profile
//! PUT    /api/magician/v2/llm/routing/{operation}/engine    -> pin one operation (`parent` | `pinned`)
//! DELETE /api/magician/v2/llm/routing/{operation}/engine    -> revert one operation to its config `engine`
//! ```
//!
//! Session-authenticated (routing is an owner decision). Overrides and
//! engine pins live in the install-level stores, never the config file:
//! shipped defaults stay exactly as validated, every change is one
//! operation, and reverting is a DELETE. The no-override guarantee is
//! untouched — this surface is the explicit, per-operation opt-in the
//! guarantee presumes.
//!
//! The parent engine is flow-scoped: a background operation rides the
//! engine that started its flow, never a process value. GET therefore
//! reports the engines that would start flows now (`driving_engines`: the
//! chat mouth and the run engine) and, per operation, whether it follows
//! them and which profile it would ride under each.

use std::collections::HashMap;

use actix_web::{web, HttpRequest, HttpResponse};
use magicllm::config::{LLMRouterConfig, OperationEngineFollow, RequestShape};
use serde_json::{json, Value};

use crate::auth_api::require_session;
use magician::config::{load_magician_config_from_path, magician_config_path};
use magician::magician_v2::execution::plane::{chat_harness_snapshot, harness_engine_snapshot};
use magician::magician_v2::query_analysis::operation_llm_router as routing;
use magician::magician_v2::query_analysis::parent_engine::normalize_parent_engine;

fn provider_class(provider: &str) -> &'static str {
    if provider == "ollama" {
        "local"
    } else if provider.starts_with("harness-") {
        "harness"
    } else {
        "api"
    }
}

fn binary_on_path(binary: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(binary).is_file())
}

fn harness_binary(provider: &str) -> Option<&'static str> {
    let suffix = provider.strip_prefix("harness-")?;
    match suffix {
        "claude_code" => Some("claude"),
        "codex" => Some("codex"),
        "grok" => Some("grok"),
        "agy" => Some("agy"),
        "pi" => Some("pi"),
        _ => None,
    }
}

fn fallback_operation_description(operation: &str) -> String {
    let readable = operation.replace(['_', ':'], " ");
    format!("Runs the configured {readable} LLM operation.")
}

fn engine_follow_name(follow: OperationEngineFollow) -> &'static str {
    match follow {
        OperationEngineFollow::Parent => "parent",
        OperationEngineFollow::Pinned => "pinned",
    }
}

fn parse_engine_follow(value: &str) -> Option<OperationEngineFollow> {
    match value.trim() {
        "parent" => Some(OperationEngineFollow::Parent),
        "pinned" => Some(OperationEngineFollow::Pinned),
        _ => None,
    }
}

/// The engines that start flows right now, normalised the way the flow
/// carriers normalise them (`None` for the native loop). Informational:
/// resolution reads each flow's own parent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrivingEngines {
    pub chat: Option<String>,
    pub run: Option<String>,
}

impl DrivingEngines {
    fn live() -> Self {
        Self {
            chat: normalize_parent_engine(Some(&chat_harness_snapshot().engine)),
            run: normalize_parent_engine(Some(&harness_engine_snapshot().engine)),
        }
    }
}

/// Everything the overview reads besides the config: the two install-level
/// stores and the engines in use. Passed in so the payload is a pure
/// function of its inputs.
#[derive(Debug, Clone, Default)]
pub struct RoutingState {
    pub overrides: HashMap<String, String>,
    pub engine_pins: HashMap<String, OperationEngineFollow>,
    pub driving: DrivingEngines,
    /// The display cell (`set_harness_affinity`), kept on the wire for
    /// older readers; nothing new derives from it.
    pub affinity: Option<String>,
    pub affinity_profile: Option<String>,
}

impl RoutingState {
    fn live(router: &LLMRouterConfig) -> Self {
        Self {
            overrides: routing::llm_routing_overrides_snapshot(),
            engine_pins: routing::llm_routing_engine_snapshot(),
            driving: DrivingEngines::live(),
            affinity: routing::harness_affinity(),
            affinity_profile: routing::harness_affinity_profile(router),
        }
    }
}

/// The `GET /llm/routing` body for one router config and one routing state.
pub fn routing_overview(router: &LLMRouterConfig, state: &RoutingState) -> Value {
    let mut profiles: Vec<_> = router
        .profiles
        .iter()
        .map(|(name, profile)| {
            let provider = profile.provider.as_str();
            let harness_binary = harness_binary(&provider);
            let class = if provider == "ollama" {
                "local"
            } else if harness_binary.is_some() {
                "harness"
            } else {
                "api"
            };
            json!({
                "name": name,
                "provider": provider,
                "model": profile.model,
                "class": class,
                "installed": harness_binary
                    .map(binary_on_path)
                    .unwrap_or(true),
                "selectable": true,
            })
        })
        .collect();
    // Adaptive composites can be an operation's DEFAULT (chat routes to
    // them); without listing them the panel's dropdown could not render
    // such an effective profile. Composites are API profiles by
    // construction (their arms name API profiles).
    for name in router.adaptive_profiles.keys() {
        profiles.push(json!({
            "name": name,
            "provider": "adaptive",
            "model": "adaptive composite",
            "class": "api",
            "installed": true,
            // Composites are listed so an operation whose DEFAULT is one
            // renders correctly — but they cannot be override TARGETS
            // (the seam resolves overrides against `profiles` only), and
            // PUT would refuse them. The dropdown must not offer a
            // choice that can never take.
            "selectable": false,
        }));
    }

    // The profile each driving engine's followers would ride, once per
    // engine rather than per operation; the router owns the mapping.
    let parent_profile_for = |engine: Option<&String>| {
        engine.and_then(|engine| routing::parent_profile_for_engine(router, engine))
    };
    let chat_parent_profile = parent_profile_for(state.driving.chat.as_ref());
    let run_parent_profile = parent_profile_for(state.driving.run.as_ref());
    let a_parent_can_be_ridden = chat_parent_profile.is_some() || run_parent_profile.is_some();

    let operations: Vec<_> = router
        .operation_mapping
        .iter()
        .map(|(operation, selector)| {
            let default_profile = selector.default_profile();
            let configured_profile = selector
                .profile_for_locality(&RequestShape::NONE, router.locality)
                .to_string();
            // Mirror the seam's staleness rule: an override naming a
            // profile the config no longer declares is IGNORED by the
            // router, so reporting it as effective would be a lie the
            // panel renders. Stale rows surface as default + a flag.
            let live_override = state
                .overrides
                .get(operation)
                .filter(|profile| router.profiles.contains_key(profile.as_str()));
            // The router's precedence for a request outside any flow:
            // panel override > config default. Inside a flow the parent
            // sits between the two for an operation that follows it, so
            // the row says whether it would and what it would ride.
            let config_default_is_local = router
                .profiles
                .get(default_profile)
                .is_some_and(|profile| provider_class(profile.provider.as_str()) == "local");
            let engine_pin = state.engine_pins.get(operation).copied();
            let engine = engine_pin.unwrap_or(if selector.follows_parent() {
                OperationEngineFollow::Parent
            } else {
                OperationEngineFollow::Pinned
            });
            // The router's floor, mirrored: a local default never follows,
            // whatever the setting says.
            let follows_parent = routing::follows_parent_with(engine_pin, Some(selector))
                && !config_default_is_local;
            let effective_profile = live_override
                .cloned()
                .unwrap_or_else(|| configured_profile.clone());
            let routing_source = if live_override.is_some() {
                "override"
            } else if follows_parent && a_parent_can_be_ridden {
                "parent"
            } else {
                "config"
            };
            let engine_source = if engine_pin.is_some() {
                "override"
            } else {
                "config"
            };
            json!({
                "operation": operation,
                "group": selector.group().unwrap_or("Other"),
                "description": selector
                    .description()
                    .map(str::to_owned)
                    .unwrap_or_else(|| fallback_operation_description(operation)),
                "configured_selector": selector,
                "default_profile": default_profile,
                "configured_profile": configured_profile,
                "effective_profile": effective_profile,
                "routing_source": routing_source,
                "overridden": live_override.is_some(),
                "stale_override": state.overrides.contains_key(operation)
                    && live_override.is_none(),
                "engine": engine_follow_name(engine),
                "engine_source": engine_source,
                "follows_parent": follows_parent,
                "local_floor": config_default_is_local,
                "parent_profiles": {
                    "chat": chat_parent_profile,
                    "run": run_parent_profile,
                },
            })
        })
        .collect();

    let engine_pins: HashMap<&str, &str> = state
        .engine_pins
        .iter()
        .map(|(operation, follow)| (operation.as_str(), engine_follow_name(*follow)))
        .collect();

    json!({
        "operations": operations,
        "profiles": profiles,
        "overrides": state.overrides,
        "engine_pins": engine_pins,
        "affinity": state.affinity,
        "affinity_profile": state.affinity_profile,
        "affinity_scope": "flow",
        "driving_engines": {
            "chat": state.driving.chat,
            "run": state.driving.run,
        },
        "locality": router.locality,
        "rule": "Explicit operation override > the engine that started the flow (chat mouth, run engine, or connected CLI) for non-local, tool-free operations unless the operation is pinned > locality-aware config mapping.",
    })
}

/// `GET /llm/routing` — every mapped operation with its default and
/// effective profile, its parent-engine rule, the profile roster classified
/// (local / api / harness with install status), the current overrides and
/// pins, and the engines driving flows now.
pub async fn llm_routing_overview_handler(req: HttpRequest) -> HttpResponse {
    if let Err(response) = require_session(&req) {
        return response;
    }
    let config = match load_magician_config_from_path(&magician_config_path()) {
        Ok(config) => config,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({
                "error": "config_load_failed",
                "message": error.to_string()
            }));
        },
    };
    let Some(router) = config.llm.router.as_ref() else {
        return HttpResponse::ServiceUnavailable().json(json!({
            "error": "router_not_configured"
        }));
    };
    HttpResponse::Ok().json(routing_overview(router, &RoutingState::live(router)))
}

/// `PUT /llm/routing/{operation}` — body `{"profile": "op-harness-claude"}`.
/// Refuses unknown operations and unknown profiles; refuses stale config
/// (profile gone) at set time so the store cannot drift into dead names.
pub async fn llm_routing_set_handler(
    req: HttpRequest,
    operation: web::Path<String>,
    body: web::Json<serde_json::Value>,
) -> HttpResponse {
    if let Err(response) = require_session(&req) {
        return response;
    }
    let profile = body
        .get("profile")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if profile.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "profile_required"
        }));
    }
    let config = match load_magician_config_from_path(&magician_config_path()) {
        Ok(config) => config,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({
                "error": "config_load_failed",
                "message": error.to_string()
            }));
        },
    };
    let Some(router) = config.llm.router.as_ref() else {
        return HttpResponse::ServiceUnavailable().json(json!({
            "error": "router_not_configured"
        }));
    };
    let operation = operation.into_inner();
    if !router.operation_mapping.contains_key(&operation) {
        return HttpResponse::NotFound().json(json!({
            "error": "unknown_operation",
            "message": format!("{operation:?} is not a mapped operation")
        }));
    }
    if !router.profiles.contains_key(profile) {
        return HttpResponse::BadRequest().json(json!({
            "error": "unknown_profile",
            "message": format!("profile {profile:?} is not declared in llm.router.profiles")
        }));
    }
    if let Err(error) = routing::set_llm_routing_override(&operation, profile) {
        return HttpResponse::InternalServerError().json(json!({
            "error": "override_persist_failed",
            "message": error.to_string()
        }));
    }
    HttpResponse::Ok().json(json!({
        "operation": operation,
        "effective_profile": profile,
        "overridden": true
    }))
}

/// `DELETE /llm/routing/{operation}` — revert to the config default.
pub async fn llm_routing_clear_handler(
    req: HttpRequest,
    operation: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = require_session(&req) {
        return response;
    }
    match routing::clear_llm_routing_override(&operation.into_inner()) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => HttpResponse::NotFound().finish(),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": "override_persist_failed",
            "message": error.to_string()
        })),
    }
}

/// The live router config, or the response that says why there is none.
fn load_live_router() -> Result<LLMRouterConfig, HttpResponse> {
    let config = load_magician_config_from_path(&magician_config_path()).map_err(|error| {
        HttpResponse::InternalServerError().json(json!({
            "error": "config_load_failed",
            "message": error.to_string()
        }))
    })?;
    config.llm.router.ok_or_else(|| {
        HttpResponse::ServiceUnavailable().json(json!({
            "error": "router_not_configured"
        }))
    })
}

/// The pin a `PUT /llm/routing/{operation}/engine` body asks for, or the
/// refusal: an unmapped operation is 404, an `engine` outside `parent` |
/// `pinned` is 400.
fn engine_pin_request(
    router: &LLMRouterConfig,
    operation: &str,
    body: &Value,
) -> Result<OperationEngineFollow, HttpResponse> {
    if !router.operation_mapping.contains_key(operation) {
        return Err(HttpResponse::NotFound().json(json!({
            "error": "unknown_operation",
            "message": format!("{operation:?} is not a mapped operation")
        })));
    }
    let engine = body.get("engine").and_then(Value::as_str).unwrap_or("");
    parse_engine_follow(engine).ok_or_else(|| {
        HttpResponse::BadRequest().json(json!({
            "error": "unknown_engine_follow",
            "message": format!("engine {engine:?} is not one of parent, pinned")
        }))
    })
}

/// `PUT /llm/routing/{operation}/engine` — body `{"engine": "parent" | "pinned"}`.
/// Pins one operation off the flow's parent engine (or back onto it) in
/// the install-level store; the config selector's `engine` is the default
/// this overrides.
pub async fn llm_routing_engine_set_handler(
    req: HttpRequest,
    operation: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    if let Err(response) = require_session(&req) {
        return response;
    }
    let router = match load_live_router() {
        Ok(router) => router,
        Err(response) => return response,
    };
    let operation = operation.into_inner();
    let follow = match engine_pin_request(&router, &operation, &body) {
        Ok(follow) => follow,
        Err(response) => return response,
    };
    if let Err(error) = routing::set_llm_routing_engine(&operation, follow) {
        return HttpResponse::InternalServerError().json(json!({
            "error": "engine_pin_persist_failed",
            "message": error.to_string()
        }));
    }
    HttpResponse::Ok().json(json!({
        "operation": operation,
        "engine": engine_follow_name(follow),
        "engine_source": "override"
    }))
}

/// `DELETE /llm/routing/{operation}/engine` — revert to the config
/// selector's `engine`.
pub async fn llm_routing_engine_clear_handler(
    req: HttpRequest,
    operation: web::Path<String>,
) -> HttpResponse {
    if let Err(response) = require_session(&req) {
        return response;
    }
    match routing::clear_llm_routing_engine(&operation.into_inner()) {
        Ok(true) => HttpResponse::NoContent().finish(),
        Ok(false) => HttpResponse::NotFound().finish(),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": "engine_pin_persist_failed",
            "message": error.to_string()
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::http::StatusCode;

    #[test]
    fn pi_operation_profile_uses_the_cli_install_probe() {
        assert_eq!(harness_binary("harness-pi"), Some("pi"));
    }

    const FOLLOWS: &str = "task_summary";
    const CONFIG_PINNED: &str = "memory_consolidation";
    const LOCAL_DEFAULT: &str = "meeting_summary";

    /// An API default every operation maps to, one harness profile, a local
    /// profile, and three operations: one that follows by default, one the
    /// config pins, one whose default is local.
    fn router() -> LLMRouterConfig {
        serde_json::from_value(json!({
            "default_profile": "cloud-tools",
            "profiles": {
                "cloud-tools": {"provider": "openai", "model": "api-model"},
                "op-harness-codex": {"provider": "harness-codex", "model": "default"},
                "local-summary": {"provider": "ollama", "model": "local-model"}
            },
            "operation_mapping": {
                FOLLOWS: {"default": "cloud-tools", "group": "Summaries"},
                CONFIG_PINNED: {"default": "cloud-tools", "engine": "pinned"},
                LOCAL_DEFAULT: {"default": "local-summary"}
            }
        }))
        .expect("the router fixture parses")
    }

    fn chat_on_codex() -> RoutingState {
        RoutingState {
            driving: DrivingEngines {
                chat: Some("codex".to_string()),
                run: None,
            },
            ..RoutingState::default()
        }
    }

    fn operation_row(overview: &Value, operation: &str) -> Value {
        overview["operations"]
            .as_array()
            .expect("operations is an array")
            .iter()
            .find(|row| row["operation"] == operation)
            .cloned()
            .unwrap_or_else(|| panic!("{operation} is listed"))
    }

    #[test]
    fn the_overview_names_the_flow_scope_and_the_driving_engines() {
        let overview = routing_overview(&router(), &chat_on_codex());

        assert_eq!(overview["affinity_scope"], "flow");
        assert_eq!(
            overview["driving_engines"],
            json!({"chat": "codex", "run": null})
        );
        assert!(overview["rule"]
            .as_str()
            .is_some_and(|rule| rule.contains("unless the operation is pinned")));

        let follows = operation_row(&overview, FOLLOWS);
        assert_eq!(follows["engine"], "parent");
        assert_eq!(follows["engine_source"], "config");
        assert_eq!(follows["follows_parent"], true);
        assert_eq!(follows["local_floor"], false);
        assert_eq!(follows["routing_source"], "parent");
        assert_eq!(follows["effective_profile"], "cloud-tools");
        assert_eq!(
            follows["parent_profiles"],
            json!({"chat": "op-harness-codex", "run": null})
        );
        assert!(follows.get("via_affinity").is_none());
    }

    #[test]
    fn a_pinned_operation_keeps_its_config_profile() {
        let overview = routing_overview(&router(), &chat_on_codex());

        let pinned = operation_row(&overview, CONFIG_PINNED);
        assert_eq!(pinned["engine"], "pinned");
        assert_eq!(pinned["engine_source"], "config");
        assert_eq!(pinned["follows_parent"], false);
        assert_eq!(pinned["routing_source"], "config");
        assert_eq!(pinned["effective_profile"], "cloud-tools");
    }

    #[test]
    fn a_store_pin_outranks_the_config_selector_both_ways() {
        let mut state = chat_on_codex();
        state
            .engine_pins
            .insert(FOLLOWS.to_string(), OperationEngineFollow::Pinned);
        state
            .engine_pins
            .insert(CONFIG_PINNED.to_string(), OperationEngineFollow::Parent);
        let overview = routing_overview(&router(), &state);

        let pinned_from_settings = operation_row(&overview, FOLLOWS);
        assert_eq!(pinned_from_settings["engine"], "pinned");
        assert_eq!(pinned_from_settings["engine_source"], "override");
        assert_eq!(pinned_from_settings["follows_parent"], false);
        assert_eq!(pinned_from_settings["routing_source"], "config");

        let released_from_settings = operation_row(&overview, CONFIG_PINNED);
        assert_eq!(released_from_settings["engine"], "parent");
        assert_eq!(released_from_settings["engine_source"], "override");
        assert_eq!(released_from_settings["follows_parent"], true);
        assert_eq!(released_from_settings["routing_source"], "parent");

        assert_eq!(
            overview["engine_pins"],
            json!({FOLLOWS: "pinned", CONFIG_PINNED: "parent"})
        );
    }

    #[test]
    fn a_local_default_never_follows_and_says_so() {
        let overview = routing_overview(&router(), &chat_on_codex());

        let local = operation_row(&overview, LOCAL_DEFAULT);
        assert_eq!(local["engine"], "parent", "the setting still shows");
        assert_eq!(local["follows_parent"], false);
        assert_eq!(local["local_floor"], true);
        assert_eq!(local["routing_source"], "config");
    }

    #[test]
    fn without_an_external_driving_engine_nothing_rides_a_parent() {
        let overview = routing_overview(&router(), &RoutingState::default());

        assert_eq!(
            overview["driving_engines"],
            json!({"chat": null, "run": null})
        );
        let follows = operation_row(&overview, FOLLOWS);
        assert_eq!(follows["follows_parent"], true, "the rule holds");
        assert_eq!(
            follows["routing_source"], "config",
            "but no flow would ride one"
        );
        assert_eq!(
            follows["parent_profiles"],
            json!({"chat": null, "run": null})
        );
    }

    #[test]
    fn an_explicit_override_outranks_the_parent() {
        let mut state = chat_on_codex();
        state
            .overrides
            .insert(FOLLOWS.to_string(), "local-summary".to_string());
        let overview = routing_overview(&router(), &state);

        let overridden = operation_row(&overview, FOLLOWS);
        assert_eq!(overridden["routing_source"], "override");
        assert_eq!(overridden["effective_profile"], "local-summary");
        assert_eq!(overridden["overridden"], true);
        assert_eq!(
            overridden["follows_parent"], true,
            "the rule is reported, the override wins"
        );
    }

    #[test]
    fn an_engine_pin_request_refuses_unknown_values_and_operations() {
        let router = router();

        let unknown_value = engine_pin_request(&router, FOLLOWS, &json!({"engine": "sometimes"}))
            .expect_err("an unknown follow is refused");
        assert_eq!(unknown_value.status(), StatusCode::BAD_REQUEST);

        let missing_value = engine_pin_request(&router, FOLLOWS, &json!({}))
            .expect_err("a missing follow is refused");
        assert_eq!(missing_value.status(), StatusCode::BAD_REQUEST);

        let unknown_operation =
            engine_pin_request(&router, "not_an_operation", &json!({"engine": "pinned"}))
                .expect_err("an unmapped operation is refused");
        assert_eq!(unknown_operation.status(), StatusCode::NOT_FOUND);

        let accepted = engine_pin_request(&router, FOLLOWS, &json!({"engine": " pinned "}))
            .expect("a known follow on a mapped operation is accepted");
        assert_eq!(accepted, OperationEngineFollow::Pinned);
        assert_eq!(engine_follow_name(accepted), "pinned");
    }
}
