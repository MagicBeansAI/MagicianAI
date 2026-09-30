//! The one place a task record is allowed to reach the disk.
//!
//! **This module exists because "the single chokepoint" was asserted twice and
//! was wrong both times.** The list index and the `/today` projection cache
//! were each hooked onto `ArtifactV2Service::persist_task_record_unlocked` on
//! the belief that every task write funnels through it. Two other writers land
//! task records — `FilesystemArtifactV2Reducer::commit_task_writes` (every
//! execution transition, including the terminal one that flips
//! `task.state.status`) and `ArtifactV2Service::commit_task_plan_bundle_unlocked`
//! (which carries an optional `TaskRecord`, and seven of its nine callers
//! mutate status) — and neither reconciled anything. An execution-completed
//! task stayed in the **Running** lane, with wrong `lane_counts`, until a
//! restart or `magician --reindex`; Today kept it in Follow-ups for the rest
//! of the cache window.
//!
//! The fix is not a third call site that remembers. It is this type: the
//! journal commit and the reconciliation are **one operation**, and the raw
//! primitive underneath it has exactly one caller in `artifact_v2` — this one.
//! Enumerating writers is what failed; a writer that cannot commit without
//! reconciling cannot repeat the failure.
//!
//! ## Two primitives land a task record, not one
//!
//! The commit is only half of it. A journal replay
//! (`ArtifactV2Workspace::recover_multi_write_journal_path`) applies a
//! persisted write set verbatim — `manifest.json`, `state/task_state.json`
//! and `task_refs.json` among them — and it runs on sixteen paths, including
//! every reducer op and every `get_task`. A commit that died between "journal
//! durable" and "writes applied" is *finished* by the next replay, so a replay
//! that reconciled nothing would land a task record with no reindex and no
//! invalidation: the same bug, third variant. Both primitives therefore live
//! behind this type — [`TaskWriteReconciler::commit_task_writes`] and
//! [`TaskWriteReconciler::recover_task_writes`] — and
//! `only_the_reconciler_reaches_a_task_write_journal` asserts that neither is
//! reachable from anywhere else in the module.
//!
//! ## What decides whether a reconcile is owed
//!
//! **The write set, not the caller.** A caller-supplied "and also reindex this"
//! flag is the same footgun in a new costume; it is remembered by exactly the
//! people who did not need reminding. So [`TaskWriteReconciler`] looks at the
//! paths being committed and reconciles when the task's own record files
//! (`manifest.json`, `state/task_state.json`, `task_refs.json`) are among them.
//! An execution-only commit — an execution record plus its index — changes no
//! list row and drops no projection, and is left alone.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use tracing::warn;

use crate::magician_v2::{
    storage::{ListIndex, ListScope},
    today_projection_cache::TodayProjectionCache,
};

use super::{
    service::{ArtifactV2Error, ScopeRef},
    workspace::ArtifactV2Workspace,
};

/// Commits task writes and reconciles everything that projects off them.
///
/// Held by `ArtifactV2Service` **and** by the reducer it owns, as the same
/// `Arc`, which is what lets a reducer running under its own cross-process
/// write lock reach an index the service was handed long after both were
/// constructed. The index arrives through a `OnceLock` that only
/// `bin/magician.rs` fills in production; sharing the slot rather than the
/// opened index is what makes the late wiring work.
pub struct TaskWriteReconciler {
    workspace: ArtifactV2Workspace,
    /// The rebuildable list index, when a host wired one in.
    ///
    /// `None` (most tests, and every embedder that never opened one) is not a
    /// degraded mode: the lists fall back to the walk, which is still the
    /// source of truth. Nothing may become unreadable because the cache is
    /// absent.
    list_index: Arc<OnceLock<ListIndex>>,
    /// The `/today` projection cache, shared with the `FeedApi` that serves
    /// it. Today's Follow-ups lane is projected from `list_tasks`, so a task
    /// created, completed, edited or deleted here changes what `/today` should
    /// answer with nothing in the Today API aware of it.
    today_projection_cache: Arc<TodayProjectionCache>,
}

