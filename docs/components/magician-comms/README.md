# Magician Comms

The comms data plane — **channel-assist** (Gmail, calendar, WhatsApp, Telegram,
and Kapso ingestion, distillation, annotation, sync, and reconciliation) — as a
satellite crate over the `magician` lib. The lib has zero references to it;
`magician-api` and `magician-bin` import channel types from
`magician_comms::channel_assist`.

## Startup and workers

- Under the HTTP binary, channel sync/distillation/classification, bridges,
  historical backfills, rank/training and resurfacing workers wait for
  [HTTP readiness](../magician/startup.md) before their first pass or configured
  startup delay. Scope, consent, lease and cancellation rules still apply.
  Embedded callers without a startup barrier start normally.
- The worker resumes shared [memory lifecycle](../magician/memory-lifecycle.md)
  reviews and durable clarification answers. The preference endpoint saves to
  canonical memory before recording its episode.
- The resurfacing worker reconciles bounded
  [memory connections](../magician/memory-connections.md): it resolves current
  sources through the interaction registry and delivers advice through Worth a
  look, For you or durable HITL. Questions request clarification only; responses
  never authorize an inferred action. Late ordinary curation cannot overwrite a
  live connection's explanation, and withdrawal survives restarts.
- The [memory connection live evaluator](../magician/memory-connections-evaluation.md)
  drives the production worker with an optional observation callback in
  disposable scopes; ordinary passes attach no observer.
- Memory connection reviews use the shared Decision Engine classification runner
  in shadow by default. Surface decisions still require grounded text, verbatim
  source validation and the existing publication owner. Saved decisions are
  withdrawn when their policy/profile identity changes (review-cache keys include
  it); receipts join the shared LLM/Decision Model ledger. Background reference
  replays re-read candidate and sources and skip changed or revoked content.
  Reconciliation waits briefly for an expired policy refresh; an outage defers
  new publication without retiring decisions, while source revocation still
  withdraws them.
