# Magdroid Companion

**Current development version:** `0.5.0` (Android version code `25`). The
shipped app is **Magican** (`ai.magicbeans.magican`).

> **Canonical links and domain (0.4.4, 2026-09-01):** Android registers and
> accepts only `magican://` app links and uses `magican.ai` for current HTTPS
> examples. The Apps attestation pin advances to code 19; code-18 identities
> must upgrade and re-enroll.

> **Constrained wake identity (0.4.3, 2026-09-01):** Android decodes every
> primary-agent `wake_spelling` and arms the in-lexicon spellings instead of the
> canonical coined name, while keeping Magican for display. The reviewed Apps
> attestation pin advances to code 18; code-17 identities must re-enroll.

> **Bearer-bound device release (0.4.2, 2026-08-31):** Principal and workspace
> remain server-owned bearer claims; authenticated internal artifacts use a
> bounded same-origin cache and cross-origin redirects are refused. The Apps
> attestation pin advances to code 17, so code-16 identities must re-enroll.

> **App Pilot diagnostics patch (0.4.1, 2026-08-28):** The packaged version
> code is 16. Clean checkouts again include the bounded App Pilot command
> history, and Apps enrollment decodes its standard Base64 challenge on both
> Android and the local JVM while retaining the exact 32-byte fail-closed
> admission rule. The server attestation pin advances with the build; devices
> enrolled on code 15 must upgrade and re-enroll before owner actions resume.

> **Platform layering release (0.4.0, 2026-08-28):** The packaged version code
> is 15. The server-side Android Apps attestation pin advances with this build;
> devices enrolled on code 14 must upgrade and re-enroll before owner actions
> are admitted.

> **Magican presentation (0.3.9, 2026-08-26):** Launcher, keyboard, widgets,
> and icons ship as Magican. `applicationId` is `ai.magicbeans.magican`;
> Kotlin namespace stays `ai.magicbeans.magdroid`. Deep links use
> `magican://`. Launcher
> shortcuts and `scripts/magdroid-device.sh` use the installed package id.

> **Apps owner update (2026-08-24):** The reviewed Android Device Owner lane
> admits the exact structured eight-action roster: snapshot, screenshot,
> launch, close, tap, type, key, and scroll. Raw generic device, shell, ADB,
> package, coordinate, and transport authority remains blocked.

On-device Android automation for Magician: an `AccessibilityService` that reads
the screen as structure, acts on it, and captures pixels when structure is not
enough — so an agent can work a phone the way `agent-browser` works a browser.

Magdroid now contains both halves that belong on the handset: the **companion**
(the automation engine) and the native **Magician client**. The client and
bridge share the same enrolled Magician scope and device credential; they are
not separate accounts or separate pairing flows.

Product-facing defaults shared by the app and bridge are generated into
`bridge/.../identity/ProductIdentity.generated.kt` from the repository-wide
`data/presentation_identity.json` manifest. Settings and the background wake
service consume that seam, including before enrollment and while offline.
The chat composer's Ask and Plan placeholders use the scoped primary agent's
display name (or first nonempty alias), falling back to the product name,
**Magican**. They observe the cached identity and refresh it when Chat resumes.

## Notes

**Menu → Notes** opens the Magician notes library in the app. The folder list
is a side panel, and the note is the Markdown file read and written through
`/api/magician/v2/notes`. Published task notes open in that same library.

## Status

