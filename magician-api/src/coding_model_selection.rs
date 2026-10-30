//! Public coding selections carry engine/model/effort, never an authored alias.
//! Generated routes are an internal admission detail; the execution owner still
//! checks eligibility and seals the selected engine in its dispatch digest.

use magician::magician_v2::{
    execution::coding_engine::{selection::engine_str, CodingEngineKind},
    vibedev::run_service::ClientVibeDevCodingChoice,
};
use serde::{de::Error, Deserialize, Deserializer};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodingSelection {
    kind: String,
    engine: Option<CodingEngineKind>,
    model_id: Option<String>,
    thinking: Option<String>,
}

fn component(value: Option<String>, field: &str) -> Result<String, String> {
    let value = value.ok_or_else(|| format!("coding_choice.{field} is required"))?;
    if value.is_empty()
        || value.len() > 512
        || value.trim() != value
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(format!("Invalid coding_choice.{field}"));
    }
    Ok(value)
}

impl CodingSelection {
    fn into_choice(self) -> Result<ClientVibeDevCodingChoice, String> {
        match self.kind.as_str() {
            "auto"
                if self.engine.is_none() && self.model_id.is_none() && self.thinking.is_none() =>
            {
                Ok(ClientVibeDevCodingChoice::Auto)
            },
            "model" => {
                let engine = self.engine.ok_or("coding_choice.engine is required")?;
                let model = component(self.model_id, "model_id")?;
                let thinking = component(self.thinking, "thinking")?;
                if thinking.contains('/') {
                    return Err("Invalid coding_choice.thinking".into());
                }
                let route = match engine {
                    CodingEngineKind::Pi if !model.starts_with("native/") => {
                        format!("pi/model/{model}/{thinking}")
                    },
                    CodingEngineKind::Pi => {
                        return Err("The coding model belongs to a different engine".into());
                    },
                    _ if model.starts_with(&format!("native/{}/", engine_str(engine))) => {
                        format!("{model}/{thinking}")
                    },
                    _ => return Err("The coding model belongs to a different engine".into()),
                };
                Ok(ClientVibeDevCodingChoice::Profile { profile_id: route })
            },
            _ => Err(
                "Use coding_choice kind auto or model with engine, model_id and thinking".into(),
            ),
        }
    }
}

pub fn parse_choice(value: Value) -> Result<ClientVibeDevCodingChoice, String> {
    serde_json::from_value::<CodingSelection>(value)
        .map_err(|error| error.to_string())?
        .into_choice()
}

pub fn deserialize_optional_choice<'de, D>(
    deserializer: D,
) -> Result<Option<ClientVibeDevCodingChoice>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<CodingSelection>::deserialize(deserializer)?
        .map(CodingSelection::into_choice)
        .transpose()
        .map_err(D::Error::custom)
}

/// Project only model descriptors backed by selectable concrete routes. Engine
/// readiness is independent of model identity and remains visible when blocked.
pub fn model_catalog(rows: &[Value]) -> Value {
    let mut engines = BTreeMap::<String, Value>::new();
    let mut models = BTreeMap::<(String, String), Value>::new();
    for row in rows {
        let Some(engine) = row["engine"].as_str() else {
            continue;
        };
        engines.entry(engine.to_owned()).or_insert_with(|| {
            let mut status = json!({"id":engine});
            for key in ["selectable", "readiness", "reason", "gateway", "version"] {
                if let Some(value) = row.get(key) {
                    status[key] = value.clone();
                }
            }
            if let Some(label) = magician::magician_v2::harness_auth::credential_label(engine) {
                status["credentials"] = json!(label);
            }
            status
        });
        if row["selectable"] == false {
            continue;
        }
        let Some(mut model) = row
            .get("model_choice")
            .filter(|value| value.is_object())
            .cloned()
        else {
            continue;
        };
        // A model its source no longer offers is hidden from every picker
        // (web, iOS, Android read this one list).
        if model["availability"]["state"] == "unavailable" {
            continue;
        }
        let Some(id) = model["id"].as_str().map(str::to_owned) else {
            continue;
        };
        let Some(choices) = model["choices"].as_object_mut() else {
            continue;
        };
        choices.retain(|thinking, route| {
            rows.iter().any(|candidate| {
                candidate["engine"] == engine
                    && candidate["selectable"] != false
                    && candidate["id"] == *route
                    && candidate["model_selection"]["model_id"] == id
                    && candidate["model_selection"]["thinking"] == *thinking
            })
        });
        if choices.is_empty() {
            continue;
        }
        model["engine"] = json!(engine);
        // Native SDK/CLI usable capacity is not attested by this API catalog.
        model["limits"] = Value::Null;
        models.insert((engine.to_owned(), id), model);
    }
    let default_selection = rows.iter().find_map(|row| {
        if row["is_default"] != true || row["selectable"] == false {
            return None;
        }
        let engine = row["engine"].as_str()?;
        let selection = &row["model_selection"];
        let id = selection["model_id"].as_str()?;
        let thinking = selection["thinking"].as_str()?;
        models.get(&(engine.to_owned(), id.to_owned()))?["choices"].get(thinking)?;
        Some(json!({"kind":"model","engine":engine,"model_id":id,"thinking":thinking}))
    });
    json!({"models":models.into_values().collect::<Vec<_>>(),"engines":engines.into_values().collect::<Vec<_>>(),"default_selection":default_selection})
}

