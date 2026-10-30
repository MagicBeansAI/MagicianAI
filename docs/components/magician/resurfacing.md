# Proactive Resurfacing Engine

Background workers sweep the assistant's accumulated corpus and **proactively
surface a few salient items** in the Today page's "Worth a look" band. Related
memories can inform a candidate's explanation, a For you card, or an owner
clarification in HITL; see [Memory connections](memory-connections.md).

- Design / plan: `docs/archive/plans/2026-07-07-proactive-resurfacing-design.md`,
  `docs/archive/plans/2026-07-07-proactive-resurfacing-implementation.md`
- Engine core: `magician/src/magician_v2/attention/resurfacing/`
  (`magician_v2::attention::resurfacing`, behind `magician_v2::resurfacing_seam`)
- Comms-coupled wiring (worker, curator, actions, interaction registry, comms
  corpus source, `regression.rs`): `magician-comms/src/channel_assist/resurfacing/`
- API: `magician-api/src/resurfacing_api.rs`
- UI: `ui/unified-ui/src/lib/today/ResurfacingBand.svelte` plus detail/action
  subcomponents and the defensive `resurfacingQueries.ts` client

## Code split

The corpus-generic engine (types, the SQLite store `resurfacing.db`, scorer,
centrality, scoring, the memory-effect pipeline, memory/task-episode sources) is
lib-side. Anything coupled to `ChannelAssistStore` stays in `magician-comms`:
worker spawn, curator, action service, interaction registry and the comms source
adapter (implements the lib `ResurfacingSource` trait). Attention learning lives
in `magician_v2::attention::learning`, except the three mail-fed workers
(historical bootstrap, semantic extraction backfill, rank recompute).

## Principle: strictly generic, no hardcoded categories

Salience is **category-free**: there are no category rules.

## Two-speed architecture

The LLM never scans the corpus, only a pre-ranked shortlist.

- **Scorer (default hourly, no LLM):** `run_scorer_pass` scores items changed since
  the per-corpus watermark.
- **Curator (default daily):** surfaces at most the configured cap from a top-ranked
  non-cooldown shortlist. Deterministic path: top-K with a signal-derived "why
  now". LLM path: reviews a wider shortlist and temporarily cools
  reviewed-but-rejected candidates so the queue rotates. Both paths hold
  router-dropped candidates that belong elsewhere out of the window, so the
  shortlist reaches reviewable candidates below them.

## Components

- **`types`**: `SourceKind`, `CandidateState`, `CorpusItem`, `SalienceSignals`,
  `Candidate`, safe `ResurfacingContentDetails`, `FeedbackAction`, `candidate_id`
  (blake3 of `{kind}:{source_ref}`); items may carry a source-native
  `content_revision`.
- **`store`** (`ResurfacingStore`): one SQLite file (WAL), rows scoped by
  `principal`/`workspace`: candidates, watermarks, feedback, revision-bound
  phrasing/recommendations, embeddings, dismissed/affinity signals, kind
  engagement, runs, idempotent action claims/events. `list_top_candidates` =
  state `candidate` ∧ `cooldown_until<=now`, by score. Upserts preserve lifecycle
  columns and reject out-of-order numeric revisions.
  `idx_resurfacing_candidates_surfaced_keyset` matches the surfaced lane's order
  (`state, COALESCE(last_surfaced_at,0) DESC, salience_score DESC, candidate_id ASC`)
  so a page is a seek; it is an `IF NOT EXISTS` bootstrap line, not a migration.
- **`scoring`** (`score_item`, pure): six normalized signals, `const` weights
  summing to 1: recency, frequency, centrality, co-occurrence, temporal_anchor
  (structured temporal facts first, display-text dates as legacy fallback; the
  closest anchor is stored as epoch ms, date-only anchors are UTC date markers,
  not reminder times), dormancy. Observed web sources add `source_affinity`
  (subscription interest + optional intent match; zero for native sources) so a
  subscribed web item is not scored as recency-only.
- **Centrality**: `EmbeddingCentrality` scores mean top-k cosine against a capped
  per-scope reference set, computed async and fed to a sync `ScoreCtx`. It
  **fails to 0** (`NoCentrality`) when the embedder is unavailable, slow or
  failing, so it never blocks the engine.
  - Budgets: `resurfacing.centrality_reference_timeout_secs` (default 90,
    includes cold model load), `resurfacing.centrality_query_timeout_secs`
    (default 15). `centrality_reference_batch_size` is parsed but ignored.
  - Keeps the 512 most recent references; a vector is reused only while content
    digest and embedding contract (provider/model/dimension/context/input)
    match. A pass embeds at most 128 new misses and persists each at once, so
    cold scopes converge across passes. All resurfacing work uses the optional
    background admission lane and yields to foreground memory/procedure
    retrieval between provider calls.
