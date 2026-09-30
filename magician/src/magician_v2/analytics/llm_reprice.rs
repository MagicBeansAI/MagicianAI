//! Corrective reprice of historical `llm_calls` Parquet rows.
//!
//! Stored `cost_usd` is frozen at write time, so rows priced under a stale or
//! wrong pricing table stay wrong forever unless deliberately corrected. This
//! module implements `magician analytics reprice-llm-calls`: walk the
//! date-partitioned Parquet lakehouse
//! (`<scope>/analytics/llm_calls/dt=YYYY-MM-DD/*.parquet`), recompute each
//! row's cost against the active effective-dated pricing table at the row's
//! own `timestamp_ms`, and — only when `--apply` is passed and a file's costs
//! actually changed — rewrite that file atomically (COPY to a per-write unique
//! staging sibling with the sink's Parquet/zstd idiom, `fsync` it, rename over
//! the original, then `fsync` the partition directory).
//!
//! Safety properties:
//! - Default is a DRY RUN: everything is computed, nothing is written.
//! - Today's partition (and any future-dated one) is skipped unless
//!   `--include-today` — the live sink appends new files there.
//! - Rows with a blank provider or model are never repriced (unrepriceable),
//!   only counted.
//! - A file that cannot be read or lacks the expected columns is skipped with
//!   a warning; the sweep never aborts on one bad file.
//! - Unchanged files are left byte-identical (no rewrite at all).
//! - Before the atomic rename, a row-count guard verifies the rewrite carries
//!   exactly the rows read from the original file; on mismatch the original
//!   is left untouched and the file is skipped with a warning (production
//!   analytics rows must never be lost).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use duckdb::Connection;
use magicllm::types::RealtimeUsage;
use magicllm::{compute_cost_with_at, LLMProviderKind, PricingTable, TokenUsage};
use tracing::warn;

use crate::magician_v2::analytics::duckdb_safety::{
    analytics_duckdb_guard, configure_analytics_connection_checked,
};
use crate::magician_v2::analytics::llm_pricing_identity::provider_kind_for_pricing;
use crate::magician_v2::artifact_v2::io::sync_parent_dir_blocking;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

/// Cost deltas at or below this magnitude are f64 noise, not a price change.
const COST_EPSILON: f64 = 1e-9;

/// Sweep configuration — mirrors the CLI flags.
#[derive(Debug, Clone, Default)]
pub struct RepriceOptions {
    /// Rewrite changed files in place. `false` = dry run (compute + report only).
    pub apply: bool,
    /// Limit to one principal (default: every scope under the storage root).
    pub principal: Option<String>,
    /// Limit to one workspace (default: every workspace of the selected principals).
    pub workspace: Option<String>,
    /// Inclusive lower `dt=YYYY-MM-DD` partition-date bound.
    pub from: Option<String>,
    /// Inclusive upper `dt=YYYY-MM-DD` partition-date bound.
    pub to: Option<String>,
    /// Also reprice today's (live) partition. Off by default because the sink
    /// appends new files there while the sweep runs.
    pub include_today: bool,
}

/// Counters for one file / partition tree / scope / whole run.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RepriceStats {
    pub files_scanned: usize,
    /// Files whose recomputed costs differ: rewritten when applying, or the
    /// would-rewrite count in a dry run.
    pub files_changed: usize,
    /// Files skipped as unreadable / missing expected columns (schema drift).
    pub files_skipped: usize,
    pub rows_scanned: usize,
    pub rows_repriced: usize,
    /// Rows with a blank provider or model — unrepriceable, never touched.
    pub rows_blank_skipped: usize,
    /// Rows whose timestamp, cost, token buckets or pricing route cannot be
    /// validated without inventing data — never rewritten.
    pub rows_unpriceable_skipped: usize,
    /// Sum of stored `cost_usd` over the repriced rows (before).
    pub old_cost_usd: f64,
    /// Sum of recomputed `cost_usd` over the repriced rows (after).
    pub new_cost_usd: f64,
}

impl RepriceStats {
    fn absorb(&mut self, other: &RepriceStats) {
        self.files_scanned += other.files_scanned;
        self.files_changed += other.files_changed;
        self.files_skipped += other.files_skipped;
        self.rows_scanned += other.rows_scanned;
        self.rows_repriced += other.rows_repriced;
        self.rows_blank_skipped += other.rows_blank_skipped;
        self.rows_unpriceable_skipped += other.rows_unpriceable_skipped;
        self.old_cost_usd += other.old_cost_usd;
        self.new_cost_usd += other.new_cost_usd;
    }
}

/// Per-(principal, workspace) result.
#[derive(Debug, Clone)]
pub struct ScopeRepriceReport {
    pub principal: String,
    pub workspace: String,
    pub stats: RepriceStats,
}

/// Whole-run result: per-scope breakdown + totals.
#[derive(Debug, Clone)]
pub struct RepriceReport {
    pub apply: bool,
    pub scopes: Vec<ScopeRepriceReport>,
    pub totals: RepriceStats,
}

impl RepriceReport {
    /// Human-readable report, clearly labeled DRY RUN vs APPLIED.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.apply {
            let _ = writeln!(out, "llm_calls reprice — APPLIED");
        } else {
            let _ = writeln!(
                out,
                "llm_calls reprice — DRY RUN (no files modified; pass --apply to rewrite)"
            );
        }
        if self.scopes.is_empty() {
            let _ = writeln!(out, "  no matching scopes with llm_calls partitions");
        }
        for scope in &self.scopes {
            let _ = writeln!(
                out,
                "  {}/{}: {}",
                scope.principal,
                scope.workspace,
                format_stats(&scope.stats, self.apply)
            );
        }
        let _ = writeln!(out, "  TOTAL: {}", format_stats(&self.totals, self.apply));
        out
    }
}

fn format_stats(stats: &RepriceStats, apply: bool) -> String {
    format!(
        "files scanned {}, files {} {}, files skipped {}, rows {}, repriced {}, blank-skipped {}, unpriceable-skipped {}, cost ${:.6} -> ${:.6} (delta {:+.6})",
        stats.files_scanned,
        if apply { "rewritten" } else { "needing rewrite" },
        stats.files_changed,
        stats.files_skipped,
        stats.rows_scanned,
        stats.rows_repriced,
        stats.rows_blank_skipped,
        stats.rows_unpriceable_skipped,
        stats.old_cost_usd,
        stats.new_cost_usd,
        stats.new_cost_usd - stats.old_cost_usd,
    )
}