/// The coding catalog plus runtime discovery: per-source listing status and
/// the published catalog drift. Both fields are additive.
pub fn model_catalog_with_discovery(rows: &[Value]) -> Value {
    let mut catalog = model_catalog(rows);
    catalog["model_sources"] = json!(magicllm::model_discovery::source_statuses());
    catalog["catalog_drift"] = json!(magicllm::model_discovery::published_drift().1);
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_choices_pin_engine_and_effort_without_alias_resolution() {
        for (engine, model, expected) in [
            ("pi", "openai/sol", "pi/model/openai/sol/high"),
            (
                "codex_app_server",
                "native/codex_app_server/harness/sol",
                "native/codex_app_server/harness/sol/high",
            ),
            (
                "claude_code",
                "native/claude_code/harness/opus",
                "native/claude_code/harness/opus/high",
            ),
            (
                "grok_acp",
                "native/grok_acp/harness/grok",
                "native/grok_acp/harness/grok/high",
            ),
            (
                "agy_cli",
                "native/agy_cli/harness/gemini",
                "native/agy_cli/harness/gemini/high",
            ),
        ] {
            assert_eq!(
                parse_choice(
                    json!({"kind":"model","engine":engine,"model_id":model,"thinking":"high"})
                )
                .unwrap(),
                ClientVibeDevCodingChoice::Profile {
                    profile_id: expected.into()
                }
            );
        }
        assert_eq!(
            parse_choice(json!({"kind":"auto"})).unwrap(),
            ClientVibeDevCodingChoice::Auto
        );
        assert_eq!(
            parse_choice(
                json!({"kind":"model","engine":"pi","model_id":"openai/sol","thinking":"auto"})
            )
            .unwrap(),
            ClientVibeDevCodingChoice::Profile {
                profile_id: "pi/model/openai/sol/auto".into()
            }
        );
    }

    #[test]
    fn obsolete_ambiguous_malformed_and_wrong_engine_choices_are_refused() {
        for value in [
            json!({"kind":"profile","profile_id":"coding-premium"}),
            json!({"kind":"auto","engine":"pi"}),
            json!({"kind":"model","engine":"pi","model_id":"native/claude_code/harness/opus","thinking":"auto"}),
            json!({"kind":"model","engine":"grok_acp","model_id":"native/agy_cli/harness/gemini","thinking":"auto"}),
            json!({"kind":"model","engine":"pi","model_id":"","thinking":"auto"}),
            json!({"kind":"model","engine":"pi","model_id":"model\nredirect","thinking":"auto"}),
            json!({"kind":"model","engine":"pi","model_id":"model","thinking":"../high"}),
            json!({"kind":"model","engine":"pi","model_id":"model","thinking":"auto","profile_id":"old"}),
        ] {
            assert!(parse_choice(value).is_err());
        }
    }

    #[test]
    fn chat_and_cockpit_accept_typed_choices_and_reject_retired_fields() {
        let choice =
            json!({"kind":"model","engine":"pi","model_id":"provider/model","thinking":"high"});
        let chat: crate::chat_api::SendMessageRequest =
            serde_json::from_value(json!({"coding_choice":choice})).unwrap();
        assert_eq!(
            chat.coding_choice,
            Some(ClientVibeDevCodingChoice::Profile {
                profile_id: "pi/model/provider/model/high".into()
            })
        );
        let mut run =
            json!({"prompt":"fix it","project_id":"project","mode":"build","coding_choice":choice});
        let cockpit: crate::vibedev_api::StartVibeDevRunRequest =
            serde_json::from_value(run.clone()).unwrap();
        assert_eq!(cockpit.coding_choice, chat.coding_choice);
        run["coding_profile_id"] = json!("coding-premium");
        assert!(serde_json::from_value::<crate::vibedev_api::StartVibeDevRunRequest>(run).is_err());
        assert!(
            serde_json::from_value::<crate::chat_api::SendMessageRequest>(
                json!({"coding_choice":{"kind":"profile","profile_id":"old"}})
            )
            .is_err()
        );
    }

    #[test]
    fn catalog_has_models_and_blocked_engines_without_profile_aliases_or_unattested_limits() {
        let mut rows = vec![
            json!({"id":"old-alias","engine":"pi","is_default":true,"model_selection":{"model_id":"provider/model","thinking":"high"}}),
            json!({"id":"pi/model/provider/model/auto","engine":"pi","selectable":true,"model_selection":{"model_id":"provider/model","thinking":"auto"},
                "model_choice":{"id":"provider/model","limits":{"context_window_tokens":123},"choices":{"auto":"pi/model/provider/model/auto","high":"pi/model/provider/model/high","none":"unqualified"}}}),
            json!({"id":"pi/model/provider/model/high","engine":"pi","selectable":true,"model_selection":{"model_id":"provider/model","thinking":"high"}}),
            json!({"id":"claude-default","engine":"claude_code","selectable":false,"readiness":"sign_in_required","reason":"Sign in"}),
        ];
        let catalog = model_catalog(&rows);
        assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
        assert_eq!(
            catalog["models"][0]["choices"].as_object().unwrap().len(),
            2
        );
        assert!(catalog["models"][0]["limits"].is_null());
        assert_eq!(
            catalog["default_selection"],
            json!({"kind":"model","engine":"pi","model_id":"provider/model","thinking":"high"})
        );
        assert_eq!(catalog["engines"][0]["id"], "claude_code");
        assert_eq!(catalog["engines"][0]["reason"], "Sign in");
        assert!(!catalog.to_string().contains("old-alias"));
        rows[0]["model_selection"]["thinking"] = json!("none");
        assert!(model_catalog(&rows)["default_selection"].is_null());
    }

    #[test]
    fn models_their_source_no_longer_offers_leave_the_picker() {
        let mut rows = vec![
            json!({"id":"native/agy_cli/harness-agy/old/auto","engine":"agy_cli","selectable":true,
                "model_selection":{"model_id":"native/agy_cli/harness-agy/old","thinking":"auto"},
                "model_choice":{"id":"native/agy_cli/harness-agy/old","choices":{"auto":"native/agy_cli/harness-agy/old/auto"},
                    "availability":{"state":"unavailable","source":"harness-agy","catalogued":true}}}),
            json!({"id":"native/agy_cli/harness-agy/new/auto","engine":"agy_cli","selectable":true,
                "model_selection":{"model_id":"native/agy_cli/harness-agy/new","thinking":"auto"},
                "model_choice":{"id":"native/agy_cli/harness-agy/new","choices":{"auto":"native/agy_cli/harness-agy/new/auto"},
                    "availability":{"state":"available","source":"harness-agy","catalogued":false}}}),
        ];
        let catalog = model_catalog(&rows);
        let models = catalog["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], "native/agy_cli/harness-agy/new");
        assert_eq!(models[0]["availability"]["catalogued"], false);
        // Unknown availability (no listing yet) keeps the model.
        rows[0]["model_choice"]["availability"]["state"] = json!("unknown");
        assert_eq!(model_catalog(&rows)["models"].as_array().unwrap().len(), 2);
        let with_discovery = model_catalog_with_discovery(&rows);
        assert!(with_discovery["model_sources"].is_array());
        assert!(with_discovery["catalog_drift"].is_array());
    }
}
