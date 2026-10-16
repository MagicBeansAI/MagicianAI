# Pause and Chat Persistence

Resumable execution state and canonical chat transcripts stay off request,
startup and reconnect stacks; each store has a small commit record plus
bounded payload files.

## Durable execution pauses

New pause records use format version 2:

- `k2_<key-hash>.json`: bounded envelope (scope, task/execution routing,
  agent/goal/cycle identity, pause kind, HITL display metadata, authorization
  hash, immutable body revision, body length, SHA-256).
- `k2_<key-hash>.body-<revision>.json`: the exact continuation body (provider
  continuation state, live tool-call/result history).

The body is synced first; the atomically replaced envelope is the commit and
authorization point, so a crash leaves at most an unreferenced body. Resume
validates filename, size, digest, scope, routing identity, pause kind and
authorization hash; a mismatch names each differing field
(`pending(field: body=… envelope=…)`).

If the envelope's directory sync fails after rename, the pair is live for this
boot and durable storage returns `EnvelopeDirectorySyncUncertain`
(best-effort storage publishes and warns). The prior
body and any legacy authority record survive until a fully synced commit;
exact recovery prefers the v2 envelope over the legacy record. A key found in
more than one scoped workspace fails closed. Body hydration runs only on the
execution runtime; HTTP and orchestration workers read only the envelope.

**Owner trust after restart.** A task paused under its unchanged root owner
recovers that owner's boot-local trust record (binding the whole continuation,
including question and spawn ceiling) only if its existing scope-HMAC terminal
receipt belongs to the execution and matches body SHA-256, envelope revision,
source segment and committed journal boundary, and runtime and durable task
still name that root and owner with no adopted successor. Failure keeps the
pause; same-boot records are never overwritten. The scoped server execution
record decides whether a receipt is required (deleting saved markers cannot
downgrade it); staged authority is checked even if a changed body drops its
elevated label. Not recovered: additional approvals, delegated owner chains,
protected invocation authority, caller tool grants. It reuses terminal
settlement receipts; no new store.

**Locking.** One bounded striped lock per pause key spans store, durable store,
take, exact recovery and execution cleanup, from body publish through envelope
rename/dir sync, stale-body cleanup and in-memory/index publication, so writers
and takes cannot delete each other's files. Single-key operations take one
stripe; whole-execution cleanup takes all in ascending order, so no new key
lands during terminal cleanup.

**Discovery.** Startup, reconnect, listing, confirmation inspection, orphan
reconciliation and cancellation read v2 envelopes only (at most 256 KiB;
larger records are classified legacy from metadata). Legacy monoliths hydrate
only for an exact-key resume or repair and become v2 on the next persist.
Cancellation revokes the envelope before removing bodies, so an unlink failure
leaves no resumable authority.

**Reaping and scope-wide admission.** The startup reaper removes a pause whose
execution directory is gone **or whose durable state is terminal**
(`completed`, `failed`, `cancelled`). Stateless-terminal admission treats a
legacy monolith that background repair indexed at its current revision as
public metadata; only an unrepaired, unquarantined monolith fails closed (else
one stale monolith would fail every admission and loop watchers).

**Bounds.** Depth and bytes are scanned before Serde; loading fails closed above
256 KiB (envelope), 64 MiB (body), the shared retained-value depth, the
aggregate node ceiling or the continuation collection limits, with one
recursive-value inventory for save and load (DOM/accessibility trees, inputs,
message blocks, frames, schemas). A rejected tree is drained iteratively on the
worker stack. Nothing is truncated.

### Cross-iteration loop-protective state

A pause carries `loop_protective_state`, the locals of
`execute_agentically_inner` that would otherwise reset on pause: cycle
detector and fingerprints, retry budgets, parse-abort and terminal-rejection
counters, failed-tool suppression, per-tool repeat counts, observation-policy
inputs and accumulated USD spend. Restoration drains it, so nested segments
cannot inherit parent budgets. Untouched fields, and an all-untouched block,
are omitted; fingerprint deques are bounded on load by the shared
protective-collection ceiling, not the record's own history bound.

The authorization hash covers the block (an on-disk edit could re-mint the
cost ceiling or disarm counters); anything a resume restores belongs in the
hash. Deliberately not restored:

| Excluded | Why |
|---|---|
| Execution history | Would bypass the JSON-node inventory; the live message log carries the conversation |
| Page merkle tree | Page state changes externally; resume re-observes |
| In-flight tool lineage | Its dispatch did not complete |
| Trust dispatch guard | Rebuilding it closes the policy TOCTOU window |
| Operator-steer buffer | Folded verbatim into the next prompt, so a tampered record would be prompt injection |

### Unspent approvals and the elevation boundary

Unspent approvals ride the pause so a resume does not re-ask. Covered by the
elevation predicate and authorization hash, they are:

- written **only** onto a pause that is already an elevation (elevations
  verify against an in-process entry lost on restart, so a promoted benign
  pause would fail closed and lose work);
- restored **only** by the exact-resume path, which verifies that entry and
  the hash (other resume endpoints have no gate and the envelope hash is
  unkeyed, so a rewritten pair could inject a pre-approved action);
