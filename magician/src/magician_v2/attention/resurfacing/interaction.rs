//! Provider-neutral interaction vocabulary shared by the resurfacing store
//! (lib) and the interaction adapters (comms-side).
//!
//! The full interaction registry — including the comms evidence-backed
//! adapter and the `ChannelAssistStore` constructor — stayed in
//! `magician-comms`'s `resurfacing::interaction` module. These three types
//! are the portion the lib's persisted surfaced-card rows need, so they live
//! here and the comms module re-exports them (plan workstream 3.0).

use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResurfacingActionKind {
    ViewDetails,
    OpenSource,
    ShowOriginal,
    AskPresto,
    CreateTask,
    CreateReminder,
    Share,
    SaveToMemory,
    SummarizeDeeper,
}

impl ResurfacingActionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ViewDetails => "view_details",
            Self::OpenSource => "open_source",
            Self::ShowOriginal => "show_original",
            Self::AskPresto => "ask_presto",
            Self::CreateTask => "create_task",
            Self::CreateReminder => "create_reminder",
            Self::Share => "share",
            Self::SaveToMemory => "save_to_memory",
            Self::SummarizeDeeper => "summarize_deeper",
        }
    }

    /// Whether this action can be performed without a resurfacing source.
    ///
    /// A reminder is built entirely from the submitted title, note, and time —
    /// it never reads the candidate — so it is reachable from any lane. Kinds
    /// that consult the interaction registry or the candidate body are not.
    ///
    /// Moved with the type from the comms crate's actions module (plan
    /// workstream 3.0); the comms action service calls it unchanged.
    pub const fn is_source_independent(self) -> bool {
        matches!(self, Self::CreateReminder)
    }
}

impl FromStr for ResurfacingActionKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "view_details" => Ok(Self::ViewDetails),
            "open_source" => Ok(Self::OpenSource),
            "show_original" => Ok(Self::ShowOriginal),
            "ask_presto" => Ok(Self::AskPresto),
            "create_task" => Ok(Self::CreateTask),
            "create_reminder" => Ok(Self::CreateReminder),
            "share" => Ok(Self::Share),
            "save_to_memory" => Ok(Self::SaveToMemory),
            "summarize_deeper" => Ok(Self::SummarizeDeeper),
            other => anyhow::bail!("unknown resurfacing action kind: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResurfacingRecommendationSource {
    Curator,
    Deterministic,
}

impl ResurfacingRecommendationSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Curator => "curator",
            Self::Deterministic => "deterministic",
        }
    }
}

impl FromStr for ResurfacingRecommendationSource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "curator" => Ok(Self::Curator),
            "deterministic" => Ok(Self::Deterministic),
            other => anyhow::bail!("unknown resurfacing recommendation source: {other}"),
        }
    }
}

/// Optional guidance for one useful operation. It contains no action input;
/// every executable payload is still constructed and validated by the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResurfacingRecommendation {
    pub kind: ResurfacingActionKind,
    pub label: String,
    pub rationale: String,
    pub confidence: f32,
    pub content_revision: Option<String>,
    pub source: ResurfacingRecommendationSource,
}

#[cfg(test)]
mod relocation_tests {
    //! Plan workstream 3.0: this vocabulary moved lib-side from
    //! `magician-comms`'s interaction module unchanged. These tests pin the
    //! wire strings and serde shape so the relocation cannot drift behavior.

    use super::*;

    #[test]
    fn action_kind_roundtrips_through_wire_strings() {
        let cases = [
            (ResurfacingActionKind::ViewDetails, "view_details"),
            (ResurfacingActionKind::OpenSource, "open_source"),
            (ResurfacingActionKind::ShowOriginal, "show_original"),
            (ResurfacingActionKind::AskPresto, "ask_presto"),
            (ResurfacingActionKind::CreateTask, "create_task"),
            (ResurfacingActionKind::CreateReminder, "create_reminder"),
            (ResurfacingActionKind::Share, "share"),
            (ResurfacingActionKind::SaveToMemory, "save_to_memory"),
            (ResurfacingActionKind::SummarizeDeeper, "summarize_deeper"),
        ];
        for (kind, wire) in cases {
            assert_eq!(kind.as_str(), wire);
            assert_eq!(
                ResurfacingActionKind::from_str(wire).unwrap(),
                kind,
                "{wire}"
            );
        }
        // Parsing trims, matching the comms-side behavior it replaced.
        assert_eq!(
            ResurfacingActionKind::from_str("  share  ").unwrap(),
            ResurfacingActionKind::Share
        );
        assert!(ResurfacingActionKind::from_str("transmogrify").is_err());
    }

    #[test]
    fn recommendation_source_roundtrips_and_rejects_unknown() {
        assert_eq!(ResurfacingRecommendationSource::Curator.as_str(), "curator");
        assert_eq!(
            ResurfacingRecommendationSource::Deterministic.as_str(),
            "deterministic"
        );
        assert_eq!(
            ResurfacingRecommendationSource::from_str("curator").unwrap(),
            ResurfacingRecommendationSource::Curator
        );
        assert!(ResurfacingRecommendationSource::from_str("oracle").is_err());
    }

    #[test]
    fn recommendation_serde_shape_is_unchanged() {
        let recommendation = ResurfacingRecommendation {
            kind: ResurfacingActionKind::SummarizeDeeper,
            label: "Summarize deeper".to_string(),
            rationale: "Two new messages joined the thread.".to_string(),
            confidence: 0.72,
            content_revision: Some("distill:9".to_string()),
            source: ResurfacingRecommendationSource::Curator,
        };
        let encoded = serde_json::to_string(&recommendation).unwrap();
        let expected = concat!(
            "{\"kind\":\"summarize_deeper\",\"label\":\"Summarize deeper\",",
            "\"rationale\":\"Two new messages joined the thread.\",",
            "\"confidence\":0.72,\"content_revision\":\"distill:9\",",
            "\"source\":\"curator\"}"
        );
        assert_eq!(encoded, expected);
        assert_eq!(
            serde_json::from_str::<ResurfacingRecommendation>(&encoded).unwrap(),
            recommendation
        );
    }
}
