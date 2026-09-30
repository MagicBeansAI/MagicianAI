# Concurrent voice requests

Implemented on web, Android and iOS. The
original design plan
is archived; this page is the implementation and maintenance reference.

## Rollout scope

The shared backend plus the web interaction below, with OpenAI Realtime,
Hands-free and dictation. Native Android/iOS clients implement the same contract
(concurrent admission, context freezing, output leases, physical playback
receipts); ambient wake sessions keep their existing behavior. Not covered:

- ESP32 — deferred; a future client must use this ledger and playback contract
  (see *ESP32 contract*).
- Other realtime providers and remote/packaged client topologies need their own
  qualification.
- Multi-replica/SaaS deployment, per-workspace fair capacity and load/failure
  drills belong to the
  cloud execution
  and resource-limit plans.
- Cross-device/meeting-audience scenarios, spoken routing for human-input
  requests and external notifications. **Review work** opens the branch's
  existing approval/input UI instead.

## Session classification and text queue controls

Generated execution branches are classified **Automated**, with a **Concurrent**
badge and a link to the parent; coordinators stay hidden. Stored
`internal_voice` provenance drives classification — titles never do. The server
normalizes old branch lanes on read/index and persists on the next write.
User-created parents keep their lane and names; new untitled parents get the
plain task excerpt; only generated branches get `#conN-`. Automatic
active-session selection excludes branches, but exact navigation and history can
open them.

For ordinary text, Send/Enter during a running reply **queues** in the same
session; the running stream stays attached (including across saved-question
refreshes after parallel admission). Queue entries offer **Stop & send** (move
first and stop the current turn), **Run in parallel** (transfer to an independent
branch) and **Remove**; waiting is the default, and the send menu offers the same
choices. Parallel requests snapshot committed context at admission; queued ones
use the preceding completed conversation when they run. Admission preserves
engine/profile routing, permission mode, attachments and request identity.
Attachment and Plan requests do not offer parallel.

- `POST /chat/sessions/{id}/queue` admits queued text without replacing the
  stream; a full queue is rejected, never evicting another message.
- `POST /chat/sessions/{id}/queue/{message_id}/action` accepts `parallel` or
  `stop_and_send`. Promotion and queue drain share the session admission fence
  (only one claims an entry); failed promotion leaves the entry queued; repeated
  successful promotion returns its receipt; a stale Stop & send refuses without
  cancelling a newer turn.
- Queue contents live in process memory; generated branches and receipts are
  durable.

Web and desktop HUD share the composer; native clients use the same server
contract. Background status is labelled **Background requests** for typed and
spoken work. On web, queued messages, background requests and the parent link
sit above the session chooser, each with a themed bottom divider. The parent link
navigates to the actual parent session (even within the same thread) and its
strip disappears once the parent is displayed, unless the parent is itself a
branch. Android and iOS put the parent link inside the composer card below
queue/background rows (same 30dp/pt header and divider; only the first row
rounds its top corners); iOS also puts its queue-sheet entry there. Native Send
options are a caret beside Send. The iOS queue sheet and Android queue actions
follow the active theme; iOS global Stop/Clear live in the sheet's overflow menu.

## User interaction

Voice requests appear at the top of the composer as a collapsed single-line
summary (latest status, latest question, count, options menu) — 30 CSS px on
web, 30dp/pt on native. The header owns a full-width hover/press highlight with
rounded top corners; chevrons point up when collapsed. On web it sits above the
session chooser and uses the theme's accent tokens. Expanded, it shows relevant
requests in a bounded scroll area with subtle dividers; **Review work** opens the
full execution conversation on all clients.

**Visibility and read state.** The panel shows while there is active work
(including child tasks), an unread result, or a selected voice topic. **View
result** records a durable `read_at` shared across clients; completed playback
also consumes a result. Consumed rows disappear unless selected or running; the
result being viewed stays until closed. Opening the panel does not mark answers
read. A read result needs explicit **Read aloud** to play again; a read
acknowledgement never forges playback completion or cancels work. **New topic**,
dismissing the row, or changing conversation clears the selected context.

**One branch per question.** Every admitted voice/dictation/background question
creates a new execution branch, even when nothing else runs — there is no
semantic new-topic classifier. It is named at admission `#conN-<task title>`
(single-line excerpt, no model call); N increases per workspace and survives
restarts and receipt pruning; retries keep the branch and name. An unnamed parent
receives the plain task title at admission; existing titles and renames are
preserved. Native **Review work** loads exact branch metadata for its title.

