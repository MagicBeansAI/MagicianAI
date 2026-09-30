# Proactive Resurfacing Engine

Background workers that sweep the assistant's accumulated corpus and
**proactively surface a few genuinely salient items** on the Today page — a
glanceable "Worth a look" band — instead of waiting to be asked. Related
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

The corpus-generic engine — types, the SQLite store (`resurfacing.db`), scorer,
centrality, scoring, the memory-effect pipeline, and the memory/task-episode
sources — lives lib-side. What stays in `magician-comms`, by design, is anything
hard-coupled to `ChannelAssistStore`: worker spawn, curator (channel
annotation/required-action writes), action service, interaction registry, and
the comms source adapter (implements the lib `ResurfacingSource` trait).
Attention learning lives in `magician_v2::attention::learning`, except the three
mail-fed workers (historical bootstrap, semantic extraction backfill, rank
recompute), which stay in `magician-comms`.

## Principle: strictly generic, no hardcoded categories

Salience is **category-free**. Birthdays / anniversaries / "something we did
earlier" are illustrative only — there are no category rules.

## Two-speed architecture

The LLM never scans the corpus, only a pre-ranked shortlist.

- **Scorer (~hourly, no LLM):** watermark-driven `run_scorer_pass` pulls only
  new/changed items since the per-corpus cursor, scores them with category-free
  signals, and upserts + decays candidates.
- **Curator (~daily):** takes a top-ranked non-cooldown shortlist and surfaces
  at most the configured cap. Deterministic path is top-K with a signal-derived
  "why now"; the LLM path reviews a wider shortlist and temporarily cools
  reviewed-but-rejected candidates so the queue rotates.

```
corpus (memory · tasks/episodes · distilled comms · observed web sources)
   │  (watermark: only new/changed)
   ▼
Scorer (hourly, no LLM) ── category-free signals ──▶ ResurfacingStore
                                    (scored, decaying candidates + cooldown)
                                          │  top-N
                                          ▼
                              Curator (daily) ── surface top few
                                          │
                                          ▼
                              /today "Worth a look" band ──▶ user action
                                          ▲                    │
                                          └──── feedback ◀──────┘
                              (Open/Ack = +, Dismiss = − + cooldown)
```

## Components

- **`types`** — `SourceKind`, `CandidateState`, `CorpusItem`, `SalienceSignals`,
  `Candidate`, safe `ResurfacingContentDetails`, `FeedbackAction`, and
  `candidate_id` (blake3 of `{kind}:{source_ref}`). Items may carry a
  source-native `content_revision`.
