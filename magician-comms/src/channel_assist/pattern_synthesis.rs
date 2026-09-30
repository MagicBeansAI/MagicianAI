//! Passive pattern synthesis — PASSIVE (no user action) consolidation of the
//! distilled channel corpus into recurring topics + their timing, written to the
//! `user.channel_patterns` memory tier. This is the "just by looking at messages,
//! figure things out" lane (e.g. a topic that recurs every May–June), distinct
//! from the `feedback_bridge` (which keys on EXPLICIT Do-it/Ack/Dismiss).
//!
//! Generic by construction: the LLM is asked to surface WHATEVER recurring
//! topics + seasonality exist — no per-topic hardcoding. Privacy: it reads only
//! metadata + the LOCALLY-derived summaries (never a raw body); the synthesis
//! op is an ordinary router dispatch bound in `operation_mapping` (local OR
//! remote) — UNBOUND ⇒ the pass is idle.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use magician::magician_v2::agents::memory::AgentMemoryResolver;
use magician::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use magician::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::chat::service::merge_user_memory_tier_fields;
use magician::magician_v2::process_storage;
use magician::magician_v2::prompts::{
    names as prompt_names, rendered_prompt, versions as prompt_versions,
};
use magician::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter,
};
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::store::{MailAssistStore, PatternCorpusRow};

const LOG_TARGET: &str = "channel_assist::pattern_synthesis";
/// Operation key — bind it in `operation_mapping` to run (local OR remote);
/// leave it unbound to keep the pass idle.
pub const CHANNEL_PATTERN_SYNTHESIS_OPERATION: &str = "channel_pattern_synthesis";
/// The tier this bridge writes — the shared tier-name contract (plan 3.1
/// prerequisite (b)), not a local string.
const TIER: &str = magician::magician_v2::evidence::tier_contracts::CHANNEL_PATTERNS_TIER;
const DEFAULT_INTERVAL_SECS: u64 = 86_400; // daily — this is slow, seasonal signal
const DEFAULT_STARTUP_DELAY_SECS: u64 = 600;
/// Look-back window + cap for the corpus.
const WINDOW_DAYS: i64 = 210; // ~7 months, enough to see a season repeat
const CORPUS_CAP: usize = 2000;
/// Below this the corpus is too thin to synthesise anything meaningful.
const MIN_CORPUS: usize = 40;
/// Bound the per-month detail so the prompt stays compact.
const MAX_SENDERS_PER_MONTH: usize = 12;
const MAX_SUBJECTS_PER_MONTH: usize = 8;
/// Cap how many patterns we persist per pass.
const MAX_PATTERNS: usize = 24;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn month_of(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The sender's domain (lower-cased) — the useful recurrence key.
fn domain_of(address: &str) -> String {
    address
        .rsplit('@')
        .next()
        .unwrap_or(address)
        .trim()
        .to_ascii_lowercase()
}

/// Pure: fold the corpus into a compact month → {top sender-domains, sample
/// subjects} digest string — the temporal signal the LLM synthesises from.
pub fn build_digest(corpus: &[PatternCorpusRow]) -> String {
    struct Month {
        total: usize,
        domains: BTreeMap<String, usize>,
        subjects: Vec<String>,
    }
    let mut months: BTreeMap<String, Month> = BTreeMap::new();
    for row in corpus {
        let m = months
            .entry(month_of(row.internal_date))
            .or_insert_with(|| Month {
                total: 0,
                domains: BTreeMap::new(),
                subjects: Vec::new(),
            });
        m.total += 1;
        if let Some(addr) = row
            .from_address
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            *m.domains.entry(domain_of(addr)).or_default() += 1;
        }
        if let Some(subj) = row
            .subject
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if m.subjects.len() < MAX_SUBJECTS_PER_MONTH && !m.subjects.iter().any(|s| s == subj) {
                m.subjects.push(subj.to_string());
            }
        }
    }
    let mut lines = Vec::new();
    for (month, data) in &months {
        let mut domains: Vec<(&String, &usize)> = data.domains.iter().collect();
        domains.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = domains
            .into_iter()
            .take(MAX_SENDERS_PER_MONTH)
            .map(|(d, c)| format!("{d}×{c}"))
            .collect();
        lines.push(format!(
            "{month} ({} msgs): senders [{}]; subjects [{}]",
            data.total,
            top.join(", "),
            data.subjects.join(" | ")
        ));
    }
    lines.join("\n")
}

