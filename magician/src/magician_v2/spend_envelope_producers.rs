//! Shared helpers for the spend envelope's explicit producers: work paid for
//! outside the router (harness turns, coding-engine turns, the Decision
//! Engine's model calls, realtime voice).
//!
//! Each producer needs the same three things: the workload class its work
//! counts under, an up-front refusal while the envelope is spent (the router
//! cannot refuse calls it never sees), and a dollar figure for a turn whose
//! engine reported tokens but no cost. Everything here is a thin layer over
//! [`super::spend_envelope`]'s public API; the envelope's own accounting stays
//! in that file.

use chrono::Utc;
use magicllm::types::TokenUsage;
use magicllm::{LLMError, LLMProviderKind, LlmWorkloadClass};

use super::execution::agentic::types::AgenticContext;
use super::spend_envelope::{self, SpendEnvelope};

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// The class a run's work counts under. A run launched from a chat (or
/// backing an inline chat turn) is the owner's interactive work and is never
/// refused; any other run is autonomous and gated.
pub fn run_workload_class(chat_launched: bool) -> LlmWorkloadClass {
    if chat_launched {
        LlmWorkloadClass::InteractiveTask
    } else {
        LlmWorkloadClass::AutonomousTask
    }
}

/// [`run_workload_class`] for an agentic run: chat lineage
/// (`chat_session_id`) or an inline chat turn (`chat_inline`) is the evidence
/// a person asked for it, as `agentic_run_workload_class` reads it.
pub fn agentic_run_class(ctx: &AgenticContext) -> LlmWorkloadClass {
    run_workload_class(ctx.chat_inline || ctx.chat_session_id.is_some())
}

/// Refuse to start work of `class` while the installed envelope is spent.
/// Exempt classes always pass, and so does everything when no envelope is
/// installed. The error is the router's typed refusal, so an agent loop parks
/// the run (`spend_envelope::park_wait_for_error`) instead of failing it.
pub fn preflight(class: LlmWorkloadClass) -> anyhow::Result<()> {
    preflight_with(
        spend_envelope::envelope().map(|envelope| &**envelope),
        class,
        now_ms(),
    )
}

fn preflight_with(
    envelope: Option<&SpendEnvelope>,
    class: LlmWorkloadClass,
    now_ms: i64,
) -> anyhow::Result<()> {
    let Some(envelope) = envelope else {
        return Ok(());
    };
    envelope.check_at(class, now_ms).map_err(|resets_at_ms| {
        anyhow::Error::from(LLMError::SpendEnvelopeExhausted { resets_at_ms })
    })
}

/// Count `micro_usd` toward the installed envelope. `estimated` marks a
/// figure Magician priced itself from tokens rather than one the producer
/// reported, so the owner's status shows how much of the spend is a guess.
pub fn charge_micro(
    class: LlmWorkloadClass,
    principal: &str,
    workspace: &str,
    micro_usd: u64,
    estimated: bool,
) {
    if let Some(envelope) = spend_envelope::envelope() {
        charge_micro_with(
            envelope,
            class,
            principal,
            workspace,
            micro_usd,
            estimated,
            now_ms(),
        );
    }
}

fn charge_micro_with(
    envelope: &SpendEnvelope,
    class: LlmWorkloadClass,
    principal: &str,
    workspace: &str,
    micro_usd: u64,
    estimated: bool,
    now_ms: i64,
) {
    if micro_usd > 0 {
        envelope.charge_at(class, principal, workspace, micro_usd, estimated, now_ms);
    }
}

/// What reported tokens cost, in micro-USD, and whether that figure is an
/// estimate: the pricing table's row for `(provider, model)` when it has one
/// (exact), else the highest rate it knows for the provider, or for any
/// provider (estimated, over-stating on purpose). `None` when the usage holds
/// no tokens at all: there is nothing to price, and nothing is guessed.
pub fn price_tokens(
    provider: &LLMProviderKind,
    model: Option<&str>,
    usage: &TokenUsage,
    at_ms: i64,
) -> Option<(u64, bool)> {
    let tokens = u64::from(usage.prompt_tokens.unwrap_or(0))
        .saturating_add(u64::from(usage.completion_tokens.unwrap_or(0)));
    if tokens == 0 {
        return None;
    }
    if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
        // A CLI may name its model `provider/model`; the table keys on the
        // bare model name.
        let bare = model.rsplit('/').next().unwrap_or(model);
        for candidate in [model, bare] {
            if let Some(usd) =
                magicllm::pricing::try_compute_cost_at(provider, candidate, usage, at_ms)
            {
                return magicllm::pricing::usd_to_microusd_ceil(usd).map(|micro| (micro, false));
            }
        }
    }
    Some((
        magicllm::pricing::estimate_unpriced_cost_microusd(provider, Some(usage), 0, at_ms),
        true,
    ))
}

/// Token counts as the pricing table reads them. `input` is every input
/// token, cached or not; `cached` and `cache_write` are parts of it.
pub fn token_usage(
    input: u64,
    output: u64,
    cached: Option<u64>,
    cache_write: Option<u64>,
) -> TokenUsage {
    let narrow = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
    TokenUsage {
        prompt_tokens: Some(narrow(input)),
        completion_tokens: Some(narrow(output)),
        total_tokens: Some(narrow(input.saturating_add(output))),
        cached_tokens: cached.map(narrow),
        cache_creation_tokens: cache_write.map(narrow),
        ..TokenUsage::default()
    }
}

