# `/observe` — Mail & Chat channel toggles

The single "Mail & chat" card on `/observe` chooses which accounts Magician
follows. Enabling an account makes it **both** observed (work evidence)
**and** assisted (follow-ups, drafts): one pipeline, one toggle. The calendar
card is separate (calendar keeps the digest, not channel-assist ingest).
Discovered channels include your Gmail, your WhatsApp, Presto's Kapso WhatsApp,
and Presto's own **AgentMail** inbox (`magican@agentmail.to`, Presto lane;
`channelProviderLabel` renders "AgentMail (Presto)").

- **Store:** `src/lib/stores/channelAssistStore.ts`,
  `fetchChannelAssistChannels()` / `saveChannelAssistChannels()` against
  `GET`/`PUT /api/magician/v2/channel-assist/channels`. GET merges discovered
  accounts with their state in the unified `channel_observe` config plus synced
  counts; PUT writes `channel_observe`. The `history_lookback_days` field only
  mirrors the startup catch-up policy for older clients.
- **Card:** in the **Sources** pane of `src/routes/(app)/observe/+page.svelte`,
  beside calendar consent. Rows show provider and account, connected state,
  synced count, a **Presto/You** lane badge and an enable checkbox. Save writes
  the **full** message-channel set, so an unlisted account cannot be dropped
  silently. The only history control is **Startup catch-up**.
  It replaces the separate `email_observe` digest consent and
  `channel_assist` registry, which asked the same question twice.
- **Follow-up stores/actions:** `channelNeedsYouStore.ts`,
  `channelStatsStore.ts` and `ChannelFollowUpActions.svelte` serve Today,
  Attention and Observe stats and call `/channel-assist/*` directly
  (mail-named wrappers are not supported).

Backend contract: `docs/components/magician/mail-assist.md` (unified
`channel_observe` config, evidence bridge, `account_lane`).

## Startup catch-up

`ObserveCatchUpPanel.svelte` is the single scoped control for automatic work
after Magician was offline. `GET/PUT /api/magician/v2/observe/catch-up` exposes
an optimistic-revision policy plus this boot's status. The owner can turn
recovery on or off and set a history window, a per-source item cap, a shared
feed/message total cap, and a duration after which no new catch-up work starts.
Defaults: 7 days, 50 items per source, 200 feed/message items overall, 15
minutes. Saving resets only this boot's admission ledger; it does not delete
records or stop ordinary polling.

Status shows admitted, running, processed, remaining budget, failures and each
source's replay class, refreshed every five seconds while recovery is waiting
or active (unsaved edits are kept):

- **Message adapters** (Gmail etc.) use provider checkpoints but never keep rows
  older than the floor; with recovery off or a cap hit they set a post-boot
  baseline instead of returning to an old cursor.
- **Local message understanding** drains only eligible rows in the window,
  local-model-only, sensitive rows suppressed.
- **Published Notes** replays eligible notes in the window across bounded pages
  with a revision-aware checkpoint; its cursor starts at the newest page and
  stays pinned to the post-boot window.
- **Product Hunt, arXiv, generic RSS** are **Current feed only**: entries that
  aged out while down cannot be reconstructed.
- **Calendar**'s scheduled metadata writer uses the day window; overdue cron
  ticks collapse to one run, and its task prompt carries the per-source
  ceiling. It is bounded by that prompt and the Task runtime, not the shared
  item ledger.

The duration is an **admission deadline, not a cancellation timer**: work
holding a durable lease finishes through its normal timeout or failure path.
Manual **Check now** does not consume the startup ledger.

## Pipeline stats page (`/observe/stats`)

Opened from the **Pipelines** link. The mail/chat section
(`channelStatsStore.ts` → `GET /channel-assist/stats` + `/sync/status`, polled
every 15s) shows counters, a synced→distilled→classified→needs-you funnel,
distillation state, classifier labels with retry/failed counts, by-lane totals,
an LLM-usage panel and a per-account table. The same API exposes runtime knobs
for history lookback, distill/classify batch size, concurrency and thread
coalescing. Read failures show as errors, never as zeroed counters.

- **Engine labels are config-resolved**: `stats.ops.{distill,classify}` render
  as `model · provider` via `bindingLabel()`.
- **Live progress:** `done / (done + pending)` with client-side EWMA throughput,
  drain ETA, and a pulsing indicator while `pending_distill > 0`.
- **Live input→output feed:** polls `GET /channel-assist/distill/recent` every
  4s: an IN card (subject and sender only) beside the OUT summary and intent
  with latency.
- **Classification reconciles to the thread total:** all four labels
  (including zero), `Awaiting classification`, `Not yet distilled`, and
  `{classified} of {total} threads classified`.
- **LLM usage is parquet-backed:** `/analytics/llm_calls/query` gives calls,
  In/Out tokens and cost of `channel_ingest_distill` + `channel_classify` over
  30d, falling back to store counts without telemetry.

