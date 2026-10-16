//! Decision Gate 3 — accepted performance and capacity budgets.
//!
//! Most lines here are the *remote-durable* acceptance numbers from
//! ADR 2026-09-01. Lines whose name ends in `Local` are the separately
//! accepted allowance for a local filesystem adapter, whose durable write
//! costs a chain of full-device flushes rather than one network round trip.
//!
//! Wall-clock lines are enforced only in an uncontended lane; see
//! [`latency_budgets_enforced`].

/// Closed by Task 18A. Cutover may proceed only when this is true and the
/// other Task 19 preconditions hold.
pub const GATE3_CLOSED: bool = true;
pub const GATE3_OWNER: &str = "Magician runtime and storage owners";
pub const GATE3_ACCEPTED_AT: &str = "2026-09-01";
pub const ONLINE_MIGRATION_REQUIRED: bool = false;

// Both gates are compile-time facts, so assert them at compile time. Reopening
// either one stops the workspace building rather than turning one test red,
// which is the stronger signal and the reason the bare runtime `assert!` these
// replace was worth removing rather than silencing.
const _: () = assert!(
    GATE3_CLOSED,
    "Gate 3 is accepted; reopening it invalidates every budget below"
);
const _: () = assert!(
    !ONLINE_MIGRATION_REQUIRED,
    "offline cutover is accepted, so section 15.3 online migration stays optional"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetLine {
    RepoReadP50Ms,
    RepoReadP95Ms,
    RepoReadP99Ms,
    RepoMutP50Ms,
    RepoMutP95Ms,
    RepoMutP99Ms,
    ChatAppendP95Ms,
    ChatPageP95Ms,
    TaskListP95Ms,
    ObjectSmallFirstByteP95Ms,
    /// Remote-durable 64 KiB complete p95: one network round trip.
    ObjectSmallCompleteP95Ms,
    /// Local-durable 64 KiB complete p95. A local put publishes through a
    /// pending file and two sidecar generations, so it pays several
    /// full-device flushes where the remote path pays one round trip.
    ObjectSmallCompleteLocalP95Ms,
    ObjectMediumFirstByteP95Ms,
    ObjectMediumCompleteP95Ms,
    ObjectLargeCompleteP95Ms,
    MaxInMemoryTransferBytes,
    ParquetFlushP95Ms,
    ManifestCommitP95Ms,
    DatasetQueryP95Ms,
    OutboxBacklog,
    DatasetBacklog,
    LeaseTtlSecs,
    LeaseRenewalSecs,
    LeaseLossDetectionSecs,
    ScratchQuotaBytes,
    ScratchCleanupOnOpen,
    IndexRebuildP95Ms,
    IndexStaleFallbackSecs,
    MigrationThroughputBytesPerSec,
    WriteFreezeSecs,
    BackupRpoSecs,
    RestoreRtoSecs,
    MonthlyStorageBytes,
    MonthlyOperations,
    MonthlyEgressBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate3Budgets {
    pub repo_read_p50_ms: u64,
    pub repo_read_p95_ms: u64,
    pub repo_read_p99_ms: u64,
    pub repo_mut_p50_ms: u64,
    pub repo_mut_p95_ms: u64,
    pub repo_mut_p99_ms: u64,
    pub chat_append_p95_ms: u64,
    pub chat_page_p95_ms: u64,
    pub task_list_p95_ms: u64,
    pub object_small_first_byte_p95_ms: u64,
    pub object_small_complete_p95_ms: u64,
    pub object_small_complete_local_p95_ms: u64,
    pub object_medium_first_byte_p95_ms: u64,
    pub object_medium_complete_p95_ms: u64,
    pub object_large_complete_p95_ms: u64,
    pub max_in_memory_transfer_bytes: u64,
    pub parquet_flush_p95_ms: u64,
    pub manifest_commit_p95_ms: u64,
    pub dataset_query_p95_ms: u64,
    pub outbox_backlog: u64,
    pub dataset_backlog: u64,
    pub lease_ttl_secs: u64,
    pub lease_renewal_secs: u64,
    pub lease_loss_detection_secs: u64,
    pub scratch_quota_bytes: u64,
    pub scratch_cleanup_on_open: u64,
    pub index_rebuild_p95_ms: u64,
    pub index_stale_fallback_secs: u64,
    pub migration_throughput_bytes_per_sec: u64,
    pub write_freeze_secs: u64,
    pub backup_rpo_secs: u64,
    pub restore_rto_secs: u64,
    pub monthly_storage_bytes: u64,
    pub monthly_operations: u64,
    pub monthly_egress_bytes: u64,
}

pub fn accepted_budgets() -> Gate3Budgets {
    Gate3Budgets {
        repo_read_p50_ms: 20,
        repo_read_p95_ms: 100,
        repo_read_p99_ms: 250,
        repo_mut_p50_ms: 50,
        repo_mut_p95_ms: 200,
        repo_mut_p99_ms: 500,
        chat_append_p95_ms: 200,
        chat_page_p95_ms: 200,
        task_list_p95_ms: 250,
        object_small_first_byte_p95_ms: 50,
        object_small_complete_p95_ms: 100,
        object_small_complete_local_p95_ms: 250,
        object_medium_first_byte_p95_ms: 100,
        object_medium_complete_p95_ms: 2_000,
        object_large_complete_p95_ms: 15_000,
        max_in_memory_transfer_bytes: 64 * 1024 * 1024,
        parquet_flush_p95_ms: 2_000,
        manifest_commit_p95_ms: 100,
        dataset_query_p95_ms: 100,
        outbox_backlog: 10_000,
        dataset_backlog: 1_000,
        lease_ttl_secs: 24 * 60 * 60,
        lease_renewal_secs: 60 * 60,
        lease_loss_detection_secs: 60 * 60,
        scratch_quota_bytes: 64 * 1024 * 1024,
        scratch_cleanup_on_open: 1,
        index_rebuild_p95_ms: 30_000,
        index_stale_fallback_secs: 60,
        migration_throughput_bytes_per_sec: 8 * 1024 * 1024,
        write_freeze_secs: 900,
        backup_rpo_secs: 0,
        restore_rto_secs: 900,
        monthly_storage_bytes: 100 * 1024 * 1024 * 1024,
        monthly_operations: 10_000_000,
        monthly_egress_bytes: 50 * 1024 * 1024 * 1024,
    }
}

impl Gate3Budgets {
    pub fn value(self, line: BudgetLine) -> u64 {
        match line {
            BudgetLine::RepoReadP50Ms => self.repo_read_p50_ms,
            BudgetLine::RepoReadP95Ms => self.repo_read_p95_ms,
            BudgetLine::RepoReadP99Ms => self.repo_read_p99_ms,
            BudgetLine::RepoMutP50Ms => self.repo_mut_p50_ms,
            BudgetLine::RepoMutP95Ms => self.repo_mut_p95_ms,
            BudgetLine::RepoMutP99Ms => self.repo_mut_p99_ms,
            BudgetLine::ChatAppendP95Ms => self.chat_append_p95_ms,
            BudgetLine::ChatPageP95Ms => self.chat_page_p95_ms,
            BudgetLine::TaskListP95Ms => self.task_list_p95_ms,
            BudgetLine::ObjectSmallFirstByteP95Ms => self.object_small_first_byte_p95_ms,
            BudgetLine::ObjectSmallCompleteP95Ms => self.object_small_complete_p95_ms,
            BudgetLine::ObjectSmallCompleteLocalP95Ms => self.object_small_complete_local_p95_ms,
            BudgetLine::ObjectMediumFirstByteP95Ms => self.object_medium_first_byte_p95_ms,
            BudgetLine::ObjectMediumCompleteP95Ms => self.object_medium_complete_p95_ms,
            BudgetLine::ObjectLargeCompleteP95Ms => self.object_large_complete_p95_ms,
            BudgetLine::MaxInMemoryTransferBytes => self.max_in_memory_transfer_bytes,
            BudgetLine::ParquetFlushP95Ms => self.parquet_flush_p95_ms,
            BudgetLine::ManifestCommitP95Ms => self.manifest_commit_p95_ms,
            BudgetLine::DatasetQueryP95Ms => self.dataset_query_p95_ms,
            BudgetLine::OutboxBacklog => self.outbox_backlog,
            BudgetLine::DatasetBacklog => self.dataset_backlog,
            BudgetLine::LeaseTtlSecs => self.lease_ttl_secs,
            BudgetLine::LeaseRenewalSecs => self.lease_renewal_secs,
            BudgetLine::LeaseLossDetectionSecs => self.lease_loss_detection_secs,
            BudgetLine::ScratchQuotaBytes => self.scratch_quota_bytes,
            BudgetLine::ScratchCleanupOnOpen => self.scratch_cleanup_on_open,
            BudgetLine::IndexRebuildP95Ms => self.index_rebuild_p95_ms,
            BudgetLine::IndexStaleFallbackSecs => self.index_stale_fallback_secs,
            BudgetLine::MigrationThroughputBytesPerSec => self.migration_throughput_bytes_per_sec,
            BudgetLine::WriteFreezeSecs => self.write_freeze_secs,
            BudgetLine::BackupRpoSecs => self.backup_rpo_secs,
            BudgetLine::RestoreRtoSecs => self.restore_rto_secs,
            BudgetLine::MonthlyStorageBytes => self.monthly_storage_bytes,
            BudgetLine::MonthlyOperations => self.monthly_operations,
            BudgetLine::MonthlyEgressBytes => self.monthly_egress_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetVerdict {
    pub line: BudgetLine,
    pub accepted: u64,
    pub measured: u64,
    pub pass: bool,
}

impl BudgetVerdict {
    pub fn compare(line: BudgetLine, measured: u64) -> Self {
        let accepted = accepted_budgets().value(line);
        Self {
            line,
            accepted,
            measured,
            pass: measured <= accepted,
        }
    }

    pub fn compare_floor(line: BudgetLine, measured: u64) -> Self {
        let accepted = accepted_budgets().value(line);
        Self {
            line,
            accepted,
            measured,
            pass: measured >= accepted,
        }
    }
}

/// Set to `0` to measure wall-clock budget lines without asserting them.
///
/// The workspace test report deliberately saturates the machine with several
/// concurrent test processes, which measures the runner rather than the
/// adapter. That lane sets this to `0`; every other caller enforces.
pub const ENFORCE_LATENCY_ENV: &str = "MAGICIAN_GATE3_ENFORCE_LATENCY";

/// Whether wall-clock budget lines are enforced in this process.
///
/// Enforcing is the default: a line is only waived when
/// [`ENFORCE_LATENCY_ENV`] is explicitly set to `0`.
pub fn latency_budgets_enforced() -> bool {
    match std::env::var(ENFORCE_LATENCY_ENV) {
        Ok(value) => value.trim() != "0",
        Err(_) => true,
    }
}

/// Assert a declared (non-measured) budget line. Always enforced.
#[track_caller]
pub fn assert_budget(line: BudgetLine, measured: u64) {
    let verdict = BudgetVerdict::compare(line, measured);
    assert!(
        verdict.pass,
        "{line:?} measured {} exceeded accepted {}",
        verdict.measured, verdict.accepted
    );
}

/// Assert a wall-clock budget line, unless this lane only records them.
///
/// The measurement is always taken and printed, so a saturated lane still
/// exercises the code path and reports the number it saw.
#[track_caller]
pub fn assert_latency_budget(line: BudgetLine, measured: u64) {
    let verdict = BudgetVerdict::compare(line, measured);
    if latency_budgets_enforced() {
        assert!(
            verdict.pass,
            "{line:?} measured {} exceeded accepted {}",
            verdict.measured, verdict.accepted
        );
    } else {
        println!(
            "gate3 latency (not enforced): {line:?} measured {} accepted {}",
            verdict.measured, verdict.accepted
        );
    }
}

pub fn percentile_ms(samples: &[u128], p: u8) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let idx = ((p as usize) * (sorted.len() - 1)) / 100;
    (sorted[idx] / 1_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate3_fails_when_a_line_exceeds_its_accepted_number() {
        assert!(!BudgetVerdict::compare(BudgetLine::RepoReadP95Ms, 101).pass);
        assert!(BudgetVerdict::compare(BudgetLine::RepoReadP95Ms, 100).pass);
        assert!(!BudgetVerdict::compare_floor(BudgetLine::MigrationThroughputBytesPerSec, 1).pass);
    }

    #[test]
    fn the_local_durable_small_object_line_is_looser_than_the_remote_one() {
        let budgets = accepted_budgets();
        assert_eq!(budgets.object_small_complete_p95_ms, 100);
        assert_eq!(budgets.object_small_complete_local_p95_ms, 250);
        assert!(budgets.object_small_complete_local_p95_ms > budgets.object_small_complete_p95_ms);
    }

    #[test]
    fn latency_budgets_are_enforced_unless_the_lane_opts_out() {
        assert_eq!(ENFORCE_LATENCY_ENV, "MAGICIAN_GATE3_ENFORCE_LATENCY");
        // Enforcing is the default, so a waived lane is always a deliberate
        // export rather than a missing one. This has to hold in either lane:
        // a budget test that reads differently depending on how the suite was
        // invoked is the failure being fixed here.
        match std::env::var(ENFORCE_LATENCY_ENV) {
            Ok(value) => assert_eq!(latency_budgets_enforced(), value.trim() != "0"),
            Err(_) => assert!(latency_budgets_enforced()),
        }
    }
}
