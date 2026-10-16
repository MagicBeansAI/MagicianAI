## Unified Work

Work replaces the Tasks tab on `codex/unified-work`, preserving its position,
view model, complete task/internal/monitor screens and deep Plan Inspector.
All work, Needs you, Crew and More use the existing Compose theme and 48dp
controls; advanced records push onto a native stack with system Back.
Filled Work task and plan actions pair the accent fill with the palette's
explicit on-accent text/icon token; status-coloured fills choose contrasting
content independently. Busy and disabled states keep the same colour roles.
New work from another Work view opens the ordinary task composer and returns to All work.
Native details add version-checked Crew definition/model/rules editing, cycle
controls, program history/restore, guided Quality and complete advanced specs,
exact artifact review/capture, learning monitoring, operator decisions and the
company loop. Frozen URL/header snapshots and lifetime invalidation prevent
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


The complete migration adds guided Crew creation/editing, actual profile choices
and global-default clearing, roster leaderboard/hierarchy/pipeline views,
full rules and goal-aware memory inspectors, and eligible isolated-user memory
merge. Program documents link to real configured members and runtime progress.
Quality adds frozen note/file references, explicit new-work versus source-task
attachment, owner/status filters and independent previous-attempt comparison.
Task/thread and Work deep links retain exact record/run identity; unknown threads
remain read-only, and connection changes invalidate task transports as well as
Work editors.

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
- Agentic (flat-loop) actions show the moment they start: `AgenticActionStarted`
  opens a running tool row and `AgenticActionExecuted` settles the same row by
  its `call_id` (an older server's completion without one keys on
  `step_id-iteration-timestamp`). An action's `detail` is the row label as-is
  (e.g. "metabase-mcp · execute_sql — SELECT …"). A chat-inline
  `<pack>__call_tool` is labelled "<pack> · <remote tool>" plus a short summary
  of its `arguments_json` (the first sql / query / term, 80 chars); raw
  arguments show only when 100 chars or shorter (`callToolDisplayName`).
- Steps header (`ActivitySection`): one unwrapped line. Live, it shows "+N" and
  the latest three dots (`ACTIVITY_STAGE_DOT_LIMIT`; the newest running step is
  the last dot and breathes, unless animations are off), a thin ring around the
  chevron, no count and no separate spinner; settled, the step count. Then the
  turn's usage, **Inspect** (the turn's run) and, live, Stop.
- The socket closes 5 s after the last subscriber leaves. Voice keeps its own
  control socket.

## Asks answered in place

HITL asks, ask-user questions and approvals are answerable wherever they show,
not only in Attention. One form (`AskAnswerForm`, in `AttentionScreen.kt`)
draws choices, confirm / deny, text, file paths, external actions and forms for
the Attention sheet, the composer ask dock and a task's deep panel. Secrets,
one-time codes and tool / sandbox grants hand off to the same secure sheet
Attention opens; the dock also hands off multi-question forms.

- **Sources** (bridge `attention/AskSurfaces.kt`): the dock merges the
  transcript's open escalation cards with Attention feed asks
  (`/feed/attention` requests, approvals and escalations, plus the pending
  `/user-requests` ledger) whose `ui_thread_id` is this chat's session, or
  whose task is one of the chat's task cards. Relentless and quality-controller
  asks carry only a task, so the task match brings them in. The deep panel
  merges the execution panel's `needs_attention` rows (a flattened feed item
  plus `hitl_request`) with feed asks for the same `task_id`. Rows are
  deduplicated by any identifier they share (correlation, pause state,
  request, item id).
- **State** (bridge `attention/AskSurfaceViewModel.kt`, activity-scoped): one
  feed for the dock and a task detail pushed over chat, refreshed on every
  live HITL frame and every 30 s while visible. An answer posts through
  `/hitl/{id}/respond`, hides the ask at once on every in-place surface, and
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
  ask's `execution_id` / `task_id`, then replaces the notification with
  "Answer sent", "Already answered" (404, 409, or `accepted:false` +
  `already_resolved`) or "Couldn't send your answer" (tap opens the ask). The
  legacy Approve / Reject uses the same path (`android_notification` is
  reserved server-side for verification-code devices). Every answer action
  needs an unlock (`setAuthenticationRequired`, API 31+); PendingIntents are
  immutable except the Reply action, with one request code per ask and action.
- **Lock screen**: Settings → Permissions → "Show questions on lock screen"
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

