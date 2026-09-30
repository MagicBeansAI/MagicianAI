//! Finish a qualified decision without allowing text generation to change it.
//!
//! This layer never mutates memory. The caller still owns source revalidation,
//! schema validation, permissions and the final storage transaction.
use std::future::Future;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextNeed {
    /// A negative decision, or a decision with no prose consumer.
    None,
    /// A useful compaction: failure retains the existing source text.
    Optional,
    /// Publishing/promoting this item requires valid generated content.
    Required,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Completed<D, T> {
    Ready { decision: D, text: Option<T> },
    MissingRequiredText,
    StalePolicy,
}

/// `D` is immutable and is returned separately from generated `T`. A generator
/// cannot overwrite approved heads. Valid incumbent text is reused exactly
/// once; optional failure never fabricates an empty replacement.
pub(crate) async fn complete<D, T, G, F, V, C>(
    decision: D,
    need: TextNeed,
    incumbent_text: Option<T>,
    generate: G,
    validate: V,
    current: C,
) -> Completed<D, T>
where
    G: FnOnce() -> F,
    F: Future<Output = Option<T>>,
    V: Fn(&D, &T) -> bool,
    C: Fn() -> bool,
{
    if !current() {
        return Completed::StalePolicy;
    }
    if need == TextNeed::None {
        return Completed::Ready {
            decision,
            text: None,
        };
    }
    let text = match incumbent_text.filter(|text| validate(&decision, text)) {
        Some(text) => Some(text),
        None => generate().await.filter(|text| validate(&decision, text)),
    };
    // Generating prose can take seconds; a gate disabled meanwhile has no
    // authority to publish the resulting item.
    if !current() {
        return Completed::StalePolicy;
    }
    if need == TextNeed::Required && text.is_none() {
        Completed::MissingRequiredText
    } else {
        Completed::Ready { decision, text }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test]
    async fn memory_decision_mixed_reuses_valid_incumbent_once() {
        let result = complete(
            true,
            TextNeed::Required,
            Some("valid"),
            || async { panic!("must reuse incumbent text") },
            |_, text| *text == "valid",
            || true,
        )
        .await;
        assert_eq!(
            result,
            Completed::Ready {
                decision: true,
                text: Some("valid")
            }
        );
    }

    #[tokio::test]
    async fn memory_decision_mixed_negative_skips_generation() {
        let result = complete(
            false,
            TextNeed::None,
            None::<String>,
            || async { panic!("negative decision must not generate text") },
            |_, _| true,
            || true,
        )
        .await;
        assert_eq!(
            result,
            Completed::Ready {
                decision: false,
                text: None
            }
        );
    }

    #[tokio::test]
    async fn memory_decision_mixed_positive_generates_and_keeps_head() {
        let calls = Cell::new(0);
        let result = complete(
            "surface",
            TextNeed::Required,
            None,
            || async {
                calls.set(calls.get() + 1);
                Some("cited summary")
            },
            |head, text| *head == "surface" && *text == "cited summary",
            || true,
        )
        .await;
        assert_eq!(calls.get(), 1);
        assert_eq!(
            result,
            Completed::Ready {
                decision: "surface",
                text: Some("cited summary")
            }
        );
    }

    #[tokio::test]
    async fn memory_decision_mixed_invalid_required_text_cannot_apply() {
        let result = complete(
            true,
            TextNeed::Required,
            None,
            || async { Some("forged citation") },
            |_, _| false,
            || true,
        )
        .await;
        assert_eq!(result, Completed::MissingRequiredText);
    }

    #[tokio::test]
    async fn memory_decision_mixed_optional_failure_retains_source() {
        let result = complete(
            "useful",
            TextNeed::Optional,
            None::<String>,
            || async { None },
            |_, _| true,
            || true,
        )
        .await;
        assert_eq!(
            result,
            Completed::Ready {
                decision: "useful",
                text: None
            }
        );
    }

    #[tokio::test]
    async fn memory_decision_mixed_rollback_during_generation_cannot_apply() {
        let enabled = Cell::new(true);
        let result = complete(
            true,
            TextNeed::Required,
            None,
            || async {
                enabled.set(false);
                Some("valid")
            },
            |_, _| true,
            || enabled.get(),
        )
        .await;
        assert_eq!(result, Completed::StalePolicy);
    }
}
