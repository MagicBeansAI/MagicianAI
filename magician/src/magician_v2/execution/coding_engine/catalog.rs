//! Native-engine model choices derived from the shared catalog. Eligibility is
//! inherited from the existing qualified engine entry, never inferred from a UI.
//!
//! Availability is runtime ([`super::model_discovery`]): a model its CLI no
//! longer offers is annotated `unavailable` and pickers hide it, and a model
//! the CLI offers that the catalog lacks is added as an uncatalogued choice
//! (the CLI's default request shape, no thinking control, price unknown).

use super::{model_discovery::NATIVE_ENGINE_SOURCES, selection::ProfileCatalogEntry};
use magicllm::{
    catalog::{ModelCapabilities, ModelChoice, ModelFeedback, ModelSelection, ThinkingControl},
    model_discovery::{self, ModelAvailability, ModelDiscovery},
    LLMRouterConfig,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub struct NativeCodingModel {
    pub engine: &'static str,
    pub default_id: &'static str,
    pub choice: ModelChoice,
    /// The engine's discovery source (`harness-agy`, ...).
    pub source: &'static str,
    /// Offered by the engine's CLI but absent from the catalog.
    pub uncatalogued: bool,
}

impl NativeCodingModel {
    /// Whether pickers may offer it: not known to be withdrawn by its CLI.
    pub fn offered(&self) -> bool {
        model_discovery::with_registry(|discovery| {
            self.offered_in(discovery, model_discovery::now_ms())
        })
    }

    pub fn offered_in(&self, discovery: &ModelDiscovery, now_ms: i64) -> bool {
        self.uncatalogued
            || discovery.availability(self.source, &self.choice.model, now_ms)
                != ModelAvailability::Unavailable
    }
}

fn native_choice_routes(choice: &mut ModelChoice, engine: &str) {
    choice.id = format!("native/{engine}/{}", choice.id);
    choice.choices = std::iter::once("auto")
        .chain(choice.thinking.levels.iter().map(String::as_str))
        .map(|level| (level.to_string(), format!("{}/{level}", choice.id)))
        .collect::<BTreeMap<_, _>>();
}

/// Choices for models a native engine's CLI offers that the catalog lacks.
fn uncatalogued_models(
    router: &LLMRouterConfig,
    discovery: &ModelDiscovery,
) -> Vec<NativeCodingModel> {
    let catalog = &router.catalog;
    NATIVE_ENGINE_SOURCES
        .iter()
        .flat_map(|&(engine, default_id, source)| {
            let connection = catalog
                .connections
                .iter()
                .find(|(_, connection)| model_discovery::source_key(connection) == source);
            let Some((connection_id, connection)) = connection else {
                return Vec::new();
            };
            discovery
                .uncatalogued(catalog, source)
                .into_iter()
                .filter_map(|model| {
                    let id = format!("{connection_id}/{model}");
                    if catalog.models.contains_key(&id) {
                        return None;
                    }
                    let mut choice = ModelChoice {
                        id,
                        provider: connection.provider.to_string(),
                        connection: connection_id.clone(),
                        label: model.clone(),
                        model,
                        feedback: ModelFeedback::Standard,
                        capabilities: ModelCapabilities::default(),
                        limits: None,
                        thinking: ThinkingControl::default(),
                        choices: BTreeMap::new(),
                    };
                    native_choice_routes(&mut choice, engine);
                    Some(NativeCodingModel {
                        engine,
                        default_id,
                        choice,
                        source,
                        uncatalogued: true,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn models(router: Option<&LLMRouterConfig>) -> Vec<NativeCodingModel> {
    model_discovery::with_registry(|discovery| models_in(router, discovery))
}

/// [`models`] against an explicit discovery registry.
pub fn models_in(
    router: Option<&LLMRouterConfig>,
    discovery: &ModelDiscovery,
) -> Vec<NativeCodingModel> {
    let Some(router) = router else {
        return Vec::new();
    };
    let mut models = catalogued_models(router);
    models.extend(uncatalogued_models(router, discovery));
    models
}

fn catalogued_models(router: &LLMRouterConfig) -> Vec<NativeCodingModel> {
    router
        .catalog
        .choices()
        .into_iter()
        .filter_map(|mut choice| {
            let (engine, default_id, source) = NATIVE_ENGINE_SOURCES
                .into_iter()
                .find(|(_, _, source)| *source == choice.provider)?;
            choice.thinking = router.catalog.models[&choice.id]
                .coding_thinking
                .clone()
                .unwrap_or_default();
            choice.limits = None;
            if choice.model == "default" {
                choice.label = "Default".into();
                // A CLI default is owner-managed; its current native effort is
                // not attested by the API model catalog.
                choice.thinking.default = None;
            }
            native_choice_routes(&mut choice, engine);
            Some(NativeCodingModel {
                engine,
                default_id,
                choice,
                source,
                uncatalogued: false,
            })
        })
        .collect()
}

pub fn entries(
    router: Option<&LLMRouterConfig>,
    base: &[ProfileCatalogEntry],
) -> Vec<ProfileCatalogEntry> {
    models(router)
        .into_iter()
        .flat_map(|model| {
            let Some(template) = base
                .iter()
                .find(|entry| entry.eligible && entry.definition.id() == model.default_id)
            else {
                return Vec::new();
            };
            model
                .choice
                .choices
                .iter()
                .filter_map(|(level, id)| {
                    let effort = if level == "auto" {
                        model.choice.thinking.default.clone()
                    } else {
                        Some(level.clone())
                    };
                    let selected_model = if model.choice.model == "default" {
                        template.definition.model()?.to_string()
                    } else {
                        model.choice.model.clone()
                    };
                    let definition = template.definition.clone().with_model_settings(
                        id.clone(),
                        selected_model,
                        effort,
                    );
                    ProfileCatalogEntry::new(definition, true).ok()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn project(router: Option<&LLMRouterConfig>, rows: &mut Vec<Value>) {
    model_discovery::with_registry(|discovery| {
        project_in(router, rows, discovery, model_discovery::now_ms())
    })
}

/// [`project`] against an explicit discovery registry.
pub fn project_in(
    router: Option<&LLMRouterConfig>,
    rows: &mut Vec<Value>,
    discovery: &ModelDiscovery,
    now_ms: i64,
) {
    for model in models_in(router, discovery) {
        let Some(template) = rows
            .iter()
            .find(|row| row["id"].as_str() == Some(model.default_id))
            .cloned()
        else {
            continue;
        };
        // Saved default ids gain the same model selection metadata.
        if model.choice.model == "default" {
            if let Some(row) = rows
                .iter_mut()
                .find(|row| row["id"].as_str() == Some(model.default_id))
            {
                row["model_selection"] = json!(ModelSelection {
                    model_id: model.choice.id.clone(),
                    thinking: "auto".into()
                });
            }
        }
        for (level, id) in &model.choice.choices {
            let mut row = template.clone();
            row["id"] = json!(id);
            row["label"] = json!(model.choice.label);
            row["model"] = json!(model.choice.model);
            row["engine"] = json!(model.engine);
            row["model_selection"] = json!(ModelSelection {
                model_id: model.choice.id.clone(),
                thinking: level.clone()
            });
            row["model_choice"] = if level == "auto" {
                let mut choice = json!(model.choice);
                if model.uncatalogued {
                    choice["availability"] = json!({
                        "state": "available",
                        "source": model.source,
                        "catalogued": false,
                    });
                }
                choice
            } else {
                Value::Null
            };
            rows.push(row);
        }
    }
    if let Some(router) = router {
        annotate_availability(router, rows, discovery, now_ms);
    }
}

/// Add the additive `model_choice.availability` field to every catalogued
/// choice row (native and Pi): `available`, `unavailable` (the source no
/// longer offers it; pickers hide it) or `unknown` (no listing yet).
pub fn annotate_availability(
    router: &LLMRouterConfig,
    rows: &mut [Value],
    discovery: &ModelDiscovery,
    now_ms: i64,
) {
    for row in rows.iter_mut() {
        let Some(choice) = row
            .get_mut("model_choice")
            .filter(|choice| choice.is_object())
        else {
            continue;
        };
        if choice.get("availability").is_some() {
            continue;
        }
        let Some(id) = choice["id"].as_str() else {
            continue;
        };
        // Native ids are `native/<engine>/<catalog id>`; Pi ids are catalog ids.
        let catalog_id = id
            .strip_prefix("native/")
            .and_then(|rest| rest.split_once('/'))
            .map(|(_, catalog_id)| catalog_id)
            .unwrap_or(id);
        if let Some(view) = discovery.view(&router.catalog, catalog_id, now_ms) {
            choice["availability"] = json!(view);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magicllm::model_discovery::ListingOrigin;

    fn router() -> LLMRouterConfig {
        let mut router = LLMRouterConfig::default();
        router.catalog = serde_json::from_value(json!({
            "connections": {"harness-agy": {"provider": "harness-agy"}},
            "models": {
                "harness-agy/default": {
                    "connection": "harness-agy", "model": "default", "label": "Default"
                },
                "harness-agy/gemini-3.7-flash-high": {
                    "connection": "harness-agy", "model": "gemini-3.7-flash-high",
                    "label": "Flash High",
                    "coding_thinking": {"mode": "effort", "levels": ["low", "high"], "default": "low"}
                }
            }
        }))
        .unwrap();
        router
    }

    #[test]
    fn withdrawn_models_are_marked_and_offered_ones_join_as_uncatalogued() {
        let router = router();
        let mut discovery = ModelDiscovery::default();
        discovery.record_listing(
            "harness-agy",
            ["gemini-3.8-flash-high"],
            ListingOrigin::CliListing,
            1,
        );
        let models = models_in(Some(&router), &discovery);
        let withdrawn = models
            .iter()
            .find(|model| model.choice.model == "gemini-3.7-flash-high")
            .unwrap();
        assert!(!withdrawn.offered_in(&discovery, 1));
        let new = models
            .iter()
            .find(|model| model.choice.model == "gemini-3.8-flash-high")
            .unwrap();
        assert!(new.uncatalogued && new.offered_in(&discovery, 1));
        assert_eq!(new.engine, "agy_cli");
        assert_eq!(
            new.choice.id,
            "native/agy_cli/harness-agy/gemini-3.8-flash-high"
        );
        // Default request shape: the CLI's own effort, no thinking levels.
        assert_eq!(new.choice.choices.keys().collect::<Vec<_>>(), vec!["auto"]);
        assert!(new.choice.thinking.default.is_none());

        let mut rows = vec![json!({"id": "agy-default", "engine": "agy_cli", "selectable": true})];
        project_in(Some(&router), &mut rows, &discovery, 1);
        let choice_of = |model: &str| {
            rows.iter()
                .find(|row| row["model_choice"]["model"] == model)
                .map(|row| row["model_choice"].clone())
                .unwrap()
        };
        assert_eq!(
            choice_of("gemini-3.7-flash-high")["availability"]["state"],
            "unavailable"
        );
        let uncatalogued = choice_of("gemini-3.8-flash-high");
        assert_eq!(uncatalogued["availability"]["state"], "available");
        assert_eq!(uncatalogued["availability"]["catalogued"], false);
        assert_eq!(choice_of("default")["availability"]["state"], "available");
    }

    #[test]
    fn without_a_listing_catalog_models_stay_offered() {
        let router = router();
        let discovery = ModelDiscovery::default();
        let models = models_in(Some(&router), &discovery);
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|model| model.offered_in(&discovery, 1)));
        assert!(models.iter().all(|model| !model.uncatalogued));
    }
}