- **`store`** (`ResurfacingStore`) — one SQLite file (WAL), rows scoped by
  `principal`/`workspace`. Tables include candidates, watermarks, feedback,
  revision-bound phrasing/recommendations, embeddings, dismissed/affinity
  signals, kind engagement, runs, and idempotent action claims/events.
  `list_top_candidates` = state `candidate` ∧ `cooldown_until<=now`, by score.
  Upsert conflicts preserve lifecycle columns and reject out-of-order numeric
  content revisions. `idx_resurfacing_candidates_surfaced_keyset` matches the
  surfaced lane's read order (`state, COALESCE(last_surfaced_at,0) DESC,
  salience_score DESC, candidate_id ASC`) so a page is a seek; it is an
  `IF NOT EXISTS` bootstrap line, no migration.
- **`scoring`** (`score_item`, pure) — six normalized signals blended with
  `const` weights summing to 1: recency (exp-decay), frequency (saturating),
  centrality, co-occurrence, temporal_anchor (structured temporal facts first,
  display-text dates only as legacy fallback), dormancy. The closest supported
  anchor is persisted as epoch ms (date-only anchors are UTC date markers, not
  reminder times). Observed web sources add a seventh `source_affinity` signal
  (subscription interest + optional intent match; zero for native sources) so
  the curator does not treat a subscribed web item as recency-only.
- **Centrality** — `EmbeddingCentrality` embeds the item and scores mean top-k
  cosine against a capped per-scope reference set, computed async in the scorer
  and fed to a precomputed sync `ScoreCtx`. It **fails to 0** (`NoCentrality`)
  when the embedder is unavailable, times out or fails: centrality is
  opportunistic and must not block the engine while local Ollama is saturated.
  - Reference build budget `resurfacing.centrality_reference_timeout_secs`
    (default 90, includes cold model load); query budget
    `resurfacing.centrality_query_timeout_secs` (default 15).
    `centrality_reference_batch_size` is parsed but ignored.
  - 512 most recent references retained, persisted with candidate identity,
    content digest and embedding contract (provider/model/dimension/context/
    input). A vector is reused only while digest and contract match. A pass
    embeds at most 128 new misses and persists each immediately, so cold scopes
    converge across passes.
  - All resurfacing work uses the optional background admission lane and yields
    between provider calls to foreground memory/procedure retrieval.
- **`sources`** — `ResurfacingSource` (`corpus_kind` + `list_changed_since`).
  - `MemorySource`: tiers in `resurfacing.memory_tiers` via
    `AgentMemoryResolver` → `load_user_knowledge`, per-entry `updated_at`.
  - `TaskEpisodeSource`: terminal tasks via `V3ReadApi::list_tasks`.
  - `CommsSource`: mail/WhatsApp **distilled summaries only**
    (`list_distilled_for_bridge`, `distill_state='done'`, non-suppressed —
    never raw bodies). Watermark is a monotonic distill revision; the provider
    message time stays the scoring timestamp, so corrected old messages refresh
    the same candidate.
  - No calendar adapter: calendar already arrives via memory
    (`user.calendar_evidence`, meeting takeaways in `user.research_findings`);
    a separate source would double-count, and the live events path has no disk
    persistence to sweep.
  - Observed web ingress: the content-source scheduler drains RSS/Atom
    envelopes into `SourceKind::Web` candidates (canonical URL as source ref,
    transient writes retry, invalid envelopes quarantined) and wakes the curator.
- **`scorer`** (`run_scorer_pass`) — per scope: watermark → `list_changed_since`
  → `score_item` → upsert (preserving state/counts/cooldown so a dismissed row
  is never resurrected) → **advance watermark only after upserts succeed** →
  `decay_all`.
- **`curator`** — `run_curation_pass_with_attention` (deterministic top-K) and
  `run_curation_pass_llm_with_recommendations` (`resurfacing_curate`,
  explicit-binding only; shipped config binds local Gemma, removing the binding
  activates the deterministic path).
  - Prompt reviews at most 50 candidate lines of ≤480 chars, each with safe
    facts, signal profile and server-computed allowed action IDs. It selects +
    phrases up to the surface cap and may name one payload-free recommendation.
    Selection and recommendation parse independently; unknown fields,
    unsupported IDs, low confidence or stale revisions drop only the
    recommendation. Missing managed prompt assets ⇒ deterministic path, no LLM
    call.
  - A deterministic prefilter rejects only structural failures (revisionless
    comm candidates, invalid scores, nothing reviewable). No word lists or
    length/recency semantic gates: the model decides from content and signal
    profile. The deterministic fallback stays conservative and rejects
    recency-only candidates.
  - For comms the curator reloads the exact message, checks distill revision,
    reapplies the required-action mapping and active annotations. Actionable
    evidence becomes one Follow-up before the candidate is withheld; each
    outcome is recorded as an attention-funnel reason.
- **`source_refs` + `interaction`** — strict encoded comm references and the
  read-only `ResurfacingInteractionAdapter`. After a scoped candidate lookup it
  reports `available`, `newer_available`, `stale`, `offline`, `deleted`,
  `suppressed`, `unsupported` or `unavailable`, and derives action descriptors
  from server/provider capabilities.
- **`actions`** — idempotent contextual-action service: strict typed inputs,
  re-validated revision and capability, claim in scoped SQLite, deterministic
  V3 task/reminder/share or LearningStore writes. Ask Presto returns a
  same-thread candidate context reference. Deeper summary is user-triggered,
  reads the bounded exact source, and runs only through an explicitly mapped,
  pinned Ollama profile with its managed prompts (else "unavailable").
- **`worker`** (`ResurfacingWorker::spawn`) — independent scorer and curator
  intervals under one shutdown token, per-scope isolation. A completed scorer
  pass wakes the curator. Sweeps enumerate actual workspace scopes plus the
  default, deterministically capped; the curator reads one fixed candidate
  window per scope.

### Memory context and trust

`memory_context` attaches scoped user-knowledge entries in **shadow**. Trust
(`stated` / `inferred` / `untrusted`) and kind (`normative` / `procedural` /
`factual` / `episodic`) are permissions.

- **Trust is derived from the ingestion path, never declared by a producer.**
  A model writes `source_type`, so on an untrusted surface it could stamp
  `explicit_user_statement` and launder room speech into `stated`. Episodes
  carry a server-minted `origin_surface`; `MemoryTrust::ceiling_for_surface`
  caps it and `clamped_by_origin` takes the weaker of declared and derived. The
  clamp rewrites `source_type` and keeps the original as
  `declared_source_type`. A transform may lower, never raise, its claim.
  Tier-sourced rules inherit the ceiling transitively.
- **Known gap:** owner surfaces are uncapped, so untrusted text passing through
  an owner session (pasted email, documents, tool results) can still declare
  `stated`.
- Labels are normalised (case, padding) before reading, because an unknown
  label falling through to `inferred` would be an upgrade for untrusted input.
- Unknown origin fails closed at `v3_memory_episode/v2` (treated as
  `untrusted`); older records predate the field and do not clamp.
- Only stated `preferences` and `research_findings` with owner-edited
  topics/entities enter the shadow set (capped). Inferred text never becomes
  "you said"; empty scopes match nothing; owner topics AND-match; stage-1
  narrowing is scope overlap, not embedding similarity. Untrusted provenance
  never conditions salience.
- `evaluate_shadow` records what *would* apply, persisted on
  `(content_revision, memory_revision)` without rewriting scores. Stated
  normative scoped memories may propose hide; inferred may only down-rank;
  `hard_eligible` stays true. Influence decays (60-day normative / 90-day
  factual half-life). A stated rule contradicting recent engagement is recorded
  as a conflict and listed on `/memory`. Serendipity never lifts items below an
  intrinsic salience floor. Enforced hide and learned `P(reject)` are off.
- Memory-cited `why_now` applies to the legacy Today read and the canonical
  Worth union. Knowledge writes take the exclusive `knowledge.json` lock.
  Shadow attach caches by knowledge-file mtime.

**Memory-effect mode.** Compile default is **Shadow** (`MEMORY_EFFECT_MODE`);
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
so a new tier cannot reach the owner silently. The default is the owner half of
`USER_MEMORY_TIERS` in `chat::service` — the same table the tier-name check
reads — so classifying a tier decides whether it resurfaces. `workflows` and
`organization` are the agent's and excluded; `preferences` is kept by owner
decision. An empty list surfaces nothing (fail closed).

Each tier carries two properties, and every consumer derives from one:

| Property | Consumers |
| --- | --- |
| `audience` — the owner's material, or the agent's own | `owner_facing_user_memory_tiers` → `resurfacing.memory_tiers` |
| `curation` — managed by the learning path, or an append-only observation lane | `is_curated_user_memory_tier` → the memory bridge's write allowlist, consolidation's clarifying questions, the internal-data surface, the feed |
| `curation` + not root-addressing | `is_curated_named_user_memory_tier` → the consolidator's promotion fan-out, where the tier name is used as a write path |
| membership | the tier-name check itself (`normalized_user_memory_tier_name`) |

`knowledge` is curated and owner-facing but addresses the store root, so it is
not a fan-out path.

Filtering governs ingestion only. Active repair's
`retract_ineligible_memory_candidates` moves already-surfaced memory cards whose
tier is no longer allowed to `dismissed` (bounded by `active_repair_batch_size`,
idempotent). It does not touch `dismiss_count` or dismissed signals — policy,
not the owner, dismissed them. Tier is read from `source_ref` (`<tier>#<key>`);
a malformed ref matches nothing and is retracted.

