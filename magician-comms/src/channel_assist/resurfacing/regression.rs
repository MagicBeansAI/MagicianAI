//! End-to-end regression harness for the Proactive Resurfacing Engine.
//!
//! Every other test in this module tree is a per-unit test (one scorer signal,
//! one store method, one curator branch). This file is the ONE deterministic
//! integration test that ties the whole pipeline together —
//! **scorer → curator → feedback → retention** — over a real temp SQLite store,
//! so that if any stage regresses in isolation the seam between stages fails
//! loudly here.
//!
//! Everything is driven by deterministic doubles ([`MockSource`],
//! [`MapCentrality`], [`TestCtx`]); there is NO real embedder, LLM, router, or
//! network, and no wall-clock — every `now` is an explicit unix-second constant.
//! The salience math the assertions pin is documented inline: with `access_count
//! == 0` the frequency/co-occurrence/dormancy signals are all `0`, and the
//! date-free titles/digests keep the temporal-anchor signal `0`, so each item's
//! score reduces to exactly:
//!
//! ```text
//! salience = W_RECENCY * recency + W_CENTRALITY * centrality
//!          = 0.30 * 0.5^(age_days / 14) + 0.20 * density
//! ```
//!
//! where `density` is the value [`MapCentrality`] hands the scorer for that item.
//! Freshly-scored rows always have `last_scored_at == now`, so the per-pass decay
//! sweep is a no-op on them (`0.5^0 == 1.0`) and the stored score equals the
//! computed score — that invariant is what lets the tests assert exact numbers.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;

use super::curator::run_curation_pass_with_attention;
use magician::magician_v2::attention::resurfacing::centrality::{
    CentralityOutcome, CentralityProvider,
};
use magician::magician_v2::attention::resurfacing::scorer::run_scorer_pass;
use magician::magician_v2::attention::resurfacing::scoring::{
    utility_multiplier, ResurfacingScoringConfig, ScoreCtx,
};
use magician::magician_v2::attention::resurfacing::sources::ResurfacingSource;
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;
use magician::magician_v2::attention::resurfacing::types::{
    candidate_id, CandidateState, CorpusItem, FeedbackAction, SourceKind,
};

/// One day in unix seconds; every timestamp below is expressed as `day(n)`.
const DAY: i64 = 86_400;
const MAP_EMBEDDING_CONTRACT: &str = "resurfacing-regression-map-v1";

/// A timestamp `n` whole days after the epoch — keeps the recency arithmetic in
/// the tests exact (age is always an integer number of days).
fn day(n: i64) -> i64 {
    n * DAY
}

/// Assert `actual ≈ expected` within `tol`, with a labelled failure message.
fn assert_close(actual: f32, expected: f32, tol: f32, what: &str) {
    assert!(
        (actual - expected).abs() < tol,
        "{what}: expected ~{expected}, got {actual}",
    );
}

// --- Deterministic test doubles ---------------------------------------------

/// An in-memory corpus source holding a fixed item set. `list_changed_since`
/// filters `watermark_cursor > watermark` (strict, matching the trait contract)
/// so a re-run at an advanced watermark yields nothing — exactly how the real
/// sources behave.
struct MockSource {
    kind: &'static str,
    items: Vec<CorpusItem>,
}

impl MockSource {
    fn new(kind: &'static str, items: Vec<CorpusItem>) -> Self {
        Self { kind, items }
    }
}

#[async_trait]
impl ResurfacingSource for MockSource {
    fn corpus_kind(&self) -> &'static str {
        self.kind
    }

    async fn list_changed_since(
        &self,
        _principal: &str,
        _workspace: &str,
        watermark: i64,
    ) -> Result<Vec<CorpusItem>> {
        Ok(self
            .items
            .iter()
            .filter(|it| it.watermark_cursor > watermark)
            .cloned()
            .collect())
    }
}

