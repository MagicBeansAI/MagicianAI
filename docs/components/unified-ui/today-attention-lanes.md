# Today Attention Lanes

The Today page renders attention as **backend-backed lanes**, not independent
client-side lists:

- **Follow-ups** — actionable comms obligations and promise-like items.
- **Worth a look** — low-urgency resurfacing of useful non-actionable context.
- **Active work** — work in progress or awaiting operational attention. The
  backend rejects stale running feed rows when task metadata says pending,
  completed, failed, cancelled, or no active root execution.
- **Delivered** — completed work and outputs.
- **Changed** — observed deltas and digest bullets (backend digest pagination).

The URL carries tab/page state so each lane is linkable. Growing lists use
backend cursor or offset pagination, never client-only slicing. Source-family
counts in Observe are diagnostic metadata, not surfaces.
[Memory connections](../magician/memory-connections.md) reuse Worth a look, For
you and HITL; connection insights stay individual Changed cards with their own
detail links and dismissal identities.

## Follow-ups

The message-follow-up sublist is paginated from `/channel-assist/follow-ups`
(`/channel-assist/needs-you` is a legacy alias). Rows show the evidence received
time and the classifier's structured action fields, including `key_details`
(due dates/times, amounts, billers, meeting times, redacted last4-style ids —
never full cards/accounts/phones/references). Cards keep their clocks: received
(`received_at`), expected follow-up (`due_text`/`due_at`). A principal or
workspace change invalidates the pending request and loads page one for the new
scope.

Actions: **Useful** posts `/useful` (positive `helpful` signal); **Acknowledge**
is a neutral "seen" with no signal (both move the annotation to `acknowledged`,
no task); Dismiss is a one-click default plus a ▾ reason menu; **Show message**
is a split button (toggle inline body; ▾ holds Open and Writing style). Snooze and
Dismiss are native 32px icon buttons that stop their own click (never falling
through to row Open), opt out of press-scale, and an open Snooze row raises its
stacking layer so its menu stays above later cards.

**Channel actions are descriptor-driven** on web and iOS: clients decode
`available_actions` and use the generic compose/commit endpoints, never branching
on Gmail, iMessage or future providers. `needs_compose` actions support draft,
edit, redraft and explicit send; direct actions honor the descriptor's
confirmation flag. Today and Attention share the native action sheet (stale
**Review & re-open**, evidence, annotation Snooze — distinct from Today
visibility snooze presets).

**Create reminder** (when advertised) creates a real Apple Reminder through the
local macOS desktop host and brings Reminders forward — never a Magician task.
Missing host or Automation permission keeps the dialog and card with a
retryable error; the retained operation key prevents duplicates.

Meeting-derived Follow-up rows use the generic Today source-action contract: with
no linked task the row shows the backend-advertised idempotent **Create task**
beside Open; success refreshes the lane and opens the canonical task panel, and
the row leaves via backend-owned linkage reconciliation.

## Worth a look

A table-style resurfacing lane shared by Today and Town Square: server
pagination, a safe summary plus up to three structured facts (including event
time from `temporal_anchor_at`), and markers for missing or newer source info.
Historical summary-only cards show `Legacy brief - repair pending` until the
bounded V2 backfill refreshes their content revision. Details and Original are
separate lazy reads.

One capability-validated recommendation is primary; secondary source/contextual
operations and Mark useful (positive, boosts similar), Acknowledge (neutral) and
Dismiss live in a keyboard-accessible More menu. Dismiss is one-click plus
"Dismiss because…" (`spam` · `already handled` · `duplicate` · `delegated` ·
`not relevant`); `postResurfacingAction` sends `reason` only on dismiss.
Resurfacing has no snooze. All Worth async work is generation-bound to the
current principal and workspace. Non-adjacent page jumps use a server offset
until a matching cursor is known; adjacent navigation uses the cursor.

