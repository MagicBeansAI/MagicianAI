# Pause and Chat Persistence

Magician keeps large resumable execution state and canonical chat transcripts
off request, startup, and reconnect stacks. Both stores use a small durable
commit record and bounded payload files.

## Durable execution pauses

New pause records use format version 2:

- `k2_<key-hash>.json` is a bounded envelope. It contains scope, task and
  execution routing, agent/goal/cycle identity, pause kind, HITL display
  metadata, authorization hash, immutable body revision, body byte length, and
  SHA-256.
- `k2_<key-hash>.body-<revision>.json` contains the exact continuation body,
  including provider continuation state and the live tool-call/result history.

The body is written and synced first. The envelope is atomically replaced last
and is therefore the commit and authorization point. An interrupted write can
leave an unreferenced body, but it cannot publish a partially written or
unmatched continuation. Resume validates filename, size, digest, scope,
routing identity, pause kind, and authorization hash before returning state.
If the envelope rename succeeds but its parent-directory sync reports an error,
the complete envelope/body pair is published for the current boot and the sync
failure is returned from durable storage as a typed
`EnvelopeDirectorySyncUncertain` error. Best-effort pause storage retains its
existing publish-and-warn behavior. Because a power loss could still roll the
directory entry back, the store also preserves the prior revision body and any
legacy authority record; neither is retired until a later envelope commit is
fully directory-synced. On an ordinary restart where both same-scope records
remain, exact recovery prefers the current v2 envelope and treats the legacy
record only as a fallback. The same key found in more than one scoped workspace
is ambiguous and fails closed rather than consuming either tenant's record.
Exact body hydration is transferred to the dedicated execution runtime before
deserialization; HTTP and orchestration workers only select the bounded
envelope and await the recovered heap-owned continuation.

All mutations for one pause key—store, durable store, take, exact recovery, and
execution cleanup—share one bounded striped lock. The lock spans body publish,
envelope rename/directory sync, stale-body cleanup, in-memory publication, and
index publication or revocation. A replacement commit therefore cannot race a
take into deleting its new envelope or let two writers reclaim each other's
bodies. Whole-execution cleanup acquires all stripes in ascending order so a
new, previously undiscovered key cannot land during terminal cleanup; ordinary
single-key operations acquire exactly one stripe.

Startup, reconnect, listing, confirmation inspection, orphan reconciliation,
and cancellation use v2 envelopes only. Legacy monoliths are intentionally
excluded from bounded discovery and are hydrated only for an explicit exact-key
resume or repair on the execution runtime. Format discovery reads at most the
256 KiB envelope ceiling; a larger authority record is classified as legacy
from file metadata without scanning up to the 64 MiB body ceiling. Cancellation revokes the envelope before
removing revision bodies, so an unlink failure cannot leave resumable
authority. Existing monolithic pause JSON remains readable and is replaced by
the versioned representation the next time that pause key is persisted.

**Reaping and scope-wide admission.** The startup reaper removes a pause record
whose execution directory is gone **or whose durable execution state is
terminal** (`completed`, `failed`, `cancelled`); a terminal execution keeps no
live pause. Execution-wide stateless-terminal admission counts a legacy monolith
the background repair has already indexed at its current revision as public
metadata; only an unrepaired, unquarantined monolith fails closed (otherwise one
stale monolith would fail every scope-wide admission and leave watchers
retrying forever).

The store scans encoded JSON depth and bytes before Serde sees either a legacy
monolith or a versioned body. It fails closed when the envelope exceeds 256
KiB, the body exceeds 64 MiB, JSON nesting exceeds the shared retained-value
contract, aggregate JSON node count exceeds its heap-amplification ceiling, or
bounded continuation collections exceed their admission limits. Save and load
use the same inventory of recursive values, including browser accessibility
trees, typed CDP DOM trees, pending/resolved inputs, live and compacted message
blocks, continuation frames, preserved tool schemas/defaults, and
expected-artifact schemas. A rejected owned tree is drained iteratively before
it leaves the execution worker stack. These limits protect authoritative state
and never truncate it.

### Cross-iteration loop-protective state

A pause carries `loop_protective_state`, the loop's own protection against
itself: the cycle/no-progress detector and its fingerprint history, the
transient-provider and yield retry budgets, the parse-abort and
terminal-rejection counters, failed-tool suppression, per-tool repeat counts,
the observation policy's inputs, and the run's accumulated USD spend.

