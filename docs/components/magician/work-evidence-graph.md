# Work-Evidence Graph

A generic, domain-agnostic substrate that distills *what work happened* from the
same execution episodes memory consolidation already sees, and turns it into
grounded, reviewable, correctable evidence. It is a **sibling** of memory-tier
knowledge (both distilled from `V3EpisodeRecord`), not a derivation of it: tier
knowledge is compact "what to remember" injected into prompts; evidence is
structured "what work happened" for reporting / Career-Copilot read paths.

Module: `magician/src/magician_v2/evidence/` (`ambient_distill`, `claims`, `compaction`, `dashboard`, `entities`, `eval`,
`feedback`, `precision`, `screen_distill`, `tier_distill`, `views`, `worklog`).
HTTP: `magician-api/src/evidence_api.rs`. Prompts:
`data/magician_v2/prompts/evidence_*`. Design:
`docs/plans/2026-03-17-work-evidence-graph-design.md`.

## Core idea: generic records, faceted domains

Records are domain-agnostic. Domain is a derived, multi-label
`Facet { label, confidence, assigned_by }` (open vocabulary, LLM-proposed,
user-overridable) — **not** a baked-in type; "work" is one facet value. Why:
keeps the substrate reusable by other producers.

## Records & storage

Records carry a `producer` tag (`task_episode` | `ambient_browser` |
`screen_observation` | `meeting` | `email` | `calendar` | future `visual`) — the
self-describing lane so consumers discover them without parsing
`source_refs`. They live in two lanes:

- **Agent-scoped** (task-linked), per-agent under the agent memory dir:
  - `evidence.json` — `Vec<EvidenceRecord>`: `evidence_id` (`evd:{episode_id}`,
    deterministic → idempotent re-distill), `summary`, `evidence_kind`,
    `observed_actions`, `entity_keys`/`people_keys`, `facets`, `importance`,
    `confidence`, `sensitivity`, `first/last_seen_at`, `status`
    (`active`/`suppressed`/`deleted`), `last_corrected_at`, `producer`.
  - `entities.json` — `Vec<EntityRecord>`: `entity_key` (normalized `type:slug`),
    `entity_type`, `canonical_name`, `aliases`, `source_refs`, `facets`, `status`,
    `merged_into`, `user_curated`, `producer`.
- **User-owned** (passive/ambient, shared across the user's agents — ambient
  browsing isn't one agent's task), under the user-memory root:
  - `work_evidence.json` / `work_entities.json` — same record shapes.

Storage methods on `AgentMemoryService`: agent-scoped
(`load/append/update_native_evidence`, `load/resolve/mutate_native_entities`);
user-owned (`load/append_user_work_evidence`, `load/resolve_user_work_entities`);
and the **unified read** `load_scoped_evidence` / `load_scoped_entities` that
merge agent-scoped ∪ user-owned (entities deduped by key). Consumers (review,
dashboard, staleness) call the unified resolver, so new producers just add to a
lane and consumers are untouched. Paths on `AgentStorage`
(`agent_evidence_path`, `agent_entities_path`, `user_work_evidence_path`,
`user_work_entities_path`). A salience gate (`EVIDENCE_SALIENCE_THRESHOLD`) and
retention caps bound what persists.

**Daily compaction.** Before the retention cap, `compact_evidence`
(`evidence/compaction.rs`) merges same-`(entity-set, evidence_kind, day)`
duplicates into one record — unioning refs/actions/facets, taking max
importance/confidence, spanning earliest→latest, and keeping the
**most-restrictive sensitivity** so a merge can't launder a sensitive item.
User-corrected and anchorless (no-entity) records pass through untouched, and
distinct `evidence_kind`s on the same entity/day stay separate. It runs on every
append (both lanes) so the cap truncates a deduped set instead of dropping valid
older items; deterministic + idempotent.

## Pipeline

