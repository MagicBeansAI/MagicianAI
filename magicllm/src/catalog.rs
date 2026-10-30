//! Provider connections and model capabilities, independent of operation choices.
//!
//! Catalog configuration is compiled into the existing internal route contract.
//! This keeps pricing, physical admission, replay and dispatch on one path while
//! removing the need to author one profile per effort/output/tool combination.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    config::{ChunkingConfig, LLMProfile, LLMRouterConfig, ReasoningDefaults},
    error::{LLMError, LLMResult},
    types::{LLMRequest, ReasoningConfig},
    LLMModality, LLMProviderKind,
};

/// Local resolver data. Never forwarded to a provider.
pub const CATALOG_SETTINGS_KEY: &str = "_magician_model_settings";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelCatalog {
    pub connections: BTreeMap<String, ProviderConnection>,
    pub models: BTreeMap<String, ModelDefinition>,
    pub policies: BTreeMap<String, WorkloadPolicy>,
    pub assignments: BTreeMap<String, ModelAssignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConnection {
    pub provider: LLMProviderKind,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_base_url: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDefinition {
    pub connection: String,
    pub model: String,
    pub label: String,
    /// Decorative selection feedback, independent of provider or route names.
    #[serde(default)]
    pub feedback: ModelFeedback,
    /// Omitted for opaque harness defaults whose physical model is unknown.
    /// Such entries cannot appear as qualified API model choices.
    #[serde(default)]
    pub limits: Option<ModelTokenLimits>,
    #[serde(default)]
    pub thinking: ThinkingControl,
    /// Native coding CLIs can expose different controls from one-shot dispatch.
    /// Resolver-only metadata: never sent as provider request fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding_thinking: Option<ThinkingControl>,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// Request-shape facts provider adapters need for this exact model.
    /// Resolver data: carried to the adapter as [`ModelWire`], never sent.
    #[serde(default, skip_serializing_if = "ModelWireTraits::is_default")]
    pub wire: ModelWireTraits,
    #[serde(default = "yes")]
    pub selectable: bool,
    #[serde(default)]
    pub embedding: bool,
    /// This exact provider/model has been qualified for the Pi coding engine.
    #[serde(default)]
    pub coding: bool,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

const fn yes() -> bool {
    true
}
const fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFeedback {
    Fast,
    #[default]
    Standard,
    Premium,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelTokenLimits {
    /// Effective physical window, including a local runner's configured limit.
    pub context_window_tokens: u32,
    #[serde(default)]
    pub max_input_tokens: Option<u32>,
    #[serde(default = "one")]
    pub min_output_tokens: u32,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub default_output_tokens: Option<u32>,
    #[serde(default = "yes")]
    pub shared_context_window: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingMode {
    #[default]
    None,
    Effort,
    Toggle,
    Budget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThinkingControl {
    pub mode: ThinkingMode,
    /// Native ordered values. `none` is present only when Off is qualified.
    pub levels: Vec<String>,
    pub default: Option<String>,
    pub budget: Option<ThinkingBudgetLimits>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingBudgetLimits {
    pub min_tokens: u32,
    pub max_tokens: u32,
    pub step_tokens: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelCapabilities {
    pub vision: bool,
    pub tools: bool,
    pub computer_use: bool,
    pub structured_output: bool,
    pub streaming: bool,
    pub web_search: bool,
}

/// How an enabled thinking request is shaped on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingWire {
    /// The model sizes its own thinking from an effort hint (Anthropic:
    /// `thinking.type = adaptive` + `output_config.effort`).
    Adaptive,
    /// An explicit thinking token budget (Anthropic: `thinking.type =
    /// enabled` + `budget_tokens`).
    Budget,
}

/// Authored request-shape facts for one model (`models.<id>.wire`). Every
/// field is optional; omitted fields mean the common shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelWireTraits {
    /// Overrides the shape derived from `thinking.mode` (`effort` →
    /// `adaptive`; `budget` / `toggle` → `budget`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingWire>,
    /// `false`: the model rejects `temperature` / `top_p`, so adapters omit
    /// them instead of sending a request that returns 400.
    pub sampling: bool,
    /// Effort sent with thinking Off, for a model that rejects Off combined
    /// with an inherited higher effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_off_effort: Option<String>,
}

impl Default for ModelWireTraits {
    fn default() -> Self {
        Self {
            thinking: None,
            sampling: true,
            thinking_off_effort: None,
        }
    }
}

impl ModelWireTraits {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Resolve the authored facts with the model's thinking control.
    pub fn resolve(&self, thinking: &ThinkingControl) -> ModelWire {
        ModelWire {
            thinking: match thinking.mode {
                ThinkingMode::None => None,
                ThinkingMode::Effort => Some(self.thinking.unwrap_or(ThinkingWire::Adaptive)),
                ThinkingMode::Budget | ThinkingMode::Toggle => {
                    Some(self.thinking.unwrap_or(ThinkingWire::Budget))
                },
            },
            thinking_off: thinking.levels.iter().any(|level| level == "none"),
            thinking_off_effort: self.thinking_off_effort.clone(),
            sampling: self.sampling,
        }
    }
}

/// Request-shape facts of the catalogued model a request resolved to,
/// handed to provider adapters on [`LLMRequest::model_wire`]. A request
/// without it targets an uncatalogued model, and each adapter falls back to
/// its own model-name rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelWire {
    /// `None`: the model has no adjustable thinking.
    pub thinking: Option<ThinkingWire>,
    /// The model accepts an explicit thinking-off request (its catalog
    /// `thinking.levels` include `none`).
    pub thinking_off: bool,
    pub thinking_off_effort: Option<String>,
    pub sampling: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkloadPolicy {
    pub output_tokens: u32,
    pub timeout_secs: u64,
    pub thinking: Option<String>,
    /// Reserve visible output when translating legacy thinking budgets.
    pub answer_reserve_tokens: u32,
    /// Optional operation admission ceiling below the model's capacity.
    pub context_window_tokens: Option<u32>,
}

impl Default for WorkloadPolicy {
    fn default() -> Self {
        Self {
            output_tokens: 16_384,
            timeout_secs: 600,
            thinking: None,
            answer_reserve_tokens: 2_048,
            context_window_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAssignment {
    pub model: String,
    /// Reviewed alternate transport of the same provider, used by migration.
    #[serde(default)]
    pub connection: Option<String>,
    #[serde(default = "auto")]
    pub thinking: String,
    #[serde(default)]
    pub policy: Option<String>,
    /// Explicit error fallback to another catalog assignment. Model choices
    /// themselves do not inherit an operation's fallback permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Operation-specific transport requirements, not model capability facts.
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
    #[serde(default)]
    pub chunking: Option<ChunkingConfig>,
    #[serde(default)]
    pub modality: Option<LLMModality>,
    /// Explicit restrictions survive legacy and sealed-route migration.
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub vision: Option<bool>,
    #[serde(default)]
    pub tools: Option<bool>,
    #[serde(default)]
    pub computer_use: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f32>,
}

fn auto() -> String {
    "auto".into()
}

impl ModelAssignment {
    pub fn choice(model: &str, thinking: &str) -> Self {
        Self {
            model: model.into(),
            connection: None,
            thinking: thinking.into(),
            policy: None,
            fallback: None,
            metadata: HashMap::new(),
            chunking: None,
            modality: None,
            reasoning: None,
            vision: None,
            tools: None,
            computer_use: None,
            temperature: None,
        }
    }
}

/// One row per model, shared by all clients. Route IDs are opaque generated
/// handles; clients display label/model and native thinking choices separately.
#[derive(Debug, Clone, Serialize)]
pub struct ModelChoice {
    pub id: String,
    pub provider: String,
    pub connection: String,
    pub model: String,
    pub label: String,
    pub feedback: ModelFeedback,
    pub capabilities: ModelCapabilities,
    pub limits: Option<ModelTokenLimits>,
    pub thinking: ThinkingControl,
    pub choices: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub model_id: String,
    #[serde(default = "auto")]
    pub thinking: String,
}

/// Decode the selected model/effort from generated route metadata.
/// This is local metadata; clients never need to parse an opaque route ID.
pub fn selection_for_profile(profile: &LLMProfile) -> Option<ModelSelection> {
    profile
        .metadata
        .as_ref()?
        .get(CATALOG_SETTINGS_KEY)?
        .get("selection")
        .and_then(|selection| serde_json::from_value(selection.clone()).ok())
}

pub fn choice_id(model_id: &str, thinking: &str) -> String {
    format!("model/{model_id}/{thinking}")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200 && !id.chars().any(char::is_control)
}

fn config_error(message: impl Into<String>) -> LLMError {
    LLMError::Configuration(message.into())
}

impl ModelCatalog {
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
            && self.models.is_empty()
            && self.assignments.is_empty()
            && self.policies.is_empty()
    }

    pub fn choices(&self) -> Vec<ModelChoice> {
        self.models
            .iter()
            .filter(|(_, model)| model.selectable && !model.embedding)
            .map(|(id, model)| {
                let mut choices = BTreeMap::from([("auto".into(), choice_id(id, "auto"))]);
                for level in &model.thinking.levels {
                    choices.insert(level.clone(), choice_id(id, level));
                }
                ModelChoice {
                    id: id.clone(),
                    provider: self.connections[&model.connection].provider.to_string(),
                    connection: model.connection.clone(),
                    model: model.model.clone(),
                    label: model.label.clone(),
                    feedback: model.feedback,
                    capabilities: model.capabilities.clone(),
                    limits: model.limits.clone(),
                    thinking: model.thinking.clone(),
                    choices,
                }
            })
            .collect()
    }

    pub fn validate(&self) -> LLMResult<()> {
        for (id, connection) in &self.connections {
            if !valid_id(id) {
                return Err(config_error(format!(
                    "invalid provider connection ID `{id}`"
                )));
            }
            if connection.max_output_tokens == Some(0) {
                return Err(config_error(format!(
                    "connection `{id}` has zero output capacity"
                )));
            }
            validate_metadata(&connection.metadata)?;
        }
        for (id, policy) in &self.policies {
            if !valid_id(id)
                || policy.output_tokens == 0
                || policy.timeout_secs == 0
                || policy.answer_reserve_tokens >= policy.output_tokens
                || policy.context_window_tokens == Some(0)
            {
                return Err(config_error(format!("invalid workload policy `{id}`")));
            }
        }
        for (id, model) in &self.models {
            if !valid_id(id) || model.model.trim().is_empty() || model.label.trim().is_empty() {
                return Err(config_error(format!("invalid model catalog entry `{id}`")));
            }
            if !self.connections.contains_key(&model.connection) {
                return Err(config_error(format!(
                    "model `{id}` references unknown connection `{}`",
                    model.connection
                )));
            }
            if model.coding && (model.embedding || !model.selectable || !model.capabilities.tools) {
                return Err(config_error(format!(
                    "coding model `{id}` must be selectable, tool-capable and non-embedding"
                )));
            }
            if let Some(limits) = &model.limits {
                if limits.context_window_tokens == 0
                    || limits.min_output_tokens == 0
                    || limits.max_output_tokens < limits.min_output_tokens
                    || limits.max_input_tokens == Some(0)
                    || limits.default_output_tokens.is_some_and(|n| {
                        n < limits.min_output_tokens || n > limits.max_output_tokens
                    })
                    || (limits.shared_context_window
                        && limits.max_output_tokens > limits.context_window_tokens)
                {
                    return Err(config_error(format!(
                        "invalid token limits for model `{id}`"
                    )));
                }
            }
            if let Some(control) = &model.coding_thinking {
                if control.mode != ThinkingMode::Effort
                    || control.levels.is_empty()
                    || control.budget.is_some()
                    || !control
                        .default
                        .as_ref()
                        .is_some_and(|value| control.levels.contains(value))
                    || control.levels.iter().any(|value| {
                        !matches!(value.as_str(), "low" | "medium" | "high" | "xhigh" | "max")
                    })
                    || control
                        .levels
                        .iter()
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        != control.levels.len()
                {
                    return Err(config_error(format!(
                        "invalid native coding thinking controls for model `{id}`"
                    )));
                }
            }
            let thinking = &model.thinking;
            let mut seen = std::collections::HashSet::new();
            for level in &thinking.levels {
                if !matches!(
                    level.as_str(),
                    "none"
                        | "minimal"
                        | "low"
                        | "medium"
                        | "high"
                        | "xhigh"
                        | "max"
                        | "ultra"
                        | "on"
                ) || !seen.insert(level)
                {
                    return Err(config_error(format!(
                        "invalid or duplicate thinking level `{level}` for model `{id}`"
                    )));
                }
            }
            if thinking.mode == ThinkingMode::None {
                if !thinking.levels.is_empty()
                    || thinking.default.is_some()
                    || thinking.budget.is_some()
                {
                    return Err(config_error(format!(
                        "non-configurable model `{id}` declares thinking controls"
                    )));
                }
            } else if thinking.levels.is_empty()
                || !thinking
                    .default
                    .as_ref()
                    .is_some_and(|d| thinking.levels.contains(d))
            {
                return Err(config_error(format!(
                    "model `{id}` needs a supported default thinking level"
                )));
            }
            if thinking.mode == ThinkingMode::Toggle
                && thinking.levels.iter().any(|v| v != "none" && v != "on")
            {
                return Err(config_error(format!(
                    "binary thinking model `{id}` accepts only none/on"
                )));
            }
            match (&thinking.budget, thinking.mode) {
                (Some(b), ThinkingMode::Budget)
                    if b.min_tokens > 0 && b.min_tokens <= b.max_tokens && b.step_tokens > 0 => {},
                (None, mode) if mode != ThinkingMode::Budget => {},
                _ => {
                    return Err(config_error(format!(
                        "invalid thinking budget limits for model `{id}`"
                    )))
                },
            }
            if model.wire.thinking.is_some() && thinking.mode == ThinkingMode::None {
                return Err(config_error(format!(
                    "model `{id}` declares a thinking wire shape without thinking controls"
                )));
            }
            if let Some(effort) = &model.wire.thinking_off_effort {
                if !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max")
                    || !thinking.levels.iter().any(|level| level == "none")
                {
                    return Err(config_error(format!(
                        "model `{id}` thinking_off_effort needs an effort level and a supported Off"
                    )));
                }
            }
            validate_metadata(&model.metadata)?;
        }
        // Assignment-only references make fallback policy independent of old
        // selector aliases. Validate the complete chain before any dispatch.
        for (id, assignment) in &self.assignments {
            validate_metadata(&assignment.metadata)?;
            if let Some(target) = &assignment.fallback {
                if !self.assignments.contains_key(target) {
                    return Err(config_error(format!(
                        "assignment `{id}` references unknown fallback assignment `{target}`"
                    )));
                }
            }
        }
        let mut complete = std::collections::HashSet::new();
        for start in self.assignments.keys() {
            let mut visiting = std::collections::HashSet::new();
            let mut current = start.as_str();
            while !complete.contains(current) {
                if !visiting.insert(current) {
                    return Err(config_error(format!(
                        "cyclic assignment fallback at `{current}`"
                    )));
                }
                match self.assignments[current].fallback.as_deref() {
                    Some(target) => current = target,
                    None => break,
                }
            }
            complete.extend(visiting);
        }
        Ok(())
    }

    /// Compile model choices and workload assignments once per config snapshot.
    /// Saved legacy IDs are handled by the offline/client migration owners.
    pub fn materialize(&self, router: &mut LLMRouterConfig) -> LLMResult<()> {
        self.validate()?;
        for (id, model) in &self.models {
            let levels =
                std::iter::once("auto").chain(model.thinking.levels.iter().map(String::as_str));
            for level in levels {
                let name = choice_id(id, level);
                let mut assignment = ModelAssignment::choice(id, level);
                if model.capabilities.streaming {
                    assignment
                        .metadata
                        .insert("streaming".into(), Value::Bool(true));
                }
                self.insert_assignment(router, &name, &assignment)?;
            }
        }
        for (name, assignment) in &self.assignments {
            if !valid_id(name) {
                return Err(config_error(format!("invalid assignment ID `{name}`")));
            }
            self.insert_assignment(router, name, assignment)?;
        }
        Ok(())
    }

    fn insert_assignment(
        &self,
        router: &mut LLMRouterConfig,
        name: &str,
        assignment: &ModelAssignment,
    ) -> LLMResult<()> {
        if router.profiles.contains_key(name) {
            return Err(config_error(format!("duplicate model route `{name}`")));
        }
        let profile = self.resolve_assignment(assignment)?;
        // Auto resolves model/workload defaults once; it does not change effort
        // through an injected escalation tool.
        insert_profile(router, name, profile)
    }

    pub fn resolve_assignment(&self, assignment: &ModelAssignment) -> LLMResult<LLMProfile> {
        let model = self
            .models
            .get(&assignment.model)
            .ok_or_else(|| config_error(format!("unknown model `{}`", assignment.model)))?;
        let connection_id = assignment
            .connection
            .as_deref()
            .unwrap_or(&model.connection);
        let connection = self
            .connections
            .get(connection_id)
            .ok_or_else(|| config_error(format!("unknown connection `{connection_id}`")))?;
        if connection.provider != self.connections[&model.connection].provider {
            return Err(config_error(
                "an assignment cannot move a model to another provider",
            ));
        }
        let policy = match assignment.policy.as_deref() {
            Some(name) => self
                .policies
                .get(name)
                .cloned()
                .ok_or_else(|| config_error(format!("unknown workload policy `{name}`")))?,
            None => self.policies.get("default").cloned().unwrap_or_default(),
        };
        let requested = if assignment.thinking == "auto" {
            policy
                .thinking
                .as_deref()
                .or(model.thinking.default.as_deref())
                .unwrap_or("none")
        } else {
            assignment.thinking.as_str()
        };
        let level = resolve_level(&model.thinking, requested, assignment.thinking == "auto")?;
        if assignment.reasoning == Some(false) && level != "none" {
            return Err(config_error(
                "a reasoning-disabled assignment requires a supported Off level",
            ));
        }
        let output = model
            .limits
            .as_ref()
            .and_then(|limits| limits.default_output_tokens)
            .unwrap_or(policy.output_tokens)
            .min(
                model
                    .limits
                    .as_ref()
                    .map(|limits| limits.max_output_tokens)
                    .unwrap_or(u32::MAX),
            )
            .min(connection.max_output_tokens.unwrap_or(u32::MAX));
        if output
            < model
                .limits
                .as_ref()
                .map(|limits| limits.min_output_tokens)
                .unwrap_or(1)
        {
            return Err(config_error(format!(
                "model `{}` has no usable output allowance",
                assignment.model
            )));
        }
        let reasoning = reasoning_config(
            &model.thinking,
            &level,
            output,
            policy.answer_reserve_tokens,
        )?;
        let mut metadata = connection.metadata.clone();
        metadata.extend(model.metadata.clone());
        validate_metadata(&assignment.metadata)?;
        metadata.extend(assignment.metadata.clone());
        let mut locked_metadata = assignment
            .metadata
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(target) = &assignment.fallback {
            if !self.assignments.contains_key(target) {
                return Err(config_error(format!(
                    "unknown fallback assignment `{target}`"
                )));
            }
            // The dispatcher still consumes resolved route metadata during
            // its contract migration. Authored configuration uses `fallback`.
            metadata.insert("fallback_profile".into(), json!(target));
            locked_metadata.insert("fallback_profile");
        }
        if model.selectable && !model.embedding && model.capabilities.tools {
            metadata
                .entry("tool_choice".into())
                .or_insert_with(|| json!("auto"));
        }
        metadata.insert(CATALOG_SETTINGS_KEY.into(), json!({"model_id": assignment.model, "selection": {"model_id": assignment.model, "thinking": assignment.thinking}, "limits": model.limits, "thinking": model.thinking, "answer_reserve_tokens": policy.answer_reserve_tokens, "max_output_tokens": model.limits.as_ref().map(|limits| limits.max_output_tokens).unwrap_or(u32::MAX).min(connection.max_output_tokens.unwrap_or(u32::MAX)), "locked_metadata": locked_metadata}));
        if let Some(Value::Object(settings)) = metadata.get_mut(CATALOG_SETTINGS_KEY) {
            settings.insert("wire".into(), json!(model.wire.resolve(&model.thinking)));
        }
        let restricted = |restriction: Option<bool>, capability: bool| -> LLMResult<bool> {
            if restriction == Some(true) && !capability {
                return Err(config_error(
                    "assignment cannot enable an unsupported model capability",
                ));
            }
            Ok(restriction.unwrap_or(capability))
        };
        Ok(LLMProfile {
            provider: connection.provider.clone(),
            model: model.model.clone(),
            api_key_env: connection.api_key_env.clone(),
            api_base_url: connection.api_base_url.clone(),
            temperature: assignment.temperature,
            max_output_tokens: Some(output),
            context_window_tokens: model.limits.as_ref().map(|limits| {
                limits
                    .context_window_tokens
                    .min(policy.context_window_tokens.unwrap_or(u32::MAX))
            }),
            chunking: assignment.chunking.clone(),
            default_modality: assignment.modality,
            reasoning: reasoning.map(|r| ReasoningDefaults {
                effort: r.effort.unwrap_or(level),
                max_reasoning_tokens: r.max_reasoning_tokens,
                strategy: r.strategy,
                summary: None,
            }),
            metadata: Some(metadata),
            supports_vision: Some(restricted(assignment.vision, model.capabilities.vision)?),
            supports_reasoning: Some(restricted(
                assignment.reasoning,
                model.thinking.mode != ThinkingMode::None,
            )?),
            supports_tool_calling: Some(restricted(assignment.tools, model.capabilities.tools)?),
            supports_computer_use: Some(restricted(
                assignment.computer_use,
                model.capabilities.computer_use,
            )?),
            timeout_secs: Some(policy.timeout_secs),
        })
    }
}

fn insert_profile(router: &mut LLMRouterConfig, name: &str, profile: LLMProfile) -> LLMResult<()> {
    if router.profiles.contains_key(name) {
        return Err(config_error(format!("duplicate model route `{name}`")));
    }
    router.profiles.insert(name.into(), profile);
    Ok(())
}

#[cfg(test)]
mod tests;

fn validate_metadata(metadata: &HashMap<String, Value>) -> LLMResult<()> {
    if metadata.contains_key("fallback_profile") {
        return Err(config_error(
            "catalog fallbacks must use assignment.fallback, not metadata.fallback_profile",
        ));
    }
    if metadata.contains_key(CATALOG_SETTINGS_KEY) {
        return Err(config_error(
            "reserved model settings metadata cannot be supplied by configuration",
        ));
    }
    Ok(())
}

fn resolve_level(control: &ThinkingControl, requested: &str, automatic: bool) -> LLMResult<String> {
    if control.mode == ThinkingMode::None {
        return if automatic || requested == "none" {
            Ok("none".into())
        } else {
            Err(LLMError::UnsupportedCapability(
                "model has no adjustable thinking control".into(),
            ))
        };
    }
    if control.levels.iter().any(|v| v == requested) {
        return Ok(requested.into());
    }
    if automatic {
        // Shared workload defaults may ask for medium on a binary or sparse
        // model. The model's explicit default is the qualified fallback.
        return control
            .default
            .clone()
            .ok_or_else(|| config_error("model lacks a default thinking level"));
    }
    Err(LLMError::UnsupportedCapability(format!(
        "thinking level `{requested}` is unsupported; accepted values: {}",
        control.levels.join(", ")
    )))
}

fn reasoning_config(
    control: &ThinkingControl,
    level: &str,
    output: u32,
    reserve: u32,
) -> LLMResult<Option<ReasoningConfig>> {
    if control.mode == ThinkingMode::None {
        return Ok(None);
    }
    let mut config = ReasoningConfig {
        effort: Some(level.into()),
        max_reasoning_tokens: None,
        strategy: Some(
            match control.mode {
                ThinkingMode::Effort => "native_effort",
                ThinkingMode::Toggle => "toggle",
                ThinkingMode::Budget => "budget",
                ThinkingMode::None => "none",
            }
            .into(),
        ),
        summary: None,
    };
    if control.mode == ThinkingMode::Budget && level != "none" {
        let limits = control
            .budget
            .as_ref()
            .ok_or_else(|| config_error("thinking budget limits missing"))?;
        let available = output.saturating_sub(reserve.max(1)).min(limits.max_tokens);
        if available < limits.min_tokens {
            return Err(LLMError::Validation(
                "output allowance cannot fit minimum thinking budget and answer reserve".into(),
            ));
        }
        let fraction = match level {
            "minimal" | "low" => 8,
            "medium" => 4,
            "high" => 2,
            _ => 1,
        };
        let desired = (available / fraction).max(limits.min_tokens);
        let steps = (desired - limits.min_tokens) / limits.step_tokens;
        config.max_reasoning_tokens = Some(
            limits
                .min_tokens
                .saturating_add(steps.saturating_mul(limits.step_tokens))
                .min(available),
        );
    }
    Ok(Some(config))
}

#[derive(Deserialize)]
struct ResolvedSettings {
    limits: Option<ModelTokenLimits>,
    thinking: ThinkingControl,
    #[serde(default)]
    wire: Option<ModelWire>,
    answer_reserve_tokens: u32,
    max_output_tokens: u32,
}

/// Carry a profile's selected level into a request without pinning an
/// automatically derived catalog budget. A bounded call may have a smaller
/// output allowance than the materialized route; derive its budget at dispatch.
/// Legacy authored budgets keep their historical explicit meaning.
pub fn request_reasoning_defaults(profile: &LLMProfile) -> Option<ReasoningConfig> {
    let defaults = profile.reasoning.as_ref()?;
    let catalog = profile
        .metadata
        .as_ref()
        .is_some_and(|metadata| metadata.contains_key(CATALOG_SETTINGS_KEY));
    Some(ReasoningConfig {
        effort: Some(defaults.effort.clone()),
        max_reasoning_tokens: if catalog {
            None
        } else {
            defaults.max_reasoning_tokens
        },
        strategy: defaults.strategy.clone(),
        summary: defaults.summary.clone(),
    })
}

/// Revalidate explicit request overrides against the selected model. The same
/// function runs for sync, streaming and fallback physical attempts.
pub fn resolve_request_settings(profile: &LLMProfile, request: &mut LLMRequest) -> LLMResult<()> {
    // A fallback hop may move a request from a catalogued model to a legacy
    // profile; the previous model's wire facts must not travel with it.
    request.model_wire = None;
    let Some(value) = profile
        .metadata
        .as_ref()
        .and_then(|m| m.get(CATALOG_SETTINGS_KEY))
    else {
        return Ok(());
    };
    let settings: ResolvedSettings = serde_json::from_value(value.clone())
        .map_err(|e| config_error(format!("invalid resolved model settings: {e}")))?;
    if request.model != profile.model {
        return Err(LLMError::Validation(
            "model override requires settings resolved for that exact model".into(),
        ));
    }
    request.model_wire = settings.wire.clone();
    let output = request
        .max_output_tokens
        .ok_or_else(|| config_error("catalog route requires an output allowance"))?;
    let ceiling = settings.max_output_tokens;
    // Explicit caller budgets are bounds. Never expand them or silently cap a
    // request whose owner quoted a different physical contract.
    let minimum_output = settings
        .limits
        .as_ref()
        .map(|limits| limits.min_output_tokens)
        .unwrap_or(1);
    if output < minimum_output || output > ceiling {
        return Err(LLMError::Validation(format!(
            "requested output allowance {output} is outside {}..={ceiling}",
            minimum_output
        )));
    }
    if let Some(reasoning) = &request.reasoning {
        let level = resolve_level(
            &settings.thinking,
            reasoning.effort.as_deref().unwrap_or("none"),
            false,
        )?;
        let generated = reasoning_config(
            &settings.thinking,
            &level,
            output,
            settings.answer_reserve_tokens,
        )?;
        if reasoning.max_reasoning_tokens.is_some()
            && settings.thinking.mode != ThinkingMode::Budget
        {
            return Err(LLMError::Validation(
                "selected model accepts effort, not an explicit reasoning token budget".into(),
            ));
        }
        if settings.thinking.mode == ThinkingMode::Budget {
            let generated = generated.expect("budget model has reasoning config");
            if let Some(budget) = reasoning.max_reasoning_tokens {
                let limits = settings
                    .thinking
                    .budget
                    .as_ref()
                    .expect("validated budget mode");
                if level == "none"
                    || budget < limits.min_tokens
                    || budget > limits.max_tokens
                    || budget >= output
                    || (budget - limits.min_tokens) % limits.step_tokens != 0
                {
                    return Err(LLMError::Validation(
                        "reasoning token budget is outside the selected model's limits".into(),
                    ));
                }
            } else {
                request.reasoning = Some(generated);
            }
        } else if let Some(generated) = generated {
            if let Some(reasoning) = request.reasoning.as_mut() {
                reasoning.strategy = generated.strategy;
            }
        } else {
            request.reasoning = None;
        }
    }
    Ok(())
}
