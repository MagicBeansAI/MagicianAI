//! Local pre-processing of large content blocks via Ollama.
//!
//! Caller pattern: include `SummarisableBlock` entries in
//! `LLMRequest.summarisable_blocks`, pointing at content positions in
//! `messages`. The worker, before invoking the provider:
//!
//! 1. For each block whose `raw.len()` exceeds `threshold_chars`, calls the
//!    configured Ollama profile DIRECTLY (not through the dispatch queue,
//!    which avoids a recursion deadlock).
//! 2. Replaces `messages[mi].content[ci]` with `ContentBlock::Text { summary }`.
//! 3. On failure (Ollama down, timeout, empty output), falls through with
//!    the raw text inlined as-is.
//! 4. Clears `request.summarisable_blocks` so providers never see it.
//!
//! Provider impls require no changes — they only see `ContentBlock::Text`
//! (or the original variant if it was something else like Image, which is
//! preserved).

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::ollama_keep_alive;
use crate::trace::{LlmCallRole, LlmParentRelation, LlmTraceContext, LlmWorkloadClass};
use crate::types::{ContentBlock, LLMRequest, RequestMetadata, SummarisationPurpose};

#[cfg(test)]
use std::sync::{Mutex, OnceLock};
#[cfg(test)]
use tokio::sync::{Mutex as AsyncMutex, Notify};

#[cfg(test)]
use crate::types::SummarisableBlock;

use super::config::LocalPrepConfig;
use super::types::{LocalPrepCallStat, LocalPrepStat, TokenSummary};

#[cfg(test)]
pub(crate) struct LocalPrepTestHook {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

#[cfg(test)]
impl LocalPrepTestHook {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        })
    }
}

#[cfg(test)]
pub(crate) struct LocalPrepTestHookGuard;

#[cfg(test)]
impl Drop for LocalPrepTestHookGuard {
    fn drop(&mut self) {
        *test_hook_slot().lock().unwrap() = None;
    }
}

#[cfg(test)]
fn test_hook_slot() -> &'static Mutex<Option<Arc<LocalPrepTestHook>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<LocalPrepTestHook>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn test_hook() -> Option<Arc<LocalPrepTestHook>> {
    test_hook_slot().lock().unwrap().clone()
}

#[cfg(test)]
pub(crate) fn install_local_prep_test_hook(hook: Arc<LocalPrepTestHook>) -> LocalPrepTestHookGuard {
    *test_hook_slot().lock().unwrap() = Some(hook);
    LocalPrepTestHookGuard
}

#[cfg(test)]
pub(crate) fn local_prep_test_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// True when this request would issue a real Ollama generate for local-prep.
/// Cheap inlining (disabled, disclosure-bound, or below threshold) stays on
/// the worker. The coordinator only parks work that would block on generation.
pub(crate) fn local_prep_needs_ollama(request: &LLMRequest, config: &LocalPrepConfig) -> bool {
    if !config.enabled || !config.yield_worker {
        return false;
    }
    if request.metadata.disclosure_guard().is_some() {
        return false;
    }
    request
        .summarisable_blocks
        .iter()
        .any(|block| block.raw.len() >= config.threshold_chars)
}