/// The provider a CLI harness or coding engine bills through: from the model
/// name it reported when that names a known provider family, else from the
/// engine. `Custom(engine)` when neither says, which prices at the highest
/// rate any provider has.
pub fn engine_provider(engine: &str, model: Option<&str>) -> LLMProviderKind {
    if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
        if let Some((prefix, _)) = model.split_once('/') {
            let named = LLMProviderKind::from_str(&prefix.to_ascii_lowercase());
            if !matches!(named, LLMProviderKind::Custom(_)) {
                return named;
            }
        }
        let bare = model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .to_ascii_lowercase();
        if bare.starts_with("claude") {
            return LLMProviderKind::Anthropic;
        }
        if bare.starts_with("gpt")
            || bare.starts_with("codex")
            || bare.starts_with("o1")
            || bare.starts_with("o3")
            || bare.starts_with("o4")
        {
            return LLMProviderKind::OpenAI;
        }
        if bare.starts_with("gemini") {
            return LLMProviderKind::Gemini;
        }
        if bare.starts_with("grok") {
            return LLMProviderKind::Xai;
        }
        if bare.starts_with("deepseek") {
            return LLMProviderKind::DeepSeek;
        }
        if bare.starts_with("minimax") {
            return LLMProviderKind::Minimax;
        }
    }
    match engine.trim() {
        "claude_code" | "claude" => LLMProviderKind::Anthropic,
        "codex" | "codex_app_server" => LLMProviderKind::OpenAI,
        "grok" | "grok_acp" => LLMProviderKind::Xai,
        "agy" | "agy_cli" | "gemini" => LLMProviderKind::Gemini,
        other => LLMProviderKind::Custom(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BudgetCapUsd, SpendEnvelopeSettings};

    fn envelope_with(daily_usd: f64) -> SpendEnvelope {
        SpendEnvelope::with_settings(&SpendEnvelopeSettings {
            daily_usd: BudgetCapUsd::Usd(daily_usd),
            ..SpendEnvelopeSettings::default()
        })
    }

    #[test]
    fn a_chat_launched_run_is_interactive_and_never_refused() {
        let now = now_ms();
        let envelope = envelope_with(1.0);
        charge_micro_with(
            &envelope,
            LlmWorkloadClass::AutonomousTask,
            "owner",
            "default",
            2_000_000,
            false,
            now,
        );
        assert_eq!(run_workload_class(true), LlmWorkloadClass::InteractiveTask);
        preflight_with(Some(&envelope), run_workload_class(true), now)
            .expect("chat-launched work is exempt");
        let refused = preflight_with(Some(&envelope), run_workload_class(false), now)
            .expect_err("autonomous work is refused once the envelope is spent");
        assert!(
            spend_envelope::is_exhaustion_error(&refused),
            "the refusal is the typed error the loop parks on"
        );
        preflight_with(None, LlmWorkloadClass::AutonomousTask, now)
            .expect("no envelope installed: nothing is gated");
    }

    #[test]
    fn an_agentic_run_is_interactive_only_with_chat_lineage() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        assert_eq!(agentic_run_class(&ctx), LlmWorkloadClass::AutonomousTask);
        ctx.chat_session_id = Some("session".into());
        assert_eq!(agentic_run_class(&ctx), LlmWorkloadClass::InteractiveTask);
        ctx.chat_session_id = None;
        ctx.chat_inline = true;
        assert_eq!(agentic_run_class(&ctx), LlmWorkloadClass::InteractiveTask);
    }

    #[test]
    fn estimated_charges_are_counted_and_marked() {
        let now = now_ms();
        let envelope = envelope_with(100.0);
        charge_micro_with(
            &envelope,
            LlmWorkloadClass::InteractiveTask,
            "owner",
            "default",
            1_500_000,
            true,
            now,
        );
        charge_micro_with(
            &envelope,
            LlmWorkloadClass::InteractiveTask,
            "owner",
            "default",
            0,
            true,
            now,
        );
        let status = envelope.status_at(now);
        assert_eq!(status.spent_usd, 1.5);
        assert_eq!(status.estimated_calls, 1, "a zero charge is not a call");
        assert_eq!(status.by_class.get("interactive_task"), Some(&1.5));
    }

    #[test]
    fn a_turn_with_no_tokens_is_not_priced() {
        let usage = token_usage(0, 0, None, None);
        assert_eq!(
            price_tokens(&LLMProviderKind::OpenAI, Some("gpt-5"), &usage, now_ms()),
            None
        );
    }

    #[test]
    fn an_unlisted_model_is_estimated_not_free() {
        let usage = token_usage(100_000, 10_000, None, None);
        let (micro, estimated) = price_tokens(
            &LLMProviderKind::Custom("harness-unknown".into()),
            Some("no-such-model-anywhere"),
            &usage,
            now_ms(),
        )
        .expect("tokens are priced");
        assert!(estimated);
        assert!(
            micro > 0,
            "an unpriced model is charged the highest known rate"
        );
    }

    #[test]
    fn the_provider_comes_from_the_model_then_the_engine() {
        assert_eq!(
            engine_provider("codex", Some("claude-sonnet-4-5")),
            LLMProviderKind::Anthropic
        );
        assert_eq!(
            engine_provider("pi", Some("openai/gpt-5.6")),
            LLMProviderKind::OpenAI
        );
        assert_eq!(engine_provider("codex", None), LLMProviderKind::OpenAI);
        assert_eq!(engine_provider("grok", Some("")), LLMProviderKind::Xai);
        assert_eq!(
            engine_provider("pi", Some("mystery")),
            LLMProviderKind::Custom("pi".into())
        );
    }
}