**Builds, tests green, and the outbound bridge and native client are wired.**
`make build-magdroid` produces a debug APK and `make test-magdroid` runs the
Android unit suite. The client currently includes Chat, functional Settings,
the Magican system keyboard, Audio Notes, the full Tasks workspace, and the full
Today workspace. Settings is not a directory of placeholders: Guide and Roadmap
open native pages; Roadmap is pending-only and never repeats shipped features as
checked rows. Its entries are platform-specific rather than numerically copied
from iOS: Android omits QR endpoint pairing from the pending list because it
ships through App Pilot, and omits screen-guided App Copilot because App Pilot
already owns that surface, while retaining the unshipped screen-plus-speech,
assistant-result, messaging-negotiation, and VibeDev handoff work. One
resizable **Magican at a glance** Home Screen widget now prioritizes Needs You,
active work, and Talk from the canonical Today response. Optional FCM data
delivery refreshes that widget and the existing task-progress notification when
the app/socket is asleep; builds without a deployment `google-services.json`
retain periodic and in-app refresh without a baked customer Firebase project.
Settings presents this as one **At a glance** section: widget discovery,
host-dependent deployment capability, and the one notification permission for Attention and
task cards. Delayed pushes are task- and timestamp-fenced, while polling is
network-constrained and bounded to one retry before retaining the cached view.
Poll and push writes are serialized and monotonic by the canonical server
generation time; realtime and FCM task-card updates share the same persisted
ordering fence. Non-terminal push progress updates the widget directly rather
than launching a Today fetch for every step. Terminal task delivery also removes its temporary server
route instead of waiting for the watchdog or a later task to replace it. The
20-minute tracking leash is durable too, so restarting the process cannot turn
an abandoned followed task into a permanent route.
Theme and appearance persist; service status performs live
probes; learned vocabulary can be reset; and every voice choice drives its
runtime adapter. Device automation configuration is intentionally absent from
general Settings: the drawer opens one canonical App Pilot surface instead of
splitting the same controls across two pages.

App Pilot retains every original Status and Setup capability in the active Magican
theme, but presents them in one operational Status tab plus a distinct Activity
timeline. Status owns the enable control, live bridge,
accessibility and screenshot state, device and latency metrics, every
Android-owned grant, pairing and Cloudflare Access credentials, and device
details. Missing prerequisites open and sort to
the top; healthy detail and advanced controls collapse into summaries. Activity merges outbound
bridge lifecycle events with tool executions from the production MCP socket, so
successful remote automation cannot disappear from on-device diagnostics.
The execution source retains at most 300 process-local entries containing only
tool name, timing, outcome, and coarse category; it does not persist arguments
or credentials.
Performance percentiles retain every command in Activity but exclude
`android_wait_for_idle`, whose deliberate stabilization delay measures the
target app rather than Magdroid's tool overhead.

Chat reconciles the realtime socket and send stream by persisted message id.
The local user echo carries its turn id until the server assigns a message id;
attachment rows remain separate. A completion arriving on both transports keeps
one copy of each reply and retains live activity, avoiding duplicate transcript
keys and the resulting Compose crash.