These live as locals in `execute_agentically_inner`; without the carry a resumed
run could re-enter a detected cycle, retry past an exhausted cap, or exceed its
per-run cost ceiling simply by pausing. Restoration drains the carry rather than copying it, so a context
cloned forward into a nested continuation segment cannot re-seed a child with
its parent's budgets.

Every field is bounded and omitted when untouched, and the record omits the
whole block rather than writing an empty object, so a run that never trips a
counter adds no bytes at all. The detector's fingerprint deques are bounded on
load against the shared protective-collection ceiling: the detector's own
history bound is deserialized from the same record, so it cannot be trusted to
limit that record.

Restoring the block is covered by the authorization hash even though it grants
no authority. The gated resume path restores it, and editing it on disk would
re-mint the per-run cost ceiling, disarm the parse-abort and terminal-rejection
counters, or set the sub-goal skip counter high enough to burn a whole budget in
no-op iterations. Anything a resume restores belongs in the hash. Five values are deliberately excluded:
the execution history (its per-iteration environment states would bypass the
shared JSON-node inventory, and the live message log already carries the same
conversation), the page merkle tree (page state is externally mutable, so resume
re-observes regardless), in-flight tool lineage (the dispatch it describes did
not complete), the trust dispatch guard (rebuilding it is what closes the
policy TOCTOU window), and the operator-steer buffer (it is folded verbatim into
the next decision prompt, so restoring it from disk would make a tampered record
a prompt-injection vector).

### Unspent approvals and the elevation boundary

A pause also carries approvals a human granted that the run has not yet spent,
so a resumed run does not re-ask a question the user already answered.

Because restoring an approval re-grants authority, the field is covered by both
the elevation predicate and the authorization hash. Two constraints follow, and
both are load-bearing:

- Approvals are written **only** onto a pause that is already an elevation for
  other reasons. An elevation is verified on resume against an in-process
  authority entry, which by design does not survive a restart; promoting an
  otherwise-benign pause (max-iterations, budget, plain user input) would make
  it fail closed afterwards and discard the user's work — strictly worse than
  re-asking for one approval.
- Approvals are restored **only** by the exact-resume path, the one entry point
  that has already verified the record against that authority entry and the
  authorization hash. The other resume endpoints have no such gate, and the
  envelope hash is unkeyed, so restoring there would let a rewritten body and
  envelope inject a pre-approved action.

The loop takes approvals from its context rather than copying them, and the
per-segment context move takes them again, so an approval spent in one
continuation segment cannot be replayed into the next.

### Budgets across a resume

A resume that raises the iteration budget also clears an **exhausted** cost
budget. Without that, a run paused on the cost dimension re-evaluates the same
accumulator on its first iteration, immediately re-pauses, and writes a fresh
synced record on every bounce without executing a step — the resume affordance
would be permanently dead. The reset is conditional on the accumulator having
actually reached the cap: an ordinary human-input or manual resume keeps its
accumulated spend, so pausing for input cannot be used to shed cost. Every other
protective value is retained across all resumes; those bound correctness rather
than spend.

### Resuming past an operator-steer terminal fence

A segment that pauses (`WaitingUser` after a question, or after
`BudgetExhausted`) admits its pause as a terminal boundary and leaves a
*terminal fence* in the run's operator-steer inbox
(`restricted/operator_steers/<blake3(execution_id)>.json`), stamped with the
segment and iteration that sealed it. The operator's answer resumes the run as a
new segment (`<execution_id>-r<N>`) whose `LoopState` is seeded fresh (cursor at
iteration 1). The seed hands the fence to the successor
(`rebind_terminal_fence_to_successor`) with the claim's iteration reset to 0,
and the successor's first `Prepare` reopens it
(`reopen_after_nonterminal_boundary`). The reset is required: the reopen rule
orders boundaries within one segment, so keeping the paused segment's iteration
would refuse the successor's iteration 1
(`operator_steer_inbox_terminal_fence_binding_mismatch`) and strand the run
paused with its answer consumed. Across segments the order holds by
construction.

### The answered ask is in the restored conversation