- **`sources`**: `ResurfacingSource` (`corpus_kind` + `list_changed_since`).
  - `MemorySource`: tiers in `resurfacing.memory_tiers` via `AgentMemoryResolver`
    → `load_user_knowledge`, per-entry `updated_at`.
  - `TaskEpisodeSource`: terminal tasks via `V3ReadApi::list_tasks`.
  - `CommsSource`: mail/WhatsApp **distilled summaries only**
    (`list_distilled_for_bridge`, `distill_state='done'`, non-suppressed; never
    raw bodies). Watermark is the monotonic distill revision; scoring time stays
    the message time, so a corrected old message refreshes the same candidate.
  - No calendar adapter: calendar arrives via memory (`user.calendar_evidence`,
    meeting takeaways in `user.research_findings`); a separate source would
    double-count, and the live events path has nothing on disk to sweep.
  - Observed web: the content-source scheduler drains RSS/Atom envelopes into
    `SourceKind::Web` candidates (canonical URL as source ref, transient writes
    retry, invalid envelopes quarantined) and wakes the curator.
- **`scorer`** (`run_scorer_pass`): watermark → `list_changed_since` →
  `score_item` → upsert (keeps state/counts/cooldown, so a dismissed row is never
  resurrected) → **advance the watermark only after upserts succeed** →
  `decay_all`.
- **`curator`**: `run_curation_pass_with_attention` (deterministic) and
  `run_curation_pass_llm_with_recommendations` (`resurfacing_curate`,
  explicit-binding only; shipped config binds local Gemma; no binding or missing
  managed prompt assets ⇒ deterministic).
  - Reviews at most 50 candidate lines of ≤480 chars (safe facts, signal
    profile, server-computed allowed action IDs); selects and phrases up to the
    surface cap and may name one payload-free recommendation. Selection and
    recommendation parse independently: unknown fields, unsupported IDs, low
    confidence or a stale revision drop only the recommendation.
  - The prefilter rejects only structural failures (revisionless comm
    candidates, invalid scores, nothing reviewable); no word lists or
    length/recency gates. The deterministic fallback rejects recency-only
    candidates.
  - Comms: reloads the exact message, checks the distill revision, reapplies
    required-action mapping and active annotations; actionable evidence becomes
    one Follow-up before the candidate is withheld, and each outcome is recorded
    as an attention-funnel reason.
  - Routing holds: a candidate the router drops because it belongs elsewhere
    (`owner_dismissed`, `action_already_handled`, `sensitive_or_suppressed`,
    `stale_content_revision`: 7 days; `active_follow_up_exists` or routed to
    another lane: 24 hours) leaves the review window through one batched
    `hold_routing_dropped_candidates` call per pass. The hold stays a
    `candidate`-state cooldown (`routing_held` in the queue stats, counted
    apart from `cooling`, and shown as "held" on the Resurfacing page; `/today`, state
    counts and the Follow-up lane are unchanged; memory-connection discovery
    skips it like any cooling row) and records the revision it
    judged (`routing_hold_revision` / `routing_hold_until`). A new content
    revision from the scorer ends it at once; an owner action or any other
    cooldown written after it wins. Expiry or a new revision only re-routes the
    row, so an owner-dismissed Follow-up is dropped and held again, never
    surfaced. A failed source read or materialization never holds.
  - Funnel events: each pass (deterministic, LLM with fallbacks, or active
    repair) collects its events and writes them with one
    `AttentionFunnelStore::append_events`. Event ids hash scope, candidate,
    content revision (title + digest for revisionless sources), state, stage
    and outcome — not `last_scored_at`, which the hourly decay rewrites — so an
    unchanged repeated decision is ignored as a duplicate id.
- **`source_refs` + `interaction`**: strict encoded comm refs and the read-only
  `ResurfacingInteractionAdapter`, which reports `available`, `newer_available`,
  `stale`, `offline`, `deleted`, `suppressed`, `unsupported` or `unavailable`
  and derives action descriptors from server/provider capabilities.