## Feedback (dismiss-teaches + cooldown)

`record_action` writes the feedback row and state transition in one
transaction. Open (Mark useful) / Acknowledge → `acted` + long cooldown;
Dismiss → `dismissed` + cooldown. Terminal states are preserved by the scorer,
so feedback sticks. **Open is positive** (affinity + lane engagement);
**Acknowledge is neutral** (stops resurfacing, no signal).

- **Neighbour penalty**: a penalizing Dismiss down-weights every still-candidate
  row in the same embedding contract at ≥0.82 cosine (`×0.5`), in the same
  transaction. `DismissReason`: `not_relevant`/`spam`/none penalize;
  `already_handled`/`duplicate`/`delegated` clear the card without penalty or
  negative engagement. The reason drives behaviour and is not stored raw.
- **Durable dismissal**: each penalizing dismissal's embedding goes to a capped
  `resurfacing_dismissed_signals`; a new candidate ≥0.80 cosine is `×0.4`.
- **Series dismissal**: a new candidate ≥ `DISMISSED_SERIES_THRESHOLD` (0.97) to
  any dismissed signal is **never created** (0.80 = similar, 0.97 = the same
  recurring thing). Suppressions are counted and logged per pass, and the
  watermark still advances.
- **Affinity**: Open/Ack persist embeddings to `resurfacing_affinity_signals`;
  a new candidate ≥0.80 cosine is `×1.5`.
