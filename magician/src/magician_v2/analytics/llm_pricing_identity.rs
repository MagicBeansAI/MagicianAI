//! Canonical provider identity used by every LLM analytics pricing path.
//!
//! Runtime adapters historically emitted a small number of implementation
//! labels for vendor providers. Pricing may recognize those exact aliases,
//! but must never infer vendor ownership from a prefix: an OpenAI-compatible
//! private endpoint is a distinct billing route.

use magicllm::LLMProviderKind;

pub fn provider_kind_for_pricing(provider: &str) -> LLMProviderKind {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "openai"
        | "openai_chat"
        | "openai_responses"
        | "openai_realtime"
        | "openai_realtime_backend"
        | "openai_live" => LLMProviderKind::OpenAI,
        "anthropic" => LLMProviderKind::Anthropic,
        "minimax" => LLMProviderKind::Minimax,
        _ => LLMProviderKind::from_str(&normalized),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn exact_runtime_adapter_aliases_share_the_vendor_pricing_identity() {
        for alias in [
            "openai",
            "openai_chat",
            "openai_responses",
            "openai_realtime",
            // The GPT-Live adapter's own label. Without it a Live session
            // priced as a custom provider, which has no realtime table at
            // all: every Live row read `runtime-pricing-miss@call-time` and
            // cost Unknown, whatever the session had actually cost.
            "openai_live",
            "openai_realtime_backend",
        ] {
            assert_eq!(provider_kind_for_pricing(alias), LLMProviderKind::OpenAI);
        }
    }

    #[test]
    fn vendor_looking_custom_providers_never_inherit_vendor_pricing() {
        assert_eq!(
            provider_kind_for_pricing("openai-compatible-private"),
            LLMProviderKind::Custom("openai-compatible-private".to_string())
        );
        assert_eq!(
            provider_kind_for_pricing("private-realtime"),
            LLMProviderKind::Custom("private-realtime".to_string())
        );
    }
}
