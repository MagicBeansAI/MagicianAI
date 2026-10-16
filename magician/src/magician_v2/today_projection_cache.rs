//! A short-lived cache for the `/today` projection.
//!
//! `/today`'s output is bounded and its input is not.
//! `TODAY_SECTION_COLLECTION_LIMIT` caps the projected set at 1,050 rows, but
//! the collector reaches that cap by reading the whole task corpus several
//! times per request, plus a per-task read of the attention projection, the
//! monitor update ledger, and on-disk knowledge and thinking maps. And Today
//! is **polled**, not loaded: each open client runs refresh timers, and a
//! single Today view issues a preview plus section pages. So the corpus is
//! walked several times per client per minute, and the cost grows with every
//! task the owner accumulates while the response size does not.
//!
//! What is cached is the **five `Vec<TodayItem>` lanes**, not the response.
//! Sectioning and paging depend on `section`, `cursor` and `limit`, which vary
//! per request; the lanes do not. So one computation serves preview mode *and*
//! every section page in the same window.
//!
//! # The key
//!
//! `(principal, workspace, the date the projection was computed for)`.
//!
//! **The date is not optional.** Every Today date predicate derives from it, so
//! two readers in different timezones must never share an entry — and section
//! membership changes at midnight with no write at all, so a projection built
//! yesterday must never serve today. The date is the READER's, from `today=`,
//! falling back to the UTC date when absent (see `feed_api`'s
//! `resolve_today_reader_date`).
//!
//! **The scope half is normalised to the directory the scope lives in**, the
//! same normalisation the list index applies, so two spellings of one scope are
//! one entry rather than two that can never invalidate each other. See
//! [`TodayCacheKey`].
//!
//! # Invalidation is load-bearing, not hygiene
//!
//! Both clients refetch the page they are on after removing a row. A stale
//! cache hit on that refetch would **resurrect the row the reader just
//! dismissed** — worse than the drain the refetch was added to fix. So any
//! Today-affecting write calls [`TodayProjectionCache::invalidate_scope`]
//! after its write commits, and that makes every date the scope holds
//! unservable: a reader may hold an entry either side of UTC midnight and
//! both are now wrong. One scope's write never touches another's entries.
//!
//! ## Stamp on write, not drop on write
//!
//! An invalidation **bumps a per-scope write counter**; it does not walk the
//! map removing rows. An entry records the counter value that was current
//! when its computation *started*, and [`TodayProjectionCache::get`] serves it
//! only while the scope's counter still reads the same. The staleness
//! guarantee is the one drop-on-write gave — a write makes every entry the
//! scope holds unservable, at once, for every date — and two things improve:
//!
//! - **The write path stops paying for the read path.** `invalidate_scope`
//!   was a `DashMap::retain` over every entry in the process, and *every task
//!   record write lands there*, including the status churn of a running
//!   execution. It is now one atomic increment against one scope's counter.
//! - **A write that lands mid-computation can no longer be overwritten by the
//!   projection it invalidated.** That was gap 3 below, and it was the worst
//!   of the three: the pre-write projection landed *after* the invalidation
//!   and then served for a full TTL. A computation now carries the stamp it
//!   started under, so [`TodayProjectionCache::insert_if_unwritten_since`]
//!   declines to store a projection a write has already overtaken.
//!
//! **Two things it still does not cover, and each can be up to one TTL late:**
//!
//! 1. Anything that writes `FeedStore` without going through `FeedApi` — the
//!    V3 projection adapter, the feed materializer, the task and
//!    agent-learning projections, and the monitors API all do. A row one of
//!    those adds or removes reaches the reader on the next miss, not at once.
//! 2. A snooze expiring. `today_apply_visibility_state` compares
//!    `snoozed_until` against the projection's own `generated_at`, so the
//!    expiry happens *inside* the cached region and there is no write to
//!    invalidate on. A row whose snooze runs out mid-window stays hidden
//!    until the entry ages out.
//!
//! # Failure
//!
//! The cache is best effort. A missing or expired entry means compute.
//! **Cache failure must never fail a request** — the same rule the list index
//! follows, for the same reason: a cache that errors its caller has stopped
//! being a cache. Nothing here returns an error, and nothing here can panic on
//! a miss.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use dashmap::DashMap;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::feed::types::{FeedAction, FeedItemStatus};

