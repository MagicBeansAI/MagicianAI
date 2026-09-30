//! Source guards for Task 13 chat and progress owners.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::owners::ChatOwner;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/chat_owners/local.rs",
    "magician/src/magician_v2/chat_owners/remote.rs",
    "magician/src/magician_v2/chat_owners/migration.rs",
];

pub fn chat_owner_source_guard(owner: ChatOwner) -> SourceGuard {
    SourceGuard::enabled(
        StorageCatalogId::parse(owner.id()).expect("catalog id"),
        ALLOWED,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_stores_do_not_rebuild_the_kit() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut offenders = Vec::new();
        for rel in [
            "magician_v2/chat/storage.rs",
            "magician_v2/chat/chat_turn_event_sink.rs",
            "magician_v2/chat/enrollment.rs",
            "magician_v2/progress_channel_seam/event_log.rs",
            "magician_v2/progress_channel_seam/storage.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("LocalChatStore::for_scope_root")
                || text.contains("RemoteChatStore::new")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "chat/progress callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn progress_maps_go_through_the_kit() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/magician_v2/progress_channel_seam/storage.rs"
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert!(
            text.contains("persist_chat_file") && text.contains("store_for_any_owner"),
            "ProgressChannelStorage must publish maps through persist_chat_file"
        );
    }

    #[test]
    fn session_metadata_goes_through_the_kit() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/magician_v2/chat/storage.rs"
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert!(
            text.contains("persist_chat_file") && text.contains("store_for_any_owner"),
            "FileChatStore must publish session.json through persist_chat_file"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = chat_owner_source_guard(ChatOwner::ChatSessions);
        guard
            .check("magician/src/magician_v2/chat_owners/local.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