`need_user_input` is a pause, not a tool call, so a pause's `live_messages` end
at the last tool result before the ask. A resumed model that sees an answer with
no visible question tends to ask again. `continue_resumed_execution` therefore
appends the exchange (`append_answered_ask_to_restored_conversation`): an
assistant `need_user_input` tool call with the question and input type, and a
tool result carrying the answer as `status: answered` with a note not to ask
again. The answer is the already-redacted summary the goal carries, so secrets
never enter the transcript. A cold resume (no restored conversation) appends
nothing.

### A resume outlives its HTTP request

`POST /api/magician/v2/executions/{id}/resume` (and the HITL `respond` route,
which lands on the same owner) runs the whole resume —
`execute_agentically_resume_with_validation` plus settlement (persisting the
next pause, `Executing → WaitingUser`, the outcome projection) — as its own
`tokio::spawn` task and awaits the join handle
(`MagicianV2Api::resume_detached_from_request`). Actix drops a handler's future
when the client disconnects; without detaching, a timed-out browser would leave
the loop running with no pause persisted and the execution stuck at
`Executing`. The task returns the response reduced to status, content type and
bytes (`HttpResponse` and actix `Error` are not `Send`). The HITL `respond`
route answers `202 resume_admitted` once the answer is admitted, because waiting
for a device flow's loop outlives the UI's 30 s fetch; see the API README.

### Lifecycle fences are not reentrant, and never wait forever

Every pause activation, resume, cancel and terminal receipt takes an exact
lifecycle fence: an in-process tokio mutex stripe (256 per kind, agent and
execution) over a durable flock (64 stripes per kind under the scope's
`pause_states/`, e.g. `.stateless-terminal-execution-locks/.pause-lifecycle-N.flock`,
`.agent-lifecycle-N.flock`). The terminal receipt takes the agent fence, then the
execution fence. The mutex is not reentrant: a task that re-acquires a fence it
holds parks forever, as does every later taker of that stripe. So a handler
releases its fence before calling anything that takes it (e.g. the orphan
finalizer), and every in-process fence wait is bounded at 30 s
(`STATELESS_LIFECYCLE_PROCESS_LOCK_WAIT`): a stuck holder surfaces as
`timed out … waiting for the in-process … lifecycle exclusion` instead of a
silent wedge. Legitimate holders keep a fence for one admission or receipt.
Diagnose a wedge by checking which lock files the process holds (`lsof`).

## Canonical chat transcript

New `session.json` documents are format version 2 and contain session metadata
only. Display messages retain their existing segmented JSONL store. Canonical
LLM history is stored under the session directory:

```text
llm_history/
  manifest.json
  segments/<generation>-<sequence>.json
```

Each transcript append is an immutable, atomically written mutation segment.
The constant-size manifest is replaced last and is the only commit point.
Unreferenced partial-tail files are ignored after a crash. A generation switch
implements an O(1) logical clear; older immutable files are eligible for
background retention instead of blocking the user-visible answer path.

The manifest tracks unmatched provider tool-call IDs and a logical group ID.
An assistant tool call and its later result retain the same group even when
they occupy different physical mutation files. Tail reads include the complete
boundary group, preventing replay from separating a tool result from its call.

Rejecting a just-submitted user turn is a cross-file transaction: its display
message and canonical transcript entry live in different segmented stores. A
bounded `rollback-intent.json` is committed before either is changed and records
the exact transcript generation, sequence, and effective count. Every session
operation recovers an outstanding intent while holding that session's lock.
Recovery idempotently deletes the selected display IDs, commits or recognizes
the one matching transcript-tail truncation, updates metadata, and removes the
intent last. Crashes after intent publication, display deletion, or transcript
manifest commit therefore cannot replay a deleted user message or truncate a
later turn twice.

Provider-native state is preserved exactly in transcript entries for OpenAI,
Anthropic, and Gemini. There is deliberately no detached "latest provider
checkpoint": a response ID without its exact assistant position and surrounding
tool-call/result group is not a valid continuation, and the router may fall
back to a stateless provider in the same turn. A bounded tail therefore returns
only semantic entries that fit its complete boundary group. Consumers that
need an older provider anchor load the authoritative semantic history rather
than synthesizing an anchor outside that tail.

An active typed-chat turn decodes that semantic history once and reuses the
same owned vector for chain repair, provider fallback, and dispatch. Stateless
providers require the full replay, and OpenAI Responses must inspect the full
chain before trusting an anchor because an older failed/orphaned turn can poison
later response IDs. Memory/procedure relevance borrows only a bounded tail of
that vector without cloning it. Voice delegation follows the same one-load
rule; voice resume compaction, which needs only recent projected tool
exchanges, reads a bounded group-preserving tail directly.

