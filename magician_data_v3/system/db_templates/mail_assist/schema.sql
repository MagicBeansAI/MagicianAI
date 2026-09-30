CREATE TABLE IF NOT EXISTS mail_threads (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    account_email TEXT NULL,
    lane TEXT NOT NULL DEFAULT 'user_assist',
    subject TEXT NULL,
    latest_summary TEXT NULL,
    latest_from_name TEXT NULL,
    latest_from_address TEXT NULL,
    recipient_domains_json JSON NOT NULL,
    label_ids_json JSON NOT NULL,
    message_count BIGINT NOT NULL,
    last_message_at BIGINT NULL,
    provider_cursor TEXT NULL,
    sensitive_suppressed BOOLEAN NOT NULL,
    origin TEXT NOT NULL,
    first_observed_at BIGINT NOT NULL,
    last_observed_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, provider, account_alias, thread_id)
);
CREATE INDEX IF NOT EXISTS idx_mail_threads_scope_last_message
    ON mail_threads (principal, workspace, provider, account_alias, last_message_at DESC);
CREATE TABLE IF NOT EXISTS mail_messages (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    message_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    account_email TEXT NULL,
    provider_cursor TEXT NULL,
    label_ids_json JSON NOT NULL,
    subject TEXT NULL,
    from_name TEXT NULL,
    from_address TEXT NULL,
    to_domains_json JSON NOT NULL,
    cc_domains_json JSON NOT NULL,
    internal_date BIGINT NOT NULL,
    observed_at BIGINT NOT NULL,
    direction TEXT NULL,
    summary TEXT NULL,
    intent TEXT NULL,
    needs_reply_hint BOOLEAN NOT NULL DEFAULT FALSE,
    follow_up_hint_json JSON NULL,
    distill_evidence_message_ids_json JSON NULL,
    distill_brief_json JSON NULL,
    distill_contract_version INTEGER NULL,
    distilled_at BIGINT NULL,
    distill_revision BIGINT NULL,
    distill_backfill_attempts INTEGER NOT NULL DEFAULT 0,
    distill_backfill_next_retry_at BIGINT NULL,
    distill_backfill_last_error TEXT NULL,
    distill_state TEXT NOT NULL DEFAULT 'pending',
    distill_attempts INTEGER NOT NULL DEFAULT 0,
    classify_attempts INTEGER NOT NULL DEFAULT 0,
    classify_next_retry_at BIGINT NULL,
    classify_last_error TEXT NULL,
    classify_failed_at BIGINT NULL,
    sensitive_suppressed BOOLEAN NOT NULL,
    origin TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, provider, account_alias, message_id)
);
CREATE INDEX IF NOT EXISTS idx_mail_messages_scope_thread
    ON mail_messages (principal, workspace, provider, account_alias, thread_id,
                      internal_date DESC);
CREATE INDEX IF NOT EXISTS idx_mail_messages_reconcile
    ON mail_messages (principal, workspace, provider, account_alias, thread_id,
                      distill_state, sensitive_suppressed, internal_date, message_id);
CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_queue
    ON mail_messages (principal, workspace, distill_state, internal_date);
CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_revision
    ON mail_messages (principal, workspace, distill_revision);
CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_backfill
    ON mail_messages (principal, workspace, distill_state, sensitive_suppressed,
                      distill_contract_version, internal_date);
CREATE TABLE IF NOT EXISTS mail_distill_revision_counters (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    last_revision BIGINT NOT NULL,
    PRIMARY KEY (principal, workspace)
);
CREATE TABLE IF NOT EXISTS mail_annotations (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    lane TEXT NOT NULL DEFAULT 'user_assist',
    state TEXT NOT NULL,
    label TEXT NULL,
    confidence DOUBLE NULL,
    reason TEXT NULL,
    evidence_refs_json JSON NOT NULL,
    evidence_message_id TEXT NULL,
    evidence_message_at BIGINT NULL,
    classification_input_revision BIGINT NULL,
    semantic_features_json JSON NULL,
    proposed_action_json JSON NULL,
    attention_lane TEXT NOT NULL DEFAULT 'follow_up',
    provenance TEXT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_mail_annotations_scope_thread
    ON mail_annotations (principal, workspace, provider, account_alias, thread_id,
                         updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_mail_annotations_reconcile
    ON mail_annotations (principal, workspace, state, updated_at, id);
CREATE INDEX IF NOT EXISTS idx_mail_annotations_today
    ON mail_annotations (principal, workspace, attention_lane, state,
                         created_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS mail_assist_events (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    annotation_id TEXT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    thread_id TEXT NULL,
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL,
    from_state TEXT NULL,
    to_state TEXT NULL,
    detail_json JSON NULL,
    created_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_mail_events_scope_annotation
    ON mail_assist_events (principal, workspace, annotation_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_mail_events_annotation_state
    ON mail_assist_events (principal, workspace, annotation_id, to_state, created_at);
CREATE TABLE IF NOT EXISTS mail_annotation_action_claims (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    annotation_id TEXT NOT NULL,
    action TEXT NOT NULL,
    claim_id TEXT NOT NULL,
    task_id TEXT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, annotation_id, action)
);
CREATE TABLE IF NOT EXISTS mail_sync_watermarks (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    last_internal_date BIGINT NULL,
    provider_cursor TEXT NULL,
    last_synced_at BIGINT NOT NULL,
    last_error TEXT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, provider, account_alias)
);
CREATE TABLE IF NOT EXISTS channel_writing_preferences (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    scope_value TEXT NOT NULL,
    statement TEXT NOT NULL,
    status TEXT NOT NULL,
    source_annotation_id TEXT NULL,
    evidence_count BIGINT NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, id),
    UNIQUE (principal, workspace, provider, account_alias, scope_kind, scope_value, statement)
);
CREATE INDEX IF NOT EXISTS idx_channel_writing_preferences_scope
    ON channel_writing_preferences (
        principal, workspace, provider, account_alias, scope_kind, scope_value, status
    );
CREATE TABLE IF NOT EXISTS channel_action_drafts (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    compose_id TEXT NOT NULL,
    annotation_id TEXT NOT NULL,
    action_id TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, compose_id)
);
CREATE INDEX IF NOT EXISTS idx_channel_action_drafts_annotation
    ON channel_action_drafts (principal, workspace, annotation_id, action_id, created_at DESC);
CREATE TABLE IF NOT EXISTS mail_assist_meta (
    meta_key TEXT NOT NULL,
    meta_value TEXT NOT NULL,
    PRIMARY KEY (meta_key)
);
