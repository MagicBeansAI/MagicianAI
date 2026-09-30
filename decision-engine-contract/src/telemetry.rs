//! Content-free receipts for physical Decision Model attempts.
//! Optional buckets distinguish unavailable measurements from measured zero.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionCallStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionModelCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    /// One physical call may eventually serve multiple items; bill it once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_ids: Vec<String>,
    pub call_id: String,
    pub retry_group_id: String,
    pub attempt: u32,
    pub operation: String,
    pub adapter: String,
    /// Pricing route, independent of the model name. A custom endpoint must
    /// never inherit a hosted vendor's rates just by requesting its model.
    pub provider: String,
    pub requested_model: String,
    pub model: String,
    pub local: bool,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub latency_ms: u64,
    /// Admission wait, separate from physical inference latency; repeated on retries.
    #[serde(default)]
    pub queue_wait_ms: u64,
    pub status: DecisionCallStatus,
    pub error_class: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

impl DecisionModelCall {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .unwrap_or(0)
            .saturating_add(self.output_tokens.unwrap_or(0))
    }
}
