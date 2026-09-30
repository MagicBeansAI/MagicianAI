use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacklogPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacklogStatus {
    Proposed,
    Promoted,
    Delivered,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacklogDeliveryDisposition {
    Accepted,
    Rework,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklogDeliveryReview {
    pub task_id: String,
    pub disposition: BacklogDeliveryDisposition,
    pub summary: String,
    pub reviewed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacklogItem {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub source_agent: String,
    pub title: String,
    pub description: String,
    pub priority: BacklogPriority,
    pub status: BacklogStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub promoted_task_id: Option<String>,
    /// Every task created from this item. `promoted_task_id` remains the current
    /// attempt while this list preserves the audit trail across rework cycles.
    #[serde(default)]
    pub promotion_task_ids: Vec<String>,
    /// A promoted task does not close the backlog item by itself. Its owner must
    /// accept the delivered outcome or return the item to `Proposed` for rework.
    #[serde(default)]
    pub delivery_review: Option<BacklogDeliveryReview>,
    /// Cross-owner promotion request: a sibling officer asked to have this item
    /// promoted to `requested_owner_agent` (an agent OUTSIDE the requester's harness
    /// scope). The owner whose scope contains that agent promotes it in their own
    /// scope; no cross-scope task is created. `#[serde(default)]` = back-compat for
    /// records written before this field existed.
    #[serde(default)]
    pub requested_owner_agent: Option<String>,
    /// The officer (agent id) that made the cross-owner request.
    #[serde(default)]
    pub requested_by_officer: Option<String>,
}

impl BacklogItem {
    pub fn new(
        principal: &str,
        workspace: &str,
        source_agent: &str,
        title: &str,
        description: &str,
        priority: BacklogPriority,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Self::id(source_agent, title),
            principal: principal.into(),
            workspace: workspace.into(),
            source_agent: source_agent.into(),
            title: title.into(),
            description: description.into(),
            priority,
            status: BacklogStatus::Proposed,
            created_at: now,
            updated_at: now,
            promoted_task_id: None,
            promotion_task_ids: Vec::new(),
            delivery_review: None,
            requested_owner_agent: None,
            requested_by_officer: None,
        }
    }

    /// Deterministic dedupe key: `bk_` + the first 12 hex chars of
    /// `sha256(source_agent \0 title)`. Two proposals with the same
    /// (source_agent, title) collapse onto the same backlog item.
    pub fn id(source_agent: &str, title: &str) -> String {
        let mut h = Sha256::new();
        h.update(source_agent.as_bytes());
        h.update(b"\0");
        h.update(title.as_bytes());
        let digest = format!("{:x}", h.finalize());
        format!("bk_{}", &digest[..12])
    }

    /// Snake_case wire form of `status`, matching the serde `rename_all`
    /// representation. A later API task filters backlog items by this string.
    pub fn status_wire(&self) -> &'static str {
        match self.status {
            BacklogStatus::Proposed => "proposed",
            BacklogStatus::Promoted => "promoted",
            BacklogStatus::Delivered => "delivered",
            BacklogStatus::Dismissed => "dismissed",
        }
    }

    /// Snake_case wire form of `priority`, matching the serde `rename_all`
    /// representation.
    pub fn priority_wire(&self) -> &'static str {
        match self.priority {
            BacklogPriority::High => "high",
            BacklogPriority::Medium => "medium",
            BacklogPriority::Low => "low",
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_dedupes_by_source_agent_and_title() {
        let a = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship weekly digest email",
            "some description",
            BacklogPriority::Medium,
        );
        let b = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship weekly digest email",
            "a DIFFERENT description",
            BacklogPriority::High,
        );
        assert_eq!(a.id, b.id, "same source_agent+title → same id (dedupe key)");
        let diff_title = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship monthly digest email",
            "x",
            BacklogPriority::Medium,
        );
        assert_ne!(a.id, diff_title.id, "different title → different id");
        let diff_agent = BacklogItem::new(
            "anonymous",
            "default",
            "sales",
            "Ship weekly digest email",
            "x",
            BacklogPriority::Medium,
        );
        assert_ne!(a.id, diff_agent.id, "different source_agent → different id");
        assert!(a.id.starts_with("bk_"));
        assert_eq!(a.status, BacklogStatus::Proposed);
    }

    #[test]
    fn status_and_priority_wire_match_serde_snake_case() {
        let mut item = BacklogItem::new("p", "w", "growth", "t", "d", BacklogPriority::High);
        assert_eq!(item.status_wire(), "proposed");
        assert_eq!(item.priority_wire(), "high");
        item.status = BacklogStatus::Promoted;
        item.priority = BacklogPriority::Low;
        assert_eq!(item.status_wire(), "promoted");
        assert_eq!(item.priority_wire(), "low");
        item.status = BacklogStatus::Dismissed;
        item.priority = BacklogPriority::Medium;
        assert_eq!(item.status_wire(), "dismissed");
        assert_eq!(item.priority_wire(), "medium");
        item.status = BacklogStatus::Delivered;
        assert_eq!(item.status_wire(), "delivered");
    }
}
