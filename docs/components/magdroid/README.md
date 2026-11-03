## Unified Work

Empty chats mirror Web's local greeting, four editable prompt starters and three recent conversations, using themed compact cards, scrolling and a single column on narrow screens or at large text sizes; starter taps focus a draft without sending.

History session rows show a compact Working badge for current chat/task activity, refreshed only while the drawer is visible in the foreground. When that 3 s `/chat/activity` poll sees a visible row's working state flip, or a turn finishes in the open chat, the drawer re-reads its current page quietly (rows stay, no spinner, tab/lane/search untouched; at most once per poll interval). Archive, restore and delete hold the row with "Archiving…" / "Restoring…" / "Deleting…" until the server answers, one change at a time; a thread change waits up to 10 minutes because it walks every session on that thread. A failure keeps the row and shows the drawer's error, and an open chat switches to a new session only after its delete succeeds. Thread names stay on one line; hover shows the full name, and the long-press menu leads with it. The top bar's session menu shows the same progress in place of its icon. A thread tap opens its newest session and keeps History on Sessions filtered to that thread (`ui_thread_id`, both lanes) behind a `#name ×` label; New Session there creates in that thread.

The transcript opens on the newest 200 messages; the session read's `has_more` (or, from an older server, a full 200-message page) says older ones exist. Dragging the first message into view reads one older page (`GET .../messages?before=<oldest id>&limit=50`), prepended with duplicates dropped and task lifecycles re-coalesced, while the top row reads "Loading earlier messages…" (a failure offers Retry and keeps the transcript). The first visible message stays where it was, programmatic scrolls (open, follow-to-bottom) never trigger a read, and a page for a chat already left is dropped. Prepended replies load their Steps on appear like any other bubble.

Chat and VibeDev retain the composer’s voice/text mode across a Live call, including after typing during the call, without clearing its draft.

The voice selector persists Dictation, Live or Hands-free and routes the central button to that flow, with distinct microphone, waveform and headphone icons. God mode exposes an auto-approve chip and a themed composer aura; enabling still uses the existing confirmation and server policy. CLI builds rescan source files instead of relying on daemon filesystem events.

Service-account and Attention sign-in choices use compact, rounded-rectangle buttons with 48-dp tap targets; private-viewer controls use aligned, wrapping button rows. Provider registration failures show the server's secret-free explanation.

Chat and VibeDev prefer canonical approval details over transcript copies, recover off-page questions, and bind replies and subsequent runs to the request's original connection. Review-bearing transcript approvals open the focused form; unacknowledged answers stay retryable and closed approvals cannot start another run.

Chat model/harness choices and unsent drafts persist per server, account, workspace and chat. The chat’s three-dot menu opens Tasks (thread-filtered Work), Thread settings (name and saved memory), Files, and Models (per-model and cumulative tokens, cache and cost); unknown usage stays explicit. Dictation and spoken replies carry the initiating chat for billing. File lists show uploads/task artifacts, sizes and confirmed removal of chat copies, retaining task originals under Work; relative links use the originating chat. Quality budget controls accept zero as Unlimited.

Chat composers expose God mode for the current chat and its Settings default; Chat and VibeDev expose backend-wide Super God mode for one hour or until turned off, with confirmation, expiry status and a turn-off action. Mode changes use owner-authenticated endpoints and stay bound to the connection whose settings were shown.

Spoken results lead with findings and keep a short verification qualification; checker diagnostics stay in the answer's disclosure. Chat detail sheets stay bound to the connection that opened them.

Chat and VibeDev consume the server's [model catalog](../magicllm/model-catalog.md).
Chat reads `/chat/models?engine=magician|pi` and sends `model_selection` with an
explicit engine on streaming, queued and concurrent-voice requests. CLI chat
engines retain their native model field. VibeDev reads `/coding/models`, filters
models by engine and sends `coding_choice` on new/follow-up runs. Readiness is
independent of model availability.

Pickers show product provider names and supported native thinking steps/switches.
Chat and VibeDev composer model chips tag the selected thinking level (Auto,
High, …), also in their content description, when the model's thinking mode is
not `none` (chat: Magician/Pi only; `modelChipThinkingLabel` /
`chatComposerThinkingLabel` in `bridge/.../chat/ChatModels.kt`).
Auto uses the model/workload or engine default without escalation; context is
read-only and token allowances stay internal. Preferences persist model/thinking
and coding engine identity. The frozen historical converter imports old selectors
once; typed preferences win. Missing/malformed/unavailable explicit choices remain
saved and block new model-based sends until replaced. There is no profile-list
fallback. Catalog requests are guarded against stale engine/session responses.

Crew editors derive lane and coding choices from routing/model descriptors.
Pins retain canonical handles and can explicitly follow global routing. Slider
drags preview locally and commit the final step on release. Quality uses the same
controls but admits only bounded, priced routes and independent review families;
the existing preview seals exact generated handles.

Chat and VibeDev use themed bordered selector boxes and bounded menus retaining
scroll state through recomposition. Every slider stop is tappable; Auto/selected
labels use rounded squares. VibeDev actions, project/mode choices and icon controls
share the 8-dp rounded-square shape with opaque secondary fills. New run actions
are 48 dp high; Now/Projects tabs are 44 dp with 13-sp labels. A project whose
`health.status` is not `ok` gets a warning/danger glyph in cards and the new-run
picker, its issues in the header and new-run sheet, and a confirmed `git_init`
button (`vibedev/VibeDevProjectHealth.kt`). Colors resolve from
the active day/night palette. Palettes classify as dark with the same Rec. 601
weights iOS uses, and monitor status hues resolve from the theme's
success/warning/danger/info tokens, so derived colours cannot drift from iOS.
Chat's top bar and bottom navigation are opaque `background` bands around the
theme wash, matching iOS's chrome. The Attention tab draws its own top bar as
iOS does — menu left, centred title, History and Refresh right (a clock icon
that flips to a list while history shows) — with no in-page mode row; Work's
"Needs you" view keeps Work's bar and its bound Refresh.