Off-plan attribution (`ui/TaskDetailScreen.kt` `RunBody`, bridge
`tasks/PlanStepActivity.kt`): when the run's activity rows carry
`plan_step_id` / `off_plan`, each plan-step row in the Run checklist shows its
activity count in a hairline rounded square and opens to that step's rows; an
**Outside the plan · N** group (muted, folded, "Work the agent did between or
beyond the planned steps") follows the steps, and the timeline is titled
**All activity**. Runs without attribution render as before. Same grouping and
test cases as web `planStepActivity.ts` and iOS `PlanStepActivity`.


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
allowed. Settings opens on Permissions, which shows Allowed or Not allowed,
asks again while Android will still show the dialog, and opens the system
notification page after a denial or when alerts are already allowed. App Pilot
→ Permissions is where accessibility, notification access, and screen capture
are requested. Accepting screen capture promotes the accessibility service to
a media-projection foreground service only when its notification can be posted;
otherwise the app stays open and screenshots use the accessibility fallback.

## Storage maintenance visibility

Settings includes Automatic maintenance with Channel Assist/Feed state, a user
message and last completion. Its authenticated `/storage/maintenance` read runs
every ten seconds while Settings is resumed. Failed reads clear stale status;
leaving the surface cancels polling and in-flight work.

Chat reloads its session and engine/profile catalog after mobile pairing, without requiring an app restart.

## Composer slash commands

`/` as the composer's first character opens `/budget`, `/budget default`,
`/engine` and `/run-engine` in the `@` picker's card, filtered as you type
(bridge `chat/ComposerCommands.kt`, a port of iOS `ComposerCommands.swift`;
plan). A command
opens its options in the same card with a Back row; sending a composer that
holds exactly a command opens it too. Options run client-side — never a chat
message — clear the composer and show a toast (the server's `message`/`error`
on failure). `/engine` sets this device's engine × model, and its scope row
also calls `PUT /plane/chat-engine`; `/run-engine` calls `PUT /plane/engine`;
`/budget` lists this chat's running/paused tasks from its task cards (newest
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

## Your preferences (Settings → Memory)

`ui/PreferencesPage.kt` over `bridge/.../preferences/OwnerPreferences.kt`
(`/preference-alignment/intentions` and `/history`, `surface=android`): the
owner's preferences exactly as checks see them. Filled preference actions use the
installed theme's accent-fill and on-accent tokens. Add (instruction, goal or
taste), reword, confirm an inferred one as the owner's own, retire, forget
(wording erased from the history, a dated entry kept) and per-preference
history. Every change but add names the revision on screen; a stale one
saves nothing and the list reloads with the server's note. A load failure
uses the app's `FailurePane` (classified, with Try again), and a refresh that
fails keeps the list with an `InlineFailure`; a preference stored as
`key: {json}` reads as a title and fields (`presentStatement`, the web's and
iOS's rule). Tests:
`bridge/.../preferences/OwnerPreferencesTest.kt`.
- See [preference-alignment.md](../magician/preference-alignment.md).

## Speaking voices

Settings → Voice and audio and the composer's Replies / Live panels expose
searchable voices from the backend catalog, including FluidAudio/Kokoro.
Backend TTS/realtime choices patch scoped media preferences shared across
devices; active calls retain their captured choice. The separate This phone
voice picker lists Android TextToSpeech voices and stores its choice locally.
Failed backend saves keep the confirmed choice and offer retry.
See the [voice selection contract](../magician/realtime-media-rails.md#speaking-voice-selection).

Voice settings use compact current-value rows for reply/transcription engines, pipelines, stages, realtime models, and personas. `VoiceChoicePicker` opens a checked searchable lazy list. Inside composer settings it replaces the current bottom sheet’s contents, with Back returning to settings; standalone Settings opens one modal bottom sheet. Catalogs no longer expand inline or open large alert dialogs.

Dictation and Hands-free pipelines show **Speaking voice** directly below **Speech model**, restricted to that selected TTS binding. Profile order uses the first available configured provider; explicit model choices switch the persona catalog. Only global Settings lists every binding. Transcription providers select recognition models and do not have speaking personas.

Changing the speech model filters the already loaded voice catalog; it does not recreate the voice loader or fetch settings for every selection. Empty advertised voice lists use a neutral provider-default message, with no error/Retry. Cancelled panel requests are not failures; a real load failure offers Retry and clears on successful reload.