When no canonical transcript exists, bounded history reads synthesize only
from the newest display-message segments. They do not load or materialize the
complete display ledger merely to return a small tail.

Legacy `session.json` files with inline display messages or `llm_history`
migrate lazily before the first read or mutation that needs that session.
Display segments use deterministic sequence names and verify any already
written prefix, so a crash resumes without duplication. LLM migration progress
is committed in the manifest and resumes by effective entry count. The inline
arrays are cleared only after their segments commit; every subsequent metadata
write rejects anything except a small format-v2 document with empty inline
arrays. Title, status/archive, thread, and display-message mutations therefore
cannot accidentally rewrite a legacy monolith.

Startup indexes only a bounded session metadata projection and never opens
transcript segments. Session documents, manifests, segments, and message
records have byte ceilings and encoded JSON is depth-admitted before Serde.
Manifest, rollback-intent, clear-intent, and mutation-segment reads enforce
their byte/depth/node ceilings on the actual open file, then rewind and decode
that same handle while verifying its admitted hash. A size stat is never the
read authority, so a corrupt or concurrently replaced control file cannot
bypass admission between metadata inspection and allocation.
All recursive values in canonical transcript models (tool arguments, Gemini
parts, Anthropic content, and projected model values) additionally share an
aggregate node ceiling on both append and read. Display messages and v2 session
metadata contain no open-ended JSON value fields.
Large legacy documents are streamed through a projection parser on the
blocking pool instead of being allocated as one startup string, and
default-session normalization remains index-only until the session is
genuinely accessed. Serialization uses capped writers, so an oversized payload
is rejected while it is being encoded rather than after an unbounded temporary
allocation.

Inline-chat prompt-size telemetry walks the existing typed transcript and tool
catalog without constructing a diagnostic JSON copy. Full dumps for successful
provider calls default off; `MAGICIAN_CHAT_SUCCESS_PROMPT_DUMPS=true` enables
bounded, asynchronously persisted diagnostics. Provider-failure dumps remain
available by default, move the terminal prompt state to the execution/storage
runtimes without cloning it, and fall back to a metadata-only record when the
4 MiB byte, JSON-depth, or JSON-node budget is exceeded. Dump filenames use the
same bounded safe-segment encoding as other scoped chat artifacts.

Startup delegate-card recovery streams each complete per-turn JSONL journal on
a blocking storage worker. Records have byte/depth/node admission, malformed or
oversized rows are skipped, and only the latest unresolved descriptor per
task/execution is retained; old unresolved delegates are therefore still found
without materializing lifetime event histories. Task-result cards retain only a
64 KiB/8,000-character preview, while the same bounded stream independently
recognizes an exact `TUTOR_ACTION_RESULT` line so tutor side effects are never
derived from a truncated payload. A corrupt canonical transcript segment fails
the active turn instead of being treated as an empty conversation.

Task and pack files projected into a chat session are copied atomically in
bounded streaming chunks. Task-output projection verifies a streamed SHA-256
during publication, so it neither retains a payload-sized byte vector nor
publishes bytes that changed after source inspection. Compact task-result and
voice caches remain non-authoritative: `task_summary.json` is capped at 64 KiB,
`voice_speech.json` at 32 KiB, and both are admitted by encoded depth/node
limits before a same-handle typed decode. Any cache failure is treated as a
miss; the primary result remains available unchanged.

Session deletion and session-output publication share one stable cross-process
lifecycle lock. Its canonical storage-identity filename lives under
`chat_sessions/.lifecycle/locks/`, outside the session directory, so deleting
the directory cannot replace the lock inode while an upload, pack projection,
task-output projection, message mutation, clear, or delete is waiting. The
global order is process-local session lock, lifecycle lock, then file-index
lock. Writers acquire the lifecycle lock before they create any session
directory or output file, then re-read the bounded session authority under that
lock; a generation marker by itself cannot authorize resurrection. Canonical
raw tool-result materialization for a Chat
owner uses the same fence before publishing inline/file manifests or payloads;
its scope-local locator cannot authorize recreation of a deleted owner.