**Context.** Spoken requests inherit the selected topic's committed context, or
the open conversation. Typed **Run in parallel** snapshots the displayed
conversation; ordinary typed sends keep the normal chat path. Capture start
freezes the addressed context through transcription and admission, so a late
result cannot retarget an in-progress utterance. Speaking a completed result
selects its context.

**Realtime and hands-free.** Hands-free final transcripts and realtime
`delegate_to_chat` acceptances use the same queue when the client negotiates
`concurrent_requests`. Realtime keeps brief direct answers; its execution catalog
advertises only `delegate_to_chat`, while the delegated chat engine keeps its full
policy-checked tool catalog. Guided Tutor/App Copilot takeovers keep their control
path. Older clients and ambient wake sessions keep the legacy path.

**Spoken cancellation.** "cancel this request" (selected context), "cancel the
previous request" (latest active in this conversation), "cancel all background
requests". Other phrases (cancelling a flight) remain assistant input. Each row
also has its own cancel while running and read-aloud/dismiss after completion.

**Delivery gating.** A response waits for active capture, pending dictation,
foreground chat, provider audio and other app TTS. Push-to-talk holds delivery
through capture and transcription; discarded taps, discarded open-mic audio
during a PTT switch, and empty transcription release the gate. Speech
interruption preserves the answer and defers playback; it never cancels work.
Automatic speech follows the current listening interaction and auto-speak
preference; manual Read aloud is explicit. An inactive or hidden tab does not
start output.

### Mixing typed messages with a live call

Web keeps the text composer available during a call. Android and iOS expose
**Type a message** in the live controls, revealing the composer with a compact
call row (mute, full voice controls, end call). Push-to-talk capture stays closed
until held; no second dictation microphone is offered while the call owns
capture.

Typed text addresses the displayed session. A running text/guided turn in that
session queues it; an idle connected concurrent call does **not** reserve the
text execution slot. Starting or ending the call preserves the typed turn and its
queue; background requests keep running. Typed parallel admission neither
inherits a frozen voice topic nor releases pending voice transcription. Voice
results wait for foreground text, draft input, capture, provider thinking/audio
and other speech to clear.

### Original-answer links

The full canonical reply is committed in the execution session before a concise
summary is projected into the parent. All clients show a link icon and **View
original answer** on the projected reply; opening it loads the exact execution
session (and its thread metadata), finds the source message and scrolls to it.
Only the projected copy has the subtle border (on web it follows the bubble and
tail). File/artifact actions keep the execution session's path resolver.

New projections carry the canonical `context_origin.message_id`, stored with the
message independently of queue visibility, read/playback state and receipt
pruning. Older projections resolve by exact saved timestamp and chat-turn ID —
never "latest answer in the branch"; web rewrites them to
`?session=…&message=…`. Clients page backwards past the newest 200 messages. A
missing answer or unavailable conversation shows an error rather than
substituting a reply. History coalescing preserves an explicitly addressed older
answer. Opening a link marks the matching result read if its receipt exists (not
a newer result in the same request), never cancels work or forges playback.
Creating a new web session clears an earlier source-message permalink.

A new question while viewing an execution session starts its own execution using
the viewed conversation as context and display destination; the coordinator is
never eligible as a conversation. Web history refreshes revalidate an omitted
selected session by exact ID before switching away, even under a lane filter.

## Persistence and APIs

All under `/api/magician/v2`, authenticated workspace:

- `POST /chat/sessions/{id}/voice/requests` — text/composer options,
  `submission_id`, optional `context_session_id`; 202 with receipt.
- `GET /media/voice/requests` — durable scope ledger and output lease.
- `GET /media/voice/requests/{id}/result` — canonical saved answer with its
  branch path resolver.
- `POST /media/voice/requests/{id}/cancel` — cancel one accepted/running request.
- `POST /media/voice/delivery` — acquire/release, claim, playback receipts,
  `read` acknowledgements, dismiss.

Limits: at most 32 outstanding work items and 96 retained records per ledger;
four execution slots shared by the service process; a full ledger returns 429
rather than losing work. Output and playback leases expire after 30 seconds. A
claim that never starts returns to pending; interrupted/uncertain physical
playback requires explicit replay. Consumed receipts age out after an hour;
retained branch identities stop an old key from starting duplicate work.