- **`actions`**: idempotent contextual-action service (strict typed inputs,
  re-validated revision and capability, scoped SQLite claim, then V3
  task/reminder/share or LearningStore writes). Ask Presto returns a
  same-thread candidate context reference. Deeper summary is user-triggered,
  reads the bounded exact source, and runs only through an explicitly mapped,
  pinned Ollama profile with its managed prompts (else "unavailable").
- **`worker`** (`ResurfacingWorker::spawn`): independent scorer and curator
  intervals under one shutdown token, isolated per scope; a scorer pass wakes
  the curator. Sweeps cover actual workspace scopes plus the default
  (deterministically capped); the curator reads one fixed window per scope.

### Memory context and trust

`memory_context` attaches scoped user-knowledge entries in **shadow**. Trust
(`stated` / `inferred` / `untrusted`) and kind (`normative` / `procedural` /
`factual` / `episodic`) are permissions.

- **Trust comes from the ingestion path, never from the producer** (a model
  could otherwise stamp `explicit_user_statement` on room speech). Episodes carry
  a server-minted `origin_surface`; `MemoryTrust::ceiling_for_surface` caps it
  and `clamped_by_origin` takes the weaker of declared and derived, rewriting
  `source_type` and keeping `declared_source_type`. Transforms may lower, never
  raise, a claim; tier-sourced rules inherit the ceiling.
- **Known gap:** owner surfaces are uncapped, so untrusted text in an owner
  session (pasted email, documents, tool results) can still declare `stated`.
- Labels are normalised (case, padding) before reading: an unknown label
  falling through to `inferred` would upgrade untrusted input.
- Unknown origin fails closed at `v3_memory_episode/v2` (`untrusted`); older
  records do not clamp.
- Only stated `preferences` and `research_findings` with owner-edited
  topics/entities enter the shadow set (capped). Inferred text never becomes
  "you said"; empty scopes match nothing; owner topics AND-match; stage-1
  narrowing is scope overlap, not embedding similarity; untrusted provenance
  never conditions salience.
- `evaluate_shadow` records what *would* apply on
  `(content_revision, memory_revision)` without rewriting scores; stated
  normative may propose hide, inferred only down-rank; `hard_eligible` stays
  true; influence decays (60/90-day normative/factual half-life). A stated rule
  contradicting recent engagement is recorded as a conflict and listed on
  `/memory`. Serendipity never lifts an item below the intrinsic salience floor.
  Enforced hide and learned `P(reject)` are off.
- Memory-cited `why_now` applies to the legacy Today read and the canonical
  Worth union. Knowledge writes take the exclusive `knowledge.json` lock; shadow
  attach caches by knowledge-file mtime.

**Memory-effect mode.** Compile default **Shadow** (`MEMORY_EFFECT_MODE`);
serving uses `effective_memory_effect_mode()`. After each scorer pass
`review_memory_effects` counts judgements and recommends Collect / Investigate /
Canary / Enforced. Advice to advance emits a `UserRequest`
(`request_type: memory_effect_review`, source `resurfacing`) for Accept / Stay;
timeout stays. Also on `GET/POST /memory/effect-review` and a `/memory` banner.
Accept writes `{base_root}/memory_effect_mode.json` (no rebuild). Shadow never
jumps to Enforced. Ranking and routing learn independently through
[attention learning](attention-routing-funnel.md).

### Which memory tiers reach the owner

`resurfacing.memory_tiers` is an allowlist (`MemorySource` maps nothing else),
so a new tier cannot reach the owner silently; an empty list surfaces nothing
(fail closed). The default is the owner half of `USER_MEMORY_TIERS` in
`chat::service`, the table the tier-name check reads, so classifying a tier
decides whether it resurfaces (`workflows` and `organization` are the agent's
and excluded; `preferences` kept by owner decision). Every consumer derives
from two tier properties:

| Property | Consumers |
| --- | --- |
| `audience`: owner's material or agent's own | `owner_facing_user_memory_tiers` → `resurfacing.memory_tiers` |
| `curation`: learning-managed or append-only observation lane | `is_curated_user_memory_tier` → memory bridge write allowlist, consolidation questions, internal-data surface, feed |
| `curation` + not root-addressing | `is_curated_named_user_memory_tier` → consolidator promotion fan-out (tier name is a write path) |
| membership | `normalized_user_memory_tier_name` |

`knowledge` is curated and owner-facing but addresses the store root, so it is
not a fan-out path. Filtering governs ingestion only; active repair's
`retract_ineligible_memory_candidates` moves already-surfaced cards whose tier
is no longer allowed (read from `source_ref` `<tier>#<key>`; malformed refs
match nothing and are retracted) to `dismissed`, bounded by
`active_repair_batch_size` and idempotent, without touching `dismiss_count` or
dismissed signals (policy, not the owner, dismissed them).