- **Lane utility**: per-`source_kind` engagement counters (survive pruning)
  give a Laplace-smoothed multiplier in `[0.5, 1.5]`, exactly 1.0 at cold
  start. The **boost half is scaled by the item's own centrality**, the penalty
  half is not: a flat boost would lift everything in a popular lane and
  self-reinforce, while a lane you keep dismissing should go quieter regardless.
- Final chain: `score × dismissal_penalty × affinity_boost × utility`.
- Signals carry the embedding contract; scoring loads only signals from the
  pass's active contract. Legacy NULL-contract rows and vectors from another
  model are neutral rather than crossing vector spaces.
- **Retention** (daily, on curator tick): age-prunes terminal candidates and old
  signals, cap-prunes by liveness, orphan-cleans phrasing and stale embeddings.
  Scorer promotions stamp vectors from the candidate's logical clock so both
  cross the horizon together.

**Contextual actions** use separate `resurfacing_action_claims`/`_events`. A
task, one-time reminder, share draft or reviewable memory completes its claim
and records positive engagement in one transaction only after the downstream
object is durable. Failures leave the card surfaced and the idempotency key
retryable; completed requests replay. A stale `started` claim is reclaimable
after a bounded lease, and writes are attempt-bound so the expired worker cannot
overwrite its successor. Revision, source state, newer evidence and capability
are re-resolved after the claim and before any side effect. Task descriptions
mark owner input as trusted and the source brief as untrusted data.

**Recommendations** are guidance only and never carry arguments. APIs return
one only when `recommendations_enabled` is on and stored revision, confidence
floor and advertised capability still match. First presentation is atomically
deduplicated; an action counts as acceptance only if that exact recommendation
was shown before selection.

## Canonical attention integration

Worth-a-look `content_revision` is an opaque source-owned identity:
compatibility is exact string equality plus the
schema/extractor/prompt/model/profile contract (no numeric parsing or ordering).
A result extracted against a revision that changed mid-lease never becomes
compatible. Per-scope scans keep missing, stale, pending, retrying, dead and
incompatible candidates in the coverage denominator. The coverage worker claims
waves only with a free execution slot, renews slow leases while it owns the
revision, prioritizes expired leases, and reports active/expired partitions. The
admin coverage control only schedules idempotent work; it never mutates
lifecycle or ranking. Semantic backfill is non-serving; actionability serving
requires a qualified immutable snapshot plus an explicit rollout.

**Projection.** The canonical projector consumes the same revision-bound
metadata list the API uses and never resolves content. A Worth-origin item
served in Follow-ups keeps a typed payload from the persisted candidate with
fixed descriptors (`open_source` when available, `useful`, `acknowledge`,
`dismiss`); a Follow-up-origin item served in Worth keeps its Follow-up payload.
Identity is origin-qualified because candidate IDs are lane-local. A stale or
short Worth load fails the projection closed while the legacy Worth list stays
an atomic baseline.

The union's Worth load is a **bounded page** (store clamps any page to 1,000
rows while reporting the true total): validation is `rows == min(total,
page_limit)`, and above the bound the union ranks the top of the lane. The
legacy duplicate cross-check runs only when both read the same universe
(`worth_a_look_source_total`).

**Learned pagination.**
`GET /api/magician/v2/channel-assist/attention-learning/canonical-deliveries/worth_a_look`:
the cursorless request freezes one Worth-served root (decision,
projection/digest, posterior, policy, seed, exact revisions); cursors resolve
contiguous positions from it without re-running selection, so replay cannot
duplicate or skip. Stale digest/revision, expired cursor, scope or lane/root
mismatch → HTTP 409 `refresh_required`. Items keep the root's policy propensity
(page delivery has conditional propensity 1) and an exposure token bound to
root, page, position, revision, scope, digest and propensities; only a matching
delivered item that crosses the root-frozen dwell rule creates an idempotent
impression. Degraded delivery freezes baseline order and never invents an
exposure from an API return.