The top also mounts `ObservableSourceObservability.svelte`: a fixed-column Web
Sources table with five summaries per server page (enabled/healthy sources, run
success, awaiting ingress, quarantined failures), per-row health,
discover→select→process counts, queued/failed handoffs, dedup, RSS 200/304,
bytes and average duration. An RSS `304` is a healthy unchanged run. Opening a
row loads its run ledger through its own cursor; a stale cursor restarts only
its own view. Processed Product Hunt, arXiv and RSS items enter the shared
resurfacing curator and can appear in **Today > Worth a look**.

## Browser tab observation visibility

The Observe Tabs card reads `GET /ambient/status` (captured today, distinct
pages, origins, retained metadata bytes, buffered signals, distill state,
pending clusters, memory-review pending) from the ambient stats ledger, not
browser state.

`/observe/stats`' Browser Tabs section uses `GET /ambient/stats`: ingestion
funnel, redacted recent signals, page/type/byte breakdowns, distill runs,
cluster journal rows (with candidate id and review state when evidence routes
into a review-gated `user.knowledge` memory candidate), `ambient_distill`
token/cost, and memory-enrichment states (pending/approved/indexed/rejected).
Each completed distill run appends one display-only summary to the current
`Tabs — YYYY-MM-DD` chat session, deduped by run id. Expired detail partitions
compact to `stats/daily/dt=YYYY-MM-DD/summary.json` before deletion, and the
today/7d/30d views merge both, so history survives retention.

The route's meeting, screen-watch and ambient-browser HTTP commands live in
`src/lib/observe/api.ts`, a typed, unit-tested boundary for serialization, id
encoding, response normalization and errors.

## Message follow-ups

The classifier's `needs_approval` annotations (`channelNeedsYouStore.ts` →
`GET /channel-assist/follow-ups`) show on `/today` and `/attention` (not on
`/observe`), sharing `ChannelFollowUpActions.svelte`:

- **Do it**: optional hint modal, then creates the follow-up task. The backend
  claims the approval first, so concurrent clicks do not duplicate.
- **Acknowledged**: positive, no action. **Snooze**: hides it.
- **Dismiss**: reason menu feeding the negative learning signal.
- **Open**: deep link routed by the card's `account_email` (`authuser`), so
  non-default mailboxes open under the right account.
- **Show message**: fetches live (never stored) the source message
  (`evidence_message_id`, else the newest distilled message) with its stored
  Summary; coalesced threads render the `evidence_messages[]` batch and note a
  newer arrival.

Rows show label (Needs reply / Follow up), lane badge, provider, subject,
sender, reason and local arrival time (`received_at`). On `/today` a "Message
follow-ups" section in **Follow-ups** adds to its count, with a short preview
and "Show all in Attention"; on `/attention` it is a `Messages` source chip with
newest-first pages. Fetch failures show a notice and keep the last rows.

## Calendar lanes (You / Presto)

The **Observe calendar** card carries the same lane badges: the owner's
calendars are `user_assist` ("You"); Presto's Google calendar (`gws-presto`) is
the `envoy` ("Presto") lane. `observeConnectorStore.ts`'s `ObserveAccount` has
a `lane` field; backend `discover_accounts` appends the `presto` account and
tags the rest `user_assist`. Enabling `presto` needs `gws-presto` to be
calendar-authenticated; otherwise its toggle is disabled ("not connected").

## Observable web sources

`ObservableSourcesPanel.svelte` manages scheduled public web feeds. Listening,
Available and Needs setup tabs load separately through `sourceApi.ts` with
opaque cursors (five rows per page); a stale cursor restarts only that tab, and
the Listening total is authoritative. Each available row starts one exact
catalog offer (source, profile, source revision, the backend's cadence and
limits). Listening rows support Check now, Save, Pause/Resume and Stop with
revision checks; a custom RSS form uses the same API. The panel never builds a
retrieval ladder or substitutes actions: a changed source or policy returns a
stable stale-source error, refreshes state and needs a deliberate retry.

Tests: `src/lib/observe/ObservableSourcesPanel.component.test.ts`,
`ObservableSourceObservability.component.test.ts`, `sourceApi.test.ts`.

## Use for verification codes (secure HITL)

Email, AgentMail and Messages rows carry a separate grant, **Use for
verification codes** (`hasVerificationCodesPurpose`,
`setChannelVerificationCodes` →
`PUT /api/magician/v2/channel-assist/channels/purpose`
`{provider, account_alias, purpose: "verification_codes", granted}`). The
runtime's resolver may then read that account for a one-time code *only while a
verification request is open* and answer the request itself, so the code never
reaches the agent. Observing grants none of this. The toggle is disabled until
the account is enabled, writes at once (not part of Save), and survives
enablement rewrites. Contract:
`docs/components/magician/verification-code-retrieval.md`.
