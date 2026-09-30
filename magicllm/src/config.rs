use crate::capability::{LLMModality, LLMProviderKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Backwards-compatible configuration shared by Magician and Magictunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub provider: LLMProviderKind,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<HashMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_calling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_computer_use: Option<bool>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: LLMProviderKind::OpenAI,
            model: "gpt-5.6-terra".to_string(),
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            api_base_url: None,
            max_tokens: Some(4000),
            temperature: Some(0.7),
            reasoning_effort: None,
            verbosity: None,
            additional_params: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
        }
    }
}

/// Named profile used by the router for operation-based routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMProfile {
    #[serde(default)]
    pub provider: LLMProviderKind,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Physical input-plus-output context capacity for one provider request.
    /// `None` preserves the historical no-preflight behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
    /// Provider-neutral logical chunking policy. Disabled policies enable
    /// observe-only physical preflight; enabled policies enforce overflow but
    /// require a caller to enter through the structured logical runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunking: Option<ChunkingConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_modality: Option<LLMModality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningDefaults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_calling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_computer_use: Option<bool>,
    /// Per-profile request timeout in seconds. Overrides the provider default
    /// when the caller has not set `RequestMetadata.timeout_secs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// Failure handling after a local chunk response cannot be repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkFallbackPolicy {
    SameProviderOnly,
    MappedProfile,
    Deterministic,
    Disabled,
}

impl Default for ChunkFallbackPolicy {
    fn default() -> Self {
        Self::SameProviderOnly
    }
}

pub const DEFAULT_CONTEXT_SAFETY_MARGIN_TOKENS: u32 = 2_048;

const fn default_chunk_safety_margin_tokens() -> u32 {
    DEFAULT_CONTEXT_SAFETY_MARGIN_TOKENS
}

/// Typed logical-context configuration attached to a standard profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_window_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_payload_tokens: Option<u32>,
    /// End-to-end deadline for the complete logical map/reduce operation.
    /// Physical `timeout_secs` still bounds each provider call independently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_timeout_secs: Option<u64>,
    #[serde(default = "default_chunk_safety_margin_tokens")]
    pub safety_margin_tokens: u32,
    #[serde(default)]
    pub fallback_policy: ChunkFallbackPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_profile: Option<String>,
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            adapter: None,
            logical_window_tokens: None,
            target_payload_tokens: None,
            logical_timeout_secs: None,
            safety_margin_tokens: default_chunk_safety_margin_tokens(),
            fallback_policy: ChunkFallbackPolicy::SameProviderOnly,
            fallback_profile: None,
        }
    }
}

impl LLMProfile {
    /// Returns the declared default modality for the profile, falling back to text.
    pub fn modality(&self) -> LLMModality {
        self.default_modality.unwrap_or(LLMModality::Text)
    }
}

/// Default reasoning configuration applied to requests when unspecified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningDefaults {
    pub effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reasoning_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Provider-specific reasoning-summary mode (OpenAI Responses
    /// only). See `ReasoningConfig::summary`. Defaults to `auto` at
    /// the provider layer when omitted and reasoning is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// How a single operation resolves to a profile, possibly conditional on
/// the request shape.
///
/// `serde(untagged)` keeps existing flat YAML config working — a string
/// value deserializes as `Simple(name)`, a struct value as `Conditional`.
/// Two-shape compatibility avoids a config migration:
///
/// ```yaml
/// operation_mapping:
///   # Old shape — still works
///   gmail_inner_loop: gptterra-responses-toolsany
///
///   # New shape — per-request alternative
///   agent_browser_inner_loop_chat:
///     default: gptterra-responses-toolsany
///     when_has_images: vision-yutori-n1
///
///   # Locality shape — operator policy picks the arm family
///   channel_ingest_distill:
///     default: op-channel-distill-local
///     when_cloud: op-channel-distill-remote
///     description: Distills an incoming channel message into durable context.
///     group: Channels
/// ```
///
/// **Transport-cohort safety**: `default` and any alternative MUST share
/// the same provider transport (e.g. both Chat Completions, or both
/// Responses API). Mid-conversation transport mismatch breaks tool-call
/// id/content shapes. The router-level guard surfaces a `WARN` and falls
/// back to `default` if cohorts diverge — see
/// `magician::operation_llm_router::request_profile_for_operation`.
/// The cohort guard governs `when_has_images` (same-conversation shape
/// swaps); `when_cloud` is an explicit operator locality decision and is
/// exempt — a local Ollama arm and its remote counterpart never share a
/// transport by design.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OperationProfileSelector {
    Simple(String),
    Conditional {
        default: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when_has_images: Option<String>,
        /// Profile serving this operation when the operator has selected the
        /// cloud locality (`privacy.processing.mode: cloud`). Absent ⇒ the
        /// operation stays on `default` in both modes — local by
        /// construction rather than by accident.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when_cloud: Option<String>,
        /// Human-readable purpose shown by operator surfaces. It lives on the
        /// mapping so routing and its explanation have one source of truth.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Short operator-facing category used to group routing entries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
        /// Whether this operation rides the engine that started the flow it
        /// runs in. Absent ⇒ [`OperationEngineFollow::Parent`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<OperationEngineFollow>,
    },
}