/// Entry point for the CLI: reprice against the process-wide active pricing
/// table (installed from `llm_pricing.json` at startup, before the CLI
/// command branch runs).
pub fn run_reprice(
    workspace_layout: &ArtifactV2Workspace,
    opts: &RepriceOptions,
) -> Result<RepriceReport> {
    reprice_scopes_root(
        &workspace_layout.scopes_root(),
        magicllm::active_table(),
        opts,
    )
}

/// Walk every `(principal, workspace)` scope under `scopes_root` — the same
/// directory shape the retention sweep walks
/// (`<scopes_root>/<principal>/<workspace>/analytics/llm_calls/dt=*/`) —
/// honoring the principal/workspace filters, and reprice each scope's
/// partition tree against an explicit pricing table.
pub fn reprice_scopes_root(
    scopes_root: &Path,
    table: &PricingTable,
    opts: &RepriceOptions,
) -> Result<RepriceReport> {
    let mut scopes: Vec<ScopeRepriceReport> = Vec::new();
    if scopes_root.exists() {
        let principals = std::fs::read_dir(scopes_root)
            .with_context(|| format!("reading scopes root {}", scopes_root.display()))?;
        for principal_entry in principals.flatten() {
            let principal_path = principal_entry.path();
            if !principal_path.is_dir() {
                continue;
            }
            let Some(principal) = principal_path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            if let Some(wanted) = &opts.principal {
                if &principal != wanted {
                    continue;
                }
            }
            let workspaces = match std::fs::read_dir(&principal_path) {
                Ok(dir) => dir,
                Err(_) => continue,
            };
            for workspace_entry in workspaces.flatten() {
                let workspace_path = workspace_entry.path();
                if !workspace_path.is_dir() {
                    continue;
                }
                let Some(workspace) = workspace_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                if let Some(wanted) = &opts.workspace {
                    if &workspace != wanted {
                        continue;
                    }
                }
                let llm_calls_root = workspace_path.join("analytics").join("llm_calls");
                if !llm_calls_root.is_dir() {
                    continue;
                }
                let stats = reprice_llm_calls_tree(&llm_calls_root, table, opts);
                scopes.push(ScopeRepriceReport {
                    principal: principal.clone(),
                    workspace,
                    stats,
                });
            }
        }
    }
    scopes.sort_by(|a, b| {
        (a.principal.as_str(), a.workspace.as_str())
            .cmp(&(b.principal.as_str(), b.workspace.as_str()))
    });
    let mut totals = RepriceStats::default();
    for scope in &scopes {
        totals.absorb(&scope.stats);
    }
    Ok(RepriceReport {
        apply: opts.apply,
        scopes,
        totals,
    })
}

/// Reprice one scope's `llm_calls` partition tree (`dt=YYYY-MM-DD/*.parquet`),
/// applying the partition-date bounds and the today-skip. Per-file failures
/// (unreadable file, missing columns) are warned and counted, never fatal.
pub fn reprice_llm_calls_tree(
    llm_calls_root: &Path,
    table: &PricingTable,
    opts: &RepriceOptions,
) -> RepriceStats {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let mut stats = RepriceStats::default();
    let entries = match std::fs::read_dir(llm_calls_root) {
        Ok(entries) => entries,
        Err(_) => return stats,
    };
    let mut partitions: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(date_str) = name.strip_prefix("dt=") else {
            continue;
        };
        partitions.push((date_str.to_string(), path.clone()));
    }
    partitions.sort();

    for (date_str, partition_dir) in partitions {
        if let Some(from) = &opts.from {
            if date_str.as_str() < from.as_str() {
                continue;
            }
        }
        if let Some(to) = &opts.to {
            if date_str.as_str() > to.as_str() {
                continue;
            }
        }
        // Today's partition is live (the sink appends new files there) and a
        // future-dated one would be clock skew; both are skipped unless
        // explicitly included.
        if !opts.include_today && date_str.as_str() >= today.as_str() {
            continue;
        }
        let files = match std::fs::read_dir(&partition_dir) {
            Ok(dir) => dir,
            Err(_) => continue,
        };
        let mut parquet_files: Vec<PathBuf> = files
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("parquet")
            })
            .collect();
        parquet_files.sort();
        for file in parquet_files {
            match reprice_partition_file(&file, table, opts.apply) {
                Ok(file_stats) => stats.absorb(&file_stats),
                Err(error) => {
                    stats.files_skipped += 1;
                    warn!(
                        target: "analytics::llm_reprice",
                        path = %file.display(),
                        error = %error,
                        "reprice: skipping unreadable / schema-drifted parquet file"
                    );
                },
            }
        }
    }
    stats
}