/// How long one computed projection may serve.
///
/// Five seconds would only ever collapse *concurrent* requests, since clients
/// poll at 20–30s. Ten also catches a single client's own repeat, and a Today
/// view already issues a preview plus section pages inside one window.
pub const TODAY_PROJECTION_CACHE_TTL: Duration = Duration::from_secs(10);

/// What one cached projection is keyed by.
///
/// Two readers in different timezones ask for different dates and must never
/// share an entry, so the date is part of the key rather than an attribute of
/// the value.
///
/// # The scope is the DIRECTORY the scope lives in, not the caller's spelling
///
/// `ArtifactV2Workspace::scope_dir_segments` maps `:` `/` `\` `*` `?` `"` `<`
/// `>` `|` to `_` and folds empty, `.` and `..` to `default`, so `user:1` and
/// `user_1` are **one scope with one task directory**. Keying on the raw
/// request strings made them two entries and, worse, made a write under one
/// spelling leave the other reader's projection standing for a full TTL —
/// [`TodayProjectionCache::invalidate_scope`] compared raw strings too, so
/// nothing about the two spellings ever met. Same defect family as the
/// principal who queried the list index as `user:<id>` and was served an
/// empty list in silence.
///
/// So this normalises on the way in, exactly as `TaskWriteReconciler::list_scope`
/// does for the index, and the fields are **private**: a struct literal would
/// be a second way to build a key, and it would be the un-normalised one.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TodayCacheKey {
    principal: String,
    workspace: String,
    /// The date the projection's predicates were evaluated against,
    /// `YYYY-MM-DD`. The READER's date, from `today=`, falling back to the UTC
    /// date when absent.
    date: String,
}

impl TodayCacheKey {
    /// `principal` and `workspace` are normalised to the directory names the
    /// scope is stored under; `date` is taken as given, since it is already
    /// canonical `YYYY-MM-DD`.
    pub fn new(principal: &str, workspace: &str, date: impl Into<String>) -> Self {
        let (principal, workspace) = ArtifactV2Workspace::scope_dir_segments(principal, workspace);
        Self {
            principal,
            workspace,
            date: date.into(),
        }
    }
}

/// Everything `today()` derives from the corpus before it pages.
///
/// The five lanes are held **after** visibility filtering, which is safe
/// precisely because dismiss/snooze state is per-scope and the scope is in the
/// key. `generated_at` travels with them so a served projection reports the
/// instant it was actually built rather than the instant it was handed out.
#[derive(Clone, Debug, Default)]
pub struct TodayProjection {
    pub needs_you: Vec<TodayItem>,
    pub delivered: Vec<TodayItem>,
    pub changed: Vec<TodayItem>,
    pub active_work: Vec<TodayItem>,
    pub followups: Vec<TodayItem>,
    /// Unix ms at which this projection was computed.
    pub generated_at: i64,
}

/// Where the projection behind one `/today` response came from.
///
/// The response reports this as `freshness.source`. `generated_at` already
/// carried the instant the projection was built, but `source` was a constant
/// — so a projection handed out nine seconds after it was computed described
/// itself as live, and the one field a reader would check to find out was the
/// one that could not tell them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TodayProjectionSource {
    /// Computed from the corpus for this request.
    Computed,
    /// Served from a live entry an earlier request built.
    Cache,
}

impl TodayProjectionSource {
    /// The spelling that goes on the wire.
    ///
    /// `live_projection` is what every response said before the cache
    /// existed, so it stays the name for the computed case and only the
    /// genuinely-cached case is new.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Computed => "live_projection",
            Self::Cache => "cached_projection",
        }
    }
}

/// The scope half of a [`TodayCacheKey`], normalised the same way, and the
/// key of the per-scope write counter.
///
/// Its own type rather than a bare tuple so the counter map cannot be indexed
/// with a raw, un-normalised pair by accident — the defect that made `user:1`
/// and `user_1` two caches neither of which could invalidate the other.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct ScopeKey {
    principal: String,
    workspace: String,
}