/// How an operation relates to the engine that started the flow it runs in
/// (a swapped chat mouth, a run engine, a connected CLI).
///
/// `Parent` (the default) lets the operation follow that engine when the
/// engine can serve it; `Pinned` keeps the operation on its own profile
/// regardless. A flat `Simple(String)` selector means `Parent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OperationEngineFollow {
    #[default]
    Parent,
    Pinned,
}

/// Operator-selected processing locality for an install. Drives the
/// `when_cloud` selector arms: `Local` is today's behavior everywhere;
/// `Cloud` moves local-eligible operations to their remote counterparts
/// (and leaves operations without a remote counterpart local).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProcessingLocality {
    #[default]
    Local,
    Cloud,
}

impl From<String> for OperationProfileSelector {
    fn from(value: String) -> Self {
        OperationProfileSelector::Simple(value)
    }
}

impl From<&str> for OperationProfileSelector {
    fn from(value: &str) -> Self {
        OperationProfileSelector::Simple(value.to_string())
    }
}

impl std::fmt::Display for OperationProfileSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationProfileSelector::Simple(name) => f.write_str(name),
            OperationProfileSelector::Conditional {
                default,
                when_has_images,
                when_cloud,
                ..
            } => match (when_has_images, when_cloud) {
                (Some(alt), Some(cloud)) => {
                    write!(
                        f,
                        "{default} (alt when_has_images={alt}, when_cloud={cloud})"
                    )
                },
                (Some(alt), None) => write!(f, "{default} (alt when_has_images={alt})"),
                (None, Some(cloud)) => write!(f, "{default} (when_cloud={cloud})"),
                (None, None) => f.write_str(default),
            },
        }
    }
}

impl OperationProfileSelector {
    pub fn default_profile(&self) -> &str {
        match self {
            OperationProfileSelector::Simple(name) => name.as_str(),
            OperationProfileSelector::Conditional { default, .. } => default.as_str(),
        }
    }

    /// Optional operator-facing explanation carried by the mapping itself.
    pub fn description(&self) -> Option<&str> {
        match self {
            OperationProfileSelector::Simple(_) => None,
            OperationProfileSelector::Conditional { description, .. } => description.as_deref(),
        }
    }

    /// Optional operator-facing group carried by the mapping itself.
    pub fn group(&self) -> Option<&str> {
        match self {
            OperationProfileSelector::Simple(_) => None,
            OperationProfileSelector::Conditional { group, .. } => group.as_deref(),
        }
    }

    /// True unless the operation pins itself off the flow's parent engine.
    /// A flat selector and an absent `engine` field both follow the parent.
    pub fn follows_parent(&self) -> bool {
        match self {
            OperationProfileSelector::Simple(_) => true,
            OperationProfileSelector::Conditional { engine, .. } => {
                *engine != Some(OperationEngineFollow::Pinned)
            },
        }
    }

    /// Validate optional catalog metadata while retaining compatibility with
    /// legacy selectors that contain routing fields only.
    pub fn validate_metadata(&self, operation: &str) -> Result<(), String> {
        for (field, value, max_bytes) in [
            ("description", self.description(), 512usize),
            ("group", self.group(), 64usize),
        ] {
            let Some(value) = value else {
                continue;
            };
            if value.trim().is_empty() {
                return Err(format!(
                    "operation mapping `{operation}` has an empty `{field}`"
                ));
            }
            if value.len() > max_bytes {
                return Err(format!(
                    "operation mapping `{operation}` `{field}` exceeds {max_bytes} bytes"
                ));
            }
            if value.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(format!(
                    "operation mapping `{operation}` `{field}` contains control bytes"
                ));
            }
        }
        Ok(())
    }

    pub fn profile_for(&self, shape: &RequestShape) -> &str {
        match self {
            OperationProfileSelector::Simple(name) => name.as_str(),
            OperationProfileSelector::Conditional {
                default,
                when_has_images,
                ..
            } => {
                if shape.has_images {
                    if let Some(alt) = when_has_images.as_deref() {
                        return alt;
                    }
                }
                default.as_str()
            },
        }
    }

    /// Locality-aware resolution. Precedence is fixed as *locality, then
    /// shape*: the locality selects the arm family (`when_cloud` for cloud,
    /// `default` for local), the request shape picks within the family. A
    /// `when_cloud` arm is a single flat profile today, so in cloud mode it
    /// wins over `when_has_images`; no production mapping combines both yet.
    /// Cloud with no `when_cloud` arm resolves to `default` — an operation
    /// without a remote counterpart stays local by construction.
    pub fn profile_for_locality(&self, shape: &RequestShape, locality: ProcessingLocality) -> &str {
        match self {
            OperationProfileSelector::Simple(name) => name.as_str(),
            OperationProfileSelector::Conditional { when_cloud, .. } => {
                if locality == ProcessingLocality::Cloud {
                    if let Some(cloud) = when_cloud.as_deref() {
                        return cloud;
                    }
                }
                self.profile_for(shape)
            },
        }
    }

    /// True when locality-aware resolution would leave the `when_cloud` arm
    /// serving this operation. Lets the shape-aware caller exempt the
    /// operator-selected remote arm from the same-conversation transport
    /// cohort guard.
    pub fn selects_cloud_arm(&self, locality: ProcessingLocality) -> bool {
        matches!(
            (self, locality),
            (
                OperationProfileSelector::Conditional {
                    when_cloud: Some(_),
                    ..
                },
                ProcessingLocality::Cloud,
            )
        )
    }
}

