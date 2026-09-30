CREATE TABLE IF NOT EXISTS feed_items (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    item_type TEXT NOT NULL,
    task_id TEXT NULL,
    ui_thread_id TEXT NULL,
    agent_id TEXT NULL,
    title TEXT NOT NULL,
    summary TEXT NULL,
    status TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    actions_json JSON NOT NULL,
    metadata_json JSON NOT NULL,
    attention_lane TEXT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_feed_scope_updated
    ON feed_items (principal, workspace, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_updated_keyset
    ON feed_items (principal, workspace, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_status_updated
    ON feed_items (principal, workspace, status, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_status_updated_keyset
    ON feed_items (principal, workspace, status, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_scope_thread_updated
    ON feed_items (principal, workspace, ui_thread_id, updated_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_feed_scope_agent_updated
    ON feed_items (principal, workspace, agent_id, updated_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_feed_scope_task_updated
    ON feed_items (principal, workspace, task_id, updated_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_feed_scope_attention_lane_updated
    ON feed_items (principal, workspace, attention_lane, updated_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS feed_attention_items (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    lane TEXT NOT NULL,
    projection_source TEXT NOT NULL,
    projection_group TEXT NOT NULL,
    item_type TEXT NOT NULL,
    task_id TEXT NULL,
    ui_thread_id TEXT NULL,
    agent_id TEXT NULL,
    title TEXT NOT NULL,
    summary TEXT NULL,
    status TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    actions_json JSON NOT NULL,
    metadata_json JSON NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_feed_attention_scope_lane_updated
    ON feed_attention_items (principal, workspace, lane, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_feed_attention_scope_projection_group
    ON feed_attention_items (principal, workspace, projection_source, projection_group);

CREATE TABLE IF NOT EXISTS feed_attention_projection_groups (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    projection_source TEXT NOT NULL,
    projection_group TEXT NOT NULL,
    source_generation BIGINT NOT NULL,
    recorded_at BIGINT NOT NULL,
    PRIMARY KEY (principal, workspace, projection_source, projection_group)
);

CREATE TABLE IF NOT EXISTS feed_attention_dismissals (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    dismissed BOOLEAN NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
