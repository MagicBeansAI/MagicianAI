# `/observe` — Mail & Chat channel toggles

The single "Mail & chat" card on `/observe` chooses which accounts Magician
follows. Unified observe+assist: enabling an account makes it **both** observed
(work evidence) **and** assisted (follow-ups, drafts) — one pipeline, one toggle.
Email is a channel here; the calendar card is separate (calendar keeps the
digest, not the channel-assist ingest). Discovered channels include your Gmail,
your WhatsApp, Presto's Kapso WhatsApp, and Presto's own **AgentMail** inbox
(`magican@agentmail.to`, Presto lane; `channelProviderLabel` renders "AgentMail
(Presto)").

- **Store:** `src/lib/stores/channelAssistStore.ts` —
  `fetchChannelAssistChannels()` / `saveChannelAssistChannels()` against
  `GET`/`PUT /api/magician/v2/channel-assist/channels`. GET merges discovered
  accounts with their state in the unified `channel_observe` config plus
  per-account synced counts; PUT writes `channel_observe` directly. The
  `history_lookback_days` transport field only mirrors the startup catch-up
  policy for older clients.
- **Card:** in the **Sources** pane of `src/routes/(app)/observe/+page.svelte`,
  beside calendar consent. Rows show provider + account, connected indicator,
  synced count, **Presto/You** lane badge and an enable checkbox. Save writes the
  **full** message-channel set, which structurally prevents silently dropping an
  unlisted account. The only history control is **Startup catch-up** below.
- **Why one surface:** the former `email_observe` digest consent and
  `channel_assist` registry were the same "let Magician into this account"
  decision made twice.
- **Follow-up stores/actions:** `channelNeedsYouStore.ts`,
  `channelStatsStore.ts`, and `ChannelFollowUpActions.svelte` are the
  channel-named contracts for Today, Attention and Observe stats; they call
  `/channel-assist/*` directly (mail-named wrappers are not supported).

Backend contract: `docs/components/magician/mail-assist.md` (unified
`channel_observe` config, evidence bridge, `account_lane`).

## Startup catch-up

`ObserveCatchUpPanel.svelte` is the single scoped control for automatic work
after Magician was offline. `GET/PUT /api/magician/v2/observe/catch-up` exposes an
optimistic-revision policy plus this boot's authoritative status. The owner can
enable/disable recovery and set a history window, per-source item cap, shared
feed/message total cap, and a duration after which no new catch-up work starts.
Defaults: 7 days, 50 items per source, 200 feed/message items overall, 15
minutes. Saving resets only this boot's admission ledger; it does not delete
stored records or disable ordinary polling.

Status shows admitted, running, processed, remaining budget, failures and each
source's honest replay class, refreshed every five seconds while recovery is
waiting or active (without overwriting unsaved edits):

- **Message adapters** (Gmail etc.) use provider checkpoints but never retain
  rows older than the floor; with recovery off or a cap hit they set a post-boot
  baseline instead of returning to an old cursor next tick.
- **Local message understanding** drains only eligible rows in the window,
  local-model-only, sensitive rows suppressed.
- **Published Notes** has a revision-aware checkpoint and replays eligible notes
  in the window across bounded pages; its catalog-offset cursor starts at the
  newest page and stays pinned to the post-boot window afterwards.
- **Product Hunt, arXiv, generic RSS** are **Current feed only**: bounded,
  date-filtered current responses; entries that aged out while down cannot be
  reconstructed.
- **Calendar**'s scheduled metadata writer uses the day window; overdue cron
  ticks collapse to one run; its task prompt carries the per-source ceiling.
  Calendar is bounded by that prompt and the Task runtime, not the shared item
  ledger.

The duration is an **admission deadline, not a cancellation timer**: an
operation holding a durable lease finishes through its normal timeout/failure
path so it cannot strand the lease. Manual **Check now** does not consume the
startup ledger.

## Pipeline stats page (`/observe/stats`)

Opened from the **Pipelines** link. The mail/chat section
(`channelStatsStore.ts` → `GET /channel-assist/stats` + `/sync/status`, polled
every 15s) shows top-line counters, a synced→distilled→classified→needs-you
funnel, distillation state, classifier labels with retry/failed counts, by-lane
totals, an LLM-usage panel, and a per-account table. The same API exposes
runtime knobs for history lookback, distill/classify batch size, bounded
concurrency and thread coalescing. Read failures show as errors, never zeroed
healthy-looking counters.

- **Engine labels are config-resolved**: `stats.ops.{distill,classify}` render as
  `model · provider` via `bindingLabel()` from the operation router.
- **Live progress:** `done / (done + pending)` with client-side EWMA throughput
  (msgs/min from polled deltas, no backend state), drain ETA, pulsing indicator
  while `pending_distill > 0`.
- **Live input→output feed:** polls `GET /channel-assist/distill/recent` every 4s,
  showing an IN card (subject + sender, metadata only) beside the OUT summary +
  intent with latency.
- **Classification reconciles to the thread total:** all four labels (including
  zero), `Awaiting classification`, `Not yet distilled`, and
  `{classified} of {total} threads classified`.
- **LLM usage is parquet-backed:** `/analytics/llm_calls/query` (the lakehouse
  `/llm` and Today's Pulse use) for calls, In/Out tokens and priced cost of
  `channel_ingest_distill` + `channel_classify` over 30d; falls back to store
  counts if telemetry is unavailable.

The top also mounts `ObservableSourceObservability.svelte`: a token-only
fixed-column Web Sources table, five summaries per server page (enabled/healthy
sources, durable run success, awaiting ingress, quarantined failures), per-row
last-check health, discover→select→process counts, queued/failed handoffs, dedup,
RSS 200/304 behavior, bytes and average duration. An RSS `304` is a healthy
unchanged run with no new candidates. Opening a row loads its bounded run ledger
through an independent cursor; stale source or run cursors restart only their own
view. Processed Product Hunt, arXiv and RSS items enter the shared resurfacing
curator and can appear in **Today > Worth a look**.

## Browser tab observation visibility

The Observe Tabs card reads the enriched `GET /ambient/status` (captured today,
distinct pages, origins, retained metadata bytes, buffered signals, distill
state, pending clusters, memory-review pending) from the ambient stats ledger,
not browser state.

`/observe/stats`' Browser Tabs section uses `GET /ambient/stats`: ingestion
funnel, redacted recent signals, page/type/byte breakdowns, distill runs, cluster
journal rows (with candidate id and review state when evidence routes into a
review-gated `user.knowledge` memory candidate), `ambient_distill` token/cost,
and memory-enrichment states (pending/approved/indexed/rejected). Completed
ambient distill runs append one display-only summary to the current
`Tabs — YYYY-MM-DD` chat session, deduped by run id. Expired detail partitions
compact to `stats/daily/dt=YYYY-MM-DD/summary.json` before deletion, and the
today/7d/30d views merge retained detail with those summaries so history survives
retention.

The `/observe` route's meeting, screen-watch and ambient-browser HTTP commands
live in `src/lib/observe/api.ts`, a typed boundary owning serialization, id
encoding, response normalization and error propagation (unit-tested without
mounting the route).

## Message follow-ups (Phase 3 approval loop)

The classifier's `needs_approval` annotations (`channelNeedsYouStore.ts` →
`GET /channel-assist/follow-ups`) surface natively on `/today` and `/attention`
(not on `/observe`), sharing `ChannelFollowUpActions.svelte`:

- **Do it** — optional hint modal, then creates the follow-up task. The backend
  claims the approval before creating the external task, so concurrent clicks do
  not duplicate.
- **Acknowledged** — positive, no action. **Snooze** — hides it.
- **Dismiss** — reason menu feeding the negative learning signal.
- **Open** — account-routed deep link: the card carries `account_email` and the
  link is `authuser`-routed, so non-default mailboxes open under the right
  account.
- **Show message** — fetches on demand (live, never stored) the message the
  summary was derived from (`evidence_message_id`, else the newest distilled
  message), with its stored Summary; for coalesced threads it renders the
  `evidence_messages[]` batch, and notes when a newer message arrived.

Rows show label (Needs reply / Follow up), lane badge, provider, subject, sender,
reason, and local arrival time (`received_at`). On `/today` a "Message follow-ups"
section in the **Follow-ups** tab rolls into its count, with a bounded preview and
"Show all in Attention"; on `/attention` it is a `Messages` source chip with
newest-first paginated rows. Fetch failures render as notices and keep the last
loaded rows rather than an empty all-clear.

## Calendar lanes (You / Presto)

The **Observe calendar** card carries the same lane badges: the owner's
calendars are `user_assist` ("You"); Presto's Google calendar (`gws-presto`) is
the `envoy` ("Presto") lane. `observeConnectorStore.ts`'s `ObserveAccount` has a
`lane` field; backend `discover_accounts` appends the `presto` account and tags
the rest `user_assist`. Enabling `presto` requires `gws-presto` to be
calendar-authenticated; otherwise its toggle is disabled ("not connected").

## Observable web sources

`ObservableSourcesPanel.svelte` is a separate server-backed surface for
scheduled public web feeds. Listening, Available and Needs setup tabs load
independently through `sourceApi.ts` with opaque cursors (five rows per page),
recovering stale cursors by restarting only the active tab; the Listening total
is authoritative. Each available row starts one exact catalog offer (source,
profile, source revision and the backend's bounded cadence/limits). Listening
rows support Check now, Save, Pause/Resume and Stop with optimistic revision
checks; a custom RSS form uses the same API. The panel never builds a retrieval
ladder or substitutes actions: source/policy changes return a stable stale-source
error, refresh authoritative state and require deliberate retry.

Tests: `src/lib/observe/ObservableSourcesPanel.component.test.ts`,
`ObservableSourceObservability.component.test.ts`, `sourceApi.test.ts`.

## Use for verification codes (secure HITL P6)

Email, AgentMail and Messages rows carry a separate grant, **Use for verification
codes** (`hasVerificationCodesPurpose`, `setChannelVerificationCodes` →
`PUT /api/magician/v2/channel-assist/channels/purpose`
`{provider, account_alias, purpose: "verification_codes", granted}`). It lets the
runtime's resolver read that account for a one-time code *only while a
verification request is open* and answer the request itself, so the code never
reaches the agent. Observing grants none of this. The toggle is disabled until
the account is enabled, writes immediately (not part of Save's list), and
survives enablement rewrites. Contract:
`docs/components/magician/verification-code-retrieval.md`.