Reply SSE and activity NDJSON use [scoped streaming responses](https://ktor.io/docs/client-responses.html#streaming-data),
so tokens and pending-question events arrive before the server closes the body.
These two streams have no total-request or socket-idle deadline while waiting
for a human answer; ordinary requests retain their five-minute budget and
connection establishment retains its twenty-second limit. Android marks an
accepted chat turn `continue_on_disconnect`, so releasing the response
after process or radio loss does not cancel server work; the canonical
transcript/realtime feed restores the completed turn without replaying its
tools. A late canonical text reply replaces the same turn's local
streaming/failed placeholder. Explicit Stop still cancels the server run.
Attention also reads the scoped `/user-requests` ledger and reuses its existing
answer forms when a chat question has no feed projection yet. Requests already
present in the feed are correlated by their response id; a failed ledger read
is shown as a failure instead of claiming nothing needs an answer.
Waiting steps link to Attention so the visible question leads to its response form.
Attention refreshes on entry and resume to recover requests raised while it was away.

Chat keeps the active conversation's lifecycle controls beside History, as on
iOS. **Clear chat** removes the transcript while preserving its session and
unfinished composer; **Archive session** removes it from the active lane; and
**Delete session** permanently removes it. Android waits for Magician to accept
each mutation before changing local history, holds duplicate taps in flight,
and retains the backend's archive/delete protection for the default General
session. Reopening the app restores the exact last session selected on that
device, keyed by server origin and principal/workspace. A confirmed deletion
creates one replacement, while offline, timeout, authentication, and server
failures remain visible and never create duplicate sessions. Existing installs
without a saved selection adopt the newest active Personal session instead of
posting a new one. History itself matches the iOS information architecture: paginated
**Sessions / Threads**, **Personal / Automated** lanes, global search across all
four collections, and context-sensitive creation. A long press archives,
restores, or deletes any non-protected row; opening a thread finds its active
session across server pages or creates one if the thread is empty. This state
is owned separately from the streaming transcript so search and pagination do
not cause token-by-token chat content to recompose. Assistant answers own the
theme's tinted response surface; Steps stays unfilled and compact beneath it,
and the small Speak action aligns to the same outer edge without Material's
default 48dp visual height. A send mounts that response immediately with its
stable `chat_turn_id`; Android follows the canonical turn projection so Steps
appear while tools are still running, including before the first answer token.
When the reader is already at the bottom, activity-row growth follows the live
edge just like streamed text. A projected result keeps its canonical Chat or
task/execution owner and exposes **Open complete result**. Android reads every
authorized page, verifies the result identity, hash, page order, selection and
cursor progress, then losslessly reconstructs complete values, typed containers,
and UTF-8 string fragments in a native viewer.
Conversational copy uses a compact 13sp/18sp scale
with an 11dp inset and clean, tail-free rounded-square bubbles; task,
escalation, attachment, and system content remains card-shaped. The composer
uses the same iOS surface role. Its mode control matches Web: Do is one split
button whose caret selects Ask or Accept, the face restores that remembered
permission after visiting Plan, and Plan is the only peer mode. Mode labels use
explicit line spacing, vertical centering, and a 28dp minimum height that can
grow with font scaling, keeping Do/Ask/Accept and Plan clear of clipping. Compact mode,
profile-tag, mute, and Live controls preserve room
for the transcript on narrow phones. The profile selector preserves the
server's adaptive tier/model metadata as theme-aware colored tags instead of
flattening every profile to plain text. It also lists installed chat harnesses
from `GET /plane/engines`; Magician/Pi show API profiles, while Claude Code,
Codex, Codex App Server, Grok, and agy show their available model choices.
Android saves the choice in app preferences and sends the engine, model, and
profile with each chat request, independently of other devices. A sent turn
keeps the engine, model, and profile selected when Send was tapped, even if the
composer choice changes while the request starts.

A microphone tap starts dictation and the next tap finishes it. A recognized
long press records until release or cancellation. The gesture stays active
across recording-state updates; quick taps do not also run the hold handlers.
Android's large voice button centers a 30dp microphone icon in its 64dp circle.
The empty text composer shows a microphone icon to switch back to voice mode.
The chat composer uses 12dp corners, shared by its top-row press highlights.

Voice matches the iOS settings model with reply voice, dictation source,
account-synchronised speak replies, per-surface audio profiles/stages, Live or
Hands-free mode, realtime provider selection, open-mic or hold-to-talk turn
boundaries, finite wake-word listening, captions, mute, stop, and bounded
transport recovery. Live captions apply `transcript.assistant.delta` as the
assistant speaks for GPT Realtime and Gemini Live; Gemini 3.5 Live Translate
is not offered as a Realtime engine. The engine list is the backend catalog in
its order (Gemini 3.8 Live and 3.8 Live Extended Thinking ahead of 3.1 Flash
Live). `interaction.status` sets `RealtimeVoiceState.assistantWorking`
(`withInteractionStatus`: only `in_progress` counts), and the live panel's
status line says "Working…" while Gemini 3.8 Live Extended Thinking has said
"let me check…" and is still on the request; `idle`, an interrupt, and a fresh
`session.ready` clear it. Wake listening has no product-name constant: Settings
refreshes the scoped primary agent and displays/listens for `Hey <name>` plus
its aliases, retains the last good identity offline, and refuses to arm before
an authoritative identity exists. Optional dictation archiving writes a durable
local Audio Note before upload and WorkManager retries it after connectivity
returns.

Today is not a summary of the task
list: it is the web's Morning Edition — masthead, realtime wire, lead story,
Economics of Operations ledger, a Reading Room (Morning Brief swipe deck or
Broadsheet columns), special reports, completed deliverables and the digest
chronicle (see `docs/components/magdroid/today.md`). Auxiliary reads fail
independently, card mutations are optimistic with exact rollback, and scoped
realtime events feed the wire and refresh the server-authored state. Broadsheet
core, message-follow-up, and Worth-a-look cards keep the iOS swipe contract:
short swipes reveal labeled action rails, full or fast swipes commit the primary
edge action, and every server-projected action remains reachable through inline
or overflow controls.

Tasks keeps creation and refresh in the page header, with a full-width search
row below it and compact lane/filter controls so the task list remains the
dominant surface. Its 52dp toolbar sits below the status-bar inset and vertically
centers the title and actions. Task and monitor details open as full-screen pages
with their own Back and trailing actions; the Tasks toolbar and bottom tabs return
on Back. Task detail uses one scrollable page per tab: the title, description,
run selector and tab controls scroll with the results. Result links still land
directly on the result card, and returning to the task list preserves its position.
The same chrome fronts Tasks, Monitors and Internal Tasks;
changing density does not change their pagination, realtime refresh, recovery,
or action contracts. Regular Tasks alone show the All/Inbox/Today/Overdue/
Running/Completed presets and tag chips. Internal Tasks starts with every row
returned by `/api/magician/v3/tasks/internal`, including completed work, and
uses its own Any-status and agent controls plus search and sort, matching Web.
Category, sort and inline-action controls share a 13sp
line box and 4dp vertical inset so their visual heights remain aligned. Task titles, status
labels, origin metadata and inline
state/overflow controls use an explicit compact scale instead of Material's
larger default button metrics; inline actions use a bounded 13sp line box and
4dp vertical inset. Card tag and origin metadata remains on one
horizontally scrollable rail when it exceeds the phone width, rather than
inflating card height or creating multiple competing metadata scrollers. Swipe
rails, clipping and translation layers are allocated only during an active,
open or settling swipe, so ordinary vertical scrolling does not keep that
rendering stack for every visible card. Page,
control, card, border, semantic-status and swipe
colors resolve from the selected Magican palette, matching iOS across day, night
and strongly colored themes. Loading, unavailable and empty copy remains small
enough to be status rather than becoming the screen's visual hero.

Today keeps the same operating workspace mounted whether Magician is reachable
or not. A failed first read appears as a compact, retryable classified banner;
the masthead, wire, ledger, Reading Room, Hidden and Special Reports remain
visible with empty values, and the header only reports an all-clear after a real Today
payload has been read.

Native forms use one compact Magican field contract across the Android client:
40–46dp single-line containers, 13sp input type, persistent compact labels and
line-based multiline growth. This avoids stock Material's oversized 56dp
minimum without clipping or vertically misaligning text, and keeps the same
keyboard, validation, masking and submit behavior on every surface.
Filled and outlined action buttons likewise use Magican's softly squared 8dp
corners; capsule geometry is reserved for chips and other intentionally
pill-shaped labels.

The selected theme owns the complete Android window, including Material-owned
dialogs and menus, app and bottom bars, and Android's status/navigation bars.
The native window and full-page Compose root both paint the selected background,
including empty margins and transparent pages. The light launch canvas uses
Longhand cream (`#F3EAD6`) until the persisted theme is applied.
Its typography also follows the Web contract for every theme variant: brand,
display, body, and mono roles resolve to bundled fonts and Material text styles.
Longhand selects real variable-font weights for Outfit, Manrope and Geist Mono;
the font files' Thin/ExtraLight defaults are not used as regular text. Secondary
text uses iOS's full palette color, and Material container tones stay in that
palette in both Day and Night. Captions use natural font leading rather than
inheriting Material's oversized body line height.
Settings and guide/keyboard groups use iOS's warmer surface layer rather than
the pale elevated-card layer. Night-mode Do · Ask uses a restrained accent tint
with light text; it never inverts the light text token into a white button fill.
Do · Ask and Plan use a 24dp minimum height with vertically centered labels,
and can grow with larger text. At a glance rows share an icon/text alignment;
widget instructions and delivery status sit below their titles so they cannot
squeeze labels into a narrow column.
The app shell does not show ambient app indicator chips; app status belongs on
the app's own surface.
Bottom navigation uses filled accent icons for the selected destination and
muted outlines for the remaining destinations, without Material's selection
capsule. Settings copies iOS's compact Appearance layout: one Dashboard Theme
row, one horizontal System/Day/Night control, and its explanation inside the
same card. Theme-family previews on both clients come from the real resolved
palette rather than duplicated display colors.

Today uses the Morning Edition newspaper layout shared with web and iOS, set in
the bundled Newsreader serif with theme-token colours. The primary and secondary
app widgets (including Brainstorm canvas and Learning review queue) follow the
Reading Room; they no longer push the day's work below the fold.

Observe uses one mobile hierarchy on iOS and Android: Brainstorm, live sessions,
upcoming calendar meetings, room capture, bot join, then Published Notes. An
upcoming meeting keeps its three different authorities visible—open the link as
the owner, listen through this phone, or send Magician's attendee bot. Android's
room-capture entry is deliberately a normal card rather than a recording hero;
once live, its Now card owns transcript progress, screen sharing, thread access,
and the stop action. The active rail polls only while Observe is visible and
deduplicates the phone's local session from the server's active-session list.

Published Briefings use the server's `muij_document` contract directly. The
client validates the v1 envelope, bounds depth and total work, then renders its
display components as themed native Compose UI; it does not stringify dashboard
JSON. Controls remain visibly read-only because a published artifact is not an
authority grant. Unknown future display types keep their label and children in
a safe fallback, while non-MUIJ `json_content` receives a bounded readable
key/value presentation.

Device automation now has one transport: the phone dials Magician and serves a
bounded MCP `2026-07-28` tool surface over that authenticated WebSocket. No
listener, debug PIN, `adb forward`, or private bridge envelope ships.

The Magican keyboard rests as an ordinary keyboard: one strip above the keys
carrying Android's selected system spell-checker candidates, with bounded
learned completions as a fallback, and a ✦ key on its trailing edge. Tapping ✦
or holding the space bar reveals the Magican Write/Ask/Act row above it; it is
hidden again for every new field. The keyboard draws from the app's selected
theme, including its Day/Night/System setting. Nothing reads or exposes text
from password, other secure, or numeric fields, where the strip is absent
entirely.

### Pairing a phone

Normal setup has no copied host, token, or Cloudflare secret:

1. Run the host's Cloudflare mobile setup once, then open **Magican Desktop
   Settings → Android App Observation**. Complete **Desktop owner identity**
   bootstrap if requested, then choose **Begin Attested Enrollment**. The
   signed Desktop owner creates a five-minute, single-use enrollment and renders
   its QR locally. The QR uses Magician's configured phone-reachable API tunnel,
   not a desktop `localhost`.
2. Open **App Pilot** on Android and choose **Scan pairing QR**. Camera access is
   requested only for this scan and is not an ongoing App Pilot requirement. The
   app validates the `magican://apps-connect` contract and shows the normalized
   Magician host before doing anything.
3. Confirm on the phone. App Pilot creates a hardware-backed key and submits the
   attested enrollment. Complete its pending decision in Desktop Settings. Once
   the signed owner receipt is accepted, the app stores the host, scope and
   per-device credential in encrypted preferences, proves the credential, and
   reconnects the outbound MCP bridge automatically.

The endpoint is deployment-owned and derived at runtime from the host's
`mobile_access.public_origin` (normally written as
`MAGICIAN_MOBILE_PUBLIC_ORIGIN` by Cloudflare provisioning); it is never baked
into the APK. The QR contains an ephemeral enrollment capability, not the
durable device credential. It expires after five minutes, can be cancelled from
Desktop Settings, and cannot be exchanged twice. Web Settings continues to
show the ordinary paired-device roster and iPhone enrollment; Android Apps
enrollment, review and recovery remain on the signed Desktop owner surface.
Manual connection fields remain collapsed under **recovery only** for damaged
credentials on the already enrolled server. They cannot edit the host or carry
stored credentials to another origin; a server change requires a new connection
QR and verification. This restriction does not yet implement an owner-pinned
allowlist of servers or a saved-server picker. See
[Device Bridge](../docs/components/magician/device-bridge.md#android-apps-action-review)
for the complete authority contract.

### Optional remote delivery setup

Periodic widget refresh does not need Firebase. To also receive Attention and
the explicitly followed task's ongoing-notification updates while the app is
asleep, register Android application id
`ai.magicbeans.magican` in the deployment's Firebase project, place its
untracked `google-services.json` at `magdroid/android/app/google-services.json`,
and point Magician's `MAGICIAN_FCM_SERVICE_ACCOUNT_PATH` at a private service
account JSON from that same project. The Gradle plugin is applied only when the
deployment file exists, so generic/self-hosted source builds stay valid without
one. Firebase's current Android setup recommends the BoM and the main messaging
module (not the retired KTX artifact):
<https://firebase.google.com/docs/android/setup>.
The app uses the current Firebase Installation ID registration callback rather
than the deprecated registration-token callbacks; the identifier is uploaded
only through the authenticated, device-bound Magician API.

## Provenance

Vendored from [NeuralBridge_mcp](https://github.com/dondetir/NeuralBridge_mcp)
at commit `635f4787bd89e236a9a9710239e1a1b22cedc5c7`, Apache 2.0.

Upstream was created 2026-03-08, last pushed 2026-03-16, and has been dormant
since. It is vendored rather than forked because there is no upstream motion to
track; `LICENSE` and `NOTICE` travel with the code, and changes are recorded in
`CHANGELOG.md`.

### Changes made on import

- package `com.neuralbridge.companion` → `ai.magicbeans.magdroid`, matching
  `ai.magicbeans.Magican`; Gradle `namespace`, `applicationId` and
  `rootProject.name` follow;
- dropped upstream's GitHub Pages site (`docs/index.html`, `diagrams/`,
  `screenshots/`), icon-generation tooling (`android/design/`), and process docs
  (`CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`,
  `THIRD-PARTY-LICENSES.md`). Upstream's `SECURITY.md` is not lost — its findings
  are recorded below and in the plan;
- kept and corrected upstream's architecture, tools, performance, and
  troubleshooting notes where they still describe the automation engine.

### Attribution obligation — decide before shipping

Upstream's `NOTICE` is not boilerplate. Beyond the usual Apache 2.0 §4 terms it
asks for **visible attribution in any product, documentation or promotional
material** that uses the software:

> "Powered by NeuralBridge — https://github.com/dondetir/NeuralBridge_mcp"

Importing into a private monorepo does not trigger distribution obligations.
Shipping does. Whether that credit appears — and where — is a product decision
owed before this leaves the building, not after.

## Security

Upstream shipped an MCP server bound to `0.0.0.0:7474` with no TLS and an auth
key its own code generated and never checked — so anything on the same network
could invoke every tool: gestures, typing, screenshot capture. On a handset that
is remote control of whatever is on screen, a banking session included.

Three things changed, in order of how much each matters:

1. **The device dials out.** `MagicianBridgeClient` holds an outbound connection
   and nothing listens for the production path. There is no port to reach, so the
   question of who may reach it does not arise.
2. **The production socket carries MCP directly.** The private bridge envelope
   and the local HTTP implementation are deleted, so testing and production do
   not exercise different protocol stacks.
3. **Magician remains the authority.** Pairing authenticates the exact scoped
   device connection, while the four agent-facing Android verbs remain the only
   model-visible invocation boundary. MCP discovery cannot grant a new verb.

## Building

```bash
make setup-magdroid-build   # one time: JDK 21, Android SDK 34, gradle wrapper jar
make build-magdroid         # debug APK
make test-magdroid          # unit tests
```

A plain checkout cannot build, for three separate reasons, and each fails in a
way that does not name its cause:

- **The JDK.** Gradle 8.13 rejects Java 24+, and this workspace's default JDK is
  newer. The failure reads as an unsupported class-file version, which looks like
  a corrupt build rather than a toolchain mismatch.
- **Gradle itself.** AGP 8.x uses a Gradle internal API removed in 9.6, so
  Homebrew's default `gradle` cannot build this project. The targets use a pinned
  `gradle@8` keg, falling back to the wrapper.
- **The wrapper jar.** Upstream's `.gitignore` excluded `*.jar` before re-including
  the wrapper, and the jar never reached their repository — `./gradlew` fails with
  "Could not find or load main class GradleWrapperMain" on a fresh clone of
  upstream too. It is committed here.

Targets skip with a clear message when the toolchain is absent, the same way the
iOS targets skip without `xcodebuild`.

### Where the toolchain lives

The SDK is ~3GB and the Gradle cache passes 1GB, so both sit on the SSD beside
the Cargo target dir and coverage output:

| path | actual location |
| --- | --- |
| `~/Library/Android/sdk` | symlink → `/Volumes/build/magician/android-sdk` |
| `~/.gradle` | symlink → `/Volumes/build/magician/gradle-home` |

Symlinks rather than environment variables, deliberately: Android Studio and
other GUI tools read those paths directly and do not inherit shell exports, which
is the same reason the notes store is a symlink rather than a configured path.
Override with `MAGDROID_SDK_STORE` or a pre-existing `ANDROID_SDK_ROOT`.

## Layout

| path | what |
| --- | --- |
| `android/bridge/src/main/kotlin/ai/magicbeans/magdroid/` | automation, authenticated transport, chat/task/Today data sources and client state |
| `.../service/MagdroidAccessibilityService.kt` | the `AccessibilityService` |
| `.../uitree/UiTreeWalker.kt` | screen as structure — primary sight |
| `.../gesture/`, `.../input/` | tap, swipe, type, keys |
| `.../screenshot/ScreenshotPipeline.kt` | pixels on demand, via `AccessibilityService.takeScreenshot()` |
| `.../mcp/McpToolRegistry.kt` | dynamic tool definitions, schemas, and safety annotations |
| `.../mcp/MagdroidMcpServer.kt` | bounded tools-only MCP server over the outbound socket |
| `android/app/src/main/kotlin/ai/magicbeans/magdroid/ui/` | Compose shell, Chat, Settings, Today and Tasks/Monitors workspaces, including explicit Magdroid-themed filter controls and lane-local recovery states |
| `.../tasks/TaskRepository.kt` | scoped V2/V3 Tasks, execution, Notes and Monitors API boundary |
| `.../today/TodayRepository.kt` | scoped Today, follow-up, resurfacing, briefing, feed, analytics and realtime boundary |
| `.../voice/` | dictation, TTS, live/hands-free control transport (voice-control WS offers `magician-voice-control-v1` plus `magician-bearer.<token>`), finite wake-word service, audio-profile catalog and durable Audio Notes outbox |
| `.../keyboard/` | Android IME, ordered Write/Ask/Act skills, language/layout state and learned vocabulary |

`McpToolRegistry` is the integration seam. Phase 2 swaps the transport in front
of it and leaves `McpToolHandler` untouched.

## Known cleanups inherited from upstream

- **Two `McpProtocolTest.kt` files**, and two `McpToolRegistryTest` classes, in
  different packages. The suite now runs and they are **not** duplicates: 7 and 12
  cases, 8 and 12 respectively, all green. Confusing names over real coverage, so
  the fix is a rename, not a delete.
- **No CI.** Upstream had no workflows.
- **`takeScreenshot()` is API 30+** while the manifest claims API 24+. The real
  screenshot floor is Android 11; below it the pipeline needs to degrade
  explicitly rather than return nothing.
- **Performance claims are unverified.** Upstream advertises ~6 ms actions and
  "100× faster than Appium" with no benchmark harness in the repository. Measure
  before repeating.

## Concurrent voice requests

New execution sessions use server-assigned `#conN-<task title>` names. An unnamed
parent receives the plain task title when a concurrent request is admitted. Review work retains
the exact session's metadata even though execution branches are absent from the
ordinary history list. Admission refreshes the parent history label.

The composer strip shows active work, unread results and the selected topic.
Viewing a result acknowledges it across devices; read/played rows disappear unless
selected or still running. **New topic** clears the selected context. Once no
relevant rows remain the strip hides. Dismissed selections are cleared without
retargeting an utterance already being transcribed. Each receipt retains the
server-resolved UI thread, parent, execution branch and seed-context IDs; unrelated
result copies displayed in the parent are excluded from backend context preparation.
See [the shared lifecycle and context contract](../docs/components/unified-ui/concurrent-voice.md).

Android Chat now places voice requests inside the composer as a collapsed latest-line
summary with options. Expand to browse active, unread and selected requests; continue a topic, read a saved
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
