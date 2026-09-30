# Magios (Magican iOS Companion)

Magios is the ubiquitous native iOS companion for Magican. It connects to the
Magician backend service while presenting the configured agents by their own
names.
It bypasses traditional "app" mechanics by relying entirely on OS-level integrations: Siri, Shortcuts, the Action Button, and the Dynamic Island.

**Version:** 0.3.0 (build 202) - see `CHANGELOG.md`.

Version 0.2.3 registers only the canonical `magican://` scheme and uses
the operator's `connect.<zone>` host in current enrollment guidance. It retains 0.2.2's complete
constrained wake spelling support and canonical Magican display/Siri identity.

Product-facing defaults used before enrollment or while offline live in the
generated `Shared/ProductIdentity.generated.swift`. Its source is the root
`data/presentation_identity.json` manifest; regenerate it with `make
presentation-identity-codegen` rather than adding another Swift fallback.
The chat composer's Ask and Plan placeholders observe the scoped primary agent's
display name (or first nonempty alias) from the foreground-refreshed identity,
falling back to the product name, **Magican**.

## Architecture
- **Main App (`Magios`):** A full Today + chat + attention client that ports the web unified-ui
  experience (see "Today, Chat & Attention" below), plus configuration. It launches directly
  into the persisted tab (Today by default), without an artificial brand-splash delay.
- **Siri Intents (`MagiosIntents`):** Exposes "Ask Magician" to the OS.
- **Widgets (`MagiosWidgets`):** One **Magican at a glance** widget with Home/Lock
  Screen size variants, plus the task, observation, and ambient Live Activity
  renderers. The three ActivityKit types remain internal lifecycle boundaries,
  not three user-facing products or settings.
- **Share Extension (`MagiosShare`, “Magican”):** Durably adds shared text, URLs, images, and files
  to Chat.
- **Action Extension (`MagiosAction`, “Magican Assist”):** Adapts to one selected text, webpage,
  or image: Writing Help for text; page-aware Summarize/Ask Sam for URLs; and Tutor, Chat, or
  on-device-OCR Writing Help for screenshots/photos. URL actions atomically create and enqueue a
  task-backed Internal execution (no planning pipeline), use headless browser first with headed
  fallback, and return a **Working / Check Status in Magican** handoff that selects the Internal tasks
  lane and opens the exact run. These backing records do not appear as user commitments in Tasks.

## Notes

**Menu → Notes** opens the Magician notes library in the app. The file explorer
is a left drawer. The note itself is the Markdown stored in the workspace, read
and written through `/api/magician/v2/notes`. Creating or deleting a note
refreshes the search index on the server. Android uses the same library.

Published task pages open in that library at the note's file path.

`make test-ios-live-notes` verifies Menu → Notes opens the in-app library on an
enrolled physical iPhone and does not edit notes. Set `MAGIOS_DEVICE_ID` and
`MAGIOS_LIVE_TEST_HOST`.

Notes are Markdown files in the workspace notes folder (fresh default
`$MAGICIAN_ROOT_DIR/MagicanNotes/spaces/<principal>/<workspace>`, normally
`~/MagicianNotes/MagicanNotes/spaces/<principal>/<workspace>`). Magician reads
and writes that folder itself. See the
[Notes provider guide](../docs/components/magician/notes-provider.md).

## Voice Notes

In Chat, tap the mic once to start and again to stop, or press and hold it and
release when finished. By default this is ephemeral dictation: the transcript
fills the composer and enters the normal three-second cancelable auto-send flow,
then the temporary recording is removed.

To retain recordings, open the composer's Voice settings → Dictation and enable
**Keep dictation recordings**. The mic visibly discloses that saving is active.
The original audio is copied off the main thread into a protected,
backup-excluded, size-bounded iPhone outbox and saved independently as a dated
Notes page under `Audio Notes/YYYY-MM-DD/`, alongside its `.m4a`, transcript,
source, duration, and capture timestamp.

Audio Notes use the scoped default Notes provider and configured fallback. A
background file-upload session retries each row independently; permanent or
exhausted failures remain under **Menu → Audio Notes** without blocking later
notes. That surface lists saved and local records and supports transcript
search, playback, pagination, Retry, and confirmed Delete/Discard. In-flight or
scheduled background rows cannot be discarded until their task settles. The local
copy is deleted only after a non-redirected response contains a matching durable
server receipt. This opt-in archive never applies to Ambient Dictation turns or
Thinking Map speech capture.

