//! Prompt data structures and storage abstractions shared across crates.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Categories of prompts used by Magician V2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PromptCategory {
    QueryAnalysis,
    TaskDecomposition,
    ToolMatching,
    ParameterElicitation,
    ResponseGeneration,
    ErrorHandling,
    AtomicComposition,
    Vision,
    General,
    /// Verification prompts for semantic text matching and result validation
    Verification,
    /// Agentic execution prompts for decision-making and user input handling
    AgenticExecution,
    /// Memory consolidation prompts for episode archival, insight extraction, and summarization
    MemoryConsolidation,
    /// Autonomous execution prompts for self-directed agent cycles
    AutonomousExecution,
    /// Agent evolution prompts for meta-agent proposals and definition improvements
    AgentEvolution,
    /// Taskplan execution prompts for plan generation and updates
    TaskplanExecution,
    /// Chat prompts for conversational mode
    Chat,
    /// Conversational/dialog prompts — including realtime voice
    /// session prompts (modality addendum, speech-tag instructions,
    /// task-completion announcements). Voice prompts use this rather
    /// than `Chat` so the category telemetry distinguishes voice rails
    /// from text-chat templates.
    Conversational,
    /// Post-run learning reflection prompts (episode → durable proposals)
    Learning,
    /// API mining prompts (CapabilitySequence merge into WorkflowGraph)
    ApiMining,
    /// Channel-assist prompts — the mail/chat observe+assist pipeline
    /// (local ingest distillation and thread classification). Kept as a
    /// distinct category so pipeline-prompt telemetry is separable from
    /// general response generation.
    ChannelAssist,
    /// Fleet social-network gate and composition prompts. This keeps social
    /// policy and publishing telemetry distinct from general chat generation.
    Social,
}

/// Variable that can be referenced within a prompt template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptVariable {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    pub default_value: Option<String>,
    #[serde(default)]
    pub examples: Vec<String>,
}

/// Metadata associated with a prompt version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptMetadata {
    pub category: PromptCategory,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub changelog: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub estimated_tokens: Option<u32>,
}

/// Versioned prompt definition with templated content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub name: String,
    pub version: String,
    #[serde(deserialize_with = "deserialize_content")]
    pub content: String,
    #[serde(deserialize_with = "deserialize_variables")]
    pub variables: Vec<PromptVariable>,
    pub metadata: PromptMetadata,
}

impl Prompt {
    pub fn new(
        name: String,
        version: String,
        content: String,
        category: PromptCategory,
        description: String,
        author: String,
    ) -> Self {
        Self {
            name,
            version,
            content,
            variables: Vec::new(),
            metadata: PromptMetadata {
                category,
                description,
                author,
                created_at: Utc::now(),
                changelog: String::new(),
                tags: Vec::new(),
                estimated_tokens: None,
            },
        }
    }

    pub fn add_variable(mut self, variable: PromptVariable) -> Self {
        self.variables.push(variable);
        self
    }

    pub fn add_tag(mut self, tag: String) -> Self {
        self.metadata.tags.push(tag);
        self
    }

    pub fn with_changelog(mut self, changelog: String) -> Self {
        self.metadata.changelog = changelog;
        self
    }

    pub fn with_estimated_tokens(mut self, tokens: u32) -> Self {
        self.metadata.estimated_tokens = Some(tokens);
        self
    }

    /// Render the prompt by substituting named placeholders.
    pub fn render(&self, variables: &HashMap<String, String>) -> Result<String> {
        let mut result = self.content.clone();
        for (key, value) in variables {
            let placeholder = format!("{{{}}}", key);
            result = result.replace(&placeholder, value);
        }

        // Apply default_value for variables not explicitly provided, then enforce required.
        for variable in &self.variables {
            if !variables.contains_key(&variable.name) {
                if let Some(ref default) = variable.default_value {
                    let placeholder = format!("{{{}}}", variable.name);
                    result = result.replace(&placeholder, default);
                } else if variable.required {
                    return Err(anyhow::anyhow!(
                        "Missing required prompt variable: {}",
                        variable.name
                    ));
                }
            }
        }

        Ok(result)
    }
}

fn deserialize_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use std::fmt;

    use serde::de::{self, Visitor};

    struct ContentVisitor;

    impl<'de> Visitor<'de> for ContentVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or array of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<String, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut lines = Vec::new();
            while let Some(line) = seq.next_element::<String>()? {
                lines.push(line);
            }
            Ok(lines.join("\n"))
        }
    }

    deserializer.deserialize_any(ContentVisitor)
}