/// Reprice a single Parquet file against an explicit pricing table.
///
/// Loads the file's FULL row set into an in-memory DuckDB table (all columns,
/// so a rewrite preserves everything), recomputes `cost_usd` per row at the
/// row's `timestamp_ms`, and — when `apply` is set and any cost moved beyond
/// f64 noise — patches the changed rows and COPYs the table to a per-write
/// unique staging sibling (Parquet/zstd, the sink's idiom) before `fsync`ing
/// it and atomically renaming it over the original. A pre-rename row-count
/// guard skips the file (original untouched, staging file removed) if the
/// rewrite would not carry exactly the rows read. Errors are per-file: callers
/// skip + count.
pub fn reprice_partition_file(
    path: &Path,
    table: &PricingTable,
    apply: bool,
) -> Result<RepriceStats> {
    let mut stats = RepriceStats {
        files_scanned: 1,
        ..Default::default()
    };

    let _duckdb_guard = analytics_duckdb_guard();
    let conn =
        Connection::open_in_memory().context("opening in-memory DuckDB for llm_calls reprice")?;
    configure_analytics_connection_checked(&conn, "llm_calls_reprice")
        .context("configuring conservative DuckDB limits for llm_calls reprice")?;

    // hive_partitioning is forced OFF: with auto-detection the `dt=YYYY-MM-DD`
    // directory would materialize as an extra `dt` column and get baked into
    // the rewritten file, drifting the schema.
    let path_sql = path.display().to_string().replace('\'', "''");
    conn.execute_batch(&format!(
        "CREATE TABLE reprice_batch AS \
         SELECT * FROM read_parquet('{path_sql}', hive_partitioning = false);"
    ))
    .with_context(|| format!("reading {} into DuckDB", path.display()))?;

    // The realtime audio-split columns are newer than most llm_calls files. Add
    // them as NULL when absent so the reprice SELECT resolves on older schemas —
    // otherwise every pre-audio file (i.e. all text-only history) would fail the
    // column check and get skipped instead of having its text rows repriced.
    conn.execute_batch(
        "ALTER TABLE reprice_batch ADD COLUMN IF NOT EXISTS audio_input_tokens INTEGER; \
         ALTER TABLE reprice_batch ADD COLUMN IF NOT EXISTS audio_output_tokens INTEGER; \
         ALTER TABLE reprice_batch ADD COLUMN IF NOT EXISTS audio_cached_tokens INTEGER;",
    )
    .context("backfilling realtime audio columns onto older reprice batches")?;

    struct Fix {
        rowid: i64,
        new_cost: f64,
    }
    let mut fixes: Vec<Fix> = Vec::new();
    {
        // The prepare fails if any expected column is missing — that IS the
        // schema-drift check; the caller skips the file with a warning.
        let mut stmt = conn
            .prepare(
                "SELECT rowid, provider, model, input_tokens, output_tokens, \
                 cache_read_tokens, cache_creation_tokens, cost_usd, timestamp_ms, \
                 audio_input_tokens, audio_output_tokens, audio_cached_tokens \
                 FROM reprice_batch",
            )
            .context("expected llm_calls columns missing (schema drift)")?;
        let mut rows = stmt.query([]).context("querying reprice rows")?;
        while let Some(row) = rows.next().context("reading reprice row")? {
            stats.rows_scanned += 1;
            let rowid: i64 = row.get(0)?;
            let provider: Option<String> = row.get(1)?;
            let model: Option<String> = row.get(2)?;
            let input_tokens: Option<i64> = row.get(3)?;
            let output_tokens: Option<i64> = row.get(4)?;
            let cache_read_tokens: Option<i64> = row.get(5)?;
            let cache_creation_tokens: Option<i64> = row.get(6)?;
            let cost_usd: Option<f64> = row.get(7)?;
            let timestamp_ms: Option<i64> = row.get(8)?;
            let audio_input_tokens: Option<i64> = row.get(9)?;
            let audio_output_tokens: Option<i64> = row.get(10)?;
            let audio_cached_tokens: Option<i64> = row.get(11)?;

            let provider = provider.unwrap_or_default();
            let model = model.unwrap_or_default();
            if provider.trim().is_empty() || model.trim().is_empty() {
                stats.rows_blank_skipped += 1;
                continue;
            }

            let Some(at_ms) = timestamp_ms.filter(|value| *value > 0) else {
                stats.rows_unpriceable_skipped += 1;
                continue;
            };
            let Some(old_cost) = cost_usd.filter(|value| value.is_finite() && *value >= 0.0) else {
                stats.rows_unpriceable_skipped += 1;
                continue;
            };
            let Some(input) = checked_token(input_tokens) else {
                stats.rows_unpriceable_skipped += 1;
                continue;
            };
            let Some(output) = checked_token(output_tokens) else {
                stats.rows_unpriceable_skipped += 1;
                continue;
            };
            let decision_rates = provider
                .starts_with("decision:")
                .then(|| table.lookup_at(&provider_kind_for_pricing(&provider), &model, at_ms))
                .flatten();
            let Some(cache_read) = checked_token(cache_read_tokens).or_else(|| {
                (cache_read_tokens.is_none()
                    && decision_rates.is_some_and(|r| {
                        r.cache_read_per_m.unwrap_or(r.input_per_m) == r.input_per_m
                    }))
                .then_some(0)
            }) else {
                stats.rows_unpriceable_skipped += 1;
                continue;
            };
            let Some(cache_creation) = checked_token(cache_creation_tokens).or_else(|| {
                (cache_creation_tokens.is_none()
                    && decision_rates.is_some_and(|r| {
                        r.cache_write_per_m.unwrap_or(r.input_per_m) == r.input_per_m
                    }))
                .then_some(0)
            }) else {
                stats.rows_unpriceable_skipped += 1;
                continue;
            };
            if u64::from(cache_read) + u64::from(cache_creation) > u64::from(input) {
                stats.rows_unpriceable_skipped += 1;
                continue;
            }
            // Realtime (voice) rows carry an audio-modality split and are priced by
            // the realtime rate shape, not the text table. Rebuild the RealtimeUsage
            // from the folded totals minus the stored audio portion and reprice via
            // the unified table's realtime pricing — so these rows reprice correctly
            // instead of collapsing to $0 under text pricing.
            let provider_kind = provider_kind_for_pricing(&provider);
            let new_cost = if provider_kind == LLMProviderKind::OpenAI
                && table.realtime_pricing_at(&model, at_ms).is_some()
            {
                // A coarse realtime row has no recoverable modality split and
                // cannot be safely repriced as if every token were text.
                let (Some(audio_in), Some(audio_out), Some(audio_cached)) = (
                    checked_token(audio_input_tokens),
                    checked_token(audio_output_tokens),
                    checked_token(audio_cached_tokens),
                ) else {
                    stats.rows_unpriceable_skipped += 1;
                    continue;
                };
                if audio_cached > cache_read
                    || u64::from(audio_in)
                        > u64::from(input)
                            .saturating_sub(u64::from(cache_read))
                            .saturating_sub(u64::from(cache_creation))
                    || audio_out > output
                    || cache_creation != 0
                {
                    stats.rows_unpriceable_skipped += 1;
                    continue;
                }
                // A model billed by the clock keeps the price its producer
                // measured: the ledger holds no seconds, so repricing it from
                // token buckets would rewrite a real cost to zero.
                if magicllm::realtime_is_duration_billed_at(&model, at_ms) {
                    stats.rows_unpriceable_skipped += 1;
                    continue;
                }
                let audio_in = u64::from(audio_in);
                let audio_out = u64::from(audio_out);
                let audio_cached = u64::from(audio_cached);
                let total_in = u64::from(input);
                let total_out = u64::from(output);
                let total_cached = u64::from(cache_read);
                let total_cache_creation = u64::from(cache_creation);
                let uncached_input = total_in
                    .saturating_sub(total_cached)
                    .saturating_sub(total_cache_creation);
                let realtime_usage = RealtimeUsage {
                    text_input_tokens: uncached_input.saturating_sub(audio_in),
                    text_cached_input_tokens: total_cached.saturating_sub(audio_cached),
                    text_output_tokens: total_out.saturating_sub(audio_out),
                    audio_input_tokens: audio_in,
                    audio_cached_input_tokens: audio_cached,
                    audio_output_tokens: audio_out,
                    // The row's seconds are not in the ledger. See the skip above.
                    billed_seconds: 0.0,
                };
                table.compute_realtime_cost_at(&model, &realtime_usage, at_ms)
            } else {
                if table.lookup_at(&provider_kind, &model, at_ms).is_none() {
                    stats.rows_unpriceable_skipped += 1;
                    continue;
                }
                // `prompt_tokens` (= stored input_tokens) already includes the
                // cache buckets; compute_cost_with_at subtracts them internally.
                let usage = TokenUsage {
                    prompt_tokens: Some(input),
                    completion_tokens: Some(output),
                    cached_tokens: Some(cache_read),
                    cache_creation_tokens: Some(cache_creation),
                    ..Default::default()
                };
                compute_cost_with_at(table, &provider_kind, &model, &usage, at_ms)
            };
            if (new_cost - old_cost).abs() > COST_EPSILON {
                stats.rows_repriced += 1;
                stats.old_cost_usd += old_cost;
                stats.new_cost_usd += new_cost;
                fixes.push(Fix { rowid, new_cost });
            }
        }
    }

    if fixes.is_empty() {
        // Unchanged file: leave it byte-identical, no rewrite of any kind.
        return Ok(stats);
    }
    stats.files_changed = 1;
    if !apply {
        return Ok(stats);
    }

    // Patch the changed rows in ONE statement joined on pre-update rowids
    // (sequential per-row UPDATEs could be confused by rowid churn).
    conn.execute_batch(
        "CREATE TABLE reprice_fixes (rid BIGINT NOT NULL, new_cost DOUBLE NOT NULL);",
    )
    .context("creating reprice_fixes table")?;
    {
        let mut app = conn
            .appender("reprice_fixes")
            .context("opening reprice_fixes appender")?;
        for fix in &fixes {
            app.append_row(duckdb::params![fix.rowid, fix.new_cost])
                .context("appending reprice fix")?;
        }
        app.flush().context("flushing reprice_fixes appender")?;
    }
    conn.execute_batch(
        "UPDATE reprice_batch SET cost_usd = f.new_cost \
         FROM reprice_fixes f WHERE reprice_batch.rowid = f.rid;",
    )
    .context("updating repriced rows")?;

    let tmp_path = reprice_staging_path(path);
    let tmp_sql = tmp_path.display().to_string().replace('\'', "''");
    if let Err(error) = conn.execute_batch(&format!(
        "COPY reprice_batch TO '{tmp_sql}' (FORMAT PARQUET, COMPRESSION 'zstd');"
    )) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error)
            .with_context(|| format!("copying repriced batch to {}", tmp_path.display()));
    }

    // DuckDB `COPY TO` returns once its writes are in the page cache. Without
    // this the rename below can publish a staging object whose bytes never
    // reached the disk, which on an unclean shutdown replaces a good partition
    // file with a truncated one. `parquet_maintenance.rs` already syncs here;
    // this writer was the one that did not.
    if let Err(error) = std::fs::File::open(&tmp_path).and_then(|file| file.sync_all()) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error)
            .with_context(|| format!("syncing repriced batch at {}", tmp_path.display()));
    }

    // Pre-rename row-count guard (production-data invariant: analytics rows
    // must never be lost). The rewrite must carry EXACTLY the rows read from
    // the original file; on any discrepancy the original stays untouched and
    // the file is skipped like any other bad file — the sweep continues.
    let batch_rows: i64 =
        match conn.query_row("SELECT count(*) FROM reprice_batch", [], |row| row.get(0)) {
            Ok(count) => count,
            Err(error) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(error).context("counting reprice_batch rows for the pre-rename guard");
            },
        };
    if !row_count_guard_passes(batch_rows, stats.rows_scanned) {
        warn!(
            target: "analytics::llm_reprice",
            path = %path.display(),
            batch_rows,
            rows_scanned = stats.rows_scanned,
            "reprice: pre-rename row-count mismatch — leaving original untouched, skipping file"
        );
        let _ = std::fs::remove_file(&tmp_path);
        // Nothing was applied: report the file as skipped, not changed.
        stats.files_changed = 0;
        stats.files_skipped = 1;
        stats.rows_repriced = 0;
        stats.old_cost_usd = 0.0;
        stats.new_cost_usd = 0.0;
        return Ok(stats);
    }

    if let Err(error) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(error)
            .with_context(|| format!("renaming {} over {}", tmp_path.display(), path.display()));
    }
    // A rename is atomic with respect to ordering, not durability: without the
    // parent-directory sync the rename can survive a power cut while the
    // repriced bytes do not, leaving the partition entry pointing at nothing.
    sync_parent_dir_blocking(path)
        .with_context(|| format!("syncing the partition directory of {}", path.display()))?;
    crate::magician_v2::dataset_owners::publish_written_parquet(path).with_context(|| {
        format!(
            "publishing repriced parquet through DatasetAccess {}",
            path.display()
        )
    })?;
    Ok(stats)
}

