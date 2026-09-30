//! What the retired Town Square engine's boundary contract became.
//!
//! This file used to pin the autonomous social worker: that its tick could not
//! enter task or execution dispatch, that its scope selection and LLM fan-out
//! were bounded, that only an `engage` decision counted as a gate pass. Queue
//! item 6, slice 4 deleted the worker, the `publish_social_post` compiled tool
//! and the first-party HTTP surface, so every one of those assertions now
//! describes code that does not exist.
//!
//! The file is kept rather than deleted because a retirement needs a pin too.
//! Deleting it would leave nothing to fail if the worker, the tool or the
//! retired router operations came back — and a stale grant or a reverted merge
//! is exactly how they would.

use std::path::Path;

const AGENT_RESOURCES: &str = include_str!("../src/magician_v2/execution/agent_resources.rs");
const COMPILED_PROVIDERS: &str = include_str!("../src/magician_v2/execution/compiled_providers.rs");
const OPERATION_ROUTER: &str =
    include_str!("../src/magician_v2/query_analysis/operation_llm_router.rs");
const SOCIAL_STORE: &str = include_str!("../src/magician_v2/social/store.rs");

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("magician crate sits inside the workspace")
}

#[test]
fn the_autonomous_worker_and_its_http_surface_are_gone() {
    for retired in [
        "magician/src/magician_v2/social/worker.rs",
        "magician/src/magician_v2/social/api.rs",
        "magician/src/magician_v2/execution/compiled_handlers/publish_social_post.rs",
        "magician/src/magician_v2/execution/embedded_pack_defs/publish_social_post.yaml",
    ] {
        assert!(
            !repo_root().join(retired).exists(),
            "`{retired}` retired with the first-party engine; the package's \
             `ambient_turn` behavior replaces it"
        );
    }
}

#[test]
fn the_social_publish_tool_cannot_be_resolved_again() {
    // A stale grant naming this tool must fall through, not quietly bind. The
    // tool wrote the first-party store, which is no longer the corpus.
    assert!(
        !COMPILED_PROVIDERS.contains("compiled_handlers::publish_social_post::handle"),
        "the retired social publish handler must not be registered"
    );
    assert!(
        !COMPILED_PROVIDERS.contains("embedded_pack_defs/publish_social_post.yaml"),
        "the retired social publish pack must not be embedded"
    );
}

#[test]
fn the_retired_router_operations_are_not_core_variants() {
    // Town Square's gate and compose belong to the package now, declared as
    // `app:` operations its manifest budgets and the operator can narrow. A
    // core variant would be a second routing identity for the same work.
    assert!(
        !OPERATION_ROUTER.contains("LLMOperation::SocialGate"),
        "`SocialGate` retired in favour of the package's `app:` operation"
    );
    assert!(
        !OPERATION_ROUTER.contains("LLMOperation::SocialCompose"),
        "`SocialCompose` retired in favour of the package's `app:` operation"
    );
}

#[test]
fn the_social_store_no_longer_carries_a_spend_ledger() {
    // Per-agent daily token budgets are the resource authority's now. The
    // tables are deliberately NOT dropped -- an existing store keeps its rows --
    // but nothing may create, read or prune them, because a store created after
    // the retirement does not have them and a `SELECT` would fail rather than
    // find zero rows.
    assert!(!SOCIAL_STORE.contains("CREATE TABLE IF NOT EXISTS spend_log"));
    assert!(!SOCIAL_STORE.contains("CREATE TABLE IF NOT EXISTS social_budget"));
    assert!(!SOCIAL_STORE.contains("fn deduct_budget"));
    assert!(!SOCIAL_STORE.contains("fn settle_budget"));
    assert!(!SOCIAL_STORE.contains("fn log_spend"));
    assert!(
        !SOCIAL_STORE.contains("DELETE FROM spend_log"),
        "retention must not query a table this store no longer creates"
    );
}

#[test]
fn live_reload_cannot_split_boot_bound_social_authority() {
    // Unchanged and still live: the boot-bound social config seam survives the
    // engine, because `max_post_chars` is still enforced on the write path.
    assert!(AGENT_RESOURCES.contains("config_with_boot_bound_social"));
    assert!(AGENT_RESOURCES.contains("incoming.social = current.social.clone()"));
}

#[test]
fn the_social_store_survives_as_a_migration_source() {
    // The store is not the authority any more, but it is not gone either: the
    // one-shot corpus migration reads it, and it stays intact afterwards so a
    // bad migration is recoverable rather than terminal.
    assert!(repo_root()
        .join("magician/src/magician_v2/social/store.rs")
        .exists());
    assert!(
        SOCIAL_STORE.contains("pub fn export_corpus"),
        "the migration reads the corpus through this store"
    );
}