**Side effects** (Task, Apple Reminder, Share, Memory dialogs) never remove a row
while in flight; failures keep row and dialog and reuse the idempotency key; a
stale content revision refreshes the row/detail and requires fresh confirmation;
only a durable success refetches the lane. Ask Presto opens normal chat with a
candidate-only route; chat stages the safe brief as an editable attachment
(never the live original, never auto-sent). Today and Town Square pass an
existing scoped chat thread — the component never invents one. "Shown" telemetry
starts only when the validated primary action is mounted.

## Canonical Follow-up/Worth union

Today, Town Square, full Attention and the compact Attention center consume one
canonical projection for both lanes, from
`GET /channel-assist/attention-learning/canonical-projection`. Legacy list
responses carry a compact reference instead of re-embedding it.

**Adoption is atomic.** The client uses the union only after validating schema
version, policy, all three lanes, every tagged origin/payload/action/group,
globally unique canonical ids, lane totals and the reconciliation equations:
`raw = materialized + duplicate_hidden` and
`raw = grouped_members + duplicate_hidden`, where exact cross-lane communication
aliases are the only valid `duplicate_hidden_total` (unmatched candidates stay
zero). Absent, malformed, partial or inconsistent → both lanes stay on guarded
legacy sources. Up to 100 deterministically ordered alias records each bind one
materialized Follow-up origin to one non-materialized Worth origin; aliases are
never cards, only a hidden-duplicate count. A failed same-scope refresh keeps the
last verified union; a scope change clears it atomically.

The server reads the four rank/grouping generations from one SQLite snapshot,
coalesces concurrent work per principal/workspace, and retries once on a typed
generation-change race; a second race fails closed to the legacy baseline with
only `canonical_learning_evidence_changed`.

Legacy Worth reads carry a strict `cross_lane_reconciliation` proof bound to
principal and workspace. Reconciliation happens before visible pagination while
the cursor advances across every scanned raw row. Cursor tuples keep the store's
exact 64-bit salience — never round through the card's 32-bit score. The browser
does no semantic or identity dedup. A last verified same-scope Worth page may
stay visible with a system diagnostic; otherwise Worth is unavailable beside an
independently preserved Follow-up surface.

Ordered arrays are presentation order: clients page but never filter, sort,
dedupe, regroup or reroute. Cross-origin cards are labelled. **Lifecycle actions
follow `origin_lane`**, never `served_lane` or `learned_lane`:

- Follow-up origins: annotation endpoints for Useful, Approve, Acknowledge,
  Dismiss, Snooze; provider deep link as GET `open_source`.
- Worth origins: resurfacing action endpoint for Useful (`action=open`),
  Acknowledge, Dismiss; an HTTP(S) source may be GET `open_source`.
- Each advertised href is validated against the tagged origin before calling the
  typed lifecycle client.

The projection strip shows exact-union integrity and policy state; with learned
routing disabled it reads **Learned routing inactive**. Shadow is preview-only.

### Exact communication identity before pagination

Before either lane builds a page or cursor, an active Follow-up owns every Worth
`comm` row with an exactly matching validated `(provider, account_alias,
thread_id)`. Those Worth rows stay durable aliases (origin evidence preserved) but
never enter rank, grouping, routing, materialization or cursor order; they
reappear when the Follow-up becomes inactive. Malformed or unavailable identity
fails closed.

### Parity with the legacy controls

The projection envelope is validated with an **exact-key allowlist**, so any
optional field the server omits must be declared optional — an unknown key
rejects the whole projection and drops Today to the pre-delivery lane, which
cannot record verified impressions. Parsers distinguish missing (`undefined`)
from empty (`''`, `null`); collapsing `''` to absent turns one subject-less email
into `fallback_reason: malformed_lane` for the page.

