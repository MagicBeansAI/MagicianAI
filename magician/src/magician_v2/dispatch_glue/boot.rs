//! Construct `LlmDispatchQueue` at boot from `magician-config.yaml`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use magicllm::dispatch::{DispatchConfig, DispatchRouter, TaskLedgerSink, TaskStateView};
use magicllm::LlmDispatchQueue;
use tracing::info;

use super::orphan_sweep::run_startup_orphan_sweep;
use super::task_ledger_sink::ArtifactV2TaskLedgerSink;
use super::task_state_view::ArtifactV2TaskStateView;
use crate::magician_v2::artifact_v2::service::ArtifactV2Service;

/// Bring up the dispatch queue: build glue impls, start the queue, then run
/// orphan recovery in bounded background maintenance. The queue must not wait
/// for historical ledger scans before the HTTP server can bind.
///
/// `router` is any `DispatchRouter` impl — typically `ConfiguredRouter` in
/// production wiring.
pub async fn start_dispatch_queue(
    router: Arc<dyn DispatchRouter>,
    artifact_service: Arc<ArtifactV2Service>,
    data_root: PathBuf,
    config: DispatchConfig,
) -> (Arc<LlmDispatchQueue>, Arc<ArtifactV2TaskStateView>) {
    let task_state_view = ArtifactV2TaskStateView::new(artifact_service.clone());
    let ledger_sink = ArtifactV2TaskLedgerSink::new(artifact_service.clone(), data_root.clone());

    let sweep_sink: Arc<dyn TaskLedgerSink> = ledger_sink.clone();

    let view: Arc<dyn TaskStateView> = task_state_view.clone();
    let sink: Arc<dyn TaskLedgerSink> = ledger_sink;
    let queue = LlmDispatchQueue::start(router, view, sink, config);
    info!("dispatch queue started; scheduling bounded orphan sweep in background");
    tokio::spawn(async move {
        match tokio::time::timeout(
            Duration::from_secs(15),
            run_startup_orphan_sweep(data_root.clone(), sweep_sink),
        )
        .await
        {
            Ok(report) => {
                info!(
                    orphans = report.total_orphans,
                    files_scanned = report.files_scanned,
                    files_skipped_old = report.files_skipped_old,
                    files_failed_metadata = report.files_failed_metadata,
                    files_timed_out = report.files_timed_out,
                    "dispatch queue startup orphan sweep done"
                );
            },
            Err(_) => {
                info!(
                    ledger_root = %data_root.join("llm_dispatch").display(),
                    "dispatch queue startup orphan sweep timed out; startup continues and recovery will retry on next boot"
                );
            },
        }
    });
    (queue, task_state_view)
}
