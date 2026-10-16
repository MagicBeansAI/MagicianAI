//! Live Thinking Map — deterministic replay, restore-as-branch, and
//! crash-recovery startup repair (Phase 2b).
//!
//! The append-only event log ([`super::store::MapEvent`]) is the authoritative
//! history; the snapshot is a derived materialization. This module leans on that
//! invariant to provide:
//!
//! - **Deterministic replay** — reconstruct the map at any past sequence /
//!   utterance / timestamp by folding the stored envelopes through the pure
//!   [`apply_envelope`] reducer from the revision-0 base. Each fold step
//!   re-verifies the reproduced `resulting_revision` + `semantic_hash` against
//!   the value the log recorded at apply time, so replay is byte-equivalent to
//!   the original run or it fails `Corrupt`.
//! - **Restore-as-branch** — fork a *new* map at a past sequence; the source map
//!   and its log are never touched (history is append-only, never rewritten).
//! - **Startup repair** — roll the snapshot/manifest FORWARD to the last durable
//!   event after a crash in the 2a append→snapshot→manifest window. Committed
//!   events are authoritative and are never rolled back, dropped, or truncated;
//!   only a truncated/partial FINAL line is quarantined.
//!
//! ## Revision-0 reconstruction invariant
//! The create-time (revision-0) map is fully reconstructible from the manifest
//! via [`ThinkingMap::new`], because no current operation mutates
//! `title`/`source`/`lifecycle`/`created_at`. Replay = reconstruct that base,
//! then re-apply the stored events. A `debug_assert!` + runtime hash check in
//! [`ThinkingMapStore::replay_to_sequence`] guards this: if a future op ever
//! mutates one of those create-time fields, the reconstructed base would diverge
//! from history and replay fails loudly rather than returning a wrong map.
//!
//! Everything here is dormant: no route/service registration, no runtime wiring.
//! Deterministic: reducer replay only — no `Utc::now()`, no randomness.

use crate::thinking_map::models::ThinkingMap;
use crate::thinking_map::reducer::{apply_envelope, semantic_hash, ApplyOutcome};

use super::store::{MapEvent, MapManifest, StoreResult, ThinkingMapStore, ThinkingMapStoreError};

// ── Diff between two map revisions ────────────────────────────────────────────

/// A light structural diff between two map states (id-set comparison only).
/// Pure data; produced by [`diff_maps`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MapDiff {
    pub added_nodes: Vec<String>,
    pub removed_nodes: Vec<String>,
    /// Nodes present in both maps whose value differs.
    pub changed_nodes: Vec<String>,
    pub added_edges: Vec<String>,
    pub removed_edges: Vec<String>,
}

/// Compare two maps by node/edge id sets. A node id present in both maps with a
/// different [`super::models::ThinkingNode`] value is reported as `changed`.
/// Deterministic (iterates the `BTreeMap`s) and does no IO.
pub fn diff_maps(old: &ThinkingMap, new: &ThinkingMap) -> MapDiff {
    let mut diff = MapDiff::default();

    for (id, node) in &new.nodes {
        match old.nodes.get(id) {
            None => diff.added_nodes.push(id.clone()),
            Some(prev) if prev != node => diff.changed_nodes.push(id.clone()),
            Some(_) => {},
        }
    }
    for id in old.nodes.keys() {
        if !new.nodes.contains_key(id) {
            diff.removed_nodes.push(id.clone());
        }
    }

    for id in new.edges.keys() {
        if !old.edges.contains_key(id) {
            diff.added_edges.push(id.clone());
        }
    }
    for id in old.edges.keys() {
        if !new.edges.contains_key(id) {
            diff.removed_edges.push(id.clone());
        }
    }

    diff
}

// ── Crash-recovery repair report ──────────────────────────────────────────────

/// Outcome of [`ThinkingMapStore::startup_repair`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepairReport {
    /// Whether the snapshot/manifest were rewritten to match the log.
    pub repaired: bool,
    /// Whether a truncated/partial FINAL log line was quarantined (dropped).
    pub quarantined_trailing_bytes: bool,
    /// Number of duplicate-sequence records removed after deterministic replay
    /// proved that each duplicate group came from one idempotent operation and
    /// that the retained branch preserves the complete event chain.
    pub deduplicated_events: u64,
    /// The log changed under the repair between the read and the write, so
    /// nothing was written. See [`ThinkingMapStore::startup_repair`]'s
    /// concurrency note: a repair that wrote here would roll a live writer's
    /// committed event back out of the snapshot, or drop it from the log.
    pub skipped_concurrent_write: bool,
    /// Snapshot revision before repair.
    pub from_revision: u64,
    /// Snapshot revision after repair (== last good event's resulting_revision).
    pub to_revision: u64,
    /// Number of events replayed from base to reconstruct the rolled-forward
    /// snapshot (0 when no repair happened).
    pub replayed_events: u64,
}

/// Aggregate outcome of one [`ThinkingMapStore::startup_repair_all`] pass.
///
/// Every map the sweep touched lands in `maps_scanned`; the other counters are
/// non-exclusive facets of that total (a map can be both quarantined and
/// repaired). `maps_failed` is the one that matters operationally: those maps
/// are still broken after the sweep and their `events_after` reads — hence
/// replay, restore-as-branch, and `GET /thinking-maps/{id}/events` — will keep
/// failing until a human looks at them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupRepairSweep {
    /// `(principal, workspace)` pairs enumerated.
    pub scopes_scanned: usize,
    /// Scopes whose map directory would not enumerate. Every map inside such a
    /// scope is invisible to this pass, which is why it is counted rather than
    /// only logged — a pass that silently skipped a whole scope must not read
    /// as clean.
    pub scopes_unreadable: usize,
    /// Maps the sweep attempted, including the ones that failed.
    pub maps_scanned: usize,
    /// Maps whose snapshot/manifest (or log) were rewritten.
    pub maps_repaired: usize,
    /// Maps that had a truncated/partial trailing log line dropped.
    pub maps_quarantined: usize,
    /// Maps whose log contained one or more provable duplicate append groups
    /// and was rewritten to the converged replay-valid sequence.
    pub maps_deduplicated: usize,
    /// Maps left alone because the log moved while the repair was reading it.
    pub maps_skipped_concurrent_write: usize,
    /// Maps that could not be repaired (interior corruption, sequence gap,
    /// snapshot ahead of the log, or I/O). Still broken after the sweep.
    pub maps_failed: usize,
    /// Maps whose duplicate history stays ambiguous after the snapshot
    /// tie-break. Marked on disk and left alone: reads keep serving the
    /// snapshot, the log will not replay, and the sweep stops re-trying (and
    /// re-logging an ERROR) every boot.
    pub maps_quarantined_ambiguous: usize,
}

impl StartupRepairSweep {
    /// Whether the pass did anything worth an operator seeing. A completely
    /// clean boot logs nothing.
    pub fn touched_anything(&self) -> bool {
        self.scopes_unreadable
            + self.maps_repaired
            + self.maps_quarantined
            + self.maps_quarantined_ambiguous
            + self.maps_deduplicated
            + self.maps_skipped_concurrent_write
            + self.maps_failed
            > 0
    }
}

fn replay_events_from_manifest(
    manifest: &MapManifest,
    events: &[MapEvent],
    target_sequence: u64,
) -> StoreResult<ThinkingMap> {
    let mut acc = ThinkingMap::new(
        manifest.map_id.clone(),
        manifest.principal.clone(),
        manifest.workspace.clone(),
        manifest.title.clone(),
        manifest.source.clone(),
        manifest.created_at.clone(),
    );
    for event in events {
        if event.sequence > target_sequence {
            break;
        }
        match apply_envelope(&acc, &event.envelope, &event.applied_at) {
            Ok(ApplyOutcome::Applied {
                map,
                resulting_revision,
                semantic_hash,
            }) if resulting_revision == event.resulting_revision
                && semantic_hash == event.semantic_hash =>
            {
                acc = map;
            },
            Ok(ApplyOutcome::Applied { .. }) => {
                return Err(ThinkingMapStoreError::Corrupt(format!(
                    "replay divergence at seq {}",
                    event.sequence
                )));
            },
            Ok(ApplyOutcome::IdempotentReplay { .. }) => {
                return Err(ThinkingMapStoreError::Corrupt(format!(
                    "replay divergence at seq {} (unexpected idempotent replay)",
                    event.sequence
                )));
            },
            Err(error) => {
                return Err(ThinkingMapStoreError::Corrupt(format!(
                    "replay divergence at seq {}: {error}",
                    event.sequence
                )));
            },
        }
    }
    Ok(acc)
}