## Feedback (dismiss-teaches + cooldown)

`record_action` writes feedback and state in one transaction. Open (Mark useful)
/ Acknowledge → `acted` + long cooldown; Dismiss → `dismissed` + cooldown;
terminal states stick (the scorer preserves them). **Open is positive**
(affinity + lane engagement); **Acknowledge is neutral** (stops resurfacing, no
signal).

| Signal | Rule |
|---|---|
| Neighbour penalty | A penalizing Dismiss multiplies still-candidate rows in the same embedding contract at ≥0.82 cosine by `×0.5`, in the same transaction. `DismissReason` `not_relevant`/`spam`/none penalize; `already_handled`/`duplicate`/`delegated` clear the card with no penalty or negative engagement. The reason is not stored raw. |
| Durable dismissal | Capped `resurfacing_dismissed_signals`; a new candidate ≥0.80 cosine is `×0.4`. |
| Series dismissal | A new candidate ≥ `DISMISSED_SERIES_THRESHOLD` (0.97) to any dismissed signal is **never created**; counted and logged per pass, watermark still advances. |
| Affinity | Open/Ack embeddings in `resurfacing_affinity_signals`; ≥0.80 cosine is `×1.5`. |
| Lane utility | Per-`source_kind` counters (survive pruning) give a Laplace-smoothed multiplier in `[0.5, 1.5]`, exactly 1.0 at cold start; the **boost half is scaled by the item's own centrality**, the penalty half is not (a flat boost would self-reinforce a popular lane). |

- Final chain: `score × dismissal_penalty × affinity_boost × utility`. Only
  signals in the pass's active embedding contract count; legacy NULL-contract
  rows and other models' vectors are neutral.
- **Retention** (daily, on curator tick): age-prunes terminal candidates and old
  signals, cap-prunes by liveness, orphan-cleans phrasing and stale embeddings.

**Contextual actions** use `resurfacing_action_claims`/`_events`. A task,
one-time reminder, share draft or reviewable memory completes its claim, with
positive engagement in one transaction, only after the downstream object is
durable; failures leave the card surfaced and the key retryable; completed
requests replay; stale `started` claims are reclaimable after 5 minutes
(`ACTION_CLAIM_STALE_SECS`),
and writes are attempt-bound so an expired worker cannot overwrite its
successor. Revision, source state, newer evidence and capability are
re-resolved after the claim and before any side effect. Task descriptions mark
owner input trusted and the source brief untrusted data.

**Recommendations** never carry arguments. One is returned only when
`recommendations_enabled` is on and the stored revision, confidence floor and
capability still match. First presentation is atomically deduplicated; an
action counts as acceptance only if that exact recommendation was shown before
selection.

## Canonical attention integration

Worth-a-look `content_revision` is opaque: compatibility is exact string equality
plus the schema/extractor/prompt/model/profile contract (no numeric ordering); a
result extracted against a revision that changed mid-lease never becomes
compatible. Coverage scans keep missing, stale, pending, retrying, dead and
incompatible candidates in the denominator; the coverage worker claims waves
only with a free execution slot, renews slow leases while it owns the revision,
prioritizes expired leases, and reports active/expired partitions. The admin
coverage control only schedules idempotent work, never mutating lifecycle or
ranking. Semantic backfill is non-serving; actionability serving needs a
qualified immutable snapshot plus an explicit rollout.

**Projection.** The canonical projector uses the API's revision-bound metadata
and never resolves content. A Worth-origin item in Follow-ups gets a typed
payload with fixed descriptors (`open_source` when available, `useful`,
`acknowledge`, `dismiss`); a Follow-up-origin item in Worth keeps its Follow-up
payload. Identity is origin-qualified (candidate IDs are lane-local). A stale or
short Worth load fails the projection closed while the legacy Worth list stays
an atomic baseline. The Worth load is a **bounded page** (store clamps to 1,000
rows, reporting the true total; validation is `rows == min(total, page_limit)`;
above the bound the union ranks the top of the lane); the legacy duplicate check
runs only on the same universe (`worth_a_look_source_total`).

