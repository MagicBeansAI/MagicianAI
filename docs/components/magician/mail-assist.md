# Channel Assist: Comms Metadata Data Plane + Annotation Store

Channel Assist is the provider-neutral comms plane: metadata sync,
distillation, annotations and follow-ups, reconciliation, bridges. Bodies are fetched only for distillation or
an owner-requested evidence view and are **never persisted**, but distillation
*does* send them to the profile bound for `channel_ingest_distill` under
`privacy.processing.mode` (see [Locality policy](#locality-policy)).

Public names are channel-neutral; some DuckDB tables and `Mail*` structs are
storage internals. Code imports `magician_comms::channel_assist` (aliases in
`channel.rs`); HTTP is `/api/magician/v2/channel-assist/*`. There is no
`/mail-assist` namespace, `magician_v2::mail_assist` module or `MAIL_*` knob.

Related: [local-channel-llm.md](local-channel-llm.md),
structured decision plane
(proposed land/lane replacement), [resurfacing.md](resurfacing.md),
[observe-channel-toggles.md](../unified-ui/observe-channel-toggles.md),
[attention-routing-funnel.md](attention-routing-funnel.md),
[work-evidence-graph.md](work-evidence-graph.md), [today-feed.md](today-feed.md),
[`data/magician_v2/attention_learning/README.md`](../../../data/magician_v2/attention_learning/README.md),
`docs/archive/plans/2026-07-05-mail-assist-phase1-design.md`.

Out of scope: Gmail DOM chips/panels, artifact-versioned Gmail drafts, any
automatic send. The only outbound action is a confirm-gated iMessage reply
(no Gmail, AgentMail, WhatsApp, Kapso or Telegram send).

## Layout

Crate `magician-comms`; HTTP in `magician-api`; locality guard
`magician::magician_v2::llm_dispatch_seam`; prompt names in
`magician-core/src/prompts/constants.rs` (re-exported as `magician_v2::prompts`).

| Path | Role |
| --- | --- |
| `magician-comms/src/channel_assist/types.rs` | Lifecycle enums, sanitizer, follow-up/brief contracts. Record `schema_version` 10 (`CHANNEL_ASSIST_SCHEMA_VERSION`). |
| `magician-comms/src/channel_assist/store.rs` | Scoped DuckDB (`MailAssistStore` = `ChannelAssistStore`): `mail_threads` / `mail_messages` / `mail_annotations` / `mail_assist_events` + preference/draft tables. Annotations are append-audited, never deleted. Schema **v14**. |
| `magician-comms/src/channel_assist/gws_client.rs` | `gws` CLI wrapper; Gmail METADATA only, never snippets or attachment ids. |
| `magician-comms/src/channel_assist/sync.rs` | Sync loop gated on `CHANNEL_SYNC_ENABLED` and `channel_observe`. |
| `magician-comms/src/channel_assist/adapter_registry.rs` + `adapters/*` | Provider factory: ingest, content fetch, deep link, readiness, actions. |
| `magician-comms/src/channel_assist/ingest_*.rs` | Pull ingestors (Gmail, iMessage, WhatsApp, Kapso, Telegram, AgentMail). |
| `magician-comms/src/channel_assist/channel.rs` | Public `Channel*` aliases over `Mail*` structs. |
| `magician-comms/src/channel_assist/sensitivity.rs` | OTP/2FA/banking suppression before distill/classify. |
| `magician-comms/src/channel_assist/channel_observe.rs` | Unified consent config. |
| `magician-comms/src/channel_assist/channel_providers.rs` | Transport classification (`kind`, channel, tier, domains). |
| `magician-comms/src/channel_assist/assist/` | Product lane (below). |
| `magician-comms/src/channel_assist/{evidence,feedback}_bridge.rs`, `pattern_synthesis.rs`, `live_content.rs`, `telemetry.rs` | Bridges, live evidence, LLM telemetry. |
| `magician-comms/src/channel_assist/{canonical_attention,attention_learning,resurfacing,memory_api}.rs` | Follow-up/Worth union, learning, Worth a look, user-memory HTTP. |
| `magician-api/src/channel_assist_api.rs` | HTTP surface. |

Template: `magician_data_v3/system/db_templates/mail_assist/schema.sql`
(fresh stores run it and record the version; existing stores migrate additively
first, then replay it). `CREATE TABLE IF NOT EXISTS` cannot
add columns, so every shape change bumps `MAIL_ASSIST_DB_SCHEMA_VERSION`.

## Product lane vs substrate

Product decisions live in `magician_comms::channel_assist::assist` (import via
`assist::`): `assist::classify` (labels, 0.7 bar), `assist::reconcile`,
`assist::distill` + `assist::content` (brief contract, in-memory MIME/quote
prep), `assist::reply_draft`, `assist::writing_preferences`,
`assist::fixtures`, `assist::draft_eval`, `assist::quality_budgets` (Today
projection budget, 250 ms) and
`assist::completion_port::ReconcileCompletionSink` (owner completions into
attention learning). Everything else is substrate.

Tier names (`user.email_evidence`, `user.chat_evidence`,
`user.channel_feedback`, `user.channel_patterns`,
`user.channel_writing_preferences`) are the contract in
`magician_v2::evidence::tier_contracts`.

## Channel adapter boundary

The store is the adapter interface: rows keyed
`(provider, account_alias, thread_id)` with an opaque `provider_cursor` (Gmail:
`historyId`). Above it everything is provider-blind (neutral `ThreadContext`;
channel wording only in prompts; Gmail naming only in `gws_client.rs` /
`adapters/gmail.rs`).

Shipped pull adapters: `gmail`, `whatsapp`, `whatsapp_kapso`, `telegram`,
`agentmail`, `imessage`. A new provider needs an adapter module, a
`default_channel_adapters()` entry, a `channel_providers` classification and
`channel_observe` discovery. Only Gmail deep-links: `mail.google.com` with
URL-encoded `authuser` when the account email is known, else no URL (never
`/u/0`). `RealtimeChannelAdapter` is reserved for realtime ingest; outbound is
`ChannelActionAdapter` (`compose` / `commit`), wired only to the iMessage reply.

## Privacy contract

Persisted per message: ids, labels, subject, sender name and address, to/cc
**domains only**, `internal_date`, direction (when derivable), derived fields (`summary`,
`intent`, hints, V2 brief, revision). Never persisted: bodies, snippets, attachments, full recipient
lists, provider event payloads. Sensitive rows store `[subject suppressed]`,
keep ids, and never reach distill or classify.

Fixture export hashes the thread id (12 hex of sha256), reduces the sender to
name + domain, drops recipients, but keeps the **real** subject; the JSONL is
local-only and never committed.

`sanitize_channel_brief_text` strips links, emails, labeled secrets/tokens,
JWTs and last-four identifiers (keeping dates and amounts) from the summary,
brief and follow-up fields.
Every client-visible URL comes from the adapter's deep link, never the model.

### Locality policy

Distillation, reply-draft and `resurfacing_deep_summary` resolve through
`llm_dispatch_seam::resolve_local_provider_for_operation` →
`require_permitted_provider`:

| `privacy.processing.mode` | Binding | Result |
| --- | --- | --- |
| `local` | unbound | OFF; rows stay `pending`, router default never consulted. |
| `local` | Ollama profile | Pinned to that profile. |
| `local` | other provider | Refused; backlog kept. |
| `cloud` | unbound | OFF; backlog kept. |
| `cloud` | explicit binding | Pinned to it (`when_cloud` arm). |

Every `DistillUnavailable` keeps the backlog (no bound-but-remote drain to
`skipped`); dispatch is pinned via
`router_profile_override` + `router_required_provider_kind`. The seed sets
`privacy.processing.mode: cloud` and binds:

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

So by default **bodies for distill, reply-draft and deeper summary go to the
cloud profile** (still never written to DuckDB); set `local` to keep them on
Ollama, using the `runtime.ollama.local_generation.selected` pin (seed
`gemma4:12b`, set per RAM tier at install; see [local-channel-llm.md](local-channel-llm.md)). Classifier and
pattern synthesis are **body-blind** and use ordinary op dispatch; unbound
means idle (`pending_classify` on `sync/status`).

### Managed prompts

All channel prompts (incl. repair) load versioned JSON only through
`PromptManager`, resolving packaged `data/magician_v2/prompts` beside the
executable first (never the caller's cwd); no compiled copies. A missing prompt degrades: distill/classify keep queued work,
patterns skip, curation goes deterministic, deeper summary reports
unavailable. Diagnostics log variable names and lengths only, never values or content. Classify is
`1.1.0` (optional `channel_attention_semantics_v1`); distill system/user
`1.1.0` / `1.1.1`.

## Consent (`channel_observe`)

One document `{suppress_sensitive, history_lookback_days, cadence,
channels[]}` in namespace `channel_observe` drives sync and the calendar
digest. `channels[]`: `email` (= `gmail`) | `whatsapp` | `whatsapp_kapso` |
`telegram` | `imessage` | `calendar` (not a message provider).
`load_or_migrate` seeds it once from the retired `email_observe` /
`calendar_observe` / `channel_assist` registry. `GET/PUT /channel-assist/channels`
(PUT → `upsert_message_channels`; `message_accounts_all` keeps disabled
accounts) and `GET /channel-assist/sync/status` read it.
`/observe/{producer}` PUT (`upsert_producer_channels`) is calendar-only; a
message-producer PUT fails rather than clobber the Mail & chat card, where one
toggle per account means observed **and** assisted.

History window: **1 / 5 / 7 / 14 / 30** days (default **7**), both the backfill
window and a hard `min_internal_date` (providers may over-fetch, never append
older rows).

`channel_observe::account_lane(provider, alias)` returns `envoy` ("Presto") for
agent accounts, else `user_assist` ("You"). Shipped: `gmail/presto`,
`calendar/presto`, `whatsapp_kapso/presto`, `telegram/presto`,
`agentmail/work`; `agent_accounts:` in `operator-config.yaml` **adds** only.
There is no `account_lane(alias)` or `alias == "presto"` check.

## Providers

`channel_providers.rs` owns classification (`channel_providers:` in
`operator-config.yaml` over defaults), each entry
`{kind: email|chat, channel, domains}`. `kind` picks the distill prompt word, the WEG tier
(`user.email_evidence` / `user.chat_evidence`) and the channel↔provider map.
**`provider` is the transport, not the domain**: all Google Workspace domains
are `gmail`; `agentmail.to` is its own.

| provider | typical alias | lane | ingestor |
| --- | --- | --- | --- |
| `gmail` | business/personal/work | user_assist | `GmailIngestor` (`gws`) |
| `gmail` | presto | envoy | same |
| `whatsapp` | self | user_assist | local baileys `wu.db` |
| `whatsapp_kapso` | presto | envoy | Kapso CLI |
| `telegram` | presto | envoy | `origin_channel=telegram` chat sessions |
| `agentmail` | work | envoy | AgentMail CLI |
| `imessage` | (owner) | user_assist | macOS `chat.db` |

- **Kapso:** preflights the CLI on the scoped `PATH`; a missing binary reports
  `unavailable` in `last_error` without spawning or moving the cursor.
  `@kapso/cli` lives once in `skillshub/node_modules/.bin`; each call gets the
  workspace's credentials, cwd and isolated `HOME`. Env `KAPSO_API_KEY`,
  `KAPSO_PHONE_NUMBER_ID`, `KAPSO_BIN`; the bot receiver verifies an HMAC over
  the raw body with the secret of the webhook it registers on start.
- **AgentMail:** `agentmail --format json inboxes:messages list|get` with
  inbox-scoped `AGENT_MAIL_KEY`; outbound iff `from` is the inbox. Env
  `AGENTMAIL_BIN`, `AGENTMAIL_ACCOUNT_<SLUG>_EMAIL` / `AGENT_MAIL_KEY_<SLUG>`.
- **iMessage:** reads `$HOME/Library/Messages/chat.db` read-only (`rusqlite`;
  `MAGICIAN_IMESSAGE_DB_PATH`) under Full Disk Access; if refused, sync,
  distill fetches and verification-code reads go through Magican Desktop's
  `/host/imessage/query` (a Desktop grant alone suffices). 500 per chat page (desktop cap 1,000 rows); ROWID
  watermark, Apple-epoch→ms dates, `is_from_me` → direction. Confirm-gated `reply`: `compose` =
  `channel_reply_draft`, `commit` = AppleScript `imessage_send`; drafts in
  `channel_action_drafts` (DB v12).
- **WhatsApp:** names from chat/contact/group tables, else `+<number>`; rows
  persist newest-first before entering the distill queue.

`/channel-assist/channels` projects `provider_display`, `channel`,
`channel_label`, `capabilities` and `connected`; `/channel-assist/sync/status`
marks Gmail connected on readiness, others when enabled.

## Sync

`ChannelSyncWorker` runs the `POST /channel-assist/sync/run` pass while
`channel_observe` has an enabled message account: Observe catch-up at startup,
then `history.list` increments with a re-list fallback; errors sit on the
watermark row. Gmail consumes every message and label change before advancing
(ten-page budget, continuation in the cursor; the thread cap never drops a
consumed delta). A missing `users.getProfile.emailAddress` fails the pass
rather than misroute links; identity is stamped on new rows, kept on upsert, and NULL/stale
identity is repaired even on an empty history page.

## Distillation

Sync lands `distill_state = pending`; `ChannelDistillWorker` distills from
in-memory content and drops it. Sensitive rows are `suppressed`; failures bump
`distill_attempts` to `skipped` at `MAX_DISTILL_ATTEMPTS` (3);
the locality guard is the availability gate; `CHANNEL_DISTILL_ENABLED=0`
stops the worker. Coalescing distills a batch's
same-thread rows as one prompt onto the newest row
(`distill_evidence_message_ids_json`) and marks the rest `skipped`.

**History window.** Each tick marks pending/retryable rows older than the
Observe cutoff `expired` (metadata, safe summaries, revisions and attempts kept;
never re-enqueued, even if the window widens; completed/suppressed/exhausted rows
untouched; startup replay budgets never expire in-range rows). Retirement needs
no model or fetch, reads ≤ 256 ids per conditional update (state rechecked under
the writer, concurrent completions win, empty passes skip the writer), ≤ 32
batches per tick; the cutoff applies in SQL
so old history cannot block repair. `sync/status.distill_queue` /
`stats.distill.queue` report `pending`, `retryable`, `outside_history`,
`expired`, `retry_exhausted`, `history_floor_ms`; compatibility pending counters
count in-window work only, "eligible" ≠ running (worker/provider gates still
apply), and `by_state` is the full histogram. `/observe/stats` shows retirement
separately from queued work. Gate: `make test-distill-history`.

Config (`channel_assist.distillation` in `magician-config.yaml`):

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

`brief_contract_version: 2` adds a safe brief (facts, changes, timing, stated
action, omitted details) to the same `channel_ingest_distill` call, constrained
by a JSON Schema (Ollama `format`; `JsonObject`/`JsonSchema` elsewhere), then
validated (semantic + privacy) with one repair retry; parse diagnostics expose
field/type errors only. Compatibility summary/intent/follow-up fields remain. Writes atomically store `distill_brief_json`,
evidence ids and a scope-monotonic `distill_revision`; consumers advance on it,
not `internal_date` (`occurred_at` stays provider time).

V2 backfill runs after new work in bounded batches (surfaced first,
newest-first, concurrency 1), yields to the shared LLM
queue, and never overwrites a prior safe result. `POST /distill/backfill`
(`?dry_run=true`, `?paused=true|false`, or bare for one pass); state survives
restart. `GET /distill/recent` is an in-memory ring (`RecentDistillEntry`, 50):
metadata and summary/intent only, never bodies, reset on restart.

## Classifier

`assist::classify` turns distilled threads into `MailThreadAnnotation`s from a
body-blind `ThreadContext`. Labels `needs_reply` | `follow_up` | `fyi` |
`no_action`; off-vocabulary → `fyi` (confidence ≤ 0.4); actionable
(`needs_reply`/`follow_up`) at ≥ 0.7 →
`needs_approval`, else `classified`.

`store.list_threads_to_classify` yields each thread's newest unclassified
revision; `classification_input_revision` stops late results overwriting newer
evidence. `ChannelClassifyWorker` backs off via `classify_attempts` /
`classify_next_retry_at` and stops at `classify_failed_at`.

- Recently acknowledged/dismissed items from the same thread go to the model;
  `repeat_of_recently_handled=true` (substantially the same item) makes the router drop the card with
  `cooldown_active` (14 days; row still annotated).
- Deterministic required-action evidence beats a disagreeing label.
- A leading `stated_action` sentinel ("None", "no action needed") counts as
  absent; `promotion` and `event` briefs never promote a stated action, unlike
  `deadline`, `transaction`, `request`, `scheduling` and `change_notice`.
- Before withholding an actionable comm from Worth a look, the curator
  atomically upserts one provisional Follow-up (approved/in-progress/completed
  get evidence only; dismissed and active claims are untouched).

Prompt category `ChannelAssist`; any load failure leaves work retryable.

## Follow-ups

`store.list_needs_approval` takes the latest active `needs_approval` per
thread; only reconciliation, not a newer `classified`/`fyi`, can hide it.

`GET /channel-assist/follow-ups` (alias `GET /needs-you`) is cursor-paged
(`limit` 1..=100, default 50) → `{items, total, limit, cursor, next_cursor,
has_more, latency}`. Cards include annotation identity, lane, label, confidence, reason, summary,
subject, sender, `proposed_action`, `received_at` (evidence
`internal_date`, not `created_at`), `account_email`, `open_url`. Over
`CHANNEL_TODAY_PROJECTION_LATENCY_BUDGET_MS` the response is still served,
with `Server-Timing` and `X-Magician-Latency-Budget`.

Transitions are transactional on expected state; stale cards get 409 and no
feedback.

| Action | Effect |
| --- | --- |
| `POST …/approve` | "Do it": `approved` + `helpful` + one-shot `executive-assistant` task (`channel-assist` tag), optional `{hint}`. Claim first; claims > 10 min are stale. Only from `needs_approval`. |
| `POST …/useful` | `acknowledged` + `helpful`, no task. |
| `POST …/acknowledge` | `acknowledged`, **no** feedback. |
| `POST …/dismiss?reason=` | `dismissed` + `not_helpful`; irrelevant / not-actionable / obsolete / not-owner / duplicate. |
| `POST …/snooze` | Back to `classified`; no verdict. |
| `POST …/review` | Re-open a stale or retracted card. |

Feedback actions may return a `feedback_receipt`. Reads add health, embedding coverage
and rank metadata. Observe mode keeps the established cursor/order; enforced
ranking orders the whole eligible universe before paging (generation-bound
cursor) and never deletes, hides or transitions a card.
Follow-up owns a thread by typed equality of `(provider, account_alias,
thread_id)` (never text, embedding or LLM match); matching Worth rows become
origin aliases outside ranking/grouping/pagination until it goes inactive;
malformed refs fail closed; public alias summaries are opaque, capped at 100,
with no raw provider/account/thread/message/source refs (see [attention-routing-funnel.md](attention-routing-funnel.md)).
UI (`/today`, `/attention`): `ChannelFollowUpActions.svelte` +
`channelNeedsYouStore.ts`; fetch failures are explicit errors and keep
last-loaded rows.

## Reconciliation

`ChannelReconcileWorker` (adapter-blind `assist::reconcile`) scans active
annotations (`needs_approval`, `approved`, scheduled/draft, `sent_detected`)
that have newer evidence, a provider change, or age past the stale window.

- `needs_reply`/owner-owed + newer outbound → `completed`
  (`owner_or_agent_sent_after_evidence`), reported via
  `ReconcileCompletionSink`.
- Waiting-on + explicitly non-actionable inbound → `completed`.
- Ambiguous newer evidence → `superseded`, once that evidence is annotated.
- Quiet past `CHANNEL_RECONCILE_STALE_DAYS` → `stale`.
- Gmail `labelsAdded` `TRASH`/`SPAM` or `labelsRemoved` `INBOX` close work;
  `messagesDeleted` only when the whole thread is confirmed gone; a later
  restore in the stream wins.
- Inbound after `draft_requested` / `draft_ready` / `inserted` → `stale`
  (`newer_message_requires_draft_review`).
- Brief-routed annotations (`required_action_source = information_brief`)
  whose action `derive_channel_required_action` no longer derives → `stale`
  (`required_action_no_longer_derived`, `routing_retracted`), found via
  `list_active_brief_routed_annotations` after evidence rules decide.

Writes use `transition_annotation_if_state` (`actor=worker`): claims block,
user transitions win, terminal states never reopen, no annotations or tasks are
created. Counters: `workers.reconcile` on sync status.

## Bridges and memory

- `ChannelEvidenceBridgeWorker` is the only email→WEG writer
  (`producer_has_digest("email")` is false): distilled rows past
  `__bridge_distill_revision_v2__` become per `(channel, account, day)` digests
  in `user.email_evidence` / `user.chat_evidence` (calendar keeps its digest).
  Malformed JSON is downgraded and counted per row; the revision still advances.
- `ChannelFeedbackBridgeWorker` folds `MailAssistUserFeedback` per
  `(account, day)` into `user.channel_feedback` (`did` / `acknowledged` /
  `dismissed`, `dismiss_reasons`). Both bridges use composite cursor tie-breakers and hold the
  watermark on a tier write failure.
- Sender cooldown: over the last 90 days, at least 3 dismissals and at least
  `2 × helpful + 1` dismissals for the sender (or its domain) suppress
  model-only routes (`FeedbackTuningProfile::reduces_noise`, `store.rs`);
  deterministic hints bypass.
- `GET/POST /channel-assist/annotations/{id}/writing-preferences` learns
  style-only sender/domain statements from draft edits (raw drafts never
  stored); `/promote` mirrors to `user.channel_writing_preferences`,
  `/dismiss` retracts; nested user-memory tiers merge by stable key.
- `ChannelPatternSynthesisWorker` (daily, body-blind) writes
  `user.channel_patterns`.

## Live evidence and Worth a look

`live_content::ChannelEvidenceResolver` is the only metadata/body boundary:
exact scoped message tuple and coalesced batch, `has_newer` without
substituting the latest message. Only an explicit original request fetches
content (detail/metadata never invokes a fetcher), capped by
`ORIGINAL_BODY_MAX_CHARS` (20_000), `ORIGINAL_RESPONSE_MAX_CHARS` (40_000) and
`ORIGINAL_EVIDENCE_MAX_MESSAGES` (8); sensitivity is checked on the message, thread and every evidence row
before any fetch.
Bodies are request-local; `GET /annotations/{id}/message` adds
`evidence_messages[]`.

Worth-a-look actions ([resurfacing.md](resurfacing.md)) use only the safe
brief and server-resolved identity, never copy a live body into a task or
claim, and carry a UUID idempotency key plus card revision; only `summarize_deeper` reads live content,
and only when `resurfacing_deep_summary` has a permitted binding.

## Attention learning

Contract: [attention-routing-funnel.md](attention-routing-funnel.md). Channel switches:

- `attention_learning.rank_recompute.enabled` ships **true** (`Disabled`
  only when off). Feedback returns a pending job at once (no model in the
  action or list request); the worker publishes only if source revision,
  universe, posterior and snapshot CAS still match, retries a missing decision,
  and enqueue/worker failure never undoes feedback.
- **Slice 2 actionability** (`attention_learning.actionability.mode`:
  `disabled` | `shadow` | `enforced`) ships **disabled**; needs an immutable
  `snapshot_id` (L2-logistic + Platt). Missing/stale/invalid inputs fall back
  to Slice 1; shadow exposes probability without reordering.
  `attention_learning::training` joins `attention_outcomes` to
  served features and refuses to write when labels, holdout AUC or calibration fail. `--install` makes the first
  scope install `shadow`; `magician attention-learning
  install-actionability-snapshot --snapshot <artifact.json>` installs without
  activation; `train-routing` uses `not_actionable` / `action_completed`.
  `GET/POST /channel-assist/attention-learning/actionability-training/{status,run}`.
  Slice-1 kNN serves without a snapshot. Shipped `bandit.mode: shadow` with
  no pin (scope install is the switch); personal ranking stays shadow until
  40 attributed updates, then canary on each lane's first page.
- **Slice 3 grouping** (`attention_learning.grouping.mode`) ships disabled,
  needs a pair-model `snapshot_id`, groups the full universe before paging,
  and collapses cards only when enforced.
  `POST /channel-assist/attention-learning/pair-corrections` takes
  `same_underlying_item` | `same_obligation` | `not_duplicate`;
  `max_pair_evaluations` (2_500_000) vs `n*(n-1)/2` → singletons over budget.
- Memory: `POST /memory/entries/{tier}/{key}/confirm` (`preferences`,
  `research_findings`) and `PATCH /memory/entries/{tier}/{key}/scope`; confirm
  never invents a scope; memory effects are judged in shadow (card stays).
- Semantics: `channel_attention_semantics_v1` persists as `missing`/`invalid`
  when absent or malformed and never changes the legacy route;
  `semantic_backfill.enabled` ships true; repair leases rows into worker
  slots, never an LLM on a Follow-up list request.

```bash
MAGICIAN_SKIP_KEYCHAIN=1 magician attention-learning train-actionability \
  --principal anonymous --workspace default \
  --split temporal --out /tmp/actionability-snapshot.json
```

## Store

`read_connection()` clones from `read_conn` on the writer's instance; writes go
only through `write_conn` under `acquire_write_guard`. The 30s `CHECKPOINT` is
compaction, not durability; governed compaction blocks new reader clones and
rebuilds `read_conn`. External file replacement is unsupported (an open
instance serves from memory until restart). Annotations index
`attention_lane` (DB v11); Today ranks only actionable rows per thread.

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

Also here: `PUT /channels/purpose`, attention impressions/decisions, canonical
delivery, health, semantic extraction, historical bootstrap, routing training,
rank-recompute schedule/requeue/process, and resurfacing groups, repair and
recommendation events (`channel_assist_scope_for_prefix`,
`magician-bin/src/main.rs`).
User-memory confirm/`…/scope` live in `memory_api.rs`.

## CLI

```bash
MAGICIAN_SKIP_KEYCHAIN=1 magician channel-assist export-fixtures \
  --limit 200 [--account <alias>] [--out <path>] \
  [--principal anonymous] [--workspace default]
magician channel-assist classify-eval --fixtures <jsonl>
magician channel-assist draft-eval \
  --fixtures magician/tests/fixtures/channel_draft_usefulness_eval.jsonl
```

Fixtures default to
`<storage root>/channel_assist_fixtures/fixtures-<date>.jsonl`. The committed
baseline `magician/tests/fixtures/channel_classifier_eval.jsonl` has ≥4 safe
cases per label. Draft-eval checks required facts, forbidden/hallucinated
phrases, privacy sentinels, length and writing preferences (unsupported rules
fail closed).

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
| `CHANNEL_EVIDENCE_BRIDGE_ENABLED` | on (120s, startup 150s) |
| `CHANNEL_FEEDBACK_BRIDGE_ENABLED` | on |
| `CHANNEL_PATTERN_SYNTHESIS_ENABLED` | on (daily) |

Startup history age is the Observe catch-up choice, not a knob.

## Observability

`GET /channel-assist/stats`: totals (`by_provider`, `by_lane`), distill-state and annotation
state/label histograms, classify retry/fail counts, V2/legacy coverage,
backfill pause/backlog, a
synced→distilled→classified→needs_approval funnel, and a live `ops` block
`{distill, classify}` (`explicit_binding_for_operation` +
`get_config_for_operation`). Store failures return 500, not zeroes.

`/observe/stats` (`channelStatsStore.ts`, 15s poll) adds `/distill/recent` (4s) and per-op
cost from `/analytics/llm_calls/query`.
Distill/classify bypass the agentic executor, so `channel_assist/telemetry.rs`
(`emit_mail_llm_call`) emits `LLMResponseReceived` per call.
`magicutor/extension/gmail_mail_assist_latency.js`: 16.7 ms chip budget
(DOM adapter not enabled).

### Structured classifier rollout

The classify worker may run the engine's `channel_classify` pack first. Seeds
enable Jev with all five thresholds at 0.7 under the owner's
`allow_unqualified_gate` override; **qualification remains failed**. Resolved rows use the same
revision-aware persistence and funnel; rows below threshold, incomplete or
unresolved fall back to the LLM. DB v14 adds nullable
`classified_by_json` (record v10; legacy = LLM). ~10% of batches get read-only
LLM comparisons (`observation.gate_sample_rate: 0.1` in `decision-engine.yaml`)
that never replace Jev results. Batches follow the policy's
`classification.max_items` capped at 8 (seed 4; excess stays pending), else
ordinary LLM batching.

Packets and reports live outside git in
`$MAGICIAN_ROOT_DIR/evals/channel_classify`. Worker calls are metered normally;
delegated adjudication counts separately from human-reviewed labels.

| Step | Command | Contract |
| --- | --- | --- |
| Review | `scripts/channel_classify_review.py --packet <review.jsonl> --baseline <baseline.jsonl> --output <review.md>` | Renders five heads; `--reviewed <approved.jsonl>` freezes approved labels only. |
| Baseline | `magician channel-assist classify-eval --decision-baseline --fixtures <review.jsonl> --output <baseline.jsonl>` | Read-only, real prompts; `--principal`/`--workspace` match the packet; kept out of the live `/llm` ledger. |
| Shadow | `--decision-shadow` | Fresh projections (≤ 1 h); shadow-only policy, private ledger, no annotations (persistence verified after qualification). |
| Collect | `eval-channel-classify-decisions.py --predictions-only` | No scoring; `--replay <rows.jsonl>` reuses exact inputs and dated prices with no model calls; never rewrites a completed artifact; missing usage stays unknown. |
| Qualify | `make qualify-channel-classify ARGS="..."` | Needs `--gold`, `--baseline`, one `--offline` dir per routed model, `--records`, `--socket`, `--reference-version`, `--review-ref`, `--principal`, `--workspace`, `--output`; recomputes the offline bar from raw receipts; 200 distinct complete current-cohort comparisons; stale thresholds, unknown costs and replays excluded. |

Thresholds are fitted on development cases and tested unchanged on held-out
ones. Reports separate confidence fit from gate authorization (which alone
proves no persistence). A qualification binds exact model/provider, pack, projection, reference,
policy and thresholds, and is never inherited; failing reports exit nonzero and
never enable a gate. Live reporting streams `DecisionShadowAgreement`
envelopes. Engine envelopes use their own `decision-engine:` profile so the
actionability trainer keeps cohorts apart; origin stays in `classified_by`.
Memory qualification floors are unaffected.