fn replay_recorded_event(acc: &ThinkingMap, event: &MapEvent) -> Option<ThinkingMap> {
    match apply_envelope(acc, &event.envelope, &event.applied_at) {
        Ok(ApplyOutcome::Applied {
            map,
            resulting_revision,
            semantic_hash,
        }) if resulting_revision == event.resulting_revision
            && semantic_hash == event.semantic_hash =>
        {
            Some(map)
        },
        _ => None,
    }
}

fn duplicate_group_is_one_idempotent_operation(group: &[MapEvent]) -> bool {
    let Some(first) = group.first() else {
        return false;
    };
    let shared_nonempty_idempotency_key = !first.envelope.idempotency_key.trim().is_empty()
        && group
            .iter()
            .all(|event| event.envelope.idempotency_key == first.envelope.idempotency_key);
    let shared_envelope_id = !first.envelope.envelope_id.trim().is_empty()
        && group
            .iter()
            .all(|event| event.envelope.envelope_id == first.envelope.envelope_id);
    let shares_identity = shared_envelope_id || shared_nonempty_idempotency_key;
    shares_identity
        && group.iter().all(|event| {
            event.envelope.map_id == first.envelope.map_id
                && event.envelope.base_revision == first.envelope.base_revision
                && event.envelope.operations == first.envelope.operations
                && event.resulting_revision == first.resulting_revision
        })
}

fn resolve_duplicate_sequence(
    manifest: &MapManifest,
    snapshot: Option<&ThinkingMap>,
    events: Vec<MapEvent>,
) -> StoreResult<(Vec<MapEvent>, u64)> {
    if !events
        .windows(2)
        .any(|pair| pair[0].sequence == pair[1].sequence)
    {
        return Ok((events, 0));
    }

    // A stale manifest can cause more than one concurrent request to mint the
    // same sequence, and the same map can contain several such groups. Fold
    // all groups in one pass. Distinct states remain separate until a later
    // recorded semantic hash disambiguates them; branches that converge to the
    // same exact map retain the earliest physical append.
    const MAX_REPLAY_BRANCHES: usize = 256;
    let seed = ThinkingMap::new(
        manifest.map_id.clone(),
        manifest.principal.clone(),
        manifest.workspace.clone(),
        manifest.title.clone(),
        manifest.source.clone(),
        manifest.created_at.clone(),
    );
    // Each branch carries whether it passed through the persisted snapshot's
    // exact state at the snapshot's revision. That snapshot is the state the
    // map actually served, so when the history stays ambiguous at the end, a
    // branch that never produced it cannot be the one users saw.
    let mut branches = vec![(seed, Vec::<MapEvent>::new(), false)];
    let mut removed = 0_u64;
    let mut cursor = 0_usize;
    let mut expected_sequence = 1_u64;

    while cursor < events.len() {
        let sequence = events[cursor].sequence;
        if sequence != expected_sequence {
            return Err(ThinkingMapStoreError::Corrupt(format!(
                "event sequence gap: expected {expected_sequence}, found {sequence} at position {cursor}"
            )));
        }
        let group_end = events[cursor..]
            .iter()
            .position(|event| event.sequence != sequence)
            .map_or(events.len(), |offset| cursor + offset);
        let group = &events[cursor..group_end];
        if group.len() > 1 && !duplicate_group_is_one_idempotent_operation(group) {
            return Err(ThinkingMapStoreError::Corrupt(format!(
                "duplicate sequence {sequence} contains divergent operations"
            )));
        }

        let mut next = Vec::new();
        for (map, chosen, matched_snapshot) in branches {
            for event in group {
                let Some(result) = replay_recorded_event(&map, event) else {
                    continue;
                };
                // If two physical duplicates produce the same state, later
                // history cannot distinguish them. Keep the first append.
                if next
                    .iter()
                    .any(|(existing, _, _): &(ThinkingMap, Vec<MapEvent>, bool)| {
                        existing == &result
                    })
                {
                    continue;
                }
                let mut candidate = chosen.clone();
                candidate.push(event.clone());
                let matched_here = matched_snapshot
                    || snapshot.is_some_and(|persisted| {
                        persisted.revision == result.revision && persisted == &result
                    });
                next.push((result, candidate, matched_here));
                if next.len() > MAX_REPLAY_BRANCHES {
                    return Err(ThinkingMapStoreError::Corrupt(format!(
                        "duplicate sequence repair exceeded {MAX_REPLAY_BRANCHES} replay branches"
                    )));
                }
            }
        }
        if next.is_empty() {
            return Err(ThinkingMapStoreError::Corrupt(format!(
                "event sequence {sequence} has no replay-valid branch"
            )));
        }
        branches = next;
        removed += (group.len() - 1) as u64;
        cursor = group_end;
        expected_sequence += 1;
    }

    match branches.len() {
        1 => Ok((branches.pop().expect("one replay branch").1, removed)),
        total => {
            let mut through_snapshot = branches
                .iter()
                .filter(|(_, _, matched_snapshot)| *matched_snapshot);
            match (through_snapshot.next(), through_snapshot.next()) {
                // Exactly one branch produced the served snapshot: keep it.
                (Some((_, chosen, _)), None) => Ok((chosen.clone(), removed)),
                _ => Err(ThinkingMapStoreError::Corrupt(format!(
                    "{AMBIGUOUS_HISTORY_REASON} ({total})"
                ))),
            }
        },
    }
}

/// Reason text shared by the resolver and the sweep so the sweep recognizes
/// the one failure it quarantines instead of retrying.
const AMBIGUOUS_HISTORY_REASON: &str =
    "duplicate event history has multiple replay-valid terminal branches";
/// Marker file written beside a quarantined map's log.
const AMBIGUOUS_HISTORY_MARKER: &str = "repair-quarantine.json";

