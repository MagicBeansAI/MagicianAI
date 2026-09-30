pub mod action_adapter;
pub mod agent_learnings_projection;
pub mod materializer;
pub mod store;
pub mod task_projection;
pub mod types;
pub mod v3_projection;

pub const ROUTINE_RESULT_PUBLISHED_EVENT_TYPE: &str = "routine.result_published";

pub use action_adapter::{
    thinking_map_node_source_marker, today_action_adapter, TodayActionPlan, TodayActionSubject,
    TodayTaskActionPlan, TodayThinkingMapPromotionPlan, THINKING_MAP_ACTION_SOURCE_KIND,
};
pub use materializer::FeedMaterializer;
pub use store::{
    AttentionProjectionReconcile, FeedAttentionLane, FeedAttentionPage, FeedAttentionPageQuery,
    FeedCounts, FeedPage, FeedPageCursor, FeedQuery, FeedStore,
};
pub use task_projection::TaskCrudFeedProjectionAdapter;
pub use types::{FeedAction, FeedItem, FeedItemPatch, FeedItemStatus, FeedItemType};
pub use v3_projection::V3FeedProjectionAdapter;