Canonical payloads carry neither `proposed_action` nor `available_actions`, so
rows are joined to the legacy lists — Follow-ups by annotation id (paging the
cursor, since the endpoint clamps `limit` to 100), Worth by candidate id (a paged
lookup, `limit` 100, at most six pages; a failed lookup leaves server-declared
verbs). The lookup map is passed to the row helper explicitly.
`ChannelFollowUpActions` is a complete control set and **substitutes** for
`item.actions`; `ResurfacingCardActions` is an overflow menu and must
**supplement** them (substituting drops Useful/Acknowledge/Dismiss).
`resurfacingActionResult` holds the surface-independent outcome (toasts, Ask
Presto navigation). A row whose origin has rich controls shows a skeleton until
its lane's lookup has **ever** settled, rather than painting and swapping. Read
actions (`view_details`, `show_original`, `open_source`) dispatch to the host.
The per-lane `CANONICAL_*_AT_PARITY` gates were removed when `/today` cut over
to Morning Edition; the canonical lanes now render on `/square` without a flag.

### Loading and optimistic writes

Canonical tabs hold a skeleton until the ranked projection is ready and the lane
until the first frozen delivery page lands — never painting the unfiltered legacy
list or unpaged universe first (that causes count/UI flash). Useful/Dismiss keep
the current page (optimistic tombstones, stale-while-revalidate); the next cursor
refills to `pageSize`; a failed write restores the exact row. The tab badge
subtracts in-flight tombstones. Clicks do not rebuild the large union; a 2s idle
refresh does once. Background polls skip while a click is in flight so they
cannot starve the four-connection attention SQLite pool.

## Decision-bound delivery

Per lane the renderer requests
`GET /channel-assist/attention-learning/canonical-deliveries/{lane}`; the first
request supplies page capacity, later ones only the opaque cursor. Shipped
config is disabled, so delivery is deterministic `baseline_fallback` and the UI
must not call it learned activation. An applied learned order must be
canary-assigned with exactly one root sample (parser accepts `{0, 1}`). An absent
endpoint or malformed response quietly keeps the canonical projection.

One frozen root decision binds lane, projection id, universe digest, policy
snapshot/model, posterior version, seed identity, universe size and expiry. A
page binds delivery id, zero-based page index/start, current and next cursors and
expiry to it; item positions are one-based and must equal
`page_start + array_index + 1`. First page `replay=false`; stored cursor pages
`replay=true`.

The browser appends only a page with identical root/projection/digest and the
next contiguous index, start, cursor and positions. A repeated delivery id is
ignored only if it is the identical replay; any changed binding, duplicate
candidate/token, gap, expired root, scope change or typed `refresh_required`
resets delivery atomically and requests a new first page. Row order comes from
the frozen items; a lifecycle click adds a scoped in-memory tombstone before the
API call — success keeps it for a two-minute reconciliation grace, failure
restores the exact position.

**Impressions.** Each card carries delivery id, page index, position and an
opaque exposure token. Fetching or mounting records nothing; only continuous
viewport visibility at the threshold for the response's `min_visible_ms`, with
the document visible and the root unexpired, posts a verified impression.
Receipts keep `root_policy_propensity` separate from
`conditional_delivery_propensity` (exactly `1.0`, since paging does not
resample). Adapter-declared communication commits carry the same frozen
attribution; with delivery unavailable, legacy rows stay actionable with
decision-only attribution and no raw-card impression.

**Rank recompute.** Explicit feedback may return an async `rank_recompute`
receipt (`enqueued` + `pending`, with the scoped
`/attention-learning/rank-recompute/jobs/{job_id}` URL); the parser accepts
`served_universe_diagnostic` and `current_universe_diagnostic` and rejects others.
The shared queue (`GET /attention-learning/rank-recompute/status`) shows inside
the learning-health strip, loaded when the disclosure opens and refreshed while
open: pending/in-flight/retry are live; succeeded/stale/dead are retained ledger
rows. Job status: `pending|in_flight|retry|succeeded|stale|dead`. Card order stays
server-owned until the next list load; direct feedback is quiet on success.

## The date Today is asked about