Work replaces the Tasks tab on `codex/unified-work`, preserving its position,
view model, complete task/internal/monitor screens and deep Plan Inspector.
All work, Needs you, Crew and More use the existing Compose theme and 48dp
controls; advanced records push onto a native stack with system Back.
Filled Work task and plan actions pair the accent fill with the palette's
explicit on-accent text/icon token; status-coloured fills choose contrasting
content independently. Busy and disabled states keep the same colour roles.
New work from another Work view opens the ordinary task composer and returns to All work.
Task links use `/work?selected=<id>`; saved `/tasks/<id>` and thread-scoped
selections retain their exact task, execution and Plan focus in native detail.
The whole Work area (Crew, Playbooks, Quality checks, Team routines and every
page they open) uses the same grouped-card design as iOS: a centred Work bar
with Refresh and New work on every view, and one All work · Needs you · Crew ·
More switcher.
Work → More → Goals (`ui/WorkGoalsScreen.kt`, rules in `ui/WorkGoals.kt`) shows the web's goal tree at parity.
All work opens with folding Goals and Playbooks summaries (`ui/WorkLanding.kt`, light goal read limits, loaded once per Work visit); Goals folds sub-goals with Expand all / Collapse all in the Work bar's overflow menu.
As on the web, goals with status `done` or `dropped` are closed: the Goals tree, its counts and the All work summary show only open goals, and an open goal under a closed one moves up to its nearest open ancestor (or heads its own tree) with "Its parent goal “…” is closed." An Open N / Closed M switch lists each workspace's closed goals by their `closed:` date, newest first, with Reopen; each open goal has Mark done. Both change only `status:` and `closed:` through `PATCH /api/magician/v2/notes/front-matter` with the note's read revision, so a note changed since the read is refused (409) and the screen reloads. When every goal the summary read is closed, it says so and links to the Closed list (`/work?view=goals&goals=closed`).
Native details add version-checked Crew definition/model/rules editing, cycle
controls, playbook history/restore and version-checked owner editing (a conflict
keeps the draft), per-focus-area shared documents, grouped-card crew screens
(shared rows in `ui/WorkGroupedRows.kt`), guided Quality and complete advanced specs,
exact artifact review/capture, learning monitoring, operator decisions and the
Team routines, whose sequence dispatches one stage per focus area of each
harness agent from the live definitions (bridge `work/TeamRoutines.kt`).
Frozen URL/header snapshots and lifetime invalidation prevent
old editors or confirmations from following a machine switch. Crew → Quality
retains the selected member in catalog filters and the new draft through
contract review/start. A task or proposal still supplies its recorded owner.
Filter/page changes during a pending write are coalesced to the latest request
and loaded after it finishes, retaining any write error. Local file-preview
save failures show a recoverable error instead of escaping the UI coroutine.
Artifact downloads stream through the frozen client and stop before EOF when
their declared or delivered size exceeds the existing 128 MiB cap, including
responses without Content-Length. The response closes on failure/cancellation;
accepted previews retain their bytes and normal review/open/share controls.

Same-origin re-pairing and changes to the active credentials or workspace also
invalidate old views and reconnect realtime; display-name edits do not. Crew
actions wrap at enlarged text sizes. The JSON editor scrolls independently of
its Cancel/Save controls, keeping the field and controls reachable with the
keyboard open.
Root navigation labels stay on one line at enlarged text sizes; narrow slots
ellipsize visually while retaining the complete accessible label.


The complete migration adds guided Crew creation/editing, model and thinking choices
and global-default clearing, roster leaderboard/hierarchy/pipeline views,
full rules and goal-aware memory inspectors, and eligible isolated-user memory
merge. Playbooks link to real configured members and runtime progress.
Quality adds frozen note/file references, explicit new-work versus source-task
attachment, owner/status filters and independent previous-attempt comparison.
Task/thread and Work deep links retain exact record/run identity; unknown threads
remain read-only, and connection changes invalidate task transports as well as
Work editors. Note links (`/notes?path=…&workspace=…`, the same route on a paired
origin, `magican://notes?…`) in chat, task summaries, asks and notes open the
native Notes screen on that note (`NoteLinks.kt`); another workspace switches to
the one paired machine signed in to it, or shows "This note is in another workspace".
Every other chat link (Markdown replies, structured text/callout/summary copy and
artifact rows) goes through one opener (`rememberChatOutputHandler`,
`ChatSessionDetails.kt`) over the shared rule set `ChatLinks.classify`
(bridge `chat/ChatLinks.kt`, the Kotlin twin of web `classifyRef`): `file://`
reads as its absolute path; a path inside a task's `outputs/` (absolute,
`file://` or `/api/magician/v3/tasks/{id}/outputs/…`) and other `/api/magician/…`
resources are downloaded with the device bearer and opened through the app's
FileProvider, as is an absolute link on the paired origin; other host paths and
workspace-relative files open on the machine (chat-session `outputs/open-file`);
chat links (`/chat?session=…`, `/t/<thread>[/chat|files|settings|models][?session=…]`)
open that conversation, or the thread's latest one (`ChatDeepLinks`), `/history`
opens History; `/t/<thread>?selected=…`, `/work…`, `/tasks…`, `/crew…` go to Work;
Attention, Today, Observe, Settings, `/notes` and `/vibe` open their screens
(`ChatLinks.appDestination`); only other app routes open the machine's web page
in the browser; http(s)/mailto/tel go to the
system. A link it cannot open shows "Can't open link". Paths a reply only mentions
(inline code, bare `/…/file.ext` in prose, `file://`) become links
(`ChatLinks.linkifyMarkdown`).
A thread's URL slug follows its name; its id never changes. A `/t/<key>` link
(slug, old slug or id) resolves to the canonical id through
`GET /api/magician/v2/ui-threads/{key}` before opening (`ThreadDirectory`,
cached; the key is used as the id when the lookup fails). Thread chips show the
thread's name, falling back to its id. New Thread asks for the name first
(Create stays off while it is empty), posts only the name — the server derives
id and slug — and opens the new thread.

Related tools open the connected web console through its normal browser login.
No native bearer or Cloudflare secret is placed in a URL. Only pages that consume
member/task/run filters receive them; global tools remain visibly workspace-wide.
The validated web Back to Work link retains the original item. Remote access to
Work and its listed specialist pages is authorized; existing session and API
permissions remain in force. Native Back retains the originating detail stack.