impl ScopeKey {
    fn new(principal: &str, workspace: &str) -> Self {
        let (principal, workspace) = ArtifactV2Workspace::scope_dir_segments(principal, workspace);
        Self {
            principal,
            workspace,
        }
    }
}

/// How many Today-affecting writes a scope has taken.
///
/// Monotonic and never reset. `u64` at even a million writes a second would
/// need half a million years to wrap, so an entry can never be revived by a
/// counter that came back around to its stamp.
#[derive(Debug, Default)]
struct ScopeWrites(AtomicU64);

impl ScopeWrites {
    fn load(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Returns the value *after* the bump, so a caller can log or assert on it.
    fn bump(&self) -> u64 {
        self.0.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }
}

#[derive(Clone, Debug)]
struct CachedProjection {
    projection: TodayProjection,
    stored_at: Instant,
    /// The scope's write count when this projection's computation **started**.
    ///
    /// Started, not finished: a write that lands while the corpus is being
    /// walked makes the result stale the moment it is produced, and stamping
    /// at the end would record a number that says otherwise.
    write_stamp: u64,
}

/// A per-scope cache of the Today projection.
///
/// **Owned by `ArtifactV2Service`, borrowed by `FeedApi`.** The reads live in
/// the API and the task writes live in the service, and both have to reach
/// the same map or a task completed on `/tasks` would sit in Today's
/// Follow-ups until the entry aged out. The backend builds one service and
/// hands `Arc` clones to every actix worker, so this is process-wide there,
/// while a test that builds its own service gets its own cache. That last
/// part matters: the key is the *scope*, not the workspace root, and every
/// test in this crate uses the same scope strings over a different temporary
/// directory. A single global map would let one test's projection serve
/// another test's reader.
#[derive(Debug)]
pub struct TodayProjectionCache {
    entries: DashMap<TodayCacheKey, CachedProjection>,
    /// One monotonic write counter per scope.
    ///
    /// Bounded by the number of scopes the process has ever seen a write or a
    /// read for — a handful — so nothing sweeps it. Entries are 16 bytes plus
    /// two short strings; dropping one would only mean the next reader
    /// restarted from zero against an entry stamped higher, which reads as
    /// "written since" and merely costs a recompute.
    writes: DashMap<ScopeKey, ScopeWrites>,
    ttl: Duration,
}

impl Default for TodayProjectionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TodayProjectionCache {
    pub fn new() -> Self {
        Self::with_ttl(TODAY_PROJECTION_CACHE_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            entries: DashMap::new(),
            writes: DashMap::new(),
            ttl,
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The scope's current write count.
    ///
    /// **Read this BEFORE starting a computation** and hand it back to
    /// [`Self::insert_if_unwritten_since`]. Reading it afterwards would stamp
    /// a projection with a number that post-dates the writes it missed.
    pub fn write_stamp(&self, principal: &str, workspace: &str) -> u64 {
        self.stamp_of(&ScopeKey::new(principal, workspace))
    }

    /// The key's two halves are already the normalised directory segments, so
    /// this builds the counter key directly rather than re-normalising —
    /// `scope_dir_segments` is idempotent, but reading a counter must not
    /// depend on that.
    fn write_stamp_for(&self, key: &TodayCacheKey) -> u64 {
        self.stamp_of(&ScopeKey {
            principal: key.principal.clone(),
            workspace: key.workspace.clone(),
        })
    }

    /// Reads the counter and **drops the map guard before returning**. Nothing
    /// may hold a `writes` guard while reaching for `entries`: `store`'s sweep
    /// goes the other way round, and two orders is a deadlock.
    fn stamp_of(&self, scope: &ScopeKey) -> u64 {
        match self.writes.get(scope) {
            Some(writes) => writes.load(),
            None => 0,
        }
    }

    /// The live projection for `key`, or `None` when there is none, it has
    /// aged out, or a write has landed in its scope since it was computed.
    ///
    /// The read guard is dropped before this returns, so a caller may compute
    /// while holding nothing. **No `DashMap` guard may be held across an
    /// `await`** — the async caller's whole reason for using `get` + `insert`
    /// rather than [`Self::get_or_insert_with`] is that its computation is
    /// asynchronous and must happen outside the map.
    pub fn get(&self, key: &TodayCacheKey) -> Option<TodayProjection> {
        let stamp = self.write_stamp_for(key);
        let entry = self.entries.get(key)?;
        if entry.stored_at.elapsed() >= self.ttl || entry.write_stamp != stamp {
            return None;
        }
        Some(entry.projection.clone())
    }

    /// Store a freshly computed projection under the scope's *current* write
    /// count.
    ///
    /// For a computation short enough that "before" and "after" are the same
    /// instant — a test fixture, or a synchronous
    /// [`Self::get_or_insert_with`]. An asynchronous computation that walks
    /// the corpus must use [`Self::insert_if_unwritten_since`] with the stamp
    /// it read before it began, or a write that landed mid-walk is stamped as
    /// having been included.
    ///
    /// The sweep keeps the map bounded without a background task: entries
    /// otherwise accumulate one per scope per date, and a reader who travels
    /// leaves the dates they used behind. An entry a write has out-stamped is
    /// swept on TTL like any other — see [`Self::store`].
    pub fn insert(&self, key: TodayCacheKey, projection: TodayProjection) {
        let stamp = self.write_stamp_for(&key);
        self.store(key, projection, stamp);
    }

    /// Store a projection **only if** no write has landed in its scope since
    /// `stamp` was read.
    ///
    /// The asynchronous caller's insert. A `/today` computation walks the
    /// corpus; a task write that commits during that walk makes the result
    /// stale before it exists, and storing it anyway is how a reader came to
    /// be served pre-write state for a full TTL *after* the invalidation had
    /// already run. Returns whether it was stored, so a caller can report the
    /// projection as freshly computed either way — the value it hands back is
    /// still the newest one it has, it just may not be worth keeping.
    pub fn insert_if_unwritten_since(
        &self,
        key: TodayCacheKey,
        projection: TodayProjection,
        stamp: u64,
    ) -> bool {
        if self.write_stamp_for(&key) != stamp {
            return false;
        }
        self.store(key, projection, stamp);
        true
    }

    /// The sweep is TTL-only, deliberately.
    ///
    /// An entry a write has already made unservable is *also* at most one TTL
    /// from ageing out, so nothing accumulates that would not have anyway, and
    /// a stamp comparison here would mean reaching into `writes` while holding
    /// an `entries` shard guard — the reverse of the order every read takes.
    /// Bounding the map is worth a sweep; a lock-order inversion is not.
    fn store(&self, key: TodayCacheKey, projection: TodayProjection, write_stamp: u64) {
        let ttl = self.ttl;
        self.entries
            .retain(|_, entry| entry.stored_at.elapsed() < ttl);
        self.entries.insert(
            key,
            CachedProjection {
                projection,
                stored_at: Instant::now(),
                write_stamp,
            },
        );
    }

    /// Serve `key` from the cache, or compute and store it.
    ///
    /// `compute` runs while no map guard is held, and is **not** run at all on
    /// a live hit. Synchronous by design: an async computation must use
    /// [`Self::write_stamp`], [`Self::get`] and
    /// [`Self::insert_if_unwritten_since`] so nothing straddles an `await`.
    pub fn get_or_insert_with<F>(&self, key: &TodayCacheKey, compute: F) -> TodayProjection
    where
        F: FnOnce() -> TodayProjection,
    {
        if let Some(hit) = self.get(key) {
            return hit;
        }
        let stamp = self.write_stamp_for(key);
        let projection = compute();
        // `compute` is synchronous, so nothing could have written during it
        // from this thread — but another thread could, and then this
        // projection is exactly the stale one that must not be stored.
        self.insert_if_unwritten_since(key.clone(), projection.clone(), stamp);
        projection
    }

    /// Make every entry this scope holds unservable, whatever date it was
    /// computed for.
    ///
    /// A write invalidates the SCOPE, not one date bucket: a reader may hold
    /// an entry either side of UTC midnight and a write makes both wrong.
    /// Another scope's entries are untouched — one owner's dismissal must not
    /// cost every other reader their projection.
    ///
    /// **One atomic increment**, not a walk of the map. Every task record
    /// write reaches here, twice per tick during a run, and a `retain` over
    /// every entry in the process was the wrong thing to put on that path.
    /// The entries themselves are reclaimed by the next [`Self::store`] sweep.
    ///
    /// **Normalised through the same funnel [`TodayCacheKey::new`] uses**, so
    /// a writer holding `user:1` clears the entry a reader spelled `user_1`
    /// and vice versa. They are one task directory; comparing the raw strings
    /// made them two scopes that could never invalidate each other.
    pub fn invalidate_scope(&self, principal: &str, workspace: &str) {
        let scope = ScopeKey::new(principal, workspace);
        // The steady state is a counter that already exists, and that path
        // takes a shard READ lock and one `fetch_add` — the write path of a
        // running execution reaches here twice a tick and must not queue
        // behind a map mutation. The insert runs once per scope, ever.
        if let Some(writes) = self.writes.get(&scope) {
            writes.bump();
            return;
        }
        self.writes
            .entry(scope)
            .or_insert_with(ScopeWrites::default)
            .bump();
    }

    /// Entries currently held, live or aged out. Diagnostics only.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    use super::TodaySectionId;
    use crate::magician_v2::feed::FeedItemStatus;

    fn key(principal: &str, workspace: &str, date: &str) -> TodayCacheKey {
        TodayCacheKey::new(principal, workspace, date)
    }

    fn today_item(id: &str, section: TodaySectionId) -> TodayItem {
        TodayItem {
            id: id.to_string(),
            principal: "alice".to_string(),
            workspace: "default".to_string(),
            section,
            priority: 100,
            title: id.to_string(),
            summary: None,
            reason: "fixture".to_string(),
            source_kind: "task".to_string(),
            source_id: id.to_string(),
            source_url: None,
            space_ids: Vec::new(),
            thread_id: None,
            task_id: None,
            agent_id: None,
            status: FeedItemStatus::Info,
            actions: Vec::new(),
            evidence_refs: Vec::new(),
            created_at: 1,
            updated_at: 1,
            expires_at: None,
            seen_at: None,
            dismissed_at: None,
            snoozed_until: None,
            metadata: serde_json::Value::Null,
        }
    }

    /// Every lane is populated from the same ids, each with its own suffix, so
    /// a cache that carried only one of the five would be caught rather than
    /// silently pass a `needs_you`-only assertion.
    fn projection_of(ids: &[&str]) -> TodayProjection {
        let lane = |suffix: &str, section: TodaySectionId| {
            ids.iter()
                .map(|id| {
                    today_item(
                        &if suffix.is_empty() {
                            (*id).to_string()
                        } else {
                            format!("{id}-{suffix}")
                        },
                        section,
                    )
                })
                .collect::<Vec<_>>()
        };
        TodayProjection {
            needs_you: lane("", TodaySectionId::NeedsYou),
            delivered: lane("delivered", TodaySectionId::Delivered),
            changed: lane("changed", TodaySectionId::Changed),
            active_work: lane("active", TodaySectionId::ActiveWork),
            followups: lane("followup", TodaySectionId::Followups),
            generated_at: 1,
        }
    }

    fn lane_ids(items: &[TodayItem]) -> Vec<&str> {
        items.iter().map(|item| item.id.as_str()).collect()
    }

    #[test]
    fn a_second_read_inside_the_window_reuses_the_first() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));
        let key = key("alice", "default", "2026-07-31");