Both `/v2/today` callers — `todayStore.ts::buildQueryString` (the lane request)
and `today/sectionCursorProbe.ts::todaySectionCursorProbeUrl` (the deep-page
cursor walk on `/today` and `/square`) — send `today=YYYY-MM-DD` from
`taskStore.ts::readerLocalDate()`, the same derivation the tasks list uses. They
must agree within a render: a Follow-ups cursor is
`{priority}:{updated_at}:{item_id}` whose band derives from the date. The server
cannot know the reader's zone and otherwise answers from the UTC date (wrong for
part of every day east of Greenwich); `toISOString()` on a local instant names
yesterday. The parameter is optional on the wire. Server side:
`docs/components/magician/today-feed.md`.

## Visibility reconciliation

Dismiss and snooze remove the row optimistically. While the visibility POST is
in flight, refetched pages strip that id and decrement counts so polls or
realtime refreshes cannot flash it back. On success Today refetches the active
server page so cursor pagination fills the gap. A failed POST restores the row at
its original section/index only if principal, workspace and query still match;
undo waits for the hide before posting restore.

## Direct HITL opening

Today and Town Square keep the source FeedItem's HITL metadata. A row with a
typed `hitl_request` (or complete legacy HITL metadata) opens the root prompt
directly and posts with the real correlation/pause/request/approval id —
independent of whether the row is in the loaded `/feed/attention` page. Approval
rows expose one Review action opening the shared canonical prompt; they never
approve or reject from a projected row. Without a typed target, the only id
fallback is `todayAttentionItemId(item)` (explicit alias, the
`today:<section>:<feed-id>` projection id, or a documented Attention-family
prefix); arbitrary `source_id` values are never sent.

URL `attention_item=…` without `attention=1` makes the center resolve that item
and open its prompt with no list behind it: loaded aliases first, then the scoped
exact-item endpoint (so rows beyond the page cap open without cursor scanning).
Taskless canonical requests (e.g. bot-auth) resolve from the backend's scoped
HITL registry and private keyed lifecycle authority; the scoped durable
UserRequest is the prompt-body authority, and sealed app-notification proofs are
content-free. Durable resolution tombstones stop stale V3 projections from
reopening a resolved prompt after a crash; stored HITL rows must still be present
in current V3 attention or the pending registry. A detached prompt listens to
the resolution lifecycle independently, so resolution elsewhere closes it. If
the id cannot be resolved, `AttentionCenter.svelte::launchSelectedItem` calls
`centerState.ts::clearAttentionSelection()` (a history *replace* stripping
`attention`/`attention_item`) so a failed direct-open never leaves an invisible,
body-frozen overlay. Audit of open paths:
`docs/archive/plans/2026-07-11-hitl-attention-open-paths-fix.md`.

## Reading the System health strip

A collapsed-by-default disclosure, mounted once above the tablist on Today; the
pipeline (embeddings, semantic features, routing, bandit) is scope-wide.
Collapsed it shows the header, mode chips and an "N needs attention" count of
blocked/degraded stages (dormant excluded). Before the first payload it holds its
row with a skeleton, retired when the request *settles*; with no data and nothing
in flight it renders nothing. Tests asserting on content must render with
`expanded: true`.

Stages are dependency-ordered: Working / Degraded / Blocked / Not enabled.
Eligible impressions with none recorded is a break; an untrained policy is
dormant by design. Grouping and personal ranking stay hidden until a snapshot is
installed; personal ranking shows as preview while the posterior learns in
shadow, then "on" after promotion to canary. The impressions row reads
`verified_impression_total` (scope-level), not `verified_impression_coverage`
(per-decision, near zero by construction). An unavailable ranked projection reads
as waiting and an unapplied order as unapplied — neither implies observe mode.
Semantic degradation names active invalid/missing/dead features; historical dead
letters stay in expanded diagnostics without degrading fully covered features.
Communication semantic coverage marks unsupported source families (task, memory…)
"not applicable", and the active count still reconciles. The taste proposals
panel likewise shows capture retries and unavailable health so an empty queue
does not mask failed extraction.

## Today's Pulse LLM chips

