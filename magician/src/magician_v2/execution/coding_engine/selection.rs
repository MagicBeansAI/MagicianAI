//! Trusted coding-engine selection contract.
//!
//! Slice 0a of
//! `docs/archive/plans/2026-08-08-vibedev-codex-app-server-support-plan.md`.
//! These types are the authority carrier persisted on the VibeDev task
//! (constraint) and execution (invocation ledger). `CodingContinuationRef`
//! is the engine-bound native session returned after a turn.
//!
//! [`VibeDevCodingChoice`] is the request-side Auto/profile wire type and
//! already lives on the run service. This module is the *next* layer: a
//! digest-pinned constraint, a resolved selection, and an append-only
//! invocation ledger.
//!
//! [`VibeDevRunService`]: crate::magician_v2::vibedev::run_service::VibeDevRunService
//! [`VibeDevCodingChoice`]: crate::magician_v2::vibedev::dispatch_intent::VibeDevCodingChoice

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::CodingEngineKind;

pub const CONSTRAINT_SCHEMA_VERSION: u16 = 1;
pub const INVOCATION_SCHEMA_VERSION: u16 = 1;
pub const SELECTION_CONTRACT_REVISION: &str = "1";

const DEFINITION_DIGEST_DOMAIN: &str = "magician.coding_engine.profile_definition.v1";
const CONSTRAINT_DIGEST_DOMAIN: &str = "magician.coding_engine.constraint.v1";
const SELECTION_DIGEST_DOMAIN: &str = "magician.coding_engine.resolved_selection.v1";
const INVOCATION_AUTHORITY_DOMAIN: &str = "magician.coding_engine.invocation_authority.v1";
const CONTINUATION_DIGEST_DOMAIN: &str = "magician.coding_engine.continuation_binding.v1";

pub use super::codex_contract::{
    CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED, CODEX_APP_SERVER_EXPERIMENTAL_OFF_IN_V1,
    CODEX_APP_SERVER_PROHIBITED_METHODS, CODEX_APP_SERVER_STABLE_METHODS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    CrossEngineProposal {
        constraint_engine: CodingEngineKind,
        proposed_engine: CodingEngineKind,
    },
    ProfileNotPinned {
        profile_id: String,
    },
    ProfileChanged {
        profile_id: String,
    },
    ProfileIneligible {
        profile_id: String,
    },
    UnknownProfile {
        profile_id: String,
    },
    ConstraintDigestMismatch,
    FloorMustBePi,
    EscalationUnknown {
        profile_id: String,
    },
    EscalationNotSameEngine {
        profile_id: String,
    },
    InvalidProfile {
        reason: String,
    },
    LedgerIntegrityConflict {
        invocation_id: String,
    },
    LedgerCapReached {
        cap: usize,
    },
    InvalidDispatchTransition {
        from: &'static str,
        to: &'static str,
    },
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CrossEngineProposal {
                constraint_engine,
                proposed_engine,
            } => write!(
                f,
                "proposed {} profile is outside a fixed {} constraint",
                engine_str(*proposed_engine),
                engine_str(*constraint_engine)
            ),
            Self::ProfileNotPinned { profile_id } => {
                write!(f, "profile `{profile_id}` was not pinned at submission")
            },
            Self::ProfileChanged { profile_id } => {
                write!(f, "profile `{profile_id}` changed after it was pinned")
            },
            Self::ProfileIneligible { profile_id } => {
                write!(f, "profile `{profile_id}` is not currently eligible")
            },
            Self::UnknownProfile { profile_id } => {
                write!(f, "profile `{profile_id}` is not in the catalog")
            },
            Self::ConstraintDigestMismatch => {
                write!(
                    f,
                    "coding-engine constraint digest does not match its contents"
                )
            },
            Self::FloorMustBePi => {
                write!(f, "legacy hydration can only produce a fixed Pi constraint")
            },
            Self::EscalationUnknown { profile_id } => {
                write!(f, "escalation target `{profile_id}` is not in the catalog")
            },
            Self::EscalationNotSameEngine { profile_id } => {
                write!(f, "escalation target `{profile_id}` is not the same engine")
            },
            Self::InvalidProfile { reason } => write!(f, "invalid coding profile: {reason}"),
            Self::LedgerIntegrityConflict { invocation_id } => write!(
                f,
                "coding invocation `{invocation_id}` replayed with a different authority digest"
            ),
            Self::LedgerCapReached { cap } => {
                write!(f, "coding invocation ledger is at its cap of {cap}")
            },
            Self::InvalidDispatchTransition { from, to } => {
                write!(f, "cannot advance coding dispatch from {from} to {to}")
            },
        }
    }
}

impl std::error::Error for SelectionError {}

pub fn engine_str(engine: CodingEngineKind) -> &'static str {
    match engine {
        CodingEngineKind::Pi => "pi",
        CodingEngineKind::CodexAppServer => "codex_app_server",
        CodingEngineKind::GrokAcp => "grok_acp",
        CodingEngineKind::ClaudeCode => "claude_code",
        CodingEngineKind::AgyCli => "agy_cli",
    }
}