/// A [`CentralityProvider`] whose per-item outcome is looked up by the item's
/// exact `embedding_text`. Each known text maps to a chosen `(density, embedding)`
/// so both the centrality SIGNAL (via density) and the semantic RELATIONSHIPS
/// (via the embedding, for dismiss suppression / cross-session penalty) are fully
/// deterministic. An unknown text returns `None` — the scorer's graceful path
/// (0.0 centrality, no persisted embedding), same as `NoCentrality`.
struct MapCentrality {
    table: HashMap<String, (f32, Vec<f32>)>,
}

impl MapCentrality {
    fn new(entries: &[(&str, f32, &[f32])]) -> Self {
        let table = entries
            .iter()
            .map(|(text, density, emb)| ((*text).to_string(), (*density, emb.to_vec())))
            .collect();
        Self { table }
    }
}

#[async_trait]
impl CentralityProvider for MapCentrality {
    fn embedding_contract(&self) -> Option<&str> {
        Some(MAP_EMBEDDING_CONTRACT)
    }

    fn embedding_dimensions(&self) -> Option<usize> {
        Some(2)
    }

    async fn evaluate(&self, embedding_text: &str) -> Option<CentralityOutcome> {
        self.table
            .get(embedding_text)
            .map(|(density, embedding)| CentralityOutcome {
                density: *density,
                embedding: embedding.clone(),
                embedding_contract: Some(MAP_EMBEDDING_CONTRACT.to_string()),
            })
    }
}

/// Fixed-clock scoring ctx with zero access stats. `access_count == 0` zeroes the
/// frequency/co-occurrence/dormancy signals (see the module docs), and
/// `neighbor_density` here is never consulted — the scorer overrides it per item
/// with the [`MapCentrality`] density via its precomputed ctx — so its value is
/// irrelevant; `Ok(0.0)` mirrors the sibling unit tests.
struct TestCtx {
    now: i64,
}

impl ScoreCtx for TestCtx {
    fn now(&self) -> i64 {
        self.now
    }

    fn neighbor_density(&self, _embedding_text: &str) -> Result<f32> {
        Ok(0.0)
    }

    fn access_count(&self, _source_ref: &str) -> u32 {
        0
    }
}

/// A memory-kind corpus item at `occurred_at` with a DATE-FREE title/digest (so
/// the temporal-anchor signal stays `0` and the score reduces to the two-term
/// form in the module docs). `embedding_text` is the [`MapCentrality`] lookup key.
fn corpus_item(source_ref: &str, occurred_at: i64, embedding_text: &str) -> CorpusItem {
    CorpusItem {
        source_kind: SourceKind::Memory,
        source_ref: source_ref.to_string(),
        title: format!("title {source_ref}"),
        digest: format!("digest {source_ref}"),
        content_details: None,
        content_revision: None,
        occurred_at,
        watermark_cursor: occurred_at,
        embedding_text: embedding_text.to_string(),
    }
}

/// Fetch a stored candidate by its memory-kind `source_ref` (panics if absent).
async fn fetch(
    store: &ResurfacingStore,
    source_ref: &str,
) -> magician::magician_v2::attention::resurfacing::types::Candidate {
    store
        .get_candidate("p", "w", &candidate_id(SourceKind::Memory, source_ref))
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("candidate {source_ref} should exist"))
}

// --- 1. Salient items surface, in salience order ----------------------------

