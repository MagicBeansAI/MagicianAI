use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported content modality for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LLMModality {
    /// Text-only generation.
    Text,
    /// Image understanding or multimodal input.
    Vision,
    /// Audio input/output (future support).
    Audio,
}

/// Reasoning capability level advertised by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LLMReasoning {
    /// No explicit reasoning support (standard chat models).
    None,
    /// Extended thinking mode (e.g., Claude 4.5 thinking).
    Standard,
    /// Dedicated reasoning tier (e.g., GPT-5, o1/o3).
    Advanced,
}

impl Default for LLMReasoning {
    fn default() -> Self {
        Self::None
    }
}

/// Identifier for a provider implementation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LLMProviderKind {
    OpenAI,
    Anthropic,
    Minimax,
    DeepSeek,
    OpenRouter,
    Ollama,
    Gemini,
    Yutori,
    /// xAI's Grok models over the xAI Responses API (`api.x.ai`).
    Xai,
    /// Sarvam AI's Indic-first models over its OpenAI-compatible Chat
    /// Completions API (`api.sarvam.ai`).
    Sarvam,
    Custom(String),
}

impl Default for LLMProviderKind {
    fn default() -> Self {
        Self::OpenAI
    }
}

impl LLMProviderKind {
    /// Returns a lowercase identifier suitable for config keys.
    pub fn as_str(&self) -> &str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::Minimax => "minimax",
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Ollama => "ollama",
            Self::Gemini => "gemini",
            Self::Yutori => "yutori",
            Self::Xai => "xai",
            Self::Sarvam => "sarvam",
            Self::Custom(id) => id.as_str(),
        }
    }

    /// Creates a provider kind from a string identifier.
    pub fn from_str(id: &str) -> Self {
        match id {
            "openai" => Self::OpenAI,
            "anthropic" => Self::Anthropic,
            "minimax" => Self::Minimax,
            "deepseek" => Self::DeepSeek,
            "openrouter" => Self::OpenRouter,
            "ollama" => Self::Ollama,
            "gemini" => Self::Gemini,
            "yutori" => Self::Yutori,
            "xai" => Self::Xai,
            "sarvam" => Self::Sarvam,
            other => Self::Custom(other.to_owned()),
        }
    }
}

impl fmt::Display for LLMProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for LLMProviderKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LLMProviderKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str(value.as_str()))
    }
}

/// Declares the feature set supported by a provider model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMCapability {
    #[serde(default)]
    pub modalities: Vec<LLMModality>,
    #[serde(default)]
    pub reasoning: LLMReasoning,
    #[serde(default)]
    pub tool_calling: bool,
    #[serde(default)]
    pub json_mode: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub computer_use: bool,
    /// Provider-executed web search (`LLMRequest` extra key
    /// `server_web_search`). True only for transports that translate the
    /// flag into a native server-side search tool; every other transport
    /// fails the request closed. See `server_web_search` module docs.
    #[serde(default)]
    pub web_search: bool,
}

impl Default for LLMCapability {
    fn default() -> Self {
        Self {
            modalities: vec![LLMModality::Text],
            reasoning: LLMReasoning::None,
            tool_calling: false,
            json_mode: false,
            streaming: false,
            computer_use: false,
            web_search: false,
        }
    }
}

impl LLMCapability {
    /// Checks if the model advertises support for the provided modality.
    pub fn supports_modality(&self, modality: LLMModality) -> bool {
        self.modalities.iter().copied().any(|m| m == modality)
    }

    /// True when the model supports any reasoning tier beyond none.
    pub fn supports_reasoning(&self) -> bool {
        !matches!(self.reasoning, LLMReasoning::None)
    }
}
