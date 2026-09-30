# Channel Assist: Comms Metadata Data Plane + Annotation Store

Channel Assist is the provider-neutral communications plane: metadata sync,
local-or-policy-permitted distillation, thread annotations/follow-ups,
reconciliation, evidence/feedback bridges, and fixture-export tooling. Message
bodies are fetched only for distillation or owner-requested evidence display
and are **not persisted**. Distillation *does* send those in-memory bodies to
the LLM profile bound for `channel_ingest_distill` under the current
`privacy.processing.mode` (see [Locality policy](#locality-policy)).

Public product names are channel-neutral. Some persisted DuckDB tables and
`Mail*` structs remain storage internals. New code imports
`magician_comms::channel_assist` (with aliases from `channel.rs`) and talks
HTTP at `/api/magician/v2/channel-assist/*`. There is no `/mail-assist`
namespace, `magician_v2::mail_assist` module or `MAIL_*` knob.

- Local Ollama pin: [local-channel-llm.md](local-channel-llm.md)
- Proposed structured-decision replacement for land/lane judgments: structured decision plane
- Worth a look: [resurfacing.md](resurfacing.md)
- Observe toggles: [observe-channel-toggles.md](../unified-ui/observe-channel-toggles.md)
- Attention funnel: [attention-routing-funnel.md](attention-routing-funnel.md)
- Work-evidence graph: [work-evidence-graph.md](work-evidence-graph.md)
- Today: [today-feed.md](today-feed.md)
- Attention snapshots: [`data/magician_v2/attention_learning/README.md`](../../../data/magician_v2/attention_learning/README.md)
- Historical design: `docs/archive/plans/2026-07-05-mail-assist-phase1-design.md`

Out of scope: Gmail DOM chips/panels, artifact-versioned Gmail draft
generation/insertion, and any automatic send. Gmail, AgentMail, WhatsApp, Kapso
and Telegram have no Channel Assist send path; confirm-gated iMessage reply is
the only outbound action.

## Layout

The crate is `magician-comms`. HTTP lives in `magician-api`. The locality
guard lives in `magician::magician_v2::llm_dispatch_seam`. Prompt names live
in `magician-core/src/prompts/constants.rs` (re-exported as
`magician_v2::prompts`).

| Path | Role |
| --- | --- |
| `magician-comms/src/channel_assist/types.rs` | Lifecycle enums, sanitizer, follow-up/brief contracts. Record `schema_version` is 9 (`CHANNEL_ASSIST_SCHEMA_VERSION`). |
| `magician-comms/src/channel_assist/store.rs` | Scoped DuckDB (`MailAssistStore`, aliased `ChannelAssistStore`). Tables `mail_threads` / `mail_messages` / `mail_annotations` / `mail_assist_events` plus writing-preference and action-draft tables. Annotation writes are state-transition + audit-append — never delete. On-disk DB schema is **v13**. |
| `magician-comms/src/channel_assist/gws_client.rs` | Thin `gws` CLI wrapper. Parses Gmail METADATA only; never deserializes snippets or attachment ids. |
| `magician-comms/src/channel_assist/sync.rs` | Background tokio loop gated on `CHANNEL_SYNC_ENABLED` and `channel_observe`. |
| `magician-comms/src/channel_assist/adapter_registry.rs` + `adapters/*` | Provider factory. Workers discover ingest, content fetch, deep-link, readiness, and actions here. |
| `magician-comms/src/channel_assist/ingest_*.rs` | Pull ingestors (Gmail, iMessage, WhatsApp, Kapso, Telegram, AgentMail). |
| `magician-comms/src/channel_assist/channel.rs` | Public `Channel*` aliases over historical `Mail*` storage structs. |
| `magician-comms/src/channel_assist/sensitivity.rs` | OTP/2FA/banking suppression before distillation/classification. |
| `magician-comms/src/channel_assist/channel_observe.rs` | Unified consent config. |
| `magician-comms/src/channel_assist/channel_providers.rs` | Transport classification (`kind`, channel name, evidence tier, domains). |
| `magician-comms/src/channel_assist/assist/` | Product lane: classify, distill, content, reconcile, reply-draft, writing preferences, fixtures/evals, quality budgets, completion port. |
| `magician-comms/src/channel_assist/{evidence,feedback}_bridge.rs`, `pattern_synthesis.rs`, `live_content.rs`, `telemetry.rs` | Bridges, live evidence, LLM telemetry. |
| `magician-comms/src/channel_assist/{canonical_attention,attention_learning,resurfacing,memory_api}.rs` | Follow-up/Worth union, learning, Worth-a-look wiring, user-memory HTTP. |
| `magician-api/src/channel_assist_api.rs` | HTTP surface. |

Fresh stores run the packaged schema then record its version; initialized
stores run additive migrations before replaying the current template.
`CREATE TABLE IF NOT EXISTS` cannot add columns that later indexes need.
Persisted shape changes advance `MAIL_ASSIST_DB_SCHEMA_VERSION` even when
DDL uses `IF NOT EXISTS`. Template:
`magician_data_v3/system/db_templates/mail_assist/schema.sql`.

## Product lane vs substrate

Product decisions live in `magician_comms::channel_assist::assist`:

- `assist::classify` — four-label vocabulary, off-vocabulary coercion to
  `fyi`, confidence clamp, 0.7 actionable bar into `needs_approval`
- `assist::reconcile` — owner-send completion, provider-change retirement,
  draft-review staleness, counterparty supersession, stale window, brief
  routing retraction
- `assist::distill` + `assist::content` — brief contract, MIME/quote prep
  (in-memory only), validation
- `assist::reply_draft`, `assist::writing_preferences`
- `assist::fixtures`, `assist::draft_eval`, classifier eval set
- `assist::quality_budgets` — Today projection latency budget (default 250 ms)
- `assist::completion_port::ReconcileCompletionSink` — owner-completion
  reports into attention learning

Substrate stays beside the seam: adapters/ingestors, `gws_client`, registry,
sync, scoped DuckDB (including transaction-bound annotation lifecycle),
`channel_observe`, `channel_providers`, live content, and the
evidence/feedback/pattern/resurfacing workers. Import product modules
through `assist::`.

Tier names (`user.email_evidence`, `user.chat_evidence`,
`user.channel_feedback`, `user.channel_patterns`,
`user.channel_writing_preferences`) are the lib-side contract
`magician_v2::evidence::tier_contracts`.

## Channel adapter boundary

The store is the adapter interface. Pull workers (or a future push webhook)
land provider-neutral thread/message rows keyed
`(provider, account_alias, thread_id)` with an opaque `provider_cursor`
(Gmail: `historyId`). Everything above the store — annotations, classifier,
Today projection — is provider-blind: the classifier consumes a neutral
`ThreadContext`, and channel-specific language lives in prompts only.
Gmail-specific naming stays inside `gws_client.rs` / `adapters/gmail.rs`.

Shipped pull adapters: `gmail`, `whatsapp`, `whatsapp_kapso`, `telegram`,
`agentmail`, `imessage`. Adding a pull provider is one adapter module plus
one `default_channel_adapters()` entry, a `channel_providers` classification,
and `channel_observe` discovery. Deep-link support is optional: Gmail
returns `mail.google.com` with URL-encoded `authuser` when the account email
is known, and fails closed (no URL) rather than opening `/u/0`. Other
providers return no open URL until a reliable native link exists.

`RealtimeChannelAdapter` exists for later realtime ingest. Outbound is
`ChannelActionAdapter` (`compose` / `commit`), not a dormant send trait.
Do not wire mutation to product surfaces except the shipped iMessage reply.

## Privacy contract

Persisted per message: ids (thread/message/provider cursor), labels, subject,
sender name+address, to/cc **domains only**, `internal_date`, direction when
derivable, and derived distill fields (`summary`, `intent`, hints, V2 brief,
revision). Never persisted: bodies, snippets, attachments, full recipient
lists, provider event payloads.

Sensitive-suppressed rows store `[subject suppressed]` with ids retained and
never enter distillation/classification.

Fixture-export rows HASH the thread id (12 hex chars of sha256), reduce the
sender to name+domain, and carry no recipient data — but keep the **real**
subject for labeling. The JSONL file is local-only and must never be
committed.

`sanitize_channel_brief_text` strips full links, email addresses, labeled
secrets/tokens, JWTs, and identifier-like values (last-four) while keeping
action-bearing dates and amounts. The same sanitizer covers summary, every
structured brief string, and follow-up counterparty/due/rationale/detail
fields. Every URL exposed to the client is owned by the source adapter's
deep-link, never by model output.

### Locality policy

Distillation, reply-draft, and `resurfacing_deep_summary` resolve through
`llm_dispatch_seam::resolve_local_provider_for_operation` →
`require_permitted_provider`:

| `privacy.processing.mode` | Binding | Result |
| --- | --- | --- |
| `local` | unbound | OFF. Rows stay `pending`. Router default is never consulted. |
| `local` | Ollama profile | Pin dispatch to that profile. |
| `local` | any other provider | Refuse. Backlog preserved. |
| `cloud` | unbound | OFF. Backlog preserved. |
| `cloud` | any explicit binding | Pin dispatch to that provider (`when_cloud` arm). |

Every `DistillUnavailable` variant preserves the backlog. There is no
bound-but-remote drain to `skipped`. Dispatch is pinned
(`router_profile_override` + `router_required_provider_kind` = the verified
kind). Unbound still means OFF.

The shipped seed selects `privacy.processing.mode: cloud` and binds:

```text
channel_ingest_distill   default: op-channel-distill-local
                         when_cloud: op-channel-distill-remote   # openai gpt-6-luna
channel_classify         default: op-channel-classify-local
                         when_cloud: op-channel-classify-remote
channel_reply_draft      default: op-channel-reply-draft-local
                         when_cloud: op-channel-reply-draft-remote-gpt61sol
channel_pattern_synthesis, resurfacing_curate, resurfacing_deep_summary
                         also carry when_cloud arms
```

Under that seed, **message bodies used for distill/reply-draft/deeper-summary
are sent to the bound cloud profile**. They are still not written to DuckDB.
Switch `privacy.processing.mode` to `local` to keep those calls on Ollama.
The local generation model is the `runtime.ollama.local_generation.selected`
pin (seed `gemma4:12b`, set per RAM tier at install); rebuild notes live in
[local-channel-llm.md](local-channel-llm.md).

Classifier and pattern synthesis are **body-blind** (metadata + already
derived summaries). They are ordinary op dispatch: local or remote is a
config choice. Unbound ⇒ the pass is idle (`pending_classify` on
`sync/status`).

### Managed prompts

Distillation, repair, classification, pattern synthesis, resurfacing
curation, and deeper summaries load versioned JSON exclusively through
`PromptManager`. Names/versions live in `magician-core/src/prompts/constants.rs`.
Worker modules contain no compiled prompt copies. Missing managed prompts
degrade the operation without changing its model contract: distillation and
classification preserve queued work, pattern synthesis skips, resurfacing
uses deterministic curation, deeper summary reports unavailable.

The prompt store resolves packaged `data/magician_v2/prompts` beside the
running executable first; it does not search the caller-controlled working
directory. Prompt diagnostics record variable names and character counts
only, never values or message content.

Current classify prompt version is `1.1.0` (optional
`channel_attention_semantics_v1` block). Distill system/user are `1.1.0` /
`1.1.1`.

## Consent (`channel_observe`)

One durable document `{suppress_sensitive, history_lookback_days, cadence,
channels[]}` in namespace `channel_observe` is the source of truth for both
the sync worker and the calendar digest.

`channels[]` speak in user-facing channels (`email` | `whatsapp` |
`whatsapp_kapso` | `telegram` | `imessage` | `calendar`). `email` maps to
the `gmail` provider. `calendar` is not a message provider.

`load_or_migrate` synthesizes the document once from the retired
`email_observe` / `calendar_observe` / `channel_assist` registry if absent,
then persists it. After that, `GET/PUT /channel-assist/channels` and
`GET /channel-assist/sync/status` read/write `channel_observe` directly
(`message_accounts_all` keeps disabled accounts for the consent UI; PUT
calls `upsert_message_channels`). `/observe/{producer}` PUT
(`upsert_producer_channels`) is calendar-only: a stray message-producer PUT
bails rather than clobbering the Mail & chat card.

History window is user-owned. Allowed values: **1 / 5 / 7 / 14 / 30**
(default **7**). The sync worker uses it as both the provider backfill
window and a hard `min_internal_date` so providers may over-fetch for
cursor safety but cannot append older rows.

`channel_observe::account_lane(provider, alias)` returns `envoy` ("Presto")
iff `(provider, alias)` is in the agent-account set, else `user_assist`
("You"). Shipped defaults: `gmail/presto`, `calendar/presto`,
`whatsapp_kapso/presto`, `telegram/presto`, `agentmail/work`. The
`agent_accounts:` block in `operator-config.yaml` **adds** to that set; it
cannot drop a shipped default. There is no `account_lane(alias)` and no
scattered `alias == "presto"` check.

`/observe` folds "Observe email" into one "Mail & chat" card: one toggle per
account = observed (work evidence) **and** assisted.

## Providers

`channel_providers.rs` is the one place a channel's classification lives,
sourced from `channel_providers:` in `operator-config.yaml` with built-in
defaults. Each entry declares `{kind: email|chat, channel, domains}`. `kind`
drives the distill prompt word, the WEG evidence tier
(`user.email_evidence` / `user.chat_evidence`), and the channel↔provider
name map. **`provider` is the transport, not the domain** — every Google
Workspace domain is the `gmail` transport; `agentmail.to` is a different
one.

| provider | typical alias | lane | ingestor |
| --- | --- | --- | --- |
| `gmail` | business/personal/work | user_assist | `GmailIngestor` (`gws`) |
| `gmail` | presto | envoy | same |
| `whatsapp` | self | user_assist | local baileys `wu.db` |
| `whatsapp_kapso` | presto | envoy | Kapso CLI |
| `telegram` | presto | envoy | `origin_channel=telegram` chat sessions |
| `agentmail` | work | envoy | AgentMail CLI |
| `imessage` | (owner) | user_assist | macOS `chat.db` |

Kapso eligibility includes an executable preflight against the scoped
subprocess `PATH`. Valid credentials with a missing CLI are reported as
`unavailable` in `last_error` without spawning or advancing the cursor.
`@kapso/cli` is installed once under `skillshub/node_modules/.bin`; each
invocation still gets the active workspace's credentials, cwd, and isolated
`HOME`. Env: `KAPSO_API_KEY`, `KAPSO_PHONE_NUMBER_ID`, `KAPSO_BIN`. The
public Kapso bot receiver additionally requires `KAPSO_WEBHOOK_SECRET` and
verifies HMAC over the raw POST body.

AgentMail shells `agentmail --format json inboxes:messages list|get`
(inbox-scoped `AGENT_MAIL_KEY`). Direction is outbound iff `from` is the
inbox. Env: `AGENTMAIL_BIN`, `AGENTMAIL_ACCOUNT_<SLUG>_EMAIL` /
`AGENT_MAIL_KEY_<SLUG>`.

iMessage reads `$HOME/Library/Messages/chat.db` read-only (`rusqlite`;
`MAGICIAN_IMESSAGE_DB_PATH` override). It needs Full Disk Access on the
Magician process. ROWID watermark, Apple-epoch→ms dates, `is_from_me` →
direction, shared sensitivity suppression. A content fetcher re-reads
message text for distillation. The adapter also ships a confirm-gated
`reply` action: `compose` = `channel_reply_draft`, `commit` = AppleScript
`imessage_send`. Drafts persist in `channel_action_drafts` (DB schema v12).

`/channel-assist/channels` projects `provider_display`, `channel`,
`channel_label`, `capabilities`, and readiness-backed `connected`.
`/channel-assist/sync/status` uses a provider-aware connected value: Gmail
means local profile readiness; non-Gmail means enabled in the resolved
config.

WhatsApp display names resolve from chat/contact/group tables with
`+<number>` fallback. Message rows persist newest-first before they enter
the distill queue.

## Sync

`ChannelSyncWorker` (`CHANNEL_SYNC_ENABLED` default on,
`CHANNEL_SYNC_INTERVAL_SECS` default 900, startup delay 90,
`CHANNEL_SYNC_MAX_THREADS` default 500) runs the same function as
`POST /channel-assist/sync/run`. Enabled = ≥1 enabled message-channel
account in `channel_observe`. Startup history is the scoped Observe
catch-up policy, then incremental via `history.list` with watermark
re-list fallback. Per-account errors surface on the watermark row.

Gmail incremental passes consume all message/label change kinds before
watermark advancement (ten-page safety budget; continuation stored in the
opaque cursor). The backfill thread cap cannot discard a consumed history
delta. Every incremental pass resolves `users.getProfile.emailAddress`
before collection; missing email fails the account pass instead of
producing misrouted links. Identity is stamped on every new row, preserved
on upsert, and reconciled for legacy NULL/stale identities even when the
history page is empty.

## Distillation

The configured Observe history window also bounds queued message work. At each
scope tick, pending and retryable failed rows older than the rolling cutoff become
`expired`. Retirement preserves message metadata, safe summaries, revisions and
attempt counters; re-syncing the same message does not enqueue it again. Completed,
suppressed and retry-exhausted rows are left as they were. Increasing the history
window does not automatically replay already expired work.

Retirement selects at most 256 identities through a reader, then takes the writer
only for one conditional update. State and age are rechecked under that writer;
concurrently completed/suppressed results win. Empty passes never take the writer.
The worker yields between batches and caps a tick at 32 batches before continuing
ordinary scope work. More history can retire on subsequent ticks. No content fetch
or model call is needed, including when the model is unavailable. Startup replay
budgets do not expire otherwise in-range messages.

Queue selection applies the cutoff in SQL before pagination. The backfill gate
checks pending/retryable work within the current processing boundary, so excluded
history cannot indefinitely block contract repair.

`sync/status.distill_queue` and `stats.distill.queue` report eligible `pending`,
`retryable`, `outside_history` awaiting retirement, `expired`, `retry_exhausted`,
and `history_floor_ms`. Compatibility pending counters count pending work
within the history window; `by_state` remains the complete retained histogram.
Eligible means within the window, not that a model is currently running: normal
startup, worker-enable and provider gates still apply. `/observe/stats` labels
waiting work as queued, shows history retirement separately, and derives throughput
from summaries and mechanical skips/coalescing rather than shrinking queue depth.
Gate: `make test-distill-history`.

Sync lands metadata rows with `distill_state = pending`. `ChannelDistillWorker`
drains them: fetch content per adapter into process memory, distill, persist
derived fields, discard the text. Sensitive rows never enter the queue
(`suppressed`). Failures increment `distill_attempts` and eventually
terminal-`skipped` at `MAX_DISTILL_ATTEMPTS` (3). The locality guard above
is the availability gate; `CHANNEL_DISTILL_ENABLED=0` stops the worker
entirely.

Cadence (env): interval 60s, startup delay 120s, batch 8, concurrency 1,
thread coalescing on, first-chunk `CHANNEL_DISTILL_CHUNK_CHARS` 10000,
`CHANNEL_DISTILL_MAX_CHUNKS` 8. Coalescing groups same-thread rows in one
batch into one newest-first prompt, writes the result on the newest row,
and marks older included rows metadata-only (`skipped`). The newest row
also stores `distill_evidence_message_ids_json`.

Structured config (`channel_assist.distillation` in `magician-config.yaml`):

```yaml
channel_assist:
  distillation:
    brief_contract_version: 2   # 1 | 2; other values normalize to 2
    summary_max_chars: 900
    backfill:
      enabled: true
      lookback_days: 30
      batch_size: 1             # worker clamp 1..=64; code default 2
      surfaced_first: true
```

`brief_contract_version: 2` extends the existing `channel_ingest_distill`
call; it does not add another automatic LLM operation. Compatibility
summary/intent/follow-up fields remain, plus a provider-neutral safe brief
(facts, before/after changes, temporal text, stated action, detail
completeness, explicit source-omitted details). Each v1/v2 managed prompt
is paired with a provider-native JSON Schema sent as Ollama `format` (and
as request `JsonObject`/`JsonSchema` on non-Ollama arms). Server-side
semantic and privacy validation still runs afterward; one repair retry
uses the same schema. Parse diagnostics expose field/type errors only.

Successful writes atomically persist compatibility fields,
`distill_brief_json`, contract version, completion time, evidence-message
ids, and a scope-local monotonic `distill_revision`. Re-distilling an old
message receives a new revision independent of provider time. Consumers
advance on `distill_revision`, not `internal_date`. `occurred_at` remains
the provider timestamp for recency scoring.

Historical V2 repair drains new pending/retryable work first, then selects
at most the configured backfill batch (surfaced comms first, newest-first,
concurrency one) and yields when the shared LLM dispatch queue has work.
Failures do not replace a prior safe result. `POST /distill/backfill` with
`?dry_run=true` inspects; `?paused=true|false` pauses/resumes; otherwise
queues one bounded pass. Pause state and counters survive restart.

`GET /distill/recent` is an in-memory ring (`RecentDistillEntry`, cap 50)
of recently distilled messages: metadata in, derived summary/intent out,
never bodies, not persisted, resets on restart.

## Classifier

`assist::classify` turns distilled threads into `MailThreadAnnotation`s
from a body-blind `ThreadContext`. Labels: `needs_reply` | `follow_up` |
`fyi` | `no_action`. Off-vocabulary model labels coerce to `fyi`
(confidence capped at 0.4). Actionable (`needs_reply`/`follow_up`) at
confidence ≥ 0.7 → `needs_approval`; else `classified`.

`store.list_threads_to_classify` returns each thread's newest distilled
non-suppressed message whose exact distill revision has not been
classified. Each annotation records `classification_input_revision`; a
late result is rejected rather than overwriting newer evidence.
`ChannelClassifyWorker`: `CHANNEL_CLASSIFY_ENABLED` (default on), interval
120s, startup delay 180s, batch 8, concurrency 1. Failures increment
`classify_attempts`, back off via `classify_next_retry_at`, and fall out
after the retry cap with `classify_failed_at`.

Before routing, the worker passes bounded recent acknowledged/dismissed
follow-ups from the same provider/account/thread. The model may set
`repeat_of_recently_handled=true` only when the new item is substantially
the same; the router then drops it with `cooldown_active` (14-day window).
Matching rows are still annotated. Deterministic required-action evidence
wins over a disagreeing classifier label.

A `stated_action` that is a sentinel ("None", "no action needed", …)
anchored at the start is treated as absent. `promotion` and `event` briefs
do not promote a stated action to owner work (marketing). `deadline`,
`transaction`, `request`, `scheduling`, and `change_notice` are unaffected.

Before an actionable comm is withheld from Worth a look, the resurfacing
curator atomically creates or refreshes one provisional Follow-up
annotation. Passive `classified` annotations reclassify in place; unclaimed
`needs_approval` refreshes in place; approved/in-progress/completed update
evidence only; dismissed is preserved. Active action claims are never
overwritten.

Prompt category is `ChannelAssist`. A bad category, missing metadata,
unknown version, or missing asset fails loading and leaves work retryable.

## Follow-ups

`store.list_needs_approval` joins the latest still-active `needs_approval`
annotation per thread. A newer quiet `classified`/`fyi` does not hide an
older unresolved card; reconciliation must transition state explicitly.

`GET /channel-assist/follow-ups` (alias `GET /needs-you`) is cursor-paged
(`limit` 1..=100, default 50) and returns `{items, total, limit, cursor,
next_cursor, has_more, latency}`. Cards carry annotation identity, lane,
label, confidence, reason, summary, `proposed_action`, subject, sender,
`received_at` (evidence `internal_date`, not classifier `created_at`),
`account_email`, `open_url`. Projection work is timed against
`CHANNEL_TODAY_PROJECTION_LATENCY_BUDGET_MS` (default 250); over-budget
responses are still served with `Server-Timing` and
`X-Magician-Latency-Budget`.

State-changing endpoints use a transactional expected-state transition;
stale cards return 409 and do not write misleading feedback.

| Action | Effect |
| --- | --- |
| `POST …/approve` | "Do it": `approved` + `helpful` feedback + one-shot `executive-assistant` task (`channel-assist` tag). Optional `{hint}`. Claim-before-create; uncompleted claims older than 10 minutes are stale. Only from `needs_approval`. |
| `POST …/useful` | Positive, no task: `acknowledged` + `helpful` feedback. |
| `POST …/acknowledge` | Neutral "seen": `acknowledged`, **no** learning feedback. |
| `POST …/dismiss?reason=` | Negative: `dismissed` + `not_helpful`. Reasons distinguish irrelevant / not-actionable / obsolete / not-owner / duplicate. |
| `POST …/snooze` | Drop the card (`classified`); no verdict. |
| `POST …/review` | Re-open a stale/retracted card. |

Approve/useful/dismiss/acknowledge action responses add an optional
`feedback_receipt`. Follow-up reads add health, embedding coverage, and
baseline/learned rank metadata. In observe mode the established
cursor/order is unchanged. Enforced semantic ranking loads and ranks the
complete active eligible universe before slicing it and uses a
generation/universe-bound learned cursor; it never deletes, transitions, or
hides an annotation.

Follow-up owns the hard cross-lane identity for an exact active
communication thread: typed equality of
`(provider, account_alias, thread_id)` — never text, subject, sender,
embedding, similarity, or an LLM guess. Matching Worth communication rows
remain durable origin aliases but do not enter ranking, grouping, routing,
materialization, or pagination. When the Follow-up becomes inactive, the
active Worth source reappears without copying or deleting source truth.
Malformed Worth communication refs fail closed. Opaque public alias
summaries are capped at 100 and contain no raw
provider/account/thread/message/source-ref values.

UI: `/today` Follow-ups tab and `/attention` Mail source share
`ChannelFollowUpActions.svelte` + `channelNeedsYouStore.ts`. Fetch failures
are explicit errors and preserve last-loaded rows.

## Reconciliation

`assist::reconcile` is adapter-blind. `ChannelReconcileWorker` scans active
annotations (`needs_approval`, `approved`, scheduled/draft, `sent_detected`)
that have newer distilled non-sensitive messages, a provider-side history
change, or are past the stale cutoff.

Env: `CHANNEL_RECONCILE_ENABLED`, interval 60s, startup delay 180s, batch
100, newer-message cap 20, stale days 30 (`0` disables age-only retirement).

Rules:

- Owner-owed / `needs_reply` + newer outbound → `completed`
  (`owner_or_agent_sent_after_evidence`). Direction/identity reconcile
  immediately; summaries refine when available. Completions report through
  `ReconcileCompletionSink`.
- Waiting-on + explicitly non-actionable inbound → `completed`.
- Ambiguous newer evidence → `superseded` only after that evidence already
  has a classifier annotation.
- Quiet past the stale window → `stale`.
- Gmail `labelsAdded` (`TRASH`/`SPAM`) and `labelsRemoved` (`INBOX`,
  archived) close active work. `messagesDeleted` closes only when a
  metadata read confirms the whole thread is gone. Later unarchive/restore
  in the same history stream wins over a transient delta.
- Inbound after `draft_requested` / `draft_ready` / `inserted` → stale with
  `newer_message_requires_draft_review`.

Writes use `transition_annotation_if_state` with `actor=worker`. Active
action claims block reconciliation; user transitions win stale races;
terminal states are never reopened. Reconciliation never creates
annotations or tasks.

`derive_channel_required_action` is deterministic over a persisted brief.
When those rules change, brief-routed annotations
(`required_action_source = information_brief`) whose action is no longer
derived transition to `stale` with `required_action_no_longer_derived`
(`routing_retracted`). Evidence-based reconciliation is decided first. The
repair uses `list_active_brief_routed_annotations` because the ordinary
scan would miss quiet threads.

`/channel-assist/sync/status` includes `workers.reconcile` pass counters.

## Bridges and memory

`ChannelEvidenceBridgeWorker` (`CHANNEL_EVIDENCE_BRIDGE_ENABLED`, interval
120s, startup 150s) reads distilled non-suppressed rows past a per-scope
`__bridge_distill_revision_v2__` watermark, groups by `(channel, account,
day)`, and writes digest-shaped rows into `user.email_evidence` /
`user.chat_evidence`. This is the sole email→WEG writer:
`producer_has_digest("email")` is false; calendar keeps its digest.
Malformed follow-up/brief JSON is downgraded per row and counted; the
revision still crosses the bridge.

`ChannelFeedbackBridgeWorker` (`CHANNEL_FEEDBACK_BRIDGE_ENABLED`) merges
typed `MailAssistUserFeedback` events per `(account, day)` into
`user.channel_feedback` (`did` / `acknowledged` / `dismissed` counts,
`dismiss_reasons` histogram, bounded notes). Both bridges use composite
cursor tie-breakers and keep the watermark unchanged on tier-write
failure.

Before routing a model-only actionable recommendation, the classifier
aggregates the last 90 days of `helpful` / `not_helpful` for the current
sender and sender domain. Three or more net-negative dismissals activate
the shared attention-router cooldown. Deterministic required-action hints
bypass it.

`GET/POST /channel-assist/annotations/{id}/writing-preferences` lists or
learns sender/domain statements. A before/after draft edit reduces to
style-only statements; raw drafts are never stored. `POST
/writing-preferences/{id}/promote` mirrors the statement to
`user.channel_writing_preferences`; `/dismiss` retracts it. Nested
user-memory tiers merge by stable key.

`ChannelPatternSynthesisWorker` (`CHANNEL_PATTERN_SYNTHESIS_ENABLED`,
daily) folds the recent distilled corpus into `user.channel_patterns`.
Body-blind; unbound ⇒ idle.

## Live evidence and Worth a look

`live_content::ChannelEvidenceResolver` is the shared metadata/body
boundary. It resolves the exact scoped
provider/account/thread/message/timestamp tuple, reloads the coalesced
`distill_evidence_message_ids_json` batch, and computes `has_newer` without
substituting the thread's latest message. Metadata/detail resolution never
invokes a content fetcher. Only an explicit original request calls the
adapter, with per-message (`ORIGINAL_BODY_MAX_CHARS` 20_000), aggregate
(`ORIGINAL_RESPONSE_MAX_CHARS` 40_000), and evidence-count
(`ORIGINAL_EVIDENCE_MAX_MESSAGES` 8) bounds. Sensitive suppression is
checked across the message, its thread, and every coalesced evidence row
before any fetch. Returned bodies are request-local.

`GET /annotations/{id}/message` returns stored summary + subject +
`has_newer` plus the coalesced `evidence_messages[]` batch, with live
bodies fetched without persisting.

Worth-a-look contextual actions consume only the persisted safe brief and
server-resolved source identity (task, reminder, share, memory, chat
handoff). They never copy a live body into a task or claim. Each request
carries a UUID idempotency key and the card content revision.
`summarize_deeper` is the sole action that reads live content; it is
hidden/refused unless `resurfacing_deep_summary` is bound to a profile the
current processing mode permits. See [resurfacing.md](resurfacing.md).

## Attention learning

Follow-up legacy annotation actions share the canonical asynchronous
rank-recompute receipt with delivery-native and Worth-a-look feedback. An
accepted action returns immediately with a pending job, attributed served
before-rank, null current after-rank/delta, and a scoped status URL; it
never runs the model in the action or list/page request. A worker reconstructs
a missing served decision from the decision ledger, computes the after rank
from that served projection when present, and publishes only when source
revision, universe digest/generation, posterior, and snapshot CAS bindings still
match. A missing decision retries rather than immediately staling. Enqueue/worker
failure does not undo feedback. Shipped `attention_learning.rank_recompute.enabled` is **true**;
pause reason `Disabled` is reported only when that flag is off.

Slice 2 (`attention_learning.actionability.mode`: `disabled` | `shadow` |
`enforced`) starts **disabled** and requires an immutable `snapshot_id`
outside disabled. Compatible snapshots use L2-logistic + Platt calibration.
Missing/stale/invalid inputs fall back to Slice 1. Shadow exposes
calibrated probability while preserving order. The offline trainer
(`attention_learning::training`, same library as CLI and in-process) joins
explicit `attention_outcomes` to the served feature vector, including
reconcile completions via the last served binding. It refuses to write when
labels, holdout AUC, or calibration fail the gates.

```bash
MAGICIAN_SKIP_KEYCHAIN=1 magician attention-learning train-actionability \
  --principal anonymous --workspace default \
  --split temporal --out /tmp/actionability-snapshot.json
```

`--install` stores a passing artifact and forces the first scope install to
`shadow`. `magician attention-learning install-actionability-snapshot
--snapshot <artifact.json>` validates and immutably installs with no
activation flag. Status/run:
`GET/POST /channel-assist/attention-learning/actionability-training/{status,run}`.
`train-routing` trains from `not_actionable` and `action_completed` lane
labels. Live serving applies Slice-1 kNN demotions/promotions without waiting
for that snapshot. The first auto-install of a passing routing snapshot stays
shadow. Slice-5 personal ranking auto-trains a prior after those snapshots
exist, stays shadow (no reorder) until 40 attributed posterior updates, then
promotes to canary on the first page of each lane. Shipped YAML is
`bandit.mode: shadow` with no snapshot pin; Magician's scope install is the
serving switch.

Slice 3 grouping (`attention_learning.grouping.mode`) also starts
disabled and requires an immutable pair-model `snapshot_id`. Follow-up and
Worth-a-look reads compute grouping over the full eligible universe before
pagination. Only effective enforced mode collapses default cards to
representatives. `POST /channel-assist/attention-learning/pair-corrections`
accepts `same_underlying_item` | `same_obligation` | `not_duplicate`.
`max_pair_evaluations` (shipped 2_500_000) is checked against `n*(n-1)/2`
before inference; over-budget projections return singletons.

User-knowledge preferences can be confirmed
(`POST /memory/entries/{tier}/{key}/confirm` on `preferences` and
`research_findings` only) and scoped
(`PATCH /memory/entries/{tier}/{key}/scope`). Confirm does not invent a
scope. Memory effects are judged in shadow: the card stays visible.

Classifier prompt v1.1.0 may emit `channel_attention_semantics_v1`. Missing
or malformed semantics persist as `missing`/`invalid` and never change the
legacy route. Coverage repair leases rows into configured concurrency
slots without putting an LLM on a Follow-up list request. Shipped
`semantic_backfill.enabled` is true.

## Store

Reads share one DuckDB instance with the writer: `read_connection()` clones
a short-lived connection off a `read_conn` template on the same instance as
`write_conn`. A throttled 30s `CHECKPOINT` is post-commit compaction, not
mutation durability. Governed compaction blocks new reader clones during
publication and rebuilds `read_conn`. External live file replacement is
unsupported — an open instance keeps serving from memory until restart.
Cloned read connections are read-write capable; writes must still go
through `write_conn` under `acquire_write_guard`.

Annotation writes materialize and index `attention_lane` (DB v11). The
Today query ranks only actionable rows per provider/account/thread.

## APIs (scope-aware workspace-bound bearer)

Prefix: `/api/magician/v2/channel-assist`.

```text
GET    /sync/status
POST   /sync/run
GET    /stats
GET    /distill/recent
POST   /distill/backfill                 ?dry_run= | ?paused=
GET    /channels
PUT    /channels
GET    /annotations                      ?provider=&account=&thread_ids=  (cap 100)
POST   /annotations/seed
POST   /annotations/{id}/dismiss
POST   /annotations/{id}/acknowledge     # neutral
POST   /annotations/{id}/useful          # positive, no task
POST   /annotations/{id}/approve         # Do it + task
POST   /annotations/{id}/snooze
POST   /annotations/{id}/review
POST   /annotations/{id}/feedback
GET    /annotations/{id}/message
GET    /annotations/{id}/writing-preferences
POST   /annotations/{id}/writing-preferences
POST   /writing-preferences/{id}/promote
POST   /writing-preferences/{id}/dismiss
POST   /annotations/{id}/action/{action_id}/compose
POST   /annotations/{id}/action/{action_id}/commit
GET    /follow-ups                       # needs-you alias
GET    /follow-ups/groups/{cluster_id}/members
POST   /follow-ups/{annotation_id}/actions
GET    /resurfacing/today
GET    /resurfacing/{id}/detail          # never body
GET    /resurfacing/{id}/original        # explicit live body
POST   /resurfacing/{id}/actions
GET    /resurfacing/stats
GET    /resurfacing/observability
POST   /attention-learning/pair-corrections
GET    /attention-learning/rank-recompute/status
GET    /attention-learning/rank-recompute/jobs/{job_id}
GET/POST /attention-learning/actionability-training/{status,run}
```

Also mounted on this scope: attention impressions/decisions, canonical
projection/delivery, delivery-health, semantic-extraction enqueue/status,
historical-bootstrap status.

User-memory confirm/scope live at `/memory/entries/{tier}/{key}/confirm`
and `…/scope` (see `memory_api.rs`).

## CLI

```bash
MAGICIAN_SKIP_KEYCHAIN=1 magician channel-assist export-fixtures \
  --limit 200 [--account <alias>] [--out <path>] \
  [--principal anonymous] [--workspace default]
```

Default output: `<storage root>/channel_assist_fixtures/fixtures-<date>.jsonl`.

```bash
magician channel-assist classify-eval --fixtures <jsonl>
magician channel-assist draft-eval \
  --fixtures magician/tests/fixtures/channel_draft_usefulness_eval.jsonl
```

Committed classifier baseline:
`magician/tests/fixtures/channel_classifier_eval.jsonl` (≥4 safe cases per
label). Production mailbox exports stay outside git. Draft-eval checks
required facts, forbidden/hallucinated phrases, privacy-leak sentinels,
length, and exact writing preferences; unsupported preference rules fail
closed.

## Env knobs

| Knob | Default |
| --- | --- |
| `CHANNEL_SYNC_ENABLED` | on |
| `CHANNEL_SYNC_INTERVAL_SECS` | 900 |
| `CHANNEL_SYNC_STARTUP_DELAY_SECS` | 90 |
| `CHANNEL_SYNC_MAX_THREADS` | 500 |
| `CHANNEL_DISTILL_ENABLED` | on |
| `CHANNEL_DISTILL_INTERVAL_SECS` | 60 |
| `CHANNEL_DISTILL_STARTUP_DELAY_SECS` | 120 |
| `CHANNEL_DISTILL_BATCH` | 8 |
| `CHANNEL_DISTILL_CONCURRENCY` | 1 |
| `CHANNEL_DISTILL_COALESCE_THREADS` | on |
| `CHANNEL_DISTILL_CHUNK_CHARS` | 10000 |
| `CHANNEL_DISTILL_MAX_CHUNKS` | 8 |
| `CHANNEL_CLASSIFY_ENABLED` | on |
| `CHANNEL_CLASSIFY_INTERVAL_SECS` | 120 |
| `CHANNEL_CLASSIFY_STARTUP_DELAY_SECS` | 180 |
| `CHANNEL_CLASSIFY_BATCH` | 8 |
| `CHANNEL_CLASSIFY_CONCURRENCY` | 1 |
| `CHANNEL_RECONCILE_ENABLED` | on |
| `CHANNEL_RECONCILE_INTERVAL_SECS` | 60 |
| `CHANNEL_RECONCILE_STARTUP_DELAY_SECS` | 180 |
| `CHANNEL_RECONCILE_BATCH` | 100 |
| `CHANNEL_RECONCILE_NEWER_MESSAGE_LIMIT` | 20 |
| `CHANNEL_RECONCILE_STALE_DAYS` | 30 (`0` = off) |
| `CHANNEL_TODAY_PROJECTION_LATENCY_BUDGET_MS` | 250 |
| `CHANNEL_EVIDENCE_BRIDGE_ENABLED` | on |
| `CHANNEL_FEEDBACK_BRIDGE_ENABLED` | on |
| `CHANNEL_PATTERN_SYNTHESIS_ENABLED` | on |

Startup history age is the scoped Observe catch-up choice, not an env knob.

## Observability

`GET /channel-assist/stats` aggregates message/thread totals (`by_provider`,
`by_lane`), distill-state histogram (done / pending / suppressed /
skipped), annotation state/label histograms, a
synced→distilled→classified→needs_approval funnel, classifier retry/fail
counts, V2/legacy coverage, backfill pause/backlog, and a live `ops` block
`{distill, classify}` resolved from the operation router
(`explicit_binding_for_operation` + `get_config_for_operation`). Store read
failures return 500, not healthy zeroes.

`/observe/stats` (`channelStatsStore.ts`, 15s poll) renders that aggregate
plus a live `/distill/recent` feed (4s) and per-op cost from
`/analytics/llm_calls/query`. Distill and classify bypass the agentic
executor, so `channel_assist/telemetry.rs` (`emit_mail_llm_call`) emits
`LLMResponseReceived` after each router call.

`magicutor/extension/gmail_mail_assist_latency.js` wraps Gmail chip render
work with a 16.7 ms one-frame budget (the Gmail DOM adapter is not enabled).
