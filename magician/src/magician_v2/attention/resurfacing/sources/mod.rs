//! Corpus sources for the Proactive Resurfacing Engine.
//!
//! A [`ResurfacingSource`] adapts one durable substrate (memory, tasks,
//! episodes, comms, calendar) into a stream of [`CorpusItem`]s newer than a
//! per-source watermark, so the (later) worker can scan each substrate
//! incrementally without re-scanning history. Each source owns a stable
//! [`corpus_kind`](ResurfacingSource::corpus_kind) key that names its
//! watermark row in the store.
//!
//! Task 5 lands the trait + the [`memory::MemorySource`] adapter; the
//! task/episode/comm/calendar adapters land in later tasks (12–14).
//!
//! The comms adapter reads `ChannelAssistStore` and therefore stayed in
//! `magician-comms` (`channel_assist::resurfacing::sources::comms`),
//! implementing this trait from the satellite side (plan workstream 3.0).

use async_trait::async_trait;

use super::types::CorpusItem;

pub mod memory;
pub mod task_episode;

/// A durable substrate the resurfacing worker can scan incrementally.
///
/// Implementations are held behind `dyn ResurfacingSource` by the worker, so
/// the trait is object-safe and `Send + Sync`.
#[async_trait]
pub trait ResurfacingSource: Send + Sync {
    /// Stable corpus kind key used for the per-source watermark row
    /// (e.g. `"memory"`, `"task"`). MUST match
    /// [`SourceKind::as_str`](super::types::SourceKind::as_str) where
    /// applicable.
    fn corpus_kind(&self) -> &'static str;

    /// Items newer than `watermark`, normalized from the source substrate for
    /// the given scope. The watermark is source-native: for memory/tasks it is
    /// unix seconds, while comms may use provider millisecond cursors. Returned
    /// items must carry a `watermark_cursor` greater than the input watermark so
    /// re-running against an unchanged substrate yields nothing.
    async fn list_changed_since(
        &self,
        principal: &str,
        workspace: &str,
        watermark: i64,
    ) -> anyhow::Result<Vec<CorpusItem>>;
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::attention::resurfacing::types::SourceKind;

    /// Minimal in-memory source proving the trait contract independent of any
    /// real substrate: the watermark filter is strict-greater-than, and the
    /// trait is usable behind `Box<dyn ResurfacingSource>` (object-safe).
    struct StaticSource {
        items: Vec<CorpusItem>,
    }

    #[async_trait]
    impl ResurfacingSource for StaticSource {
        fn corpus_kind(&self) -> &'static str {
            "mock"
        }

        async fn list_changed_since(
            &self,
            _principal: &str,
            _workspace: &str,
            watermark: i64,
        ) -> anyhow::Result<Vec<CorpusItem>> {
            Ok(self
                .items
                .iter()
                .filter(|it| it.watermark_cursor > watermark)
                .cloned()
                .collect())
        }
    }

    fn item(source_ref: &str, occurred_at: i64) -> CorpusItem {
        CorpusItem {
            source_kind: SourceKind::Memory,
            source_ref: source_ref.to_string(),
            title: source_ref.to_string(),
            digest: source_ref.to_string(),
            content_details: None,
            content_revision: None,
            occurred_at,
            watermark_cursor: occurred_at,
            embedding_text: source_ref.to_string(),
        }
    }

    #[tokio::test]
    async fn watermark_filter_is_strict_and_dyn_compatible() {
        let source: Box<dyn ResurfacingSource> = Box::new(StaticSource {
            items: vec![item("a", 100), item("b", 200)],
        });
        assert_eq!(source.corpus_kind(), "mock");
        // watermark == 100 → the boundary item ("a") is excluded (strict >).
        let got = source.list_changed_since("p", "w", 100).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source_ref, "b");
    }
}