/// Deserialize `variables` as either an array of `PromptVariable` structs or an array of
/// plain strings (shorthand: each string becomes a required variable with that name).
fn deserialize_variables<'de, D>(deserializer: D) -> Result<Vec<PromptVariable>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use std::fmt;

    use serde::de::{self, SeqAccess, Visitor};

    struct VariablesVisitor;

    impl<'de> Visitor<'de> for VariablesVisitor {
        type Value = Vec<PromptVariable>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an array of PromptVariable objects or plain strings")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Vec<PromptVariable>, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut vars = Vec::new();
            while let Some(item) = seq.next_element::<serde_json::Value>()? {
                match item {
                    serde_json::Value::String(name) => {
                        vars.push(PromptVariable {
                            name,
                            description: String::new(),
                            required: true,
                            default_value: None,
                            examples: Vec::new(),
                        });
                    },
                    serde_json::Value::Object(_) => {
                        let pv: PromptVariable =
                            serde_json::from_value(item).map_err(de::Error::custom)?;
                        vars.push(pv);
                    },
                    other => {
                        return Err(de::Error::custom(format!(
                            "expected string or object in variables array, got {}",
                            other
                        )));
                    },
                }
            }
            Ok(vars)
        }
    }

    deserializer.deserialize_seq(VariablesVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_prompt(content: &str, variables: Vec<PromptVariable>) -> Prompt {
        Prompt {
            name: "test".into(),
            version: "1.0.0".into(),
            content: content.into(),
            variables,
            metadata: PromptMetadata {
                category: PromptCategory::General,
                description: String::new(),
                author: String::new(),
                created_at: Utc::now(),
                changelog: String::new(),
                tags: Vec::new(),
                estimated_tokens: None,
            },
        }
    }

    #[test]
    fn render_substitutes_provided_variables() {
        let prompt = make_prompt("Hello {name}!", vec![]);
        let vars = HashMap::from([("name".into(), "World".into())]);
        assert_eq!(prompt.render(&vars).unwrap(), "Hello World!");
    }

    #[test]
    fn render_applies_default_value_when_variable_not_provided() {
        let prompt = make_prompt(
            "identity: {identity_section}",
            vec![PromptVariable {
                name: "identity_section".into(),
                description: String::new(),
                required: false,
                default_value: Some("default-identity".into()),
                examples: Vec::new(),
            }],
        );
        let vars = HashMap::new();
        assert_eq!(prompt.render(&vars).unwrap(), "identity: default-identity");
    }

    #[test]
    fn render_errors_on_missing_required_variable_without_default() {
        let prompt = make_prompt(
            "schema: {tier_schema}",
            vec![PromptVariable {
                name: "tier_schema".into(),
                description: String::new(),
                required: true,
                default_value: None,
                examples: Vec::new(),
            }],
        );
        let vars = HashMap::new();
        let err = prompt.render(&vars).unwrap_err();
        assert!(err
            .to_string()
            .contains("Missing required prompt variable: tier_schema"));
    }

    #[test]
    fn render_required_variable_with_default_uses_default() {
        let prompt = make_prompt(
            "count: {episode_count}",
            vec![PromptVariable {
                name: "episode_count".into(),
                description: String::new(),
                required: true,
                default_value: Some("0".into()),
                examples: Vec::new(),
            }],
        );
        let vars = HashMap::new();
        assert_eq!(prompt.render(&vars).unwrap(), "count: 0");
    }

    #[test]
    fn render_provided_value_overrides_default() {
        let prompt = make_prompt(
            "count: {episode_count}",
            vec![PromptVariable {
                name: "episode_count".into(),
                description: String::new(),
                required: true,
                default_value: Some("0".into()),
                examples: Vec::new(),
            }],
        );
        let vars = HashMap::from([("episode_count".into(), "42".into())]);
        assert_eq!(prompt.render(&vars).unwrap(), "count: 42");
    }
}

/// Async prompt store abstraction.
#[async_trait]
pub trait PromptStore: Send + Sync {
    async fn get_prompt(&self, name: &str, version: &str) -> Result<Prompt>;
    async fn list_versions(&self, name: &str) -> Result<Vec<String>>;
    async fn list_prompt_names(&self) -> Result<Vec<String>>;
    async fn save_prompt(&self, prompt: &Prompt) -> Result<()>;
    async fn prompt_exists(&self, name: &str, version: &str) -> Result<bool>;
    async fn latest_version(&self, name: &str) -> Result<String>;
    async fn delete_prompt(&self, name: &str, version: &str) -> Result<()>;
    async fn initialize(&self) -> Result<()>;
    async fn health_check(&self) -> Result<bool>;
}