- The connection runtime reuses the boot-installed scoped taste-profile loader.
  Profile citations are version-checked before publication and answer
  reconciliation ([profile handoff](../magician/memory-connections.md#approved-profile-handoff)).
- User requests minted here construct `UserRequest` with `sensitive: None` —
  ordinary decisions, never credential collection
  ([sensitivity contract](../magician/hitl-attention.md#sensitivity-contract-secure-hitl-credentials-p1)).

## Storage

- Memory bridges (feedback, evidence, patterns, writing preferences) use
  `process_storage::workspace()` / `AgentMemoryResolver::with_workspace_layout`
  and never resolve `MAGICIAN_ROOT_DIR` themselves. The scoped mail-assist
  DuckDB opens through `database_file_path(..., ChannelAssistDuckdb)`; `/storage`
  inventory uses the same kit helpers. Device-local iMessage/WhatsApp ingest
  stays outside the kit.
- Distillation queue retirement (`channel_assist::store::distill_queue`):
  out-of-window pending/retryable work becomes `expired` through bounded
  conditional updates, preserving source rows and results. Reads use the reader
  and filter dates before the limit; empty passes never take the writer.
  Backfill admission ignores excluded history. See
  [Distillation](../magician/mail-assist.md#distillation).
- The mail-assist DuckDB throttles `CHECKPOINT` after committed writes. If
  another write transaction is active DuckDB refuses it; the WAL already holds
  the bytes, so the store defers to the next attempt, reports success, and logs
  at debug.
- The production Storage inventory includes the workspace's encrypted App store,
  using the App policy and maintenance descriptors from
  `magician_v2::storage_governance` (integrity, checkpoint/optimization,
  encrypted reclamation only when the database exists). The API dispatches these
  through the canonical App registry owner. Inventory reports SQLite main/WAL/SHM
  sizes without opening the store. See [Storage governance](../magician/storage-governance.md).
- Inventory coalesces concurrent reads per scope, but attention optimization,
  retention and reclamation do not hold that gate; the attention entry reads
  file metadata only, so App store controls stay visible during maintenance.
  Completion invalidates the short inventory cache (`make test-app-storage`).
- Channel Assist owns a reader/writer admission gate for online copy compaction.
  The governance worker schedules Channel and Feed repairs, publishes scoped
  activity events and persists lightweight status. Annotation reads and message
  imports batch at 128. See
  [Storage Governance](../magician/storage-governance.md#automatic-channel-assist-and-feed-maintenance).

## Product lane vs substrate

The crate is split into the **product lane** and the **substrate**
(seam-registered modules, not an app package):

- **Product lane — `channel_assist::assist`** owns every product decision:
  `classify` (label vocabulary, off-vocabulary coercion, confidence
  normalization, needs_approval promotion), `reconcile`
  (retire/supersede/require-review/stale/routing-retracted rules), `distill`
  (local-only information-brief/follow-up-hint contract), `content` (pure
  MIME/quote/chunk prep), `reply_draft` (local-pinned drafting),
  `writing_preferences`, `fixtures`/`draft_eval`, `quality_budgets` (the
  Today-projection latency budget), and `completion_port` — the port through
  which reconciliation reports owner-proved completions to attention learning
  without naming it.
- **Substrate (Layer 1)**: connectors/adapters (`adapters/`,
  `adapter_registry`), ingestors (`ingest_*`), `gws_client`, `registry`, `sync`,
  `store` (the scoped DuckDB plane, including transaction-bound
  annotation-lifecycle application functions whose decisions live in `assist`),
  `channel_providers` (transport registry), `live_content`, `channel_observe`,
  the evidence/feedback/pattern bridges, and the comms-coupled worker trees.
- Consumers import product types from `channel_assist::assist::{...}` directly.
- **Tier-name contracts**: the `user.email_evidence`-style tier names are the
  lib-side contract `magician_v2::evidence::tier_contracts`.

## Lib-side seams

- `channel_assist/` is the crate's real Rust name (formerly the
  `magician_v2/mail_assist/` directory).
- `magician_v2::observe_connectors` owns the unified channel-observe config plus
  `ChannelLane` (in `magician_v2::channel_types`).
- `magician_v2::llm_dispatch_seam` owns the pinned local-LLM dispatch seam
  (`DistillLlm`, `RouterDistillLlm`, `RouterPinnedDispatch`, binding resolution,
  mail LLM telemetry), shared with memory-applicability judging.
- Corpus-generic resurfacing and attention-learning cores live at
  `magician_v2::attention::{resurfacing,learning}` behind
  `magician_v2::resurfacing_seam`. This crate keeps the comms-coupled worker,
  curator, action service, interaction registry, corpus source adapter, and the
  three mail-fed attention workers (historical bootstrap, semantic extraction
  backfill, rank recompute). See [resurfacing.md](../magician/resurfacing.md).

## Ingest CLIs are resolved before spawn

The `gws`, `kapso` and `agentmail` clients resolve their program with
`runtime_core::process::resolve_program` against the scoped PATH they hand the
child: a bare name plus a `PATH` override makes Rust `fork` instead of
`posix_spawn`, and a forked copy of the server can hang in macOS atfork handlers.
See `docs/components/runtime-core/runtime-core.md`.

## Attention-store liveness

- Canonical projection applies Slice-5 personal ranking after Slice-1 order and
  routing, per lane, without changing `served_route`. Shipped YAML is
  `bandit.mode: shadow` with no snapshot pin; a store-installed snapshot is enough.
- Rank-recompute jobs enqueued without a client decision id reconstruct the
  served decision from `attention_decision_items` (raw and canonical candidate
  aliases, selected rows first) and retry until the ledger is visible rather than
  staling as `no_served_decision`.
- Reconciliation seeks delivery page items through the durable
  `(decision_id, position)` index, created at store bootstrap for new and
  existing databases. A query-plan test requires that covering index.
- The exact reconciliation scan keeps its atomic writer snapshot, but a SQLite
  progress deadline interrupts it after 5 s so a planner regression fails the
  pass and releases the writer.
- Serving-path writes (decision, projection, delivery) wait at most 2 s for the
  attention writer. Past that, projection fails closed to the atomic baseline
  with reason `attention_writer_busy`, and delivery creation answers 503 with
  `Retry-After`. Why: page reads must never queue behind maintenance (Today's
  lane budget is 250 ms).
- Historical bootstrap discovers scopes from the mail, resurfacing and attention
  learning stores, then drops scopes whose directory is gone and **reserved
  sinks** (`scope_hosts_user_subsystems`) before initialising. Why: rows record
  what a scope once did, not that it exists, and `initialize` opens an inbox
  DuckDB plus a core-sized scheduler pool. A sink's own checkpoint rows are not
  evidence it needs the pass.
- Background passes enumerate tenants, not every scope. `background_scopes`
  exists twice (resurfacing, distillation), each backing several passes. Both
  reach the mail store, and `scope_inner` caches a scope's DuckDB for the process
  lifetime, so enumerating a sink would keep an inbox open and checkpointing.
- Attention workers use incremental history maintenance, immediate rank backlog
  draining, and schema-constrained semantic backfill; known invalid active
  features get one audited recovery per revision. Semantic repair reconciles
  completed queue receipts against active source envelopes and rechecks coverage
  after leasing. Same-revision, same-producer refreshes preserve valid features
  when the producer returns invalid or missing ones; revision or producer changes
  replace the envelope. See
  [the attention recovery contract](../magician/attention-routing-funnel.md#incremental-history-maintenance-and-worker-recovery).

## Verification-code sources (secure HITL P6)

The Gmail and AgentMail ingest clients are *verification-code sources* for
`magician_v2::verification_codes` (contract:
[verification-code-retrieval.md](../magician/verification-code-retrieval.md)).

- **Gmail's verdict is the receiving server's own.** `gws_client` reads the FIRST
  `Authentication-Results` header only, and only when its authserv-id is
  `mx.google.com`; a second such header causes refusal, because any hop or the
  sender can write one. `dmarc=pass` counts only with explicit `header.from`
  alignment, and the domain check uses the FIRST `From`. ARC sets are never read.
- **A mailbox with no verdict is not a source.** AgentMail offers no sender
  authentication, so its watcher reports itself `unavailable` rather than return
  messages a forgeable `From` would decide.
