use crate::{
    config::{LLMProfile, DEFAULT_CONTEXT_SAFETY_MARGIN_TOKENS},
    types::LLMRequest,
};

use super::{ContextError, TokenEstimator};

/// Whether a context preflight is telemetry-only or rejects overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPreflightMode {
    Observe,
    Enforce,
}

impl ContextPreflightMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

/// Complete physical-context budget decision for one provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPreflight {
    pub estimator: &'static str,
    pub source_bytes: u64,
    pub estimated_input_tokens: u32,
    pub reserved_output_tokens: u32,
    pub safety_margin_tokens: u32,
    pub required_tokens: u64,
    pub context_window_tokens: u32,
    pub would_overflow: bool,
    pub mode: ContextPreflightMode,
}

impl ContextPreflight {
    pub fn enforce(&self) -> Result<(), ContextError> {
        if self.mode == ContextPreflightMode::Enforce && self.would_overflow {
            return Err(ContextError::PhysicalContextWindowExceeded {
                estimated_input_tokens: self.estimated_input_tokens,
                reserved_output_tokens: self.reserved_output_tokens,
                safety_margin_tokens: self.safety_margin_tokens,
                required_tokens: self.required_tokens,
                context_window_tokens: self.context_window_tokens,
            });
        }
        Ok(())
    }
}

/// Calculate physical capacity for a resolved profile/request pair.
///
/// A profile without a typed context window is outside Phase 1 preflight and
/// returns `None`. Chunking-disabled profiles observe only; explicitly enabled
/// profiles enforce the same calculation before provider invocation.
pub fn preflight_request(
    profile: &LLMProfile,
    request: &LLMRequest,
    estimator: &dyn TokenEstimator,
) -> Result<Option<ContextPreflight>, ContextError> {
    let Some(context_window_tokens) = profile.context_window_tokens else {
        return Ok(None);
    };
    let estimate = estimator.estimate_request(&request.model, request);
    let reserved_output_tokens = request.max_output_tokens.unwrap_or(0);
    let safety_margin_tokens = profile
        .chunking
        .as_ref()
        .map(|chunking| chunking.safety_margin_tokens)
        .unwrap_or(DEFAULT_CONTEXT_SAFETY_MARGIN_TOKENS);
    let required_tokens = u64::from(estimate.estimated_tokens)
        .saturating_add(u64::from(reserved_output_tokens))
        .saturating_add(u64::from(safety_margin_tokens));
    let mode = if profile
        .chunking
        .as_ref()
        .map(|chunking| chunking.enabled)
        .unwrap_or(false)
    {
        ContextPreflightMode::Enforce
    } else {
        ContextPreflightMode::Observe
    };

    Ok(Some(ContextPreflight {
        estimator: estimate.estimator,
        source_bytes: estimate.source_bytes,
        estimated_input_tokens: estimate.estimated_tokens,
        reserved_output_tokens,
        safety_margin_tokens,
        required_tokens,
        context_window_tokens,
        would_overflow: required_tokens > u64::from(context_window_tokens),
        mode,
    }))
}

/// Calculate the source payload available after static/output reservations.
pub fn calculate_effective_payload(
    target_payload_tokens: u32,
    context_window_tokens: u32,
    estimated_static_overhead_tokens: u32,
    reserved_output_tokens: u32,
    safety_margin_tokens: u32,
) -> Result<u32, ContextError> {
    let reserved = u64::from(estimated_static_overhead_tokens)
        .saturating_add(u64::from(reserved_output_tokens))
        .saturating_add(u64::from(safety_margin_tokens));
    if reserved >= u64::from(context_window_tokens) {
        return Err(ContextError::StaticContextWindowExceeded {
            estimated_static_overhead_tokens,
            reserved_output_tokens,
            safety_margin_tokens,
            context_window_tokens,
        });
    }
    let available = u64::from(context_window_tokens) - reserved;
    Ok(target_payload_tokens.min(u32::try_from(available).unwrap_or(u32::MAX)))
}

/// Reject a logical input above the configured end-to-end window.
pub fn validate_logical_context(
    estimated_logical_tokens: u32,
    logical_window_tokens: u32,
) -> Result<(), ContextError> {
    if estimated_logical_tokens > logical_window_tokens {
        return Err(ContextError::LogicalContextWindowExceeded {
            estimated_logical_tokens,
            logical_window_tokens,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_payload_uses_all_reservations() {
        assert_eq!(
            calculate_effective_payload(24_576, 32_768, 3_000, 4_096, 2_048),
            Ok(23_624)
        );
    }

    #[test]
    fn static_reservations_fail_before_provider_capacity_is_negative() {
        assert!(matches!(
            calculate_effective_payload(24_576, 32_768, 20_000, 12_000, 2_048),
            Err(ContextError::StaticContextWindowExceeded { .. })
        ));
    }

    #[test]
    fn logical_limit_has_typed_error() {
        assert!(matches!(
            validate_logical_context(262_145, 262_144),
            Err(ContextError::LogicalContextWindowExceeded { .. })
        ));
    }
}