/// Per-write staging sibling for an in-place partition rewrite.
///
/// The name carries a UUID rather than a fixed `<file>.reprice.tmp` suffix. A
/// fixed name is shared by every concurrent sweep of the same partition file,
/// so two of them interleaving rename a half-written Parquet object over live
/// analytics data — the same failure the shared byte writer in
/// `artifact_v2::io` exists to prevent. (That writer cannot own this publish:
/// DuckDB's `COPY TO` produces the staging bytes itself, so there is no
/// in-memory buffer to hand it.)
///
/// The staging file stays in the partition directory so the publish is a
/// same-filesystem rename, and it is dot-prefixed and keeps the `.tmp`
/// extension so neither this sweep's `*.parquet` filter nor a reader's glob
/// can mistake a leftover for a partition object.
fn reprice_staging_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "partition".to_string());
    path.with_file_name(format!(
        ".{file_name}.reprice.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ))
}

/// Pre-rename row-count guard predicate: passes only when the in-memory
/// reprice table still holds exactly the number of rows read from the
/// original file. A negative or non-representable DuckDB count never passes.
fn row_count_guard_passes(batch_rows: i64, rows_scanned: usize) -> bool {
    usize::try_from(batch_rows).is_ok_and(|count| count == rows_scanned)
}

/// Historical cost correction must never turn missing, negative or oversized
/// usage into a plausible zero/saturated token count.
fn checked_token(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magicllm::{PricingRow, ProviderPricing};

    /// Mirrors the sink's `llm_calls_batch` schema (llm_parquet_sink.rs) so
    /// fixtures are shape-identical to production partition files.
    const FIXTURE_SCHEMA: &str = r#"
        CREATE TABLE llm_calls_batch (
            timestamp_ms BIGINT NOT NULL,
            started_at_ms BIGINT NOT NULL,
            latency_ms BIGINT NOT NULL,
            principal VARCHAR NOT NULL,
            workspace VARCHAR NOT NULL,
            execution_id VARCHAR NOT NULL,
            task_id VARCHAR,
            plan_id VARCHAR NOT NULL,
            step_id VARCHAR,
            step_index BIGINT,
            agent_id VARCHAR,
            delegated_agent_id VARCHAR,
            chat_session_id VARCHAR,
            operation VARCHAR NOT NULL,
            profile VARCHAR,
            provider VARCHAR NOT NULL,
            model VARCHAR NOT NULL,
            capability VARCHAR NOT NULL,
            response_kind VARCHAR NOT NULL,
            attempt INTEGER NOT NULL,
            success BOOLEAN NOT NULL,
            error VARCHAR,
            input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL,
            reasoning_tokens INTEGER NOT NULL,
            cache_read_tokens INTEGER NOT NULL,
            cache_creation_tokens INTEGER NOT NULL,
            audio_input_tokens INTEGER,
            audio_output_tokens INTEGER,
            audio_cached_tokens INTEGER,
            ttft_ms BIGINT,
            cost_usd DOUBLE NOT NULL
        );
    "#;

    struct FixtureRow {
        provider: &'static str,
        model: &'static str,
        input_tokens: i32,
        output_tokens: i32,
        cache_read_tokens: i32,
        cache_creation_tokens: i32,
        /// Realtime audio-split portions (None for text rows).
        audio_input_tokens: Option<i32>,
        audio_output_tokens: Option<i32>,
        audio_cached_tokens: Option<i32>,
        cost_usd: f64,
        timestamp_ms: i64,
    }

    fn write_fixture(path: &Path, rows: &[FixtureRow]) {
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("mkdir");
        let conn = Connection::open_in_memory().expect("duckdb");
        conn.execute_batch(FIXTURE_SCHEMA).expect("schema");
        {
            let mut app = conn.appender("llm_calls_batch").expect("appender");
            for (index, row) in rows.iter().enumerate() {
                app.append_row(duckdb::params![
                    row.timestamp_ms,
                    row.timestamp_ms,
                    250i64,
                    "anonymous",
                    "default",
                    format!("exec-{index}"),
                    None::<String>,
                    "plan-1",
                    None::<String>,
                    None::<i64>,
                    Some("agent-1".to_string()),
                    None::<String>,
                    None::<String>,
                    "agentic_decision",
                    None::<String>,
                    row.provider,
                    row.model,
                    "chat",
                    "success",
                    1i32,
                    true,
                    None::<String>,
                    row.input_tokens,
                    row.output_tokens,
                    0i32,
                    row.cache_read_tokens,
                    row.cache_creation_tokens,
                    row.audio_input_tokens,
                    row.audio_output_tokens,
                    row.audio_cached_tokens,
                    None::<i64>,
                    row.cost_usd,
                ])
                .expect("append fixture row");
            }
            app.flush().expect("flush");
        }
        let path_sql = path.display().to_string().replace('\'', "''");
        conn.execute_batch(&format!(
            "COPY llm_calls_batch TO '{path_sql}' (FORMAT PARQUET, COMPRESSION 'zstd');"
        ))
        .expect("copy fixture parquet");
    }

    /// $10/M input, $20/M output, $1/M cache-read, $12.50/M cache-write for
    /// every anthropic `claude-*` model, effective from epoch 0 — an explicit
    /// table so tests never depend on the process-wide `active_table` OnceLock.
    fn test_table() -> PricingTable {
        PricingTable::with_pricing_rows(vec![PricingRow::new(
            LLMProviderKind::Anthropic,
            "claude-",
            0,
            ProviderPricing {
                input_per_m: 10.0,
                output_per_m: 20.0,
                cache_read_per_m: Some(1.0),
                cache_write_per_m: Some(12.5),
            },
        )])
    }

    fn read_costs(path: &Path) -> Vec<f64> {
        let conn = Connection::open_in_memory().expect("duckdb");
        let path_sql = path.display().to_string().replace('\'', "''");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT cost_usd FROM read_parquet('{path_sql}', hive_partitioning = false) \
                 ORDER BY execution_id"
            ))
            .expect("prepare");
        let costs = stmt
            .query_map([], |row| row.get::<_, f64>(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("rows");
        costs
    }

    fn mispriced_row() -> FixtureRow {
        FixtureRow {
            provider: "anthropic",
            model: "claude-test-1",
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            audio_input_tokens: None,
            audio_output_tokens: None,
            audio_cached_tokens: None,
            cost_usd: 0.0, // correct cost under test_table() is 10 + 20 = 30
            timestamp_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn realtime_rows_reprice_via_realtime_table_not_zeroed() {
        // A realtime voice row: `gpt-realtime-2.1` isn't in the text pricing table,
        // so before the unified-pricing + audio-split work it would reprice to $0.
        // Now the repricer rebuilds the RealtimeUsage from the folded totals minus
        // the stored audio split and prices it via the realtime rate shape.
        // Folded: input 1.2M (audio 1M + text 200k), output 600k (audio 500k +
        // text 100k). gpt-realtime-2.1: audio $32/$64, text $4/$24 per 1M →
        // 1M*32 + 0.2M*4 + 0.5M*64 + 0.1M*24 = 32 + 0.8 + 32 + 2.4 = $67.20.
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("dt=2026-01-01").join("voice.parquet");
        let realtime = FixtureRow {
            provider: "openai",
            model: "gpt-realtime-2.1",
            input_tokens: 1_200_000,
            output_tokens: 600_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            audio_input_tokens: Some(1_000_000),
            audio_output_tokens: Some(500_000),
            audio_cached_tokens: Some(0),
            cost_usd: 0.0, // stored $0 — must reprice to the realtime price, not stay $0
            timestamp_ms: 1_700_000_000_000,
        };
        write_fixture(&file, &[realtime]);

        let stats = reprice_partition_file(&file, &test_table(), true).expect("reprice");
        assert_eq!(
            stats.rows_repriced, 1,
            "realtime row must reprice, not skip"
        );
        let costs = read_costs(&file);
        assert!(
            (costs[0] - 67.2).abs() < 1e-6,
            "realtime row should reprice to $67.20 via realtime pricing, got {}",
            costs[0]
        );
    }

    #[test]
    fn realtime_reprice_does_not_double_bill_cached_input_as_uncached_text() {
        // Folded input contains every input bucket: 100k uncached text + 20k
        // cached text + 300k uncached audio + 80k cached audio = 500k. The
        // audio split stores uncached audio separately and cached audio as a
        // cache subset, so reconstructing text must first remove all cache.
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("dt=2026-01-01").join("voice-cache.parquet");
        let realtime = FixtureRow {
            provider: "openai",
            model: "gpt-realtime-2.1",
            input_tokens: 500_000,
            output_tokens: 200_000,
            cache_read_tokens: 100_000,
            cache_creation_tokens: 0,
            audio_input_tokens: Some(300_000),
            audio_output_tokens: Some(150_000),
            audio_cached_tokens: Some(80_000),
            cost_usd: 0.0,
            timestamp_ms: 1_700_000_000_000,
        };
        write_fixture(&file, &[realtime]);

        let expected = test_table().compute_realtime_cost_at(
            "gpt-realtime-2.1",
            &RealtimeUsage {
                text_input_tokens: 100_000,
                text_cached_input_tokens: 20_000,
                text_output_tokens: 50_000,
                audio_input_tokens: 300_000,
                audio_cached_input_tokens: 80_000,
                audio_output_tokens: 150_000,
                billed_seconds: 0.0,
            },
            1_700_000_000_000,
        );
        reprice_partition_file(&file, &test_table(), true).expect("reprice");
        let costs = read_costs(&file);
        assert!((costs[0] - expected).abs() < 1e-9);
    }

    #[test]
    fn historical_runtime_adapter_alias_uses_the_same_pricing_identity_as_live_capture() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp
            .path()
            .join("dt=2026-01-01")
            .join("voice-adapter-alias.parquet");
        let realtime = FixtureRow {
            provider: "openai_realtime_backend",
            model: "gpt-realtime-2.1",
            input_tokens: 500_000,
            output_tokens: 200_000,
            cache_read_tokens: 100_000,
            cache_creation_tokens: 0,
            audio_input_tokens: Some(300_000),
            audio_output_tokens: Some(150_000),
            audio_cached_tokens: Some(80_000),
            cost_usd: 0.0,
            timestamp_ms: 1_700_000_000_000,
        };
        write_fixture(&file, &[realtime]);

        let stats = reprice_partition_file(&file, &test_table(), true).expect("reprice");
        assert_eq!(stats.rows_repriced, 1);
        assert!(read_costs(&file)[0] > 0.0);
    }

    #[test]
    fn apply_reprices_changed_row_and_preserves_others() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("dt=2026-01-01").join("batch_a.parquet");
        // Row 0 mispriced; row 1 already correct (cache buckets exercised:
        // 500k uncached input * $10/M + 300k read * $1/M + 200k write
        // * $12.50/M + 1M output * $20/M = 5 + 0.3 + 2.5 + 20 = 27.8).
        let correct = FixtureRow {
            provider: "anthropic",
            model: "claude-test-2",
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 300_000,
            cache_creation_tokens: 200_000,
            audio_input_tokens: None,
            audio_output_tokens: None,
            audio_cached_tokens: None,
            cost_usd: 27.8,
            timestamp_ms: 1_700_000_000_000,
        };
        write_fixture(&file, &[mispriced_row(), correct]);

        let stats = reprice_partition_file(&file, &test_table(), true).expect("reprice");
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.files_changed, 1);
        assert_eq!(
            stats.files_skipped, 0,
            "pre-rename row-count guard must pass on a normal apply"
        );
        assert_eq!(stats.rows_scanned, 2);
        assert_eq!(stats.rows_repriced, 1);
        assert_eq!(stats.rows_blank_skipped, 0);
        assert!((stats.old_cost_usd - 0.0).abs() < 1e-9);
        assert!((stats.new_cost_usd - 30.0).abs() < 1e-9);

        let costs = read_costs(&file); // ordered by execution_id: exec-0, exec-1
        assert_eq!(costs.len(), 2);
        assert!((costs[0] - 30.0).abs() < 1e-9, "repriced row: {}", costs[0]);
        assert!(
            (costs[1] - 27.8).abs() < 1e-9,
            "untouched row: {}",
            costs[1]
        );
        assert!(
            !partition_has_staging_files(&file),
            "staging file must be renamed away"
        );
    }

    /// A reader of the partition directory must see the repriced file or the
    /// original, never a staging object alongside it.
    #[test]
    fn apply_leaves_no_staging_file_and_the_published_parquet_parses() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let partition = tmp.path().join("dt=2026-01-01");
        // Two files in one partition: the staging name is per-write, so
        // rewriting both must leave the directory holding exactly the two
        // published objects.
        let first = partition.join("staging_a.parquet");
        let second = partition.join("staging_b.parquet");
        write_fixture(&first, &[mispriced_row()]);
        write_fixture(&second, &[mispriced_row()]);

        reprice_partition_file(&first, &test_table(), true).expect("reprice first");
        reprice_partition_file(&second, &test_table(), true).expect("reprice second");

        assert!(
            !partition_has_staging_files(&first),
            "publish must leave no staging sibling in the partition directory"
        );
        // Both parse as Parquet through the same reader the sweep uses, and
        // carry the repriced cost rather than the mispriced one.
        for published in [&first, &second] {
            let costs = read_costs(published);
            assert_eq!(costs.len(), 1, "{} lost rows", published.display());
            assert!(
                (costs[0] - 30.0).abs() < 1e-9,
                "{} published {} instead of the repriced cost",
                published.display(),
                costs[0]
            );
        }
    }

    /// Any `.tmp` entry in the partition directory holding `file`.
    fn partition_has_staging_files(file: &Path) -> bool {
        let Some(partition) = file.parent() else {
            return false;
        };
        std::fs::read_dir(partition)
            .expect("partition listing")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
    }

    /// The guard predicate itself: only an exact match between the reprice
    /// table's count and the rows read from the original file may proceed to
    /// the rename. (Forcing a real mid-apply mismatch through DuckDB would
    /// mean mocking it — the applied path is instead covered by the
    /// happy-path assertion above that a normal apply is never guard-skipped.)
    #[test]
    fn row_count_guard_requires_exact_match() {
        assert!(row_count_guard_passes(0, 0));
        assert!(row_count_guard_passes(2, 2));
        assert!(!row_count_guard_passes(1, 2), "dropped row must fail");
        assert!(!row_count_guard_passes(3, 2), "duplicated row must fail");
        assert!(!row_count_guard_passes(-1, 0), "negative count must fail");
    }

    #[test]
    fn invalid_or_unpriced_rows_are_never_rewritten_as_plausible_zero_cost() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("dt=2026-01-01").join("unpriceable.parquet");
        let unpriced = FixtureRow {
            provider: "unknown-provider",
            model: "unknown-model",
            cost_usd: 123.0,
            ..mispriced_row()
        };
        let impossible_cache = FixtureRow {
            input_tokens: 10,
            cache_read_tokens: 8,
            cache_creation_tokens: 8,
            cost_usd: 456.0,
            ..mispriced_row()
        };
        let coarse_realtime = FixtureRow {
            provider: "openai",
            model: "gpt-realtime-2.1",
            input_tokens: 500,
            output_tokens: 200,
            cache_read_tokens: 100,
            cache_creation_tokens: 0,
            audio_input_tokens: None,
            audio_output_tokens: None,
            audio_cached_tokens: None,
            cost_usd: 789.0,
            timestamp_ms: 1_700_000_000_000,
        };
        let spoofed_realtime = FixtureRow {
            provider: "private-realtime",
            model: "gpt-realtime-2.1",
            input_tokens: 500,
            output_tokens: 200,
            cache_read_tokens: 100,
            cache_creation_tokens: 0,
            audio_input_tokens: Some(300),
            audio_output_tokens: Some(150),
            audio_cached_tokens: Some(80),
            cost_usd: 321.0,
            timestamp_ms: 1_700_000_000_000,
        };
        write_fixture(
            &file,
            &[
                unpriced,
                impossible_cache,
                coarse_realtime,
                spoofed_realtime,
            ],
        );
        let bytes_before = std::fs::read(&file).expect("read");

        let stats = reprice_partition_file(&file, &test_table(), true).expect("reprice");
        assert_eq!(stats.rows_unpriceable_skipped, 4);
        assert_eq!(stats.rows_repriced, 0);
        assert_eq!(stats.files_changed, 0);
        assert_eq!(std::fs::read(&file).expect("read"), bytes_before);
    }

    #[test]
    fn checked_token_rejects_missing_negative_and_oversized_values() {
        assert_eq!(checked_token(Some(0)), Some(0));
        assert_eq!(checked_token(Some(42)), Some(42));
        assert_eq!(checked_token(None), None);
        assert_eq!(checked_token(Some(-1)), None);
        assert_eq!(checked_token(Some(i64::from(u32::MAX) + 1)), None);
    }

    #[test]
    fn blank_provider_or_model_rows_are_skipped_not_repriced() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("dt=2026-01-01").join("batch_b.parquet");
        let blank_provider = FixtureRow {
            provider: "",
            model: "claude-test-1",
            cost_usd: 123.0, // wrong under any table, but unrepriceable
            ..mispriced_row()
        };
        let blank_model = FixtureRow {
            provider: "anthropic",
            model: "",
            cost_usd: 456.0,
            ..mispriced_row()
        };
        write_fixture(&file, &[blank_provider, blank_model]);
        let bytes_before = std::fs::read(&file).expect("read");

        let stats = reprice_partition_file(&file, &test_table(), true).expect("reprice");
        assert_eq!(stats.rows_blank_skipped, 2);
        assert_eq!(stats.rows_repriced, 0);
        assert_eq!(stats.files_changed, 0);
        let bytes_after = std::fs::read(&file).expect("read");
        assert_eq!(
            bytes_before, bytes_after,
            "blank-only file must not be rewritten"
        );
    }

    #[test]
    fn dry_run_reports_changes_but_leaves_file_byte_identical() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("dt=2026-01-01").join("batch_c.parquet");
        write_fixture(&file, &[mispriced_row()]);
        let bytes_before = std::fs::read(&file).expect("read");

        let stats = reprice_partition_file(&file, &test_table(), false).expect("reprice");
        assert_eq!(stats.rows_repriced, 1);
        assert_eq!(stats.files_changed, 1, "dry run still counts would-rewrite");
        let bytes_after = std::fs::read(&file).expect("read");
        assert_eq!(
            bytes_before, bytes_after,
            "dry run must not modify the file"
        );
    }

    #[test]
    fn unchanged_file_is_left_byte_identical_on_apply() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("dt=2026-01-01").join("batch_d.parquet");
        let correct = FixtureRow {
            cost_usd: 30.0,
            ..mispriced_row()
        };
        write_fixture(&file, &[correct]);
        let bytes_before = std::fs::read(&file).expect("read");

        let stats = reprice_partition_file(&file, &test_table(), true).expect("reprice");
        assert_eq!(stats.rows_repriced, 0);
        assert_eq!(stats.files_changed, 0);
        let bytes_after = std::fs::read(&file).expect("read");
        assert_eq!(
            bytes_before, bytes_after,
            "unchanged file must not be rewritten"
        );
    }

    #[test]
    fn partition_date_bounds_and_today_skip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("llm_calls");
        let today = Utc::now().format("%Y-%m-%d").to_string();
        write_fixture(
            &root.join("dt=2026-01-01").join("a.parquet"),
            &[mispriced_row()],
        );
        write_fixture(
            &root.join("dt=2026-01-02").join("b.parquet"),
            &[mispriced_row()],
        );
        write_fixture(
            &root.join(format!("dt={today}")).join("c.parquet"),
            &[mispriced_row()],
        );

        let table = test_table();
        let dry = RepriceOptions::default();

        // Default: both past partitions, today skipped.
        let stats = reprice_llm_calls_tree(&root, &table, &dry);
        assert_eq!(stats.files_scanned, 2);

        // --from excludes the earlier partition (inclusive bound).
        let stats = reprice_llm_calls_tree(
            &root,
            &table,
            &RepriceOptions {
                from: Some("2026-01-02".to_string()),
                ..dry.clone()
            },
        );
        assert_eq!(stats.files_scanned, 1);

        // --to excludes the later partition (inclusive bound).
        let stats = reprice_llm_calls_tree(
            &root,
            &table,
            &RepriceOptions {
                to: Some("2026-01-01".to_string()),
                ..dry.clone()
            },
        );
        assert_eq!(stats.files_scanned, 1);

        // --include-today picks up the live partition too.
        let stats = reprice_llm_calls_tree(
            &root,
            &table,
            &RepriceOptions {
                include_today: true,
                ..dry
            },
        );
        assert_eq!(stats.files_scanned, 3);
    }

    #[test]
    fn schema_drifted_file_is_skipped_without_aborting_the_sweep() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("llm_calls");
        let partition = root.join("dt=2026-01-01");
        std::fs::create_dir_all(&partition).expect("mkdir");

        // A parquet file that lacks the llm_calls columns entirely.
        let drifted = partition.join("a_drifted.parquet");
        {
            let conn = Connection::open_in_memory().expect("duckdb");
            conn.execute_batch("CREATE TABLE t (unrelated VARCHAR); INSERT INTO t VALUES ('x');")
                .expect("table");
            let path_sql = drifted.display().to_string().replace('\'', "''");
            conn.execute_batch(&format!(
                "COPY t TO '{path_sql}' (FORMAT PARQUET, COMPRESSION 'zstd');"
            ))
            .expect("copy");
        }
        write_fixture(&partition.join("b_good.parquet"), &[mispriced_row()]);

        let stats = reprice_llm_calls_tree(&root, &test_table(), &RepriceOptions::default());
        assert_eq!(stats.files_skipped, 1, "drifted file skipped with a warn");
        assert_eq!(stats.files_scanned, 1, "good file still processed");
        assert_eq!(stats.rows_repriced, 1);
    }

    #[test]
    fn scope_walk_honors_principal_and_workspace_filters() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scopes_root = tmp.path().join("scopes");
        for (principal, workspace) in [("anonymous", "default"), ("other", "ws2")] {
            write_fixture(
                &scopes_root
                    .join(principal)
                    .join(workspace)
                    .join("analytics")
                    .join("llm_calls")
                    .join("dt=2026-01-01")
                    .join("a.parquet"),
                &[mispriced_row()],
            );
        }
        let table = test_table();

        let all =
            reprice_scopes_root(&scopes_root, &table, &RepriceOptions::default()).expect("walk");
        assert_eq!(all.scopes.len(), 2);
        assert_eq!(all.totals.files_scanned, 2);
        assert_eq!(all.totals.rows_repriced, 2);
        assert!(!all.apply);

        let filtered = reprice_scopes_root(
            &scopes_root,
            &table,
            &RepriceOptions {
                principal: Some("other".to_string()),
                workspace: Some("ws2".to_string()),
                ..Default::default()
            },
        )
        .expect("walk");
        assert_eq!(filtered.scopes.len(), 1);
        assert_eq!(filtered.scopes[0].principal, "other");
        assert_eq!(filtered.scopes[0].workspace, "ws2");
        assert_eq!(filtered.totals.files_scanned, 1);
    }
    #[test]
    fn decision_model_repricing_preserves_unknown_cache_and_requires_flat_rates() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("dt=2026-09-27").join("decision.parquet");
        let mut row = mispriced_row();
        row.provider = "decision:typesafe";
        row.model = "jev-1.13.0";
        row.timestamp_ms = 1_790_467_200_000;
        write_fixture(&file, &[row]);
        let path = file.display().to_string().replace('\'', "''");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("CREATE TABLE missing_cache AS SELECT * REPLACE(NULL::BIGINT AS cache_read_tokens, NULL::BIGINT AS cache_creation_tokens) FROM read_parquet('{path}'); COPY missing_cache TO '{path}' (FORMAT PARQUET)")).unwrap();
        let stats = reprice_partition_file(&file, &PricingTable::builtin(), true).unwrap();
        assert_eq!(stats.rows_repriced, 1);
        assert!((read_costs(&file)[0] - 0.042).abs() < 1e-12);
        let missing: i64 = conn.query_row(&format!("SELECT count(*) FROM read_parquet('{path}') WHERE cache_read_tokens IS NULL AND cache_creation_tokens IS NULL"), [], |r| r.get(0)).unwrap();
        assert_eq!(missing, 1, "repricing must not invent zero cache counters");
        let discounted = PricingTable::builtin_with(vec![PricingRow::new(
            LLMProviderKind::Custom("decision:typesafe".into()),
            "jev-1.13",
            1_789_430_400_000,
            ProviderPricing {
                input_per_m: 0.042,
                output_per_m: 0.0,
                cache_read_per_m: Some(0.01),
                cache_write_per_m: None,
            },
        )]);
        let stats = reprice_partition_file(&file, &discounted, true).unwrap();
        assert_eq!(stats.rows_repriced, 0);
        assert_eq!(stats.rows_unpriceable_skipped, 1);
    }
}