        let first = cache.get_or_insert_with(&key, || projection_of(&["a"]));
        let second = cache.get_or_insert_with(&key, || projection_of(&["DIFFERENT"]));

        // The closure must not have run the second time — proved by the value
        // returned, not by a call counter, so the assertion cannot pass
        // vacuously against a cache that recomputes and discards.
        assert_eq!(lane_ids(&second.needs_you), lane_ids(&first.needs_you));
        assert_eq!(lane_ids(&second.needs_you), vec!["a"]);
        // All five lanes come back, not just the first one. A cache that
        // stored one lane and rebuilt the rest would pass the assertion above.
        assert_eq!(lane_ids(&second.delivered), vec!["a-delivered"]);
        assert_eq!(lane_ids(&second.changed), vec!["a-changed"]);
        assert_eq!(lane_ids(&second.active_work), vec!["a-active"]);
        assert_eq!(lane_ids(&second.followups), vec!["a-followup"]);
    }

    #[test]
    fn a_different_date_is_a_different_projection() {
        // Section membership changes at midnight with no write at all, so a
        // projection computed yesterday must never serve today.
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));

        let yesterday =
            cache.get_or_insert_with(&key("a", "d", "2026-07-30"), || projection_of(&["old"]));
        let today =
            cache.get_or_insert_with(&key("a", "d", "2026-07-31"), || projection_of(&["new"]));

        assert_eq!(lane_ids(&yesterday.needs_you), vec!["old"]);
        assert_eq!(
            lane_ids(&today.needs_you),
            vec!["new"],
            "a date dropped from the key would serve yesterday's projection today"
        );
        // And yesterday's entry is still its own: the later date did not
        // overwrite it, which is what a one-entry-per-scope cache would do.
        assert_eq!(
            lane_ids(
                &cache
                    .get(&key("a", "d", "2026-07-30"))
                    .expect("yesterday's entry survives today's")
                    .needs_you
            ),
            vec!["old"]
        );
    }

    #[test]
    fn a_different_scope_is_a_different_projection() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));

        cache.get_or_insert_with(&key("alice", "default", "2026-07-31"), || {
            projection_of(&["alice-row"])
        });
        let bob = cache.get_or_insert_with(&key("bob", "default", "2026-07-31"), || {
            projection_of(&["bob-row"])
        });
        let workspace_neighbour = cache
            .get_or_insert_with(&key("alice", "other-workspace", "2026-07-31"), || {
                projection_of(&["other-workspace-row"])
            });

        assert_eq!(
            lane_ids(&bob.needs_you),
            vec!["bob-row"],
            "one reader must never see another's Today"
        );
        assert_eq!(
            lane_ids(&workspace_neighbour.needs_you),
            vec!["other-workspace-row"],
            "the workspace is part of the key, not only the principal"
        );
    }

    #[test]
    fn an_expired_entry_recomputes() {
        let cache = TodayProjectionCache::with_ttl(Duration::ZERO);
        let key = key("a", "d", "2026-07-31");

        cache.get_or_insert_with(&key, || projection_of(&["first"]));
        let second = cache.get_or_insert_with(&key, || projection_of(&["second"]));

        assert_eq!(lane_ids(&second.needs_you), vec!["second"]);
        assert!(
            cache.get(&key).is_none(),
            "an entry past its TTL is never served, however it was asked for"
        );
    }

    #[test]
    fn invalidating_a_scope_drops_every_date_it_holds() {
        // A write invalidates the SCOPE, not one date bucket: the reader may
        // hold an entry either side of UTC midnight and both are now wrong.
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));
        cache.get_or_insert_with(&key("a", "d", "2026-07-30"), || projection_of(&["x"]));
        cache.get_or_insert_with(&key("a", "d", "2026-07-31"), || projection_of(&["y"]));
        cache.get_or_insert_with(&key("other", "d", "2026-07-31"), || {
            projection_of(&["kept"])
        });

        cache.invalidate_scope("a", "d");

        let after = cache.get_or_insert_with(&key("a", "d", "2026-07-31"), || {
            projection_of(&["recomputed"])
        });
        assert_eq!(lane_ids(&after.needs_you), vec!["recomputed"]);
        let yesterday = cache.get_or_insert_with(&key("a", "d", "2026-07-30"), || {
            projection_of(&["also-gone"])
        });
        assert_eq!(
            lane_ids(&yesterday.needs_you),
            vec!["also-gone"],
            "the other side of midnight is just as wrong after a write"
        );
        let untouched = cache.get_or_insert_with(&key("other", "d", "2026-07-31"), || {
            projection_of(&["MISS"])
        });
        assert_eq!(
            lane_ids(&untouched.needs_you),
            vec!["kept"],
            "one scope's write must not clear another's"
        );
    }

    /// **Two spellings of one scope are one entry, and a write under either
    /// clears it.**
    ///
    /// `user:1` and `user_1` are the same task directory — `safe_segment` maps
    /// `:` and `/` to `_` — so they are the same reader's Today. Keyed raw they
    /// were two entries that could never invalidate one another: a write
    /// arriving as `user:1` left the `user_1` reader a projection built before
    /// it, for a full TTL, silently.
    ///
    /// Both directions are asserted, because the bug is not symmetric-looking
    /// from either side alone: the writer and the reader each believe they used
    /// the scope's name.
    #[test]
    fn two_spellings_of_one_scope_are_one_entry_and_one_write_clears_both() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));

        cache.insert(
            key("user:1", "work/space", "2026-07-31"),
            projection_of(&["one-scope"]),
        );
        cache.insert(
            key("user_1", "work_space", "2026-07-31"),
            projection_of(&["one-scope"]),
        );
        assert_eq!(
            cache.len(),
            1,
            "one scope directory holds one projection per date; two entries \
             means two readers of the same tasks are caching separately and \
             neither write can reach the other"
        );
        assert_eq!(
            lane_ids(
                &cache
                    .get(&key("user_1", "work_space", "2026-07-31"))
                    .expect("either spelling reads the one entry")
                    .needs_you
            ),
            vec!["one-scope"]
        );

        // A write that arrives normalised clears the entry a raw-spelled
        // reader is holding.
        cache.invalidate_scope("user_1", "work_space");
        assert!(
            cache
                .get(&key("user:1", "work/space", "2026-07-31"))
                .is_none(),
            "a write under the directory spelling has to clear the reader who \
             spelled the scope raw"
        );

        // And a write that arrives raw clears the entry a normalised reader is
        // holding — the direction the task writers actually take, since
        // `TaskWriteReconciler` passes the caller's `ScopeRef` through.
        cache.insert(
            key("user_1", "work_space", "2026-07-31"),
            projection_of(&["reseeded"]),
        );
        cache.invalidate_scope("user:1", "work/space");
        assert!(
            cache
                .get(&key("user_1", "work_space", "2026-07-31"))
                .is_none(),
            "a write under the raw spelling has to clear the reader who \
             spelled the scope the way the directory is named"
        );

        // Normalising must not over-match: a neighbour that merely sanitises
        // to something similar keeps its projection.
        cache.insert(
            key("user:2", "work/space", "2026-07-31"),
            projection_of(&["neighbour"]),
        );
        cache.invalidate_scope("user:1", "work/space");
        assert_eq!(
            lane_ids(
                &cache
                    .get(&key("user_2", "work_space", "2026-07-31"))
                    .expect("another scope's entry survives this scope's write")
                    .needs_you
            ),
            vec!["neighbour"],
            "one scope's write must not clear another's, however either is spelled"
        );
    }

    /// **Two requests with no write between them compute once.**
    ///
    /// The plain-language statement of what the cache is for, asserted through
    /// the async-shaped API (`write_stamp` → compute → `insert_if_unwritten_since`)
    /// rather than through `get_or_insert_with`, because that is the pair
    /// `/today` actually uses and the pair a stamp bug would break.
    #[test]
    fn two_reads_with_no_write_between_them_compute_once() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));
        let key = key("alice", "default", "2026-07-31");

        let stamp = cache.write_stamp("alice", "default");
        assert!(
            cache.insert_if_unwritten_since(key.clone(), projection_of(&["first"]), stamp),
            "nothing wrote, so the projection has to be worth keeping"
        );

        assert_eq!(
            lane_ids(
                &cache
                    .get(&key)
                    .expect("the second read is served from the first computation")
                    .needs_you
            ),
            vec!["first"]
        );
    }

    /// **A write between two reads recomputes.**
    ///
    /// The same staleness guarantee drop-on-write gave, now expressed as a
    /// counter the entry disagrees with rather than as a removed row.
    #[test]
    fn a_write_between_two_reads_recomputes() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));
        let key = key("alice", "default", "2026-07-31");

        cache.insert(key.clone(), projection_of(&["before-the-write"]));
        assert!(
            cache.get(&key).is_some(),
            "the entry has to be servable first, or the assertion below proves \
             nothing"
        );

        cache.invalidate_scope("alice", "default");

        assert!(
            cache.get(&key).is_none(),
            "a task write in this scope makes every projection over it stale, \
             whether the entry was removed or merely out-stamped"
        );
    }

    /// **A write that lands mid-computation is not overwritten by the
    /// projection it invalidated.**
    ///
    /// This is the case drop-on-write could not express and documented as a
    /// known gap: the invalidation ran against a map the in-flight compute had
    /// not inserted into yet, so the pre-write projection landed *after* the
    /// drop and then served for a full TTL. The reader who had just changed
    /// something was handed the state from before their change.
    ///
    /// The interleaving is written out rather than raced, so the test asserts
    /// the ordering rule instead of hoping to hit it.
    #[test]
    fn a_projection_a_write_overtook_is_never_stored() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));
        let key = key("alice", "default", "2026-07-31");

        // The reader starts a walk of the corpus.
        let stamp = cache.write_stamp("alice", "default");
        // A task record commits while that walk is still running.
        cache.invalidate_scope("alice", "default");
        // The walk finishes and offers a projection built from the pre-write
        // corpus.
        let stored =
            cache.insert_if_unwritten_since(key.clone(), projection_of(&["pre-write"]), stamp);

        assert!(
            !stored,
            "the walk read the corpus before the write, so its projection is \
             stale the moment it exists and must not be stored"
        );
        assert!(
            cache.get(&key).is_none(),
            "and no reader may be served it — this is the case where the \
             invalidation ran before the insert it was meant to prevent"
        );
    }

    /// One scope's writes must not out-stamp another's entries. The
    /// counter is per scope, so this is the counter's version of
    /// `invalidating_a_scope_drops_every_date_it_holds`'s last assertion.
    #[test]
    fn a_write_stamp_is_per_scope() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));
        let mine = key("alice", "default", "2026-07-31");
        let theirs = key("bob", "default", "2026-07-31");

        cache.insert(mine.clone(), projection_of(&["alice-row"]));
        cache.insert(theirs.clone(), projection_of(&["bob-row"]));

        cache.invalidate_scope("alice", "default");

        assert!(cache.get(&mine).is_none());
        assert_eq!(
            lane_ids(
                &cache
                    .get(&theirs)
                    .expect("another owner's write must not cost this one a recompute")
                    .needs_you
            ),
            vec!["bob-row"]
        );
        assert_eq!(
            cache.write_stamp("bob", "default"),
            0,
            "and their counter must not have moved either"
        );
    }

    /// The counter is keyed by the scope DIRECTORY, exactly as the entries
    /// are, so a write spelled one way out-stamps a reader who spelled it the
    /// other. Keyed raw, the two would be separate counters and neither could
    /// ever reach the other's entry — the same defect the entry key already
    /// had, reintroduced one level down.
    #[test]
    fn the_write_counter_is_keyed_by_the_scope_directory() {
        let cache = TodayProjectionCache::with_ttl(Duration::from_secs(10));

        cache.insert(
            key("user_1", "work_space", "2026-07-31"),
            projection_of(&["one-scope"]),
        );
        cache.invalidate_scope("user:1", "work/space");

        assert!(
            cache
                .get(&key("user_1", "work_space", "2026-07-31"))
                .is_none(),
            "the raw and normalised spellings are one task directory, so they \
             have to be one counter"
        );
        assert_eq!(cache.write_stamp("user_1", "work_space"), 1);
        assert_eq!(
            cache.write_stamp("user:1", "work/space"),
            1,
            "and either spelling has to read the same counter"
        );
    }

    #[test]
    fn an_insert_sweeps_entries_that_have_already_aged_out() {
        // Entries accumulate one per scope per date. Nothing sweeps them in
        // the background, so an insert has to, or a long-lived process keeps
        // every date every reader ever asked for.
        let cache = TodayProjectionCache::with_ttl(Duration::ZERO);
        cache.insert(key("a", "d", "2026-07-30"), projection_of(&["stale"]));
        cache.insert(key("a", "d", "2026-07-31"), projection_of(&["stale-too"]));

        cache.insert(key("a", "d", "2026-08-01"), projection_of(&["newest"]));

        assert_eq!(
            cache.len(),
            1,
            "an insert drops what has aged out instead of growing forever"
        );
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TodayItem {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub section: TodaySectionId,
    pub priority: i64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub reason: String,
    pub source_kind: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub status: FeedItemStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<FeedAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<i64>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TodaySectionId {
    NeedsYou,
    Delivered,
    Changed,
    ActiveWork,
    Spaces,
    Followups,
}