**Feedback → rank recompute.** Worth feedback (legacy endpoint included) uses
the same async rank-recompute job as Follow-ups: the response returns a pending
job, null after-rank, and a status URL. The worker publishes an after rank only
if revision, digest, generation, posterior and snapshot CAS bindings still
match; drift finishes stale. List and cursor reads never wait for recompute.
Recompute ships disabled/paused.

**Cross-lane identity.** Before ranking, a `comm` ref must parse to the exact
`(provider, account_alias, thread_id)`. Only equality with an active Follow-up
tuple creates a durable alias (no title/sender/embedding/LLM heuristics); the
Follow-up owns the card while Worth keeps its evidence, and the Worth row
returns when the Follow-up is inactive. A malformed comm ref makes
reconciliation unavailable: canonical publication fails closed and the legacy
guard withholds unresolved comm rows (non-comm sources stay eligible, with a
content-free diagnostic). Alias summaries are capped at 100 opaque records. The
page total excludes aliases before cursor construction.

Reconciliation is per candidate, so a page pays for a page: the Today read loads
the Follow-up owner set once, takes lane size and hidden count from one narrow
count, then seeks to the page and walks until full plus one row to prove
`has_more`. `reconciliation_digest` covers the owner set plus that read's rows
(never null on success — clients reject that). The group-members read still
drains the whole lane, since resolving a cluster needs the full visible
universe.

## Config

Env, on by default:

| Var | Default | Meaning |
|-----|---------|---------|
| `RESURFACING_ENABLED` | `1` | Kill switch (`0`/`false` disables). |
| `RESURFACING_SCORER_INTERVAL_SECS` | 3600 | Scorer cadence. |
| `RESURFACING_CURATOR_INTERVAL_SECS` | 86400 | Curator cadence. |
| `RESURFACING_SURFACE_CAP` | `resurfacing.surface_cap` (code 5; seed 15) | Max cards surfaced. |
| `RESURFACING_DECAY_HALFLIFE_DAYS` | 14 | Salience decay half-life. |
| `RESURFACING_RETENTION_DAYS` | 90 | Age past which terminal candidates + old dismissed signals are pruned. |
| `RESURFACING_CANDIDATE_CAP` | 2000 | Max candidates kept per scope (cap-pruned by liveness). |

Scoring is env-tunable without a rebuild (`ResurfacingScoringConfig::from_env`,
defaults = compiled consts): weights `RESURFACING_W_RECENCY|FREQUENCY|CENTRALITY|COOCCURRENCE|TEMPORAL|DORMANCY`,
half-lives `RESURFACING_RECENCY|TEMPORAL|DECAY_HALFLIFE_DAYS`, feedback
`RESURFACING_DISMISS_PENALTY_THRESHOLD|FACTOR`,
`RESURFACING_AFFINITY_THRESHOLD|BOOST_FACTOR`, utility
`RESURFACING_UTILITY_MIN|MAX_MULTIPLIER`, `RESURFACING_UTILITY_SMOOTHING`.

YAML `resurfacing` block: `memory_tiers`; centrality timeouts (above);
`recommendations_enabled` (API presentation; disabling is an independent
rollback that deletes nothing), `recommendation_min_confidence`,
`contextual_actions_enabled` (side-effecting IDs may enter the curator
allow-list); `rich_briefs_enabled`, `source_details_enabled` (enforced by the
interaction registry); `active_repair_enabled` + `active_repair_batch_size`
(sequential, revision-idempotent routing check over surfaced comm cards before
curation — required-action cards go through the Follow-up materializer and are
withheld without recording owner feedback).

## API

- `GET /api/magician/v2/channel-assist/resurfacing/today?limit=20` → `{ cards:
  [{ candidate_id, line, why_now, source_kind, source_ref, brief,
  content_revision, recommended_action, actions, temporal_anchor_at }],
  total, limit, offset, has_more, next_cursor, next_cursor_token }`. Scope comes
  only from the workspace-bound bearer. The band uses object `next_cursor`
  (`cursor_surfaced_at`, `cursor_score`, `cursor_candidate_id`); canonical
  callers pass `next_cursor_token` as `cursor`. Never fetches a source body.
  Phrasing and recommendations are batch-read per page
  (`get_phrasing_batch` / `get_recommendation_batch`) with the same revision
  guard as per-card reads; absence falls back to title + signal "why now".