impl TaskWriteReconciler {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self {
            workspace,
            list_index: Arc::new(OnceLock::new()),
            today_projection_cache: Arc::new(TodayProjectionCache::new()),
        }
    }

    /// Hand the index in. Idempotent — first call wins.
    pub fn set_list_index(&self, index: ListIndex) {
        let _ = self.list_index.set(index);
    }

    /// The index, if a host wired one in. A caller must still check
    /// [`ListIndex::is_ready`] before believing it: present-but-rebuilding is
    /// the state that looks like a complete index holding fewer tasks.
    pub fn list_index(&self) -> Option<&ListIndex> {
        self.list_index.get()
    }

    pub fn today_projection_cache(&self) -> &Arc<TodayProjectionCache> {
        &self.today_projection_cache
    }

    /// The index's spelling of a scope. Normalised through the workspace so it
    /// is the directory name a rebuild would have recorded.
    pub fn list_scope(scope: &ScopeRef) -> ListScope {
        let (principal, workspace) =
            ArtifactV2Workspace::scope_dir_segments(&scope.principal(), &scope.workspace());
        ListScope::new(principal, workspace)
    }

    /// **The** task write. Commit the multi-write journal, then reconcile the
    /// index and the Today cache if the write set carried the task record.
    ///
    /// Reconciling *after* the commit is load-bearing in both directions: an
    /// index updated first would advertise a state the disk never took, and a
    /// projection dropped first could be repopulated from the pre-write state
    /// by a concurrent read and outlive the write by a further TTL.
    ///
    /// Reconciliation is best effort and cannot fail the write. The files are
    /// the source of truth; a write that landed must not be reported as failed
    /// because a cache could not be updated.
    pub async fn commit_task_writes(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        writes: &[(PathBuf, Vec<u8>)],
    ) -> Result<(), ArtifactV2Error> {
        let journal_path = self.workspace.task_multi_write_journal_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        );
        self.workspace
            .commit_multi_write_journal_path(&journal_path, writes)
            .await?;
        let carried_task_record = self.paths_carry_task_record(
            scope,
            task_id,
            writes.iter().map(|(path, _)| path.as_path()),
        );
        if carried_task_record {
            self.task_record_committed(scope, task_id).await;
        }
        Ok(())
    }

    /// **The** task write's other half: replay a persisted journal and
    /// reconcile whatever the replay landed.
    ///
    /// A recovery is a task-record write. [`Self::commit_task_writes`] can die
    /// between "journal durable" and "every write applied" — a failed
    /// `write_atomic_path`, or the process going away — and it correctly `?`s
    /// out without reconciling, because nothing it promised is on disk yet.
    /// The next touch of that task replays the journal and **completes the
    /// write**. If that replay did not reconcile too, the record would reach
    /// disk with no reindex and no invalidation, and nothing bounds the drift:
    /// there is no periodic reconciler, and `is_ready()` is true for a
    /// stale-but-complete index. That is the commit-path bug in a third
    /// costume, so it gets the commit path's answer — recovery and
    /// reconciliation are one operation, and a caller gets both by calling
    /// this rather than by remembering to act on a returned flag.
    ///
    /// **Reconciles on failure too.** A replay that errors part-way may
    /// already have landed the record, and the paths are gone with the error,
    /// so this cannot tell. It reindexes rather than guess; the journal
    /// survives a partial replay, so the next recovery finishes the write and
    /// reconciles again. Reindexing a task that did not change costs one
    /// re-read; skipping one that did is the bug.
    pub async fn recover_task_writes(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<(), ArtifactV2Error> {
        let journal_path = self.workspace.task_multi_write_journal_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
        );
        let replayed = self
            .workspace
            .recover_multi_write_journal_path(&journal_path)
            .await;
        let carried_task_record = match &replayed {
            Ok(paths) => {
                self.paths_carry_task_record(scope, task_id, paths.iter().map(PathBuf::as_path))
            },
            Err(_) => true,
        };
        if carried_task_record {
            self.task_record_committed(scope, task_id).await;
        }
        replayed?;
        Ok(())
    }

    /// Do any of these paths name the task's own record files?
    ///
    /// Compared as whole paths against the workspace's own path helpers rather
    /// than sniffed for a file name, so a rename of any of the three files
    /// moves this with it instead of silently switching the reconcile off.
    ///
    /// Takes paths rather than a write set because both callers have to ask
    /// the same question about different things: the bytes a commit is about
    /// to write, and the paths a recovery just replayed.
    fn paths_carry_task_record<'a>(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> bool {
        let record_paths = [
            self.workspace
                .task_manifest_path(&scope.principal(), &scope.workspace(), task_id),
            self.workspace
                .task_state_path(&scope.principal(), &scope.workspace(), task_id),
            self.workspace
                .task_refs_path(&scope.principal(), &scope.workspace(), task_id),
        ];
        paths
            .into_iter()
            .any(|path| record_paths.iter().any(|record| record == path))
    }

    /// A task record just landed on disk: bring its index rows in line and
    /// drop the scope's cached Today projection.
    ///
    /// **Private on purpose.** This is the reconcile half of a task write, and
    /// nothing outside this module may have it without the write half — a
    /// caller able to reach both could write the record with one primitive and
    /// reconcile with this, which is the whole failure this type exists to make
    /// unrepresentable. It is reached only by committing or replaying.
    async fn task_record_committed(&self, scope: &ScopeRef, task_id: &str) {
        self.reindex_task(scope, task_id).await;
        self.invalidate_today_projection(scope);
    }

    /// A task's records are gone from disk: drop its index rows and the
    /// scope's cached Today projection.
    ///
    /// A delete writes no task record, so it needs its own call — this is the
    /// one write path that legitimately does not go through
    /// [`Self::commit_task_writes`].
    pub async fn task_record_removed(&self, scope: &ScopeRef, task_id: &str) {
        self.deindex_task(task_id).await;
        self.invalidate_today_projection(scope);
    }

    /// Drop this scope's cached Today projection.
    ///
    /// **Correctness over hit rate.** Every task record write lands here,
    /// including the status churn of a running execution, so a scope with busy
    /// tasks recomputes Today more often. A projection that outlived the write
    /// under it is not a cheaper answer, it is a wrong one.
    ///
    /// The caller's `ScopeRef` goes in as it stands: the cache normalises the
    /// scope to its directory segments itself, exactly as [`Self::list_scope`]
    /// does for the index. It did not always — the cache was keyed by the
    /// spelling a reader's request happened to use, which made `user:1` and
    /// `user_1` two entries over one task directory, so a write under either
    /// left the other reader's projection standing for a full TTL.
    ///
    /// Private for the same reason as [`Self::task_record_committed`]: half a
    /// reconcile is not a thing a caller outside this module should be able to
    /// perform, and the deletes that legitimately need one call
    /// [`Self::task_record_removed`].
    fn invalidate_today_projection(&self, scope: &ScopeRef) {
        self.today_projection_cache
            .invalidate_scope(&scope.principal(), &scope.workspace());
    }

    /// Bring one task's index rows in line with what is now on disk.
    ///
    /// The index re-reads the record itself rather than taking the in-memory
    /// copy, so it can only ever hold what a rebuild would reproduce.
    ///
    /// **This runs on eleven of the sixteen reducer transitions**, two of them
    /// per-tick during a run: `reduce_runtime_signal` on every runtime signal
    /// and `reduce_step_event` on every step outcome. `index_task_from_disk`
    /// declines to write when the record's indexed columns did not move, which
    /// is what keeps the step-event path from being a SQLite write per tick;
    /// a runtime signal moves `updated_at`, an indexed and order-bearing
    /// column, so it genuinely owes the write. See `list-storage-index.md`
    /// for what that leaves on the table.
    async fn reindex_task(&self, scope: &ScopeRef, task_id: &str) {
        let Some(index) = self.list_index.get() else {
            return;
        };
        let index = index.clone();
        let scopes_root = self.workspace.scopes_root();
        let list_scope = Self::list_scope(scope);
        let owned_task_id = task_id.to_string();
        // SQLite plus two small JSON reads: admitted blocking work, gated
        // on the async side first so the index mutex wait cannot occupy a
        // process-wide permit.
        if let Err(error) = index
            .index_task_from_disk_admitted(scopes_root, list_scope, owned_task_id)
            .await
        {
            warn!(
                principal = %scope.principal(),
                workspace = %scope.workspace(),
                task_id = %task_id,
                error = %error,
                "[LIST-INDEX] Failed to reindex a task after a write; lists fall back to the walk"
            );
        }
    }

    /// Drop every index row a deleted task owned — its `task`, `internal` and
    /// `monitor` rows. Best effort for the same reason as [`Self::reindex_task`],
    /// but the failure is worse to leave: a row whose record is gone is a list
    /// entry a reader can click into and find nothing behind.
    async fn deindex_task(&self, task_id: &str) {
        let Some(index) = self.list_index.get() else {
            return;
        };
        let index = index.clone();
        let owned_task_id = task_id.to_string();
        if let Err(error) = index.remove_task_admitted(owned_task_id).await {
            warn!(
                task_id = %task_id,
                error = %error,
                "[LIST-INDEX] Failed to remove a deleted task's index rows"
            );
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::fs;

    use super::*;
    use crate::magician_v2::{
        artifact_v2::{
            models::{
                ExecutionOutcomeSnapshot, ExecutionRecord, ExecutionRefs, ExecutionState,
                TaskLifecycle, TaskManifest, TaskOutputMode, TaskSyncMode,
            },
            service::CreateTaskInput,
        },
        storage::{ListKind, ListPageQuery},
        task_lanes::TaskLane,
        test_support::{build_test_artifact_v2_service, wire_test_list_index},
        today_projection_cache::{TodayCacheKey, TodayProjection},
    };

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";
    const TODAY: &str = "2026-07-31";

    fn test_scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(&PRINCIPAL.to_string(), &WORKSPACE.to_string())
    }

    fn create_input() -> CreateTaskInput {
        CreateTaskInput {
            principal: PRINCIPAL.to_string(),
            workspace: WORKSPACE.to_string(),
            title: "Indexed through the reducer".to_string(),
            description: "Indexed through the reducer".to_string(),
            agent_id: "personal-assistant".to_string(),
            goal_id: None,
            ui_thread_id: "general".to_string(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: "user".to_string(),
            depends_on: Vec::new(),
            approved: true,
            schedule: None,
            output_mode: TaskOutputMode::Accumulate,
            chat_session_id: None,
            lifecycle: TaskLifecycle::Persistent,
            sync_mode: TaskSyncMode::default(),
        }
    }

    /// What the index says this task's status is — `None` when the index has
    /// no row for it at all, which is the shape of the bug this covers.
    fn indexed_status(index: &ListIndex, task_id: &str) -> Option<String> {
        index
            .page(&ListPageQuery::new(
                ListKind::Task,
                TaskWriteReconciler::list_scope(&test_scope()),
                50,
            ))
            .expect("the index pages")
            .items
            .into_iter()
            .find(|entry| entry.id == task_id)
            .map(|entry| entry.status)
    }

    fn lane_count(index: &ListIndex, lane: TaskLane) -> usize {
        index
            .lane_counts(
                ListKind::Task,
                &TaskWriteReconciler::list_scope(&test_scope()),
                TODAY,
            )
            .expect("the index counts lanes")
            .get(lane.wire_name())
            .copied()
            .expect("every lane is present, even at zero")
    }

    /// A root execution for `task_id`, the shape provisioning mints.
    fn root_execution(task_id: &str, execution_id: &str) -> ExecutionRecord {
        ExecutionRecord {
            state: ExecutionState {
                execution_id: execution_id.to_string(),
                task_id: task_id.to_string(),
                root_execution_id: Some(execution_id.to_string()),
                parent_execution_id: None,
                agent_id: "personal-assistant".to_string(),
                relationship_type: "root".to_string(),
                status: "ready".to_string(),
                completion_kind: None,
                open_items: Vec::new(),
                plan_id: None,
                primary_execution_output_id: None,
                active_child_execution_ids: Vec::new(),
                started_at: "2026-07-31T06:00:00Z".to_string(),
                completed_at: None,
                updated_at: "2026-07-31T06:00:00Z".to_string(),
                completed_step_ids: Vec::new(),
                failed_step_ids: Vec::new(),
                current_step_id: None,
                task_output_mode: TaskOutputMode::default(),
                refinement: None,
                synthesis_pending: false,
                synthesis_failed: None,
            },
            refs: ExecutionRefs {
                execution_id: execution_id.to_string(),
                ..Default::default()
            },
        }
    }

    fn seed_today_projection(cache: &TodayProjectionCache) -> TodayCacheKey {
        let key = TodayCacheKey::new(PRINCIPAL, WORKSPACE, TODAY);
        cache.insert(key.clone(), TodayProjection::default());
        assert!(
            cache.get(&key).is_some(),
            "the seeded projection has to be there, or the assertion that it \
             was dropped proves nothing"
        );
        key
    }

    /// **The regression test for the claim that was wrong twice.**
    ///
    /// Every write here goes through the *reducer* — `reduce_task_created`,
    /// `reduce_execution_initialized`, `reduce_execution_terminal_without_outputs`
    /// — and not one of them touches
    /// `ArtifactV2Service::persist_task_record_unlocked`, the write both
    /// earlier rounds hooked as "the single chokepoint". Before the fix this
    /// test fails three times over: the created task has no index row at all,
    /// the started task never enters the Running lane, and the completed task
    /// never leaves it.
    ///
    /// It asserts the two things a reader would actually notice — the row's
    /// `status` column, which is what every lane predicate reads, and the
    /// lane counts beside it — plus the Today projection the same write has to
    /// drop, because a completed task that stays in Follow-ups for a further
    /// TTL is the same stale write seen from the other surface.
    #[tokio::test]
    async fn a_reducer_write_reaches_the_list_index_and_todays_cache() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let service = build_test_artifact_v2_service(tmp.path());
        // Wired before any record exists, so nothing here is indexed by the
        // rebuild — every row this test sees was put there by a write hook.
        let index = wire_test_list_index(&service);
        let cache = Arc::clone(service.today_projection_cache());

        // 1. Creation. `create_task` writes through `reduce_task_created` and
        //    nothing else, so this row exists only if the reducer reconciles.
        let key = seed_today_projection(&cache);
        let mut task = service
            .create_task(create_input())
            .await
            .expect("the task creates");
        let task_id = task.manifest.task_id.clone();
        assert_eq!(
            indexed_status(&index, &task_id).as_deref(),
            Some("pending"),
            "a created task has to be in the index; `create_task` writes \
             through the reducer, never through the service's persist path"
        );
        assert!(
            cache.get(&key).is_none(),
            "creating a task changes what Today's Follow-ups lane answers"
        );

        // 2. The run starts. `reduce_execution_initialized` flips the task to
        //    `running` and takes the active root — a task-record write under
        //    the reducer's own cross-process lock.
        let execution = root_execution(&task_id, "exec_reconcile_1");
        let key = seed_today_projection(&cache);
        service
            .reducer()
            .reduce_execution_initialized(&mut task, &execution)
            .await
            .expect("the execution initializes");
        assert_eq!(
            indexed_status(&index, &task_id).as_deref(),
            Some("running"),
            "the started task has to read as running in the index"
        );
        assert_eq!(lane_count(&index, TaskLane::Running), 1);
        assert_eq!(lane_count(&index, TaskLane::Completed), 0);
        assert!(
            cache.get(&key).is_none(),
            "a task that starts running has left Today's Follow-ups lane, so \
             the cached projection describing it has to go too"
        );

        // 3. The run finishes. This is the write the bug report names: the
        //    reducer sets `task.state.status = outcome.task_status` and
        //    commits, and before the fix the index kept saying `running`
        //    until a restart or a `--reindex`.
        let key = seed_today_projection(&cache);
        let mut execution = execution;
        service
            .reducer()
            .reduce_execution_terminal_without_outputs(
                &mut task,
                &mut execution,
                &ExecutionOutcomeSnapshot {
                    execution_status: "completed".to_string(),
                    task_status: "completed".to_string(),
                    outcome_type: "success".to_string(),
                    outcome_summary: "finished".to_string(),
                    iterations_used: Some(1),
                    is_terminal: true,
                    completion_kind: None,
                    open_items: Vec::new(),
                },
            )
            .await
            .expect("the terminal transition reduces");

        assert_eq!(
            indexed_status(&index, &task_id).as_deref(),
            Some("completed"),
            "an execution-completed task must not still read as running"
        );
        assert_eq!(
            lane_count(&index, TaskLane::Running),
            0,
            "the completed task has to leave the Running lane"
        );
        assert_eq!(
            lane_count(&index, TaskLane::Completed),
            1,
            "and arrive in the Completed one, with the counts to match"
        );
        assert!(
            cache.get(&key).is_none(),
            "a task completed on the reducer path has to drop Today's cached \
             projection, or Follow-ups keeps it for up to the TTL"
        );
    }

    /// An execution-only commit is **not** a task-record write, and must not
    /// pay for one.
    ///
    /// The reconcile is decided by the write set rather than by the caller, so
    /// this is the other half of that decision: the execution record and its
    /// index change no list row and drop no projection. Without this the
    /// cheapest way to be correct would be to reindex on every commit, and a
    /// running execution commits constantly.
    #[tokio::test]
    async fn an_execution_only_commit_reconciles_nothing() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let service = build_test_artifact_v2_service(tmp.path());
        let index = wire_test_list_index(&service);
        let scope = test_scope();
        let cache = Arc::clone(service.today_projection_cache());

        let task = service
            .create_task(create_input())
            .await
            .expect("the task creates");
        let task_id = task.manifest.task_id.clone();
        let execution = root_execution(&task_id, "exec_reconcile_2");

        // Make the index disagree with the disk on purpose. If the commit
        // below reconciled, the row would come back and this test could not
        // tell the two behaviours apart.
        //
        // `remove` reports whether a row was actually there, and that report
        // is the whole setup: a task that was never indexed would leave both
        // assertions below true no matter what the commit did.
        assert!(
            index
                .remove(ListKind::Task, &task_id)
                .expect("the index removes"),
            "there was no row to drop, so the two assertions below hold for a \
             reason that has nothing to do with the behaviour under test"
        );
        assert_eq!(indexed_status(&index, &task_id), None);
        let key = seed_today_projection(&cache);

        service
            .reducer()
            .reduce_execution_discovered(&scope, &execution)
            .await
            .expect("the execution record commits");

        assert_eq!(
            indexed_status(&index, &task_id),
            None,
            "an execution-only commit carries no task record and must not \
             reindex"
        );
        assert!(
            cache.get(&key).is_some(),
            "and must not drop Today's projection either"
        );
    }

    /// **A journal replay finishes somebody else's task write, so it owes the
    /// same reconciliation the commit does.**
    ///
    /// `commit_task_writes` can die between "journal durable" and "every write
    /// applied"; it `?`s out without reconciling, which is right, because
    /// nothing it promised is on disk yet. This simulates exactly that state —
    /// a durable journal carrying a changed `task_state.json` — and then
    /// touches the task through `get_task`, one of the sixteen paths that
    /// replay a journal. The replay completes the write. Before the fix it
    /// completed it *silently*: the index kept the old `status` and Today kept
    /// its cached projection, with no periodic reconciler and no `is_ready()`
    /// signal to bound the drift.
    #[tokio::test]
    async fn a_recovered_journal_reaches_the_list_index_and_todays_cache() {
        use crate::magician_v2::artifact_v2::service::V3ReadApi;

        let tmp = tempfile::TempDir::new().expect("temp dir");
        let service = build_test_artifact_v2_service(tmp.path());
        let index = wire_test_list_index(&service);
        let scope = test_scope();
        let cache = Arc::clone(service.today_projection_cache());
        let workspace = &service.task_writes().workspace;

        let task = service
            .create_task(create_input())
            .await
            .expect("the task creates");
        let task_id = task.manifest.task_id.clone();
        assert_eq!(indexed_status(&index, &task_id).as_deref(), Some("pending"));

        // The crash state: the journal is durable, its writes are not applied.
        // Its one write moves `status`, which is the column every lane
        // predicate reads — so a replay that reconciles nothing is visible as
        // a task stuck in the wrong lane.
        let state_path = workspace.task_state_path(PRINCIPAL, WORKSPACE, &task_id);
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).expect("the state file reads"))
                .expect("the state file parses");
        state["status"] = serde_json::Value::String("completed".to_string());
        let replayed_bytes = serde_json::to_vec_pretty(&state).expect("the state re-serializes");
        workspace
            .write_json_atomic_path(
                workspace.task_multi_write_journal_path(PRINCIPAL, WORKSPACE, &task_id),
                &serde_json::json!({
                    "version": 1,
                    "writes": [{ "path": state_path, "bytes": replayed_bytes }],
                }),
            )
            .await
            .expect("the journal persists");

        let key = seed_today_projection(&cache);
        let recovered = service
            .get_task(&scope, &task_id)
            .await
            .expect("the task reads back");
        assert_eq!(
            recovered.state.status, "completed",
            "the replay has to have applied the journal, or the assertions \
             below are about a write that never happened"
        );
        assert_eq!(
            indexed_status(&index, &task_id).as_deref(),
            Some("completed"),
            "a journal replay lands a task record, so it owes a reindex just \
             as the commit that abandoned it did"
        );
        assert!(
            cache.get(&key).is_none(),
            "and owes Today's cached projection the same drop"
        );
    }

    /// **The recovery invariant's second guard, deliberately built out of
    /// different parts from the first.**
    ///
    /// Replaying without reconciling is the third variant of an invariant that
    /// escaped twice, it is the newest code here, and
    /// `only_the_reconciler_reaches_a_task_write_journal` is blind to it —
    /// that walk greps for `commit_multi_write_journal_path(`, and a replay
    /// says `recover_`. For a while
    /// `a_recovered_journal_reaches_the_list_index_and_todays_cache` was the
    /// only test that failed when a replay stopped reconciling, which puts the
    /// variant one deletion away from returning. A copy of that test would be
    /// worth nothing; this shares none of its three moving parts:
    ///
    /// 1. **No list index is wired.** The other test asserts the index row
    ///    first and Today's cache second, so a break in the index half fails it
    ///    before the cache half runs. Here `reindex_task` returns at its
    ///    `let Some(index)` and the cached projection is the *only* observable,
    ///    which is exactly what makes it independent.
    /// 2. **A different one of the sixteen replay paths.** The other test
    ///    enters through `V3ReadApi::get_task`; this enters through the
    ///    reducer's `with_task_write_lock`, which every reducer op takes.
    /// 3. **A different record file, so a different arm of
    ///    [`TaskWriteReconciler::paths_carry_task_record`]** — `manifest.json`
    ///    rather than `state/task_state.json`.
    ///
    /// **The op it rides in on provably reconciles nothing by itself.**
    /// `reduce_execution_discovered` carries an execution record and its index
    /// and no task record, and `an_execution_only_commit_reconciles_nothing`
    /// is the standing proof that it therefore drops no projection. So the
    /// dropped entry asserted below can only have come from the replay that
    /// ran before it.
    #[tokio::test]
    async fn a_replay_under_a_reducer_op_drops_todays_projection_with_no_index_present() {
        const REPLAYED_TITLE: &str = "the title the abandoned commit was writing";

        let tmp = tempfile::TempDir::new().expect("temp dir");
        let service = build_test_artifact_v2_service(tmp.path());
        let scope = test_scope();
        let cache = Arc::clone(service.today_projection_cache());
        let workspace = &service.task_writes().workspace;
        assert!(
            service.task_writes().list_index().is_none(),
            "no index is wired here on purpose — Today's cache is meant to be \
             this test's only observable, so that it cannot pass or fail on an \
             index assertion the way the other recovery test can"
        );

        let task = service
            .create_task(create_input())
            .await
            .expect("the task creates");
        let task_id = task.manifest.task_id.clone();

        // The crash state, built from the task's own manifest so the replayed
        // bytes are a real serialization of the real type rather than a
        // hand-poked JSON blob: a commit that got its journal durable and then
        // died before applying it.
        let manifest_path = workspace.task_manifest_path(PRINCIPAL, WORKSPACE, &task_id);
        let mut manifest: TaskManifest = workspace
            .read_json_path(&manifest_path)
            .await
            .expect("the manifest reads");
        manifest.title = REPLAYED_TITLE.to_string();
        let replayed_bytes = serde_json::to_vec_pretty(&manifest).expect("the manifest serializes");
        workspace
            .write_json_atomic_path(
                workspace.task_multi_write_journal_path(PRINCIPAL, WORKSPACE, &task_id),
                &serde_json::json!({
                    "version": 1,
                    "writes": [{ "path": manifest_path, "bytes": replayed_bytes }],
                }),
            )
            .await
            .expect("the journal persists");

        let key = seed_today_projection(&cache);
        service
            .reducer()
            .reduce_execution_discovered(&scope, &root_execution(&task_id, "exec_recover_2"))
            .await
            .expect("the execution record commits");

        let replayed: TaskManifest = workspace
            .read_json_path(&manifest_path)
            .await
            .expect("the manifest reads back");
        assert_eq!(
            replayed.title, REPLAYED_TITLE,
            "the reducer op has to have replayed the journal first, or the \
             assertion below is about a write that never happened"
        );
        assert!(
            cache.get(&key).is_none(),
            "the replay finished a task-record write, so it owes Today's \
             cached projection the same drop the abandoned commit owed — and \
             the op it rode in on carries no task record, so nothing else here \
             could have dropped it"
        );
    }

    /// **The structural half: no writer can land a task record without
    /// reconciling, because every primitive that lands one has a single
    /// caller.**
    ///
    /// Enumerating call sites is exactly what failed twice — a behavioural
    /// test per writer proves only the writers someone thought of. This asserts
    /// the shape instead: inside `artifact_v2`, the raw multi-write journal
    /// primitives are called from `task_writes.rs` alone, and `task_writes.rs`
    /// reconciles. A new writer that reaches for either directly fails here,
    /// naming its own file and line, before it can ship another stale-index
    /// bug.
    ///
    /// **Outside this module the compiler says it, not this test.** Both
    /// primitives are `pub(in crate::magician_v2::artifact_v2)`; they were
    /// `pub`, and `workspace()` is handed out crate-wide, so a writer in
    /// `api/` or `storage/` could land a task record with no reindex and no
    /// invalidation while this stayed green — it reads only the flat
    /// `artifact_v2/` directory. What is left here is the half visibility
    /// cannot express: the sanctioned callers are *inside* the module, so
    /// nothing but this walk stops a second one appearing beside them.
    ///
    /// **Both primitives, because both land a task record.** Covering the
    /// commit alone is how the recovery variant shipped: a replay applies a
    /// persisted write set verbatim and so finishes a write the commit path
    /// abandoned. A test blind to `recover_` would have passed throughout.
    ///
    /// Comment-only lines are skipped, so naming a primitive in prose — this
    /// module's own header does — is not an offence. Only the flat
    /// `artifact_v2` directory is read, which covers every file in it today;
    /// **whoever adds a subdirectory has to make this walk recursive**, or the
    /// new files are outside the invariant without anything saying so.
    #[test]
    fn only_the_reconciler_reaches_a_task_write_journal() {
        const PRIMITIVES: [&str; 2] = [
            "commit_multi_write_journal_path(",
            "recover_multi_write_journal_path(",
        ];
        // The primitives' own definitions live in `workspace.rs`; their one
        // sanctioned caller is this module.
        const ALLOWED: [&str; 2] = ["workspace.rs", "task_writes.rs"];

        let module_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/magician_v2/artifact_v2");
        let mut offenders = Vec::new();
        // Counts the files actually searched — the allowed two are skipped
        // before this, so it cannot be satisfied by them alone.
        let mut files_searched = 0usize;
        let mut allowed_seen = 0usize;
        for entry in fs::read_dir(&module_dir).expect("the artifact_v2 module dir reads") {
            let path = entry.expect("a dir entry reads").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("a source file has a name")
                .to_string();
            if ALLOWED.contains(&file_name.as_str()) {
                allowed_seen += 1;
                continue;
            }
            files_searched += 1;
            let source = fs::read_to_string(&path).expect("a source file reads");
            for (offset, line) in source.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if PRIMITIVES.iter().any(|primitive| line.contains(primitive)) {
                    offenders.push(format!("{file_name}:{}", offset + 1));
                }
            }
        }

        assert!(
            files_searched > 0,
            "no file was searched for the primitives, so this proves nothing"
        );
        assert_eq!(
            allowed_seen,
            ALLOWED.len(),
            "an allow-listed file is not there under that name any more, so \
             the allowlist is stale and the invariant it encodes is not the \
             one being asserted"
        );
        assert!(
            offenders.is_empty(),
            "these commit or replay a multi-write journal without going \
             through `TaskWriteReconciler`, so a task record they land would \
             leave the list index and Today's cache behind: {offenders:?}"
        );
    }
}
