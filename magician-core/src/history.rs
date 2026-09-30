use serde::{Deserialize, Serialize};

/// User-facing history is split between explicitly owned conversations and
/// product-generated activity. `Legacy` is an internal deserialization marker
/// and is normalized before records leave their stores.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HistoryLane {
    Personal,
    Automated,
    #[default]
    Legacy,
}

impl HistoryLane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Automated => "automated",
            Self::Legacy => "legacy",
        }
    }

    pub fn parse_filter(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "personal" => Some(Self::Personal),
            "automated" => Some(Self::Automated),
            _ => None,
        }
    }
}

/// Existing records predate explicit provenance, so only the permanent
/// `#general` thread can be proven user-owned. Explicitly created history is
/// assigned `Personal` at creation; every other legacy record is conservative.
pub fn infer_legacy_history_lane(ui_thread_id: &str) -> HistoryLane {
    let id = ui_thread_id.trim().to_ascii_lowercase();
    if id == "general" {
        HistoryLane::Personal
    } else {
        HistoryLane::Automated
    }
}

/// Seed legacy chat sessions without treating every historical `#general`
/// session as user-created. This allowlist is only consulted when the durable
/// record has no explicit lane; new sessions always carry caller provenance.
pub fn infer_legacy_session_history_lane(ui_thread_id: &str, title: Option<&str>) -> HistoryLane {
    if ui_thread_id.trim().eq_ignore_ascii_case("general") {
        let title = title.unwrap_or_default().trim().to_ascii_lowercase();
        if title == "wtf" || title.starts_with("hi i think you have access") {
            return HistoryLane::Personal;
        }
    }
    HistoryLane::Automated
}

#[cfg(test)]
mod tests {
    use super::{infer_legacy_history_lane, infer_legacy_session_history_lane, HistoryLane};

    #[test]
    fn legacy_classifier_keeps_only_general_personal() {
        assert_eq!(infer_legacy_history_lane("general"), HistoryLane::Personal);
        assert_eq!(
            infer_legacy_history_lane("travel-plans"),
            HistoryLane::Automated
        );
    }

    #[test]
    fn legacy_classifier_recognizes_product_owned_thread_namespaces() {
        assert_eq!(infer_legacy_history_lane("tabs"), HistoryLane::Automated);
        assert_eq!(
            infer_legacy_history_lane("meeting-ad-hoc-2026-07-27"),
            HistoryLane::Automated
        );
        assert_eq!(
            infer_legacy_history_lane("contextual-writing:site-example-com"),
            HistoryLane::Automated
        );
    }

    #[test]
    fn legacy_session_seed_keeps_only_selected_general_sessions_personal() {
        assert_eq!(
            infer_legacy_session_history_lane("general", Some("Wtf")),
            HistoryLane::Personal
        );
        assert_eq!(
            infer_legacy_session_history_lane(
                "general",
                Some("hi i think you have access to metabase.. can you...")
            ),
            HistoryLane::Personal
        );
        assert_eq!(
            infer_legacy_session_history_lane("general", Some("Generated summary")),
            HistoryLane::Automated
        );
        assert_eq!(
            infer_legacy_session_history_lane("travel", Some("Wtf")),
            HistoryLane::Automated
        );
    }
}