/// Per-call request shape used by the router to pick between profile
/// alternatives. Currently just `has_images`; future signals like
/// `has_audio` or `long_context` slot in here without changing call sites.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestShape {
    pub has_images: bool,
    /// The request carries tool definitions. Routing that would move the
    /// call onto a text-only provider must leave such a request where it is.
    pub has_tools: bool,
}

impl RequestShape {
    pub const NONE: RequestShape = RequestShape {
        has_images: false,
        has_tools: false,
    };
}

/// Composite "adaptive" profile that pairs a fast (no-thinking) profile
/// with a thinking profile. The chat runtime starts a turn in the fast
/// profile and exposes a `request_thinking_mode` tool; the LLM calls the
/// tool when it decides the task warrants extended reasoning, the
/// runtime swaps to the `thinking_profile` and restarts the turn.
///
/// Adaptive profiles live in their own map (`LLMRouterConfig::adaptive_profiles`)
/// keyed by name — alongside (not inside) the regular `profiles` map. Two
/// reasons:
///   1. Existing `LLMProfile` shape is unchanged. No schema migration on
///      any standard profile; YAML loading is forward-compatible.
///   2. The discriminator is *placement* — a name in `adaptive_profiles`
///      is adaptive; a name in `profiles` is standard. No `kind` field
///      required on standards.
///
/// References (`fast_profile`, `thinking_profile`) MUST point at entries
/// in the `profiles` map; pointing at another adaptive profile is
/// disallowed at load time to prevent escalation loops.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveProfile {
    /// Name of the standard profile used for the initial fast turn.
    pub fast_profile: String,
    /// Name of the standard profile used after the runtime escalates.
    pub thinking_profile: String,
    /// Optional operator-facing description shown in the profile picker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional tier label for UI grouping inside the Adaptive group
    /// (e.g. `instant` / `normal` / `advanced` / `frontier` to communicate the
    /// size / capability trade-off without exposing raw model names).
    /// Free-form: the UI renders whatever string is here as a chip.
    /// None = no chip is shown (caller relies on `description` alone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Optional metadata bag for future use (e.g. per-channel overrides).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, Value>>,
}

/// Resolved view of a profile name, distinguishing adaptive composites
/// from standard profiles. Returned by
/// [`LLMRouterConfig::resolve_profile`].
#[derive(Debug, Clone, Copy)]
pub enum ResolvedProfile<'a> {
    /// A regular profile — what every existing caller expects.
    Standard(&'a LLMProfile),
    /// An adaptive composite — pre-resolved to its fast + thinking
    /// inner profiles. The chat runtime treats this case specially
    /// (exposes `request_thinking_mode` tool, injects fast-mode prompt
    /// block); non-chat callers can safely treat the `fast` field as
    /// "the profile to use" and get standard behavior.
    Adaptive {
        name: &'a str,
        composite: &'a AdaptiveProfile,
        fast: &'a LLMProfile,
        thinking: &'a LLMProfile,
    },
}

/// Router configuration mapping operations to profiles.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LLMRouterConfig {
    /// Loaded from the sibling `llm-profiles.yaml`, spliced into the config
    /// text before parse — see `magician::config::splice_router_profiles`.
    ///
    /// `skip_serializing` is the structural half of that split. Two settings
    /// writers (`privacy_processing_settings`, `workspace_storage_settings`)
    /// save by deserializing the whole config, replacing one section, and
    /// re-serializing. Without this, the first such save would write ~3,800
    /// lines of spliced profiles back into `magician-config.yaml` and silently
    /// undo the split — and a future writer would do it again. Omitting them
    /// here means no config writer can, whether or not it remembers to.
    ///
    /// Safe because deserialization keeps `default`, so a config without the
    /// key still parses, and nothing serializes this struct expecting profiles.
    #[serde(default, skip_serializing)]
    pub profiles: HashMap<String, LLMProfile>,
    /// Adaptive composite profiles — see [`AdaptiveProfile`]. Empty by
    /// default; only populated when the operator opts in via YAML.
    #[serde(default)]
    pub adaptive_profiles: HashMap<String, AdaptiveProfile>,
    /// Loaded from the sibling `llm-router.yaml` alongside [`Self::profiles`],
    /// and skipped on serialize for the same reason: a settings writer that
    /// re-serializes the whole config would otherwise copy it back into
    /// `magician-config.yaml` and leave two diverging copies.
    #[serde(default, skip_serializing)]
    pub operation_mapping: HashMap<String, OperationProfileSelector>,
    #[serde(default = "default_profile_name")]
    pub default_profile: String,
    /// Operator processing locality driving the `when_cloud` selector arms.
    /// Derived from the top-level `privacy.processing.mode` at magician
    /// config load (single source of truth); absent YAML means `Local`, so
    /// an install that has never seen the field resolves exactly as before.
    #[serde(default, skip_serializing_if = "locality_is_local")]
    pub locality: ProcessingLocality,
    /// Realtime voice provider routing. Lives alongside the LLM
    /// profile mapping so a single config file describes every model
    /// magician talks to — chat, completion, AND realtime audio.
    /// Empty by default; populated only when realtime voice is wired.
    #[serde(default)]
    pub realtime_voice: RealtimeVoiceConfig,
}

