# Work-Evidence Graph

A generic, domain-agnostic substrate that distills *what work happened* from the
same execution episodes memory consolidation sees, and turns it into grounded,
reviewable, correctable evidence. It is a **sibling** of memory-tier knowledge
(both distilled from `V3EpisodeRecord`), not a derivation of it: tier knowledge
is compact "what to remember" for prompts; evidence is structured "what work
happened" for reporting and Career-Copilot read paths.

Module: `magician/src/magician_v2/evidence/`; the parts this page covers are
`ambient_distill`, `claims`, `compaction`, `dashboard`, `entities`, `eval`,
`feedback`, `precision`, `screen_distill`, `tier_distill`, `views` and
`worklog` (the directory also holds other evidence modules, e.g.
`outward_assertions`, `observed_statements`, `transcript_ingestion`).
HTTP: `magician-api/src/evidence_api.rs`. Prompts:
`data/magician_v2/prompts/evidence_*`. Design:
`docs/plans/2026-03-17-work-evidence-graph-design.md`.

## Core idea: generic records, faceted domains

Records are domain-agnostic. Domain is a derived, multi-label
`Facet { label, confidence, assigned_by }` (open vocabulary, LLM-proposed,
user-overridable), **not** a baked-in type; "work" is one facet value.

## Records & storage

Records carry a `producer` tag (`task_episode` | `ambient_browser` |
`screen_observation` | `meeting` | `email` | `calendar`; a free string,
default `task_episode`), so
consumers discover them without parsing `source_refs`. Two lanes:

- **Agent-scoped** (task-linked), under the agent memory dir:
  - `evidence.json` — `Vec<EvidenceRecord>`: `evidence_id` (`evd:{episode_id}`,
    deterministic, so re-distill is idempotent), `summary`, `evidence_kind`,
    `observed_actions`, `entity_keys`/`people_keys`, `facets`, `importance`,
    `confidence`, `sensitivity`, `first/last_seen_at`, `status`
    (`active`/`suppressed`/`deleted`), `last_corrected_at`, `producer`.
  - `entities.json` — `Vec<EntityRecord>`: `entity_key` (normalized `type:slug`),
    `entity_type`, `canonical_name`, `aliases`, `source_refs`, `facets`, `status`,
    `merged_into`, `user_curated`, `producer`.