"LLM calls" and "Top model" come from `pulseQueries.ts::buildLlmPulseSql` over the
`llm_calls` compat relation. Totals filter
`COALESCE(provider_attempt_count, 1) <> 0` (excludes bookkeeping
`logical_chunk_summary` rows; NULL = legacy real call). Top model counts only
physical calls with non-empty provider/model identity; the parser rejects an
`unknown` provider fallback. Backend reconstruction: "One row per call" in
`docs/components/magician/duckdb-analytics.md`.

## Morning Edition action contract

`/today` renders the lanes as the Morning Edition
(`lib/today/MorningEdition.svelte`): masthead, Realtime Wire, lead story,
Operations carousel, Reading Room (Morning Brief swipe deck or Broadsheet
columns), § 3 briefings, § 4 completed deliverables, § 5 digest. iOS and Android
follow the same layout and rules:

- For You / Worth cards are removed optimistically. The channel-assist and
  resurfacing helpers return `{ ok: false }` rather than throwing; a failed
  action shows an error and reloads the lane, never a success toast.
- Channel follow-up snooze has no duration (annotation → `classified`, leaves
  Today), so cards offer one "Snooze — hide from Today". Core Today items keep
  `snooze_minutes` via `/today/items/{id}/visibility`.
- "Shouldn't be flagged" (`wrong_classification`) is follow-ups only;
  resurfacing's dismiss reasons are `spam`, `already_handled`, `duplicate`,
  `delegated`, `not_relevant`.
- ⚡ is labelled for what it runs: `Do it` (`approve`) on a follow-up; `Open` on a
  Worth card (records `open` feedback, then opens `source_route` or `open_url`).
- The Realtime Wire starts empty and shows only real feed items, agent updates
  and stream events; a failed 24h count leaves the count unchanged — never a
  made-up number.

### Operations carousel

The Daily Index (`lib/today/TodayNewspaperLedger.svelte`) is a fixed-height
carousel (`TodayOpsCarousel.svelte`); slides are `{ id, title }` entries rendered
through a snippet. Pure rules: `lib/today/opsCarousel.ts`.

- **Economics of Operations** (`TodayOpsEconomicsSlide.svelte`): today's LLM
  spend, delta vs yesterday, model calls, stat grid (Avg / call, Peak hour,
  Coding runs, Memories or Evals passed), 24-hour spend histogram, Analytics link.
- **State of Operations** (`TodayOpsStateSlide.svelte`): Active
  (`running`/`paused`/`planning`), Succeeded (`completed`, never below the
  pulse's completed-today count), Failed as pie + legend (dashed IDLE disc when
  empty); agents Enabled · Active · Total; the 20 most recently updated tasks
  from `taskStore` as one-line rows linking to `/tasks?selected=<id>`, scrolling
  inside the slide (card height 8.5rem, 13.5rem stacked).
- **State of the Crew** (`TodayOpsCrewSlide.svelte`, rules in
  `lib/today/crewQueries.ts`): a rolling 24h window. Per-agent model use from one
  grouped `llm_calls` query (same attempt filter as the pulse); per-agent tasks
  from `GET /api/magician/v3/tasks?limit=100&sort=updated_at&order=desc`, paged
  while the last row is in the window (max 5 pages). Totals: Active, Cost 24h,
  Tasks 24h, Reliability (Σok/Σcalls). One row per agent with activity or active
  now (active, then cost, then tasks) with cost, calls, tasks done, success =
  done/(done+failed), reliability = ok/calls ("—" at zero denominator; ≥95% ok,
  80–95 warning, <80 danger), linking to `/crew/<agent_id>`. Refreshes every
  60 s; a failed read shows Retry and keeps the last good numbers marked possibly
  stale — never substitutes.
- Header shows the current slide title; dots are labelled ("Slide 2 of 3, State
  of Operations"); non-current slides are `aria-hidden` and `inert`.
- Auto-advance every 8 s, wrapping; holds on hover, focus inside, drag, and for
  15 s after a dot/swipe/arrow-key navigation. No auto-advance or animation under
  `prefers-reduced-motion`. The label is announced (`aria-live="polite"`) only
  once the user interacts.
- Horizontal pointer/touch swipe only; a drag never triggers the link under it.