/// The scorer→curator seam must surface exactly the top-`cap` items by salience,
/// in descending order, and a recent+central item must beat an old+peripheral
/// one. Six items span the recency×centrality plane; with `now = day(100)` and
/// `score = 0.30*0.5^(age_days/14) + 0.20*density`:
///
/// | ref     | day | age_days | recency  | density | score    |
/// |---------|-----|----------|----------|---------|----------|
/// | alpha   | 100 |  0       | 1.0      | 0.9     | 0.48     |  recent + central
/// | bravo   | 100 |  0       | 1.0      | 0.5     | 0.40     |
/// | charlie |  86 | 14       | 0.5      | 0.9     | 0.33     |  central beats fresher-but-flat
/// | delta   |  93 |  7       | 0.7071   | 0.2     | 0.2521   |
/// | echo    |  72 | 28       | 0.25     | 0.6     | 0.195    |
/// | foxtrot |  44 | 56       | 0.0625   | 0.0     | 0.01875  |  old + peripheral
///
/// So with `cap = 3` the surface is exactly `[alpha, bravo, charlie]`.
#[tokio::test]
async fn e2e_surfaces_top_k_in_salience_order() {
    let store = ResurfacingStore::open_in_temp();
    let now = day(100);

    let src = MockSource::new(
        "memory",
        vec![
            corpus_item("alpha", day(100), "alpha"),
            corpus_item("bravo", day(100), "bravo"),
            corpus_item("charlie", day(86), "charlie"),
            corpus_item("delta", day(93), "delta"),
            corpus_item("echo", day(72), "echo"),
            corpus_item("foxtrot", day(44), "foxtrot"),
        ],
    );
    // Density per item; embeddings are irrelevant here (no dismissal in this
    // test), so each is empty.
    let centrality = MapCentrality::new(&[
        ("alpha", 0.9, &[]),
        ("bravo", 0.5, &[]),
        ("charlie", 0.9, &[]),
        ("delta", 0.2, &[]),
        ("echo", 0.6, &[]),
        ("foxtrot", 0.0, &[]),
    ]);
    let ctx = TestCtx { now };
    let sources: &[&dyn ResurfacingSource] = &[&src];

    let scored = run_scorer_pass(
        "p",
        "w",
        &store,
        sources,
        &ctx,
        &centrality,
        &ResurfacingScoringConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(scored, 6, "all six items are scored");

    let surfaced = run_curation_pass_with_attention("p", "w", &store, 3, now, None, None)
        .await
        .unwrap();

    // Exactly the top-3 by salience, in descending order, all transitioned.
    let refs: Vec<&str> = surfaced.iter().map(|c| c.source_ref.as_str()).collect();
    assert_eq!(refs, vec!["alpha", "bravo", "charlie"]);
    assert!(surfaced.iter().all(|c| c.state == CandidateState::Surfaced));
    assert!(surfaced.iter().all(|c| c.surface_count == 1));

    // Pin the salience math on the clean-valued rows so a scoring-weight or
    // recency-halflife regression trips here.
    let alpha = fetch(&store, "alpha").await;
    let bravo = fetch(&store, "bravo").await;
    let charlie = fetch(&store, "charlie").await;
    let delta = fetch(&store, "delta").await;
    let echo = fetch(&store, "echo").await;
    let foxtrot = fetch(&store, "foxtrot").await;
    assert_close(alpha.salience_score, 0.48, 1e-4, "alpha salience");
    assert_close(bravo.salience_score, 0.40, 1e-4, "bravo salience");
    assert_close(charlie.salience_score, 0.33, 1e-4, "charlie salience");
    assert_close(foxtrot.salience_score, 0.01875, 1e-4, "foxtrot salience");

    // Full strict ordering across the whole corpus (locks the relative ranking).
    assert!(alpha.salience_score > bravo.salience_score);
    assert!(bravo.salience_score > charlie.salience_score);
    assert!(charlie.salience_score > delta.salience_score);
    assert!(delta.salience_score > echo.salience_score);
    assert!(echo.salience_score > foxtrot.salience_score);

    // The headline relationship the engine exists for: recent+central outranks
    // old+peripheral by a wide margin.
    assert!(
        alpha.salience_score > foxtrot.salience_score,
        "a recent, central item must outrank an old, peripheral one",
    );
}

// --- 2. Dismiss suppresses a same-pass near neighbor ------------------------

/// Dismissing a candidate must down-weight its still-live SEMANTIC neighbors
/// in-session (store `suppress_neighbors`, cosine ≥ 0.82 ⇒ salience ×0.5) without
/// touching semantically-distant candidates. Three items are scored to the SAME
/// base salience (identical recency + density); `a`≈`b` in embedding space and
/// `c` is orthogonal. Dismissing `a` halves `b` and leaves `c` untouched, so `c`
/// — previously tied with `b` — now strictly out-competes it for surfacing.
#[tokio::test]
async fn e2e_dismiss_suppresses_same_pass_near_neighbor() {
    let store = ResurfacingStore::open_in_temp();
    let now = day(100);

    let src = MockSource::new(
        "memory",
        vec![
            corpus_item("a", day(100), "a"),
            corpus_item("b", day(100), "b"),
            corpus_item("c", day(100), "c"),
        ],
    );
    // Equal density ⇒ equal base score (0.30 + 0.20*0.5 = 0.40). a & b embed
    // near-parallel (cosine ~0.999 ≥ 0.82); c is orthogonal (cosine 0).
    let centrality = MapCentrality::new(&[
        ("a", 0.5, &[1.0, 0.0]),
        ("b", 0.5, &[0.95, 0.05]),
        ("c", 0.5, &[0.0, 1.0]),
    ]);
    let ctx = TestCtx { now };
    let sources: &[&dyn ResurfacingSource] = &[&src];

    run_scorer_pass(
        "p",
        "w",
        &store,
        sources,
        &ctx,
        &centrality,
        &ResurfacingScoringConfig::default(),
    )
    .await
    .unwrap();

    // Baseline: b and c scored to the same salience.
    let b_before = fetch(&store, "b").await.salience_score;
    let c_before = fetch(&store, "c").await.salience_score;
    assert_close(b_before, 0.40, 1e-4, "b base salience");
    assert_close(b_before, c_before, 1e-6, "b and c start tied");

    // Dismiss a (dismiss cooldown 100, ack cooldown 1000).
    let a_id = candidate_id(SourceKind::Memory, "a");
    store
        .record_action("p", "w", &a_id, FeedbackAction::Dismiss, now, 100, 1_000)
        .await
        .unwrap();

    let b_after = fetch(&store, "b").await.salience_score;
    let c_after = fetch(&store, "c").await.salience_score;

    // b (near neighbor) is cut by the suppress factor (×0.5); c (orthogonal) is
    // byte-for-byte unchanged.
    assert_close(b_after, b_before * 0.5, 1e-5, "near neighbor halved");
    assert_eq!(
        c_after, c_before,
        "orthogonal candidate must be completely untouched",
    );
    assert!(
        b_after < c_after,
        "the suppressed near neighbor must now sink below the far candidate",
    );

    // And it no longer out-competes c: with cap=1 the curator picks c, not b.
    // (a is Dismissed ⇒ already ineligible.)
    let surfaced = run_curation_pass_with_attention("p", "w", &store, 1, now, None, None)
        .await
        .unwrap();
    let refs: Vec<&str> = surfaced.iter().map(|c| c.source_ref.as_str()).collect();
    assert_eq!(refs, vec!["c"], "c out-competes the suppressed b");
}

// --- 3. Dismiss teaches across passes (cross-session penalty) ---------------

/// A dismissal must persist a durable signal that penalizes RESEMBLING future
/// candidates scored in a LATER pass (scorer `dismissal_penalty`, cosine ≥ 0.80
/// ⇒ salience ×0.4), even after the original candidate is gone from working
/// memory. Pass 1 scores + dismisses `seed` (embedding `[1,0]`). Pass 2 ingests
/// two fresh items with identical intrinsic scores — `nearby` (embeds near the
/// dismissed `[1,0]`) and `faraway` (orthogonal control). The near item's stored
/// salience is penalized to ~×0.4 of the untouched control's.
#[tokio::test]
async fn e2e_dismiss_penalizes_resembling_item_next_pass() {
    let store = ResurfacingStore::open_in_temp();

    let centrality = MapCentrality::new(&[
        ("seed", 0.5, &[1.0, 0.0]),
        // Cosine 0.9 to the seed: inside the dismissal PENALTY band and outside
        // series suppression. [0.95, 0.05] would be 0.9986 — a repeat, not a
        // resemblance — so the candidate would never be created and this test
        // would be asserting a different behavior than its name claims.
        ("near", 0.5, &[0.9, 0.436]),
        ("far", 0.5, &[0.0, 1.0]),
    ]);

    // Pass 1 (an earlier "session"): score seed, then dismiss it so its embedding
    // is captured as a durable dismissed-signal.
    let seed_src = MockSource::new("memory", vec![corpus_item("seed", day(50), "seed")]);
    let seed_sources: &[&dyn ResurfacingSource] = &[&seed_src];
    run_scorer_pass(
        "p",
        "w",
        &store,
        seed_sources,
        &TestCtx { now: day(60) },
        &centrality,
        &ResurfacingScoringConfig::default(),
    )
    .await
    .unwrap();
    let seed_id = candidate_id(SourceKind::Memory, "seed");
    store
        .record_action(
            "p",
            "w",
            &seed_id,
            FeedbackAction::Dismiss,
            day(60),
            100,
            1_000,
        )
        .await
        .unwrap();

    // Pass 2 (a fresh "session"): two brand-new items with identical intrinsic
    // scores, differing only in whether they resemble the past dismissal.
    let fresh_src = MockSource::new(
        "memory",
        vec![
            corpus_item("nearby", day(100), "near"),
            corpus_item("faraway", day(100), "far"),
        ],
    );
    let fresh_sources: &[&dyn ResurfacingSource] = &[&fresh_src];
    run_scorer_pass(
        "p",
        "w",
        &store,
        fresh_sources,
        &TestCtx { now: day(100) },
        &centrality,
        &ResurfacingScoringConfig::default(),
    )
    .await
    .unwrap();

    let nearby = fetch(&store, "nearby").await.salience_score;
    let faraway = fetch(&store, "faraway").await.salience_score;

    // The control is UNPENALIZED by the embedding dismissal penalty (it's
    // orthogonal to the dismissed vector). It IS, however, subject to the P4c
    // per-lane utility multiplier: the earlier dismiss recorded one negative on
    // the memory lane, so every memory item scored in this pass — control
    // included — is down-weighted by `utility_multiplier(0, 1)`. Both items share
    // that same lane factor, so the ×0.4 dismissal-penalty relationship BETWEEN
    // them is unchanged.
    let lane = utility_multiplier(0, 1, &ResurfacingScoringConfig::default());
    assert_close(
        faraway,
        0.40 * lane,
        1e-4,
        "control (far): only the lane-utility factor",
    );
    assert_close(
        nearby,
        faraway * 0.4,
        1e-5,
        "resembling item penalized ×0.4",
    );
    assert!(
        nearby < faraway,
        "a cross-session dismissal must sink the resembling fresh candidate",
    );
}

// --- 4. Retention prunes terminal + orphans, keeps live ---------------------

/// The retention sweep must age out terminal rows and their side-table rows
/// while leaving live candidates (and their embeddings) intact. A `stale`
/// dismissed candidate is built in an OLD session (with an embedding, phrasing,
/// and a dismissed-signal); a `fresh` live candidate is built in a NEW session.
///
/// Both passes run with `decay_halflife_days = 0.0` on purpose: the decay sweep
/// rewrites every scanned row's `last_scored_at` to `now`, which would un-age the
/// old terminal row and defeat the age-prune. Disabling decay (a documented
/// no-op for a non-positive half-life) keeps `stale`'s timestamps genuinely old.
#[tokio::test]
async fn e2e_retention_prunes_terminal_and_orphans_keeps_live() {
    let store = ResurfacingStore::open_in_temp();

    let centrality =
        MapCentrality::new(&[("stale", 0.5, &[0.0, 1.0]), ("fresh", 0.5, &[1.0, 0.0])]);

    // Both passes disable time-decay (a documented no-op for a non-positive
    // half-life) so the old terminal row's timestamps stay genuinely old — see
    // the test doc. Only `decay_halflife_days` differs from the default config.
    let no_decay = ResurfacingScoringConfig {
        decay_halflife_days: 0.0,
        ..Default::default()
    };

    // Old session: score `stale`, dismiss it (captures embedding + signal), add
    // curator phrasing — a fully terminal, aged, side-table-bearing row.
    let old_now = day(10);
    let stale_src = MockSource::new("memory", vec![corpus_item("stale", day(5), "stale")]);
    let stale_sources: &[&dyn ResurfacingSource] = &[&stale_src];
    run_scorer_pass(
        "p",
        "w",
        &store,
        stale_sources,
        &TestCtx { now: old_now },
        &centrality,
        &no_decay,
    )
    .await
    .unwrap();
    let stale_id = candidate_id(SourceKind::Memory, "stale");
    store
        .record_action(
            "p",
            "w",
            &stale_id,
            FeedbackAction::Dismiss,
            old_now,
            100,
            1_000,
        )
        .await
        .unwrap();
    store
        .upsert_phrasing(
            "p",
            "w",
            &stale_id,
            "circle back",
            "was relevant",
            None,
            old_now,
        )
        .await
        .unwrap();

    // New session: a fresh live candidate with an embedding (decay off, so the
    // old row's timestamps are NOT bumped).
    let fresh_now = day(100);
    let fresh_src = MockSource::new("memory", vec![corpus_item("fresh", day(100), "fresh")]);
    let fresh_sources: &[&dyn ResurfacingSource] = &[&fresh_src];
    run_scorer_pass(
        "p",
        "w",
        &store,
        fresh_sources,
        &TestCtx { now: fresh_now },
        &centrality,
        &no_decay,
    )
    .await
    .unwrap();
    let fresh_id = candidate_id(SourceKind::Memory, "fresh");

    // Sweep with a 30-day horizon: cutoff = day(70); stale (activity at day(10))
    // is terminal + aged out, fresh (live) is kept regardless of age.
    let pruned = store
        .retention_sweep("p", "w", fresh_now, 30, 1_000)
        .await
        .unwrap();
    assert!(
        pruned > 0,
        "the sweep must delete the aged terminal row + orphans"
    );

    // The old terminal candidate and BOTH its side-table rows are gone.
    assert!(
        store
            .get_candidate("p", "w", &stale_id)
            .await
            .unwrap()
            .is_none(),
        "aged dismissed candidate is pruned",
    );
    assert!(
        store
            .get_embedding("p", "w", &stale_id)
            .await
            .unwrap()
            .is_none(),
        "its orphaned embedding is cleaned",
    );
    assert!(
        store
            .get_phrasing("p", "w", &stale_id)
            .await
            .unwrap()
            .is_none(),
        "its orphaned phrasing is cleaned",
    );

    // The live candidate and its embedding survive.
    assert!(
        store
            .get_candidate("p", "w", &fresh_id)
            .await
            .unwrap()
            .is_some(),
        "the live candidate is kept",
    );
    assert!(
        store
            .get_embedding("p", "w", &fresh_id)
            .await
            .unwrap()
            .is_some(),
        "the live candidate's embedding is kept",
    );
}

// --- 5. Empty corpus is clean -----------------------------------------------

/// A pass over a source that yields nothing must not panic and must surface
/// nothing — the degenerate case the worker hits on an idle scope.
#[tokio::test]
async fn e2e_empty_corpus_surfaces_nothing() {
    let store = ResurfacingStore::open_in_temp();
    let now = day(100);

    let src = MockSource::new("memory", Vec::new());
    let centrality = MapCentrality::new(&[]);
    let ctx = TestCtx { now };
    let sources: &[&dyn ResurfacingSource] = &[&src];

    let scored = run_scorer_pass(
        "p",
        "w",
        &store,
        sources,
        &ctx,
        &centrality,
        &ResurfacingScoringConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(scored, 0, "nothing to score");

    let surfaced = run_curation_pass_with_attention("p", "w", &store, 3, now, None, None)
        .await
        .unwrap();
    assert!(surfaced.is_empty(), "nothing to surface");
    assert!(
        store.list_surfaced("p", "w", 10).await.unwrap().is_empty(),
        "no surfaced rows exist",
    );
}