`make check-magdroid` compiles and runs debug/release unit tests.
`make test-magdroid-work ANDROID_SERIAL=<device>` installs the test build and runs
actual Compose interactions against a device-local HTTP fixture. See
[Unified Work](../unified-ui/unified-work.md) and its
[qualification record](../unified-ui/unified-work-qualification.md) for emulator,
small-screen, enlarged-text and theme evidence and remaining specialist limits.

## Component docs

- [Today — Morning Edition](today.md): masthead, wire, operations carousel (economics · operations · crew), swipe deck, paged broadsheet, data sources.
- [MUIJ graph](muij-graph.md)
- [Plan Inspector](plan-inspector.md): a task plan's graph, steps, history, edit/save, clarifications and approval.
- [Live call panel](live-call.md): aurora orb, timer, rolling captions, the Open mic / Hold to talk control and the mic-level state.

## Realtime hub

The app holds one realtime connection: `RealtimeHub`
(`bridge/.../realtime/`), a process-wide Ktor WebSocket to
`/api/magician/v2/realtime/ws`. Chat (transcript, pending-ask badge, turn
Steps), Tasks, the task progress notification, Today, the thinking map and the
tutor subscribe to it with a `HubFilter` (event-type prefix on the outer and
`AgentEvent` inner type, task / execution / chat turn / agent / session ids,
categories, `user_relevant`, or a predicate). None of them opens a socket or a
held turn stream; contract in
the single event hub plan.

- Reconnects forever: jittered backoff from 750 ms to 30 s, skipped when an
  activity starts (foreground) or the default network becomes available.
- Resumes with `resume_epoch` / `resume_seq` from the last stamped event, and
  dedupes a replay by `(epoch, seq)`. Sends `{"type":"ping"}` every 25 s.
- `AgentEvent` frames reach subscribers unwrapped (`data.event`) as well as
  raw; the tutor draws from the inner event.
- `refs` come from the server when it stamps them and are otherwise read from
  the payload, so filters work against servers that predate the hub.
- Resync: on `hub.resync`, on a reconnect the server could not resume, on every
  reconnect to a server that sends no `hub.hello`, and for a subscriber whose
  512-event buffer overflowed. Each surface re-reads by REST: chat merges the
  open transcript, the pending-ask badge re-reads Attention, a turn's Steps
  re-read `GET .../turns/{turn}/events`, Tasks re-read the list and open detail,
  the progress notification re-reads the run's panel, Today refreshes, and the
  open map is re-fetched.
- A turn's Steps load once by REST, then follow hub events that name the turn
  or name a task / execution an event of that turn named (the client twin of
  the server's chat fan-out).
- Turns outlive the stream (`continue_on_disconnect`). The session read,
  `/chat/active` and `/queue` carry the latest `turn_state`, and
  `chat.turn.state_changed` (an `AgentEvent`) carries every change. After a
  stream drop, relaunch or reconnect a running turn is a live bubble keyed by
  its `chat_turn_id`, Steps backfilled and followed, with Stop; its reply
  replaces it. A stop by the owner reads "Stopped", a watchdog stop shows its
  persisted notice, a failed turn a failed or interrupted line, and an
  unanswered latest turn after a server restart (`turn_state: null`)
  "Interrupted before it finished" (`ChatTurnState.kt`). A dropped stream asks
  `/queue` before calling its turn failed. Realtime voice turns
  (`voice-turn-*`, `voice-request-*`) are left to the call surface.
- Agentic (flat-loop) actions show the moment they start: `AgenticActionStarted`
  opens a running tool row and `AgenticActionExecuted` settles the same row by
  its `call_id` (an older server's completion without one keys on
  `step_id-iteration-timestamp`). An action's `detail` is the row label as-is
  (e.g. "metabase-mcp · execute_sql — SELECT …"). A chat-inline
  `<pack>__call_tool` is labelled "<pack> · <remote tool>" plus a short summary
  of its `arguments_json` (the first sql / query / term, 80 chars); raw
  arguments show only when 100 chars or shorter (`callToolDisplayName`).
- Model calls show while they run: with `correlation.llm_call_id` (newer
  servers) one row per call, "Thinking…" from the request, "Thinking with
  <model>" once routed (latest route wins), settled in place. Older servers
  draw one row per response, as before. A chat call's `llm.*` copy (keyed by
  trace) and its typed copy (keyed by call id) fold into one row once
  `llm.succeeded` names the `llm_call_id`, in either order; the chat-side
  settle's detail (time to first token, usage) wins. Running model and tool rows tick their
  elapsed time (`LiveElapsedText`, `12s`, `3m 4s`) until they settle.
- Steps header (`ActivitySection`): one unwrapped line. Live, it shows "+N" and
  the latest three dots (`ACTIVITY_STAGE_DOT_LIMIT`; the newest running step is
  the last dot and breathes, unless animations are off), a thin ring around the
  chevron, no count and no separate spinner; settled, the step count. Then the
  turn's usage, **Inspect** (the turn's run) and, live, Stop.
- Card times (`CardTimes` / `CardTimeText`, `CardTimesLine`): the live task
  card shows "Created <time> · Updated <relative>" under its meta row, and an
  open Steps card shows "Started … · Updated …" above its rows. The absolute
  part is the time alone today, else month, day and time, in the phone's
  locale and 24-hour setting; the relative part is the platform's "2 min. ago",
  "just now" under a minute. A long press shows the exact updated date-time.
  A part whose time is unknown is left out. Task card: Created is the task
  record's `created_at` once fetched, else the first status update; Updated is
  the later of the record's time (`state.last_progress_at` when present, else
  `updated_at`, never the later of the two since heartbeats move `updated_at`)
  and the latest status update. Steps: the earliest row start or event,
  and the latest row event or call end. The task card's clock ticks each
  second while running and every 30 s otherwise; the Steps card reads the
  shared clock at 30 s.
- The socket closes 5 s after the last subscriber leaves. Voice keeps its own
  control socket.

## Asks answered in place