## Live Thinking Map

Thinking Map is the iPhone-first brainstorming surface, available from
**Brainstorm an idea** in Observe, the app drawer, `@brainstorm` in Chat, and
contextual share actions. Each idea is an independent durable map with a
spatial Canvas plus Focus and Outline lenses, typed or spoken capture,
branching and cross-links, editing/Undo, library lifecycle actions, and
Markdown Harvest. The canonical backend is the only active data path; a
one-time importer preserves maps created by the earlier local prototype.

When Magician is reachable, the dedicated Loom facilitator receives the
bounded graph and active lineage and proposes a deliberately small frontier.
Continue and Break open carry the currently selected node in the interpretation
request, so a fast branch selection cannot race back to the root. The local
selection also stays pinned while an older authoritative projection is in flight;
ambient Listen attaches a live voice session and refreshes from realtime map
updates with a slow safety poll. Nodes can be promoted through governed Task or
Memory actions, while Magican Assist can seed a new map or append to the current
one from shared text and webpages. Demo and UI-test launches use an isolated
in-process canonical transport, so the full navigation remains testable without
the backend. See the [Magios feature guide](../docs/components/magios/README.md#live-thinking-map)
and [canonical client architecture](../docs/components/magios/thinking-map-canonical.md).

## Today, Chat & Attention (web parity)
The primary surfaces are native SwiftUI ports of the web unified-ui experience.
Cards, notices, badges, swipe actions, and filled controls share semantic theme
tokens for their surfaces, borders, status hues, and contrast-safe foregrounds;
feature views should not introduce fixed white/gray/status colors for ordinary UI.
Settings keeps appearance as a compact System/Day/Night segmented choice and
opens theme-family selection as a visual sheet: every option previews its real
resolved background, accent and text palette rather than showing a name alone.
All 22 variants use the same four typography roles as Web: Outfit for brand
marks, each theme's display family for navigation chrome, its body family for
ordinary text, and its mono family for technical data. The app bundles every
referenced family, so theme changes do not depend on a font download.
The Settings roadmap is likewise pending-only: shipped capabilities move to
How to Use and release notes instead of lingering as checked or stale rows.

Today app widgets follow the core lanes, briefings and Activity, matching Android.
Optional widget/pinned-app regions keep a concrete SwiftUI lifecycle host while
empty so their initial load starts on a cold launch.
In Night mode, the composer's Do · Ask selection uses an accent tint and light
text, shared with Android, rather than a bright inverted text-color fill.

Implemented and compile-verified (behavior confirmed in the live shakedown):

- **Today:** the web operating surface adapted for a narrow, touch-first screen: all-clear and
  summary metrics; local-calendar dates and exact snooze targets; the full LLM/task/coding/
  memory/eval pulse with hourly spend; space-grouped, cursor-paginated lanes; mark-seen and
  native destination routing; paginated What Changed; rich Worth a look briefs, originals,
  recommendations, contextual actions, and telemetry; channel follow-up hints, dismissal
  reasons, live message/evidence reads, and absolute local received times; app-scoped optimistic
  card mutations with exact failure rollback; optimistic hide with Undo and restore; native rendered
  briefings; and searchable/mutable durable Activity. Published briefing `muij_document` payloads
  are validated and rendered as themed SwiftUI stacks, cards, metric grids, tables, activity feeds,
  progress, notices, charts, media links, and Markdown instead of serialized JSON. The read-only
  surface bounds depth and component count, rejects malformed/duplicate structures, preserves
  unknown display components with a visible forward-compatible fallback, and never dispatches a
  control without a mobile authority contract. Non-MUIJ JSON receives a human-readable bounded
  key/value presentation. Today,
  follow-ups, pulse, Activity, and published surfaces refresh on the same polling/realtime
  lifecycle as web while retaining last-good secondary data during partial failures. Core Today
  navigation uses a fixed native top bar with the drawer and refresh controls, so those controls
  remain available while the greeting, status summary, and work lanes scroll independently. Cards
  swipe left for Dismiss and right for Open without firing a tap on swipe release; compact
  inline Create Task and timed Snooze controls are deliberately not duplicated on those rails.
  Worth a look cards use their own feedback rails (left Dismiss, right Mark useful) and a compact
  Actions button whose bottom sheet exposes Acknowledge plus the same contextual capabilities as
  full detail. Create reminder uses EventKit to write a real Apple Reminder with the selected due
  time, note, and alert, durably records the native identifier, and then opens Reminders; it is
  never implemented as Create Task. Failed receipt retries preserve the same reminder and
  idempotency key. Dismiss, Snooze, Useful, Acknowledge, and Attention HITL responses remove their
  card and update counts/badges before their per-card request completes. Same-card duplicate actions are
  coalesced, unrelated cards remain interactive, and a bounded shared tombstone keeps a stale Today
  or Attention refresh from resurrecting a just-resolved card without hiding a later reissue. A
  failed request restores the exact row and projections with a visible error. This is an in-memory
  optimistic queue, not a force-quit-safe
  offline outbox; provider side effects such as sending a reply remain success-gated. Useful,
  Dismiss, and neutral Acknowledge outcomes carry the exact server-issued
  decision/candidate/revision attribution and a unique event id into the shared attention-learning
  path; stale, cross-lane, or unselected bindings are omitted rather than guessed, and Snooze stays
  lifecycle-only. When the canonical delivery page exactly reconciles with the native rows, iOS
  preserves its server order and frozen delivery identity. A card must remain at least half visible
  for the response-owned dwell interval before iOS records an impression; the verified receipt can
  then accompany feedback. Adapter-declared provider actions such as Reply and Like use the same
  attribution path, and learning receipts are retained for diagnostics without adding success noise.
- **Chat:** SSE token streaming (WS-echo de-duplicated), GFM markdown (MarkdownUI), tool-result
  cards + content blocks with visible Preview/Open/Copy/Share-or-Save actions, inline
  authed-image previews (`AuthAsyncImage`), session lifecycle
  (new/clear/archive/delete), attachments with staged preview chips, per-message delete, the
  queue inspector with Copy/Delete, swipe deletion, Clear all, Stop turn, timestamps, and
  failure-safe mutations, a two-line thread/session navigation title with trailing session actions,
  and scroll-lock with a viewport-pinned jump-to-latest control. A subtle service-state dot replaces
  the labelled Chat health pill. Chat remembers the last session opened on this device per server
  origin and principal/workspace scope, validates that exact session on launch, and only creates a
  replacement after confirmed absence; a transient offline launch never creates a duplicate.
  Live `ChatMessageReceived` events must name the selected session in both their envelope and
  saved message. Both sides of another device's conversation appear once; this device's user
  echo is reconciled by turn id. Legacy orchestrator `MessageCompleted` summaries do not become
  chat replies. A superseded SSE stream cannot update the replacement transcript, even when
  the same session is reopened, and reply-echo suppression applies only to its own turn.
  Chat and ambient-dictation streams transfer accepted turns to the server with
  `continue_on_disconnect`, so an iOS suspension or process loss is recovered from the canonical
  transcript/realtime feed without replaying the turn. A late canonical reply replaces any partial
  SSE preview for that turn. Explicit Stop remains a server cancellation.
  Installations upgrading without a remembered selection fall back to the newest active Personal
  session across all threads. Each assistant response owns a collapsible activity timeline with
  its coalesced step count and latest-five expanded preview; **All** opens the full chronological
  log, while native **Run**, **Stop**, file open/reveal, and markdown-link actions route to the same
  execution and chat-session targets as web. Live replies tail the canonical
  `chat_turn_id` projection while historical messages hydrate it lazily; duplicate and out-of-order
  durable events are normalized before the shared `ChatTurnActivity` mapper runs. A single projector
  handles persisted and realtime task/tool/attachment/escalation content: task updates coalesce to
  the latest lifecycle card and preserve summaries, synthesis state, output artifacts, controls, and
  Inspect Run after reload rather than falling through as empty text. Escalation
  cards resolve only after a successful HITL response and retain their actions on failure.
- **Composer:** a unified card (input + tools + Send inside one surface) with an `@`-mention
  picker for agents, tools, personalities, and iOS-supported features. `@brainstorm <thought>`
  opens a fresh Live Thinking Map rooted in that thought (bare `@brainstorm` opens capture), while
  the dedicated Loom facilitator uses graph context, relevant memory, and bounded realtime activity
  rather than a predefined flow. Each map receives a distinct persistent Loom session in the
  **Brainstorming** thread; older idea sessions stay active until explicit archive/delete. The composer also provides Do/Plan modes, an
  adaptive profile chip; and a Tutor screenshot picker.
- **Tutor screenshot overlay:** select a screenshot/photo and share it to Magican Assist,
  ask a question, and watch the validated tutor shapes draw progressively over the static
  screenshot. Narration uses the selected iOS TTS engine and reveals each drawing from the
  actual audio playback-start callback. Guides support Replay with narration, Keep Showing,
  automatic expiry, Dismiss with server cancellation, and Ask Again on the same screenshot.
- **Attention:** lane-tabbed, paginated requests, approvals, escalations and failures,
  with the existing modal for every HITL input type (choice / multi-choice / confirmation /
  password / guidance / diff-approval / file-path / external-action / tool-authorization).
  Refresh reads both `/feed/attention` and the scoped `/user-requests` ledger, so a chat
  question is answerable before a feed card exists. Ledger rows reuse the same response
  forms and `/hitl/{correlation_id}/respond` route with `source: user_request`; correlation
  matching prevents a later feed projection from adding a second card. Failed reads retain
  the last snapshot and show a retryable error instead of an empty all-clear. The full read
  repeats on entry, foreground resume and relevant realtime events; pagination retains the
  server cursors. The scoped realtime socket accepts equivalent text
  and UTF-8 binary event frames, clears failed socket ownership before reconnecting, and suppresses
  delayed reconnect after the view disconnects, so a stale transport cannot silently turn the
  inbox into a pull-to-refresh-only surface.
- **Tasks:** native port of the web `/tasks` workspace — two lanes (Tasks / Internal tasks), the
  same preset filters (All / Inbox / Today / Overdue / Running / Completed), derived tag chips,
  search, and a per-status count ledger; task cards with capability-driven Pause/Resume/Steer/
  Stop controls for live executions and the web card-action matrix (View/Answer/Review Plan,
  PrePlan + Run Now, Run Plan, View Execution, Reset to Ready). A compact Actions sheet contains
  completion, description, priority, custom due date, plan approve/reject/replan, tags, recurring
  schedule/retention, cancel, and delete with optional folder cleanup; a New Task form (agent,
  priority, due date, tags, recurrence); a HITL "Answer" badge;
  realtime updates and a 5s completion grace; and the same full-screen detail workspace for both
  normal and internal tasks. Cross-surface and `magican://task/<id>` links resolve both lanes,
  including older internal tasks outside the newest loaded page, then select the correct lane and
  open that task directly. Task cards also provide state-aware swipe rails: swipe right to
  Complete or Mark not done; swipe left to Cancel when applicable or Delete. Cancel and Delete
  require confirmation, while planning/run/reset stay visible and execution-authorized
  Pause/Resume/Steer remain inline. Reset settles any active root, preserves run history, and
  reports backend failures on both the card and full task workspace.
- **Task detail workspace:** a phone-sized, horizontally scrollable five-tab layout preserves
  the web execution panel rather than stacking it into an unreadable page: **Overview** (markdown
  brief, attention, metadata, tags and IDs), **Run** (execution selection, live state, questions,
  responsibility/delegation, steps, activity and shell/debug internals), **Output** (result,
  synthesis state/retry, deliveries and tappable authed artifacts), **Plan** (status/actions,
  questions, steps and markdown), and **History** (all executions with per-run outputs and
  persisted internal artifacts). The view tolerantly combines `/execution-panel`, `/details`,
  `/outputs`, and `/plan`, retaining useful partial data if one optional endpoint fails.
  Verdicts distinguish all nine lifecycle meanings, including resumable Paused and terminal
  Archived. Run shows the newest 200 events with the complete event count and keeps run-level
  totals and duration based on the full selected snapshot.
  Selecting a historical run changes inspection only; controls always target the authoritative
  active root. Chat task cards continue to expose controls inline when their realtime payload
  includes an execution ID. Steer uses a native composer with a 4 KiB UTF-8 limit; Stop requires
  destructive confirmation. Native controls mutate only an active root,
  share one execution-scoped busy/invalidation coordinator across duplicate views, refresh when
  host task state changes, and allow 45 seconds for cooperative Pause settlement. Post-action
  reconciliation runs after both success and failure; a failed refresh keeps the prior snapshot
  disabled and presents an explicit retry instead of replacing it with an empty control state.

## Networking
Magios does not run its own LLM. It routes all intelligence requests to the core Magician backend.
The app contains no customer endpoint or Cloudflare credential. In Magician
Settings, **Mobile devices → Connect iPhone** creates a five-minute,
single-use QR. Scanning it configures the deployment's runtime public origin,
canonical scope, a revocable iPhone credential, and—when the deployment uses
Cloudflare Access—the current outer service credential. Magios verifies the
ordinary authenticated `/devices/me` route before atomically replacing its
Keychain profile. Camera/Safari `magican://connect` links and the in-app scanner
use the same confirmation flow. Each request resolves one immutable profile
snapshot before attaching scope and credentials, so a concurrent re-enrollment
cannot mix authority from two profiles. Recovery credentials left by older
releases migrate from shared defaults into Keychain on first use; the plaintext
copy is retained if the write fails and removed only after exact Keychain
read-back succeeds.

The host derives the advertised origin from `mobile_access.public_origin`, then
`MAGICIAN_MOBILE_PUBLIC_ORIGIN`, with
`MAGICIAN_DEVICE_PUBLIC_ORIGIN` retained only as a migration fallback. The
Cloudflare setup derives that value from its configured mobile hostname. The
exact enrollment exchange is the only Access bypass; all ordinary REST and
WebSocket calls require both the outer Access key and the per-device identity.

The same device identity registers an APNs application token and task-scoped
ActivityKit update tokens. The Home Screen widget reduces the canonical Today
response (Needs You → active work → Talk) and caches the last truthful result in
the App Group. Generic Attention pushes contain no prompt or secret; a tap opens
the exact correlation. Task Live Activities receive remote progress/end updates,
while observation and ambient microphone activities stay local-only. Settings
offers one **At a glance** permission/registration surface rather than separate
widget, notification, and Live Activity choices.

Task activity teardown cancels and drains token registration before deleting
the server route, so a late PUT cannot resurrect an ended activity. Unbound
activities never consume another run's realtime event; failed or uncorrelatable
Siri dispatches settle immediately instead of waiting for the watchdog. Every
Siri invocation owns its lifecycle, so late callbacks from a superseded request
cannot bind or tear down the newer activity. A burst
of background notifications also shares one canonical Today refresh. The app
and WidgetKit extension compare and replace their cache through one
OS-coordinated App Group file, so a slower response from either process cannot
replace the newer snapshot already shown. Foreground alerts are presented
immediately while that refresh runs separately. After the server accepts a
task-scoped ActivityKit route, the app releases the silent keepalive and local
socket; the Live Activity and remote updates share the same 20-minute stale
deadline, and terminal state is applied as one ordered end operation.

Remote delivery additionally needs an Apple push key for bundle id
`com.magicbeans100x.magican`. Keep the `.p8` file outside git and configure
`MAGICIAN_APNS_KEY_ID`, `MAGICIAN_APNS_TEAM_ID`,
`MAGICIAN_APNS_PRIVATE_KEY_PATH`, and (only when the bundle topic differs)
`MAGICIAN_APNS_TOPIC`. Without those values, local realtime activity updates and
widget polling continue unchanged and remote registration reports that host
setup is unavailable rather than claiming readiness.

## Development
The checked-in `Magios.xcodeproj` is the build and test input. Routine Make targets never
regenerate it. `project.yml` remains a synchronized manifest/reference for project settings;
when project settings change, update both files intentionally. Its explicit
`productName: Magican` is load-bearing: XcodeGen otherwise writes the internal target
name into the shared scheme even though `PRODUCT_NAME` builds `Magican.app`. Open
`Magios.xcodeproj` and build directly.

### Tests

Run `make test-ios` from the repository root to run the blocking `MagiosTests` unit suite against
the existing checked-in Xcode project. The UI smoke suite runs separately through
`make test-ios-ui`. Both runnable lanes first invoke the idempotent
`setup-magios-vosk` prerequisite, which fetches and checksum-verifies the large
gitignored framework/model artifacts on a fresh macOS clone. Neither command
regenerates the project. `make verify-magios-vosk` provides the corresponding
read-only preflight used by `make check-all`. The unit-test scheme gathers code
coverage by default. To use a different installed simulator, pass
`IOS_TEST_DESTINATION`, for example:

```sh
make test-ios IOS_TEST_DESTINATION='platform=iOS Simulator,name=iPhone 16 Pro'
```

For an already enrolled **test iPhone**, `make test-ios-live-chat` runs one
opt-in XCTest against its real backend. Set `MAGIOS_DEVICE_ID` and
`MAGIOS_LIVE_TEST_HOST` (hostname only); use `MAGIOS_DERIVED_DATA` and
`MAGIOS_SPM_DIR` to reuse the device build on the external build volume.
It verifies the host in Settings before sending a unique chat prompt, then checks
the reply after an app restart. It uses one build job, no retries or fixtures,
and retains screenshots in a timestamped `.xcresult` under DerivedData
(`MAGIOS_LIVE_TEST_RESULT` overrides that path). This invokes the configured
inference provider and leaves synthetic messages in the test runtime. The live
test skips in ordinary test runs unless its explicit host opt-in is supplied.
Keep the physical iPhone unlocked for XCTest to start. The live command enables
device signing for the test runner; the usual simulator test targets remain
unsigned. An active Mirroring session does not satisfy Xcode's unlock preflight.
If the runner times out enabling automation mode, inspect the physical phone:
iOS can separately request its passcode for **XCTest → Enable UI Automation**
even when CoreDevice reports no unlock passcode required. Complete that system
prompt on the phone before rerunning; another build does not resolve it.

`make test-ios-live-recovery` uses the same device and host settings. It sends a
long, unique turn with `continue_on_disconnect`, terminates Magican while the
turn is active, relaunches the app, and requires the canonical reply to appear
exactly once. The result bundle records the unique marker and termination time;
pair it with the selected backend's transcript when recording acceptance.

`make test-ios-live-today` uses the same device/host settings for a read-only
cold-launch check: the test workspace's Brainstorm canvas and Learning review
queue must load below Activity. It retains a screenshot and does not send chat
messages or invoke widget actions.

`make test-ios-live-appearance` uses that same opt-in to capture Longhand's
Settings, Today and Chat on the physical iPhone in Day and Night, restoring the
original appearance afterward. The images allow comparison of actual rendered
surfaces with Android rather than relying only on matching palette constants.

`make test-ios-live-upload` shares a synthetic PDF from Safari through the
installed Share Extension, the app's HTTPS upload and chat reply, then a cold
restart. It requires `MAGIOS_LIVE_UPLOAD_URL` (reachable by the iPhone),
`MAGIOS_LIVE_UPLOAD_FILENAME` (filename prefix) and `MAGIOS_LIVE_UPLOAD_REPLY`
in addition to the device/host opt-in. Use a fresh unique value for each run.
Serve only a synthetic fixture; its
expected reply belongs in the file, not the chat prompt. The test verifies the
selected host before sharing, refuses to overwrite a draft, waits for upload
completion before sending and checks the reply after restart. Independently
compare the received Linux file bytes with the fixture. It leaves the fixture
in Safari history and synthetic chat/upload artifacts in the test workspace.

`make test-ios-live-playback` uses the same physical-device/host opt-in to send
a synthetic reply and play it with Backend host selected. The message's Speak
control exposes the source that actually started playback to VoiceOver, including
a local fallback, and reports successful completion or failure from the player
callback. The test requires backend audio to start and complete successfully, and restores
the original reply-voice and automatic-speech preferences. It invokes the configured
chat/TTS providers and leaves a synthetic chat turn; it does not qualify microphone
capture or the physical speaker's acoustic output.
For a cross-device playback check, set `MAGIOS_LIVE_PLAYBACK_TEXT` to an existing
synthetic reply and `MAGIOS_LIVE_PLAYBACK_MESSAGE_ID` to its persisted Linux
assistant-message id. Optionally set `MAGIOS_LIVE_PLAYBACK_SESSION_TITLE` to open
its uniquely titled conversation from History (and leave it selected).
The test checks the exact text and taps that message's Speak
control without entering a new prompt. This is useful when iOS keyboard automation
is unavailable; it does not qualify the iPhone's composer. Preference restoration
uses registered XCTest teardown so an assertion failure still runs cleanup.

`make test-ios-live-question` answers a synthetic pending choice created by another
enrolled client's open chat turn. Alongside the device/host opt-in, set
`MAGIOS_LIVE_QUESTION_FIXTURE_JSON` with `question`, `option_label`,
`expected_reply` and `session_title`. It checks the selected endpoint, finds the
question through the normal Attention tab, answers it, and verifies the continued
reply and resolved state after restart. Independently verify the Linux response
record has `channel: ios`, the selected decision, and the matching chat turn.
The optional string field `answer_delay_seconds` (0–600) holds the visible
question before answering; use `"360"` to qualify a wait beyond five minutes.
This leaves a synthetic resolved request and chat turn in the test workspace.

`make test-ios-live-session-routing` checks two real clients with the same device/host
opt-in. `MAGIOS_LIVE_ROUTING_FIXTURE_JSON` supplies nonempty `session_title`,
`baseline_reply`, `shared_prompt`, `shared_reply` and `foreign_reply` strings for a
synthetic test conversation. After the runner logs `MAGIOS_ROUTING_READY`, send the
foreign probe into another conversation from the second phone, then send
`shared_prompt` into the selected conversation. The 90-second check requires its
live reply and user message, excludes the foreign reply, and verifies the matching
conversation again after a cold restart. Check Linux's saved session/message ids
independently; the test leaves the synthetic conversation selected.

`make test-ios-live-enrollment` uses the same device/host opt-in and waits for
`MAGIOS_ENROLLMENT_READY`. Then open a fresh enrollment link on the iPhone using
the host's enrollment flow. The test confirms only the specified host, verifies
the exchange and authenticated probe, and checks persistence after restart. Keep
the one-time URI out of command logs and test fixtures. This replaces the test
phone's connection; use the isolated acceptance backend.

The unit tests use an isolated URL protocol and never contact a live customer tunnel.
Hosted tests also use a per-process defaults and Keychain namespace: credential
reset/migration fixtures must not clear the physical phone's real enrollment.
Live device UI tests continue to exercise the normal saved connection.
Today has focused unit tests spanning aggregation, paging, optimistic mutation and rollback,
Undo ordering, follow-up and resurfacing workflows, digest/briefing reads, MUIJ parsing and safety
bounds, Activity mutation,
realtime classification, failure isolation, and local-time behavior. Debug UI tests use a
deterministic launch fixture to exercise rich lanes, routing, detail sheets, hidden records, and
Activity without a live backend; normal app launches cannot activate the fixture. The current
baseline passes **323/323 unit tests**. UI smoke tests remain an on-demand device/simulator
lane rather than part of the blocking coverage run.

Every run also writes a self-contained test and coverage dashboard to
`coverage/ios/latest.html` and prints a clickable `file://` link near the end of the command.
The dashboard includes the overall result, test counts and duration, suite and case details,
failure messages, target/file/function coverage, and a searchable result list. It is generated
even when tests fail, while `make test-ios` still returns the original `xcodebuild` exit status.
Raw timestamped `.xcresult` bundles are retained under `coverage/ios/results/`. Override the
artifact location when needed with `IOS_TEST_REPORT_DIR`, for example:

```sh
make test-ios IOS_TEST_REPORT_DIR="$HOME/Desktop/magios-test-report"
```

Xcode may launch `simctl diagnose` after XCTest completes and otherwise wait up to 600 seconds
for simulator logs. The repository runner leaves XCTest unbounded but caps only that descendant
diagnostic collector at 30 seconds, then lets `xcodebuild` finalize the real `.xcresult`. Override
the cap with `IOS_TEST_DIAGNOSTIC_TIMEOUT=<seconds>` when deeper simulator diagnostics are needed.

### Signing
Set `DEVELOPMENT_TEAM` in the gitignored `Signing.local.xcconfig` (copy
`Signing.local.xcconfig.example`). Every target inherits it through
`Signing.xcconfig`; keep team literals out of `project.yml` and the generated
`.pbxproj`. Xcode's target-level Team picker can introduce an override, so remove
conflicting overrides when changing the local configuration. Use Xcode's Team ID,
which is not always the identifier shown by `security find-identity`.
The account must be added in **Xcode → Settings → Accounts** for automatic provisioning to
register the bundle IDs and device.

The main app uses bundle ID `com.magicbeans100x.magican`; its extensions append
their target name (for example, `.Share` and `.Widgets`). The main app and all
extensions use the paid-team App Group
`group.ai.magicbeans.magician.shared`. Keep the entitlement, shared-store constants,
and `project.yml` value in sync; changing only one surface breaks the shared container
used by the keyboard, widgets, and share/broadcast extensions.

The ordinary `make ios-debug-build` / `make ios-debug-run` lane deliberately
does not request `aps-environment`, so it can be signed by Xcode's seven-day
Personal Team profile. Widgets and local Live Activities continue to work, but
remote APNs alerts do not. With a paid Apple Developer team and Push
Notifications enabled for `com.magicbeans100x.magican`, use
`make ios-debug-build-push` or `make ios-debug-run-push` for sandbox APNs.
If that paid team is not the project default, pass its identifier as
`MAGIOS_DEVELOPMENT_TEAM=<team-id>` on the same Make invocation.
Release uses the production APNs entitlement. Do not add `aps-environment` back
to `Magios/Magican.entitlements`; the paid-team lanes use
`Magios/MagicanPush.entitlements`.

The Make device lane keeps DerivedData at `/tmp/magios_dd` so a later
`ios-debug-deploy` can install the exact preceding build. After an Xcode or SDK
update, that persistent cache can retain references to evicted explicit-module
`.pcm` files even though an IDE build succeeds in its separate DerivedData.
`scripts/magios-run.sh` recognizes only that exact missing-PCM diagnostic,
removes its owned intermediates/module/stat caches, and retries once. It never
retries source, signing, package-resolution, or other compiler failures.

The app remains adaptive rather than requiring full-screen mode. Its generated
Info.plist explicitly supports portrait and both landscape orientations on
iPhone, and all four orientations on iPad, matching Xcode's universal-app
validation contract.

## Concurrent voice requests

New execution sessions use server-assigned `#conN-<task title>` names. An unnamed
parent receives the plain task title when a concurrent request is admitted. Review work selects
the exact branch metadata for the navigation title; new admissions refresh the
selected parent's title without changing its conversation.

The composer strip shows active work, unread results and the selected topic.
Viewing a result acknowledges it across devices; read/played rows disappear unless
selected or still running. **New topic** clears the selected context. Once no
relevant rows remain the strip hides. Dismissed selections are cleared without
retargeting an utterance already being transcribed. Each receipt retains the
server-resolved UI thread, parent, execution branch and seed-context IDs; unrelated
result copies displayed in the parent are excluded from backend context preparation.
See [the shared lifecycle and context contract](../docs/components/unified-ui/concurrent-voice.md).

iOS Chat now places voice requests inside the composer as a collapsed latest-line
summary with options. The composer and its top-row press highlights share 12pt corners.
Expand to browse active, unread and selected requests; continue a topic, read a saved
answer, inspect its result, review the execution conversation, cancel a running
request or dismiss a result. Dictation and **Run in parallel** admit independent
requests, and in-app live calls negotiate the same durable backend queue. Capture
freezes the topic, and queued speech waits for the foreground audio to drain.
Closing the screen releases playback ownership without cancelling server work.

See [the shared implementation reference](../docs/components/unified-ui/concurrent-voice.md)
for APIs, device-test status, output receipts and the deferred ESP32 contract.

### Linked concurrent answers

Projected background answers have a subtle border and **View original answer** with a link icon.
The action loads the execution session and thread metadata, retrieves older
message pages when needed, and scrolls to the canonical saved answer. New links
use a durable message ID; older origin-tagged copies resolve by exact turn and
timestamp. Links remain usable after the voice queue entry is read or pruned.
Opening the exact saved result also acknowledges its matching unread receipt.
The canonical answer has no forwarded border. Missing answers show an error rather than jumping to an unrelated reply.
See [the shared contract](../docs/components/unified-ui/concurrent-voice.md#original-answer-links).

### Text queue and Automated concurrent sessions

During a live call, **Type a message** reveals the text composer with a compact
call-control row. Text queues behind an actual running turn in the displayed
session; an idle concurrent call leaves that lane available. Typed parallel
requests use the displayed chat and preserve any pending voice capture context.
`MAGIOS_DEVICE_ID=<id> make test-ios-live-mixed-input` verifies typed queue drain
with a real call in push-to-talk mode, without opening microphone capture.

Send while a reply is active queues text in the current conversation. Send options
and queued entries expose **Stop & send** and **Run in parallel**. Parallel work
uses the shared Background requests strip and appears as Automated / Concurrent
in history, with parent provenance retained. Regular parent conversations stay
Personal. Queue admission is separate from the live reply stream; server actions
claim a queued message atomically before parallel execution.
The parent-conversation link is a centered 30dp/pt row inside the composer
top section, below queue/background updates. Its full-width press highlight
follows the card corners and a subtle divider separates it from the input.
Send options is a compact caret beside Send, with no extra row above the draft.
Queue action controls use the selected theme; iOS also themes the sheet list and
navigation bar, with global queue actions in an overflow menu.