#[derive(Debug, Deserialize)]
struct SynthesisReply {
    #[serde(default)]
    patterns: Vec<SynthPattern>,
}

#[derive(Debug, Deserialize)]
struct SynthPattern {
    topic: String,
    #[serde(default)]
    timing: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    examples: Vec<String>,
}

/// Strip a ``` fence if the model added one, then parse the first JSON object.
fn parse_reply(raw: &str) -> Option<SynthesisReply> {
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    serde_json::from_str(&trimmed[start..=end]).ok()
}

/// A URL/key-safe slug for the tier field key.
fn slug(topic: &str) -> String {
    let s: String = topic
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "topic".to_string()
    } else {
        s.chars().take(48).collect()
    }
}

/// One synthesis pass for a scope: corpus → digest → LLM → `user.channel_patterns`.
/// Idle (Ok(0)) when the op is unbound or the corpus is too thin.
pub async fn run_synthesis_pass(
    store: &MailAssistStore,
    router: Option<&Arc<OperationLlmRouter>>,
    event_broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    principal: &str,
    workspace: &str,
) -> Result<usize> {
    let Some(router) = router else {
        return Ok(0);
    };
    if router
        .explicit_binding_for_operation(CHANNEL_PATTERN_SYNTHESIS_OPERATION)
        .is_none()
    {
        return Ok(0); // unbound ⇒ idle (no default-remote surprise)
    }

    let since = now_ms() - WINDOW_DAYS * 86_400_000;
    let corpus = store
        .list_pattern_corpus(principal, workspace, since, CORPUS_CAP)
        .await?;
    if corpus.len() < MIN_CORPUS {
        return Ok(0);
    }
    let digest = build_digest(&corpus);

    let mut vars = std::collections::HashMap::new();
    vars.insert("digest".to_string(), digest);
    let system = rendered_prompt(
        prompt_names::CHANNEL_PATTERN_SYNTHESIS_SYSTEM,
        prompt_versions::CHANNEL_PATTERN_SYNTHESIS,
        std::collections::HashMap::new(),
    )
    .await
    .context("rendering managed channel pattern synthesis system prompt")?;
    let user = rendered_prompt(
        prompt_names::CHANNEL_PATTERN_SYNTHESIS_USER,
        prompt_versions::CHANNEL_PATTERN_SYNTHESIS,
        vars,
    )
    .await
    .context("rendering managed channel pattern synthesis user prompt")?;

    let operation = LLMOperation::Other(CHANNEL_PATTERN_SYNTHESIS_OPERATION.to_string());
    let llm_started = std::time::Instant::now();
    let scoped_router =
        router.with_scope_context(Some(magicllm::LlmScope::new(principal, workspace)));
    let response = scoped_router
        .generate_for_operation_with_system(&operation, Some(&system), &user)
        .await
        .context("channel pattern synthesis LLM call failed")?;
    let parsed = parse_reply(&response.content);
    if let Some(broadcaster) = event_broadcaster {
        let telemetry = OperationLlmTelemetryContext::new(
            Arc::clone(broadcaster),
            principal,
            workspace,
            "channel_assist",
        );
        let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        match parsed.as_ref() {
            Some(_) => telemetry.emit_validated_success(
                CHANNEL_PATTERN_SYNTHESIS_OPERATION,
                &response,
                latency_ms,
                OperationLlmCallAttribution::default(),
                "channel_pattern_json",
            ),
            None => telemetry.emit_validation_failure(
                CHANNEL_PATTERN_SYNTHESIS_OPERATION,
                &response,
                latency_ms,
                OperationLlmCallAttribution::default(),
                "channel_pattern_json",
                "pattern synthesis reply was not parseable JSON",
            ),
        }
    }
    let Some(reply) = parsed else {
        warn!(target: LOG_TARGET, "pattern synthesis reply was not parseable JSON");
        return Ok(0);
    };

    let resolver = AgentMemoryResolver::with_workspace_layout(process_storage::workspace());
    let synthesized_at = now_ms();
    let mut written = 0usize;
    for pattern in reply.patterns.into_iter().take(MAX_PATTERNS) {
        if pattern.topic.trim().is_empty() {
            continue;
        }
        let mut obj = Map::new();
        obj.insert("topic".into(), json!(pattern.topic));
        obj.insert("timing".into(), json!(pattern.timing.unwrap_or_default()));
        obj.insert("note".into(), json!(pattern.note.unwrap_or_default()));
        obj.insert("examples".into(), json!(pattern.examples));
        obj.insert("source".into(), json!("passive_channel_synthesis"));
        obj.insert("synthesized_at".into(), json!(synthesized_at));
        let mut fields = Map::new();
        fields.insert(slug(&pattern.topic), Value::Object(obj));
        let outcome =
            merge_user_memory_tier_fields(&resolver, principal, workspace, TIER, &fields).await;
        if outcome.get("status").and_then(Value::as_str) == Some("error") {
            let reason = outcome.get("reason").and_then(Value::as_str).unwrap_or("");
            warn!(target: LOG_TARGET, tier = TIER, reason, "pattern tier write failed");
        } else {
            written += 1;
        }
    }
    info!(target: LOG_TARGET, patterns = written, corpus = corpus.len(), "pattern synthesis pass complete");
    Ok(written)
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

pub struct ChannelPatternSynthesisWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl ChannelPatternSynthesisWorker {
    /// `CHANNEL_PATTERN_SYNTHESIS_ENABLED` (default on) — but the pass is ALSO idle
    /// unless `channel_pattern_synthesis` is bound in `operation_mapping`, so the
    /// worker is a no-op until an operator opts the op in.
    pub fn enabled_from_env() -> bool {
        std::env::var("CHANNEL_PATTERN_SYNTHESIS_ENABLED")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
            })
            .unwrap_or(true)
    }

    pub fn spawn(
        store: MailAssistStore,
        router: Option<Arc<OperationLlmRouter>>,
        event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    ) -> Self {
        let interval = Duration::from_secs(env_secs(
            "CHANNEL_PATTERN_SYNTHESIS_INTERVAL_SECS",
            DEFAULT_INTERVAL_SECS,
        ));
        let startup = Duration::from_secs(env_secs(
            "CHANNEL_PATTERN_SYNTHESIS_STARTUP_DELAY_SECS",
            DEFAULT_STARTUP_DELAY_SECS,
        ));
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel_for_task)
                .await
            {
                return;
            }
            tokio::select! {
                _ = cancel_for_task.cancelled() => return,
                _ = tokio::time::sleep(startup) => {},
            }
            let mut tick = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = cancel_for_task.cancelled() => return,
                    _ = tick.tick() => {},
                }
                match run_synthesis_pass(
                    &store,
                    router.as_ref(),
                    event_broadcaster.as_ref(),
                    DEFAULT_SCOPE_PRINCIPAL,
                    DEFAULT_SCOPE_WORKSPACE,
                )
                .await
                {
                    Ok(n) if n > 0 => {
                        debug!(target: LOG_TARGET, patterns = n, "synthesis tick wrote patterns")
                    },
                    Ok(_) => {},
                    Err(error) => {
                        warn!(target: LOG_TARGET, error = %error, "synthesis tick failed")
                    },
                }
            }
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn row(ms: i64, addr: &str, subject: &str) -> PatternCorpusRow {
        PatternCorpusRow {
            internal_date: ms,
            subject: Some(subject.to_string()),
            from_address: Some(addr.to_string()),
            summary: Some("s".to_string()),
        }
    }

    #[test]
    fn digest_buckets_by_month_and_domain() {
        // Two months, repeated sender domain.
        let may = 1_778_000_000_000; // ~2026-05
        let jun = 1_780_600_000_000; // ~2026-06
        let corpus = vec![
            row(may, "renew@insco.com", "Policy renewal"),
            row(may, "renew@insco.com", "Policy renewal reminder"),
            row(jun, "billing@power.com", "Statement"),
        ];
        let digest = build_digest(&corpus);
        assert!(digest.contains("insco.com×2"));
        assert!(digest.contains("power.com×1"));
        assert!(digest.lines().count() >= 2);
    }

    #[test]
    fn slug_is_key_safe() {
        assert_eq!(slug("Car Insurance!"), "car_insurance");
        assert_eq!(slug("   "), "topic");
    }

    #[test]
    fn parse_reply_tolerates_code_fence() {
        let raw = "```json\n{\"patterns\":[{\"topic\":\"bills\",\"timing\":\"monthly\"}]}\n```";
        let reply = parse_reply(raw).expect("parses");
        assert_eq!(reply.patterns.len(), 1);
        assert_eq!(reply.patterns[0].topic, "bills");
    }
}