fn absorb(hasher: &mut blake3::Hasher, label: &str, value: &str) {
    hasher.update(&(label.len() as u64).to_le_bytes());
    hasher.update(label.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn absorb_optional(hasher: &mut blake3::Hasher, label: &str, value: Option<&str>) {
    absorb(hasher, label, value.unwrap_or_default());
    hasher.update(&[u8::from(value.is_some())]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingConstraintSource {
    UserUi,
    AssistantFacade,
    MigratedLegacy,
    ConfiguredDefault,
    /// Nothing chose: the build inherited the engine of the chat turn or run
    /// that launched it (`RunEnginePin`), mapped onto a Ready catalog entry.
    InheritedRun,
}

impl CodingConstraintSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserUi => "user_ui",
            Self::AssistantFacade => "assistant_facade",
            Self::MigratedLegacy => "migrated_legacy",
            Self::ConfiguredDefault => "configured_default",
            Self::InheritedRun => "inherited_run",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingSelectionSource {
    Fixed,
    Coordinator,
    Default,
}

impl CodingSelectionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Coordinator => "coordinator",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingBillingBasis {
    ProviderPriced,
    ChatgptEntitlement,
    External,
    Unknown,
}

impl CodingBillingBasis {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProviderPriced => "provider_priced",
            Self::ChatgptEntitlement => "chatgpt_entitlement",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingTerminalClass {
    Completed,
    Failed,
    Interrupted,
    EventBackpressure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePin {
    pub profile_id: String,
    pub definition_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CodingConstraintMode {
    Fixed {
        engine: CodingEngineKind,
        floor_profile_id: String,
        floor_definition_digest: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allowed_escalations: Vec<ProfilePin>,
    },
    Auto {
        allowed_profiles: Vec<ProfilePin>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingEngineConstraint {
    pub schema_version: u16,
    pub mode: CodingConstraintMode,
    pub source: CodingConstraintSource,
    pub digest: String,
}

impl CodingEngineConstraint {
    pub fn new(mode: CodingConstraintMode, source: CodingConstraintSource) -> Self {
        let mut constraint = Self {
            schema_version: CONSTRAINT_SCHEMA_VERSION,
            mode,
            source,
            digest: String::new(),
        };
        constraint.digest = constraint.compute_digest();
        constraint
    }

    pub fn verify(&self) -> Result<(), SelectionError> {
        if self.digest != self.compute_digest() {
            return Err(SelectionError::ConstraintDigestMismatch);
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        absorb(&mut hasher, "domain", CONSTRAINT_DIGEST_DOMAIN);
        absorb(
            &mut hasher,
            "schema_version",
            &self.schema_version.to_string(),
        );
        absorb(&mut hasher, "source", self.source.as_str());
        match &self.mode {
            CodingConstraintMode::Fixed {
                engine,
                floor_profile_id,
                floor_definition_digest,
                allowed_escalations,
            } => {
                absorb(&mut hasher, "mode", "fixed");
                absorb(&mut hasher, "engine", engine_str(*engine));
                absorb(&mut hasher, "floor_profile_id", floor_profile_id);
                absorb(
                    &mut hasher,
                    "floor_definition_digest",
                    floor_definition_digest,
                );
                absorb(
                    &mut hasher,
                    "escalation_count",
                    &allowed_escalations.len().to_string(),
                );
                for pin in allowed_escalations {
                    absorb(&mut hasher, "escalation_id", &pin.profile_id);
                    absorb(&mut hasher, "escalation_digest", &pin.definition_digest);
                }
            },
            CodingConstraintMode::Auto { allowed_profiles } => {
                absorb(&mut hasher, "mode", "auto");
                absorb(
                    &mut hasher,
                    "profile_count",
                    &allowed_profiles.len().to_string(),
                );
                for pin in allowed_profiles {
                    absorb(&mut hasher, "profile_id", &pin.profile_id);
                    absorb(&mut hasher, "definition_digest", &pin.definition_digest);
                }
            },
        }
        hasher.finalize().to_hex().to_string()
    }

    pub fn fixed_engine(&self) -> Option<CodingEngineKind> {
        match &self.mode {
            CodingConstraintMode::Fixed { engine, .. } => Some(*engine),
            CodingConstraintMode::Auto { .. } => None,
        }
    }
}

/// Execution-bearing profile definition. `label` / `description` are carried
/// for settings projection and are excluded from [`definition_digest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum CodingProfileDefinition {
    Pi {
        id: String,
        llm_profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        escalates_to: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing_basis: Option<CodingBillingBasis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    CodexAppServer {
        id: String,
        model: String,
        reasoning_effort: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        escalates_to: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing_basis: Option<CodingBillingBasis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    GrokAcp {
        id: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        escalates_to: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing_basis: Option<CodingBillingBasis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    ClaudeCode {
        id: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        escalates_to: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing_basis: Option<CodingBillingBasis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    AgyCli {
        id: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_timeout_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        escalates_to: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        billing_basis: Option<CodingBillingBasis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl CodingProfileDefinition {
    pub fn synthesized_codex_default(
        model: impl Into<String>,
        reasoning_effort: impl Into<String>,
    ) -> Result<Self, SelectionError> {
        let definition = Self::CodexAppServer {
            id: super::discovery::CODEX_DEFAULT_PROFILE_ID.to_string(),
            model: model.into(),
            reasoning_effort: reasoning_effort.into(),
            turn_timeout_secs: None,
            escalates_to: Vec::new(),
            capabilities: Vec::new(),
            billing_basis: Some(CodingBillingBasis::ChatgptEntitlement),
            label: Some("Codex".to_string()),
            description: None,
        };
        definition.validate()?;
        Ok(definition)
    }

    pub fn synthesized_grok_default(model: impl Into<String>) -> Result<Self, SelectionError> {
        let definition = Self::GrokAcp {
            id: super::discovery::GROK_DEFAULT_PROFILE_ID.to_string(),
            model: model.into(),
            turn_timeout_secs: None,
            escalates_to: Vec::new(),
            capabilities: Vec::new(),
            billing_basis: Some(CodingBillingBasis::External),
            label: Some("Grok".to_string()),
            description: None,
        };
        definition.validate()?;
        Ok(definition)
    }

    pub fn synthesized_claude_default() -> Result<Self, SelectionError> {
        let definition = Self::ClaudeCode {
            id: super::discovery::CLAUDE_DEFAULT_PROFILE_ID.to_string(),
            model: "claude".to_string(),
            turn_timeout_secs: None,
            escalates_to: Vec::new(),
            capabilities: Vec::new(),
            billing_basis: Some(CodingBillingBasis::External),
            label: Some("Claude".to_string()),
            description: None,
        };
        definition.validate()?;
        Ok(definition)
    }

    pub fn synthesized_agy_default() -> Result<Self, SelectionError> {
        let definition = Self::AgyCli {
            id: super::discovery::AGY_DEFAULT_PROFILE_ID.to_string(),
            model: "agy".to_string(),
            turn_timeout_secs: None,
            escalates_to: Vec::new(),
            capabilities: Vec::new(),
            billing_basis: Some(CodingBillingBasis::External),
            label: Some("Antigravity".to_string()),
            description: None,
        };
        definition.validate()?;
        Ok(definition)
    }

    pub fn engine(&self) -> CodingEngineKind {
        match self {
            Self::Pi { .. } => CodingEngineKind::Pi,
            Self::CodexAppServer { .. } => CodingEngineKind::CodexAppServer,
            Self::GrokAcp { .. } => CodingEngineKind::GrokAcp,
            Self::ClaudeCode { .. } => CodingEngineKind::ClaudeCode,
            Self::AgyCli { .. } => CodingEngineKind::AgyCli,
        }
    }

    pub fn push_escalation(&mut self, target: impl Into<String>) {
        let target = target.into();
        let list = match self {
            Self::Pi { escalates_to, .. }
            | Self::CodexAppServer { escalates_to, .. }
            | Self::GrokAcp { escalates_to, .. }
            | Self::ClaudeCode { escalates_to, .. }
            | Self::AgyCli { escalates_to, .. } => escalates_to,
        };
        if !list.iter().any(|id| id == &target) {
            list.push(target);
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Pi { id, .. }
            | Self::CodexAppServer { id, .. }
            | Self::GrokAcp { id, .. }
            | Self::ClaudeCode { id, .. }
            | Self::AgyCli { id, .. } => id,
        }
    }

    pub fn escalates_to(&self) -> &[String] {
        match self {
            Self::Pi { escalates_to, .. }
            | Self::CodexAppServer { escalates_to, .. }
            | Self::GrokAcp { escalates_to, .. }
            | Self::ClaudeCode { escalates_to, .. }
            | Self::AgyCli { escalates_to, .. } => escalates_to,
        }
    }

    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Pi { .. } => None,
            Self::CodexAppServer { model, .. }
            | Self::GrokAcp { model, .. }
            | Self::ClaudeCode { model, .. }
            | Self::AgyCli { model, .. } => Some(model.as_str()),
        }
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        match self {
            Self::Pi { .. }
            | Self::GrokAcp { .. }
            | Self::ClaudeCode { .. }
            | Self::AgyCli { .. } => None,
            Self::CodexAppServer {
                reasoning_effort, ..
            } => Some(reasoning_effort.as_str()),
        }
    }

    pub fn validate(&self) -> Result<(), SelectionError> {
        if self.id().is_empty() {
            return Err(SelectionError::InvalidProfile {
                reason: "profile id is empty".into(),
            });
        }
        match self {
            Self::Pi { llm_profile, .. } if llm_profile.is_empty() => {
                Err(SelectionError::InvalidProfile {
                    reason: "pi profile is missing llm_profile".into(),
                })
            },
            Self::CodexAppServer {
                model,
                reasoning_effort,
                ..
            } if model.is_empty() || reasoning_effort.is_empty() => {
                Err(SelectionError::InvalidProfile {
                    reason: "codex profile needs a concrete model and reasoning effort".into(),
                })
            },
            Self::GrokAcp { model, .. } if model.is_empty() => {
                Err(SelectionError::InvalidProfile {
                    reason: "grok profile needs a concrete model".into(),
                })
            },
            Self::ClaudeCode { model, .. } if model.is_empty() => {
                Err(SelectionError::InvalidProfile {
                    reason: "claude profile needs a concrete model".into(),
                })
            },
            Self::AgyCli { model, .. } if model.is_empty() => Err(SelectionError::InvalidProfile {
                reason: "agy profile needs a concrete model".into(),
            }),
            _ => Ok(()),
        }
    }

    pub fn definition_digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        absorb(&mut hasher, "domain", DEFINITION_DIGEST_DOMAIN);
        absorb(&mut hasher, "engine", engine_str(self.engine()));
        absorb(&mut hasher, "id", self.id());
        match self {
            Self::Pi { llm_profile, .. } => {
                absorb(&mut hasher, "llm_profile", llm_profile);
            },
            Self::CodexAppServer {
                model,
                reasoning_effort,
                ..
            } => {
                absorb(&mut hasher, "model", model);
                absorb(&mut hasher, "reasoning_effort", reasoning_effort);
            },
            Self::GrokAcp { model, .. } => {
                absorb(&mut hasher, "model", model);
            },
            Self::ClaudeCode { model, .. } => {
                absorb(&mut hasher, "model", model);
            },
            Self::AgyCli { model, .. } => {
                absorb(&mut hasher, "model", model);
            },
        }
        let (timeout, capabilities, billing, escalates_to) = match self {
            Self::Pi {
                turn_timeout_secs,
                capabilities,
                billing_basis,
                escalates_to,
                ..
            }
            | Self::CodexAppServer {
                turn_timeout_secs,
                capabilities,
                billing_basis,
                escalates_to,
                ..
            }
            | Self::GrokAcp {
                turn_timeout_secs,
                capabilities,
                billing_basis,
                escalates_to,
                ..
            }
            | Self::ClaudeCode {
                turn_timeout_secs,
                capabilities,
                billing_basis,
                escalates_to,
                ..
            }
            | Self::AgyCli {
                turn_timeout_secs,
                capabilities,
                billing_basis,
                escalates_to,
                ..
            } => (turn_timeout_secs, capabilities, billing_basis, escalates_to),
        };
        absorb_optional(
            &mut hasher,
            "turn_timeout_secs",
            timeout.map(|secs| secs.to_string()).as_deref(),
        );
        absorb(
            &mut hasher,
            "billing_basis",
            billing.map(CodingBillingBasis::as_str).unwrap_or("none"),
        );
        let mut caps = capabilities.clone();
        caps.sort();
        absorb(&mut hasher, "capability_count", &caps.len().to_string());
        for cap in &caps {
            absorb(&mut hasher, "capability", cap);
        }
        absorb(
            &mut hasher,
            "escalation_count",
            &escalates_to.len().to_string(),
        );
        for target in escalates_to {
            absorb(&mut hasher, "escalates_to", target);
        }
        hasher.finalize().to_hex().to_string()
    }

    fn pin(&self) -> ProfilePin {
        ProfilePin {
            profile_id: self.id().to_string(),
            definition_digest: self.definition_digest(),
        }
    }
}

/// Current catalog row used only to revalidate a pin at dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCatalogEntry {
    pub definition: CodingProfileDefinition,
    pub eligible: bool,
}

impl ProfileCatalogEntry {
    pub fn new(
        definition: CodingProfileDefinition,
        eligible: bool,
    ) -> Result<Self, SelectionError> {
        definition.validate()?;
        Ok(Self {
            definition,
            eligible,
        })
    }
}

pub fn fixed_constraint_from_profile(
    floor: &CodingProfileDefinition,
    catalog: &[ProfileCatalogEntry],
    source: CodingConstraintSource,
) -> Result<CodingEngineConstraint, SelectionError> {
    floor.validate()?;
    let mut allowed_escalations = Vec::new();
    for target_id in floor.escalates_to() {
        if target_id == floor.id() {
            continue;
        }
        let target = catalog
            .iter()
            .find(|entry| entry.definition.id() == target_id)
            .ok_or_else(|| SelectionError::EscalationUnknown {
                profile_id: target_id.clone(),
            })?;
        if target.definition.engine() != floor.engine() {
            return Err(SelectionError::EscalationNotSameEngine {
                profile_id: target_id.clone(),
            });
        }
        target.definition.validate()?;
        allowed_escalations.push(target.definition.pin());
    }
    Ok(CodingEngineConstraint::new(
        CodingConstraintMode::Fixed {
            engine: floor.engine(),
            floor_profile_id: floor.id().to_string(),
            floor_definition_digest: floor.definition_digest(),
            allowed_escalations,
        },
        source,
    ))
}

/// Snapshot currently eligible profiles. Pins are sorted by id so catalog
/// iteration order cannot silently change the digest.
pub fn auto_constraint(
    eligible: &[ProfileCatalogEntry],
    source: CodingConstraintSource,
) -> Result<CodingEngineConstraint, SelectionError> {
    let mut pins = Vec::new();
    for entry in eligible {
        if !entry.eligible {
            continue;
        }
        entry.definition.validate()?;
        pins.push(entry.definition.pin());
    }
    pins.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
    pins.dedup_by(|left, right| left.profile_id == right.profile_id);
    Ok(CodingEngineConstraint::new(
        CodingConstraintMode::Auto {
            allowed_profiles: pins,
        },
        source,
    ))
}

/// Request-side choice, independent of the VibeDev wire type so this module
/// does not depend on the run service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedCodingChoice<'a> {
    Auto,
    Profile { profile_id: &'a str },
}

/// Pin a VibeDev request against the catalog that was eligible at submission.
///
/// `None` is the configured default and becomes a **fixed** Pi floor. Auto
/// snapshots every currently eligible entry. A named profile must already be
/// in the catalog; it cannot widen the request later.
pub fn constraint_from_requested_choice(
    choice: Option<RequestedCodingChoice<'_>>,
    catalog: &[ProfileCatalogEntry],
    default_profile_id: &str,
    source: CodingConstraintSource,
) -> Result<CodingEngineConstraint, SelectionError> {
    match choice {
        None => {
            let floor = catalog_definition(catalog, default_profile_id)?;
            fixed_constraint_from_profile(floor, catalog, source)
        },
        Some(RequestedCodingChoice::Auto) => auto_constraint(catalog, source),
        Some(RequestedCodingChoice::Profile { profile_id }) => {
            let floor = catalog_definition(catalog, profile_id)?;
            fixed_constraint_from_profile(floor, catalog, source)
        },
    }
}

/// Current cockpit policy until `coding.profiles[].escalates_to` exists.
pub const LEGACY_COCKPIT_FLOOR_PROFILE_ID: &str = "coding-balanced";
pub const LEGACY_COCKPIT_PREMIUM_PROFILE_ID: &str = "coding-premium";

/// If both cockpit floor and premium rows exist, pin the one-hop escalation
/// the composer already documents.
pub fn apply_legacy_cockpit_escalations(entries: &mut [ProfileCatalogEntry]) {
    let has_premium = entries
        .iter()
        .any(|entry| entry.eligible && entry.definition.id() == LEGACY_COCKPIT_PREMIUM_PROFILE_ID);
    if !has_premium {
        return;
    }
    for entry in entries {
        if entry.definition.id() == LEGACY_COCKPIT_FLOOR_PROFILE_ID {
            entry
                .definition
                .push_escalation(LEGACY_COCKPIT_PREMIUM_PROFILE_ID);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VibeDevDispatchDecision {
    NotVibeDev,
    Selected(ResolvedCodingEngineSelection),
}

/// Choose the profile a VibeDev coding invocation may use.
///
/// Non-VibeDev callers keep their existing argument→agent→default path.
/// VibeDev with a stored pin verifies it. VibeDev without a pin hydrates a
/// fixed default from the current catalog (including the cockpit one-hop)
/// so old records do not silently accept a Codex profile.
pub fn decide_vibedev_dispatch_profile(
    in_vibedev_chain: bool,
    stored_constraint: Option<CodingEngineConstraint>,
    proposed_profile_id: Option<&str>,
    catalog: &[ProfileCatalogEntry],
    default_profile_id: &str,
) -> Result<VibeDevDispatchDecision, SelectionError> {
    if !in_vibedev_chain {
        return Ok(VibeDevDispatchDecision::NotVibeDev);
    }
    let constraint = match stored_constraint {
        Some(constraint) => {
            constraint.verify()?;
            constraint
        },
        None => {
            let floor = catalog_definition(catalog, default_profile_id)?;
            fixed_constraint_from_profile(floor, catalog, CodingConstraintSource::MigratedLegacy)?
        },
    };
    let selection = resolve_proposed_profile(
        &constraint,
        proposed_profile_id,
        catalog,
        default_profile_id,
    )?;
    Ok(VibeDevDispatchDecision::Selected(selection))
}

fn catalog_definition<'a>(
    catalog: &'a [ProfileCatalogEntry],
    profile_id: &str,
) -> Result<&'a CodingProfileDefinition, SelectionError> {
    catalog
        .iter()
        .find(|entry| entry.definition.id() == profile_id)
        .map(|entry| &entry.definition)
        .ok_or_else(|| SelectionError::UnknownProfile {
            profile_id: profile_id.to_string(),
        })
}

/// Missing historical tasks hydrate as a fixed Pi floor. The definition must
/// already be Pi; this does not invent an `llm_profile`.
pub fn hydrate_legacy_fixed_pi(
    floor: &CodingProfileDefinition,
) -> Result<CodingEngineConstraint, SelectionError> {
    if floor.engine() != CodingEngineKind::Pi {
        return Err(SelectionError::FloorMustBePi);
    }
    fixed_constraint_from_profile(floor, &[], CodingConstraintSource::MigratedLegacy)
}

/// Opaque engine-bound continuation. One native session or thread, never
/// shared across engines or root chains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingContinuationRef {
    pub engine: CodingEngineKind,
    pub native_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_turn_id: Option<String>,
    pub scope_binding_digest: String,
    pub project_binding_digest: String,
    pub root_task_id: String,
    pub generation: u64,
}

impl CodingContinuationRef {
    pub fn for_pi_session(
        native_session_id: impl Into<String>,
        scope_root: &Path,
        workspace_root: &Path,
        root_task_id: Option<&str>,
    ) -> Self {
        Self::for_engine(
            CodingEngineKind::Pi,
            native_session_id,
            scope_root,
            workspace_root,
            root_task_id,
        )
    }

    pub fn for_codex_thread(
        native_session_id: impl Into<String>,
        scope_root: &Path,
        workspace_root: &Path,
        root_task_id: Option<&str>,
    ) -> Self {
        Self::for_engine(
            CodingEngineKind::CodexAppServer,
            native_session_id,
            scope_root,
            workspace_root,
            root_task_id,
        )
    }

    pub fn for_grok_session(
        native_session_id: impl Into<String>,
        scope_root: &Path,
        workspace_root: &Path,
        root_task_id: Option<&str>,
    ) -> Self {
        Self::for_engine(
            CodingEngineKind::GrokAcp,
            native_session_id,
            scope_root,
            workspace_root,
            root_task_id,
        )
    }

    pub fn for_claude_session(
        native_session_id: impl Into<String>,
        scope_root: &Path,
        workspace_root: &Path,
        root_task_id: Option<&str>,
    ) -> Self {
        Self::for_engine(
            CodingEngineKind::ClaudeCode,
            native_session_id,
            scope_root,
            workspace_root,
            root_task_id,
        )
    }

    pub fn for_agy_session(
        native_session_id: impl Into<String>,
        scope_root: &Path,
        workspace_root: &Path,
        root_task_id: Option<&str>,
    ) -> Self {
        Self::for_engine(
            CodingEngineKind::AgyCli,
            native_session_id,
            scope_root,
            workspace_root,
            root_task_id,
        )
    }

    /// The engine-keyed constructor the five named ones delegate to. Public
    /// because a caller that already holds a [`CodingEngineKind`] value — the
    /// live-session reporter — would otherwise have to re-match it back onto
    /// one of the five, which is a fifth place to forget an engine.
    pub fn for_engine(
        engine: CodingEngineKind,
        native_session_id: impl Into<String>,
        scope_root: &Path,
        workspace_root: &Path,
        root_task_id: Option<&str>,
    ) -> Self {
        Self {
            engine,
            native_session_id: native_session_id.into(),
            last_completed_turn_id: None,
            scope_binding_digest: binding_digest("scope", scope_root),
            project_binding_digest: binding_digest("project", workspace_root),
            root_task_id: root_task_id.unwrap_or("").to_string(),
            generation: 0,
        }
    }
}

fn binding_digest(label: &str, path: &Path) -> String {
    let mut hasher = blake3::Hasher::new();
    absorb(&mut hasher, "domain", CONTINUATION_DIGEST_DOMAIN);
    absorb(&mut hasher, "label", label);
    absorb(&mut hasher, "path", &path.display().to_string());
    hasher.finalize().to_hex().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCodingEngineSelection {
    pub profile_id: String,
    pub engine: CodingEngineKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness_receipt_digest: Option<String>,
    pub adapter_revision: String,
    pub constraint_digest: String,
    pub selection_source: CodingSelectionSource,
}

impl ResolvedCodingEngineSelection {
    pub fn identity_digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        absorb(&mut hasher, "domain", SELECTION_DIGEST_DOMAIN);
        absorb(&mut hasher, "profile_id", &self.profile_id);
        absorb(&mut hasher, "engine", engine_str(self.engine));
        absorb_optional(&mut hasher, "model", self.model.as_deref());
        absorb_optional(
            &mut hasher,
            "reasoning_effort",
            self.reasoning_effort.as_deref(),
        );
        absorb_optional(
            &mut hasher,
            "readiness_revision",
            self.readiness_revision
                .map(|rev| rev.to_string())
                .as_deref(),
        );
        absorb_optional(
            &mut hasher,
            "readiness_receipt_digest",
            self.readiness_receipt_digest.as_deref(),
        );
        absorb(&mut hasher, "adapter_revision", &self.adapter_revision);
        absorb(&mut hasher, "constraint_digest", &self.constraint_digest);
        absorb(
            &mut hasher,
            "selection_source",
            self.selection_source.as_str(),
        );
        hasher.finalize().to_hex().to_string()
    }
}

/// Resolve a coordinator `coding_profile` proposal against a trusted
/// constraint.
///
/// `proposed_profile_id = None` means the coordinator omitted the field.
/// For a fixed constraint that selects the floor; for Auto it selects
/// `default_profile_id` if that id was pinned at submission.
pub fn resolve_proposed_profile(
    constraint: &CodingEngineConstraint,
    proposed_profile_id: Option<&str>,
    catalog: &[ProfileCatalogEntry],
    default_profile_id: &str,
) -> Result<ResolvedCodingEngineSelection, SelectionError> {
    constraint.verify()?;
    let (profile_id, selection_source) = match (&constraint.mode, proposed_profile_id) {
        (
            CodingConstraintMode::Fixed {
                floor_profile_id, ..
            },
            None,
        ) => (floor_profile_id.clone(), CodingSelectionSource::Fixed),
        (CodingConstraintMode::Fixed { .. }, Some(id)) => {
            (id.to_string(), selection_source_for_fixed(constraint, id)?)
        },
        (CodingConstraintMode::Auto { .. }, None) => (
            default_profile_id.to_string(),
            CodingSelectionSource::Default,
        ),
        (CodingConstraintMode::Auto { .. }, Some(id)) => {
            (id.to_string(), CodingSelectionSource::Coordinator)
        },
    };

    let pin = find_pin(constraint, &profile_id).ok_or_else(|| {
        if let (Some(engine), Some(entry)) = (
            constraint.fixed_engine(),
            catalog
                .iter()
                .find(|entry| entry.definition.id() == profile_id),
        ) {
            if entry.definition.engine() != engine {
                return SelectionError::CrossEngineProposal {
                    constraint_engine: engine,
                    proposed_engine: entry.definition.engine(),
                };
            }
        }
        if catalog
            .iter()
            .any(|entry| entry.definition.id() == profile_id)
        {
            SelectionError::ProfileNotPinned {
                profile_id: profile_id.clone(),
            }
        } else {
            SelectionError::UnknownProfile {
                profile_id: profile_id.clone(),
            }
        }
    })?;

    let entry = catalog
        .iter()
        .find(|entry| entry.definition.id() == profile_id)
        .ok_or_else(|| SelectionError::UnknownProfile {
            profile_id: profile_id.clone(),
        })?;
    if entry.definition.definition_digest() != pin.definition_digest {
        return Err(SelectionError::ProfileChanged {
            profile_id: profile_id.clone(),
        });
    }
    if !entry.eligible {
        return Err(SelectionError::ProfileIneligible {
            profile_id: profile_id.clone(),
        });
    }
    if let Some(engine) = constraint.fixed_engine() {
        if entry.definition.engine() != engine {
            return Err(SelectionError::CrossEngineProposal {
                constraint_engine: engine,
                proposed_engine: entry.definition.engine(),
            });
        }
    }

    Ok(ResolvedCodingEngineSelection {
        profile_id,
        engine: entry.definition.engine(),
        model: entry.definition.model().map(str::to_string),
        reasoning_effort: entry.definition.reasoning_effort().map(str::to_string),
        readiness_revision: None,
        readiness_receipt_digest: None,
        adapter_revision: SELECTION_CONTRACT_REVISION.to_string(),
        constraint_digest: constraint.digest.clone(),
        selection_source,
    })
}

fn selection_source_for_fixed(
    constraint: &CodingEngineConstraint,
    proposed_id: &str,
) -> Result<CodingSelectionSource, SelectionError> {
    match &constraint.mode {
        CodingConstraintMode::Fixed {
            floor_profile_id, ..
        } if proposed_id == floor_profile_id => Ok(CodingSelectionSource::Fixed),
        CodingConstraintMode::Fixed { .. } => Ok(CodingSelectionSource::Coordinator),
        CodingConstraintMode::Auto { .. } => Ok(CodingSelectionSource::Coordinator),
    }
}

fn pinned_definition_digest(
    constraint: &CodingEngineConstraint,
    profile_id: &str,
) -> Option<String> {
    match &constraint.mode {
        CodingConstraintMode::Fixed {
            floor_profile_id,
            floor_definition_digest,
            allowed_escalations,
            engine: _,
        } => {
            if floor_profile_id == profile_id {
                Some(floor_definition_digest.clone())
            } else {
                allowed_escalations
                    .iter()
                    .find(|pin| pin.profile_id == profile_id)
                    .map(|pin| pin.definition_digest.clone())
            }
        },
        CodingConstraintMode::Auto { allowed_profiles } => allowed_profiles
            .iter()
            .find(|pin| pin.profile_id == profile_id)
            .map(|pin| pin.definition_digest.clone()),
    }
}

fn find_pin(constraint: &CodingEngineConstraint, profile_id: &str) -> Option<ProfilePin> {
    pinned_definition_digest(constraint, profile_id).map(|definition_digest| ProfilePin {
        profile_id: profile_id.to_string(),
        definition_digest,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingInvocationRef {
    pub execution_id: String,
    pub invocation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CodingDispatchState {
    Prepared {
        canonical_input_digest: String,
        /// The provider turn a reconciliation would match new turns against.
        ///
        /// # ALWAYS `None` in production
        ///
        /// **Verified 2026-08-29.** The one production write of this field is
        /// [`super::ledger::prepare_coding_invocation`], which writes `None`;
        /// [`CodingDispatchState::mark_request_may_have_started`] carries that
        /// `None` across into `RequestMayHaveStarted`. Every `Some` in the tree
        /// is a test fixture. It is `Option` because it was designed to be
        /// filled, not because it is sometimes filled.
        ///
        /// Its sole reader is `codex_lifecycle::reconcile_dispatch`, which is
        /// superseded and uncalled — so today this field is written by one
        /// function that always writes `None` and read by nobody.
        ///
        /// # What would have to write it
        ///
        /// The Codex thread's last completed turn id at the moment the
        /// invocation is prepared, which is available: a resumed thread's
        /// `CodingContinuationRef::last_completed_turn_id` is exactly that
        /// value. `prepare_coding_invocation` does not receive the continuation
        /// — the resume thread is bound later, in
        /// `run_coding_task::bind_codex_turn_options` — so filling this means
        /// either threading the continuation into `prepare_coding_invocation` or
        /// a second ledger write after the bind. Neither is worth doing while
        /// the only reader is superseded; both are listed so the next reader
        /// knows the value is reachable rather than unknowable.
        ///
        /// A fresh thread has no base turn and `None` is the honest answer
        /// there, which is why the reconciliation could never be made to work by
        /// this field alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_turn_id: Option<String>,
    },
    RequestMayHaveStarted {
        canonical_input_digest: String,
        /// Carried across from `Prepared`, and therefore always `None` in
        /// production for the same reason. See the `Prepared` variant's
        /// `base_turn_id` above.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_turn_id: Option<String>,
    },
    Accepted {
        provider_turn_id: String,
    },
    Settled {
        terminal: CodingTerminalClass,
    },
    /// **Unreachable on disk.** Its only writer,
    /// [`super::ledger::mark_invocation_unknown`], has no callers; see that
    /// function for why the ambiguous-dispatch outcome now lives on the loop's
    /// effect ledger instead.
    DispatchUnknown,
}

impl CodingDispatchState {
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Prepared { .. } => "prepared",
            Self::RequestMayHaveStarted { .. } => "request_may_have_started",
            Self::Accepted { .. } => "accepted",
            Self::Settled { .. } => "settled",
            Self::DispatchUnknown => "dispatch_unknown",
        }
    }

    /// Conservative transition immediately before the first `turn/start` write.
    /// A crash between this commit and the pipe write stays ambiguous.
    pub fn mark_request_may_have_started(self) -> Result<Self, SelectionError> {
        match self {
            Self::Prepared {
                canonical_input_digest,
                base_turn_id,
            } => Ok(Self::RequestMayHaveStarted {
                canonical_input_digest,
                base_turn_id,
            }),
            other => Err(SelectionError::InvalidDispatchTransition {
                from: other.class_name(),
                to: "request_may_have_started",
            }),
        }
    }

    /// Only a `Prepared` dispatch may be re-fired without asking anybody.
    ///
    /// # Nothing in production consults this — and the property still holds
    ///
    /// **Verified 2026-08-29.** Its only non-test caller is
    /// `codex_lifecycle::reconcile_dispatch`, which is itself superseded and
    /// uncalled. So this predicate is a correct statement that no production
    /// path currently reads, and it is worth saying out loud because the
    /// conclusion "a started coding job is never re-fired automatically" is
    /// TRUE and is easy to attribute here by mistake.
    ///
    /// What actually delivers it is the loop: a coding job declares
    /// `RetrySafety::Reattachable`, so `EffectLedger::disposition` answers
    /// `EffectDisposition::Reattach` and never `Refire` — the refusal is made
    /// from the effect row, before this record is consulted at all.
    pub fn automatic_retry_allowed(&self) -> bool {
        matches!(self, Self::Prepared { .. })
    }

    pub fn mark_accepted(
        self,
        provider_turn_id: impl Into<String>,
    ) -> Result<Self, SelectionError> {
        match self {
            Self::Prepared { .. } | Self::RequestMayHaveStarted { .. } => Ok(Self::Accepted {
                provider_turn_id: provider_turn_id.into(),
            }),
            other => Err(SelectionError::InvalidDispatchTransition {
                from: other.class_name(),
                to: "accepted",
            }),
        }
    }

    pub fn mark_unknown(self) -> Self {
        match self {
            Self::Settled { .. } => self,
            _ => Self::DispatchUnknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingEngineUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Decimal USD when the provider reports a price. Absent means unknown,
    /// never `"0"` as a stand-in for entitlement/unpriced usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<String>,
    pub billing_basis: CodingBillingBasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingInvocationState {
    pub invocation_id: String,
    pub schema_version: u16,
    pub constraint_digest: String,
    pub selection: ResolvedCodingEngineSelection,
    pub dispatch: CodingDispatchState,
    /// Stable identity of the tool-action input. Kept outside `dispatch` so a
    /// later `Settled` entry can still be matched by a retried `Prepared`.
    pub canonical_input_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<CodingContinuationRef>,
    /// The session this invocation is running on **while the turn is still
    /// open**, written the moment the engine first reports one.
    ///
    /// # Why this is not just an early write to `continuation`
    ///
    /// `continuation` means *the session this invocation finished on*, and the
    /// resume binds read it that way: `latest_ledger_continuation` answers "the
    /// most recent session in this ledger" to a **different** invocation that is
    /// about to open a turn, and all four binds that resume by session id —
    /// Codex, Grok, Claude, Agy — reach it through
    /// `run_coding_task::resolve_previous_chain_continuation`. (Until 2026-08-29
    /// the Codex bind had its own reverse scan over this field instead; it now
    /// goes through the same helper, which is why one name covers them all.)
    /// Filling `continuation` early would put a session with a turn
    /// still in flight in front of those scans — so a second coding job in the
    /// same execution could bind `resume_thread_id` to a thread another process
    /// is at that moment driving, and two turns would interleave on one native
    /// session. That is a worse failure than the one the early write fixes.
    ///
    /// Keeping it in its own field means every existing reader keeps its
    /// existing meaning, and exactly one lookup — `invocation_continuation`,
    /// which is keyed by invocation id and whose only production caller is
    /// `WorkerHost::reattach_state` — falls back to it. A reattach
    /// asks about *this* invocation by name, so a live session is the right
    /// answer there and only there.
    ///
    /// `last_completed_turn_id` is `None` on this value, always: no turn has
    /// completed. It is the field that keeps the ref honest mid-flight.
    ///
    /// # Adding this field is not backward compatible, and the exposure is real
    ///
    /// This struct is `#[serde(deny_unknown_fields)]`, so a binary that predates
    /// this field cannot parse a ledger carrying it — and the failure is hard,
    /// not a degrade: [`super::ledger::load_coding_ledger`] surfaces
    /// `LedgerStoreError::Parse`, `prepare_coding_invocation` propagates it, and
    /// `run_coding_task` refuses the job. The entry is never rewritten by the
    /// old binary, so the refusal persists for that execution dir rather than
    /// clearing itself.
    ///
    /// The window is narrow — only a ledger crashed mid-turn carries the field,
    /// since [`super::ledger::attach_invocation_continuation`] clears it on the
    /// success path — but it is a rollback **across** such a crash, which is
    /// exactly when a rollback happens.
    ///
    /// **Decision owed to the branch owner** (assessed 2026-08-29, not taken
    /// here). Dropping `deny_unknown_fields` would fix it and weaken every other
    /// field's validation permanently, for a rollback-only benefit; moving the
    /// live session to a sidecar file would leave this struct byte-identical but
    /// turns one atomic ledger write into two and invents a consistency question
    /// the single file does not have. Accepting the exposure is the third
    /// option, and note that a rolled-back binary also lacks
    /// `attach_live_invocation_session` and `mark_invocation_may_have_started`,
    /// so the reattach this field serves is gone under rollback regardless — the
    /// parse failure only makes that loud.
    ///
    /// `INVOCATION_SCHEMA_VERSION` was deliberately NOT bumped for this field:
    /// it is absorbed into [`CodingInvocationState::authority_digest`], so a bump
    /// would make every re-run present a different digest for the same
    /// invocation id and `CodingExecutionLedger::append_or_reuse` would refuse it
    /// as `LedgerIntegrityConflict`. The version is not the compatibility lever
    /// it looks like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_continuation: Option<CodingContinuationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<CodingInvocationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_envelope_digest: Option<String>,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CodingEngineUsage>,
}

impl CodingInvocationState {
    pub fn authority_digest(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        absorb(&mut hasher, "domain", INVOCATION_AUTHORITY_DOMAIN);
        absorb(
            &mut hasher,
            "schema_version",
            &self.schema_version.to_string(),
        );
        absorb(&mut hasher, "constraint_digest", &self.constraint_digest);
        absorb(&mut hasher, "selection", &self.selection.identity_digest());
        absorb(
            &mut hasher,
            "canonical_input_digest",
            &self.canonical_input_digest,
        );
        absorb_optional(
            &mut hasher,
            "predecessor_execution",
            self.predecessor
                .as_ref()
                .map(|pred| pred.execution_id.as_str()),
        );
        absorb_optional(
            &mut hasher,
            "predecessor_invocation",
            self.predecessor
                .as_ref()
                .map(|pred| pred.invocation_id.as_str()),
        );
        absorb_optional(
            &mut hasher,
            "context_envelope_digest",
            self.context_envelope_digest.as_deref(),
        );
        hasher.finalize().to_hex().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingExecutionLedger {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocations: Vec<CodingInvocationState>,
}

impl CodingExecutionLedger {
    pub fn append_or_reuse(
        &mut self,
        entry: CodingInvocationState,
        cap: usize,
    ) -> Result<&CodingInvocationState, SelectionError> {
        if let Some(index) = self
            .invocations
            .iter()
            .position(|existing| existing.invocation_id == entry.invocation_id)
        {
            if self.invocations[index].authority_digest() != entry.authority_digest() {
                return Err(SelectionError::LedgerIntegrityConflict {
                    invocation_id: entry.invocation_id,
                });
            }
            return Ok(&self.invocations[index]);
        }
        if self.invocations.len() >= cap {
            return Err(SelectionError::LedgerCapReached { cap });
        }
        self.invocations.push(entry);
        Ok(self
            .invocations
            .last()
            .expect("just pushed a coding invocation"))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn pi(id: &str, llm: &str, escalates_to: &[&str]) -> CodingProfileDefinition {
        CodingProfileDefinition::Pi {
            id: id.to_string(),
            llm_profile: llm.to_string(),
            turn_timeout_secs: None,
            escalates_to: escalates_to.iter().map(|id| (*id).to_string()).collect(),
            capabilities: Vec::new(),
            billing_basis: Some(CodingBillingBasis::ProviderPriced),
            label: Some(format!("{id} label")),
            description: Some("ignored".into()),
        }
    }

    fn codex(id: &str, model: &str) -> CodingProfileDefinition {
        CodingProfileDefinition::CodexAppServer {
            id: id.to_string(),
            model: model.to_string(),
            reasoning_effort: "medium".into(),
            turn_timeout_secs: None,
            escalates_to: Vec::new(),
            capabilities: Vec::new(),
            billing_basis: Some(CodingBillingBasis::ChatgptEntitlement),
            label: Some("Codex · Default".into()),
            description: Some("ignored".into()),
        }
    }

    fn eligible(definition: CodingProfileDefinition) -> ProfileCatalogEntry {
        ProfileCatalogEntry::new(definition, true).expect("valid profile")
    }

    fn catalog(defs: &[CodingProfileDefinition]) -> Vec<ProfileCatalogEntry> {
        defs.iter().cloned().map(eligible).collect()
    }

    fn prepared_invocation(
        id: &str,
        constraint: &CodingEngineConstraint,
        selection: ResolvedCodingEngineSelection,
        input: &str,
    ) -> CodingInvocationState {
        CodingInvocationState {
            invocation_id: id.to_string(),
            schema_version: INVOCATION_SCHEMA_VERSION,
            constraint_digest: constraint.digest.clone(),
            selection,
            dispatch: CodingDispatchState::Prepared {
                canonical_input_digest: input.to_string(),
                base_turn_id: None,
            },
            canonical_input_digest: input.to_string(),
            continuation: None,
            live_continuation: None,
            predecessor: None,
            context_envelope_digest: None,
            generation: 1,
            usage: None,
        }
    }

    #[test]
    fn coding_engine_kind_keeps_the_serialized_pi_tag() {
        assert_eq!(
            serde_json::to_string(&CodingEngineKind::Pi).expect("pi"),
            "\"pi\""
        );
        assert_eq!(
            serde_json::to_string(&CodingEngineKind::CodexAppServer).expect("codex"),
            "\"codex_app_server\""
        );
        assert_eq!(
            serde_json::to_string(&CodingEngineKind::GrokAcp).expect("grok"),
            "\"grok_acp\""
        );
        assert_eq!(
            serde_json::to_string(&CodingEngineKind::ClaudeCode).expect("claude"),
            "\"claude_code\""
        );
        assert_eq!(
            serde_json::to_string(&CodingEngineKind::AgyCli).expect("agy"),
            "\"agy_cli\""
        );
        assert_eq!(engine_str(CodingEngineKind::GrokAcp), "grok_acp");
        assert_eq!(engine_str(CodingEngineKind::ClaudeCode), "claude_code");
        assert_eq!(engine_str(CodingEngineKind::AgyCli), "agy_cli");
        assert_eq!(engine_str(CodingEngineKind::Pi), "pi");
        assert_eq!(
            engine_str(CodingEngineKind::CodexAppServer),
            "codex_app_server"
        );
    }

    #[test]
    fn synthesized_grok_default_validates_as_grok_acp() {
        let definition =
            CodingProfileDefinition::synthesized_grok_default("grok-build").expect("grok");
        definition.validate().expect("valid");
        assert_eq!(definition.engine(), CodingEngineKind::GrokAcp);
        assert_eq!(definition.id(), "grok-default");
        assert_eq!(definition.model(), Some("grok-build"));
        assert_eq!(definition.reasoning_effort(), None);
    }

    #[test]
    fn synthesized_claude_default_validates_as_claude_code() {
        let definition = CodingProfileDefinition::synthesized_claude_default().expect("claude");
        definition.validate().expect("valid");
        assert_eq!(definition.engine(), CodingEngineKind::ClaudeCode);
        assert_eq!(definition.id(), "claude-default");
        assert_eq!(definition.model(), Some("claude"));
        assert_eq!(definition.reasoning_effort(), None);
        assert_eq!(
            match definition {
                CodingProfileDefinition::ClaudeCode { billing_basis, .. } => billing_basis,
                _ => None,
            },
            Some(CodingBillingBasis::External)
        );
    }

    #[test]
    fn synthesized_agy_default_validates_as_agy_cli() {
        let definition = CodingProfileDefinition::synthesized_agy_default().expect("agy");
        definition.validate().expect("valid");
        assert_eq!(definition.engine(), CodingEngineKind::AgyCli);
        assert_eq!(definition.id(), "agy-default");
        assert_eq!(definition.model(), Some("agy"));
    }

    #[test]
    fn label_edits_do_not_change_a_definition_digest() {
        let mut original = pi("coding-balanced", "cheap", &["coding-premium"]);
        let first = original.definition_digest();
        match &mut original {
            CodingProfileDefinition::Pi {
                label, description, ..
            } => {
                *label = Some("Balanced (renamed)".into());
                *description = Some("new copy".into());
            },
            _ => unreachable!(),
        }
        assert_eq!(original.definition_digest(), first);
    }

    #[test]
    fn a_codex_profile_without_a_concrete_model_or_effort_is_invalid() {
        let mut broken = codex("codex-default", "gpt-5.6-luna");
        match &mut broken {
            CodingProfileDefinition::CodexAppServer { model, .. } => {
                *model = String::new();
            },
            _ => unreachable!(),
        }
        assert!(broken.validate().is_err());
    }

    #[test]
    fn changing_llm_profile_or_escalation_changes_the_definition_digest() {
        let balanced = pi("coding-balanced", "cheap", &["coding-premium"]);
        let other_model = pi("coding-balanced", "expensive", &["coding-premium"]);
        let no_escalation = pi("coding-balanced", "cheap", &[]);
        assert_ne!(
            balanced.definition_digest(),
            other_model.definition_digest()
        );
        assert_ne!(
            balanced.definition_digest(),
            no_escalation.definition_digest()
        );
    }

    #[test]
    fn constraint_digest_ignores_its_own_digest_field() {
        let floor = pi("coding-balanced", "cheap", &[]);
        let constraint = hydrate_legacy_fixed_pi(&floor).expect("legacy pi");
        let recomputed = constraint.compute_digest();
        assert_eq!(constraint.digest, recomputed);
        let mut tampered = constraint.clone();
        tampered.digest = "0".repeat(64);
        assert_eq!(tampered.compute_digest(), recomputed);
        assert!(matches!(
            tampered.verify(),
            Err(SelectionError::ConstraintDigestMismatch)
        ));
    }

    #[test]
    fn unknown_constraint_fields_fail_closed() {
        let err = serde_json::from_str::<CodingEngineConstraint>(
            r#"{"schema_version":1,"mode":{"mode":"auto","allowed_profiles":[]},"source":"user_ui","digest":"x","engine":"pi"}"#,
        );
        assert!(err.is_err(), "{err:?}");
    }

    #[test]
    fn fixed_pi_rejects_a_codex_proposal() {
        let floor = pi("coding-balanced", "cheap", &["coding-premium"]);
        let premium = pi("coding-premium", "expensive", &[]);
        let default = codex("codex-default", "gpt-5.6-luna");
        let catalog = catalog(&[floor.clone(), premium, default.clone()]);
        let constraint =
            fixed_constraint_from_profile(&floor, &catalog, CodingConstraintSource::UserUi)
                .expect("fixed pi");
        let err = resolve_proposed_profile(
            &constraint,
            Some("codex-default"),
            &catalog,
            "coding-balanced",
        )
        .expect_err("cross-engine");
        assert!(
            matches!(
                err,
                SelectionError::CrossEngineProposal {
                    constraint_engine: CodingEngineKind::Pi,
                    proposed_engine: CodingEngineKind::CodexAppServer,
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn fixed_codex_rejects_a_pi_proposal() {
        let default = codex("codex-default", "gpt-5.6-luna");
        let balanced = pi("coding-balanced", "cheap", &[]);
        let catalog = catalog(&[default.clone(), balanced]);
        let constraint =
            fixed_constraint_from_profile(&default, &catalog, CodingConstraintSource::UserUi)
                .expect("fixed codex");
        let err = resolve_proposed_profile(
            &constraint,
            Some("coding-balanced"),
            &catalog,
            "codex-default",
        )
        .expect_err("cross-engine");
        assert!(
            matches!(
                err,
                SelectionError::CrossEngineProposal {
                    constraint_engine: CodingEngineKind::CodexAppServer,
                    proposed_engine: CodingEngineKind::Pi,
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn fixed_pi_accepts_the_floor_and_its_one_hop_escalation() {
        let floor = pi("coding-balanced", "cheap", &["coding-premium"]);
        let premium = pi("coding-premium", "expensive", &["coding-ultra"]);
        let ultra = pi("coding-ultra", "ultra", &[]);
        let catalog = catalog(&[floor.clone(), premium, ultra]);
        let constraint =
            fixed_constraint_from_profile(&floor, &catalog, CodingConstraintSource::UserUi)
                .expect("fixed");
        match &constraint.mode {
            CodingConstraintMode::Fixed {
                allowed_escalations,
                ..
            } => {
                assert_eq!(
                    allowed_escalations
                        .iter()
                        .map(|pin| pin.profile_id.as_str())
                        .collect::<Vec<_>>(),
                    ["coding-premium"]
                );
            },
            _ => panic!("expected fixed"),
        }

        let omitted = resolve_proposed_profile(&constraint, None, &catalog, "coding-balanced")
            .expect("floor");
        assert_eq!(omitted.profile_id, "coding-balanced");
        assert_eq!(omitted.selection_source, CodingSelectionSource::Fixed);

        let escalated = resolve_proposed_profile(
            &constraint,
            Some("coding-premium"),
            &catalog,
            "coding-balanced",
        )
        .expect("one-hop");
        assert_eq!(escalated.profile_id, "coding-premium");
        assert_eq!(
            escalated.selection_source,
            CodingSelectionSource::Coordinator
        );

        let err = resolve_proposed_profile(
            &constraint,
            Some("coding-ultra"),
            &catalog,
            "coding-balanced",
        )
        .expect_err("no recursion");
        assert!(
            matches!(err, SelectionError::ProfileNotPinned { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn auto_rejects_a_profile_added_after_the_snapshot() {
        let balanced = pi("coding-balanced", "cheap", &[]);
        let premium = pi("coding-premium", "expensive", &[]);
        let later = codex("codex-default", "gpt-5.6-luna");
        let at_submission = catalog(&[balanced.clone(), premium.clone()]);
        let constraint =
            auto_constraint(&at_submission, CodingConstraintSource::UserUi).expect("auto");
        let after = catalog(&[balanced, premium, later]);

        let ok = resolve_proposed_profile(
            &constraint,
            Some("coding-premium"),
            &after,
            "coding-balanced",
        )
        .expect("pinned");
        assert_eq!(ok.engine, CodingEngineKind::Pi);

        let err = resolve_proposed_profile(
            &constraint,
            Some("codex-default"),
            &after,
            "coding-balanced",
        )
        .expect_err("widened");
        assert!(
            matches!(err, SelectionError::ProfileNotPinned { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn auto_omitted_uses_the_configured_default_when_it_was_pinned() {
        let balanced = pi("coding-balanced", "cheap", &[]);
        let premium = pi("coding-premium", "expensive", &[]);
        let catalog = catalog(&[balanced, premium]);
        let constraint =
            auto_constraint(&catalog, CodingConstraintSource::ConfiguredDefault).expect("auto");
        let resolved = resolve_proposed_profile(&constraint, None, &catalog, "coding-balanced")
            .expect("default");
        assert_eq!(resolved.profile_id, "coding-balanced");
        assert_eq!(resolved.selection_source, CodingSelectionSource::Default);
    }

    #[test]
    fn stale_or_ineligible_pins_fail_closed() {
        let floor = pi("coding-balanced", "cheap", &[]);
        let catalog = catalog(&[floor.clone()]);
        let constraint =
            fixed_constraint_from_profile(&floor, &catalog, CodingConstraintSource::UserUi)
                .expect("fixed");

        let mut drifted = floor.clone();
        match &mut drifted {
            CodingProfileDefinition::Pi { llm_profile, .. } => {
                *llm_profile = "other".into();
            },
            _ => unreachable!(),
        }
        let drifted_catalog = vec![eligible(drifted)];
        let changed =
            resolve_proposed_profile(&constraint, None, &drifted_catalog, "coding-balanced")
                .expect_err("changed");
        assert!(
            matches!(changed, SelectionError::ProfileChanged { .. }),
            "{changed:?}"
        );

        let ineligible = vec![ProfileCatalogEntry {
            definition: floor,
            eligible: false,
        }];
        let blocked = resolve_proposed_profile(&constraint, None, &ineligible, "coding-balanced")
            .expect_err("ineligible");
        assert!(
            matches!(blocked, SelectionError::ProfileIneligible { .. }),
            "{blocked:?}"
        );
    }

    #[test]
    fn legacy_hydration_is_fixed_pi_and_rejects_a_codex_floor() {
        let floor = pi("coding-balanced", "cheap", &[]);
        let constraint = hydrate_legacy_fixed_pi(&floor).expect("legacy");
        assert_eq!(constraint.source, CodingConstraintSource::MigratedLegacy);
        assert_eq!(constraint.fixed_engine(), Some(CodingEngineKind::Pi));
        assert!(hydrate_legacy_fixed_pi(&codex("codex-default", "gpt-5.6-luna")).is_err());
    }

    #[test]
    fn ledger_reuses_the_same_tool_action_and_refuses_a_conflicting_digest() {
        let floor = pi("coding-balanced", "cheap", &[]);
        let catalog = catalog(&[floor.clone()]);
        let constraint = hydrate_legacy_fixed_pi(&floor).expect("legacy");
        let selection =
            resolve_proposed_profile(&constraint, None, &catalog, "coding-balanced").expect("sel");
        let mut ledger = CodingExecutionLedger::default();
        let first = prepared_invocation("inv-1", &constraint, selection.clone(), "input-a");
        ledger
            .append_or_reuse(first.clone(), 2)
            .expect("first append");
        let reused = ledger
            .append_or_reuse(first, 2)
            .expect("replay")
            .invocation_id
            .clone();
        assert_eq!(reused, "inv-1");
        assert_eq!(ledger.invocations.len(), 1);

        let conflict = prepared_invocation("inv-1", &constraint, selection, "input-b");
        let err = ledger.append_or_reuse(conflict, 2).expect_err("conflict");
        assert!(
            matches!(err, SelectionError::LedgerIntegrityConflict { .. }),
            "{err:?}"
        );
        assert_eq!(ledger.invocations.len(), 1);
    }

    #[test]
    fn ledger_retains_multiple_invocations_and_never_evicts_at_the_cap() {
        let floor = pi("coding-balanced", "cheap", &[]);
        let catalog = catalog(&[floor.clone()]);
        let constraint = hydrate_legacy_fixed_pi(&floor).expect("legacy");
        let selection =
            resolve_proposed_profile(&constraint, None, &catalog, "coding-balanced").expect("sel");
        let mut ledger = CodingExecutionLedger::default();
        ledger
            .append_or_reuse(
                prepared_invocation("inv-1", &constraint, selection.clone(), "a"),
                2,
            )
            .expect("one");
        ledger
            .append_or_reuse(
                prepared_invocation("inv-2", &constraint, selection.clone(), "b"),
                2,
            )
            .expect("two");
        let err = ledger
            .append_or_reuse(prepared_invocation("inv-3", &constraint, selection, "c"), 2)
            .expect_err("cap");
        assert!(matches!(err, SelectionError::LedgerCapReached { cap: 2 }));
        assert_eq!(ledger.invocations.len(), 2);
        assert_eq!(ledger.invocations[0].invocation_id, "inv-1");
        assert_eq!(ledger.invocations[1].invocation_id, "inv-2");
    }

    #[test]
    fn prepared_is_the_only_automatic_retry_and_advances_before_the_write() {
        let prepared = CodingDispatchState::Prepared {
            canonical_input_digest: "in".into(),
            base_turn_id: Some("turn-1".into()),
        };
        assert!(prepared.automatic_retry_allowed());
        let started = prepared
            .clone()
            .mark_request_may_have_started()
            .expect("advance");
        assert!(!started.automatic_retry_allowed());
        assert!(matches!(
            started,
            CodingDispatchState::RequestMayHaveStarted { .. }
        ));
        assert!(started.clone().mark_request_may_have_started().is_err());
        assert!(!CodingDispatchState::DispatchUnknown.automatic_retry_allowed());
        let accepted = started.mark_accepted("turn-9").expect("accepted");
        assert!(matches!(
            &accepted,
            CodingDispatchState::Accepted { provider_turn_id } if provider_turn_id == "turn-9"
        ));
        assert!(!accepted.automatic_retry_allowed());
    }

    #[test]
    fn unknown_cost_is_not_serialized_as_zero() {
        let usage = CodingEngineUsage {
            input_tokens: 10,
            output_tokens: 4,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
            billing_basis: CodingBillingBasis::ChatgptEntitlement,
        };
        let json = serde_json::to_value(&usage).expect("json");
        assert_eq!(json["billing_basis"], "chatgpt_entitlement");
        assert!(json.get("cost_usd").is_none());
    }

    #[test]
    fn stable_and_prohibited_method_lists_do_not_overlap() {
        for method in CODEX_APP_SERVER_STABLE_METHODS {
            assert!(
                !CODEX_APP_SERVER_PROHIBITED_METHODS.contains(method),
                "{method} is both stable and prohibited"
            );
        }
        for method in CODEX_APP_SERVER_EXPERIMENTAL_OFF_IN_V1 {
            assert!(
                !CODEX_APP_SERVER_STABLE_METHODS.contains(method),
                "{method} is experimental but listed as stable"
            );
        }
        assert!(CODEX_APP_SERVER_PROHIBITED_METHODS.contains(&"thread/shellCommand"));
        assert!(CODEX_APP_SERVER_STABLE_METHODS.contains(&"config/read"));
        assert!(!CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED);
    }

    #[test]
    fn omitted_choice_pins_the_configured_default_as_fixed_pi() {
        let balanced = pi("coding-balanced", "cheap", &["coding-premium"]);
        let premium = pi("coding-premium", "expensive", &[]);
        let catalog = catalog(&[balanced, premium]);
        let constraint = constraint_from_requested_choice(
            None,
            &catalog,
            "coding-balanced",
            CodingConstraintSource::ConfiguredDefault,
        )
        .expect("omitted");
        assert_eq!(constraint.fixed_engine(), Some(CodingEngineKind::Pi));
        assert_eq!(constraint.source, CodingConstraintSource::ConfiguredDefault);
        match &constraint.mode {
            CodingConstraintMode::Fixed {
                floor_profile_id,
                allowed_escalations,
                ..
            } => {
                assert_eq!(floor_profile_id, "coding-balanced");
                assert_eq!(allowed_escalations.len(), 1);
                assert_eq!(allowed_escalations[0].profile_id, "coding-premium");
            },
            other => panic!("expected fixed, got {other:?}"),
        }
    }

    #[test]
    fn a_named_choice_unknown_to_the_catalog_fails_closed() {
        let catalog = catalog(&[pi("coding-balanced", "cheap", &[])]);
        let err = constraint_from_requested_choice(
            Some(RequestedCodingChoice::Profile {
                profile_id: "codex-default",
            }),
            &catalog,
            "coding-balanced",
            CodingConstraintSource::UserUi,
        )
        .expect_err("unknown");
        assert!(
            matches!(&err, SelectionError::UnknownProfile { profile_id } if profile_id == "codex-default"),
            "{err:?}"
        );
    }

    #[test]
    fn auto_choice_snapshots_eligible_profiles_and_rejects_later_additions() {
        let balanced = pi("coding-balanced", "cheap", &[]);
        let premium = pi("coding-premium", "expensive", &[]);
        let at_submit = catalog(&[balanced.clone(), premium.clone()]);
        let constraint = constraint_from_requested_choice(
            Some(RequestedCodingChoice::Auto),
            &at_submit,
            "coding-balanced",
            CodingConstraintSource::UserUi,
        )
        .expect("auto");
        let later = catalog(&[balanced, premium, codex("codex-default", "gpt-5.6-luna")]);
        let err = resolve_proposed_profile(
            &constraint,
            Some("codex-default"),
            &later,
            "coding-balanced",
        )
        .expect_err("widened");
        assert!(
            matches!(err, SelectionError::ProfileNotPinned { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn decide_leaves_non_vibedev_callers_alone() {
        let catalog = catalog(&[pi("coding-balanced", "cheap", &[])]);
        let decision = decide_vibedev_dispatch_profile(
            false,
            None,
            Some("codex-default"),
            &catalog,
            "coding-balanced",
        )
        .expect("not vibedev");
        assert!(matches!(decision, VibeDevDispatchDecision::NotVibeDev));
    }

    #[test]
    fn decide_hydrates_a_missing_pin_as_fixed_default_and_rejects_codex() {
        let catalog = catalog(&[
            pi("coding-balanced", "cheap", &[]),
            codex("codex-default", "gpt-5.6-luna"),
        ]);
        let err = decide_vibedev_dispatch_profile(
            true,
            None,
            Some("codex-default"),
            &catalog,
            "coding-balanced",
        )
        .expect_err("cross-engine");
        assert!(
            matches!(err, SelectionError::CrossEngineProposal { .. }),
            "{err:?}"
        );
        let ok = decide_vibedev_dispatch_profile(true, None, None, &catalog, "coding-balanced")
            .expect("hydrated");
        match ok {
            VibeDevDispatchDecision::Selected(selection) => {
                assert_eq!(selection.profile_id, "coding-balanced");
                assert_eq!(selection.engine, CodingEngineKind::Pi);
            },
            other => panic!("expected selected, got {other:?}"),
        }
    }

    #[test]
    fn decide_enforces_a_stored_fixed_pin() {
        let floor = pi("coding-balanced", "cheap", &["coding-premium"]);
        let premium = pi("coding-premium", "expensive", &[]);
        let catalog = catalog(&[floor.clone(), premium]);
        let constraint = constraint_from_requested_choice(
            Some(RequestedCodingChoice::Profile {
                profile_id: "coding-balanced",
            }),
            &catalog,
            "coding-balanced",
            CodingConstraintSource::UserUi,
        )
        .expect("pin");
        let escalated = decide_vibedev_dispatch_profile(
            true,
            Some(constraint.clone()),
            Some("coding-premium"),
            &catalog,
            "coding-balanced",
        )
        .expect("one-hop");
        match escalated {
            VibeDevDispatchDecision::Selected(selection) => {
                assert_eq!(selection.profile_id, "coding-premium");
            },
            other => panic!("expected selected, got {other:?}"),
        }
        let rejected = decide_vibedev_dispatch_profile(
            true,
            Some(constraint),
            Some("coding-ultra"),
            &catalog,
            "coding-balanced",
        )
        .expect_err("not pinned");
        assert!(
            matches!(
                rejected,
                SelectionError::UnknownProfile { .. } | SelectionError::ProfileNotPinned { .. }
            ),
            "{rejected:?}"
        );
    }

    #[test]
    fn apply_legacy_cockpit_escalations_pins_balanced_to_premium() {
        let mut entries = catalog(&[
            pi("coding-balanced", "cheap", &[]),
            pi("coding-premium", "expensive", &[]),
        ]);
        apply_legacy_cockpit_escalations(&mut entries);
        let balanced = entries
            .iter()
            .find(|entry| entry.definition.id() == "coding-balanced")
            .expect("floor");
        assert_eq!(balanced.definition.escalates_to(), ["coding-premium"]);
    }

    #[test]
    fn auto_snapshot_order_does_not_change_the_digest() {
        let balanced = eligible(pi("coding-balanced", "cheap", &[]));
        let premium = eligible(pi("coding-premium", "expensive", &[]));
        let left = auto_constraint(
            &[balanced.clone(), premium.clone()],
            CodingConstraintSource::UserUi,
        )
        .expect("left");
        let right =
            auto_constraint(&[premium, balanced], CodingConstraintSource::UserUi).expect("right");
        assert_eq!(left.digest, right.digest);
    }

    #[test]
    fn pi_continuation_ref_is_engine_bound_and_path_stable() {
        let first = CodingContinuationRef::for_pi_session(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/project"),
            Some("task-root"),
        );
        let same = CodingContinuationRef::for_pi_session(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/project"),
            Some("task-root"),
        );
        let other_project = CodingContinuationRef::for_pi_session(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/other"),
            Some("task-root"),
        );
        assert_eq!(first.engine, CodingEngineKind::Pi);
        assert_eq!(first.native_session_id, "sess-1");
        assert_eq!(first.root_task_id, "task-root");
        assert_eq!(first, same);
        assert_ne!(
            first.project_binding_digest,
            other_project.project_binding_digest
        );
        let json = serde_json::to_string(&first).expect("json");
        assert!(!json.contains("last_completed_turn_id"), "{json}");
    }

    #[test]
    fn codex_continuation_ref_does_not_share_a_pi_session() {
        let pi = CodingContinuationRef::for_pi_session(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/project"),
            Some("task-root"),
        );
        let codex = CodingContinuationRef::for_codex_thread(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/project"),
            Some("task-root"),
        );
        assert_eq!(codex.engine, CodingEngineKind::CodexAppServer);
        assert_eq!(codex.native_session_id, "sess-1");
        assert_ne!(pi, codex);
        let grok = CodingContinuationRef::for_grok_session(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/project"),
            Some("task-root"),
        );
        assert_eq!(grok.engine, CodingEngineKind::GrokAcp);
        assert_ne!(pi, grok);
        assert_ne!(codex, grok);
        let claude = CodingContinuationRef::for_claude_session(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/project"),
            Some("task-root"),
        );
        assert_eq!(claude.engine, CodingEngineKind::ClaudeCode);
        assert_ne!(grok, claude);
        let agy = CodingContinuationRef::for_agy_session(
            "sess-1",
            Path::new("/tmp/scope"),
            Path::new("/tmp/project"),
            Some("task-root"),
        );
        assert_eq!(agy.engine, CodingEngineKind::AgyCli);
        assert_ne!(claude, agy);
    }
}