HITL asks, ask-user questions and approvals are answerable wherever they show,
not only in Attention. One form (`AskAnswerForm`, in `AttentionScreen.kt`)
draws choices, confirm / deny, text, file paths, external actions and forms for
the Attention sheet, the composer ask dock and a task's deep panel. Secrets,
one-time codes and tool / sandbox grants hand off to the same secure sheet
Attention opens; the dock also hands off multi-question forms.
Tool scope choices distinguish one action from the whole integration; MCP/CLI timeline labels name the underlying operation.
Long prompts and decisions scroll separately in Chat, VibeDev, task cards and choice reviews; Review opens the full request, including when compact layouts need a sheet for God-mode grants.
An MCP sign-in (`mcp_oauth`) shows **Not now** (`input_schema.dismiss_label`)
under its sign-in buttons; it posts `{type:"aborted"}`, cancelling the flow and
closing the ask.
The chat and VibeDev composer docks skip a dismiss-only notice (`AttentionItem.isDismissOnly`: a service-health or expired notice), which Attention keeps. Taskless questions match their originating chat session from the request ledger or feed schema, even without a thread stamp. Canonical nested scope stamps and transcript escalation tasks also route delegated questions and their siblings to the dock.

- **Sources** (bridge `attention/AskSurfaces.kt`): the dock merges the
  transcript's open escalation cards with every open Attention feed ask
  (`/feed/attention` requests, approvals and escalations, plus the pending
  `/user-requests` ledger), so the reader answers wherever they are
  (`AskSurfaces.forComposer`). This chat's asks come first: `ui_thread_id` is
  this chat's session, or the task is one of the chat's task cards.
  Relentless and quality-controller asks carry only a task, so the task match
  brings them in. A chat's question belongs to that chat only: when an ask
  names the session that raised it (`chat_session_id`, stamped, in
  `input_schema.context` or scope) it is here exactly when that is this
  chat's session, whatever thread or task they share. Any other ask shows an origin line (`From <chat> · <agent>`
  or `From task …`) and is answered with the dock's own controls. A failed tool
  row in the deep panel's Run timeline shows why it failed under its title
  (two lines, tap to expand; `TaskActivityCalls.failureDetail`). The deep panel
  merges the execution panel's `needs_attention` rows (a flattened feed item
  plus `hitl_request`) with feed asks for the same `task_id`. Rows are
  deduplicated by any identifier they share (correlation, pause state,
  request, item id).
- **VibeDev**: the new-run and follow-up composers use the same dock, showing
  all run turns, project-session questions and workspace God approvals first,
  then every other open ask; Review plan opens the native plan surface.