**Context isolation.** A follow-up copies bounded completed text (plus finalized
task summaries and task IDs, excluding unfinished synthesis and duplicate
updates) from the addressed branch into a new branch; full provider history/tool
state is never shared. The receipt carries `parent_session_id` (display
destination), `branch_session_id` (execution owner), server-resolved
`ui_thread_id` and `context_session_id` (exact seed). Parent copies carry
`context_origin: { ui_thread_id, session_id, request_id, message_id? }` (origin =
the branch). These projections are excluded from history fallback, new branch
snapshots and realtime resume context **before** message limits apply, so one
topic's private content cannot leak into a sibling. Legacy projection IDs are
recognized; canonical branch messages sharing the turn ID stay eligible.
Already-materialized legacy snapshots are not rewritten — start a new topic for
clean history.

**Task results.** Task-status messages from a branch update the ledger after
commit. A terminal task result gets its own notification, distinct from an early
chat acknowledgement. Late synthesis stays attached to the same notification:
a newer canonical result replaces preliminary text without re-announcing, and
**invalidates the projection receipt**, so a coordinator-fenced upsert refreshes
the same saved parent message and source link in place (timeline position and
read/playback state kept; stale workers cannot overwrite a newer result or mark
it projected). Web accepts a newer source revision in place, even while another
answer streams. Missing task labels fall back to the branch topic. Playback is
fenced while the saved result changes. Pending task bindings keep the receipt
from pruning. Bounded periodic reconciliation repairs missed projections
(including legacy receipts; replayed events are idempotent). Task cancellation
validates the task's branch and scope first.

**Delegation repeat guard.** Before admitting another delegation batch, the
planner path reads the current user turn's canonical delegation receipts; if a
requested agent still has work in flight (including pending synthesis), it
returns the existing receipt instead of creating replacement work from rephrased
instructions. A first batch may hold several independent assignments to one
agent; other agents, terminal children and new user turns stay independent; a
batch that hits existing work starts no children. This guards background
launches only — repeated shell commands can be intentional. The decision adapter
forwards the canonical projection's typed success/failure outcome, so an empty
successful tool result counts as success evidence.

On restart a committed assistant result is recovered; an unfinished opaque
execution is marked interrupted and not blindly retried.

The web activity store aborts the final reader's HTTP stream on unsubscribe
(keeping its event cache) — leaked streams starve new browser requests across
repeated turns.

Direct WebRTC output-buffer semantics follow the
[Realtime server events reference](https://platform.openai.com/docs/api-reference/realtime-server-events);
backend-proxied playback waits for the local AudioBufferSource queue to drain.

**Known gap:** a Decision Engine `arguments_do_not_match_schema` rejection of a
native tool proposal currently ends the turn with no final reply; a bounded
proposal-correction path with useful diagnostics is still needed. No validator
bypass or argument coercion is used.

## Validation

- `make check-concurrent-voice` — backend/API/media/binary compile and tests.
- `make test-concurrent-voice` — state machine, file store, API authority,
  spoken cancellation.
- `make test-concurrent-voice-ui` — capture/claim races, output serialization,
  physical playback completion, context freezing, inactive-tab behavior.
- `make test-chat-completion-repeat` — delegation guard, empty-output evidence,
  durable result replacement, stale-writer rejection, receipt preservation.
- `make check-ui` — full UI type check.
- `make check-magdroid`.
- `make test-concurrent-voice-ios` (simulator by default;
  `IOS_TEST_DESTINATION` / `MAGIOS_DEBUG_DERIVED_DATA` target a phone).
- `MAGIOS_DEVICE_ID=<id> make test-ios-live-concurrent-voice` — uses the phone's
  saved connection, submits two synthetic questions, checks controls, opens a
  result and verifies playback status.

## Native implementation

Android and iOS `ConcurrentVoiceCoordinator` poll the same authenticated ledger,
renew output authority, and acknowledge the speaker's start/completion/
cancellation callbacks. Main-thread capture and output gates are re-checked after
the network claim. Live provider playback must drain before a queued answer
starts. Leaving chat releases output ownership; accepted work continues. Explicit
**Read aloud** works with automatic speech disabled. Neither client cancels
execution when a newer question arrives. Empty or failed dictation releases the
capture/input gates without submitting the draft.

### ESP32 contract for later

Keep capture and playback as separate state machines. Send a unique admission key
per question and retain it across transport retries. Freeze the addressed context
at capture start. Use the shared output lease and fenced claim, report actual DAC
start/drain/interruption, and stop audio immediately when authority is lost. A
small display can show the latest line and count with a button to cycle or expand;
a screenless unit needs equivalent spoken/navigation controls. Reconnection must
recover ledger state without resubmitting or cancelling accepted work. Buffer
drain, echo handling, reconnect and cross-device ownership need device tests
before ESP32 parity is claimed.