1. **Distill** — the consolidation hook (`memory_consolidator`, on
   `AgentCycleCompleted` with ≥1 updated tier) calls `distill_episode`
   (`evidence_distill_system`/`_user` prompts, `distill_evidence` op). The model
   decides promote/skip and proposes summary, kind, keys, enriched `entities`,
   and facets. Deterministic stamping owns ids/provenance/timestamps.
2. **Resolve entities** — `entity_candidates_from_proposal` builds anchors
   (LLM-enriched name/aliases, falling back to key-derived); `resolve_entities`
   merges **conservatively** (exact key/alias only — no fuzzy merge → no
   fragmentation, no spurious merges). Free-text aliases are display/manual-merge
   metadata, never auto-merge triggers.
3. **Assemble + synthesize** — `select_evidence_window(records, since, facet)`
   (active-only) → `build_review_packet` → `synthesize_review` (`evidence_review`
   op) emits Markdown with inline `[evd:...]` citations, persisted as a durable
   artifact (`evidence-reviews` namespace) carrying `input_evidence_ids`.
4. **Verify** — `verify_review` = deterministic citation coverage (every claim
   bullet must cite an admissible id) × the `evidence_review_verify` critic
   (catches invented/exaggerated claims that cite real ids). A grounding note is
   appended to the artifact; `strict` review requests get `422` when ungrounded.

## Corrections, durability & lineage

- **Corrections** (inbox): evidence suppress / delete-tombstone / re-facet;
  entity rename / merge / split / suppress / delete. Merges tombstone the absorbed
  anchor with `merged_into` (reversible via `split_entity`).
- **Durability** — `EvidenceRecord::is_user_corrected()` locks human-touched
  records so re-distillation never resurrects a suppression or wipes a re-label;
  `resolve_entities` never resurrects a deleted anchor and won't overwrite a
  user-curated canonical name.
- **Reverse lineage** — `last_corrected_at` is stamped on every correction; the
  reviews list flags a review `stale` when any cited evidence changed after it was
  generated.
- **Forward lineage** — `GET /evidence?with_usage=true` scans review artifacts and
  reports the reviews each returned evidence record was cited in (`used_in`).
  Evidence listing is server-paged (`limit`/`offset`, default 50, max 200), so
  lineage decoration is bounded to the current page.

## Quality metrics

- `evidence::eval` — deterministic metrics: salience precision, merge
  distinctness, `merge_quality_regression` (known-distinct fixtures kept
  separate), review citation coverage, correction rate; plus
  `evidence_quality_report` (totals, `correction_rate`, sensitive/anchored
  fractions, distinct entities, facet coverage, compaction headroom — should stay
  ~0). Headless over RAW (unfiltered) lanes: `magician evidence-eval --principal
  <p> --workspace <w> [--agent <id>]` → JSON.
- `evidence::precision` — LLM judge: `grade_evidence_precision` samples
  `(record, episode outcome)` pairs, asks `evidence_precision_judge` whether the
  summary invents nothing; `precision = faithful / sampled` (parse failure =
  not faithful). `magician evidence-precision [--agent <id>] [--sample N]`.
- `evidence::feedback` — utility: `POST /evidence/review/feedback`
  (`{review, verdict: accepted|edited|discarded, edit_ratio?}`);
  `GET /evidence/utility` computes `utility_rate = (accepted + lightly-edited) /
  distinct reviews` (latest per review wins; `edit_ratio ≤ 0.3` is light).
  `/reviews` posts it on accept/edit/discard.

## HTTP API (`/api/magician/v2`)