- **State** (bridge `attention/AskSurfaceViewModel.kt`, activity-scoped): one
  feed for the dock and a task detail pushed over chat, refreshed on every
  live HITL frame and every 30 s while visible. An answer posts through
  `/hitl/{id}/respond` (`AttentionRequest.respondCall`, from the server
  `respond` handle; see [hitl-attention](../magician/hitl-attention.md#respond-handle-respond)),
  hides the ask at once on every in-place surface, and
  resyncs the pending-ask tracker so Attention re-reads too. `already_resolved`
  and a fresh feed that no longer lists an open sheet's ask close it with
  "Answered elsewhere."
- **Review link**: `review_href` / `review_label` are read from the item's
  metadata, then `input_schema.context` (a Relentless review's
  `/work?view=quality&run=…&task=…`). App routes go through Work deep-link
  routing (a task detail closes first); other links open in the browser
  against the paired host.
- **Decisions**: a `choice` ask whose options are exactly `approve` and
  `reject` (the Relentless review's "Approve draft" / "Reject draft") draws a
  Reject / Approve pair and posts the chosen id.

## Push route registration

The app registers by Firebase Installation ID (`onRegistered`), which Firebase
calls only when the registration changes. `AndroidMobilePushRegistration.ensure`
therefore also re-PUTs the stored id (an idempotent upsert on
`/devices/me/push`): forced on launch, pairing, saved credentials, a
notification grant and Settings' Refresh registration; at most every 15 minutes
on foreground. A legacy `onNewToken` rotation triggers the same forced
re-registration without uploading the legacy token.

## Answering asks from a notification

An `attention_requested` push with `notify=true` posts the generic "Magican
needs your input" notification at once (tag = `correlation_id`; legacy
`actions=approve_reject` still adds Approve / Reject). When the push also names
`answer_mode` (`confirm` | `choice` | `text`), `AttentionAnswerNotifications`
(`app/.../push/`) then reads `GET /api/magician/v2/feed/attention/{correlation_id}`
with `MagicianAccess.headers` (3 s connect / 4 s read, on FCM's worker thread)
and, if the generic notification is still up, re-posts the same tag with the
question, the hint and answer actions (`setOnlyAlertOnce`, so it does not ring
twice). Any fetch failure leaves the generic notification.

- **Eligibility** is re-checked on the fetched ask (`notificationAnswerPlan`):
  confirm = `confirmation` (buttons `confirm_label` / `deny_label`, else
  Approve / Reject); choice = single select with 1–3 options, none
  `requires_input` (4+ options stay open-only — Android has 3 action slots);
  text = plain `text`, one `RemoteInput` Reply. Never for a sensitive
  (password / OTP / `sensitive`), destructive, grant or OAuth ask, a form, or
  a mode the fetched ask disagrees with.
- **Answering**: `AttentionActionReceiver` (non-exported) posts
  `/hitl/{correlation_id}/respond` with `channel=android_push`, `input_type`,
  `value` (`confirmation` / `choice` `selected_id` / `text` `value`) and the
  ask's `execution_id` / `task_id` (from `metadata.respond` when present),
  then replaces the notification with
  "Answer sent", "Already answered" (404, 409, or `accepted:false` +
  `already_resolved`) or "Couldn't send your answer" (tap opens the ask). The
  legacy Approve / Reject uses the same path (`android_notification` is
  reserved server-side for verification-code devices). Every answer action
  needs an unlock (`setAuthenticationRequired`, API 31+); PendingIntents are
  immutable except the Reply action, with one request code per ask and action.
- **Lock screen**: Settings → Notifications & permissions → "Show questions on lock screen"
  (off by default, `magican_attention_notifications_v1`). Off posts the
  question version `VISIBILITY_SECRET`, on posts it `VISIBILITY_PUBLIC`.
  `VISIBILITY_PRIVATE` + a public version is not used: SystemUI redacts a
  private notification only when the system setting hides sensitive content,
  and apps cannot set a channel-level lockscreen override. Trade-off: with the
  setting off, the answerable version is not on the lock screen at all (the
  generic notification already alerted); it appears once unlocked.

## Magician at work (summary notification and Follow)

One ongoing "Magician at work" notification (`CodingRunsNotifier`,
`bridge/.../vibedev/`) summarizes coding runs, VibeDev crews and Work tasks,
ranked once on the server (`work_summary`). It replaces the "Coding runs" card
and keeps its channel id (`magician.vibedev.runs`, renamed, low importance) and
notification id 4715, so an owner's channel settings carry over.

- **Sources**: FCM `kind=coding_runs` (same route) while the app is closed —
  the plan's `featured_*`, `action_kind` / `action_id`, `more` and `next` (a JSON
  string, ≤2) on top of the old keys; a payload without `featured_id` still
  renders from `headline` / `detail`. In the app, the Now screen and every
  foreground read `GET /api/magician/v2/work/live`, falling back to
  `/vibedev/runs/live` on 404 (re-asked after 10 minutes).
- **Card**: title = featured item, text = its step or "Needs you", subtext =
  counts ("3 running · 1 needs you"), `InboxStyle` lines (featured, next two,
  "+N more", counts), `setOnlyAlertOnce`. Ongoing while anything is active; a
  just-done / just-failed featured item shows non-ongoing for 2 minutes;
  nothing to show takes it down. Up to three actions: Approve (`approve_diff`,
  answered by approval id through `/hitl/{id}/respond`, unlock required), Open
  (the item's `deep_link`; `magican://work?task=…` opens `magican://task/<id>`,
  a VibeDev run or crew link opens that run or project over Now) and Follow.
- **Needs you**: when `needs_you` rises above the last shown summary, one
  heads-up on `magician.work.needs_you` (high importance, id 4716) names the
  featured item; it clears when nothing needs the owner.
- **Live Updates**: not used — Android 16 promotion needs compileSdk 36 and the
  app compiles against 34; the summary is a plain ongoing notification.
- **Follow**: Follow / Unfollow on the run header, the crew card and the task
  detail menu call `POST /work/follow` `{item_id}` (409 `follow_limit` past
  three, shown as a message) and `DELETE /work/follow/{item_id}`. `WorkFollows`
  keeps the server's list (re-synced on each `/work/live` read) and asks
  `WorkActionReceiver` to register a `work_item` push route (FCM id, item id in
  `task_id`) for each followed id and to remove it on unfollow; routes are
  re-sent with the application route. FCM `kind=work_item` drives
  `FollowedWorkNotifier`: one card per item (tag = item id, id 4717, channel
  `magician.work.followed`, grouped with the summary, at most three, ordered
  per item), updated silently, with Approve and Unfollow; `done=true` shows the
  final state, stops being ongoing and dismisses itself after 10 minutes.
  A machine switch clears follows and their cards.

## Thinking map canvas

`graphLayout` (`bridge/.../thinking/ThinkingGraphLayout.kt`) places the map as
a `parentId` tree drawn left to right in 0..1 space: depth picks the column,
siblings follow `created_at`, and each parent sits centred beside its own
children. Roots share the first column; a missing parent or a cycle falls back
to a root. Every live edge is drawn but never places a node. Web and iOS use
the same rule (drawn top-down).

## Machines (one profile per Magician)

Remote pairing uses the configured server address; help text and fixtures
contain no operator deployment hostname.

The app keeps one paired profile per Magician machine and switches the active
one without re-pairing. Plan:
machine registry and picker §5.

- **Store.** `MagicianAccess` (`bridge/.../access/`) reads `magdroid_access_v2`
  (EncryptedSharedPreferences): `profiles`, a JSON map origin →
  `MachineProfile` (`base_url`, `cf_access_client_id`,
  `cf_access_client_secret`, `principal`, `workspace`, `bridge_pairing_token`,
  `device_id`, optional `machine_name`), plus `active_origin`. Profiles stored
  before `machine_name` existed decode with it null. The rules live in the pure
  `MachineProfileBook` (`MachineProfiles.kt`).
- **Same accessors.** `baseUrl()`, `headers()`, `connectionSnapshot()`,
  `bridgeWebSocketUrl()`, `principal()`/`workspace()`, `isConfigured()` read the
  active profile, so their callers follow a switch unchanged.
- **Migration.** On first access with no v2 marker, the v1 flat keys
  (`magdroid_access_v1`) become the only, active profile. The v1 credential keys
  are then removed (moved, not copied), so a machine forgotten later cannot come
  back from v1. `device_id` stays in v1: one `magdroid-<uuid>` per install,
  presented to every machine.
- **Pairing adds.** `saveEnrollment` adds or replaces that origin's profile and
  makes it active; other machines are kept. It is the only way an origin enters
  the store, after the owner-issued QR exchange and the `/devices/me` proof.
- **Same machine, other route.** The server keeps one pairing per device per
  machine, so pairing a machine by its other route (same-Wi-Fi
  `http://192.168.x.x:3002` vs remote `https://<machine>…`) revokes the older
  profile's token. The exchange response's optional `machine_name` and
  `machine_origins` (the machine's public origin and current same-Wi-Fi
  origin) drive `MachineProfileBook.upsertReplacingSameMachine`: every other
  profile with the same non-null machine name (trimmed, case-insensitive) or
  an origin in `machine_origins` (normalized like profile keys) is removed,
  along with an App Pilot enrollment that route issued. It only removes; a
  never-paired origin still needs its own owner-issued code. An older server
  sends neither field and nothing extra is removed.
- **Switching is not pairing.** `activate(origin)` accepts only an origin that
  already has a profile (`requirePairedOrigin`); a never-paired origin still
  needs that machine's own connection code. Manual recovery (`save`) still uses
  `requireRecoveryOrigin` against the active origin only.
- **Forget and sign out.** `forget(origin)` deletes one profile; if it was
  active, the oldest other profile becomes active, else none. `clear()` signs
  out of the active machine only and leaves nothing active, so it never quietly
  starts using another machine.
- **App Pilot.** The automation enrollment and the pending-Apps retry journal
  stay in v1 (one per install). `apps_automation_origin` records which machine
  issued the enrollment (migration tags an existing one with the v1 origin).
  `hasAutomationEnrollment()` is true only while that machine is active, so the
  device bridge stays idle on other machines. Forgetting or signing out of the
  issuing machine, or re-pairing it with a plain mobile code, drops the
  enrollment and its Keystore key. Pairing a different machine keeps it.
- **Reconnect.** `MagicianAccess.activeMachine` emits when the active origin
  changes. `MagdroidAccessibilityService` closes the bridge socket and, while
  App Pilot is on, dials the new machine. `RealtimeHub` restarts (subscribers
  resync). Settings also calls the chat rebind and uploads the push route to
  the new machine (`AndroidMobilePushRegistration.onActiveMachineChanged`); the
  old machine's route is not removed.
- **Settings → Connection → Machines** (`MachinesSettings.kt`). It lists paired
  machines (the stored `machine_name`, else the registry name when `api_origin`
  matches, else the host), marks the one in use, switches on tap, and has
  Forget. Then it lists registry machines not yet paired (neither origin nor
  name matches a paired profile) with **Pair**, which opens that machine's `ui_origin` (pair
  from its Settings → Mobile devices QR). The registry is
  `GET {active}/api/magician/v2/remote-machines` (`RemoteMachinesClient`, the
  active machine's own headers). A 404 from an older server lists nothing, and
  entries whose origins fail the QR origin rules are dropped.

## Tasks parity with web

Task cards follow web `taskActions`: completed tasks lead with **Result** (when a
summary, non-failure outcome or named artifacts exist) then **Publish to Notes**;
failed/cancelled tasks offer **Reset to Ready** only once they have an execution.
Result opens task detail on the Output act at the Result card. A **↻ Recurring**
chip (cron, `recurring`/`app_recurring` tag, or app `recurring_schedule`) replaces
the raw cron pill and carries the human schedule. Output groups task-scoped
**Deliverables** above a collapsed "Intermediate artifacts & evidence" disclosure
(direct, delegated, persisted). Run activity folds delegated children into
status-accented envelopes with a per-device Grouped/Chronological toggle; History
shows each run's duration. Detail colours come from the theme palette.
`completion_kind: "partial"` on a `completed` task or run reads **Partly done**
(warning tone) with its first `open_items` entry (`tasks/TaskCompletion.kt`).

Off-plan attribution (`ui/TaskDetailScreen.kt` `RunBody`, bridge
`tasks/PlanStepActivity.kt`): when the run's activity rows carry
`plan_step_id` / `off_plan`, each plan-step row in the Run checklist shows its
activity count in a hairline rounded square and opens to that step's rows; an
**Outside the plan · N** group (muted, folded, "Work the agent did between or
beyond the planned steps") follows the steps, and the timeline is titled
**All activity**. Runs without attribution render as before. Same grouping and
test cases as web `planStepActivity.ts` and iOS `PlanStepActivity`.

One row per call on the Run timeline (bridge `tasks/TaskActivityCalls.kt`),
paired on `metadata.llm_call_id` / `metadata.call_id` like web `deriveTimeline`
and titled "Thinking with <model>"; running calls tick their elapsed time. The
detail follows `ExecutionPanelDelta` hub frames, with the 4 s
`/execution-panel` poll as fallback.

## Observe — Command Deck

Observe mirrors the web `/observe` Command Deck. A command header (kicker,
title, "N captures live" / "N meetings live — nothing capturing" / "Quiet"
status, one Refresh that reloads every lane with `refresh=true`) sits over four
tappable KPI cards — **Now & Live**, **Sources on**, **Audio Profiles**,
**Notes & Recents** — which are the only view switchers. The chosen view is
remembered per device; `magican://observe?pane=now|sources|audio|notes` opens a
view directly. The **LIVE** block (in-app capture with Share/Stop screen, Stop,
live transcript, battery `powerWarning`; other server sessions with Stop / Open
transcript) sits above every view.

- **Now**: capture launchpad (Listen, Join as agent, Share screen — explains
  that sharing starts from the live card — and Brainstorm → Thinking Map),
  Upcoming (loading, failures, Join (Me)/Listen/Send bot, active match + Open),
  the `/observe` `reviews` widget slot, and Recent (`GET /meetings` → `recent`,
  newest 30, tap opens the thread).
- **Sources**: *This phone* (editable: microphone and notification permission,
  "Also capture the screen" — also in Settings — and the battery guard) and
  *Web & accounts* (view-only: channel-assist channels, calendar observation,
  enabled continuous sources, browser tabs, startup catch-up; each loads and
  retries on its own).
- **Audio**: meeting and listening profile pickers over
  `GET/PUT /media/preferences` + `/media/providers`, saved on selection
  (optimistic, rolled back on failure; clears that surface's stage overrides
  like the web control).
- **Notes**: Published Notes at iOS parity (search via `q`, 5/10/20/50 per page,
  Previous/Next with "a–b of total", Publish next 25, Promote to memory, Open in
  Notes) and an Audio notes link (the drawer entry stays).

Starting to listen also asks once for `POST_NOTIFICATIONS`; a refusal never
blocks capture and is explained instead. Chat asks for the same permission at
first launch on Android 13 or newer, and skips the dialog when it is already
allowed. Settings → Notifications & permissions shows Allowed or Not allowed,
asks again while Android will still show the dialog, and opens the system
notification page after a denial or when alerts are already allowed. App Pilot
→ Permissions is where accessibility, notification access, and screen capture
are requested. Accepting screen capture promotes the accessibility service to
a media-projection foreground service only when its notification can be posted;
otherwise the app stays open and screenshots use the accessibility fallback.

## Settings

Settings (`ui/SettingsScreen.kt`) keeps only what belongs to this phone, as one
headed list under a search field. Rows, sections and search keywords are indexed
in `ui/SettingsIndex.kt` (`DeviceSettingsRow`, `DeviceSettingsSection`):

| Section | Rows |
|---|---|
| (top) | **Magican settings ›**: the shared web Settings, embedded |
| Voice & audio | Talk to your assistant (wake service + microphone), Wakes on, This phone · speaking voice, Stay available for, Also capture the screen, Conversation mode, Speak replies |
| Notifications & permissions | Notifications (status + allow / open settings), Refresh registration (once allowed), Show questions on lock screen, Home Screen widget, Remote delivery |
| Keyboard & privacy | Magican Keyboard, Reset learned words, Protected apps |
| Connection | Connect to Magician (machines and pairing), Services (health + refresh) |
| Help & about | How to Use, About (Magican Android `versionName (versionCode)`; Magician backend core version, Magician API, Magicutor, Supervisor and Desktop when `/health` reports them; scope) |

The backend row (About and Connection → Services) shows the Magician core
version: `/health` `versions.magician`, else `version` (older servers sent the
magician-api crate version there). Every request that carries
`X-Magician-Device-Id` also carries `X-Magician-App-Version: <versionName>
(<versionCode>)`, so web Settings → Devices shows which build each phone runs.

Search keeps the rows whose title, keywords or section title contain every word
of the query (case-insensitive) and hides empty sections. The Magican settings
row carries keywords for the settings that moved to the web page (budgets,
spend, harness credentials, preferences, storage, voices, trust rules,
workspaces, models, engines, notes, notifications, devices), so searching for
one leads there. When nothing matches, a "No settings match" row offers the
web page. Sections carry bold sentence-case headings (`SettingsSectionHeading`).
A horizontally scrolling chip bar, pinned under the search field outside the
scrolling list, has one chip per drawn section (only matching sections while
searching; hidden when none match): tapping a chip animates the list to that
heading, and a scroll-spy (`activeSettingsSection`) lights the chip of the last
heading above a line near the top, or the last section at the end of the list.

Budgets, Harness credentials, Your preferences (the former Memory row), storage
maintenance status and the shared speaking-voice picker have no native page any
more: they are edited in the web Settings. The composer's voice panels still
use `VoicePersonaControls`; engine pickers still label credentials through
`preferences/HarnessCredentials.kt`.

**Magican settings** (`ui/EmbeddedWebSettingsPage.kt`, `SettingsPage.WebSettings`)
opens `<origin>/settings?embed=1&host=android` in a full-screen `WebView`. The
origin is the active machine's `ui_origin` from `/api/magician/v2/remote-machines`
(matched as the machine picker matches it, else the registry's `is_self` entry;
4 s timeout), else the connection's own origin. Without a paired machine the page
asks to pair first and links to Settings → Connection.

- **Sign-in**: the page uses its normal web sign-in. The paired-device token is
  not a web session (`/auth/session` refuses it), so no Authorization or
  Cloudflare Access header is added. Cookies are accepted and flushed when the
  page closes, so the one-time sign-in and any Cloudflare Access login persist.
  JavaScript and DOM storage are on; file and content access are off; no
  JavaScript interface is exposed.
- **Navigation**: main-frame loads stay in the page for the origin's
  `/settings`, `/login…` and `/cdn-cgi/access/…` paths, for server redirects,
  and while a sign-in is on another origin (Cloudflare Access, its identity
  provider). Anything else opens in the browser. A client-side route from
  Settings to another app page (which never reaches `shouldOverrideUrlLoading`)
  is opened in the browser and the page steps back to Settings.
- **Back** walks the page's history first, then returns to the Settings list.
  A progress bar shows while loading; a main-frame load error shows a Retry.
- Cleartext follows the app's existing manifest policy
  (`usesCleartextTraffic`); origins come from the same validated pairing and
  registry data the API client uses (https, or http to a private address).
- Tests: `ui/SettingsParityContractTest.kt`, `ui/EmbeddedWebSettingsTest.kt`.

Chat reloads its session and engine/profile catalog after mobile pairing, without requiring an app restart.

## Composer slash commands

`/` as the composer's first character opens `/budget`, `/budget default`,
`/engine`, `/run-engine` and `/voice-mode` in the `@` picker's card, filtered
as you type (bridge `chat/ComposerCommands.kt`, a port of iOS
`ComposerCommands.swift`; plan,
engine/model/voice design).
A command opens its options in the same card with a Back row; sending a
composer that holds exactly a command, or a partial one whose every word is in a
single command's name (`/eng`, `/budget defa`), opens it too; any other text
after `/` is sent as a message. Options run client-side —
never a chat message — clear the composer and show a toast (the server's
`message`/`error` on failure).

- `/engine` mirrors the composer's engine/model dropdown. A note row shows the
  choice now ("Now: Magician · GPT-6.1 Sol · High (this device) · everywhere:
  Coding agent"), then the scope row and one row per installed engine. Magician
  and the coding agent (`pi`) open their `GET /chat/models?engine=` catalog;
  other engines with more than `default` open the roster's `models[]` (`CLI
  default` first); a default-only engine is set at once.
  A pick goes through the composer's own `selectHarnessEngine` /
  `selectProfile`, so the model choices reload for a new engine. The scope row
  "Default everywhere" lists engines only and also calls
  `PUT /plane/chat-engine`; the model stays a choice of this device.
- `/run-engine` mirrors Settings → Agentic flows: a note with the run engine
  and model (or "Same as Chat (…)"), "Same as Chat" (`harness_engine: chat`),
  then the engines. Magician lists the `agentic_decision` operation's models
  from `GET /llm/routing` and saves `PUT /llm/routing/agentic_decision
  {selection}`, pinning the run engine to Magician with `PUT /plane/engine`
  first unless runs already resolve to it (its engine, or Same as Chat with a
  Magician chat default). The coding agent lists "Default" (`model_selection:
  null`, its own default) then its catalog, and sends `PUT /plane/engine
  {harness_engine: "pi", harness_model: "default", model_selection}`; other
  engines send `harness_model`.
- A catalog model whose thinking control offers a choice opens a third level
  (`/engine pi <model> `, "Back to models"): Auto then the control's levels,
  the level in force marked for the model in use, else its default. Any other
  model is picked at once, keeping the current thinking level when it offers
  it, else its default. The composer text carries each chosen engine and
  model, and Send never posts an open level (`holdsSend`). A model list that
  is loading or failed shows a note row ("Loading models…", "Models
  unavailable: …"); unavailable models are greyed out.
- `/voice-mode` offers Live, Dictate and Hands-free (the mic menu's modes) with
  the current one checked, and saves `VoicePrefs.setComposerVoiceMode`. Live
  and Hands-free are greyed "Not available here" when the voice catalog has no
  native realtime profile or no hands-free voice.
- `/budget` lists this chat's running/paused tasks from its task cards (newest
  first), then `+$25`, `+$100`, `+1M tokens`, `+50 iterations`, `Unlimited` →
  `POST /tasks/{id}/budget`; `/budget default` marks `GET /budgets/defaults`
  and offers harness `$50/$100/$250/Unlimited` and native `$5/$15/$50/Unlimited`
  → `PUT /budgets/defaults`.

Launcher icons include adaptive foreground and monochrome star variants. The
Android 12 system splash and the app splash use the same star drawable and
background; density-specific raster assets keep the mark within device masks.

Task creation refreshes the current Crew definitions and defaults to the enabled
primary Personal owner. Missing or failed Crew loading cannot silently submit a
fixed owner ID. Crew current-cycle inspection uses the execution panel directly
and keeps execution controls while hiding ordinary task-only actions; bootstrap
and health records and the authenticated web live surface remain reachable.
Program overview reads are limited to four concurrent requests. Work also keeps
paged completed references/manual dependencies, title editing, rechecked thread
moves and separate schedule pause/resume. Schedule edits preserve current limits
and pause state. Create is reachable at the top of the ordinary composer.
Corpus search, internal server filters and page sizes remain operable. Confirmed
internal cleanup reports partial failures and excludes App-managed runs; their
paged history, recurring schedule, exact execution/output and originating App
remain inspectable. See the unified Work qualification record for executed lanes
and physical-device/live-provider limits.
## Preference alignment holds (P6.3)

- **Model.** `attention/AlignmentAsk.kt` reads a hold's public projection
  (`input_schema.alignment` on a `confirmation`) leniently: a malformed block
  is no hold, never a failed row.
- **A private surface (2026-10-11, owner decision).** Like web and iOS, the
  sheet reads the owner-private detail (`attention/AlignmentDetail.kt`,
  `GET /hitl/{id}/alignment?surface=android`): why the act was held and the
  cited preferences. Lock-screen and push text stay generic.
- **Shared Work surfaces.** The composer dock, task deep panel and Attention
  sheet use the same answer form and repository-backed private-detail read.
  Review links, plan approvals and existing themed controls remain available.
- **What the sheet shows.** `ui/AlignmentAnswer.kt` keeps three things apart:
  - the act's typed choices, posted as `Choice(selected_id)`;
  - "Change what Magician remembers" (change or remember a preference). A
    change is two steps: the new wording, then "Save this wording" with the
    server's readback token (`Choice("confirm_wording", token)`);
  - a reply in the owner's words, posted as `Text`, which is never consent.
  When the private read fails, the act's choices and the reply still work.
- **Labels.** The hold's cancel reads "Cancel the action".
- **Combined asks.** When the projection's `also_covers` names other gates,
  the sheet adds that allowing it approves it too (gate names only; their
  reasons stay on the private surfaces).
- **Tests.** `bridge/.../attention/AlignmentAskTest.kt`, `AlignmentDetailTest.kt`.

## Speaking voices

The composer's Replies / Live panels expose searchable voices from the backend
catalog, including FluidAudio/Kokoro; the full list is edited in the embedded
web Settings.
Backend TTS/realtime choices patch scoped media preferences shared across
devices; active calls retain their captured choice. The separate This phone
voice picker (Settings → Voice & audio) lists Android TextToSpeech voices and
stores its choice locally.
Failed backend saves keep the confirmed choice and offer retry.
See the [voice selection contract](../magician/realtime-media-rails.md#speaking-voice-selection).

Voice settings use compact current-value rows for reply/transcription engines, pipelines, stages, realtime models, and personas. `VoiceChoicePicker` opens a checked searchable lazy list. Inside composer settings it replaces the current bottom sheet’s contents, with Back returning to settings; the standalone This phone picker opens one modal bottom sheet. Catalogs no longer expand inline or open large alert dialogs.

Dictation and Hands-free pipelines show **Speaking voice** directly below **Speech model**, restricted to that selected TTS binding. Profile order uses the first available configured provider; explicit model choices switch the persona catalog. Only the web Settings lists every binding. Transcription providers select recognition models and do not have speaking personas.

Changing the speech model filters the already loaded voice catalog; it does not recreate the voice loader or fetch settings for every selection. Empty advertised voice lists use a neutral provider-default message, with no error/Retry. Cancelled panel requests are not failures; a real load failure offers Retry and clears on successful reload.

God-mode approvals appear in every chat composer in the authenticated workspace,
with the originating chat/run, staged diff and every offered grant choice.
Another chat's approval is listed with that origin rather than as this
chat's (`AskSurfaces.forComposer`, as on web and iOS); VibeDev's dock keeps
God approvals first. Session grants remain tied to the origin. Answering anywhere clears the shared
request everywhere; enforcement and grant storage stay on the backend.
Exact shell approvals retain session and always choices under the backend's
[invocation scope rules](../magician/god-mode.md).
Scope pickers retain full paths above short duration buttons; diff/details
scroll separately from decisions. Pending asks remain answerable during chat sends.

Settings → Services → Service accounts and shared MCP question controls use the [browser sign-in](../magician/browser-signin.md) handoff with a native private viewer. Account rows show the engine's status chip and detail, offer Reconnect when connected, pass the row's `endpoint_alias` to launch, and reload on resume and after a launch; `unavailable` rows show a grey chip with no Connect. Managed-bot rows (Google Workspace, Telegram, WhatsApp) open a secure dialog running the `bot/start|input|state|cancel` flow (method, phone, code, two-step password, pairing code, PNG QR, or Google's ticket in the private viewer, with an **Open on computer** fallback), polling every 2 s while signing in.
