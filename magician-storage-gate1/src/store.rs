use async_trait::async_trait;
use magician_storage::StorageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub principal: String,
    pub workspace: String,
    pub task_id: String,
    pub revision: i64,
    pub idempotency_key: String,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatPage {
    pub seqs: Vec<i64>,
    pub bodies: Vec<String>,
}

#[async_trait]
pub trait Gate1Store: Send + Sync {
    fn name(&self) -> &'static str;
    async fn migrate(&self) -> Result<(), StorageError>;
    async fn migrate_v2_add_task_status(&self) -> Result<(), StorageError>;
    async fn create_task(&self, task: &TaskRow) -> Result<TaskRow, StorageError>;
    async fn cas_task(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        expected: i64,
        payload: &str,
    ) -> Result<TaskRow, StorageError>;
    async fn get_task(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Result<Option<TaskRow>, StorageError>;
    async fn create_session(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> Result<(), StorageError>;
    async fn append_message(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
        body: &str,
    ) -> Result<i64, StorageError>;
    async fn page_messages(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<ChatPage, StorageError>;
    async fn put_attention(
        &self,
        principal: &str,
        workspace: &str,
        item_id: &str,
        score: f64,
        retained_until: i64,
    ) -> Result<(), StorageError>;
    async fn retained_attention(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<Vec<String>, StorageError>;
    async fn enqueue_outbox(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        payload: &str,
    ) -> Result<(), StorageError>;
    async fn claim_outbox(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        owner: &str,
        now: i64,
        ttl: i64,
    ) -> Result<i64, StorageError>;
    async fn acquire_lease(
        &self,
        resource: &str,
        owner: &str,
        now: i64,
        ttl: i64,
    ) -> Result<i64, StorageError>;
    async fn renew_lease(
        &self,
        resource: &str,
        owner: &str,
        expected: i64,
        now: i64,
        ttl: i64,
    ) -> Result<i64, StorageError>;
    async fn uncommitted_insert_dropped(&self) -> Result<bool, StorageError>;
    async fn committed_insert_survives(&self) -> Result<bool, StorageError>;
    async fn backup_restore_round_trip(&self) -> Result<bool, StorageError>;
}
