//! Typed model/thinking admission, shared by chat, queue and concurrent voice.

use super::{ChatApi, SendMessageRequest};
use actix_web::{web, HttpResponse, Responder};
use magician::magician_v2::query_analysis::multi_llm_service::ChatProfileInfo;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Deserialize)]
pub struct ChatModelsQuery {
    pub engine: Option<String>,
}

fn refusal(message: &str) -> HttpResponse {
    HttpResponse::BadRequest().json(json!({"error":"invalid_model_selection","message":message}))
}

pub(super) fn qualified_pi_models() -> Result<HashSet<String>, HttpResponse> {
    let config =
        magician::config::load_magician_config_from_path(&magician::config::magician_config_path())
            .map_err(|_| {
                HttpResponse::ServiceUnavailable()
                    .json(json!({"error":"model_catalog_unavailable"}))
            })?;
    let router = config.router_config().ok_or_else(|| {
        HttpResponse::ServiceUnavailable().json(json!({"error":"model_catalog_unavailable"}))
    })?;
    Ok(router
        .catalog
        .models
        .iter()
        .filter(|(_, model)| model.coding)
        .map(|(id, _)| id.clone())
        .collect())
}

pub(super) fn selected_route(
    profiles: &[ChatProfileInfo],
    body: &SendMessageRequest,
    pi_models: Option<&HashSet<String>>,
) -> Result<String, HttpResponse> {
    let selection = body
        .model_selection
        .as_ref()
        .ok_or_else(|| refusal("A model selection is required."))?;
    if !matches!(body.harness_engine.as_deref(), Some("magician" | "pi")) {
        return Err(refusal(
            "Model selections require an explicit Magician or Pi engine.",
        ));
    }
    if body
        .harness_model
        .as_deref()
        .is_some_and(|model| !model.is_empty() && model != "default")
    {
        return Err(refusal(
            "Use model_selection instead of a harness model for this engine.",
        ));
    }
    if body.harness_engine.as_deref() == Some("pi")
        && !pi_models.is_some_and(|models| models.contains(&selection.model_id))
    {
        return Err(refusal("This model is not qualified for the Pi engine."));
    }
    let model = profiles
        .iter()
        .filter_map(|profile| profile.model_choice.as_ref())
        .find(|model| model.id == selection.model_id)
        .ok_or_else(|| {
            refusal("This model is unavailable for chat. Choose a model to continue.")
        })?;
    let route = model
        .choices
        .get(&selection.thinking)
        .filter(|route| {
            profiles
                .iter()
                .any(|profile| profile.name.as_str() == route.as_str())
        })
        .ok_or_else(|| refusal("This thinking level is unavailable for the selected model."))?;
    Ok(route.clone())
}

pub fn chat_model_catalog(
    profiles: &[ChatProfileInfo],
    pi_models: Option<&HashSet<String>>,
) -> Value {
    let models: BTreeMap<_, _> = profiles
        .iter()
        .filter_map(|profile| profile.model_choice.as_ref())
        .filter(|model| pi_models.is_none_or(|qualified| qualified.contains(&model.id)))
        .map(|model| {
            let mut model = model.clone();
            if pi_models.is_some() {
                // Pi's usable context is SDK-owned until the engine attests it.
                model.limits = None;
            }
            (model.id.clone(), model)
        })
        .collect();
    let default_selection = profiles
        .iter()
        .filter(|profile| profile.is_default)
        .filter_map(|profile| profile.model_selection.as_ref())
        .find(|selection| {
            models
                .get(&selection.model_id)
                .is_some_and(|model| model.choices.contains_key(&selection.thinking))
        })
        .cloned();
    json!({"models":models.into_values().collect::<Vec<_>>(),"default_selection":default_selection})
}