fn locality_is_local(locality: &ProcessingLocality) -> bool {
    *locality == ProcessingLocality::Local
}

/// Realtime voice provider profiles + operation mapping. Mirrors the
/// shape of the LLM profile section but with realtime-specific
/// fields. Lives under `realtime_voice` so chat profile schemas stay
/// clean. Operation names are matched against
/// `LLMOperation::VoiceController` / `VoiceContextCompaction` from
/// the magician operation enum.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RealtimeVoiceConfig {
    /// Named profiles (e.g. `voice_realtime_default`). Each profile
    /// specifies the provider, model, optional voice override, and lifecycle
    /// hints needed to mint an upstream session.
    #[serde(default)]
    pub profiles: HashMap<String, RealtimeVoiceProfile>,
    /// Maps operations (`voice_controller`) to profile names. The
    /// orchestrator looks up the operation it's about to run and
    /// resolves the matching profile.
    #[serde(default)]
    pub operation_mapping: HashMap<String, String>,
    /// Profile used when no operation mapping matches. Optional —
    /// when `None`, an unmapped operation surfaces an error rather
    /// than silently falling back.
    #[serde(default)]
    pub default_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeVoiceProfile {
    /// Provider implementation key. Matched against the impls registered with
    /// `RealtimeProviderFactory`: `openai_realtime`,
    /// `openai_realtime_backend`, `openai_live`, or `gemini_live`.
    pub provider: String,
    /// Model identifier (e.g. `"gpt-realtime"`,
    /// `"gpt-realtime-2025-08-25"`).
    pub model: String,
    /// User-facing label used by Web/iOS realtime provider pickers. Profiles
    /// without a label remain internal routing details and are not advertised.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Whether this profile may be selected explicitly by a client. Operation
    /// mappings can still use non-selectable profiles.
    #[serde(default)]
    pub selectable: bool,
    /// Conversational assistant or continuous speech translator. Translation
    /// profiles intentionally omit system instructions and tools because the
    /// Gemini translation model does not support either capability.
    #[serde(default)]
    pub mode: RealtimeVoiceMode,
    /// Explicit operator acknowledgement that this provider may answer before
    /// Magician's finalized-transcript retrieval gate completes. This defaults
    /// off so adding a new provider cannot silently weaken voice grounding.
    #[serde(default)]
    pub allow_without_turn_grounding: bool,
    /// Voice identifier. Optional — some providers infer from model.
    #[serde(default)]
    pub voice: Option<String>,
    /// Hard upper bound on a single upstream session's lifetime,
    /// in seconds. The orchestrator schedules a proactive rotation
    /// ~60 s before this fires. `None` falls back to whatever the
    /// provider impl reports.
    #[serde(default)]
    pub max_session_duration_secs: Option<u64>,
    /// Fraction of the model's context window that triggers a
    /// compaction-then-rotate. `None` falls back to the provider
    /// impl's default (typically 0.7).
    #[serde(default)]
    pub compaction_token_watermark: Option<f32>,
    /// Override for the provider's base URL — useful for
    /// OpenAI-compatible local servers (LMStudio, vLLM, etc.)
    /// or air-gapped deployments. `None` uses the provider's
    /// canonical endpoint.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Fallback chain of profile names. Tried in order on
    /// `RealtimeProviderError::Upstream`. Empty by default.
    #[serde(default)]
    pub fallback: Vec<String>,
    /// Model id used for input-audio transcription on the upstream
    /// session (OpenAI Realtime's `input_audio_transcription.model`).
    /// OpenAI realtime profiles must set this explicitly; there is no
    /// code-level transcription model fallback.
    /// `local` asks a backend-proxied Magician surface to attach its
    /// call-scoped streaming-STT chain and disable vendor transcription only
    /// after that attachment succeeds.
    #[serde(default)]
    pub transcription_model: Option<String>,
    /// Vendor transcription model restored if a `local` call-scoped STT
    /// stream cannot start or fails during a backend-proxied call. Required
    /// when `transcription_model` is `local`; there is no code-level model
    /// fallback.
    #[serde(default)]
    pub transcription_fallback_model: Option<String>,
    /// Server-side voice-activity-detection mode. `"server_vad"` =
    /// upstream auto-commits user turns when it detects silence;
    /// `"none"` = client owns turn-taking (push-to-talk). Frontend
    /// can still flip to PTT per-call via the existing
    /// `pushToTalkMode` store — this profile field sets the default.
    #[serde(default)]
    pub turn_detection_mode: Option<String>,
    /// Approximate context-window size in tokens. Used as the
    /// denominator for the watermark trigger. Different per model
    /// (32K for legacy realtime, 128K for `gpt-realtime-2`, etc.).
    /// `None` falls back to a conservative 32K default.
    #[serde(default)]
    pub context_window_tokens: Option<u64>,
    /// Number of recent voice turns to replay verbatim alongside
    /// the compacted summary on every rotation. `None` falls back
    /// to the compactor's default (12).
    #[serde(default)]
    pub verbatim_recent_turns: Option<usize>,
    /// Soft cap on the chat-ledger window the compactor reads on
    /// each compaction pass. `None` falls back to the compactor's
    /// default (200).
    #[serde(default)]
    pub compaction_input_turn_limit: Option<usize>,
    /// BCP-47 target for translation profiles. Gemini defaults to English, but
    /// keeping the value in config makes language-specific profiles
    /// declarative.
    #[serde(default)]
    pub translation_target_language: Option<String>,
    /// Whether speech already in the target language should be echoed.
    #[serde(default)]
    pub translation_echo_target_language: bool,
    /// Background reasoning depth for Live models that reason while they
    /// speak (`low` / `medium` / `high`). Only Gemini's extended-thinking Live
    /// model accepts it; the factory rejects the field on any other model so
    /// the mismatch shows up as an unavailable profile instead of a mid-call
    /// upstream close.
    #[serde(default)]
    pub thinking_level: Option<String>,
    /// When a Live model runs Magician's tools without blocking its speech,
    /// this says when the result should be spoken: `when_idle` (finish the
    /// current utterance first, the default), `interrupt` (cut in right away),
    /// or `silent` (fold it into later turns). Ignored by models that block on
    /// every tool call.
    #[serde(default)]
    pub tool_result_scheduling: Option<String>,
    /// Position in Web/iOS/Android engine pickers. Lower sorts first;
    /// profiles without it sort as `0`, then by profile id, so the existing
    /// alphabetical order holds until an operator moves something.
    #[serde(default)]
    pub display_order: Option<i32>,
}

