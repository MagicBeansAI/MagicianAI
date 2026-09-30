//! Presentation identities shared by assistant-facing backend surfaces.
//!
//! The `magician` service key is deliberately absent from these types. It is
//! an operational identifier for health, logs, config, and process control;
//! it is not an assistant, product, or agent name.

use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::types::AgentDefinition;

include!("presentation_identity_generated.rs");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresentationIdentity {
    pub product_name: String,
    pub assistant_fallback: String,
    pub host_app_name: String,
}

impl Default for PresentationIdentity {
    fn default() -> Self {
        Self {
            product_name: PRODUCT_NAME.to_string(),
            assistant_fallback: ASSISTANT_FALLBACK_NAME.to_string(),
            host_app_name: HOST_APP_NAME.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPresentationIdentity {
    pub agent_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// In-lexicon spellings the on-device wake spotter arms instead of the
    /// advertised names. See `AgentDefinition::wake_spellings`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_spellings: Vec<String>,
    pub is_primary: bool,
}

impl AgentPresentationIdentity {
    pub fn fallback() -> Self {
        Self {
            agent_id: String::new(),
            name: ASSISTANT_FALLBACK_NAME.to_string(),
            aliases: Vec::new(),
            wake_spellings: Vec::new(),
            is_primary: false,
        }
    }

    pub fn from_definition(definition: &AgentDefinition) -> Self {
        let name = non_empty(&definition.name)
            .unwrap_or(ASSISTANT_FALLBACK_NAME)
            .to_string();
        let aliases = bounded_agent_aliases(&definition.aliases);
        let wake_spellings = bounded_agent_aliases(&definition.wake_spellings);
        Self {
            agent_id: definition.agent_id.clone(),
            name,
            aliases,
            wake_spellings,
            is_primary: definition.is_primary,
        }
    }
}

pub fn bounded_agent_aliases(aliases: &[String]) -> Vec<String> {
    aliases
        .iter()
        .filter_map(|alias| non_empty(alias).map(str::to_string))
        .take(8)
        .collect()
}

/// How the agent refers to its owner, from the operator identity layer
/// (`MAGICIAN_OWNER_NAME`, written by `make setup-identity`). Falls back to
/// the neutral phrase "the owner" — never to a personal name.
pub fn owner_name() -> String {
    std::env::var("MAGICIAN_OWNER_NAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "the owner".to_string())
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn presentation_defaults_do_not_use_backend_service_identity() {
        let identity = PresentationIdentity::default();
        assert_eq!(identity.product_name, PRODUCT_NAME);
        assert_eq!(identity.assistant_fallback, ASSISTANT_FALLBACK_NAME);
        assert_eq!(identity.host_app_name, HOST_APP_NAME);
        assert!(!serde_json::to_string(&identity)
            .expect("serialize presentation identity")
            .to_lowercase()
            .contains("magician"));
    }

    #[test]
    fn agent_aliases_are_trimmed_filtered_and_bounded() {
        let aliases = vec![
            "  Sam  ", "", "Presto", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();

        assert_eq!(
            bounded_agent_aliases(&aliases),
            vec!["Sam", "Presto", "One", "Two", "Three", "Four", "Five", "Six"]
        );
    }

    #[test]
    fn fallback_identity_is_explicit_and_has_no_invented_agent_id() {
        assert_eq!(
            AgentPresentationIdentity::fallback(),
            AgentPresentationIdentity {
                agent_id: String::new(),
                name: ASSISTANT_FALLBACK_NAME.to_string(),
                aliases: Vec::new(),
                wake_spellings: Vec::new(),
                is_primary: false,
            }
        );
    }
}
