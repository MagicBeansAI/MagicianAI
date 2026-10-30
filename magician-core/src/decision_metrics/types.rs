use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Stats {
    pub counts: BTreeMap<String, i64>,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    // Logarithmic histogram: 1ms through >24h, bounded to 160 bins.
    pub model_latency: Vec<i64>,
    pub workflow_latency: Vec<i64>,
}
impl Stats {
    pub fn count(&mut self, key: &str, n: i64) {
        *self.counts.entry(key.into()).or_default() += n;
    }
    pub fn merge(&mut self, other: &Self, sign: i64) {
        for (k, v) in &other.counts {
            self.count(k, v * sign);
        }
        self.cost_usd += other.cost_usd * sign as f64;
        self.input_tokens += other.input_tokens * sign;
        self.output_tokens += other.output_tokens * sign;
        self.cache_read_tokens += other.cache_read_tokens * sign;
        self.cache_write_tokens += other.cache_write_tokens * sign;
        for (dst, src) in [
            (&mut self.model_latency, &other.model_latency),
            (&mut self.workflow_latency, &other.workflow_latency),
        ] {
            dst.resize(dst.len().max(src.len()), 0);
            for (i, n) in src.iter().enumerate() {
                dst[i] += n * sign;
            }
        }
    }
    pub fn latency(&mut self, ms: u64, workflow: bool) {
        let bucket = ((ms.max(1) as f64).ln() / 1.12_f64.ln()).floor().min(159.0) as usize;
        let histogram = if workflow {
            &mut self.workflow_latency
        } else {
            &mut self.model_latency
        };
        histogram.resize(histogram.len().max(bucket + 1), 0);
        histogram[bucket] += 1;
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DailyRow {
    pub day: i64,
    pub kind: String,
    pub cohort: String,
    pub operation: String,
    pub model: String,
    pub identity: Value,
    pub stats: Stats,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsQuery {
    pub days: Option<u32>,
    pub operation: Option<String>,
    pub model: Option<String>,
    pub cohort: Option<String>,
    pub before: Option<i64>,
    pub before_id: Option<String>,
}
impl MetricsQuery {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.days.unwrap_or(7), 1 | 7 | 30) {
            return Err("Choose Today, 7 or 30 days".into());
        }
        if [&self.operation, &self.model, &self.cohort, &self.before_id]
            .into_iter()
            .flatten()
            .any(|s| s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err("Invalid filter".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Dashboard {
    pub rows: Vec<DailyRow>,
    pub truncated: bool,
    pub collected_since_ms: Option<i64>,
    pub generated_at_ms: i64,
    pub last_comparison_ms: Option<i64>,
    pub capture_dropped: u64,
    pub write_errors: u64,
    pub read_ms: f64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    pub at_ms: i64,
    pub operation: String,
    pub primary: Option<Value>,
    pub reference: Option<Value>,
    pub reference_status: Option<String>,
    pub applications: BTreeMap<String, Value>,
    pub projection: Option<Value>,
    pub reviews: BTreeMap<String, BTreeMap<String, Value>>,
    pub review_revision: u64,
    /// Closed runner status for a decision that never reached the engine
    /// (`disabled`, `policy_unavailable`, `admission_or_coalesced`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_status: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewUpdate {
    pub comparison_id: String,
    pub revision: u64,
    pub labels: BTreeMap<String, BTreeMap<String, Value>>,
}