| Method + path | Purpose |
|---|---|
| `POST /evidence/review` | assemble window+facet, synthesize, verify, persist; returns markdown + `verification` |
| `GET /evidence` | list evidence (active+suppressed; `facet`/`days`/`include_deleted`/`with_usage`; paged with `limit`/`offset`, returns `total_count`/`returned_count`/`has_more`) |
| `POST /evidence/correct` | suppress / unsuppress / delete / set_facets |
| `GET /entities` | list entity anchors (`facet`/`include_deleted`) |
| `POST /entities/correct` | rename / merge / split / suppress / unsuppress / delete |
| `GET /evidence/reviews` | past reviews with `stale` + `cited_count` |
| `GET /evidence/dashboard` | impact dashboard payload (`facet`/`days`) |
| `POST /evidence/dashboard/publish` | publish the dashboard as an artifact-driven surface |
| `POST /evidence/review/feedback` | per-review `accepted`/`edited`/`discarded` |
| `GET /evidence/utility` | aggregated utility rate |
| `GET /ambient/status` · `PUT /ambient/config` | ambient capture consent (enable/pause) + denylist + counters |
| `POST /ambient/enroll` | issue a scope-bound collector token (`X-Collector-Token`) |
| `POST /ambient/signals/batch` | extension batch upload (consent-gated + ingress) → analytics raw store |
| `POST /ambient/distill` | local-only manual distill of the window's ambient signals → user-owned evidence |
| `GET /ambient/stats` | heavier ledger view for the Browser Tabs stats surface |
| `POST /screen/observations/distill` | distill the window's desktop screen-observation sessions → user-owned evidence |
| `POST /evidence/distill/{producer}` | generic tier connector: distil a producer's per-`(key,day)` roll-up tier into user-owned evidence (`{producer}` ∈ `meeting`/`email`/`calendar`/… via `tier_distill::producer_spec`) |
| `GET /observe/accounts` | accounts the Email/Calendar consent cards offer — `gws_accounts`/`agentmail_accounts` from operator-config, filtered to authenticated (`gws-presto` excluded) |
| `GET /observe/{producer}/status` · `PUT /observe/{producer}/config` | per-producer (`email`/`calendar`) consent + cadence config (enabled, chosen accounts, frequency, time, suppress_sensitive); PUT refuses to enable without a connected account |

Scope resolves from the workspace-bound bearer. CLI
`distill-evidence` (`--persist` seeds evidence + entities), `review`,
`evidence-eval`, `evidence-graph`, `evidence-claims`, and `evidence-precision`
drive the same library for backfill / one-shot use.

## Layer-3 derived views

`evidence::views` projects the evidence base into typed, re-runnable views —
Rust projections over the same JSON `dashboard.rs` reads (no durable edge store):
`entity_neighborhood` (evidence touching an entity + co-occurring entities /
people / kinds) and `cooccurrence_edges` (**sparse relations as a derived view** —
entity-pair weights ≥ `min_weight`). Both are facet-parameterized and skip
non-live / affirmatively-sensitive records. Run headless: `magician evidence-graph
[--entity <key>] [--facet <f>] [--min-weight N]` → JSON.

## Generic claim records

`evidence::claims` derives facet-scoped interpretive **claims** over evidence and
keeps them **ephemeral** (read/output-time, never a durable tier). `propose_claims`
asks the model (store prompt `evidence_claims`) for assertions grounded in the
assembled packet's evidence ids; `validate_claim_grounding` is the deterministic
gate — every `supporting_evidence_ids` entry must resolve to an in-scope (active,
non-sensitive) record and there must be ≥ 1, else the claim is dropped. Run
headless: `magician evidence-claims [--facet <f>] [--days N]` → JSON of
`{claim, grounding}` for grounded claims only.

## Impact dashboard + artifact-driven surface

`evidence::dashboard` deterministically rolls accrued evidence + entities into a
`DashboardData` payload — headline counts, coverage by facet, entity-type mix,
top entities (by source count), weekly activity, recent evidence, and computed
**visibility gaps** (thin-facet + single-source-anchor signals). `GET
/evidence/dashboard` returns it as JSON for the UI.

`POST /evidence/dashboard/publish` renders the payload to Markdown
(`render_dashboard_markdown`) and pushes it through the existing artifact-driven
surface machinery: it creates an `Internal` task, writes the Markdown as that
task's user-output (`write_user_output_direct`), then `publish_surface_record`
materializes it to MUIJ on the `/briefing` rail. A stable `logical_surface_id`
(`evidence-dashboard:<agent>:<facet>`) means re-publishing supersedes the prior
surface rather than piling up.