impl ThinkingMapStore {
    /// Sits beside `events.jsonl`; its presence means an earlier boot proved
    /// the log ambiguous and gave up on it.
    fn ambiguous_history_marker_path(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> std::path::PathBuf {
        self.events_path(principal, workspace, map_id)
            .with_file_name(AMBIGUOUS_HISTORY_MARKER)
    }

    // ── Replay ────────────────────────────────────────────────────────────────

    /// Deterministically reconstruct the map as of `target_sequence` by folding
    /// the stored envelopes through the reducer from the revision-0 base.
    ///
    /// This is the deterministic replay core; its per-step hash + revision check
    /// IS the byte-equivalence guarantee. `target_sequence == 0` returns the
    /// reconstructed base; a `target_sequence` beyond the last event applies all
    /// events.
    pub async fn replay_to_sequence(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
        target_sequence: u64,
    ) -> StoreResult<ThinkingMap> {
        self.validate_ids(principal, workspace, map_id)?;

        let manifest = self
            .read_manifest(principal, workspace, map_id)
            .await?
            .ok_or_else(|| ThinkingMapStoreError::NotFound(map_id.to_string()))?;

        // `events_after(_, 0)` returns every event, ascending by sequence.
        let events = self.events_after(principal, workspace, map_id, 0).await?;
        let acc = replay_events_from_manifest(&manifest, &events, target_sequence)?;

        // Invariant guard: the reconstructed base must reproduce revision-0. If a
        // future op ever mutates title/source/lifecycle/created_at, folding from
        // this base would diverge — catch the base itself here.
        debug_assert_eq!(
            {
                let base = ThinkingMap::new(
                    manifest.map_id.clone(),
                    manifest.principal.clone(),
                    manifest.workspace.clone(),
                    manifest.title.clone(),
                    manifest.source.clone(),
                    manifest.created_at.clone(),
                );
                base.revision
            },
            0,
            "reconstructed base must be revision 0"
        );

        Ok(acc)
    }

    /// Replay to the highest-sequence event whose envelope carries
    /// `utterance_id == Some(utterance_id)`. Unknown utterance ⇒ `Ok(None)`.
    pub async fn replay_to_utterance(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
        utterance_id: &str,
    ) -> StoreResult<Option<ThinkingMap>> {
        self.validate_ids(principal, workspace, map_id)?;
        // Ensure the map exists (fail-closed NotFound, not silent None).
        if self
            .read_manifest(principal, workspace, map_id)
            .await?
            .is_none()
        {
            return Err(ThinkingMapStoreError::NotFound(map_id.to_string()));
        }

        let events = self.events_after(principal, workspace, map_id, 0).await?;
        let target = events
            .iter()
            .filter(|e| e.envelope.utterance_id.as_deref() == Some(utterance_id))
            .map(|e| e.sequence)
            .max();

        match target {
            None => Ok(None),
            Some(seq) => Ok(Some(
                self.replay_to_sequence(principal, workspace, map_id, seq)
                    .await?,
            )),
        }
    }

    /// Replay to the highest-sequence event whose `applied_at <=
    /// at_or_before_rfc3339`. No qualifying event ⇒ the revision-0 base.
    ///
    /// NOTE: uses lexicographic string comparison on `applied_at`, which is
    /// correct only for RFC3339 timestamps normalized to a single (Z) offset with
    /// consistent field widths — the format the reducer/store already emit.
    pub async fn replay_to_time(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
        at_or_before_rfc3339: &str,
    ) -> StoreResult<ThinkingMap> {
        self.validate_ids(principal, workspace, map_id)?;
        // read_manifest via replay_to_sequence also guards NotFound.
        let events = self.events_after(principal, workspace, map_id, 0).await?;
        let target = events
            .iter()
            .filter(|e| e.applied_at.as_str() <= at_or_before_rfc3339)
            .map(|e| e.sequence)
            .max()
            .unwrap_or(0);
        self.replay_to_sequence(principal, workspace, map_id, target)
            .await
    }

    // ── Restore as a new branch (never rewrite history) ───────────────────────

    /// Fork a NEW map (`new_map_id`) from `source_map_id` as of `at_sequence`.
    ///
    /// The replayed node/edge/clarification/proposal/view state is carried into
    /// the branch at the same `revision`; the branch starts a fresh, empty
    /// idempotency ledger and records `branched_from_*` provenance in its
    /// manifest. The SOURCE map and its event log are left completely untouched.
    /// Fails `AlreadyExists` if `new_map_id` is taken.
    #[allow(clippy::too_many_arguments)]
    pub async fn restore_as_branch(
        &self,
        principal: &str,
        workspace: &str,
        source_map_id: &str,
        at_sequence: u64,
        new_map_id: &str,
        new_title: &str,
        restored_at: &str,
    ) -> StoreResult<ThinkingMap> {
        self.validate_ids(principal, workspace, source_map_id)?;
        self.validate_ids(principal, workspace, new_map_id)?;

        // Replay the source (read-only) to the requested sequence.
        let replayed = self
            .replay_to_sequence(principal, workspace, source_map_id, at_sequence)
            .await?;

        // Build the branch map: same scope/source, new id/title, carried content,
        // same revision, fresh idempotency ledger, restored timestamps.
        let mut branch = ThinkingMap::new(
            new_map_id.to_string(),
            principal,
            workspace,
            new_title,
            replayed.source.clone(),
            restored_at,
        );
        branch.schema_version = replayed.schema_version;
        branch.revision = replayed.revision;
        branch.view_state = replayed.view_state.clone();
        branch.nodes = replayed.nodes.clone();
        branch.edges = replayed.edges.clone();
        branch.clarifications = replayed.clarifications.clone();
        branch.proposals = replayed.proposals.clone();
        // applied_envelopes intentionally left empty (fresh ledger).
        branch.created_at = restored_at.to_string();
        branch.updated_at = restored_at.to_string();

        // Persist the branch (fails AlreadyExists if taken). This writes an
        // events.jsonl, snapshot, and a manifest WITHOUT provenance.
        self.create_map(&branch).await?;

        // Stamp branch provenance into the manifest (under the branch's lock).
        let lock = self.map_lock(principal, workspace, new_map_id);
        let _guard = lock.lock().await;
        let mut manifest = self
            .read_manifest(principal, workspace, new_map_id)
            .await?
            .ok_or_else(|| ThinkingMapStoreError::NotFound(new_map_id.to_string()))?;
        manifest.branched_from_map_id = Some(source_map_id.to_string());
        manifest.branched_from_sequence = Some(at_sequence);
        let manifest_path = self.manifest_path(principal, workspace, new_map_id);
        self.workspace()
            .write_json_atomic_path(&manifest_path, &manifest)
            .await?;

        Ok(branch)
    }

    // ── Startup repair (roll-forward only; never fail-open) ────────────────────

    /// Reconcile the snapshot/manifest with the authoritative event log after a
    /// crash. Roll-forward only: committed events are never rolled back, dropped,
    /// or truncated to match a stale snapshot. A truncated/partial FINAL log line
    /// is quarantined; a parse error on any interior line, a sequence gap, or a
    /// snapshot AHEAD of the log is `Corrupt`.
    ///
    /// ## Concurrency
    /// The process-wide per-map lock serializes this repair with every
    /// `ThinkingMapStore` instance over the same normalized storage root, which
    /// covers the short-lived stores built by REST handlers. It cannot exclude
    /// a second OS process pointed at the same files, so before the first write
    /// the raw log is also re-read and compared with the bytes used to make the
    /// repair decision. If it moved, nothing is written and the report says so.
    /// Startup still completes this sweep before the map accepts traffic; the
    /// byte comparison is defense in depth for accidental multi-process access.
    pub async fn startup_repair(
        &self,
        principal: &str,
        workspace: &str,
        map_id: &str,
    ) -> StoreResult<RepairReport> {
        self.validate_ids(principal, workspace, map_id)?;
        let lock = self.map_lock(principal, workspace, map_id);
        let _guard = lock.lock().await;

        // Read the raw log so we can distinguish a truncated FINAL line (quarantine)
        // from interior corruption (Corrupt).
        let events_path = self.events_path(principal, workspace, map_id);
        let raw = match self.workspace().read_to_string_path(&events_path).await {
            Ok(body) => body,
            Err(magician::magician_v2::artifact_v2::ArtifactV2Error::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                String::new()
            },
            Err(err) => return Err(ThinkingMapStoreError::Io(err)),
        };

        // Collect non-empty (trimmed) lines with their original index.
        let non_empty: Vec<(usize, &str)> = raw
            .lines()
            .enumerate()
            .filter_map(|(idx, l)| {
                let t = l.trim();
                if t.is_empty() {
                    None
                } else {
                    Some((idx, t))
                }
            })
            .collect();

        let mut quarantined_trailing_bytes = false;
        let mut good: Vec<MapEvent> = Vec::with_capacity(non_empty.len());

        for (pos, (idx, line)) in non_empty.iter().enumerate() {
            let is_final = pos + 1 == non_empty.len();
            match serde_json::from_str::<MapEvent>(line) {
                Ok(event) => good.push(event),
                Err(err) => {
                    if is_final {
                        // Truncated/partial trailing record — quarantine it.
                        quarantined_trailing_bytes = true;
                    } else {
                        // Interior corruption — never silently drop committed data.
                        return Err(ThinkingMapStoreError::Corrupt(format!(
                            "{} line {}: {err}",
                            events_path.display(),
                            idx + 1
                        )));
                    }
                },
            }
        }

        // Current snapshot/manifest state. The manifest is also the immutable
        // revision-0 seed used to prove a duplicate branch before touching the
        // log.
        let snapshot = self.read_snapshot(principal, workspace, map_id).await?;
        let manifest = self.read_manifest(principal, workspace, map_id).await?;
        let mut deduplicated_events = 0_u64;
        if good
            .windows(2)
            .any(|pair| pair[0].sequence == pair[1].sequence)
        {
            let seed = manifest.as_ref().ok_or_else(|| {
                ThinkingMapStoreError::Corrupt(
                    "duplicate event sequence cannot be proven without a manifest".to_string(),
                )
            })?;
            (good, deduplicated_events) =
                resolve_duplicate_sequence(seed, snapshot.as_ref(), good)?;
        }

        // Sequences must be contiguous 1,2,3,… — a gap means missing committed data.
        for (i, event) in good.iter().enumerate() {
            let expected = (i as u64) + 1;
            if event.sequence != expected {
                return Err(ThinkingMapStoreError::Corrupt(format!(
                    "event sequence gap: expected {expected}, found {} at position {i}",
                    event.sequence
                )));
            }
        }

        let last_good = good.last().cloned();

        let snapshot_revision = snapshot.as_ref().map(|m| m.revision).unwrap_or(0);

        // Compute the log's authoritative head.
        let (log_revision, log_sequence) = match &last_good {
            Some(ev) => (ev.resulting_revision, ev.sequence),
            None => (0, 0),
        };

        // Snapshot AHEAD of the log is impossible under 2a ordering (log leads).
        // Never silently accept it.
        if snapshot_revision > log_revision {
            return Err(ThinkingMapStoreError::Corrupt(format!(
                "snapshot ahead of log: snapshot revision {snapshot_revision} > log revision {log_revision}"
            )));
        }

        // Already consistent: snapshot matches the log head AND manifest agrees.
        // No writes. (When the log is empty and snapshot is revision 0, this
        // holds.) Checked BEFORE the quarantine rewrite even though the rewrite
        // used to come first: a quarantine always falsifies this condition, so
        // the order is behaviour-identical, and it keeps the clean-map path —
        // the overwhelmingly common one at boot — at zero extra I/O.
        let manifest_sequence = manifest.as_ref().map(|m| m.latest_sequence).unwrap_or(0);
        if snapshot_revision == log_revision
            && manifest_sequence == log_sequence
            && !quarantined_trailing_bytes
            && deduplicated_events == 0
        {
            return Ok(RepairReport {
                repaired: false,
                quarantined_trailing_bytes: false,
                deduplicated_events: 0,
                skipped_concurrent_write: false,
                from_revision: snapshot_revision,
                to_revision: snapshot_revision,
                replayed_events: 0,
            });
        }

        // Everything below writes. Re-read the log and confirm it is still
        // byte-identical to what the decision above was made from. A live
        // `apply_and_persist` that appended while we were parsing/reading would
        // otherwise lose its event to the quarantine rewrite, or have its
        // snapshot rolled back to our older head. Leave the map alone instead —
        // an unrepaired map is recoverable on the next boot, a clobbered log is
        // not.
        let still_current = match self.workspace().read_to_string_path(&events_path).await {
            Ok(body) => body == raw,
            Err(magician::magician_v2::artifact_v2::ArtifactV2Error::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                raw.is_empty()
            },
            // Unreadable on the second pass ⇒ treat as moved, not as clean.
            Err(_) => false,
        };
        if !still_current {
            return Ok(RepairReport {
                repaired: false,
                quarantined_trailing_bytes: false,
                deduplicated_events: 0,
                skipped_concurrent_write: true,
                from_revision: snapshot_revision,
                to_revision: snapshot_revision,
                replayed_events: 0,
            });
        }

        // Rewrite only when a trailing fragment was quarantined or replay
        // proved one or more duplicate append groups. Arbitrary interior
        // corruption never reaches this point.
        if quarantined_trailing_bytes || deduplicated_events > 0 {
            let mut body = String::new();
            for event in &good {
                body.push_str(&serde_json::to_string(event).map_err(|err| {
                    ThinkingMapStoreError::Io(
                        magician::magician_v2::artifact_v2::ArtifactV2Error::from(err),
                    )
                })?);
                body.push('\n');
            }
            self.workspace()
                .write_string_atomic_path(&events_path, &body)
                .await?;
        }

        // Snapshot/manifest are BEHIND the log (the 2a crash window) or the
        // manifest sequence drifted — roll FORWARD by replaying to the last good
        // event, then rewrite snapshot then manifest to match.
        //
        // If the log is empty (no good events) there is nothing to roll forward
        // to; the only way here is a quarantined-only empty log, which is already
        // consistent at revision 0 (snapshot must be 0 given the ahead check).
        let (rolled_snapshot, to_revision, replayed_events) = match &last_good {
            Some(ev) => {
                let replayed = self
                    .replay_to_sequence(principal, workspace, map_id, ev.sequence)
                    .await?;
                let replayed_count = good.len() as u64;
                (replayed, ev.resulting_revision, replayed_count)
            },
            None => {
                // Empty log after quarantine: reconstruct the revision-0 base.
                let replayed = self
                    .replay_to_sequence(principal, workspace, map_id, 0)
                    .await?;
                (replayed, 0, 0)
            },
        };

        // Nothing to write if snapshot already equals the target and manifest
        // agrees and we did not quarantine (covered above), so here we always
        // (re)write to reconcile.
        let snapshot_path = self.snapshot_path(principal, workspace, map_id);
        self.workspace()
            .write_json_atomic_path(&snapshot_path, &rolled_snapshot)
            .await?;

        let mut new_manifest = Self::manifest_from_map(
            &rolled_snapshot,
            log_sequence,
            semantic_hash(&rolled_snapshot),
        );
        // `manifest_from_map` builds a fresh head record from the map and knows
        // nothing about branch provenance, which lives only in the manifest.
        // Carry it across: rolling a branch forward must not erase what it was
        // forked from. (Lifecycle and title need no special handling — both are
        // event-sourced through the reducer and in the semantic hash, so the
        // replayed snapshot already carries the right values.)
        if let Some(previous) = manifest.as_ref() {
            new_manifest
                .branched_from_map_id
                .clone_from(&previous.branched_from_map_id);
            new_manifest.branched_from_sequence = previous.branched_from_sequence;
        }
        let manifest_path = self.manifest_path(principal, workspace, map_id);
        self.workspace()
            .write_json_atomic_path(&manifest_path, &new_manifest)
            .await?;

        Ok(RepairReport {
            repaired: true,
            quarantined_trailing_bytes,
            deduplicated_events,
            skipped_concurrent_write: false,
            from_revision: snapshot_revision,
            to_revision,
            replayed_events,
        })
    }

    /// Run [`Self::startup_repair`] over every map in every scope on disk.
    ///
    /// This is the boot entry point. Without it `startup_repair` never runs,
    /// and a crash inside `apply_and_persist`'s append→snapshot→manifest window
    /// is permanent: a torn trailing line makes `events_after` fail forever for
    /// that map (taking replay, restore-as-branch, and the events endpoint with
    /// it), and a stale `manifest.latest_sequence` makes the next apply mint a
    /// DUPLICATE sequence number.
    ///
    /// One damaged map never stops the sweep. A map that fails is logged with
    /// its scope and counted; the pass continues to the next map, and a scope
    /// whose directory will not enumerate is counted too rather than vanishing
    /// from the tally. Nothing here returns `Err`: a boot reconciler that
    /// aborted on the first bad map would leave every map after it unrepaired,
    /// which is the failure mode this exists to prevent.
    pub async fn startup_repair_all(&self) -> StartupRepairSweep {
        let mut sweep = StartupRepairSweep::default();

        let scopes = match self.workspace().list_scope_segments().await {
            Ok(scopes) => scopes,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "[THINKING-MAP] startup repair could not enumerate scopes; no maps were checked"
                );
                return sweep;
            },
        };

