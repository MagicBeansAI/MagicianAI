//! Owner-specific source guard for the sent-index packet.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

pub const SENT_INDEX_OWNER_ID: &str = "delivery_receipts";

pub fn sent_index_source_guard() -> SourceGuard {
    SourceGuard::enabled(
        StorageCatalogId::parse(SENT_INDEX_OWNER_ID).expect("catalog id"),
        [
            "magician/src/magician_v2/delivery_receipts/sent_index.rs",
            "magician/src/magician_v2/delivery_receipts/store.rs",
            "magician/src/magician_v2/delivery_receipts/sqlite.rs",
            "magician/src/magician_v2/delivery_receipts/migration.rs",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_sent_index_new_stays_on_the_allowlist() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/magician_v2");
        let mut offenders = Vec::new();
        for rel in [
            "delivery_receipts/ingest.rs",
            "delivery_receipts/pull.rs",
            "delivery_receipts/dsn.rs",
            "delivery_receipts/maildir.rs",
            "delivery_hygiene/worker.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("SentMessageIndex::new") {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "sent-index callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = sent_index_source_guard();
        guard
            .check("magician/src/magician_v2/delivery_receipts/sent_index.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