/// GET /chat/models?engine=magician|pi — no saved-profile alias rows.
pub async fn list_chat_models_handler(
    api: web::Data<ChatApi>,
    query: web::Query<ChatModelsQuery>,
) -> impl Responder {
    let engine = query.engine.as_deref().unwrap_or("magician");
    if !matches!(engine, "magician" | "pi") {
        return refusal("Use the native engine roster for this chat engine.");
    }
    let pi_models = if engine == "pi" {
        match qualified_pi_models() {
            Ok(models) => Some(models),
            Err(response) => return response,
        }
    } else {
        None
    };
    HttpResponse::Ok().json(chat_model_catalog(
        &api.chat_service.list_chat_profiles(),
        pi_models.as_ref(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profiles() -> Vec<ChatProfileInfo> {
        let model = magicllm::catalog::ModelChoice {
            id: "provider/model".into(),
            provider: "provider".into(),
            connection: "provider".into(),
            model: "physical-model".into(),
            label: "Model".into(),
            feedback: magicllm::catalog::ModelFeedback::Standard,
            capabilities: serde_json::from_value(
                json!({"vision":true,"tools":true,"streaming":true}),
            )
            .unwrap(),
            limits: Some(
                serde_json::from_value(json!({
                    "context_window_tokens":128000,"min_output_tokens":1,
                    "max_output_tokens":16384,"shared_context_window":true
                }))
                .unwrap(),
            ),
            thinking: serde_json::from_value(
                json!({"mode":"effort","levels":["low","high"],"default":"low"}),
            )
            .unwrap(),
            choices: BTreeMap::from([
                ("auto".into(), "model/provider/model/auto".into()),
                ("high".into(), "model/provider/model/high".into()),
            ]),
        };
        ["auto", "high"]
            .into_iter()
            .map(|thinking| ChatProfileInfo {
                name: model.choices[thinking].clone(),
                provider: "provider".into(),
                model: "physical-model".into(),
                is_default: thinking == "auto",
                supports_user_image_inputs: true,
                model_selection: Some(magicllm::catalog::ModelSelection {
                    model_id: model.id.clone(),
                    thinking: thinking.into(),
                }),
                model_choice: (thinking == "auto").then_some(model.clone()),
            })
            .collect()
    }

    fn request(value: Value) -> SendMessageRequest {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn retired_profile_input_is_rejected_even_without_a_typed_selection() {
        for value in [
            json!({"profile":"old"}),
            json!({"profile":null}),
            json!({"profile":"old","model_selection":{"model_id":"provider/model","thinking":"auto"}}),
        ] {
            assert!(serde_json::from_value::<SendMessageRequest>(value).is_err());
        }
    }

    #[test]
    fn typed_chat_selection_resolves_without_aliases_and_keeps_auto_distinct() {
        let rows = profiles();
        for thinking in ["auto", "high"] {
            let body = request(
                json!({"harness_engine":"magician","model_selection":{"model_id":"provider/model","thinking":thinking}}),
            );
            assert_eq!(
                selected_route(&rows, &body, None).unwrap(),
                format!("model/provider/model/{thinking}")
            );
        }
        let catalog = chat_model_catalog(&rows, None);
        assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
        assert_eq!(catalog["default_selection"]["thinking"], "auto");
        assert!(catalog.get("profiles").is_none());
    }

    #[test]
    fn invalid_ambiguous_or_wrong_engine_selections_are_never_defaulted() {
        let selection = json!({"model_id":"provider/model","thinking":"high"});
        for value in [
            json!({"model_selection":selection}),
            json!({"harness_engine":"codex_app_server","model_selection":selection}),
            json!({"harness_engine":"magician","harness_model":"another-model","model_selection":selection}),
            json!({"harness_engine":"magician","model_selection":{"model_id":"missing","thinking":"auto"}}),
            json!({"harness_engine":"magician","model_selection":{"model_id":"provider/model","thinking":"none"}}),
        ] {
            assert!(selected_route(&profiles(), &request(value), None).is_err());
        }
        let mut rows = profiles();
        rows.pop();
        let body = request(json!({"harness_engine":"magician","model_selection":selection}));
        assert!(
            selected_route(&rows, &body, None).is_err(),
            "descriptor alone cannot admit an ineligible route"
        );
    }

    #[test]
    fn pi_catalog_and_admission_share_the_same_qualification_boundary() {
        let rows = profiles();
        let body = request(
            json!({"harness_engine":"pi","model_selection":{"model_id":"provider/model","thinking":"high"}}),
        );
        let empty = HashSet::new();
        assert!(selected_route(&rows, &body, Some(&empty)).is_err());
        assert_eq!(
            chat_model_catalog(&rows, Some(&empty))["models"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        let qualified = HashSet::from(["provider/model".into()]);
        assert!(selected_route(&rows, &body, Some(&qualified)).is_ok());
        assert_eq!(
            chat_model_catalog(&rows, None)["models"][0]["limits"]["context_window_tokens"],
            128000
        );
        assert!(chat_model_catalog(&rows, Some(&qualified))["models"][0]["limits"].is_null());
        assert_eq!(
            chat_model_catalog(&rows, Some(&qualified))["models"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