impl RealtimeVoiceProfile {
    /// Picker sort key: explicit `display_order` first, then the profile id
    /// so equal ranks stay deterministic.
    pub fn display_sort_key<'a>(&self, profile_id: &'a str) -> (i32, &'a str) {
        (self.display_order.unwrap_or(0), profile_id)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeVoiceMode {
    #[default]
    Assistant,
    Translation,
}

impl LLMRouterConfig {
    /// Validate the operator-facing catalog fields embedded in operation
    /// selectors. Routing-only legacy selectors remain valid.
    pub fn validate_operation_metadata(&self) -> Result<(), Vec<String>> {
        let errors: Vec<String> = self
            .operation_mapping
            .iter()
            .filter_map(|(operation, selector)| selector.validate_metadata(operation).err())
            .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Returns the profile associated with the specified operation.
    /// Picks the unconditional default profile (no request-shape signals).
    /// Use [`Self::get_profile_for_operation_with_shape`] when an
    /// alternative-aware lookup is desired.
    ///
    /// Adaptive profiles transparently resolve to their `fast_profile`
    /// for callers that aren't adaptive-aware. The chat runtime calls
    /// [`Self::resolve_profile_for_operation`] instead to handle both
    /// variants explicitly.
    pub fn get_profile_for_operation(&self, operation: &str) -> Option<&LLMProfile> {
        self.get_profile_for_operation_with_shape(operation, &RequestShape::NONE)
    }

    /// Returns the profile selected for the operation given the request
    /// shape (e.g. presence of images). Falls back to `default_profile`
    /// when the operation isn't mapped. Adaptive composites transparently
    /// resolve to their `fast_profile` — see [`Self::resolve_profile_for_operation`]
    /// for the adaptive-aware variant.
    pub fn get_profile_for_operation_with_shape(
        &self,
        operation: &str,
        shape: &RequestShape,
    ) -> Option<&LLMProfile> {
        let profile_name = self.profile_name_for_operation(operation, shape);
        match self.adaptive_profiles.get(profile_name) {
            Some(adaptive) => self.profiles.get(&adaptive.fast_profile),
            None => self.profiles.get(profile_name),
        }
    }

    /// Adaptive-aware variant that surfaces the composite when the
    /// resolved name is an adaptive profile. Chat-side runtime calls
    /// this to decide whether to inject the escalation tool + fast-mode
    /// prompt block. Non-adaptive operations can keep using
    /// `get_profile_for_operation` without change.
    pub fn resolve_profile_for_operation<'a>(
        &'a self,
        operation: &str,
    ) -> Option<ResolvedProfile<'a>> {
        self.resolve_profile_for_operation_with_shape(operation, &RequestShape::NONE)
    }

    pub fn resolve_profile_for_operation_with_shape<'a>(
        &'a self,
        operation: &str,
        shape: &RequestShape,
    ) -> Option<ResolvedProfile<'a>> {
        let profile_name = self.profile_name_for_operation(operation, shape);
        self.resolve_profile(profile_name)
    }

    /// Direct profile-name resolution, distinguishing standard vs
    /// adaptive. Returns None when the name doesn't exist in either map.
    pub fn resolve_profile<'a>(&'a self, name: &'a str) -> Option<ResolvedProfile<'a>> {
        if let Some(adaptive) = self.adaptive_profiles.get(name) {
            let fast = self.profiles.get(&adaptive.fast_profile)?;
            let thinking = self.profiles.get(&adaptive.thinking_profile)?;
            return Some(ResolvedProfile::Adaptive {
                name,
                composite: adaptive,
                fast,
                thinking,
            });
        }
        self.profiles.get(name).map(ResolvedProfile::Standard)
    }

    /// Compute the profile name selected for an operation + shape. Falls
    /// back to `default_profile` when the operation isn't mapped.
    fn profile_name_for_operation<'a>(&'a self, operation: &str, shape: &RequestShape) -> &'a str {
        self.operation_mapping
            .get(operation)
            .map(|selector| selector.profile_for_locality(shape, self.locality))
            .unwrap_or(self.default_profile.as_str())
    }

    /// Validate adaptive-profile references at config-load time. Returns
    /// human-readable errors when:
    ///   - an adaptive profile's `fast_profile` or `thinking_profile`
    ///     references a name that doesn't exist in `profiles`
    ///   - either reference points at another adaptive profile (would
    ///     create an escalation loop)
    ///   - both references point at the same profile (escalation would
    ///     be a no-op — operator typo)
    pub fn validate_adaptive_profiles(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        for (name, adaptive) in &self.adaptive_profiles {
            if !self.profiles.contains_key(&adaptive.fast_profile) {
                if self.adaptive_profiles.contains_key(&adaptive.fast_profile) {
                    errors.push(format!(
                        "adaptive profile `{name}` has fast_profile `{}` which is itself adaptive — adaptive profiles cannot nest",
                        adaptive.fast_profile
                    ));
                } else {
                    errors.push(format!(
                        "adaptive profile `{name}` references fast_profile `{}` which doesn't exist in `profiles`",
                        adaptive.fast_profile
                    ));
                }
            }
            if !self.profiles.contains_key(&adaptive.thinking_profile) {
                if self
                    .adaptive_profiles
                    .contains_key(&adaptive.thinking_profile)
                {
                    errors.push(format!(
                        "adaptive profile `{name}` has thinking_profile `{}` which is itself adaptive — adaptive profiles cannot nest",
                        adaptive.thinking_profile
                    ));
                } else {
                    errors.push(format!(
                        "adaptive profile `{name}` references thinking_profile `{}` which doesn't exist in `profiles`",
                        adaptive.thinking_profile
                    ));
                }
            }
            if adaptive.fast_profile == adaptive.thinking_profile {
                errors.push(format!(
                    "adaptive profile `{name}` has identical fast_profile and thinking_profile (`{}`) — escalation would be a no-op",
                    adaptive.fast_profile
                ));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn default_profile_name() -> String {
    "default".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::LLMProviderKind;

    #[test]
    fn simple_selector_returns_same_profile_for_any_shape() {
        let selector = OperationProfileSelector::Simple("foo".into());
        assert_eq!(selector.default_profile(), "foo");
        assert_eq!(
            selector.profile_for(&RequestShape {
                has_images: false,
                has_tools: false
            }),
            "foo"
        );
        assert_eq!(
            selector.profile_for(&RequestShape {
                has_images: true,
                has_tools: false
            }),
            "foo"
        );
    }

    #[test]
    fn conditional_selector_picks_alternative_only_when_has_images() {
        let selector = OperationProfileSelector::Conditional {
            default: "text-profile".into(),
            when_has_images: Some("vision-profile".into()),
            when_cloud: None,
            description: None,
            group: None,
            engine: None,
        };
        assert_eq!(selector.default_profile(), "text-profile");
        assert_eq!(
            selector.profile_for(&RequestShape {
                has_images: false,
                has_tools: false
            }),
            "text-profile"
        );
        assert_eq!(
            selector.profile_for(&RequestShape {
                has_images: true,
                has_tools: false
            }),
            "vision-profile"
        );
    }

    #[test]
    fn selector_shapes_parse_and_when_cloud_round_trips() {
        // Flat string — still parses unchanged.
        let simple: OperationProfileSelector =
            serde_json::from_str("\"gptterra-responses-toolsany\"").expect("simple parses");
        assert_eq!(simple.default_profile(), "gptterra-responses-toolsany");
        assert_eq!(
            serde_json::to_string(&simple)
                .expect("serialize simple")
                .trim(),
            "\"gptterra-responses-toolsany\""
        );

        // Two-field conditional — still parses unchanged.
        let two_field: OperationProfileSelector =
            serde_json::from_str("{\"default\": \"text\", \"when_has_images\": \"vision\"}")
                .expect("two-field conditional parses");
        assert_eq!(two_field.default_profile(), "text");

        // when_cloud round-trips and is omitted when absent.
        let with_cloud: OperationProfileSelector =
            serde_json::from_str("{\"default\": \"op-local\", \"when_cloud\": \"op-remote\"}")
                .expect("when_cloud conditional parses");
        let serialized = serde_json::to_string(&with_cloud).expect("serialize with_cloud");
        assert!(serialized.contains("\"when_cloud\":\"op-remote\""));
        let without_cloud: OperationProfileSelector =
            serde_json::from_str("{\"default\": \"op-local\"}")
                .expect("no-cloud conditional parses");
        assert!(!serde_json::to_string(&without_cloud)
            .expect("serialize without_cloud")
            .contains("when_cloud"));

        // Display metadata lives on the same mapping entry and survives the
        // same serde round trip without changing routing resolution.
        let documented: OperationProfileSelector = serde_json::from_str(
            r#"{
                "default": "op-local",
                "when_cloud": "op-remote",
                "description": "Explains what this operation does.",
                "group": "Memory"
            }"#,
        )
        .expect("documented selector parses");
        assert_eq!(documented.default_profile(), "op-local");
        assert_eq!(
            documented.description(),
            Some("Explains what this operation does.")
        );
        assert_eq!(documented.group(), Some("Memory"));
        let serialized =
            serde_json::to_string(&documented).expect("documented selector serializes");
        assert!(serialized.contains("\"description\":\"Explains what this operation does.\""));
        assert!(serialized.contains("\"group\":\"Memory\""));
    }

    #[test]
    fn an_operation_pins_itself_off_the_parent_engine() {
        // The YAML mapping `default: x` + `engine: pinned` deserializes
        // through the same untagged path as this JSON.
        let pinned: OperationProfileSelector =
            serde_json::from_str(r#"{"default": "op-own", "engine": "pinned"}"#)
                .expect("pinned selector parses");
        assert!(!pinned.follows_parent());
        assert_eq!(pinned.default_profile(), "op-own");
        assert!(matches!(
            pinned,
            OperationProfileSelector::Conditional {
                engine: Some(OperationEngineFollow::Pinned),
                ..
            }
        ));

        let serialized = serde_json::to_string(&pinned).expect("pinned selector serializes");
        assert!(serialized.contains("\"engine\":\"pinned\""));
        let round_tripped: OperationProfileSelector =
            serde_json::from_str(&serialized).expect("pinned selector round-trips");
        assert!(!round_tripped.follows_parent());
    }

    #[test]
    fn engine_defaults_to_parent() {
        assert_eq!(
            OperationEngineFollow::default(),
            OperationEngineFollow::Parent
        );

        let simple = OperationProfileSelector::Simple("op-own".to_string());
        assert!(simple.follows_parent());

        let absent: OperationProfileSelector = serde_json::from_str(r#"{"default": "op-own"}"#)
            .expect("selector without engine parses");
        assert!(absent.follows_parent());
        assert!(!serde_json::to_string(&absent)
            .expect("serialize without engine")
            .contains("engine"));

        let explicit: OperationProfileSelector =
            serde_json::from_str(r#"{"default": "op-own", "engine": "parent"}"#)
                .expect("explicit parent parses");
        assert!(explicit.follows_parent());
        assert!(serde_json::to_string(&explicit)
            .expect("serialize explicit parent")
            .contains("\"engine\":\"parent\""));
    }

    #[test]
    fn operation_catalog_metadata_is_optional_but_must_be_bounded_and_readable() {
        let legacy = OperationProfileSelector::Simple("profile".to_string());
        assert_eq!(legacy.description(), None);
        assert_eq!(legacy.group(), None);
        assert!(legacy.validate_metadata("legacy").is_ok());

        let invalid: OperationProfileSelector =
            serde_json::from_str(r#"{"default":"profile","description":"   ","group":"Planning"}"#)
                .expect("shape parses before semantic validation");
        assert_eq!(
            invalid.validate_metadata("bad_operation"),
            Err("operation mapping `bad_operation` has an empty `description`".to_string())
        );
    }

    #[test]
    fn locality_resolution_takes_the_cloud_arm_only_in_cloud_mode() {
        let selector = OperationProfileSelector::Conditional {
            default: "op-local".to_string(),
            when_has_images: Some("op-local-vision".to_string()),
            when_cloud: Some("op-remote".to_string()),
            description: None,
            group: None,
            engine: None,
        };
        let none = RequestShape::NONE;
        let images = RequestShape {
            has_images: true,
            has_tools: false,
        };

        // Local mode: today's behavior exactly.
        assert_eq!(
            selector.profile_for_locality(&none, ProcessingLocality::Local),
            "op-local"
        );
        assert_eq!(
            selector.profile_for_locality(&images, ProcessingLocality::Local),
            "op-local-vision"
        );
        assert!(!selector.selects_cloud_arm(ProcessingLocality::Local));

        // Cloud mode: locality picks the arm family; the flat cloud arm wins
        // over shape. No production mapping combines both (and the combined
        // precedence — locality, then shape — is fixed here).
        assert_eq!(
            selector.profile_for_locality(&none, ProcessingLocality::Cloud),
            "op-remote"
        );
        assert_eq!(
            selector.profile_for_locality(&images, ProcessingLocality::Cloud),
            "op-remote"
        );
        assert!(selector.selects_cloud_arm(ProcessingLocality::Cloud));
    }

    #[test]
    fn cloud_mode_without_a_cloud_arm_stays_local_by_construction() {
        let selector = OperationProfileSelector::Conditional {
            default: "op-local".to_string(),
            when_has_images: None,
            when_cloud: None,
            description: None,
            group: None,
            engine: None,
        };
        assert_eq!(
            selector.profile_for_locality(&RequestShape::NONE, ProcessingLocality::Cloud),
            "op-local"
        );
        assert!(!selector.selects_cloud_arm(ProcessingLocality::Cloud));
        // Simple selectors have no cloud arm either.
        let simple = OperationProfileSelector::Simple("only-local".to_string());
        assert_eq!(
            simple.profile_for_locality(&RequestShape::NONE, ProcessingLocality::Cloud),
            "only-local"
        );
    }

    #[test]
    fn conditional_without_image_alternative_falls_back_to_default() {
        let selector = OperationProfileSelector::Conditional {
            default: "text-profile".into(),
            when_has_images: None,
            when_cloud: None,
            description: None,
            group: None,
            engine: None,
        };
        // No alternative configured → default fires for both shapes.
        assert_eq!(
            selector.profile_for(&RequestShape {
                has_images: true,
                has_tools: false
            }),
            "text-profile"
        );
    }

    #[test]
    fn simple_string_deserializes_as_simple_variant() {
        // YAML's `legacy_op: profile-name` deserializes to JSON
        // `{"legacy_op": "profile-name"}` — same untagged path. The
        // backward-compat property holds for both.
        let json = r#"{
            "profiles": {},
            "operation_mapping": {"legacy_op": "legacy-profile"},
            "default_profile": "default"
        }"#;
        let config: LLMRouterConfig = serde_json::from_str(json).expect("json should parse");
        let selector = config
            .operation_mapping
            .get("legacy_op")
            .expect("operation present");
        assert!(
            matches!(selector, OperationProfileSelector::Simple(name) if name == "legacy-profile")
        );
    }

    #[test]
    fn struct_deserializes_as_conditional_variant() {
        let json = r#"{
            "profiles": {},
            "operation_mapping": {
                "inner_loop_chat": {
                    "default": "text-profile",
                    "when_has_images": "vision-yutori"
                }
            },
            "default_profile": "default"
        }"#;
        let config: LLMRouterConfig = serde_json::from_str(json).expect("json should parse");
        let selector = config
            .operation_mapping
            .get("inner_loop_chat")
            .expect("operation present");
        match selector {
            OperationProfileSelector::Conditional {
                default,
                when_has_images,
                ..
            } => {
                assert_eq!(default, "text-profile");
                assert_eq!(when_has_images.as_deref(), Some("vision-yutori"));
            },
            _ => panic!("expected Conditional, got Simple"),
        }
    }

    fn make_profile(provider: LLMProviderKind) -> LLMProfile {
        LLMProfile {
            provider,
            model: "model".to_string(),
            api_key_env: None,
            api_base_url: None,
            temperature: None,
            max_output_tokens: None,
            context_window_tokens: None,
            chunking: None,
            timeout_secs: None,
            default_modality: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
            reasoning: None,
            metadata: None,
        }
    }

    #[test]
    fn get_profile_for_operation_with_shape_picks_alternative() {
        let mut profiles = HashMap::new();
        profiles.insert("text".to_string(), make_profile(LLMProviderKind::OpenAI));
        profiles.insert("vision".to_string(), make_profile(LLMProviderKind::OpenAI));
        let mut operation_mapping = HashMap::new();
        operation_mapping.insert(
            "inner_loop_chat".to_string(),
            OperationProfileSelector::Conditional {
                default: "text".to_string(),
                when_has_images: Some("vision".to_string()),
                when_cloud: None,
                description: None,
                group: None,
                engine: None,
            },
        );
        let config = LLMRouterConfig {
            profiles,
            adaptive_profiles: HashMap::new(),
            operation_mapping,
            locality: ProcessingLocality::default(),
            default_profile: "text".to_string(),
            realtime_voice: RealtimeVoiceConfig::default(),
        };
        let with_image = config
            .get_profile_for_operation_with_shape(
                "inner_loop_chat",
                &RequestShape {
                    has_images: true,
                    has_tools: false,
                },
            )
            .expect("vision picked");
        assert_eq!(with_image.model, "model");
        let without_image = config
            .get_profile_for_operation_with_shape(
                "inner_loop_chat",
                &RequestShape {
                    has_images: false,
                    has_tools: false,
                },
            )
            .expect("text picked");
        assert_eq!(without_image.model, "model");
    }

    #[test]
    fn disabled_chunking_config_deserializes_with_safe_defaults() {
        let profile: LLMProfile = serde_json::from_value(serde_json::json!({
            "provider": "ollama",
            "model": "gemma4:12b",
            "context_window_tokens": 32768,
            "chunking": {"enabled": false}
        }))
        .expect("profile should deserialize");

        assert_eq!(profile.context_window_tokens, Some(32_768));
        let chunking = profile.chunking.expect("chunking policy");
        assert!(!chunking.enabled);
        assert_eq!(chunking.safety_margin_tokens, 2_048);
        assert_eq!(
            chunking.fallback_policy,
            ChunkFallbackPolicy::SameProviderOnly
        );
    }
}
