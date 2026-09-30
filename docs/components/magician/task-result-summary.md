# Task-Result Summary (chat card + `/tasks` listing)

How a completed task's result surfaces as readable text in the chat result card
and the `/tasks` listing — and how a long/noisy HTML deliverable is condensed by
an auxiliary Luna call after the primary answer is ready, cached once per output
revision, and shown without ever calling the model on a read path.

Agent-decision evidence, output-synthesis caps, and the grounding gate that
runs before task-user publication are documented in
[Unified Task Panel](../unified-ui/unified-task-panel.md). Auxiliary
`task_summary` condensation starts only after that primary output is ready.

## File-backed execution artifacts

The result summary and the actual file are separate records. Terminal artifacts
must expose task/execution-relative paths so web and native clients can offer
Open/Download. The execution artifact index writer records these addresses for
files inside the owning execution's output directory; reads recover missing
relative paths from older absolute-only records without rewriting history.
Canonical path checks reject files in another execution and symlinks escaping
the owning output folder. Tests: `make test-terminal-artifact-links`.

## The problem it solves

A task's final user deliverable defaults to **HTML** (`out_task_user_<id>.html`).
The chat result card (`ChatMessageContent::TaskStatusUpdate.summary`) and the
`/tasks` listing derive their summary text from that deliverable, so loaders
must read HTML; an `.md`/`.txt`-only loader falls back to a generic
"Execution completed." and hides the result.

## How it works

### 1. HTML-aware loaders (zero LLM cost)

`load_task_user_summary_inner` (chat card) and `read_output_summary` (`/tasks`
listing) accept `out_task_user_*.html` and strip it to readable text via
`strip_html_to_text` (shared with the web-fetch handler — removes script/style,
drops tags, decodes entities, collapses whitespace). MD/TXT are used verbatim.
The real result prose surfaces immediately, with no model call.

### 2. Optional Luna condensation, after answer readiness

`FilesystemTaskUserFinalizer::finalize` persists and returns the primary user
output without performing card/list summarization. The terminal reducer commits
that `OutputRef` and clears `synthesis_pending`; that durable write is the answer
readiness boundary. Feed/progress refresh and auxiliary task summarization occur
afterward and cannot delay or roll back the answer. The summary worker is
scheduled after the initial feed/progress refresh so normal task cards usually
show the primary answer before their condensed metadata arrives.

Continuation-context output, media `OutputRef` registration, terminal schedule
refresh, and root episode recording use a separate post-answer worker keyed by
scope, execution, and primary-output revision. It reloads fresh task/execution
state, merges auxiliary refs through the reducer instead of writing a stale task
snapshot, and writes `terminal_auxiliary.json` only after required work succeeds.
The startup reconciler repairs a missing marker or episode, while an in-process
single-flight claim coalesces duplicate terminal observers.

The background job strips the committed deliverable to text and gates it:

- **long** (> 250 chars) **OR noisy** (non-alphanumeric ratio > 0.35) →
  summarized once by the `task_summary` LLM op;
- otherwise → cached **verbatim**.

The result is atomically written to a **`task_summary.json` sidecar** beside the
output (mirroring the existing `voice_speech.json` sidecar):

```json
{
  "text": "…",
  "generated_by": "llm" | "verbatim",
  "source_output_id": "out_task_user_…",
  "source_output_revision": "sha256…"
}
```

Both readers **prefer a current sidecar** and validate its stored revision
fingerprint against the authoritative primary `OutputRef`. File modification
times are not used as revision authority: an obsolete background job can finish
later than the current output. Therefore a reused accumulate/rerun output id
cannot briefly display its previous summary. The LLM is **never** called on a
read path — the `/tasks` listing loop stays LLM-free. When
`generated_by == "llm"`, a small **"— LLM generated"** note is appended to the
displayed summary. The call is **best-effort**: a model/router outage cannot
change primary-answer readiness; the job caches verbatim text when the model
call fails. The cache read is best-effort too: it admits at most 64 KiB with
explicit encoded JSON depth/node ceilings, then decodes from the same open file
handle. Missing, malformed, oversized, deep, or concurrently changed sidecars
are ordinary cache misses and never suppress the authoritative output.

The background call is still part of the execution tree for routing,
cancellation, cost, and telemetry. Before dispatch the worker reloads the
source execution's durable routing overlay, applies it to the summary router,
and attaches a full `TaskRef` containing scope, task, root execution, source
execution, the source execution's actual owner agent (including delegated
children), and chat session. Eval/diagnostic executions therefore keep
their selected profile for auxiliary summary work instead of falling back to a
process-global production route.