/// Run local-prep on the supplied request. Returns `Some(LocalPrepStat)`
/// when any blocks were processed; `None` when local-prep was disabled or
/// `request.summarisable_blocks` is empty.
pub async fn maybe_local_prep_direct(
    request: &mut LLMRequest,
    config: &LocalPrepConfig,
) -> Option<LocalPrepStat> {
    maybe_local_prep_direct_cancellable(request, config, None, None)
        .await
        .0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalPrepCancellation {
    Task,
    QueueShutdown,
}

/// Cancellation-aware worker entry point. Unlike cancelling the outer future,
/// this lets an already-started local provider invocation return its exact
/// terminal child-call telemetry before the parent queue job is tombstoned.
pub(crate) async fn maybe_local_prep_direct_cancellable(
    request: &mut LLMRequest,
    config: &LocalPrepConfig,
    task_cancel: Option<&CancellationToken>,
    shutdown_cancel: Option<&CancellationToken>,
) -> (Option<LocalPrepStat>, Option<LocalPrepCancellation>) {
    if request.summarisable_blocks.is_empty() {
        return (None, cancellation_state(task_cancel, shutdown_cancel));
    }
    // The final disclosure guard does not attest this separate direct Ollama
    // call. Preserve the raw block for the guarded final route rather than
    // sending protected bytes through an unrepresented hidden consumer.
    let disclosure_bound = request.metadata.disclosure_guard().is_some();
    let shared_blocks = std::mem::take(&mut request.summarisable_blocks);
    let blocks = Arc::try_unwrap(shared_blocks).unwrap_or_else(|shared| (*shared).clone());
    let start = Instant::now();
    let mut chars_in = 0u64;
    let mut chars_out = 0u64;
    let mut processed = 0u32;
    let mut fell_through = 0u32;
    let mut affected_messages: Vec<usize> = Vec::new();
    let parent_trace = request.metadata.trace_context.clone();
    let mut calls = Vec::new();
    let mut cancellation = cancellation_state(task_cancel, shutdown_cancel);

    for block in blocks {
        if cancellation.is_some() {
            break;
        }
        chars_in += block.raw.len() as u64;
        let summary =
            if config.enabled && !disclosure_bound && block.raw.len() >= config.threshold_chars {
                let max_chars = block.max_chars.unwrap_or(config.max_chars_out);
                let trace_context = parent_trace.as_ref().map_or_else(
                    || {
                        let mut context = LlmTraceContext::legacy(
                            request.metadata.trace_id.as_deref(),
                            LlmWorkloadClass::System,
                        );
                        context.call_role = LlmCallRole::Summarizer;
                        context
                    },
                    |parent| parent.child(LlmParentRelation::Summarizes, LlmCallRole::Summarizer),
                );
                let (result, call, call_cancellation) = summarise_chunk(
                    &config.base_url,
                    &config.model,
                    block.purpose,
                    &block.raw,
                    max_chars,
                    Duration::from_secs(config.timeout_secs),
                    config.keep_alive.as_deref(),
                    config.context_tokens,
                    trace_context,
                    task_cancel,
                    shutdown_cancel,
                )
                .await;
                calls.push(call);
                cancellation = call_cancellation;
                match result {
                    Ok(summary) => {
                        chars_out += summary.len() as u64;
                        processed += 1;
                        summary
                    },
                    Err(err) => {
                        warn!(
                            purpose = block.purpose.as_str(),
                            chars = block.raw.len(),
                            error = %err,
                            "local prep failed; falling through with raw content"
                        );
                        fell_through += 1;
                        block.raw.clone()
                    },
                }
            } else {
                // Below threshold or disabled — inline raw.
                fell_through += 1;
                block.raw.clone()
            };

        replace_in_messages(request, block.message_index, block.content_index, summary);
        affected_messages.push(block.message_index);
        if cancellation.is_some() {
            break;
        }
    }

    // Producers that carry a summarisable body split one user prompt into
    // sibling `[before, placeholder, after]` Text blocks (the placeholder is
    // what we just replaced above). Re-coalesce consecutive Text blocks in each
    // touched message back into a single contiguous Text block. Without this,
    // providers that join a message's text parts with a separator (OpenAI Chat
    // / Ollama join with '\n') would inject delimiters at the carve boundaries,
    // so the wire text would differ from the original inline prompt on the
    // fall-through path. Coalescing restores exact equivalence (and keeps the
    // summary path a clean single block) for every provider.
    affected_messages.sort_unstable();
    affected_messages.dedup();
    for message_index in affected_messages {
        if let Some(message) = request.messages_mut().get_mut(message_index) {
            coalesce_consecutive_text(&mut message.content);
        }
    }

    if processed == 0 && fell_through == 0 {
        return (None, cancellation);
    }
    debug!(
        processed,
        fell_through, chars_in, chars_out, "local-prep complete"
    );
    (
        Some(LocalPrepStat {
            blocks_processed: processed,
            chars_in,
            chars_out,
            model: config.model.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
            calls,
        }),
        cancellation,
    )
}

/// Summarise a standalone string via local-prep (Ollama), for producers that
/// compress large bodies at WRITE TIME rather than through request
/// `summarisable_blocks` (e.g. agentic tool/step outputs that are stored once
/// and re-sent every turn — summarising per-request would re-run Ollama each
/// turn).
///
/// Returns `Some(summary)` only when local-prep is enabled, `raw` meets the
/// configured `threshold_chars`, and Ollama returns a non-empty result.
/// Returns `None` otherwise (disabled, below threshold, or Ollama
/// down/timeout/empty) so the caller keeps its original/truncated text. Never
/// panics; all Ollama failures map to `None`.
pub async fn summarise_text(
    config: &LocalPrepConfig,
    purpose: SummarisationPurpose,
    raw: &str,
) -> Option<String> {
    let mut trace_context = LlmTraceContext::legacy(None, LlmWorkloadClass::System);
    trace_context.call_role = LlmCallRole::Summarizer;
    summarise_text_traced(config, purpose, raw, trace_context)
        .await
        .0
}

/// Correlation-preserving variant of [`summarise_text`]. Repository-owned
/// runtime callers use this entry point so the otherwise-direct Ollama call is
/// represented as an independent supporting logical call. The compatibility
/// wrapper remains available to downstream `magicllm` consumers.
pub async fn summarise_text_traced(
    config: &LocalPrepConfig,
    purpose: SummarisationPurpose,
    raw: &str,
    trace_context: LlmTraceContext,
) -> (Option<String>, Option<LocalPrepCallStat>) {
    if !config.enabled || raw.len() < config.threshold_chars {
        return (None, None);
    }
    let (result, call, _) = summarise_chunk(
        &config.base_url,
        &config.model,
        purpose,
        raw,
        config.max_chars_out,
        Duration::from_secs(config.timeout_secs),
        config.keep_alive.as_deref(),
        config.context_tokens,
        trace_context,
        None,
        None,
    )
    .await;
    let summary = match result {
        Ok(summary) if !summary.is_empty() => Some(summary),
        Ok(_) => None,
        Err(err) => {
            warn!(
                purpose = purpose.as_str(),
                chars = raw.len(),
                error = %err,
                "local-prep summarise_text_traced failed; caller keeps original text"
            );
            None
        },
    };
    (summary, Some(call))
}

fn replace_in_messages(
    request: &mut LLMRequest,
    message_index: usize,
    content_index: usize,
    text: String,
) {
    if let Some(message) = request.messages_mut().get_mut(message_index) {
        if let Some(block) = message.content.get_mut(content_index) {
            *block = ContentBlock::Text { text };
            return;
        }
        // Position out of range — append the text as a new block so the
        // content isn't silently lost.
        message.content.push(ContentBlock::Text { text });
        return;
    }
    // Message out of range — drop silently with a warn. Callers should
    // ensure indices are valid; this is a defensive no-op rather than a panic.
    warn!(
        message_index,
        content_index, "local-prep: message index out of range; raw content dropped"
    );
}

/// Merge runs of consecutive `ContentBlock::Text` in `content` into single
/// blocks (concatenated with no separator). Non-text blocks (Image, ToolCall,
/// ToolResult, Json) are left untouched and act as barriers, so mixed content
/// keeps its structure. Used to undo the summarisable carve's sibling-block
/// split after the placeholder has been filled, restoring a single contiguous
/// text block so provider serialisation matches the original inline prompt.
fn coalesce_consecutive_text(content: &mut Vec<ContentBlock>) {
    if content.len() < 2 {
        return;
    }
    let mut merged: Vec<ContentBlock> = Vec::with_capacity(content.len());
    for block in content.drain(..) {
        if let ContentBlock::Text { text } = &block {
            if let Some(ContentBlock::Text { text: prev }) = merged.last_mut() {
                prev.push_str(text);
                continue;
            }
        }
        merged.push(block);
    }
    *content = merged;
}

async fn summarise_chunk(
    base_url: &str,
    model: &str,
    purpose: SummarisationPurpose,
    raw: &str,
    max_chars_out: usize,
    timeout: Duration,
    keep_alive: Option<&str>,
    context_tokens: u32,
    trace_context: LlmTraceContext,
    task_cancel: Option<&CancellationToken>,
    shutdown_cancel: Option<&CancellationToken>,
) -> (
    Result<String, String>,
    LocalPrepCallStat,
    Option<LocalPrepCancellation>,
) {
    let prompt = build_prompt(purpose, raw, max_chars_out);

    #[cfg(test)]
    if let Some(hook) = test_hook() {
        hook.entered.notify_waiters();
        let cancellation = tokio::select! {
            biased;
            _ = wait_for_cancellation(task_cancel) => Some(LocalPrepCancellation::Task),
            _ = wait_for_cancellation(shutdown_cancel) => {
                Some(LocalPrepCancellation::QueueShutdown)
            },
            _ = hook.release.notified() => None,
        };
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let call = LocalPrepCallStat {
            provider_attempt_id: trace_context.provider_attempt_id(1),
            trace_context,
            provider_attempt_count: 1,
            provider: "ollama".to_string(),
            model: model.to_string(),
            purpose: purpose.as_str().to_string(),
            started_at_ms,
            completed_at_ms: chrono::Utc::now().timestamp_millis(),
            latency_ms: 0,
            success: cancellation.is_none(),
            error_class: cancellation.map(|_| "cancelled".to_string()),
            tokens: None,
        };
        let result = if cancellation.is_some() {
            Err("local-prep test hook cancelled".to_string())
        } else {
            Ok("hook-summary".to_string())
        };
        return (result, call, cancellation);
    }

    // Route through OllamaProvider so local-prep shares the single ollama
    // chokepoint: the provider sets `think` (false here, since local-prep is a
    // summarisation utility and never a reasoning task — Ollama reasoning models
    // like Qwen3.x / gemma-4 `*-it` / `*-a4b` default to thinking-ON, which
    // degenerates into repetition or diverts the answer into a `thinking`
    // field), applies generation options, normalizes `keep_alive`, and maps
    // `eval_count`/`prompt_eval_count`→`TokenUsage`. We call it DIRECTLY (never
    // through the dispatch queue): `maybe_local_prep_direct` runs inside a
    // dispatch worker, so re-queuing would recurse/deadlock.
    //
    // `reasoning: None` → the provider sends `think: false`. `extra.prompt`
    // (with empty `messages`) makes the provider use this raw prompt verbatim,
    // bypassing role-tag formatting. `extra.options.num_ctx` is merged in, and
    // `max_output_tokens` becomes `options.num_predict` = `(max_chars_out/3).max(64)`.
    // Resolve keep_alive up-front (env → configured default → "10m") exactly as
    // the old `request_keep_alive(keep_alive)` did, so `with_keep_alive(None)`
    // can't drop the crate default.
    let keep_alive = ollama_keep_alive::request_keep_alive(keep_alive);
    // `config.base_url` is a bare host (config strips any `/api/generate`);
    // OllamaProvider posts to its `base_url` verbatim, so append the endpoint
    // exactly as the old hand-built POST did.
    let endpoint = format!("{}/api/generate", base_url.trim_end_matches('/'));
    let provider = crate::providers::ollama::OllamaProvider::with_base_url(endpoint)
        .with_keep_alive(keep_alive);
    let mut request = LLMRequest {
        model: model.to_string(),
        extra: Some(
            serde_json::json!({ "prompt": prompt, "options": { "num_ctx": context_tokens } })
                .into(),
        ),
        max_output_tokens: Some(((max_chars_out / 3).max(64)) as u32),
        reasoning: None,
        metadata: RequestMetadata {
            operation: "local_prep_summarise".into(),
            timeout_secs: Some(timeout.as_secs()),
            ..Default::default()
        },
        ..Default::default()
    };
    request.metadata.set_trace_context(trace_context.clone());
    let provider_attempt_count = request.metadata.record_provider_attempt();
    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let started = Instant::now();
    let (provider_result, cancellation) = tokio::select! {
        biased;
        _ = wait_for_cancellation(task_cancel) => (
            Err(crate::error::LLMError::Cancelled {
                reason: "task_cancelled_during_local_prep".to_string(),
            }),
            Some(LocalPrepCancellation::Task),
        ),
        _ = wait_for_cancellation(shutdown_cancel) => (
            Err(crate::error::LLMError::Cancelled {
                reason: "queue_shutdown_during_local_prep".to_string(),
            }),
            Some(LocalPrepCancellation::QueueShutdown),
        ),
        result = crate::provider::LLMProvider::invoke(&provider, request) => (result, None),
    };
    let completed_at_ms = chrono::Utc::now().timestamp_millis();
    let latency_ms = started.elapsed().as_millis() as u64;
    let (result, tokens, error_class) = match provider_result {
        Ok(response) => {
            let tokens = response.usage.as_ref().map(|usage| TokenSummary {
                prompt_tokens: usage.prompt_tokens.unwrap_or(0),
                completion_tokens: usage.completion_tokens.unwrap_or(0),
                cached_tokens: usage.cached_tokens.unwrap_or(0),
                reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
            });
            let text = response.text.as_deref().unwrap_or_default().to_string();
            let trimmed = text.trim();
            if trimmed.is_empty() {
                (
                    Err("ollama returned empty response".to_string()),
                    tokens,
                    Some("empty_response".to_string()),
                )
            } else {
                (
                    Ok(trimmed.chars().take(max_chars_out).collect()),
                    tokens,
                    None,
                )
            }
        },
        Err(error) => (
            Err(error.to_string()),
            None,
            Some(local_prep_error_class(&error).to_string()),
        ),
    };
    let call = LocalPrepCallStat {
        provider_attempt_id: trace_context.provider_attempt_id(provider_attempt_count),
        trace_context,
        provider_attempt_count,
        provider: "ollama".to_string(),
        model: model.to_string(),
        purpose: purpose.as_str().to_string(),
        started_at_ms,
        completed_at_ms,
        latency_ms,
        success: result.is_ok(),
        error_class,
        tokens,
    };
    (result, call, cancellation)
}

async fn wait_for_cancellation(token: Option<&CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

fn cancellation_state(
    task_cancel: Option<&CancellationToken>,
    shutdown_cancel: Option<&CancellationToken>,
) -> Option<LocalPrepCancellation> {
    if task_cancel.is_some_and(CancellationToken::is_cancelled) {
        Some(LocalPrepCancellation::Task)
    } else if shutdown_cancel.is_some_and(CancellationToken::is_cancelled) {
        Some(LocalPrepCancellation::QueueShutdown)
    } else {
        None
    }
}

fn local_prep_error_class(error: &crate::error::LLMError) -> &'static str {
    match error {
        crate::error::LLMError::Timeout | crate::error::LLMError::DeadlineExceeded => "timeout",
        crate::error::LLMError::RateLimited { .. } => "rate_limit",
        crate::error::LLMError::Cancelled { .. } => "cancelled",
        crate::error::LLMError::ProviderUnavailable => "provider_unavailable",
        crate::error::LLMError::Validation(_) => "validation",
        _ => "provider_error",
    }
}

fn build_prompt(purpose: SummarisationPurpose, raw: &str, max_chars_out: usize) -> String {
    match purpose {
        SummarisationPurpose::ConsolidationEpisode => format!(
            "Summarise the following agent execution episode for memory \
             consolidation. Preserve: decisions made, tools invoked, outcomes, \
             and any user-stated preferences. Output ≤{} characters of plain \
             prose, no markdown.\n\n---\n{}\n---\n\nSummary:",
            max_chars_out, raw
        ),
        SummarisationPurpose::LargeStepOutput => format!(
            "Summarise the following step output. Preserve: errors, final \
             results, structural shape. Drop verbose logs. Output ≤{} chars \
             plain prose.\n\n---\n{}\n---\n\nSummary:",
            max_chars_out, raw
        ),
        SummarisationPurpose::Other => format!(
            "Summarise concisely (≤{} chars):\n\n{}\n\nSummary:",
            max_chars_out, raw
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        LLMMessage, LlmDisclosureAuthorizer, LlmDisclosureCapturePolicy, LlmDisclosureGuard,
        MessageRole,
    };

    #[derive(Debug)]
    struct CurrentDisclosureAuthority;

    #[async_trait::async_trait]
    impl LlmDisclosureAuthorizer for CurrentDisclosureAuthority {
        async fn revalidate(
            &self,
            _profile: &str,
            _provider: &crate::capability::LLMProviderKind,
            _model: &str,
            _api_base_url: Option<&str>,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn disabled_config() -> LocalPrepConfig {
        LocalPrepConfig {
            enabled: false,
            ..LocalPrepConfig::default()
        }
    }

    #[test]
    fn coalesce_merges_consecutive_text_blocks() {
        let mut content = vec![
            ContentBlock::Text {
                text: "before ".to_string(),
            },
            ContentBlock::Text {
                text: "RAW".to_string(),
            },
            ContentBlock::Text {
                text: " after".to_string(),
            },
        ];
        coalesce_consecutive_text(&mut content);
        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "before RAW after"),
            other => panic!("expected single Text block, got {other:?}"),
        }
    }

    #[test]
    fn coalesce_stops_at_non_text_barrier() {
        let mut content = vec![
            ContentBlock::Text {
                text: "a".to_string(),
            },
            ContentBlock::Text {
                text: "b".to_string(),
            },
            ContentBlock::Image {
                data: vec![1, 2],
                media_type: "image/png".to_string(),
                caption: None,
            },
            ContentBlock::Text {
                text: "c".to_string(),
            },
        ];
        coalesce_consecutive_text(&mut content);
        // "ab" merged, image barrier, then "c" — three blocks.
        assert_eq!(content.len(), 3);
        assert!(matches!(&content[0], ContentBlock::Text { text } if text == "ab"));
        assert!(matches!(&content[1], ContentBlock::Image { .. }));
        assert!(matches!(&content[2], ContentBlock::Text { text } if text == "c"));
    }

    #[tokio::test]
    async fn disabled_local_prep_reconstructs_single_contiguous_block() {
        // Simulates the consolidator carve: one user message split into
        // [before, placeholder, after] with the raw body in a SummarisableBlock.
        // With local-prep disabled, the worker inlines raw and coalescing must
        // yield a single block byte-identical to the original inline prompt.
        let before = "Consolidate.\n\nSource data (JSON):\n";
        let raw = "[{\"e\":1}]";
        let after = "\n\nReturn strict JSON only.";
        let mut request = LLMRequest {
            messages: vec![LLMMessage {
                role: MessageRole::User,
                content: vec![
                    ContentBlock::Text {
                        text: before.to_string(),
                    },
                    ContentBlock::Text {
                        text: String::new(),
                    }, // placeholder
                    ContentBlock::Text {
                        text: after.to_string(),
                    },
                ],
            }]
            .into(),
            summarisable_blocks: vec![SummarisableBlock {
                message_index: 0,
                content_index: 1,
                raw: raw.to_string(),
                purpose: SummarisationPurpose::ConsolidationEpisode,
                max_chars: None,
            }]
            .into(),
            ..Default::default()
        };

        let stat = maybe_local_prep_direct(&mut request, &disabled_config()).await;
        assert!(stat.is_some(), "fall-through still reports a stat");
        assert!(request.summarisable_blocks.is_empty(), "blocks consumed");
        assert_eq!(
            request.messages[0].content.len(),
            1,
            "coalesced to one block"
        );
        match &request.messages[0].content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(text, &format!("{before}{raw}{after}"));
            },
            other => panic!("expected single Text block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disclosure_bound_request_never_enters_unattested_local_prep() {
        let raw = "protected app content";
        let mut request = LLMRequest {
            messages: vec![LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: String::new(),
                }],
            }]
            .into(),
            summarisable_blocks: vec![SummarisableBlock {
                message_index: 0,
                content_index: 0,
                raw: raw.to_string(),
                purpose: SummarisationPurpose::Other,
                max_chars: None,
            }]
            .into(),
            ..Default::default()
        };
        request.metadata.set_disclosure_guard(
            LlmDisclosureGuard::new(
                "guarded-profile",
                "0".repeat(64),
                "continuation-partition",
                "policy-digest",
                LlmDisclosureCapturePolicy::MetadataOnly,
                Arc::new(CurrentDisclosureAuthority),
            )
            .expect("guard"),
        );
        let config = LocalPrepConfig {
            enabled: true,
            threshold_chars: 0,
            base_url: "http://127.0.0.1:1".to_string(),
            ..LocalPrepConfig::default()
        };

        let stat = maybe_local_prep_direct(&mut request, &config)
            .await
            .expect("raw fall-through stat");
        assert_eq!(stat.blocks_processed, 0);
        assert_eq!(stat.chars_in, raw.len() as u64);
        assert_eq!(stat.chars_out, 0);
        assert!(matches!(
            &request.messages[0].content[0],
            ContentBlock::Text { text } if text == raw
        ));
    }

    fn request_with_raw_block(raw: &str) -> LLMRequest {
        LLMRequest {
            summarisable_blocks: vec![SummarisableBlock {
                message_index: 0,
                content_index: 0,
                raw: raw.to_string(),
                purpose: SummarisationPurpose::Other,
                max_chars: None,
            }]
            .into(),
            ..Default::default()
        }
    }

    #[test]
    fn local_prep_needs_ollama_is_false_on_cheap_paths() {
        let large = request_with_raw_block("0123456789");
        assert!(
            !local_prep_needs_ollama(
                &large,
                &LocalPrepConfig {
                    enabled: false,
                    yield_worker: true,
                    threshold_chars: 8,
                    ..LocalPrepConfig::default()
                }
            ),
            "disabled prep must stay on the worker"
        );
        assert!(
            !local_prep_needs_ollama(
                &large,
                &LocalPrepConfig {
                    enabled: true,
                    yield_worker: false,
                    threshold_chars: 8,
                    ..LocalPrepConfig::default()
                }
            ),
            "yield_worker=false restores in-worker HOL"
        );
        assert!(
            !local_prep_needs_ollama(
                &request_with_raw_block("short"),
                &LocalPrepConfig {
                    enabled: true,
                    yield_worker: true,
                    threshold_chars: 32,
                    ..LocalPrepConfig::default()
                }
            ),
            "below-threshold inlining stays on the worker"
        );

        let mut disclosure_bound = request_with_raw_block("0123456789");
        disclosure_bound.metadata.set_disclosure_guard(
            LlmDisclosureGuard::new(
                "guarded-profile",
                "0".repeat(64),
                "continuation-partition",
                "policy-digest",
                LlmDisclosureCapturePolicy::MetadataOnly,
                Arc::new(CurrentDisclosureAuthority),
            )
            .expect("guard"),
        );
        assert!(
            !local_prep_needs_ollama(
                &disclosure_bound,
                &LocalPrepConfig {
                    enabled: true,
                    yield_worker: true,
                    threshold_chars: 8,
                    ..LocalPrepConfig::default()
                }
            ),
            "disclosure-bound requests must not park for unattested Ollama"
        );
    }

    #[test]
    fn local_prep_needs_ollama_is_true_when_ollama_generate_would_run() {
        let config = LocalPrepConfig {
            enabled: true,
            yield_worker: true,
            threshold_chars: 8,
            ..LocalPrepConfig::default()
        };
        assert!(local_prep_needs_ollama(
            &request_with_raw_block("01234567"),
            &config
        ));
        assert!(local_prep_needs_ollama(
            &request_with_raw_block("0123456789"),
            &config
        ));
    }
}