**Learned pagination.**
`GET /api/magician/v2/channel-assist/attention-learning/canonical-deliveries/worth_a_look`:
the cursorless request freezes one root (decision, digest, posterior, policy,
seed, revisions); cursors resolve positions from it, so replay cannot duplicate
or skip. Stale digest/revision, expired cursor or scope/lane/root mismatch → HTTP
409 `refresh_required`. Items keep the root's policy propensity (page delivery
has conditional propensity 1); exposure tokens bind root, page, position,
revision, scope, digest and propensities, and only a matching delivered item
crossing the root-frozen dwell rule creates an idempotent impression. Degraded
delivery freezes baseline order and never invents an exposure from an API
return.

**Feedback → rank recompute.** Worth feedback (legacy endpoint included) uses
the Follow-ups async rank-recompute job (pending job, null after-rank, status
URL); the after-rank publishes only if revision, digest, generation, posterior
and snapshot CAS still match, else finishes stale. List and cursor reads never
wait. Recompute ships disabled/paused.

**Cross-lane identity.** Before ranking, a `comm` ref must parse to the exact
`(provider, account_alias, thread_id)`; only equality with an active Follow-up
tuple creates a durable alias (no title/sender/embedding/LLM heuristics). The
Follow-up owns the card while Worth keeps its evidence; the Worth row returns
when the Follow-up is inactive. A malformed comm ref fails canonical
publication closed, and the legacy guard withholds unresolved comm rows
(non-comm sources stay eligible, with a content-free diagnostic). Alias
summaries cap at 100 opaque records; the page total excludes aliases before
cursor construction. The Today read loads the Follow-up owner set once, counts
lane size and hidden rows in one query, then seeks to the page plus one row for
`has_more`; `reconciliation_digest` covers the owner set plus those rows and is
never null on success. Known limit: the group-members read still drains the
whole lane.

## Config

Env, on by default:

| Var | Default | Meaning |
|-----|---------|---------|
| `RESURFACING_ENABLED` | `1` | Kill switch (`0`/`false` disables). |
| `RESURFACING_SCORER_INTERVAL_SECS` | 3600 | Scorer cadence. |
| `RESURFACING_CURATOR_INTERVAL_SECS` | 86400 | Curator cadence. |
| `RESURFACING_SURFACE_CAP` | `resurfacing.surface_cap` (code 5; seed 15) | Max cards surfaced. |
| `RESURFACING_DECAY_HALFLIFE_DAYS` | 14 | Salience decay half-life. |
| `RESURFACING_RETENTION_DAYS` | 90 | Prune age for terminal candidates and dismissed signals. |
| `RESURFACING_CANDIDATE_CAP` | 2000 | Max candidates per scope (cap-pruned by liveness). |

Scoring tunables, no rebuild (`ResurfacingScoringConfig::from_env`, defaults = compiled
consts): `RESURFACING_W_RECENCY|FREQUENCY|CENTRALITY|COOCCURRENCE|TEMPORAL|DORMANCY`,
`RESURFACING_RECENCY|TEMPORAL|DECAY_HALFLIFE_DAYS`,
`RESURFACING_DISMISS_PENALTY_THRESHOLD|FACTOR`,
`RESURFACING_AFFINITY_THRESHOLD|BOOST_FACTOR`,
`RESURFACING_UTILITY_MIN|MAX_MULTIPLIER`, `RESURFACING_UTILITY_SMOOTHING`.

YAML `resurfacing` block: `memory_tiers`; the centrality timeouts;
`recommendations_enabled` (independent rollback, deletes nothing),
`recommendation_min_confidence`, `contextual_actions_enabled` (side-effecting
IDs may enter the curator allow-list); `rich_briefs_enabled`,
`source_details_enabled` (enforced by the interaction registry);
`active_repair_enabled` + `active_repair_batch_size` (sequential,
revision-idempotent routing check over surfaced comm cards before curation;
required-action cards go to the Follow-up materializer, withheld without owner
feedback).

## API

`…` = `/api/magician/v2/channel-assist`. Scope comes only from the
workspace-bound bearer; no GET fetches a source body except `original`.

- `GET /api/magician/v2/channel-assist/resurfacing/today?limit=20` → `{ cards:
  [{ candidate_id, line, why_now, source_kind, source_ref, brief,
  content_revision, recommended_action, actions, temporal_anchor_at }],
  total, limit, offset, has_more, next_cursor, next_cursor_token }`. The band uses
  object `next_cursor` (`cursor_surfaced_at`, `cursor_score`,
  `cursor_candidate_id`); canonical callers pass `next_cursor_token` as `cursor`.
  Phrasing and recommendations are revision-guarded batch reads
  (`get_phrasing_batch` / `get_recommendation_batch`); absence falls back to
  title + signal "why now".