## Ambient browser capture

Opt-in, user-owned capture of *normal browsing* (distinct from `magicutor`'s
agent-driven, execution-scoped page-signal lane), using the existing
`magicutor/extension`.

```
normal tab → ambient_browsing.js (content script: live-DOM metadata summary, no CDP)
           → ambient_collector.js (chrome.storage.local queue + ~30s alarm flush)
           → POST /ambient/signals/batch
           → analytics.events (event_type="ambient_signal")        [Layer 1 raw store]
           → AmbientDistillWorker while Observe-tabs is enabled
             (cluster by origin/day → salience → local ambient_distill LLM op)
           → user-owned work_evidence.json (producer="ambient_browser")
           → load_scoped_evidence (unified) → /reviews · dashboard
```

- **Capture** (`magicutor/extension/ambient_browsing.js`): top-frame only,
  metadata-first (title, headings, structure, password-field flag), `safeUrl`
  redaction (mirrors `ambient_page_signals.js`), SPA-route-aware. No full HTML /
  screenshots, no CDP debugger. `ambient_collector.js` queues + batch-uploads;
  incognito tabs are dropped on receipt. The collector sends scope via
  `magicianFetch` (bearer) and, when paired, `X-Collector-Token`.
- **Consent + control**: the `/observe` **"Observe tabs"** card →
  `GET /ambient/status` + `PUT /ambient/config` (enable/pause + denylist). Capture
  never persists unless `enabled`. Distillation follows the same lifecycle: the
  background `AmbientDistillWorker` only processes scopes whose ambient config is
  currently enabled.
- **Ingest** (`api::ambient_api`): consent-gated; ingress drops denylisted origins,
  redacts secret-keyed fields, dedups within a batch; persists sanitized signals to
  the analytics raw store (never prompt-injected) **and** a scoped
  `ambient/signals.json` buffer the distiller reads (the analytics sink holds
  `analytics.duckdb` exclusively).
- **Distill** (`evidence::ambient_distill`): local-only by guard and config
  under `privacy.processing.mode: local` (`ambient_distill` must map to an
  Ollama profile, shipped as `op-ambient-distill-local` on the
  `*local_generation_model` anchor); under `cloud` the guard accepts the
  `when_cloud` arm (`op-ambient-distill-remote`). Unbound is off in both modes
  (`llm_dispatch_seam::resolve_local_provider_for_operation`).
  The worker defaults to a 3-minute startup delay, 15-minute tick, 1-day lookback,
  max 4 host/day clusters per scope per tick, and at least 30 minutes before
  re-distilling a changed host/day cluster. `cluster_signals` (by origin) →
  deterministic salience (action weight submit≫view, engagement, work-tool boost,
  login/SSO noise penalty) → `distill_ambient_cluster_pinned` (one local
  `ambient_distill` LLM call/cluster) → `stamp_ambient_evidence` (idempotent
  `evd:amb:{host}:{day}`, `producer="ambient_browser"`) → user-owned lane +
  entities. Worker receipts live in `ambient/distill_state.json` and store only
  host/day keys, signal hashes, counts, and timestamps.

**Privacy posture**: opt-in + pausable from the UI; private/incognito excluded;
denylist + secret-field redaction; metadata-first; only sanitized signals reach
the raw store. **Collector-token pairing**: the Observe "Pair browser" control
issues a scope-bound token (`POST /ambient/enroll`) that the extension sends as
`X-Collector-Token`; the server resolves scope from the token so the untrusted
extension can't self-assert a principal (unpaired → default scope).
**Sensitivity classification**: the ambient distiller classifies each record
(`work`/`personal`/`financial`/`health`/`credentials`/`private_comms`/…);
affirmatively-sensitive evidence is default-suppressed from reviews/dashboard
(`load_scoped_evidence`) and **never promoted into agent memory** (the memory
bridge skips it). **Rejection audit + receipt ledger**: rejected signals leave a
content-free stub (id + reason + ts) and a server-owned `signal_id` ledger gives
cross-batch idempotency (`ambient/receipts.json`).