        for (principal, workspace) in scopes {
            sweep.scopes_scanned += 1;
            let map_ids = match self.list_map_ids(&principal, &workspace).await {
                Ok(ids) => ids,
                Err(error) => {
                    sweep.scopes_unreadable += 1;
                    tracing::warn!(
                        principal = %principal,
                        workspace = %workspace,
                        error = %error,
                        "[THINKING-MAP] startup repair could not enumerate maps for scope; \
                         its maps were not checked"
                    );
                    continue;
                },
            };

            for map_id in map_ids {
                sweep.maps_scanned += 1;
                let marker = self.ambiguous_history_marker_path(&principal, &workspace, &map_id);
                if self.workspace().read_to_string_path(&marker).await.is_ok() {
                    sweep.maps_quarantined_ambiguous += 1;
                    tracing::info!(
                        principal = %principal,
                        workspace = %workspace,
                        map_id = %map_id,
                        marker = %marker.display(),
                        "[THINKING-MAP] map was quarantined on an earlier boot for ambiguous duplicate history; not retried"
                    );
                    continue;
                }
                match self.startup_repair(&principal, &workspace, &map_id).await {
                    Ok(report) => {
                        if report.quarantined_trailing_bytes {
                            sweep.maps_quarantined += 1;
                        }
                        if report.deduplicated_events > 0 {
                            sweep.maps_deduplicated += 1;
                        }
                        if report.skipped_concurrent_write {
                            sweep.maps_skipped_concurrent_write += 1;
                            tracing::warn!(
                                principal = %principal,
                                workspace = %workspace,
                                map_id = %map_id,
                                "[THINKING-MAP] startup repair skipped a map whose log changed \
                                 mid-repair; it will be reconciled on the next boot"
                            );
                        }
                        if report.repaired {
                            sweep.maps_repaired += 1;
                            tracing::info!(
                                principal = %principal,
                                workspace = %workspace,
                                map_id = %map_id,
                                quarantined_trailing_bytes = report.quarantined_trailing_bytes,
                                deduplicated_events = report.deduplicated_events,
                                from_revision = report.from_revision,
                                to_revision = report.to_revision,
                                replayed_events = report.replayed_events,
                                "[THINKING-MAP] startup repair reconciled a map"
                            );
                        }
                    },
                    Err(ThinkingMapStoreError::Corrupt(reason))
                        if reason.contains(AMBIGUOUS_HISTORY_REASON) =>
                    {
                        // Nothing later can disambiguate this log, so a retry
                        // on every boot only repeats the ERROR. Record it once
                        // and stop looking; the snapshot keeps serving reads.
                        let recorded_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|elapsed| elapsed.as_secs())
                            .unwrap_or(0);
                        let record = serde_json::json!({
                            "reason": reason,
                            "recorded_at_unix": recorded_at,
                            "note": "event log kept as-is; snapshot serves reads; delete this marker to retry repair",
                        });
                        match self
                            .workspace()
                            .write_json_atomic_path(&marker, &record)
                            .await
                        {
                            Ok(()) => {
                                sweep.maps_quarantined_ambiguous += 1;
                                tracing::warn!(
                                    principal = %principal,
                                    workspace = %workspace,
                                    map_id = %map_id,
                                    marker = %marker.display(),
                                    reason = %reason,
                                    "[THINKING-MAP] quarantined a map with ambiguous duplicate history; its log will not replay and the sweep will not retry it"
                                );
                            },
                            Err(error) => {
                                sweep.maps_failed += 1;
                                tracing::error!(
                                    principal = %principal,
                                    workspace = %workspace,
                                    map_id = %map_id,
                                    error = %error,
                                    "[THINKING-MAP] could not write the quarantine marker; the map will be retried next boot"
                                );
                            },
                        }
                    },
                    Err(error) => {
                        sweep.maps_failed += 1;
                        tracing::error!(
                            principal = %principal,
                            workspace = %workspace,
                            map_id = %map_id,
                            error = %error,
                            "[THINKING-MAP] startup repair FAILED for this map; its event log \
                             still will not replay. Sweep continues."
                        );
                    },
                }
            }
        }

        sweep
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thinking_map::models::{
        AssertionOrigin, EpistemicState, MapLifecycle, NodeKind, ThinkingMap, ThinkingMapSource,
        ThinkingNode,
    };
    use crate::thinking_map::operations::{MapOperation, MapOperationEnvelope, OperationActor};
    use crate::thinking_map::reducer::{apply_envelope, ApplyOutcome};
    use crate::thinking_map::store::MapEvent;
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use tempfile::TempDir;

    const TS: &str = "2026-07-19T00:00:00Z";
    const TS2: &str = "2026-07-19T01:00:00Z";

    fn store() -> (TempDir, ThinkingMapStore) {
        let tmp = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(tmp.path());
        (tmp, ThinkingMapStore::new(workspace))
    }

    fn sample_map(map_id: &str) -> ThinkingMap {
        ThinkingMap::new(
            map_id.to_string(),
            "anonymous",
            "default",
            "Test map",
            ThinkingMapSource::Solo,
            TS,
        )
    }

    fn owner() -> OperationActor {
        OperationActor::Owner {
            principal: "anonymous".to_string(),
        }
    }

    fn node(id: &str) -> ThinkingNode {
        ThinkingNode {
            node_id: id.to_string(),
            kind: NodeKind::Idea,
            label: format!("label-{id}"),
            detail_markdown: None,
            epistemic_state: EpistemicState::Provisional,
            assertion_origin: AssertionOrigin::OwnerSpoken,
            confidence: 0.5,
            speaker: None,
            source_refs: vec![],
            parent_id: None,
            position: None,
            position_locked: false,
            promoted_refs: vec![],
            tombstoned: false,
            created_at: TS.to_string(),
            updated_at: TS.to_string(),
        }
    }

    /// Envelope adding `node_id` against `base_revision`, optionally with an
    /// `utterance_id`.
    fn add_node_env(
        map_id: &str,
        base_revision: u64,
        idem: &str,
        node_id: &str,
        utterance_id: Option<&str>,
    ) -> MapOperationEnvelope {
        let mut env = MapOperationEnvelope::new(
            format!("env-{idem}"),
            map_id.to_string(),
            base_revision,
            owner(),
            idem,
            vec![MapOperation::AddNode {
                node: node(node_id),
            }],
            TS,
        );
        env.utterance_id = utterance_id.map(|u| u.to_string());
        env
    }

    /// Apply three distinct add-node envelopes; return the store (map "m1" seeded).
    async fn seed_three(store: &ThinkingMapStore) {
        store.create_map(&sample_map("m1")).await.expect("create");
        for (rev, (idem, node_id)) in [("i1", "n1"), ("i2", "n2"), ("i3", "n3")]
            .into_iter()
            .enumerate()
        {
            let env = add_node_env("m1", rev as u64, idem, node_id, None);
            store
                .apply_and_persist("anonymous", "default", "m1", &env, TS2)
                .await
                .expect("apply");
        }
    }

    #[tokio::test]
    async fn replay_reproduces_intermediate_states_and_hashes() {
        let (_tmp, store) = store();
        seed_three(&store).await;

        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert_eq!(events.len(), 3);

        // Each replay-to-seq reproduces the stored semantic hash + revision.
        for ev in &events {
            let replayed = store
                .replay_to_sequence("anonymous", "default", "m1", ev.sequence)
                .await
                .unwrap();
            assert_eq!(replayed.revision, ev.resulting_revision);
            assert_eq!(semantic_hash(&replayed), ev.semantic_hash);
        }

        // Replay to the last seq byte-equals the live snapshot.
        let full = store
            .replay_to_sequence("anonymous", "default", "m1", 3)
            .await
            .unwrap();
        let live = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(full, live);

        // Replay to 0 == reconstructed base (revision 0, empty).
        let base = store
            .replay_to_sequence("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert_eq!(base.revision, 0);
        assert!(base.nodes.is_empty());
        assert!(base.edges.is_empty());
        assert_eq!(base, sample_map("m1"));

        // A target beyond the last event applies all events.
        let beyond = store
            .replay_to_sequence("anonymous", "default", "m1", 99)
            .await
            .unwrap();
        assert_eq!(beyond, live);
    }

    #[tokio::test]
    async fn replay_missing_map_is_not_found() {
        let (_tmp, store) = store();
        let err = store
            .replay_to_sequence("anonymous", "default", "ghost", 1)
            .await
            .expect_err("missing");
        assert!(matches!(err, ThinkingMapStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn replay_divergence_on_tampered_hash_is_corrupt() {
        let (_tmp, store) = store();
        seed_three(&store).await;

        // Corrupt event #2's stored semantic_hash on disk.
        let events_path = store.events_path("anonymous", "default", "m1");
        let body = store
            .workspace()
            .read_to_string_path(&events_path)
            .await
            .unwrap();
        let mut lines: Vec<MapEvent> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        lines[1].semantic_hash = "deadbeef".to_string();
        let mut rewritten = String::new();
        for ev in &lines {
            rewritten.push_str(&serde_json::to_string(ev).unwrap());
            rewritten.push('\n');
        }
        store
            .workspace()
            .write_string_atomic_path(&events_path, &rewritten)
            .await
            .unwrap();

        let err = store
            .replay_to_sequence("anonymous", "default", "m1", 3)
            .await
            .expect_err("should detect divergence");
        match err {
            ThinkingMapStoreError::Corrupt(msg) => {
                assert!(msg.contains("replay divergence at seq 2"), "msg was {msg}");
            },
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_to_utterance_lands_on_right_sequence() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();

        // seq 1 with utterance u1, seq 2 with utterance u2.
        let e1 = add_node_env("m1", 0, "i1", "n1", Some("u1"));
        store
            .apply_and_persist("anonymous", "default", "m1", &e1, TS2)
            .await
            .unwrap();
        let e2 = add_node_env("m1", 1, "i2", "n2", Some("u2"));
        store
            .apply_and_persist("anonymous", "default", "m1", &e2, TS2)
            .await
            .unwrap();

        let at_u1 = store
            .replay_to_utterance("anonymous", "default", "m1", "u1")
            .await
            .unwrap()
            .expect("u1 present");
        assert_eq!(at_u1.revision, 1);
        assert!(at_u1.nodes.contains_key("n1"));
        assert!(!at_u1.nodes.contains_key("n2"));

        let at_u2 = store
            .replay_to_utterance("anonymous", "default", "m1", "u2")
            .await
            .unwrap()
            .expect("u2 present");
        assert_eq!(at_u2.revision, 2);
        assert!(at_u2.nodes.contains_key("n2"));

        // Unknown utterance ⇒ Ok(None).
        let none = store
            .replay_to_utterance("anonymous", "default", "m1", "nope")
            .await
            .unwrap();
        assert!(none.is_none());
    }

    #[tokio::test]
    async fn replay_to_time_selects_last_at_or_before() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();

        // seq1 applied at T1, seq2 applied at T2 (> T1).
        let t1 = "2026-07-19T02:00:00Z";
        let t2 = "2026-07-19T03:00:00Z";
        let e1 = add_node_env("m1", 0, "i1", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &e1, t1)
            .await
            .unwrap();
        let e2 = add_node_env("m1", 1, "i2", "n2", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &e2, t2)
            .await
            .unwrap();

        // At/before T1 ⇒ only seq1.
        let at_t1 = store
            .replay_to_time("anonymous", "default", "m1", t1)
            .await
            .unwrap();
        assert_eq!(at_t1.revision, 1);
        // Before everything ⇒ base.
        let before = store
            .replay_to_time("anonymous", "default", "m1", "2026-07-19T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(before.revision, 0);
        // At/after T2 ⇒ both.
        let at_t2 = store
            .replay_to_time("anonymous", "default", "m1", t2)
            .await
            .unwrap();
        assert_eq!(at_t2.revision, 2);
    }

    #[tokio::test]
    async fn diff_maps_add_remove_change() {
        let (_tmp, store) = store();
        seed_three(&store).await;

        let s1 = store
            .replay_to_sequence("anonymous", "default", "m1", 1)
            .await
            .unwrap();
        let s2 = store
            .replay_to_sequence("anonymous", "default", "m1", 2)
            .await
            .unwrap();

        // s1 has n1; s2 has n1+n2 ⇒ n2 added.
        let diff = diff_maps(&s1, &s2);
        assert_eq!(diff.added_nodes, vec!["n2".to_string()]);
        assert!(diff.removed_nodes.is_empty());
        assert!(diff.changed_nodes.is_empty());

        // Reverse ⇒ n2 removed.
        let rev = diff_maps(&s2, &s1);
        assert_eq!(rev.removed_nodes, vec!["n2".to_string()]);

        // Changed: mutate n1's label between two hand-built maps.
        let mut a = sample_map("x");
        a.nodes.insert("n1".to_string(), node("n1"));
        let mut b = a.clone();
        b.nodes.get_mut("n1").unwrap().label = "changed".to_string();
        let d = diff_maps(&a, &b);
        assert_eq!(d.changed_nodes, vec!["n1".to_string()]);
        assert!(d.added_nodes.is_empty());
    }

    #[tokio::test]
    async fn restore_as_branch_carries_state_and_leaves_source_untouched() {
        let (_tmp, store) = store();
        seed_three(&store).await;

        // Snapshot the source's on-disk bytes before restore.
        let src_events_path = store.events_path("anonymous", "default", "m1");
        let src_snapshot_path = store.snapshot_path("anonymous", "default", "m1");
        let src_manifest_path = store.manifest_path("anonymous", "default", "m1");
        let src_events_before = store
            .workspace()
            .read_to_string_path(&src_events_path)
            .await
            .unwrap();
        let src_snapshot_before = store
            .workspace()
            .read_to_string_path(&src_snapshot_path)
            .await
            .unwrap();
        let src_manifest_before = store
            .workspace()
            .read_to_string_path(&src_manifest_path)
            .await
            .unwrap();

        // Restore at seq 2 into a new map id.
        let branch = store
            .restore_as_branch(
                "anonymous",
                "default",
                "m1",
                2,
                "branch1",
                "Forked at seq 2",
                "2026-07-19T05:00:00Z",
            )
            .await
            .unwrap();
        assert_eq!(branch.map_id, "branch1");
        assert_eq!(branch.title, "Forked at seq 2");
        assert_eq!(branch.revision, 2);
        assert!(branch.nodes.contains_key("n1"));
        assert!(branch.nodes.contains_key("n2"));
        assert!(!branch.nodes.contains_key("n3"));
        assert!(branch.applied_envelopes.is_empty());

        // The persisted branch equals the seq-2 replay content.
        let loaded_branch = store
            .load_map("anonymous", "default", "branch1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded_branch, branch);

        // Branch manifest records provenance.
        let branch_manifest = store
            .read_manifest("anonymous", "default", "branch1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(branch_manifest.branched_from_map_id, Some("m1".to_string()));
        assert_eq!(branch_manifest.branched_from_sequence, Some(2));

        // Source completely untouched (byte-identical).
        let src_events_after = store
            .workspace()
            .read_to_string_path(&src_events_path)
            .await
            .unwrap();
        let src_snapshot_after = store
            .workspace()
            .read_to_string_path(&src_snapshot_path)
            .await
            .unwrap();
        let src_manifest_after = store
            .workspace()
            .read_to_string_path(&src_manifest_path)
            .await
            .unwrap();
        assert_eq!(src_events_before, src_events_after);
        assert_eq!(src_snapshot_before, src_snapshot_after);
        assert_eq!(src_manifest_before, src_manifest_after);

        // Applying a new op to the branch does not affect the source, and vice versa.
        let benv = add_node_env("branch1", 2, "b1", "bnode", None);
        store
            .apply_and_persist("anonymous", "default", "branch1", &benv, TS2)
            .await
            .unwrap();
        let src_after_branch_op = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert!(!src_after_branch_op.nodes.contains_key("bnode"));
        assert_eq!(src_after_branch_op.revision, 3);
    }

    #[tokio::test]
    async fn restore_as_branch_existing_id_is_already_exists() {
        let (_tmp, store) = store();
        seed_three(&store).await;
        // Restore into an id that already exists (m1 itself).
        let err = store
            .restore_as_branch("anonymous", "default", "m1", 2, "m1", "dup", TS2)
            .await
            .expect_err("dup id");
        assert!(matches!(err, ThinkingMapStoreError::AlreadyExists(_)));
    }

    /// Compute the MapEvent that WOULD be produced by applying `env` to the map
    /// at `snapshot`, for simulating a crash-window extra append.
    fn synth_event(
        snapshot: &ThinkingMap,
        env: &MapOperationEnvelope,
        sequence: u64,
        applied_at: &str,
    ) -> MapEvent {
        match apply_envelope(snapshot, env, applied_at).unwrap() {
            ApplyOutcome::Applied {
                resulting_revision,
                semantic_hash,
                ..
            } => MapEvent {
                sequence,
                envelope: env.clone(),
                resulting_revision,
                semantic_hash,
                applied_at: applied_at.to_string(),
            },
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn startup_repair_rolls_forward_crash_window() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let e1 = add_node_env("m1", 0, "i1", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &e1, TS2)
            .await
            .unwrap();

        // Simulate the 2a crash: append ONE more valid event WITHOUT updating
        // snapshot/manifest.
        let snapshot = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        let e2 = add_node_env("m1", 1, "i2", "n2", None);
        let extra = synth_event(&snapshot, &e2, 2, "2026-07-19T02:00:00Z");
        let events_path = store.events_path("anonymous", "default", "m1");
        store
            .workspace()
            .append_jsonl_path(&events_path, &extra)
            .await
            .unwrap();

        // Snapshot still at revision 1 before repair.
        let before = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.revision, 1);

        let report = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .unwrap();
        assert!(report.repaired);
        assert!(!report.quarantined_trailing_bytes);
        assert_eq!(report.from_revision, 1);
        assert_eq!(report.to_revision, 2);
        assert_eq!(report.replayed_events, 2);

        // Snapshot + manifest now reflect the extra event.
        let after = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.revision, 2);
        assert!(after.nodes.contains_key("n2"));
        let manifest = store
            .read_manifest("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(manifest.latest_sequence, 2);
        assert_eq!(manifest.latest_revision, 2);

        // Idempotent: a second repair is a no-op.
        let report2 = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .unwrap();
        assert!(!report2.repaired);
        assert_eq!(report2.to_revision, 2);
    }

    #[tokio::test]
    async fn startup_repair_keeps_the_only_replay_valid_idempotent_duplicate() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let first = add_node_env("m1", 0, "first", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &first, TS2)
            .await
            .unwrap();

        let revision_one = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        let duplicate_envelope = add_node_env("m1", 1, "duplicate", "n2", None);
        let stale_duplicate = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:00:00Z",
        );
        let live_duplicate = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:01:00Z",
        );
        assert_ne!(stale_duplicate.semantic_hash, live_duplicate.semantic_hash);
        let revision_two =
            match apply_envelope(&revision_one, &duplicate_envelope, "2026-07-19T02:01:00Z")
                .unwrap()
            {
                ApplyOutcome::Applied { map, .. } => map,
                other => panic!("expected applied duplicate branch, got {other:?}"),
            };
        let third_envelope = add_node_env("m1", 2, "third", "n3", None);
        let third = synth_event(&revision_two, &third_envelope, 3, "2026-07-19T03:00:00Z");
        let events_path = store.events_path("anonymous", "default", "m1");
        for event in [&stale_duplicate, &live_duplicate, &third] {
            store
                .workspace()
                .append_jsonl_path(&events_path, event)
                .await
                .unwrap();
        }

        let report = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .expect("provable duplicate is repairable");
        assert!(report.repaired);
        assert_eq!(report.deduplicated_events, 1);
        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(events[1].semantic_hash, live_duplicate.semantic_hash);
        let repaired = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repaired.revision, 3);
        assert!(repaired.nodes.contains_key("n3"));
    }

    #[tokio::test]
    async fn startup_repair_handles_multiple_duplicate_groups_and_triplicate_appends() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let first = add_node_env("m1", 0, "first", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &first, TS2)
            .await
            .unwrap();

        let revision_one = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        let duplicate_envelope = add_node_env("m1", 1, "duplicate", "n2", None);
        let stale_first = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:00:00Z",
        );
        let surviving = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:01:00Z",
        );
        let stale_second = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:02:00Z",
        );
        let revision_two =
            match apply_envelope(&revision_one, &duplicate_envelope, "2026-07-19T02:01:00Z")
                .unwrap()
            {
                ApplyOutcome::Applied { map, .. } => map,
                other => panic!("expected applied duplicate branch, got {other:?}"),
            };
        let third_envelope = add_node_env("m1", 2, "third", "n3", None);
        let third = synth_event(&revision_two, &third_envelope, 3, "2026-07-19T03:00:00Z");
        let revision_three =
            match apply_envelope(&revision_two, &third_envelope, "2026-07-19T03:00:00Z").unwrap() {
                ApplyOutcome::Applied { map, .. } => map,
                other => panic!("expected applied third event, got {other:?}"),
            };
        let fourth_envelope = add_node_env("m1", 3, "fourth", "n4", None);
        let fourth = synth_event(&revision_three, &fourth_envelope, 4, "2026-07-19T04:00:00Z");
        let events_path = store.events_path("anonymous", "default", "m1");
        for event in [
            &stale_first,
            &surviving,
            &stale_second,
            &third,
            &fourth,
            &fourth,
        ] {
            store
                .workspace()
                .append_jsonl_path(&events_path, event)
                .await
                .unwrap();
        }

        let report = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .expect("multiple provable duplicate groups are repairable");
        assert!(report.repaired);
        assert_eq!(report.deduplicated_events, 3);
        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(events[1].semantic_hash, surviving.semantic_hash);
        assert_eq!(events[3].semantic_hash, fourth.semantic_hash);
    }

    #[tokio::test]
    async fn startup_repair_keeps_the_duplicate_tail_the_served_snapshot_came_from() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let first = add_node_env("m1", 0, "first", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &first, TS2)
            .await
            .unwrap();

        let revision_one = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        let duplicate_envelope = add_node_env("m1", 1, "duplicate", "n2", None);
        let first_tail = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:00:00Z",
        );
        let second_tail = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:01:00Z",
        );
        assert_ne!(first_tail, second_tail);
        let events_path = store.events_path("anonymous", "default", "m1");
        for event in [&first_tail, &second_tail] {
            store
                .workspace()
                .append_jsonl_path(&events_path, event)
                .await
                .unwrap();
        }
        // The map served the FIRST tail's state: persist it as the snapshot.
        let served =
            match apply_envelope(&revision_one, &duplicate_envelope, "2026-07-19T02:00:00Z")
                .unwrap()
            {
                ApplyOutcome::Applied { map, .. } => map,
                other => panic!("expected Applied, got {other:?}"),
            };
        store
            .workspace()
            .write_json_atomic_path(&store.snapshot_path("anonymous", "default", "m1"), &served)
            .await
            .unwrap();

        let report = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .expect("the served snapshot disambiguates the duplicate tail");
        assert_eq!(report.deduplicated_events, 1);
        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].applied_at, first_tail.applied_at);
    }

    #[tokio::test]
    async fn startup_sweep_quarantines_an_ambiguous_map_once_and_stops_retrying() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let first = add_node_env("m1", 0, "first", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &first, TS2)
            .await
            .unwrap();
        let revision_one = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        let duplicate_envelope = add_node_env("m1", 1, "duplicate", "n2", None);
        let events_path = store.events_path("anonymous", "default", "m1");
        for applied_at in ["2026-07-19T02:00:00Z", "2026-07-19T02:01:00Z"] {
            let tail = synth_event(&revision_one, &duplicate_envelope, 2, applied_at);
            store
                .workspace()
                .append_jsonl_path(&events_path, &tail)
                .await
                .unwrap();
        }
        // Snapshot stays at revision 1, so neither tail can be proven served.
        let first_sweep = store.startup_repair_all().await;
        assert_eq!(first_sweep.maps_quarantined_ambiguous, 1);
        assert_eq!(first_sweep.maps_failed, 0);
        assert!(store
            .workspace()
            .read_to_string_path(&store.ambiguous_history_marker_path("anonymous", "default", "m1"))
            .await
            .is_ok());

        let second_sweep = store.startup_repair_all().await;
        assert_eq!(second_sweep.maps_quarantined_ambiguous, 1);
        assert_eq!(second_sweep.maps_failed, 0);
        // The log is untouched: reads keep serving the snapshot.
        let map = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(map.revision, 1);
    }

    #[tokio::test]
    async fn startup_repair_rejects_distinct_replay_valid_duplicate_tails() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let first = add_node_env("m1", 0, "first", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &first, TS2)
            .await
            .unwrap();

        let revision_one = store
            .load_map("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        let duplicate_envelope = add_node_env("m1", 1, "duplicate", "n2", None);
        let first_tail = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:00:00Z",
        );
        let second_tail = synth_event(
            &revision_one,
            &duplicate_envelope,
            2,
            "2026-07-19T02:01:00Z",
        );
        assert_ne!(first_tail, second_tail);
        let events_path = store.events_path("anonymous", "default", "m1");
        for event in [&first_tail, &second_tail] {
            store
                .workspace()
                .append_jsonl_path(&events_path, event)
                .await
                .unwrap();
        }
        let before = store
            .workspace()
            .read_to_string_path(&events_path)
            .await
            .unwrap();

        let error = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .expect_err("an ambiguous duplicate tail must fail closed");
        assert!(
            error
                .to_string()
                .contains("multiple replay-valid terminal branches"),
            "unexpected fail-closed reason: {error}"
        );
        assert_eq!(
            store
                .workspace()
                .read_to_string_path(&events_path)
                .await
                .unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn startup_repair_quarantines_truncated_trailing_line() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let e1 = add_node_env("m1", 0, "i1", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &e1, TS2)
            .await
            .unwrap();

        // Append a partial/garbage trailing line (a torn write).
        let events_path = store.events_path("anonymous", "default", "m1");
        store
            .workspace()
            .append_path(&events_path, b"{\"sequence\":\n")
            .await
            .unwrap();

        let report = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .unwrap();
        assert!(report.quarantined_trailing_bytes);
        // Reconciled to the last good event (seq 1). Snapshot was already at
        // revision 1, so no roll-forward beyond quarantine, but the log was
        // rewritten so `repaired` is true.
        assert!(report.repaired);
        assert_eq!(report.to_revision, 1);

        // The partial line is gone; the file parses cleanly.
        let body = store
            .workspace()
            .read_to_string_path(&events_path)
            .await
            .unwrap();
        assert!(!body.contains("{\"sequence\":\n") && !body.contains("\"sequence\":\n"));
        let events = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 1);
    }

    #[tokio::test]
    async fn startup_repair_interior_corruption_is_corrupt() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        // Two good events.
        let e1 = add_node_env("m1", 0, "i1", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &e1, TS2)
            .await
            .unwrap();
        let e2 = add_node_env("m1", 1, "i2", "n2", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &e2, TS2)
            .await
            .unwrap();

        // Corrupt the FIRST (interior, non-final) line.
        let events_path = store.events_path("anonymous", "default", "m1");
        let body = store
            .workspace()
            .read_to_string_path(&events_path)
            .await
            .unwrap();
        let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
        lines[0] = "{ this is not json".to_string();
        let rewritten = format!("{}\n", lines.join("\n"));
        store
            .workspace()
            .write_string_atomic_path(&events_path, &rewritten)
            .await
            .unwrap();

        let err = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .expect_err("interior corruption");
        assert!(matches!(err, ThinkingMapStoreError::Corrupt(_)));

        // The file was NOT truncated — both lines still present.
        let after = store
            .workspace()
            .read_to_string_path(&events_path)
            .await
            .unwrap();
        assert_eq!(after.lines().filter(|l| !l.trim().is_empty()).count(), 2);
    }

    #[tokio::test]
    async fn startup_repair_already_consistent_is_noop() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        let e1 = add_node_env("m1", 0, "i1", "n1", None);
        store
            .apply_and_persist("anonymous", "default", "m1", &e1, TS2)
            .await
            .unwrap();

        let snapshot_path = store.snapshot_path("anonymous", "default", "m1");
        let manifest_path = store.manifest_path("anonymous", "default", "m1");
        let snap_before = store
            .workspace()
            .read_to_string_path(&snapshot_path)
            .await
            .unwrap();
        let man_before = store
            .workspace()
            .read_to_string_path(&manifest_path)
            .await
            .unwrap();

        let report = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .unwrap();
        assert!(!report.repaired);
        assert!(!report.quarantined_trailing_bytes);
        assert_eq!(report.from_revision, 1);
        assert_eq!(report.to_revision, 1);
        assert_eq!(report.replayed_events, 0);

        // Snapshot + manifest byte-unchanged.
        let snap_after = store
            .workspace()
            .read_to_string_path(&snapshot_path)
            .await
            .unwrap();
        let man_after = store
            .workspace()
            .read_to_string_path(&manifest_path)
            .await
            .unwrap();
        assert_eq!(snap_before, snap_after);
        assert_eq!(man_before, man_after);
    }

    #[tokio::test]
    async fn startup_repair_empty_fresh_map_is_noop() {
        let (_tmp, store) = store();
        store.create_map(&sample_map("m1")).await.unwrap();
        // Fresh map: empty log, snapshot at revision 0.
        let report = store
            .startup_repair("anonymous", "default", "m1")
            .await
            .unwrap();
        assert!(!report.repaired);
        assert_eq!(report.to_revision, 0);
    }

    // ── Boot sweep (`startup_repair_all`) ────────────────────────────────────

    fn sample_map_in(principal: &str, workspace: &str, map_id: &str) -> ThinkingMap {
        ThinkingMap::new(
            map_id.to_string(),
            principal,
            workspace,
            "Test map",
            ThinkingMapSource::Solo,
            TS,
        )
    }

    /// Create `map_id` in `(principal, workspace)` and apply one add-node
    /// envelope so the log has exactly one committed event.
    async fn seed_one(store: &ThinkingMapStore, principal: &str, workspace: &str, map_id: &str) {
        store
            .create_map(&sample_map_in(principal, workspace, map_id))
            .await
            .expect("create");
        let env = add_node_env(map_id, 0, &format!("{map_id}-i1"), "n1", None);
        store
            .apply_and_persist(principal, workspace, map_id, &env, TS2)
            .await
            .expect("apply");
    }

    /// FIX 1 core claim: a map whose log has a torn trailing line is broken for
    /// `events_after` (and therefore replay / restore / the events endpoint)
    /// until something calls the repair — and the BOOT SWEEP is that something.
    #[tokio::test]
    async fn startup_repair_all_fixes_torn_tail_and_events_after_works_again() {
        let (_tmp, store) = store();
        seed_one(&store, "anonymous", "default", "m1").await;

        // Torn write: a partial trailing record, exactly what a crash inside
        // `append_jsonl_path` leaves behind.
        let events_path = store.events_path("anonymous", "default", "m1");
        store
            .workspace()
            .append_path(&events_path, b"{\"sequence\":")
            .await
            .unwrap();

        // Pre-condition: the log is now permanently unreadable for this map.
        let before = store.events_after("anonymous", "default", "m1", 0).await;
        assert!(
            matches!(before, Err(ThinkingMapStoreError::Corrupt(_))),
            "torn tail should make events_after fail before the sweep, got {before:?}"
        );

        let sweep = store.startup_repair_all().await;
        assert_eq!(sweep.maps_scanned, 1);
        assert_eq!(sweep.maps_quarantined, 1);
        assert_eq!(sweep.maps_repaired, 1);
        assert_eq!(sweep.maps_failed, 0);
        assert_eq!(sweep.maps_skipped_concurrent_write, 0);
        assert!(sweep.touched_anything());

        // Post-condition: the committed event survived, the fragment is gone.
        let after = store
            .events_after("anonymous", "default", "m1", 0)
            .await
            .expect("events_after works after the sweep");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].sequence, 1);

        // And the manifest head agrees with the log, so the next apply cannot
        // mint a duplicate sequence.
        let manifest = store
            .read_manifest("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(manifest.latest_sequence, 1);
    }

    /// One bad map must not stop the sweep. `m1` has interior corruption and is
    /// genuinely unrepairable; `m2` is in the append→snapshot crash window and
    /// is repairable. The sweep must fix `m2` even though it hit `m1` first
    /// (`list_map_ids` is sorted, so `m1` is visited first).
    #[tokio::test]
    async fn startup_repair_all_continues_past_an_unrepairable_map() {
        let (_tmp, store) = store();

        // m1: two committed events, then the FIRST (interior) line mangled.
        store.create_map(&sample_map("m1")).await.unwrap();
        for (rev, idem, node) in [(0u64, "i1", "n1"), (1, "i2", "n2")] {
            let env = add_node_env("m1", rev, idem, node, None);
            store
                .apply_and_persist("anonymous", "default", "m1", &env, TS2)
                .await
                .unwrap();
        }
        let m1_events = store.events_path("anonymous", "default", "m1");
        let body = store
            .workspace()
            .read_to_string_path(&m1_events)
            .await
            .unwrap();
        let mut lines: Vec<String> = body.lines().map(|l| l.to_string()).collect();
        lines[0] = "{ this is not json".to_string();
        store
            .workspace()
            .write_string_atomic_path(&m1_events, &format!("{}\n", lines.join("\n")))
            .await
            .unwrap();

        // m2: one committed event, then a second appended WITHOUT the snapshot
        // /manifest write — the 2a crash window.
        seed_one(&store, "anonymous", "default", "m2").await;
        let m2_snapshot = store
            .load_map("anonymous", "default", "m2")
            .await
            .unwrap()
            .unwrap();
        let e2 = add_node_env("m2", 1, "m2-i2", "n2", None);
        let extra = synth_event(&m2_snapshot, &e2, 2, "2026-07-19T02:00:00Z");
        store
            .workspace()
            .append_jsonl_path(&store.events_path("anonymous", "default", "m2"), &extra)
            .await
            .unwrap();

        let sweep = store.startup_repair_all().await;
        assert_eq!(sweep.scopes_scanned, 1);
        assert_eq!(sweep.maps_scanned, 2, "both maps must be attempted");
        assert_eq!(sweep.maps_failed, 1, "m1 is unrepairable");
        assert_eq!(sweep.maps_repaired, 1, "m2 must still be repaired");
        assert_eq!(sweep.maps_quarantined, 0);

        // m2 rolled forward.
        let m2 = store
            .load_map("anonymous", "default", "m2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(m2.revision, 2);
        assert!(m2.nodes.contains_key("n2"));

        // m1 was NOT truncated to make the numbers look good.
        let m1_after = store
            .workspace()
            .read_to_string_path(&m1_events)
            .await
            .unwrap();
        assert_eq!(m1_after.lines().filter(|l| !l.trim().is_empty()).count(), 2);
    }

    /// The sweep spans every scope on disk, and a boot where nothing is damaged
    /// reports nothing (so the log line only ever means "something happened").
    #[tokio::test]
    async fn startup_repair_all_spans_scopes_and_is_silent_when_clean() {
        let (_tmp, store) = store();
        seed_one(&store, "anonymous", "default", "m1").await;
        seed_one(&store, "anonymous", "default", "m2").await;
        seed_one(&store, "alice", "team", "m1").await;

        let sweep = store.startup_repair_all().await;
        assert_eq!(sweep.scopes_scanned, 2);
        assert_eq!(sweep.scopes_unreadable, 0);
        assert_eq!(sweep.maps_scanned, 3);
        assert_eq!(sweep.maps_repaired, 0);
        assert_eq!(sweep.maps_quarantined, 0);
        assert_eq!(sweep.maps_failed, 0);
        assert!(!sweep.touched_anything());
    }

    /// A soft-deleted map is still swept: the tombstone stays loadable for
    /// recovery/audit, so its log has to replay too. `list_maps` would have
    /// hidden it — `list_map_ids` deliberately does not.
    #[tokio::test]
    async fn startup_repair_all_includes_soft_deleted_tombstones() {
        let (_tmp, store) = store();
        seed_one(&store, "anonymous", "default", "m1").await;
        let mut manifest = store
            .read_manifest("anonymous", "default", "m1")
            .await
            .unwrap()
            .unwrap();
        manifest.lifecycle = MapLifecycle::Deleted;
        store
            .workspace()
            .write_json_atomic_path(
                &store.manifest_path("anonymous", "default", "m1"),
                &manifest,
            )
            .await
            .unwrap();

        let sweep = store.startup_repair_all().await;
        assert_eq!(sweep.maps_scanned, 1);
    }
}