- `GET …/attention-learning/canonical-deliveries/worth_a_look` → schema-v1
  `{ status, fallback_reason, root_decision, page, impression_policy, items,
  health }`; optional `page_size`, then only the opaque `cursor`; typed 409.
- `GET …/resurfacing/{candidate_id}/detail` → persisted safe brief, current
  source metadata, revision, status, `has_newer`, route, action capabilities.
- `GET …/resurfacing/{candidate_id}/original` → explicit source resolution;
  comms fetch the exact message and coalesced evidence live (20k chars/message,
  40k/response, at most eight messages); task/memory return current scoped
  detail. Suppressed sources return nothing.
- `POST …/resurfacing/{candidate_id}/action` `{ "action": "open" |
  "acknowledge" | "dismiss" }`.
- `POST …/resurfacing/{candidate_id}/recommendation-event`
  `{ kind, content_revision, event: "presented" | "selected" | "completed" }`;
  `presented` deduplicates atomically; `selected`/`completed` accept only
  Details, Open source or Original after a presentation. Contextual actions
  record these inside their claim flow.
- `POST …/resurfacing/{candidate_id}/actions`
  `{ kind, idempotency_key, content_revision, input }`. Kinds: `create_task`,
  `create_reminder`, `save_to_memory`, `ask_presto`, `share`, `summarize_deeper`
  (verified local binding only). Duplicate completed requests replay; key
  conflict or stale content → 409; unavailable dependency → 503 without
  changing feedback. Error `code`:
  `invalid`, `stale_revision`, `in_progress`, `idempotency_conflict`,
  `not_actionable`, `unavailable`, `not_found`, `internal`. `create_reminder` is
  a native Apple Reminder, never a Magician task: iOS creates it via EventKit
  and submits `delivery=client_apple_eventkit` + `external_id`; web submits
  `delivery=host_apple_reminders` and Magician delegates to the loopback
  desktop host. Receipt: `reminder_id`, `provider`, due time, timezone.
- `POST …/resurfacing/repair?dry_run=true&limit=10` previews a bounded active
  routing pass; without `dry_run` it writes per-revision receipts,
  materializes Follow-ups and withholds rerouted cards. No LLM.
- `GET …/resurfacing/stats` → `{ lanes: [{ source_kind, positive, negative,
  engagement_rate, utility_multiplier }] }` (multiplier exact for penalty,
  upper bound for boost).
- `GET …/resurfacing/observability` → see below.

## Unified UI interaction contract

Today and Town Square mount the same `ResurfacingBand`, so facts, capability
gating, pagination and actions cannot drift. A row shows the safe summary plus
at most three structured facts, with source omissions and update state visible;
summary-only payloads fall back to a single Details action. Details is a lazy
safe-detail read; Original is a separate explicit read rendered as bounded plain
text (no source HTML). One validated recommendation may be the primary button;
other advertised capabilities and Mark useful/Acknowledge/Dismiss live in a
keyboard menu. The client never invents a capability.

Task, reminder, share and memory dialogs submit a content revision and one UUID
idempotency key kept across retries. Failures keep the row; only
`stale_revision` closes the dialog, reloads and asks for re-confirmation. A web
reminder is opened by the macOS host and returns no browser URL. Ask Presto
opens the server-returned chat route with only `resurfacing_candidate=<id>`;
`ChatPanel` resolves it via the safe detail endpoint and stages a bounded text
attachment (never `original`); the composer is prefilled, not sent.

## Observability

Every scorer / curator / retention / `routing_repair` pass is recorded
in `resurfacing_runs` (kind, `started_at`, `duration_ms`, `produced`,
`success`, last `error`; capped at 500 per scope; a failed write logs a warning
and the pass continues). `GET …/observability` returns `pipeline` (per-kind
aggregates) and `recent_runs` (newest 50); `funnel` (by
`state × source_kind`, plus `pending`/`eligible` (cooldown elapsed, actionable
now), `candidate_pool`, `cooling` (ordinary cooldowns such as model
deferrals), `routing_held` (live curator routing holds, never inside
`cooling`), `surfaced`) and `queue` (the same snapshot
standalone); `watermarks`, `sizes` (every table, including repair receipts);
`engagement` (as `/stats`); `recommendations` (shown/selected/completed totals
and rates by kind); `briefs` (comm coverage, detail status); `actions`
(started/completed/failed, per-kind events, bounded error classes). Labels,
rationales, content and user values are never dimensions.