**Stats ledger.** Accepted signals retain redacted page metadata only.
`/ambient/status` exposes merge-safe current-day counters; `/ambient/stats`
is the heavier ledger (`from`/`to`, 90-day cap, compacted daily summaries
plus live current-day detail). Retention sweep writes
`ambient/stats/daily/dt=YYYY-MM-DD/summary.json`. Defaults: signal detail 14
days, batch 30, run/cluster journals 90
(`AMBIENT_STATS_*_RETENTION_DAYS`). Each distill run posts one display-only
summary into `Tabs — YYYY-MM-DD`, deduped by run id.

## Desktop screen observations

The screen-observe rail writes per-session roll-ups to
`user.screen_observations` (`key = observe:<id>`). `evidence::screen_distill`
clusters by `(purpose, day)`, gates on `SCREEN_SALIENCE_THRESHOLD` (0.4),
calls `screen_evidence_distill`, and upserts user-owned evidence
(`producer="screen_observation"`, `evd:scr:{purpose}:{day}`). Input is the
tier, not raw frames. Screen-specific code is the cluster type and two store
prompts; salience, entities, memory bridge, compaction, dashboard, claims,
and views are shared.

## Evidence → memory → retrieval

`learning::evidence_memory_bridge::route_user_evidence_to_memory` turns an
ambient `EvidenceRecord` into a **review-gated** `MemoryFact` targeting
`user.knowledge` (evidence id is the upsert key; provenance keeps
`evd:amb:*`). Always `review_required`. On approval, `MemoryIndexMaintainer`
indexes it. Agent-episode evidence stays on the reflection
`memory_candidates` path. Chat, the autonomous executor, and the
orchestrator all pass a real query into
`render_memory_tiers_for_prompt_with_index`. User `knowledge` ranks 90 in
`base_tier_priority`; `weg-ambient-memory-regression` guards the path.

## Meeting / email / calendar producers

`tier_distill` (`POST /evidence/distill/{producer}` and the
`distill_evidence` tool) is the generic connector. `meeting` is auto-triggered:
`ScopedMeetingMemoryWriter::write_summary` calls
`distill_tier_producer("meeting", …)` fail-soft after appending
`meeting_capture` (one `evd:meet:*` per thread-id + day). `email`/`calendar`
are consent-gated scheduled tasks from `observe_connectors_api::ensure_schedule`
(skills write `user.email_evidence`/`user.calendar_evidence`, then call
`distill_evidence`). `ambient` stays bespoke (raw signal buffer, not a tier).

## UI

`/reviews` (generate + slider window + facet + grounding verdict + stale +
regenerate, plus a **Dashboard** tab with **Publish to /briefing**), `/evidence`
(Evidence + Entities inbox tabs), and the `/observe` **"Observe tabs"** card
(ambient consent + status + denylist). `/reviews` goes through the typed
`ui/unified-ui/src/lib/reviews/api.ts` boundary.

## Config

`llm-router.yaml` `operation_mapping`: `distill_evidence` →
`op-memory-evidence-distillation-local-chunked` (`when_cloud`:
`op-memory-evidence-distillation-remote`), `ambient_distill` →
`op-ambient-distill-local` (`when_cloud`: `op-ambient-distill-remote`);
`screen_evidence_distill`, `tier_evidence_distill`, and
`evidence_review_verify` → `op-memory-evidence-distillation-fast`
(`gpt-6-luna` with `gpt-6-luna` retry), `evidence_review` →
`gpt6luna-responses-toolsnone-out16k`, `evidence_claims` →
`gpt6luna-responses-toolsany`, and `evidence_precision_judge` →
`gpt61sol-responses-toolsany` (GPT-6.1 Sol).

The judge deliberately stays on a stronger model than the distillers: a judge on
the cheapest model degrades silently — slightly wrong evidence, confidently
scored.

Not built: DuckDB-SQL Layer-3 views, a `visual` producer, sensitive-facet
capture controls, a domain-specific Career-Copilot output layer.