### 3. Revision-idempotent background materialization

The logical key contains scope, task id, and an `OutputRef` revision fingerprint
(output identity, path/media type, creation instant, owning execution/plan, and
source output ids). An in-memory claim coalesces concurrent observers during one
boot. After the potentially slow model call, the worker acquires the task's
cross-process write lock and revalidates the exact primary-output fingerprint
immediately before atomically replacing the sidecar. A stale rerun can consume
a provider call but cannot overwrite the current revision. The sidecar is the
durable materialization marker. Startup reconciliation scans completed tasks and
reschedules only missing or stale revisions; tasks whose primary synthesis is
still pending are skipped because their normal completion path will schedule
the job.

Summary arrival refreshes only the keyed task feed item used by Tasks and Today.
It does **not** republish task progress or another terminal
`task.status_changed`, so chat subscriptions, Request Activity, output-ready
handling, and completion speech see one lifecycle completion. The frontend also
deduplicates off-call completion speech by `(task_id, execution_id)` as a
defence-in-depth guard against unrelated duplicate delivery.

Concurrent parent-conversation copies use a stable request-derived message ID.
When a canonical task result arrives after an initial generic completion, its
text and original-answer link replace that same copy without resetting read or
playback receipts. The storage update and receipt acknowledgement share a
coordinator lock, and web uses the canonical result timestamp to reject stale
transport updates. The current-turn delegation guard reuses an already-running
child instead of relaunching it with reworded instructions
(`make test-chat-completion-repeat`).

The Luna call emits a paired `LLMRequestSent` and terminal
`LLMResponseReceived` for success, validation failure, or router failure. The
terminal event carries task, execution, chat-session, and explicit root-execution
lineage so tree-wide analytics include this auxiliary cost even though answer
readiness does not wait for it.

Output-synthesis provider failures retain the existing bounded retry and HITL
recovery path. A dispatch cancellation owned by a task/execution is different:
it stops on the first attempt because the same cancelled authority cannot
succeed on retry, and it does not create a misleading retry-synthesis HITL.

Sidecars without a revision fingerprint are not authoritative: readers fall
back to the primary output and startup reconciliation treats them as stale and
schedules a revision-pinned replacement.

> Tutor tasks keep their existing raw action-result path (the chat reader runs
> the tutor side effects first and only prefers the sidecar for non-tutor tasks).

### Config (no routing code)

`router.operation_mapping.task_summary: op-task-summary-luna` binds the op to a
dedicated GPT-5.6 Luna profile with tools disabled, reasoning effort `none`, a
2K output cap, and low verbosity. Meeting summaries remain independently bound
to `op-meeting-summary-local`; changing task-card metadata does not move meeting
or channel content off-device. Routing remains **config-only**
(`LLMOperation::Other("task_summary")` is map-keyed, no Rust enum change). The
system prompt lives in the store:
`data/magician_v2/prompts/task_summary_system_v1.0.0.json` (edit there, never in
Rust).

## Startup recovery

Startup recovery populates missing revision-aware sidecars through the same
runtime `task_summary` operation used by new tasks. This is the canonical
backfill path and requires no read-time LLM work.

## Key files

| Concern | Path |
|---|---|
| Chat card summary loader (HTML-aware + sidecar-prefer) | `magician/src/magician_v2/chat/service.rs` (`load_task_user_summary_inner`) |
| `/tasks` listing summary (HTML-aware + sidecar-prefer) | `magician/src/magician_v2/artifact_v2/service.rs` (`read_output_summary`) |
| Answer-ready boundary + background revision claim | `magician/src/magician_v2/artifact_v2/service.rs` (`spawn_task_summary_for_output`, `reconcile_stale_task_summaries_at_startup`) |
| Sidecar + gate + LLM call | `magician/src/magician_v2/artifact_v2/service.rs` (`TaskSummarySidecar`, `build_task_summary_sidecar`, `commit_task_summary_sidecar_if_current`) |
| HTML→text helper | `magician/src/magician_v2/execution/compiled_handlers/web_fetch.rs` (`strip_html_to_text`) |
| Store prompt | `data/magician_v2/prompts/task_summary_system_v1.0.0.json` |
| Op → profile binding | `llm-router.yaml` (`operation_mapping.task_summary`) |