- `GET …/attention-learning/canonical-deliveries/worth_a_look` → schema-v1
  `{ status, fallback_reason, root_decision, page, impression_policy, items,
  health }`; optional `page_size`, then only the opaque `cursor`; typed 409 on
  refresh-required.
- `GET …/resurfacing/{candidate_id}/detail` → persisted safe brief plus current
  source metadata, revision, status, `has_newer`, route, action capabilities.
  Never fetches raw content.
- `GET …/resurfacing/{candidate_id}/original` → explicit source resolution.
  Comms fetch the exact message and its coalesced evidence live (20k
  chars/message, 40k/response, max eight messages); task/memory return current
  scoped detail. Suppressed sources return nothing.
- `POST …/resurfacing/{candidate_id}/action` `{ "action": "open" |
  "acknowledge" | "dismiss" }`.
- `POST …/resurfacing/{candidate_id}/recommendation-event`
  `{ kind, content_revision, event: "presented" | "selected" | "completed" }`.
  `presented` deduplicates atomically; `selected`/`completed` accept only
  Details, Open source or Original after that recommendation was presented.
  Contextual actions record these inside their claim flow.
- `POST …/resurfacing/{candidate_id}/actions`
  `{ kind, idempotency_key, content_revision, input }`. Kinds: `create_task`,
  `create_reminder`, `save_to_memory`, `ask_presto`, `share`, and
  `summarize_deeper` (verified local binding only). Duplicate completed request
  replays; key conflict or stale content → 409; unavailable dependency → 503
  without changing feedback. Error `code`: `invalid`, `stale_revision`,
  `in_progress`, `idempotency_conflict`, `not_actionable`, `unavailable`,
  `not_found`, `internal`. `create_reminder` is a native Apple Reminder, never a
  Magician task: iOS creates it via EventKit and submits
  `delivery=client_apple_eventkit` + `external_id`; web submits
  `delivery=host_apple_reminders` and Magician delegates to the loopback
  desktop host. Receipt: `reminder_id`, `provider`, due time, timezone.
- `POST …/resurfacing/repair?dry_run=true&limit=10` — preview a bounded active
  routing pass; without `dry_run` it writes per-revision receipts, materializes
  Follow-ups and withholds rerouted cards. No LLM call.
- `GET …/resurfacing/stats` → `{ lanes: [{ source_kind, positive, negative,
  engagement_rate, utility_multiplier }] }` (multiplier exact for penalty,
  upper bound for boost).
- `GET …/resurfacing/observability` → see below.

(`…` = `/api/magician/v2/channel-assist`.)

## Unified UI interaction contract

Today and Town Square mount the same `ResurfacingBand`, so facts, capability
gating, pagination and actions cannot drift. A row shows the safe summary plus
at most three structured facts, with source omissions and update state visible.
Summary-only payloads fall back to a single Details action.

Details is a lazy safe-detail read; Original is a separate explicit read,
rendered as bounded plain text (no source HTML). One validated recommendation
may be the primary button; the rest of the advertised capabilities and Mark
useful/Acknowledge/Dismiss live in a keyboard menu. The client never invents a
capability.

Task, reminder, share and memory dialogs submit a content revision and one UUID
idempotency key retained across retries. Failures keep the row; only
`stale_revision` closes the dialog, reloads, and asks for re-confirmation. A web
reminder is opened by the macOS host and returns no browser URL.

Ask Presto navigates to the server-returned chat route with only
`resurfacing_candidate=<id>`; `ChatPanel` resolves it via the safe detail
endpoint and stages a bounded text attachment (never `original`). The composer
is prefilled, not sent.

## Observability

Every scorer / curator / retention / `routing_repair` pass is recorded
best-effort in `resurfacing_runs` (kind, `started_at`, `duration_ms`,
`produced`, `success`, last `error`; capped 500/scope). `GET …/observability`
returns:

- `pipeline` — per-kind aggregates; `recent_runs` — last ~50 passes.
- `funnel` — candidates by `state × source_kind` plus `pending`/`eligible`
  (cooldown elapsed, actionable now), `candidate_pool`, `cooling`, `surfaced`;
  `queue` — the same active-queue snapshot standalone.
- `watermarks`, `sizes` (every table, including repair receipts).
- `engagement` — same as `/stats`.
- `recommendations` — shown/selected/completed totals and rates by kind (labels,
  rationales and content are never dimensions).
- `briefs` — comm coverage and detail status; `actions` — started/completed/
  failed totals, per-kind events, bounded error classes (no user values).