- **User-owned** (passive/ambient, shared across the user's agents), under the
  user-memory root: `work_evidence.json` / `work_entities.json`, same shapes.

`AgentMemoryService` methods: agent-scoped
(`load/append/update_native_evidence`, `load/resolve/mutate_native_entities`),
user-owned (`load/append_user_work_evidence`, `load/resolve_user_work_entities`),
and the **unified read** `load_scoped_evidence` / `load_scoped_entities`, which
merges both lanes (entities deduped by key). Consumers call the unified read, so
new producers only add to a lane. Paths on `AgentStorage`:
`agent_evidence_path`, `agent_entities_path`, `user_work_evidence_path`,
`user_work_entities_path`. A salience gate (`EVIDENCE_SALIENCE_THRESHOLD`) and
retention caps bound what persists.

**Daily compaction.** On every append (both lanes), before the retention cap,
`compact_evidence` (`evidence/compaction.rs`) merges
same-`(entity-set, evidence_kind, day)` duplicates: it unions refs/actions/facets, takes max
importance/confidence, spans earliest→latest, and keeps the
**most-restrictive sensitivity**, so a merge cannot launder a sensitive item.
User-corrected and anchorless records pass through; it is deterministic and
idempotent.

## Pipeline

1. **Distill** — the consolidation hook (`memory_consolidator`, on
   `AgentCycleCompleted` with ≥1 updated tier) calls `distill_episode`
   (`evidence_distill_system`/`_user` prompts, `distill_evidence` op). The model
   decides promote/skip and proposes summary, kind, keys, `entities` and facets;
   deterministic code stamps ids, provenance and timestamps.
2. **Resolve entities** — `entity_candidates_from_proposal` builds anchors;
   `resolve_entities` merges **conservatively** (exact key/alias only, no fuzzy
   merge). Free-text aliases are display/manual-merge metadata, never auto-merge
   triggers.
3. **Assemble + synthesize** — `select_evidence_window(records, since, facet)`
   (active-only) → `build_review_packet` → `synthesize_review` (`evidence_review`
   op) emits Markdown with inline `[evd:...]` citations, persisted as an artifact
   (`evidence-reviews` namespace) carrying `input_evidence_ids`.
4. **Verify** — `verify_review` = deterministic citation coverage (every claim
   bullet cites an admissible id) × the `evidence_review_verify` critic (catches
   invented claims that cite real ids). A grounding note is appended; `strict`
   review requests get `422` when ungrounded.

## Corrections, durability & lineage

- **Corrections** (inbox): evidence suppress / delete-tombstone / re-facet;
  entity rename / merge / split / suppress / delete. A merge tombstones the
  absorbed anchor with `merged_into` (reversible via `split_entity`).
- **Durability** — `EvidenceRecord::is_user_corrected()` locks human-touched
  records against re-distillation; `resolve_entities` never resurrects a deleted
  anchor or overwrites a user-curated name.
- **Reverse lineage** — `last_corrected_at` is stamped on every correction; a
  review is flagged `stale` when any cited evidence changed after it.
- **Forward lineage** — `GET /evidence?with_usage=true` reports the reviews each
  returned record was cited in (`used_in`), bounded to the current page
  (`limit`/`offset`, default 50, max 200).

## Quality metrics

- `evidence::eval` — deterministic: salience precision, merge distinctness,
  `merge_quality_regression`, review citation coverage, correction rate, plus
  `evidence_quality_report` (totals, `correction_rate`, sensitive/anchored
  fractions, distinct entities, facet coverage, compaction headroom ≈ 0).
  Headless over raw lanes:
  `magician evidence-eval --principal <p> --workspace <w> [--agent <id>]` → JSON.
- `evidence::precision` — `grade_evidence_precision` samples
  `(record, episode outcome)` pairs and asks `evidence_precision_judge` whether
  the summary invents nothing; `precision = faithful / sampled` (parse failure =
  not faithful). `magician evidence-precision [--agent <id>] [--sample N]`.
- `evidence::feedback` — `POST /evidence/review/feedback`
  (`{review, verdict: accepted|edited|discarded, edit_ratio?}`), which `/reviews`
  posts on accept/edit/discard; `GET /evidence/utility` computes
  `utility_rate = (accepted + lightly-edited) / distinct reviews` (latest per
  review; `edit_ratio ≤ 0.3` is light).

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
| `GET /observe/{producer}/status` · `PUT /observe/{producer}/config` | per-producer (`email`/`calendar`) consent + cadence (enabled, accounts, frequency, time, suppress_sensitive); PUT refuses to enable without a connected account |

Scope resolves from the workspace-bound bearer. CLI `distill-evidence`
(`--persist` seeds evidence + entities), `review`, `evidence-eval`,
`evidence-graph`, `evidence-claims` and `evidence-precision` drive the same
library for backfill and one-shot use.

## Layer-3 derived views

`evidence::views` projects the evidence base into re-runnable Rust views over
the same JSON `dashboard.rs` reads (no durable edge store):
`entity_neighborhood` (evidence touching an entity plus co-occurring entities,
people and kinds) and `cooccurrence_edges` (entity-pair weights ≥ `min_weight`).
Both are facet-parameterized and skip non-live or sensitive records. Headless:
`magician evidence-graph [--entity <key>] [--facet <f>] [--min-weight N]` → JSON.

## Generic claim records

`evidence::claims` derives facet-scoped interpretive **claims** and keeps them
**ephemeral** (never a durable tier). `propose_claims` asks the model (prompt
`evidence_claims`) for assertions grounded in the packet's evidence ids;
`validate_claim_grounding` drops a claim unless it has ≥ 1
`supporting_evidence_ids` and every one resolves to an active, non-sensitive
record. Headless: `magician evidence-claims [--facet <f>] [--days N]` → JSON of
`{claim, grounding}`.

## Impact dashboard + artifact-driven surface

`evidence::dashboard` rolls evidence and entities into a `DashboardData`
payload: headline counts, coverage by facet, entity-type mix, top entities,
weekly activity, recent evidence, and **visibility gaps** (thin-facet and
single-source-anchor signals). `GET /evidence/dashboard` returns it as JSON.

`POST /evidence/dashboard/publish` renders it to Markdown
(`render_dashboard_markdown`), creates an `Internal` task, writes the Markdown as
its user-output (`write_user_output_direct`), and `publish_surface_record`
materializes it to MUIJ on the `/briefing` rail. A stable `logical_surface_id`
(`evidence-dashboard:<agent>:<facet>`) makes a re-publish supersede the prior
surface.

## Ambient browser capture

Opt-in, user-owned capture of *normal browsing* (distinct from `magicutor`'s
agent-driven page-signal lane), using `magicutor/extension`.

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
  redaction (mirrors `ambient_page_signals.js`), SPA-route-aware. No full HTML,
  screenshots or CDP. Incognito tabs are dropped on receipt. The collector sends
  scope via `magicianFetch` (bearer) and, when paired, `X-Collector-Token`.
- **Consent**: the `/observe` **"Observe tabs"** card → `GET /ambient/status` +
  `PUT /ambient/config` (enable/pause + denylist). Nothing persists, and
  `AmbientDistillWorker` processes no scope, unless `enabled`.
- **Ingest** (`api::ambient_api`): consent-gated; drops denylisted origins,
  redacts secret-keyed fields, dedups within a batch; persists sanitized signals
  to the analytics raw store (never prompt-injected) **and** a scoped
  `ambient/signals.json` buffer the distiller reads (the analytics sink holds
  `analytics.duckdb` exclusively).
- **Distill** (`evidence::ambient_distill`): under
  `privacy.processing.mode: local`, `ambient_distill` must map to an Ollama
  profile (`op-ambient-distill-local` on the `*local_generation_model` anchor);
  under `cloud` the `when_cloud` arm (`op-ambient-distill-remote`) is accepted.
  Unbound is off in both modes
  (`llm_dispatch_seam::resolve_local_provider_for_operation`). Worker defaults:
  3-minute startup delay, 15-minute tick, 1-day lookback, at most 4 host/day
  clusters per scope per tick, at least 30 minutes before re-distilling a changed
  cluster. `cluster_signals` (by origin) → deterministic salience (submit≫view,
  engagement, work-tool boost, login/SSO penalty) →
  `distill_ambient_cluster_pinned` (one call per cluster) →
  `stamp_ambient_evidence` (idempotent `evd:amb:{host}:{day}`,
  `producer="ambient_browser"`) → user-owned lane + entities. Receipts in
  `ambient/distill_state.json` hold only host/day keys, signal hashes, counts and
  timestamps.

**Privacy posture**: opt-in and pausable; incognito excluded; denylist and
secret-field redaction; metadata-first. **Collector-token pairing**: "Pair
browser" issues a scope-bound token (`POST /ambient/enroll`) sent as
`X-Collector-Token`; the server resolves scope from it, so the extension cannot
self-assert a principal (unpaired → default scope). **Sensitivity**: the
distiller classifies each record (`work`/`personal`/`financial`/`health`/
`credentials`/`private_comms`/…); sensitive evidence is suppressed from
reviews/dashboard by default and **never promoted into agent memory**.
**Rejection audit**: rejected signals leave a content-free stub (id + reason +
ts), and a server-owned `signal_id` ledger (`ambient/receipts.json`) gives
cross-batch idempotency.

**Stats ledger.** Accepted signals keep redacted page metadata only.
`/ambient/status` exposes current-day counters; `/ambient/stats` is the heavier
ledger (`from`/`to`, 90-day cap, compacted daily summaries plus live current-day
detail). The retention sweep writes
`ambient/stats/daily/dt=YYYY-MM-DD/summary.json`. Defaults: signal detail 14
days, batch 30, run/cluster journals 90 (`AMBIENT_STATS_*_RETENTION_DAYS`). Each
distill run posts one display-only summary into `Tabs — YYYY-MM-DD`, deduped by
run id.

## Desktop screen observations

The screen-observe rail writes per-session roll-ups to
`user.screen_observations` (`key = observe:<id>`). `evidence::screen_distill`
clusters by `(purpose, day)`, gates on `SCREEN_SALIENCE_THRESHOLD` (0.4), calls
`screen_evidence_distill`, and upserts user-owned evidence
(`producer="screen_observation"`, `evd:scr:{purpose}:{day}`). Input is the tier,
not raw frames; only the cluster type and two prompts are screen-specific.

## Evidence → memory → retrieval

`learning::evidence_memory_bridge::route_user_evidence_to_memory` turns an
ambient `EvidenceRecord` into a **review-gated** `MemoryFact` targeting
`user.knowledge` (evidence id is the upsert key; provenance keeps `evd:amb:*`;
always `review_required`). On approval, `MemoryIndexMaintainer` indexes it.
Agent-episode evidence stays on the reflection `memory_candidates` path. Chat,
the autonomous executor and the orchestrator pass a real query into
`render_memory_tiers_for_prompt_with_index`. User `knowledge` ranks 90 in
`base_tier_priority`; `weg-ambient-memory-regression` guards the path.

## Meeting / email / calendar producers

`tier_distill` (`POST /evidence/distill/{producer}` and the `distill_evidence`
tool) is the generic connector. `meeting` is auto-triggered:
`ScopedMeetingMemoryWriter::write_summary` calls
`distill_tier_producer("meeting", …)` fail-soft after appending
`meeting_capture` (one `evd:meet:*` per thread-id + day). `email`/`calendar` are
consent-gated scheduled tasks from `observe_connectors_api::ensure_schedule`
(skills write `user.email_evidence`/`user.calendar_evidence`, then call
`distill_evidence`). Ambient stays bespoke (raw signal buffer, not a tier).

## UI

`/reviews` (generate, window slider, facet, grounding verdict, stale,
regenerate, and a **Dashboard** tab with **Publish to /briefing**), `/evidence`
(Evidence + Entities inbox tabs), and the `/observe` **"Observe tabs"** card.
`/reviews` uses the typed `ui/unified-ui/src/lib/reviews/api.ts` boundary.

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
`gpt61sol-responses-toolsany` (GPT-6.1 Sol). The judge stays on a stronger model
than the distillers on purpose: a cheap judge degrades silently.

Not built: DuckDB-SQL Layer-3 views, a `visual` producer, sensitive-facet
capture controls, a domain-specific Career-Copilot output layer.