- taken, not copied, by the loop and each segment move, so they cannot replay.

### Budgets across a resume

A resume that raises the iteration budget also clears an **exhausted** cost
budget, else a cost-paused run re-pauses forever. Unexhausted spend is kept,
so pausing for input cannot shed cost; every other protective value is kept
across all resumes.

### Resuming past an operator-steer terminal fence

A segment that pauses (`WaitingUser` after a question, or after
`BudgetExhausted`) leaves a *terminal fence* stamped with segment and
iteration in `restricted/operator_steers/<blake3(execution_id)>.json`. The
answer resumes as `<execution_id>-r<N>` with a fresh `LoopState`;
`rebind_terminal_fence_to_successor` hands over the fence with iteration reset
to 0 (else iteration 1 fails with
`operator_steer_inbox_terminal_fence_binding_mismatch`, stranding the run) and
the first `Prepare` reopens it (`reopen_after_nonterminal_boundary`).

### The answered ask is in the restored conversation

`need_user_input` is a pause, not a tool call, so `live_messages` end before the
ask. `continue_resumed_execution` appends it
(`append_answered_ask_to_restored_conversation`) as a `need_user_input` call
(question, input type) plus a result carrying the already-redacted answer as
`status: answered` and a note not to ask again, so secrets never enter the
transcript. A cold resume appends nothing.

### A resume outlives its HTTP request

`POST /api/magician/v2/executions/{id}/resume` and HITL `respond` run
`execute_agentically_resume_with_validation` and settlement (next pause,
`Executing → WaitingUser`, outcome) in a separate `tokio::spawn` task
(`MagicianV2Api::resume_detached_from_request`) and awaits it, since Actix drops
the handler future on disconnect and would strand the execution at `Executing`
(the task returns bytes, as `HttpResponse` and actix `Error` are not `Send`).
`respond` returns `202 resume_admitted` once the answer is admitted, since
device flows outlive the UI's 30 s fetch; see the API README.

### Lifecycle fences are not reentrant, and never wait forever

Pause activation, resume, cancel and terminal receipt each take an exact
lifecycle fence (terminal receipt: agent, then execution).