Each session id also has a bounded generation marker outside the deletable
tree. Deletion publishes an exact `(session_id, session_generation)` tombstone
before removing bytes and metadata. A writer from that generation then fails
closed and cannot recreate the directory. A deliberate restore that reuses the
same id must publish a distinct generation while holding the same lifecycle
lock; the old tombstone does not block the restored session, but delayed
writers carrying the old generation remain rejected. Atomic create or marker
errors are resolved by exact readback before any rollback. An unreadable
session whose generation cannot be verified is retained for repair rather than
being partially deleted. If removal leaves only a partial tree, the tombstone
stays active; a later delete uses the matching generation marker to finish that
cleanup even when `session.json` has already disappeared. Identity-keyed
external cleanup remains inside the old generation fence, so a same-id restore
cannot publish new lifecycle state that a delayed deletion callback then erases.

Deleted-message and abandoned server-screen-capture reclamation is
metadata-first and crash recoverable. A bounded, path-safe
`output_cleanup_intent.json` records exact `(record_id, stored_name)` pairs
before matching file-index rows are removed. Physical files are unlinked only
after the index commit, and the intent remains until every unlink is confirmed
or idempotently absent. The next session operation—or the existing bounded
quiescent-session maintenance pass—retries a retained intent.
A durability error on any file-index replacement is accepted as committed only
when bounded readback equals the complete intended typed index, never merely
because the added or removed rows happen to be present. On divergent readback,
bytes are removed only for exact rows proven absent; bytes behind possibly
committed rows are retained for repair.
A newly published row that reuses a stored name is never unlinked; current
writers use full UUID-derived names, and recovery verifies the live index under
the same lock. The journal admits at most 10,000 items and 4 MiB, accepts only a
single normal filename component, and never follows an absolute or parent path.

The dedicated keyed HITL lifecycle authority is separate from pause bodies and
the retention-bounded transport log. Its current owner is the private
`pending_hitl_lifecycle.v3.sqlite3` store: ready startup validates authority and
uses one indexed minimum-expiry query, but does not replay or hydrate the
aggregate table. Exact reads and writes use the scoped primary key. Permanent
generic authority remains aggregate-unbounded on disk, so file size is an
operational capacity concern rather than hot-path replay work. The legacy JSONL
is read only by the one-time migration, which streams fixed 64 KiB chunks with
an 8 MiB per-record cap and encoded depth/node admission before typed decode,
then installs the durable downgrade fence before producers start. New lifecycle
events are bounded before pending-registry or transport ownership, including
in-memory-only deployments where no durable authority is configured. Size
admission and append share one exact-wire encoder: scalar fields retain the
derived Serde layout, while `input_schema` is traversed with heap-owned frames,
so accepted deep schemas do not reintroduce recursive serialization. Copies
retained by the pending registry and returned by exact lookup use the same
heap-framed clone rather than recursive `Value::clone`.

## Operational invariants

- Scope and HITL authorization checks are unchanged and fail closed.
- Provider tool-call/result ordering and raw/projected result authority are
  unchanged.
- Legacy migration and compaction move owned transcript batches; they do not
  recursively clone provider or tool-result trees.
- No larger runtime stacks or stack-size overrides are used.
- Authoritative payloads are rejected rather than silently truncated.
- Transcript compaction and stale-generation retention are maintenance work;
  neither runs synchronously while an answer is becoming visible.
- Generation clears are O(1); later maintenance reclaims stale transcript
  generations even when the new generation never reaches the compaction
  threshold.
- Session deletion is fenced outside the deletable tree and is generation
  aware; stale writers cannot resurrect a deleted or deliberately restored id.
- Session-output cleanup commits intent, then index removal, then physical
  unlink, and retains the bounded intent until recovery proves completion.
- **One unreadable session document errors the whole scope's listing.**
  `FileChatStore::list_sessions` reads as tolerant — `if let Ok(doc) =
  self.load_document(..)` skips a session it cannot parse — but the
  `lock_session(..)?` on the line above validates the document first and
  propagates, so that branch is unreachable for real damage. A corrupt or
  deleted document costs every other session in the scope, not just its own row
  (deletion surfaces as `No such file or directory`, corruption as
  `workspace JSON document failed encoded admission`).
  This is why `list_session_thread_lanes` reads the in-memory index instead.
  Plan for per-row tolerance:
  store durability adoption.
