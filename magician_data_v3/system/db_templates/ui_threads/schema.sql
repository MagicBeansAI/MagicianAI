CREATE TABLE IF NOT EXISTS ui_threads (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    name TEXT NOT NULL,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    sort_order BIGINT NOT NULL DEFAULT 0,
    memory_summary TEXT NULL,
    memory_updated_at BIGINT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    display_mode TEXT NOT NULL DEFAULT 'chat',
    plan_mode BOOLEAN NOT NULL DEFAULT FALSE,
    history_lane TEXT NOT NULL DEFAULT 'personal',
    deleted_at BIGINT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_ui_threads_scope_order
    ON ui_threads (principal, workspace, deleted_at, archived, sort_order ASC, updated_at DESC, id ASC);
CREATE INDEX IF NOT EXISTS idx_ui_threads_scope_history
    ON ui_threads (principal, workspace, deleted_at, history_lane, updated_at DESC, id ASC);
