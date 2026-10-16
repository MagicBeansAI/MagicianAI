//! V2 Storage System - Clean, purpose-built for MagicianV2

pub mod file;
/// Rebuildable SQLite index over the file store, so a list page costs a seek
/// instead of a walk of every record. Files remain canonical.
pub mod list_index;
pub mod models;
pub mod r#trait;

// Task/task-execution models and scheduling helpers.
pub mod task_models;
pub mod task_scheduler;

pub use file::FileV2Store;
pub use list_index::{
    epoch_millis_from_rfc3339, row_follows_cursor, row_follows_cursor_with, DiscardReason,
    ListCursor, ListEntry, ListIndex, ListIndexOpen, ListKind, ListPage, ListPageQuery, ListScope,
    ListTieBreak, MonitorPage, MonitorPageQuery, RebuildReport, ReindexOutcome,
    LIST_INDEX_FILE_NAME, LIST_INDEX_SCHEMA_VERSION,
};
pub use models::{
    CreateExecutionParams, ExecutionEntryMode, ExecutionIndex, ExecutionRun, ExecutionRunDocument,
    ExecutionSummary, PaginatedResult, PaginationInfo, PaginationParams, StrategyAttempt,
    TurnDirection, V2Slot, V2SlotStatus, V2Turn, WaitingState,
};
pub use r#trait::{V2ConversationStore, V2StorageError};

pub use task_models::{
    task_execution_artifact_chain_id, Task, TaskCreatedBy, TaskExecutionLinkedInputRecord,
    TaskExecutionRecord, TaskExecutionSnapshot, TaskExecutionStepRecord, TaskPriority,
    TaskSchedule, TaskScheduleKind, TaskStatus, TaskSummary, TaskTag,
};
pub use task_scheduler::{TaskSchedulerEntry, TaskSchedulerService};