- **Agent fence:** tokio mutex stripe (256) over a flock stripe (64,
  `.agent-lifecycle-N.flock` under the scope's `pause_states/`).
- **Execution fence:** a per-execution mutex (weak map keyed by digest) over
  `.stateless-terminal-execution-locks/.<digest>.flock`, unlinked while held
  on release; a waiter on the unlinked inode detects it
  (`validate_locked_file_path_identity`) and reopens within the 5 s deadline.
  Windows instead deletes after release via an exclusive (share mode 0)
  delete-on-close open that fails while a waiter holds the file (that waiter
  removes it; openers retry on sharing violation / access denied). Uncompiled:
  the crate does not build for Windows yet.

`maybe_resume_parent_after_v3_child_results` dispatches a delegated parent's
continuation as its own job (settled by `settle_resumed_parent_continuation`),
never under the child's fence. It refuses a parent not parked
(`WaitingChildren`, or `Runnable` with a prepared handoff), so a failed resume
no longer re-announces `delegation.results_ready` each wake, and one that failed
before parking is not recorded as `waiting_for_children`.

Fences are not reentrant: re-acquiring a held one parks that task and every
later taker of the stripe forever, so handlers
release before calling anything that takes it (e.g. the orphan finalizer).
In-process waits are bounded at 30 s (`STATELESS_LIFECYCLE_PROCESS_LOCK_WAIT`)
and fail with `timed out … waiting for the in-process … lifecycle exclusion`;
legitimate holders keep a fence for one admission or receipt.
Diagnose a wedge from the lock files the process holds (`lsof`).

## Canonical chat transcript

New `session.json` documents are format version 2 (metadata only); display
messages keep their segmented JSONL store. Canonical LLM history:

```text
llm_history/
  manifest.json
  segments/<generation>-<sequence>.json
```

Appends are immutable, atomic segments; the constant-size manifest, replaced
last, is the only commit point; unreferenced partial tails are ignored after a
crash. A generation switch is an O(1) clear. The manifest tracks unmatched
tool-call IDs and a logical group ID; tail reads return whole boundary groups,
never a result without its call.

**Rejecting a just-submitted turn** spans two stores, so a bounded
`rollback-intent.json` (generation, sequence, effective count) commits first.
Any session operation recovers it under the session lock (display delete,
transcript truncation, metadata, then intent removal), idempotently: no crash
point replays a deleted message or truncates a later turn twice.

**Provider state.** OpenAI, Anthropic and Gemini native state is kept exactly
in transcript entries. There is deliberately no detached "latest provider
checkpoint": a response ID is only valid with its position and tool-call
group, and the router may fall back to a stateless provider mid-turn. A
bounded tail returns only entries whose whole boundary group fits; callers
needing an older anchor load full semantic history, never synthesize one. Typed
chat and voice delegation decode the full semantic history once per turn
(OpenAI Responses checks the whole chain, since an old failed turn can poison
later IDs); relevance and voice resume compaction read bounded tails. Without
a canonical transcript, bounded reads use the newest display segments.

**Legacy migration.** `session.json` files with inline display messages or
`llm_history` migrate lazily on first need, resumably (deterministic display
segment names with prefix verification, LLM progress in the manifest), without
duplication. Inline arrays clear only after
their segments commit; later metadata writes accept only a small format-v2
document, so title, status, thread and message mutations cannot rewrite a
legacy monolith.

**Bounds.** Startup indexes bounded metadata and never opens transcript
segments. Every record has a byte ceiling and depth admission before Serde;
manifest, rollback-intent, clear-intent and segment reads admit and decode the
same open handle against its admitted hash (never trusting a size stat).
Recursive transcript values share an aggregate node ceiling; display messages
and v2 metadata have no open-ended JSON. Large legacy documents stream through a
projection parser on the blocking pool, default-session normalization stays
index-only until access, and capped writers reject oversized payloads while
encoding.

**Diagnostics.** Success prompt dumps are off by default
(`MAGICIAN_CHAT_SUCCESS_PROMPT_DUMPS=true` enables them); provider-failure
dumps are on, move prompt state off the request without cloning, and degrade
to metadata past the 4 MiB byte, JSON-depth or node budget. Dump filenames use
the bounded safe-segment encoding.

**Delegate cards and caches.** Startup streams per-turn JSONL journals with
admission, skipping bad rows and keeping the latest unresolved descriptor per
task/execution. Task-result cards keep a 64 KiB/8,000-character
preview, but an exact `TUTOR_ACTION_RESULT` line is matched on the full stream.
A corrupt transcript segment fails the turn rather than reading as empty.
Projected task and pack files copy atomically in bounded chunks (task output
with a streamed SHA-256 check). `task_summary.json` (64 KiB) and
`voice_speech.json` (32 KiB) are non-authoritative caches; any admission or
decode failure is a miss and the primary result is unchanged.

**Session deletion.** Deletion and session-output publication share one
cross-process lifecycle lock in `chat_sessions/.lifecycle/locks/`, outside the
deletable directory (order: session lock, lifecycle lock, file-index lock).
Writers, including Chat-owner raw tool-result materialization, take it before
creating anything and re-read session authority under it; neither a generation
marker nor a scope-local locator alone authorizes recreation. Each session id
has a generation marker outside the tree; deletion first publishes an exact
`(session_id, session_generation)` tombstone so writers of that generation fail
closed. A same-id restore publishes a new generation under the lifecycle lock;
old-generation writers stay rejected. Create and marker errors are resolved by
exact readback before rollback. An unverifiable session is kept for repair; a
partial removal keeps its tombstone and a later delete finishes via the marker
even without `session.json`. Identity-keyed external cleanup stays inside the
old generation fence, so a delayed callback cannot erase a restore's new state.

**Output cleanup.** Deleted-message and abandoned server-screen-capture
reclamation records exact `(record_id, stored_name)` pairs in a bounded
`output_cleanup_intent.json`, removes index rows, then unlinks; the intent
stays until every unlink is confirmed (retried by the next session operation or
quiescent-session maintenance). An index durability error counts as committed
only if readback equals the intended index; otherwise only bytes behind rows
proven absent go. A file behind a newly published row reusing a stored name
is never unlinked (writers use UUID names; recovery checks the live index under
the same lock). The journal admits at most 10,000 items and 4 MiB of single
normal filename components, never absolute or parent paths.

**HITL lifecycle authority.** Kept apart from pause bodies and the transport log
in the private `pending_hitl_lifecycle.v3.sqlite3` store. Startup runs one
indexed minimum-expiry query and never replays the table; exact access uses the
scoped primary key. The file grows without an aggregate bound (a capacity
concern). Legacy JSONL is read once by migration (64 KiB chunks, 8 MiB
per-record cap, depth/node admission before decode), which installs a downgrade
fence before producers start. New events are bounded before registry or
transport ownership, even without durable authority. Admission and append share
one exact-wire encoder; `input_schema` is traversed and cloned with heap-owned
frames, not recursive `Value::clone`.

## Operational invariants

- Scope and HITL authorization checks fail closed.
- Provider tool-call/result ordering and raw/projected result authority hold.
- Migration and compaction move owned batches, never recursively cloning trees.
- No larger runtime stacks or stack-size overrides.
- Authoritative payloads are rejected, never truncated.
- Compaction and stale-generation retention stay off the answer path; stale
  generations are reclaimed even below the compaction threshold.
- Session deletion is generation-fenced outside the deletable tree; output
  cleanup goes intent, index removal, unlink.
- **One unreadable session document errors the whole scope's listing.**
  `FileChatStore::list_sessions` looks tolerant
  (`if let Ok(doc) = self.load_document(..)`), but the preceding
  `lock_session(..)?` propagates (`No such file or directory`, or
  `workspace JSON document failed encoded admission`), hence
  `list_session_thread_lanes` reads the in-memory index. Per-row tolerance:
  [store durability
  adoption](../../archive/plans/2026-08-11-store-durability-adoption.md).
