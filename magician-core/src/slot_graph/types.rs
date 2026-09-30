use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Primary record describing a collected slot value and its provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotRecord {
    pub id: String,
    pub slot_type: SlotType,
    pub value: serde_json::Value,
    pub confidence: f64,
    #[serde(default)]
    pub provenance: Vec<ProvenanceRecord>,
    #[serde(default)]
    pub evidence_links: Vec<String>,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub updated_at: DateTime<Utc>,
}

/// Enumerates the known kinds of slots that can be collected by the agent.
///
/// ```
/// use magician_core::slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType};
/// use chrono::Utc;
///
/// let mut record = SlotRecord {
///     id: "customer_name".into(),
///    slot_type: SlotType::Entity,
///     value: serde_json::json!("Acme Corp"),
///     confidence: 0.8,
///     provenance: vec![ProvenanceRecord {
///         source: ProvenanceSource::UserReply,
///         timestamp: Utc::now(),
///     }],
///     evidence_links: vec![],
///     created_at: Utc::now(),
///     updated_at: Utc::now(),
/// };
/// record.touch();
/// assert_eq!(record.slot_type, SlotType::Entity);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SlotType {
    /// Named entities such as people, organizations, or products (`"Acme Corp"`).
    Entity,
    /// Temporal details like dates, times, or durations (`"tomorrow at 9am"`).
    Temporal,
    /// Spatial references including locations or directions (`"3rd floor"`).
    Spatial,
    /// Emotional tone or urgency expressed in the conversation (`"frustrated"`).
    Emotion,
    /// Requested actions or verbs (`"reset the password"`).
    Action,
    /// Modifiers and constraints that adjust requests (`"critical"`, `"within budget"`).
    Modifier,
    /// External resources the agent should reference (`"https://example.com/spec.pdf"`).
    Resource,
    /// Current status or availability signals (`"pending"`, `"completed"`).
    Status,
    /// Tool approach selection for ambiguous services that can be accessed multiple ways.
    /// Format: `"service_access"` with value like `"api"`, `"browser"`, or `"shell"`.
    /// Example: `tool_selection:github_access` with value `"api"` for GitHub API access.
    ToolSelection,
}

/// Captures audit trail information about who/what/when updated a slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    pub source: ProvenanceSource,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub timestamp: DateTime<Utc>,
}

/// Sources that can contribute to a slot record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSource {
    LlmPrimary,
    UserReply,
    ScreenshotInference,
    DeterministicCheck,
    MemoryLookup,
    /// Tool selection prerequisite from hierarchical outline phase
    OutlinePrerequisite,
}

impl SlotRecord {
    /// Helper to refresh the updated timestamp.
    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// Construct a [`SlotRecord`] from a provisional extraction result.
    pub fn from_provisional(workflow_id: &str, provisional: ProvisionalSlot) -> Self {
        let now = Utc::now();
        let confidence = provisional.confidence.clamp(0.0, 1.0);

        SlotRecord {
            id: format!("{}::{}", workflow_id, Uuid::new_v4()),
            slot_type: provisional.slot_type,
            value: provisional.value,
            confidence,
            provenance: vec![ProvenanceRecord {
                source: ProvenanceSource::LlmPrimary,
                timestamp: now,
            }],
            evidence_links: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Result of the extraction request prior to persistence in the slot graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProvisionalSlot {
    pub slot_type: SlotType,
    pub value: serde_json::Value,
    pub confidence: f64,
    pub rationale: String,
}

impl ProvisionalSlot {
    pub fn validate(mut self) -> Result<Self> {
        if !(0.0..=1.0).contains(&self.confidence) {
            self.confidence = self.confidence.clamp(0.0, 1.0);
        }
        if !self.value.is_object() {
            return Err(anyhow!("slot value must be a JSON object"));
        }
        if self.rationale.trim().is_empty() {
            return Err(anyhow!("slot rationale cannot be empty"));
        }
        Ok(self)
    }
}
